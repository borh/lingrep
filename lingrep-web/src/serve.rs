//! The lingrep-web server entry point shared by the `lingrep-web` binary and
//! `ling serve`.

use std::future::{Future, IntoFuture};
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::Duration;

use indexmap::IndexMap;

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

    let shutdown = termination_signal();
    println!("lingrep-web listening on http://{addr}");
    #[expect(
        clippy::unwrap_used,
        reason = "startup serve failure is a hard error for the lingrep-web entry point"
    )]
    {
        serve_until(listener, app, shutdown, SHUTDOWN_GRACE)
            .await
            .unwrap();
    }
    Ok(())
}

/// How long in-flight requests, including open event streams, may run after a
/// termination signal. It is below systemd's default `TimeoutStopSec` of 90 s,
/// after which systemd sends SIGKILL.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(10);

/// Serves `app` until `shutdown` completes, then stops accepting connections
/// and returns when in-flight requests have finished or `grace` has elapsed,
/// whichever is first.
async fn serve_until(
    listener: tokio::net::TcpListener,
    app: axum::Router,
    shutdown: impl Future<Output = ()> + Send + 'static,
    grace: Duration,
) -> std::io::Result<()> {
    let (signalled_tx, signalled_rx) = tokio::sync::oneshot::channel();
    let server = axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            shutdown.await;
            let _ = signalled_tx.send(());
        })
        .into_future();
    tokio::pin!(server);
    tokio::select! {
        result = &mut server => result,
        Ok(()) = signalled_rx => tokio::time::timeout(grace, &mut server)
            .await
            .unwrap_or_else(|_| {
                tracing::warn!(?grace, "stopping with requests still in flight");
                Ok(())
            }),
    }
}

/// A future that completes on SIGINT or, on Unix, SIGTERM. The handlers are
/// registered when this is called, not when the future is first polled: under
/// `ling serve`, a signal that arrives before then reaches only `ling`'s own
/// handlers and is lost.
fn termination_signal() -> impl Future<Output = ()> + Send + 'static {
    #[cfg(unix)]
    let signalled = {
        use tokio::signal::unix::{SignalKind, signal};
        let mut interrupt =
            signal(SignalKind::interrupt()).expect("the SIGINT handler must be installable");
        let mut terminate =
            signal(SignalKind::terminate()).expect("the SIGTERM handler must be installable");
        async move {
            tokio::select! {
                _ = interrupt.recv() => {}
                _ = terminate.recv() => {}
            }
        }
    };
    #[cfg(not(unix))]
    let signalled = async {
        tokio::signal::ctrl_c()
            .await
            .expect("the Ctrl-C handler must be installable");
    };
    async move {
        signalled.await;
        tracing::info!("termination signal received; finishing in-flight requests");
    }
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

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use axum::{Router, routing::get};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::sync::oneshot;

    use super::serve_until;

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

    #[tokio::test]
    async fn shutdown_lets_an_in_flight_request_finish() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (release_tx, release_rx) = oneshot::channel();
        let (router, entered) = slow_router(release_rx);
        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        let server = tokio::spawn(serve_until(
            listener,
            router,
            async move {
                let _ = shutdown_rx.await;
            },
            Duration::from_secs(60),
        ));

        let response = request(address).await;
        entered.await.unwrap();
        shutdown_tx.send(()).unwrap();
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
        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        let server = tokio::spawn(serve_until(
            listener,
            router,
            async move {
                let _ = shutdown_rx.await;
            },
            Duration::from_millis(200),
        ));

        let _response = request(address).await;
        entered.await.unwrap();
        shutdown_tx.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(10), server)
            .await
            .expect("the server returns once the grace period ends")
            .unwrap()
            .unwrap();
    }
}
