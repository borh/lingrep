//! The lingrep-web server entry point shared by the `lingrep-web` binary and
//! `ling serve`.

use std::future::{Future, IntoFuture};
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::Duration;

use indexmap::IndexMap;
use tokio::sync::watch;

use crate::corpus_runtime::CorpusRuntime;
use crate::corpus_set::{CorpusSet, CorpusSlot};
use crate::server_context::ServerContext;
use crate::state::{Config, DetailLimits, RequestStore};
use crate::{Args, app_router_at};

/// Returns `Err` with a message when the configuration is invalid or the
/// listener cannot bind its address; otherwise serves until SIGINT or SIGTERM
/// and returns `Ok` once in-flight requests finish or a bounded grace period
/// ends. A serving failure panics. Must not be called from inside a Tokio runtime.
pub fn run(args: &Args) -> Result<(), String> {
    run_with(args, os_signal_count)
}

/// As [`run`], but stops when `signals`, a count of termination signals the
/// caller has received, is at least 1, including when it already is. It
/// installs no signal handlers of its own.
///
/// # Errors
///
/// As [`run`].
pub fn run_until(args: &Args, signals: watch::Receiver<u32>) -> Result<(), String> {
    run_with(args, move || signals)
}

fn run_with(
    args: &Args,
    signals: impl FnOnce() -> watch::Receiver<u32> + Send,
) -> Result<(), String> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();
    let config = config(args)?;
    block_on(async move { serve(args, config, signals()).await })
}

/// Runs `future` to completion on a new multi-thread runtime, then shuts the
/// runtime down without waiting for blocking tasks still running, which
/// continue until the process exits.
fn block_on<F: Future>(future: F) -> F::Output {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("the server runtime must be constructible");
    let output = runtime.block_on(future);
    runtime.shutdown_background();
    output
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

async fn serve(args: &Args, config: Config, signals: watch::Receiver<u32>) -> Result<(), String> {
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
        serve_until(listener, app, signals, SHUTDOWN_GRACE)
            .await
            .unwrap();
    }
    Ok(())
}

/// How long in-flight requests, including open event streams, may run after a
/// termination signal. It is below systemd's default `TimeoutStopSec` of 90 s,
/// after which systemd sends SIGKILL.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(10);

/// Serves `app` until `signals` is at least 1, then stops accepting
/// connections and returns when in-flight requests have finished, `grace` has
/// elapsed, or `signals` is at least 2, whichever is first.
async fn serve_until(
    listener: tokio::net::TcpListener,
    app: axum::Router,
    signals: watch::Receiver<u32>,
    grace: Duration,
) -> std::io::Result<()> {
    let mut first = signals.clone();
    let server = axum::serve(listener, app)
        .with_graceful_shutdown(async move { signal_number(&mut first, 1).await })
        .into_future();
    tokio::pin!(server);
    let mut signals = signals;
    tokio::select! {
        result = &mut server => return result,
        () = signal_number(&mut signals, 1) => {}
    }
    tracing::info!("termination signal received; finishing in-flight requests");
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

/// Completes once `signals` is at least `number`; never, if the sender is
/// dropped first.
async fn signal_number(signals: &mut watch::Receiver<u32>, number: u32) {
    if signals.wait_for(|&count| count >= number).await.is_err() {
        std::future::pending::<()>().await;
    }
}

/// A count of the SIGINT and, on Unix, SIGTERM signals received from now on.
/// Must be called inside a Tokio runtime.
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
/// started and every `--count-table` bound.
///
/// # Errors
///
/// A message naming the label file, count table or corpus that stops startup.
pub async fn store(args: &Args, config: Config) -> Result<RequestStore, String> {
    let labels = crate::metadata_labels::KeyLabels::read(&args.metadata_labels)?;
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
        .await?
        .with_labels(labels),
    );
    let store =
        RequestStore::with_corpus_set_capacity(config, context, corpus_set, max_active_sessions)
            .with_default_pattern(default_pattern)
            .with_catalogs(catalogs)
            .with_frequency(frequency);
    Ok(store)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use axum::{Router, routing::get};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::sync::{oneshot, watch};

    use super::{block_on, serve_until};

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

    /// Returns a router whose `/slow` answers once `release` fires, and a
    /// receiver that fires when `/slow` has been entered.
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

    /// A blocking task the runtime cannot cancel, such as a corpus build, does
    /// not keep `block_on` from returning.
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
            });
            returned_tx.send(()).unwrap();
        });
        returned_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("block_on returns within 5 s");
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
