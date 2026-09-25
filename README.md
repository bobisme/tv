# tv

A small, keyboard driven viewer for Typst and Markdown documents. It compiles both formats directly in Rust, renders the current page in a native window, and refreshes after edits. No `typst` command is required.

## Run

```sh
cargo install --path .
tv path/to/document.typ
tv path/to/document.md
```

During development, you can view this README:

```sh
cargo run --release -- README.md
```

Markdown is parsed in memory and typeset with a NeurIPS-inspired paper layout: US Letter pages, ruled title, compact serif text, an abstract, tables, and page numbers. The first H1 is used as the title; otherwise the filename is used. A long opening paragraph becomes the abstract. Tables, links, lists, code blocks, emphasis, and local images are supported. Embedded HTML images also render, including local WebP images and `width` attributes; `<p align="center">` and `<div align="center">` center them. Common HTML text formatting, links, and line breaks are supported. HTML and CSS are not rendered as a browser would render them. The Markdown file and local images are watched for changes. Math is shown verbatim because Markdown and Typst use different math syntax.

The input file's directory is the default Typst project root. A `fonts` directory next to the input file is added automatically. Use `--root DIR`, `--font-path DIR`, or `--ppi N` to override these defaults. The initial render resolution is 144 PPI. Compilation errors appear in the launching terminal and are marked in the window title; the last good page remains visible until compilation succeeds again.

## Controls

| Input | Action |
| --- | --- |
| `q` | Quit |
| `Esc` | Cancel or clear search; quit when no search is active |
| `n`, `Space`, `Page Down` | Next page |
| `p`, `Backspace`, `Page Up` | Previous page |
| `g`, `Home` | First page |
| `G`, `End` | Last page |
| `j` `k`, vertical arrows, mouse wheel | Scroll; continue to adjacent page at an edge |
| `h` `l`, horizontal arrows | Pan horizontally |
| `+`, `-` | Zoom around fit size |
| `f`, `0` | Fit page to window and reset pan |
| `/`, then type and press `Enter` | Search all pages; matches are highlighted |
| `n`, `N` after a search | Next or previous match |
| Drag from text | Select text |
| Click | Clear selection |
| `Ctrl+C` | Copy selected text |

The viewer keeps the compiled document and current page image in memory. Selection follows the rendered glyphs, including text in nested frames. The copied text uses layout based spacing and line breaks, so complex layouts may copy differently from source order.
