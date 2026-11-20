mod query;

use std::path::PathBuf;
use std::sync::Arc;

use lingrep_cli_common::{DictArgs, MISSING_DICTIONARY_HINT};
use lingrep_core::analysis::ValidatedAnalysisBatch;
use lingrep_core::analysis::{
    AnalyzerLanes, AnalyzerPlan, AnalyzerSession, InputUnitId, NormalizationPolicy, TextUnit,
    VibratoPlan,
};
use lingrep_core::dictionary::create_tokenizer_with_identity;
use lingrep_core::execution::DEFAULT_MAX_INPUT_RECORD_BYTES;
use lingrep_core::token_match::{AttributeKey, BoundProjection, TokenSchema};
use pyo3::exceptions::{PyOSError, PyValueError};
use pyo3::prelude::*;
use rayon::{ThreadPool, ThreadPoolBuilder};

#[pyclass(frozen)]
struct Engine {
    plan: VibratoPlan,
    schema: Arc<TokenSchema>,
    pool: ThreadPool,
    #[pyo3(get)]
    dictionary_path: PathBuf,
    #[pyo3(get)]
    dictionary_digest: String,
}

fn invalid(error: impl std::fmt::Display) -> PyErr {
    PyValueError::new_err(error.to_string())
}

impl Engine {
    fn analyze(
        &self,
        texts: Vec<String>,
        projection: &BoundProjection,
    ) -> PyResult<ValidatedAnalysisBatch> {
        if projection.schema_digest() != self.plan.schema().digest() {
            return Err(invalid("query was compiled for a different token schema"));
        }
        if let Some(text) = texts
            .iter()
            .find(|text| text.len() > DEFAULT_MAX_INPUT_RECORD_BYTES)
        {
            return Err(invalid(format!(
                "sentence of {} bytes exceeds the {DEFAULT_MAX_INPUT_RECORD_BYTES}-byte limit",
                text.len()
            )));
        }
        let inputs = texts
            .into_iter()
            .enumerate()
            .map(|(id, text)| {
                TextUnit::sentence(InputUnitId::new(id as u64), text, NormalizationPolicy::None)
            })
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| invalid(format!("{e:?}")))?;
        let mut lanes = AnalyzerLanes::new(&self.plan, self.pool.current_num_threads(), &self.pool);
        lanes
            .analyze_bounded_batch(inputs, projection)
            .map_err(invalid)
    }
}

#[pymethods]
impl Engine {
    #[new]
    fn new(py: Python<'_>, dict_path: Option<PathBuf>, workers: usize) -> PyResult<Self> {
        if workers == 0 {
            return Err(invalid("workers must be positive"));
        }
        py.detach(move || {
            let path = DictArgs { dict_path }
                .resolve()
                .ok_or_else(|| PyOSError::new_err(MISSING_DICTIONARY_HINT))?;
            let (tokenizer, digest) = create_tokenizer_with_identity(
                path.to_str()
                    .ok_or_else(|| invalid("dictionary path must be UTF-8"))?,
            )
            .map_err(|e| PyOSError::new_err(e.to_string()))?;
            let plan = VibratoPlan::configured(
                tokenizer,
                &path,
                digest,
                lingrep_core::analysis::ExtractionProfile::Morphology,
                lingrep_core::analysis::SegmentationMode::Input,
                NormalizationPolicy::None,
            )
            .map_err(invalid)?;
            Ok(Self {
                schema: Arc::new(plan.schema().clone()),
                plan,
                dictionary_path: path.canonicalize()?,
                dictionary_digest: blake3::Hash::from(digest).to_hex().to_string(),
                pool: ThreadPoolBuilder::new()
                    .num_threads(workers)
                    .build()
                    .map_err(invalid)?,
            })
        })
    }

    #[getter]
    fn analysis_identity(&self) -> String {
        blake3::Hash::from(*self.plan.descriptor().semantic_identity().as_bytes())
            .to_hex()
            .to_string()
    }

    #[getter]
    fn schema(&self) -> query::Schema {
        query::Schema(self.schema.clone())
    }

    #[pyo3(signature = (texts, attributes=None))]
    fn sentences(
        &self,
        py: Python<'_>,
        texts: Vec<String>,
        attributes: Option<Vec<String>>,
    ) -> PyResult<Vec<query::Sentence>> {
        py.detach(|| {
            let keys = match attributes {
                Some(keys) => keys
                    .iter()
                    .map(AttributeKey::new)
                    .collect::<Result<_, _>>()
                    .map_err(invalid)?,
                None => self.schema.keys().to_vec(),
            };
            let projection = Arc::new(self.schema.bind_projection(keys).map_err(invalid)?);
            Ok(self
                .analyze(texts, &projection)?
                .into_inputs()
                .into_vec()
                .into_iter()
                .map(|item| query::Sentence::native(item, self.schema.clone(), projection.clone()))
                .collect())
        })
    }

    fn surfaces(&self, py: Python<'_>, texts: Vec<String>) -> PyResult<Vec<Vec<String>>> {
        py.detach(|| {
            let projection = self
                .plan
                .schema()
                .bind_projection(Vec::<AttributeKey>::new())
                .map_err(invalid)?;
            let batch = self.analyze(texts, &projection)?;
            batch
                .inputs()
                .iter()
                .map(|item| {
                    item.sentences()[0]
                        .surface_tokens()
                        .iter()
                        .map(|token| {
                            item.input()
                                .slice(token.source_range())
                                .map(str::to_owned)
                                .map_err(|e| invalid(format!("{e:?}")))
                        })
                        .collect()
                })
                .collect()
        })
    }
}

#[pymodule]
fn _native(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_class::<Engine>()?;
    module.add_class::<query::Schema>()?;
    module.add_class::<query::Sentence>()?;
    module.add_class::<query::Token>()?;
    module.add_class::<query::CompiledQuery>()
}
