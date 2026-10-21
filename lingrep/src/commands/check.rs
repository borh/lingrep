use std::fs;
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
    report_catalog_drift,
};

#[cfg(test)]
use crate::catalog_input::catalog_fingerprint;
use crate::catalog_input::{CatalogSource, load_and_compile};
use crate::cli::{CheckArgs, CheckFormat};
use crate::error::CliError;
use crate::presentation::OutputMode;
use crate::protocol::check::{
    BindingData, BindingSiteData, CheckData, CheckEnvelope, CheckEvent, CompileStats,
    CompileWarning, DefinitionDescriptor, DriftBodyData, DriftCarrierData, DriftData,
    SourceLocation, UnavailableAttributeData,
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
    let catalog = load_and_compile(&args.catalog.catalog_input()?)?;
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
        .then(|| verify_examples(args, config, &catalog.output.matcher))
        .transpose()?;
    let examples_failed = examples
        .as_ref()
        .is_some_and(|r| !r.failures.is_empty() || !r.post_policy_failures.is_empty());
    let stats = catalog.output.stats;
    let warnings = catalog
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
    let (drift, comparison) = compare_catalogs(&catalog.source, &args.compare_with)?;
    let drift_found = !drift.is_empty();
    let binding = args
        .schema
        .map(|schema| assess_binding(schema, &catalog.output));
    let binding_refused = binding
        .as_ref()
        .is_some_and(|binding| binding.refusal.is_some());
    let check = CheckEnvelope::new(CheckEvent::Check(CheckData {
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
    }));

    let stdout = io::stdout();
    let mut writer = stdout.lock();
    let format = if output == OutputMode::Human {
        CheckFormat::Human
    } else {
        CheckFormat::Json
    };
    let outcome = publish_check(&mut writer, format, &check, terminal)?;
    let failed = (args.fail_on_drift && drift_found) || binding_refused || examples_failed;
    if failed && outcome == CommandOutcome::Success {
        Ok(CommandOutcome::Failure)
    } else {
        Ok(outcome)
    }
}

/// Binds the compiled catalog against the named declared schema and reports
/// what a run against such a corpus would warn about or refuse.
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

fn publish_check<W: Write>(
    writer: &mut W,
    format: CheckFormat,
    check: &CheckEnvelope,
    terminal: &TerminalArbiter,
) -> Result<CommandOutcome, CliError> {
    if !terminal.try_claim_completion() {
        return Ok(CommandOutcome::Cancelled);
    }
    match format {
        CheckFormat::Json => {
            write_jsonl(writer, check).map_err(|error| CliError::Io(io::Error::other(error)))?
        }
        CheckFormat::Human => {
            let CheckEvent::Check(data) = &check.event;
            writeln!(
                writer,
                "catalog {}: {} pattern(s), {} warning(s)",
                SafeText::from_untrusted(&data.catalog_fingerprint),
                data.definitions.len(),
                data.warnings.len()
            )
            .map_err(CliError::Io)?;
            if let Some(examples) = &data.examples {
                writeln!(
                    writer,
                    "examples: {} checked, {} failure(s); post-policy: {} checked, {} failure(s)",
                    examples.checked,
                    examples.failures.len(),
                    examples.post_policy_checked,
                    examples.post_policy_failures.len()
                )?;
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
                    )?;
                }
            }
            if let Some(binding) = &data.binding {
                if let Some(refusal) = &binding.refusal {
                    writeln!(
                        writer,
                        "binding {}: refused — {}",
                        SafeText::from_untrusted(&binding.schema),
                        SafeText::from_untrusted(refusal)
                    )
                    .map_err(CliError::Io)?;
                } else {
                    writeln!(
                        writer,
                        "binding {}: {} unavailable attribute(s)",
                        SafeText::from_untrusted(&binding.schema),
                        binding.unavailable.len()
                    )
                    .map_err(CliError::Io)?;
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
                            "the catalog's publication policy".to_owned()
                        } else {
                            sites.join(", ")
                        };
                        writeln!(
                            writer,
                            "  {} — referenced by {}",
                            SafeText::from_untrusted(&attribute.attribute),
                            SafeText::from_untrusted(&referenced_by)
                        )
                        .map_err(CliError::Io)?;
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
                .map_err(CliError::Io)?;
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
        let text = fs::read_to_string(path).map_err(|error| {
            CliError::PatternParse(format!(
                "failed to read lingrep patterns catalog {}: {error}",
                path.display()
            ))
        })?;
        catalogs.push(CatalogSource {
            name: path.display().to_string(),
            text,
        });
    }

    let sources: Vec<PatternSource<'_>> = catalogs
        .iter()
        .map(|catalog| PatternSource::named(&catalog.name, &catalog.text))
        .collect();
    let report = report_catalog_drift(&sources).map_err(|error| {
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

fn verify_examples(
    args: &CheckArgs,
    config: &crate::cli::ExecutionCliConfig,
    matcher: &lingrep_core::fsm::CompiledCatalog,
) -> Result<crate::protocol::check::ExampleVerification, CliError> {
    use lingrep_core::analysis::{
        AnalyzerPlan, AnalyzerSession, InputUnitId, SegmentationMode, TextUnit,
    };
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
        .or_else(|| std::env::var_os("LINGREP_DICTIONARY_PATH").map(Into::into));
    let resolved = crate::analyzer_selection::resolve_native(config, dictionary.as_deref())?;
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
    let tokenize = |text: &str| -> Vec<TokenInfo> {
        if failure.borrow().is_some() {
            return Vec::new();
        }
        let analyze = || -> Result<Vec<TokenInfo>, String> {
            if text.len() > config.limits.max_sentence_bytes {
                return Err("example exceeds --max-sentence-bytes".into());
            }
            let unit = TextUnit::sentence(InputUnitId::new(0), text, config.normalization)
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
    let report = lingrep_core::fsm::verify_examples(matcher, &tokenize);
    if let Some(error) = failure.into_inner() {
        return Err(CliError::Corpus(format!(
            "example analysis failed: {error}"
        )));
    }
    let convert = |f: lingrep_core::fsm::ExampleFailure| crate::protocol::check::ExampleFailure {
        definition: f.definition,
        example: f.example,
        kind: match f.kind {
            lingrep_core::fsm::ExampleFailureKind::ExpectedMatch => "expected_match",
            lingrep_core::fsm::ExampleFailureKind::UnexpectedMatch => "unexpected_match",
            lingrep_core::fsm::ExampleFailureKind::Malformed => "malformed",
        }
        .into(),
    };
    Ok(crate::protocol::check::ExampleVerification {
        analyzer: crate::protocol::query::AnalyzerRun::new(
            &lingrep_corpus::AnalyzerRecord::from_descriptor(plan.descriptor()),
            "locally-executed",
            plan.schema(),
        ),
        resource_identity: crate::direct_source::encode_digest(resolved.resource_identity),
        checked: report.checked as u64,
        failures: report.failures.into_iter().map(convert).collect(),
        post_policy_checked: report.post_policy_checked as u64,
        post_policy_failures: report
            .post_policy_failures
            .into_iter()
            .map(convert)
            .collect(),
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
            publish_check(&mut output, CheckFormat::Json, &check_fixture(), &terminal).unwrap();

        assert_eq!(outcome, CommandOutcome::Cancelled);
        assert!(output.is_empty());
    }

    #[test]
    fn check_completion_claim_precedes_success_output_and_ignores_late_signals() {
        let terminal = TerminalArbiter::new();
        let mut output = Vec::new();

        let outcome =
            publish_check(&mut output, CheckFormat::Human, &check_fixture(), &terminal).unwrap();

        assert_eq!(outcome, CommandOutcome::Success);
        assert!(!output.is_empty());
        assert!(!terminal.try_cancel(signal_hook::consts::SIGTERM));
    }

    #[test]
    fn check_output_failure_remains_an_error_after_completion_wins() {
        let terminal = TerminalArbiter::new();

        let error = publish_check(
            &mut BrokenWriter,
            CheckFormat::Json,
            &check_fixture(),
            &terminal,
        )
        .unwrap_err();

        assert!(matches!(error, CliError::Io(_)));
        assert_eq!(terminal.winning_signal(), None);
    }
}
