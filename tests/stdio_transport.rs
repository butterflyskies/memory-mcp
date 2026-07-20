//! stdio transport integration tests (#104, ADR-0040).
//!
//! These are subprocess tests because the behavior under test is process
//! shape: JSON-RPC framing on stdout with zero pollution, tracing on stderr,
//! the single-writer lock across process boundaries, and clean exit on stdin
//! EOF. In-process tower tests (ADR-0036) cannot observe any of that.

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

/// Response budget per stdout line: the embedding model loads synchronously
/// before the server answers `initialize` (same startup budget as the HTTP
/// subprocess tests).
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(120);

/// Kill the child on test panic so failures don't leak server processes.
struct ServerGuard(Child);

impl Drop for ServerGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn spawn_stdio_server(repo: &Path, stderr_to: Stdio) -> ServerGuard {
    let child = Command::new(env!("CARGO_BIN_EXE_memory-mcp"))
        .args([
            "serve",
            "--transport",
            "stdio",
            "--repo-path",
            repo.to_str().expect("utf8 temp path"),
        ])
        // Force verbose tracing: any log line that leaked to stdout would
        // fail the pollution assertions below.
        .env("RUST_LOG", "debug")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(stderr_to)
        .spawn()
        .expect("failed to spawn memory-mcp serve --transport stdio");
    ServerGuard(child)
}

/// Pump child stdout lines over a channel so response waits can time out
/// instead of hanging the test on a wedged server.
fn stdout_lines(child: &mut Child) -> mpsc::Receiver<String> {
    let stdout = child.stdout.take().expect("child stdout piped");
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    rx
}

fn next_line(rx: &mpsc::Receiver<String>) -> String {
    rx.recv_timeout(RESPONSE_TIMEOUT)
        .expect("timed out waiting for a stdout line from the server")
}

fn send(child: &mut Child, msg: &serde_json::Value) {
    let stdin = child.stdin.as_mut().expect("child stdin piped");
    let mut line = msg.to_string();
    line.push('\n');
    stdin
        .write_all(line.as_bytes())
        .expect("write to child stdin");
    stdin.flush().expect("flush child stdin");
}

/// Drive the MCP initialize handshake to completion and return the
/// `initialize` response.
fn initialize(
    child: &mut Child,
    rx: &mpsc::Receiver<String>,
    stderr_path: &Path,
) -> serde_json::Value {
    send(
        child,
        &serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-03-26",
                "capabilities": {},
                "clientInfo": {"name": "stdio-transport-test", "version": "0"}
            }
        }),
    );
    let line = rx.recv_timeout(RESPONSE_TIMEOUT).unwrap_or_else(|e| {
        let stderr = std::fs::read_to_string(stderr_path)
            .unwrap_or_else(|read_err| format!("<failed to read stderr: {read_err}>"));
        panic!("failed waiting for initialize response ({e}); child stderr:\n{stderr}")
    });
    let resp: serde_json::Value = serde_json::from_str(&line).unwrap_or_else(|e| {
        let stderr = std::fs::read_to_string(stderr_path)
            .unwrap_or_else(|read_err| format!("<failed to read stderr: {read_err}>"));
        panic!("initialize response is not valid JSON ({e}): {line}; child stderr:\n{stderr}")
    });
    assert_eq!(resp["id"], 1, "initialize response id: {resp}");
    if resp.get("error").is_some() {
        let stderr = std::fs::read_to_string(stderr_path)
            .unwrap_or_else(|e| format!("<failed to read stderr: {e}>"));
        panic!("initialize must not error: {resp}; child stderr:\n{stderr}");
    }
    send(
        child,
        &serde_json::json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
    );
    resp
}

fn wait_with_timeout(child: &mut Child, timeout: Duration) -> std::process::ExitStatus {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait().expect("try_wait on server") {
            return status;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "server did not exit within {timeout:?}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Full stdio round trip: initialize handshake, tools/list, one recall —
/// every stdout byte is JSON-RPC (zero pollution, verified under
/// RUST_LOG=debug), tracing lands on stderr, and stdin EOF is a clean exit.
#[test]
fn stdio_round_trip_serves_tools_with_clean_stdout() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let stderr_path = tmp.path().join("stderr.log");
    let stderr_file = std::fs::File::create(&stderr_path).expect("create stderr capture");

    let mut server = spawn_stdio_server(tmp.path(), Stdio::from(stderr_file));
    let rx = stdout_lines(&mut server.0);
    let mut transcript: Vec<String> = Vec::new();

    let init_resp = initialize(&mut server.0, &rx, &stderr_path);
    transcript.push(init_resp.to_string());
    assert!(
        init_resp["result"]["serverInfo"]["name"].is_string(),
        "initialize result should carry serverInfo: {init_resp}"
    );

    // tools/list must include the memory tools.
    send(
        &mut server.0,
        &serde_json::json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
    );
    let line = next_line(&rx);
    transcript.push(line.clone());
    let tools_resp: serde_json::Value =
        serde_json::from_str(&line).expect("tools/list response is valid JSON");
    assert_eq!(tools_resp["id"], 2, "tools/list response id: {tools_resp}");
    let tools = tools_resp["result"]["tools"]
        .as_array()
        .unwrap_or_else(|| panic!("tools/list result has a tools array: {tools_resp}"));
    assert!(
        tools.iter().any(|t| t["name"] == "recall"),
        "tools/list should include recall: {tools_resp}"
    );

    // One real tool call end to end: recall exercises the embedding engine
    // and the vector index over the stdio channel.
    send(
        &mut server.0,
        &serde_json::json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {"name": "recall", "arguments": {"query": "stdio transport"}}
        }),
    );
    let line = next_line(&rx);
    transcript.push(line.clone());
    let recall_resp: serde_json::Value =
        serde_json::from_str(&line).expect("recall response is valid JSON");
    assert_eq!(recall_resp["id"], 3, "recall response id: {recall_resp}");
    assert!(
        recall_resp.get("error").is_none(),
        "recall must not error: {recall_resp}"
    );
    assert_ne!(
        recall_resp["result"]["isError"],
        serde_json::Value::Bool(true),
        "recall tool result must not be an error: {recall_resp}"
    );

    // Closing stdin is the normal MCP end-of-session: the server must exit
    // cleanly (this is also when the vector index is persisted).
    drop(server.0.stdin.take());
    let status = wait_with_timeout(&mut server.0, Duration::from_secs(60));
    assert!(status.success(), "clean exit on stdin EOF, got: {status:?}");

    // Drain anything the server wrote during shutdown, then assert zero
    // stdout pollution: every line is a JSON-RPC message, nothing else.
    while let Ok(line) = rx.recv_timeout(Duration::from_secs(5)) {
        transcript.push(line);
    }
    for line in &transcript {
        let msg: serde_json::Value = serde_json::from_str(line)
            .unwrap_or_else(|e| panic!("non-JSON bytes on stdout ({e}): {line:?}"));
        assert_eq!(
            msg["jsonrpc"], "2.0",
            "stdout line is not a JSON-RPC message: {line:?}"
        );
    }

    // The tracing receipt: with RUST_LOG=debug the server logs plenty — and
    // all of it must have landed on stderr, not stdout.
    let stderr = std::fs::read_to_string(&stderr_path).expect("read captured stderr");
    assert!(
        !stderr.trim().is_empty(),
        "expected tracing output on stderr under RUST_LOG=debug"
    );
}

/// Single-writer enforcement (ADR-0040): while one instance holds the lock,
/// a second instance — regardless of transport — exits non-zero, fast, with
/// an error naming the holder's pid.
#[test]
fn second_instance_fails_fast_while_first_holds_the_lock() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let stderr_path = tmp.path().join("first-stderr.log");
    let stderr_file = std::fs::File::create(&stderr_path).expect("stderr capture");

    let mut first = spawn_stdio_server(tmp.path(), Stdio::from(stderr_file));
    let rx = stdout_lines(&mut first.0);

    // Complete the handshake so the first instance is provably up. The lock
    // is acquired before subsystem init, so this wait is conservative.
    initialize(&mut first.0, &rx, &stderr_path);

    // Second instance against the same repo, using the default HTTP
    // transport: the lock applies across transports, and it must fail before
    // the (slow) embedding init or any bind.
    let output = Command::new(env!("CARGO_BIN_EXE_memory-mcp"))
        .args([
            "serve",
            "--repo-path",
            tmp.path().to_str().expect("utf8 temp path"),
            "--bind",
            "127.0.0.1:0",
        ])
        .output()
        .expect("run second memory-mcp instance");

    assert!(
        !output.status.success(),
        "second instance must exit non-zero while the lock is held, got: {:?}",
        output.status
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("already serving"),
        "second instance should explain the contention: {stderr}"
    );
    let first_pid = first.0.id().to_string();
    assert!(
        stderr.contains(&first_pid),
        "second instance should name the holder pid {first_pid}: {stderr}"
    );

    // Clean shutdown of the first instance releases the lock via the kernel.
    drop(first.0.stdin.take());
    let status = wait_with_timeout(&mut first.0, Duration::from_secs(60));
    assert!(
        status.success(),
        "first instance clean exit, got: {status:?}"
    );
}

/// Scope-mapped repositories are covered by the single-writer lock too
/// (#329 review, round 2): two processes with *distinct* default repos that
/// share one mapped repo must contend on the shared repo's lock — the
/// second exits non-zero, fast, naming the holder's pid and the contended
/// path.
#[test]
fn second_instance_fails_fast_when_configs_share_a_mapped_repo() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let default_a = tmp.path().join("default-a");
    let default_b = tmp.path().join("default-b");
    let shared = tmp.path().join("shared-mapped");
    std::fs::create_dir_all(&default_a).expect("create default-a");
    std::fs::create_dir_all(&default_b).expect("create default-b");

    // Two configs, both mapping the `work` scope to the same local repo.
    // The URL is never fetched at startup; only the path matters here.
    let write_config = |name: &str| {
        let path = tmp.path().join(name);
        std::fs::write(
            &path,
            format!(
                "[[remotes]]\nscope = \"work\"\nurl = \"https://example.invalid/shared.git\"\npath = \"{}\"\n",
                shared.display()
            ),
        )
        .expect("write config");
        path
    };
    let config_a = write_config("config-a.toml");
    let config_b = write_config("config-b.toml");

    let stderr_path = tmp.path().join("first-stderr.log");
    let stderr_file = std::fs::File::create(&stderr_path).expect("stderr capture");
    let mut first = Command::new(env!("CARGO_BIN_EXE_memory-mcp"))
        .args([
            "serve",
            "--transport",
            "stdio",
            "--repo-path",
            default_a.to_str().expect("utf8 temp path"),
            "--config",
            config_a.to_str().expect("utf8 temp path"),
        ])
        .env("RUST_LOG", "debug")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::from(stderr_file))
        .spawn()
        .map(ServerGuard)
        .expect("spawn first instance");
    let rx = stdout_lines(&mut first.0);

    // Complete the handshake so the first instance provably holds every
    // lock (they are all acquired before subsystem init).
    initialize(&mut first.0, &rx, &stderr_path);

    // Second instance: different default repo, same mapped repo. Without
    // mapped-repo locking both processes would happily serve and mutate the
    // shared git repo concurrently; it must fail fast on the shared lock.
    let output = Command::new(env!("CARGO_BIN_EXE_memory-mcp"))
        .args([
            "serve",
            "--repo-path",
            default_b.to_str().expect("utf8 temp path"),
            "--config",
            config_b.to_str().expect("utf8 temp path"),
            "--bind",
            "127.0.0.1:0",
        ])
        .output()
        .expect("run second memory-mcp instance");

    assert!(
        !output.status.success(),
        "second instance must exit non-zero while the shared mapped repo \
         lock is held, got: {:?}",
        output.status
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("already serving"),
        "second instance should explain the contention: {stderr}"
    );
    assert!(
        stderr.contains("shared-mapped"),
        "second instance should name the contended mapped repo path: {stderr}"
    );
    let first_pid = first.0.id().to_string();
    assert!(
        stderr.contains(&first_pid),
        "second instance should name the holder pid {first_pid}: {stderr}"
    );

    drop(first.0.stdin.take());
    let status = wait_with_timeout(&mut first.0, Duration::from_secs(60));
    assert!(
        status.success(),
        "first instance clean exit, got: {status:?}"
    );
}

/// SIGTERM during an active session exercises the signal branch of
/// shutdown (#329 review, round 2): the running service must be cancelled
/// and awaited — draining rmcp cleanup — *before* `run_serve` persists the
/// vector index. The stderr log order is the receipt: the transport drain
/// line, then the mutation-registry drain line (#329 review, round 3 — the
/// application-side gate with no transport-imposed ceiling), then the
/// index-saved line, and the exit must be clean. The in-process unit tests
/// (`shutdown_signal_drains_in_flight_tool_call_before_returning` and
/// `shutdown_awaits_mutation_blocked_beyond_rmcp_drain_window`) prove the
/// two drains actually block on in-flight work.
#[cfg(unix)]
#[test]
fn sigterm_drains_service_before_index_persistence() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let stderr_path = tmp.path().join("stderr.log");
    let stderr_file = std::fs::File::create(&stderr_path).expect("create stderr capture");

    let mut server = spawn_stdio_server(tmp.path(), Stdio::from(stderr_file));
    let rx = stdout_lines(&mut server.0);
    initialize(&mut server.0, &rx, &stderr_path);

    // SIGTERM, not stdin EOF: the client end stays open so only the signal
    // path can end the session.
    let kill_status = Command::new("kill")
        .args(["-TERM", &server.0.id().to_string()])
        .status()
        .expect("send SIGTERM");
    assert!(kill_status.success(), "kill -TERM failed: {kill_status:?}");

    let status = wait_with_timeout(&mut server.0, Duration::from_secs(60));
    assert!(
        status.success(),
        "clean exit after SIGTERM (graceful shutdown, not signal death), got: {status:?}"
    );

    let stderr = std::fs::read_to_string(&stderr_path).expect("read captured stderr");
    let drained = stderr
        .find("stdio transport drained and closed")
        .unwrap_or_else(|| panic!("missing drain completion log line in stderr:\n{stderr}"));
    let mutations_drained = stderr
        .find("mutation units drained")
        .unwrap_or_else(|| panic!("missing mutation-registry drain log line in stderr:\n{stderr}"));
    let saved = stderr
        .find("vector index saved")
        .unwrap_or_else(|| panic!("missing index persistence log line in stderr:\n{stderr}"));
    assert!(
        drained < mutations_drained && mutations_drained < saved,
        "shutdown order must be transport drain, then mutation-registry \
         drain (#329 round 3), then index persistence:\n{stderr}"
    );
}
