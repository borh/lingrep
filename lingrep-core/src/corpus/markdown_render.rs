//! Renders Markdown-dialect documents as HTML with source-line mappings.
//!
//! Raw HTML is escaped; link targets are limited to `http:`, `https:`,
//! `mailto:` and `#`, and mathematics remains source text.

use std::collections::{BTreeMap, BTreeSet};

use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};

use super::markdown_dialect::{
    DivLine, closes_fence, opens_fence, parse_classes, parse_div, parse_heading,
    strip_attribute_spans,
};

/// Change this revision whenever rendered HTML, text runs or block boundaries change, including after dependency upgrades.
pub const MARKDOWN_RENDERER_REVISION: &str = "lingrep-core/markdown-render/1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedDocument {
    /// The classes of each `:::` region, space-separated, by region index.
    pub regions: Vec<String>,
    /// Top-level blocks in document order.
    pub blocks: Vec<RenderedBlock>,
    /// The block that holds each `data-c` element, by its `data-c` value.
    pub container_blocks: Vec<u32>,
    /// Source text in output order.
    pub runs: Vec<TextRun>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedBlock {
    pub html: String,
    /// The regions enclosing the block, outermost first.
    pub regions: Vec<u32>,
    /// The 1-based source line the block starts on.
    pub first_line: usize,
}

/// Source text written into one `data-c` element.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextRun {
    /// The 1-based source line the text came from.
    pub line: usize,
    pub container: u32,
    /// UTF-16 offset of the text in the element's text content.
    pub utf16_start: u32,
    pub text: String,
}

enum Fence {
    Open(String),
    Close,
}

struct Prepared {
    lines: Vec<String>,
    fences: Vec<(usize, Fence)>,
    heading_classes: BTreeMap<usize, String>,
}

fn prepare(source: &str) -> Prepared {
    enum Block {
        Body,
        Frontmatter,
        Comment,
        Fenced(char, usize),
    }
    let mut lines = Vec::new();
    let mut fences = Vec::new();
    let mut heading_classes = BTreeMap::new();
    let mut block = Block::Body;
    for (index, raw) in source_lines(source).enumerate() {
        let number = index + 1;
        if index == 0 && raw.trim_end() == "---" {
            block = Block::Frontmatter;
            lines.push(String::new());
            continue;
        }
        match block {
            Block::Frontmatter => {
                if raw.trim_end() == "---" {
                    block = Block::Body;
                }
                lines.push(String::new());
                continue;
            }
            Block::Comment => {
                if raw.contains("-->") {
                    block = Block::Body;
                }
                lines.push(String::new());
                continue;
            }
            Block::Fenced(marker, minimum) => {
                if closes_fence(raw.trim_start(), marker, minimum) {
                    block = Block::Body;
                }
                lines.push(raw.to_owned());
                continue;
            }
            Block::Body => {}
        }
        let mut content = raw;
        let mut trimmed = content.trim_start();
        let mut blank = trimmed.is_empty();
        while !blank && trimmed.starts_with("<!--") {
            match trimmed.split_once("-->") {
                None => {
                    block = Block::Comment;
                    blank = true;
                }
                Some((_, tail)) => {
                    trimmed = tail.trim_start();
                    content = trimmed;
                    blank = trimmed.is_empty();
                }
            }
        }
        if blank {
            lines.push(String::new());
            continue;
        }
        if let Some((marker, minimum)) = opens_fence(trimmed) {
            block = Block::Fenced(marker, minimum);
            lines.push(content.to_owned());
            continue;
        }
        if let Some(div) = parse_div(trimmed) {
            match div {
                DivLine::Open(classes) => fences.push((number, Fence::Open(join(&classes)))),
                DivLine::Close => fences.push((number, Fence::Close)),
                DivLine::Malformed => {}
            }
            lines.push(String::new());
            continue;
        }
        if parse_heading(trimmed).is_some() {
            heading_classes.insert(number, join(&parse_classes(trimmed)));
        }
        lines.push(strip_attribute_spans(content));
    }
    Prepared {
        lines,
        fences,
        heading_classes,
    }
}

fn source_lines(source: &str) -> impl Iterator<Item = &str> {
    source
        .split_inclusive('\n')
        .map(|line| line.strip_suffix('\n').unwrap_or(line))
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
}

fn join(classes: &BTreeSet<String>) -> String {
    classes
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(" ")
}

struct Assembled {
    text: String,
    line_starts: Vec<usize>,
    source_lines: Vec<Option<usize>>,
}

impl Assembled {
    fn new(prepared: &Prepared, breaks: &BTreeSet<usize>) -> Self {
        let mut text = String::new();
        let mut line_starts = Vec::new();
        let mut source_lines = Vec::new();
        for (index, line) in prepared.lines.iter().enumerate() {
            let number = index + 1;
            if breaks.contains(&number) {
                line_starts.push(text.len());
                source_lines.push(None);
                text.push('\n');
            }
            line_starts.push(text.len());
            source_lines.push(Some(number));
            text.push_str(line);
            text.push('\n');
        }
        Self {
            text,
            line_starts,
            source_lines,
        }
    }

    fn line_index(&self, offset: usize) -> usize {
        self.line_starts
            .partition_point(|start| *start <= offset)
            .saturating_sub(1)
    }

    fn source_line(&self, index: usize) -> Option<usize> {
        self.source_lines.get(index).copied().flatten()
    }

    fn line_text(&self, index: usize) -> &str {
        let start = self.line_starts.get(index).copied().unwrap_or(0);
        let end = self
            .line_starts
            .get(index + 1)
            .copied()
            .unwrap_or(self.text.len());
        self.text
            .get(start..end)
            .unwrap_or_default()
            .trim_end_matches('\n')
    }
}

fn options() -> Options {
    Options::ENABLE_TABLES | Options::ENABLE_FOOTNOTES
}

fn table_breaks(assembled: &Assembled) -> BTreeSet<usize> {
    let starts_with_pipe = |index: usize| assembled.line_text(index).trim_start().starts_with('|');
    let mut breaks = BTreeSet::new();
    let mut table: Option<bool> = None;
    for (event, range) in Parser::new_ext(&assembled.text, options()).into_offset_iter() {
        match event {
            Event::Start(Tag::TableHead) => {
                let delimiter = assembled.line_index(range.start) + 1;
                table = Some(starts_with_pipe(delimiter));
            }
            Event::Start(Tag::TableRow) if table == Some(true) => {
                let row = assembled.line_index(range.start);
                if !starts_with_pipe(row) {
                    if let Some(line) = assembled.source_line(row) {
                        breaks.insert(line);
                    }
                    table = Some(false);
                }
            }
            Event::End(TagEnd::Table) => table = None,
            _ => {}
        }
    }
    breaks
}

fn row_cells(line: &str) -> Vec<(usize, &str)> {
    let body_start = line.len() - line.trim_start().len();
    let first = body_start + usize::from(line.trim_start().starts_with('|'));
    let mut start = first;
    let mut cells = Vec::new();
    let mut escaped = false;
    for (offset, character) in line
        .char_indices()
        .skip_while(|(offset, _)| *offset < first)
    {
        if escaped {
            escaped = false;
        } else if character == '\\' {
            escaped = true;
        } else if character == '|' {
            cells.push((start, offset));
            start = offset + 1;
        }
    }
    if line
        .get(start..)
        .is_some_and(|rest| !rest.trim().is_empty())
    {
        cells.push((start, line.len()));
    }
    cells
        .into_iter()
        .filter_map(|(start, end)| {
            let cell = line.get(start..end)?;
            let leading = cell.len() - cell.trim_start().len();
            Some((start + leading, cell.trim()))
        })
        .collect()
}

fn escape(text: &str, out: &mut String) {
    for character in text.chars() {
        match character {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            other => out.push(other),
        }
    }
}

fn escaped(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    escape(text, &mut out);
    out
}

fn safe_href(destination: &str) -> bool {
    let lower = destination.to_ascii_lowercase();
    ["http://", "https://", "mailto:", "#"]
        .iter()
        .any(|scheme| lower.starts_with(scheme))
}

struct Writer<'a> {
    assembled: &'a Assembled,
    prepared: &'a Prepared,
    next_fence: usize,
    region_stack: Vec<u32>,
    document: RenderedDocument,
    html: String,
    depth: usize,
    block_regions: Vec<u32>,
    block_first_line: usize,
    tags: Vec<bool>,
    open: Vec<(u32, u32)>,
    footnotes: Vec<String>,
    in_head: bool,
    row: Option<(usize, usize)>,
}

impl Writer<'_> {
    fn markup(&mut self, markup: &str) {
        self.html.push_str(markup);
    }

    fn text(&mut self, text: &str, line: Option<usize>) {
        if text.is_empty() {
            return;
        }
        escape(text, &mut self.html);
        if let (Some(line), Some(&(container, utf16_start))) = (line, self.open.last()) {
            self.document.runs.push(TextRun {
                line,
                container,
                utf16_start,
                text: text.to_owned(),
            });
        }
        let units = text.encode_utf16().count() as u32;
        for (_, length) in &mut self.open {
            *length += units;
        }
    }

    fn source_text(&mut self, text: &str, offset: usize) {
        let mut index = self.assembled.line_index(offset);
        for piece in text.split_inclusive('\n') {
            let line = self.assembled.source_line(index);
            self.text(piece, line);
            if piece.ends_with('\n') {
                index += 1;
            }
        }
    }

    fn line_at(&self, offset: usize) -> Option<usize> {
        self.assembled
            .source_line(self.assembled.line_index(offset))
    }

    fn begin_block(&mut self, offset: usize) {
        let first_line = self.line_at(offset).unwrap_or(self.block_first_line);
        while let Some((line, fence)) = self.prepared.fences.get(self.next_fence)
            && *line < first_line
        {
            match fence {
                Fence::Open(classes) => {
                    self.region_stack.push(self.document.regions.len() as u32);
                    self.document.regions.push(classes.clone());
                }
                Fence::Close => {
                    self.region_stack.pop();
                }
            }
            self.next_fence += 1;
        }
        self.block_regions = self.region_stack.clone();
        self.block_first_line = first_line;
    }

    fn end_block(&mut self) {
        self.document.blocks.push(RenderedBlock {
            html: std::mem::take(&mut self.html),
            regions: std::mem::take(&mut self.block_regions),
            first_line: self.block_first_line,
        });
    }

    fn open_container(&mut self, tag: &str, class: Option<&str>) {
        let container = self.document.container_blocks.len() as u32;
        self.document
            .container_blocks
            .push(self.document.blocks.len() as u32);
        self.html.push('<');
        self.html.push_str(tag);
        if let Some(class) = class.filter(|class| !class.is_empty()) {
            self.html.push_str(" class=\"");
            escape(class, &mut self.html);
            self.html.push('"');
        }
        self.html.push_str(&format!(" data-c=\"{container}\">"));
        self.open.push((container, 0));
    }

    fn footnote_number(&mut self, label: &str) -> usize {
        if let Some(index) = self.footnotes.iter().position(|known| known == label) {
            return index + 1;
        }
        self.footnotes.push(label.to_owned());
        self.footnotes.len()
    }

    fn start(&mut self, tag: Tag<'_>, offset: usize) {
        if self.depth == 0 {
            self.begin_block(offset);
        }
        self.depth += 1;
        let mut container = true;
        match tag {
            Tag::Paragraph => self.open_container("p", None),
            Tag::Heading { level, .. } => {
                let classes = self
                    .line_at(offset)
                    .and_then(|line| self.prepared.heading_classes.get(&line))
                    .cloned();
                self.open_container(&level.to_string(), classes.as_deref());
            }
            Tag::CodeBlock(_) => {
                self.open_container("pre", None);
                self.markup("<code>");
            }
            Tag::HtmlBlock => self.open_container("p", Some("raw-html")),
            Tag::Item => self.open_container("li", None),
            Tag::TableCell => {
                if let Some((_, cells)) = &mut self.row {
                    *cells += 1;
                }
                self.open_container(if self.in_head { "th" } else { "td" }, None);
            }
            other => {
                container = false;
                match other {
                    Tag::BlockQuote(_) => self.markup("<blockquote>"),
                    Tag::List(Some(1)) => self.markup("<ol>"),
                    Tag::List(Some(start)) => self.markup(&format!("<ol start=\"{start}\">")),
                    Tag::List(None) => self.markup("<ul>"),
                    Tag::FootnoteDefinition(label) => {
                        let number = self.footnote_number(&label);
                        self.markup(&format!(
                            "<div class=\"footnote-definition\" id=\"fn-{number}\"><sup class=\"footnote-label\">"
                        ));
                        self.text(&number.to_string(), None);
                        self.markup("</sup>");
                    }
                    Tag::Table(_) => self.markup("<table>"),
                    Tag::TableHead => {
                        self.in_head = true;
                        self.markup("<thead><tr>");
                    }
                    Tag::TableRow => {
                        self.row = Some((self.assembled.line_index(offset), 0));
                        self.markup("<tr>");
                    }
                    Tag::Emphasis => self.markup("<em>"),
                    Tag::Strong => self.markup("<strong>"),
                    Tag::Strikethrough => self.markup("<del>"),
                    Tag::Link { dest_url, .. } => {
                        if safe_href(&dest_url) {
                            self.markup(&format!("<a href=\"{}\">", escaped(&dest_url)));
                        } else {
                            self.markup("<a>");
                        }
                    }
                    Tag::Image { .. } => self.markup("<span class=\"image\">"),
                    _ => self.markup("<div>"),
                }
            }
        }
        self.tags.push(container);
    }

    fn end(&mut self, tag: TagEnd) {
        if tag == TagEnd::TableRow {
            self.extra_cells();
        }
        let markup = match tag {
            TagEnd::Paragraph | TagEnd::HtmlBlock => "</p>".to_owned(),
            TagEnd::Heading(level) => format!("</{level}>"),
            TagEnd::BlockQuote(_) => "</blockquote>".to_owned(),
            TagEnd::CodeBlock => "</code></pre>".to_owned(),
            TagEnd::List(true) => "</ol>".to_owned(),
            TagEnd::List(false) => "</ul>".to_owned(),
            TagEnd::Item => "</li>".to_owned(),
            TagEnd::FootnoteDefinition => "</div>".to_owned(),
            TagEnd::Table => "</tbody></table>".to_owned(),
            TagEnd::TableHead => {
                self.in_head = false;
                "</tr></thead><tbody>".to_owned()
            }
            TagEnd::TableRow => {
                self.row = None;
                "</tr>".to_owned()
            }
            TagEnd::TableCell => if self.in_head { "</th>" } else { "</td>" }.to_owned(),
            TagEnd::Emphasis => "</em>".to_owned(),
            TagEnd::Strong => "</strong>".to_owned(),
            TagEnd::Strikethrough => "</del>".to_owned(),
            TagEnd::Link => "</a>".to_owned(),
            TagEnd::Image => "</span>".to_owned(),
            _ => "</div>".to_owned(),
        };
        self.markup(&markup);
        if self.tags.pop() == Some(true) {
            self.open.pop();
        }
        self.depth = self.depth.saturating_sub(1);
        if self.depth == 0 {
            self.end_block();
        }
    }

    fn extra_cells(&mut self) {
        let Some((index, written)) = self.row else {
            return;
        };
        let line = self.assembled.line_text(index);
        let base = self.assembled.line_starts.get(index).copied().unwrap_or(0);
        let extra: Vec<(usize, String)> = row_cells(line)
            .into_iter()
            .skip(written)
            .map(|(offset, cell)| (base + offset, cell.to_owned()))
            .collect();
        for (offset, cell) in extra {
            self.open_container("td", None);
            self.tags.push(true);
            self.depth += 1;
            for (event, range) in Parser::new_ext(&cell, Options::empty()).into_offset_iter() {
                match event {
                    Event::Start(
                        Tag::Emphasis
                        | Tag::Strong
                        | Tag::Strikethrough
                        | Tag::Link { .. }
                        | Tag::Image { .. },
                    )
                    | Event::End(
                        TagEnd::Emphasis
                        | TagEnd::Strong
                        | TagEnd::Strikethrough
                        | TagEnd::Link
                        | TagEnd::Image,
                    )
                    | Event::Text(_)
                    | Event::Code(_)
                    | Event::Html(_)
                    | Event::InlineHtml(_)
                    | Event::SoftBreak
                    | Event::HardBreak => self.event(event, offset + range.start),
                    _ => {}
                }
            }
            self.depth -= 1;
            self.tags.pop();
            self.open.pop();
            self.markup("</td>");
        }
    }

    fn event(&mut self, event: Event<'_>, offset: usize) {
        match event {
            Event::Start(tag) => self.start(tag, offset),
            Event::End(tag) => self.end(tag),
            Event::Text(text)
            | Event::Html(text)
            | Event::InlineHtml(text)
            | Event::InlineMath(text)
            | Event::DisplayMath(text) => self.source_text(&text, offset),
            Event::Code(text) => {
                self.markup("<code>");
                self.source_text(&text, offset + 1);
                self.markup("</code>");
            }
            Event::FootnoteReference(label) => {
                let number = self.footnote_number(&label);
                self.markup(&format!(
                    "<sup class=\"footnote-ref\"><a href=\"#fn-{number}\">"
                ));
                self.text(&number.to_string(), None);
                self.markup("</a></sup>");
            }
            Event::SoftBreak => self.text("\n", None),
            Event::HardBreak => self.markup("<br>"),
            Event::Rule => {
                if self.depth == 0 {
                    self.begin_block(offset);
                    self.markup("<hr>");
                    self.end_block();
                } else {
                    self.markup("<hr>");
                }
            }
            Event::TaskListMarker(_) => {}
        }
    }
}

#[must_use]
pub fn render_markdown(source: &str) -> RenderedDocument {
    let prepared = prepare(source);
    let mut breaks = BTreeSet::new();
    let assembled = loop {
        let assembled = Assembled::new(&prepared, &breaks);
        let found = table_breaks(&assembled);
        if found.is_subset(&breaks) {
            break assembled;
        }
        breaks.extend(found);
    };
    let mut writer = Writer {
        assembled: &assembled,
        prepared: &prepared,
        next_fence: 0,
        region_stack: Vec::new(),
        document: RenderedDocument {
            regions: Vec::new(),
            blocks: Vec::new(),
            container_blocks: Vec::new(),
            runs: Vec::new(),
        },
        html: String::new(),
        depth: 0,
        block_regions: Vec::new(),
        block_first_line: 1,
        tags: Vec::new(),
        open: Vec::new(),
        footnotes: Vec::new(),
        in_head: false,
        row: None,
    };
    for (event, range) in Parser::new_ext(&assembled.text, options()).into_offset_iter() {
        writer.event(event, range.start);
    }
    writer.document
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn container_texts(document: &RenderedDocument) -> BTreeMap<u32, String> {
        let mut texts = BTreeMap::new();
        for block in &document.blocks {
            let mut stack: Vec<(String, Option<u32>)> = Vec::new();
            let mut rest = block.html.as_str();
            while !rest.is_empty() {
                if let Some(tag) = rest.strip_prefix('<') {
                    let (inside, after) = tag.split_once('>').expect("closed tag");
                    rest = after;
                    if let Some(name) = inside.strip_prefix('/') {
                        let (open, _) = stack.pop().expect("open tag");
                        assert_eq!(open, name, "balanced tags in {}", block.html);
                    } else {
                        let name = inside.split(' ').next().expect("tag name").to_owned();
                        if matches!(name.as_str(), "br" | "hr") {
                            continue;
                        }
                        let container = inside
                            .split_once("data-c=\"")
                            .and_then(|(_, value)| value.split('"').next())
                            .map(|value| value.parse().expect("container number"));
                        if let Some(container) = container {
                            texts.insert(container, String::new());
                        }
                        stack.push((name, container));
                    }
                    continue;
                }
                let end = rest.find('<').unwrap_or(rest.len());
                let (chunk, after) = rest.split_at(end);
                rest = after;
                let text = chunk
                    .replace("&lt;", "<")
                    .replace("&gt;", ">")
                    .replace("&quot;", "\"")
                    .replace("&amp;", "&");
                for (_, container) in &stack {
                    if let Some(container) = container {
                        texts.get_mut(container).expect("open").push_str(&text);
                    }
                }
            }
            assert!(stack.is_empty(), "every tag closes in {}", block.html);
        }
        texts
    }

    fn line_text(document: &RenderedDocument, line: usize) -> String {
        document
            .runs
            .iter()
            .filter(|run| run.line == line)
            .map(|run| run.text.as_str())
            .collect()
    }

    fn html(document: &RenderedDocument) -> String {
        document
            .blocks
            .iter()
            .map(|block| block.html.as_str())
            .collect()
    }

    #[test]
    fn a_table_ends_at_the_first_line_that_does_not_start_with_a_pipe() {
        let document = render_markdown("| a | b |\n|---|---|\n| 1 | 2 |\n次の文です。\n");
        assert_eq!(
            html(&document),
            "<table><thead><tr><th data-c=\"0\">a</th><th data-c=\"1\">b</th></tr></thead><tbody>\
             <tr><td data-c=\"2\">1</td><td data-c=\"3\">2</td></tr></tbody></table>\
             <p data-c=\"4\">次の文です。</p>"
        );
        assert_eq!(document.blocks.len(), 2);
        assert_eq!(line_text(&document, 4), "次の文です。");
    }

    #[test]
    fn cells_beyond_the_header_width_are_rendered() {
        let document = render_markdown("| a | b |\n|---|---|\n| 1 | 2 | *3* | `4` |\n");
        assert_eq!(
            html(&document),
            "<table><thead><tr><th data-c=\"0\">a</th><th data-c=\"1\">b</th></tr></thead><tbody>\
             <tr><td data-c=\"2\">1</td><td data-c=\"3\">2</td><td data-c=\"4\"><em>3</em></td>\
             <td data-c=\"5\"><code>4</code></td></tr></tbody></table>"
        );
        assert_eq!(line_text(&document, 3), "1234");
    }

    #[test]
    fn raw_html_is_written_as_escaped_text_and_unsafe_links_lose_their_target() {
        let document = render_markdown(
            "<script>alert(1)</script>\n\n本文<b onclick=\"x\">太字</b>です。\n\n\
             [a](javascript:alert(1)) [b](https://example.org/?q=\"x\") [c](JavaScript:x)\n",
        );
        assert_eq!(
            html(&document),
            "<p class=\"raw-html\" data-c=\"0\">&lt;script&gt;alert(1)&lt;/script&gt;\n</p>\
             <p data-c=\"1\">本文&lt;b onclick=&quot;x&quot;&gt;太字&lt;/b&gt;です。</p>\
             <p data-c=\"2\"><a>a</a> <a href=\"https://example.org/?q=&quot;x&quot;\">b</a> <a>c</a></p>"
        );
        assert_eq!(
            line_text(&document, 3),
            "本文<b onclick=\"x\">太字</b>です。"
        );
    }

    #[test]
    fn the_dialect_line_rules_apply_before_rendering() {
        let document = render_markdown(
            "---\ntitle: x\n---\n# 序論 {.intro .title}\n\n::: {.note}\n<!-- c --> 注 {.x} の文。\n\n## 小見出し\n:::\n\n```\n:::\n# 非見出し\n```\n後の文。\n",
        );
        assert_eq!(document.regions, ["note"]);
        let shape: Vec<(&str, &[u32], usize)> = document
            .blocks
            .iter()
            .map(|block| {
                (
                    block.html.as_str(),
                    block.regions.as_slice(),
                    block.first_line,
                )
            })
            .collect();
        assert_eq!(
            shape,
            [
                (
                    "<h1 class=\"intro title\" data-c=\"0\">序論</h1>",
                    &[][..],
                    4
                ),
                ("<p data-c=\"1\">注  の文。</p>", &[0][..], 7),
                ("<h2 data-c=\"2\">小見出し</h2>", &[0][..], 9),
                (
                    "<pre data-c=\"3\"><code>:::\n# 非見出し\n</code></pre>",
                    &[][..],
                    12
                ),
                ("<p data-c=\"4\">後の文。</p>", &[][..], 16),
            ]
        );
        assert_eq!(line_text(&document, 14), "# 非見出し\n");
    }

    #[test]
    fn footnotes_are_numbered_by_first_appearance() {
        let document = render_markdown("本文[^b]と[^a]。\n\n[^a]: 甲。\n[^b]: 乙。\n");
        assert_eq!(
            html(&document),
            "<p data-c=\"0\">本文<sup class=\"footnote-ref\"><a href=\"#fn-1\">1</a></sup>と\
             <sup class=\"footnote-ref\"><a href=\"#fn-2\">2</a></sup>。</p>\
             <div class=\"footnote-definition\" id=\"fn-2\"><sup class=\"footnote-label\">2</sup><p data-c=\"1\">甲。</p></div>\
             <div class=\"footnote-definition\" id=\"fn-1\"><sup class=\"footnote-label\">1</sup><p data-c=\"2\">乙。</p></div>"
        );
    }

    const LINES: &[&str] = &[
        "",
        "本文の文です。",
        "*強調* と `コード` と 😀 と &amp;",
        "| a | b |",
        "|---|---|",
        "| 1 | 2 | 3 |",
        "> 引用の文。",
        "> | x |",
        "- 項目",
        "1. 番号",
        "# 見出し {.results}",
        "::: {.note}",
        ":::",
        "```",
        "<!-- 注釈 -->",
        "<div class=\"x\">生</div>",
        "<img src=x onerror=alert(1)>",
        "[l](javascript:alert(1)) [m](http://example.org)",
        "参照[^1]。",
        "[^1]: 脚注。",
        "    字下げ",
        "---",
        "$x_1$ と {T \\to \\infty}",
    ];

    const TAGS: &[&str] = &[
        "p",
        "h1",
        "h2",
        "h3",
        "h4",
        "h5",
        "h6",
        "pre",
        "code",
        "blockquote",
        "ol",
        "ul",
        "li",
        "div",
        "sup",
        "a",
        "table",
        "thead",
        "tbody",
        "tr",
        "th",
        "td",
        "em",
        "strong",
        "del",
        "span",
        "br",
        "hr",
    ];

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        #[test]
        fn runs_address_their_text_and_only_the_writer_emits_tags(
            picks in proptest::collection::vec(0..LINES.len(), 0..24)
        ) {
            let source: String = picks.iter().map(|pick| format!("{}\n", LINES[*pick])).collect();
            let document = render_markdown(&source);
            let texts = container_texts(&document);
            for run in &document.runs {
                let text: Vec<u16> = texts[&run.container].encode_utf16().collect();
                let start = run.utf16_start as usize;
                let units: Vec<u16> = run.text.encode_utf16().collect();
                prop_assert_eq!(text.get(start..start + units.len()), Some(units.as_slice()));
                prop_assert!(run.line >= 1 && run.line <= picks.len());
            }
            for (container, block) in document.container_blocks.iter().enumerate() {
                let marker = format!("data-c=\"{container}\"");
                prop_assert!(document.blocks[*block as usize].html.contains(&marker));
            }
            for block in &document.blocks {
                for tag in block.html.split('<').skip(1) {
                    let inside = tag.split('>').next().unwrap_or_default();
                    let name = inside.trim_start_matches('/').split(' ').next().unwrap_or_default();
                    prop_assert!(TAGS.contains(&name), "tag {:?} in {}", name, block.html);
                    let mut attributes = inside.split_once(' ').map_or("", |(_, rest)| rest);
                    while let Some((key, rest)) = attributes.split_once("=\"") {
                        let (value, rest) = rest.split_once('"').unwrap_or_default();
                        prop_assert!(
                            ["class", "id", "href", "start", "data-c"].contains(&key.trim()),
                            "attribute {:?} in {}", key, block.html
                        );
                        if key.trim() == "href" {
                            prop_assert!(safe_href(value), "href {:?}", value);
                        }
                        attributes = rest;
                    }
                    prop_assert!(attributes.trim().is_empty(), "attributes {:?}", attributes);
                }
            }
        }
    }
}
