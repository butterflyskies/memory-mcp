# ADR-0012: OAuth device flow for token acquisition

## Status
Accepted

## Context
Users need a way to obtain a GitHub token with the correct scopes
(`repo`) without manually creating a PAT or exposing tokens in CLI
arguments (see ADR-0004). The token must be stored securely across
desktop and Kubernetes environments.

## Decision
Use GitHub's OAuth device flow via a registered OAuth App owned by
the butterflyskies organization. The `auth login` command initiates
the flow, displays a user code, polls for approval, and stores the
token using a fallback chain: keyring first (desktop), then token file
(~/.config/memory-mcp/token with 0600 permissions). A `--store` flag
allows forcing a specific backend (keyring, file, stdout). The client
ID is defined as a constant in the source code — OAuth Apps used with
device flow do not require a client secret.

## Consequences
- Users get scoped tokens without manual PAT creation
- No browser redirect required — works from SSH sessions and terminals
- Keyring storage integrates with ADR-0010
- stdout backend enables piping to `kubectl create secret` for k8s
- If Marketplace listing is needed later, migrate to a GitHub App
  (swap client ID and scope format, no structural code change)
