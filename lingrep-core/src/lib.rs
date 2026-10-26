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
