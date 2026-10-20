#![allow(clippy::panic, clippy::unwrap_used)]
use std::path::PathBuf;

use lingrep_core::corpus::SourceDocument;

pub fn source_document(
    ordinal: u64,
    name: &str,
    target_group_ordinal: u64,
    external_target: bool,
) -> SourceDocument {
    SourceDocument {
        ordinal,
        logical_name: name.into(),
        path: PathBuf::from(format!("/unopened/{name}")),
        target_group_ordinal,
        external_target,
    }
}

/// Restores retired published bytes and checks the rebuild boundary.
pub fn assert_retired_artifact_requires_rebuild(bytes: &[u8], name: &str, generation: u32) {
    let files: std::collections::BTreeMap<String, Vec<u8>> =
        serde_json::from_slice(&zstd::stream::decode_all(bytes).unwrap()).unwrap();
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join(name);
    std::fs::create_dir(&root).unwrap();
    for (file, bytes) in files {
        std::fs::write(root.join(file), bytes).unwrap();
    }
    let error = match lingrep_corpus::CorpusArtifact::open(&root) {
        Ok(_) => panic!("retired generation {generation} was admitted"),
        Err(error) => error,
    };
    let message = error.to_string();
    assert!(
        message.contains(&format!("generation {generation}")),
        "{message}"
    );
    assert!(message.contains("rebuild from source"), "{message}");
}
