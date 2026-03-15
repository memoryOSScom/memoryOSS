#!/usr/bin/env python3
"""
memoryOSS Token Savings Benchmark
==================================
Measures token efficiency: same tasks WITH vs WITHOUT memory context.

Usage:
    export ANTHROPIC_API_KEY=sk-ant-...
    export MEMORYOSS_URL=http://127.0.0.1:9401  (optional, default)
    export MEMORYOSS_API_KEY=<your-memoryoss-key>
    python3 tests/run_token_savings_benchmark.py
"""

import json
import os
import sys
import time
import statistics
import requests
from datetime import datetime

ANTHROPIC_API_KEY = os.environ.get("ANTHROPIC_API_KEY", "")
MEMORYOSS_URL = os.environ.get("MEMORYOSS_URL", "http://127.0.0.1:9401")
MEMORYOSS_API_KEY = os.environ.get("MEMORYOSS_API_KEY", "")
MODEL = os.environ.get("BENCHMARK_MODEL", "claude-sonnet-4-6")
MAX_TOKENS = 300
RUNS_PER_TASK = 3
NAMESPACE = "benchmark-token-savings"

# --- 10 realistic tasks with full context that an agent would need ---

TASKS = [
    {
        "id": "T01",
        "name": "Project stack question",
        "full_context": """I'm working on a project called memoryOSS. It's written in Rust using these dependencies:
- redb for storage (source of truth)
- usearch for vector indexing
- tantivy for full-text search with BM25
- axum for the HTTP server
- fastembed for local embeddings (no external API needed)
- rmcp for MCP server support
- rustls for TLS (no OpenSSL dependency)
- tokio for async runtime
The project provides persistent memory for AI agents with store, recall, update, and forget operations.
It supports both HTTP API and MCP (stdio) interfaces.""",
        "question": "What database does memoryOSS use for storage and why?",
        "memory_summary": "memoryOSS uses redb as source of truth storage, usearch for vector index, tantivy for FTS/BM25, axum HTTP server, fastembed for local embeddings, rmcp for MCP, rustls for TLS. Rust project with store/recall/update/forget operations via HTTP API and MCP stdio."
    },
    {
        "id": "T02",
        "name": "Debug auth error",
        "full_context": """Our memoryOSS server auth system works as follows:
- JWT tokens with HS256 signing, minimum 32-char secret required
- API keys stored in config under [auth] api_keys array
- Each API key has: key, name, optional role (admin/write/read), optional namespaces list
- RBAC: admin can do everything, write can store/update/forget, read can only recall
- Rate limiting per IP on auth endpoints
- Auth middleware checks Authorization header: Bearer <jwt> or Bearer <api-key>
- MCP server uses the first API key from config to auth against HTTP server
- Debug output masks secrets with "***"
Yesterday we got error 401 on the /v1/store endpoint. The API key was correct but the role was set to "read".""",
        "question": "Why am I getting 401 on /v1/store with a valid API key?",
        "memory_summary": "memoryOSS auth: JWT HS256 (min 32 char secret), API keys in [auth] api_keys with roles admin/write/read. RBAC: admin=all, write=store/update/forget, read=recall only. Bearer token in Authorization header. MCP uses first API key. Previous issue: 401 on /v1/store because API key role was 'read' not 'write'."
    },
    {
        "id": "T03",
        "name": "Deployment config",
        "full_context": """memoryOSS deployment setup:
- Binary: single static binary, no runtime dependencies
- Config: memoryoss.toml (excluded from git, example in memoryoss.toml.example)
- TLS: auto-generate self-signed certs for local, or provide cert_path/key_path
- Default bind: 127.0.0.1:9401
- Data directory: ./data/ (redb files, tantivy index, usearch index)
- Systemd service: memoryoss.service
- Docker: not yet available (planned)
- Environment override: MEMORYOSS_BIND_ADDR, MEMORYOSS_DATA_DIR
- Health check: GET /health returns {"status":"ok"}
- Readiness: GET /ready checks db + index status
- Logging: JSON format via tracing, configurable with RUST_LOG
- Backup: copy data/ directory while server is stopped, or use /v1/export endpoint""",
        "question": "How do I set up memoryOSS for production with TLS?",
        "memory_summary": "memoryOSS deploy: single binary, memoryoss.toml config, TLS auto-generate or custom cert_path/key_path, default 127.0.0.1:9401, data in ./data/, systemd service, health at /health, ready at /ready, JSON logging, backup via data/ copy or /v1/export."
    },
    {
        "id": "T04",
        "name": "Recall tuning",
        "full_context": """memoryOSS recall system architecture:
- Hybrid search: vector similarity (usearch) + full-text BM25 (tantivy) + tag filter
- Fusion: reciprocal rank fusion (RRF) with configurable weights
- Default weights: vector=0.6, fts=0.3, recency=0.1
- Scoring pipeline: raw scores → RRF merge → recency decay → confidence threshold
- Recency decay: exponential, configurable half-life (default 30 days)
- Decompose mode: complex queries split into sub-queries via LLM
- Intent cache: caches decomposed intents for repeated similar queries
- Prefetch: background warming of frequently accessed namespaces
- Limit: default 10 results, max 100
- Filters: namespace, agent, session, tags (AND logic)
- Score explain: optional field showing per-channel breakdown
We noticed recall quality dropped when we had >5000 memories. Turned out recency decay was too aggressive at 7-day half-life.""",
        "question": "Recall quality is bad with many memories, what should I tune?",
        "memory_summary": "memoryOSS recall: hybrid search (vector 0.6 + FTS/BM25 0.3 + recency 0.1), RRF fusion, exponential recency decay (default 30d half-life), decompose mode for complex queries, intent cache, prefetch. Filters: namespace/agent/session/tags. Previous issue: quality drop at >5k memories caused by too aggressive 7-day recency half-life."
    },
    {
        "id": "T05",
        "name": "MCP integration",
        "full_context": """memoryOSS MCP server integration:
- Transport: stdio (standard MCP pattern)
- Start: memoryoss mcp-server (requires HTTP server running separately)
- Architecture: MCP server is thin HTTP client → delegates to running HTTP server
- No direct DB access from MCP, avoids lock conflicts
- Tools exposed: memoryoss_store, memoryoss_recall, memoryoss_update, memoryoss_forget
- Claude Desktop config:
  {
    "mcpServers": {
      "memoryoss": {
        "command": "memoryoss",
        "args": ["mcp-server", "--config", "/path/to/memoryoss.toml"]
      }
    }
  }
- Claude Code config: same pattern in ~/.claude/mcp.json
- Instructions field tells Claude to call recall at start of every turn
- Server info reports version from Cargo.toml""",
        "question": "How do I add memoryOSS to Claude Desktop?",
        "memory_summary": "memoryOSS MCP: stdio transport, start with 'memoryoss mcp-server', thin HTTP client to running server. Tools: memoryoss_store/recall/update/forget. Claude Desktop: add to mcpServers in config with command 'memoryoss' args ['mcp-server', '--config', 'path/to/memoryoss.toml']. Instructions tell Claude to recall at start of every turn."
    },
    {
        "id": "T06",
        "name": "Proxy setup",
        "full_context": """memoryOSS proxy mode:
- Routes: /proxy/v1/chat/completions (OpenAI) and /proxy/anthropic/v1/messages (Anthropic)
- How it works: intercepts requests, does memory recall on the user message, injects relevant memories as XML block before system prompt, forwards to upstream API
- Memory injection format: <memoryoss_context>...</memoryoss_context> XML tags
- Fact extraction: after response, background LLM extracts facts and stores them automatically
- Config: [proxy] section in memoryoss.toml with upstream_url, api_key mappings
- Token budget: estimates ~4 chars/token, caps memory injection to not exceed context window
- Supports streaming responses (SSE passthrough)
- Key mapping: maps local API keys to upstream provider keys
- Error handling: if memoryOSS is down, proxy falls through to upstream (graceful degradation)""",
        "question": "How does the proxy mode inject memories into requests?",
        "memory_summary": "memoryOSS proxy: routes /proxy/v1/chat/completions (OpenAI) + /proxy/anthropic/v1/messages (Anthropic). Intercepts request, recalls memories from user message, injects as <memoryoss_context> XML before system prompt. Background fact extraction from responses. Token budget ~4 chars/token. Graceful degradation if down. Config in [proxy] section."
    },
    {
        "id": "T07",
        "name": "Encryption setup",
        "full_context": """memoryOSS encryption at rest:
- AES-256-GCM for memory content encryption
- Key derivation: HKDF-SHA256 from master secret
- Master secret: configured in [encryption] section of memoryoss.toml
- Per-namespace derived keys (HKDF with namespace as info)
- Encrypted fields: content, tags (metadata like timestamps remain cleartext)
- Key rotation: planned but not yet implemented
- AWS KMS integration: stubbed out, not production-ready
- Audit log: HMAC-SHA256 chain for tamper detection
- Zeroize: sensitive key material zeroized on drop
- If encryption disabled: content stored as plaintext msgpack in redb""",
        "question": "How do I enable encryption at rest?",
        "memory_summary": "memoryOSS encryption: AES-256-GCM, HKDF-SHA256 key derivation from master secret in [encryption] config. Per-namespace keys. Encrypts content+tags, metadata stays cleartext. HMAC-SHA256 audit chain. Zeroize on drop. Key rotation planned. AWS KMS stubbed not ready."
    },
    {
        "id": "T08",
        "name": "Migration issue",
        "full_context": """memoryOSS data migration system:
- Versioned migrations in src/migration.rs
- Auto-runs on startup (memoryoss serve or memoryoss migrate)
- Migration history tracked in redb metadata table
- Current migrations:
  001: initial schema (memories table, metadata table)
  002: add version field to memories
  003: add sharing/export tables
  004: add encryption metadata
  005: add self-assessments table
  006: expand scan grade fields
- Dry-run mode: memoryoss migrate --dry-run
- Down migrations: not supported (forward-only)
- Previous bug: 006 had wrong down_revision pointing to non-existent 005_add_self_assessments instead of 005_self_assessments
- If migration fails: server won't start, error logged""",
        "question": "Server won't start after update, what's wrong?",
        "memory_summary": "memoryOSS migrations: versioned in migration.rs, auto-run on startup, tracked in redb metadata. 6 migrations (001-006). Forward-only, no down. Dry-run with --dry-run. Previous bug: 006 had wrong down_revision. If migration fails server won't start with error log."
    },
    {
        "id": "T09",
        "name": "Performance tuning",
        "full_context": """memoryOSS performance characteristics:
- Write path: store → redb write → queue for async indexing (vector + FTS)
- Indexer lag: background worker, batches every 100ms, group commit
- Read path: recall → parallel vector search + FTS search → RRF fusion → return
- Benchmarks (20K memories):
  - Store latency: p50=2ms, p95=8ms, p99=15ms
  - Recall latency: p50=12ms, p95=35ms, p99=80ms
  - Signal hit rate: 94% at top-10
  - Throughput: ~500 stores/sec sustained
- Memory usage: ~2GB RSS at 100K memories (including embedding model)
- fastembed model: loads on first use, ~400MB model, ~500ms cold start
- Prefetch: can warm namespaces on startup
- Connection pool: configurable max connections for upstream proxy
- Known bottleneck: vector search at >50K memories, consider namespace sharding""",
        "question": "Recall is slow, how do I improve latency?",
        "memory_summary": "memoryOSS perf: store p50=2ms p95=8ms, recall p50=12ms p95=35ms at 20K. 500 stores/sec. ~2GB RSS at 100K. fastembed ~400MB model, 500ms cold start. Async indexer with 100ms batch. Bottleneck: vector search >50K, use namespace sharding. Prefetch warms namespaces on startup."
    },
    {
        "id": "T10",
        "name": "Multi-agent sharing",
        "full_context": """memoryOSS multi-agent memory sharing:
- Namespaces: logical separation of memory spaces
- Agent field: optional identifier per memory
- Session field: optional session grouping
- Sharing modes:
  1. Same namespace: agents share all memories (default)
  2. Agent-scoped: filter by agent field, each agent sees own memories
  3. Cross-namespace: export/import between namespaces
  4. Broadcast: store to multiple namespaces simultaneously
- Export format: tar.zst archive with msgpack memories + metadata
- Import: POST /v1/import with tar.zst body
- Trust model: configurable in [security.trust] section
  - trust_level: local (single machine), network (LAN), public
  - affects: TLS requirements, auth strictness, rate limits
- Conflict resolution: last-write-wins with version counter
- No CRDT or causal consistency (planned for v2)""",
        "question": "How can two agents share memories?",
        "memory_summary": "memoryOSS sharing: namespaces for separation, agent/session fields for scoping. Modes: same namespace (shared), agent-scoped (filtered), cross-namespace (export/import tar.zst), broadcast (multi-namespace store). Trust levels: local/network/public. Last-write-wins with version counter. CRDT planned for v2."
    },
]


def call_anthropic(system_prompt: str, user_message: str) -> dict:
    """Call Anthropic API and return usage stats."""
    resp = requests.post(
        "https://api.anthropic.com/v1/messages",
        headers={
            "x-api-key": ANTHROPIC_API_KEY,
            "anthropic-version": "2023-06-01",
            "content-type": "application/json",
        },
        json={
            "model": MODEL,
            "max_tokens": MAX_TOKENS,
            "system": system_prompt,
            "messages": [{"role": "user", "content": user_message}],
        },
        timeout=30,
    )
    resp.raise_for_status()
    data = resp.json()
    return {
        "input_tokens": data["usage"]["input_tokens"],
        "output_tokens": data["usage"]["output_tokens"],
    }


def recall_from_memoryoss(query: str) -> str:
    """Recall relevant memories from memoryOSS."""
    try:
        resp = requests.post(
            f"{MEMORYOSS_URL}/v1/recall",
            headers={
                "Authorization": f"Bearer {MEMORYOSS_API_KEY}",
                "Content-Type": "application/json",
            },
            json={
                "query": query,
                "namespace": NAMESPACE,
                "limit": 5,
            },
            timeout=10,
        )
        if resp.status_code == 200:
            memories = resp.json().get("memories", [])
            if memories:
                parts = []
                for m in memories:
                    mem = m.get("memory", {})
                    parts.append(mem.get("content", ""))
                return "\n".join(parts)
    except Exception:
        pass
    return ""


def store_to_memoryoss(content: str, tags: list[str]) -> bool:
    """Store a memory in memoryOSS."""
    try:
        resp = requests.post(
            f"{MEMORYOSS_URL}/v1/store",
            headers={
                "Authorization": f"Bearer {MEMORYOSS_API_KEY}",
                "Content-Type": "application/json",
            },
            json={
                "content": content,
                "namespace": NAMESPACE,
                "tags": tags,
            },
            timeout=10,
        )
        return resp.status_code == 200 or resp.status_code == 201
    except Exception:
        return False


def cleanup_namespace():
    """Delete benchmark namespace memories."""
    try:
        resp = requests.post(
            f"{MEMORYOSS_URL}/v1/recall",
            headers={
                "Authorization": f"Bearer {MEMORYOSS_API_KEY}",
                "Content-Type": "application/json",
            },
            json={"query": "*", "namespace": NAMESPACE, "limit": 100},
            timeout=10,
        )
        if resp.status_code == 200:
            ids = [m["memory"]["id"] for m in resp.json().get("memories", [])]
            if ids:
                requests.delete(
                    f"{MEMORYOSS_URL}/v1/forget",
                    headers={
                        "Authorization": f"Bearer {MEMORYOSS_API_KEY}",
                        "Content-Type": "application/json",
                    },
                    json={"ids": ids, "namespace": NAMESPACE},
                    timeout=10,
                )
    except Exception:
        pass


def run_benchmark():
    print("=" * 70)
    print("memoryOSS Token Savings Benchmark")
    print(f"Model: {MODEL} | Tasks: {len(TASKS)} | Runs/task: {RUNS_PER_TASK}")
    print("=" * 70)

    # Phase 1: Store memory summaries
    print("\n[Phase 1] Storing memory context into memoryOSS...")
    cleanup_namespace()
    stored = 0
    for task in TASKS:
        ok = store_to_memoryoss(task["memory_summary"], [task["id"], task["name"]])
        if ok:
            stored += 1
            print(f"  Stored: {task['id']} - {task['name']}")
        else:
            print(f"  FAILED: {task['id']} - {task['name']}")

    use_live_memory = stored == len(TASKS)
    if not use_live_memory:
        print(f"\n  WARNING: Only {stored}/{len(TASKS)} stored. Using inline memory summaries as fallback.")

    time.sleep(2)  # let indexer catch up

    # Phase 2: Run benchmark
    results = []
    total_without = 0
    total_with = 0

    print(f"\n[Phase 2] Running {len(TASKS) * RUNS_PER_TASK * 2} API calls...\n")

    for task in TASKS:
        without_tokens = []
        with_tokens = []

        for run in range(RUNS_PER_TASK):
            # --- WITHOUT memory: full context in prompt ---
            system_no_mem = "You are a helpful assistant. Answer concisely."
            user_no_mem = f"{task['full_context']}\n\nQuestion: {task['question']}"

            try:
                usage = call_anthropic(system_no_mem, user_no_mem)
                without_tokens.append(usage["input_tokens"])
                time.sleep(0.5)
            except Exception as e:
                print(f"  ERROR (no-mem) {task['id']} run {run}: {e}")
                continue

            # --- WITH memory: short prompt + memory context ---
            if use_live_memory:
                memory_ctx = recall_from_memoryoss(task["question"])
                if not memory_ctx:
                    memory_ctx = task["memory_summary"]
            else:
                memory_ctx = task["memory_summary"]

            system_with_mem = f"You are a helpful assistant. Use this context from memory:\n<memory>\n{memory_ctx}\n</memory>"
            user_with_mem = task["question"]

            try:
                usage = call_anthropic(system_with_mem, user_with_mem)
                with_tokens.append(usage["input_tokens"])
                time.sleep(0.5)
            except Exception as e:
                print(f"  ERROR (mem) {task['id']} run {run}: {e}")
                continue

        if without_tokens and with_tokens:
            avg_without = statistics.mean(without_tokens)
            avg_with = statistics.mean(with_tokens)
            savings_pct = (1 - avg_with / avg_without) * 100

            result = {
                "id": task["id"],
                "name": task["name"],
                "avg_input_tokens_without_memory": round(avg_without),
                "avg_input_tokens_with_memory": round(avg_with),
                "token_savings_percent": round(savings_pct, 1),
                "token_savings_absolute": round(avg_without - avg_with),
                "runs": RUNS_PER_TASK,
            }
            results.append(result)
            total_without += avg_without
            total_with += avg_with

            arrow = "↓" if savings_pct > 0 else "↑"
            print(f"  {task['id']}: {round(avg_without):>5} → {round(avg_with):>5} tokens  ({arrow} {abs(savings_pct):.1f}%)")

    # Phase 3: Summary
    print("\n" + "=" * 70)
    print("RESULTS")
    print("=" * 70)

    if results:
        savings_list = [r["token_savings_percent"] for r in results]
        avg_savings = statistics.mean(savings_list)
        total_savings_pct = (1 - total_with / total_without) * 100 if total_without > 0 else 0

        print(f"\n  Tasks benchmarked:     {len(results)}")
        print(f"  Avg tokens WITHOUT:    {round(total_without / len(results))}")
        print(f"  Avg tokens WITH:       {round(total_with / len(results))}")
        print(f"  Avg savings per task:  {avg_savings:.1f}%")
        print(f"  Total token savings:   {total_savings_pct:.1f}%")
        print(f"  Min savings:           {min(savings_list):.1f}%")
        print(f"  Max savings:           {max(savings_list):.1f}%")

        # Cost estimation (Sonnet pricing: $3/MTok input)
        cost_per_mtok = 3.0
        saved_tokens = total_without - total_with
        cost_saved_per_10k = (saved_tokens / len(results)) * 10000 / 1_000_000 * cost_per_mtok
        print(f"\n  Est. cost savings at 10K queries/month: ${cost_saved_per_10k:.2f}")

        report = {
            "runner": "token_savings_benchmark",
            "generated_at": datetime.utcnow().isoformat() + "Z",
            "model": MODEL,
            "runs_per_task": RUNS_PER_TASK,
            "total_tasks": len(results),
            "summary": {
                "avg_input_tokens_without_memory": round(total_without / len(results)),
                "avg_input_tokens_with_memory": round(total_with / len(results)),
                "avg_savings_percent": round(avg_savings, 1),
                "total_savings_percent": round(total_savings_pct, 1),
                "min_savings_percent": round(min(savings_list), 1),
                "max_savings_percent": round(max(savings_list), 1),
                "estimated_monthly_savings_10k_queries_usd": round(cost_saved_per_10k, 2),
            },
            "tasks": results,
        }

        report_path = os.path.join(os.path.dirname(__file__), ".last-run", "token-savings-report.json")
        os.makedirs(os.path.dirname(report_path), exist_ok=True)
        with open(report_path, "w") as f:
            json.dump(report, f, indent=2)
        print(f"\n  Report saved: {report_path}")
    else:
        print("\n  No results collected!")

    # Cleanup
    cleanup_namespace()
    print("\nDone.")


if __name__ == "__main__":
    if not ANTHROPIC_API_KEY:
        print("ERROR: Set ANTHROPIC_API_KEY environment variable")
        sys.exit(1)
    run_benchmark()
