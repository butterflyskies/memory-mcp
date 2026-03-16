use std::{path::PathBuf, sync::Arc};

use anyhow::Context;
use clap::Parser;
use rmcp::transport::streamable_http_server::{
    session::local::LocalSessionManager, StreamableHttpServerConfig, StreamableHttpService,
};
use tokio_util::sync::CancellationToken;
use tracing::info;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

mod auth;
mod embedding;
mod error;
mod index;
mod repo;
mod server;
mod types;

use auth::AuthProvider;
use embedding::EmbeddingEngine;
use index::VectorIndex;
use repo::MemoryRepo;
use server::MemoryServer;
use types::AppState;

// ---------------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------------

#[derive(Parser)]
#[command(name = "memory-mcp", about = "Semantic memory MCP server")]
struct Cli {
    /// Address to bind the HTTP server to.
    #[arg(long, default_value = "127.0.0.1:8080", env = "MEMORY_MCP_BIND")]
    bind: String,

    /// Path to the git-backed memory repository.
    #[arg(long, default_value = "~/.memory-mcp", env = "MEMORY_MCP_REPO_PATH")]
    repo_path: String,

    /// Embedding model name. Defaults to BGESmallENV15.
    #[arg(
        long,
        default_value = "BGESmallENV15",
        env = "MEMORY_MCP_EMBEDDING_MODEL"
    )]
    embedding_model: String,

    /// URL path at which the MCP service is mounted.
    #[arg(long, default_value = "/mcp", env = "MEMORY_MCP_PATH")]
    mcp_path: String,
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Tracing goes to stderr only — stdout must remain clean for MCP.
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".to_string().into()),
        )
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
        .init();

    let cli = Cli::parse();

    // Expand `~` in repo_path, failing loudly if HOME is not set and the
    // path requires it (i.e. the user did not provide --repo-path explicitly).
    let repo_path = expand_tilde(&cli.repo_path)?;
    info!("repo path: {}", repo_path.display());

    // Initialise subsystems.
    let repo = MemoryRepo::init_or_open(&repo_path)
        .with_context(|| format!("failed to open/init repo at {}", repo_path.display()))?;

    let embedding = EmbeddingEngine::new(&cli.embedding_model)
        .with_context(|| format!("failed to init embedding model '{}'", cli.embedding_model))?;

    let dimensions = embedding.dimensions();

    // Attempt to load existing index; create fresh if missing.
    let index_path = repo_path.join(".memory-mcp-index").join("index.usearch");
    let index = if index_path.exists() {
        VectorIndex::load(&index_path).unwrap_or_else(|e| {
            tracing::warn!("could not load index ({}), creating fresh", e);
            VectorIndex::new(dimensions).expect("failed to create vector index")
        })
    } else {
        VectorIndex::new(dimensions).context("failed to create vector index")?
    };

    let auth = AuthProvider::new();

    let state = Arc::new(AppState {
        repo: Arc::new(repo),
        embedding,
        index,
        auth,
    });

    // Keep a reference for post-shutdown index persistence.
    let state_for_shutdown = Arc::clone(&state);

    // Build the MCP service.
    let ct = CancellationToken::new();
    let ct_child = ct.child_token();

    let service = StreamableHttpService::new(
        move || Ok(MemoryServer::new(Arc::clone(&state))),
        LocalSessionManager::default().into(),
        StreamableHttpServerConfig {
            cancellation_token: ct_child,
            ..Default::default()
        },
    );

    let mcp_path = cli.mcp_path.clone();
    let router = axum::Router::new().nest_service(&mcp_path, service);

    let listener = tokio::net::TcpListener::bind(&cli.bind)
        .await
        .with_context(|| format!("failed to bind to {}", cli.bind))?;

    info!("listening on {} (MCP at {})", cli.bind, cli.mcp_path);

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

    // Persist the vector index so the next startup can skip a full reindex.
    let index_dir = repo_path.join(".memory-mcp-index");
    std::fs::create_dir_all(&index_dir)
        .with_context(|| format!("failed to create index dir {}", index_dir.display()))?;
    let index_path = index_dir.join("index.usearch");
    if let Err(e) = state_for_shutdown.index.save(&index_path) {
        tracing::warn!("failed to persist vector index on shutdown: {}", e);
    } else {
        info!("vector index saved to {}", index_path.display());
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn expand_tilde(path: &str) -> anyhow::Result<PathBuf> {
    if let Some(rest) = path.strip_prefix("~/") {
        let home = std::env::var("HOME").context(
            "HOME environment variable is not set; \
             please provide --repo-path explicitly or set HOME",
        )?;
        Ok(PathBuf::from(home).join(rest))
    } else if path == "~" {
        let home = std::env::var("HOME").context(
            "HOME environment variable is not set; \
             please provide --repo-path explicitly or set HOME",
        )?;
        Ok(PathBuf::from(home))
    } else {
        Ok(PathBuf::from(path))
    }
}
