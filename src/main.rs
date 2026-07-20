//! Thin CLI wrapper around the [`memory_mcp`] library crate.

use std::{path::PathBuf, sync::Arc};

use anyhow::Context;
use clap::{Args, Parser, Subcommand, ValueEnum};
use mcp_session::BoundedSessionManagerBuilder;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use tokio_util::sync::CancellationToken;
use tracing::Instrument;
use tracing::{info, warn};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

// The SdkTracerProvider type is used in the otlp feature only.
#[cfg(feature = "otlp")]
use opentelemetry_sdk::trace::SdkTracerProvider as OtlpProvider;

use memory_mcp::auth::{self, AuthProvider, StoreBackend};
use memory_mcp::embedding::{CandleEmbeddingEngine, EmbeddingBackend, MODEL_ID};
use memory_mcp::health::{healthz_handler, readyz_handler, version_handler, HealthRegistry};
use memory_mcp::index::{UsearchStore, VectorStore};
use memory_mcp::recall_log::RecallLog;
use memory_mcp::repo::MemoryRepo;
use memory_mcp::server::MemoryServer;
use memory_mcp::types::{validate_branch_name, AppState};

// ---------------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------------

#[derive(Parser)]
#[command(
    name = "memory-mcp",
    about = "Semantic memory MCP server for AI agents",
    version
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Run the MCP server (default)
    Serve(ServeArgs),
    /// Manage authentication
    Auth(AuthCommand),
    /// Pre-warm the embedding model cache (useful as a k8s init container)
    Warmup(WarmupArgs),
    /// Show recall precision statistics by distance bucket
    RecallStats(RecallStatsArgs),
}

#[derive(Args)]
struct AuthCommand {
    #[command(subcommand)]
    action: AuthAction,
}

#[derive(Subcommand)]
enum AuthAction {
    /// Authenticate with GitHub via device flow
    Login(LoginArgs),
    /// Show current auth status
    Status,
}

#[derive(Args)]
struct LoginArgs {
    /// Where to store the token
    #[arg(long, value_enum)]
    store: Option<StoreBackend>,

    /// Kubernetes namespace for the token Secret.
    #[cfg(feature = "k8s")]
    #[arg(long, default_value = "memory-mcp")]
    k8s_namespace: String,

    /// Name of the Kubernetes Secret to create/update.
    #[cfg(feature = "k8s")]
    #[arg(long, default_value = "memory-mcp-github-token")]
    k8s_secret_name: String,
}

/// MCP transport for the `serve` command.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum Transport {
    /// Streamable HTTP server on `--bind` (default). Shared daemon; serves
    /// many clients and networked deployments (ADR-0001).
    Http,
    /// JSON-RPC over stdin/stdout. One process per client, lifecycle managed
    /// by the MCP client; for single-user local use (ADR-0040).
    Stdio,
}

#[derive(Args)]
struct ServeArgs {
    /// MCP transport. `http` serves Streamable HTTP on --bind; `stdio`
    /// serves a single client over stdin/stdout. HTTP-only flags (--bind,
    /// --mcp-path, session limits, --allowed-host) are ignored under stdio.
    #[arg(long, value_enum, default_value = "http", env = "MEMORY_MCP_TRANSPORT")]
    transport: Transport,

    /// Address to bind the HTTP server to.
    #[arg(long, default_value = "127.0.0.1:8080", env = "MEMORY_MCP_BIND")]
    bind: String,

    /// Path to the git-backed memory repository.
    #[arg(long, default_value = "~/.memory-mcp", env = "MEMORY_MCP_REPO_PATH")]
    repo_path: String,

    /// Path to the TOML config file for per-scope remote mapping.
    /// Defaults to `~/.config/memory-mcp/config.toml`. Set to an empty
    /// string to disable config loading.
    #[arg(long, env = "MEMORY_MCP_CONFIG")]
    config: Option<String>,

    /// URL path at which the MCP service is mounted.
    #[arg(long, default_value = "/mcp", env = "MEMORY_MCP_PATH")]
    mcp_path: String,

    /// Remote URL for the git origin. If set, the origin remote is created or
    /// updated on startup. Omit to run in local-only mode (no push/pull).
    #[arg(long, env = "MEMORY_MCP_REMOTE_URL")]
    remote_url: Option<String>,

    /// Branch name used for push/pull operations.
    #[arg(long, default_value = "main", env = "MEMORY_MCP_BRANCH")]
    branch: String,

    /// Maximum number of concurrent MCP sessions. Oldest session is evicted
    /// when the limit is reached. Must be at least 1.
    #[arg(
        long,
        default_value_t = 100,
        env = "MEMORY_MCP_MAX_SESSIONS",
        value_parser = parse_nonzero_usize
    )]
    max_sessions: usize,

    /// Maximum number of new sessions allowed within the rate-limit window.
    /// Set to 0 to disable rate limiting.
    #[arg(long, default_value_t = 10, env = "MEMORY_MCP_SESSION_RATE_LIMIT")]
    session_rate_limit: usize,

    /// Duration of the session creation rate-limit window, in seconds.
    /// Set to 0 to disable rate limiting (treated the same as setting
    /// `--session-rate-limit 0`).
    #[arg(
        long,
        default_value_t = 60,
        env = "MEMORY_MCP_SESSION_RATE_WINDOW_SECS"
    )]
    session_rate_window_secs: u64,

    /// Idle timeout for MCP sessions, in seconds. Sessions are closed after
    /// this duration of inactivity. Set to 0 to disable (not recommended).
    #[arg(long, default_value_t = 14400, env = "MEMORY_MCP_IDLE_TIMEOUT_SECS")]
    idle_timeout_secs: u64,

    /// Maximum session lifetime in seconds, regardless of activity. Sessions
    /// are closed after this duration even if actively used. Set to 0 to
    /// disable (default).
    #[arg(
        long,
        default_value_t = 0,
        env = "MEMORY_MCP_MAX_SESSION_LIFETIME_SECS"
    )]
    max_session_lifetime_secs: u64,

    /// Additional hostname to accept in the HTTP Host header. Required when
    /// the server is accessed via a reverse proxy or gateway (e.g.
    /// `memory-mcp.svc.echoes`). Can be specified multiple times.
    #[arg(long, env = "MEMORY_MCP_ALLOWED_HOST")]
    allowed_host: Vec<String>,

    /// Include remote sync health in readiness checks. When enabled, push/pull
    /// failures will cause /readyz to return 503.
    #[arg(long, default_value_t = false, env = "MEMORY_MCP_REQUIRE_REMOTE_SYNC")]
    require_remote_sync: bool,

    /// SQLite busy timeout in seconds for the recall event log.
    ///
    /// When multiple processes access the recall log concurrently, this
    /// controls how long each connection waits for a database lock before
    /// returning an error.
    #[arg(long, default_value_t = 5, env = "MEMORY_MCP_RECALL_LOG_BUSY_TIMEOUT")]
    recall_log_busy_timeout: u64,

    /// Seconds after which a subsystem with no successful operations is considered
    /// stale. Set to 0 to disable staleness detection (default).
    #[arg(long, default_value_t = 0, env = "MEMORY_MCP_HEALTH_STALE_SECS")]
    health_stale_secs: u64,

    #[command(flatten)]
    embed: EmbedArgs,

    /// Enable OTLP span export. Startup fails fast if the collector is not
    /// reachable (TCP probe, 5s total); outages after startup are handled
    /// by the batch exporter as usual. Use --otlp-optional for graceful
    /// fallback.
    #[cfg(feature = "otlp")]
    #[arg(long, default_value_t = false, env = "MEMORY_MCP_OTLP_REQUIRED")]
    otlp_required: bool,

    /// Enable OTLP span export with graceful fallback: if the collector is
    /// unreachable, log a warning and continue with fmt-only tracing.
    #[cfg(feature = "otlp")]
    #[arg(long, default_value_t = false, env = "MEMORY_MCP_OTLP_OPTIONAL")]
    otlp_optional: bool,
}

#[derive(Args)]
struct WarmupArgs {
    #[command(flatten)]
    embed: EmbedArgs,
}

#[derive(Args)]
struct RecallStatsArgs {
    /// Path to the recall log database.
    #[arg(long, env = "MEMORY_MCP_RECALL_LOG")]
    recall_log: Option<String>,

    /// Path to the index directory (default: ~/.memory-mcp/.memory-mcp-index).
    #[arg(long)]
    index_dir: Option<String>,
}

#[derive(Args)]
struct EmbedArgs {
    /// Maximum seconds a single embedding call may block. After a timeout the
    /// caller receives an error but the worker recovers automatically.
    #[arg(
        long,
        default_value_t = 30,
        env = "MEMORY_MCP_EMBED_TIMEOUT_SECS",
        value_parser = parse_nonzero_u64,
    )]
    embed_timeout_secs: u64,

    /// Maximum number of embedding requests that can queue behind the worker.
    /// Higher values allow more concurrent callers to wait; lower values fail
    /// fast under load.
    #[arg(
        long,
        default_value_t = 64,
        env = "MEMORY_MCP_EMBED_QUEUE_SIZE",
        value_parser = parse_nonzero_usize,
    )]
    embed_queue_size: usize,
}

// ---------------------------------------------------------------------------
// Tracing initialisation
// ---------------------------------------------------------------------------

/// Initialise the global tracing subscriber.
///
/// Default build: Registry + EnvFilter (`memory_mcp=info` default) + fmt(stderr).
/// `otlp` feature: same + OpenTelemetry layer with BatchSpanProcessor.
///
/// Returns the OTLP tracer provider when the `otlp` feature is enabled, so the
/// caller can call `provider.shutdown()` after the server exits.
#[cfg(not(feature = "otlp"))]
fn init_tracing() {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "memory_mcp=info,warn".to_string().into()),
        )
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
        .init();
}

/// Initialise fmt-only tracing (no OTLP). Used for non-serve commands
/// (`warmup`, `auth`) when the `otlp` feature is enabled but OTLP is only
/// meaningful for the long-running server process.
#[cfg(feature = "otlp")]
fn init_tracing_fmt_only() {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "memory_mcp=info,warn".to_string().into()),
        )
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
        .init();
}

/// Total timeout for the startup OTLP reachability probe. This bounds the
/// entire probe — DNS resolution plus every connect attempt combined — not
/// each address individually.
#[cfg(feature = "otlp")]
const OTLP_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Default OTLP gRPC endpoint per the OpenTelemetry specification. This is
/// the only place the OTLP default port 4317 enters the probe: an explicitly
/// configured endpoint that omits a port gets its scheme default (80/443),
/// exactly as the tonic exporter resolves it.
#[cfg(feature = "otlp")]
const OTLP_DEFAULT_ENDPOINT: &str = "http://localhost:4317";

/// Resolve the OTLP endpoint the tonic exporter will target, mirroring its
/// environment lookup: `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT` takes precedence
/// over `OTEL_EXPORTER_OTLP_ENDPOINT`; both fall back to the spec default.
/// Empty values are treated as unset.
///
/// Returns the endpoint plus the name of the environment variable that
/// supplied it (or `"default"`), so error paths can name the source without
/// echoing the — possibly credential-bearing — value itself.
#[cfg(feature = "otlp")]
fn resolve_otlp_endpoint(
    traces_endpoint: Option<&str>,
    general_endpoint: Option<&str>,
) -> (String, &'static str) {
    if let Some(e) = traces_endpoint.filter(|s| !s.trim().is_empty()) {
        return (e.to_owned(), "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT");
    }
    if let Some(e) = general_endpoint.filter(|s| !s.trim().is_empty()) {
        return (e.to_owned(), "OTEL_EXPORTER_OTLP_ENDPOINT");
    }
    (OTLP_DEFAULT_ENDPOINT.to_owned(), "default")
}

/// Parsed probe target: connect coordinates plus a credential-free display
/// form (`scheme://host:port`) that is safe to include in error messages.
#[cfg(feature = "otlp")]
#[derive(Debug)]
struct OtlpProbeTarget {
    /// Hostname or IP literal, without IPv6 brackets.
    host: String,
    /// Port — explicit, or the scheme default (80/443).
    port: u16,
    /// Sanitized `scheme://host:port` for error messages. Never contains the
    /// raw endpoint's path, query string, or userinfo.
    display: String,
}

/// Parse an OTLP endpoint into a probe target using [`http::Uri`] — the same
/// URI type the tonic exporter parses the endpoint with.
///
/// Invariant: the probe must agree with what the exporter will do with the
/// same endpoint string. tonic requires a schemed HTTP(S) URI, so schemeless
/// endpoints are rejected here instead of probed on guessed semantics, and a
/// missing port defaults to the scheme port (80 for http, 443 for https) —
/// never to 4317, which applies only through the spec-default endpoint
/// [`OTLP_DEFAULT_ENDPOINT`].
///
/// Error messages never echo the raw endpoint, which may carry credentials
/// in its query string or userinfo.
#[cfg(feature = "otlp")]
fn parse_otlp_endpoint(endpoint: &str) -> Result<OtlpProbeTarget, String> {
    let uri: http::Uri = endpoint
        .parse()
        .map_err(|e| format!("endpoint is not a valid URI: {e}"))?;
    let scheme = uri.scheme_str().ok_or_else(|| {
        "endpoint has no scheme; the tonic exporter requires a full HTTP URI \
         like http://collector:4317"
            .to_owned()
    })?;
    let default_port: u16 = match scheme {
        "http" => 80,
        "https" => 443,
        other => {
            return Err(format!(
                "endpoint has unsupported scheme '{other}' (expected http or https)"
            ));
        }
    };
    // `Uri::host()` keeps the square brackets on IPv6 literals; strip them
    // so the literal is usable for socket-address resolution (previously the
    // bracketed form was sent to DNS verbatim and always failed).
    let host = match uri.host() {
        Some(h) if !h.is_empty() => h
            .strip_prefix('[')
            .and_then(|inner| inner.strip_suffix(']'))
            .unwrap_or(h)
            .to_owned(),
        _ => return Err("endpoint has no host".to_owned()),
    };
    let port = uri.port_u16().unwrap_or(default_port);
    let display = if host.contains(':') {
        format!("{scheme}://[{host}]:{port}")
    } else {
        format!("{scheme}://{host}:{port}")
    };
    Ok(OtlpProbeTarget {
        host,
        port,
        display,
    })
}

/// Split the remaining probe budget evenly across the connect attempts that
/// have not run yet, flooring at 1ms because `TcpStream::connect_timeout`
/// rejects a zero duration.
#[cfg(feature = "otlp")]
fn per_attempt_budget(remaining: std::time::Duration, attempts_left: usize) -> std::time::Duration {
    let attempts = attempts_left.max(1).min(u32::MAX as usize) as u32;
    (remaining / attempts).max(std::time::Duration::from_millis(1))
}

/// Probe the OTLP collector endpoint with a deadline-bounded TCP connect.
///
/// `timeout` bounds the whole probe, not each address: DNS resolution (a
/// blocking OS call with no timeout parameter, so it runs on a helper thread
/// that is abandoned if the deadline passes) and all connect attempts share
/// one budget, with the remainder split evenly across the addresses not yet
/// tried.
///
/// Returns `Ok(())` when a TCP connection can be established, `Err` with a
/// human-readable, credential-free reason otherwise. This is a reachability
/// check only — it does not validate that the listener speaks OTLP/gRPC.
#[cfg(feature = "otlp")]
fn probe_otlp_endpoint(endpoint: &str, timeout: std::time::Duration) -> Result<(), String> {
    use std::net::{TcpStream, ToSocketAddrs};

    let target = parse_otlp_endpoint(endpoint)?;
    let deadline = std::time::Instant::now() + timeout;

    let (tx, rx) = std::sync::mpsc::channel();
    {
        let host = target.host.clone();
        let port = target.port;
        std::thread::spawn(move || {
            let result = (host.as_str(), port)
                .to_socket_addrs()
                .map(|addrs| addrs.collect::<Vec<_>>());
            // The receiver may have given up at the deadline — ignore send
            // failure.
            let _ = tx.send(result);
        });
    }
    let addrs = match rx.recv_timeout(timeout) {
        Ok(Ok(addrs)) => addrs,
        Ok(Err(e)) => return Err(format!("failed to resolve {}: {e}", target.display)),
        Err(_) => {
            return Err(format!(
                "DNS resolution for {} did not complete within {timeout:?}",
                target.display
            ));
        }
    };
    if addrs.is_empty() {
        return Err(format!("{} resolved to no addresses", target.display));
    }

    let mut last_err = String::new();
    let total = addrs.len();
    for (i, addr) in addrs.into_iter().enumerate() {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            if last_err.is_empty() {
                last_err = "no connect attempt was made".to_owned();
            }
            return Err(format!(
                "probe deadline ({timeout:?} total) exhausted probing {}; last error: {last_err}",
                target.display
            ));
        }
        match TcpStream::connect_timeout(&addr, per_attempt_budget(remaining, total - i)) {
            Ok(_) => return Ok(()),
            Err(e) => {
                last_err = format!("connect to {addr} (for {}) failed: {e}", target.display);
            }
        }
    }
    Err(last_err)
}

/// Startup gate for `--otlp-required`: probe the resolved collector endpoint
/// and fail if it is unreachable. When `otlp_required` is false (including
/// the `--otlp-optional` path) no probe is performed and the exporter stays
/// fully lazy.
///
/// "Required" means *reachable at startup*, not guaranteed forever: once the
/// probe passes, later collector outages surface as batch-exporter errors per
/// normal OTLP semantics and do not stop the server.
#[cfg(feature = "otlp")]
fn otlp_startup_probe(
    otlp_required: bool,
    traces_endpoint: Option<&str>,
    general_endpoint: Option<&str>,
) -> Result<(), String> {
    if !otlp_required {
        return Ok(());
    }
    let (endpoint, source) = resolve_otlp_endpoint(traces_endpoint, general_endpoint);
    // Never echo the raw endpoint here — it may carry credentials in its
    // query string. The probe error already names the sanitized
    // scheme://host:port; we add only which source supplied the endpoint.
    probe_otlp_endpoint(&endpoint, OTLP_PROBE_TIMEOUT)
        .map_err(|e| format!("OTLP collector unreachable: {e} (endpoint supplied by {source})"))
}

/// Name the variant of an exporter build error without echoing its message.
///
/// The SDK error's `Display` output can embed the raw endpoint — tonic's
/// HTTPS-without-TLS `InvalidConfig` repeats the full URL, query string and
/// all — so construction-failure paths must never print the SDK message.
/// Of the two sanitization options (classify the error vs. scrub the endpoint
/// out of the SDK text), we classify: printing only this variant name plus
/// the sanitized `scheme://host:port` is simpler and structurally cannot
/// leak, at the cost of some diagnostic detail.
#[cfg(feature = "otlp")]
fn exporter_build_error_kind(e: &opentelemetry_otlp::ExporterBuildError) -> &'static str {
    use opentelemetry_otlp::ExporterBuildError as E;
    match e {
        E::ThreadSpawnFailed => "ThreadSpawnFailed",
        E::NoHttpClient => "NoHttpClient",
        E::UnsupportedCompressionAlgorithm(_) => "UnsupportedCompressionAlgorithm",
        E::InvalidUri(..) => "InvalidUri",
        E::InvalidConfig { .. } => "InvalidConfig",
        E::InternalFailure(_) => "InternalFailure",
        // `ExporterBuildError` is #[non_exhaustive], and some variants are
        // cfg-gated on SDK features, so anything unmatched lands here.
        _ => "Other",
    }
}

/// Initialise tracing for the serve command. If `--otlp-required` or
/// `--otlp-optional` is set, activates OTLP export. Otherwise uses fmt-only
/// (passive — the feature is compiled in but not activated).
///
/// With `--otlp-required`, the collector endpoint is probed eagerly (TCP
/// connect, bounded timeout) and the process exits non-zero when it is
/// unreachable — before the server binds or serves.
///
/// Like the probe errors, exporter construction-failure messages never echo
/// the raw endpoint or the SDK error text (which can embed the endpoint):
/// they name only the error kind, the sanitized `scheme://host:port`, and
/// the environment source — see [`exporter_build_error_kind`].
#[cfg(feature = "otlp")]
fn init_tracing_for_serve(args: &ServeArgs) -> Option<OtlpProvider> {
    if !args.otlp_required && !args.otlp_optional {
        init_tracing_fmt_only();
        return None;
    }

    let traces_env = std::env::var("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT").ok();
    let general_env = std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT").ok();
    // Resolved exactly as the probe (and the tonic exporter) resolves it, so
    // the construction-failure paths below can reuse the probe's sanitized
    // display form instead of deriving anything from the SDK error.
    let (endpoint, endpoint_source) =
        resolve_otlp_endpoint(traces_env.as_deref(), general_env.as_deref());

    if let Err(e) = otlp_startup_probe(
        args.otlp_required,
        traces_env.as_deref(),
        general_env.as_deref(),
    ) {
        eprintln!(
            "error: {e}\n\
             Hint: start the collector, fix OTEL_EXPORTER_OTLP_ENDPOINT, or pass \
             --otlp-optional to fall back to fmt-only logging."
        );
        std::process::exit(1);
    }

    use opentelemetry_otlp::SpanExporter;
    use opentelemetry_sdk::trace::{BatchSpanProcessor, SdkTracerProvider};
    use tracing_opentelemetry::OpenTelemetryLayer;

    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| "memory_mcp=info,warn".to_string().into());

    let fmt_layer = tracing_subscriber::fmt::layer().with_writer(std::io::stderr);

    let otlp_result = SpanExporter::builder()
        .with_tonic()
        .build()
        .map(|exporter| {
            let batch = BatchSpanProcessor::builder(exporter).build();
            SdkTracerProvider::builder()
                .with_span_processor(batch)
                .build()
        });

    match otlp_result {
        Ok(provider) => {
            let tracer = opentelemetry::trace::TracerProvider::tracer(&provider, "memory-mcp");
            let otel_layer = OpenTelemetryLayer::new(tracer);
            tracing_subscriber::registry()
                .with(filter)
                .with(fmt_layer)
                .with(otel_layer)
                .init();
            Some(provider)
        }
        Err(e) => {
            // Never print `e` itself: its text can embed the raw endpoint,
            // credentials included. Kind + sanitized endpoint + source only.
            let error_kind = exporter_build_error_kind(&e);
            let sanitized_endpoint = parse_otlp_endpoint(&endpoint)
                .map(|t| t.display)
                .unwrap_or_else(|_| "<unparseable endpoint>".to_owned());
            if args.otlp_optional {
                tracing_subscriber::registry()
                    .with(filter)
                    .with(fmt_layer)
                    .init();
                tracing::warn!(
                    error_kind,
                    endpoint = %sanitized_endpoint,
                    endpoint_source,
                    "OTLP exporter init failed — continuing with fmt-only tracing (--otlp-optional is set)"
                );
                None
            } else {
                eprintln!(
                    "error: OTLP exporter init failed ({error_kind}) for {sanitized_endpoint} \
                     (endpoint supplied by {endpoint_source})\n\
                     Hint: pass --otlp-optional to fall back to fmt-only logging."
                );
                std::process::exit(1);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Set a restrictive umask so all files created by this process are
    // owner-only by default.
    #[cfg(unix)]
    {
        // SAFETY: `umask` is a simple syscall that sets the process file-creation
        // mask. It has no memory-safety implications — the `unsafe` is required
        // only because it is an FFI call. We are a single-process server so the
        // process-global nature of umask is not a concern.
        unsafe {
            libc::umask(0o077);
        }
    }

    // Tracing goes to stderr only — stdout must remain clean for MCP.
    #[cfg(not(feature = "otlp"))]
    init_tracing();
    // otlp: tracing is initialized per-command arm below (serve may activate
    // OTLP export; other commands always use fmt-only).

    let cli = Cli::parse();

    match cli.command {
        None => {
            // Re-parse as "memory-mcp serve" so clap's env var resolution runs.
            let cli = Cli::parse_from(["memory-mcp", "serve"]);
            match cli.command {
                Some(Command::Serve(args)) => {
                    let is_stdio = args.transport == Transport::Stdio;
                    #[cfg(feature = "otlp")]
                    let _otlp_provider = init_tracing_for_serve(&args);
                    let result = run_serve(args).await;
                    #[cfg(feature = "otlp")]
                    if let Some(provider) = _otlp_provider {
                        let _ = provider.shutdown();
                    }
                    if is_stdio {
                        exit_after_stdio_serve(result);
                    }
                    result?;
                }
                _ => unreachable!(),
            }
        }
        Some(Command::Serve(args)) => {
            let is_stdio = args.transport == Transport::Stdio;
            #[cfg(feature = "otlp")]
            let _otlp_provider = init_tracing_for_serve(&args);
            let result = run_serve(args).await;
            #[cfg(feature = "otlp")]
            if let Some(provider) = _otlp_provider {
                let _ = provider.shutdown();
            }
            if is_stdio {
                exit_after_stdio_serve(result);
            }
            result?;
        }
        Some(Command::Warmup(args)) => {
            #[cfg(feature = "otlp")]
            init_tracing_fmt_only();
            run_warmup(args).await?;
        }
        Some(Command::Auth(auth_cmd)) => {
            #[cfg(feature = "otlp")]
            init_tracing_fmt_only();
            match auth_cmd.action {
                AuthAction::Login(login_args) => {
                    #[cfg(feature = "k8s")]
                    let k8s_config = if matches!(login_args.store, Some(StoreBackend::K8sSecret)) {
                        Some(auth::K8sSecretConfig {
                            namespace: login_args.k8s_namespace.clone(),
                            secret_name: login_args.k8s_secret_name.clone(),
                        })
                    } else {
                        None
                    };
                    auth::device_flow_login(
                        &auth::GitHubDeviceFlow,
                        login_args.store,
                        #[cfg(feature = "k8s")]
                        k8s_config,
                    )
                    .await
                    .map_err(|e| anyhow::anyhow!("{}", e))?;
                }
                AuthAction::Status => {
                    let provider = AuthProvider::default();
                    auth::print_auth_status(&provider);
                }
            }
        }
        Some(Command::RecallStats(args)) => {
            #[cfg(feature = "otlp")]
            init_tracing_fmt_only();
            run_recall_stats(args)?;
        }
    }

    Ok(())
}

/// How long shutdown waits for in-flight application mutation units after
/// the transport has closed, before giving up and persisting the index
/// *uncertified* (#329 review, round 3).
///
/// Generous on purpose: a unit mid-embedding on a cold CPU backend can
/// legitimately take tens of seconds. Expiry never certifies — the drain
/// helper records a mirror gap so the persisted index keeps its last
/// verified SHA and the next startup reindexes from git truth.
const SHUTDOWN_MUTATION_DRAIN_DEADLINE: std::time::Duration = std::time::Duration::from_secs(60);

/// Terminate the process explicitly after a stdio serve session.
///
/// tokio's `Stdin` performs reads on the blocking thread pool; a read that
/// is still pending — the client held its end of the pipe open while we shut
/// down on a signal — keeps the runtime's drop at the end of `main` waiting
/// indefinitely. All durable state (git repos, vector index, recall log) has
/// already been flushed by `run_serve` by the time it returns — including
/// awaiting the in-flight mutation registry, or refusing index certification
/// when its deadline expired — so exiting here loses nothing and makes
/// SIGTERM shutdown deterministic.
fn exit_after_stdio_serve(result: anyhow::Result<()>) -> ! {
    match result {
        Ok(()) => std::process::exit(0),
        Err(e) => {
            eprintln!("Error: {e:?}");
            std::process::exit(1);
        }
    }
}

// ---------------------------------------------------------------------------
// Single-writer lock
// ---------------------------------------------------------------------------

/// Acquire the advisory single-writer lock for one memory repository.
///
/// The server assumes exclusive ownership of every git repo it opens, the
/// usearch index files, and the recall log; none of these tolerate a second
/// writer (ADR-0040). That covers not just the default `--repo-path` repo
/// but every scope-mapped repository from config — two processes with
/// different defaults can still share a mapped repo, so `run_serve` calls
/// this once per distinct canonical repo path, in sorted order, and retains
/// all guards. Sorted acquisition keeps contention deterministic: processes
/// sharing any subset of repos always collide on the first shared path.
///
/// The lock is an OS advisory lock ([`std::fs::File::try_lock`], `flock`
/// semantics on Linux) on `<repo>/.memory-mcp-index/.lock`, so the kernel
/// releases it when the process exits — including on crash — and it can
/// never go stale.
///
/// Fails fast when another process holds the lock, naming the holder's pid
/// and the contended repository path. No waiting, no lease heuristics: a
/// second server writing the same repository is a deployment error
/// regardless of which transport either process uses or whether the repo is
/// a default or a mapping.
///
/// The returned guard must be kept alive for the lifetime of the server;
/// dropping it releases the lock.
fn acquire_single_writer_lock(repo_path: &std::path::Path) -> anyhow::Result<std::fs::File> {
    let index_dir = repo_path.join(".memory-mcp-index");
    std::fs::create_dir_all(&index_dir)
        .with_context(|| format!("failed to create index dir {}", index_dir.display()))?;
    let lock_path = index_dir.join(".lock");
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .with_context(|| format!("failed to open lock file {}", lock_path.display()))?;
    match file.try_lock() {
        Ok(()) => {
            // Best-effort holder breadcrumb for the contention error below.
            // The lock itself is the flock, not this content.
            file.set_len(0).ok();
            use std::io::Write;
            let _ = (&file).write_all(format!("{}\n", std::process::id()).as_bytes());
            Ok(file)
        }
        Err(std::fs::TryLockError::WouldBlock) => {
            let holder = std::fs::read_to_string(&lock_path)
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "unknown".to_string());
            anyhow::bail!(
                "another memory-mcp process (pid {holder}) is already serving the \
                 repository at {} (lock file: {}). The server requires exclusive \
                 access to every memory repo it opens — default or scope-mapped — \
                 including its vector index and recall log. Stop the other \
                 instance, or remove the shared repository from one of the \
                 configurations.",
                repo_path.display(),
                lock_path.display()
            )
        }
        Err(e) => Err(e).with_context(|| format!("failed to lock {}", lock_path.display())),
    }
}

// ---------------------------------------------------------------------------
// Server startup
// ---------------------------------------------------------------------------

/// Start and run the MCP server with the provided arguments, over the
/// selected transport (streamable HTTP by default, or stdio).
async fn run_serve(args: ServeArgs) -> anyhow::Result<()> {
    // Validate branch name early to prevent ref injection.
    validate_branch_name(&args.branch).context("invalid --branch value")?;

    // Expand `~` in repo_path, failing loudly if HOME is not set and the
    // path requires it (i.e. the user did not provide --repo-path explicitly).
    // Canonicalize BEFORE opening so the location the repo is opened at and
    // the router's collision-detection key can never diverge (#293 review,
    // round 5: a symlink-plus-`..` spelling otherwise opens one physical
    // repo while collision detection records another).
    let repo_path = expand_path(&args.repo_path)?;
    let repo_path = memory_mcp::fs_util::canonicalize_allow_missing(&repo_path)
        .context("failed to canonicalize repo path")?;
    info!("repo path: {}", repo_path.display());

    // Data-dir layout: the vector index, recall log, and single-writer lock
    // all live under `.memory-mcp-index` inside the repo path.
    let index_dir = repo_path.join(".memory-mcp-index");

    // Load per-scope remote config BEFORE locking: scope-mapped repositories
    // are written by this process exactly like the default one, so the
    // single-writer guarantee must cover every repo the config names — two
    // processes with distinct defaults sharing one mapped repo would
    // otherwise hold different locks while mutating the same git tree
    // (#329 review, round 2).
    let config = {
        let config_path = match &args.config {
            Some(p) if p.is_empty() => None,
            Some(p) => Some(expand_path(p)?),
            None => memory_mcp::config::Config::resolve_path().ok(),
        };
        match config_path {
            Some(path) => Some(
                memory_mcp::config::Config::load(&path)
                    .with_context(|| format!("failed to load config from {}", path.display()))?,
            ),
            None => None,
        }
    };

    // Collect every distinct repository this process will write: the default
    // plus all mapped paths, canonicalized so different spellings of one
    // location cannot yield different lock files. BTreeSet gives sorted,
    // deduplicated acquisition order — deterministic contention, and no
    // self-conflict when a mapping resolves to the default repo (the router
    // rejects that collision later with a clearer error).
    let mut lock_targets = std::collections::BTreeSet::new();
    lock_targets.insert(repo_path.clone());
    if let Some(config) = &config {
        for mapping in &config.remotes {
            let mapped = mapping
                .resolved_path()
                .with_context(|| format!("failed to resolve path for scope '{}'", mapping.scope))?;
            let mapped =
                memory_mcp::fs_util::canonicalize_allow_missing(&mapped).with_context(|| {
                    format!(
                        "failed to canonicalize repo path for scope '{}'",
                        mapping.scope
                    )
                })?;
            lock_targets.insert(mapped);
        }
    }

    // Enforce single-writer before any subsystem opens: the git repos, index
    // files, and recall log all assume exclusive ownership (ADR-0040).
    // Acquiring before the (slow) embedding init makes contention fail fast.
    let _single_writer_locks: Vec<std::fs::File> = lock_targets
        .iter()
        .map(|repo| acquire_single_writer_lock(repo))
        .collect::<anyhow::Result<_>>()?;

    // Filter out empty string to treat MEMORY_MCP_REMOTE_URL="" as unset.
    let remote_url = args.remote_url.clone().filter(|u| !u.is_empty());

    if args.require_remote_sync && remote_url.is_none() {
        anyhow::bail!("--require-remote-sync requires --remote-url to be set");
    }

    // Create the health registry early so reporters can be passed to subsystems.
    let stale_threshold = if args.health_stale_secs == 0 {
        None
    } else {
        Some(std::time::Duration::from_secs(args.health_stale_secs))
    };
    let health = HealthRegistry::with_config(args.require_remote_sync, stale_threshold);

    // Initialise subsystems — each called function creates its own span.
    let repo = MemoryRepo::init_or_open_with_reporter(
        &repo_path,
        remote_url.as_deref(),
        health.git.clone(),
        health.sync.clone(),
    )
    .with_context(|| format!("failed to open/init repo at {}", repo_path.display()))?;

    let embed_timeout = std::time::Duration::from_secs(args.embed.embed_timeout_secs);
    let embedding: Box<dyn EmbeddingBackend> = Box::new(
        CandleEmbeddingEngine::new(
            embed_timeout,
            args.embed.embed_queue_size,
            health.embedding.clone(),
        )
        .context("failed to init embedding engine")?,
    );

    let dimensions = embedding.dimensions();

    // Remove legacy single-index files if they still exist from an old install.
    let old_index = index_dir.join("index.usearch");
    if old_index.exists() {
        if let Err(e) = std::fs::remove_file(&old_index) {
            tracing::warn!(error = %e, "failed to remove legacy index file");
        }
        let keys_file = index_dir.join("index.usearch.keys.json");
        if let Err(e) = std::fs::remove_file(&keys_file) {
            if e.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(error = %e, "failed to remove legacy index keys file");
            }
        }
        info!("removed legacy single-index files");
    }

    let repo = Arc::new(repo);

    // Build the repo router from the config loaded above (before lock
    // acquisition) — every repo the router opens is already covered by a
    // single-writer lock held by this process.
    let router = match &config {
        Some(config) if !config.remotes.is_empty() => {
            memory_mcp::repo_router::RepoRouter::from_config(
                Arc::clone(&repo),
                &config.remotes,
                &health.git,
                &health.sync,
            )
            .context("failed to initialise scope-specific repos from config")?
        }
        _ => memory_mcp::repo_router::RepoRouter::single(Arc::clone(&repo)),
    };

    // Load the persisted index; create fresh if missing or corrupt. Freshness
    // against repo HEAD is decided inside `startup_prepare_index`, after the
    // initial pull.
    let loaded_index: Box<dyn VectorStore> = Box::new(
        UsearchStore::load_with_reporter(&index_dir, dimensions, health.vector_index.clone())
            .unwrap_or_else(|e| {
                tracing::warn!("could not load index ({}), creating fresh", e);
                UsearchStore::new_with_reporter(dimensions, health.vector_index.clone())
                    .expect("failed to create index")
            }),
    );

    let auth = AuthProvider::new();

    // When --require-remote-sync is set, the initial pull runs BEFORE the
    // index freshness check (#327): on a fresh repo path the pull is what
    // populates the repo, so the startup reindex must run against post-pull
    // git truth — otherwise the server reports ready while semantic recall
    // misses every pulled memory. The pull covers every routed repo (default
    // + scope-mapped remotes, with branch overrides — #328 review, round 2)
    // and seeds aggregate sync health with a known state.
    let initial_pull =
        (args.require_remote_sync && remote_url.is_some()).then_some((&auth, args.branch.as_str()));

    let vector_reporter = health.vector_index.clone();
    let (index, reindex_ok) = memory_mcp::server::startup_prepare_index(
        initial_pull,
        &router,
        embedding.as_ref(),
        &index_dir,
        loaded_index,
        move || {
            Box::new(
                UsearchStore::new_with_reporter(dimensions, vector_reporter)
                    .expect("failed to create index"),
            )
        },
    )
    .await;

    // Tracks whether the vector index verifiably mirrors git truth at startup.
    // A reindex that did not complete cleanly is a gap only the next full
    // reindex closes, so SHA advancement must stay blocked for this process.
    let startup_mirror_gap = !reindex_ok;

    // Only mark subsystems healthy if the reindex succeeded or was skipped
    // (SHA matched). If the reindex had errors, the subsystems have already
    // reported their own state via their reporters — an unconditional
    // `health.git.report_ok()` here erased a strict-list/reindex repo
    // failure recorded during the rebuild (#293 review, round 4). Git
    // init/open succeeding earlier is not evidence that every repo listed
    // cleanly.
    if reindex_ok {
        health.git.report_ok();
        health.embedding.report_ok();
        health.vector_index.report_ok();
    }

    let recall_log_path = index_dir.join("recall_log.sqlite");
    let recall_log = match RecallLog::open(
        &recall_log_path,
        std::time::Duration::from_secs(args.recall_log_busy_timeout),
    ) {
        Ok(log) => {
            info!("recall log opened at {}", recall_log_path.display());
            Some(Arc::new(log))
        }
        Err(e) => {
            warn!(error = %e, "failed to open recall log — recall events will not be logged");
            None
        }
    };

    let state = Arc::new(AppState::with_router(
        repo,
        router,
        args.branch.clone(),
        embedding,
        index,
        auth,
        health,
        recall_log,
    ));
    if startup_mirror_gap {
        state.mark_index_mirror_incomplete();
    }

    // Populate the lexical (BM25) index. It lives in RAM only — indexing
    // text is cheap, unlike embedding it — so it is rebuilt from the repo on
    // every startup and never persisted or migrated. Failure degrades recall
    // to semantic-only; it never blocks startup.
    match memory_mcp::search::rebuild_lexical_from_router(&state.router, &state.lexical)
        .instrument(tracing::info_span!("startup.lexical_rebuild"))
        .await
    {
        Ok(count) => info!(count, "lexical index built"),
        Err(e) => {
            // Every startup-rebuild failure (including a repository-listing
            // failure before the rebuild seam) marks the index degraded, so
            // repair is scheduled here and re-triggered by recall until it
            // converges — the failure can never silently disable keyword
            // search for the process lifetime.
            tracing::warn!(
                error = %e,
                degraded = state.lexical.is_degraded(),
                "lexical index startup rebuild failed — keyword search \
                 degraded until background repair converges"
            );
            memory_mcp::search::spawn_lexical_repair_for_router(&state.router, &state.lexical);
        }
    }

    // Keep a reference for post-shutdown index persistence.
    let state_for_shutdown = Arc::clone(&state);

    match args.transport {
        Transport::Http => serve_http(&args, Arc::clone(&state)).await?,
        Transport::Stdio => serve_stdio(Arc::clone(&state)).await?,
    }

    // The transport drain above cannot vouch for application mutations:
    // rmcp bounds its awaited response drain (~2s) and detaches handler
    // tasks, so a mutation stalled past that window is still running when
    // the transport reports closed (#329 review, round 3). Seal admission
    // and await the application's own mutation registry before touching the
    // index (#329 review, round 4: the seal closes the window where a
    // detached handler not yet polled could register *after* the drain
    // observed zero in-flight).
    match memory_mcp::server::drain_mutations_before_persist(
        &state_for_shutdown,
        &index_dir,
        SHUTDOWN_MUTATION_DRAIN_DEADLINE,
    )
    .await
    {
        Ok(true) => {
            info!("mutation units drained — persisting vector index");

            // Persist the scoped vector index so the next startup can skip a
            // full reindex. The stored SHA only advances when the in-process
            // mirror is intact — a recorded mirror gap keeps the last verified
            // SHA so the next startup rebuilds from git truth.
            if let Err(e) =
                memory_mcp::server::persist_index_on_shutdown(&state_for_shutdown, &index_dir).await
            {
                tracing::warn!("failed to persist vector index on shutdown: {}", e);
            } else {
                info!("vector index saved to {}", index_dir.display());
            }
        }
        Ok(false) => {
            // Drain deadline expired with a mutation abandoned mid-flight. Do
            // not touch the on-disk index at all (#329 review, round 4): the
            // in-memory index may hold that unit's partial mirror *without* git
            // HEAD having advanced, so writing it out — even with the last
            // verified SHA — could hand the next startup a dirty index whose
            // stored SHA still equals HEAD. The untouched on-disk snapshot was
            // consistent with git when it was written: if the abandoned unit's
            // commit landed, HEAD moved and the SHA check forces a reindex; if
            // it never landed, the snapshot still mirrors git truth. The drain
            // helper has also revoked the in-memory certification for any other
            // persistence path, and — because that revocation dies with this
            // process while the old snapshot's stored SHA may still equal an
            // unadvanced HEAD — durably recorded the forced next-start reindex
            // (reindex-required marker, or on marker-write failure the
            // persisted certification itself is revoked) so the next startup
            // rebuilds regardless of SHA/HEAD equality (#329 review, rounds
            // 4–5).
            tracing::warn!(
                "mutation drain deadline expired — leaving the on-disk vector index \
                 untouched; the durable revocation forces the next startup to \
                 rebuild from git truth"
            );
        }
        Err(e) => {
            // Neither durable revocation channel could be recorded (#329
            // review, round 5): exiting normally here would let the next
            // startup wrongly certify the stale on-disk snapshot when HEAD
            // did not advance. Propagate so shutdown reports failure
            // (non-zero exit) instead of logging the guarantee away.
            return Err(anyhow::Error::from(e)
                .context("shutdown could not durably revoke index certification"));
        }
    }

    Ok(())
}

/// Serve MCP over streamable HTTP (ADR-0001): axum router with health
/// endpoints, bounded session manager, graceful shutdown on SIGINT/SIGTERM.
async fn serve_http(args: &ServeArgs, state: Arc<AppState>) -> anyhow::Result<()> {
    let state_for_routes = Arc::clone(&state);

    // Build the MCP service.
    let ct = CancellationToken::new();
    let ct_child = ct.child_token();

    let service = StreamableHttpService::new(
        move || Ok(MemoryServer::new(Arc::clone(&state))),
        {
            let mut builder = BoundedSessionManagerBuilder::new(args.max_sessions);
            if args.idle_timeout_secs == 0 && args.max_session_lifetime_secs == 0 {
                warn!("both idle timeout and max session lifetime are disabled; sessions are only cleaned up by FIFO eviction at capacity");
            }
            if args.idle_timeout_secs > 0 {
                builder =
                    builder.idle_timeout(std::time::Duration::from_secs(args.idle_timeout_secs));
            }
            if args.max_session_lifetime_secs > 0 {
                builder = builder.max_lifetime(std::time::Duration::from_secs(
                    args.max_session_lifetime_secs,
                ));
            }
            if args.session_rate_limit > 0 && args.session_rate_window_secs > 0 {
                builder = builder.rate_limit(
                    args.session_rate_limit,
                    std::time::Duration::from_secs(args.session_rate_window_secs),
                );
            }
            builder.build()
        },
        {
            let mut server_config = StreamableHttpServerConfig::default();
            server_config.cancellation_token = ct_child;
            for host in &args.allowed_host {
                server_config.allowed_hosts.push(host.clone());
            }
            server_config
        },
    );

    let mcp_path = args.mcp_path.clone();
    let router = axum::Router::new()
        // Static liveness check. Always returns 200 OK once the process is running.
        .route("/healthz", axum::routing::get(healthz_handler))
        .route("/readyz", axum::routing::get(readyz_handler))
        .route("/version", axum::routing::get(version_handler))
        .with_state(state_for_routes)
        .nest_service(&mcp_path, service);

    let listener = tokio::net::TcpListener::bind(&args.bind)
        .await
        .with_context(|| format!("failed to bind to {}", args.bind))?;

    info!("listening on {} (MCP at {})", args.bind, args.mcp_path);

    axum::serve(listener, router)
        .with_graceful_shutdown(async move {
            shutdown_signal().await;
            info!("shutdown signal received");
            ct.cancel();
        })
        .await
        .context("server error")?;

    Ok(())
}

/// Serve MCP over stdio (ADR-0040): stdout carries JSON-RPC framing (all
/// tracing goes to stderr), stdin EOF is the normal end-of-session signal.
/// One process serves exactly one client; the MCP client owns the lifecycle.
async fn serve_stdio(state: Arc<AppState>) -> anyhow::Result<()> {
    info!("serving MCP over stdio (stdout is the protocol channel)");

    let service = rmcp::serve_server(MemoryServer::new(state), rmcp::transport::stdio())
        .await
        .context("failed to initialize stdio transport")?;

    drive_service_until_quit(service, shutdown_signal()).await
}

/// Run an initialized rmcp service until the client closes the session or
/// `shutdown` resolves (SIGINT/SIGTERM in production).
///
/// The signal path must not simply drop the `waiting()` future: rmcp only
/// guarantees cleanup ordering — draining in-flight handler responses, then
/// closing the transport — for an *awaited* cancellation; a dropped future
/// leaves the drop guard cancelling asynchronously while `run_serve`
/// proceeds to stamp and persist the vector index, racing any in-flight
/// mutation (#329 review, round 2). So on shutdown this cancels through the
/// service's token and then awaits the same waiting future, making
/// drain-before-persist a contract instead of a coincidence.
///
/// That contract is *bounded*: rmcp 1.8 drains handler responses for at
/// most ~2 seconds and its handler tasks are detached, so a mutation
/// stalled past the window outlives this function (#329 review, round 3).
/// Returning here therefore only means the transport is closed —
/// `run_serve` must still await the application's mutation registry
/// (`drain_mutations_before_persist`) before index persistence may certify.
async fn drive_service_until_quit<S>(
    service: rmcp::service::RunningService<rmcp::RoleServer, S>,
    shutdown: impl std::future::Future<Output = ()>,
) -> anyhow::Result<()>
where
    S: rmcp::service::Service<rmcp::RoleServer>,
{
    // Grab the cancellation token before `waiting()` consumes the service.
    let ct = service.cancellation_token();
    let waiting = service.waiting();
    tokio::pin!(waiting);
    tokio::pin!(shutdown);

    tokio::select! {
        quit = &mut waiting => {
            let reason = quit.context("stdio transport task failed")?;
            info!(?reason, "stdio transport closed");
        }
        _ = &mut shutdown => {
            info!("shutdown signal received — draining service before index persistence");
            ct.cancel();
            let reason = waiting
                .await
                .context("stdio transport task failed during shutdown drain")?;
            info!(?reason, "stdio transport drained and closed");
        }
    }

    Ok(())
}

/// Resolve when the process receives SIGINT (ctrl-c) or, on unix, SIGTERM.
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut sigterm = signal(SignalKind::terminate()).expect("SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {},
            _ = sigterm.recv() => {},
        }
    }
    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to listen for ctrl-c");
    }
}

/// Print recall precision statistics bucketed by distance range.
fn run_recall_stats(args: RecallStatsArgs) -> anyhow::Result<()> {
    // Resolve the recall log path: explicit flag > env var (handled by clap) >
    // default derived from index_dir or ~/.memory-mcp/.memory-mcp-index.
    let log_path = if let Some(p) = args.recall_log {
        PathBuf::from(p)
    } else {
        let index_dir = if let Some(d) = args.index_dir {
            PathBuf::from(d)
        } else {
            let home = dirs::home_dir()
                .ok_or_else(|| anyhow::anyhow!("could not determine home directory"))?;
            home.join(".memory-mcp").join(".memory-mcp-index")
        };
        index_dir.join("recall_log.sqlite")
    };

    let log = RecallLog::open(&log_path, std::time::Duration::from_secs(5))
        .with_context(|| format!("failed to open recall log at {}", log_path.display()))?;

    let buckets = log
        .recall_stats()
        .context("failed to compute recall stats")?;

    // Print only non-empty buckets.
    let any = buckets.iter().any(|b| b.total > 0);
    if !any {
        println!("No recall events recorded yet.");
        return Ok(());
    }

    for b in &buckets {
        if b.total == 0 {
            continue;
        }
        let applied_pct = if b.applied > 0 {
            format!("{}%", b.applied * 100 / b.total)
        } else {
            "0%".to_string()
        };
        println!(
            "{:.2}–{:.2}:  applied {} ({}/{}), maybe {}, not_applied {}, unknown {}",
            b.range_start,
            b.range_end,
            applied_pct,
            b.applied,
            b.total,
            b.maybe,
            b.not_applied,
            b.unknown,
        );
    }

    Ok(())
}

/// Load the embedding model and run a single dummy embed to warm the on-disk
/// model cache, then exit. Intended for use as a Kubernetes init container.
async fn run_warmup(args: WarmupArgs) -> anyhow::Result<()> {
    use memory_mcp::health::SubsystemReporter;
    info!("warming up embedding model '{}'", MODEL_ID);
    let engine = CandleEmbeddingEngine::new(
        std::time::Duration::from_secs(args.embed.embed_timeout_secs),
        args.embed.embed_queue_size,
        SubsystemReporter::new(),
    )
    .context("failed to init embedding engine")?;
    // Run one dummy embed to ensure the model weights are fully loaded and any
    // cached files are written to disk.
    let _ = engine
        .embed(&["warmup".to_string()])
        .await
        .context("warmup embed failed")?;
    info!("warmup complete");
    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Parse a `usize` that must be at least 1. Used as a clap `value_parser`.
fn parse_nonzero_usize(s: &str) -> Result<usize, String> {
    let n: usize = s
        .parse()
        .map_err(|_| format!("'{s}' is not a valid integer"))?;
    if n == 0 {
        return Err("value must be at least 1".to_owned());
    }
    Ok(n)
}

/// Parse a `u64` that must be at least 1. Used as a clap `value_parser`.
fn parse_nonzero_u64(s: &str) -> Result<u64, String> {
    let n: u64 = s
        .parse()
        .map_err(|_| format!("'{s}' is not a valid integer"))?;
    if n == 0 {
        return Err("value must be at least 1".to_owned());
    }
    Ok(n)
}

fn expand_path(path: &str) -> anyhow::Result<PathBuf> {
    memory_mcp::fs_util::expand_tilde(path).map_err(|e| anyhow::anyhow!("{e}"))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn test_cli_bare_has_no_command() {
        let cli = Cli::try_parse_from(["memory-mcp"]).expect("bare invocation should parse");
        assert!(cli.command.is_none());
    }

    #[test]
    fn test_cli_serve_with_bind() {
        let cli = Cli::try_parse_from(["memory-mcp", "serve", "--bind", "0.0.0.0:9090"])
            .expect("serve --bind should parse");
        match cli.command {
            Some(Command::Serve(args)) => assert_eq!(args.bind, "0.0.0.0:9090"),
            _ => panic!("expected Serve command"),
        }
    }

    #[test]
    fn test_cli_serve_transport_defaults_to_http() {
        let cli = Cli::try_parse_from(["memory-mcp", "serve"]).expect("serve should parse");
        match cli.command {
            Some(Command::Serve(args)) => assert_eq!(args.transport, Transport::Http),
            _ => panic!("expected Serve command"),
        }
    }

    #[test]
    fn test_cli_serve_transport_stdio_parses() {
        let cli = Cli::try_parse_from(["memory-mcp", "serve", "--transport", "stdio"])
            .expect("serve --transport stdio should parse");
        match cli.command {
            Some(Command::Serve(args)) => assert_eq!(args.transport, Transport::Stdio),
            _ => panic!("expected Serve command"),
        }
    }

    #[test]
    fn test_cli_serve_transport_rejects_unknown_value() {
        assert!(
            Cli::try_parse_from(["memory-mcp", "serve", "--transport", "carrier-pigeon"]).is_err(),
            "unknown transport value must be rejected"
        );
    }

    #[test]
    fn single_writer_lock_excludes_second_acquisition_and_releases_on_drop() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let index_dir = tmp.path().join(".memory-mcp-index");

        let guard = acquire_single_writer_lock(&index_dir).expect("first acquire");

        // flock is per open-file-description, so a second acquisition from
        // the same process still contends — same shape as a second process.
        let err = acquire_single_writer_lock(&index_dir).expect_err("second acquire must fail");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("already serving"),
            "error should explain the contention: {msg}"
        );
        assert!(
            msg.contains(&std::process::id().to_string()),
            "error should name the holder pid: {msg}"
        );

        drop(guard);
        let _reacquired = acquire_single_writer_lock(&index_dir).expect("re-acquire after release");
    }

    #[test]
    fn test_cli_auth_login_store_keyring() {
        let cli = Cli::try_parse_from(["memory-mcp", "auth", "login", "--store", "keyring"])
            .expect("auth login --store keyring should parse");
        match cli.command {
            Some(Command::Auth(auth_cmd)) => match auth_cmd.action {
                AuthAction::Login(login_args) => {
                    assert!(matches!(login_args.store, Some(StoreBackend::Keyring)));
                }
                _ => panic!("expected Login action"),
            },
            _ => panic!("expected Auth command"),
        }
    }

    #[test]
    fn test_cli_auth_status() {
        let cli = Cli::try_parse_from(["memory-mcp", "auth", "status"])
            .expect("auth status should parse");
        match cli.command {
            Some(Command::Auth(auth_cmd)) => {
                assert!(matches!(auth_cmd.action, AuthAction::Status));
            }
            _ => panic!("expected Auth command"),
        }
    }

    #[test]
    fn test_bare_serve_reparsed_uses_env_var() {
        // Simulate what happens in the None arm: parse_from builds ServeArgs
        // from env vars. This test just checks that parse_from succeeds and
        // produces a Serve command.
        let cli = Cli::parse_from(["memory-mcp", "serve"]);
        assert!(matches!(cli.command, Some(Command::Serve(_))));
    }

    #[cfg(feature = "k8s")]
    #[test]
    fn test_cli_auth_login_store_k8s_secret() {
        let cli = Cli::try_parse_from(["memory-mcp", "auth", "login", "--store", "k8s-secret"])
            .expect("auth login --store k8s-secret should parse");
        match cli.command {
            Some(Command::Auth(auth_cmd)) => match auth_cmd.action {
                AuthAction::Login(login_args) => {
                    assert!(matches!(login_args.store, Some(StoreBackend::K8sSecret)));
                    assert_eq!(login_args.k8s_namespace, "memory-mcp");
                    assert_eq!(login_args.k8s_secret_name, "memory-mcp-github-token");
                }
                _ => panic!("expected Login action"),
            },
            _ => panic!("expected Auth command"),
        }
    }

    #[test]
    fn test_parse_nonzero_usize_zero_is_err() {
        assert!(parse_nonzero_usize("0").is_err());
    }

    #[test]
    fn test_parse_nonzero_usize_non_numeric_is_err() {
        assert!(parse_nonzero_usize("abc").is_err());
    }

    #[test]
    fn test_parse_nonzero_usize_one_is_ok() {
        assert_eq!(parse_nonzero_usize("1").unwrap(), 1);
    }

    #[test]
    fn test_parse_nonzero_u64_zero_is_err() {
        assert!(parse_nonzero_u64("0").is_err());
    }

    #[test]
    fn test_parse_nonzero_u64_non_numeric_is_err() {
        assert!(parse_nonzero_u64("abc").is_err());
    }

    #[test]
    fn test_parse_nonzero_u64_one_is_ok() {
        assert_eq!(parse_nonzero_u64("1").unwrap(), 1);
    }

    #[test]
    fn test_cli_serve_allowed_host_single() {
        let cli = Cli::try_parse_from([
            "memory-mcp",
            "serve",
            "--allowed-host",
            "memory-mcp.svc.echoes",
        ])
        .expect("serve --allowed-host should parse");
        match cli.command {
            Some(Command::Serve(args)) => {
                assert_eq!(args.allowed_host, vec!["memory-mcp.svc.echoes"]);
            }
            _ => panic!("expected Serve command"),
        }
    }

    #[test]
    fn test_cli_serve_allowed_host_multiple() {
        let cli = Cli::try_parse_from([
            "memory-mcp",
            "serve",
            "--allowed-host",
            "host-a.example.com",
            "--allowed-host",
            "host-b.example.com:8080",
        ])
        .expect("serve with multiple --allowed-host should parse");
        match cli.command {
            Some(Command::Serve(args)) => {
                assert_eq!(args.allowed_host.len(), 2);
                assert_eq!(args.allowed_host[0], "host-a.example.com");
                assert_eq!(args.allowed_host[1], "host-b.example.com:8080");
            }
            _ => panic!("expected Serve command"),
        }
    }

    #[test]
    fn test_cli_serve_no_allowed_host_defaults_empty() {
        let cli =
            Cli::try_parse_from(["memory-mcp", "serve"]).expect("serve without hosts should parse");
        match cli.command {
            Some(Command::Serve(args)) => {
                assert!(args.allowed_host.is_empty());
            }
            _ => panic!("expected Serve command"),
        }
    }

    #[test]
    fn test_cli_version() {
        match Cli::try_parse_from(["memory-mcp", "--version"]) {
            Err(e) => assert_eq!(e.kind(), clap::error::ErrorKind::DisplayVersion),
            Ok(_) => panic!("--version should cause clap to exit"),
        }
    }

    #[test]
    fn test_expand_path_tilde_alone() {
        let result = expand_path("~").unwrap();
        assert_eq!(result, dirs::home_dir().unwrap());
    }

    #[test]
    fn test_expand_path_tilde_slash() {
        let result = expand_path("~/foo/bar").unwrap();
        assert_eq!(result, dirs::home_dir().unwrap().join("foo/bar"));
    }

    #[test]
    fn test_expand_path_absolute() {
        let result = expand_path("/tmp/repo").unwrap();
        assert_eq!(result, PathBuf::from("/tmp/repo"));
    }

    #[test]
    fn test_expand_path_relative() {
        let result = expand_path("relative/path").unwrap();
        assert_eq!(result, PathBuf::from("relative/path"));
    }

    #[test]
    fn test_expand_path_tilde_user_rejected() {
        let result = expand_path("~otheruser/path");
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("not supported"),
            "error should mention unsupported: {msg}"
        );
    }

    #[test]
    fn test_cli_serve_idle_timeout_default() {
        let cli = Cli::try_parse_from(["memory-mcp", "serve"]).expect("serve should parse");
        match cli.command {
            Some(Command::Serve(args)) => assert_eq!(args.idle_timeout_secs, 14400),
            _ => panic!("expected Serve command"),
        }
    }

    #[test]
    fn test_cli_serve_idle_timeout_custom() {
        let cli = Cli::try_parse_from(["memory-mcp", "serve", "--idle-timeout-secs", "300"])
            .expect("serve with idle-timeout should parse");
        match cli.command {
            Some(Command::Serve(args)) => assert_eq!(args.idle_timeout_secs, 300),
            _ => panic!("expected Serve command"),
        }
    }

    #[test]
    fn test_cli_serve_max_session_lifetime_default() {
        let cli = Cli::try_parse_from(["memory-mcp", "serve"]).expect("serve should parse");
        match cli.command {
            Some(Command::Serve(args)) => assert_eq!(args.max_session_lifetime_secs, 0),
            _ => panic!("expected Serve command"),
        }
    }

    #[test]
    fn test_cli_serve_max_session_lifetime_custom() {
        let cli = Cli::try_parse_from([
            "memory-mcp",
            "serve",
            "--max-session-lifetime-secs",
            "86400",
        ])
        .expect("serve with max-session-lifetime should parse");
        match cli.command {
            Some(Command::Serve(args)) => assert_eq!(args.max_session_lifetime_secs, 86400),
            _ => panic!("expected Serve command"),
        }
    }

    /// Regression: `--otlp-required` with an unreachable collector must fail
    /// the startup probe (previously the lazy tonic exporter let the server
    /// start and errors only surfaced on the first export attempt).
    #[cfg(feature = "otlp")]
    #[test]
    fn test_otlp_required_dead_endpoint_fails_probe() {
        // Bind an ephemeral port, then drop the listener so the port is closed.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
        let port = listener.local_addr().expect("local addr").port();
        drop(listener);

        let endpoint = format!("http://127.0.0.1:{port}");
        let result = otlp_startup_probe(true, None, Some(&endpoint));
        let err = result.expect_err("probe against a closed port must fail");
        assert!(
            err.contains(&endpoint),
            "error should name the endpoint: {err}"
        );
    }

    /// `--otlp-required` with a reachable collector proceeds past the probe.
    /// A bare TCP listener is enough — the probe checks reachability, not
    /// OTLP protocol conformance.
    #[cfg(feature = "otlp")]
    #[test]
    fn test_otlp_required_live_endpoint_passes_probe() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
        let port = listener.local_addr().expect("local addr").port();

        let endpoint = format!("http://127.0.0.1:{port}");
        otlp_startup_probe(true, None, Some(&endpoint))
            .expect("probe against a live listener must pass");
    }

    /// Without `--otlp-required` (including the `--otlp-optional` path) no
    /// probe runs: a dead endpoint must not block startup.
    #[cfg(feature = "otlp")]
    #[test]
    fn test_otlp_not_required_skips_probe() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
        let port = listener.local_addr().expect("local addr").port();
        drop(listener);

        let endpoint = format!("http://127.0.0.1:{port}");
        otlp_startup_probe(false, None, Some(&endpoint))
            .expect("no probe should run when otlp is not required");
    }

    /// Regression: an IPv6 literal endpoint like `http://[::1]:PORT` must
    /// probe the IPv6 listener. Previously the bracketed literal was passed
    /// to DNS resolution verbatim, so a live IPv6 collector failed the probe
    /// with a bogus lookup error.
    #[cfg(feature = "otlp")]
    #[test]
    fn test_otlp_probe_ipv6_literal_live_listener() {
        let listener = match std::net::TcpListener::bind("[::1]:0") {
            Ok(l) => l,
            Err(e) => {
                // Environment without IPv6 loopback — nothing to assert.
                eprintln!("skipping: IPv6 loopback unavailable: {e}");
                return;
            }
        };
        let port = listener.local_addr().expect("local addr").port();

        let endpoint = format!("http://[::1]:{port}");
        otlp_startup_probe(true, None, Some(&endpoint))
            .expect("probe against a live IPv6 listener must pass");
    }

    /// The probe budget is split across remaining connect attempts so the
    /// probe is bounded by ~OTLP_PROBE_TIMEOUT total, not per address.
    #[cfg(feature = "otlp")]
    #[test]
    fn test_per_attempt_budget_splits_remaining() {
        use std::time::Duration;
        assert_eq!(
            per_attempt_budget(Duration::from_secs(4), 4),
            Duration::from_secs(1)
        );
        assert_eq!(
            per_attempt_budget(Duration::from_secs(3), 1),
            Duration::from_secs(3)
        );
        // Floored at 1ms: connect_timeout rejects a zero duration.
        assert_eq!(
            per_attempt_budget(Duration::from_micros(10), 4),
            Duration::from_millis(1)
        );
        // Defensive: zero attempts must not divide by zero.
        assert_eq!(
            per_attempt_budget(Duration::from_secs(1), 0),
            Duration::from_secs(1)
        );
    }

    /// The deadline covers DNS resolution too: with a zero budget the probe
    /// fails at the deadline instead of blocking on the resolver or
    /// proceeding to connect attempts.
    #[cfg(feature = "otlp")]
    #[test]
    fn test_otlp_probe_total_deadline_bounds_dns() {
        let err = probe_otlp_endpoint("http://localhost:4317", std::time::Duration::ZERO)
            .expect_err("a zero total budget must fail the probe");
        assert!(
            err.contains("did not complete") || err.contains("deadline"),
            "error should attribute the failure to the total deadline: {err}"
        );
    }

    /// Regression: a credential-bearing endpoint must never be echoed in
    /// probe error text — only the sanitized `scheme://host:port` plus the
    /// name of the env var that supplied the endpoint appear.
    #[cfg(feature = "otlp")]
    #[test]
    fn test_otlp_probe_error_sanitizes_credentials() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
        let port = listener.local_addr().expect("local addr").port();
        drop(listener);

        let endpoint = format!("http://127.0.0.1:{port}/v1/traces?api_key=super-secret");
        let err = otlp_startup_probe(true, None, Some(&endpoint))
            .expect_err("probe against a closed port must fail");
        assert!(
            !err.contains("super-secret") && !err.contains("api_key"),
            "credential leaked into error text: {err}"
        );
        assert!(
            err.contains(&format!("http://127.0.0.1:{port}")),
            "sanitized endpoint should appear: {err}"
        );
        assert!(
            err.contains("OTEL_EXPORTER_OTLP_ENDPOINT"),
            "error should name the supplying env var: {err}"
        );
    }

    #[cfg(feature = "otlp")]
    #[test]
    fn test_resolve_otlp_endpoint_default() {
        assert_eq!(
            resolve_otlp_endpoint(None, None),
            (OTLP_DEFAULT_ENDPOINT.to_owned(), "default")
        );
        // Empty values are treated as unset.
        assert_eq!(
            resolve_otlp_endpoint(Some(""), Some("  ")),
            (OTLP_DEFAULT_ENDPOINT.to_owned(), "default")
        );
    }

    #[cfg(feature = "otlp")]
    #[test]
    fn test_resolve_otlp_endpoint_traces_takes_precedence() {
        assert_eq!(
            resolve_otlp_endpoint(Some("http://traces:4317"), Some("http://general:4317")),
            (
                "http://traces:4317".to_owned(),
                "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT"
            )
        );
        assert_eq!(
            resolve_otlp_endpoint(None, Some("http://general:4317")),
            (
                "http://general:4317".to_owned(),
                "OTEL_EXPORTER_OTLP_ENDPOINT"
            )
        );
    }

    #[cfg(feature = "otlp")]
    #[test]
    fn test_parse_otlp_endpoint_forms() {
        let t = parse_otlp_endpoint("http://localhost:4317").unwrap();
        assert_eq!((t.host.as_str(), t.port), ("localhost", 4317));
        assert_eq!(t.display, "http://localhost:4317");

        // Paths and query strings are dropped from the probe target.
        let t = parse_otlp_endpoint("https://collector.example.com:4318/v1/traces").unwrap();
        assert_eq!((t.host.as_str(), t.port), ("collector.example.com", 4318));
        assert_eq!(t.display, "https://collector.example.com:4318");

        // IPv6 literals: `Uri::host()` strips the brackets, so the host is
        // resolvable; the display form re-adds them.
        let t = parse_otlp_endpoint("http://[::1]:4317").unwrap();
        assert_eq!((t.host.as_str(), t.port), ("::1", 4317));
        assert_eq!(t.display, "http://[::1]:4317");
    }

    /// A missing port defaults to the scheme port (80/443), matching what
    /// the tonic exporter will connect to — never the OTLP default 4317,
    /// which only applies through the spec-default endpoint string.
    #[cfg(feature = "otlp")]
    #[test]
    fn test_parse_otlp_endpoint_scheme_default_ports() {
        let t = parse_otlp_endpoint("http://collector").unwrap();
        assert_eq!((t.host.as_str(), t.port), ("collector", 80));
        let t = parse_otlp_endpoint("https://collector").unwrap();
        assert_eq!((t.host.as_str(), t.port), ("collector", 443));
        // `http::Uri` accepts a non-numeric port (`port_u16()` returns
        // `None`) and hyper/tonic then connect to the scheme default, so the
        // probe must mirror that rather than reject.
        let t = parse_otlp_endpoint("http://host:notaport").unwrap();
        assert_eq!((t.host.as_str(), t.port), ("host", 80));
    }

    /// tonic rejects endpoints without an http/https scheme, so the probe
    /// must reject them too instead of probing guessed semantics.
    #[cfg(feature = "otlp")]
    #[test]
    fn test_parse_otlp_endpoint_rejects_schemeless() {
        for endpoint in ["collector:9999", "localhost", "127.0.0.1:4317"] {
            let err =
                parse_otlp_endpoint(endpoint).expect_err("schemeless endpoint must be rejected");
            assert!(
                err.contains("scheme"),
                "error for '{endpoint}' should mention the scheme requirement: {err}"
            );
        }
    }

    #[cfg(feature = "otlp")]
    #[test]
    fn test_parse_otlp_endpoint_invalid() {
        assert!(parse_otlp_endpoint("").is_err());
        assert!(parse_otlp_endpoint("http://").is_err());
        assert!(parse_otlp_endpoint("http://:4317").is_err());
        assert!(parse_otlp_endpoint("ftp://collector:4317").is_err());
    }

    #[cfg(feature = "k8s")]
    #[test]
    fn test_cli_auth_login_k8s_namespace_override() {
        let cli = Cli::try_parse_from([
            "memory-mcp",
            "auth",
            "login",
            "--store",
            "k8s-secret",
            "--k8s-namespace",
            "custom-ns",
            "--k8s-secret-name",
            "custom-name",
        ])
        .expect("auth login with k8s flags should parse");
        match cli.command {
            Some(Command::Auth(auth_cmd)) => match auth_cmd.action {
                AuthAction::Login(login_args) => {
                    assert!(matches!(login_args.store, Some(StoreBackend::K8sSecret)));
                    assert_eq!(login_args.k8s_namespace, "custom-ns");
                    assert_eq!(login_args.k8s_secret_name, "custom-name");
                }
                _ => panic!("expected Login action"),
            },
            _ => panic!("expected Auth command"),
        }
    }

    /// Test handler whose only tool call blocks until released, recording
    /// entry and completion so tests can assert drain ordering. When a
    /// mutation registry is supplied, the call holds a registry slot for its
    /// full duration — the same accounting `shielded_mutation_unit` gives
    /// real mutation units.
    #[derive(Clone)]
    struct BlockingToolServer {
        entered: Arc<tokio::sync::Notify>,
        release: Arc<tokio::sync::Notify>,
        done: Arc<std::sync::atomic::AtomicBool>,
        registry: Option<Arc<memory_mcp::types::MutationRegistry>>,
    }

    impl rmcp::ServerHandler for BlockingToolServer {
        async fn call_tool(
            &self,
            _request: rmcp::model::CallToolRequestParams,
            _context: rmcp::service::RequestContext<rmcp::RoleServer>,
        ) -> Result<rmcp::model::CallToolResult, rmcp::ErrorData> {
            let _slot = self.registry.as_ref().map(|r| r.enter());
            self.entered.notify_one();
            self.release.notified().await;
            self.done.store(true, std::sync::atomic::Ordering::SeqCst);
            Ok(rmcp::model::CallToolResult::success(vec![]))
        }
    }

    /// Drive the client half of an in-memory stdio transport: initialize
    /// handshake, then a `tools/call` that blocks inside the handler. Holds
    /// its end of the pipe open, draining server output until close.
    fn spawn_blocking_tool_client(
        client_io: tokio::io::DuplexStream,
    ) -> tokio::task::JoinHandle<()> {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        tokio::spawn(async move {
            let (read_half, mut write_half) = tokio::io::split(client_io);
            let mut lines = BufReader::new(read_half).lines();
            write_half
                .write_all(
                    concat!(
                        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":"#,
                        r#"{"protocolVersion":"2025-03-26","capabilities":{},"#,
                        r#""clientInfo":{"name":"drain-test","version":"0"}}}"#,
                        "\n"
                    )
                    .as_bytes(),
                )
                .await
                .expect("write initialize");
            lines
                .next_line()
                .await
                .expect("read initialize response")
                .expect("initialize response before EOF");
            write_half
                .write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n")
                .await
                .expect("write initialized notification");
            write_half
                .write_all(
                    concat!(
                        r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":"#,
                        r#"{"name":"block","arguments":{}}}"#,
                        "\n"
                    )
                    .as_bytes(),
                )
                .await
                .expect("write tools/call");
            // Hold the connection open (keeping write_half alive) and drain
            // whatever the server sends until it closes the transport.
            while let Ok(Some(_)) = lines.next_line().await {}
            drop(write_half);
        })
    }

    /// Regression for #329 review round 2 (medium): a shutdown signal must
    /// not let `drive_service_until_quit` return while a tool call is still
    /// executing. `run_serve` persists the vector index immediately after
    /// this function returns, so returning early would race persistence
    /// against the in-flight mutation. The blocked handler is released only
    /// 250ms *after* the shutdown fires; the old drop-the-waiting-future
    /// code returned immediately and failed the `done` assertion.
    #[tokio::test]
    async fn shutdown_signal_drains_in_flight_tool_call_before_returning() {
        let (client_io, server_io) = tokio::io::duplex(64 * 1024);

        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let handler = BlockingToolServer {
            entered: Arc::clone(&entered),
            release: Arc::clone(&release),
            done: Arc::clone(&done),
            registry: None,
        };

        let client = spawn_blocking_tool_client(client_io);

        let service = rmcp::serve_server(handler, server_io)
            .await
            .expect("initialize over in-memory transport");

        // Wait until the mutation is provably in flight.
        entered.notified().await;

        // Release the blocked handler 250ms after the signal fires — well
        // inside rmcp's cancellation drain window — from a separate task, so
        // an implementation that returns without draining is caught red-
        // handed by the `done` flag.
        let releaser = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
            release.notify_one();
        });

        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
        shutdown_tx.send(()).expect("send shutdown signal");
        drive_service_until_quit(service, async {
            let _ = shutdown_rx.await;
        })
        .await
        .expect("drive_service_until_quit");

        assert!(
            done.load(std::sync::atomic::Ordering::SeqCst),
            "service returned before the in-flight tool call completed — \
             index persistence would race the mutation"
        );
        releaser.await.expect("releaser task");
        client.abort();
    }

    /// Regression for #329 review round 3 (medium): rmcp's awaited
    /// cancellation drain is bounded (~2s in rmcp 1.8) and its handler tasks
    /// are detached, so a mutation blocked *beyond* that window outlives
    /// `waiting()` — the round-2 test above releases at 250ms, inside the
    /// window, and cannot exercise this boundary. Here the handler holds a
    /// mutation-registry slot (the same accounting `shielded_mutation_unit`
    /// gives real units) and stays blocked until 4s after the shutdown
    /// signal, proving:
    ///
    /// 1. the transport drain returns first with the mutation still
    ///    executing (`done` is false) — exactly the state in which
    ///    `run_serve` previously began stamping the index, and
    /// 2. the registry drain — the gate `run_serve` now applies before
    ///    persistence — does not pass until the mutation completes, with no
    ///    2s ceiling.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn shutdown_awaits_mutation_blocked_beyond_rmcp_drain_window() {
        let (client_io, server_io) = tokio::io::duplex(64 * 1024);

        let registry = Arc::new(memory_mcp::types::MutationRegistry::default());
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let handler = BlockingToolServer {
            entered: Arc::clone(&entered),
            release: Arc::clone(&release),
            done: Arc::clone(&done),
            registry: Some(Arc::clone(&registry)),
        };

        let client = spawn_blocking_tool_client(client_io);

        let service = rmcp::serve_server(handler, server_io)
            .await
            .expect("initialize over in-memory transport");

        // Wait until the mutation is provably in flight and registered.
        entered.notified().await;
        assert_eq!(registry.in_flight(), 1, "handler must hold its slot");

        // Release the blocked handler 4s after the signal fires — well
        // *outside* rmcp's ~2s cancellation drain window.
        let releaser = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(4)).await;
            release.notify_one();
        });

        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
        shutdown_tx.send(()).expect("send shutdown signal");
        drive_service_until_quit(service, async {
            let _ = shutdown_rx.await;
        })
        .await
        .expect("drive_service_until_quit");

        // The transport gave up before the mutation finished. If this fires,
        // rmcp's bounded-drain behavior changed (it now awaits handlers
        // fully) — re-evaluate whether the registry gate and this regression
        // still model reality.
        assert!(
            !done.load(std::sync::atomic::Ordering::SeqCst),
            "expected the transport drain to return while the mutation was \
             still blocked (rmcp's ~2s drain ceiling); it drained fully instead"
        );
        assert_eq!(
            registry.in_flight(),
            1,
            "the abandoned mutation must still hold its registry slot"
        );

        // The gate `run_serve` applies before persistence: it must wait out
        // the mutation rather than inherit the transport's ceiling.
        assert!(
            registry
                .drained_within(std::time::Duration::from_secs(30))
                .await,
            "registry drain must resolve once the mutation completes"
        );
        assert!(
            done.load(std::sync::atomic::Ordering::SeqCst),
            "registry drained before the mutation completed — index \
             persistence would still race the mutation"
        );
        releaser.await.expect("releaser task");
        client.abort();
    }
}
