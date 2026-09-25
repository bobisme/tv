use std::error::Error;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Instant;

use typst::diag::FileResult;
use typst::foundations::{Bytes, Datetime, Duration};
use typst::syntax::{FileId, RootedPath, Source, VirtualPath, VirtualRoot};
use typst::text::{Font, FontBook};
use typst::utils::LazyHash;
use typst::{Library, LibraryExt, World};
use typst_kit::datetime::Time;
use typst_kit::diagnostics::{self, DiagnosticFormat, DiagnosticWorld};
use typst_kit::downloader::SystemDownloader;
use typst_kit::files::{FileStore, FsRoot, SystemFiles};
use typst_kit::fonts::{self, FontStore};
use typst_kit::packages::SystemPackages;
use typst_kit::watcher::Watcher;
use typst_layout::PagedDocument;

use crate::Options;
use crate::markdown;
use crate::selection::{SearchPage, TextIndex};

pub enum CompileMessage {
    Ready {
        document: Arc<PagedDocument>,
        search_pages: Vec<SearchPage>,
    },
    Failed,
    Fatal(String),
}

pub struct Compiler {
    pub messages: Receiver<CompileMessage>,
}

impl Compiler {
    pub fn start(options: Options) -> Self {
        let (sender, messages) = mpsc::channel();
        std::thread::spawn(move || {
            if let Err(error) = run(options, &sender) {
                eprintln!("typst compilation stopped: {error}");
                let _ = sender.send(CompileMessage::Fatal(error));
            }
        });
        Self { messages }
    }
}

struct LocalWorld {
    library: LazyHash<Library>,
    main: FileId,
    files: FileStore<SystemFiles>,
    fonts: FontStore,
    time: Time,
    markdown_source: Option<Source>,
}

impl LocalWorld {
    fn new(options: &Options) -> Result<Self, Box<dyn Error + Send + Sync>> {
        let virtual_path = VirtualPath::virtualize(&options.root, &options.input)
            .map_err(|error| format!("source file must be within the project root: {error:?}"))?;
        let main = RootedPath::new(VirtualRoot::Project, virtual_path).intern();
        let packages = SystemPackages::new(SystemDownloader::new("typst-viewer/0.1.0"));
        let files = FileStore::new(SystemFiles::new(
            FsRoot::new(options.root.clone()),
            packages,
        ));

        let mut fonts = FontStore::new();
        if let Some(path) = &options.font_path {
            fonts.extend(fonts::scan(path));
        }
        if let Some(paths) = std::env::var_os("TYPST_FONT_PATHS") {
            for path in std::env::split_paths(&paths) {
                fonts.extend(fonts::scan(&path));
            }
        }
        fonts.extend(fonts::system());
        fonts.extend(fonts::embedded());

        Ok(Self {
            library: LazyHash::new(Library::default()),
            main,
            files,
            fonts,
            time: Time::system(),
            markdown_source: None,
        })
    }

    fn refresh_markdown(&mut self, options: &Options) -> io::Result<()> {
        if options.input.extension().is_some_and(|ext| ext == "md") {
            let input = fs::read_to_string(&options.input)?;
            self.markdown_source = Some(Source::new(
                self.main,
                markdown::to_typst(&input, &options.input),
            ));
        }
        Ok(())
    }

    fn dependencies(&mut self) -> Vec<PathBuf> {
        let (loader, ids) = self.files.dependencies();
        ids.filter_map(|id| loader.resolve(id).ok()).collect()
    }

    fn reset(&mut self) {
        self.files.reset();
        self.time.reset();
    }
}

impl World for LocalWorld {
    fn library(&self) -> &LazyHash<Library> {
        &self.library
    }
    fn book(&self) -> &LazyHash<FontBook> {
        self.fonts.book()
    }
    fn main(&self) -> FileId {
        self.main
    }
    fn source(&self, id: FileId) -> FileResult<Source> {
        if id == self.main
            && let Some(source) = &self.markdown_source
        {
            return Ok(source.clone());
        }
        self.files.source(id)
    }
    fn file(&self, id: FileId) -> FileResult<Bytes> {
        self.files.file(id)
    }
    fn font(&self, index: usize) -> Option<Font> {
        self.fonts.font(index)
    }
    fn today(&self, offset: Option<Duration>) -> Option<Datetime> {
        self.time.today(offset)
    }
}

impl DiagnosticWorld for LocalWorld {
    fn name(&self, id: FileId) -> String {
        match id.root() {
            VirtualRoot::Project => id
                .vpath()
                .realize(Path::new("."))
                .unwrap_or_default()
                .display()
                .to_string(),
            VirtualRoot::Package(package) => format!("{package}{}", id.vpath().get_with_slash()),
        }
    }
}

fn run(options: Options, sender: &Sender<CompileMessage>) -> Result<(), String> {
    let mut world = LocalWorld::new(&options).map_err(|error| error.to_string())?;
    let mut watcher = Watcher::new(None).map_err(|error| error.to_string())?;
    loop {
        let start = Instant::now();
        let message = match world.refresh_markdown(&options) {
            Ok(()) => {
                let result = typst::compile::<PagedDocument>(&world);
                let mut output = diagnostics::termcolor::NoColor::new(io::stderr());
                let errors = result.output.as_ref().err();
                let diagnostics = errors
                    .into_iter()
                    .flat_map(|errors| errors.iter())
                    .chain(result.warnings.iter());
                if let Err(error) =
                    diagnostics::emit(&mut output, &world, diagnostics, DiagnosticFormat::Human)
                {
                    eprintln!("could not display Typst diagnostics: {error}");
                }
                match result.output {
                    Ok(document) => {
                        eprintln!(
                            "compiled {} page(s) in {:.1} ms",
                            document.pages().len(),
                            start.elapsed().as_secs_f64() * 1000.0
                        );
                        let search_pages = document
                            .pages()
                            .iter()
                            .map(|page| TextIndex::from_page(page).search_page())
                            .collect();
                        CompileMessage::Ready {
                            document: Arc::new(document),
                            search_pages,
                        }
                    }
                    Err(_) => CompileMessage::Failed,
                }
            }
            Err(error) => {
                eprintln!("could not read {}: {error}", options.input.display());
                CompileMessage::Failed
            }
        };
        let mut dependencies = world.dependencies();
        dependencies.push(options.input.clone());
        watcher
            .update(dependencies)
            .map_err(|error| error.to_string())?;
        world.reset();
        if sender.send(message).is_err() {
            return Ok(());
        }
        watcher.wait().map_err(|error| error.to_string())?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};
    use typst::layout::{Frame, FrameItem};

    fn contains_image(frame: &Frame) -> bool {
        frame.items().any(|(_, item)| match item {
            FrameItem::Image(..) => true,
            FrameItem::Group(group) => contains_image(&group.frame),
            _ => false,
        })
    }

    #[test]
    fn markdown_html_webp_image_compiles_and_is_watched() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("tv-html-test-{}-{nonce}", std::process::id()));
        fs::create_dir_all(root.join("images")).unwrap();
        let input = root.join("README.md");
        fs::write(
            &input,
            "# Ward\n\n<p align=\"center\">\n<img src=\"images/ward.webp\" alt=\"Ward\" width=\"400\" />\n</p>\n",
        )
        .unwrap();
        fs::write(
            root.join("images/ward.webp"),
            include_bytes!("../tests/assets/sample.webp"),
        )
        .unwrap();
        let mut world = LocalWorld::new(&Options {
            input: input.clone(),
            root: root.clone(),
            font_path: None,
            ppi: 144,
        })
        .unwrap();
        world
            .refresh_markdown(&Options {
                input,
                root: root.clone(),
                font_path: None,
                ppi: 144,
            })
            .unwrap();
        let document = typst::compile::<PagedDocument>(&world).output.unwrap();
        assert!(
            document
                .pages()
                .iter()
                .any(|page| contains_image(&page.frame))
        );
        assert!(
            world
                .dependencies()
                .contains(&root.join("images/ward.webp"))
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn project_readme_compiles() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let compiler = Compiler::start(Options {
            input: root.join("README.md"),
            root,
            font_path: None,
            ppi: 144,
        });
        match compiler
            .messages
            .recv_timeout(Duration::from_secs(15))
            .unwrap()
        {
            CompileMessage::Ready { document, .. } => assert!(!document.pages().is_empty()),
            _ => panic!("project README did not compile"),
        }
    }

    #[test]
    fn markdown_edits_recompile() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("tv-markdown-test-{}-{nonce}", std::process::id()));
        fs::create_dir(&root).unwrap();
        let input = root.join("paper.md");
        fs::write(&input, "# First title\n\nBody text.\n").unwrap();
        let compiler = Compiler::start(Options {
            input: input.clone(),
            root: root.clone(),
            font_path: None,
            ppi: 144,
        });
        let receive = || match compiler
            .messages
            .recv_timeout(Duration::from_secs(15))
            .unwrap()
        {
            CompileMessage::Ready {
                document,
                search_pages,
            } => (document, search_pages),
            _ => panic!("Markdown did not compile"),
        };
        let (first, _) = receive();
        assert_eq!(first.pages().len(), 1);
        fs::write(
            &input,
            "# Second title\n\n| Readout | Development /160 | Fresh /53 |\n|---|---:|---:|\n| Model A | 151 | 49 |\n",
        )
        .unwrap();
        let (second, search_pages) = receive();
        let page_text: String = TextIndex::from_page(&second.pages()[0])
            .glyphs
            .into_iter()
            .map(|glyph| glyph.text)
            .collect();
        assert!(page_text.contains("Second title"), "{page_text}");
        assert_eq!(
            crate::find_document_matches(&search_pages, "development").len(),
            1
        );
        drop(compiler);

        let typst_input = root.join("paper.typ");
        fs::write(
            &typst_input,
            "= Native Typst\n#pagebreak()\n= Second Page\n",
        )
        .unwrap();
        let typst_compiler = Compiler::start(Options {
            input: typst_input.clone(),
            root: root.clone(),
            font_path: None,
            ppi: 144,
        });
        let search_pages = match typst_compiler
            .messages
            .recv_timeout(Duration::from_secs(15))
            .unwrap()
        {
            CompileMessage::Ready { search_pages, .. } => search_pages,
            _ => panic!("Typst did not compile"),
        };
        let hits = crate::find_document_matches(&search_pages, "second");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].page, 2);
        drop(typst_compiler);
        fs::remove_file(input).unwrap();
        fs::remove_file(typst_input).unwrap();
        fs::remove_dir(root).unwrap();
    }
}
