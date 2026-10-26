//! Reader for the Markdown corpus dialect.
//!
//! The dialect is not general Markdown. Its contract, verified against a
//! local 100-paper research sample (`sample-markdown-corpus/`, not tracked):
//!
//! - one sentence per line; a blank line separates paragraphs
//! - a leading `---` block is YAML frontmatter and becomes document metadata
//! - `:::+ {.classes}` opens a region; a bare `:::+` closes it
//! - `#{1,6} text {.classes}` labels the section it introduces
//! - `{.class}` spans are structure; every other `{...}` is mathematics
//! - `<!-- ... -->` is skipped; every other raw HTML form is an error
//!
//! Callers ingesting converter-produced documents may explicitly enable a
//! bounded recovery policy. Strict parsing remains the default.

use super::section_policy::SectionPolicy;
use super::structured::{AdmittedCorpus, CorpusEvent, metadata_from_json, sentence_texts};
use regex::Regex;
use std::collections::{BTreeSet, VecDeque};
use std::io::{self, BufRead};
use std::sync::LazyLock;
use std::sync::{Arc, Mutex};
use xxhash_rust::xxh3::xxh3_64;

/// Pandoc attribute spans. The leading `\.` is what distinguishes structure
/// (`{.results}`) from mathematics (`{T \to \infty}`).
static ATTRIBUTE_SPAN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\{\.[^}]*\}").expect("attribute span regex"));
static PRESENTATION_TAG: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)</?(?:center|p|div|sup|sub)(?:\s+[^>]*)?/?>").expect("presentation tag regex")
});
static BREAK_TAG: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)<br\s*/?>").expect("break tag regex"));
static SI_NOTATION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)<si(?:\s+[^>]*)?>").expect("si notation regex"));

#[derive(Debug, PartialEq, Eq)]
pub enum DivLine {
    Open(BTreeSet<String>),
    Close,
    /// A colon fence the dialect does not define. Never silently ignored.
    Malformed,
}

/// Collect dot-prefixed class names from the first attribute span in `text`.
pub fn parse_classes(text: &str) -> BTreeSet<String> {
    let Some(span) = ATTRIBUTE_SPAN.find(text) else {
        return BTreeSet::new();
    };
    span.as_str()
        .trim_matches(['{', '}'])
        .split_whitespace()
        .filter_map(|token| token.strip_prefix('.'))
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Remove pandoc attribute spans, leaving every other brace untouched.
pub fn strip_attribute_spans(line: &str) -> String {
    ATTRIBUTE_SPAN.replace_all(line, "").into_owned()
}

/// Classify a colon fence.
///
/// A fence opens only when its remainder is exactly one attribute span naming
/// at least one class. Anything else structural-looking is `Malformed`: opening
/// on `::: nonsense` would swallow the line and shift the div depth silently.
///
/// "Exactly one span" is checked against the whole remainder, not just its
/// first and last byte. `{.a} {.b}` opens and closes with braces but is two
/// spans, and `parse_classes` reads only the first, so accepting it would
/// open with `{a}` and drop `.b` silently. It is not valid pandoc; unioning the
/// two would be a guess, and this dialect rejects rather than guesses.
pub fn parse_div(line: &str) -> Option<DivLine> {
    let colons = line
        .chars()
        .take_while(|character| *character == ':')
        .count();
    if colons < 3 {
        return None;
    }
    // `colons` counts leading `:` characters; each is one ASCII byte, so the
    // byte offset equals the char count and is a char boundary.
    #[expect(
        clippy::string_slice,
        reason = "`colons` counts leading ASCII ':' bytes, a char boundary"
    )]
    let rest = line[colons..].trim();
    if rest.is_empty() {
        return Some(DivLine::Close);
    }
    // The one span `parse_classes` will read must be the whole remainder.
    let Some(span) = ATTRIBUTE_SPAN.find(rest) else {
        return Some(DivLine::Malformed);
    };
    if span.start() != 0 || span.end() != rest.len() {
        return Some(DivLine::Malformed);
    }
    let classes = parse_classes(rest);
    if classes.is_empty() {
        return Some(DivLine::Malformed);
    }
    Some(DivLine::Open(classes))
}

pub fn parse_heading(line: &str) -> Option<(usize, BTreeSet<String>)> {
    let level = line
        .chars()
        .take_while(|character| *character == '#')
        .count();
    // `level` counts leading `#` characters; each is one ASCII byte, so the
    // byte offset equals the char count and is a char boundary.
    #[expect(
        clippy::string_slice,
        reason = "`level` counts leading ASCII '#' bytes, a char boundary"
    )]
    if level == 0 || level > 6 || !line[level..].starts_with(' ') {
        return None;
    }
    #[expect(
        clippy::string_slice,
        reason = "`level` counts leading ASCII '#' bytes, a char boundary"
    )]
    let heading = &line[level..];
    Some((level, parse_classes(heading)))
}

/// The raw HTML form opening this line, if the dialect rejects it.
///
/// Covers element openers and closers, declarations, processing instructions
/// and CDATA — every raw block form except comments, which are skipped
/// elsewhere. Autolinks (`<https://…>`, `<a@b.c>`) are prose: an element name
/// is followed by whitespace, `>`, or `/`, never `:` or `@`.
pub fn raw_html_form(line: &str) -> Option<&str> {
    if line.starts_with("<!--") {
        return None;
    }
    if line.starts_with("<![CDATA[") {
        return Some("![CDATA[");
    }
    if line.starts_with("<?") {
        return Some("?");
    }
    if let Some(declaration) = line.strip_prefix("<!") {
        let end = declaration
            .find(|character: char| !character.is_ascii_alphabetic())
            .unwrap_or(declaration.len());
        // `declaration` is the line after `<!`; its first `end` bytes are all
        // ASCII alphabetic (the `find` above stops at the first non-letter),
        // so byte `2 + end` is a char boundary and `line[1]` is `!`.
        #[expect(
            clippy::string_slice,
            reason = "`<!` plus `end` ASCII alphabetic bytes ends on a char boundary"
        )]
        let form = &line[1..2 + end];
        return Some(form);
    }
    let rest = line.strip_prefix('<')?;
    let rest = rest.strip_prefix('/').unwrap_or(rest);
    let end = rest
        .find(|character: char| !character.is_ascii_alphanumeric() && character != '-')
        .unwrap_or(rest.len());
    // The first `end` bytes of `rest` are all ASCII alphanumeric or `-` (the
    // `find` above stops at the first other byte), so `end` is a char boundary.
    #[expect(
        clippy::string_slice,
        reason = "`end` is the length of a run of ASCII tag-name bytes"
    )]
    let tag = &rest[..end];
    if tag.is_empty() || !tag.starts_with(|c: char| c.is_ascii_alphabetic()) {
        return None;
    }
    // An autolink continues with `:` or `@`; an HTML tag does not.
    // `end` is the length of the ASCII tag-name run above, a char boundary.
    #[expect(
        clippy::string_slice,
        reason = "`end` is the length of a run of ASCII tag-name bytes"
    )]
    match rest[end..].chars().next() {
        None | Some('>' | '/') => Some(tag),
        Some(character) if character.is_ascii_whitespace() => Some(tag),
        _ => None,
    }
}

/// Classify a fence opener on an **already left-trimmed** line.
///
/// Indentation is the caller's business, and both fence predicates take the
/// line the same way. They must: an opener that its own closer cannot close
/// swallows every line between them, and the reader has no way to notice: an
/// opener given a trimmed line and a closer given a raw one would open an
/// 8-space fence and never close it.
///
/// A fence is a fence at any indentation. CommonMark stops recognizing one past
/// 3 spaces because at 4 it becomes an *indented code block*, but this dialect
/// does not model indented code blocks, so that rule has nothing to separate
/// here and its only effect would be to leak the fence and its contents into
/// the corpus as prose. "Fenced code is skipped" holds unconditionally instead.
pub fn opens_fence(trimmed: &str) -> Option<(char, usize)> {
    let marker = trimmed.chars().next()?;
    if !matches!(marker, '`' | '~') {
        return None;
    }
    let length = trimmed
        .chars()
        .take_while(|character| *character == marker)
        .count();
    (length >= 3).then_some((marker, length))
}

/// Whether an already left-trimmed line closes an open fence. Takes its line
/// exactly as `opens_fence` does; see there.
pub fn closes_fence(trimmed: &str, marker: char, minimum_length: usize) -> bool {
    let Some((found_marker, length)) = opens_fence(trimmed) else {
        return false;
    };
    found_marker == marker
        && length >= minimum_length
        && trimmed.chars().skip(length).all(char::is_whitespace)
}

/// Convert one source line to its text content.
///
/// Every `Text` event is joined into a single unit: under this dialect a line
/// is exactly one sentence, so splitting per event would shatter it. `Code` and
/// `Html` contents are dropped, matching the reader this replaces.
pub fn inline_text(line: &str) -> String {
    use pulldown_cmark::{Event, Parser};

    let mut text = String::with_capacity(line.len());
    for event in Parser::new(line) {
        match event {
            Event::Text(chunk) => text.push_str(&chunk),
            Event::SoftBreak | Event::HardBreak => text.push(' '),
            _ => {}
        }
    }
    text
}

fn recover_known_conversion_artifacts(line: &str) -> String {
    let line = BREAK_TAG.replace_all(line, " ");
    let line = PRESENTATION_TAG.replace_all(&line, "");
    SI_NOTATION
        .replace_all(&line, |captures: &regex::Captures<'_>| {
            let notation = captures.get(0).expect("whole regex capture").as_str();
            // `SI_NOTATION` matches `<si…>`, so the capture starts with ASCII
            // `<` and ends with ASCII `>`, making both slice ends char
            // boundaries and the capture at least 3 bytes long.
            #[expect(
                clippy::string_slice,
                reason = "SI_NOTATION capture begins with ASCII '<' and ends with ASCII '>'"
            )]
            let notation_body = &notation[1..notation.len() - 1];
            format!("&lt;{notation_body}&gt;")
        })
        .into_owned()
}

/// The two structural label carriers, tracked independently.
///
/// Divs and heading sections are separate structures; neither contains the
/// other. Both are bounded by document depth, never by document length.
///
/// `active` is cached rather than rebuilt per line: the reference corpus has
/// 26,132 prose lines against ~2,634 structure lines — roughly 10:1 — and this
/// is the pre-tokenization streaming path. (Under this dialect one prose line
/// is one sentence, so the prose-line count is the permissive sentence total.)
#[derive(Debug, Default)]
pub struct LabelStacks {
    divs: Vec<BTreeSet<String>>,
    headings: Vec<(usize, BTreeSet<String>)>,
    active: BTreeSet<String>,
}

impl LabelStacks {
    pub fn open_div(&mut self, classes: BTreeSet<String>) {
        self.divs.push(classes);
        self.refresh();
    }

    /// Close the innermost div. Returns `false` when nothing was open, which
    /// the caller reports as an error rather than absorbing.
    pub fn close_div(&mut self) -> bool {
        let closed = self.divs.pop().is_some();
        if closed {
            self.refresh();
        }
        closed
    }

    pub fn open_div_count(&self) -> usize {
        self.divs.len()
    }

    /// Replace any heading at or below `level`, then record this one.
    pub fn push_heading(&mut self, level: usize, classes: BTreeSet<String>) {
        while self
            .headings
            .last()
            .is_some_and(|(existing, _)| *existing >= level)
        {
            self.headings.pop();
        }
        self.headings.push((level, classes));
        self.refresh();
    }

    /// Labels in force: every open div, plus the nearest labeled ancestor
    /// heading.
    pub fn active(&self) -> &BTreeSet<String> {
        &self.active
    }

    /// Heading resolution takes the nearest *labeled* ancestor. Not the most
    /// recent heading: 506 corpus headings carry no class and would reset the
    /// section to nothing. Not the union of ancestors either: heading level is
    /// an unreliable section-semantics signal here, so union would hold labels
    /// active together that are not jointly in force. 246 labeled headings
    /// across 42 of the 100 documents sit under another labeled heading, and 7
    /// of those nest a bibliography under the conclusion, across 6 documents.
    /// A bibliography is
    /// not part of the conclusion, so union would label every bibliography line
    /// `conclusion` and `--md-include-sections conclusion` would pull in the
    /// references.
    ///
    /// A heading class sometimes names the section it introduces and sometimes
    /// names the heading's own text, and nothing here can tell which: this
    /// stack is a pure structure over whatever classes it is handed. The reader
    /// resolves that upstream, filtering the caller-declared self-labels out of
    /// a heading's classes before the push, so a class that reaches here always
    /// labels the section beneath it. See `DEFAULT_SELF_LABELS`.
    fn refresh(&mut self) {
        let mut labels: BTreeSet<String> = self.divs.iter().flatten().cloned().collect();
        if let Some((_, classes)) = self
            .headings
            .iter()
            .rev()
            .find(|(_, classes)| !classes.is_empty())
        {
            labels.extend(classes.iter().cloned());
        }
        self.active = labels;
    }
}

/// Labels seen in the input, shared with the caller for typo diagnostics.
///
/// A [`MarkdownDiagnostics`] sink wraps this so a caller can inject one per
/// stream and read what labels the reader observed, without the reusable
/// reader configuration owning mutable state.
pub type ObservedLabels = Arc<Mutex<BTreeSet<String>>>;

/// Heading classes that describe the heading's own text rather than the
/// section it introduces. Declared, not inferred: the corpus offers no
/// structural signal — `results` (214 headings, 0 divs) and `title` (133, 0)
/// are indistinguishable by carrier, level, or frequency.
pub const DEFAULT_SELF_LABELS: &str = "title,subtitle";
pub const DEFAULT_MAX_FRONTMATTER_BYTES: usize = 128 * 1024;
/// Schema revision for the canonical Markdown reader-tag encoding.
///
/// Revision 2 introduced bounded cumulative frontmatter. Revision 1 tags,
/// including the historical untagged default `0`, must never be reused.
pub const MARKDOWN_READER_SCHEMA_REVISION: u32 = 4;
const MARKDOWN_READER_TAG_DOMAIN: &[u8] = b"lingrep-core/markdown-reader-tag";

/// Append a label set to `encoded`: the label count, then each label's byte
/// length followed by its bytes.
///
/// Length prefixes are what make the encoding injective. Three label sets feed
/// one hash, so the encoding must recover which set each label came from, and a
/// separator can only do that while it cannot appear in the data. Labels are
/// opaque and arbitrary — `--md-self-labels` and `--md-*-sections` are free-form
/// strings and `parse_labels` only splits on `,` and trims, so `[`, `]`, `=` and
/// newlines all reach here verbatim. A label spelling a separator would shift a
/// following field into a preceding one and collide two readings onto one cache.
/// A length says how many bytes to consume without consulting them, so no label
/// content can forge a boundary. `BTreeSet` iterates sorted, so construction
/// order cannot change the result.
fn encode_labels(encoded: &mut Vec<u8>, labels: &BTreeSet<String>) {
    encoded.extend_from_slice(&(labels.len() as u64).to_le_bytes());
    for label in labels {
        encoded.extend_from_slice(&(label.len() as u64).to_le_bytes());
        encoded.extend_from_slice(label.as_bytes());
    }
}

/// How a Markdown corpus is *read*: which labelled sections are admitted,
/// which heading classes propagate, and whether converter artifacts are
/// recovered. Configuration only — it never owns observed labels, so a
/// reusable config can stream many documents without one stream's labels
/// leaking into another's diagnostics.
///
/// The labels a stream observes are written into a per-stream
/// [`MarkdownDiagnostics`] sink the caller injects at the streaming boundary;
/// see [`read_markdown_events`] and the `stream_input_*` functions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarkdownReaderConfig {
    pub policy: SectionPolicy,
    /// Heading classes that label only their own heading. Empty is legal: it
    /// means every heading class introduces a section.
    pub self_labels: BTreeSet<String>,
    recover_conversion_artifacts: bool,
    max_frontmatter_bytes: usize,
}

/// The labels one stream observed, injected per stream so a reusable
/// [`MarkdownReaderConfig`] never carries mutable state across streams.
///
/// The caller retains its own clone; the reader writes into the shared
/// `Arc<Mutex<…>>`, and the caller reads
/// [`MarkdownDiagnostics::observed_labels`] after the stream completes. Two
/// sinks are deliberately not `PartialEq`: the identity of a sink is not the
/// content it accumulated, and a sink is a write-target, not a value.
#[derive(Debug, Clone, Default)]
pub struct MarkdownDiagnostics(ObservedLabels);

impl MarkdownDiagnostics {
    /// The labels this sink has recorded so far.
    pub fn observed_labels(&self) -> BTreeSet<String> {
        self.0.lock().expect("observed labels lock").clone()
    }

    /// Records one label this stream observed. The Markdown dialect and the
    /// BCCWJ reader both feed the same unused-label report through this.
    pub fn observe_label(&self, label: &str) {
        self.0
            .lock()
            .expect("observed labels lock")
            .insert(label.to_owned());
    }
}

impl MarkdownReaderConfig {
    pub fn new(policy: SectionPolicy) -> Self {
        Self::with_self_labels(policy, SectionPolicy::parse_labels(DEFAULT_SELF_LABELS))
    }

    pub fn with_self_labels(policy: SectionPolicy, self_labels: BTreeSet<String>) -> Self {
        Self {
            policy,
            recover_conversion_artifacts: false,
            self_labels,
            max_frontmatter_bytes: DEFAULT_MAX_FRONTMATTER_BYTES,
        }
    }

    /// Recover a narrow set of artifacts emitted by the Markdown corpus
    /// converter: presentational HTML, `<si ...>` linguistic notation, and
    /// div imbalance at the physical document boundary.
    #[must_use]
    pub fn recover_conversion_artifacts(mut self) -> Self {
        self.recover_conversion_artifacts = true;
        self
    }

    /// Set the cumulative YAML frontmatter byte limit, including one logical
    /// newline byte for every retained physical line.
    #[must_use]
    pub fn with_max_frontmatter_bytes(mut self, max_frontmatter_bytes: usize) -> Self {
        self.max_frontmatter_bytes = max_frontmatter_bytes;
        self
    }

    /// An opaque discriminator for the corpus this reader configuration
    /// produces, for use as part of a token cache key: two configurations that
    /// could emit different sentences from one file must not share a cache.
    ///
    /// Every semantic field that can move a line into or out of a successful
    /// emitted corpus must reach the tag: the policy decides which labelled
    /// sections are admitted; the self-label set decides which heading classes
    /// propagate, and so which lines the policy ever sees; recovery decides
    /// whether lines carrying converter artifacts are read at all or rejected.
    /// Two runs differing only in any one of them read two different corpora,
    /// and each omission has already served one run's cache to the other.
    ///
    /// `max_frontmatter_bytes` is deliberately not semantic identity. It is a
    /// resource guard: a value can reject a document, but two sufficiently
    /// large values that both complete emit the same corpus and must select the
    /// same reader and analysis identities.
    ///
    /// A method, not a free function taking the parts: this is the mechanism the
    /// omissions above came from. Computed from loose arguments away from the
    /// struct, a field could be added to `MarkdownReaderConfig` and never reach
    /// the key — which is exactly how `recover_conversion_artifacts` arrived.
    /// The destructure below is load-bearing for the same reason: it names
    /// every field with no `..` rest pattern, so a new field is a compile error
    /// here and someone must decide whether it changes what is read.
    ///
    /// The schema domain and revision are always encoded. A changed reader
    /// schema therefore cannot silently claim a cache produced by an older
    /// interpretation of the same source bytes.
    pub fn reader_tag(&self) -> u64 {
        let Self {
            policy,
            self_labels,
            recover_conversion_artifacts,
            max_frontmatter_bytes: _max_frontmatter_bytes,
        } = self;

        let (include, exclude) = policy.canonical_fields();
        let mut encoded = Vec::new();
        encoded.extend_from_slice(MARKDOWN_READER_TAG_DOMAIN);
        encoded.extend_from_slice(&MARKDOWN_READER_SCHEMA_REVISION.to_le_bytes());
        // A fixed-width leading field, so it cannot be confused with the
        // length-prefixed label data that follows it.
        encoded.push(u8::from(*recover_conversion_artifacts));
        // `None` ("admit every label") and `Some(empty)` ("admit nothing") are
        // opposite readings carrying no labels to tell them apart, so the variant is
        // marked rather than inferred: the stream then holds a fixed three fields
        // whichever variant this is, and stays decodable without knowing the policy.
        match include {
            None => encoded.push(0u8),
            Some(include) => {
                encoded.push(1u8);
                encode_labels(&mut encoded, include);
            }
        }
        encode_labels(&mut encoded, exclude);
        encode_labels(&mut encoded, self_labels);

        let tag = xxh3_64(&encoded);
        // Zero means the historical untagged reader at the cache boundary.
        // xxh3 does not promise a nonzero output, so reserve it explicitly.
        if tag == 0 { 1 } else { tag }
    }
}

#[derive(Debug, Clone, Copy)]
/// Every non-`Body` variant carries the line that opened it, so a block cannot
/// be entered without recording where — the alternative, a line field alongside
/// `block`, can drift out of step with it and go unnoticed.
enum Block {
    Frontmatter {
        opened_at: usize,
    },
    FencedCode {
        marker: char,
        minimum_length: usize,
        opened_at: usize,
    },
    Comment {
        opened_at: usize,
    },
    Body,
}

impl Block {
    /// The construct this block is waiting to have terminated and the line that
    /// opened it, or `None` for `Body` — the one state in which the input may
    /// legitimately end.
    fn unterminated(self) -> Option<(&'static str, usize)> {
        match self {
            Block::Frontmatter { opened_at } => Some(("YAML frontmatter", opened_at)),
            Block::FencedCode { opened_at, .. } => Some(("code fence", opened_at)),
            Block::Comment { opened_at } => Some(("`<!--` comment", opened_at)),
            Block::Body => None,
        }
    }
}

struct DialectIterator<R> {
    lines: std::io::Lines<R>,
    block: Block,
    stacks: LabelStacks,
    /// The line that opened each currently open div, outermost first, mirroring
    /// `stacks`' div stack.
    ///
    /// Kept here rather than in `LabelStacks` because that stack is a pure
    /// structure over the classes it is handed — the same decision that keeps
    /// self-labels out of it. A source line is provenance, not a label: it would
    /// be threaded through `open_div` and ignored by `refresh`/`active`, earning
    /// the type nothing, while the line number it needs already lives here.
    div_lines: Vec<usize>,
    division_ids: Vec<u64>,
    next_division_id: u64,
    config: MarkdownReaderConfig,
    /// The per-stream sink the reader writes observed labels into. Never shared
    /// with `config`, so the reusable configuration carries no mutable state.
    diagnostics: MarkdownDiagnostics,
    line_number: usize,
    started: bool,
    drained: bool,
    unmatched_close_at_end: bool,
    source_name: String,
    frontmatter: String,
    document_started: bool,
    paragraph_open: bool,
    pending: VecDeque<CorpusEvent>,
}

pub fn read_markdown_events<R>(
    reader: R,
    config: MarkdownReaderConfig,
    diagnostics: MarkdownDiagnostics,
    source_name: String,
) -> impl Iterator<Item = io::Result<CorpusEvent>> + Send
where
    R: BufRead + Send + 'static,
{
    DialectIterator {
        lines: reader.lines(),
        block: Block::Body,
        stacks: LabelStacks::default(),
        div_lines: Vec::new(),
        division_ids: Vec::new(),
        next_division_id: 0,
        config,
        diagnostics,
        line_number: 0,
        started: false,
        drained: false,
        unmatched_close_at_end: false,
        source_name,
        frontmatter: String::new(),
        document_started: false,
        paragraph_open: false,
        pending: VecDeque::new(),
    }
}

pub fn read_markdown_dialect<R>(
    reader: R,
    config: MarkdownReaderConfig,
    diagnostics: MarkdownDiagnostics,
) -> impl Iterator<Item = io::Result<String>> + Send
where
    R: BufRead + Send + 'static,
{
    sentence_texts(AdmittedCorpus::new(read_markdown_events(
        reader,
        config,
        diagnostics,
        "<markdown>".into(),
    )))
}

impl<R: BufRead> Iterator for DialectIterator<R> {
    type Item = io::Result<CorpusEvent>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(event) = self.queued_event() {
                return Some(Ok(event));
            }
            let Some(line) = self.lines.next() else {
                return self.finish();
            };
            let line = match line {
                Ok(line) => line,
                Err(error) => return Some(Err(error)),
            };
            self.line_number += 1;
            match self.consume(&line) {
                Ok(Some(event)) => {
                    self.pending.push_back(event);
                    return self.queued_event().map(Ok);
                }
                Ok(None) => continue,
                Err(error) => return Some(Err(error)),
            }
        }
    }
}

impl<R: BufRead> DialectIterator<R> {
    /// Report structure left open when the input ends. Emitted once.
    ///
    /// `Block::Body` is the only legitimate terminal state: every other block
    /// swallows the lines that follow it, so ending inside one means the rest
    /// of the document was silently discarded. That is reported ahead of the
    /// div depth, because an unterminated block is what makes the depth
    /// untrustworthy — a `:::` inside an unclosed fence was never counted, so
    /// the depth is a symptom and the open block is the cause.
    ///
    /// Both name the line that opened the structure, not the line the input ran
    /// out on: "at end of input" is where the reader noticed, never where the
    /// fault is. For divs that is the *outermost* open one, which together with
    /// the count bounds the suspect region to [that line, end of input] — the
    /// innermost bounds nothing, since the rest opened earlier and unnamed.
    fn finish(&mut self) -> Option<io::Result<CorpusEvent>> {
        if self.drained {
            return self.queued_event().map(Ok);
        }
        self.drained = true;
        if let Some((construct, opened_at)) = self.block.unterminated() {
            return Some(Err(self.at(
                opened_at,
                format!(
                    "unclosed {construct} at end of input; \
                     the rest of the document was not read"
                ),
            )));
        }
        let open = self.stacks.open_div_count();
        debug_assert_eq!(
            self.div_lines.len(),
            open,
            "a tracked opening line per open div"
        );
        // Recovery accepts div imbalance at the physical document boundary. A
        // strict read still refuses it, and names the line that opened it: the
        // count alone bounds nothing in a truncated paper.
        if !self.config.recover_conversion_artifacts
            && let Some(&opened_at) = self.div_lines.first()
        {
            return Some(Err(self.at(
                opened_at,
                format!("unclosed pandoc div at end of input ({open} still open)"),
            )));
        }
        if !self.document_started {
            self.document_started = true;
            self.pending.push_back(CorpusEvent::DocumentStart {
                source_name: self.source_name.clone(),
                metadata: Box::new([]),
            });
        }
        if self.paragraph_open {
            self.paragraph_open = false;
            self.pending.push_back(CorpusEvent::ParagraphEnd);
        }
        while let Some(source_ordinal) = self.division_ids.pop() {
            self.pending
                .push_back(CorpusEvent::DivisionEnd { source_ordinal });
        }
        self.pending.push_back(CorpusEvent::DocumentEnd);
        self.queued_event().map(Ok)
    }

    fn queued_event(&mut self) -> Option<CorpusEvent> {
        let event = self.pending.pop_front()?;
        if matches!(event, CorpusEvent::DocumentStart { .. }) {
            for kind in ["paragraph", "section", "region"].into_iter().rev() {
                self.pending.push_front(CorpusEvent::DivisionObservation {
                    kind: kind.into(),
                    observation: super::DivisionObservation::Observed,
                });
            }
        }
        Some(event)
    }

    fn invalid(&self, message: String) -> io::Error {
        io::Error::new(io::ErrorKind::InvalidData, message)
    }

    fn at(&self, line: usize, message: String) -> io::Error {
        self.invalid(format!("line {line}: {message}"))
    }

    fn at_line(&self, message: String) -> io::Error {
        self.at(self.line_number, message)
    }

    fn observe(&self, classes: &BTreeSet<String>) {
        if classes.is_empty() {
            return;
        }
        if let Ok(mut observed) = self.diagnostics.0.lock() {
            observed.extend(classes.iter().cloned());
        }
    }

    fn consume(&mut self, line: &str) -> io::Result<Option<CorpusEvent>> {
        // Frontmatter exists only when the document opens with `---`. A `---`
        // anywhere else is a thematic break.
        if !self.started {
            self.started = true;
            if line.trim_end() == "---" {
                self.block = Block::Frontmatter {
                    opened_at: self.line_number,
                };
                return Ok(None);
            }
            let document = self.start_document(Box::new([]));
            self.pending.push_back(document);
        }

        match self.block {
            Block::Frontmatter { .. } => {
                if line.trim_end() == "---" {
                    self.block = Block::Body;
                    let value = serde_saphyr::from_str::<serde_json::Value>(&self.frontmatter)
                        .map_err(|error| {
                            self.at_line(format!("failed to parse YAML frontmatter: {error}"))
                        })?;
                    let metadata = metadata_from_json(value, &self.source_name)?;
                    return Ok(Some(self.start_document(metadata)));
                }
                let additional = line
                    .len()
                    .checked_add(1)
                    .ok_or_else(|| self.at_line("YAML frontmatter byte count overflow".into()))?;
                let observed = self
                    .frontmatter
                    .len()
                    .checked_add(additional)
                    .ok_or_else(|| self.at_line("YAML frontmatter byte count overflow".into()))?;
                if observed > self.config.max_frontmatter_bytes {
                    return Err(self.at_line(format!(
                        "YAML frontmatter is at least {observed} bytes; limit is {}",
                        self.config.max_frontmatter_bytes
                    )));
                }
                self.frontmatter.push_str(line);
                self.frontmatter.push('\n');
                return Ok(None);
            }
            Block::Comment { .. } => {
                if line.contains("-->") {
                    self.block = Block::Body;
                }
                return Ok(None);
            }
            Block::FencedCode {
                marker,
                minimum_length,
                ..
            } => {
                // Trimmed, exactly as the opener was: the two sides classifying
                // a fence differently is what swallows documents.
                if closes_fence(line.trim_start(), marker, minimum_length) {
                    self.block = Block::Body;
                }
                return Ok(None);
            }
            Block::Body => {}
        }

        // `content` is the line minus any comments that prefix it. It is what
        // gets emitted, so the tail of a closed comment reaches the corpus
        // instead of being dropped with the comment that preceded it.
        let mut content = line;
        let mut trimmed = content.trim_start();
        if trimmed.is_empty() {
            return Ok(self.close_paragraph());
        }

        if self.unmatched_close_at_end && !matches!(parse_div(trimmed), Some(DivLine::Close)) {
            return Err(self.at_line(
                "content follows an unmatched pandoc div close; only trailing closes can be recovered"
                    .to_owned(),
            ));
        }

        // A comment is a prefix, not a licence to drop the rest of the line.
        // Comments do not nest, so each ends at its *first* `-->`: scanning to
        // the last would read `<!-- a --> 本文 <!-- b -->` as one comment and
        // discard 本文 — the same silent drop, one line further along. Reducing
        // one comment at a time also leaves a reopened comment visible to the
        // next turn, so `<!-- a --> <!-- b` still opens the block.
        //
        // The tail falls through to the ordinary classification below rather
        // than being emitted: a `:::` fence behind a comment is still a fence,
        // and raw HTML behind one is still an error.
        while trimmed.starts_with("<!--") {
            let Some(end) = trimmed.find("-->") else {
                self.block = Block::Comment {
                    opened_at: self.line_number,
                };
                return Ok(None);
            };
            // `end` is `trimmed.find("-->")`, and `-->` is 3 ASCII bytes, so
            // `end + 3` lands just past the marker on a char boundary.
            #[expect(
                clippy::string_slice,
                reason = "`end` is a char boundary and '-->' is 3 ASCII bytes"
            )]
            let tail = &trimmed[end + "-->".len()..];
            trimmed = tail.trim_start();
            if trimmed.is_empty() {
                return Ok(None);
            }
            content = trimmed;
        }

        // Recovery normalizes what the comment loop left, not the raw line: the
        // innards of a stripped comment must not decide whether the surviving
        // tail is recoverable. It runs before `raw_html_form` because that is
        // the rejection it exists to pre-empt.
        let recovered;
        let (content, trimmed) = if self.config.recover_conversion_artifacts {
            recovered = recover_known_conversion_artifacts(content);
            let recovered_trimmed = recovered.trim_start();
            if recovered_trimmed.is_empty() {
                return Ok(None);
            }
            (recovered.as_str(), recovered_trimmed)
        } else {
            (content, trimmed)
        };

        if let Some(form) = raw_html_form(trimmed) {
            return Err(self.at_line(format!(
                "raw HTML `<{form}>` is not part of the Markdown corpus dialect; \
                 only `<!-- ... -->` comments are supported"
            )));
        }

        if let Some((marker, minimum_length)) = opens_fence(trimmed) {
            self.block = Block::FencedCode {
                marker,
                minimum_length,
                opened_at: self.line_number,
            };
            return Ok(None);
        }

        if let Some(div) = parse_div(trimmed) {
            match div {
                DivLine::Open(classes) => {
                    self.observe(&classes);
                    let metadata = metadata_from_json(
                        serde_json::json!({"classes":classes.iter().collect::<Vec<_>>()}),
                        "Markdown region",
                    )?;
                    let source_ordinal = self.next_division_id;
                    self.next_division_id = source_ordinal
                        .checked_add(1)
                        .ok_or_else(|| self.at_line("division ID overflow".into()))?;
                    let parent_source_ordinal = self.division_ids.last().copied();
                    self.division_ids.push(source_ordinal);
                    self.pending.push_back(CorpusEvent::DivisionStart {
                        source_id: None,
                        source_ordinal,
                        kind: "region".into(),
                        parent_source_ordinal,
                        metadata,
                    });
                    self.stacks.open_div(classes);
                    self.div_lines.push(self.line_number);
                }
                DivLine::Close => {
                    if !self.stacks.close_div() {
                        if self.config.recover_conversion_artifacts {
                            self.unmatched_close_at_end = true;
                        } else {
                            return Err(self.at_line(
                                "pandoc div fence closes a div that was never opened".to_owned(),
                            ));
                        }
                    }
                    self.div_lines.pop();
                    if let Some(source_ordinal) = self.division_ids.pop() {
                        self.pending
                            .push_back(CorpusEvent::DivisionEnd { source_ordinal });
                    }
                }
                DivLine::Malformed => {
                    return Err(self.at_line(
                        "colon fence is neither `:::+ {.class}` nor a bare closing fence"
                            .to_owned(),
                    ));
                }
            }
            return Ok(None);
        }

        // A heading labels the section it introduces. It is structure, not a
        // sentence, so it updates the stacks and yields nothing.
        //
        // Self-labels are filtered out before the push: they describe the
        // heading's own text, which is never emitted, so propagating them would
        // label the whole section beneath with something that names nothing.
        // `LabelStacks` stays a pure structure and never learns the concept.
        if let Some((level, classes)) = parse_heading(trimmed) {
            self.observe(&classes);
            let introduces = classes
                .difference(&self.config.self_labels)
                .cloned()
                .collect();
            self.stacks.push_heading(level, introduces);
            // `level` comes from `parse_heading`'s count of leading ASCII '#'
            // on the same `trimmed`, so it is a char boundary.
            #[expect(
                clippy::string_slice,
                reason = "`level` counts leading ASCII '#' bytes of `trimmed`"
            )]
            let text = inline_text(&strip_attribute_spans(trimmed[level..].trim_start()))
                .trim()
                .to_owned();
            return Ok(Some(CorpusEvent::Heading {
                level: level as u8,
                text,
                classes: classes.into_iter().collect(),
            }));
        }

        if !self.config.policy.admits(self.stacks.active()) {
            return Ok(None);
        }

        let sentence = inline_text(&strip_attribute_spans(content));
        let event = CorpusEvent::Sentence {
            text: sentence,
            words: None,
            enhanced_graph: None,
            active_labels: self.stacks.active().iter().cloned().collect(),
            metadata: Box::new([]),
        };
        if !self.paragraph_open {
            self.paragraph_open = true;
            self.pending.push_back(CorpusEvent::ParagraphStart {
                metadata: Box::new([]),
            });
        }
        Ok(Some(event))
    }

    fn start_document(&mut self, metadata: super::structured::Metadata) -> CorpusEvent {
        self.document_started = true;
        CorpusEvent::DocumentStart {
            source_name: self.source_name.clone(),
            metadata,
        }
    }

    fn close_paragraph(&mut self) -> Option<CorpusEvent> {
        if self.paragraph_open {
            self.paragraph_open = false;
            Some(CorpusEvent::ParagraphEnd)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod reader_tag_tests {
    use super::*;
    use proptest::prelude::*;

    fn labels(spec: &str) -> BTreeSet<String> {
        SectionPolicy::parse_labels(spec)
    }

    fn default_self_labels() -> BTreeSet<String> {
        labels(DEFAULT_SELF_LABELS)
    }

    /// A label set built without going through `parse_labels`' comma split, so a
    /// test can name a label containing whatever bytes it likes.
    fn raw_labels<const N: usize>(names: [&str; N]) -> BTreeSet<String> {
        names.iter().map(|name| (*name).to_owned()).collect()
    }

    /// The tag of a *strict* reading — conversion-artifact recovery off — which
    /// is the reading every aliasing test below is written under.
    ///
    /// Recovery is named in the helper rather than left to a default, because
    /// the whole point of the change this helper serves is that the tag is a
    /// function of a whole `MarkdownReaderConfig`. A helper spelled `tag(policy,
    /// self_labels)` would hide the third field the same way the free function
    /// did, and the next field would hide behind it too.
    fn strict_tag(policy: SectionPolicy, self_labels: BTreeSet<String>) -> u64 {
        let options = MarkdownReaderConfig::with_self_labels(policy, self_labels);
        assert!(
            !options.recover_conversion_artifacts,
            "strict_tag must describe a strict read"
        );
        options.reader_tag()
    }

    /// The same reading with conversion-artifact recovery on.
    fn recovering_tag(policy: SectionPolicy, self_labels: BTreeSet<String>) -> u64 {
        MarkdownReaderConfig::with_self_labels(policy, self_labels)
            .recover_conversion_artifacts()
            .reader_tag()
    }

    fn reference_encode_labels(encoded: &mut Vec<u8>, labels: &BTreeSet<String>) {
        encoded.extend_from_slice(&(labels.len() as u64).to_le_bytes());
        for label in labels {
            encoded.extend_from_slice(&(label.len() as u64).to_le_bytes());
            encoded.extend_from_slice(label.as_bytes());
        }
    }

    fn reference_tag(
        include: Option<&BTreeSet<String>>,
        exclude: &BTreeSet<String>,
        self_labels: &BTreeSet<String>,
        recovery: bool,
    ) -> u64 {
        let mut encoded = b"lingrep-core/markdown-reader-tag".to_vec();
        encoded.extend_from_slice(&4u32.to_le_bytes());
        encoded.push(u8::from(recovery));
        match include {
            None => encoded.push(0),
            Some(labels) => {
                encoded.push(1);
                reference_encode_labels(&mut encoded, labels);
            }
        }
        reference_encode_labels(&mut encoded, exclude);
        reference_encode_labels(&mut encoded, self_labels);
        match xxh3_64(&encoded) {
            0 => 1,
            tag => tag,
        }
    }

    fn bounded_label_set() -> impl Strategy<Value = BTreeSet<String>> {
        proptest::collection::btree_set(
            prop::sample::select(vec![
                String::new(),
                "a".to_owned(),
                "ab".to_owned(),
                "bc".to_owned(),
                "title".to_owned(),
                "references".to_owned(),
                "]\nself=[".to_owned(),
                "]\nexclude=[".to_owned(),
            ]),
            0..4,
        )
    }

    #[test]
    fn semantic_reader_fields_have_distinct_tags_in_named_bounded_examples() {
        let baseline = strict_tag(
            SectionPolicy::new(None, labels("references")),
            labels("title"),
        );
        let variants = [
            recovering_tag(
                SectionPolicy::new(None, labels("references")),
                labels("title"),
            ),
            strict_tag(
                SectionPolicy::new(Some(labels("methods")), labels("references")),
                labels("title"),
            ),
            strict_tag(SectionPolicy::new(None, labels("caption")), labels("title")),
            strict_tag(
                SectionPolicy::new(None, labels("references")),
                labels("subtitle"),
            ),
        ];
        for variant in variants {
            assert_ne!(baseline, variant);
        }
        assert_ne!(
            baseline, 0,
            "zero is reserved for the historical untagged reader"
        );
    }

    #[test]
    fn default_strict_and_recovering_readers_remain_distinct_nonzero() {
        let strict = strict_tag(SectionPolicy::default(), default_self_labels());
        let recovering = recovering_tag(SectionPolicy::default(), default_self_labels());
        assert_ne!(strict, 0);
        assert_ne!(recovering, 0);
        assert_ne!(strict, recovering);
    }

    #[test]
    fn canonical_encoding_resists_historical_separator_forgeries() {
        let pairs = [
            (
                strict_tag(
                    SectionPolicy::new(None, raw_labels(["ab"])),
                    raw_labels(["c"]),
                ),
                strict_tag(
                    SectionPolicy::new(None, raw_labels(["a"])),
                    raw_labels(["bc"]),
                ),
            ),
            (
                strict_tag(
                    SectionPolicy::new(None, raw_labels(["a", "bc"])),
                    default_self_labels(),
                ),
                strict_tag(
                    SectionPolicy::new(None, raw_labels(["ab", "c"])),
                    default_self_labels(),
                ),
            ),
            (
                strict_tag(
                    SectionPolicy::new(None, raw_labels(["a"])),
                    raw_labels(["b]\nself=[c"]),
                ),
                strict_tag(
                    SectionPolicy::new(None, raw_labels(["a]\nself=[b"])),
                    raw_labels(["c"]),
                ),
            ),
            (
                strict_tag(
                    SectionPolicy::new(Some(raw_labels(["x]\nexclude=[y"])), BTreeSet::new()),
                    default_self_labels(),
                ),
                strict_tag(
                    SectionPolicy::new(Some(raw_labels(["x"])), raw_labels(["y]\nexclude=["])),
                    default_self_labels(),
                ),
            ),
        ];
        for (left, right) in pairs {
            assert_ne!(left, right);
        }
    }

    #[test]
    fn option_and_policy_fields_have_distinct_tags_in_named_bounded_examples() {
        let self_labels = default_self_labels();
        let admit_all = strict_tag(
            SectionPolicy::new(None, BTreeSet::new()),
            self_labels.clone(),
        );
        let admit_none = strict_tag(
            SectionPolicy::new(Some(BTreeSet::new()), BTreeSet::new()),
            self_labels.clone(),
        );
        assert_ne!(admit_all, admit_none);

        let include_side = strict_tag(
            SectionPolicy::new(Some(labels("references")), BTreeSet::new()),
            self_labels.clone(),
        );
        let exclude_side = strict_tag(SectionPolicy::new(None, labels("references")), self_labels);
        assert_ne!(include_side, exclude_side);
    }

    #[test]
    fn the_frontmatter_resource_bound_is_not_part_of_semantic_reader_identity() {
        let default = MarkdownReaderConfig::new(SectionPolicy::default()).reader_tag();
        let tighter = MarkdownReaderConfig::new(SectionPolicy::default())
            .with_max_frontmatter_bytes(DEFAULT_MAX_FRONTMATTER_BYTES - 1)
            .reader_tag();

        assert_ne!(default, 0);
        assert_ne!(tighter, 0);
        assert_eq!(default, tighter);
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]

        // Differential, not injectivity: the finite generator uses eight literal
        // labels with at most three labels per field and compares the production
        // tag to an independently assembled canonical byte encoding.
        #[test]
        fn bounded_reader_tags_match_the_canonical_encoding(
            include in prop_oneof![Just(None), bounded_label_set().prop_map(Some)],
            exclude in bounded_label_set(),
            self_labels in bounded_label_set(),
            recovery in any::<bool>(),
            max_frontmatter_bytes in 0usize..(DEFAULT_MAX_FRONTMATTER_BYTES * 2),
        ) {
            let policy = SectionPolicy::new(include.clone(), exclude.clone());
            let mut config = MarkdownReaderConfig::with_self_labels(policy, self_labels.clone())
                .with_max_frontmatter_bytes(max_frontmatter_bytes);
            if recovery {
                config = config.recover_conversion_artifacts();
            }
            prop_assert_eq!(
                config.reader_tag(),
                reference_tag(include.as_ref(), &exclude, &self_labels, recovery)
            );
            prop_assert_ne!(config.reader_tag(), 0);
        }
    }
}

#[cfg(test)]
mod primitive_tests {
    use super::*;

    fn labels(spec: &str) -> BTreeSet<String> {
        spec.split(',')
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect()
    }

    #[test]
    fn parse_classes_reads_dot_prefixed_names_including_underscores() {
        assert_eq!(
            parse_classes("{.literature_review}"),
            labels("literature_review")
        );
        assert_eq!(
            parse_classes("{.literature_review .methods}"),
            labels("literature_review,methods")
        );
        assert_eq!(parse_classes("no attributes here"), labels(""));
    }

    #[test]
    fn attribute_span_stripping_table() {
        let cases = [
            ("見出し {.methods}", "見出し "),
            ("x {.a .b} y", "x  y"),
            // Non-dot braces are mathematics, not structure.
            ("値は {ij} です", "値は {ij} です"),
            ("{T \\to \\infty}", "{T \\to \\infty}"),
        ];
        for (input, expected) in cases {
            assert_eq!(strip_attribute_spans(input), expected, "{input:?}");
        }
    }

    #[test]
    fn div_line_classification_table() {
        let cases = [
            ("::::: {.author}", Some(DivLine::Open(labels("author")))),
            ("::: {.a .b}", Some(DivLine::Open(labels("a,b")))),
            (":::::", Some(DivLine::Close)),
            ("::", None),
            ("普通の文です。", None),
            // Structural-looking garbage must be loud, not shift div depth.
            ("::: nonsense", Some(DivLine::Malformed)),
            ("::: {#id}", Some(DivLine::Malformed)),
            ("::: {.a} trailing", Some(DivLine::Malformed)),
        ];
        for (input, expected) in cases {
            assert_eq!(parse_div(input), expected, "{input:?}");
        }
    }

    #[test]
    fn a_div_fence_with_several_attribute_spans_is_malformed_not_half_read() {
        // `parse_classes` reads only the first span, so `::: {.a} {.b}` opened
        // with `{a}` and dropped `.b` silently. It is not valid pandoc, so
        // it is rejected; unioning the classes would be a guess, and this
        // parser avoids guessing.
        assert!(matches!(
            parse_div("::: {.a} {.b}"),
            Some(DivLine::Malformed)
        ));

        // The single-span fence — the only form the corpus contains — is
        // untouched: one span covering the whole remainder still opens.
        assert!(matches!(parse_div("::: {.a}"), Some(DivLine::Open(_))));
        assert!(matches!(parse_div("::: {.a .b}"), Some(DivLine::Open(_))));

        // The whole-remainder check replaced a `starts_with('{')` /
        // `ends_with('}')` pair and must still refuse what that pair refused:
        // a span preceded by text ends where the remainder ends, so only the
        // span's *start* rejects this one.
        assert!(matches!(parse_div("::: x{.a}"), Some(DivLine::Malformed)));

        // A `}` inside an attribute value ends the span early, so the remainder
        // is no longer one span even though it opens `{` and closes `}`. Pandoc
        // would accept it; this dialect does not define it, and it now says so
        // instead of opening with a half-read class list.
        assert!(matches!(
            parse_div("::: {.a name=\"x}y\"}"),
            Some(DivLine::Malformed)
        ));
    }

    #[test]
    fn parse_heading_reads_level_and_classes() {
        assert_eq!(
            parse_heading("## 2. 方法 {.methods}"),
            Some((2, labels("methods")))
        );
        assert_eq!(parse_heading("### (1) 課題"), Some((3, labels(""))));
        assert_eq!(parse_heading("#hashtag"), None);
        assert_eq!(parse_heading("####### too deep"), None);
    }

    #[test]
    fn raw_html_classification_table() {
        let cases = [
            ("<div class=\"x\">", Some("div")),
            ("</div>", Some("div")),
            ("<br/>", Some("br")),
            ("<!DOCTYPE html>", Some("!DOCTYPE")),
            ("<?xml version=\"1.0\"?>", Some("?")),
            ("<![CDATA[x]]>", Some("![CDATA[")),
            ("<!-- TABLE REMOVED START -->", None),
            // Autolinks and ordinary prose are not HTML blocks.
            ("<https://example.com/a>", None),
            ("<user@example.com>", None),
            ("普通の文です。", None),
            ("5 < 6 です", None),
        ];
        for (input, expected) in cases {
            assert_eq!(raw_html_form(input), expected, "{input:?}");
        }
    }

    #[test]
    fn inline_text_joins_events_into_one_unit() {
        // The reader this replaces emitted one unit per Text event, shattering
        // sentences: two sentences became six units.
        assert_eq!(
            inline_text("これは**強調**された文です。"),
            "これは強調された文です。"
        );
        assert_eq!(
            inline_text("次の行は[リンク](http://x)を含む文です。"),
            "次の行はリンクを含む文です。"
        );
    }

    #[test]
    fn fence_predicate_table_and_indentation_invariant() {
        let open_cases = [
            ("```rust", Some(('`', 3))),
            ("~~~~", Some(('~', 4))),
            ("``", None),
            ("普通の文", None),
        ];
        for (input, expected) in open_cases {
            assert_eq!(opens_fence(input), expected, "{input:?}");
        }
        let close_cases = [
            ("```", true),
            ("```   ", true),
            ("~~~", false),
            ("``` x", false),
            ("``", false),
        ];
        for (input, expected) in close_cases {
            assert_eq!(closes_fence(input, '`', 3), expected, "{input:?}");
        }

        // The invariant, not an example of it: whatever opens must close. When
        // the two disagreed, the lines between an opener and the closer that
        // could not close it vanished with no diagnostic. Both take the trimmed
        // line, so indentation cannot make them diverge.
        for indentation in ["", " ", "   ", "        "] {
            let fence = format!("{indentation}```");
            let trimmed = fence.trim_start();
            assert_eq!(opens_fence(trimmed), Some(('`', 3)), "{fence:?}");
            assert!(closes_fence(trimmed, '`', 3), "{fence:?}");
        }
    }
}

#[cfg(test)]
mod stack_tests {
    use super::*;
    use proptest::prelude::*;

    fn labels(spec: &str) -> BTreeSet<String> {
        spec.split(',')
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect()
    }

    #[test]
    fn closing_a_div_keeps_a_label_an_enclosing_div_still_carries() {
        // `refresh` rebuilds `active` from the divs that remain, so a label two
        // divs share survives the inner one closing. Subtracting the closed
        // div's classes instead would drop `figure` here while still passing
        // `nested_divs_union_their_classes`, whose labels are disjoint. The
        // cache exists for throughput, which is exactly the pressure that
        // invites an incremental rewrite — so the property is pinned, not left
        // correct-by-construction.
        let mut stacks = LabelStacks::default();
        stacks.open_div(labels("figure"));
        stacks.open_div(labels("caption,figure"));
        assert_eq!(*stacks.active(), labels("caption,figure"));
        assert!(stacks.close_div());
        assert_eq!(*stacks.active(), labels("figure"));
    }

    #[test]
    fn an_unmatched_close_is_reported_not_ignored() {
        let mut stacks = LabelStacks::default();
        assert!(!stacks.close_div(), "an unmatched close must be reportable");
    }

    #[test]
    fn classless_subheadings_inherit_the_nearest_labeled_ancestor() {
        // 506 corpus headings carry no class; they must not reset the section.
        let mut stacks = LabelStacks::default();
        stacks.push_heading(2, labels("methods"));
        stacks.push_heading(3, labels(""));
        assert_eq!(*stacks.active(), labels("methods"));
    }

    #[test]
    fn a_level_one_title_does_not_label_the_whole_document() {
        // {.title} sits on 120 level-1 headings. Union-of-ancestors would make
        // every line in those papers a `title` line, and the default policy
        // would discard the paper.
        let mut stacks = LabelStacks::default();
        stacks.push_heading(1, labels("title"));
        assert_eq!(*stacks.active(), labels("title"));
        stacks.push_heading(2, labels("introduction"));
        assert_eq!(*stacks.active(), labels("introduction"));
    }

    fn labels_from_mask(mask: u8) -> BTreeSet<String> {
        ["caption", "figure", "results"]
            .into_iter()
            .enumerate()
            .filter(|(index, _)| mask & (1 << index) != 0)
            .map(|(_, label)| label.to_owned())
            .collect()
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]

        #[test]
        fn label_stacks_match_a_reference_model(
            operations in proptest::collection::vec((0u8..3, 1usize..7, 0u8..8), 0..48),
        ) {
            let mut actual = LabelStacks::default();
            let mut divs: Vec<BTreeSet<String>> = Vec::new();
            let mut headings: Vec<(usize, BTreeSet<String>)> = Vec::new();

            for (operation, level, mask) in operations {
                let classes = labels_from_mask(mask);
                match operation {
                    0 => {
                        actual.open_div(classes.clone());
                        divs.push(classes);
                    }
                    1 => {
                        prop_assert_eq!(actual.close_div(), divs.pop().is_some());
                    }
                    _ => {
                        actual.push_heading(level, classes.clone());
                        while headings.last().is_some_and(|(existing, _)| *existing >= level) {
                            headings.pop();
                        }
                        headings.push((level, classes));
                    }
                }

                let mut expected: BTreeSet<String> = divs.iter().flatten().cloned().collect();
                if let Some((_, classes)) = headings.iter().rev().find(|(_, set)| !set.is_empty()) {
                    expected.extend(classes.iter().cloned());
                }
                prop_assert_eq!(actual.active(), &expected);
                prop_assert_eq!(actual.open_div_count(), divs.len());
            }
        }
    }
}

#[cfg(test)]
mod reader_tests {
    use super::*;
    use std::io::{BufReader, Cursor, Read};
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn permissive() -> SectionPolicy {
        SectionPolicy::new(None, BTreeSet::new())
    }

    fn read(markdown: &str, policy: SectionPolicy) -> io::Result<Vec<String>> {
        read_markdown_dialect(
            BufReader::new(Cursor::new(markdown.to_owned())),
            MarkdownReaderConfig::new(policy),
            MarkdownDiagnostics::default(),
        )
        .collect()
    }

    fn read_all(markdown: &str, policy: SectionPolicy) -> Vec<String> {
        read(markdown, policy).expect("dialect read")
    }

    fn read_recovering(markdown: &str) -> Vec<String> {
        read_markdown_dialect(
            BufReader::new(Cursor::new(markdown.to_owned())),
            MarkdownReaderConfig::new(permissive()).recover_conversion_artifacts(),
            MarkdownDiagnostics::default(),
        )
        .collect::<io::Result<Vec<_>>>()
        .expect("dialect read")
    }

    #[test]
    fn recovery_normalizes_the_comment_tail_and_not_the_raw_line() {
        // Comment stripping runs first and recovery normalizes what it left.
        // Recovering from the raw line instead would resurrect the stripped
        // comment: the tail is re-derived from bytes that still contain it, and
        // the comment reaches the corpus as prose. Nothing else pins the order
        // of these two — they were written independently and meet only here.
        assert_eq!(
            read_recovering("<!-- FIGURE REMOVED --> <sup>1</sup> 本文です。\n"),
            ["1 本文です。"]
        );
    }

    #[test]
    fn a_comment_tail_that_recovery_empties_is_not_emitted() {
        // `<br>` recovers to a space, so this line has no prose left once the
        // comment is stripped. It must yield nothing rather than a blank line.
        assert_eq!(read_recovering("<!-- c --> <br>\n"), Vec::<String>::new());
    }

    #[test]
    fn frontmatter_never_reaches_the_corpus() {
        let markdown = "---\ntitle: SECRET TITLE\nauthor: SECRET AUTHOR\n---\n\n本文です。\n";
        assert_eq!(read_all(markdown, permissive()), ["本文です。"]);
    }

    #[test]
    fn duplicate_frontmatter_keys_are_rejected() {
        let error = read(
            "---\ntitle: first\ntitle: second\n---\n本文です。\n",
            permissive(),
        )
        .expect_err("duplicate YAML metadata key");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(
            error.to_string().to_lowercase().contains("duplicate"),
            "{error}"
        );
    }

    #[test]
    fn a_body_thematic_break_is_not_frontmatter() {
        let markdown = "本文です。\n\n---\n\n続きです。\n";
        assert_eq!(
            read_all(markdown, permissive()),
            ["本文です。", "続きです。"]
        );
    }

    #[test]
    fn inline_markup_does_not_shatter_a_sentence() {
        let markdown = "これは**強調**された文です。\n次の行は[リンク](http://x)を含む文です。\n";
        assert_eq!(
            read_all(markdown, permissive()),
            ["これは強調された文です。", "次の行はリンクを含む文です。"]
        );
    }

    #[test]
    fn pandoc_structure_never_reaches_the_corpus() {
        let markdown = "::: {.abstract}\n要旨です。\n:::\n\n## 2. 方法 {.methods}\n\n本文です。\n";
        assert_eq!(
            read_all(markdown, permissive()),
            ["要旨です。", "本文です。"]
        );
    }

    #[test]
    fn latex_braces_survive() {
        let markdown = "極限は {T \\to \\infty} です。\n";
        assert_eq!(
            read_all(markdown, permissive()),
            ["極限は {T \\to \\infty} です。"]
        );
    }

    #[test]
    fn comments_are_skipped_including_multiline() {
        let markdown = "前です。\n<!-- TABLE REMOVED START -->\n<!--\nhidden\n-->\n後です。\n";
        assert_eq!(read_all(markdown, permissive()), ["前です。", "後です。"]);
    }

    #[test]
    fn prose_after_a_closing_comment_is_not_dropped() {
        // The line closed its comment and the tail was thrown away with it, so
        // this yielded []. A comment is a prefix, not a licence to drop a line.
        let markdown = "<!-- TABLE REMOVED --> 本文です。\n";
        assert_eq!(read_all(markdown, permissive()), ["本文です。"]);
    }

    #[test]
    fn a_comment_tail_is_classified_not_emitted_blind() {
        // A `:::` fence hiding behind a comment tail is still a fence, and the
        // heading behind one still labels its section. Emitting the tail as
        // prose would put structure in the corpus — a worse failure than the
        // drop being fixed.
        let fence = "<!-- x --> ::: {.abstract}\n要旨です。\n:::\n";
        assert_eq!(read_all(fence, permissive()), ["要旨です。"]);

        let heading = "<!-- x --> ## 2. 方法 {.methods}\n本文です。\n";
        assert_eq!(read_all(heading, permissive()), ["本文です。"]);
    }

    #[test]
    fn raw_html_in_a_comment_tail_is_still_an_error() {
        // The tail goes through the same rejection as any other line: reading a
        // comment tail must not become a hole in the raw-HTML refusal.
        let markdown = "<!-- x --> <div class=\"y\">\n";
        let error = read(markdown, permissive()).expect_err("raw HTML in a comment tail");
        assert!(
            error.to_string().contains("raw HTML"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn a_line_that_closes_and_reopens_a_comment_stays_open() {
        // The reopened comment has no `-->` of its own on this line, so the
        // block must still open; otherwise `hidden` leaks into the corpus.
        let markdown = "前です。\n<!-- a --> <!-- b\nhidden\n-->\n後です。\n";
        assert_eq!(read_all(markdown, permissive()), ["前です。", "後です。"]);
    }

    #[test]
    fn a_comment_ends_at_its_first_close_not_its_last() {
        // Comments do not nest: each ends at its own `-->`. Scanning to the
        // LAST `-->` on the line would treat everything up to it as one comment
        // and silently drop the prose between two of them — the very defect
        // this pair of tests exists to close.
        let between = "<!-- a --> 本文です。 <!-- b -->\n";
        assert_eq!(read_all(between, permissive()), ["本文です。 "]);

        // Chained comments before the prose reduce one at a time.
        let chained = "<!-- a --> <!-- b --> 本文です。\n";
        assert_eq!(read_all(chained, permissive()), ["本文です。"]);
    }

    #[test]
    fn fenced_code_is_skipped() {
        let markdown = "前です。\n\n```rust\nlet hidden = 1;\n```\n\n後です。\n";
        assert_eq!(read_all(markdown, permissive()), ["前です。", "後です。"]);
    }

    #[test]
    fn a_fence_indented_within_the_limit_still_opens_and_closes() {
        let markdown = "前です。\n\n   ```rust\nlet hidden = 1;\n   ```\n\n後です。\n";
        assert_eq!(read_all(markdown, permissive()), ["前です。", "後です。"]);
    }

    #[test]
    fn an_over_indented_fence_still_closes() {
        // The opener was trimmed and the closer was not, so `opens_fence` never
        // saw indentation while `closes_fence` refused anything past 3 spaces:
        // this fence opened and its own matching closer could not close it. The
        // rest of the document was swallowed into the code block and the run
        // ended in a spurious "unclosed code fence".
        let markdown = "前です。\n\n        ```\nlet hidden = 1;\n        ```\n\n後です。\n";
        assert_eq!(read_all(markdown, permissive()), ["前です。", "後です。"]);
    }

    #[test]
    fn a_fence_closes_however_the_two_sides_are_indented() {
        // The mixed case: over-indented opener, flush closer.
        let markdown = "前です。\n\n        ```\nlet hidden = 1;\n```\n\n後です。\n";
        assert_eq!(read_all(markdown, permissive()), ["前です。", "後です。"]);
    }

    #[test]
    fn the_policy_selects_by_active_label() {
        let markdown =
            "## 参考文献 {.references}\n\n書誌です。\n\n## 方法 {.methods}\n\n本文です。\n";
        let policy = SectionPolicy::new(None, SectionPolicy::parse_labels("references"));
        assert_eq!(read_all(markdown, policy), ["本文です。"]);
    }

    #[test]
    fn observed_labels_record_what_the_input_carried() {
        let markdown = "## 方法 {.methods}\n\n本文です。\n\n::: {.caption}\n図です。\n:::\n";
        let config = MarkdownReaderConfig::new(permissive());
        let diagnostics = MarkdownDiagnostics::default();
        read_markdown_dialect(
            BufReader::new(Cursor::new(markdown.to_owned())),
            config,
            diagnostics.clone(),
        )
        .collect::<io::Result<Vec<_>>>()
        .expect("read");

        let seen = diagnostics.observed_labels();
        assert!(seen.contains("methods"));
        assert!(seen.contains("caption"));
    }

    #[test]
    fn raw_html_blocks_are_a_loud_error() {
        let error = read("前です。\n<div>\nhidden\n</div>\n", permissive())
            .expect_err("raw HTML must not be silently skipped");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("div"), "{error}");
        assert!(error.to_string().contains("line 2"), "{error}");
    }

    #[test]
    fn a_doctype_is_a_loud_error_too() {
        let error = read("<!DOCTYPE html>\n", permissive()).expect_err("declarations are raw HTML");
        assert!(error.to_string().contains("DOCTYPE"), "{error}");
    }

    #[test]
    fn a_malformed_div_fence_is_a_loud_error() {
        let error = read("::: nonsense\n本文です。\n", permissive())
            .expect_err("a structural-looking fence must not be swallowed");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("line 1"), "{error}");
    }

    #[test]
    fn an_unmatched_close_is_a_loud_error() {
        let error = read("本文です。\n:::\n", permissive()).expect_err("unmatched div close");
        assert!(error.to_string().contains("line 2"), "{error}");
    }

    #[test]
    fn an_unclosed_block_error_names_the_line_that_opened_it() {
        // "at end of input" names the fault but not its site. In a truncated
        // 500-line paper that is a hunt rather than a fix. The line that opened
        // the block is the site; the line the input ran out on is not.
        let error =
            read("本文です。\n\n```rust\ncode\n", permissive()).expect_err("unclosed fence");
        let message = error.to_string();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(message.contains("fence"), "{message}");
        assert!(message.contains("line 3"), "{message}");
    }

    #[test]
    fn an_unclosed_frontmatter_error_names_the_line_that_opened_it() {
        // The likeliest real case: a truncated download. Frontmatter can only
        // open on line 1, so reporting the *last* line read would say "line 3".
        let error =
            read("---\ntitle: X\n本文です。\n", permissive()).expect_err("unclosed frontmatter");
        let message = error.to_string();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(message.contains("frontmatter"), "{message}");
        assert!(message.contains("line 1"), "{message}");
        assert!(!message.contains("line 3"), "{message}");
    }

    #[test]
    fn an_unclosed_comment_error_names_the_line_that_opened_it() {
        let error = read("前です。\n<!--\nhidden\n", permissive()).expect_err("unclosed comment");
        let message = error.to_string();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(message.contains("comment"), "{message}");
        assert!(message.contains("line 2"), "{message}");
        assert!(!message.contains("line 3"), "{message}");
    }

    #[test]
    fn an_unclosed_div_error_names_the_line_that_opened_it() {
        let error = read("::: {.caption}\n図です。\n", permissive()).expect_err("unclosed div");
        let message = error.to_string();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(message.contains("unclosed"), "{message}");
        assert!(message.contains("line 1"), "{message}");
    }

    #[test]
    fn an_unclosed_div_error_names_the_outermost_open_div() {
        // Two are open, so the choice is observable. The outermost is named:
        // with the count, it bounds the suspect region to [line 3, EOF], which
        // contains every one of the open openers. The innermost bounds nothing
        // — the others opened before it, at unnamed lines.
        let markdown = "本文です。\n\n::: {.outer}\n::: {.inner}\n図です。\n";
        let error = read(markdown, permissive()).expect_err("two unclosed divs");
        let message = error.to_string();
        assert!(message.contains("2 still open"), "{message}");
        assert!(message.contains("line 3"), "{message}");
        assert!(!message.contains("line 4"), "{message}");
    }

    #[test]
    fn closing_an_inner_div_still_reports_the_outer_opening_line() {
        // The tracked opening lines must pop with the stack and from the same
        // end: closing `.inner` must retire line 2, not line 1.
        let markdown = "::: {.outer}\n::: {.inner}\n図です。\n:::\n";
        let error = read(markdown, permissive()).expect_err("outer div never closed");
        let message = error.to_string();
        assert!(message.contains("1 still open"), "{message}");
        assert!(message.contains("line 1"), "{message}");
        assert!(!message.contains("line 2"), "{message}");
    }

    #[test]
    fn an_open_block_is_reported_ahead_of_a_div_it_hides() {
        // Precedence, restated with line numbers. The `::: {.inner}` on line 4
        // is inside the fence and was never counted, so the div depth is a
        // symptom; the fence is the cause. The named line is the fence's (3),
        // not the genuinely-open `.caption` div's (1).
        let markdown = "::: {.caption}\n\n```rust\n::: {.inner}\ncode\n";
        let error = read(markdown, permissive()).expect_err("unclosed fence");
        let message = error.to_string();
        assert!(message.contains("code fence"), "{message}");
        assert!(message.contains("line 3"), "{message}");
        assert!(!message.contains("line 1"), "{message}");
    }

    #[test]
    fn a_well_formed_document_still_ends_cleanly() {
        // Guard against over-triggering: normal input must not error.
        let markdown = "---\ntitle: X\n---\n\n本文です。\n\n::: {.caption}\n図です。\n:::\n";
        assert_eq!(read_all(markdown, permissive()), ["本文です。", "図です。"]);
    }

    #[test]
    fn empty_input_is_not_an_error() {
        assert_eq!(read_all("", permissive()), Vec::<String>::new());
    }

    #[test]
    fn the_error_is_emitted_once_not_forever() {
        let mut source = read_markdown_dialect(
            BufReader::new(Cursor::new("---\ntitle: X\n".to_owned())),
            MarkdownReaderConfig::new(permissive()),
            MarkdownDiagnostics::default(),
        );
        assert!(source.next().expect("first").is_err());
        assert!(source.next().is_none(), "the error must not repeat forever");
    }

    #[test]
    fn a_self_label_does_not_propagate_to_the_section_below_it() {
        // {.title} sits on 133 headings and zero divs. It describes the heading's
        // own text, which is never emitted — so it must label nothing beneath it.
        // Propagating it discarded 1,127 lines across 77 of 100 papers, abstracts
        // included, whenever `title` was excluded.
        let markdown = "# 論文題目 {.title}\n\n::: {.abstract}\n要旨です。\n:::\n";
        let policy = SectionPolicy::new(None, SectionPolicy::parse_labels("title"));
        assert_eq!(read_all(markdown, policy), ["要旨です。"]);
    }

    #[test]
    fn a_section_label_still_propagates() {
        let markdown = "## 2. 方法 {.methods}\n\n本文です。\n";
        let policy = SectionPolicy::new(None, SectionPolicy::parse_labels("methods"));
        assert_eq!(read_all(markdown, policy), Vec::<String>::new());
    }

    #[test]
    fn a_self_label_is_still_observed_for_diagnostics() {
        let markdown = "# 論文題目 {.title}\n\n本文です。\n";
        let config = MarkdownReaderConfig::new(permissive());
        let diagnostics = MarkdownDiagnostics::default();
        read_markdown_dialect(
            BufReader::new(Cursor::new(markdown.to_owned())),
            config,
            diagnostics.clone(),
        )
        .collect::<io::Result<Vec<_>>>()
        .expect("read");

        assert!(diagnostics.observed_labels().contains("title"));
    }

    #[test]
    fn the_self_label_set_is_caller_declared_not_hardcoded() {
        // The vocabulary is open: a corpus may name its self-labels anything.
        let markdown = "# 見出し {.banner}\n\n本文です。\n";
        let config = MarkdownReaderConfig::with_self_labels(
            SectionPolicy::new(None, SectionPolicy::parse_labels("banner")),
            SectionPolicy::parse_labels("banner"),
        );
        let text = read_markdown_dialect(
            BufReader::new(Cursor::new(markdown.to_owned())),
            config,
            MarkdownDiagnostics::default(),
        )
        .collect::<io::Result<Vec<_>>>()
        .expect("read");

        assert_eq!(text, ["本文です。"]);
    }

    #[test]
    fn a_div_class_is_never_treated_as_a_self_label() {
        // Self-labels describe headings. A div bounds real content, so the same
        // name on a div must still exclude that content.
        let markdown = "::: {.title}\n題目です。\n:::\n\n本文です。\n";
        let policy = SectionPolicy::new(None, SectionPolicy::parse_labels("title"));
        assert_eq!(read_all(markdown, policy), ["本文です。"]);
    }

    #[test]
    fn a_heading_carrying_only_self_labels_leaves_the_section_unlabeled() {
        let markdown = "# 論文題目 {.title}\n\n本文です。\n";
        let policy =
            SectionPolicy::new(Some(SectionPolicy::parse_labels("title")), BTreeSet::new());
        // `title` never labels an emitted line, so an include list naming only
        // `title` admits nothing.
        assert_eq!(read_all(markdown, policy), Vec::<String>::new());
    }

    #[test]
    fn declaring_no_self_labels_lets_every_heading_class_introduce_a_section() {
        // The caller's declaration replaces the shipped default rather than
        // accreting onto it: `DEFAULT_SELF_LABELS` is an opinion, not a floor.
        // Pairs with `a_heading_carrying_only_self_labels_leaves_the_section_
        // unlabeled` — same input and include list, opposite outcome — so the
        // self-label set is isolated as the only cause.
        let markdown = "# 論文題目 {.title}\n\n本文です。\n";
        let config = MarkdownReaderConfig::with_self_labels(
            SectionPolicy::new(Some(SectionPolicy::parse_labels("title")), BTreeSet::new()),
            BTreeSet::new(),
        );
        let text = read_markdown_dialect(
            BufReader::new(Cursor::new(markdown.to_owned())),
            config,
            MarkdownDiagnostics::default(),
        )
        .collect::<io::Result<Vec<_>>>()
        .expect("read");

        assert_eq!(text, ["本文です。"]);
    }

    #[test]
    fn a_heading_mixing_a_self_label_and_a_section_label_keeps_the_section_label() {
        // The filter removes only the declared self-labels. `{.title .methods}`
        // does not occur in the reference corpus — title/subtitle are always solo
        // there — so nothing else pins this. It is the property that makes
        // filtering safe, and an unpinned safety property is an assumption.
        let markdown = "# 見出し {.title .methods}\n\n本文です。\n";
        let policy = SectionPolicy::new(
            Some(SectionPolicy::parse_labels("methods")),
            BTreeSet::new(),
        );
        assert_eq!(read_all(markdown, policy), ["本文です。"]);
    }

    #[test]
    fn reading_is_bounded_by_the_current_line() {
        struct CountingReader {
            inner: Cursor<Vec<u8>>,
            consumed: Arc<AtomicUsize>,
        }
        impl Read for CountingReader {
            fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
                let mut one = [0];
                let read = self.inner.read(&mut one)?;
                if read != 0 {
                    buffer[0] = one[0];
                    self.consumed.fetch_add(read, Ordering::SeqCst);
                }
                Ok(read)
            }
        }

        let markdown = format!("最初の文です。\n{}", "後の文です。\n".repeat(1_000));
        let consumed = Arc::new(AtomicUsize::new(0));
        let reader = CountingReader {
            inner: Cursor::new(markdown.clone().into_bytes()),
            consumed: Arc::clone(&consumed),
        };
        let mut source = read_markdown_dialect(
            BufReader::new(reader),
            MarkdownReaderConfig::new(permissive()),
            MarkdownDiagnostics::default(),
        );

        assert_eq!(
            source.next().transpose().unwrap().as_deref(),
            Some("最初の文です。")
        );
        assert!(
            consumed.load(Ordering::SeqCst) < markdown.len(),
            "the dialect reader must not consume the corpus to yield its first sentence"
        );
    }
}
