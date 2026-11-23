use std::io::{self, Write};

#[cfg(test)]
use blake3::Hasher;
use lingrep_cli_common::SafeText;
#[cfg(test)]
use lingrep_core::fsm::{
    COMPILER_SCHEMA_IDENTITY, GRAMMAR_SEMANTICS_REVISION, POLICY_SEMANTICS_IDENTITY,
};
use lingrep_core::fsm::{
    DiagnosticSeverity as CompilerSeverity, DriftDefinitionKind, PatternSource,
    report_catalog_drift_with_vectors,
};

#[cfg(test)]
use crate::catalog_input::catalog_fingerprint;
use crate::catalog_input::{CatalogInput, CatalogSource, compile, load, vectors_path};
use crate::cli::CheckArgs;
use crate::error::CliError;
use crate::presentation::OutputMode;
use crate::protocol::check::{
    BindingData, BindingSiteData, CheckData, CheckEnvelope, CheckEvent, CompileStats,
    CompileWarning, ContainedPairData, ContainmentData, ContainmentNotDecided,
    DefinitionDescriptor, DriftBodyData, DriftCarrierData, DriftData, SourceLocation,
    UnavailableAttributeData,
};
use crate::protocol::query::SourceSpan;
use crate::protocol::write_jsonl;
use crate::terminal::TerminalArbiter;

use super::CommandOutcome;

pub fn run(
    args: &CheckArgs,
    config: &crate::cli::ExecutionCliConfig,
    terminal: &TerminalArbiter,
    output: OutputMode,
) -> Result<CommandOutcome, CliError> {
    if !args.compare_with.is_empty() && args.catalog.pattern.is_some() {
        return Err(CliError::InvalidArgs(
            "--compare-with requires --patterns: an inline --pattern has no stable catalog name to report as a drift carrier"
                .into(),
        ));
    }
    let input = args.catalog.catalog_input()?;
    let table = vectors_path(args.vectors.as_deref())
        .map(|path| lingrep_corpus::word_vectors::VectorTable::open(&path))
        .transpose()?;
    let catalog = compile(
        load(&input)?,
        table.as_ref(),
        matches!(input, CatalogInput::File(_)),
    )?;
    super::query_support::write_vectors_notice(&catalog, config)?;
    if args.diagram {
        return super::check_diagram::run(args, &catalog, terminal);
    }
    let definitions = catalog
        .authored_definitions()?
        .into_iter()
        .map(|definition| DefinitionDescriptor {
            name: definition.name,
            catalog_ordinal: definition.catalog_ordinal,
            body: match definition.body {
                crate::catalog_input::AuthoredDefinitionBody::Pattern { expression } => {
                    crate::protocol::query::DefinitionBody::Pattern { expression }
                }
                crate::catalog_input::AuthoredDefinitionBody::Lexicon { by, member_count } => {
                    crate::protocol::query::DefinitionBody::Lexicon { by, member_count }
                }
            },
            metadata: definition.metadata,
            source_file: definition.source_file,
            source_span: SourceSpan {
                start: definition.source_span.start,
                end: definition.source_span.end,
            },
        })
        .collect();
    let examples = args
        .verify_examples
        .then(|| verify_examples(args, config, &catalog.output, terminal))
        .transpose()?;
    let examples_failed = examples
        .as_ref()
        .is_some_and(|r| !r.failures.is_empty() || !r.post_policy_failures.is_empty());
    let stats = catalog.output.stats;
    let mut warnings: Vec<CompileWarning> = catalog
        .output
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.severity() == CompilerSeverity::Warning)
        .map(|diagnostic| {
            let location = diagnostic.location();
            CompileWarning {
                code: diagnostic.code().as_str().to_owned(),
                message: diagnostic.message.clone(),
                location: Some(SourceLocation {
                    source_file: location.source_name().to_owned(),
                    source_span: SourceSpan {
                        start: location.byte_range().start as u64,
                        end: location.byte_range().end as u64,
                    },
                }),
            }
        })
        .collect();
    warnings.extend(
        catalog
            .output
            .no_match
            .unsatisfiable()
            .into_iter()
            .map(unsatisfiable_warning),
    );
    let containment = args.containment.then(|| {
        let report = catalog.output.no_match.containments();
        ContainmentData {
            pairs: report
                .pairs
                .into_iter()
                .map(|pair| ContainedPairData {
                    contained: pair.contained,
                    containing: pair.containing,
                    equivalent: pair.equivalent,
                })
                .collect(),
            not_decided: report
                .not_decided
                .into_iter()
                .map(|(definition, construct)| ContainmentNotDecided {
                    definition,
                    construct: construct.to_owned(),
                })
                .collect(),
        }
    });
    let (drift, comparison) =
        compare_catalogs(&catalog.source, &args.compare_with, table.as_ref())?;
    let drift_found = !drift.is_empty();
    let binding = args
        .schema
        .map(|schema| assess_binding(schema, &catalog.output));
    let binding_refused = binding
        .as_ref()
        .is_some_and(|binding| binding.refusal.is_some());
    let check = CheckEnvelope::new(CheckEvent::Check(CheckData {
        capture_key_edits: catalog
            .output
            .capture_key_edits
            .into_iter()
            .map(Into::into)
            .collect(),
        comparison,
        examples,
        catalog_fingerprint: catalog.fingerprint,
        definitions,
        compile_stats: CompileStats {
            source_bytes: stats.source_bytes() as u64,
            definitions: stats.definitions() as u64,
            lexicon_members: stats.lexicon_members() as u64,
            expression_nodes: stats.expression_nodes() as u64,
            max_depth: stats.max_depth() as u64,
            metadata_bytes: stats.metadata_bytes() as u64,
            regex_bytes: stats.regex_bytes() as u64,
            reference_edges: stats.reference_edges() as u64,
            expansion_work: stats.expansion_work() as u64,
            generated_owned_bytes: stats.generated_owned_bytes() as u64,
            nfa_states: stats.nfa_states() as u64,
            transitions: stats.transitions() as u64,
        },
        warnings,
        drift,
        binding,
        containment,
    }));

    let stdout = io::stdout();
    let mut writer = stdout.lock();
    let outcome = publish_check(&mut writer, output, &check, terminal)?;
    let failed = (args.fail_on_drift && drift_found) || binding_refused || examples_failed;
    if failed && outcome == CommandOutcome::Success {
        Ok(CommandOutcome::Failure)
    } else {
        Ok(outcome)
    }
}

fn assess_binding(
    schema: crate::cli::CheckSchemaArg,
    output: &lingrep_core::fsm::CompileOutput,
) -> BindingData {
    use crate::cli::CheckSchemaArg;
    let (name, token_schema) = match schema {
        CheckSchemaArg::UnidicMorphology => (
            "unidic-morphology",
            lingrep_core::token_match::TokenSchema::unidic_morphology(),
        ),
        CheckSchemaArg::SudachiMorphology => (
            "sudachi-morphology",
            lingrep_core::analysis::declared_token_schema(
                lingrep_core::analysis::AnalyzerFamily::Sudachi,
                lingrep_core::analysis::ExtractionProfile::Morphology,
            )
            .expect("the Sudachi morphology schema is declared"),
        ),
        CheckSchemaArg::Tokenize => (
            "tokenize",
            lingrep_core::analysis::declared_token_schema(
                lingrep_core::analysis::AnalyzerFamily::Vibrato,
                lingrep_core::analysis::ExtractionProfile::Tokenize,
            )
            .expect("the tokenize schema is declared"),
        ),
        CheckSchemaArg::Conllu => ("conllu", lingrep_core::corpus::conllu::token_schema()),
    };
    // The same set a run binds: matching attributes plus the publication
    // projection, so `check` and `matches` report one finding for one catalog.
    let projected = lingrep_core::execution::cached_run_attributes(&output.matcher);
    match output
        .matcher
        .bind_with_projection(&token_schema, projected)
    {
        Ok(bound) => BindingData {
            schema: name.to_owned(),
            unavailable: bound
                .unavailable_attributes()
                .iter()
                .map(|attribute| UnavailableAttributeData {
                    attribute: attribute.as_str().to_owned(),
                    sites: output
                        .analysis
                        .references_to(attribute)
                        .map(|site| BindingSiteData {
                            definition: site.definition.to_string(),
                            source_file: site.source_name.to_string(),
                            line: site.declaration_line as u64,
                        })
                        .collect(),
                })
                .collect(),
            refusal: None,
        },
        Err(error) => BindingData {
            schema: name.to_owned(),
            unavailable: Vec::new(),
            refusal: Some(error.to_string()),
        },
    }
}

fn unsatisfiable_warning(finding: lingrep_core::fsm::Unsatisfiable) -> CompileWarning {
    let (code, message) = match finding.kind {
        lingrep_core::fsm::UnsatisfiableKind::TokenTest { test } => (
            "unsatisfiable_test",
            format!("`{}`: no token satisfies {test}", finding.definition),
        ),
        lingrep_core::fsm::UnsatisfiableKind::Definition => (
            "unsatisfiable_definition",
            format!("`{}` matches no token sequence", finding.definition),
        ),
    };
    CompileWarning {
        code: code.to_owned(),
        message,
        location: Some(SourceLocation {
            source_file: finding.location.source.to_string(),
            source_span: SourceSpan {
                start: finding.location.byte_range.start as u64,
                end: finding.location.byte_range.end as u64,
            },
        }),
    }
}

fn publish_check<W: Write>(
    writer: &mut W,
    output: OutputMode,
    check: &CheckEnvelope,
    terminal: &TerminalArbiter,
) -> Result<CommandOutcome, CliError> {
    if !terminal.try_claim_completion() {
        return Ok(CommandOutcome::Cancelled);
    }
    match output {
        OutputMode::Jsonl | OutputMode::Tsv => {
            write_jsonl(writer, check).map_err(CliError::from)?
        }
        OutputMode::Human => {
            let CheckEvent::Check(data) = &check.event;
            writeln!(
                writer,
                "catalog checked: {} definition(s), {} warning(s)",
                data.definitions.len(),
                data.warnings.len()
            )
            .map_err(crate::protocol::OutputError::from)?;
            for warning in data
                .warnings
                .iter()
                .filter(|warning| warning.code.starts_with("unsatisfiable_"))
            {
                writeln!(
                    writer,
                    "warning[{}]: {}",
                    warning.code,
                    SafeText::from_untrusted(&warning.message)
                )
                .map_err(crate::protocol::OutputError::from)?;
            }
            if let Some(containment) = &data.containment {
                writeln!(
                    writer,
                    "containment: {} pair(s), {} definition(s) not compared",
                    containment.pairs.len(),
                    containment.not_decided.len()
                )
                .map_err(crate::protocol::OutputError::from)?;
                for pair in &containment.pairs {
                    let relation = if pair.equivalent {
                        "publishes the same spans as"
                    } else {
                        "publishes only spans of"
                    };
                    writeln!(
                        writer,
                        "  {} {relation} {}",
                        SafeText::from_untrusted(&pair.contained),
                        SafeText::from_untrusted(&pair.containing)
                    )
                    .map_err(crate::protocol::OutputError::from)?;
                }
                for skipped in &containment.not_decided {
                    writeln!(
                        writer,
                        "  {} not compared: its body has {}",
                        SafeText::from_untrusted(&skipped.definition),
                        SafeText::from_untrusted(&skipped.construct)
                    )
                    .map_err(crate::protocol::OutputError::from)?;
                }
            }
            if let Some(examples) = &data.examples {
                writeln!(
                    writer,
                    "examples: {} checked, {} failure(s); post-policy: {} checked, {} failure(s)",
                    examples.checked,
                    examples.failures.len(),
                    examples.post_policy_checked,
                    examples.post_policy_failures.len()
                )
                .map_err(crate::protocol::OutputError::from)?;
                for failure in examples
                    .failures
                    .iter()
                    .chain(&examples.post_policy_failures)
                {
                    writeln!(
                        writer,
                        "{}: {}: {}",
                        SafeText::from_untrusted(&failure.definition),
                        failure.kind,
                        SafeText::from_untrusted(&failure.example)
                    )
                    .map_err(crate::protocol::OutputError::from)?;
                    if let Some(explanation) = &failure.explanation {
                        for line in explanation_lines(explanation) {
                            writeln!(writer, "  {}", SafeText::from_untrusted(&line))
                                .map_err(crate::protocol::OutputError::from)?;
                        }
                    }
                }
                for line in counterexample_lines(examples) {
                    writeln!(writer, "{}", SafeText::from_untrusted(&line))
                        .map_err(crate::protocol::OutputError::from)?;
                }
            }
            if let Some(binding) = &data.binding {
                if let Some(refusal) = &binding.refusal {
                    writeln!(
                        writer,
                        "binding {}: rejected — {}",
                        SafeText::from_untrusted(&binding.schema),
                        SafeText::from_untrusted(refusal)
                    )
                    .map_err(crate::protocol::OutputError::from)?;
                } else {
                    writeln!(
                        writer,
                        "binding {}: {} unavailable attribute(s)",
                        SafeText::from_untrusted(&binding.schema),
                        binding.unavailable.len()
                    )
                    .map_err(crate::protocol::OutputError::from)?;
                    for attribute in &binding.unavailable {
                        let sites: Vec<String> = attribute
                            .sites
                            .iter()
                            .map(|site| {
                                format!(
                                    "`{}` ({}:{})",
                                    site.definition, site.source_file, site.line
                                )
                            })
                            .collect();
                        let referenced_by = if sites.is_empty() {
                            "the catalog configuration".to_owned()
                        } else {
                            sites.join(", ")
                        };
                        writeln!(
                            writer,
                            "  {} — referenced by {}",
                            SafeText::from_untrusted(&attribute.attribute),
                            SafeText::from_untrusted(&referenced_by)
                        )
                        .map_err(crate::protocol::OutputError::from)?;
                    }
                }
            }
            // One line per drifting name: report which catalogs carry each
            // distinct body, never a structural diff.
            for entry in &data.drift {
                let bodies: Vec<String> = entry
                    .bodies
                    .iter()
                    .map(|body| {
                        body.carriers
                            .iter()
                            .map(|carrier| carrier.catalog.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    })
                    .collect();
                writeln!(
                    writer,
                    "drift: {} has {} bodies ({})",
                    SafeText::from_untrusted(&entry.name),
                    entry.bodies.len(),
                    SafeText::from_untrusted(&bodies.join("; "))
                )
                .map_err(crate::protocol::OutputError::from)?;
            }
        }
    }
    Ok(CommandOutcome::Success)
}

/// Compares the primary catalog against each `--compare-with` file.
///
/// Drift is a notice unless the caller requests `--fail-on-drift`. A *compile*
/// failure in a compared catalog is the
/// existing catalog-compile path, the same rule the primary follows — drift
/// measured over a broken catalog is meaningless.
fn compare_catalogs(
    primary: &CatalogSource,
    compare_with: &[std::path::PathBuf],
    table: Option<&lingrep_corpus::word_vectors::VectorTable>,
) -> Result<
    (
        Vec<DriftData>,
        Vec<crate::protocol::check::ComparedDefinition>,
    ),
    CliError,
> {
    if compare_with.is_empty() {
        return Ok((Vec::new(), Vec::new()));
    }

    let mut catalogs = vec![CatalogSource {
        name: primary.name.clone(),
        text: primary.text.clone(),
    }];
    for path in compare_with {
        catalogs.push(load(&CatalogInput::File(path.clone()))?);
    }

    let sources: Vec<PatternSource<'_>> = catalogs
        .iter()
        .map(|catalog| PatternSource::named(&catalog.name, &catalog.text))
        .collect();
    let report = report_catalog_drift_with_vectors(
        &sources,
        table.map(|table| table as &dyn lingrep_core::fsm::WordVectors),
    )
    .map_err(|error| {
        // Pair the failure with the text of the catalog it came from, so the
        // rendered diagnostic carries the right source context rather than the
        // primary's.
        let source_text = error
            .diagnostics
            .first()
            .map(|diagnostic| diagnostic.location().source_name())
            .and_then(|name| {
                catalogs
                    .iter()
                    .find(|catalog| catalog.name == name)
                    .map(|catalog| catalog.text.clone())
            })
            .unwrap_or_else(|| primary.text.clone());
        CliError::CatalogCompile { error, source_text }
    })?;

    let comparison = report
        .definitions
        .iter()
        .map(|entry| {
            let revisions = entry
                .bodies
                .iter()
                .enumerate()
                .flat_map(|(group, body)| body.carriers.iter().map(move |carrier| (group, carrier)))
                .map(|(group, carrier)| {
                    let source = catalogs
                        .iter()
                        .find(|c| c.name == carrier.catalog)
                        .ok_or_else(|| {
                            CliError::PatternParse("comparison source missing".into())
                        })?;
                    let expansion = lingrep_core::fsm::expand_pattern_definition(
                        PatternSource::named(&source.name, &source.text),
                        lingrep_core::fsm::CompileLimits::default(),
                        &entry.name,
                    )
                    .map_err(|e| CliError::PatternParse(e.to_string()))?
                    .ok_or_else(|| {
                        CliError::PatternParse("comparison definition missing".into())
                    })?;
                    let text = expansion.expanded.unwrap_or(expansion.declaration);
                    let formatted = lingrep_core::fsm::format_pattern_source(
                        PatternSource::named(&source.name, &text),
                        lingrep_core::fsm::CompileLimits::default(),
                    )
                    .map_err(|e| CliError::PatternParse(e.to_string()))?;
                    Ok(crate::protocol::check::DefinitionRevision {
                        catalog: carrier.catalog.clone(),
                        kind: match carrier.kind {
                            DriftDefinitionKind::Pattern => "pattern",
                            DriftDefinitionKind::Fragment => "fragment",
                            DriftDefinitionKind::Lexicon => "lexicon",
                        }
                        .into(),
                        body_group: group as u64,
                        expanded: formatted.source,
                    })
                })
                .collect::<Result<Vec<_>, CliError>>()?;
            Ok(crate::protocol::check::ComparedDefinition {
                name: entry.name.clone(),
                revisions,
            })
        })
        .collect::<Result<Vec<_>, CliError>>()?;
    let drift = report
        .entries
        .into_iter()
        .map(|entry| DriftData {
            name: entry.name,
            bodies: entry
                .bodies
                .into_iter()
                .map(|body| DriftBodyData {
                    carriers: body
                        .carriers
                        .into_iter()
                        .map(|carrier| DriftCarrierData {
                            catalog: carrier.catalog,
                            kind: match carrier.kind {
                                DriftDefinitionKind::Pattern => "pattern".to_owned(),
                                DriftDefinitionKind::Fragment => "fragment".to_owned(),
                                DriftDefinitionKind::Lexicon => "lexicon".to_owned(),
                            },
                        })
                        .collect(),
                })
                .collect(),
        })
        .collect();
    Ok((drift, comparison))
}

/// The human rendering of one explanation, one line per fact.
fn explanation_lines(explanation: &crate::protocol::check::NoMatchExplanation) -> Vec<String> {
    let spans = || {
        explanation
            .spans
            .iter()
            .map(|[start, end]| format!("{start}..{end}"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let mut lines = match (explanation.outcome.as_str(), &explanation.attempt) {
        ("stopped", Some(attempt)) => {
            let element = attempt
                .element
                .as_ref()
                .map_or_else(String::new, |element| {
                    format!(" at {}:{}:{}", element.source, element.line, element.column)
                });
            let token = attempt
                .token
                .as_ref()
                .map_or_else(String::new, |token| format!(" \u{300c}{token}\u{300d}"));
            let stop = match attempt.stop.as_str() {
                "token" => format!(
                    "token {}{token} fails the element{element}",
                    attempt.position
                ),
                "sentence_ended" => format!("the sentence ended before the element{element}"),
                "not_sentence_start" => {
                    format!("token {} is not at the sentence start", attempt.position)
                }
                "not_sentence_end" => {
                    format!("token {}{token} is not the sentence end", attempt.position)
                }
                kind => format!(
                    "{kind}{element} does not hold before token {}{token}",
                    attempt.position
                ),
            };
            let mut lines = vec![format!(
                "furthest attempt: started at token {}, matched {} token(s); {stop}",
                attempt.start,
                attempt.position - attempt.start
            )];
            lines.extend(attempt.tests.iter().map(|test| {
                format!(
                    "  {} {}{}: token has {}",
                    if test.held { "holds" } else { "fails" },
                    test.attribute,
                    test.test,
                    test.actual.as_deref().unwrap_or("no value")
                )
            }));
            lines
        }
        ("not_published", _) => vec![format!(
            "the definition matched token span(s) {} and result policy published none",
            spans()
        )],
        ("where_rejected", _) => vec![format!(
            "the body matched token span(s) {} and the where clause rejected each",
            spans()
        )],
        _ => vec![format!(
            "no explanation: {}",
            explanation.detail.as_deref().unwrap_or("unknown")
        )],
    };
    if explanation.outcome == "stopped" || explanation.outcome == "where_rejected" {
        lines.push(format!(
            "from a separate explanation pass over the compiled catalog: {} start position(s) tried{}",
            explanation.starts_tried,
            if explanation.truncated {
                ", stopped at its step limit"
            } else {
                ""
            }
        ));
    }
    lines
}

fn relaxation_text(relaxation: &crate::protocol::check::Relaxation) -> String {
    let element = &relaxation.element;
    let at = format!("{}:{}:{}", element.source, element.line, element.column);
    match (&relaxation.attribute, &relaxation.test) {
        (Some(attribute), Some(test)) => {
            let mut shown = test.chars().take(60).collect::<String>();
            if shown.len() < test.len() {
                shown.push('…');
            }
            let separator = if shown.starts_with(char::is_alphabetic) {
                " "
            } else {
                ""
            };
            format!("the test {attribute}{separator}{shown} at {at}")
        }
        _ => format!("the {} at {at}", relaxation.kind),
    }
}

fn counterexample_lines(examples: &crate::protocol::check::ExampleVerification) -> Vec<String> {
    let mut lines = Vec::new();
    for vacuous in &examples.vacuous_counterexamples {
        lines.push(format!(
            "{}: vacuous_counterexample: {}",
            vacuous.definition, vacuous.example
        ));
        lines.push(format!(
            "  the definition does not match it with any one of its {} constraint(s) removed, so it does not test a constraint",
            vacuous.relaxations_tried.len()
        ));
        lines.extend(
            vacuous
                .relaxations_tried
                .iter()
                .map(|relaxation| format!("  tried without {}", relaxation_text(relaxation))),
        );
    }
    for unchecked in &examples.unchecked_counterexamples {
        lines.push(format!(
            "{}: unchecked_counterexample: {}",
            unchecked.definition, unchecked.example
        ));
        lines.push(format!("  {}", unchecked.reason));
    }
    if let Some(scan) = &examples.counterexample_candidates {
        lines.push(format!(
            "counterexample candidates from {}: {} sentence(s) scanned against {} definition(s); at most {} per definition and constraint",
            scan.source, scan.sentences_scanned, scan.definitions_evaluated, scan.candidates_per_relaxation
        ));
        if !scan.definitions_skipped.is_empty() {
            lines.push(format!(
                "  not evaluated: {}",
                scan.definitions_skipped.join(", ")
            ));
        }
        for candidate in &scan.candidates {
            lines.push(format!(
                "{}: candidate: {}",
                candidate.definition, candidate.sentence
            ));
            lines.push(format!(
                "  matches tokens {}..{} without {}",
                candidate.span[0],
                candidate.span[1],
                relaxation_text(&candidate.relaxation)
            ));
        }
    }
    lines
}

fn verify_examples(
    args: &CheckArgs,
    config: &crate::cli::ExecutionCliConfig,
    output: &lingrep_core::fsm::CompileOutput,
    terminal: &TerminalArbiter,
) -> Result<crate::protocol::check::ExampleVerification, CliError> {
    let matcher = &output.matcher;
    use lingrep_core::analysis::{AnalyzerSession, InputUnitId, SegmentationMode, TextUnit};
    use lingrep_core::token_match::TokenInfo;
    use std::cell::RefCell;
    if config.segmentation != SegmentationMode::Input {
        return Err(CliError::InvalidArgs(
            "--verify-examples requires --segmentation input: each claim is one sentence".into(),
        ));
    }
    let dictionary = args
        .dict_path
        .clone()
        .or_else(crate::packaged_dictionaries::fallback_dictionary_path);
    let resolved = crate::analyzer_selection::resolve_native(
        config,
        dictionary.as_deref(),
        terminal.cancellation(),
    )?;
    let plan = resolved.plan;
    let bound = matcher
        .bind(plan.schema())
        .map_err(|error| CliError::InvalidArgs(error.to_string()))?;
    if !bound.unavailable_attributes().is_empty() {
        return Err(CliError::InvalidArgs(format!(
            "cannot verify examples: analyzer attributes unavailable: {:?}",
            bound.unavailable_attributes()
        )));
    }
    let projection = plan
        .schema()
        .bind_projection(plan.schema().keys().iter().cloned())
        .map_err(|error| CliError::InvalidArgs(error.to_string()))?;
    let session = RefCell::new(plan.create_session());
    let failure = RefCell::new(None);
    let input_id = std::cell::Cell::new(0u64);
    let tokenize = |text: &str| -> Vec<TokenInfo> {
        if failure.borrow().is_some() {
            return Vec::new();
        }
        let analyze = || -> Result<Vec<TokenInfo>, String> {
            if text.len() > config.limits.max_sentence_bytes {
                return Err("example exceeds --max-sentence-bytes".into());
            }
            let unit = TextUnit::sentence(
                InputUnitId::new(input_id.replace(input_id.get() + 1)),
                text,
                config.normalization,
            )
            .map_err(|e| format!("{e:?}"))?;
            let batch = session
                .borrow_mut()
                .analyze_bounded_batch(vec![unit], &projection)
                .map_err(|e| e.to_string())?;
            let sentences = batch
                .inputs()
                .first()
                .map_or(&[][..], lingrep_core::analysis::AnalyzedInput::sentences);
            if sentences.len() > 1 {
                return Err("example analysis produced multiple sentences".into());
            }
            Ok(sentences
                .iter()
                .flat_map(|s| {
                    (0..s.words().len()).filter_map(|i| TokenInfo::from_sequence(s.words(), i))
                })
                .collect())
        };
        match analyze() {
            Ok(tokens) => tokens,
            Err(error) => {
                *failure.borrow_mut() = Some(error);
                Vec::new()
            }
        }
    };
    let publication_error = |error| match error {
        lingrep_core::fsm::MatchPublicationError::Resource(error) => {
            CliError::Execution(lingrep_core::execution::ExecutionError::Matching(
                lingrep_core::fsm::BoundExecutionError::Resource(error),
            ))
        }
        lingrep_core::fsm::MatchPublicationError::Policy(error) => {
            CliError::Execution(lingrep_core::execution::ExecutionError::PolicyAnchor(error))
        }
    };
    let report =
        lingrep_core::fsm::try_verify_examples(matcher, &tokenize).map_err(publication_error)?;
    let convert = |f: lingrep_core::fsm::ExampleFailure| {
        let explanation = if f.kind == lingrep_core::fsm::ExampleFailureKind::ExpectedMatch {
            let tokens = tokenize(&f.example);
            let explanation = output
                .explain_definition(&f.definition, tokens.as_slice())
                .map_err(publication_error)?;
            Some(crate::protocol::check::NoMatchExplanation::new(
                explanation,
                |index| {
                    tokens
                        .get(index)
                        .and_then(|token| token.surface())
                        .map(str::to_owned)
                },
            ))
        } else {
            None
        };
        Ok::<_, CliError>(crate::protocol::check::ExampleFailure {
            definition: f.definition,
            example: f.example,
            kind: match f.kind {
                lingrep_core::fsm::ExampleFailureKind::ExpectedMatch => "expected_match",
                lingrep_core::fsm::ExampleFailureKind::UnexpectedMatch => "unexpected_match",
                lingrep_core::fsm::ExampleFailureKind::Malformed => "malformed",
            }
            .into(),
            explanation,
        })
    };
    let failures = report
        .failures
        .into_iter()
        .map(convert)
        .collect::<Result<Vec<_>, _>>()?;
    let post_policy_failures = report
        .post_policy_failures
        .into_iter()
        .map(convert)
        .collect::<Result<Vec<_>, _>>()?;
    let mut vacuous_counterexamples = Vec::new();
    let mut unchecked_counterexamples = Vec::new();
    for (definition, example, outcome) in output.relax_counterexamples(&tokenize) {
        if failures
            .iter()
            .any(|failure| failure.definition == definition && failure.example == example)
        {
            continue;
        }
        match outcome {
            lingrep_core::fsm::RelaxationOutcome::Distinguished { .. }
            | lingrep_core::fsm::RelaxationOutcome::BodyMatches => {}
            lingrep_core::fsm::RelaxationOutcome::Vacuous { tried } => {
                vacuous_counterexamples.push(crate::protocol::check::VacuousCounterexample {
                    definition,
                    example,
                    relaxations_tried: tried.into_iter().map(Into::into).collect(),
                });
            }
            lingrep_core::fsm::RelaxationOutcome::Unchecked { reason } => {
                unchecked_counterexamples.push(crate::protocol::check::UncheckedCounterexample {
                    definition,
                    example,
                    reason,
                });
            }
        }
    }
    let counterexample_candidates = args
        .propose_counterexamples
        .as_deref()
        .map(|path| propose_counterexamples(path, output, &tokenize))
        .transpose()?;
    if let Some(error) = failure.into_inner() {
        return Err(CliError::Corpus(format!(
            "example analysis failed: {error}"
        )));
    }
    session.borrow_mut().finish().map_err(CliError::corpus)?;
    Ok(crate::protocol::check::ExampleVerification {
        analyzer: crate::protocol::query::AnalyzerRun::new(
            &lingrep_corpus::AnalyzerRecord::from_descriptor(plan.descriptor()),
            "locally-executed",
            plan.schema(),
        ),
        resource_identity: crate::direct_source::encode_digest(resolved.resource_identity),
        checked: report.checked as u64,
        failures,
        post_policy_checked: report.post_policy_checked as u64,
        post_policy_failures,
        vacuous_counterexamples,
        unchecked_counterexamples,
        counterexample_candidates,
    })
}

/// Candidates kept per definition and relaxation.
const CANDIDATES_PER_RELAXATION: usize = 3;

fn propose_counterexamples(
    path: &std::path::Path,
    output: &lingrep_core::fsm::CompileOutput,
    tokenize: &dyn Fn(&str) -> Vec<lingrep_core::token_match::TokenInfo>,
) -> Result<crate::protocol::check::CandidateScan, CliError> {
    use lingrep_core::fsm::RelaxationOutcome;
    let text = std::fs::read_to_string(path)
        .map_err(|error| CliError::Corpus(format!("{}: {error}", path.display())))?;
    let definitions = output.matcher.patterns().keys().collect::<Vec<_>>();
    let mut skipped = std::collections::BTreeSet::new();
    let mut kept = std::collections::BTreeMap::<_, usize>::new();
    let mut candidates = Vec::new();
    let mut sentences_scanned = 0u64;
    for sentence in text.lines().map(str::trim).filter(|line| !line.is_empty()) {
        sentences_scanned += 1;
        let tokens = tokenize(sentence);
        for definition in &definitions {
            if skipped.contains(definition.as_str()) {
                continue;
            }
            match output.relax_definition(definition, tokens.as_slice()) {
                RelaxationOutcome::Distinguished { by, span } => {
                    let relaxation = crate::protocol::check::Relaxation::from(by);
                    let count = kept
                        .entry((definition.to_string(), relaxation.clone()))
                        .or_default();
                    if *count < CANDIDATES_PER_RELAXATION {
                        *count += 1;
                        candidates.push(crate::protocol::check::CounterexampleCandidate {
                            definition: definition.to_string(),
                            sentence: sentence.to_owned(),
                            relaxation,
                            span: [span.0 as u64, span.1 as u64],
                        });
                    }
                }
                RelaxationOutcome::Unchecked { .. } if tokens.is_empty() => {}
                RelaxationOutcome::Unchecked { .. } => {
                    skipped.insert(definition.as_str());
                }
                RelaxationOutcome::BodyMatches | RelaxationOutcome::Vacuous { .. } => {}
            }
        }
    }
    candidates.sort_by(|left, right| {
        (&left.definition, left.relaxation.element.byte_start)
            .cmp(&(&right.definition, right.relaxation.element.byte_start))
    });
    Ok(crate::protocol::check::CandidateScan {
        source: path.display().to_string(),
        sentences_scanned,
        definitions_evaluated: (definitions.len() - skipped.len()) as u64,
        definitions_skipped: skipped.into_iter().map(str::to_owned).collect(),
        candidates_per_relaxation: CANDIDATES_PER_RELAXATION as u64,
        candidates,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::CommandOutcome;
    use crate::terminal::TerminalArbiter;

    /// Every identity the fingerprint claims to cover, recomputed here.
    ///
    /// A golden hex would pin the same thing, but it would say only "this
    /// changed" — this says *which* input is missing when it fails, and it
    /// fails if `catalog_fingerprint` ever stops folding one in.
    #[test]
    fn the_fingerprint_covers_the_grammar_the_compiler_and_the_policy_engine() {
        let source = "p := [text=a].";
        let expected = {
            let mut digest = Hasher::new();
            digest.update(b"lingrep/catalog/v1");
            digest.update(b"tree-sitter-lingrep/");
            digest.update(GRAMMAR_SEMANTICS_REVISION.as_bytes());
            digest.update(COMPILER_SCHEMA_IDENTITY.as_bytes());
            digest.update(POLICY_SEMANTICS_IDENTITY.as_bytes());
            digest.update(&(source.len() as u64).to_le_bytes());
            digest.update(source.as_bytes());
            digest.finalize().to_hex().to_string()
        };
        assert_eq!(catalog_fingerprint(source), expected);
    }

    /// Leading and trailing whitespace is not catalog identity.
    #[test]
    fn the_fingerprint_ignores_surrounding_whitespace() {
        assert_eq!(
            catalog_fingerprint("p := [text=a]."),
            catalog_fingerprint("\n  p := [text=a].\n\n")
        );
    }

    /// The value `ling check` 0.3.2 prints for this catalog.
    #[test]
    fn the_fingerprint_keeps_the_value_0_3_2_printed() {
        assert_eq!(
            catalog_fingerprint("p := [text=a]."),
            "a795b107e076ffa0a5d6d17bff4f116ff5e2cc6304e76a0304c5b39fd30d9f50"
        );
    }

    struct BrokenWriter;

    impl Write for BrokenWriter {
        fn write(&mut self, _buffer: &[u8]) -> io::Result<usize> {
            Err(io::Error::new(io::ErrorKind::BrokenPipe, "closed"))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn check_fixture() -> CheckEnvelope {
        CheckEnvelope::new(CheckEvent::Check(CheckData {
            capture_key_edits: Vec::new(),
            comparison: Vec::new(),
            examples: None,
            catalog_fingerprint: "catalog".into(),
            definitions: Vec::new(),
            compile_stats: CompileStats {
                source_bytes: 0,
                definitions: 0,
                lexicon_members: 0,
                expression_nodes: 0,
                max_depth: 0,
                metadata_bytes: 0,
                regex_bytes: 0,
                reference_edges: 0,
                expansion_work: 0,
                generated_owned_bytes: 0,
                nfa_states: 0,
                transitions: 0,
            },
            warnings: Vec::new(),
            containment: None,
            drift: Vec::new(),
            binding: None,
        }))
    }

    #[test]
    fn cancellation_claimed_before_check_publication_writes_no_success_bytes() {
        let terminal = TerminalArbiter::new();
        assert!(terminal.try_cancel(signal_hook::consts::SIGINT));
        let mut output = Vec::new();

        let outcome =
            publish_check(&mut output, OutputMode::Jsonl, &check_fixture(), &terminal).unwrap();

        assert_eq!(outcome, CommandOutcome::Cancelled);
        assert!(output.is_empty());
    }

    #[test]
    fn check_completion_claim_precedes_success_output_and_ignores_late_signals() {
        let terminal = TerminalArbiter::new();
        let mut output = Vec::new();

        let outcome =
            publish_check(&mut output, OutputMode::Human, &check_fixture(), &terminal).unwrap();

        assert_eq!(outcome, CommandOutcome::Success);
        assert!(!output.is_empty());
        assert!(!terminal.try_cancel(signal_hook::consts::SIGTERM));
    }

    #[test]
    fn check_output_failure_remains_an_error_after_completion_wins() {
        let terminal = TerminalArbiter::new();

        let error = publish_check(
            &mut BrokenWriter,
            OutputMode::Jsonl,
            &check_fixture(),
            &terminal,
        )
        .unwrap_err();

        assert!(matches!(
            error,
            CliError::Output(crate::protocol::OutputError::Io(_))
        ));
        assert!(error.is_broken_pipe());
        assert_eq!(terminal.winning_signal(), None);
    }
}
