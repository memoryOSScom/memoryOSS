# memoryOSS — mcp.so Listing Submission Guide

> Verified against the public submit surfaces on 2026-03-19.
> Status: ready to execute, trimmed to claims we could actually verify.

---

## 1. Submission Paths

mcp.so currently exposes two visible submission paths:

1. **Web form** at https://mcp.so/submit
2. **GitHub issue fallback** at https://github.com/chatmcp/mcpso/issues/1

Use the web form first. Keep the GitHub issue as fallback if the form fails or you want a public submission trail.

---

## 2. Web Form Values

Fill the form with:

| Field | Value |
|-------|-------|
| **Type** | `MCP Server` |
| **Name** | `memoryOSS` |
| **URL** | `https://github.com/memoryOSScom/memoryOSS` |
| **Server Config** | paste the JSON below |

### Server Config JSON

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

Use an absolute config path in examples. That matches the current README and the actual CLI shape.

---

## 3. GitHub Issue Fallback

If you submit via GitHub instead, comment on https://github.com/chatmcp/mcpso/issues/1 with:

```text
memoryOSS — Persistent long-term memory for AI agents (Rust, AGPL-3.0)

https://github.com/memoryOSScom/memoryOSS

Local-first persistent memory for AI agents with MCP tools and optional gateway proxy.
Provides memoryoss_store, memoryoss_recall, memoryoss_update, and memoryoss_forget.
Install: curl -fsSL https://memoryoss.com/install.sh | sh
Then run: memoryoss setup --profile claude

Website: https://memoryoss.com
```

---

## 4. Repo Checklist Before Submission

- Ensure the repo description on GitHub is current and specific.
- Keep the README top clear on install, MCP config, and supported clients.
- Keep the MCP JSON example in the README aligned with `memoryoss -c <path> mcp-server`.
- Make sure the repo topics include `mcp`, `mcp-server`, `memory`, `long-term-memory`, `ai-agents`, and `rust`.
- Submit after `v0.2.0` is tagged so the listing points at a current stable release instead of the old `v0.1.1` snapshot.

---

## 5. Notes

- This guide intentionally omits unstable category counts, competitor star counts, and internal database-schema claims.
- If mcp.so changes its form fields, prefer the live form over this draft.
