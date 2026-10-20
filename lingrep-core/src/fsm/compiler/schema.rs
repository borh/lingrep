//! The catalog-declared key schema.
//!
//! A `schema` block **closes** the descriptive key set for the catalog that
//! declares it; a catalog with no block is unconstrained. Compiler-owned keys —
//! [`POLICY_KEYS`](crate::fsm::policy::POLICY_KEYS) plus
//! [`ANCHOR_KEY`](crate::fsm::policy::ANCHOR_KEY) and
//! [`MAX_NESTING_KEY`](crate::fsm::policy::MAX_NESTING_KEY) — are admitted
//! without declaration and are an error to
//! declare, because the compiler already owns their schema and that is a
//! stronger statement than a declared type. `examples` and `counterexamples`
//! are **not** in that set: they are authored content read by the example
//! verifier, so a catalog may type them.
//!
//! Requiredness is scoped by visibility because a flat `required` is unusable
//! for mixed catalogs: `patterns/sf-experimental.lx` carries
//! `layer`, `category` and `examples` on all 81 of its patterns and on none of
//! its 17 fragments.

use compact_str::CompactString;
use serde_json::Value;
use std::collections::HashMap;

use super::model::{DefinitionVisibility, ParsedDefinition, SourceSpan};

/// The four value shapes a catalog may declare.
///
/// Deliberately not a `map`: a declared `map` would have to say what its keys
/// are to be worth more than "some object", and that is a schema language, not
/// a type. The three compiler-owned map-valued keys are typed by the compiler.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MetadataType {
    String,
    Number,
    Boolean,
    List,
}

impl MetadataType {
    pub(crate) fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "string" => Self::String,
            "number" => Self::Number,
            "boolean" => Self::Boolean,
            "list" => Self::List,
            _ => return None,
        })
    }

    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::String => "string",
            Self::Number => "number",
            Self::Boolean => "boolean",
            Self::List => "list",
        }
    }

    fn admits(self, value: &Value) -> bool {
        match self {
            Self::String => value.is_string(),
            Self::Number => value.is_number(),
            Self::Boolean => value.is_boolean(),
            Self::List => value.is_array(),
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct SchemaEntry {
    pub(crate) visibility: Option<DefinitionVisibility>,
    pub(crate) declared: MetadataType,
    pub(crate) required: bool,
    pub(crate) members: Option<Vec<String>>,
    pub(crate) span: SourceSpan,
}

/// Every declaration one catalog makes, and the refusals they imply.
#[derive(Clone, Debug, Default)]
pub(crate) struct CatalogSchema {
    // Keyed by (key, visibility). One `SchemaEntry` per key cannot represent
    //
    //     pattern confidence: string
    //     fragment confidence: number
    //
    // which the design permits: every definition still has exactly one
    // applicable declaration, so the two do not overlap. What the design
    // forbids is an *unprefixed* entry beside a prefixed one for the same key,
    // and a duplicate of either — both of which this key admits and which
    // `insert` rejects explicitly.
    entries: HashMap<(CompactString, Option<DefinitionVisibility>), SchemaEntry>,
}

/// What a catalog's schema says about one key on one definition.
///
/// Three cases rather than an `Option` and a flag: "declared for the other
/// visibility" carries the visibility it *was* declared for, which only the
/// lookup can know, and making that a variant removes the impossible fourth
/// combination rather than asserting against it.
enum Declaration<'a> {
    Applies(&'a SchemaEntry),
    WrongVisibility(DefinitionVisibility),
    Undeclared,
}

/// One refusal: a message and the span it belongs on.
///
/// Returned rather than built into a diagnostic here so the lowerer keeps sole
/// ownership of its source label and its line index.
pub(crate) struct SchemaViolation {
    pub(crate) span: SourceSpan,
    pub(crate) message: String,
}

/// Whether the compiler, rather than the catalog, owns this key's schema.
///
/// An alias for [`compiler_owned_key`], which moved beside [`POLICY_KEYS`] when
/// `lingrep-web`'s pattern browser needed the same question answered — a key the
/// compiler owns is publication mechanics, and the browser's grouping heuristic
/// has no way to tell that from research metadata on its own. Two copies of
/// this predicate would let the two surfaces drift.
pub(crate) use crate::fsm::policy::compiler_owned_key as compiler_owned;

impl CatalogSchema {
    /// Records one declaration, or says why it cannot be recorded.
    pub(crate) fn insert(
        &mut self,
        key: CompactString,
        entry: SchemaEntry,
    ) -> Result<(), SchemaViolation> {
        if compiler_owned(&key) {
            return Err(SchemaViolation {
                span: entry.span,
                message: format!(
                    "`{key}` is owned by the compiler, whose own grammar types it; a catalog \
                     does not declare it"
                ),
            });
        }
        if entry.members.is_some() && entry.declared != MetadataType::String {
            return Err(SchemaViolation {
                span: entry.span,
                message: format!(
                    "`in {{…}}` enumerates only a `string` declaration; `{key}` is declared \
                     `{}`",
                    entry.declared.name()
                ),
            });
        }
        // An unprefixed declaration and a prefixed one for the same key would
        // both apply to the same definition, so which constrains it would
        // depend on lookup order.
        let conflicts = self.entries.keys().any(|(name, visibility)| {
            *name == key && (visibility.is_none() || entry.visibility.is_none())
        }) || self.entries.contains_key(&(key.clone(), entry.visibility));
        if conflicts {
            return Err(SchemaViolation {
                span: entry.span,
                message: format!(
                    "every definition takes exactly one declaration of `{key}`; declare it once \
                     without a prefix, or once per visibility"
                ),
            });
        }
        self.entries.insert((key, entry.visibility), entry);
        Ok(())
    }

    /// What this catalog says about `key` on a definition of `visibility`.
    fn lookup(&self, key: &str, visibility: Option<DefinitionVisibility>) -> Declaration<'_> {
        if let Some(visibility) = visibility
            && let Some(entry) = self
                .entries
                .get(&(CompactString::from(key), Some(visibility)))
        {
            return Declaration::Applies(entry);
        }
        if let Some(entry) = self.entries.get(&(CompactString::from(key), None)) {
            return Declaration::Applies(entry);
        }
        // Declared, but for the other visibility. Distinguished from undeclared
        // because the two need different messages: one is a typo, the other is
        // a key on the wrong kind of definition. The visibility is carried out
        // of the lookup rather than re-read from the entry, because only the
        // two lookups above can tell the cases apart — an unprefixed entry has
        // already matched by the time control reaches here.
        self.entries
            .iter()
            .find(|((name, _), _)| name.as_str() == key)
            .and_then(|((_, declared), _)| declared.map(Declaration::WrongVisibility))
            .unwrap_or(Declaration::Undeclared)
    }
}

/// Checks every definition's effective metadata against the catalog's schema.
///
/// Runs after materialization, because requiredness is a question about a
/// definition's *effective* metadata — a key supplied by `defaults` satisfies a
/// requirement exactly as one written on the definition does.
pub(crate) fn check_definitions(
    schema: &CatalogSchema,
    definitions: &[ParsedDefinition],
) -> Vec<SchemaViolation> {
    let mut violations = Vec::new();
    for definition in definitions {
        for (key, entry) in definition.metadata.iter() {
            if compiler_owned(key) {
                continue;
            }
            let declaration = match schema.lookup(key, definition.visibility()) {
                Declaration::Applies(declaration) => declaration,
                Declaration::Undeclared => {
                    violations.push(SchemaViolation {
                        span: entry.span,
                        message: format!(
                            "`{key}` is not declared by this catalog's `schema` block"
                        ),
                    });
                    continue;
                }
                Declaration::WrongVisibility(declared_for) => {
                    violations.push(SchemaViolation {
                        span: entry.span,
                        message: format!(
                            "`{key}` is declared for `{}` definitions, and `{}` is not one",
                            declared_for.name(),
                            definition.name
                        ),
                    });
                    continue;
                }
            };
            if !declaration.declared.admits(&entry.value) {
                violations.push(SchemaViolation {
                    span: entry.span,
                    message: format!(
                        "`{key}` is declared `{}`, but carries {}",
                        declaration.declared.name(),
                        entry.value
                    ),
                });
                continue;
            }
            if let Some(members) = &declaration.members
                && let Some(text) = entry.value.as_str()
                && !members.iter().any(|member| member == text)
            {
                violations.push(SchemaViolation {
                    span: entry.span,
                    message: format!(
                        "`{key}` is `{text}`, which is not one of the declared values"
                    ),
                });
            }
        }

        for ((key, visibility), declaration) in &schema.entries {
            if !declaration.required {
                continue;
            }
            if visibility.is_some_and(|declared| Some(declared) != definition.visibility()) {
                continue;
            }
            if definition.metadata.contains_key(key) {
                continue;
            }
            violations.push(SchemaViolation {
                // An absent key has no span of its own, so the diagnostic points
                // at the requirement that was not met.
                span: declaration.span,
                message: format!(
                    "this catalog requires `{key}`, and `{}` does not carry it",
                    definition.name
                ),
            });
        }
    }
    violations
}

#[cfg(test)]
mod tests {
    // Every span assertion below slices the fixture by a byte range the
    // compiler reported. Test code is exempt from the panic deny-floor
    // (docs/lint-policy.md), and a panic here is the test failing, which is
    // what it is for.
    #![allow(clippy::string_slice)]

    use super::super::PatternSource;
    use super::super::limits::CompileLimits;
    use super::super::lower::lower_document;

    fn refuse(source: &str) -> String {
        lower_document(
            &PatternSource::named("t.lx", source),
            &CompileLimits::default(),
        )
        .expect_err("source is refused")
        .to_string()
    }

    /// The byte range the refusal points at.
    ///
    /// Every schema diagnostic asserts through this as well as through
    /// `refuse`. Prose is not a span: a message can name the right key while
    /// the reported range still covers the whole definition. Return the range
    /// rather than the slice
    /// because several cases need *which* occurrence of a repeated key was
    /// reported, not just its text.
    fn refuse_range(source: &str) -> std::ops::Range<usize> {
        lower_document(
            &PatternSource::named("t.lx", source),
            &CompileLimits::default(),
        )
        .expect_err("source is refused")
        .diagnostics
        .first()
        .expect("a diagnostic is present")
        .location()
        .byte_range()
    }

    fn refuse_at(source: &str) -> &str {
        &source[refuse_range(source)]
    }

    fn accept(source: &str) {
        lower_document(
            &PatternSource::named("t.lx", source),
            &CompileLimits::default(),
        )
        .unwrap_or_else(|error| panic!("source must lower: {error}"));
    }

    #[test]
    fn an_undeclared_key_is_refused_under_a_closed_schema() {
        const SOURCE: &str = "schema\n  layer: string\nend\n\nlayr: \"S1\"\np := x.\n";
        let message = refuse(SOURCE);
        assert!(message.contains("`layr` is not declared"), "{message}");
        assert_eq!(
            refuse_at(SOURCE),
            "layr",
            "points at the key the author wrote"
        );
    }

    #[test]
    fn a_compiler_owned_key_needs_no_declaration() {
        accept("schema\n  layer: string\nend\n\nlayer: \"S1\"\noutput_name: \"pub\"\np := x.\n");
    }

    #[test]
    fn declaring_a_compiler_owned_key_is_an_error() {
        for key in ["output_name", "anchor", "max_nesting"] {
            let source = format!("schema\n  {key}: string\nend\np := x.\n");
            let message = refuse(&source);
            assert!(
                message.contains("owned by the compiler"),
                "{key}: {message}"
            );
            assert_eq!(
                refuse_at(&source),
                key,
                "points at the schema entry, which is the line to delete"
            );
        }
    }

    #[test]
    fn examples_and_counterexamples_stay_declarable() {
        accept("schema\n  examples: list\nend\n\nexamples: [\"a\"]\np := x.\n");
        accept("schema\n  counterexamples: list\nend\n\ncounterexamples: [\"a\"]\np := x.\n");
    }

    #[test]
    fn a_type_violation_is_refused_at_the_offending_key() {
        const SOURCE: &str = "schema\n  layer: string\nend\n\nlayer: 1\np := x.\n";
        let message = refuse(SOURCE);
        assert!(message.contains("declared `string`"), "{message}");
        // The annotation, not the schema entry: the schema is right and the
        // value is wrong.
        let range = refuse_range(SOURCE);
        assert_eq!(&SOURCE[range.clone()], "layer");
        assert!(
            range.start > SOURCE.find("\nend").expect("block ends"),
            "the annotation is reported, not the declaration"
        );
    }

    #[test]
    fn a_value_outside_a_declared_set_is_refused_at_the_offending_key() {
        const SOURCE: &str = "schema\n  layer: string in {\"S1\"}\nend\n\nlayer: \"S9\"\np := x.\n";
        let message = refuse(SOURCE);
        assert!(message.contains("not one of"), "{message}");
        assert_eq!(refuse_at(SOURCE), "layer");
    }

    #[test]
    fn in_on_a_non_string_declaration_is_a_schema_error() {
        let message = refuse("schema\n  depth: number in {\"1\"}\nend\np := x.\n");
        assert!(message.contains("only a `string`"), "{message}");
    }

    #[test]
    fn a_missing_required_key_reports_the_schema_entry_span() {
        const SOURCE: &str = "schema\n  pattern layer: string required\nend\n\np := x.\n";
        let message = refuse(SOURCE);
        assert!(message.contains("requires `layer`"), "{message}");
        // An absent key has no span, so the diagnostic points at the
        // requirement. Asserted as a byte range, not as prose containing the
        // word "schema" — a definition-span diagnostic could say that too.
        let range = refuse_range(SOURCE);
        assert_eq!(&SOURCE[range.clone()], "layer");
        assert!(
            range.start < SOURCE.find("\nend").expect("block ends"),
            "the span is inside the schema block, not on the definition"
        );
    }

    #[test]
    fn a_visibility_mismatch_reports_the_offending_annotation() {
        const SOURCE: &str =
            "schema\n  pattern layer: string\nend\n\nlayer: \"S1\"\nfragment f := y.\n";
        let message = refuse(SOURCE);
        assert!(message.contains("declared for `pattern`"), "{message}");
        let range = refuse_range(SOURCE);
        assert_eq!(&SOURCE[range.clone()], "layer");
        assert!(
            range.start > SOURCE.find("\nend").expect("block ends"),
            "the annotation is reported, not the declaration that constrains it"
        );
    }

    #[test]
    fn a_pattern_scoped_key_is_not_required_of_a_fragment() {
        accept(
            "schema\n  pattern layer: string required\nend\n\nlayer: \"S1\"\np := x.\n\
             fragment f := y.\n",
        );
    }

    #[test]
    fn a_key_may_be_declared_once_per_visibility() {
        accept(
            "schema\n  pattern confidence: string\n  fragment confidence: number\nend\n\n\
             confidence: \"high\"\np := x.\n\nconfidence: 3\nfragment f := y.\n",
        );
    }

    #[test]
    fn an_unprefixed_entry_overlapping_a_prefixed_one_is_an_error() {
        let message = refuse("schema\n  layer: string\n  pattern layer: string\nend\np := x.\n");
        assert!(message.contains("exactly one declaration"), "{message}");
    }

    #[test]
    fn a_duplicate_schema_entry_is_an_error() {
        let message = refuse("schema\n  layer: string\n  layer: string\nend\np := x.\n");
        assert!(message.contains("exactly one declaration"), "{message}");
    }

    #[test]
    fn a_catalog_without_a_schema_is_unconstrained() {
        accept("anything: \"goes\"\np := x.\n");
    }

    #[test]
    fn an_empty_schema_closes_the_catalog_to_compiler_owned_keys() {
        accept("schema\nend\n\noutput_name: \"pub\"\np := x.\n");
        let message = refuse("schema\nend\n\nlayer: \"S1\"\np := x.\n");
        assert!(message.contains("`layer` is not declared"), "{message}");
    }

    #[test]
    fn a_second_schema_block_is_an_error() {
        let message = refuse("schema\n  a: string\nend\nschema\n  b: string\nend\np := x.\n");
        assert!(message.contains("one `schema` block"), "{message}");
    }

    #[test]
    fn a_schema_block_after_a_definition_is_an_error() {
        let message = refuse("p := x.\nschema\n  a: string\nend\n");
        assert!(message.contains("must precede"), "{message}");
    }

    #[test]
    fn a_default_satisfies_a_requirement() {
        // Requiredness is checked after materialization, so a key supplied by
        // `defaults` counts exactly as one written on the definition.
        accept(
            "defaults\n  status: \"experimental\"\nend\n\
             schema\n  status: string required\nend\n\np := x.\n",
        );
    }
}
