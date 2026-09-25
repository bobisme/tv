use typst::layout::{Frame, FrameItem, Point, Transform};
use typst_layout::Page;

#[derive(Clone, Debug)]
pub struct GlyphBox {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
    pub text: String,
    run: usize,
    size: f32,
}

#[derive(Default)]
pub struct TextIndex {
    pub glyphs: Vec<GlyphBox>,
}

pub struct SearchPage {
    haystack: String,
    glyph_at_byte: Vec<usize>,
}

impl TextIndex {
    pub fn from_page(page: &Page) -> Self {
        let mut index = Self::default();
        index.collect(&page.frame, &mut Vec::new());
        index
    }

    fn collect(&mut self, frame: &Frame, ancestors: &mut Vec<(Point, Transform)>) {
        for &(position, ref item) in frame.items() {
            match item {
                FrameItem::Group(group) => {
                    ancestors.push((position, group.transform));
                    self.collect(&group.frame, ancestors);
                    ancestors.pop();
                }
                FrameItem::Text(text) => {
                    let run = self.glyphs.len();
                    let mut cursor = position;
                    for glyph in &text.glyphs {
                        let width = glyph.x_advance.at(text.size);
                        let height = glyph.y_advance.at(text.size);
                        let from = Point::new(cursor.x, cursor.y - text.size);
                        let to = Point::new(cursor.x + width, cursor.y + text.size * 0.25);
                        let corners =
                            [from, Point::new(to.x, from.y), to, Point::new(from.x, to.y)];
                        let mut left = f32::INFINITY;
                        let mut top = f32::INFINITY;
                        let mut right = f32::NEG_INFINITY;
                        let mut bottom = f32::NEG_INFINITY;
                        for mut point in corners {
                            for (offset, transform) in ancestors.iter().rev() {
                                point = point.transform(*transform) + *offset;
                            }
                            left = left.min(point.x.to_pt() as f32);
                            top = top.min(point.y.to_pt() as f32);
                            right = right.max(point.x.to_pt() as f32);
                            bottom = bottom.max(point.y.to_pt() as f32);
                        }
                        let range = glyph.range();
                        if let Some(fragment) = text.text.get(range)
                            && !fragment.is_empty()
                        {
                            self.glyphs.push(GlyphBox {
                                left,
                                top,
                                right,
                                bottom,
                                text: fragment.to_owned(),
                                run,
                                size: text.size.to_pt() as f32,
                            });
                        }
                        cursor.x += width;
                        cursor.y -= height;
                    }
                }
                _ => {}
            }
        }
    }

    pub fn nearest(&self, x: f32, y: f32) -> Option<usize> {
        self.glyphs
            .iter()
            .enumerate()
            .min_by(|(_, a), (_, b)| distance_sq(a, x, y).total_cmp(&distance_sq(b, x, y)))
            .map(|(index, _)| index)
    }

    pub fn hit(&self, x: f32, y: f32) -> Option<usize> {
        let index = self.nearest(x, y)?;
        (distance_sq(&self.glyphs[index], x, y) <= 16.0).then_some(index)
    }

    pub fn copied_text(&self, anchor: usize, focus: usize) -> String {
        let (start, end) = if anchor <= focus {
            (anchor, focus)
        } else {
            (focus, anchor)
        };
        let Some(selected) = self.glyphs.get(start..=end) else {
            return String::new();
        };
        let mut copied = String::new();
        let mut previous: Option<&GlyphBox> = None;
        for glyph in selected {
            if let Some(before) = previous {
                if (glyph.top - before.top).abs() > before.size.max(glyph.size) * 0.5 {
                    copied.push('\n');
                } else if glyph.run != before.run && glyph.left - before.right > glyph.size * 0.3 {
                    copied.push(' ');
                }
            }
            copied.push_str(&glyph.text);
            previous = Some(glyph);
        }
        copied
    }

    pub fn search_page(&self) -> SearchPage {
        let mut haystack = String::new();
        let mut glyph_at_byte = Vec::new();
        let mut previous: Option<&GlyphBox> = None;
        for (index, glyph) in self.glyphs.iter().enumerate() {
            if let Some(before) = previous
                && ((glyph.top - before.top).abs() > before.size.max(glyph.size) * 0.5
                    || glyph.run != before.run && glyph.left - before.right > glyph.size * 0.3)
            {
                push_search_char(&mut haystack, &mut glyph_at_byte, ' ', index);
            }
            for ch in glyph.text.chars().flat_map(char::to_lowercase) {
                push_search_char(&mut haystack, &mut glyph_at_byte, ch, index);
            }
            previous = Some(glyph);
        }
        SearchPage {
            haystack,
            glyph_at_byte,
        }
    }
}

impl SearchPage {
    pub fn matches(&self, query: &str) -> Vec<(usize, usize)> {
        let needle = query
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase();
        if needle.is_empty() {
            return Vec::new();
        }
        self.haystack
            .match_indices(&needle)
            .map(|(byte, _)| {
                (
                    self.glyph_at_byte[byte],
                    self.glyph_at_byte[byte + needle.len() - 1],
                )
            })
            .collect()
    }
}

fn push_search_char(haystack: &mut String, glyph_at_byte: &mut Vec<usize>, ch: char, index: usize) {
    if ch.is_whitespace() {
        if haystack.is_empty() || haystack.ends_with(' ') {
            return;
        }
        haystack.push(' ');
        glyph_at_byte.push(index);
    } else {
        haystack.push(ch);
        glyph_at_byte.extend(std::iter::repeat_n(index, ch.len_utf8()));
    }
}

fn distance_sq(glyph: &GlyphBox, x: f32, y: f32) -> f32 {
    let dx = (glyph.left - x).max(0.0).max(x - glyph.right);
    let dy = (glyph.top - y).max(0.0).max(y - glyph.bottom);
    dx * dx + dy * dy
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copies_selection_in_both_drag_directions() {
        let index = TextIndex {
            glyphs: vec![
                GlyphBox {
                    left: 0.0,
                    top: 0.0,
                    right: 8.0,
                    bottom: 12.0,
                    text: "A".into(),
                    run: 0,
                    size: 10.0,
                },
                GlyphBox {
                    left: 8.0,
                    top: 0.0,
                    right: 16.0,
                    bottom: 12.0,
                    text: "β".into(),
                    run: 0,
                    size: 10.0,
                },
                GlyphBox {
                    left: 0.0,
                    top: 16.0,
                    right: 8.0,
                    bottom: 28.0,
                    text: "C".into(),
                    run: 2,
                    size: 10.0,
                },
            ],
        };
        assert_eq!(index.copied_text(0, 2), "Aβ\nC");
        assert_eq!(index.copied_text(2, 0), "Aβ\nC");
        assert_eq!(index.hit(9.0, 5.0), Some(1));
        assert_eq!(index.hit(100.0, 100.0), None);
        let search = index.search_page();
        assert_eq!(search.matches("β"), vec![(1, 1)]);
        assert_eq!(search.matches("aβ c"), vec![(0, 2)]);
        assert!(search.matches("missing").is_empty());
    }
}
