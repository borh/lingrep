//! Authored divisions and the bounded structural context of one sentence.

use std::collections::BTreeMap;
use std::io;
use std::sync::Arc;

use super::{DocumentId, Metadata, metadata_from_json};

pub const MAX_DIVISION_METADATA_BYTES: usize = 1024 * 1024;
pub const MAX_ACTIVE_DIVISIONS: usize = 256;
pub const MAX_DIVISION_DEPTH: usize = 64;
pub const MAX_STRUCTURAL_CONTEXT_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_DIVISION_KINDS: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DivisionId(pub u64);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Division {
    pub source_id: Option<String>,
    pub id: DivisionId,
    pub document_id: DocumentId,
    pub kind: String,
    pub parent_id: Option<DivisionId>,
    pub metadata: Metadata,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum DivisionObservation {
    Observed,
    Unavailable { reason: String },
}

impl DivisionObservation {
    pub fn validate(&self) -> io::Result<()> {
        if let Self::Unavailable { reason } = self
            && (reason.is_empty() || reason.len() > 1024)
        {
            return Err(invalid(
                "structural unavailability reason must contain 1–1024 bytes",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StructuralContext {
    pub observations: BTreeMap<String, DivisionObservation>,
    pub divisions: Box<[Arc<Division>]>,
}

impl StructuralContext {
    pub fn observes(&self, kind: &str) -> bool {
        matches!(
            self.observations.get(kind),
            Some(DivisionObservation::Observed)
        )
    }
}

pub fn validate_kind(kind: &str) -> io::Result<()> {
    if kind.is_empty()
        || kind.len() > 256
        || !kind
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '_' | '-'))
    {
        return Err(invalid(
            "division kind must contain 1–256 bytes of letters, numbers, underscores or hyphens",
        ));
    }
    Ok(())
}

pub fn metadata_json(metadata: &Metadata) -> serde_json::Value {
    serde_json::Value::Object(
        metadata
            .iter()
            .map(|(key, value)| (key.clone(), value.to_json()))
            .collect(),
    )
}

pub fn validate_metadata(metadata: &Metadata) -> io::Result<usize> {
    let value = metadata_json(metadata);
    let bytes = serde_json::to_vec(&value).map_err(io::Error::other)?.len();
    if bytes > MAX_DIVISION_METADATA_BYTES {
        return Err(invalid("division metadata exceeds 1 MiB"));
    }
    metadata_from_json(value, "division metadata")?;
    Ok(bytes)
}

pub(crate) fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

pub(crate) fn check_metadata_size(value: &serde_json::Value) -> io::Result<()> {
    struct Budget(usize);
    impl io::Write for Budget {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0 = self
                .0
                .checked_add(bytes.len())
                .ok_or_else(|| invalid("metadata byte count overflow"))?;
            if self.0 > MAX_DIVISION_METADATA_BYTES {
                return Err(invalid("metadata record exceeds 1 MiB"));
            }
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    serde_json::to_writer(Budget(0), value).map_err(io::Error::other)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::corpus::{AdmittedCorpus, AdmittedCorpusEvent, CorpusEvent};
    use crate::scope::{ExclusionReason, NoLabels, ScopeClause, Verdict};

    fn division(id: u64, role: &str, reviewer: Option<&str>) -> Arc<Division> {
        let mut metadata = serde_json::json!({"role":role});
        if let Some(reviewer) = reviewer {
            metadata["reviewer"] = reviewer.into();
        }
        Arc::new(Division {
            source_id: None,
            id: DivisionId(id),
            document_id: DocumentId::new(0),
            kind: "section".into(),
            parent_id: None,
            metadata: metadata_from_json(metadata, "test").unwrap(),
        })
    }

    #[test]
    fn a_compound_structural_predicate_requires_one_complete_witness() {
        let context = StructuralContext {
            observations: BTreeMap::from([("section".into(), DivisionObservation::Observed)]),
            divisions: Box::new([
                division(0, "methods", Some("B")),
                division(1, "results", Some("A")),
            ]),
        };
        for (source, expected) in [
            (
                "within(section, role=methods & reviewer=A)",
                Verdict::Excluded(ExclusionReason::Predicate),
            ),
            (
                "within(section, role=methods) & within(section, reviewer=A)",
                Verdict::Admitted,
            ),
            (
                "not within(section, role=methods & reviewer=A)",
                Verdict::Admitted,
            ),
        ] {
            let clause = ScopeClause::parse(source).unwrap();
            assert_eq!(
                clause.evaluate_sentence(
                    &Metadata::default(),
                    &Metadata::default(),
                    &NoLabels,
                    &context
                ),
                expected,
                "{source}"
            );
        }
    }

    #[test]
    fn missing_values_never_prove_structural_absence() {
        let context = StructuralContext {
            observations: BTreeMap::from([("section".into(), DivisionObservation::Observed)]),
            divisions: Box::new([division(0, "methods", None)]),
        };
        let clause = ScopeClause::parse("not within(section, reviewer=A)").unwrap();
        assert_eq!(
            clause.evaluate_sentence(
                &Metadata::default(),
                &Metadata::default(),
                &NoLabels,
                &context
            ),
            Verdict::Excluded(ExclusionReason::MissingField)
        );
        assert!(
            clause
                .validate_structure(&StructuralContext::default())
                .is_err()
        );
        let empty = StructuralContext {
            divisions: Box::new([]),
            ..context
        };
        assert_eq!(
            clause.evaluate_sentence(
                &Metadata::default(),
                &Metadata::default(),
                &NoLabels,
                &empty
            ),
            Verdict::Admitted
        );
    }

    #[test]
    fn structural_negation_respects_known_boolean_evidence() {
        let context = StructuralContext {
            observations: BTreeMap::from([("section".into(), DivisionObservation::Observed)]),
            divisions: Box::new([division(0, "methods", None)]),
        };
        for (source, expected) in [
            (
                "not within(section, role=results & reviewer=A)",
                Verdict::Admitted,
            ),
            (
                "within(section, (role=methods | reviewer=A))",
                Verdict::Admitted,
            ),
            (
                "within(section, role=methods & reviewer=A)",
                Verdict::Excluded(ExclusionReason::MissingField),
            ),
            (
                "not within(section, (role=results | reviewer=A))",
                Verdict::Excluded(ExclusionReason::MissingField),
            ),
            (
                "within(section, role!=methods)",
                Verdict::Excluded(ExclusionReason::Predicate),
            ),
        ] {
            assert_eq!(
                ScopeClause::parse(source).unwrap().evaluate_sentence(
                    &Metadata::default(),
                    &Metadata::default(),
                    &NoLabels,
                    &context
                ),
                expected,
                "{source}"
            );
        }
    }

    #[test]
    fn active_metadata_bytes_are_bounded_independently_of_division_count() {
        let metadata = (0..15)
            .map(|key| {
                (
                    format!("field-{key}"),
                    serde_json::Value::String("x".repeat(60000)),
                )
            })
            .collect::<serde_json::Map<_, _>>();
        let metadata = metadata_from_json(metadata.into(), "budget").unwrap();
        let mut events = vec![CorpusEvent::DocumentStart {
            source_name: "budget".into(),
            metadata: Metadata::default(),
        }];
        for ordinal in 0..19 {
            events.push(CorpusEvent::DivisionStart {
                source_ordinal: ordinal,
                source_id: None,
                kind: "part".into(),
                parent_source_ordinal: None,
                metadata: metadata.clone(),
            });
        }
        let error = AdmittedCorpus::new(events.into_iter().map(Ok))
            .collect::<io::Result<Vec<_>>>()
            .unwrap_err();
        assert!(error.to_string().contains("byte budget"), "{error}");
    }

    #[test]
    fn crossing_and_adjacent_equal_divisions_keep_distinct_identities() {
        let mut events = vec![CorpusEvent::DocumentStart {
            source_name: "test".into(),
            metadata: Metadata::default(),
        }];
        for event in [
            CorpusEvent::DivisionStart {
                source_id: None,
                source_ordinal: 0,
                kind: "section".into(),
                parent_source_ordinal: None,
                metadata: Metadata::default(),
            },
            sentence("one"),
            CorpusEvent::DivisionStart {
                source_id: None,
                source_ordinal: 1,
                kind: "region".into(),
                parent_source_ordinal: None,
                metadata: Metadata::default(),
            },
            sentence("two"),
            CorpusEvent::DivisionEnd { source_ordinal: 0 },
            CorpusEvent::DivisionStart {
                source_id: None,
                source_ordinal: 2,
                kind: "section".into(),
                parent_source_ordinal: None,
                metadata: Metadata::default(),
            },
            sentence("three"),
            CorpusEvent::DivisionEnd { source_ordinal: 1 },
            CorpusEvent::DivisionEnd { source_ordinal: 2 },
            CorpusEvent::DocumentEnd,
        ] {
            events.push(event);
        }
        let sentences = AdmittedCorpus::new(events.into_iter().map(Ok))
            .map(Result::unwrap)
            .filter_map(|event| match event {
                AdmittedCorpusEvent::Sentence(sentence) => Some(sentence),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            sentences
                .iter()
                .map(|sentence| sentence
                    .structure
                    .divisions
                    .iter()
                    .map(|division| division.id.0)
                    .collect::<Vec<_>>())
                .collect::<Vec<_>>(),
            vec![vec![0], vec![0, 1], vec![1, 2]]
        );
    }

    proptest::proptest! {
        #[test]
        fn membership_matches_half_open_authored_ranges(ranges in proptest::collection::vec((0usize..24,0usize..24),0..48)) {
            let ranges=ranges.into_iter().map(|(a,b)|(a.min(b),a.max(b))).collect::<Vec<_>>();
            let mut events=vec![CorpusEvent::DocumentStart {source_name:"generated".into(),metadata:Metadata::default()}];
            let mut next=0u64;
            let mut assigned=vec![0;ranges.len()];
            for boundary in 0..=24 {
                for (index,(start,_)) in ranges.iter().enumerate().filter(|(_,range)|range.0==boundary) {
                    assigned[index]=next;
                    events.push(CorpusEvent::DivisionStart {source_ordinal:next,source_id:None,kind:"region".into(),parent_source_ordinal:None,metadata:Metadata::default()});
                    next+=1;
                    let _=start;
                }
                for (index,_) in ranges.iter().enumerate().filter(|(_,range)|range.1==boundary) { events.push(CorpusEvent::DivisionEnd {source_ordinal:assigned[index]}); }
                if boundary<24 {events.push(sentence("one"));}
            }
            events.push(CorpusEvent::DocumentEnd);
            let admitted=AdmittedCorpus::new(events.into_iter().map(Ok)).collect::<io::Result<Vec<_>>>().unwrap();
            for (ordinal,sentence) in admitted.iter().filter_map(|event|match event {AdmittedCorpusEvent::Sentence(sentence)=>Some(sentence),_=>None}).enumerate() {
                let mut expected=ranges.iter().enumerate().filter(|(_,range)|range.0<=ordinal && ordinal<range.1).map(|(index,_)|assigned[index]).collect::<Vec<_>>();
                expected.sort_unstable();
                proptest::prop_assert_eq!(sentence.structure.divisions.iter().map(|division|division.id.0).collect::<Vec<_>>(),expected);
            }
        }
    }

    #[test]
    fn admission_refuses_excessive_active_structure_and_parent_depth() {
        for parents in [false, true] {
            let mut events = vec![CorpusEvent::DocumentStart {
                source_name: "budget".into(),
                metadata: Metadata::default(),
            }];
            let count = if parents {
                MAX_DIVISION_DEPTH + 1
            } else {
                MAX_ACTIVE_DIVISIONS + 1
            };
            for ordinal in 0..count as u64 {
                events.push(CorpusEvent::DivisionStart {
                    source_ordinal: ordinal,
                    source_id: None,
                    kind: "part".into(),
                    parent_source_ordinal: if parents {
                        ordinal.checked_sub(1)
                    } else {
                        None
                    },
                    metadata: Metadata::default(),
                });
            }
            let error = AdmittedCorpus::new(events.into_iter().map(Ok))
                .collect::<io::Result<Vec<_>>>()
                .unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains(if parents { "64 levels" } else { "active" }),
                "{error}"
            );
        }
        assert!(
            metadata_from_json(
                serde_json::json!({"large":"x".repeat(MAX_DIVISION_METADATA_BYTES)}),
                "budget"
            )
            .is_err()
        );
    }

    #[test]
    fn derived_label_bytes_are_bounded_at_admission() {
        let mut event = sentence("one");
        if let CorpusEvent::Sentence { active_labels, .. } = &mut event {
            *active_labels = vec!["x".repeat(MAX_STRUCTURAL_CONTEXT_BYTES)].into_boxed_slice();
        }
        let events = vec![
            CorpusEvent::DocumentStart {
                source_name: "labels".into(),
                metadata: Metadata::default(),
            },
            event,
        ];
        let error = AdmittedCorpus::new(events.into_iter().map(Ok))
            .collect::<io::Result<Vec<_>>>()
            .unwrap_err();
        assert!(error.to_string().contains("labels exceed"), "{error}");
    }

    fn sentence(text: &str) -> CorpusEvent {
        CorpusEvent::Sentence {
            text: text.into(),
            words: None,
            enhanced_graph: None,
            active_labels: Box::new([]),
            metadata: Metadata::default(),
        }
    }
}
