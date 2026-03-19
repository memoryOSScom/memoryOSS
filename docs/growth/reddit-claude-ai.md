# How I gave Claude Code persistent memory across sessions

**Subreddit: r/ClaudeAI**

---

You know the drill. You spend 2 hours with Claude Code on a complex refactor, it learns your project structure, your naming conventions, your preferences. Next session? Gone. You're back to explaining everything from scratch.

I built memoryOSS to solve this. It gives Claude Code long-term memory that persists between sessions — and it runs entirely on your machine.

## Before memoryOSS

```
You: Refactor the auth module
Claude: What framework are you using? What's your project structure?
        What auth library? What's the naming convention?
You: *sighs, re-explains for the 15th time*
```

## After memoryOSS

```
You: Refactor the auth module
Claude: [recalls: Express + Passport, src/modules/auth/, camelCase,
         JWT with refresh tokens, you prefer explicit error handling]
Claude: I'll refactor the auth module following your existing patterns...
```

## Setup (takes ~2 minutes)

1. Install: `curl -fsSL https://memoryoss.com/install.sh | sh`
2. Run the setup wizard: `memoryoss setup --profile claude`
   - It writes the Claude MCP registration plus the managed local hook path
3. That's it. Claude Code now has the `memoryoss_*` tools available through MCP.

With the managed hook path in place, Claude is nudged into calling `memoryoss_recall` before substantial work and `memoryoss_store` or `memoryoss_update` before stopping after new durable learning.

## What actually gets stored

Not your entire conversation. memoryOSS uses trust scoring — memories strengthen when confirmed, decay when unused, get quarantined if contradicted. After a few sessions, it builds a reliable knowledge base of your project: architecture decisions, preferences, patterns, gotchas.

## Key details

- **Fully local** — no data leaves your machine
- **Single Rust binary** — no Docker, no database, no dependencies
- **4-channel hybrid retrieval** — vector + BM25 + exact match + recency, so it finds the right memories, not just similar ones
- **AGPL-3.0 open source**

GitHub: https://github.com/memoryOSScom/memoryOSS

Been using it daily for a few months now. The difference is especially noticeable on large codebases where re-establishing context eats a significant chunk of your token budget. Happy to answer questions.
