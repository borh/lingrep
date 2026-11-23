//! Source statistics for direct and prepared queries.

use lingrep_core::analysis::AnalyzerSession;
use lingrep_core::execution::{
    NativeAnalysisSource, OversizedSentenceOmissions, SentenceBatchSource,
};
use lingrep_corpus::PreparedSentenceSource;

use crate::direct_source::{
    DirectSentenceSource, DirectSourceStats, OversizedSentenceOmissionSummary,
};

pub(crate) type BoxQuerySource = Box<dyn QuerySentenceSource + Send>;

pub(crate) trait QuerySentenceSource: SentenceBatchSource {
    fn into_stats(self: Box<Self>) -> QueryInputStats;
}

impl<I> QuerySentenceSource for DirectSentenceSource<I>
where
    Self: SentenceBatchSource,
{
    fn into_stats(self: Box<Self>) -> QueryInputStats {
        QueryInputStats::Direct((*self).into_stats())
    }
}

impl QuerySentenceSource for PreparedSentenceSource {
    fn into_stats(self: Box<Self>) -> QueryInputStats {
        QueryInputStats::Prepared {
            documents: self.admitted_documents(),
            sentences_proven_absent: self.sentences_proven_absent(),
            omissions: self.oversized_omissions(),
        }
    }
}

impl<A: AnalyzerSession> QuerySentenceSource for NativeAnalysisSource<BoxQuerySource, A> {
    fn into_stats(self: Box<Self>) -> QueryInputStats {
        let (source, _) = (*self).into_parts();
        source.into_stats()
    }
}

pub(crate) enum QueryInputStats {
    Direct(DirectSourceStats),
    Prepared {
        documents: u64,
        sentences_proven_absent: u64,
        omissions: Option<OversizedSentenceOmissions>,
    },
}

impl QueryInputStats {
    pub(crate) fn documents_admitted(&self) -> u64 {
        match self {
            Self::Direct(stats) => stats.documents_admitted(),
            Self::Prepared { documents, .. } => *documents,
        }
    }

    pub(crate) fn sentences_proven_absent(&self) -> u64 {
        match self {
            Self::Direct(_) => 0,
            Self::Prepared {
                sentences_proven_absent,
                ..
            } => *sentences_proven_absent,
        }
    }

    pub(crate) fn observed_division_kinds(
        &self,
    ) -> Option<&std::collections::BTreeSet<compact_str::CompactString>> {
        match self {
            Self::Direct(stats) => Some(stats.observed_division_kinds()),
            Self::Prepared { .. } => None,
        }
    }

    pub(crate) fn max_staged_raw_bytes(&self) -> usize {
        match self {
            Self::Direct(stats) => stats.max_staged_raw_bytes(),
            Self::Prepared { .. } => 0,
        }
    }

    pub(crate) fn oversized_sentence_omissions(&self) -> Option<OversizedSentenceOmissionSummary> {
        match self {
            Self::Direct(stats) => stats.oversized_sentence_omissions(),
            Self::Prepared { omissions, .. } => {
                omissions
                    .as_ref()
                    .map(|summary| OversizedSentenceOmissionSummary {
                        count: summary.count,
                        first_source_name: summary
                            .first_source_name
                            .as_deref()
                            .unwrap_or_default()
                            .to_owned(),
                        first_ordinal: summary.first_ordinal,
                        first_actual: summary.first_actual,
                        limit: summary.limit,
                    })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prepared_omissions_report_the_count_and_first_location() {
        let mut omissions = OversizedSentenceOmissions::default();
        omissions.record(7, Some("big.md".into()), 65_537, 65_536);
        omissions.record(9, Some("later.md".into()), 70_000, 65_536);
        let stats = QueryInputStats::Prepared {
            documents: 0,
            sentences_proven_absent: 0,
            omissions: omissions.summary(),
        };
        assert_eq!(
            stats.oversized_sentence_omissions(),
            Some(OversizedSentenceOmissionSummary {
                count: 2,
                first_source_name: "big.md".into(),
                first_ordinal: 7,
                first_actual: 65_537,
                limit: 65_536,
            })
        );
    }
}
