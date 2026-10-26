#[cfg(not(target_env = "msvc"))]
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

use std::num::NonZeroUsize;
use std::sync::Arc;

use clap::Parser;
use indexmap::IndexMap;
use lingrep_web::corpus_runtime::CorpusRuntime;
use lingrep_web::corpus_set::{CorpusSet, CorpusSlot};
use lingrep_web::server_context::ServerContext;
use lingrep_web::state::{Config, DetailLimits, RequestStore};
use lingrep_web::{Args, app_router_at};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();
    let args = Args::parse();

    let corpora = match lingrep_web::cli::declared_corpora(&args) {
        Ok(corpora) => corpora,
        Err(message) => {
            eprintln!("Invalid lingrep-web configuration: {message}");
            std::process::exit(2);
        }
    };

    let config = Config {
        dict_path: args.dict.dict_path.display().to_string(),
        corpora,
        input_format: args.input.input_format,
        markdown: match args.markdown.resolve() {
            Ok(markdown) => markdown,
            Err(message) => {
                eprintln!("Invalid lingrep-web configuration: {message}");
                std::process::exit(2);
            }
        },
        max_record_bytes: args.max_record_bytes,
        detail_limits: DetailLimits {
            max_kwic: args.max_kwic_lines_per_pattern,
        },
    };
    if let Err(error) = config.validate() {
        eprintln!("Invalid lingrep-web configuration: {error}");
        std::process::exit(2);
    }
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
    // Only the default slot is started eagerly; every other declared corpus
    // builds lazily on first selection (wired up in a later task).
    corpus_set
        .resolve(None)
        .expect("config.validate() guarantees at least one declared corpus")
        .runtime
        .start_or_observe()
        .await;

    let default_pattern = lingrep_web::cli::read_default_pattern(&args.patterns);
    let catalogs = std::sync::Arc::new(lingrep_web::catalog_index::CatalogIndex::discover(
        &lingrep_web::cli::resolve_pattern_root(&args),
    ));
    let store =
        RequestStore::with_corpus_set_capacity(config, context, corpus_set, max_active_sessions)
            .with_default_pattern(default_pattern)
            .with_catalogs(catalogs);

    let app = app_router_at(store, &args.base_path);

    let addr = format!("{}:{}", args.host, args.port);
    #[expect(
        clippy::unwrap_used,
        reason = "startup bind failure is a hard error for the lingrep-web entry point"
    )]
    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
    println!("lingrep-web listening on http://{addr}");
    #[expect(
        clippy::unwrap_used,
        reason = "startup serve failure is a hard error for the lingrep-web entry point"
    )]
    {
        axum::serve(listener, app).await.unwrap();
    }
}
