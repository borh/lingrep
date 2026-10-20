use tree_sitter::{Node, Parser, Point};
use tree_sitter_lingrep::LANGUAGE;

use super::PatternSource;
use super::limits::CompileLimits;
use super::lower::lower_document;
use super::source_text::parser_text;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FormatOutcome {
    pub source: String,
    pub changed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum FormatError {
    #[error("invalid lingrep patterns syntax at {line}:{column}")]
    Syntax { line: usize, column: usize },
    #[error("lingrep patterns source uses {actual} bytes; maximum is {maximum}")]
    SourceTooLarge { actual: usize, maximum: usize },
    #[error("lingrep patterns parser initialization failed")]
    Parser,
}

pub fn format_pattern_source(
    source: PatternSource<'_>,
    limits: CompileLimits,
) -> Result<FormatOutcome, FormatError> {
    let actual =
        source
            .text
            .len()
            .checked_add(source.name.len())
            .ok_or(FormatError::SourceTooLarge {
                actual: usize::MAX,
                maximum: limits.max_source_bytes(),
            })?;
    if actual > limits.max_source_bytes() {
        return Err(FormatError::SourceTooLarge {
            actual,
            maximum: limits.max_source_bytes(),
        });
    }

    let parser_source = parser_text(source.text);
    let mut parser = Parser::new();
    parser
        .set_language(&LANGUAGE.into())
        .map_err(|_| FormatError::Parser)?;
    let tree = parser
        .parse(parser_source.as_ref(), None)
        .ok_or(FormatError::Parser)?;
    if let Some(point) = first_error(tree.root_node()) {
        return Err(FormatError::Syntax {
            line: point.row + 1,
            column: point.column + 1,
        });
    }
    if let Err(error) = lower_document(&source, &limits) {
        let location = error
            .diagnostics
            .first()
            .map(super::diagnostic::Diagnostic::location);
        return Err(FormatError::Syntax {
            line: location.map_or(1, super::diagnostic::DiagnosticLocation::line),
            column: location.map_or(1, super::diagnostic::DiagnosticLocation::column),
        });
    }

    let formatted = render(tree.root_node(), parser_source.as_ref());
    Ok(FormatOutcome {
        changed: formatted != source.text,
        source: formatted,
    })
}

#[derive(Clone, Copy)]
struct Leaf<'a> {
    text: &'a str,
    start: usize,
    end: usize,
    is_comment: bool,
    /// Indentation to supply if this leaf opens a line and the author supplied
    /// none, in two-space units. Derived from the parse tree, which is what
    /// lets a line that continues a definition be told apart from one that
    /// starts a new one.
    fallback_indent: usize,
}

/// A block whose body is written one level in: `defaults`, `schema`, `policy`.
///
/// The block keyword and the closing `end` are direct token children of the
/// block node and stay at the block's own level; everything between them is a
/// statement and sits one level further in.
fn is_block(node: Node<'_>) -> bool {
    matches!(
        node.kind(),
        "defaults_block" | "schema_block" | "policy_block"
    )
}

/// Whether a leaf closes a bracket, and so belongs one level back out.
fn closes_a_level(text: &str) -> bool {
    matches!(text, ")" | "]" | "}")
}

fn render(root: Node<'_>, source: &str) -> String {
    let mut leaves = Vec::new();
    collect_document(root, source, 0, &mut leaves);
    let mut output = String::with_capacity(source.len() + 1);
    let mut previous: Option<Leaf<'_>> = None;

    for index in 0..leaves.len() {
        let leaf = leaves[index];
        let next = leaves.get(index + 1).map(|leaf| leaf.text);
        if let Some(previous) = previous {
            // `previous.end`/`leaf.start` are tree-sitter byte offsets, which
            // are always char-aligned.
            #[expect(
                clippy::string_slice,
                reason = "tree-sitter byte offsets are always char boundaries"
            )]
            let original_gap = &source[previous.end..leaf.start];
            if previous.is_comment || original_gap.contains('\n') {
                trim_end_spaces(&mut output);
                if !output.ends_with('\n') {
                    output.push('\n');
                }
                // Keep one blank line wherever the author left one or more.
                // Collapsing a run of them to a single blank line is the usual
                // formatter rule; deleting every one is not, and that is what
                // this did — it ran every paragraph of every tracked catalog
                // together, which is the structure a reader navigates a long
                // catalog by.
                if original_gap.matches('\n').count() > 1 && !output.ends_with("\n\n") {
                    output.push('\n');
                }
                output.push_str(&indentation_for(original_gap, leaf));
            } else if needs_space(previous.text, leaf.text, next, leaf.is_comment) {
                output.push(' ');
            }
        }
        output.push_str(leaf.text);
        previous = Some(leaf);
    }
    trim_end_spaces(&mut output);
    if !output.is_empty() && !output.ends_with('\n') {
        output.push('\n');
    }
    output
}

/// The indentation to emit before a leaf that opens a line.
///
/// The author's own indentation wins whenever there is one. This formatter
/// never moves a line break — it normalizes spacing *within* a line — so
/// re-deriving indentation from a structure it declines to impose would only
/// let it fight hand-aligned continuations, which `patterns/double-wo.lx`
/// nests four levels deep and `patterns/sfe.lx` aligns under an opening
/// bracket. The derived level is a fallback, supplied only where the author
/// wrote nothing: a freshly typed block body or continuation still lands
/// indented in the browser editor.
fn indentation_for(original_gap: &str, leaf: Leaf<'_>) -> String {
    let authored = original_gap
        .rsplit('\n')
        .next()
        .unwrap_or_default()
        .trim_end_matches(|c| c != ' ' && c != '\t');
    if !authored.is_empty() {
        return authored.to_owned();
    }
    let level = leaf
        .fallback_indent
        .saturating_sub(usize::from(closes_a_level(leaf.text)));
    " ".repeat(level * 2)
}

/// Walk the document, giving each leaf the indentation level of the line it
/// would open if the author supplied none.
///
/// Three levels exist and no more, because lingrep patterns nests no further: a statement at
/// the document level, a statement inside a `defaults`/`schema`/`policy` block,
/// and a continuation line of either. A comment is a tree-sitter extra and so
/// sits where it was written — beside a statement, or inside one — and picks up
/// that position's level for free.
fn collect_document<'source>(
    node: Node<'_>,
    source: &'source str,
    base: usize,
    leaves: &mut Vec<Leaf<'source>>,
) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if is_block(child) {
            let mut block = child.walk();
            for entry in child.children(&mut block) {
                if entry.child_count() == 0 && entry.kind() != "comment" {
                    // The block keyword and its `end`.
                    push_leaf(entry, source, base, leaves);
                } else {
                    collect_statement(entry, source, base + 1, leaves);
                }
            }
        } else {
            collect_statement(child, source, base, leaves);
        }
    }
}

/// Emit one statement: its first leaf opens a line at `base`, and every later
/// line it spans is a continuation, one level further in.
fn collect_statement<'source>(
    node: Node<'_>,
    source: &'source str,
    base: usize,
    leaves: &mut Vec<Leaf<'source>>,
) {
    let first = leaves.len();
    collect_leaves(node, source, base + 1, leaves);
    if let Some(leaf) = leaves.get_mut(first) {
        leaf.fallback_indent = base;
    }
}

fn collect_leaves<'source>(
    node: Node<'_>,
    source: &'source str,
    fallback_indent: usize,
    leaves: &mut Vec<Leaf<'source>>,
) {
    if node.child_count() == 0 {
        push_leaf(node, source, fallback_indent, leaves);
        return;
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_leaves(child, source, fallback_indent, leaves);
    }
}

fn push_leaf<'source>(
    node: Node<'_>,
    source: &'source str,
    fallback_indent: usize,
    leaves: &mut Vec<Leaf<'source>>,
) {
    let mut start = node.start_byte();
    let end = node.end_byte();
    if start == end {
        return;
    }
    // `start`/`end` are tree-sitter byte offsets, always char-aligned.
    #[expect(
        clippy::string_slice,
        reason = "tree-sitter byte offsets are always char boundaries"
    )]
    let raw = &source[start..end];
    let is_comment = node.kind() == "comment";
    let text = if is_comment {
        raw
    } else {
        raw.trim_start_matches([' ', '\t'])
    };
    start += raw.len() - text.len();
    leaves.push(Leaf {
        text,
        start,
        end,
        is_comment,
        fallback_indent,
    });
}

fn needs_space(
    previous: &str,
    current: &str,
    next: Option<&str>,
    current_is_comment: bool,
) -> bool {
    if current_is_comment {
        return true;
    }
    // `~` and `!~` are set tight against a literal, like `=` and `!=`. A regex
    // operand is different: it carries its own delimiters, and `cform~/\A終止形/`
    // buries the operator between two slashes. Twelve of the fourteen regex
    // operands in the tracked catalogs are written spaced for that reason.
    if matches!(previous, "~" | "!~") && is_regex(current) {
        return true;
    }
    if matches!(current, "~" | "!~") && next.is_some_and(is_regex) {
        return true;
    }
    // A relation bracket's delimiters are single tokens — `-[` and `]->`, not
    // `-` `[` and `]` `-` `>` — so neither reached the bracket rules below
    // them and both fell through to the two-non-punctuation fallback, which
    // spaced them: every `where` clause in the tracked catalogs formatted as
    // `$v -[ nsubj ]-> $s`. They bracket like `[` and `]`, and the space
    // before `-[` and after `]->` is supplied by the rules that already handle
    // the capture sigil on either side.
    if matches!(
        current,
        "." | "," | ")" | "]" | "}" | "+" | "*" | "?" | "]->"
    ) || matches!(previous, "(" | "[" | "{" | "-[")
    {
        return false;
    }
    // Every scalar attribute operator is set tight against a literal, so `~`
    // and `!~` format like `=` and `!=` rather than picking up surrounding
    // spaces. The regex case is spaced and was handled above.
    if matches!(current, "=" | "!=" | "~" | "!~") || matches!(previous, "=" | "!=" | "~" | "!~") {
        return false;
    }
    if current == ":" {
        return false;
    }
    if previous == ":" || previous == "," {
        return true;
    }
    if current == "(" || current == "(?<" {
        // `(` is tight only where the language spells a call: after one of the
        // reserved construct names, and after `!`, whose operand the grammar
        // guarantees is `(` or `@`. Everywhere else it opens a group, and a
        // group standing beside a reference or a token test needs its space —
        // `@KOTO ([text=が] | [text=も])`, not `@KOTO([text=が] | [text=も])`,
        // which reads as a call to a construct that does not exist.
        return !matches!(
            previous,
            "same_as"
                | "preceded_by"
                | "followed_by"
                | "not_preceded_by"
                | "not_followed_by"
                | "segments"
                | "!"
        );
    }
    if current == "[" {
        return !matches!(previous, "(" | "[");
    }
    // A brace opens a value set, in the two places one can appear: after `in`
    // as an operand, and after `:=` as a `set` declaration's members. Every
    // other brace is set tight to whatever precedes it.
    if current == "{" {
        return matches!(previous, "in" | ":=");
    }
    // A named capture header is written `(?<name>` with no internal spaces,
    // which is how every catalog and every example spells it. `(?<` is one
    // token and `>` is another, and neither appears anywhere else in lingrep patterns, so
    // this needs no context. Without it the fallback reads both as ordinary
    // words and emits `(?< n >`.
    if current == ">" || previous == "(?<" {
        return false;
    }
    // A sigil binds to the name that follows it, and only forwards. `@`
    // introduces a definition or set reference, `%` a policy group, `$` a
    // capture — but `$` is *also* the standalone sentence-end anchor, so
    // suppressing the space before a sigil too would glue `@ARU $.` into
    // `@ARU$.`. Directional, therefore, rather than a punctuation-set
    // membership, which `needs_space` reads symmetrically.
    if matches!(previous, "@" | "%" | "$") {
        return false;
    }
    // `!` binds forward to its operand, which the grammar guarantees is `(` or
    // `@`. Directional like the sigils: `& !@t` keeps its leading space.
    if previous == "!" {
        return false;
    }
    // A sigil is a normal atom on its leading edge: whatever came before it
    // still needs the space it would need before any other name. Without
    // this, the fallback below denies that space whenever `previous` is
    // closing punctuation or a quantifier (`]`, `)`, `}`, `+`, `*`, `?`) —
    // `is_punctuation` calls those punctuation too, and the fallback
    // requires *both* sides non-punctuation, so `[text=そう]` directly
    // before `@ARU` fell through to no space, same defect as the closing
    // bracket already colliding with `same_as(` in this file's own tests.
    // The `previous` case above already ruled out two sigils in a row and
    // an opening bracket was excluded earlier still, so this is unconditional.
    if matches!(current, "@" | "%" | "$") {
        return true;
    }
    // A closing bracket or a quantifier is punctuation on its trailing edge,
    // but what follows one is routinely a word: `[…] followed_by(…)`,
    // `[…] same_as($unit, by=kana)`, `(@gap)* not_preceded_by(…)`. The
    // fallback below wants BOTH sides non-punctuation and so denied that
    // space, emitting `[text=決して]followed_by(` — the same defect the sigil
    // rule above fixed for `@`, `%` and `$`, and the one this file's own
    // comment already named for `same_as(`. Tested on the *word* rather than
    // on `!is_punctuation`, because a quantifier is one token (`{0,4}`) and
    // is not in that set, so `(@gap){0,4}` would otherwise come apart.
    if is_closing(previous) && starts_a_word(current) {
        return true;
    }
    matches!(current, ":=" | "|" | "&" | "in" | "not")
        || matches!(previous, ":=" | "|" | "&" | "in" | "not")
        || (!is_punctuation(previous) && !is_punctuation(current))
}

/// A regex operand, which the grammar hands over as a `value` leaf carrying
/// its own `/…/` delimiters.
fn is_regex(text: &str) -> bool {
    text.len() >= 2 && text.starts_with('/') && text.ends_with('/')
}

/// A token that closes something: a bracket, or a braced quantifier, or a
/// postfix repetition operator.
fn is_closing(text: &str) -> bool {
    matches!(text, ")" | "]" | "}" | "+" | "*" | "?")
        || (text.starts_with('{') && text.ends_with('}'))
}

fn starts_a_word(text: &str) -> bool {
    text.chars()
        .next()
        .is_some_and(|c| c.is_alphanumeric() || c == '_')
}

fn is_punctuation(text: &str) -> bool {
    matches!(
        text,
        ":=" | "|"
            | "&"
            | "="
            | "!="
            | "~"
            | "!~"
            | ":"
            | ","
            | "."
            | "("
            | ")"
            | "["
            | "]"
            | "{"
            | "}"
            | "+"
            | "*"
            | "?"
    )
}

fn trim_end_spaces(output: &mut String) {
    while output.ends_with([' ', '\t']) {
        output.pop();
    }
}

fn first_error(node: Node<'_>) -> Option<Point> {
    if node.is_error() || node.is_missing() {
        return Some(node.start_position());
    }
    let mut cursor = node.walk();
    node.children(&mut cursor).find_map(first_error)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Formats `source` and unwraps the result, for tests that only care
    /// about the emitted text.
    fn formatted(source: &str) -> String {
        format_pattern_source(
            PatternSource::named("lingrep-web.lx", source),
            CompileLimits::default(),
        )
        .expect("test source formats")
        .source
    }

    #[test]
    fn crlf_metadata_is_accepted_and_emits_lf() {
        let source = "category: \"test\"\r\nitem := [upos=VERB].\r\n";
        let output = format_pattern_source(
            PatternSource::named("lingrep-web.lx", source),
            CompileLimits::default(),
        )
        .unwrap();
        assert!(!output.source.contains('\r'));
    }

    /// Expression-level `&` and attribute conjunction use the same spacing.
    #[test]
    fn expression_intersection_is_spaced_like_its_attribute_namesake() {
        let output = format_pattern_source(
            PatternSource::named(
                "lingrep-web.lx",
                "pattern p := ([text=a&lemma=A]&[text=b])[text=c].\n",
            ),
            CompileLimits::default(),
        )
        .unwrap();
        assert_eq!(
            output.source,
            "pattern p := ([text=a & lemma=A] & [text=b]) [text=c].\n"
        );
    }

    #[test]
    fn ordered_key_paths_keep_source_order_and_canonical_spacing() {
        assert_eq!(
            formatted("key:[$head.span.norm,$collocate.norm]\np:=x."),
            "key: [$head.span.norm, $collocate.norm]\np := x.\n"
        );
    }

    #[test]
    fn malformed_source_reports_first_tree_sitter_location() {
        let error = format_pattern_source(
            PatternSource::named("lingrep-web.lx", "ok := [upos=VERB].\nbad := [upos=]."),
            CompileLimits::default(),
        )
        .unwrap_err();
        assert!(matches!(error, FormatError::Syntax { line: 2, .. }));
    }

    #[test]
    fn diagnostic_name_is_charged_to_the_source_budget() {
        let limits = CompileLimits::default()
            .tightened(|draft| draft.max_source_bytes = "lingrep-web.lx".len() + 5)
            .unwrap();
        let error = format_pattern_source(PatternSource::named("lingrep-web.lx", "123456"), limits)
            .unwrap_err();
        assert!(matches!(error, FormatError::SourceTooLarge { .. }));
    }

    #[test]
    fn formats_definition_spacing_without_touching_regex_or_comments() {
        let source =
            "# /not syntax/\n猫 :=([text~/疑う\\/べく/]|犬)+ [upos not in { NOUN,VERB } ] .";
        let output = format_pattern_source(
            PatternSource::named("lingrep-web.lx", source),
            CompileLimits::default(),
        )
        .unwrap();
        assert_eq!(
            output.source,
            "# /not syntax/\n猫 := ([text ~ /疑う\\/べく/] | 犬)+ [upos not in {NOUN, VERB}].\n"
        );
    }

    #[test]
    fn match_mode_operators_are_set_tight_around_a_literal_and_spaced_around_a_regex() {
        let output = format_pattern_source(
            PatternSource::named(
                "lingrep-web.lx",
                "x := [text ~ ない] [unidic.cform !~ /終止形/] [upos = VERB].",
            ),
            CompileLimits::default(),
        )
        .unwrap();
        assert_eq!(
            output.source,
            "x := [text~ない] [unidic.cform !~ /終止形/] [upos=VERB].\n"
        );
    }

    /// The four spacings a leaf-driven formatter got wrong by reading its two
    /// neighbours as unrelated words.
    ///
    /// Each is a token boundary the fallback rule — space iff *neither* side is
    /// punctuation — decided the wrong way, and each was visible in the tracked
    /// catalogs before this: `(?<t1> [])` came back `(?< t1> [])`,
    /// `@KOTO ([text=が] | [text=も])` came back `@KOTO([text=が] | [text=も])`
    /// — a call to a construct lingrep patterns does not have — `[text=決して] followed_by(…)`
    /// came back glued, and a regex operand lost the space that keeps `~` out
    /// of the delimiters.
    #[test]
    fn adjacent_tokens_keep_the_spacing_that_tells_them_apart() {
        for (input, expected) in [
            (
                "p := (?<t1> []) (?<t2> []).",
                "p := (?<t1> []) (?<t2> []).\n",
            ),
            (
                "p := @KOTO ( [text=が] | [text=も] ) [lemma=知る].",
                "p := @KOTO ([text=が] | [text=も]) [lemma=知る].\n",
            ),
            (
                "p := [text=決して] followed_by([]* @NEG).",
                "p := [text=決して] followed_by([]* @NEG).\n",
            ),
            (
                "p := (?<unit> [unidic.pos1=副詞]) same_as($unit, by=unidic.kana).",
                "p := (?<unit> [unidic.pos1=副詞]) same_as($unit, by=unidic.kana).\n",
            ),
            (
                "p := [unidic.cform~/\\A終止形/] [text~ない].",
                "p := [unidic.cform ~ /\\A終止形/] [text~ない].\n",
            ),
            // A quantifier is one token and starts with `{`, so the
            // closing-bracket rule must not pull it off its operand.
            ("p := (@gap){0,4} [] {1,8}.", "p := (@gap){0,4} []{1,8}.\n"),
        ] {
            assert_eq!(formatted(input), expected, "input: {input}");
        }
    }

    /// A blank line survives, a run of them collapses to one, and a line the
    /// author indented keeps the indentation they chose.
    ///
    /// The formatter never moves a line break — it respaces within a line — so
    /// re-deriving indentation would only let it fight hand-aligned
    /// continuations. What it derives is a *fallback*, used where the author
    /// wrote none, which is why the third case gains an indent it never had.
    #[test]
    fn line_structure_survives_and_bare_continuations_gain_a_fallback_indent() {
        assert_eq!(
            formatted("a := 猫.\n\n\n\nb := 犬.\n"),
            "a := 猫.\n\nb := 犬.\n"
        );
        assert_eq!(
            formatted("p :=\n      [upos=VERB]\n    | [upos=NOUN].\n"),
            "p :=\n      [upos=VERB]\n    | [upos=NOUN].\n"
        );
        assert_eq!(
            formatted("p :=\n[upos=VERB]\n| [upos=NOUN].\n"),
            "p :=\n  [upos=VERB]\n  | [upos=NOUN].\n"
        );
        assert_eq!(
            formatted("schema\nlayer: string required\nend\n"),
            "schema\n  layer: string required\nend\n"
        );
        assert_eq!(
            formatted("policy\nresolve all rank: [longest, start, end, pattern].\nend\n"),
            "policy\n  resolve all rank: [longest, start, end, pattern].\nend\n"
        );
        // A closer belongs one level back out, so a multi-line set literal does
        // not acquire an indent on the line that ends it.
        assert_eq!(
            formatted("set S := {\n猫, 犬\n}.\n"),
            "set S := {\n  猫, 犬\n}.\n"
        );
    }

    #[test]
    fn formatting_is_idempotent_for_representative_documents() {
        for source in [
            "x:=[upos=VERB].",
            "x := [text ~ ない] [unidic.cform !~ /終止形/].",
            "category: \"test\"\r\nx := (猫|犬)?.\r\n",
            "defaults\r\n  status: \"experimental\"\r\nend\r\n\r\nx := 猫.\r\n",
            "schema\r\n  layer: string required in {\"S1\"}\r\nend\r\n\r\nlayer: \"S1\"\r\nx := 猫.\r\n",
            "x := preceded_by(猫) [text~/疑う\\/べく/]. # tail",
        ] {
            let once = format_pattern_source(
                PatternSource::named("lingrep-web.lx", source),
                CompileLimits::default(),
            )
            .unwrap()
            .source;
            let twice = format_pattern_source(
                PatternSource::named("lingrep-web.lx", &once),
                CompileLimits::default(),
            )
            .unwrap()
            .source;
            assert_eq!(once, twice, "source: {source}");
        }
    }

    #[hegel::test(test_cases = 300, derandomize = true)]
    fn generated_valid_documents_are_idempotent_and_preserve_catalog(tc: hegel::TestCase) {
        let gap = tc.draw(hegel::generators::sampled_from(vec![" ", "\t", "  "]));
        let eol = tc.draw(hegel::generators::sampled_from(vec!["\n", "\r\n"]));
        let term = tc.draw(hegel::generators::sampled_from(vec![
            "猫",
            "[upos=VERB&unidic.cform!=連用形]",
            "[upos not in {NOUN,VERB}]",
            "(猫|犬)?",
            "preceded_by(猫)",
            "[text~/疑う\\/べく/]",
        ]));
        let source = format!("# 日本語 comment{eol}対象{gap}:={gap}{term}{gap}猫{gap}.{eol}");
        let once = format_pattern_source(
            PatternSource::named("generated.lx", &source),
            CompileLimits::default(),
        )
        .expect("constructed lingrep patterns must format")
        .source;
        let twice = format_pattern_source(
            PatternSource::named("generated.lx", &once),
            CompileLimits::default(),
        )
        .expect("formatted lingrep patterns must format")
        .source;
        assert_eq!(once, twice);

        let original = super::super::compile_catalog(
            PatternSource::named("generated.lx", &source),
            CompileLimits::default(),
        )
        .expect("constructed lingrep patterns must compile");
        let formatted = super::super::compile_catalog(
            PatternSource::named("generated.lx", &once),
            CompileLimits::default(),
        )
        .expect("formatted lingrep patterns must compile");
        assert_eq!(
            original.matcher.patterns().keys().collect::<Vec<_>>(),
            formatted.matcher.patterns().keys().collect::<Vec<_>>()
        );
    }

    #[test]
    fn the_any_token_predicate_formats_as_an_empty_bracket_pair() {
        // `needs_space` returns false whenever the current token is `]`, so
        // `[ ]` collapses; a following `[` takes a space because its previous
        // token is `]`, which is not in the `"(" | "["` exclusion set.
        let output = format_pattern_source(
            PatternSource::named("any-token.lx", "x := [ ] [upos=NOUN] [] ."),
            CompileLimits::default(),
        )
        .unwrap();
        assert_eq!(output.source, "x := [] [upos=NOUN] [].\n");
    }

    /// A `set` declaration keeps the space after `:=`.
    ///
    /// `{` is set tight to whatever precedes it except after `in`, which was
    /// the only place a brace could follow a keyword until `set NAME := {…}.`
    /// existed. A declaration's brace follows `:=`, so the rule denied the
    /// space and emitted `set IU :={言う, いう}.`. The output still reparses —
    /// whitespace is an `extra` — so idempotence and the round-trip property
    /// both stayed green, which is why only reading the emitted text catches
    /// it.
    #[test]
    fn a_set_declaration_keeps_the_space_after_its_assignment() {
        let output = format_pattern_source(
            PatternSource::named("sets.lx", "set IU := {言う, いう}.\nx := [text in @IU].\n"),
            CompileLimits::default(),
        )
        .expect("formats");
        assert_eq!(
            output.source,
            "set IU := {言う, いう}.\nx := [text in @IU].\n"
        );
    }

    /// A relation bracket brackets, and its delimiters are one token each.
    ///
    /// `-[` and `]->` are single tokens, so neither matched the `"(" | "["`
    /// opener set nor the `")" | "]"` closer set and both fell through to the
    /// two-non-punctuation rule: every `where` clause in the tracked catalogs
    /// emitted as `$v -[ nsubj ]-> $s`. The output reparsed — whitespace is an
    /// `extra` — so idempotence, the round trip and the line-structure pin all
    /// stayed green, exactly like the `set` and sigil defects above, and only
    /// reading the emitted text catches it.
    #[test]
    fn a_relation_bracket_binds_tight_to_its_labels() {
        let source = concat!(
            "pattern p := (?<s> [upos=NOUN]) (?<v> [upos=VERB])\n",
            "  where $v -[nsubj|obl:*]-> $s.\n",
        );
        let output = format_pattern_source(
            PatternSource::named("where.lx", source),
            CompileLimits::default(),
        )
        .expect("formats");
        assert_eq!(
            output.source,
            concat!(
                "pattern p := (?<s> [upos=NOUN]) (?<v> [upos=VERB])\n",
                "  where $v -[nsubj | obl:*]-> $s.\n",
            ),
            "the delimiters bracket; `|` keeps this file's spacing for `|`"
        );
    }

    /// Every sigil binds tight to the name it introduces.
    ///
    /// `is_punctuation` listed no sigil, so `needs_space` fell through to its
    /// "two non-punctuation tokens" rule and inserted one. The output reparsed —
    /// whitespace is an `extra`, so `@ ARU` is still a reference and the
    /// idempotence test still passed — which is why this survived unseen: `@`
    /// appears nowhere else in this file, including its tests.
    #[test]
    fn a_sigil_binds_tight_to_its_name() {
        let source = "fragment ARU := [text=ある].\n\
                      pattern p := @ARU (?<unit> [upos=ADV]) same_as($unit, by=unidic.kana).\n";
        let output = format_pattern_source(
            PatternSource::named("sigils.lx", source),
            CompileLimits::default(),
        )
        .expect("formats");
        assert!(
            output.source.contains("@ARU"),
            "a reference keeps its sigil tight: {}",
            output.source
        );
        assert!(
            output.source.contains("$unit"),
            "a capture reference keeps its sigil tight: {}",
            output.source
        );
        assert!(
            !output.source.contains("@ "),
            "no sigil is followed by a space: {}",
            output.source
        );
    }

    /// A sigil suppresses the space after it and never the space before it.
    ///
    /// `$` is the capture sigil *and* the standalone sentence-end anchor, so a
    /// symmetric rule — membership in `is_punctuation`, which `needs_space` reads
    /// on both `previous` and `current` — glues the anchor to the token before it.
    /// This is AGENTS.md's own documented pattern, and the first attempt at this
    /// task formatted it as `^ [text=そう]@ARU$.`.
    #[test]
    fn a_sigil_suppresses_only_the_space_after_it() {
        let source = "pattern result := ^ [text=そう] @ARU $.\npattern two := @ARU @DA.\n";
        let output = format_pattern_source(
            PatternSource::named("anchors.lx", source),
            CompileLimits::default(),
        )
        .expect("formats");
        assert!(
            output.source.contains("@ARU $."),
            "the sentence-end anchor keeps the space before it: {}",
            output.source
        );
        assert!(
            output.source.contains("[text=そう] @ARU"),
            "a reference keeps the space before it: {}",
            output.source
        );
        assert!(
            output.source.contains("@ARU @DA"),
            "two adjacent references stay apart: {}",
            output.source
        );
    }

    /// `!` binds forward to its operand, like the `@` and `%` sigils and for
    /// the same reason — the space belongs before the operator, never after it.
    #[test]
    fn complement_binds_to_its_operand() {
        assert_eq!(
            formatted("test t := text=a.\nfragment f := [unidic.pos1=名詞 &!@t]."),
            "test t := text=a.\nfragment f := [unidic.pos1=名詞 & !@t].\n"
        );
        assert_eq!(
            formatted("fragment f := [!(text=a)]."),
            "fragment f := [!(text=a)].\n"
        );
    }
}
