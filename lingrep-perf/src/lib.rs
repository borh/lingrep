//! Tooling for running performance campaigns on this repository.
//!
//! This crate exists to make the loop in
//! [`optimizing-performance`](../../docs/PERFORMANCE_REQUIREMENTS.md) cheaper
//! to follow, not to add a CI feature. Its consumer is whoever is holding an
//! optimization hypothesis.
//!
//! Two halves, deliberately split:
//!
//! * `work` — the counted-work screen. Requires the `screen` feature, because
//!   it needs `lingrep-core` compiled with `bench-instrumentation`. Deliberately not
//!   an intra-doc link: the module does not exist in a default build, and a
//!   link to it fails the `-D warnings` rustdoc lane there.
//! * The campaign adjudicator — a pure function of a campaign file, with no
//!   dependency on `lingrep-core` at all, so it stays unit-testable on synthetic
//!   campaigns and compiles in every build.
//!
//! `lingrep-core` remains optional because Cargo unifies features across workspace
//! members within one build. Taking
//! `lingrep-core` with `bench-instrumentation` unconditionally here would compile
//! execution-work counters into the shipping `ling` binary. Every other
//! crate in this workspace takes `lingrep-core/testing` as a *dev*-dependency to
//! avoid the same hazard.

pub mod campaign;
pub mod plan;
pub mod report;
pub mod stats;

#[cfg(feature = "screen")]
pub mod orchestrate;

#[cfg(feature = "screen")]
pub mod work;
