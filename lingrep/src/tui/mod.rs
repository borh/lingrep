//! Model-view-update implementation of the interactive explorer.

pub mod cache;
mod command;
mod loop_;
mod message;
mod model;
mod paged_store;
mod types;
mod update;
pub mod watcher;
pub mod workers;

use lingrep_core::fsm::{
    CompileLimits, CompiledCatalog, PatternSource, compile_catalog, render_pattern_error,
};

pub(crate) fn compile_catalog_source(
    source_name: &str,
    source: &str,
) -> Result<CompiledCatalog, String> {
    compile_catalog(
        PatternSource::named(source_name, source),
        CompileLimits::default(),
    )
    .map(|output| {
        log::debug!("compiled lingrep patterns {source_name}: {}", output.stats);
        output.matcher
    })
    .map_err(|error| render_pattern_error(Some(source), &error))
}

pub(crate) use loop_::run;
pub use model::Model;
pub use types::RequestId;

pub(crate) enum ExplorerOutcome {
    Completed,
    Cancelled,
}
