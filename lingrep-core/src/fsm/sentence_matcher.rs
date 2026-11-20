use crate::execution::{BoundSentenceInput, window_at};
use crate::fsm::matcher::CompiledCatalog;
use crate::fsm::matcher::resource::OutputBudget;
use crate::fsm::types::Match;
use crate::fsm::{BoundCatalog, BoundExecutionError};
use crate::token_match::TokenInfoTrait;
use rayon::prelude::*;
use std::ops::Range;
use std::sync::Arc;

/// Schedules independent sentence matches on a caller-owned Rayon pool.
///
/// Batch-scoped: streaming callers pass only their current chunk rather than
/// collecting an entire corpus before matching.
pub struct SentenceMatcher<'a> {
    matcher: &'a CompiledCatalog,
}

/// Batch-scoped scheduler for schema-bound token storage.
pub struct BoundSentenceMatcher<'a> {
    matcher: &'a BoundCatalog,
    cancel: Option<&'a crate::execution::CancellationFlag>,
}

impl<'a> BoundSentenceMatcher<'a> {
    pub fn new(matcher: &'a BoundCatalog) -> Self {
        Self {
            matcher,
            cancel: None,
        }
    }

    pub(crate) fn with_cancellation(
        mut self,
        cancel: &'a crate::execution::CancellationFlag,
    ) -> Self {
        self.cancel = Some(cancel);
        self
    }

    pub fn match_all(
        &self,
        pool: &rayon::ThreadPool,
        sentences: &[BoundSentenceInput],
    ) -> Result<Vec<Vec<Match>>, BoundExecutionError> {
        let output = Arc::new(OutputBudget::default().with_cancellation(self.cancel));
        output
            .reserve_buffer::<Result<Vec<Match>, BoundExecutionError>>(
                sentences.len().saturating_mul(4),
            )
            .map_err(BoundExecutionError::Resource)?;
        let rows: Result<Vec<_>, _> = pool.install(|| {
            sentences
                .par_iter()
                .map(|sentence| {
                    self.matcher
                        .find_matches_input_with_output(sentence, &output)
                })
                .collect()
        });
        output.check().map_err(BoundExecutionError::Resource)?;
        rows
    }

    /// Matches `sentences[targets]` in order. A `within=` lookaround may
    /// match the neighbouring sentences of the slice.
    ///
    /// `links` holds one entry per sentence and per scope of
    /// [`CompiledCatalog::lookaround_reach`], in that order:
    /// `links[i * scopes + s]` states whether `sentences[i]` continues the
    /// window of `sentences[i - 1]` in scope `s`.
    pub fn match_in_windows(
        &self,
        pool: &rayon::ThreadPool,
        sentences: &[BoundSentenceInput],
        links: &[bool],
        targets: Range<usize>,
    ) -> Result<Vec<Vec<Match>>, BoundExecutionError> {
        let reach: Vec<_> = self
            .matcher
            .compiled
            .lookaround_reach()
            .into_iter()
            .collect();
        if reach.is_empty() {
            return self.match_all(pool, &sentences[targets]);
        }
        let stride = reach.len();
        debug_assert_eq!(links.len(), sentences.len() * stride);
        let output = Arc::new(OutputBudget::default().with_cancellation(self.cancel));
        output
            .reserve_buffer::<Result<Vec<Match>, BoundExecutionError>>(
                targets.len().saturating_mul(4),
            )
            .map_err(BoundExecutionError::Resource)?;
        let rows: Result<Vec<_>, _> = pool.install(|| {
            targets
                .into_par_iter()
                .map(|index| {
                    let windows: Vec<_> = reach
                        .iter()
                        .enumerate()
                        .map(|(scope, (name, reach))| {
                            let window = window_at(sentences, links, stride, scope, *reach, index);
                            (name, &sentences[window.before], &sentences[window.after])
                        })
                        .collect();
                    self.matcher.find_matches_input_in_windows_with_output(
                        &sentences[index],
                        &windows,
                        &output,
                    )
                })
                .collect()
        });
        output.check().map_err(BoundExecutionError::Resource)?;
        rows
    }
}

impl<'a> SentenceMatcher<'a> {
    pub fn new(matcher: &'a CompiledCatalog) -> Self {
        Self { matcher }
    }

    /// Matches every sentence in its input order using the supplied pool.
    /// # Panics
    /// Panics when matching exceeds a resource budget; use [`Self::try_match_all`] to handle this error.
    pub fn match_all<T: TokenInfoTrait + Send + Sync>(
        &self,
        pool: &rayon::ThreadPool,
        sentences: &[Vec<T>],
    ) -> Vec<Vec<Match>> {
        self.try_match_all(pool, sentences)
            .expect("matching exceeded its resource budget")
    }

    /// Preserves sentence order and returns no batch results if matching exceeds a resource budget.
    pub fn try_match_all<T: TokenInfoTrait + Send + Sync>(
        &self,
        pool: &rayon::ThreadPool,
        sentences: &[Vec<T>],
    ) -> Result<Vec<Vec<Match>>, crate::fsm::MatchResourceError> {
        let output = Arc::new(OutputBudget::default());
        output.reserve_buffer::<Result<Vec<Match>, crate::fsm::MatchResourceError>>(
            sentences.len().saturating_mul(4),
        )?;
        let rows: Result<Vec<_>, _> = pool.install(|| {
            sentences
                .par_iter()
                .map(|sentence| {
                    self.matcher
                        .try_find_matches_with_context_and_output(sentence, None, &output)
                })
                .collect()
        });
        output.check()?;
        rows
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fsm::PatternSource;
    use crate::fsm::matcher::{CompiledCatalog, FsmBuilder};
    use crate::token_match::{BoundTokenSentence, TokenAttributeIndex, TokenInfo, TokenSchema};

    fn matcher() -> CompiledCatalog {
        let mut builder = FsmBuilder::new();
        builder
            .add_definition(PatternSource::named("a.lx", "a := [text=a]."))
            .unwrap();
        builder.build().unwrap().matcher
    }

    fn token(orth: &str) -> TokenInfo {
        let mut token = TokenInfo::default();
        token.set(TokenAttributeIndex::Orth, orth.into());
        token
    }

    #[test]
    fn sentence_scheduler_matches_sequential_results_in_input_order() {
        let matcher = matcher();
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(2)
            .build()
            .unwrap();
        let sentences = vec![vec![token("a")], vec![token("b")]];

        let scheduled = SentenceMatcher::new(&matcher).match_all(&pool, &sentences);
        let sequential = sentences
            .iter()
            .map(|sentence| matcher.find_matches(sentence))
            .collect::<Vec<_>>();

        assert_eq!(scheduled, sequential);
    }

    #[test]
    fn bound_sentence_scheduler_preserves_input_order() {
        let compiled = matcher();
        let schema = TokenSchema::unidic_morphology();
        let bound = compiled.bind(&schema).unwrap();
        let sentences = [vec![token("a")], vec![token("b")]]
            .into_iter()
            .map(|tokens| {
                BoundTokenSentence::project_for_schema(&schema, bound.projection(), &tokens)
                    .map(|words| {
                        BoundSentenceInput::new(
                            words,
                            crate::execution::EnhancedGraphInput::NotRequested,
                        )
                    })
                    .unwrap()
            })
            .collect::<Vec<_>>();
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(2)
            .build()
            .unwrap();

        let scheduled = BoundSentenceMatcher::new(&bound)
            .match_all(&pool, &sentences)
            .unwrap();

        assert_eq!(scheduled[0].len(), 1);
        assert!(scheduled[1].is_empty());
    }

    /// Regex predicates evaluate through per-thread regex clones; every
    /// worker must produce the sequential result for regex-gated patterns.
    #[test]
    fn sentence_scheduler_matches_regex_predicates_across_threads() {
        let mut builder = FsmBuilder::new();
        builder
            .add_definition(PatternSource::named(
                "re.lx",
                "re := [text~/^a+b$/] [text=c]?.",
            ))
            .unwrap();
        let matcher = builder.build().unwrap().matcher;
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(4)
            .build()
            .unwrap();
        let sentences = (0..64)
            .map(|index| {
                vec![
                    token(if index % 2 == 0 { "aab" } else { "ab" }),
                    token(if index % 3 == 0 { "c" } else { "x" }),
                ]
            })
            .collect::<Vec<_>>();

        let scheduled = SentenceMatcher::new(&matcher).match_all(&pool, &sentences);
        let sequential = sentences
            .iter()
            .map(|sentence| matcher.find_matches(sentence))
            .collect::<Vec<_>>();

        assert_eq!(scheduled, sequential);
        assert!(scheduled.iter().any(|matches| !matches.is_empty()));
    }

    #[hegel::test(test_cases = 200, derandomize = true)]
    fn sentence_scheduler_preserves_sequential_semantics(tc: hegel::TestCase) {
        use hegel::generators;

        let matcher = matcher();
        let thread_count = tc.draw(generators::integers::<usize>().min_value(1).max_value(4));
        let symbols = tc.draw(
            generators::vecs(
                generators::vecs(generators::sampled_from(vec!["a", "b", "x"])).max_size(8),
            )
            .max_size(16),
        );
        tc.target(symbols.len() as f64);
        let sentences = symbols
            .iter()
            .map(|sentence| sentence.iter().map(|symbol| token(symbol)).collect())
            .collect::<Vec<Vec<_>>>();
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(thread_count)
            .build()
            .expect("bounded generated thread count is valid");

        let scheduled = SentenceMatcher::new(&matcher).match_all(&pool, &sentences);
        let sequential = sentences
            .iter()
            .map(|sentence| matcher.find_matches(sentence))
            .collect::<Vec<_>>();

        assert_eq!(scheduled, sequential);
    }
}
