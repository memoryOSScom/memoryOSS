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
CLAUDE_HOOK_EVENTS = [
    "PreToolUse",
    "SessionStart",
    "Stop",
    "SubagentStop",
    "UserPromptSubmit",
]


def free_port() -> int:
    """Get a random free port."""
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        s.bind(("", 0))
        return s.getsockname()[1]


PORT = (
    int(os.environ["WIZARD_TEST_PORT"])
    if "WIZARD_TEST_PORT" in os.environ
    else free_port()
)


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


def write_stale_codex_config(home: Path) -> None:
    codex_dir = home / ".codex"
    codex_dir.mkdir(parents=True, exist_ok=True)
    (codex_dir / "config.toml").write_text(
        '[mcp_servers.memoryoss]\ncommand = "/usr/local/bin/memoryoss"\nargs = ["-c", "/tmp/stale-mcp/memoryoss.toml", "mcp-server"]\n',
        encoding="utf-8",
    )


def write_stale_cursor_integration(home: Path) -> None:
    cursor_dir = home / ".cursor"
    cursor_dir.mkdir(parents=True, exist_ok=True)
    (cursor_dir / "mcp.json").write_text(
        json.dumps(
            {
                "mcpServers": {
                    "memoryoss": {
                        "type": "stdio",
                        "command": "/usr/local/bin/memoryoss",
                        "args": ["-c", "/tmp/stale-cursor/memoryoss.toml", "mcp-server"],
                        "env": {},
                    }
                }
            },
            indent=2,
        ),
        encoding="utf-8",
    )
    rules_dir = cursor_dir / "rules"
    rules_dir.mkdir(parents=True, exist_ok=True)
    (rules_dir / "memoryoss.mdc").write_text(
        "# stale cursor rule\n- no memoryoss markers here\n",
        encoding="utf-8",
    )


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


def write_team_manifest(tmp: Path) -> Path:
    manifest_path = tmp / "team-bootstrap.json"
    manifest = {
        "team_id": "team-alpha",
        "team_label": "Team Alpha",
        "catalog": {
            "catalog_id": "team-alpha-defaults",
            "label": "Team Alpha Defaults",
            "exported_at": iso_now(),
            "identities": [
                {
                    "id": "device:team-alpha-signer",
                    "kind": "device",
                    "label": "Team Alpha Signer",
                    "registered_at": iso_now(),
                }
            ],
            "revocations": [],
        },
    }
    manifest_path.write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
    return manifest_path


def make_which_stub(bin_dir: Path, *, has_claude: bool, has_codex: bool, has_cursor: bool) -> None:
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
  cursor|cursor-agent)
    {'exit 0' if has_cursor else 'exit 1'}
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


def read_json(path: Path) -> dict:
    return json.loads(path.read_text(encoding="utf-8"))


def claude_hook_command(hook_path: Path) -> str:
    return f"python3 {hook_path}"


def claude_hooks_configured(settings_local: dict, hook_path: Path) -> bool:
    hooks = settings_local.get("hooks", {})
    expected_command = claude_hook_command(hook_path)
    for event in CLAUDE_HOOK_EVENTS:
        entries = hooks.get(event)
        if not isinstance(entries, list) or not entries:
            return False
        hook_entries = entries[0].get("hooks", [])
        if not isinstance(hook_entries, list) or not hook_entries:
            return False
        hook = hook_entries[0]
        if hook.get("type") != "command" or hook.get("command") != expected_command:
            return False
    return True


def run_claude_hook(hook_path: Path, event_name: str, transcript_lines: list[dict], *, tool_name: str = "Bash") -> dict:
    tmp = Path(tempfile.mkdtemp(prefix="memoryoss-claude-hook-"))
    transcript_path = tmp / "transcript.jsonl"
    transcript_path.write_text(
        "".join(json.dumps(line) + "\n" for line in transcript_lines),
        encoding="utf-8",
    )
    os.chmod(transcript_path, 0o644)
    payload = {
        "hook_event_name": event_name,
        "tool_name": tool_name,
        "tool_input": {"cmd": "echo hi"},
        "transcript_path": str(transcript_path),
    }
    result = subprocess.run(
        ["python3", str(hook_path)],
        input=json.dumps(payload),
        text=True,
        capture_output=True,
        check=False,
    )
    shutil.rmtree(tmp, ignore_errors=True)
    if result.returncode != 0:
        raise RuntimeError(f"hook run failed: {result.stderr}")
    return json.loads(result.stdout)


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
    has_cursor: bool,
    openai_key: bool,
    anthropic_key: bool,
    profile: str = "auto",
    choose_claude_api_key: bool = False,
    choose_codex_api_key: bool = False,
    idempotency: bool = False,
    stale_codex_mcp: bool = False,
    stale_cursor_integration: bool = False,
    team_manifest: bool = False,
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
    bin_dir = tmp / "bin"
    bin_dir.mkdir(parents=True, exist_ok=True)
    team_manifest_path = write_team_manifest(tmp) if team_manifest else None

    if has_claude:
        write_claude_oauth_creds(home)
    make_which_stub(
        bin_dir,
        has_claude=has_claude,
        has_codex=has_codex,
        has_cursor=has_cursor,
    )
    if has_codex:
        write_codex_oauth_auth(home)
    if has_cursor:
        (home / ".cursor").mkdir(parents=True, exist_ok=True)

    env = os.environ.copy()
    env["HOME"] = str(home)
    env["SHELL"] = "/bin/bash"
    env["PATH"] = f"{bin_dir}:/usr/bin:/bin:/usr/local/bin"
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
        write_stale_codex_config(home)
    if stale_cursor_integration:
        write_stale_cursor_integration(home)

    def launch(log_target: Path):
        handle = log_target.open("wb")
        args = [str(binary), "--config", str(config_path), "setup", "--profile", profile]
        if team_manifest_path is not None:
            args.extend(["--team-manifest", str(team_manifest_path)])
        process = subprocess.Popen(
            args,
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
    cursor_mcp_path = home / ".cursor" / "mcp.json"
    cursor_rule_path = home / ".cursor" / "rules" / "memoryoss.mdc"
    team_receipt_path = home / ".memoryoss" / "team-bootstrap.json"
    team_trust_path = home / ".memoryoss" / "data" / "trust-fabric.json"

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
            assert_result(
                "Selected setup profile persisted",
                f'profile = "{profile.replace("-", "_")}"' in config_text,
            ),
            assert_result(
                "Team bootstrap metadata matches scenario",
                ("team_id = " in config_text) == team_manifest,
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
        ]
    )

    if has_claude:
        claude_user = read_json(home / ".claude.json")
        claude_settings = read_json(home / ".claude" / "settings.json")
        claude_settings_local = read_json(home / ".claude" / "settings.local.json")
        hook_path = home / ".claude" / "memoryoss-guard.py"
        expected_args = ["-c", str(config_path), "mcp-server"]
        assertions.extend(
            [
                assert_result("Claude user MCP config written", hook_path.parent.exists() and "memoryoss" in claude_user.get("mcpServers", {})),
                assert_result(
                    "Claude user MCP command matches setup binary",
                    claude_user.get("mcpServers", {}).get("memoryoss", {}).get("args") == expected_args,
                ),
                assert_result(
                    "Claude compatibility MCP config written",
                    claude_settings.get("mcpServers", {}).get("memoryoss", {}).get("args") == expected_args,
                ),
                assert_result("Claude guard hook script written", hook_path.exists()),
                assert_result("Claude hook script is executable", os.access(hook_path, os.X_OK)),
                assert_result(
                    "Claude settings.local configures all memoryOSS hooks",
                    claude_hooks_configured(claude_settings_local, hook_path),
                ),
                assert_result(
                    "Claude statusline configured",
                    "memoryOSS health indicator" in (home / ".claude" / "statusline-command.sh").read_text(encoding="utf-8"),
                ),
            ]
        )

        deny = run_claude_hook(hook_path, "PreToolUse", [])
        allow = run_claude_hook(
            hook_path,
            "PreToolUse",
            [
                {
                    "type": "assistant",
                    "message": {
                        "content": [
                            {"type": "tool_use", "name": "mcp__memoryoss__memoryoss_recall"}
                        ]
                    },
                }
            ],
        )
        stop_block = run_claude_hook(
            hook_path,
            "Stop",
            [
                {
                    "type": "assistant",
                    "message": {
                        "content": [
                            {"type": "tool_use", "name": "Bash"}
                        ]
                    },
                }
            ],
        )
        assertions.append(
            assert_result(
                "Claude hook denies tool use before recall",
                deny.get("hookSpecificOutput", {}).get("permissionDecision") == "deny",
            )
        )
        assertions.append(
            assert_result(
                "Claude hook allows tool use after recall",
                allow.get("continue") is True
                and allow.get("hookSpecificOutput") is None,
            )
        )
        assertions.append(
            assert_result(
                "Claude hook blocks stop without store",
                stop_block.get("continue") is False and stop_block.get("decision") == "block",
            )
        )

    if has_codex:
        codex_config = (home / ".codex" / "config.toml").read_text(encoding="utf-8")
        agents_text = (home / "AGENTS.md").read_text(encoding="utf-8")
        assertions.extend(
            [
                assert_result(
                    "Codex MCP config written",
                    '[mcp_servers.memoryoss]' in codex_config and f'"{config_path}"' in codex_config,
                ),
                assert_result(
                    "Codex AGENTS policy block written",
                    "MEMORYOSS_POLICY_BEGIN" in agents_text and "memoryoss_recall" in agents_text,
                ),
            ]
        )

    expect_claude = profile == "claude" or (profile in {"auto", "team-node"} and has_claude)
    expect_codex = profile == "codex" or (profile in {"auto", "team-node"} and has_codex)
    expect_cursor = profile == "cursor" or (profile in {"auto", "team-node"} and has_cursor)
    cursor_mcp_text = cursor_mcp_path.read_text(encoding="utf-8") if cursor_mcp_path.exists() else ""
    cursor_rule_text = cursor_rule_path.read_text(encoding="utf-8") if cursor_rule_path.exists() else ""
    assertions.extend(
        [
            assert_result(
                "Cursor MCP surface matches selected profile",
                ('"memoryoss"' in cursor_mcp_text) == expect_cursor,
            ),
            assert_result(
                "Cursor runtime rule matches selected profile",
                ("MEMORYOSS_CURSOR_RULE_BEGIN" in cursor_rule_text) == expect_cursor,
            ),
        ]
    )

    if team_manifest:
        receipt = read_json(team_receipt_path)
        trust_text = team_trust_path.read_text(encoding="utf-8") if team_trust_path.exists() else ""
        expected_clients = []
        if expect_claude:
            expected_clients.append("claude")
        if expect_codex:
            expected_clients.append("codex")
        if expect_cursor:
            expected_clients.append("cursor")
        assertions.extend(
            [
                assert_result(
                    "Team manifest path persisted",
                    f'team_manifest_path = "{team_manifest_path}"' in config_text,
                ),
                assert_result("Team bootstrap receipt written", team_receipt_path.exists()),
                assert_result(
                    "Team bootstrap receipt preserves configured clients",
                    receipt.get("configured_clients") == expected_clients,
                ),
                assert_result(
                    "Team trust catalog imported into local trust fabric",
                    '"catalog_id": "team-alpha-defaults"' in trust_text
                    and '"device:team-alpha-signer"' in trust_text,
                ),
            ]
        )

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
            "cursor": has_cursor,
            "profile": profile,
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
        ("No tools at all", False, False, False, False, False, "auto", False, False, False, False, False, False),
        ("Claude OAuth only", True, False, False, False, False, "auto", False, False, False, False, False, False),
        ("Codex OAuth only", False, True, False, False, False, "auto", False, False, False, False, False, False),
        ("Both OAuth without keys", True, True, False, False, False, "auto", False, False, False, False, False, False),
        ("Claude OAuth + OpenAI key", True, False, False, True, False, "auto", False, False, False, False, False, False),
        ("Claude OAuth + Anthropic key (default OAuth)", True, False, False, False, True, "auto", False, False, False, False, False, False),
        ("Claude OAuth + Anthropic key (force API key)", True, False, False, False, True, "auto", True, False, False, False, False, False),
        ("Both OAuth + OpenAI key (default OAuth)", True, True, False, True, False, "auto", False, False, False, False, False, False),
        ("Both OAuth + OpenAI key (force Codex API key)", True, True, False, True, False, "auto", False, True, False, False, False, False),
        ("Codex OAuth + OpenAI key (default OAuth)", False, True, False, True, False, "auto", False, False, False, False, False, False),
        ("Codex OAuth + OpenAI key (force API key)", False, True, False, True, False, "auto", False, True, False, False, False, False),
        ("Cursor explicit profile", False, False, True, False, False, "cursor", False, False, False, False, False, False),
        ("Cursor stale integration is repaired", False, False, True, False, False, "cursor", False, False, False, True, False, False),
        ("Cursor opt-out via Codex profile removes Cursor surfaces", False, True, True, False, False, "codex", False, False, False, False, False, False),
        ("Idempotency double run", True, False, False, False, False, "auto", False, False, True, False, False, False),
        ("Codex stale MCP config is repaired", False, True, False, False, False, "auto", False, False, False, False, True, False),
        ("Team node bootstrap with rerun", True, True, True, False, False, "team-node", False, False, True, False, False, True),
    ]

    results = [
        run_setup_once(
            name=name,
            has_claude=has_claude,
            has_codex=has_codex,
            has_cursor=has_cursor,
            openai_key=openai_key,
            anthropic_key=anthropic_key,
            profile=profile,
            choose_claude_api_key=choose_claude_api_key,
            choose_codex_api_key=choose_codex_api_key,
            idempotency=idempotency,
            stale_codex_mcp=stale_codex_mcp,
            stale_cursor_integration=stale_cursor_integration,
            team_manifest=team_manifest,
        )
        for (
            name,
            has_claude,
            has_codex,
            has_cursor,
            openai_key,
            anthropic_key,
            profile,
            choose_claude_api_key,
            choose_codex_api_key,
            idempotency,
            stale_cursor_integration,
            stale_codex_mcp,
            team_manifest,
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
