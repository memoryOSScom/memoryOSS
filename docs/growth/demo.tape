# memoryoss Demo — Persistent Memory for AI Coding Agents
# Run with: vhs demo.tape

Output demo.gif

Set FontSize 16
Set Width 1200
Set Height 600
Set Theme "Catppuccin Mocha"
Set TypingSpeed 40ms
Set Padding 20

# --- Scene 1: Install ---
Sleep 500ms

Type "curl -fsSL https://memoryoss.com/install.sh | sh"
Enter
Sleep 300ms
Type "  Downloading memoryoss v0.2.0..."
Enter
Sleep 400ms
Type "  memoryoss v0.2.0 installed to /usr/local/bin/memoryoss"
Enter
Sleep 800ms

# --- Scene 2: Setup Wizard ---
Type "memoryoss setup --profile claude"
Enter
Sleep 300ms
Type "  ✓ Detected Claude Code environment"
Enter
Sleep 200ms
Type "  ✓ MCP server configured in ~/.claude/settings.json"
Enter
Sleep 200ms
Type "  ✓ Config written: memoryoss.toml"
Enter
Sleep 200ms
Type "  Ready! memoryoss is now active."
Enter
Sleep 1s

# --- Scene 3: Claude Code Session 1 — Storing a memory ---
Type "# === Claude Code Session 1 ==="
Enter
Sleep 500ms

Type "You: Store: this project uses PostgreSQL 16 with pgvector"
Enter
Sleep 400ms

Type "  → memoryoss_store({\"content\": \"Project uses PostgreSQL 16 with pgvector\", \"type\": \"fact\"})"
Enter
Sleep 300ms
Type "  ✓ Memory stored (id: mem_a7f3)"
Enter
Sleep 300ms

Type "Claude: Got it — stored for future sessions."
Enter
Sleep 1s

# --- Scene 4: Claude Code Session 2 — Recall ---
Type "# === Claude Code Session 2 (new session) ==="
Enter
Sleep 500ms

Type "  → memoryoss_recall({\"query\": \"database\"})"
Enter
Sleep 300ms
Type "  ← [mem_a7f3] Project uses PostgreSQL 16 with pgvector"
Enter
Sleep 400ms

Type "You: What database does this project use?"
Enter
Sleep 400ms

Type "Claude: This project uses PostgreSQL 16 with pgvector."
Enter
Sleep 2s
