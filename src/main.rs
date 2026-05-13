//! Thin CLI wrapper around the [`memory_mcp`] library crate.

use std::{path::PathBuf, sync::Arc};

use anyhow::Context;
use clap::{Args, Parser, Subcommand};
use mcp_session::{BoundedSessionManager, SessionConfig};
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use tokio_util::sync::CancellationToken;
use tracing::info;
use tracing::Instrument;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

// The SdkTracerProvider type is used in the otlp feature only.
#[cfg(feature = "otlp")]
use opentelemetry_sdk::trace::SdkTracerProvider as OtlpProvider;

use memory_mcp::auth::{self, AuthProvider, StoreBackend};
use memory_mcp::embedding::{CandleEmbeddingEngine, EmbeddingBackend, MODEL_ID};
use memory_mcp::health::{healthz_handler, readyz_handler, version_handler, HealthRegistry};
use memory_mcp::index::{UsearchStore, VectorStore};
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

#[derive(Args)]
struct ServeArgs {
    /// Address to bind the HTTP server to.
    #[arg(long, default_value = "127.0.0.1:8080", env = "MEMORY_MCP_BIND")]
    bind: String,

    /// Path to the git-backed memory repository.
    #[arg(long, default_value = "~/.memory-mcp", env = "MEMORY_MCP_REPO_PATH")]
    repo_path: String,

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

    /// Additional hostname to accept in the HTTP Host header. Required when
    /// the server is accessed via a reverse proxy or gateway (e.g.
    /// `memory-mcp.svc.echoes`). Can be specified multiple times.
    #[arg(long, env = "MEMORY_MCP_ALLOWED_HOST")]
    allowed_host: Vec<String>,

    /// Include remote sync health in readiness checks. When enabled, push/pull
    /// failures will cause /readyz to return 503.
    #[arg(long, default_value_t = false, env = "MEMORY_MCP_REQUIRE_REMOTE_SYNC")]
    require_remote_sync: bool,

    /// Seconds after which a subsystem with no successful operations is considered
    /// stale. Set to 0 to disable staleness detection (default).
    #[arg(long, default_value_t = 0, env = "MEMORY_MCP_HEALTH_STALE_SECS")]
    health_stale_secs: u64,

    #[command(flatten)]
    embed: EmbedArgs,

    /// Enable OTLP span export. The server will crash on startup if the
    /// collector is unreachable. Use --otlp-optional for graceful fallback.
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

/// Initialise tracing for the serve command. If `--otlp-required` or
/// `--otlp-optional` is set, activates OTLP export. Otherwise uses fmt-only
/// (passive — the feature is compiled in but not activated).
#[cfg(feature = "otlp")]
fn init_tracing_for_serve(args: &ServeArgs) -> Option<OtlpProvider> {
    if !args.otlp_required && !args.otlp_optional {
        init_tracing_fmt_only();
        return None;
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
            if args.otlp_optional {
                tracing_subscriber::registry()
                    .with(filter)
                    .with(fmt_layer)
                    .init();
                tracing::warn!(
                    error = %e,
                    "OTLP exporter init failed — continuing with fmt-only tracing (--otlp-optional is set)"
                );
                None
            } else {
                eprintln!(
                    "error: OTLP exporter init failed: {e}\n\
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
                    #[cfg(feature = "otlp")]
                    let _otlp_provider = init_tracing_for_serve(&args);
                    let result = run_serve(args).await;
                    #[cfg(feature = "otlp")]
                    if let Some(provider) = _otlp_provider {
                        let _ = provider.shutdown();
                    }
                    result?;
                }
                _ => unreachable!(),
            }
        }
        Some(Command::Serve(args)) => {
            #[cfg(feature = "otlp")]
            let _otlp_provider = init_tracing_for_serve(&args);
            let result = run_serve(args).await;
            #[cfg(feature = "otlp")]
            if let Some(provider) = _otlp_provider {
                let _ = provider.shutdown();
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
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Server startup
// ---------------------------------------------------------------------------

/// Start and run the MCP HTTP server with the provided arguments.
async fn run_serve(args: ServeArgs) -> anyhow::Result<()> {
    // Validate branch name early to prevent ref injection.
    validate_branch_name(&args.branch).context("invalid --branch value")?;

    // Expand `~` in repo_path, failing loudly if HOME is not set and the
    // path requires it (i.e. the user did not provide --repo-path explicitly).
    let repo_path = expand_path(&args.repo_path)?;
    info!("repo path: {}", repo_path.display());

    // Filter out empty string to treat MEMORY_MCP_REMOTE_URL="" as unset.
    let remote_url = args.remote_url.filter(|u| !u.is_empty());

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

    // Attempt to load the scoped index; create fresh if missing or corrupt.
    let index_dir = repo_path.join(".memory-mcp-index");

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

    // Load the persisted index and check freshness against repo HEAD.
    // If the SHA doesn't match, discard the loaded index entirely and start
    // fresh — this prevents ghost entries from deleted memories lingering.
    let mut index: Box<dyn VectorStore> = Box::new(
        UsearchStore::load_with_reporter(&index_dir, dimensions, health.vector_index.clone())
            .unwrap_or_else(|e| {
                tracing::warn!("could not load index ({}), creating fresh", e);
                UsearchStore::new_with_reporter(dimensions, health.vector_index.clone())
                    .expect("failed to create index")
            }),
    );

    let head_sha = repo.head_sha().await;
    let needs_reindex = head_sha != index.commit_sha();
    // Track whether the reindex (if it ran) completed without errors.
    // Used below to gate startup report_ok for embedding and vector_index.
    let reindex_ok;
    if needs_reindex {
        info!(
            head = ?head_sha,
            index = ?index.commit_sha(),
            "index SHA does not match repo HEAD — rebuilding from scratch"
        );
        index = Box::new(
            UsearchStore::new_with_reporter(dimensions, health.vector_index.clone())
                .expect("failed to create index"),
        );

        reindex_ok =
            match memory_mcp::server::full_reindex(&repo, embedding.as_ref(), index.as_ref())
                .instrument(tracing::info_span!("startup.full_reindex"))
                .await
            {
                Ok(stats) => {
                    info!(
                        added = stats.added,
                        errors = stats.errors,
                        "startup reindex complete"
                    );
                    if stats.added > 0 || stats.errors == 0 {
                        if stats.errors > 0 {
                            tracing::warn!(
                            added = stats.added,
                            errors = stats.errors,
                            "startup reindex partially failed — some memories may not be searchable"
                        );
                        }
                        if let Some(sha) = &head_sha {
                            index.set_commit_sha(Some(sha));
                        }
                        stats.errors == 0
                    } else {
                        tracing::warn!(
                            errors = stats.errors,
                            "all embeds failed — SHA not stamped, will retry on next startup"
                        );
                        false
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        "startup reindex failed — SHA not stamped, will retry on next startup"
                    );
                    false
                }
            };
    } else {
        tracing::debug!(sha = ?head_sha, "index SHA matches repo HEAD — skipping reindex");
        reindex_ok = true;
    }

    let auth = AuthProvider::new();

    // When --require-remote-sync is set, perform an initial pull so the sync
    // reporter starts with a known state (and the local repo is up-to-date).
    if args.require_remote_sync && remote_url.is_some() {
        info!("--require-remote-sync: performing initial pull");
        match repo.pull(&auth, &args.branch).await {
            Ok(result) => {
                info!(?result, "initial pull completed");
            }
            Err(e) => {
                tracing::warn!(error = %e, "initial pull failed — sync reporter will show degraded");
            }
        }
    }

    // Mark git as healthy — if we reached this point, git init/open succeeded.
    health.git.report_ok();
    // Only mark embedding and vector_index healthy if the reindex succeeded or
    // was skipped (SHA matched). If the reindex had errors, the subsystems have
    // already reported their own state via their reporters.
    if reindex_ok {
        health.embedding.report_ok();
        health.vector_index.report_ok();
    }

    let state = Arc::new(AppState::new(
        repo,
        args.branch.clone(),
        embedding,
        index,
        auth,
        health,
    ));

    // Keep a reference for post-shutdown index persistence.
    let state_for_shutdown = Arc::clone(&state);

    // Build the MCP service.
    let ct = CancellationToken::new();
    let ct_child = ct.child_token();

    // SessionConfig and StreamableHttpServerConfig are #[non_exhaustive] in
    // rmcp 1.4+, so struct literal syntax is unavailable from external crates.
    // Default + field mutation is the intended pattern (see mcp-session#11).
    #[allow(clippy::field_reassign_with_default)]
    let service = StreamableHttpService::new(
        move || Ok(MemoryServer::new(Arc::clone(&state))),
        Arc::new({
            let mut session_config = SessionConfig::default();
            session_config.keep_alive = Some(std::time::Duration::from_secs(4 * 60 * 60));
            let mgr = BoundedSessionManager::new(session_config, args.max_sessions);
            if args.session_rate_limit > 0 && args.session_rate_window_secs > 0 {
                mgr.with_rate_limit(
                    args.session_rate_limit,
                    std::time::Duration::from_secs(args.session_rate_window_secs),
                )
            } else {
                mgr
            }
        }),
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
        .with_state(Arc::clone(&state_for_shutdown))
        .nest_service(&mcp_path, service);

    let listener = tokio::net::TcpListener::bind(&args.bind)
        .await
        .with_context(|| format!("failed to bind to {}", args.bind))?;

    info!("listening on {} (MCP at {})", args.bind, args.mcp_path);

    axum::serve(listener, router)
        .with_graceful_shutdown(async move {
            tokio::signal::ctrl_c()
                .await
                .expect("failed to listen for ctrl-c");
            info!("shutdown signal received");
            ct.cancel();
        })
        .await
        .context("server error")?;

    // Persist the scoped vector index so the next startup can skip a full reindex.
    std::fs::create_dir_all(&index_dir)
        .with_context(|| format!("failed to create index dir {}", index_dir.display()))?;
    if let Some(sha) = state_for_shutdown.repo.head_sha().await {
        state_for_shutdown.index.set_commit_sha(Some(&sha));
    }
    if let Err(e) = state_for_shutdown.index.save(&index_dir) {
        tracing::warn!("failed to persist vector index on shutdown: {}", e);
    } else {
        info!("vector index saved to {}", index_dir.display());
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
    match path.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with('/') => {
            let home = dirs::home_dir().ok_or_else(|| {
                anyhow::anyhow!(
                    "could not expand '~': home directory could not be determined; \
                     please provide --repo-path explicitly or set HOME"
                )
            })?;
            Ok(home.join(rest.strip_prefix('/').unwrap_or(rest)))
        }
        Some(_) => anyhow::bail!(
            "~user path expansion is not supported; \
             please use an absolute path or ~/..."
        ),
        None => Ok(PathBuf::from(path)),
    }
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
}
