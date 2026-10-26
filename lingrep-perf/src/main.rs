//! `lingrep-perf` — performance campaign tooling.
//!
//! Run it through the `Justfile`, which supplies the feature the screen needs:
//!
//! ```text
//! just perf-work                        # the campaign suite, one line each
//! just perf-work --json > before.json   # save, change something, then:
//! just perf-work --baseline before.json # what moved
//! ```

#[cfg(all(feature = "screen", not(target_env = "msvc")))]
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "lingrep-perf",
    about = "Performance campaign tooling for lingrep",
    long_about = "Tooling for running performance campaigns. `work` screens counted execution \
                  work so a campaign can tell a work change from a cost-per-unit change \
                  before spending measurement time on it."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Report counted execution work for every tracked catalog.
    Work {
        /// Screen one catalog only, by repository-relative path.
        #[arg(long, value_name = "PATH", conflicts_with = "all")]
        catalog: Option<String>,
        /// Screen every discovered .lx file, not only the campaign suite.
        #[arg(long)]
        all: bool,
        /// Emit JSON rather than a table. Save it to compare against later.
        #[arg(long)]
        json: bool,
        /// Compare against a JSON report saved earlier.
        #[arg(long, value_name = "FILE", conflicts_with = "json")]
        baseline: Option<std::path::PathBuf>,
    },
    /// Declare a campaign: seal the manifest and write an empty campaign file.
    ///
    /// Requires the `screen` feature, because the guardrail suite comes from
    /// `lingrep-core`. Written once and never edited — the digest it returns is what
    /// every appended observation is bound to.
    Declare(Box<DeclareArgs>),
    /// Append one measured run to a campaign file.
    ///
    /// Rejects runs incomparable with existing observations: a rebuilt arm, a
    /// different machine, or a catalog outside the suite.
    Record(Box<RecordArgs>),
    /// Print the BLAKE3 digest of a file.
    ///
    /// Exists because BLAKE3 has no coreutils spelling and the orchestrator
    /// names frozen arms by content. The same function the manifest and the
    /// exec fingerprint use, so a frozen arm's name and its recorded digest
    /// cannot differ.
    Digest {
        #[arg(value_name = "FILE")]
        file: std::path::PathBuf,
    },
    /// Read JSONL on stdin and print its normalized digest.
    ///
    /// The same number `record --output` computes, taken as the run writes it
    /// rather than after materializing it. Consumes its whole input even after
    /// a parse failure: exiting early would `EPIPE` the process being measured
    /// and turn a correct run into a recorded failure.
    ///
    /// Requires the `screen` feature, because the field list it neutralizes
    /// belongs to `ling`.
    DigestStream,
    /// Print remaining required runs for each catalog in the campaign suite.
    ///
    /// One TSV row per catalog: path, required coverage, blocks recorded. The
    /// coverage table is the tier rules, and an operator should not have to
    /// hold them in their head to know what to run next.
    ///
    /// Pure, like `adjudicate`.
    Plan {
        #[arg(long, value_name = "FILE")]
        campaign: std::path::PathBuf,
        /// Print only catalogs with remaining required runs.
        #[arg(long)]
        outstanding: bool,
    },
    /// Print the sealed command for one catalog, one argument per line.
    ///
    /// The runner executes what this prints rather than reassembling the
    /// command itself, so the seal is the single source of the command instead
    /// of a copy that has to be kept in step with one. Pure.
    Template {
        #[arg(long, value_name = "FILE")]
        campaign: std::path::PathBuf,
        /// The catalog under measurement, by repository-relative path.
        #[arg(long, value_name = "PATH")]
        catalog: String,
    },
    /// Change which metric a campaign's verdict turns on, after the fact.
    ///
    /// Appends to an unsealed, append-only log; the manifest records what
    /// was declared and the report names both. It exists because answering
    /// "what do these observations indicate on `user`?" once cost a second
    /// sixty-four-run campaign, when every observation already carried every
    /// metric.
    ///
    /// It does not widen the alpha budget: that already covers the whole
    /// verdict-bearing family, because the report prints all of it on every
    /// run. Pure but for the write, which is why it is not `adjudicate`.
    Reselect {
        #[arg(long, value_name = "FILE")]
        campaign: std::path::PathBuf,
        #[arg(long, value_name = "METRIC")]
        primary_metric: String,
        /// Why. Required and non-blank.
        #[arg(long, value_name = "TEXT")]
        reason: String,
    },
    /// Adjudicate a campaign file and render its results document.
    ///
    /// Pure: requires no dictionary, no corpus, and no `screen` feature. A
    /// campaign carries the guardrail suite it was declared against, so this
    /// produces the same output on any machine and at any later commit.
    /// produces the same output on any machine and at any later commit.
    Adjudicate {
        /// The campaign JSON to read.
        #[arg(long, value_name = "FILE")]
        campaign: std::path::PathBuf,
        /// Print the verdict alone rather than the full results document.
        #[arg(long)]
        verdict_only: bool,
    },
}

/// The declaration a campaign is sealed with.
///
/// Long, and each field earns it: this is `optimizing-performance`'s steps 1
/// and 4 written down before anything is measured, which is the step agents
/// skip.
#[derive(Debug, clap::Args)]
struct DeclareArgs {
    /// Where to write the campaign file. Never overwritten.
    #[arg(long, value_name = "FILE")]
    out: std::path::PathBuf,
    /// A short slug, used as the results document's title.
    #[arg(long, value_name = "NAME")]
    campaign: String,
    /// Evidence tier: 1, 2 or 3.
    #[arg(long)]
    tier: u8,
    /// The dominant constraint, named from a profile before any change.
    #[arg(long, value_name = "BOUND")]
    bound: String,
    /// What this campaign is likely to hit next.
    #[arg(long, value_name = "BOUND")]
    runner_up_bound: Option<String>,
    /// The profile that named the bound, with its attribution in numbers.
    #[arg(long, value_name = "TEXT")]
    profile_evidence: String,
    /// The leverage-ladder rung this change occupies.
    #[arg(long, value_name = "RUNG")]
    ladder_rung: String,
    /// The catalog the primary metric is read on.
    #[arg(long, value_name = "PATH")]
    primary_catalog: String,
    /// The metric the verdict turns on.
    #[arg(long, value_name = "METRIC", default_value = "wall")]
    primary_metric: String,
    /// A runtime regime this change can affect. Repeat for several; catalogs
    /// exercising one get paired blocks rather than a screen pair.
    #[arg(long = "affected-regime", value_name = "TAG")]
    affected_regimes: Vec<String>,
    /// A supported mode this campaign measures. Tier 1 requires one pair each.
    #[arg(long = "mode", value_name = "NAME")]
    modes: Vec<String>,
    /// The smallest win that would pay for this change's permanent cost.
    #[arg(long, value_name = "PCT", default_value_t = 1.0)]
    minimum_useful_effect_pct: f64,
    /// A guardrail ceiling, as `metric=pct`. Repeat to override one.
    #[arg(long = "guardrail", value_name = "METRIC=PCT")]
    guardrails: Vec<String>,
    /// Block counts to escalate through. Defaults to the tier's ladder.
    #[arg(long, value_name = "N", value_delimiter = ',')]
    ladder: Vec<usize>,
    /// An absolute goal whose achievement is a reason to stop.
    #[arg(long, value_name = "PATH", requires = "target_below")]
    target_catalog: Option<String>,
    #[arg(long, value_name = "METRIC", default_value = "wall")]
    target_metric: String,
    #[arg(long, value_name = "VALUE", requires = "target_catalog")]
    target_below: Option<f64>,
    /// The cargo profile the arms were built with. `lto` and `codegen-units`
    /// are read from the workspace rather than accepted as a claim.
    #[arg(long, value_name = "NAME", default_value = "release")]
    build_profile: String,
    /// A workload file this campaign measures over, rehashed before every
    /// run. Repeat. Use for a direct corpus; a prepared artifact is
    /// `--workload-artifact`.
    #[arg(long = "workload-file", value_name = "PATH")]
    workload_files: Vec<String>,
    /// A prepared V1 artifact this campaign measures over. Its identity is
    /// read from its own manifest — token and location component digests, the
    /// dictionary and token schema, and the document inventory — and reread
    /// before every run. Repeat.
    #[arg(long = "workload-artifact", value_name = "PATH")]
    workload_artifacts: Vec<String>,
    /// Default prepared artifact substituted for `{corpus}`. The campaign
    /// runner supplies this from its existing `--corpus` option.
    #[arg(long, value_name = "PATH")]
    default_corpus: Option<String>,
    /// Override the prepared artifact for one catalog. Repeat as
    /// `CATALOG=PATH`; requires `--default-corpus`.
    #[arg(
        long = "catalog-corpus",
        value_name = "CATALOG=PATH",
        requires = "default_corpus"
    )]
    catalog_corpora: Vec<String>,
    #[arg(long, value_name = "PATH")]
    arm_a: std::path::PathBuf,
    #[arg(long, value_name = "COMMIT", default_value = "unknown")]
    arm_a_commit: String,
    #[arg(long, value_name = "LABEL", default_value = "baseline")]
    arm_a_label: String,
    #[arg(long, value_name = "PATH")]
    arm_b: std::path::PathBuf,
    #[arg(long, value_name = "COMMIT", default_value = "unknown")]
    arm_b_commit: String,
    #[arg(long, value_name = "LABEL", default_value = "candidate")]
    arm_b_label: String,
    /// The worker pool every run is executed with. Sealed, because occupancy
    /// is divided by it and a pool that moved mid-campaign makes the arms
    /// incomparable.
    #[arg(long, value_name = "N")]
    workers: Option<u32>,
    /// Pipe each run's output through `digest-stream` rather than writing it
    /// to a file first. Sealed, and `record` refuses a run that disagrees.
    ///
    /// Nothing reads the bytes except the hasher, and one catalog's output is
    /// ~42 GB per run. This trades those writes for a hashing consumer, which
    /// is a change in what is measured.
    #[arg(long)]
    stream_output: bool,
    /// The command every run executes, after `--`, with `{catalog}` where the
    /// catalog under measurement goes and, for a mapped prepared campaign,
    /// `{corpus}` where that catalog's artifact goes. Sealed by the manifest
    /// digest, so a campaign cannot change what it measures halfway through
    /// and two campaigns measuring different things cannot carry the same
    /// seal.
    #[arg(last = true, value_name = "ARGV")]
    command_template: Vec<String>,
}

/// One measured run, plus everything needed to fingerprint what produced it.
#[derive(Debug, clap::Args)]
struct RecordArgs {
    #[arg(long, value_name = "FILE")]
    campaign: std::path::PathBuf,
    #[arg(long, value_name = "PATH")]
    catalog: String,
    /// `a` or `b`.
    #[arg(long)]
    arm: String,
    #[arg(long)]
    block: usize,
    /// Position within the block: 0..4 for a block, 0..2 for a screen pair.
    #[arg(long)]
    order: usize,
    #[arg(long, value_name = "NAME")]
    mode: Option<String>,
    /// The run exited non-zero. Recorded, and refused at adjudication.
    #[arg(long)]
    failed: bool,
    #[arg(long, value_name = "SECONDS")]
    wall: f64,
    #[arg(long, value_name = "SECONDS")]
    user: f64,
    #[arg(long, value_name = "SECONDS")]
    sys: f64,
    #[arg(long, value_name = "KB")]
    peak_rss_kb: f64,
    /// The arm binary, rehashed now rather than trusted from the manifest.
    #[arg(long, value_name = "PATH")]
    binary: std::path::PathBuf,
    /// The run's JSONL output, digested here with the run-variant fields
    /// nulled. Use `--output-digest` instead when the run was streamed.
    #[arg(long, value_name = "FILE", required_unless_present = "output_digest")]
    output: Option<std::path::PathBuf>,
    /// The digest `lingrep-perf digest-stream` printed for this run's output.
    ///
    /// The same number `--output` would produce, taken as the run wrote it.
    /// It exists because one catalog's output is ~42 GB that nothing reads
    /// except the hasher.
    #[arg(long, value_name = "DIGEST", conflicts_with = "output")]
    output_digest: Option<String>,
    /// A file input to rehash. Repeat. Corpus artifacts are excluded on
    /// purpose: V1 authenticates its own components.
    #[arg(long = "input", value_name = "PATH")]
    inputs: Vec<std::path::PathBuf>,
    /// An allowlisted environment entry, as `NAME=VALUE`. Repeat.
    #[arg(long = "env", value_name = "NAME=VALUE")]
    env: Vec<String>,
    #[arg(long, value_name = "CPUS", default_value = "")]
    affinity: String,
    #[arg(long)]
    workers: Option<u32>,
    /// The command line that was run, after `--`.
    #[arg(last = true, value_name = "ARGV")]
    argv: Vec<String>,
}

fn main() -> std::process::ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Command::Work {
            catalog,
            all,
            json,
            baseline,
        } => run_work(catalog, all, json, baseline.as_deref()),
        Command::Plan {
            campaign,
            outstanding,
        } => run_plan(&campaign, outstanding),
        Command::Template { campaign, catalog } => run_template(&campaign, &catalog),
        Command::Reselect {
            campaign,
            primary_metric,
            reason,
        } => run_reselect(&campaign, &primary_metric, &reason),
        Command::Digest { file } => run_digest(&file),
        Command::DigestStream => run_digest_stream(),
        Command::Declare(args) => run_declare(&args),
        Command::Record(args) => run_record(&args),
        Command::Adjudicate {
            campaign,
            verdict_only,
        } => run_adjudicate(&campaign, verdict_only),
    }
}

fn run_template(path: &std::path::Path, catalog: &str) -> std::process::ExitCode {
    let campaign = match read_campaign(path) {
        Ok(campaign) => campaign,
        Err(code) => return code,
    };
    let argv = match campaign.manifest.expected_argv(catalog) {
        Ok(Some(argv)) => argv,
        Ok(None) => {
            eprintln!(
                "lingrep-perf: {} seals no command. It was declared before the command was sealed, \
                 and its runs cannot be reproduced from it.",
                path.display()
            );
            return std::process::ExitCode::FAILURE;
        }
        Err(error) => {
            eprintln!("lingrep-perf: {error}");
            return std::process::ExitCode::FAILURE;
        }
    };
    for argument in argv {
        println!("{argument}");
    }
    std::process::ExitCode::SUCCESS
}

fn run_reselect(path: &std::path::Path, metric: &str, reason: &str) -> std::process::ExitCode {
    let mut campaign = match read_campaign(path) {
        Ok(campaign) => campaign,
        Err(code) => return code,
    };
    let metric: lingrep_perf::campaign::Metric = match parse_enum("primary-metric", metric) {
        Ok(metric) => metric,
        Err(error) => {
            eprintln!("lingrep-perf: {error}");
            return std::process::ExitCode::FAILURE;
        }
    };
    let was = campaign.effective_primary_metric();
    if let Err(refusal) = lingrep_perf::campaign::reselect(&mut campaign, metric, reason) {
        eprintln!("lingrep-perf: {refusal}");
        return std::process::ExitCode::FAILURE;
    }
    if let Err(error) = lingrep_perf::campaign::write(path, &campaign) {
        eprintln!("lingrep-perf: writing {} failed: {error}", path.display());
        return std::process::ExitCode::FAILURE;
    }
    println!(
        "{} -> {}: {reason}",
        was.name(),
        campaign.effective_primary_metric().name()
    );
    std::process::ExitCode::SUCCESS
}

fn run_plan(path: &std::path::Path, outstanding_only: bool) -> std::process::ExitCode {
    let campaign = match read_campaign(path) {
        Ok(campaign) => campaign,
        Err(code) => return code,
    };
    // On stderr, and never as a column: the TSV is a contract the runner
    // reads positionally, and it checks the field count precisely because a
    // widened row was once read as garbage positions.
    if let Some(estimate) = lingrep_perf::campaign::wall_planning_estimate(&campaign) {
        eprintln!("lingrep-perf: {estimate}");
    }
    for row in lingrep_perf::plan::plan(&campaign) {
        if outstanding_only && !row.outstanding() {
            continue;
        }
        println!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            row.catalog,
            row.kind,
            row.required,
            row.complete,
            row.partial_runs,
            row.next_block,
            row.next_order,
            row.next_width,
            row.reason
        );
    }
    std::process::ExitCode::SUCCESS
}

fn read_campaign(
    path: &std::path::Path,
) -> Result<lingrep_perf::campaign::Campaign, std::process::ExitCode> {
    let text = std::fs::read_to_string(path).map_err(|error| {
        eprintln!("lingrep-perf: {} is unreadable: {error}", path.display());
        std::process::ExitCode::FAILURE
    })?;
    serde_json::from_str(&text).map_err(|error| {
        eprintln!(
            "lingrep-perf: {} is not a campaign: {error}",
            path.display()
        );
        std::process::ExitCode::FAILURE
    })
}

fn run_adjudicate(path: &std::path::Path, verdict_only: bool) -> std::process::ExitCode {
    use lingrep_perf::campaign::adjudicate;

    let campaign = match read_campaign(path) {
        Ok(campaign) => campaign,
        Err(code) => return code,
    };

    match adjudicate(&campaign) {
        // A refusal is its own exit status, distinct from every verdict. An
        // invalid campaign must not read as a decision.
        Err(refusal) => {
            eprintln!("lingrep-perf: refused to adjudicate: {refusal}");
            std::process::ExitCode::from(2)
        }
        Ok(adjudication) => {
            if verdict_only {
                println!("{:?}", adjudication.verdict);
            } else {
                print!("{}", lingrep_perf::report::render(&campaign, &adjudication));
            }
            std::process::ExitCode::SUCCESS
        }
    }
}

/// Parse an enum by its wire spelling.
///
/// Through `serde` rather than `clap::ValueEnum` so the campaign module stays
/// free of a CLI dependency, and so the spellings a caller types are exactly
/// the spellings a campaign file carries. Serde's error already names every
/// accepted variant.
fn parse_enum<T: serde::de::DeserializeOwned>(flag: &str, value: &str) -> Result<T, String> {
    serde_json::from_value(serde_json::Value::String(value.to_owned()))
        .map_err(|error| format!("--{flag}: {error}"))
}

#[cfg(feature = "screen")]
fn build_declaration(
    args: &DeclareArgs,
) -> Result<
    (
        lingrep_perf::orchestrate::Declaration,
        std::collections::BTreeMap<String, String>,
    ),
    String,
> {
    use lingrep_perf::campaign::{
        Arms, Metric, OutputCapture, Target, TemplateArg, Tier, WorkloadKind,
    };
    use lingrep_perf::orchestrate::{arm, known_regimes, read_build_profile};
    use std::collections::{BTreeMap, BTreeSet};

    let tier = match args.tier {
        1 => Tier::One,
        2 => Tier::Two,
        3 => Tier::Three,
        other => return Err(format!("--tier: {other} is not 1, 2 or 3")),
    };
    let primary_metric: Metric = parse_enum("primary-metric", &args.primary_metric)?;

    let mut command_template = Vec::with_capacity(args.command_template.len());
    for token in &args.command_template {
        command_template.push(TemplateArg::parse(token).map_err(|error| format!("--: {error}"))?);
    }

    // A mistyped regime is the quiet failure here: it selects no catalog, so
    // every guardrail silently drops to a screen pair and the campaign looks
    // cheaper rather than wrong.
    let known = known_regimes();
    let mut affected_regimes = BTreeSet::new();
    for regime in &args.affected_regimes {
        if !known.contains(regime) {
            return Err(format!(
                "--affected-regime: {regime} is not a runtime regime. The suite exercises: {}",
                known.iter().cloned().collect::<Vec<_>>().join(", ")
            ));
        }
        affected_regimes.insert(regime.clone());
    }

    let mut guardrail_ceilings_pct: BTreeMap<Metric, f64> = [
        (Metric::Wall, 2.0),
        (Metric::User, 2.0),
        (Metric::PeakRss, 5.0),
    ]
    .into_iter()
    .collect();
    for entry in &args.guardrails {
        let (metric, ceiling) = entry
            .split_once('=')
            .ok_or_else(|| format!("--guardrail: {entry} is not `metric=pct`"))?;
        let metric: Metric = parse_enum("guardrail", metric)?;
        let ceiling: f64 = ceiling
            .parse()
            .map_err(|_| format!("--guardrail: {ceiling} is not a number"))?;
        guardrail_ceilings_pct.insert(metric, ceiling);
    }

    let target = match (&args.target_catalog, args.target_below) {
        (Some(catalog), Some(below)) => Some(Target {
            catalog: catalog.clone(),
            metric: parse_enum("target-metric", &args.target_metric)?,
            below,
        }),
        _ => None,
    };

    let mut workload = Vec::new();
    for path in &args.workload_files {
        workload.push(
            lingrep_perf::orchestrate::workload_input(path, WorkloadKind::File)
                .map_err(|error| error.to_string())?,
        );
    }
    for path in &args.workload_artifacts {
        workload.push(
            lingrep_perf::orchestrate::workload_input(path, WorkloadKind::PreparedArtifact)
                .map_err(|error| error.to_string())?,
        );
    }
    if workload.is_empty() {
        return Err(
            "a campaign must declare what it measures over: --workload-file or \
             --workload-artifact"
                .to_owned(),
        );
    }

    let mut catalog_corpora = BTreeMap::new();
    if let Some(default) = &args.default_corpus {
        for catalog in lingrep_perf::orchestrate::campaign_catalogs() {
            catalog_corpora.insert(catalog.path, default.clone());
        }
        let mut overridden = BTreeSet::new();
        for entry in &args.catalog_corpora {
            let (catalog, corpus) = entry
                .split_once('=')
                .ok_or_else(|| format!("--catalog-corpus: {entry:?} is not `CATALOG=PATH`"))?;
            if catalog.is_empty() || corpus.is_empty() {
                return Err(format!(
                    "--catalog-corpus: {entry:?} must have a nonempty catalog and path"
                ));
            }
            if !overridden.insert(catalog.to_owned()) {
                return Err(format!(
                    "--catalog-corpus: {catalog} is overridden more than once"
                ));
            }
            if !catalog_corpora.contains_key(catalog) {
                return Err(format!(
                    "--catalog-corpus: {catalog} is not in this campaign's suite"
                ));
            }
            catalog_corpora.insert(catalog.to_owned(), corpus.to_owned());
        }
    }

    let root = lingrep_core::testing::catalogs::repository_root();
    Ok((
        lingrep_perf::orchestrate::Declaration {
            campaign: args.campaign.clone(),
            tier,
            bound: parse_enum("bound", &args.bound)?,
            runner_up_bound: args
                .runner_up_bound
                .as_deref()
                .map(|bound| parse_enum("runner-up-bound", bound))
                .transpose()?,
            profile_evidence: args.profile_evidence.clone(),
            ladder_rung: parse_enum("ladder-rung", &args.ladder_rung)?,
            target,
            primary_catalog: args.primary_catalog.clone(),
            primary_metric,
            affected_regimes,
            modes: args.modes.clone(),
            minimum_useful_effect_pct: args.minimum_useful_effect_pct,
            guardrail_ceilings_pct,
            escalation_ladder: if args.ladder.is_empty() {
                tier.default_ladder()
            } else {
                args.ladder.clone()
            },
            build: read_build_profile(&root, &args.build_profile).map_err(|e| e.to_string())?,
            workload,
            command_template,
            workers: args.workers,
            output_capture: if args.stream_output {
                OutputCapture::Stream
            } else {
                OutputCapture::File
            },
            arms: Arms {
                a: arm(&args.arm_a_label, &args.arm_a_commit, &args.arm_a)
                    .map_err(|e| e.to_string())?,
                b: arm(&args.arm_b_label, &args.arm_b_commit, &args.arm_b)
                    .map_err(|e| e.to_string())?,
            },
        },
        catalog_corpora,
    ))
}

#[cfg(feature = "screen")]
fn run_digest(file: &std::path::Path) -> std::process::ExitCode {
    match lingrep_perf::orchestrate::digest_file(file) {
        Ok(digest) => {
            println!("{digest}");
            std::process::ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("lingrep-perf: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}

#[cfg(feature = "screen")]
fn run_declare(args: &DeclareArgs) -> std::process::ExitCode {
    use lingrep_perf::campaign::Role;

    // Never overwritten, for the same reason artifact publication is not: a
    // campaign that can be re-declared in place can be re-declared after the
    // numbers are in.
    if args.out.exists() {
        eprintln!(
            "lingrep-perf: {} already exists. A manifest is written once; delete it explicitly \
             to start over.",
            args.out.display()
        );
        return std::process::ExitCode::FAILURE;
    }

    let (declaration, catalog_corpora) = match build_declaration(args) {
        Ok(declaration) => declaration,
        Err(error) => {
            eprintln!("lingrep-perf: {error}");
            return std::process::ExitCode::FAILURE;
        }
    };
    let campaign =
        match lingrep_perf::orchestrate::declare_with_catalog_corpora(declaration, catalog_corpora)
        {
            Ok(campaign) => campaign,
            Err(error) => {
                eprintln!("lingrep-perf: {error}");
                return std::process::ExitCode::FAILURE;
            }
        };
    let written = serde_json::to_vec_pretty(&campaign)
        .map_err(|error| error.to_string())
        .and_then(|bytes| std::fs::write(&args.out, bytes).map_err(|error| error.to_string()));
    if let Err(error) = written {
        eprintln!(
            "lingrep-perf: writing {} failed: {error}",
            args.out.display()
        );
        return std::process::ExitCode::FAILURE;
    }
    // A floor catalog is "measured, but never evidence that an optimization is
    // broadly effective" — so it is reported and excluded from the aggregate.
    // Naming it the primary is legitimate for a decode-path change and a
    // mistake for anything else, which is why this warns rather than refuses.
    if campaign
        .manifest
        .catalogs
        .iter()
        .any(|catalog| catalog.path == args.primary_catalog && catalog.role == Role::Floor)
    {
        eprintln!(
            "lingrep-perf: warning: {} is a floor catalog, so the verdict will turn on a case \
             that is not evidence of broad effectiveness, and it will be excluded from the \
             aggregate it is the primary of.",
            args.primary_catalog
        );
    }

    // Streaming makes the hasher the pipeline's rate limiter, and a limiter
    // paces both arms equally — so a catalog that publishes faster than the
    // hasher consumes reports wall ~1.0 whatever the change did. This warns
    // rather than refuses because it is conditional on output rate, which is
    // not knowable here: the catalogs it will bite are discovered at
    // adjudication, where the wall-versus-user disagreement names them.
    if campaign.manifest.output_capture == lingrep_perf::campaign::OutputCapture::Stream
        && campaign.manifest.primary_metric == lingrep_perf::campaign::Metric::Wall
    {
        eprintln!(
            "lingrep-perf: warning: --stream-output with a `wall` primary. The hasher paces both \
             arms on any catalog that publishes faster than it consumes, so wall there will \
             read ~1.0 whatever the change did — including, possibly, on the primary. Prefer \
             `--primary-metric user`, or drop --stream-output if the disk allows. The \
             adjudication names every catalog this happened to."
        );
    }

    // The digest on stdout, so the orchestrator can echo it into a log without
    // parsing the campaign file.
    println!("{}", campaign.manifest_digest);
    std::process::ExitCode::SUCCESS
}

#[cfg(feature = "screen")]
fn run_digest_stream() -> std::process::ExitCode {
    let stdin = std::io::stdin();
    let mut reader = std::io::BufReader::new(stdin.lock());
    match lingrep_perf::orchestrate::stream_digest(&mut reader) {
        Ok(digest) => {
            println!("{digest}");
            std::process::ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("lingrep-perf: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}

#[cfg(not(feature = "screen"))]
fn run_digest_stream() -> std::process::ExitCode {
    refuse_without_screen("digest-stream")
}

#[cfg(feature = "screen")]
fn run_record(args: &RecordArgs) -> std::process::ExitCode {
    use lingrep_perf::orchestrate::{Execution, Measurement, OutputIdentity, record};

    let execution = Execution {
        argv: &args.argv,
        env: &args.env,
        affinity: &args.affinity,
        workers: args.workers,
        binary: &args.binary,
        inputs: &args.inputs,
    };
    let measurement = Measurement {
        catalog: args.catalog.clone(),
        arm: args.arm.clone(),
        block: args.block,
        order: args.order,
        mode: args.mode.clone(),
        ok: !args.failed,
        wall_s: args.wall,
        user_s: args.user,
        sys_s: args.sys,
        peak_rss_kb: args.peak_rss_kb,
    };
    let output = match (&args.output, &args.output_digest) {
        (Some(path), _) => OutputIdentity::File(path),
        (None, Some(digest)) => OutputIdentity::Digest(digest),
        (None, None) => {
            eprintln!("lingrep-perf: --output FILE or --output-digest DIGEST is required");
            return std::process::ExitCode::FAILURE;
        }
    };
    match record(&args.campaign, measurement, &execution, &output) {
        Ok(observation) => {
            println!(
                "{} {} block {} order {}: wall {:.3}s",
                observation.catalog,
                observation.arm,
                observation.block,
                observation.order,
                observation.wall_s
            );
            std::process::ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("lingrep-perf: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}

#[cfg(not(feature = "screen"))]
fn run_digest(_file: &std::path::Path) -> std::process::ExitCode {
    refuse_without_screen("digest")
}

#[cfg(not(feature = "screen"))]
fn run_declare(_args: &DeclareArgs) -> std::process::ExitCode {
    refuse_without_screen("declare")
}

#[cfg(not(feature = "screen"))]
fn run_record(_args: &RecordArgs) -> std::process::ExitCode {
    refuse_without_screen("record")
}

/// Refused rather than answered wrongly, for the same reason `work` is: a
/// campaign step that quietly did nothing reads exactly like one that worked.
#[cfg(not(feature = "screen"))]
fn refuse_without_screen(command: &str) -> std::process::ExitCode {
    eprintln!(
        "lingrep-perf: `{command}` needs the `screen` feature, which links the workspace.\n\
         Run it through `scripts/perf_campaign.sh`, or build with `--features screen`."
    );
    std::process::ExitCode::FAILURE
}

#[cfg(feature = "screen")]
fn run_work(
    catalog: Option<String>,
    all: bool,
    json: bool,
    baseline: Option<&std::path::Path>,
) -> std::process::ExitCode {
    use lingrep_perf::work::{CatalogComparison, ScopeSelection, compare, measure};
    use std::io::Write;

    let scope = match (catalog, all) {
        (Some(catalog), _) => ScopeSelection::One(catalog),
        (None, true) => ScopeSelection::AllDiscovered,
        (None, false) => ScopeSelection::CampaignSuite,
    };
    let report = match measure(&scope) {
        Ok(report) => report,
        Err(error) => {
            eprintln!("lingrep-perf: {error}");
            return std::process::ExitCode::FAILURE;
        }
    };

    if json {
        let mut stdout = std::io::stdout().lock();
        let written = serde_json::to_writer_pretty(&mut stdout, &report)
            .map_err(|error| error.to_string())
            .and_then(|()| stdout.write_all(b"\n").map_err(|error| error.to_string()));
        return match written {
            Ok(()) => std::process::ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("lingrep-perf: writing the report failed: {error}");
                std::process::ExitCode::FAILURE
            }
        };
    }

    let Some(baseline) = baseline else {
        for (name, measurement) in &report.catalogs {
            let sentences = measurement.sentences;
            let work = &measurement.work;
            let counters = work
                .counters()
                .iter()
                .map(|(counter, value)| format!("{counter}={value}"))
                .collect::<Vec<_>>()
                .join(" ");
            let silent = if work.is_silent() { "  [no work]" } else { "" };
            println!("{name}\tsentences={sentences}\t{counters}{silent}");
        }
        return std::process::ExitCode::SUCCESS;
    };

    let text = match std::fs::read_to_string(baseline) {
        Ok(text) => text,
        Err(error) => {
            eprintln!(
                "lingrep-perf: {} is unreadable: {error}",
                baseline.display()
            );
            return std::process::ExitCode::FAILURE;
        }
    };
    let before = match serde_json::from_str(&text) {
        Ok(before) => before,
        Err(error) => {
            eprintln!(
                "lingrep-perf: {} is not a work report: {error}",
                baseline.display()
            );
            return std::process::ExitCode::FAILURE;
        }
    };

    let comparisons = match compare(&before, &report) {
        Ok(comparisons) => comparisons,
        Err(error) => {
            eprintln!("lingrep-perf: {error}");
            return std::process::ExitCode::FAILURE;
        }
    };

    let mut moved = false;
    let mut incomparable = false;
    for (name, comparison) in comparisons {
        match comparison {
            CatalogComparison::InputChanged {
                before_sentences,
                after_sentences,
            } => {
                incomparable = true;
                println!(
                    "{name}\tINPUT CHANGED (sentences {before_sentences} -> {after_sentences}); \
                     counters are not comparable across a changed catalog or fixture"
                );
            }
            CatalogComparison::Compared { deltas } => {
                if deltas.is_empty() {
                    println!("{name}\tunchanged");
                    continue;
                }
                moved = true;
                for delta in deltas {
                    println!(
                        "{name}\t{}\t{} -> {} ({:+})",
                        delta.counter,
                        delta.before,
                        delta.after,
                        delta.change()
                    );
                }
            }
            CatalogComparison::Added => {
                moved = true;
                println!("{name}\tADDED since the baseline");
            }
            CatalogComparison::Removed => {
                moved = true;
                println!("{name}\tREMOVED since the baseline");
            }
        }
    }

    if incomparable {
        eprintln!(
            "\nlingrep-perf: at least one catalog was measured over different input than the \
             baseline was, so this run does not answer whether matcher work changed."
        );
        return std::process::ExitCode::FAILURE;
    }
    if !moved {
        println!(
            "\nNo counter moved. This is a cost-per-unit change, not a work change — \
             it needs a timing campaign, and it is not a null result."
        );
    }
    std::process::ExitCode::SUCCESS
}

#[cfg(not(feature = "screen"))]
fn run_work(
    _catalog: Option<String>,
    _all: bool,
    _json: bool,
    _baseline: Option<&std::path::Path>,
) -> std::process::ExitCode {
    // Refused rather than reported as zero work. A screen that silently
    // measured nothing would read exactly like a change that moved nothing,
    // which is the one answer this command must never fake.
    eprintln!(
        "lingrep-perf: this binary was built without the `screen` feature, so no matcher work \
         counters are compiled in.\n\
         Run `just perf-work`, or build with `--features screen`."
    );
    std::process::ExitCode::FAILURE
}
