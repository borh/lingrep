//! Exact per-(pattern, section) sentence counting.

mod producer;
mod span_events;
pub mod surface;
mod tally;

pub use producer::spawn_span_producer;
pub use span_events::{SpanEvent, SpanEvents, SpanSource, SpanStreamError};
pub use surface::{
    SURFACE_CEILING_BYTES, SURFACE_VOCABULARY_LIMIT, SurfaceRefusal, SurfaceSections, SurfaceTally,
};
pub use tally::{
    RETAINED_CEILING_BYTES, SectionAvailability, SectionRefusal, SectionSlot, SectionTally,
    SectionTallyBuilder, SectionTallyError, admit, build,
};
