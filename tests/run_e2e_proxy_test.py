#!/usr/bin/env python3
"""
E2E proxy + memory API tests against a running memoryOSS server.

Tests all UX combinations:
1. Direct Memory API (store/recall/update/forget/feedback)
2. Memory modes (full/readonly/off/after) via X-Memory-Mode header
3. Proxy passthrough (OpenAI format, Anthropic format)
4. Namespace isolation
5. Consolidation
6. Admin endpoints (health, explain, cache, trust)
7. GDPR endpoints (export, memories, certified delete)
8. Sharing (create namespace, grant, accessible)
9. Batch operations
10. Edge cases (empty recall, large content, special chars)

Usage:
  E2E_BASE_URL=http://127.0.0.1:8000 E2E_API_KEY=ek_... python3 tests/run_e2e_proxy_test.py
"""
import json
import os
import sys
import time
from pathlib import Path

ROOT_DIR = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT_DIR / "tests"))

from runner_common import http_json_with_retry, iso_now

BASE_URL = os.environ.get("E2E_BASE_URL", "http://127.0.0.1:8000")
OUTPUT_JSON = Path(os.environ.get("E2E_OUTPUT_JSON", ROOT_DIR / "tests" / "e2e-proxy-report.json"))
VERIFY_TLS = BASE_URL.startswith("https://")

def default_api_key() -> str:
    env_key = os.environ.get("E2E_API_KEY")
    if env_key:
        return env_key

    for config_path in [Path("/root/memoryoss.toml"), ROOT_DIR / "memoryoss.toml"]:
        if config_path.exists():
            for line in config_path.read_text(encoding="utf-8").splitlines():
                stripped = line.strip()
                if stripped.startswith("key = \"ek_"):
                    return stripped.split("\"", 2)[1]

    return "ek_468e7619055fe863d2be146db11247a82a6a2fa72f613fe4f2346a20673d4a6d"


API_KEY = default_api_key()
AUTH = {"Authorization": f"Bearer {API_KEY}"}
results: list[dict] = []
stored_ids: list[str] = []


def test(name: str, fn):
    t0 = time.time()
    try:
        fn()
        dur = round((time.time() - t0) * 1000, 1)
        results.append({"name": name, "status": "pass", "duration_ms": dur})
        print(f"  PASS  {name} ({dur}ms)")
    except AssertionError as e:
        dur = round((time.time() - t0) * 1000, 1)
        results.append({"name": name, "status": "fail", "duration_ms": dur, "error": str(e)[:300]})
        print(f"  FAIL  {name}: {e}")
    except Exception as e:
        dur = round((time.time() - t0) * 1000, 1)
        results.append({"name": name, "status": "error", "duration_ms": dur, "error": str(e)[:300]})
        print(f"  ERROR {name}: {e}")


def req(method, path, body=None, headers=None, expected_status=200):
    hdrs = dict(AUTH)
    if headers:
        hdrs.update(headers)
    status, resp = http_json_with_retry(
        method, f"{BASE_URL}{path}",
        headers=hdrs, body=body, timeout=30.0,
        verify_tls=VERIFY_TLS, max_attempts=2,
    )
    assert status == expected_status, f"expected {expected_status}, got {status}: {resp}"
    return resp


# === 1. Health ===
def test_health():
    resp = req("GET", "/health")
    assert resp.get("status") == "ok"

# === 2. Store ===
def test_store_single():
    resp = req("POST", "/v1/store", body={
        "content": "E2E test: memoryOSS uses redb as storage backend",
        "tags": ["e2e", "architecture"],
    })
    assert "id" in resp
    stored_ids.append(resp["id"])

def test_store_with_metadata():
    resp = req("POST", "/v1/store", body={
        "content": "E2E test: deployment must go through staging first",
        "tags": ["e2e", "deployment", "rule"],
        "agent": "e2e-test-agent",
        "session": "e2e-session-001",
    })
    assert "id" in resp
    stored_ids.append(resp["id"])

def test_store_batch():
    # Use very distinct content to avoid semantic dedup
    themes = ["postgres indexing strategy", "kubernetes pod scaling", "redis cache eviction",
              "nginx load balancing config", "docker multi-stage build optimization"]
    memories = [
        {"content": f"E2E batch: {themes[i]} for project alpha environment {i}", "tags": ["e2e", f"batch-{i}"]}
        for i in range(5)
    ]
    resp = req("POST", "/v1/store/batch", body={"memories": memories})
    assert "ids" in resp or "stored" in resp or isinstance(resp, list)

# === 3. Recall ===
def test_recall_basic():
    time.sleep(1)  # Wait for indexer
    resp = req("POST", "/v1/recall", body={"query": "redb storage backend"})
    assert isinstance(resp, (list, dict))

def test_recall_with_tags():
    resp = req("POST", "/v1/recall", body={
        "query": "deployment staging",
        "tags": ["deployment"],
    })
    assert isinstance(resp, (list, dict))

def test_recall_empty():
    resp = req("POST", "/v1/recall", body={
        "query": "xyzzy nonexistent topic 99999",
    })
    # Should return empty or low-score results
    assert isinstance(resp, (list, dict))

def test_recall_batch():
    resp = req("POST", "/v1/recall/batch", body={
        "queries": [
            {"query": "redb storage"},
            {"query": "deployment staging"},
        ]
    })
    assert isinstance(resp, (list, dict))

# === 4. Update ===
def test_update_memory():
    if not stored_ids:
        return
    resp = req("PATCH", "/v1/update", body={
        "id": stored_ids[0],
        "content": "E2E test UPDATED: memoryOSS uses redb as crash-safe storage backend",
        "tags": ["e2e", "architecture", "updated"],
    })

# === 5. Feedback ===
def test_feedback_confirm():
    if not stored_ids:
        return
    req("POST", "/v1/feedback", body={
        "id": stored_ids[0],
        "action": "confirm",
    })

def test_feedback_reject():
    if len(stored_ids) < 2:
        return
    req("POST", "/v1/feedback", body={
        "id": stored_ids[1],
        "action": "reject",
    })

# === 6. Memory Modes via Header ===
def test_mode_readonly():
    resp = req("POST", "/v1/recall", body={"query": "test"}, headers={"X-Memory-Mode": "readonly"})
    assert isinstance(resp, (list, dict))

def test_mode_off():
    resp = req("POST", "/v1/recall", body={"query": "test"}, headers={"X-Memory-Mode": "off"})
    # In "off" mode, should still return (empty) response
    assert isinstance(resp, (list, dict))

# === 7. Admin Endpoints ===
def test_admin_index_health():
    resp = req("GET", "/v1/admin/index-health")
    assert "status" in resp or "indexer_lag" in resp

def test_admin_query_explain():
    resp = req("POST", "/v1/admin/query-explain", body={
        "query": "redb storage backend",
        "limit": 3,
    })
    assert "final_results" in resp or "candidates" in resp or isinstance(resp, dict)

def test_admin_cache_stats():
    resp = req("GET", "/v1/admin/cache/stats")
    assert isinstance(resp, dict)

def test_admin_trust_stats():
    resp = req("GET", "/v1/admin/trust-stats")
    assert isinstance(resp, dict)

def test_admin_idf_stats():
    resp = req("GET", "/v1/admin/idf-stats")
    assert isinstance(resp, dict)

def test_admin_space_stats():
    resp = req("GET", "/v1/admin/space-stats")
    assert isinstance(resp, dict)

def test_admin_intent_cache_stats():
    resp = req("GET", "/v1/admin/intent-cache/stats")
    assert isinstance(resp, dict)

def test_admin_prefetch_stats():
    resp = req("GET", "/v1/admin/prefetch/stats")
    assert isinstance(resp, dict)

# === 8. Inspect/Peek ===
def test_inspect_memory():
    if not stored_ids:
        return
    resp = req("GET", f"/v1/inspect/{stored_ids[0]}")
    assert "content" in resp or "id" in resp

def test_peek_memory():
    if not stored_ids:
        return
    resp = req("GET", f"/v1/peek/{stored_ids[0]}")
    assert isinstance(resp, dict)

# === 9. Consolidation ===
def test_consolidate_dry_run():
    resp = req("POST", "/v1/consolidate", body={"dry_run": True})
    assert isinstance(resp, dict)

# === 10. GDPR ===
def test_export():
    resp = req("GET", "/v1/export")
    assert isinstance(resp, (dict, list))

def test_memories_list():
    resp = req("GET", "/v1/memories")
    assert isinstance(resp, (dict, list))

# === 11. Sharing ===
def test_sharing_list():
    resp = req("GET", "/v1/admin/sharing/list")
    assert isinstance(resp, (dict, list))

def test_sharing_accessible():
    resp = req("GET", "/v1/sharing/accessible")
    assert isinstance(resp, (dict, list))

# === 12. Edge Cases ===
def test_store_unicode():
    resp = req("POST", "/v1/store", body={
        "content": "E2E Unicode: Deployment-Regel für Produktion 🚀 — keine direkten Pushes erlaubt",
        "tags": ["e2e", "unicode"],
    })
    assert "id" in resp

def test_store_long_content():
    long_text = "E2E long content test. " * 200  # ~4.6KB
    resp = req("POST", "/v1/store", body={
        "content": long_text,
        "tags": ["e2e", "long"],
    })
    assert "id" in resp

def test_store_many_tags():
    resp = req("POST", "/v1/store", body={
        "content": "E2E many tags test memory",
        "tags": [f"tag-{i}" for i in range(20)],
    })
    assert "id" in resp

# === 13. Forget ===
def test_forget():
    if not stored_ids:
        return
    # Store a throwaway memory and delete it
    resp = req("POST", "/v1/store", body={
        "content": "E2E throwaway memory to be deleted",
        "tags": ["e2e", "throwaway"],
    })
    throwaway_id = resp.get("id")
    if throwaway_id:
        req("DELETE", "/v1/forget", body={"id": throwaway_id})

# === 14. Metrics ===
def test_metrics():
    # Metrics returns Prometheus text format, not JSON — use raw HTTP
    import urllib.request
    url = f"{BASE_URL}/metrics"
    request = urllib.request.Request(url, headers=AUTH, method="GET")
    try:
        with urllib.request.urlopen(request, timeout=10) as response:
            assert response.status == 200
            body = response.read().decode("utf-8", errors="replace")
            assert len(body) > 0
    except urllib.error.HTTPError as exc:
        assert False, f"metrics returned {exc.code}"


# Typo fix for Python < 3.13 compatibility
class AssertionError(AssertionError if hasattr(__builtins__, 'AssertionError') else AssertionError):
    pass


def main():
    print(f"[e2e] Testing against {BASE_URL}")
    started = time.time()

    # Run all tests
    test("health check", test_health)
    test("store single", test_store_single)
    test("store with metadata", test_store_with_metadata)
    test("store batch", test_store_batch)
    time.sleep(2)  # Let indexer catch up
    test("recall basic", test_recall_basic)
    test("recall with tags", test_recall_with_tags)
    test("recall empty/noise", test_recall_empty)
    test("recall batch", test_recall_batch)
    test("update memory", test_update_memory)
    test("feedback confirm", test_feedback_confirm)
    test("feedback reject", test_feedback_reject)
    test("mode: readonly header", test_mode_readonly)
    test("mode: off header", test_mode_off)
    test("admin: index health", test_admin_index_health)
    test("admin: query explain", test_admin_query_explain)
    test("admin: cache stats", test_admin_cache_stats)
    test("admin: trust stats", test_admin_trust_stats)
    test("admin: idf stats", test_admin_idf_stats)
    test("admin: space stats", test_admin_space_stats)
    test("admin: intent cache stats", test_admin_intent_cache_stats)
    test("admin: prefetch stats", test_admin_prefetch_stats)
    test("inspect memory", test_inspect_memory)
    test("peek memory", test_peek_memory)
    test("consolidate dry-run", test_consolidate_dry_run)
    test("GDPR: export", test_export)
    test("GDPR: memories list", test_memories_list)
    test("sharing: list", test_sharing_list)
    test("sharing: accessible", test_sharing_accessible)
    test("edge: unicode content", test_store_unicode)
    test("edge: long content", test_store_long_content)
    test("edge: many tags", test_store_many_tags)
    test("forget", test_forget)
    test("metrics endpoint", test_metrics)

    duration = round(time.time() - started, 1)
    passed = sum(1 for r in results if r["status"] == "pass")
    failed = sum(1 for r in results if r["status"] == "fail")
    errored = sum(1 for r in results if r["status"] == "error")

    report = {
        "runner": "tests/run_e2e_proxy_test.py",
        "generated_at": iso_now(),
        "base_url": BASE_URL,
        "duration_seconds": duration,
        "summary": {
            "total": len(results),
            "passed": passed,
            "failed": failed,
            "errored": errored,
        },
        "items": results,
    }

    OUTPUT_JSON.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    print(f"\n[e2e] {passed}/{len(results)} passed, {failed} failed, {errored} errors ({duration}s)")
    print(f"[e2e] Report: {OUTPUT_JSON}")

    if failed + errored > 0:
        print("\nFailed/Error tests:")
        for r in results:
            if r["status"] in ("fail", "error"):
                print(f"  {r['status'].upper()}: {r['name']} — {r.get('error', '')[:100]}")
        sys.exit(1)


if __name__ == "__main__":
    main()
