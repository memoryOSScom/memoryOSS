#!/usr/bin/env python3
import json
import http.client
import os
import socket
import ssl
import subprocess
import time
import urllib.error
import urllib.request
from pathlib import Path


ROOT_DIR = Path(__file__).resolve().parent.parent
DEFAULT_BINARY = ROOT_DIR / "target" / "debug" / "memoryoss"


def ensure_binary() -> Path:
    if DEFAULT_BINARY.exists():
        return DEFAULT_BINARY
    subprocess.run(["cargo", "build"], cwd=ROOT_DIR, check=True)
    return DEFAULT_BINARY


def free_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def wait_for_port(port: int, timeout: float = 30.0) -> None:
    deadline = time.time() + timeout
    while time.time() < deadline:
        try:
            with socket.create_connection(("127.0.0.1", port), timeout=0.5):
                return
        except OSError:
            time.sleep(0.25)
    raise TimeoutError(f"port {port} did not become reachable within {timeout}s")


def http_json(
    method: str,
    url: str,
    *,
    headers: dict | None = None,
    body: dict | list | None = None,
    timeout: float = 60.0,
    verify_tls: bool = False,
):
    payload = None
    request_headers = dict(headers or {})
    if body is not None:
        payload = json.dumps(body).encode("utf-8")
        request_headers.setdefault("Content-Type", "application/json")

    request = urllib.request.Request(url, data=payload, headers=request_headers, method=method)
    context = None
    if url.startswith("https://") and not verify_tls:
        context = ssl._create_unverified_context()

    try:
        with urllib.request.urlopen(request, timeout=timeout, context=context) as response:
            raw = response.read()
            return response.status, json.loads(raw.decode("utf-8")) if raw else None
    except urllib.error.HTTPError as exc:
        raw = exc.read()
        body_json = None
        if raw:
            try:
                body_json = json.loads(raw.decode("utf-8"))
            except Exception:
                body_json = {"raw": raw.decode("utf-8", errors="replace")}
        return exc.code, body_json


def http_json_with_retry(
    method: str,
    url: str,
    *,
    headers: dict | None = None,
    body: dict | list | None = None,
    timeout: float = 60.0,
    verify_tls: bool = False,
    max_attempts: int = 8,
):
    last_status = None
    last_body = None
    last_error = None
    for attempt in range(max_attempts):
        try:
            status, response_body = http_json(
                method,
                url,
                headers=headers,
                body=body,
                timeout=timeout,
                verify_tls=verify_tls,
            )
        except (
            urllib.error.URLError,
            http.client.RemoteDisconnected,
            TimeoutError,
            OSError,
            ssl.SSLError,
        ) as exc:
            last_error = exc
            if attempt == max_attempts - 1:
                break
            time.sleep(min(0.25 * (attempt + 1), 3.0))
            continue
        last_status = status
        last_body = response_body
        if status != 429:
            return status, response_body
        retry_after_ms = 250
        if isinstance(response_body, dict):
            retry_after_ms = int(response_body.get("retry_after_ms", retry_after_ms))
        time.sleep(min(max(retry_after_ms / 1000.0, 0.05), 3.0))
        if attempt == max_attempts - 1:
            break
    if last_error is not None and last_status is None:
        raise RuntimeError(f"request failed after {max_attempts} attempts: {last_error}")
    return last_status, last_body


def wait_for_health(base_url: str, timeout: float = 45.0, verify_tls: bool = False) -> None:
    deadline = time.time() + timeout
    last_error = None
    while time.time() < deadline:
        try:
            status, body = http_json(
                "GET",
                f"{base_url}/health",
                timeout=3.0,
                verify_tls=verify_tls,
            )
            if status == 200 and body and body.get("status") == "ok":
                return
        except Exception as exc:  # pragma: no cover - helper path
            last_error = exc
        time.sleep(0.5)
    if last_error:
        raise TimeoutError(f"health check failed: {last_error}")
    raise TimeoutError(f"{base_url}/health did not become ready within {timeout}s")


def write_test_config(
    path: Path,
    *,
    port: int,
    data_dir: Path,
    auth_entries: str,
    extra_sections: str = "",
) -> None:
    path.write_text(
        f"""
[server]
host = "127.0.0.1"
port = {port}

[tls]
enabled = true
auto_generate = true

[storage]
data_dir = "{data_dir}"

[auth]
jwt_secret = "test-secret-that-is-at-least-32-characters-long"

{auth_entries}

[logging]
level = "warn"

{extra_sections}
""".strip()
        + "\n",
        encoding="utf-8",
    )


def start_server(config_path: Path, *, log_path: Path | None = None):
    binary = ensure_binary()
    if log_path is None:
        log_file = subprocess.DEVNULL
        stderr_file = subprocess.DEVNULL
    else:
        log_path.parent.mkdir(parents=True, exist_ok=True)
        handle = log_path.open("wb")
        log_file = handle
        stderr_file = handle

    process = subprocess.Popen(
        [str(binary), "--config", str(config_path), "serve"],
        cwd=ROOT_DIR,
        stdout=log_file,
        stderr=stderr_file,
    )
    return process


def stop_process(process: subprocess.Popen) -> None:
    if process.poll() is not None:
        return
    process.terminate()
    try:
        process.wait(timeout=8)
    except subprocess.TimeoutExpired:
        process.kill()
        process.wait(timeout=8)


def wait_for_indexer_sync(base_url: str, auth_header: str, timeout: float = 120.0) -> dict:
    deadline = time.time() + timeout
    headers = {"Authorization": auth_header}
    last = None
    while time.time() < deadline:
        status, body = http_json(
            "GET",
            f"{base_url}/v1/admin/index-health",
            headers=headers,
            timeout=10.0,
        )
        last = body
        if status == 200 and body and body.get("indexer_lag") == 0:
            return body
        time.sleep(0.5)
    raise TimeoutError(f"indexer did not catch up within {timeout}s; last={last}")


def get_index_health(base_url: str, auth_header: str) -> tuple[int, dict | None]:
    return http_json(
        "GET",
        f"{base_url}/v1/admin/index-health",
        headers={"Authorization": auth_header},
        timeout=15.0,
    )


def wait_for_indexer_lag_below(
    base_url: str,
    auth_header: str,
    *,
    target_lag: int,
    timeout: float = 180.0,
) -> dict:
    deadline = time.time() + timeout
    last = None
    while time.time() < deadline:
        status, body = get_index_health(base_url, auth_header)
        last = body
        if status == 200 and body and int(body.get("indexer_lag", 0)) <= target_lag:
            return body
        time.sleep(0.5)
    raise TimeoutError(
        f"indexer lag did not fall below {target_lag} within {timeout}s; last={last}"
    )


def percentile(values: list[float], p: float) -> float:
    if not values:
        return 0.0
    sorted_values = sorted(values)
    if len(sorted_values) == 1:
        return float(sorted_values[0])
    rank = (len(sorted_values) - 1) * p
    lower = int(rank)
    upper = min(lower + 1, len(sorted_values) - 1)
    weight = rank - lower
    return float(sorted_values[lower] * (1.0 - weight) + sorted_values[upper] * weight)


def iso_now() -> str:
    return time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
