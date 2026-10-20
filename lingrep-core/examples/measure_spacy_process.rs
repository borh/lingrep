#[cfg(target_os = "linux")]
use std::collections::{HashMap, HashSet};
#[cfg(target_os = "linux")]
use std::error::Error;
#[cfg(target_os = "linux")]
use std::fs::File;
#[cfg(target_os = "linux")]
use std::io::{self, Read};
#[cfg(target_os = "linux")]
use std::path::{Path, PathBuf};
#[cfg(target_os = "linux")]
use std::process::Command;
#[cfg(target_os = "linux")]
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(target_os = "linux")]
use std::sync::{Arc, Mutex};
#[cfg(target_os = "linux")]
use std::thread;
#[cfg(target_os = "linux")]
use std::time::{Duration, Instant};

#[cfg(target_os = "linux")]
use clap::Parser;
#[cfg(target_os = "linux")]
use lingrep_core::analysis::{
    AnalyzerDescriptor, AnalyzerFamily, ExternalComponent, ExternalProtocolLimits,
    ExternalRunConfig, ExtractionProfile, InputUnitId, NormalizationPolicy, SegmentationMode,
    TextUnit, external_component_closure_digest,
};
#[cfg(target_os = "linux")]
use lingrep_core::execution::{
    ExecutionLimits, ExternalAdapterCommand, ExternalAdapterProcess, VerifiedAdapterManifest,
};
#[cfg(target_os = "linux")]
use lingrep_core::token_match::{
    AttributeKind, AttributeOrdinal, AttributeValueRef, SchemaAttribute, TokenSchema,
};
#[cfg(target_os = "linux")]
use serde_json::json;

#[cfg(target_os = "linux")]
const WORKLOAD: [&str; 4] = [
    "猫が走る。",
    "東京都で自然言語処理を研究する。",
    "新しいモデルを評価した。",
    "形態素解析の結果を比較する。",
];

#[cfg(target_os = "linux")]
#[derive(Parser)]
struct Args {
    #[arg(long)]
    python: PathBuf,
    #[arg(long, default_value = ".")]
    manifest_root: PathBuf,
    #[arg(long)]
    model_root: PathBuf,
    #[arg(long, value_parser = ["cpu", "rocm"])]
    backend: String,
    #[arg(long, default_value_t = 8)]
    batches: u64,
    #[arg(long, default_value_t = 32)]
    documents_per_batch: u64,
}

#[cfg(target_os = "linux")]
fn main() -> Result<(), Box<dyn Error>> {
    let args = Args::parse();
    if args.batches == 0 || args.documents_per_batch == 0 {
        return Err("batches and documents-per-batch must be positive".into());
    }
    let root = args.manifest_root.canonicalize()?;
    let entrypoint = PathBuf::from("python/spacy-adapter/bin/lingrep-spacy-adapter");
    let project = PathBuf::from("python/spacy-adapter");
    let mut implementation = Vec::new();
    for relative in [
        project.join("bin/lingrep-spacy-adapter"),
        project.join("pyproject.toml"),
        project.join("uv.lock"),
    ] {
        implementation.push(hash_entry(&root, &relative)?);
    }
    collect_entries(&root, &project.join("src"), &mut implementation)?;
    implementation.sort_by(|left, right| left.0.cmp(&right.0));
    let mut resources = Vec::new();
    collect_entries(&root, &args.model_root, &mut resources)?;
    resources.sort_by(|left, right| left.0.cmp(&right.0));

    let model = root.join(&args.model_root);
    let profile: serde_json::Value =
        serde_json::from_slice(&std::fs::read(model.join("lingrep-spacy-model.json"))?)?;
    let selection = &profile["selections"]["morphology/input"];
    let profile_digest = decode_digest(
        profile["selection_digests"]["morphology/input"]
            .as_str()
            .ok_or("missing morphology/input selection digest")?,
    )?;
    let schema = TokenSchema::new(
        selection["schema"]
            .as_array()
            .ok_or("missing morphology/input schema")?
            .iter()
            .map(|attribute| {
                let key = attribute["key"].as_str().ok_or("schema key is not text")?;
                let kind = match attribute["kind"].as_str() {
                    Some("scalar") => AttributeKind::Scalar,
                    Some("string_set") => AttributeKind::StringSet,
                    _ => return Err("unsupported schema kind"),
                };
                SchemaAttribute::new(key, kind).map_err(|_| "invalid schema attribute")
            })
            .collect::<Result<Vec<_>, _>>()?,
    )?;
    let projection = schema.bind_projection(schema.keys().iter().cloned())?;
    let command = ExternalAdapterCommand::spacy(
        args.python,
        &entrypoint,
        &args.model_root,
        &args.backend,
        u32::try_from(args.documents_per_batch)?,
    )?;
    let sampler = if args.backend == "rocm" {
        Some(RocmSampler::start()?)
    } else {
        None
    };
    let measurement_started = Instant::now();
    let manifest = VerifiedAdapterManifest::new(
        &root,
        implementation,
        resources,
        &command,
        vec![ExternalComponent::new("spacy-profile", profile_digest)?],
    )?;
    let implementation_digest = manifest.implementation_digest();
    let resource_manifest_digest = manifest.resource_manifest_digest();
    let descriptor = AnalyzerDescriptor::new(
        AnalyzerFamily::External,
        ExtractionProfile::Morphology,
        SegmentationMode::Input,
        NormalizationPolicy::None,
        profile["language"]
            .as_str()
            .ok_or("model language missing")?,
        selection["boundary_policy"]
            .as_str()
            .ok_or("boundary policy missing")?,
        implementation_digest,
        resource_manifest_digest,
        [0; 32],
        [0; 32],
        external_component_closure_digest(manifest.components()),
        profile_digest,
        schema.digest(),
    )?;
    let config = ExternalRunConfig::new(
        "spacy-process-capacity-v1",
        descriptor,
        schema,
        projection.clone(),
        manifest.components().to_vec(),
        ExternalProtocolLimits::default(),
    )?;
    let mut process =
        ExternalAdapterProcess::launch(command, manifest, config, Arc::new(|_| Ok(())))?;
    let process_id = u32::try_from(process.process_group_id())?;
    let ready_seconds = measurement_started.elapsed().as_secs_f64();
    let runtime = process.execution_provenance().runtime.clone();
    let analysis_started = Instant::now();
    let mut first_batch_seconds = None;
    let mut words = 0_u64;
    let mut output = blake3::Hasher::new();
    for batch in 0..args.batches {
        let inputs = (0..args.documents_per_batch)
            .map(|document| -> Result<_, Box<dyn Error>> {
                let id = batch * args.documents_per_batch + document;
                let workload_index = usize::try_from(id % u64::try_from(WORKLOAD.len())?)?;
                Ok(TextUnit::sentence(
                    InputUnitId::new(id),
                    WORKLOAD[workload_index],
                    NormalizationPolicy::None,
                )?)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let analyzed = process.exchange_batch(inputs, &ExecutionLimits::default())?;
        if first_batch_seconds.is_none() {
            first_batch_seconds = Some(analysis_started.elapsed().as_secs_f64());
        }
        digest_batch(&mut output, analyzed.analysis(), &projection, &mut words)?;
    }
    let analysis_seconds = analysis_started.elapsed().as_secs_f64();
    process.finish()?;
    let peak_accelerator_bytes = sampler
        .map(|sampler| sampler.finish(process_id))
        .transpose()?
        .flatten();
    let documents = args.batches * args.documents_per_batch;
    println!(
        "{}",
        serde_json::to_string(&json!({
            "backend": args.backend,
            "batches": args.batches,
            "documents_per_batch": args.documents_per_batch,
            "documents": documents,
            "words": words,
            "ready_seconds": ready_seconds,
            "first_observable_batch_seconds_after_ready": first_batch_seconds,
            "analysis_seconds": analysis_seconds,
            "words_per_second": words as f64 / analysis_seconds,
            "max_individual_process_rss_kib": peak_rss_kib()?,
            "peak_accelerator_process_bytes": peak_accelerator_bytes,
            "output_blake3": output.finalize().to_hex().as_str(),
            "implementation_digest": hex(&implementation_digest),
            "resource_manifest_digest": hex(&resource_manifest_digest),
            "schema_digest": hex(projection.schema_digest().as_bytes()),
            "runtime": runtime,
        }))?
    );
    Ok(())
}

#[cfg(target_os = "linux")]
fn digest_batch(
    digest: &mut blake3::Hasher,
    batch: &lingrep_core::analysis::ValidatedAnalysisBatch,
    projection: &lingrep_core::token_match::BoundProjection,
    words: &mut u64,
) -> Result<(), Box<dyn Error>> {
    for input in batch.inputs() {
        frame(digest, input.input().text().as_bytes())?;
        for sentence in input.sentences() {
            digest.update(&sentence.source_range().start().to_le_bytes());
            digest.update(&sentence.source_range().end().to_le_bytes());
            for surface in sentence.surface_tokens() {
                digest.update(&surface.source_range().start().to_le_bytes());
                digest.update(&surface.source_range().end().to_le_bytes());
                digest.update(&surface.word_range().start.to_le_bytes());
                digest.update(&surface.word_range().end.to_le_bytes());
            }
            *words = words
                .checked_add(u64::try_from(sentence.words().len())?)
                .ok_or("word count overflow")?;
            for token in 0..sentence.words().len() {
                let row = sentence.words().token(token).ok_or("token row missing")?;
                for ordinal in 0..projection.len() {
                    match row.value(AttributeOrdinal::new(u8::try_from(ordinal)?)) {
                        None => {
                            digest.update(&[0]);
                        }
                        Some(AttributeValueRef::Scalar(value)) => {
                            digest.update(&[1]);
                            frame(digest, value.as_bytes())?;
                        }
                        Some(AttributeValueRef::StringSet(values)) => {
                            digest.update(&[2]);
                            digest.update(&u64::try_from(values.len())?.to_le_bytes());
                            for value in values {
                                frame(digest, value.as_bytes())?;
                            }
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn frame(digest: &mut blake3::Hasher, bytes: &[u8]) -> Result<(), Box<dyn Error>> {
    digest.update(&u64::try_from(bytes.len())?.to_le_bytes());
    digest.update(bytes);
    Ok(())
}

#[cfg(target_os = "linux")]
fn collect_entries(
    root: &Path,
    relative: &Path,
    entries: &mut Vec<(PathBuf, [u8; 32])>,
) -> Result<(), Box<dyn Error>> {
    let absolute = root.join(relative);
    if absolute.is_symlink() {
        return Err(format!("{} is a symlink", relative.display()).into());
    }
    if absolute.is_file() {
        entries.push(hash_entry(root, relative)?);
        return Ok(());
    }
    if !absolute.is_dir() {
        return Err(format!("{} is not a regular file or directory", relative.display()).into());
    }
    for child in std::fs::read_dir(absolute)? {
        let child = child?;
        collect_entries(root, &relative.join(child.file_name()), entries)?;
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn hash_entry(root: &Path, relative: &Path) -> Result<(PathBuf, [u8; 32]), Box<dyn Error>> {
    let mut stream = File::open(root.join(relative))?;
    let mut digest = blake3::Hasher::new();
    let mut buffer = [0_u8; 1024 * 1024];
    loop {
        let read = stream.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok((relative.to_path_buf(), *digest.finalize().as_bytes()))
}

#[cfg(target_os = "linux")]
fn decode_digest(value: &str) -> Result<[u8; 32], Box<dyn Error>> {
    if value.len() != 64 {
        return Err("digest must contain 64 hexadecimal characters".into());
    }
    let mut bytes = [0_u8; 32];
    for (index, chunk) in value.as_bytes().as_chunks::<2>().0.iter().enumerate() {
        bytes[index] = u8::from_str_radix(std::str::from_utf8(chunk)?, 16)?;
    }
    Ok(bytes)
}

#[cfg(target_os = "linux")]
fn hex(bytes: &[u8; 32]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(target_os = "linux")]
struct RocmSampler {
    stop: Arc<AtomicBool>,
    samples: Arc<Mutex<HashMap<u32, u64>>>,
    thread: thread::JoinHandle<()>,
}

#[cfg(target_os = "linux")]
impl RocmSampler {
    fn start() -> Result<Self, Box<dyn Error>> {
        let baseline = rocm_process_rows()?.into_keys().collect::<HashSet<_>>();
        let stop = Arc::new(AtomicBool::new(false));
        let samples = Arc::new(Mutex::new(HashMap::<u32, u64>::new()));
        let thread_stop = Arc::clone(&stop);
        let thread_samples = Arc::clone(&samples);
        let thread = thread::spawn(move || {
            while !thread_stop.load(Ordering::Relaxed) {
                if let Ok(rows) = rocm_process_rows()
                    && let Ok(mut maxima) = thread_samples.lock()
                {
                    for (pid, bytes) in rows {
                        if !baseline.contains(&pid) {
                            maxima
                                .entry(pid)
                                .and_modify(|maximum| *maximum = (*maximum).max(bytes))
                                .or_insert(bytes);
                        }
                    }
                }
                thread::sleep(Duration::from_millis(20));
            }
        });
        Ok(Self {
            stop,
            samples,
            thread,
        })
    }

    fn finish(self, pid: u32) -> Result<Option<u64>, Box<dyn Error>> {
        self.stop.store(true, Ordering::Relaxed);
        self.thread
            .join()
            .map_err(|_| "ROCm sampler thread panicked")?;
        Ok(self
            .samples
            .lock()
            .map_err(|_| "ROCm sampler mutex poisoned")?
            .get(&pid)
            .copied())
    }
}

#[cfg(target_os = "linux")]
fn rocm_process_rows() -> Result<HashMap<u32, u64>, Box<dyn Error>> {
    let output = Command::new("rocm-smi")
        .args(["--showpids", "details"])
        .output()?;
    if !output.status.success() {
        return Err("rocm-smi failed".into());
    }
    Ok(parse_rocm_process_rows(&String::from_utf8(output.stdout)?))
}

#[cfg(target_os = "linux")]
fn parse_rocm_process_rows(output: &str) -> HashMap<u32, u64> {
    output
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let pid = fields.next()?.parse().ok()?;
            fields.next()?;
            fields.next()?.parse::<u32>().ok()?;
            let bytes = fields.next()?.parse().ok()?;
            Some((pid, bytes))
        })
        .collect()
}

#[cfg(all(test, target_os = "linux"))]
fn parse_rocm_process_vram(output: &str, pid: u32) -> Option<u64> {
    parse_rocm_process_rows(output).get(&pid).copied()
}

#[cfg(target_os = "linux")]
fn peak_rss_kib() -> Result<i64, io::Error> {
    let mut self_usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
    let mut child_usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
    if unsafe { libc::getrusage(libc::RUSAGE_SELF, self_usage.as_mut_ptr()) } != 0
        || unsafe { libc::getrusage(libc::RUSAGE_CHILDREN, child_usage.as_mut_ptr()) } != 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { self_usage.assume_init() }
        .ru_maxrss
        .max(unsafe { child_usage.assume_init() }.ru_maxrss))
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::parse_rocm_process_vram;

    #[test]
    fn rocm_process_rows_bind_vram_to_the_requested_pid() {
        let output = r#"
KFD process information:
PID     PROCESS NAME GPU VRAM USED SDMA USED CU OCCUPANCY
1234    python       1   261496832 0         UNKNOWN
5678    python       1   987654321 0         UNKNOWN
"#;

        assert_eq!(parse_rocm_process_vram(output, 5678), Some(987_654_321));
        assert_eq!(parse_rocm_process_vram(output, 42), None);
    }
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("measure_spacy_process is supported only on Linux");
}
