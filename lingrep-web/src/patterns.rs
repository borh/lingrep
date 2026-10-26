use lingrep_core::fsm::{
    CompileError, CompileLimits, CompileOutput, DefinitionExpansion, ExpandError, FormatError,
    FormatOutcome, PatternSource, compile_catalog, expand_pattern_definition,
    format_pattern_source, render_definition_diagram, render_pattern_error,
};

pub const WEB_PATTERN_SOURCE_NAME: &str = "lingrep-web.lx";

pub fn compile_web_pattern(source: &str) -> Result<CompileOutput, CompileError> {
    compile_catalog(
        PatternSource::named(WEB_PATTERN_SOURCE_NAME, source),
        CompileLimits::default(),
    )
}

pub fn format_web_pattern(source: &str) -> Result<FormatOutcome, FormatError> {
    format_pattern_source(
        PatternSource::named(WEB_PATTERN_SOURCE_NAME, source),
        CompileLimits::default(),
    )
}

/// Resolve one authored definition of `source` into a self-contained one, by
/// name.
///
/// The source is the catalog a completed run was produced from, not the live
/// editor, so what this describes is the pattern the results on screen came
/// from rather than whatever has been typed since.
pub fn expand_web_pattern(
    source: &str,
    name: &str,
) -> Result<Option<DefinitionExpansion>, ExpandError> {
    expand_pattern_definition(
        PatternSource::named(WEB_PATTERN_SOURCE_NAME, source),
        CompileLimits::default(),
        name,
    )
}

/// Expand a derived compound item from the names of its components, in chain
/// order.
///
/// A compound has no authored declaration; each component is expanded and the
/// results are concatenated so its declarations, full expansions, references,
/// and railroad diagram are shown together under `name`. `Ok(None)` when any
/// component is not a definition of `source`.
pub fn expand_compound_pattern(
    source: &str,
    name: &str,
    component_names: &[&str],
) -> Result<Option<DefinitionExpansion>, ExpandError> {
    let mut declarations = Vec::with_capacity(component_names.len());
    let mut expanded_parts = Vec::with_capacity(component_names.len());
    let mut any_expanded = false;
    let mut references: Vec<String> = Vec::new();

    for component in component_names {
        let Some(exp) = expand_pattern_definition(
            PatternSource::named(WEB_PATTERN_SOURCE_NAME, source),
            CompileLimits::default(),
            component,
        )?
        else {
            return Ok(None);
        };
        if let Some(expanded) = &exp.expanded {
            any_expanded = true;
            expanded_parts.push(expanded.clone());
        } else {
            expanded_parts.push(exp.declaration.clone());
        }
        declarations.push(exp.declaration);
        for reference in &exp.references {
            if !references.contains(reference) {
                references.push(reference.clone());
            }
        }
    }

    let declaration = declarations.join("\n");
    let joined_expanded = expanded_parts.join("\n");
    let diagram = render_definition_diagram(&joined_expanded);

    Ok(Some(DefinitionExpansion {
        name: name.to_owned(),
        kind: lingrep_core::fsm::ExpandedDefinitionKind::Pattern,
        declaration,
        expanded: any_expanded.then_some(joined_expanded),
        references: references.into_boxed_slice(),
        diagram,
    }))
}

/// Render a compile error without a source excerpt or caret.
///
/// This is not redaction: diagnostics may contain author-chosen identifiers.
/// The response returns only to the submitting request and the browser renders
/// it through `textContent`. An audit log, shared session, or HTML rendering
/// would require a new trust-boundary review.
///
/// `search_invalid_regex_never_formats_submitted_secret_or_source` below pins
/// the narrower promise that regex contents and the complete source stay out
/// of this rendering.
pub(crate) fn format_pattern_compile_error(error: &CompileError) -> String {
    render_pattern_error(None, error)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostics_use_one_web_source_identity() {
        let error = match compile_web_pattern("x := @missing.") {
            Ok(_) => panic!("undefined reference must fail"),
            Err(error) => error,
        };
        assert!(
            error
                .diagnostics
                .iter()
                .all(|diagnostic| diagnostic.location().source_name() == WEB_PATTERN_SOURCE_NAME)
        );
    }

    fn formatted_compile_error(source_name: &str, source: &str) -> String {
        let error = compile_catalog(
            PatternSource::named(source_name, source),
            CompileLimits::default(),
        )
        .err()
        .expect("fixture must fail compilation");
        format_pattern_compile_error(&error)
    }

    #[test]
    fn default_catalog_publishes_the_longer_authored_match_before_compound_derivation() {
        use lingrep_core::{fsm::Match, token_match::TokenInfo};

        let compiled =
            compile_web_pattern(include_str!("../../patterns/sentence-final.lx")).unwrap();
        let tokens = vec![
            TokenInfo::default().with_orth("と"),
            TokenInfo::default().with_orth("考え").with_lemma("考える"),
            TokenInfo::default()
                .with_orth("られる")
                .with_lemma("られる"),
        ];
        let published = compiled.matcher.find_published_matches(&tokens).unwrap();
        assert_eq!(
            published,
            vec![Match {
                pattern: "と考えられる".into(),
                start: 0,
                end: 3,
                captures: Box::default(),
                graph_bindings: Box::default(),
                lexical: None,
            }]
        );
    }

    #[test]
    fn search_invalid_regex_never_formats_submitted_secret_or_source() {
        const SECRET: &str = "SEARCH_REGEX_SECRET_62E8";
        let source = format!("秘密 := [text~/({SECRET}/].");
        let rendered = formatted_compile_error("search.lx", &source);

        assert!(rendered.contains("[InvalidRegex] search.lx:1:"));
        assert!(!rendered.contains(SECRET), "{rendered}");
        assert!(!rendered.contains(&source), "{rendered}");
    }

    /// The other half of the redaction boundary, kept honest in both
    /// directions: an author-chosen identifier DOES reach the response, inside
    /// the message, while the source line it came from does not.
    ///
    /// Asserted rather than merely documented so the boundary cannot move
    /// unnoticed. If a future change starts redacting names, this fails and the
    /// doc comment on `format_pattern_compile_error` gets revisited with it —
    /// which is the point, because that comment's safety argument rests on who
    /// receives this string and how the browser renders it.
    #[test]
    fn a_definition_name_reaches_the_message_but_its_source_line_does_not() {
        const NAME: &str = "DEFINITION_NAME_4B71";
        let source = format!("{NAME} := 猫.\n{NAME} := 犬.");
        let rendered = formatted_compile_error("search.lx", &source);

        assert!(rendered.contains(NAME), "{rendered}");
        assert!(!rendered.contains("猫"), "{rendered}");
        assert!(!rendered.contains("犬"), "{rendered}");
        assert!(!rendered.contains(&source), "{rendered}");
    }

    /// The display name is a label: components come from the caller's list,
    /// so a name with no parseable structure expands all the same.
    #[test]
    fn compound_expansion_resolves_components_from_the_supplied_names_only() {
        let source = "a := [text=甲].\nb := [text=乙].";
        let expansion = expand_compound_pattern(source, "unrelated label", &["b", "a"])
            .expect("the catalog compiles")
            .expect("both components are definitions");
        assert_eq!(expansion.name, "unrelated label");
        assert_eq!(expansion.declaration, "b := [text=乙].\na := [text=甲].");
        assert!(
            expand_compound_pattern(source, "複合項目〔a ＋ c〕", &["a", "c"])
                .unwrap()
                .is_none(),
            "an unknown component yields no expansion"
        );
    }

    #[test]
    fn search_semantic_diagnostic_formats_multiline_unicode_location() {
        let source = "先頭 := 猫.\n対象 :=\n  猫 @未定.";
        let rendered = formatted_compile_error("search.lx", source);

        assert!(
            rendered.contains("[UndefinedReference] search.lx:3:7:"),
            "{rendered}"
        );
        assert!(!rendered.contains(source), "{rendered}");
    }
}
