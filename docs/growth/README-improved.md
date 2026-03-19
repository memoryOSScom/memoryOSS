# memoryOSS

[![License: AGPL-3.0](https://img.shields.io/badge/License-AGPL--3.0-blue.svg)](https://www.gnu.org/licenses/agpl-3.0)
[![GitHub stars](https://img.shields.io/github/stars/memoryOSScom/memoryOSS?style=social)](https://github.com/memoryOSScom/memoryOSS/stargazers)
[![Latest Release](https://img.shields.io/github/v/release/memoryOSScom/memoryOSS)](https://github.com/memoryOSScom/memoryOSS/releases/latest)
[![CI](https://github.com/memoryOSScom/memoryOSS/actions/workflows/ci.yml/badge.svg)](https://github.com/memoryOSScom/memoryOSS/actions/workflows/ci.yml)
[![Rust](https://img.shields.io/badge/Rust-stable-orange.svg)](https://www.rust-lang.org/)

**Persistent long-term memory for AI agents. Local-first. Open source.**

---

Agents forget everything between sessions. memoryOSS is a local memory runtime that recalls project context before each LLM call and extracts durable facts after each response — so your agent remembers decisions, conventions, fixes, and preferences across every session.

> **Public Beta (v0.2.0)** — Features, APIs, and configuration may change without notice. Do not use for critical or regulated workloads. Please keep your own backups. This notice does not limit any mandatory statutory rights.

---

## Demo

Verified demo scripts live in `docs/growth/demo.tape` and `docs/growth/demo-asciinema.sh`. Record and embed a short terminal walkthrough before replacing the repo README with this draft.

---

## Quick Install

```bash
curl -fsSL https://memoryoss.com/install.sh | sh
memoryoss setup --profile claude
```

Or grab a [prebuilt binary](https://github.com/memoryOSScom/memoryOSS/releases/latest) and run `memoryoss setup --profile claude`.

---

## What It Does

- **Recalls** relevant project memories before every LLM call — decisions, conventions, prior fixes
- **Extracts** durable facts from responses automatically (async, confidence-gated)
- **Injects** scoped context transparently via local gateway or explicitly via MCP tools
- **Fails open** — if the memory core is unavailable, requests pass through to the upstream API unchanged
- **Encrypts** all stored memories with AES-256-GCM, per-namespace
- **Ports** memories between tools via passport bundles, history replay, and adapter bridges

memoryOSS is for project context, preferences, prior fixes, and working history — not for replacing general world knowledge the model already has.

---

## Works With

| Client | Integration |
|--------|-------------|
| **Claude Code** | Gateway proxy + MCP |
| **Codex CLI** | Gateway proxy + MCP |
| **Cursor** | MCP + managed rules |
| **OpenAI SDK** | Gateway proxy (Chat Completions + Responses API) |
| **Aider** | OpenAI-compatible proxy |
| **Continue** | OpenAI-compatible proxy |
| **LangChain** | OpenAI-compatible proxy |
| Any OpenAI-compatible client | Gateway proxy |

Setup profiles: `claude`, `codex`, `cursor`, `team-node`, or `auto` (detects your environment).

---

## Architecture

```
Client (Claude / Codex / Cursor / any)
    |
    v
memoryOSS Gateway (:8000)
    |-- 1. Try memory core
    |-- 2. Recall: find relevant project memories
    |-- 3. Inject: add scoped context to the request
    |-- 4. Forward: send to upstream LLM API
    |-- 5. Extract: pull candidate facts from response (async)
    +-- 6. Fail open to direct upstream if the core is unavailable
    |
    v
Upstream API (Anthropic / OpenAI)
```

**Internals:**

- **Storage:** redb (embedded, crash-safe, single-file)
- **Vector Index:** usearch (dimension follows the configured embedding model)
- **Full-Text Search:** tantivy (BM25 + structured metadata fields)
- **Recall:** 4-channel retrieval (vector 0.30 + BM25 0.30 + exact match 0.25 + recency 0.15) with IDF identifier boosting, precision gate, MMR diversity, and trust weighting
- **Extraction:** Async LLM-based fact extraction with quarantine (confidence scoring)
- **Indexer:** Async outbox-based pipeline with crash recovery across all namespaces
- **Group Committer:** Batches concurrent writes into single redb transactions
- **Trust Scoring:** 4-signal Bayesian (recency decay, source reputation, embedding coherence, access frequency)
- **Encryption:** AES-256-GCM per-namespace (local key provider or Vault Transit)
- **Security:** Constant-time key comparison, NFKC injection filtering, secret redaction, rate limiting, body size limits, path traversal protection

---

## Benchmarks

| Metric | Value | Notes |
|--------|-------|-------|
| **Token efficiency** | 44.4% reduction | Recalled memory context vs. replaying full task context (10-task Claude benchmark) |
| **Retention** | 20,000+ memories | Early high-signal memories still retrieved after corpus grows to tens of thousands |
| **Test suite** | 302 tests passing | Across unit, integration, conformance, and soak runs |

The benchmark surface publishes internal proof loops (20K retention, long-memory regression, soak stability, universal memory loop proof, extraction quality, duplicate rate, false-positive injection rate) plus an open comparison lane with published fixture memories, queries, expected anchors, and explicit failure thresholds.

A multilingual calibration lane runs alongside the English default lane before any embedding-model change is considered.

---

## Setup

### Prebuilt Binary (Linux / macOS)

```bash
curl -L https://github.com/memoryOSScom/memoryOSS/releases/latest/download/memoryoss-linux-x86_64.tar.gz -o memoryoss.tar.gz
tar xzf memoryoss.tar.gz
sudo install -m 0755 memoryoss /usr/local/bin/memoryoss
memoryoss setup --profile claude
```

Available archives: `memoryoss-linux-x86_64`, `memoryoss-linux-aarch64`, `memoryoss-darwin-x86_64`, `memoryoss-darwin-aarch64`, `memoryoss-windows-x86_64`.

Every archive ships with a matching `.sha256` file and GitHub artifact attestation.

### From Source

```bash
cargo install --git https://github.com/memoryOSScom/memoryOSS.git
memoryoss setup
```

### Docker

```bash
docker run -d \
  --name memoryoss \
  -p 8000:8000 \
  -v "$(pwd)/memoryoss.toml:/config/memoryoss.toml:ro" \
  -v memoryoss-data:/data \
  ghcr.io/memoryosscom/memoryoss:latest \
  -c /config/memoryoss.toml serve
```

Or with `docker compose`:

```yaml
services:
  memoryoss:
    image: ghcr.io/memoryosscom/memoryoss:latest
    restart: unless-stopped
    ports:
      - "8000:8000"
    volumes:
      - ./memoryoss.toml:/config/memoryoss.toml:ro
      - memoryoss-data:/data
    command: ["-c", "/config/memoryoss.toml", "serve"]

volumes:
  memoryoss-data:
```

Before starting the container, copy [memoryoss.toml.example](memoryoss.toml.example) to `memoryoss.toml`, set `[server].host = "0.0.0.0"`, `[storage].data_dir = "/data"`, and fill in your API keys.

### Windows

```powershell
Invoke-WebRequest `
  https://github.com/memoryOSScom/memoryOSS/releases/latest/download/memoryoss-windows-x86_64.zip `
  -OutFile memoryoss-windows-x86_64.zip
Expand-Archive .\memoryoss-windows-x86_64.zip -DestinationPath .\memoryoss
cd .\memoryoss
.\memoryoss.exe setup
```

Windows notes:
- Keep `memoryoss.toml`, `memoryoss.key`, and the data directory in a user-only location such as `%LOCALAPPDATA%\memoryoss`
- Use NTFS ACLs to protect secrets instead of Unix-style `chmod 600`
- Current Windows builds use a portable brute-force vector backend instead of `usearch`, so very large-memory recall will be slower than on Linux/macOS

---

## Hybrid Mode (Recommended)

For API-key setups, the default setup is hybrid:

- Supported clients talk to the local memoryOSS gateway via `BASE_URL`
- MCP is also registered for Claude/Codex
- If the memory core is healthy, requests get recall/injection/extraction
- If the memory core is unavailable, the gateway falls back to direct upstream passthrough

For OAuth-first Claude/Codex setups, the wizard keeps MCP enabled and skips global `BASE_URL` exports so provider login continues to work normally.

Background fact extraction is only enabled automatically when a real provider API key is available. OAuth alone is enough for passthrough traffic but not treated as a reliable extraction credential.

### Manual Proxy Mode

**Claude Code / Claude API:**
```bash
export ANTHROPIC_BASE_URL=http://127.0.0.1:8000/proxy/anthropic/v1
export ANTHROPIC_API_KEY=<your-existing-key-or-oauth-flow>
```

**OpenAI / Codex CLI:**
```bash
export OPENAI_BASE_URL=http://127.0.0.1:8000/proxy/v1
export OPENAI_API_KEY=<your-openai-api-key>
```

Both the Chat Completions API (`/v1/chat/completions`) and the Responses API (`/v1/responses`) are supported.

---

## Memory Modes

| Mode | Recall | Store | Use Case |
|------|--------|-------|----------|
| `full` | Yes | Yes | Full automatic memory — recall past context, store new facts |
| `readonly` | Yes | No | See past memories but don't save anything from this session |
| `after` | Yes (filtered) | Yes | Only recall memories after a specific date |
| `off` | No | No | Pure proxy passthrough, no memory involvement |

On a fresh setup the wizard defaults to **full**. If existing memories are already present, the wizard asks which mode to use.

### Per-Request Memory Control

| Header | Values | Effect |
|--------|--------|--------|
| `X-Memory-Mode` | `full` / `readonly` / `off` / `after` | Set memory mode for this request |
| `X-Memory-After` | `YYYY-MM-DD` | Only inject memories after this date (with mode `after`) |
| `X-Memory-Policy-Confirm` | `true` / `1` / `yes` | Confirm a risky action flagged by the policy firewall |

---

## Configuration

Representative hybrid config (the exact extraction provider/model depends on what the wizard detects):

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

[embeddings]
model = "all-minilm-l6-v2"                # Or bge-small-en-v1.5 / bge-base-en-v1.5 / bge-large-en-v1.5

[proxy]
enabled = true
passthrough_auth = true
passthrough_local_only = true
upstream_url = "https://api.openai.com/v1"
default_memory_mode = "full"
extraction_enabled = false                 # True only when a real provider API key is available
extract_provider = "openai"                # Or "claude", depending on detected auth
extract_model = "gpt-4o-mini"
allow_client_memory_control = true
max_memory_pct = 0.10
min_recall_score = 0.40
min_channel_score = 0.15
diversity_factor = 0.3
identifier_first_routing = true
memory_coprocessor = "off"                 # Or "local_heuristic" for deterministic local extraction

[[proxy.key_mapping]]
proxy_key = "ek_..."
namespace = "default"

[logging]
level = "info"

[decay]
enabled = true
strategy = "age"
after_days = 14

[sharing]
allow_private_webhooks = false
```

### Managed Key Providers

- `[encryption].provider = "local"` — default, fully supported
- `[encryption].provider = "vault"` — Vault Transit (bootstrap, reload, rotation with grace window, fail-closed errors)
- `aws_kms` — accepted only to fail closed with an explicit unsupported error

---

## MCP Server

For Claude Desktop, Claude Code, or Codex MCP support:

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

Provides 4 tools: `recall`, `store`, `update`, `forget`.

If you used the setup wizard, it writes this registration automatically.

---

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
| `/v1/recall` | POST | Semantic recall with summary/evidence view |
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
| `/v1/admin/trust/fabric` | GET | Trust identities, catalogs, pins, revocations, replacement links |
| `/v1/admin/trust/catalog/export` | GET | Export trust fabric as portable signer catalog |
| `/v1/admin/trust/catalog/import` | POST | Import shared signer catalog into local trust fabric |
| `/v1/admin/trust/pin` | POST | Locally pin a trust identity |
| `/v1/admin/trust/unpin` | POST | Remove a local trust pin |
| `/v1/admin/trust/register` | POST | Register identity for artifact signing |
| `/v1/admin/trust/revoke` | POST | Revoke a trust identity with optional replacement |
| `/v1/admin/trust/restore` | POST | Re-trust a previously revoked identity |
| `/v1/admin/trust/sign` | POST | Sign a portable artifact |
| `/v1/admin/trust/verify` | POST | Verify a signed artifact against trust fabric |
| `/v1/admin/index-health` | GET | Index status |
| `/v1/admin/idf-stats` | GET | IDF index statistics |
| `/v1/admin/space-stats` | GET | Space index statistics |
| `/v1/admin/query-explain` | POST | Query debug/explain with summary + evidence drill-down |
| `/v1/admin/hud` | GET | Operator HUD (JSON or `?format=html`) |
| `/v1/admin/lifecycle` | GET | Lifecycle state summary |
| `/v1/admin/recent` | GET | Recent injections, extractions, feedbacks, consolidations |
| `/v1/admin/team/governance` | GET | Branch/scope governance overview |
| `/v1/admin/team/governance/propose` | POST | Propose a governed team-memory write |
| `/v1/admin/review-queue` | GET | Review inbox with suggested actions |
| `/v1/admin/review/action` | POST | Confirm, reject, or supersede via review keys |
| `/v1/admin/intent-cache/stats` | GET | Intent cache statistics |
| `/v1/admin/intent-cache/flush` | POST | Flush intent cache |
| `/v1/admin/prefetch/stats` | GET | Prefetch statistics |
| `/v1/inspect/{id}` | GET | Inspect memory by ID |
| `/v1/peek/{id}` | GET | Peek at memory content |
| `/v1/source` | GET | AGPL-3.0 source code info |
| `/metrics` | GET | Prometheus-style metrics |

### Portability & GDPR

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/v1/export` | GET | Data export (all memories) |
| `/v1/history/{id}` | GET | Lineage, review chain, contradictions, state transitions |
| `/v1/history/{id}/bundle` | GET | Export a deterministic history replay bundle |
| `/v1/history/replay` | POST | Replay a history bundle into an empty target namespace |
| `/v1/passport/export` | GET | Selective portable memory passport export |
| `/v1/passport/import` | POST | Dry-run or apply a portable passport bundle |
| `/v1/bundles/export` | GET | Export a versioned memory bundle envelope |
| `/v1/bundles/preview` | POST | Preview bundle metadata without importing |
| `/v1/bundles/validate` | POST | Validate bundle envelope and trust state |
| `/v1/bundles/diff` | POST | Diff two bundle envelopes offline |
| `/v1/adapters/export` | GET | Export as foreign client artifact |
| `/v1/adapters/import` | POST | Normalize and import a foreign client artifact |
| `/v1/connectors` | GET | List ambient connector kinds |
| `/v1/connectors/ingest` | POST | Ingest an ambient connector signal |
| `/v1/runtime/contract` | GET | Versioned portable runtime contract |
| `/v1/memories` | GET | List memories |
| `/v1/forget/certified` | DELETE | Certified deletion with audit trail |

---

## Proxy Injection Format

Proxy memory injection uses a two-level format inside `<memory_context>`:
- `<summary>` blocks give the compact task-facing memory
- `<evidence>` blocks carry bounded preview snippets plus provenance

For recognized task classes (`deploy`, `bugfix`, `review`, `style`), memoryOSS compiles an explicit `<task_state>` block separating facts, constraints, recent actions, open questions, and evidence.

### Policy Firewall

For risky action prompts (`deploy`, `delete`, `exfiltrate`, `override`), the proxy evaluates whether the request should `warn`, `block`, or `require_confirmation`. Response headers:

- `x-memory-policy-decision`
- `x-memory-policy-actions`
- `x-memory-policy-match-count`
- `x-memory-policy-confirmed`

If the firewall returns `require_confirmation`, resend with `X-Memory-Policy-Confirm: true`. Hard blocks return `403` before the upstream model is called.

### Local Memory Coprocessor

Set `memory_coprocessor = "local_heuristic"` to keep extraction fully local and deterministic for high-signal decisions (deploy policies, branch habits, proxy env exports) instead of calling a remote extraction model.

---

## Cross-App Adapter Bridges

Import/export memories across tools:

- **`claude_project`** — Markdown-style Claude Project knowledge files
- **`cursor_rules`** — `.mdc` / rule-style Cursor memory files
- **`git_history`** — Recent Git commit history as candidate memories

```bash
memoryoss adapter import --kind cursor_rules .cursor/rules/review.mdc --namespace test --dry-run
memoryoss adapter export --kind claude_project --namespace test -o claude-project.md
memoryoss adapter import --kind git_history . --namespace test --dry-run
```

Guarantees: foreign artifacts are normalized into runtime records (not raw blobs), carry opaque provenance plus source tags, and dry-run previews show create/merge/conflict before any write.

---

## Ambient Connector Mesh

Opt-in connectors for ambient signals: `editor`, `terminal`, `browser`, `docs`, `ticket`, `calendar`, `pull_request`, `incident`.

Defaults are conservative: disabled, sensitive fragments redacted, raw excerpts not captured unless explicitly allowed.

```bash
memoryoss connector ingest --kind terminal --namespace test \
  --summary "Use cargo fmt before release commits." \
  --source-ref "terminal://release/42" --dry-run
```

---

## Portable Runtime Contract

memoryOSS exposes a versioned runtime contract at `/v1/runtime/contract`.

**Stable semantics:** namespace-scoped records, opaque provenance and lifecycle metadata, explicit merge and supersede lineage, contract-tagged portability export.

**Outside the stable contract for now:** retrieval strategy details, broad replay/branch semantics, first-class typed policy and evidence objects.

### Runtime Conformance Kit

The contract is backed by a versioned conformance kit in [conformance/](conformance/README.md) with JSON Schemas, canonical fixtures, reference reader/writer paths (Rust, Python, TypeScript), and an automated compatibility harness.

### Memory Bundle Envelope

Portable artifacts are wrapped in `memoryoss.bundle.v1alpha1` with bundle version, portable URI, outer-envelope integrity, runtime signature, and embedded signature chain.

```bash
memoryoss bundle export --kind passport --namespace test --scope project -o project.membundle.json
memoryoss bundle validate project.membundle.json
memoryoss reader open project.membundle.json
memoryoss reader diff old.membundle.json new.membundle.json --format html
```

---

## CLI Commands

| Command | Description |
|---------|-------------|
| `memoryoss setup` | Interactive setup wizard |
| `memoryoss serve` | Start the server (monolith or hybrid gateway) |
| `memoryoss dev` | Start without TLS (development) |
| `memoryoss mcp-server` | Start as MCP server (stdio) |
| `memoryoss status` | Namespace health, lifecycle counts, worker state, index health |
| `memoryoss doctor` | Diagnose config, auth, database, index, and integration drift |
| `memoryoss recent` | Recent injections, extractions, feedbacks, consolidations |
| `memoryoss hud` | Terminal HUD for search/review/import/export loops |
| `memoryoss bundle export` | Export a portable memory bundle envelope |
| `memoryoss bundle preview` | Preview bundle metadata without import |
| `memoryoss bundle validate` | Validate envelope, artifact integrity, trust state |
| `memoryoss bundle diff` | Diff two bundle envelopes offline |
| `memoryoss reader open` | Open a bundle in the offline universal reader |
| `memoryoss reader diff` | Diff two offline artifacts (text, JSON, or HTML) |
| `memoryoss review queue` | List the review inbox |
| `memoryoss review confirm` | Confirm a queue item |
| `memoryoss review reject` | Reject a queue item |
| `memoryoss review supersede` | Supersede one queue item with another |
| `memoryoss passport export` | Export a selective portable passport bundle |
| `memoryoss passport import` | Preview merge/conflict then apply a bundle |
| `memoryoss adapter import` | Normalize and import a foreign client artifact |
| `memoryoss adapter export` | Export runtime state as a foreign client artifact |
| `memoryoss connector list` | Show ambient connector kinds and privacy defaults |
| `memoryoss connector ingest` | Preview or store an ambient connector candidate |
| `memoryoss history show` | Show lineage and review chain for a memory |
| `memoryoss history export` | Export a deterministic history replay bundle |
| `memoryoss history replay` | Replay a history bundle into an empty namespace |
| `memoryoss history branch` | Branch-from-here into a new empty namespace |
| `memoryoss conformance normalize` | Reference reader/writer for published fixtures |
| `memoryoss inspect` | Inspect a memory |
| `memoryoss backup` | Backup all data |
| `memoryoss restore` | Restore from backup |
| `memoryoss decay` | Run memory decay (age-based cleanup) |
| `memoryoss migrate` | Run schema migrations |
| `memoryoss migrate-embeddings` | Re-embed all memories with a new model |

---

## Update Channels

| Channel | Source |
|---------|--------|
| **Stable** | GitHub Releases `vX.Y.Z` + `ghcr.io/memoryosscom/memoryoss:latest` |
| **Beta** | Prerelease tags `vX.Y.Z-beta.N` + `ghcr.io/memoryosscom/memoryoss:beta` |
| **Rollback** | Reinstall last known-good release, restore pre-update backup |

Recommended upgrade:

```bash
memoryoss backup -o memoryoss-preupdate.tar.zst --include-key
curl -L https://github.com/memoryOSScom/memoryOSS/releases/latest/download/memoryoss-linux-x86_64.tar.gz -o memoryoss.tar.gz
tar xzf memoryoss.tar.gz
sudo install -m 0755 memoryoss /usr/local/bin/memoryoss
memoryoss doctor
memoryoss migrate
```

If something breaks, roll back:

```bash
sudo install -m 0755 ./previous-memoryoss /usr/local/bin/memoryoss
memoryoss restore memoryoss-preupdate.tar.zst
memoryoss doctor
```

---

## Team Rollout

```bash
memoryoss setup --profile team-node --team-manifest ./team-bootstrap.json
memoryoss doctor --repair
```

Replays managed Claude/Codex/Cursor surfaces plus the shared trust catalog for the current workstation. `doctor --repair` handles drift.

---

## Security

- All proxy endpoints require authentication
- Extracted memories start at confidence 0.2 (quarantine) — only memories with confidence >= 0.3 and passing trust scoring are injected
- Memory content is filtered against prompt injection patterns (NFKC-normalized)
- Secrets (API keys, tokens, passwords) are redacted before sending to extraction LLM
- API keys are compared in constant time (SHA-256 hash comparison)
- Request bodies capped at 2MB, responses at 10MB
- Rate limiting on all endpoints including passthrough

---

## Contributing

Contributions welcome. Open an issue or pull request at [github.com/memoryOSScom/memoryOSS](https://github.com/memoryOSScom/memoryOSS).

---

## License

[AGPL-3.0](https://www.gnu.org/licenses/agpl-3.0.html) — free to use, modify, and self-host. If you offer it as a network service (SaaS), you must publish your source code. Commercial licenses available for SaaS providers who prefer not to open-source their modifications.
