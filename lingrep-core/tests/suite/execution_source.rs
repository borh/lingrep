// Exempt test/bench/example code from the workspace panic deny-floor (see docs/lint-policy.md).
#![allow(
    clippy::string_slice,
    clippy::panic,
    clippy::panic_in_result_fn,
    clippy::todo,
    clippy::unimplemented,
    clippy::unreachable,
    clippy::exit,
    clippy::unchecked_time_subtraction,
    clippy::unwrap_used
)]
use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use lingrep_core::analysis::{InputUnitId, NormalizationPolicy, TextUnitKind};
use lingrep_core::execution::{
    DEFAULT_MAX_STAGE_SENTENCES, ExecutionBatch, ExecutionError, ExecutionLimits,
    FallibleTextIteratorSource, FallibleTokenizedIteratorSource, SentenceBatchSource,
    TextIteratorSource, cached_run_attributes, estimate_token_info_bytes,
};
use lingrep_core::fsm::{CompileLimits, PatternSource, compile_catalog};
use lingrep_core::token_match::TokenInfo;

fn text_values(batch: ExecutionBatch) -> Vec<(u64, String)> {
    let ExecutionBatch::Text(sentences) = batch else {
        panic!("expected a text batch");
    };
    sentences
        .into_iter()
        .map(|sentence| {
            (
                sentence.location.corpus_sentence_ordinal,
                sentence.value.text().to_owned(),
            )
        })
        .collect()
}

fn tokenized_values(batch: ExecutionBatch) -> Vec<(u64, Vec<TokenInfo>)> {
    let ExecutionBatch::Tokenized { sentences, .. } = batch else {
        panic!("expected a tokenized batch");
    };
    sentences
        .into_iter()
        .map(|sentence| (sentence.location.corpus_sentence_ordinal, sentence.value))
        .collect()
}

fn with_stage_sentence_limit(limit: usize) -> ExecutionLimits {
    ExecutionLimits {
        max_stage_sentences: limit,
        ..ExecutionLimits::default()
    }
}

#[test]
fn empty_text_source_returns_none() {
    let mut source = TextIteratorSource::new(std::iter::empty());

    assert!(
        source
            .next_batch(&ExecutionLimits::default())
            .unwrap()
            .is_none()
    );
}

#[test]
fn direct_text_sources_admit_sentence_units_without_normalization() {
    let decomposed = "e\u{301}";
    let mut source = TextIteratorSource::new([decomposed.to_owned()].into_iter());
    let batch = source
        .next_batch(&ExecutionLimits::default())
        .unwrap()
        .unwrap();
    let ExecutionBatch::Text(sentences) = batch else {
        panic!("expected a text batch");
    };
    let [sentence] = sentences.as_slice() else {
        panic!("expected one sentence");
    };

    assert_eq!(sentence.value.id(), InputUnitId::new(0));
    assert_eq!(sentence.value.kind(), TextUnitKind::Sentence);
    assert_eq!(sentence.value.normalization(), NormalizationPolicy::None);
    assert_eq!(sentence.value.text().as_bytes(), decomposed.as_bytes());
}

#[test]
fn ordinals_are_contiguous_across_batch_boundaries() {
    let mut source = TextIteratorSource::new(
        ["zero", "one", "two", "three", "four"]
            .into_iter()
            .map(str::to_owned),
    );
    let limits = with_stage_sentence_limit(2);
    let mut seen = Vec::new();

    while let Some(batch) = source.next_batch(&limits).unwrap() {
        seen.extend(text_values(batch));
    }

    assert_eq!(
        seen,
        vec![
            (0, "zero".to_owned()),
            (1, "one".to_owned()),
            (2, "two".to_owned()),
            (3, "three".to_owned()),
            (4, "four".to_owned()),
        ]
    );
}

#[test]
fn text_stages_stop_at_the_sentence_count_cap() {
    // Expressed against the constant rather than its value: this asserts that
    // the count cap binds and where it splits, which is what the source
    // promises. Decimal ordinals are a few bytes each, so the byte cap stays
    // far away and the count is the only thing that can end the batch.
    let cap = DEFAULT_MAX_STAGE_SENTENCES;
    let mut source = TextIteratorSource::new((0..=cap).map(|index| index.to_string()));
    let limits = ExecutionLimits::default();

    let first = text_values(source.next_batch(&limits).unwrap().unwrap());
    let second = text_values(source.next_batch(&limits).unwrap().unwrap());

    assert_eq!(first.len(), cap);
    assert_eq!(first.first().unwrap().0, 0);
    assert_eq!(first.last().unwrap().0, (cap - 1) as u64);
    assert_eq!(second, vec![(cap as u64, cap.to_string())]);
    assert!(source.next_batch(&limits).unwrap().is_none());
}

#[test]
fn text_stages_stop_at_the_byte_cap_and_keep_the_crossing_sentence_pending() {
    let limits = ExecutionLimits {
        max_sentence_bytes: 100 * 1024,
        max_stage_input_bytes: 128 * 1024,
        ..ExecutionLimits::default()
    };
    let first_value = "a".repeat(80 * 1024);
    let crossing_value = "界".repeat(20 * 1024);
    assert_eq!(crossing_value.len(), 60 * 1024);
    let mut source =
        TextIteratorSource::new([first_value.clone(), crossing_value.clone()].into_iter());

    let first = text_values(source.next_batch(&limits).unwrap().unwrap());
    let second = text_values(source.next_batch(&limits).unwrap().unwrap());

    assert_eq!(first, vec![(0, first_value)]);
    assert_eq!(second, vec![(1, crossing_value)]);
    assert!(source.next_batch(&limits).unwrap().is_none());
}

#[test]
fn a_sentence_at_the_default_limit_makes_progress_as_a_singleton() {
    let limits = ExecutionLimits {
        max_stage_input_bytes: 64 * 1024,
        ..ExecutionLimits::default()
    };
    let first_value = "x".repeat(64 * 1024);
    let second_value = "y".repeat(64 * 1024);
    let mut source =
        TextIteratorSource::new([first_value.clone(), second_value.clone()].into_iter());

    assert_eq!(
        text_values(source.next_batch(&limits).unwrap().unwrap()),
        vec![(0, first_value)]
    );
    assert_eq!(
        text_values(source.next_batch(&limits).unwrap().unwrap()),
        vec![(1, second_value)]
    );
    assert!(source.next_batch(&limits).unwrap().is_none());
}

#[test]
fn an_oversized_sentence_fails_before_it_can_be_tokenized() {
    let limits = ExecutionLimits::default();
    let mut source = TextIteratorSource::new(std::iter::once("x".repeat(64 * 1024 + 1)));

    assert!(matches!(
        source.next_batch(&limits),
        Err(ExecutionError::SentenceTooLarge {
            ordinal: 0,
            actual,
            limit,
            ref context,
        }) if actual == 64 * 1024 + 1 && limit == 64 * 1024 && context.is_empty()
    ));
}

#[test]
fn iterator_failure_after_a_valid_batch_is_terminal_and_retains_its_message() {
    let items = vec![
        Ok("valid".to_owned()),
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "reader exploded",
        )),
        Ok("must not be observed".to_owned()),
    ];
    let mut source = FallibleTextIteratorSource::new(items.into_iter());
    let limits = with_stage_sentence_limit(1);

    assert_eq!(
        text_values(source.next_batch(&limits).unwrap().unwrap()),
        vec![(0, "valid".to_owned())]
    );
    for _ in 0..2 {
        assert!(matches!(
            source.next_batch(&limits),
            Err(ExecutionError::Source {
                ordinal: 1,
                ref message,
            }) if message == "reader exploded"
        ));
    }
}

#[test]
fn fallible_text_source_returns_a_prefix_before_an_error_discovered_in_the_same_pull() {
    let advances = Arc::new(AtomicUsize::new(0));
    let items = vec![
        Ok("valid".to_owned()),
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "reader exploded",
        )),
        Ok("must not be observed".to_owned()),
    ];
    let iterator = items.into_iter().inspect({
        let advances = Arc::clone(&advances);
        move |_| {
            advances.fetch_add(1, Ordering::SeqCst);
        }
    });
    let mut source = FallibleTextIteratorSource::new(iterator);
    let limits = with_stage_sentence_limit(2);

    assert_eq!(
        text_values(source.next_batch(&limits).unwrap().unwrap()),
        vec![(0, "valid".to_owned())]
    );
    assert_eq!(
        advances.load(Ordering::SeqCst),
        2,
        "the source must discover the error while filling the first batch"
    );

    for _ in 0..2 {
        assert!(matches!(
            source.next_batch(&limits),
            Err(ExecutionError::Source {
                ordinal: 1,
                ref message,
            }) if message == "reader exploded"
        ));
        assert_eq!(
            advances.load(Ordering::SeqCst),
            2,
            "a latched error must neither become EOF nor advance the iterator"
        );
    }
}

#[test]
fn malformed_tokenized_json_is_an_error_not_eof() {
    let lines = ["[{}]", "not-json"];
    let iterator = lines
        .into_iter()
        .map(serde_json::from_str::<Vec<TokenInfo>>);
    let mut source = FallibleTokenizedIteratorSource::new(iterator);
    let limits = with_stage_sentence_limit(1);

    let first = tokenized_values(source.next_batch(&limits).unwrap().unwrap());
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].0, 0);
    assert_eq!(first[0].1, vec![TokenInfo::default()]);

    assert!(matches!(
        source.next_batch(&limits),
        Err(ExecutionError::Source {
            ordinal: 1,
            ref message,
        }) if message.contains("expected ident")
    ));
}

#[test]
fn fallible_tokenized_source_returns_a_prefix_before_the_next_pull_reports_an_error() {
    let advances = Arc::new(AtomicUsize::new(0));
    let items = vec![
        Ok(vec![TokenInfo::default()]),
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "decoder exploded",
        )),
        Ok(vec![TokenInfo::default()]),
    ];
    let iterator = items.into_iter().inspect({
        let advances = Arc::clone(&advances);
        move |_| {
            advances.fetch_add(1, Ordering::SeqCst);
        }
    });
    let mut source = FallibleTokenizedIteratorSource::new(iterator);
    let limits = with_stage_sentence_limit(2);

    assert_eq!(
        tokenized_values(source.next_batch(&limits).unwrap().unwrap()),
        vec![(0, vec![TokenInfo::default()])]
    );
    assert_eq!(
        advances.load(Ordering::SeqCst),
        1,
        "a tokenized pull must not retain an unchecked second vector"
    );

    for _ in 0..2 {
        assert!(matches!(
            source.next_batch(&limits),
            Err(ExecutionError::Source {
                ordinal: 1,
                ref message,
            }) if message == "decoder exploded"
        ));
        assert_eq!(
            advances.load(Ordering::SeqCst),
            2,
            "a latched error must neither become EOF nor advance the iterator"
        );
    }
}

#[test]
fn tokenized_source_validates_aggregate_bytes_before_retaining_another_vector() {
    let tokenized = |surface: &str| vec![TokenInfo::default().with_orth(surface.repeat(1_024))];
    let first = tokenized("a");
    let second = tokenized("b");
    let one_sentence_bytes = estimate_token_info_bytes(&first).unwrap();
    assert_eq!(
        estimate_token_info_bytes(&second).unwrap(),
        one_sentence_bytes
    );
    let advances = Arc::new(AtomicUsize::new(0));
    let iterator = vec![Ok::<_, io::Error>(first.clone()), Ok(second.clone())]
        .into_iter()
        .inspect({
            let advances = Arc::clone(&advances);
            move |_| {
                advances.fetch_add(1, Ordering::SeqCst);
            }
        });
    let mut source = FallibleTokenizedIteratorSource::new(iterator);
    let limits = ExecutionLimits {
        max_stage_sentences: 2,
        max_tokenized_bytes: one_sentence_bytes + 1,
        ..ExecutionLimits::default()
    };

    assert_eq!(
        tokenized_values(source.next_batch(&limits).unwrap().unwrap()),
        vec![(0, first)]
    );
    assert_eq!(
        advances.load(Ordering::SeqCst),
        1,
        "the next arbitrary vector must not be pulled into an over-budget aggregate"
    );
    assert_eq!(
        tokenized_values(source.next_batch(&limits).unwrap().unwrap()),
        vec![(1, second)]
    );
    assert_eq!(advances.load(Ordering::SeqCst), 2);
    assert!(source.next_batch(&limits).unwrap().is_none());
}

struct DropSentinel {
    drops: Arc<AtomicUsize>,
}

impl Drop for DropSentinel {
    fn drop(&mut self) {
        self.drops.fetch_add(1, Ordering::SeqCst);
    }
}

struct DropSentinelIterator<I> {
    iterator: I,
    drops: Arc<AtomicUsize>,
    current_payload: Option<DropSentinel>,
}

impl<I: Iterator> Iterator for DropSentinelIterator<I> {
    type Item = I::Item;

    fn next(&mut self) -> Option<Self::Item> {
        self.current_payload = None;
        let item = self.iterator.next();
        if item.is_some() {
            self.current_payload = Some(DropSentinel {
                drops: Arc::clone(&self.drops),
            });
        }
        item
    }
}

#[test]
fn next_pull_advances_the_iterator_and_drops_its_prior_item_state() {
    let drops = Arc::new(AtomicUsize::new(0));
    let iterator = DropSentinelIterator {
        iterator: ["first".to_owned(), "second".to_owned()].into_iter(),
        drops: Arc::clone(&drops),
        current_payload: None,
    };
    let mut source = TextIteratorSource::new(iterator);
    let limits = with_stage_sentence_limit(1);

    let first = source.next_batch(&limits).unwrap().unwrap();
    assert_eq!(drops.load(Ordering::SeqCst), 0);

    let second = source.next_batch(&limits).unwrap().unwrap();
    assert_eq!(drops.load(Ordering::SeqCst), 1);
    drop(first);
    drop(second);
}

#[test]
fn cached_run_attributes_include_matcher_requirements_and_orth() {
    let fsm = compile_catalog(
        PatternSource::named("attributes.lx", "verb := [upos=VERB]."),
        CompileLimits::default(),
    )
    .unwrap()
    .matcher;

    let attributes = cached_run_attributes(&fsm);

    assert!(attributes.iter().any(|key| key.as_str() == "upos"));
    assert!(attributes.iter().any(|key| key.as_str() == "text"));
}
