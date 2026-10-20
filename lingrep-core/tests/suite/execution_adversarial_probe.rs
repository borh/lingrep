use std::sync::atomic::{AtomicUsize, Ordering};

use lingrep_core::dictionary::create_tokenizer_from_path;
use lingrep_core::execution::{
    CancellationFlag, ExecutionError, ExecutionLimits, ExecutionProgress, MatchSink, RunOutcome,
    SentenceAdmission, SentenceLocation, SinkControl, TextIteratorSource,
    TokenizedBatchObservation, TokenizedBatchObserver, execute,
};
use lingrep_core::fsm::{CompileLimits, Match, PatternSource, compile_catalog};
use lingrep_core::token_match::{TokenSequence, tokenize};

const SENTENCE_BYTES: usize = 64 * 1024;

struct DiscardSink;

impl MatchSink for DiscardSink {
    fn push(
        &mut self,
        _location: &SentenceLocation,
        _sentence: &dyn TokenSequence,
        matches: &[Match],
    ) -> Result<SinkControl, ExecutionError> {
        Ok(SinkControl::Continue {
            records_emitted: matches.len(),
        })
    }
}

#[derive(Default)]
struct ResidencyObserver {
    estimates: Vec<usize>,
}

impl TokenizedBatchObserver for ResidencyObserver {
    fn observe(&mut self, observation: TokenizedBatchObservation) -> Result<(), ExecutionError> {
        self.estimates.push(observation.charged_bytes);
        Ok(())
    }
}

fn exact_bytes(unit: &str) -> String {
    let mut value = unit.repeat(SENTENCE_BYTES / unit.len());
    value.push_str(&"x".repeat(SENTENCE_BYTES - value.len()));
    assert_eq!(value.len(), SENTENCE_BYTES);
    value
}

#[test]
#[ignore = "requires LINGREP_DICTIONARY_PATH and performs real 64 KiB tokenization"]
fn public_executor_bounds_adversarial_64k_records_and_rejects_64k_plus_one() {
    let dictionary = std::env::var("LINGREP_DICTIONARY_PATH")
        .expect("set LINGREP_DICTIONARY_PATH to a Nix UniDic artifact");
    let tokenizer =
        create_tokenizer_from_path(&dictionary).expect("adversarial probe dictionary must load");
    let matcher = compile_catalog(
        PatternSource::named("never.lx", "never := [text=__never__]."),
        CompileLimits::default(),
    )
    .expect("adversarial probe matcher must compile")
    .matcher;
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(8)
        .build()
        .expect("adversarial probe pool must build");
    let limits = ExecutionLimits::default();

    let fixtures = [
        ("spaced-digits", "1 ".repeat(SENTENCE_BYTES / 2)),
        ("spaced-ascii", "a ".repeat(SENTENCE_BYTES / 2)),
        ("punctuation", exact_bytes("。")),
        ("combining-characters", exact_bytes("は\u{3099}")),
        ("long-lexeme", "a".repeat(SENTENCE_BYTES)),
    ];

    for (label, fixture) in fixtures {
        assert_eq!(fixture.len(), SENTENCE_BYTES);
        let tokenizations = AtomicUsize::new(0);
        let mut observer = ResidencyObserver::default();
        let outcome = execute(
            TextIteratorSource::new(std::iter::once(fixture.clone())),
            |sentence| {
                tokenizations.fetch_add(1, Ordering::SeqCst);
                tokenize(sentence, &tokenizer)
            },
            &matcher,
            &pool,
            &limits,
            1_000,
            &CancellationFlag::new(),
            &mut DiscardSink,
            &mut |_: ExecutionProgress, _: &DiscardSink| {},
            &mut observer,
            lingrep_core::execution::PolicySelection::Apply,
            &SentenceAdmission::All,
        )
        .expect("64 KiB adversarial record must execute");
        assert!(matches!(outcome, RunOutcome::Completed(_)));
        assert_eq!(tokenizations.load(Ordering::SeqCst), 1);
        assert_eq!(observer.estimates.len(), 1);
        assert!(observer.estimates[0] <= limits.max_tokenized_bytes);
        eprintln!(
            "ooc_adversarial/{label} raw_bytes={} estimated_batch_bytes={} limit={}",
            fixture.len(),
            observer.estimates[0],
            limits.max_tokenized_bytes,
        );

        let mut oversized = fixture;
        oversized.push('x');
        let rejected_tokenizations = AtomicUsize::new(0);
        let mut rejected_observer = ResidencyObserver::default();
        let error = execute(
            TextIteratorSource::new(std::iter::once(oversized)),
            |sentence| {
                rejected_tokenizations.fetch_add(1, Ordering::SeqCst);
                tokenize(sentence, &tokenizer)
            },
            &matcher,
            &pool,
            &limits,
            1_000,
            &CancellationFlag::new(),
            &mut DiscardSink,
            &mut |_: ExecutionProgress, _: &DiscardSink| {},
            &mut rejected_observer,
            lingrep_core::execution::PolicySelection::Apply,
            &SentenceAdmission::All,
        )
        .expect_err("64 KiB + 1 raw record must fail before tokenization");
        assert!(matches!(
            error,
            ExecutionError::SentenceTooLarge {
                actual,
                limit,
                ..
            } if actual == SENTENCE_BYTES + 1 && limit == SENTENCE_BYTES
        ));
        assert_eq!(rejected_tokenizations.load(Ordering::SeqCst), 0);
        assert!(rejected_observer.estimates.is_empty());
    }
}
