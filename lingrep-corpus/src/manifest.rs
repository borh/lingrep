use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::Path,
};

use lingrep_core::corpus::{DiscoveryReport, InventoryStats, TraversalHighWater, TraversalLimits};
use serde::{Deserialize, Serialize};

use lingrep_core::analysis::SemanticAnalysisIdentity;
use lingrep_core::token_match::{
    AttributeKind, AttributeProvider, SchemaAttribute, TokenAttributeIndex, TokenSchema,
};

use crate::{
    AdmissionIdentity, AnalysisIdentity, AnalysisInputs, AnalyzerRecord, CorpusError,
    DictionaryFingerprint, IdentityScheme, ReaderIdentity, SourceIdentity, TokenSchemaIdentity,
    TrustClass,
};

pub(crate) const MANIFEST_NAME: &str = "manifest.json";

/// Whether `path` names a **prepared corpus artifact** rather than a corpus
/// source.
///
/// Structural and cheap: a directory carrying a `manifest.json`. It answers
/// "which reader does this path want", not "is this artifact sound" —
/// [`crate::CorpusArtifact::open`] is the admission check and is the thing
/// that reports *why* a malformed artifact is refused.
///
/// It lives here because the artifact's on-disk shape is this crate's to
/// define. A consumer that hand-rolled `path.join("manifest.json")` would be a
/// second definition of the same rule, free to drift from this one — the same
/// reason [`crate::canonical_artifact_path`] owns the naming rule rather than
/// each caller re-deriving it.
#[must_use]
pub fn is_prepared_artifact(path: &Path) -> bool {
    path.is_dir() && path.join(MANIFEST_NAME).is_file()
}
pub(crate) const LOCATIONS_NAME: &str = "locations.v1";
pub(crate) const PROVENANCE_NAME: &str = "provenance.sqlite";
pub(crate) const TOKENS_NAME: &str = "tokens.v1.zst";
const MANIFEST_SCHEMA_VERSION: u32 = 1;

/// Schema of the lingrep provenance sidecar.
const PROVENANCE_SCHEMA_VERSION: u32 = crate::provenance::SCHEMA_VERSION;

const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;
const ADMITTED_INVENTORY_DOMAIN: &[u8] = b"lingrep-corpus/admitted-logical-inventory/v1";
const ADMITTED_ENTRY_TAG: u8 = 1;
const ADMITTED_END_TAG: u8 = 255;

pub(crate) struct AdmittedInventoryHasher {
    hash: blake3::Hasher,
    documents: u64,
}

impl AdmittedInventoryHasher {
    pub(crate) fn new() -> Self {
        let mut hash = blake3::Hasher::new();
        hash.update(&(ADMITTED_INVENTORY_DOMAIN.len() as u64).to_le_bytes());
        hash.update(ADMITTED_INVENTORY_DOMAIN);
        Self { hash, documents: 0 }
    }

    pub(crate) fn push(
        &mut self,
        logical_name: &str,
        target_group_ordinal: u64,
        external_target: bool,
    ) -> Result<(), CorpusError> {
        self.hash.update(&[ADMITTED_ENTRY_TAG]);
        self.hash.update(&(logical_name.len() as u64).to_le_bytes());
        self.hash.update(logical_name.as_bytes());
        self.hash.update(&target_group_ordinal.to_le_bytes());
        self.hash.update(&[u8::from(external_target)]);
        self.documents = self
            .documents
            .checked_add(1)
            .ok_or_else(|| invalid("admitted inventory document count overflow"))?;
        Ok(())
    }

    pub(crate) fn finish(mut self) -> [u8; 32] {
        self.hash.update(&[ADMITTED_END_TAG]);
        self.hash.update(&self.documents.to_le_bytes());
        *self.hash.finalize().as_bytes()
    }
}

/// What a build truncated, and the limit it truncated at.
///
/// Recorded rather than merely warned about, because the warning reaches the
/// operator who ran the build and the artifact outlives them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SentenceTruncation {
    /// Number of sentences truncated. Never zero: absence is spelled `None`.
    pub count: u64,
    /// The `max_sentence_bytes` in force, in bytes.
    pub limit: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Manifest {
    pub schema_version: u32,
    pub format_generation: u32,
    pub identity_scheme: IdentityScheme,
    pub source_identity: String,
    pub analysis_identity: String,
    pub reader_identity: String,
    pub admission_identity: String,
    pub dictionary_fingerprint: String,
    pub token_schema_identity: String,
    pub physical_token_schema_identity: String,
    pub logical_token_schema: Vec<ManifestSchemaAttribute>,
    /// Enhanced graph capability and normalization contract. Presence must
    /// agree with the enhanced component inventory.
    #[serde(default)]
    pub enhanced_graph: Option<EnhancedGraphManifest>,
    pub trust_class: String,
    pub analyzer: AnalyzerRecord,
    pub document_count: u64,
    pub sentence_count: u64,
    pub token_count: u64,
    pub inventory: ManifestInventory,
    #[serde(default)]
    pub admitted_inventory: ManifestInventory,
    pub configured_limits: ManifestLimits,
    pub observed: ManifestHighWater,
    /// Sentences this build truncated to fit `--max-sentence-bytes`, when any.
    ///
    /// **This is the one recorded fact that makes a resource limit semantic.**
    /// Every other limit either rejects a corpus or is invisible, which is why
    /// limits are deliberately absent from identity: two limits that both
    /// complete emit the same corpus. Truncation breaks that — two limits that
    /// both complete emit *different* corpora — so an artifact that truncated
    /// anything must record the limit it truncated at, or a later build under
    /// a larger limit silently reuses a shorter corpus.
    ///
    /// `#[serde(default)]` for the reason `identity_scheme` carries it: an
    /// artifact written before this field existed truncated nothing, because
    /// the build refused instead, and `None` *is* that value. So
    /// [`MANIFEST_SCHEMA_VERSION`] does not move and no artifact on disk is
    /// invalidated.
    #[serde(default)]
    pub sentence_truncation: Option<SentenceTruncation>,
    pub ordered_inventory_digest: String,
    #[serde(default)]
    pub admitted_inventory_digest: String,
    pub provenance_schema_version: u32,
    pub components: BTreeMap<String, ComponentFact>,
    pub producer_version: String,
    pub build_timestamp_unix_seconds: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct ComponentFact {
    pub length: u64,
    pub blake3: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ManifestSchemaAttribute {
    key: String,
    kind: String,
    provider: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EnhancedGraphManifest {
    format_version: u32,
    normalization_identity: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(crate) struct ManifestInventory {
    documents: u64,
    distinct_targets: u64,
    alias_documents: u64,
    max_target_multiplicity: u64,
    external_targets: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct ManifestLimits {
    max_depth: usize,
    max_source_entries: u64,
    max_directory_entries: usize,
    max_documents: u64,
    max_index_bytes: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct ManifestHighWater {
    max_depth: usize,
    examined_entries: u64,
    max_directory_entries: usize,
    index_bytes: usize,
}

pub(crate) struct ManifestInputs<'a> {
    pub source_identity: SourceIdentity,
    pub analysis_identity: AnalysisIdentity,
    pub analysis: &'a AnalysisInputs,
    pub logical_token_schema: &'a TokenSchema,
    pub physical_token_schema: TokenSchemaIdentity,
    pub enhanced_graph: bool,
    /// Recorded inspection fact. Not an identity input; see [`AnalysisInputs`].
    pub dictionary: DictionaryFingerprint,
    pub trust: TrustClass,
    pub analyzer: &'a AnalyzerRecord,
    pub document_count: u64,
    pub sentence_count: u64,
    pub token_count: u64,
    pub discovery: &'a DiscoveryReport,
    pub admitted_inventory: &'a InventoryStats,
    pub admitted_inventory_digest: [u8; 32],
    pub components: BTreeMap<String, ComponentFact>,
    pub sentence_truncation: Option<SentenceTruncation>,
}

impl Manifest {
    pub(crate) fn new(inputs: ManifestInputs<'_>) -> Self {
        Self {
            schema_version: MANIFEST_SCHEMA_VERSION,
            format_generation: crate::CORPUS_ARTIFACT_FORMAT_GENERATION,
            identity_scheme: IdentityScheme::CURRENT,
            source_identity: encode_digest(inputs.source_identity.as_bytes()),
            analysis_identity: encode_digest(inputs.analysis_identity.as_bytes()),
            reader_identity: encode_digest(inputs.analysis.reader().as_bytes()),
            admission_identity: encode_digest(inputs.analysis.admission().as_bytes()),
            dictionary_fingerprint: encode_digest(inputs.dictionary.as_bytes()),
            token_schema_identity: encode_digest(inputs.analysis.token_schema().as_bytes()),
            physical_token_schema_identity: encode_digest(inputs.physical_token_schema.as_bytes()),
            logical_token_schema: inputs
                .logical_token_schema
                .attributes()
                .iter()
                .map(|attribute| ManifestSchemaAttribute {
                    key: attribute.key().as_str().to_owned(),
                    kind: match attribute.kind() {
                        AttributeKind::Scalar => "scalar",
                        AttributeKind::StringSet => "string-set",
                    }
                    .to_owned(),
                    provider: match attribute.provider() {
                        AttributeProvider::Dynamic => "dynamic",
                        AttributeProvider::TokenColumn(_) => "token-column",
                        AttributeProvider::EnhancedGraph => "enhanced-graph",
                    }
                    .to_owned(),
                })
                .collect(),
            enhanced_graph: inputs.enhanced_graph.then(|| EnhancedGraphManifest {
                format_version: crate::enhanced::FORMAT_VERSION,
                normalization_identity: encode_digest(&crate::enhanced::normalization_identity()),
            }),
            trust_class: inputs.trust.address_component().to_owned(),
            analyzer: inputs.analyzer.clone(),
            document_count: inputs.document_count,
            sentence_count: inputs.sentence_count,
            token_count: inputs.token_count,
            inventory: (&inputs.discovery.inventory).into(),
            admitted_inventory: inputs.admitted_inventory.into(),
            configured_limits: (&inputs.discovery.configured_limits).into(),
            observed: (&inputs.discovery.observed).into(),
            ordered_inventory_digest: encode_digest(&inputs.discovery.ordered_inventory_digest),
            admitted_inventory_digest: encode_digest(&inputs.admitted_inventory_digest),
            provenance_schema_version: PROVENANCE_SCHEMA_VERSION,
            components: inputs.components,
            sentence_truncation: inputs.sentence_truncation,
            producer_version: env!("CARGO_PKG_VERSION").to_owned(),
            build_timestamp_unix_seconds: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        }
    }

    pub(crate) fn write_new(&self, path: &Path) -> Result<(), CorpusError> {
        let bytes = serde_json::to_vec_pretty(self).map_err(json)?;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(CorpusError::Io)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        Ok(())
    }

    pub(crate) fn open(path: &Path) -> Result<Self, CorpusError> {
        let metadata = fs::metadata(path)?;
        if metadata.len() > MAX_MANIFEST_BYTES {
            return Err(invalid("manifest exceeds the fixed size bound"));
        }
        let mut bytes = Vec::with_capacity(
            usize::try_from(metadata.len()).map_err(|_| invalid("manifest is too large"))?,
        );
        File::open(path)?.read_to_end(&mut bytes)?;
        #[derive(Deserialize)]
        struct ManifestVersion {
            schema_version: u32,
            format_generation: u32,
        }
        let version: ManifestVersion = serde_json::from_slice(&bytes).map_err(json)?;
        let generation = version.format_generation;
        if generation != crate::CORPUS_ARTIFACT_FORMAT_GENERATION {
            return Err(CorpusError::UnsupportedArtifactGeneration { generation });
        }
        if version.schema_version != MANIFEST_SCHEMA_VERSION {
            return Err(CorpusError::PreparedArtifactRebuildRequired {
                version: version.schema_version,
            });
        }
        let manifest: Self = serde_json::from_slice(&bytes).map_err(json)?;
        if manifest.provenance_schema_version != PROVENANCE_SCHEMA_VERSION {
            return Err(CorpusError::UnsupportedSchema {
                component: "provenance.sqlite",
                version: manifest.provenance_schema_version,
            });
        }
        manifest.validate_components()?;
        manifest.validate_inventories()?;
        Ok(manifest)
    }

    pub(crate) fn source_identity(&self) -> Result<SourceIdentity, CorpusError> {
        Ok(SourceIdentity::from_bytes(decode_digest(
            &self.source_identity,
            "source identity",
        )?))
    }

    pub(crate) fn analysis_identity(&self) -> Result<AnalysisIdentity, CorpusError> {
        Ok(AnalysisIdentity::from_bytes(decode_digest(
            &self.analysis_identity,
            "analysis identity",
        )?))
    }

    pub(crate) fn token_schema_identity(&self) -> Result<TokenSchemaIdentity, CorpusError> {
        Ok(TokenSchemaIdentity::from_bytes(decode_digest(
            &self.token_schema_identity,
            "token schema identity",
        )?))
    }

    pub(crate) fn physical_token_schema_identity(
        &self,
    ) -> Result<TokenSchemaIdentity, CorpusError> {
        Ok(TokenSchemaIdentity::from_bytes(decode_digest(
            &self.physical_token_schema_identity,
            "physical token schema identity",
        )?))
    }

    pub(crate) fn logical_token_schema(&self) -> Result<TokenSchema, CorpusError> {
        let attributes = self
            .logical_token_schema
            .iter()
            .map(|attribute| {
                let kind = match attribute.kind.as_str() {
                    "scalar" => AttributeKind::Scalar,
                    "string-set" => AttributeKind::StringSet,
                    _ => return Err(invalid("manifest logical attribute kind is invalid")),
                };
                let mut value = SchemaAttribute::new(&attribute.key, kind).map_err(|error| {
                    invalid(format!("manifest logical schema is invalid: {error}"))
                })?;
                let provider = match attribute.provider.as_str() {
                    "dynamic" => AttributeProvider::Dynamic,
                    "enhanced-graph" if attribute.key == "dep.enhanced" => {
                        AttributeProvider::EnhancedGraph
                    }
                    "token-column" => TokenAttributeIndex::from_attribute_key(value.key())
                        .map(AttributeProvider::TokenColumn)
                        .ok_or_else(|| invalid("manifest token-column provider has no column"))?,
                    _ => return Err(invalid("manifest logical attribute provider is invalid")),
                };
                value = value.with_provider(provider);
                Ok(value)
            })
            .collect::<Result<Vec<_>, CorpusError>>()?;
        let schema = TokenSchema::new(attributes)
            .map_err(|error| invalid(format!("manifest logical schema is invalid: {error}")))?;
        if schema.digest().as_bytes() != self.token_schema_identity()?.as_bytes() {
            return Err(invalid("manifest logical schema differs from its identity"));
        }
        let graph_provider = schema
            .attributes()
            .iter()
            .any(|attribute| attribute.provider() == AttributeProvider::EnhancedGraph);
        if graph_provider != self.enhanced_graph.is_some() {
            return Err(invalid(
                "manifest enhanced provider and component capability differ",
            ));
        }
        Ok(schema)
    }

    pub(crate) fn enhanced_graph_capable(&self) -> bool {
        self.enhanced_graph.is_some()
    }

    pub(crate) fn analysis_inputs(&self) -> Result<AnalysisInputs, CorpusError> {
        Ok(AnalysisInputs::from_recorded(
            ReaderIdentity::from_bytes(decode_digest(&self.reader_identity, "reader identity")?),
            AdmissionIdentity::from_bytes(decode_digest(
                &self.admission_identity,
                "admission identity",
            )?),
            SemanticAnalysisIdentity::from_bytes(decode_digest(
                &self.analyzer.semantic_identity,
                "analyzer semantic identity",
            )?),
            self.token_schema_identity()?,
        ))
    }

    /// Returns the dictionary fingerprint recorded as an inspection fact.
    pub(crate) fn dictionary_fingerprint(&self) -> Result<DictionaryFingerprint, CorpusError> {
        Ok(DictionaryFingerprint::from_bytes(decode_digest(
            &self.dictionary_fingerprint,
            "dictionary fingerprint",
        )?))
    }

    pub(crate) fn discovery_report(&self) -> Result<DiscoveryReport, CorpusError> {
        Ok(DiscoveryReport {
            inventory: (&self.inventory).into(),
            configured_limits: (&self.configured_limits).into(),
            observed: (&self.observed).into(),
            ordered_inventory_digest: decode_digest(
                &self.ordered_inventory_digest,
                "ordered inventory digest",
            )?,
        })
    }

    pub(crate) fn admitted_inventory(&self) -> InventoryStats {
        (&self.admitted_inventory).into()
    }

    pub(crate) fn admitted_inventory_digest(&self) -> Result<[u8; 32], CorpusError> {
        decode_digest(&self.admitted_inventory_digest, "admitted inventory digest")
    }

    pub(crate) fn component_blake3(&self, name: &str) -> Result<[u8; 32], CorpusError> {
        let fact = self
            .components
            .get(name)
            .ok_or_else(|| invalid("manifest component is absent"))?;
        decode_digest(&fact.blake3, "component BLAKE3")
    }

    fn validate_components(&self) -> Result<(), CorpusError> {
        let required = [TOKENS_NAME, LOCATIONS_NAME, PROVENANCE_NAME];
        let expected_len = required.len()
            + usize::from(self.enhanced_graph.is_some())
            + usize::from(self.components.contains_key(crate::postings::NAME));
        if self.components.len() != expected_len
            || required
                .iter()
                .any(|name| !self.components.contains_key(*name))
            || self.enhanced_graph.is_some()
                != self.components.contains_key(crate::enhanced::ENHANCED_NAME)
        {
            return Err(invalid("manifest component inventory is not canonical"));
        }
        if let Some(record) = &self.enhanced_graph {
            let normalization = decode_digest(
                &record.normalization_identity,
                "enhanced normalization identity",
            )?;
            if record.format_version != crate::enhanced::FORMAT_VERSION
                || normalization != crate::enhanced::normalization_identity()
            {
                return Err(invalid("manifest enhanced graph contract is invalid"));
            }
        }
        for fact in self.components.values() {
            decode_digest(&fact.blake3, "component BLAKE3")?;
        }
        Ok(())
    }

    fn validate_inventories(&self) -> Result<(), CorpusError> {
        let discovered: InventoryStats = (&self.inventory).into();
        let admitted: InventoryStats = (&self.admitted_inventory).into();
        Self::validate_inventory(&discovered)?;
        Self::validate_inventory(&admitted)?;
        decode_digest(&self.admitted_inventory_digest, "admitted inventory digest")?;
        let limits_are_positive = self.configured_limits.max_depth > 0
            && self.configured_limits.max_source_entries > 0
            && self.configured_limits.max_directory_entries > 0
            && self.configured_limits.max_documents > 0
            && self.configured_limits.max_index_bytes > 0;
        let observed_is_bounded = self.observed.max_depth <= self.configured_limits.max_depth
            && self.observed.examined_entries <= self.configured_limits.max_source_entries
            && self.observed.max_directory_entries <= self.configured_limits.max_directory_entries
            && self.observed.index_bytes <= self.configured_limits.max_index_bytes
            && discovered.documents <= self.configured_limits.max_documents
            && admitted.documents <= self.configured_limits.max_documents
            && self.document_count <= self.configured_limits.max_documents;
        let aggregates_are_consistent =
            self.observed.max_directory_entries as u64 <= self.observed.examined_entries;
        if !limits_are_positive
            || !observed_is_bounded
            || !aggregates_are_consistent
            || admitted.documents != self.document_count
        {
            return Err(CorpusError::DiscoveryReportMismatch);
        }
        Ok(())
    }

    fn validate_inventory(inventory: &InventoryStats) -> Result<(), CorpusError> {
        let expected_aliases = inventory
            .documents
            .checked_sub(inventory.distinct_targets)
            .ok_or(CorpusError::DiscoveryReportMismatch)?;
        let multiplicity_is_valid = if inventory.documents == 0 {
            inventory.distinct_targets == 0 && inventory.max_target_multiplicity == 0
        } else {
            let minimum_maximum = inventory
                .documents
                .div_ceil(inventory.distinct_targets.max(1));
            let maximum_maximum = inventory
                .documents
                .checked_sub(inventory.distinct_targets)
                .and_then(|aliases| aliases.checked_add(1))
                .unwrap_or(0);
            inventory.distinct_targets > 0
                && inventory.max_target_multiplicity >= minimum_maximum
                && inventory.max_target_multiplicity <= maximum_maximum
        };
        if inventory.alias_documents != expected_aliases
            || inventory.external_targets > inventory.documents
            || !multiplicity_is_valid
        {
            return Err(CorpusError::DiscoveryReportMismatch);
        }
        Ok(())
    }
}

impl From<&InventoryStats> for ManifestInventory {
    fn from(value: &InventoryStats) -> Self {
        Self {
            documents: value.documents,
            distinct_targets: value.distinct_targets,
            alias_documents: value.alias_documents,
            max_target_multiplicity: value.max_target_multiplicity,
            external_targets: value.external_targets,
        }
    }
}

impl From<&ManifestInventory> for InventoryStats {
    fn from(value: &ManifestInventory) -> Self {
        Self {
            documents: value.documents,
            distinct_targets: value.distinct_targets,
            alias_documents: value.alias_documents,
            max_target_multiplicity: value.max_target_multiplicity,
            external_targets: value.external_targets,
        }
    }
}

impl From<&TraversalLimits> for ManifestLimits {
    fn from(value: &TraversalLimits) -> Self {
        Self {
            max_depth: value.max_depth,
            max_source_entries: value.max_source_entries,
            max_directory_entries: value.max_directory_entries,
            max_documents: value.max_documents,
            max_index_bytes: value.max_index_bytes,
        }
    }
}

impl From<&ManifestLimits> for TraversalLimits {
    fn from(value: &ManifestLimits) -> Self {
        Self {
            max_depth: value.max_depth,
            max_source_entries: value.max_source_entries,
            max_directory_entries: value.max_directory_entries,
            max_documents: value.max_documents,
            max_index_bytes: value.max_index_bytes,
        }
    }
}

impl From<&TraversalHighWater> for ManifestHighWater {
    fn from(value: &TraversalHighWater) -> Self {
        Self {
            max_depth: value.max_depth,
            examined_entries: value.examined_entries,
            max_directory_entries: value.max_directory_entries,
            index_bytes: value.index_bytes,
        }
    }
}

impl From<&ManifestHighWater> for TraversalHighWater {
    fn from(value: &ManifestHighWater) -> Self {
        Self {
            max_depth: value.max_depth,
            examined_entries: value.examined_entries,
            max_directory_entries: value.max_directory_entries,
            index_bytes: value.index_bytes,
        }
    }
}

pub(crate) fn component_fact(path: &Path) -> Result<ComponentFact, CorpusError> {
    component_fact_guarded(path, || true)?
        .ok_or_else(|| invalid("unconditional component hash was cancelled"))
}

pub(crate) fn component_fact_guarded(
    path: &Path,
    mut should_continue: impl FnMut() -> bool,
) -> Result<Option<ComponentFact>, CorpusError> {
    let metadata = fs::metadata(path)?;
    let mut file = File::open(path)?;
    let mut hash = blake3::Hasher::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        if !should_continue() {
            return Ok(None);
        }
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hash.update(&buffer[..read]);
    }
    Ok(Some(ComponentFact {
        length: metadata.len(),
        blake3: hash.finalize().to_hex().to_string(),
    }))
}

pub(crate) fn encode_digest(bytes: &[u8; 32]) -> String {
    let mut encoded = String::with_capacity(64);
    for byte in bytes {
        use std::fmt::Write as _;
        write!(&mut encoded, "{byte:02x}").expect("writing to String cannot fail");
    }
    encoded
}

pub(crate) fn decode_digest(value: &str, field: &str) -> Result<[u8; 32], CorpusError> {
    if value.len() != 64 {
        return Err(invalid(format!("{field} must contain 64 hex digits")));
    }
    let mut bytes = [0_u8; 32];
    for (index, byte) in bytes.iter_mut().enumerate() {
        // `value` is verified above to be 64 ASCII hex digits, so every
        // 2-char pair starts at a char boundary.
        #[expect(
            clippy::string_slice,
            reason = "`value` is 64 ASCII hex digits, so pairs are ASCII-aligned"
        )]
        let pair = &value[index * 2..index * 2 + 2];
        *byte = u8::from_str_radix(pair, 16)
            .map_err(|_| invalid(format!("{field} is not hexadecimal")))?;
    }
    Ok(bytes)
}

fn json(error: serde_json::Error) -> CorpusError {
    invalid(error.to_string())
}

fn invalid(message: impl Into<String>) -> CorpusError {
    CorpusError::InvalidFormat {
        component: MANIFEST_NAME,
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::component_fact_guarded;

    #[test]
    fn component_hashing_cancels_between_bounded_chunks() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("large-component");
        std::fs::write(&path, vec![0x5a; 3 * 64 * 1024]).unwrap();
        let calls = Cell::new(0);

        let fact = component_fact_guarded(&path, || {
            let next = calls.get() + 1;
            calls.set(next);
            next < 2
        })
        .unwrap();

        assert!(fact.is_none());
        assert_eq!(calls.get(), 2);
    }
}
