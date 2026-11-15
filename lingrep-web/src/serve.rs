//! The lingrep-web server entry point shared by the `lingrep-web` binary and
//! `ling serve`.

use std::future::{Future, IntoFuture};
use std::io::IsTerminal as _;
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::Duration;

use indexmap::IndexMap;
use tokio::sync::watch;

use crate::coordinator::CoordinatorRegistry;
use crate::corpus_runtime::CorpusRuntime;
use crate::corpus_set::{CorpusSet, CorpusSlot};
use crate::server_context::ServerContext;
use crate::state::{Config, DetailLimits, RequestStore};
use crate::{Args, app_router_at};

#[derive(Debug, thiserror::Error)]
pub enum ServeError {
    #[error("{0}")]
    InvalidArguments(String),
    #[error("{0}")]
    Runtime(String),
}

/// Serves until SIGINT or SIGTERM, then waits boundedly for requests, scans and builds.
/// Distinguishes invocation and runtime failures; must not run inside a Tokio runtime.
pub fn run(args: &Args) -> Result<(), ServeError> {
    run_with(args, os_signal_count, None)
}

/// As [`run`], but stops when `signals`, a count of termination signals the
/// caller has received, is at least 1, including when it already is. It
/// installs no signal handlers of its own.
///
/// # Errors
///
/// As [`run`].
pub fn run_until(args: &Args, signals: watch::Receiver<u32>) -> Result<(), ServeError> {
    run_with(args, move || signals, None)
}

/// As [`run_until`], and prints the address of `query`, a
/// [`crate::permalink::workbench_link`], on the line after the listening
/// address, then opens it with the program named by `BROWSER` or the
/// platform's opener.
///
/// # Errors
///
/// As [`run`].
pub fn run_until_opening(
    args: &Args,
    signals: watch::Receiver<u32>,
    query: &str,
) -> Result<(), ServeError> {
    run_with(args, move || signals, Some(query))
}

fn run_with(
    args: &Args,
    signals: impl FnOnce() -> watch::Receiver<u32> + Send,
    query: Option<&str>,
) -> Result<(), ServeError> {
    let no_color = std::env::var_os("NO_COLOR").is_some_and(|value| !value.is_empty());
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_ansi(std::io::stderr().is_terminal() && !no_color)
        .with_writer(std::io::stderr)
        .init();
    let config = config(args)?;
    crate::patterns::open_word_vectors(args.vectors.as_deref()).map_err(ServeError::Runtime)?;
    let context = Arc::new(ServerContext::new(&config.dict_path).map_err(|error| {
        ServeError::Runtime(format!("cannot create server scan pool: {error}"))
    })?);
    block_on(async move {
        let signals = signals();
        let result = serve(args, config, Arc::clone(&context), signals.clone(), query).await;
        stop_corpus_builds(&context, signals).await;
        result
    })
    .map_err(|error| ServeError::Runtime(format!("cannot create server runtime: {error}")))?
    .map_err(ServeError::Runtime)
}

async fn stop_corpus_builds(context: &ServerContext, mut signals: watch::Receiver<u32>) {
    let seen = *signals.borrow();
    if seen >= 2 {
        return;
    }
    tokio::select! {
        stopped = tokio::time::timeout(SHUTDOWN_GRACE, context.stop_builds()) => {
            if stopped.is_err() {
                tracing::warn!("stopping with a corpus build still running");
            }
        }
        () = signal_number(&mut signals, seen.saturating_add(1)) => {}
    }
}

fn block_on<F: Future>(future: F) -> std::io::Result<F::Output> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let output = runtime.block_on(future);
    runtime.shutdown_background();
    Ok(output)
}

/// The validated server configuration `args` declare.
///
/// # Errors
///
/// Invalid declarations or unreadable configured inputs.
pub fn config(args: &Args) -> Result<Config, ServeError> {
    let config = Config {
        dict_path: args
            .dict
            .resolve()
            .ok_or_else(|| {
                ServeError::InvalidArguments(format!(
                    "no dictionary: {}",
                    lingrep_cli_common::MISSING_DICTIONARY_HINT
                ))
            })?
            .display()
            .to_string(),
        corpora: crate::cli::declared_corpora(args).map_err(ServeError::InvalidArguments)?,
        input_format: args.input.input_format,
        markdown: args
            .markdown
            .resolve()
            .map_err(ServeError::InvalidArguments)?,
        max_record_bytes: args.max_record_bytes,
        detail_limits: DetailLimits {
            max_kwic: args.max_kwic_lines_per_pattern,
        },
    };
    config.validate().map_err(|error| match error {
        crate::state::ConfigError::UnreadablePath { .. }
        | crate::state::ConfigError::UnreadableArtifact { .. } => {
            ServeError::Runtime(error.to_string())
        }
        _ => ServeError::InvalidArguments(error.to_string()),
    })?;
    Ok(config)
}

async fn serve(
    args: &Args,
    config: Config,
    context: Arc<ServerContext>,
    signals: watch::Receiver<u32>,
    query: Option<&str>,
) -> Result<(), String> {
    let addr = format!("{}:{}", args.host, args.port);
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .map_err(|error| format!("cannot listen on {addr}: {error}"))?;
    let addr = listener
        .local_addr()
        .map_err(|error| format!("cannot listen on {addr}: {error}"))?;
    let Some(store) = unless_signalled(&signals, store_in(args, config, context)).await else {
        tracing::info!("termination signal received during startup");
        return Ok(());
    };
    let store = store?;
    let coordinators = Arc::clone(&store.coordinators);
    let app = app_router_at(store, &args.base_path);

    println!("lingrep-web listening on http://{addr}");
    if let Some(query) = query {
        let base = args.base_path.as_str().trim_end_matches('/');
        let link = format!("http://{addr}{base}/{query}");
        println!("{link}");
        open_in_browser(&link);
    }
    let result = serve_until(listener, app, signals.clone(), SHUTDOWN_GRACE).await;
    stop_scans(&coordinators, signals).await;
    result.map_err(|error| format!("server at {addr} stopped: {error}"))
}

fn open_in_browser(link: &str) {
    let program = std::env::var_os("BROWSER")
        .filter(|program| !program.is_empty())
        .unwrap_or_else(|| {
            if cfg!(target_os = "macos") {
                "open".into()
            } else {
                "xdg-open".into()
            }
        });
    match std::process::Command::new(&program)
        .arg(link)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(mut child) => {
            std::thread::spawn(move || child.wait());
        }
        Err(error) => eprintln!(
            "cannot start `{}` to open the link: {error}",
            program.to_string_lossy()
        ),
    }
}

async fn stop_scans(coordinators: &CoordinatorRegistry, mut signals: watch::Receiver<u32>) {
    let seen = *signals.borrow();
    if seen >= 2 {
        return;
    }
    tokio::select! {
        () = coordinators.shutdown() => {}
        () = signal_number(&mut signals, seen.saturating_add(1)) => {}
    }
}

const SHUTDOWN_GRACE: Duration = Duration::from_secs(10);

async fn serve_until(
    listener: tokio::net::TcpListener,
    app: axum::Router,
    signals: watch::Receiver<u32>,
    grace: Duration,
) -> std::io::Result<()> {
    let mut first = signals.clone();
    let server = axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            signal_number(&mut first, 1).await;
            tracing::info!("termination signal received; finishing in-flight requests");
        })
        .into_future();
    tokio::pin!(server);
    let mut signals = signals;
    tokio::select! {
        result = &mut server => return result,
        () = signal_number(&mut signals, 1) => {}
    }
    tokio::select! {
        result = &mut server => result,
        () = tokio::time::sleep(grace) => {
            tracing::warn!(?grace, "stopping with requests still in flight");
            Ok(())
        }
        () = signal_number(&mut signals, 2) => {
            tracing::warn!("second termination signal received; stopping with requests still in flight");
            Ok(())
        }
    }
}

async fn unless_signalled<T>(
    signals: &watch::Receiver<u32>,
    work: impl Future<Output = T>,
) -> Option<T> {
    let mut signals = signals.clone();
    tokio::select! {
        biased;
        () = signal_number(&mut signals, 1) => None,
        output = work => Some(output),
    }
}

async fn signal_number(signals: &mut watch::Receiver<u32>, number: u32) {
    if signals.wait_for(|&count| count >= number).await.is_err() {
        std::future::pending::<()>().await;
    }
}

fn os_signal_count() -> watch::Receiver<u32> {
    let (count, signals) = watch::channel(0_u32);
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut interrupt =
            signal(SignalKind::interrupt()).expect("the SIGINT handler must be installable");
        let mut terminate =
            signal(SignalKind::terminate()).expect("the SIGTERM handler must be installable");
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = interrupt.recv() => {}
                    _ = terminate.recv() => {}
                }
                count.send_modify(|count| *count = count.saturating_add(1));
            }
        });
    }
    #[cfg(not(unix))]
    tokio::spawn(async move {
        while tokio::signal::ctrl_c().await.is_ok() {
            count.send_modify(|count| *count = count.saturating_add(1));
        }
    });
    signals
}

/// The request state `args` and `config` declare, with the default corpus
/// started and every `--count-table` and `--reading-sidecar` bound.
///
/// # Errors
///
/// A message naming the label file, count table, reading sidecar or corpus
/// that stops startup.
pub async fn store(args: &Args, config: Config) -> Result<RequestStore, String> {
    let context = Arc::new(
        ServerContext::new(&config.dict_path)
            .map_err(|error| format!("cannot create server scan pool: {error}"))?,
    );
    store_in(args, config, context).await
}

async fn store_in(
    args: &Args,
    config: Config,
    context: Arc<ServerContext>,
) -> Result<RequestStore, String> {
    let labels = crate::metadata_labels::KeyLabels::read(&args.metadata_labels)?;
    let max_active_sessions =
        NonZeroUsize::new(args.max_active_sessions).unwrap_or(NonZeroUsize::MIN);
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
    corpus_set
        .resolve(None)
        .expect("config.validate() guarantees at least one declared corpus")
        .runtime
        .start_or_observe()
        .await;

    let reading_sidecars = Arc::new(crate::document_reader::ReadingSidecars::bind(
        &args.reading_sidecar,
        &config.corpora,
    )?);
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
            &crate::cli::resolve_pattern_root(args),
        )
        .await?
        .with_labels(labels),
    );
    let store =
        RequestStore::with_corpus_set_capacity(config, context, corpus_set, max_active_sessions)
            .with_default_pattern(default_pattern)
            .with_catalogs(catalogs)
            .with_frequency(frequency)
            .with_reading_sidecars(reading_sidecars);
    Ok(store)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use axum::{Router, routing::get};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::sync::{oneshot, watch};

    use super::{block_on, serve_until, unless_signalled};

    async fn request(address: std::net::SocketAddr) -> tokio::task::JoinHandle<String> {
        let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
        stream
            .write_all(b"GET /slow HTTP/1.1\r\nHost: test\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
        tokio::spawn(async move {
            let mut response = String::new();
            stream.read_to_string(&mut response).await.unwrap();
            response
        })
    }

    fn slow_router(release: oneshot::Receiver<()>) -> (Router, oneshot::Receiver<()>) {
        let (entered_tx, entered_rx) = oneshot::channel();
        let slot = std::sync::Mutex::new(Some((entered_tx, release)));
        let slot = std::sync::Arc::new(slot);
        let router = Router::new().route(
            "/slow",
            get(move || {
                let (entered, release) = slot.lock().unwrap().take().unwrap();
                async move {
                    entered.send(()).unwrap();
                    let _ = release.await;
                    "done"
                }
            }),
        );
        (router, entered_rx)
    }

    #[test]
    fn block_on_returns_while_a_blocking_task_is_still_running() {
        let (returned_tx, returned_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            block_on(async {
                tokio::task::spawn_blocking(|| {
                    loop {
                        std::thread::sleep(Duration::from_secs(1));
                    }
                });
            })
            .expect("runtime");
            returned_tx.send(()).unwrap();
        });
        returned_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("block_on returns within 5 s");
    }

    #[tokio::test]
    async fn a_signal_during_startup_work_drops_the_work_and_its_temporary_files() {
        let (signals_tx, signals_rx) = watch::channel(0);
        let (created_tx, created_rx) = oneshot::channel();
        let startup = tokio::spawn(async move {
            unless_signalled(&signals_rx, async move {
                let spool = tempfile::NamedTempFile::new().unwrap();
                created_tx.send(spool.path().to_path_buf()).unwrap();
                std::future::pending::<()>().await;
            })
            .await
        });

        let spool = created_rx.await.unwrap();
        assert!(spool.exists());
        signals_tx.send(1).unwrap();
        let output = tokio::time::timeout(Duration::from_secs(10), startup)
            .await
            .expect("startup returns once the signal arrives")
            .unwrap();
        assert!(output.is_none());
        assert!(!spool.exists());
    }

    #[tokio::test]
    async fn shutdown_lets_an_in_flight_request_finish() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (release_tx, release_rx) = oneshot::channel();
        let (router, entered) = slow_router(release_rx);
        let (signals_tx, signals_rx) = watch::channel(0);
        let server = tokio::spawn(serve_until(
            listener,
            router,
            signals_rx,
            Duration::from_secs(60),
        ));

        let response = request(address).await;
        entered.await.unwrap();
        signals_tx.send(1).unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(
            !server.is_finished(),
            "the server stopped before the request finished"
        );

        release_tx.send(()).unwrap();
        let response = response.await.unwrap();
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        assert!(response.ends_with("done"), "{response}");
        tokio::time::timeout(Duration::from_secs(10), server)
            .await
            .expect("the server returns once the request finished")
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn shutdown_returns_after_the_grace_period_when_a_request_does_not_finish() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (_release_tx, release_rx) = oneshot::channel();
        let (router, entered) = slow_router(release_rx);
        let (signals_tx, signals_rx) = watch::channel(0);
        let server = tokio::spawn(serve_until(
            listener,
            router,
            signals_rx,
            Duration::from_millis(200),
        ));

        let _response = request(address).await;
        entered.await.unwrap();
        signals_tx.send(1).unwrap();
        tokio::time::timeout(Duration::from_secs(10), server)
            .await
            .expect("the server returns once the grace period ends")
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn a_second_signal_returns_before_the_grace_period_ends() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (_release_tx, release_rx) = oneshot::channel();
        let (router, entered) = slow_router(release_rx);
        let (signals_tx, signals_rx) = watch::channel(0);
        let server = tokio::spawn(serve_until(
            listener,
            router,
            signals_rx,
            Duration::from_secs(600),
        ));

        let _response = request(address).await;
        entered.await.unwrap();
        signals_tx.send(1).unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(
            !server.is_finished(),
            "the server stopped before the grace period"
        );
        signals_tx.send(2).unwrap();
        tokio::time::timeout(Duration::from_secs(10), server)
            .await
            .expect("the server returns once the second signal arrives")
            .unwrap()
            .unwrap();
    }
}
