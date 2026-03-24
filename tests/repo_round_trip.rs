//! Integration tests for the memory repository — save, read, list, pull, delete.
//!
//! Uses `AuthProvider::with_token` to inject a known token without needing
//! real credentials. The `push_pull_with_bare_remote` test exercises the full
//! credential callback path through a `file://` bare remote.

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
/// the repo layer.
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

/// Push and pull against a `file://` bare remote, exercising the full
/// credential callback path through `AuthProvider::with_token`.
///
/// `file://` remotes don't actually check credentials, but the git2
/// `RemoteCallbacks::credentials` closure is still invoked — this proves
/// the token flows from `AuthProvider` through `build_auth_callbacks` into
/// the git transport layer without panicking or type errors.
#[tokio::test]
async fn push_pull_with_bare_remote() {
    // Set up a bare remote.
    let remote_dir = tempfile::tempdir().unwrap();
    git2::Repository::init_bare(remote_dir.path()).expect("failed to init bare repo");
    let remote_url = format!("file://{}", remote_dir.path().display());

    // Init a local repo with origin pointing at the bare remote.
    let local_dir = tempfile::tempdir().unwrap();
    let repo = Arc::new(
        MemoryRepo::init_or_open(local_dir.path(), Some(&remote_url))
            .expect("should init local repo with remote"),
    );
    let auth = AuthProvider::with_token("ghp_fake_push_token");

    // Save a memory so there's something to push.
    let metadata = MemoryMetadata::new(Scope::Global, vec!["push-test".into()], None);
    let memory = Memory::new(
        "push-memory".into(),
        "Content for push test.".into(),
        metadata,
    );
    repo.save_memory(&memory)
        .await
        .expect("save should succeed");

    // Push to the bare remote.
    repo.push(&auth, "main").await.expect("push should succeed");

    // Verify the bare remote received the commit.
    let bare = git2::Repository::open_bare(remote_dir.path()).unwrap();
    let head = bare.find_reference("refs/heads/main").unwrap();
    let commit = head.peel_to_commit().unwrap();
    assert!(
        commit.message().unwrap().contains("push-memory"),
        "bare remote should contain a commit mentioning the memory name",
    );

    // Pull into a fresh clone to verify the round-trip.
    let clone_dir = tempfile::tempdir().unwrap();
    let clone_repo = Arc::new(
        MemoryRepo::init_or_open(clone_dir.path(), Some(&remote_url))
            .expect("should init clone repo"),
    );
    let clone_auth = AuthProvider::with_token("ghp_fake_pull_token");
    let pull_result = clone_repo
        .pull(&clone_auth, "main")
        .await
        .expect("pull should succeed");
    assert!(
        matches!(
            pull_result,
            PullResult::FastForward { .. } | PullResult::Merged { .. }
        ),
        "expected FastForward or Merged pull, got: {pull_result:?}",
    );

    // The cloned repo should have the memory.
    let memories = clone_repo
        .list_memories(None)
        .await
        .expect("list should succeed");
    assert_eq!(memories.len(), 1);
    assert_eq!(memories[0].name, "push-memory");
    assert_eq!(memories[0].content, "Content for push test.");
}
