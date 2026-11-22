//! Token attributes, schemas, storage, and tokenization.

mod attribute_key;
mod bound;
mod maps;
mod schema;
mod sentence_span;
mod storage;
pub use sentence_span::SentenceSpan;

pub use crate::analysis::vibrato::{featurize, resolve_pos, tokenize, tokenize_bound};
pub use attribute_key::{AttributeKey, AttributeKeyError};
pub use bound::{
    AttributeValueRef, BoundToken, BoundTokenError, BoundTokenSentence, DenseStorageKind,
    ProjectedAttributeValue,
};
pub use maps::{TAG_BIGRAM_MAP, TAG_MAP, TAG_ORTH_MAP};
pub use schema::{
    AttributeKind, AttributeOrdinal, AttributeProvider, BoundProjection, SchemaAttribute,
    TokenSchema, TokenSchemaDigest, TokenSchemaError,
};
#[doc(hidden)]
pub use storage::AttributeLocator;
pub use storage::{CharSpan, SequenceToken, TokenInfoTrait, TokenSequence};

use compact_str::CompactString;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

impl fmt::Display for TokenAttributeIndex {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum TokenAttributeIndex {
    Pos1,
    Pos2,
    Pos3,
    Pos4,
    CType,
    CForm,
    LForm,
    Lemma,
    Orth,
    Pron,
    OrthBase,
    PronBase,
    Goshu,
    IType,
    IForm,
    FType,
    FForm,
    IConType,
    FConType,
    Type,
    Kana,
    KanaBase,
    Form,
    FormBase,
    AType,
    AConType,
    AModType,
    // Derived attributes.
    Pos,
    Tag,
    Norm,
    Surface,
    LexType,
}

impl TokenAttributeIndex {
    /// The number of attributes, and so the width of a [`TokenInfo`].
    pub const COUNT: usize = 32;

    pub const ALL: [Self; Self::COUNT] = [
        Pos1, Pos2, Pos3, Pos4, CType, CForm, LForm, Lemma, Orth, Pron, OrthBase, PronBase, Goshu,
        IType, IForm, FType, FForm, IConType, FConType, Type, Kana, KanaBase, Form, FormBase,
        AType, AConType, AModType, Pos, Tag, Norm, Surface, LexType,
    ];

    pub const fn pattern_key(self) -> &'static str {
        match self {
            Pos1 => "pos1",
            Pos2 => "pos2",
            Pos3 => "pos3",
            Pos4 => "pos4",
            CType => "ctype",
            CForm => "cform",
            LForm => "lform",
            Lemma => "lemma",
            Orth => "orth",
            Pron => "pron",
            OrthBase => "orthbase",
            PronBase => "pronbase",
            Goshu => "goshu",
            IType => "itype",
            IForm => "iform",
            FType => "ftype",
            FForm => "fform",
            IConType => "icontype",
            FConType => "fcontype",
            Type => "type",
            Kana => "kana",
            KanaBase => "kanabase",
            Form => "form",
            FormBase => "formbase",
            AType => "atype",
            AConType => "acontype",
            AModType => "amodtype",
            Pos => "pos",
            Tag => "tag",
            Norm => "norm",
            Surface => "surface",
            LexType => "lextype",
        }
    }

    pub fn from_usize(index: usize) -> Option<Self> {
        Self::ALL.get(index).copied()
    }

    #[doc(hidden)]
    pub fn from_attribute_key(key: &AttributeKey) -> Option<Self> {
        match key.as_str() {
            "text" => return Some(Self::Orth),
            "lemma" => return Some(Self::Lemma),
            "norm" => return Some(Self::Norm),
            "upos" => return Some(Self::Pos),
            "xpos" => return Some(Self::Tag),
            "unidic.form" => return Some(Self::Form),
            _ => {}
        }
        let unprefixed = key
            .as_str()
            .strip_prefix("unidic.")
            .unwrap_or_else(|| key.as_str());
        Self::ALL
            .into_iter()
            .find(|attribute| attribute.pattern_key() == unprefixed)
    }
}

impl From<TokenAttributeIndex> for AttributeKey {
    fn from(attribute: TokenAttributeIndex) -> Self {
        let key = match attribute {
            TokenAttributeIndex::Orth => "text".to_owned(),
            TokenAttributeIndex::Lemma => "lemma".to_owned(),
            TokenAttributeIndex::Norm => "norm".to_owned(),
            TokenAttributeIndex::Pos => "upos".to_owned(),
            TokenAttributeIndex::Tag => "xpos".to_owned(),
            TokenAttributeIndex::Surface => "surface".to_owned(),
            other => format!("unidic.{}", other.pattern_key()),
        };
        Self::new(key).expect("physical Vibrato attributes have canonical V1 keys")
    }
}

impl FromStr for TokenAttributeIndex {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "pos1" => Ok(Pos1),
            "pos2" => Ok(Pos2),
            "pos3" => Ok(Pos3),
            "pos4" => Ok(Pos4),
            "ctype" => Ok(CType),
            "cform" => Ok(CForm),
            "lform" => Ok(LForm),
            "lemma" => Ok(Lemma),
            "orth" => Ok(Orth),
            "pron" => Ok(Pron),
            "orthbase" | "orth_base" => Ok(OrthBase),
            "pronbase" | "pron_base" => Ok(PronBase),
            "goshu" => Ok(Goshu),
            "itype" => Ok(IType),
            "iform" => Ok(IForm),
            "ftype" => Ok(FType),
            "fform" => Ok(FForm),
            "icontype" => Ok(IConType),
            "fcontype" => Ok(FConType),
            "type" => Ok(Type),
            "kana" => Ok(Kana),
            "kanabase" => Ok(KanaBase),
            "form" => Ok(Form),
            "formbase" => Ok(FormBase),
            "atype" => Ok(AType),
            "acontype" => Ok(AConType),
            "amodtype" => Ok(AModType),
            "pos" => Ok(Pos),
            "tag" => Ok(Tag),
            "norm" => Ok(Norm),
            "surface" => Ok(Surface),
            "lextype" => Ok(LexType),
            _ => Err(()),
        }
    }
}

use TokenAttributeIndex::*;

/// Serializes as named fields for the physical token attributes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(into = "SerdeTokenInfo", from = "SerdeTokenInfo")]
pub struct TokenInfo {
    values: [Option<CompactString>; TokenAttributeIndex::COUNT],
}

impl Default for TokenInfo {
    /// Spelled as an inline-const repeat rather than `array::from_fn`, and the
    /// difference is measured, not stylistic.
    ///
    /// `from_fn` builds through a closure behind a panic-safety drop guard, and
    /// LLVM then cannot see that every slot is `None`. The decode path allocates
    /// with `vec![TokenInfo::default(); n]`, whose clone of an opaque value
    /// becomes a 32-way branch per token. A const repeat lets the clone fold
    /// through the value.
    fn default() -> Self {
        Self {
            values: [const { None }; TokenAttributeIndex::COUNT],
        }
    }
}

impl storage::TokenInfoTrait for TokenInfo {
    #[inline]
    fn get(&self, index: TokenAttributeIndex) -> Option<&CompactString> {
        self.values[index as usize].as_ref()
    }
}

impl TokenInfo {
    /// Materializes one token from indexed storage at a publication boundary.
    pub fn from_sequence<S: TokenSequence + ?Sized>(sequence: &S, token: usize) -> Option<Self> {
        if token >= sequence.len() {
            return None;
        }
        let mut output = Self::default();
        for attribute in TokenAttributeIndex::ALL {
            if let Some(value) = sequence.get(token, attribute) {
                output.set(attribute, value.clone());
            }
        }
        Some(output)
    }

    /// Returns every attribute in [`TokenAttributeIndex::ALL`] order.
    ///
    /// Residency accounting traverses this on the matching hot path. It is a
    /// borrow rather than a constructed array: building one cost six of the
    /// hottest instructions in `estimate_token_info_bytes`, all of them stack
    /// stores.
    #[inline]
    #[must_use]
    pub(crate) fn attribute_values(&self) -> &[Option<CompactString>; TokenAttributeIndex::COUNT] {
        &self.values
    }

    /// Builds a token from one value per attribute, in
    /// [`TokenAttributeIndex::ALL`] order.
    #[inline]
    #[must_use]
    pub fn from_values(values: [Option<CompactString>; TokenAttributeIndex::COUNT]) -> Self {
        Self { values }
    }

    #[inline]
    pub fn set(&mut self, index: TokenAttributeIndex, value: CompactString) {
        self.values[index as usize] = Some(value);
    }

    /// Removes an attribute, leaving it absent rather than empty.
    #[inline]
    pub fn clear(&mut self, index: TokenAttributeIndex) {
        self.values[index as usize] = None;
    }
}

macro_rules! token_attribute_accessors {
    ($(($field:ident, $setter:ident, $builder:ident) => $variant:ident,)*) => {
        impl TokenInfo {
            $(
                #[doc = concat!("Returns the `", stringify!($field), "` attribute.")]
                #[inline]
                #[must_use]
                pub fn $field(&self) -> Option<&str> {
                    self.values[TokenAttributeIndex::$variant as usize]
                        .as_ref()
                        .map(CompactString::as_str)
                }

                #[doc = concat!("Sets the `", stringify!($field), "` attribute.")]
                #[inline]
                pub fn $setter(&mut self, value: impl Into<CompactString>) {
                    self.values[TokenAttributeIndex::$variant as usize] = Some(value.into());
                }

                #[doc = concat!("Returns this token with `", stringify!($field), "` set.")]
                #[inline]
                #[must_use]
                pub fn $builder(mut self, value: impl Into<CompactString>) -> Self {
                    self.values[TokenAttributeIndex::$variant as usize] = Some(value.into());
                    self
                }
            )*
        }

        #[derive(Debug, Clone, Default, Serialize, Deserialize)]
        struct SerdeTokenInfo {
            $( $field: Option<CompactString>, )*
        }

        impl From<TokenInfo> for SerdeTokenInfo {
            fn from(token: TokenInfo) -> Self {
                let [ $( $field, )* ] = token.values;
                Self { $( $field, )* }
            }
        }

        impl From<SerdeTokenInfo> for TokenInfo {
            fn from(mirror: SerdeTokenInfo) -> Self {
                Self { values: [ $( mirror.$field, )* ] }
            }
        }

        #[cfg(test)]
        const NAMED_ACCESSOR_ORDER: [TokenAttributeIndex; TokenAttributeIndex::COUNT] =
            [ $( TokenAttributeIndex::$variant, )* ];

        #[cfg(test)]
        const NAMED_ACCESSOR_FIELDS: [&str; TokenAttributeIndex::COUNT] =
            [ $( stringify!($field), )* ];
    };
}

token_attribute_accessors! {
    (pos1, set_pos1, with_pos1) => Pos1,
    (pos2, set_pos2, with_pos2) => Pos2,
    (pos3, set_pos3, with_pos3) => Pos3,
    (pos4, set_pos4, with_pos4) => Pos4,
    (ctype, set_ctype, with_ctype) => CType,
    (cform, set_cform, with_cform) => CForm,
    (lform, set_lform, with_lform) => LForm,
    (lemma, set_lemma, with_lemma) => Lemma,
    (orth, set_orth, with_orth) => Orth,
    (pron, set_pron, with_pron) => Pron,
    (orth_base, set_orth_base, with_orth_base) => OrthBase,
    (pron_base, set_pron_base, with_pron_base) => PronBase,
    (goshu, set_goshu, with_goshu) => Goshu,
    (itype, set_itype, with_itype) => IType,
    (iform, set_iform, with_iform) => IForm,
    (ftype, set_ftype, with_ftype) => FType,
    (fform, set_fform, with_fform) => FForm,
    (icon_type, set_icon_type, with_icon_type) => IConType,
    (fcon_type, set_fcon_type, with_fcon_type) => FConType,
    (type_, set_type_, with_type_) => Type,
    (kana, set_kana, with_kana) => Kana,
    (kana_base, set_kana_base, with_kana_base) => KanaBase,
    (form, set_form, with_form) => Form,
    (form_base, set_form_base, with_form_base) => FormBase,
    (atype, set_atype, with_atype) => AType,
    (acon_type, set_acon_type, with_acon_type) => AConType,
    (amod_type, set_amod_type, with_amod_type) => AModType,
    (pos, set_pos, with_pos) => Pos,
    (tag, set_tag, with_tag) => Tag,
    (norm, set_norm, with_norm) => Norm,
    (surface, set_surface, with_surface) => Surface,
    (lex_type, set_lex_type, with_lex_type) => LexType,
}

static SURFACE_KEY: std::sync::LazyLock<AttributeKey> =
    std::sync::LazyLock::new(|| AttributeKey::new("surface").expect("surface is canonical"));
static TEXT_KEY: std::sync::LazyLock<AttributeKey> =
    std::sync::LazyLock::new(|| AttributeKey::new("text").expect("text is canonical"));

/// The token's text as output displays it: `surface` when the token carries
/// one, otherwise `text`.
///
/// `text` can differ from the source (full-width ASCII folds to ASCII), and
/// `surface` is the exact source slice. Schemas without `surface` — 0.8
/// artifacts, Sudachi, external analyzers, CoNLL-U — display `text`.
pub fn display_surface<S: TokenSequence + ?Sized>(tokens: &S, index: usize) -> Option<&str> {
    tokens
        .scalar_key(index, &SURFACE_KEY)
        .or_else(|| tokens.scalar_key(index, &TEXT_KEY))
}

/// The attributes [`display_surface`] reads that `schema` carries, for a
/// caller that binds a projection against a known schema.
#[must_use]
pub fn display_surface_attributes(schema: &TokenSchema) -> Vec<AttributeKey> {
    [&*SURFACE_KEY, &*TEXT_KEY]
        .into_iter()
        .filter(|key| schema.keys().contains(key))
        .cloned()
        .collect()
}

/// Whether adjacent tokens are separated in the source. Returns `None` if either
/// token lacks a character span. Words sharing a multiword-token span touch.
pub fn source_space_before<S: TokenSequence + ?Sized>(tokens: &S, index: usize) -> Option<bool> {
    let left = tokens.char_span(index.checked_sub(1)?)?;
    let right = tokens.char_span(index)?;
    Some(left.end < right.start)
}

/// Whether displayed text has a space between token `index` and the token
/// before it.
///
/// Use the gap between adjacent character spans when both are available.
/// Words that share a multiword-token span have no gap. If either span is
/// missing, infer spacing from the writing systems at the token boundary.
///
/// The first token of a sentence, and an index past its end, have no space
/// before them.
pub fn display_space_before<S: TokenSequence + ?Sized>(tokens: &S, index: usize) -> bool {
    let Some(previous) = index.checked_sub(1) else {
        return false;
    };
    if index >= tokens.len() {
        return false;
    }
    source_space_before(tokens, index).unwrap_or_else(|| {
        crate::utils::spaced_script_boundary(
            display_surface(tokens, previous).unwrap_or(""),
            display_surface(tokens, index).unwrap_or(""),
        )
    })
}

/// Appends the tokens of `span` to `text` as output displays them: each
/// token's [`display_surface`], with one U+0020 wherever
/// [`display_space_before`] reports a space between two tokens of the span.
///
/// The first token receives no leading space. Use [`display_space_before`] to
/// insert a separator when joining adjacent spans. Tokens without displayable
/// text contribute no characters of their own.
pub fn push_display_text<S: TokenSequence + ?Sized>(
    text: &mut String,
    tokens: &S,
    span: std::ops::Range<usize>,
) {
    let end = span.end.min(tokens.len());
    for index in span.start.min(end)..end {
        if index > span.start && display_space_before(tokens, index) {
            text.push(' ');
        }
        if let Some(surface) = display_surface(tokens, index) {
            text.push_str(surface);
        }
    }
}

/// [`push_display_text`] into a new string.
#[must_use]
pub fn display_text<S: TokenSequence + ?Sized>(tokens: &S, span: std::ops::Range<usize>) -> String {
    let mut text = String::new();
    push_display_text(&mut text, tokens, span);
    text
}

/// One sentence of [`TokenInfo`] together with the source spans recorded for
/// its tokens.
///
/// A bare `Vec<TokenInfo>` cannot answer [`TokenSequence::char_span`], so
/// [`display_space_before`] can only guess over one. Dereferences to the
/// token slice.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TokenInfoSentence {
    tokens: Vec<TokenInfo>,
    char_offsets: Option<std::sync::Arc<Vec<CharSpan>>>,
}

impl TokenInfoSentence {
    /// `char_offsets` is kept only when it has one span per token, the same
    /// all-or-nothing rule as [`BoundTokenSentence::with_char_offsets`].
    #[must_use]
    pub fn new(
        tokens: Vec<TokenInfo>,
        char_offsets: Option<std::sync::Arc<Vec<CharSpan>>>,
    ) -> Self {
        let char_offsets = char_offsets.filter(|spans| spans.len() == tokens.len());
        Self {
            tokens,
            char_offsets,
        }
    }
}

impl TokenInfoSentence {
    /// The tokens alone, without their spans.
    #[must_use]
    pub fn into_tokens(self) -> Vec<TokenInfo> {
        self.tokens
    }
}

impl From<Vec<TokenInfo>> for TokenInfoSentence {
    fn from(tokens: Vec<TokenInfo>) -> Self {
        Self::new(tokens, None)
    }
}

impl std::ops::Deref for TokenInfoSentence {
    type Target = [TokenInfo];

    fn deref(&self) -> &[TokenInfo] {
        &self.tokens
    }
}

impl TokenSequence for TokenInfoSentence {
    fn len(&self) -> usize {
        self.tokens.len()
    }

    fn char_span(&self, token: usize) -> Option<CharSpan> {
        self.char_offsets.as_ref()?.get(token).copied()
    }

    fn get(&self, token: usize, attribute: TokenAttributeIndex) -> Option<&CompactString> {
        TokenInfoTrait::get(<[TokenInfo]>::get(&self.tokens, token)?, attribute)
    }
}

/// Construct a token from a surface string, setting `orth` and leaving all
/// morphological attributes `None`.
impl From<String> for TokenInfo {
    fn from(surface: String) -> Self {
        Self::default().with_orth(surface)
    }
}

#[cfg(test)]
mod tests {
    use super::{TokenAttributeIndex, TokenInfo};
    use crate::token_match::TokenInfoTrait;

    use crate::testing::helpers::placed_tokens as placed;

    #[test]
    fn display_text_keeps_the_spaces_the_source_has_between_tokens() {
        let english = placed("I saw NHK news", &["I", "saw", "NHK", "news"]);
        assert_eq!(super::display_text(&english, 0..4), "I saw NHK news");
        assert_eq!(super::display_text(&english, 1..3), "saw NHK");
        assert!(super::display_space_before(&english, 1));
        assert!(!super::display_space_before(&english, 0));
        assert!(!super::display_space_before(&english, 4));

        let mixed = placed(
            "昨日 NHK news を見た。",
            &[
                "昨日", "N", "H", "K", "n", "e", "w", "s", "を", "見", "た", "。",
            ],
        );
        assert_eq!(super::display_text(&mixed, 0..12), "昨日 NHK news を見た。");

        let japanese = placed(
            "昨日はニュースを見た。",
            &["昨日", "は", "ニュース", "を", "見", "た", "。"],
        );
        assert_eq!(
            super::display_text(&japanese, 0..7),
            "昨日はニュースを見た。"
        );
        assert_eq!(super::display_text(&japanese, 2..4), "ニュースを");
    }

    #[test]
    fn display_text_shows_any_gap_as_one_space_and_none_inside_a_multiword_token() {
        let wide = placed("a \t b", &["a", "b"]);
        assert_eq!(super::display_text(&wide, 0..2), "a b");

        // Both words of a CoNLL-U multiword token carry the token's span.
        let shared = super::TokenInfoSentence::new(
            ["vamos", "nos", "ya"]
                .map(|word| TokenInfo::default().with_orth(word))
                .to_vec(),
            Some(std::sync::Arc::new(vec![
                super::CharSpan::new(0, 7),
                super::CharSpan::new(0, 7),
                super::CharSpan::new(8, 10),
            ])),
        );
        assert_eq!(super::display_text(&shared, 0..3), "vamosnos ya");
    }

    #[test]
    fn display_text_without_spans_guesses_from_the_script() {
        let words = ["前", "DNA", "machine", "learning", "解析"]
            .map(|word| TokenInfo::default().with_orth(word));
        assert_eq!(
            super::display_text(&words, 0..5),
            "前DNA machine learning解析"
        );
        assert_eq!(
            super::display_text(&words, 0..9),
            "前DNA machine learning解析"
        );
    }

    #[test]
    fn physical_token_size_matches_the_attribute_count() {
        assert_eq!(size_of::<TokenInfo>(), 768);
        assert_eq!(size_of::<Option<compact_str::CompactString>>(), 24);
        assert_eq!(TokenAttributeIndex::ALL.len(), 32);
    }

    /// Every attribute round-trips through the indexed accessors, and setting
    /// one attribute disturbs no other. Written against the 32-field
    /// representation so it pins behavior across the move to an array.
    #[test]
    fn every_attribute_round_trips_through_get_and_set() {
        for (index, attribute) in TokenAttributeIndex::ALL.into_iter().enumerate() {
            let mut token = TokenInfo::default();
            assert_eq!(token.get(attribute), None, "{attribute:?} starts unset");

            let value = format!("value-{index}");
            token.set(attribute, value.as_str().into());
            assert_eq!(
                token.get(attribute).map(compact_str::CompactString::as_str),
                Some(value.as_str()),
                "{attribute:?} reads back what was written"
            );

            for other in TokenAttributeIndex::ALL {
                if other != attribute {
                    assert_eq!(
                        token.get(other),
                        None,
                        "{other:?} disturbed by {attribute:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn token_info_named_accessors_agree_with_the_indexed_schema() {
        assert_eq!(super::NAMED_ACCESSOR_ORDER, TokenAttributeIndex::ALL);

        // And each accessor's own name matches its attribute's lingrep patterns key, modulo
        // the underscores Rust spells them with.
        for (field, attribute) in super::NAMED_ACCESSOR_FIELDS
            .into_iter()
            .zip(TokenAttributeIndex::ALL)
        {
            let normalized = field.replace('_', "");
            assert_eq!(
                normalized,
                attribute.pattern_key(),
                "accessor `{field}` does not name {attribute:?}"
            );
        }
    }

    /// Overwriting replaces rather than accumulating.
    #[test]
    fn setting_an_attribute_twice_keeps_the_last_value() {
        let mut token = TokenInfo::default();
        token.set(TokenAttributeIndex::Orth, "first".into());
        token.set(TokenAttributeIndex::Orth, "second".into());

        assert_eq!(
            token
                .get(TokenAttributeIndex::Orth)
                .map(compact_str::CompactString::as_str),
            Some("second")
        );
    }

    /// The residency estimate for a fixed sentence is an artifact-visible
    /// number. This pins it exactly, so a representation change that moved it
    /// could not pass unnoticed.
    #[test]
    fn residency_estimate_for_a_fixed_sentence_is_unchanged() {
        use crate::execution::estimate_token_info_bytes;

        let mut first = TokenInfo::default();
        first.set(TokenAttributeIndex::Orth, "ある".into());
        first.set(TokenAttributeIndex::Lemma, "有る".into());
        let mut second = TokenInfo::default();
        second.set(TokenAttributeIndex::Orth, "そう".into());

        let sentence = vec![first, second];

        // Two tokens of inline storage; every value here is short enough to
        // live inline in its `CompactString`, so no heap capacity is counted.
        assert_eq!(estimate_token_info_bytes(&sentence).unwrap(), 2 * 768);
    }

    #[test]
    fn base_form_underscore_aliases_match_canonical_names() {
        assert_eq!(
            "orthbase".parse::<TokenAttributeIndex>(),
            "orth_base".parse::<TokenAttributeIndex>()
        );
        assert_eq!(
            "pronbase".parse::<TokenAttributeIndex>(),
            "pron_base".parse::<TokenAttributeIndex>()
        );
    }

    #[test]
    fn token_info_attribute_values_cover_the_indexed_schema_in_order() {
        let mut token = TokenInfo::default();
        for (position, index) in TokenAttributeIndex::ALL.into_iter().enumerate() {
            token.set(index, format!("attribute-{position}").into());
        }

        let direct = token.attribute_values();
        assert_eq!(direct.len(), TokenAttributeIndex::ALL.len());
        for ((actual, index), position) in direct
            .iter()
            .map(Option::as_ref)
            .zip(TokenAttributeIndex::ALL)
            .zip(0usize..)
        {
            assert_eq!(
                actual,
                token.get(index),
                "direct attribute {position} drifted from {index:?}"
            );
        }
    }
}
