# memoryOSS

Persistent long-term memory for AI agents. memoryOSS runs as a local memory layer in front of the LLM API, with MCP always available for explicit memory tools.

> **Public Beta (v0.1.1)** — memoryOSS is a public beta for evaluation and testing. Features, APIs, and configuration may change without notice. Do not use for critical or regulated workloads. Please keep your own backups. This notice does not limit any mandatory statutory rights.

memoryOSS is for project context, preferences, prior fixes, and working history — not for replacing general world knowledge the model already has.

In internal retention, regression, and soak runs, memoryOSS continued to retrieve early high-signal memories even after the stored corpus grew into the tens of thousands.

The public tests page publishes the current benchmark and quality artifacts: 20k retention, long-memory regression, extraction quality, observed duplicate rate, active memory size, observed false-positive injection rate, and the supported Claude/Codex compatibility matrix.

In a separate constrained 10-task Claude benchmark, using recalled memory context instead of replaying the full task context reduced average input tokens by 44.4%. We treat this as evidence for repeated-task context compression in that workload, not as a universal promise of lower token usage in every workload.
## Quickstart

```bash
# Install from source
cargo install --git https://github.com/memoryOSScom/memoryOSS.git

# Or build locally
cargo build --release

# Interactive setup (creates config, registers MCP, starts the hybrid gateway)
memoryoss setup

# Or start directly with an existing config
memoryoss -c memoryoss.toml serve
```

The setup wizard auto-detects your environment, registers MCP for Claude/Codex, and enables local proxy exports when they are safe for the selected auth mode. OAuth-first setups keep MCP enabled without forcing global `BASE_URL` overrides, so login flows keep working. On a fresh setup it starts in **full** mode. If existing memories are already present, the wizard asks which memory mode you want and defaults that prompt to **full**.

If your auth setup changes later — for example from OAuth to API key or the other way around — run `memoryoss setup` again so memoryOSS can safely update the integration path.

## Hybrid Mode (Recommended)

For API-key setups, the default setup is hybrid:

- supported clients talk to the local memoryOSS gateway via `BASE_URL`
- MCP is also registered for Claude/Codex
- if the memory core is healthy, requests get recall/injection/extraction
- if the memory core is unavailable, the gateway falls back to direct upstream passthrough instead of breaking the client

For OAuth-first Claude/Codex setups, the wizard keeps MCP enabled and skips global `BASE_URL` exports by default so provider login continues to work normally. Claude can still use the proxy in supported OAuth paths; Codex OAuth stays MCP-first by default, and proxy mode for Codex requires an OpenAI API key.

Background fact extraction is only enabled automatically when a real provider API key is available. OAuth alone is enough for passthrough traffic, but not treated as a reliable extraction credential.

So you get transparent memory when available, plus explicit MCP tools when needed.

### After `memoryoss setup`

Start Claude Code or Codex normally. The wizard always registers MCP and writes local `BASE_URL` exports only when the chosen auth mode is proxy-safe.

### Manual proxy mode (optional)

If you want to point clients at the gateway yourself:

### Claude Code / Claude API

```bash
export ANTHROPIC_BASE_URL=http://127.0.0.1:8000/proxy/anthropic/v1
export ANTHROPIC_API_KEY=<your-existing-key-or-oauth-flow>
```

### OpenAI / Codex CLI

```bash
export OPENAI_BASE_URL=http://127.0.0.1:8000/proxy/v1
export OPENAI_API_KEY=<your-openai-api-key>
```

Both the Chat Completions API (`/v1/chat/completions`) and the Responses API (`/v1/responses`) are supported in manual proxy mode. For Codex OAuth, the supported path remains MCP-first without forced global `BASE_URL` overrides.

### What memoryOSS Adds

memoryOSS is most useful when the missing context is specific to you or your work:

- project conventions and architecture decisions
- previous fixes, incidents, and deployment notes
- user preferences and recurring workflows
- facts learned in earlier sessions with Claude or Codex

It is not meant to inject generic facts the model already knows.

### How It Works

```
Client (Claude/Codex)
    │
    ▼
memoryOSS Gateway (:8000)
    ├── 1. Try memory core
    ├── 2. Recall: find relevant project memories
    ├── 3. Inject: add scoped context to the request
    ├── 4. Forward: send to upstream LLM API
    ├── 5. Extract: pull candidate facts from response (async)
    └── 6. Fail open to direct upstream if the core is unavailable
    │
    ▼
Upstream API (Anthropic / OpenAI)
```

### Memory Modes

memoryOSS supports 4 memory modes, configurable per-server (in config) or per-request (via headers):

| Mode | Recall | Store | Use Case |
|------|--------|-------|----------|
| `full` | Yes | Yes | Full automatic memory — recall past context, store new facts |
| `readonly` | Yes | No | See past memories but don't save anything from this session |
| `after` | Yes (filtered) | Yes | Only recall memories after a specific date |
| `off` | No | No | Pure proxy passthrough, no memory involvement |

On a fresh setup the wizard defaults to **full**. If existing memories are already present, the wizard asks which mode to use and defaults that prompt to `full`.

### Per-Request Memory Control

Control memory behavior per request via headers:

| Header | Values | Effect |
|--------|--------|--------|
| `X-Memory-Mode` | `full` / `readonly` / `off` / `after` | Set memory mode for this request |
| `X-Memory-After` | `YYYY-MM-DD` | Only inject memories after this date (with mode `after`) |

Server operators can set a default mode in config (`default_memory_mode`) and disable client overrides with `allow_client_memory_control = false`.

## Configuration

Representative generated config

This is a representative hybrid config. The exact extraction provider/model and whether extraction is enabled depend on the tooling and real provider credentials the wizard detects.

```toml
[server]
host = "127.0.0.1"
port = 8000
hybrid_mode = true
core_port = 8001

[tls]
enabled = false
auto_generate = false

[auth]
jwt_secret = "..."
audit_hmac_secret = "..."

[[auth.api_keys]]
key = "ek_..."       # Generated by setup wizard
role = "admin"
namespace = "default"

[storage]
data_dir = "data"

[proxy]
enabled = true
passthrough_auth = true
passthrough_local_only = true              # Restrict passthrough to loopback clients by default
upstream_url = "https://api.openai.com/v1"
default_memory_mode = "full"               # Fresh setup default; existing installs are prompted
extraction_enabled = false                 # True only when a real provider API key is available
extract_provider = "openai"                # Or "claude", depending on detected auth
extract_model = "gpt-4o-mini"              # Or a provider-specific default such as Claude Haiku
allow_client_memory_control = true         # Allow X-Memory-Mode header (default: true)
max_memory_pct = 0.10                      # Max 10% of context window for memories
min_recall_score = 0.40                    # Minimum relevance score for injection (calibrated from internal query benchmarks)
min_channel_score = 0.15                   # Precision gate: min score in any channel (default: 0.15)
diversity_factor = 0.3                     # MMR diversity penalty (default: 0.3)

[[proxy.key_mapping]]
proxy_key = "ek_..."                       # Client-facing key
namespace = "default"                      # Memory isolation namespace
# upstream_key = "sk-..."                  # Optional per-client upstream key override

[logging]
level = "info"

[decay]
enabled = true
strategy = "age"
after_days = 14

[sharing]
allow_private_webhooks = false             # Keep localhost/private webhook targets blocked by default
```

## API Endpoints

### Proxy (transparent, no code changes)

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/proxy/v1/chat/completions` | POST | OpenAI Chat Completions proxy with memory |
| `/proxy/v1/responses` | POST | OpenAI Responses API proxy with memory (Codex CLI) |
| `/proxy/v1/models` | GET | Model list passthrough |
| `/proxy/anthropic/v1/messages` | POST | Anthropic proxy with memory |
| `/proxy/v1/debug/stats` | GET | Proxy metrics (auth required) |

### Memory API (direct access)

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/v1/auth/token` | POST | Get JWT from API key |
| `/v1/store` | POST | Store a memory |
| `/v1/store/batch` | POST | Store multiple memories |
| `/v1/recall` | POST | Semantic recall |
| `/v1/recall/batch` | POST | Batch recall |
| `/v1/update` | PATCH | Update a memory |
| `/v1/forget` | DELETE | Delete memories |
| `/v1/consolidate` | POST | Merge similar memories |
| `/health` | GET | Health check |

### Admin

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/v1/admin/keys` | GET | List API keys |
| `/v1/admin/keys/rotate` | POST | Rotate key |
| `/v1/admin/keys/{id}` | DELETE | Revoke a specific key |
| `/v1/admin/tokens` | POST | Create scoped tokens |
| `/v1/admin/cache/flush` | POST | Flush recall cache |
| `/v1/admin/cache/stats` | GET | Cache statistics |
| `/v1/admin/trust-stats` | GET | Memory trust scores |
| `/v1/admin/index-health` | GET | Index status |
| `/v1/admin/idf-stats` | GET | IDF index statistics |
| `/v1/admin/space-stats` | GET | Space index statistics |
| `/v1/admin/query-explain` | POST | Query debug/explain |
| `/v1/admin/intent-cache/stats` | GET | Intent cache statistics |
| `/v1/admin/intent-cache/flush` | POST | Flush intent cache |
| `/v1/admin/prefetch/stats` | GET | Prefetch statistics |
| `/v1/inspect/{id}` | GET | Inspect memory by ID |
| `/v1/peek/{id}` | GET | Peek at memory content |
| `/v1/source` | GET | AGPL-3.0 source code info |
| `/metrics` | GET | Prometheus-style metrics |

### Sharing (cross-namespace collaboration)

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/v1/admin/sharing/create` | POST | Create shared namespace |
| `/v1/admin/sharing/list` | GET | List shared namespaces |
| `/v1/admin/sharing/{name}` | DELETE | Delete shared namespace |
| `/v1/admin/sharing/{name}/grants/add` | POST | Add sharing grant |
| `/v1/admin/sharing/{name}/grants/list` | GET | List sharing grants |
| `/v1/admin/sharing/{name}/grants/{grant_id}` | DELETE | Remove sharing grant |
| `/v1/sharing/accessible` | GET | List accessible shared namespaces |

### GDPR Compliance

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/v1/export` | GET | Data export (all memories) |
| `/v1/memories` | GET | Data access (list memories) |
| `/v1/forget/certified` | DELETE | Certified deletion with audit trail |

## MCP Server

For Claude Desktop, Claude Code, or Codex MCP support:

```json
{
  "mcpServers": {
    "memory": {
      "command": "memoryoss",
      "args": ["-c", "memoryoss.toml", "mcp-server"]
    }
  }
}
```

Provides 4 tools: `store`, `recall`, `update`, `forget`. In the default setup MCP runs alongside the gateway. It is not the transport failover path; it is the explicit memory-tool path.

## Anthropic Local MCP Readiness

memoryOSS uses the same local stdio MCP path Anthropic documents for Claude Desktop extensions and local MCP directory submissions. The repo now maps the submission-critical pieces explicitly instead of implying they are already bundled:

| Requirement | memoryOSS mapping | Status |
|-------------|-------------------|--------|
| Local MCP server | `memoryoss mcp-server` over stdio via [server.json](server.json) and [src/mcp.rs](src/mcp.rs) | Ready |
| Public privacy policy | https://memoryoss.com/datenschutz.html | Ready |
| Support channel | `hello@memoryoss.com` and GitHub issues | Ready |
| Minimum three usable examples | See examples below | Ready |
| Tool titles + safety hints | Canonical mapping in [src/mcp.rs](src/mcp.rs) | Mapped |
| `manifest.json` / `.mcpb` package | Template documented below | Documented, not bundled |

Current packaging gaps are explicit:

- `rmcp 0.1.5` does not yet serialize MCP spec-era `title`, `readOnlyHint`, and `destructiveHint` fields on the live `tools/list` response, so memoryOSS is not claiming directory-ready wire compatibility yet.
- A portable `.mcpb` artifact and Windows release path are follow-up packaging work; the current documented install path is source/binary based.

### Current install path

For Claude Desktop, Claude Code, or Codex today, install memoryOSS locally and register the stdio server:

```json
{
  "mcpServers": {
    "memoryoss": {
      "command": "memoryoss",
      "args": ["-c", "/absolute/path/to/memoryoss.toml", "mcp-server"]
    }
  }
}
```

If you used the setup wizard, it writes the equivalent registration automatically.

### Marketplace tool annotation map

Anthropic requires a clear title and safety annotation per tool. memoryOSS currently maps them as follows in [src/mcp.rs](src/mcp.rs):

| Tool | Title | Safety annotation | Why |
|------|-------|-------------------|-----|
| `memoryoss_recall` | `Recall Relevant Memories` | `readOnlyHint: true` | Reads stored memory only |
| `memoryoss_store` | `Store New Memory` | `destructiveHint: true` | Persists new user/project data |
| `memoryoss_update` | `Update Existing Memory` | `destructiveHint: true` | Mutates existing stored data |
| `memoryoss_forget` | `Delete Stored Memory` | `destructiveHint: true` | Deletes stored data |

### Submission examples

These are the three practical examples prepared for a future Claude Desktop / local MCP submission:

1. Project continuity: “Before we touch deployment docs, recall anything we already decided about staging-first rollouts for this repo.”
2. Preference capture: “Remember that I only want concise summaries and never raw memory dumps unless I ask.”
3. Memory hygiene: “Find the outdated memory that says staging is optional and delete it if it is still stored.”

### Manifest / MCPB template

When the packaging step is added, the Anthropic Desktop extension bundle will follow a manifest shape like this:

```json
{
  "manifest_version": "0.3",
  "name": "memoryoss",
  "display_name": "memoryOSS",
  "version": "0.1.1",
  "description": "Persistent memory for AI agents with local MCP tools.",
  "author": {
    "name": "memoryOSS Contributors"
  },
  "homepage": "https://memoryoss.com",
  "documentation": "https://github.com/memoryOSScom/memoryOSS#anthropic-local-mcp-readiness",
  "support": "https://github.com/memoryOSScom/memoryOSS/issues",
  "privacy_policies": [
    "https://memoryoss.com/datenschutz.html"
  ],
  "server": {
    "type": "binary",
    "entry_point": "memoryoss",
    "mcp_config": {
      "command": "memoryoss",
      "args": ["-c", "${user_config.config_path}", "mcp-server"]
    }
  },
  "compatibility": {
    "platforms": ["darwin", "linux"]
  },
  "user_config": {
    "config_path": {
      "type": "string",
      "title": "memoryOSS config path",
      "description": "Absolute path to your memoryoss.toml file.",
      "required": true
    }
  }
}
```

This template is intentionally documented, not claimed as a shipped `.mcpb` artifact yet.

## CLI Commands

| Command | Description |
|---------|-------------|
| `memoryoss setup` | Interactive setup wizard |
| `memoryoss serve` | Start the configured server mode (monolith or hybrid gateway) |
| `memoryoss dev` | Start without TLS (development) |
| `memoryoss mcp-server` | Start as MCP server (stdio, embedded) |
| `memoryoss inspect <id>` | Inspect a memory |
| `memoryoss backup -o backup.tar.zst` | Backup all data |
| `memoryoss restore <path>` | Restore from backup |
| `memoryoss decay` | Run memory decay (age-based cleanup) |
| `memoryoss migrate` | Run schema migrations |
| `memoryoss migrate-embeddings` | Re-embed all memories with a new model |

## Architecture

- **Storage:** redb (embedded, crash-safe, single-file) — source of truth
- **Vector Index:** usearch (384-dim, AllMiniLM-L6-V2)
- **Full-Text Search:** tantivy (BM25 + structured metadata fields)
- **Recall:** 4-channel retrieval (vector 0.30 + BM25 0.30 + exact match 0.25 + recency 0.15) with IDF identifier boosting, precision gate, MMR diversity, and trust weighting
- **Extraction:** Async LLM-based fact extraction with quarantine (confidence scoring)
- **Indexer:** Async outbox-based pipeline with crash recovery across all namespaces
- **Group Committer:** Batches concurrent writes into single redb transactions
- **Trust Scoring:** 4-signal Bayesian (recency decay, source reputation, embedding coherence, access frequency) — persisted to redb
- **Encryption:** AES-256-GCM per-namespace (local key provider, AWS KMS and Vault stubs)
- **Security:** Constant-time key comparison, NFKC injection filtering, secret redaction (API keys, tokens, passwords), rate limiting, body size limits, path traversal protection

## Security

- All proxy endpoints require authentication
- Extracted memories start at confidence 0.2 (quarantine) — only memories with confidence ≥0.3 and passing trust scoring are injected
- Memory content is filtered against prompt injection patterns (NFKC-normalized)
- Secrets (API keys, tokens, passwords) are redacted before sending to extraction LLM
- API keys are compared in constant time (SHA-256 hash comparison)
- Request bodies capped at 2MB, responses at 10MB
- Rate limiting on all endpoints including passthrough

## Compatible With

Claude Code · OpenAI SDK · Codex CLI · Cursor · Aider · Continue · LangChain · Any OpenAI-compatible client

## License

AGPL-3.0 — free to use, modify, and self-host. If you offer it as a network service (SaaS), you must publish your source code. Commercial licenses available for SaaS providers who prefer not to open-source their modifications.
