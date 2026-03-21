//! Verifies the `testing` feature gate exposes `AuthProvider::with_token`
//! to integration tests (external crates).

use memory_mcp::auth::AuthProvider;

#[test]
fn with_token_constructs_provider_with_preset_token() {
    let provider = AuthProvider::with_token("ghp_test_integration_token");
    let resolved = provider
        .resolve_token()
        .expect("should resolve pre-set token");
    assert_eq!(resolved.expose(), "ghp_test_integration_token");
}
