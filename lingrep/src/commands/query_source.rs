//! Source statistics for direct and prepared queries.

use lingrep_core::analysis::AnalyzerSession;
use lingrep_core::corpus::{AdmittedCorpus, CorpusEvent, ScopeCounts};
use lingrep_core::execution::{
    NativeAnalysisSource, OversizedSentenceOmissions, SentenceBatchSource,
};
use lingrep_corpus::PreparedSentenceSource;

use crate::direct_source::{
    DirectSentenceSource, DirectSourceStats, FilesystemCorpusEvents, FilesystemObservation,
    OversizedSentenceOmissionSummary,
};

pub(crate) type BoxQuerySource = Box<dyn QuerySentenceSource + Send>;

pub(crate) trait QuerySentenceSource: SentenceBatchSource {
    fn into_stats(self: Box<Self>) -> QueryInputStats;
}

trait DirectQueryEvents {
    fn into_observation(self) -> Option<Box<FilesystemObservation>>;
}

impl DirectQueryEvents for FilesystemCorpusEvents {
    fn into_observation(self) -> Option<Box<FilesystemObservation>> {
        Some(Box::new(FilesystemCorpusEvents::into_observation(self)))
    }
}

impl DirectQueryEvents for Box<dyn Iterator<Item = std::io::Result<CorpusEvent>> + Send> {
    fn into_observation(self) -> Option<Box<FilesystemObservation>> {
        None
    }
}

impl<I> QuerySentenceSource for DirectSentenceSource<AdmittedCorpus<I>>
where
    Self: SentenceBatchSource,
    I: DirectQueryEvents,
{
    fn into_stats(self: Box<Self>) -> QueryInputStats {
        let (events, source) = (*self).into_parts();
        let (events, scope_counts) = events.into_parts();
        QueryInputStats::Direct {
            source,
            scope_counts,
            filesystem: events.into_observation(),
        }
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
    Direct {
        source: DirectSourceStats,
        scope_counts: ScopeCounts,
        filesystem: Option<Box<FilesystemObservation>>,
    },
    Prepared {
        documents: u64,
        sentences_proven_absent: u64,
        omissions: Option<OversizedSentenceOmissions>,
    },
}

impl QueryInputStats {
    pub(crate) fn filesystem_observation(&self) -> Option<&FilesystemObservation> {
        match self {
            Self::Direct { filesystem, .. } => filesystem.as_deref(),
            Self::Prepared { .. } => None,
        }
    }

    pub(crate) fn documents_admitted(&self) -> u64 {
        match self {
            Self::Direct { source, .. } => source.documents_admitted(),
            Self::Prepared { documents, .. } => *documents,
        }
    }

    pub(crate) fn sentences_proven_absent(&self) -> u64 {
        match self {
            Self::Direct { .. } => 0,
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
            Self::Direct { source, .. } => Some(source.observed_division_kinds()),
            Self::Prepared { .. } => None,
        }
    }

    pub(crate) fn max_staged_raw_bytes(&self) -> usize {
        match self {
            Self::Direct { source, .. } => source.max_staged_raw_bytes(),
            Self::Prepared { .. } => 0,
        }
    }

    pub(crate) fn scope_counts(&self) -> ScopeCounts {
        match self {
            Self::Direct { scope_counts, .. } => *scope_counts,
            Self::Prepared { .. } => ScopeCounts::default(),
        }
    }

    pub(crate) fn oversized_sentence_omissions(&self) -> Option<OversizedSentenceOmissionSummary> {
        match self {
            Self::Direct { source, .. } => source.oversized_sentence_omissions(),
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
