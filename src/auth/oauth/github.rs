use super::{validate_endpoint_url, DeviceFlowProvider};
use crate::error::MemoryError;

const CLIENT_ID: &str = "Ov23liWxHYkwXTxCrYHp";
const DEVICE_CODE_URL: &str = "https://github.com/login/device/code";
const ACCESS_TOKEN_URL: &str = "https://github.com/login/oauth/access_token";
const SCOPES: &[&str] = &["repo"];

/// GitHub's RFC 8628 device authorization flow.
///
/// All values are compile-time constants — no runtime configuration needed.
pub struct GitHubDeviceFlow;

impl DeviceFlowProvider for GitHubDeviceFlow {
    fn client_id(&self) -> &str {
        CLIENT_ID
    }

    fn device_code_url(&self) -> &str {
        DEVICE_CODE_URL
    }

    fn access_token_url(&self) -> &str {
        ACCESS_TOKEN_URL
    }

    fn scopes(&self) -> &[&str] {
        SCOPES
    }

    fn validate(&self) -> Result<(), MemoryError> {
        validate_github_client_id(self.client_id())?;
        validate_endpoint_url(self.device_code_url(), "device_code_url")?;
        validate_endpoint_url(self.access_token_url(), "access_token_url")?;
        Ok(())
    }
}

/// Validate a GitHub OAuth client ID format.
///
/// GitHub client IDs are alphanumeric strings, typically 20 characters.
/// Exposed as `pub(crate)` for testing.
pub(crate) fn validate_github_client_id(id: &str) -> Result<(), MemoryError> {
    if id.is_empty() {
        return Err(MemoryError::OAuth(
            "GitHub client ID must not be empty".into(),
        ));
    }
    if id.len() < 4 || id.len() > 64 {
        return Err(MemoryError::OAuth(format!(
            "GitHub client ID has unexpected length ({})",
            id.len()
        )));
    }
    Ok(())
}
