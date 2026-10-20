//! Shared lingrep patterns compile-error renderer with one coordinate contract.
//!
//! Rich form (source supplied): header `line:col` AND caret use Unicode
//! display width; tabs are expanded to a single space in the rendered
//! snippet (and column arithmetic) so the caret aligns. Headless form
//! (source `None`): uses the compiler's byte column, no caret, and it omits
//! the source excerpt.
//!
//! Omitting the excerpt is not redaction: diagnostic messages are formatted
//! from source-derived text (`bind.rs` builds `duplicate definition {name}`
//! from the user's own definition name), so the headless form can still echo
//! caller-supplied identifiers.

use super::diagnostic::{CompileError, Diagnostic, DiagnosticLocation, DiagnosticSeverity};
use unicode_width::UnicodeWidthChar;

/// Format a slice of diagnostics into a single string.
///
/// Takes a slice rather than an [`CompileError`] because non-fatal
/// diagnostics arrive as a plain `Vec` on `CompileOutput`, and
/// `CompileError`'s constructor is crate-private — so a caller holding a
/// successful compile's diagnostics had no way to render them.
///
/// `source: Some(text)` → a rustc-style snippet using Unicode display width
/// (tabs expanded to one space so the caret aligns). `source: None` → a
/// headless one-line form using the compiler's byte column.
///
/// **`None` omits the source excerpt; it is not redaction.** Diagnostic
/// messages can embed source-derived text — `bind.rs` formats
/// `duplicate definition {name}` from the user's own definition name — so the
/// no-source form can still echo caller-supplied identifiers.
pub fn render_diagnostics(source: Option<&str>, diagnostics: &[Diagnostic]) -> String {
    diagnostics
        .iter()
        .map(|d| render_one(source, d))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Format lingrep patterns compile errors into a single string.
///
/// A thin wrapper over [`render_diagnostics`]; see it for the source-excerpt
/// contract.
pub fn render_pattern_error(source: Option<&str>, error: &CompileError) -> String {
    render_diagnostics(source, &error.diagnostics)
}

/// Render a diagnostic's related locations as `note:` lines.
///
fn render_notes(source: Option<&str>, d: &Diagnostic) -> String {
    let primary = d.location();
    d.related_locations()
        .iter()
        .map(|related| {
            let loc = related.location();
            format!(
                "\n  note: {}\n  --> {}:{}:{}",
                related.message(),
                loc.source_name(),
                loc.line(),
                related_column(source, primary, loc),
            )
        })
        .collect()
}

/// The column to print for a related location.
///
/// The rich form uses display-width columns, while a related location's stored
/// `column()` is byte-based.
///
/// The renderer holds ONE `&str`, so it can only measure a related location
/// that lives in the same source as the primary. When the names differ there is
/// no text to measure against and the byte column is printed as-is. That is a
/// documented fallback: a caller rendering diagnostics that span sources gets a
/// coordinate it can still resolve, not a confidently wrong one.
fn related_column(
    source: Option<&str>,
    primary: &DiagnosticLocation,
    related: &DiagnosticLocation,
) -> usize {
    let Some(text) = source else {
        return related.column();
    };
    if related.source_name() != primary.source_name() {
        return related.column();
    }
    snippet_and_caret(text, related)
        .map_or_else(|| related.column(), |(_, display_col, _)| display_col)
}

fn render_one(source: Option<&str>, d: &Diagnostic) -> String {
    let loc = d.location();
    let notes = render_notes(source, d);
    let headerless = format!(
        "[{}] {}:{}:{}: {}",
        d.code(),
        loc.source_name(),
        loc.line(),
        loc.column(),
        d.message
    );
    let Some(text) = source else {
        return headerless + &notes;
    };
    let (snippet, display_col, caret_len) = match snippet_and_caret(text, loc) {
        Some(x) => x,
        None => return headerless + &notes,
    };
    let gutter = loc.line().to_string();
    let blank = " ".repeat(gutter.len());
    // The rich form writes the word `error`, so it must not be hardcoded: a
    // `Warning` would otherwise announce itself as fatal to every consumer,
    // including `check`'s exit code. The headless form says only `[{code}]`
    // and needs no such distinction.
    let label = match d.severity() {
        DiagnosticSeverity::Error => "error",
        DiagnosticSeverity::Warning => "warning",
    };
    format!(
        "{label}[{code}]: {msg}\n  --> {name}:{line}:{col}\n {gutter} | {snippet}\n {blank} | {pad}{carets}{notes}",
        label = label,
        code = d.code(),
        msg = d.message,
        name = loc.source_name(),
        line = loc.line(),
        col = display_col,
        gutter = gutter,
        blank = blank,
        snippet = snippet,
        pad = " ".repeat(display_col.saturating_sub(1)),
        carets = "^".repeat(caret_len),
        notes = notes,
    )
}

/// Return the rendered line, one-based caret column, and caret display width.
fn snippet_and_caret(text: &str, loc: &DiagnosticLocation) -> Option<(String, usize, usize)> {
    // Use the original source for line/byte mapping: compiler locations count
    // `\n` bytes in the original source (see `lower.rs::source_line_column`).
    // `parser_text` normalizes CRLF by replacing `\r` with `\n`, LEAVING the
    // following `\n` — so it injects a phantom extra newline per CRLF. Splitting
    // the original source by `\n` therefore counts the same lines the compiler
    // did; a trailing `\r` is stripped from the displayed line only.
    let line_start = line_start_byte(text, loc.line());
    // `line_start` from `line_start_byte` is byte 0 or the byte just after a
    // `\n`, both char boundaries.
    #[expect(
        clippy::string_slice,
        reason = "`line_start` is a line-start byte (0 or just past ASCII newline)"
    )]
    let line_end = text[line_start..]
        .find('\n')
        .map_or(text.len(), |i| line_start + i);
    // Strip a trailing CR (CRLF) for display; the CR is not a `\n`, so it does
    // not affect line counting above.
    #[expect(
        clippy::string_slice,
        reason = "`line_start`/`line_end` are char boundaries"
    )]
    let raw_line = text[line_start..line_end].trim_end_matches('\r');
    let mut snippet = String::with_capacity(raw_line.len());
    for c in raw_line.chars() {
        snippet.push(if c == '\t' { ' ' } else { c });
    }
    let range = loc.byte_range();
    let rel_start = range.start.saturating_sub(line_start);
    let rel_end = range.end.saturating_sub(line_start).min(raw_line.len());
    let display_col = byte_to_display_col(raw_line, rel_start);
    let end_col = byte_to_display_col(raw_line, rel_end);
    let caret_len = end_col.saturating_sub(display_col).max(1);
    Some((snippet, display_col, caret_len))
}

fn line_start_byte(text: &str, line_one_based: usize) -> usize {
    if line_one_based <= 1 {
        // Line 1 starts at byte 0; the `nth(line - 2)` formula below would
        // wrongly pick the first `\n` for line 1 (saturating_sub turns -1
        // into 0). Short-circuit.
        return 0;
    }
    text.match_indices('\n')
        .nth(line_one_based - 2)
        .map_or(0, |(i, _)| i + 1)
}

/// Map a byte offset within a line to a 1-based display-width column. Tabs
/// count as width 1 (matching the snippet's tab→space expansion).
fn byte_to_display_col(line: &str, byte_off: usize) -> usize {
    let mut col = 1usize;
    let mut bytes = 0usize;
    for c in line.chars() {
        if bytes >= byte_off {
            break;
        }
        bytes += c.len_utf8();
        col += char_display_width(c);
    }
    col
}

fn char_display_width(c: char) -> usize {
    if c == '\t' { 1 } else { c.width().unwrap_or(1) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fsm::compiler::diagnostic::{
        CompileError, Diagnostic, DiagnosticCode, DiagnosticLocation, RelatedDiagnosticLocation,
    };
    use crate::fsm::{CompileLimits, PatternSource, compile_catalog};

    fn diag(
        name: &str,
        range: std::ops::Range<usize>,
        msg: &str,
        line: usize,
        col: usize,
    ) -> CompileError {
        CompileError::from_diagnostics(vec![Diagnostic::error(
            DiagnosticCode::Syntax,
            name,
            range,
            msg,
            line,
            col,
        )])
    }

    fn with_note(
        primary_name: &str,
        primary_range: std::ops::Range<usize>,
        related_name: &str,
        related_range: std::ops::Range<usize>,
        related_col: usize,
    ) -> CompileError {
        let diagnostic = Diagnostic::error(
            DiagnosticCode::DuplicateDefinition,
            primary_name,
            primary_range,
            "duplicate definition p",
            2,
            1,
        )
        .with_related_location(RelatedDiagnosticLocation::new(
            DiagnosticLocation::new(related_name, related_range, 1, related_col),
            "first definition is here",
        ));
        CompileError::from_diagnostics(vec![diagnostic])
    }

    #[test]
    fn rich_form_uses_display_width_for_unicode_and_aligns_caret() {
        // を is East-Asian-Wide (display width 2). EOF caret at byte 20 lands
        // on display column 20 (not 19). 19 prefix spaces then `^`.
        let src = "inline := [text=を]";
        let err = diag("inline.lx", 20..20, "missing `.`", 1, 21);
        let out = render_pattern_error(Some(src), &err);
        assert!(out.contains("error[Syntax]: missing `.`"), "{out}");
        assert!(
            out.contains("--> inline.lx:1:20"),
            "display col 20, got:\n{out}"
        );
        assert!(out.contains("1 | inline := [text=を]"), "{out}");
        assert!(
            out.contains("                   ^"),
            "19 spaces + caret at col 20, got:\n{out}"
        );
    }

    #[test]
    fn headless_form_uses_byte_column_and_no_source() {
        // Headless keeps the compiler's BYTE column (21), no caret, no source.
        let err = diag("inline.lx", 20..20, "missing `.`", 1, 21);
        let out = render_pattern_error(None, &err);
        assert_eq!(out, "[Syntax] inline.lx:1:21: missing `.`");
        assert!(!out.contains("inline := [text=を]"));
    }

    #[test]
    fn render_diagnostics_accepts_a_slice_without_an_error_wrapper() {
        // `CompileOutput::diagnostics` is a plain Vec on a SUCCESSFUL compile.
        // Rendering it must not require constructing an CompileError, whose
        // constructor is crate-private. Non-fatal diagnostics therefore need
        // the slice-based renderer.
        let diagnostics = vec![Diagnostic::error(
            DiagnosticCode::Syntax,
            "a.lx",
            0..1,
            "boom",
            1,
            1,
        )];
        assert_eq!(
            render_diagnostics(None, &diagnostics),
            "[Syntax] a.lx:1:1: boom"
        );
    }

    #[test]
    fn a_warning_does_not_render_as_an_error() {
        let warning = vec![Diagnostic::warning(
            DiagnosticCode::Syntax,
            "a.lx",
            0..1,
            "suspicious",
            1,
            1,
        )];
        let out = render_diagnostics(Some("p := [].\n"), &warning);
        assert!(out.contains("warning[Syntax]: suspicious"), "{out}");
        assert!(
            !out.contains("error["),
            "a warning must not claim to be an error: {out}"
        );
    }

    #[test]
    fn ascii_range_renders_one_caret_per_byte() {
        // ASCII span bytes 0..7 → 7 carets.
        let src = "p := [or";
        let err = diag("p.lx", 0..7, "invalid lingrep patterns syntax", 1, 1);
        let out = render_pattern_error(Some(src), &err);
        assert!(out.contains("^^^^^^^"), "{out}");
    }

    #[test]
    fn unicode_range_uses_display_width_two_for_wide_chars() {
        // を spans bytes 6..9 in `[text=を]`: display cols 7..9 → TWO carets.
        let src = "[text=を]";
        let err = diag("x.lx", 6..9, "bad", 1, 7);
        let out = render_pattern_error(Some(src), &err);
        assert!(
            out.contains("      ^^"),
            "two carets over を (display width 2), got:\n{out}"
        );
    }

    #[test]
    fn tabs_are_expanded_to_one_space_in_snippet_and_caret() {
        // `\t[text=を]` → snippet shows ` [text=を]` (tab→space); `[` was at
        // byte 1, display col 2; caret prefix = 1 space.
        let src = "\t[text=を]";
        let err = diag("t.lx", 1..2, "bad", 1, 2);
        let out = render_pattern_error(Some(src), &err);
        assert!(
            out.contains("1 |  [text=を]"),
            "tab expanded to one space, got:\n{out}"
        );
        assert!(
            out.contains("  |  ^"),
            "caret aligned under `[` at display col 2, got:\n{out}"
        );
    }

    #[test]
    fn crlf_input_strips_trailing_cr_in_snippet() {
        // `p := [text=を]` is 15 bytes; missing `.` at byte 15 (before CRLF).
        // Original line 1 starts at byte 0; the `\r` at byte 15 is stripped
        // from the displayed line only.
        let src = "p := [text=を]\r\n";
        let err = diag("crlf.lx", 15..15, "missing `.`", 1, 16);
        let out = render_pattern_error(Some(src), &err);
        assert!(
            out.contains("1 | p := [text=を]"),
            "no stray CR, got:\n{out}"
        );
        assert!(
            out.contains("              ^"),
            "14 prefix spaces + caret at EOF col 15, got:\n{out}"
        );
    }

    #[test]
    fn crlf_two_line_doc_renders_original_second_line_not_phantom() {
        // Regression: `parser_text` replaces CR with LF but leaves the following
        // LF, producing two `\n` per CRLF. The compiler counts `\n` in the
        // ORIGINAL source (see `lower.rs::source_line_column`), so a diagnostic
        // on original line 2 must render line 2's text — not an empty phantom
        // line 2 followed by the real content on line 3. The renderer splits
        // the ORIGINAL source to match.
        //
        // Source bytes: `a := [upos=VERB].` (0..16, 16 bytes), `\r`@16, `\n`@17,
        // then `b := [text=]` at bytes 18..30 (line 2). `[text=]` spans
        // bytes 23..30; its `[` is at byte 23 → original column 5.
        let src = "a := [upos=VERB].\r\nb := [text=]";
        let err = diag("crlf2.lx", 23..30, "invalid lingrep patterns syntax", 2, 5);
        let out = render_pattern_error(Some(src), &err);
        assert!(
            out.contains("--> crlf2.lx:2:5"),
            "line 2 header, got:\n{out}"
        );
        assert!(
            out.contains("2 | b := [text=]"),
            "line-2 text rendered, not a phantom empty line, got:\n{out}"
        );
        assert!(
            out.contains("     ^^^^^^^"),
            "7 carets over `[text=]` (bytes 23..30), got:\n{out}"
        );
    }

    #[test]
    fn eof_zero_width_range_renders_single_caret_at_eof() {
        // `p := [text=を]` is 15 bytes; EOF caret at byte 15..15, display col 15.
        let src = "p := [text=を]";
        let err = diag("eof.lx", 15..15, "missing `.`", 1, 16);
        let out = render_pattern_error(Some(src), &err);
        assert!(
            out.contains("              ^"),
            "14 spaces + caret at EOF col 15, got:\n{out}"
        );
    }

    #[test]
    fn multiline_uses_absolute_byte_offset_into_second_line() {
        // `先頭 := 猫.\n  猫 @未定.`: `@未定` is bytes 21..28 on line 2.
        // Line2 `  猫 @未定.`: spaces(2) + 猫(2) = 4 cols, ` ` = col5,
        // `@` at col6. `@未定` = @ + 未 + 定 = 5 display cols.
        let src = "先頭 := 猫.\n  猫 @未定.";
        let err = diag("multi.lx", 21..28, "undefined @未定", 2, 7);
        let out = render_pattern_error(Some(src), &err);
        assert!(
            out.contains("--> multi.lx:2:6"),
            "@ at display col 6, got:\n{out}"
        );
        assert!(out.contains("2 |   猫 @未定."), "{out}");
        assert!(
            out.contains("     ^^^^^"),
            "caret covering @未定 (5 display cols), got:\n{out}"
        );
    }

    #[test]
    fn multiple_diagnostics_join_with_newlines() {
        let src = "a := [text=]\nb := [or";
        let err = CompileError::from_diagnostics(vec![
            Diagnostic::error(DiagnosticCode::Syntax, "a.lx", 0..7, "first", 1, 1),
            Diagnostic::error(DiagnosticCode::Syntax, "a.lx", 11..18, "second", 2, 1),
        ]);
        let out = render_pattern_error(Some(src), &err);
        assert_eq!(out.matches("error[Syntax]").count(), 2);
    }

    #[test]
    fn rich_form_renders_the_related_location_the_compiler_already_computed() {
        let src = "p := [].\np := [].\n";
        let out = render_pattern_error(Some(src), &with_note("dup.lx", 9..10, "dup.lx", 0..1, 1));

        assert!(out.contains("error[DuplicateDefinition]"), "{out}");
        assert!(out.contains("note: first definition is here"), "{out}");
        assert!(
            out.contains("--> dup.lx:1:1"),
            "the note's own location: {out}"
        );
    }

    #[test]
    fn a_related_location_after_wide_text_uses_the_rich_display_column() {
        // `研究` is two East-Asian-Wide chars: 6 bytes, 4 display columns. A
        // related location at byte 7 has BYTE column 8 but DISPLAY column 6.
        // The rich form promises display columns for the primary location, so a
        // note printed beside it must use the same convention or the diagnostic
        // contradicts itself.
        let src = "研究 := [].\np := [].\n";
        let out = render_pattern_error(Some(src), &with_note("w.lx", 16..17, "w.lx", 7..8, 8));

        assert!(
            out.contains("--> w.lx:1:6"),
            "display column expected:\n{out}"
        );
        assert!(
            !out.contains("w.lx:1:8"),
            "byte column leaked into the rich form:\n{out}"
        );
    }

    #[test]
    fn a_related_location_in_another_source_falls_back_to_the_byte_column() {
        // The renderer holds ONE `&str`. It cannot measure display width in a
        // file it was not given, so it prints the compiler's byte column. A
        // documented fallback, not a silent wrong answer.
        let src = "p := [].\n";
        let out = render_pattern_error(Some(src), &with_note("a.lx", 0..1, "other.lx", 7..8, 8));

        assert!(out.contains("--> other.lx:1:8"), "{out}");
    }

    #[test]
    fn a_diagnostic_without_a_related_location_is_byte_identical() {
        // Guards the exact-equality contract lingrep-web's output depends on.
        let err = diag("inline.lx", 20..20, "missing `.`", 1, 21);
        assert_eq!(
            render_pattern_error(None, &err),
            "[Syntax] inline.lx:1:21: missing `.`"
        );
    }

    #[test]
    fn omitting_the_source_omits_the_excerpt_but_not_the_definition_name() {
        // Routed through the real compiler so the assertion cannot be vacuous.
        //
        // This pins the LIMIT of the no-source form, not a redaction guarantee:
        // `bind.rs` formats `duplicate definition {name}` from the user's own
        // identifier, so a name-shaped secret reaches even this output. The
        // source LINE is what gets omitted.
        const NAME: &str = "SECRET_NAME_7C3";
        let source = format!("{NAME} := [].\n{NAME} := [].\n");
        // `CompileOutput` is not `Debug`, so `expect_err` is unavailable.
        let Err(error) = compile_catalog(
            PatternSource::named("s.lx", &source),
            CompileLimits::default(),
        ) else {
            panic!("a duplicate definition must fail to compile");
        };

        let out = render_pattern_error(None, &error);
        assert!(
            out.contains(NAME),
            "the message carries the definition name — omission is not redaction: {out}"
        );
    }

    #[test]
    fn omitting_the_source_omits_the_source_line() {
        // The property the parameter actually controls, again routed through
        // the compiler so a real path from the value to the renderer exists.
        const VALUE: &str = "VALUE_ONLY_IN_SOURCE_5A9";
        let source = format!("p := [text={VALUE}\n");
        // `CompileOutput` is not `Debug`, so `expect_err` is unavailable.
        let Err(error) = compile_catalog(
            PatternSource::named("s.lx", &source),
            CompileLimits::default(),
        ) else {
            panic!("an unterminated predicate must fail to compile");
        };

        let out = render_pattern_error(None, &error);
        assert!(!out.contains(VALUE), "no source excerpt may appear: {out}");
        assert!(
            render_pattern_error(Some(&source), &error).contains(VALUE),
            "the rich form does echo the source line, by design"
        );
    }
}
