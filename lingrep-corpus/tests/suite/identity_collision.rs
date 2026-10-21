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
//! Characterization of the live prepared-corpus identity collision.
//!
//! Two reachable Sudachi flag spellings — `--sudachi-split` and the
//! segmentation mode — change what a build *emits* while leaving the
//! analysis identity, and therefore the canonical artifact path, unmoved.
//! `resolve_destination` then hands back an artifact segmented differently
//! from the one requested.
//!
//! The assertions here are written against the *fixed* behavior: analysis
//! identity keys on the complete analyzer semantic identity, so two
//! descriptors that differ semantically address two artifacts.

use lingrep_core::analysis::{
    ExtractionProfile, NormalizationPolicy, SegmentationMode, SudachiConfig, SudachiEdition,
    SudachiResources, SudachiSplit,
};
use lingrep_corpus::{
    AdmissionIdentity, AnalysisIdentity, AnalysisInputs, AnalyzerRecord, ReaderIdentity,
    SourceObservation, TokenSchemaIdentity, TrustClass, canonical_artifact_path,
    derive_analysis_identity,
};

fn config(split: SudachiSplit, segmentation: SegmentationMode) -> SudachiConfig {
    SudachiConfig::new(
        "/dictionaries/system.dic",
        SudachiResources::directory("/dictionaries/resources"),
        [11; 32],
        [12; 32],
        SudachiEdition::Core,
        split,
        ExtractionProfile::Morphology,
        segmentation,
        NormalizationPolicy::Nfc,
    )
    .unwrap()
}

fn observed() -> lingrep_corpus::ObservedSources {
    let observation = SourceObservation::new();
    observation.begin_source("corpus.jsonl").unwrap();
    observation.update_source_bytes(b"one sentence").unwrap();
    observation.end_source().unwrap();
    observation.finish().unwrap()
}

fn inputs(config: &SudachiConfig) -> AnalysisInputs {
    AnalysisInputs::new(
        ReaderIdentity::from_bytes([1; 32]),
        AdmissionIdentity::from_bytes([2; 32]),
        config.descriptor(),
    )
}

fn identity(config: &SudachiConfig) -> AnalysisIdentity {
    derive_analysis_identity(&observed(), &inputs(config))
}

#[test]
fn sudachi_split_modes_derive_distinct_analysis_identities() {
    let a = config(SudachiSplit::A, SegmentationMode::Input);
    let c = config(SudachiSplit::C, SegmentationMode::Input);

    // The descriptor already distinguished these; only the address did not.
    assert_ne!(
        a.semantic_identity(),
        c.semantic_identity(),
        "split reaches output_config_digest and always has"
    );
    assert_ne!(
        identity(&a).as_bytes(),
        identity(&c).as_bytes(),
        "two split modes over one source must not share an analysis identity"
    );
}

#[test]
fn sudachi_segmentation_modes_derive_distinct_analysis_identities() {
    let input = config(SudachiSplit::C, SegmentationMode::Input);
    let analyzer = config(SudachiSplit::C, SegmentationMode::Analyzer);

    assert_ne!(input.semantic_identity(), analyzer.semantic_identity());
    assert_ne!(
        identity(&input).as_bytes(),
        identity(&analyzer).as_bytes(),
        "two segmentation modes over one source must not share an analysis identity"
    );
}

#[test]
fn distinct_identities_yield_distinct_canonical_paths() {
    let root = std::path::Path::new("/data/corpus.jsonl");
    let a = canonical_artifact_path(
        root,
        &identity(&config(SudachiSplit::A, SegmentationMode::Input)),
        TrustClass::LocallyExecuted,
    );
    let c = canonical_artifact_path(
        root,
        &identity(&config(SudachiSplit::C, SegmentationMode::Input)),
        TrustClass::LocallyExecuted,
    );
    assert_ne!(a, c);
}

#[test]
fn recorded_analyzer_record_distinguishes_what_the_address_now_distinguishes() {
    // The manifest already stored the distinguishing fact; nothing read it.
    let a = AnalyzerRecord::from_descriptor(
        config(SudachiSplit::A, SegmentationMode::Input).descriptor(),
    );
    let c = AnalyzerRecord::from_descriptor(
        config(SudachiSplit::C, SegmentationMode::Input).descriptor(),
    );
    assert_ne!(a.semantic_identity, c.semantic_identity);
}

#[test]
fn token_schema_must_agree_with_the_descriptor_schema() {
    let config = config(SudachiSplit::C, SegmentationMode::Input);
    let inputs = inputs(&config);
    assert_eq!(
        inputs.token_schema().as_bytes(),
        config.schema().digest().as_bytes(),
        "the structural storage input is the descriptor's own schema digest"
    );
    assert!(
        AnalysisInputs::with_token_schema(
            ReaderIdentity::from_bytes([1; 32]),
            AdmissionIdentity::from_bytes([2; 32]),
            config.descriptor(),
            TokenSchemaIdentity::from_bytes([9; 32]),
        )
        .is_err(),
        "a token schema that disagrees with the descriptor cannot be constructed"
    );
}
