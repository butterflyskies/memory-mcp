//! Integration tests for the `--allowed-host` DNS rebinding protection.
//!
//! These tests build the full service stack in-process and call it via
//! `tower::ServiceExt::oneshot` — no subprocess or port allocation needed.

mod common;

use axum::body::Body;
use http::{header::HOST, Request};
use tower::ServiceExt as _;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Build a GET /mcp request with the given `Host` header value.
fn mcp_request(host: &str) -> Request<Body> {
    Request::builder()
        .method("GET")
        .uri("/mcp")
        .header(HOST, host)
        .body(Body::empty())
        .expect("valid request")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// A request to /mcp with a default localhost Host header is not rejected with 403.
///
/// rmcp accepts `127.0.0.1` by default. The response may be 405 or 400
/// (no MCP headers), but NOT 403 — the host check passed.
#[tokio::test]
async fn request_with_default_localhost_host_is_accepted() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let state = common::build_healthy_state(&tmp);
    // Use the default allowed_hosts list (localhost, 127.0.0.1, ::1).
    let router = common::build_test_router(
        state,
        vec![
            "localhost".to_string(),
            "127.0.0.1".to_string(),
            "::1".to_string(),
        ],
    );

    let resp = router
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/mcp")
                .header(HOST, "127.0.0.1")
                .body(Body::empty())
                .expect("valid request"),
        )
        .await
        .expect("service call should not fail");

    assert_ne!(
        resp.status().as_u16(),
        403,
        "localhost should not be rejected"
    );
}

/// A request with an unknown Host header is rejected with 403.
#[tokio::test]
async fn request_with_unknown_host_is_rejected() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let state = common::build_healthy_state(&tmp);
    let router = common::build_test_router(
        state,
        vec![
            "localhost".to_string(),
            "127.0.0.1".to_string(),
            "::1".to_string(),
        ],
    );

    let resp = router
        .oneshot(mcp_request("evil.attacker.com"))
        .await
        .expect("service call should not fail");

    assert_eq!(
        resp.status().as_u16(),
        403,
        "unknown host should be rejected with 403"
    );
}

/// A request with an explicitly allowed extra host is accepted.
#[tokio::test]
async fn request_with_allowed_host_is_accepted() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let state = common::build_healthy_state(&tmp);
    // Include the default hosts plus our custom one.
    let router = common::build_test_router(
        state,
        vec![
            "localhost".to_string(),
            "127.0.0.1".to_string(),
            "::1".to_string(),
            "memory-mcp.svc.echoes".to_string(),
        ],
    );

    let resp = router
        .oneshot(mcp_request("memory-mcp.svc.echoes"))
        .await
        .expect("service call should not fail");

    assert_ne!(
        resp.status().as_u16(),
        403,
        "explicitly allowed host should not be rejected"
    );
}
