use crate::snapshot::{CompletedSnapshot, PatternKey};
use crate::templates::format_count;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocumentShare {
    pub documents: u64,
    pub documents_label: String,
    pub scanned: u64,
    pub scanned_label: String,
    pub top_one_label: String,
    pub top_five_label: String,
}

pub fn document_share(snapshot: &CompletedSnapshot, key: PatternKey) -> Option<DocumentShare> {
    let summary = snapshot.documents.as_ref()?;
    let pattern = snapshot.pattern(key)?;
    let reach = summary.reach(key)?;
    if pattern.exact_total == 0 {
        return None;
    }
    let percent = |count: u64| {
        let tenths = count * 1000 / pattern.exact_total;
        format!("{}.{}", tenths / 10, tenths % 10)
    };
    Some(DocumentShare {
        documents: reach.documents,
        documents_label: format_count(reach.documents),
        scanned: summary.documents_scanned,
        scanned_label: format_count(summary.documents_scanned),
        top_one_label: percent(reach.top_counts.first().copied().unwrap_or(0)),
        top_five_label: percent(reach.top_counts.iter().take(5).sum()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::{DocumentReach, DocumentSummary, PatternDescriptor, PatternOrigin};

    fn snapshot(documents: Option<DocumentSummary>) -> CompletedSnapshot {
        CompletedSnapshot {
            search_scope: Default::default(),
            excluded_sentences: 0,
            revision: crate::revision::RevisionId(1),
            corpus_identity: crate::corpus_runtime::CorpusIdentity::new(""),
            source: "".into(),
            derive_compounds: false,
            compound_category: crate::snapshot::CompoundCategory::Compound,
            patterns: vec![
                PatternDescriptor {
                    key: PatternKey::new(0).unwrap(),
                    name: "ている".into(),
                    exact_total: 1_012,
                    metadata: Box::default(),
                    origin: PatternOrigin::Authored { chained_total: 0 },
                },
                PatternDescriptor {
                    key: PatternKey::new(1).unwrap(),
                    name: "none".into(),
                    exact_total: 0,
                    metadata: Box::default(),
                    origin: PatternOrigin::Authored { chained_total: 0 },
                },
            ]
            .into_boxed_slice(),
            match_refs: Box::default(),
            capture_catalog: Box::default(),
            sections: crate::section_tally::SectionAvailability::Available(
                crate::section_tally::SectionTally::unlabelled(),
            ),
            observed_sentences: 6_570,
            matched_sentences: 3_516,
            documents,
            sentences_matched: 6_570,
            oversized_omissions: None,
        }
    }

    #[test]
    fn shares_are_the_top_documents_over_the_exact_total() {
        let summary = DocumentSummary {
            documents_scanned: 80,
            by_pattern: Box::from([
                DocumentReach {
                    documents: 80,
                    top_counts: Box::from([60, 50, 40, 30, 23]),
                },
                DocumentReach {
                    documents: 0,
                    top_counts: Box::default(),
                },
            ]),
        };
        let share = document_share(&snapshot(Some(summary)), PatternKey::new(0).unwrap()).unwrap();
        assert_eq!((share.documents, share.scanned), (80, 80));
        assert_eq!(share.top_one_label, "5.9");
        assert_eq!(share.top_five_label, "20.0");
        assert!(
            document_share(&snapshot(None), PatternKey::new(0).unwrap()).is_none(),
            "a run that did not count documents"
        );
    }

    #[test]
    fn a_pattern_without_occurrences_has_no_share() {
        let summary = DocumentSummary {
            documents_scanned: 3,
            by_pattern: Box::from([
                DocumentReach {
                    documents: 1,
                    top_counts: Box::from([1_012]),
                },
                DocumentReach {
                    documents: 0,
                    top_counts: Box::default(),
                },
            ]),
        };
        let snapshot = snapshot(Some(summary));
        assert!(document_share(&snapshot, PatternKey::new(1).unwrap()).is_none());
        let single = document_share(&snapshot, PatternKey::new(0).unwrap()).unwrap();
        assert_eq!(
            (
                single.top_one_label.as_str(),
                single.top_five_label.as_str()
            ),
            ("100.0", "100.0")
        );
    }
}
