//! Deterministic corpus-unit sampling and pre-analysis sentence filtering.

use super::source::{ExecutionBatch, ExecutionSentence, SentenceBatchSource};
use super::{ExecutionError, ExecutionLimits};

/// Rate denominator. Parts per million, so the admission test is integer
/// arithmetic end to end.
///
/// A float rate would make the sample depend on the platform's rounding, and
/// "the same corpus gives the same sample" is the property that makes a
/// sampled result quotable.
pub const RATE_SCALE: u32 = 1_000_000;

/// Inclusion rate and seed for deterministic corpus-unit selection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CorpusSample {
    rate_per_million: u32,
    seed: u64,
}

impl CorpusSample {
    /// Rejects a rate of zero — which would admit nothing and report it as a
    /// corpus — and a rate above [`RATE_SCALE`], which is not a rate.
    ///
    /// A rate of exactly [`RATE_SCALE`] is accepted and admits everything, so
    /// `--sample 1.0` is a valid no-op rather than an error to explain.
    #[must_use]
    pub const fn new(rate_per_million: u32, seed: u64) -> Option<Self> {
        if rate_per_million == 0 || rate_per_million > RATE_SCALE {
            return None;
        }
        Some(Self {
            rate_per_million,
            seed,
        })
    }

    #[must_use]
    pub const fn rate_per_million(self) -> u32 {
        self.rate_per_million
    }

    #[must_use]
    pub const fn seed(self) -> u64 {
        self.seed
    }

    /// Whether a unit identified by its first corpus sentence ordinal is selected.
    ///
    /// A pure function of the seed and the **corpus sentence ordinal**, so the
    /// sample is a property of the corpus rather than of the run: batching,
    /// worker count, scan order and where a `--limit` stopped cannot move it,
    /// and two runs over one corpus look at the same sentences. That is also
    /// what lets a sampled run be compared against another sampled run at the
    /// same seed.
    #[must_use]
    pub const fn admits(self, ordinal: u64) -> bool {
        mix(self.seed ^ mix(ordinal)) % (RATE_SCALE as u64) < self.rate_per_million as u64
    }
}

/// SplitMix64. Written out rather than taken from a crate or from
/// `DefaultHasher`, whose output Rust does not promise to keep stable across
/// releases — a sample that changed with the toolchain would not be
/// reproducible, which is the whole claim being made about it.
const fn mix(z: u64) -> u64 {
    let mut x = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

/// Wraps a source and drops the sentences the sample excludes.
///
/// Placed between the source and the executor rather than inside either, so
/// one implementation covers every source the CLI can build and no source has
/// to know it is being sampled. Filtering here still precedes analysis: a
/// direct source yields raw text and the executor tokenizes it, so a dropped
/// sentence is never tokenized.
#[derive(Debug)]
pub struct SampledSentences<S> {
    inner: S,
    sample: Option<CorpusSample>,
}

impl<S> SampledSentences<S> {
    /// `None` is a pass-through, delegating without touching a batch.
    ///
    /// Taking the option here rather than at the call site is what lets an
    /// unsampled run reach the executor through one code path instead of a
    /// duplicated thirteen-argument call — and the pass-through is exact: an
    /// unsampled run does not pay a hash per sentence to learn it is not
    /// being sampled.
    #[must_use]
    pub const fn new(inner: S, sample: Option<CorpusSample>) -> Self {
        Self { inner, sample }
    }
}

/// Retains the sentences the sample admits, and their charges with them.
///
/// The charge vector is positional, so filtering the two independently would
/// misattribute residency to the wrong sentence — silently, since both stay
/// the same length as each other.
fn retain_sampled<T>(
    sentences: &mut Vec<ExecutionSentence<T>>,
    charges: Option<&mut Vec<usize>>,
    sample: CorpusSample,
) {
    let Some(charges) = charges else {
        sentences.retain(|sentence| sample.admits(sentence.location.corpus_sentence_ordinal));
        return;
    };
    debug_assert_eq!(
        sentences.len(),
        charges.len(),
        "a charge vector is positional and must match its sentences"
    );
    let mut kept = Vec::with_capacity(charges.len());
    let mut index = 0;
    sentences.retain(|sentence| {
        let admitted = sample.admits(sentence.location.corpus_sentence_ordinal);
        if admitted && let Some(charge) = charges.get(index) {
            kept.push(*charge);
        }
        index += 1;
        admitted
    });
    *charges = kept;
}

impl<S: SentenceBatchSource> SentenceBatchSource for SampledSentences<S> {
    fn next_batch(
        &mut self,
        limits: &ExecutionLimits,
    ) -> Result<Option<ExecutionBatch>, ExecutionError> {
        let Some(sample) = self.sample else {
            return self.inner.next_batch(limits);
        };
        // The trait promises a *non-empty* batch, and a sample can empty one
        // outright, so pull until something survives or the source ends.
        loop {
            let Some(mut batch) = self.inner.next_batch(limits)? else {
                return Ok(None);
            };
            let empty = match &mut batch {
                ExecutionBatch::Text(sentences) => {
                    retain_sampled(sentences, None, sample);
                    sentences.is_empty()
                }
                ExecutionBatch::Tokenized { sentences, charges } => {
                    retain_sampled(sentences, charges.as_mut(), sample);
                    sentences.is_empty()
                }
                ExecutionBatch::BoundTokenized { sentences, charges } => {
                    retain_sampled(sentences, Some(charges), sample);
                    sentences.is_empty()
                }
            };
            if !empty {
                return Ok(Some(batch));
            }
        }
    }

    fn wants_lookahead(&self) -> bool {
        // Delegated, never answered here: this wrapper does not change whether
        // how far the source read is observable, and the trait's own
        // documentation records what a defaulted answer at a delegating
        // wrapper already cost once.
        self.inner.wants_lookahead()
    }
}

/// Deterministic Algorithm R draw; reduce modulo the one-based number of records seen.
#[must_use]
pub const fn reservoir_draw(seed: u64, stream: u64, seen: u64) -> u64 {
    mix(mix(seed ^ stream.wrapping_mul(0x9E37_79B9_7F4A_7C15)) ^ seen)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rate_of_zero_or_above_one_is_not_a_sample() {
        assert!(CorpusSample::new(0, 7).is_none(), "admits nothing");
        assert!(CorpusSample::new(RATE_SCALE + 1, 7).is_none(), "not a rate");
        assert!(CorpusSample::new(RATE_SCALE, 7).is_some(), "a valid no-op");
    }

    #[test]
    fn reservoir_draws_pin_the_selection_algorithm() {
        assert_eq!(
            [1, 2, 100, 1000].map(|seen| reservoir_draw(7, 0, seen)),
            [
                4414019431610648415,
                5090977316425868581,
                5094126703666729024,
                6785964006689842358
            ]
        );
    }

    #[test]
    fn fixed_seed_draws_are_stable() {
        let sample = CorpusSample::new(500_000, 7).unwrap();
        assert_eq!(
            [0, 3, 259, 266, 522].map(|id| sample.admits(id)),
            [true, true, false, true, true]
        );
    }

    #[test]
    fn a_full_rate_admits_every_sentence() {
        let sample = CorpusSample::new(RATE_SCALE, 1).unwrap();
        assert!((0..5_000).all(|ordinal| sample.admits(ordinal)));
    }

    /// The rate is honoured in aggregate. Loose bounds on purpose: this pins
    /// that the hash is not degenerate, not that it passes a uniformity test
    /// no fixed seed should be asked to pass.
    #[test]
    fn the_admitted_fraction_tracks_the_requested_rate() {
        let sample = CorpusSample::new(RATE_SCALE / 100, 0xABCD).unwrap();
        let admitted = (0..200_000u64)
            .filter(|ordinal| sample.admits(*ordinal))
            .count();
        assert!(
            (1_600..2_400).contains(&admitted),
            "1% of 200,000 should land near 2,000, got {admitted}"
        );
    }

    /// The property that makes a sampled result quotable: the sample is a
    /// function of the corpus and the seed, not of the run.
    #[test]
    fn one_seed_selects_one_sample_however_the_run_is_shaped() {
        let sample = CorpusSample::new(RATE_SCALE / 20, 99).unwrap();
        let first = (0..10_000u64)
            .filter(|o| sample.admits(*o))
            .collect::<Vec<_>>();
        // Evaluated in a different order, as a differently batched run would.
        let second = (0..10_000u64)
            .rev()
            .filter(|o| sample.admits(*o))
            .collect::<Vec<_>>();
        let second = second.into_iter().rev().collect::<Vec<_>>();
        assert_eq!(first, second);
        assert!(!first.is_empty());
    }

    /// Drains a sampled source and reports the ordinals that survived, plus
    /// the batch sizes, so a test can also see that no empty batch escaped.
    fn drain(sentences: usize, sample: Option<CorpusSample>) -> (Vec<u64>, Vec<usize>) {
        let source = super::super::source::TextIteratorSource::new(
            (0..sentences).map(|index| format!("sentence {index}")),
        );
        let mut sampled = SampledSentences::new(source, sample);
        let limits = ExecutionLimits::default();
        let mut ordinals = Vec::new();
        let mut sizes = Vec::new();
        while let Some(batch) = sampled
            .next_batch(&limits)
            .expect("the fixture never fails")
        {
            let ExecutionBatch::Text(sentences) = batch else {
                panic!("a text source yields text batches");
            };
            sizes.push(sentences.len());
            ordinals.extend(
                sentences
                    .iter()
                    .map(|sentence| sentence.location.corpus_sentence_ordinal),
            );
        }
        (ordinals, sizes)
    }

    #[test]
    fn an_unsampled_source_passes_every_sentence_through() {
        let (ordinals, _) = drain(500, None);
        assert_eq!(ordinals, (0..500).collect::<Vec<_>>());
    }

    /// The decorator keeps exactly the sentences [`CorpusSample::admits`]
    /// names, and keeps their corpus ordinals — the ordinal is what the sample
    /// is a function of, so a wrapper that renumbered would make a sampled run
    /// unreproducible while still looking correct.
    #[test]
    fn the_decorator_keeps_exactly_the_admitted_ordinals() {
        let sample = CorpusSample::new(RATE_SCALE / 10, 4242).unwrap();
        let (ordinals, _) = drain(2_000, Some(sample));
        let expected = (0..2_000u64)
            .filter(|ordinal| sample.admits(*ordinal))
            .collect::<Vec<_>>();
        assert_eq!(ordinals, expected);
        assert!(!expected.is_empty(), "the fixture must exercise the filter");
        assert!(expected.len() < 2_000, "and must actually drop something");
    }

    /// `next_batch` promises a non-empty batch or `None`, and a sample can
    /// empty one outright — so the wrapper has to keep pulling rather than
    /// hand an empty batch to the executor.
    #[test]
    fn a_sample_that_empties_a_batch_never_yields_an_empty_one() {
        // One in a million, so nearly every batch the source produces is
        // emptied and the pull loop is what makes progress.
        let sample = CorpusSample::new(1, 7).unwrap();
        let (_, sizes) = drain(5_000, Some(sample));
        assert!(
            sizes.iter().all(|size| *size > 0),
            "an empty batch reached the executor: {sizes:?}"
        );
    }

    #[test]
    fn lookahead_is_delegated_rather_than_answered_here() {
        let inner = super::super::source::TextIteratorSource::new(std::iter::empty::<String>());
        let expected = inner.wants_lookahead();
        let sampled = SampledSentences::new(
            super::super::source::TextIteratorSource::new(std::iter::empty::<String>()),
            CorpusSample::new(RATE_SCALE / 2, 0),
        );
        assert_eq!(sampled.wants_lookahead(), expected);
    }

    #[test]
    fn a_different_seed_selects_a_different_sample() {
        let left = CorpusSample::new(RATE_SCALE / 20, 1).unwrap();
        let right = CorpusSample::new(RATE_SCALE / 20, 2).unwrap();
        let of = |sample: CorpusSample| {
            (0..10_000u64)
                .filter(|o| sample.admits(*o))
                .collect::<Vec<_>>()
        };
        assert_ne!(of(left), of(right));
    }
}
