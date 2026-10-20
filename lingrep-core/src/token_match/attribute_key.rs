use compact_str::CompactString;
use serde::{Deserialize, Serialize};
use std::{fmt, str::FromStr};

/// A case-sensitive symbolic V1 attribute name.
///
/// Attribute keys are analyzer/schema data. Adding a provider key therefore
/// does not require adding a Rust enum variant. The Vibrato adapter maps its
/// fixed physical columns to these values at the storage boundary.
#[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub struct AttributeKey(CompactString);

/// Why a symbolic attribute key is not structurally valid.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AttributeKeyError {
    Empty,
    TooLong,
    EmptySegment,
    InvalidCharacter,
    InvalidNamespace,
}

impl AttributeKey {
    pub const MAX_BYTES: usize = u8::MAX as usize;

    /// Validates and owns one canonical V1 key.
    pub fn new(value: impl AsRef<str>) -> Result<Self, AttributeKeyError> {
        let value = value.as_ref();
        if value.is_empty() {
            return Err(AttributeKeyError::Empty);
        }
        if value.len() > Self::MAX_BYTES {
            return Err(AttributeKeyError::TooLong);
        }
        validate_canonical_key(value)?;
        Ok(Self(CompactString::new(value)))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        self.0.as_bytes()
    }

    /// Returns the source spelling. Kept as a naming bridge for catalog
    /// inspection APIs while callers migrate from the retired physical enum.
    #[must_use]
    pub fn pattern_key(&self) -> &str {
        self.as_str()
    }
}

impl PartialEq<super::TokenAttributeIndex> for AttributeKey {
    fn eq(&self, other: &super::TokenAttributeIndex) -> bool {
        self == &AttributeKey::from(*other)
    }
}

impl PartialEq<AttributeKey> for super::TokenAttributeIndex {
    fn eq(&self, other: &AttributeKey) -> bool {
        other == self
    }
}

impl AsRef<str> for AttributeKey {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl fmt::Display for AttributeKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for AttributeKey {
    type Err = AttributeKeyError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

impl fmt::Display for AttributeKeyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Empty => "attribute keys must not be empty",
            Self::TooLong => "attribute key exceeds 255 UTF-8 bytes",
            Self::EmptySegment => "attribute key contains an empty path segment",
            Self::InvalidCharacter => "attribute key contains an unsupported character",
            Self::InvalidNamespace => {
                "attribute key is not a canonical portable, feature, or provider key"
            }
        })
    }
}

impl std::error::Error for AttributeKeyError {}

fn validate_canonical_key(value: &str) -> Result<(), AttributeKeyError> {
    if matches!(value, "text" | "lemma" | "norm" | "upos" | "xpos") {
        return Ok(());
    }
    if let Some(feature) = value.strip_prefix("feat.") {
        return validate_feature(feature);
    }
    let segments = value.split('.').collect::<Vec<_>>();
    if segments.len() < 2 {
        return Err(AttributeKeyError::InvalidNamespace);
    }
    for segment in segments {
        if segment.is_empty() {
            return Err(AttributeKeyError::EmptySegment);
        }
        if !segment
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        {
            return Err(AttributeKeyError::InvalidCharacter);
        }
    }
    Ok(())
}

fn validate_feature(feature: &str) -> Result<(), AttributeKeyError> {
    let (name, layer) = match feature.split_once('[') {
        Some((name, suffix)) => {
            let layer = suffix
                .strip_suffix(']')
                .ok_or(AttributeKeyError::InvalidCharacter)?;
            if layer.is_empty()
                || layer
                    .bytes()
                    .any(|byte| !byte.is_ascii_lowercase() && !byte.is_ascii_digit())
                || layer.contains(['[', ']'])
            {
                return Err(AttributeKeyError::InvalidCharacter);
            }
            (name, Some(layer))
        }
        None => (feature, None),
    };
    let mut bytes = name.bytes();
    if !bytes.next().is_some_and(|byte| byte.is_ascii_uppercase())
        || bytes.any(|byte| !byte.is_ascii_alphabetic() && !byte.is_ascii_digit())
        || name.contains(['[', ']'])
    {
        return Err(AttributeKeyError::InvalidNamespace);
    }
    let _ = layer;
    Ok(())
}
