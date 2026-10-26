use std::fs::File;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use indexmap::IndexMap;
use thiserror::Error;

use crate::coordinator::CoordinatorRegistry;
use crate::corpus_runtime::CorpusRuntime;
use crate::corpus_set::{CorpusName, CorpusSet, CorpusSlot};
use crate::server_context::ServerContext;
use lingrep_core::corpus::CorpusSource;

pub use crate::server_context::TokenizerUnavailable;

#[derive(Debug, Clone)]
pub struct Config {
    pub dict_path: String,
    /// Declared corpora, in declaration order, `--input` first. Each entry is
    /// `(key, path, display_name)` — see `cli::declared_corpora`. All declared
    /// corpora share `input_format`, `markdown` and `max_record_bytes` below.
    pub corpora: Vec<(CorpusName, PathBuf, String)>,
    pub input_format: Option<lingrep_core::corpus::CorpusFormat>,
    pub markdown: lingrep_core::corpus::MarkdownReaderConfig,
    /// Bound on one physical input record, applied while the corpus artifact is
    /// staged. Independent of how execution batches raw bytes: a JSONL document
    /// larger than one batch is ordinary.
    pub max_record_bytes: usize,
    pub detail_limits: DetailLimits,
}

#[derive(Debug, Clone, Copy)]
pub struct DetailLimits {
    /// The ceiling on KWIC lines retained per pattern, not the only value a
    /// run may use. A request asks for its own retention and is clamped to
    /// this; see [`retention_offers`].
    pub max_kwic: usize,
}

/// The retention values the workbench offers, in ascending order.
///
/// The configured ceiling is what an operator sized the server for, so it is
/// both the largest offer and the default. The rungs below it exist because a
/// reader skimming counts and a reader working through a concordance want
/// different numbers, and changing the deployed unit's arguments is not a user
/// interface. A ladder rung equal to the ceiling is not offered twice.
pub fn retention_offers(ceiling: usize) -> Vec<usize> {
    const LADDER: [usize; 6] = [20, 50, 100, 250, 500, 1_000];
    let mut offers: Vec<usize> = LADDER.into_iter().filter(|rung| *rung < ceiling).collect();
    offers.push(ceiling.max(1));
    offers
}

/// The offer a shared link's retention selects on this server: the largest
/// offer not above the link's value, or the smallest offer when every offer is
/// above it. `None` selects the ceiling, as a page opened without a link does.
pub fn link_retention(offers: &[usize], requested: Option<usize>) -> usize {
    let ceiling = offers.last().copied().unwrap_or(1);
    let Some(requested) = requested else {
        return ceiling;
    };
    offers
        .iter()
        .rev()
        .find(|offer| **offer <= requested)
        .or(offers.first())
        .copied()
        .unwrap_or(ceiling)
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("dictionary path must not be empty")]
    EmptyDictionaryPath,
    #[error("no corpus is declared; --input is required")]
    EmptyCorpusPath,
    #[error("corpus {name:?} has an empty path")]
    EmptyDeclaredCorpusPath { name: String },
    #[error("{kind} path is not a readable file: {path}")]
    UnreadablePath { kind: String, path: String },
    /// A declared path looked like a prepared artifact — a directory carrying a
    /// `manifest.json` — but would not open as one. Carries the artifact
    /// reader's own reason, because "not a readable file" says nothing an
    /// operator can act on about a directory that plainly exists.
    #[error("corpus {name:?} is a prepared artifact that could not be opened: {path}: {cause}")]
    UnreadableArtifact {
        name: String,
        path: String,
        cause: String,
    },
    #[error("detail limits must be greater than zero")]
    ZeroDetailLimit,
    #[error(transparent)]
    InvalidLimit(#[from] lingrep_core::execution::ExecutionError),
}

impl Config {
    pub fn validate(&self) -> Result<(), ConfigError> {
        validate_readable_file(
            &self.dict_path,
            "dictionary".to_owned(),
            ConfigError::EmptyDictionaryPath,
        )?;
        if self.corpora.is_empty() {
            return Err(ConfigError::EmptyCorpusPath);
        }
        for (name, path, _display_name) in &self.corpora {
            let path_display = path.display().to_string();
            if path_display.trim().is_empty() {
                // Named by the declared key: unlike the "nothing declared at
                // all" case above, one *was* declared, just with an empty path.
                return Err(ConfigError::EmptyDeclaredCorpusPath {
                    name: name.as_str().to_owned(),
                });
            }
            // A prepared artifact is producer-free: it is read directly and no
            // reader configuration applies to it, so it is admitted (or
            // refused) before any of the source-reading checks below.
            if lingrep_corpus::is_prepared_artifact(path) {
                lingrep_corpus::CorpusArtifact::open(path).map_err(|error| {
                    ConfigError::UnreadableArtifact {
                        name: name.as_str().to_owned(),
                        path: path_display.clone(),
                        cause: error.to_string(),
                    }
                })?;
                continue;
            }
            let read_config =
                crate::corpus_runtime::read_config(path, self.input_format, self.markdown.clone());
            if read_config.format() == lingrep_core::corpus::CorpusFormat::Bccwj {
                // BCCWJ is a ling corpus format; lingrep-web streams
                // Markdown/JSONL/plain sources and artifacts only. Named by
                // the declared key, like the error below.
                return Err(ConfigError::UnreadablePath {
                    kind: format!("bccwj corpus {:?}", name.as_str()),
                    path: path_display,
                });
            }
            if matches!(
                read_config.format(),
                lingrep_core::corpus::CorpusFormat::Tei
                    | lingrep_core::corpus::CorpusFormat::SoranohaTei
            ) {
                return Err(ConfigError::UnreadablePath {
                    kind: format!(
                        "TEI corpus {:?}; prepare it with ling corpus build",
                        name.as_str()
                    ),
                    path: path_display,
                });
            }
            if read_config.format() == lingrep_core::corpus::CorpusFormat::Soranoha {
                // A Soranoha archive is one ZIP of works; lingrep-web has no
                // reader for it and serves the producer-free artifact instead.
                return Err(ConfigError::UnreadablePath {
                    kind: format!(
                        "soranoha archive {:?}; prepare it with ling corpus build",
                        name.as_str()
                    ),
                    path: path_display,
                });
            }
            if read_config.format() == lingrep_core::corpus::CorpusFormat::Conllu {
                // Raw CoNLL-U owns its token analysis. The web preparation
                // worker currently owns a configured analyzer, so accepting
                // this source here would silently analyze it again. A
                // ling-built artifact is producer-free and was admitted
                // above.
                return Err(ConfigError::UnreadablePath {
                    kind: format!(
                        "raw conllu corpus {:?}; prepare it with ling corpus build",
                        name.as_str()
                    ),
                    path: path_display,
                });
            }
            // Named by the declared key, not just the path, so an operator with
            // several `--corpus` entries knows which one is wrong.
            CorpusSource::discover(path, read_config).map_err(|_| ConfigError::UnreadablePath {
                kind: format!("corpus {:?}", name.as_str()),
                path: path_display,
            })?;
        }
        if self.detail_limits.max_kwic == 0 {
            return Err(ConfigError::ZeroDetailLimit);
        }
        // The record bound is measured against the same sentence bound the
        // server will execute under, so the rule relating them is enforced by
        // the type that owns it rather than restated here.
        self.execution_limits()
            .validate()
            .map_err(ConfigError::InvalidLimit)?;
        Ok(())
    }

    /// The bounds this server reads and executes under.
    #[must_use]
    pub fn execution_limits(&self) -> lingrep_core::execution::ExecutionLimits {
        lingrep_core::execution::ExecutionLimits {
            max_input_record_bytes: self.max_record_bytes,
            ..Default::default()
        }
    }
}

fn validate_readable_file(
    path: &str,
    kind: String,
    empty_error: ConfigError,
) -> Result<(), ConfigError> {
    if path.trim().is_empty() {
        return Err(empty_error);
    }
    let path_ref = Path::new(path);
    if !path_ref.is_file() || File::open(path_ref).is_err() {
        return Err(ConfigError::UnreadablePath {
            kind,
            path: path.to_owned(),
        });
    }
    Ok(())
}

#[derive(Clone)]
pub struct RequestStore {
    pub config: Config,
    /// Every declared corpus. The first entry is the default (`--input`).
    pub corpora: Arc<CorpusSet>,
    pub coordinators: Arc<CoordinatorRegistry>,
    /// The resources shared across every corpus this server serves — the scan
    /// pool, dictionary bytes, build permit, and the sample-analysis
    /// tokenizer. Each slot's runtime holds the same context internally; this
    /// field gives handlers direct access without threading it through.
    pub context: Arc<ServerContext>,
    /// Seed text for the index-page pattern editor (the `--patterns` grammar
    /// file contents). Empty unless set via [`RequestStore::with_default_pattern`];
    /// only the index handler reads it.
    pub default_pattern: Arc<str>,
    /// Catalogs discovered at startup and offered in the UI's catalog list.
    /// Empty unless set via [`RequestStore::with_catalogs`]; only the index
    /// handler and the catalog routes read it.
    pub catalogs: std::sync::Arc<crate::catalog_index::CatalogIndex>,
}

impl std::fmt::Debug for RequestStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RequestStore")
            .field("config", &self.config)
            .field("coordinators", &self.coordinators)
            .field("default_pattern", &self.default_pattern)
            .field("catalogs", &self.catalogs)
            .field("context", &self.context)
            .finish_non_exhaustive()
    }
}

/// Build a runtime for every entry in `config.corpora` against the shared
/// `context`, wrap each in a slot, and collect the declaration-ordered set.
///
/// # Panics
/// If `config.corpora` is empty; `Config::validate` rejects that shape before
/// a caller reaches this, and the test-only callers below always supply at
/// least one entry.
fn build_corpus_set(config: &Config, context: &Arc<ServerContext>) -> CorpusSet {
    let mut slots = IndexMap::new();
    for (name, path, display_name) in &config.corpora {
        let runtime = Arc::new(
            CorpusRuntime::new_with_override(
                Arc::clone(context),
                path,
                config.input_format,
                config.markdown.clone(),
                config.max_record_bytes,
            )
            .expect("the fixed Rayon corpus pool must be constructible"),
        );
        slots.insert(
            name.clone(),
            CorpusSlot::new(name.clone(), runtime, display_name.clone()),
        );
    }
    CorpusSet::new(slots)
}

/// Wrap an already-built runtime as the sole, default-keyed corpus. Used by
/// [`RequestStore::with_runtime`] and [`RequestStore::with_runtime_capacity`],
/// whose many test callers build a `CorpusRuntime` directly (often over a
/// hand-built `from_ready` cache) rather than going through `declared_corpora`.
/// The key and display name come from `config.corpora`'s first entry when
/// present, so a caller that cares about the displayed name can still set it.
fn single_slot_corpus_set(config: &Config, corpus_runtime: Arc<CorpusRuntime>) -> CorpusSet {
    let (name, display_name) = config.corpora.first().map_or_else(
        || (CorpusName::default_key(), String::new()),
        |(name, _path, display_name)| (name.clone(), display_name.clone()),
    );
    let mut slots = IndexMap::new();
    slots.insert(
        name.clone(),
        CorpusSlot::new(name, corpus_runtime, display_name),
    );
    CorpusSet::new(slots)
}

impl RequestStore {
    pub fn new(config: Config) -> Self {
        let context = Arc::new(
            ServerContext::new(&config.dict_path)
                .expect("the shared scan pool must be constructible"),
        );
        let corpora = Arc::new(build_corpus_set(&config, &context));
        let coordinators = Arc::new(CoordinatorRegistry::new(config.detail_limits.max_kwic));
        Self {
            config,
            corpora,
            coordinators,
            default_pattern: Arc::from(""),
            catalogs: Arc::new(crate::catalog_index::CatalogIndex::default()),
            context,
        }
    }

    pub fn with_runtime(config: Config, corpus_runtime: Arc<CorpusRuntime>) -> Self {
        let context = corpus_runtime.context();
        let corpora = Arc::new(single_slot_corpus_set(&config, corpus_runtime));
        let coordinators = Arc::new(CoordinatorRegistry::new(config.detail_limits.max_kwic));
        Self {
            config,
            corpora,
            coordinators,
            default_pattern: Arc::from(""),
            catalogs: Arc::new(crate::catalog_index::CatalogIndex::default()),
            context,
        }
    }

    /// Like [`RequestStore::with_runtime`] but bounds the coordinator registry to
    /// `max_entries` concurrently retained workbenches. Tests keep using
    /// `with_runtime`; `main.rs` uses [`RequestStore::with_corpus_set_capacity`]
    /// directly, since it declares several corpora up front.
    pub fn with_runtime_capacity(
        config: Config,
        corpus_runtime: Arc<CorpusRuntime>,
        max_entries: NonZeroUsize,
    ) -> Self {
        let context = corpus_runtime.context();
        let corpora = Arc::new(single_slot_corpus_set(&config, corpus_runtime));
        Self::with_corpus_set_capacity(config, context, corpora, max_entries)
    }

    /// Builds the store over an already-assembled `CorpusSet`. `main.rs` calls
    /// this after constructing every declared corpus's runtime itself and
    /// starting the default slot alone, so the plural declaration is visible at
    /// the call site rather than hidden behind a config-driven loop.
    #[must_use]
    pub fn with_corpus_set_capacity(
        config: Config,
        context: Arc<ServerContext>,
        corpora: Arc<CorpusSet>,
        max_entries: NonZeroUsize,
    ) -> Self {
        let coordinators = Arc::new(CoordinatorRegistry::with_max_entries(
            config.detail_limits.max_kwic,
            max_entries,
        ));
        Self {
            config,
            corpora,
            coordinators,
            default_pattern: Arc::from(""),
            catalogs: Arc::new(crate::catalog_index::CatalogIndex::default()),
            context,
        }
    }

    /// Seeds the index-page pattern editor with `default_pattern` (the
    /// `--patterns` grammar file contents). A builder so the many test
    /// constructors keep an empty seed without change; `main.rs` calls it once.
    #[must_use]
    pub fn with_default_pattern(mut self, default_pattern: impl Into<Arc<str>>) -> Self {
        self.default_pattern = default_pattern.into();
        self
    }

    /// Attaches the startup-discovered catalog index. A builder so the many test
    /// constructors keep an empty index without change; `main.rs` calls it once.
    #[must_use]
    pub fn with_catalogs(
        mut self,
        catalogs: std::sync::Arc<crate::catalog_index::CatalogIndex>,
    ) -> Self {
        self.catalogs = catalogs;
        self
    }
}

impl Default for RequestStore {
    fn default() -> Self {
        Self::new(Config {
            dict_path: String::new(),
            corpora: vec![(CorpusName::default_key(), PathBuf::new(), String::new())],
            input_format: None,
            markdown: lingrep_core::corpus::MarkdownReaderConfig::new(
                lingrep_core::corpus::SectionPolicy::default(),
            ),
            max_record_bytes: lingrep_core::execution::DEFAULT_MAX_INPUT_RECORD_BYTES,
            detail_limits: DetailLimits { max_kwic: 1_000 },
        })
    }
}

#[cfg(test)]
mod retention_tests {
    use super::{link_retention, retention_offers};

    #[test]
    fn a_link_selects_the_largest_offer_not_above_its_retention() {
        let offers = retention_offers(1_000);
        assert_eq!(link_retention(&offers, None), 1_000);
        assert_eq!(link_retention(&offers, Some(50)), 50);
        assert_eq!(link_retention(&offers, Some(60)), 50);
        assert_eq!(link_retention(&offers, Some(2_000)), 1_000);
        assert_eq!(link_retention(&offers, Some(5)), 20);
    }

    #[test]
    fn the_ceiling_is_the_largest_offer_and_is_never_duplicated() {
        assert_eq!(retention_offers(1_000), vec![20, 50, 100, 250, 500, 1_000]);
        assert_eq!(retention_offers(250), vec![20, 50, 100, 250]);
        assert_eq!(
            retention_offers(2_000),
            vec![20, 50, 100, 250, 500, 1_000, 2_000]
        );
    }

    #[test]
    fn a_ceiling_below_the_first_rung_offers_only_itself() {
        // A server started with `--max-kwic-lines-per-pattern 5` offers 5, not
        // a rung it would silently clamp back down to.
        assert_eq!(retention_offers(5), vec![5]);
        assert_eq!(retention_offers(1), vec![1]);
    }
}
