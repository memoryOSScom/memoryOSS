# State Machines

These diagrams describe the system as it is currently implemented after the hybrid gateway/core split.

Relevant code anchors:
- [src/main.rs](/root/engraim/src/main.rs)
- [src/config.rs](/root/engraim/src/config.rs)
- [src/server/gateway.rs](/root/engraim/src/server/gateway.rs)
- [src/server/mod.rs](/root/engraim/src/server/mod.rs)
- [src/server/routes.rs](/root/engraim/src/server/routes.rs)
- [src/server/proxy.rs](/root/engraim/src/server/proxy.rs)
- [src/mcp.rs](/root/engraim/src/mcp.rs)
- [src/memory.rs](/root/engraim/src/memory.rs)
- [src/engines/document.rs](/root/engraim/src/engines/document.rs)
- [tests/integration.rs](/root/engraim/tests/integration.rs)

## 1. Runtime Topology

The important architectural fact is:
- `memoryoss serve` starts the **hybrid gateway** when `server.hybrid_mode = true`
- the gateway can manage a loopback **memory core** child
- automatic failover is **gateway -> direct upstream passthrough**
- MCP is a parallel explicit-tool path, not the transport failover path

```mermaid
stateDiagram-v2
    [*] --> ServeCommand

    ServeCommand --> MonolithServer: hybrid_mode = false
    ServeCommand --> HybridGateway: hybrid_mode = true

    HybridGateway --> SpawnManagedCore: manage_core = true
    HybridGateway --> GatewayOnly: manage_core = false

    SpawnManagedCore --> CoreStarting
    CoreStarting --> CoreHealthy
    CoreStarting --> CoreExited
    CoreExited --> CoreRestartSleep
    CoreRestartSleep --> CoreStarting

    GatewayOnly --> GatewayServing
    CoreHealthy --> GatewayServing
    CoreExited --> GatewayServing

    GatewayServing --> [*]
    MonolithServer --> [*]
```

## 2. Hybrid Gateway Request Routing

The gateway distinguishes three classes of traffic:
- `/health` is always answered by the gateway
- `/v1/*` and `/metrics` are **core-only** and return `503` if the core is unavailable
- `/proxy/*` first tries the core and then fails open to direct upstream passthrough

```mermaid
stateDiagram-v2
    [*] --> IncomingRequest

    IncomingRequest --> HealthRoute: /health
    IncomingRequest --> CoreOnlyRoute: /v1/* or /metrics
    IncomingRequest --> ProxyRoute: /proxy/*

    HealthRoute --> ReportCoreOk: core /health reachable
    HealthRoute --> ReportCoreDegraded: core unreachable
    ReportCoreOk --> Return200
    ReportCoreDegraded --> Return200

    CoreOnlyRoute --> ForwardToCore
    ForwardToCore --> CoreResponse: core reachable
    ForwardToCore --> CoreUnavailable503: core unreachable
    CoreResponse --> ReturnCorePayload

    ProxyRoute --> TryCoreFirst
    TryCoreFirst --> CoreProxyResponse: core reachable
    TryCoreFirst --> DirectUpstreamFallback: core unreachable
    CoreProxyResponse --> ReturnProxyPayload

    DirectUpstreamFallback --> FallbackOpenAI: /proxy/v1/models|chat/completions|responses
    DirectUpstreamFallback --> FallbackAnthropic: /proxy/anthropic/v1/messages
    DirectUpstreamFallback --> Fallback404: unsupported proxy subpath

    FallbackOpenAI --> ReturnFallbackPayload
    FallbackAnthropic --> ReturnFallbackPayload

    Return200 --> [*]
    ReturnCorePayload --> [*]
    ReturnProxyPayload --> [*]
    ReturnFallbackPayload --> [*]
    CoreUnavailable503 --> [*]
    Fallback404 --> [*]
```

## 3. OpenAI / Codex Proxy Auth Resolution

This is the effective decision tree used both by the core proxy path and by direct gateway fallback.

```mermaid
stateDiagram-v2
    [*] --> ReadAuthorization
    ReadAuthorization --> Reject401: no Bearer token

    ReadAuthorization --> CheckKeyMapping: Bearer token present
    CheckKeyMapping --> ProxyKeyMapped: token matches configured proxy key
    CheckKeyMapping --> PassthroughGate: no mapping match

    PassthroughGate --> Reject401: passthrough_auth = false
    PassthroughGate --> Reject401: passthrough_local_only = true and client is not loopback
    PassthroughGate --> OAuthPassthrough: token starts with eyJ
    PassthroughGate --> ApiKeyPassthrough: other Bearer token

    ProxyKeyMapped --> UpstreamApiKey
    ApiKeyPassthrough --> UseConfiguredOpenAIKey: upstream_api_key configured
    ApiKeyPassthrough --> UseClientKeyDirectly: no upstream_api_key configured
    OAuthPassthrough --> UseClientOAuthToken

    UpstreamApiKey --> [*]
    UseConfiguredOpenAIKey --> [*]
    UseClientKeyDirectly --> [*]
    UseClientOAuthToken --> [*]
    Reject401 --> [*]
```

## 4. Anthropic / Claude Proxy Auth Resolution

Anthropic has two live auth paths:
- `x-api-key` for API keys
- `Authorization: Bearer ...` for OAuth passthrough

```mermaid
stateDiagram-v2
    [*] --> ReadHeaders

    ReadHeaders --> CheckXApiKey: x-api-key present
    ReadHeaders --> CheckBearer: no x-api-key
    ReadHeaders --> Reject401: neither header present

    CheckXApiKey --> ProxyKeyMapped: x-api-key matches configured proxy key
    CheckXApiKey --> PassthroughApiGate: x-api-key does not match mapping

    PassthroughApiGate --> Reject401: passthrough not allowed
    PassthroughApiGate --> ClientApiKeyPassthrough: passthrough allowed

    CheckBearer --> Reject401: no Bearer token
    CheckBearer --> PassthroughBearerGate: Bearer token present
    PassthroughBearerGate --> Reject401: passthrough not allowed
    PassthroughBearerGate --> OAuthPassthrough: passthrough allowed

    ProxyKeyMapped --> UseConfiguredAnthropicKey
    ClientApiKeyPassthrough --> UseConfiguredAnthropicKeyIfPresent
    ClientApiKeyPassthrough --> UseClientAnthropicKey
    OAuthPassthrough --> UseBearerTokenDirectly

    UseConfiguredAnthropicKey --> [*]
    UseConfiguredAnthropicKeyIfPresent --> [*]
    UseClientAnthropicKey --> [*]
    UseBearerTokenDirectly --> [*]
    Reject401 --> [*]
```

## 5. Core Proxy Path

When the core is healthy, this is the main memory path for both Claude and Codex proxy traffic.

```mermaid
stateDiagram-v2
    [*] --> ResolveProxyAuth
    ResolveProxyAuth --> Reject401: auth resolution failed
    ResolveProxyAuth --> ParseMemoryMode

    ParseMemoryMode --> OffMode: off
    ParseMemoryMode --> ReadonlyMode: readonly
    ParseMemoryMode --> FullMode: full or after

    OffMode --> ForwardUpstream
    ReadonlyMode --> RecallPhase
    FullMode --> RecallPhase

    RecallPhase --> RankAndFuse
    RankAndFuse --> FilterInjectable
    FilterInjectable --> ForwardUpstream

    ForwardUpstream --> StoreGate
    StoreGate --> ReturnResponse: extraction disabled or no facts
    StoreGate --> DuplicateCheck: extraction enabled and facts found

    DuplicateCheck --> ConfirmExisting: near-duplicate / same fact already exists
    DuplicateCheck --> StoreCandidate: new candidate fact
    ConfirmExisting --> ReturnResponse
    StoreCandidate --> ReturnResponse

    Reject401 --> [*]
    ReturnResponse --> [*]
```

## 6. Fail-Open Fallback Path

This is the most important system-level change in the current architecture.

```mermaid
stateDiagram-v2
    [*] --> ProxyTrafficAtGateway
    ProxyTrafficAtGateway --> TryCoreProxy

    TryCoreProxy --> CoreProxySucceeded: core accepted request
    TryCoreProxy --> CoreUnavailable: core connect/send failed

    CoreUnavailable --> ReResolveAuthAtGateway
    ReResolveAuthAtGateway --> Reject401: passthrough not permitted
    ReResolveAuthAtGateway --> NormalizeBody

    NormalizeBody --> ForwardDirectToOpenAI: /proxy/v1/*
    NormalizeBody --> ForwardDirectToAnthropic: /proxy/anthropic/*

    ForwardDirectToOpenAI --> ReturnUpstreamResponse
    ForwardDirectToAnthropic --> ReturnUpstreamResponse

    CoreProxySucceeded --> ReturnCoreResponse
    Reject401 --> [*]
    ReturnUpstreamResponse --> [*]
    ReturnCoreResponse --> [*]
```

Important consequence:
- if the **core** dies, Claude/Codex can keep talking to the upstream LLM through the gateway
- if the **gateway** dies, there is no failover because clients are pointed at the gateway itself

## 7. MCP HTTP Client Path

The MCP server is an HTTP client over stdio. It talks to the HTTP server configured in `Config::bind_addr()`.

In hybrid mode that means:
- MCP talks to the **gateway**
- the gateway then forwards `/v1/*` to the core
- if the core is down, MCP gets a clear `503 memoryOSS core unavailable`

```mermaid
stateDiagram-v2
    [*] --> McpServerStart
    McpServerStart --> BuildBaseUrl
    BuildBaseUrl --> ProbeGatewayHealth
    ProbeGatewayHealth --> WarningOnly: gateway unreachable
    ProbeGatewayHealth --> Ready: gateway healthy or warning emitted

    Ready --> ToolCall
    ToolCall --> Store
    ToolCall --> Recall
    ToolCall --> Update
    ToolCall --> Forget

    Store --> GatewayV1
    Recall --> GatewayV1
    Update --> GatewayV1
    Forget --> GatewayV1

    GatewayV1 --> CoreHealthy
    GatewayV1 --> CoreUnavailable503

    CoreHealthy --> ToolSuccess
    CoreUnavailable503 --> ToolError

    ToolSuccess --> [*]
    ToolError --> [*]
    WarningOnly --> Ready
```

## 8. Memory Lifecycle

`archived` is still a boolean overlay, not a dedicated enum state.

```mermaid
stateDiagram-v2
    [*] --> Active: manual store / batch store / MCP store
    [*] --> Candidate: proxy extraction

    Candidate --> Active: confirm_from_signal / feedback(confirm) / consolidation keep
    Candidate --> Contested: feedback(reject)

    Active --> Contested: feedback(reject)
    Active --> Stale: feedback(supersede)

    Contested --> Active: feedback(confirm) / confirm_from_signal
    Contested --> Stale: feedback(supersede)

    Stale --> Active: feedback(confirm) or confirm_from_signal if not superseded

    state ArchivedOverlay <<choice>>
    Active --> ArchivedOverlay: decay/archive command
    Candidate --> ArchivedOverlay: decay/archive command
    Contested --> ArchivedOverlay: decay/archive command
    Stale --> ArchivedOverlay: decay/archive command
    ArchivedOverlay --> [*]: archived=true excludes from indexes and injection
```

## 9. Write and Index Pipeline

```mermaid
stateDiagram-v2
    [*] --> IncomingWrite
    IncomingWrite --> Validate
    Validate --> Reject: invalid input / rate limit / backpressure / dedup
    Validate --> BuildMemory
    BuildMemory --> PersistSourceOfTruth
    PersistSourceOfTruth --> AppendOutboxEvent
    AppendOutboxEvent --> WakeIndexer
    WakeIndexer --> IndexedEventually

    state IndexedEventually {
        [*] --> PollOutbox
        PollOutbox --> LoadMemory
        LoadMemory --> VectorUpdate
        LoadMemory --> FtsUpdate
        LoadMemory --> IdfUpdate
        LoadMemory --> SpaceIndexUpdate
        VectorUpdate --> Checkpoint
        FtsUpdate --> Checkpoint
        IdfUpdate --> Checkpoint
        SpaceIndexUpdate --> Checkpoint
        Checkpoint --> GcOutbox
        GcOutbox --> [*]
    }

    IndexedEventually --> [*]
    Reject --> [*]
```

## 10. Setup Wizard

The wizard now writes a hybrid config:
- `hybrid_mode = true`
- `core_port = port + 1` (or env override)
- MCP registration for Claude/Codex
- local `BASE_URL` exports for supported clients

```mermaid
stateDiagram-v2
    [*] --> DetectEnvironment
    DetectEnvironment --> DetectClaude
    DetectEnvironment --> DetectCodex
    DetectEnvironment --> DetectApiKeys

    DetectClaude --> ChooseMemoryMode
    DetectCodex --> ChooseMemoryMode
    DetectApiKeys --> ChooseMemoryMode

    ChooseMemoryMode --> GenerateHybridConfig
    GenerateHybridConfig --> WriteConfig
    WriteConfig --> UpdateShellExports
    UpdateShellExports --> RegisterClaudeMcp
    RegisterClaudeMcp --> RegisterCodexMcp
    RegisterCodexMcp --> InstallClaudeStatusline
    InstallClaudeStatusline --> StartServe
    StartServe --> ReadyBanner
    ReadyBanner --> [*]
```

Current shell export logic:
- Claude detected or `ANTHROPIC_API_KEY` present -> write `ANTHROPIC_BASE_URL`
- Codex detected or `OPENAI_API_KEY` present -> write `OPENAI_BASE_URL`
- MCP is still registered in parallel

## 11. System-Level Verification Summary

These are the most important paths now verified in tests:

- Hybrid gateway fail-open covers all 4 auth combinations:
  - Codex OAuth
  - Codex API key
  - Claude OAuth
  - Claude API key
- Gateway proxies memory API calls to a healthy core
- `memoryoss serve` manages the core child and reports gateway health correctly
- Wizard still completes successfully across the scenario matrix

## 12. Residual Limits

These are architectural limits, not unverified guesses:

- MCP is **not** the transport failover for the same model request
- automatic failover exists only for **proxy traffic through the gateway**
- if the gateway process itself is down, clients pointed at the gateway still fail
- `/v1/*` memory API and MCP continue to depend on a healthy core

## 13. Coverage Implications for `tests/run_all.sh`

The current runner should cover:
- Rust formatting, linting, unit tests, integration tests
- CRUD write path
- recall path
- query explain path
- lifecycle feedback transitions
- lifecycle admin view
- MCP store/recall/update/forget
- proxy transport paths: OpenAI `models`, `chat/completions`, `responses`, Anthropic `messages`
- hybrid gateway fail-open for all 4 auth paths
- gateway-managed core startup
- sharing create/list/grant/remove/accessible
- sharing webhook delivery
- GDPR export/access/certified forget
- key rotation/list/revoke and restart/grace-expiry coverage
- decay, backup/restore, and embedding migration command coverage
- setup wizard smoke path
- setup wizard matrix
- TypeScript SDK build/test
- dependency audit when an offline advisory DB is available
