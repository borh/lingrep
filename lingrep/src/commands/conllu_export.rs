use std::collections::BTreeMap;
use std::io::Write;

use lingrep_core::corpus::SourceMetadataProjection;
use lingrep_core::corpus::conllu::{EnhancedGraph, EnhancedNodeId};
use lingrep_core::execution::{
    BoundSentenceInput, EnhancedGraphInput, ExecutionError, ExecutionSentence,
};
use lingrep_core::token_match::{AttributeKey, AttributeValueRef, TokenSequence};

use super::query_support::execution_output_io;
use crate::error::CliError;
use crate::protocol::query::Termination;

pub(super) fn metadata() -> Result<SourceMetadataProjection, CliError> {
    SourceMetadataProjection::new(vec![
        "@document.newdoc_id".into(),
        "@sentence.sent_id".into(),
    ])
    .map_err(CliError::corpus)
}

pub(super) struct ConlluExport {
    pretokenized: bool,
    previous_document: Option<u64>,
    pending: Option<PendingSentence>,
}

struct PendingSentence {
    bytes: Vec<u8>,
    comment_bytes: usize,
    row_count: usize,
}

impl ConlluExport {
    pub(super) const fn new(pretokenized: bool) -> Self {
        Self {
            pretokenized,
            previous_document: None,
            pending: None,
        }
    }

    pub(super) fn sentence(
        &mut self,
        writer: &mut impl Write,
        sentence: &ExecutionSentence<BoundSentenceInput>,
        keys: &[AttributeKey],
    ) -> Result<(), ExecutionError> {
        let location = &sentence.location;
        let words = &sentence.value;
        let metadata = location.source_metadata.as_deref();
        let metadata_value = |owner: &str, key: &str| metadata?.get(owner)?.get(key)?.as_str();
        let format_error = |message: &str| ExecutionError::Source {
            ordinal: location.corpus_sentence_ordinal,
            message: format!("{}: {message}", location.source_name),
        };
        let mut output = Vec::new();
        if self.previous_document != Some(location.document_ordinal) {
            if let Some(id) = metadata_value("document", "newdoc_id") {
                comment(&mut output, "newdoc id", id).map_err(format_error)?;
            } else if self.previous_document.is_some() {
                output.extend_from_slice(b"# newdoc\n");
            }
            self.previous_document = Some(location.document_ordinal);
        }
        if let Some(id) = metadata_value("sentence", "sent_id") {
            comment(&mut output, "sent_id", id).map_err(format_error)?;
        } else if !self.pretokenized {
            comment(
                &mut output,
                "sent_id",
                &format!("lingrep-{}", location.corpus_sentence_ordinal),
            )
            .map_err(format_error)?;
        }

        let forms = (0..words.len())
            .map(|word| {
                scalar(words, word, "surface")
                    .filter(|form| !form.is_empty())
                    .ok_or_else(|| {
                        format_error("CoNLL-U export requires the exact surface of every word")
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;
        if forms.is_empty() {
            return Err(format_error(
                "CoNLL-U export requires at least one word per sentence",
            ));
        }
        let offsets = (0..forms.len())
            .map(|word| words.char_span(word))
            .collect::<Option<Vec<_>>>();
        // The words of a multiword token share its span, and their forms do not
        // spell its text.
        let tiled = offsets
            .as_ref()
            .filter(|offsets| offsets.windows(2).all(|pair| pair[0].end <= pair[1].start));
        if let Some(offsets) = tiled {
            let mut text = String::new();
            let mut end = 0;
            for (form, span) in forms.iter().zip(offsets) {
                if span.start > end {
                    text.push(' ');
                }
                text.push_str(form);
                end = span.end;
            }
            comment(&mut output, "text", &text).map_err(format_error)?;
        }
        let comment_bytes = output.len();
        let mut spans = if self.pretokenized {
            Vec::new()
        } else {
            span_misc(words)
        };
        let graph = match words.enhanced_graph() {
            EnhancedGraphInput::Observation(observation) => observation.graph(),
            EnhancedGraphInput::NotRequested => None,
        };
        let deps = graph.map(graph_dependencies).unwrap_or_default();
        let mut empty = graph
            .into_iter()
            .flat_map(EnhancedGraph::empty_nodes)
            .peekable();
        for boundary in 0..=words.len() {
            while let Some(node) = empty.next_if(|node| {
                matches!(node.id(), EnhancedNodeId::Empty { boundary: at, .. } if at == boundary)
            }) {
                let id = node.id();
                write_row(&mut output, [
                    &id.spelling(),
                    node.form().unwrap_or("_"),
                    node.lemma().unwrap_or("_"),
                    node.upos().unwrap_or("_"),
                    node.xpos().unwrap_or("_"),
                    &pipe_field(node.raw_features()),
                    "_",
                    "_",
                    &pipe_field(deps.get(&id).into_iter().flatten().map(String::as_str)),
                    &pipe_field(node.misc()),
                ]).map_err(format_error)?;
            }
            let Some(form) = forms.get(boundary) else {
                break;
            };
            let word = boundary;
            let feats = if self.pretokenized {
                pipe_field(set(words, word, "conllu.feats"))
            } else {
                pipe_field(keys.iter().filter_map(|key| {
                    let feature = key.as_str().strip_prefix("feat.")?;
                    let values = strings(words.value_key(word, key)).join(",");
                    (!values.is_empty()).then(|| format!("{feature}={values}"))
                }))
            };
            let dependencies = if graph.is_some() {
                pipe_field(
                    deps.get(&EnhancedNodeId::Word { token_index: word })
                        .into_iter()
                        .flatten()
                        .map(String::as_str),
                )
            } else {
                pipe_field(set(words, word, "dep.enhanced"))
            };
            let misc = if self.pretokenized {
                pipe_field(set(words, word, "conllu.misc"))
            } else {
                let mut fields = std::mem::take(&mut spans[word]);
                if offsets.as_ref().is_some_and(|offsets| {
                    offsets
                        .get(word + 1)
                        .is_some_and(|next| offsets[word].end == next.start)
                }) {
                    fields.push("SpaceAfter=No".into());
                }
                fields.extend(keys.iter().filter_map(|key| {
                    let name = key.as_str();
                    if !name.contains('.') && name != "norm"
                        || name.starts_with("feat.")
                        || name.starts_with("dep.")
                        || name.starts_with("conllu.")
                    {
                        return None;
                    }
                    let values = strings(words.value_key(word, key));
                    (!values.is_empty()).then(|| {
                        let values = values
                            .into_iter()
                            .map(misc_escape)
                            .collect::<Vec<_>>()
                            .join(",");
                        format!("{name}={values}")
                    })
                }));
                fields.sort_unstable();
                pipe_field(fields)
            };
            write_row(
                &mut output,
                [
                    &(word + 1).to_string(),
                    form,
                    scalar(words, word, "lemma").unwrap_or("_"),
                    scalar(words, word, "upos").unwrap_or("_"),
                    scalar(words, word, "xpos").unwrap_or("_"),
                    &feats,
                    scalar(words, word, "dep.head").unwrap_or("_"),
                    scalar(words, word, "dep.rel").unwrap_or("_"),
                    &dependencies,
                    &misc,
                ],
            )
            .map_err(format_error)?;
        }
        output.push(b'\n');
        if let Some(previous) = self.pending.replace(PendingSentence {
            bytes: output,
            comment_bytes,
            row_count: words.len() + graph.map_or(0, |graph| graph.empty_nodes().len()),
        }) {
            writer
                .write_all(&previous.bytes)
                .map_err(execution_output_io)?;
        }
        Ok(())
    }

    pub(super) fn finish(
        &self,
        writer: &mut impl Write,
        termination: Termination,
    ) -> Result<(), CliError> {
        if let Some(pending) = &self.pending {
            let state = match termination {
                Termination::Eof => "complete",
                Termination::ResultLimit => "prefix",
            };
            let (comments, rows) = pending.bytes.split_at(pending.comment_bytes);
            writer.write_all(comments).map_err(CliError::stdout)?;
            writeln!(writer, "# lingrep_export = {state} {}", pending.row_count)
                .map_err(CliError::stdout)?;
            writer.write_all(rows).map_err(CliError::stdout)?;
        }
        writer.flush().map_err(CliError::stdout)
    }
}

fn comment(writer: &mut Vec<u8>, name: &str, value: &str) -> Result<(), &'static str> {
    if value.contains(['\r', '\n']) {
        return Err("CoNLL-U comments cannot contain line breaks");
    }
    writer.extend_from_slice(format!("# {name} = {value}\n").as_bytes());
    Ok(())
}

fn write_row(writer: &mut Vec<u8>, fields: [&str; 10]) -> Result<(), &'static str> {
    for (index, field) in fields.into_iter().enumerate() {
        if field.contains(['\t', '\r', '\n']) {
            return Err("CoNLL-U fields cannot contain tabs or line breaks");
        }
        if index != 0 {
            writer.push(b'\t');
        }
        writer.extend_from_slice(if field.is_empty() {
            b"_"
        } else {
            field.as_bytes()
        });
    }
    writer.push(b'\n');
    Ok(())
}

fn pipe_field(values: impl IntoIterator<Item = impl AsRef<str>>) -> String {
    let mut output = String::new();
    for value in values {
        if !output.is_empty() {
            output.push('|');
        }
        output.push_str(value.as_ref());
    }
    if output.is_empty() {
        "_".into()
    } else {
        output
    }
}

fn scalar<'a>(words: &'a BoundSentenceInput, word: usize, key: &str) -> Option<&'a str> {
    match words.value_key(word, &AttributeKey::new(key).expect("valid export key"))? {
        AttributeValueRef::Scalar(value) => Some(value),
        AttributeValueRef::StringSet(_) => None,
    }
}

fn set<'a>(words: &'a BoundSentenceInput, word: usize, key: &str) -> Vec<&'a str> {
    strings(words.value_key(word, &AttributeKey::new(key).expect("valid export key")))
}

fn strings(value: Option<AttributeValueRef<'_>>) -> Vec<&str> {
    match value {
        Some(AttributeValueRef::Scalar(value)) => vec![value],
        Some(AttributeValueRef::StringSet(values)) => values.iter().map(AsRef::as_ref).collect(),
        None => Vec::new(),
    }
}

fn graph_dependencies(graph: &EnhancedGraph) -> BTreeMap<EnhancedNodeId, Vec<String>> {
    let mut deps = BTreeMap::<_, Vec<_>>::new();
    for edge in graph.edges() {
        deps.entry(edge.dependent()).or_default().push(format!(
            "{}:{}",
            edge.head().spelling(),
            edge.relation()
        ));
    }
    deps
}

fn span_misc(words: &BoundSentenceInput) -> Vec<Vec<String>> {
    let mut fields = vec![Vec::new(); words.len()];
    for span in words.spans() {
        let key = match span.kind.as_str() {
            "bunsetsu" => "BunsetuBILabel",
            "luw" => "LUWBILabel",
            _ => continue,
        };
        fields[span.start].push(format!("{key}=B"));
        for row in &mut fields[span.start + 1..span.end] {
            row.push(format!("{key}=I"));
        }
        if span.kind == "bunsetsu" {
            if let Some(head) = span.head {
                let marker = if scalar(words, head, "dep.head") == Some("0") {
                    "ROOT"
                } else {
                    "SEM_HEAD"
                };
                fields[head].push(format!("BunsetuPositionType={marker}"));
            }
        } else if let Some(label) = &span.label {
            fields[span.start].push(format!("LUWPOS={}", misc_escape(label)));
        }
    }
    fields
}

fn misc_escape(value: &str) -> String {
    let mut escaped = String::new();
    for ch in value.chars() {
        match ch {
            '%' => escaped.push_str("%25"),
            ',' => escaped.push_str("%2C"),
            '=' => escaped.push_str("%3D"),
            '|' => escaped.push_str("%7C"),
            '\t' => escaped.push_str("%09"),
            '\r' => escaped.push_str("%0D"),
            '\n' => escaped.push_str("%0A"),
            _ => escaped.push(ch),
        }
    }
    escaped
}
