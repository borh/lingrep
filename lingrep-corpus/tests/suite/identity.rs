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
use lingrep_core::analysis::{
    AnalyzerDescriptor, AnalyzerFamily, ExtractionProfile, NormalizationPolicy, SegmentationMode,
};
use lingrep_core::corpus::{
    CorpusReadConfig, admission_identity_digest, reader_identity_digest,
    token_schema_identity_digest,
};
use lingrep_corpus::{
    AdmissionIdentity, AnalysisInputs, IdentityError, ReaderIdentity, SourceObservation,
    derive_analysis_identity, vibrato_fixture_descriptor,
};

const SOURCE_DOMAIN: &[u8] = b"lingrep-corpus/source-identity/v1";
const SOURCE_BYTES_DOMAIN: &[u8] = b"lingrep-corpus/source-bytes/v1";
const INVENTORY_DOMAIN: &[u8] = b"lingrep-corpus/source-inventory/v1";
const ANALYSIS_DOMAIN: &[u8] = b"lingrep-corpus/analysis-identity/v1";

fn bytes<const N: usize>(value: u8) -> [u8; N] {
    [value; N]
}

fn update_prefix(hasher: &mut blake3::Hasher, domain: &[u8]) {
    hasher.update(&u32::try_from(domain.len()).unwrap().to_le_bytes());
    hasher.update(domain);
    hasher.update(&1_u32.to_le_bytes());
}

fn source_content_digest(content: &[u8]) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    update_prefix(&mut hasher, SOURCE_BYTES_DOMAIN);
    hasher.update(content);
    *hasher.finalize().as_bytes()
}

fn manually_framed_source_identity(sources: &[(&str, &[u8])]) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    update_prefix(&mut hasher, SOURCE_DOMAIN);
    for (name, content) in sources {
        hasher.update(&[1]);
        hasher.update(&u64::try_from(name.len()).unwrap().to_le_bytes());
        hasher.update(name.as_bytes());
        hasher.update(&u64::try_from(content.len()).unwrap().to_le_bytes());
        hasher.update(&source_content_digest(content));
    }
    hasher.update(&[255]);
    hasher.update(&u64::try_from(sources.len()).unwrap().to_le_bytes());
    *hasher.finalize().as_bytes()
}

fn manually_framed_inventory(sources: &[(&str, &[u8])]) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    update_prefix(&mut hasher, INVENTORY_DOMAIN);
    for (name, _) in sources {
        hasher.update(&[1]);
        hasher.update(&u64::try_from(name.len()).unwrap().to_le_bytes());
        hasher.update(name.as_bytes());
    }
    hasher.update(&[255]);
    hasher.update(&u64::try_from(sources.len()).unwrap().to_le_bytes());
    *hasher.finalize().as_bytes()
}

fn observe(sources: &[(&str, &[&[u8]])]) -> lingrep_corpus::ObservedSources {
    let observation = SourceObservation::new();
    for (name, chunks) in sources {
        observation.begin_source(name).unwrap();
        for chunk in *chunks {
            observation.update_source_bytes(chunk).unwrap();
        }
        observation.end_source().unwrap();
    }
    observation.finish().unwrap()
}

fn inputs() -> AnalysisInputs {
    AnalysisInputs::new(
        ReaderIdentity::from_bytes(bytes(1)),
        AdmissionIdentity::from_bytes(bytes(2)),
        &vibrato_fixture_descriptor(),
    )
}

/// A descriptor differing from the fixture only in the named resource digest.
///
/// Proves that a resource change moves the identity, and so does an output
/// change with identical resources.
fn descriptor_with(resources: [u8; 32], output: [u8; 32]) -> AnalyzerDescriptor {
    AnalyzerDescriptor::new(
        AnalyzerFamily::Vibrato,
        ExtractionProfile::Morphology,
        SegmentationMode::Input,
        NormalizationPolicy::Nfc,
        "ja",
        "input-unit",
        [0xf1; 32],
        resources,
        [0xf3; 32],
        [0xf4; 32],
        [0xf5; 32],
        output,
        lingrep_core::token_match::TokenSchema::unidic_morphology().digest(),
    )
    .unwrap()
}

#[test]
fn source_identity_rows_cover_framing_sensitivity_and_chunking() {
    let observed = observe(&[("a.md", &[b"x"]), ("b.md", &[b"yz"])]);
    let sources = [("a.md", b"x".as_slice()), ("b.md", b"yz".as_slice())];

    assert_eq!(
        observed.source_identity().as_bytes(),
        &manually_framed_source_identity(&sources),
    );
    assert_eq!(
        observed.ordered_inventory_digest(),
        &manually_framed_inventory(&sources),
    );
    assert_eq!(observed.document_count(), 2);

    let original = observe(&[("a.md", &[b"abcd"])]);
    for (name, candidate) in [
        ("source rename", observe(&[("b.md", &[b"abcd"])])),
        ("source bytes", observe(&[("a.md", &[b"abce"])])),
    ] {
        assert_ne!(
            original.source_identity(),
            candidate.source_identity(),
            "source identity must discriminate {name}"
        );
    }

    for (name, candidate) in [
        ("two-plus-two chunks", observe(&[("a.md", &[b"ab", b"cd"])])),
        (
            "one-plus-three chunks",
            observe(&[("a.md", &[b"a", b"bcd"])]),
        ),
    ] {
        assert_eq!(
            original.source_identity(),
            candidate.source_identity(),
            "source identity changed for {name}"
        );
        assert_eq!(
            original.ordered_inventory_digest(),
            candidate.ordered_inventory_digest(),
            "inventory identity changed for {name}"
        );
        assert_eq!(
            derive_analysis_identity(&original, &inputs()),
            derive_analysis_identity(&candidate, &inputs()),
            "analysis identity changed for {name}"
        );
    }
}

#[test]
fn analysis_identity_rows_cover_framing_and_semantic_sensitivity() {
    let observed = observe(&[("a.md", &[b"x"])]);
    let inputs = inputs();
    let actual = derive_analysis_identity(&observed, &inputs);
    let mut hasher = blake3::Hasher::new();
    update_prefix(&mut hasher, ANALYSIS_DOMAIN);
    hasher.update(observed.source_identity().as_bytes());
    hasher.update(inputs.reader().as_bytes());
    hasher.update(inputs.admission().as_bytes());
    hasher.update(inputs.analyzer().as_bytes());
    hasher.update(inputs.token_schema().as_bytes());
    let expected = *hasher.finalize().as_bytes();

    assert_eq!(actual.as_bytes(), &expected);

    let changed_inputs = [
        (
            "reader",
            inputs
                .clone()
                .with_reader(ReaderIdentity::from_bytes(bytes(9))),
        ),
        (
            "admission",
            AnalysisInputs::new(
                inputs.reader(),
                AdmissionIdentity::from_bytes(bytes(9)),
                &vibrato_fixture_descriptor(),
            ),
        ),
        (
            "analyzer resources",
            AnalysisInputs::new(
                inputs.reader(),
                inputs.admission(),
                &descriptor_with(bytes(9), [0xf6; 32]),
            ),
        ),
        // A change to output configuration alone, with every resource
        // identical. This is the
        // shape of a Sudachi split mode.
        (
            "analyzer output configuration",
            AnalysisInputs::new(
                inputs.reader(),
                inputs.admission(),
                &descriptor_with([0xf2; 32], bytes(9)),
            ),
        ),
    ];
    for (name, changed) in changed_inputs {
        assert_ne!(
            actual,
            derive_analysis_identity(&observed, &changed),
            "analysis identity must discriminate {name}"
        );
    }
}

#[test]
fn blake3_full_width_byte_order_is_canonical() {
    assert_eq!(
        blake3::hash(b"").to_hex().as_str(),
        "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262"
    );
}

#[test]
fn shared_semantic_identity_digests_use_versioned_blake3_framing() {
    let mut reader = blake3::Hasher::new();
    reader.update(b"lingrep/corpus-reader/v1");
    reader.update(&CorpusReadConfig::Plain.reader_tag().to_le_bytes());
    reader.update(&[1]);
    assert_eq!(
        reader_identity_digest(&CorpusReadConfig::Plain),
        *reader.finalize().as_bytes()
    );
    assert_eq!(
        admission_identity_digest(),
        *blake3::hash(b"lingrep/sentence-admission/v2").as_bytes()
    );

    // The identity binds the complete ordered symbolic schema, including
    // each value kind.
    assert_eq!(
        token_schema_identity_digest(),
        [
            2, 88, 235, 113, 222, 159, 89, 131, 201, 91, 138, 159, 169, 252, 75, 210, 58, 136, 63,
            0, 135, 161, 108, 242, 111, 217, 166, 246, 185, 237, 167, 204,
        ]
    );
}

#[test]
fn observation_rejects_invalid_state_transitions_and_finalized_updates() {
    let observation = SourceObservation::new();
    assert_eq!(
        observation.update_source_bytes(b"x"),
        Err(IdentityError::InvalidState)
    );
    observation.begin_source("a.md").unwrap();
    assert_eq!(
        observation.begin_source("b.md"),
        Err(IdentityError::InvalidState)
    );
    assert!(matches!(
        observation.finish(),
        Err(IdentityError::InvalidState)
    ));
    observation.end_source().unwrap();
    observation.finish().unwrap();
    assert_eq!(observation.end_source(), Err(IdentityError::InvalidState));
    assert_eq!(
        observation.begin_source("resurrected.md"),
        Err(IdentityError::InvalidState)
    );
    assert_eq!(
        observation.update_source_bytes(b"x"),
        Err(IdentityError::InvalidState)
    );
}
fn observe_with_context(
    context: &[(&str, &[u8])],
    sources: &[(&str, &[&[u8]])],
) -> lingrep_corpus::ObservedSources {
    let observation = SourceObservation::new();
    for (name, bytes) in context {
        observation.observe_context_file(name, bytes).unwrap();
    }
    for (name, chunks) in sources {
        observation.begin_source(name).unwrap();
        for chunk in *chunks {
            observation.update_source_bytes(chunk).unwrap();
        }
        observation.end_source().unwrap();
    }
    observation.finish().unwrap()
}

#[test]
fn context_files_do_not_move_the_document_identity() {
    // No context files: byte-identical to the pre-existing framing.
    let bare = observe(&[("a.md", &[b"raw" as &[u8]])]);
    let with_none = observe_with_context(&[], &[("a.md", &[b"raw" as &[u8]])]);
    assert_eq!(
        bare.source_identity().as_bytes(),
        with_none.source_identity().as_bytes()
    );
    assert_eq!(
        bare.ordered_inventory_digest(),
        with_none.ordered_inventory_digest()
    );
    assert_eq!(bare.document_count(), with_none.document_count());

    // Context files: the name, the order and the bytes all reach
    // source_identity, but not document_count and not the inventory digest.
    let two = observe_with_context(
        &[("DOC/Joined_info.txt", b"j"), ("DOC/BCCWJ-NDC.txt", b"n")],
        &[("a.md", &[b"raw" as &[u8]])],
    );
    assert_ne!(
        bare.source_identity().as_bytes(),
        two.source_identity().as_bytes()
    );
    assert_eq!(
        bare.ordered_inventory_digest(),
        two.ordered_inventory_digest()
    );
    assert_eq!(two.document_count(), 1);

    // Reordering the context files changes the identity.
    let swapped = observe_with_context(
        &[("DOC/BCCWJ-NDC.txt", b"n"), ("DOC/Joined_info.txt", b"j")],
        &[("a.md", &[b"raw" as &[u8]])],
    );
    assert_ne!(
        two.source_identity().as_bytes(),
        swapped.source_identity().as_bytes()
    );

    // Changing the bytes or the name changes the identity.
    let changed_bytes = observe_with_context(
        &[("DOC/Joined_info.txt", b"J"), ("DOC/BCCWJ-NDC.txt", b"n")],
        &[("a.md", &[b"raw" as &[u8]])],
    );
    assert_ne!(
        two.source_identity().as_bytes(),
        changed_bytes.source_identity().as_bytes()
    );
    let renamed = observe_with_context(
        &[("DOC/other.txt", b"j"), ("DOC/BCCWJ-NDC.txt", b"n")],
        &[("a.md", &[b"raw" as &[u8]])],
    );
    assert_ne!(
        two.source_identity().as_bytes(),
        renamed.source_identity().as_bytes()
    );
}

#[test]
fn context_files_are_invalid_once_any_document_starts() {
    let observation = SourceObservation::new();
    observation.observe_context_file("DOC/a.txt", b"x").unwrap();
    observation.observe_context_file("DOC/b.txt", b"y").unwrap();
    observation.begin_source("a.md").unwrap();
    assert_eq!(
        observation.observe_context_file("DOC/c.txt", b"z"),
        Err(IdentityError::InvalidState)
    );
    // Even back in the Ready phase after end_source: "before the first
    // begin_source" means before the first one, ever.
    observation.update_source_bytes(b"raw").unwrap();
    observation.end_source().unwrap();
    assert_eq!(
        observation.observe_context_file("DOC/c.txt", b"z"),
        Err(IdentityError::InvalidState)
    );
    // A finished observation rejects everything.
    observation.finish().unwrap();
    assert_eq!(
        observation.observe_context_file("DOC/c.txt", b"z"),
        Err(IdentityError::InvalidState)
    );
}
