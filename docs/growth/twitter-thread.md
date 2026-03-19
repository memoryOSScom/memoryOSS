# X/Twitter Thread — memoryOSS Launch

---

**Tweet 1 (Hook)**

Your AI coding agent forgets everything the moment you close the session.

Your architecture decisions. Your conventions. That bug you spent 3 hours debugging.

Gone. Every. Single. Time.

I built something to fix this. 🧵

---

**Tweet 2 (What it is)**

memoryOSS — persistent long-term memory for AI agents.

Single Rust binary. Runs locally. No cloud, no Docker, no database.

It sits between your AI tool and the API, automatically injecting relevant memories from past sessions into the current context.

---

**Tweet 3 (How retrieval works)**

Not just "vector search and pray."

4-channel hybrid retrieval running in parallel:
- Vector similarity (semantic)
- BM25 (keyword matching)
- Exact match (identifiers, paths)
- Recency scoring

Results fused and ranked. 44.4% token efficiency — you get the right context, not a memory dump.

---

**Tweet 4 (Trust scoring)**

Memories aren't static. They have trust scores.

- Confirmed by the agent again? Trust goes up.
- Unused for weeks? Decays.
- Contradicted by new information? Quarantined.

Bayesian scoring means your memory gets more reliable over time, not more cluttered.

---

**Tweet 5 (Compatibility)**

Works with:
- Claude Code (MCP + hooks)
- Cursor (MCP + managed rule)
- Codex CLI (MCP + optional proxy)
- Any OpenAI-compatible client (gateway proxy)

Setup can auto-detect installed tools, or you can pick an explicit profile. Fresh setup is a couple of minutes.

---

**Tweet 6 (Privacy & architecture)**

Everything runs on your machine.

- AES-256-GCM encryption per namespace
- Multi-tenant isolation per project
- No telemetry, no cloud calls
- 302 tests, 20K memory retention benchmark

If you care about keeping your code context private, this matters.

---

**Tweet 7 (CTA)**

AGPL-3.0, fully open source.

GitHub: https://github.com/memoryOSScom/memoryOSS
Website: https://memoryoss.com

Star it, try it, break it, contribute. Feedback welcome.

Built by @[HANDLE] because re-explaining my own codebase to my own tools felt absurd.
