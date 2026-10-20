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
                                "`where` constraint relates `${head}` to itself; a \
                                 basic dependency tree contains no self-edge, so this \
                                 pattern could never match"
                            ),
                        ));
                    }
                    ParsedRelationTarget::Captured {
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
mod tests {
    // Every span assertion below slices the fixture by a byte range the
    // compiler reported. Test code is exempt from the panic deny-floor
    // (docs/lint-policy.md), and a panic here is the test failing, which is
    // what it is for.
    #![allow(clippy::string_slice)]

    use super::{lower_definition, lower_document, lower_document_with_stats, resolved_attr};
    use crate::fsm::{AttrValue, AttrValueType};
    use crate::token_match::AttributeKey;
    use crate::token_match::TokenAttributeIndex;
    use serde_json::json;
    use std::collections::BTreeSet;

    use super::super::PatternSource;
    use super::super::diagnostic::{CompileError, DiagnosticCode};
    use super::super::limits::CompileLimits;
    use super::super::model::{
        CaptureProjection, LookDirection, LookPolarity, LookaroundExpr, ParsedAttrValue,
        ParsedDefinition, ParsedDefinitionBody, ParsedExpr, ParsedProgram, ParsedTest, SourceId,
        SourceSpan,
    };

    fn lower(source: &str) -> ParsedProgram {
        lower_document(
            &PatternSource::named("fixture.lx", source),
            &CompileLimits::default(),
        )
        .expect("fixture must lower")
    }

    /// The source text a span covers.
    ///
    /// Every span assertion goes through this, so none of them can silently
    /// degrade into "some two spans differ" — a value span, a whole-line span,
    /// and the right span all satisfy `assert_ne!`; this helper asserts the
    /// authored text itself.
    fn spanned(source: &str, span: SourceSpan) -> &str {
        let range = span.byte_range();
        &source[range.start as usize..range.end as usize]
    }

    /// The byte range an error's first diagnostic points at.
    fn error_range(error: &CompileError) -> std::ops::Range<usize> {
        error
            .diagnostics
            .first()
            .expect("an error carries at least one diagnostic")
            .location()
            .byte_range()
    }

    fn refuse(source: &str) -> CompileError {
        lower_document(
            &PatternSource::named("fixture.lx", source),
            &CompileLimits::default(),
        )
        .expect_err("source is refused")
    }

    #[test]
    fn annotation_run_binds_to_the_following_definition_with_key_spans() {
        let source = "layer: \"S2\"\nexamples: [\"a\"]\nteiru := x.\n";
        let program = lower(source);
        assert_eq!(program.definitions.len(), 1);
        let metadata = &program.definitions[0].metadata;
        assert_eq!(metadata["layer"].value, json!("S2"));
        assert_eq!(metadata["examples"].value, json!(["a"]));
        assert_eq!(spanned(source, metadata["layer"].span), "layer");
        assert_eq!(spanned(source, metadata["examples"].span), "examples");
    }

    #[test]
    fn a_dangling_annotation_run_reports_its_first_key() {
        let source = "layer: \"S2\"\ncategory: \"c\"\n";
        let error = refuse(source);
        assert!(
            error.to_string().contains("annotates no definition"),
            "message names the defect: {error}"
        );
        assert_eq!(
            &source[error_range(&error)],
            "layer",
            "the run's first key is where the author starts reading"
        );
    }

    #[test]
    fn a_duplicate_key_in_one_run_reports_the_later_key() {
        let source = "layer: \"S1\"\nlayer: \"S2\"\np := x.\n";
        let error = refuse(source);
        assert!(
            error.to_string().contains("duplicate metadata key"),
            "{error}"
        );
        let range = error_range(&error);
        assert_eq!(&source[range.clone()], "layer");
        assert_eq!(
            range.start,
            source.find("layer: \"S2\"").expect("second entry present"),
            "the later of the two keys is reported, not the first"
        );
    }

    #[test]
    fn a_second_defaults_block_is_refused() {
        let source = "defaults\n  a: \"1\"\nend\ndefaults\n  b: \"2\"\nend\np := x.\n";
        let error = refuse(source);
        assert!(
            error.to_string().contains("at most one `defaults` block"),
            "{error}"
        );
        assert_eq!(
            error_range(&error).start,
            source.find("defaults\n  b").expect("second block present"),
            "the second block is where the author has to look"
        );
    }

    #[test]
    fn a_second_schema_block_is_refused() {
        let source = "schema\n  layer: string\nend\nschema\n  layer: string\nend\np := x.\n";
        let error = refuse(source);
        assert!(
            error.to_string().contains("at most one `schema` block"),
            "{error}"
        );
        assert_eq!(
            error_range(&error).start,
            source.rfind("schema").expect("two schema blocks"),
            "the second block is reported"
        );
    }

    #[test]
    fn a_second_policy_block_is_refused() {
        const BLOCK: &str = "policy\n  resolve all rank: [longest, start, end, pattern].\nend\n";
        let source = format!("{BLOCK}{BLOCK}p := x.\n");
        let error = refuse(&source);
        assert!(
            error.to_string().contains("at most one `policy` block"),
            "{error}"
        );
        assert_eq!(error_range(&error).start, BLOCK.len(), "the second block");
    }

    #[test]
    fn defaults_after_the_first_annotation_is_refused() {
        let source = "layer: \"S1\"\ndefaults\n  a: \"1\"\nend\np := x.\n";
        let error = refuse(source);
        assert!(
            error
                .to_string()
                .contains("a `defaults` block must precede the first annotation or definition"),
            "{error}"
        );
        assert_eq!(
            error_range(&error).start,
            source.find("defaults").expect("block present")
        );
    }

    #[test]
    fn defaults_after_the_first_definition_is_refused() {
        let source = "p := x.\ndefaults\n  a: \"1\"\nend\n";
        let error = refuse(source);
        assert!(
            error
                .to_string()
                .contains("a `defaults` block must precede the first annotation or definition"),
            "{error}"
        );
    }

    #[test]
    fn schema_after_the_first_definition_is_refused() {
        let source = "p := x.\nschema\n  layer: string\nend\n";
        let error = refuse(source);
        assert!(
            error
                .to_string()
                .contains("a `schema` block must precede the first annotation or definition"),
            "{error}"
        );
    }

    #[test]
    fn an_analyzer_declaration_after_an_annotation_is_refused() {
        let source = "layer: \"S1\"\n\
                      analyzer: { family: \"vibrato\", resource: \"unidic-cwj\" }\n\
                      p := x.\n";
        let error = refuse(source);
        assert!(
            error
                .to_string()
                .contains("analyzer declarations must precede annotations and definitions"),
            "{error}"
        );
    }

    #[test]
    fn an_annotation_run_before_a_non_pattern_declaration_is_refused() {
        // Every catalog-value declaration shares one refusal: a run can only
        // annotate the pattern or lexicon that follows it, so a declaration
        // that takes no metadata must not swallow the run.
        for declaration in [
            "set S := {a}.",
            "test t := text=a.",
            "group g := layer in {\"S1\"}.",
        ] {
            let source = format!("layer: \"S1\"\n{declaration}\np := x.\n");
            let error = refuse(&source);
            assert!(
                error
                    .to_string()
                    .contains("metadata annotates no definition"),
                "{declaration}: {error}"
            );
            assert_eq!(
                error_range(&error).start,
                0,
                "{declaration}: the run's first key is reported, not the declaration"
            );
        }
    }

    #[test]
    fn a_string_value_must_be_quoted() {
        refuse("status: experimental\np := x.\n");
    }

    #[test]
    fn there_is_no_null_metadata_value() {
        refuse("source: null\np := x.\n");
    }

    #[test]
    fn comments_and_blank_lines_do_not_break_an_annotation_run() {
        let source = "layer: \"S2\"\n# why this layer\n\ncategory: \"S2d\"\np := x.\n";
        let program = lower(source);
        assert_eq!(program.definitions.len(), 1);
        let metadata = &program.definitions[0].metadata;
        assert_eq!(metadata["layer"].value, json!("S2"));
        assert_eq!(
            metadata["category"].value,
            json!("S2d"),
            "a comment between two annotations does not start a new run"
        );
    }

    #[test]
    fn typed_values_round_trip_through_the_walker() {
        let source = "depth: 2\nratio: -1.5e2\nkeep: true\ndrop: false\n\
                      tags: [\"a\", \"b\"]\nanchor: {name: \"end\", nested: {depth: 0}}\np := x.\n";
        let program = lower(source);
        let metadata = &program.definitions[0].metadata;
        assert_eq!(metadata["depth"].value, json!(2));
        assert_eq!(metadata["ratio"].value, json!(-150.0));
        assert_eq!(metadata["keep"].value, json!(true));
        assert_eq!(metadata["drop"].value, json!(false));
        assert_eq!(metadata["tags"].value, json!(["a", "b"]));
        assert_eq!(
            metadata["anchor"].value,
            json!({"name": "end", "nested": {"depth": 0}})
        );
    }

    #[test]
    fn a_string_value_resolves_json_escapes() {
        let program = lower("note: \"a\\\"b\\nc\\u3042\"\np := x.\n");
        assert_eq!(
            program.definitions[0].metadata["note"].value,
            json!("a\"b\nc\u{3042}"),
            "escape handling is serde_json's, not a second implementation of it"
        );
    }

    #[test]
    fn a_default_is_materialized_into_every_definition() {
        let source = "defaults\n  status: \"experimental\"\nend\n\np := x.\nq := y.\n";
        let program = lower(source);
        for definition in &program.definitions {
            assert_eq!(
                definition.metadata["status"].value,
                json!("experimental"),
                "{} carries the materialized default",
                definition.name
            );
        }
    }

    #[test]
    fn a_definition_annotation_overrides_a_default() {
        let source = "defaults\n  role: \"research\"\nend\n\nrole: \"helper\"\np := x.\nq := y.\n";
        let program = lower(source);
        let by_name = |name: &str| {
            program
                .definitions
                .iter()
                .find(|definition| definition.name == name)
                .expect("definition present")
        };
        assert_eq!(by_name("p").metadata["role"].value, json!("helper"));
        assert_eq!(
            by_name("q").metadata["role"].value,
            json!("research"),
            "an override binds to one definition, not to the rest of the file"
        );
    }

    #[test]
    fn a_defaulted_key_carries_the_defaults_entry_span() {
        let source = "defaults\n  status: \"experimental\"\nend\n\np := x.\n";
        let program = lower(source);
        let span = program.definitions[0].metadata["status"].span;
        assert_eq!(spanned(source, span), "status");
        assert!(
            (span.start() as usize) < source.find("p :=").expect("definition present"),
            "a defaulted key points at the default, where the author would fix it"
        );
    }

    #[test]
    fn a_second_defaults_block_is_an_error() {
        let error = refuse("defaults\n  a: \"1\"\nend\ndefaults\n  b: \"2\"\nend\np := x.\n");
        assert!(
            error.to_string().contains("one `defaults` block"),
            "{error}"
        );
    }

    #[test]
    fn a_defaults_block_after_a_definition_is_an_error() {
        let error = refuse("p := x.\ndefaults\n  a: \"1\"\nend\n");
        assert!(
            error.to_string().contains("must precede"),
            "message says where it belongs: {error}"
        );
    }

    #[test]
    fn a_duplicate_key_within_defaults_reports_the_later_key() {
        let source = "defaults\n  alpha: \"1\"\n  alpha: \"2\"\nend\np := x.\n";
        let error = refuse(source);
        let range = error_range(&error);
        assert_eq!(&source[range.clone()], "alpha");
        assert_eq!(
            range.start,
            source.find("alpha: \"2\"").expect("second entry present"),
            "the later of the two keys is reported"
        );
    }

    #[test]
    fn charges_a_defaults_entry_source_bytes_once() {
        const DEFINITION_COUNT: usize = 128;
        let payload = "x".repeat(8 * 1024);
        let entry = format!("  payload: \"{payload}\"");
        let mut source = format!("defaults\n{entry}\nend\n\n");
        for index in 0..DEFINITION_COUNT {
            source.push_str(&format!("p{index:03} := a.\n"));
        }
        // Assert the exact charge. A test that only survives a tight limit also
        // passes when the accounting is missing entirely, because charging zero
        // survives every limit.
        let lowered = lower_document_with_stats(
            &PatternSource::named("t.lx", &source),
            &CompileLimits::default(),
        )
        .expect("document lowers");
        assert_eq!(
            lowered.metadata_bytes,
            entry.trim_start().len(),
            "one authored entry is charged once — not once per materialized copy, \
             and not zero; leading indentation is `extras` and is not charged"
        );
    }

    #[test]
    fn refuses_a_defaults_entry_one_byte_over_the_metadata_budget() {
        let payload = "x".repeat(8 * 1024);
        let entry = format!("  payload: \"{payload}\"");
        let source = format!("defaults\n{entry}\nend\n\np := a.\n");
        let limits = CompileLimits::default()
            .tightened(|limits| limits.max_metadata_bytes = entry.trim_start().len() - 1)
            .expect("metadata limit is tightened below the authored entry");
        lower_document(&PatternSource::named("t.lx", &source), &limits)
            .expect_err("the budget binds the authored entry");
    }

    #[test]
    fn materialized_defaults_are_charged_as_owned_memory_per_pattern() {
        const DEFINITION_COUNT: usize = 64;
        let payload = "x".repeat(512);
        let mut with_default = format!("defaults\n  note: \"{payload}\"\nend\n\n");
        let mut without_default = String::new();
        for index in 0..DEFINITION_COUNT {
            with_default.push_str(&format!("p{index:03} := a.\n"));
            without_default.push_str(&format!("p{index:03} := a.\n"));
        }
        let charged = |source: &str| {
            super::super::compile_catalog(
                PatternSource::named("t.lx", source),
                CompileLimits::default(),
            )
            .expect("document compiles")
            .stats
            .generated_owned_bytes()
        };
        let extra = charged(&with_default) - charged(&without_default);
        assert!(
            extra >= payload.len() * DEFINITION_COUNT,
            "each materialized copy is real owned memory and must be counted; \
             charging it once would make the budget lie about the artifact"
        );
    }

    #[test]
    fn the_public_pattern_metadata_carries_values_not_entries() {
        let output = super::super::compile_catalog(
            PatternSource::named("t.lx", "layer: \"S2\"\np := a.\n"),
            CompileLimits::default(),
        )
        .expect("document compiles");
        // The public contract is HashMap<CompactString, serde_json::Value>;
        // spans stay behind the compiler boundary.
        assert_eq!(
            output.matcher.patterns()["p"].metadata["layer"],
            json!("S2")
        );
    }

    #[test]
    fn missing_definition_terminator_names_the_dot_exactly() {
        let error = lower_document(
            &PatternSource::named("fixture.lx", "inline := [text=を]"),
            &CompileLimits::default(),
        )
        .expect_err("missing terminator must fail");
        let d = &error.diagnostics[0];
        assert_eq!(d.code(), DiagnosticCode::Syntax);
        assert_eq!(d.location().byte_range(), 20..20);
        assert_eq!(d.location().line(), 1);
        assert_eq!(d.location().column(), 21);
        assert_eq!(d.message, "missing `.`");
    }

    #[test]
    fn error_node_recovery_uses_generic_message_not_expected_error() {
        let error = lower_document(
            &PatternSource::named("fixture.lx", "inline := [text=]"),
            &CompileLimits::default(),
        )
        .expect_err("empty attribute value must fail");
        let d = &error.diagnostics[0];
        assert_eq!(d.code(), DiagnosticCode::Syntax);
        assert!(
            d.message == "invalid lingrep patterns syntax",
            "must not be 'expected ERROR'; got: {}",
            d.message
        );
        assert!(!d.message.contains("ERROR"), "got: {}", d.message);
    }

    #[test]
    fn bare_cr_compilation_behavior_is_unchanged_by_parser_view_extraction() {
        let source = PatternSource::named("bare-cr.lx", "x := [text~/a\rb/].");
        assert!(lower_document(&source, &CompileLimits::default()).is_ok());
    }

    fn only_definition(source: &str) -> ParsedDefinition {
        let mut definitions = lower(source).definitions;
        assert_eq!(definitions.len(), 1, "fixture must define one pattern");
        definitions.remove(0)
    }

    fn only_expr(source: &str) -> ParsedExpr {
        match only_definition(source).body {
            ParsedDefinitionBody::Pattern { expr, .. } => expr,
            ParsedDefinitionBody::Lexicon { .. } => panic!("fixture must define a pattern"),
        }
    }

    fn token(text: &str) -> ParsedExpr {
        ParsedExpr::Token(ParsedTest::Attr(resolved_attr(AttrValue {
            attr: TokenAttributeIndex::Orth,
            value: AttrValueType::Single(text.into()),
            complement: false,
        })))
    }

    /// The parsed test a bracketed conjunction of `attributes` lowers to.
    ///
    /// One attribute is [`ParsedTest::Attr`] and several are
    /// [`ParsedTest::And`], because that is what the grammar produces: a lone
    /// attribute is an `attribute` node, and `&` is what introduces an
    /// `attribute_conjunction`. Written out here so a fixture below does not
    /// have to remember which.
    fn conjunction(attributes: Vec<super::super::model::ParsedAttr>) -> ParsedTest {
        match <[_; 1]>::try_from(attributes) {
            Ok([only]) => ParsedTest::Attr(only),
            Err(several) => ParsedTest::And(several.into_iter().map(ParsedTest::Attr).collect()),
        }
    }

    fn assert_diagnostic(
        source: &str,
        limits: &CompileLimits,
        code: DiagnosticCode,
        byte_range: std::ops::Range<usize>,
    ) {
        let error = lower_document(&PatternSource::named("fixture.lx", source), limits)
            .expect_err("fixture must be rejected");
        assert_eq!(error.diagnostics.len(), 1);
        assert_eq!(error.diagnostics[0].code(), code);
        assert_eq!(error.diagnostics[0].location().byte_range(), byte_range);
    }

    #[test]
    fn lowers_each_core_expression_shape_without_fallbacks() {
        let regex = regex::Regex::new(r"^(食べる|見る)$").expect("test regex is valid");
        let cases = [
            ("literal", "p := 猫.", token("猫")),
            (
                "three-way attribute conjunction",
                "p := [upos=VERB & unidic.cform=終止形 & lemma~/^(食べる|見る)$/].",
                ParsedExpr::Token(conjunction(vec![
                    resolved_attr(AttrValue {
                        attr: TokenAttributeIndex::Pos,
                        value: AttrValueType::Single("VERB".into()),
                        complement: false,
                    }),
                    resolved_attr(AttrValue {
                        attr: TokenAttributeIndex::CForm,
                        value: AttrValueType::Single("終止形".into()),
                        complement: false,
                    }),
                    resolved_attr(AttrValue {
                        attr: TokenAttributeIndex::Lemma,
                        value: AttrValueType::Regex(regex),
                        complement: false,
                    }),
                ])),
            ),
            (
                "sequence",
                "p := 猫 が いる.",
                ParsedExpr::Sequence(vec![token("猫"), token("が"), token("いる")]),
            ),
            (
                "alternation precedence",
                "p := a b | c d.",
                ParsedExpr::Alternation(vec![
                    ParsedExpr::Sequence(vec![token("a"), token("b")]),
                    ParsedExpr::Sequence(vec![token("c"), token("d")]),
                ]),
            ),
        ];

        for (label, source, expected) in cases {
            assert_eq!(only_expr(source), expected, "case: {label}");
        }
    }

    #[test]
    fn classifies_only_complete_slash_delimited_leaf_values_as_regex() {
        assert_eq!(
            only_expr("p := [text~/a+/]."),
            ParsedExpr::Token(conjunction(vec![resolved_attr(AttrValue {
                attr: TokenAttributeIndex::Orth,
                value: AttrValueType::Regex(regex::Regex::new("a+").expect("test regex is valid"),),
                complement: false,
            })]))
        );
        assert_eq!(
            only_expr("p := [text=and/or]."),
            ParsedExpr::Token(conjunction(vec![resolved_attr(AttrValue {
                attr: TokenAttributeIndex::Orth,
                value: AttrValueType::Single("and/or".into()),
                complement: false,
            })]))
        );
    }

    #[test]
    fn lowers_every_pinned_quantifier_form_to_checked_bounds() {
        let actual = only_expr("p := a? b* c+ d{2} e{3,} f{4,5}.");
        let repeat = |text, min, max| ParsedExpr::Repeat {
            expr: Box::new(token(text)),
            min,
            max,
        };

        assert_eq!(
            actual,
            ParsedExpr::Sequence(vec![
                repeat("a", 0, Some(1)),
                repeat("b", 0, None),
                repeat("c", 1, None),
                repeat("d", 2, Some(2)),
                repeat("e", 3, None),
                repeat("f", 4, Some(5)),
            ])
        );
    }

    #[test]
    fn preserves_references_parentheses_named_groups_and_unicode_names() {
        let source = "日本語 := (?<命題> (@参照 | 猫)).";
        let reference_start = source.find("@参照").expect("fixture has a reference");
        let capture_start = source.find("(?<命題>").expect("fixture has a capture");
        let definition = only_definition(source);

        assert_eq!(definition.name.as_str(), "日本語");
        assert_eq!(definition.span.byte_range(), 0..41);
        assert_eq!(
            definition.expr().expect("pattern"),
            &ParsedExpr::NamedGroup {
                name: "命題".into(),
                expr: Box::new(ParsedExpr::Group(Box::new(ParsedExpr::Alternation(vec![
                    ParsedExpr::Reference {
                        name: "参照".into(),
                        span: SourceSpan::new(
                            SourceId::new(0),
                            reference_start..reference_start + "@参照".len(),
                        )
                        .unwrap(),
                    },
                    token("猫")
                ],)))),
                span: SourceSpan::new(
                    SourceId::new(0),
                    capture_start..capture_start + "(?<命題> (@参照 | 猫))".len(),
                )
                .unwrap(),
            }
        );
    }

    #[test]
    fn lowers_capture_spans_and_span_equality_projection() {
        let source = "pattern p := (?<unit> [text=a]{1,3}) same_as($unit, by=lemma).";
        let ParsedExpr::Sequence(expressions) = only_expr(source) else {
            panic!("fixture must lower to a sequence");
        };
        assert_eq!(expressions.len(), 2);

        let group_start = source.find("(?<unit>").expect("fixture has capture");
        match &expressions[0] {
            ParsedExpr::NamedGroup { name, span, .. } => {
                assert_eq!(name, "unit");
                assert_eq!(
                    span.byte_range(),
                    u32::try_from(group_start).unwrap()
                        ..u32::try_from(group_start + "(?<unit> [text=a]{1,3})".len()).unwrap()
                );
            }
            other => panic!("expected named group, got {other:?}"),
        }

        let same_as_start = source.find("same_as").expect("fixture has same_as");
        assert_eq!(
            expressions[1],
            ParsedExpr::SameAs {
                name: "unit".into(),
                projection: CaptureProjection(TokenAttributeIndex::Lemma.into()),
                span: SourceSpan::new(
                    SourceId::new(0),
                    same_as_start..same_as_start + "same_as($unit, by=lemma)".len(),
                )
                .expect("fixture span is valid"),
            }
        );
    }

    #[test]
    fn same_as_remains_available_as_an_attribute_value() {
        assert_eq!(only_expr("pattern p := [text=same_as]."), token("same_as"));
    }

    #[test]
    fn lowers_all_regular_lookaround_forms_with_direction_polarity_body_and_span() {
        let cases = [
            (
                "preceded_by",
                LookDirection::PrecededBy,
                LookPolarity::Positive,
            ),
            (
                "not_preceded_by",
                LookDirection::PrecededBy,
                LookPolarity::Negative,
            ),
            (
                "followed_by",
                LookDirection::FollowedBy,
                LookPolarity::Positive,
            ),
            (
                "not_followed_by",
                LookDirection::FollowedBy,
                LookPolarity::Negative,
            ),
        ];

        for (construct, direction, polarity) in cases {
            let source = format!("pattern p := {construct}([text=a] [text=b]*) [text=x].");
            let start = source.find(construct).expect("fixture contains construct");
            let end = source
                .find(") [text=x]")
                .expect("fixture contains closing paren")
                + 1;
            let ParsedExpr::Sequence(mut expressions) = only_expr(&source) else {
                panic!("{construct} fixture must lower to a sequence");
            };
            let lookaround = expressions.remove(0);

            assert_eq!(
                lookaround,
                ParsedExpr::Lookaround(LookaroundExpr {
                    direction,
                    polarity,
                    body: Box::new(ParsedExpr::Sequence(vec![
                        token("a"),
                        ParsedExpr::Repeat {
                            expr: Box::new(token("b")),
                            min: 0,
                            max: None,
                        },
                    ])),
                    span: SourceSpan::new(SourceId::new(0), start..end)
                        .expect("fixture span is valid"),
                }),
                "construct: {construct}",
            );
        }
    }

    #[test]
    fn rejects_unknown_capture_projection_at_the_projection_name() {
        let source = "pattern p := (?<unit> a) same_as($unit, by=surface).";
        let start = source.find("surface").expect("fixture has projection");
        assert_diagnostic(
            source,
            &CompileLimits::default(),
            DiagnosticCode::Syntax,
            start - 5..start + "surface".len(),
        );
    }

    #[test]
    fn preserves_typed_metadata_for_every_annotated_definition() {
        let source = r#"# leading comment
examples: ["a", "考える"]
status: "FIXME"
count: 2
enabled: true
一 := 猫.
examples: ["a", "考える"]
status: "FIXME"
count: 2
enabled: true
二 := 犬. # trailing comment
外 := 鳥."#;
        let program = lower(source);

        assert_eq!(
            program
                .definitions
                .iter()
                .map(|definition| definition.name.as_str())
                .collect::<Vec<_>>(),
            ["一", "二", "外"]
        );
        let expected = [
            ("examples", json!(["a", "考える"])),
            ("status", json!("FIXME")),
            ("count", json!(2)),
            ("enabled", json!(true)),
        ];
        for definition in &program.definitions[..2] {
            for (key, value) in &expected {
                assert_eq!(&definition.metadata[*key].value, value);
            }
        }
        assert!(program.definitions[2].metadata.is_empty());
    }

    #[test]
    fn comment_only_source_may_describe_definition_syntax() {
        let program = lower("# Example syntax: name := token.");

        assert!(program.definitions.is_empty());
    }

    #[test]
    fn rejects_recovered_cst_unknown_nodes_attributes_and_non_rust_regex() {
        let cases = [
            // The unterminated bracket is a MISSING `]` rather than an ERROR
            // subtree, and the span is the zero-width position the token
            // belongs at. That is a property of the grammar's `value` token,
            // which excludes `.` — so `a` closes the value, `.` closes the
            // definition, and the only thing absent is the bracket. It read as
            // one ERROR spanning the whole definition while `.` was a legal
            // value character.
            (
                "MISSING bracket",
                "p := [text=a.",
                DiagnosticCode::Syntax,
                12..12,
            ),
            (
                "MISSING node",
                "p := [text=].",
                DiagnosticCode::Syntax,
                11..11,
            ),
            (
                "standalone expression",
                "[text=a]",
                DiagnosticCode::Syntax,
                0..8,
            ),
            (
                "unknown token attribute",
                "p := [unknown=a].",
                DiagnosticCode::Syntax,
                6..15,
            ),
            (
                "unsupported negative lookahead",
                "p := [text~/^(?!a)$/].",
                DiagnosticCode::InvalidRegex,
                11..20,
            ),
        ];

        for (label, source, code, byte_range) in cases {
            let error = lower_document(
                &PatternSource::named("fixture.lx", source),
                &CompileLimits::default(),
            )
            .expect_err(label);
            assert_eq!(error.diagnostics.len(), 1, "case: {label}");
            assert_eq!(error.diagnostics[0].code(), code, "case: {label}");
            assert_eq!(
                error.diagnostics[0].location().byte_range(),
                byte_range,
                "case: {label}"
            );
        }
    }

    #[test]
    fn malformed_attribute_conjunction_is_syntax_at_recovered_token() {
        assert_diagnostic(
            "p := [text=a & & lemma=b].",
            &CompileLimits::default(),
            DiagnosticCode::Syntax,
            13..14,
        );
    }

    #[test]
    fn rejects_invalid_or_overflowing_quantifier_bounds() {
        for source in ["p := a{5,2}.", "p := a{999999999999999999999999999999}."] {
            let quantifier_start = source.find('{').expect("fixture has a quantifier");
            assert_diagnostic(
                source,
                &CompileLimits::default(),
                DiagnosticCode::InvalidQuantifier,
                quantifier_start..source.find('}').expect("fixture closes") + 1,
            );
        }
    }

    #[test]
    fn enforces_all_limits_owned_by_cst_lowering() {
        let source = "p := a.";
        let limits = CompileLimits::default()
            .tightened(|limits| limits.max_source_bytes = source.len() - 1)
            .expect("source limit is tightened");
        assert_diagnostic(
            source,
            &limits,
            DiagnosticCode::LimitExceeded,
            0..source.len(),
        );

        let source = "p := a.\nq := b.";
        let limits = CompileLimits::default()
            .tightened(|limits| limits.max_definitions = 1)
            .expect("definition limit is tightened");
        let second = source
            .find("q :=")
            .expect("fixture has a second definition");
        assert_diagnostic(
            source,
            &limits,
            DiagnosticCode::LimitExceeded,
            second..source.len(),
        );

        let source = "p := a b.";
        let limits = CompileLimits::default()
            .tightened(|limits| limits.max_expression_nodes = 2)
            .expect("expression limit is tightened");
        let b = source.find("b.").expect("fixture has a second token");
        assert_diagnostic(source, &limits, DiagnosticCode::LimitExceeded, b..b + 1);

        let source = "p := ((a)).";
        let limits = CompileLimits::default()
            .tightened(|limits| limits.max_nesting_depth = 2)
            .expect("depth limit is tightened");
        let a = source.find('a').expect("fixture has a nested token");
        assert_diagnostic(source, &limits, DiagnosticCode::LimitExceeded, a..a + 1);

        let source = "category: \"test\"\np := a.\n";
        let limits = CompileLimits::default()
            .tightened(|limits| limits.max_metadata_bytes = 0)
            .expect("metadata limit is tightened");
        let line_start = source.find("category").expect("fixture has metadata");
        // `line_start` is a byte offset from `find` of an ASCII substring, so
        // it is a char boundary.
        #[expect(
            clippy::string_slice,
            reason = "`line_start` is an ASCII-substring `find` offset, a char boundary"
        )]
        let line_end = source[line_start..].find('\n').expect("line ends") + line_start;
        assert_diagnostic(
            source,
            &limits,
            DiagnosticCode::LimitExceeded,
            line_start..line_end,
        );

        let source = "p := [text~/a+/].";
        let limits = CompileLimits::default()
            .tightened(|limits| limits.max_regex_bytes = 1)
            .expect("regex limit is tightened");
        let regex_start = source.find('/').expect("fixture has regex");
        let regex_end = source.rfind('/').expect("fixture closes regex") + 1;
        assert_diagnostic(
            source,
            &limits,
            DiagnosticCode::LimitExceeded,
            regex_start..regex_end,
        );
    }

    #[test]
    fn deep_malformed_nesting_hits_budgets_before_recursive_recovery() {
        let nesting = 128;
        let source = format!(
            "p := {}[text=]{}.",
            "(".repeat(nesting),
            ")".repeat(nesting)
        );
        let limits = [
            CompileLimits::default()
                .tightened(|limits| limits.max_nesting_depth = 8)
                .expect("depth limit is tightened"),
            CompileLimits::default()
                .tightened(|limits| limits.max_expression_nodes = 8)
                .expect("node limit is tightened"),
        ];

        for limits in limits {
            let result = std::panic::catch_unwind(|| {
                lower_document(&PatternSource::named("deep.lx", &source), &limits)
            });
            let error = result
                .expect("deep malformed lowering must not panic")
                .expect_err("deep malformed lowering must be rejected");

            assert_eq!(error.diagnostics[0].code(), DiagnosticCode::LimitExceeded);
        }
    }

    #[test]
    fn lower_definition_accepts_exactly_one_definition() {
        let source = PatternSource::named("one.lx", "p := a.");
        assert_eq!(
            lower_definition(&source, &CompileLimits::default())
                .expect("one definition is accepted")
                .name
                .as_str(),
            "p"
        );

        let source = PatternSource::named("two.lx", "p := a.\nq := b.");
        let error = lower_definition(&source, &CompileLimits::default())
            .expect_err("multiple definitions are rejected");
        assert_eq!(error.diagnostics[0].code(), DiagnosticCode::Syntax);
    }

    #[test]
    fn lowers_real_document_to_exact_definition_catalog_and_metadata() {
        let source = include_str!("../../../../patterns/sfe.lx");
        let program = lower_document(
            &PatternSource::named("patterns/sfe.lx", source),
            &CompileLimits::default(),
        )
        .expect("the coordinated real document must lower completely");

        assert_eq!(
            program
                .definitions
                .iter()
                .map(|definition| definition.name.as_str())
                .collect::<Vec<_>>(),
            [
                "動詞終止形",
                "形容詞終止形",
                "助動詞終止形",
                "用言終止形",
                "終助詞",
                "動詞連用形",
                "形容詞連用形",
                "形容詞語幹",
                "用言連用形",
                "形状詞",
                "形容動詞",
                "ARU",
                "KAKO",
                "NAI",
                "だ",
                "である",
                "たい",
                "なさい",
                "のに",
                "必要がある",
                "恐れがある",
                "だろう",
                "であろう",
                "う",
                "よう",
                "ようだ",
                "かもしれない",
            ]
        );

        let definition = |name: &str| {
            program
                .definitions
                .iter()
                .find(|definition| definition.name == name)
                .expect("asserted definition exists")
        };
        // The retained catalog puts its executable claims on an emitting
        // pattern, rather than on a helper fragment.
        assert_eq!(
            definition("必要がある").metadata["examples"].value,
            json!(["必要があった", "必要がある"])
        );
        assert_eq!(
            definition("必要がある").metadata["counterexamples"].value,
            json!(["必要があろう", "必要があれば"])
        );
        assert!(!definition("用言連用形").metadata.contains_key("examples"));
        assert_eq!(
            definition("恐れがある").metadata["category"].value,
            "がある"
        );
        assert_eq!(definition("だろう").metadata["subcategory"].value, "う");
        assert_eq!(definition("よう").metadata["subcategory"].value, "よう");
        assert_eq!(
            definition("かもしれない").metadata["category"].value,
            "かもしれない"
        );
        assert_eq!(
            definition("う").expr().expect("pattern"),
            &ParsedExpr::Token(conjunction(vec![
                resolved_attr(AttrValue {
                    attr: TokenAttributeIndex::CForm,
                    value: AttrValueType::Single("意志推量形".into()),
                    complement: false,
                }),
                resolved_attr(AttrValue {
                    attr: TokenAttributeIndex::Orth,
                    value: AttrValueType::Single("う".into()),
                    complement: false,
                }),
            ]))
        );

        let mut attributes = BTreeSet::new();
        for definition in &program.definitions {
            collect_attributes(definition.expr().expect("pattern"), &mut attributes);
        }
        assert_eq!(
            attributes,
            [
                TokenAttributeIndex::CForm,
                TokenAttributeIndex::Lemma,
                TokenAttributeIndex::Orth,
                TokenAttributeIndex::Pos,
                TokenAttributeIndex::Pos1,
                TokenAttributeIndex::Pos2,
                TokenAttributeIndex::Pos3,
            ]
            .into_iter()
            .map(AttributeKey::from)
            .collect()
        );
    }

    fn collect_attributes(expr: &ParsedExpr, attributes: &mut BTreeSet<AttributeKey>) {
        match expr {
            ParsedExpr::Token(test) => collect_test_attributes(test, attributes),
            ParsedExpr::Sequence(exprs) | ParsedExpr::Alternation(exprs) => {
                for expr in exprs {
                    collect_attributes(expr, attributes);
                }
            }
            ParsedExpr::Intersection { operands, .. } => {
                for operand in operands {
                    collect_attributes(operand, attributes);
                }
            }
            ParsedExpr::AnyOrder { operands, .. } => {
                for operand in operands {
                    collect_attributes(operand, attributes);
                }
            }
            ParsedExpr::Repeat { expr, .. }
            | ParsedExpr::Group(expr)
            | ParsedExpr::NamedGroup { expr, .. } => collect_attributes(expr, attributes),
            ParsedExpr::Lookaround(lookaround) => collect_attributes(&lookaround.body, attributes),
            ParsedExpr::Assertion(assertion) => {
                if let crate::fsm::PositionAssertion::PreviousToken(predicate) = assertion {
                    attributes.extend(predicate.iter().map(|attribute| attribute.attr.clone()));
                }
            }
            ParsedExpr::SameAs { .. } | ParsedExpr::Reference { .. } => {}
        }
    }

    /// Compile `source`, expect failure, return the first diagnostic's
    /// (message, byte_range, line, column).
    fn first_recovery_diagnostic(source: &str) -> (String, std::ops::Range<usize>, usize, usize) {
        let error = lower_document(
            &PatternSource::named("fixture.lx", source),
            &CompileLimits::default(),
        )
        .expect_err("fixture must fail to lower");
        let d = &error.diagnostics[0];
        assert_eq!(d.code(), DiagnosticCode::Syntax);
        (
            d.message.clone(),
            d.location().byte_range(),
            d.location().line(),
            d.location().column(),
        )
    }

    #[test]
    fn unterminated_definition_points_at_the_dot_not_the_next_definition() {
        // Byte 13 is the end of `[text=x]`. The `:=` tree-sitter actually errors
        // on is on line 2; before this fix the diagnostic pointed there and said
        // "invalid lingrep patterns syntax".
        assert_eq!(
            first_recovery_diagnostic("a := [text=x]\nb := [text=y].\n"),
            ("missing `.`".to_string(), 13..13, 1, 14)
        );
    }

    #[test]
    fn unterminated_definition_keeps_a_trailing_reference_as_the_final_term() {
        // `ref` is a legitimate pattern reference ending definition `a`. Only
        // `b` is the absorbed header, so the terminator belongs after `ref`.
        assert_eq!(
            first_recovery_diagnostic("a := ref\nb := [text=y].\n"),
            ("missing `.`".to_string(), 8..8, 1, 9)
        );
    }

    #[test]
    fn unterminated_definition_consumes_a_fragment_or_pattern_keyword_header() {
        // Both kind keywords, since the recognizer matches on the literal text.
        // `pattern a := ` is 13 bytes and `fragment a := ` is 14, so `[text=x]`
        // ends at 21 and 22 respectively.
        for (keyword, offset) in [("pattern", 21_usize), ("fragment", 22_usize)] {
            let source = format!("{keyword} a := [text=x]\n{keyword} b := [text=y].\n");
            assert_eq!(
                first_recovery_diagnostic(&source),
                ("missing `.`".to_string(), offset..offset, 1, offset + 1),
                "keyword: {keyword}"
            );
        }
    }

    #[test]
    fn unterminated_definition_places_the_dot_before_a_trailing_comment() {
        // `#...` runs to end of line, so a `.` suggested after the comment would
        // be swallowed by it.
        assert_eq!(
            first_recovery_diagnostic("a := [text=x] # note\nb := [text=y].\n"),
            ("missing `.`".to_string(), 13..13, 1, 14)
        );
    }

    #[test]
    fn a_nested_group_never_reports_a_missing_terminator() {
        // Same local shape, but inside a parenthesized_pattern. A definition
        // cannot begin there, so `missing `.`` would be a confident lie.
        let (message, ..) = first_recovery_diagnostic("a := ([text=x] b := [text=y]).\n");
        assert_eq!(message, "invalid lingrep patterns syntax");
    }

    #[test]
    fn an_alternation_branch_never_reports_a_missing_terminator() {
        let (message, ..) = first_recovery_diagnostic("a := [text=x] | [text=z]\nb := [text=y].\n");
        assert_eq!(message, "invalid lingrep patterns syntax");
    }

    #[test]
    fn unterminated_definition_falls_back_when_no_term_precedes_the_reference() {
        // `a := b := [text=x].` is all on one line and has no term before the
        // absorbed `b`. There is nowhere honest to put the caret.
        let (message, ..) = first_recovery_diagnostic("a := b := [text=x].\n");
        assert_eq!(message, "invalid lingrep patterns syntax");
    }

    #[test]
    fn a_genuine_reference_before_an_unrelated_error_is_not_called_a_missing_dot() {
        // The ERROR here comes from the empty attribute value, not from a `:=`.
        let (message, ..) = first_recovery_diagnostic("a := ref [text=].\n");
        assert_eq!(message, "invalid lingrep patterns syntax");
    }

    /// Every attribute one token test names, wherever `&`, `|` and `!` put it.
    ///
    /// A `Reference` contributes nothing: the test it names is a declaration of
    /// its own, and this walker is over one definition's expression.
    fn collect_test_attributes(test: &ParsedTest, attributes: &mut BTreeSet<AttributeKey>) {
        match test {
            ParsedTest::Attr(attribute) => {
                attributes.insert(attribute.attr.clone());
            }
            ParsedTest::Reference { .. } => {}
            ParsedTest::And(operands) | ParsedTest::Or(operands) => {
                for operand in operands {
                    collect_test_attributes(operand, attributes);
                }
            }
            ParsedTest::Not(inner) => collect_test_attributes(inner, attributes),
        }
    }

    /// The single literal value of a one-token, one-attribute definition.
    ///
    /// Panics rather than returning an Option: every fixture below is written
    /// to have exactly this shape, so any other shape is a broken test, not a
    /// case to handle.
    fn single_literal(source: &str) -> String {
        let program = lower(source);
        let [definition] = &program.definitions[..] else {
            panic!("fixture must have exactly one definition");
        };
        let ParsedExpr::Token(ParsedTest::Attr(attribute)) = definition.expr().expect("pattern")
        else {
            panic!("fixture must lower to a single token predicate");
        };
        let ParsedAttrValue::Resolved(AttrValueType::Single(value)) = &attribute.value else {
            panic!("fixture must have a single literal value");
        };
        value.to_string()
    }

    fn lower_error_code(source: &str) -> DiagnosticCode {
        lower_document(
            &PatternSource::named("fixture.lx", source),
            &CompileLimits::default(),
        )
        .expect_err("fixture must fail to lower")
        .diagnostics[0]
            .code()
    }

    #[test]
    fn a_unicode_escape_denotes_the_character_it_names() {
        // U+3000 is invisible in a diff, in a review and in a terminal. A
        // literal that depends on one is a literal nobody can proofread, and one
        // was in fact silently dropped during an edit of this very catalog. The
        // escape gives the same value a spelling that survives being looked at.
        assert_eq!(single_literal("p := [text=\\u3000]."), "\u{3000}");
        assert_eq!(single_literal("p := [text=\\u3042]."), "あ");
        // Mixed with ordinary text, and more than once.
        assert_eq!(
            single_literal("p := [text=a\\u3000b\\u3000c]."),
            "a\u{3000}b\u{3000}c"
        );
    }

    #[test]
    fn a_backslash_escape_denotes_exactly_one_backslash() {
        assert_eq!(single_literal("p := [text=\\\\]."), "\\");
        assert_eq!(single_literal("p := [text=a\\\\b]."), "a\\b");
    }

    #[test]
    fn a_literal_without_a_backslash_is_unchanged() {
        // The fast path. Every existing catalog goes through it, so if this
        // moves, every fingerprint in the repository moves with it.
        assert_eq!(single_literal("p := [text=を]."), "を");
        assert_eq!(single_literal("p := [text=\u{3000}]."), "\u{3000}");
    }

    #[test]
    fn exactly_four_hex_digits_are_consumed_and_the_rest_stays_literal() {
        // `　0` is U+3000 followed by the character `0`, not U+30000. Four
        // digits is the whole rule; there is no longest-match and no brace
        // form. Pinned because the alternative reading is plausible enough
        // that someone will eventually assume it.
        assert_eq!(single_literal("p := [text=\\u30000]."), "\u{3000}0");
    }

    #[test]
    fn a_malformed_unicode_escape_is_an_error() {
        for (label, source) in [
            ("three digits then the end", "p := [text=\\u300]."),
            ("two digits then the end", "p := [text=\\u30]."),
            ("no digits at all", "p := [text=\\u]."),
            ("non-hexadecimal digits", "p := [text=\\uZZZZ]."),
            ("three hex digits then a non-digit", "p := [text=\\u30ag]."),
        ] {
            assert_eq!(
                lower_error_code(source),
                DiagnosticCode::Syntax,
                "case: {label}"
            );
        }
    }

    #[test]
    fn an_escape_naming_a_surrogate_is_an_error() {
        // D800-DFFF are not Unicode scalar values; `char::from_u32` rejects
        // them and so must this, rather than silently substituting U+FFFD.
        assert_eq!(
            lower_error_code("p := [text=\\uD800]."),
            DiagnosticCode::Syntax
        );
        assert_eq!(
            lower_error_code("p := [text=\\uDFFF]."),
            DiagnosticCode::Syntax
        );
    }

    #[test]
    fn an_unrecognised_escape_is_an_error_not_a_literal_backslash() {
        // Rejecting the unknown case keeps every other `\x` spelling free for a
        // later addition. Treating it as a literal backslash would make those
        // spellings permanently ambiguous.
        for source in [
            "p := [text=\\q].",
            "p := [text=\\n].",
            "p := [text=\\U0001F600].",
        ] {
            assert_eq!(lower_error_code(source), DiagnosticCode::Syntax, "{source}");
        }
    }

    #[test]
    fn a_trailing_backslash_is_an_error() {
        assert_eq!(lower_error_code("p := [text=a\\]."), DiagnosticCode::Syntax);
    }

    #[test]
    fn an_escape_inside_a_regex_value_is_left_to_the_regex_engine() {
        // `\d` is regex syntax and must reach the regex compiler untouched. If
        // the decoder ran here it would reject `\d` as an unknown escape.
        lower("p := [text~/\\d+/].");
        lower("p := [text~/\\u3000/].");
    }
}
