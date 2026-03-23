//! Integration tests for [`BoundedSessionManager`] session-limit enforcement
//! and idle-timeout behaviour.
//!
//! Each test binds to a random port on 127.0.0.1, spawns an in-process axum
//! server backed by a minimal MCP handler, and drives it via raw HTTP requests
//! using [`reqwest`].

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use memory_mcp::session::BoundedSessionManager;
use rmcp::transport::streamable_http_server::{
    session::local::SessionConfig, StreamableHttpServerConfig, StreamableHttpService,
};
use rmcp::ServerHandler;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

// ---------------------------------------------------------------------------
// Minimal MCP handler — no tools, no repo, just enough to accept `initialize`.
// ---------------------------------------------------------------------------

/// A no-op MCP server used only for exercising session lifecycle.
#[derive(Clone)]
struct NoopServer;

impl ServerHandler for NoopServer {}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Build a test axum [`Router`] backed by a [`BoundedSessionManager`].
///
/// Returns the router and a [`CancellationToken`] the caller can cancel to
/// stop background workers.
fn build_router(session_config: SessionConfig, max_sessions: usize) -> (Router, CancellationToken) {
    let ct = CancellationToken::new();
    let ct_child = ct.child_token();

    let service = StreamableHttpService::new(
        || Ok(NoopServer),
        Arc::new(BoundedSessionManager::new(session_config, max_sessions)),
        StreamableHttpServerConfig {
            cancellation_token: ct_child,
            ..Default::default()
        },
    );

    let router = Router::new().nest_service("/mcp", service);
    (router, ct)
}

/// Spawn a server on a random port and return the base URL and cancellation
/// token. The server task runs until the token is cancelled.
async fn spawn_server(
    session_config: SessionConfig,
    max_sessions: usize,
) -> (String, CancellationToken) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind to random port");
    let addr = listener.local_addr().expect("get local addr");
    let base_url = format!("http://{}", addr);

    let (router, ct) = build_router(session_config, max_sessions);
    let ct_child = ct.child_token();

    tokio::spawn(async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(ct_child.cancelled_owned())
            .await
            .expect("server error");
    });

    (base_url, ct)
}

/// JSON-RPC `initialize` body (MCP 2025-03-26).
fn initialize_body() -> serde_json::Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-03-26",
            "capabilities": {},
            "clientInfo": {"name": "test", "version": "0.1.0"}
        }
    })
}

/// JSON-RPC `tools/list` body.
fn tools_list_body() -> serde_json::Value {
    serde_json::json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}})
}

/// POST `body` to `url/mcp`.
///
/// Returns `(status, Mcp-Session-Id header value if present)`.
async fn post_mcp(
    client: &reqwest::Client,
    base_url: &str,
    session_id: Option<&str>,
    body: &serde_json::Value,
) -> (reqwest::StatusCode, Option<String>) {
    let mut builder = client
        .post(format!("{}/mcp", base_url))
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .json(body);

    if let Some(sid) = session_id {
        builder = builder.header("Mcp-Session-Id", sid);
    }

    let resp = builder.send().await.expect("HTTP request succeeded");
    let status = resp.status();
    let returned_sid = resp
        .headers()
        .get("Mcp-Session-Id")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    (status, returned_sid)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// A session that has been idle past its keep_alive duration must be rejected.
#[tokio::test]
async fn test_session_idle_timeout() {
    let config = SessionConfig {
        keep_alive: Some(Duration::from_secs(1)),
        ..Default::default()
    };
    let (base_url, _ct) = spawn_server(config, 10).await;
    let client = reqwest::Client::new();

    // Create a session.
    let (status, session_id) = post_mcp(&client, &base_url, None, &initialize_body()).await;
    assert!(
        status.is_success(),
        "initialize should succeed, got {status}"
    );
    let session_id = session_id.expect("response must carry Mcp-Session-Id");

    // Wait longer than the keep_alive timeout.
    tokio::time::sleep(Duration::from_secs(3)).await;

    // The session should have expired; the server must not return 200.
    let (status, _) = post_mcp(&client, &base_url, Some(&session_id), &tools_list_body()).await;
    assert!(
        !status.is_success(),
        "expired session should be rejected, got {status}"
    );
}

/// When `max_sessions` is exceeded the oldest session must be evicted.
#[tokio::test]
async fn test_max_sessions_eviction() {
    let config = SessionConfig {
        // Long keep_alive so sessions don't expire on their own.
        keep_alive: Some(Duration::from_secs(300)),
        ..Default::default()
    };
    let (base_url, _ct) = spawn_server(config, 2).await;
    let client = reqwest::Client::new();

    // Create session 1.
    let (status, sid1) = post_mcp(&client, &base_url, None, &initialize_body()).await;
    assert!(status.is_success(), "session 1 initialize failed: {status}");
    let sid1 = sid1.expect("session 1 must have Mcp-Session-Id");

    // Create session 2.
    let (status, sid2) = post_mcp(&client, &base_url, None, &initialize_body()).await;
    assert!(status.is_success(), "session 2 initialize failed: {status}");
    let sid2 = sid2.expect("session 2 must have Mcp-Session-Id");

    // Create session 3 — this should evict session 1.
    let (status, sid3) = post_mcp(&client, &base_url, None, &initialize_body()).await;
    assert!(status.is_success(), "session 3 initialize failed: {status}");
    let sid3 = sid3.expect("session 3 must have Mcp-Session-Id");

    // Session 1 should be gone.
    let (status, _) = post_mcp(&client, &base_url, Some(&sid1), &tools_list_body()).await;
    assert!(
        !status.is_success(),
        "evicted session 1 should be rejected, got {status}"
    );

    // Session 2 should still be alive (only session 1 was evicted).
    let (status, _) = post_mcp(&client, &base_url, Some(&sid2), &tools_list_body()).await;
    assert!(
        status.is_success(),
        "session 2 should still be active after session 3 creation, got {status}"
    );

    // Session 3 should still be alive.
    let (status, _) = post_mcp(&client, &base_url, Some(&sid3), &tools_list_body()).await;
    assert!(
        status.is_success(),
        "session 3 should still be active, got {status}"
    );
}

/// Expired sessions (removed from inner via keep_alive) must not consume a
/// capacity slot. Create one session, let it expire, then fill to capacity —
/// proving the expired slot was reclaimed.
///
/// Note: rmcp's `keep_alive` is an absolute timer from session creation, not
/// reset-on-activity. We stagger creation to control which sessions are alive.
#[tokio::test]
async fn test_expired_session_does_not_consume_capacity() {
    let config = SessionConfig {
        keep_alive: Some(Duration::from_secs(1)),
        ..Default::default()
    };
    // Two-session capacity.
    let (base_url, _ct) = spawn_server(config, 2).await;
    let client = reqwest::Client::new();

    // Create session 1 — will expire after ~1 second.
    let (status, sid1) = post_mcp(&client, &base_url, None, &initialize_body()).await;
    assert!(status.is_success(), "session 1 initialize failed: {status}");
    let sid1 = sid1.expect("session 1 must have Mcp-Session-Id");

    // Wait for session 1 to expire.
    tokio::time::sleep(Duration::from_secs(3)).await;

    // Session 1 should be expired.
    let (status, _) = post_mcp(&client, &base_url, Some(&sid1), &tools_list_body()).await;
    assert!(
        !status.is_success(),
        "session 1 should have expired, got {status}"
    );

    // Now create sessions 2 and 3. Both should succeed because the expired
    // session 1 does not consume a capacity slot (inner.sessions is the
    // authority, not creation_order).
    let (status, sid2) = post_mcp(&client, &base_url, None, &initialize_body()).await;
    assert!(
        status.is_success(),
        "session 2 should succeed after expiry freed a slot, got {status}"
    );
    let sid2 = sid2.expect("session 2 must have Mcp-Session-Id");

    let (status, sid3) = post_mcp(&client, &base_url, None, &initialize_body()).await;
    assert!(
        status.is_success(),
        "session 3 should succeed (capacity 2, slot freed by expiry), got {status}"
    );
    let sid3 = sid3.expect("session 3 must have Mcp-Session-Id");

    // Both new sessions should be alive.
    let (status, _) = post_mcp(&client, &base_url, Some(&sid2), &tools_list_body()).await;
    assert!(
        status.is_success(),
        "session 2 should be active, got {status}"
    );

    let (status, _) = post_mcp(&client, &base_url, Some(&sid3), &tools_list_body()).await;
    assert!(
        status.is_success(),
        "session 3 should be active, got {status}"
    );
}
