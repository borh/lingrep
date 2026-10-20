use compact_str::CompactString;
use std::fmt;
use std::sync::Arc;

use super::{
    AttributeKey, AttributeKind, AttributeOrdinal, BoundProjection, CharSpan, TokenInfo,
    TokenInfoTrait, TokenSchema, TokenSchemaDigest, TokenSequence,
};

/// Owned input cell for constructing one projected dense sentence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProjectedAttributeValue {
    Scalar(Option<CompactString>),
    StringSet(Vec<CompactString>),
}

impl ProjectedAttributeValue {
    #[must_use]
    pub fn scalar(value: impl Into<CompactString>) -> Self {
        Self::Scalar(Some(value.into()))
    }

    #[must_use]
    pub const fn missing_scalar() -> Self {
        Self::Scalar(None)
    }

    #[must_use]
    pub fn string_set<I, S>(values: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<CompactString>,
    {
        Self::StringSet(values.into_iter().map(Into::into).collect())
    }
}

/// Borrowed schema-typed value from one projected row.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AttributeValueRef<'a> {
    Scalar(&'a str),
    StringSet(&'a [CompactString]),
}

/// Which spike-selected dense layout owns one sentence.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DenseStorageKind {
    Scalars,
    Mixed,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum DenseValues {
    Scalars(Box<[Option<CompactString>]>),
    Mixed(Box<[AttributeCell]>),
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum AttributeCell {
    Scalar(Option<CompactString>),
    StringSet(Box<[CompactString]>),
}

/// A bounded row-major dense projection of one tokenized sentence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BoundTokenSentence {
    schema_digest: TokenSchemaDigest,
    keys: Arc<[AttributeKey]>,
    width: u8,
    token_count: usize,
    ordinals: [Option<AttributeOrdinal>; super::TokenAttributeIndex::COUNT],
    values: DenseValues,
    /// Each token's code-point span within its sentence, when the source
    /// supplies them.
    ///
    /// Carried beside the values rather than among them because an offset is
    /// not an attribute: it is absent from the token schema, so it never
    /// reaches the schema digest and therefore never moves analysis identity.
    /// That is the whole reason offsets can ship without renaming every
    /// prepared artifact.
    ///
    /// Shared rather than cloned: a reprojection copies the values and keeps
    /// the same offsets, which do not depend on which attributes were selected.
    ///
    /// `Arc<Vec<_>>` rather than `Arc<[_]>` so the field is one thin pointer
    /// instead of a fat one. Eight bytes matters here because this type is
    /// carried inside `AdmittedCorpusEvent::Sentence`, whose size clippy
    /// bounds against its sibling variants.
    char_offsets: Option<Arc<Vec<CharSpan>>>,
}

/// One token-sized row borrowed from a [`BoundTokenSentence`].
#[derive(Clone, Copy, Debug)]
pub struct BoundToken<'a> {
    ordinals: &'a [Option<AttributeOrdinal>; super::TokenAttributeIndex::COUNT],
    values: DenseValuesRef<'a>,
}

#[derive(Clone, Copy, Debug)]
enum DenseValuesRef<'a> {
    Scalars(&'a [Option<CompactString>]),
    Mixed(&'a [AttributeCell]),
}

/// Why a dense sentence could not be projected or accounted.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BoundTokenError {
    SchemaMismatch {
        expected: TokenSchemaDigest,
        actual: TokenSchemaDigest,
    },
    KindMismatch {
        ordinal: AttributeOrdinal,
        expected: AttributeKind,
        actual: AttributeKind,
    },
    ResourceAccounting,
}

impl fmt::Display for BoundTokenError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SchemaMismatch { .. } => {
                formatter.write_str("bound projection schema does not match the token schema")
            }
            Self::KindMismatch { .. } => {
                formatter.write_str("projected value kind does not match its token schema")
            }
            Self::ResourceAccounting => {
                formatter.write_str("bound token residency accounting overflow")
            }
        }
    }
}

impl std::error::Error for BoundTokenError {}

impl BoundTokenSentence {
    /// Attaches each token's sentence-relative code-point span.
    ///
    /// The caller owns the invariant that there is exactly one span per token
    /// and that they address this sentence's own text; a source that cannot
    /// establish both passes `None` rather than a partial vector, because a
    /// half-populated offset table would report positions for some tokens and
    /// silently omit others.
    #[must_use]
    pub fn with_char_offsets(mut self, offsets: Option<Arc<Vec<CharSpan>>>) -> Self {
        self.char_offsets = offsets.filter(|spans| spans.len() == self.token_count);
        self
    }

    /// Reprojects one sentence within the same source schema.
    pub fn reproject(&self, projection: &BoundProjection) -> Result<Self, BoundTokenError> {
        if self.schema_digest != projection.schema_digest() {
            return Err(BoundTokenError::SchemaMismatch {
                expected: projection.schema_digest(),
                actual: self.schema_digest,
            });
        }
        let mut values = Vec::with_capacity(self.token_count * projection.len());
        for token in 0..self.token_count {
            for key in projection.keys() {
                let value = match TokenSequence::value_key(self, token, key) {
                    Some(AttributeValueRef::Scalar(value)) => {
                        ProjectedAttributeValue::scalar(value)
                    }
                    Some(AttributeValueRef::StringSet(value)) => {
                        ProjectedAttributeValue::string_set(value.iter().cloned())
                    }
                    None => match projection
                        .kind(
                            projection
                                .ordinal_for_key(key)
                                .expect("projected key has an ordinal"),
                        )
                        .expect("projected ordinal has a kind")
                    {
                        AttributeKind::Scalar => ProjectedAttributeValue::missing_scalar(),
                        AttributeKind::StringSet => {
                            ProjectedAttributeValue::string_set(std::iter::empty::<CompactString>())
                        }
                    },
                };
                values.push(value);
            }
        }
        Self::from_projected_attribute_values(projection, self.token_count, values)
            .map(|sentence| sentence.with_char_offsets(self.char_offsets.clone()))
    }

    /// Projects a sentence onto a compatible subset schema.
    ///
    /// Unlike [`Self::reproject`], this is an intentional schema-identity
    /// transition. Every target attribute must exist with the same kind in
    /// `source_schema`; the sentence itself must be bound to that source.
    pub fn project_onto_schema(
        &self,
        source_schema: &TokenSchema,
        target: &BoundProjection,
    ) -> Result<Self, BoundTokenError> {
        if self.schema_digest != source_schema.digest() {
            return Err(BoundTokenError::SchemaMismatch {
                expected: source_schema.digest(),
                actual: self.schema_digest,
            });
        }
        for (index, (key, target_kind)) in target.keys().iter().zip(target.kinds()).enumerate() {
            let Some(source_kind) = source_schema.kind_for_key(key) else {
                return Err(BoundTokenError::SchemaMismatch {
                    expected: target.schema_digest(),
                    actual: self.schema_digest,
                });
            };
            if source_kind != *target_kind {
                return Err(BoundTokenError::KindMismatch {
                    ordinal: AttributeOrdinal::new(index as u8),
                    expected: *target_kind,
                    actual: source_kind,
                });
            }
        }

        let mut values = Vec::with_capacity(self.token_count * target.len());
        for token in 0..self.token_count {
            for (key, kind) in target.keys().iter().zip(target.kinds()) {
                let value = match TokenSequence::value_key(self, token, key) {
                    Some(AttributeValueRef::Scalar(value)) => {
                        ProjectedAttributeValue::scalar(value)
                    }
                    Some(AttributeValueRef::StringSet(value)) => {
                        ProjectedAttributeValue::string_set(value.iter().cloned())
                    }
                    None => match kind {
                        AttributeKind::Scalar => ProjectedAttributeValue::missing_scalar(),
                        AttributeKind::StringSet => {
                            ProjectedAttributeValue::string_set(std::iter::empty::<CompactString>())
                        }
                    },
                };
                values.push(value);
            }
        }
        Self::from_projected_attribute_values(target, self.token_count, values)
            .map(|sentence| sentence.with_char_offsets(self.char_offsets.clone()))
    }

    /// Constructs a sentence from already projected row-major values.
    pub fn from_projected_values(
        projection: &BoundProjection,
        token_count: usize,
        values: Vec<Option<CompactString>>,
    ) -> Result<Self, BoundTokenError> {
        let expected = token_count
            .checked_mul(projection.len())
            .ok_or(BoundTokenError::ResourceAccounting)?;
        if values.len() != expected {
            return Err(BoundTokenError::ResourceAccounting);
        }
        if let Some((index, _)) = projection
            .kinds()
            .iter()
            .enumerate()
            .find(|(_, kind)| **kind != AttributeKind::Scalar)
        {
            return Err(BoundTokenError::KindMismatch {
                ordinal: AttributeOrdinal::new(index as u8),
                expected: AttributeKind::StringSet,
                actual: AttributeKind::Scalar,
            });
        }
        Self::finish(
            projection,
            token_count,
            DenseValues::Scalars(values.into_boxed_slice()),
        )
    }

    /// Constructs a sentence from schema-typed row-major values.
    pub fn from_projected_attribute_values(
        projection: &BoundProjection,
        token_count: usize,
        values: Vec<ProjectedAttributeValue>,
    ) -> Result<Self, BoundTokenError> {
        let expected = token_count
            .checked_mul(projection.len())
            .ok_or(BoundTokenError::ResourceAccounting)?;
        if values.len() != expected {
            return Err(BoundTokenError::ResourceAccounting);
        }
        if !projection.contains_sets() {
            let scalars = values
                .into_iter()
                .enumerate()
                .map(|(index, value)| match value {
                    ProjectedAttributeValue::Scalar(value) => Ok(value),
                    ProjectedAttributeValue::StringSet(_) => Err(BoundTokenError::KindMismatch {
                        ordinal: AttributeOrdinal::new((index % projection.len()) as u8),
                        expected: AttributeKind::Scalar,
                        actual: AttributeKind::StringSet,
                    }),
                })
                .collect::<Result<Vec<_>, _>>()?;
            return Self::finish(
                projection,
                token_count,
                DenseValues::Scalars(scalars.into_boxed_slice()),
            );
        }

        let mut cells = Vec::with_capacity(values.len());
        for (index, value) in values.into_iter().enumerate() {
            let ordinal = AttributeOrdinal::new((index % projection.len()) as u8);
            let expected = projection
                .kind(ordinal)
                .ok_or(BoundTokenError::ResourceAccounting)?;
            let cell = match (expected, value) {
                (AttributeKind::Scalar, ProjectedAttributeValue::Scalar(value)) => {
                    AttributeCell::Scalar(value)
                }
                (AttributeKind::StringSet, ProjectedAttributeValue::StringSet(mut values)) => {
                    values.sort_unstable_by(|left, right| left.as_bytes().cmp(right.as_bytes()));
                    values.dedup();
                    AttributeCell::StringSet(values.into_boxed_slice())
                }
                (expected, ProjectedAttributeValue::Scalar(_)) => {
                    return Err(BoundTokenError::KindMismatch {
                        ordinal,
                        expected,
                        actual: AttributeKind::Scalar,
                    });
                }
                (expected, ProjectedAttributeValue::StringSet(_)) => {
                    return Err(BoundTokenError::KindMismatch {
                        ordinal,
                        expected,
                        actual: AttributeKind::StringSet,
                    });
                }
            };
            cells.push(cell);
        }
        Self::finish(
            projection,
            token_count,
            DenseValues::Mixed(cells.into_boxed_slice()),
        )
    }

    fn finish(
        projection: &BoundProjection,
        token_count: usize,
        values: DenseValues,
    ) -> Result<Self, BoundTokenError> {
        Ok(Self {
            schema_digest: projection.schema_digest(),
            keys: projection.shared_keys(),
            width: u8::try_from(projection.len())
                .map_err(|_| BoundTokenError::ResourceAccounting)?,
            token_count,
            ordinals: projection.ordinal_table(),
            values,
            char_offsets: None,
        })
    }

    /// Projects current full tokens into one row-major backing allocation.
    pub fn project(
        projection: &BoundProjection,
        tokens: &[TokenInfo],
    ) -> Result<Self, BoundTokenError> {
        let width = projection.len();
        let value_count = tokens
            .len()
            .checked_mul(width)
            .ok_or(BoundTokenError::ResourceAccounting)?;
        let mut values = Vec::with_capacity(value_count);
        for token in tokens {
            values.extend(
                projection
                    .attributes()
                    .iter()
                    .map(|attribute| token.get(*attribute).cloned()),
            );
        }
        debug_assert_eq!(values.len(), value_count);
        Self::from_projected_values(projection, tokens.len(), values)
    }

    /// Projects tokens after checking that the caller's schema owns the projection.
    pub fn project_for_schema(
        schema: &TokenSchema,
        projection: &BoundProjection,
        tokens: &[TokenInfo],
    ) -> Result<Self, BoundTokenError> {
        if schema.digest() != projection.schema_digest() {
            return Err(BoundTokenError::SchemaMismatch {
                expected: projection.schema_digest(),
                actual: schema.digest(),
            });
        }
        Self::project(projection, tokens)
    }

    /// Returns the schema digest against which values were projected.
    #[must_use]
    pub const fn schema_digest(&self) -> TokenSchemaDigest {
        self.schema_digest
    }

    /// Returns the number of tokens.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.token_count
    }

    /// Reports whether the sentence has no tokens.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.token_count == 0
    }

    /// Returns the number of projected values per token.
    #[must_use]
    pub const fn width(&self) -> usize {
        self.width as usize
    }

    /// Returns the number of value slots in the single backing allocation.
    #[must_use]
    pub const fn backing_len(&self) -> usize {
        match &self.values {
            DenseValues::Scalars(values) => values.len(),
            DenseValues::Mixed(values) => values.len(),
        }
    }

    /// Reports the schema-selected sentence layout.
    #[must_use]
    pub const fn storage_kind(&self) -> DenseStorageKind {
        match self.values {
            DenseValues::Scalars(_) => DenseStorageKind::Scalars,
            DenseValues::Mixed(_) => DenseStorageKind::Mixed,
        }
    }

    /// Borrows one projected token row.
    #[must_use]
    pub fn token(&self, index: usize) -> Option<BoundToken<'_>> {
        if index >= self.token_count {
            return None;
        }
        let width = self.width();
        let start = index * width;
        Some(BoundToken {
            ordinals: &self.ordinals,
            values: match &self.values {
                DenseValues::Scalars(values) => {
                    DenseValuesRef::Scalars(&values[start..start + width])
                }
                DenseValues::Mixed(values) => DenseValuesRef::Mixed(&values[start..start + width]),
            },
        })
    }

    /// Estimates owned live bytes for resource accounting.
    pub fn resident_bytes(&self) -> Result<usize, BoundTokenError> {
        let mut bytes = match &self.values {
            DenseValues::Scalars(values) => std::mem::size_of_val(values.as_ref()),
            DenseValues::Mixed(values) => std::mem::size_of_val(values.as_ref()),
        };
        match &self.values {
            DenseValues::Scalars(values) => {
                for value in values.iter().flatten() {
                    bytes = add_string_capacity(bytes, value)?;
                }
            }
            DenseValues::Mixed(values) => {
                for value in values {
                    match value {
                        AttributeCell::Scalar(Some(value)) => {
                            bytes = add_string_capacity(bytes, value)?;
                        }
                        AttributeCell::Scalar(None) => {}
                        AttributeCell::StringSet(set) => {
                            bytes = bytes
                                .checked_add(std::mem::size_of_val(set.as_ref()))
                                .ok_or(BoundTokenError::ResourceAccounting)?;
                            for member in set {
                                bytes = add_string_capacity(bytes, member)?;
                            }
                        }
                    }
                }
            }
        }
        if let Some(offsets) = &self.char_offsets {
            bytes = bytes
                .checked_add(offsets.capacity() * std::mem::size_of::<CharSpan>())
                .ok_or(BoundTokenError::ResourceAccounting)?;
        }
        Ok(bytes)
    }
}

impl TokenInfoTrait for BoundToken<'_> {
    fn get(&self, index: super::TokenAttributeIndex) -> Option<&CompactString> {
        let ordinal = self.ordinals[index as usize]?;
        self.scalar(ordinal)
    }
}

impl super::TokenSequence for BoundTokenSentence {
    fn len(&self) -> usize {
        self.len()
    }

    fn char_span(&self, token: usize) -> Option<super::CharSpan> {
        self.char_offsets.as_ref()?.get(token).copied()
    }

    fn get(&self, token: usize, attribute: super::TokenAttributeIndex) -> Option<&CompactString> {
        let ordinal = self.ordinals[attribute as usize]?;
        let index = token
            .checked_mul(self.width())?
            .checked_add(ordinal.index())?;
        match &self.values {
            DenseValues::Scalars(values) => values.get(index)?.as_ref(),
            DenseValues::Mixed(values) => match values.get(index)? {
                AttributeCell::Scalar(value) => value.as_ref(),
                AttributeCell::StringSet(_) => None,
            },
        }
    }

    fn get_key(&self, token: usize, key: &AttributeKey) -> Option<&CompactString> {
        let ordinal = self
            .keys
            .iter()
            .position(|candidate| candidate == key)
            .and_then(|index| u8::try_from(index).ok())
            .map(AttributeOrdinal::new)?;
        self.token(token)?.scalar_compact(ordinal)
    }

    fn value_key(&self, token: usize, key: &AttributeKey) -> Option<AttributeValueRef<'_>> {
        let ordinal = self
            .keys
            .iter()
            .position(|candidate| candidate == key)
            .and_then(|index| u8::try_from(index).ok())
            .map(AttributeOrdinal::new)?;
        self.token(token)?.value(ordinal)
    }
}

impl<'a> BoundToken<'a> {
    /// Reads one schema-bound attribute ordinal.
    #[must_use]
    pub fn get(self, ordinal: AttributeOrdinal) -> Option<&'a str> {
        self.scalar(ordinal).map(CompactString::as_str)
    }

    /// Reads one schema-bound typed value.
    #[must_use]
    pub fn value(self, ordinal: AttributeOrdinal) -> Option<AttributeValueRef<'a>> {
        match self.values {
            DenseValuesRef::Scalars(values) => values
                .get(ordinal.index())?
                .as_deref()
                .map(AttributeValueRef::Scalar),
            DenseValuesRef::Mixed(values) => match values.get(ordinal.index())? {
                AttributeCell::Scalar(value) => value.as_deref().map(AttributeValueRef::Scalar),
                AttributeCell::StringSet(values) if values.is_empty() => None,
                AttributeCell::StringSet(values) => Some(AttributeValueRef::StringSet(values)),
            },
        }
    }

    fn scalar(self, ordinal: AttributeOrdinal) -> Option<&'a CompactString> {
        match self.values {
            DenseValuesRef::Scalars(values) => values.get(ordinal.index())?.as_ref(),
            DenseValuesRef::Mixed(values) => match values.get(ordinal.index())? {
                AttributeCell::Scalar(value) => value.as_ref(),
                AttributeCell::StringSet(_) => None,
            },
        }
    }

    fn scalar_compact(self, ordinal: AttributeOrdinal) -> Option<&'a CompactString> {
        self.scalar(ordinal)
    }
}

fn add_string_capacity(bytes: usize, value: &CompactString) -> Result<usize, BoundTokenError> {
    if value.is_heap_allocated() {
        bytes
            .checked_add(value.capacity())
            .ok_or(BoundTokenError::ResourceAccounting)
    } else {
        Ok(bytes)
    }
}

#[cfg(test)]
mod reproject_tests {
    use super::*;
    use crate::token_match::SchemaAttribute;

    #[test]
    fn reproject_preserves_scalar_set_and_missing_values_by_key() {
        let schema = TokenSchema::new([
            SchemaAttribute::new("text", AttributeKind::Scalar).unwrap(),
            SchemaAttribute::new("dep.enhanced", AttributeKind::StringSet).unwrap(),
            SchemaAttribute::new("dep.rel", AttributeKind::Scalar).unwrap(),
        ])
        .unwrap();
        let complete = schema
            .bind_projection(schema.keys().iter().cloned())
            .unwrap();
        let sentence = BoundTokenSentence::from_projected_attribute_values(
            &complete,
            1,
            complete
                .keys()
                .iter()
                .map(|key| match key.as_str() {
                    "text" => ProjectedAttributeValue::scalar("走る"),
                    "dep.enhanced" => ProjectedAttributeValue::string_set(["0:root", "2:conj"]),
                    "dep.rel" => ProjectedAttributeValue::missing_scalar(),
                    unexpected => panic!("unexpected test schema key: {unexpected}"),
                })
                .collect(),
        )
        .unwrap();
        let subset = schema
            .bind_projection([
                AttributeKey::new("dep.rel").unwrap(),
                AttributeKey::new("dep.enhanced").unwrap(),
            ])
            .unwrap();

        let projected = sentence.reproject(&subset).unwrap();
        let token = projected.token(0).unwrap();
        assert_eq!(token.value(subset.ordinal_key("dep.rel").unwrap()), None);
        assert_eq!(
            token.value(subset.ordinal_key("dep.enhanced").unwrap()),
            Some(AttributeValueRef::StringSet(&[
                CompactString::new("0:root"),
                CompactString::new("2:conj"),
            ]))
        );
    }

    #[test]
    fn reproject_rejects_another_schema_even_when_keys_overlap() {
        let source = TokenSchema::from_keys(["text"]).unwrap();
        let source_projection = source
            .bind_projection(source.keys().iter().cloned())
            .unwrap();
        let sentence = BoundTokenSentence::from_projected_values(
            &source_projection,
            1,
            vec![Some("猫".into())],
        )
        .unwrap();
        let other = TokenSchema::from_keys(["text", "lemma"]).unwrap();
        let other_projection = other
            .bind_projection([AttributeKey::new("text").unwrap()])
            .unwrap();

        assert!(matches!(
            sentence.reproject(&other_projection),
            Err(BoundTokenError::SchemaMismatch { .. })
        ));
    }

    #[test]
    fn project_onto_schema_permits_a_compatible_subset_identity() {
        let source = TokenSchema::new([
            SchemaAttribute::new("text", AttributeKind::Scalar).unwrap(),
            SchemaAttribute::new("dep.enhanced", AttributeKind::StringSet).unwrap(),
        ])
        .unwrap();
        let complete = source
            .bind_projection(source.keys().iter().cloned())
            .unwrap();
        let sentence = BoundTokenSentence::from_projected_attribute_values(
            &complete,
            1,
            vec![
                ProjectedAttributeValue::scalar("猫"),
                ProjectedAttributeValue::string_set(["0:root"]),
            ],
        )
        .unwrap()
        .with_char_offsets(Some(Arc::new(vec![CharSpan::new(2, 3)])));
        let target_schema = TokenSchema::from_keys(["text"]).unwrap();
        let target = target_schema
            .bind_projection(target_schema.keys().iter().cloned())
            .unwrap();

        let projected = sentence.project_onto_schema(&source, &target).unwrap();
        assert_eq!(projected.schema_digest(), target_schema.digest());
        assert_eq!(
            TokenSequence::char_span(&projected, 0),
            Some(CharSpan::new(2, 3))
        );
        assert_eq!(
            projected
                .token(0)
                .unwrap()
                .get(target.ordinal_key("text").unwrap()),
            Some("猫")
        );
    }
}
