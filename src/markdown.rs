use std::path::Path;

use pulldown_cmark::{Alignment, CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};

const PAPER_STYLE: &str = r#"
#set page(
  paper: "us-letter",
  margin: (top: 0.78in, bottom: 0.72in, left: 1.14in, right: 1.14in),
  footer: context [#align(center)[#counter(page).display()]],
)
#set text(font: "New Computer Modern", size: 9.3pt, lang: "en")
#set par(justify: true, leading: 0.44em, spacing: 1.1em, first-line-indent: 0pt)
#set heading(numbering: (n) => [#n#h(0.65em)])
#show heading.where(level: 1): set text(size: 11pt, weight: "bold")
#show heading.where(level: 1): set block(above: 1.5em, below: 1.5em)
#show link: set text(fill: black)
#show table: set text(size: 8pt, hyphenate: false)
#show table: set par(justify: false)
#show table: set table(inset: (x: 2pt, y: 3pt), gutter: 0pt, stroke: none)
#show table.cell.where(y: 0): set text(weight: "bold")
#show raw.where(block: true): set text(size: 7.2pt)
"#;

pub fn to_typst(markdown: &str, input: &Path) -> String {
    let options = Options::ENABLE_TABLES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_FOOTNOTES;
    let mut events: Vec<_> = Parser::new_ext(markdown, options).collect();
    let title = if matches!(
        events.first(),
        Some(Event::Start(Tag::Heading {
            level: HeadingLevel::H1,
            ..
        }))
    ) {
        let end = events
            .iter()
            .position(|event| matches!(event, Event::End(TagEnd::Heading(HeadingLevel::H1))))
            .unwrap_or(0);
        let title = plain_text(&events[1..end]);
        events.drain(..=end);
        title
    } else {
        let stem = input
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .replace(['-', '_'], " ");
        if stem.chars().any(char::is_alphabetic)
            && stem
                .chars()
                .all(|ch| !ch.is_alphabetic() || ch.is_uppercase())
        {
            stem.to_lowercase()
                .split_whitespace()
                .map(|word| {
                    let mut chars = word.chars();
                    chars
                        .next()
                        .map(char::to_uppercase)
                        .into_iter()
                        .flatten()
                        .chain(chars)
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
                .join(" ")
        } else {
            stem
        }
    };

    let abstract_text = if matches!(events.first(), Some(Event::Start(Tag::Paragraph))) {
        let end = events
            .iter()
            .position(|event| matches!(event, Event::End(TagEnd::Paragraph)))
            .unwrap_or(0);
        if plain_text(&events[1..end]).len() >= 120 {
            let paragraph: Vec<_> = events.drain(..=end).collect();
            Some(Renderer::new(&paragraph[1..end]).render_until(None, true))
        } else {
            None
        }
    } else {
        None
    };

    let mut result = String::from(PAPER_STYLE);
    result.push_str("\n#v(0.1in)\n#line(length: 100%, stroke: 1.6pt)\n#v(0.1in)\n");
    result.push_str("#align(center)[#text(size: 16pt, weight: \"bold\")[");
    result.push_str(&text(&title));
    result.push_str("]]\n#v(0.1in)\n#line(length: 100%, stroke: 0.55pt)\n#v(0.34in)\n");
    if let Some(abstract_text) = abstract_text {
        result.push_str("#align(center)[#text(weight: \"bold\")[Abstract]]\n#v(0.12in)\n");
        result.push_str("#align(center)[#block(width: 84%)[#set par(justify: true, spacing: 0pt)\n#align(left)[");
        result.push_str(&abstract_text);
        result.push_str("]]]\n#v(0.36in)\n\n");
    }
    result.push_str(&Renderer::new(&events).render_until(None, false));
    result
}

fn plain_text(events: &[Event<'_>]) -> String {
    events
        .iter()
        .filter_map(|event| match event {
            Event::Text(value) | Event::Code(value) => Some(value.as_ref()),
            _ => None,
        })
        .collect()
}

fn quoted(value: &str) -> String {
    let mut result = String::from("\"");
    for ch in value.chars() {
        match ch {
            '\\' => result.push_str("\\\\"),
            '"' => result.push_str("\\\""),
            '\n' => result.push_str("\\n"),
            '\r' => result.push_str("\\r"),
            '\t' => result.push_str("\\t"),
            ch if ch.is_control() => {}
            ch => result.push(ch),
        }
    }
    result.push('"');
    result
}

fn text(value: &str) -> String {
    format!("#text({})", quoted(value))
}

struct Renderer<'a> {
    events: &'a [Event<'a>],
    pos: usize,
}

struct TableCell {
    markup: String,
    plain: String,
}

impl TableCell {
    fn metric(&self) -> bool {
        let value = self.plain.trim();
        !value.is_empty()
            && value.chars().any(|ch| ch.is_ascii_digit())
            && value.chars().all(|ch| {
                ch.is_ascii_digit() || matches!(ch, '.' | ',' | '/' | '%' | '±' | '-' | '−' | ' ')
            })
    }

    fn header_markup(&self) -> String {
        if let Some((label, denominator)) = self.plain.rsplit_once(" /")
            && !label.is_empty()
            && !denominator.is_empty()
            && denominator.chars().all(|ch| ch.is_ascii_digit())
        {
            return format!(
                "{}#linebreak(){}",
                text(label),
                text(&format!("/{denominator}"))
            );
        }
        self.markup.clone()
    }
}

impl<'a> Renderer<'a> {
    fn new(events: &'a [Event<'a>]) -> Self {
        Self { events, pos: 0 }
    }

    fn render_until(&mut self, end: Option<TagEnd>, inline: bool) -> String {
        let mut out = String::new();
        while let Some(event) = self.events.get(self.pos) {
            self.pos += 1;
            match event {
                Event::End(tag) if Some(*tag) == end => break,
                Event::End(_) => {}
                Event::Start(tag) => {
                    let rendered = match tag {
                        Tag::Paragraph => {
                            let inner = self.render_until(Some(TagEnd::Paragraph), true);
                            if inline {
                                inner
                            } else {
                                format!("{inner}\n\n")
                            }
                        }
                        Tag::Heading { level, .. } => {
                            let inner = self.render_until(Some(TagEnd::Heading(*level)), true);
                            format!("\n#heading(level: {})[{inner}]\n\n", *level as u8)
                        }
                        Tag::BlockQuote(kind) => {
                            let inner = self.render_until(Some(TagEnd::BlockQuote(*kind)), false);
                            format!("\n#quote(block: true)[{inner}]\n\n")
                        }
                        Tag::CodeBlock(kind) => self.code_block(kind),
                        Tag::List(start) => self.list(*start),
                        Tag::Table(alignments) => self.table(alignments),
                        Tag::Emphasis => self.wrap(TagEnd::Emphasis, "emph"),
                        Tag::Strong => self.wrap(TagEnd::Strong, "strong"),
                        Tag::Strikethrough => self.wrap(TagEnd::Strikethrough, "strike"),
                        Tag::Superscript => self.wrap(TagEnd::Superscript, "super"),
                        Tag::Subscript => self.wrap(TagEnd::Subscript, "sub"),
                        Tag::Link { dest_url, .. } => {
                            let label = self.render_until(Some(TagEnd::Link), true);
                            format!("#link({})[{label}]", quoted(dest_url))
                        }
                        Tag::Image { dest_url, .. } => {
                            let alt = self.render_until(Some(TagEnd::Image), true);
                            if dest_url.starts_with("http://") || dest_url.starts_with("https://") {
                                alt
                            } else {
                                format!("#image({}, width: 100%)", quoted(dest_url))
                            }
                        }
                        Tag::FootnoteDefinition(label) => {
                            let body = self.render_until(Some(TagEnd::FootnoteDefinition), false);
                            format!("\n#footnote[{body}] {}\n", text(label))
                        }
                        Tag::HtmlBlock => self.render_until(Some(TagEnd::HtmlBlock), true),
                        Tag::MetadataBlock(kind) => {
                            let _ = self.render_until(Some(TagEnd::MetadataBlock(*kind)), true);
                            String::new()
                        }
                        _ => self.render_until(Some(tag.to_end()), inline),
                    };
                    out.push_str(&rendered);
                }
                Event::Text(value) => out.push_str(&text(value)),
                Event::Code(value) => out.push_str(&format!("#raw({})", quoted(value))),
                Event::SoftBreak => out.push_str("#text(\" \")"),
                Event::HardBreak => out.push_str("#linebreak()"),
                Event::Rule => out.push_str("\n#line(length: 100%, stroke: 0.5pt)\n"),
                Event::TaskListMarker(done) => out.push_str(if *done {
                    "#text(\"☑ \")"
                } else {
                    "#text(\"☐ \")"
                }),
                Event::InlineMath(value) | Event::DisplayMath(value) => {
                    out.push_str(&format!("#raw({})", quoted(value)));
                }
                Event::Html(value) | Event::InlineHtml(value) => out.push_str(&text(value)),
                Event::FootnoteReference(value) => out.push_str(&text(&format!("[{value}]"))),
            }
        }
        out
    }

    fn wrap(&mut self, end: TagEnd, function: &str) -> String {
        let inner = self.render_until(Some(end), true);
        format!("#{function}[{inner}]")
    }

    fn code_block(&mut self, kind: &CodeBlockKind<'_>) -> String {
        let mut body = String::new();
        while let Some(event) = self.events.get(self.pos) {
            self.pos += 1;
            match event {
                Event::End(TagEnd::CodeBlock) => break,
                Event::Text(value) | Event::Code(value) => body.push_str(value),
                _ => {}
            }
        }
        let language = match kind {
            CodeBlockKind::Fenced(info) => info.split_whitespace().next().unwrap_or(""),
            CodeBlockKind::Indented => "",
        };
        format!(
            "\n#raw({}, block: true, lang: {})\n\n",
            quoted(&body),
            quoted(language)
        )
    }

    fn list(&mut self, start: Option<u64>) -> String {
        let mut items = Vec::new();
        while let Some(event) = self.events.get(self.pos) {
            self.pos += 1;
            match event {
                Event::Start(Tag::Item) => items.push(self.render_until(Some(TagEnd::Item), true)),
                Event::End(TagEnd::List(_)) => break,
                _ => {}
            }
        }
        let mut out = if let Some(number) = start {
            format!("\n#enum(start: {number}, ")
        } else {
            String::from("\n#list(")
        };
        for item in items {
            out.push_str(&format!("[{item}], "));
        }
        out.push_str(")\n\n");
        out
    }

    fn table(&mut self, alignments: &[Alignment]) -> String {
        let mut header = Vec::new();
        let mut rows = Vec::new();
        while let Some(event) = self.events.get(self.pos) {
            self.pos += 1;
            match event {
                Event::Start(Tag::TableHead) => header = self.table_cells(TagEnd::TableHead),
                Event::Start(Tag::TableRow) => rows.push(self.table_cells(TagEnd::TableRow)),
                Event::End(TagEnd::Table) => break,
                _ => {}
            }
        }
        let columns = alignments.len().max(header.len()).max(1);
        let metric_table = columns > 1
            && !rows.is_empty()
            && rows
                .iter()
                .all(|row| row.len() == columns && row.iter().skip(1).all(TableCell::metric));
        let mut widths = vec![12usize; columns];
        for row in std::iter::once(&header).chain(rows.iter()) {
            for (column, cell) in row.iter().enumerate().take(columns) {
                widths[column] = widths[column].max(cell.plain.chars().count().min(48));
            }
        }
        let mut out = String::from("\n#table(\n  columns: (");
        if metric_table {
            out.push_str("1fr, ");
            for _ in 1..columns {
                out.push_str("auto, ");
            }
            out.push_str("),\n  inset: (x: 4pt, y: 3pt),");
        } else {
            for width in widths {
                out.push_str(&format!("{width}fr, "));
            }
            out.push_str("),");
        }
        out.push_str("\n  align: (");
        for column in 0..columns {
            out.push_str(match alignments.get(column).unwrap_or(&Alignment::None) {
                Alignment::Right => "right, ",
                Alignment::Center => "center, ",
                _ => "left, ",
            });
        }
        out.push_str("),\n  table.hline(stroke: 0.7pt),\n  table.header(");
        for (column, cell) in header.into_iter().enumerate() {
            if metric_table && column > 0 {
                out.push_str(&format!(
                    "table.cell(align: center)[{}], ",
                    cell.header_markup()
                ));
            } else {
                out.push_str(&format!("[{}], ", cell.header_markup()));
            }
        }
        out.push_str("),\n  table.hline(stroke: 0.45pt),\n");
        for row in rows {
            for cell in row {
                out.push_str(&format!("  [{}],\n", cell.markup));
            }
        }
        out.push_str("  table.hline(stroke: 0.7pt),\n)\n\n");
        out
    }

    fn table_cells(&mut self, end: TagEnd) -> Vec<TableCell> {
        let mut cells = Vec::new();
        while let Some(event) = self.events.get(self.pos) {
            self.pos += 1;
            match event {
                Event::Start(Tag::TableCell) => {
                    let start = self.pos;
                    let markup = self.render_until(Some(TagEnd::TableCell), true);
                    let plain = plain_text(&self.events[start..self.pos - 1]);
                    cells.push(TableCell { markup, plain });
                }
                Event::End(tag) if *tag == end => break,
                _ => {}
            }
        }
        cells
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_typst_markup_and_renders_paper_features() {
        let source = "# A *paper*\n\nAn abstract with #code, a [link](https://example.com), and enough text to be treated as the abstract for this test document.\n\n| Readout | Value |\n|---|---:|\n| **a** | `2` |\n\n- One\n- Two\n";
        let rendered = to_typst(source, Path::new("/tmp/paper.md"));
        assert!(rendered.contains("#text(\"A paper\")"));
        assert!(rendered.contains("#text(\"An abstract with #code, a \")"));
        assert!(rendered.contains("#link(\"https://example.com\")"));
        assert!(rendered.contains("#table("));
        assert!(rendered.contains("#list("));
        assert!(to_typst("", Path::new("/tmp/RESULTS.md")).contains("#text(\"Results\")"));
    }

    #[test]
    fn metric_headers_get_room_and_break_before_denominators() {
        let source = "| Readout | Development /160 | Fresh /53 | Binding /12 | Withheld /12 |\n|---|---:|---:|---:|---:|\n| A long model formulation | 151 | 49 | 12 | 12 |\n";
        let rendered = to_typst(source, Path::new("/tmp/report.md"));
        assert!(rendered.contains("columns: (1fr, auto, auto, auto, auto, )"));
        assert!(rendered.contains("#text(\"Development\")#linebreak()#text(\"/160\")"));
        assert!(rendered.contains("table.cell(align: center)"));
    }
}
