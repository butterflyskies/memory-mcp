//! Thin CLI wrapper around the [`memory_mcp`] library crate.

use std::{path::PathBuf, sync::Arc};

use anyhow::Context;
use clap::{Args, Parser, Subcommand};
use mcp_session::{BoundedSessionManager, SessionConfig};
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use tokio_util::sync::CancellationToken;
use tracing::info;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

use memory_mcp::auth::{self, AuthProvider, StoreBackend};
use memory_mcp::embedding::{CandleEmbeddingEngine, EmbeddingBackend, MODEL_ID};
use memory_mcp::index::ScopedIndex;
use memory_mcp::repo::MemoryRepo;
use memory_mcp::server::MemoryServer;
use memory_mcp::types::{validate_branch_name, AppState};

// ---------------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------------

#[derive(Parser)]
#[command(
    name = "memory-mcp",
    about = "Semantic memory MCP server for AI agents"
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
}

#[derive(Args)]
struct WarmupArgs {}

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
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".to_string().into()),
        )
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
        .init();

    let cli = Cli::parse();

    match cli.command {
        None => {
            // Re-parse as "memory-mcp serve" so clap's env var resolution runs.
            let cli = Cli::parse_from(["memory-mcp", "serve"]);
            match cli.command {
                Some(Command::Serve(args)) => run_serve(args).await?,
                _ => unreachable!(),
            }
        }
        Some(Command::Serve(args)) => run_serve(args).await?,
        Some(Command::Warmup(args)) => run_warmup(args).await?,
        Some(Command::Auth(auth_cmd)) => match auth_cmd.action {
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
        },
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

    // Initialise subsystems.
    let repo = MemoryRepo::init_or_open(&repo_path, remote_url.as_deref())
        .with_context(|| format!("failed to open/init repo at {}", repo_path.display()))?;

    let embedding: Box<dyn EmbeddingBackend> =
        Box::new(CandleEmbeddingEngine::new().context("failed to init embedding engine")?);

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

    let index = ScopedIndex::load(&index_dir, dimensions).unwrap_or_else(|e| {
        tracing::warn!("could not load scoped index ({}), creating fresh", e);
        ScopedIndex::new(dimensions).expect("failed to create scoped index")
    });

    let auth = AuthProvider::new();

    let state = Arc::new(AppState::new(
        Arc::new(repo),
        args.branch.clone(),
        embedding,
        index,
        auth,
    ));

    // Keep a reference for post-shutdown index persistence.
    let state_for_shutdown = Arc::clone(&state);

    // Build the MCP service.
    let ct = CancellationToken::new();
    let ct_child = ct.child_token();

    let service = StreamableHttpService::new(
        move || Ok(MemoryServer::new(Arc::clone(&state))),
        Arc::new({
            let mgr = BoundedSessionManager::new(
                SessionConfig {
                    keep_alive: Some(std::time::Duration::from_secs(4 * 60 * 60)),
                    ..Default::default()
                },
                args.max_sessions,
            );
            if args.session_rate_limit > 0 && args.session_rate_window_secs > 0 {
                mgr.with_rate_limit(
                    args.session_rate_limit,
                    std::time::Duration::from_secs(args.session_rate_window_secs),
                )
            } else {
                mgr
            }
        }),
        StreamableHttpServerConfig {
            cancellation_token: ct_child,
            ..Default::default()
        },
    );

    let mcp_path = args.mcp_path.clone();
    let router = axum::Router::new()
        .route(
            // Static liveness check. Always returns 200 OK once the process is
            // running. NOTE: a /readyz endpoint performing subsystem health checks
            // (repo accessible, index loaded, embedding model ready) should be
            // added when multi-replica deployments are supported.
            "/healthz",
            axum::routing::get(|| async {
                axum::response::Json(serde_json::json!({"status": "ok"}))
            }),
        )
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
    // TODO: set commit_sha to repo HEAD before saving so the next startup
    // can use SHA-based freshness to skip reindexing unchanged scopes.
    // For now, indexes are always rebuilt from scratch on startup if missing.
    if let Err(e) = state_for_shutdown.index.save(&index_dir) {
        tracing::warn!("failed to persist vector index on shutdown: {}", e);
    } else {
        info!("vector index saved to {}", index_dir.display());
    }

    Ok(())
}

/// Load the embedding model and run a single dummy embed to warm the on-disk
/// model cache, then exit. Intended for use as a Kubernetes init container.
async fn run_warmup(_args: WarmupArgs) -> anyhow::Result<()> {
    info!("warming up embedding model '{}'", MODEL_ID);
    let engine = CandleEmbeddingEngine::new().context("failed to init embedding engine")?;
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

fn expand_path(path: &str) -> anyhow::Result<PathBuf> {
    let expanded = shellexpand::tilde(path);
    if expanded.starts_with('~') {
        anyhow::bail!(
            "could not expand '~': home directory could not be determined; \
             please provide --repo-path explicitly or set HOME"
        );
    }
    Ok(PathBuf::from(expanded.as_ref()))
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
    fn test_parse_nonzero_usize_hundred_is_ok() {
        assert_eq!(parse_nonzero_usize("100").unwrap(), 100);
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
