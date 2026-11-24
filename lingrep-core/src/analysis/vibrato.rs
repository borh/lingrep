use std::borrow::Cow;
use std::path::{Path, PathBuf};

use compact_str::CompactString;
use vibrato_rkyv::{
    Tokenizer,
    dictionary::{LexType, LexiconEntries},
    token::Token,
    tokenizer::worker::Worker,
};

use crate::token_match::{
    BoundProjection, BoundTokenError, BoundTokenSentence, TAG_BIGRAM_MAP, TAG_MAP, TAG_ORTH_MAP,
    TokenAttributeIndex, TokenInfo, TokenSchema,
};

use super::{
    AnalysisError, AnalyzedInput, AnalyzedSentence, AnalyzerDescriptor, AnalyzerFamily,
    AnalyzerPlan, AnalyzerSession, CodePointCursor, ExtractionProfile, JapaneseSegmentationPolicy,
    NormalizationPolicy, SegmentationMode, SurfaceToken, TEXT_KEY_REVISION, TextUnit, TextUnitKind,
    UNPROTECTED, ValidatedAnalysisBatch, ViewFold, ViewSpan, analyzer::sealed,
    declared_token_schema, text_key,
};

/// Immutable Vibrato configuration and the scalar schema it can produce.
#[derive(Clone)]
pub struct VibratoPlan {
    tokenizer: Tokenizer,
    fold: ViewFold,
    schema: TokenSchema,
    segmentation: SegmentationMode,
    descriptor: AnalyzerDescriptor,
}

impl VibratoPlan {
    /// # Errors
    ///
    /// As [`Self::configured`].
    pub fn new(tokenizer: Tokenizer, dictionary: &Path) -> Result<Self, LexiconEnumerationError> {
        Self::with_mode(tokenizer, dictionary, SegmentationMode::Input)
    }

    /// # Errors
    ///
    /// As [`Self::configured`].
    pub fn with_mode(
        tokenizer: Tokenizer,
        dictionary: &Path,
        segmentation: SegmentationMode,
    ) -> Result<Self, LexiconEnumerationError> {
        Self::configured(
            tokenizer,
            dictionary,
            [0; 32],
            ExtractionProfile::Morphology,
            segmentation,
            NormalizationPolicy::None,
        )
    }

    /// Builds a plan that analyzes the [`ViewFold`] view of each sentence,
    /// protecting the surfaces of the tokenizer's system and user lexicons.
    ///
    /// `dictionary` is the path `tokenizer` was loaded from; it names the
    /// dictionary in the error.
    ///
    /// # Errors
    ///
    /// Returns [`LexiconEnumerationError`] if a lexicon of the tokenizer
    /// cannot be enumerated.
    pub fn configured(
        tokenizer: Tokenizer,
        dictionary: &Path,
        dictionary_digest: [u8; 32],
        profile: ExtractionProfile,
        segmentation: SegmentationMode,
        normalization: NormalizationPolicy,
    ) -> Result<Self, LexiconEnumerationError> {
        let schema = declared_token_schema(AnalyzerFamily::Vibrato, profile)
            .expect("Vibrato declares a schema for every profile");
        let descriptor =
            Self::descriptor_for(dictionary_digest, profile, segmentation, normalization);
        let fold = dictionary_fold(&tokenizer, dictionary)?;
        Ok(Self {
            tokenizer,
            fold,
            schema,
            segmentation,
            descriptor,
        })
    }

    /// Builds the descriptor a [`VibratoPlan`] with these settings would carry,
    /// without loading a tokenizer.
    ///
    /// A caller that has to name a content-addressed artifact needs the
    /// analyzer's semantic identity *before* it decides whether to build, and
    /// loading a ~1.3 GB dictionary to answer "does this already exist" would
    /// invert the cost of the question. The descriptor is a pure function of
    /// these four inputs, so this is the same value [`VibratoPlan::configured`]
    /// stores. The adapter, projection, and output tags below own semantic
    /// versioning for built-in Vibrato tokenization: an output-affecting change
    /// must bump the applicable tag so prepared-corpus identity moves with it.
    #[must_use]
    pub fn descriptor_for(
        dictionary_digest: [u8; 32],
        profile: ExtractionProfile,
        segmentation: SegmentationMode,
        normalization: NormalizationPolicy,
    ) -> AnalyzerDescriptor {
        let schema = declared_token_schema(AnalyzerFamily::Vibrato, profile)
            .expect("Vibrato declares a schema for every profile");
        AnalyzerDescriptor::new(
            AnalyzerFamily::Vibrato,
            profile,
            segmentation,
            normalization,
            "ja",
            match segmentation {
                SegmentationMode::Input => "input-v1",
                SegmentationMode::Analyzer => "ja-delimiter-v1",
            },
            digest(&[b"lingrep.vibrato-adapter/v2", &ViewFold::rules_identity()]),
            dictionary_digest,
            digest(&[b"lingrep.vibrato-projection/v2", TEXT_KEY_REVISION]),
            *blake3::hash(b"lingrep.vibrato-alignment/v2").as_bytes(),
            *blake3::hash(b"lingrep.vibrato-components/tokenizer").as_bytes(),
            *blake3::hash(b"lingrep.vibrato-output/v2").as_bytes(),
            schema.digest(),
        )
        .expect("the built-in Vibrato descriptor is valid")
    }

    #[must_use]
    pub const fn descriptor(&self) -> &AnalyzerDescriptor {
        &self.descriptor
    }

    /// A plan over the same tokenizer and protected-surface table with the
    /// descriptor [`Self::configured`] would build from these settings.
    #[must_use]
    pub fn reconfigured(
        &self,
        dictionary_digest: [u8; 32],
        profile: ExtractionProfile,
        segmentation: SegmentationMode,
        normalization: NormalizationPolicy,
    ) -> Self {
        Self {
            tokenizer: self.tokenizer.clone(),
            fold: self.fold.clone(),
            schema: declared_token_schema(AnalyzerFamily::Vibrato, profile)
                .expect("Vibrato declares a schema for every profile"),
            segmentation,
            descriptor: Self::descriptor_for(
                dictionary_digest,
                profile,
                segmentation,
                normalization,
            ),
        }
    }

    /// Tokenizes one sentence, as a session of this plan analyzes it, with
    /// every attribute.
    #[must_use]
    pub fn tokenize(&self, text: &str) -> Vec<TokenInfo> {
        tokenize_folded(text, &self.tokenizer, &self.fold)
    }
}

fn digest(parts: &[&[u8]]) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    for part in parts {
        hasher.update(&(part.len() as u64).to_le_bytes());
        hasher.update(part);
    }
    *hasher.finalize().as_bytes()
}

/// A Vibrato dictionary whose lexicon cannot be enumerated, so the
/// protected-surface table of its [`ViewFold`] cannot be built.
#[derive(Debug, thiserror::Error)]
#[error("cannot enumerate the lexicon of Vibrato dictionary {}", .dictionary.display())]
pub struct LexiconEnumerationError {
    pub dictionary: PathBuf,
    #[source]
    pub source: vibrato_rkyv::errors::VibratoError,
}

fn dictionary_fold(
    tokenizer: &Tokenizer,
    dictionary: &Path,
) -> Result<ViewFold, LexiconEnumerationError> {
    fold_from_lexicons(
        dictionary,
        std::iter::once(tokenizer.system_lexicon())
            .chain(tokenizer.user_lexicon())
            .map(|lexicon| lexicon.entries()),
    )
}

fn fold_from_lexicons<'a>(
    dictionary: &Path,
    lexicons: impl IntoIterator<Item = vibrato_rkyv::errors::Result<LexiconEntries<'a>>>,
) -> Result<ViewFold, LexiconEnumerationError> {
    let lexicons = lexicons
        .into_iter()
        .collect::<Result<Vec<_>, _>>()
        .map_err(|source| LexiconEnumerationError {
            dictionary: dictionary.to_owned(),
            source,
        })?;
    Ok(ViewFold::from_dictionary_surfaces(
        lexicons
            .iter()
            .flat_map(|entries| entries.iter().map(|entry| entry.surface())),
    ))
}

impl sealed::Plan for VibratoPlan {}

impl AnalyzerPlan for VibratoPlan {
    type Session = VibratoSession;

    fn schema(&self) -> &TokenSchema {
        &self.schema
    }

    fn create_session(&self) -> Self::Session {
        VibratoSession {
            worker: self.tokenizer.new_worker(),
            fold: self.fold.clone(),
            schema: self.schema.clone(),
            segmentation: self.segmentation,
        }
    }
}

/// Lane-owned Vibrato scratch state reused across bounded calls.
pub struct VibratoSession {
    worker: Worker,
    fold: ViewFold,
    schema: TokenSchema,
    segmentation: SegmentationMode,
}

impl sealed::Session for VibratoSession {}

impl AnalyzerSession for VibratoSession {
    fn schema(&self) -> &TokenSchema {
        &self.schema
    }

    fn analyze_bounded_batch(
        &mut self,
        inputs: Vec<TextUnit>,
        projection: &BoundProjection,
    ) -> Result<ValidatedAnalysisBatch, AnalysisError> {
        if projection.physical_attributes().iter().any(Option::is_none) {
            return Err(AnalysisError::UnsupportedProjection);
        }

        let mut analyzed_inputs = Vec::with_capacity(inputs.len());
        for input in inputs {
            let expected_kind = match self.segmentation {
                SegmentationMode::Input => TextUnitKind::Sentence,
                SegmentationMode::Analyzer => TextUnitKind::Segment,
            };
            if input.kind() != expected_kind {
                return Err(AnalysisError::WrongInputKind);
            }
            let sentence_ranges = match self.segmentation {
                SegmentationMode::Input => vec![
                    input
                        .source_range(0, input.len())
                        .map_err(|_| AnalysisError::InvalidSourceRange)?,
                ]
                .into_boxed_slice(),
                SegmentationMode::Analyzer => {
                    JapaneseSegmentationPolicy::V1.sentence_ranges(&input)?
                }
            };
            let mut sentences = Vec::with_capacity(sentence_ranges.len());
            for (ordinal, sentence_range) in sentence_ranges.into_vec().into_iter().enumerate() {
                let sentence_text = input
                    .slice(sentence_range)
                    .map_err(|_| AnalysisError::InvalidSourceRange)?;
                let (words, token_ranges) =
                    tokenize_worker_bound(&mut self.worker, sentence_text, &self.fold, projection)
                        .map_err(|_| AnalysisError::ResourceAccounting)?;
                let sentence_start = sentence_range.start() as usize;
                let mut surface_tokens = Vec::with_capacity(token_ranges.len());
                for (index, range) in token_ranges.into_iter().enumerate() {
                    let start = sentence_start
                        .checked_add(range.start)
                        .ok_or(AnalysisError::ResourceAccounting)?;
                    let end = sentence_start
                        .checked_add(range.end)
                        .ok_or(AnalysisError::ResourceAccounting)?;
                    let source_range = input
                        .source_range(start, end)
                        .map_err(|_| AnalysisError::InvalidSourceRange)?;
                    let word_start =
                        u32::try_from(index).map_err(|_| AnalysisError::ResourceAccounting)?;
                    let word_end = word_start
                        .checked_add(1)
                        .ok_or(AnalysisError::ResourceAccounting)?;
                    surface_tokens.push(SurfaceToken::new(source_range, word_start, word_end));
                }
                sentences.push(AnalyzedSentence::new(
                    u32::try_from(ordinal).map_err(|_| AnalysisError::ResourceAccounting)?,
                    sentence_range,
                    surface_tokens.into_boxed_slice(),
                    words,
                )?);
            }
            analyzed_inputs.push(AnalyzedInput::new(input, sentences, self.segmentation)?);
        }
        ValidatedAnalysisBatch::new(analyzed_inputs)
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
        return (CompactString::const_new(pos), None);
    }

    if let Some(next_tag) = next_tag
        && let Some(&(current_pos, next_pos)) = TAG_BIGRAM_MAP.get(&(tag, next_tag))
    {
        return (
            CompactString::const_new(
                current_pos.unwrap_or_else(|| TAG_MAP.get(tag).copied().unwrap_or("")),
            ),
            next_pos.map(CompactString::const_new),
        );
    }

    // Without a following token, the potentially dependent verb is independent.
    if next_tag.is_none() && tag == "動詞-非自立可能" {
        return (CompactString::const_new("VERB"), None);
    }

    (
        CompactString::const_new(TAG_MAP.get(tag).copied().unwrap_or("")),
        None,
    )
}

// UniDic lemmas can include a disambiguator after the first hyphen, as in
// ピックアップ-pick-up. Update the Vibrato policy digest in `VibratoPlan` if this
// normalization changes the attributes stored in prepared corpora.
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

// Vibrato retains UniDic's CSV quoting, including comma fields such as
// `","` and lexemes such as `1,2-ジクロロエタン`.
fn for_each_feature_field(feature: &str, mut visit: impl FnMut(usize, &str)) {
    if !feature.contains('"') {
        for (i, field) in feature.split(',').enumerate() {
            visit(i, field);
        }
        return;
    }
    let mut rest = feature;
    let mut i = 0;
    loop {
        let (field, remainder) = next_feature_field(rest);
        visit(i, field.as_ref());
        let Some(remainder) = remainder else {
            return;
        };
        rest = remainder;
        i += 1;
    }
}

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

/// Featurizes a token whose surface is its own analyzed text.
pub fn featurize(token: &Token, next_token: Option<&Token>) -> TokenInfo {
    TokenInfo::from_values(featurize_selected(
        token,
        next_token,
        &[true; TokenAttributeIndex::COUNT],
        token.surface(),
    ))
}

/// Joins up to four POS components into a hyphen-separated `CompactString`
/// without intermediate `Vec` or `String` allocations.
fn push_pos_tag<'a>(parts: impl IntoIterator<Item = &'a str>) -> CompactString {
    let mut tag = CompactString::default();
    for part in parts {
        if !tag.is_empty() {
            tag.push('-');
        }
        tag.push_str(part);
    }
    tag
}

/// `surface` is the source slice that produced `token`, which the analyzer
/// read in its folded view.
fn featurize_selected(
    token: &Token,
    next_token: Option<&Token>,
    requested: &[bool; TokenAttributeIndex::COUNT],
    surface: &str,
) -> [Option<CompactString>; TokenAttributeIndex::COUNT] {
    use TokenAttributeIndex::{
        Lemma, LexType as LexTypeAttr, Norm, Orth, OrthBase, Pos, Pos1, Pos2, Pos3, Pos4, Surface,
        Tag,
    };

    let mut values: [Option<CompactString>; TokenAttributeIndex::COUNT] =
        [const { None }; TokenAttributeIndex::COUNT];

    let needs_pos = requested[Pos as usize];
    let needs_tag = requested[Tag as usize] || needs_pos;
    let needs_lemma = requested[Lemma as usize] || requested[Norm as usize];

    for_each_feature_field(token.feature(), |i, field| {
        let Some(&index) = TokenAttributeIndex::ALL.get(i) else {
            return;
        };
        let needed_for_derivation = match index {
            Pos1 | Pos2 | Pos3 | Pos4 => needs_tag,
            Lemma => needs_lemma,
            Orth => needs_pos,
            _ => false,
        };
        if requested[index as usize] || needed_for_derivation {
            values[index as usize] = Some(CompactString::from(field));
        }
    });

    let tag = if needs_tag {
        let tag = push_pos_tag(
            [
                values[Pos1 as usize].as_deref(),
                values[Pos2 as usize].as_deref(),
                values[Pos3 as usize].as_deref(),
                values[Pos4 as usize].as_deref(),
            ]
            .into_iter()
            .flatten()
            .filter(|s| !s.is_empty() && *s != "*"),
        );
        if requested[Tag as usize] {
            values[Tag as usize] = Some(tag.clone());
        }
        Some(tag)
    } else {
        None
    };

    let orth_for_pos = if needs_pos {
        Some(
            values[Orth as usize]
                .take()
                .unwrap_or_else(|| CompactString::from(token.surface())),
        )
    } else {
        None
    };
    if requested[Orth as usize] {
        values[Orth as usize] = Some(CompactString::from(text_key(surface)));
    }
    if requested[Surface as usize] {
        values[Surface as usize] = Some(CompactString::from(surface));
    }

    if token.lex_type() == LexType::Unknown {
        for attribute in [OrthBase, Lemma] {
            if values[attribute as usize].is_none()
                && (requested[attribute as usize] || (attribute == Lemma && needs_lemma))
            {
                values[attribute as usize] = Some(CompactString::from(token.surface()));
            }
        }
    }

    // Computed after the fallback above and unchanged by it: the rule is
    // "the normalized lemma if present, else the surface", and the
    // fallback sets lemma to exactly that surface. This is the established
    // local convention the fallback generalises, not a new one.
    if requested[Norm as usize] {
        values[Norm as usize] = Some(match values[Lemma as usize].as_ref() {
            None => CompactString::from(token.surface()),
            Some(lemma) => normalized_lemma(lemma),
        });
    }
    if requested[LexTypeAttr as usize] {
        values[LexTypeAttr as usize] = Some(match token.lex_type() {
            LexType::System => CompactString::const_new("system"),
            LexType::User => CompactString::const_new("user"),
            LexType::Unknown => CompactString::const_new("unknown"),
        });
    }

    if needs_pos {
        let next_tag =
            next_token.map(|t| push_pos_tag(t.feature().split(',').take(4).filter(|&s| s != "*")));
        let orth = orth_for_pos
            .as_deref()
            .expect("orth_for_pos is computed whenever needs_pos is true");
        let tag = tag
            .as_deref()
            .expect("tag is computed whenever needs_pos is true");
        let (pos, _) = resolve_pos(orth, tag, next_tag.as_deref());
        assert!(
            !pos.is_empty(),
            "Empty 'pos' field encountered for surface {:?}",
            token.surface()
        );
        values[Pos as usize] = Some(pos);
    }

    for attribute in [Pos1, Pos2, Pos3, Pos4, Lemma, Orth] {
        if !requested[attribute as usize] {
            values[attribute as usize] = None;
        }
    }
    values
}

/// Tokenizes `text` with every attribute, folding it with
/// [`ViewFold::unprotected`].
pub fn tokenize(text: &str, tokenizer: &Tokenizer) -> Vec<TokenInfo> {
    tokenize_folded(text, tokenizer, &UNPROTECTED)
}

/// Tokenizes the `fold` view of `text` with every attribute.
fn tokenize_folded(text: &str, tokenizer: &Tokenizer, fold: &ViewFold) -> Vec<TokenInfo> {
    let mut worker = tokenizer.new_worker();
    let mut tokens = Vec::new();
    for_each_folded_token(
        &mut worker,
        text,
        fold,
        &[true; TokenAttributeIndex::COUNT],
        |_, values| tokens.push(TokenInfo::from_values(values)),
    );
    tokens
}

/// Tokenizes one sentence directly into the catalog's dense execution
/// projection, folding it with [`ViewFold::unprotected`].
pub fn tokenize_bound(
    text: &str,
    tokenizer: &Tokenizer,
    projection: &BoundProjection,
) -> Result<BoundTokenSentence, BoundTokenError> {
    let mut worker = tokenizer.new_worker();
    tokenize_worker_bound(&mut worker, text, &UNPROTECTED, projection).map(|(sentence, _)| sentence)
}

/// Tokenizes the `fold` view of `source` with lane-owned scratch state and
/// returns each token's byte range in `source`.
fn tokenize_worker_bound(
    worker: &mut Worker,
    source: &str,
    fold: &ViewFold,
    projection: &BoundProjection,
) -> Result<(BoundTokenSentence, Vec<std::ops::Range<usize>>), BoundTokenError> {
    let mut requested = [false; TokenAttributeIndex::COUNT];
    for attribute in projection.attributes() {
        requested[*attribute as usize] = true;
    }
    let mut values = Vec::new();
    let mut ranges = Vec::new();
    for_each_folded_token(
        worker,
        source,
        fold,
        &requested,
        |range, mut token_values| {
            ranges.push(range);
            values.extend(
                projection
                    .attributes()
                    .iter()
                    .map(|attribute| token_values[*attribute as usize].take()),
            );
        },
    );
    BoundTokenSentence::from_projected_values(projection, ranges.len(), values)
        .map(|sentence| (sentence, ranges))
}

/// Tokenizes the `fold` view of `source` and calls `visit` with each token's
/// byte range in `source` and its `requested` attributes, in token order.
///
/// A token boundary inside an alignment group of the view moves to the end of
/// the group, so the token that starts the group covers all of its source and
/// the next token's source range is empty.
fn for_each_folded_token(
    worker: &mut Worker,
    source: &str,
    fold: &ViewFold,
    requested: &[bool; TokenAttributeIndex::COUNT],
    mut visit: impl FnMut(std::ops::Range<usize>, [Option<CompactString>; TokenAttributeIndex::COUNT]),
) {
    let view = fold.fold(source);
    worker.reset_sentence(view.text());
    worker.tokenize();
    let token_count = worker.num_tokens();
    let unchanged = view.text() == source;
    let mut source_bytes = CodePointCursor::new(source);
    for index in 0..token_count {
        let token = worker.token(index);
        let range = if unchanged {
            token.range_byte()
        } else {
            let to_source = |position: usize| {
                let position = u32::try_from(position).expect("a view fits u32 code points");
                view.map()
                    .to_source(ViewSpan::new(position, position))
                    .end()
            };
            let chars = token.range_char();
            let start = source_bytes.byte_at(to_source(chars.start));
            let end = source_bytes.byte_at(to_source(chars.end));
            start..end
        };
        let next_token = (index + 1 < token_count).then(|| worker.token(index + 1));
        let surface = source
            .get(range.clone())
            .expect("mapped token ranges lie on character boundaries");
        let token_values = featurize_selected(&token, next_token.as_ref(), requested, surface);
        visit(range, token_values);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::token_match::TokenInfoTrait;
    use vibrato_rkyv::{Dictionary, SystemDictionaryBuilder};

    #[test]
    fn an_unreadable_lexicon_is_an_error_naming_the_dictionary_and_its_cause() {
        let cause = || {
            vibrato_rkyv::errors::VibratoError::StdIo(std::io::Error::other("trie is truncated"))
        };
        let error = fold_from_lexicons(std::path::Path::new("/dict/unidic.dic"), [Err(cause())])
            .expect_err("an unreadable lexicon builds no fold");
        assert_eq!(error.dictionary, std::path::Path::new("/dict/unidic.dic"));
        assert_eq!(
            error.to_string(),
            "cannot enumerate the lexicon of Vibrato dictionary /dict/unidic.dic"
        );
        assert_eq!(
            std::error::Error::source(&error).map(ToString::to_string),
            Some(cause().to_string())
        );
    }

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
        assert!(projection.ordinal(TokenAttributeIndex::Surface).is_none());
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
                .get(TokenAttributeIndex::Surface)
                .map(compact_str::CompactString::as_str),
            Some("表層")
        );
    }

    // Use a six-field UniDic unknown-word entry to exercise absent features
    // from `lform` onward.
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

    fn featurize_oov_token(surface: &str) -> TokenInfo {
        featurize_first(
            "表層,0,0,1,名詞,普通名詞,一般,*,*,*,ヒョウソウ,表層",
            "DEFAULT,0,0,100,名詞,普通名詞,一般,*,*,*",
            surface,
        )
    }

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
            let mut out = Vec::new();
            for_each_feature_field(record, |_, field| out.push(field.to_owned()));
            out
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
            TokenAttributeIndex::Surface,
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
