//! What the orchestrator needs from Rust, and nothing else.
//!
//! `scripts/perf_campaign.sh` owns the loop: build the arms, pin to a CCD,
//! interleave the runs, read wall/user/sys/peak RSS from `/usr/bin/time`. That
//! is what a shell is good at. Three things it is bad at live here instead:
//!
//! * **Digests.** BLAKE3 has no coreutils spelling, and hashing is what makes
//!   an arm content-addressed rather than a mutable path.
//! * **Output normalization.** The run-variant field list belongs to the crate
//!   that owns the schema. A `sed` expression restating it was wrong twice in
//!   one afternoon, which is why
//!   [`lingrep::protocol::query::normalize_run_variance`] exists at all.
//! * **The manifest seal.** A manifest is digested by the same code that
//!   verifies it, so declaring and adjudicating cannot drift apart.
//!
//! Everything here is behind the `screen` feature, which is what "this part
//! links the workspace" means for this crate. The adjudicator stays pure.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::campaign::{
    Arm, Arms, Bound, BuildProfile, Campaign, CampaignCatalog, LadderRung, Machine, Manifest,
    Metric, Observation, OutputCapture, Role, Target, TemplateArg, Tier, WorkloadInput,
    WorkloadKind, progress,
};

/// Why the orchestrator cannot proceed.
///
/// Every variant stops the campaign. There is no recoverable case: a run whose
/// identity cannot be established is not a run that may be appended anyway.
#[derive(Debug)]
pub enum OrchestrateError {
    Unreadable {
        path: PathBuf,
        cause: String,
    },
    NotJsonl {
        path: PathBuf,
        line: usize,
    },
    MachineUnobservable {
        field: &'static str,
    },
    MachineChanged {
        field: &'static str,
        declared: String,
        observed: String,
    },
    UnknownArm {
        arm: String,
    },
    BinarySwapped {
        arm: String,
        declared: String,
        observed: String,
    },
    UnknownCatalog {
        catalog: String,
        known: Vec<String>,
    },
    NotACampaign {
        path: PathBuf,
        cause: String,
    },
    ProfileUnreadable {
        profile: String,
    },
    WorkloadChanged {
        path: String,
        declared: String,
        observed: String,
    },
    NotAFile {
        path: PathBuf,
    },
    OutOfSequence {
        catalog: String,
        expected: String,
        found: String,
    },
    CommandTemplateMissing,
    MalformedOutputDigest {
        value: String,
    },
    OutputCaptureDiffers {
        sealed: OutputCapture,
        observed: OutputCapture,
    },
    WorkerCountDiffers {
        sealed: Option<u32>,
        observed: Option<u32>,
    },
    CommandDiffers {
        catalog: String,
        expected: Vec<String>,
        observed: Vec<String>,
    },
    CorpusNotMapped {
        catalog: String,
    },
    CorpusMappingWithoutHole,
    CorpusNotDeclared {
        catalog: String,
        corpus: String,
    },
    PreparedArtifactUnmapped {
        corpus: String,
    },
}

impl std::fmt::Display for OrchestrateError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unreadable { path, cause } => {
                write!(formatter, "{} is unreadable: {cause}", path.display())
            }
            Self::NotJsonl { path, line } => write!(
                formatter,
                "{} line {line} is not JSON, so its run-variant fields cannot be neutralized; \
                 a campaign measures `--json` output",
                path.display()
            ),
            Self::MachineUnobservable { field } => write!(
                formatter,
                "cannot observe {field}; a campaign that cannot identify its machine cannot \
                 pin one"
            ),
            Self::MachineChanged {
                field,
                declared,
                observed,
            } => write!(
                formatter,
                "this campaign declared {field} {declared} and is running on {observed}; \
                 blocks measured on two machines are not paired runs"
            ),
            Self::UnknownArm { arm } => {
                write!(
                    formatter,
                    "{arm} is not an arm of this campaign; use `a` or `b`"
                )
            }
            Self::BinarySwapped {
                arm,
                declared,
                observed,
            } => write!(
                formatter,
                "arm {arm} was declared as {declared} and is now {observed}; the arm was \
                 rebuilt or replaced mid-campaign"
            ),
            Self::UnknownCatalog { catalog, known } => write!(
                formatter,
                "{catalog} is not in this campaign's suite. It measures: {}",
                known.join(", ")
            ),
            Self::WorkloadChanged {
                path,
                declared,
                observed,
            } => write!(
                formatter,
                "this campaign declared {path} as {declared} and it is now {observed}; the \
                 workload changed under the campaign, and a path is not an identity"
            ),
            Self::NotAFile { path } => write!(
                formatter,
                "{} is not a regular file, so it has no digest; a campaign input must be one",
                path.display()
            ),
            Self::OutOfSequence {
                catalog,
                expected,
                found,
            } => write!(
                formatter,
                "{catalog} expected {expected} and was given {found}; runs are appended in \
                 order so that an interrupted block can be resumed rather than abandoned \
                 half-measured"
            ),
            Self::NotACampaign { path, cause } => {
                write!(formatter, "{} is not a campaign: {cause}", path.display())
            }
            Self::ProfileUnreadable { profile } => write!(
                formatter,
                "the workspace Cargo.toml declares no [profile.{profile}], so the build these \
                 arms were compiled with cannot be recorded truthfully"
            ),
            Self::WorkerCountDiffers { sealed, observed } => {
                let show = |value: &Option<u32>| {
                    value.map_or_else(|| "unset".to_owned(), |workers| workers.to_string())
                };
                write!(
                    formatter,
                    "this run used {} workers and the campaign seals {}. Occupancy is measured \
                     against the sealed count, and a pool that moved mid-campaign makes the two \
                     arms incomparable.",
                    show(observed),
                    show(sealed)
                )
            }
            Self::OutputCaptureDiffers { sealed, observed } => write!(
                formatter,
                "this run captured its output as {observed:?} and the campaign seals \
                 {sealed:?}. The digest is the same number either way, but the two paths cost \
                 the measured process differently, so a campaign that changes halfway compares \
                 blocks measured under two rigs."
            ),
            Self::MalformedOutputDigest { value } => write!(
                formatter,
                "{value:?} is not an output digest. Expected `blake3:` and 64 lowercase hex \
                 characters, which is what `lingrep-perf digest-stream` prints — an empty or \
                 truncated value means the hasher did not finish, and recording it would \
                 compare a run against nothing."
            ),
            Self::CommandTemplateMissing => write!(
                formatter,
                "a campaign must seal the command it measures with. Pass it after `--`, \
                 using {} where the catalog goes.",
                crate::campaign::TemplateArg::CATALOG_HOLE
            ),
            Self::CommandDiffers {
                catalog,
                expected,
                observed,
            } => write!(
                formatter,
                "{catalog} was run with a command the manifest does not seal.\n  \
                 sealed:   {}\n  observed: {}",
                expected.join(" "),
                observed.join(" ")
            ),
            Self::CorpusNotMapped { catalog } => write!(
                formatter,
                "the sealed command needs a prepared corpus for {catalog}, but the campaign \
                 maps none"
            ),
            Self::CorpusMappingWithoutHole => write!(
                formatter,
                "the campaign maps catalogs to prepared corpora, but its command contains no \
                 {} hole",
                crate::campaign::TemplateArg::CORPUS_HOLE
            ),
            Self::CorpusNotDeclared { catalog, corpus } => write!(
                formatter,
                "{catalog} maps to {corpus}, but that path is not a declared prepared-artifact \
                 workload whose identity can be re-observed before every run"
            ),
            Self::PreparedArtifactUnmapped { corpus } => write!(
                formatter,
                "{corpus} is declared as a prepared-artifact workload but no catalog maps to \
                 it; every sealed workload must be part of the command this campaign executes"
            ),
        }
    }
}

impl std::error::Error for OrchestrateError {}

fn read(path: &Path) -> Result<Vec<u8>, OrchestrateError> {
    std::fs::read(path).map_err(|cause| OrchestrateError::Unreadable {
        path: path.to_path_buf(),
        cause: cause.to_string(),
    })
}

/// BLAKE3 of a file's bytes, tagged so a digest is never mistaken for a hex
/// blob of some other provenance.
pub fn digest_file(path: &Path) -> Result<String, OrchestrateError> {
    if !path.is_file() {
        return Err(OrchestrateError::NotAFile {
            path: path.to_path_buf(),
        });
    }
    Ok(format!("blake3:{}", blake3::hash(&read(path)?).to_hex()))
}

/// BLAKE3 of a run's output with the run-variant fields neutralized.
///
/// Each JSONL record is parsed, normalized, and re-encoded before hashing.
/// Objects are recursively key-sorted before re-encoding, so two runs that emit
/// the same records in a different key order still agree — and two runs that
/// emit different records still differ. The explicit sort is required because
/// workspace feature unification may enable `serde_json/preserve_order`.
pub fn output_digest(path: &Path) -> Result<String, OrchestrateError> {
    let text = String::from_utf8_lossy(&read(path)?).into_owned();
    let mut hasher = OutputHasher::new();
    for (index, line) in text.lines().enumerate() {
        hasher
            .line(line)
            .map_err(|line_number| OrchestrateError::NotJsonl {
                path: path.to_path_buf(),
                line: line_number.max(index + 1),
            })?;
    }
    Ok(hasher.finish())
}

/// The normalization behind [`output_digest`], one line at a time.
///
/// Extracted so a digest can be taken from a stream as well as a file, and
/// so the two paths cannot drift: the digest of a run is the same number
/// whichever way its output reached the hasher, which is what lets a baseline
/// recorded one way be compared with a run measured the other.
pub struct OutputHasher {
    hasher: blake3::Hasher,
    lines: usize,
}

impl Default for OutputHasher {
    fn default() -> Self {
        Self::new()
    }
}

impl OutputHasher {
    #[must_use]
    pub fn new() -> Self {
        Self {
            hasher: blake3::Hasher::new(),
            lines: 0,
        }
    }

    /// Fold one line in. Blank lines are skipped, as they always were.
    ///
    /// # Errors
    ///
    /// The 1-based line number, when the line is not JSON.
    pub fn line(&mut self, line: &str) -> Result<(), usize> {
        self.lines += 1;
        if line.trim().is_empty() {
            return Ok(());
        }
        let mut event: serde_json::Value = serde_json::from_str(line).map_err(|_| self.lines)?;
        lingrep::protocol::query::normalize_run_variance(&mut event);
        canonicalize_json(&mut event);
        self.hasher.update(event.to_string().as_bytes());
        self.hasher.update(b"\n");
        Ok(())
    }

    #[must_use]
    pub fn finish(self) -> String {
        format!("blake3:{}", self.hasher.finalize().to_hex())
    }
}

fn canonicalize_json(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Array(values) => {
            for value in values {
                canonicalize_json(value);
            }
        }
        serde_json::Value::Object(fields) => {
            for value in fields.values_mut() {
                canonicalize_json(value);
            }
            fields.sort_keys();
        }
        _ => {}
    }
}

/// The digest of a run's output as it streams past, never materialized.
///
/// `matches --limit all --json` on `distributions.lx` over `science-corpus-v7`
/// writes ~42 GB that nothing reads except this function, against 43 GB of free
/// disk — which is why a campaign whose bound was profiled on that corpus was
/// measured on one 2.4x smaller instead.
///
/// **The whole input is consumed**, always, even after a parse failure. An
/// early return would deliver `EPIPE` to the process under measurement and
/// turn a correct run into a recorded failure, which is a worse outcome than
/// hashing a few more megabytes.
///
/// # Errors
///
/// When reading fails, or a line is not JSON.
pub fn stream_digest(input: &mut dyn std::io::BufRead) -> Result<String, OrchestrateError> {
    let mut hasher = OutputHasher::new();
    let mut failure: Option<OrchestrateError> = None;
    let mut line = String::new();
    loop {
        line.clear();
        match input.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {
                let trimmed = line.trim_end_matches(['\n', '\r']);
                if let Err(number) = hasher.line(trimmed)
                    && failure.is_none()
                {
                    failure = Some(OrchestrateError::NotJsonl {
                        path: PathBuf::from("<stdin>"),
                        line: number,
                    });
                }
            }
            Err(cause) if failure.is_none() => {
                failure = Some(OrchestrateError::Unreadable {
                    path: PathBuf::from("<stdin>"),
                    cause: cause.to_string(),
                });
                break;
            }
            Err(_) => break,
        }
    }
    match failure {
        Some(error) => Err(error),
        None => Ok(hasher.finish()),
    }
}

/// The machine this process is running on, as far as it can be observed.
///
/// `rustc` is provenance rather than a pin: it identifies the toolchain that
/// built the arms, and the arms are already content-addressed, so a toolchain
/// change after they were built cannot alter them. Only `cpu_model` and
/// `kernel` are compared per block.
pub fn observe_machine() -> Result<Machine, OrchestrateError> {
    let cpu_model = observe_cpu_model()?;
    let kernel = observe_kernel()?;
    let rustc = std::process::Command::new("rustc")
        .arg("--version")
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .ok_or(OrchestrateError::MachineUnobservable { field: "rustc" })?;
    Ok(Machine {
        cpu_model,
        kernel,
        rustc,
    })
}

/// `model name` from `/proc/cpuinfo`, unchanged, wherever procfs exists.
#[cfg(not(target_os = "macos"))]
fn observe_cpu_model() -> Result<String, OrchestrateError> {
    let cpuinfo = std::fs::read_to_string("/proc/cpuinfo")
        .map_err(|_| OrchestrateError::MachineUnobservable { field: "cpu_model" })?;
    cpuinfo
        .lines()
        .find_map(|line| line.strip_prefix("model name")?.split_once(':'))
        .map(|(_, value)| value.trim().to_owned())
        .ok_or(OrchestrateError::MachineUnobservable { field: "cpu_model" })
}

#[cfg(not(target_os = "macos"))]
fn observe_kernel() -> Result<String, OrchestrateError> {
    Ok(std::fs::read_to_string("/proc/sys/kernel/osrelease")
        .map_err(|_| OrchestrateError::MachineUnobservable { field: "kernel" })?
        .trim()
        .to_owned())
}

/// darwin has no procfs, so the same two facts come from sysctl. Every
/// campaign refusal that compares a machine reads these, so without them
/// `observe_machine` failed on `cpu_model` and no declaration could be
/// sealed at all -- the orchestrator's own tests included.
#[cfg(target_os = "macos")]
fn observe_cpu_model() -> Result<String, OrchestrateError> {
    sysctl("machdep.cpu.brand_string", "cpu_model")
}

#[cfg(target_os = "macos")]
fn observe_kernel() -> Result<String, OrchestrateError> {
    sysctl("kern.osrelease", "kernel")
}

/// Shelling out rather than binding `sysctlbyname`, because `rustc --version`
/// above already establishes that a probe here may be a subprocess and this
/// crate carries no libc dependency to add one for. An empty value is a
/// failure: a machine field that seals as `""` is worse than one that refuses.
#[cfg(target_os = "macos")]
fn sysctl(name: &'static str, field: &'static str) -> Result<String, OrchestrateError> {
    let value = std::process::Command::new("sysctl")
        .args(["-n", name])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .ok_or(OrchestrateError::MachineUnobservable { field })?;
    if value.is_empty() {
        return Err(OrchestrateError::MachineUnobservable { field });
    }
    Ok(value)
}

/// Everything both arms must have shared: argv, environment, affinity,
/// workers, the freshly observed input digests, and the machine.
///
/// Deliberately excludes the binary. That exclusion is the point: the two arms
/// differ only in the binary, so this is the one identity that can be compared
/// *across* them — and a one-pair screen has no other cross-arm check at all.
fn shared_fingerprint(
    execution: &Execution<'_>,
    machine: &Machine,
    workload: &[(String, String)],
) -> Result<String, OrchestrateError> {
    let mut hasher = blake3::Hasher::new();
    let mut field = |tag: &str, value: &str| {
        hasher.update(tag.as_bytes());
        hasher.update(b"\x1f");
        hasher.update(value.as_bytes());
        hasher.update(b"\x1e");
    };
    for argument in execution.argv {
        field("argv", argument);
    }
    for entry in execution.env {
        field("env", entry);
    }
    field("affinity", execution.affinity);
    field(
        "workers",
        &execution
            .workers
            .map_or_else(String::new, |n| n.to_string()),
    );
    for input in execution.inputs {
        field("input", &input.to_string_lossy());
        field("input-digest", &digest_file(input)?);
    }
    for (path, identity) in workload {
        field("workload", path);
        field("workload-identity", identity);
    }
    field("cpu_model", &machine.cpu_model);
    field("kernel", &machine.kernel);
    Ok(format!("blake3:{}", hasher.finalize().to_hex()))
}

/// Everything one execution actually was.
pub struct Execution<'a> {
    pub argv: &'a [String],
    /// Allowlisted environment, name and value, in the order the caller passed
    /// them. Order is part of the fingerprint because the caller controls it
    /// and a stable orchestrator produces a stable order.
    pub env: &'a [String],
    pub affinity: &'a str,
    pub workers: Option<u32>,
    /// The arm binary, rehashed immediately before the run.
    pub binary: &'a Path,
    /// Every small file input, rehashed immediately before the run. Prepared
    /// artifacts are not here: they carry their own authenticated identity,
    /// checked separately, because rehashing gigabytes per block would cost
    /// more than the measurement it protects.
    pub inputs: &'a [PathBuf],
}

/// What the block actually ran, as one digest, including the binary.
///
/// Argv alone would prove nothing — it carries *paths*, so a catalog edited
/// between blocks leaves every argv identical. The observed half is what makes
/// this checkable, and it is why [`crate::campaign::Refusal::ExecFingerprintDiffers`]
/// can be stated as a fact about the runs rather than a hope about the setup.
///
/// Because it covers the binary it is arm-specific, and so can only be
/// compared within one arm; `shared_fingerprint` is the other half.
pub fn exec_fingerprint(
    execution: &Execution<'_>,
    machine: &Machine,
    workload: &[(String, String)],
) -> Result<String, OrchestrateError> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(shared_fingerprint(execution, machine, workload)?.as_bytes());
    hasher.update(b"\x1e");
    hasher.update(digest_file(execution.binary)?.as_bytes());
    Ok(format!("blake3:{}", hasher.finalize().to_hex()))
}

/// A prepared artifact's recorded identity.
///
/// Read from the artifact's own manifest, so it costs a manifest parse and no
/// hashing — which is what lets it run before *every* execution rather than
/// once per invocation of the orchestrator.
///
/// It covers the stored token and location components by digest, the
/// dictionary and token schema the tokens were produced under, and the
/// document inventory. The document inventory alone is not enough for
/// performance evidence: the same documents retokenized under a different
/// dictionary change token count, attributes, predicates, matches and
/// therefore work, while leaving that digest identical.
pub fn prepared_identity(path: &Path) -> Result<String, OrchestrateError> {
    let artifact = lingrep_corpus::CorpusArtifact::open(path).map_err(|cause| {
        OrchestrateError::Unreadable {
            path: path.to_path_buf(),
            cause: cause.to_string(),
        }
    })?;
    Ok(artifact.recorded_identity().digest())
}

/// Re-observe every declared workload input and refuse any that moved.
///
/// A file is rehashed; a prepared artifact's manifest is reread. Both happen
/// inside `record`, which runs once per measured execution, so "re-observed
/// per block" is a property of where this is called rather than a discipline
/// the caller has to keep.
fn observe_workload(manifest: &Manifest) -> Result<Vec<(String, String)>, OrchestrateError> {
    let mut observed = Vec::new();
    for input in &manifest.workload {
        let identity = match input.kind {
            WorkloadKind::File => digest_file(Path::new(&input.path))?,
            WorkloadKind::PreparedArtifact => prepared_identity(Path::new(&input.path))?,
        };
        if identity != input.identity {
            return Err(OrchestrateError::WorkloadChanged {
                path: input.path.clone(),
                declared: input.identity.clone(),
                observed: identity,
            });
        }
        observed.push((input.path.clone(), identity));
    }
    Ok(observed)
}

/// Establish a workload input's identity at declare time.
pub fn workload_input(path: &str, kind: WorkloadKind) -> Result<WorkloadInput, OrchestrateError> {
    let identity = match kind {
        WorkloadKind::File => digest_file(Path::new(path))?,
        WorkloadKind::PreparedArtifact => prepared_identity(Path::new(path))?,
    };
    Ok(WorkloadInput {
        path: path.to_owned(),
        kind,
        identity,
    })
}

/// `lto` and `codegen-units` as the workspace actually declares them.
///
/// Read rather than accepted as a flag: an unoptimized build invalidates
/// everything downstream, and a manifest whose build stanza is a claim proves
/// nothing about the binaries beside it.
pub fn read_build_profile(root: &Path, profile: &str) -> Result<BuildProfile, OrchestrateError> {
    let text = std::fs::read_to_string(root.join("Cargo.toml")).map_err(|cause| {
        OrchestrateError::Unreadable {
            path: root.join("Cargo.toml"),
            cause: cause.to_string(),
        }
    })?;
    let header = format!("[profile.{profile}]");
    let mut in_section = false;
    let (mut lto, mut codegen_units) = (None, None);
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_section = line == header;
            continue;
        }
        if !in_section {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            let value = value.trim().trim_matches('"').to_owned();
            match key.trim() {
                "lto" => lto = Some(value),
                "codegen-units" => codegen_units = value.parse().ok(),
                _ => {}
            }
        }
    }
    if !text.contains(&header) {
        return Err(OrchestrateError::ProfileUnreadable {
            profile: profile.to_owned(),
        });
    }
    Ok(BuildProfile {
        profile: profile.to_owned(),
        // Cargo's own defaults, so a profile that leaves a key out records
        // what it will actually be compiled with.
        lto: lto.unwrap_or_else(|| "false".to_owned()),
        codegen_units: codegen_units.unwrap_or(16),
    })
}

/// The guardrail suite as it stands in this tree.
pub fn campaign_catalogs() -> Vec<CampaignCatalog> {
    lingrep_core::testing::catalogs::catalog_campaign_descriptors()
        .into_iter()
        .map(|descriptor| CampaignCatalog {
            path: descriptor.path.to_owned(),
            role: match descriptor.role {
                lingrep_core::testing::catalogs::GuardrailRole::Substantive => Role::Substantive,
                lingrep_core::testing::catalogs::GuardrailRole::Floor => Role::Floor,
            },
            regimes: descriptor
                .regimes
                .iter()
                .map(|regime| format!("{regime:?}"))
                .collect(),
        })
        .collect()
}

/// Every regime tag a campaign may name, for a caller that mistyped one.
pub fn known_regimes() -> BTreeSet<String> {
    campaign_catalogs()
        .into_iter()
        .flat_map(|catalog| catalog.regimes)
        .collect()
}

/// What `declare` needs that is not observable.
pub struct Declaration {
    pub campaign: String,
    pub tier: Tier,
    pub bound: Bound,
    pub runner_up_bound: Option<Bound>,
    pub profile_evidence: String,
    pub ladder_rung: LadderRung,
    pub target: Option<Target>,
    pub primary_catalog: String,
    pub primary_metric: Metric,
    pub affected_regimes: BTreeSet<String>,
    pub modes: Vec<String>,
    pub minimum_useful_effect_pct: f64,
    pub guardrail_ceilings_pct: BTreeMap<Metric, f64>,
    pub escalation_ladder: Vec<usize>,
    pub build: BuildProfile,
    pub arms: Arms,
    /// What this campaign measures over. Sealed by the manifest digest,
    /// because a path inside argv is not an identity.
    pub workload: Vec<WorkloadInput>,
    /// The command every run executes. Required.
    pub command_template: Vec<TemplateArg>,
    /// The worker pool every run is executed with.
    pub workers: Option<u32>,
    /// How each run's output reaches the hasher.
    pub output_capture: OutputCapture,
}

/// Build the two arms' provenance by hashing what was actually built.
pub fn arm(label: &str, commit: &str, binary: &Path) -> Result<Arm, OrchestrateError> {
    Ok(Arm {
        label: label.to_owned(),
        commit: commit.to_owned(),
        binary_digest: digest_file(binary)?,
    })
}

/// Seal a declaration into a campaign with no observations yet.
pub fn declare(declaration: Declaration) -> Result<Campaign, OrchestrateError> {
    declare_with_catalog_corpora(declaration, BTreeMap::new())
}

/// Seal a declaration whose prepared artifact can vary by catalog.
///
/// The map is a separate argument rather than a second command source. Its
/// paths are substituted only through [`TemplateArg::Corpus`], and every path
/// must also appear among the authenticated prepared-artifact workloads.
pub fn declare_with_catalog_corpora(
    declaration: Declaration,
    catalog_corpora: BTreeMap<String, String>,
) -> Result<Campaign, OrchestrateError> {
    // Required here rather than tolerated, so the only campaigns without a
    // sealed command are the ones that already exist on disk.
    if declaration.command_template.is_empty() {
        return Err(OrchestrateError::CommandTemplateMissing);
    }
    let catalogs = campaign_catalogs();
    if !catalogs
        .iter()
        .any(|catalog| catalog.path == declaration.primary_catalog)
    {
        return Err(OrchestrateError::UnknownCatalog {
            catalog: declaration.primary_catalog,
            known: catalogs.into_iter().map(|catalog| catalog.path).collect(),
        });
    }
    let has_corpus_hole = declaration
        .command_template
        .iter()
        .any(|argument| matches!(argument, TemplateArg::Corpus));
    if !catalog_corpora.is_empty() && !has_corpus_hole {
        return Err(OrchestrateError::CorpusMappingWithoutHole);
    }
    if has_corpus_hole {
        if let Some(unknown) = catalog_corpora.keys().find(|catalog| {
            !catalogs
                .iter()
                .any(|known| known.path.as_str() == catalog.as_str())
        }) {
            return Err(OrchestrateError::UnknownCatalog {
                catalog: unknown.clone(),
                known: catalogs
                    .iter()
                    .map(|catalog| catalog.path.clone())
                    .collect(),
            });
        }
        for catalog in &catalogs {
            if !catalog_corpora.contains_key(&catalog.path) {
                return Err(OrchestrateError::CorpusNotMapped {
                    catalog: catalog.path.clone(),
                });
            }
        }
        let prepared: BTreeSet<&str> = declaration
            .workload
            .iter()
            .filter(|input| input.kind == WorkloadKind::PreparedArtifact)
            .map(|input| input.path.as_str())
            .collect();
        for (catalog, corpus) in &catalog_corpora {
            if !prepared.contains(corpus.as_str()) {
                return Err(OrchestrateError::CorpusNotDeclared {
                    catalog: catalog.clone(),
                    corpus: corpus.clone(),
                });
            }
        }
        let used: BTreeSet<&str> = catalog_corpora.values().map(String::as_str).collect();
        if let Some(unmapped) = prepared.difference(&used).next() {
            return Err(OrchestrateError::PreparedArtifactUnmapped {
                corpus: (*unmapped).to_owned(),
            });
        }
    }
    let manifest = Manifest {
        campaign: declaration.campaign,
        tier: declaration.tier,
        bound: declaration.bound,
        runner_up_bound: declaration.runner_up_bound,
        profile_evidence: Some(declaration.profile_evidence),
        ladder_rung: declaration.ladder_rung,
        target: declaration.target,
        primary_catalog: declaration.primary_catalog,
        primary_metric: declaration.primary_metric,
        affected_regimes: declaration.affected_regimes,
        catalogs,
        modes: declaration.modes,
        minimum_useful_effect_pct: declaration.minimum_useful_effect_pct,
        guardrail_ceilings_pct: declaration.guardrail_ceilings_pct,
        escalation_ladder: declaration.escalation_ladder,
        build: declaration.build,
        machine: observe_machine()?,
        arms: declaration.arms,
        workload: declaration.workload,
        catalog_corpora,
        command_template: declaration.command_template,
        workers: declaration.workers,
        output_capture: declaration.output_capture,
    };
    let manifest_digest = manifest
        .digest()
        .ok_or(OrchestrateError::MachineUnobservable { field: "manifest" })?;
    Ok(Campaign {
        manifest,
        manifest_digest,
        observations: Vec::new(),
        selections: Vec::new(),
    })
}

/// Where a run's output identity comes from.
///
/// A path is hashed here; a digest was hashed by `digest-stream` as the run
/// wrote it, and never touched the disk. The two produce the same number for
/// the same output, so a campaign may use either and baselines do not move.
pub enum OutputIdentity<'a> {
    File(&'a Path),
    Digest(&'a str),
}

impl OutputIdentity<'_> {
    fn capture(&self) -> OutputCapture {
        match self {
            Self::File(_) => OutputCapture::File,
            Self::Digest(_) => OutputCapture::Stream,
        }
    }

    fn resolve(&self) -> Result<String, OrchestrateError> {
        match self {
            Self::File(path) => output_digest(path),
            Self::Digest(value) => {
                let hex = value.strip_prefix("blake3:").unwrap_or_default();
                if hex.len() != 64 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                    return Err(OrchestrateError::MalformedOutputDigest {
                        value: (*value).to_owned(),
                    });
                }
                if hex.bytes().any(|byte| byte.is_ascii_uppercase()) {
                    return Err(OrchestrateError::MalformedOutputDigest {
                        value: (*value).to_owned(),
                    });
                }
                Ok((*value).to_owned())
            }
        }
    }
}

/// One measurement, as the orchestrator observed it.
pub struct Measurement {
    pub catalog: String,
    pub arm: String,
    pub block: usize,
    pub order: usize,
    pub mode: Option<String>,
    pub ok: bool,
    pub wall_s: f64,
    pub user_s: f64,
    pub sys_s: f64,
    pub peak_rss_kb: f64,
}

/// Append one observation to a campaign, refusing anything that would make the
/// appended block incomparable with the ones already there.
///
/// The checks are here rather than in `adjudicate` for one reason: this is the
/// only moment at which the *observed* world is still available. Adjudication
/// happens later, possibly elsewhere, and sees only what was written down.
pub fn record(
    path: &Path,
    measurement: Measurement,
    execution: &Execution<'_>,
    output: &OutputIdentity<'_>,
) -> Result<Observation, OrchestrateError> {
    let text = String::from_utf8_lossy(&read(path)?).into_owned();
    let mut campaign: Campaign =
        serde_json::from_str(&text).map_err(|cause| OrchestrateError::NotACampaign {
            path: path.to_path_buf(),
            cause: cause.to_string(),
        })?;

    let declared_arm = match measurement.arm.as_str() {
        "a" => &campaign.manifest.arms.a,
        "b" => &campaign.manifest.arms.b,
        other => {
            return Err(OrchestrateError::UnknownArm {
                arm: other.to_owned(),
            });
        }
    };
    let binary_digest = digest_file(execution.binary)?;
    if binary_digest != declared_arm.binary_digest {
        return Err(OrchestrateError::BinarySwapped {
            arm: measurement.arm,
            declared: declared_arm.binary_digest.clone(),
            observed: binary_digest,
        });
    }
    if !campaign
        .manifest
        .catalogs
        .iter()
        .any(|catalog| catalog.path == measurement.catalog)
    {
        return Err(OrchestrateError::UnknownCatalog {
            catalog: measurement.catalog,
            known: campaign
                .manifest
                .catalogs
                .iter()
                .map(|catalog| catalog.path.clone())
                .collect(),
        });
    }

    // The command, reconstructed from the seal and compared exactly. This is
    // what argv's presence in the execution fingerprint cannot do: that keeps
    // the two arms consistent with each other, and says nothing about whether
    // either ran what the campaign was declared to measure.
    //
    // The failure it catches is a wrapper that mangles argv between reading
    // the seal and executing it — the class the runner's quoting bug was in.
    // It is deliberately *not* the check for an edited manifest: editing the
    // template moves the manifest digest, so `adjudicate` refuses that with
    // `ManifestAltered`, and duplicating the check here would leave two
    // answers to one question.
    let expected = campaign
        .manifest
        .expected_argv(&measurement.catalog)
        .map_err(|_| OrchestrateError::CorpusNotMapped {
            catalog: measurement.catalog.clone(),
        })?;
    if let Some(expected) = expected
        && expected != execution.argv
    {
        return Err(OrchestrateError::CommandDiffers {
            catalog: measurement.catalog,
            expected,
            observed: execution.argv.to_vec(),
        });
    }

    // How the output was captured, checked against the seal, for the same
    // reason the pool is: it is part of the rig rather than part of the
    // answer, and a campaign whose rig moved halfway is comparing two of them.
    if campaign.manifest.output_capture != output.capture() {
        return Err(OrchestrateError::OutputCaptureDiffers {
            sealed: campaign.manifest.output_capture,
            observed: output.capture(),
        });
    }

    // The pool, checked against the seal. `record` already carries it into
    // both fingerprints, which keeps the runs consistent with each other and
    // says nothing about whether they used the pool the campaign declared —
    // and occupancy is divided by the sealed count, so a run at a different
    // width would be reported against the wrong denominator.
    if campaign.manifest.workers.is_some() && campaign.manifest.workers != execution.workers {
        return Err(OrchestrateError::WorkerCountDiffers {
            sealed: campaign.manifest.workers,
            observed: execution.workers,
        });
    }

    // Where this catalog left off. An interrupted campaign must resume the
    // block it was in the middle of: skipping to the next one leaves the
    // partial block permanently unbalanced, and adjudication then refuses the
    // whole campaign for a reason no later run can repair.
    let (expected_block, expected_order) = progress(&campaign, &measurement.catalog).next;
    if (measurement.block, measurement.order) != (expected_block, expected_order) {
        return Err(OrchestrateError::OutOfSequence {
            catalog: measurement.catalog,
            expected: format!("block {expected_block} order {expected_order}"),
            found: format!("block {} order {}", measurement.block, measurement.order),
        });
    }

    let machine = observe_machine()?;
    for (field, declared, observed) in [
        (
            "cpu_model",
            &campaign.manifest.machine.cpu_model,
            &machine.cpu_model,
        ),
        ("kernel", &campaign.manifest.machine.kernel, &machine.kernel),
    ] {
        if declared != observed {
            return Err(OrchestrateError::MachineChanged {
                field,
                declared: declared.clone(),
                observed: observed.clone(),
            });
        }
    }

    let workload = observe_workload(&campaign.manifest)?;
    let observation = Observation {
        manifest_digest: campaign.manifest_digest.clone(),
        exec_fingerprint: exec_fingerprint(execution, &machine, &workload)?,
        input_fingerprint: shared_fingerprint(execution, &machine, &workload)?,
        output_digest: output.resolve()?,
        catalog: measurement.catalog,
        arm: measurement.arm,
        block: measurement.block,
        order: measurement.order,
        mode: measurement.mode,
        ok: measurement.ok,
        wall_s: measurement.wall_s,
        user_s: measurement.user_s,
        sys_s: measurement.sys_s,
        peak_rss_kb: measurement.peak_rss_kb,
    };
    campaign.observations.push(observation.clone());

    write_campaign(path, &campaign)?;
    Ok(observation)
}

/// Write a campaign atomically.
///
/// The discipline lives in [`crate::campaign::write`], which owns the file
/// format; this only maps its error into the orchestrator's.
pub fn write_campaign(path: &Path, campaign: &Campaign) -> Result<(), OrchestrateError> {
    crate::campaign::write(path, campaign).map_err(|cause| OrchestrateError::Unreadable {
        path: path.to_path_buf(),
        cause: cause.to_string(),
    })
}

#[cfg(test)]
mod tests;
