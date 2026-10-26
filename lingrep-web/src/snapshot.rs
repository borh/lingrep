use std::ops::Range;

use crate::corpus_runtime::CorpusIdentity;
use crate::revision::RevisionId;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PatternKey(u32);

impl PatternKey {
    pub fn new(index: usize) -> Option<Self> {
        u32::try_from(index).ok().map(Self)
    }

    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PatternMetadataValue {
    String(String),
    Number(String),
    Bool(bool),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompoundCategory {
    Compound,
    SourceCategories,
}

/// Where a published pattern came from. A compound is identified by its
/// ordered component keys; its `複合項目〔A ＋ B〕` name is a label.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PatternOrigin {
    /// A definition the run's catalog declares.
    Authored {
        /// Occurrences that are a component of at least one chain. Zero
        /// unless the run derived compounds.
        chained_total: u64,
    },
    /// One contiguous chain of two or more adjacent base matches, in order.
    /// Components are authored patterns, never another compound.
    Compound {
        components: Box<[PatternKey]>,
        /// Per authored pattern, this compound's occurrences whose span is
        /// identical to one of that pattern's published matches in the same
        /// sentence; descending by count, then by key.
        coincident: Box<[(PatternKey, u64)]>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatternDescriptor {
    pub key: PatternKey,
    pub name: String,
    pub exact_total: u64,
    pub metadata: Box<[(String, PatternMetadataValue)]>,
    pub origin: PatternOrigin,
}

impl PatternDescriptor {
    /// The ordered component keys of a compound; `None` for an authored
    /// pattern.
    pub fn components(&self) -> Option<&[PatternKey]> {
        match &self.origin {
            PatternOrigin::Authored { .. } => None,
            PatternOrigin::Compound { components, .. } => Some(components),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CaptureKey(u32);

impl CaptureKey {
    pub fn new(index: usize) -> Option<Self> {
        u32::try_from(index).ok().map(Self)
    }

    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureDescriptor {
    pub key: CaptureKey,
    pub pattern_key: PatternKey,
    pub name: String,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureSpanRef {
    pub capture_key: CaptureKey,
    pub span: Range<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatchRef {
    pub sentence_index: u64,
    pub span: Range<usize>,
    pub pattern_key: PatternKey,
    pub captures: Box<[CaptureSpanRef]>,
    pub graph_bindings: Box<[lingrep_core::fsm::GraphBinding]>,
    pub lexical: Option<LexicalMatchRef>,
    /// Whether this match is a component of a derived chain. `false` for a
    /// compound's own match and whenever the run derived no compounds.
    pub in_chain: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LexicalMatchRef {
    pub term: String,
    pub matched_by: Box<[String]>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletedSnapshot {
    pub search_scope: crate::search_scope::SearchScope,
    pub excluded_sentences: u64,
    pub revision: RevisionId,
    pub corpus_identity: CorpusIdentity,
    pub source: std::sync::Arc<str>,
    pub derive_compounds: bool,
    pub compound_category: CompoundCategory,
    pub patterns: Box<[PatternDescriptor]>,
    pub match_refs: Box<[MatchRef]>,
    pub capture_catalog: Box<[CaptureDescriptor]>,
    /// Exact per-`(pattern, section)` sentence counts, or the reason there are
    /// none. Run-result data rather than catalog description, which is why it
    /// hangs here and not off `PatternDescriptor`.
    pub sections: crate::section_tally::SectionAvailability,
    /// Sentences the sink observed. Checked against `sentences_matched` before
    /// publication; see `resolve_scan`.
    pub observed_sentences: u64,
    /// Observed sentences with at least one published match, counted once per
    /// sentence.
    pub matched_sentences: u64,
    pub documents: Option<DocumentSummary>,
    /// Sentences the matcher processed, from `ExecutionReport`.
    pub sentences_matched: u64,
    /// Sentences omitted as oversized (`TokenizedAdmission::Omit`), when any.
    /// A completed run carrying this is degraded-but-published; see the scan's
    /// banner and the 2026-08-11 robustness plan for the divergence
    /// argument.
    pub oversized_omissions: Option<lingrep_core::execution::OversizedSentenceOmissions>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocumentReach {
    /// Distinct documents with at least one occurrence.
    pub documents: u64,
    /// The largest per-document occurrence counts, descending, at most five.
    pub top_counts: Box<[u64]>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocumentSummary {
    /// Documents with at least one scanned sentence.
    pub documents_scanned: u64,
    pub by_pattern: Box<[DocumentReach]>,
}

impl DocumentSummary {
    pub fn reach(&self, key: PatternKey) -> Option<&DocumentReach> {
        self.by_pattern.get(key.index())
    }
}

impl CompletedSnapshot {
    pub fn pattern(&self, key: PatternKey) -> Option<&PatternDescriptor> {
        self.patterns
            .get(key.index())
            .filter(|descriptor| descriptor.key == key)
    }

    pub fn match_ref(&self, key: PatternKey, ordinal: usize) -> Option<&MatchRef> {
        self.match_refs
            .iter()
            .filter(|reference| reference.pattern_key == key)
            .nth(ordinal)
    }

    pub fn capture(&self, key: CaptureKey) -> Option<&CaptureDescriptor> {
        self.capture_catalog
            .get(key.index())
            .filter(|descriptor| descriptor.key == key)
    }
}
