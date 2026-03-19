# Cursor forgets everything between sessions — here's how to fix it

**Subreddit: r/cursor**

---

Cursor is great until you close the tab. Next session, it has no idea what you were working on, what decisions you made, or what conventions your project uses. You end up re-explaining context that should just be... remembered.

memoryOSS adds persistent long-term memory to Cursor. It's a local tool — single Rust binary, nothing goes to the cloud. Here's how to set it up.

## Setup (5 minutes)

### 1. Install memoryOSS

```bash
curl -fsSL https://memoryoss.com/install.sh | sh
```

### 2. Run the setup wizard

```bash
memoryoss setup --profile cursor
```

This writes your local `memoryoss.toml` at the selected config path, plus `~/.cursor/mcp.json` and `~/.cursor/rules/memoryoss.mdc`.

### 3. Restart Cursor

After restarting, Cursor now has access to memory tools:
- `memoryoss_recall` — retrieves relevant memories at the start of each conversation
- `memoryoss_store` — saves important context for future sessions
- `memoryoss_update` — updates existing memories when things change
- `memoryoss_forget` — removes outdated information

### 4. Managed rule is already installed

The `cursor` setup profile already installs a managed runtime rule. The behavior is effectively:

```
Call memoryoss_recall at the START of every conversation to check what you already know about this project. Call memoryoss_store when you learn something worth remembering.
```

## What changes in practice

**Before:** "What framework is this project using?" every. single. session.

**After:** Cursor recalls your stack, your patterns, your preferences, and that weird workaround in the payment module you explained last Tuesday.

## How it works under the hood

- 4-channel hybrid retrieval (vector + BM25 + exact match + recency) — finds the right memories, not just vaguely similar ones
- Trust scoring — memories strengthen when reconfirmed, decay when unused
- Multi-tenant — separate memory namespaces per project
- AES-256-GCM encryption for stored memories
- 44.4% token efficiency — injects relevant context without flooding your context window

## Key points

- **100% local** — your code context never leaves your machine
- **No Docker, no database** — single binary, runs in the background
- **AGPL-3.0 open source**

GitHub: https://github.com/memoryOSScom/memoryOSS

This has been a game changer for large projects where the context window isn't big enough to hold everything Cursor needs to know. Questions welcome.
