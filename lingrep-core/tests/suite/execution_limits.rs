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
use lingrep_core::execution::{
    DEFAULT_MAX_INPUT_RECORD_BYTES, DEFAULT_MAX_SENTENCE_BYTES, DEFAULT_MAX_STAGE_INPUT_BYTES,
    DEFAULT_MAX_STAGE_SENTENCES, DEFAULT_MAX_TOKENIZED_BYTES, ExecutionError, ExecutionLimits,
    estimate_token_info_bytes,
};
use lingrep_core::token_match::{TokenAttributeIndex, TokenInfo, TokenInfoTrait};

#[test]
fn defaults_and_validation_are_explicit() {
    let defaults = ExecutionLimits::default();
    assert_eq!(
        defaults,
        ExecutionLimits {
            max_sentence_bytes: DEFAULT_MAX_SENTENCE_BYTES,
            max_input_record_bytes: DEFAULT_MAX_INPUT_RECORD_BYTES,
            max_stage_sentences: DEFAULT_MAX_STAGE_SENTENCES,
            max_stage_input_bytes: DEFAULT_MAX_STAGE_INPUT_BYTES,
            max_tokenized_bytes: DEFAULT_MAX_TOKENIZED_BYTES,
        }
    );
    assert_eq!(DEFAULT_MAX_SENTENCE_BYTES, 64 * 1024);
    // Wide enough that the byte cap below normally decides the retained batch.
    assert_eq!(DEFAULT_MAX_STAGE_SENTENCES, 2048);
    assert_eq!(DEFAULT_MAX_STAGE_INPUT_BYTES, 128 * 1024);
    assert_eq!(DEFAULT_MAX_TOKENIZED_BYTES, 32 * 1024 * 1024);
    assert!(defaults.validate().is_ok());

    for field in [
        "max_sentence_bytes",
        "max_input_record_bytes",
        "max_stage_sentences",
        "max_stage_input_bytes",
        "max_tokenized_bytes",
    ] {
        let mut limits = defaults;
        match field {
            "max_sentence_bytes" => limits.max_sentence_bytes = 0,
            "max_input_record_bytes" => limits.max_input_record_bytes = 0,
            "max_stage_sentences" => limits.max_stage_sentences = 0,
            "max_stage_input_bytes" => limits.max_stage_input_bytes = 0,
            "max_tokenized_bytes" => limits.max_tokenized_bytes = 0,
            _ => unreachable!(),
        }

        assert!(matches!(
            limits.validate(),
            Err(ExecutionError::InvalidLimit { field: actual, .. }) if actual == field
        ));
    }

    let limits = ExecutionLimits {
        max_stage_input_bytes: defaults.max_sentence_bytes - 1,
        ..defaults
    };
    assert!(matches!(
        limits.validate(),
        Err(ExecutionError::InvalidLimit {
            field: "max_stage_input_bytes",
            ..
        })
    ));

    // A record holds at least one sentence, so the same rule binds the record
    // bound. It lives here rather than in a caller so that every caller — both
    // CLIs and any library consumer — gets it from one place.
    let limits = ExecutionLimits {
        max_input_record_bytes: defaults.max_sentence_bytes - 1,
        ..defaults
    };
    assert!(matches!(
        limits.validate(),
        Err(ExecutionError::InvalidLimit {
            field: "max_input_record_bytes",
            ..
        })
    ));
}

/// The record bound scopes a document; the stage bound scopes a batch. They
/// default apart, so neither can be silently read as the other.
#[test]
fn the_record_bound_and_the_stage_bound_are_distinct_quantities() {
    let defaults = ExecutionLimits::default();

    assert_eq!(
        defaults.max_input_record_bytes,
        DEFAULT_MAX_INPUT_RECORD_BYTES
    );
    assert_eq!(
        defaults.max_stage_input_bytes,
        DEFAULT_MAX_STAGE_INPUT_BYTES
    );
    assert_ne!(
        defaults.max_input_record_bytes,
        defaults.max_stage_input_bytes
    );

    // A record larger than a whole stage is admissible: it simply feeds several.
    let spanning = ExecutionLimits {
        max_input_record_bytes: defaults.max_stage_input_bytes * 8,
        ..defaults
    };
    assert!(spanning.validate().is_ok());
}

#[test]
fn empty_token_sentence_has_no_residency() {
    let sentence = Vec::<TokenInfo>::new();

    assert_eq!(estimate_token_info_bytes(&sentence).unwrap(), 0);
}

#[test]
fn default_token_residency_is_its_inline_layout() {
    let sentence = vec![TokenInfo::default()];

    assert_eq!(
        estimate_token_info_bytes(&sentence).unwrap(),
        std::mem::size_of_val(sentence.as_slice())
    );
}

#[test]
fn token_residency_includes_owned_attribute_capacity() {
    let token = TokenInfo::default()
        .with_orth("orthographic-form-that-is-longer-than-inline-storage")
        .with_lemma("lexical-form-that-is-longer-than-inline-storage")
        .with_pos("NOUN")
        .with_cform("conjugated-form-that-is-longer-than-inline-storage")
        .with_kana("kana-representation-that-is-longer-than-inline-storage")
        .with_tag("tag-representation-that-is-longer-than-inline-storage");
    let sentence = vec![token];

    assert_eq!(
        estimate_token_info_bytes(&sentence).unwrap(),
        expected_residency(&sentence)
    );
}

#[test]
fn token_residency_counts_shared_attribute_contents_for_each_token() {
    let shared = "shared-value-that-is-longer-than-inline-storage";
    let first = TokenInfo::default().with_orth(shared).with_lemma(shared);
    let second = TokenInfo::default()
        .with_orth(first.orth().unwrap())
        .with_lemma(first.lemma().unwrap());
    let sentence = vec![first, second];

    assert_eq!(
        estimate_token_info_bytes(&sentence).unwrap(),
        expected_residency(&sentence)
    );
}

#[cfg(target_pointer_width = "64")]
#[test]
fn token_info_residency_schema_changes_require_an_explicit_update() {
    assert_eq!(
        TokenAttributeIndex::ALL.len(),
        32,
        "update the residency estimator and probe evidence for the attribute schema"
    );
    assert_eq!(
        std::mem::size_of::<TokenInfo>(),
        768,
        "update the residency estimator and probe evidence for TokenInfo layout"
    );
}

fn expected_residency(sentence: &[TokenInfo]) -> usize {
    std::mem::size_of_val(sentence)
        + sentence
            .iter()
            .flat_map(|token| {
                TokenAttributeIndex::ALL
                    .into_iter()
                    .filter_map(|index| token.get(index))
            })
            .filter(|value| value.is_heap_allocated())
            .map(compact_str::CompactString::capacity)
            .sum::<usize>()
}
