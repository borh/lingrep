//! Reading declared result policy off a compiled catalog.
//!
//! The vocabulary and the engine live in [`crate::fsm::policy`]; this is the
//! compiler half, which needs three things that module deliberately does not
//! depend on: every definition's name, its visibility, and its source span.
//!
//! Extraction runs on the **parsed** program, before binding. Every input it
//! needs — names, visibility, metadata, spans — is already present there, and
//! running before `bind` is what lets the emitters synthesized for
//! a statement's fragment operands join the program in time to be bound with it,
//! rather than forcing a second bind over an augmented program.
//!
//! Two functions here are the exception, and are named so the reader does not
//! have to find them: [`resolve_anchors`] and [`anchor_set_digest`]
//! run on the **bound** program. What they ask — whether an anchor's `exclude`
//! fragment can match the empty span, whether it names a capture, and what its
//! transitive expansion is — is a question about a resolved body, and the parsed
//! program cannot answer it. They are here rather than in `bind` because they
//! are about a *declaration*; binding does not know anchors exist.
//!
//! An unresolved policy reference reuses [`DiagnosticCode::UndefinedReference`]
//! rather than inventing a parallel vocabulary: a policy operand naming `X`
//! and a body-level `@X` are the same mistake, and a consumer filtering that
//! code must see both.
//!
//! Spans follow provenance. A defect in a metadata *value* is reported at the
//! key that carried it, which is the line the author edits; a defect in the
//! definition — a fragment declaring publication policy at all — keeps the
//! definition's span.

use std::collections::{BTreeMap, BTreeSet};

use compact_str::CompactString;
use serde_json::Value;

use super::bind::{definition_nullability, location};
use super::diagnostic::RelatedDiagnosticLocation;
use super::guard::compile_guard;
use super::model::{
    BoundDefinition, BoundExpr, BoundProgram, DefinitionId, DefinitionVisibility, GroupOperator,
    LookDirection, LookPolarity, Metadata, ParsedDefinition, ParsedExpr, ParsedGroupDecl,
    ParsedProgram, SourceRegistry, SourceSpan, TokenPredicate,
};
use super::{Diagnostic, DiagnosticCode, DiagnosticSeverity};
use crate::fsm::policy::{
    ANCHOR_KEY, AnchorSetDigest, CatalogPolicy, CatalogPolicyBuilder, CompiledGuard,
    DICTIONARY_KEY, HIDDEN_PREFIX, MAX_NESTING_KEY, NestingClaim, NestingEntry, POLICY_KEYS,
    PatternPolicy, RENDERER_REGISTRY, SubstituteTarget, TagSource, TransformOp, TransformStep,
    is_hidden, suggest_policy_key,
};
use crate::fsm::policy_program::{BindError, BindErrorKind, Op, Operand};
use crate::fsm::types::{AttrValueType, PositionAssertion};
use crate::token_match::AttributeKey;

/// The operation names `transform:` accepts, for the diagnostics that list
/// them.
const TRANSFORM_OPS: &[&str] = &["drop_edge", "truncate_before", "substitute", "join"];

/// The closed field set for `anchor:` mappings.
const ANCHOR_FIELDS: &[&str] = &["name", "exclude"];

/// Domain-separation prefix for [`anchor_set_digest`].
///
/// Prevents collisions with other unkeyed BLAKE3 uses in the workspace. The
/// version identifies the encoded walk if the digest becomes persistent.
const DIGEST_DOMAIN: &[u8] = b"lingrep.anchor-set.v1";

/// Renders a closed set of names for a diagnostic.
///
/// Every message refusing a name lists the alternatives, because a span says
/// where the mistake is and not what would have been right, and there is
/// nowhere else an author learns what exists.
pub(super) fn quoted_list(names: &[&str]) -> String {
    names
        .iter()
        .map(|name| format!("`{name}`"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The explicit key-component names, rendered for a diagnostic.
/// Metadata keys that are documented prose, not policy, and must never be
/// measured by the typo detector.
///
/// Without this a correct `counterexamples:` would be reported as a near-miss
/// of some policy key the moment one is added within an edit of it. The list is
/// every other key the engine recognizes: the claim keys (`CLAIM_KEYS`) and the
/// three catalog-level ones. Any key outside it is free-form researcher
/// annotation, which the detector already leaves alone unless it is one edit
/// from a recognized key.
///
/// A key added to [`suggest_policy_key`]'s search must be added here too, or
/// its own correct spelling reports as a typo of itself.
const NON_POLICY_KEYS: &[&str] = &[
    "examples",
    "counterexamples",
    DICTIONARY_KEY,
    ANCHOR_KEY,
    MAX_NESTING_KEY,
];

const RETIRED_KEYS: &[(&str, &str)] = &[
    ("trim_suffix", "use `detach` with `relation: at_end`"),
    ("trim_prefix", "use `detach` with `relation: at_start`"),
    (
        "priority_group",
        "use `groups:` for membership and a `resolve` statement for adjudication",
    ),
    (
        "longest_in_group",
        "choose a group-level `resolve` rank after reviewing every member",
    ),
    ("suppress_within", "use `exclude` with `relation: within`"),
    ("terminal", "use an `admit` statement"),
];

/// Reads every declared policy decision, reporting what cannot be honored.
///
/// Returned diagnostics carry their own severity: an unusable value or an
/// unresolved reference is an error (the catalog claims something the engine
/// cannot do), a suspected typo or a policy on a non-emitting definition is a
/// warning (the catalog compiles and matches correctly; what is broken is the
/// declaration). Splitting them is the caller's job.
pub(crate) fn extract_policy(
    program: &ParsedProgram,
) -> (CatalogPolicy, Vec<Vec<CompactString>>, Vec<Diagnostic>) {
    let mut policy = CatalogPolicyBuilder::default();
    let mut diagnostics = Vec::new();
    // One entry per authored definition, in definition order, so the slice
    // index is the `DefId`. It is returned rather than turned into a
    // `GroupIndex` here because synthesis has not run yet: it appends
    // definitions, and the index has to span the complete list.
    let mut authored_groups: Vec<Vec<CompactString>> =
        Vec::with_capacity(program.definitions.len());

    let declaration_spans = read_anchor_declarations(program, &mut policy, &mut diagnostics);
    check_duplicate_group_declarations(program, &mut diagnostics);

    for definition in &program.definitions {
        let mut reader = PolicyReader {
            definition,
            diagnostics: &mut diagnostics,
            program,
            key_span: None,
        };
        let read = reader.read();
        let mut groups = read.groups;
        // Derived membership joins the literal list here, at the one site
        // membership is read, so everything downstream — evidence synthesis,
        // the `GroupIndex`, subject expansion — sees one union and cannot
        // disagree about it. Emitting definitions only: a fragment joins a
        // group by literal list, which is how a `%group` operand names one,
        // but a predicate derives the set a statement adjudicates, and a
        // fragment has no records to adjudicate.
        if definition.is_emitting() {
            for declaration in &program.groups {
                if derived_member(declaration, definition) && !groups.contains(&declaration.name) {
                    groups.push(declaration.name.clone());
                }
            }
        }
        // Definition order is `DefId` order, so this must push once per
        // definition whether or not either field is present.
        authored_groups.push(groups);
        if let Some(declared) = read.policy {
            policy.insert_entry(definition.name.clone(), declared);
        }
    }

    resolve_anchor_references(program, &policy, &declaration_spans, &mut diagnostics);

    (policy.build(), authored_groups, diagnostics)
}

/// Two `group` declarations with one name, reported at the second and naming
/// the first.
///
/// The check is over the accumulated catalog — the scope a `%name` resolves
/// against — for the reason the set and test tables check there: two documents
/// each legal on their own can still collide. Only group-versus-group is a
/// collision at all, because `%` is its own namespace: a group named like a
/// fragment, a set, or a test is legal.
fn check_duplicate_group_declarations(program: &ParsedProgram, diagnostics: &mut Vec<Diagnostic>) {
    let mut declared: BTreeMap<&str, SourceSpan> = BTreeMap::new();
    for declaration in &program.groups {
        match declared.entry(declaration.name.as_str()) {
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(declaration.span);
            }
            std::collections::btree_map::Entry::Occupied(entry) => {
                diagnostics.push(
                    diagnostic_at(
                        &program.sources,
                        declaration.span,
                        DiagnosticCode::DuplicateGroupDefinition,
                        format!(
                            "duplicate group declaration `{}` — one name, one predicate; \
                             keeping both would let two predicates contest one membership",
                            declaration.name
                        ),
                        DiagnosticSeverity::Error,
                    )
                    .with_related_location(RelatedDiagnosticLocation::new(
                        location(&program.sources, *entry.get()),
                        "first declaration is here",
                    )),
                );
            }
        }
    }
}

/// Whether a `group` declaration's predicate selects this definition.
///
/// Existential and two-valued over presence: every form requires the key to be
/// present and string-valued, and `!=`/`not in` then require the value to
/// differ. A definition lacking the key, or carrying a non-string value, is a
/// member of **no** form — the presence rule `resolve … partition:` applies to
/// its classes, not the `[…]` complement rule, because metadata that says
/// nothing about a key has declared nothing a predicate could hold of.
fn derived_member(declaration: &ParsedGroupDecl, definition: &ParsedDefinition) -> bool {
    let Some(entry) = definition.metadata.get(declaration.key.as_str()) else {
        return false;
    };
    let Value::String(value) = &entry.value else {
        return false;
    };
    let named = declaration.values.iter().any(|held| held == value);
    match declaration.operator {
        GroupOperator::Equals | GroupOperator::In => named,
        GroupOperator::NotEquals | GroupOperator::NotIn => !named,
    }
}

/// Checks every anchor reference against the declared anchors, and every
/// declared anchor against the references to it.
///
/// One kind of reference since `terminal:` retired: an `admit` statement. It is
/// checked here rather than with the other policy-program rules because an
/// anchor name is a string the parser already has — binding resolves
/// definitions and groups, and knows nothing about anchors. Leaving it out made
/// a catalog whose only reference was an `admit` report its anchor as
/// unreferenced *and* left the reference itself unchecked, so the runtime
/// `AnchorError::Unknown` that
/// [`AnchorPositions`](crate::fsm::policy::AnchorPositions) documents as
/// unreachable was reachable.
///
/// Both directions are pure questions over [`CatalogPolicy::anchors`] and the
/// parsed program, complete the moment the loop above ends. Neither needs a
/// bound body, so neither waits for binding — the same reason `compile_program`
/// reads policy before `bind` at all: an `admit` that names nothing should not
/// have to queue behind an unrelated body error to be seen.
fn resolve_anchor_references(
    program: &ParsedProgram,
    policy: &CatalogPolicyBuilder,
    declaration_spans: &BTreeMap<CompactString, SourceSpan>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let mut referenced: BTreeSet<&str> = BTreeSet::new();

    for op in program.policy.iter().flat_map(|declared| &declared.ops) {
        let Op::Admit { anchor, span, .. } = op else {
            continue;
        };
        // The reserved anchor is neither an unresolved reference nor a use of
        // a declared one: it is compiler-owned, so it resolves without a
        // declaration and cannot be what keeps a declared anchor referenced.
        if anchor == crate::fsm::policy::SENTENCE_END_ANCHOR {
            continue;
        }
        if policy.anchors.contains_key(anchor) {
            referenced.insert(anchor.as_str());
            continue;
        }
        diagnostics.push(diagnostic_at(
            &program.sources,
            span.0,
            DiagnosticCode::InvalidPolicyValue,
            format!(
                "this `admit` measures against the anchor `{anchor}`, which this catalog never \
                 declares; add `anchor: {{name: {anchor}, exclude: \"…\"}}` to a metadata block, \
                 naming the fragment that recognizes the trailing material it strips"
            ),
            DiagnosticSeverity::Error,
        ));
    }

    for anchor in policy.anchors.keys() {
        if referenced.contains(anchor.as_str()) {
            continue;
        }
        // The *declaring* block's span, not the `exclude` fragment's: two
        // anchors may share one `exclude`, and pointing at the fragment would
        // report an unreferenced anchor against a definition that is used.
        let span = *declaration_spans
            .get(anchor)
            .expect("a recorded anchor and its declaring span are written together");
        diagnostics.push(diagnostic_at(
            &program.sources,
            span,
            DiagnosticCode::UnreferencedAnchor,
            format!(
                "anchor `{anchor}` is declared but no `admit` statement measures against it, so \
                 it cannot affect any record; write \
                 `admit @pattern terminal: {{exact: true, anchor: {anchor}}}.` in a `policy` \
                 block, or delete the declaration"
            ),
            DiagnosticSeverity::Warning,
        ));
    }
}

/// Reads the catalog's declared dictionary binding.
///
/// A catalog's predicates are written against one dictionary's tagset, and
/// nothing else in the system records which. `token_schema_identity` digests
/// the attribute *names*, which every dictionary shares, and
/// `dictionary_identity` describes the run rather than the catalog — so a
/// catalog authored for one dictionary and run against another agrees on every
/// identity while meaning something different. Measured in
/// `docs/results/2026-08-04-catalog-tagset-binding/`.
///
/// The binding is catalog-level, but metadata attaches to definitions, so it is
/// read as the agreed value across every definition that declares it.
///
/// Disagreement is fatal, unlike almost everything else read here. A catalog
/// bound to two dictionaries has no answer to give the run-side check, and the
/// run-side check is a notice — so if this were also a notice, the condition
/// would have nowhere to be reported as the defect it is.
pub(crate) fn extract_dictionary(program: &ParsedProgram) -> (Vec<CompactString>, Vec<Diagnostic>) {
    let mut diagnostics = Vec::new();
    let mut bound: Option<(Vec<CompactString>, &ParsedDefinition)> = None;

    for definition in &program.definitions {
        let Some(value) = definition
            .metadata
            .get(DICTIONARY_KEY)
            .map(|entry| &entry.value)
        else {
            continue;
        };
        let Some(names) = read_dictionary_names(value) else {
            diagnostics.push(dictionary_error(
                program,
                definition,
                format!(
                    "`{DICTIONARY_KEY}:` on `{}` must be a dictionary name or a non-empty list \
                     of them; found {value}",
                    definition.name
                ),
            ));
            continue;
        };
        match &bound {
            Some((existing, first)) if *existing != names => {
                diagnostics.push(dictionary_error(
                    program,
                    definition,
                    format!(
                        "catalog declares two different dictionary bindings: {} on `{}` and {} \
                         on `{}` — write one list naming every compatible dictionary rather \
                         than disagreeing declarations",
                        render_names(existing),
                        first.name,
                        render_names(&names),
                        definition.name
                    ),
                ));
            }
            Some(_) => {}
            None => bound = Some((names, definition)),
        }
    }

    (
        bound.map(|(names, _)| names).unwrap_or_default(),
        diagnostics,
    )
}

/// Accepts a bare name or a list of them, normalized to a sorted, deduplicated
/// set so that two spellings of the same claim compare equal.
fn read_dictionary_names(value: &Value) -> Option<Vec<CompactString>> {
    let mut names: Vec<CompactString> = match value {
        Value::String(name) => vec![CompactString::from(name.trim())],
        Value::Array(entries) => entries
            .iter()
            .map(|entry| {
                entry
                    .as_str()
                    .map(|name| CompactString::from(name.trim()))
                    .filter(|name| !name.is_empty())
            })
            .collect::<Option<Vec<_>>>()?,
        _ => return None,
    };
    if names.iter().any(CompactString::is_empty) || names.is_empty() {
        return None;
    }
    names.sort_unstable();
    names.dedup();
    Some(names)
}

fn render_names(names: &[CompactString]) -> String {
    let rendered = names
        .iter()
        .map(|name| format!("`{name}`"))
        .collect::<Vec<_>>()
        .join(", ");
    if names.len() == 1 {
        rendered
    } else {
        format!("[{rendered}]")
    }
}

fn dictionary_error(
    program: &ParsedProgram,
    definition: &ParsedDefinition,
    message: String,
) -> Diagnostic {
    let span = key_span(definition, DICTIONARY_KEY);
    let (line, column) = program.sources.line_column(span);
    let range = span.byte_range();
    Diagnostic::error_with_source_label(
        DiagnosticCode::ConflictingDictionary,
        program.sources.source_label(span.source()),
        usize::try_from(range.start).expect("source offsets originated as usize")
            ..usize::try_from(range.end).expect("source offsets originated as usize"),
        message,
        line,
        column,
    )
}

/// Collects every catalog-level `anchor:` declaration, returning the span of
/// the block that declared each.
///
/// An anchor is a property of the *catalog* rather than of the definition whose
/// metadata block happens to carry it, which is why this is one pass over every
/// block instead of part of [`PolicyReader`]. The recommended home is the
/// fragment block holding the `exclude` fragment, exactly as for `dictionary:`.
///
/// A metadata block attaches its keys to *every* definition it wraps, so one
/// `anchor:` line on a two-fragment block arrives here twice. An `anchor:` value
/// already read is therefore skipped outright: replication says nothing new, and
/// reading it again would report a malformed declaration once per wrapped
/// definition, each copy naming a different definition as the culprit.
///
/// What is refused is disagreement — the same name excluding two different
/// fragments — and a name repeated within one `anchor:` value, which no amount
/// of block replication can produce.
///
/// Only what the parsed program can decide is decided here: the mapping's shape,
/// its fields, whether the name is unique, and whether `exclude` names a
/// fragment. Whether that fragment's *body* is usable as an anchor recognizer
/// needs binding, and is checked in [`resolve_anchors`].
fn read_anchor_declarations(
    program: &ParsedProgram,
    policy: &mut CatalogPolicyBuilder,
    diagnostics: &mut Vec<Diagnostic>,
) -> BTreeMap<CompactString, SourceSpan> {
    let mut fragments: BTreeSet<&str> = BTreeSet::new();
    let mut patterns: BTreeSet<&str> = BTreeSet::new();
    for definition in &program.definitions {
        if definition.visibility() == Some(DefinitionVisibility::Fragment) {
            fragments.insert(definition.name.as_str());
        } else if definition.is_ordinary_pattern() {
            patterns.insert(definition.name.as_str());
        }
    }

    let mut declaration_spans = BTreeMap::new();
    let mut already_read: Vec<&Value> = Vec::new();
    for definition in &program.definitions {
        let Some(value) = definition
            .metadata
            .get(ANCHOR_KEY)
            .map(|entry| &entry.value)
        else {
            continue;
        };
        if already_read.contains(&value) {
            continue;
        }
        already_read.push(value);
        let declarations: Vec<&Value> = match value {
            Value::Object(_) => vec![value],
            Value::Array(items) => items.iter().collect(),
            _ => {
                diagnostics.push(anchor_error(
                    program,
                    definition,
                    format!(
                        "`{ANCHOR_KEY}:` on `{}` is {value}; an anchor is declared as a mapping \
                         `{{name: \"…\", exclude: \"…\"}}`, or as a flow list of them",
                        definition.name
                    ),
                ));
                continue;
            }
        };
        let mut declared_here: BTreeSet<CompactString> = BTreeSet::new();
        for declaration in declarations {
            read_anchor_declaration(AnchorDeclarationSite {
                program,
                definition,
                declaration,
                fragments: &fragments,
                patterns: &patterns,
                declared_here: &mut declared_here,
                declaration_spans: &mut declaration_spans,
                policy,
                diagnostics,
            });
        }
    }
    declaration_spans
}

/// Everything one `{name: …, exclude: …}` mapping is read against.
///
/// A struct rather than nine parameters, which is what the argument list had
/// grown to.
struct AnchorDeclarationSite<'a> {
    program: &'a ParsedProgram,
    definition: &'a ParsedDefinition,
    declaration: &'a Value,
    fragments: &'a BTreeSet<&'a str>,
    patterns: &'a BTreeSet<&'a str>,
    /// The names this definition's own `anchor:` value has already declared.
    declared_here: &'a mut BTreeSet<CompactString>,
    declaration_spans: &'a mut BTreeMap<CompactString, SourceSpan>,
    policy: &'a mut CatalogPolicyBuilder,
    diagnostics: &'a mut Vec<Diagnostic>,
}

fn read_anchor_declaration(site: AnchorDeclarationSite<'_>) {
    let AnchorDeclarationSite {
        program,
        definition,
        declaration,
        fragments,
        patterns,
        declared_here,
        declaration_spans,
        policy,
        diagnostics,
    } = site;

    let Some(map) = declaration.as_object() else {
        diagnostics.push(anchor_error(
            program,
            definition,
            format!(
                "`{ANCHOR_KEY}:` on `{}` has the entry {declaration}; each entry is a mapping \
                 `{{name: \"…\", exclude: \"…\"}}`",
                definition.name
            ),
        ));
        return;
    };
    for field in map.keys() {
        if !ANCHOR_FIELDS.contains(&field.as_str()) {
            diagnostics.push(anchor_error(
                program,
                definition,
                format!(
                    "`{ANCHOR_KEY}:` on `{}` carries the field `{field}`, which an anchor \
                     declaration does not have; its fields are {} — delete it",
                    definition.name,
                    quoted_list(ANCHOR_FIELDS)
                ),
            ));
            return;
        }
    }

    let (Some(Value::String(name)), Some(Value::String(exclude))) =
        (map.get("name"), map.get("exclude"))
    else {
        diagnostics.push(anchor_error(
            program,
            definition,
            format!(
                "`{ANCHOR_KEY}:` on `{}` declares {declaration}; an anchor needs both a `name` \
                 to be referred to by and an `exclude` fragment to strip, each a string",
                definition.name
            ),
        ));
        return;
    };
    // Trimmed for the same reason `terminal:`'s `anchor:` is: a padded spelling
    // must resolve against an unpadded one rather than be reported downstream as
    // an anchor nobody declared.
    let (name, exclude) = (name.trim(), exclude.trim());
    // Named individually: the condition knows which field is blank, so echoing
    // "`name` or `exclude`" back would make the author check both.
    for (field, value, names) in [
        ("name", name, "what a `terminal:` refers to the anchor by"),
        (
            "exclude",
            exclude,
            "the fragment recognizing the material the anchor strips",
        ),
    ] {
        if value.is_empty() {
            diagnostics.push(anchor_error(
                program,
                definition,
                format!(
                    "`{ANCHOR_KEY}:` on `{}` declares an anchor whose `{field}` is blank; it \
                     names {names}, so it cannot be empty",
                    definition.name
                ),
            ));
            return;
        }
    }

    if name == crate::fsm::policy::SENTENCE_END_ANCHOR {
        diagnostics.push(anchor_error(
            program,
            definition,
            format!(
                "`{ANCHOR_KEY}:` on `{}` declares an anchor named `{name}`, which is \
                 compiler-owned and always means the raw sentence end — rename this anchor, \
                 since a derived position and the raw end are different boundaries",
                definition.name
            ),
        ));
        return;
    }

    if !fragments.contains(exclude) {
        let complaint = if patterns.contains(exclude) {
            format!(
                "`{ANCHOR_KEY}:` on `{}` excludes `{exclude}`, which is a pattern; an anchor is \
                 a position rather than a record, so its recognizer never emits — write \
                 `fragment {exclude} :=` instead of `pattern {exclude} :=`, or point `exclude` \
                 at a fragment",
                definition.name
            )
        } else {
            format!(
                "`{ANCHOR_KEY}:` on `{}` excludes `{exclude}`, which this catalog does not \
                 define; declare the trailing material it strips as `fragment {exclude} := …`",
                definition.name
            )
        };
        diagnostics.push(anchor_error(program, definition, complaint));
        return;
    }

    // Whether the name repeats *within this one value* and whether it disagrees
    // with an existing declaration are independent questions, and the answer to
    // the second decides the message either way. Deciding on the first alone
    // told an author that a second entry excluding a different fragment "says
    // nothing the first does not" — advice that silently changes which fragment
    // the anchor strips.
    let repeated_here = !declared_here.insert(CompactString::new(name));
    match policy.anchors.get(name) {
        Some(previous) if previous == exclude => {
            if repeated_here {
                diagnostics.push(anchor_error(
                    program,
                    definition,
                    format!(
                        "anchor `{name}` is declared twice in one `{ANCHOR_KEY}:` value, \
                         excluding `{exclude}` both times; the second entry says nothing the \
                         first does not, so delete it"
                    ),
                ));
            }
            // Otherwise the same claim, replicated across declarations. Nothing
            // to record and nothing to complain about.
        }
        Some(previous) => diagnostics.push(anchor_error(
            program,
            definition,
            format!(
                "anchor `{name}` is declared twice, excluding `{previous}` once and `{exclude}` \
                 the other time; a `terminal:` naming it could mean either, so give the two \
                 different names or delete one"
            ),
        )),
        None => {
            policy
                .anchors
                .insert(CompactString::new(name), CompactString::new(exclude));
            declaration_spans.insert(CompactString::new(name), key_span(definition, ANCHOR_KEY));
        }
    }
}

/// Reports at the `anchor:` key, which is the line an author edits.
///
/// Falls back to the definition only if the key is somehow absent, which
/// cannot happen on the paths that call this — every one of them reached here
/// by reading that key's value.
fn anchor_error(
    program: &ParsedProgram,
    definition: &ParsedDefinition,
    message: String,
) -> Diagnostic {
    diagnostic_at(
        &program.sources,
        key_span(definition, ANCHOR_KEY),
        DiagnosticCode::InvalidAnchorDeclaration,
        message,
        DiagnosticSeverity::Error,
    )
}

/// The span of one metadata key on a definition, or the definition's own.
fn key_span(definition: &ParsedDefinition, key: &str) -> SourceSpan {
    definition
        .metadata
        .get(key)
        .map_or(definition.span, |entry| entry.span)
}

/// A diagnostic pointing at a definition-level span.
///
/// Shared by the anchor readers on both sides of binding, which have a
/// [`SourceRegistry`] and a span but no [`PolicyReader`] to borrow the
/// definition from.
/// One diagnostic anchored at a [`SourceSpan`], with its line and column.
///
/// `pub(super)` because `policy_program` reports against the same spans and a
/// second construction path would let the two drift on how a position is
/// resolved.
pub(super) fn diagnostic_at(
    sources: &SourceRegistry,
    span: SourceSpan,
    code: DiagnosticCode,
    message: String,
    severity: DiagnosticSeverity,
) -> Diagnostic {
    let (line, column) = sources.line_column(span);
    let range = span.byte_range();
    let label = sources.source_label(span.source());
    let range = usize::try_from(range.start).expect("source offsets originated as usize")
        ..usize::try_from(range.end).expect("source offsets originated as usize");
    match severity {
        DiagnosticSeverity::Error => {
            Diagnostic::error_with_source_label(code, label, range, message, line, column)
        }
        DiagnosticSeverity::Warning => {
            Diagnostic::warning_with_source_label(code, label, range, message, line, column)
        }
    }
}

/// One definition's metadata, being read into a [`PatternPolicy`].
struct PolicyReader<'a> {
    definition: &'a ParsedDefinition,
    diagnostics: &'a mut Vec<Diagnostic>,
    program: &'a ParsedProgram,
    /// The key currently being read, if any.
    ///
    /// Every diagnostic about a metadata *value* is about the key that carried
    /// it, so it is reported there. `None` while the reader is judging the
    /// definition as a whole — a fragment declaring policy, say — where the
    /// definition's own span is the right place.
    key_span: Option<SourceSpan>,
}

/// What [`PolicyReader::read`] yields for one definition.
///
/// Two fields rather than one because they have different lifetimes in the
/// pipeline: publication policy is discarded on a fragment, while group
/// membership survives on every definition and is what `%name` resolves
/// against.
struct ReadPolicy {
    policy: Option<PatternPolicy>,
    groups: Vec<CompactString>,
}

impl PolicyReader<'_> {
    /// Returns the declared policy and this definition's group membership.
    ///
    /// `policy` is `None` when the definition declares none *or* declares one
    /// that can never apply. `group` survives either way: it is not a
    /// publication decision.
    fn read(&mut self) -> ReadPolicy {
        let mut declared = PatternPolicy::default();
        if let Some(key) = &self.definition.key {
            declared.key = key.projections.clone();
        }

        // Metadata is a `HashMap`, so iterate a sorted key list: diagnostic
        // order must not depend on hash seed.
        let mut keys: Vec<&CompactString> = self.definition.metadata.keys().collect();
        keys.sort_unstable();

        for key in keys {
            let entry = &self.definition.metadata[key];
            let value = &entry.value;
            self.key_span = Some(entry.span);
            if let Some((_, advice)) = RETIRED_KEYS
                .iter()
                .find(|(retired, _)| *retired == key.as_str())
            {
                self.error(
                    DiagnosticCode::RetiredPolicyKey,
                    format!(
                        "`{key}:` was retired; {advice} in a `policy` block. \
                         See `ling check --help` for the current policy syntax"
                    ),
                );
            } else if POLICY_KEYS.contains(&key.as_str()) {
                self.read_key(key, value, &mut declared);
            } else if !NON_POLICY_KEYS.contains(&key.as_str())
                && let Some(suggestion) = suggest_policy_key(key)
            {
                self.warn(
                    DiagnosticCode::SuspiciousPolicyKey,
                    format!(
                        "`{key}:` on `{}` is not a recognized result-policy key, but is one \
                         edit from `{suggestion}:` — rename it, or leave it if it is a \
                         deliberate annotation",
                        self.definition.name
                    ),
                );
            }
        }

        self.key_span = None;

        // Group membership is lifted out here, before the fragment discard,
        // and it is the *same* parse: `read_key` put it in `declared` and this
        // takes it back out. A second reader over the metadata could disagree
        // with the first, which is the failure this ordering avoids.
        //
        // It is lifted because grouping is not a publication decision. An
        // operand group must be able to contain a fragment — a fragment is
        // exactly what an association looks for, through its synthesized
        // evidence emitter — so discarding it with the publication keys would
        // make `%group` unable to name the thing it exists to name.
        let groups = declared.groups.clone();

        // Grouping is excluded from this emptiness test on purpose: a
        // definition whose only policy key is `groups` has declared no
        // *publication* policy, so a fragment carrying it must not reach the
        // warning below.
        let mut publication = declared.clone();
        publication.groups = Vec::new();
        if publication.is_empty() {
            return ReadPolicy {
                policy: if declared.is_empty() {
                    None
                } else {
                    Some(declared)
                },
                groups,
            };
        }

        // A fragment never emits, so publication policy on it cannot run and
        // would be indistinguishable from policy that happened to keep nothing.
        if self.definition.visibility() == Some(DefinitionVisibility::Fragment) {
            self.warn(
                DiagnosticCode::IneffectivePolicy,
                format!(
                    "fragment `{}` declares result policy, but a fragment never emits, so no \
                     policy on it can apply — move it to a pattern that references the fragment",
                    self.definition.name
                ),
            );
            return ReadPolicy {
                policy: None,
                groups,
            };
        }

        ReadPolicy {
            policy: Some(declared),
            groups,
        }
    }

    /// Parses one recognized key, reporting an unusable value.
    ///
    /// Recognized keys contain no definition references. Policy-statement
    /// operands are resolved later against the bound catalog.
    fn read_key(&mut self, key: &str, value: &Value, declared: &mut PatternPolicy) {
        match key {
            "output_name" => match value.as_str() {
                Some(name) => declared.output_name = Some(CompactString::new(name)),
                None => self.invalid(key, value, "a string"),
            },
            "groups" => match value.as_array() {
                Some(names) if names.is_empty() => {
                    self.invalid(key, value, "at least one group name");
                }
                Some(names) => {
                    let mut groups: Vec<CompactString> = Vec::with_capacity(names.len());
                    for name in names {
                        let Some(name) = name.as_str() else {
                            return self.invalid(key, value, "a list of strings");
                        };
                        if !crate::fsm::policy::is_group_reference_spelling(name) {
                            return self.invalid(
                                key,
                                value,
                                "a group name writable as `%name` in a policy block",
                            );
                        }
                        if groups.iter().any(|seen| seen == name) {
                            // Refuse duplicates rather than silently normalizing
                            // malformed metadata.
                            return self.invalid(
                                key,
                                value,
                                &format!("`{name}` named twice; each group once"),
                            );
                        }
                        if self
                            .program
                            .groups
                            .iter()
                            .any(|declaration| declaration.name == *name)
                        {
                            // Derivation and restatement must not coexist: a
                            // literal entry restating a predicate holds the
                            // same answer in two places, right until an edit
                            // moves one and not the other, silently.
                            return self.error(
                                DiagnosticCode::LiteralEntryForDerivedGroup,
                                format!(
                                    "`groups:` on `{}` names `{name}`, which is a derived \
                                     `group` — membership is computed from the declaration's \
                                     predicate and must not be restated; delete this entry, or \
                                     the declaration",
                                    self.definition.name
                                ),
                            );
                        }
                        groups.push(CompactString::new(name));
                    }
                    declared.groups = groups;
                }
                None => self.invalid(key, value, "a list of strings"),
            },
            "priority_order" => match value.as_i64() {
                Some(order) => declared.priority_order = Some(order),
                None => self.invalid(key, value, "an integer"),
            },
            "publish" => self.read_publish(key, value, declared),
            "transform" => self.read_transform(key, value, declared),
            "tags" => self.read_tags(key, value, declared),
            "render" => self.read_render(key, value, declared),
            other => debug_assert!(false, "unhandled recognized policy key `{other}`"),
        }
    }

    /// `publish:` names token attributes to carry into published records
    /// without constraining on them.
    ///
    /// A list of attribute names, deduplicated in authored order. An entry the
    /// definition already reads is legal and redundant rather than an error:
    /// what a `key:` projects is an implementation detail of the key, and a
    /// consumer stating its own requirement should not have to know which
    /// attributes some projection happens to pull in.
    ///
    /// Whether the bound corpus supplies the attribute is not decided here —
    /// an unavailable one is the ordinary `warning[attribute_unavailable]` at
    /// binding, which is where the schema is known.
    fn read_publish(&mut self, key: &str, value: &Value, declared: &mut PatternPolicy) {
        let Some(entries) = value.as_array().filter(|entries| !entries.is_empty()) else {
            self.invalid(key, value, "a non-empty list of token attribute names");
            return;
        };
        let mut attributes: Vec<AttributeKey> = Vec::with_capacity(entries.len());
        for entry in entries {
            let Some(name) = entry.as_str() else {
                self.invalid(key, value, "a list of strings");
                return;
            };
            let Ok(attribute) = AttributeKey::new(name) else {
                self.error(
                    DiagnosticCode::InvalidPolicyValue,
                    format!(
                        "`{key}:` on `{}` names `{name}`, which is not a token attribute",
                        self.definition.name
                    ),
                );
                return;
            };
            if attributes.contains(&attribute) {
                self.invalid(
                    key,
                    value,
                    &format!("`{name}` named twice; each attribute once"),
                );
                return;
            }
            attributes.push(attribute);
        }
        declared.publish = attributes;
    }

    /// `transform:` is an ordered list of operations, each applied once.
    ///
    /// A list, not a map, and that is load-bearing: order is the whole meaning
    /// of a transform declaration, and a YAML map's iteration order in this
    /// workspace depends on the build graph (see the note on
    /// [`PatternPolicy::key`]). A repeated op kind is legal — the list is a
    /// sequence, and execution applies entries rather than unique operation
    /// kinds.
    fn read_transform(&mut self, key: &str, value: &Value, declared: &mut PatternPolicy) {
        let Some(entries) = value.as_array().filter(|entries| !entries.is_empty()) else {
            self.invalid(key, value, "a non-empty list of operations");
            return;
        };
        let mut steps = Vec::with_capacity(entries.len());
        for entry in entries {
            let Some(step) = self.read_transform_op(key, entry) else {
                return;
            };
            steps.push(step);
        }

        // A `substitute` writes only to the record's per-token overrides, so a
        // declaration nothing reads back is *provably* inert: the researcher
        // declares a transformation, `check` passes, and the output is
        // byte-identical to not declaring it. That is the silent no-op this
        // tier exists to close, so it is refused at compile time.
        //
        // Three things read overrides. A later `join` renders them into the
        // unit. A `key:` field projecting the written form renders them into
        // the key — added when scoped transforms made a key the point of a
        // substitution rather than an afterthought. And a `render:` would, the
        // moment `RENDERER_REGISTRY` gains an entry.
        //
        // Reading `declared.key` here is sound because the caller visits a
        // *sorted* key list, so `key` and `render` are both parsed before
        // `transform` — the same ordering the retired-key branch relies on.
        let after_last_join = steps
            .iter()
            .rposition(|step| matches!(step.op, TransformOp::Join))
            .map_or(0, |index| index + 1);
        let read_by_key = declared
            .key
            .iter()
            .any(|field| field.attribute == *crate::fsm::policy::unit_surface());
        if !read_by_key
            && declared.render.is_none()
            && steps[after_last_join..]
                .iter()
                .any(|step| matches!(step.op, TransformOp::Substitute { .. }))
        {
            self.error(
                DiagnosticCode::InvalidPolicyValue,
                format!(
                    "`{key}:` on `{}` declares `substitute` with no later `{{op: \"join\"}}` \
                     and no `key:` projecting the written form; a substitution is only \
                     observable through one of those, so as written it would change nothing",
                    self.definition.name
                ),
            );
            return;
        }

        declared.transform = steps;
    }

    /// One `{op: …}` flow map.
    fn read_transform_op(&mut self, key: &str, value: &Value) -> Option<TransformStep> {
        let Some(fields) = value.as_object() else {
            self.invalid(key, value, "a list of `{op: \"…\"}` maps");
            return None;
        };
        let scope = self.read_transform_scope(key, fields.get("scope"))?;
        let Some(name) = fields.get("op").and_then(Value::as_str) else {
            self.error(
                DiagnosticCode::InvalidPolicyValue,
                format!(
                    "`{key}:` on `{}` has an entry with no `op:`; every operation names itself, \
                     one of {}",
                    self.definition.name,
                    quoted_list(TRANSFORM_OPS)
                ),
            );
            return None;
        };
        match name {
            "drop_edge" => {
                let leading = match fields.get("edge").and_then(Value::as_str) {
                    Some("leading") => true,
                    Some("trailing") => false,
                    _ => {
                        self.error(
                            DiagnosticCode::InvalidPolicyValue,
                            format!(
                                "`{key}:` on `{}` declares `drop_edge` without `edge: leading` or \
                                 `edge: \"trailing`; which edge is removed has no default\"",
                                self.definition.name
                            ),
                        );
                        return None;
                    }
                };
                let drop = self.read_guard(key, name, "drop", fields.get("drop"))?;
                let keep = match fields.get("keep") {
                    Some(keep) => Some(self.read_guard(key, name, "keep", Some(keep))?),
                    None => None,
                };
                Some(TransformOp::DropEdge {
                    leading,
                    drop,
                    keep,
                })
            }
            "truncate_before" => {
                let stop = self.read_guard(key, name, "stop", fields.get("stop"))?;
                Some(TransformOp::TruncateBefore { stop })
            }
            "substitute" => {
                let target = self.read_substitute_target(key, fields.get("component"))?;
                let Some(attr) = fields.get("attr").and_then(Value::as_str) else {
                    self.error(
                        DiagnosticCode::InvalidPolicyValue,
                        format!(
                            "`{key}:` on `{}` declares `substitute` without `attr:`; what is \
                             written in place of the token has no default",
                            self.definition.name
                        ),
                    );
                    return None;
                };
                let Ok(attr) = AttributeKey::new(attr) else {
                    self.error(
                        DiagnosticCode::InvalidPolicyValue,
                        format!(
                            "`{key}:` on `{}` substitutes `{attr}`, which is not a token attribute",
                            self.definition.name
                        ),
                    );
                    return None;
                };
                let table = match fields.get("table") {
                    Some(table) => self.read_substitute_table(key, table)?,
                    None => Vec::new(),
                };
                Some(TransformOp::Substitute {
                    target,
                    attr,
                    table,
                })
            }
            "join" => Some(TransformOp::Join),
            _ => {
                self.error(
                    DiagnosticCode::InvalidPolicyValue,
                    format!(
                        "`{key}:` on `{}` names the operation `{name}`, which does not exist; \
                         the operations are {}",
                        self.definition.name,
                        quoted_list(TRANSFORM_OPS)
                    ),
                );
                None
            }
        }
        .map(|op| TransformStep { scope, op })
    }

    /// `scope: "$name"` confines one operation to a named capture.
    ///
    /// Only the shape is checked here — a `$` reference to a non-empty name.
    /// Whether the capture exists, is unambiguous, and covers a token needs
    /// the materialized expression, so it is checked in
    /// [`super::capture_key`] alongside `key:` and `tags:` references.
    fn read_transform_scope(
        &mut self,
        key: &str,
        value: Option<&Value>,
    ) -> Option<Option<CompactString>> {
        let Some(value) = value else {
            return Some(None);
        };
        let name = value
            .as_str()
            .and_then(|text| text.strip_prefix('$'))
            .filter(|name| !name.is_empty());
        let Some(name) = name else {
            self.error(
                DiagnosticCode::InvalidPolicyValue,
                format!(
                    "`{key}:` on `{}` declares a `scope:` that is not a capture reference; \
                     write `scope: \"$name\"`, or omit it to rewrite the whole record",
                    self.definition.name
                ),
            );
            return None;
        };
        Some(Some(CompactString::new(name)))
    }

    /// Compiles a guard, reporting the failure against the key and op that
    /// carried it — `compile_guard` knows the predicate but not where it was
    /// written.
    fn read_guard(
        &mut self,
        key: &str,
        op: &str,
        argument: &str,
        value: Option<&Value>,
    ) -> Option<CompiledGuard> {
        let Some(source) = value.and_then(Value::as_str) else {
            self.error(
                DiagnosticCode::InvalidPolicyValue,
                format!(
                    "`{key}:` on `{}` declares `{op}` whose `{argument}:` is not a lingrep patterns token \
                     predicate written as a string, e.g. `{argument}: \"[upos in {{AUX, ADP}}]\"`",
                    self.definition.name
                ),
            );
            return None;
        };
        match compile_guard(source) {
            Ok(guard) => Some(guard),
            Err(reason) => {
                self.error(
                    DiagnosticCode::InvalidPolicyValue,
                    format!(
                        "`{key}:` on `{}` declares `{op}` whose `{argument}: {source}` {reason}",
                        self.definition.name
                    ),
                );
                None
            }
        }
    }

    /// `substitute` writes to one edge token, so its `component:` is `first` or
    /// `last`.
    ///
    /// `span` and a `$capture` target are refused rather than approximated:
    /// projecting several tokens onto one attribute has no defined semantics
    /// and no witness. The names are shared with `key:`'s components on purpose
    /// — one vocabulary — which is exactly why the two that do not apply here
    /// have to say so instead of being quietly accepted.
    fn read_substitute_target(
        &mut self,
        key: &str,
        value: Option<&Value>,
    ) -> Option<SubstituteTarget> {
        match value.and_then(Value::as_str) {
            Some("first") => Some(SubstituteTarget::First),
            Some("last") => Some(SubstituteTarget::Last),
            Some(other) => {
                let deferred = other == "span" || other.starts_with('$');
                self.error(
                    DiagnosticCode::InvalidPolicyValue,
                    if deferred {
                        format!(
                            "`{key}:` on `{}` substitutes `{other}`, which covers more than one \
                             token; writing several tokens from one attribute has no defined \
                             meaning, so `substitute` takes `first` or `last`",
                            self.definition.name
                        )
                    } else {
                        format!(
                            "`{key}:` on `{}` substitutes `{other}`, which is not a component; \
                             `substitute` takes `first` or `last`",
                            self.definition.name
                        )
                    },
                );
                None
            }
            None => {
                self.error(
                    DiagnosticCode::InvalidPolicyValue,
                    format!(
                        "`{key}:` on `{}` declares `substitute` without `component: first` or \
                         `component: \"last`; which token is rewritten has no default\"",
                        self.definition.name
                    ),
                );
                None
            }
        }
    }

    /// The `table:` rewrites, sorted so the stored policy does not depend on
    /// the build graph's map implementation.
    fn read_substitute_table(&mut self, key: &str, value: &Value) -> Option<Vec<(String, String)>> {
        let Some(entries) = value.as_object() else {
            self.invalid(key, value, "a map of attribute value to replacement");
            return None;
        };
        let mut table = Vec::with_capacity(entries.len());
        for (from, to) in entries {
            let Some(to) = to.as_str() else {
                self.invalid(key, to, "a replacement string");
                return None;
            };
            table.push((from.clone(), to.to_owned()));
        }
        table.sort_by(|left, right| left.0.cmp(&right.0));
        Some(table)
    }

    /// Reads `tags:` as a map from tag name to value source.
    ///
    /// Sorted by tag name for the reason [`PatternPolicy::key`] records: a
    /// map-valued metadata key has no stable iteration order, while record
    /// annotations must be deterministic across builds.
    fn read_tags(&mut self, key: &str, value: &Value, declared: &mut PatternPolicy) {
        let Some(entries) = value.as_object().filter(|entries| !entries.is_empty()) else {
            self.invalid(key, value, "a non-empty map of tag name to source");
            return;
        };
        let mut tags = Vec::with_capacity(entries.len());
        for (name, source) in entries {
            let Some(source) = self.read_tag_source(key, name, source) else {
                return;
            };
            tags.push((CompactString::new(name), source));
        }
        tags.sort_by(|left, right| left.0.cmp(&right.0));
        declared.tags = tags;
    }

    /// One tag's source: a scalar, a capture-presence mapping, or a projection.
    ///
    /// Form (b) and form (c) are both single-line flow maps, so the `$` sigil is
    /// what tells them apart — all-`$` keys is a projection, no-`$` keys is a
    /// presence mapping. Mixing is refused rather than resolved: a map with one
    /// of each is not an unusual spelling of either, it is an author who meant
    /// one and typed the other.
    fn read_tag_source(&mut self, key: &str, name: &str, value: &Value) -> Option<TagSource> {
        match value {
            Value::String(text) => Some(TagSource::Static(text.clone())),
            Value::Bool(_) | Value::Number(_) => Some(TagSource::Static(value.to_string())),
            Value::Object(entries) if !entries.is_empty() => {
                let sigils = entries.keys().filter(|key| key.starts_with('$')).count();
                if sigils == entries.len() {
                    self.read_tag_projection(key, name, entries)
                } else if sigils == 0 {
                    let mut mapping: Vec<(CompactString, String)> =
                        Vec::with_capacity(entries.len());
                    for (capture, text) in entries {
                        let Some(text) = text.as_str() else {
                            self.invalid(key, text, "a tag value");
                            return None;
                        };
                        mapping.push((CompactString::new(capture), text.to_owned()));
                    }
                    mapping.sort_by(|left, right| left.0.cmp(&right.0));
                    Some(TagSource::CapturePresence(mapping))
                } else {
                    self.error(
                        DiagnosticCode::InvalidPolicyValue,
                        format!(
                            "`{key}:` on `{}` gives `{name}` a map mixing `$capture` keys with \
                             plain ones; a tag source is either one `$capture` projected onto an \
                             attribute, or capture names mapped to the value each stands for",
                            self.definition.name
                        ),
                    );
                    None
                }
            }
            _ => {
                self.invalid(key, value, "a value, a capture mapping, or a projection");
                None
            }
        }
    }

    /// Form (c): exactly one `$capture` projected onto an attribute.
    fn read_tag_projection(
        &mut self,
        key: &str,
        name: &str,
        entries: &serde_json::Map<String, Value>,
    ) -> Option<TagSource> {
        if entries.len() != 1 {
            self.error(
                DiagnosticCode::InvalidPolicyValue,
                format!(
                    "`{key}:` on `{}` projects `{name}` off {} captures; one tag carries one \
                     value, so a projection names exactly one",
                    self.definition.name,
                    entries.len()
                ),
            );
            return None;
        }
        let (reference, attribute) = entries.iter().next()?;
        let capture = reference
            .strip_prefix('$')
            .filter(|name| !name.is_empty())?;
        let Some(attribute) = attribute.as_str() else {
            self.invalid(key, attribute, "an attribute name");
            return None;
        };
        let Ok(attr) = AttributeKey::new(attribute) else {
            self.error(
                DiagnosticCode::InvalidPolicyValue,
                format!(
                    "`{key}:` on `{}` projects `{name}` onto `{attribute}`, which is not a token \
                     attribute",
                    self.definition.name
                ),
            );
            return None;
        };
        Some(TagSource::Projection {
            capture: CompactString::new(capture),
            attr,
        })
    }

    /// `render:` names an entry of the renderer registry.
    ///
    /// The registry is empty in v1, so every use is an error. Future renderers
    /// must be registered explicitly; unknown names fail `check`.
    fn read_render(&mut self, key: &str, value: &Value, declared: &mut PatternPolicy) {
        let Some(name) = value.as_str() else {
            self.invalid(key, value, "a renderer name");
            return;
        };
        if !RENDERER_REGISTRY.contains(&name) {
            self.error(
                DiagnosticCode::UnknownRenderer,
                if RENDERER_REGISTRY.is_empty() {
                    format!(
                        "`{key}:` on `{}` names the renderer `{name}`, but no renderers are \
                         registered; every unit this release can build is built by `transform:`",
                        self.definition.name
                    )
                } else {
                    format!(
                        "`{key}:` on `{}` names the renderer `{name}`, which is not registered; \
                         the registered renderers are {}",
                        self.definition.name,
                        quoted_list(RENDERER_REGISTRY)
                    )
                },
            );
            return;
        }
        declared.render = Some(CompactString::new(name));
    }

    fn invalid(&mut self, key: &str, value: &Value, expected: &str) {
        self.error(
            DiagnosticCode::InvalidPolicyValue,
            format!(
                "`{key}:` on `{}` must be {expected}; found {value}",
                self.definition.name
            ),
        );
    }

    fn error(&mut self, code: DiagnosticCode, message: String) {
        self.report(code, message, DiagnosticSeverity::Error);
    }

    fn warn(&mut self, code: DiagnosticCode, message: String) {
        self.report(code, message, DiagnosticSeverity::Warning);
    }

    /// Reports at the key the author wrote, falling back to the definition.
    ///
    /// A diagnostic about a metadata value belongs on that value's key: the
    /// author fixes it by editing the key's line, not by reading the whole
    /// definition. `key_span` is `None` only where the subject really is the
    /// definition — a fragment that declares policy at all, for instance —
    /// and then the definition's span is correct rather than a fallback.
    ///
    /// Delegates to [`diagnostic_at`], which the anchor readers also use.
    fn report(&mut self, code: DiagnosticCode, message: String, severity: DiagnosticSeverity) {
        self.diagnostics.push(diagnostic_at(
            &self.program.sources,
            self.key_span.unwrap_or(self.definition.span),
            code,
            message,
            severity,
        ));
    }
}

/// Mints an emitting definition for every fragment a declared policy must
/// observe, and records it in [`CatalogPolicy::hidden`].
///
/// A policy statement's operand names a shape whose *matches* the engine needs
/// at publication time — where a trailing region ends, whether an exclusion
/// overlaps. A fragment never emits, so those matches do not exist. Synthesis
/// mints `__policy__<fragment>`, whose body is a reference to the fragment:
/// exactly what a researcher would write by hand, minus the obligation to
/// publish it.
///
/// A referenced *pattern* synthesizes nothing; it already emits. Duplicate
/// references synthesize once. Nothing is minted unless a statement names a
/// fragment.
///
/// This runs before `bind`, so the minted definitions are bound, materialized,
/// and lowered with the rest of the program in one pass.
/// Builds the catalog's [`GroupIndex`] over the **complete** definition list
/// and installs it.
///
/// Called after [`synthesize_hidden_emitters`], which appends. A synthesized
/// emitter joins no group, which is what the `None` padding says.
///
/// Building after synthesis keeps the index and definition table on the same
/// domain and makes the size assertion below meaningful.
///
/// [`DefId`]: crate::fsm::policy_program::DefId
pub(crate) fn install_group_index(
    program: &ParsedProgram,
    policy: &mut CatalogPolicy,
    mut authored_groups: Vec<Vec<CompactString>>,
) {
    debug_assert!(
        authored_groups.len() <= program.definitions.len(),
        "synthesis only ever appends"
    );
    authored_groups.resize(program.definitions.len(), Vec::new());
    let mut index =
        crate::fsm::policy_program::GroupIndex::from_definition_groups(&authored_groups);
    // A derived group whose predicate currently selects nothing still exists:
    // `%name` resolves to it and its statements draw the empty-subject
    // warning, rather than an unknown-group refusal claiming the catalog does
    // not declare a group it declares one line up.
    for declaration in &program.groups {
        index.declare(&declaration.name);
    }
    policy.install_group_index(index);
}

/// Builds the catalog's [`DefinitionDomain`] over the **complete** definition
/// list and installs it.
///
/// A sibling of [`install_group_index`] rather than part of it: both index the
/// post-synthesis list and neither can run before it, but one answers what a
/// `%group` selects and the other what a record's name resolves to, and a
/// single call named for one of them would hide the other.
///
/// Eligibility is the **subject domain** — an emitting definition that is not a
/// synthesized evidence emitter. A fragment never emits, so no record can carry
/// its name; an emitter's records are stripped before publication, so no record
/// of one can be a subject either. Both are still operands, which is why this
/// filter is not the domain's only view of a definition: [`DefinitionDomain`]
/// holds every name, eligible or not.
///
/// [`DefinitionDomain`]: crate::fsm::policy_program::DefinitionDomain
pub(crate) fn install_definition_domain(program: &ParsedProgram, policy: &mut CatalogPolicy) {
    policy.install_definitions(crate::fsm::policy_program::DefinitionDomain::new(
        program.definitions.iter().map(|definition| {
            let eligible =
                definition.is_emitting() && !crate::fsm::policy::is_hidden(&definition.name);
            (
                definition.name.as_str(),
                eligible,
                definition.is_ordinary_pattern(),
            )
        }),
    ));
}

/// Lexicons preserve authored member identity, so span/unit rewrites are not a
/// coherent operation on them in the first lexical slice.
pub(crate) fn lexicon_definition_policy_errors(program: &ParsedProgram) -> Vec<Diagnostic> {
    program
        .definitions
        .iter()
        .filter(|definition| definition.expr().is_none())
        .flat_map(|definition| {
            ["transform", "render"].into_iter().filter_map(move |key| {
                let entry = definition.metadata.get(key)?;
                Some(diagnostic_at(
                    &program.sources,
                    entry.span,
                    DiagnosticCode::InvalidPolicyValue,
                    format!("`{key}:` cannot be applied to lexicon `{}`; remove it (or write `{key}: none` on the lexicon to unset an inherited default)", definition.name),
                    DiagnosticSeverity::Error,
                ))
            })
        })
        .collect()
}

/// Binds the authored policy program and installs it.
///
/// Runs after [`install_group_index`], which runs after
/// [`synthesize_hidden_emitters`]. The chain is load-bearing rather than
/// stylistic: a [`DefId`] indexes the **complete** definition list, synthesis
/// appends to that list, and binding resolves `%name` through the index built
/// over it. Reordering any link resolves a name to a different definition
/// without failing to compile.
///
/// A catalog declaring no block installs an empty program, which is what keeps
/// the whole mechanism inert over every tracked catalog.
///
/// Returns the **warnings** the static rules raised — a statement the engine
/// runs over an empty set is honored and inert, and refusing it would be the
/// compiler overruling a declaration it can execute. Errors take the `Err` arm,
/// which is the same split [`extract_policy`] makes over its own diagnostics.
///
/// # Errors
///
/// Every subject or operand naming no definition and no declared group, and
/// every static rule the bound program violates. They are reported together,
/// because an author fixing one name should not be told about the next on the
/// following compile.
///
/// [`DefId`]: crate::fsm::policy_program::DefId
pub(crate) fn bind_policy_program(
    program: &ParsedProgram,
    policy: &mut CatalogPolicy,
) -> Result<Vec<Diagnostic>, Vec<Diagnostic>> {
    let Some(authored) = program.policy.as_ref() else {
        return Ok(Vec::new());
    };
    let names: Vec<CompactString> = program
        .definitions
        .iter()
        .map(|definition| definition.name.clone())
        .collect();
    // Built only to explain a failure, so it is assembled on the error path.
    // `program.sets` is the accumulated catalog's table by the time policy
    // binds, not just this document's, which is what lets the note point into
    // the lexicon file a set was declared in.
    let mut bound = authored
        .bind(&names, policy.group_index())
        .map_err(|errors| {
            let sets: BTreeMap<CompactString, SourceSpan> = program
                .sets
                .iter()
                .map(|set| (set.name.clone(), set.span))
                .collect();
            errors
                .into_iter()
                .map(|error| bind_diagnostic(&program.sources, &sets, &error))
                .collect::<Vec<_>>()
        })?;
    let (partition_errors, partition_warnings): (Vec<_>, Vec<_>) =
        resolve_partitions(authored, &mut bound, program, policy)
            .into_iter()
            .partition(|diagnostic| diagnostic.severity() == DiagnosticSeverity::Error);
    if !partition_errors.is_empty() {
        return Err(partition_errors);
    }

    let mut lexical_policy_errors = Vec::new();
    for op in &bound.ops {
        let (subject, span, verb) = match op {
            crate::fsm::policy_program::BoundOp::Resolve {
                subject,
                partition,
                span,
                ..
            } => {
                let selected = policy
                    .group_index()
                    .expand_subject(*subject, policy.definitions().eligible_for(*subject));
                let reaches_lexicon = selected.into_iter().any(|id| {
                    let name = policy.definitions().name(id);
                    program.definitions[id.index()].expr().is_none()
                        && partition
                            .as_ref()
                            .is_none_or(|classes| classes.contains_key(name))
                });
                if reaches_lexicon {
                    lexical_policy_errors.push(diagnostic_at(&program.sources, span.0, DiagnosticCode::InvalidPolicyValue, "`resolve` cannot select a lexicon; use the closed subject `patterns`, a group containing only ordinary patterns, or a specific ordinary definition".to_owned(), DiagnosticSeverity::Error));
                }
                continue;
            }
            crate::fsm::policy_program::BoundOp::Associate {
                effect: crate::fsm::policy_program::Effect::Detach,
                subject,
                span,
                ..
            } => (*subject, *span, "detach"),
            _ => continue,
        };
        if policy
            .group_index()
            .expand_subject(subject, policy.definitions().eligible_for(subject))
            .into_iter()
            .any(|id| program.definitions[id.index()].expr().is_none())
        {
            lexical_policy_errors.push(diagnostic_at(&program.sources, span.0, DiagnosticCode::InvalidPolicyValue, format!("`{verb}` cannot select a lexicon because it would rewrite authored lexical identity; scope it to `patterns` or ordinary definitions"), DiagnosticSeverity::Error));
        }
    }
    if !lexical_policy_errors.is_empty() {
        return Err(lexical_policy_errors);
    }

    // After binding, because both rules need a resolved operand: whether it
    // reaches a fragment, and whether a `%group` expands to one.
    let (errors, check_warnings): (Vec<_>, Vec<_>) = super::policy_program::check_policy_program(
        &bound,
        &program.definitions,
        policy.group_index(),
        policy.definitions(),
        &program.sources,
    )
    .into_iter()
    .partition(|diagnostic| diagnostic.severity() == DiagnosticSeverity::Error);
    if !errors.is_empty() {
        return Err(errors);
    }

    let mut warnings = partition_warnings;
    warnings.extend(check_warnings);
    policy.install_program(bound);
    Ok(warnings)
}

/// Resolves each partitioned `resolve`'s classes from catalog metadata.
///
/// Here rather than in `Program::bind` because the map is a function of the
/// accumulated catalog's metadata, which binding does not hold — and resolving
/// it any later would put a metadata read inside the engine, which is the one
/// place the language promises metadata never reaches at run time.
///
/// The walk pairs authored and bound operations by index, which is sound on
/// binding's `Ok` arm alone: a statement abandons binding only by pushing an
/// error, and any error takes the `Err` arm before this runs.
///
/// A selected definition **lacking** the key gets no entry — its records are
/// not selected by the statement, which is the same warn-not-refuse stance an
/// empty subject takes. One **carrying** the key with a non-string value is an
/// error at that definition's own key: a class is the key's string value, so a
/// number names no class, and silently skipping the definition would move it
/// out of every contest without saying so.
///
/// A key **nothing** in a non-empty selected set carries — a typo'd
/// `partition: layr` — leaves every class empty, and the statement runs over
/// nothing, on every sentence, forever. That is the empty-subject warning's
/// situation exactly, so it draws the same `IneffectivePolicy` warning rather
/// than a refusal: the engine executes the statement exactly as written.
/// An empty *subject* is not re-warned here — the subject rule already covers
/// it, and two warnings naming one defect would send the author two ways.
///
/// Returns mixed severities, like `check_policy_program`:
/// [`DiagnosticCode::PartitionKeyNotString`] errors, once per offending
/// definition, and the inert-key warning per statement. The warning is
/// suppressed for a statement that drew an error — an all-non-string selected
/// set is also classless, and warning about the emptiness the errors explain
/// would be noise atop the cause.
fn resolve_partitions(
    authored: &crate::fsm::policy_program::Program,
    bound: &mut crate::fsm::policy_program::BoundProgram,
    program: &ParsedProgram,
    policy: &CatalogPolicy,
) -> Vec<Diagnostic> {
    use crate::fsm::policy_program::{BoundOp, PartitionMap};

    debug_assert_eq!(
        bound.ops.len(),
        authored.ops.len(),
        "binding is one-to-one on success"
    );
    let mut diagnostics = Vec::new();
    for (authored_op, bound_op) in authored.ops.iter().zip(bound.ops.iter_mut()) {
        let Op::Resolve {
            partition: Some(key),
            ..
        } = authored_op
        else {
            continue;
        };
        let BoundOp::Resolve {
            subject,
            partition,
            span,
            ..
        } = bound_op
        else {
            continue;
        };
        let reported_before = diagnostics.len();
        let selected = policy
            .group_index()
            .expand_subject(*subject, policy.definitions().eligible_for(*subject));
        let mut classes = PartitionMap::new();
        for definition in &selected {
            let parsed = &program.definitions[definition.index()];
            let Some(entry) = parsed.metadata.get(key.as_str()) else {
                continue;
            };
            if let Value::String(class) = &entry.value {
                classes.insert(parsed.name.clone(), CompactString::from(class.as_str()));
            } else {
                diagnostics.push(diagnostic_at(
                    &program.sources,
                    entry.span,
                    DiagnosticCode::PartitionKeyNotString,
                    format!(
                        "a `resolve … partition: {key}` selects `{}` and reads this key's string \
                         value as the record's class, but the value here is {}; a class is a \
                         string",
                        parsed.name,
                        metadata_value_shape(&entry.value)
                    ),
                    DiagnosticSeverity::Error,
                ));
            }
        }
        if classes.is_empty() && !selected.is_empty() && diagnostics.len() == reported_before {
            diagnostics.push(diagnostic_at(
                &program.sources,
                span.0,
                DiagnosticCode::IneffectivePolicy,
                format!(
                    "this statement can never affect a record — no definition it selects carries \
                     `{key}`, so every class is empty and the contest runs over nothing; \
                     partition by a key the selected definitions carry, or delete the statement"
                ),
                DiagnosticSeverity::Warning,
            ));
        }
        *partition = Some(classes);
    }
    diagnostics
}

/// How a metadata value reads in a diagnostic, so a message can name what
/// arrived rather than only what was wanted.
fn metadata_value_shape(value: &Value) -> &'static str {
    match value {
        Value::String(_) => "a string",
        Value::Number(_) => "a number",
        Value::Bool(_) => "a boolean",
        Value::Array(_) => "a list",
        Value::Object(_) => "a mapping",
        // The metadata grammar has no `null`; a key that says nothing is
        // omitted. Kept total so a future lowering change fails loudly in a
        // message rather than silently in a panic.
        Value::Null => "nothing",
    }
}

/// One unresolved policy name, reported at the statement that named it.
///
/// `sets` is the catalog's declared set table, consulted only to tell an
/// author who wrote `@S` for a declared `set S` what is actually wrong. `@`
/// spans one name space, so the parser cannot tell the two apart, and binding
/// resolves policy operands against definitions alone — a set carries no
/// records for a statement to act on. Without this arm the operand fell to the
/// generic message and reported that the catalog does not declare `S` when it
/// declares it on the line above, sending the author to look for a typo that
/// is not there.
fn bind_diagnostic(
    sources: &SourceRegistry,
    sets: &BTreeMap<CompactString, SourceSpan>,
    error: &BindError,
) -> Diagnostic {
    let (namespace, sigil) = match error.kind {
        BindErrorKind::UnknownDefinition => ("definition", '@'),
        BindErrorKind::UnknownGroup => ("group", '%'),
    };
    if error.kind == BindErrorKind::UnknownDefinition
        && let Some(declared) = sets.get(&error.name)
    {
        return diagnostic_at(
            sources,
            error.span.0,
            DiagnosticCode::UndefinedReference,
            format!(
                "this statement names `@{}`, which this catalog declares as a `set`; a policy \
                 operand must name a definition, and a set emits no records to act on",
                error.name
            ),
            DiagnosticSeverity::Error,
        )
        .with_related_location(RelatedDiagnosticLocation::new(
            location(sources, *declared),
            "declared as a set here",
        ));
    }
    diagnostic_at(
        sources,
        error.span.0,
        DiagnosticCode::UndefinedReference,
        format!(
            "this statement names the {namespace} `{sigil}{}`, which this catalog does not \
             declare",
            error.name
        ),
        DiagnosticSeverity::Error,
    )
}

pub(crate) fn synthesize_hidden_emitters(
    program: &mut ParsedProgram,
    policy: &mut CatalogPolicy,
    authored_groups: &[Vec<CompactString>],
) {
    let fragments: BTreeMap<CompactString, SourceSpan> = program
        .definitions
        .iter()
        .filter(|definition| definition.visibility() == Some(DefinitionVisibility::Fragment))
        .map(|definition| (definition.name.clone(), definition.span))
        .collect();

    // A `BTreeSet` keyed by name: synthesis order must not depend on which
    // statement happened to reference the fragment first.
    //
    // A fragment never emits, so a policy operand needs a synthesized observer
    // or an association would find no raw evidence.
    //
    // This walks the AUTHORED program by name. Binding cannot have happened
    // yet — it assigns a `DefId` over the complete definition list, and this
    // function is what appends to that list. `authored_groups` is that same
    // pre-synthesis list's membership, which is why a `%group` operand can be
    // expanded here without a `GroupIndex` existing.
    let mut referenced: BTreeSet<CompactString> = BTreeSet::new();
    if let Some(authored) = program.policy.as_ref() {
        for op in &authored.ops {
            let Op::Associate { operand, .. } = op else {
                continue;
            };
            match operand {
                Operand::Def(name) => {
                    if fragments.contains_key(name) {
                        referenced.insert(name.clone());
                    }
                }
                Operand::Group(group) => {
                    for (index, held) in authored_groups.iter().enumerate() {
                        if !held.contains(group) {
                            continue;
                        }
                        let Some(member) = program.definitions.get(index) else {
                            continue;
                        };
                        if fragments.contains_key(&member.name) {
                            referenced.insert(member.name.clone());
                        }
                    }
                }
            }
        }
    }

    for fragment in referenced {
        let span = fragments[&fragment];
        let hidden = CompactString::new(format!("{HIDDEN_PREFIX}{fragment}"));
        program.definitions.push(ParsedDefinition {
            name: hidden.clone(),
            body: super::model::ParsedDefinitionBody::Pattern {
                visibility: DefinitionVisibility::Pattern,
                expr: ParsedExpr::Reference {
                    name: fragment.clone(),
                    span,
                },
                relations: Box::default(),
                enhanced_relations: Box::default(),
            },
            metadata: Metadata::default(),
            key: None,
            span,
        });
        policy.hidden.insert(hidden, fragment);
    }
}

/// Resolves each anchor's `exclude` against the **bound** program, checking the
/// two things that need a resolved body.
///
/// Those two are whether the fragment can match the empty span, and whether it
/// names a capture or asserts span equality. Everything else an anchor
/// declaration can get wrong — its shape, fields, uniqueness, whether
/// `exclude` names a fragment, and whether anything references it — is
/// answered by [`extract_policy`] before binding, so a
/// declaration error never queues behind an unrelated body error.
///
/// **Lookaround inside an `exclude` is permitted.** The assertion compiler
/// already walks nested lookaround bodies. Captures and `same_as`
/// stay forbidden: an anchor is a position, and nothing downstream can observe a
/// capture made inside one.
///
/// The resolution is returned rather than recomputed downstream. This is the one
/// pass that must look each `exclude` up by name, so it is the one place the
/// "checked before binding" invariant is asserted; [`anchor_set_digest`] and the
/// assertion compiler take ids and cannot restate it — differently or at all.
///
/// The map is anchor name to [`DefinitionId`], and carries no second copy of the
/// `exclude` name: `bound.definitions[id.index()].name` already holds it, and
/// two fields that are equal by construction are the small inconsistent state
/// this resolution exists to remove.
///
/// `Err` carries errors only, and the signature says so. Every non-fatal thing
/// an anchor declaration can be wrong about is answered before binding, so a
/// severity split here would be a branch that cannot be taken.
pub(crate) fn resolve_anchors(
    policy: &CatalogPolicy,
    bound: &BoundProgram,
) -> Result<BTreeMap<CompactString, DefinitionId>, Vec<Diagnostic>> {
    // Declaring nothing costs nothing: the nullability table is a walk over
    // every definition, and a catalog with no anchors has no question for it.
    if policy.anchors.is_empty() {
        return Ok(BTreeMap::new());
    }

    let mut diagnostics = Vec::new();
    let mut resolved = BTreeMap::new();
    let nullable = definition_nullability(bound);

    for (anchor, exclude) in &policy.anchors {
        let definition = find_definition(bound, exclude)
            .expect("a declared anchor's exclude names a fragment, checked before binding");
        resolved.insert(anchor.clone(), definition.id);
        if nullable[definition.id.index()] {
            diagnostics.push(diagnostic_at(
                &bound.sources,
                definition.span,
                DiagnosticCode::NullablePattern,
                format!(
                    "anchor `{anchor}`'s exclude fragment `{exclude}` matches the empty span, so \
                     every boundary is the earliest one it matches from and the anchor would \
                     always be the raw sentence end; make it consume at least one token"
                ),
                DiagnosticSeverity::Error,
            ));
        }
        if find_capture_or_same_as(
            definition
                .expr()
                .expect("anchor excludes are ordinary fragments"),
            bound,
            &mut BTreeSet::new(),
        ) {
            diagnostics.push(diagnostic_at(
                &bound.sources,
                definition.span,
                DiagnosticCode::InvalidAnchorDeclaration,
                format!(
                    "anchor `{anchor}`'s exclude fragment `{exclude}` uses a capture or \
                     `same_as`; an anchor is a position rather than a record, so nothing \
                     downstream can observe either — write the group unnamed as `( … )` and \
                     express the repetition without span equality"
                ),
                DiagnosticSeverity::Error,
            ));
        }
    }
    if diagnostics.is_empty() {
        Ok(resolved)
    } else {
        Err(diagnostics)
    }
}

/// Reads every declared anchor's `max_nesting:` claim, keyed by anchor name.
///
/// Refuses four invalid states: a
/// declared anchor whose `exclude` carries no claim, a claim whose shape cannot
/// be executed, an entry whose sentinel cannot be located, and a claim on a
/// fragment that is no anchor's `exclude`. The last is the same rule as the
/// first read backwards — a claim nothing executes is as inert as an unchecked
/// number, which is what the compound encoding exists to prevent.
///
/// Runs on the **bound** program, beside [`resolve_anchors`], although every
/// question it asks is answerable before binding. That placement is deliberate:
/// a nullable or capture-carrying `exclude` is refused by `resolve_anchors`
/// first, so an author whose anchor cannot work at all is told *that* rather
/// than being told to document its depth. It reads nothing binding produced,
/// so it stays dictionary-free and needs no automaton.
///
/// What it does **not** do is execute the claim. Whether a witness's derived
/// anchor is where the sentinel says needs tokens, and tokens need a
/// dictionary; [`crate::fsm::verify_anchor_claims`] is the half that has one.
///
/// `Err` carries errors only. Nothing a `max_nesting:` can be wrong about is
/// non-fatal — an unexecuted bound and an honored one look identical from the
/// catalog, which is the failure this whole tier exists to close.
pub(crate) fn read_nesting_claims(
    policy: &CatalogPolicy,
    bound: &BoundProgram,
) -> Result<BTreeMap<CompactString, NestingClaim>, Vec<Diagnostic>> {
    let carriers: Vec<&BoundDefinition> = bound
        .definitions
        .iter()
        .filter(|definition| definition.metadata.contains_key(MAX_NESTING_KEY))
        .collect();
    // With neither anchors nor claims, neither validation loop has work.
    if policy.anchors.is_empty() && carriers.is_empty() {
        return Ok(BTreeMap::new());
    }

    let mut diagnostics = Vec::new();
    // Parsed once per `exclude` fragment rather than once per anchor: two
    // anchors may share one, and a malformed claim written once must be
    // reported once.
    let mut parsed: BTreeMap<&str, NestingClaim> = BTreeMap::new();
    // Every carrier is judged independently: a `max_nesting:` annotation binds
    // to the definition it precedes, so a claim no anchor uses is unexecuted.
    //
    // Anchor *declarations* still dedupe by value (`read_anchor_declarations`'
    // `already_read`), and the asymmetry is deliberate: a declaration names its
    // own subject, so two identical `{name: "a", exclude: "F"}` values say the
    // same thing wherever they are written, while a claim's subject is the
    // definition carrying it.

    let excludes: BTreeSet<&str> = policy.anchors.values().map(CompactString::as_str).collect();
    for exclude in excludes.iter().copied() {
        let definition = find_definition(bound, exclude)
            .expect("a declared anchor's exclude names a fragment, checked before binding");
        let anchor = policy
            .anchors
            .iter()
            .find(|(_, named)| named.as_str() == exclude)
            .map(|(anchor, _)| anchor.as_str())
            .expect("the exclude set is built from these declarations");
        let Some(value) = definition
            .metadata
            .get(MAX_NESTING_KEY)
            .map(|entry| &entry.value)
        else {
            diagnostics.push(nesting_error(
                bound,
                definition.span,
                format!(
                    "anchor `{anchor}` excludes `{exclude}`, which carries no \
                     `{MAX_NESTING_KEY}:`; it recognizes nesting to a fixed depth where the \
                     scanner it approximates counts without limit — add `{MAX_NESTING_KEY}: \
                     {{depth: \"N\", witness: [\"…|…\"], divergence: [\"…|…\"]}}` beside the \
                     `{ANCHOR_KEY}:` declaration"
                ),
            ));
            continue;
        };
        match read_nesting_claim(exclude, value) {
            Ok(claim) => {
                parsed.insert(exclude, claim);
            }
            Err(complaint) => diagnostics.push(nesting_error(bound, definition.span, complaint)),
        }
    }

    for definition in carriers {
        if excludes.contains(definition.name.as_str()) {
            continue;
        }
        diagnostics.push(nesting_error(
            bound,
            definition.span,
            format!(
                "`{MAX_NESTING_KEY}:` on `{name}` is no anchor's `exclude`, so nothing executes \
                 it and an unexecuted bound is as inert as an unchecked number — declare \
                 `{ANCHOR_KEY}: {{name: \"…\", exclude: {name}}}`, or delete the claim",
                name = definition.name
            ),
        ));
    }

    if !diagnostics.is_empty() {
        return Err(diagnostics);
    }
    Ok(policy
        .anchors
        .iter()
        .filter_map(|(anchor, exclude)| {
            parsed
                .get(exclude.as_str())
                .map(|claim| (anchor.clone(), claim.clone()))
        })
        .collect())
}

/// The wire shape of a `max_nesting:` value.
///
/// All three fields required and no others accepted, which is the load-bearing
/// choice in the encoding: a depth without witnesses is unrepresentable, so it
/// is a parse error rather than a lint someone can suppress. `serde` states
/// that rather than a hand-written reader restating it per field.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct NestingClaimSource {
    depth: u32,
    witness: Vec<String>,
    divergence: Vec<String>,
}

/// Reads one `max_nesting:` value carried by the fragment named `name`.
fn read_nesting_claim(name: &str, value: &Value) -> Result<NestingClaim, String> {
    // The shape is checked before `serde` sees it, so that a scalar draws a
    // message written for a researcher rather than one naming this file's
    // private deserialization struct.
    if !value.is_object() {
        return Err(format!(
            "`{MAX_NESTING_KEY}:` on `{name}` is {value}; a nesting claim is a mapping \
             `{{depth: \"N\", witness: [\"…|…\"], divergence: [\"…|…\"]}}`, and all three fields are \
             required so that a depth no sentence exercises is unrepresentable rather than \
             merely discouraged"
        ));
    }
    let source: NestingClaimSource = serde_json::from_value(value.clone()).map_err(|error| {
        format!(
            "`{MAX_NESTING_KEY}:` on `{name}` is not a nesting claim: {error}; the shape is \
             `{{depth: \"N\", witness: [\"…|…\"], divergence: [\"…|…\"]}}`"
        )
    })?;
    Ok(NestingClaim {
        depth: source.depth,
        witness: read_nesting_entries(name, "witness", &source.witness, source.depth)?,
        divergence: read_nesting_entries(
            name,
            "divergence",
            &source.divergence,
            source.depth.saturating_add(1),
        )?,
    })
}

/// Splits every entry of one `witness:`/`divergence:` list on its sentinel.
///
/// `nests_to` is prose only — the depth the field's entries must reach — and is
/// checked against tokens by [`crate::fsm::verify_anchor_claims`], not here.
fn read_nesting_entries(
    name: &str,
    field: &str,
    entries: &[String],
    nests_to: u32,
) -> Result<Vec<NestingEntry>, String> {
    if entries.is_empty() {
        return Err(format!(
            "`{MAX_NESTING_KEY}:` on `{name}` has an empty `{field}:`; a declared depth no \
             sentence exercises is a number nothing can contradict — add at least one sentence \
             nesting to {nests_to}, marking with `|` where the anchor is expected to land"
        ));
    }
    entries
        .iter()
        .map(|entry| {
            NestingEntry::parse(entry).map_err(|found| {
                format!(
                    "`{MAX_NESTING_KEY}:` on `{name}` has the `{field}:` entry `{entry}`, which \
                     contains {found} `|`; an entry marks its expected anchor with exactly one, \
                     and a sentence containing a literal `|` cannot be a witness — pick another \
                     sentence"
                )
            })
        })
        .collect()
}

fn nesting_error(bound: &BoundProgram, span: SourceSpan, message: String) -> Diagnostic {
    diagnostic_at(
        &bound.sources,
        span,
        DiagnosticCode::InvalidNestingClaim,
        message,
        DiagnosticSeverity::Error,
    )
}

/// The bound definition `name` refers to, if the catalog declares one.
fn find_definition<'a>(bound: &'a BoundProgram, name: &str) -> Option<&'a BoundDefinition> {
    bound
        .definitions
        .iter()
        .find(|definition| definition.name == name)
}

/// Whether this expression, or anything it transitively references, names a
/// capture or asserts span equality.
///
/// Transitive because a reference expands: an `exclude` whose body is `@HELPER`
/// carries whatever `HELPER` declares, and a capture is exactly as unobservable
/// one level down as it is at the root.
fn find_capture_or_same_as(
    expr: &BoundExpr,
    bound: &BoundProgram,
    visited: &mut BTreeSet<DefinitionId>,
) -> bool {
    match expr {
        BoundExpr::Capture { .. } | BoundExpr::SameAs { .. } => true,
        BoundExpr::Token(_) | BoundExpr::Assertion(_) => false,
        BoundExpr::Lookaround(lookaround) => {
            find_capture_or_same_as(&lookaround.body, bound, visited)
        }
        BoundExpr::Sequence(children) | BoundExpr::Alternation(children) => children
            .iter()
            .any(|child| find_capture_or_same_as(child, bound, visited)),
        BoundExpr::AnyOrder { operands, .. } => operands
            .iter()
            .any(|operand| find_capture_or_same_as(operand, bound, visited)),
        BoundExpr::Intersection { operands, .. } => operands
            .iter()
            .any(|operand| find_capture_or_same_as(operand, bound, visited)),
        BoundExpr::Repeat { expr, .. } => find_capture_or_same_as(expr, bound, visited),
        BoundExpr::Definition { id, .. } => {
            visited.insert(*id)
                && find_capture_or_same_as(
                    bound.definitions[id.index()]
                        .expr()
                        .expect("lexicon references are refused"),
                    bound,
                    visited,
                )
        }
    }
}

/// Hashes the complete transitive semantics of every declared anchor.
///
/// **Not the root expression.** A [`BoundExpr`] references definitions by id,
/// and two catalogs whose roots are byte-identical can have different referenced
/// bodies — so hashing the root alone would call non-interchangeable catalogs
/// equal, which is precisely the substitution [`AnchorSetDigest`] exists to
/// refuse. The walk therefore covers, for each anchor in name order:
///
/// * the anchor's name and its `exclude` fragment's name;
/// * every predicate, in the order the expression reaches it;
/// * every position assertion;
/// * every repetition's `min` and `max`;
/// * the alternation/sequence/group structure itself, as discriminant bytes;
/// * each referenced definition's body, expanded transitively, each visited
///   definition contributing its body once (a cycle is impossible — `bind`
///   rejects one);
/// * each nested lookaround's direction, **polarity**, and body.
///
/// Source spans are deliberately absent: the digest asks whether two catalogs
/// compute the same positions, and inserting a comment does not change that.
pub(crate) fn anchor_set_digest(
    anchors: &BTreeMap<CompactString, DefinitionId>,
    bound: &BoundProgram,
) -> AnchorSetDigest {
    // No anchors use the type's canonical empty identity.
    if anchors.is_empty() {
        return AnchorSetDigest::default();
    }
    let mut hasher = blake3::Hasher::new();
    // Domain separation gives changes to the encoded walk an explicit version.
    hasher.update(DIGEST_DOMAIN);
    for (anchor, exclude) in anchors {
        // Two names and a body for every anchor, with no arm that can contribute
        // fewer — the one hole an otherwise injective encoding could have.
        // `resolve_anchors` cannot return an anchor without its body.
        let definition = &bound.definitions[exclude.index()];
        hash_name(&mut hasher, anchor);
        hash_name(&mut hasher, &definition.name);
        let mut visited = BTreeSet::from([definition.id]);
        hash_bound_expr(
            definition
                .expr()
                .expect("anchor excludes are ordinary fragments"),
            bound,
            &mut visited,
            &mut hasher,
        );
    }
    AnchorSetDigest::from_bytes(*hasher.finalize().as_bytes())
}

/// Length-prefixed, so that `("ab", "c")` and `("a", "bc")` cannot collide.
fn hash_name(hasher: &mut blake3::Hasher, name: &str) {
    hasher.update(&(name.len() as u64).to_le_bytes());
    hasher.update(name.as_bytes());
}

/// Exhaustive over [`BoundExpr`], with **no `_` arm**.
///
/// That is the whole reason the digest can be trusted as the language grows: a
/// new variant is a compile error here rather than a silently unhashed one, and
/// a silently unhashed variant would make two catalogs that differ only in it
/// compare interchangeable.
fn hash_bound_expr(
    expr: &BoundExpr,
    bound: &BoundProgram,
    visited: &mut BTreeSet<DefinitionId>,
    hasher: &mut blake3::Hasher,
) {
    match expr {
        BoundExpr::Token(predicate) => {
            hasher.update(&[1]);
            hash_predicate(hasher, predicate);
        }
        BoundExpr::Assertion(assertion) => {
            hasher.update(&[2]);
            hash_assertion(hasher, assertion);
        }
        BoundExpr::Lookaround(lookaround) => {
            hasher.update(&[3]);
            hasher.update(&[match lookaround.direction {
                LookDirection::PrecededBy => 0,
                LookDirection::FollowedBy => 1,
            }]);
            hasher.update(&[match lookaround.polarity {
                LookPolarity::Positive => 0,
                LookPolarity::Negative => 1,
            }]);
            hash_bound_expr(&lookaround.body, bound, visited, hasher);
        }
        BoundExpr::Sequence(children) => {
            hasher.update(&[4]);
            hash_children(children, bound, visited, hasher);
        }
        BoundExpr::Alternation(children) => {
            hasher.update(&[5]);
            hash_children(children, bound, visited, hasher);
        }
        // A fresh tag appended after the existing nine, not inserted among
        // them: the tags are the digest, so renumbering an existing variant
        // would move every catalog's anchor digest to record that the language
        // grew a construct the catalog does not use. The span is not hashed —
        // it is where the text sits, not what the text means.
        BoundExpr::Intersection { operands, .. } => {
            hasher.update(&[10]);
            hash_children(operands, bound, visited, hasher);
        }
        BoundExpr::AnyOrder { operands, .. } => {
            hasher.update(&[11]);
            hash_children(operands, bound, visited, hasher);
        }
        BoundExpr::Repeat { expr, min, max } => {
            hasher.update(&[6]);
            hasher.update(&(*min as u64).to_le_bytes());
            match max {
                Some(max) => {
                    hasher.update(&[1]);
                    hasher.update(&(*max as u64).to_le_bytes());
                }
                None => {
                    hasher.update(&[0]);
                }
            }
            hash_bound_expr(expr, bound, visited, hasher);
        }
        // A capture and a `same_as` are refused inside an `exclude` by
        // `anchor_binding_diagnostics`, so neither arm can be reached from a
        // catalog that compiles. They exist because exhaustiveness is the
        // guarantee, and they hash the authored name and the projection rather
        // than the `CaptureDeclId`, which is an allocation counter.
        BoundExpr::Capture { name, expr, .. } => {
            hasher.update(&[7]);
            hash_name(hasher, name);
            hash_bound_expr(expr, bound, visited, hasher);
        }
        BoundExpr::SameAs { projection, .. } => {
            hasher.update(&[8]);
            hash_name(hasher, projection.attribute().as_str());
        }
        // The reference contributes its target's *name* every time, and its
        // target's body only on first visit: repeating the body would be work
        // without information, and omitting the name would let two distinct
        // helpers with equal bodies look like one.
        BoundExpr::Definition { id, .. } => {
            hasher.update(&[9]);
            let definition = &bound.definitions[id.index()];
            hash_name(hasher, &definition.name);
            if visited.insert(*id) {
                hash_bound_expr(
                    definition.expr().expect("lexicon references are refused"),
                    bound,
                    visited,
                    hasher,
                );
            }
        }
    }
}

fn hash_children(
    children: &[BoundExpr],
    bound: &BoundProgram,
    visited: &mut BTreeSet<DefinitionId>,
    hasher: &mut blake3::Hasher,
) {
    hasher.update(&(children.len() as u64).to_le_bytes());
    for child in children {
        hash_bound_expr(child, bound, visited, hasher);
    }
}

/// Exhaustive over [`PositionAssertion`], for the same reason as
/// [`hash_bound_expr`].
fn hash_assertion(hasher: &mut blake3::Hasher, assertion: &PositionAssertion) {
    match assertion {
        PositionAssertion::SentenceStart => hasher.update(&[1]),
        PositionAssertion::SentenceEnd => hasher.update(&[2]),
        PositionAssertion::PreviousToken(predicate) => {
            hasher.update(&[3]);
            hash_predicate(hasher, predicate);
            hasher
        }
        // Occurrence ids are assigned by the assertion compiler, which runs
        // after binding, so a `BoundExpr` never carries one. Hashed anyway
        // rather than skipped: an unreachable arm that discards information is
        // one refactor away from a reachable one that does.
        PositionAssertion::Lookaround(occurrence) => {
            hasher.update(&[4]);
            hasher.update(&(occurrence.index() as u64).to_le_bytes())
        }
    };
}

fn hash_predicate(hasher: &mut blake3::Hasher, predicate: &TokenPredicate) {
    hasher.update(&(predicate.len() as u64).to_le_bytes());
    for attribute in predicate {
        hash_attr_value(hasher, attribute);
    }
}

/// Exhaustive over [`AttrValueType`], for the same reason as
/// [`hash_bound_expr`].
///
/// The attribute is hashed by its lingrep patterns spelling rather than by its position in
/// a physical analyzer column list, so changing one adapter's storage order —
/// which changes no catalog's meaning — does not change any digest.
fn hash_attr_value(hasher: &mut blake3::Hasher, attribute: &crate::fsm::SymbolicAttrValue) {
    hash_name(hasher, attribute.attr.as_str());
    let (tag, values): (u8, Vec<&str>) = match &attribute.value {
        AttrValueType::Single(value) => (1, vec![value.as_str()]),
        AttrValueType::OneOf(values) => (2, values.iter().map(CompactString::as_str).collect()),
        AttrValueType::NotIn(values) => (3, values.iter().map(CompactString::as_str).collect()),
        AttrValueType::SetHas(value) => (8, vec![value.as_str()]),
        AttrValueType::SetHasAny(values) => (9, values.iter().map(CompactString::as_str).collect()),
        AttrValueType::SetNotHas(value) => (10, vec![value.as_str()]),
        AttrValueType::SetNotHasAny(values) => {
            (11, values.iter().map(CompactString::as_str).collect())
        }
        AttrValueType::Contains(value) => (4, vec![value.as_str()]),
        AttrValueType::NotContains(value) => (5, vec![value.as_str()]),
        AttrValueType::Regex(regex) => (6, vec![regex.as_str()]),
        AttrValueType::NotRegex(regex) => (7, vec![regex.as_str()]),
    };
    hasher.update(&[tag]);
    hasher.update(&(values.len() as u64).to_le_bytes());
    for value in values {
        hash_name(hasher, value);
    }
    // Appended, and only when true — same discipline as `AttrValue`'s
    // `Hash` impl, so that no tracked catalog's digest moves to record a
    // property it does not use.
    if attribute.complement {
        hasher.update(&[1]);
    }
}

/// Rejects a source-authored name in the reserved synthesis namespace.
///
/// Synthesis mints `__policy__<fragment>` emitters, so source-authored names in
/// that namespace could collide with generated records.
pub(crate) fn reserved_name_errors(program: &ParsedProgram) -> Vec<Diagnostic> {
    program
        .definitions
        .iter()
        .filter(|definition| is_hidden(&definition.name))
        .map(|definition| {
            let (line, column) = program.sources.line_column(definition.span);
            let range = definition.span.byte_range();
            Diagnostic::error_with_source_label(
                DiagnosticCode::ReservedName,
                program.sources.source_label(definition.span.source()),
                usize::try_from(range.start).expect("source offsets originated as usize")
                    ..usize::try_from(range.end).expect("source offsets originated as usize"),
                format!(
                    "`{}` uses the `__policy__` prefix, which the compiler reserves for \
                     emitters it synthesizes for a policy statement's fragment operands",
                    definition.name
                ),
                line,
                column,
            )
        })
        .collect()
}

/// Checks that no definition can be detached to an empty span.
///
/// The bridge between the catalog's installed program and the width analysis
/// over the bound definitions. Split from
/// [`super::policy_program::check_detach_non_emptiness`] so that function takes
/// only what it reasons about, and so the two places that know how a
/// `CatalogPolicy` stores its program and its domains stay on this side.
///
/// # Errors
///
/// Every definition detached at both edges whose operands can meet.
pub(crate) fn check_detach_widths(
    policy: &CatalogPolicy,
    bound: &BoundProgram,
) -> Result<(), Vec<Diagnostic>> {
    if policy.program().ops.is_empty() {
        return Ok(());
    }
    let widths = super::policy_program::definition_widths(&bound.definitions);
    let names = |definition: crate::fsm::policy_program::DefId| {
        bound
            .definitions
            .get(definition.index())
            .map_or_else(|| CompactString::new("?"), |held| held.name.clone())
    };
    let diagnostics = super::policy_program::check_detach_non_emptiness(
        policy.program(),
        policy.group_index(),
        policy.definitions(),
        &widths,
        &bound.sources,
        &names,
    );
    if diagnostics.is_empty() {
        Ok(())
    } else {
        Err(diagnostics)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fsm::compiler::PatternSource;
    use crate::fsm::compiler::limits::CompileLimits;
    use crate::fsm::compiler::lower::lower_document;

    fn parse(text: &str) -> ParsedProgram {
        let source = PatternSource::named("test.lx", text);
        lower_document(&source, &CompileLimits::default()).expect("document lowers")
    }

    /// Drops the authored group membership, which these tests do not assert
    /// on: `%name` resolution is covered end to end in
    /// `lingrep-core/tests/suite/policy_group_test.rs`, where the index is built
    /// over the complete post-synthesis definition list.
    fn extract(text: &str) -> (CatalogPolicy, Vec<Diagnostic>) {
        let (policy, _groups, diagnostics) = extract_policy(&parse(text));
        (policy, diagnostics)
    }

    fn messages(diagnostics: &[Diagnostic]) -> Vec<String> {
        diagnostics
            .iter()
            .map(|diagnostic| diagnostic.message.clone())
            .collect()
    }

    #[test]
    fn recognized_keys_parse_into_a_pattern_policy() {
        let (policy, diagnostics) = extract(
            r#"
output_name: "published"
exact := [text=a].

priority_order: -2
near := [text=b].
"#,
        );
        assert_eq!(messages(&diagnostics), Vec::<String>::new());

        let exact = policy.get("exact").expect("exact declares policy");
        assert_eq!(exact.output_name.as_deref(), Some("published"));

        let near = policy.get("near").expect("near declares policy");
        assert_eq!(near.priority_order, Some(-2));
        // Entry order is declaration order, so diagnostics and any downstream
        // rendering are stable across runs.
        assert_eq!(
            policy
                .entries()
                .keys()
                .map(CompactString::as_str)
                .collect::<Vec<_>>(),
            ["exact", "near"]
        );
    }

    #[test]
    fn a_value_of_the_wrong_shape_is_an_error() {
        let (policy, diagnostics) = extract(
            r#"
priority_order: "yes"
bad := [text=a].
"#,
        );
        assert_eq!(diagnostics.len(), 1, "{diagnostics:#?}");
        assert_eq!(diagnostics[0].code(), DiagnosticCode::InvalidPolicyValue);
        assert_eq!(
            diagnostics[0].severity(),
            crate::fsm::compiler::DiagnosticSeverity::Error
        );
        assert!(
            diagnostics[0].message.contains("`priority_order:`")
                && diagnostics[0].message.contains("bad")
                && diagnostics[0].message.contains("\"yes\""),
            "{}",
            diagnostics[0].message
        );
        assert!(!policy.declared(), "{policy:#?}");
    }

    #[test]
    fn terminal_false_reports_the_retirement_and_not_a_bad_value() {
        let (policy, diagnostics) = extract(
            r#"
terminal: false
off := [text=a].
"#,
        );
        assert_eq!(diagnostics.len(), 1, "{diagnostics:#?}");
        assert_eq!(diagnostics[0].code(), DiagnosticCode::RetiredPolicyKey);
        assert!(!policy.declared(), "{policy:#?}");
    }

    #[test]
    fn a_near_miss_key_warns_and_an_unrelated_key_does_not() {
        let (policy, diagnostics) = extract(
            r#"
output_namme: "x"
mapping_note: "deliberate free-form annotation"
typo := [text=a].
"#,
        );
        assert_eq!(diagnostics.len(), 1, "{diagnostics:#?}");
        assert_eq!(diagnostics[0].code(), DiagnosticCode::SuspiciousPolicyKey);
        assert_eq!(
            diagnostics[0].severity(),
            crate::fsm::compiler::DiagnosticSeverity::Warning
        );
        assert!(
            diagnostics[0].message.contains("output_namme")
                && diagnostics[0].message.contains("output_name"),
            "{}",
            diagnostics[0].message
        );
        // The misspelled key is not silently honored — the catalog declares
        // nothing, which is what makes the warning worth reading.
        assert!(!policy.declared(), "{policy:#?}");
    }

    #[test]
    fn a_definition_with_no_policy_keys_has_no_entry() {
        let (policy, diagnostics) = extract(
            r#"
role: "research"
examples: ["あ"]
plain := [text=あ].
"#,
        );
        assert_eq!(messages(&diagnostics), Vec::<String>::new());
        assert_eq!(policy, CatalogPolicy::default());
    }

    #[test]
    fn policy_on_a_fragment_warns_and_is_not_stored() {
        // A fragment never emits, so `output_name` on one is prose that looks
        // executable. Same failure mode `UnverifiableExamples` exists to close.
        let (policy, diagnostics) = extract(
            r#"
output_name: "never"
fragment hidden_shape := [text=a].

pattern user := @hidden_shape.
"#,
        );
        assert_eq!(diagnostics.len(), 1, "{diagnostics:#?}");
        assert_eq!(diagnostics[0].code(), DiagnosticCode::IneffectivePolicy);
        assert_eq!(
            diagnostics[0].severity(),
            crate::fsm::compiler::DiagnosticSeverity::Warning
        );
        assert!(
            diagnostics[0].message.contains("hidden_shape"),
            "{}",
            diagnostics[0].message
        );
        assert!(!policy.declared(), "{policy:#?}");
    }

    #[test]
    fn the_reserved_synthesis_prefix_is_rejected_in_source() {
        let errors = reserved_name_errors(&parse("__policy__collide := [text=a]."));
        assert_eq!(errors.len(), 1, "{errors:#?}");
        assert_eq!(errors[0].code(), DiagnosticCode::ReservedName);
        assert!(
            errors[0].message.contains("__policy__collide"),
            "{}",
            errors[0].message
        );
    }
}
