use std::fmt;

use tracing::{debug, warn};

use crate::error::MemoryError;

/// Token resolution order:
/// 1. `MEMORY_MCP_GITHUB_TOKEN` environment variable
/// 2. `~/.config/memory-mcp/token` file
/// 3. OAuth device flow (not yet implemented)
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
    /// 3. OAuth device flow — returns `Err(Auth("not yet implemented"))`.
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

        // 3. OAuth device flow — not yet implemented.
        Err(MemoryError::Auth(
            "no token available; set MEMORY_MCP_GITHUB_TOKEN or add ~/.config/memory-mcp/token. \
             OAuth device flow is not yet implemented."
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
