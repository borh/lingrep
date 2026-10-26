use std::collections::BTreeMap;
use std::io;
use std::path::Path;

use lingrep_core::execution::{ExecutionError, SentenceLocation};
use lingrep_core::fsm::Match;
use lingrep_core::token_match::{AttributeKey, TokenSequence};
use serde::Serialize;

use crate::protocol::query::SourceSpan;

const MAX_BYTES: usize = 32 * 1024 * 1024;
const MAX_GROUPS: usize = 4096;
const EXAMPLES_PER_GROUP: usize = 3;

#[derive(Default, Serialize)]
pub(super) struct EndingReport {
    pub sentences: u64,
    pub documents: u64,
    pub unexplained_sentences: u64,
    #[serde(skip)]
    last_document: Option<u64>,
    #[serde(skip)]
    bytes: usize,
    #[serde(skip)]
    groups: BTreeMap<(String, Vec<String>), Group>,
}

#[derive(Serialize)]
struct Group {
    kind: String,
    tokens: Vec<String>,
    occurrences: u64,
    documents: u64,
    #[serde(skip)]
    last_document: Option<u64>,
    examples: Vec<Example>,
}

#[derive(Serialize)]
struct Example {
    source: String,
    document_ordinal: u64,
    sentence_ordinal: u64,
    tokens: Vec<String>,
    span: SourceSpan,
    preceding: Vec<Preceding>,
}

#[derive(Serialize)]
struct Preceding {
    pattern: String,
    span: SourceSpan,
}

impl EndingReport {
    pub(super) fn observe(
        &mut self,
        location: &SentenceLocation,
        sentence: &dyn TokenSequence,
        matches: &[Match],
    ) -> Result<(), ExecutionError> {
        let error = || {
            ExecutionError::Sink(
                "ending report exceeds 4096 groups or 32 MiB; narrow the query scope".into(),
            )
        };
        if self
            .last_document
            .is_some_and(|previous| location.document_ordinal < previous)
        {
            return Err(ExecutionError::Sink(
                "ending report requires document order".into(),
            ));
        }
        self.sentences += 1;
        if self.last_document != Some(location.document_ordinal) {
            self.documents += 1;
            self.last_document = Some(location.document_ordinal);
        }
        let upos = AttributeKey::new("upos").expect("canonical key");
        let text = AttributeKey::new("text").expect("canonical key");
        let mut end = sentence.len();
        while end > 0 && sentence.scalar_key(end - 1, &upos) == Some("PUNCT") {
            end -= 1;
        }
        if end == 0 || matches.iter().any(|m| m.start < end && m.end >= end) {
            return Ok(());
        }
        let start = matches
            .iter()
            .filter(|m| m.end <= end)
            .map(|m| m.end)
            .max()
            .unwrap_or(0);
        let tokens = (0..sentence.len())
            .map(|index| {
                sentence
                    .scalar_key(index, &text)
                    .map(str::to_owned)
                    .ok_or_else(|| {
                        ExecutionError::Sink("ending report requires text on every token".into())
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let kind = if matches.is_empty() {
            "no_published_match"
        } else if start == 0 {
            "no_preceding_match"
        } else {
            "trailing_after_match"
        };
        let key = (kind.to_owned(), tokens[start..end].to_vec());
        let new_group = !self.groups.contains_key(&key);
        let retain_example = self
            .groups
            .get(&key)
            .is_none_or(|group| group.examples.len() < EXAMPLES_PER_GROUP);
        let preceding = if retain_example {
            matches
                .iter()
                .filter(|m| m.end == start)
                .map(|m| Preceding {
                    pattern: m.pattern.to_string(),
                    span: SourceSpan {
                        start: m.start as u64,
                        end: m.end as u64,
                    },
                })
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        let string_bytes = |values: &[String]| {
            values
                .iter()
                .map(|value| value.len() + std::mem::size_of::<String>())
                .sum::<usize>()
        };
        let added = if new_group {
            512 + 2 * string_bytes(&key.1)
        } else {
            0
        } + if retain_example {
            512 + string_bytes(&tokens)
                + location.source_name.len()
                + preceding
                    .iter()
                    .map(|p| p.pattern.len() + std::mem::size_of::<Preceding>())
                    .sum::<usize>()
        } else {
            0
        };
        if (new_group && self.groups.len() == MAX_GROUPS)
            || self.bytes.saturating_add(added) > MAX_BYTES
        {
            return Err(error());
        }
        self.bytes += added;
        self.unexplained_sentences += 1;
        let group = self.groups.entry(key.clone()).or_insert_with(|| Group {
            kind: key.0,
            tokens: key.1,
            occurrences: 0,
            documents: 0,
            last_document: None,
            examples: Vec::new(),
        });
        group.occurrences += 1;
        if group.last_document != Some(location.document_ordinal) {
            group.documents += 1;
            group.last_document = Some(location.document_ordinal);
        }
        if retain_example {
            group.examples.push(Example {
                source: location.source_name.to_string(),
                document_ordinal: location.document_ordinal,
                sentence_ordinal: location.document_sentence_ordinal,
                tokens,
                span: SourceSpan {
                    start: start as u64,
                    end: end as u64,
                },
                preceding,
            });
        }
        Ok(())
    }

    pub(super) fn write(
        &self,
        path: &Path,
        run: &crate::protocol::query::QueryEnvelope,
        corpus: &crate::protocol::query::CompleteCorpus,
        scope: Option<&crate::protocol::query::ScopeReport>,
    ) -> io::Result<()> {
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let mut file = tempfile::NamedTempFile::new_in(parent)?;
        #[derive(Serialize)]
        struct Report<'a> {
            version: u32,
            basis: &'static str,
            punctuation: &'static str,
            examples_per_group: usize,
            run: &'a crate::protocol::query::QueryEnvelope,
            corpus: &'a crate::protocol::query::CompleteCorpus,
            scope: Option<&'a crate::protocol::query::ScopeReport>,
            #[serde(flatten)]
            totals: &'a EndingReport,
            groups: Vec<&'a Group>,
        }
        serde_json::to_writer_pretty(
            &mut file,
            &Report {
                version: 1,
                basis: "Unexplained by this published run; policy-suppressed matches are not evidence of catalog absence. Counts cover all sentences delivered in the selected query scope; groups use exact token surfaces. Examples retain token sequences, not original spacing.",
                punctuation: "Ignore trailing tokens with upos=PUNCT; other tokens are substantive. An unavailable upos attribute excludes no tokens.",
                examples_per_group: EXAMPLES_PER_GROUP,
                run,
                corpus,
                scope,
                totals: self,
                groups: self.groups.values().collect(),
            },
        )?;
        file.persist_noclobber(path).map_err(|error| error.error)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lingrep_core::token_match::TokenInfo;

    fn location(document: u64) -> SentenceLocation {
        SentenceLocation {
            corpus_sentence_ordinal: document,
            document_id: lingrep_core::corpus::DocumentId::new(document),
            document_ordinal: document,
            document_sentence_ordinal: 0,
            source_name: "fixture".into(),
            source_metadata: None,
        }
    }

    #[test]
    fn counts_are_exact_across_documents_and_examples_are_bounded() {
        let sentence = vec![TokenInfo::from("未知".to_owned())];
        let mut report = EndingReport::default();
        for document in [0, 0, 0, 0, 1] {
            report
                .observe(&location(document), &sentence, &[])
                .expect("observed");
        }
        let group = report.groups.values().next().expect("one group");
        assert_eq!(
            (group.occurrences, group.documents, group.examples.len()),
            (5, 2, 3)
        );
        assert_eq!((report.sentences, report.documents), (5, 2));
        report.bytes = MAX_BYTES;
        let another = vec![TokenInfo::from("別".to_owned())];
        assert!(report.observe(&location(2), &another, &[]).is_err());
        let mut punctuation = TokenInfo::from("。".to_owned());
        punctuation.set(
            lingrep_core::token_match::TokenAttributeIndex::Pos,
            "PUNCT".into(),
        );
        let sentence = vec![TokenInfo::from("未知".to_owned()), punctuation];
        let matched = Match {
            pattern: "punctuation".into(),
            start: 1,
            end: 2,
            captures: Box::new([]),
            graph_bindings: Box::new([]),
            lexical: None,
        };
        let mut report = EndingReport::default();
        report
            .observe(&location(0), &sentence, &[matched])
            .expect("observed");
        assert_eq!(
            report.groups.values().next().expect("group").kind,
            "no_preceding_match"
        );
    }
}
