use compact_str::CompactString;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::Arc;

use super::{AttributeKey, AttributeKeyError, TokenAttributeIndex};

/// Physical value kind declared by one schema attribute.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum AttributeKind {
    Scalar,
    StringSet,
}

/// Physical provider for one logical token attribute.
///
/// Provider identity is deliberately absent from [`TokenSchemaDigest`]: the
/// digest describes the logical values a source supplies, while this enum
/// describes how one implementation obtains them.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum AttributeProvider {
    /// Values arrive already projected from a dynamic or prepared token row.
    Dynamic,
    /// Values come from one current Vibrato token column.
    TokenColumn(TokenAttributeIndex),
    /// Values are derived from sentence-level enhanced graph adjacency.
    EnhancedGraph,
}

/// One canonical logical token-schema entry and its value provider.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SchemaAttribute {
    key: AttributeKey,
    kind: AttributeKind,
    provider: AttributeProvider,
}

impl SchemaAttribute {
    pub fn new(key: impl AsRef<str>, kind: AttributeKind) -> Result<Self, AttributeKeyError> {
        Ok(Self {
            key: AttributeKey::new(key)?,
            kind,
            provider: AttributeProvider::Dynamic,
        })
    }

    /// Selects the physical provider for this logical attribute.
    #[must_use]
    pub const fn with_provider(mut self, provider: AttributeProvider) -> Self {
        self.provider = provider;
        self
    }

    #[must_use]
    pub const fn key(&self) -> &AttributeKey {
        &self.key
    }

    #[must_use]
    pub const fn kind(&self) -> AttributeKind {
        self.kind
    }

    /// Returns the implementation that supplies this attribute's values.
    #[must_use]
    pub const fn provider(&self) -> AttributeProvider {
        self.provider
    }
}

/// A dense, schema-local attribute position used by bound execution plans.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct AttributeOrdinal(u8);

impl AttributeOrdinal {
    /// Constructs an ordinal from its zero-based position.
    #[must_use]
    pub const fn new(value: u8) -> Self {
        Self(value)
    }

    /// Returns the zero-based position for slice indexing.
    #[must_use]
    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

/// Structural identity of one ordered token schema.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct TokenSchemaDigest([u8; 32]);

impl TokenSchemaDigest {
    /// Returns the digest bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// An ordered set of attribute keys available from one token source.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TokenSchema {
    attributes: Box<[SchemaAttribute]>,
    keys: Box<[AttributeKey]>,
    digest: TokenSchemaDigest,
}

/// Why a token schema or projection could not be constructed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TokenSchemaError {
    EmptyKey,
    InvalidKey {
        key: CompactString,
        source: AttributeKeyError,
    },
    DuplicateKey(CompactString),
    ConflictingKind(CompactString),
    TooManyKeys(usize),
    UnavailableAttribute(AttributeKey),
}

impl fmt::Display for TokenSchemaError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyKey => formatter.write_str("token schema keys must not be empty"),
            Self::InvalidKey { key, source } => {
                write!(formatter, "invalid token schema key `{key}`: {source}")
            }
            Self::DuplicateKey(key) => write!(formatter, "duplicate token schema key `{key}`"),
            Self::ConflictingKind(key) => {
                write!(
                    formatter,
                    "token schema key `{key}` has conflicting value kinds"
                )
            }
            Self::TooManyKeys(count) => write!(
                formatter,
                "token schema has {count} keys; at most 255 are supported"
            ),
            Self::UnavailableAttribute(attribute) => {
                write!(formatter, "token schema does not provide `{attribute}`")
            }
        }
    }
}

impl std::error::Error for TokenSchemaError {}

impl TokenSchema {
    /// The complete ordered schema a UniDic feature layout supplies.
    ///
    /// Named for the **dictionary**, not the analyzer. Vibrato is a tokenizer
    /// and imposes no feature layout of its own: the entries here are UniDic's
    /// CSV columns, read positionally by
    /// [`featurize`](super::featurize), so the same code over an IPADic `.dic`
    /// would fill `unidic.lform` from IPADic's 原形 column. Every dictionary
    /// this workspace packages is UniDic 202512, which is what makes the
    /// layout a constant rather than a runtime question; see
    /// [`declared_token_schema`](crate::analysis::declared_token_schema),
    /// which carries the same precondition.
    #[must_use]
    pub fn unidic_morphology() -> Self {
        Self::new(
            TokenAttributeIndex::ALL
                .into_iter()
                // The physical `Text` slot repeats its surface `Orth` value.
                // Keep the storage slot for decoder compatibility, but
                // publish only the portable `text` identity.
                .filter(|index| *index != TokenAttributeIndex::Text)
                .map(AttributeKey::from)
                .map(|key| SchemaAttribute {
                    provider: TokenAttributeIndex::from_attribute_key(&key)
                        .map_or(AttributeProvider::Dynamic, AttributeProvider::TokenColumn),
                    key,
                    kind: AttributeKind::Scalar,
                }),
        )
        .expect("the compile-time current schema is nonempty and unique")
    }

    /// Constructs a canonical, kind-aware V1 schema.
    pub fn new<I>(attributes: I) -> Result<Self, TokenSchemaError>
    where
        I: IntoIterator<Item = SchemaAttribute>,
    {
        let mut attributes: Vec<_> = attributes.into_iter().collect();
        if attributes.len() > u8::MAX as usize {
            return Err(TokenSchemaError::TooManyKeys(attributes.len()));
        }
        attributes.sort_by(|left, right| canonical_key_order(left.key(), right.key()));
        for pair in attributes.windows(2) {
            if pair[0].key == pair[1].key {
                let key = CompactString::new(pair[0].key.as_str());
                return if pair[0].kind == pair[1].kind {
                    Err(TokenSchemaError::DuplicateKey(key))
                } else {
                    Err(TokenSchemaError::ConflictingKind(key))
                };
            }
        }
        let keys = attributes
            .iter()
            .map(|attribute| attribute.key.clone())
            .collect::<Vec<_>>()
            .into_boxed_slice();
        let digest = digest_attributes(&attributes);
        Ok(Self {
            attributes: attributes.into_boxed_slice(),
            keys,
            digest,
        })
    }

    /// Constructs an ordered schema and rejects ambiguous keys.
    pub fn from_keys<I, K>(keys: I) -> Result<Self, TokenSchemaError>
    where
        I: IntoIterator<Item = K>,
        K: AsRef<str>,
    {
        let attributes = keys
            .into_iter()
            .map(|key| {
                let raw = CompactString::new(key.as_ref());
                let key = AttributeKey::new(raw.as_str())
                    .map_err(|source| TokenSchemaError::InvalidKey { key: raw, source })?;
                Ok(SchemaAttribute {
                    provider: TokenAttributeIndex::from_attribute_key(&key)
                        .map_or(AttributeProvider::Dynamic, AttributeProvider::TokenColumn),
                    key,
                    kind: AttributeKind::Scalar,
                })
            })
            .collect::<Result<Vec<_>, TokenSchemaError>>()?;
        Self::new(attributes)
    }

    /// Returns the complete canonical schema entries.
    #[must_use]
    pub fn attributes(&self) -> &[SchemaAttribute] {
        &self.attributes
    }

    /// Returns the ordered schema keys.
    #[must_use]
    pub fn keys(&self) -> &[AttributeKey] {
        &self.keys
    }

    /// Returns this ordered schema's structural identity.
    #[must_use]
    pub const fn digest(&self) -> TokenSchemaDigest {
        self.digest
    }

    #[must_use]
    pub fn kind_for_key(&self, key: &AttributeKey) -> Option<AttributeKind> {
        self.attributes
            .iter()
            .find(|attribute| attribute.key() == key)
            .map(SchemaAttribute::kind)
    }

    /// Returns the physical provider for one logical key.
    #[must_use]
    pub fn provider_for_key(&self, key: &AttributeKey) -> Option<AttributeProvider> {
        self.attributes
            .iter()
            .find(|attribute| attribute.key() == key)
            .map(SchemaAttribute::provider)
    }

    /// Resolves a set of current symbolic attributes to dense ordinals.
    pub fn bind_projection<I, K>(&self, attributes: I) -> Result<BoundProjection, TokenSchemaError>
    where
        I: IntoIterator<Item = K>,
        K: Into<AttributeKey>,
    {
        let requested: BTreeSet<AttributeKey> = attributes.into_iter().map(Into::into).collect();
        for key in &requested {
            if !self.keys.contains(key) {
                return Err(TokenSchemaError::UnavailableAttribute(key.clone()));
            }
        }
        let keys: Arc<[_]> = self
            .keys
            .iter()
            .filter(|key| requested.contains(*key))
            .cloned()
            .collect();
        let kinds: Box<[_]> = keys
            .iter()
            .map(|key| {
                self.attributes
                    .iter()
                    .find(|attribute| attribute.key() == key)
                    .expect("projection keys came from this schema")
                    .kind()
            })
            .collect();
        let providers: Box<[_]> = keys
            .iter()
            .map(|key| {
                self.provider_for_key(key)
                    .expect("projection keys came from this schema")
            })
            .collect();
        let physical_attributes: Box<[_]> = providers
            .iter()
            .map(|provider| match provider {
                AttributeProvider::TokenColumn(attribute) => Some(*attribute),
                AttributeProvider::Dynamic | AttributeProvider::EnhancedGraph => None,
            })
            .collect();
        let attributes: Box<[_]> = physical_attributes.iter().flatten().copied().collect();
        let mut ordinals = [None; TokenAttributeIndex::COUNT];
        let mut ordinal_by_key = BTreeMap::new();
        for (index, (key, attribute)) in keys.iter().zip(&physical_attributes).enumerate() {
            let ordinal =
                u8::try_from(index).expect("a schema projection cannot exceed 255 attributes");
            let ordinal = AttributeOrdinal::new(ordinal);
            ordinal_by_key.insert(key.clone(), ordinal);
            if let Some(attribute) = attribute {
                ordinals[*attribute as usize] = Some(ordinal);
            }
        }
        Ok(BoundProjection {
            schema_digest: self.digest,
            keys,
            kinds,
            providers,
            attributes,
            physical_attributes,
            ordinal_by_key,
            ordinals,
        })
    }
}

/// The ordered dense attribute subset used by one bound execution plan.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BoundProjection {
    schema_digest: TokenSchemaDigest,
    keys: Arc<[AttributeKey]>,
    kinds: Box<[AttributeKind]>,
    providers: Box<[AttributeProvider]>,
    attributes: Box<[TokenAttributeIndex]>,
    physical_attributes: Box<[Option<TokenAttributeIndex>]>,
    ordinal_by_key: BTreeMap<AttributeKey, AttributeOrdinal>,
    ordinals: [Option<AttributeOrdinal>; TokenAttributeIndex::COUNT],
}

impl BoundProjection {
    /// Returns the source schema this projection was bound against.
    #[must_use]
    pub const fn schema_digest(&self) -> TokenSchemaDigest {
        self.schema_digest
    }

    /// Returns attributes in their dense ordinal order.
    #[must_use]
    pub fn keys(&self) -> &[AttributeKey] {
        &self.keys
    }

    pub(crate) fn shared_keys(&self) -> Arc<[AttributeKey]> {
        Arc::clone(&self.keys)
    }

    /// Returns value kinds in dense ordinal order.
    #[must_use]
    pub fn kinds(&self) -> &[AttributeKind] {
        &self.kinds
    }

    /// Returns physical providers in dense projection order.
    #[must_use]
    pub fn providers(&self) -> &[AttributeProvider] {
        &self.providers
    }

    /// Returns the provider for one dense ordinal.
    #[must_use]
    pub fn provider(&self, ordinal: AttributeOrdinal) -> Option<AttributeProvider> {
        self.providers.get(ordinal.index()).copied()
    }

    /// Returns the kind of one dense ordinal.
    #[must_use]
    pub fn kind(&self, ordinal: AttributeOrdinal) -> Option<AttributeKind> {
        self.kinds.get(ordinal.index()).copied()
    }

    /// Reports whether this projection requires tagged set-capable storage.
    #[must_use]
    pub fn contains_sets(&self) -> bool {
        self.kinds.contains(&AttributeKind::StringSet)
    }

    /// Returns the physical Vibrato-column adapter projection.
    ///
    /// This is empty for a wholly dynamic projection.
    #[must_use]
    pub fn attributes(&self) -> &[TokenAttributeIndex] {
        &self.attributes
    }

    /// Returns current Vibrato physical attributes in projection order.
    ///
    /// `None` identifies a schema key without a corresponding Vibrato column.
    #[must_use]
    pub fn physical_attributes(&self) -> &[Option<TokenAttributeIndex>] {
        &self.physical_attributes
    }

    /// Returns the dense ordinal for a projected attribute.
    #[must_use]
    pub fn ordinal(&self, attribute: TokenAttributeIndex) -> Option<AttributeOrdinal> {
        self.ordinals[attribute as usize]
    }

    /// Returns the dense ordinal of a symbolic scalar key.
    #[must_use]
    pub fn ordinal_key(&self, key: &str) -> Option<AttributeOrdinal> {
        self.ordinal_by_key
            .iter()
            .find_map(|(candidate, ordinal)| (candidate.as_str() == key).then_some(*ordinal))
    }

    #[must_use]
    pub fn ordinal_for_key(&self, key: &AttributeKey) -> Option<AttributeOrdinal> {
        self.ordinal_by_key.get(key).copied()
    }

    pub(crate) const fn ordinal_table(
        &self,
    ) -> [Option<AttributeOrdinal>; TokenAttributeIndex::COUNT] {
        self.ordinals
    }

    /// Returns the projection width.
    #[must_use]
    pub fn len(&self) -> usize {
        self.keys.len()
    }

    /// Reports whether no attributes are projected.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }
}

fn digest_attributes(attributes: &[SchemaAttribute]) -> TokenSchemaDigest {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"lingrep.token-schema/v1");
    hasher.update(&(attributes.len() as u64).to_le_bytes());
    for attribute in attributes {
        hasher.update(&(attribute.key.as_bytes().len() as u64).to_le_bytes());
        hasher.update(attribute.key.as_bytes());
        hasher.update(&[match attribute.kind {
            AttributeKind::Scalar => 0,
            AttributeKind::StringSet => 1,
        }]);
    }
    TokenSchemaDigest(*hasher.finalize().as_bytes())
}

fn canonical_key_order(left: &AttributeKey, right: &AttributeKey) -> std::cmp::Ordering {
    canonical_key_rank(left)
        .cmp(&canonical_key_rank(right))
        .then_with(|| left.as_bytes().cmp(right.as_bytes()))
}

fn canonical_key_rank(key: &AttributeKey) -> (u8, u8) {
    match key.as_str() {
        "text" => (0, 0),
        "lemma" => (0, 1),
        "norm" => (0, 2),
        "upos" => (0, 3),
        "xpos" => (0, 4),
        value if value.starts_with("feat.") => (1, 0),
        _ => (2, 0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_schema_can_bind_token_dynamic_and_graph_providers() {
        let schema = TokenSchema::new([
            SchemaAttribute::new("text", AttributeKind::Scalar)
                .unwrap()
                .with_provider(AttributeProvider::TokenColumn(TokenAttributeIndex::Orth)),
            SchemaAttribute::new("external.score", AttributeKind::Scalar).unwrap(),
            SchemaAttribute::new("dep.enhanced", AttributeKind::StringSet)
                .unwrap()
                .with_provider(AttributeProvider::EnhancedGraph),
        ])
        .unwrap();
        let projection = schema
            .bind_projection(schema.keys().iter().cloned())
            .unwrap();

        assert_eq!(
            projection.providers(),
            &[
                AttributeProvider::TokenColumn(TokenAttributeIndex::Orth),
                AttributeProvider::EnhancedGraph,
                AttributeProvider::Dynamic,
            ]
        );
        assert_eq!(
            projection.physical_attributes(),
            &[Some(TokenAttributeIndex::Orth), None, None]
        );
    }

    #[test]
    fn provider_choice_does_not_change_logical_schema_identity() {
        let dynamic =
            TokenSchema::new([
                SchemaAttribute::new("dep.enhanced", AttributeKind::StringSet).unwrap(),
            ])
            .unwrap();
        let graph =
            TokenSchema::new([
                SchemaAttribute::new("dep.enhanced", AttributeKind::StringSet)
                    .unwrap()
                    .with_provider(AttributeProvider::EnhancedGraph),
            ])
            .unwrap();

        assert_eq!(dynamic.digest(), graph.digest());
        assert_ne!(dynamic, graph);
    }
}
