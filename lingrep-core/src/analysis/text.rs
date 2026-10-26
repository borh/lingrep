use std::{fmt, ops::Range};

use unicode_normalization::UnicodeNormalization;

use super::{AnalysisError, TextUnitProvenance};

/// Stable identity of one bounded input unit within an admitted run.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct InputUnitId(u64);

impl InputUnitId {
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Whether an admitted unit is already a sentence or awaits segmentation.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum TextUnitKind {
    Sentence,
    Segment,
}

/// Source-changing normalization performed before analysis.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
#[repr(u8)]
pub enum NormalizationPolicy {
    #[default]
    None = 0,
    Nfc = 1,
}

impl NormalizationPolicy {
    /// Unicode table version used by this policy, absent when text is unchanged.
    #[must_use]
    pub const fn unicode_version(self) -> Option<[u8; 3]> {
        match self {
            Self::None => None,
            Self::Nfc => Some([
                unicode_normalization::UNICODE_VERSION.0,
                unicode_normalization::UNICODE_VERSION.1,
                unicode_normalization::UNICODE_VERSION.2,
            ]),
        }
    }

    /// Identity encoding of the policy and its Unicode tables.
    #[must_use]
    pub const fn identity_bytes(self) -> &'static [u8] {
        match self {
            Self::None => &[0],
            Self::Nfc => &[
                1,
                unicode_normalization::UNICODE_VERSION.0,
                unicode_normalization::UNICODE_VERSION.1,
                unicode_normalization::UNICODE_VERSION.2,
            ],
        }
    }

    /// Applies this policy to admitted text.
    ///
    /// The single definition of what a policy *does*. [`TextUnit::admit`] calls
    /// it, and so does anything that has to decide whether some other
    /// derivation of the same source agrees — see [`Self::normalizes`].
    #[must_use]
    pub fn apply(self, text: String) -> String {
        match self {
            Self::None => text,
            Self::Nfc => text.nfc().collect(),
        }
    }

    /// Whether `normalized` is exactly what this policy makes of `raw`.
    ///
    /// Exists because a component can hold the *raw* admitted bytes and be
    /// handed the *analyzed* text, and asking whether they are the same
    /// sentence is then a question about this policy rather than about string
    /// equality. Comparing them directly is what made a `nfc` build refuse any
    /// corpus that was not already NFC.
    ///
    /// Allocation-free: a `char` sequence determines a UTF-8 string, so the
    /// comparison streams rather than materializing the normalized form.
    #[must_use]
    pub fn normalizes(self, raw: &str, normalized: &str) -> bool {
        match self {
            Self::None => raw == normalized,
            Self::Nfc => raw.nfc().eq(normalized.chars()),
        }
    }
}

/// Source annotations are excluded from the admitted-unit identity.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct TextUnitIdentity([u8; 32]);

impl TextUnitIdentity {
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// A validated UTF-8 byte range relative to one admitted unit.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SourceRange {
    start: u32,
    end: u32,
}

impl SourceRange {
    #[must_use]
    pub const fn start(self) -> u32 {
        self.start
    }

    #[must_use]
    pub const fn end(self) -> u32 {
        self.end
    }

    #[must_use]
    pub const fn as_usize(self) -> Range<usize> {
        self.start as usize..self.end as usize
    }
}

/// One bounded, normalized text unit admitted for analysis.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TextUnit {
    id: InputUnitId,
    kind: TextUnitKind,
    normalization: NormalizationPolicy,
    identity: TextUnitIdentity,
    provenance: TextUnitProvenance,
    text: String,
    pub(super) annotations: Box<[super::TextAnnotation]>,
}

/// Why admitted text or a source-relative range is invalid.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TextUnitError {
    TooLarge,
    RangeOutOfBounds,
    RangeNotUtf8Boundary,
}

impl TextUnit {
    pub fn admit(
        id: InputUnitId,
        kind: TextUnitKind,
        text: impl Into<String>,
        normalization: NormalizationPolicy,
    ) -> Result<Self, TextUnitError> {
        Self::admit_with_provenance(id, kind, text, normalization, TextUnitProvenance::default())
    }

    pub fn admit_with_provenance(
        id: InputUnitId,
        kind: TextUnitKind,
        text: impl Into<String>,
        normalization: NormalizationPolicy,
        provenance: TextUnitProvenance,
    ) -> Result<Self, TextUnitError> {
        let text = normalization.apply(text.into());
        if u32::try_from(text.len()).is_err() {
            return Err(TextUnitError::TooLarge);
        }
        let identity = identity(id, kind, normalization, &provenance, &text);
        Ok(Self {
            id,
            kind,
            normalization,
            identity,
            provenance,
            text,
            annotations: Box::new([]),
        })
    }

    pub fn sentence(
        id: InputUnitId,
        text: impl Into<String>,
        normalization: NormalizationPolicy,
    ) -> Result<Self, TextUnitError> {
        Self::admit(id, TextUnitKind::Sentence, text, normalization)
    }

    pub fn annotated(
        id: InputUnitId,
        kind: TextUnitKind,
        text: super::AnnotatedText,
        normalization: NormalizationPolicy,
    ) -> Result<Self, AnalysisError> {
        let (text, annotations) = text.normalize(normalization)?.into_parts();
        let mut unit = Self::admit(id, kind, text, normalization)
            .map_err(|_| AnalysisError::InvalidSourceRange)?;
        unit.annotations = annotations;
        Ok(unit)
    }

    pub fn annotations(&self) -> &[super::TextAnnotation] {
        &self.annotations
    }

    #[must_use]
    pub const fn id(&self) -> InputUnitId {
        self.id
    }

    #[must_use]
    pub const fn kind(&self) -> TextUnitKind {
        self.kind
    }

    #[must_use]
    pub const fn normalization(&self) -> NormalizationPolicy {
        self.normalization
    }

    #[must_use]
    pub const fn identity(&self) -> TextUnitIdentity {
        self.identity
    }

    #[must_use]
    pub const fn provenance(&self) -> &TextUnitProvenance {
        &self.provenance
    }

    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.text.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    pub fn source_range(&self, start: usize, end: usize) -> Result<SourceRange, TextUnitError> {
        if start > end || end > self.text.len() {
            return Err(TextUnitError::RangeOutOfBounds);
        }
        if !self.text.is_char_boundary(start) || !self.text.is_char_boundary(end) {
            return Err(TextUnitError::RangeNotUtf8Boundary);
        }
        Ok(SourceRange {
            start: u32::try_from(start).map_err(|_| TextUnitError::TooLarge)?,
            end: u32::try_from(end).map_err(|_| TextUnitError::TooLarge)?,
        })
    }

    pub fn slice(&self, range: SourceRange) -> Result<&str, TextUnitError> {
        self.text
            .get(range.as_usize())
            .ok_or(TextUnitError::RangeNotUtf8Boundary)
    }
}

fn identity(
    id: InputUnitId,
    kind: TextUnitKind,
    normalization: NormalizationPolicy,
    provenance: &TextUnitProvenance,
    text: &str,
) -> TextUnitIdentity {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"lingrep.text-unit/v1");
    hasher.update(&id.get().to_le_bytes());
    hasher.update(&[match kind {
        TextUnitKind::Sentence => 0,
        TextUnitKind::Segment => 1,
    }]);
    hasher.update(normalization.identity_bytes());
    provenance.hash_into(&mut hasher);
    hasher.update(&(text.len() as u64).to_le_bytes());
    hasher.update(text.as_bytes());
    TextUnitIdentity(*hasher.finalize().as_bytes())
}

impl fmt::Display for TextUnitError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::TooLarge => "text unit exceeds the supported byte range",
            Self::RangeOutOfBounds => "source range lies outside its text unit",
            Self::RangeNotUtf8Boundary => "source range is not on UTF-8 boundaries",
        })
    }
}

impl std::error::Error for TextUnitError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn none_preserves_decomposed_bytes_and_nfc_changes_identity() {
        let decomposed = "e\u{301}";
        let none =
            TextUnit::sentence(InputUnitId::new(7), decomposed, NormalizationPolicy::None).unwrap();
        let nfc =
            TextUnit::sentence(InputUnitId::new(7), decomposed, NormalizationPolicy::Nfc).unwrap();

        assert_eq!(none.text().as_bytes(), decomposed.as_bytes());
        assert_eq!(nfc.text(), "é");
        assert_ne!(none.identity(), nfc.identity());
    }

    #[test]
    fn nfc_reorders_new_unicode18_combining_marks() {
        let input = "\u{315}\u{300}\u{5c8}";
        assert_eq!(
            NormalizationPolicy::Nfc.apply(input.into()),
            "\u{5c8}\u{300}\u{315}"
        );
        assert_eq!(NormalizationPolicy::None.apply(input.into()), input);
    }

    #[test]
    fn source_ranges_address_the_admitted_buffer() {
        let unit =
            TextUnit::sentence(InputUnitId::new(1), "a猫 b", NormalizationPolicy::None).unwrap();
        let range = unit.source_range(1, 4).unwrap();

        assert_eq!(unit.slice(range), Ok("猫"));
        assert_eq!(
            unit.source_range(2, 4),
            Err(TextUnitError::RangeNotUtf8Boundary)
        );
    }

    #[test]
    fn kind_and_input_identity_are_semantic() {
        let sentence = TextUnit::admit(
            InputUnitId::new(1),
            TextUnitKind::Sentence,
            "same",
            NormalizationPolicy::None,
        )
        .unwrap();
        let segment = TextUnit::admit(
            InputUnitId::new(1),
            TextUnitKind::Segment,
            "same",
            NormalizationPolicy::None,
        )
        .unwrap();
        let other =
            TextUnit::sentence(InputUnitId::new(2), "same", NormalizationPolicy::None).unwrap();

        assert_ne!(sentence.identity(), segment.identity());
        assert_ne!(sentence.identity(), other.identity());
    }

    #[test]
    fn parent_source_document_paragraph_and_metadata_are_semantic() {
        let first = TextUnit::admit_with_provenance(
            InputUnitId::new(1),
            TextUnitKind::Sentence,
            "same",
            NormalizationPolicy::None,
            TextUnitProvenance::new([1; 32], 7, Some(2), [3; 32]),
        )
        .unwrap();
        let second = TextUnit::admit_with_provenance(
            InputUnitId::new(1),
            TextUnitKind::Sentence,
            "same",
            NormalizationPolicy::None,
            TextUnitProvenance::new([1; 32], 7, Some(3), [3; 32]),
        )
        .unwrap();

        assert_ne!(first.identity(), second.identity());
        assert_ne!(first.provenance(), second.provenance());
    }

    /// `normalizes` has to agree with `apply` on every input, because the two
    /// answer the same question from opposite sides: one derives the analyzed
    /// text, the other decides whether some other component's copy of the raw
    /// bytes describes that same text. A build compares across that seam, and
    /// comparing raw to analyzed *directly* is the defect this pair replaced.
    #[test]
    fn a_policy_recognizes_exactly_the_text_it_produces() {
        // U+304B U+3099 is the decomposed spelling of U+304C.
        for raw in ["か\u{3099}まん。", "がまん。", "", "abc", "\u{2460}"] {
            for policy in [NormalizationPolicy::None, NormalizationPolicy::Nfc] {
                let applied = policy.apply(raw.to_owned());
                assert!(
                    policy.normalizes(raw, &applied),
                    "{policy:?} does not recognize what it produced from {raw:?}"
                );
            }
        }

        let decomposed = "か\u{3099}まん。";
        let composed = "がまん。";
        assert_eq!(
            NormalizationPolicy::Nfc.apply(decomposed.to_owned()),
            composed
        );
        assert!(NormalizationPolicy::Nfc.normalizes(decomposed, composed));
        assert!(
            !NormalizationPolicy::None.normalizes(decomposed, composed),
            "`none` normalizes nothing, so the two spellings stay distinct"
        );
        assert!(
            !NormalizationPolicy::Nfc.normalizes(decomposed, "ちがう。"),
            "recomposition is not a licence to accept a different sentence"
        );
    }
}
