//! C-XML entry parsing: the zipper-free port of `corpus-utils.bccwj/walk-and-emit`.
//!
//! [`parse_entry`] consumes one entry's bytes and returns a [`BccwjDocument`]
//! value — paragraphs with their tag sets and sentence texts. It is pure and
//! total over well-formed entries: a `&[u8]` in, a value out, no I/O.
//!
//! The reference walks a fast-zip tree; the walk is a pre-order DFS, which a
//! single-pass `quick-xml` streaming reader provides directly. The one
//! deliberate divergence (spec §3.6): the reference's zipper `:same`
//! transition displaces a leaf's tag where the parent belongs, and the
//! displacement persists for the rest of the document (verified live on
//! `LBtn_00026`: every paragraph carries `:info` and never `:article` after
//! the leading `<info/>`). This reader records the *true* Start/End
//! ancestry instead; the differential run (spec §5.6) accounts for the gap.
//!
//! The other deliberate deviation (spec §3.4): the `OY` rule is *activated*.
//! The reference's `(case corpus "OY" ...)` never matches because the driver
//! passes the Japanese `corpusName` (`"Yahoo!ブログ"`), not the two-letter
//! code, so its `br` exception is dead code. Here `Subcorpus::OY` excludes
//! `br` from the paragraph-level set.

use std::collections::BTreeSet;
use std::io;

use quick_xml::Reader;
use quick_xml::events::Event;

use super::metadata::Subcorpus;

/// The reference's paragraph-level tags (`walk-and-emit`'s
/// `paragraph-level-tags`), with the `OY` exception applied by
/// [`is_paragraph_level`].
const PARAGRAPH_TAGS: &[&str] = &[
    "article",
    "blockEnd",
    "cluster",
    "titleBlock",
    "title",
    "orphanedTitle",
    "list",
    "paragraph",
    "verse",
    "br",
    "speech",
    "speaker",
    "caption",
    "citation",
    "quotation",
    "OCAnswer",
    "OCQuestion",
];

/// The reference's rename map applied at push time: both title spellings are
/// recorded as `title`.
fn rename(tag: &str) -> &str {
    match tag {
        "titleBlock" | "orphanedTitle" => "title",
        other => other,
    }
}

fn is_paragraph_level(tag: &str, subcorpus: Subcorpus) -> bool {
    if tag == "br" {
        // The OY rule: `br` does not break a paragraph in Yahoo! blogs.
        subcorpus != Subcorpus::OY
    } else {
        PARAGRAPH_TAGS.contains(&tag)
    }
}

/// One paragraph of a parsed entry: the recorded tag set (deduplicated,
/// sorted for determinism) and the non-empty sentence texts, in document
/// order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BccwjParagraph {
    pub tags: Box<[String]>,
    pub sentences: Box<[String]>,
}

/// A parsed C-XML entry: the non-empty paragraphs in document order.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BccwjDocument {
    pub paragraphs: Box<[BccwjParagraph]>,
}

/// The zstd magic, recognized only to refuse: canonical BCCWJ entries are
/// plain UTF-8 XML. The zstd layer in the corpus-utils reference is its own
/// local runtime cache (`zipfile-cached-files` writes `<entry>.zstd` copies
/// into a `./cache/` directory and reads those back), not a property of the
/// distribution — verified across all 198,312 entries of the 2012
/// distribution, none of which carry the magic. A zstd-prefixed entry is
/// therefore an error, not an input to decode (spec §3.4, rev 11).
const ZSTD_MAGIC: &[u8; 4] = b"\x28\xb5\x2f\xfd";

/// Parses one C-XML entry into a [`BccwjDocument`].
///
/// # Errors
///
/// A zstd-compressed entry (refused on the `ZSTD_MAGIC` prefix), invalid UTF-8,
/// malformed XML (naming the offending construct), or an unterminated
/// element at end of input (naming the tag).
pub fn parse_entry(bytes: &[u8], subcorpus: Subcorpus) -> io::Result<BccwjDocument> {
    if bytes.starts_with(ZSTD_MAGIC) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "zstd-compressed C-XML entry: canonical BCCWJ entries are plain XML; the zstd layer in the corpus-utils reference is its own local cache, not a distribution input",
        ));
    }
    let xml = String::from_utf8(bytes.to_vec())
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
    let mut reader = Reader::from_str(&xml);

    let mut stack: Vec<String> = Vec::new();
    let mut paragraphs: Vec<BccwjParagraph> = Vec::new();
    let mut current = ParagraphBuilder::default();
    // Character data accumulates across `Text`/`GeneralRef` events; the
    // reader emits entity references as separate events, so the raw
    // `&amp;` spelling is reassembled here and unescaped on flush.
    let mut raw_text = String::new();

    loop {
        let event = match reader.read_event() {
            Ok(event) => event,
            Err(err) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("malformed C-XML: {err}"),
                ));
            }
        };
        match event {
            Event::Start(start) => {
                flush_text(&mut raw_text, &mut current)?;
                handle_element(
                    &local_name_str(start.local_name())?,
                    &mut stack,
                    &mut paragraphs,
                    &mut current,
                    subcorpus,
                    false,
                )?;
            }
            Event::Empty(empt) => {
                flush_text(&mut raw_text, &mut current)?;
                // A self-closing element: the same open-and-close as a
                // Start/End pair, with no children in between.
                handle_element(
                    &local_name_str(empt.local_name())?,
                    &mut stack,
                    &mut paragraphs,
                    &mut current,
                    subcorpus,
                    true,
                )?;
            }
            Event::End(end) => {
                flush_text(&mut raw_text, &mut current)?;
                let name = local_name_str(end.local_name())?;
                // `br` never enters the stack — outside OY it opens a
                // paragraph with the stack unchanged, and under OY it is
                // ignored entirely — so `</br>` must never pop in either
                // subcorpus. Popping under OY silently removed the enclosing
                // element and stripped a coded ancestor (`speech`, …) from
                // every later paragraph's tag set.
                if name != "br" {
                    // `check_end_names` (on by default) guarantees the name
                    // matches the innermost open element.
                    stack.pop();
                }
            }
            Event::Text(text) => {
                raw_text.push_str(&text.decode().map_err(|err| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("malformed C-XML: {err}"),
                    )
                })?);
            }
            Event::CData(data) => {
                raw_text.push_str(&data.decode().map_err(|err| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("malformed C-XML: {err}"),
                    )
                })?);
            }
            Event::GeneralRef(reference) => {
                // Reassemble the raw `&name;` spelling for the unescaper.
                let name = std::str::from_utf8(reference.as_ref()).map_err(|_| {
                    io::Error::new(io::ErrorKind::InvalidData, "non-UTF-8 reference name")
                })?;
                raw_text.push_str(&format!("&{name};"));
            }
            Event::Eof => break,
            // StartDoc is absent (from_str); Decl, PI, Comment, DocType:
            // no effect.
            _ => {}
        }
    }
    flush_text(&mut raw_text, &mut current)?;

    if let Some(tag) = stack.last() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unterminated element <{tag}> at end of input"),
        ));
    }
    current.flush(&mut paragraphs);
    Ok(BccwjDocument {
        paragraphs: paragraphs.into_boxed_slice(),
    })
}

/// The element's local name as UTF-8. The name borrows from the event,
/// which does not outlive the match arm, so the result is owned.
fn local_name_str(name: quick_xml::name::LocalName<'_>) -> io::Result<String> {
    std::str::from_utf8(name.into_inner())
        .map(str::to_owned)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "non-UTF-8 element name"))
}

/// Unescapes the accumulated character-data run and appends it to the open
/// sentence. `mem::take` keeps the (possibly borrowed) unescaped text valid
/// after the buffer is reset.
///
/// Whitespace-only runs are dropped: the reference parses with
/// `clojure.xml/parse`, which filters whitespace-only text nodes out of the
/// tree (verified: the `\n` between C-XML tags never reaches
/// `walk-and-emit`), so a faithful port must not either.
///
/// The dropped class is **ASCII** whitespace only, and deliberately not
/// Rust's `str::trim`. The rule exists to discard the newline-plus-indent
/// runs between C-XML tags, which are ASCII; `str::trim` spans Unicode
/// `White_Space`, so it would additionally delete a run made only of
/// U+3000 IDEOGRAPHIC SPACE or U+00A0 NBSP — corpus text in a Japanese
/// corpus, kept by every Java whitespace predicate the reference could be
/// using (`String.trim` strips only chars ≤ U+0020;
/// `Character.isWhitespace` excludes the non-breaking spaces). Deleting
/// text is the unrecoverable direction, so the narrower class wins.
fn flush_text(raw_text: &mut String, current: &mut ParagraphBuilder) -> io::Result<()> {
    if raw_text.is_empty() {
        return Ok(());
    }
    let taken: String = std::mem::take(raw_text);
    // XML 1.0 §2.11: `#xD#xA` and lone `#xD` are normalized to `#xA`, and
    // §4.6 puts that normalization *before* reference expansion — so a
    // literal CR becomes LF while `&#xD;` survives as CR. Normalizing the
    // raw run first is what keeps the two apart; `quick-xml` passes raw
    // bytes through, and the reference's JDK-backed `clojure.xml/parse`
    // does not. Only the `\r`-bearing run pays for a re-encode.
    let normalized = if taken.contains('\r') {
        Some(taken.replace("\r\n", "\n").replace('\r', "\n"))
    } else {
        None
    };
    let source = normalized.as_deref().unwrap_or(&taken);
    let text = quick_xml::escape::unescape(source).map_err(|err| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("invalid reference: {err}"),
        )
    })?;
    if !text
        .chars()
        .all(|character| character.is_ascii_whitespace())
    {
        current.append(&text);
    }
    Ok(())
}

/// One element's open-side effect, shared by `Start` and `Empty`:
/// paragraph-level tags close the current paragraph and open a new one;
/// `<sentence>` adds a slot; everything else just enters the stack. A
/// self-closing (`empty`) element is popped immediately after.
fn handle_element(
    tag: &str,
    stack: &mut Vec<String>,
    paragraphs: &mut Vec<BccwjParagraph>,
    current: &mut ParagraphBuilder,
    subcorpus: Subcorpus,
    empty: bool,
) -> io::Result<()> {
    let tag = rename(tag);
    let is_br = tag == "br";
    if is_paragraph_level(tag, subcorpus) {
        current.flush(paragraphs);
        if !is_br {
            // `br` never enters the stack; the set is unchanged. The `End`
            // arm mirrors this by never popping for `</br>`.
            stack.push(tag.to_owned());
        }
        current.open(stack_tags(stack));
        if empty && !is_br {
            stack.pop();
        }
    } else if !is_br {
        if tag == "sentence" {
            current.new_sentence();
        }
        // Every other element (sentence included) enters the stack; the
        // reference pushes `:sentence` too, so `</sentence>` pops cleanly.
        stack.push(tag.to_owned());
        if empty {
            stack.pop();
        }
    }
    Ok(())
}

/// The open paragraph: its recorded tag set and the sentence slots built so
/// far. A paragraph always starts with one implicit empty sentence slot
/// (the zipper's initial slot); `<sentence>` adds further slots.
#[derive(Debug, Default)]
struct ParagraphBuilder {
    tags: Option<Box<[String]>>,
    sentences: Vec<String>,
}

impl ParagraphBuilder {
    fn open(&mut self, tags: Box<[String]>) {
        self.sentences.clear();
        self.sentences.push(String::new());
        self.tags = Some(tags);
    }

    /// Closes the open paragraph, keeping it only if it has a non-empty
    /// sentence (the `parse-document` filter).
    fn flush(&mut self, paragraphs: &mut Vec<BccwjParagraph>) {
        let Some(tags) = self.tags.take() else {
            return;
        };
        let sentences: Vec<String> = self
            .sentences
            .drain(..)
            .filter(|sentence| !sentence.is_empty())
            .collect();
        if !sentences.is_empty() {
            paragraphs.push(BccwjParagraph {
                tags,
                sentences: sentences.into_boxed_slice(),
            });
        }
    }

    /// The zipper starts a placeholder paragraph (empty tag set) before the
    /// first paragraph-level start; C-XML always opens with `<article>`, so
    /// this only guards degenerate entries.
    fn new_sentence(&mut self) {
        if self.tags.is_none() {
            self.open(Box::new([]));
        }
        self.sentences.push(String::new());
    }

    fn append(&mut self, text: &str) {
        if self.tags.is_none() {
            self.open(Box::new([]));
        }
        self.sentences
            .last_mut()
            .expect("a sentence slot is always open")
            .push_str(text);
    }
}

/// The current stack as the paragraph's recorded tag set: deduplicated and
/// sorted (the reference folds to a set; the sort is the determinism pin).
fn stack_tags(stack: &[String]) -> Box<[String]> {
    let set: BTreeSet<&str> = stack.iter().map(String::as_str).collect();
    set.into_iter().map(String::from).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// All fixture text is self-authored (§2).
    const SIMPLE: &str = r#"<article>
  <titleBlock>
    <title><sentence><orth>仮題の</orth><orth>一行</orth></sentence></title>
  </titleBlock>
  <paragraph><sentence><orth>最初の</orth><orth>文。</orth></sentence><sentence><orth>二つ目の</orth><orth>文。</orth></sentence></paragraph>
  <paragraph><sentence><orth>二段落目。</orth></sentence></paragraph>
</article>"#;

    fn document(xml: &str, subcorpus: Subcorpus) -> BccwjDocument {
        parse_entry(xml.as_bytes(), subcorpus).expect("parse")
    }

    fn tags(paragraph: &BccwjParagraph) -> Vec<&str> {
        paragraph.tags.iter().map(String::as_str).collect()
    }

    fn sentences(paragraph: &BccwjParagraph) -> Vec<&str> {
        paragraph.sentences.iter().map(String::as_str).collect()
    }

    #[test]
    fn paragraphs_sentences_and_text_compose() {
        let doc = document(SIMPLE, Subcorpus::LB);
        assert_eq!(doc.paragraphs.len(), 3);
        // titleBlock/orphanedTitle are renamed to title at push time; the
        // title paragraph carries the whole ancestor path.
        assert_eq!(tags(&doc.paragraphs[0]), ["article", "title"]);
        assert_eq!(sentences(&doc.paragraphs[0]), ["仮題の一行"]);
        assert_eq!(
            sentences(&doc.paragraphs[1]),
            ["最初の文。", "二つ目の文。"]
        );
        // The root `<article>` opens the document's first (dropped) paragraph
        // but is in every surviving paragraph's tag set.
        assert_eq!(tags(&doc.paragraphs[1]), ["article", "paragraph"]);
        assert_eq!(sentences(&doc.paragraphs[2]), ["二段落目。"]);
    }

    #[test]
    fn nested_coded_tags_carry_the_full_ancestry() {
        let xml = "<article><speech><quotation><paragraph><sentence><orth>引用文。</orth></sentence></paragraph></quotation></speech></article>";
        let doc = document(xml, Subcorpus::LB);
        // The textless speech and quotation paragraphs are dropped, leaving
        // the inner paragraph with its full ancestor tag set.
        assert_eq!(doc.paragraphs.len(), 1);
        assert_eq!(
            tags(&doc.paragraphs[0]),
            ["article", "paragraph", "quotation", "speech"]
        );
    }

    #[test]
    fn br_opens_a_paragraph_outside_oy() {
        let xml = "<article><paragraph><sentence><orth>改行前。</orth></sentence></paragraph><br/><paragraph><sentence><orth>改行後。</orth></sentence></paragraph></article>";
        let doc = document(xml, Subcorpus::LB);
        // The `<br/>` opens a paragraph with the unchanged stack; it has no
        // text, so it is dropped, leaving the two real paragraphs.
        assert_eq!(doc.paragraphs.len(), 2);
        assert_eq!(sentences(&doc.paragraphs[0]), ["改行前。"]);
        assert_eq!(sentences(&doc.paragraphs[1]), ["改行後。"]);

        // Text after a `<br/>` and before the next `<sentence>` lands in the
        // br paragraph (the zipper's slot-0 behavior).
        let xml = "<article><br/><orth>冒頭の</orth><orth>テキスト。</orth></article>";
        let doc = document(xml, Subcorpus::LB);
        assert_eq!(doc.paragraphs.len(), 1);
        assert_eq!(tags(&doc.paragraphs[0]), ["article"]);
        assert_eq!(sentences(&doc.paragraphs[0]), ["冒頭のテキスト。"]);
    }

    #[test]
    fn oy_does_not_break_paragraphs_on_br() {
        let xml = "<article><paragraph><sentence><orth>一。</orth></sentence><br/><sentence><orth>二。</orth></sentence></paragraph></article>";
        let doc = document(xml, Subcorpus::OY);
        assert_eq!(doc.paragraphs.len(), 1);
        assert_eq!(sentences(&doc.paragraphs[0]), ["一。", "二。"]);

        // For LB the `<br/>` opens its own paragraph, so the second
        // sentence (still inside the same `<paragraph>` element) belongs to
        // the br paragraph.
        let doc = document(xml, Subcorpus::LB);
        assert_eq!(doc.paragraphs.len(), 2);
        assert_eq!(sentences(&doc.paragraphs[1]), ["二。"]);
    }

    #[test]
    fn self_closing_paragraph_level_tags_are_dropped() {
        let xml = "<article><blockEnd/><paragraph><sentence><orth>文。</orth></sentence></paragraph><blockEnd/></article>";
        let doc = document(xml, Subcorpus::LB);
        assert_eq!(doc.paragraphs.len(), 1);
        assert_eq!(sentences(&doc.paragraphs[0]), ["文。"]);
    }

    #[test]
    fn empty_sentences_and_paragraphs_are_dropped() {
        let xml = "<article><paragraph><sentence/><sentence><orth>文。</orth></sentence></paragraph><paragraph><sentence></sentence></paragraph></article>";
        let doc = document(xml, Subcorpus::LB);
        assert_eq!(doc.paragraphs.len(), 1);
        assert_eq!(sentences(&doc.paragraphs[0]), ["文。"]);
    }

    #[test]
    fn entities_are_unescaped_and_whitespace_is_kept() {
        let xml = "<article><paragraph><sentence><orth>A &amp; B &lt;C&gt;</orth></sentence></paragraph></article>";
        let doc = document(xml, Subcorpus::LB);
        assert_eq!(sentences(&doc.paragraphs[0]), ["A & B <C>"]);
    }

    #[test]
    fn carriage_returns_are_normalized_to_line_feeds() {
        // XML 1.0 §2.11: `#xD#xA` and lone `#xD` in character data become `#xA`.
        let xml = "<article><paragraph><sentence>前\r\n後</sentence><sentence>孤\r立</sentence></paragraph></article>";
        let doc = document(xml, Subcorpus::LB);
        assert_eq!(sentences(&doc.paragraphs[0]), ["前\n後", "孤\n立"]);
    }

    #[test]
    fn a_numeric_carriage_return_reference_survives_normalization() {
        // §4.6: line-ending normalization runs before reference expansion,
        // so `&#13;` is a CR the parser must not rewrite — only a *literal*
        // CR becomes LF.
        let xml = "<article><paragraph><sentence>前&#13;後</sentence></paragraph></article>";
        let doc = document(xml, Subcorpus::LB);
        assert_eq!(sentences(&doc.paragraphs[0]), ["前\r後"]);
    }

    #[test]
    fn non_ascii_whitespace_runs_are_corpus_text_and_are_kept() {
        // The drop rule targets the ASCII newline-and-indent between tags.
        // A run made only of U+3000 or U+00A0 is text, and deleting it would
        // silently shorten a sentence.
        let xml = "<article><paragraph>\n  <sentence>\u{3000}<orth>本文。</orth></sentence>\n  <sentence><orth>甲</orth>\u{a0}<orth>乙</orth></sentence>\n</paragraph></article>";
        let doc = document(xml, Subcorpus::LB);
        assert_eq!(
            sentences(&doc.paragraphs[0]),
            ["\u{3000}本文。", "甲\u{a0}乙"]
        );

        // The ASCII run between tags is still dropped, which is the whole
        // point of the rule.
        let xml = "<article><paragraph>\n  <sentence>\n    <orth>本文。</orth>\n  </sentence>\n</paragraph></article>";
        let doc = document(xml, Subcorpus::LB);
        assert_eq!(sentences(&doc.paragraphs[0]), ["本文。"]);
    }

    #[test]
    fn a_paired_br_never_pops_the_tag_stack() {
        // `<br></br>` (rather than `<br/>`) must leave the stack exactly as
        // `<br/>` does, in both subcorpora: under OY a popped end tag would
        // strip `speech` from every later paragraph.
        let xml = "<article><speech><paragraph><br></br><sentence><orth>甲。</orth></sentence></paragraph><paragraph><sentence><orth>乙。</orth></sentence></paragraph></speech></article>";
        for subcorpus in [Subcorpus::OY, Subcorpus::LB] {
            let doc = document(xml, subcorpus);
            let last = doc.paragraphs.last().expect("a paragraph");
            assert_eq!(sentences(last), ["乙。"], "{subcorpus:?}");
            assert_eq!(
                tags(last),
                ["article", "paragraph", "speech"],
                "{subcorpus:?}"
            );
        }
    }

    #[test]
    fn unterminated_elements_fail_with_the_tag_named() {
        let error = parse_entry(b"<article><paragraph><sentence/>", Subcorpus::LB)
            .expect_err("unterminated");
        let message = error.to_string();
        assert!(message.contains("<paragraph>"), "{message}");
    }

    #[test]
    fn malformed_xml_fails() {
        assert!(parse_entry(b"<article></wrong>", Subcorpus::LB).is_err());
        assert!(parse_entry(b"\xff\xfe\x00<nul>", Subcorpus::LB).is_err());
    }

    #[test]
    fn zstd_compressed_entries_are_refused_not_decoded() {
        // The reference's zstd layer is its own local cache, not a
        // distribution input; a zstd-magic entry is refused with the reason
        // named instead of silently decoded.
        let compressed =
            zstd::encode_all(std::io::Cursor::new(SIMPLE.as_bytes()), 3).expect("compress");
        let error = parse_entry(&compressed, Subcorpus::LB).expect_err("zstd refused");
        let message = error.to_string();
        assert!(message.contains("zstd"), "{message}");
        assert!(message.contains("plain XML"), "{message}");
    }
}
