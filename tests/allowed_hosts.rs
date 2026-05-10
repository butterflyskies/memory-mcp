//! Integration tests for the `--allowed-host` DNS rebinding protection.
//!
//! These tests spin up a real HTTP server and verify that rmcp's host
//! validation accepts or rejects requests based on the `Host` header.

use std::time::Duration;

use reqwest::header::{HeaderValue, HOST};

/// Start the server binary with the given args and an empty repo, wait for
/// it to be ready, and return the child process, port, and temp dir handle.
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

    // Wait for the server to be ready by polling /healthz.
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

#[tokio::test]
async fn request_with_default_localhost_host_is_accepted() {
    let (mut child, port, _tmp) = start_server(&[]).await;

    let client = reqwest::Client::new();
    let resp = client
        .get(format!("http://127.0.0.1:{port}/mcp"))
        .send()
        .await
        .expect("request should succeed");

    // /mcp without proper MCP headers returns 405 or 400, but NOT 403 —
    // the point is the host check passed.
    assert_ne!(
        resp.status().as_u16(),
        403,
        "localhost should not be rejected"
    );

    child.kill().await.ok();
}

#[tokio::test]
async fn request_with_unknown_host_is_rejected() {
    let (mut child, port, _tmp) = start_server(&[]).await;

    let client = reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("client");
    let resp = client
        .get(format!("http://127.0.0.1:{port}/mcp"))
        .header(HOST, HeaderValue::from_static("evil.attacker.com"))
        .send()
        .await
        .expect("request should succeed at TCP level");

    assert_eq!(
        resp.status().as_u16(),
        403,
        "unknown host should be rejected with 403"
    );

    child.kill().await.ok();
}

#[tokio::test]
async fn request_with_allowed_host_is_accepted() {
    let (mut child, port, _tmp) = start_server(&["--allowed-host", "memory-mcp.svc.echoes"]).await;

    let client = reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("client");
    let resp = client
        .get(format!("http://127.0.0.1:{port}/mcp"))
        .header(HOST, HeaderValue::from_static("memory-mcp.svc.echoes"))
        .send()
        .await
        .expect("request should succeed");

    // Should pass host check — NOT 403.
    assert_ne!(
        resp.status().as_u16(),
        403,
        "explicitly allowed host should not be rejected"
    );

    child.kill().await.ok();
}
