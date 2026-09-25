mod compiler;
mod markdown;
mod selection;

use std::env;
use std::error::Error;
use std::fs;
use std::num::NonZeroU32;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use arboard::Clipboard;
use compiler::{CompileMessage, Compiler};
use font8x8::{BASIC_FONTS, GREEK_FONTS, LATIN_FONTS, UnicodeFonts};
use image::{RgbaImage, imageops::FilterType};
use selection::{SearchPage, TextIndex};
use softbuffer::{Context, Surface};
use typst::utils::Scalar;
use typst_layout::PagedDocument;
use typst_render::RenderOptions;
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, OwnedDisplayHandle};
use winit::keyboard::{Key, KeyCode, ModifiersState, NamedKey, PhysicalKey};
use winit::window::{Window, WindowId};

const TICK: Duration = Duration::from_millis(50);
const BACKGROUND: u32 = 0x24262b;

struct Options {
    input: PathBuf,
    root: PathBuf,
    font_path: Option<PathBuf>,
    ppi: u32,
}

fn usage() {
    eprintln!("Usage: tv [--root DIR] [--font-path DIR] [--ppi N] FILE.typ|FILE.md");
    eprintln!(
        "q quit | n/Space next | p/Backspace previous | g/G first/last | hjkl pan | +/- zoom | f fit | / search | drag select | Ctrl+C copy"
    );
}

fn options() -> Result<Option<Options>, Box<dyn Error>> {
    let mut args = env::args_os().skip(1);
    let (mut input, mut root, mut font_path, mut ppi) = (None, None, None, 144);
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--help" | "-h") => {
                usage();
                return Ok(None);
            }
            Some("--root") => {
                root = Some(PathBuf::from(
                    args.next().ok_or("--root needs a directory")?,
                ))
            }
            Some("--font-path") => {
                font_path = Some(PathBuf::from(
                    args.next().ok_or("--font-path needs a directory")?,
                ))
            }
            Some("--ppi") => {
                ppi = args
                    .next()
                    .ok_or("--ppi needs a number")?
                    .to_string_lossy()
                    .parse()?;
                if !(36..=600).contains(&ppi) {
                    return Err("--ppi must be between 36 and 600".into());
                }
            }
            Some(value) if value.starts_with('-') => {
                return Err(format!("unknown option: {value}").into());
            }
            _ if input.is_none() => input = Some(PathBuf::from(arg)),
            _ => return Err("provide exactly one .typ or .md file".into()),
        }
    }
    let input = fs::canonicalize(input.ok_or("missing .typ or .md file")?)?;
    if !input.is_file() {
        return Err("input is not a file".into());
    }
    if !matches!(
        input.extension().and_then(|ext| ext.to_str()),
        Some("typ" | "md")
    ) {
        return Err("input must be a .typ or .md file".into());
    }
    let parent = input.parent().ok_or("input has no parent directory")?;
    let root = fs::canonicalize(root.unwrap_or_else(|| parent.to_path_buf()))?;
    let font_path = match font_path {
        Some(path) => Some(fs::canonicalize(path)?),
        None => parent.join("fonts").is_dir().then(|| parent.join("fonts")),
    };
    Ok(Some(Options {
        input,
        root,
        font_path,
        ppi,
    }))
}

struct Raster {
    width: u32,
    height: u32,
    pixels: Vec<u32>,
}

#[derive(Clone, Copy)]
struct SearchMatch {
    page: usize,
    start: usize,
    end: usize,
}

fn find_document_matches(pages: &[SearchPage], query: &str) -> Vec<SearchMatch> {
    if query.trim().is_empty() {
        return Vec::new();
    }
    pages
        .iter()
        .enumerate()
        .flat_map(|(page, content)| {
            content
                .matches(query)
                .into_iter()
                .map(move |(start, end)| SearchMatch {
                    page: page + 1,
                    start,
                    end,
                })
        })
        .collect()
}

fn rasterize(image: &RgbaImage, width: u32, height: u32) -> Raster {
    let resized = image::imageops::resize(image, width, height, FilterType::Triangle);
    let pixels = resized
        .pixels()
        .map(|p| {
            let [r, g, b, a] = p.0;
            let blend =
                |c: u8| ((u16::from(c) * u16::from(a) + 255 * (255 - u16::from(a))) / 255) as u32;
            blend(r) << 16 | blend(g) << 8 | blend(b)
        })
        .collect();
    Raster {
        width,
        height,
        pixels,
    }
}

fn scaled_page_size(image: &RgbaImage, width: u32, height: u32, zoom: f64) -> (u32, u32) {
    let fit = (f64::from(width.saturating_sub(48).max(1)) / f64::from(image.width()))
        .min(f64::from(height.saturating_sub(48).max(1)) / f64::from(image.height()));
    let scale = fit * zoom;
    let dimension = |original: u32| (f64::from(original) * scale).round().clamp(1.0, 8192.0) as u32;
    (dimension(image.width()), dimension(image.height()))
}

fn render_page(
    document: &PagedDocument,
    page: usize,
    ppi: u32,
) -> Option<(RgbaImage, TextIndex, (f32, f32))> {
    let page = document.pages().get(page.checked_sub(1)?)?;
    let options = RenderOptions {
        pixel_per_pt: Scalar::new(f64::from(ppi) / 72.0),
        render_bleed: false,
    };
    let pixmap = typst_render::render(page, &options);
    let mut bytes = pixmap.data().to_vec();
    // tiny-skia stores premultiplied RGBA; image::resize expects straight RGBA.
    let (pixels, _) = bytes.as_chunks_mut::<4>();
    for pixel in pixels {
        let alpha = u32::from(pixel[3]);
        if alpha > 0 && alpha < 255 {
            for channel in &mut pixel[..3] {
                *channel = (u32::from(*channel) * 255 / alpha).min(255) as u8;
            }
        }
    }
    let image = RgbaImage::from_raw(pixmap.width(), pixmap.height(), bytes)?;
    let size = (
        page.frame.width().to_pt() as f32,
        page.frame.height().to_pt() as f32,
    );
    Some((image, TextIndex::from_page(page), size))
}

struct App {
    context: Context<OwnedDisplayHandle>,
    surface: Option<Surface<OwnedDisplayHandle, Rc<Window>>>,
    compiler: Compiler,
    document: Option<Arc<PagedDocument>>,
    filename: String,
    ppi: u32,
    total: usize,
    page: usize,
    image: Option<RgbaImage>,
    page_size: (f32, f32),
    text: TextIndex,
    raster: Option<Raster>,
    zoom: f64,
    pan_x: i32,
    pan_y: i32,
    cursor: (f64, f64),
    selection: Option<(usize, usize)>,
    press: Option<(usize, (f64, f64))>,
    search_query: String,
    search_editing: bool,
    search_origin_page: usize,
    search_matches: Vec<SearchMatch>,
    search_pages: Vec<SearchPage>,
    active_match: Option<usize>,
    modifiers: ModifiersState,
    clipboard: Option<Clipboard>,
    error: bool,
    compiler_exited: bool,
    last_poll: Instant,
}

impl App {
    fn new(
        context: Context<OwnedDisplayHandle>,
        compiler: Compiler,
        filename: String,
        ppi: u32,
    ) -> Self {
        Self {
            context,
            surface: None,
            compiler,
            document: None,
            filename,
            ppi,
            total: 0,
            page: 1,
            image: None,
            page_size: (0.0, 0.0),
            text: TextIndex::default(),
            raster: None,
            zoom: 1.0,
            pan_x: 0,
            pan_y: 0,
            cursor: (0.0, 0.0),
            selection: None,
            press: None,
            search_query: String::new(),
            search_editing: false,
            search_origin_page: 1,
            search_matches: Vec::new(),
            search_pages: Vec::new(),
            active_match: None,
            modifiers: ModifiersState::empty(),
            clipboard: None,
            error: false,
            compiler_exited: false,
            last_poll: Instant::now() - TICK,
        }
    }

    fn window(&self) -> Option<&Rc<Window>> {
        self.surface.as_ref().map(Surface::window)
    }
    fn redraw(&self) {
        if let Some(window) = self.window() {
            window.request_redraw();
        }
    }

    fn title(&self) {
        let Some(window) = self.window() else { return };
        let status = if self.compiler_exited {
            " · compiler stopped"
        } else if self.error {
            " · error (see terminal)"
        } else if self.image.is_none() {
            " · compiling…"
        } else {
            ""
        };
        let search = if self.search_editing || !self.search_query.is_empty() {
            let query: String = self.search_query.chars().take(48).collect();
            let progress = self.active_match.map(|index| index + 1).unwrap_or(0);
            format!(" · /{query} ({progress}/{})", self.search_matches.len())
        } else {
            String::new()
        };
        window.set_title(&format!(
            "{} · {}/{} · {}%{}{} · tv",
            self.filename,
            self.page,
            self.total.max(1),
            (self.zoom * 100.0).round() as u32,
            status,
            search
        ));
    }

    fn load_page(&mut self) {
        if let Some(document) = &self.document
            && let Some((image, text, size)) = render_page(document, self.page, self.ppi)
        {
            self.image = Some(image);
            self.text = text;
            self.page_size = size;
            self.raster = None;
            self.selection = None;
            self.press = None;
            self.clamp_pan();
            self.redraw();
        }
        self.title();
    }

    fn poll(&mut self) {
        if self.last_poll.elapsed() < TICK {
            return;
        }
        self.last_poll = Instant::now();
        while let Ok(message) = self.compiler.messages.try_recv() {
            match message {
                CompileMessage::Ready {
                    document,
                    search_pages,
                } => {
                    self.total = document.pages().len();
                    self.page = self.page.clamp(1, self.total.max(1));
                    self.document = Some(document);
                    self.search_pages = search_pages;
                    self.error = false;
                    self.load_page();
                    self.rebuild_search();
                }
                CompileMessage::Failed => {
                    self.error = true;
                    self.title();
                }
                CompileMessage::Fatal(error) => {
                    eprintln!("{error}");
                    self.compiler_exited = true;
                    self.title();
                }
            }
        }
    }

    fn show_page(&mut self, page: usize) {
        if page == 0 || page > self.total || page == self.page {
            return;
        }
        self.page = page;
        self.pan_x = 0;
        self.pan_y = 0;
        self.load_page();
    }

    fn rebuild_search(&mut self) {
        self.search_matches.clear();
        self.active_match = None;
        self.search_matches = find_document_matches(&self.search_pages, &self.search_query);
        if !self.search_matches.is_empty() {
            let index = self
                .search_matches
                .iter()
                .position(|found| found.page >= self.search_origin_page)
                .unwrap_or(0);
            self.activate_match(index);
        } else {
            self.title();
            self.redraw();
        }
    }

    fn activate_match(&mut self, index: usize) {
        let Some(found) = self.search_matches.get(index).copied() else {
            return;
        };
        self.active_match = Some(index);
        self.show_page(found.page);
        self.title();
        self.redraw();
    }

    fn next_match(&mut self, direction: i32) {
        if self.search_matches.is_empty() {
            return;
        }
        let len = self.search_matches.len();
        let current = self.active_match.unwrap_or(0);
        let index = if direction < 0 {
            (current + len - 1) % len
        } else {
            (current + 1) % len
        };
        self.activate_match(index);
    }

    fn zoom_by(&mut self, factor: f64) {
        self.zoom = (self.zoom * factor).clamp(0.25, 4.0);
        self.raster = None;
        self.clamp_pan();
        self.title();
        self.redraw();
    }

    fn pan_limits(&self) -> (i32, i32) {
        let (Some(image), Some(window)) = (&self.image, self.window()) else {
            return (0, 0);
        };
        let size = window.inner_size();
        let (width, height) = scaled_page_size(image, size.width, size.height, self.zoom);
        (
            ((i64::from(width) - i64::from(size.width)).max(0) / 2) as i32,
            ((i64::from(height) - i64::from(size.height)).max(0) / 2) as i32,
        )
    }

    fn clamp_pan(&mut self) {
        let (x, y) = self.pan_limits();
        self.pan_x = self.pan_x.clamp(-x, x);
        self.pan_y = self.pan_y.clamp(-y, y);
    }

    fn pan(&mut self, dx: i32, dy: i32) {
        self.pan_x = self.pan_x.saturating_add(dx);
        self.pan_y = self.pan_y.saturating_add(dy);
        self.clamp_pan();
        self.redraw();
    }

    fn scroll(&mut self, dy: i32) {
        let (_, limit) = self.pan_limits();
        if dy < 0 && self.pan_y <= -limit {
            self.show_page(self.page.saturating_add(1));
        } else if dy > 0 && self.pan_y >= limit {
            self.show_page(self.page.saturating_sub(1));
        } else {
            self.pan(0, dy);
        }
    }

    fn page_rect(&self) -> Option<(f32, f32, f32, f32)> {
        let window = self.window()?;
        let image = self.image.as_ref()?;
        let size = window.inner_size();
        let (width, height) = scaled_page_size(image, size.width, size.height, self.zoom);
        let left = (i64::from(size.width) - i64::from(width)) / 2 + i64::from(self.pan_x);
        let top = (i64::from(size.height) - i64::from(height)) / 2 + i64::from(self.pan_y);
        Some((left as f32, top as f32, width as f32, height as f32))
    }

    fn hit_at_cursor(&self, strict: bool) -> Option<usize> {
        let (left, top, width, height) = self.page_rect()?;
        let x = self.cursor.0 as f32;
        let y = self.cursor.1 as f32;
        if x < left || x > left + width || y < top || y > top + height {
            return None;
        }
        let page_x = (x - left) * self.page_size.0 / width;
        let page_y = (y - top) * self.page_size.1 / height;
        if strict {
            self.text.hit(page_x, page_y)
        } else {
            self.text.nearest(page_x, page_y)
        }
    }

    fn copy_selection(&mut self) {
        let Some((anchor, focus)) = self.selection else {
            return;
        };
        let text = self.text.copied_text(anchor, focus);
        if text.is_empty() {
            return;
        }
        if self.clipboard.is_none() {
            match Clipboard::new() {
                Ok(clipboard) => self.clipboard = Some(clipboard),
                Err(error) => {
                    eprintln!("clipboard unavailable: {error}");
                    return;
                }
            }
        }
        if let Some(clipboard) = &mut self.clipboard
            && let Err(error) = clipboard.set_text(text)
        {
            eprintln!("could not copy selection: {error}");
        }
    }

    fn draw(&mut self) {
        let Some(surface) = self.surface.as_mut() else {
            return;
        };
        let size = surface.window().inner_size();
        let (Some(width), Some(height)) =
            (NonZeroU32::new(size.width), NonZeroU32::new(size.height))
        else {
            return;
        };
        if let Err(error) = surface.resize(width, height) {
            eprintln!("resize failed: {error}");
            return;
        }
        if let Some(image) = &self.image {
            let (rw, rh) = scaled_page_size(image, size.width, size.height, self.zoom);
            if self
                .raster
                .as_ref()
                .is_none_or(|r| r.width != rw || r.height != rh)
            {
                self.raster = Some(rasterize(image, rw, rh));
            }
        }
        let Ok(mut buffer) = surface.buffer_mut() else {
            return;
        };
        buffer.fill(BACKGROUND);
        if let Some(raster) = &self.raster {
            let x = (i64::from(size.width) - i64::from(raster.width)) / 2 + i64::from(self.pan_x);
            let y = (i64::from(size.height) - i64::from(raster.height)) / 2 + i64::from(self.pan_y);
            let left = x.max(0).min(i64::from(size.width)) as usize;
            let top = y.max(0).min(i64::from(size.height)) as usize;
            let right = (x + i64::from(raster.width))
                .max(0)
                .min(i64::from(size.width)) as usize;
            let bottom = (y + i64::from(raster.height))
                .max(0)
                .min(i64::from(size.height)) as usize;
            for row in top..bottom {
                let src =
                    (row as i64 - y) as usize * raster.width as usize + (left as i64 - x) as usize;
                let dst = row * size.width as usize + left;
                buffer[dst..dst + right - left]
                    .copy_from_slice(&raster.pixels[src..src + right - left]);
            }
            for (index, found) in self.search_matches.iter().enumerate() {
                if found.page == self.page {
                    let active = self.active_match == Some(index);
                    paint_range(
                        &mut buffer,
                        (size.width, size.height),
                        (
                            x as f32,
                            y as f32,
                            raster.width as f32,
                            raster.height as f32,
                        ),
                        self.page_size,
                        &self.text,
                        (found.start, found.end),
                        if active {
                            (255, 156, 42, 60)
                        } else {
                            (255, 205, 82, 35)
                        },
                    );
                }
            }
            if let Some((anchor, focus)) = self.selection {
                paint_range(
                    &mut buffer,
                    (size.width, size.height),
                    (
                        x as f32,
                        y as f32,
                        raster.width as f32,
                        raster.height as f32,
                    ),
                    self.page_size,
                    &self.text,
                    (anchor, focus),
                    (31, 113, 245, 45),
                );
            }
        }
        if self.search_editing || !self.search_query.is_empty() {
            let progress = self.active_match.map(|index| index + 1).unwrap_or(0);
            let label = format!(
                "/{}{}  {progress}/{}{}",
                self.search_query,
                if self.search_editing { "_" } else { "" },
                self.search_matches.len(),
                if self.search_editing {
                    ""
                } else {
                    "  n/N next/previous"
                }
            );
            draw_search_bar(&mut buffer, (size.width, size.height), &label);
        }
        if let Err(error) = buffer.present() {
            eprintln!("present failed: {error}");
        }
    }
}

fn draw_search_bar(buffer: &mut [u32], size: (u32, u32), label: &str) {
    let (width, height) = (size.0 as usize, size.1 as usize);
    if width == 0 || height == 0 {
        return;
    }
    let top = height.saturating_sub(24);
    buffer[top * width..].fill(0x191c21);
    buffer[top * width..(top + 1) * width].fill(0xebb754);
    let y0 = top + 4;
    let capacity = width.saturating_sub(24) / 16;
    for (index, ch) in label.chars().take(capacity).enumerate() {
        let glyph = BASIC_FONTS
            .get(ch)
            .or_else(|| LATIN_FONTS.get(ch))
            .or_else(|| GREEK_FONTS.get(ch))
            .or_else(|| BASIC_FONTS.get('?'));
        let Some(glyph) = glyph else { continue };
        for (row, bits) in glyph.iter().enumerate() {
            for column in 0..8 {
                if bits & (1 << column) == 0 {
                    continue;
                }
                let x = 12 + index * 16 + column * 2;
                let y = y0 + row * 2;
                for py in y..(y + 2).min(height) {
                    for px in x..(x + 2).min(width) {
                        buffer[py * width + px] = 0xf2f1eb;
                    }
                }
            }
        }
    }
}

fn paint_range(
    buffer: &mut [u32],
    screen_size: (u32, u32),
    rect: (f32, f32, f32, f32),
    page_size: (f32, f32),
    index: &TextIndex,
    selection: (usize, usize),
    shade: (u32, u32, u32, u32),
) {
    if page_size.0 <= 0.0 || page_size.1 <= 0.0 {
        return;
    }
    let (stride, screen_height) = screen_size;
    let (anchor, focus) = selection;
    let (start, end) = if anchor <= focus {
        (anchor, focus)
    } else {
        (focus, anchor)
    };
    let (left, top, width, height) = rect;
    for glyph in index.glyphs.get(start..=end).unwrap_or_default() {
        let x0 = (left + glyph.left * width / page_size.0).floor().max(0.0) as usize;
        let y0 = (top + glyph.top * height / page_size.1).floor().max(0.0) as usize;
        let x1 = (left + glyph.right * width / page_size.0)
            .ceil()
            .min(stride as f32) as usize;
        let y1 = (top + glyph.bottom * height / page_size.1)
            .ceil()
            .min(screen_height as f32) as usize;
        for y in y0..y1 {
            for x in x0..x1 {
                let pixel = &mut buffer[y * stride as usize + x];
                let keep = 100 - shade.3;
                let r = ((*pixel >> 16) & 255) * keep / 100 + shade.0 * shade.3 / 100;
                let g = ((*pixel >> 8) & 255) * keep / 100 + shade.1 * shade.3 / 100;
                let b = (*pixel & 255) * keep / 100 + shade.2 * shade.3 / 100;
                *pixel = (r << 16) | (g << 8) | b;
            }
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.surface.is_some() {
            return;
        }
        let attributes = Window::default_attributes()
            .with_title("tv · compiling…")
            .with_inner_size(LogicalSize::new(1000, 800));
        let window = Rc::new(
            event_loop
                .create_window(attributes)
                .expect("could not create window"),
        );
        self.surface =
            Some(Surface::new(&self.context, window).expect("could not create drawing surface"));
        self.title();
        self.redraw();
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::RedrawRequested => self.draw(),
            WindowEvent::Resized(_) => {
                self.raster = None;
                self.clamp_pan();
                self.redraw();
            }
            WindowEvent::ModifiersChanged(modifiers) => self.modifiers = modifiers.state(),
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor = (position.x, position.y);
                if let Some((anchor, start)) = self.press {
                    let moved = (self.cursor.0 - start.0).hypot(self.cursor.1 - start.1);
                    if moved >= 4.0
                        && let Some(focus) = self.hit_at_cursor(false)
                    {
                        self.selection = Some((anchor, focus));
                        self.redraw();
                    }
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                self.press = self.hit_at_cursor(true).map(|index| (index, self.cursor));
                self.selection = None;
                self.redraw();
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => self.press = None,
            WindowEvent::MouseWheel { delta, .. } => {
                let amount = match delta {
                    MouseScrollDelta::LineDelta(_, y) => (y * 80.0) as i32,
                    MouseScrollDelta::PixelDelta(position) => position.y as i32,
                };
                self.scroll(amount);
            }
            WindowEvent::KeyboardInput { event, .. }
                if event.state == ElementState::Pressed
                    && (!event.repeat || self.search_editing) =>
            {
                if self.modifiers.control_key()
                    && event.physical_key == PhysicalKey::Code(KeyCode::KeyC)
                {
                    self.copy_selection();
                    return;
                }
                if self.search_editing {
                    match &event.logical_key {
                        Key::Named(NamedKey::Escape) => {
                            self.search_editing = false;
                            self.search_query.clear();
                            self.rebuild_search();
                        }
                        Key::Named(NamedKey::Enter) => {
                            self.search_editing = false;
                            self.title();
                            self.redraw();
                        }
                        Key::Named(NamedKey::Backspace) => {
                            self.search_query.pop();
                            self.rebuild_search();
                        }
                        _ if !self.modifiers.control_key()
                            && !self.modifiers.alt_key()
                            && !self.modifiers.super_key()
                            && self.search_query.chars().count() < 128 =>
                        {
                            if let Some(text) = &event.text
                                && text.chars().all(|ch| !ch.is_control())
                            {
                                self.search_query.push_str(text);
                                self.rebuild_search();
                            }
                        }
                        _ => {}
                    }
                    return;
                }
                match &event.logical_key {
                    Key::Named(NamedKey::Escape) if !self.search_query.is_empty() => {
                        self.search_query.clear();
                        self.rebuild_search();
                    }
                    Key::Named(NamedKey::Escape) => event_loop.exit(),
                    Key::Named(NamedKey::ArrowDown) => self.scroll(-80),
                    Key::Named(NamedKey::ArrowUp) => self.scroll(80),
                    Key::Named(NamedKey::ArrowLeft) => self.pan(80, 0),
                    Key::Named(NamedKey::ArrowRight) => self.pan(-80, 0),
                    Key::Named(NamedKey::PageDown | NamedKey::Space) => {
                        self.show_page(self.page + 1)
                    }
                    Key::Named(NamedKey::PageUp | NamedKey::Backspace) => {
                        self.show_page(self.page.saturating_sub(1))
                    }
                    Key::Named(NamedKey::Home) => self.show_page(1),
                    Key::Named(NamedKey::End) => self.show_page(self.total),
                    Key::Character(ch) => match ch.as_str() {
                        "q" => event_loop.exit(),
                        "/" => {
                            self.search_editing = true;
                            self.search_origin_page = self.page;
                            self.search_query.clear();
                            self.rebuild_search();
                        }
                        "n" if !self.search_matches.is_empty() => self.next_match(1),
                        "N" if !self.search_matches.is_empty() => self.next_match(-1),
                        "n" | " " => self.show_page(self.page + 1),
                        "p" => self.show_page(self.page.saturating_sub(1)),
                        "g" => self.show_page(1),
                        "G" => self.show_page(self.total),
                        "j" => self.scroll(-80),
                        "k" => self.scroll(80),
                        "h" => self.pan(80, 0),
                        "l" => self.pan(-80, 0),
                        "+" | "=" => self.zoom_by(1.2),
                        "-" | "_" => self.zoom_by(1.0 / 1.2),
                        "f" | "0" => {
                            self.zoom = 1.0;
                            self.pan_x = 0;
                            self.pan_y = 0;
                            self.raster = None;
                            self.title();
                            self.redraw();
                        }
                        _ => {}
                    },
                    _ => {}
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        self.poll();
        event_loop.set_control_flow(ControlFlow::WaitUntil(Instant::now() + TICK));
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let Some(options) = options()? else {
        return Ok(());
    };
    let filename = options
        .input
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let ppi = options.ppi;
    let compiler = Compiler::start(options);
    let event_loop = EventLoop::new()?;
    let context = Context::new(event_loop.owned_display_handle())?;
    let mut app = App::new(context, compiler, filename, ppi);
    event_loop.run_app(&mut app)?;
    Ok(())
}
