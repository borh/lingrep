use std::sync::{Arc, Mutex};

use lingrep_core::analysis::{AnalyzerDescriptor, SemanticAnalysisIdentity};

const IDENTITY_SCHEMA_VERSION: u32 = 1;
const SOURCE_DOMAIN: &[u8] = b"lingrep-corpus/source-identity/v1";
const SOURCE_BYTES_DOMAIN: &[u8] = b"lingrep-corpus/source-bytes/v1";
const INVENTORY_DOMAIN: &[u8] = b"lingrep-corpus/source-inventory/v1";
const ANALYSIS_DOMAIN: &[u8] = b"lingrep-corpus/analysis-identity/v1";
const SOURCE_ENTRY_TAG: u8 = 1;
/// Non-document (context) bytes observed before any document: the tag that
/// frames [`SourceObservation::observe_context_file`]. Distinct from
/// [`SOURCE_ENTRY_TAG`] so the two frame shapes can never collide.
const CONTEXT_ENTRY_TAG: u8 = 2;
const END_TAG: u8 = 255;

macro_rules! identity_type {
    ($name:ident, $docs:literal) => {
        #[doc = $docs]
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub struct $name([u8; 32]);

        impl $name {
            /// Constructs an identity from its canonical BLAKE3-256 bytes.
            pub const fn from_bytes(bytes: [u8; 32]) -> Self {
                Self(bytes)
            }

            /// Returns the canonical BLAKE3-256 bytes.
            pub const fn as_bytes(&self) -> &[u8; 32] {
                &self.0
            }
        }
    };
}

identity_type!(
    SourceIdentity,
    "Identity of ordered logical sources and their bytes."
);
identity_type!(
    AnalysisIdentity,
    "Identity of source content and semantic analysis inputs."
);
identity_type!(
    ReaderIdentity,
    "Identity of the reader policy and its schema."
);
identity_type!(
    AdmissionIdentity,
    "Identity of sentence-admission policy and its schema."
);
identity_type!(
    DictionaryFingerprint,
    "Fingerprint of the tokenizer dictionary."
);
identity_type!(TokenSchemaIdentity, "Identity of the stored token schema.");

const READER_WITH_NORMALIZATION_DOMAIN: &[u8] = b"lingrep/reader-with-normalization/v1";

impl ReaderIdentity {
    /// Derives the identity of reading `config` under `normalization`.
    ///
    /// **The only derivation.** It is defined in the crate both binaries depend on
    /// so that `ling` and `lingrep-web` name one artifact for one JSONL root;
    /// the unframed
    /// [`reader_identity_digest`](lingrep_core::corpus::reader_identity_digest)
    /// names a different one.
    ///
    /// Normalization is an input because it determines the bytes a reader admits.
    /// It is also recorded on the analyzer descriptor, so an artifact's address
    /// distinguishes `nfc` from `none` twice over; that redundancy is retained
    /// to preserve stored artifact identities across versions.
    ///
    /// The domain string keeps `ling`'s original spelling. It names the
    /// derivation, not the crate that hosts it, and rewording it would move
    /// every artifact identity on disk.
    #[must_use]
    pub fn derive(
        config: &lingrep_core::corpus::CorpusReadConfig,
        normalization: lingrep_core::analysis::NormalizationPolicy,
    ) -> Self {
        let mut hash = blake3::Hasher::new();
        hash.update(READER_WITH_NORMALIZATION_DOMAIN);
        hash.update(&lingrep_core::corpus::reader_identity_digest(config));
        hash.update(normalization.identity_bytes());
        Self::from_bytes(*hash.finalize().as_bytes())
    }
}

/// Semantic inputs that distinguish analyses of the same sources.
///
/// The analyzer input is the descriptor's complete
/// [`SemanticAnalysisIdentity`], not a resource fingerprint. A resource
/// fingerprint covers the dictionary a run loaded and nothing else, so two
/// reachable configurations that change what a build *emits* — a Sudachi
/// split mode, a segmentation mode — used to derive one identity and address
/// one artifact. Keying on the descriptor closes that by construction: any
/// semantic difference the descriptor records is a different address.
///
/// The dictionary fingerprint survives as an independently recorded
/// inspection fact on the manifest. It is not an identity input.
///
/// Construct through [`AnalysisInputs::new`], which takes the descriptor and
/// derives the storage schema from it; the fields are private so the two
/// cannot drift.
#[derive(Clone)]
pub struct AnalysisInputs {
    reader: ReaderIdentity,
    admission: AdmissionIdentity,
    analyzer: SemanticAnalysisIdentity,
    token_schema: TokenSchemaIdentity,
}

impl AnalysisInputs {
    /// Builds inputs from a reader, an admission policy, and an analyzer
    /// descriptor.
    ///
    /// The storage schema identity is taken from the descriptor, so it always
    /// agrees with the semantics it is stored beside.
    #[must_use]
    pub fn new(
        reader: ReaderIdentity,
        admission: AdmissionIdentity,
        descriptor: &AnalyzerDescriptor,
    ) -> Self {
        Self {
            reader,
            admission,
            analyzer: descriptor.semantic_identity(),
            token_schema: TokenSchemaIdentity::from_bytes(*descriptor.schema_digest().as_bytes()),
        }
    }

    /// Builds inputs with an independently obtained storage schema identity,
    /// refusing one that disagrees with the descriptor.
    ///
    /// A caller that already holds a bound schema digest uses this to assert
    /// the agreement rather than assume it.
    pub fn with_token_schema(
        reader: ReaderIdentity,
        admission: AdmissionIdentity,
        descriptor: &AnalyzerDescriptor,
        token_schema: TokenSchemaIdentity,
    ) -> Result<Self, IdentityError> {
        let inputs = Self::new(reader, admission, descriptor);
        if inputs.token_schema != token_schema {
            return Err(IdentityError::TokenSchemaMismatch);
        }
        Ok(inputs)
    }

    /// Reconstructs inputs from facts a manifest already recorded.
    ///
    /// No descriptor is available on this path, so no agreement check runs:
    /// the values are read back exactly as the build that wrote them stored
    /// them.
    pub(crate) const fn from_recorded(
        reader: ReaderIdentity,
        admission: AdmissionIdentity,
        analyzer: SemanticAnalysisIdentity,
        token_schema: TokenSchemaIdentity,
    ) -> Self {
        Self {
            reader,
            admission,
            analyzer,
            token_schema,
        }
    }

    /// Returns the reader-policy identity.
    #[must_use]
    pub const fn reader(&self) -> ReaderIdentity {
        self.reader
    }

    /// Returns the sentence-admission identity.
    #[must_use]
    pub const fn admission(&self) -> AdmissionIdentity {
        self.admission
    }

    /// Returns the analyzer's complete semantic identity.
    #[must_use]
    pub const fn analyzer(&self) -> SemanticAnalysisIdentity {
        self.analyzer
    }

    /// Returns the stored token-schema identity.
    #[must_use]
    pub const fn token_schema(&self) -> TokenSchemaIdentity {
        self.token_schema
    }

    /// Returns these inputs with a different reader identity.
    #[must_use]
    pub fn with_reader(&self, reader: ReaderIdentity) -> Self {
        Self {
            reader,
            admission: self.admission,
            analyzer: self.analyzer,
            token_schema: self.token_schema,
        }
    }
}

/// Invalid source-observation state or a canonical-frame length overflow.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum IdentityError {
    #[error("source observation is not in a valid state for this operation")]
    InvalidState,
    #[error("canonical identity frame length overflow")]
    LengthOverflow,
    #[error("token schema identity disagrees with the analyzer descriptor")]
    TokenSchemaMismatch,
}

/// Builder-owned ordered source observation.
#[derive(Clone)]
pub struct SourceObservation {
    state: Arc<Mutex<ObservationState>>,
}

/// Completed source observation. Its digest fields cannot be caller-supplied.
pub struct ObservedSources {
    source_identity: SourceIdentity,
    ordered_inventory_digest: [u8; 32],
    document_count: u64,
}

enum ObservationPhase {
    Ready,
    Active {
        content: Box<blake3::Hasher>,
        content_len: u64,
    },
    Finished,
}

struct ObservationState {
    source: blake3::Hasher,
    inventory: blake3::Hasher,
    document_count: u64,
    /// Set by the first [`SourceObservation::begin_source`]: context files
    /// are only observable before the first document starts.
    started: bool,
    phase: ObservationPhase,
}

impl SourceObservation {
    /// Starts an empty, ordered observation.
    pub fn new() -> Self {
        let mut source = blake3::Hasher::new();
        let mut inventory = blake3::Hasher::new();
        update_prefix(&mut source, SOURCE_DOMAIN);
        update_prefix(&mut inventory, INVENTORY_DOMAIN);
        Self {
            state: Arc::new(Mutex::new(ObservationState {
                source,
                inventory,
                document_count: 0,
                started: false,
                phase: ObservationPhase::Ready,
            })),
        }
    }

    /// Observes one non-document (context) file in full: its name and
    /// content bytes reach `source_identity` only.
    ///
    /// Valid only in the `Ready` phase and only **before the first
    /// [`Self::begin_source`]**; mid-observation calls are
    /// [`IdentityError::InvalidState`]. The order among context files is
    /// the caller's — it must be byte-sorted, reproducible order — and the
    /// bytes are domain-separated under `SOURCE_BYTES_DOMAIN` exactly as a
    /// document's content is.
    ///
    /// Deliberate restrictions: it does **not** increment `document_count`
    /// (a context file is not a document, and `document_count` is
    /// cross-checked against the discovery inventory) and it does **not**
    /// touch the `inventory` hasher, so `ordered_inventory_digest` stays
    /// the ordered logical *document* names.
    pub fn observe_context_file(
        &self,
        logical_name: &str,
        bytes: &[u8],
    ) -> Result<(), IdentityError> {
        let mut state = self.state.lock().map_err(|_| IdentityError::InvalidState)?;
        if !matches!(state.phase, ObservationPhase::Ready) || state.started {
            return Err(IdentityError::InvalidState);
        }
        let name_len = checked_len(logical_name.len())?;
        let byte_len = checked_len(bytes.len())?;
        state.source.update(&[CONTEXT_ENTRY_TAG]);
        state.source.update(&name_len.to_le_bytes());
        state.source.update(logical_name.as_bytes());
        state.source.update(&byte_len.to_le_bytes());
        let mut content = blake3::Hasher::new();
        update_prefix(&mut content, SOURCE_BYTES_DOMAIN);
        content.update(bytes);
        state.source.update(content.finalize().as_bytes());
        Ok(())
    }

    /// Begins exactly one logical source in the observation order.
    pub fn begin_source(&self, logical_name: &str) -> Result<(), IdentityError> {
        let mut state = self.state.lock().map_err(|_| IdentityError::InvalidState)?;
        if !matches!(state.phase, ObservationPhase::Ready) {
            return Err(IdentityError::InvalidState);
        }
        let name_len = checked_len(logical_name.len())?;
        state.started = true;
        state.source.update(&[SOURCE_ENTRY_TAG]);
        state.source.update(&name_len.to_le_bytes());
        state.source.update(logical_name.as_bytes());
        state.inventory.update(&[SOURCE_ENTRY_TAG]);
        state.inventory.update(&name_len.to_le_bytes());
        state.inventory.update(logical_name.as_bytes());

        let mut content = blake3::Hasher::new();
        update_prefix(&mut content, SOURCE_BYTES_DOMAIN);
        state.phase = ObservationPhase::Active {
            content: Box::new(content),
            content_len: 0,
        };
        Ok(())
    }

    /// Observes raw bytes for the active logical source.
    pub fn update_source_bytes(&self, chunk: &[u8]) -> Result<(), IdentityError> {
        let mut state = self.state.lock().map_err(|_| IdentityError::InvalidState)?;
        let ObservationPhase::Active {
            content,
            content_len,
        } = &mut state.phase
        else {
            return Err(IdentityError::InvalidState);
        };
        *content_len = content_len
            .checked_add(checked_len(chunk.len())?)
            .ok_or(IdentityError::LengthOverflow)?;
        content.update(chunk);
        Ok(())
    }

    /// Completes the active logical source.
    pub fn end_source(&self) -> Result<(), IdentityError> {
        let mut state = self.state.lock().map_err(|_| IdentityError::InvalidState)?;
        let (content_digest, content_len) = match &state.phase {
            ObservationPhase::Active {
                content,
                content_len,
            } => (*content.clone().finalize().as_bytes(), *content_len),
            ObservationPhase::Ready | ObservationPhase::Finished => {
                return Err(IdentityError::InvalidState);
            }
        };
        state.phase = ObservationPhase::Ready;
        state.source.update(&content_len.to_le_bytes());
        state.source.update(&content_digest);
        state.document_count = state
            .document_count
            .checked_add(1)
            .ok_or(IdentityError::LengthOverflow)?;
        Ok(())
    }

    /// Finalizes this observation and makes all later transitions invalid.
    pub fn finish(&self) -> Result<ObservedSources, IdentityError> {
        let mut state = self.state.lock().map_err(|_| IdentityError::InvalidState)?;
        if !matches!(state.phase, ObservationPhase::Ready) {
            return Err(IdentityError::InvalidState);
        }
        let document_count = state.document_count;
        state.source.update(&[END_TAG]);
        state.source.update(&document_count.to_le_bytes());
        state.inventory.update(&[END_TAG]);
        state.inventory.update(&document_count.to_le_bytes());
        let source_identity = SourceIdentity(*state.source.clone().finalize().as_bytes());
        let ordered_inventory_digest = *state.inventory.clone().finalize().as_bytes();
        state.phase = ObservationPhase::Finished;
        Ok(ObservedSources {
            source_identity,
            ordered_inventory_digest,
            document_count,
        })
    }
}

impl Default for SourceObservation {
    fn default() -> Self {
        Self::new()
    }
}

impl ObservedSources {
    /// Returns the identity produced from this builder-owned observation.
    pub fn source_identity(&self) -> &SourceIdentity {
        &self.source_identity
    }

    /// Returns the digest of ordered logical source names.
    pub fn ordered_inventory_digest(&self) -> &[u8; 32] {
        &self.ordered_inventory_digest
    }

    /// Returns the number of completed logical source documents.
    pub fn document_count(&self) -> u64 {
        self.document_count
    }
}

/// Derives the semantic analysis identity. Physical cache encoding is not an input.
pub fn derive_analysis_identity(
    observed: &ObservedSources,
    inputs: &AnalysisInputs,
) -> AnalysisIdentity {
    derive_analysis_identity_from_source(observed.source_identity(), inputs)
}

pub(crate) fn derive_analysis_identity_from_source(
    source_identity: &SourceIdentity,
    inputs: &AnalysisInputs,
) -> AnalysisIdentity {
    let mut hasher = blake3::Hasher::new();
    update_prefix(&mut hasher, ANALYSIS_DOMAIN);
    hasher.update(source_identity.as_bytes());
    hasher.update(inputs.reader.as_bytes());
    hasher.update(inputs.admission.as_bytes());
    hasher.update(inputs.analyzer.as_bytes());
    hasher.update(inputs.token_schema.as_bytes());
    AnalysisIdentity(*hasher.finalize().as_bytes())
}

fn update_prefix(hasher: &mut blake3::Hasher, domain: &[u8]) {
    hasher.update(
        &u32::try_from(domain.len())
            .expect("fixed domain fits u32")
            .to_le_bytes(),
    );
    hasher.update(domain);
    hasher.update(&IDENTITY_SCHEMA_VERSION.to_le_bytes());
}

fn checked_len(length: usize) -> Result<u64, IdentityError> {
    u64::try_from(length).map_err(|_| IdentityError::LengthOverflow)
}

#[cfg(test)]
mod reader_identity_tests {
    use lingrep_core::analysis::NormalizationPolicy;
    use lingrep_core::corpus::{CorpusReadConfig, MarkdownReaderConfig, SectionPolicy};

    use super::ReaderIdentity;

    fn hex(identity: &ReaderIdentity) -> String {
        identity
            .as_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    #[test]
    fn structural_reader_identities_are_pinned() {
        let markdown =
            CorpusReadConfig::Markdown(MarkdownReaderConfig::new(SectionPolicy::default()));
        for (config, normalization, expected) in [
            (
                &CorpusReadConfig::Jsonl,
                NormalizationPolicy::Nfc,
                "65d0a037af9c05ba6473371af4f3b1c10aa6c6131335f6e8fcad4f3cddeb735b",
            ),
            (
                &CorpusReadConfig::Jsonl,
                NormalizationPolicy::None,
                "3343fdcf585975f8e90f93cd0b6b33130bdce35a857e58d3cb9b1a134b7043e8",
            ),
            (
                &CorpusReadConfig::Plain,
                NormalizationPolicy::Nfc,
                "4929553aefc1959cfd5e0e595d7e6d79fc72cd6fd1d55ce0ca85fff60997820d",
            ),
            (
                &markdown,
                NormalizationPolicy::Nfc,
                "ea21e88cc5d2a659d5320dc455846c10392dae1a031bc3fe1f8c14280e9223f1",
            ),
            (
                &CorpusReadConfig::Bccwj,
                NormalizationPolicy::Nfc,
                "d1ee857e00bfa141eec574e1a247f9189d343a9f9b920ce35075b8fc4d7fc2d7",
            ),
        ] {
            assert_eq!(
                hex(&ReaderIdentity::derive(config, normalization)),
                expected,
                "{normalization:?} over {config:?} changed its artifact address"
            );
        }
    }

    /// Normalization is an input, so the two policies never collide.
    #[test]
    fn the_two_normalization_policies_derive_distinct_identities() {
        assert_ne!(
            ReaderIdentity::derive(&CorpusReadConfig::Jsonl, NormalizationPolicy::Nfc),
            ReaderIdentity::derive(&CorpusReadConfig::Jsonl, NormalizationPolicy::None)
        );
    }
}
