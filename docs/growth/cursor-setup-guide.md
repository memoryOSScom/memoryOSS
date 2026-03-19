# Add Persistent Memory to Cursor in 2 Minutes

Every time you start a new Cursor session, the AI forgets everything from the last one. [memoryOSS](https://github.com/memoryOSScom/memoryOSS) fixes that. It runs locally, registers an MCP server for Cursor, and installs a managed rule so project decisions, past fixes, and preferences can be recalled consistently across sessions.

## Prerequisites

- Cursor installed
- macOS, Linux, or Windows

## Step 1: Install memoryOSS

**macOS / Linux:**

```bash
curl -fsSL https://memoryoss.com/install.sh | sh
```

Or manually:

```bash
curl -L https://github.com/memoryOSScom/memoryOSS/releases/latest/download/memoryoss-linux-x86_64.tar.gz -o memoryoss.tar.gz
tar xzf memoryoss.tar.gz
sudo install -m 0755 memoryoss /usr/local/bin/memoryoss
```

**Windows (PowerShell):**

```powershell
Invoke-WebRequest https://github.com/memoryOSScom/memoryOSS/releases/latest/download/memoryoss-windows-x86_64.zip -OutFile memoryoss.zip
Expand-Archive .\memoryoss.zip -DestinationPath .\memoryoss
```

## Step 2: Run the Setup Wizard

```bash
memoryoss setup --profile cursor
```

This does three things automatically:

1. Writes your local config at the path you chose (default: `./memoryoss.toml`)
2. Writes the MCP config to `~/.cursor/mcp.json`
3. Creates a runtime rule at `~/.cursor/rules/memoryoss.mdc` that teaches Cursor when to recall and store memories

That's it. The wizard handles the wiring.

## Step 3: Verify the MCP Config

After setup, your `~/.cursor/mcp.json` should contain:

```json
{
  "mcpServers": {
    "memoryoss": {
      "type": "stdio",
      "command": "/usr/local/bin/memoryoss",
      "args": ["-c", "/absolute/path/to/memoryoss.toml", "mcp-server"],
      "env": {}
    }
  }
}
```

The wizard writes the correct paths for your system. If you need to edit manually, open Cursor Settings > MCP and add the entry above with your actual paths.

## Step 4: Test It

1. Open Cursor and start a new chat
2. Ask: *"Store a memory: this project uses pytest for testing"*
3. Close the chat, open a new one
4. Ask: *"What testing framework does this project use?"*

If Cursor answers "pytest" from memory, everything works.

## What Happens Next

Cursor now has four memory tools available via MCP:

- **memoryoss_recall** -- retrieves relevant memories at session start
- **memoryoss_store** -- saves new facts worth remembering
- **memoryoss_update** -- refines existing memories
- **memoryoss_forget** -- removes outdated memories

The installed runtime rule (`memoryoss.mdc`) instructs Cursor to call `memoryoss_recall` at the start of every session and `memoryoss_store` when it learns something important. Over time, Cursor builds up project context that persists across sessions: architecture decisions, debugging history, your coding preferences, deployment notes.

All data stays local on your machine.

## Troubleshooting

**Cursor doesn't see the MCP tools:**
Run `memoryoss doctor` to check your setup. Re-run `memoryoss setup --profile cursor` if needed.

**Wrong binary path after update:**
Run `memoryoss setup --profile cursor` again to refresh all paths.

**Windows: binary not found:**
Use the full path to `memoryoss.exe` in your MCP config instead of relying on PATH.

**Reset everything:**
```bash
memoryoss setup --profile cursor
memoryoss doctor --repair
```

---

[memoryOSS on GitHub](https://github.com/memoryOSScom/memoryOSS) | [Website](https://memoryoss.com)
