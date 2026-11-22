//! Diagnostics for the catalog ↔ corpus binding seam.
//!
//! Binding a lingrep patterns catalog against a token schema can end three ways: a
//! clean bind, a bind that carries referenced-but-unavailable attributes
//! ([`lingrep_core::fsm::BoundCatalog::unavailable_attributes`]), or an error when a
//! dependency `where` clause's edge attributes are missing (or a key's value
//! kind conflicts). This module turns the second into per-attribute
//! `warning[attribute_unavailable]` notices naming the referencing
//! definitions and the token schema, and the third into an invalid-invocation
//! error explaining what to change, rather than reporting a setup failure as
//! a read failure.

use lingrep_core::fsm::{
    BoundCatalog, CatalogAnalysis, CompileOutput, DiagnosticCode, SchemaBindError,
};
use lingrep_core::token_match::{AttributeKey, TokenSchema};

use crate::cli::ExecutionCliConfig;
use crate::error::CliError;
use crate::protocol::diagnostic::DiagnosticStage;

/// How many referencing definitions one notice names before eliding.
const NAMED_REFERRERS: usize = 3;

/// Wraps a bind error as an invalid invocation.
///
/// `schema` is the human name of the token schema, e.g. `"vibrato
/// morphology"` or `"pretokenized syntax"` — the recorded analyzer family and
/// extraction profile, which is what
/// [`analyzer_name`] builds. The messages attribute the error to the **schema** and not
/// "the analyzer": which attributes a morphology schema carries is determined by
/// the dictionary supplying the feature columns, so a tokenizer that reads a
/// different dictionary is a different schema and the same analyzer.
pub(crate) fn bind_refusal(error: &SchemaBindError, schema: &str) -> CliError {
    CliError::InvalidArgs(format!(
        "the catalog cannot bind to this corpus's token schema ({schema}): {error}"
    ))
}

/// One `attribute_unavailable` message per unavailable attribute, in the
/// bound catalog's sorted order.
pub(crate) fn unavailable_attribute_messages(
    unavailable: &[AttributeKey],
    analysis: &CatalogAnalysis,
    schema: &str,
) -> Vec<String> {
    unavailable
        .iter()
        .map(|attribute| {
            let referrers: Vec<String> = analysis
                .references_to(attribute)
                .map(|site| {
                    format!(
                        "`{}` ({}:{})",
                        site.definition, site.source_name, site.declaration_line
                    )
                })
                .collect();
            let definition_reference = !referrers.is_empty();
            let referenced_by = if referrers.is_empty() {
                "the catalog configuration".to_owned()
            } else if referrers.len() > NAMED_REFERRERS {
                format!(
                    "{}, and {} more",
                    referrers[..NAMED_REFERRERS].join(", "),
                    referrers.len() - NAMED_REFERRERS
                )
            } else {
                referrers.join(", ")
            };
            let consequence = if definition_reference {
                "; positive tests over it match nothing, and only `!(…)` complements hold"
            } else {
                ""
            };
            format!(
                "`{attribute}` is referenced by {referenced_by} but this corpus's token schema \
                 ({schema}) does not carry it{consequence}"
            )
        })
        .collect()
}

/// Writes the notices to stderr, unconditionally — the finding is a silent
/// zero-match in the making, so it is not gated on `--progress`.
pub(crate) fn write_unavailable_attribute_notices(
    bound: &BoundCatalog,
    analysis: &CatalogAnalysis,
    schema: &str,
    config: &ExecutionCliConfig,
) -> Result<(), CliError> {
    for message in unavailable_attribute_messages(bound.unavailable_attributes(), analysis, schema)
    {
        crate::commands::query_support::write_warning(
            &config.stderr,
            config.diagnostic_format,
            DiagnosticStage::Catalog,
            "attribute_unavailable",
            message,
            None,
            Vec::new(),
        )
        .map_err(CliError::Io)?;
    }
    Ok(())
}

/// Writes the compiler's Unicode normalization warnings to stderr,
/// unconditionally: a literal compiled as its NFC form or its folded `text`
/// key, or a regex compiled as written although NFC or the fold would change
/// it, means the run matches something other than the bytes in the catalog.
pub(crate) fn write_normalization_notices(
    compiled: &CompileOutput,
    config: &ExecutionCliConfig,
) -> Result<(), CliError> {
    for diagnostic in &compiled.diagnostics {
        let code = match diagnostic.code() {
            DiagnosticCode::NormalizedLiteral => "literal_normalized",
            DiagnosticCode::RegexNotNfc => "regex_not_nfc",
            DiagnosticCode::FoldedLiteral => "literal_folded",
            DiagnosticCode::RegexNotFolded => "regex_not_folded",
            _ => continue,
        };
        let location = diagnostic.location();
        crate::commands::query_support::write_warning(
            &config.stderr,
            config.diagnostic_format,
            DiagnosticStage::Catalog,
            code,
            format!(
                "{}:{}:{}: {}",
                location.source_name(),
                location.line(),
                location.column(),
                diagnostic.message
            ),
            None,
            Vec::new(),
        )
        .map_err(CliError::Io)?;
    }
    Ok(())
}

/// The `text_unfolded` message when the catalog compares `text` and the
/// corpus's `text` is not the folded key its literals are compiled to.
pub(crate) fn unfolded_text_message(
    schema: &TokenSchema,
    analysis: &CatalogAnalysis,
    schema_name: &str,
) -> Option<String> {
    let text = AttributeKey::new("text").expect("text is canonical");
    if schema.text_is_folded_key() || !schema.keys().contains(&text) {
        return None;
    }
    let referrers: Vec<String> = analysis
        .references_to(&text)
        .map(|site| format!("`{}`", site.definition))
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    if referrers.is_empty() {
        return None;
    }
    let referenced_by = if referrers.len() > NAMED_REFERRERS {
        format!(
            "{}, and {} more",
            referrers[..NAMED_REFERRERS].join(", "),
            referrers.len() - NAMED_REFERRERS
        )
    } else {
        referrers.join(", ")
    };
    let verb = if referrers.len() == 1 {
        "compares"
    } else {
        "compare"
    };
    Some(format!(
        "{referenced_by} {verb} `text` as the folded match key, but this corpus's token \
         schema ({schema_name}) carries `text` unfolded, so a token spelled ＮＨＫ or ｶﾞﾗｽ \
         matches neither `[text=ＮＨＫ]` nor `[text=NHK]`. This corpus was prepared before \
         0.9: rebuild it with this ling (`ling corpus build`)"
    ))
}

/// Writes [`unfolded_text_message`] to stderr, unconditionally, as
/// `write_unavailable_attribute_notices` does.
pub(crate) fn write_unfolded_text_notice(
    schema: &TokenSchema,
    analysis: &CatalogAnalysis,
    schema_name: &str,
    config: &ExecutionCliConfig,
) -> Result<(), CliError> {
    let Some(message) = unfolded_text_message(schema, analysis, schema_name) else {
        return Ok(());
    };
    crate::commands::query_support::write_warning(
        &config.stderr,
        config.diagnostic_format,
        DiagnosticStage::Catalog,
        "text_unfolded",
        message,
        None,
        Vec::new(),
    )
    .map_err(CliError::Io)
}

pub(crate) fn write_unobserved_division_notices(
    kinds: &[compact_str::CompactString],
    observes: impl Fn(&str) -> bool,
    config: &ExecutionCliConfig,
    run_id: Option<&str>,
) -> Result<(), CliError> {
    for kind in kinds.iter().filter(|kind| !observes(kind)) {
        crate::commands::query_support::write_warning(
            &config.stderr,
            config.diagnostic_format,
            DiagnosticStage::Catalog,
            "division_unavailable",
            format!(
                "`within={kind}` lookarounds match within their own sentence: this corpus \
                 observes no `{kind}` divisions"
            ),
            run_id,
            Vec::new(),
        )
        .map_err(CliError::Io)?;
    }
    Ok(())
}

/// Writes one `span_kind_unavailable` warning per span kind that no admitted
/// sentence carried: each kind the catalog's span constraints name, by
/// `report.span_kinds_observed`, and the `--by-span` kind when the flag paired
/// with it is false. Writes nothing for a run that admitted no sentence.
pub(crate) fn write_unobserved_span_kind_notices(
    catalog: &lingrep_core::fsm::CompiledCatalog,
    report: &lingrep_core::execution::ExecutionReport,
    by_span: Option<(&str, bool)>,
    config: &ExecutionCliConfig,
    run_id: &str,
) -> Result<(), CliError> {
    if report.sentences_admitted == 0 {
        return Ok(());
    }
    let constrained = catalog
        .span_constraint_kinds()
        .iter()
        .zip(&report.span_kinds_observed)
        .filter(|(_, observed)| !**observed)
        .map(|((kind, definitions), _)| {
            let definitions: Vec<String> = definitions
                .iter()
                .map(|definition| format!("`{definition}`"))
                .collect();
            let named = if definitions.len() > NAMED_REFERRERS {
                format!(
                    "{}, and {} more",
                    definitions[..NAMED_REFERRERS].join(", "),
                    definitions.len() - NAMED_REFERRERS
                )
            } else {
                definitions.join(", ")
            };
            format!(
                "no sentence of this run carries a `{kind}` span, so nothing matches the span \
                 constraints of {named}"
            )
        });
    let grouped = by_span.filter(|(_, observed)| !observed).map(|(kind, _)| {
        format!(
            "`--by-span {kind}`: no sentence of this run carries a `{kind}` span, so every \
                 record is unkeyed"
        )
    });
    for message in constrained.chain(grouped) {
        crate::commands::query_support::write_warning(
            &config.stderr,
            config.diagnostic_format,
            DiagnosticStage::Matching,
            "span_kind_unavailable",
            message,
            Some(run_id),
            Vec::new(),
        )
        .map_err(CliError::Io)?;
    }
    Ok(())
}

/// The schema name a notice carries, from the artifact's recorded analyzer
/// family and extraction profile.
///
/// This is the closest an artifact gets to naming its schema: an
/// [`AnalyzerRecord`](lingrep_corpus::AnalyzerRecord) carries no dictionary name,
/// only the content digest folded into `resource_manifest_digest`. So
/// `"vibrato morphology"` identifies the layout by the analyzer that produced
/// it rather than by the UniDic release that defined its columns.
pub(crate) fn analyzer_name(record: &lingrep_corpus::AnalyzerRecord) -> String {
    format!("{} {}", record.family, record.profile)
}

const MAX_NAMED_OUT_OF_SET_VALUES: usize = 8;

/// One `label_outside_declared_set` message per declaration for which the
/// corpus has values outside the set, naming the most frequent values with
/// token counts.
pub(crate) fn out_of_set_label_messages(
    declarations: &[lingrep_core::fsm::LabelDeclaration],
    observed: &[lingrep_core::fsm::OutOfSetValues],
) -> Vec<String> {
    declarations
        .iter()
        .zip(observed)
        .filter(|(_, values)| !values.is_empty())
        .map(|(declaration, values)| {
            let mut counted: Vec<(&str, u64)> = values
                .values
                .iter()
                .map(|(value, count)| (value.as_str(), *count))
                .collect();
            counted.sort_by(|left, right| right.1.cmp(&left.1).then(left.0.cmp(right.0)));
            let mut named: Vec<String> = counted
                .iter()
                .take(MAX_NAMED_OUT_OF_SET_VALUES)
                .map(|(value, count)| format!("`{value}` ({count})"))
                .collect();
            let elided_tokens = counted
                .iter()
                .skip(MAX_NAMED_OUT_OF_SET_VALUES)
                .map(|(_, count)| count)
                .sum::<u64>()
                + values.untracked_tokens;
            if elided_tokens > 0 {
                named.push(format!("{elided_tokens} tokens with other values"));
            }
            format!(
                "the corpus has `{}` values outside the label set `{}` declared at {}:{}: {}; \
                 the catalog's literals may not match this corpus's annotation scheme",
                declaration.attribute().as_str(),
                declaration.set_name(),
                declaration.source_name(),
                declaration.line(),
                named.join(", ")
            )
        })
        .collect()
}

pub(crate) fn write_out_of_set_label_notices(
    declarations: &[lingrep_core::fsm::LabelDeclaration],
    observed: &[lingrep_core::fsm::OutOfSetValues],
    config: &ExecutionCliConfig,
    run_id: &str,
) -> Result<(), CliError> {
    for message in out_of_set_label_messages(declarations, observed) {
        crate::commands::query_support::write_warning(
            &config.stderr,
            config.diagnostic_format,
            DiagnosticStage::Matching,
            "label_outside_declared_set",
            message,
            Some(run_id),
            Vec::new(),
        )
        .map_err(CliError::Io)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use lingrep_core::fsm::{CompileLimits, PatternSource, compile_catalog};
    use lingrep_core::token_match::TokenSchema;

    fn compiled(source: &str) -> lingrep_core::fsm::CompileOutput {
        compile_catalog(
            PatternSource::named("t.lx", source),
            CompileLimits::default(),
        )
        .expect("catalog compiles")
    }

    #[test]
    fn a_notice_names_the_attribute_the_referrers_and_the_schema() {
        let output = compiled("p := [unidic.cform~連用形].\nq := [unidic.cform=終止形-一般].");
        let bound = output
            .matcher
            .bind(&TokenSchema::from_keys(["text"]).unwrap())
            .expect("binds with the attribute unavailable");

        let messages = unavailable_attribute_messages(
            bound.unavailable_attributes(),
            &output.analysis,
            "pretokenized syntax",
        );

        assert_eq!(messages.len(), 1);
        assert_eq!(
            messages[0],
            "`unidic.cform` is referenced by `p` (t.lx:1), `q` (t.lx:2) but this corpus's \
             token schema (pretokenized syntax) does not carry it; positive tests over it match \
             nothing, and only `!(…)` complements hold"
        );
    }

    #[test]
    fn a_policy_only_attribute_falls_back_to_the_policy_as_referrer() {
        let output = compiled("key: [unidic.pos1]\npattern p := [text=猫].");
        let bound = output
            .matcher
            .bind_with_projection(
                &TokenSchema::from_keys(["text"]).unwrap(),
                lingrep_core::execution::cached_run_attributes(&output.matcher),
            )
            .expect("binds with the projection attribute unavailable");

        let messages = unavailable_attribute_messages(
            bound.unavailable_attributes(),
            &output.analysis,
            "vibrato tokenize",
        );

        assert_eq!(messages.len(), 1);
        assert!(
            messages[0].starts_with("`unidic.pos1` is referenced by the catalog configuration"),
            "{}",
            messages[0]
        );
    }

    #[test]
    fn a_long_referrer_list_is_elided_after_three() {
        let output = compiled(
            "a := [feat.Case=Nom].\nb := [feat.Case=Acc].\nc := [feat.Case=Dat].\nd := [feat.Case=Gen].",
        );
        let bound = output
            .matcher
            .bind(&TokenSchema::from_keys(["text"]).unwrap())
            .expect("binds");

        let messages = unavailable_attribute_messages(
            bound.unavailable_attributes(),
            &output.analysis,
            "vibrato morphology",
        );

        assert!(
            messages[0].contains("`c` (t.lx:3), and 1 more"),
            "{}",
            messages[0]
        );
    }

    #[test]
    fn a_where_refusal_reads_as_a_requirement_not_a_read_failure() {
        let output =
            compiled("pattern p := (?<s> [text=猫]) (?<v> [text=走る]) where $v -[nsubj]-> $s.");
        let error = output
            .matcher
            .bind(&TokenSchema::unidic_morphology())
            .expect_err("dependency edges are missing");

        let wrapped = bind_refusal(&error, "vibrato morphology");

        assert_eq!(
            wrapped.to_string(),
            "Invalid arguments: the catalog cannot bind to this corpus's token schema (vibrato \
             morphology): dependency `where` clauses require dep.head, dep.rel, which the \
             token schema does not carry"
        );
    }

    #[test]
    fn only_a_schema_carrying_text_without_surface_is_named_unfolded() {
        let output = compiled("g := [text=ＮＨＫ].\nh := [lemma=猫].");
        let message = |keys: &[&str]| {
            unfolded_text_message(
                &TokenSchema::from_keys(keys.iter().copied()).unwrap(),
                &output.analysis,
                "vibrato morphology",
            )
        };
        assert_eq!(
            message(&["text", "lemma"]).as_deref(),
            Some(
                "`g` compares `text` as the folded match key, but this corpus's token schema \
                 (vibrato morphology) carries `text` unfolded, so a token spelled ＮＨＫ or ｶﾞﾗｽ \
                 matches neither `[text=ＮＨＫ]` nor `[text=NHK]`. This corpus was prepared \
                 before 0.9: rebuild it with this ling (`ling corpus build`)"
            )
        );
        assert_eq!(message(&["text", "surface", "lemma"]), None);
        assert_eq!(message(&["lemma"]), None);
    }
}
