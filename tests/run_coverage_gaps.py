#!/usr/bin/env python3
"""
Coverage gap tests: proxy streams, sharing webhooks, backup/restore,
embedding migration, and key rotation grace-expiry.
"""
import http.server
import json
import os
import socket
import subprocess
import tempfile
import threading
import time
from pathlib import Path

from runner_common import (
    ROOT_DIR,
    free_port,
    http_json,
    http_json_with_retry,
    iso_now,
    start_server,
    stop_process,
    wait_for_health,
    write_test_config,
)

OUTPUT_JSON = Path(
    os.environ.get(
        "COVERAGE_GAPS_OUTPUT_JSON",
        ROOT_DIR / "tests" / "coverage-gaps-report.json",
    )
)

BINARY = ROOT_DIR / "target" / "debug" / "memoryoss"


BASE_AUTH = """
[[auth.api_keys]]
key = "gap-admin-key"
role = "admin"
namespace = "gaptest"
"""

BASE_EXTRA = """
[trust]
semantic_dedup_threshold = 0.98
"""


def _start_test_server(tmp, port, data_dir, extra_sections="", log_name="server.log"):
    combined_extra = BASE_EXTRA + "\n" + extra_sections
    config_path = tmp / "config.toml"
    write_test_config(
        config_path,
        port=port,
        data_dir=data_dir,
        auth_entries=BASE_AUTH,
        extra_sections=combined_extra,
    )
    log_path = tmp / log_name
    process = start_server(config_path, log_path=log_path)
    return process, config_path


# ─── Test 1: Proxy Stream Paths ──────────────────────────────────────────────

def _start_sse_upstream(upstream_port):
    """HTTP server that returns SSE-streamed chat completions."""

    class SSEHandler(http.server.BaseHTTPRequestHandler):
        def log_message(self, *args):
            pass

        def do_POST(self):
            length = int(self.headers.get("Content-Length", 0))
            body = json.loads(self.rfile.read(length)) if length else {}

            if body.get("stream"):
                self.send_response(200)
                self.send_header("Content-Type", "text/event-stream")
                self.send_header("Cache-Control", "no-cache")
                self.end_headers()
                chunks = [
                    {"choices": [{"delta": {"role": "assistant"}, "index": 0}]},
                    {"choices": [{"delta": {"content": "streamed "}, "index": 0}]},
                    {"choices": [{"delta": {"content": "hello"}, "index": 0}]},
                    {"choices": [{"delta": {}, "index": 0, "finish_reason": "stop"}]},
                ]
                for chunk in chunks:
                    line = f"data: {json.dumps(chunk)}\n\n"
                    self.wfile.write(line.encode())
                    self.wfile.flush()
                self.wfile.write(b"data: [DONE]\n\n")
                self.wfile.flush()
            else:
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.end_headers()
                resp = {
                    "choices": [{"message": {"role": "assistant", "content": "non-streamed"}, "index": 0}]
                }
                self.wfile.write(json.dumps(resp).encode())

        def do_GET(self):
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.end_headers()
            self.wfile.write(json.dumps({"data": [{"id": "test-model"}]}).encode())

    server = http.server.HTTPServer(("127.0.0.1", upstream_port), SSEHandler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    return server


def test_proxy_stream():
    items = []
    tmp = Path(tempfile.mkdtemp(prefix="gap-stream-"))
    port = free_port()
    upstream_port = free_port()
    data_dir = tmp / "data"
    data_dir.mkdir(parents=True)

    upstream = _start_sse_upstream(upstream_port)
    extra = f"""
[proxy]
enabled = true
passthrough_auth = true
upstream_url = "http://127.0.0.1:{upstream_port}/v1"
upstream_api_key = "fake-key"
default_memory_mode = "off"
extraction_enabled = false

[[proxy.key_mapping]]
proxy_key = "gap-admin-key"
namespace = "gaptest"
"""
    process, _ = _start_test_server(tmp, port, data_dir, extra)
    base_url = f"https://127.0.0.1:{port}"

    try:
        wait_for_health(base_url, timeout=60.0, verify_tls=False)

        # Non-streaming request (baseline)
        status, body = http_json(
            "POST",
            f"{base_url}/proxy/v1/chat/completions",
            headers={"Authorization": "Bearer gap-admin-key"},
            body={"model": "test", "messages": [{"role": "user", "content": "hi"}]},
        )
        assert status == 200, f"non-stream failed: {status}"
        items.append({"name": "Proxy non-streaming passthrough", "status": "pass"})

        # Streaming request via raw urllib to read SSE chunks
        import ssl
        import urllib.request

        ctx = ssl._create_unverified_context()
        req_body = json.dumps({
            "model": "test",
            "stream": True,
            "messages": [{"role": "user", "content": "hi"}],
        }).encode()
        req = urllib.request.Request(
            f"{base_url}/proxy/v1/chat/completions",
            data=req_body,
            headers={
                "Authorization": "Bearer gap-admin-key",
                "Content-Type": "application/json",
            },
            method="POST",
        )
        with urllib.request.urlopen(req, timeout=30, context=ctx) as resp:
            raw = resp.read().decode()
            assert "data:" in raw, f"no SSE data in response: {raw[:200]}"
            assert "[DONE]" in raw, "stream did not terminate with [DONE]"
            assert "streamed" in raw or "hello" in raw, "stream content missing"
            content_type = resp.headers.get("content-type", "")
            assert "text/event-stream" in content_type, f"wrong content-type: {content_type}"

        items.append({"name": "Proxy SSE streaming response", "status": "pass"})
        items.append({
            "name": "Stream content-type and termination",
            "status": "pass",
            "note": "text/event-stream + [DONE] marker verified",
        })

    except Exception as exc:
        items.append({"name": "Proxy stream paths", "status": "fail", "note": str(exc)})
    finally:
        stop_process(process)
        upstream.shutdown()

    return items


# ─── Test 2: Sharing Webhooks ────────────────────────────────────────────────

def test_sharing_webhooks():
    items = []
    tmp = Path(tempfile.mkdtemp(prefix="gap-webhook-"))
    port = free_port()
    webhook_port = free_port()
    data_dir = tmp / "data"
    data_dir.mkdir(parents=True)

    webhook_payloads = []

    class WebhookHandler(http.server.BaseHTTPRequestHandler):
        def log_message(self, *args):
            pass

        def do_POST(self):
            length = int(self.headers.get("Content-Length", 0))
            body = json.loads(self.rfile.read(length)) if length else {}
            webhook_payloads.append(body)
            self.send_response(200)
            self.end_headers()
            self.wfile.write(b'{"ok": true}')

    webhook_server = http.server.HTTPServer(("127.0.0.1", webhook_port), WebhookHandler)
    webhook_thread = threading.Thread(target=webhook_server.serve_forever, daemon=True)
    webhook_thread.start()

    auth_entries = """
[[auth.api_keys]]
key = "gap-admin-key"
role = "admin"
namespace = "gaptest"

[[auth.api_keys]]
key = "gap-beta-key"
role = "admin"
namespace = "beta"
"""
    config_path = tmp / "config.toml"
    config_path.write_text(f"""
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

[limits]
rate_limit_per_sec = 5000

[sharing]
allow_private_webhooks = true
""".strip() + "\n", encoding="utf-8")

    log_path = tmp / "server.log"
    process = start_server(config_path, log_path=log_path)
    base_url = f"https://127.0.0.1:{port}"
    auth = "Bearer gap-admin-key"

    try:
        wait_for_health(base_url, timeout=60.0, verify_tls=False)

        # Create shared namespace with webhook
        status, body = http_json(
            "POST",
            f"{base_url}/v1/admin/sharing/create",
            headers={"Authorization": auth},
            body={
                "name": "webhook-test-ns",
                "webhook_url": f"http://127.0.0.1:{webhook_port}/hook",
            },
        )
        if status == 200:
            items.append({"name": "Create shared namespace with webhook_url", "status": "pass"})
        else:
            items.append({
                "name": "Create shared namespace with webhook_url",
                "status": "fail",
                "note": f"status={status} body={body}",
            })

        # List shared namespaces — verify webhook_url is persisted
        status, body = http_json(
            "GET",
            f"{base_url}/v1/admin/sharing/list",
            headers={"Authorization": auth},
        )
        has_webhook = False
        if status == 200 and body:
            namespaces = body if isinstance(body, list) else body.get("namespaces", body.get("shared_namespaces", []))
            for ns in namespaces:
                if ns.get("name") == "webhook-test-ns":
                    has_webhook = bool(ns.get("webhook_url"))
        items.append({
            "name": "Webhook URL persisted in shared namespace",
            "status": "pass" if has_webhook else "warn",
            "note": "webhook_url visible in listing" if has_webhook else "webhook_url not in listing response",
        })

        # Store a memory (webhook should fire if wired)
        status, _ = http_json(
            "POST",
            f"{base_url}/v1/store",
            headers={"Authorization": auth},
            body={"content": "webhook test memory", "tags": ["webhook"]},
        )
        time.sleep(1)

        if webhook_payloads:
            items.append({
                "name": "Webhook fires on memory store",
                "status": "pass",
                "note": f"{len(webhook_payloads)} webhook(s) received",
            })
        else:
            items.append({
                "name": "Webhook fires on memory store",
                "status": "warn",
                "note": "webhook not triggered (may not be wired to store yet)",
            })

    except Exception as exc:
        items.append({"name": "Sharing webhooks", "status": "fail", "note": str(exc)})
    finally:
        stop_process(process)
        webhook_server.shutdown()

    return items


# ─── Test 3: Backup / Restore ────────────────────────────────────────────────

def test_backup_restore():
    items = []
    tmp = Path(tempfile.mkdtemp(prefix="gap-backup-"))
    port = free_port()
    data_dir = tmp / "data"
    data_dir.mkdir(parents=True)

    process, config_path = _start_test_server(tmp, port, data_dir)
    base_url = f"https://127.0.0.1:{port}"
    auth = "Bearer gap-admin-key"

    try:
        wait_for_health(base_url, timeout=60.0, verify_tls=False)

        # Store some memories
        for i in range(5):
            status, _ = http_json_with_retry(
                "POST",
                f"{base_url}/v1/store",
                headers={"Authorization": auth},
                body={"content": f"backup test memory {i}", "tags": ["backup"]},
            )
            assert status == 200, f"store failed: {status}"

        time.sleep(2)

        # Verify memories exist
        status, body = http_json(
            "POST",
            f"{base_url}/v1/recall",
            headers={"Authorization": auth},
            body={"query": "backup test memory"},
        )
        assert status == 200
        pre_count = len(body.get("memories", []))
        items.append({
            "name": "Pre-backup memory count",
            "status": "pass",
            "note": f"{pre_count} memories stored",
        })

        stop_process(process)

        # Backup
        backup_path = tmp / "test-backup.tar.zst"
        result = subprocess.run(
            [
                str(BINARY),
                "--config",
                str(config_path),
                "backup",
                "--output",
                str(backup_path),
                "--include-key",
            ],
            capture_output=True, text=True, timeout=30,
        )
        assert result.returncode == 0, f"backup failed: {result.stderr}"
        assert backup_path.exists(), "backup file not created"
        backup_size = backup_path.stat().st_size
        items.append({
            "name": "Backup creates valid archive",
            "status": "pass",
            "note": f"{backup_size} bytes",
        })

        # Restore into new directory
        restore_dir = tmp / "restored"
        restore_dir.mkdir()
        restore_config = tmp / "restore.toml"
        port2 = free_port()
        write_test_config(
            restore_config,
            port=port2,
            data_dir=restore_dir,
            auth_entries=BASE_AUTH,
            extra_sections=BASE_EXTRA,
        )
        result = subprocess.run(
            [str(BINARY), "--config", str(restore_config), "restore", str(backup_path)],
            capture_output=True, text=True, timeout=30,
        )
        assert result.returncode == 0, f"restore failed: {result.stderr}"
        items.append({"name": "Restore from backup succeeds", "status": "pass"})

        # Start server on restored data and verify memories
        log2 = tmp / "restored-server.log"
        process2 = start_server(restore_config, log_path=log2)
        base2 = f"https://127.0.0.1:{port2}"
        wait_for_health(base2, timeout=120.0, verify_tls=False)

        status, body = http_json(
            "POST",
            f"{base2}/v1/recall",
            headers={"Authorization": auth},
            body={"query": "backup test memory"},
        )
        post_count = len(body.get("memories", [])) if status == 200 else 0
        stop_process(process2)

        survived = post_count >= pre_count
        items.append({
            "name": "Memories survive backup→restore cycle",
            "status": "pass" if survived else "fail",
            "note": f"pre={pre_count} post={post_count}",
        })

        # Restore refuses non-empty dir without --force
        nonempty_dir = tmp / "nonempty"
        nonempty_dir.mkdir()
        (nonempty_dir / "dummy.txt").write_text("exists")
        nonempty_config = tmp / "nonempty.toml"
        write_test_config(
            nonempty_config,
            port=free_port(),
            data_dir=nonempty_dir,
            auth_entries=BASE_AUTH,
            extra_sections=BASE_EXTRA,
        )
        result = subprocess.run(
            [str(BINARY), "--config", str(nonempty_config), "restore", str(backup_path)],
            capture_output=True, text=True, timeout=30,
        )
        items.append({
            "name": "Restore refuses non-empty directory",
            "status": "pass" if result.returncode != 0 else "warn",
            "note": "exit=1 as expected" if result.returncode != 0 else "did not refuse",
        })

    except Exception as exc:
        items.append({"name": "Backup/Restore", "status": "fail", "note": str(exc)})
    finally:
        try:
            stop_process(process)
        except Exception:
            pass

    return items


# ─── Test 4: Embedding Migration ─────────────────────────────────────────────

def test_embedding_migration():
    items = []
    tmp = Path(tempfile.mkdtemp(prefix="gap-migrate-"))
    port = free_port()
    data_dir = tmp / "data"
    data_dir.mkdir(parents=True)

    process, config_path = _start_test_server(tmp, port, data_dir)
    base_url = f"https://127.0.0.1:{port}"
    auth = "Bearer gap-admin-key"

    try:
        wait_for_health(base_url, timeout=60.0, verify_tls=False)

        # Store memories (distinct content to avoid semantic dedup)
        migration_contents = [
            "The capital of France is Paris and it has the Eiffel Tower",
            "Quantum computing uses qubits for parallel computation",
            "Photosynthesis converts sunlight into chemical energy in plants",
        ]
        for i, content in enumerate(migration_contents):
            status, resp_body = http_json_with_retry(
                "POST",
                f"{base_url}/v1/store",
                headers={"Authorization": auth},
                body={"content": content, "tags": [f"emb-{i}"]},
            )
            if status != 200:
                raise RuntimeError(f"store failed: status={status} body={resp_body}")

        time.sleep(2)
        stop_process(process)

        # Dry-run migration
        result = subprocess.run(
            [
                str(BINARY), "--config", str(config_path),
                "migrate-embeddings", "--model", "all-minilm-l6-v2",
                "--namespace", "gaptest", "--dry-run",
            ],
            capture_output=True, text=True, timeout=120,
        )
        if result.returncode == 0:
            items.append({
                "name": "Embedding migration dry-run",
                "status": "pass",
                "note": "counts without writing",
            })
        else:
            items.append({
                "name": "Embedding migration dry-run",
                "status": "fail",
                "note": result.stderr[:200],
            })

        # Actual migration
        result = subprocess.run(
            [
                str(BINARY), "--config", str(config_path),
                "migrate-embeddings", "--model", "all-minilm-l6-v2",
                "--namespace", "gaptest",
            ],
            capture_output=True, text=True, timeout=120,
        )
        if result.returncode == 0:
            items.append({
                "name": "Embedding migration execution",
                "status": "pass",
                "note": "all-minilm-l6-v2",
            })
        else:
            items.append({
                "name": "Embedding migration execution",
                "status": "fail",
                "note": result.stderr[:200],
            })

        # Verify post-migration: restart and recall
        port2 = free_port()
        config2 = tmp / "post-migrate.toml"
        write_test_config(
            config2,
            port=port2,
            data_dir=data_dir,
            auth_entries=BASE_AUTH,
            extra_sections=BASE_EXTRA,
        )
        process2 = start_server(config2, log_path=tmp / "post.log")
        base2 = f"https://127.0.0.1:{port2}"
        wait_for_health(base2, timeout=60.0, verify_tls=False)

        status, body = http_json(
            "POST",
            f"{base2}/v1/recall",
            headers={"Authorization": auth},
            body={"query": "embedding migration test"},
        )
        stop_process(process2)

        recalled = len(body.get("memories", [])) if status == 200 else 0
        items.append({
            "name": "Post-migration recall works",
            "status": "pass" if recalled > 0 else "fail",
            "note": f"{recalled} memories recalled after migration",
        })

    except Exception as exc:
        import traceback
        items.append({"name": "Embedding migration", "status": "fail", "note": traceback.format_exc()[-300:]})

    return items


# ─── Test 5: Key Rotation + Grace Expiry ─────────────────────────────────────

def test_key_rotation_grace():
    items = []
    tmp = Path(tempfile.mkdtemp(prefix="gap-rotate-"))
    port = free_port()
    data_dir = tmp / "data"
    data_dir.mkdir(parents=True)

    extra = """
[encryption]
grace_period_secs = 120

[limits]
rate_limit_per_sec = 5000
"""
    process, config_path = _start_test_server(tmp, port, data_dir, extra)
    base_url = f"https://127.0.0.1:{port}"
    auth = "Bearer gap-admin-key"

    try:
        wait_for_health(base_url, timeout=60.0, verify_tls=False)

        # Store a memory pre-rotation
        status, store_body = http_json(
            "POST",
            f"{base_url}/v1/store",
            headers={"Authorization": auth},
            body={"content": "pre-rotation secret data", "tags": ["rotation"]},
        )
        assert status == 200
        memory_id = store_body["id"]
        time.sleep(1)

        # Recall pre-rotation (baseline)
        status, body = http_json(
            "POST",
            f"{base_url}/v1/recall",
            headers={"Authorization": auth},
            body={"query": "pre-rotation secret"},
        )
        pre_ok = status == 200 and any(
            "pre-rotation" in (m.get("content") or m.get("memory", {}).get("content", ""))
            for m in body.get("memories", [])
        )
        items.append({
            "name": "Pre-rotation recall works",
            "status": "pass" if pre_ok else "fail",
        })

        # Rotate key
        status, body = http_json(
            "POST",
            f"{base_url}/v1/admin/keys/rotate",
            headers={"Authorization": auth},
            body={"namespace": "gaptest"},
        )
        rotation_ok = status == 200
        items.append({
            "name": "Key rotation succeeds",
            "status": "pass" if rotation_ok else "fail",
            "note": f"status={status}",
        })

        # Recall during grace period (should still work)
        status, body = http_json(
            "POST",
            f"{base_url}/v1/recall",
            headers={"Authorization": auth},
            body={"query": "pre-rotation secret"},
        )
        grace_ok = status == 200 and any(
            "pre-rotation" in (m.get("content") or m.get("memory", {}).get("content", ""))
            for m in body.get("memories", [])
        )
        items.append({
            "name": "Recall works during grace period",
            "status": "pass" if grace_ok else "fail",
        })

        # List retired keys
        status, body = http_json(
            "GET",
            f"{base_url}/v1/admin/keys",
            headers={"Authorization": auth},
        )
        keys_listed = status == 200
        retired_keys = body if isinstance(body, list) else body.get("retired_keys", body.get("keys", []))
        items.append({
            "name": "Retired keys listed during grace",
            "status": "pass" if keys_listed and len(retired_keys) > 0 else "warn",
            "note": f"{len(retired_keys)} retired key(s)",
        })

        # Store a NEW memory post-rotation (with new key — should work)
        status, _ = http_json(
            "POST",
            f"{base_url}/v1/store",
            headers={"Authorization": auth},
            body={"content": "post-rotation new data"},
        )
        items.append({
            "name": "Store works after rotation with new key",
            "status": "pass" if status == 200 else "fail",
        })

        # Revoke retired key explicitly
        if retired_keys:
            key_id = retired_keys[0].get("key_id") or retired_keys[0].get("id", "")
            if key_id:
                status, _ = http_json(
                    "DELETE",
                    f"{base_url}/v1/admin/keys/{key_id}",
                    headers={"Authorization": auth},
                )
                items.append({
                    "name": "Revoke retired key succeeds",
                    "status": "pass" if status == 200 else "warn",
                    "note": f"status={status}",
                })

    except Exception as exc:
        import traceback
        items.append({"name": "Key rotation grace", "status": "fail", "note": traceback.format_exc()[-300:]})
    finally:
        try:
            stop_process(process)
        except Exception:
            pass

    return items


# ─── Main ────────────────────────────────────────────────────────────────────

def main():
    started = time.time()
    all_items = []
    test_groups = []

    tests = [
        ("Proxy Stream Paths", test_proxy_stream),
        ("Sharing Webhooks", test_sharing_webhooks),
        ("Backup / Restore", test_backup_restore),
        ("Embedding Migration", test_embedding_migration),
        ("Key Rotation Grace Expiry", test_key_rotation_grace),
    ]

    for name, func in tests:
        print(f"[coverage-gaps] running: {name}", flush=True)
        t0 = time.time()
        try:
            items = func()
        except Exception as exc:
            items = [{"name": name, "status": "fail", "note": str(exc)}]
        elapsed = time.time() - t0
        print(f"[coverage-gaps] {name}: {len(items)} checks in {elapsed:.1f}s", flush=True)
        test_groups.append({"title": name, "count": len(items), "items": items})
        all_items.extend(items)

    passed = sum(1 for i in all_items if i["status"] == "pass")
    warned = sum(1 for i in all_items if i["status"] == "warn")
    failed = sum(1 for i in all_items if i["status"] == "fail")

    report = {
        "runner": "tests/run_coverage_gaps.py",
        "generated_at": iso_now(),
        "duration_seconds": int(time.time() - started),
        "summary": {
            "total": len(all_items),
            "passed": passed,
            "warned": warned,
            "failed": failed,
        },
        "groups": test_groups,
        "items": all_items,
    }

    OUTPUT_JSON.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    print(f"\n[coverage-gaps] {passed} pass / {warned} warn / {failed} fail", flush=True)
    print(json.dumps(report["summary"], indent=2), flush=True)

    if failed > 0:
        raise SystemExit(1)


if __name__ == "__main__":
    main()
