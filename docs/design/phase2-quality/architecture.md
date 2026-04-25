<!-- design-meta
status: draft
last-updated: 2026-04-25
phase: 3
-->

# Architecture — Phase 2 Quality: Config, Testability, Index Abstraction & Health

Issues: #94, #145, #146, #164

## Architectural Decisions

### 1. VectorStore trait (public, semantic-level)

`AppState.index` changes from `ScopedIndex` to `Box<dyn VectorStore>`. The trait
defines what the rest of the system needs from vector storage — add, remove, search
by name and scope, persist, report health. This matches the existing pattern where
`AppState.embedding` is already `Box<dyn EmbeddingBackend>`.

**Dynamic dispatch** (`Box<dyn>`) rather than generics on `AppState`. This avoids
generic parameter proliferation through the server stack and keeps the trait
object-safe.

Implementations:
- **`UsearchStore`** — wraps existing `ScopedIndex`/`VectorIndex` logic. Internally
  uses a private `RawIndex` trait for failure injection in tests.
- **`InMemoryStore`** — `HashMap`-based, for fast deterministic tests at the trait
  level.

### 2. OAuthDeviceFlowProvider trait (RFC 8628)

The hardcoded GitHub OAuth constants become a trait covering the RFC 8628 device
authorization grant specifically. See [ADR-0024](../../../docs/adr/0024-oauth-device-flow-provider-trait.md)
for the full analysis of why this is scoped to device flow, not all OAuth.

`device_flow_login()` takes `&dyn OAuthDeviceFlowProvider` instead of importing
constants directly.

Implementations:
- **`GitHubDeviceFlow`** — zero-sized struct, compile-time constants.
- **`MockDeviceFlow`** — points at an in-process test server for integration tests.

### 3. /readyz endpoint

A new Axum route alongside `/healthz`. Checks three subsystems via their existing
APIs — no new network calls, no heavy computation:
- Git repo: HEAD resolvable
- Embedding: `dimensions() > 0`
- Vector index: `is_ready()` from VectorStore trait

Returns 200 + JSON or 503 + JSON with per-subsystem status. Response bodies use a
fixed vocabulary (`"up"` / `"down"` + a constrained `reason` field on failure) —
no internal paths, versions, or stack traces.

No auth required — this is an infrastructure endpoint, same as `/healthz`.

### 4. Integration test architecture

In-process mock OAuth server (small Axum app started per test) implementing the
device code and token endpoints. All tests use ephemeral ports to avoid conflicts
in parallel runs.

Tests are `#[tokio::test]`, calling the same entry points with injected config
rather than spawning the binary.

## Diagrams

### System Context

memory-mcp sits between four external relationships: AI agents drive it over
HTTP/MCP, a Kubernetes orchestrator probes its health endpoints, a git remote
backs its storage, and an OAuth provider handles credential issuance. All network
traffic crosses a single ingress point (the Axum router), and the git and OAuth
relationships are outbound-only.

```mermaid
graph TB
    agent["🤖 MCP Client / AI Agent<br/><i>Calls remember, recall, read,<br/>edit, forget, list, sync</i>"]
    ops["☸ Kubernetes Orchestrator<br/><i>Probes /healthz and /readyz</i>"]

    subgraph server["memory-mcp"]
        mcp["Rust MCP Server<br/>Semantic memory storage<br/>with git backing + HNSW vector search"]
    end

    git_remote["Git Remote (GitHub / GitLab)<br/><i>Stores serialised memory objects</i>"]
    oauth["OAuth Provider (GitHub / GitLab)<br/><i>Issues device-flow tokens (RFC 8628)</i>"]

    agent -->|"HTTP POST /mcp<br/>(JSON-RPC)"| mcp
    ops -->|"HTTP GET<br/>/healthz  /readyz"| mcp
    mcp -->|"git push / fetch<br/>(HTTPS, token auth)"| git_remote
    mcp -->|"device_code + token poll<br/>(HTTPS POST)"| oauth
```

### Component Diagram

Internal structure after the Phase 2 refactor, wired through `AppState`. Three
trait-abstracted subsystems: `EmbeddingBackend` (existing), `VectorStore` (new),
and `OAuthDeviceFlowProvider` (new). The `/readyz` endpoint reaches into all three
subsystems to assert readiness. The private `RawIndex` trait inside `UsearchStore`
is shown separately — it exists only for failure injection testing.

```mermaid
graph TB
    subgraph HTTP["HTTP Layer (Axum 0.8)"]
        healthz["/healthz<br/>liveness probe"]
        readyz["/readyz<br/>readiness probe"]
        mcppath["/mcp<br/>MCP JSON-RPC"]
    end

    subgraph Handlers["MCP Handlers (src/server.rs)"]
        remember["remember"]
        recall["recall"]
        read_h["read"]
        edit_h["edit"]
        forget_h["forget"]
        list_h["list"]
        sync_h["sync"]
    end

    subgraph State["AppState (src/types.rs)"]
        repo["repo: Arc‹MemoryRepo›"]
        embedding_field["embedding: Box‹dyn EmbeddingBackend›"]
        index_field["index: Box‹dyn VectorStore›"]
        auth_field["auth: AuthProvider"]
    end

    subgraph EmbeddingSub["Embedding Subsystem"]
        EB["«trait» EmbeddingBackend<br/>embed() / embed_one() / dimensions()"]
        Candle["CandleEmbeddingEngine<br/>(BGE-small-en-v1.5)"]
    end

    subgraph VectorSub["Vector Index Subsystem"]
        VS["«trait» VectorStore<br/>add / remove / search<br/>save / load / is_ready"]
        UStore["UsearchStore<br/>(ScopedIndex + VectorIndex)"]
        IMStore["InMemoryStore<br/>(HashMap, tests)"]
        subgraph RawIdx["Private: RawIndex (failure injection)"]
            URaw["UsearchRawIndex"]
            FRaw["FailingRawIndex<br/>(test double)"]
        end
    end

    subgraph AuthSub["Auth Subsystem"]
        AP["AuthProvider<br/>env var → keyring → token file"]
        DFP["«trait» OAuthDeviceFlowProvider<br/>client_id / device_code_url<br/>access_token_url / scopes / validate"]
        GDF["GitHubDeviceFlow<br/>(zero-sized, constants)"]
        MDF["MockDeviceFlow<br/>(test server)"]
    end

    subgraph RepoSub["Git Repo Subsystem"]
        MR["MemoryRepo<br/>(git-backed CRUD)"]
    end

    mcppath --> Handlers
    Handlers --> State
    readyz --> State

    embedding_field -.-> EB
    Candle -.->|implements| EB

    index_field -.-> VS
    UStore -.->|implements| VS
    IMStore -.->|implements| VS
    UStore --> URaw
    UStore --> FRaw

    repo --> MR
    auth_field --> AP
    AP -.-> DFP
    GDF -.->|implements| DFP
    MDF -.->|implements| DFP
```

### Data Flow Diagram with Trust Boundaries

Three network trust boundaries: external HTTP ingress, outbound OAuth, and outbound
git. Inside the server, the abstraction boundary between MCP handlers and
`VectorStore` is a design seam (not a security boundary) that enables test
substitution. The `/readyz` handler is entirely within the server boundary — no
external calls, only introspecting subsystem state through trait methods.

```mermaid
graph LR
    subgraph external_in["TRUST BOUNDARY: External Network (inbound)"]
        client["MCP Client<br/>/ AI Agent"]
        k8s["k8s Orchestrator"]
    end

    subgraph server["TRUST BOUNDARY: memory-mcp Process"]
        router["Axum Router"]

        subgraph readiness["Readiness (internal only)"]
            readyz_h["/readyz handler"]
            repo_check["MemoryRepo<br/>HEAD resolvable?"]
            embed_check["EmbeddingBackend<br/>dimensions > 0?"]
            index_check["VectorStore<br/>is_ready()?"]
        end

        subgraph abstraction["ABSTRACTION BOUNDARY: Trait Contracts"]
            mcp_handlers["MCP Handlers<br/>(remember / recall / read<br/>edit / forget / list / sync)"]
            embedding_trait["«trait» EmbeddingBackend"]
            vector_trait["«trait» VectorStore"]
        end

        subgraph impls["Concrete Implementations"]
            candle["CandleEmbeddingEngine"]
            usearch["UsearchStore"]
            memrepo["MemoryRepo"]
            auth_prov["AuthProvider"]
        end
    end

    subgraph oauth_out["TRUST BOUNDARY: External Network (outbound — OAuth)"]
        gh_oauth["OAuth Provider<br/>device_code + token endpoints"]
    end

    subgraph git_out["TRUST BOUNDARY: External Network (outbound — git)"]
        gh_git["Git Remote<br/>(push / fetch)"]
    end

    client -->|"HTTP POST /mcp<br/>(JSON-RPC)"| router
    k8s -->|"HTTP GET<br/>/healthz  /readyz"| router

    router --> readyz_h
    router --> mcp_handlers

    readyz_h --> repo_check
    readyz_h --> embed_check
    readyz_h --> index_check
    repo_check -.-> memrepo
    embed_check -.-> candle
    index_check -.-> usearch

    mcp_handlers --> embedding_trait
    mcp_handlers --> vector_trait
    mcp_handlers --> memrepo
    mcp_handlers --> auth_prov

    embedding_trait --> candle
    vector_trait --> usearch

    auth_prov -->|"device flow"| gh_oauth
    memrepo -->|"git push / fetch<br/>(HTTPS + token)"| gh_git
```

### Sequence: Memory Recall Flow

The full path of a `recall` call, making the trait boundary explicit: the handler
calls methods on `VectorStore`, never on `UsearchStore` directly. The embedding
step is a separate round-trip through `EmbeddingBackend` before vector search.

```mermaid
sequenceDiagram
    actor Client as MCP Client
    participant Router as Axum Router
    participant Handler as recall handler
    participant Embed as «trait» EmbeddingBackend
    participant Candle as CandleEmbeddingEngine
    participant VS as «trait» VectorStore
    participant UStore as UsearchStore
    participant Repo as MemoryRepo

    Client->>Router: POST /mcp {tool: "recall", query, scope?, limit?}
    Router->>Handler: dispatch recall(AppState, params)

    Note over Handler,Candle: Abstraction boundary: EmbeddingBackend
    Handler->>Embed: embed_one(query_text)
    Embed->>Candle: tokenise + forward pass
    Candle-->>Embed: f32 vector [384]
    Embed-->>Handler: Ok(query_vector)

    Note over Handler,UStore: Abstraction boundary: VectorStore
    Handler->>VS: search(query_vector, scope_filter, limit)
    VS->>UStore: delegate
    UStore->>UStore: HNSW ANN search
    UStore-->>VS: Vec‹SearchResult›
    VS-->>Handler: Ok(results)

    Handler->>Repo: read each matched memory
    Repo-->>Handler: Vec‹Memory›

    Handler-->>Router: JSON-RPC result
    Router-->>Client: 200 OK {memories: [...]}
```

### Sequence: Device Flow Login

The `OAuthDeviceFlowProvider` abstraction insulates login orchestration from
hardcoded constants. `GitHubDeviceFlow` supplies all endpoint URLs and scopes.
`MockDeviceFlow` can substitute in integration tests by pointing at a local test
server without any change to the flow driver.

```mermaid
sequenceDiagram
    actor CLI as CLI / Operator
    participant Login as device_flow_login()
    participant DFP as «trait» OAuthDeviceFlowProvider
    participant GDF as GitHubDeviceFlow
    participant GHDevice as OAuth device_code endpoint
    participant GHToken as OAuth access_token endpoint
    participant AP as AuthProvider

    CLI->>Login: login(provider: &dyn OAuthDeviceFlowProvider)

    Note over Login,GDF: Provider abstraction: config injection
    Login->>DFP: validate()
    DFP->>GDF: check client_id format, URL schemes
    GDF-->>Login: Ok(())

    Login->>DFP: client_id() / device_code_url() / scopes()
    DFP-->>Login: "Iv1.…" / URL / ["repo"]

    Note over Login,GHDevice: Network boundary: outbound OAuth
    Login->>GHDevice: POST {client_id, scope}
    GHDevice-->>Login: {device_code, user_code, verification_uri}

    Login-->>CLI: "Visit URL — enter code XXXX-YYYY"

    loop Poll until granted or expired
        Login->>DFP: access_token_url()
        DFP-->>Login: URL
        Login->>GHToken: POST {client_id, device_code, grant_type}
        alt authorization_pending
            GHToken-->>Login: {error: "authorization_pending"}
        else granted
            GHToken-->>Login: {access_token}
        end
    end

    Login->>AP: store_token(access_token)
    AP-->>Login: Ok(())
    Login-->>CLI: Ok(())
```

### Sequence: Readiness Check

`/readyz` is entirely inward-facing: every check is a method call on an
already-initialised subsystem. No network I/O occurs. The handler aggregates
per-subsystem status into structured JSON and returns 503 if any check fails,
200 if all pass, without exposing internal details.

```mermaid
sequenceDiagram
    actor K8s as k8s Orchestrator
    participant Router as Axum Router
    participant RH as /readyz handler
    participant Repo as MemoryRepo
    participant Embed as «trait» EmbeddingBackend
    participant VS as «trait» VectorStore

    K8s->>Router: GET /readyz
    Router->>RH: readyz_handler(AppState)

    Note over RH,Repo: Check 1: git repo
    RH->>Repo: head_resolvable()
    Repo-->>RH: Ok / Err

    Note over RH,Embed: Check 2: embedding model
    RH->>Embed: dimensions()
    Embed-->>RH: 384 (or 0 if not loaded)

    Note over RH,VS: Check 3: vector index
    RH->>VS: is_ready()
    VS-->>RH: ReadyStatus

    RH->>RH: aggregate: all up → 200, any down → 503

    alt all subsystems up
        RH-->>Router: 200 {"status":"ready","checks":{"git_repo":{"status":"up"},...}}
    else any subsystem down
        RH-->>Router: 503 {"status":"not_ready","checks":{...,"reason":"..."}}
    end

    Router-->>K8s: HTTP response
```
