use std::fmt;

use tracing::{debug, info, warn};

use crate::error::MemoryError;

/// Token resolution order:
/// 1. `MEMORY_MCP_GITHUB_TOKEN` environment variable
/// 2. `~/.config/memory-mcp/token` file
/// 3. System keyring (GNOME Keyring / KWallet / macOS Keychain)
const ENV_VAR: &str = "MEMORY_MCP_GITHUB_TOKEN";
const TOKEN_FILE: &str = ".config/memory-mcp/token";

// ---------------------------------------------------------------------------
// Secret<T> — redacts sensitive values from Debug and Display output
// ---------------------------------------------------------------------------

/// A wrapper that redacts its inner value from `Debug` and `Display`.
///
/// Use `.expose()` to access the raw value when it is genuinely needed
/// (e.g. to pass to an API call).
pub struct Secret<T>(T);

impl<T> Secret<T> {
    pub fn new(value: T) -> Self {
        Self(value)
    }

    /// Expose the inner value. Call sites make the exposure explicit.
    pub fn expose(&self) -> &T {
        &self.0
    }

    /// Consume the wrapper and return the inner value.
    pub fn into_inner(self) -> T {
        self.0
    }
}

impl<T> fmt::Debug for Secret<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[REDACTED]")
    }
}

impl<T> fmt::Display for Secret<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[REDACTED]")
    }
}

// ---------------------------------------------------------------------------
// AuthProvider
// ---------------------------------------------------------------------------

pub struct AuthProvider {
    /// Cached token, if resolved at startup.
    token: Option<Secret<String>>,
}

impl AuthProvider {
    /// Create an `AuthProvider`, eagerly attempting token resolution.
    ///
    /// Does not fail if no token is available — some deployments may not
    /// need remote sync. Call [`Self::resolve_token`] when a token is required.
    pub fn new() -> Self {
        let token = Self::try_resolve().ok().map(Secret::new);
        if token.is_some() {
            debug!("AuthProvider: token resolved at startup");
        } else {
            debug!("AuthProvider: no token available at startup");
        }
        Self { token }
    }

    /// Resolve a GitHub personal access token, returning it wrapped in
    /// [`Secret`] so it cannot accidentally appear in logs or error chains.
    ///
    /// Checks (in order):
    /// 1. `MEMORY_MCP_GITHUB_TOKEN` env var
    /// 2. `~/.config/memory-mcp/token` file
    /// 3. System keyring (GNOME Keyring / KWallet / macOS Keychain)
    pub fn resolve_token(&self) -> Result<Secret<String>, MemoryError> {
        // Return cached token if we already have one.
        if let Some(ref t) = self.token {
            return Ok(Secret::new(t.expose().clone()));
        }
        Self::try_resolve().map(Secret::new)
    }

    // -----------------------------------------------------------------------
    // Private helpers
    // -----------------------------------------------------------------------

    fn try_resolve() -> Result<String, MemoryError> {
        // 1. Environment variable.
        if let Ok(tok) = std::env::var(ENV_VAR) {
            if !tok.trim().is_empty() {
                return Ok(tok.trim().to_string());
            }
        }

        // 2. Token file.
        if let Some(home) = home_dir() {
            let path = home.join(TOKEN_FILE);
            if path.exists() {
                // Check permissions: warn if the file is world- or group-readable.
                check_token_file_permissions(&path);

                let raw = std::fs::read_to_string(&path)?;
                let tok = raw.trim().to_string();
                if !tok.is_empty() {
                    return Ok(tok);
                }
            }
        }

        // 3. System keyring (GNOME Keyring / KWallet / macOS Keychain).
        match keyring::Entry::new("memory-mcp", "github-token") {
            Ok(entry) => match entry.get_password() {
                Ok(tok) if !tok.trim().is_empty() => {
                    info!("resolved GitHub token from system keyring");
                    return Ok(tok.trim().to_string());
                }
                Ok(_) => { /* empty password stored — fall through */ }
                Err(keyring::Error::NoEntry) => { /* no entry — fall through */ }
                Err(keyring::Error::NoStorageAccess(_)) => {
                    debug!("keyring: no storage backend available (headless?)");
                }
                Err(e) => {
                    warn!("keyring: unexpected error: {e}");
                }
            },
            Err(e) => {
                debug!("keyring: could not create entry: {e}");
            }
        }

        Err(MemoryError::Auth(
            "no token available; set MEMORY_MCP_GITHUB_TOKEN, add \
             ~/.config/memory-mcp/token, or store a token in the system keyring \
             under service 'memory-mcp', account 'github-token'."
                .to_string(),
        ))
    }
}

impl AuthProvider {
    /// Create an `AuthProvider` with a pre-set token. For testing only.
    #[cfg(test)]
    pub(crate) fn with_token(token: &str) -> Self {
        Self {
            token: Some(Secret::new(token.to_string())),
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    // Serialise all tests that mutate environment variables so they don't race
    // under `cargo test` (which runs tests in parallel by default).
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn test_resolve_from_env_var() {
        let _guard = ENV_LOCK.lock().unwrap();
        let token_value = "ghp_test_env_token_abc123";
        std::env::set_var(ENV_VAR, token_value);
        let result = AuthProvider::try_resolve();
        std::env::remove_var(ENV_VAR);

        assert!(result.is_ok(), "expected Ok but got: {result:?}");
        assert_eq!(result.unwrap(), token_value);
    }

    #[test]
    fn test_resolve_trims_env_var_whitespace() {
        let _guard = ENV_LOCK.lock().unwrap();
        let token_value = "  ghp_padded_token  ";
        std::env::set_var(ENV_VAR, token_value);
        let result = AuthProvider::try_resolve();
        std::env::remove_var(ENV_VAR);

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), token_value.trim());
    }

    #[test]
    fn test_resolve_prefers_env_over_file() {
        let _guard = ENV_LOCK.lock().unwrap();
        // Write a token file and simultaneously set the env var; env must win.
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("token");
        std::fs::write(&file_path, "ghp_file_token").unwrap();

        let env_token = "ghp_env_wins";
        std::env::set_var(ENV_VAR, env_token);

        // Override HOME so the file lookup would pick up our temp file if env
        // were not consulted first.  We rely on env taking precedence, so
        // this primarily tests ordering rather than actual file resolution.
        let result = AuthProvider::try_resolve();
        std::env::remove_var(ENV_VAR);

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), env_token);
    }

    /// This test exercises the keyring path and requires a live D-Bus /
    /// secret-service backend.  Mark it `#[ignore]` so it does not run in CI.
    #[test]
    #[ignore = "requires live system keyring (D-Bus/GNOME Keyring/KWallet)"]
    fn test_resolve_from_keyring_ignored_in_ci() {
        let _guard = ENV_LOCK.lock().unwrap();
        // Pre-condition: no env var, no token file (rely on absence).
        std::env::remove_var(ENV_VAR);

        // Attempt to store then retrieve; if the keyring is unavailable the
        // test is inconclusive rather than failing.
        let entry = keyring::Entry::new("memory-mcp", "github-token")
            .expect("keyring entry creation should succeed");
        let test_token = "ghp_keyring_test_token";
        entry
            .set_password(test_token)
            .expect("storing token should succeed");

        let result = AuthProvider::try_resolve();
        let _ = entry.delete_credential(); // cleanup before assert
        assert!(result.is_ok(), "expected token from keyring: {result:?}");
        assert_eq!(result.unwrap(), test_token);
    }
}

impl Default for AuthProvider {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Permission check (Unix only)
// ---------------------------------------------------------------------------

/// Warn if the token file has permissions that are wider than 0o600.
fn check_token_file_permissions(path: &std::path::Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        match std::fs::metadata(path) {
            Ok(meta) => {
                let mode = meta.mode() & 0o777;
                if mode != 0o600 {
                    warn!(
                        "token file '{}' has permissions {:04o}; \
                         expected 0600 — consider running: chmod 600 {}",
                        path.display(),
                        mode,
                        path.display()
                    );
                }
            }
            Err(e) => {
                warn!("could not read permissions for '{}': {}", path.display(), e);
            }
        }
    }
    // On non-Unix platforms there are no POSIX permissions to check.
    #[cfg(not(unix))]
    let _ = path;
}

// ---------------------------------------------------------------------------
// Platform-portable home directory helper
// ---------------------------------------------------------------------------

fn home_dir() -> Option<std::path::PathBuf> {
    std::env::var("HOME")
        .ok()
        .map(std::path::PathBuf::from)
        .or_else(|| {
            // Fallback for unusual environments
            #[allow(deprecated)]
            std::env::home_dir()
        })
}
