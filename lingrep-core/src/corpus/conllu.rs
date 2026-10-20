//! Streaming CoNLL-U sentence decoding into the canonical token schema.
//!
//! CoNLL-U is already tokenized. This reader therefore never reconstructs text
//! and sends it through an analyzer: integer-ID word rows become matcher words
//! directly, multiword-token and empty-node rows remain structural input, and
//! basic dependency heads retain CoNLL-U's sentence-local `0`/one-based
//! convention.

use std::collections::{BTreeMap, HashSet};
use std::io::{self, BufRead};

use compact_str::CompactString;

use crate::token_match::{
    AttributeKey, AttributeKind, AttributeProvider, BoundProjection, BoundTokenSentence, CharSpan,
    ProjectedAttributeValue, SchemaAttribute, TokenSchema,
};

// CoNLL-U sentences span records, so the per-record reader bound alone cannot
// cap their accumulation. This is a format safety ceiling, not an execution
// batch target; ordinary configurable sentence/token residency limits still
// apply after decoding.
const MAX_SENTENCE_SOURCE_BYTES: usize = 64 << 20;

/// The metadata keys this reader carries at **sentence** grain — the UD
/// `# sent_id` comment, under UD's own bare spelling. Declared statically so
/// `--where 'sent_id = …'` is answered per sentence even over a file that
/// never wrote the comment, keeping the direct and prepared paths on one
/// grain. `newdoc id` is deliberately absent: it becomes *document* metadata
/// (`newdoc_id`), not a sentence value.
pub const SENTENCE_METADATA_KEYS: [&str; 1] = ["sent_id"];

/// The UD v2 feature inventory used by the in-process reader and checked
/// against the external Python adapters.
pub const UD_FEATURE_NAMES: [&str; 43] = [
    "Abbr",
    "AdpType",
    "AdvType",
    "Animacy",
    "Aspect",
    "Case",
    "Clusivity",
    "ConjType",
    "Definite",
    "Degree",
    "Deixis",
    "DeixisRef",
    "Echo",
    "Evident",
    "ExtPos",
    "Foreign",
    "Gender",
    "Hyph",
    "Mood",
    "NameType",
    "NounClass",
    "NounType",
    "Number",
    "NumForm",
    "NumType",
    "PartType",
    "Person",
    "Polarity",
    "Polite",
    "Poss",
    "PrepCase",
    "PronType",
    "PunctSide",
    "PunctType",
    "Reflex",
    "Style",
    "Subcat",
    "Tense",
    "Typo",
    "Variant",
    "VerbForm",
    "VerbType",
    "Voice",
];

/// The fixed schema of a generic CoNLL-U stream.
///
/// `conllu.feats` and `conllu.misc` retain complete entries as sets instead of
/// discarding language-specific features or inventing portable meanings for
/// producer-specific MISC keys. Known universal features are additionally
/// projected as typed `feat.*` attributes.
pub fn token_schema() -> TokenSchema {
    let portable = ["text", "lemma", "upos", "xpos"].into_iter().map(|key| {
        SchemaAttribute::new(key, AttributeKind::Scalar)
            .expect("canonical CoNLL-U scalar key must be valid")
    });
    let features = UD_FEATURE_NAMES.into_iter().map(|name| {
        SchemaAttribute::new(format!("feat.{name}"), AttributeKind::StringSet)
            .expect("canonical UD feature key must be valid")
    });
    let dependency = [
        SchemaAttribute::new("conllu.feats", AttributeKind::StringSet)
            .expect("canonical CoNLL-U feature key must be valid"),
        SchemaAttribute::new("dep.head", AttributeKind::Scalar)
            .expect("canonical dependency head key must be valid"),
        SchemaAttribute::new("dep.rel", AttributeKind::Scalar)
            .expect("canonical dependency relation key must be valid"),
        SchemaAttribute::new("dep.enhanced", AttributeKind::StringSet)
            .expect("canonical enhanced dependency key must be valid")
            .with_provider(AttributeProvider::EnhancedGraph),
        SchemaAttribute::new("conllu.misc", AttributeKind::StringSet)
            .expect("canonical CoNLL-U MISC key must be valid"),
    ];
    TokenSchema::new(portable.chain(features).chain(dependency))
        .expect("canonical CoNLL-U schema must be valid")
}

/// Semantic identity of canonical CoNLL-U decoding. The format supplies the
/// analysis, so this describes decoding rather than an NLP model invocation.
pub fn analyzer_descriptor() -> crate::analysis::AnalyzerDescriptor {
    use crate::analysis::{
        AnalyzerDescriptor, AnalyzerFamily, ExtractionProfile, NormalizationPolicy,
        SegmentationMode,
    };

    let schema = token_schema();
    let implementation = *blake3::hash(include_bytes!("conllu.rs")).as_bytes();
    AnalyzerDescriptor::new(
        AnalyzerFamily::Pretokenized,
        ExtractionProfile::Syntax,
        SegmentationMode::Input,
        NormalizationPolicy::None,
        "und",
        "conllu-sentence-v1",
        implementation,
        [0; 32],
        *blake3::hash(b"lingrep.conllu-projection/v1").as_bytes(),
        *blake3::hash(b"lingrep.conllu-word-alignment/v1").as_bytes(),
        [0; 32],
        *blake3::hash(b"lingrep.conllu-output/v1").as_bytes(),
        schema.digest(),
    )
    .expect("the fixed CoNLL-U descriptor is valid")
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Word {
    form: CompactString,
    lemma: Option<CompactString>,
    upos: Option<CompactString>,
    xpos: Option<CompactString>,
    features: Vec<(CompactString, Box<[CompactString]>)>,
    raw_features: Box<[CompactString]>,
    head: Option<CompactString>,
    relation: Option<CompactString>,
    misc: Box<[CompactString]>,
}

/// Sentence-local identity of one node in a CoNLL-U enhanced graph.
///
/// Integer word IDs become zero-based matcher token indices. Decimal empty
/// IDs retain both parts: `boundary` counts the integer words before the node,
/// and `subindex` distinguishes several empty nodes at that boundary.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum EnhancedNodeId {
    Word { token_index: usize },
    Empty { boundary: usize, subindex: usize },
}

impl EnhancedNodeId {
    fn source_order_key(self) -> (usize, u8, usize) {
        match self {
            Self::Word { token_index } => (token_index, 1, 0),
            Self::Empty { boundary, subindex } => (boundary, 0, subindex),
        }
    }

    #[must_use]
    pub fn spelling(self) -> String {
        match self {
            Self::Word { token_index } => (token_index + 1).to_string(),
            Self::Empty { boundary, subindex } => format!("{boundary}.{subindex}"),
        }
    }
}

/// The head endpoint of one enhanced edge. Root is not a bindable graph node.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum EnhancedHead {
    Root,
    Node(EnhancedNodeId),
}

impl EnhancedHead {
    fn source_order_key(self) -> Option<(usize, u8, usize)> {
        match self {
            Self::Root => None,
            Self::Node(node) => Some(node.source_order_key()),
        }
    }

    #[must_use]
    pub fn spelling(self) -> String {
        match self {
            Self::Root => "0".to_owned(),
            Self::Node(node) => node.spelling(),
        }
    }
}

/// One normalized enhanced dependency edge.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct EnhancedEdge {
    head: EnhancedHead,
    dependent: EnhancedNodeId,
    relation: CompactString,
}

impl EnhancedEdge {
    #[must_use]
    pub const fn head(&self) -> EnhancedHead {
        self.head
    }

    #[must_use]
    pub const fn dependent(&self) -> EnhancedNodeId {
        self.dependent
    }

    #[must_use]
    pub fn relation(&self) -> &str {
        &self.relation
    }
}

/// Owned boundary representation used to reconstruct one normalized edge.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EnhancedEdgeData {
    pub head: EnhancedHead,
    pub dependent: EnhancedNodeId,
    pub relation: String,
}

/// An enhanced empty node and the source attributes graph predicates may read.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EnhancedEmptyNode {
    id: EnhancedNodeId,
    form: Option<CompactString>,
    lemma: Option<CompactString>,
    upos: Option<CompactString>,
    xpos: Option<CompactString>,
    features: Vec<(CompactString, Box<[CompactString]>)>,
    raw_features: Box<[CompactString]>,
    misc: Box<[CompactString]>,
}

impl EnhancedEmptyNode {
    #[must_use]
    pub const fn id(&self) -> EnhancedNodeId {
        self.id
    }

    #[must_use]
    pub fn form(&self) -> Option<&str> {
        self.form.as_deref()
    }

    #[must_use]
    pub fn lemma(&self) -> Option<&str> {
        self.lemma.as_deref()
    }

    #[must_use]
    pub fn upos(&self) -> Option<&str> {
        self.upos.as_deref()
    }

    #[must_use]
    pub fn xpos(&self) -> Option<&str> {
        self.xpos.as_deref()
    }

    #[must_use]
    pub fn feature_values(&self, name: &str) -> Option<impl ExactSizeIterator<Item = &str>> {
        self.features
            .iter()
            .find_map(|(feature, values)| (feature == name).then_some(values))
            .map(|values| values.iter().map(CompactString::as_str))
    }

    #[must_use]
    pub fn feature_names(&self) -> impl ExactSizeIterator<Item = &str> {
        self.features.iter().map(|(name, _)| name.as_str())
    }

    #[must_use]
    pub fn raw_features(&self) -> impl ExactSizeIterator<Item = &str> {
        self.raw_features.iter().map(CompactString::as_str)
    }

    #[must_use]
    pub fn misc(&self) -> impl ExactSizeIterator<Item = &str> {
        self.misc.iter().map(CompactString::as_str)
    }
}

/// Owned boundary representation used to reconstruct one normalized empty node.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EnhancedEmptyNodeData {
    pub id: EnhancedNodeId,
    pub form: Option<String>,
    pub lemma: Option<String>,
    pub upos: Option<String>,
    pub xpos: Option<String>,
    pub features: Vec<(String, Vec<String>)>,
    pub raw_features: Vec<String>,
    pub misc: Vec<String>,
}

/// A validated, canonical sentence-local enhanced graph.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EnhancedGraph(Box<EnhancedGraphData>);

#[derive(Clone, Debug, Eq, PartialEq)]
struct EnhancedGraphData {
    word_count: usize,
    empty_nodes: Box<[EnhancedEmptyNode]>,
    edges: Box<[EnhancedEdge]>,
}

impl EnhancedGraph {
    /// Returns the integer-word count this sentence graph was validated against.
    #[must_use]
    pub const fn word_count(&self) -> usize {
        self.0.word_count
    }

    #[must_use]
    pub fn empty_nodes(&self) -> &[EnhancedEmptyNode] {
        &self.0.empty_nodes
    }

    #[must_use]
    pub fn edges(&self) -> &[EnhancedEdge] {
        &self.0.edges
    }

    /// Owned live bytes used by this normalized graph, for bounded execution.
    #[must_use]
    pub fn resident_bytes(&self) -> Option<usize> {
        let mut bytes = std::mem::size_of::<EnhancedGraphData>()
            .checked_add(std::mem::size_of_val(self.0.empty_nodes.as_ref()))?
            .checked_add(std::mem::size_of_val(self.0.edges.as_ref()))?;
        for edge in self.edges() {
            bytes = bytes.checked_add(edge.relation.capacity())?;
        }
        for node in self.empty_nodes() {
            for value in [&node.form, &node.lemma, &node.upos, &node.xpos]
                .into_iter()
                .flatten()
            {
                bytes = bytes.checked_add(value.capacity())?;
            }
            bytes = bytes
                .checked_add(std::mem::size_of_val(node.features.as_slice()))?
                .checked_add(std::mem::size_of_val(node.raw_features.as_ref()))?
                .checked_add(std::mem::size_of_val(node.misc.as_ref()))?;
            for (name, values) in &node.features {
                bytes = bytes
                    .checked_add(name.capacity())?
                    .checked_add(std::mem::size_of_val(values.as_ref()))?;
                for value in values {
                    bytes = bytes.checked_add(value.capacity())?;
                }
            }
            for value in node.raw_features.iter().chain(node.misc.iter()) {
                bytes = bytes.checked_add(value.capacity())?;
            }
        }
        Some(bytes)
    }

    /// Reconstructs a trusted normalized graph from a storage or wire boundary.
    ///
    /// The constructor validates node ranges, canonical empty-node identity,
    /// endpoint resolution, relation spelling, duplicate edges, and canonical
    /// edge order. Admission creates the same invariants directly while parsing.
    pub fn from_normalized_parts(
        word_count: usize,
        empty_nodes: Vec<EnhancedEmptyNodeData>,
        edges: Vec<EnhancedEdgeData>,
    ) -> Result<Self, String> {
        if word_count == 0 {
            return Err("enhanced graph must belong to a nonempty sentence".into());
        }
        let mut normalized_empty = Vec::with_capacity(empty_nodes.len());
        let mut previous = None;
        let mut expected_subindex = BTreeMap::<usize, usize>::new();
        for node in empty_nodes {
            let EnhancedNodeId::Empty { boundary, subindex } = node.id else {
                return Err("enhanced empty-node data must carry an empty node ID".into());
            };
            if boundary > word_count {
                return Err("enhanced empty-node boundary exceeds word count".into());
            }
            let expected = expected_subindex.entry(boundary).or_insert(1);
            if subindex != *expected {
                return Err("enhanced empty-node subindices are not canonical".into());
            }
            *expected += 1;
            if previous.is_some_and(|prior| prior >= node.id.source_order_key()) {
                return Err("enhanced empty nodes are not in canonical order".into());
            }
            previous = Some(node.id.source_order_key());
            normalized_empty.push(EnhancedEmptyNode {
                id: node.id,
                form: node.form.map(CompactString::new),
                lemma: node.lemma.map(CompactString::new),
                upos: node.upos.map(CompactString::new),
                xpos: node.xpos.map(CompactString::new),
                features: node
                    .features
                    .into_iter()
                    .map(|(name, values)| {
                        (
                            CompactString::new(name),
                            values
                                .into_iter()
                                .map(CompactString::new)
                                .collect::<Vec<_>>()
                                .into_boxed_slice(),
                        )
                    })
                    .collect(),
                raw_features: node
                    .raw_features
                    .into_iter()
                    .map(CompactString::new)
                    .collect(),
                misc: node.misc.into_iter().map(CompactString::new).collect(),
            });
        }
        let empty_ids = normalized_empty
            .iter()
            .map(EnhancedEmptyNode::id)
            .collect::<HashSet<_>>();
        let mut normalized_edges = Vec::with_capacity(edges.len());
        for edge in edges {
            if edge.relation.is_empty() || edge.relation.chars().any(char::is_whitespace) {
                return Err("enhanced edge relation must be nonempty and whitespace-free".into());
            }
            if !graph_node_resolves(edge.dependent, word_count, &empty_ids)
                || matches!(edge.head, EnhancedHead::Node(head) if !graph_node_resolves(head, word_count, &empty_ids))
            {
                return Err("enhanced edge endpoint does not resolve".into());
            }
            normalized_edges.push(EnhancedEdge {
                head: edge.head,
                dependent: edge.dependent,
                relation: CompactString::new(edge.relation),
            });
        }
        if normalized_edges
            .windows(2)
            .any(|pair| enhanced_edge_key(&pair[0]) >= enhanced_edge_key(&pair[1]))
        {
            return Err("enhanced edges are not in canonical unique order".into());
        }
        Ok(Self(Box::new(EnhancedGraphData {
            word_count,
            empty_nodes: normalized_empty.into_boxed_slice(),
            edges: normalized_edges.into_boxed_slice(),
        })))
    }
}

/// Whether a sentence supplies enough enhanced graph evidence for matching.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EnhancedGraphState {
    Unavailable,
    Incomplete,
    Complete,
}

/// One sentence's graph evidence, distinct from an observed empty edge set.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EnhancedGraphObservation {
    Unavailable,
    Incomplete(EnhancedGraph),
    Complete(EnhancedGraph),
}

impl EnhancedGraphObservation {
    #[must_use]
    pub const fn state(&self) -> EnhancedGraphState {
        match self {
            Self::Unavailable => EnhancedGraphState::Unavailable,
            Self::Incomplete(_) => EnhancedGraphState::Incomplete,
            Self::Complete(_) => EnhancedGraphState::Complete,
        }
    }

    #[must_use]
    pub fn graph(&self) -> Option<&EnhancedGraph> {
        match self {
            Self::Unavailable => None,
            Self::Incomplete(graph) | Self::Complete(graph) => Some(graph),
        }
    }

    #[must_use]
    pub fn complete(&self) -> Option<&EnhancedGraph> {
        match self {
            Self::Complete(graph) => Some(graph),
            Self::Unavailable | Self::Incomplete(_) => None,
        }
    }

    #[must_use]
    pub fn resident_bytes(&self) -> Option<usize> {
        self.graph().map_or(Some(0), EnhancedGraph::resident_bytes)
    }

    /// Derives the canonical `dep.enhanced` compatibility values for words.
    ///
    /// Empty nodes remain available through [`EnhancedGraph::empty_nodes`]
    /// and graph edges; this projection contains only word dependents because
    /// it mirrors the historical token attribute.
    ///
    /// # Errors
    ///
    /// Returns an error when the caller's token sentence has a different word
    /// count from the sentence against which the graph was validated.
    pub fn word_adjacency_values(
        &self,
        word_count: usize,
    ) -> Result<Vec<Vec<CompactString>>, String> {
        let mut by_word = vec![Vec::new(); word_count];
        let Some(graph) = self.graph() else {
            return Ok(by_word);
        };
        if graph.word_count() != word_count {
            return Err("enhanced graph word count differs from the token sentence".into());
        }
        for edge in graph.edges() {
            let EnhancedNodeId::Word { token_index } = edge.dependent else {
                continue;
            };
            by_word[token_index].push(CompactString::new(format!(
                "{}:{}",
                edge.head.spelling(),
                edge.relation
            )));
        }
        Ok(by_word)
    }
}

/// One decoded syntactic sentence. Original CoNLL-U token IDs are deliberately
/// absent: integer word rows are validated as `1..=n`, so matcher ordinals plus
/// one are the same identity without publishing a second identifier. The
/// sentence-level `# sent_id` comment is a *sentence* identity, not a token
/// one, and is carried.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Sentence {
    text: String,
    words: Vec<Word>,
    enhanced_graph: EnhancedGraphObservation,
    sent_id: Option<String>,
    newdoc: Option<NewdocBoundary>,
    /// Each token's code-point span within `text`, or `None` when a supplied
    /// `# text` disagreed with what the rows reconstruct.
    char_offsets: Option<Box<[CharSpan]>>,
}

/// A `# newdoc` comment observed before this sentence: the sentence begins a
/// new document, optionally named by `# newdoc id = …`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NewdocBoundary {
    pub id: Option<String>,
}

impl Sentence {
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Each token's sentence-relative code-point span, when they are
    /// derivable.
    ///
    /// `None` means the sentence declared a `# text` the rows do not
    /// reconstruct, so no offset here can be trusted to address it.
    #[must_use]
    pub fn char_offsets(&self) -> Option<&[CharSpan]> {
        self.char_offsets.as_deref()
    }

    /// The sentence's `# sent_id`, when the source carried one.
    #[must_use]
    pub fn sent_id(&self) -> Option<&str> {
        self.sent_id.as_deref()
    }

    /// The document boundary this sentence begins, when the source marked one.
    #[must_use]
    pub const fn newdoc(&self) -> Option<&NewdocBoundary> {
        self.newdoc.as_ref()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.words.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.words.is_empty()
    }

    #[must_use]
    pub const fn enhanced_graph(&self) -> &EnhancedGraphObservation {
        &self.enhanced_graph
    }

    pub(super) fn into_projected_parts(
        self,
        projection: &BoundProjection,
    ) -> Result<
        (
            String,
            BoundTokenSentence,
            EnhancedGraphObservation,
            Option<String>,
            Option<NewdocBoundary>,
        ),
        crate::token_match::BoundTokenError,
    > {
        let words = self.project(projection)?;
        Ok((
            self.text,
            words,
            self.enhanced_graph,
            self.sent_id,
            self.newdoc,
        ))
    }

    pub fn project(
        &self,
        projection: &BoundProjection,
    ) -> Result<BoundTokenSentence, crate::token_match::BoundTokenError> {
        let enhanced_by_word = projection
            .keys()
            .iter()
            .any(|key| key.as_str() == "dep.enhanced")
            .then(|| {
                self.enhanced_graph
                    .word_adjacency_values(self.words.len())
                    .expect("an admitted graph is bound to its sentence word count")
            });
        let mut values = Vec::with_capacity(self.words.len() * projection.len());
        for (word_index, word) in self.words.iter().enumerate() {
            for key in projection.keys().iter().map(AttributeKey::as_str) {
                let value = match key {
                    "text" => ProjectedAttributeValue::scalar(word.form.clone()),
                    "lemma" => ProjectedAttributeValue::Scalar(word.lemma.clone()),
                    "upos" => ProjectedAttributeValue::Scalar(word.upos.clone()),
                    "xpos" => ProjectedAttributeValue::Scalar(word.xpos.clone()),
                    "dep.head" => ProjectedAttributeValue::Scalar(word.head.clone()),
                    "dep.rel" => ProjectedAttributeValue::Scalar(word.relation.clone()),
                    "dep.enhanced" => ProjectedAttributeValue::string_set(
                        enhanced_by_word
                            .as_ref()
                            .expect("the enhanced projection was detected")[word_index]
                            .iter()
                            .cloned(),
                    ),
                    "conllu.misc" => ProjectedAttributeValue::string_set(word.misc.iter().cloned()),
                    "conllu.feats" => {
                        ProjectedAttributeValue::string_set(word.raw_features.iter().cloned())
                    }
                    _ if key.starts_with("feat.") => {
                        let name = key
                            .strip_prefix("feat.")
                            .expect("guard proves the feature prefix is present");
                        let found = word.features.iter().find(|(feature, _)| feature == name);
                        ProjectedAttributeValue::string_set(
                            found
                                .into_iter()
                                .flat_map(|(_, values)| values.iter().cloned()),
                        )
                    }
                    _ => ProjectedAttributeValue::missing_scalar(),
                };
                values.push(value);
            }
        }
        BoundTokenSentence::from_projected_attribute_values(projection, self.words.len(), values)
            // The offsets ride with the words rather than as a separate event
            // field, because that is the path every consumer already takes and
            // because an offset is a fact about a token, not about the run.
            .map(|sentence| {
                sentence.with_char_offsets(
                    self.char_offsets
                        .as_ref()
                        .map(|spans| std::sync::Arc::new(spans.to_vec())),
                )
            })
    }
}

/// A bounded-memory iterator over blank-line-delimited CoNLL-U sentences.
pub struct Reader<R> {
    lines: io::Lines<R>,
    source_name: String,
    line_number: usize,
    sentence_number: usize,
}

impl<R: BufRead> Reader<R> {
    #[must_use]
    pub fn new(reader: R, source_name: impl Into<String>) -> Self {
        Self {
            lines: reader.lines(),
            source_name: source_name.into(),
            line_number: 0,
            sentence_number: 0,
        }
    }
}

impl<R: BufRead> Iterator for Reader<R> {
    type Item = io::Result<Sentence>;

    fn next(&mut self) -> Option<Self::Item> {
        let mut rows = Vec::new();
        let mut text = None;
        let mut sent_id = None;
        let mut newdoc = None;
        let mut source_bytes = 0_usize;
        loop {
            match self.lines.next() {
                Some(Ok(line)) => {
                    self.line_number += 1;
                    let line = line.strip_suffix('\r').unwrap_or(&line);
                    if line.is_empty() {
                        if rows.is_empty() {
                            continue;
                        }
                        break;
                    }
                    if let Some(value) = line.strip_prefix("# text = ") {
                        text = Some(value.to_owned());
                    } else if let Some(value) = line.strip_prefix("# sent_id = ") {
                        sent_id = Some(value.to_owned());
                    } else if let Some(value) = line.strip_prefix("# newdoc id = ") {
                        newdoc = Some(NewdocBoundary {
                            id: Some(value.to_owned()),
                        });
                    } else if line == "# newdoc" {
                        newdoc = Some(NewdocBoundary { id: None });
                    } else if !line.starts_with('#') {
                        source_bytes = match source_bytes
                            .checked_add(line.len())
                            .and_then(|bytes| bytes.checked_add(1))
                        {
                            Some(bytes) if bytes <= MAX_SENTENCE_SOURCE_BYTES => bytes,
                            _ => {
                                return Some(Err(invalid(
                                    &self.source_name,
                                    self.line_number,
                                    "CoNLL-U sentence exceeds the 64 MiB source safety ceiling",
                                )));
                            }
                        };
                        rows.push((self.line_number, line.to_owned()));
                    }
                }
                Some(Err(error)) => return Some(Err(error)),
                None if rows.is_empty() => return None,
                None => break,
            }
        }
        self.sentence_number += 1;
        Some(parse_sentence(
            &self.source_name,
            self.sentence_number,
            text,
            sent_id,
            newdoc,
            rows,
        ))
    }
}

fn parse_sentence(
    source_name: &str,
    sentence_number: usize,
    text: Option<String>,
    sent_id: Option<String>,
    newdoc: Option<NewdocBoundary>,
    rows: Vec<(usize, String)>,
) -> io::Result<Sentence> {
    let mut words = Vec::new();
    let mut multiword = Vec::<(std::ops::Range<usize>, String, bool)>::new();
    let mut empty_nodes = Vec::new();
    let mut graph_nodes = Vec::new();
    let mut pending_edges = Vec::new();
    let mut word_deps_present = Vec::new();
    let mut empty_subindices: BTreeMap<usize, Vec<(usize, usize)>> = BTreeMap::new();
    for (line_number, row) in rows {
        let columns = row.split('\t').collect::<Vec<_>>();
        if columns.len() != 10 {
            return Err(invalid(
                source_name,
                line_number,
                "expected exactly 10 tab-separated columns",
            ));
        }
        if let Some((start, end)) = columns[0].split_once('-') {
            let range = start
                .parse::<usize>()
                .ok()
                .zip(end.parse::<usize>().ok())
                .filter(|(start, end)| *start > 0 && start < end)
                .map(|(start, end)| start - 1..end)
                .ok_or_else(|| {
                    invalid(source_name, line_number, "invalid multiword-token range")
                })?;
            if range.start != words.len()
                || multiword
                    .last()
                    .is_some_and(|(previous, _, _)| previous.end > range.start)
            {
                return Err(invalid(
                    source_name,
                    line_number,
                    "multiword-token ranges must precede their words and not overlap",
                ));
            }
            multiword.push((
                range,
                columns[1].to_owned(),
                !columns[9].split('|').any(|item| item == "SpaceAfter=No"),
            ));
            continue;
        }
        let node = parse_graph_node_id(columns[0], source_name, line_number)?;
        graph_nodes.push((node, line_number));
        let (deps_present, edges) =
            parse_enhanced_edges(columns[8], node, source_name, line_number)?;
        pending_edges.extend(edges.into_iter().map(|edge| (line_number, edge)));

        match node {
            EnhancedNodeId::Word { token_index } => {
                if token_index != words.len() {
                    return Err(invalid(
                        source_name,
                        line_number,
                        "integer word IDs must be contiguous and start at 1",
                    ));
                }
                let (features, raw_features) =
                    parse_features(columns[5], source_name, line_number)?;
                let head = optional(columns[6]);
                if let Some(head) = head.as_deref() {
                    head.parse::<usize>().map_err(|_| {
                        invalid(
                            source_name,
                            line_number,
                            "HEAD must be 0 or an integer word ID",
                        )
                    })?;
                }
                words.push(Word {
                    form: CompactString::new(columns[1]),
                    lemma: optional(columns[2]),
                    upos: optional(columns[3]),
                    xpos: optional(columns[4]),
                    features,
                    raw_features,
                    head,
                    relation: optional(columns[7])
                        .map(|value| CompactString::new(value.to_lowercase())),
                    misc: split_set(columns[9], '|'),
                });
                word_deps_present.push(deps_present);
            }
            EnhancedNodeId::Empty { boundary, subindex } => {
                if columns[6] != "_" || columns[7] != "_" {
                    return Err(invalid(
                        source_name,
                        line_number,
                        &format!("empty node {} must omit HEAD and DEPREL", columns[0]),
                    ));
                }
                if !deps_present {
                    return Err(invalid(
                        source_name,
                        line_number,
                        &format!("empty node {} must carry DEPS", columns[0]),
                    ));
                }
                let (features, raw_features) =
                    parse_features(columns[5], source_name, line_number)?;
                empty_nodes.push(EnhancedEmptyNode {
                    id: node,
                    form: optional(columns[1]),
                    lemma: optional(columns[2]),
                    upos: optional(columns[3]),
                    xpos: optional(columns[4]),
                    features,
                    raw_features,
                    misc: split_set(columns[9], '|'),
                });
                empty_subindices
                    .entry(boundary)
                    .or_default()
                    .push((subindex, line_number));
            }
        }
    }
    if words.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "{source_name} sentence {sentence_number}: sentence contains no integer-ID words"
            ),
        ));
    }
    if let Some((_, line_number)) = graph_nodes.windows(2).find_map(|pair| {
        (pair[0].0.source_order_key() >= pair[1].0.source_order_key()).then_some(pair[1])
    }) {
        return Err(invalid(
            source_name,
            line_number,
            "graph node rows must follow CoNLL-U source order",
        ));
    }
    for (boundary, values) in &empty_subindices {
        if *boundary > words.len() {
            let (_, line_number) = values[0];
            return Err(invalid(
                source_name,
                line_number,
                &format!(
                    "empty node boundary {boundary} exceeds sentence word count {}",
                    words.len()
                ),
            ));
        }
        for (expected, (actual, line_number)) in (1..).zip(values) {
            if expected != *actual {
                return Err(invalid(
                    source_name,
                    *line_number,
                    &format!(
                        "empty node subindices after boundary {boundary} must be consecutive from 1"
                    ),
                ));
            }
        }
    }
    for (index, word) in words.iter().enumerate() {
        if let Some(head) = word.head.as_deref() {
            let value = head.parse::<usize>().expect("HEAD parsed above");
            if value > words.len() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "{source_name} sentence {sentence_number} word {}: HEAD {value} exceeds sentence word count {}",
                        index + 1,
                        words.len()
                    ),
                ));
            }
        }
    }
    let nodes = (0..words.len())
        .map(|token_index| EnhancedNodeId::Word { token_index })
        .chain(empty_nodes.iter().map(EnhancedEmptyNode::id))
        .collect::<HashSet<_>>();
    let mut edges = Vec::with_capacity(pending_edges.len());
    for (line_number, edge) in pending_edges {
        if let EnhancedHead::Node(head) = edge.head
            && !nodes.contains(&head)
        {
            return Err(invalid(
                source_name,
                line_number,
                &format!("DEPS head {} does not resolve", head.spelling()),
            ));
        }
        edges.push(edge);
    }
    edges.sort_unstable_by(|left, right| enhanced_edge_key(left).cmp(&enhanced_edge_key(right)));
    let unavailable = empty_nodes.is_empty() && word_deps_present.iter().all(|present| !present);
    let enhanced_graph = if unavailable {
        EnhancedGraphObservation::Unavailable
    } else {
        let graph = EnhancedGraph(Box::new(EnhancedGraphData {
            word_count: words.len(),
            empty_nodes: empty_nodes.into_boxed_slice(),
            edges: edges.into_boxed_slice(),
        }));
        if word_deps_present.iter().all(|present| *present) {
            EnhancedGraphObservation::Complete(graph)
        } else {
            EnhancedGraphObservation::Incomplete(graph)
        }
    };
    if multiword
        .last()
        .is_some_and(|(range, _, _)| range.end > words.len())
    {
        return Err(invalid(
            source_name,
            0,
            "multiword-token range exceeds sentence word count",
        ));
    }
    let (reconstructed, derived_offsets) = reconstruct_with_offsets(&words, &multiword);
    // `# text` is authoritative when present, so offsets derived from the rows
    // describe it only when the two agree. UD requires that they do; this
    // reader has never verified it, and an offset into a string it does not
    // describe points confidently at the wrong characters. Absent beats wrong,
    // so a disagreement withdraws the offsets and leaves matching untouched.
    let char_offsets = match text.as_deref() {
        Some(supplied) if supplied != reconstructed => None,
        _ => Some(derived_offsets),
    };
    let text = text.unwrap_or(reconstructed);
    Ok(Sentence {
        text,
        words,
        enhanced_graph,
        sent_id,
        newdoc,
        char_offsets,
    })
}

fn parse_graph_node_id(
    value: &str,
    source_name: &str,
    line_number: usize,
) -> io::Result<EnhancedNodeId> {
    if let Some((boundary, subindex)) = value.split_once('.') {
        if subindex.contains('.') {
            return Err(invalid(
                source_name,
                line_number,
                "empty node ID must be BOUNDARY.SUBINDEX",
            ));
        }
        let boundary = parse_canonical_integer(boundary, true).ok_or_else(|| {
            invalid(
                source_name,
                line_number,
                "empty node boundary must be a canonical nonnegative integer",
            )
        })?;
        let subindex = parse_canonical_integer(subindex, false).ok_or_else(|| {
            invalid(
                source_name,
                line_number,
                "empty node subindex must be a canonical positive integer",
            )
        })?;
        return Ok(EnhancedNodeId::Empty { boundary, subindex });
    }
    let id = parse_canonical_integer(value, false).ok_or_else(|| {
        invalid(
            source_name,
            line_number,
            "word ID must be a canonical positive integer",
        )
    })?;
    Ok(EnhancedNodeId::Word {
        token_index: id - 1,
    })
}

fn parse_canonical_integer(value: &str, allow_zero: bool) -> Option<usize> {
    if value.is_empty()
        || !value.bytes().all(|byte| byte.is_ascii_digit())
        || (value.len() > 1 && value.starts_with('0'))
    {
        return None;
    }
    let parsed = value.parse::<usize>().ok()?;
    (allow_zero || parsed > 0).then_some(parsed)
}

fn parse_enhanced_edges(
    value: &str,
    dependent: EnhancedNodeId,
    source_name: &str,
    line_number: usize,
) -> io::Result<(bool, Vec<EnhancedEdge>)> {
    if value == "_" {
        return Ok((false, Vec::new()));
    }
    let mut edges = Vec::new();
    let mut seen = HashSet::new();
    for member in value.split('|') {
        let Some((head, relation)) = member.split_once(':') else {
            return Err(invalid(
                source_name,
                line_number,
                "DEPS member must be HEAD:RELATION",
            ));
        };
        if head.is_empty() || relation.is_empty() || member.chars().any(char::is_whitespace) {
            return Err(invalid(
                source_name,
                line_number,
                "DEPS member must have nonempty, whitespace-free HEAD and RELATION",
            ));
        }
        let head = if head == "0" {
            EnhancedHead::Root
        } else {
            EnhancedHead::Node(parse_graph_node_id(head, source_name, line_number)?)
        };
        let edge = EnhancedEdge {
            head,
            dependent,
            relation: CompactString::new(relation),
        };
        if !seen.insert(edge.clone()) {
            return Err(invalid(
                source_name,
                line_number,
                &format!("duplicate DEPS edge {member}"),
            ));
        }
        edges.push(edge);
    }
    Ok((true, edges))
}

fn enhanced_edge_key(
    edge: &EnhancedEdge,
) -> ((usize, u8, usize), Option<(usize, u8, usize)>, &[u8]) {
    (
        edge.dependent.source_order_key(),
        edge.head.source_order_key(),
        edge.relation.as_bytes(),
    )
}

fn graph_node_resolves(
    node: EnhancedNodeId,
    word_count: usize,
    empty_ids: &HashSet<EnhancedNodeId>,
) -> bool {
    match node {
        EnhancedNodeId::Word { token_index } => token_index < word_count,
        EnhancedNodeId::Empty { .. } => empty_ids.contains(&node),
    }
}

fn parse_features(
    value: &str,
    source_name: &str,
    line_number: usize,
) -> io::Result<(
    Vec<(CompactString, Box<[CompactString]>)>,
    Box<[CompactString]>,
)> {
    if value == "_" {
        return Ok((Vec::new(), Box::new([])));
    }
    let raw = value.split('|').map(CompactString::new).collect::<Vec<_>>();
    let mut known = Vec::new();
    for entry in &raw {
        let (name, values) = entry
            .split_once('=')
            .ok_or_else(|| invalid(source_name, line_number, "FEATS entry must be NAME=VALUE"))?;
        if name.is_empty() || values.is_empty() {
            return Err(invalid(
                source_name,
                line_number,
                "FEATS entry must have a nonempty name and value",
            ));
        }
        if !UD_FEATURE_NAMES.contains(&name) {
            continue;
        }
        let values = values
            .split(',')
            .map(CompactString::new)
            .collect::<Vec<_>>()
            .into_boxed_slice();
        known.push((CompactString::new(name), values));
    }
    Ok((known, raw.into_boxed_slice()))
}

fn optional(value: &str) -> Option<CompactString> {
    (value != "_").then(|| CompactString::new(value))
}

fn split_set(value: &str, separator: char) -> Box<[CompactString]> {
    if value == "_" {
        Box::new([])
    } else {
        value
            .split(separator)
            .map(CompactString::new)
            .collect::<Vec<_>>()
            .into_boxed_slice()
    }
}

/// The sentence text, and each token's code-point span within it.
///
/// One pass, because the two answers are the same walk and computing them
/// apart is how they drift: an offset is only meaningful against the exact
/// string this returns.
///
/// The separator is removed only when one was appended. It used to be an
/// unconditional `pop`, which silently ate the final character of any sentence
/// whose last token carried `SpaceAfter=No` — the ordinary shape of Japanese
/// CoNLL-U without a `# text` comment (`vibrato-pipe-91p`).
fn reconstruct_with_offsets(
    words: &[Word],
    multiword: &[(std::ops::Range<usize>, String, bool)],
) -> (String, Box<[CharSpan]>) {
    let mut text = String::new();
    let mut spans = Vec::with_capacity(words.len());
    let mut chars: u32 = 0;
    let mut trailing_separator = false;
    let mut index = 0;
    let mut ranges = multiword.iter().peekable();
    while index < words.len() {
        let (form, word_end, separator) = if ranges
            .peek()
            .is_some_and(|(range, _, _)| range.start == index)
        {
            let (range, form, separator) = ranges.next().expect("peeked matching multiword range");
            (form.as_str(), range.end, *separator)
        } else {
            let word = &words[index];
            (
                word.form.as_str(),
                index + 1,
                !word.misc.iter().any(|entry| entry == "SpaceAfter=No"),
            )
        };
        let start = chars;
        text.push_str(form);
        chars = chars.saturating_add(u32::try_from(form.chars().count()).unwrap_or(u32::MAX));
        spans.resize(word_end, CharSpan::new(start, chars));
        index = word_end;
        trailing_separator = separator;
        if trailing_separator {
            text.push(' ');
            chars = chars.saturating_add(1);
        }
    }
    if trailing_separator {
        text.pop();
    }
    (text, spans.into_boxed_slice())
}

fn invalid(source_name: &str, line_number: usize, message: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("{source_name} line {line_number}: {message}"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::token_match::{AttributeValueRef, TokenSequence};

    const UD_GSD: &str = "# sent_id = test-s1\n# text = 太郎は走る。\n1\t太郎\t太郎\tPROPN\t名詞-固有名詞\tCase=Nom\t3\tnsubj\t3:nsubj\tSpaceAfter=No\n2\tは\tは\tADP\t助詞\t_\t1\tcase\t1:case\tSpaceAfter=No\n3\t走る\t走る\tVERB\t動詞\tVerbForm=Fin\t0\troot\t0:root\tSpaceAfter=No\n4\t。\t。\tPUNCT\t補助記号-句点\t_\t3\tpunct\t3:punct\tSpaceAfter=No\n\n";

    /// A sentence with no `# text` whose last token carries `SpaceAfter=No`.
    ///
    /// The reader falls back to reconstruction here, and the reconstruction is
    /// what any derived character offset has to describe.
    #[test]
    fn reconstructed_text_keeps_the_final_token_when_nothing_follows_it() {
        let input = "1\t\u{592a}\u{90ce}\t_\tPROPN\t_\t_\t2\tnsubj\t_\tSpaceAfter=No\n2\t\u{8d70}\u{308b}\t_\tVERB\t_\t_\t0\troot\t_\tSpaceAfter=No\n3\t\u{3002}\t_\tPUNCT\t_\t_\t2\tpunct\t_\tSpaceAfter=No\n\n";
        let sentence = Reader::new(input.as_bytes(), "no-text.conllu")
            .next()
            .expect("one sentence")
            .expect("parses");
        assert_eq!(sentence.text(), "\u{592a}\u{90ce}\u{8d70}\u{308b}\u{3002}");
    }

    /// Offsets index the sentence's own text, and `SpaceAfter=No` is exactly
    /// what a naive cumulative token length cannot see.
    #[test]
    fn char_offsets_address_the_sentence_text_across_a_space() {
        let input = "# text = the dog\n1\tthe\tthe\tDET\tDT\t_\t2\tdet\t_\t_\n2\tdog\tdog\tNOUN\tNN\t_\t0\troot\t_\tSpaceAfter=No\n\n";
        let sentence = Reader::new(input.as_bytes(), "spaced.conllu")
            .next()
            .expect("one sentence")
            .expect("parses");
        let text: Vec<char> = sentence.text().chars().collect();
        let offsets = sentence.char_offsets().expect("text agrees with the rows");
        let slice = |span: &CharSpan| -> String {
            text[span.start as usize..span.end as usize]
                .iter()
                .collect()
        };
        assert_eq!(slice(&offsets[0]), "the");
        assert_eq!(slice(&offsets[1]), "dog");
        // 4, not 3: the separator after `the` is a character of the sentence,
        // which is the whole reason a cumulative token length is not an offset.
        assert_eq!(offsets[1].start, 4);
    }

    /// The direct path projects, then reprojects onto the run's projection.
    /// Offsets do not depend on which attributes were selected, so they must
    /// survive that second hop — it is where they would silently vanish.
    #[test]
    fn char_offsets_survive_projection_and_reprojection() {
        let input = "# text = the dog\n1\tthe\tthe\tDET\tDT\t_\t2\tdet\t_\t_\n2\tdog\tdog\tNOUN\tNN\t_\t0\troot\t_\tSpaceAfter=No\n\n";
        let sentence = Reader::new(input.as_bytes(), "reproject.conllu")
            .next()
            .expect("one sentence")
            .expect("parses");
        let schema = token_schema();
        let full = schema
            .bind_projection([
                AttributeKey::new("text").unwrap(),
                AttributeKey::new("upos").unwrap(),
            ])
            .expect("full projection");
        let words = sentence.project(&full).expect("projects");
        assert_eq!(
            TokenSequence::char_span(&words, 1),
            Some(CharSpan::new(4, 7))
        );

        let narrow = schema
            .bind_projection([AttributeKey::new("upos").unwrap()])
            .expect("narrow projection");
        let reprojected = words.reproject(&narrow).expect("reprojects");
        assert_eq!(
            TokenSequence::char_span(&reprojected, 1),
            Some(CharSpan::new(4, 7)),
            "a narrower attribute projection must not drop the offsets"
        );
    }

    #[test]
    fn multiword_words_address_the_enclosing_surface_token() {
        let input = "# text = can't go\n1-2\tcan't\t_\t_\t_\t_\t_\t_\t_\t_\n1\tca\tcan\tAUX\tMD\t_\t3\taux\t_\t_\n2\tn't\tnot\tPART\tRB\t_\t3\tadvmod\t_\tSpaceAfter=No\n3\tgo\tgo\tVERB\tVB\t_\t0\troot\t_\t_\n\n";
        let sentence = Reader::new(input.as_bytes(), "mwt.conllu")
            .next()
            .expect("one sentence")
            .expect("parses");
        assert_eq!(sentence.text(), "can't go");
        assert_eq!(
            sentence.char_offsets().unwrap(),
            &[
                CharSpan::new(0, 5),
                CharSpan::new(0, 5),
                CharSpan::new(6, 8)
            ]
        );
    }

    #[test]
    fn malformed_multiword_ranges_are_refused() {
        for range in ["1-1", "0-2", "1-4", "2-3", "1-x"] {
            let input = format!(
                "{range}\tcan't\t_\t_\t_\t_\t_\t_\t_\t_\n1\tca\t_\tX\t_\t_\t0\troot\t_\t_\n2\tn't\t_\tX\t_\t_\t1\tdep\t_\t_\n\n"
            );
            assert!(
                Reader::new(input.as_bytes(), "mwt.conllu")
                    .next()
                    .unwrap()
                    .is_err(),
                "{range}"
            );
        }
    }

    /// Non-BMP characters are where a code-point count and a UTF-16 or byte
    /// count disagree, so an all-BMP fixture cannot prove the unit.
    #[test]
    fn char_offsets_count_code_points_not_utf16_units_or_bytes() {
        let input = "# text = \u{1f600}\u{3042}\n1\t\u{1f600}\t_\tX\t_\t_\t0\troot\t_\tSpaceAfter=No\n2\t\u{3042}\t_\tX\t_\t_\t1\tdep\t_\tSpaceAfter=No\n\n";
        let sentence = Reader::new(input.as_bytes(), "astral.conllu")
            .next()
            .expect("one sentence")
            .expect("parses");
        let offsets = sentence.char_offsets().expect("text agrees with the rows");
        // One code point, though it is two UTF-16 units and four UTF-8 bytes.
        assert_eq!(offsets[0], CharSpan::new(0, 1));
        assert_eq!(offsets[1], CharSpan::new(1, 2));
    }

    /// A `# text` the rows do not reconstruct withdraws the offsets rather
    /// than publishing ones that address a different string.
    #[test]
    fn disagreeing_declared_text_withdraws_offsets_but_not_the_sentence() {
        let input = "# text = \u{592a}\u{90ce}\u{304c}\u{8d70}\u{308b}\n1\t\u{592a}\u{90ce}\t_\tPROPN\t_\t_\t2\tnsubj\t_\tSpaceAfter=No\n2\t\u{8d70}\u{308b}\t_\tVERB\t_\t_\t0\troot\t_\tSpaceAfter=No\n\n";
        let sentence = Reader::new(input.as_bytes(), "disagreeing.conllu")
            .next()
            .expect("one sentence")
            .expect("parses");
        // The declared text carries a \u{304c} the rows do not.
        assert_eq!(sentence.text(), "\u{592a}\u{90ce}\u{304c}\u{8d70}\u{308b}");
        assert_eq!(sentence.char_offsets(), None);
        assert_eq!(sentence.len(), 2);
    }

    #[test]
    fn ud_treebank_sentence_projects_portable_and_dependency_values() {
        let sentence = Reader::new(UD_GSD.as_bytes(), "ja_gsd-ud-test.conllu")
            .next()
            .unwrap()
            .unwrap();
        assert_eq!(sentence.text(), "太郎は走る。");
        assert_eq!(sentence.len(), 4);
        let schema = token_schema();
        assert_eq!(
            schema.provider_for_key(&AttributeKey::new("dep.enhanced").unwrap()),
            Some(AttributeProvider::EnhancedGraph)
        );
        let projection = schema
            .bind_projection([
                AttributeKey::new("text").unwrap(),
                AttributeKey::new("upos").unwrap(),
                AttributeKey::new("feat.Case").unwrap(),
                AttributeKey::new("dep.head").unwrap(),
                AttributeKey::new("dep.rel").unwrap(),
                AttributeKey::new("dep.enhanced").unwrap(),
                AttributeKey::new("conllu.misc").unwrap(),
            ])
            .unwrap();
        let bound = sentence.project(&projection).unwrap();
        let first = bound.token(0).unwrap();
        assert_eq!(
            first.get(projection.ordinal_key("text").unwrap()),
            Some("太郎")
        );
        assert_eq!(
            first.get(projection.ordinal_key("dep.head").unwrap()),
            Some("3")
        );
        assert_eq!(
            bound
                .token(2)
                .unwrap()
                .get(projection.ordinal_key("dep.rel").unwrap()),
            Some("root")
        );
        assert_eq!(
            bound
                .token(2)
                .unwrap()
                .get(projection.ordinal_key("upos").unwrap()),
            Some("VERB")
        );
        assert_eq!(
            first.value(projection.ordinal_key("feat.Case").unwrap()),
            Some(AttributeValueRef::StringSet(&[CompactString::new("Nom")]))
        );
    }

    #[test]
    fn sentence_identity_comments_are_carried() {
        let sentence = Reader::new(UD_GSD.as_bytes(), "ja_gsd-ud-test.conllu")
            .next()
            .unwrap()
            .unwrap();
        assert_eq!(sentence.sent_id(), Some("test-s1"));
        assert_eq!(sentence.newdoc(), None);
    }

    #[test]
    fn newdoc_comments_mark_boundaries_named_and_bare() {
        let input = "# newdoc id = doc-1\n# sent_id = s1\n# text = 猫\n1\t猫\t猫\tNOUN\t名詞\t_\t0\troot\t_\t_\n\n# newdoc\n# text = 犬\n1\t犬\t犬\tNOUN\t名詞\t_\t0\troot\t_\t_\n\n# text = 鳥\n1\t鳥\t鳥\tNOUN\t名詞\t_\t0\troot\t_\t_\n\n";
        let mut reader = Reader::new(input.as_bytes(), "boundaries.conllu");
        let first = reader.next().unwrap().unwrap();
        assert_eq!(
            first.newdoc(),
            Some(&NewdocBoundary {
                id: Some("doc-1".to_owned())
            })
        );
        assert_eq!(first.sent_id(), Some("s1"));
        let second = reader.next().unwrap().unwrap();
        assert_eq!(second.newdoc(), Some(&NewdocBoundary { id: None }));
        assert_eq!(second.sent_id(), None);
        let third = reader.next().unwrap().unwrap();
        assert_eq!(third.newdoc(), None);
    }

    #[test]
    fn unrecognized_comments_stay_ignored() {
        // `# parallel_id` and producer annotations are not consumed: only the
        // two UD identity comments are, per the scoping decision.
        let input = "# parallel_id = jagsd/test6\n# meta::genre = news\n# text = 猫\n1\t猫\t猫\tNOUN\t名詞\t_\t0\troot\t_\t_\n\n";
        let sentence = Reader::new(input.as_bytes(), "other.conllu")
            .next()
            .unwrap()
            .unwrap();
        assert_eq!(sentence.sent_id(), None);
        assert_eq!(sentence.newdoc(), None);
    }

    #[test]
    fn structural_rows_do_not_create_duplicate_matcher_identifiers() {
        let input = "# text = can't go\n1-2\tcan't\t_\t_\t_\t_\t_\t_\t_\t_\n1\tca\tcan\tAUX\tMD\t_\t3\taux\t3:aux\t_\n2\tn't\tnot\tPART\tRB\tPolarity=Neg\t3\tadvmod\t3:advmod\tSpaceAfter=No\n2.1\treally\treally\tADV\tRB\t_\t_\t_\t3:advmod\t_\n3\tgo\tgo\tVERB\tVB\tVerbForm=Inf\t0\troot\t0:root\t_\n\n";
        let sentence = Reader::new(input.as_bytes(), "ewt.conllu")
            .next()
            .unwrap()
            .unwrap();
        assert_eq!(sentence.len(), 3);
    }

    #[test]
    fn canonical_dependency_attributes_are_queryable() {
        use crate::fsm::{FsmBuilder, PatternSource};

        let sentence = Reader::new(UD_GSD.as_bytes(), "ja_gsd-ud-test.conllu")
            .next()
            .unwrap()
            .unwrap();
        let schema = token_schema();
        let mut builder = FsmBuilder::new();
        builder
            .add_definition(PatternSource::named(
                "dependency.lx",
                "pattern root := [dep.rel=root & upos=VERB].",
            ))
            .unwrap();
        let compiled = builder.build().unwrap().matcher;
        assert!(
            compiled
                .required_attributes()
                .iter()
                .any(|key| key.as_str() == "dep.rel")
        );
        let bound = compiled.bind(&schema).unwrap();
        let words = sentence.project(bound.projection()).unwrap();
        assert_eq!(bound.find_matches(&words).unwrap().len(), 1);
    }

    #[test]
    fn language_specific_features_remain_queryable_without_a_registry_entry() {
        let input =
            "# text = x\r\n1\tx\tx\tX\tX\tNovelFeature=Value\t0\tROOT\t_\tProducerFlag\r\n\r\n";
        let sentence = Reader::new(input.as_bytes(), "extension.conllu")
            .next()
            .unwrap()
            .unwrap();
        let schema = token_schema();
        let projection = schema
            .bind_projection([
                AttributeKey::new("conllu.feats").unwrap(),
                AttributeKey::new("conllu.misc").unwrap(),
                AttributeKey::new("dep.rel").unwrap(),
            ])
            .unwrap();
        let bound = sentence.project(&projection).unwrap();
        let token = bound.token(0).unwrap();
        assert_eq!(
            token.value(projection.ordinal_key("conllu.feats").unwrap()),
            Some(AttributeValueRef::StringSet(&[CompactString::new(
                "NovelFeature=Value"
            )]))
        );
        assert_eq!(
            token.value(projection.ordinal_key("conllu.misc").unwrap()),
            Some(AttributeValueRef::StringSet(&[CompactString::new(
                "ProducerFlag"
            )]))
        );
        assert_eq!(
            token.get(projection.ordinal_key("dep.rel").unwrap()),
            Some("root")
        );
    }

    #[test]
    fn enhanced_observation_states_are_sentence_local() {
        let cases = [
            (
                "unavailable",
                "1\tx\tx\tX\t_\t_\t0\troot\t_\t_\n",
                EnhancedGraphState::Unavailable,
            ),
            (
                "complete",
                "1\tx\tx\tX\t_\t_\t0\troot\t0:root\t_\n",
                EnhancedGraphState::Complete,
            ),
            (
                "mixed words",
                "1\tx\tx\tX\t_\t_\t2\tnsubj\t2:nsubj\t_\n2\ty\ty\tX\t_\t_\t0\troot\t_\t_\n",
                EnhancedGraphState::Incomplete,
            ),
            (
                "only an empty node supplies DEPS",
                "1\tx\tx\tX\t_\t_\t0\troot\t_\t_\n1.1\ty\ty\tX\t_\t_\t_\t_\t1:dep\t_\n",
                EnhancedGraphState::Incomplete,
            ),
        ];

        for (name, input, expected) in cases {
            let sentence = Reader::new(input.as_bytes(), name).next().unwrap().unwrap();
            assert_eq!(sentence.enhanced_graph().state(), expected, "{name}");
        }

        let input = "1\tx\tx\tX\t_\t_\t0\troot\t0:root\t_\n\n1\ty\ty\tX\t_\t_\t0\troot\t_\t_\n";
        let states = Reader::new(input.as_bytes(), "mixed-file.conllu")
            .map(|sentence| sentence.unwrap().enhanced_graph().state())
            .collect::<Vec<_>>();
        assert_eq!(
            states,
            [
                EnhancedGraphState::Complete,
                EnhancedGraphState::Unavailable
            ]
        );
    }

    #[test]
    fn enhanced_graph_keeps_same_boundary_empty_nodes_distinct() {
        let input = "1\tx\tx\tX\t_\t_\t0\troot\t0:root\t_\n1.1\ta\ta\tVERB\tV\tVerbForm=Fin\t_\t_\t1:dep\tSpaceAfter=No\n1.2\tb\tb\tADJ\tA\tDegree=Pos\t_\t_\t1.1:conj\t_\n";
        let sentence = Reader::new(input.as_bytes(), "same-boundary.conllu")
            .next()
            .unwrap()
            .unwrap();
        let graph = sentence
            .enhanced_graph()
            .complete()
            .expect("the fixture supplies every DEPS cell");

        assert_eq!(graph.empty_nodes().len(), 2);
        assert_eq!(
            graph.empty_nodes()[0].id(),
            EnhancedNodeId::Empty {
                boundary: 1,
                subindex: 1,
            }
        );
        assert_eq!(graph.empty_nodes()[0].form(), Some("a"));
        assert_eq!(
            graph.empty_nodes()[0].raw_features().collect::<Vec<_>>(),
            ["VerbForm=Fin"]
        );
        assert_eq!(
            graph.empty_nodes()[1].id(),
            EnhancedNodeId::Empty {
                boundary: 1,
                subindex: 2,
            }
        );
        assert_eq!(graph.empty_nodes()[1].misc().count(), 0);
        assert_eq!(graph.edges().len(), 3);
        assert_eq!(
            graph.edges()[2].head(),
            EnhancedHead::Node(EnhancedNodeId::Empty {
                boundary: 1,
                subindex: 1,
            })
        );
        assert_eq!(
            graph.edges()[2].dependent(),
            EnhancedNodeId::Empty {
                boundary: 1,
                subindex: 2,
            }
        );
        assert_eq!(graph.edges()[2].relation(), "conj");
    }

    #[test]
    fn enhanced_graph_normalization_does_not_depend_on_deps_member_order() {
        let forward =
            "1\tx\tx\tX\t_\t_\t0\troot\t0:root|2:dep\t_\n2\ty\ty\tX\t_\t_\t1\tdep\t1:dep\t_\n";
        let reversed =
            "1\tx\tx\tX\t_\t_\t0\troot\t2:dep|0:root\t_\n2\ty\ty\tX\t_\t_\t1\tdep\t1:dep\t_\n";
        let parse = |input: &str| {
            Reader::new(input.as_bytes(), "order.conllu")
                .next()
                .unwrap()
                .unwrap()
        };

        assert_eq!(
            parse(forward).enhanced_graph(),
            parse(reversed).enhanced_graph()
        );
    }

    #[test]
    fn malformed_enhanced_graph_sources_are_refused_at_admission() {
        let cases = [
            (
                "empty node without DEPS",
                "1\tx\tx\tX\t_\t_\t0\troot\t0:root\t_\n1.1\ty\ty\tX\t_\t_\t_\t_\t_\t_\n",
                "empty node 1.1 must carry DEPS",
            ),
            (
                "empty node with basic edge",
                "1\tx\tx\tX\t_\t_\t0\troot\t0:root\t_\n1.1\ty\ty\tX\t_\t_\t1\tdep\t1:dep\t_\n",
                "empty node 1.1 must omit HEAD and DEPREL",
            ),
            (
                "dangling enhanced head",
                "1\tx\tx\tX\t_\t_\t0\troot\t2:dep\t_\n",
                "DEPS head 2 does not resolve",
            ),
            (
                "duplicate enhanced edge",
                "1\tx\tx\tX\t_\t_\t0\troot\t0:root|0:root\t_\n",
                "duplicate DEPS edge 0:root",
            ),
            (
                "malformed enhanced member",
                "1\tx\tx\tX\t_\t_\t0\troot\troot\t_\n",
                "DEPS member must be HEAD:RELATION",
            ),
            (
                "skipped empty subindex",
                "1\tx\tx\tX\t_\t_\t0\troot\t0:root\t_\n1.2\ty\ty\tX\t_\t_\t_\t_\t1:dep\t_\n",
                "empty node subindices after boundary 1 must be consecutive from 1",
            ),
            (
                "noncanonical empty row order",
                "1\tx\tx\tX\t_\t_\t0\troot\t0:root\t_\n2\ty\ty\tX\t_\t_\t1\tdep\t1:dep\t_\n1.1\tz\tz\tX\t_\t_\t_\t_\t1:dep\t_\n",
                "graph node rows must follow CoNLL-U source order",
            ),
        ];

        for (name, input, expected) in cases {
            let error = Reader::new(input.as_bytes(), name)
                .next()
                .unwrap()
                .unwrap_err();
            assert!(
                error.to_string().contains(expected),
                "{name}: expected {expected:?}, got {error}"
            );
        }
    }
}
