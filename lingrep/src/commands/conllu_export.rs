use std::collections::BTreeMap;
use std::io::Write;

use lingrep_core::corpus::SourceMetadataProjection;
use lingrep_core::corpus::conllu::{EnhancedGraph, EnhancedNodeId};
use lingrep_core::execution::{
    BoundSentenceInput, EnhancedGraphInput, ExecutionError, ExecutionSentence,
};
use lingrep_core::token_match::{
    AttributeKey, AttributeOrdinal, AttributeValueRef, BoundTokenSentence, TokenSequence,
};

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
        let tokens = words.words();
        let column =
            |name: &str| tokens.ordinal(&AttributeKey::new(name).expect("valid export key"));
        let surface = column("surface");
        let dep_head = column("dep.head");
        let lemma = column("lemma");
        let upos = column("upos");
        let xpos = column("xpos");
        let dep_rel = column("dep.rel");
        let conllu_feats = column("conllu.feats");
        let conllu_misc = column("conllu.misc");
        let dep_enhanced = column("dep.enhanced");
        let feat_columns = keys
            .iter()
            .filter_map(|key| Some((key.as_str().strip_prefix("feat.")?, tokens.ordinal(key))))
            .collect::<Vec<_>>();
        let misc_columns = keys
            .iter()
            .filter(|key| {
                let name = key.as_str();
                (name.contains('.') || name == "norm")
                    && !name.starts_with("feat.")
                    && !name.starts_with("dep.")
                    && !name.starts_with("conllu.")
            })
            .map(|key| (key.as_str(), tokens.ordinal(key)))
            .collect::<Vec<_>>();
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
                scalar(tokens, word, surface)
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
            span_misc(tokens, dep_head)
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
                pipe_field(strings(value(tokens, word, conllu_feats)))
            } else {
                let mut feats = String::new();
                for (feature, ordinal) in &feat_columns {
                    let entry = feats.len();
                    if entry != 0 {
                        feats.push('|');
                    }
                    feats.push_str(feature);
                    feats.push('=');
                    let values = feats.len();
                    push_values(&mut feats, value(tokens, word, *ordinal), false);
                    if feats.len() == values {
                        feats.truncate(entry);
                    }
                }
                if feats.is_empty() { "_".into() } else { feats }
            };
            let dependencies = if graph.is_some() {
                pipe_field(
                    deps.get(&EnhancedNodeId::Word { token_index: word })
                        .into_iter()
                        .flatten()
                        .map(String::as_str),
                )
            } else {
                pipe_field(strings(value(tokens, word, dep_enhanced)))
            };
            let misc = if self.pretokenized {
                pipe_field(strings(value(tokens, word, conllu_misc)))
            } else {
                let mut fields = std::mem::take(&mut spans[word]);
                if offsets.as_ref().is_some_and(|offsets| {
                    offsets
                        .get(word + 1)
                        .is_some_and(|next| offsets[word].end == next.start)
                }) {
                    fields.push("SpaceAfter=No".into());
                }
                fields.extend(misc_columns.iter().filter_map(|(name, ordinal)| {
                    let mut entry = format!("{name}=");
                    (push_values(&mut entry, value(tokens, word, *ordinal), true) != 0)
                        .then_some(entry)
                }));
                fields.sort_unstable();
                pipe_field(fields)
            };
            write_row(
                &mut output,
                [
                    &(word + 1).to_string(),
                    form,
                    scalar(tokens, word, lemma).unwrap_or("_"),
                    scalar(tokens, word, upos).unwrap_or("_"),
                    scalar(tokens, word, xpos).unwrap_or("_"),
                    &feats,
                    scalar(tokens, word, dep_head).unwrap_or("_"),
                    scalar(tokens, word, dep_rel).unwrap_or("_"),
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

fn value(
    words: &BoundTokenSentence,
    word: usize,
    ordinal: Option<AttributeOrdinal>,
) -> Option<AttributeValueRef<'_>> {
    words.token(word)?.value(ordinal?)
}

fn scalar(
    words: &BoundTokenSentence,
    word: usize,
    ordinal: Option<AttributeOrdinal>,
) -> Option<&str> {
    match value(words, word, ordinal)? {
        AttributeValueRef::Scalar(value) => Some(value),
        AttributeValueRef::StringSet(_) => None,
    }
}

/// Appends the values of `value` separated by commas, MISC-escaped when
/// `escape` is set, and returns how many there were.
fn push_values(output: &mut String, value: Option<AttributeValueRef<'_>>, escape: bool) -> usize {
    let mut count = 0;
    let mut push = |value: &str| {
        if count != 0 {
            output.push(',');
        }
        if escape {
            misc_escape(output, value);
        } else {
            output.push_str(value);
        }
        count += 1;
    };
    match value {
        Some(AttributeValueRef::Scalar(value)) => push(value),
        Some(AttributeValueRef::StringSet(values)) => values.iter().for_each(|value| push(value)),
        None => {}
    }
    count
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

fn span_misc(words: &BoundTokenSentence, dep_head: Option<AttributeOrdinal>) -> Vec<Vec<String>> {
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
                let marker = if scalar(words, head, dep_head) == Some("0") {
                    "ROOT"
                } else {
                    "SEM_HEAD"
                };
                fields[head].push(format!("BunsetuPositionType={marker}"));
            }
        } else if let Some(label) = &span.label {
            let mut entry = "LUWPOS=".to_owned();
            misc_escape(&mut entry, label);
            fields[span.start].push(entry);
        }
    }
    fields
}

fn misc_escape(escaped: &mut String, value: &str) {
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
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use lingrep_core::corpus::DocumentId;
    use lingrep_core::execution::SentenceLocation;
    use lingrep_core::token_match::{
        AttributeKind, BoundTokenSentence, CharSpan, ProjectedAttributeValue, SchemaAttribute,
        SentenceSpan, TokenSchema,
    };

    use super::*;

    /// Two words of an analyzer schema whose values exercise every FEATS and
    /// MISC branch of a non-pretokenized export.
    fn analyzed_sentence(keys: &mut Vec<AttributeKey>) -> ExecutionSentence<BoundSentenceInput> {
        let schema = TokenSchema::new([
            SchemaAttribute::new("text", AttributeKind::Scalar).unwrap(),
            SchemaAttribute::new("surface", AttributeKind::Scalar).unwrap(),
            SchemaAttribute::new("lemma", AttributeKind::Scalar).unwrap(),
            SchemaAttribute::new("norm", AttributeKind::Scalar).unwrap(),
            SchemaAttribute::new("upos", AttributeKind::Scalar).unwrap(),
            SchemaAttribute::new("feat.Case", AttributeKind::StringSet).unwrap(),
            SchemaAttribute::new("feat.Number", AttributeKind::StringSet).unwrap(),
            SchemaAttribute::new("unidic.goshu", AttributeKind::Scalar).unwrap(),
            SchemaAttribute::new("unidic.pron", AttributeKind::StringSet).unwrap(),
        ])
        .unwrap();
        let projection = schema
            .bind_projection(schema.keys().iter().cloned())
            .unwrap();
        keys.extend(schema.keys().iter().cloned());
        keys.sort();
        let scalar = ProjectedAttributeValue::scalar;
        let set = |values: &[&str]| ProjectedAttributeValue::string_set(values.iter().copied());
        let first = BTreeMap::from([
            ("text", scalar("猫")),
            ("surface", scalar("猫")),
            ("lemma", scalar("猫")),
            ("norm", scalar("ネコ")),
            ("upos", scalar("NOUN")),
            ("feat.Case", set(&["Nom", "Acc"])),
            ("feat.Number", set(&[])),
            ("unidic.goshu", scalar("和,=|%")),
            ("unidic.pron", set(&["ネ", "コ"])),
        ]);
        let second = BTreeMap::from([
            ("text", scalar("だ")),
            ("surface", scalar("だ")),
            ("lemma", ProjectedAttributeValue::missing_scalar()),
            ("norm", scalar("")),
            ("upos", scalar("AUX")),
            ("feat.Case", set(&[])),
            ("feat.Number", set(&["Sing"])),
            ("unidic.goshu", scalar("")),
            ("unidic.pron", set(&[])),
        ]);
        let words = BoundTokenSentence::from_projected_attribute_values(
            &projection,
            2,
            [&first, &second]
                .into_iter()
                .flat_map(|row| {
                    projection
                        .keys()
                        .iter()
                        .map(|key| row[key.as_str()].clone())
                })
                .collect(),
        )
        .unwrap()
        .with_char_offsets(Some(Arc::new(vec![
            CharSpan::new(0, 1),
            CharSpan::new(1, 2),
        ])))
        .with_spans(vec![
            SentenceSpan {
                kind: "bunsetsu".into(),
                start: 0,
                end: 2,
                head: Some(0),
                label: None,
                governor: None,
            },
            SentenceSpan {
                kind: "luw".into(),
                start: 1,
                end: 2,
                head: None,
                label: Some("助動詞,=".into()),
                governor: None,
            },
        ])
        .unwrap();
        ExecutionSentence {
            location: SentenceLocation {
                corpus_sentence_ordinal: 7,
                document_id: DocumentId::new(0),
                document_ordinal: 0,
                document_sentence_ordinal: 0,
                source_name: Arc::from("fixture"),
                source_metadata: None,
                lookaround_divisions: None,
            },
            value: BoundSentenceInput::new(words, EnhancedGraphInput::NotRequested),
        }
    }

    #[test]
    fn analyzed_rows_pin_feats_misc_escaping_spans_and_spacing() {
        let mut keys = Vec::new();
        let sentence = analyzed_sentence(&mut keys);
        let mut export = ConlluExport::new(false);
        let mut output = Vec::new();
        export.sentence(&mut output, &sentence, &keys).unwrap();
        export.finish(&mut output, Termination::Eof).unwrap();
        assert_eq!(
            String::from_utf8(output).unwrap(),
            "# sent_id = lingrep-7\n\
             # text = 猫だ\n\
             # lingrep_export = complete 2\n\
             1\t猫\t猫\tNOUN\t_\tCase=Acc,Nom\t_\t_\t_\t\
             BunsetuBILabel=B|BunsetuPositionType=SEM_HEAD|SpaceAfter=No|norm=ネコ|\
             unidic.goshu=和%2C%3D%7C%25|unidic.pron=コ,ネ\n\
             2\tだ\t_\tAUX\t_\tNumber=Sing\t_\t_\t_\t\
             BunsetuBILabel=I|LUWBILabel=B|LUWPOS=助動詞%2C%3D|norm=|unidic.goshu=\n\n"
        );
    }
}
