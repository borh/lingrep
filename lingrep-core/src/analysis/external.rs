use std::collections::BTreeSet;
use std::fmt;
use std::io::{BufRead, Read, Write};

use compact_str::CompactString;
use serde::de::{DeserializeSeed, Error as _, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use crate::token_match::{
    AttributeKey, AttributeKind, BoundProjection, BoundTokenSentence, ProjectedAttributeValue,
    SchemaAttribute, TokenSchema,
};

use super::{
    AnalyzedInput, AnalyzedSentence, AnalyzerDescriptor, AnalyzerFamily, ExecutionProvenance,
    ExtractionProfile, InputUnitId, NormalizationPolicy, SegmentationMode, SurfaceToken, TextUnit,
    TextUnitKind, TextUnitProvenance, ValidatedAnalysisBatch,
};

const PROTOCOL: &str = "lingrep.external-morphology/v1";
const REQUEST_DIGEST_DOMAIN: &[u8] = b"lingrep.external-morphology/request/v1";

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ExternalProtocolLimits {
    pub max_record_bytes: usize,
    pub max_request_batch_bytes: usize,
    pub max_response_batch_bytes: usize,
    pub max_documents_per_batch: usize,
    pub max_schema_keys: usize,
    pub max_schema_key_bytes: usize,
    pub max_warnings_per_sentence: usize,
    pub max_warning_message_bytes: usize,
    pub max_stderr_tail_bytes: usize,
}

impl Default for ExternalProtocolLimits {
    fn default() -> Self {
        Self {
            max_record_bytes: 1 << 20,
            max_request_batch_bytes: 64 << 20,
            max_response_batch_bytes: 64 << 20,
            max_documents_per_batch: 2048,
            max_schema_keys: u8::MAX as usize,
            max_schema_key_bytes: 256,
            max_warnings_per_sentence: 64,
            max_warning_message_bytes: 16 << 10,
            max_stderr_tail_bytes: 64 << 10,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExternalProtocolError {
    category: &'static str,
    message: String,
    batch_id: Option<u64>,
    input_id: Option<InputUnitId>,
}

impl ExternalProtocolError {
    fn new(category: &'static str, message: impl Into<String>) -> Self {
        Self {
            category,
            message: message.into(),
            batch_id: None,
            input_id: None,
        }
    }

    fn batch(mut self, batch_id: u64) -> Self {
        self.batch_id = Some(batch_id);
        self
    }

    fn input(mut self, input_id: u64) -> Self {
        self.input_id = Some(InputUnitId::new(input_id));
        self
    }

    pub(crate) fn new_for_execution(
        category: &'static str,
        message: impl Into<String>,
        batch_id: Option<u64>,
    ) -> Self {
        let mut error = Self::new(category, message);
        error.batch_id = batch_id;
        error
    }

    #[must_use]
    pub const fn category(&self) -> &'static str {
        self.category
    }

    #[must_use]
    pub const fn batch_id(&self) -> Option<u64> {
        self.batch_id
    }

    #[must_use]
    pub const fn input_id(&self) -> Option<InputUnitId> {
        self.input_id
    }
}

impl fmt::Display for ExternalProtocolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "external morphology {} error", self.category)?;
        if let Some(batch_id) = self.batch_id {
            write!(formatter, " in batch {batch_id}")?;
        }
        if let Some(input_id) = self.input_id {
            write!(formatter, " for input {}", input_id.get())?;
        }
        write!(formatter, ": {}", self.message)
    }
}

impl std::error::Error for ExternalProtocolError {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExternalWarning {
    pub code: Box<str>,
    pub input_id: InputUnitId,
    pub message: Box<str>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExternalComponent {
    name: String,
    digest: [u8; 32],
}

impl ExternalComponent {
    pub fn new(name: impl Into<String>, digest: [u8; 32]) -> Result<Self, &'static str> {
        let name = name.into();
        if name.is_empty() {
            return Err("external component name must be nonempty");
        }
        Ok(Self { name, digest })
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub const fn digest(&self) -> &[u8; 32] {
        &self.digest
    }
}

#[must_use]
pub fn external_component_closure_digest(components: &[ExternalComponent]) -> [u8; 32] {
    let mut ordered = components.to_vec();
    ordered.sort_by(|left, right| left.name.cmp(&right.name));
    let mut hash = blake3::Hasher::new();
    hash.update(b"lingrep.external-morphology/component-closure/v1");
    hash.update(&(ordered.len() as u64).to_le_bytes());
    for component in ordered {
        hash.update(&(component.name.len() as u64).to_le_bytes());
        hash.update(component.name.as_bytes());
        hash.update(&component.digest);
    }
    *hash.finalize().as_bytes()
}

#[derive(Clone, Debug)]
pub struct ExternalRunConfig {
    header: RunHeader,
    schema: TokenSchema,
    projection: BoundProjection,
    limits: ExternalProtocolLimits,
}

impl ExternalRunConfig {
    pub fn new(
        run_id: impl Into<String>,
        descriptor: AnalyzerDescriptor,
        schema: TokenSchema,
        projection: BoundProjection,
        mut components: Vec<ExternalComponent>,
        limits: ExternalProtocolLimits,
    ) -> Result<Self, ExternalProtocolError> {
        validate_limits(limits)?;
        let run_id = run_id.into();
        if components.len() > limits.max_schema_keys
            || components
                .iter()
                .any(|component| component.name.len() > limits.max_schema_key_bytes)
        {
            return Err(ExternalProtocolError::new(
                "limit",
                "component closure exceeds configured name/count limits",
            ));
        }
        let schema_attributes = wire_schema(schema.attributes());
        validate_schema_attributes(&schema_attributes, limits)?;
        if descriptor.schema_digest() != schema.digest()
            || projection.schema_digest() != schema.digest()
        {
            return Err(ExternalProtocolError::new(
                "schema",
                "run descriptor, schema, and projection must share one digest",
            ));
        }
        components.sort_by(|left, right| left.name.cmp(&right.name));
        if components
            .windows(2)
            .any(|pair| pair[0].name == pair[1].name)
        {
            return Err(ExternalProtocolError::new(
                "protocol",
                "external component names must be unique",
            ));
        }
        if descriptor.component_closure_digest() != &external_component_closure_digest(&components)
        {
            return Err(ExternalProtocolError::new(
                "handshake",
                "analyzer descriptor does not bind the component closure",
            ));
        }
        let header = RunHeader {
            record: RunHeaderTag::RunHeader,
            protocol: PROTOCOL.to_owned(),
            run_id,
            family: descriptor.family().into(),
            profile: descriptor.profile().into(),
            segmentation: descriptor.segmentation().into(),
            normalization: descriptor.normalization().into(),
            language_policy: descriptor.language_policy().to_owned(),
            boundary_policy: descriptor.boundary_policy().to_owned(),
            projection: wire_projection(&projection),
            implementation_digest: Digest(*descriptor.implementation_digest()),
            resource_manifest_digest: Digest(*descriptor.resource_manifest_digest()),
            projection_policy_digest: Digest(*descriptor.projection_policy_digest()),
            alignment_policy_digest: Digest(*descriptor.alignment_policy_digest()),
            output_config_digest: Digest(*descriptor.output_config_digest()),
            component_closure: components
                .into_iter()
                .map(|component| WireComponent {
                    name: component.name,
                    digest: Digest(component.digest),
                })
                .collect(),
            protocol_limits: limits,
        };
        validate_header(&header, limits)?;
        Ok(Self {
            header,
            schema,
            projection,
            limits,
        })
    }

    #[must_use]
    pub const fn limits(&self) -> ExternalProtocolLimits {
        self.limits
    }

    #[must_use]
    pub const fn schema(&self) -> &TokenSchema {
        &self.schema
    }

    #[must_use]
    pub const fn projection(&self) -> &BoundProjection {
        &self.projection
    }

    #[must_use]
    pub(crate) fn segmentation(&self) -> SegmentationMode {
        self.header.segmentation.into()
    }

    pub(crate) fn header_bytes(&self) -> Result<Vec<u8>, ExternalProtocolError> {
        encode_bounded_record(
            &self.header,
            self.limits.max_record_bytes,
            "run_header exceeds max_record_bytes",
            None,
        )
    }

    pub(crate) fn admits_verified_manifest(
        &self,
        implementation_digest: [u8; 32],
        resource_manifest_digest: [u8; 32],
        components: &[ExternalComponent],
    ) -> bool {
        if self.header.implementation_digest.0 != implementation_digest
            || self.header.resource_manifest_digest.0 != resource_manifest_digest
            || self.header.component_closure.len() != components.len()
        {
            return false;
        }
        self.header
            .component_closure
            .iter()
            .zip(components)
            .all(|(expected, actual)| {
                expected.name == actual.name && expected.digest.0 == actual.digest
            })
    }

    pub(crate) fn batch_request_bytes(
        &self,
        batch_id: u64,
        inputs: &[TextUnit],
        max_input_record_bytes: usize,
    ) -> Result<Vec<Vec<u8>>, ExternalProtocolError> {
        let document_count = u64::try_from(inputs.len())
            .map_err(|_| ExternalProtocolError::new("limit", "document count overflow"))?;
        if inputs.len() > self.limits.max_documents_per_batch {
            return Err(ExternalProtocolError::new(
                "limit",
                "document count exceeds max_documents_per_batch",
            )
            .batch(batch_id));
        }
        let mut records = Vec::with_capacity(inputs.len() + 2);
        let begin_value = BatchBegin {
            record: BatchBeginTag::BatchBegin,
            batch_id,
            document_count,
        };
        let begin = encode_bounded_record(
            &begin_value,
            self.limits
                .max_record_bytes
                .min(self.limits.max_request_batch_bytes),
            "batch_begin exceeds its record/request bound",
            Some(batch_id),
        )?;
        let mut request_bytes = admit_request_record(&begin, 0, self.limits, batch_id)?;
        records.push(begin);
        for (ordinal, input) in inputs.iter().enumerate() {
            if input.normalization() != self.header.normalization.into() {
                return Err(ExternalProtocolError::new(
                    "document",
                    "input normalization differs from the run",
                )
                .batch(batch_id)
                .input(input.id().get()));
            }
            let document = DocumentRef {
                record: DocumentTag::Document,
                batch_id,
                document_ordinal: u64::try_from(ordinal).map_err(|_| {
                    ExternalProtocolError::new("limit", "document ordinal overflow").batch(batch_id)
                })?,
                input_id: input.id().get(),
                kind: input.kind().into(),
                text: input.text(),
                provenance: WireProvenance {
                    source_identity: Digest(*input.provenance().source_identity()),
                    document_identity: input.provenance().document_identity(),
                    paragraph_identity: input.provenance().paragraph_identity(),
                    metadata_digest: Digest(*input.provenance().metadata_digest()),
                },
            };
            let remaining_request = self
                .limits
                .max_request_batch_bytes
                .checked_sub(request_bytes)
                .ok_or_else(|| {
                    ExternalProtocolError::new("limit", "request batch bytes exhausted")
                        .batch(batch_id)
                })?;
            let bytes = encode_bounded_record(
                &document,
                self.limits
                    .max_record_bytes
                    .min(max_input_record_bytes)
                    .min(remaining_request),
                "document exceeds its record/request bound",
                Some(batch_id),
            )
            .map_err(|error| error.input(input.id().get()))?;
            request_bytes = admit_request_record(&bytes, request_bytes, self.limits, batch_id)?;
            if bytes.len() > max_input_record_bytes {
                return Err(ExternalProtocolError::new(
                    "limit",
                    "document record exceeds max_input_record_bytes",
                )
                .batch(batch_id)
                .input(input.id().get()));
            }
            records.push(bytes);
        }
        let end_value = BatchEnd {
            record: BatchEndTag::BatchEnd,
            batch_id,
        };
        let remaining_request = self
            .limits
            .max_request_batch_bytes
            .checked_sub(request_bytes)
            .ok_or_else(|| {
                ExternalProtocolError::new("limit", "request batch bytes exhausted").batch(batch_id)
            })?;
        let end = encode_bounded_record(
            &end_value,
            self.limits.max_record_bytes.min(remaining_request),
            "batch_end exceeds its record/request bound",
            Some(batch_id),
        )?;
        admit_request_record(&end, request_bytes, self.limits, batch_id)?;
        records.push(end);
        Ok(records)
    }
}

fn admit_request_record(
    record: &[u8],
    current_bytes: usize,
    limits: ExternalProtocolLimits,
    batch_id: u64,
) -> Result<usize, ExternalProtocolError> {
    if record.len() > limits.max_record_bytes {
        return Err(
            ExternalProtocolError::new("limit", "request record exceeds max_record_bytes")
                .batch(batch_id),
        );
    }
    let total = current_bytes.checked_add(record.len()).ok_or_else(|| {
        ExternalProtocolError::new("limit", "request batch byte count overflow").batch(batch_id)
    })?;
    if total > limits.max_request_batch_bytes {
        return Err(ExternalProtocolError::new(
            "limit",
            "request batch exceeds max_request_batch_bytes",
        )
        .batch(batch_id));
    }
    Ok(total)
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct ExternalBatchBounds {
    pub max_input_record_bytes: usize,
    pub max_stage_input_bytes: usize,
    pub max_stage_sentences: usize,
    pub max_sentence_bytes: usize,
    pub max_tokenized_bytes: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExternalValidatedBatch {
    batch_id: u64,
    analysis: ValidatedAnalysisBatch,
    warnings: Box<[ExternalWarning]>,
    input_record_bytes: Box<[usize]>,
}

impl ExternalValidatedBatch {
    #[must_use]
    pub const fn batch_id(&self) -> u64 {
        self.batch_id
    }

    #[must_use]
    pub const fn analysis(&self) -> &ValidatedAnalysisBatch {
        &self.analysis
    }

    #[must_use]
    pub fn warnings(&self) -> &[ExternalWarning] {
        &self.warnings
    }

    pub(crate) fn into_parts(
        self,
    ) -> (ValidatedAnalysisBatch, Box<[ExternalWarning]>, Box<[usize]>) {
        (self.analysis, self.warnings, self.input_record_bytes)
    }
}

#[derive(Debug)]
pub struct ExternalTranscript<R> {
    reader: R,
    limits: ExternalProtocolLimits,
    header: RunHeader,
    descriptor: AnalyzerDescriptor,
    execution: ExecutionProvenance,
    schema: TokenSchema,
    projection: BoundProjection,
    next_batch_id: u64,
    last_input_id: Option<u64>,
    request_hasher: blake3::Hasher,
    file_hasher: blake3::Hasher,
    clean_eof: bool,
}

impl<R: BufRead> ExternalTranscript<R> {
    pub fn open(reader: R, limits: ExternalProtocolLimits) -> Result<Self, ExternalProtocolError> {
        Self::open_at(reader, limits, 0)
    }

    pub(crate) fn open_at(
        reader: R,
        limits: ExternalProtocolLimits,
        next_batch_id: u64,
    ) -> Result<Self, ExternalProtocolError> {
        validate_limits(limits)?;
        let mut transcript = Self {
            reader,
            limits,
            header: RunHeader::placeholder(),
            descriptor: AnalyzerDescriptor::new(
                AnalyzerFamily::External,
                ExtractionProfile::Morphology,
                SegmentationMode::Input,
                NormalizationPolicy::None,
                "pending",
                "pending",
                [0; 32],
                [0; 32],
                [0; 32],
                [0; 32],
                [0; 32],
                [0; 32],
                TokenSchema::from_keys(["external.pending"])
                    .expect("placeholder schema is valid")
                    .digest(),
            )
            .expect("placeholder descriptor is valid"),
            execution: ExecutionProvenance {
                backend: "pending".into(),
                runtime: "pending".into(),
                lock_digest: [0; 32],
                workers: 0,
                batch_size: 0,
            },
            schema: TokenSchema::from_keys(["external.pending"])
                .expect("placeholder schema is valid"),
            projection: TokenSchema::from_keys(["external.pending"])
                .expect("placeholder schema is valid")
                .bind_projection([
                    AttributeKey::new("external.pending").expect("placeholder key is valid")
                ])
                .expect("placeholder projection is valid"),
            next_batch_id,
            last_input_id: None,
            request_hasher: blake3::Hasher::new(),
            file_hasher: blake3::Hasher::new(),
            clean_eof: false,
        };
        transcript.request_hasher.update(REQUEST_DIGEST_DOMAIN);
        let (record, _) = transcript
            .read_record()?
            .ok_or_else(|| ExternalProtocolError::new("truncated", "missing run_header"))?;
        let ExternalRecord::RunHeader(header) = record else {
            return Err(ExternalProtocolError::new(
                "state",
                "first record must be run_header",
            ));
        };
        validate_header(&header, limits)?;
        transcript.hash_request(&ExternalRecord::RunHeader(header.clone()))?;

        let (record, _) = transcript
            .read_record()?
            .ok_or_else(|| ExternalProtocolError::new("truncated", "missing run_ready"))?;
        let ExternalRecord::RunReady(ready) = record else {
            return Err(ExternalProtocolError::new(
                "state",
                "run_header must be followed by run_ready",
            ));
        };
        let (descriptor, execution, schema, projection) = validate_ready(&header, &ready, limits)?;
        transcript.header = header;
        transcript.descriptor = descriptor;
        transcript.execution = execution;
        transcript.schema = schema;
        transcript.projection = projection;
        Ok(transcript)
    }

    #[must_use]
    pub const fn descriptor(&self) -> &AnalyzerDescriptor {
        &self.descriptor
    }

    #[must_use]
    pub const fn execution_provenance(&self) -> &ExecutionProvenance {
        &self.execution
    }

    #[must_use]
    pub const fn schema(&self) -> &TokenSchema {
        &self.schema
    }

    #[must_use]
    pub const fn projection(&self) -> &BoundProjection {
        &self.projection
    }

    #[must_use]
    pub fn request_digest(&self) -> [u8; 32] {
        *self.request_hasher.clone().finalize().as_bytes()
    }

    #[must_use]
    pub fn whole_file_digest(&self) -> Option<[u8; 32]> {
        self.clean_eof
            .then(|| *self.file_hasher.clone().finalize().as_bytes())
    }

    pub fn next_validated_batch(
        &mut self,
    ) -> Result<Option<ExternalValidatedBatch>, ExternalProtocolError> {
        self.next_validated_batch_with_bounds(ExternalBatchBounds {
            max_input_record_bytes: self.limits.max_record_bytes,
            max_stage_input_bytes: self.limits.max_request_batch_bytes,
            max_stage_sentences: self.limits.max_documents_per_batch,
            max_sentence_bytes: self.limits.max_record_bytes,
            max_tokenized_bytes: self.limits.max_response_batch_bytes,
        })
    }

    pub(crate) fn next_validated_batch_with_bounds(
        &mut self,
        bounds: ExternalBatchBounds,
    ) -> Result<Option<ExternalValidatedBatch>, ExternalProtocolError> {
        if self.clean_eof {
            return Ok(None);
        }
        let Some((record, begin_bytes)) = self.read_record()? else {
            self.clean_eof = true;
            return Ok(None);
        };
        let ExternalRecord::BatchBegin(begin) = record else {
            return Err(ExternalProtocolError::new(
                "state",
                "expected batch_begin at the run boundary",
            ));
        };
        if begin.batch_id != self.next_batch_id {
            return Err(ExternalProtocolError::new(
                "order",
                format!(
                    "expected batch {}, received {}",
                    self.next_batch_id, begin.batch_id
                ),
            )
            .batch(begin.batch_id));
        }
        let batch_id = begin.batch_id;
        self.hash_request(&ExternalRecord::BatchBegin(begin.clone()))?;
        let document_count = usize::try_from(begin.document_count)
            .map_err(|_| limit_error("document count", batch_id))?;
        if document_count == 0 {
            return Err(state_error("external batches must be nonempty", batch_id));
        }
        if document_count > self.limits.max_documents_per_batch
            || document_count > bounds.max_stage_sentences
        {
            return Err(limit_error("document count", batch_id));
        }
        let mut documents = Vec::with_capacity(document_count);
        let mut input_record_bytes = Vec::with_capacity(document_count);
        let mut request_bytes = begin_bytes;
        let mut input_bytes = 0_usize;
        for expected in 0..document_count {
            let (record, record_bytes) = self.required_record_with_limit(
                "document",
                batch_id,
                bounds.max_input_record_bytes,
            )?;
            let ExternalRecord::Document(document) = record else {
                return Err(state_error("expected document", batch_id));
            };
            validate_batch_id(document.batch_id, batch_id)?;
            if document.document_ordinal
                != u64::try_from(expected).map_err(|_| limit_error("document ordinal", batch_id))?
            {
                return Err(order_error("document ordinal", batch_id).input(document.input_id));
            }
            if self
                .last_input_id
                .is_some_and(|prior| document.input_id <= prior)
            {
                return Err(order_error("global input identity", batch_id).input(document.input_id));
            }
            self.last_input_id = Some(document.input_id);
            if record_bytes > bounds.max_input_record_bytes {
                return Err(limit_error("input record bytes", batch_id).input(document.input_id));
            }
            request_bytes = request_bytes
                .checked_add(record_bytes)
                .ok_or_else(|| limit_error("request batch bytes", batch_id))?;
            input_bytes = input_bytes
                .checked_add(document.text.len())
                .ok_or_else(|| limit_error("stage input bytes", batch_id))?;
            if request_bytes > self.limits.max_request_batch_bytes
                || input_bytes > bounds.max_stage_input_bytes
            {
                return Err(
                    limit_error("request/stage input bytes", batch_id).input(document.input_id)
                );
            }
            self.hash_request(&ExternalRecord::Document(document.clone()))?;
            documents.push(admit_document(document, &self.header, batch_id)?);
            input_record_bytes.push(record_bytes);
        }
        let (record, end_bytes) = self.required_record("batch_end", batch_id)?;
        let ExternalRecord::BatchEnd(end) = record else {
            return Err(state_error("expected batch_end", batch_id));
        };
        validate_batch_id(end.batch_id, batch_id)?;
        request_bytes = request_bytes
            .checked_add(end_bytes)
            .ok_or_else(|| limit_error("request batch bytes", batch_id))?;
        if request_bytes > self.limits.max_request_batch_bytes {
            return Err(limit_error("request batch bytes", batch_id));
        }
        self.hash_request(&ExternalRecord::BatchEnd(end))?;

        let mut sentences: Vec<Vec<AnalyzedSentence>> =
            (0..document_count).map(|_| Vec::new()).collect();
        let mut warnings = Vec::new();
        let mut response_bytes = 0_usize;
        let mut sentence_count = 0_usize;
        let mut tokenized_bytes = 0_usize;
        let mut last_response_document = None;
        loop {
            let (record, bytes) = self.required_record("sentence or batch_complete", batch_id)?;
            response_bytes = response_bytes
                .checked_add(bytes)
                .ok_or_else(|| limit_error("response batch bytes", batch_id))?;
            if response_bytes > self.limits.max_response_batch_bytes {
                return Err(limit_error("response batch bytes", batch_id));
            }
            match record {
                ExternalRecord::Sentence(sentence) => {
                    sentence_count = sentence_count
                        .checked_add(1)
                        .ok_or_else(|| limit_error("sentence count", batch_id))?;
                    if sentence_count > bounds.max_stage_sentences {
                        return Err(limit_error("sentence count", batch_id));
                    }
                    if last_response_document.is_some_and(|prior| sentence.document_ordinal < prior)
                    {
                        return Err(order_error("response document order", batch_id)
                            .input(sentence.input_id));
                    }
                    last_response_document = Some(sentence.document_ordinal);
                    validate_sentence(
                        sentence,
                        batch_id,
                        &documents,
                        &mut sentences,
                        &mut warnings,
                        &self.projection,
                        self.limits,
                        bounds,
                        &mut tokenized_bytes,
                    )?;
                }
                ExternalRecord::BatchComplete(complete) => {
                    validate_batch_id(complete.batch_id, batch_id)?;
                    if complete.sentence_counts.len() != document_count {
                        return Err(state_error("sentence count shape mismatch", batch_id));
                    }
                    for (actual, declared) in sentences.iter().zip(complete.sentence_counts) {
                        if actual.len() as u64 != declared {
                            return Err(state_error("sentence count mismatch", batch_id));
                        }
                    }
                    break;
                }
                _ => return Err(state_error("unexpected record in batch response", batch_id)),
            }
        }

        let mut analyzed_inputs = Vec::with_capacity(document_count);
        for (input, sentences) in documents.into_iter().zip(sentences) {
            analyzed_inputs.push(
                AnalyzedInput::new(input, sentences, self.header.segmentation.into())
                    .map_err(|error| analysis_error(error, batch_id))?,
            );
        }
        let analysis = ValidatedAnalysisBatch::new(analyzed_inputs)
            .map_err(|error| analysis_error(error, batch_id))?;
        self.next_batch_id = self
            .next_batch_id
            .checked_add(1)
            .ok_or_else(|| limit_error("batch identity", batch_id))?;
        Ok(Some(ExternalValidatedBatch {
            batch_id,
            analysis,
            warnings: warnings.into_boxed_slice(),
            input_record_bytes: input_record_bytes.into_boxed_slice(),
        }))
    }

    fn required_record(
        &mut self,
        expected: &str,
        batch_id: u64,
    ) -> Result<(ExternalRecord, usize), ExternalProtocolError> {
        self.read_record()?.ok_or_else(|| {
            ExternalProtocolError::new("truncated", format!("missing {expected}")).batch(batch_id)
        })
    }

    fn required_record_with_limit(
        &mut self,
        expected: &str,
        batch_id: u64,
        limit: usize,
    ) -> Result<(ExternalRecord, usize), ExternalProtocolError> {
        self.read_record_with_limit(limit)?.ok_or_else(|| {
            ExternalProtocolError::new("truncated", format!("missing {expected}")).batch(batch_id)
        })
    }

    fn hash_request(&mut self, record: &ExternalRecord) -> Result<(), ExternalProtocolError> {
        let bytes = encode_record(record)?;
        self.request_hasher.update(&bytes);
        Ok(())
    }

    fn read_record(&mut self) -> Result<Option<(ExternalRecord, usize)>, ExternalProtocolError> {
        self.read_record_with_limit(self.limits.max_record_bytes)
    }

    fn read_record_with_limit(
        &mut self,
        local_limit: usize,
    ) -> Result<Option<(ExternalRecord, usize)>, ExternalProtocolError> {
        let mut bytes = Vec::new();
        let admitted_limit = self.limits.max_record_bytes.min(local_limit);
        let read_limit = admitted_limit
            .checked_add(1)
            .ok_or_else(|| ExternalProtocolError::new("limit", "record bound overflow"))?;
        let read = self
            .reader
            .by_ref()
            .take(read_limit as u64)
            .read_until(b'\n', &mut bytes)
            .map_err(|error| ExternalProtocolError::new("io", error.to_string()))?;
        if read == 0 {
            return Ok(None);
        }
        self.file_hasher.update(&bytes);
        if bytes.len() > admitted_limit {
            let message = if admitted_limit < self.limits.max_record_bytes {
                "input record bytes exceed the execution limit"
            } else {
                "protocol record exceeds max_record_bytes"
            };
            return Err(ExternalProtocolError::new("limit", message));
        }
        if bytes.last() != Some(&b'\n') {
            return Err(ExternalProtocolError::new(
                "framing",
                "protocol record is missing its final LF",
            ));
        }
        bytes.pop();
        if bytes.last() == Some(&b'\r') {
            return Err(ExternalProtocolError::new(
                "framing",
                "CRLF records are not admitted",
            ));
        }
        if bytes.is_empty() {
            return Err(ExternalProtocolError::new(
                "framing",
                "blank protocol records are not admitted",
            ));
        }
        std::str::from_utf8(&bytes)
            .map_err(|error| ExternalProtocolError::new("utf8", error.to_string()))?;
        let value = decode_unique_json(&bytes)?;
        Ok(Some((decode_record(value)?, read)))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ResponseRecordKind {
    Sentence {
        batch_id: u64,
        document_ordinal: u64,
        input_id: u64,
        sentence_ordinal: u64,
    },
    BatchComplete {
        batch_id: u64,
    },
}

pub(crate) fn response_record_kind(
    bytes: &[u8],
    limits: ExternalProtocolLimits,
) -> Result<ResponseRecordKind, ExternalProtocolError> {
    if bytes.len() > limits.max_record_bytes || bytes.last() != Some(&b'\n') {
        return Err(ExternalProtocolError::new(
            "framing",
            "adapter response is not one bounded LF record",
        ));
    }
    if bytes.get(bytes.len().saturating_sub(2)) == Some(&b'\r') {
        return Err(ExternalProtocolError::new(
            "framing",
            "CRLF records are not admitted",
        ));
    }
    let value = decode_unique_json(&bytes[..bytes.len() - 1])?;
    match decode_record(value)? {
        ExternalRecord::Sentence(sentence) => Ok(ResponseRecordKind::Sentence {
            batch_id: sentence.batch_id,
            document_ordinal: sentence.document_ordinal,
            input_id: sentence.input_id,
            sentence_ordinal: sentence.sentence_ordinal,
        }),
        ExternalRecord::BatchComplete(complete) => Ok(ResponseRecordKind::BatchComplete {
            batch_id: complete.batch_id,
        }),
        _ => Err(ExternalProtocolError::new(
            "state",
            "unexpected adapter response record",
        )),
    }
}

fn validate_limits(limits: ExternalProtocolLimits) -> Result<(), ExternalProtocolError> {
    if limits.max_record_bytes == 0
        || limits.max_request_batch_bytes == 0
        || limits.max_response_batch_bytes == 0
        || limits.max_documents_per_batch == 0
        || limits.max_schema_keys == 0
        || limits.max_schema_key_bytes == 0
        || limits.max_warning_message_bytes == 0
    {
        return Err(ExternalProtocolError::new(
            "limit",
            "external protocol limits must be nonzero",
        ));
    }
    Ok(())
}

fn validate_header(
    header: &RunHeader,
    limits: ExternalProtocolLimits,
) -> Result<(), ExternalProtocolError> {
    if header.protocol != PROTOCOL {
        return Err(ExternalProtocolError::new(
            "protocol",
            format!("unsupported protocol identity `{}`", header.protocol),
        ));
    }
    if header.run_id.is_empty()
        || header.language_policy.is_empty()
        || header.boundary_policy.is_empty()
    {
        return Err(ExternalProtocolError::new(
            "protocol",
            "run and policy identities must be nonempty",
        ));
    }
    if [
        header.run_id.len(),
        header.language_policy.len(),
        header.boundary_policy.len(),
    ]
    .into_iter()
    .any(|length| length > limits.max_schema_key_bytes)
        || header.component_closure.len() > limits.max_schema_keys
        || header
            .component_closure
            .iter()
            .any(|component| component.name.len() > limits.max_schema_key_bytes)
    {
        return Err(ExternalProtocolError::new(
            "limit",
            "run identity or component closure exceeds configured limits",
        ));
    }
    if header.protocol_limits != limits {
        return Err(ExternalProtocolError::new(
            "handshake",
            "run_header protocol limits differ from the admitting limits",
        ));
    }
    if header.component_closure.is_empty()
        || header
            .component_closure
            .iter()
            .any(|component| component.name.is_empty())
        || header
            .component_closure
            .windows(2)
            .any(|pair| pair[0].name >= pair[1].name)
    {
        return Err(ExternalProtocolError::new(
            "protocol",
            "component closure must be nonempty and strictly name-ordered",
        ));
    }
    validate_schema_attributes(&header.projection, limits)
}

fn validate_ready(
    header: &RunHeader,
    ready: &RunReady,
    limits: ExternalProtocolLimits,
) -> Result<
    (
        AnalyzerDescriptor,
        ExecutionProvenance,
        TokenSchema,
        BoundProjection,
    ),
    ExternalProtocolError,
> {
    if ready.run_id != header.run_id
        || ready.family != header.family
        || ready.profile != header.profile
        || ready.segmentation != header.segmentation
        || ready.normalization != header.normalization
        || ready.language_policy != header.language_policy
        || ready.boundary_policy != header.boundary_policy
        || ready.implementation_digest != header.implementation_digest
        || ready.resource_manifest_digest != header.resource_manifest_digest
        || ready.projection_policy_digest != header.projection_policy_digest
        || ready.alignment_policy_digest != header.alignment_policy_digest
        || ready.output_config_digest != header.output_config_digest
        || ready.component_closure != header.component_closure
    {
        return Err(ExternalProtocolError::new(
            "handshake",
            "run_ready does not describe the requested run",
        ));
    }
    if ready.returned_projection != header.projection {
        return Err(ExternalProtocolError::new(
            "handshake",
            "returned projection differs from the requested ordered projection",
        ));
    }
    if !ready.capabilities.contains(&header.profile) {
        return Err(ExternalProtocolError::new(
            "handshake",
            "run_ready omits the requested capability",
        ));
    }
    validate_schema_attributes(&ready.schema, limits)?;
    let attributes = ready
        .schema
        .iter()
        .map(WireSchemaAttribute::to_schema_attribute)
        .collect::<Result<Vec<_>, _>>()?;
    let schema = TokenSchema::new(attributes)
        .map_err(|error| ExternalProtocolError::new("schema", error.to_string()))?;
    if wire_schema(schema.attributes()) != ready.schema {
        return Err(ExternalProtocolError::new(
            "schema",
            "produced schema is not in canonical key order",
        ));
    }
    let projection_keys = ready
        .returned_projection
        .iter()
        .map(|attribute| AttributeKey::new(&attribute.key).map_err(|error| error.to_string()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| ExternalProtocolError::new("schema", error))?;
    let projection = schema
        .bind_projection(projection_keys)
        .map_err(|error| ExternalProtocolError::new("schema", error.to_string()))?;
    if wire_projection(&projection) != ready.returned_projection {
        return Err(ExternalProtocolError::new(
            "schema",
            "returned projection kinds differ from the produced schema",
        ));
    }
    let descriptor = AnalyzerDescriptor::new(
        ready.family.into(),
        ready.profile.into(),
        ready.segmentation.into(),
        ready.normalization.into(),
        ready.language_policy.clone(),
        ready.boundary_policy.clone(),
        ready.implementation_digest.0,
        ready.resource_manifest_digest.0,
        ready.projection_policy_digest.0,
        ready.alignment_policy_digest.0,
        external_component_closure_digest(
            &ready
                .component_closure
                .iter()
                .map(|component| ExternalComponent {
                    name: component.name.clone(),
                    digest: component.digest.0,
                })
                .collect::<Vec<_>>(),
        ),
        ready.output_config_digest.0,
        schema.digest(),
    )
    .map_err(|error| ExternalProtocolError::new("handshake", error))?;
    let execution = ExecutionProvenance {
        backend: ready.execution.backend.clone().into_boxed_str(),
        runtime: ready.execution.runtime.clone().into_boxed_str(),
        lock_digest: ready.execution.lock_digest.0,
        workers: ready.execution.workers,
        batch_size: ready.execution.batch_size,
    };
    Ok((descriptor, execution, schema, projection))
}

fn validate_schema_attributes(
    attributes: &[WireSchemaAttribute],
    limits: ExternalProtocolLimits,
) -> Result<(), ExternalProtocolError> {
    if attributes.is_empty() || attributes.len() > limits.max_schema_keys {
        return Err(ExternalProtocolError::new(
            "limit",
            "schema/projection key count is outside protocol limits",
        ));
    }
    if attributes
        .iter()
        .any(|attribute| attribute.key.len() > limits.max_schema_key_bytes)
    {
        return Err(ExternalProtocolError::new(
            "limit",
            "schema/projection key exceeds max_schema_key_bytes",
        ));
    }
    let parsed = attributes
        .iter()
        .map(WireSchemaAttribute::to_schema_attribute)
        .collect::<Result<Vec<_>, _>>()?;
    let canonical = TokenSchema::new(parsed)
        .map_err(|error| ExternalProtocolError::new("schema", error.to_string()))?;
    if wire_schema(canonical.attributes()) != attributes {
        return Err(ExternalProtocolError::new(
            "schema",
            "schema/projection attributes are not in canonical order",
        ));
    }
    Ok(())
}

fn wire_schema(attributes: &[SchemaAttribute]) -> Vec<WireSchemaAttribute> {
    attributes
        .iter()
        .map(|attribute| WireSchemaAttribute {
            key: attribute.key().as_str().to_owned(),
            kind: attribute.kind().into(),
        })
        .collect()
}

fn wire_projection(projection: &BoundProjection) -> Vec<WireSchemaAttribute> {
    projection
        .keys()
        .iter()
        .zip(projection.kinds())
        .map(|(key, kind)| WireSchemaAttribute {
            key: key.as_str().to_owned(),
            kind: (*kind).into(),
        })
        .collect()
}

fn validate_canonical_set(
    values: &[String],
    batch_id: u64,
    input_id: u64,
) -> Result<(), ExternalProtocolError> {
    if values.iter().any(String::is_empty)
        || values
            .windows(2)
            .any(|pair| pair[0].as_bytes() >= pair[1].as_bytes())
    {
        return Err(ExternalProtocolError::new(
            "schema",
            "string-set cells must be nonempty-member, bytewise-sorted, and deduplicated",
        )
        .batch(batch_id)
        .input(input_id));
    }
    Ok(())
}

fn admit_document(
    document: Document,
    header: &RunHeader,
    batch_id: u64,
) -> Result<TextUnit, ExternalProtocolError> {
    let authored_text = document.text.clone();
    let provenance = TextUnitProvenance::new(
        document.provenance.source_identity.0,
        document.provenance.document_identity,
        document.provenance.paragraph_identity,
        document.provenance.metadata_digest.0,
    );
    let input = TextUnit::admit_with_provenance(
        InputUnitId::new(document.input_id),
        document.kind.into(),
        document.text,
        header.normalization.into(),
        provenance,
    )
    .map_err(|error| {
        ExternalProtocolError::new("document", error.to_string())
            .batch(batch_id)
            .input(document.input_id)
    })?;
    if input.text() != authored_text {
        return Err(ExternalProtocolError::new(
            "document",
            "document text is not already canonical under the run normalization",
        )
        .batch(batch_id)
        .input(input.id().get()));
    }
    Ok(input)
}

#[allow(clippy::too_many_arguments)]
fn validate_sentence(
    sentence: Sentence,
    batch_id: u64,
    documents: &[TextUnit],
    sentences: &mut [Vec<AnalyzedSentence>],
    warnings: &mut Vec<ExternalWarning>,
    projection: &BoundProjection,
    limits: ExternalProtocolLimits,
    bounds: ExternalBatchBounds,
    tokenized_bytes: &mut usize,
) -> Result<(), ExternalProtocolError> {
    validate_batch_id(sentence.batch_id, batch_id)?;
    let document_index = usize::try_from(sentence.document_ordinal)
        .map_err(|_| order_error("document ordinal", batch_id))?;
    let input = documents
        .get(document_index)
        .ok_or_else(|| order_error("document ordinal", batch_id).input(sentence.input_id))?;
    if input.id().get() != sentence.input_id {
        return Err(order_error("input identity", batch_id).input(sentence.input_id));
    }
    if sentence.sentence_ordinal
        != u64::try_from(sentences[document_index].len())
            .map_err(|_| limit_error("sentence ordinal", batch_id))?
    {
        return Err(order_error("sentence ordinal", batch_id).input(sentence.input_id));
    }
    if sentence.warnings.len() > limits.max_warnings_per_sentence {
        return Err(limit_error("warning count", batch_id).input(sentence.input_id));
    }
    for warning in &sentence.warnings {
        if warning.message.len() > limits.max_warning_message_bytes {
            return Err(limit_error("warning message bytes", batch_id).input(sentence.input_id));
        }
        if warning.input_id != sentence.input_id
            || warning.code.is_empty()
            || !warning.code.contains('.')
        {
            return Err(ExternalProtocolError::new(
                "warning",
                "warning identity or provider-namespaced code is invalid",
            )
            .batch(batch_id)
            .input(sentence.input_id));
        }
    }
    let word_count = usize::try_from(sentence.word_count)
        .map_err(|_| limit_error("word count", batch_id).input(sentence.input_id))?;
    let expected_values = word_count
        .checked_mul(projection.len())
        .ok_or_else(|| limit_error("returned value count", batch_id).input(sentence.input_id))?;
    if sentence.values.len() != expected_values {
        return Err(ExternalProtocolError::new(
            "alignment",
            "returned value count does not equal word_count times projection width",
        )
        .batch(batch_id)
        .input(sentence.input_id));
    }
    let values = sentence
        .values
        .into_iter()
        .enumerate()
        .map(|(index, value)| {
            let ordinal =
                crate::token_match::AttributeOrdinal::new((index % projection.len()) as u8);
            let kind = projection
                .kind(ordinal)
                .ok_or_else(|| limit_error("returned value ordinal", batch_id))?;
            match (kind, value) {
                (AttributeKind::Scalar, Some(WireAttributeValue::Scalar(value))) => {
                    Ok(ProjectedAttributeValue::scalar(value))
                }
                (AttributeKind::Scalar, None) => Ok(ProjectedAttributeValue::missing_scalar()),
                (AttributeKind::StringSet, Some(WireAttributeValue::StringSet(values))) => {
                    validate_canonical_set(&values, batch_id, sentence.input_id)?;
                    Ok(ProjectedAttributeValue::StringSet(
                        values.into_iter().map(CompactString::from).collect(),
                    ))
                }
                (AttributeKind::StringSet, None) => {
                    Ok(ProjectedAttributeValue::StringSet(Vec::new()))
                }
                _ => Err(ExternalProtocolError::new(
                    "schema",
                    "returned cell kind differs from its projection kind",
                )
                .batch(batch_id)
                .input(sentence.input_id)),
            }
        })
        .collect::<Result<Vec<_>, ExternalProtocolError>>()?;
    let source_start = usize::try_from(sentence.source_range[0]).map_err(|_| {
        range_error(
            "source range start exceeds usize",
            batch_id,
            sentence.input_id,
        )
    })?;
    let source_end = usize::try_from(sentence.source_range[1]).map_err(|_| {
        range_error(
            "source range end exceeds usize",
            batch_id,
            sentence.input_id,
        )
    })?;
    let source_range = input
        .source_range(source_start, source_end)
        .map_err(|error| range_error(error, batch_id, sentence.input_id))?;
    if source_range.as_usize().len() > bounds.max_sentence_bytes {
        return Err(limit_error("sentence bytes", batch_id).input(sentence.input_id));
    }
    let identity_alignment = matches!(&sentence.alignment, WireAlignment::Identity);
    let word_ranges: Vec<[u64; 2]> = match sentence.alignment {
        WireAlignment::Identity => {
            if sentence.surface_ranges.len() != word_count {
                return Err(alignment_error(batch_id, sentence.input_id));
            }
            (0..word_count)
                .map(|index| {
                    let start = u64::try_from(index)
                        .map_err(|_| alignment_error(batch_id, sentence.input_id))?;
                    let end = start
                        .checked_add(1)
                        .ok_or_else(|| alignment_error(batch_id, sentence.input_id))?;
                    Ok([start, end])
                })
                .collect::<Result<Vec<_>, ExternalProtocolError>>()?
        }
        WireAlignment::Expanded { word_ranges } => {
            if word_ranges.len() != sentence.surface_ranges.len() {
                return Err(alignment_error(batch_id, sentence.input_id));
            }
            word_ranges
        }
    };
    let mut surfaces = Vec::with_capacity(sentence.surface_ranges.len());
    for (range, words) in sentence.surface_ranges.into_iter().zip(word_ranges) {
        let start = usize::try_from(range[0]).map_err(|_| {
            range_error(
                "surface range start exceeds usize",
                batch_id,
                sentence.input_id,
            )
        })?;
        let end = usize::try_from(range[1]).map_err(|_| {
            range_error(
                "surface range end exceeds usize",
                batch_id,
                sentence.input_id,
            )
        })?;
        let range = input
            .source_range(start, end)
            .map_err(|error| range_error(error, batch_id, sentence.input_id))?;
        surfaces.push(SurfaceToken::new(
            range,
            u32::try_from(words[0]).map_err(|_| alignment_error(batch_id, sentence.input_id))?,
            u32::try_from(words[1]).map_err(|_| alignment_error(batch_id, sentence.input_id))?,
        ));
    }
    if identity_alignment {
        let text = projection
            .keys()
            .iter()
            .position(|key| key.as_str() == "text")
            .ok_or_else(|| alignment_error(batch_id, sentence.input_id))?;
        for (word, surface) in surfaces.iter().enumerate() {
            let value_index = word
                .checked_mul(projection.len())
                .and_then(|index| index.checked_add(text))
                .ok_or_else(|| limit_error("returned value index", batch_id))?;
            let returned = match &values[value_index] {
                ProjectedAttributeValue::Scalar(Some(value)) => value.as_str(),
                _ => return Err(alignment_error(batch_id, sentence.input_id)),
            };
            let source = input
                .slice(surface.source_range())
                .map_err(|error| range_error(error, batch_id, sentence.input_id))?;
            if returned != source {
                return Err(alignment_error(batch_id, sentence.input_id));
            }
        }
    }
    let words = BoundTokenSentence::from_projected_attribute_values(projection, word_count, values)
        .map_err(|error| analysis_error(error, batch_id).input(sentence.input_id))?;
    let resident_charge = words
        .resident_bytes()
        .map_err(|error| analysis_error(error, batch_id).input(sentence.input_id))?;
    if resident_charge > bounds.max_tokenized_bytes {
        return Err(limit_error("tokenized sentence bytes", batch_id).input(sentence.input_id));
    }
    *tokenized_bytes = tokenized_bytes
        .checked_add(resident_charge)
        .ok_or_else(|| limit_error("tokenized batch bytes", batch_id))?;
    if *tokenized_bytes > bounds.max_tokenized_bytes {
        return Err(limit_error("tokenized batch bytes", batch_id));
    }
    let analyzed = AnalyzedSentence::new(
        u32::try_from(sentence.sentence_ordinal)
            .map_err(|_| alignment_error(batch_id, sentence.input_id))?,
        source_range,
        surfaces.into_boxed_slice(),
        words,
    )
    .map_err(|error| analysis_error(error, batch_id).input(sentence.input_id))?;
    sentences[document_index].push(analyzed);
    warnings.extend(
        sentence
            .warnings
            .into_iter()
            .map(|warning| ExternalWarning {
                code: warning.code.into_boxed_str(),
                input_id: InputUnitId::new(warning.input_id),
                message: warning.message.into_boxed_str(),
            }),
    );
    Ok(())
}

fn validate_batch_id(actual: u64, expected: u64) -> Result<(), ExternalProtocolError> {
    if actual == expected {
        Ok(())
    } else {
        Err(order_error("batch identity", expected))
    }
}

fn state_error(message: &str, batch_id: u64) -> ExternalProtocolError {
    ExternalProtocolError::new("state", message).batch(batch_id)
}

fn order_error(message: &str, batch_id: u64) -> ExternalProtocolError {
    ExternalProtocolError::new("order", message).batch(batch_id)
}

fn limit_error(message: &str, batch_id: u64) -> ExternalProtocolError {
    ExternalProtocolError::new("limit", message).batch(batch_id)
}

fn alignment_error(batch_id: u64, input_id: u64) -> ExternalProtocolError {
    ExternalProtocolError::new("alignment", "invalid surface-to-word alignment")
        .batch(batch_id)
        .input(input_id)
}

fn range_error(error: impl fmt::Display, batch_id: u64, input_id: u64) -> ExternalProtocolError {
    ExternalProtocolError::new("range", error.to_string())
        .batch(batch_id)
        .input(input_id)
}

fn analysis_error(error: impl fmt::Display, batch_id: u64) -> ExternalProtocolError {
    ExternalProtocolError::new("analysis", error.to_string()).batch(batch_id)
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum WireFamily {
    Vibrato,
    Sudachi,
    Spacy,
    Stanza,
    Pretokenized,
    External,
}

impl From<WireFamily> for AnalyzerFamily {
    fn from(value: WireFamily) -> Self {
        match value {
            WireFamily::Vibrato => Self::Vibrato,
            WireFamily::Sudachi => Self::Sudachi,
            WireFamily::Spacy => Self::Spacy,
            WireFamily::Stanza => Self::Stanza,
            WireFamily::Pretokenized => Self::Pretokenized,
            WireFamily::External => Self::External,
        }
    }
}

impl From<AnalyzerFamily> for WireFamily {
    fn from(value: AnalyzerFamily) -> Self {
        match value {
            AnalyzerFamily::Vibrato => Self::Vibrato,
            AnalyzerFamily::Sudachi => Self::Sudachi,
            AnalyzerFamily::Spacy => Self::Spacy,
            AnalyzerFamily::Stanza => Self::Stanza,
            AnalyzerFamily::Pretokenized => Self::Pretokenized,
            AnalyzerFamily::External => Self::External,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum WireProfile {
    Tokenize,
    Morphology,
    Syntax,
}

impl From<WireProfile> for ExtractionProfile {
    fn from(value: WireProfile) -> Self {
        match value {
            WireProfile::Tokenize => Self::Tokenize,
            WireProfile::Morphology => Self::Morphology,
            WireProfile::Syntax => Self::Syntax,
        }
    }
}

impl From<ExtractionProfile> for WireProfile {
    fn from(value: ExtractionProfile) -> Self {
        match value {
            ExtractionProfile::Tokenize => Self::Tokenize,
            ExtractionProfile::Morphology => Self::Morphology,
            ExtractionProfile::Syntax => Self::Syntax,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum WireSegmentation {
    Input,
    Analyzer,
}

impl From<WireSegmentation> for SegmentationMode {
    fn from(value: WireSegmentation) -> Self {
        match value {
            WireSegmentation::Input => Self::Input,
            WireSegmentation::Analyzer => Self::Analyzer,
        }
    }
}

impl From<SegmentationMode> for WireSegmentation {
    fn from(value: SegmentationMode) -> Self {
        match value {
            SegmentationMode::Input => Self::Input,
            SegmentationMode::Analyzer => Self::Analyzer,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum WireNormalization {
    None,
    Nfc,
}

impl From<WireNormalization> for NormalizationPolicy {
    fn from(value: WireNormalization) -> Self {
        match value {
            WireNormalization::None => Self::None,
            WireNormalization::Nfc => Self::Nfc,
        }
    }
}

impl From<NormalizationPolicy> for WireNormalization {
    fn from(value: NormalizationPolicy) -> Self {
        match value {
            NormalizationPolicy::None => Self::None,
            NormalizationPolicy::Nfc => Self::Nfc,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum WireTextKind {
    Sentence,
    Segment,
}

impl From<WireTextKind> for TextUnitKind {
    fn from(value: WireTextKind) -> Self {
        match value {
            WireTextKind::Sentence => Self::Sentence,
            WireTextKind::Segment => Self::Segment,
        }
    }
}

impl From<TextUnitKind> for WireTextKind {
    fn from(value: TextUnitKind) -> Self {
        match value {
            TextUnitKind::Sentence => Self::Sentence,
            TextUnitKind::Segment => Self::Segment,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Digest([u8; 32]);

impl Serialize for Digest {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&hex_digest(&self.0))
    }
}

impl<'de> Deserialize<'de> for Digest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        parse_digest(&value).map(Self).map_err(D::Error::custom)
    }
}

fn parse_digest(value: &str) -> Result<[u8; 32], &'static str> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err("digests must contain exactly 64 lowercase hexadecimal characters");
    }
    let mut digest = [0_u8; 32];
    let bytes = value.as_bytes();
    for (index, byte) in digest.iter_mut().enumerate() {
        let high = hex_nibble(bytes[index * 2]).ok_or("invalid hexadecimal digest")?;
        let low = hex_nibble(bytes[index * 2 + 1]).ok_or("invalid hexadecimal digest")?;
        *byte = (high << 4) | low;
    }
    Ok(digest)
}

const fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}

fn hex_digest(digest: &[u8; 32]) -> String {
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RunHeader {
    record: RunHeaderTag,
    protocol: String,
    run_id: String,
    family: WireFamily,
    profile: WireProfile,
    segmentation: WireSegmentation,
    normalization: WireNormalization,
    language_policy: String,
    boundary_policy: String,
    projection: Vec<WireSchemaAttribute>,
    implementation_digest: Digest,
    resource_manifest_digest: Digest,
    projection_policy_digest: Digest,
    alignment_policy_digest: Digest,
    output_config_digest: Digest,
    component_closure: Vec<WireComponent>,
    protocol_limits: ExternalProtocolLimits,
}

impl RunHeader {
    fn placeholder() -> Self {
        Self {
            record: RunHeaderTag::RunHeader,
            protocol: String::new(),
            run_id: String::new(),
            family: WireFamily::External,
            profile: WireProfile::Morphology,
            segmentation: WireSegmentation::Input,
            normalization: WireNormalization::None,
            language_policy: String::new(),
            boundary_policy: String::new(),
            projection: Vec::new(),
            implementation_digest: Digest([0; 32]),
            resource_manifest_digest: Digest([0; 32]),
            projection_policy_digest: Digest([0; 32]),
            alignment_policy_digest: Digest([0; 32]),
            output_config_digest: Digest([0; 32]),
            component_closure: Vec::new(),
            protocol_limits: ExternalProtocolLimits::default(),
        }
    }
}

macro_rules! record_tag {
    ($name:ident, $variant:ident, $wire:literal) => {
        #[derive(Clone, Copy, Debug, Deserialize, Serialize)]
        enum $name {
            #[serde(rename = $wire)]
            $variant,
        }
    };
}

record_tag!(RunHeaderTag, RunHeader, "run_header");
record_tag!(RunReadyTag, RunReady, "run_ready");
record_tag!(BatchBeginTag, BatchBegin, "batch_begin");
record_tag!(DocumentTag, Document, "document");
record_tag!(BatchEndTag, BatchEnd, "batch_end");
record_tag!(SentenceTag, Sentence, "sentence");
record_tag!(BatchCompleteTag, BatchComplete, "batch_complete");

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct WireComponent {
    name: String,
    digest: Digest,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct WireSchemaAttribute {
    key: String,
    kind: WireAttributeKind,
}

impl WireSchemaAttribute {
    fn to_schema_attribute(&self) -> Result<SchemaAttribute, ExternalProtocolError> {
        SchemaAttribute::new(&self.key, self.kind.into())
            .map_err(|error| ExternalProtocolError::new("schema", error.to_string()))
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum WireAttributeKind {
    Scalar,
    StringSet,
}

impl From<AttributeKind> for WireAttributeKind {
    fn from(kind: AttributeKind) -> Self {
        match kind {
            AttributeKind::Scalar => Self::Scalar,
            AttributeKind::StringSet => Self::StringSet,
        }
    }
}

impl From<WireAttributeKind> for AttributeKind {
    fn from(kind: WireAttributeKind) -> Self {
        match kind {
            WireAttributeKind::Scalar => Self::Scalar,
            WireAttributeKind::StringSet => Self::StringSet,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RunReady {
    record: RunReadyTag,
    run_id: String,
    family: WireFamily,
    profile: WireProfile,
    segmentation: WireSegmentation,
    normalization: WireNormalization,
    language_policy: String,
    boundary_policy: String,
    implementation_digest: Digest,
    resource_manifest_digest: Digest,
    projection_policy_digest: Digest,
    alignment_policy_digest: Digest,
    component_closure: Vec<WireComponent>,
    output_config_digest: Digest,
    schema: Vec<WireSchemaAttribute>,
    returned_projection: Vec<WireSchemaAttribute>,
    capabilities: Vec<WireProfile>,
    execution: WireExecution,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct WireExecution {
    backend: String,
    runtime: String,
    lock_digest: Digest,
    workers: u32,
    batch_size: u32,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct BatchBegin {
    record: BatchBeginTag,
    batch_id: u64,
    document_count: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Document {
    record: DocumentTag,
    batch_id: u64,
    document_ordinal: u64,
    input_id: u64,
    kind: WireTextKind,
    text: String,
    provenance: WireProvenance,
}

#[derive(Serialize)]
struct DocumentRef<'a> {
    record: DocumentTag,
    batch_id: u64,
    document_ordinal: u64,
    input_id: u64,
    kind: WireTextKind,
    text: &'a str,
    provenance: WireProvenance,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct WireProvenance {
    source_identity: Digest,
    document_identity: u64,
    paragraph_identity: Option<u64>,
    metadata_digest: Digest,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct BatchEnd {
    record: BatchEndTag,
    batch_id: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Sentence {
    record: SentenceTag,
    batch_id: u64,
    document_ordinal: u64,
    input_id: u64,
    sentence_ordinal: u64,
    source_range: [u64; 2],
    surface_ranges: Vec<[u64; 2]>,
    alignment: WireAlignment,
    word_count: u64,
    values: Vec<Option<WireAttributeValue>>,
    warnings: Vec<WireWarning>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(untagged)]
enum WireAttributeValue {
    Scalar(String),
    StringSet(Vec<String>),
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum WireAlignment {
    Identity,
    Expanded { word_ranges: Vec<[u64; 2]> },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct WireWarning {
    code: String,
    input_id: u64,
    message: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct BatchComplete {
    record: BatchCompleteTag,
    batch_id: u64,
    sentence_counts: Vec<u64>,
}

#[derive(Clone, Debug)]
enum ExternalRecord {
    RunHeader(RunHeader),
    RunReady(RunReady),
    BatchBegin(BatchBegin),
    Document(Document),
    BatchEnd(BatchEnd),
    Sentence(Sentence),
    BatchComplete(BatchComplete),
}

fn decode_record(value: Value) -> Result<ExternalRecord, ExternalProtocolError> {
    let tag = value
        .as_object()
        .and_then(|object| object.get("record"))
        .and_then(Value::as_str)
        .ok_or_else(|| ExternalProtocolError::new("json", "record tag is missing or invalid"))?;
    macro_rules! decode {
        ($ty:ty, $variant:ident) => {
            serde_json::from_value::<$ty>(value)
                .map(ExternalRecord::$variant)
                .map_err(|error| ExternalProtocolError::new("json", error.to_string()))
        };
    }
    match tag {
        "run_header" => decode!(RunHeader, RunHeader),
        "run_ready" => decode!(RunReady, RunReady),
        "batch_begin" => decode!(BatchBegin, BatchBegin),
        "document" => decode!(Document, Document),
        "batch_end" => decode!(BatchEnd, BatchEnd),
        "sentence" => decode!(Sentence, Sentence),
        "batch_complete" => decode!(BatchComplete, BatchComplete),
        other => Err(ExternalProtocolError::new(
            "json",
            format!("unknown record tag `{other}`"),
        )),
    }
}

fn encode_record(record: &ExternalRecord) -> Result<Vec<u8>, ExternalProtocolError> {
    let mut bytes = match record {
        ExternalRecord::RunHeader(value) => serde_json::to_vec(value),
        ExternalRecord::RunReady(value) => serde_json::to_vec(value),
        ExternalRecord::BatchBegin(value) => serde_json::to_vec(value),
        ExternalRecord::Document(value) => serde_json::to_vec(value),
        ExternalRecord::BatchEnd(value) => serde_json::to_vec(value),
        ExternalRecord::Sentence(value) => serde_json::to_vec(value),
        ExternalRecord::BatchComplete(value) => serde_json::to_vec(value),
    }
    .map_err(|error| ExternalProtocolError::new("json", error.to_string()))?;
    bytes.push(b'\n');
    Ok(bytes)
}

struct BoundedRecordWriter {
    bytes: Vec<u8>,
    json_limit: usize,
    exceeded: bool,
}

impl Write for BoundedRecordWriter {
    fn write(&mut self, input: &[u8]) -> std::io::Result<usize> {
        let Some(next_len) = self.bytes.len().checked_add(input.len()) else {
            self.exceeded = true;
            return Err(std::io::Error::other("bounded record length overflow"));
        };
        if next_len > self.json_limit {
            self.exceeded = true;
            return Err(std::io::Error::other("bounded record limit exceeded"));
        }
        if self.bytes.try_reserve_exact(input.len()).is_err() {
            self.exceeded = true;
            return Err(std::io::Error::other("bounded record allocation failed"));
        }
        self.bytes.extend_from_slice(input);
        Ok(input.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn encode_bounded_record<T: Serialize>(
    value: &T,
    record_limit: usize,
    limit_message: &str,
    batch_id: Option<u64>,
) -> Result<Vec<u8>, ExternalProtocolError> {
    let Some(json_limit) = record_limit.checked_sub(1) else {
        let error = ExternalProtocolError::new("limit", limit_message);
        return Err(batch_id.map_or(error.clone(), |batch| error.batch(batch)));
    };
    let mut writer = BoundedRecordWriter {
        bytes: Vec::new(),
        json_limit,
        exceeded: false,
    };
    if let Err(error) = serde_json::to_writer(&mut writer, value) {
        if writer.exceeded {
            let protocol_error = ExternalProtocolError::new("limit", limit_message);
            return Err(
                batch_id.map_or(protocol_error.clone(), |batch| protocol_error.batch(batch))
            );
        }
        return Err(ExternalProtocolError::new("json", error.to_string()));
    }
    writer.bytes.push(b'\n');
    Ok(writer.bytes)
}

fn decode_unique_json(bytes: &[u8]) -> Result<Value, ExternalProtocolError> {
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    let value = UniqueValueSeed
        .deserialize(&mut deserializer)
        .map_err(|error| ExternalProtocolError::new("json", error.to_string()))?;
    deserializer
        .end()
        .map_err(|error| ExternalProtocolError::new("json", error.to_string()))?;
    Ok(value)
}

struct UniqueValueSeed;

impl<'de> DeserializeSeed<'de> for UniqueValueSeed {
    type Value = Value;

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(UniqueValueVisitor)
    }
}

struct UniqueValueVisitor;

impl<'de> Visitor<'de> for UniqueValueVisitor {
    type Value = Value;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a JSON value without duplicate object keys")
    }

    fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E> {
        Ok(Value::Bool(value))
    }

    fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E> {
        Ok(Value::Number(value.into()))
    }

    fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E> {
        Ok(Value::Number(value.into()))
    }

    fn visit_f64<E>(self, value: f64) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        serde_json::Number::from_f64(value)
            .map(Value::Number)
            .ok_or_else(|| E::custom("non-finite JSON number"))
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E> {
        Ok(Value::String(value.to_owned()))
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E> {
        Ok(Value::String(value))
    }

    fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(Value::Null)
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(Value::Null)
    }

    fn visit_some<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        UniqueValueSeed.deserialize(deserializer)
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut values = Vec::new();
        while let Some(value) = sequence.next_element_seed(UniqueValueSeed)? {
            values.push(value);
        }
        Ok(Value::Array(values))
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut keys = BTreeSet::new();
        let mut values = serde_json::Map::new();
        while let Some(key) = map.next_key::<String>()? {
            if !keys.insert(key.clone()) {
                return Err(A::Error::custom(format!("duplicate object key `{key}`")));
            }
            values.insert(key, map.next_value_seed(UniqueValueSeed)?);
        }
        Ok(Value::Object(values))
    }
}
