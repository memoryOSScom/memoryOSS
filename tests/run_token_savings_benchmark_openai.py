#!/usr/bin/env python3
"""
memoryOSS Token Savings Benchmark — OpenAI/GPT variant
"""

import json
import os
import sys
import time
import statistics
import requests
from datetime import datetime, timezone

OPENAI_API_KEY = os.environ.get("OPENAI_API_KEY", "")
MODEL = os.environ.get("BENCHMARK_MODEL", "gpt-4o")
MAX_TOKENS = 300
RUNS_PER_TASK = 3

# Same 10 tasks as Anthropic benchmark
TASKS = [
    {"id": "T01", "name": "Project stack question",
     "full_context": "I'm working on a project called memoryOSS. It's written in Rust using these dependencies:\n- redb for storage (source of truth)\n- usearch for vector indexing\n- tantivy for full-text search with BM25\n- axum for the HTTP server\n- fastembed for local embeddings (no external API needed)\n- rmcp for MCP server support\n- rustls for TLS (no OpenSSL dependency)\n- tokio for async runtime\nThe project provides persistent memory for AI agents with store, recall, update, and forget operations.\nIt supports both HTTP API and MCP (stdio) interfaces.",
     "question": "What database does memoryOSS use for storage and why?",
     "memory_summary": "memoryOSS uses redb as source of truth storage, usearch for vector index, tantivy for FTS/BM25, axum HTTP server, fastembed for local embeddings, rmcp for MCP, rustls for TLS. Rust project with store/recall/update/forget operations via HTTP API and MCP stdio."},
    {"id": "T02", "name": "Debug auth error",
     "full_context": "Our memoryOSS server auth system works as follows:\n- JWT tokens with HS256 signing, minimum 32-char secret required\n- API keys stored in config under [auth] api_keys array\n- Each API key has: key, name, optional role (admin/write/read), optional namespaces list\n- RBAC: admin can do everything, write can store/update/forget, read can only recall\n- Rate limiting per IP on auth endpoints\n- Auth middleware checks Authorization header: Bearer <jwt> or Bearer <api-key>\n- MCP server uses the first API key from config to auth against HTTP server\n- Debug output masks secrets with \"***\"\nYesterday we got error 401 on the /v1/store endpoint. The API key was correct but the role was set to \"read\".",
     "question": "Why am I getting 401 on /v1/store with a valid API key?",
     "memory_summary": "memoryOSS auth: JWT HS256 (min 32 char secret), API keys in [auth] api_keys with roles admin/write/read. RBAC: admin=all, write=store/update/forget, read=recall only. Bearer token in Authorization header. MCP uses first API key. Previous issue: 401 on /v1/store because API key role was 'read' not 'write'."},
    {"id": "T03", "name": "Deployment config",
     "full_context": "memoryOSS deployment setup:\n- Binary: single static binary, no runtime dependencies\n- Config: memoryoss.toml (excluded from git, example in memoryoss.toml.example)\n- TLS: auto-generate self-signed certs for local, or provide cert_path/key_path\n- Default bind: 127.0.0.1:9401\n- Data directory: ./data/ (redb files, tantivy index, usearch index)\n- Systemd service: memoryoss.service\n- Docker: not yet available (planned)\n- Environment override: MEMORYOSS_BIND_ADDR, MEMORYOSS_DATA_DIR\n- Health check: GET /health returns {\"status\":\"ok\"}\n- Readiness: GET /ready checks db + index status\n- Logging: JSON format via tracing, configurable with RUST_LOG\n- Backup: copy data/ directory while server is stopped, or use /v1/export endpoint",
     "question": "How do I set up memoryOSS for production with TLS?",
     "memory_summary": "memoryOSS deploy: single binary, memoryoss.toml config, TLS auto-generate or custom cert_path/key_path, default 127.0.0.1:9401, data in ./data/, systemd service, health at /health, ready at /ready, JSON logging, backup via data/ copy or /v1/export."},
    {"id": "T04", "name": "Recall tuning",
     "full_context": "memoryOSS recall system architecture:\n- Hybrid search: vector similarity (usearch) + full-text BM25 (tantivy) + tag filter\n- Fusion: reciprocal rank fusion (RRF) with configurable weights\n- Default weights: vector=0.6, fts=0.3, recency=0.1\n- Scoring pipeline: raw scores -> RRF merge -> recency decay -> confidence threshold\n- Recency decay: exponential, configurable half-life (default 30 days)\n- Decompose mode: complex queries split into sub-queries via LLM\n- Intent cache: caches decomposed intents for repeated similar queries\n- Prefetch: background warming of frequently accessed namespaces\n- Limit: default 10 results, max 100\n- Filters: namespace, agent, session, tags (AND logic)\n- Score explain: optional field showing per-channel breakdown\nWe noticed recall quality dropped when we had >5000 memories. Turned out recency decay was too aggressive at 7-day half-life.",
     "question": "Recall quality is bad with many memories, what should I tune?",
     "memory_summary": "memoryOSS recall: hybrid search (vector 0.6 + FTS/BM25 0.3 + recency 0.1), RRF fusion, exponential recency decay (default 30d half-life), decompose mode for complex queries, intent cache, prefetch. Filters: namespace/agent/session/tags. Previous issue: quality drop at >5k memories caused by too aggressive 7-day recency half-life."},
    {"id": "T05", "name": "MCP integration",
     "full_context": "memoryOSS MCP server integration:\n- Transport: stdio (standard MCP pattern)\n- Start: memoryoss mcp-server (requires HTTP server running separately)\n- Architecture: MCP server is thin HTTP client -> delegates to running HTTP server\n- No direct DB access from MCP, avoids lock conflicts\n- Tools exposed: memoryoss_store, memoryoss_recall, memoryoss_update, memoryoss_forget\n- Claude Desktop config:\n  {\"mcpServers\": {\"memoryoss\": {\"command\": \"memoryoss\", \"args\": [\"mcp-server\", \"--config\", \"/path/to/memoryoss.toml\"]}}}\n- Claude Code config: same pattern in ~/.claude/mcp.json\n- Instructions field tells Claude to call recall at start of every turn\n- Server info reports version from Cargo.toml",
     "question": "How do I add memoryOSS to Claude Desktop?",
     "memory_summary": "memoryOSS MCP: stdio transport, start with 'memoryoss mcp-server', thin HTTP client to running server. Tools: memoryoss_store/recall/update/forget. Claude Desktop: add to mcpServers in config with command 'memoryoss' args ['mcp-server', '--config', 'path/to/memoryoss.toml']. Instructions tell Claude to recall at start of every turn."},
    {"id": "T06", "name": "Proxy setup",
     "full_context": "memoryOSS proxy mode:\n- Routes: /proxy/v1/chat/completions (OpenAI) and /proxy/anthropic/v1/messages (Anthropic)\n- How it works: intercepts requests, does memory recall on the user message, injects relevant memories as XML block before system prompt, forwards to upstream API\n- Memory injection format: <memoryoss_context>...</memoryoss_context> XML tags\n- Fact extraction: after response, background LLM extracts facts and stores them automatically\n- Config: [proxy] section in memoryoss.toml with upstream_url, api_key mappings\n- Token budget: estimates ~4 chars/token, caps memory injection to not exceed context window\n- Supports streaming responses (SSE passthrough)\n- Key mapping: maps local API keys to upstream provider keys\n- Error handling: if memoryOSS is down, proxy falls through to upstream (graceful degradation)",
     "question": "How does the proxy mode inject memories into requests?",
     "memory_summary": "memoryOSS proxy: routes /proxy/v1/chat/completions (OpenAI) + /proxy/anthropic/v1/messages (Anthropic). Intercepts request, recalls memories from user message, injects as <memoryoss_context> XML before system prompt. Background fact extraction from responses. Token budget ~4 chars/token. Graceful degradation if down. Config in [proxy] section."},
    {"id": "T07", "name": "Encryption setup",
     "full_context": "memoryOSS encryption at rest:\n- AES-256-GCM for memory content encryption\n- Key derivation: HKDF-SHA256 from master secret\n- Master secret: configured in [encryption] section of memoryoss.toml\n- Per-namespace derived keys (HKDF with namespace as info)\n- Encrypted fields: content, tags (metadata like timestamps remain cleartext)\n- Key rotation: planned but not yet implemented\n- AWS KMS integration: stubbed out, not production-ready\n- Audit log: HMAC-SHA256 chain for tamper detection\n- Zeroize: sensitive key material zeroized on drop\n- If encryption disabled: content stored as plaintext msgpack in redb",
     "question": "How do I enable encryption at rest?",
     "memory_summary": "memoryOSS encryption: AES-256-GCM, HKDF-SHA256 key derivation from master secret in [encryption] config. Per-namespace keys. Encrypts content+tags, metadata stays cleartext. HMAC-SHA256 audit chain. Zeroize on drop. Key rotation planned. AWS KMS stubbed not ready."},
    {"id": "T08", "name": "Migration issue",
     "full_context": "memoryOSS data migration system:\n- Versioned migrations in src/migration.rs\n- Auto-runs on startup (memoryoss serve or memoryoss migrate)\n- Migration history tracked in redb metadata table\n- Current migrations:\n  001: initial schema (memories table, metadata table)\n  002: add version field to memories\n  003: add sharing/export tables\n  004: add encryption metadata\n  005: add self-assessments table\n  006: expand scan grade fields\n- Dry-run mode: memoryoss migrate --dry-run\n- Down migrations: not supported (forward-only)\n- Previous bug: 006 had wrong down_revision pointing to non-existent 005_add_self_assessments instead of 005_self_assessments\n- If migration fails: server won't start, error logged",
     "question": "Server won't start after update, what's wrong?",
     "memory_summary": "memoryOSS migrations: versioned in migration.rs, auto-run on startup, tracked in redb metadata. 6 migrations (001-006). Forward-only, no down. Dry-run with --dry-run. Previous bug: 006 had wrong down_revision. If migration fails server won't start with error log."},
    {"id": "T09", "name": "Performance tuning",
     "full_context": "memoryOSS performance characteristics:\n- Write path: store -> redb write -> queue for async indexing (vector + FTS)\n- Indexer lag: background worker, batches every 100ms, group commit\n- Read path: recall -> parallel vector search + FTS search -> RRF fusion -> return\n- Benchmarks (20K memories):\n  - Store latency: p50=2ms, p95=8ms, p99=15ms\n  - Recall latency: p50=12ms, p95=35ms, p99=80ms\n  - Signal hit rate: 94% at top-10\n  - Throughput: ~500 stores/sec sustained\n- Memory usage: ~2GB RSS at 100K memories (including embedding model)\n- fastembed model: loads on first use, ~400MB model, ~500ms cold start\n- Prefetch: can warm namespaces on startup\n- Connection pool: configurable max connections for upstream proxy\n- Known bottleneck: vector search at >50K memories, consider namespace sharding",
     "question": "Recall is slow, how do I improve latency?",
     "memory_summary": "memoryOSS perf: store p50=2ms p95=8ms, recall p50=12ms p95=35ms at 20K. 500 stores/sec. ~2GB RSS at 100K. fastembed ~400MB model, 500ms cold start. Async indexer with 100ms batch. Bottleneck: vector search >50K, use namespace sharding. Prefetch warms namespaces on startup."},
    {"id": "T10", "name": "Multi-agent sharing",
     "full_context": "memoryOSS multi-agent memory sharing:\n- Namespaces: logical separation of memory spaces\n- Agent field: optional identifier per memory\n- Session field: optional session grouping\n- Sharing modes:\n  1. Same namespace: agents share all memories (default)\n  2. Agent-scoped: filter by agent field, each agent sees own memories\n  3. Cross-namespace: export/import between namespaces\n  4. Broadcast: store to multiple namespaces simultaneously\n- Export format: tar.zst archive with msgpack memories + metadata\n- Import: POST /v1/import with tar.zst body\n- Trust model: configurable in [security.trust] section\n  - trust_level: local (single machine), network (LAN), public\n  - affects: TLS requirements, auth strictness, rate limits\n- Conflict resolution: last-write-wins with version counter\n- No CRDT or causal consistency (planned for v2)",
     "question": "How can two agents share memories?",
     "memory_summary": "memoryOSS sharing: namespaces for separation, agent/session fields for scoping. Modes: same namespace (shared), agent-scoped (filtered), cross-namespace (export/import tar.zst), broadcast (multi-namespace store). Trust levels: local/network/public. Last-write-wins with version counter. CRDT planned for v2."},
]


def call_openai(system_prompt: str, user_message: str) -> dict:
    resp = requests.post(
        "https://api.openai.com/v1/chat/completions",
        headers={
            "Authorization": f"Bearer {OPENAI_API_KEY}",
            "Content-Type": "application/json",
        },
        json={
            "model": MODEL,
            "max_tokens": MAX_TOKENS,
            "messages": [
                {"role": "system", "content": system_prompt},
                {"role": "user", "content": user_message},
            ],
        },
        timeout=60,
    )
    resp.raise_for_status()
    data = resp.json()
    usage = data["usage"]
    return {
        "input_tokens": usage.get("prompt_tokens", 0),
        "output_tokens": usage.get("completion_tokens", 0),
    }


def run_benchmark():
    print("=" * 70)
    print(f"memoryOSS Token Savings Benchmark (OpenAI)")
    print(f"Model: {MODEL} | Tasks: {len(TASKS)} | Runs/task: {RUNS_PER_TASK}")
    print("=" * 70)

    results = []
    total_without = 0
    total_with = 0

    print(f"\nRunning {len(TASKS) * RUNS_PER_TASK * 2} API calls...\n")

    for task in TASKS:
        without_tokens = []
        with_tokens = []

        for run in range(RUNS_PER_TASK):
            # WITHOUT memory
            system_no_mem = "You are a helpful assistant. Answer concisely."
            user_no_mem = f"{task['full_context']}\n\nQuestion: {task['question']}"
            try:
                usage = call_openai(system_no_mem, user_no_mem)
                without_tokens.append(usage["input_tokens"])
                time.sleep(0.5)
            except Exception as e:
                print(f"  ERROR (no-mem) {task['id']} run {run}: {e}")
                continue

            # WITH memory
            memory_ctx = task["memory_summary"]
            system_with_mem = f"You are a helpful assistant. Use this context from memory:\n<memory>\n{memory_ctx}\n</memory>"
            user_with_mem = task["question"]
            try:
                usage = call_openai(system_with_mem, user_with_mem)
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

    # Summary
    print("\n" + "=" * 70)
    print("RESULTS")
    print("=" * 70)

    if results:
        savings_list = [r["token_savings_percent"] for r in results]
        avg_savings = statistics.mean(savings_list)
        total_savings_pct = (1 - total_with / total_without) * 100 if total_without > 0 else 0

        print(f"\n  Model:                 {MODEL}")
        print(f"  Tasks benchmarked:     {len(results)}")
        print(f"  Avg tokens WITHOUT:    {round(total_without / len(results))}")
        print(f"  Avg tokens WITH:       {round(total_with / len(results))}")
        print(f"  Avg savings per task:  {avg_savings:.1f}%")
        print(f"  Total token savings:   {total_savings_pct:.1f}%")
        print(f"  Min savings:           {min(savings_list):.1f}%")
        print(f"  Max savings:           {max(savings_list):.1f}%")

        report = {
            "runner": "token_savings_benchmark_openai",
            "generated_at": datetime.now(timezone.utc).isoformat(),
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
            },
            "tasks": results,
        }

        report_path = os.path.join(os.path.dirname(__file__), ".last-run", f"token-savings-report-{MODEL}.json")
        os.makedirs(os.path.dirname(report_path), exist_ok=True)
        with open(report_path, "w") as f:
            json.dump(report, f, indent=2)
        print(f"\n  Report saved: {report_path}")

    print("\nDone.")


if __name__ == "__main__":
    if not OPENAI_API_KEY:
        print("ERROR: Set OPENAI_API_KEY environment variable")
        sys.exit(1)
    run_benchmark()
