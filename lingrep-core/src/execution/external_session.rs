use super::{
    DiagnosticCallback, ExecutionLimits, ExternalAdapterCancellation, ExternalAdapterCommand,
    ExternalAdapterProcess, VerifiedAdapterManifest,
};
use compact_str::CompactString;

use crate::analysis::{
    AnalysisError, AnalysisView, AnalyzedInput, AnalyzedSentence, AnalyzerDescriptor,
    AnalyzerSession, ExternalProtocolLimits, ExternalRunConfig, SourceBoundaries, SurfaceToken,
    TEXT_KEY_REVISION, TextUnit, UNPROTECTED, ValidatedAnalysisBatch, ViewFold, analyzer::sealed,
    text_key,
};
use crate::token_match::{
    AttributeKey, AttributeKind, AttributeValueRef, BoundProjection, BoundTokenSentence,
    ProjectedAttributeValue, SchemaAttribute, TokenSchema, TokenSchemaError, TokenSequence,
};

const SURFACE_PROJECTION_REVISION: &[u8] = b"lingrep.external-surface-projection/v1";

/// The token schema lingrep publishes for an adapter whose schema is
/// `adapter`: the same attributes, plus `surface` when the adapter produces
/// `text`.
///
/// # Errors
///
/// Returns [`TokenSchemaError::DuplicateKey`] or
/// [`TokenSchemaError::ConflictingKind`] when the adapter itself declares
/// `surface` alongside `text`.
pub fn published_schema(adapter: &TokenSchema) -> Result<TokenSchema, TokenSchemaError> {
    if !adapter.keys().iter().any(|key| key.as_str() == "text") {
        return Ok(adapter.clone());
    }
    TokenSchema::new(adapter.attributes().iter().cloned().chain([
        SchemaAttribute::new("surface", AttributeKind::Scalar).expect("surface is a canonical key"),
    ]))
}

/// The identity of analysis through [`ExternalAnalyzerSession`] by an adapter
/// run under `adapter`, publishing `schema` from [`published_schema`].
#[must_use]
pub fn published_descriptor(
    adapter: &AnalyzerDescriptor,
    schema: &TokenSchema,
) -> AnalyzerDescriptor {
    let mut projection = blake3::Hasher::new();
    for part in [
        SURFACE_PROJECTION_REVISION,
        TEXT_KEY_REVISION,
        &ViewFold::rules_identity(),
        adapter.projection_policy_digest(),
    ] {
        projection.update(&(part.len() as u64).to_le_bytes());
        projection.update(part);
    }
    AnalyzerDescriptor::new(
        adapter.family(),
        adapter.profile(),
        adapter.segmentation(),
        adapter.normalization(),
        adapter.language_policy(),
        adapter.boundary_policy(),
        *adapter.implementation_digest(),
        *adapter.resource_manifest_digest(),
        *projection.finalize().as_bytes(),
        *adapter.alignment_policy_digest(),
        *adapter.component_closure_digest(),
        *adapter.output_config_digest(),
        schema.digest(),
    )
    .expect("the adapter descriptor's policies are nonempty")
}

/// Owns one verified process with a fixed projection until completion or drop.
///
/// The adapter reads the [`ViewFold::unprotected`] view of each input. Its
/// token ranges are mapped back to the input, each word that is a whole
/// source token publishes that source slice as `surface`, a word of a
/// multiword token publishes the adapter's `text` as `surface`, and `text` is
/// the folded key of `surface`.
pub struct ExternalAnalyzerSession {
    command: ExternalAdapterCommand,
    manifest: VerifiedAdapterManifest,
    descriptor: AnalyzerDescriptor,
    adapter_schema: TokenSchema,
    schema: TokenSchema,
    limits: ExecutionLimits,
    diagnostic: DiagnosticCallback,
    cancellation: ExternalAdapterCancellation,
    process: Option<ExternalAdapterProcess>,
    projection: Option<BoundProjection>,
    finished: bool,
}

impl ExternalAnalyzerSession {
    /// `descriptor` and `schema` describe the adapter's own output; the
    /// session publishes [`published_schema`] of `schema`.
    ///
    /// # Errors
    ///
    /// As [`published_schema`].
    pub fn new(
        command: ExternalAdapterCommand,
        manifest: VerifiedAdapterManifest,
        descriptor: AnalyzerDescriptor,
        schema: TokenSchema,
        limits: ExecutionLimits,
        diagnostic: DiagnosticCallback,
        cancellation: ExternalAdapterCancellation,
    ) -> Result<Self, TokenSchemaError> {
        Ok(Self {
            command,
            manifest,
            descriptor,
            schema: published_schema(&schema)?,
            adapter_schema: schema,
            limits,
            diagnostic,
            cancellation,
            process: None,
            projection: None,
            finished: false,
        })
    }

    /// The adapter's projection for a published `projection`: every key but
    /// `surface`, and `text` whenever the adapter produces it.
    fn adapter_projection(
        &self,
        projection: &BoundProjection,
    ) -> Result<BoundProjection, AnalysisError> {
        let keys = projection
            .keys()
            .iter()
            .filter(|key| key.as_str() != "surface")
            .cloned()
            .chain(
                self.adapter_schema
                    .keys()
                    .iter()
                    .filter(|key| key.as_str() == "text")
                    .cloned(),
            );
        self.adapter_schema
            .bind_projection(keys)
            .map_err(|_| AnalysisError::UnsupportedProjection)
    }

    fn publish(
        &self,
        analysis: ValidatedAnalysisBatch,
        inputs: Vec<TextUnit>,
        views: Vec<Option<AnalysisView>>,
        projection: &BoundProjection,
    ) -> Result<ValidatedAnalysisBatch, AnalysisError> {
        let analyzed = analysis.into_inputs().into_vec();
        if analyzed.len() != inputs.len() {
            return Err(AnalysisError::InvalidAlignment);
        }
        let mut published = Vec::with_capacity(inputs.len());
        for ((analyzed, input), view) in analyzed.into_iter().zip(inputs).zip(views) {
            let (adapter_input, sentences) = analyzed.into_parts();
            if adapter_input.id() != input.id() {
                return Err(AnalysisError::InvalidAlignment);
            }
            let mut boundaries = view
                .as_ref()
                .map(|view| SourceBoundaries::new(input.text(), view));
            let mut to_source = |byte: u32| {
                boundaries
                    .as_mut()
                    .map_or(byte as usize, |map| map.source_byte(byte as usize))
            };
            let mut published_sentences = Vec::with_capacity(sentences.len());
            for sentence in sentences {
                let start = to_source(sentence.source_range().start());
                let surfaces = sentence
                    .surface_tokens()
                    .iter()
                    .map(|token| {
                        let range = token.source_range();
                        let start = to_source(range.start());
                        let end = to_source(range.end());
                        let range = input
                            .source_range(start, end)
                            .map_err(|_| AnalysisError::InvalidSourceRange)?;
                        Ok(SurfaceToken::new(
                            range,
                            token.word_range().start,
                            token.word_range().end,
                        ))
                    })
                    .collect::<Result<Vec<_>, AnalysisError>>()?;
                let end = to_source(sentence.source_range().end());
                let range = input
                    .source_range(start, end)
                    .map_err(|_| AnalysisError::InvalidSourceRange)?;
                let ordinal = sentence.analyzer_sentence_ordinal();
                let words = publish_words(&sentence.into_words(), &surfaces, &input, projection)?;
                published_sentences.push(AnalyzedSentence::new(
                    ordinal,
                    range,
                    surfaces.into_boxed_slice(),
                    words,
                )?);
            }
            published.push(AnalyzedInput::new(
                input,
                published_sentences,
                self.descriptor.segmentation(),
            )?);
        }
        ValidatedAnalysisBatch::new(published)
    }
}

/// `words` in the adapter's projection, as `projection` publishes them over
/// `surfaces`, which address `input`.
fn publish_words(
    words: &BoundTokenSentence,
    surfaces: &[SurfaceToken],
    input: &TextUnit,
    projection: &BoundProjection,
) -> Result<BoundTokenSentence, AnalysisError> {
    let text = AttributeKey::new("text").expect("text is canonical");
    let mut values = Vec::with_capacity(words.len() * projection.len());
    for surface_token in surfaces {
        let range = surface_token.word_range();
        for word in range.start as usize..range.end as usize {
            let surface = if range.len() == 1 {
                Some(
                    input
                        .slice(surface_token.source_range())
                        .map_err(|_| AnalysisError::InvalidSourceRange)?,
                )
            } else {
                words.scalar_key(word, &text)
            };
            for (key, kind) in projection.keys().iter().zip(projection.kinds()) {
                values.push(match key.as_str() {
                    "surface" => ProjectedAttributeValue::Scalar(surface.map(CompactString::from)),
                    "text" => ProjectedAttributeValue::Scalar(
                        surface.map(|surface| CompactString::from(text_key(surface))),
                    ),
                    _ => match words.value_key(word, key) {
                        Some(AttributeValueRef::Scalar(value)) => {
                            ProjectedAttributeValue::scalar(value)
                        }
                        Some(AttributeValueRef::StringSet(value)) => {
                            ProjectedAttributeValue::string_set(value.iter().cloned())
                        }
                        None => match kind {
                            AttributeKind::Scalar => ProjectedAttributeValue::missing_scalar(),
                            AttributeKind::StringSet => ProjectedAttributeValue::string_set(
                                std::iter::empty::<CompactString>(),
                            ),
                        },
                    },
                });
            }
        }
    }
    BoundTokenSentence::from_projected_attribute_values(projection, words.len(), values)
        .map_err(|_| AnalysisError::ResourceAccounting)
}

impl sealed::Session for ExternalAnalyzerSession {}
impl AnalyzerSession for ExternalAnalyzerSession {
    fn schema(&self) -> &TokenSchema {
        &self.schema
    }
    fn analyze_bounded_batch(
        &mut self,
        inputs: Vec<TextUnit>,
        projection: &BoundProjection,
    ) -> Result<ValidatedAnalysisBatch, AnalysisError> {
        if self.finished {
            return Err(AnalysisError::ProviderFailure(
                "external analyzer session has finished".into(),
            ));
        }
        if projection.schema_digest() != self.schema.digest()
            || self
                .projection
                .as_ref()
                .is_some_and(|bound| bound != projection)
        {
            return Err(AnalysisError::UnsupportedProjection);
        }
        if self.process.is_none() {
            let adapter_projection = self.adapter_projection(projection)?;
            let config = ExternalRunConfig::new(
                "ling-cli",
                self.descriptor.clone(),
                self.adapter_schema.clone(),
                adapter_projection,
                self.manifest.components().to_vec(),
                ExternalProtocolLimits::default(),
            )
            .map_err(AnalysisError::provider)?;
            let process = ExternalAdapterProcess::launch_with_cancellation(
                self.command.clone(),
                self.manifest.clone(),
                config,
                self.diagnostic.clone(),
                self.cancellation.clone(),
            )
            .map_err(AnalysisError::provider)?;
            self.process = Some(process);
            self.projection = Some(projection.clone());
        }
        let mut views = Vec::with_capacity(inputs.len());
        let mut adapter_inputs = Vec::with_capacity(inputs.len());
        for input in &inputs {
            let view = UNPROTECTED.fold(input.text());
            if view.text() == input.text() {
                adapter_inputs.push(input.clone());
                views.push(None);
            } else {
                adapter_inputs.push(
                    TextUnit::admit_with_provenance(
                        input.id(),
                        input.kind(),
                        view.text(),
                        input.normalization(),
                        input.provenance().clone(),
                    )
                    .map_err(|_| AnalysisError::InvalidSourceRange)?,
                );
                views.push(Some(view));
            }
        }
        let result = self
            .process
            .as_mut()
            .expect("launched process")
            .exchange_batch(adapter_inputs, &self.limits);
        match result {
            Ok(batch) => {
                let (analysis, warnings, _) = batch.into_parts();
                for warning in warnings {
                    (self.diagnostic)(format!("{warning:?}").as_bytes())
                        .map_err(AnalysisError::ProviderFailure)?;
                }
                self.publish(analysis, inputs, views, projection)
            }
            Err(error) => {
                self.finished = true;
                self.process.take();
                Err(AnalysisError::provider(error))
            }
        }
    }
    fn finish(&mut self) -> Result<(), AnalysisError> {
        self.finished = true;
        if let Some(process) = self.process.take() {
            process.finish().map_err(AnalysisError::provider)?;
        }
        Ok(())
    }
}
