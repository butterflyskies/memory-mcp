//! Integration tests for the health and info HTTP endpoints (`/readyz`, `/version`).
//!
//! The healthy-path test spins up the real binary. The degraded-path test
//! constructs its own axum server with a controlled `AppState` so we can
//! inject subsystem failures without modifying the production binary.

use std::sync::Arc;
use std::time::Duration;

use memory_mcp::auth::AuthProvider;
use memory_mcp::embedding::EmbeddingBackend;
use memory_mcp::error::MemoryError;
use memory_mcp::health::{readyz_handler, HealthRegistry};
use memory_mcp::index::InMemoryStore;
use memory_mcp::repo::MemoryRepo;
use memory_mcp::types::AppState;

// ---------------------------------------------------------------------------
// Test helpers
// ---------------------------------------------------------------------------

/// Start the real server binary with a fresh repo and wait for /healthz.
async fn start_server(extra_args: &[&str]) -> (tokio::process::Child, u16, tempfile::TempDir) {
    let tmp = tempfile::tempdir().expect("failed to create temp dir");
    let repo_path = tmp.path().to_str().expect("non-utf8 temp path");
    let port = portpicker::pick_unused_port().expect("no free port");
    let bind = format!("127.0.0.1:{port}");

    let mut cmd = tokio::process::Command::new(env!("CARGO_BIN_EXE_memory-mcp"));
    cmd.args(["serve", "--bind", &bind, "--repo-path", repo_path])
        .args(extra_args)
        .kill_on_drop(true)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());

    let child = cmd.spawn().expect("failed to start memory-mcp");

    let client = reqwest::Client::new();
    let healthz_url = format!("http://{bind}/healthz");
    for _ in 0..100 {
        if client.get(&healthz_url).send().await.is_ok() {
            return (child, port, tmp);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("server did not become ready within 10s");
}

/// A stub embedding backend — used for AppState construction in the degraded
/// test. Health is controlled via the HealthRegistry, not this backend.
struct StubEmbeddingBackend;

#[async_trait::async_trait]
impl EmbeddingBackend for StubEmbeddingBackend {
    async fn embed(&self, _texts: &[String]) -> Result<Vec<Vec<f32>>, MemoryError> {
        Ok(vec![])
    }

    fn dimensions(&self) -> usize {
        4
    }
}

/// Build a test server with a degraded `AppState` and return its base URL.
///
/// The `/readyz` handler reads directly from the `HealthRegistry`, so we
/// control the reported state by calling `report_err` on reporters before
/// the server starts — no need to inject failures through the subsystems.
async fn start_degraded_server() -> (u16, tempfile::TempDir, tokio::task::JoinHandle<()>) {
    let tmp = tempfile::tempdir().expect("failed to create temp dir");
    let repo = MemoryRepo::init_or_open(tmp.path(), None).expect("repo init");

    // Build a registry and mark the embedding subsystem as degraded.
    // git_repo and vector_index are left at "not yet checked" (also unhealthy),
    // but we explicitly mark embedding to test the reason field.
    let registry = HealthRegistry::new();
    registry.embedding.report_err("embed failed");

    let state = Arc::new(AppState::new(
        Arc::new(repo),
        "main".to_string(),
        Box::new(StubEmbeddingBackend),
        Box::new(InMemoryStore::new(4)),
        AuthProvider::new(),
        registry,
    ));

    let router = axum::Router::new()
        .route(
            "/healthz",
            axum::routing::get(|| async {
                axum::response::Json(serde_json::json!({"status": "ok"}))
            }),
        )
        .route("/readyz", axum::routing::get(readyz_handler))
        .with_state(state);

    let port = portpicker::pick_unused_port().expect("no free port");
    let listener = tokio::net::TcpListener::bind(format!("127.0.0.1:{port}"))
        .await
        .expect("bind");

    let handle = tokio::spawn(async move {
        axum::serve(listener, router).await.ok();
    });

    // Wait for the server to accept connections.
    let client = reqwest::Client::new();
    for _ in 0..50 {
        if client
            .get(format!("http://127.0.0.1:{port}/healthz"))
            .send()
            .await
            .is_ok()
        {
            return (port, tmp, handle);
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("degraded test server did not start within 2.5s");
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Healthy server (real binary): /readyz returns 200 with all subsystems "up".
#[tokio::test]
async fn readyz_healthy_server_returns_200_ready() {
    let (mut child, port, _tmp) = start_server(&[]).await;

    let client = reqwest::Client::new();
    let resp = client
        .get(format!("http://127.0.0.1:{port}/readyz"))
        .send()
        .await
        .expect("readyz request should succeed");

    assert_eq!(
        resp.status().as_u16(),
        200,
        "healthy server should return 200"
    );

    let body: serde_json::Value = resp.json().await.expect("body should be valid JSON");

    assert_eq!(body["status"], "ready");

    for check in &["git_repo", "embedding", "vector_index"] {
        assert_eq!(
            body["checks"][check]["status"], "up",
            "check '{check}' should be 'up', got: {body}"
        );
        assert!(
            body["checks"][check]["reason"].is_null(),
            "check '{check}' should have no reason when up"
        );
    }

    child.kill().await.ok();
}

/// Degraded server (test-controlled): /readyz returns 503 with failing subsystems.
///
/// The HealthRegistry is the source of truth — we mark subsystems as degraded
/// directly via `report_err`, bypassing any subsystem implementation details.
#[tokio::test]
async fn readyz_degraded_server_returns_503_not_ready() {
    let (port, _tmp, handle) = start_degraded_server().await;

    let client = reqwest::Client::new();
    let resp = client
        .get(format!("http://127.0.0.1:{port}/readyz"))
        .send()
        .await
        .expect("readyz request should succeed");

    assert_eq!(
        resp.status().as_u16(),
        503,
        "degraded server should return 503"
    );

    let body: serde_json::Value = resp.json().await.expect("body should be valid JSON");

    assert_eq!(body["status"], "not_ready");

    // Embedding was explicitly marked down with a reason.
    assert_eq!(body["checks"]["embedding"]["status"], "down");
    assert_eq!(body["checks"]["embedding"]["reason"], "embed failed");

    // Git and vector_index were never reported (still "not yet checked").
    assert_eq!(body["checks"]["git_repo"]["status"], "down");
    assert_eq!(body["checks"]["vector_index"]["status"], "down");

    handle.abort();
}

/// `/version` returns 200 with the crate version from Cargo.toml.
#[tokio::test]
async fn version_returns_cargo_pkg_version() {
    let (mut child, port, _tmp) = start_server(&[]).await;

    let client = reqwest::Client::new();
    let resp = client
        .get(format!("http://127.0.0.1:{port}/version"))
        .send()
        .await
        .expect("version request should succeed");

    assert_eq!(resp.status().as_u16(), 200);

    let body: serde_json::Value = resp.json().await.expect("body should be valid JSON");
    assert_eq!(body["version"], env!("CARGO_PKG_VERSION"));

    child.kill().await.ok();
}
