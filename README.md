# memoryOSS

Persistent long-term memory for AI agents. memoryOSS runs as a local memory layer in front of the LLM API, with MCP always available for explicit memory tools. The runtime contract is versioned and machine-readable, so the product can be described precisely as a portable memory runtime instead of only a proxy.

> **Public Beta (v0.1.1)** — memoryOSS is a public beta for evaluation and testing. Features, APIs, and configuration may change without notice. Do not use for critical or regulated workloads. Please keep your own backups. This notice does not limit any mandatory statutory rights.

memoryOSS is for project context, preferences, prior fixes, and working history — not for replacing general world knowledge the model already has.

## Why It Exists

Agents lose project context between sessions. memoryOSS persists the things that actually matter in day-to-day work: decisions, conventions, fixes, constraints, and user preferences.

In public retention, regression, and soak runs, memoryOSS continued to retrieve early high-signal memories even after the stored corpus grew into the tens of thousands. The public test page publishes the current proof set: 20k retention, long-memory regression, soak stability, a universal memory loop proof, extraction quality, observed duplicate rate, active memory size, observed false-positive injection rate, and the supported Claude/Codex compatibility matrix.

In a separate constrained 10-task Claude benchmark, using recalled memory context instead of replaying the full task context reduced average input tokens by 44.4%. We treat this as evidence for repeated-task context compression in that workload, not as a universal promise of lower token usage in every workload.

## How It Works

1. Run memoryOSS locally as a small proxy and memory service.
2. Let it recall relevant project memory before the LLM call.
3. Let it extract durable project-specific facts after the response.
4. Keep explicit memory tools available through MCP when you want direct control.

## Fastest Install

Use a prebuilt release binary from GitHub Releases. Current archives:

- `memoryoss-linux-x86_64.tar.gz`
- `memoryoss-linux-aarch64.tar.gz`
- `memoryoss-darwin-x86_64.tar.gz`
- `memoryoss-darwin-aarch64.tar.gz`
- `memoryoss-windows-x86_64.zip`

Linux/macOS example:

```bash
curl -L https://github.com/memoryOSScom/memoryOSS/releases/latest/download/memoryoss-linux-x86_64.tar.gz -o memoryoss.tar.gz
tar xzf memoryoss.tar.gz
sudo install -m 0755 memoryoss /usr/local/bin/memoryoss
memoryoss setup
```

Windows has a PowerShell example in [Windows](#windows).

Every published archive ships with a matching `.sha256` file, and the release workflows now attach a GitHub artifact attestation for the built archives before the GitHub Release is published. That is the signed install surface for the binary/update path today; memoryOSS is not claiming OS-native notarization or MSI/PKG installers yet.

## From Source

```bash
cargo install --git https://github.com/memoryOSScom/memoryOSS.git
memoryoss setup
```

## Docker

memoryOSS also ships as an official container image on GHCR:

- `ghcr.io/memoryosscom/memoryoss:latest`
- `ghcr.io/memoryosscom/memoryoss:<version>`

This is an additional self-hosting path, not a replacement for the source/binary install.

Before starting the container:

- copy [memoryoss.toml.example](memoryoss.toml.example) to `memoryoss.toml`
- set `[server].host = "0.0.0.0"` so the service is reachable outside the container
- set `[storage].data_dir = "/data"` so the persisted database lands on the mounted volume
- fill in your real API keys and secrets

### Minimal `docker run`

```bash
docker run -d \
  --name memoryoss \
  -p 8000:8000 \
  -v "$(pwd)/memoryoss.toml:/config/memoryoss.toml:ro" \
  -v memoryoss-data:/data \
  ghcr.io/memoryosscom/memoryoss:latest \
  -c /config/memoryoss.toml serve
```

### Minimal `docker compose`

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

## Windows

Official GitHub Releases also publish a Windows archive with `memoryoss.exe`:

- `memoryoss-windows-x86_64.zip`

### Minimal PowerShell install

```powershell
Invoke-WebRequest `
  https://github.com/memoryOSScom/memoryOSS/releases/latest/download/memoryoss-windows-x86_64.zip `
  -OutFile memoryoss-windows-x86_64.zip
Expand-Archive .\memoryoss-windows-x86_64.zip -DestinationPath .\memoryoss
Invoke-WebRequest `
  https://raw.githubusercontent.com/memoryOSScom/memoryOSS/main/memoryoss.toml.example `
  -OutFile .\memoryoss\memoryoss.toml
cd .\memoryoss
.\memoryoss.exe setup
```

Or, if you already have a config:

```powershell
.\memoryoss.exe -c .\memoryoss.toml serve
```

Windows-specific notes:

- keep `memoryoss.toml`, `memoryoss.key`, and the data directory in a user-only location such as `%LOCALAPPDATA%\memoryoss`
- use NTFS ACLs to protect secrets instead of Unix-style `chmod 600`
- if you set an absolute Windows `data_dir`, use a TOML-safe path such as `'C:\Users\you\AppData\Local\memoryoss\data'`
- if you need remote access from outside the machine, change `[server].host` to `0.0.0.0`; otherwise keep the default loopback host
- current Windows builds use a portable brute-force vector backend instead of `usearch`, so very large-memory recall will be slower than on Linux/macOS

The setup wizard auto-detects your environment, registers MCP for Claude/Codex, and enables local proxy exports when they are safe for the selected auth mode. OAuth-first setups keep MCP enabled without forcing global `BASE_URL` overrides, so login flows keep working. On a fresh setup it starts in **full** mode. If existing memories are already present, the wizard asks which memory mode you want and defaults that prompt to **full**.

If your auth setup changes later — for example from OAuth to API key or the other way around — run `memoryoss setup` again so memoryOSS can safely update the integration path.

## Update Channels

memoryOSS now treats install and upgrade flow as an explicit product surface:

- stable channel: GitHub Releases tagged `vX.Y.Z` plus `ghcr.io/memoryosscom/memoryoss:latest`
- beta channel: prerelease tags such as `vX.Y.Z-beta.N` or `vX.Y.Z-rc.N` plus `ghcr.io/memoryosscom/memoryoss:beta`
- rollback channel: reinstall the last known-good release archive or pinned container tag, then restore the pre-update backup

The release smoke workflow exercises install, checksum verification, doctor, migrate, failed-update detection, and rollback recovery on Linux, macOS, and Windows before a smoke branch is promoted to a published release.

Recommended binary upgrade flow:

```bash
memoryoss backup -o memoryoss-preupdate.tar.zst --include-key
curl -L https://github.com/memoryOSScom/memoryOSS/releases/latest/download/memoryoss-linux-x86_64.tar.gz -o memoryoss.tar.gz
tar xzf memoryoss.tar.gz
sudo install -m 0755 memoryoss /usr/local/bin/memoryoss
memoryoss doctor
memoryoss migrate
```

If `doctor`, `migrate`, or post-upgrade startup fails:

```bash
sudo install -m 0755 ./previous-memoryoss /usr/local/bin/memoryoss
memoryoss restore memoryoss-preupdate.tar.zst
memoryoss doctor
```

For container installs, the equivalent rollback is to pin the previous image tag and keep the same mounted data volume plus the same backup/restore sequence.

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
| `X-Memory-Policy-Confirm` | `true` / `1` / `yes` | Confirm a risky action that the policy firewall marked as `require_confirmation` |

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
identifier_first_routing = true            # Route path/endpoint/env-var style queries through lexical-first reranking
memory_coprocessor = "off"                 # Or "local_heuristic" for deterministic local extraction

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
| `/v1/recall` | POST | Semantic recall with raw memories plus summary/evidence view |
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
| `/v1/admin/trust/fabric` | GET | Portable trust identities, revocations, and replacement links |
| `/v1/admin/trust/register` | POST | Register an author, device, or sync-peer identity for artifact signing |
| `/v1/admin/trust/revoke` | POST | Revoke a portable trust identity and attach an optional replacement |
| `/v1/admin/trust/restore` | POST | Re-trust a previously revoked identity without silent data loss |
| `/v1/admin/trust/sign` | POST | Sign a portable bundle, passport, history artifact, or sync-peer descriptor |
| `/v1/admin/trust/verify` | POST | Verify a signed portable artifact against the current trust fabric |
| `/v1/admin/index-health` | GET | Index status |
| `/v1/admin/idf-stats` | GET | IDF index statistics |
| `/v1/admin/space-stats` | GET | Space index statistics |
| `/v1/admin/query-explain` | POST | Query debug/explain with summary + evidence drill-down |
| `/v1/admin/hud` | GET | Unified operator HUD as JSON or `?format=html` desktop view |
| `/v1/admin/lifecycle` | GET | Lifecycle state summary and latest memories |
| `/v1/admin/recent` | GET | Recent injections, extractions, feedbacks, and consolidations |
| `/v1/admin/team/governance` | GET | Branch/scope governance overview for governed team memory |
| `/v1/admin/team/governance/propose` | POST | Propose a governed team-memory write without dedup rejection |
| `/v1/admin/review-queue` | GET | Candidate / contested / rejected review inbox with suggested actions |
| `/v1/admin/review/action` | POST | Confirm, reject, or supersede via review keys |
| `/v1/admin/intent-cache/stats` | GET | Intent cache statistics |
| `/v1/admin/intent-cache/flush` | POST | Flush intent cache |
| `/v1/admin/prefetch/stats` | GET | Prefetch statistics |
| `/v1/inspect/{id}` | GET | Inspect memory by ID |
| `/v1/peek/{id}` | GET | Peek at memory content |
| `/v1/source` | GET | AGPL-3.0 source code info |
| `/metrics` | GET | Prometheus-style metrics |

Proxy memory injection now uses a two-level format inside `<memory_context>`:
- `<summary>` blocks give the compact task-facing memory
- `<evidence>` blocks carry bounded preview snippets plus provenance so operators can drill down without dumping raw stored content into every prompt

For recognized task classes (`deploy`, `bugfix`, `review`, `style`), memoryOSS now compiles an explicit `<task_state>` block instead of a flat memory list. That compiled state separates:
- facts
- constraints
- recent actions
- open questions
- evidence

The admin explain surface exposes the same compiled task state plus the input memories and condensation decisions that produced it.

Query explain now also returns a `policy_firewall` section. For risky action prompts (`deploy`, `delete`, `exfiltrate`, `override`) the same policy evaluation used by the proxy reports whether the request would `warn`, `block`, or `require_confirmation`, plus the exact policy memories that caused that intervention.

For privacy-sensitive flows, the proxy can switch to a local memory coprocessor instead of a remote extraction model. Set `memory_coprocessor = "local_heuristic"` to keep a bounded rule set fully local and deterministic for high-signal decisions such as deploy policies, branch habits, and proxy env exports. The proxy debug stats expose the active coprocessor mode so operators can verify when the local path is in effect.

The operator HUD at `/v1/admin/hud` folds that together with lifecycle, recent activity, review inbox, and import/export launcher actions. Use JSON for scripting or `?format=html` for a browser-ready desktop dashboard.

Governed team memory now rides on the same review and history primitives. `POST /v1/admin/team/governance/propose` records branch, scope, owners, watchlists, and whether the scope is review-required; `GET /v1/admin/team/governance` summarizes duplicate writes, stale merged policy, and conflicting decisions per branch. When a governed memory requires review, `/v1/admin/review/action` only allows `confirm` or `supersede` from a listed owner, and the merge provenance survives `history/replay` and passport export/import.

Proxy responses surface the same preflight via headers:
- `x-memory-policy-decision`
- `x-memory-policy-actions`
- `x-memory-policy-match-count`
- `x-memory-policy-confirmed`

If the firewall returns `require_confirmation`, resend the request with `X-Memory-Policy-Confirm: true`. Hard blocks return `403` before the upstream model is called.

### Portable Runtime Contract

memoryOSS now exposes a versioned runtime contract at `/v1/runtime/contract`.

Stable runtime semantics today:
- namespace-scoped memory records
- opaque provenance and lifecycle metadata
- explicit merge and supersede lineage
- contract-tagged portability export

Explicitly outside the stable runtime contract for now:
- retrieval strategy details like confidence gating and identifier-first routing
- broad replay/branch semantics beyond the current safe empty-target scope
- first-class typed policy and evidence objects with full import/export fidelity

### Runtime Conformance Kit

The runtime contract is now backed by a versioned conformance kit in [conformance/](conformance/README.md).

It ships:
- JSON Schemas for the current runtime contract, passport bundle, and history bundle lines
- canonical fixtures for `memoryoss.runtime.v1alpha1`, `memoryoss.passport.v1alpha1`, and `memoryoss.history.v1alpha1`
- reference reader/writer paths in Rust, Python, and TypeScript
- an automated compatibility harness in `python3 tests/run_conformance_kit.py`

Reference commands:

```bash
memoryoss conformance normalize --kind runtime_contract --input conformance/fixtures/runtime-contract.json --output /tmp/runtime.json
python3 tests/reference_conformance.py --kind passport --input conformance/fixtures/passport-bundle.json --output /tmp/passport.json
node sdk/typescript/dist/conformance.js --kind history --input conformance/fixtures/history-bundle.json --output /tmp/history.json
```

Compatibility policy:
- published artifact lines are immutable; breaking wire changes require a new `contract_id` or `bundle_version`
- readers must ignore unknown additive fields inside a published line
- once a successor line ships, memoryOSS keeps reader compatibility for the previous published line for at least two minor releases
- the conformance harness is the authoritative pass/fail gate for published lines

LTS policy for the current published line set:

| Surface | Current write line | Read / verify window | Migration / rollback guarantee |
| --- | --- | --- | --- |
| Runtime contract | `memoryoss.runtime.v1alpha1` | `N`, `N-1`, `N-2` published fixture snapshots | `memoryoss conformance normalize` must still accept published snapshots before a line leaves support |
| Passport bundle | `memoryoss.passport.v1alpha1` | `N`, `N-1`, `N-2` in the reader and passport import preview | Dry-run import must stay available for published fixtures during the support window |
| History bundle | `memoryoss.history.v1alpha1` | `N`, `N-1`, `N-2` in the reader and history replay preview | Dry-run replay must stay available for published fixtures during the support window |
| Memory bundle envelope | `memoryoss.bundle.v1alpha1` | `N`, `N-1`, `N-2` in reader, diff, and validate | Envelope validation must succeed for published snapshots unless integrity is actually broken |
| Sync peer sidecar signatures | current trust-fabric line | `N`, `N-1`, `N-2` verification window | Revocation, replacement identity, and signature verification remain readable even after writer upgrades |

Operational rules:
- memoryOSS only writes the current published line, never silently rewrites an older published artifact in place
- any future successor line must be announced in the conformance kit and public docs before the older line leaves the `N-2` reader window
- `bash tests/run_all.sh` now includes an explicit `compatibility and LTS` gate, and that gate exercises published `N`, `N-1`, and `N-2` fixture snapshots through normalize, reader, passport import dry-run, and history replay dry-run paths

### Universal Memory Loop Proof

The public portability claim is now backed by a reproducible proof runner:

```bash
python3 tests/run_universal_memory_loop.py
```

That runner now executes three stable end-to-end loops on local runtimes:
- store durable memories through the HTTP API
- export a passport through the CLI
- verify the exported artifact through the offline reader
- import that passport into a second runtime with dry-run merge/conflict preview
- verify portability through `query-explain`
- queue and confirm governed review items to measure operator throughput
- block risky delete requests and require confirmation for deploy requests through the proxy
- replay a history bundle into a clean namespace and compare lineage fidelity

The published report tracks hard daily-utility metrics from those loops:
- repeated-context elimination versus replaying the full portable notebook
- portability success rate
- passport merge/conflict rate
- review throughput
- blocked-bad-actions rate
- confirmation-gate success rate
- replay fidelity
- task-state quality

Claim lanes stay explicit:
- stable: repeated-context elimination, passport portability, review throughput, blocked bad actions, history replay, task-state portability proof
- experimental: retrieval tuning, confidence gating, identifier-first routing, extraction quality deltas, provider-specific token/cost evidence
- moonshot: ambient everyday utility across every client and every workday

### Memory Bundle Envelope

memoryOSS now wraps portable artifacts in `memoryoss.bundle.v1alpha1`, a stable envelope around existing passport and history payloads.

The envelope adds:
- a top-level `bundle_version`
- a portable `memoryoss://bundle/...` URI and attachment-style filename
- separate outer-envelope integrity on top of the nested passport/history integrity line
- a runtime signature from `device:local-runtime` when `auth.audit_hmac_secret` is stable
- an embedded signature chain that can later be checked against revocations or replacement identities

Reference commands:

```bash
memoryoss bundle export --kind passport --namespace test --scope project -o project.membundle.json
memoryoss bundle preview project.membundle.json
memoryoss bundle validate project.membundle.json
memoryoss bundle diff old.membundle.json new.membundle.json
memoryoss reader open project.membundle.json
memoryoss reader diff old.membundle.json new.membundle.json --format html
```

`memoryoss reader` is the offline read-only path on top of those artifacts. It can open an exported envelope or a raw published passport/history artifact, print summary/provenance/signature data, and diff two artifacts without needing a running daemon or even a valid local config file.

When a valid `--config` is available, the reader also verifies signed bundle envelopes against the local trust fabric and shows `trusted`, `revoked`, or `invalid_signature` plus any replacement identity. The admin trust endpoints provide the write path around that same fabric so passports and sync-peer descriptors can use sidecar signatures without mutating the raw artifact.

### Cross-App Adapter Bridges

memoryOSS can now normalize dominant local client artifacts into the same runtime contract instead of treating them as opaque files.

Implemented adapter paths:
- `claude_project` — import/export Markdown-style Claude Project knowledge files
- `cursor_rules` — import/export `.mdc` / rule-style Cursor memory files
- `git_history` — import recent Git commit history as candidate runtime memories

Adapter guarantees:
- foreign artifacts are normalized into runtime records instead of being stored as one raw blob
- imported records carry opaque runtime provenance plus source tags like `adapter:*`, `client:*`, and `source_ref:*`
- dry-run import previews show `create / merge / conflict` before any write happens
- one write-once-read-everywhere loop is now covered in tests: Cursor rules → memoryOSS runtime → Claude project artifact

Reference endpoints:
- `POST /v1/adapters/import`
- `GET /v1/adapters/export?kind=claude_project`

Reference commands:

```bash
memoryoss adapter import --kind cursor_rules .cursor/rules/review.mdc --namespace test --dry-run
memoryoss adapter export --kind claude_project --namespace test -o claude-project.md
memoryoss adapter import --kind git_history . --namespace test --dry-run
```

### Ambient Connector Mesh

memoryOSS now exposes an opt-in connector mesh so ambient signals can enter the same candidate and review path as extracted memories.

Supported connector kinds today:
- `editor`
- `terminal`
- `browser`
- `docs`
- `ticket`
- `calendar`
- `pull_request`
- `incident`

Connector defaults are intentionally conservative:
- disabled by default
- sensitive fragments are redacted by default
- raw excerpts are not captured unless explicitly allowed
- every candidate carries uniform provenance tags like `connector:*`, `client:ambient_mesh`, and `source_ref:*`

Reference endpoints:
- `GET /v1/connectors`
- `POST /v1/connectors/ingest`

Reference commands:

```bash
memoryoss connector list
memoryoss connector ingest --kind terminal --namespace test --summary "Terminal note: use cargo fmt before release commits." --evidence "export API_KEY=sk-live-secret" --source-ref "terminal://release/42" --dry-run
```

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
| `/v1/history/{id}` | GET | Inspect lineage, review chain, contradictions, and state transitions |
| `/v1/history/{id}/bundle` | GET | Export a deterministic time-machine replay bundle |
| `/v1/history/replay` | POST | Replay a history bundle into an empty target namespace |
| `/v1/adapters/export` | GET | Export runtime memories into a foreign client artifact |
| `/v1/adapters/import` | POST | Dry-run or apply a normalized foreign client artifact |
| `/v1/connectors` | GET | List supported ambient connector kinds plus privacy defaults |
| `/v1/connectors/ingest` | POST | Preview or store an opt-in ambient connector signal as a candidate memory |
| `/v1/bundles/export` | GET | Export a versioned memory bundle envelope around passport or history artifacts |
| `/v1/bundles/preview` | POST | Preview bundle metadata, URI, and sampled contents without importing |
| `/v1/bundles/validate` | POST | Validate bundle envelope integrity, nested artifact integrity, and current trust state |
| `/v1/bundles/diff` | POST | Diff two bundle envelopes without importing either one |
| `/v1/passport/export` | GET | Selective portable memory passport bundle export |
| `/v1/passport/import` | POST | Dry-run or apply a portable memory passport bundle |
| `/v1/runtime/contract` | GET | Versioned portable memory runtime contract |
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
- A portable `.mcpb` artifact is still follow-up packaging work; current documented install paths cover source/binary installs plus GitHub Release archives for Windows x86_64.

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
| `memoryoss status` | Show namespace health, lifecycle counts, worker state, and index health |
| `memoryoss doctor` | Diagnose config, auth, database, and index issues (non-zero on error) |
| `memoryoss recent` | Show recent injections, extractions, feedbacks, and consolidations |
| `memoryoss hud --namespace test --limit 5` | Terminal HUD for quick search/why/recent/review/import/export loops |
| `memoryoss bundle export --kind passport --namespace test --scope project -o project.membundle.json` | Export a portable memory bundle envelope and sign it with the local runtime identity |
| `memoryoss bundle preview project.membundle.json` | Preview bundle metadata, URI, and sampled contents without import |
| `memoryoss bundle validate project.membundle.json` | Validate envelope, nested artifact integrity, and trust state when config is available |
| `memoryoss bundle diff old.membundle.json new.membundle.json` | Diff two bundle envelopes offline |
| `memoryoss reader open project.membundle.json` | Open a bundle or raw published artifact in the offline universal reader, with trust verification when config is present |
| `memoryoss reader diff old.membundle.json new.membundle.json --format html` | Diff two offline artifacts in text, JSON, or HTML without a running runtime |
| `memoryoss review queue --namespace test` | List the current review inbox without raw UUIDs |
| `memoryoss review confirm --namespace test --item 1` | Confirm a queue item by inbox position |
| `memoryoss review reject --namespace test --item 2` | Reject a queue item by inbox position |
| `memoryoss review supersede --namespace test --item 1 --with-item 2` | Supersede one queue item with another by inbox position |
| `memoryoss passport export --namespace test --scope project -o passport.json` | Export a selective portable memory passport bundle |
| `memoryoss passport import passport.json --namespace test --dry-run` | Preview merge/conflict results before applying a bundle |
| `memoryoss adapter import --kind cursor_rules .cursor/rules/review.mdc --namespace test --dry-run` | Normalize a Cursor rule file into runtime records with preview |
| `memoryoss adapter export --kind claude_project --namespace test -o claude-project.md` | Export the current runtime state as a Claude Project artifact |
| `memoryoss adapter import --kind git_history . --namespace test --dry-run` | Preview recent Git history as candidate runtime memories |
| `memoryoss connector list` | Show supported ambient connector kinds and privacy defaults |
| `memoryoss connector ingest --kind docs --namespace test --summary "Release checklist lives in docs/releases/README.md." --dry-run` | Preview one ambient connector candidate before storing it |
| `memoryoss history show <id> --namespace test` | Show lineage, transitions, and review chain for one memory |
| `memoryoss history export <id> --namespace test -o history.json` | Export a deterministic history replay bundle |
| `memoryoss history replay history.json --namespace test --dry-run` | Preview a safe replay into an empty target namespace |
| `memoryoss history branch <id> --namespace test --target-namespace branch --dry-run` | Preview a branch-from-here into a new empty namespace |
| `memoryoss conformance normalize --kind passport --input passport.json --output normalized.json` | Reference reader/writer for published runtime fixtures |
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
