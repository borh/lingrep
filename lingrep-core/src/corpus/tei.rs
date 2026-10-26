use super::structured::{CorpusEvent, Metadata, metadata_from_json};
use crate::analysis::{AnnotatedText, TextAnnotation};
use crate::token_match::{AttributeKey, AttributeKind, SchemaAttribute, TokenSchema};
use compact_str::CompactString;
use roxmltree::{Document, Node};
use std::collections::{BTreeMap, HashMap};
use std::io::{self, Read};
use std::ops::Range;

const NS: &str = "http://www.tei-c.org/ns/1.0";
const XML_NS: &str = "http://www.w3.org/XML/1998/namespace";
pub const MAX_WORK_BYTES: usize = 64 * 1024 * 1024;
const MAX_NODES: u32 = 1_000_000;
const MAX_DEPTH: usize = 128;
const MAX_ANNOTATION_BYTES: usize = 64 * 1024 * 1024;
pub const ANNOTATION_KEYS: [&str; 4] = ["tei.element", "tei.rend", "tei.ruby", "tei.source"];

pub fn annotated_schema(
    base: &TokenSchema,
) -> Result<TokenSchema, crate::token_match::TokenSchemaError> {
    TokenSchema::new(base.attributes().iter().cloned().chain(
        ANNOTATION_KEYS.into_iter().map(|key| {
            SchemaAttribute::new(key, AttributeKind::StringSet).expect("constant TEI key")
        }),
    ))
}

pub fn annotated_descriptor(
    base: &crate::analysis::AnalyzerDescriptor,
    schema: &TokenSchema,
) -> crate::analysis::AnalyzerDescriptor {
    let mut projection = blake3::Hasher::new();
    projection.update(b"lingrep.tei-source-annotation-projection/v1");
    projection.update(base.projection_policy_digest());
    crate::analysis::AnalyzerDescriptor::new(
        base.family(),
        base.profile(),
        base.segmentation(),
        base.normalization(),
        base.language_policy(),
        base.boundary_policy(),
        *base.implementation_digest(),
        *base.resource_manifest_digest(),
        *projection.finalize().as_bytes(),
        *base.alignment_policy_digest(),
        *base.component_closure_digest(),
        *base.output_config_digest(),
        schema.digest(),
    )
    .expect("the base analyzer descriptor and TEI schema are valid")
}

pub struct TeiReading {
    text: String,
    annotations: Vec<TextAnnotation>,
    active: BTreeMap<usize, TextAnnotation>,
    next_annotation: usize,
    position: usize,
    max_record_bytes: usize,
    pub(crate) metadata: Metadata,
    pub(crate) work_identifier: Option<String>,
    pub(crate) soranoha_profile: bool,
}

impl TeiReading {
    /// Reads the principal TEI body; annotation ranges address the projected reading, never XML bytes.
    pub fn parse(xml: &str, max_record_bytes: usize) -> io::Result<Self> {
        if xml.len() > MAX_WORK_BYTES {
            return Err(invalid("TEI work exceeds 64 MiB"));
        }
        let doc = Document::parse_with_options(
            xml,
            roxmltree::ParsingOptions {
                nodes_limit: MAX_NODES,
                ..Default::default()
            },
        )
        .map_err(|e| invalid(format!("malformed TEI: {e}")))?;
        let root = doc.root_element();
        if tag(root) != Some("TEI") {
            return Err(invalid("expected a TEI P5 root in the TEI namespace"));
        }
        let texts = root
            .children()
            .filter(|n| tag(*n) == Some("text"))
            .collect::<Vec<_>>();
        if texts.len() != 1 {
            return Err(invalid("expected exactly one TEI text"));
        }
        let bodies = texts[0]
            .children()
            .filter(|n| tag(*n) == Some("body"))
            .collect::<Vec<_>>();
        if bodies.len() != 1 {
            return Err(invalid(
                "expected one principal body; grouped/composite TEI texts require an explicit reading policy",
            ));
        }
        let vocabulary = root.attribute(("https://w3id.org/soranoha/ns/tei", "vocab-version"));
        if vocabulary.is_some_and(|v| !matches!(v, "1" | "2" | "3")) {
            return Err(invalid("unsupported Soranoha TEI vocabulary version"));
        }
        let soranoha_profile = vocabulary.is_some();
        let mut builder = ReadingBuilder {
            block_starts: !soranoha_profile,
            ..Default::default()
        };
        builder.walk(bodies[0], 0)?;
        if builder.last_block_end {
            builder.text.pop();
        }
        for annotation in &mut builder.annotations {
            annotation.range.end = annotation.range.end.min(builder.text.len());
        }
        builder.annotations.retain(|a| a.range.start < a.range.end);
        let mut metadata = serde_json::Map::new();
        let header = root.children().find(|n| tag(*n) == Some("teiHeader"));
        let mut work_identifier = None;
        if let Some(header) = header {
            for (key, element, attribute) in [
                ("tei.languages", "language", "ident"),
                ("tei.licences", "licence", "target"),
            ] {
                let values = header
                    .descendants()
                    .filter(|n| tag(*n) == Some(element))
                    .filter_map(|n| n.attribute(attribute))
                    .map(str::to_owned)
                    .collect::<Vec<_>>();
                if !values.is_empty() {
                    metadata.insert(key.into(), serde_json::json!(values));
                }
            }
            for (key, element) in [("title", "title"), ("author", "author")] {
                if let Some(node) = header.descendants().find(|n| tag(*n) == Some(element)) {
                    let value = node
                        .descendants()
                        .filter(Node::is_text)
                        .filter_map(|n| n.text())
                        .collect::<String>();
                    if !value.is_empty() {
                        metadata.insert(key.into(), serde_json::json!(value));
                    }
                }
            }
            work_identifier = header
                .descendants()
                .find(|n| {
                    tag(*n) == Some("idno")
                        && n.attribute("type") == Some("soranoha-work-identifier")
                })
                .and_then(|n| n.text())
                .map(str::to_owned);
        }
        let mut annotations = builder.annotations;
        annotations.sort_by_key(|a| (a.range.start, a.range.end));
        Ok(Self {
            text: builder.text,
            annotations,
            active: BTreeMap::new(),
            next_annotation: 0,
            position: 0,
            max_record_bytes,
            metadata: metadata_from_json(metadata.into(), "TEI header")?,
            work_identifier,
            soranoha_profile,
        })
    }
}

impl Iterator for TeiReading {
    type Item = io::Result<AnnotatedText>;
    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if self.position >= self.text.len() {
                return None;
            }
            let start = self.position;
            let mut end = self.text.len();
            let Some(remaining) = self.text.get(start..) else {
                self.position = self.text.len();
                return Some(Err(invalid("invalid TEI sentence start")));
            };
            let mut chars = remaining.char_indices().peekable();
            while let Some((offset, c)) = chars.next() {
                if c == '\n' {
                    end = start + offset;
                    self.position = end + 1;
                    break;
                }
                if crate::utils::is_sentence_delimiter(c) {
                    end = start + offset + c.len_utf8();
                    if let Some(&(offset, quote)) = chars.peek()
                        && crate::utils::is_sentence_closing_quotation(quote)
                    {
                        end = start + offset + quote.len_utf8();
                    }
                    self.position = end;
                    break;
                }
            }
            if end == self.text.len() {
                self.position = end;
            }
            let Some(text) = self.text.get(start..end) else {
                self.position = self.text.len();
                return Some(Err(invalid("invalid TEI sentence range")));
            };
            if text.trim().is_empty() {
                continue;
            }
            self.active.retain(|_, a| a.range.end > start);
            while let Some(annotation) = self.annotations.get(self.next_annotation) {
                if annotation.range.start >= end {
                    break;
                }
                if annotation.range.end > start {
                    self.active.insert(self.next_annotation, annotation.clone());
                }
                self.next_annotation += 1;
            }
            let annotations = self
                .active
                .values()
                .map(|a| TextAnnotation {
                    range: a.range.start.max(start) - start..a.range.end.min(end) - start,
                    attribute: a.attribute.clone(),
                    values: a.values.clone(),
                })
                .collect();
            let result=AnnotatedText::new(text.to_owned(),annotations)
                .map_err(|e|invalid(e.to_string())).and_then(|value| {
                    if value.resident_bytes()>self.max_record_bytes {Err(invalid(format!("TEI sentence text and annotations exceed {}-byte input-record memory limit",self.max_record_bytes)))}
                    else {Ok(value)}
                });
            if result.is_err() {
                self.position = self.text.len();
            }
            return Some(result);
        }
    }
}

#[derive(Default)]
struct ReadingBuilder {
    text: String,
    annotations: Vec<TextAnnotation>,
    identifiers: HashMap<String, Range<usize>>,
    annotation_bytes: usize,
    last_block_end: bool,
    block_starts: bool,
}
impl ReadingBuilder {
    fn append(&mut self, text: &str) -> io::Result<()> {
        let remove = usize::from(self.last_block_end && text.starts_with('\n'));
        if self
            .text
            .len()
            .saturating_sub(remove)
            .saturating_add(text.len())
            > MAX_WORK_BYTES
        {
            return Err(invalid("projected TEI reading exceeds 64 MiB"));
        }
        if remove != 0 {
            self.text.pop();
        }
        self.text.push_str(text);
        self.last_block_end = false;
        Ok(())
    }
    fn annotation(
        &mut self,
        range: Range<usize>,
        key: &str,
        values: Vec<CompactString>,
    ) -> io::Result<()> {
        if range.is_empty() || values.is_empty() {
            return Ok(());
        }
        if values.iter().any(|s| s.len() > 64 * 1024) {
            return Err(invalid("TEI annotation scalar exceeds 64 KiB"));
        }
        self.annotation_bytes += std::mem::size_of::<TextAnnotation>()
            + key.len()
            + values
                .iter()
                .map(|s| s.len() + std::mem::size_of::<CompactString>())
                .sum::<usize>();
        if self.annotation_bytes > MAX_ANNOTATION_BYTES {
            return Err(invalid("TEI annotations exceed 64 MiB"));
        }
        self.annotations.push(TextAnnotation {
            range,
            attribute: AttributeKey::new(key).expect("constant TEI key"),
            values: values.into_boxed_slice(),
        });
        Ok(())
    }
    fn walk(&mut self, node: Node<'_, '_>, depth: usize) -> io::Result<()> {
        if depth > MAX_DEPTH {
            return Err(invalid("TEI reading exceeds 128 nested elements"));
        }
        if node.is_text() {
            let text = node.text().unwrap_or_default();
            let structural = node.parent().and_then(tag).is_some_and(|t| {
                matches!(
                    t,
                    "body" | "div" | "lg" | "list" | "text" | "floatingText" | "figure"
                )
            });
            if !(structural
                && text.chars().all(|c| {
                    c.is_whitespace() && !matches!(c, '\u{85}' | '\u{a0}' | '\u{2007}' | '\u{202f}')
                }))
            {
                self.append(text)?;
            }
            return Ok(());
        }
        if !node.is_element() {
            return Ok(());
        }
        let name = tag(node).ok_or_else(|| invalid("foreign element in selected TEI body"))?;
        if matches!(name, "note" | "fw" | "figDesc" | "rt") {
            return Ok(());
        }
        if self.block_starts
            && matches!(name, "p" | "head" | "l" | "ab" | "item")
            && !self.text.is_empty()
            && !self.text.ends_with('\n')
        {
            self.append("\n")?;
        }
        let start = self.text.len();
        if name == "p" {
            let width = indent(node)?;
            if width > MAX_WORK_BYTES.saturating_sub(self.text.len()) / 3 {
                return Err(invalid("TEI indentation exceeds remaining work budget"));
            }
            self.append(&"　".repeat(width))?;
        }
        match name {
            "lb" | "pb" | "cb" => self.append("\n")?,
            "g" if !node
                .descendants()
                .any(|n| n.is_text() && !n.text().unwrap_or_default().is_empty()) =>
            {
                self.append("\u{fffc}")?
            }
            "ruby" => {
                let bases = node
                    .children()
                    .filter(|n| tag(*n) == Some("rb"))
                    .collect::<Vec<_>>();
                if bases.is_empty() {
                    return Err(invalid("TEI ruby has no base"));
                }
                for base in &bases {
                    self.walk(*base, depth + 1)?;
                }
                let base = start..self.text.len();
                for reading in node.children().filter(|n| tag(*n) == Some("rt")) {
                    let mut read_builder = Self::default();
                    for child in reading.children() {
                        read_builder.walk(child, depth + 1)?;
                    }
                    let range = if let Some(target) = reading.attribute("target") {
                        self.reference(target)?.clone()
                    } else if let (Some(from), Some(to)) =
                        (reading.attribute("from"), reading.attribute("to"))
                    {
                        self.reference(from)?.start..self.reference(to)?.end
                    } else {
                        if reading.has_attribute("from")
                            || reading.has_attribute("to")
                            || bases.len() != 1
                        {
                            return Err(invalid("ambiguous TEI ruby target"));
                        }
                        base.clone()
                    };
                    if range.is_empty() || range.start < base.start || range.end > base.end {
                        return Err(invalid("TEI ruby target is outside its base"));
                    }
                    self.annotation(range, "tei.ruby", vec![read_builder.text.into()])?;
                }
            }
            "choice" => {
                let children = ["corr", "reg", "expan", "sic", "orig", "abbr"]
                    .into_iter()
                    .find_map(|preferred| {
                        let nodes = node
                            .children()
                            .filter(|n| tag(*n) == Some(preferred))
                            .collect::<Vec<_>>();
                        (!nodes.is_empty()).then_some(nodes)
                    })
                    .ok_or_else(|| invalid("no supported reading in TEI choice"))?;
                for child in children {
                    self.walk(child, depth + 1)?;
                }
            }
            "app" => {
                let lemmas = node
                    .children()
                    .filter(|n| tag(*n) == Some("lem"))
                    .collect::<Vec<_>>();
                if lemmas.len() != 1 {
                    return Err(invalid("TEI apparatus requires exactly one lemma"));
                }
                self.walk(lemmas[0], depth + 1)?;
            }
            _ => {
                for child in node.children() {
                    self.walk(child, depth + 1)?;
                }
            }
        }
        let end = self.text.len();
        if let Some(id) = node.attribute((XML_NS, "id"))
            && self.identifiers.insert(id.into(), start..end).is_some()
        {
            return Err(invalid("duplicate XML identifier in TEI reading"));
        }
        self.annotation(start..end, "tei.element", vec![name.into()])?;
        for (attr, key) in [("rend", "tei.rend"), ("source", "tei.source")] {
            if let Some(value) = node.attribute(attr) {
                let mut values = Vec::new();
                let mut bytes =
                    self.annotation_bytes + std::mem::size_of::<TextAnnotation>() + key.len();
                for part in value.split_whitespace() {
                    if part.len() > 64 * 1024 {
                        return Err(invalid("TEI annotation scalar exceeds 64 KiB"));
                    }
                    bytes = bytes.saturating_add(part.len() + std::mem::size_of::<CompactString>());
                    if bytes > MAX_ANNOTATION_BYTES {
                        return Err(invalid("TEI annotations exceed 64 MiB"));
                    }
                    values.push(part.into());
                }
                self.annotation(start..end, key, values)?;
            }
        }
        if matches!(name, "p" | "head" | "l" | "ab" | "item")
            && end > start
            && !self.text.ends_with('\n')
        {
            self.append("\n")?;
            self.last_block_end = true;
        }
        Ok(())
    }
    fn reference(&self, value: &str) -> io::Result<&Range<usize>> {
        value
            .strip_prefix('#')
            .and_then(|id| self.identifiers.get(id))
            .ok_or_else(|| {
                invalid(format!(
                    "TEI ruby target {value:?} does not identify selected base text"
                ))
            })
    }
}
fn tag<'a>(node: Node<'a, '_>) -> Option<&'a str> {
    (node.is_element() && node.tag_name().namespace() == Some(NS)).then(|| node.tag_name().name())
}
fn indent(node: Node<'_, '_>) -> io::Result<usize> {
    let rend = node.attribute("rend").unwrap_or_default();
    let width = rend.split_whitespace().find_map(|s| {
        s.strip_prefix("first-line-indent(")
            .and_then(|s| s.strip_suffix(')'))
    });
    if let Some(width) = width {
        return width
            .parse()
            .map_err(|_| invalid("invalid TEI first-line indentation"));
    }
    let style = node.attribute("style").unwrap_or_default();
    let width = style.split(';').find_map(|s| {
        s.trim()
            .strip_prefix("text-indent:")
            .and_then(|s| s.trim().strip_suffix("em"))
    });
    width.map_or(Ok(0), |s| {
        s.trim()
            .parse()
            .map_err(|_| invalid("invalid TEI text-indent"))
    })
}
fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

pub struct TeiEvents {
    pending: std::collections::VecDeque<CorpusEvent>,
    reading: TeiReading,
    ended: bool,
}
impl TeiEvents {
    pub fn new(
        mut reader: impl io::Read,
        name: String,
        max_record_bytes: usize,
    ) -> io::Result<Self> {
        let mut xml = String::new();
        reader
            .by_ref()
            .take(MAX_WORK_BYTES as u64 + 1)
            .read_to_string(&mut xml)?;
        let mut reading = TeiReading::parse(&xml, max_record_bytes)?;
        let metadata = std::mem::take(&mut reading.metadata);
        Ok(Self {
            pending: [
                CorpusEvent::DocumentStart {
                    source_name: name,
                    metadata,
                },
                CorpusEvent::DivisionObservation {
                    kind: "paragraph".into(),
                    observation: super::DivisionObservation::Unavailable {
                        reason: "TEI reading policy does not assert authored paragraph boundaries"
                            .into(),
                    },
                },
            ]
            .into(),
            reading,
            ended: false,
        })
    }
}
impl Iterator for TeiEvents {
    type Item = io::Result<CorpusEvent>;
    fn next(&mut self) -> Option<Self::Item> {
        if let Some(event) = self.pending.pop_front() {
            return Some(Ok(event));
        }
        if self.ended {
            return None;
        }
        match self.reading.next() {
            Some(Ok(text)) => Some(Ok(CorpusEvent::AnnotatedSentence {
                text,
                active_labels: Box::new([]),
                metadata: Box::new([]),
            })),
            Some(Err(error)) => {
                self.ended = true;
                Some(Err(error))
            }
            None => {
                self.ended = true;
                Some(Ok(CorpusEvent::DocumentEnd))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn document(body: &str) -> String {
        format!("<TEI xmlns='{NS}'><text><body>{body}</body></text></TEI>")
    }
    fn sentences(body: &str) -> Vec<AnnotatedText> {
        TeiReading::parse(&document(body), 1024 * 1024)
            .unwrap()
            .collect::<io::Result<Vec<_>>>()
            .unwrap()
    }
    #[test]
    fn principal_reading_excludes_alternatives_and_preserves_ruby_base_annotations() {
        let values = sentences(
            "<p rend='first-line-indent(1)'><ruby><rb>山</rb><rt>やま</rt></ruby><hi rend='bouten right'>猫</hi><choice><orig>旧</orig><reg>新</reg><corr>正</corr></choice><app><rdg>異</rdg><lem>本</lem></app><note>注</note><fw>柱</fw><figure><figDesc>画像</figDesc></figure><g/>。</p><head>題</head><p>次<lb/>行。</p>",
        );
        assert_eq!(
            values.iter().map(AnnotatedText::text).collect::<Vec<_>>(),
            vec!["　山猫正本\u{fffc}。", "題", "次", "行。"]
        );
        let ruby = values[0]
            .annotations()
            .iter()
            .find(|a| a.attribute.as_str() == "tei.ruby")
            .unwrap();
        assert_eq!(values[0].text().get(ruby.range.clone()).unwrap(), "山");
        assert_eq!(&*ruby.values, &[CompactString::new("やま")]);
        assert!(
            values[0]
                .annotations()
                .iter()
                .any(|a| a.attribute.as_str() == "tei.rend"
                    && a.values.iter().any(|s| s == "bouten"))
        );
        assert!(
            values[0]
                .annotations()
                .iter()
                .any(|a| a.attribute.as_str() == "tei.element"
                    && a.values.iter().any(|s| s == "body"))
        );
    }
    #[test]
    fn ranged_ruby_uses_selected_base_anchor_and_element_extents() {
        let values = sentences(
            "<p><ruby><rb><anchor xml:id='a'/>入学<anchor xml:id='b'/>試験</rb><rt from='#a' to='#b'>にゅうがく</rt></ruby>。</p><p><ruby><rb><c xml:id='first'>大</c><c xml:id='last'>学</c></rb><rt from='#first' to='#last'>だいがく</rt></ruby></p>",
        );
        let ranges = values
            .iter()
            .flat_map(|t| {
                t.annotations()
                    .iter()
                    .filter(|a| a.attribute.as_str() == "tei.ruby")
                    .map(|a| t.text().get(a.range.clone()).unwrap().to_owned())
            })
            .collect::<Vec<_>>();
        assert_eq!(ranges, vec!["入学", "大学"]);
    }
    #[test]
    fn malformed_or_ambiguous_tei_never_produces_a_reading() {
        for xml in [
            "<TEI><text><body/></text></TEI>".to_owned(),
            document("<choice><foo>x</foo></choice>"),
            document("<app><rdg>x</rdg></app>"),
            document("<ruby><rb>猫</rb><rt from='#missing' to='#missing'>ねこ</rt></ruby>"),
            document("<p><ruby><rt>ねこ</rt></ruby></p>"),
            document("<p>猫</body>"),
            document("<app><lem>a</lem><lem>b</lem></app>"),
            document("<ruby><rb xml:id='a'>a</rb><rb xml:id='b'>b</rb><rt>x</rt></ruby>"),
            document(
                "<ruby><rb><c xml:id='a'>a</c><c xml:id='b'>b</c></rb><rt from='#b' to='#a'>x</rt></ruby>",
            ),
            document(&format!("{}猫{}", "<hi>".repeat(129), "</hi>".repeat(129))),
            format!("<TEI xmlns='{NS}'><text><group><text><body/></text></group></text></TEI>"),
            format!(
                "<!DOCTYPE TEI [<!ENTITY external SYSTEM 'file:///etc/passwd'>]>{}",
                document("<p>&external;</p>")
            ),
        ] {
            assert!(TeiReading::parse(&xml, 1024).is_err(), "{xml}");
        }
    }
    #[test]
    fn generated_indentation_cannot_expand_past_the_work_budget() {
        let body = "<p rend='first-line-indent(22369621)'>x</p>".repeat(2);
        assert!(TeiReading::parse(&document(&body), 1024 * 1024).is_err());
    }
    #[test]
    fn sentence_memory_accounting_includes_annotation_storage() {
        let mut reading =
            TeiReading::parse(&document("<p><hi rend='bouten'>猫</hi></p>"), 3).unwrap();
        assert!(
            reading
                .next()
                .unwrap()
                .unwrap_err()
                .to_string()
                .contains("input-record memory")
        );
        assert!(reading.next().is_none());
    }
    #[hegel::test(test_cases = 200, derandomize = true)]
    fn namespace_prefixes_preserve_complete_reading_annotations(tc: hegel::TestCase) {
        use hegel::generators as g;
        let raw = tc.draw(
            g::vecs(g::sampled_from(vec![
                '猫', '山', '字', 'é', '\u{301}', '&', '<', '>', ' ',
            ]))
            .min_size(1)
            .max_size(24),
        );
        let text = raw.iter().collect::<String>();
        let escaped = text
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;");
        let body = format!("<p><ruby><rb>{escaped}</rb><rt>よみ</rt></ruby>。</p>");
        let plain = document(&body);
        let prefixed = format!(
            "<t:TEI xmlns:t='{NS}'><t:text><t:body><t:p><t:ruby><t:rb>{escaped}</t:rb><t:rt>よみ</t:rt></t:ruby>。</t:p></t:body></t:text></t:TEI>"
        );
        let a = TeiReading::parse(&plain, 1024 * 1024)
            .unwrap()
            .collect::<io::Result<Vec<_>>>()
            .unwrap();
        let b = TeiReading::parse(&prefixed, 1024 * 1024)
            .unwrap()
            .collect::<io::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(a, b);
        assert_eq!(
            a.iter().map(AnnotatedText::text).collect::<String>(),
            format!("{text}。")
        );
    }
    #[test]
    fn generic_blocks_separate_text_while_soranoha_preserves_its_projection() {
        for block in ["p", "head", "l", "ab", "item"] {
            let body = format!("<ab>前<{block}>後</{block}></ab>");
            assert_eq!(
                sentences(&body)
                    .iter()
                    .map(AnnotatedText::text)
                    .collect::<Vec<_>>(),
                vec!["前", "後"]
            );
            let source = document(&body).replacen(
                "<TEI ",
                "<TEI xmlns:snh='https://w3id.org/soranoha/ns/tei' snh:vocab-version='3' ",
                1,
            );
            let reading = TeiReading::parse(&source, 1024 * 1024)
                .unwrap()
                .collect::<io::Result<Vec<_>>>()
                .unwrap();
            assert_eq!(
                reading.iter().map(AnnotatedText::text).collect::<Vec<_>>(),
                vec!["前後"]
            );
        }
    }

    #[test]
    fn editorial_choice_precedence_is_independent_of_xml_child_order() {
        for children in [
            "<orig>旧</orig><reg>新</reg>",
            "<reg>新</reg><orig>旧</orig>",
            "<reg>新</reg><corr>新</corr><orig>旧</orig>",
            "<orig>旧</orig><corr>新</corr><reg>別</reg>",
        ] {
            assert_eq!(
                sentences(&format!("<p><choice>{children}</choice>。</p>"))
                    .iter()
                    .map(AnnotatedText::text)
                    .collect::<String>(),
                "新。"
            );
        }
    }
}
