//! ling: CLI frontend for pattern matching

pub(crate) mod attribute_binding;
mod bccwj_seam;
pub(crate) mod catalog_input;
pub mod cli;
pub mod cli_surface;
pub mod commands;
pub mod corpus_mode;
pub mod dictionary_binding;
pub mod direct_source;
pub mod error;
pub mod examples;
mod interactive_execution;
pub mod manpages;
pub mod output;
pub mod packaged_dictionaries;
pub mod pattern_reference;
pub(crate) mod policy_report;
mod prepared_source;
pub mod presentation;
pub mod protocol;
pub mod scope_precompute;
mod section_diagnostics;
mod soranoha_seam;
pub mod summary;
pub mod terminal;
pub mod tui;

pub use cli::Cli;
pub use error::CliError;
pub use protocol::write_jsonl;
mod analyzer_selection;
