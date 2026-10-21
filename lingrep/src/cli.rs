//! Public `ling` command tree and bounded-resource arguments.

use std::path::{Path, PathBuf};

use clap::{Args, CommandFactory, FromArgMatches, Parser, Subcommand, ValueEnum};
use lingrep_cli_common::{MarkdownReaderArgs, TerminalSafeText};
use lingrep_core::corpus::{CorpusFormat, TraversalLimits};
use lingrep_core::execution::{
    DEFAULT_MAX_INPUT_RECORD_BYTES, DEFAULT_MAX_SENTENCE_BYTES, DEFAULT_MAX_STAGE_INPUT_BYTES,
    DEFAULT_MAX_STAGE_SENTENCES, DEFAULT_MAX_TOKENIZED_BYTES, ExecutionLimits,
};

use crate::error::CliError;
use crate::output::ContextColorTheme;
pub use crate::presentation::ProgressMode;
use crate::presentation::{ColorMode, ExplicitLimit, OutputMode};

const DEFAULT_MAX_TEMP_BYTES: u64 = 16 * 1024 * 1024 * 1024;

/// The flag that sets each [`ExecutionLimits`] field.
///
/// `ExecutionLimits` owns every rule relating one bound to another and names
/// the violated field in its own vocabulary. This is the only place that
/// vocabulary is translated back to the one a researcher typed.
fn limit_flag(field: &str) -> &str {
    match field {
        "max_sentence_bytes" => "--max-sentence-bytes",
        "max_input_record_bytes" => "--max-record-bytes",
        "max_stage_sentences" => "--batch-sentences",
        "max_stage_input_bytes" => "--batch-raw-bytes",
        "max_tokenized_bytes" => "--max-tokenized-bytes",
        other => other,
    }
}

/// Renders an `ExecutionLimits` rejection using flag names on both sides, so
/// "must be at least max_sentence_bytes" reads as the flag that supplies it.
fn limit_diagnostic(error: &lingrep_core::execution::ExecutionError) -> String {
    match error {
        lingrep_core::execution::ExecutionError::InvalidLimit { field, message } => {
            let message = message.replace("max_sentence_bytes", limit_flag("max_sentence_bytes"));
            format!("{} {message}", limit_flag(field))
        }
        other => other.to_string(),
    }
}

fn positive_u64(raw: &str) -> Result<u64, String> {
    raw.parse::<u64>()
        .map_err(|error| error.to_string())
        .and_then(|value| {
            (value > 0)
                .then_some(value)
                .ok_or_else(|| "must be greater than zero".into())
        })
}

fn positive_usize(raw: &str) -> Result<usize, String> {
    raw.parse::<usize>()
        .map_err(|error| error.to_string())
        .and_then(|value| {
            (value > 0)
                .then_some(value)
                .ok_or_else(|| "must be greater than zero".into())
        })
}

/// The pool width a run gets when `--workers` says nothing.
///
/// The smaller of what this process is *allowed* to run on and how many
/// physical cores exist. Both halves are load-bearing and neither alone is
/// right:
///
/// `available_parallelism` respects a cgroup CPU quota, which
/// `num_cpus::get_physical` does not — a container limited to four CPUs on a
/// large host must not be handed the host's core count. But on an SMT machine
/// it reports *logical* CPUs, and this workload does not want them. Measured on
/// a 16-core/32-thread 7950X3D over `data-archive/corpus-2000.jsonl`, 32
/// workers was the worst or near-worst point on every path — 3.91 s against
/// 3.59 s at 16 direct, 2.21 s against 2.12 s prepared, 2.47 s against 2.39 s
/// prepared record-emitting — while burning 30–35% more CPU and ~12% more
/// resident bytes for it. Matching and analysis are both memory-latency bound
/// and Rayon spin-waits, so an SMT sibling mostly contends with its twin.
fn default_workers() -> usize {
    let permitted = std::thread::available_parallelism().map_or(1, usize::from);
    permitted.min(num_cpus::get_physical()).max(1)
}

/// Search Japanese text and CoNLL-U treebanks for token patterns over surface
/// form, lemma, part of speech, morphological features and dependency relations.
#[derive(Debug, Parser)]
#[command(name = "ling", version, after_help = ROOT_AFTER_HELP)]
pub struct Cli {
    /// Write JSONL records and machine-readable JSON diagnostics.
    #[arg(long, global = true, help_heading = "Output")]
    pub json: bool,

    /// Control ANSI styling in human output.
    #[arg(
        long,
        global = true,
        value_enum,
        default_value_t,
        help_heading = "Output"
    )]
    pub color: ColorMode,

    #[command(subcommand)]
    pub command: Command,
}

const ROOT_AFTER_HELP: &str = "\
Examples:
  ling check --pattern '[upos=VERB].'
  ling dictionary install
  ling matches --pattern '[lemma=走る].' novel.txt
  ling summarize --patterns catalog.lx novel.txt
  ling matches --pattern '[upos=NOUN] [dep.rel=case].' treebank.conllu

Japanese text is analyzed with the dictionary installed by
`ling dictionary install`, or one specified by --dict-path or
LINGREP_DICTIONARY_PATH; CoNLL-U input is already analyzed.
Pattern syntax: ling check --help. Command details: ling <COMMAND> --help.";

/// Top-level public operation.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Search a corpus and print each match with its context.
    Matches(MatchesArgs),
    /// Count matches per pattern, grouped by matched surface or declared key.
    ///
    /// If multiple input aliases name the same source, each alias contributes
    /// its own counts.
    Summarize(SummarizeArgs),
    /// Print each sentence's tokens and their attributes.
    Tokenize(TokenizeArgs),
    /// Count the tokens that carry each attribute.
    #[command(name = "coverage")]
    Attributes(AttributesArgs),
    /// Browse pattern counts and matches in an interactive terminal view.
    Explore(ExploreArgs),
    /// Count matches followed only by punctuation to the end of the sentence.
    SentenceFinal(SentenceFinalArgs),
    /// Save analyzed corpora for reuse, inspect them, and list corpus sources.
    Corpus(CorpusArgs),
    /// Validate patterns without reading a corpus; --help lists the syntax.
    #[command(
        after_help = "Run `ling check --help` for the complete lingrep patterns language reference.",
        after_long_help = crate::pattern_reference::PATTERN_REFERENCE
    )]
    Check(CheckArgs),
    /// Download a UniDic dictionary for Japanese analysis.
    Dictionary(DictionaryArgs),
    /// Check that a dictionary, catalog and prepared corpus load.
    Doctor(DoctorArgs),
    /// Show a command's arguments, examples and output formats.
    Describe(DescribeArgs),
    /// List or show the JSONL and TSV output formats.
    #[command(name = "contracts")]
    Schema(SchemaArgs),
    /// Print a shell completion script.
    Completions(CompletionsArgs),
    /// Write a manual page for every command to a directory.
    Manpages(ManpagesArgs),
}

#[derive(Debug, Args)]
pub struct DoctorArgs {
    /// Dictionary to load; overrides LINGREP_DICTIONARY_PATH.
    #[arg(short = 'd', long, value_name = "PATH")]
    pub dict_path: Option<PathBuf>,
    /// lingrep patterns catalog to compile as an independent readiness probe.
    #[arg(short = 'f', long, value_name = "FILE", help_heading = "Catalog")]
    pub patterns: Option<PathBuf>,
    /// Verify that this prepared artifact opens; do not hash stored content.
    #[arg(long, value_name = "ARTIFACT")]
    pub corpus: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct DescribeArgs {
    /// Command to describe; omit to describe the complete command surface.
    #[arg(value_name = "COMMAND", num_args = 0..=2)]
    pub command: Vec<String>,
}

#[derive(Debug, Args)]
pub struct CompletionsArgs {
    /// Shell for which to generate a completion definition.
    #[arg(value_enum)]
    pub shell: CompletionShell,
}

#[derive(Debug, Args)]
pub struct DictionaryArgs {
    #[command(subcommand)]
    pub command: DictionaryCommand,
}

#[derive(Debug, Subcommand)]
pub enum DictionaryCommand {
    /// Download a UniDic dictionary, check its digest, and install it.
    ///
    /// Dictionaries are installed in `$XDG_DATA_HOME/lingrep/dictionaries`
    /// (`~/.local/share/lingrep/dictionaries` when `XDG_DATA_HOME` is unset).
    /// The installed `cwj` dictionary is used when neither `--dict-path` nor
    /// `LINGREP_DICTIONARY_PATH` names one; pass any other with `--dict-path`.
    Install(DictionaryInstallArgs),
}

#[derive(Debug, Args)]
pub struct DictionaryInstallArgs {
    /// Dictionary to install.
    #[arg(value_enum, default_value = "cwj")]
    pub name: crate::packaged_dictionaries::DictionaryName,
}

#[derive(Debug, Args)]
pub struct ManpagesArgs {
    /// Create this directory and write all section-1 manual pages into it.
    #[arg(long, value_name = "DIRECTORY")]
    pub output: PathBuf,
}

#[derive(Debug, Args)]
pub struct SchemaArgs {
    /// Contract to describe; omit to list the compact index.
    #[arg(value_enum, value_name = "CONTRACT", hide_short_help = true)]
    pub contract: Option<crate::protocol::ContractId>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum CompletionShell {
    Bash,
    Zsh,
    Fish,
    Elvish,
    Powershell,
}

#[derive(Debug, Args)]
pub struct CheckArgs {
    /// Execute examples and counterexamples with the selected native analyzer.
    #[arg(long, conflicts_with = "schema")]
    pub verify_examples: bool,
    /// Vibrato dictionary for example verification; overrides LINGREP_DICTIONARY_PATH.
    #[arg(long, requires = "verify_examples", value_name = "PATH")]
    pub dict_path: Option<PathBuf>,
    #[command(flatten)]
    pub resources: ResourceArgs,

    #[command(flatten)]
    pub catalog: CatalogArgs,

    /// Compare another catalog with the primary catalog. Repeat for more files.
    /// Differences are reported without causing the check to fail.
    #[arg(long = "compare-with", value_name = "FILE", action = clap::ArgAction::Append)]
    pub compare_with: Vec<PathBuf>,

    /// Return a failure status when `--compare-with` finds definition drift.
    #[arg(long, requires = "compare_with", help_heading = "Diagnostics")]
    pub fail_on_drift: bool,

    /// Report binding findings against a named token schema without reading a
    /// corpus: referenced attributes absent from the schema are listed, and
    /// catalogs that require unsupported attributes (such as dependency `where`
    /// clauses on a schema without `dep.*`) fail the check.
    #[arg(long, value_enum, value_name = "SCHEMA", help_heading = "Diagnostics")]
    pub schema: Option<CheckSchemaArg>,
}

/// The declared token schemas `check --schema` can bind against.
///
/// These name **schemas**, not analyzers. A morphology layout belongs to the
/// dictionary that supplies the columns, so `unidic-morphology` is what
/// Vibrato produces over any of the packaged UniDic dictionaries.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum CheckSchemaArg {
    /// UniDic morphology, as Vibrato reads it: the portable keys plus `unidic.*`.
    UnidicMorphology,
    /// Sudachi morphology: the portable keys plus `sudachi.*`.
    SudachiMorphology,
    /// Surface-only tokenization: `text` alone.
    Tokenize,
    /// Pretokenized CoNLL-U: portable keys, `feat.*`, `conllu.*`, `dep.*`.
    Conllu,
}

#[derive(Debug, Args)]
pub struct MatchesArgs {
    #[command(flatten)]
    pub catalog: CatalogArgs,
    #[command(flatten)]
    pub input: QueryInputArgs,
    #[command(flatten)]
    pub selection: ScopeArgs,

    #[command(flatten)]
    pub sample: SampleArgs,
    #[command(flatten)]
    pub cache: CacheArgs,
    #[command(flatten)]
    pub resources: ResourceArgs,

    /// Write headerless `lingrep.matches-tsv/v1` records: sequence, pattern,
    /// source, document and sentence ordinals, match span, and context surfaces.
    #[arg(long, conflicts_with = "json", help_heading = "Output")]
    pub tsv: bool,

    /// Color the nearest N context morphemes on each side of the hit.
    #[arg(
        long,
        value_name = "N",
        default_value_t = 5,
        conflicts_with = "tsv",
        help_heading = "Output"
    )]
    pub context_color_depth: usize,

    /// Adapt the context color ramp to the terminal background.
    #[arg(
        long,
        value_enum,
        value_name = "THEME",
        default_value_t,
        conflicts_with = "tsv",
        help_heading = "Output"
    )]
    pub context_color_theme: ContextColorTheme,

    /// Align every hit at a fixed display column in human output.
    #[arg(
        long,
        conflicts_with_all = ["json", "tsv"],
        help_heading = "Output"
    )]
    pub align_hits: bool,

    /// Insert this printable text between displayed morphemes.
    #[arg(
        long,
        value_name = "TEXT",
        default_value = "",
        conflicts_with_all = ["json", "tsv"],
        help_heading = "Output"
    )]
    pub morpheme_separator: TerminalSafeText,

    /// Publish @document.KEY, @sentence.KEY or @division(KIND).KEY in JSONL matches (repeatable).
    #[arg(
        long = "source-metadata",
        value_name = "KEY",
        conflicts_with = "tsv",
        help_heading = "Output"
    )]
    pub source_metadata: Vec<String>,

    /// Include this many tokens before and after each match.
    #[arg(long, help_heading = "Output")]
    pub context: Option<usize>,

    /// Publish only the first N matches in normal order; `all` removes the limit.
    #[arg(long, value_name = "N|all", help_heading = "Output")]
    pub limit: Option<ExplicitLimit<u64>>,

    /// Scan the complete scope and retain at most N JSON match records (64 MiB maximum).
    #[arg(long, value_parser = clap::value_parser!(u32).range(1..=100_000), conflicts_with_all = ["limit", "tsv"], help_heading = "Output")]
    pub retain: Option<u32>,

    /// Seed for reproducible retained-match selection.
    #[arg(
        long,
        default_value_t = 0,
        requires = "retain",
        help_heading = "Output"
    )]
    pub retain_seed: u64,

    /// Publish matcher output before applying the catalog's policy block.
    #[arg(long, help_heading = "Matching")]
    pub raw_matches: bool,

    /// Run the query and report policy decisions for at most N published records.
    /// `--plan` only validates and prints the invocation.
    #[arg(
        long,
        value_name = "N",
        conflicts_with = "raw_matches",
        help_heading = "Diagnostics"
    )]
    pub trace_policy: Option<usize>,

    /// Validate inputs and print the execution plan without reading corpus records.
    #[arg(long, help_heading = "Diagnostics")]
    pub plan: bool,
}

#[derive(Debug, Args)]
pub struct SummarizeArgs {
    #[command(flatten)]
    pub catalog: CatalogArgs,
    #[command(flatten)]
    pub input: QueryInputArgs,
    #[command(flatten)]
    pub selection: ScopeArgs,

    #[command(flatten)]
    pub sample: SampleArgs,
    #[command(flatten)]
    pub cache: CacheArgs,
    #[command(flatten)]
    pub resources: ResourceArgs,

    /// Write headerless `lingrep.summarize-tsv/v1` records with pattern,
    /// rank, typed group, and exact count columns.
    #[arg(long, conflicts_with = "json", help_heading = "Output")]
    pub tsv: bool,

    /// Publish at most N exact group-count rows per pattern; `all` removes the limit.
    #[arg(long, value_name = "N|all", help_heading = "Output")]
    pub top: Option<ExplicitLimit<u64>>,

    /// Run the query and report policy decisions for at most N published records.
    /// `--plan` only validates and prints the invocation.
    #[arg(long, value_name = "N", help_heading = "Diagnostics")]
    pub trace_policy: Option<usize>,

    /// Validate inputs and print the execution plan without reading corpus records.
    #[arg(long, help_heading = "Diagnostics")]
    pub plan: bool,
}

#[derive(Debug, Args)]
pub struct CorpusArgs {
    #[command(subcommand)]
    pub command: CorpusCommand,
}

#[derive(Debug, Subcommand)]
pub enum CorpusCommand {
    /// Build or reuse an immutable prepared-corpus artifact.
    Build(Box<CorpusBuildArgs>),
    /// Inspect an immutable prepared-corpus artifact.
    Inspect(CorpusInspectArgs),
    /// Inventory sources discovered under a corpus root.
    #[command(name = "inventory")]
    Sources(CorpusSourcesArgs),
    /// List prepared artifacts stored beside one source.
    Variants(CorpusVariantsArgs),
}

#[derive(Debug, Args)]
pub struct CorpusBuildArgs {
    /// Tokenizer dictionary used to prepare the artifact.
    #[arg(short = 'd', long, value_name = "PATH", help_heading = "Input")]
    pub dict_path: Option<PathBuf>,

    /// Publish to this new directory; never replace an existing path. If
    /// omitted, publish to the canonical content-addressed path beside ROOT.
    /// Deriving that name requires one additional read of the source.
    #[arg(long, value_name = "ARTIFACT", help_heading = "Output")]
    pub output: Option<PathBuf>,

    /// Input filesystem file or directory.
    #[arg(value_name = "ROOT", help_heading = "Input")]
    pub root: PathBuf,

    /// Override the inferred corpus format. Directories default to `markdown`.
    /// The selected format becomes part of the artifact identity.
    #[arg(long = "input-format", value_enum, help_heading = "Input")]
    pub input_format: Option<CorpusFormat>,

    #[command(flatten)]
    pub resources: ResourceArgs,

    /// Section selection for Markdown input. These determine which lines the
    /// artifact records, defining the artifact identity.
    #[command(flatten)]
    pub markdown: MarkdownReaderArgs,

    /// Validate inputs and print the execution plan without traversing ROOT.
    #[arg(long, help_heading = "Diagnostics")]
    pub plan: bool,
}

#[derive(Debug, Args)]
pub struct CorpusInspectArgs {
    /// Discover document and sentence metadata with exact counts and bounded examples.
    #[arg(long)]
    pub metadata: bool,

    /// Hash and cross-check all content stored in the artifact.
    #[arg(long)]
    pub verify: bool,

    /// Include the stored document inventory in `lingrep.sources/v1` form.
    #[arg(long)]
    pub documents: bool,

    /// Prepared artifact directory to inspect.
    #[arg(value_name = "ARTIFACT")]
    pub artifact: PathBuf,

    /// Validate inputs and print the execution plan without opening corpus data.
    #[arg(long)]
    pub plan: bool,
}

/// Discovery of prepared artifacts. Reads manifests and reports; never builds,
/// selects, or deletes artifacts.
#[derive(Debug, Args)]
pub struct CorpusVariantsArgs {
    /// Source root or prepared artifact used as the comparison point.
    ///
    /// With an artifact, differences are relative to that artifact. With a
    /// source, artifacts are grouped by the source content they record.
    #[arg(value_name = "SOURCE-OR-ARTIFACT")]
    pub target: PathBuf,

    /// Report at most N prepared artifacts; `all` removes the limit.
    ///
    /// The completion record reports the total count even when output is limited.
    #[arg(long, value_name = "N|all")]
    pub limit: Option<ExplicitLimit<u64>>,

    /// Delete the artifacts of this source that are marked prunable.
    ///
    /// Prunable artifacts were written in an earlier format than this build
    /// reads, so only a rebuild from source can replace them. Every artifact
    /// this build can open is kept, as are newer formats, damaged artifacts,
    /// other sources and symbolic links. Run without `--prune` first to see
    /// what would be deleted.
    #[arg(long)]
    pub prune: bool,

    /// Validate inputs and print the execution plan without scanning artifacts.
    #[arg(long)]
    pub plan: bool,
}

#[derive(Debug, Args)]
pub struct CorpusSourcesArgs {
    #[command(flatten)]
    pub traversal: TraversalArgs,

    #[command(flatten)]
    pub markdown: MarkdownReaderArgs,

    /// Control whether progress is written to stderr.
    #[arg(long, value_enum, default_value_t)]
    pub progress: ProgressMode,

    /// Write headerless `lingrep.sources-tsv/v1` records with source ordinal,
    /// logical name, target group, external-target flag, and multiplicity.
    #[arg(long, conflicts_with = "json", help_heading = "Output")]
    pub tsv: bool,

    /// Filesystem file or directory to inventory.
    #[arg(value_name = "ROOT", help_heading = "Input")]
    pub root: PathBuf,

    /// Override the inferred corpus format. Directories default to `markdown`.
    #[arg(long = "input-format", value_enum, help_heading = "Input")]
    pub input_format: Option<CorpusFormat>,

    /// Validate inputs and print the execution plan without traversing ROOT.
    #[arg(long, help_heading = "Diagnostics")]
    pub plan: bool,
}

#[derive(Debug, Args)]
pub struct TokenizeArgs {
    #[command(flatten)]
    pub input: QueryInputArgs,
    #[command(flatten)]
    pub cache: CacheArgs,
    #[command(flatten)]
    pub resources: ResourceArgs,

    /// Write headerless `lingrep.tokenize-tsv/v1` columns:
    /// `corpus_sentence_ordinal`, `source_name`, `document_ordinal`,
    /// `document_sentence_ordinal`, `token_index`, `orth`, `lemma`, `pos`, `cform`.
    #[arg(long, conflicts_with = "json", help_heading = "Output")]
    pub tsv: bool,

    /// Publish at most N sentences; `all` removes the limit.
    #[arg(long, value_name = "N|all", help_heading = "Output")]
    pub limit: Option<ExplicitLimit<u64>>,

    /// Validate inputs and print the execution plan without reading corpus records.
    #[arg(long, help_heading = "Diagnostics")]
    pub plan: bool,
}

#[derive(Debug, Args)]
pub struct ExploreArgs {
    #[command(flatten)]
    pub query: InteractiveQueryArgs,
}

#[derive(Debug, Args)]
pub struct SentenceFinalArgs {
    #[command(flatten)]
    pub query: InteractiveQueryArgs,

    /// Write headerless `lingrep.sentence-final-tsv/v1` records with pattern,
    /// surface, and exact count columns.
    #[arg(long, conflicts_with = "json", help_heading = "Output")]
    pub tsv: bool,

    /// Run the query and report policy decisions for at most N published records.
    /// `--plan` only validates and prints the invocation.
    #[arg(long, value_name = "N", help_heading = "Diagnostics")]
    pub trace_policy: Option<usize>,

    /// Validate inputs and print the execution plan without reading corpus records.
    #[arg(long, help_heading = "Diagnostics")]
    pub plan: bool,
}

#[derive(Debug, Args)]
pub struct InteractiveQueryArgs {
    #[command(flatten)]
    pub catalog: CatalogArgs,
    #[command(flatten)]
    pub input: QueryInputArgs,
    #[command(flatten)]
    pub resources: InteractiveResourceArgs,
}

/// Complete-file or normalized one-definition catalog selection.
#[derive(Debug, Args)]
#[group(required = true, multiple = false)]
pub struct CatalogArgs {
    /// Compile a complete lingrep patterns document from this file. For syntax, run
    /// `ling check --help`.
    #[arg(short = 'f', long, value_name = "FILE", help_heading = "Catalog")]
    pub patterns: Option<PathBuf>,

    /// Compile one inline pattern expression or catalog. For syntax, run
    /// `ling check --help`.
    #[arg(long, value_name = "PATTERN", help_heading = "Catalog")]
    pub pattern: Option<String>,
}

impl CatalogArgs {
    pub(crate) fn catalog_input(
        &self,
    ) -> Result<crate::catalog_input::CatalogInput, crate::error::CliError> {
        match (&self.patterns, &self.pattern) {
            (Some(path), None) => Ok(crate::catalog_input::CatalogInput::File(path.clone())),
            (None, Some(pattern)) => {
                Ok(crate::catalog_input::CatalogInput::Inline(pattern.clone()))
            }
            _ => Err(crate::error::CliError::InvalidArgs(
                "exactly one of --patterns or --pattern is required".into(),
            )),
        }
    }
}

/// [`CatalogArgs`] without `required = true`.
///
/// Other commands require a catalog, so `CatalogArgs` marks the argument
/// required. Here a bare coverage run is the common case — checking attribute
/// coverage before writing patterns — while the two options remain mutually
/// exclusive.
#[derive(Debug, Args)]
#[group(required = false, multiple = false)]
pub struct OptionalCatalogArgs {
    /// Compile a complete lingrep patterns document from this file. For syntax, run
    /// `ling check --help`.
    #[arg(short = 'f', long, value_name = "FILE", help_heading = "Catalog")]
    pub patterns: Option<PathBuf>,

    /// Compile one inline pattern expression or catalog. For syntax, run
    /// `ling check --help`.
    #[arg(long, value_name = "PATTERN", help_heading = "Catalog")]
    pub pattern: Option<String>,
}

impl OptionalCatalogArgs {
    /// `None` when no catalog was given, which is not an error here.
    pub(crate) fn catalog_input(&self) -> Option<crate::catalog_input::CatalogInput> {
        match (&self.patterns, &self.pattern) {
            (Some(path), None) => Some(crate::catalog_input::CatalogInput::File(path.clone())),
            (None, Some(pattern)) => {
                Some(crate::catalog_input::CatalogInput::Inline(pattern.clone()))
            }
            _ => None,
        }
    }
}

/// `coverage` reads tokens and counts, so it takes the same input and
/// resource groups as `tokenize`; the catalog is optional because coverage is
/// worth having on its own.
#[derive(Debug, Args)]
pub struct AttributesArgs {
    #[command(flatten)]
    pub catalog: OptionalCatalogArgs,
    #[command(flatten)]
    pub input: QueryInputArgs,
    #[command(flatten)]
    pub selection: ScopeArgs,
    #[command(flatten)]
    pub cache: CacheArgs,
    #[command(flatten)]
    pub resources: ResourceArgs,

    /// Write only the headerless `lingrep.attributes-tsv/v1` coverage table:
    /// attribute, present, unspecified, absent, and admitted-token counts.
    ///
    /// This cannot be combined with `--patterns` or `--pattern`, because
    /// catalog analysis adds a second table with different columns. Use
    /// `--json` to receive both record types.
    ///
    // The catalog conflict is checked after parsing so its diagnostic can
    // name `--json` as the recovery.
    #[arg(long, conflicts_with = "json", help_heading = "Output")]
    pub tsv: bool,

    /// Validate inputs and print the execution plan without reading corpus records.
    #[arg(long, help_heading = "Diagnostics")]
    pub plan: bool,
}

/// Direct root/stdin or immutable prepared corpus.
#[derive(Debug, Args)]
pub struct QueryInputArgs {
    /// Vibrato dictionary for direct input; overrides `LINGREP_DICTIONARY_PATH`.
    /// Not accepted with `--corpus`: a prepared artifact already contains analyzed tokens.
    #[arg(
        short = 'd',
        long,
        value_name = "PATH",
        conflicts_with = "corpus",
        help_heading = "Input"
    )]
    pub dict_path: Option<PathBuf>,

    /// Read analyzed tokens and source information from this prepared artifact.
    #[arg(
        long,
        value_name = "ARTIFACT",
        conflicts_with = "root",
        help_heading = "Input"
    )]
    pub corpus: Option<PathBuf>,

    /// Section filters for Markdown input. These filters determine which lines
    /// enter the corpus; `--where '@section …'` filters admitted documents.
    #[command(flatten)]
    pub markdown: MarkdownReaderArgs,

    /// Direct input file or directory. Use `-` to read stdin.
    #[arg(
        value_name = "ROOT",
        conflicts_with = "corpus",
        required_unless_present = "corpus",
        help_heading = "Input"
    )]
    pub root: Option<PathBuf>,

    /// Override the inferred corpus format. Directories default to `markdown`.
    /// Not accepted with `--corpus`: a prepared artifact records its own format.
    #[arg(
        long = "input-format",
        value_enum,
        conflicts_with = "corpus",
        help_heading = "Input"
    )]
    pub input_format: Option<CorpusFormat>,
}

/// Document selection for commands that apply scope predicates.
#[derive(Debug, Default, Args)]
pub struct ScopeArgs {
    /// Filter by @document.KEY, @sentence.KEY, @section or within(KIND, KEY=value).
    ///
    /// Use `&` for conjunction and parentheses around disjunctions:
    /// `@document.subject_area = 社会学 & (@document.year >= 2010 | @document.keywords contains 農学)`.
    #[arg(long = "where", value_name = "CLAUSE", help_heading = "Selection")]
    pub clause: Option<String>,
}

/// Sentence subsampling for commands that report over a corpus.
///
/// Separate from [`ScopeArgs`] although both select sentences: a scope clause
/// restricts counting to a named subset, so its counts are a census of that
/// subset, while a sample's counts estimate the whole corpus. Sharing a group
/// would put those two on the same footing.
#[derive(Clone, Debug, Default, Args)]
pub struct SampleArgs {
    /// Look at this fraction of the corpus, between 0 and 1.
    ///
    /// By default, sentences are selected by a hash of `--sample-seed` and the corpus
    /// sentence ordinal, so the sample is a property of the corpus rather than
    /// of the run: batching, worker count and where a `--limit` stopped cannot
    /// move it, and two runs at one seed look at the same sentences.
    ///
    /// Match and summary counts describe the sample. The `run`
    /// record states the sampling rate and seed. TSV output has no field for
    /// them, so `--sample` is not accepted with `--tsv`.
    #[arg(long = "sample", value_name = "RATE", help_heading = "Selection")]
    pub rate: Option<f64>,

    /// Sample whole prepared token blocks; adjacent sentences are correlated.
    #[arg(long = "sample-blocks", requires_all = ["rate", "corpus"], help_heading = "Selection")]
    pub blocks: bool,

    /// Seed for `--sample`; repeat with the same sampling unit and corpus.
    #[arg(
        long = "sample-seed",
        value_name = "N",
        default_value_t = 0,
        requires = "rate",
        help_heading = "Selection"
    )]
    pub seed: u64,
}

impl SampleArgs {
    /// The sample these flags describe, or `None` when unsampled.
    ///
    /// The rate arrives as a decimal because that is how a reader writes one,
    /// and is converted here to parts per million — once, at the boundary — so
    /// that nothing downstream does floating-point arithmetic to decide which
    /// sentences to look at. A rate that rounds to zero is rejected rather than
    /// silently admitting nothing: the run would read a whole corpus, publish
    /// no matches, and exit `0`.
    ///
    /// # Errors
    ///
    /// Returns a message when the rate is not a number in `(0, 1]`, or when it
    /// is positive but too small to select anything at this resolution.
    pub fn resolve(&self) -> Result<Option<lingrep_core::execution::CorpusSample>, String> {
        let Some(rate) = self.rate else {
            return Ok(None);
        };
        if !rate.is_finite() || rate <= 0.0 || rate > 1.0 {
            return Err(format!(
                "--sample takes a fraction greater than 0 and at most 1, not `{rate}`"
            ));
        }
        let scale = f64::from(lingrep_core::execution::RATE_SCALE);
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "bounded to (0, RATE_SCALE] by the range check above"
        )]
        let per_million = (rate * scale).round() as u32;
        let sample = lingrep_core::execution::CorpusSample::new(per_million, self.seed).ok_or_else(|| {
            format!(
                "--sample {rate} rounds to no sample units at all; the smallest                  selectable rate is {:.7}",
                1.0 / scale
            )
        })?;
        Ok(Some(sample))
    }
}

/// Optional prepared-artifact behavior for non-interactive corpus commands.
#[derive(Clone, Debug, Default, Args)]
pub struct CacheArgs {
    /// Reuse a compatible artifact beside ROOT, or create one when absent.
    ///
    /// Reusing an artifact scans ROOT to verify its exact source identity.
    /// Specify a known artifact with `--corpus` to skip source verification.
    ///
    /// By default, ROOT is analyzed directly without reading or writing an
    /// artifact. Stdin cannot be cached.
    #[arg(long, conflicts_with = "corpus", help_heading = "Input")]
    pub cache: bool,
}

/// The two formats whose root shape is part of the format: `bccwj` is a
/// distribution root (a directory containing `C-XML/` and `DOC/`), and
/// `soranoha` is one bulk archive file. Every command that declares
/// `--input-format` calls this before opening any input, so a directory or
/// stdin given for these formats is reported as a format mismatch. Returns the
/// message; each caller wraps it in its own error type.
pub(crate) fn validate_format_root(
    format: CorpusFormat,
    root: Option<&Path>,
    stdin: bool,
) -> Result<(), String> {
    match format {
        CorpusFormat::Bccwj if stdin || root.is_none_or(|root| !root.is_dir()) => Err(
            "bccwj input must be a distribution root (a directory containing C-XML/ and DOC/)"
                .to_owned(),
        ),
        CorpusFormat::Soranoha if stdin || root.is_none_or(|root| !root.is_file()) => Err(
            "soranoha input must be a bulk archive file (the .zip downloaded from soranoha.org)"
                .to_owned(),
        ),
        _ => Ok(()),
    }
}

impl QueryInputArgs {
    /// Resolve the dictionary for a direct-input execution.
    ///
    /// Environment fallback is deliberately resolved here rather than by
    /// clap: a prepared artifact must ignore `LINGREP_DICTIONARY_PATH` entirely,
    /// including when the inherited value is invalid.
    pub fn direct_dictionary_path(&self) -> Result<PathBuf, CliError> {
        self.dict_path
            .clone()
            .or_else(crate::packaged_dictionaries::fallback_dictionary_path)
            .ok_or_else(|| {
                CliError::InvalidArgs(format!(
                    "direct input requires a dictionary: {}",
                    crate::packaged_dictionaries::MISSING_DICTIONARY_HINT
                ))
            })
    }
}

/// Recursive source-admission bounds.
#[derive(Debug, Clone, Args)]
pub struct TraversalArgs {
    /// Maximum number of nested directories below ROOT.
    #[arg(
        long,
        default_value_t = 64,
        value_parser = positive_usize,
        help_heading = "Safety limits",
        hide_short_help = true
    )]
    pub max_traversal_depth: usize,
    /// Maximum number of filesystem entries examined under ROOT.
    #[arg(
        long,
        default_value_t = 100_000_000,
        value_parser = positive_u64,
        help_heading = "Safety limits",
        hide_short_help = true
    )]
    pub max_source_entries: u64,
    /// Maximum number of entries accepted from any one directory.
    #[arg(
        long,
        default_value_t = 1_000_000,
        value_parser = positive_usize,
        help_heading = "Safety limits",
        hide_short_help = true
    )]
    pub max_directory_entries: usize,
    /// Maximum number of documents discovered under ROOT.
    #[arg(
        long,
        default_value_t = 10_000_000,
        value_parser = positive_u64,
        help_heading = "Safety limits",
        hide_short_help = true
    )]
    pub max_documents: u64,
    /// Maximum bytes used to retain the discovered source index.
    #[arg(
        long,
        default_value_t = 536_870_912,
        value_parser = positive_usize,
        help_heading = "Safety limits",
        hide_short_help = true
    )]
    pub max_source_index_bytes: usize,
}

impl Default for TraversalArgs {
    fn default() -> Self {
        Self {
            max_traversal_depth: 64,
            max_source_entries: 100_000_000,
            max_directory_entries: 1_000_000,
            max_documents: 10_000_000,
            max_source_index_bytes: 536_870_912,
        }
    }
}

impl From<&TraversalArgs> for TraversalLimits {
    fn from(args: &TraversalArgs) -> Self {
        Self {
            max_depth: args.max_traversal_depth,
            max_source_entries: args.max_source_entries,
            max_directory_entries: args.max_directory_entries,
            max_documents: args.max_documents,
            max_index_bytes: args.max_source_index_bytes,
        }
    }
}

/// Analysis choices for commands that can build tokens from direct input.
#[derive(Clone, Debug, Args)]
pub struct ResourceArgs {
    /// Normalize input text before analysis; `nfc` is the default.
    #[arg(long, value_enum, default_value_t, help_heading = "Analysis")]
    pub normalization: NormalizationArg,

    /// Morphological analyzer used for direct input; `vibrato` is the default.
    #[arg(long, value_enum, default_value_t, help_heading = "Analysis")]
    pub analyzer: AnalyzerArg,

    /// Extract token text only (`text`) or full morphology (the default).
    #[arg(long, value_enum, default_value_t, help_heading = "Analysis")]
    pub profile: ProfileArg,

    /// Treat each input record as one sentence (the default), or let the analyzer segment it.
    #[arg(long, value_enum, default_value_t, help_heading = "Analysis")]
    pub segmentation: SegmentationArg,

    /// Sudachi dictionary file; valid only with `--analyzer sudachi`.
    #[arg(
        long,
        value_name = "PATH",
        help_heading = "Analyzer-specific",
        hide_short_help = true
    )]
    pub sudachi_dict: Option<PathBuf>,

    /// Sudachi resource directory holding `char.def`, `unk.def`, `rewrite.def`
    /// and `sudachi.json`; without it, the copies built into `ling` are used.
    #[arg(
        long,
        value_name = "DIRECTORY",
        help_heading = "Analyzer-specific",
        hide_short_help = true
    )]
    pub sudachi_resources: Option<PathBuf>,

    /// Sudachi split mode; `c` when omitted.
    #[arg(
        long,
        value_enum,
        help_heading = "Analyzer-specific",
        hide_short_help = true
    )]
    pub sudachi_split: Option<SudachiSplitArg>,

    /// Sudachi dictionary edition, recorded in the analysis identity; read
    /// from a `system_small.dic`, `system_core.dic` or `system_full.dic` file
    /// name when omitted.
    #[arg(
        long,
        value_enum,
        help_heading = "Analyzer-specific",
        hide_short_help = true
    )]
    pub sudachi_edition: Option<SudachiEditionArg>,

    /// External adapter executable; valid only with `--analyzer external`.
    #[arg(
        long,
        value_name = "PATH",
        help_heading = "Analyzer-specific",
        hide_short_help = true
    )]
    pub external_executable: Option<PathBuf>,

    /// Pass one argument to the external adapter; repeat for multiple arguments.
    #[arg(
        long = "external-arg",
        value_name = "VALUE",
        action = clap::ArgAction::Append,
        help_heading = "Analyzer-specific",
        hide_short_help = true
    )]
    pub external_args: Vec<String>,

    #[command(flatten)]
    pub runtime: RuntimeArgs,
}

/// Resource bounds and execution controls shared by direct and prepared input.
#[derive(Clone, Debug, Args)]
pub struct RuntimeArgs {
    #[command(flatten)]
    pub traversal: TraversalArgs,

    /// Maximum UTF-8 bytes accepted for one raw sentence.
    #[arg(
        long,
        default_value_t = DEFAULT_MAX_SENTENCE_BYTES,
        value_parser = positive_usize,
        help_heading = "Safety limits",
        hide_short_help = true
    )]
    pub max_sentence_bytes: usize,

    /// Maximum bytes accepted for one physical input record: a JSONL document,
    /// or a plaintext or Markdown line. Independent of `--batch-raw-bytes`,
    /// which limits a batch of admitted sentences rather than a document.
    #[arg(
        long,
        default_value_t = DEFAULT_MAX_INPUT_RECORD_BYTES,
        value_parser = positive_usize,
        help_heading = "Safety limits",
        hide_short_help = true
    )]
    pub max_record_bytes: usize,

    /// Maximum sentence count retained in one physical batch.
    #[arg(
        long,
        default_value_t = DEFAULT_MAX_STAGE_SENTENCES,
        value_parser = positive_usize,
        help_heading = "Safety limits",
        hide_short_help = true
    )]
    pub batch_sentences: usize,

    /// Maximum raw UTF-8 bytes retained in one physical batch.
    #[arg(
        long,
        default_value_t = DEFAULT_MAX_STAGE_INPUT_BYTES,
        value_parser = positive_usize,
        help_heading = "Safety limits",
        hide_short_help = true
    )]
    pub batch_raw_bytes: usize,

    /// Maximum estimated memory bytes retained for tokenized execution.
    #[arg(
        long,
        default_value_t = DEFAULT_MAX_TOKENIZED_BYTES,
        value_parser = positive_usize,
        help_heading = "Safety limits",
        hide_short_help = true
    )]
    pub max_tokenized_bytes: usize,

    /// Maximum bytes of temporary storage created by this process.
    #[arg(
        long,
        default_value_t = DEFAULT_MAX_TEMP_BYTES,
        value_parser = positive_u64,
        help_heading = "Safety limits",
        hide_short_help = true
    )]
    pub max_temp_bytes: u64,

    /// Number of worker threads used for analysis and matching.
    #[arg(
        long,
        default_value_t = default_workers(),
        value_parser = positive_usize,
        help_heading = "Execution"
    )]
    pub workers: usize,

    /// Control whether progress is written to stderr.
    #[arg(long, value_enum, default_value_t, help_heading = "Execution")]
    pub progress: ProgressMode,
}

/// Resource controls used by interactive Vibrato workflows.
#[derive(Clone, Debug, Args)]
pub struct InteractiveResourceArgs {
    /// Normalize direct input text before analysis; `nfc` is the default.
    #[arg(long, value_enum, default_value_t, help_heading = "Analysis")]
    pub normalization: NormalizationArg,

    #[command(flatten)]
    pub runtime: RuntimeArgs,
}

impl InteractiveResourceArgs {
    pub(crate) fn as_resource_args(&self) -> ResourceArgs {
        ResourceArgs {
            normalization: self.normalization,
            runtime: self.runtime.clone(),
            ..ResourceArgs::default()
        }
    }
}

impl std::ops::Deref for ResourceArgs {
    type Target = RuntimeArgs;

    fn deref(&self) -> &Self::Target {
        &self.runtime
    }
}

impl std::ops::Deref for InteractiveResourceArgs {
    type Target = RuntimeArgs;

    fn deref(&self) -> &Self::Target {
        &self.runtime
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
pub enum DiagnosticFormat {
    #[default]
    Human,
    Json,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
pub enum NormalizationArg {
    #[default]
    Nfc,
    None,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
pub enum AnalyzerArg {
    #[default]
    Vibrato,
    Sudachi,
    External,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
pub enum ProfileArg {
    Text,
    #[default]
    Morphology,
}

impl From<ProfileArg> for lingrep_core::analysis::ExtractionProfile {
    fn from(value: ProfileArg) -> Self {
        match value {
            ProfileArg::Text => Self::Tokenize,
            ProfileArg::Morphology => Self::Morphology,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
pub enum SegmentationArg {
    #[default]
    Input,
    Analyzer,
}

impl From<SegmentationArg> for lingrep_core::analysis::SegmentationMode {
    fn from(value: SegmentationArg) -> Self {
        match value {
            SegmentationArg::Input => Self::Input,
            SegmentationArg::Analyzer => Self::Analyzer,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum SudachiSplitArg {
    A,
    B,
    C,
}

impl From<SudachiSplitArg> for lingrep_core::analysis::SudachiSplit {
    fn from(value: SudachiSplitArg) -> Self {
        match value {
            SudachiSplitArg::A => Self::A,
            SudachiSplitArg::B => Self::B,
            SudachiSplitArg::C => Self::C,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum SudachiEditionArg {
    Small,
    Core,
    Full,
}

impl SudachiEditionArg {
    /// Reads the edition from SudachiDict's file names, `system_<edition>.dic`.
    fn from_dictionary_file_name(dictionary: &std::path::Path) -> Option<Self> {
        match dictionary.file_name()?.to_str()? {
            "system_small.dic" => Some(Self::Small),
            "system_core.dic" => Some(Self::Core),
            "system_full.dic" => Some(Self::Full),
            _ => None,
        }
    }
}

impl From<SudachiEditionArg> for lingrep_core::analysis::SudachiEdition {
    fn from(value: SudachiEditionArg) -> Self {
        match value {
            SudachiEditionArg::Small => Self::Small,
            SudachiEditionArg::Core => Self::Core,
            SudachiEditionArg::Full => Self::Full,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AnalyzerCliConfig {
    Vibrato,
    Sudachi {
        dictionary: PathBuf,
        resources: Option<PathBuf>,
        split: lingrep_core::analysis::SudachiSplit,
        edition: lingrep_core::analysis::SudachiEdition,
    },
    External {
        executable: PathBuf,
        arguments: Box<[String]>,
    },
}

impl From<NormalizationArg> for lingrep_core::analysis::NormalizationPolicy {
    fn from(value: NormalizationArg) -> Self {
        match value {
            NormalizationArg::Nfc => Self::Nfc,
            NormalizationArg::None => Self::None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
pub enum CheckFormat {
    #[default]
    Json,
    Human,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
pub enum MachineFormat {
    #[default]
    Jsonl,
    Tsv,
}

/// The v1 tokenization protocol has one canonical machine projection.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
pub enum TokenizeFormat {
    #[default]
    Jsonl,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
pub enum MatchFormat {
    #[default]
    Jsonl,
    Tsv,
    Human,
}

/// Validated process-independent configuration passed to command owners.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecutionCliConfig {
    pub traversal: TraversalLimits,
    pub normalization: lingrep_core::analysis::NormalizationPolicy,
    pub analyzer: AnalyzerCliConfig,
    pub profile: lingrep_core::analysis::ExtractionProfile,
    pub segmentation: lingrep_core::analysis::SegmentationMode,
    /// Every resource bound this run executes under, in one owned value. The
    /// bounds are not re-derived per command: a command reads `limits` and
    /// passes it through.
    pub limits: ExecutionLimits,
    pub max_temp_bytes: u64,
    pub workers: usize,
    pub progress: ProgressMode,
    pub diagnostic_format: DiagnosticFormat,
}

fn prepared_input_conflict(matches: &clap::ArgMatches) -> Option<&'static str> {
    let (command, args) = matches.subcommand()?;
    if !matches!(
        command,
        "matches" | "summarize" | "tokenize" | "coverage" | "explore" | "sentence-final"
    ) || args.get_one::<PathBuf>("corpus").is_none()
    {
        return None;
    }

    let mut candidates = vec![
        "md_include_sections",
        "md_exclude_sections",
        "md_self_labels",
        "normalization",
    ];
    if matches!(command, "matches" | "summarize" | "tokenize" | "coverage") {
        candidates.extend([
            "analyzer",
            "profile",
            "segmentation",
            "sudachi_dict",
            "sudachi_resources",
            "sudachi_split",
            "sudachi_edition",
            "external_executable",
            "external_args",
        ]);
    }
    candidates
        .into_iter()
        .find(|id| args.value_source(id) == Some(clap::parser::ValueSource::CommandLine))
}

impl Cli {
    /// Parse and apply cross-command constraints involving global selectors.
    pub fn try_parse_from<I, T>(args: I) -> Result<Self, clap::Error>
    where
        I: IntoIterator<Item = T>,
        T: Into<std::ffi::OsString> + Clone,
    {
        let matches = Self::command().try_get_matches_from(args)?;
        let human_match_option_used =
            matches
                .subcommand_matches("matches")
                .is_some_and(|matches| {
                    matches.get_flag("align_hits")
                        || matches.value_source("morpheme_separator")
                            == Some(clap::parser::ValueSource::CommandLine)
                        || matches.value_source("context_color_depth")
                            == Some(clap::parser::ValueSource::CommandLine)
                        || matches.value_source("context_color_theme")
                            == Some(clap::parser::ValueSource::CommandLine)
                });
        if let Some(option) = prepared_input_conflict(&matches) {
            return Err(Self::command().error(
                clap::error::ErrorKind::ArgumentConflict,
                format!(
                    "--{} applies only to direct input; a prepared artifact already records its reader and analysis settings",
                    option.replace('_', "-")
                ),
            ));
        }
        let cli = Self::from_arg_matches(&matches)?;
        if cli.json && cli.has_tsv() {
            return Err(Self::command().error(
                clap::error::ErrorKind::ArgumentConflict,
                "--json cannot be used with --tsv",
            ));
        }
        if cli.json && cli.color == ColorMode::Always {
            return Err(Self::command().error(
                clap::error::ErrorKind::ArgumentConflict,
                "--color=always cannot be used with machine output",
            ));
        }
        if cli.json && human_match_option_used {
            return Err(Self::command().error(
                clap::error::ErrorKind::ArgumentConflict,
                "--align-hits, --morpheme-separator, --context-color-depth, and --context-color-theme cannot be used with --json",
            ));
        }
        Ok(cli)
    }

    fn has_tsv(&self) -> bool {
        match &self.command {
            Command::Matches(args) => args.tsv,
            Command::Summarize(args) => args.tsv,
            Command::Corpus(CorpusArgs {
                command: CorpusCommand::Sources(args),
            }) => args.tsv,
            Command::Tokenize(args) => args.tsv,
            Command::Attributes(args) => args.tsv,
            Command::SentenceFinal(args) => args.tsv,
            Command::Doctor(_)
            | Command::Dictionary(_)
            | Command::Check(_)
            | Command::Corpus(_)
            | Command::Explore(_)
            | Command::Describe(_)
            | Command::Completions(_)
            | Command::Manpages(_)
            | Command::Schema(_) => false,
        }
    }

    #[must_use]
    pub fn output_mode(&self) -> OutputMode {
        if self.json {
            return OutputMode::Jsonl;
        }
        match &self.command {
            Command::Matches(args) if args.tsv => OutputMode::Tsv,
            Command::Summarize(args) if args.tsv => OutputMode::Tsv,
            Command::Corpus(CorpusArgs {
                command: CorpusCommand::Sources(args),
            }) if args.tsv => OutputMode::Tsv,
            Command::Tokenize(args) if args.tsv => OutputMode::Tsv,
            Command::Attributes(args) if args.tsv => OutputMode::Tsv,
            Command::SentenceFinal(args) if args.tsv => OutputMode::Tsv,
            Command::Doctor(_)
            | Command::Dictionary(_)
            | Command::Check(_)
            | Command::Matches(_)
            | Command::Summarize(_)
            | Command::Corpus(_)
            | Command::Tokenize(_)
            | Command::Attributes(_)
            | Command::Explore(_)
            | Command::SentenceFinal(_) => OutputMode::Human,
            Command::Describe(_)
            | Command::Completions(_)
            | Command::Manpages(_)
            | Command::Schema(_) => OutputMode::Human,
        }
    }

    #[must_use]
    pub fn diagnostic_format(&self) -> DiagnosticFormat {
        if self.json {
            DiagnosticFormat::Json
        } else {
            DiagnosticFormat::Human
        }
    }

    /// Return `corpus inventory` arguments without exposing command-tree matching
    /// to every consumer.
    pub fn corpus_sources(&self) -> Option<&CorpusSourcesArgs> {
        match &self.command {
            Command::Corpus(CorpusArgs {
                command: CorpusCommand::Sources(args),
            }) => Some(args),
            _ => None,
        }
    }

    /// Validate cross-field resource relationships before
    /// any command is allowed to publish output.
    pub fn execution_config(&self) -> Result<ExecutionCliConfig, CliError> {
        let resources = match &self.command {
            Command::Check(args) => args.resources.clone(),
            Command::Matches(args) => args.resources.clone(),
            Command::Summarize(args) => args.resources.clone(),
            Command::Corpus(CorpusArgs {
                command: CorpusCommand::Build(args),
            }) => args.resources.clone(),
            Command::Tokenize(args) => args.resources.clone(),
            Command::Attributes(args) => args.resources.clone(),
            Command::Explore(args) => args.query.resources.as_resource_args(),
            Command::SentenceFinal(args) => args.query.resources.as_resource_args(),
            Command::Corpus(CorpusArgs {
                command: CorpusCommand::Sources(args),
            }) => ResourceArgs::with_traversal(args.traversal.clone(), args.progress),
            Command::Doctor(_)
            | Command::Dictionary(_)
            | Command::Corpus(CorpusArgs {
                command: CorpusCommand::Inspect(_) | CorpusCommand::Variants(_),
            })
            | Command::Describe(_)
            | Command::Completions(_)
            | Command::Manpages(_)
            | Command::Schema(_) => ResourceArgs::default(),
        };

        let limits = ExecutionLimits {
            max_sentence_bytes: resources.max_sentence_bytes,
            max_input_record_bytes: resources.max_record_bytes,
            max_stage_sentences: resources.batch_sentences,
            max_stage_input_bytes: resources.batch_raw_bytes,
            max_tokenized_bytes: resources.max_tokenized_bytes,
        };
        // `ExecutionLimits::validate` checks every rule that relates one bound to
        // another; this function does not repeat those checks.
        limits
            .validate()
            .map_err(|error| CliError::InvalidArgs(limit_diagnostic(&error)))?;

        let has_sudachi_options = resources.sudachi_dict.is_some()
            || resources.sudachi_resources.is_some()
            || resources.sudachi_split.is_some()
            || resources.sudachi_edition.is_some();
        let has_external_options =
            resources.external_executable.is_some() || !resources.external_args.is_empty();
        let analyzer = match resources.analyzer {
            AnalyzerArg::Vibrato => {
                if has_sudachi_options || has_external_options {
                    return Err(CliError::InvalidArgs(
                        "Sudachi/external options require their matching --analyzer value".into(),
                    ));
                }
                AnalyzerCliConfig::Vibrato
            }
            AnalyzerArg::Sudachi => {
                if has_external_options {
                    return Err(CliError::InvalidArgs(
                        "external adapter options are invalid with --analyzer sudachi".into(),
                    ));
                }
                let dictionary = resources.sudachi_dict.clone().ok_or_else(|| {
                    CliError::InvalidArgs(
                        "--sudachi-dict is required with --analyzer sudachi".into(),
                    )
                })?;
                let edition = resources
                    .sudachi_edition
                    .or_else(|| SudachiEditionArg::from_dictionary_file_name(&dictionary))
                    .ok_or_else(|| {
                        CliError::InvalidArgs(format!(
                            "the Sudachi edition cannot be read from the file name {}; \
                             pass --sudachi-edition small, core or full",
                            dictionary.display()
                        ))
                    })?;
                AnalyzerCliConfig::Sudachi {
                    dictionary,
                    resources: resources.sudachi_resources.clone(),
                    split: resources.sudachi_split.unwrap_or(SudachiSplitArg::C).into(),
                    edition: edition.into(),
                }
            }
            AnalyzerArg::External => {
                if has_sudachi_options {
                    return Err(CliError::InvalidArgs(
                        "Sudachi options are invalid with --analyzer external".into(),
                    ));
                }
                AnalyzerCliConfig::External {
                    executable: resources.external_executable.clone().ok_or_else(|| {
                        CliError::InvalidArgs(
                            "--external-executable is required with --analyzer external".into(),
                        )
                    })?,
                    arguments: resources.external_args.clone().into_boxed_slice(),
                }
            }
        };
        if matches!(&self.command, Command::SentenceFinal(_))
            && !matches!(&analyzer, AnalyzerCliConfig::Vibrato)
        {
            return Err(CliError::InvalidArgs(
                "sentence-final supports only --analyzer vibrato".into(),
            ));
        }
        let direct_dictionary = match &self.command {
            Command::Check(args) => args.dict_path.as_ref(),
            Command::Matches(args) => args.input.dict_path.as_ref(),
            Command::Summarize(args) => args.input.dict_path.as_ref(),
            Command::Tokenize(args) => args.input.dict_path.as_ref(),
            Command::Attributes(args) => args.input.dict_path.as_ref(),
            Command::Explore(args) => args.query.input.dict_path.as_ref(),
            Command::SentenceFinal(args) => args.query.input.dict_path.as_ref(),
            Command::Corpus(CorpusArgs {
                command: CorpusCommand::Build(args),
            }) => args.dict_path.as_ref(),
            _ => None,
        };
        if !matches!(&analyzer, AnalyzerCliConfig::Vibrato) && direct_dictionary.is_some() {
            return Err(CliError::InvalidArgs(
                "--dict-path is valid only with --analyzer vibrato".into(),
            ));
        }

        Ok(ExecutionCliConfig {
            traversal: TraversalLimits::from(&resources.traversal),
            normalization: resources.normalization.into(),
            analyzer,
            profile: resources.profile.into(),
            segmentation: resources.segmentation.into(),
            limits,
            max_temp_bytes: resources.max_temp_bytes,
            workers: resources.workers,
            progress: resources.progress,
            diagnostic_format: self.diagnostic_format(),
        })
    }
}

impl ResourceArgs {
    fn with_traversal(traversal: TraversalArgs, progress: ProgressMode) -> Self {
        Self {
            runtime: RuntimeArgs {
                traversal,
                progress,
                ..RuntimeArgs::default()
            },
            ..Self::default()
        }
    }
}

impl Default for RuntimeArgs {
    fn default() -> Self {
        Self {
            traversal: TraversalArgs::default(),
            max_sentence_bytes: DEFAULT_MAX_SENTENCE_BYTES,
            max_record_bytes: DEFAULT_MAX_INPUT_RECORD_BYTES,
            batch_sentences: DEFAULT_MAX_STAGE_SENTENCES,
            batch_raw_bytes: DEFAULT_MAX_STAGE_INPUT_BYTES,
            max_tokenized_bytes: DEFAULT_MAX_TOKENIZED_BYTES,
            max_temp_bytes: DEFAULT_MAX_TEMP_BYTES,
            workers: default_workers(),
            progress: ProgressMode::Auto,
        }
    }
}

impl Default for ResourceArgs {
    fn default() -> Self {
        Self {
            normalization: NormalizationArg::Nfc,
            analyzer: AnalyzerArg::Vibrato,
            profile: ProfileArg::Morphology,
            segmentation: SegmentationArg::Input,
            sudachi_dict: None,
            sudachi_resources: None,
            sudachi_split: None,
            sudachi_edition: None,
            external_executable: None,
            external_args: Vec::new(),
            runtime: RuntimeArgs::default(),
        }
    }
}

#[cfg(test)]
mod analyzer_selection_tests {
    use super::*;

    #[test]
    fn direct_analysis_defaults_to_vibrato_nfc_morphology_input() {
        let cli = Cli::try_parse_from(["ling", "matches", "--pattern", "[]", "input.txt"]).unwrap();
        let config = cli.execution_config().unwrap();
        assert_eq!(config.analyzer, AnalyzerCliConfig::Vibrato);
        assert_eq!(
            config.normalization,
            lingrep_core::analysis::NormalizationPolicy::Nfc
        );
        assert_eq!(
            config.profile,
            lingrep_core::analysis::ExtractionProfile::Morphology
        );
        assert_eq!(
            config.segmentation,
            lingrep_core::analysis::SegmentationMode::Input
        );
    }

    #[test]
    fn sudachi_requires_its_complete_native_selection_and_rejects_dict_path() {
        let incomplete = Cli::try_parse_from([
            "ling",
            "matches",
            "--pattern",
            "[]",
            "--analyzer",
            "sudachi",
            "input.txt",
        ])
        .unwrap();
        assert!(incomplete.execution_config().is_err());

        let conflicting = Cli::try_parse_from([
            "ling",
            "matches",
            "--pattern",
            "[]",
            "--analyzer",
            "sudachi",
            "--sudachi-dict",
            "system_core.dic",
            "--sudachi-resources",
            "resources",
            "--sudachi-split",
            "b",
            "--dict-path",
            "vibrato.dic",
            "input.txt",
        ])
        .unwrap();
        assert!(conflicting.execution_config().is_err());
    }

    #[test]
    fn sudachi_defaults_split_c_built_in_resources_and_the_file_name_edition() {
        use lingrep_core::analysis::{SudachiEdition, SudachiSplit};
        let select = |extra: &[&str]| {
            let mut args = vec![
                "ling",
                "matches",
                "--pattern",
                "[]",
                "--analyzer",
                "sudachi",
            ];
            args.extend_from_slice(extra);
            args.push("input.txt");
            Cli::try_parse_from(args)
                .unwrap()
                .execution_config()
                .map(|config| config.analyzer)
        };
        let sudachi = |dictionary: &str, edition, split, resources: Option<&str>| {
            AnalyzerCliConfig::Sudachi {
                dictionary: dictionary.into(),
                resources: resources.map(PathBuf::from),
                split,
                edition,
            }
        };
        for (args, expected) in [
            (
                vec!["--sudachi-dict", "d/system_small.dic"],
                sudachi(
                    "d/system_small.dic",
                    SudachiEdition::Small,
                    SudachiSplit::C,
                    None,
                ),
            ),
            (
                vec!["--sudachi-dict", "system_full.dic", "--sudachi-split", "a"],
                sudachi(
                    "system_full.dic",
                    SudachiEdition::Full,
                    SudachiSplit::A,
                    None,
                ),
            ),
            (
                vec![
                    "--sudachi-dict",
                    "custom.dic",
                    "--sudachi-edition",
                    "core",
                    "--sudachi-resources",
                    "r",
                ],
                sudachi(
                    "custom.dic",
                    SudachiEdition::Core,
                    SudachiSplit::C,
                    Some("r"),
                ),
            ),
            (
                vec![
                    "--sudachi-dict",
                    "system_full.dic",
                    "--sudachi-edition",
                    "small",
                ],
                sudachi(
                    "system_full.dic",
                    SudachiEdition::Small,
                    SudachiSplit::C,
                    None,
                ),
            ),
        ] {
            assert_eq!(select(&args).unwrap(), expected, "{args:?}");
        }

        let unnamed = select(&["--sudachi-dict", "custom.dic"]).unwrap_err();
        assert!(
            unnamed.to_string().contains("--sudachi-edition"),
            "{unnamed}"
        );

        let edition_without_sudachi = Cli::try_parse_from([
            "ling",
            "matches",
            "--pattern",
            "[]",
            "--sudachi-edition",
            "core",
            "input.txt",
        ])
        .unwrap();
        assert!(edition_without_sudachi.execution_config().is_err());
    }

    #[test]
    fn external_arguments_are_literal_and_sentence_final_refuses_external() {
        let cli = Cli::try_parse_from([
            "ling",
            "matches",
            "--pattern",
            "[]",
            "--analyzer",
            "external",
            "--external-executable",
            "adapter",
            "--external-arg=--model",
            "--external-arg=path with spaces;rm -rf nope",
            "input.txt",
        ])
        .unwrap();
        let config = cli.execution_config().unwrap();
        let AnalyzerCliConfig::External { arguments, .. } = config.analyzer else {
            panic!("external selection was not retained");
        };
        assert_eq!(
            arguments.as_ref(),
            ["--model", "path with spaces;rm -rf nope"]
        );

        let specialist = Cli::try_parse_from([
            "ling",
            "sentence-final",
            "--pattern",
            "[]",
            "--analyzer",
            "external",
            "--external-executable",
            "adapter",
            "input.txt",
        ]);
        assert!(specialist.is_err());
    }
}
