//! Shared filesystem, stdin, and prepared source adapter for matching queries.

use std::sync::{Arc, Mutex};

use lingrep_core::execution::{ExecutionError, OversizedSentenceOmissions, TokenizedAdmission};
use lingrep_corpus::PreparedSentenceSource;

use crate::direct_source::{
    BoxSentenceSource, DirectSentenceSource, DirectSourceStats, OversizedSentenceOmissionSummary,
    boxed_direct_source,
};

pub(crate) fn direct<I>(
    source: Box<DirectSentenceSource<I>>,
) -> (BoxSentenceSource, QueryInputStats)
where
    DirectSentenceSource<I>: lingrep_core::execution::SentenceBatchSource + Send + 'static,
{
    let (source, stats) = boxed_direct_source(source);
    (source, QueryInputStats::Direct(stats))
}

pub(crate) fn prepared(
    source: PreparedSentenceSource,
    documents: u64,
) -> (BoxSentenceSource, QueryInputStats) {
    let omissions = Arc::new(Mutex::new(OversizedSentenceOmissions::default()));
    (
        Box::new(source.with_tokenized_admission(TokenizedAdmission::Omit(Arc::clone(&omissions)))),
        QueryInputStats::Prepared {
            documents,
            omissions,
        },
    )
}

pub(crate) enum QueryInputStats {
    Direct(DirectSourceStats),
    Prepared {
        documents: u64,
        omissions: Arc<Mutex<OversizedSentenceOmissions>>,
    },
}

impl QueryInputStats {
    pub(crate) fn documents_admitted(&self) -> u64 {
        match self {
            Self::Direct(stats) => stats.documents_admitted(),
            Self::Prepared { documents, .. } => *documents,
        }
    }

    pub(crate) fn max_staged_raw_bytes(&self) -> usize {
        match self {
            Self::Direct(stats) => stats.max_staged_raw_bytes(),
            Self::Prepared { .. } => 0,
        }
    }

    pub(crate) fn oversized_sentence_omissions(
        &self,
    ) -> Result<Option<OversizedSentenceOmissionSummary>, ExecutionError> {
        match self {
            Self::Direct(stats) => stats.oversized_sentence_omissions(),
            Self::Prepared { omissions, .. } => {
                let omissions = omissions
                    .lock()
                    .map_err(|_| ExecutionError::ResourceAccounting)?;
                Ok(omissions
                    .summary()
                    .map(|summary| OversizedSentenceOmissionSummary {
                        count: summary.count,
                        first_source_name: summary
                            .first_source_name
                            .map(|name| name.to_string())
                            .unwrap_or_default(),
                        first_ordinal: summary.first_ordinal,
                        first_actual: summary.first_actual,
                        limit: summary.limit,
                    }))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prepared_stats_map_omissions_into_the_direct_summary_shape() {
        let omissions = Arc::new(Mutex::new(OversizedSentenceOmissions::default()));
        let stats = QueryInputStats::Prepared {
            documents: 3,
            omissions: Arc::clone(&omissions),
        };
        assert_eq!(stats.documents_admitted(), 3);
        assert_eq!(
            stats.oversized_sentence_omissions().expect("handle reads"),
            None
        );

        {
            let mut omissions = omissions.lock().expect("lock");
            omissions.record(7, Some(Arc::from("big.md")), 65_537, 65_536);
            omissions.record(9, Some(Arc::from("later.md")), 70_000, 65_536);
        }
        let summary = stats
            .oversized_sentence_omissions()
            .expect("handle reads")
            .expect("omissions are reported");
        assert_eq!(summary.count, 2);
        assert_eq!(summary.first_ordinal, 7);
        assert_eq!(summary.first_source_name, "big.md");
    }
}
