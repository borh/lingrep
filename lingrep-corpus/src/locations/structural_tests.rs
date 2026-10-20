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
use std::sync::Arc;

use lingrep_core::spill::TempBudget;
use tempfile::tempdir;

use super::{LocationWriter, MappedLocations};
use crate::{AnalysisIdentity, CorpusError, DocumentId, SentenceLocation};

fn analysis(seed: u8) -> AnalysisIdentity {
    AnalysisIdentity::from_bytes([seed; 32])
}

fn location_writer(path: &std::path::Path) -> LocationWriter {
    LocationWriter::create(path, Arc::new(TempBudget::new(u64::MAX))).unwrap()
}

fn location(
    sentence: u64,
    document_id: u64,
    document_ordinal: u64,
    document_sentence_ordinal: u64,
    source_name: &str,
) -> SentenceLocation {
    SentenceLocation {
        source_metadata: None,
        corpus_sentence_ordinal: sentence,
        document_id: DocumentId::new(document_id),
        document_ordinal,
        document_sentence_ordinal,
        source_name: Arc::from(source_name),
    }
}

fn write_fixture(path: &std::path::Path, identity: AnalysisIdentity) {
    let mut writer = location_writer(path);
    writer.append(&location(0, 10, 0, 0, "first.md")).unwrap();
    writer.append(&location(1, 10, 0, 4, "first.md")).unwrap();
    writer.append(&location(2, 20, 1, 0, "second.md")).unwrap();
    writer.finish(identity).unwrap();
}

fn read_u64(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap())
}

fn rewrite_directory_checksum(bytes: &mut [u8]) {
    let directory_offset = usize::try_from(read_u64(bytes, 72)).unwrap();
    let checksum = *blake3::hash(&bytes[directory_offset..]).as_bytes();
    bytes[96..128].copy_from_slice(&checksum);
}

#[test]
fn writer_creation_preserves_an_existing_location_file() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("locations.v1");
    let original = b"already-published-locations";
    std::fs::write(&path, original).unwrap();

    assert!(matches!(
        LocationWriter::create(&path, Arc::new(TempBudget::new(u64::MAX))),
        Err(CorpusError::DestinationExists { .. })
    ));
    assert_eq!(std::fs::read(&path).unwrap(), original);
}

#[test]
fn mapped_locations_provide_random_access_and_document_names() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("locations.v1");
    let identity = analysis(1);
    write_fixture(&path, identity);
    let bytes = std::fs::read(&path).unwrap();
    assert_eq!(&bytes[0..8], b"LINGLOC1");
    assert_eq!(u32::from_le_bytes(bytes[8..12].try_into().unwrap()), 1);

    let mapped = MappedLocations::open(&path, &identity).unwrap();
    assert_eq!(mapped.sentence_count(), 3);
    assert_eq!(mapped.document_count(), 2);
    assert_eq!(mapped.sentence(1).unwrap().document_sentence_ordinal, 4);
    assert_eq!(
        mapped.sentence(2).unwrap().source_name.as_ref(),
        "second.md"
    );
    assert_eq!(
        mapped.document_name(DocumentId::new(10)).unwrap(),
        "first.md"
    );
    mapped.verify_integrity().unwrap();
}

#[test]
fn writer_rejects_new_document_ids_that_disagree_with_document_ordinal_order() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("locations.v1");
    let mut writer = location_writer(&path);
    writer.append(&location(0, 20, 0, 0, "first.md")).unwrap();

    assert!(matches!(
        writer.append(&location(1, 10, 1, 0, "second.md")),
        Err(CorpusError::InvalidFormat {
            component: "locations.v1",
            ..
        })
    ));
}

#[test]
fn writer_allows_documents_to_be_registered_before_sentence_streaming() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("locations.v1");
    let mut writer = location_writer(&path);
    writer
        .register_document_unresolved(DocumentId::new(10), 0, Arc::from("first.md"), 0, false)
        .unwrap();
    writer
        .register_document_unresolved(DocumentId::new(20), 1, Arc::from("second.md"), 1, false)
        .unwrap();

    writer.append(&location(0, 10, 0, 0, "first.md")).unwrap();
    writer.append(&location(1, 20, 1, 0, "second.md")).unwrap();
    writer.finish(analysis(1)).unwrap();

    let mapped = MappedLocations::open(&path, &analysis(1)).unwrap();
    assert_eq!(mapped.sentence(0).unwrap().source_name.as_ref(), "first.md");
    assert_eq!(
        mapped.sentence(1).unwrap().source_name.as_ref(),
        "second.md"
    );
}

#[test]
fn writer_allows_a_registered_document_to_have_zero_sentences() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("locations.v1");
    let mut writer = location_writer(&path);
    writer
        .register_document_unresolved(DocumentId::new(10), 0, Arc::from("empty.md"), 0, false)
        .unwrap();
    writer
        .register_document_unresolved(DocumentId::new(20), 1, Arc::from("nonempty.md"), 1, false)
        .unwrap();

    writer
        .append(&location(0, 20, 1, 0, "nonempty.md"))
        .unwrap();
    writer.finish(analysis(1)).unwrap();

    let mapped = MappedLocations::open(&path, &analysis(1)).unwrap();
    assert_eq!(mapped.document_count(), 2);
    assert_eq!(
        mapped.sentence(0).unwrap().source_name.as_ref(),
        "nonempty.md"
    );
}

#[test]
fn mapped_open_rejects_an_unexpected_analysis_identity() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("locations.v1");
    write_fixture(&path, analysis(1));

    assert!(matches!(
        MappedLocations::open(&path, &analysis(2)),
        Err(CorpusError::VerificationMismatch {
            component: "locations.v1",
            ..
        })
    ));
}

#[test]
fn mapped_open_rejects_a_header_count_inconsistent_with_its_sections() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("locations.v1");
    let corrupt = directory.path().join("wrong-count.v1");
    write_fixture(&path, analysis(1));
    let mut bytes = std::fs::read(&path).unwrap();
    bytes[48..56].copy_from_slice(&2_u64.to_le_bytes());
    std::fs::write(&corrupt, bytes).unwrap();

    assert!(matches!(
        MappedLocations::open(&corrupt, &analysis(1)),
        Err(CorpusError::InvalidFormat {
            component: "locations.v1",
            ..
        })
    ));
}

#[test]
fn mapped_open_rejects_a_truncated_directory() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("locations.v1");
    let truncated = directory.path().join("truncated.v1");
    write_fixture(&path, analysis(1));
    let bytes = std::fs::read(&path).unwrap();
    std::fs::write(&truncated, &bytes[..128]).unwrap();

    assert!(matches!(
        MappedLocations::open(&truncated, &analysis(1)),
        Err(CorpusError::Truncated { .. })
    ));
}

#[test]
fn mapped_sentence_detects_a_corrupt_payload_lazily() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("locations.v1");
    let corrupt = directory.path().join("corrupt.v1");
    write_fixture(&path, analysis(1));
    let mut bytes = std::fs::read(&path).unwrap();
    bytes[128] ^= 1;
    std::fs::write(&corrupt, bytes).unwrap();

    let mapped = MappedLocations::open(&corrupt, &analysis(1)).unwrap();
    assert!(matches!(
        mapped.sentence(0),
        Err(CorpusError::VerificationMismatch {
            component: "locations.v1",
            ..
        })
    ));
}

#[test]
fn verifying_one_block_does_not_excuse_a_later_corrupt_one() {
    // Two full blocks plus a remainder, so a block other than the first is
    // reached and the last one is short.
    const RECORDS_PER_BLOCK: u64 = 1024;
    let total = RECORDS_PER_BLOCK * 2 + 5;
    let directory = tempdir().unwrap();
    let path = directory.path().join("locations.v1");
    let mut writer = location_writer(&path);
    for ordinal in 0..total {
        writer
            .append(&location(ordinal, 10, 0, ordinal, "first.md"))
            .unwrap();
    }
    writer.finish(analysis(1)).unwrap();

    // Corrupt one record inside the second block. 128 is the record section
    // offset; the block's payload starts one block of records in.
    let mut bytes = std::fs::read(&path).unwrap();
    let second_block_payload = 128 + usize::try_from(RECORDS_PER_BLOCK * 24).unwrap();
    bytes[second_block_payload + 40] ^= 1;
    let corrupt = directory.path().join("corrupt-second-block.v1");
    std::fs::write(&corrupt, bytes).unwrap();

    let mapped = MappedLocations::open(&corrupt, &analysis(1)).unwrap();
    // Read every sentence of block 0 first, so the mapping has a verified block
    // recorded before it ever reaches the corrupt one.
    for ordinal in 0..RECORDS_PER_BLOCK {
        assert_eq!(
            mapped.sentence(ordinal).unwrap().corpus_sentence_ordinal,
            ordinal
        );
    }
    // Entering block 1 anywhere must still verify it. Start away from its first
    // record, since the checksum covers the block and not the record read.
    assert!(matches!(
        mapped.sentence(RECORDS_PER_BLOCK + 700),
        Err(CorpusError::VerificationMismatch {
            component: "locations.v1",
            ..
        })
    ));
    // And it stays refused rather than passing once a bit is set somewhere.
    assert!(matches!(
        mapped.sentence(RECORDS_PER_BLOCK),
        Err(CorpusError::VerificationMismatch { .. })
    ));
    // The untouched third block still reads.
    assert_eq!(
        mapped
            .sentence(RECORDS_PER_BLOCK * 2 + 1)
            .unwrap()
            .corpus_sentence_ordinal,
        RECORDS_PER_BLOCK * 2 + 1
    );
}

#[test]
fn full_integrity_verification_detects_valid_utf8_source_name_tampering() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("locations.v1");
    let corrupt = directory.path().join("corrupt-name.v1");
    write_fixture(&path, analysis(1));
    let mut bytes = std::fs::read(&path).unwrap();
    let last = bytes.len() - 1;
    bytes[last] = b'D';
    std::fs::write(&corrupt, bytes).unwrap();

    let mapped = MappedLocations::open(&corrupt, &analysis(1)).unwrap();
    assert_eq!(
        mapped.document_name(DocumentId::new(20)).unwrap(),
        "second.mD"
    );
    assert!(matches!(
        mapped.verify_integrity(),
        Err(CorpusError::VerificationMismatch {
            component: "locations.v1",
            ..
        })
    ));
}

#[test]
fn full_integrity_verification_rejects_substituted_valid_utf8_name_offset() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("locations.v1");
    let corrupt = directory.path().join("substituted-name-offset.v1");
    write_fixture(&path, analysis(1));
    let mut bytes = std::fs::read(&path).unwrap();
    let documents_offset = usize::try_from(read_u64(&bytes, 80)).unwrap();
    let strings_offset = read_u64(&bytes, 88);
    bytes[documents_offset + 16..documents_offset + 24]
        .copy_from_slice(&(strings_offset + 8).to_le_bytes());
    rewrite_directory_checksum(&mut bytes);
    std::fs::write(&corrupt, bytes).unwrap();

    let mapped = MappedLocations::open(&corrupt, &analysis(1)).unwrap();
    assert_eq!(
        mapped.document_name(DocumentId::new(10)).unwrap(),
        "second.m"
    );
    assert!(matches!(
        mapped.verify_integrity(),
        Err(CorpusError::InvalidFormat {
            component: "locations.v1",
            ..
        })
    ));
}
