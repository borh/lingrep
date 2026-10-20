use std::borrow::Cow;
use std::collections::HashMap;

use lingrep_core::execution::{ExecutionError, MatchSink, SentenceLocation, SinkControl};
use lingrep_core::fsm::Match;

use crate::derived_patterns::{compound_name, contiguous_chains};
use crate::export::text::truncate_cell;
use crate::inspection::{InspectionProjector, WHOLE_SENTENCE_CONTEXT, kwic_orth_split};
use crate::snapshot::PatternKey;
use compact_str::CompactString;

pub(crate) struct ExportRowRaw {
    pub sentence_index: u64,
    pub left: String,
    pub key: String,
    pub right: String,
}

pub(crate) struct PatternScan {
    pub name: String,
    pub exact_matches: u64,
    pub rows: Vec<ExportRowRaw>,
}

pub(crate) struct ExportScanData {
    pub patterns: Vec<PatternScan>,
    pub exact_matches: u64,
    pub exported_rows: usize,
}

pub(crate) struct ExportSink {
    patterns: Vec<PatternScan>,
    index_of: HashMap<String, usize>,
    max_rows: usize,
    max_rows_per_pattern: usize,
    kwic_char_cap: usize,
    derive_compounds: bool,
    compound_ids: HashMap<Vec<PatternKey>, usize>,
    exact_matches: u64,
    exported_rows: usize,
}

impl ExportSink {
    #[cfg(test)]
    pub(crate) fn new(
        pattern_names: &[&str],
        max_rows: usize,
        max_rows_per_pattern: usize,
        kwic_char_cap: usize,
    ) -> Self {
        Self::new_with_compounds(
            pattern_names,
            max_rows,
            max_rows_per_pattern,
            kwic_char_cap,
            false,
        )
    }

    pub(crate) fn new_with_compounds(
        pattern_names: &[&str],
        max_rows: usize,
        max_rows_per_pattern: usize,
        kwic_char_cap: usize,
        derive_compounds: bool,
    ) -> Self {
        let mut patterns = Vec::with_capacity(pattern_names.len());
        let mut index_of = HashMap::with_capacity(pattern_names.len());
        for (i, name) in pattern_names.iter().enumerate() {
            index_of.insert((*name).to_string(), i);
            patterns.push(PatternScan {
                name: (*name).to_string(),
                exact_matches: 0,
                rows: Vec::new(),
            });
        }
        Self {
            patterns,
            index_of,
            max_rows,
            max_rows_per_pattern,
            kwic_char_cap,
            derive_compounds,
            compound_ids: HashMap::new(),
            exact_matches: 0,
            exported_rows: 0,
        }
    }

    pub(crate) fn finish(self) -> ExportScanData {
        ExportScanData {
            patterns: self.patterns,
            exact_matches: self.exact_matches,
            exported_rows: self.exported_rows,
        }
    }
}

impl MatchSink for ExportSink {
    fn push(
        &mut self,
        location: &SentenceLocation,
        sentence: &dyn lingrep_core::token_match::TokenSequence,
        matches: &[Match],
    ) -> Result<SinkControl, ExecutionError> {
        let sentence_index = location.corpus_sentence_ordinal;
        let mut published_matches = Cow::Borrowed(matches);
        if self.derive_compounds {
            let mut derived_matches = matches.to_vec();
            let components = matches
                .iter()
                .map(|matched| {
                    let index = *self.index_of.get(matched.pattern.as_str()).ok_or_else(|| {
                        ExecutionError::Sink("match referenced an unknown pattern".into())
                    })?;
                    let key = PatternKey::new(index).ok_or_else(|| {
                        ExecutionError::Sink(
                            "export pattern index exceeded u32 identity space".into(),
                        )
                    })?;
                    Ok((key, matched.start, matched.end))
                })
                .collect::<Result<Vec<_>, ExecutionError>>()?;
            for chain in contiguous_chains(&components) {
                let index = if let Some(index) = self.compound_ids.get(&chain.components) {
                    *index
                } else {
                    let names = chain
                        .components
                        .iter()
                        .map(|key| self.patterns[key.index()].name.clone())
                        .collect::<Vec<_>>();
                    let name =
                        compound_name(&names, |candidate| self.index_of.contains_key(candidate));
                    let index = self.patterns.len();
                    self.patterns.push(PatternScan {
                        name: name.clone(),
                        exact_matches: 0,
                        rows: Vec::new(),
                    });
                    self.index_of.insert(name, index);
                    self.compound_ids.insert(chain.components.clone(), index);
                    index
                };
                derived_matches.push(Match {
                    pattern: CompactString::from(self.patterns[index].name.as_str()),
                    start: chain.start,
                    end: chain.end,
                    captures: Box::default(),
                    graph_bindings: Box::default(),
                    lexical: None,
                });
            }
            derived_matches.sort_by(|left, right| {
                left.start
                    .cmp(&right.start)
                    .then_with(|| left.end.cmp(&right.end))
                    .then_with(|| left.pattern.cmp(&right.pattern))
            });
            published_matches = Cow::Owned(derived_matches);
        }
        for matched in published_matches.iter() {
            let idx = *self.index_of.get(matched.pattern.as_str()).ok_or_else(|| {
                ExecutionError::Sink("match referenced an unknown pattern".into())
            })?;
            self.exact_matches += 1;
            self.patterns[idx].exact_matches += 1;
            if self.exported_rows >= self.max_rows
                || self.patterns[idx].rows.len() >= self.max_rows_per_pattern
            {
                continue;
            }
            let inspection = InspectionProjector::project_window(
                sentence,
                matched.start..matched.end,
                WHOLE_SENTENCE_CONTEXT,
            );
            let (left, key, right) = kwic_orth_split(&inspection);
            self.patterns[idx].rows.push(ExportRowRaw {
                sentence_index,
                left: truncate_cell(&left, self.kwic_char_cap),
                key: truncate_cell(&key, self.kwic_char_cap),
                right: truncate_cell(&right, self.kwic_char_cap),
            });
            self.exported_rows += 1;
        }
        // `max_rows` caps exported rows, not publication: a match past the cap
        // still counts toward the exact totals this scan reports.
        //
        // The runner validates this count against the supplied slice; a derived
        // compound is output built from these records, not one of them.
        Ok(SinkControl::Continue {
            records_emitted: matches.len(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use compact_str::CompactString;
    use lingrep_core::token_match::TokenInfo;

    fn location(ordinal: u64) -> SentenceLocation {
        SentenceLocation {
            source_metadata: None,
            corpus_sentence_ordinal: ordinal,
            document_id: lingrep_core::corpus::DocumentId::new(0),
            document_ordinal: 0,
            document_sentence_ordinal: ordinal,
            source_name: "".into(),
        }
    }

    fn tok(orth: &str) -> TokenInfo {
        TokenInfo::default().with_orth(orth)
    }
    fn m(pattern: &str, start: usize, end: usize) -> lingrep_core::fsm::Match {
        lingrep_core::fsm::Match {
            pattern: CompactString::from(pattern),
            start,
            end,
            captures: Box::default(),
            graph_bindings: Box::default(),
            lexical: None,
        }
    }

    #[test]
    fn projects_kwic_and_groups_by_pattern() {
        let mut sink = ExportSink::new(&["p"], 100, 100, 1_024);
        let s = vec![tok("だと"), tok("思っ"), tok("た"), tok("のは")];
        sink.push(&location(7), &s, &[m("p", 1, 3)]).unwrap();
        let data = sink.finish();
        assert_eq!((data.exact_matches, data.exported_rows), (1, 1));
        let p = &data.patterns[0];
        assert_eq!(p.name, "p");
        assert_eq!(p.rows[0].sentence_index, 7);
        assert_eq!(
            (
                p.rows[0].left.as_str(),
                p.rows[0].key.as_str(),
                p.rows[0].right.as_str()
            ),
            ("だと", "思った", "のは")
        );
    }
    #[test]
    fn caps_rows_but_counts_every_match() {
        let mut sink = ExportSink::new(&["p"], 100, 2, 1_024);
        let s = vec![tok("あ"), tok("い")];
        for i in 0..5 {
            sink.push(&location(i), &s, &[m("p", 0, 1)]).unwrap();
        }
        let data = sink.finish();
        assert_eq!((data.exact_matches, data.exported_rows), (5, 2));
        assert_eq!(data.patterns[0].exact_matches, 5);
        assert_eq!(data.patterns[0].rows.len(), 2);
    }

    #[test]
    fn per_pattern_example_cap_does_not_crowd_out_other_patterns() {
        let mut sink = ExportSink::new(&["p", "q"], 100, 2, 1_024);
        let sentence = vec![tok("あ")];
        for i in 0..30 {
            sink.push(&location(i), &sentence, &[m("p", 0, 1), m("q", 0, 1)])
                .unwrap();
        }
        let data = sink.finish();
        assert_eq!(data.exact_matches, 60);
        assert_eq!(data.exported_rows, 4);
        assert_eq!(data.patterns[0].rows.len(), 2);
        assert_eq!(data.patterns[1].rows.len(), 2);
        assert_eq!(data.patterns[0].exact_matches, 30);
        assert_eq!(data.patterns[1].exact_matches, 30);
    }
    #[test]
    fn caps_kwic_field_length() {
        let mut sink = ExportSink::new(&["p"], 100, 100, 4);
        let s = vec![tok("あいうえおか"), tok("x")];
        sink.push(&location(0), &s, &[m("p", 1, 2)]).unwrap();
        assert!(sink.finish().patterns[0].rows[0].left.chars().count() <= 4);
    }

    #[test]
    fn export_includes_every_derived_contiguous_chain_when_selected() {
        let mut sink = ExportSink::new_with_compounds(&["a", "b", "c"], 100, 20, 1_024, true);
        let sentence = vec![tok("甲"), tok("乙"), tok("丙")];
        sink.push(
            &location(0),
            &sentence,
            &[m("a", 0, 1), m("b", 1, 2), m("c", 2, 3)],
        )
        .unwrap();
        let data = sink.finish();
        assert_eq!(data.exact_matches, 6);
        assert_eq!(data.patterns.len(), 6);
        let derived = data
            .patterns
            .iter()
            .filter(|pattern| pattern.name.starts_with("複合項目"))
            .collect::<Vec<_>>();
        assert_eq!(derived.len(), 3);
        assert!(derived.iter().all(|pattern| pattern.exact_matches == 1));
        assert!(derived.iter().all(|pattern| pattern.rows.len() == 1));
    }

    #[test]
    fn derived_compounds_report_the_supplied_slice_count() {
        let mut sink = ExportSink::new_with_compounds(&["a", "b"], 100, 20, 1_024, true);
        let sentence = vec![tok("甲"), tok("乙")];
        let control = sink
            .push(&location(0), &sentence, &[m("a", 0, 1), m("b", 1, 2)])
            .unwrap();
        assert_eq!(control, SinkControl::Continue { records_emitted: 2 });
        assert_eq!(sink.finish().exact_matches, 3, "one derived chain");
    }

    #[test]
    fn unknown_pattern_name_is_a_sink_error() {
        let mut sink = ExportSink::new(&["p"], 100, 100, 1_024);
        let err = sink
            .push(&location(0), &[tok("あ")], &[m("other", 0, 1)])
            .unwrap_err();
        assert!(matches!(
            err,
            lingrep_core::execution::ExecutionError::Sink(_)
        ));
    }
}
