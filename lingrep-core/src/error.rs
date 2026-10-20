//! Errors shared across `lingrep-core` boundaries.

use std::io;
use std::path::PathBuf;

/// Library-level failure.
#[derive(thiserror::Error, Debug)]
pub enum PipelineError {
    #[error("Dictionary error: {0}")]
    Dictionary(String),

    /// I/O failure and the path being accessed.
    #[error("I/O error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("Invalid data format: {0}")]
    InvalidData(String),
}

/// Bridge `PipelineError` into `io::Error` so that `io::Result`-returning entry points
/// (e.g. `fn main() -> io::Result<()>` in the CLI crates) can propagate a
/// dictionary-load failure with `?` instead of mapping by hand at each call.
///
/// This is a lossy conversion (the structured `PipelineError` is rendered as a string
/// and wrapped in `io::ErrorKind::Other`); use `PipelineError` directly when the
/// structured type is available.
impl From<PipelineError> for io::Error {
    fn from(e: PipelineError) -> Self {
        io::Error::other(e)
    }
}
