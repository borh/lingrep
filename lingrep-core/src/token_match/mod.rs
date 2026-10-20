//! Token attributes, schemas, storage, and tokenization.

mod attribute_key;
mod bound;
mod maps;
mod schema;
mod storage;

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
use std::borrow::Cow;
use std::fmt;
use std::str::FromStr;
use vibrato_rkyv::Tokenizer;
use vibrato_rkyv::dictionary::LexType;
use vibrato_rkyv::token::Token;
use vibrato_rkyv::tokenizer::worker::Worker;

impl fmt::Display for TokenAttributeIndex {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

pub fn resolve_pos(
    orth: &str,
    tag: &str,
    next_tag: Option<&str>,
) -> (CompactString, Option<CompactString>) {
    if let Some(orth_map) = TAG_ORTH_MAP.get(tag)
        && let Some(&pos) = orth_map.get(orth)
    {
        return (pos.to_string().into(), None);
    }

    if let Some(next_tag) = next_tag
        && let Some(&(current_pos, next_pos)) = TAG_BIGRAM_MAP.get(&(tag, next_tag))
    {
        return (
            current_pos
                .map_or_else(
                    || TAG_MAP.get(tag).unwrap_or(&"").to_string(),
                    std::string::ToString::to_string,
                )
                .into(),
            next_pos.map(|pos| pos.to_string().into()),
        );
    }

    // Without a following token, the potentially dependent verb is independent.
    if next_tag.is_none() && tag == "動詞-非自立可能" {
        return ("VERB".to_string().into(), None);
    }

    (TAG_MAP.get(tag).unwrap_or(&"").to_string().into(), None)
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
    Text,
    LexType,
}

impl TokenAttributeIndex {
    /// The number of attributes, and so the width of a [`TokenInfo`].
    pub const COUNT: usize = 32;

    pub const ALL: [Self; Self::COUNT] = [
        Pos1, Pos2, Pos3, Pos4, CType, CForm, LForm, Lemma, Orth, Pron, OrthBase, PronBase, Goshu,
        IType, IForm, FType, FForm, IConType, FConType, Type, Kana, KanaBase, Form, FormBase,
        AType, AConType, AModType, Pos, Tag, Norm, Text, LexType,
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
            Text => "text",
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
            "text" => Ok(Text),
            "lextype" => Ok(LexType),
            _ => Err(()),
        }
    }
}

use TokenAttributeIndex::*;

/// One value per [`TokenAttributeIndex`], stored in `TokenAttributeIndex::ALL`
/// order.
///
/// An array rather than named fields, so an indexed access on the serial decode
/// path is not a 32-arm jump table; generated accessors keep names and indices
/// in one definition.
///
/// `size_of::<TokenInfo>()` is **768** and must stay there:
/// [`crate::execution::estimate_token_info_bytes`] starts from
/// `size_of_val`, and its result is the residency quantity every V6 artifact
/// recorded in its index. `token_info_occupies_the_size_the_residency_index_was_written_with`
/// is the guard.
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

/// Generates the named accessors, the named builders, and the serde mirror
/// from one list, so field name, builder name and variant cannot drift apart.
///
/// The order of this list **is** `TokenAttributeIndex::ALL` order, and the
/// serde mirror's field order is the wire format `TokenTable` is persisted
/// with. `token_info_named_accessors_agree_with_the_indexed_schema` checks the
/// first; the mirror's field order is checked by the second half of the same
/// test.
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

        /// The persisted shape of [`TokenInfo`].
        ///
        /// `TokenTable` is written to disk with `TokenInfo`'s derived `serde`
        /// representation, so that representation is an on-disk format and
        /// does not follow the in-memory one. This mirror keeps the named
        /// 32-field encoding the array replaced.
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
    (text, set_text, with_text) => Text,
    (lex_type, set_lex_type, with_lex_type) => LexType,
}

/// Construct a token from a surface string, setting `orth` and leaving all
/// morphological attributes `None`.
impl From<String> for TokenInfo {
    fn from(surface: String) -> Self {
        Self::default().with_orth(surface)
    }
}

/// The normalized form of a UniDic lemma — what `norm` means.
///
/// `norm` is the *dictionary's normalized form*. The name is Sudachi's
/// (`normalized_form()`, spaCy's `norm_`); UniDic emits no such feature, so
/// this pipeline derives it. Deriving it as a verbatim copy of `lemma`, which
/// is what revision 1 did, is wrong under UniDic for two reasons:
///
/// * **A UniDic lemma is a lexeme identifier, not a form.** Homographs and
///   loanwords carry a disambiguator after a hyphen — `システム-system`,
///   `我々-代名詞`, `差す-他動詞`. Measured over 500 documents of academic
///   prose, 7.60% of joined lemma values carry one.
/// * **The light verb lemmatizes to 為る**, an uncommon authored orthography.
///   natsume-simple ships the identical mapping as `simple_lemma`.
///
/// Split on the **first** hyphen: `ピックアップ-pick-up` shows the
/// disambiguator can itself contain one, so splitting on the last keeps half
/// of it. An empty head keeps the whole lemma, because a lemma that *is*
/// punctuation (`-`, `--`, `->`) has no head to take — 15 such types occur in
/// the same corpus.
///
/// This changes grouping almost not at all — 3 merges in 29,295 keyed pairs —
/// because a disambiguator is deterministic per lexeme and so has nothing to
/// merge with. It is a presentation rule, and it is applied here rather than
/// at each rendering site so that counting and display cannot disagree.
///
/// Both rules are no-ops where the convention is absent: a lemma without a
/// hyphen and a lexicon without 為る pass through untouched. The rule is
/// nonetheless UniDic-specific. Any output-affecting change must move the
/// applicable Vibrato analyzer policy digest in `analysis::VibratoPlan`, which
/// is what current prepared-corpus identities record.
fn normalized_lemma(lemma: &CompactString) -> CompactString {
    let head = lemma.split('-').next().unwrap_or(lemma.as_str());
    let base = if head.is_empty() {
        lemma.as_str()
    } else {
        head
    };
    if base == "為る" {
        CompactString::const_new("する")
    } else {
        CompactString::from(base)
    }
}

/// Splits one UniDic feature record into its fields.
///
/// The record is CSV, not a comma-joined list: vibrato stores it verbatim from
/// the lexicon, where a field containing a comma or a quote is quoted with `"`
/// and an embedded quote is doubled. UniDic uses this for the ASCII comma's own
/// entry — `…,，,",",*,",",*,記号,…` — and for lexemes such as
/// `1,2-ジクロロエタン`. Splitting on a bare comma turns one such field into
/// two and shifts every column after it, which silently corrupted `orth`,
/// `orthBase`, `goshu` and everything past them.
///
/// Borrowed whenever the record carries no quote at all, which is all but a
/// fraction of a percent of tokens, so the common path stays allocation-free.
fn feature_fields(feature: &str) -> Vec<Cow<'_, str>> {
    if !feature.contains('"') {
        return feature.split(',').map(Cow::Borrowed).collect();
    }
    let mut fields = Vec::new();
    let mut rest = feature;
    loop {
        let (field, remainder) = next_feature_field(rest);
        fields.push(field);
        let Some(remainder) = remainder else {
            return fields;
        };
        rest = remainder;
    }
}

/// Reads one field, returning it and whatever follows its delimiter.
///
/// `None` for the remainder means the record ended. An unterminated quoted
/// field takes the rest of the record rather than failing: a feature string is
/// dictionary data this crate did not write, and refusing to featurize a token
/// over it would be a worse outcome than reading it literally.
fn next_feature_field(rest: &str) -> (Cow<'_, str>, Option<&str>) {
    let Some(body) = rest.strip_prefix('"') else {
        return match rest.split_once(',') {
            Some((field, remainder)) => (Cow::Borrowed(field), Some(remainder)),
            None => (Cow::Borrowed(rest), None),
        };
    };
    let mut value = String::new();
    let mut tail = body;
    let after_quote = loop {
        let Some((chunk, after)) = tail.split_once('"') else {
            value.push_str(tail);
            return (Cow::Owned(value), None);
        };
        value.push_str(chunk);
        match after.strip_prefix('"') {
            // A doubled quote is one literal quote, and the field continues.
            Some(escaped) => {
                value.push('"');
                tail = escaped;
            }
            None => break after,
        }
    };
    (Cow::Owned(value), after_quote.strip_prefix(','))
}

pub fn featurize(token: &Token, next_token: Option<&Token>) -> TokenInfo {
    TokenInfo::from_values(featurize_selected(
        token,
        next_token,
        &[true; TokenAttributeIndex::COUNT],
    ))
}

fn featurize_selected(
    token: &Token,
    next_token: Option<&Token>,
    requested: &[bool; TokenAttributeIndex::COUNT],
) -> [Option<CompactString>; TokenAttributeIndex::COUNT] {
    let mut values: [Option<CompactString>; TokenAttributeIndex::COUNT] =
        [const { None }; TokenAttributeIndex::COUNT];

    let feature_array = feature_fields(token.feature());

    const INDICES: [TokenAttributeIndex; 32] = [
        Pos1, Pos2, Pos3, Pos4, CType, CForm, LForm, Lemma, Orth, Pron, OrthBase, PronBase, Goshu,
        IType, IForm, FType, FForm, IConType, FConType, Type, Kana, KanaBase, Form, FormBase,
        AType, AConType, AModType, Pos, Tag, Norm, Text, LexType,
    ];

    for (i, &index) in INDICES.iter().enumerate() {
        let needed_for_derivation = matches!(index, Pos1 | Pos2 | Pos3 | Pos4 | Lemma | Orth);
        if (requested[index as usize] || needed_for_derivation)
            && let Some(s) = feature_array.get(i)
        {
            values[index as usize] = Some(s.as_ref().into());
        }
    }

    let tag: CompactString = [
        values[Pos1 as usize].as_ref(),
        values[Pos2 as usize].as_ref(),
        values[Pos3 as usize].as_ref(),
        values[Pos4 as usize].as_ref(),
    ]
    .iter()
    .filter_map(|s| s.as_ref())
    .filter(|s| !s.is_empty() && **s != "*")
    .map(|s| CompactString::from(s.as_str()))
    .collect::<Vec<_>>()
    .join("-")
    .into();
    if requested[Tag as usize] {
        values[Tag as usize] = Some(tag.clone());
    }

    let orth = values[Orth as usize]
        .clone()
        .unwrap_or_else(|| CompactString::from(token.surface()));
    if requested[Orth as usize] {
        values[Orth as usize] = Some(orth.clone());
    }
    if requested[Text as usize] {
        values[Text as usize] = Some(CompactString::from(token.surface()));
    }

    // For a token the dictionary does not know, attributes whose value space
    // *is* the surface orthography fall back to the surface. An unknown-word
    // entry carries a six-field feature string, so twenty attributes are absent
    // on an OOV token — and a missing attribute satisfies neither polarity,
    // making both `[orthBase=X]` and `[orthBase!=X]` fail silently on that
    // material.
    //
    // Only these two qualify. A pronunciation, kana reading or inflection class
    // is not determined by the surface; synthesising one would invent data.
    //
    // Two conditions, and both matter:
    //
    // * `LexType::Unknown`, not merely "absent". A system or user entry with a
    //   short feature record is a *supplied* record that stops early, and
    //   filling it would break the property this design rests on — that
    //   `lexType=unknown` identifies every synthesised value. Read from
    //   `token.lex_type()` rather than the `LexType` attribute, which this
    //   function has not set yet.
    // * `is_none`, not "is `*`". UniDic writes `*` for "does not apply here",
    //   which is a supplied value and must survive untouched.
    if token.lex_type() == LexType::Unknown {
        for attribute in [OrthBase, Lemma] {
            if values[attribute as usize].is_none()
                && (requested[attribute as usize] || attribute == Lemma)
            {
                values[attribute as usize] = Some(CompactString::from(token.surface()));
            }
        }
    }

    // Computed after the fallback above, and deliberately unchanged by it: the
    // rule is "the normalized lemma if present, else the surface", and the
    // fallback sets lemma to exactly that surface. This is the established
    // local convention the fallback generalises, not a new one.
    if requested[Norm as usize] {
        values[Norm as usize] = Some(match values[Lemma as usize].as_ref() {
            None => CompactString::from(token.surface()),
            Some(lemma) => normalized_lemma(lemma),
        });
    }
    if requested[LexType as usize] {
        values[LexType as usize] = Some(match token.lex_type() {
            LexType::System => "system".into(),
            LexType::User => "user".into(),
            LexType::Unknown => "unknown".into(),
        });
    }

    // Resolve POS
    let next_tag: Option<CompactString> = next_token.map(|t| {
        CompactString::from(
            t.feature()
                .split(',')
                .take(4)
                .filter(|&s| s != "*")
                .collect::<Vec<_>>()
                .join("-"),
        )
    });
    let (pos, _) = resolve_pos(orth.as_str(), &tag, next_tag.as_deref());
    if pos.is_empty() {
        #[expect(
            clippy::panic,
            reason = "a resolved token always carries a non-empty pos field"
        )]
        {
            panic!(
                "Empty 'pos' field encountered for surface {:?}",
                token.surface()
            );
        }
    } else if requested[Pos as usize] {
        values[Pos as usize] = Some(pos);
    }

    for attribute in [Pos1, Pos2, Pos3, Pos4, Lemma, Orth] {
        if !requested[attribute as usize] {
            values[attribute as usize] = None;
        }
    }
    values
}

pub fn tokenize(text: &str, tokenizer: &Tokenizer) -> Vec<TokenInfo> {
    let mut worker = tokenizer.new_worker();
    worker.reset_sentence(text);
    worker.tokenize();
    let num_tokens = worker.num_tokens();
    let tokens: Vec<TokenInfo> = (0..num_tokens)
        .map(|i| {
            let next_token = if i < num_tokens - 1 {
                let next_token = worker.token(i + 1);
                Some(next_token)
            } else {
                None
            };
            featurize(&worker.token(i), next_token.as_ref())
        })
        .collect();

    tokens
}

/// Tokenizes one sentence directly into the catalog's dense execution projection.
pub fn tokenize_bound(
    text: &str,
    tokenizer: &Tokenizer,
    projection: &BoundProjection,
) -> Result<BoundTokenSentence, BoundTokenError> {
    let mut worker = tokenizer.new_worker();
    tokenize_worker_bound(&mut worker, text, projection).map(|(sentence, _)| sentence)
}

/// Tokenizes with lane-owned scratch state and returns exact admitted-byte ranges.
pub(crate) fn tokenize_worker_bound(
    worker: &mut Worker,
    text: &str,
    projection: &BoundProjection,
) -> Result<(BoundTokenSentence, Vec<std::ops::Range<usize>>), BoundTokenError> {
    worker.reset_sentence(text);
    worker.tokenize();
    let token_count = worker.num_tokens();
    let mut requested = [false; TokenAttributeIndex::COUNT];
    for attribute in projection.attributes() {
        requested[*attribute as usize] = true;
    }
    let value_count = token_count
        .checked_mul(projection.len())
        .ok_or(BoundTokenError::ResourceAccounting)?;
    let mut values = Vec::with_capacity(value_count);
    let mut ranges = Vec::with_capacity(token_count);
    for index in 0..token_count {
        let next_token = (index + 1 < token_count).then(|| worker.token(index + 1));
        ranges.push(worker.token(index).range_byte());
        let mut token_values =
            featurize_selected(&worker.token(index), next_token.as_ref(), &requested);
        values.extend(
            projection
                .attributes()
                .iter()
                .map(|attribute| token_values[*attribute as usize].take()),
        );
    }
    BoundTokenSentence::from_projected_values(projection, token_count, values)
        .map(|sentence| (sentence, ranges))
}

#[cfg(test)]
mod tests {
    use super::{
        TokenAttributeIndex, TokenInfo, TokenSchema, featurize, resolve_pos, tokenize_bound,
    };
    use crate::token_match::TokenInfoTrait;

    /// `size_of::<TokenInfo>()` is a published quantity, not an implementation
    /// detail: `estimate_token_info_bytes` starts from `size_of_val(sentence)`,
    /// and that result is what the V6 writer records in its residency index,
    /// `verify_integrity` checks decoded sentences against, preflight compares
    /// the encoded estimate to, and deferred execution partitions by. Changing
    /// it invalidates the residency index of every artifact on disk.
    ///
    /// 32 attributes, each an `Option<CompactString>` occupying a
    /// niche-optimized 24 bytes.
    #[test]
    fn token_info_occupies_the_size_the_residency_index_was_written_with() {
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

    /// The generated named accessors are only sound if the list that generates
    /// them is in `TokenAttributeIndex::ALL` order — that ordering is what maps
    /// each accessor to its array slot, and what fixes the serde mirror's field
    /// order, which is the on-disk shape `TokenTable` is persisted with.
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

    use vibrato_rkyv::{Dictionary, SystemDictionaryBuilder, Tokenizer};

    #[test]
    fn bound_tokenization_preserves_requested_derived_values() {
        let dictionary = SystemDictionaryBuilder::from_readers(
            "食べ,0,0,1,動詞,一般,*,*,下一段-バ行,連用形-一般,タベル,食べる,食べ,タベ,食べる,タベル,和,*,*,*,*,*,*,*,*,タベ,タベル,タベ,タベル,0,C2,*,*"
                .as_bytes(),
            "1 1\n0 0 0".as_bytes(),
            "DEFAULT 0 1 0".as_bytes(),
            "DEFAULT,0,0,100,名詞,普通名詞,一般,*,*,*".as_bytes(),
        )
        .expect("test dictionary should build");
        let mut bytes = Vec::new();
        dictionary
            .write(&mut bytes)
            .expect("test dictionary should serialize");
        let tokenizer = Tokenizer::new(
            Dictionary::read(bytes.as_slice()).expect("test dictionary should deserialize"),
        );
        let schema = TokenSchema::unidic_morphology();
        let projection = schema
            .bind_projection([
                TokenAttributeIndex::Lemma,
                TokenAttributeIndex::Orth,
                TokenAttributeIndex::Pos,
            ])
            .expect("the current schema provides the requested values");

        let sentence = tokenize_bound("食べ", &tokenizer, &projection)
            .expect("bound tokenization fits resource accounting");
        let token = sentence.token(0).expect("one token is produced");

        assert_eq!(
            token.get(projection.ordinal(TokenAttributeIndex::Lemma).unwrap()),
            Some("食べる")
        );
        assert_eq!(
            token.get(projection.ordinal(TokenAttributeIndex::Orth).unwrap()),
            Some("食べ")
        );
        assert_eq!(
            token.get(projection.ordinal(TokenAttributeIndex::Pos).unwrap()),
            Some("VERB")
        );
        assert!(projection.ordinal(TokenAttributeIndex::Text).is_none());
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

    #[test]
    fn unidic_sentence_punctuation_normalizes_to_punct() {
        assert_eq!(resolve_pos("。", "補助記号-句点", None).0, "PUNCT");
    }

    #[test]
    fn featurize_uses_surface_when_dictionary_omits_orth() {
        let lexicon_csv = "表層,0,0,1,名詞,普通名詞,一般,*,*,*,ヒョウソウ,表層";
        let matrix_def = "1 1\n0 0 0";
        let char_def = "DEFAULT 0 1 0";
        let unk_def = "DEFAULT,0,0,100,名詞,普通名詞,一般,*,*,*,*,*";
        let dictionary = SystemDictionaryBuilder::from_readers(
            lexicon_csv.as_bytes(),
            matrix_def.as_bytes(),
            char_def.as_bytes(),
            unk_def.as_bytes(),
        )
        .expect("test dictionary should build");
        let mut bytes = Vec::new();
        dictionary
            .write(&mut bytes)
            .expect("test dictionary should serialize");
        let tokenizer = Tokenizer::new(
            Dictionary::read(bytes.as_slice()).expect("test dictionary should deserialize"),
        );
        let mut worker = tokenizer.new_worker();
        worker.reset_sentence("表層");
        worker.tokenize();

        let token = featurize(&worker.token(0), None);

        assert_eq!(
            token
                .get(TokenAttributeIndex::Orth)
                .map(compact_str::CompactString::as_str),
            Some("表層")
        );
        assert_eq!(
            token
                .get(TokenAttributeIndex::Text)
                .map(compact_str::CompactString::as_str),
            Some("表層")
        );
    }

    /// Featurize the first token of `text` under a minimal in-memory
    /// dictionary.
    ///
    /// `unk_def`'s feature field count is the whole point of these fixtures:
    /// UniDic's real unknown-word entry carries six fields, so everything from
    /// `lform` onward is *absent* rather than `*`. An eight-field entry would
    /// deliver `lemma = "*"` and silently stop exercising the case.
    fn featurize_first(lexicon_csv: &str, unk_def: &str, text: &str) -> TokenInfo {
        let dictionary = SystemDictionaryBuilder::from_readers(
            lexicon_csv.as_bytes(),
            "1 1\n0 0 0".as_bytes(),
            "DEFAULT 0 1 0".as_bytes(),
            unk_def.as_bytes(),
        )
        .expect("test dictionary should build");
        let mut bytes = Vec::new();
        dictionary
            .write(&mut bytes)
            .expect("test dictionary should serialize");
        let tokenizer = Tokenizer::new(
            Dictionary::read(bytes.as_slice()).expect("test dictionary should deserialize"),
        );
        let mut worker = tokenizer.new_worker();
        worker.reset_sentence(text);
        worker.tokenize();
        featurize(&worker.token(0), None)
    }

    /// A token no lexicon entry covers. Reproduces the real unknown-word
    /// attribute profile: `pos1`–`cform` present, twenty attributes absent.
    fn featurize_oov_token(surface: &str) -> TokenInfo {
        featurize_first(
            "表層,0,0,1,名詞,普通名詞,一般,*,*,*,ヒョウソウ,表層",
            "DEFAULT,0,0,100,名詞,普通名詞,一般,*,*,*",
            surface,
        )
    }

    /// V1 renames this value to `xpos` and must not change how it is built.
    #[test]
    fn tag_joins_pos_components_and_drops_asterisks() {
        let token = featurize_first(
            "語,0,0,1,名詞,普通名詞,一般,*,*,*,ゴ,語",
            "DEFAULT,0,0,100,名詞,普通名詞,一般,*,*,*",
            "語",
        );
        assert_eq!(
            token
                .get(TokenAttributeIndex::Tag)
                .map(compact_str::CompactString::as_str),
            Some("名詞-普通名詞-一般")
        );
    }

    /// V1 renames this to `upos`. It is derived, not feature field 28.
    #[test]
    fn pos_is_derived_rather_than_read_from_the_feature_string() {
        let token = featurize_first(
            "語,0,0,1,名詞,普通名詞,一般,*,*,*,ゴ,語,語,*,*,*,*,*,*,*,*,*,*,*,*,*,*,*,NOT_DERIVED",
            "DEFAULT,0,0,100,名詞,普通名詞,一般,*,*,*",
            "語",
        );
        assert_eq!(
            token
                .get(TokenAttributeIndex::Pos)
                .map(compact_str::CompactString::as_str),
            Some("NOUN")
        );
    }

    #[test]
    fn norm_falls_back_to_the_surface_when_lemma_is_absent() {
        let token = featurize_oov_token("未知語");
        assert_eq!(
            token
                .get(TokenAttributeIndex::Norm)
                .map(compact_str::CompactString::as_str),
            Some("未知語")
        );
    }

    /// Exactly two attributes get the OOV surface fallback. V1 must not widen it.
    #[test]
    fn oov_surface_fallback_covers_orthbase_and_lemma_only() {
        let token = featurize_oov_token("未知語");
        for attribute in [TokenAttributeIndex::OrthBase, TokenAttributeIndex::Lemma] {
            assert_eq!(
                token.get(attribute).map(compact_str::CompactString::as_str),
                Some("未知語"),
                "{attribute:?} receives the OOV-only surface fallback"
            );
        }
        for attribute in [
            TokenAttributeIndex::LForm,
            TokenAttributeIndex::Pron,
            TokenAttributeIndex::PronBase,
            TokenAttributeIndex::Kana,
            TokenAttributeIndex::KanaBase,
            TokenAttributeIndex::Form,
            TokenAttributeIndex::FormBase,
        ] {
            assert!(
                token.get(attribute).is_none(),
                "{attribute:?} must remain absent rather than receive the OOV fallback"
            );
        }
    }

    /// A known token whose `orthBase` the dictionary supplies as `*`.
    /// The eleventh feature field is `orthBase`; here it is UniDic's
    /// "unspecified" marker, which is a value and not an absence.
    fn featurize_known_token_with_unspecified_base() -> TokenInfo {
        featurize_first(
            "テスト,0,0,1,名詞,普通名詞,一般,*,*,*,*,*,テスト,*,*",
            "DEFAULT,0,0,100,名詞,普通名詞,一般,*,*,*",
            "テスト",
        )
    }

    /// A known token whose lemma the lexicon supplies verbatim.
    fn featurize_with_lemma(lemma: &str) -> TokenInfo {
        featurize_first(
            &format!("テスト,0,0,1,名詞,普通名詞,一般,*,*,*,テスト,{lemma},テスト,*,*"),
            "DEFAULT,0,0,100,名詞,普通名詞,一般,*,*,*",
            "テスト",
        )
    }

    fn norm_of(lemma: &str) -> String {
        featurize_with_lemma(lemma)
            .get(TokenAttributeIndex::Norm)
            .map(compact_str::CompactString::to_string)
            .expect("norm is always populated")
    }

    /// UniDic's lemma is a lexeme identifier, not a normalized form: it carries
    /// a disambiguator after a hyphen. `norm` is the normalized form, so it
    /// must not.
    #[test]
    fn norm_strips_the_unidic_lexeme_disambiguator() {
        assert_eq!(norm_of("システム-system"), "システム");
        assert_eq!(norm_of("我々-代名詞"), "我々");
        assert_eq!(norm_of("差す-他動詞"), "差す");
    }

    /// `ピックアップ-pick-up` is why the split is on the FIRST hyphen: the
    /// disambiguator itself contains one, so splitting on the last would keep
    /// half of it.
    #[test]
    fn norm_splits_at_the_first_hyphen_not_the_last() {
        assert_eq!(norm_of("ピックアップ-pick-up"), "ピックアップ");
    }

    /// A lemma that is punctuation has no head to keep. Without this guard the
    /// split yields an empty string and the token loses its normalized form
    /// entirely — measured on 15 real lemma types (`-`, `--`, `->`, `-➀`).
    #[test]
    fn norm_keeps_a_lemma_whose_head_would_be_empty() {
        assert_eq!(norm_of("--"), "--");
        assert_eq!(norm_of("-"), "-");
        assert_eq!(norm_of("-➀"), "-➀");
    }

    /// UniDic lemmatizes the light verb to the orthographic 為る, which no
    /// reader writes. natsume-simple ships the same mapping as `simple_lemma`.
    #[test]
    fn norm_maps_the_orthographic_light_verb_to_its_written_form() {
        assert_eq!(norm_of("為る"), "する");
    }

    /// The normalization is a rewrite of `norm` only. `lemma` remains the
    /// lexeme identifier, disambiguator included, because that is what makes it
    /// able to distinguish homographs at all.
    #[test]
    fn lemma_itself_is_left_alone() {
        let token = featurize_with_lemma("システム-system");
        assert_eq!(
            token
                .get(TokenAttributeIndex::Lemma)
                .map(compact_str::CompactString::as_str),
            Some("システム-system")
        );
    }

    /// An ordinary lemma passes through untouched, which is most of them.
    #[test]
    fn norm_leaves_an_undisambiguated_lemma_unchanged() {
        assert_eq!(norm_of("実験"), "実験");
        assert_eq!(norm_of("行う"), "行う");
    }

    /// Pins the splitter against the cases vibrato's own `parse_csv_row` tests,
    /// since `mod utils` is private and this reimplements those semantics
    /// rather than calling them.
    #[test]
    fn feature_fields_matches_the_csv_semantics_the_dictionary_was_built_with() {
        let fields = |record| {
            super::feature_fields(record)
                .into_iter()
                .map(std::borrow::Cow::into_owned)
                .collect::<Vec<_>>()
        };

        // vibrato's `test_parse_csv_row` and `test_parse_csv_row_with_quote`.
        assert_eq!(fields("名詞,トスカーナ"), ["名詞", "トスカーナ"]);
        assert_eq!(
            fields(r#"名詞,"1,2-ジクロロエタン""#),
            ["名詞", "1,2-ジクロロエタン"]
        );

        // A doubled quote is one literal quote and does not end the field.
        assert_eq!(
            fields(r#"a,"he said ""hi""",b"#),
            ["a", r#"he said "hi""#, "b"]
        );
        // Empty fields survive at both ends and in the middle.
        assert_eq!(fields("a,,b"), ["a", "", "b"]);
        assert_eq!(fields(""), [""]);
        // An unterminated quote takes the rest rather than failing.
        assert_eq!(fields(r#"a,"unterminated,b"#), ["a", "unterminated,b"]);
    }

    /// A feature record may quote a field that contains a comma — UniDic does
    /// exactly this for the ASCII comma's own entry, and for lexemes like
    /// `1,2-ジクロロエタン`. Splitting on a bare comma shatters that field and
    /// shifts every column after it.
    #[test]
    fn a_quoted_feature_field_does_not_shift_the_columns() {
        // The real UniDic record for `,`, whose orth and orthBase are the
        // literal comma. Field 13 is goshu and field 20 is type; both land
        // wrong if the two quoted fields are split.
        let token = featurize_first(
            r#"",",0,0,1,補助記号,読点,*,*,*,*,*,，,",",*,",",*,記号,*,*,*,*,*,*,補助"#,
            "DEFAULT,0,0,100,名詞,普通名詞,一般,*,*,*",
            ",",
        );

        let value = |attribute| token.get(attribute).map(compact_str::CompactString::as_str);
        assert_eq!(value(TokenAttributeIndex::Pos1), Some("補助記号"));
        assert_eq!(value(TokenAttributeIndex::Pos2), Some("読点"));
        assert_eq!(value(TokenAttributeIndex::Lemma), Some("，"));
        assert_eq!(value(TokenAttributeIndex::Orth), Some(","));
        assert_eq!(value(TokenAttributeIndex::OrthBase), Some(","));
        assert_eq!(value(TokenAttributeIndex::Goshu), Some("記号"));
        assert_eq!(value(TokenAttributeIndex::Type), Some("補助"));
    }

    #[test]
    fn an_unknown_token_carries_surface_derived_base_forms() {
        // A researcher writing [orthBase=X] or [lemma=X] has no way to see that
        // the predicate cannot match unknown-word material: a missing attribute
        // satisfies neither polarity, so even the negative form fails silently.
        let token = featurize_oov_token("qwertyzxcv");

        for attribute in [
            TokenAttributeIndex::Orth,
            TokenAttributeIndex::OrthBase,
            TokenAttributeIndex::Lemma,
            TokenAttributeIndex::Norm,
            TokenAttributeIndex::Text,
        ] {
            assert_eq!(
                token.get(attribute).map(compact_str::CompactString::as_str),
                Some("qwertyzxcv"),
                "{attribute:?} must fall back to the surface"
            );
        }
    }

    #[test]
    fn an_unknown_token_remains_directly_identifiable() {
        // No information is lost by synthesising a base form, because lexType
        // still distinguishes a synthesised one from a dictionary-supplied one.
        let token = featurize_oov_token("qwertyzxcv");

        assert_eq!(
            token
                .get(TokenAttributeIndex::LexType)
                .map(compact_str::CompactString::as_str),
            Some("unknown")
        );
    }

    #[test]
    fn the_surface_fallback_does_not_reach_attributes_the_surface_cannot_determine() {
        // The fallback is confined to attributes whose value space *is* the
        // surface orthography. A pronunciation, a kana reading or an inflection
        // class is not derivable from the surface, so synthesising one would
        // invent data rather than recover it.
        let token = featurize_oov_token("qwertyzxcv");

        // All eighteen. The corrected twenty-attribute measurement is the
        // central empirical finding behind this policy; enumerating a subset
        // would let an over-broad fallback pass unnoticed on the ones left out.
        for attribute in [
            TokenAttributeIndex::LForm,
            TokenAttributeIndex::Pron,
            TokenAttributeIndex::PronBase,
            TokenAttributeIndex::Goshu,
            TokenAttributeIndex::IType,
            TokenAttributeIndex::IForm,
            TokenAttributeIndex::FType,
            TokenAttributeIndex::FForm,
            TokenAttributeIndex::IConType,
            TokenAttributeIndex::FConType,
            TokenAttributeIndex::Type,
            TokenAttributeIndex::Kana,
            TokenAttributeIndex::KanaBase,
            TokenAttributeIndex::Form,
            TokenAttributeIndex::FormBase,
            TokenAttributeIndex::AType,
            TokenAttributeIndex::AConType,
            TokenAttributeIndex::AModType,
        ] {
            assert!(
                token.get(attribute).is_none(),
                "{attribute:?} must stay absent"
            );
        }
    }

    #[test]
    fn a_known_token_with_a_short_feature_record_is_not_given_a_base_form() {
        // The fallback is scoped to unknown-word tokens, not to absence. A
        // system or user entry whose feature string stops early is a *supplied*
        // record that happens to be short, and synthesising into it would break
        // the property the whole design rests on: that lexType=unknown
        // identifies every synthesised value.
        //
        // This is not hypothetical. The eight-field lexicon row below is the
        // one `featurize_uses_surface_when_dictionary_omits_orth` already uses,
        // and user dictionaries routinely carry short records.
        let token = featurize_first(
            "表層,0,0,1,名詞,普通名詞,一般,*,*,*,ヒョウソウ,表層",
            "DEFAULT,0,0,100,名詞,普通名詞,一般,*,*,*",
            "表層",
        );

        assert_eq!(
            token
                .get(TokenAttributeIndex::LexType)
                .map(compact_str::CompactString::as_str),
            Some("system"),
            "fixture must be a known token or it tests nothing"
        );
        assert!(
            token.get(TokenAttributeIndex::OrthBase).is_none(),
            "a short system record must not be filled in"
        );
        // `orth` is the pre-existing exception and stays: it has fallen back to
        // the surface since long before this policy, for every token.
        assert_eq!(
            token
                .get(TokenAttributeIndex::Orth)
                .map(compact_str::CompactString::as_str),
            Some("表層")
        );
    }

    #[test]
    fn a_dictionary_supplied_unspecified_value_is_not_replaced() {
        // UniDic writes `*` for "this attribute does not apply here". That is a
        // supplied value, not an absence, and overwriting it would erase a real
        // distinction. A known token whose base form is `*` keeps `*`.
        let token = featurize_known_token_with_unspecified_base();

        assert_eq!(
            token
                .get(TokenAttributeIndex::OrthBase)
                .map(compact_str::CompactString::as_str),
            Some("*")
        );
    }

    #[test]
    fn the_normalized_form_of_an_unknown_token_is_unchanged_by_the_fallback() {
        // `norm` was already "lemma if present, else the surface", and it is
        // computed after the fallback sets lemma to exactly that surface — so
        // its value must not move. If it does, the fallback ran in the wrong
        // order and the two rules have diverged.
        assert_eq!(
            featurize_oov_token("qwertyzxcv")
                .get(TokenAttributeIndex::Norm)
                .map(compact_str::CompactString::as_str),
            Some("qwertyzxcv")
        );
        assert_eq!(
            featurize_known_token_with_unspecified_base()
                .get(TokenAttributeIndex::Norm)
                .map(compact_str::CompactString::as_str),
            Some("*")
        );
    }
}
