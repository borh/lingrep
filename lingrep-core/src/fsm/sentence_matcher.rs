use crate::execution::BoundSentenceInput;
use crate::fsm::matcher::CompiledCatalog;
use crate::fsm::types::Match;
use crate::fsm::{BoundCatalog, BoundExecutionError};
use crate::token_match::TokenInfoTrait;
use rayon::prelude::*;

/// Schedules independent sentence matches on a caller-owned Rayon pool.
///
/// This is deliberately batch-scoped: streaming callers pass only their current
/// chunk rather than collecting an entire corpus before matching.
pub struct SentenceMatcher<'a> {
    matcher: &'a CompiledCatalog,
}

/// Batch-scoped scheduler for schema-bound token storage.
pub struct BoundSentenceMatcher<'a> {
    matcher: &'a BoundCatalog,
}

impl<'a> BoundSentenceMatcher<'a> {
    pub fn new(matcher: &'a BoundCatalog) -> Self {
        Self { matcher }
    }

    pub fn match_all(
        &self,
        pool: &rayon::ThreadPool,
        sentences: &[BoundSentenceInput],
    ) -> Result<Vec<Vec<Match>>, BoundExecutionError> {
        pool.install(|| {
            sentences
                .par_iter()
                .map(|sentence| self.matcher.find_matches_input(sentence))
                .collect()
        })
    }
}

impl<'a> SentenceMatcher<'a> {
    pub fn new(matcher: &'a CompiledCatalog) -> Self {
        Self { matcher }
    }

    /// Matches every sentence in its input order using the supplied pool.
    pub fn match_all<T: TokenInfoTrait + Send + Sync>(
        &self,
        pool: &rayon::ThreadPool,
        sentences: &[Vec<T>],
    ) -> Vec<Vec<Match>> {
        pool.install(|| {
            sentences
                .par_iter()
                .map(|sentence| self.matcher.find_matches(sentence))
                .collect()
        })
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
