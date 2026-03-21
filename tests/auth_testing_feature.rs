//! Integration tests exercising the `testing` feature gate.
//!
//! These tests use `AuthProvider::with_token` (available only via the `testing`
//! feature) to construct an authenticated provider without real credentials,
//! then exercise code paths that require auth.

use std::sync::Arc;

use memory_mcp::auth::AuthProvider;
use memory_mcp::repo::MemoryRepo;
use memory_mcp::types::{Memory, MemoryMetadata, PullResult, Scope};

/// Full round-trip: init repo → save memory → read it back → list → delete → pull.
///
/// `pull` exercises the auth path: `AuthProvider::with_token` provides a
/// pre-seeded token, but because there's no remote configured the pull
/// returns `PullResult::NoRemote` without ever hitting the network.
/// This proves the `with_token` constructor works end-to-end through
/// the server stack.
#[tokio::test]
async fn repo_round_trip_with_test_auth_provider() {
    let tmp = tempfile::tempdir().unwrap();
    let repo =
        Arc::new(MemoryRepo::init_or_open(tmp.path(), None).expect("should init fresh repo"));
    let auth = AuthProvider::with_token("ghp_fake_test_token");

    // Save a memory.
    let metadata = MemoryMetadata::new(Scope::Global, vec!["test".into()], None);
    let memory = Memory::new(
        "test-memory".into(),
        "Hello from integration test.".into(),
        metadata,
    );
    repo.save_memory(&memory)
        .await
        .expect("save should succeed");

    // Read it back.
    let loaded = repo
        .read_memory("test-memory", &Scope::Global)
        .await
        .expect("read should find the memory");
    assert_eq!(loaded.name, "test-memory");
    assert_eq!(loaded.content, "Hello from integration test.");

    // List all memories.
    let list = repo.list_memories(None).await.expect("list should succeed");
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].name, "test-memory");

    // Pull with no remote — exercises auth resolution path via with_token.
    let pull_result = repo.pull(&auth, "main").await.expect("pull should succeed");
    assert!(
        matches!(pull_result, PullResult::NoRemote),
        "expected NoRemote since we didn't configure origin, got: {pull_result:?}",
    );

    // Delete the memory.
    repo.delete_memory("test-memory", &Scope::Global)
        .await
        .expect("delete should succeed");

    // Verify it's gone.
    let list_after = repo.list_memories(None).await.expect("list should succeed");
    assert!(list_after.is_empty(), "memory should be deleted");
}
