#[cfg(target_os = "linux")]
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

#[cfg(target_os = "linux")]
use std::error::Error;
#[cfg(target_os = "linux")]
use std::io;
#[cfg(target_os = "linux")]
use std::path::PathBuf;
#[cfg(target_os = "linux")]
use std::sync::Arc;
#[cfg(target_os = "linux")]
use std::time::Instant;

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
use lingrep_core::token_match::{AttributeKey, AttributeKind, SchemaAttribute, TokenSchema};

#[cfg(target_os = "linux")]
fn main() -> Result<(), Box<dyn Error>> {
    const BATCHES: u64 = 100;
    const DOCUMENTS_PER_BATCH: u64 = 64;

    let adapter =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/external_echo_adapter.py");
    let adapter_bytes = std::fs::read(&adapter)?;
    let adapter_digest = *blake3::hash(&adapter_bytes).as_bytes();
    let command = ExternalAdapterCommand::interpreted(
        "python3",
        "external_echo_adapter.py",
        [PathBuf::from("echo")],
    )
    .map_err(io::Error::other)?;
    let manifest = VerifiedAdapterManifest::new(
        adapter
            .parent()
            .ok_or_else(|| io::Error::other("adapter has no parent"))?,
        [(PathBuf::from("external_echo_adapter.py"), adapter_digest)],
        std::iter::empty::<(PathBuf, [u8; 32])>(),
        &command,
        vec![ExternalComponent::new("adapter", adapter_digest).map_err(io::Error::other)?],
    )?;
    let schema = TokenSchema::new([
        SchemaAttribute::new("text", AttributeKind::Scalar)?,
        SchemaAttribute::new("lemma", AttributeKind::Scalar)?,
    ])?;
    let projection =
        schema.bind_projection([AttributeKey::new("text")?, AttributeKey::new("lemma")?])?;
    let descriptor = AnalyzerDescriptor::new(
        AnalyzerFamily::External,
        ExtractionProfile::Morphology,
        SegmentationMode::Input,
        NormalizationPolicy::None,
        "ja",
        "input-v1",
        manifest.implementation_digest(),
        manifest.resource_manifest_digest(),
        [0; 32],
        [0; 32],
        external_component_closure_digest(manifest.components()),
        [0; 32],
        schema.digest(),
    )
    .map_err(io::Error::other)?;
    let config = ExternalRunConfig::new(
        "echo-capacity-v1",
        descriptor,
        schema,
        projection,
        manifest.components().to_vec(),
        ExternalProtocolLimits::default(),
    )?;
    let started = Instant::now();
    let mut process =
        ExternalAdapterProcess::launch(command, manifest, config, Arc::new(|_| Ok(())))?;
    let mut words = 0_u64;
    for batch in 0..BATCHES {
        let inputs = (0..DOCUMENTS_PER_BATCH)
            .map(|document| {
                let id = batch * DOCUMENTS_PER_BATCH + document;
                TextUnit::sentence(
                    InputUnitId::new(id),
                    format!("外部形態素解析の固定文{id}。"),
                    NormalizationPolicy::None,
                )
            })
            .collect::<Result<Vec<_>, _>>()?;
        let analyzed = process.exchange_batch(inputs, &ExecutionLimits::default())?;
        words += analyzed
            .analysis()
            .inputs()
            .iter()
            .flat_map(lingrep_core::analysis::AnalyzedInput::sentences)
            .map(|sentence| sentence.words().len() as u64)
            .sum::<u64>();
    }
    process.finish()?;
    let elapsed = started.elapsed();
    let documents = BATCHES * DOCUMENTS_PER_BATCH;
    let rss_kib = peak_rss_kib()?;
    println!(
        "{{\"batches\":{BATCHES},\"documents\":{documents},\"words\":{words},\"elapsed_seconds\":{:.6},\"documents_per_second\":{:.3},\"peak_rss_kib\":{rss_kib}}}",
        elapsed.as_secs_f64(),
        documents as f64 / elapsed.as_secs_f64(),
    );
    Ok(())
}

#[cfg(target_os = "linux")]
fn peak_rss_kib() -> Result<i64, io::Error> {
    let mut self_usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
    let mut child_usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
    let self_result = unsafe { libc::getrusage(libc::RUSAGE_SELF, self_usage.as_mut_ptr()) };
    let child_result = unsafe { libc::getrusage(libc::RUSAGE_CHILDREN, child_usage.as_mut_ptr()) };
    if self_result != 0 || child_result != 0 {
        return Err(io::Error::last_os_error());
    }
    let self_usage = unsafe { self_usage.assume_init() };
    let child_usage = unsafe { child_usage.assume_init() };
    Ok(self_usage.ru_maxrss.max(child_usage.ru_maxrss))
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("measure_external_echo is supported only on Linux");
}
