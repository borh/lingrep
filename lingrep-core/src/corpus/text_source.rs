use super::markdown_dialect::{MarkdownDiagnostics, MarkdownReaderConfig};
use super::structured::{
    AdmittedCorpus, CorpusEvent, Metadata, MetadataScalar, MetadataValue, metadata_from_json,
    sentence_texts,
};
use crate::utils::split_sentence_ja;
use serde_json::{Map, Value};
use std::collections::VecDeque;
use std::io::{self, BufRead};

#[derive(Debug, Clone)]
pub enum InputFormat {
    Jsonl,
    Plaintext,
    Markdown(MarkdownReaderConfig),
    Conllu,
}

struct ConlluEvents<R> {
    sentences: super::conllu::Reader<R>,
    projection: crate::token_match::BoundProjection,
    source_name: String,
    /// Structural events queued ahead of the next reader pull. A `# newdoc`
    /// boundary closes one document and opens another, which is five events
    /// for one decoded sentence, so the surplus waits here.
    pending: VecDeque<CorpusEvent>,
    document_open: bool,
    finished: bool,
}

impl<R: BufRead> ConlluEvents<R> {
    fn document_start(&self, newdoc_id: Option<&str>) -> CorpusEvent {
        let metadata: Metadata = match newdoc_id {
            Some(id) => Box::new([(
                "newdoc_id".to_owned(),
                MetadataValue::Scalar(MetadataScalar::String(id.to_owned())),
            )]),
            None => Box::new([]),
        };
        CorpusEvent::DocumentStart {
            source_name: self.source_name.clone(),
            metadata,
        }
    }
}

impl<R: BufRead> Iterator for ConlluEvents<R> {
    type Item = io::Result<CorpusEvent>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(event) = self.pending.pop_front() {
                return Some(Ok(event));
            }
            if self.finished {
                return None;
            }
            match self.sentences.next() {
                Some(Ok(sentence)) => {
                    let (text, words, enhanced_graph, sent_id, newdoc) = match sentence
                        .into_projected_parts(&self.projection)
                    {
                        Ok(parts) => parts,
                        Err(error) => {
                            return Some(Err(io::Error::new(io::ErrorKind::InvalidData, error)));
                        }
                    };
                    if !self.document_open {
                        // The first document opens whether or not the file
                        // marks it; a leading `# newdoc id` names it rather
                        // than preceding an empty document.
                        let start =
                            self.document_start(newdoc.as_ref().and_then(|b| b.id.as_deref()));
                        self.pending.push_back(start);
                        self.pending.push_back(CorpusEvent::DivisionObservation { kind: "paragraph".into(), observation: super::DivisionObservation::Unavailable { reason: "CoNLL-U paragraph boundaries are not observed by this reader; within-sentence boundaries are unsupported".into() } });
                        self.document_open = true;
                    } else if let Some(boundary) = &newdoc {
                        self.pending.push_back(CorpusEvent::DocumentEnd);
                        let start = self.document_start(boundary.id.as_deref());
                        self.pending.push_back(start);
                        self.pending.push_back(CorpusEvent::DivisionObservation { kind: "paragraph".into(), observation: super::DivisionObservation::Unavailable { reason: "CoNLL-U paragraph boundaries are not observed by this reader; within-sentence boundaries are unsupported".into() } });
                    }
                    let metadata: Metadata = match sent_id {
                        Some(id) => Box::new([(
                            "sent_id".to_owned(),
                            MetadataValue::Scalar(MetadataScalar::String(id)),
                        )]),
                        None => Box::new([]),
                    };
                    self.pending.push_back(CorpusEvent::Sentence {
                        text,
                        words: Some(words),
                        enhanced_graph: Some(enhanced_graph),
                        active_labels: Box::new([]),
                        metadata,
                    });
                }
                Some(Err(error)) => return Some(Err(error)),
                None => {
                    self.finished = true;
                    if self.document_open {
                        self.document_open = false;
                    } else {
                        // An empty stream keeps its historical shape: one
                        // empty document rather than no events at all.
                        self.pending.push_back(self.document_start(None));
                        self.pending.push_back(CorpusEvent::DivisionObservation { kind: "paragraph".into(), observation: super::DivisionObservation::Unavailable { reason: "CoNLL-U paragraph boundaries are not observed by this reader; within-sentence boundaries are unsupported".into() } });
                    }

                    self.pending.push_back(CorpusEvent::DocumentEnd);
                }
            }
        }
    }
}

const MAX_JSONL_RECORD_BYTES: usize = 16 * 1024 * 1024;

struct JsonLines<R> {
    reader: R,
    failed: bool,
}
impl<R: BufRead> Iterator for JsonLines<R> {
    type Item = io::Result<String>;
    fn next(&mut self) -> Option<Self::Item> {
        use std::io::Read;
        if self.failed {
            return None;
        }
        let mut bytes = Vec::new();
        let result = (&mut self.reader)
            .take((MAX_JSONL_RECORD_BYTES + 1) as u64)
            .read_until(b'\n', &mut bytes);
        match result {
            Ok(0) => None,
            Ok(_) if bytes.len() > MAX_JSONL_RECORD_BYTES => {
                self.failed = true;
                Some(Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "JSONL record exceeds 16 MiB",
                )))
            }
            Ok(_) => {
                if bytes.last() == Some(&b'\n') {
                    bytes.pop();
                    if bytes.last() == Some(&b'\r') {
                        bytes.pop();
                    }
                }
                Some(
                    String::from_utf8(bytes)
                        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error)),
                )
            }
            Err(error) => {
                self.failed = true;
                Some(Err(error))
            }
        }
    }
}
struct JsonlEvents<R> {
    lines: JsonLines<R>,
    pending: VecDeque<CorpusEvent>,
    line_num: usize,
    source_name: String,
}

struct PlaintextEvents<R> {
    lines: std::io::Lines<R>,
    /// Sentence segments produced by splitting the current physical line on
    /// Japanese sentence delimiters (`。！？` and a following closing
    /// quotation), via [`split_sentence_ja`]). Drained one `Sentence` event per
    /// `next()` call before the next physical line is read. This keeps the
    /// shared plaintext stream's sentence boundaries aligned with the legacy
    /// `SentenceFramer` (which frames raw bytes the same way), so a plain
    /// corpus whose sentences share a physical line is still split into one
    /// admitted sentence per delimiter, not one per line. Blank segments are
    /// admitted as `Sentence` events and dropped by the admission layer.
    pending: VecDeque<String>,
    state: u8,
    source_name: String,
}

impl<R: BufRead> Iterator for PlaintextEvents<R> {
    type Item = io::Result<CorpusEvent>;

    fn next(&mut self) -> Option<Self::Item> {
        match self.state {
            0 => {
                self.state = 1;
                Some(Ok(CorpusEvent::DocumentStart {
                    source_name: self.source_name.clone(),
                    metadata: Box::new([]),
                }))
            }
            1 => {
                self.state = 2;
                Some(Ok(CorpusEvent::DivisionObservation {
                    kind: "paragraph".into(),
                    observation: super::DivisionObservation::Unavailable {
                        reason: "plain text does not declare authored paragraph boundaries".into(),
                    },
                }))
            }
            2 => loop {
                if let Some(segment) = self.pending.pop_front() {
                    return Some(Ok(CorpusEvent::Sentence {
                        text: segment,
                        words: None,
                        enhanced_graph: None,
                        active_labels: Box::new([]),
                        metadata: Box::new([]),
                    }));
                }
                match self.lines.next() {
                    Some(Ok(line)) => {
                        for segment in split_sentence_ja(&line) {
                            self.pending.push_back(segment);
                        }
                        // Loop: drain the just-queued segments, or read the
                        // next physical line if `split_sentence_ja` produced
                        // none (e.g. a blank line).
                    }
                    Some(Err(error)) => return Some(Err(error)),
                    None => {
                        self.state = 4;
                        return Some(Ok(CorpusEvent::DocumentEnd));
                    }
                }
            },
            _ => None,
        }
    }
}

impl<R: BufRead> Iterator for JsonlEvents<R> {
    type Item = io::Result<CorpusEvent>;

    fn next(&mut self) -> Option<Self::Item> {
        if let Some(event) = self.pending.pop_front() {
            return Some(Ok(event));
        }
        loop {
            let line = self.lines.next()?;
            self.line_num += 1;
            let line = match line {
                Ok(line) => line,
                Err(error) => return Some(Err(error)),
            };
            let context = format!("{} line {}", self.source_name, self.line_num);
            let document = match serde_json::from_str::<Map<String, Value>>(&line) {
                Ok(document) => document,
                Err(error) => {
                    return Some(Err(invalid_jsonl(
                        &context,
                        &format!("failed to parse JSON object: {error}"),
                    )));
                }
            };
            match jsonl_document_events(document, &context) {
                Ok(events) => {
                    self.pending.extend(events);
                    if let Some(event) = self.pending.pop_front() {
                        return Some(Ok(event));
                    }
                }
                Err(error) => return Some(Err(error)),
            }
        }
    }
}

fn jsonl_document_events(
    mut document: Map<String, Value>,
    source_name: &str,
) -> io::Result<Vec<CorpusEvent>> {
    let structure = document.remove("structure");
    let sentences = document.remove("sentences");
    let paragraphs = document.remove("paragraphs");
    let text = if sentences.is_none() && paragraphs.is_none() {
        document.remove("text")
    } else {
        None
    };
    if sentences.is_some() && paragraphs.is_some() {
        return Err(invalid_jsonl(
            source_name,
            "document cannot contain both `sentences` and `paragraphs`",
        ));
    }
    if sentences.is_none() && paragraphs.is_none() && text.is_none() {
        return Err(invalid_jsonl(
            source_name,
            "document must contain `text`, `sentences`, or `paragraphs`",
        ));
    }
    let metadata = take_flexible_metadata(&mut document, source_name)?;
    let mut events = vec![CorpusEvent::DocumentStart {
        source_name: source_name.to_owned(),
        metadata,
    }];
    if let Some(sentences) = sentences {
        push_sentences(&mut events, sentences, source_name)?;
    } else if let Some(text) = text {
        let Value::String(text) = text else {
            return Err(invalid_jsonl(source_name, "`text` must be a string"));
        };

        for text in split_sentence_ja(&text) {
            events.push(CorpusEvent::Sentence {
                text,
                words: None,
                enhanced_graph: None,
                active_labels: Box::new([]),
                metadata: Box::new([]),
            });
        }
    } else if let Some(Value::Array(paragraphs)) = paragraphs {
        events.push(CorpusEvent::DivisionObservation {
            kind: "paragraph".into(),
            observation: super::DivisionObservation::Observed,
        });
        for (ordinal, paragraph) in paragraphs.into_iter().enumerate() {
            let Value::Object(mut paragraph) = paragraph else {
                return Err(invalid_jsonl(source_name, "paragraph must be an object"));
            };
            let sentences = paragraph
                .remove("sentences")
                .ok_or_else(|| invalid_jsonl(source_name, "paragraph is missing `sentences`"))?;
            let metadata = take_flexible_metadata(
                &mut paragraph,
                &format!("{source_name} paragraph {}", ordinal + 1),
            )?;
            events.push(CorpusEvent::ParagraphStart { metadata });
            push_sentences(&mut events, sentences, source_name)?;
            events.push(CorpusEvent::ParagraphEnd);
        }
    } else {
        return Err(invalid_jsonl(source_name, "`paragraphs` must be an array"));
    }
    events.push(CorpusEvent::DocumentEnd);
    apply_divisions(events, structure, source_name)
}

fn take_flexible_metadata(
    object: &mut Map<String, Value>,
    context: &str,
) -> io::Result<super::structured::Metadata> {
    let mut metadata = match object.remove("metadata") {
        None => Map::new(),
        Some(Value::Object(metadata)) => metadata,
        Some(_) => return Err(invalid_jsonl(context, "`metadata` must be an object")),
    };
    for (key, value) in std::mem::take(object) {
        if metadata.insert(key.clone(), value).is_some() {
            return Err(invalid_jsonl(
                context,
                &format!("metadata key `{key}` is declared twice"),
            ));
        }
    }
    metadata_from_json(Value::Object(metadata), context)
}

fn push_sentences(
    events: &mut Vec<CorpusEvent>,
    sentences: Value,
    context: &str,
) -> io::Result<()> {
    let Value::Array(sentences) = sentences else {
        return Err(invalid_jsonl(context, "`sentences` must be an array"));
    };
    for (ordinal, sentence) in sentences.into_iter().enumerate() {
        let context = format!("{context} sentence {}", ordinal + 1);
        let (text, metadata) = match sentence {
            Value::String(text) => (text, Box::new([]) as Metadata),
            Value::Object(mut object) => {
                let Some(Value::String(text)) = object.remove("text") else {
                    return Err(invalid_jsonl(
                        &context,
                        "sentence object requires a string `text`",
                    ));
                };
                let metadata = take_flexible_metadata(&mut object, &context)?;
                (text, metadata)
            }
            _ => {
                return Err(invalid_jsonl(
                    &context,
                    "sentence must be a string or an object with `text`",
                ));
            }
        };
        events.push(CorpusEvent::Sentence {
            text,
            words: None,
            enhanced_graph: None,
            active_labels: Box::new([]),
            metadata,
        });
    }
    Ok(())
}

fn invalid_jsonl(context: &str, message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, format!("{context}: {message}"))
}

pub fn stream_input_reader<R>(
    format: InputFormat,
    reader: R,
    diagnostics: Option<MarkdownDiagnostics>,
) -> Box<dyn Iterator<Item = io::Result<String>> + Send>
where
    R: BufRead + Send + 'static,
{
    Box::new(sentence_texts(AdmittedCorpus::new(stream_input_events(
        format,
        reader,
        "<input>".into(),
        diagnostics,
    ))))
}

pub fn stream_input_events<R>(
    format: InputFormat,
    reader: R,
    source_name: String,
    diagnostics: Option<MarkdownDiagnostics>,
) -> Box<dyn Iterator<Item = io::Result<CorpusEvent>> + Send>
where
    R: BufRead + Send + 'static,
{
    match format {
        InputFormat::Jsonl => Box::new(JsonlEvents {
            lines: JsonLines {
                reader,
                failed: false,
            },
            pending: VecDeque::new(),
            line_num: 0,
            source_name,
        }),
        InputFormat::Plaintext => Box::new(PlaintextEvents {
            lines: reader.lines(),
            pending: VecDeque::new(),
            state: 0,
            source_name,
        }),
        InputFormat::Conllu => {
            let schema = super::conllu::token_schema();
            let projection = schema
                .bind_projection(schema.keys().iter().cloned())
                .expect("the complete CoNLL-U schema projects itself");
            Box::new(ConlluEvents {
                sentences: super::conllu::Reader::new(reader, source_name.clone()),
                projection,
                source_name,
                pending: VecDeque::new(),
                document_open: false,
                finished: false,
            })
        }
        InputFormat::Markdown(config) => Box::new(super::markdown_dialect::read_markdown_events(
            reader,
            config,
            // `None` means the caller has no interest in the labels this stream
            // observed — a throwaway sink the caller cannot read. The reader
            // still writes into one so its diagnostics path is uniform; the
            // caller simply never looks. `Some` is a shared sink the caller
            // retains a clone of so it survives consuming the iterator.
            diagnostics.unwrap_or_default(),
            source_name,
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::super::section_policy::SectionPolicy;
    use super::super::structured::{MetadataScalar, MetadataValue};
    use super::*;
    use std::collections::BTreeSet;
    use std::io::{BufReader, Cursor};

    #[test]
    fn markdown_format_reads_the_corpus_dialect() {
        let markdown = "---\ntitle: SECRET\n---\n\nこれは**強調**された文です。\n";
        let config = MarkdownReaderConfig::new(SectionPolicy::new(None, BTreeSet::new()));
        let text = stream_input_reader(
            InputFormat::Markdown(config),
            BufReader::new(Cursor::new(markdown)),
            None,
        )
        .collect::<io::Result<Vec<_>>>()
        .unwrap();

        assert_eq!(text, ["これは強調された文です。"]);
    }

    #[test]
    fn authored_structure_refuses_invalid_identity_parent_and_sentence_ranges() {
        let division = serde_json::json!({"id":"a","kind":"part","start":0,"end":1});
        for divisions in [
            serde_json::json!([division, division]),
            serde_json::json!([{"id":"a","kind":"part","start":0,"end":2}]),
            serde_json::json!([{"id":"a","kind":"part","start":1,"end":0}]),
            serde_json::json!([{"id":"a","kind":"part","start":0.5,"end":1}]),
            serde_json::json!([{"id":"a","kind":"part","parent":"a","start":0,"end":1}]),
            serde_json::json!([{"id":"a","kind":"part","parent":"later","start":0,"end":1}]),
            serde_json::json!([{"id":"a","kind":"part","start":0,"end":0},{"id":"b","kind":"section","parent":"a","start":0,"end":1}]),
            serde_json::json!([{"id":"a","kind":"bad kind","start":0,"end":1}]),
            serde_json::json!([{"id":"a","kind":"part","start":0,"end":1,"token_start":1}]),
        ] {
            let document =
                serde_json::json!({"sentences":["one"],"structure":{"divisions":divisions}});
            assert!(
                jsonl_document_events(document.as_object().unwrap().clone(), "invalid").is_err(),
                "{document}"
            );
        }
    }

    #[test]
    fn jsonl_record_budget_is_enforced_before_decoding() {
        let input = vec![b' '; 16 * 1024 * 1024 + 1];
        let mut events = stream_input_events(
            InputFormat::Jsonl,
            BufReader::new(Cursor::new(input)),
            "budget".into(),
            None,
        );
        assert!(
            events
                .next()
                .unwrap()
                .unwrap_err()
                .to_string()
                .contains("16 MiB")
        );
        assert!(events.next().is_none());
    }

    #[test]
    fn jsonl_format_is_unchanged() {
        let jsonl = "{\"sentences\":[\"一つ目。\",\"二つ目。\"]}\n";
        let text =
            stream_input_reader(InputFormat::Jsonl, BufReader::new(Cursor::new(jsonl)), None)
                .collect::<io::Result<Vec<_>>>()
                .unwrap();

        assert_eq!(text, ["一つ目。", "二つ目。"]);
    }

    #[test]
    fn jsonl_text_documents_use_the_canonical_sentence_framer() {
        let jsonl = "{\"text\":\"猫がいる。犬もいる！『本当？』\",\"source_id\":\"row-1\"}\n";
        let text =
            stream_input_reader(InputFormat::Jsonl, BufReader::new(Cursor::new(jsonl)), None)
                .collect::<io::Result<Vec<_>>>()
                .unwrap();

        assert_eq!(text, ["猫がいる。", "犬もいる！", "『本当？』"]);
    }

    fn conllu_events(input: &str) -> Vec<CorpusEvent> {
        stream_input_events(
            InputFormat::Conllu,
            BufReader::new(Cursor::new(input.to_owned())),
            "fixture.conllu".to_owned(),
            None,
        )
        .collect::<io::Result<Vec<_>>>()
        .unwrap()
    }

    fn scalar(value: &str) -> MetadataValue {
        MetadataValue::Scalar(MetadataScalar::String(value.to_owned()))
    }

    const ROW: &str = "1\t猫\t猫\tNOUN\t名詞\t_\t0\troot\t_\t_\n";

    #[test]
    fn conllu_without_newdoc_stays_one_document_with_sentence_metadata() {
        let input = format!("# sent_id = s1\n# text = 猫\n{ROW}\n# text = 猫\n{ROW}\n");
        let events = conllu_events(&input);

        assert_eq!(events.len(), 5);
        let CorpusEvent::DocumentStart { metadata, .. } = &events[0] else {
            panic!("first event must start the document");
        };
        assert!(metadata.is_empty());
        let CorpusEvent::Sentence {
            metadata,
            enhanced_graph,
            ..
        } = &events[2]
        else {
            panic!("third event must be the first sentence");
        };
        assert_eq!(metadata.as_ref(), [("sent_id".to_owned(), scalar("s1"))]);
        assert_eq!(
            enhanced_graph
                .as_ref()
                .map(super::super::conllu::EnhancedGraphObservation::state),
            Some(super::super::conllu::EnhancedGraphState::Unavailable)
        );
        let CorpusEvent::Sentence { metadata, .. } = &events[3] else {
            panic!("fourth event must be the second sentence");
        };
        assert!(metadata.is_empty(), "a comment-free sentence carries none");
        assert!(matches!(events[4], CorpusEvent::DocumentEnd));
    }

    #[test]
    fn enhanced_graph_capability_and_observation_survive_structured_admission() {
        let input = "# text = 猫\n1\t猫\t猫\tNOUN\t名詞\t_\t0\troot\t0:root\t_\n";
        let admitted = AdmittedCorpus::new(conllu_events(input).into_iter().map(Ok))
            .collect::<io::Result<Vec<_>>>()
            .unwrap();
        let sentence = admitted
            .iter()
            .find_map(|event| event.sentence())
            .expect("the CoNLL-U sentence must be admitted");
        let observation = sentence
            .enhanced_graph
            .as_ref()
            .expect("the CoNLL-U reader declares enhanced-graph capability");

        assert_eq!(
            observation.state(),
            super::super::conllu::EnhancedGraphState::Complete
        );
        assert_eq!(
            observation
                .graph()
                .expect("a complete observation carries a graph")
                .edges()
                .len(),
            1
        );

        let plain_events = stream_input_events(
            InputFormat::Plaintext,
            BufReader::new(Cursor::new("猫。\n")),
            "fixture.txt".to_owned(),
            None,
        )
        .collect::<io::Result<Vec<_>>>()
        .unwrap();
        let CorpusEvent::Sentence { enhanced_graph, .. } = &plain_events[2] else {
            panic!("third event must be the plaintext sentence");
        };
        assert!(
            enhanced_graph.is_none(),
            "a text reader must not claim graph observation capability"
        );
    }

    #[test]
    fn conllu_newdoc_boundaries_split_documents_and_name_them() {
        let input = format!(
            "# newdoc id = doc-1\n# text = 猫\n{ROW}\n# newdoc\n# text = 猫\n{ROW}\n# text = 猫\n{ROW}\n"
        );
        let events = conllu_events(&input);

        // doc-1 (one sentence), then an unnamed document (two sentences); a
        // leading newdoc names the first document rather than preceding an
        // empty one.
        let starts = events
            .iter()
            .filter_map(|event| match event {
                CorpusEvent::DocumentStart { metadata, .. } => Some(metadata.clone()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(starts.len(), 2);
        assert_eq!(
            starts[0].as_ref(),
            [("newdoc_id".to_owned(), scalar("doc-1"))]
        );
        assert!(starts[1].is_empty(), "a bare newdoc starts an unnamed one");
        let sentences_per_document =
            events.split_inclusive(|event| matches!(event, CorpusEvent::DocumentEnd));
        let counts = sentences_per_document
            .map(|document| {
                document
                    .iter()
                    .filter(|event| matches!(event, CorpusEvent::Sentence { .. }))
                    .count()
            })
            .collect::<Vec<_>>();
        assert_eq!(counts, [1, 2]);
    }

    #[test]
    fn conllu_empty_input_keeps_the_empty_document_shape() {
        let events = conllu_events("");

        assert_eq!(events.len(), 3);
        assert!(matches!(events[0], CorpusEvent::DocumentStart { .. }));
        assert!(matches!(events[1], CorpusEvent::DivisionObservation { .. }));
        assert!(matches!(events[2], CorpusEvent::DocumentEnd));
    }

    #[test]
    fn structured_jsonl_keeps_a_text_metadata_field() {
        let jsonl = "{\"sentences\":[\"猫。\"],\"text\":\"catalog label\"}\n";
        let events = stream_input_events(
            InputFormat::Jsonl,
            BufReader::new(Cursor::new(jsonl)),
            "fixture".to_owned(),
            None,
        )
        .collect::<io::Result<Vec<_>>>()
        .unwrap();

        let CorpusEvent::DocumentStart { metadata, .. } = &events[0] else {
            panic!("first event must start the document");
        };
        assert_eq!(
            metadata.as_ref(),
            [(
                "text".to_owned(),
                MetadataValue::Scalar(MetadataScalar::String("catalog label".to_owned()))
            )]
        );
    }
    #[test]
    fn jsonl_sentence_objects_reject_invalid_shapes_duplicates_and_field_limits() {
        for sentence in [
            serde_json::json!({}),
            serde_json::json!({"text":1}),
            serde_json::json!({"text":"x", "metadata":[]}),
            serde_json::json!({"text":"x", "metadata":{"id":1}, "id":2}),
            serde_json::json!(false),
            serde_json::json!({"text":"x", "metadata":{"large":"x".repeat(65537)}}),
        ] {
            let value = serde_json::json!({"sentences":[sentence]});
            let error = jsonl_document_events(value.as_object().unwrap().clone(), "captions.jsonl")
                .unwrap_err();
            assert!(
                error.to_string().contains("captions.jsonl sentence 1"),
                "{error}"
            );
        }
        let metadata = (0..257)
            .map(|index| (index.to_string(), Value::from(index)))
            .collect::<Map<_, _>>();
        let value = serde_json::json!({"sentences":[{"text":"x","metadata":metadata}]});
        assert!(
            jsonl_document_events(value.as_object().unwrap().clone(), "captions.jsonl")
                .unwrap_err()
                .to_string()
                .contains("256 fields")
        );
    }
    #[test]
    fn jsonl_sentence_metadata_stays_on_its_event_in_both_array_forms() {
        let sentence = serde_json::json!({"text":"猫", "metadata":{"role":"hit"}});
        for value in [
            serde_json::json!({"sentences":[sentence.clone(), "犬"]}),
            serde_json::json!({"paragraphs":[{"sentences":[sentence, "犬"]}]}),
        ] {
            let events =
                jsonl_document_events(value.as_object().unwrap().clone(), "captions.jsonl")
                    .unwrap();
            let CorpusEvent::DocumentStart { .. } = &events[0] else {
                panic!("missing document start");
            };
            assert!(events.iter().any(|event|matches!(event,CorpusEvent::Sentence { metadata,.. } if metadata.iter().any(|(key,value)|key=="role" && value==&scalar("hit")))));
        }
    }
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct JsonStructure {
    #[serde(default)]
    observed_kinds: Vec<String>,
    #[serde(default)]
    divisions: Vec<JsonDivision>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct JsonDivision {
    id: String,
    kind: String,
    #[serde(default)]
    parent: Option<String>,
    start: usize,
    end: usize,
    #[serde(default)]
    metadata: Map<String, Value>,
}

fn apply_divisions(
    events: Vec<CorpusEvent>,
    structure: Option<Value>,
    context: &str,
) -> io::Result<Vec<CorpusEvent>> {
    let Some(structure) = structure else {
        return Ok(events);
    };
    let structure: JsonStructure = serde_json::from_value(structure)
        .map_err(|error| invalid_jsonl(context, &error.to_string()))?;
    let count = events
        .iter()
        .filter(|event| matches!(event, CorpusEvent::Sentence { .. }))
        .count();
    let mut by_name = std::collections::BTreeMap::new();
    for (index, division) in structure.divisions.iter().enumerate() {
        super::divisions::validate_kind(&division.kind)?;
        if division.id.is_empty()
            || division.id.len() > 256
            || by_name.insert(division.id.as_str(), index).is_some()
        {
            return Err(invalid_jsonl(
                context,
                "division IDs must be unique nonempty strings of at most 256 bytes",
            ));
        }
        if division.start > division.end || division.end > count {
            return Err(invalid_jsonl(
                context,
                "division range is outside authored sentence boundaries",
            ));
        }
        if let Some(parent) = &division.parent {
            let Some(parent_index) = by_name.get(parent.as_str()).copied() else {
                return Err(invalid_jsonl(
                    context,
                    "division parent must be declared earlier",
                ));
            };
            if parent_index == index {
                return Err(invalid_jsonl(context, "division cannot parent itself"));
            }
            let parent = &structure.divisions[parent_index];
            if parent.start > division.start || parent.end < division.end {
                return Err(invalid_jsonl(
                    context,
                    "division extent is outside its parent",
                ));
            }
        }
    }
    let mut order = (0..structure.divisions.len()).collect::<Vec<_>>();
    order.sort_by_key(|index| {
        let division = &structure.divisions[*index];
        (division.start, std::cmp::Reverse(division.end), *index)
    });
    let mut ids = vec![0; order.len()];
    for (id, index) in order.iter().enumerate() {
        ids[*index] = id as u64;
    }
    let mut boundaries: std::collections::BTreeMap<
        usize,
        (Vec<CorpusEvent>, Vec<(u64, CorpusEvent)>),
    > = Default::default();
    for index in order {
        let division = &structure.divisions[index];
        let metadata = metadata_from_json(Value::Object(division.metadata.clone()), context)?;
        super::divisions::validate_metadata(&metadata)?;
        boundaries
            .entry(division.start)
            .or_default()
            .0
            .push(CorpusEvent::DivisionStart {
                source_id: Some(division.id.clone()),
                source_ordinal: ids[index],
                kind: division.kind.clone(),
                parent_source_ordinal: division
                    .parent
                    .as_ref()
                    .map(|name| ids[by_name[name.as_str()]]),
                metadata,
            });
        boundaries.entry(division.end).or_default().1.push((
            ids[index],
            CorpusEvent::DivisionEnd {
                source_ordinal: ids[index],
            },
        ));
    }
    let mut output = Vec::new();
    let mut ordinal = 0;
    for event in events {
        let document_start = matches!(event, CorpusEvent::DocumentStart { .. });
        let sentence = matches!(event, CorpusEvent::Sentence { .. });
        output.push(event);
        if document_start {
            let observed = structure
                .observed_kinds
                .iter()
                .chain(structure.divisions.iter().map(|division| &division.kind))
                .collect::<std::collections::BTreeSet<_>>();
            for kind in observed {
                super::divisions::validate_kind(kind)?;
                output.push(CorpusEvent::DivisionObservation {
                    kind: kind.clone(),
                    observation: super::DivisionObservation::Observed,
                });
            }
        }
        if sentence {
            ordinal += 1;
        }
        if (document_start || sentence)
            && let Some((open, mut close)) = boundaries.remove(&ordinal)
        {
            output.extend(open);
            close.sort_by_key(|(id, _)| std::cmp::Reverse(*id));
            output.extend(close.into_iter().map(|(_, event)| event));
        }
    }
    Ok(output)
}
