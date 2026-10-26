use std::{
    fs::File,
    io::Read,
    path::{Path, PathBuf},
    sync::Arc,
};

use compact_str::CompactString;
use sudachi::{
    analysis::{Mode, mlist::MorphemeList, stateful_tokenizer::StatefulTokenizer},
    config::{Config, PathResolver},
    dic::dictionary::JapaneseDictionary,
};

use crate::token_match::{BoundProjection, BoundTokenSentence, TokenSchema};

use super::{
    AnalysisError, AnalyzedInput, AnalyzedSentence, AnalyzerDescriptor, AnalyzerFamily,
    AnalyzerPlan, AnalyzerSession, ExtractionProfile, JapaneseSegmentationPolicy,
    NormalizationPolicy, SegmentationMode, SemanticAnalysisIdentity, SurfaceToken, TextUnit,
    TextUnitKind, ValidatedAnalysisBatch, analyzer::sealed, declared_token_schema,
};

const SUDACHI_REVISION: &[u8] = b"5de0410b6edfd428fc510cc9763440772e9619d5";

/// BLAKE3 of `resources/char.def` at [`SUDACHI_REVISION`], the copy compiled
/// into `sudachi`. A resource directory holding that file has the same digest,
/// so it produces the same analysis identity as [`SudachiResources::BuiltIn`].
const BUILT_IN_CHAR_DEF_DIGEST: [u8; 32] = [
    0x3f, 0x62, 0xc5, 0xa5, 0xdc, 0x26, 0x66, 0xc2, 0x96, 0xaa, 0x77, 0xc1, 0x55, 0x05, 0x40, 0x1f,
    0xce, 0x7e, 0xe5, 0xdc, 0x21, 0xcf, 0xe2, 0xa7, 0xda, 0x8d, 0x79, 0xf9, 0x8b, 0x64, 0x64, 0xa2,
];

/// Leading bytes of the block-description container, the one the pinned
/// `sudachi` revision reads.
const SUDACHI_BINARY_DESCRIPTION_MAGIC: &[u8] = b"SudachiBinaryDic";

/// Little-endian version words that open the earlier version-header container.
/// The dictionary's release date follows at offset 16.
const SUDACHI_HEADER_VERSION_WORDS: [u64; 2] = [0x7366_d3f1_8bd1_11e7, 0xce9f_011a_9239_4434];

/// Which on-disk container a Sudachi dictionary is written in.
///
/// A reader supports only one of the two containers. The pinned `sudachi` revision reads
/// the block-description container that starts with the ASCII magic
/// `SudachiBinaryDic`, and rejects the earlier version-header container with
/// `V0 version`, which names neither container. The load error therefore
/// names the container found.
enum DictionaryContainer {
    /// The version-header container the pinned revision cannot read.
    HeaderVersioned,
    /// The block-description container it reads.
    BinaryDescription,
}

/// Identifies a dictionary's container from its first bytes.
///
/// `None` means "not identified", never "unsupported": a missing file, a
/// truncated one, or bytes that are neither magic belongs to the loader, which
/// reports its own failure for those.
fn dictionary_container(path: &Path) -> Option<DictionaryContainer> {
    let mut header = [0u8; 16];
    let read = File::open(path).ok()?.read(&mut header).ok()?;
    let header = &header[..read];
    if header.starts_with(SUDACHI_BINARY_DESCRIPTION_MAGIC) {
        return Some(DictionaryContainer::BinaryDescription);
    }
    let version = u64::from_le_bytes(<[u8; 8]>::try_from(header.get(..8)?).ok()?);
    SUDACHI_HEADER_VERSION_WORDS
        .contains(&version)
        .then_some(DictionaryContainer::HeaderVersioned)
}

const ADAPTER_REVISION: &[u8] = b"lingrep.sudachi-adapter/v2";
const MAPPING_REVISION: &[u8] = b"lingrep.sudachi-pos-mapping/v1";
const ALIGNMENT_REVISION: &[u8] = b"lingrep.sudachi-byte-alignment/v1";

pub(super) const MORPHOLOGY_KEYS: [&str; 13] = [
    "text",
    "lemma",
    "norm",
    "upos",
    "xpos",
    "sudachi.cform",
    "sudachi.ctype",
    "sudachi.lextype",
    "sudachi.pos1",
    "sudachi.pos2",
    "sudachi.pos3",
    "sudachi.pos4",
    "sudachi.reading",
];

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum SudachiSplit {
    A,
    B,
    C,
}

impl SudachiSplit {
    const fn mode(self) -> Mode {
        match self {
            Self::A => Mode::A,
            Self::B => Mode::B,
            Self::C => Mode::C,
        }
    }

    const fn identity_byte(self) -> u8 {
        match self {
            Self::A => b'A',
            Self::B => b'B',
            Self::C => b'C',
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum SudachiEdition {
    Small,
    Core,
    Full,
}

impl SudachiEdition {
    const fn identity(self) -> &'static [u8] {
        match self {
            Self::Small => b"small",
            Self::Core => b"core",
            Self::Full => b"full",
        }
    }
}

/// Where Sudachi's `char.def`, `unk.def`, `rewrite.def` and `sudachi.json`
/// come from.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SudachiResources {
    /// The copies compiled into the pinned `sudachi` revision.
    BuiltIn,
    /// A directory holding the four files.
    Directory(PathBuf),
}

impl SudachiResources {
    pub fn directory(directory: impl Into<PathBuf>) -> Self {
        Self::Directory(directory.into())
    }

    fn char_def_digest(&self) -> Result<[u8; 32], SudachiLoadError> {
        match self {
            Self::BuiltIn => Ok(BUILT_IN_CHAR_DEF_DIGEST),
            Self::Directory(directory) => file_digest(&directory.join("char.def")),
        }
    }

    fn resolver(&self) -> PathResolver {
        match self {
            Self::BuiltIn => PathResolver::from_embedded(),
            Self::Directory(directory) => PathResolver::from_path(directory),
        }
    }
}

#[derive(Clone, Debug)]
pub struct SudachiConfig {
    dictionary_path: PathBuf,
    resources: SudachiResources,
    dictionary_digest: [u8; 32],
    resource_digest: [u8; 32],
    split: SudachiSplit,
    segmentation: SegmentationMode,
    normalization: NormalizationPolicy,
    schema: TokenSchema,
    descriptor: AnalyzerDescriptor,
}

impl SudachiConfig {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        dictionary_path: impl Into<PathBuf>,
        resources: SudachiResources,
        dictionary_digest: [u8; 32],
        resource_digest: [u8; 32],
        edition: SudachiEdition,
        split: SudachiSplit,
        profile: ExtractionProfile,
        segmentation: SegmentationMode,
        normalization: NormalizationPolicy,
    ) -> Result<Self, SudachiLoadError> {
        let dictionary_path = dictionary_path.into();
        if dictionary_path.as_os_str().is_empty()
            || matches!(&resources, SudachiResources::Directory(directory) if directory.as_os_str().is_empty())
            || dictionary_digest == [0; 32]
            || resource_digest == [0; 32]
        {
            return Err(SudachiLoadError::InvalidConfig);
        }
        let schema = declared_token_schema(AnalyzerFamily::Sudachi, profile)
            .ok_or_else(|| SudachiLoadError::Schema("Sudachi declares no such profile".into()))?;

        let implementation_digest = digest(b"implementation", [SUDACHI_REVISION, ADAPTER_REVISION]);
        let resource_manifest_digest = digest(
            b"resources",
            [edition.identity(), &dictionary_digest, &resource_digest],
        );
        let projection_policy_digest = digest(b"projection", [MAPPING_REVISION]);
        let alignment_policy_digest = digest(b"alignment", [ALIGNMENT_REVISION]);
        let split_identity = [split.identity_byte()];
        let output_config_digest = digest(b"output", [edition.identity(), &split_identity]);
        let component_closure_digest = digest(b"components", [ADAPTER_REVISION]);
        let boundary_policy = match segmentation {
            SegmentationMode::Input => "input-v1",
            SegmentationMode::Analyzer => "ja-delimiter-v1",
        };
        let descriptor = AnalyzerDescriptor::new(
            AnalyzerFamily::Sudachi,
            profile,
            segmentation,
            normalization,
            "ja",
            boundary_policy,
            implementation_digest,
            resource_manifest_digest,
            projection_policy_digest,
            alignment_policy_digest,
            component_closure_digest,
            output_config_digest,
            schema.digest(),
        )
        .map_err(|_| SudachiLoadError::InvalidConfig)?;
        Ok(Self {
            dictionary_path,
            resources,
            dictionary_digest,
            resource_digest,
            split,
            segmentation,
            normalization,
            schema,
            descriptor,
        })
    }

    #[must_use]
    pub fn schema(&self) -> &TokenSchema {
        &self.schema
    }

    #[must_use]
    pub fn semantic_identity(&self) -> SemanticAnalysisIdentity {
        self.descriptor.semantic_identity()
    }

    /// Returns the fixed semantic description this configuration analyzes under.
    ///
    /// Available before a dictionary is loaded, which is what lets an identity
    /// or addressing decision be made without paying for a load.
    #[must_use]
    pub const fn descriptor(&self) -> &AnalyzerDescriptor {
        &self.descriptor
    }

    /// Hashes the exact dictionary and `char.def` consumed by the minimal config.
    pub fn content_digests(
        dictionary_path: impl AsRef<Path>,
        resources: &SudachiResources,
    ) -> Result<([u8; 32], [u8; 32]), SudachiLoadError> {
        Ok((
            file_digest(dictionary_path.as_ref())?,
            resources.char_def_digest()?,
        ))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SudachiLoadError {
    #[error("Sudachi paths and content digests must be nonempty")]
    InvalidConfig,
    #[error("invalid Sudachi schema: {0}")]
    Schema(String),
    #[error("cannot load Sudachi dictionary: {0}")]
    Dictionary(String),
    #[error("cannot read Sudachi content at {path}: {message}")]
    ContentIo { path: PathBuf, message: String },
    #[error("Sudachi {0} content digest does not match its configured identity")]
    ContentMismatch(&'static str),
}

#[derive(Clone)]
pub struct SudachiPlan {
    dictionary: Arc<JapaneseDictionary>,
    schema: TokenSchema,
    descriptor: AnalyzerDescriptor,
    split: SudachiSplit,
    segmentation: SegmentationMode,
    normalization: NormalizationPolicy,
}

impl SudachiPlan {
    pub fn load(config: SudachiConfig) -> Result<Self, SudachiLoadError> {
        let (dictionary_digest, resource_digest) =
            SudachiConfig::content_digests(&config.dictionary_path, &config.resources)?;
        if dictionary_digest != config.dictionary_digest {
            return Err(SudachiLoadError::ContentMismatch("dictionary"));
        }
        if resource_digest != config.resource_digest {
            return Err(SudachiLoadError::ContentMismatch("resource"));
        }
        let dictionary_config = Config::minimal_at(config.resources.resolver())
            .with_system_dic(&config.dictionary_path);
        let dictionary = JapaneseDictionary::from_cfg(&dictionary_config).map_err(|error| {
            SudachiLoadError::Dictionary(match dictionary_container(&config.dictionary_path) {
                Some(DictionaryContainer::HeaderVersioned) => format!(
                    "{error}; {} is in the version-header container, and this build reads \
                     only the `SudachiBinaryDic` container. Download the SudachiDict \
                     v20261015 or later archive from \
                     https://github.com/WorksApplications/SudachiDict/releases",
                    config.dictionary_path.display()
                ),
                _ => error.to_string(),
            })
        })?;
        Ok(Self {
            dictionary: Arc::new(dictionary),
            schema: config.schema,
            descriptor: config.descriptor,
            split: config.split,
            segmentation: config.segmentation,
            normalization: config.normalization,
        })
    }

    #[must_use]
    pub const fn descriptor(&self) -> &AnalyzerDescriptor {
        &self.descriptor
    }
}

impl sealed::Plan for SudachiPlan {}

// The pinned sudachi input_text::buffer uses u16 offsets and caps input at u16::MAX / 4 * 3 bytes.
const SUDACHI_MAX_INPUT_BYTES: usize = u16::MAX as usize / 4 * 3;

fn chunk_end(text: &str) -> usize {
    if text.len() <= SUDACHI_MAX_INPUT_BYTES {
        return text.len();
    }
    let mut end = SUDACHI_MAX_INPUT_BYTES;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.get(..end)
        .and_then(|prefix| {
            prefix.char_indices().rev().find(|(offset, ch)| {
                *offset >= end / 2
                    && (ch.is_whitespace() || matches!(ch, '、' | '，' | ',' | '；' | ';'))
            })
        })
        .map_or(end, |(offset, ch)| offset + ch.len_utf8())
}

impl AnalyzerPlan for SudachiPlan {
    type Session = SudachiSession;

    fn schema(&self) -> &TokenSchema {
        &self.schema
    }

    fn create_session(&self) -> Self::Session {
        SudachiSession {
            tokenizer: StatefulTokenizer::new(Arc::clone(&self.dictionary), self.split.mode()),
            results: MorphemeList::empty(Arc::clone(&self.dictionary)),
            schema: self.schema.clone(),
            split: self.split,
            segmentation: self.segmentation,
            normalization: self.normalization,
        }
    }
}

pub struct SudachiSession {
    tokenizer: StatefulTokenizer<Arc<JapaneseDictionary>>,
    results: MorphemeList<Arc<JapaneseDictionary>>,
    schema: TokenSchema,
    split: SudachiSplit,
    segmentation: SegmentationMode,
    normalization: NormalizationPolicy,
}

impl sealed::Session for SudachiSession {}

impl AnalyzerSession for SudachiSession {
    fn schema(&self) -> &TokenSchema {
        &self.schema
    }

    fn analyze_bounded_batch(
        &mut self,
        inputs: Vec<TextUnit>,
        projection: &BoundProjection,
    ) -> Result<ValidatedAnalysisBatch, AnalysisError> {
        if projection.schema_digest() != self.schema.digest() {
            return Err(AnalysisError::UnsupportedProjection);
        }
        let fields = bind_fields(projection)?;
        let needs_pos = fields.iter().any(|field| field.needs_pos());
        let mut analyzed_inputs = Vec::with_capacity(inputs.len());
        for input in inputs {
            if input.normalization() != self.normalization {
                return Err(AnalysisError::InvalidAlignment);
            }
            let expected_kind = match self.segmentation {
                SegmentationMode::Input => TextUnitKind::Sentence,
                SegmentationMode::Analyzer => TextUnitKind::Segment,
            };
            if input.kind() != expected_kind {
                return Err(AnalysisError::WrongInputKind);
            }
            let ranges = match self.segmentation {
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
            let mut sentences = Vec::with_capacity(ranges.len());
            for (ordinal, sentence_range) in ranges.into_vec().into_iter().enumerate() {
                let sentence_text = input
                    .slice(sentence_range)
                    .map_err(|_| AnalysisError::InvalidSourceRange)?;
                let mut values = Vec::new();
                let mut surfaces = Vec::new();
                let mut chunk_start = 0;
                while chunk_start < sentence_text.len() {
                    let remaining = sentence_text
                        .get(chunk_start..)
                        .ok_or(AnalysisError::InvalidSourceRange)?;
                    let length = chunk_end(remaining);
                    let sentence_text = remaining
                        .get(..length)
                        .ok_or(AnalysisError::InvalidSourceRange)?;
                    self.tokenizer.set_mode(self.split.mode());
                    self.tokenizer.reset().push_str(sentence_text);
                    self.tokenizer
                        .do_tokenize()
                        .map_err(|error| AnalysisError::ProviderFailure(error.to_string()))?;
                    self.results
                        .collect_results(&mut self.tokenizer)
                        .map_err(|error| AnalysisError::ProviderFailure(error.to_string()))?;

                    let token_count = self.results.len();
                    surfaces.reserve(token_count);
                    values.reserve(
                        token_count
                            .checked_mul(fields.len())
                            .ok_or(AnalysisError::ResourceAccounting)?,
                    );
                    let word_offset = surfaces.len();
                    let sentence_start = sentence_range.start() as usize + chunk_start;
                    for (word_index, morpheme) in self.results.iter().enumerate() {
                        let start = sentence_start
                            .checked_add(morpheme.begin())
                            .ok_or(AnalysisError::ResourceAccounting)?;
                        let end = sentence_start
                            .checked_add(morpheme.end())
                            .ok_or(AnalysisError::ResourceAccounting)?;
                        let source_range = input
                            .source_range(start, end)
                            .map_err(|_| AnalysisError::InvalidSourceRange)?;
                        let surface = morpheme.surface();
                        if input.slice(source_range).ok() != Some(surface.as_ref()) {
                            return Err(AnalysisError::InvalidAlignment);
                        }
                        let word_start = u32::try_from(word_offset + word_index)
                            .map_err(|_| AnalysisError::ResourceAccounting)?;
                        surfaces.push(SurfaceToken::new(
                            source_range,
                            word_start,
                            word_start
                                .checked_add(1)
                                .ok_or(AnalysisError::ResourceAccounting)?,
                        ));
                        let pos = needs_pos.then(|| morpheme.part_of_speech());
                        values.extend(fields.iter().map(|field| {
                            Some(field.value(&morpheme, pos.unwrap_or(&[]), &surface))
                        }));
                    }
                    chunk_start += length;
                }
                let token_count = surfaces.len();
                let words =
                    BoundTokenSentence::from_projected_values(projection, token_count, values)
                        .map_err(|_| AnalysisError::ResourceAccounting)?;
                sentences.push(AnalyzedSentence::new(
                    u32::try_from(ordinal).map_err(|_| AnalysisError::ResourceAccounting)?,
                    sentence_range,
                    surfaces.into_boxed_slice(),
                    words,
                )?);
            }
            analyzed_inputs.push(AnalyzedInput::new(input, sentences, self.segmentation)?);
        }
        ValidatedAnalysisBatch::new(analyzed_inputs)
    }
}

#[derive(Clone, Copy)]
enum Field {
    Form,
    Lemma,
    Norm,
    Upos,
    Xpos,
    CForm,
    CType,
    LexType,
    Pos1,
    Pos2,
    Pos3,
    Pos4,
    Reading,
}

impl Field {
    const fn needs_pos(self) -> bool {
        matches!(
            self,
            Self::Upos
                | Self::Xpos
                | Self::CForm
                | Self::CType
                | Self::Pos1
                | Self::Pos2
                | Self::Pos3
                | Self::Pos4
        )
    }

    fn value(
        self,
        morpheme: &sudachi::analysis::morpheme::Morpheme<'_, Arc<JapaneseDictionary>>,
        pos: &[String],
        surface: &str,
    ) -> CompactString {
        match self {
            Self::Form => surface.into(),
            Self::Lemma => morpheme.dictionary_form().into(),
            Self::Norm => morpheme.normalized_form().into(),
            Self::Upos => sudachi_upos(pos).into(),
            Self::Xpos => xpos(pos),
            Self::CForm => pos_value(pos, 5).into(),
            Self::CType => pos_value(pos, 4).into(),
            Self::LexType => lex_type(morpheme),
            Self::Pos1 => pos_value(pos, 0).into(),
            Self::Pos2 => pos_value(pos, 1).into(),
            Self::Pos3 => pos_value(pos, 2).into(),
            Self::Pos4 => pos_value(pos, 3).into(),
            Self::Reading => morpheme.reading_form().into(),
        }
    }
}

fn bind_fields(projection: &BoundProjection) -> Result<Vec<Field>, AnalysisError> {
    projection
        .keys()
        .iter()
        .map(|key| match key.as_str() {
            "text" => Ok(Field::Form),
            "lemma" => Ok(Field::Lemma),
            "norm" => Ok(Field::Norm),
            "upos" => Ok(Field::Upos),
            "xpos" => Ok(Field::Xpos),
            "sudachi.cform" => Ok(Field::CForm),
            "sudachi.ctype" => Ok(Field::CType),
            "sudachi.lextype" => Ok(Field::LexType),
            "sudachi.pos1" => Ok(Field::Pos1),
            "sudachi.pos2" => Ok(Field::Pos2),
            "sudachi.pos3" => Ok(Field::Pos3),
            "sudachi.pos4" => Ok(Field::Pos4),
            "sudachi.reading" => Ok(Field::Reading),
            _ => Err(AnalysisError::UnsupportedProjection),
        })
        .collect()
}

fn pos_value(pos: &[String], index: usize) -> &str {
    pos.get(index).map_or("*", String::as_str)
}

fn sudachi_upos(pos: &[String]) -> &'static str {
    match (pos_value(pos, 0), pos_value(pos, 1), pos_value(pos, 2)) {
        ("名詞", "固有名詞", _) => "PROPN",
        ("名詞", "数詞", _) => "NUM",
        ("名詞", "代名詞", _) | ("代名詞", _, _) => "PRON",
        ("名詞", "助動詞語幹", _) => "AUX",
        ("名詞", _, _) => "NOUN",
        ("動詞", "非自立可能", _) => "AUX",
        ("動詞", _, _) => "VERB",
        ("形状詞", "助動詞語幹", _) => "AUX",
        ("形容詞" | "形状詞", _, _) => "ADJ",
        ("副詞", _, _) => "ADV",
        ("助詞", "格助詞" | "係助詞" | "副助詞", _) => "ADP",
        ("助詞", "終助詞", _) => "PART",
        ("助詞", "準体助詞" | "接続助詞", _) => "SCONJ",
        ("助動詞", _, _) => "AUX",
        ("接続詞", _, _) => "CCONJ",
        ("接頭辞", _, _) => "NOUN",
        ("接尾辞", "名詞的", _) => "NOUN",
        ("接尾辞", "形容詞的", _) => "AUX",
        ("接尾辞", "形状詞的" | "動詞的", _) => "PART",
        ("連体詞", _, _) => "DET",
        ("感動詞", _, _) => "INTJ",
        ("補助記号", "ＡＡ", "顔文字") => "PUNCT",
        ("補助記号", "ＡＡ" | "一般", _) => "SYM",
        ("補助記号", _, _) => "PUNCT",
        ("記号", _, _) => "NOUN",
        _ => "X",
    }
}

fn xpos(pos: &[String]) -> CompactString {
    pos.iter()
        .take(4)
        .map(String::as_str)
        .filter(|value| !value.is_empty() && *value != "*")
        .collect::<Vec<_>>()
        .join("-")
        .into()
}

fn lex_type(
    morpheme: &sudachi::analysis::morpheme::Morpheme<'_, Arc<JapaneseDictionary>>,
) -> CompactString {
    match morpheme.dictionary_id() {
        -1 => "oov".into(),
        0 => "system".into(),
        dictionary => format!("user-{dictionary}").into(),
    }
}

fn digest<'a, I>(domain: &[u8], parts: I) -> [u8; 32]
where
    I: IntoIterator<Item = &'a [u8]>,
{
    let mut hash = blake3::Hasher::new();
    hash.update(b"lingrep.sudachi-config/v1");
    hash.update(&(domain.len() as u64).to_le_bytes());
    hash.update(domain);
    for part in parts {
        hash.update(&(part.len() as u64).to_le_bytes());
        hash.update(part);
    }
    *hash.finalize().as_bytes()
}

fn file_digest(path: &Path) -> Result<[u8; 32], SudachiLoadError> {
    let mut file = std::fs::File::open(path).map_err(|error| SudachiLoadError::ContentIo {
        path: path.to_path_buf(),
        message: error.to_string(),
    })?;
    let mut hash = blake3::Hasher::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| SudachiLoadError::ContentIo {
                path: path.to_path_buf(),
                message: error.to_string(),
            })?;
        if read == 0 {
            break;
        }
        hash.update(&buffer[..read]);
    }
    Ok(*hash.finalize().as_bytes())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn pos_mapping_is_total_and_provider_owned() {
        assert_eq!(sudachi_upos(&["名詞".into(), "固有名詞".into()]), "PROPN");
        assert_eq!(sudachi_upos(&["動詞".into()]), "VERB");
        assert_eq!(sudachi_upos(&["動詞".into(), "非自立可能".into()]), "AUX");
        assert_eq!(sudachi_upos(&["助詞".into(), "終助詞".into()]), "PART");
        assert_eq!(sudachi_upos(&["助詞".into(), "接続助詞".into()]), "SCONJ");
        assert_eq!(sudachi_upos(&["補助記号".into(), "一般".into()]), "SYM");
        assert_eq!(sudachi_upos(&["接頭辞".into()]), "NOUN");
        assert_eq!(sudachi_upos(&["未知".into()]), "X");
        assert_eq!(
            xpos(&["名詞".into(), "普通名詞".into(), "*".into()]),
            "名詞-普通名詞"
        );
    }

    #[test]
    fn the_identity_revision_is_the_locked_sudachi_revision() {
        let revision = std::str::from_utf8(SUDACHI_REVISION).expect("ASCII revision");
        let locked = format!("sudachi.rs.git?rev={revision}");
        assert!(
            include_str!("../../../Cargo.lock").contains(&locked),
            "Cargo.lock does not pin {locked}"
        );
        let flake_input = format!("github:WorksApplications/sudachi.rs/{revision}");
        assert!(
            include_str!("../../../flake.nix").contains(&flake_input),
            "flake.nix does not pin {flake_input}"
        );
    }

    /// Builds a config whose digests match the given files, so that the
    /// dictionary load itself is what the test observes.
    fn config_for(directory: &Path, dictionary: &Path) -> SudachiConfig {
        let resources = directory.join("resources");
        fs::create_dir(&resources).expect("create the resource directory");
        fs::write(resources.join("char.def"), b"\n").expect("write char.def");
        let (dictionary_digest, resource_digest) =
            SudachiConfig::content_digests(dictionary, &SudachiResources::directory(&resources))
                .expect("content digests");
        SudachiConfig::new(
            dictionary,
            SudachiResources::directory(&resources),
            dictionary_digest,
            resource_digest,
            SudachiEdition::Core,
            SudachiSplit::C,
            ExtractionProfile::Morphology,
            SegmentationMode::Input,
            NormalizationPolicy::None,
        )
        .expect("a matching config")
    }

    /// A container this build cannot read is named, with its recovery.
    #[test]
    fn an_older_dictionary_container_is_named_in_the_load_error() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let dictionary = directory.path().join("system_core.dic");
        let mut bytes = SUDACHI_HEADER_VERSION_WORDS[1].to_le_bytes().to_vec();
        bytes.extend_from_slice(&[0; 8]);
        bytes.extend_from_slice(b"20261015");
        fs::write(&dictionary, &bytes).expect("write the dictionary");

        let message = SudachiPlan::load(config_for(directory.path(), &dictionary))
            .err()
            .expect("the container is unreadable")
            .to_string();
        assert!(message.contains("version-header container"), "{message}");
        assert!(message.contains("v20261015"), "{message}");
    }

    /// A truncated dictionary is an error, not a panic.
    #[test]
    fn a_truncated_dictionary_is_rejected_rather_than_panicking() {
        let mut header = SUDACHI_BINARY_DESCRIPTION_MAGIC.to_vec();
        header.extend_from_slice(&1u64.to_le_bytes());
        for length in 0..=header.len() {
            let directory = tempfile::tempdir().expect("temporary directory");
            let dictionary = directory.path().join("system_core.dic");
            fs::write(&dictionary, &header[..length]).expect("write");
            assert!(
                SudachiPlan::load(config_for(directory.path(), &dictionary)).is_err(),
                "a {length}-byte dictionary loaded"
            );
        }
    }

    /// Identification is by first bytes only; anything else stays the loader's
    /// failure to report.
    #[test]
    fn a_dictionary_container_is_identified_by_its_opening_bytes() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("dictionary.dic");

        fs::write(&path, SUDACHI_HEADER_VERSION_WORDS[1].to_le_bytes()).expect("write");
        assert!(matches!(
            dictionary_container(&path),
            Some(DictionaryContainer::HeaderVersioned)
        ));

        let mut newer = SUDACHI_BINARY_DESCRIPTION_MAGIC.to_vec();
        newer.extend_from_slice(&1u64.to_le_bytes());
        fs::write(&path, &newer).expect("write");
        assert!(matches!(
            dictionary_container(&path),
            Some(DictionaryContainer::BinaryDescription)
        ));

        fs::write(&path, [0u8; 16]).expect("write");
        assert!(dictionary_container(&path).is_none());

        fs::write(&path, b"abc").expect("write");
        assert!(dictionary_container(&path).is_none());

        fs::remove_file(&path).expect("remove");
        assert!(dictionary_container(&path).is_none());
    }
}
