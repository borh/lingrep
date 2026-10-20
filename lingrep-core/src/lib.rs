#[cfg(all(
    not(target_env = "msvc"),
    not(test),
    not(feature = "external-allocator")
))]
use tikv_jemallocator::Jemalloc;

#[cfg(all(
    not(target_env = "msvc"),
    not(test),
    not(feature = "external-allocator")
))]
#[global_allocator]
static GLOBAL: Jemalloc = Jemalloc;

pub mod analysis;
pub mod corpus;
pub mod dictionary;
pub mod error;
pub mod execution;
pub mod fsm;
pub mod scope;
pub mod spill;
pub mod token_match;
pub mod utils;

#[cfg(any(test, feature = "testing"))]
pub mod testing;

pub use error::PipelineError;
