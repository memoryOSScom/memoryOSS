#!/usr/bin/env python3
import json
import os
import shutil
import socket
import subprocess
import tempfile
import time
from pathlib import Path

from runner_common import ROOT_DIR, ensure_binary, http_json, iso_now, stop_process


OUTPUT_JSON = Path(os.environ.get("WIZARD_MATRIX_OUTPUT_JSON", ROOT_DIR / "tests" / "wizard-matrix.json"))


def free_port() -> int:
    """Get a random free port."""
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        s.bind(("", 0))
        return s.getsockname()[1]


PORT = int(os.environ.get("WIZARD_TEST_PORT", str(free_port())))


def port_available(port: int) -> bool:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as sock:
        sock.settimeout(0.2)
        return sock.connect_ex(("127.0.0.1", port)) != 0


def wait_for_port_available(port: int, timeout: float = 20.0) -> bool:
    deadline = time.time() + timeout
    while time.time() < deadline:
        if port_available(port):
            return True
        time.sleep(0.5)
    return False


def make_codex_stub(bin_dir: Path, log_path: Path) -> None:
    stub_path = bin_dir / "codex"
    stub_path.write_text(
        """#!/usr/bin/env python3
import json
import os
import sys
from pathlib import Path

log_path = Path(os.environ["WIZARD_CODEX_LOG"])
log_path.parent.mkdir(parents=True, exist_ok=True)
with log_path.open("a", encoding="utf-8") as handle:
    handle.write(json.dumps(sys.argv[1:]) + "\\n")

if len(sys.argv) >= 4 and sys.argv[1:4] == ["mcp", "add", "memoryoss"]:
    print("added memoryoss")
    raise SystemExit(0)
if len(sys.argv) >= 4 and sys.argv[1:4] == ["mcp", "remove", "memoryoss"]:
    print("removed memoryoss")
    raise SystemExit(0)
if len(sys.argv) >= 3 and sys.argv[1:3] == ["mcp", "list"]:
    print("memoryoss")
    raise SystemExit(0)
if len(sys.argv) >= 4 and sys.argv[1:4] == ["mcp", "get", "memoryoss"]:
    print(os.environ.get("WIZARD_CODEX_GET_OUTPUT", "memoryoss"))
    raise SystemExit(0)
raise SystemExit(0)
""",
        encoding="utf-8",
    )
    stub_path.chmod(0o755)


def write_codex_oauth_auth(home: Path) -> None:
    codex_dir = home / ".codex"
    codex_dir.mkdir(parents=True, exist_ok=True)
    auth = {
        "auth_mode": "chatgpt",
        "tokens": {
            "access_token": "oauth-test-token",
            "refresh_token": "oauth-refresh-token",
        },
        "OPENAI_API_KEY": None,
    }
    (codex_dir / "auth.json").write_text(json.dumps(auth), encoding="utf-8")


def write_claude_oauth_creds(home: Path) -> None:
    claude_dir = home / ".claude"
    claude_dir.mkdir(parents=True, exist_ok=True)
    creds = {
        "claudeAiOauth": {
            "subscriptionType": "max",
            "accessToken": "oauth-test-token",
        }
    }
    (claude_dir / ".credentials.json").write_text(json.dumps(creds), encoding="utf-8")


def make_which_stub(bin_dir: Path, *, has_claude: bool, has_codex: bool) -> None:
    stub_path = bin_dir / "which"
    stub_path.write_text(
        f"""#!/usr/bin/env bash
case "${{1:-}}" in
  claude)
    {'exit 0' if has_claude else 'exit 1'}
    ;;
  codex)
    {'exit 0' if has_codex else 'exit 1'}
    ;;
esac
exec /usr/bin/which "$@"
""",
        encoding="utf-8",
    )
    stub_path.chmod(0o755)


def wait_for_ready(log_path: Path, config_path: Path, timeout: float = 40.0) -> bool:
    deadline = time.time() + timeout
    while time.time() < deadline:
        if config_path.exists():
            try:
                status, body = http_json("GET", f"http://127.0.0.1:{PORT}/health", timeout=2.0)
                log_text = log_path.read_text(encoding="utf-8", errors="replace") if log_path.exists() else ""
                if (
                    status == 200
                    and body
                    and body.get("status") == "ok"
                    and "Setup done" in log_text
                ):
                    return True
            except Exception:
                pass
        time.sleep(0.5)
    if log_path.exists():
        print(log_path.read_text(encoding="utf-8", errors="replace")[-2000:])
    return False


def assert_result(name: str, passed: bool, note: str | None = None) -> dict:
    result = {"name": name, "status": "pass" if passed else "fail"}
    if note:
        result["note"] = note
    return result


def run_setup_once(
    *,
    name: str,
    has_claude: bool,
    has_codex: bool,
    openai_key: bool,
    anthropic_key: bool,
    choose_claude_api_key: bool = False,
    choose_codex_api_key: bool = False,
    idempotency: bool = False,
    stale_codex_mcp: bool = False,
) -> dict:
    if not wait_for_port_available(PORT):
        raise RuntimeError(f"port {PORT} is already in use; wizard matrix requires it to be free")

    binary = ensure_binary()
    tmp = Path(tempfile.mkdtemp(prefix="memoryoss-wizard-"))
    home = tmp / "home"
    home.mkdir(parents=True, exist_ok=True)
    data_dir = home / ".memoryoss" / "data"
    data_dir.mkdir(parents=True, exist_ok=True)
    (data_dir / "memoryoss.redb").write_bytes(b"wizard-matrix-existing-data")
    config_path = tmp / "memoryoss.toml"
    log_path = tmp / "setup.log"
    codex_log = tmp / "codex.log"
    bin_dir = tmp / "bin"
    bin_dir.mkdir(parents=True, exist_ok=True)

    if has_claude:
        write_claude_oauth_creds(home)
    make_which_stub(bin_dir, has_claude=has_claude, has_codex=has_codex)
    if has_codex:
        write_codex_oauth_auth(home)
        make_codex_stub(bin_dir, codex_log)

    env = os.environ.copy()
    env["HOME"] = str(home)
    env["SHELL"] = "/bin/bash"
    env["PATH"] = f"{bin_dir}:/usr/bin:/bin:/usr/local/bin"
    env["WIZARD_CODEX_LOG"] = str(codex_log)
    if has_codex:
        env["CODEX_HOME"] = str(home / ".codex")
    else:
        env.pop("CODEX_HOME", None)
    if openai_key:
        env["OPENAI_API_KEY"] = "sk-test-openai-key-1234567890"
    else:
        env.pop("OPENAI_API_KEY", None)
    if anthropic_key:
        env["ANTHROPIC_API_KEY"] = "sk-ant-test-anthropic-key-1234567890"
    else:
        env.pop("ANTHROPIC_API_KEY", None)
    env.pop("OPENAI_BASE_URL", None)
    env.pop("ANTHROPIC_BASE_URL", None)
    env["MEMORYOSS_PORT"] = str(PORT)
    env["MEMORYOSS_DISABLE_SYSTEMD"] = "1"
    if stale_codex_mcp:
        env["WIZARD_CODEX_GET_OUTPUT"] = (
            "memoryoss\n"
            "  enabled: true\n"
            "  transport: stdio\n"
            "  command: /usr/local/bin/memoryoss\n"
            "  args: -c /tmp/stale-mcp/memoryoss.toml mcp-server\n"
        )
    else:
        env.pop("WIZARD_CODEX_GET_OUTPUT", None)

    def launch(log_target: Path):
        handle = log_target.open("wb")
        process = subprocess.Popen(
            [str(binary), "--config", str(config_path), "setup"],
            cwd=ROOT_DIR,
            env=env,
            stdin=subprocess.PIPE,
            stdout=handle,
            stderr=handle,
        )
        assert process.stdin is not None
        prompt_answers = []
        if has_claude and anthropic_key:
            prompt_answers.append("2" if choose_claude_api_key else "")
        if has_codex and openai_key:
            prompt_answers.append("2" if choose_codex_api_key else "")
        prompt_answers.append("1")
        process.stdin.write(("\n".join(prompt_answers) + "\n").encode("utf-8"))
        process.stdin.close()
        return process, handle

    assertions = []

    process, handle = launch(log_path)
    try:
        ready = wait_for_ready(log_path, config_path)
        assertions.append(assert_result("Wizard reached ready health check", ready))
        if not ready:
            stop_process(process)
            process.wait(timeout=5)
            raise RuntimeError(f"wizard scenario '{name}' did not become ready")
        stop_process(process)
        process.wait(timeout=5)
        # Kill any background server the setup spawned
        subprocess.run(["pkill", "-f", f"memoryoss.*{config_path}.*serve"], capture_output=True)
        time.sleep(1)
    finally:
        handle.close()

    config_text = config_path.read_text(encoding="utf-8")
    bashrc_path = home / ".bashrc"
    bashrc_text = bashrc_path.read_text(encoding="utf-8") if bashrc_path.exists() else ""
    log_text = log_path.read_text(encoding="utf-8", errors="replace")
    codex_calls = []
    if codex_log.exists():
        codex_calls = [json.loads(line) for line in codex_log.read_text(encoding="utf-8").splitlines() if line.strip()]

    expect_extraction = openai_key or anthropic_key
    expect_openai_base = openai_key and (not has_codex or choose_codex_api_key)
    expect_anthropic_base = anthropic_key and choose_claude_api_key
    expect_extract_provider = "openai" if openai_key or not anthropic_key else "claude"
    expect_extract_model = (
        "claude-haiku-4-5-20251001" if expect_extract_provider == "claude" else "gpt-4o-mini"
    )

    assertions.extend(
        [
            assert_result("Config file written", config_path.exists()),
            assert_result("Existing memory prompt shown", "Existing memories detected" in log_text),
            assert_result("Full mode persisted", 'default_memory_mode = "full"' in config_text),
            assert_result("Proxy passthrough enabled", "passthrough_auth = true" in config_text),
            assert_result("Generated admin key uses ek_ prefix", 'key = "ek_' in config_text),
            assert_result(
                "Extraction flag matches available real provider credentials",
                f"extraction_enabled = {'true' if expect_extraction else 'false'}" in config_text,
            ),
            assert_result(
                "Extraction provider matches scenario",
                f'extract_provider = "{expect_extract_provider}"' in config_text,
            ),
            assert_result(
                "Extraction model matches provider",
                f'extract_model = "{expect_extract_model}"' in config_text,
            ),
            assert_result("Ready banner printed", "Setup done" in log_text),
            assert_result(
                "OPENAI_BASE_URL export matches scenario",
                ("OPENAI_BASE_URL=" in bashrc_text) == expect_openai_base,
            ),
            assert_result(
                "ANTHROPIC_BASE_URL export matches scenario",
                ("ANTHROPIC_BASE_URL=" in bashrc_text) == expect_anthropic_base,
            ),
            assert_result(
                "Codex MCP registration matches scenario",
                (len(codex_calls) > 0) == has_codex,
                None if not has_codex else f"{len(codex_calls)} codex invocation(s)",
            ),
        ]
    )

    if has_codex:
        flattened = [" ".join(call) for call in codex_calls]
        assertions.append(
            assert_result(
                "Codex MCP add invoked with memoryoss mcp-server",
                any("mcp add memoryoss --" in call and "mcp-server" in call for call in flattened),
            )
        )
        if stale_codex_mcp:
            assertions.append(
                assert_result(
                    "Codex stale MCP registration removed before add",
                    any("mcp remove memoryoss" in call for call in flattened),
                )
            )
    else:
        assertions.append(assert_result("No Codex MCP log created", not codex_log.exists()))

    if idempotency:
        second_log = tmp / "setup-second.log"
        process2, handle2 = launch(second_log)
        try:
            exit_code = process2.wait(timeout=10)
            assertions.append(assert_result("Second run exits cleanly", exit_code == 0))
        finally:
            handle2.close()
        second_bashrc = bashrc_path.read_text(encoding="utf-8") if bashrc_path.exists() else ""
        second_log_text = second_log.read_text(encoding="utf-8", errors="replace")
        assertions.extend(
            [
                assert_result(
                    "Shell config is unchanged on second run",
                    second_bashrc == bashrc_text,
                ),
                assert_result(
                    "Second run keeps existing config by default",
                    "Keeping existing config." in second_log_text,
                ),
            ]
        )

    passed = all(entry["status"] == "pass" for entry in assertions)
    result = {
        "name": name,
        "status": "pass" if passed else "fail",
        "signals": {
            "claude": has_claude,
            "codex": has_codex,
            "openai_key": openai_key,
            "anthropic_key": anthropic_key,
        },
        "assertions": assertions,
        "assertion_count": len(assertions),
    }

    shutil.rmtree(tmp, ignore_errors=True)
    return result


def main() -> None:
    started = time.time()
    scenarios = [
        ("No tools at all", False, False, False, False, False, False, False, False),
        ("Claude OAuth only", True, False, False, False, False, False, False, False),
        ("Codex OAuth only", False, True, False, False, False, False, False, False),
        ("Both OAuth without keys", True, True, False, False, False, False, False, False),
        ("Claude OAuth + OpenAI key", True, False, True, False, False, False, False, False),
        ("Claude OAuth + Anthropic key (default OAuth)", True, False, False, True, False, False, False, False),
        ("Claude OAuth + Anthropic key (force API key)", True, False, False, True, True, False, False, False),
        ("Both OAuth + OpenAI key (default OAuth)", True, True, True, False, False, False, False, False),
        ("Both OAuth + OpenAI key (force Codex API key)", True, True, True, False, False, True, False, False),
        ("Codex OAuth + OpenAI key (default OAuth)", False, True, True, False, False, False, False, False),
        ("Codex OAuth + OpenAI key (force API key)", False, True, True, False, False, True, False, False),
        ("Idempotency double run", True, False, False, False, False, False, True, False),
        ("Codex stale MCP config is repaired", False, True, False, False, False, False, False, True),
    ]

    results = [
        run_setup_once(
            name=name,
            has_claude=has_claude,
            has_codex=has_codex,
            openai_key=openai_key,
            anthropic_key=anthropic_key,
            choose_claude_api_key=choose_claude_api_key,
            choose_codex_api_key=choose_codex_api_key,
            idempotency=idempotency,
            stale_codex_mcp=stale_codex_mcp,
        )
        for (
            name,
            has_claude,
            has_codex,
            openai_key,
            anthropic_key,
            choose_claude_api_key,
            choose_codex_api_key,
            idempotency,
            stale_codex_mcp,
        ) in scenarios
    ]

    duration = int(time.time() - started)
    passed_assertions = sum(
        1
        for scenario in results
        for assertion in scenario["assertions"]
        if assertion["status"] == "pass"
    )
    report = {
        "runner": "tests/run_wizard_matrix.sh",
        "generated_at": iso_now(),
        "duration_seconds": duration,
        "summary": {
            "scenarios": len(results),
            "scenarios_passed": sum(1 for scenario in results if scenario["status"] == "pass"),
            "assertions_passed": passed_assertions,
        },
        "scenarios": results,
    }
    OUTPUT_JSON.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(report["summary"], indent=2))


if __name__ == "__main__":
    main()
