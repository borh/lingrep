//! Utilities for compiling inline lingrep patterns source.

use tree_sitter::{Node, Parser};
use tree_sitter_lingrep::LANGUAGE;

use crate::fsm::compiler::source_text::parser_text;

/// Normalize an inline `--pattern` value into a complete lingrep patterns document.
///
/// Classification is delegated to the tree-sitter grammar (the single source
/// of truth). The grammar represents BOTH normal and `fragment` forms as a
/// single `pattern_definition` node kind (a fragment has a `definition_kind`
/// child whose text is `"fragment"`); there is no `fragment_definition` kind.
///
/// The ORIGINAL input is parsed (not a trimmed copy) so that a valid document
/// is returned byte-for-byte unchanged (trailing newlines survive). When a
/// definition-terminator `.` is missing, it is inserted BEFORE trailing
/// whitespace so the original suffix (newline, comment) survives.
pub fn normalize_inline_pattern(input: &str) -> String {
    let parser_source = parser_text(input);
    let mut parser = Parser::new();
    let Ok(()) = parser.set_language(&LANGUAGE.into()) else {
        // Grammar load failure: fall back to the body form so callers still get
        // a compileable (if wrong) input rather than a panic.
        return wrap_body(input.trim());
    };
    let tree = match parser.parse(parser_source.as_ref(), None) {
        Some(tree) => tree,
        None => return wrap_body(input.trim()),
    };

    if has_definition_child(tree.root_node()) {
        if has_missing_dot(tree.root_node()) {
            insert_terminator_before_trailing_ws(input)
        } else {
            // Valid document: return the ORIGINAL input unchanged so trailing
            // newlines/whitespace survive (a trimmed copy would drop them).
            input.to_string()
        }
    } else {
        wrap_body(input.trim())
    }
}

/// Insert a `.` before trailing whitespace, preserving the original suffix
/// (newlines, trailing spaces, trailing comments). Operates on the trimmed
/// end to stay UTF-8-safe at char boundaries.
fn insert_terminator_before_trailing_ws(input: &str) -> String {
    let end = input.trim_end().len();
    let mut out = String::with_capacity(input.len() + 1);
    // `trim_end().len()` is the byte length of `input` with trailing whitespace
    // chars removed, which ends on a char boundary.
    #[expect(
        clippy::string_slice,
        reason = "`trim_end().len()` ends a whole trimmed str, a char boundary"
    )]
    let head = &input[..end];
    out.push_str(head);
    if !out.ends_with('.') {
        out.push('.');
    }
    // Same `trim_end().len()` char boundary as `head` above.
    #[expect(
        clippy::string_slice,
        reason = "same `trim_end().len()` char boundary as `head` above"
    )]
    let tail = &input[end..];
    out.push_str(tail);
    out
}

fn wrap_body(trimmed: &str) -> String {
    let mut out = String::with_capacity(trimmed.len() + 12);
    out.push_str("inline := ");
    out.push_str(trimmed);
    if !trimmed.ends_with('.') {
        out.push('.');
    }
    out
}

/// A document contains a `pattern_definition` as a direct child of
/// `source_file`. NOTE: both normal and `fragment` forms are
/// `pattern_definition` (a fragment carries a `definition_kind` child whose
/// text is `"fragment"`); there is no `fragment_definition` kind, so match
/// `pattern_definition` only. The metadata block that once nested definitions
/// under a `begin`…`end` region is gone, and `source_file` is the grammar's
/// only producer of `pattern_definition`, so a direct-child scan is complete.
fn has_definition_child(root: Node) -> bool {
    let mut cursor = root.walk();
    root.children(&mut cursor)
        .any(|child| child.kind() == "pattern_definition")
}

fn has_missing_dot(node: Node) -> bool {
    if node.is_missing() && node.kind() == "." {
        return true;
    }
    let mut cursor = node.walk();
    node.children(&mut cursor).any(has_missing_dot)
}

#[cfg(test)]
mod normalize_tests {
    use super::normalize_inline_pattern;

    #[test]
    fn bare_body_gets_header_and_terminator() {
        assert_eq!(
            normalize_inline_pattern("[text=を]"),
            "inline := [text=を]."
        );
    }

    #[test]
    fn bare_body_with_terminator_gets_header_only() {
        assert_eq!(
            normalize_inline_pattern("[text=を]."),
            "inline := [text=を]."
        );
    }

    #[test]
    fn bare_body_with_trailing_newline_is_trimmed_to_body() {
        // The body branch trims surrounding whitespace before wrapping, so a
        // trailing newline on a body is dropped (documents preserve it; bodies
        // do not).
        assert_eq!(
            normalize_inline_pattern("[text=を]\n"),
            "inline := [text=を]."
        );
    }

    #[test]
    fn multi_token_body() {
        assert_eq!(
            normalize_inline_pattern("[upos=VERB] [upos=AUX]+"),
            "inline := [upos=VERB] [upos=AUX]+.",
        );
    }

    #[test]
    fn full_definition_passes_through_byte_for_byte() {
        // Valid document: ORIGINAL input returned unchanged (not trimmed).
        assert_eq!(
            normalize_inline_pattern("p := [text=を]."),
            "p := [text=を]."
        );
    }

    #[test]
    fn full_definition_preserves_trailing_newline() {
        // Regression: rev 2 trimmed this and dropped the trailing \n.
        assert_eq!(
            normalize_inline_pattern("p := [text=を].\n"),
            "p := [text=を].\n"
        );
    }

    #[test]
    fn full_definition_preserves_leading_whitespace() {
        assert_eq!(
            normalize_inline_pattern("  p := [text=を]."),
            "  p := [text=を]."
        );
    }

    #[test]
    fn full_definition_missing_terminator_inserts_dot_before_trailing_newline() {
        // Insertion BEFORE trailing whitespace; newline survives after `.`.
        assert_eq!(
            normalize_inline_pattern("p := [text=を]\n"),
            "p := [text=を].\n"
        );
    }

    #[test]
    fn fragment_leading_document_is_not_wrapped() {
        // Real catalog shape (patterns/sfe.lx:9). Must NOT become `inline := fragment …`.
        let src = "fragment 動詞終止形 := [upos in {VERB, AUX}]+.";
        assert_eq!(normalize_inline_pattern(src), src);
    }

    #[test]
    fn fragment_missing_terminator_keeps_comment_after_inserted_dot() {
        let src = "fragment X := [upos=VERB] # note";
        assert_eq!(
            normalize_inline_pattern(src),
            "fragment X := [upos=VERB] # note."
        );
    }

    #[test]
    fn comment_leading_document_is_not_wrapped() {
        let src = "# header\np := [upos=VERB].";
        assert_eq!(normalize_inline_pattern(src), src);
    }

    #[test]
    fn annotated_document_returned_unchanged_including_trailing_newline() {
        let src = "category: \"x\"\np := [upos=VERB].\n";
        assert_eq!(normalize_inline_pattern(src), src);
    }

    #[test]
    fn one_character_definition_name_is_a_document() {
        // Regression: a hand-rolled `saw_ident` detector skipped the first char
        // before setting its flag, missing single-char identifiers.
        assert_eq!(
            normalize_inline_pattern("a := [upos=VERB]."),
            "a := [upos=VERB]."
        );
    }

    #[test]
    fn one_character_definition_missing_terminator_inserts_dot_preserving_suffix() {
        assert_eq!(
            normalize_inline_pattern("a := [upos=VERB]\n"),
            "a := [upos=VERB].\n"
        );
    }

    #[test]
    fn cjk_identifier_definition_is_a_document() {
        assert_eq!(
            normalize_inline_pattern("がある := [upos=VERB]."),
            "がある := [upos=VERB]."
        );
    }

    #[test]
    fn regex_body_with_an_operator_is_a_body() {
        // A body containing an attribute operator and `/` but no definition
        // header. Classification is structural, so the operator is irrelevant.
        assert_eq!(
            normalize_inline_pattern("[text~/^(a)$/]"),
            "inline := [text~/^(a)$/].",
        );
    }

    #[test]
    fn multiline_document_intact() {
        let src = "a := [upos=VERB].\nb := [upos=NOUN].\n";
        assert_eq!(normalize_inline_pattern(src), src);
    }

    #[test]
    fn multiline_document_missing_final_terminator_inserts_dot_before_newline() {
        assert_eq!(
            normalize_inline_pattern("a := [upos=VERB].\nb := [upos=NOUN]\n"),
            "a := [upos=VERB].\nb := [upos=NOUN].\n",
        );
    }

    #[test]
    fn surrounding_whitespace_trimmed_for_body_only() {
        // Body branch trims (it constructs `inline := <trimmed>.`); only the
        // body form trims — documents above preserve surrounding whitespace.
        assert_eq!(
            normalize_inline_pattern("  [text=を]  \n"),
            "inline := [text=を]."
        );
    }

    #[test]
    fn empty_input_yields_empty_inline_definition() {
        assert_eq!(normalize_inline_pattern("   "), "inline := .");
    }
}
