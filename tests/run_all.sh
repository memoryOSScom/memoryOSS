#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

REPORT_DIR="${RUN_ALL_REPORT_DIR:-$ROOT_DIR/tests/.last-run}"
REPORT_JSON="${RUN_ALL_REPORT_JSON:-$ROOT_DIR/tests/report.json}"
REPORT_MD="${RUN_ALL_REPORT_MD:-$ROOT_DIR/tests/report.md}"
WEBSITE_REPORT_JSON="${RUN_ALL_WEBSITE_REPORT_JSON:-$ROOT_DIR/website/tests-report.json}"
WIZARD_MATRIX_JSON="${RUN_ALL_WIZARD_MATRIX_JSON:-$ROOT_DIR/tests/wizard-matrix.json}"
BENCHMARK_JSON="${RUN_ALL_BENCHMARK_JSON:-$ROOT_DIR/tests/benchmark-report.json}"
CALIBRATION_JSON="${RUN_ALL_CALIBRATION_JSON:-$ROOT_DIR/tests/calibration-report.json}"
EXTRACTION_EVAL_JSON="${RUN_ALL_EXTRACTION_EVAL_JSON:-$ROOT_DIR/tests/extraction-eval-report.json}"
COVERAGE_GAPS_JSON="${RUN_ALL_COVERAGE_GAPS_JSON:-$ROOT_DIR/tests/coverage-gaps-report.json}"
mkdir -p "$REPORT_DIR"

# Generate report even on early failure (Codex Befund 4)
generate_report() {
  local end_ts duration
  end_ts="$(date +%s)"
  duration=$((end_ts - RUN_START_TS))
  python3 "$ROOT_DIR/tests/generate_report.py" \
    --steps "$REPORT_DIR/steps.tsv" \
    --output-json "$REPORT_JSON" \
    --output-md "$REPORT_MD" \
    --website-json "$WEBSITE_REPORT_JSON" \
    --wizard-json "$WIZARD_MATRIX_JSON" \
    --benchmark-json "$BENCHMARK_JSON" \
    --calibration-json "$CALIBRATION_JSON" \
    --extraction-eval-json "$EXTRACTION_EVAL_JSON" \
    --coverage-gaps-json "$COVERAGE_GAPS_JSON" \
    --duration "$duration" 2>/dev/null || true
}
trap generate_report EXIT
: >"$REPORT_DIR/steps.tsv"
RUN_START_TS="$(date +%s)"

record_step() {
  local slug="$1"
  local label="$2"
  local status="$3"
  local duration="$4"
  local log_path="$5"
  printf '%s\t%s\t%s\t%s\t%s\n' "$slug" "$label" "$status" "$duration" "$log_path" >>"$REPORT_DIR/steps.tsv"
}

run_step() {
  local label="$1"
  local slug="$2"
  shift 2

  local log_path="$REPORT_DIR/${slug}.log"
  local started finished duration cmd_status status
  started="$(date +%s)"

  printf '\n==> %s\n' "$label" | tee "$log_path"

  set +e
  "$@" 2>&1 | tee -a "$log_path"
  cmd_status=${PIPESTATUS[0]}
  set -e

  finished="$(date +%s)"
  duration=$((finished - started))
  status="pass"
  if grep -q '^SKIP:' "$log_path"; then
    status="skip"
  elif [ "$cmd_status" -ne 0 ]; then
    status="fail"
  fi

  record_step "$slug" "$label" "$status" "$duration" "$log_path"

  if [ "$cmd_status" -ne 0 ]; then
    return "$cmd_status"
  fi
}

typescript_sdk_checks() {
  if ! command -v npm >/dev/null 2>&1; then
    echo "SKIP: npm is not installed"
    return 0
  fi
  if [ ! -f "$ROOT_DIR/sdk/typescript/package.json" ]; then
    echo "SKIP: sdk/typescript/package.json not found"
    return 0
  fi

  (
    cd "$ROOT_DIR/sdk/typescript"
    if [ ! -d node_modules ] || [ "${RUN_ALL_NPM_CI:-0}" = "1" ]; then
      npm ci
    else
      echo "Using existing sdk/typescript/node_modules (set RUN_ALL_NPM_CI=1 to force npm ci)"
    fi
    npm run build
    npm test
  )
}

dependency_audit() {
  if ! cargo audit --version >/dev/null 2>&1; then
    echo "SKIP: cargo-audit is not installed"
    return 0
  fi

  local advisory_db="${CARGO_HOME:-$HOME/.cargo}/advisory-db"
  if [ ! -d "$advisory_db" ]; then
    echo "SKIP: cargo advisory DB not available locally"
    return 0
  fi

  cargo audit --no-fetch \
    --ignore RUSTSEC-2025-0119 \
    --ignore RUSTSEC-2024-0436 \
    --ignore RUSTSEC-2026-0002
}

wizard_smoke_test() {
  if ! command -v curl >/dev/null 2>&1; then
    echo "SKIP: curl is not installed"
    return 0
  fi

  # Use dynamic port to avoid conflicts with running memoryoss
  local WIZARD_PORT
  WIZARD_PORT=$(python3 -c 'import socket; s=socket.socket(); s.bind(("",0)); print(s.getsockname()[1]); s.close()' 2>/dev/null || echo 8199)

  if ss -lnt 2>/dev/null | grep -qE "(:|\\[::\\]:)${WIZARD_PORT}\\b"; then
    echo "SKIP: port ${WIZARD_PORT} is already in use"
    return 0
  fi

  local tmp_home log_path config_path pid ready
  tmp_home="$(mktemp -d)"
  log_path="$tmp_home/setup.log"
  config_path="$tmp_home/test-memoryoss.toml"
  mkdir -p "$tmp_home/bin"

  cat >"$tmp_home/bin/which" <<'EOF'
#!/usr/bin/env bash
case "${1:-}" in
  codex|claude)
    exit 1
    ;;
esac
command -v "${1:-}" >/dev/null 2>&1
EOF
  chmod +x "$tmp_home/bin/which"

  setsid bash -lc '
    export HOME="'"$tmp_home"'"
    export PATH="'"$tmp_home"'/bin:/usr/bin:/bin:/usr/local/bin"
    export MEMORYOSS_PORT="'"$WIZARD_PORT"'"
    export MEMORYOSS_DISABLE_SYSTEMD=1
    unset CODEX_HOME OPENAI_API_KEY ANTHROPIC_API_KEY OPENAI_BASE_URL ANTHROPIC_BASE_URL
    "'"$ROOT_DIR"'/target/debug/memoryoss" --config "'"$config_path"'" setup >"'"$log_path"'" 2>&1
  ' &
  pid=$!
  ready=0

  for _ in $(seq 1 30); do
    if [ -f "$config_path" ] && curl -fsS "http://127.0.0.1:${WIZARD_PORT}/health" >/dev/null 2>&1; then
      ready=1
      break
    fi
    # Don't break when wizard exits — the background server may still be starting
    sleep 1
  done

  if [ "$ready" -ne 1 ]; then
    kill -- "-$pid" 2>/dev/null || true
    wait "$pid" 2>/dev/null || true
    echo "Wizard smoke test failed. Recent log output:"
    tail -n 80 "$log_path" || true
    return 1
  fi

  kill -- "-$pid" 2>/dev/null || true
  wait "$pid" 2>/dev/null || true

  # Kill any background server the setup may have spawned
  pkill -f "memoryoss.*$config_path.*serve" 2>/dev/null || true

  for _ in $(seq 1 20); do
    if ! ss -lnt 2>/dev/null | grep -qE "(:|\\[::\\]:)${WIZARD_PORT}\\b"; then
      break
    fi
    sleep 0.5
  done

  grep -q '\[server\]' "$config_path"
  grep -q 'default_memory_mode = "full"' "$config_path"
  grep -q 'Setup done' "$log_path"

  echo "Wizard smoke test passed"
}

run_step "cargo fmt --check" cargo_fmt cargo fmt --check
run_step "cargo clippy -- -D warnings" cargo_clippy cargo clippy -- -D warnings
run_step "cargo test" cargo_test cargo test
run_step "cargo build" cargo_build cargo build
run_step "setup wizard smoke test" wizard_smoke wizard_smoke_test
run_step "setup wizard matrix" wizard_matrix env WIZARD_MATRIX_OUTPUT_JSON="$WIZARD_MATRIX_JSON" bash "$ROOT_DIR/tests/run_wizard_matrix.sh"
run_step "20k benchmark" benchmark env BENCHMARK_OUTPUT_JSON="$BENCHMARK_JSON" bash "$ROOT_DIR/tests/run_benchmarks.sh"
run_step "scoring calibration" calibration env CALIBRATION_OUTPUT_JSON="$CALIBRATION_JSON" bash "$ROOT_DIR/tests/run_calibration.sh"
run_step "extraction quality evaluation" extraction_eval env EXTRACTION_EVAL_OUTPUT_JSON="$EXTRACTION_EVAL_JSON" bash "$ROOT_DIR/tests/run_extraction_eval.sh"
run_step "coverage gaps" coverage_gaps env COVERAGE_GAPS_OUTPUT_JSON="$COVERAGE_GAPS_JSON" bash "$ROOT_DIR/tests/run_coverage_gaps.sh"
run_step "TypeScript SDK build/test" typescript_sdk typescript_sdk_checks
run_step "cargo audit (offline if available)" cargo_audit dependency_audit

RUN_END_TS="$(date +%s)"
RUN_DURATION=$((RUN_END_TS - RUN_START_TS))

python3 "$ROOT_DIR/tests/generate_report.py" \
  --steps "$REPORT_DIR/steps.tsv" \
  --output-json "$REPORT_JSON" \
  --output-md "$REPORT_MD" \
  --website-json "$WEBSITE_REPORT_JSON" \
  --wizard-json "$WIZARD_MATRIX_JSON" \
  --benchmark-json "$BENCHMARK_JSON" \
  --calibration-json "$CALIBRATION_JSON" \
  --extraction-eval-json "$EXTRACTION_EVAL_JSON" \
  --coverage-gaps-json "$COVERAGE_GAPS_JSON" \
  --duration "$RUN_DURATION"

printf '\nAll checks passed.\n'
printf 'Report written: %s\n' "$REPORT_JSON"
