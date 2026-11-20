use lingrep_core::analysis::{
    AnalyzedInput, AnalyzerFamily, ExtractionProfile, declared_token_schema,
};
use lingrep_core::corpus::conllu::EnhancedNodeId;
use lingrep_core::execution::{BoundSentenceInput, EnhancedGraphInput};
use lingrep_core::fsm::{
    BoundCatalog, BoundSentenceMatcher, Capture, CompileLimits, CompiledCatalog, GraphBinding,
    Interpretation, Match, PatternSource, compile_catalog, normalize_inline_pattern,
    render_diagnostics, render_pattern_error,
};
use lingrep_core::token_match::{
    AttributeKey, AttributeKind, AttributeOrdinal, AttributeValueRef, BoundProjection,
    BoundTokenSentence, CharSpan, ProjectedAttributeValue, SchemaAttribute, TokenSchema,
    TokenSequence,
};
use pyo3::exceptions::{PyAttributeError, PyIndexError, PyKeyError};
use pyo3::prelude::*;
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex, OnceLock};

use super::invalid;

fn key_list(keys: &[AttributeKey]) -> String {
    keys.iter()
        .map(AttributeKey::as_str)
        .collect::<Vec<_>>()
        .join(", ")
}

/// The attributes an analyzer can supply for each token.
#[pyclass(frozen, eq, skip_from_py_object, module = "lingrep")]
#[derive(Clone)]
pub struct Schema(pub Arc<TokenSchema>);

impl PartialEq for Schema {
    fn eq(&self, other: &Self) -> bool {
        self.0.digest() == other.0.digest()
    }
}

#[pymethods]
impl Schema {
    #[getter]
    fn attributes(&self) -> Vec<&str> {
        self.0.keys().iter().map(AttributeKey::as_str).collect()
    }

    fn __repr__(&self) -> String {
        format!("Schema({})", key_list(self.0.keys()))
    }

    /// `fields` pairs each key with whether it holds a string set, in the order
    /// the adapter lays out a token's values.
    #[staticmethod]
    fn _from_fields(fields: Vec<(String, bool)>) -> PyResult<Self> {
        let schema = TokenSchema::new(
            fields
                .iter()
                .map(|(key, set)| {
                    SchemaAttribute::new(
                        key,
                        if *set {
                            AttributeKind::StringSet
                        } else {
                            AttributeKind::Scalar
                        },
                    )
                })
                .collect::<Result<Vec<_>, _>>()
                .map_err(invalid)?,
        )
        .map_err(invalid)?;
        if fields.is_empty()
            || !schema
                .keys()
                .iter()
                .map(AttributeKey::as_str)
                .eq(fields.iter().map(|field| field.0.as_str()))
        {
            return Err(invalid("expected attributes in canonical projection order"));
        }
        Ok(Self(Arc::new(schema)))
    }
}

#[pyclass(frozen, module = "lingrep")]
pub struct Sentence {
    byte_offsets: OnceLock<Vec<usize>>,
    text: String,
    words: BoundTokenSentence,
    schema: Arc<TokenSchema>,
    projection: Arc<BoundProjection>,
}

impl Sentence {
    pub fn native(
        item: AnalyzedInput,
        schema: Arc<TokenSchema>,
        projection: Arc<BoundProjection>,
    ) -> Self {
        let (text, sentences) = item.into_parts();
        Self {
            byte_offsets: OnceLock::new(),
            text: text.text().to_owned(),
            words: sentences.into_vec().remove(0).into_words(),
            schema,
            projection,
        }
    }

    fn unknown(&self, key: &str) -> PyErr {
        PyKeyError::new_err(format!(
            "sentence carries no attribute {key}; it has {}",
            key_list(self.projection.keys())
        ))
    }

    /// The source text a token covers, for sentences that do not project `surface`.
    fn source_surface(&self, index: usize) -> Option<&str> {
        let span = self.words.char_span(index)?;
        let offsets = self.byte_offsets.get_or_init(|| {
            self.text
                .char_indices()
                .map(|(i, _)| i)
                .chain([self.text.len()])
                .collect()
        });
        self.text
            .get(*offsets.get(span.start as usize)?..*offsets.get(span.end as usize)?)
    }

    fn ordinal(&self, key: &str) -> PyResult<Option<AttributeOrdinal>> {
        match self.projection.ordinal_key(key) {
            None if key != "surface" => Err(self.unknown(key)),
            value => Ok(value),
        }
    }

    fn cell(&self, index: usize, key: &str) -> PyResult<Option<ColumnValue<'_>>> {
        Ok(self.value(index, self.ordinal(key)?))
    }

    fn value(&self, index: usize, ordinal: Option<AttributeOrdinal>) -> Option<ColumnValue<'_>> {
        let Some(ordinal) = ordinal else {
            return self.source_surface(index).map(ColumnValue::Scalar);
        };
        self.words
            .token(index)
            .and_then(|word| word.value(ordinal))
            .map(|cell| match cell {
                AttributeValueRef::Scalar(value) => ColumnValue::Scalar(value),
                AttributeValueRef::StringSet(values) => {
                    ColumnValue::Set(values.iter().map(AsRef::as_ref).collect())
                }
            })
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Cell {
    Scalar(String),
    Set(Vec<String>),
}

#[derive(Serialize)]
#[serde(untagged)]
enum ColumnValue<'a> {
    Scalar(&'a str),
    Set(Vec<&'a str>),
}

#[pymethods]
impl Sentence {
    #[getter]
    fn text(&self) -> &str {
        &self.text
    }

    #[getter]
    fn schema(&self) -> Schema {
        Schema(self.schema.clone())
    }

    /// The attributes this sentence carries, a subset of its schema's.
    #[getter]
    fn attributes(&self) -> Vec<&str> {
        self.projection
            .keys()
            .iter()
            .map(AttributeKey::as_str)
            .collect()
    }

    fn __len__(&self) -> usize {
        self.words.len()
    }

    fn __getitem__(slf: Bound<'_, Self>, index: isize) -> PyResult<Token> {
        let len = slf.get().words.len();
        let resolved = if index < 0 {
            index + len as isize
        } else {
            index
        };
        if resolved < 0 || resolved as usize >= len {
            return Err(PyIndexError::new_err("token index out of range"));
        }
        Ok(Token {
            sentence: slf.unbind(),
            index: resolved as usize,
        })
    }

    fn __iter__(slf: Py<Self>) -> TokenIter {
        TokenIter {
            sentence: slf,
            index: 0,
        }
    }

    fn __repr__(&self) -> String {
        format!("Sentence({:?}, {} tokens)", self.text, self.words.len())
    }

    #[getter]
    fn char_spans<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let spans = (0..self.words.len())
            .map(|index| char_span(&self.words, index, index + 1))
            .collect::<Vec<_>>();
        Ok(pythonize::pythonize(py, &spans)?)
    }

    fn _is_set(&self, key: &str) -> PyResult<bool> {
        Ok(self
            .ordinal(key)?
            .and_then(|ordinal| self.projection.kind(ordinal))
            == Some(AttributeKind::StringSet))
    }

    fn column<'py>(&self, py: Python<'py>, key: &str) -> PyResult<Bound<'py, PyAny>> {
        let ordinal = self.ordinal(key)?;
        let column = (0..self.words.len())
            .map(|index| self.value(index, ordinal))
            .collect::<Vec<_>>();
        Ok(pythonize::pythonize(py, &column)?)
    }

    #[staticmethod]
    fn _from_projection(
        text: String,
        schema: &Schema,
        values: &Bound<'_, PyAny>,
        ranges: Vec<(usize, usize)>,
        word_ranges: Vec<(usize, usize)>,
        word_count: usize,
    ) -> PyResult<Self> {
        let schema = schema.0.clone();
        let projection = schema
            .bind_projection(schema.keys().iter().cloned())
            .map_err(invalid)?;
        let cells: Vec<Option<Cell>> = pythonize::depythonize(values)?;
        let cells = cells
            .into_iter()
            .map(|cell| match cell {
                Some(Cell::Scalar(value)) => ProjectedAttributeValue::scalar(value),
                Some(Cell::Set(values)) => ProjectedAttributeValue::string_set(values),
                None => ProjectedAttributeValue::missing_scalar(),
            })
            .collect();
        let words =
            BoundTokenSentence::from_projected_attribute_values(&projection, word_count, cells)
                .map_err(invalid)?;
        if ranges.len() != word_ranges.len() {
            return Err(invalid("incomplete surface alignment"));
        }
        let offsets = CharSpan::from_surface_ranges(
            &text,
            word_count,
            ranges
                .into_iter()
                .zip(word_ranges)
                .map(|((a, b), (i, j))| (a..b, i..j)),
        )
        .ok_or_else(|| invalid("invalid adapter token alignment"))?;
        Ok(Self {
            byte_offsets: OnceLock::new(),
            text,
            words: words.with_char_offsets(Some(Arc::new(offsets))),
            schema,
            projection: Arc::new(projection),
        })
    }
}

#[pyclass(module = "lingrep")]
struct TokenIter {
    sentence: Py<Sentence>,
    index: usize,
}

#[pymethods]
impl TokenIter {
    fn __iter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
        slf
    }

    fn __next__(&mut self, py: Python<'_>) -> Option<Token> {
        if self.index == self.sentence.get().words.len() {
            return None;
        }
        let index = self.index;
        self.index += 1;
        Some(Token {
            sentence: self.sentence.clone_ref(py),
            index,
        })
    }
}

/// One token of a `Sentence`; attribute values are read from the sentence.
#[pyclass(frozen, module = "lingrep")]
pub struct Token {
    sentence: Py<Sentence>,
    index: usize,
}

#[pymethods]
impl Token {
    #[getter]
    fn index(&self) -> usize {
        self.index
    }

    #[getter]
    fn char_span<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let span = char_span(&self.sentence.get().words, self.index, self.index + 1);
        Ok(pythonize::pythonize(py, &span)?)
    }

    fn __getitem__<'py>(&self, py: Python<'py>, key: &str) -> PyResult<Bound<'py, PyAny>> {
        Ok(pythonize::pythonize(
            py,
            &self.sentence.get().cell(self.index, key)?,
        )?)
    }

    fn __getattr__<'py>(&self, py: Python<'py>, name: &str) -> PyResult<Bound<'py, PyAny>> {
        let cell = self
            .sentence
            .get()
            .cell(self.index, name)
            .map_err(|_| PyAttributeError::new_err(name.to_owned()))?;
        Ok(pythonize::pythonize(py, &cell)?)
    }

    fn __repr__(&self) -> PyResult<String> {
        let sentence = self.sentence.get();
        Ok(match sentence.cell(self.index, "surface")? {
            Some(ColumnValue::Scalar(surface)) => format!("Token({surface:?})"),
            _ => format!("Token(index={})", self.index),
        })
    }
}

#[pyclass(frozen, module = "lingrep")]
pub struct CompiledQuery {
    catalog: CompiledCatalog,
    schema: Arc<TokenSchema>,
    /// Bound to the attributes the query reads and no others.
    minimal: Arc<BoundCatalog>,
    /// The binding for the most recent other projection `find_matches` saw.
    recent: Mutex<Option<Arc<BoundCatalog>>>,
    #[pyo3(get)]
    diagnostics: Vec<String>,
}

impl CompiledQuery {
    /// Returns the catalog bound to exactly the attributes `projection` lays out.
    fn bound_for(&self, projection: &BoundProjection) -> PyResult<Arc<BoundCatalog>> {
        if projection.schema_digest() != self.schema.digest() {
            return Err(invalid("query was compiled for a different token schema"));
        }
        if self.minimal.projection().keys() == projection.keys() {
            return Ok(self.minimal.clone());
        }
        let mut recent = self.recent.lock().expect("no panic while binding");
        if let Some(bound) = recent.as_ref()
            && bound.projection().keys() == projection.keys()
        {
            return Ok(bound.clone());
        }
        let bound = self
            .catalog
            .bind_with_projection(&self.schema, projection.keys().iter().cloned())
            .map_err(invalid)?;
        if bound.projection().keys() != projection.keys() {
            let missing = bound
                .projection()
                .keys()
                .iter()
                .filter(|key| !projection.keys().contains(key))
                .cloned()
                .collect::<Vec<_>>();
            return Err(invalid(format!(
                "sentence lacks attributes the query reads: {}",
                key_list(&missing)
            )));
        }
        let bound = Arc::new(bound);
        *recent = Some(bound.clone());
        Ok(bound)
    }
}

#[derive(Serialize)]
struct RawSpan<T> {
    start: T,
    end: T,
}

#[derive(Serialize)]
struct RawCapture<'a> {
    name: &'a str,
    hygienic_path: &'a str,
    span: RawSpan<usize>,
    char_span: Option<RawSpan<u32>>,
}

#[derive(Serialize)]
struct RawMatch<'a> {
    pattern: &'a str,
    span: RawSpan<usize>,
    char_span: Option<RawSpan<u32>>,
    captures: Vec<RawCapture<'a>>,
    interpretations: &'a [Interpretation],
    graph_bindings: Vec<RawBinding<'a>>,
    lexical: Option<RawLexical<'a>>,
}

#[derive(Serialize)]
struct RawLexical<'a> {
    term: &'a str,
    matched_by: Vec<&'a str>,
}

#[derive(Serialize)]
#[serde(tag = "node_kind", rename_all = "snake_case")]
enum RawNode {
    Word { token_index: usize },
    Empty { boundary: usize, subindex: usize },
}

impl From<EnhancedNodeId> for RawNode {
    fn from(node: EnhancedNodeId) -> Self {
        match node {
            EnhancedNodeId::Word { token_index } => Self::Word { token_index },
            EnhancedNodeId::Empty { boundary, subindex } => Self::Empty { boundary, subindex },
        }
    }
}

#[derive(Serialize)]
struct RawNodeAttribute<'a> {
    name: &'a str,
    value: &'a str,
}

/// Field names and tags follow `lingrep.query/v2` graph bindings, which
/// docs/pattern-language-reference.md describes.
#[derive(Serialize)]
#[serde(tag = "binding_kind", rename_all = "snake_case")]
enum RawBinding<'a> {
    Measure {
        name: &'a str,
        value: Option<f64>,
        status: &'static str,
    },
    Count {
        name: &'a str,
        value: Option<usize>,
    },
    Node {
        name: &'a str,
        node: RawNode,
        attributes: Vec<RawNodeAttribute<'a>>,
    },
    Edge {
        name: &'a str,
        head: RawNode,
        dependent: RawNode,
        relation: &'a str,
    },
}

impl<'a> From<&'a GraphBinding> for RawBinding<'a> {
    fn from(binding: &'a GraphBinding) -> Self {
        match binding {
            GraphBinding::Measure { name, value } => Self::Measure {
                name,
                value: value.number(),
                status: value.status(),
            },
            GraphBinding::Count { name, value } => Self::Count {
                name,
                value: *value,
            },
            GraphBinding::Node {
                name,
                node,
                attributes,
            } => Self::Node {
                name,
                node: (*node).into(),
                attributes: attributes
                    .iter()
                    .map(|attribute| RawNodeAttribute {
                        name: attribute.name.as_str(),
                        value: &attribute.value,
                    })
                    .collect(),
            },
            GraphBinding::Edge {
                name,
                head,
                dependent,
                relation,
            } => Self::Edge {
                name,
                head: (*head).into(),
                dependent: (*dependent).into(),
                relation,
            },
        }
    }
}

fn char_span(words: &BoundTokenSentence, start: usize, end: usize) -> Option<RawSpan<u32>> {
    if start >= end {
        return None;
    }
    Some(RawSpan {
        start: words.char_span(start)?.start,
        end: words.char_span(end - 1)?.end,
    })
}

#[pymethods]
impl CompiledQuery {
    #[staticmethod]
    #[pyo3(signature = (source, name, inline, schema=None))]
    fn _compile(
        py: Python<'_>,
        source: String,
        name: String,
        inline: bool,
        schema: Option<&Schema>,
    ) -> PyResult<Self> {
        let schema = match schema {
            Some(schema) => schema.0.clone(),
            None => Arc::new(
                declared_token_schema(AnalyzerFamily::Vibrato, ExtractionProfile::Morphology)
                    .expect("Vibrato morphology declares a token schema"),
            ),
        };
        py.detach(|| {
            let text = if inline {
                normalize_inline_pattern(&source)
            } else {
                source
            };
            let output =
                compile_catalog(PatternSource::named(&name, &text), CompileLimits::default())
                    .map_err(|error| invalid(render_pattern_error(Some(&text), &error)))?;
            if !output.matcher.lookaround_reach().is_empty() {
                return Err(invalid(
                    "cross-sentence lookarounds require a corpus search",
                ));
            }
            let minimal = output.matcher.bind(&schema).map_err(invalid)?;
            if minimal.requires_enhanced_graph() {
                return Err(invalid(
                    "enhanced dependency graphs are unavailable in sentence schema",
                ));
            }
            let mut diagnostics = output
                .diagnostics
                .iter()
                .map(|d| render_diagnostics(Some(&text), std::slice::from_ref(d)))
                .collect::<Vec<_>>();
            diagnostics.extend(
                minimal.unavailable_attributes().iter().map(|key| {
                    format!("attribute_unavailable: {} in sentence schema", key.as_str())
                }),
            );
            Ok(Self {
                catalog: output.matcher,
                schema,
                minimal: Arc::new(minimal),
                recent: Mutex::new(None),
                diagnostics,
            })
        })
    }

    #[getter]
    fn schema(&self) -> Schema {
        Schema(self.schema.clone())
    }

    /// The schema attributes the query reads.
    #[getter]
    fn attributes(&self) -> Vec<&str> {
        self.minimal
            .projection()
            .keys()
            .iter()
            .map(AttributeKey::as_str)
            .collect()
    }

    fn __repr__(&self) -> String {
        format!(
            "CompiledQuery(reads {})",
            key_list(self.minimal.projection().keys())
        )
    }

    fn find_matches<'py>(
        &self,
        py: Python<'py>,
        sentence: &Sentence,
    ) -> PyResult<Bound<'py, PyAny>> {
        let bound = self.bound_for(&sentence.projection)?;
        let words = &sentence.words;
        let matches = py.detach(|| bound.find_matches(words)).map_err(invalid)?;
        Ok(pythonize::pythonize(py, &records(&matches, words))?)
    }

    fn _batch<'py>(
        &self,
        py: Python<'py>,
        engine: &super::Engine,
        texts: Vec<String>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let (inputs, matches) = py.detach(|| {
            let batch = engine.analyze(texts, self.minimal.projection())?;
            let inputs = batch
                .into_inputs()
                .into_vec()
                .into_iter()
                .map(|input| {
                    let (_, sentences) = input.into_parts();
                    BoundSentenceInput::new(
                        sentences.into_vec().remove(0).into_words(),
                        EnhancedGraphInput::NotRequested,
                    )
                })
                .collect::<Vec<_>>();
            let matches = BoundSentenceMatcher::new(&self.minimal)
                .match_all(&engine.pool, &inputs)
                .map_err(invalid)?;
            Ok::<_, PyErr>((inputs, matches))
        })?;
        let rows = inputs
            .iter()
            .zip(&matches)
            .map(|(input, hits)| records(hits, input.words()))
            .collect::<Vec<_>>();
        Ok(pythonize::pythonize(py, &rows)?)
    }
}

fn records<'a>(matches: &'a [Match], words: &BoundTokenSentence) -> Vec<RawMatch<'a>> {
    matches
        .iter()
        .map(|m| RawMatch {
            pattern: &m.pattern,
            span: RawSpan {
                start: m.start,
                end: m.end,
            },
            char_span: char_span(words, m.start, m.end),
            captures: m
                .captures
                .iter()
                .map(|c: &Capture| RawCapture {
                    name: &c.name,
                    hygienic_path: &c.path,
                    span: RawSpan {
                        start: c.start,
                        end: c.end,
                    },
                    char_span: char_span(words, c.start, c.end),
                })
                .collect(),
            interpretations: &m.interpretations,
            graph_bindings: m.graph_bindings.iter().map(RawBinding::from).collect(),
            lexical: m.lexical.as_deref().map(|lexical| RawLexical {
                term: &lexical.term,
                matched_by: lexical
                    .matched_by
                    .iter()
                    .map(lingrep_core::token_match::AttributeKey::as_str)
                    .collect(),
            }),
        })
        .collect()
}
