#!/usr/bin/env bash
# memoryoss Demo — fallback script for asciinema
# Usage: asciinema rec demo.cast -c ./demo-asciinema.sh
#    or: ./demo-asciinema.sh  (plain terminal playback)

set -e

GREEN='\033[0;32m'
CYAN='\033[0;36m'
DIM='\033[2m'
BOLD='\033[1m'
RESET='\033[0m'

type_slow() {
  local text="$1"
  for (( i=0; i<${#text}; i++ )); do
    printf '%s' "${text:$i:1}"
    sleep 0.03
  done
  echo
}

prompt() {
  printf '%b$ %b' "$BOLD" "$RESET"
  type_slow "$1"
}

info() {
  printf '%b  %s%b\n' "$DIM" "$1" "$RESET"
}

ok() {
  printf '%b  ✓ %s%b\n' "$GREEN" "$1" "$RESET"
}

agent() {
  printf '%b  → %s%b\n' "$CYAN" "$1" "$RESET"
}

recall() {
  printf '%b  ← %s%b\n' "$CYAN" "$1" "$RESET"
}

clear

# --- Install ---
prompt "curl -fsSL https://memoryoss.com/install.sh | sh"
sleep 0.3
info "Downloading memoryoss v0.2.0..."
sleep 0.4
ok "memoryoss v0.2.0 installed to /usr/local/bin/memoryoss"
sleep 0.8

# --- Setup ---
prompt "memoryoss setup --profile claude"
sleep 0.3
ok "Detected Claude Code environment"
sleep 0.2
ok "MCP server configured in ~/.claude/settings.json"
sleep 0.2
ok "Config written: memoryoss.toml"
sleep 0.2
echo "  Ready! memoryoss is now active."
sleep 1

# --- Session 1: Store ---
echo
printf '%b# === Claude Code Session 1 ===%b\n' "$BOLD" "$RESET"
sleep 0.5

echo "You: Store: this project uses PostgreSQL 16 with pgvector"
sleep 0.4
agent 'memoryoss_store({"content": "Project uses PostgreSQL 16 with pgvector", "type": "fact"})'
sleep 0.3
ok "Memory stored (id: mem_a7f3)"
sleep 0.3
echo "Claude: Got it — stored for future sessions."
sleep 1

# --- Session 2: Recall ---
echo
printf '%b# === Claude Code Session 2 (new session) ===%b\n' "$BOLD" "$RESET"
sleep 0.5

agent 'memoryoss_recall({"query": "database"})'
sleep 0.3
recall "[mem_a7f3] Project uses PostgreSQL 16 with pgvector"
sleep 0.4

echo "You: What database does this project use?"
sleep 0.4
echo "Claude: This project uses PostgreSQL 16 with pgvector."
sleep 2
