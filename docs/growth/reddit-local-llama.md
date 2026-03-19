# memoryOSS: Local-first persistent memory for AI agents — single Rust binary, no cloud, no Docker

**Subreddit: r/LocalLLaMA**

---

Built something you might find useful if you're running local models and want them to remember things between sessions.

## The problem

AI agents are stateless. Every conversation starts from zero. If you're using local models for coding assistance, you end up re-feeding the same context about your project structure, preferences, and past decisions every single time.

## What memoryOSS does

It's a local HTTP proxy that sits between your client and your model (or API). It intercepts requests, retrieves relevant memories from previous sessions, and injects them into the context. When the model learns something worth keeping, it stores it locally.

## Why you might care (local-first crowd specifically)

- **Nothing leaves your machine.** Zero cloud dependencies. All data stored locally with AES-256-GCM encryption per namespace.
- **Single binary.** Install via `curl -fsSL https://memoryoss.com/install.sh | sh` or download a release archive. No Docker. No Postgres. No Redis. No Elasticsearch. It's one Rust binary.
- **Works with any OpenAI-compatible client.** If your tool can talk to an OpenAI-compatible API, memoryOSS can sit in front of it. LM Studio, ollama (via compatible proxy), text-generation-webui, whatever you're running.
- **Also works as an MCP server** for tools that support MCP (Claude Code, Cursor, etc.)

## How retrieval works

Not just vector search. 4 channels run in parallel:

1. **Vector similarity** — semantic matching via embeddings
2. **BM25** — keyword/term frequency matching
3. **Exact match** — for specific identifiers, paths, config values
4. **Recency** — recent memories get a boost

Results are fused and ranked. Trust scoring with Bayesian signals means memories strengthen when confirmed and decay when unused. Bad memories get quarantined.

## Numbers

- 302 tests
- 20K memory retention benchmark
- 44.4% token efficiency (injects relevant context, not your entire memory dump)
- Multi-tenant — isolate memories per project with namespace encryption

## The catch

AGPL-3.0 licensed. Fully open source. No "community edition" bait-and-switch.

GitHub: https://github.com/memoryOSScom/memoryOSS

If you're self-hosting your inference stack, this fits right in. Interested to hear how people would want to integrate it with their local setups.
