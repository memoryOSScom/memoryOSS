#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

provider="${EXTRACTION_EVAL_PROVIDER:-}"
if [ -z "$provider" ]; then
  if [ -n "${ANTHROPIC_API_KEY:-}" ]; then
    provider="claude"
  elif [ -n "${OPENAI_API_KEY:-}" ]; then
    provider="openai"
  fi
fi

if [ -z "$provider" ]; then
  echo "SKIP: no extraction eval provider credentials configured"
  exit 0
fi

export EXTRACTION_EVAL_PROVIDER="$provider"
python3 "$ROOT_DIR/tests/run_extraction_eval.py"
