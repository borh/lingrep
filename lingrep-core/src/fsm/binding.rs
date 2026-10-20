use compact_str::CompactString;
use std::fmt;

use super::{CompiledCatalog, PredicateDigest};
use crate::execution::{BoundSentenceInput, EnhancedGraphInput};
use crate::fsm::types::Match;
use crate::token_match::{
    AttributeKey, AttributeKind, AttributeLocator, AttributeValueRef, BoundProjection,
    BoundTokenSentence, TokenSchema, TokenSchemaDigest, TokenSequence,
};

/// A lingrep patterns catalog whose symbolic attribute names have been resolved against a
/// concrete token schema.
#[derive(Clone)]
pub struct BoundCatalog {
    pub(crate) compiled: CompiledCatalog,
    projection: BoundProjection,
    unavailable: Box<[AttributeKey]>,
}

impl fmt::Debug for BoundCatalog {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BoundCatalog")
            .field("projection", &self.projection)
            .field("unavailable", &self.unavailable)
            .finish_non_exhaustive()
    }
}

/// Why a symbolic lingrep patterns catalog could not bind to a token schema.
///
/// Binding refuses only what would make the catalog's semantics undefined: a
/// dependency `where` clause whose edge attributes the schema cannot supply,
/// or an execution attribute the schema carries under a conflicting value
/// kind. Any other referenced-but-absent attribute binds as universally absent
/// — the three-valued semantics a per-token absence already has — and is
/// reported by [`BoundCatalog::unavailable_attributes`] instead.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SchemaBindError {
    /// Keys a `where` clause requires and the schema does not carry.
    MissingKeys(Vec<CompactString>),
    KindMismatch {
        key: AttributeKey,
        expected: AttributeKind,
        actual: AttributeKind,
    },
}

impl SchemaBindError {
    /// Returns every required symbolic key absent from the target schema.
    #[must_use]
    pub fn missing_keys(&self) -> &[CompactString] {
        match self {
            Self::MissingKeys(keys) => keys,
            Self::KindMismatch { .. } => &[],
        }
    }
}

impl fmt::Display for SchemaBindError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingKeys(keys) => write!(
                formatter,
                "token schema is missing required lingrep patterns keys: {}",
                keys.join(", ")
            ),
            Self::KindMismatch {
                key,
                expected,
                actual,
            } => write!(
                formatter,
                "lingrep patterns key `{key}` requires {} values but the token schema declares {}",
                kind_name(*expected),
                kind_name(*actual),
            ),
        }
    }
}

const fn kind_name(kind: AttributeKind) -> &'static str {
    match kind {
        AttributeKind::Scalar => "scalar",
        AttributeKind::StringSet => "string-set",
    }
}

impl std::error::Error for SchemaBindError {}

/// Why bound execution cannot begin for a projected sentence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BoundExecutionError {
    SchemaMismatch {
        expected: TokenSchemaDigest,
        actual: TokenSchemaDigest,
    },
    EnhancedGraphNotRequested,
    EnhancedGraphWordCountMismatch {
        expected: usize,
        actual: usize,
    },
}

impl fmt::Display for BoundExecutionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SchemaMismatch { .. } => {
                formatter.write_str("bound lingrep patterns catalog and token sentence use different schemas")
            }
            Self::EnhancedGraphNotRequested => formatter.write_str(
                "bound lingrep patterns catalog requires enhanced graph input that the source did not request",
            ),
            Self::EnhancedGraphWordCountMismatch { .. } => formatter
                .write_str("enhanced graph word count differs from the bound token sentence"),
        }
    }
}

impl std::error::Error for BoundExecutionError {}

impl CompiledCatalog {
    /// Resolves the catalog's complete attribute projection against one schema.
    pub fn bind(&self, schema: &TokenSchema) -> Result<BoundCatalog, SchemaBindError> {
        self.bind_with_projection(schema, std::iter::empty::<AttributeKey>())
    }

    /// Resolves matching and caller-required publication attributes together.
    pub fn bind_with_projection<I, K>(
        &self,
        schema: &TokenSchema,
        additional: I,
    ) -> Result<BoundCatalog, SchemaBindError>
    where
        I: IntoIterator<Item = K>,
        K: Into<AttributeKey>,
    {
        let mut required: Vec<AttributeKey> = self.required_attributes();
        required.extend(additional.into_iter().map(Into::into));
        required.sort_unstable();
        required.dedup();
        let (available, unavailable): (Vec<AttributeKey>, Vec<AttributeKey>) = required
            .into_iter()
            .partition(|attribute| schema.keys().iter().any(|key| key == attribute));
        // A `where` clause's edge attributes are the one absence that is a
        // refusal rather than a report: without them every dependency
        // constraint is meaningless, not merely unsatisfied.
        let refused: Vec<CompactString> = self
            .relation_attributes()
            .iter()
            .filter(|attribute| unavailable.contains(attribute))
            .map(|attribute| CompactString::new(attribute.as_str()))
            .collect();
        if !refused.is_empty() {
            return Err(SchemaBindError::MissingKeys(refused));
        }
        for (key, expected) in self.required_attribute_kinds() {
            // An unavailable key binds as absent, so it has no kind to conflict.
            let Some(actual) = schema.kind_for_key(&key) else {
                continue;
            };
            if actual != expected {
                return Err(SchemaBindError::KindMismatch {
                    key,
                    expected,
                    actual,
                });
            }
        }
        let projection = schema
            .bind_projection(available)
            .expect("all projected attributes were checked against this schema");
        let mut compiled = self.clone();
        compiled.bind_attributes(&projection);
        if self.sentence_filter.as_ref().is_some_and(|filter| {
            self.catalog_policy().declared()
                || !self.lexicons().is_empty()
                || filter.clauses().iter().flatten().any(|term| {
                    schema.kind_for_key(term.attribute()) != Some(AttributeKind::Scalar)
                })
        }) {
            compiled.sentence_filter = None;
        }
        Ok(BoundCatalog {
            compiled,
            projection,
            unavailable: unavailable.into_boxed_slice(),
        })
    }
}

impl BoundCatalog {
    /// A necessary sentence condition; passing it does not establish a match.
    #[must_use]
    pub fn sentence_filter(&self) -> Option<&super::SentenceFilter> {
        self.compiled.sentence_filter.as_ref()
    }

    /// Executes one complete bound sentence input.
    pub fn find_matches_input(
        &self,
        input: &BoundSentenceInput,
    ) -> Result<Vec<Match>, BoundExecutionError> {
        self.validate_tokens(input.words())?;
        if !self.compiled.requires_enhanced_graph() {
            return Ok(self.compiled.find_matches(&BoundExecutionTokens {
                tokens: input.words(),
                projection: &self.projection,
            }));
        }
        match input.enhanced_graph() {
            EnhancedGraphInput::NotRequested => Err(BoundExecutionError::EnhancedGraphNotRequested),
            EnhancedGraphInput::Observation(observation) => {
                let Some(graph) = observation.complete() else {
                    return Ok(self.compiled.find_matches(&BoundExecutionTokens {
                        tokens: input.words(),
                        projection: &self.projection,
                    }));
                };
                if graph.word_count() != input.words().len() {
                    return Err(BoundExecutionError::EnhancedGraphWordCountMismatch {
                        expected: input.words().len(),
                        actual: graph.word_count(),
                    });
                }
                Ok(self.compiled.find_matches_with_complete_graph(
                    &BoundExecutionTokens {
                        tokens: input.words(),
                        projection: &self.projection,
                    },
                    graph,
                ))
            }
        }
    }

    #[must_use]
    pub fn requires_enhanced_graph(&self) -> bool {
        self.compiled.requires_enhanced_graph()
    }

    /// Returns the schema-local dense projection used by this catalog.
    #[must_use]
    pub fn projection(&self) -> &BoundProjection {
        &self.projection
    }

    /// Referenced attributes the bound schema does not carry, sorted.
    ///
    /// Each one is bound as universally absent: positive tests over it match
    /// no token, absence-sensitive exclusions (`!=`, `!~`, `not in`) match no
    /// token either, and only the true complement `!(…)` holds. A catalog can
    /// legitimately rely on that — a complemented test over another analyzer's
    /// attribute still means what it says — so this is a report for the
    /// caller's diagnostics, not an error.
    #[must_use]
    pub fn unavailable_attributes(&self) -> &[AttributeKey] {
        &self.unavailable
    }

    /// Returns the structural digest of the bound catalog's predicates.
    #[must_use]
    pub fn predicate_digest(&self) -> PredicateDigest {
        self.compiled.predicate_digest()
    }

    pub fn catalog_policy(&self) -> &crate::fsm::CatalogPolicy {
        self.compiled.catalog_policy()
    }

    pub fn anchor_evaluator(&self) -> crate::fsm::AnchorEvaluator<'_> {
        self.compiled.anchor_evaluator()
    }

    /// Checks the schema identity before any token execution begins.
    pub fn validate_tokens(&self, tokens: &BoundTokenSentence) -> Result<(), BoundExecutionError> {
        let expected = self.projection.schema_digest();
        let actual = tokens.schema_digest();
        if expected != actual {
            return Err(BoundExecutionError::SchemaMismatch { expected, actual });
        }
        Ok(())
    }

    /// Executes this catalog after validating the sentence schema.
    pub fn find_matches(
        &self,
        tokens: &BoundTokenSentence,
    ) -> Result<Vec<Match>, BoundExecutionError> {
        self.validate_tokens(tokens)?;
        Ok(self.compiled.find_matches(&BoundExecutionTokens {
            tokens,
            projection: &self.projection,
        }))
    }

    /// Runs schema-bound matching while collecting benchmark execution work.
    #[cfg(feature = "bench-instrumentation")]
    pub fn find_matches_with_execution_metrics(
        &self,
        tokens: &BoundTokenSentence,
    ) -> Result<(Vec<Match>, super::ExecutionMetrics), BoundExecutionError> {
        self.validate_tokens(tokens)?;
        Ok(self
            .compiled
            .find_matches_with_execution_metrics(&BoundExecutionTokens {
                tokens,
                projection: &self.projection,
            }))
    }

    /// Runs schema-bound matching while collecting benchmark assertion work.
    #[cfg(feature = "bench-instrumentation")]
    pub fn find_matches_with_assertion_metrics(
        &self,
        tokens: &BoundTokenSentence,
    ) -> Result<(Vec<Match>, super::AssertionMetrics), BoundExecutionError> {
        self.validate_tokens(tokens)?;
        Ok(self
            .compiled
            .find_matches_with_assertion_metrics(&BoundExecutionTokens {
                tokens,
                projection: &self.projection,
            }))
    }
}

struct BoundExecutionTokens<'a> {
    tokens: &'a BoundTokenSentence,
    projection: &'a BoundProjection,
}

impl TokenSequence for BoundExecutionTokens<'_> {
    fn len(&self) -> usize {
        self.tokens.len()
    }

    fn get(
        &self,
        token: usize,
        attribute: crate::token_match::TokenAttributeIndex,
    ) -> Option<&CompactString> {
        TokenSequence::get(self.tokens, token, attribute)
    }

    fn value_key(&self, token: usize, key: &AttributeKey) -> Option<AttributeValueRef<'_>> {
        let ordinal = self.projection.ordinal_for_key(key)?;
        self.tokens.token(token)?.value(ordinal)
    }

    fn value_locator(
        &self,
        token: usize,
        locator: &AttributeLocator,
    ) -> Option<AttributeValueRef<'_>> {
        let ordinal = locator.ordinal()?;
        self.tokens.token(token)?.value(ordinal)
    }

    fn get_key(&self, token: usize, key: &AttributeKey) -> Option<&CompactString> {
        match self.value_key(token, key)? {
            AttributeValueRef::Scalar(value) => {
                let ordinal = self.projection.ordinal_for_key(key)?;
                self.tokens.token(token)?.get(ordinal)?;
                // Scalar storage owns `CompactString` and the typed read
                // surface returns `&str`, so no `&CompactString` is lent here;
                // scalar values are read through `value_key`.
                let _ = value;
                None
            }
            AttributeValueRef::StringSet(_) => None,
        }
    }
}
