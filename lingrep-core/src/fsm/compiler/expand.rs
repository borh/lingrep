//! Reference expansion: what a definition matches once every `@name` it
//! stands on has been substituted into it.
//!
//! A catalog is written in fragments on purpose — `patterns/sf-experimental.lx`
//! has eighteen of them under 86 emitting patterns — and reading one emitting
//! definition therefore means reading a chain of others. This resolves the
//! chain and hands back a single self-contained definition, so a reader can see
//! what a pattern is without holding the catalog in their head.
//!
//! The output is **lingrep patterns**, not a rendering of one: it re-parses, so it can be
//! pasted back into an editor and compiled. That is the property that makes it
//! worth substituting text rather than describing structure, and it is pinned
//! below.
//!
//! Substitution walks the parse tree and is re-spaced by [`super::format`],
//! which is why this module knows nothing about binding. The preflight lowers
//! the document — syntax and shape — and deliberately stops there: reference
//! resolution belongs to `bind`, so an undefined name and a reference cycle are
//! both refused *here*, by the recursion, and named in the error.

use tree_sitter::{Node, Parser};
use tree_sitter_lingrep::LANGUAGE;

use super::PatternSource;
use super::format::format_pattern_source;
use super::limits::CompileLimits;
use super::lower::lower_document;
use super::source_text::parser_text;

/// A definition and its reference-free equivalent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DefinitionExpansion {
    /// The definition's authored name, without any sigil.
    pub name: String,
    pub kind: ExpandedDefinitionKind,
    /// The definition exactly as the catalog spells it, whitespace included.
    /// Not reformatted: this is the text the reader is looking at elsewhere.
    pub declaration: String,
    /// The same definition with every reference substituted, present only when
    /// there was at least one reference to substitute. Re-spaced by the
    /// formatter and valid lingrep patterns.
    pub expanded: Option<String>,
    /// The names substituted, in first-appearance order, each once.
    pub references: Box<[String]>,
    /// A left-to-right railroad / state-flow SVG diagram of the expanded
    /// definition's expression, when it is a pattern or fragment.
    pub diagram: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExpandedDefinitionKind {
    /// An emitting definition: `pattern x := …` or the bare `x := …`.
    Pattern,
    Fragment,
    Lexicon,
    Set,
    Test,
    Group,
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ExpandError {
    #[error("invalid lingrep patterns syntax at {line}:{column}")]
    Syntax { line: usize, column: usize },
    #[error("lingrep patterns source uses {actual} bytes; maximum is {maximum}")]
    SourceTooLarge { actual: usize, maximum: usize },
    #[error("expanding {name} exceeds {maximum} bytes")]
    ExpansionTooLarge { name: String, maximum: usize },
    #[error("undefined reference @{name}")]
    UnresolvedReference { name: String },
    #[error("reference cycle: {}", names.join(" -> "))]
    ReferenceCycle { names: Vec<String> },
    #[error("lingrep patterns parser initialization failed")]
    Parser,
}

/// Expand one definition of `source`, by name.
///
/// `Ok(None)` means the catalog compiles and holds no definition of that name —
/// a caller's stale pattern list, not a defect in the source.
pub fn expand_pattern_definition(
    source: PatternSource<'_>,
    limits: CompileLimits,
    name: &str,
) -> Result<Option<DefinitionExpansion>, ExpandError> {
    let actual =
        source
            .text
            .len()
            .checked_add(source.name.len())
            .ok_or(ExpandError::SourceTooLarge {
                actual: usize::MAX,
                maximum: limits.max_source_bytes(),
            })?;
    if actual > limits.max_source_bytes() {
        return Err(ExpandError::SourceTooLarge {
            actual,
            maximum: limits.max_source_bytes(),
        });
    }

    let parser_source = parser_text(source.text);
    let text = parser_source.as_ref();
    let tree = parse(text)?;
    if let Some(node) = first_error(tree.root_node()) {
        let point = node.start_position();
        return Err(ExpandError::Syntax {
            line: point.row + 1,
            column: point.column + 1,
        });
    }
    // Lowering is what makes the splice below safe: it refuses an undefined
    // reference and a reference cycle, so every name this resolves exists and
    // the recursion terminates on the catalog's own structure.
    if let Err(error) = lower_document(&source, &limits) {
        let location = error
            .diagnostics
            .first()
            .map(super::diagnostic::Diagnostic::location);
        return Err(ExpandError::Syntax {
            line: location.map_or(1, super::diagnostic::DiagnosticLocation::line),
            column: location.map_or(1, super::diagnostic::DiagnosticLocation::column),
        });
    }

    let index = index_definitions(tree.root_node(), text);
    let Some(entry) = index.iter().find(|entry| entry.name == name) else {
        return Ok(None);
    };

    let mut state = Expansion {
        subject: entry.name,
        out: String::with_capacity(entry.declaration.len()),
        path: vec![entry.name],
        references: Vec::new(),
    };
    render_definition(entry, &index, text, limits, &mut state)?;

    // The splice carries whatever spacing the referenced body happened to have,
    // so hand the result to the formatter rather than shipping `@A`'s
    // indentation in the middle of `@B`'s line.
    let expanded = (!state.references.is_empty()).then(|| {
        format_pattern_source(PatternSource::named(name, &state.out), limits)
            .map_or_else(|_| state.out.clone(), |outcome| outcome.source)
            .trim_end()
            .to_owned()
    });

    let expanded_source = expanded.as_deref().unwrap_or(entry.declaration);
    let diagram = render_definition_diagram(expanded_source);

    Ok(Some(DefinitionExpansion {
        name: entry.name.to_owned(),
        kind: entry.kind,
        declaration: entry.declaration.to_owned(),
        expanded,
        references: state.references.into_boxed_slice(),
        diagram,
    }))
}

/// One definition's place in the source: what it is called, what kind it is,
/// where its whole declaration sits, and where the body after `:=` sits.
struct Entry<'tree, 'source> {
    name: &'source str,
    kind: ExpandedDefinitionKind,
    /// The authored text of the whole declaration.
    declaration: &'source str,
    /// The declaration's own node, which is what the renderer walks: a
    /// reference is resolved by descending into the referent's subtree, so the
    /// tree has to outlive the index.
    node: Node<'tree>,
}

fn index_definitions<'tree, 'source>(
    root: Node<'tree>,
    text: &'source str,
) -> Vec<Entry<'tree, 'source>> {
    let mut entries = Vec::new();
    let mut cursor = root.walk();
    for child in root.children(&mut cursor) {
        let kind = match child.kind() {
            "pattern_definition" => pattern_kind(child, text),
            "lexicon_definition" => ExpandedDefinitionKind::Lexicon,
            "set_definition" => ExpandedDefinitionKind::Set,
            "test_definition" => ExpandedDefinitionKind::Test,
            "group_definition" => ExpandedDefinitionKind::Group,
            _ => continue,
        };
        let Some(name) = child_of_kind(child, "identifier").map(|node| slice(text, span(node)))
        else {
            continue;
        };
        entries.push(Entry {
            name,
            kind,
            declaration: slice(text, span(child)),
            node: child,
        });
    }
    entries
}

/// A `pattern_definition` carries an optional `definition_kind`; a bare
/// definition has none and emits, exactly like an explicit `pattern`.
fn pattern_kind(node: Node<'_>, text: &str) -> ExpandedDefinitionKind {
    match child_of_kind(node, "definition_kind").map(|kind| slice(text, span(kind))) {
        Some("fragment") => ExpandedDefinitionKind::Fragment,
        _ => ExpandedDefinitionKind::Pattern,
    }
}

/// A definition's body: every child after `:=`, without the terminating `.`.
fn body_nodes<'tree>(node: Node<'tree>) -> Vec<Node<'tree>> {
    let mut cursor = node.walk();
    let all: Vec<Node<'tree>> = node.children(&mut cursor).collect();
    let Some(assign) = all.iter().position(|child| child.kind() == ":=") else {
        return Vec::new();
    };
    all.get(assign + 1..)
        .unwrap_or_default()
        .iter()
        .filter(|child| child.kind() != ".")
        .copied()
        .collect()
}

/// Whether a substituted body has to be parenthesized where it lands.
///
/// A `set_reference` stands where a braced set does and never is. The other two
/// stand where an expression does, and a *composite* one re-associates against
/// whatever surrounds it unless it is bracketed: `@A+` with `A := [x] [y]`
/// means `([x] [y])+`, never `[x] [y]+`. An atomic body is already a unit and
/// gets no parentheses, which is most of them — the tracked catalogs' fragments
/// are overwhelmingly a single `[…]`, and wrapping every one of those turned an
/// expansion into a hedge.
///
/// The one case that is atomic and still needs the brackets is a test behind
/// `!`, because `!` takes `(…)` or `@name` and nothing else, so `[!@T]` with
/// `T := text=x` cannot become `[!text=x]`.
fn needs_parentheses(kind: ReferenceKind, at: Node<'_>, body: &[Node<'_>]) -> bool {
    if kind == ReferenceKind::Set {
        return false;
    }
    // A reference that IS the whole body has nothing around it to re-associate
    // against, so it never needs brackets. This is the common shape of a thin
    // alias — `ている := @VERB_TEIRU.` — and wrapping it put a redundant pair
    // of parentheses around the entire expansion of the first pattern a reader
    // opens.
    if at
        .parent()
        .is_some_and(|parent| parent.kind().ends_with("_definition"))
    {
        return false;
    }
    let [only] = body else {
        return true;
    };
    match kind {
        ReferenceKind::Set => false,
        ReferenceKind::Pattern => !matches!(
            only.kind(),
            "node_pattern"
                | "parenthesized_pattern"
                | "bare_element"
                | "preceded_by"
                | "followed_by"
                | "not_preceded_by"
                | "not_followed_by"
        ),
        ReferenceKind::Test => {
            only.kind() != "attribute"
                || at
                    .parent()
                    .is_some_and(|parent| parent.kind() == "test_negation")
        }
    }
}

/// Render a definition with every reference resolved, depth first.
///
/// Recursive rather than pass-based because the recursion *is* the cycle check:
/// `path` holds the names currently being expanded, so `@A` reaching itself is
/// caught at the reference that closes the loop and named there. Nothing else
/// in this module would catch it — the preflight lowers the document, which
/// checks syntax and shape, and leaves reference resolution to `bind`.
fn render_definition<'source>(
    entry: &Entry<'_, 'source>,
    index: &[Entry<'_, 'source>],
    text: &'source str,
    limits: CompileLimits,
    state: &mut Expansion<'source>,
) -> Result<(), ExpandError> {
    let mut cursor = entry.node.walk();
    let children: Vec<Node<'_>> = entry.node.children(&mut cursor).collect();
    render_children(&children, index, text, limits, false, state)
}

/// The body alone: everything after `:=`, without the terminating `.`.
fn render_body<'source>(
    entry: &Entry<'_, 'source>,
    index: &[Entry<'_, 'source>],
    text: &'source str,
    limits: CompileLimits,
    state: &mut Expansion<'source>,
) -> Result<(), ExpandError> {
    render_children(&body_nodes(entry.node), index, text, limits, true, state)
}

fn render_children<'source>(
    children: &[Node<'_>],
    index: &[Entry<'_, 'source>],
    text: &'source str,
    limits: CompileLimits,
    inlined: bool,
    state: &mut Expansion<'source>,
) -> Result<(), ExpandError> {
    let mut previous: Option<usize> = None;
    for child in children {
        if let Some(end) = previous {
            let gap = slice(text, (end, child.start_byte()));
            if inlined {
                // A substituted body is collapsed to one line. It carried the
                // line breaks and indentation of wherever it was *declared*,
                // which mean nothing at the point it is spliced into — the
                // ragged result is the whole reason this is not the author's
                // gap. The emitting definition's own line structure survives,
                // because it is rendered with `inlined` false.
                if !gap.is_empty() {
                    state.out.push(' ');
                }
            } else {
                state.out.push_str(gap);
            }
        }
        render_node(*child, index, text, limits, inlined, state)?;
        previous = Some(child.end_byte());
    }
    Ok(())
}

fn render_node<'source>(
    node: Node<'_>,
    index: &[Entry<'_, 'source>],
    text: &'source str,
    limits: CompileLimits,
    inlined: bool,
    state: &mut Expansion<'source>,
) -> Result<(), ExpandError> {
    if state.out.len() > limits.max_source_bytes() {
        return Err(ExpandError::ExpansionTooLarge {
            name: state.subject.to_owned(),
            maximum: limits.max_source_bytes(),
        });
    }

    if let Some(kind) = reference_kind(node) {
        let name =
            child_of_kind(node, "identifier").map(|identifier| slice(text, span(identifier)));
        let Some(name) = name else {
            state.out.push_str(slice(text, span(node)));
            return Ok(());
        };
        let Some(entry) = index.iter().find(|entry| entry.name == name) else {
            return Err(ExpandError::UnresolvedReference {
                name: name.to_owned(),
            });
        };
        if state.path.contains(&name) {
            let mut names: Vec<String> = state.path.iter().map(|open| (*open).to_owned()).collect();
            names.push(name.to_owned());
            return Err(ExpandError::ReferenceCycle { names });
        }

        if !state.references.iter().any(|seen| seen == name) {
            state.references.push(name.to_owned());
        }
        state.path.push(entry.name);
        if kind == ReferenceKind::Set && in_label_set(node) {
            render_label_terms(entry, text, state);
            state.path.pop();
            return Ok(());
        }
        let parenthesize = needs_parentheses(kind, node, &body_nodes(entry.node));
        if parenthesize {
            state.out.push('(');
        }
        render_body(entry, index, text, limits, state)?;
        if parenthesize {
            state.out.push(')');
        }
        state.path.pop();
        return Ok(());
    }

    if node.child_count() == 0 {
        state.out.push_str(slice(text, span(node)));
        return Ok(());
    }
    let mut cursor = node.walk();
    let children: Vec<Node<'_>> = node.children(&mut cursor).collect();
    render_children(&children, index, text, limits, inlined, state)
}

/// Whether this reference sits in a `where` clause's relation bracket.
fn in_label_set(node: Node<'_>) -> bool {
    node.parent()
        .is_some_and(|parent| parent.kind() == "label_set")
}

/// A `set` spliced into a relation bracket: its members, joined by `|`.
///
/// Everywhere else a `set_reference` stands exactly where a braced set stands,
/// so the declaration's own `{…}` *is* the substitution and
/// [`needs_parentheses`] says so. A relation bracket is the one place that is
/// not true: `-[@S|obl]->` holds `|`-separated **terms**, so splicing the
/// braces would give `-[{nsubj, obj}|obl]->`, which does not parse — and this
/// module's contract is that an expansion re-compiles. The members joined by
/// `|` do parse, and they are what `bind` expands the reference to anyway, so
/// the text and the compiled meaning stay the same thing.
fn render_label_terms(entry: &Entry<'_, '_>, text: &str, state: &mut Expansion<'_>) {
    let Some(members) = child_of_kind(entry.node, "value_set") else {
        return;
    };
    let mut cursor = members.walk();
    let mut first = true;
    for member in members.named_children(&mut cursor) {
        if !first {
            state.out.push('|');
        }
        first = false;
        state.out.push_str(slice(text, span(member)));
    }
}

/// What is being expanded, what has been substituted, and which names are open.
struct Expansion<'source> {
    subject: &'source str,
    out: String,
    path: Vec<&'source str>,
    references: Vec<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ReferenceKind {
    Pattern,
    Test,
    Set,
}

fn reference_kind(node: Node<'_>) -> Option<ReferenceKind> {
    match node.kind() {
        "pattern_reference" => Some(ReferenceKind::Pattern),
        "test_reference" => Some(ReferenceKind::Test),
        "set_reference" => Some(ReferenceKind::Set),
        _ => None,
    }
}

fn parse(text: &str) -> Result<tree_sitter::Tree, ExpandError> {
    let mut parser = Parser::new();
    parser
        .set_language(&LANGUAGE.into())
        .map_err(|_| ExpandError::Parser)?;
    parser.parse(text, None).ok_or(ExpandError::Parser)
}

fn first_error(node: Node<'_>) -> Option<Node<'_>> {
    if node.is_error() || node.is_missing() {
        return Some(node);
    }
    let mut cursor = node.walk();
    node.children(&mut cursor).find_map(first_error)
}

fn child_of_kind<'tree>(node: Node<'tree>, kind: &str) -> Option<Node<'tree>> {
    let mut cursor = node.walk();
    node.children(&mut cursor)
        .find(|child| child.kind() == kind)
}

fn span(node: Node<'_>) -> (usize, usize) {
    (node.start_byte(), node.end_byte())
}

#[expect(
    clippy::string_slice,
    reason = "tree-sitter byte offsets are always char boundaries"
)]
fn slice(text: &str, at: (usize, usize)) -> &str {
    // Tree-sitter byte offsets are always char boundaries, and `at` only ever
    // comes from one.
    &text[at.0..at.1]
}

// ---------------------------------------------------------------------------
// Railroad / state-flow SVG diagram for expanded pattern definitions
// ---------------------------------------------------------------------------

const MAX_DIAGRAM_NODES: usize = 64;

#[derive(Clone)]
struct DiagramBox {
    width: i64,
    height: i64,
    mid_y: i64,
    svg: String,
}

/// Render a left-to-right railroad / state-flow SVG diagram for a snippet of
/// expanded `lingrep` pattern definitions. When the snippet contains multiple
/// `pattern_definition` statements, their bodies are chained in sequence.
pub fn render_definition_diagram(source: &str) -> Option<String> {
    let parser_source = parser_text(source);
    let text = parser_source.as_ref();
    let tree = parse(text).ok()?;
    let root = tree.root_node();
    let mut cursor = root.walk();
    let defs: Vec<Node<'_>> = root
        .children(&mut cursor)
        .filter(|child| child.kind() == "pattern_definition")
        .collect();
    if defs.is_empty() {
        return None;
    }
    let mut budget = MAX_DIAGRAM_NODES;
    let mut parts = Vec::with_capacity(defs.len());
    for def in &defs {
        let body = def.child_by_field_name("pattern")?;
        parts.push(layout_expr(body, text, &mut budget)?);
    }
    let content = layout_sequence(&parts)?;

    let pad_x = 16;
    let pad_y = 14;
    let start_r = 5;
    let end_r = 6;
    let lead = 18;
    let start_cx = pad_x + start_r;
    let content_x = start_cx + start_r + lead;
    let end_cx = content_x + content.width + lead + end_r;
    let total_w = end_cx + end_r + pad_x;
    let total_h = (content.height + pad_y * 2).max(44);
    let offset_y = (total_h - content.height) / 2;
    let line_y = offset_y + content.mid_y;

    let mut where_badge = String::new();
    let mut final_h = total_h;
    if let Some(where_node) = defs
        .first()
        .and_then(|def| def.child_by_field_name("where"))
    {
        let raw = slice(text, span(where_node)).trim();
        let label = collapse_ws(raw);
        let truncated = truncate_chars(&label, 84);
        let badge_y = total_h + 4;
        final_h = total_h + 26;
        where_badge = format!(
            r#"<text class="fsm-where-note" x="{pad_x}" y="{}" font-size="11"><title>{}</title>{}</text>"#,
            badge_y + 13,
            escape_xml(&label),
            escape_xml(&truncated),
        );
    }

    let x1 = start_cx + start_r;
    let body_svg = &content.svg;
    let x_out = content_x + content.width;
    let x_end = end_cx - end_r;
    let ax = x_end - 5;
    let ay_top = line_y - 3;
    let ay_bot = line_y + 3;
    Some(format!(
        r#"<svg class="pattern-fsm-svg" viewBox="0 0 {total_w} {final_h}" width="{total_w}" height="{final_h}" role="img" aria-label="Expanded pattern state machine"><circle class="fsm-start" cx="{start_cx}" cy="{line_y}" r="{start_r}"/><line class="fsm-edge" x1="{x1}" y1="{line_y}" x2="{content_x}" y2="{line_y}"/><g transform="translate({content_x},{offset_y})">{body_svg}</g><line class="fsm-edge" x1="{x_out}" y1="{line_y}" x2="{x_end}" y2="{line_y}"/><polygon class="fsm-arrow" points="{x_end},{line_y} {ax},{ay_top} {ax},{ay_bot}"/><circle class="fsm-accept-outer" cx="{end_cx}" cy="{line_y}" r="{end_r}"/><circle class="fsm-accept-inner" cx="{end_cx}" cy="{line_y}" r="3"/>{where_badge}</svg>"#
    ))
}

fn layout_expr(node: Node<'_>, text: &str, budget: &mut usize) -> Option<DiagramBox> {
    if *budget == 0 {
        return None;
    }
    *budget -= 1;

    match node.kind() {
        "parenthesized_pattern" => {
            let mut cursor = node.walk();
            let inner = node
                .named_children(&mut cursor)
                .find(|child| child.kind() != "comment")?;
            layout_expr(inner, text, budget)
        }
        "pattern_sequence" => {
            let mut cursor = node.walk();
            let mut items = Vec::new();
            for child in node.named_children(&mut cursor) {
                if child.kind() != "comment" {
                    items.push(layout_expr(child, text, budget)?);
                }
            }
            layout_sequence(&items)
        }
        "alternation" | "intersection" | "any_order" => {
            let mut cursor = node.walk();
            let mut branches = Vec::new();
            for child in node.named_children(&mut cursor) {
                if child.kind() != "comment" {
                    branches.push(layout_expr(child, text, budget)?);
                }
            }
            let badge = match node.kind() {
                "intersection" => Some("&"),
                "any_order" => Some("any_order"),
                _ => None,
            };
            layout_parallel(&branches, badge)
        }
        "quantified_pattern" => {
            let mut cursor = node.walk();
            let mut expr_node = None;
            let mut quant_node = None;
            for child in node.named_children(&mut cursor) {
                if child.kind() == "quantifier" {
                    quant_node = Some(child);
                } else if child.kind() != "comment" {
                    expr_node = Some(child);
                }
            }
            let inner = layout_expr(expr_node?, text, budget)?;
            let quant = slice(text, span(quant_node?)).trim();
            Some(layout_quantified(&inner, quant))
        }
        "named_group" => {
            let name_node = node.child_by_field_name("name")?;
            let pattern_node = node.child_by_field_name("pattern")?;
            let name = slice(text, span(name_node));
            let inner = layout_expr(pattern_node, text, budget)?;
            Some(layout_group(&inner, &format!("${name}"), "fsm-capture-box"))
        }
        "preceded_by" | "not_preceded_by" | "followed_by" | "not_followed_by" => {
            let body_node = node.child_by_field_name("body")?;
            let inner = layout_expr(body_node, text, budget)?;
            let label = match node.kind() {
                "preceded_by" => "⇐ preceded_by",
                "not_preceded_by" => "⇐ ¬preceded_by",
                "followed_by" => "⇒ followed_by",
                _ => "⇒ ¬followed_by",
            };
            Some(layout_group(&inner, label, "fsm-lookaround-box"))
        }
        "sentence_start" => Some(layout_pill("^", "Sentence start (^)", "fsm-anchor-node")),
        "sentence_end" => Some(layout_pill("$", "Sentence end ($)", "fsm-anchor-node")),
        "same_as" => {
            let full = collapse_ws(slice(text, span(node)));
            let short = truncate_chars(&full, 28);
            Some(layout_pill(&short, &full, "fsm-same-as-node"))
        }
        "node_pattern" | "bare_element" | "pattern_reference" => {
            let full = collapse_ws(slice(text, span(node)));
            let display = if full == "[]" {
                "any token []".to_owned()
            } else {
                truncate_chars(&full, 30)
            };
            Some(layout_pill(&display, &full, "fsm-token-node"))
        }
        _ => {
            let full = collapse_ws(slice(text, span(node)));
            let short = truncate_chars(&full, 28);
            Some(layout_pill(&short, &full, "fsm-token-node"))
        }
    }
}

fn layout_pill(label: &str, full_tooltip: &str, class: &str) -> DiagramBox {
    let text_w = advance_width(label);
    let width = (text_w + 20).max(38);
    let height = 28;
    let mid_y = 14;
    let rx = if class == "fsm-anchor-node" { 14 } else { 5 };
    let cx = width / 2;
    let tip = escape_xml(full_tooltip);
    let lbl = escape_xml(label);
    let svg = format!(
        r#"<g class="fsm-node {class}"><title>{tip}</title><rect x="0" y="0" width="{width}" height="{height}" rx="{rx}"/><text x="{cx}" y="18" text-anchor="middle">{lbl}</text></g>"#
    );
    DiagramBox {
        width,
        height,
        mid_y,
        svg,
    }
}

fn layout_sequence(items: &[DiagramBox]) -> Option<DiagramBox> {
    if items.is_empty() {
        return None;
    }
    if items.len() == 1 {
        return Some(items[0].clone());
    }
    let gap = 18;
    let max_above = items.iter().map(|b| b.mid_y).max().unwrap_or(14);
    let max_below = items.iter().map(|b| b.height - b.mid_y).max().unwrap_or(14);
    let height = max_above + max_below;
    let mid_y = max_above;

    let mut x = 0;
    let mut svg = String::new();
    for (idx, item) in items.iter().enumerate() {
        if idx > 0 {
            let x1 = x - gap;
            let x2 = x;
            let ax = x2 - 5;
            let ay_top = mid_y - 3;
            let ay_bot = mid_y + 3;
            svg.push_str(&format!(
                r#"<line class="fsm-edge" x1="{x1}" y1="{mid_y}" x2="{x2}" y2="{mid_y}"/><polygon class="fsm-arrow" points="{x2},{mid_y} {ax},{ay_top} {ax},{ay_bot}"/>"#
            ));
        }
        let y = mid_y - item.mid_y;
        svg.push_str(&format!(
            r#"<g transform="translate({x},{y})">{}</g>"#,
            item.svg
        ));
        x += item.width + gap;
    }
    let width = x - gap;
    Some(DiagramBox {
        width,
        height,
        mid_y,
        svg,
    })
}

fn layout_parallel(branches: &[DiagramBox], badge: Option<&str>) -> Option<DiagramBox> {
    if branches.is_empty() {
        return None;
    }
    if branches.len() == 1 && badge.is_none() {
        return Some(branches[0].clone());
    }
    let pad_x = 18;
    let row_gap = 8;
    let top_pad = if badge.is_some() { 16 } else { 0 };
    let inner_w = branches.iter().map(|b| b.width).max().unwrap_or(40);
    let total_w = inner_w + pad_x * 2;
    let total_h: i64 = top_pad
        + branches.iter().map(|b| b.height).sum::<i64>()
        + row_gap * (branches.len().saturating_sub(1) as i64);
    let mid_y = top_pad + (total_h - top_pad) / 2;

    let mut svg = String::new();
    if let Some(op) = badge {
        svg.push_str(&format!(
            r#"<rect class="fsm-group-frame" x="4" y="2" width="{}" height="{}" rx="6"/><text class="fsm-group-label" x="10" y="13">{}</text>"#,
            total_w - 8,
            total_h - 4,
            escape_xml(op),
        ));
    }

    let mut cur_y = top_pad;
    for branch in branches {
        let by = cur_y + branch.mid_y;
        let bx = pad_x + (inner_w - branch.width) / 2;
        let bx_end = bx + branch.width;
        let c1 = pad_x / 2;
        let c2 = total_w - pad_x / 2;
        let b_svg = &branch.svg;
        svg.push_str(&format!(
            r#"<path class="fsm-edge" d="M 0,{mid_y} C {c1},{mid_y} {c1},{by} {bx},{by}"/><g transform="translate({bx},{cur_y})">{b_svg}</g><path class="fsm-edge" d="M {bx_end},{by} C {c2},{by} {c2},{mid_y} {total_w},{mid_y}"/>"#
        ));
        cur_y += branch.height + row_gap;
    }

    Some(DiagramBox {
        width: total_w,
        height: total_h,
        mid_y,
        svg,
    })
}

fn layout_quantified(inner: &DiagramBox, quant: &str) -> DiagramBox {
    let optional = quant == "?" || quant == "*" || quant.starts_with("{0,");
    let repeating = quant == "+"
        || quant == "*"
        || (quant.starts_with('{') && quant != "{0,1}" && quant != "{1}");

    let pad_x = 16;
    let top_extra = if optional { 14 } else { 4 };
    let bottom_extra = if repeating { 20 } else { 6 };
    let width = inner.width + pad_x * 2;
    let height = inner.height + top_extra + bottom_extra;
    let mid_y = top_extra + inner.mid_y;
    let bx = pad_x;
    let bx_end = pad_x + inner.width;

    let b_svg = &inner.svg;
    let mut svg = format!(
        r#"<line class="fsm-edge" x1="0" y1="{mid_y}" x2="{bx}" y2="{mid_y}"/><g transform="translate({bx},{top_extra})">{b_svg}</g><line class="fsm-edge" x1="{bx_end}" y1="{mid_y}" x2="{width}" y2="{mid_y}"/>"#
    );

    if optional {
        let bypass_y = 4;
        let rx1 = width - 12;
        let rx2 = width - 2;
        svg.push_str(&format!(
            r#"<path class="fsm-edge fsm-bypass" d="M 2,{mid_y} Q 2,{bypass_y} 12,{bypass_y} L {rx1},{bypass_y} Q {rx2},{bypass_y} {rx2},{mid_y}"/>"#
        ));
    }

    if repeating {
        let loop_y = height - 6;
        let lx1 = width - 6;
        let lx2 = width - 14;
        let cx = width / 2;
        let ty = loop_y - 2;
        let q = escape_xml(quant);
        svg.push_str(&format!(
            r#"<path class="fsm-edge fsm-loop" d="M {lx1},{mid_y} Q {lx1},{loop_y} {lx2},{loop_y} L 14,{loop_y} Q 6,{loop_y} 6,{mid_y}"/><text class="fsm-quant-label" x="{cx}" y="{ty}" text-anchor="middle">{q}</text>"#
        ));
    } else if optional {
        let cx = width / 2;
        let q = escape_xml(quant);
        svg.push_str(&format!(
            r#"<text class="fsm-quant-label" x="{cx}" y="12" text-anchor="middle">{q}</text>"#
        ));
    }

    DiagramBox {
        width,
        height,
        mid_y,
        svg,
    }
}

fn layout_group(inner: &DiagramBox, label: &str, class: &str) -> DiagramBox {
    let label_w = advance_width(label) + 16;
    let inner_w = inner.width.max(label_w);
    let pad_x = 10;
    let top_pad = 18;
    let bottom_pad = 8;
    let width = inner_w + pad_x * 2;
    let height = inner.height + top_pad + bottom_pad;
    let mid_y = top_pad + inner.mid_y;
    let bx = pad_x + (inner_w - inner.width) / 2;
    let bx_end = bx + inner.width;

    let lbl = escape_xml(label);
    let b_svg = &inner.svg;
    let svg = format!(
        r#"<g class="{class}"><rect class="fsm-group-frame" x="0" y="0" width="{width}" height="{height}" rx="6"/><text class="fsm-group-label" x="8" y="13">{lbl}</text><line class="fsm-edge" x1="0" y1="{mid_y}" x2="{bx}" y2="{mid_y}"/><g transform="translate({bx},{top_pad})">{b_svg}</g><line class="fsm-edge" x1="{bx_end}" y1="{mid_y}" x2="{width}" y2="{mid_y}"/></g>"#
    );

    DiagramBox {
        width,
        height,
        mid_y,
        svg,
    }
}

fn advance_width(text: &str) -> i64 {
    text.chars()
        .map(|ch| if ch.is_ascii() { 7 } else { 13 })
        .sum()
}

fn collapse_ws(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    let count = text.chars().count();
    if count <= max_chars {
        text.to_owned()
    } else {
        let mut s: String = text.chars().take(max_chars.saturating_sub(1)).collect();
        s.push('…');
        s
    }
}

fn escape_xml(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fsm::compiler::compile_catalog;

    fn expansion(source: &str, name: &str) -> DefinitionExpansion {
        expand_pattern_definition(
            PatternSource::named("test.lx", source),
            CompileLimits::default(),
            name,
        )
        .expect("source must expand")
        .expect("definition must be present")
    }

    #[test]
    fn a_chain_of_fragments_resolves_to_one_self_contained_definition() {
        let source = concat!(
            "fragment INNER := [upos=VERB] [upos=NOUN].\n",
            "fragment OUTER := @INNER [text=を].\n",
            "pattern top := @OUTER [lemma=する].\n",
        );
        let expansion = expansion(source, "top");

        assert_eq!(expansion.kind, ExpandedDefinitionKind::Pattern);
        assert_eq!(expansion.declaration, "pattern top := @OUTER [lemma=する].");
        assert_eq!(
            expansion.expanded.as_deref(),
            Some("pattern top := (([upos=VERB] [upos=NOUN]) [text=を]) [lemma=する].")
        );
        // First-appearance order, each once: `OUTER` is reached first even
        // though `INNER` is the deeper of the two.
        assert_eq!(&*expansion.references, ["OUTER", "INNER"]);
    }

    /// The output is lingrep patterns, not a description of it. This is the property the
    /// whole textual approach rests on, so it is asserted against the compiler
    /// rather than against a string.
    #[test]
    fn an_expansion_compiles_and_needs_no_further_expansion() {
        let source = concat!(
            "fragment A := [upos=VERB] | [upos=ADJ].\n",
            "test T := unidic.pos1=助詞.\n",
            "set S := {動詞, 助動詞}.\n",
            "pattern p := @A+ [!@T] [unidic.pos1 in @S].\n",
        );
        let expansion = expansion(source, "p");
        let expanded = expansion.expanded.expect("references must be substituted");

        assert!(!expanded.contains('@'), "{expanded}");
        compile_catalog(
            PatternSource::named("expanded.lx", &expanded),
            CompileLimits::default(),
        )
        .expect("an expansion must compile on its own");
    }

    /// `@A+` with `A := [x] [y]` is `([x] [y])+`. Splicing the body bare would
    /// quantify only its last element, which is a different pattern that still
    /// compiles — the failure mode a parenthesis-free splice hides.
    #[test]
    fn a_quantified_reference_keeps_the_whole_body_under_its_quantifier() {
        let source = "fragment A := [upos=VERB] [upos=NOUN].\npattern p := @A+.\n";
        assert_eq!(
            expansion(source, "p").expanded.as_deref(),
            Some("pattern p := ([upos=VERB] [upos=NOUN])+.")
        );
    }

    /// An atomic body is spliced bare. Wrapping every reference turned an
    /// expansion into a hedge — the tracked catalogs' fragments are
    /// overwhelmingly a single `[…]`, so nearly every parenthesis in the output
    /// was noise around something already a unit.
    #[test]
    fn an_atomic_body_is_substituted_without_parentheses() {
        let source = concat!(
            "fragment ONE := [upos=VERB].\n",
            "fragment GROUPED := ([upos=NOUN] | [upos=ADJ]).\n",
            "fragment SEQ := [upos=ADV] [upos=PART].\n",
            "pattern p := @ONE @GROUPED @SEQ.\n",
        );
        assert_eq!(
            expansion(source, "p").expanded.as_deref(),
            Some("pattern p := [upos=VERB] ([upos=NOUN] | [upos=ADJ]) ([upos=ADV] [upos=PART]).")
        );
    }

    /// A thin alias expands to its referent, not to a bracketed copy of it.
    #[test]
    fn a_reference_that_is_the_whole_body_needs_no_parentheses() {
        let source = "fragment SEQ := [text in {て, で}] [lemma=居る].\nている := @SEQ.\n";
        assert_eq!(
            expansion(source, "ている").expanded.as_deref(),
            Some("ている := [text in {て, で}] [lemma=居る].")
        );
    }

    /// A substituted body is collapsed to one line, and the emitting
    /// definition's own line structure is kept.
    ///
    /// A fragment carries the indentation of wherever it was *declared*, which
    /// means nothing where it is spliced: keeping it produced output ragged
    /// enough to be unreadable on `patterns/double-wo.lx`, whose fragments run
    /// four hand-indented lines each.
    #[test]
    fn a_multi_line_body_is_collapsed_where_it_lands() {
        let source = concat!(
            "fragment WIDE :=\n    [upos=NOUN]\n  | [upos=ADJ].\n",
            "pattern p :=\n  @WIDE\n  [lemma=だ].\n",
        );
        assert_eq!(
            expansion(source, "p").expanded.as_deref(),
            Some("pattern p :=\n  ([upos=NOUN] | [upos=ADJ])\n  [lemma=だ].")
        );
    }

    /// A set reference stands where a braced set does, so it is substituted
    /// bare — parenthesizing it would not parse.
    #[test]
    fn a_set_reference_is_substituted_without_parentheses() {
        let source = "set CONTENT := {動詞, 助動詞}.\npattern p := [unidic.pos1 in @CONTENT].\n";
        assert_eq!(
            expansion(source, "p").expanded.as_deref(),
            Some("pattern p := [unidic.pos1 in {動詞, 助動詞}].")
        );
    }

    /// The one place the braces are the wrong substitution.
    ///
    /// A relation bracket holds `|`-separated terms, not a braced set, so the
    /// splice the test above pins would produce `-[{nsubj, obj}|obl]->` and
    /// break this module's contract that an expansion re-compiles. The
    /// compilation is half the assertion for exactly that reason.
    #[test]
    fn a_set_reference_in_a_relation_bracket_is_substituted_as_label_terms() {
        let source = concat!(
            "set CORE := {nsubj, obj}.\n",
            "pattern p := (?<a> [upos=NOUN]) (?<v> [upos=VERB])\n",
            "  where $v -[@CORE|obl]-> $a.\n",
        );
        let expanded = expansion(source, "p")
            .expanded
            .expect("the set reference must be substituted");
        assert!(
            // Joined by `|` here and re-spaced by the formatter, which gives
            // `|` the spacing it gives every other `|`.
            expanded.contains("-[nsubj | obj | obl]->"),
            "the members must be joined as terms: {expanded}"
        );
        compile_catalog(
            PatternSource::named("expanded.lx", &expanded),
            CompileLimits::default(),
        )
        .expect("a label-set expansion must compile");
    }

    /// A test reference is parenthesized, which is what keeps `!` in front of
    /// an operand it accepts: `!` takes `(…)` or `@name` and nothing else, so
    /// a bare splice of `text=x` into `[!@T]` would not compile.
    #[test]
    fn a_negated_test_reference_keeps_the_parentheses_its_operand_needs() {
        let source = "test T := unidic.pos1=助詞.\npattern p := [!@T].\n";
        let expanded = expansion(source, "p")
            .expanded
            .expect("the test reference must be substituted");
        assert_eq!(expanded, "pattern p := [!(unidic.pos1=助詞)].");
        compile_catalog(
            PatternSource::named("expanded.lx", &expanded),
            CompileLimits::default(),
        )
        .expect("a negated test expansion must compile");
    }

    #[test]
    fn a_definition_standing_on_nothing_reports_no_expansion() {
        let expansion = expansion("pattern p := [upos=VERB].\n", "p");
        assert_eq!(expansion.expanded, None);
        assert!(expansion.references.is_empty());
    }

    #[test]
    fn every_definition_kind_is_named() {
        let source = concat!(
            "pattern p := [upos=VERB].\n",
            "bare := [upos=NOUN].\n",
            "fragment f := [upos=ADJ].\n",
            "set s := {a}.\n",
            "test t := text=x.\n",
            "layer: \"S1\"\n",
            "pattern q := [upos=ADV].\n",
            "group g := layer = \"S1\".\n",
            "lexicon l by [text] := {\"語\"}.\n",
        );
        for (name, kind) in [
            ("p", ExpandedDefinitionKind::Pattern),
            ("bare", ExpandedDefinitionKind::Pattern),
            ("f", ExpandedDefinitionKind::Fragment),
            ("s", ExpandedDefinitionKind::Set),
            ("t", ExpandedDefinitionKind::Test),
            ("g", ExpandedDefinitionKind::Group),
            ("l", ExpandedDefinitionKind::Lexicon),
        ] {
            assert_eq!(expansion(source, name).kind, kind, "definition {name}");
        }
    }

    #[test]
    fn an_absent_name_is_not_an_error() {
        let outcome = expand_pattern_definition(
            PatternSource::named("test.lx", "pattern p := [upos=VERB].\n"),
            CompileLimits::default(),
            "absent",
        )
        .expect("the catalog compiles");
        assert_eq!(outcome, None);
    }

    #[test]
    fn a_source_that_does_not_parse_is_refused_by_location() {
        let error = expand_pattern_definition(
            PatternSource::named("test.lx", "pattern p := [upos=VERB].\nq := [upos=].\n"),
            CompileLimits::default(),
            "p",
        )
        .expect_err("a syntax error must be refused");
        assert!(
            matches!(error, ExpandError::Syntax { line: 2, .. }),
            "{error}"
        );
    }

    /// Refusal is scoped to what the expansion actually walks.
    ///
    /// The preflight lowers the document, which checks syntax and shape and
    /// stops; binding — where an undefined reference is normally caught — has
    /// not run. So this refuses a name it reaches and cannot resolve, and says
    /// nothing about a broken definition elsewhere in the catalog, because
    /// expanding one definition never reads one it does not stand on.
    #[test]
    fn an_undefined_reference_is_refused_only_when_the_expansion_reaches_it() {
        let source = "pattern p := [upos=VERB].\nq := @missing.\n";
        assert!(
            expansion(source, "p").expanded.is_none(),
            "p stands on nothing and must expand"
        );

        let error = expand_pattern_definition(
            PatternSource::named("test.lx", source),
            CompileLimits::default(),
            "q",
        )
        .expect_err("q stands on a name that does not exist");
        assert!(
            matches!(error, ExpandError::UnresolvedReference { ref name } if name == "missing"),
            "{error}"
        );
    }

    /// The recursion is the cycle check, and it has to be: `bind` would refuse
    /// this, and `bind` has not run.
    #[test]
    fn a_reference_cycle_is_refused_with_the_path_that_closes_it() {
        let error = expand_pattern_definition(
            PatternSource::named(
                "test.lx",
                "fragment a := [upos=VERB] @b.\nfragment b := @a.\npattern p := @a.\n",
            ),
            CompileLimits::default(),
            "p",
        )
        .expect_err("a cycle must be refused");
        assert!(
            matches!(error, ExpandError::ReferenceCycle { ref names } if names == &["p", "a", "b", "a"]),
            "{error}"
        );
    }

    /// A budget refusal, not a hang: the compiler has already refused cycles,
    /// so what remains is a diamond that multiplies out.
    #[test]
    fn an_expansion_past_the_source_budget_is_refused_by_name() {
        let source = concat!(
            "fragment a := [upos=VERB] [upos=NOUN] [upos=ADJ] [upos=ADV].\n",
            "fragment b := @a @a @a @a.\n",
            "fragment c := @b @b @b @b.\n",
            "pattern wide := @c @c @c @c.\n",
        );
        let limits = CompileLimits::default()
            .tightened(|draft| draft.max_source_bytes = 512)
            .expect("512 bytes is a valid budget");
        let error =
            expand_pattern_definition(PatternSource::named("test.lx", source), limits, "wide")
                .expect_err("the expansion must exceed the budget");
        assert!(
            matches!(error, ExpandError::ExpansionTooLarge { ref name, .. } if name == "wide"),
            "{error}"
        );
    }

    /// The declaration is the authored text, whitespace and all — it is what
    /// the reader is looking at in the editor, and reformatting it would put a
    /// second spelling of the same definition on screen beside the first.
    #[test]
    fn the_declaration_is_returned_exactly_as_authored() {
        let source = "fragment A := [upos=NOUN].\npattern p :=\n    @A\n  | [upos=VERB].\n";
        assert_eq!(
            expansion(source, "p").declaration,
            "pattern p :=\n    @A\n  | [upos=VERB]."
        );
    }
}
