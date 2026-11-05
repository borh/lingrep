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

use lingrep_core::fsm::{BoundCatalog, CatalogAnalysis, SchemaBindError};
use lingrep_core::token_match::AttributeKey;

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
    let detail = match error {
        SchemaBindError::MissingKeys(keys) => format!(
            "its dependency `where` clauses require {}, which this schema does not carry",
            keys.join(", ")
        ),
        SchemaBindError::KindMismatch { .. } => error.to_string(),
    };
    CliError::InvalidArgs(format!(
        "the catalog cannot bind to this corpus's token schema ({schema}): {detail}"
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
            let referenced_by = if referrers.is_empty() {
                // Reference sites cover definition bodies; what remains is the
                // publication policy's projection (`key:`, `transform:`).
                "the catalog's publication policy".to_owned()
            } else if referrers.len() > NAMED_REFERRERS {
                format!(
                    "{}, and {} more",
                    referrers[..NAMED_REFERRERS].join(", "),
                    referrers.len() - NAMED_REFERRERS
                )
            } else {
                referrers.join(", ")
            };
            format!(
                "`{attribute}` is referenced by {referenced_by} but this corpus's token schema \
                 ({schema}) does not carry it; positive tests over it match nothing, and only \
                 `!(…)` complements hold"
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

/// One `division_unavailable` notice per division kind that `within=`
/// lookarounds name and the corpus does not observe.
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
            messages[0]
                .starts_with("`unidic.pos1` is referenced by the catalog's publication policy"),
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
             morphology): its dependency `where` clauses require dep.head, dep.rel, which this \
             schema does not carry"
        );
    }
}
