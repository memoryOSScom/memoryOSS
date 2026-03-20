#!/usr/bin/env python3
"""Deterministic dry-run harness for the main memoryOSS state-machine branches.

This harness spins up:
- a local stub OpenAI-compatible upstream
- a temporary hybrid memoryOSS runtime

It then verifies the four request-time memory modes against the real proxy path:
- full
- readonly
- after
- off

The test is intentionally system-level:
- seed a real memory via /v1/store
- hit /proxy/v1/chat/completions
- inspect x-memory-* headers
- inspect what the upstream actually received
- verify whether extraction stored a new memory or not
"""

from __future__ import annotations

import json
import os
import re
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.error import HTTPError, URLError
from urllib.request import Request, urlopen


SCRIPT_ROOT = Path(__file__).resolve().parent.parent
ROOT = SCRIPT_ROOT if (SCRIPT_ROOT / "README.md").exists() else Path.cwd()
REPORT_PATH = Path(
    os.environ.get(
        "STATE_MACHINE_DRYRUN_REPORT",
        str(ROOT / "tests" / "state-machine-dryrun-report.json"),
    )
)
EXTRACTION_PREFIX = "Extract ONLY project-specific information from this conversation."
SEEDED_SIGNAL = "dryrun seeded injection signal for proxy mode verification"
SEEDED_MEMORY_CONTENT = (
    f"{SEEDED_SIGNAL}. This applies to the local state-machine "
    "dryrun harness for proxy recall verification."
)


def free_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as sock:
        sock.bind(("127.0.0.1", 0))
        return int(sock.getsockname()[1])


def resolve_binary() -> str:
    override = os.environ.get("STATE_MACHINE_DRYRUN_BINARY")
    candidates = [
        override,
        str(ROOT / "target" / "debug" / "memoryoss"),
        shutil.which("memoryoss"),
    ]
    for candidate in candidates:
        if candidate and Path(candidate).exists():
            return candidate
    raise RuntimeError("memoryoss binary not found")


def http_json(method: str, url: str, body=None, headers=None, timeout: float = 10.0):
    payload = None
    request_headers = {"content-type": "application/json"}
    if headers:
        request_headers.update(headers)
    if body is not None:
        payload = json.dumps(body).encode("utf-8")
    request = Request(url, data=payload, headers=request_headers, method=method)
    try:
        with urlopen(request, timeout=timeout) as response:
            raw = response.read().decode("utf-8")
            parsed = json.loads(raw) if raw else {}
            return response.status, dict(response.headers.items()), parsed
    except HTTPError as exc:
        raw = exc.read().decode("utf-8", errors="replace")
        try:
            parsed = json.loads(raw) if raw else {}
        except json.JSONDecodeError:
            parsed = {"raw": raw}
        return exc.code, dict(exc.headers.items()), parsed
    except URLError as exc:
        raise RuntimeError(f"{method} {url} failed: {exc}") from exc


class StubState:
    def __init__(self) -> None:
        self.normal_calls: list[dict] = []
        self.extraction_calls: list[dict] = []


class StubHandler(BaseHTTPRequestHandler):
    state: StubState | None = None

    def log_message(self, format: str, *args) -> None:
        return

    def _write_json(self, status: int, payload: dict) -> None:
        raw = json.dumps(payload).encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(raw)))
        self.end_headers()
        self.wfile.write(raw)

    def do_GET(self) -> None:  # noqa: N802
        if self.path == "/v1/models":
            self._write_json(
                200,
                {"object": "list", "data": [{"id": "stub-model", "object": "model"}]},
            )
            return
        self._write_json(404, {"error": "not found"})

    def do_POST(self) -> None:  # noqa: N802
        length = int(self.headers.get("Content-Length", "0"))
        raw = self.rfile.read(length).decode("utf-8")
        try:
            payload = json.loads(raw) if raw else {}
        except json.JSONDecodeError:
            self._write_json(400, {"error": "invalid json"})
            return

        if self.path != "/v1/chat/completions":
            self._write_json(404, {"error": "not found"})
            return

        prompt = (
            payload.get("messages", [{}])[0].get("content", "")
            if isinstance(payload.get("messages"), list)
            else ""
        )
        mode_match = re.search(r"DRYRUN_MODE_(full|readonly|after|off)", json.dumps(payload))
        mode = mode_match.group(1) if mode_match else "unknown"

        if prompt.startswith(EXTRACTION_PREFIX):
            fact = {
                "content": (
                    f"State-machine dryrun extracted fact for DRYRUN_MODE_{mode} "
                    "on the local proxy harness."
                ),
                "tags": ["state-machine-dryrun", mode],
            }
            if self.state is not None:
                self.state.extraction_calls.append(
                    {
                        "mode": mode,
                        "authorization": self.headers.get("Authorization"),
                        "prompt_preview": prompt[:240],
                    }
                )
            self._write_json(
                200,
                {
                    "id": "cmpl-stub-extraction",
                    "object": "chat.completion",
                    "choices": [{"message": {"role": "assistant", "content": json.dumps([fact])}}],
                },
            )
            return

        if self.state is not None:
            self.state.normal_calls.append(
                {
                    "mode": mode,
                    "authorization": self.headers.get("Authorization"),
                    "payload": payload,
                }
            )
        reply = (
            f"Assistant reply for DRYRUN_MODE_{mode}. "
            f"Remember the project rule for DRYRUN_MODE_{mode} on the local proxy harness."
        )
        self._write_json(
            200,
            {
                "id": "cmpl-stub-normal",
                "object": "chat.completion",
                "choices": [{"message": {"role": "assistant", "content": reply}}],
            },
        )


def wait_for_health(base_url: str, timeout_seconds: float = 90.0) -> None:
    deadline = time.time() + timeout_seconds
    last_error = None
    while time.time() < deadline:
        try:
            status, _, body = http_json("GET", f"{base_url}/health", timeout=2.0)
            if (
                status == 200
                and body.get("status") == "ok"
                and body.get("core_status") == "ok"
            ):
                return
        except Exception as exc:  # noqa: BLE001
            last_error = exc
        time.sleep(0.5)
    raise RuntimeError(f"runtime did not become healthy in time: {last_error}")


def response_memory_contents(payload) -> list[str]:
    if isinstance(payload, list):
        memories = payload
    elif isinstance(payload, dict):
        memories = payload.get("memories", [])
    else:
        memories = []
    return [
        memory.get("content", "")
        for memory in memories
        if isinstance(memory, dict) and isinstance(memory.get("content"), str)
    ]


def poll_access(base_url: str, api_key: str, expected_content: str, timeout_seconds: float = 12.0):
    deadline = time.time() + timeout_seconds
    headers = {"Authorization": f"Bearer {api_key}"}
    url = f"{base_url}/v1/memories"
    last = None
    while time.time() < deadline:
        status, _, body = http_json("GET", url, headers=headers)
        if status == 200:
            last = body
            contents = response_memory_contents(body)
            if any(expected_content in content for content in contents):
                return body
        time.sleep(0.3)
    return last


def write_config(
    path: Path,
    proxy_port: int,
    core_port: int,
    upstream_port: int,
    api_key: str,
    data_dir: Path,
) -> None:
    jwt_secret = "j" * 64
    audit_secret = "a" * 64
    config = f"""# state-machine dryrun config
[server]
host = "127.0.0.1"
port = {proxy_port}
hybrid_mode = true
core_port = {core_port}

[tls]
enabled = false
auto_generate = false

[auth]
jwt_secret = "{jwt_secret}"
audit_hmac_secret = "{audit_secret}"
jwt_expiry_secs = 3600

[[auth.api_keys]]
key = "{api_key}"
role = "admin"
namespace = "default"

[storage]
data_dir = "{data_dir}"

[embeddings]
model = "all-minilm-l6-v2"

[encryption]
provider = "local"

[proxy]
enabled = true
passthrough_auth = true
passthrough_local_only = true
upstream_url = "http://127.0.0.1:{upstream_port}/v1"
upstream_api_key = "stub-upstream-key"
default_memory_mode = "full"
allow_client_memory_control = true
min_recall_score = 0.0
extraction_enabled = true
extract_provider = "openai"
extract_model = "gpt-4o-mini"

[[proxy.key_mapping]]
proxy_key = "{api_key}"
namespace = "default"

[logging]
level = "info"
json = false
"""
    path.write_text(config, encoding="utf-8")


def main() -> int:
    binary = resolve_binary()
    upstream_port = free_port()
    proxy_port = free_port()
    core_port = free_port()
    stub_state = StubState()
    StubHandler.state = stub_state
    stub_server = ThreadingHTTPServer(("127.0.0.1", upstream_port), StubHandler)
    stub_thread = threading.Thread(target=stub_server.serve_forever, daemon=True)
    stub_thread.start()

    with tempfile.TemporaryDirectory(prefix="memoryoss-state-machine-") as temp_dir_str:
        temp_dir = Path(temp_dir_str)
        config_path = temp_dir / "memoryoss.toml"
        data_dir = temp_dir / "data"
        data_dir.mkdir(parents=True, exist_ok=True)
        api_key = "ek_state_machine_dryrun_0123456789abcdef"
        write_config(config_path, proxy_port, core_port, upstream_port, api_key, data_dir)

        log_path = temp_dir / "memoryoss.log"
        log_file = log_path.open("w", encoding="utf-8")
        process = subprocess.Popen(  # noqa: S603
            [binary, "-c", str(config_path), "serve"],
            stdout=log_file,
            stderr=subprocess.STDOUT,
            preexec_fn=os.setsid,
        )

        report = {
            "generated_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
            "binary": binary,
            "config_path": str(config_path),
            "log_path": str(log_path),
            "proxy_base_url": f"http://127.0.0.1:{proxy_port}",
            "upstream_base_url": f"http://127.0.0.1:{upstream_port}/v1",
            "results": [],
        }

        try:
            wait_for_health(report["proxy_base_url"])

            admin_headers = {"Authorization": f"Bearer {api_key}"}
            store_status, _, store_body = http_json(
                "POST",
                f"{report['proxy_base_url']}/v1/store",
                body={"content": SEEDED_MEMORY_CONTENT, "tags": ["state-machine-dryrun", "seed"]},
                headers=admin_headers,
            )
            if store_status != 200:
                raise RuntimeError(f"seed store failed: {store_status} {store_body}")
            time.sleep(1.2)

            mode_specs = [
                {
                    "mode": "full",
                    "headers": {"X-Memory-Mode": "full"},
                    "expected_injected": True,
                    "expected_store": True,
                },
                {
                    "mode": "readonly",
                    "headers": {"X-Memory-Mode": "readonly"},
                    "expected_injected": True,
                    "expected_store": False,
                },
                {
                    "mode": "after",
                    "headers": {
                        "X-Memory-Mode": "after",
                        "X-Memory-After": "2099-01-01",
                    },
                    "expected_injected": False,
                    "expected_store": True,
                },
                {
                    "mode": "off",
                    "headers": {"X-Memory-Mode": "off"},
                    "expected_injected": False,
                    "expected_store": False,
                },
            ]

            for spec in mode_specs:
                mode = spec["mode"]
                expected_fact = (
                    f"State-machine dryrun extracted fact for DRYRUN_MODE_{mode} "
                    "on the local proxy harness."
                )
                before_calls = len(stub_state.normal_calls)
                before_extraction_calls = len(stub_state.extraction_calls)
                status, response_headers, response_body = http_json(
                    "POST",
                    f"{report['proxy_base_url']}/proxy/v1/chat/completions",
                    body={
                        "model": "stub-model",
                        "messages": [
                            {
                                "role": "user",
                                "content": (
                                    f"Please answer for DRYRUN_MODE_{mode}. "
                                    f"Use the phrase {SEEDED_SIGNAL} if recall is active."
                                ),
                            }
                        ],
                    },
                    headers={"Authorization": f"Bearer {api_key}", **spec["headers"]},
                    timeout=15.0,
                )
                if status != 200:
                    raise RuntimeError(f"proxy call for {mode} failed: {status} {response_body}")

                injected_count = int(response_headers.get("x-memory-injected-count", "0"))
                stub_payload = stub_state.normal_calls[before_calls]["payload"]
                upstream_payload_text = json.dumps(stub_payload)
                upstream_saw_seed = "<memory_context>" in upstream_payload_text
                recall_result = poll_access(
                    report["proxy_base_url"], api_key, expected_fact
                )
                new_extraction_calls = stub_state.extraction_calls[before_extraction_calls:]
                extraction_call_seen = any(call.get("mode") == mode for call in new_extraction_calls)
                recalled_contents = response_memory_contents(recall_result)
                stored = any(expected_fact in content for content in recalled_contents)

                passed = (
                    (injected_count > 0) == bool(spec["expected_injected"])
                    and upstream_saw_seed == bool(spec["expected_injected"])
                    and stored == bool(spec["expected_store"])
                )
                report["results"].append(
                    {
                        "mode": mode,
                        "proxy_status": status,
                        "injected_count": injected_count,
                        "expected_injected": spec["expected_injected"],
                        "upstream_saw_memory_context": upstream_saw_seed,
                        "extraction_call_seen": extraction_call_seen,
                        "new_extraction_call_count": len(new_extraction_calls),
                        "expected_store": spec["expected_store"],
                        "stored_extracted_fact": stored,
                        "expected_fact": expected_fact,
                        "recalled_contents": recalled_contents[:5],
                        "upstream_payload_preview": upstream_payload_text[:500],
                        "response_preview": json.dumps(response_body)[:220],
                        "passed": passed,
                    }
                )

            report["summary"] = {
                "total": len(report["results"]),
                "passed": sum(1 for item in report["results"] if item["passed"]),
                "failed": sum(1 for item in report["results"] if not item["passed"]),
            }
        finally:
            try:
                os.killpg(os.getpgid(process.pid), signal.SIGTERM)
            except ProcessLookupError:
                pass
            process.wait(timeout=15)
            log_file.close()
            stub_server.shutdown()
            stub_server.server_close()

        if log_path.exists():
            report["log_tail"] = log_path.read_text(encoding="utf-8", errors="replace").splitlines()[
                -80:
            ]
        REPORT_PATH.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
        print(json.dumps(report["summary"], indent=2))
        print(f"report: {REPORT_PATH}")
        return 0 if report["summary"]["failed"] == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
