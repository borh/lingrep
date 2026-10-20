//! Internal facade for the NFA executor and spawn machinery.

pub(crate) use super::dense::DenseMachine;
pub(crate) use super::exec::ExecScratch;
pub use super::exec::Matcher;
pub(crate) use super::spawn::{SpawnDecline, SpawnTable};
