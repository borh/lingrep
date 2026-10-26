use crate::fsm::{AttrValue, AttrValueType, KeyComponent, KeyProjection, PositionAssertion};
use crate::token_match::{AttributeKey, TokenAttributeIndex};
use compact_str::CompactString;
use regex::Regex;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::sync::Arc;
use tree_sitter::{Node, Parser};
use tree_sitter_lingrep::LANGUAGE;

use super::PatternSource;
use super::diagnostic::{CompileError, Diagnostic, DiagnosticCode};
use super::limits::CompileLimits;
use super::model::{
    CaptureProjection, DefinitionVisibility, GroupOperator, LookDirection, LookPolarity,
    LookaroundExpr, Metadata, MetadataEntry, ParsedAnalyzerConstraint, ParsedAttr, ParsedAttrValue,
    ParsedDefinition, ParsedDefinitionBody, ParsedExpr, ParsedGroupDecl, ParsedKey,
    ParsedLabelTerm, ParsedProgram, ParsedRelation, ParsedRelationTarget, ParsedSet, ParsedTest,
    ParsedTestDecl, RelationLabel, RelationLabels, RelationPolarity, SourceId, SourceRegistry,
    SourceSpan,
};
use super::policy_program;
use super::schema::{self, CatalogSchema, MetadataType, SchemaEntry};
use super::source_text::parser_text;

/// Lowers one complete lingrep patterns document without resolving its references.
pub(super) fn lower_document(
    source: &PatternSource<'_>,
    limits: &CompileLimits,
) -> Result<ParsedProgram, CompileError> {
    lower_document_with_stats(source, limits).map(|output| output.program)
}

pub(super) struct LoweredProgram {
    pub(super) program: ParsedProgram,
    pub(super) source_bytes: usize,
    pub(super) metadata_bytes: usize,
    /// Non-fatal diagnostics raised while reading the syntax tree.
    ///
    /// Carried out beside the program rather than raised through `Result`,
    /// because a warning by definition does not stop the lowering that found
    /// it. Each one is already a resolved [`Diagnostic`] holding this
    /// document's own label, line and column, so an accumulating builder
    /// appends without remapping — unlike a `SourceSpan`, which is relative to
    /// a `SourceId` the accumulator reassigns.
    pub(super) warnings: Vec<Diagnostic>,
}

pub(super) struct LoweredDefinition {
    pub(super) definition: ParsedDefinition,
    pub(super) sources: SourceRegistry,
    pub(super) source_bytes: usize,
    pub(super) metadata_bytes: usize,
    pub(super) warnings: Vec<Diagnostic>,
}

/// Lowers one complete lingrep patterns document and returns measurements made while
/// validating the strict syntax tree.
pub(super) fn lower_document_with_stats(
    source: &PatternSource<'_>,
    limits: &CompileLimits,
) -> Result<LoweredProgram, CompileError> {
    let source_bytes = source
        .text
        .len()
        .checked_add(source.name.len())
        .ok_or_else(|| source_byte_limit_error(source.text.len()))?;
    if source_bytes > limits.max_source_bytes() {
        return Err(source_byte_limit_error(source.text.len()));
    }

    // The source-byte budget includes both lingrep patterns text and its UTF-8 diagnostic
    // label. Admission is checked above before the compiler owns either the
    // label or its line index.
    let sources = SourceRegistry::single(source.name, source.text)
        .map_err(|_| source_registry_error(source.text.len()))?;
    let source_label = sources.source_label(SourceId::new(0));

    let mut parser = Parser::new();
    parser
        .set_language(&LANGUAGE.into())
        .map_err(|error| CompileError::new_with_source_label(Arc::clone(&source_label), error))?;
    // The pinned grammar's metadata token wins over `begin\r` when fed CRLF.
    // Replacing only the CR byte with LF gives the parser an equivalent,
    // same-length view while keeping every node offset aligned to the caller's
    // original source.
    let parser_text = parser_text(source.text);
    let tree = parser.parse(parser_text.as_ref(), None).ok_or_else(|| {
        CompileError::new_with_source_label(
            Arc::clone(&source_label),
            "failed to parse lingrep patterns source",
        )
    })?;
    let root = tree.root_node();
    let lowerer = Lowerer::new(source, limits, sources, source_label, source_bytes);

    if root.kind() != "source_file" {
        return Err(lowerer.error(
            root,
            DiagnosticCode::UnknownNode,
            "unknown lingrep patterns document root",
        ));
    }

    lowerer.lower_root(root)
}

/// Lowers exactly one definition and discards its measurements.
#[cfg(test)]
pub(super) fn lower_definition(
    source: &PatternSource<'_>,
    limits: &CompileLimits,
) -> Result<ParsedDefinition, CompileError> {
    lower_definition_with_stats(source, limits).map(|output| output.definition)
}

pub(super) fn lower_definition_with_stats(
    source: &PatternSource<'_>,
    limits: &CompileLimits,
) -> Result<LoweredDefinition, CompileError> {
    let lowered = lower_document_with_stats(source, limits)?;
    let ParsedProgram {
        mut definitions,
        sources,
        // Catalog-level declarations are admitted only by `add_document`.
        policy: _,
        analyzers: _,
        sets: _,
        tests: _,
        groups: _,
    } = lowered.program;
    if definitions.len() != 1 {
        let source_label = sources.source_label(SourceId::new(0));
        return Err(CompileError::from_diagnostics(vec![
            Diagnostic::error_with_source_label(
                DiagnosticCode::Syntax,
                source_label,
                0..source.text.len(),
                "expected exactly one lingrep patterns definition",
                1,
                1,
            ),
        ]));
    }
    Ok(LoweredDefinition {
        definition: definitions.remove(0),
        sources,
        source_bytes: lowered.source_bytes,
        metadata_bytes: lowered.metadata_bytes,
        warnings: lowered.warnings,
    })
}

fn source_byte_limit_error(source_text_bytes: usize) -> CompileError {
    CompileError::from_diagnostics(vec![Diagnostic::error(
        DiagnosticCode::LimitExceeded,
        "<pattern>",
        0..source_text_bytes,
        "lingrep patterns source exceeds the source byte limit",
        1,
        1,
    )])
}

fn source_registry_error(source_text_bytes: usize) -> CompileError {
    CompileError::from_diagnostics(vec![Diagnostic::error(
        DiagnosticCode::LimitExceeded,
        "<pattern>",
        0..source_text_bytes,
        "source registry exceeds the supported offset range",
        1,
        1,
    )])
}

struct Lowerer<'a, 'src> {
    source: &'a PatternSource<'src>,
    limits: &'a CompileLimits,
    sources: SourceRegistry,
    source_label: Arc<str>,
    source_bytes: usize,
    source_id: SourceId,
    definitions: usize,
    expression_nodes: usize,
    metadata_bytes: usize,
    regex_bytes: usize,
    warnings: Vec<Diagnostic>,
}

/// One lowered `metadata_entry`: a value to materialize, or an unset.
///
/// `Unset` is delete-with-provenance: the key ends up absent from the
/// definition, but the site that merges the annotation run over the file
/// defaults still knows an unset happened there and where it was written —
/// which is what lets `UnsetWithoutDefault` point at the key. Only a
/// per-definition annotation run may hold one; `lower_entry_block` refuses it
/// for `defaults`, where the entry *is* the default.
#[derive(Clone)]
enum LoweredEntry {
    MetadataSet(MetadataEntry),
    KeySet(ParsedKey),
    Unset { span: SourceSpan },
}

impl LoweredEntry {
    /// The span of the entry's key, whichever kind of entry it is.
    fn span(&self) -> SourceSpan {
        match self {
            Self::MetadataSet(entry) => entry.span,
            Self::KeySet(key) => key.source_span,
            Self::Unset { span } => *span,
        }
    }
}

/// The catalog being accumulated from one document, and the rules that govern
/// how its parts arrive.
///
/// `lower_root` used to keep these as eight locals and enforce their
/// relationships inside individual match arms: "a header block must precede
/// the first annotation or definition" was three separate checks, "a catalog
/// declares at most one of this" was three more, and the annotation run's
/// lifecycle was spread over four sites (push, drain, the declarations that
/// must not swallow it, and end of document). The rules are the document's,
/// not the walker's, so they live here — once each, as a decision that returns
/// a [`DraftRefusal`] rather than as an effect.
///
/// Deliberately free of `tree_sitter` types: a refusal names a span, and the
/// lowerer renders it. That is what keeps the ordering rules testable without
/// a syntax tree, and what stops a rule from acquiring a rendering concern.
#[derive(Default)]
struct DocumentDraft {
    definitions: Vec<ParsedDefinition>,
    /// The annotation run accumulated since the last definition. A run binds to
    /// the definition that follows it, so it is drained on a definition and
    /// must be empty at end of document.
    run: Vec<(CompactString, LoweredEntry)>,
    defaults: Option<HashMap<CompactString, LoweredEntry>>,
    schema: Option<CatalogSchema>,
    policy: Option<crate::fsm::policy_program::Program>,
    analyzers: Vec<ParsedAnalyzerConstraint>,
    sets: Vec<ParsedSet>,
    tests: Vec<ParsedTestDecl>,
    groups: Vec<ParsedGroupDecl>,
    saw_definition: bool,
}

/// A rule the draft can reach without knowing how to render it.
struct DraftRefusal {
    code: DiagnosticCode,
    span: SourceSpan,
    message: String,
}

impl DocumentDraft {
    /// Whether a header block may still arrive.
    ///
    /// `defaults`, `schema` and an `analyzer` declaration are inherited by the
    /// definitions that follow, so they must arrive before the first annotation
    /// or definition. A `policy` block is exempt and says why at its arm: it
    /// declares nothing a later definition inherits.
    fn headers_allowed(&self) -> bool {
        !self.saw_definition && self.run.is_empty()
    }

    /// Admits one annotation of the current run, or refuses it.
    ///
    /// Two facts are judged where the annotation is written: a duplicate key in
    /// one run is ambiguous, and an unset is delete-with-provenance, so it needs
    /// a file default to delete. Both point at the key, because that is what the
    /// author renames or removes.
    fn admit_annotation(
        &mut self,
        key: CompactString,
        entry: LoweredEntry,
    ) -> Result<(), DraftRefusal> {
        if self
            .run
            .iter()
            .any(|(existing, _)| existing.as_str() == key.as_str())
        {
            return Err(DraftRefusal {
                code: DiagnosticCode::Syntax,
                span: entry.span(),
                message: "duplicate metadata key".to_owned(),
            });
        }
        if let LoweredEntry::Unset { span } = &entry {
            let has_default = self
                .defaults
                .as_ref()
                .is_some_and(|entries| entries.contains_key(&key));
            if !has_default {
                return Err(DraftRefusal {
                    code: DiagnosticCode::UnsetWithoutDefault,
                    span: *span,
                    message: format!(
                        "`none` unsets a file default, and no file default supplies `{key}`"
                    ),
                });
            }
        }
        self.run.push((key, entry));
        Ok(())
    }

    /// Refuses a run that no definition can absorb.
    ///
    /// Reached both before a catalog-value declaration (`set`, `test`, `group`
    /// take no metadata, so a run must not drift past them to the next
    /// definition) and at end of document.
    fn dangling_run(&self) -> Option<DraftRefusal> {
        self.run.first().map(|(_, entry)| DraftRefusal {
            code: DiagnosticCode::Syntax,
            span: entry.span(),
            message: "metadata annotates no definition".to_owned(),
        })
    }

    /// The effective metadata a definition leaves with, and its `key:`.
    ///
    /// Starts from the file defaults and lets this definition's own run
    /// overwrite — or, for `none`, delete — per key. Every definition therefore
    /// leaves the loop with its *effective* metadata, and nothing downstream can
    /// see an unexpanded default. An unset key is simply absent, exactly as if
    /// the file had no default; an `Unset` with nothing to delete was already
    /// refused by [`Self::admit_annotation`].
    fn materialize_metadata(&mut self) -> (Metadata, Option<ParsedKey>) {
        let mut effective = self.defaults.clone().unwrap_or_default();
        if let Some(LoweredEntry::KeySet(key)) = effective.get_mut("key") {
            key.inherited = true;
        }
        for (key, entry) in std::mem::take(&mut self.run) {
            match entry {
                LoweredEntry::Unset { .. } => {
                    effective.remove(&key);
                }
                LoweredEntry::MetadataSet(_) | LoweredEntry::KeySet(_) => {
                    effective.insert(key, entry);
                }
            }
        }
        let key = effective.remove("key").and_then(|entry| match entry {
            LoweredEntry::KeySet(key) => Some(key),
            LoweredEntry::MetadataSet(_) | LoweredEntry::Unset { .. } => None,
        });
        let metadata: HashMap<CompactString, MetadataEntry> = effective
            .into_iter()
            .filter_map(|(name, entry)| match entry {
                LoweredEntry::MetadataSet(entry) => Some((name, entry)),
                LoweredEntry::KeySet(_) | LoweredEntry::Unset { .. } => None,
            })
            .collect();
        (Arc::new(metadata), key)
    }

    /// The catalog this document declares, plus the source registry the
    /// document's spans are relative to.
    fn into_program(self, sources: SourceRegistry) -> ParsedProgram {
        ParsedProgram {
            definitions: self.definitions,
            sources,
            policy: self.policy,
            analyzers: self.analyzers,
            sets: self.sets,
            tests: self.tests,
            groups: self.groups,
        }
    }
}

impl<'a, 'src> Lowerer<'a, 'src> {
    fn new(
        source: &'a PatternSource<'src>,
        limits: &'a CompileLimits,
        sources: SourceRegistry,
        source_label: Arc<str>,
        source_bytes: usize,
    ) -> Self {
        Self {
            source,
            limits,
            sources,
            source_label,
            source_bytes,
            source_id: SourceId::new(0),
            definitions: 0,
            expression_nodes: 0,
            metadata_bytes: 0,
            regex_bytes: 0,
            warnings: Vec::new(),
        }
    }

    fn lower_root(mut self, root: Node<'_>) -> Result<LoweredProgram, CompileError> {
        self.check_recovery_node(root)?;
        let mut draft = DocumentDraft::default();
        let mut cursor = root.walk();
        for child in root.named_children(&mut cursor) {
            match child.kind() {
                "comment" => {}
                "metadata_entry" | "key_entry" => {
                    let (key, entry) = self.lower_entry(child)?;
                    draft
                        .admit_annotation(key, entry)
                        .map_err(|refusal| self.refuse_draft(refusal))?;
                }
                "defaults_block" => {
                    if draft.defaults.is_some() {
                        return Err(self.error(
                            child,
                            DiagnosticCode::Syntax,
                            "a catalog declares at most one `defaults` block",
                        ));
                    }
                    if !draft.headers_allowed() {
                        return Err(self.error(
                            child,
                            DiagnosticCode::Syntax,
                            "a `defaults` block must precede the first annotation or definition",
                        ));
                    }
                    draft.defaults = Some(self.lower_entry_block(child)?);
                }
                "schema_block" => {
                    if draft.schema.is_some() {
                        return Err(self.error(
                            child,
                            DiagnosticCode::Syntax,
                            "a catalog declares at most one `schema` block",
                        ));
                    }
                    if !draft.headers_allowed() {
                        return Err(self.error(
                            child,
                            DiagnosticCode::Syntax,
                            "a `schema` block must precede the first annotation or definition",
                        ));
                    }
                    draft.schema = Some(self.lower_schema_block(child)?);
                }
                "policy_block" => {
                    // Uniqueness is per **accumulated catalog**, not per file,
                    // so a second block in one document is caught here and a
                    // second block across two documents is caught at
                    // `DefinitionAccumulator::build`. Placement is
                    // deliberately unconstrained: unlike `defaults` and
                    // `schema`, a policy block declares nothing that a later
                    // definition could inherit, so there is nothing for a
                    // placement rule to protect.
                    if draft.policy.is_some() {
                        return Err(self.error(
                            child,
                            DiagnosticCode::Syntax,
                            "a catalog declares at most one `policy` block",
                        ));
                    }
                    self.check_recovery_subtree(child)?;
                    draft.policy = Some(
                        policy_program::lower_policy_block(
                            child,
                            self.source,
                            &self.source_label,
                            self.source_id,
                        )
                        .map_err(CompileError::from_diagnostics)?,
                    );
                }
                "analyzer_entry" => {
                    if !draft.headers_allowed() {
                        return Err(self.error(
                            child,
                            DiagnosticCode::Syntax,
                            "analyzer declarations must precede annotations and definitions",
                        ));
                    }
                    draft.analyzers.push(self.lower_analyzer_entry(child)?);
                }
                "set_definition" => {
                    self.refuse_dangling_run(&draft)?;
                    draft.sets.push(self.lower_set_definition(child)?);
                }
                "test_definition" => {
                    self.refuse_dangling_run(&draft)?;
                    draft.tests.push(self.lower_test_definition(child)?);
                }
                "group_definition" => {
                    self.refuse_dangling_run(&draft)?;
                    draft.groups.push(self.lower_group_definition(child)?);
                }
                "pattern_definition" | "lexicon_definition" => {
                    draft.saw_definition = true;
                    let (metadata, key) = draft.materialize_metadata();
                    draft
                        .definitions
                        .push(self.lower_definition_node(child, metadata, key)?);
                }
                _ => {
                    return Err(self.error(
                        child,
                        DiagnosticCode::UnknownNode,
                        "unknown named node in lingrep patterns document",
                    ));
                }
            }
        }

        self.refuse_dangling_run(&draft)?;

        // Checked last, over every definition's *effective* metadata:
        // requiredness cannot be answered while a definition is still being
        // built, because a `defaults` key satisfies it exactly as an annotation
        // does.
        if let Some(schema) = &draft.schema {
            let violations = schema::check_definitions(schema, &draft.definitions);
            if let Some(first) = violations.first() {
                return Err(self.error_at_span(
                    first.span,
                    DiagnosticCode::Syntax,
                    first.message.clone(),
                ));
            }
        }

        if draft.definitions.is_empty() && draft.saw_definition {
            return Err(CompileError::from_diagnostics(vec![
                Diagnostic::error_with_source_label(
                    DiagnosticCode::DefinitionDropped,
                    Arc::clone(&self.source_label),
                    0..self.source.text.len(),
                    "definition syntax lowered to no definitions",
                    1,
                    1,
                ),
            ]));
        }

        Ok(LoweredProgram {
            program: draft.into_program(self.sources),
            source_bytes: self.source_bytes,
            metadata_bytes: self.metadata_bytes,
            warnings: self.warnings,
        })
    }

    fn lower_analyzer_entry(
        &mut self,
        node: Node<'_>,
    ) -> Result<ParsedAnalyzerConstraint, CompileError> {
        self.check_recovery_subtree(node)?;
        let map = required_field(
            self.source,
            &self.source_label,
            node,
            "value",
            "analyzer_map",
        )?;
        let mut family = None;
        let mut resource = None;
        let mut split = None;
        let mut segmentation = Vec::new();
        let mut seen = HashSet::new();
        let mut cursor = map.walk();
        for wrapper in map.named_children(&mut cursor) {
            let field = if wrapper.kind() == "analyzer_field" {
                wrapper.named_child(0).ok_or_else(|| {
                    self.error(
                        wrapper,
                        DiagnosticCode::Syntax,
                        "analyzer field is missing its value",
                    )
                })?
            } else {
                wrapper
            };
            let key_node =
                required_field(self.source, &self.source_label, field, "key", "identifier")?;
            let key = self.text(key_node)?;
            if !seen.insert(key) {
                return Err(self.error(
                    key_node,
                    DiagnosticCode::Syntax,
                    "duplicate analyzer field",
                ));
            }
            match field.kind() {
                "analyzer_family_field" => {
                    let value = required_field(
                        self.source,
                        &self.source_label,
                        field,
                        "value",
                        "analyzer_identifier",
                    )?;
                    family = Some(self.metadata_string_text(value)?.into());
                }
                "analyzer_resource_field" => {
                    let value = required_field(
                        self.source,
                        &self.source_label,
                        field,
                        "value",
                        "analyzer_identifier",
                    )?;
                    resource = Some(self.metadata_string_text(value)?.into());
                }
                "analyzer_split_field" => {
                    let value = required_field(
                        self.source,
                        &self.source_label,
                        field,
                        "value",
                        "sudachi_split",
                    )?;
                    split = Some(self.metadata_string_text(value)?.into());
                }
                "analyzer_segmentation_field" => {
                    let mut values = field.walk();
                    for value in field.children_by_field_name("value", &mut values) {
                        let decoded: CompactString = self.metadata_string_text(value)?.into();
                        if segmentation.contains(&decoded) {
                            return Err(self.error(
                                value,
                                DiagnosticCode::Syntax,
                                "duplicate analyzer segmentation mode",
                            ));
                        }
                        segmentation.push(decoded);
                    }
                }
                _ => {
                    return Err(self.error(
                        field,
                        DiagnosticCode::UnknownNode,
                        "unknown analyzer field",
                    ));
                }
            }
        }
        let family = family.ok_or_else(|| {
            self.error(
                map,
                DiagnosticCode::Syntax,
                "analyzer declaration requires `family`",
            )
        })?;
        Ok(ParsedAnalyzerConstraint {
            family,
            resource,
            split,
            segmentation: segmentation.into_boxed_slice(),
        })
    }

    /// Lowers a `schema` block into the catalog's declarations.
    ///
    /// An empty block is legal and meaningful: it closes the catalog to
    /// compiler-owned keys alone.
    fn lower_schema_block(&mut self, block: Node<'_>) -> Result<CatalogSchema, CompileError> {
        self.check_recovery_node(block)?;
        let mut schema = CatalogSchema::default();
        let mut cursor = block.walk();
        for child in block.named_children(&mut cursor) {
            match child.kind() {
                "comment" => {}
                "schema_entry" => {
                    let (key, entry) = self.lower_schema_entry(child)?;
                    schema.insert(key, entry).map_err(|violation| {
                        self.error_at_span(
                            violation.span,
                            DiagnosticCode::Syntax,
                            violation.message,
                        )
                    })?;
                }
                _ => {
                    return Err(self.error(
                        child,
                        DiagnosticCode::UnknownNode,
                        "unknown named node in schema block",
                    ));
                }
            }
        }
        Ok(schema)
    }

    fn lower_schema_entry(
        &mut self,
        node: Node<'_>,
    ) -> Result<(CompactString, SchemaEntry), CompileError> {
        self.check_recovery_node(node)?;
        self.charge_metadata_bytes(node)?;

        let visibility = match node.child_by_field_name("visibility") {
            None => None,
            Some(kind) => Some(match self.text(kind)? {
                "pattern" => DefinitionVisibility::Pattern,
                "fragment" => DefinitionVisibility::Fragment,
                other => {
                    return Err(self.error(
                        kind,
                        DiagnosticCode::UnknownNode,
                        format!("unknown definition kind `{other}`"),
                    ));
                }
            }),
        };
        let key_node = required_field(self.source, &self.source_label, node, "key", "identifier")?;
        let type_node = required_field(
            self.source,
            &self.source_label,
            node,
            "type",
            "metadata_type",
        )?;
        let declared = MetadataType::parse(self.text(type_node)?).ok_or_else(|| {
            self.error(
                type_node,
                DiagnosticCode::UnknownNode,
                "unknown metadata type",
            )
        })?;
        let required = node.child_by_field_name("required").is_some();

        let mut members = None;
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            if child.kind() != "metadata_value_set" {
                continue;
            }
            let mut values = Vec::new();
            let mut set_cursor = child.walk();
            for member in child.named_children(&mut set_cursor) {
                if member.kind() != "metadata_string" {
                    continue;
                }
                values.push(self.metadata_string_text(member)?);
            }
            members = Some(values);
        }

        Ok((
            CompactString::from(self.text(key_node)?),
            SchemaEntry {
                visibility,
                declared,
                required,
                members,
                span: self.span_of(key_node)?,
            },
        ))
    }

    /// Lowers a block's `metadata_entry` children into one keyed map.
    ///
    /// Rejects a repeated key at the later of the two, which is the one the
    /// author added. Rejects an `Unset` outright: a `defaults` entry declares
    /// the very value a per-definition `none` deletes, so an unset here has
    /// nothing it could mean.
    fn lower_entry_block(
        &mut self,
        block: Node<'_>,
    ) -> Result<HashMap<CompactString, LoweredEntry>, CompileError> {
        self.check_recovery_node(block)?;
        let mut entries = HashMap::new();
        let mut cursor = block.walk();
        for child in block.named_children(&mut cursor) {
            match child.kind() {
                "comment" => {}
                "metadata_entry" | "key_entry" => {
                    let (key, entry) = self.lower_entry(child)?;
                    let entry = match entry {
                        LoweredEntry::MetadataSet(entry) => LoweredEntry::MetadataSet(entry),
                        LoweredEntry::KeySet(key) => LoweredEntry::KeySet(key),
                        LoweredEntry::Unset { .. } => {
                            return Err(self.error_at_key(
                                child,
                                DiagnosticCode::UnsetOutsideAnnotation,
                                "`none` unsets a file default from a definition's \
                                 annotation run; a `defaults` entry declares that \
                                 default and cannot unset it",
                            ));
                        }
                    };
                    if entries.insert(key, entry).is_some() {
                        return Err(self.error_at_key(
                            child,
                            DiagnosticCode::Syntax,
                            "duplicate metadata key",
                        ));
                    }
                }
                _ => {
                    return Err(self.error(
                        child,
                        DiagnosticCode::UnknownNode,
                        "unknown named node in metadata block",
                    ));
                }
            }
        }
        Ok(entries)
    }

    /// Lowers one `metadata_entry` to its key, typed value, and the key's span.
    ///
    /// Charges the entry's source bytes exactly once, here, where the author
    /// wrote them. Materializing a default into many definitions copies the
    /// *value*, not the source, so it must not charge again — see
    /// `charges_a_defaults_entry_source_bytes_once`.
    fn lower_metadata_entry(
        &mut self,
        node: Node<'_>,
    ) -> Result<(CompactString, LoweredEntry), CompileError> {
        self.check_recovery_node(node)?;
        self.charge_metadata_bytes(node)?;

        let key_node = required_field(self.source, &self.source_label, node, "key", "identifier")?;
        let value_node = node.child_by_field_name("value").ok_or_else(|| {
            self.error(node, DiagnosticCode::Syntax, "metadata entry has no value")
        })?;
        let span = self.span_of(key_node)?;
        let key = CompactString::from(self.text(key_node)?);
        if key == "key" {
            return Err(self.error_at_span(
                span,
                DiagnosticCode::Syntax,
                "`key:` uses a non-empty ordered projection list, for example `key: [unidic.pos1, $head.norm]`",
            ));
        }
        // An entry whose *whole* value is `none` is an unset, recognized here
        // — before `lower_metadata_value`, which refuses `none` everywhere
        // else — and carrying the key's span so the caller that judges it can
        // point its diagnostic at authored source.
        if value_node.kind() == "metadata_none" {
            return Ok((key, LoweredEntry::Unset { span }));
        }
        let value = self.lower_metadata_value(value_node)?;
        Ok((
            key,
            LoweredEntry::MetadataSet(MetadataEntry { value, span }),
        ))
    }

    fn lower_entry(
        &mut self,
        node: Node<'_>,
    ) -> Result<(CompactString, LoweredEntry), CompileError> {
        match node.kind() {
            "metadata_entry" => self.lower_metadata_entry(node),
            "key_entry" => self.lower_key_entry(node),
            _ => Err(self.error(
                node,
                DiagnosticCode::UnknownNode,
                "unknown annotation entry",
            )),
        }
    }

    fn lower_key_entry(
        &mut self,
        node: Node<'_>,
    ) -> Result<(CompactString, LoweredEntry), CompileError> {
        self.check_recovery_subtree(node)?;
        self.charge_metadata_bytes(node)?;
        let key_node = required_field(self.source, &self.source_label, node, "key", "identifier")?;
        let value_node = node
            .child_by_field_name("value")
            .ok_or_else(|| self.error(node, DiagnosticCode::Syntax, "key entry has no value"))?;
        let source_span = self.span_of(key_node)?;
        if value_node.kind() == "metadata_none" {
            return Ok(("key".into(), LoweredEntry::Unset { span: source_span }));
        }

        let mut projections = Vec::new();
        let mut seen = HashSet::new();
        let mut cursor = value_node.walk();
        for projection_node in value_node.named_children(&mut cursor) {
            if projection_node.kind() == "lexical_term_projection" {
                let projection = KeyProjection::lexical_term();
                if !seen.insert(projection.name()) {
                    return Err(self.error(
                        projection_node,
                        DiagnosticCode::InvalidPolicyValue,
                        "duplicate key projection",
                    ));
                }
                projections.push(projection);
                continue;
            }
            let attribute_node = projection_node
                .child_by_field_name("attribute")
                .ok_or_else(|| {
                    self.error(
                        projection_node,
                        DiagnosticCode::Syntax,
                        "key projection has no attribute",
                    )
                })?;
            if !matches!(attribute_node.kind(), "attribute_key" | "identifier") {
                return Err(self.error(
                    attribute_node,
                    DiagnosticCode::UnknownNode,
                    "key projection attribute has an unknown named node",
                ));
            }
            let attribute_name = self.text(attribute_node)?;
            let edge_relation_syntax = attribute_node.kind() == "identifier";
            if edge_relation_syntax && attribute_name != "rel" {
                return Err(self.error(
                    attribute_node,
                    DiagnosticCode::InvalidPolicyValue,
                    "the contextual graph-edge projection is `rel`",
                ));
            }
            let attribute = (!edge_relation_syntax)
                .then(|| AttributeKey::new(attribute_name))
                .transpose()
                .map_err(|error| {
                    self.error(
                        attribute_node,
                        DiagnosticCode::InvalidPolicyValue,
                        format!("invalid token attribute `{attribute_name}`: {error}"),
                    )
                })?;
            let projection = match projection_node.kind() {
                "match_key_projection" => KeyProjection::match_attribute(
                    attribute.expect("match projections use an attribute_key"),
                ),
                "capture_key_projection" => {
                    let capture_node = required_field(
                        self.source,
                        &self.source_label,
                        projection_node,
                        "capture",
                        "capture_reference",
                    )?;
                    let name_node = required_field(
                        self.source,
                        &self.source_label,
                        capture_node,
                        "name",
                        "identifier",
                    )?;
                    let component = match projection_node.child_by_field_name("component") {
                        None => KeyComponent::Sole,
                        Some(component_node) => {
                            let name = self.text(component_node)?;
                            KeyComponent::from_name(name).ok_or_else(|| {
                                self.error(
                                    component_node,
                                    DiagnosticCode::InvalidPolicyValue,
                                    format!(
                                        "`{name}` is not a key component; expected one of {}",
                                        KeyComponent::EXPLICIT_NAMES.join(", ")
                                    ),
                                )
                            })?
                        }
                    };
                    let name = self.text(name_node)?;
                    if edge_relation_syntax {
                        if component != KeyComponent::Sole {
                            return Err(self.error(
                                projection_node,
                                DiagnosticCode::InvalidPolicyValue,
                                "an enhanced edge relation has no span component",
                            ));
                        }
                        KeyProjection::unresolved_edge_relation(name)
                    } else {
                        KeyProjection::capture(
                            name,
                            component,
                            attribute.expect("ordinary capture projections use an attribute_key"),
                        )
                    }
                }
                "lexical_segments_projection" => KeyProjection::lexical_segments(
                    attribute.expect("lexical projections use an attribute_key"),
                ),
                _ => {
                    return Err(self.error(
                        projection_node,
                        DiagnosticCode::UnknownNode,
                        "unknown key projection",
                    ));
                }
            };
            let canonical = projection.name();
            if !seen.insert(canonical.clone()) {
                return Err(self.error(
                    projection_node,
                    DiagnosticCode::InvalidPolicyValue,
                    format!("duplicate key projection `{canonical}`"),
                ));
            }
            projections.push(projection);
        }
        Ok((
            "key".into(),
            LoweredEntry::KeySet(ParsedKey {
                projections,
                source_span,
                inherited: false,
            }),
        ))
    }

    /// Charges one authored metadata node against the source-byte budget.
    ///
    /// Leading indentation is outside the node, and `metadata_entry` ends at
    /// its value, so its byte range contains no line terminator.
    fn charge_metadata_bytes(&mut self, node: Node<'_>) -> Result<(), CompileError> {
        self.metadata_bytes = self
            .metadata_bytes
            .checked_add(node.byte_range().len())
            .ok_or_else(|| {
                self.error(
                    node,
                    DiagnosticCode::LimitExceeded,
                    "metadata byte count overflowed",
                )
            })?;
        if self.metadata_bytes > self.limits.max_metadata_bytes() {
            return Err(self.error(
                node,
                DiagnosticCode::LimitExceeded,
                "lingrep patterns metadata exceeds the metadata byte limit",
            ));
        }
        Ok(())
    }

    fn lower_metadata_value(&mut self, node: Node<'_>) -> Result<Value, CompileError> {
        match node.kind() {
            "metadata_string" => Ok(Value::String(self.metadata_string_text(node)?)),
            "metadata_number" => {
                let text = self.text(node)?;
                let number = text.parse::<serde_json::Number>().map_err(|error| {
                    self.error(
                        node,
                        DiagnosticCode::Syntax,
                        format!("invalid metadata number: {error}"),
                    )
                })?;
                Ok(Value::Number(number))
            }
            "metadata_boolean" => Ok(Value::Bool(self.text(node)? == "true")),
            "metadata_list" => {
                let mut items = Vec::new();
                let mut cursor = node.walk();
                for child in node.named_children(&mut cursor) {
                    if child.kind() == "comment" {
                        continue;
                    }
                    items.push(self.lower_metadata_value(child)?);
                }
                Ok(Value::Array(items))
            }
            "metadata_map" => {
                let mut map = serde_json::Map::new();
                let mut cursor = node.walk();
                for child in node.named_children(&mut cursor) {
                    if child.kind() == "comment" {
                        continue;
                    }
                    // A map key is an identifier or a capture reference; the
                    // JSON key is the node's text either way, so `$head` stays
                    // `$head` and `key:`'s component spelling is unchanged.
                    let key_node = child.child_by_field_name("key").ok_or_else(|| {
                        self.error(child, DiagnosticCode::Syntax, "map entry has no key")
                    })?;
                    let key = self.text(key_node)?.to_owned();
                    let value_node = child.child_by_field_name("value").ok_or_else(|| {
                        self.error(child, DiagnosticCode::Syntax, "map entry has no value")
                    })?;
                    let value = self.lower_metadata_value(value_node)?;
                    if map.insert(key.clone(), value).is_some() {
                        return Err(self.error(
                            key_node,
                            DiagnosticCode::Syntax,
                            format!("duplicate key `{key}` in metadata map"),
                        ));
                    }
                }
                Ok(Value::Object(map))
            }
            // A nested `none` — a list item, a map value — is refused at the
            // `none` itself: only an annotation entry's whole value can mean
            // "unset the file default", and the defect sits on the inner
            // value, not on the key beside it.
            "metadata_none" => Err(self.error(
                node,
                DiagnosticCode::UnsetOutsideAnnotation,
                "`none` is legal only as an annotation entry's whole value, \
                 where it unsets the file default for that key",
            )),
            other => Err(self.error(
                node,
                DiagnosticCode::Syntax,
                format!("unknown metadata value node `{other}`"),
            )),
        }
    }

    /// The decoded contents of a `metadata_string` token.
    ///
    /// Handed to `serde_json`, which already implements JSON string semantics
    /// exactly. The grammar's token and JSON's string grammar agree, so
    /// hand-rolling escape handling here would only be a second, divergent
    /// implementation of one specification.
    fn metadata_string_text(&self, node: Node<'_>) -> Result<String, CompileError> {
        serde_json::from_str::<String>(self.text(node)?).map_err(|error| {
            self.error(
                node,
                DiagnosticCode::Syntax,
                format!("invalid metadata string: {error}"),
            )
        })
    }

    fn span_of(&self, node: Node<'_>) -> Result<SourceSpan, CompileError> {
        SourceSpan::new(self.source_id, node.byte_range()).map_err(|_| {
            self.error(
                node,
                DiagnosticCode::LimitExceeded,
                "source span exceeds the supported offset range",
            )
        })
    }

    /// Reports at an entry's `key` field rather than at the whole entry.
    ///
    /// A duplicate is a fact about the key, and the author fixes it by deleting
    /// or renaming that key — not the value beside it.
    fn error_at_key(
        &self,
        entry: Node<'_>,
        code: DiagnosticCode,
        message: impl Into<String>,
    ) -> CompileError {
        let target = entry.child_by_field_name("key").unwrap_or(entry);
        self.error(target, code, message)
    }

    fn error_at_span(
        &self,
        span: SourceSpan,
        code: DiagnosticCode,
        message: impl Into<String>,
    ) -> CompileError {
        let range = span.byte_range();
        error_at_range(
            self.source,
            Arc::clone(&self.source_label),
            range.start as usize..range.end as usize,
            code,
            message,
        )
    }

    /// Renders a [`DocumentDraft`] refusal at the span the draft named.
    ///
    /// The draft decides; the lowerer renders. Keeping the two apart is what
    /// lets the ordering rules hold no syntax-tree types and no `self`.
    fn refuse_draft(&self, refusal: DraftRefusal) -> CompileError {
        self.error_at_span(refusal.span, refusal.code, refusal.message)
    }

    /// Refuses a run that no definition can absorb, if the draft holds one.
    fn refuse_dangling_run(&self, draft: &DocumentDraft) -> Result<(), CompileError> {
        match draft.dangling_run() {
            Some(refusal) => Err(self.refuse_draft(refusal)),
            None => Ok(()),
        }
    }

    /// Lowers `set NAME := {…}.` into a catalog-scoped value list.
    ///
    /// Members go through [`Self::lower_literal_value`], the same function an
    /// inline `in {…}` uses, which is what makes an escape decode identically
    /// in both places. That function's `negated` argument only selects between
    /// naming `~` and `!~` in the regex-rejection message; a declaration knows
    /// no operator, so this passes `false` and the message names none.
    fn lower_set_definition(&mut self, node: Node<'_>) -> Result<ParsedSet, CompileError> {
        self.check_recovery_node(node)?;
        let name_node =
            required_field(self.source, &self.source_label, node, "name", "identifier")?;
        self.check_recovery_node(name_node)?;
        let name: CompactString = self.text(name_node)?.into();
        let members_node = required_field(
            self.source,
            &self.source_label,
            node,
            "members",
            "value_set",
        )?;
        self.check_recovery_node(members_node)?;

        // `commaSep1` makes an empty `{}` a parse error, so emptiness needs no
        // check of its own here.
        let mut members = Vec::new();
        let mut seen = HashSet::new();
        let mut cursor = members_node.walk();
        for value_node in members_node.named_children(&mut cursor) {
            if value_node.kind() != "value" {
                return Err(self.error(
                    value_node,
                    DiagnosticCode::UnknownNode,
                    "unknown set member",
                ));
            }
            let literal = self.lower_literal_value(value_node, false)?;
            if !seen.insert(literal.clone()) {
                return Err(self.error(
                    value_node,
                    DiagnosticCode::Syntax,
                    "set members must be unique",
                ));
            }
            members.push(literal);
        }

        Ok(ParsedSet {
            name,
            members,
            span: self.span_of(node)?,
        })
    }

    /// Lowers `test NAME := <test>.` into a catalog-scoped predicate.
    ///
    /// The body goes through [`Self::lower_test`], the same function a `[…]`
    /// uses, which is what makes a named test mean exactly what writing it
    /// inline would have meant. Depth starts at 1 rather than at the depth of
    /// some enclosing expression: a declaration has no enclosing expression,
    /// and the nesting a reference contributes is bounded at resolution by the
    /// distribution budget instead.
    fn lower_test_definition(&mut self, node: Node<'_>) -> Result<ParsedTestDecl, CompileError> {
        self.check_recovery_node(node)?;
        let name_node =
            required_field(self.source, &self.source_label, node, "name", "identifier")?;
        self.check_recovery_node(name_node)?;
        let name: CompactString = self.text(name_node)?.into();
        let body_node = node.child_by_field_name("body").ok_or_else(|| {
            self.error(
                node,
                DiagnosticCode::Syntax,
                "test definition is missing its body",
            )
        })?;
        let body = self
            .lower_test(body_node, 1)
            .map_err(|diagnostic| CompileError::from_diagnostics(vec![diagnostic]))?;

        Ok(ParsedTestDecl {
            name,
            body,
            span: self.span_of(node)?,
        })
    }

    /// Lowers `group NAME := key <op> value(s).` into a catalog-scoped
    /// membership predicate.
    ///
    /// The operator is one named `group_operator` node — `not in` is a single
    /// operator span, like `attribute_operator`'s — so the form is classified
    /// by reading one node's text rather than scanning anonymous children.
    /// Values are `metadata_string`s: the predicate reads metadata, and
    /// strings there are always quoted, so they decode through the same
    /// function an annotation value uses.
    fn lower_group_definition(&mut self, node: Node<'_>) -> Result<ParsedGroupDecl, CompileError> {
        self.check_recovery_node(node)?;
        let name_node =
            required_field(self.source, &self.source_label, node, "name", "identifier")?;
        self.check_recovery_node(name_node)?;
        let name: CompactString = self.text(name_node)?.into();
        let body = required_field(
            self.source,
            &self.source_label,
            node,
            "body",
            "group_predicate",
        )?;
        self.check_recovery_node(body)?;
        let key_node = required_field(self.source, &self.source_label, body, "key", "identifier")?;
        let key: CompactString = self.text(key_node)?.into();
        let operator_node = required_field(
            self.source,
            &self.source_label,
            body,
            "operator",
            "group_operator",
        )?;
        let operator = match self.text(operator_node)? {
            "=" => GroupOperator::Equals,
            "!=" => GroupOperator::NotEquals,
            "in" => GroupOperator::In,
            // `not in` is one node whose text spans both words, so whatever
            // whitespace separates them sits inside the span — the exact
            // spelling cannot be matched, only its ends.
            text if text.starts_with("not") && text.ends_with("in") => GroupOperator::NotIn,
            _ => {
                return Err(self.error(
                    operator_node,
                    DiagnosticCode::UnknownNode,
                    "unknown group operator",
                ));
            }
        };

        // `commaSep1` makes an empty `{}` a parse error, and the scalar forms
        // carry exactly one value, so emptiness needs no check of its own.
        let mut values: Vec<String> = Vec::new();
        let mut seen = HashSet::new();
        let mut cursor = body.walk();
        for value_node in body.children_by_field_name("value", &mut cursor) {
            if value_node.kind() != "metadata_string" {
                return Err(self.error(
                    value_node,
                    DiagnosticCode::UnknownNode,
                    "unknown group predicate value",
                ));
            }
            let value = self.metadata_string_text(value_node)?;
            // Refused rather than deduplicated, exactly as a `set` member is:
            // the author meant one of them, and which is not the compiler's
            // guess to make.
            if !seen.insert(value.clone()) {
                return Err(self.error(
                    value_node,
                    DiagnosticCode::Syntax,
                    "group predicate values must be unique",
                ));
            }
            values.push(value);
        }

        Ok(ParsedGroupDecl {
            name,
            key,
            operator,
            values,
            span: self.span_of(node)?,
        })
    }

    fn lower_definition_node(
        &mut self,
        node: Node<'_>,
        metadata: Metadata,
        key: Option<ParsedKey>,
    ) -> Result<ParsedDefinition, CompileError> {
        self.check_recovery_node(node)?;
        self.definitions = self.definitions.checked_add(1).ok_or_else(|| {
            self.error(
                node,
                DiagnosticCode::LimitExceeded,
                "definition count overflowed",
            )
        })?;
        if self.definitions > self.limits.max_definitions() {
            return Err(self.error(
                node,
                DiagnosticCode::LimitExceeded,
                "lingrep patterns document exceeds the definition limit",
            ));
        }

        let name_node =
            required_field(self.source, &self.source_label, node, "name", "identifier")?;
        if node.kind() == "lexicon_definition" {
            let mut by = Vec::new();
            let mut members = Vec::new();
            let mut seen_by = HashSet::new();
            let mut seen_members = HashSet::new();
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                match child.kind() {
                    "attribute_key" => {
                        let attribute_name = self.text(child)?;
                        let attribute = AttributeKey::new(attribute_name).map_err(|error| {
                            self.error(
                                child,
                                DiagnosticCode::InvalidPolicyValue,
                                format!("invalid token attribute `{attribute_name}`: {error}"),
                            )
                        })?;
                        if !matches!(attribute.as_str(), "text" | "norm") {
                            return Err(self.error(
                                child,
                                DiagnosticCode::InvalidPolicyValue,
                                "a lexicon `by` projection must be `text` or `norm`",
                            ));
                        }
                        if !seen_by.insert(attribute.clone()) {
                            return Err(self.error(
                                child,
                                DiagnosticCode::InvalidPolicyValue,
                                "duplicate lexicon `by` projection",
                            ));
                        }
                        by.push(attribute);
                    }
                    "metadata_string" => {
                        let decoded: CompactString = self.metadata_string_text(child)?.into();
                        if decoded.is_empty() {
                            return Err(self.error(
                                child,
                                DiagnosticCode::InvalidPolicyValue,
                                "a lexicon member must not be empty",
                            ));
                        }
                        if !seen_members.insert(decoded.clone()) {
                            return Err(self.error(
                                child,
                                DiagnosticCode::InvalidPolicyValue,
                                "duplicate decoded lexicon member",
                            ));
                        }
                        members.push(decoded);
                    }
                    _ => {}
                }
            }
            if by.is_empty() {
                return Err(self.error(
                    node,
                    DiagnosticCode::Syntax,
                    "a lexicon requires at least one `by` projection",
                ));
            }
            return Ok(ParsedDefinition {
                name: self.text(name_node)?.into(),
                body: ParsedDefinitionBody::Lexicon {
                    by: by.into_boxed_slice(),
                    members: members.into_boxed_slice(),
                },
                metadata,
                key,
                span: self.span_of(node)?,
            });
        }
        let pattern_node = node.child_by_field_name("pattern").ok_or_else(|| {
            self.error(
                node,
                DiagnosticCode::Syntax,
                "pattern definition is missing its expression",
            )
        })?;
        let visibility = match node.child_by_field_name("kind") {
            None => DefinitionVisibility::Pattern,
            Some(kind) => match self.text(kind)? {
                "pattern" => DefinitionVisibility::Pattern,
                "fragment" => DefinitionVisibility::Fragment,
                _ => {
                    return Err(self.error(
                        kind,
                        DiagnosticCode::UnknownNode,
                        "unknown definition kind",
                    ));
                }
            },
        };
        if !is_expression_kind(pattern_node.kind()) {
            return Err(self.error(
                pattern_node,
                DiagnosticCode::UnknownNode,
                "unknown pattern expression node",
            ));
        }
        let (relations, enhanced_relations) = match node.child_by_field_name("where") {
            None => (Box::default(), Box::default()),
            Some(where_node) => {
                if visibility == DefinitionVisibility::Fragment {
                    return Err(self.error(
                        where_node,
                        DiagnosticCode::WhereOnFragment,
                        "a fragment may not declare a `where` clause: a \
                         fragment never emits, so the clause could act only \
                         through inclusion — declare it on the emitting \
                         pattern instead",
                    ));
                }
                self.lower_where_clause(where_node)?
            }
        };

        Ok(ParsedDefinition {
            name: self.text(name_node)?.into(),
            body: ParsedDefinitionBody::Pattern {
                visibility,
                expr: self.lower_expr(pattern_node, 1)?,
                relations,
                enhanced_relations,
            },
            metadata,
            key,
            span: SourceSpan::new(self.source_id, node.byte_range()).map_err(|_| {
                self.error(
                    node,
                    DiagnosticCode::LimitExceeded,
                    "source span exceeds the supported offset range",
                )
            })?,
        })
    }

    /// Lowers a definition's `where` clause into its authored constraints.
    ///
    /// Only what is decidable without capture identities is checked here: the
    /// self-edge refusal fires on operand *names*, which is equivalent to the
    /// capture-identity comparison because duplicate capture names within one
    /// definition are refused. Existence and width are materialization and
    /// analysis questions, answered after fragment expansion.
    fn lower_where_clause(
        &mut self,
        node: Node<'_>,
    ) -> Result<
        (
            Box<[ParsedRelation]>,
            Box<[super::model::ParsedEnhancedRelation]>,
        ),
        CompileError,
    > {
        self.check_recovery_node(node)?;
        let mut relations = Vec::new();
        let mut enhanced_relations = Vec::new();
        let mut cursor = node.walk();
        let constraints: Vec<_> = node
            .children_by_field_name("constraint", &mut cursor)
            .collect();
        for constraint in constraints {
            self.check_recovery_node(constraint)?;
            if constraint.child_by_field_name("kind").is_some() {
                let head_node = constraint.child_by_field_name("head").ok_or_else(|| {
                    self.error(
                        constraint,
                        DiagnosticCode::Syntax,
                        "enhanced relation is missing field head",
                    )
                })?;
                let dependent_node =
                    constraint.child_by_field_name("dependent").ok_or_else(|| {
                        self.error(
                            constraint,
                            DiagnosticCode::Syntax,
                            "enhanced relation is missing field dependent",
                        )
                    })?;
                let label_node = required_field(
                    self.source,
                    &self.source_label,
                    constraint,
                    "label",
                    "label_set",
                )?;
                let edge = constraint
                    .child_by_field_name("edge")
                    .map(|node| {
                        let name = required_field(
                            self.source,
                            &self.source_label,
                            node,
                            "name",
                            "identifier",
                        )?;
                        Ok((CompactString::from(self.text(name)?), self.span_of(node)?))
                    })
                    .transpose()?;
                enhanced_relations.push(super::model::ParsedEnhancedRelation {
                    polarity: if constraint.child_by_field_name("polarity").is_some() {
                        RelationPolarity::Negative
                    } else {
                        RelationPolarity::Positive
                    },
                    head: self.lower_enhanced_endpoint(head_node)?,
                    labels: self.lower_relation_labels(label_node)?,
                    label_span: self.span_of(label_node)?,
                    edge,
                    dependent: self.lower_enhanced_endpoint(dependent_node)?,
                });
                continue;
            }
            let head_node = required_field(
                self.source,
                &self.source_label,
                constraint,
                "head",
                "capture_reference",
            )?;
            let head_name = required_field(
                self.source,
                &self.source_label,
                head_node,
                "name",
                "identifier",
            )?;
            let label_node = required_field(
                self.source,
                &self.source_label,
                constraint,
                "label",
                "label_set",
            )?;
            let head: CompactString = self.text(head_name)?.into();
            let dependent_node = constraint.child_by_field_name("dependent").ok_or_else(|| {
                self.error(
                    constraint,
                    DiagnosticCode::Syntax,
                    "relation_constraint is missing field dependent",
                )
            })?;
            let target = match dependent_node.kind() {
                "capture_reference" => {
                    let dependent_name = required_field(
                        self.source,
                        &self.source_label,
                        dependent_node,
                        "name",
                        "identifier",
                    )?;
                    let dependent: CompactString = self.text(dependent_name)?.into();
                    if head == dependent {
                        return Err(self.error(
                            constraint,
                            DiagnosticCode::WhereSelfEdge,
                            format!(
                                "a basic `where` constraint requires distinct capture \
                                 operands; both endpoints name `${head}`"
                            ),
                        ));
                    }
                    ParsedRelationTarget::Captured {
                        polarity: if constraint.child_by_field_name("polarity").is_some() {
                            RelationPolarity::Negative
                        } else {
                            RelationPolarity::Positive
                        },
                        capture: dependent,
                        span: self.span_of(dependent_node)?,
                    }
                }
                "anonymous_dependency_node" => ParsedRelationTarget::Anonymous {
                    polarity: if constraint.child_by_field_name("polarity").is_some() {
                        RelationPolarity::Negative
                    } else {
                        RelationPolarity::Positive
                    },
                    span: self.span_of(dependent_node)?,
                },
                "node_pattern" => ParsedRelationTarget::Predicate {
                    polarity: if constraint.child_by_field_name("polarity").is_some() {
                        RelationPolarity::Negative
                    } else {
                        RelationPolarity::Positive
                    },
                    test: self
                        .lower_token_test(dependent_node, 0)
                        .map_err(|diagnostic| CompileError::from_diagnostics(vec![diagnostic]))?,
                    span: self.span_of(dependent_node)?,
                },
                _ => {
                    return Err(self.error(
                        dependent_node,
                        DiagnosticCode::UnknownNode,
                        "field dependent has an unknown named node",
                    ));
                }
            };
            relations.push(ParsedRelation {
                head,
                head_span: self.span_of(head_node)?,
                labels: self.lower_relation_labels(label_node)?,
                label_span: self.span_of(label_node)?,
                target,
            });
        }
        Ok((
            relations.into_boxed_slice(),
            enhanced_relations.into_boxed_slice(),
        ))
    }

    fn lower_enhanced_endpoint(
        &mut self,
        node: Node<'_>,
    ) -> Result<super::model::ParsedEnhancedEndpoint, CompileError> {
        use super::model::ParsedEnhancedEndpoint;
        let span = self.span_of(node)?;
        match node.kind() {
            "capture_reference" => {
                let name_node =
                    required_field(self.source, &self.source_label, node, "name", "identifier")?;
                Ok(ParsedEnhancedEndpoint::Reference {
                    name: CompactString::from(self.text(name_node)?),
                    span,
                })
            }
            "graph_node_binding" => {
                let name_node =
                    required_field(self.source, &self.source_label, node, "name", "identifier")?;
                let predicate = required_field(
                    self.source,
                    &self.source_label,
                    node,
                    "predicate",
                    "node_pattern",
                )?;
                Ok(ParsedEnhancedEndpoint::Binding {
                    name: CompactString::from(self.text(name_node)?),
                    test: self
                        .lower_token_test(predicate, 0)
                        .map_err(|diagnostic| CompileError::from_diagnostics(vec![diagnostic]))?,
                    span,
                })
            }
            "basic_head_endpoint" => {
                let dependent = required_field(
                    self.source,
                    &self.source_label,
                    node,
                    "dependent",
                    "capture_reference",
                )?;
                let name = required_field(
                    self.source,
                    &self.source_label,
                    dependent,
                    "name",
                    "identifier",
                )?;
                Ok(ParsedEnhancedEndpoint::BasicHead {
                    dependent: CompactString::from(self.text(name)?),
                    span,
                })
            }
            _ => Err(self.error(
                node,
                DiagnosticCode::UnknownNode,
                "enhanced relation endpoint has an unknown named node",
            )),
        }
    }

    /// Lowers one `-[…]->` bracket into its authored label terms.
    ///
    /// A literal term is parsed here, where the grammar has already
    /// constrained its shape, so [`RelationLabel::parse`] cannot fail — the
    /// fallible producer is a `set` member, which bind checks. A `@name` term
    /// stays symbolic for the same reason `[x in @S]` does: the set it names
    /// may be declared in another document of the accumulated catalog.
    ///
    /// `*` is a complete predicate rather than a label term, so the grammar
    /// makes it mutually exclusive with this listed-term path.
    ///
    /// Duplicates are refused rather than deduped, and only among literals:
    /// two set references, or a literal already inside a referenced set,
    /// cannot be compared until bind resolves them, and that check lives
    /// there.
    fn lower_relation_labels(
        &mut self,
        node: Node<'_>,
    ) -> Result<RelationLabels<ParsedLabelTerm>, CompileError> {
        self.check_recovery_node(node)?;
        if node
            .named_child(0)
            .is_some_and(|child| child.kind() == "any_relation")
        {
            return Ok(RelationLabels::Any);
        }
        let mut cursor = node.walk();
        let term_nodes: Vec<_> = node.children_by_field_name("term", &mut cursor).collect();
        let mut terms: Vec<ParsedLabelTerm> = Vec::with_capacity(term_nodes.len());
        for term_node in term_nodes {
            self.check_recovery_node(term_node)?;
            let term = if term_node.kind() == "set_reference" {
                // Sole named child rather than a field, for the reason
                // `lower_attribute`'s `in` arm gives: `set_reference` is
                // `seq('@', $.identifier)` and declares no field on the name.
                // `@` with no name parses as a MISSING `identifier`, which
                // `check_recovery_node` above has already refused, so the
                // `ok_or_else` is total-match and not a real branch.
                let name_node = term_node.named_child(0).ok_or_else(|| {
                    self.error(
                        term_node,
                        DiagnosticCode::Syntax,
                        "set reference is missing its name",
                    )
                })?;
                ParsedLabelTerm::SetReference {
                    name: self.text(name_node)?.into(),
                    span: self.span_of(term_node)?,
                }
            } else {
                let text = self.text(term_node)?;
                let label = RelationLabel::parse(text).ok_or_else(|| {
                    self.error(
                        term_node,
                        DiagnosticCode::InvalidRelationLabel,
                        format!("`{text}` is not a well-formed dependency relation label"),
                    )
                })?;
                if terms
                    .iter()
                    .any(|seen| matches!(seen, ParsedLabelTerm::Label(other) if *other == label))
                {
                    return Err(self.error(
                        term_node,
                        DiagnosticCode::DuplicateRelationLabel,
                        format!(
                            "`{}` appears twice in one `where` label set; a dependent carries \
                             one `dep.rel`, so the repeat adds nothing and is more likely a \
                             typo than intent",
                            label.authored()
                        ),
                    ));
                }
                ParsedLabelTerm::Label(label)
            };
            terms.push(term);
        }
        Ok(RelationLabels::Listed(terms.into_boxed_slice()))
    }

    fn lower_expr(&mut self, node: Node<'_>, depth: usize) -> Result<ParsedExpr, CompileError> {
        self.check_recovery_node(node)?;
        self.check_depth(node, depth)?;
        match node.kind() {
            "quantified_pattern" => self.lower_quantified(node, depth),
            "bare_element" => {
                self.enter_expr(node, depth)?;
                let identifier =
                    only_named_child(self.source, &self.source_label, node, "identifier")?;
                Ok(self.literal_token(identifier)?)
            }
            "node_pattern" => {
                self.enter_expr(node, depth)?;
                let test = self
                    .lower_token_test(node, depth)
                    .map_err(|diagnostic| CompileError::from_diagnostics(vec![diagnostic]))?;
                Ok(ParsedExpr::Token(test))
            }
            "sentence_start" => {
                self.enter_expr(node, depth)?;
                Ok(ParsedExpr::Assertion(PositionAssertion::SentenceStart))
            }
            "sentence_end" => {
                self.enter_expr(node, depth)?;
                Ok(ParsedExpr::Assertion(PositionAssertion::SentenceEnd))
            }
            "preceded_by" => self.lower_lookaround(
                node,
                depth,
                LookDirection::PrecededBy,
                LookPolarity::Positive,
            ),
            "not_preceded_by" => self.lower_lookaround(
                node,
                depth,
                LookDirection::PrecededBy,
                LookPolarity::Negative,
            ),
            "followed_by" => self.lower_lookaround(
                node,
                depth,
                LookDirection::FollowedBy,
                LookPolarity::Positive,
            ),
            "not_followed_by" => self.lower_lookaround(
                node,
                depth,
                LookDirection::FollowedBy,
                LookPolarity::Negative,
            ),
            "same_as" => {
                self.enter_expr(node, depth)?;
                let capture = required_field(
                    self.source,
                    &self.source_label,
                    node,
                    "capture",
                    "capture_reference",
                )?;
                let name = required_field(
                    self.source,
                    &self.source_label,
                    capture,
                    "name",
                    "identifier",
                )?;
                let projection = match node.child_by_field_name("projection") {
                    None => CaptureProjection(
                        AttributeKey::new("text").expect("form is a canonical attribute key"),
                    ),
                    Some(wrapper) => {
                        let key = self.text(wrapper)?;
                        CaptureProjection(AttributeKey::new(key).map_err(|error| {
                            self.error(
                                wrapper,
                                DiagnosticCode::InvalidCaptureProjection,
                                format!("invalid capture projection `{key}`: {error}"),
                            )
                        })?)
                    }
                };
                Ok(ParsedExpr::SameAs {
                    name: self.text(name)?.into(),
                    projection,
                    span: SourceSpan::new(self.source_id, node.byte_range()).map_err(|_| {
                        self.error(
                            node,
                            DiagnosticCode::LimitExceeded,
                            "span-equality source span exceeds the supported offset range",
                        )
                    })?,
                })
            }
            "pattern_reference" => {
                self.enter_expr(node, depth)?;
                let identifier =
                    only_named_child(self.source, &self.source_label, node, "identifier")?;
                Ok(ParsedExpr::Reference {
                    name: self.text(identifier)?.into(),
                    span: SourceSpan::new(self.source_id, node.byte_range()).map_err(|_| {
                        self.error(
                            node,
                            DiagnosticCode::LimitExceeded,
                            "reference span exceeds the supported offset range",
                        )
                    })?,
                })
            }
            "pattern_sequence" => {
                self.enter_expr(node, depth)?;
                Ok(ParsedExpr::Sequence(
                    self.lower_expression_children(node, depth + 1)?,
                ))
            }
            "alternation" => {
                self.enter_expr(node, depth)?;
                Ok(ParsedExpr::Alternation(
                    self.lower_expression_children(node, depth + 1)?,
                ))
            }
            "intersection" => {
                self.enter_expr(node, depth)?;
                Ok(ParsedExpr::Intersection {
                    operands: self.lower_expression_children(node, depth + 1)?,
                    span: SourceSpan::new(self.source_id, node.byte_range()).map_err(|_| {
                        self.error(
                            node,
                            DiagnosticCode::LimitExceeded,
                            "intersection span exceeds the supported offset range",
                        )
                    })?,
                })
            }
            "any_order" => {
                self.enter_expr(node, depth)?;
                Ok(ParsedExpr::AnyOrder {
                    operands: self.lower_expression_children(node, depth + 1)?,
                    span: SourceSpan::new(self.source_id, node.byte_range()).map_err(|_| {
                        self.error(
                            node,
                            DiagnosticCode::LimitExceeded,
                            "any-order source span exceeds the supported offset range",
                        )
                    })?,
                })
            }
            "parenthesized_pattern" => {
                self.enter_expr(node, depth)?;
                let child = only_expression_child(self.source, &self.source_label, node)?;
                Ok(ParsedExpr::Group(Box::new(
                    self.lower_expr(child, depth + 1)?,
                )))
            }
            "named_group" => {
                self.enter_expr(node, depth)?;
                let name =
                    required_field(self.source, &self.source_label, node, "name", "identifier")?;
                let pattern = node.child_by_field_name("pattern").ok_or_else(|| {
                    self.error(
                        node,
                        DiagnosticCode::Syntax,
                        "named group is missing its expression",
                    )
                })?;
                if !is_expression_kind(pattern.kind()) {
                    return Err(self.error(
                        pattern,
                        DiagnosticCode::UnknownNode,
                        "unknown named-group expression node",
                    ));
                }
                Ok(ParsedExpr::NamedGroup {
                    name: self.text(name)?.into(),
                    expr: Box::new(self.lower_expr(pattern, depth + 1)?),
                    span: SourceSpan::new(self.source_id, node.byte_range()).map_err(|_| {
                        self.error(
                            node,
                            DiagnosticCode::LimitExceeded,
                            "capture source span exceeds the supported offset range",
                        )
                    })?,
                })
            }
            _ => Err(self.error(
                node,
                DiagnosticCode::UnknownNode,
                "unknown named expression node",
            )),
        }
    }

    fn lower_lookaround(
        &mut self,
        node: Node<'_>,
        depth: usize,
        direction: LookDirection,
        polarity: LookPolarity,
    ) -> Result<ParsedExpr, CompileError> {
        self.enter_expr(node, depth)?;
        let body = node.child_by_field_name("body").ok_or_else(|| {
            self.error(
                node,
                DiagnosticCode::Syntax,
                "lookaround is missing its body",
            )
        })?;
        if !is_expression_kind(body.kind()) {
            return Err(self.error(
                body,
                DiagnosticCode::UnknownNode,
                "unknown lookaround body node",
            ));
        }
        let span = SourceSpan::new(self.source_id, node.byte_range()).map_err(|_| {
            self.error(
                node,
                DiagnosticCode::LimitExceeded,
                "lookaround source span exceeds the supported offset range",
            )
        })?;
        Ok(ParsedExpr::Lookaround(LookaroundExpr {
            direction,
            polarity,
            body: Box::new(self.lower_expr(body, depth + 1)?),
            span,
        }))
    }

    fn lower_quantified(
        &mut self,
        node: Node<'_>,
        depth: usize,
    ) -> Result<ParsedExpr, CompileError> {
        let mut expression = None;
        let mut quantifier = None;
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            match child.kind() {
                "comment" => {}
                "quantifier" if quantifier.replace(child).is_none() => {}
                kind if is_expression_kind(kind) && expression.replace(child).is_none() => {}
                _ => {
                    return Err(self.error(
                        child,
                        DiagnosticCode::UnknownNode,
                        "unknown or duplicate quantified-pattern child",
                    ));
                }
            }
        }
        let expression = expression.ok_or_else(|| {
            self.error(
                node,
                DiagnosticCode::Syntax,
                "quantified pattern is missing its expression",
            )
        })?;
        let quantifier = quantifier.ok_or_else(|| {
            self.error(
                node,
                DiagnosticCode::Syntax,
                "quantified pattern is missing its quantifier",
            )
        })?;
        self.enter_expr(node, depth)?;
        let (min, max) = self.lower_quantifier(quantifier)?;
        Ok(ParsedExpr::Repeat {
            expr: Box::new(self.lower_expr(expression, depth + 1)?),
            min,
            max,
        })
    }

    fn lower_quantifier(&self, node: Node<'_>) -> Result<(usize, Option<usize>), CompileError> {
        let text = self.text(node)?;
        let bounds = match text {
            "?" => Some((0, Some(1))),
            "*" => Some((0, None)),
            "+" => Some((1, None)),
            text if text.starts_with('{') && text.ends_with('}') => {
                // Guarded by `starts_with('{') && ends_with('}')`, so both ends
                // are single-byte ASCII braces and the interior is a char slice.
                #[expect(
                    clippy::string_slice,
                    reason = "guarded by {{...}} brace delimiters, ASCII char boundaries"
                )]
                let inner = &text[1..text.len() - 1];
                if let Some((min, max)) = inner.split_once(',') {
                    let min = min.parse::<usize>().ok();
                    let max = if max.is_empty() {
                        Some(None)
                    } else {
                        max.parse::<usize>().ok().map(Some)
                    };
                    min.zip(max)
                } else {
                    inner
                        .parse::<usize>()
                        .ok()
                        .map(|exact| (exact, Some(exact)))
                }
            }
            _ => None,
        };
        let Some((min, max)) = bounds else {
            return Err(self.error(
                node,
                DiagnosticCode::InvalidQuantifier,
                "invalid quantifier",
            ));
        };
        if max.is_some_and(|max| min > max) {
            return Err(self.error(
                node,
                DiagnosticCode::InvalidQuantifier,
                "quantifier minimum exceeds its maximum",
            ));
        }
        Ok((min, max))
    }

    fn lower_expression_children(
        &mut self,
        node: Node<'_>,
        depth: usize,
    ) -> Result<Vec<ParsedExpr>, CompileError> {
        let mut expressions = Vec::with_capacity(node.named_child_count());
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            if is_expression_kind(child.kind()) {
                expressions.push(self.lower_expr(child, depth)?);
            } else if child.kind() != "comment" {
                return Err(self.error(
                    child,
                    DiagnosticCode::UnknownNode,
                    "unknown sequence or alternation child",
                ));
            }
        }
        Ok(expressions)
    }

    /// Lowers a `[…]` body, maps all token attributes and compiles all
    /// regexes at one strict boundary.
    ///
    /// An empty bracket pair is [`ParsedTest::And`] over no operands, which
    /// distributes to exactly one empty branch — the zero-attribute predicate
    /// the runtime has always compiled to `CompiledPredicate::Empty`.
    fn lower_token_test(&mut self, node: Node<'_>, depth: usize) -> Result<ParsedTest, Diagnostic> {
        let mut body = None;
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            if child.kind() == "comment" {
                continue;
            }
            if body.replace(child).is_some() {
                return Err(diagnostic_at(
                    self.source,
                    Arc::clone(&self.source_label),
                    child,
                    DiagnosticCode::UnknownNode,
                    "unknown token-predicate child",
                ));
            }
        }
        match body {
            None => Ok(ParsedTest::And(Vec::new())),
            Some(body) => self.lower_test(body, depth),
        }
    }

    /// Lowers one node of the Boolean token-test grammar.
    ///
    /// `depth` is the enclosing expression's depth, so a deeply parenthesized
    /// test is refused by the same nesting limit an equally deep expression is
    /// — this recursion is otherwise unbounded on adversarial source.
    fn lower_test(&mut self, node: Node<'_>, depth: usize) -> Result<ParsedTest, Diagnostic> {
        self.check_recovery_node(node)
            .map_err(|error| self.first_diagnostic(error, node))?;
        self.check_depth(node, depth)
            .map_err(|error| self.first_diagnostic(error, node))?;
        match node.kind() {
            "attribute" => Ok(ParsedTest::Attr(
                self.lower_attribute(node)
                    .map_err(|error| self.first_diagnostic(error, node))?,
            )),
            "attribute_conjunction" => Ok(ParsedTest::And(self.lower_test_operands(node, depth)?)),
            "test_disjunction" => Ok(ParsedTest::Or(self.lower_test_operands(node, depth)?)),
            "test_negation" => {
                // `seq('!', choice($.parenthesized_test, $.test_reference))`,
                // so exactly one named child and no field on it.
                let operand = only_test_child(self.source, &self.source_label, node)?;
                Ok(ParsedTest::Not(Box::new(
                    self.lower_test(operand, depth.saturating_add(1))?,
                )))
            }
            // Pure syntax, like `parenthesized_pattern`: the parentheses have
            // already done their work by grouping, and keeping a node for them
            // would make `(a)` and `a` distinct parsed trees.
            "parenthesized_test" => {
                let inner = only_test_child(self.source, &self.source_label, node)?;
                self.lower_test(inner, depth.saturating_add(1))
            }
            "test_reference" => {
                // `seq('@', $.identifier)` with no field on the name, so take
                // the sole named child rather than inventing a field the
                // grammar does not declare.
                let name_node = node.named_child(0).ok_or_else(|| {
                    diagnostic_at(
                        self.source,
                        Arc::clone(&self.source_label),
                        node,
                        DiagnosticCode::Syntax,
                        "test reference is missing its name",
                    )
                })?;
                Ok(ParsedTest::Reference {
                    name: self
                        .text(name_node)
                        .map_err(|error| self.first_diagnostic(error, name_node))?
                        .into(),
                    span: self
                        .span_of(node)
                        .map_err(|error| self.first_diagnostic(error, node))?,
                })
            }
            _ => Err(diagnostic_at(
                self.source,
                Arc::clone(&self.source_label),
                node,
                DiagnosticCode::UnknownNode,
                "unknown token-predicate child",
            )),
        }
    }

    /// The operands of a flat `&` or `|` list.
    ///
    /// Both rules are `repeat1`, so the operands are direct named children and
    /// the recursion never sees a right-nested spine — the shape
    /// `attribute_conjunction`'s grammar comment demands.
    fn lower_test_operands(
        &mut self,
        node: Node<'_>,
        depth: usize,
    ) -> Result<Vec<ParsedTest>, Diagnostic> {
        let mut operands = Vec::with_capacity(node.named_child_count());
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            if child.kind() == "comment" {
                continue;
            }
            operands.push(self.lower_test(child, depth.saturating_add(1))?);
        }
        Ok(operands)
    }

    fn first_diagnostic(&self, error: CompileError, fallback_node: Node<'_>) -> Diagnostic {
        error.diagnostics.into_iter().next().unwrap_or_else(|| {
            diagnostic_at(
                self.source,
                Arc::clone(&self.source_label),
                fallback_node,
                DiagnosticCode::Syntax,
                "token-predicate lowering failed without a diagnostic",
            )
        })
    }

    /// Lowers one `[key op operand]` test.
    ///
    /// Returns [`ParsedAttr`] rather than [`AttrValue`] because `in`/`not in`
    /// admit a `set_reference`, whose members live in another document until
    /// the catalog is accumulated. Every other operator produces a
    /// [`ParsedAttrValue::Resolved`] here and is final.
    fn lower_attribute(&mut self, node: Node<'_>) -> Result<ParsedAttr, CompileError> {
        self.check_recovery_node(node)?;
        let key_node = required_field(self.source, &self.source_label, node, "key", "key")?;
        self.check_recovery_node(key_node)?;
        let key = self.text(key_node)?;
        let attr = AttributeKey::new(key).map_err(|error| {
            self.error(
                key_node,
                DiagnosticCode::UnknownNode,
                format!("invalid token attribute `{key}`: {error}"),
            )
        })?;

        let operator_node = node.child_by_field_name("operator").ok_or_else(|| {
            self.error(
                node,
                DiagnosticCode::Syntax,
                "attribute is missing field operator",
            )
        })?;
        self.check_recovery_node(operator_node)?;
        let operator_text = self.text(operator_node)?;
        let operator = if operator_text.trim_start().starts_with(':') {
            ":".to_owned()
        } else {
            operator_text
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
        };

        let value = match operator.as_str() {
            // Exact mode. Literals only: a regex operand here silently read as
            // an unanchored search, which is the defect `~` exists to fix.
            "=" | ":" => {
                let value_node =
                    required_field(self.source, &self.source_label, node, "value", "value")?;
                let literal = self.lower_literal_value(value_node, false)?;
                AttrValueType::Single(literal)
            }
            "!=" => {
                let value_node =
                    required_field(self.source, &self.source_label, node, "value", "value")?;
                let literal = self.lower_literal_value(value_node, true)?;
                AttrValueType::NotIn(Arc::from(vec![literal]))
            }
            // Search mode: literals use substring containment and regexes use
            // `is_match` semantics.
            "~" | "!~" => {
                let value_node =
                    required_field(self.source, &self.source_label, node, "value", "value")?;
                let negated = operator == "!~";
                match self.lower_scalar_value(value_node)? {
                    AttrValueType::Single(literal) if negated => {
                        AttrValueType::NotContains(literal)
                    }
                    AttrValueType::Single(literal) => AttrValueType::Contains(literal),
                    AttrValueType::Regex(regex) if negated => AttrValueType::NotRegex(regex),
                    AttrValueType::Regex(regex) => AttrValueType::Regex(regex),
                    AttrValueType::OneOf(_)
                    | AttrValueType::NotIn(_)
                    | AttrValueType::SetHas(_)
                    | AttrValueType::SetHasAny(_)
                    | AttrValueType::SetNotHas(_)
                    | AttrValueType::SetNotHasAny(_)
                    | AttrValueType::Contains(_)
                    | AttrValueType::NotContains(_)
                    | AttrValueType::NotRegex(_) => {
                        return Err(self.error(
                            value_node,
                            DiagnosticCode::Syntax,
                            "`~` and `!~` accept a literal or a regex, not a set",
                        ));
                    }
                }
            }
            "has" | "not has" => {
                let value_node =
                    required_field(self.source, &self.source_label, node, "value", "value")?;
                let literal = self.lower_literal_value(value_node, operator == "not has")?;
                if operator == "has" {
                    AttrValueType::SetHas(literal)
                } else {
                    AttrValueType::SetNotHas(literal)
                }
            }
            "has any" | "not has any" => {
                if let Some(reference) = node.child_by_field_name("set") {
                    self.check_recovery_node(reference)?;
                    let name_node = reference.named_child(0).ok_or_else(|| {
                        self.error(
                            reference,
                            DiagnosticCode::Syntax,
                            "set reference is missing its name",
                        )
                    })?;
                    return Ok(ParsedAttr {
                        attr,
                        value: ParsedAttrValue::SetAny {
                            name: self.text(name_node)?.into(),
                            negated: operator == "not has any",
                            span: self.span_of(reference)?,
                        },
                    });
                }
                let values_node =
                    required_field(self.source, &self.source_label, node, "values", "value_set")?;
                let values = self.lower_finite_values(values_node, operator == "not has any")?;
                if operator == "has any" {
                    AttrValueType::SetHasAny(Arc::from(values))
                } else {
                    AttrValueType::SetNotHasAny(Arc::from(values))
                }
            }
            "in" | "not in" => {
                // `attribute` offers two operands here: an inline `value_set`
                // and a `set_reference`. A reference cannot resolve at this
                // point — the set may be declared in another accumulated
                // document — so it is carried to `bind` instead.
                if let Some(reference) = node.child_by_field_name("set") {
                    self.check_recovery_node(reference)?;
                    // `set_reference` is `seq('@', $.identifier)` with no field
                    // on the name, so take the sole named child rather than
                    // inventing a field the grammar does not declare.
                    //
                    // The `ok_or_else` is the total-match arm and nothing
                    // more: `@` with no name parses as a MISSING `identifier`,
                    // which `check_recovery_node` above has already refused.
                    // It is a sentence rather than a panic for the reason
                    // `compile_guard`'s `Group` arm is — a later grammar
                    // change should reach a researcher as a diagnostic.
                    let name_node = reference.named_child(0).ok_or_else(|| {
                        self.error(
                            reference,
                            DiagnosticCode::Syntax,
                            "set reference is missing its name",
                        )
                    })?;
                    return Ok(ParsedAttr {
                        attr,
                        value: ParsedAttrValue::Set {
                            name: self.text(name_node)?.into(),
                            negated: operator == "not in",
                            span: self.span_of(reference)?,
                        },
                    });
                }
                let values_node =
                    required_field(self.source, &self.source_label, node, "values", "value_set")?;
                self.check_recovery_node(values_node)?;
                let mut values = Vec::new();
                let mut seen = HashSet::new();
                let mut cursor = values_node.walk();
                for value_node in values_node.named_children(&mut cursor) {
                    if value_node.kind() != "value" {
                        return Err(self.error(
                            value_node,
                            DiagnosticCode::UnknownNode,
                            "unknown finite-set member",
                        ));
                    }
                    let literal = self.lower_literal_value(value_node, operator == "not in")?;
                    if !seen.insert(literal.clone()) {
                        return Err(self.error(
                            value_node,
                            DiagnosticCode::Syntax,
                            "finite-set members must be unique",
                        ));
                    }
                    values.push(literal);
                }
                if values.is_empty() {
                    return Err(self.error(
                        values_node,
                        DiagnosticCode::Syntax,
                        "finite sets must contain at least one value",
                    ));
                }
                if operator == "in" {
                    AttrValueType::OneOf(Arc::from(values))
                } else {
                    AttrValueType::NotIn(Arc::from(values))
                }
            }
            _ => {
                return Err(self.error(
                    operator_node,
                    DiagnosticCode::Syntax,
                    "unsupported attribute operator",
                ));
            }
        };
        Ok(ParsedAttr {
            attr,
            value: ParsedAttrValue::Resolved(value),
        })
    }

    fn lower_finite_values(
        &mut self,
        values_node: Node<'_>,
        negated: bool,
    ) -> Result<Vec<CompactString>, CompileError> {
        self.check_recovery_node(values_node)?;
        let mut values = Vec::new();
        let mut seen = HashSet::new();
        let mut cursor = values_node.walk();
        for value_node in values_node.named_children(&mut cursor) {
            if value_node.kind() != "value" {
                return Err(self.error(
                    value_node,
                    DiagnosticCode::UnknownNode,
                    "unknown finite-set member",
                ));
            }
            let literal = self.lower_literal_value(value_node, negated)?;
            if !seen.insert(literal.clone()) {
                return Err(self.error(
                    value_node,
                    DiagnosticCode::Syntax,
                    "finite-set members must be unique",
                ));
            }
            values.push(literal);
        }
        if values.is_empty() {
            return Err(self.error(
                values_node,
                DiagnosticCode::Syntax,
                "finite sets must contain at least one value",
            ));
        }
        values.sort_unstable();
        Ok(values)
    }

    fn lower_scalar_value(&mut self, value_node: Node<'_>) -> Result<AttrValueType, CompileError> {
        self.check_recovery_node(value_node)?;
        let value = match value_node.named_child_count() {
            0 => self.lower_leaf_value(value_node)?,
            1 => {
                let child = value_node.named_child(0).ok_or_else(|| {
                    self.error(
                        value_node,
                        DiagnosticCode::Syntax,
                        "attribute value is missing",
                    )
                })?;
                match child.kind() {
                    "identifier" => AttrValueType::Single(self.text(child)?.into()),
                    "regex" => self.lower_regex(child)?,
                    _ => {
                        return Err(self.error(
                            child,
                            DiagnosticCode::UnknownNode,
                            "unknown attribute value node",
                        ));
                    }
                }
            }
            _ => {
                return Err(self.error(
                    value_node,
                    DiagnosticCode::UnknownNode,
                    "attribute value contains multiple named nodes",
                ));
            }
        };
        Ok(value)
    }

    /// Lowers a value node that must be a literal.
    ///
    /// Serves `=`, `!=`, `in`, and `not in` — every operator whose operand
    /// rules exclude a regex. `negated` selects which match-mode operator the
    /// diagnostic recommends: recommending `!~` for a positive `in {…}` would
    /// send the author to the complement of what they wrote.
    fn lower_literal_value(
        &mut self,
        value_node: Node<'_>,
        negated: bool,
    ) -> Result<CompactString, CompileError> {
        self.check_recovery_node(value_node)?;
        let literal_node = match value_node.named_child_count() {
            0 => value_node,
            1 => value_node.named_child(0).ok_or_else(|| {
                self.error(
                    value_node,
                    DiagnosticCode::Syntax,
                    "literal value is missing",
                )
            })?,
            _ => {
                return Err(self.error(
                    value_node,
                    DiagnosticCode::UnknownNode,
                    "literal value contains multiple named nodes",
                ));
            }
        };
        let text = self.text(literal_node)?;
        if literal_node.kind() == "regex"
            || (text.len() >= 2 && text.starts_with('/') && text.ends_with('/'))
        {
            let operator = if negated { "!~" } else { "~" };
            return Err(self.error(
                literal_node,
                DiagnosticCode::Syntax,
                format!(
                    "regex operands belong to `{operator}`: write `{operator} /…/` for a \
                     substring search, or `{operator} /\\A(?:…)\\z/` for a whole-value match"
                ),
            ));
        }
        self.decode_literal_escapes(literal_node, text)
    }

    fn lower_leaf_value(&mut self, node: Node<'_>) -> Result<AttrValueType, CompileError> {
        let text = self.text(node)?;
        if text.len() >= 2 && text.starts_with('/') && text.ends_with('/') {
            self.lower_regex(node)
        } else {
            Ok(AttrValueType::Single(
                self.decode_literal_escapes(node, text)?,
            ))
        }
    }

    /// Decode `\uXXXX` and `\\` in a non-regex literal value.
    ///
    /// Regex values are excluded: their backslashes belong to the regex engine.
    /// Every unrecognised escape is an error rather than a literal backslash,
    /// which keeps the remaining spellings available for later use.
    ///
    /// This is also the one place every unquoted literal passes through — `=`,
    /// `!=`, `in`, `not in`, `~`, `!~` and a `set` member alike — which is why
    /// [`Self::refuse_unspaced_pipe`] is called from here rather than from
    /// each operator's arm. Reaching it at all is what proves the value is not
    /// a regex.
    fn decode_literal_escapes(
        &mut self,
        node: Node<'_>,
        text: &str,
    ) -> Result<CompactString, CompileError> {
        self.refuse_unspaced_pipe(node, text)?;
        self.refuse_quoted_value(node, text)?;
        if !text.contains('\\') {
            return Ok(text.into());
        }
        let mut decoded = String::with_capacity(text.len());
        let mut characters = text.chars();
        while let Some(character) = characters.next() {
            if character != '\\' {
                decoded.push(character);
                continue;
            }
            match characters.next() {
                Some('\\') => decoded.push('\\'),
                Some('u') => {
                    let digits: String = characters.by_ref().take(4).collect();
                    // `from_str_radix` alone would accept a leading sign, so
                    // `\u+123` would name U+0123. Four hex digits means four
                    // hex digits.
                    let code = (digits.len() == 4
                        && digits.chars().all(|digit| digit.is_ascii_hexdigit()))
                    .then(|| u32::from_str_radix(&digits, 16).ok())
                    .flatten();
                    let Some(character) = code.and_then(char::from_u32) else {
                        return Err(self.error(
                            node,
                            DiagnosticCode::Syntax,
                            "`\\u` takes exactly four hexadecimal digits naming a Unicode scalar",
                        ));
                    };
                    decoded.push(character);
                }
                _ => {
                    return Err(self.error(
                        node,
                        DiagnosticCode::Syntax,
                        "unrecognised escape in a literal value; `\\uXXXX` and `\\\\` are the \
                         only ones, and every other `\\x` is reserved",
                    ));
                }
            }
        }
        Ok(decoded.into())
    }

    /// Refuse an unquoted literal that swallowed a `|`.
    ///
    /// The grammar's bare `value` token stops at `&` and does **not** stop at
    /// `|`, so `[text=a|text=b]` lowers to one atom whose value is the seven
    /// characters `a|text=b`. That predicate matches no token, emits nothing,
    /// and is indistinguishable from the phenomenon being absent — while
    /// `[text=a & unidic.pos1=N]` written unspaced means exactly what it looks like,
    /// which is what makes the wrong reading the natural one to write.
    ///
    /// Refusing this spelling prevents a silently empty result. Literal pipes
    /// remain expressible because escape decoding runs after tokenization.
    ///
    /// Regex operands never reach here — [`Self::lower_leaf_value`] routes them
    /// to [`Self::lower_regex`] first — which is what keeps `[text~/a|b/]`,
    /// where `|` is legitimate alternation, accepted.
    fn refuse_unspaced_pipe(&self, node: Node<'_>, text: &str) -> Result<(), CompileError> {
        if !text.contains('|') {
            return Ok(());
        }
        Err(self.error(
            node,
            DiagnosticCode::PipeInBareValue,
            format!(
                "`|` does not end an unquoted value, so this value would be the literal \
                 `{text}`. Write `\\u007C` for a pipe inside the literal, or space the \
                 operands (`a | b`) for a disjunction"
            ),
        ))
    }

    /// Refuse an unquoted literal that carried metadata's quoting convention.
    ///
    /// lingrep patterns quotes metadata values always and token-test values never, so
    /// `[text="の"]` is well-formed and asks for the three-character literal
    /// `"の"`. No corpus holds one, so the catalog compiles clean, warns
    /// nothing, and reports zero — the same silence
    /// [`Self::refuse_unspaced_pipe`] exists to close.
    ///
    /// Reached from [`Self::decode_literal_escapes`], so it covers every
    /// unquoted literal position at once: `=`, `!=`, `in`, `not in`, `has`,
    /// `has any` and a `set` member. Regex operands never arrive here, so
    /// `[text~/"/]` is untouched.
    ///
    /// A literal that genuinely begins and ends with a double quote stays
    /// expressible, because escape decoding runs after tokenization.
    fn refuse_quoted_value(&self, node: Node<'_>, text: &str) -> Result<(), CompileError> {
        // `strip_suffix` after `strip_prefix` is what makes a lone `"` not a
        // wrapping pair: stripping the prefix leaves nothing for the suffix.
        let Some(inner) = text
            .strip_prefix('"')
            .and_then(|rest| rest.strip_suffix('"'))
        else {
            return Ok(());
        };
        Err(self.error(
            node,
            DiagnosticCode::QuotedAttributeValue,
            format!(
                "a token-test value is never quoted, so this value would be the literal \
                 `{text}`, quotes included. Write `{inner}` for the value itself, or \
                 `\\u0022{inner}\\u0022` if the quotes belong inside it"
            ),
        ))
    }

    fn lower_regex(&mut self, node: Node<'_>) -> Result<AttrValueType, CompileError> {
        let text = self.text(node)?;
        let Some(inner) = text
            .strip_prefix('/')
            .and_then(|text| text.strip_suffix('/'))
        else {
            return Err(self.error(
                node,
                DiagnosticCode::InvalidRegex,
                "regex value is missing delimiters",
            ));
        };
        self.regex_bytes = self.regex_bytes.checked_add(inner.len()).ok_or_else(|| {
            self.error(
                node,
                DiagnosticCode::LimitExceeded,
                "regex byte count overflowed",
            )
        })?;
        if self.regex_bytes > self.limits.max_regex_bytes() {
            return Err(self.error(
                node,
                DiagnosticCode::LimitExceeded,
                "lingrep patterns regexes exceed the regex byte limit",
            ));
        }

        Regex::new(inner)
            .map(AttrValueType::Regex)
            .map_err(|_| self.error(node, DiagnosticCode::InvalidRegex, "invalid Rust regex"))
    }

    fn literal_token(&self, node: Node<'_>) -> Result<ParsedExpr, CompileError> {
        Ok(ParsedExpr::Token(ParsedTest::Attr(resolved_attr(
            AttrValue {
                attr: TokenAttributeIndex::Orth,
                value: AttrValueType::Single(self.text(node)?.into()),
                complement: false,
            },
        ))))
    }

    fn enter_expr(&mut self, node: Node<'_>, depth: usize) -> Result<(), CompileError> {
        self.check_depth(node, depth)?;
        self.expression_nodes = self.expression_nodes.checked_add(1).ok_or_else(|| {
            self.error(
                node,
                DiagnosticCode::LimitExceeded,
                "expression node count overflowed",
            )
        })?;
        if self.expression_nodes > self.limits.max_expression_nodes() {
            return Err(self.error(
                node,
                DiagnosticCode::LimitExceeded,
                "lingrep patterns document exceeds the expression-node limit",
            ));
        }
        Ok(())
    }

    fn check_depth(&self, node: Node<'_>, depth: usize) -> Result<(), CompileError> {
        if depth > self.limits.max_nesting_depth() {
            return Err(self.error(
                node,
                DiagnosticCode::LimitExceeded,
                "lingrep patterns expression exceeds the nesting-depth limit",
            ));
        }
        Ok(())
    }

    /// [`Self::check_recovery_node`] over an entire subtree.
    ///
    /// The expression lowerer gets this for free — it calls
    /// `check_recovery_node` at every node it descends through. The policy
    /// lowerer is a separate module that does not carry this `Lowerer`'s
    /// recovery heuristics, so its block is checked whole before it is handed
    /// over rather than growing a second, weaker copy of them.
    fn check_recovery_subtree(&self, node: Node<'_>) -> Result<(), CompileError> {
        self.check_recovery_node(node)?;
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            self.check_recovery_subtree(child)?;
        }
        Ok(())
    }

    fn check_recovery_node(&self, node: Node<'_>) -> Result<(), CompileError> {
        if node.is_error() || node.is_missing() {
            return Err(self.recovery_error(node));
        }

        let mut has_named_error_child = false;
        for index in 0..node.child_count() {
            let index = u32::try_from(index).map_err(|_| {
                self.error(
                    node,
                    DiagnosticCode::LimitExceeded,
                    "lingrep patterns syntax node has too many children",
                )
            })?;
            let Some(child) = node.child(index) else {
                continue;
            };
            if child.is_error() || child.is_missing() || (child.has_error() && !child.is_named()) {
                return Err(self.recovery_error(child));
            }
            has_named_error_child |= child.has_error();
        }

        if node.has_error() && !has_named_error_child {
            // Anonymous MISSING tokens are represented by their smallest
            // named ancestor, often a zero-width `value`.
            return Err(self.recovery_error(node));
        }
        Ok(())
    }

    /// Build the user-facing message for a tree-sitter recovery node. For a
    /// MISSING node the message names the missing token kind (e.g.
    /// `missing \`.\``); for an ERROR node (tree-sitter's recovery marker, not a
    /// real expected token) the message is the generic `invalid lingrep patterns syntax`,
    /// never `expected ERROR`.
    fn recovery_message(&self, node: Node<'_>) -> String {
        if node.is_missing() {
            format!("missing `{}`", node.kind())
        } else {
            "invalid lingrep patterns syntax".to_string()
        }
    }

    /// Report a recovery node, re-pointing the one shape tree-sitter gets
    /// unhelpfully wrong: a definition whose `.` is missing and which is
    /// followed by another definition.
    fn recovery_error(&self, node: Node<'_>) -> CompileError {
        if let Some(offset) = self.unterminated_definition_offset(node) {
            return error_at_offset(
                self.source,
                Arc::clone(&self.source_label),
                offset,
                DiagnosticCode::Syntax,
                "missing `.`",
            );
        }
        self.error(node, DiagnosticCode::Syntax, self.recovery_message(node))
    }

    /// Byte offset where a missing `.` belongs, if `node` is the ERROR that
    /// tree-sitter emits when a definition runs into the next one.
    ///
    /// Omitting a `.` does NOT produce a MISSING node unless the definition is
    /// last in the file. Otherwise the parser absorbs the next definition's
    /// head as `bare_element` pattern references and errors on its `:=`.
    ///
    /// Every gate here exists to keep a *confident* message off an *uncertain*
    /// shape. When any of them fails the caller falls back to the generic
    /// message, which is no worse than the behavior this replaces.
    fn unterminated_definition_offset(&self, node: Node<'_>) -> Option<usize> {
        // The ERROR must be the absorbed definition's `:=`. This is what keeps
        // ordinary malformed input — a real reference followed by an unrelated
        // ERROR — out of this path entirely.
        if !node.is_error() || self.text(node).ok()? != ":=" {
            return None;
        }

        // A definition can only begin at the top level of another definition's
        // body. Inside a group, a lookaround, or an alternation branch, the
        // same local shape means something else. Rejecting by ancestry is what
        // prevents a false `missing `.`` inside `( … )`.
        let sequence = node.parent()?;
        if sequence.kind() != "pattern_sequence"
            || sequence.parent()?.kind() != "pattern_definition"
        {
            return None;
        }

        // The node immediately before the ERROR is the absorbed name.
        let name = node.prev_named_sibling()?;
        if name.kind() != "bare_element" {
            return None;
        }

        // Consume ONE more node only when it is literally the `pattern` or
        // `fragment` keyword on the same line as the name. Consuming an
        // arbitrary run of `bare_element`s would eat a legitimate trailing
        // pattern reference — `a := ref` — and lose the real final term.
        let mut previous = name.prev_named_sibling()?;
        if previous.kind() == "bare_element"
            && matches!(self.text(previous).ok()?, "pattern" | "fragment")
            && previous.start_position().row == name.start_position().row
        {
            previous = previous.prev_named_sibling()?;
        }

        // `#…` runs to end of line, so a `.` placed after a comment would be
        // swallowed by it. Step back to the last real term.
        while previous.kind() == "comment" {
            previous = previous.prev_named_sibling()?;
        }

        // The absorbed header must start on a LATER line than the term ends.
        // Two definitions on one line are genuinely ambiguous; stay generic.
        if name.start_position().row <= previous.end_position().row {
            return None;
        }

        Some(previous.end_byte())
    }

    fn text(&self, node: Node<'_>) -> Result<&'src str, CompileError> {
        node.utf8_text(self.source.text.as_bytes())
            .map_err(|error| {
                self.error(
                    node,
                    DiagnosticCode::Syntax,
                    format!("node text is not valid UTF-8: {error}"),
                )
            })
    }

    fn error(
        &self,
        node: Node<'_>,
        code: DiagnosticCode,
        message: impl Into<String>,
    ) -> CompileError {
        error_at(
            self.source,
            Arc::clone(&self.source_label),
            node,
            code,
            message,
        )
    }
}

/// Wraps an inline-literal attribute as already resolved.
///
/// Used where the lowerer synthesizes a predicate itself rather than reading
/// one out of `attribute`: a bare literal token, and the tests' expectations.
/// [`Lowerer::lower_attribute`] builds its own variant, because only it can
/// see whether the operand was a `set_reference`.
fn resolved_attr(attribute: AttrValue) -> ParsedAttr {
    ParsedAttr {
        attr: attribute.attr.into(),
        value: ParsedAttrValue::Resolved(attribute.value),
    }
}

fn is_expression_kind(kind: &str) -> bool {
    matches!(
        kind,
        "alternation"
            | "intersection"
            | "any_order"
            | "quantified_pattern"
            | "bare_element"
            | "named_group"
            | "node_pattern"
            | "sentence_start"
            | "sentence_end"
            | "preceded_by"
            | "not_preceded_by"
            | "followed_by"
            | "not_followed_by"
            | "same_as"
            | "parenthesized_pattern"
            | "pattern_reference"
            | "pattern_sequence"
    )
}

fn only_expression_child<'tree>(
    source: &PatternSource<'_>,
    source_label: &Arc<str>,
    node: Node<'tree>,
) -> Result<Node<'tree>, CompileError> {
    let mut expression = None;
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if child.kind() == "comment" {
            continue;
        }
        if !is_expression_kind(child.kind()) || expression.replace(child).is_some() {
            return Err(error_at(
                source,
                Arc::clone(source_label),
                child,
                DiagnosticCode::UnknownNode,
                "parenthesized pattern must contain one known expression",
            ));
        }
    }
    expression.ok_or_else(|| {
        error_at(
            source,
            Arc::clone(source_label),
            node,
            DiagnosticCode::Syntax,
            "parenthesized pattern is empty",
        )
    })
}

/// The sole test node inside a `parenthesized_test` or a `test_negation`.
///
/// The counterpart of [`only_expression_child`] for the token-test grammar.
/// Neither rule declares a field on its operand, so this takes the one named
/// child rather than inventing one; the kind is then checked by
/// [`Lowerer::lower_test`]'s own dispatch, which is the only place that knows
/// the full set.
fn only_test_child<'tree>(
    source: &PatternSource<'_>,
    source_label: &Arc<str>,
    node: Node<'tree>,
) -> Result<Node<'tree>, Diagnostic> {
    let mut operand = None;
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if child.kind() == "comment" {
            continue;
        }
        if operand.replace(child).is_some() {
            return Err(diagnostic_at(
                source,
                Arc::clone(source_label),
                child,
                DiagnosticCode::UnknownNode,
                format!("{} has more than one operand", node.kind()),
            ));
        }
    }
    operand.ok_or_else(|| {
        diagnostic_at(
            source,
            Arc::clone(source_label),
            node,
            DiagnosticCode::Syntax,
            format!("{} is empty", node.kind()),
        )
    })
}

fn only_named_child<'tree>(
    source: &PatternSource<'_>,
    source_label: &Arc<str>,
    node: Node<'tree>,
    expected_kind: &str,
) -> Result<Node<'tree>, CompileError> {
    let mut cursor = node.walk();
    let mut children = node
        .named_children(&mut cursor)
        .filter(|child| child.kind() != "comment");
    let child = children.next().ok_or_else(|| {
        error_at(
            source,
            Arc::clone(source_label),
            node,
            DiagnosticCode::Syntax,
            format!("{} is missing its named child", node.kind()),
        )
    })?;
    if child.kind() != expected_kind || children.next().is_some() {
        return Err(error_at(
            source,
            Arc::clone(source_label),
            child,
            DiagnosticCode::UnknownNode,
            format!("{} has an unknown named child", node.kind()),
        ));
    }
    Ok(child)
}

fn required_field<'tree>(
    source: &PatternSource<'_>,
    source_label: &Arc<str>,
    node: Node<'tree>,
    field: &str,
    expected_kind: &str,
) -> Result<Node<'tree>, CompileError> {
    let child = node.child_by_field_name(field).ok_or_else(|| {
        error_at(
            source,
            Arc::clone(source_label),
            node,
            DiagnosticCode::Syntax,
            format!("{} is missing field {field}", node.kind()),
        )
    })?;
    if child.kind() != expected_kind {
        return Err(error_at(
            source,
            Arc::clone(source_label),
            child,
            DiagnosticCode::UnknownNode,
            format!("field {field} has an unknown named node"),
        ));
    }
    Ok(child)
}

/// Build an error at an explicit byte offset rather than at a node's range.
/// The resulting span is zero-width, matching what tree-sitter produces for a
/// MISSING token — the renderer already handles that shape.
fn error_at_offset(
    source: &PatternSource<'_>,
    source_label: Arc<str>,
    offset: usize,
    code: DiagnosticCode,
    message: impl Into<String>,
) -> CompileError {
    let (line, column) = source_line_column(source.text, offset);
    CompileError::from_diagnostics(vec![Diagnostic::error_with_source_label(
        code,
        source_label,
        offset..offset,
        message,
        line,
        column,
    )])
}

/// Build an error at an explicit byte range.
///
/// Used where the offending thing is a recorded `SourceSpan` rather than a live
/// CST node — a materialized default's key, for instance, whose node belongs to
/// the `defaults` block and not to the definition being reported.
fn error_at_range(
    source: &PatternSource<'_>,
    source_label: Arc<str>,
    range: Range<usize>,
    code: DiagnosticCode,
    message: impl Into<String>,
) -> CompileError {
    let (line, column) = source_line_column(source.text, range.start);
    CompileError::from_diagnostics(vec![Diagnostic::error_with_source_label(
        code,
        source_label,
        range,
        message,
        line,
        column,
    )])
}

fn error_at(
    source: &PatternSource<'_>,
    source_label: Arc<str>,
    node: Node<'_>,
    code: DiagnosticCode,
    message: impl Into<String>,
) -> CompileError {
    CompileError::from_diagnostics(vec![diagnostic_at(
        source,
        source_label,
        node,
        code,
        message,
    )])
}

/// One error diagnostic anchored at a node, with its line and column.
///
/// `pub(super)` because the policy lowerer is a sibling module rather than a
/// method on [`Lowerer`], and reporting through a second construction path
/// would let the two drift on how a position is computed.
pub(super) fn diagnostic_at(
    source: &PatternSource<'_>,
    source_label: Arc<str>,
    node: Node<'_>,
    code: DiagnosticCode,
    message: impl Into<String>,
) -> Diagnostic {
    let (line, column) = source_line_column(source.text, node.start_byte());
    Diagnostic::error_with_source_label(
        code,
        source_label,
        node.byte_range(),
        message,
        line,
        column,
    )
}

fn source_line_column(source: &str, offset: usize) -> (usize, usize) {
    // `offset` is a tree-sitter byte offset (diagnostic/`node.start_byte()`),
    // always char-aligned.
    #[expect(
        clippy::string_slice,
        reason = "`offset` is a tree-sitter byte offset, always a char boundary"
    )]
    let prefix = &source[..offset];
    let line = prefix.bytes().filter(|byte| *byte == b'\n').count() + 1;
    let line_start = prefix.rfind('\n').map_or(0, |index| index + 1);
    (line, offset - line_start + 1)
}

#[cfg(test)]
#[path = "lower_tests.rs"]
mod tests;
