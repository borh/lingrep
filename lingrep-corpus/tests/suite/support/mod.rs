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
