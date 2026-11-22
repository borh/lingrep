use crate::token_match::{TokenSchema, TokenSchemaDigest};

use super::NormalizationPolicy;

const SEMANTIC_DOMAIN: &[u8] = b"lingrep.analyzer-semantics/v1";

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[repr(u8)]
pub enum AnalyzerFamily {
    Vibrato = 0,
    Sudachi = 1,
    Spacy = 2,
    Stanza = 3,
    External = 4,
    Pretokenized = 5,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[repr(u8)]
pub enum ExtractionProfile {
    Tokenize = 0,
    Morphology = 1,
    Syntax = 2,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[repr(u8)]
pub enum SegmentationMode {
    Input = 0,
    Analyzer = 1,
}

/// Returns the built-in schema for `family` and `profile` when one is defined.
///
/// Adapter-hosted analyzers declare their schema during the handshake, so this
/// function returns `None` for them. Vibrato morphology assumes UniDic 202512
/// because `featurize` reads the dictionary's features by column position.
#[must_use]
pub fn declared_token_schema(
    family: AnalyzerFamily,
    profile: ExtractionProfile,
) -> Option<TokenSchema> {
    match (family, profile) {
        (AnalyzerFamily::Vibrato | AnalyzerFamily::Sudachi, ExtractionProfile::Tokenize) => Some(
            TokenSchema::from_keys(["text", "surface"])
                .expect("the portable surface schema is valid"),
        ),
        (AnalyzerFamily::Vibrato, ExtractionProfile::Morphology) => {
            Some(TokenSchema::unidic_morphology())
        }
        (AnalyzerFamily::Sudachi, ExtractionProfile::Morphology) => Some(
            TokenSchema::from_keys(super::sudachi::MORPHOLOGY_KEYS)
                .expect("the Sudachi morphology schema is valid"),
        ),
        (AnalyzerFamily::Pretokenized, ExtractionProfile::Syntax) => {
            Some(crate::corpus::conllu::token_schema())
        }
        (
            AnalyzerFamily::Spacy
            | AnalyzerFamily::Stanza
            | AnalyzerFamily::External
            | AnalyzerFamily::Pretokenized,
            _,
        )
        | (AnalyzerFamily::Vibrato | AnalyzerFamily::Sudachi, ExtractionProfile::Syntax) => None,
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SemanticAnalysisIdentity([u8; 32]);

impl SemanticAnalysisIdentity {
    /// Reconstructs an identity from bytes a producer already recorded.
    ///
    /// The only supported caller is a reader restoring what a manifest stored.
    /// Deriving one is [`AnalyzerDescriptor::semantic_identity`]'s job, and
    /// this constructor is not a way around it.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// Fixed semantic inputs for one analyzer plan.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AnalyzerDescriptor {
    family: AnalyzerFamily,
    profile: ExtractionProfile,
    segmentation: SegmentationMode,
    normalization: NormalizationPolicy,
    language_policy: Box<str>,
    boundary_policy: Box<str>,
    implementation_digest: [u8; 32],
    resource_manifest_digest: [u8; 32],
    projection_policy_digest: [u8; 32],
    alignment_policy_digest: [u8; 32],
    component_closure_digest: [u8; 32],
    output_config_digest: [u8; 32],
    schema_digest: TokenSchemaDigest,
}

impl AnalyzerDescriptor {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        family: AnalyzerFamily,
        profile: ExtractionProfile,
        segmentation: SegmentationMode,
        normalization: NormalizationPolicy,
        language_policy: impl Into<Box<str>>,
        boundary_policy: impl Into<Box<str>>,
        implementation_digest: [u8; 32],
        resource_manifest_digest: [u8; 32],
        projection_policy_digest: [u8; 32],
        alignment_policy_digest: [u8; 32],
        component_closure_digest: [u8; 32],
        output_config_digest: [u8; 32],
        schema_digest: TokenSchemaDigest,
    ) -> Result<Self, &'static str> {
        let language_policy = language_policy.into();
        let boundary_policy = boundary_policy.into();
        if language_policy.is_empty() || boundary_policy.is_empty() {
            return Err("language and boundary policies must be nonempty");
        }
        Ok(Self {
            family,
            profile,
            segmentation,
            normalization,
            language_policy,
            boundary_policy,
            implementation_digest,
            resource_manifest_digest,
            projection_policy_digest,
            alignment_policy_digest,
            component_closure_digest,
            output_config_digest,
            schema_digest,
        })
    }

    #[must_use]
    pub fn semantic_identity(&self) -> SemanticAnalysisIdentity {
        let mut hash = blake3::Hasher::new();
        frame(&mut hash, SEMANTIC_DOMAIN);
        hash.update(&[self.family as u8]);
        hash.update(&[self.profile as u8]);
        hash.update(&[self.segmentation as u8]);
        hash.update(self.normalization.identity_bytes());
        frame(&mut hash, self.language_policy.as_bytes());
        frame(&mut hash, self.boundary_policy.as_bytes());
        hash.update(&self.implementation_digest);
        hash.update(&self.resource_manifest_digest);
        hash.update(&self.projection_policy_digest);
        hash.update(&self.alignment_policy_digest);
        hash.update(&self.component_closure_digest);
        hash.update(&self.output_config_digest);
        hash.update(self.schema_digest.as_bytes());
        SemanticAnalysisIdentity(*hash.finalize().as_bytes())
    }

    #[must_use]
    pub const fn family(&self) -> AnalyzerFamily {
        self.family
    }

    #[must_use]
    pub const fn profile(&self) -> ExtractionProfile {
        self.profile
    }

    #[must_use]
    pub const fn segmentation(&self) -> SegmentationMode {
        self.segmentation
    }

    #[must_use]
    pub const fn normalization(&self) -> NormalizationPolicy {
        self.normalization
    }

    #[must_use]
    pub fn language_policy(&self) -> &str {
        &self.language_policy
    }

    #[must_use]
    pub fn boundary_policy(&self) -> &str {
        &self.boundary_policy
    }

    #[must_use]
    pub const fn implementation_digest(&self) -> &[u8; 32] {
        &self.implementation_digest
    }

    #[must_use]
    pub const fn resource_manifest_digest(&self) -> &[u8; 32] {
        &self.resource_manifest_digest
    }

    #[must_use]
    pub const fn projection_policy_digest(&self) -> &[u8; 32] {
        &self.projection_policy_digest
    }

    #[must_use]
    pub const fn alignment_policy_digest(&self) -> &[u8; 32] {
        &self.alignment_policy_digest
    }

    #[must_use]
    pub const fn component_closure_digest(&self) -> &[u8; 32] {
        &self.component_closure_digest
    }

    #[must_use]
    pub const fn output_config_digest(&self) -> &[u8; 32] {
        &self.output_config_digest
    }

    #[must_use]
    pub const fn schema_digest(&self) -> TokenSchemaDigest {
        self.schema_digest
    }
}

/// Execution facts excluded from semantic analysis identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExecutionProvenance {
    pub backend: Box<str>,
    pub runtime: Box<str>,
    pub lock_digest: [u8; 32],
    pub workers: u32,
    pub batch_size: u32,
}

fn frame(hash: &mut blake3::Hasher, bytes: &[u8]) {
    hash.update(&(bytes.len() as u64).to_le_bytes());
    hash.update(bytes);
}

#[cfg(test)]
mod tests {
    use crate::token_match::TokenSchema;

    use super::*;

    fn descriptor() -> AnalyzerDescriptor {
        AnalyzerDescriptor::new(
            AnalyzerFamily::Vibrato,
            ExtractionProfile::Morphology,
            SegmentationMode::Input,
            NormalizationPolicy::None,
            "ja",
            "input-v1",
            [1; 32],
            [2; 32],
            [3; 32],
            [4; 32],
            [5; 32],
            [6; 32],
            TokenSchema::unidic_morphology().digest(),
        )
        .unwrap()
    }

    #[test]
    fn semantic_identity_covers_output_inputs_but_not_execution_provenance() {
        let original = descriptor();
        let mut changed = descriptor();
        changed.resource_manifest_digest = [9; 32];
        assert_ne!(original.semantic_identity(), changed.semantic_identity());
        let mut changed = descriptor();
        changed.component_closure_digest = [9; 32];
        assert_ne!(original.semantic_identity(), changed.semantic_identity());

        let cpu = ExecutionProvenance {
            backend: "cpu".into(),
            runtime: "rust".into(),
            lock_digest: [7; 32],
            workers: 1,
            batch_size: 1,
        };
        let rocm = ExecutionProvenance {
            backend: "rocm".into(),
            workers: 8,
            ..cpu.clone()
        };
        assert_ne!(cpu, rocm);
        assert_eq!(
            original.semantic_identity(),
            descriptor().semantic_identity()
        );
    }
}
