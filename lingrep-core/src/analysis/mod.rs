//! Analyzer-neutral admitted text and morphology values.

mod analyzer;
mod batch;
mod descriptor;
mod external;
mod lanes;
mod native;
mod provenance;
mod segmentation;
mod sudachi;
mod text;
mod vibrato;

pub use analyzer::{AnalysisError, AnalyzerPlan, AnalyzerSession};
pub use batch::{AnalyzedInput, AnalyzedSentence, SurfaceToken, ValidatedAnalysisBatch};
pub use descriptor::{
    AnalyzerDescriptor, AnalyzerFamily, ExecutionProvenance, ExtractionProfile, SegmentationMode,
    SemanticAnalysisIdentity, declared_token_schema,
};
pub(crate) use external::{ExternalBatchBounds, ResponseRecordKind, response_record_kind};
pub use external::{
    ExternalComponent, ExternalProtocolError, ExternalProtocolLimits, ExternalRunConfig,
    ExternalTranscript, ExternalValidatedBatch, ExternalWarning, external_component_closure_digest,
};
pub use lanes::AnalyzerLanes;
pub use native::{NativeAnalyzerPlan, NativeAnalyzerSession};
pub use provenance::TextUnitProvenance;
pub use segmentation::JapaneseSegmentationPolicy;
pub use sudachi::{
    SudachiConfig, SudachiEdition, SudachiLoadError, SudachiPlan, SudachiSession, SudachiSplit,
};
pub use text::{
    InputUnitId, NormalizationPolicy, SourceRange, TextUnit, TextUnitError, TextUnitIdentity,
    TextUnitKind,
};
pub use vibrato::{VibratoPlan, VibratoSession};
