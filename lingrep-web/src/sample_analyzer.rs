use std::{collections::HashMap, path::Path, sync::Arc, time::Duration};

#[cfg(unix)]
use lingrep_cli_common::external_analyzer::ExternalPlan;
use lingrep_core::{
    analysis::{
        AnalyzerDescriptor, AnalyzerPlan, AnalyzerSession, ExtractionProfile, InputUnitId,
        NormalizationPolicy, SegmentationMode, SudachiConfig, SudachiEdition, SudachiPlan,
        SudachiResources, SudachiSplit, TextUnit,
    },
    execution::CancellationFlag,
    token_match::{BoundProjection, BoundTokenSentence, TokenSchema},
};

#[cfg(unix)]
use lingrep_core::execution::{
    ExecutionLimits, ExternalAdapterCancellation, ExternalAnalyzerSession,
};

use crate::sample::{
    ClaimBudget, SampleAnalysis, SampleReport, analyze_live_sample, compile_sample,
};

const INPUT_BYTES: usize = 64 * 1024;
const RESIDENT_BYTES: usize = 8 * 1024 * 1024;
const RUN_TIMEOUT: Duration = Duration::from_secs(20);

pub(crate) struct LiveSampleAnalyzer {
    pub(crate) name: String,
    pub(crate) semantic_identity: String,
    plan: Plan,
    schema: TokenSchema,
    projection: BoundProjection,
    worker: Arc<tokio::sync::Mutex<Option<Worker>>>,
}

enum Plan {
    #[cfg(unix)]
    External(Box<ExternalPlan>),
    Sudachi(Box<SudachiPlan>),
}

struct Worker {
    #[cfg(unix)]
    external: Option<ExternalAnalyzerSession>,
    cancellation: CancellationFlag,
    next_input: u64,
    sample: Option<(blake3::Hash, Vec<BoundTokenSentence>)>,
    claims: HashMap<String, BoundTokenSentence>,
}

#[derive(Debug)]
pub(crate) enum LiveSampleError {
    TimedOut,
    Failed,
}

struct CancelOnDrop(Option<CancellationFlag>);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        if let Some(cancel) = &self.0 {
            cancel.cancel();
        }
    }
}

impl LiveSampleAnalyzer {
    #[cfg(unix)]
    pub(crate) fn load_external(
        path: &Path,
        normalization: NormalizationPolicy,
    ) -> Result<Self, String> {
        let plan = ExternalPlan::load(
            path,
            ExtractionProfile::Morphology,
            SegmentationMode::Input,
            normalization,
        )
        .map_err(|error| error.to_string())?;
        let analyzer = lingrep_corpus::AnalyzerRecord::from_descriptor(&plan.descriptor);
        let name = if plan
            .schema
            .keys()
            .iter()
            .any(|key| key.as_str().starts_with("spacy.ginza."))
        {
            "GiNZA".to_owned()
        } else if plan
            .schema
            .keys()
            .iter()
            .any(|key| key.as_str().starts_with("spacy."))
        {
            "spaCy".to_owned()
        } else if analyzer.family == "stanza" {
            "Stanza".to_owned()
        } else {
            format!("{} {}", analyzer.family, analyzer.profile)
        };
        Self::new(name, Plan::External(Box::new(plan)))
    }

    pub(crate) fn load_sudachi(
        dictionary: &Path,
        resources: Option<&Path>,
        split: SudachiSplit,
        normalization: NormalizationPolicy,
    ) -> Result<Self, String> {
        let edition = match dictionary.file_name().and_then(std::ffi::OsStr::to_str) {
            Some("system_small.dic") => SudachiEdition::Small,
            Some("system_core.dic") => SudachiEdition::Core,
            Some("system_full.dic") => SudachiEdition::Full,
            _ => return Err("the Sudachi dictionary must be named system_small.dic, system_core.dic, or system_full.dic".to_owned()),
        };
        let resources = resources.map_or(SudachiResources::BuiltIn, SudachiResources::directory);
        let (dictionary_digest, resource_digest) =
            SudachiConfig::content_digests(dictionary, &resources)
                .map_err(|error| error.to_string())?;
        let config = SudachiConfig::new(
            dictionary,
            resources,
            dictionary_digest,
            resource_digest,
            edition,
            split,
            ExtractionProfile::Morphology,
            SegmentationMode::Input,
            normalization,
        )
        .map_err(|error| error.to_string())?;
        let plan = SudachiPlan::load(config).map_err(|error| error.to_string())?;
        Self::new(format!("Sudachi {split:?}"), Plan::Sudachi(Box::new(plan)))
    }

    fn new(name: String, plan: Plan) -> Result<Self, String> {
        let schema = plan.schema().clone();
        let projection = schema
            .bind_projection(schema.keys().iter().cloned())
            .map_err(|error| error.to_string())?;
        let analyzer = lingrep_corpus::AnalyzerRecord::from_descriptor(plan.descriptor());
        Ok(Self {
            name,
            semantic_identity: analyzer.semantic_identity,
            schema,
            plan,
            projection,
            worker: Arc::default(),
        })
    }

    pub(crate) async fn analyze(
        self: Arc<Self>,
        pattern: String,
        sample: String,
        permit: tokio::sync::SemaphorePermit<'static>,
    ) -> Result<SampleReport, LiveSampleError> {
        let deadline = tokio::time::Instant::now() + RUN_TIMEOUT;
        let mut worker = tokio::time::timeout_at(deadline, Arc::clone(&self.worker).lock_owned())
            .await
            .map_err(|_| LiveSampleError::TimedOut)?;
        if worker
            .as_ref()
            .is_some_and(|worker| worker.cancellation.is_cancelled())
        {
            *worker = None;
        }
        let active = worker.get_or_insert_with(|| self.new_worker());
        let mut cancel = CancelOnDrop(Some(active.cancellation.clone()));
        let task = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let active = worker
                .as_mut()
                .expect("the analyzer worker was initialized");
            let result = self.run(active, &pattern, &sample);
            if result.is_err() {
                *worker = None;
            }
            result
        });
        let result = tokio::time::timeout_at(deadline, task)
            .await
            .map_err(|_| LiveSampleError::TimedOut)?
            .map_err(|_| LiveSampleError::Failed)?;
        cancel.0 = None;
        result.map_err(|error| {
            tracing::warn!(%error, "Sample analyzer failed");
            LiveSampleError::Failed
        })
    }

    fn new_worker(&self) -> Worker {
        let cancellation = CancellationFlag::new();
        Worker {
            #[cfg(unix)]
            external: self.plan.external_session(&cancellation),
            cancellation,
            next_input: 0,
            sample: None,
            claims: HashMap::new(),
        }
    }

    fn run(
        &self,
        worker: &mut Worker,
        pattern: &str,
        sample: &str,
    ) -> Result<SampleReport, String> {
        let output = match compile_sample(pattern) {
            Ok(output) => output,
            Err(error) => {
                return Ok(SampleReport {
                    analysis: SampleAnalysis::Failed(error),
                    run: None,
                });
            }
        };
        let binding = output.matcher.bind(&self.schema);
        let claims: Vec<_> =
            if binding.is_ok_and(|binding| binding.unavailable_attributes().is_empty()) {
                lingrep_core::fsm::declared_claims(&output.matcher)
                    .take(ClaimBudget::REQUEST.claims)
                    .map(|(_, _, text)| text)
                    .collect()
            } else {
                Vec::new()
            };
        let mut native;
        let session: &mut dyn AnalyzerSession = match &self.plan {
            Plan::Sudachi(plan) => {
                native = plan.create_session();
                &mut native
            }
            #[cfg(unix)]
            Plan::External(_) => worker
                .external
                .as_mut()
                .ok_or("sample session is unavailable")?,
        };
        let normalization = self.plan.descriptor().normalization();
        let digest = blake3::hash(sample.as_bytes());
        if worker
            .sample
            .as_ref()
            .is_none_or(|(cached, _)| *cached != digest)
        {
            worker.sample = None;
            let lines = sample.lines().filter(|line| !line.trim().is_empty());
            let words = analyze_words(
                session,
                &mut worker.next_input,
                lines,
                &self.projection,
                normalization,
            )?;
            worker.sample = Some((digest, words));
        }

        let mut bytes = 0usize;
        let retained = claims
            .iter()
            .take(ClaimBudget::REQUEST.claims)
            .take_while(|text| {
                bytes = bytes.saturating_add(text.len());
                bytes <= INPUT_BYTES
            })
            .count();
        let wanted = &claims[..retained];
        worker
            .claims
            .retain(|text, _| wanted.contains(&text.as_str()));
        let mut missing: Vec<_> = wanted
            .iter()
            .copied()
            .filter(|text| !worker.claims.contains_key(*text))
            .collect();
        missing.sort_unstable();
        missing.dedup();
        let words = analyze_words(
            session,
            &mut worker.next_input,
            missing.iter().copied(),
            &self.projection,
            normalization,
        )?;
        worker
            .claims
            .extend(missing.into_iter().map(str::to_owned).zip(words));
        let claim_bytes = worker.claims.values().try_fold(0usize, |bytes, words| {
            bytes.checked_add(words.resident_bytes().ok()?)
        });
        if claim_bytes.is_none_or(|bytes| bytes > RESIDENT_BYTES) {
            return Err("sample examples exceed the analyzer output limit".to_owned());
        }
        let lines = worker
            .sample
            .as_ref()
            .map_or(&[][..], |(_, words)| words.as_slice());
        Ok(analyze_live_sample(
            &output,
            pattern,
            &self.schema,
            self.name.clone(),
            lines,
            &|text| worker.claims[text].clone(),
            ClaimBudget {
                claims: retained,
                ..ClaimBudget::REQUEST
            },
        ))
    }
}

impl Plan {
    fn schema(&self) -> &TokenSchema {
        match self {
            #[cfg(unix)]
            Self::External(plan) => &plan.schema,
            Self::Sudachi(plan) => plan.schema(),
        }
    }

    fn descriptor(&self) -> &AnalyzerDescriptor {
        match self {
            #[cfg(unix)]
            Self::External(plan) => &plan.descriptor,
            Self::Sudachi(plan) => plan.descriptor(),
        }
    }

    #[cfg(unix)]
    fn external_session(&self, cancellation: &CancellationFlag) -> Option<ExternalAnalyzerSession> {
        match self {
            #[cfg(unix)]
            Self::External(plan) => Some(plan.session(
                ExecutionLimits {
                    max_sentence_bytes: INPUT_BYTES,
                    max_stage_input_bytes: INPUT_BYTES,
                    max_tokenized_bytes: RESIDENT_BYTES,
                    ..ExecutionLimits::default()
                },
                Arc::new(|_| Ok(())),
                ExternalAdapterCancellation::from_flag(cancellation),
            )),
            Self::Sudachi(_) => None,
        }
    }
}

fn analyze_words<'a>(
    session: &mut dyn AnalyzerSession,
    next_input: &mut u64,
    texts: impl IntoIterator<Item = &'a str>,
    projection: &BoundProjection,
    normalization: NormalizationPolicy,
) -> Result<Vec<BoundTokenSentence>, String> {
    let texts: Vec<_> = texts.into_iter().collect();
    let mut inputs = Vec::new();
    for text in texts.iter().copied().filter(|text| !text.is_empty()) {
        let unit = TextUnit::sentence(InputUnitId::new(*next_input), text, normalization)
            .map_err(|error| format!("{error:?}"))?;
        *next_input = next_input
            .checked_add(1)
            .ok_or("sample input identifier exhausted")?;
        inputs.push(unit);
    }
    let mut outputs = if inputs.is_empty() {
        Vec::new().into_iter()
    } else {
        session
            .analyze_bounded_batch(inputs, projection)
            .map_err(|error| error.to_string())?
            .into_inputs()
            .into_vec()
            .into_iter()
    };
    let mut words = Vec::with_capacity(texts.len());
    for text in texts {
        if text.is_empty() {
            words.push(
                BoundTokenSentence::from_projected_attribute_values(projection, 0, Vec::new())
                    .map_err(|error| error.to_string())?,
            );
            continue;
        }
        let input = outputs.next().ok_or("analyzer omitted a sample input")?;
        let [sentence] = input
            .into_parts()
            .1
            .into_vec()
            .try_into()
            .map_err(|_| "analyzer did not return one sentence per input")?;
        words.push(sentence.into_words());
    }
    let bytes = words.iter().try_fold(0usize, |bytes, sentence| {
        bytes.checked_add(sentence.resident_bytes().ok()?)
    });
    if bytes.is_none_or(|bytes| bytes > RESIDENT_BYTES) {
        return Err("sample exceeds the analyzer output limit".to_owned());
    }
    Ok(words)
}
