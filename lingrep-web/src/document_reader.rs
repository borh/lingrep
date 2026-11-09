//! Documents of a prepared Markdown corpus shown as `ling corpus render`
//! rendered them, from the reading sidecars bound at startup.

use std::ops::Range;
use std::path::PathBuf;
use std::sync::Arc;

use askama::Template;
use lingrep_core::corpus::{DocumentId, SentenceId};
use lingrep_corpus::{
    CorpusArtifact, HighlightRange, PageRequest, ReadingPage, ReadingSidecar, ReadingSidecarError,
    ReadingSidecarKey, StoredSentenceAnalysis,
};

use crate::corpus_set::CorpusName;

/// Most HTML bytes of rendered blocks one page holds, unless its first block
/// alone is larger.
pub(crate) const PAGE_BYTES: usize = 64 * 1024;

/// The sidecars `--reading-sidecar` names, each bound to the declared
/// prepared corpora whose key it records.
#[derive(Default)]
pub struct ReadingSidecars {
    bound: Vec<Arc<ReadingSidecar>>,
}

impl ReadingSidecars {
    /// Opens and verifies each sidecar in `paths` and binds it to the
    /// prepared corpora in `declared` with its analysis identity.
    ///
    /// # Errors
    ///
    /// A message naming the sidecar that cannot be read, does not verify,
    /// was written for another source identity or renderer revision, matches
    /// no declared corpus, or renders the same corpus as an earlier sidecar.
    pub fn bind(
        paths: &[PathBuf],
        declared: &[(CorpusName, PathBuf, String)],
    ) -> Result<Self, String> {
        if paths.is_empty() {
            return Ok(Self::default());
        }
        let mut corpora = Vec::new();
        for (name, path, _) in declared {
            if lingrep_corpus::is_prepared_artifact(path) {
                let artifact = CorpusArtifact::open(path)
                    .map_err(|error| format!("corpus {:?}: {error}", name.as_str()))?;
                corpora.push((name, ReadingSidecarKey::for_artifact(&artifact)));
            }
        }
        let mut sidecars = Self::default();
        for path in paths {
            let named =
                |message: String| format!("--reading-sidecar {}: {message}", path.display());
            let key = ReadingSidecar::read_key(path).map_err(|error| named(error.to_string()))?;
            let Some((name, expected)) = corpora
                .iter()
                .find(|(_, expected)| expected.analysis_identity == key.analysis_identity)
            else {
                return Err(named(unmatched(&key, &corpora)));
            };
            let sidecar =
                ReadingSidecar::open(path, expected).map_err(|error| named(error.to_string()))?;
            sidecar
                .verify_digest()
                .map_err(|error| named(error.to_string()))?;
            if let Some(other) = sidecars.bound.iter().find(|other| other.key() == expected) {
                return Err(format!(
                    "--reading-sidecar {} and --reading-sidecar {} both render corpus {:?}",
                    other.path().display(),
                    path.display(),
                    name.as_str()
                ));
            }
            sidecars.bound.push(Arc::new(sidecar));
        }
        Ok(sidecars)
    }

    /// The sidecar bound to `artifact`.
    pub(crate) fn for_artifact(&self, artifact: &CorpusArtifact) -> Option<Arc<ReadingSidecar>> {
        if self.bound.is_empty() {
            return None;
        }
        let key = ReadingSidecarKey::for_artifact(artifact);
        self.bound
            .iter()
            .find(|sidecar| sidecar.key() == &key)
            .map(Arc::clone)
    }
}

fn unmatched(key: &ReadingSidecarKey, corpora: &[(&CorpusName, ReadingSidecarKey)]) -> String {
    let served = if corpora.is_empty() {
        "no served corpus is a prepared .lingcorpus artifact".to_owned()
    } else {
        corpora
            .iter()
            .map(|(name, expected)| {
                format!(
                    "corpus {:?} has analysis identity {}",
                    name.as_str(),
                    expected.analysis_identity
                )
            })
            .collect::<Vec<_>>()
            .join(", ")
    };
    format!(
        "matches no served corpus: it renders analysis identity {}, and {served}. \
         A corpus prepared again with another analysis needs `ling corpus render` again",
        key.analysis_identity
    )
}

/// The characters of `analysis.text` that the words `words` of its sentence
/// cover, or `None` when some of those words have no source text.
pub(crate) fn char_range(
    analysis: &StoredSentenceAnalysis,
    words: Range<usize>,
) -> Option<Range<u32>> {
    let bytes = crate::reading::source_highlight(analysis, words)?;
    let start = analysis.text.get(..bytes.start)?.chars().count();
    let length = analysis.text.get(bytes)?.chars().count();
    Some(u32::try_from(start).ok()?..u32::try_from(start + length).ok()?)
}

/// How much of an occurrence the rendered document shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Anchoring {
    Exact,
    Partial,
    Missing,
    /// The occurrence's sentence exceeds
    /// [`crate::reading::MAX_READING_SENTENCE_BYTES`], so it is not located.
    OverLimit,
}

/// Consecutive rendered blocks, with the sentences the search scope excludes
/// and the controls that load the blocks before and after them.
#[derive(Template)]
#[template(path = "fragments/document_pages.html")]
pub(crate) struct DocumentPages {
    /// The reader's document address, which takes `blocks_before` or
    /// `blocks_from`.
    pub root_url: String,
    pub earlier: Option<u32>,
    pub later: Option<u32>,
    pub html: String,
    /// `[container, UTF-16 start, UTF-16 end]` triples, as JSON.
    pub excluded: String,
}

impl DocumentPages {
    /// `page`, with the sentences not `admitted` marked, and the controls
    /// for the blocks `earlier` and `later` than it when it has them.
    pub fn of(
        root_url: String,
        page: ReadingPage,
        admitted: &[bool],
        earlier: bool,
        later: bool,
    ) -> Self {
        let excluded = page
            .anchors
            .iter()
            .zip(admitted)
            .filter(|(_, admitted)| !**admitted)
            .flat_map(|(anchor, _)| &anchor.ranges);
        Self {
            root_url,
            earlier: match page.previous {
                Some(PageRequest::Before(_, block)) if earlier => Some(block),
                _ => None,
            },
            later: match page.next {
                Some(PageRequest::From(_, block)) if later => Some(block),
                _ => None,
            },
            excluded: ranges_json(excluded),
            html: page.html,
        }
    }
}

/// A document's rendered page around an occurrence.
pub(crate) struct RenderedDocument {
    /// `[container, UTF-16 start, UTF-16 end]` triples, as JSON.
    pub highlight: String,
    pub anchoring: Anchoring,
    /// Whether a search scope excludes some sentences of the corpus.
    pub scoped: bool,
    pub pages: DocumentPages,
}

impl RenderedDocument {
    pub fn pages_html(&self) -> String {
        self.pages.render().unwrap_or_default()
    }
}

fn ranges_json<'a>(ranges: impl Iterator<Item = &'a HighlightRange>) -> String {
    let triples: Vec<[u32; 3]> = ranges
        .map(|range| [range.container, range.utf16_start, range.utf16_end])
        .collect();
    serde_json::to_string(&triples).unwrap_or_else(|_| "[]".into())
}

/// The sentences with rendered text on `page`, in sentence order.
pub(crate) fn sentences(page: &ReadingPage) -> Vec<SentenceId> {
    page.anchors.iter().map(|anchor| anchor.sentence).collect()
}

/// The page of `sentence`'s document around it, the highlight ranges of the
/// characters `chars` of its stored text on that page, and how much of them
/// the page shows.
pub(crate) fn open(
    sidecar: &ReadingSidecar,
    sentence: u64,
    chars: Option<Range<u32>>,
) -> Result<(ReadingPage, String, Anchoring), ReadingSidecarError> {
    let sentence = SentenceId::new(sentence);
    let Some(chars) = chars else {
        let page = sidecar.page(PageRequest::Around(sentence), PAGE_BYTES)?;
        return Ok((page, "[]".into(), Anchoring::Missing));
    };
    let (page, occurrence) = sidecar.page_at(sentence, chars, PAGE_BYTES)?;
    let anchoring = if occurrence.exact {
        Anchoring::Exact
    } else if occurrence.ranges.is_empty() {
        Anchoring::Missing
    } else {
        Anchoring::Partial
    };
    Ok((page, ranges_json(occurrence.ranges.iter()), anchoring))
}

/// The blocks of `document` before `block`, or from `block` on.
pub(crate) fn turn(
    sidecar: &ReadingSidecar,
    document: DocumentId,
    before: bool,
    block: u32,
) -> Result<ReadingPage, ReadingSidecarError> {
    let request = if before {
        PageRequest::Before(document, block)
    } else {
        PageRequest::From(document, block)
    };
    sidecar.page(request, PAGE_BYTES)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn char_ranges_count_characters_not_bytes() {
        let analysis = StoredSentenceAnalysis {
            text: "猫😺が いる。".into(),
            surfaces: Box::new([
                lingrep_corpus::StoredSurfaceAlignment {
                    source_range: 0..3,
                    word_range: 0..1,
                },
                lingrep_corpus::StoredSurfaceAlignment {
                    source_range: 3..10,
                    word_range: 1..2,
                },
                lingrep_corpus::StoredSurfaceAlignment {
                    source_range: 11..17,
                    word_range: 2..3,
                },
            ]),
            input_unit_id: None,
            input_kind: None,
            normalization: None,
            input_identity: None,
            source_range: None,
        };
        assert_eq!(char_range(&analysis, 1..2), Some(1..3));
        assert_eq!(char_range(&analysis, 1..3), Some(1..6));
        assert_eq!(char_range(&analysis, 3..4), None);
    }
}
