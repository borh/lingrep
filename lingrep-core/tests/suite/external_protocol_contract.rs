#![allow(clippy::unwrap_used)]

use std::io::{self, BufRead, Cursor, Read};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use lingrep_core::analysis::{ExternalProtocolLimits, ExternalTranscript, InputUnitId};
use lingrep_core::execution::{
    ExecutionBatch, ExecutionLimits, ExternalAttestation, OfflineExternalSource,
    SentenceBatchSource,
};
use lingrep_core::token_match::{AttributeOrdinal, AttributeValueRef};

const ZERO: &str = "0000000000000000000000000000000000000000000000000000000000000000";
const ONE: &str = "0101010101010101010101010101010101010101010101010101010101010101";

fn header() -> String {
    format!(
        "{{\"record\":\"run_header\",\"protocol\":\"lingrep.external-morphology/v1\",\"run_id\":\"run-1\",\"family\":\"external\",\"profile\":\"morphology\",\"segmentation\":\"input\",\"normalization\":\"none\",\"language_policy\":\"ja\",\"boundary_policy\":\"input-v1\",\"projection\":[{{\"key\":\"text\",\"kind\":\"scalar\"}},{{\"key\":\"lemma\",\"kind\":\"scalar\"}}],\"implementation_digest\":\"{ZERO}\",\"resource_manifest_digest\":\"{ZERO}\",\"projection_policy_digest\":\"{ZERO}\",\"alignment_policy_digest\":\"{ZERO}\",\"component_closure\":[{{\"name\":\"adapter\",\"digest\":\"{ZERO}\"}}],\"output_config_digest\":\"{ZERO}\",\"protocol_limits\":{{\"max_record_bytes\":1048576,\"max_request_batch_bytes\":67108864,\"max_response_batch_bytes\":67108864,\"max_documents_per_batch\":2048,\"max_schema_keys\":255,\"max_schema_key_bytes\":256,\"max_warnings_per_sentence\":64,\"max_warning_message_bytes\":16384,\"max_stderr_tail_bytes\":65536}}}}\n"
    )
}

fn ready() -> String {
    format!(
        "{{\"record\":\"run_ready\",\"run_id\":\"run-1\",\"family\":\"external\",\"profile\":\"morphology\",\"segmentation\":\"input\",\"normalization\":\"none\",\"language_policy\":\"ja\",\"boundary_policy\":\"input-v1\",\"implementation_digest\":\"{ZERO}\",\"resource_manifest_digest\":\"{ZERO}\",\"projection_policy_digest\":\"{ZERO}\",\"alignment_policy_digest\":\"{ZERO}\",\"component_closure\":[{{\"name\":\"adapter\",\"digest\":\"{ZERO}\"}}],\"output_config_digest\":\"{ZERO}\",\"schema\":[{{\"key\":\"text\",\"kind\":\"scalar\"}},{{\"key\":\"lemma\",\"kind\":\"scalar\"}}],\"returned_projection\":[{{\"key\":\"text\",\"kind\":\"scalar\"}},{{\"key\":\"lemma\",\"kind\":\"scalar\"}}],\"capabilities\":[\"morphology\"],\"execution\":{{\"backend\":\"cpu\",\"runtime\":\"python-3\",\"lock_digest\":\"{ONE}\",\"workers\":1,\"batch_size\":1}}}}\n"
    )
}

fn batch() -> String {
    concat!(
            "{\"record\":\"batch_begin\",\"batch_id\":0,\"document_count\":1}\n",
            "{\"record\":\"document\",\"batch_id\":0,\"document_ordinal\":0,\"input_id\":7,\"kind\":\"sentence\",\"text\":\"猫。\",\"provenance\":{\"source_identity\":\"$ZERO\",\"document_identity\":9,\"paragraph_identity\":null,\"metadata_digest\":\"$ZERO\"}}\n",
            "{\"record\":\"batch_end\",\"batch_id\":0}\n",
            "{\"record\":\"sentence\",\"batch_id\":0,\"document_ordinal\":0,\"input_id\":7,\"sentence_ordinal\":0,\"source_range\":[0,6],\"surface_ranges\":[[0,3],[3,6]],\"alignment\":{\"kind\":\"identity\"},\"word_count\":2,\"values\":[\"猫\",\"猫\",\"。\",\"。\"],\"warnings\":[]}\n",
            "{\"record\":\"batch_complete\",\"batch_id\":0,\"sentence_counts\":[1]}\n"
        )
        .replace("$ZERO", ZERO)
}

fn valid_transcript() -> String {
    header() + &ready() + &batch()
}

fn mixed_transcript(values: &str) -> String {
    let header = header().replace(
        "{\"key\":\"lemma\",\"kind\":\"scalar\"}",
        "{\"key\":\"feat.Number\",\"kind\":\"string_set\"}",
    );
    let ready = ready().replace(
        "{\"key\":\"lemma\",\"kind\":\"scalar\"}",
        "{\"key\":\"feat.Number\",\"kind\":\"string_set\"}",
    );
    let batch = batch().replace("[\"猫\",\"猫\",\"。\",\"。\"]", values);
    header + &ready + &batch
}

#[test]
fn v2_set_values_round_trip_canonically_and_empty_sets_are_missing() {
    let mut transcript = ExternalTranscript::open(
        Cursor::new(mixed_transcript("[\"猫\",[\"Plur\",\"Sing\"],\"。\",[]]").into_bytes()),
        ExternalProtocolLimits::default(),
    )
    .unwrap();
    let batch = transcript.next_validated_batch().unwrap().unwrap();
    let words = batch.analysis().inputs()[0].sentences()[0].words();
    let first = words.token(0).unwrap();
    assert_eq!(
        first.value(AttributeOrdinal::new(1)),
        Some(AttributeValueRef::StringSet(&[
            "Plur".into(),
            "Sing".into()
        ]))
    );
    assert_eq!(
        words.token(1).unwrap().value(AttributeOrdinal::new(1)),
        None
    );
}

#[test]
fn v2_rejects_noncanonical_sets_and_schema_value_kind_mismatches() {
    for (values, category) in [
        ("[\"猫\",[\"Sing\",\"Plur\"],\"。\",null]", "schema"),
        ("[\"猫\",[\"Sing\",\"Sing\"],\"。\",null]", "schema"),
        ("[\"猫\",\"Sing\",\"。\",null]", "schema"),
        ("[[\"猫\"],[\"Sing\"],\"。\",null]", "schema"),
    ] {
        let mut transcript = ExternalTranscript::open(
            Cursor::new(mixed_transcript(values).into_bytes()),
            ExternalProtocolLimits::default(),
        )
        .unwrap();
        assert_eq!(
            transcript.next_validated_batch().unwrap_err().category(),
            category
        );
    }
}

struct CountingReader {
    inner: Cursor<Vec<u8>>,
    consumed: Arc<AtomicUsize>,
}

impl Read for CountingReader {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        let count = self.inner.read(output)?;
        self.consumed.fetch_add(count, Ordering::Relaxed);
        Ok(count)
    }
}

impl BufRead for CountingReader {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        self.inner.fill_buf()
    }

    fn consume(&mut self, amount: usize) {
        self.inner.consume(amount);
        self.consumed.fetch_add(amount, Ordering::Relaxed);
    }
}

#[test]
fn canonical_records_round_trip_with_required_lf() {
    let mut transcript = ExternalTranscript::open(
        Cursor::new(valid_transcript().into_bytes()),
        ExternalProtocolLimits::default(),
    )
    .unwrap();
    let batch = transcript.next_validated_batch().unwrap().unwrap();
    assert_eq!(
        batch.analysis().inputs()[0].input().id(),
        InputUnitId::new(7)
    );
    assert_eq!(batch.analysis().inputs()[0].input().text(), "猫。");
    assert_eq!(batch.analysis().inputs()[0].sentences()[0].words().len(), 2);
    assert!(transcript.next_validated_batch().unwrap().is_none());

    let mut missing_lf = valid_transcript();
    missing_lf.pop();
    let mut transcript = ExternalTranscript::open(
        Cursor::new(missing_lf.into_bytes()),
        ExternalProtocolLimits::default(),
    )
    .unwrap();
    let error = transcript.next_validated_batch().unwrap_err();
    assert_eq!(error.category(), "framing");
}

#[test]
fn one_batch_validation_does_not_read_into_the_next_batch() {
    let first = valid_transcript();
    let second = batch()
        .replace("\"batch_id\":0", "\"batch_id\":1")
        .replace("\"input_id\":7", "\"input_id\":8");
    let consumed = Arc::new(AtomicUsize::new(0));
    let reader = CountingReader {
        inner: Cursor::new((first.clone() + &second).into_bytes()),
        consumed: Arc::clone(&consumed),
    };
    let mut transcript =
        ExternalTranscript::open(reader, ExternalProtocolLimits::default()).unwrap();
    assert!(transcript.next_validated_batch().unwrap().is_some());
    assert_eq!(consumed.load(Ordering::Relaxed), first.len());
    assert!(transcript.next_validated_batch().unwrap().is_some());
}

#[test]
fn duplicate_keys_are_rejected() {
    let input = header().replacen(
        "\"protocol\":\"lingrep.external-morphology/v1\"",
        "\"protocol\":\"lingrep.external-morphology/v1\",\"protocol\":\"lingrep.external-morphology/v1\"",
        1,
    ) + &ready();
    let error = ExternalTranscript::open(
        Cursor::new(input.into_bytes()),
        ExternalProtocolLimits::default(),
    )
    .unwrap_err();
    assert_eq!(error.category(), "json");
}

#[test]
fn unknown_fields_and_versions_are_rejected() {
    for input in [
        header().replacen("\"run_id\"", "\"extra\":1,\"run_id\"", 1) + &ready(),
        header().replace(
            "lingrep.external-morphology/v1",
            "lingrep.external-morphology/v0",
        ) + &ready(),
    ] {
        let error = ExternalTranscript::open(
            Cursor::new(input.into_bytes()),
            ExternalProtocolLimits::default(),
        )
        .unwrap_err();
        assert!(matches!(error.category(), "json" | "protocol"));
    }
}

#[test]
fn record_limit_applies_before_json_decode() {
    let limits = ExternalProtocolLimits {
        max_record_bytes: 32,
        ..ExternalProtocolLimits::default()
    };
    let error = ExternalTranscript::open(Cursor::new(header().into_bytes()), limits).unwrap_err();
    assert_eq!(error.category(), "limit");
}

#[test]
fn premature_eof_and_batch_order_are_terminal() {
    let mut truncated = ExternalTranscript::open(
        Cursor::new(
            (header()
                + &ready()
                + "{\"record\":\"batch_begin\",\"batch_id\":0,\"document_count\":1}\n")
                .into_bytes(),
        ),
        ExternalProtocolLimits::default(),
    )
    .unwrap();
    let error = truncated.next_validated_batch().unwrap_err();
    assert_eq!(error.category(), "truncated");
    assert_eq!(error.batch_id(), Some(0));

    let reordered = batch().replacen("\"document_ordinal\":0", "\"document_ordinal\":1", 1);
    let mut transcript = ExternalTranscript::open(
        Cursor::new((header() + &ready() + &reordered).into_bytes()),
        ExternalProtocolLimits::default(),
    )
    .unwrap();
    let error = transcript.next_validated_batch().unwrap_err();
    assert_eq!(error.category(), "order");
    assert_eq!(error.input_id(), Some(InputUnitId::new(7)));
}

#[test]
fn utf8_ranges_and_projection_width_are_validated_before_commit() {
    let invalid_range = batch().replacen("\"source_range\":[0,6]", "\"source_range\":[0,1]", 1);
    let mut transcript = ExternalTranscript::open(
        Cursor::new((header() + &ready() + &invalid_range).into_bytes()),
        ExternalProtocolLimits::default(),
    )
    .unwrap();
    assert_eq!(
        transcript.next_validated_batch().unwrap_err().category(),
        "range"
    );

    let short_values =
        batch().replacen("[\"猫\",\"猫\",\"。\",\"。\"]", "[\"猫\",\"猫\",\"。\"]", 1);
    let mut transcript = ExternalTranscript::open(
        Cursor::new((header() + &ready() + &short_values).into_bytes()),
        ExternalProtocolLimits::default(),
    )
    .unwrap();
    assert_eq!(
        transcript.next_validated_batch().unwrap_err().category(),
        "alignment"
    );
}

#[test]
fn identity_alignment_binds_form_to_the_admitted_source_slice() {
    let mismatched = batch().replacen("[\"猫\",\"猫\",", "[\"犬\",\"猫\",", 1);
    let mut transcript = ExternalTranscript::open(
        Cursor::new((header() + &ready() + &mismatched).into_bytes()),
        ExternalProtocolLimits::default(),
    )
    .unwrap();
    assert_eq!(
        transcript.next_validated_batch().unwrap_err().category(),
        "alignment"
    );

    let expanded = mismatched.replace(
        "\"alignment\":{\"kind\":\"identity\"}",
        "\"alignment\":{\"kind\":\"expanded\",\"word_ranges\":[[0,1],[1,2]]}",
    );
    let mut transcript = ExternalTranscript::open(
        Cursor::new((header() + &ready() + &expanded).into_bytes()),
        ExternalProtocolLimits::default(),
    )
    .unwrap();
    assert!(transcript.next_validated_batch().unwrap().is_some());
}

#[test]
fn nfc_transcripts_require_document_text_to_already_be_canonical() {
    let header = header().replace("\"normalization\":\"none\"", "\"normalization\":\"nfc\"");
    let ready = ready().replace("\"normalization\":\"none\"", "\"normalization\":\"nfc\"");
    let decomposed_values = format!("[\"e{}\",\"e{}\"]", '\u{301}', '\u{301}');
    let decomposed = batch()
        .replace("\"text\":\"猫。\"", "\"text\":\"e\\u0301\"")
        .replace("\"source_range\":[0,6]", "\"source_range\":[0,3]")
        .replace(
            "\"surface_ranges\":[[0,3],[3,6]]",
            "\"surface_ranges\":[[0,3]]",
        )
        .replace("\"word_count\":2", "\"word_count\":1")
        .replace("[\"猫\",\"猫\",\"。\",\"。\"]", &decomposed_values);
    let mut transcript = ExternalTranscript::open(
        Cursor::new((header + &ready + &decomposed).into_bytes()),
        ExternalProtocolLimits::default(),
    )
    .unwrap();
    assert_eq!(
        transcript.next_validated_batch().unwrap_err().category(),
        "document"
    );
}

#[test]
fn declared_document_count_is_bounded_before_capacity_allocation() {
    let limits = ExternalProtocolLimits {
        max_documents_per_batch: 2,
        ..ExternalProtocolLimits::default()
    };
    let bounded_header = header().replacen(
        "\"max_documents_per_batch\":2048",
        "\"max_documents_per_batch\":2",
        1,
    );
    let attack =
        "{\"record\":\"batch_begin\",\"batch_id\":0,\"document_count\":18446744073709551615}\n";
    let mut transcript = ExternalTranscript::open(
        Cursor::new((bounded_header + &ready() + attack).into_bytes()),
        limits,
    )
    .unwrap();
    assert_eq!(
        transcript.next_validated_batch().unwrap_err().category(),
        "limit"
    );
}

#[test]
fn request_and_file_digests_have_distinct_ownership() {
    let first = valid_transcript();
    let second = first.replace("\"backend\":\"cpu\"", "\"backend\":\"gpu\"");
    let changed_request = first
        .replacen("\"text\":\"猫。\"", "\"text\":\"犬。\"", 1)
        .replacen("[\"猫\",\"猫\",", "[\"犬\",\"犬\",", 1);

    let mut digests = Vec::new();
    for bytes in [first, second, changed_request] {
        let mut transcript = ExternalTranscript::open(
            Cursor::new(bytes.into_bytes()),
            ExternalProtocolLimits::default(),
        )
        .unwrap();
        transcript.next_validated_batch().unwrap().unwrap();
        assert!(transcript.whole_file_digest().is_none());
        assert!(transcript.next_validated_batch().unwrap().is_none());
        digests.push((
            transcript.request_digest(),
            transcript.whole_file_digest().unwrap(),
        ));
    }
    assert_eq!(digests[0].0, digests[1].0);
    assert_ne!(digests[0].1, digests[1].1);
    assert_ne!(digests[0].0, digests[2].0);
    assert_eq!(
        digests[0].0,
        [
            203, 96, 84, 240, 160, 241, 136, 93, 172, 154, 174, 117, 48, 57, 254, 154, 44, 247, 67,
            1, 221, 106, 180, 42, 104, 111, 95, 158, 20, 54, 188, 0,
        ]
    );
}

#[test]
fn warnings_are_preserved_but_not_interpreted() {
    let warned = batch().replace(
        "\"warnings\":[]",
        "\"warnings\":[{\"code\":\"example.lossy\",\"input_id\":7,\"message\":\"provider note\"}]",
    );
    let mut transcript = ExternalTranscript::open(
        Cursor::new((header() + &ready() + &warned).into_bytes()),
        ExternalProtocolLimits::default(),
    )
    .unwrap();
    let batch = transcript.next_validated_batch().unwrap().unwrap();
    assert_eq!(batch.warnings()[0].code.as_ref(), "example.lossy");
    assert_eq!(batch.warnings()[0].message.as_ref(), "provider note");
}

#[test]
fn offline_source_flattens_only_a_committed_attested_batch() {
    let attestation = ExternalAttestation::new("research-import", [9; 32]).unwrap();
    let mut source = OfflineExternalSource::open(
        Cursor::new(valid_transcript().into_bytes()),
        attestation,
        ExternalProtocolLimits::default(),
    )
    .unwrap();
    assert_eq!(source.trust_class(), "externally-attested");
    assert!(!source.wants_lookahead());

    let batch = source
        .next_batch(&ExecutionLimits::default())
        .unwrap()
        .unwrap();
    let ExecutionBatch::BoundTokenized { sentences, charges } = batch else {
        panic!("offline morphology must yield schema-bound sentences");
    };
    assert_eq!(sentences.len(), 1);
    assert_eq!(sentences[0].location.corpus_sentence_ordinal, 0);
    assert_eq!(sentences[0].location.document_id.get(), 9);
    assert_eq!(
        sentences[0].location.source_name.as_ref(),
        "research-import"
    );
    assert_eq!(sentences[0].value.words().len(), 2);
    assert_eq!(charges.len(), 1);
    assert!(
        source
            .next_batch(&ExecutionLimits::default())
            .unwrap()
            .is_none()
    );
    assert!(source.whole_file_digest().is_some());
}

#[test]
fn empty_batches_and_globally_reordered_input_ids_are_rejected() {
    let empty = concat!(
        "{\"record\":\"batch_begin\",\"batch_id\":0,\"document_count\":0}\n",
        "{\"record\":\"batch_end\",\"batch_id\":0}\n",
        "{\"record\":\"batch_complete\",\"batch_id\":0,\"sentence_counts\":[]}\n",
    );
    let mut transcript = ExternalTranscript::open(
        Cursor::new((header() + &ready() + empty).into_bytes()),
        ExternalProtocolLimits::default(),
    )
    .unwrap();
    assert!(transcript.next_validated_batch().is_err());

    let second = batch()
        .replace("\"batch_id\":0", "\"batch_id\":1")
        .replace("\"input_id\":7", "\"input_id\":6");
    let mut transcript = ExternalTranscript::open(
        Cursor::new((valid_transcript() + &second).into_bytes()),
        ExternalProtocolLimits::default(),
    )
    .unwrap();
    transcript.next_validated_batch().unwrap().unwrap();
    let error = transcript.next_validated_batch().unwrap_err();
    assert_eq!(error.category(), "order");
    assert_eq!(error.input_id(), Some(InputUnitId::new(6)));
}

#[test]
fn every_wire_limit_owner_rejects_its_own_overage() {
    let cases = [
        (
            "max_request_batch_bytes",
            "67108864",
            "64",
            ExternalProtocolLimits {
                max_request_batch_bytes: 64,
                ..ExternalProtocolLimits::default()
            },
            batch(),
        ),
        (
            "max_documents_per_batch",
            "2048",
            "1",
            ExternalProtocolLimits {
                max_documents_per_batch: 1,
                ..ExternalProtocolLimits::default()
            },
            batch().replacen("\"document_count\":1", "\"document_count\":2", 1),
        ),
        (
            "max_response_batch_bytes",
            "67108864",
            "64",
            ExternalProtocolLimits {
                max_response_batch_bytes: 64,
                ..ExternalProtocolLimits::default()
            },
            batch(),
        ),
        (
            "max_schema_keys",
            "255",
            "1",
            ExternalProtocolLimits {
                max_schema_keys: 1,
                ..ExternalProtocolLimits::default()
            },
            batch(),
        ),
        (
            "max_schema_key_bytes",
            "256",
            "3",
            ExternalProtocolLimits {
                max_schema_key_bytes: 3,
                ..ExternalProtocolLimits::default()
            },
            batch(),
        ),
        (
            "max_warnings_per_sentence",
            "64",
            "0",
            ExternalProtocolLimits {
                max_warnings_per_sentence: 0,
                ..ExternalProtocolLimits::default()
            },
            batch().replace(
                "\"warnings\":[]",
                "\"warnings\":[{\"code\":\"example.note\",\"input_id\":7,\"message\":\"x\"}]",
            ),
        ),
        (
            "max_warning_message_bytes",
            "16384",
            "4",
            ExternalProtocolLimits {
                max_warning_message_bytes: 4,
                ..ExternalProtocolLimits::default()
            },
            batch().replace(
                "\"warnings\":[]",
                "\"warnings\":[{\"code\":\"example.note\",\"input_id\":7,\"message\":\"oversized\"}]",
            ),
        ),
    ];

    for (field, old, new, limits, batch) in cases {
        let header = header().replacen(
            &format!("\"{field}\":{old}"),
            &format!("\"{field}\":{new}"),
            1,
        );
        let result = ExternalTranscript::open(
            Cursor::new((header + &ready() + &batch).into_bytes()),
            limits,
        )
        .and_then(|mut transcript| transcript.next_validated_batch().map(|_| transcript));
        let error = result.unwrap_err();
        assert_eq!(error.category(), "limit", "wrong category for {field}");
    }
}

#[test]
fn offline_source_applies_execution_input_record_limit_to_document_records() {
    let attestation = ExternalAttestation::new("bounded-import", [3; 32]).unwrap();
    let mut source = OfflineExternalSource::open(
        Cursor::new(valid_transcript().into_bytes()),
        attestation,
        ExternalProtocolLimits::default(),
    )
    .unwrap();
    let limits = ExecutionLimits {
        max_sentence_bytes: 100,
        max_input_record_bytes: 100,
        max_stage_sentences: 4,
        max_stage_input_bytes: 100,
        max_tokenized_bytes: 4096,
    };
    let error = source.next_batch(&limits).unwrap_err();
    assert!(error.to_string().contains("input record bytes"));
    assert!(source.next_batch(&ExecutionLimits::default()).is_err());
}
