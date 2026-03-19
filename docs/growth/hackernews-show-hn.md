# Show HN: memoryOSS – Persistent memory for AI agents (Rust, single binary)

Every AI coding session starts from zero. Your agent forgets your architecture decisions, your coding conventions, what you tried yesterday that didn't work. You re-explain the same context over and over.

memoryOSS fixes this. It's a local proxy that sits between your AI tool and the API. It intercepts requests, retrieves relevant memories via hybrid retrieval, and injects them into the context window. When the agent learns something worth remembering, it stores it. Next session, that knowledge is there.

**How it works:**

- MCP server + HTTP proxy architecture — works with Claude Code, Codex, Cursor, or any OpenAI-compatible client
- 4-channel hybrid retrieval: vector similarity, BM25, exact match, and recency scoring run in parallel, results are fused
- Trust scoring with Bayesian signals — memories decay, get quarantined if contradicted, strengthen when confirmed
- Multi-tenant with AES-256-GCM namespace isolation per project/agent

**What it's not:**

- Not a cloud service. Runs entirely on your machine. Single Rust binary, no Docker, no database dependencies.
- Not a RAG pipeline. It's purpose-built for agent memory with trust semantics, not document retrieval.

**Numbers:**

- 302 tests passing
- 20K memory retention benchmark
- 44.4% token efficiency (relevant memories injected, not your entire history)
- Setup supports auto-detection or explicit client profiles, and fresh installs are fast

AGPL-3.0, fully open source.

GitHub: https://github.com/memoryOSScom/memoryOSS

Built this because I got tired of re-explaining my own codebase to my own tools. Happy to answer questions about the retrieval architecture or trust scoring.
