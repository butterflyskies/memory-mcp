//! Integration tests for scope-affinity filtering in list and recall.
//!
//! These tests exercise the repo layer directly with `list_memories` and
//! verify that the server-layer filtering pattern (match on ScopeFilter) works
//! as expected when applied to the full memory list.

use std::sync::Arc;

use memory_mcp::repo::MemoryRepo;
use memory_mcp::types::{Memory, MemoryMetadata, Scope};

/// Helper: initialise a fresh in-memory repo in a temp directory.
async fn make_repo() -> (Arc<MemoryRepo>, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let repo =
        Arc::new(MemoryRepo::init_or_open(tmp.path(), None).expect("should init fresh repo"));
    (repo, tmp)
}

/// Helper: save a memory with the given scope and name.
async fn save(repo: &Arc<MemoryRepo>, name: &str, scope: Scope) {
    let metadata = MemoryMetadata::new(scope, vec![], None);
    let memory = Memory::new(name.to_string(), format!("Content for {}", name), metadata);
    repo.save_memory(&memory)
        .await
        .expect("save should succeed");
}

// ---------------------------------------------------------------------------
// list_memories(Some(&Scope::Global)) — global-only
// ---------------------------------------------------------------------------

#[tokio::test]
async fn list_scope_filter_global_only() {
    let (repo, _tmp) = make_repo().await;

    save(&repo, "global-mem", Scope::Global).await;
    save(&repo, "proj-mem", Scope::Project("test-proj".to_string())).await;

    let memories = repo
        .list_memories(Some(&Scope::Global))
        .await
        .expect("list should succeed");

    assert_eq!(memories.len(), 1, "expected only the global memory");
    assert_eq!(memories[0].name, "global-mem");
    assert_eq!(memories[0].metadata.scope, Scope::Global);
}

// ---------------------------------------------------------------------------
// list_memories(Some(&Scope::Project(...))) — specific project only
// ---------------------------------------------------------------------------

#[tokio::test]
async fn list_scope_filter_project_specific() {
    let (repo, _tmp) = make_repo().await;

    save(&repo, "global-mem", Scope::Global).await;
    save(&repo, "proj-mem", Scope::Project("test-proj".to_string())).await;
    save(
        &repo,
        "other-proj-mem",
        Scope::Project("other-proj".to_string()),
    )
    .await;

    let memories = repo
        .list_memories(Some(&Scope::Project("test-proj".to_string())))
        .await
        .expect("list should succeed");

    assert_eq!(memories.len(), 1, "expected only the test-proj memory");
    assert_eq!(memories[0].name, "proj-mem");
    assert_eq!(
        memories[0].metadata.scope,
        Scope::Project("test-proj".to_string())
    );
}

// ---------------------------------------------------------------------------
// list_memories(None) — all scopes
// ---------------------------------------------------------------------------

#[tokio::test]
async fn list_scope_filter_all() {
    let (repo, _tmp) = make_repo().await;

    save(&repo, "global-mem", Scope::Global).await;
    save(&repo, "proj-mem", Scope::Project("test-proj".to_string())).await;

    let memories = repo.list_memories(None).await.expect("list should succeed");

    assert_eq!(memories.len(), 2, "expected both memories");
    let names: Vec<&str> = memories.iter().map(|m| m.name.as_str()).collect();
    assert!(names.contains(&"global-mem"));
    assert!(names.contains(&"proj-mem"));
}

// ---------------------------------------------------------------------------
// Production path: two targeted list_memories calls merged (ProjectAndGlobal)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn list_scope_filter_project_and_global() {
    let (repo, _tmp) = make_repo().await;

    save(&repo, "global-mem", Scope::Global).await;
    save(&repo, "proj-mem", Scope::Project("test-proj".to_string())).await;
    save(
        &repo,
        "other-proj-mem",
        Scope::Project("other-proj".to_string()),
    )
    .await;

    // Mirror the production code path in server.rs: two targeted calls merged.
    let project_scope = Scope::Project("test-proj".to_string());
    let mut memories = repo
        .list_memories(Some(&Scope::Global))
        .await
        .expect("global list should succeed");
    memories.extend(
        repo.list_memories(Some(&project_scope))
            .await
            .expect("project list should succeed"),
    );

    assert_eq!(
        memories.len(),
        2,
        "expected global + test-proj memories, not other-proj"
    );
    let names: Vec<&str> = memories.iter().map(|m| m.name.as_str()).collect();
    assert!(names.contains(&"global-mem"), "missing global-mem");
    assert!(names.contains(&"proj-mem"), "missing proj-mem");
    assert!(
        !names.contains(&"other-proj-mem"),
        "other-proj-mem should be excluded"
    );
}
