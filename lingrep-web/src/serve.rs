//! The lingrep-web server entry point shared by the `lingrep-web` binary and
//! `ling serve`.

use std::num::NonZeroUsize;
use std::sync::Arc;

use indexmap::IndexMap;

use crate::corpus_runtime::CorpusRuntime;
use crate::corpus_set::{CorpusSet, CorpusSlot};
use crate::server_context::ServerContext;
use crate::state::{Config, DetailLimits, RequestStore};
use crate::{Args, app_router_at};

/// Returns `Err` with a message when the configuration is invalid or the
/// listener cannot bind its address; otherwise serves until serving fails,
/// which panics. Must not be called from inside a Tokio runtime.
pub fn run(args: &Args) -> Result<(), String> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();
    let config = config(args)?;
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("the server runtime must be constructible")
        .block_on(serve(args, config))
}

/// The validated server configuration `args` declare.
///
/// # Errors
///
/// A message naming the invalid argument.
pub fn config(args: &Args) -> Result<Config, String> {
    let config = Config {
        dict_path: args.dict.dict_path.display().to_string(),
        corpora: crate::cli::declared_corpora(args)?,
        input_format: args.input.input_format,
        markdown: args.markdown.resolve()?,
        max_record_bytes: args.max_record_bytes,
        detail_limits: DetailLimits {
            max_kwic: args.max_kwic_lines_per_pattern,
        },
    };
    config.validate().map_err(|error| error.to_string())?;
    Ok(config)
}

async fn serve(args: &Args, config: Config) -> Result<(), String> {
    let addr = format!("{}:{}", args.host, args.port);
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .map_err(|error| format!("cannot listen on {addr}: {error}"))?;
    let store = store(args, config).await?;
    let app = app_router_at(store, &args.base_path);

    println!("lingrep-web listening on http://{addr}");
    #[expect(
        clippy::unwrap_used,
        reason = "startup serve failure is a hard error for the lingrep-web entry point"
    )]
    {
        axum::serve(listener, app).await.unwrap();
    }
    Ok(())
}

/// The request state `args` and `config` declare, with the default corpus
/// started and every `--count-table` bound.
///
/// # Errors
///
/// A message naming the count table or corpus that stops startup.
pub async fn store(args: &Args, config: Config) -> Result<RequestStore, String> {
    let max_active_sessions =
        NonZeroUsize::new(args.max_active_sessions).unwrap_or(NonZeroUsize::MIN);
    let context = Arc::new(
        ServerContext::new(&config.dict_path).expect("the shared scan pool must be constructible"),
    );
    let mut slots = IndexMap::new();
    for (name, path, display_name) in &config.corpora {
        let runtime = Arc::new(
            CorpusRuntime::new_with_override(
                Arc::clone(&context),
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
    let corpus_set = Arc::new(CorpusSet::new(slots));
    // Other declared corpora start when a request first selects them.
    corpus_set
        .resolve(None)
        .expect("config.validate() guarantees at least one declared corpus")
        .runtime
        .start_or_observe()
        .await;

    let default_pattern = crate::cli::read_default_pattern(&args.patterns);
    let catalogs = std::sync::Arc::new(crate::catalog_index::CatalogIndex::discover(
        &crate::cli::resolve_pattern_root(args),
    ));
    let frequency = Arc::new(
        crate::frequency::FrequencyViews::bind(
            &args.count_table,
            &config.corpora,
            &corpus_set,
            &catalogs,
        )
        .await?,
    );
    let store =
        RequestStore::with_corpus_set_capacity(config, context, corpus_set, max_active_sessions)
            .with_default_pattern(default_pattern)
            .with_catalogs(catalogs)
            .with_frequency(frequency);
    Ok(store)
}
