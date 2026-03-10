# Test Report

- Runner: `tests/run_all.sh`
- Generated at: `2026-03-09T20:53:51.468483+00:00`
- Duration: `151s`
- Total passed checks: `101`

## Build Gates (4)

- [PASS] cargo fmt --check — 1s
- [PASS] cargo clippy -- -D warnings — 19s
- [PASS] cargo test — 24s
- [PASS] cargo build — 1s

## Rust Unit Tests (26)

- [PASS] Structural duplicate detection handles containment
- [PASS] Content fusion preserves unique sentences
- [PASS] Fusion explain collapse merges structural duplicates
- [PASS] Fusion recall collapse merges structural duplicates
- [PASS] Intent cache canonicalization handles stopword-only input
- [PASS] Intent cache keeps meaningful query terms
- [PASS] Intent cache canonicalization normalizes basic queries
- [PASS] Intent cache sorts and deduplicates tokens
- [PASS] Intent cache invalidation on writes
- [PASS] Intent cache hit/miss behavior
- [PASS] Intent cache session isolation
- [PASS] Candidate memories promote from repeated signal
- [PASS] Intent cache strips punctuation
- [PASS] Prefetch query recording deduplicates repeated prompts
- [PASS] Superseded stale memories do not revive automatically
- [PASS] Prefetch ring buffer evicts oldest entries
- [PASS] Prefetch session tracking avoids duplicate warmups
- [PASS] server::proxy::tests::extraction_ready_accepts_request_scoped_override
- [PASS] server::proxy::tests::extraction_ready_accepts_anthropic_key_for_claude_provider
- [PASS] server::proxy::tests::extraction_ready_requires_openai_credentials
- [PASS] server::proxy::tests::extraction_request_falls_back_to_claude_for_claude_oauth
- [PASS] server::proxy::tests::extraction_uses_mapping_key_as_openai_fallback
- [PASS] server::proxy::tests::passthrough_local_only_allows_loopback_direct
- [PASS] server::proxy::tests::passthrough_local_only_rejects_remote_forwarded_client
- [PASS] tests::prefers_current_binary_when_not_in_target_tree
- [PASS] tests::prefers_stable_candidate_over_target_build_output

## Core API & Lifecycle (6)

- [PASS] Unauthorized requests are rejected cleanly
- [PASS] Feedback transitions memory lifecycle states
- [PASS] Concurrent stores and recalls stay stable
- [PASS] Lifecycle admin view filters and summarizes status
- [PASS] Admin query explain exposes real score breakdown
- [PASS] Store -> Recall -> Update -> Forget roundtrip

## Connection Paths (6)

- [PASS] GDPR export, access, and certified forget roundtrip
- [PASS] Decay and migrate CLI commands work against real data
- [PASS] Key rotation paths cover rotate, list, revoke, readability
- [PASS] Proxy transport paths cover OpenAI and Anthropic connections
- [PASS] Proxy handles upstream failure without panicking
- [PASS] Sharing paths cover owner, grantee, grant, revoke, accessible

## MCP (2)

- [PASS] MCP unknown tool returns JSON-RPC error
- [PASS] MCP stdio roundtrip: initialize, tools/list, store, recall, update, forget

## Other Integration Paths (7)

- [PASS] test_batch_store_handles_zero_knowledge_and_source_provenance
- [PASS] test_hybrid_gateway_fallback_covers_four_auth_paths_and_reports_core_unavailable
- [PASS] test_hybrid_serve_manages_core_and_exposes_gateway_health
- [PASS] test_hybrid_gateway_proxies_memory_api_to_running_core
- [PASS] test_proxy_anthropic_oauth_passthrough_uses_bearer_and_preserves_headers
- [PASS] test_server_can_run_plain_http_without_dev_mode
- [PASS] test_proxy_passthrough_is_local_only_by_default

## Wizard Smoke Test (3)

- [PASS] Setup wizard writes a config file — 2s
- [PASS] Setup wizard persists `default_memory_mode = "readonly"` — 2s
- [PASS] Setup wizard reaches the ready banner and serves `/health` — 2s

## TypeScript SDK Tests (5)

- [PASS] constructs with defaults
- [PASS] constructs with options
- [PASS] throws MemoryOSSError on auth without key
- [PASS] connect returns a client without apiKey
- [PASS] formats message with status

## Dependency Audit (1)

- [PASS] cargo audit (offline if available) — 2s

## Wizard Scenario Matrix (9)

- [PASS] No tools at all — claude=False, codex=False, openai_key=False, anthropic_key=False, assertions=11
- [PASS] Claude Code only — claude=True, codex=False, openai_key=False, anthropic_key=False, assertions=11
- [PASS] Codex CLI only — claude=False, codex=True, openai_key=False, anthropic_key=False, assertions=11
- [PASS] Both without keys — claude=True, codex=True, openai_key=False, anthropic_key=False, assertions=11
- [PASS] Claude + OpenAI key — claude=True, codex=False, openai_key=True, anthropic_key=False, assertions=11
- [PASS] Claude + Anthropic key — claude=True, codex=False, openai_key=False, anthropic_key=True, assertions=11
- [PASS] Both + OpenAI key — claude=True, codex=True, openai_key=True, anthropic_key=False, assertions=11
- [PASS] Codex + OpenAI key — claude=False, codex=True, openai_key=True, anthropic_key=False, assertions=11
- [PASS] Idempotency double run — claude=True, codex=False, openai_key=False, anthropic_key=False, assertions=14

## 20k Scaling Benchmark (6)

- [PASS] Stored 1,000 memories in batches of 200 — 120.60 memories/sec with client-provided embeddings
- [PASS] Indexer backpressure handling — lag waits=0, retry waits=0
- [PASS] Synthetic dedup guard — semantic_dedup_threshold=0.9999 for the synthetic scale corpus
- [PASS] Signal hit rate at current threshold — 100.0%
- [PASS] Noise rejection at current threshold — 100.0%
- [PASS] Recall latency p50 / p95 / p99 — 63.92ms / 82.08ms / 93.74ms

## Scoring Calibration (6)

- [PASS] Calibration corpus — 1,000 queries (17 exact, 21 related, 962 noise)
- [PASS] Exact score distribution — min 0.849, p5 0.850, p50 0.853, p95 0.858, max 0.858
- [PASS] Related score distribution — min 0.603, p5 0.607, p50 0.809, p95 0.827, max 0.828
- [PASS] Noise score distribution — min 0.384, p5 0.384, p50 0.384, p95 0.409, max 0.431
- [PASS] Threshold 0.400 — noise rejected 93.6%, exact kept 100.0%, related kept 100.0%, F1 0.551
- [PASS] Best scanned threshold 0.450 — F1 1.000

## Proxy Stream Paths (3)

- [PASS] Proxy non-streaming passthrough
- [PASS] Proxy SSE streaming response
- [PASS] Stream content-type and termination — text/event-stream + [DONE] marker verified

## Sharing Webhooks (3)

- [PASS] Create shared namespace with webhook_url
- [PASS] Webhook URL persisted in shared namespace — webhook_url visible in listing
- [PASS] Webhook fires on memory store — 1 webhook(s) received

## Backup / Restore (5)

- [PASS] Pre-backup memory count — 5 memories stored
- [PASS] Backup creates valid archive — 31893 bytes
- [PASS] Restore from backup succeeds
- [PASS] Memories survive backup→restore cycle — pre=5 post=5
- [PASS] Restore refuses non-empty directory — exit=1 as expected

## Embedding Migration (3)

- [PASS] Embedding migration dry-run — counts without writing
- [PASS] Embedding migration execution — all-minilm-l6-v2
- [PASS] Post-migration recall works — 3 memories recalled after migration

## Key Rotation Grace Expiry (6)

- [PASS] Pre-rotation recall works
- [PASS] Key rotation succeeds — status=200
- [PASS] Recall works during grace period
- [PASS] Retired keys listed during grace — 1 retired key(s)
- [PASS] Store works after rotation with new key
- [PASS] Revoke retired key succeeds — status=200

## Coverage Gaps
