---
name: memory-best-practices
description: Guidelines for effective memory usage. Use when memoryOSS MCP tools are available to determine what to store, recall, update, or forget.
---

# Memory Best Practices

When memoryOSS is available, follow these guidelines:

## Always Recall First
At the start of every conversation turn, call `memoryoss_recall` with the user's question or topic. You may already know relevant context from previous sessions.

## What to Store
- User preferences ("I prefer TypeScript over JavaScript")
- Project decisions ("We chose PostgreSQL for the database")
- Key facts ("The API endpoint is /api/v2/users")
- Debugging insights ("The auth bug was caused by expired JWT tokens")
- Architecture context ("The frontend uses Next.js with App Router")

## What NOT to Store
- Temporary information (build output, error logs being actively debugged)
- Sensitive data (passwords, API keys, tokens)
- Information that changes every session (current file contents)

## When to Update vs Store New
- Use `memoryoss_update` when a fact changed (e.g., "we switched from PostgreSQL to SQLite")
- Use `memoryoss_store` for new distinct facts
- Memories are automatically deduplicated — storing the same fact twice is harmless

## Tags
Use descriptive tags to make future recall more precise:
- `preference`, `decision`, `architecture`, `debugging`, `context`, `person`
