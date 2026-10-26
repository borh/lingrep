use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{Read, SeekFrom},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use lingrep_core::{
    analysis::AnalyzedInput,
    corpus::{
        AdmittedCorpusEvent, DiscoveryReport, InventoryStats, Metadata, SourceDocument,
        conllu::EnhancedGraphObservation,
    },
    token_match::{
        AttributeProvider, BoundProjection, BoundTokenSentence, TokenAttributeIndex, TokenInfo,
        TokenSchema,
    },
};

use crate::{
    AnalysisIdentity, AnalysisInputs, CorpusError, PreparedSentenceSource, ProvenanceView,
    RichSentenceMetadata, SentenceId, SentenceLocation, SourceIdentity, SourceObservation,
    TokenSchemaIdentity, derive_analysis_identity,
    enhanced::{ENHANCED_NAME, EnhancedReader, EnhancedWriter},
    identity::derive_analysis_identity_from_source,
    locations::{LocationDocument, LocationWriter, MappedLocations},
    manifest::{
        AdmittedInventoryHasher, LOCATIONS_NAME, MANIFEST_NAME, Manifest, ManifestInputs,
        PROVENANCE_NAME, TOKENS_NAME, component_fact_guarded, encode_digest,
    },
    provenance::{AnalysisRecord, ProvenanceHandle},
    scratch::BudgetedScratch,
    tokens::{TokenReader, TokenWriter},
};

static STAGING_SEQUENCE: AtomicU64 = AtomicU64::new(0);

const UNMAPPED_TARGET_GROUP: u64 = u64::MAX;

/// Generation embedded in content-addressed artifact cache paths.
pub const CORPUS_ARTIFACT_FORMAT_GENERATION: u32 = 2;

/// Formats identity bytes as lowercase hex (64 characters for a 32-byte identity).
///
/// This is the crate's one spelling of an identity digest: it names the
/// analysis identity in a [`canonical_artifact_path`] and the identity
/// digests a manifest records, so the two agree by construction.
pub fn format_identity(bytes: &[u8; 32]) -> String {
    crate::manifest::encode_digest(bytes)
}

/// Version of the analysis-identity derivation used by a lingrep artifact.
#[derive(
    Clone, Copy, Debug, Eq, Hash, PartialEq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub enum IdentityScheme {
    #[serde(rename = "a1")]
    V1,
}

impl IdentityScheme {
    pub const CURRENT: Self = Self::V1;

    #[must_use]
    pub const fn address_component(self) -> &'static str {
        "a1"
    }

    fn parse(value: &str) -> Result<Self, CorpusError> {
        match value {
            "a1" => Ok(Self::V1),
            _ => Err(invalid_artifact("artifact identity scheme is unsupported")),
        }
    }
}

/// Returns the canonical prepared-artifact destination for a source root,
/// named `<source-name>.g<generation>.a<scheme>.<analysis-identity>.<trust>.lingcorpus`
/// and placed beside the root.
///
/// The [`CORPUS_ARTIFACT_FORMAT_GENERATION`], the [`IdentityScheme`] and the
/// [`AnalysisIdentity`] in the name make the path content-addressed: a
/// different analysis, identity scheme, or format generation yields a
/// different path. This is the one placement rule; [`crate::CorpusArtifact`]
/// consumers and artifact producers must not re-derive it locally.
///
/// A root with no file name of its own — `/`, `.`, or a path ending in `..` — has
/// nothing to sit beside, so the name is built from an empty stem and lands
/// *inside* that root rather than next to it. This function is pure and reads
/// no filesystem; a caller that needs the nesting case resolved canonicalizes
/// the root before naming.
pub fn canonical_artifact_path(
    root: &Path,
    analysis_identity: &AnalysisIdentity,
    trust: TrustClass,
) -> PathBuf {
    let mut name = root.file_name().unwrap_or_default().to_os_string();
    let scheme = format!("{}.", IdentityScheme::CURRENT.address_component());
    name.push(format!(
        ".g{}.{}{}.{}.lingcorpus",
        CORPUS_ARTIFACT_FORMAT_GENERATION,
        scheme,
        format_identity(analysis_identity.as_bytes()),
        trust.address_component(),
    ));
    root.with_file_name(name)
}

/// Facts a canonical artifact file name carries.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParsedArtifactName {
    /// File name of the source this artifact realizes.
    pub source_name: String,
    pub format_generation: u32,
    pub scheme: IdentityScheme,
    pub analysis_identity: AnalysisIdentity,
    pub trust: TrustClass,
}

/// Parses a canonical lingrep artifact file name.
///
/// Returns `None` for any name that is not a canonical artifact name, which is
/// how a sibling scan skips an unrelated directory entry without treating it
/// as a defect.
#[must_use]
pub fn parse_artifact_name(name: &str) -> Option<ParsedArtifactName> {
    let stem = name.strip_suffix(".lingcorpus")?;
    let (rest, trust) = stem.rsplit_once('.')?;
    let trust = TrustClass::parse(trust).ok()?;
    let (rest, identity) = rest.rsplit_once('.')?;
    let analysis_identity = AnalysisIdentity::from_bytes(decode_hex_identity(identity)?);
    let (rest, scheme) = rest.rsplit_once('.')?;
    let scheme = IdentityScheme::parse(scheme).ok()?;
    let (source_name, generation) = rest.rsplit_once(".g")?;
    let format_generation = generation.parse().ok()?;
    Some(ParsedArtifactName {
        source_name: source_name.to_owned(),
        format_generation,
        scheme,
        analysis_identity,
        trust,
    })
}

fn decode_hex_identity(value: &str) -> Option<[u8; 32]> {
    if value.len() != 64 {
        return None;
    }
    let mut bytes = [0_u8; 32];
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(value.get(index * 2..index * 2 + 2)?, 16).ok()?;
    }
    Some(bytes)
}

/// Trust boundary under which analyzer output was admitted.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum TrustClass {
    LocallyExecuted,
    ExternallyAttested,
}

impl TrustClass {
    pub const fn address_component(self) -> &'static str {
        match self {
            Self::LocallyExecuted => "locally-executed",
            Self::ExternallyAttested => "externally-attested",
        }
    }

    pub(crate) fn parse(value: &str) -> Result<Self, CorpusError> {
        match value {
            "locally-executed" => Ok(Self::LocallyExecuted),
            "externally-attested" => Ok(Self::ExternallyAttested),
            _ => Err(invalid_artifact("manifest trust class is invalid")),
        }
    }
}

#[cfg(test)]
mod candidate_address_tests {
    use super::*;

    #[test]
    fn trust_classes_are_distinct_realization_namespaces() {
        let identity = AnalysisIdentity::from_bytes([7; 32]);
        let local = canonical_artifact_path(
            Path::new("corpus.jsonl"),
            &identity,
            TrustClass::LocallyExecuted,
        );
        let attested = canonical_artifact_path(
            Path::new("corpus.jsonl"),
            &identity,
            TrustClass::ExternallyAttested,
        );
        assert_ne!(local, attested);
        assert!(local.to_string_lossy().contains(".locally-executed."));
        assert!(attested.to_string_lossy().contains(".externally-attested."));
    }
}

/// Maximum number of sentence ordinals composed by one shared provenance page.
pub const MAX_PROVENANCE_PAGE_SIZE: usize = 1_024;

/// Semantic and physical inputs for one prepared corpus build.
pub struct BuildConfig {
    pub analysis: AnalysisInputs,
    pub analyzer: AnalyzerRecord,
    /// Dictionary fingerprint recorded for inspection.
    ///
    /// Not an identity input. The analyzer's own semantic identity carries
    /// every resource that reaches output, including this one; recording the
    /// fingerprint separately is what lets `inspect` and `doctor` name the
    /// exact dictionary a build loaded.
    pub dictionary: crate::DictionaryFingerprint,
    pub trust: TrustClass,
    pub temp_budget: Arc<lingrep_core::spill::TempBudget>,
    /// The reader's static declaration of sentence-grain metadata keys
    /// (`CorpusFormat::sentence_metadata_keys`), recorded in the provenance
    /// sidecar for discovery, including fields absent from all sentences.
    /// Scope ownership is explicit and does not depend on this inventory.
    /// Empty for every format except CoNLL-U.
    pub sentence_metadata_keys: Vec<String>,
}

/// Stable analyzer semantics stored with a V1 artifact and published by consumers.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnalyzerRecord {
    pub family: String,
    pub profile: String,
    pub segmentation: String,
    pub normalization: String,
    pub normalization_unicode_version: Option<[u8; 3]>,
    pub language_policy: String,
    pub boundary_policy: String,
    pub semantic_identity: String,
}

impl AnalyzerRecord {
    #[must_use]
    pub fn from_descriptor(descriptor: &lingrep_core::analysis::AnalyzerDescriptor) -> Self {
        use lingrep_core::analysis::{
            AnalyzerFamily, ExtractionProfile, NormalizationPolicy, SegmentationMode,
        };
        let family = match descriptor.family() {
            AnalyzerFamily::Vibrato => "vibrato",
            AnalyzerFamily::Sudachi => "sudachi",
            AnalyzerFamily::Spacy => "spacy",
            AnalyzerFamily::Stanza => "stanza",
            AnalyzerFamily::Pretokenized => "pretokenized",
            AnalyzerFamily::External => "external",
        };
        let profile = match descriptor.profile() {
            ExtractionProfile::Tokenize => "tokenize",
            ExtractionProfile::Morphology => "morphology",
            ExtractionProfile::Syntax => "syntax",
        };
        let segmentation = match descriptor.segmentation() {
            SegmentationMode::Input => "input",
            SegmentationMode::Analyzer => "analyzer",
        };
        let normalization = match descriptor.normalization() {
            NormalizationPolicy::None => "none",
            NormalizationPolicy::Nfc => "nfc",
        };
        Self {
            family: family.into(),
            profile: profile.into(),
            segmentation: segmentation.into(),
            normalization: normalization.into(),
            normalization_unicode_version: descriptor.normalization().unicode_version(),
            language_policy: descriptor.language_policy().into(),
            boundary_policy: descriptor.boundary_policy().into(),
            semantic_identity: encode_digest(descriptor.semantic_identity().as_bytes()),
        }
    }

    /// The token schema this build produces for the analyzer this record
    /// describes, or `None` when it cannot say.
    ///
    /// Reads back the two fields [`Self::from_descriptor`] wrote and asks
    /// [`lingrep_core::analysis::declared_token_schema`]. The inverse mapping lives
    /// here, beside the forward one, so a family renamed in one is a
    /// compile-time hole in the other.
    ///
    /// An unrecognized family or profile is `None` rather than an error: this
    /// answers "what would this build make of that analyzer", and "a family
    /// this build does not have" is a legitimate answer to it, not a corrupt
    /// artifact.
    #[must_use]
    pub fn declared_token_schema(&self) -> Option<lingrep_core::token_match::TokenSchema> {
        use lingrep_core::analysis::{AnalyzerFamily, ExtractionProfile};
        let family = match self.family.as_str() {
            "vibrato" => AnalyzerFamily::Vibrato,
            "sudachi" => AnalyzerFamily::Sudachi,
            "spacy" => AnalyzerFamily::Spacy,
            "stanza" => AnalyzerFamily::Stanza,
            "pretokenized" => AnalyzerFamily::Pretokenized,
            "external" => AnalyzerFamily::External,
            _ => return None,
        };
        let profile = match self.profile.as_str() {
            "tokenize" => ExtractionProfile::Tokenize,
            "morphology" => ExtractionProfile::Morphology,
            "syntax" => ExtractionProfile::Syntax,
            _ => return None,
        };
        lingrep_core::analysis::declared_token_schema(family, profile)
    }

    /// Minimal deterministic record for storage-focused fixtures.
    ///
    /// Derived from [`vibrato_fixture_descriptor`], so the record's
    /// `semantic_identity` and the [`AnalysisInputs`] a fixture build derives
    /// its address from agree. They must: analysis identity now keys on the
    /// analyzer semantics this record stores, and
    /// [`CorpusArtifact::open`] re-derives and compares the two.
    #[doc(hidden)]
    #[must_use]
    pub fn vibrato_fixture() -> Self {
        Self::from_descriptor(&vibrato_fixture_descriptor())
    }
}

/// Minimal deterministic analyzer description for storage-focused fixtures.
///
/// Its resource digests are literals no real dictionary produces, which is the
/// point: a fixture must be recognizable as one. The durability gate refuses it
/// as a fixture source for exactly that reason — a stored artifact that proves
/// a descriptor comparison works has to carry a descriptor a production build
/// could emit.
///
/// The schema digest is the real stored V1 morphology schema, because
/// publication validates by opening what it wrote and an artifact recording a
/// featurization no build produces cannot open.
#[doc(hidden)]
#[must_use]
pub fn vibrato_fixture_descriptor() -> lingrep_core::analysis::AnalyzerDescriptor {
    use lingrep_core::analysis::{
        AnalyzerDescriptor, AnalyzerFamily, ExtractionProfile, NormalizationPolicy,
        SegmentationMode,
    };
    AnalyzerDescriptor::new(
        AnalyzerFamily::Vibrato,
        ExtractionProfile::Morphology,
        SegmentationMode::Input,
        NormalizationPolicy::Nfc,
        "ja",
        "input-unit",
        [0xf1; 32],
        [0xf2; 32],
        [0xf3; 32],
        [0xf4; 32],
        [0xf5; 32],
        [0xf6; 32],
        lingrep_core::token_match::TokenSchema::unidic_morphology().digest(),
    )
    .expect("fixture descriptor policies are nonempty")
}

/// One document projected from the artifact's dense document relation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredDocument {
    pub ordinal: u64,
    pub logical_name: String,
    pub target_group_ordinal: u64,
    pub target_multiplicity: u64,
    pub external_target: bool,
}

/// A prepared artifact's recorded identity.
///
/// Read from the manifest, never recomputed. [`PreparedIdentity::digest`]
/// folds it into one value for callers that need a single comparable string.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreparedIdentity {
    pub schema_version: u32,
    pub source_identity: String,
    pub analysis_identity: String,
    pub dictionary_fingerprint: String,
    pub token_schema_identity: String,
    pub ordered_inventory_digest: String,
    pub admitted_inventory_digest: String,
    /// Component name to its recorded BLAKE3, including the token and
    /// location components.
    pub components: std::collections::BTreeMap<String, String>,
    pub document_count: u64,
    pub sentence_count: u64,
    pub token_count: u64,
}

impl PreparedIdentity {
    /// One comparable value over every recorded fact.
    #[must_use]
    pub fn digest(&self) -> String {
        let mut hasher = blake3::Hasher::new();
        let mut field = |tag: &str, value: &str| {
            hasher.update(tag.as_bytes());
            hasher.update(b"\x1f");
            hasher.update(value.as_bytes());
            hasher.update(b"\x1e");
        };
        field("schema", &self.schema_version.to_string());
        field("source", &self.source_identity);
        field("analysis", &self.analysis_identity);
        field("dictionary", &self.dictionary_fingerprint);
        field("token-schema", &self.token_schema_identity);
        field("ordered-inventory", &self.ordered_inventory_digest);
        field("admitted-inventory", &self.admitted_inventory_digest);
        for (name, digest) in &self.components {
            field("component", name);
            field("component-digest", digest);
        }
        field("documents", &self.document_count.to_string());
        field("sentences", &self.sentence_count.to_string());
        field("tokens", &self.token_count.to_string());
        format!("blake3:{}", hasher.finalize().to_hex())
    }
}

/// Authoritative and rich provenance for one stored sentence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SentenceProvenance {
    pub location: SentenceLocation,
    pub rich: RichSentenceMetadata,
    pub active_regions: Box<[crate::ActiveRegion]>,
}

/// A complete, unpublished artifact directory.
#[derive(Debug)]
pub(crate) struct CompletedStagingArtifact {
    staging: StagingOwner,
    destination: PathBuf,
    token_name: &'static str,
    has_enhanced: bool,
    has_postings: bool,
}

impl CompletedStagingArtifact {
    /// Returns the private staging path for publication and staging contracts.
    pub(crate) fn path(&self) -> &Path {
        self.staging.path()
    }

    pub(crate) fn destination(&self) -> &Path {
        &self.destination
    }

    pub(crate) fn publication_components(&self) -> Vec<PathBuf> {
        let mut components = vec![
            self.staging.join(self.token_name),
            self.staging.join(LOCATIONS_NAME),
            self.staging.join(PROVENANCE_NAME),
        ];
        if self.has_enhanced {
            components.push(self.staging.join(ENHANCED_NAME));
        }
        if self.has_postings {
            components.push(self.staging.join(crate::postings::NAME));
        }
        components.push(self.staging.join(MANIFEST_NAME));
        components
    }

    pub(crate) fn mark_published(&mut self) {
        self.staging.disarm();
    }
}

/// Streaming builder for a complete but not yet published artifact.
///
/// Staging finalization is sealed inside `lingrep-corpus`; callers must cross the
/// immutable no-replace publication boundary through [`CorpusBuilder::finish`].
///
/// ```compile_fail
/// use lingrep_corpus::CorpusBuilder;
///
/// let _ = CorpusBuilder::finalize_staging;
/// ```
pub struct CorpusBuilder {
    config: BuildConfig,
    destination: PathBuf,
    observation: SourceObservation,
    discovered_inventory: SourceObservation,
    registered_inventory: SourceObservation,
    admitted_inventory: SourceObservation,
    token_writer: Option<TokenWriter>,
    enhanced_writer: Option<EnhancedWriter>,
    location_writer: Option<LocationWriter>,
    provenance_writer: Option<ProvenanceHandle>,
    pending_source: Option<RegisteredDocument>,
    current_document: Option<CurrentDocument>,
    active_divisions: std::collections::BTreeMap<
        lingrep_core::corpus::DivisionId,
        Arc<lingrep_core::corpus::Division>,
    >,
    division_observations:
        std::collections::BTreeMap<String, lingrep_core::corpus::DivisionObservation>,
    pending_tokens: Option<SentenceLocation>,
    pending_enhanced_graph: Option<EnhancedGraphObservation>,
    logical_token_schema: TokenSchema,
    physical_projection: BoundProjection,
    physical_token_schema: TokenSchemaIdentity,
    discovered_documents: u64,
    discovered_distinct_target_groups: u64,
    discovered_target_counts: BudgetedScratch,
    discovered_to_admitted_groups: BudgetedScratch,
    discovered_max_target_multiplicity: u64,
    discovered_external_targets: u64,
    registered_documents: u64,
    admitted_documents: u64,
    admitted_max_target_multiplicity: u64,
    admitted_external_targets: u64,
    admitted_inventory_digest: AdmittedInventoryHasher,
    sentence_count: u64,
    token_count: u64,
    /// Sentences the caller truncated to fit its sentence limit, accumulated
    /// here so the manifest records what the build actually admitted rather
    /// than what it was asked to admit.
    sentence_truncation: Option<crate::SentenceTruncation>,
    distinct_target_groups: u64,
    discovery_digest: blake3::Hasher,
    staging: StagingOwner,
}

/// Immutable, normally size-independent open view of one prepared artifact.
pub struct CorpusArtifact {
    pub(crate) root: PathBuf,
    pub(crate) manifest: Manifest,
    source_identity: SourceIdentity,
    pub(crate) analysis_identity: AnalysisIdentity,
    pub(crate) physical_token_schema: crate::TokenSchemaIdentity,
    logical_token_schema: TokenSchema,
    pub(crate) discovery: DiscoveryReport,
    trust: TrustClass,
    locations: MappedLocations,
    pub(crate) enhanced: Option<Arc<EnhancedReader>>,
    pub(crate) postings: Option<Arc<crate::postings::PostingIndex>>,
}

#[derive(Clone, Debug)]
struct RegisteredDocument {
    ordinal: u64,
    logical_name: String,
    target_group_ordinal: u64,
    external_target: bool,
}

struct CurrentDocument {
    id: crate::DocumentId,
    ordinal: u64,
    source_name: Arc<str>,
    metadata_json: Option<String>,
}

impl CorpusBuilder {
    /// Creates a uniquely named private sibling staging directory.
    pub async fn begin(config: BuildConfig, destination: &Path) -> Result<Self, CorpusError> {
        Self::begin_with_schema(
            config,
            destination,
            lingrep_core::token_match::TokenSchema::unidic_morphology(),
        )
        .await
    }

    /// Creates a V1 artifact for one complete canonical analyzer profile schema.
    pub async fn begin_with_schema(
        config: BuildConfig,
        destination: &Path,
        storage_schema: lingrep_core::token_match::TokenSchema,
    ) -> Result<Self, CorpusError> {
        if config.analysis.token_schema().as_bytes() != storage_schema.digest().as_bytes() {
            return Err(invalid_builder(
                "analysis token-schema identity differs from the selected storage schema",
            ));
        }
        let logical_token_schema = storage_schema;
        let graph_capable = logical_token_schema
            .attributes()
            .iter()
            .any(|attribute| attribute.provider() == AttributeProvider::EnhancedGraph);
        if logical_token_schema.attributes().iter().any(|attribute| {
            attribute.provider() == AttributeProvider::EnhancedGraph
                && (attribute.key().as_str() != "dep.enhanced"
                    || attribute.kind() != lingrep_core::token_match::AttributeKind::StringSet)
        }) {
            return Err(invalid_builder(
                "the enhanced graph provider requires the dep.enhanced string-set key",
            ));
        }
        let physical_attributes = logical_token_schema
            .attributes()
            .iter()
            .filter(|attribute| attribute.provider() != AttributeProvider::EnhancedGraph)
            .cloned()
            .collect::<Vec<_>>();
        if physical_attributes.is_empty() {
            return Err(invalid_builder(
                "prepared storage requires at least one physical token attribute",
            ));
        }
        let physical_schema = TokenSchema::new(physical_attributes)
            .map_err(|error| invalid_builder(format!("invalid physical token schema: {error}")))?;
        let physical_projection = physical_schema
            .bind_projection(physical_schema.keys().iter().cloned())
            .map_err(|error| invalid_builder(format!("invalid physical projection: {error}")))?;
        let physical_token_schema =
            TokenSchemaIdentity::from_bytes(*physical_schema.digest().as_bytes());
        let parent = destination.parent().unwrap_or_else(|| Path::new("."));
        let base = destination
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("corpus");
        let staging = create_staging(parent, base)?;
        let tokens_name = TOKENS_NAME;
        let token_writer = TokenWriter::create_indexed(
            &staging.join(tokens_name),
            physical_schema,
            Arc::clone(&config.temp_budget),
        )?;
        let location_writer = LocationWriter::create(
            &staging.join(LOCATIONS_NAME),
            Arc::clone(&config.temp_budget),
        )?;
        let provenance_writer = ProvenanceHandle::create(
            &staging.join(PROVENANCE_NAME),
            config.sentence_metadata_keys.clone(),
        )?;
        let enhanced_writer = graph_capable
            .then(|| EnhancedWriter::create(&staging.join(ENHANCED_NAME), &config.temp_budget))
            .transpose()?;
        let discovered_target_counts = BudgetedScratch::create(
            &staging.join(".discovered-target-counts.scratch"),
            &config.temp_budget,
        )?;
        let discovered_to_admitted_groups = BudgetedScratch::create(
            &staging.join(".discovered-to-admitted-groups.scratch"),
            &config.temp_budget,
        )?;
        Ok(Self {
            config,
            destination: destination.to_path_buf(),
            observation: SourceObservation::new(),
            discovered_inventory: SourceObservation::new(),
            registered_inventory: SourceObservation::new(),
            admitted_inventory: SourceObservation::new(),
            token_writer: Some(token_writer),
            enhanced_writer,
            location_writer: Some(location_writer),
            provenance_writer: Some(provenance_writer),
            pending_source: None,
            current_document: None,
            active_divisions: Default::default(),
            division_observations: Default::default(),
            pending_tokens: None,
            pending_enhanced_graph: None,
            logical_token_schema,
            physical_projection,
            physical_token_schema,
            discovered_documents: 0,
            discovered_distinct_target_groups: 0,
            discovered_target_counts,
            discovered_to_admitted_groups,
            discovered_max_target_multiplicity: 0,
            discovered_external_targets: 0,
            registered_documents: 0,
            admitted_documents: 0,
            admitted_max_target_multiplicity: 0,
            admitted_external_targets: 0,
            admitted_inventory_digest: AdmittedInventoryHasher::new(),
            sentence_count: 0,
            token_count: 0,
            sentence_truncation: None,
            distinct_target_groups: 0,
            discovery_digest: blake3::Hasher::new(),
            staging,
        })
    }

    /// Returns the builder-owned raw-byte observation handle.
    pub fn source_observation(&self) -> SourceObservation {
        self.observation.clone()
    }

    /// Records that the caller truncated `count` sentences to fit `limit`.
    ///
    /// The builder never truncates: it is handed already-admitted text, and
    /// what "too long" means belongs to the caller's execution limits. What it
    /// owns is the *record*, because the limit stops being invisible the moment
    /// it can shorten a sentence — see
    /// [`SentenceTruncation`](crate::SentenceTruncation).
    ///
    /// Calls accumulate, so a caller may report per window rather than once.
    pub fn record_sentence_truncation(&mut self, count: u64, limit: u64) {
        if count == 0 {
            return;
        }
        let total = self
            .sentence_truncation
            .map_or(count, |recorded| recorded.count.saturating_add(count));
        self.sentence_truncation = Some(crate::SentenceTruncation {
            count: total,
            limit,
        });
    }

    /// Finalizes and atomically publishes one immutable artifact.
    pub async fn finish(self, discovery: DiscoveryReport) -> Result<CorpusArtifact, CorpusError> {
        self.finish_guarded(discovery, || true, || true)
            .await?
            .ok_or_else(|| invalid_builder("unconditional publication guard cancelled"))
    }

    /// Publishes only when the builder-observed analysis still matches a
    /// caller's preflight identity.
    pub async fn finish_expected_analysis(
        self,
        discovery: DiscoveryReport,
        expected: AnalysisIdentity,
    ) -> Result<CorpusArtifact, CorpusError> {
        let mut should_continue = || true;
        let mut completed = self
            .finalize_staging_guarded(discovery, &mut should_continue)
            .await?
            .ok_or_else(|| invalid_builder("unconditional staging finalization cancelled"))?;
        let artifact = CorpusArtifact::open(completed.path())?;
        if artifact.analysis_identity() != &expected {
            return Err(CorpusError::AnalysisIdentityMismatch {
                component: MANIFEST_NAME,
            });
        }
        let destination = completed.destination().to_path_buf();
        if crate::publication::publish_guarded(&mut completed, || true, || true)? {
            Ok(artifact.into_published_at(destination))
        } else {
            Err(invalid_builder("unconditional publication guard cancelled"))
        }
    }

    /// Finalizes privately, then publishes only while the supplied guards allow it.
    ///
    /// `should_continue` is checked between durability operations. `claim_commit`
    /// is called exactly once immediately before the no-replace rename and
    /// should atomically exclude later cancellation. Returning `false` from
    /// either guard drops the private staging directory and returns `Ok(None)`.
    pub async fn finish_guarded(
        self,
        discovery: DiscoveryReport,
        mut should_continue: impl FnMut() -> bool,
        claim_commit: impl FnOnce() -> bool,
    ) -> Result<Option<CorpusArtifact>, CorpusError> {
        let Some(mut completed) = self
            .finalize_staging_guarded(discovery, &mut should_continue)
            .await?
        else {
            return Ok(None);
        };
        if !should_continue() {
            return Ok(None);
        }
        let artifact = CorpusArtifact::open(completed.path())?;
        let destination = completed.destination().to_path_buf();
        if crate::publication::publish_guarded(&mut completed, should_continue, claim_commit)? {
            Ok(Some(artifact.into_published_at(destination)))
        } else {
            Ok(None)
        }
    }

    /// Registers the discovery record paired with the next admitted document.
    ///
    /// Physical paths and filesystem identities are deliberately discarded.
    pub fn register_source_document(&mut self, source: &SourceDocument) -> Result<(), CorpusError> {
        self.register_discovered_source(source)?;
        self.register_admitted_document_from_source(source.ordinal, &source.logical_name, source)
    }

    /// Registers one physical source from the shared discovery cursor.
    pub fn register_discovered_source(
        &mut self,
        source: &SourceDocument,
    ) -> Result<(), CorpusError> {
        if source.ordinal != self.discovered_documents
            || source.target_group_ordinal > self.discovered_distinct_target_groups
        {
            return Err(CorpusError::SourceInventoryMismatch);
        }
        if source.target_group_ordinal == self.discovered_distinct_target_groups {
            self.discovered_target_counts.seek(SeekFrom::End(0))?;
            self.discovered_target_counts.append(&1_u64.to_le_bytes())?;
            self.discovered_to_admitted_groups.seek(SeekFrom::End(0))?;
            self.discovered_to_admitted_groups
                .append(&UNMAPPED_TARGET_GROUP.to_le_bytes())?;
            self.discovered_distinct_target_groups = self
                .discovered_distinct_target_groups
                .checked_add(1)
                .ok_or_else(|| invalid_builder("discovered target group count overflow"))?;
            self.discovered_max_target_multiplicity =
                self.discovered_max_target_multiplicity.max(1);
        } else {
            let count = increment_scratch_count(
                &mut self.discovered_target_counts,
                source.target_group_ordinal,
                "discovered target multiplicity overflow",
            )?;
            self.discovered_max_target_multiplicity =
                self.discovered_max_target_multiplicity.max(count);
        }
        self.discovered_external_targets = self
            .discovered_external_targets
            .checked_add(u64::from(source.external_target))
            .ok_or_else(|| invalid_builder("discovered external target count overflow"))?;
        self.discovered_inventory
            .begin_source(&source.logical_name)?;
        self.discovered_inventory.end_source()?;
        self.discovery_digest
            .update(&(source.logical_name.len() as u64).to_le_bytes());
        self.discovery_digest.update(source.logical_name.as_bytes());
        self.discovered_documents = self
            .discovered_documents
            .checked_add(1)
            .ok_or_else(|| invalid_builder("discovered document count overflow"))?;
        Ok(())
    }

    /// Registers one admitted logical document with an already-dense target ordinal.
    fn register_dense_admitted_document(
        &mut self,
        source: &SourceDocument,
    ) -> Result<(), CorpusError> {
        if self.pending_source.is_some() || self.current_document.is_some() {
            return Err(invalid_builder(
                "source documents must be registered immediately before DocumentStart",
            ));
        }
        if source.ordinal != self.registered_documents {
            return Err(CorpusError::SourceInventoryMismatch);
        }
        if source.target_group_ordinal > self.distinct_target_groups {
            return Err(CorpusError::SourceInventoryMismatch);
        }
        if source.target_group_ordinal == self.distinct_target_groups {
            self.distinct_target_groups = self
                .distinct_target_groups
                .checked_add(1)
                .ok_or_else(|| invalid_builder("target group count overflow"))?;
        }
        self.admitted_external_targets = self
            .admitted_external_targets
            .checked_add(u64::from(source.external_target))
            .ok_or_else(|| invalid_builder("external target count overflow"))?;
        self.admitted_inventory_digest.push(
            &source.logical_name,
            source.target_group_ordinal,
            source.external_target,
        )?;
        self.registered_inventory
            .begin_source(&source.logical_name)?;
        self.registered_inventory.end_source()?;
        self.registered_documents += 1;
        self.pending_source = Some(RegisteredDocument {
            ordinal: source.ordinal,
            logical_name: source.logical_name.clone(),
            target_group_ordinal: source.target_group_ordinal,
            external_target: source.external_target,
        });
        Ok(())
    }

    /// Registers one admitted logical document against its discovered physical target.
    ///
    /// Physical target ordinals may have gaps in the admitted stream when an
    /// empty source emits no logical document. The builder keeps the canonical
    /// dense remap in budgeted scratch rather than caller-retained memory.
    pub fn register_admitted_document_from_source(
        &mut self,
        ordinal: u64,
        logical_name: &str,
        physical: &SourceDocument,
    ) -> Result<(), CorpusError> {
        if physical.target_group_ordinal >= self.discovered_distinct_target_groups {
            return Err(CorpusError::SourceInventoryMismatch);
        }
        let offset = physical
            .target_group_ordinal
            .checked_mul(8)
            .ok_or_else(|| invalid_builder("target group scratch offset overflow"))?;
        self.discovered_to_admitted_groups
            .seek(SeekFrom::Start(offset))?;
        let mut bytes = [0_u8; 8];
        self.discovered_to_admitted_groups.read_exact(&mut bytes)?;
        let stored = u64::from_le_bytes(bytes);
        let target_group_ordinal = if stored == UNMAPPED_TARGET_GROUP {
            self.distinct_target_groups
        } else {
            stored
        };
        self.register_dense_admitted_document(&SourceDocument {
            ordinal,
            logical_name: logical_name.to_owned(),
            path: physical.path.clone(),
            target_group_ordinal,
            external_target: physical.external_target,
        })?;
        if stored == UNMAPPED_TARGET_GROUP {
            self.discovered_to_admitted_groups
                .seek(SeekFrom::Start(offset))?;
            self.discovered_to_admitted_groups
                .write_existing(&target_group_ordinal.to_le_bytes())?;
        }
        Ok(())
    }

    /// Records one admitted structural event and returns its sentence location.
    pub async fn push_corpus_event(
        &mut self,
        event: &AdmittedCorpusEvent,
    ) -> Result<Option<SentenceLocation>, CorpusError> {
        if self.pending_tokens.is_some() {
            return Err(invalid_builder(
                "tokenized sentence must follow its admitted sentence",
            ));
        }
        match event {
            AdmittedCorpusEvent::DocumentStart {
                id,
                source_name,
                metadata,
            } => {
                if self.current_document.is_some() {
                    return Err(invalid_builder("nested document start"));
                }
                let registered = self
                    .pending_source
                    .take()
                    .ok_or(CorpusError::SourceInventoryMismatch)?;
                if registered.logical_name != *source_name
                    || registered.ordinal != self.admitted_documents
                {
                    return Err(CorpusError::SourceInventoryMismatch);
                }
                self.admitted_inventory.begin_source(source_name)?;
                self.admitted_inventory.end_source()?;
                let target_multiplicity = self
                    .location_writer
                    .as_mut()
                    .expect("location writer is open")
                    .register_document_unresolved(
                        *id,
                        registered.ordinal,
                        Arc::from(registered.logical_name.as_str()),
                        registered.target_group_ordinal,
                        registered.external_target,
                    )?;
                self.admitted_max_target_multiplicity = self
                    .admitted_max_target_multiplicity
                    .max(target_multiplicity);
                self.current_document = Some(CurrentDocument {
                    id: *id,
                    ordinal: registered.ordinal,
                    source_name: Arc::from(source_name.as_str()),
                    metadata_json: metadata_json(metadata)?,
                });
                self.admitted_documents += 1;
                Ok(None)
            }
            AdmittedCorpusEvent::DivisionStart(division) => {
                let document = self
                    .current_document
                    .as_ref()
                    .ok_or_else(|| invalid_builder("division outside document"))?;
                if division.document_id != document.id
                    || self.active_divisions.contains_key(&division.id)
                    || division
                        .parent_id
                        .is_some_and(|parent| !self.active_divisions.contains_key(&parent))
                {
                    return Err(invalid_builder(
                        "invalid division identity, owner or parent",
                    ));
                }
                self.active_divisions
                    .insert(division.id, Arc::clone(division));
                if !self.division_observations.contains_key(&division.kind) {
                    let observation = lingrep_core::corpus::DivisionObservation::Observed;
                    self.division_observations
                        .insert(division.kind.clone(), observation.clone());
                    self.provenance_writer
                        .as_mut()
                        .expect("provenance writer is open")
                        .structure_event(AdmittedCorpusEvent::DivisionObservation {
                            document_id: division.document_id,
                            kind: division.kind.clone(),
                            observation,
                        })?;
                }
                self.provenance_writer
                    .as_mut()
                    .expect("provenance writer is open")
                    .structure_event(event.clone())?;
                Ok(None)
            }
            AdmittedCorpusEvent::DivisionEnd { id } => {
                if self
                    .active_divisions
                    .values()
                    .any(|division| division.parent_id == Some(*id))
                    || self.active_divisions.remove(id).is_none()
                {
                    return Err(invalid_builder("invalid division close"));
                }
                self.provenance_writer
                    .as_mut()
                    .expect("provenance writer is open")
                    .structure_event(event.clone())?;
                Ok(None)
            }
            AdmittedCorpusEvent::DivisionObservation {
                document_id,
                kind,
                observation,
            } => {
                if self
                    .current_document
                    .as_ref()
                    .is_none_or(|document| document.id != *document_id)
                {
                    return Err(invalid_builder(
                        "structural observation outside its document",
                    ));
                }
                self.division_observations
                    .insert(kind.clone(), observation.clone());
                self.provenance_writer
                    .as_mut()
                    .expect("provenance writer is open")
                    .structure_event(event.clone())?;
                Ok(None)
            }
            AdmittedCorpusEvent::Sentence(sentence) => {
                let document = self
                    .current_document
                    .as_ref()
                    .ok_or_else(|| invalid_builder("sentence outside document"))?;
                if document.id != sentence.document_id || sentence.id.get() != self.sentence_count {
                    return Err(invalid_builder("invalid admitted sentence order"));
                }
                let location = SentenceLocation {
                    source_metadata: None,
                    corpus_sentence_ordinal: self.sentence_count,
                    document_id: document.id,
                    document_ordinal: document.ordinal,
                    document_sentence_ordinal: sentence.document_sentence_ordinal,
                    source_name: Arc::clone(&document.source_name),
                };
                let rich = RichSentenceMetadata {
                    structure: Arc::new(lingrep_core::corpus::StructuralContext {
                        observations: self.division_observations.clone(),
                        divisions: self.active_divisions.values().cloned().collect(),
                    }),
                    region_labels: sentence.active_labels.clone(),
                    metadata_json: document.metadata_json.clone(),
                    sentence_metadata_json: metadata_json(&sentence.metadata)?,
                };
                self.location_writer
                    .as_mut()
                    .expect("location writer is open")
                    .append(&location)?;
                self.provenance_writer
                    .as_mut()
                    .expect("provenance writer is open")
                    .push_sentence_with_text(
                        &location,
                        &rich,
                        &sentence.text,
                        sentence.words.as_ref(),
                    )?;
                match (&self.enhanced_writer, &sentence.enhanced_graph) {
                    (Some(_), Some(observation)) => {
                        self.pending_enhanced_graph = Some(observation.clone());
                    }
                    (Some(_), None) => {
                        return Err(invalid_builder(
                            "graph-capable schema received a sentence without graph observation",
                        ));
                    }
                    (None, Some(_)) => {
                        return Err(invalid_builder(
                            "sentence graph observation has no graph-capable schema",
                        ));
                    }
                    (None, None) => {}
                }
                self.sentence_count += 1;
                self.pending_tokens = Some(location.clone());
                Ok(Some(location))
            }
            AdmittedCorpusEvent::DocumentEnd { id } => {
                if self
                    .current_document
                    .as_ref()
                    .is_none_or(|document| document.id != *id)
                {
                    return Err(invalid_builder("invalid document end"));
                }
                if !self.active_divisions.is_empty() {
                    return Err(invalid_builder("document ended with open divisions"));
                }
                self.division_observations.clear();
                self.current_document = None;
                Ok(None)
            }
        }
    }

    /// Writes the tokens paired with the immediately preceding sentence event.
    pub async fn push_tokenized_sentence(
        &mut self,
        location: &SentenceLocation,
        tokens: &[TokenInfo],
    ) -> Result<(), CorpusError> {
        let expected = self
            .pending_tokens
            .take()
            .ok_or_else(|| invalid_builder("token sentence has no admitted sentence"))?;
        if &expected != location {
            return Err(invalid_builder(
                "token sentence location differs from admitted location",
            ));
        }
        self.token_writer
            .as_mut()
            .expect("token writer is open")
            .push_sentence(tokens)?;
        self.token_count = self
            .token_count
            .checked_add(
                u64::try_from(tokens.len())
                    .map_err(|_| invalid_builder("token count does not fit u64"))?,
            )
            .ok_or_else(|| invalid_builder("token count overflow"))?;
        Ok(())
    }

    /// Writes one validated native-analysis sentence in its complete typed schema.
    /// Takes the analysis by value: the token writer buffers the sentence
    /// until its block flushes, and every caller already owns the value.
    pub async fn push_analyzed_sentence(
        &mut self,
        location: &SentenceLocation,
        analyzed: AnalyzedInput,
        projection: &BoundProjection,
    ) -> Result<(), CorpusError> {
        let [sentence] = analyzed.sentences() else {
            return Err(invalid_builder(
                "prepared analyzed input must contain exactly one sentence",
            ));
        };
        if analyzed.input().id().get() != location.corpus_sentence_ordinal
            || sentence.words().schema_digest() != projection.schema_digest()
        {
            return Err(invalid_builder(
                "analyzed sentence does not carry the complete stored schema or location identity",
            ));
        }
        let expected = self
            .pending_tokens
            .take()
            .ok_or_else(|| invalid_builder("analyzed sentence has no admitted sentence"))?;
        if &expected != location {
            return Err(invalid_builder(
                "analyzed sentence location differs from admitted location",
            ));
        }
        let record = AnalysisRecord::extract(location.corpus_sentence_ordinal, &analyzed)?;
        self.provenance_writer
            .as_mut()
            .expect("provenance writer is open")
            .attach(record)?;
        let words = analyzed
            .into_parts()
            .1
            .into_vec()
            .pop()
            .expect("the single-sentence shape was checked above")
            .into_words();
        let token_count = u64::try_from(words.len())
            .map_err(|_| invalid_builder("token count does not fit u64"))?;
        self.token_writer
            .as_mut()
            .expect("token writer is open")
            .push_bound_sentence(words)?;
        self.token_count = self
            .token_count
            .checked_add(token_count)
            .ok_or_else(|| invalid_builder("token count overflow"))?;
        Ok(())
    }

    /// Writes one pretokenized sentence in the artifact's complete schema.
    ///
    /// Pretokenized formats have no analyzer invocation to record. Their
    /// admitted text remains in provenance and their bound words are stored
    /// verbatim.
    pub async fn push_bound_sentence(
        &mut self,
        location: &SentenceLocation,
        words: BoundTokenSentence,
        projection: &BoundProjection,
    ) -> Result<(), CorpusError> {
        if words.schema_digest() != projection.schema_digest()
            || projection.schema_digest() != self.logical_token_schema.digest()
        {
            return Err(invalid_builder(
                "pretokenized sentence does not carry the complete stored schema",
            ));
        }
        let expected = self
            .pending_tokens
            .take()
            .ok_or_else(|| invalid_builder("pretokenized sentence has no admitted sentence"))?;
        if &expected != location {
            return Err(invalid_builder(
                "pretokenized sentence location differs from admitted location",
            ));
        }
        let token_count = u64::try_from(words.len())
            .map_err(|_| invalid_builder("token count does not fit u64"))?;
        if let Some(writer) = self.enhanced_writer.as_mut() {
            let observation = self.pending_enhanced_graph.take().ok_or_else(|| {
                invalid_builder("graph-capable token sentence has no pending observation")
            })?;
            writer.push(observation, words.len())?;
        }
        let physical_words = words
            .project_onto_schema(&self.logical_token_schema, &self.physical_projection)
            .map_err(|_| {
                invalid_builder("pretokenized sentence cannot project to physical storage")
            })?;
        self.token_writer
            .as_mut()
            .expect("token writer is open")
            .push_bound_sentence(physical_words)?;
        self.token_count = self
            .token_count
            .checked_add(token_count)
            .ok_or_else(|| invalid_builder("token count overflow"))?;
        Ok(())
    }

    /// Finalizes a complete private artifact without publishing it.
    #[cfg(test)]
    pub(crate) async fn finalize_staging(
        self,
        discovery: DiscoveryReport,
    ) -> Result<CompletedStagingArtifact, CorpusError> {
        let mut should_continue = || true;
        self.finalize_staging_guarded(discovery, &mut should_continue)
            .await?
            .ok_or_else(|| invalid_builder("unconditional staging finalization cancelled"))
    }

    async fn finalize_staging_guarded(
        mut self,
        discovery: DiscoveryReport,
        should_continue: &mut impl FnMut() -> bool,
    ) -> Result<Option<CompletedStagingArtifact>, CorpusError> {
        if !should_continue() {
            return Ok(None);
        }
        let observed = self.observation.finish()?;
        let discovered = self.discovered_inventory.finish()?;
        let registered = self.registered_inventory.finish()?;
        let admitted = self.admitted_inventory.finish()?;
        if observed.document_count() != self.discovered_documents
            || observed.ordered_inventory_digest() != discovered.ordered_inventory_digest()
            || registered.document_count() != self.registered_documents
            || registered.document_count() != self.admitted_documents
            || registered.ordered_inventory_digest() != admitted.ordered_inventory_digest()
        {
            return Err(CorpusError::SourceInventoryMismatch);
        }
        let discovered_inventory = InventoryStats {
            documents: self.discovered_documents,
            distinct_targets: self.discovered_distinct_target_groups,
            alias_documents: self
                .discovered_documents
                .checked_sub(self.discovered_distinct_target_groups)
                .ok_or(CorpusError::SourceInventoryMismatch)?,
            max_target_multiplicity: self.discovered_max_target_multiplicity,
            external_targets: self.discovered_external_targets,
        };
        let discovery_digest = *self.discovery_digest.finalize().as_bytes();
        if discovery.inventory != discovered_inventory
            || discovery.ordered_inventory_digest != discovery_digest
        {
            return Err(CorpusError::DiscoveryReportMismatch);
        }
        let admitted_inventory = InventoryStats {
            documents: self.admitted_documents,
            distinct_targets: self.distinct_target_groups,
            alias_documents: self
                .admitted_documents
                .checked_sub(self.distinct_target_groups)
                .ok_or(CorpusError::SourceInventoryMismatch)?,
            max_target_multiplicity: self.admitted_max_target_multiplicity,
            external_targets: self.admitted_external_targets,
        };
        let admitted_inventory_digest = self.admitted_inventory_digest.finish();
        if self.pending_source.is_some()
            || self.current_document.is_some()
            || self.pending_tokens.is_some()
            || self.pending_enhanced_graph.is_some()
        {
            return Err(invalid_builder(
                "builder ended with incomplete structural state",
            ));
        }

        let source_identity = *observed.source_identity();
        let analysis_identity = derive_analysis_identity(&observed, &self.config.analysis);
        let tokens_name = TOKENS_NAME;
        let tokens_path = self.staging.join(tokens_name);
        let locations_path = self.staging.join(LOCATIONS_NAME);
        let provenance_path = self.staging.join(PROVENANCE_NAME);
        let enhanced_path = self.staging.join(ENHANCED_NAME);
        let postings = self
            .token_writer
            .take()
            .expect("token writer is open")
            .finish(analysis_identity)?;
        if !should_continue() {
            return Ok(None);
        }
        self.location_writer
            .take()
            .expect("location writer is open")
            .finish(analysis_identity)?;
        if !should_continue() {
            return Ok(None);
        }
        self.provenance_writer
            .take()
            .expect("provenance writer is open")
            .finish(analysis_identity)?;
        if !should_continue() {
            return Ok(None);
        }
        if let Some(writer) = self.enhanced_writer.take() {
            if writer.sentence_count() != self.sentence_count {
                return Err(invalid_builder(
                    "enhanced component sentence count differs from the artifact",
                ));
            }
            writer.finish(
                analysis_identity,
                TokenSchemaIdentity::from_bytes(*self.logical_token_schema.digest().as_bytes()),
            )?;
            if !should_continue() {
                return Ok(None);
            }
        }

        let mut components = BTreeMap::new();
        let Some(tokens_fact) = component_fact_guarded(&tokens_path, &mut *should_continue)? else {
            return Ok(None);
        };
        let reader = TokenReader::open(
            &tokens_path,
            &analysis_identity,
            &self.physical_token_schema,
        )?;
        let built = match postings {
            Some(postings) => postings.finish(
                &self.staging,
                &reader,
                &analysis_identity,
                &self.physical_token_schema,
                &tokens_fact.blake3,
                should_continue,
            ),
            None => Ok(false),
        };
        if !should_continue() {
            return Ok(None);
        }
        if built? {
            let Some(fact) = component_fact_guarded(
                &self.staging.join(crate::postings::NAME),
                &mut *should_continue,
            )?
            else {
                return Ok(None);
            };
            components.insert(crate::postings::NAME.to_owned(), fact);
        }
        components.insert(tokens_name.to_owned(), tokens_fact);
        let Some(locations_fact) = component_fact_guarded(&locations_path, &mut *should_continue)?
        else {
            return Ok(None);
        };
        components.insert(LOCATIONS_NAME.to_owned(), locations_fact);
        let Some(provenance_fact) =
            component_fact_guarded(&provenance_path, &mut *should_continue)?
        else {
            return Ok(None);
        };
        components.insert(PROVENANCE_NAME.to_owned(), provenance_fact);
        if enhanced_path.exists() {
            let Some(enhanced_fact) =
                component_fact_guarded(&enhanced_path, &mut *should_continue)?
            else {
                return Ok(None);
            };
            components.insert(ENHANCED_NAME.to_owned(), enhanced_fact);
        }
        if !should_continue() {
            return Ok(None);
        }
        let has_enhanced = components.contains_key(ENHANCED_NAME);
        let has_postings = components.contains_key(crate::postings::NAME);
        Manifest::new(ManifestInputs {
            source_identity,
            analysis_identity,
            analysis: &self.config.analysis,
            logical_token_schema: &self.logical_token_schema,
            physical_token_schema: self.physical_token_schema,
            enhanced_graph: has_enhanced,
            dictionary: self.config.dictionary,
            trust: self.config.trust,
            analyzer: &self.config.analyzer,
            document_count: self.admitted_documents,
            sentence_count: self.sentence_count,
            token_count: self.token_count,
            discovery: &discovery,
            admitted_inventory: &admitted_inventory,
            admitted_inventory_digest,
            components,
            sentence_truncation: self.sentence_truncation,
        })
        .write_new(&self.staging.join(MANIFEST_NAME))?;

        Ok(Some(CompletedStagingArtifact {
            staging: self.staging,
            destination: self.destination,
            token_name: tokens_name,
            has_enhanced,
            has_postings,
        }))
    }
}

impl CorpusArtifact {
    /// Opens fixed regular components without hashing payloads or opening SQLite.
    ///
    /// The root is resolved through a symlink; the components inside it are
    /// not. The asymmetry is the point. A *component* symlink is a containment
    /// escape — the manifest describes bytes that then live somewhere else —
    /// and `ensure_regular` refuses it. The *root* is the path the caller
    /// named, and staging a corpus behind a symlink is what deployment tooling
    /// does: this repository's own NixOS module assembles every declared corpus
    /// as `<state>/corpora/<name>`, so refusing one here made a declared
    /// artifact unserveable in the one deployment shape we ship.
    ///
    /// It also has to resolve, because [`crate::is_prepared_artifact`] — the
    /// gate every caller passes through first — uses `Path::is_dir`, which
    /// follows. The two disagreeing produced a contradiction inside a single
    /// branch: the gate answered "this is an artifact" and the open then
    /// answered "expected artifact directory" about the same path.
    ///
    /// A symlink to a regular file is still refused, because the resolved type
    /// is what is checked; a dangling one surfaces as the underlying I/O error.
    pub fn open(path: &Path) -> Result<Self, CorpusError> {
        let metadata = fs::metadata(path)?;
        if !metadata.file_type().is_dir() {
            return Err(CorpusError::ExpectedArtifactDirectory {
                path: path.to_path_buf(),
            });
        }

        ensure_regular(path, MANIFEST_NAME)?;
        let manifest = Manifest::open(&path.join(MANIFEST_NAME))?;
        reject_sqlite_sidecars(path)?;
        for (name, fact) in &manifest.components {
            ensure_regular(path, component_name(name))?;
            let actual = fs::metadata(path.join(name))?.len();
            if actual != fact.length {
                return Err(CorpusError::ComponentLengthMismatch {
                    component: component_name(name),
                });
            }
        }

        let source_identity = manifest.source_identity()?;
        let analysis_identity = manifest.analysis_identity()?;
        let token_schema = manifest.token_schema_identity()?;
        let physical_token_schema = manifest.physical_token_schema_identity()?;
        let trust = TrustClass::parse(&manifest.trust_class)?;
        if derive_analysis_identity_from_source(&source_identity, &manifest.analysis_inputs()?)
            != analysis_identity
        {
            return Err(CorpusError::AnalysisIdentityMismatch {
                component: MANIFEST_NAME,
            });
        }
        validate_fixed_headers(path, &manifest, &analysis_identity, &physical_token_schema)?;
        let token_reader = TokenReader::open(
            &path.join(TOKENS_NAME),
            &analysis_identity,
            &physical_token_schema,
        )
        .map_err(map_open_error)?;
        let logical_token_schema = manifest.logical_token_schema()?;
        let enhanced = manifest
            .enhanced_graph_capable()
            .then(|| {
                EnhancedReader::open(&path.join(ENHANCED_NAME), &analysis_identity, &token_schema)
            })
            .transpose()
            .map_err(map_open_error)?
            .map(Arc::new);
        let locations = MappedLocations::open(&path.join(LOCATIONS_NAME), &analysis_identity)
            .map_err(map_open_error)?;
        if token_reader.sentence_count() != manifest.sentence_count
            || locations.sentence_count() != manifest.sentence_count
            || enhanced
                .as_ref()
                .is_some_and(|reader| reader.sentence_count() != manifest.sentence_count)
        {
            return Err(CorpusError::SentenceCountMismatch {
                component: "tokens.v1.zst/locations.v1",
            });
        }
        if token_reader.token_count() != manifest.token_count {
            return Err(invalid_artifact("token header differs from manifest"));
        }
        if locations.document_count() != manifest.document_count {
            return Err(CorpusError::SentenceCountMismatch {
                component: LOCATIONS_NAME,
            });
        }
        let postings = manifest
            .components
            .contains_key(crate::postings::NAME)
            .then(|| {
                crate::postings::PostingIndex::open(
                    &path.join(crate::postings::NAME),
                    &analysis_identity,
                    &physical_token_schema,
                    &manifest.components[TOKENS_NAME].blake3,
                    manifest.sentence_count,
                )
            })
            .transpose()?
            .map(Arc::new);
        let discovery = manifest.discovery_report()?;
        Ok(Self {
            root: path.to_path_buf(),
            manifest,
            source_identity,
            analysis_identity,
            physical_token_schema,
            logical_token_schema,
            discovery,
            trust,
            locations,
            enhanced,
            postings,
        })
    }

    pub fn source_identity(&self) -> &SourceIdentity {
        &self.source_identity
    }

    fn into_published_at(mut self, destination: PathBuf) -> Self {
        self.root = destination;
        self
    }

    pub fn analysis_identity(&self) -> &AnalysisIdentity {
        &self.analysis_identity
    }

    /// Returns the trust boundary recorded for this analyzer realization.
    pub const fn trust_class(&self) -> TrustClass {
        self.trust
    }

    /// Returns the exact semantic analyzer description sealed in the manifest.
    pub fn analyzer(&self) -> &AnalyzerRecord {
        &self.manifest.analyzer
    }

    pub fn analysis_inputs(&self) -> Result<AnalysisInputs, CorpusError> {
        self.manifest.analysis_inputs()
    }

    /// Returns the scheme this artifact's recorded analysis identity was
    /// derived under.
    #[must_use]
    pub const fn identity_scheme(&self) -> IdentityScheme {
        self.manifest.identity_scheme
    }

    /// Returns the dictionary fingerprint recorded as an inspection fact.
    pub fn dictionary_fingerprint(&self) -> Result<crate::DictionaryFingerprint, CorpusError> {
        self.manifest.dictionary_fingerprint()
    }

    /// Returns the complete ordered V1 profile schema stored by this artifact.
    pub fn token_schema(&self) -> Result<lingrep_core::token_match::TokenSchema, CorpusError> {
        Ok(self.logical_token_schema.clone())
    }

    /// Returns whether this artifact stores sentence-level enhanced graph observations.
    #[must_use]
    pub fn has_enhanced_graphs(&self) -> bool {
        self.enhanced.is_some()
    }

    /// Reads one normalized graph observation without opening or decoding it
    /// for artifacts and queries that do not request graph evidence.
    pub fn enhanced_graph_observation(
        &self,
        sentence_ordinal: u64,
    ) -> Result<Option<EnhancedGraphObservation>, CorpusError> {
        self.enhanced
            .as_ref()
            .map(|reader| reader.observation(sentence_ordinal))
            .transpose()
    }

    fn require_complete_sentences(&self) -> Result<(), CorpusError> {
        if let Some(truncation) = self.manifest.sentence_truncation
            && truncation.count > 0
        {
            return Err(invalid_artifact(format!(
                "artifact contains {} truncated sentence(s); rebuild the complete corpus with ling corpus build",
                truncation.count
            )));
        }
        Ok(())
    }

    pub fn sentence_source(&self) -> Result<PreparedSentenceSource, CorpusError> {
        let schema = self.token_schema()?;
        let attributes = TokenAttributeIndex::ALL
            .into_iter()
            .filter(|attribute| schema.keys().contains(&(*attribute).into()))
            .collect::<Vec<_>>();
        self.sentence_source_with_attributes(&attributes)
    }

    /// Opens a prepared sentence source that reconstructs only requested token attributes.
    pub fn sentence_source_with_attributes(
        &self,
        attributes: &[TokenAttributeIndex],
    ) -> Result<PreparedSentenceSource, CorpusError> {
        self.require_complete_sentences()?;
        let reader = TokenReader::open(
            &self.root.join(TOKENS_NAME),
            &self.analysis_identity,
            &self.physical_token_schema,
        )?;
        let locations =
            MappedLocations::open(&self.root.join(LOCATIONS_NAME), &self.analysis_identity)?;
        PreparedSentenceSource::new(reader, locations, attributes.to_vec())
    }

    /// Opens a prepared source that decodes only the matcher's attributes.
    ///
    /// The projection is final: nothing refills the attributes it skips, so a
    /// caller must project everything it intends to publish.
    pub fn deferred_sentence_source(
        &self,
        matcher_attributes: &[lingrep_core::token_match::AttributeKey],
    ) -> Result<PreparedSentenceSource, CorpusError> {
        self.require_complete_sentences()?;
        let reader = TokenReader::open(
            &self.root.join(TOKENS_NAME),
            &self.analysis_identity,
            &self.physical_token_schema,
        )?;
        let locations =
            MappedLocations::open(&self.root.join(LOCATIONS_NAME), &self.analysis_identity)?;
        let projection = self
            .logical_token_schema
            .bind_projection(matcher_attributes.iter().cloned())
            .map_err(|error| {
                invalid_artifact(format!("catalog projection is unavailable: {error}"))
            })?;
        PreparedSentenceSource::new_bound_deferred(
            reader,
            self.enhanced.clone(),
            locations,
            &self.logical_token_schema,
            projection,
        )
    }

    /// Opens a prepared source bound to one run-scoped execution projection.
    pub fn bound_sentence_source(
        &self,
        projection: &BoundProjection,
    ) -> Result<PreparedSentenceSource, CorpusError> {
        self.require_complete_sentences()?;
        let reader = TokenReader::open(
            &self.root.join(TOKENS_NAME),
            &self.analysis_identity,
            &self.physical_token_schema,
        )?;
        let locations =
            MappedLocations::open(&self.root.join(LOCATIONS_NAME), &self.analysis_identity)?;
        PreparedSentenceSource::new_bound_deferred(
            reader,
            self.enhanced.clone(),
            locations,
            &self.logical_token_schema,
            projection.clone(),
        )
    }

    /// A sentence is included if any label is selected, or if it has no labels and `include_unlabelled` is true.
    /// `None` selects every label; an empty list selects none.
    /// Pull batches on a blocking thread while `runtime` remains alive.
    pub fn bound_sentence_source_with_sections(
        &self,
        projection: &BoundProjection,
        provenance: Arc<ProvenanceView>,
        runtime: tokio::runtime::Handle,
        included: Option<Vec<String>>,
        include_unlabelled: bool,
    ) -> Result<PreparedSentenceSource, CorpusError> {
        self.validate_provenance_view(&provenance)?;
        Ok(self.bound_sentence_source(projection)?.with_sections(
            provenance,
            runtime,
            included,
            include_unlabelled,
        ))
    }

    /// Prunes sentences using a proof from the catalog the caller will execute.
    ///
    /// Unvisited token blocks remain unchecked; complete integrity requires `verify_integrity`.
    /// Admission population includes `filter_stats().sentences_proven_absent()` in addition to returned sentences.
    pub fn filtered_sentence_source(
        &self,
        projection: &BoundProjection,
        filter: &lingrep_core::fsm::SentenceFilter,
    ) -> Result<PreparedSentenceSource, CorpusError> {
        let mut source = self.bound_sentence_source(projection)?;
        if let Some(index) = &self.postings {
            source.candidates = index.candidates(filter)?;
        }
        Ok(source)
    }

    /// Opens a blocking provenance reader bound to this exact artifact.
    pub fn provenance_reader(&self) -> Result<Arc<crate::BlockingProvenance>, CorpusError> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let view = runtime.block_on(self.provenance_view())?;
        Ok(Arc::new(crate::BlockingProvenance::new(runtime, view)))
    }

    /// Uses an already-opened reader bound to this exact artifact.
    pub fn bound_sentence_source_with_metadata(
        &self,
        projection: &BoundProjection,
        metadata: lingrep_core::corpus::SourceMetadataProjection,
        reader: Arc<crate::BlockingProvenance>,
    ) -> Result<PreparedSentenceSource, CorpusError> {
        self.validate_provenance_reader(&reader)?;
        let source = self.bound_sentence_source(projection)?;
        Ok(source.with_source_metadata(reader, metadata))
    }

    fn validate_provenance_reader(
        &self,
        reader: &crate::BlockingProvenance,
    ) -> Result<(), CorpusError> {
        self.validate_provenance_view(reader.view())
    }

    fn validate_provenance_view(&self, view: &ProvenanceView) -> Result<(), CorpusError> {
        if view.analysis_identity() != &self.analysis_identity {
            return Err(CorpusError::AnalysisIdentityMismatch {
                component: PROVENANCE_NAME,
            });
        }
        if view.artifact_binding() != Some(&self.manifest.component_blake3(PROVENANCE_NAME)?) {
            return Err(CorpusError::ArtifactBindingMismatch {
                component: PROVENANCE_NAME,
            });
        }
        Ok(())
    }

    /// The reader's format-level sentence metadata keys; document-specific
    /// keys are returned by `document_metadata`.
    pub async fn sentence_metadata_keys(
        &self,
        view: &ProvenanceView,
    ) -> Result<std::collections::BTreeSet<String>, CorpusError> {
        view.sentence_metadata_keys().await
    }

    pub async fn provenance_view(&self) -> Result<ProvenanceView, CorpusError> {
        let view = ProvenanceView::open(&self.root.join(PROVENANCE_NAME), &self.analysis_identity)
            .await
            .map_err(map_provenance_error)?
            .bind_to_artifact(self.manifest.component_blake3(PROVENANCE_NAME)?);
        if view.sentence_count() != self.manifest.sentence_count {
            return Err(CorpusError::SentenceCountMismatch {
                component: PROVENANCE_NAME,
            });
        }
        view.validate_document_count(self.document_count()).await?;
        Ok(view)
    }

    pub async fn sentence_provenance(
        &self,
        view: &ProvenanceView,
        ordinal: u64,
    ) -> Result<SentenceProvenance, CorpusError> {
        self.sentence_provenance_page(view, &[ordinal])
            .await?
            .pop()
            .flatten()
            .ok_or_else(|| invalid_artifact("sentence provenance is missing"))
    }

    /// Composes a bounded provenance page while retaining input positions.
    pub async fn sentence_provenance_page(
        &self,
        view: &ProvenanceView,
        ordinals: &[u64],
    ) -> Result<Vec<Option<SentenceProvenance>>, CorpusError> {
        if ordinals.len() > MAX_PROVENANCE_PAGE_SIZE {
            return Err(CorpusError::ProvenancePageCapacity {
                limit: MAX_PROVENANCE_PAGE_SIZE,
            });
        }
        if view.analysis_identity() != &self.analysis_identity {
            return Err(CorpusError::AnalysisIdentityMismatch {
                component: PROVENANCE_NAME,
            });
        }
        let expected_binding = self.manifest.component_blake3(PROVENANCE_NAME)?;
        if view.artifact_binding() != Some(&expected_binding) {
            return Err(CorpusError::ArtifactBindingMismatch {
                component: PROVENANCE_NAME,
            });
        }
        if view.sentence_count() != self.manifest.sentence_count {
            return Err(CorpusError::SentenceCountMismatch {
                component: PROVENANCE_NAME,
            });
        }
        let sentences = ordinals
            .iter()
            .copied()
            .map(SentenceId::new)
            .collect::<Vec<_>>();
        let rich = view.rich_sentence_page(&sentences).await?;
        ordinals
            .iter()
            .copied()
            .zip(rich)
            .map(|(ordinal, rich)| {
                rich.map(|rich| {
                    Ok(SentenceProvenance {
                        location: self.locations.sentence(ordinal)?,
                        rich: rich.metadata,
                        active_regions: rich.active_regions,
                    })
                })
                .transpose()
            })
            .collect()
    }

    pub fn documents(&self) -> impl Iterator<Item = Result<StoredDocument, CorpusError>> + '_ {
        (0..self.locations.document_count())
            .map(|ordinal| self.locations.document(ordinal).map(stored_document))
    }

    /// Returns the number of admitted logical documents in the artifact.
    pub fn document_count(&self) -> u64 {
        self.locations.document_count()
    }

    /// Returns the manifest count admitted during bounded artifact open.
    pub fn sentence_count(&self) -> u64 {
        self.manifest.sentence_count
    }

    /// Returns the manifest count admitted during bounded artifact open.
    pub fn token_count(&self) -> u64 {
        self.manifest.token_count
    }

    /// The artifact's recorded identity, read rather than recomputed.
    ///
    /// Every field is already in the manifest, so this costs a manifest parse
    /// and no hashing — which is what makes it usable before every run of a
    /// paired measurement, where rehashing gigabytes is not.
    ///
    /// It covers what a performance campaign has to hold fixed: the stored
    /// token and location components by digest, the dictionary and token
    /// schema the tokens were produced under, and the document inventory.
    /// `ordered_inventory_digest` alone does not — the same documents
    /// retokenized under a different dictionary produce different token
    /// counts, attributes, predicates and matches, and so different work.
    #[must_use]
    pub fn recorded_identity(&self) -> PreparedIdentity {
        PreparedIdentity {
            schema_version: self.manifest.schema_version,
            source_identity: self.manifest.source_identity.clone(),
            analysis_identity: self.manifest.analysis_identity.clone(),
            dictionary_fingerprint: self.manifest.dictionary_fingerprint.clone(),
            token_schema_identity: self.manifest.token_schema_identity.clone(),
            ordered_inventory_digest: self.manifest.ordered_inventory_digest.clone(),
            admitted_inventory_digest: self.manifest.admitted_inventory_digest.clone(),
            components: self
                .manifest
                .components
                .iter()
                .map(|(name, fact)| (name.clone(), fact.blake3.clone()))
                .collect(),
            document_count: self.manifest.document_count,
            sentence_count: self.manifest.sentence_count,
            token_count: self.manifest.token_count,
        }
    }

    pub fn inventory_stats(&self) -> &InventoryStats {
        &self.discovery.inventory
    }

    pub fn discovery_report(&self) -> &DiscoveryReport {
        &self.discovery
    }
}

fn stored_document(document: LocationDocument) -> StoredDocument {
    StoredDocument {
        ordinal: document.document_ordinal,
        logical_name: document.source_name,
        target_group_ordinal: document.target_group_ordinal,
        target_multiplicity: document.target_multiplicity,
        external_target: document.external_target,
    }
}

fn increment_scratch_count(
    counts: &mut BudgetedScratch,
    target_group_ordinal: u64,
    overflow_message: &'static str,
) -> Result<u64, CorpusError> {
    let offset = target_group_ordinal
        .checked_mul(8)
        .ok_or_else(|| invalid_builder("target group scratch offset overflow"))?;
    counts.seek(SeekFrom::Start(offset))?;
    let mut bytes = [0_u8; 8];
    counts.read_exact(&mut bytes)?;
    let count = u64::from_le_bytes(bytes)
        .checked_add(1)
        .ok_or_else(|| invalid_builder(overflow_message))?;
    counts.seek(SeekFrom::Start(offset))?;
    counts.write_existing(&count.to_le_bytes())?;
    Ok(count)
}

#[derive(Debug)]
struct StagingOwner {
    path: PathBuf,
    armed: bool,
}

impl StagingOwner {
    fn path(&self) -> &Path {
        &self.path
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl std::ops::Deref for StagingOwner {
    type Target = Path;

    fn deref(&self) -> &Self::Target {
        self.path()
    }
}

impl Drop for StagingOwner {
    fn drop(&mut self) {
        if self.armed {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

fn create_staging(parent: &Path, base: &str) -> Result<StagingOwner, CorpusError> {
    for _ in 0..1024 {
        let sequence = STAGING_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = parent.join(format!(".{base}.staging-{}-{sequence}", std::process::id()));
        match fs::create_dir(&path) {
            Ok(()) => {
                return Ok(StagingOwner { path, armed: true });
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(CorpusError::Io(error)),
        }
    }
    Err(invalid_builder("could not allocate a private staging name"))
}

fn metadata_json(metadata: &Metadata) -> Result<Option<String>, CorpusError> {
    if metadata.is_empty() {
        return Ok(None);
    }
    let object = metadata
        .iter()
        .map(|(key, value)| (key.clone(), value.to_json()))
        .collect();
    serde_json::to_string(&serde_json::Value::Object(object))
        .map(Some)
        .map_err(json_error)
}

fn ensure_regular(root: &Path, name: &'static str) -> Result<(), CorpusError> {
    let metadata = fs::symlink_metadata(root.join(name))?;
    if metadata.file_type().is_symlink() {
        return Err(CorpusError::SymlinkComponent { component: name });
    }
    if !metadata.file_type().is_file() {
        return Err(invalid_artifact(format!(
            "artifact component {name} is not a regular file"
        )));
    }
    Ok(())
}

fn reject_sqlite_sidecars(root: &Path) -> Result<(), CorpusError> {
    for suffix in ["-wal", "-shm", "-journal"] {
        match fs::symlink_metadata(root.join(format!("{PROVENANCE_NAME}{suffix}"))) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Ok(_) => return Err(CorpusError::SqliteSidecar),
            Err(error) => return Err(CorpusError::Io(error)),
        }
    }
    Ok(())
}

fn validate_fixed_headers(
    root: &Path,
    manifest: &Manifest,
    analysis: &AnalysisIdentity,
    token_schema: &crate::TokenSchemaIdentity,
) -> Result<(), CorpusError> {
    let token_header = read_prefix(&root.join(TOKENS_NAME), 128, TOKENS_NAME)?;
    if &token_header[..8] != b"LINGTOK1" {
        return Err(invalid_artifact("wrong V1 token magic"));
    }
    let token_version = u32::from_le_bytes(token_header[8..12].try_into().expect("four bytes"));
    if token_version != crate::tokens::FORMAT_VERSION {
        return Err(CorpusError::UnsupportedSchema {
            component: TOKENS_NAME,
            version: token_version,
        });
    }
    let header_len = u32::from_le_bytes(token_header[12..16].try_into().expect("four bytes"));
    let attributes = u32::from_le_bytes(token_header[16..20].try_into().expect("four bytes"));
    if header_len != 128 || attributes == 0 || attributes > u8::MAX as u32 {
        return Err(invalid_artifact("wrong V1 token header layout"));
    }
    if &token_header[24..56] != analysis.as_bytes() {
        return Err(CorpusError::AnalysisIdentityMismatch {
            component: TOKENS_NAME,
        });
    }
    if &token_header[56..88] != token_schema.as_bytes() {
        return Err(invalid_artifact(
            "token schema identity differs from manifest",
        ));
    }
    let token_sentences = u64::from_le_bytes(token_header[88..96].try_into().expect("eight bytes"));
    if token_sentences != manifest.sentence_count {
        return Err(CorpusError::SentenceCountMismatch {
            component: TOKENS_NAME,
        });
    }

    let location_header = read_prefix(&root.join(LOCATIONS_NAME), 128, LOCATIONS_NAME)?;
    if &location_header[..8] != b"LINGLOC1" {
        return Err(invalid_artifact("wrong location magic"));
    }
    let location_version =
        u32::from_le_bytes(location_header[8..12].try_into().expect("four bytes"));
    if location_version != 1 {
        return Err(CorpusError::UnsupportedSchema {
            component: LOCATIONS_NAME,
            version: location_version,
        });
    }
    if &location_header[16..48] != analysis.as_bytes() {
        return Err(CorpusError::AnalysisIdentityMismatch {
            component: LOCATIONS_NAME,
        });
    }
    let location_sentences =
        u64::from_le_bytes(location_header[48..56].try_into().expect("eight bytes"));
    if location_sentences != manifest.sentence_count {
        return Err(CorpusError::SentenceCountMismatch {
            component: LOCATIONS_NAME,
        });
    }
    Ok(())
}

fn read_prefix(path: &Path, len: usize, component: &'static str) -> Result<Vec<u8>, CorpusError> {
    let mut file = File::open(path)?;
    let mut bytes = vec![0; len];
    file.read_exact(&mut bytes)
        .map_err(|error| match error.kind() {
            std::io::ErrorKind::UnexpectedEof => CorpusError::TruncatedComponent { component },
            _ => CorpusError::Io(error),
        })?;
    Ok(bytes)
}

fn map_open_error(error: CorpusError) -> CorpusError {
    match error {
        CorpusError::Truncated { component, .. } => CorpusError::TruncatedComponent { component },
        CorpusError::VerificationMismatch {
            component, message, ..
        } if message.contains("analysis identity") => {
            CorpusError::AnalysisIdentityMismatch { component }
        }
        error => error,
    }
}

fn map_provenance_error(error: CorpusError) -> CorpusError {
    match error {
        CorpusError::InvalidFormat { message, .. } if message.contains("sidecar") => {
            CorpusError::SqliteSidecar
        }
        CorpusError::VerificationMismatch { message, .. }
            if message.contains("analysis identity") =>
        {
            CorpusError::AnalysisIdentityMismatch {
                component: PROVENANCE_NAME,
            }
        }
        CorpusError::VerificationMismatch { message, .. }
            if message.contains("sentence count")
                || message.contains("row count")
                || message.contains("sentence IDs") =>
        {
            CorpusError::SentenceCountMismatch {
                component: PROVENANCE_NAME,
            }
        }
        error => error,
    }
}

fn component_name(name: &str) -> &'static str {
    match name {
        TOKENS_NAME => TOKENS_NAME,
        LOCATIONS_NAME => LOCATIONS_NAME,
        PROVENANCE_NAME => PROVENANCE_NAME,
        ENHANCED_NAME => ENHANCED_NAME,
        crate::postings::NAME => crate::postings::NAME,
        _ => "artifact",
    }
}

fn json_error(error: serde_json::Error) -> CorpusError {
    CorpusError::InvalidFormat {
        component: "corpus builder",
        message: error.to_string(),
    }
}

fn invalid_builder(message: impl Into<String>) -> CorpusError {
    CorpusError::InvalidFormat {
        component: "corpus builder",
        message: message.into(),
    }
}

pub(crate) fn invalid_artifact(message: impl Into<String>) -> CorpusError {
    CorpusError::InvalidFormat {
        component: "artifact",
        message: message.into(),
    }
}
