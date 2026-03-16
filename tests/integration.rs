// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 memoryOSS Contributors
// Integration tests: server API, MCP HTTP client, proxy, concurrency.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::{
    Json, Router,
    extract::State,
    http::HeaderMap,
    response::IntoResponse,
    routing::{get, post},
};

/// Helper: find a free TCP port.
fn free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

fn preferred_runtime_binary_for_home(home_dir: &std::path::Path) -> std::path::PathBuf {
    let current = std::path::PathBuf::from(env!("CARGO_BIN_EXE_memoryoss"))
        .canonicalize()
        .unwrap();
    let current_is_target = current
        .components()
        .any(|component| component.as_os_str() == "target");
    if !current_is_target && current.is_file() {
        return current;
    }

    for candidate in [
        std::path::PathBuf::from("/usr/local/bin/memoryoss"),
        std::path::PathBuf::from("/usr/bin/memoryoss"),
        home_dir.join(".cargo/bin/memoryoss"),
    ] {
        if candidate != current && candidate.is_file() {
            return candidate;
        }
    }

    current
}

fn write_team_bootstrap_manifest(path: &std::path::Path) {
    std::fs::write(
        path,
        serde_json::to_string_pretty(&serde_json::json!({
            "team_id": "team-alpha",
            "team_label": "Team Alpha",
            "catalog": {
                "catalog_id": "team-alpha-defaults",
                "label": "Team Alpha Defaults",
                "exported_at": chrono::Utc::now().to_rfc3339(),
                "identities": [
                    {
                        "id": "device:team-alpha-signer",
                        "kind": "device",
                        "label": "Team Alpha Signer",
                        "registered_at": chrono::Utc::now().to_rfc3339()
                    }
                ],
                "revocations": []
            }
        }))
        .unwrap(),
    )
    .unwrap();
}

fn read_json_value(path: &std::path::Path) -> serde_json::Value {
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

fn test_config_with_sections(
    port: u16,
    data_dir: &str,
    auth_entries: &str,
    extra_sections: &str,
) -> String {
    format!(
        r#"
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
audit_hmac_secret = "test-audit-secret-that-is-at-least-32-characters-long"

{auth_entries}

[logging]
level = "warn"

{extra_sections}
"#
    )
}

/// Helper: build a minimal config TOML for testing.
fn test_config(port: u16, data_dir: &str) -> String {
    test_config_with_sections(
        port,
        data_dir,
        r#"
[[auth.api_keys]]
key = "test-key-integration"
role = "admin"
namespace = "test"
"#,
        "",
    )
}

fn test_config_http(port: u16, data_dir: &str) -> String {
    format!(
        r#"
[server]
host = "127.0.0.1"
port = {port}

[tls]
enabled = false
auto_generate = false

[storage]
data_dir = "{data_dir}"

[auth]
jwt_secret = "test-secret-that-is-at-least-32-characters-long"
audit_hmac_secret = "test-audit-secret-that-is-at-least-32-characters-long"

[[auth.api_keys]]
key = "test-key-integration"
role = "admin"
namespace = "test"

[logging]
level = "warn"
"#
    )
}

fn test_config_http_with_sections(
    port: u16,
    data_dir: &str,
    auth_entries: &str,
    extra_sections: &str,
) -> String {
    format!(
        r#"
[server]
host = "127.0.0.1"
port = {port}

[tls]
enabled = false
auto_generate = false

[storage]
data_dir = "{data_dir}"

[auth]
jwt_secret = "test-secret-that-is-at-least-32-characters-long"

{auth_entries}

[logging]
level = "warn"

{extra_sections}
"#
    )
}

fn multi_namespace_test_config(port: u16, data_dir: &str) -> String {
    test_config_with_sections(
        port,
        data_dir,
        r#"
[[auth.api_keys]]
key = "alpha-admin-key"
role = "admin"
namespace = "alpha"

[[auth.api_keys]]
key = "beta-admin-key"
role = "admin"
namespace = "beta"
"#,
        "",
    )
}

fn test_key_id(raw_key: &str) -> String {
    use sha2::{Digest, Sha256};
    let hash = Sha256::digest(raw_key.as_bytes());
    hex::encode(&hash[..8])
}

fn compact_json_bytes(value: &serde_json::Value) -> Vec<u8> {
    serde_json::to_vec(value).unwrap()
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(bytes))
}

fn passport_payload_sha256(bundle: &serde_json::Value) -> String {
    let payload = serde_json::json!({
        "bundle_version": bundle["bundle_version"],
        "passport_id": bundle["passport_id"],
        "runtime_contract": bundle["runtime_contract"],
        "scope": bundle["scope"],
        "namespace": bundle["namespace"],
        "exported_at": bundle["exported_at"],
        "provenance": bundle["provenance"],
        "memories": bundle["memories"],
    });
    sha256_hex(&compact_json_bytes(&payload))
}

fn compatibility_fixture_snapshot(
    mut value: serde_json::Value,
    window: &str,
    published_in: &str,
) -> serde_json::Value {
    let snapshot = serde_json::json!({
        "window": window,
        "published_in": published_in,
        "reader_support": "n_to_n_minus_2",
    });
    if let Some(object) = value.as_object_mut() {
        object.insert("_compat_snapshot".to_string(), snapshot.clone());
    }
    if let Some(runtime_contract) = value
        .get_mut("runtime_contract")
        .and_then(serde_json::Value::as_object_mut)
    {
        runtime_contract.insert("_compat_snapshot".to_string(), snapshot.clone());
    }
    if let Some(memories) = value
        .get_mut("memories")
        .and_then(serde_json::Value::as_array_mut)
    {
        if let Some(first_memory) = memories
            .first_mut()
            .and_then(serde_json::Value::as_object_mut)
        {
            first_memory.insert(
                "_compat_snapshot".to_string(),
                serde_json::json!(format!("{window}:{published_in}")),
            );
        }
    }
    value
}

fn writer_only_test_config(port: u16, data_dir: &str) -> String {
    test_config_with_sections(
        port,
        data_dir,
        r#"
[[auth.api_keys]]
key = "writer-only-key"
role = "writer"
namespace = "test"
"#,
        "",
    )
}

fn test_client() -> reqwest::Client {
    reqwest::Client::builder()
        .danger_accept_invalid_certs(true)
        .build()
        .unwrap()
}

async fn inspect_memory(
    client: &reqwest::Client,
    base: &str,
    api_key: &str,
    memory_id: &str,
) -> serde_json::Value {
    let resp = client
        .get(format!("{base}/v1/inspect/{memory_id}"))
        .header("Authorization", format!("Bearer {api_key}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "inspect failed for memory {memory_id}");
    resp.json().await.unwrap()
}

async fn review_queue(
    client: &reqwest::Client,
    base: &str,
    api_key: &str,
    namespace: &str,
    limit: usize,
) -> serde_json::Value {
    let resp = client
        .get(format!(
            "{base}/v1/admin/review-queue?namespace={namespace}&limit={limit}"
        ))
        .header("Authorization", format!("Bearer {api_key}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "review queue failed");
    resp.json().await.unwrap()
}

async fn assert_contested_pair(
    client: &reqwest::Client,
    base: &str,
    api_key: &str,
    first_id: &str,
    second_id: &str,
) {
    let first = inspect_memory(client, base, api_key, first_id).await;
    assert_eq!(first["status"].as_str(), Some("contested"));
    assert_eq!(first["eligible_for_injection"].as_bool(), Some(false));
    assert_eq!(first["contradiction_count"].as_u64(), Some(1));
    assert!(
        first["contradicts_with"]
            .as_array()
            .map(|ids| ids.iter().any(|id| id.as_str() == Some(second_id)))
            .unwrap_or(false),
        "first memory should reference second as contradiction"
    );
    assert!(
        first["conflicts"]
            .as_array()
            .map(|conflicts| {
                conflicts
                    .iter()
                    .any(|conflict| conflict["id"].as_str() == Some(second_id))
            })
            .unwrap_or(false),
        "first memory should expose second in conflicts"
    );

    let second = inspect_memory(client, base, api_key, second_id).await;
    assert_eq!(second["status"].as_str(), Some("contested"));
    assert_eq!(second["eligible_for_injection"].as_bool(), Some(false));
    assert_eq!(second["contradiction_count"].as_u64(), Some(1));
    assert!(
        second["contradicts_with"]
            .as_array()
            .map(|ids| ids.iter().any(|id| id.as_str() == Some(first_id)))
            .unwrap_or(false),
        "second memory should reference first as contradiction"
    );
    assert!(
        second["conflicts"]
            .as_array()
            .map(|conflicts| {
                conflicts
                    .iter()
                    .any(|conflict| conflict["id"].as_str() == Some(first_id))
            })
            .unwrap_or(false),
        "second memory should expose first in conflicts"
    );
}

async fn wait_for_proxy_facts_extracted(
    client: &reqwest::Client,
    base: &str,
    proxy_key: &str,
    expected: u64,
) -> serde_json::Value {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let resp = client
            .get(format!("{base}/proxy/v1/debug/stats"))
            .header("Authorization", format!("Bearer {proxy_key}"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200, "proxy debug stats request failed");
        let body: serde_json::Value = resp.json().await.unwrap();
        let extracted = body["metrics"]["facts_extracted"].as_u64().unwrap_or(0);
        if extracted >= expected {
            return body;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for proxy extracted facts: expected >= {expected}, got {extracted}"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

async fn wait_for_superseded_by(
    client: &reqwest::Client,
    base: &str,
    api_key: &str,
    memory_id: &str,
) -> String {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let body = inspect_memory(client, base, api_key, memory_id).await;
        if let Some(derived_id) = body["superseded_by"].as_str() {
            return derived_id.to_string();
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for memory {memory_id} to be superseded"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

async fn wait_for_specific_superseded_by(
    client: &reqwest::Client,
    base: &str,
    api_key: &str,
    memory_id: &str,
    expected_derived_id: &str,
) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let body = inspect_memory(client, base, api_key, memory_id).await;
        if body["superseded_by"].as_str() == Some(expected_derived_id) {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for memory {memory_id} to be superseded by {expected_derived_id}"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

/// Start a server command in background, wait for it to be ready, return the child.
async fn start_server_command(config_path: &str, command: &str) -> tokio::process::Child {
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_memoryoss"))
        .args(["--config", config_path, command])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .expect("failed to start memoryoss");

    let config_text = std::fs::read_to_string(config_path).expect("failed to read test config");
    let port = if command == "serve-core" {
        config_text
            .lines()
            .map(str::trim)
            .find_map(|line| line.strip_prefix("core_port = "))
            .and_then(|raw| raw.parse::<u16>().ok())
            .expect("failed to parse test core port from config")
    } else {
        config_text
            .lines()
            .map(str::trim)
            .find_map(|line| line.strip_prefix("port = "))
            .and_then(|raw| raw.parse::<u16>().ok())
            .expect("failed to parse test server port from config")
    };

    let tls_enabled = config_text
        .lines()
        .map(str::trim)
        .find_map(|line| line.strip_prefix("enabled = "))
        .map(|raw| raw == "true")
        .unwrap_or(true);

    let scheme = if tls_enabled { "https" } else { "http" };
    let client = if tls_enabled {
        test_client()
    } else {
        reqwest::Client::builder().build().unwrap()
    };

    let health_url = format!("{scheme}://127.0.0.1:{port}/health");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    loop {
        if let Some(status) = child.try_wait().expect("failed to poll child process") {
            panic!("server exited before readiness check passed: {status}");
        }
        if let Some(status) = child.id()
            && TcpStream::connect(("127.0.0.1", port)).is_ok()
            && client
                .get(&health_url)
                .send()
                .await
                .ok()
                .map(|r| r.status().as_u16())
                == Some(200)
        {
            let _ = status;
            break;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!("server did not become ready: {health_url}");
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    child
}

/// Start server in background, wait for it to be ready, return the base URL.
async fn start_server(config_path: &str) -> tokio::process::Child {
    start_server_command(config_path, "serve").await
}

/// Helper: extract memory ID from a recall result entry.
/// The API returns `{"memory": {"id": "...", "content": "..."}, "score": ...}`.
fn mem_id(entry: &serde_json::Value) -> Option<&str> {
    entry["memory"]["id"].as_str()
}

fn mem_content(entry: &serde_json::Value) -> Option<&str> {
    entry["memory"]["content"].as_str()
}

fn sparse_embedding(seed: usize) -> Vec<f32> {
    let mut embedding = vec![0.0; 384];
    embedding[seed % 384] = 1.0;
    embedding[(seed * 17 + 11) % 384] = 0.5;
    embedding
}

#[derive(Clone, Default)]
struct DummyUpstreamState {
    requests: Arc<Mutex<Vec<serde_json::Value>>>,
}

fn record_upstream_request(
    state: &DummyUpstreamState,
    path: &str,
    headers: &HeaderMap,
    body: Option<serde_json::Value>,
) {
    state.requests.lock().unwrap().push(serde_json::json!({
        "path": path,
        "authorization": headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .unwrap_or(""),
        "x_api_key": headers
            .get("x-api-key")
            .and_then(|v| v.to_str().ok())
            .unwrap_or(""),
        "anthropic_version": headers
            .get("anthropic-version")
            .and_then(|v| v.to_str().ok())
            .unwrap_or(""),
        "anthropic_beta": headers
            .get("anthropic-beta")
            .and_then(|v| v.to_str().ok())
            .unwrap_or(""),
        "body": body,
    }));
}

async fn dummy_models(
    State(state): State<DummyUpstreamState>,
    headers: HeaderMap,
) -> Json<serde_json::Value> {
    record_upstream_request(&state, "/v1/models", &headers, None);
    Json(serde_json::json!({
        "data": [{"id": "gpt-4o-mini", "object": "model"}]
    }))
}

async fn dummy_chat(
    State(state): State<DummyUpstreamState>,
    headers: HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> axum::response::Response {
    let is_stream = body.get("stream").and_then(|v| v.as_bool()) == Some(true);
    record_upstream_request(&state, "/v1/chat/completions", &headers, Some(body.clone()));
    if is_stream {
        if body.get("tools").is_some() {
            let sse = concat!(
                "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_weather_1\",\"type\":\"function\",\"function\":{\"name\":\"get_weather\",\"arguments\":\"\"}}]}}]}\n\n",
                "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"{\\\"city\\\":\\\"Ber\"}}]}}]}\n\n",
                "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"lin\\\"}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\n",
                "data: [DONE]\n\n"
            );
            return (
                [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
                sse,
            )
                .into_response();
        }
        let sse = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"dummy \"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"chat completion\"}}]}\n\n",
            "data: [DONE]\n\n"
        );
        return (
            [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
            sse,
        )
            .into_response();
    }

    if body.get("tools").is_some() {
        return Json(serde_json::json!({
            "id": "chatcmpl-tools",
            "object": "chat.completion",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call_weather_1",
                        "type": "function",
                        "function": {
                            "name": "get_weather",
                            "arguments": "{\"city\":\"Berlin\"}"
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }]
        }))
        .into_response();
    }

    if body.get("response_format").is_some() {
        return Json(serde_json::json!({
            "id": "chatcmpl-json",
            "object": "chat.completion",
            "choices": [{
                "index": 0,
                "message": { "role": "assistant", "content": "{\"ok\":true,\"city\":\"Berlin\"}" },
                "finish_reason": "stop"
            }]
        }))
        .into_response();
    }

    Json(serde_json::json!({
        "id": "chatcmpl-test",
        "object": "chat.completion",
        "choices": [{
            "index": 0,
            "message": { "role": "assistant", "content": "dummy chat completion" },
            "finish_reason": "stop"
        }]
    }))
    .into_response()
}

async fn dummy_responses(
    State(state): State<DummyUpstreamState>,
    headers: HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    record_upstream_request(&state, "/v1/responses", &headers, Some(body));
    Json(serde_json::json!({
        "id": "resp_test",
        "output": [{
            "type": "message",
            "content": [{
                "type": "output_text",
                "text": "dummy responses output"
            }]
        }]
    }))
}

async fn dummy_anthropic(
    State(state): State<DummyUpstreamState>,
    headers: HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    record_upstream_request(&state, "/v1/messages", &headers, Some(body.clone()));
    if body["model"].as_str() == Some("claude-test-extract") {
        return Json(serde_json::json!({
            "id": "msg_extract",
            "type": "message",
            "role": "assistant",
            "model": "claude-test-extract",
            "stop_reason": "end_turn",
            "content": [{
                "type": "text",
                "text": "[{\"content\":\"Production deploys do not require staging approval before rollout.\",\"tags\":[\"deploy\",\"approval\"]}]"
            }],
            "usage": { "input_tokens": 1, "output_tokens": 1 }
        }));
    }
    if body["model"].as_str() == Some("claude-test-promote") {
        return Json(serde_json::json!({
            "id": "msg_promote",
            "type": "message",
            "role": "assistant",
            "model": "claude-test-promote",
            "stop_reason": "end_turn",
            "content": [{
                "type": "text",
                "text": "[{\"content\":\"Promotion fact alpha: use the rollout checklist before every production release.\",\"tags\":[\"deploy\",\"checklist\"]}]"
            }],
            "usage": { "input_tokens": 1, "output_tokens": 1 }
        }));
    }
    Json(serde_json::json!({
        "id": "msg_test",
        "type": "message",
        "role": "assistant",
        "model": "claude-3-5-haiku-latest",
        "stop_reason": "end_turn",
        "content": [{
            "type": "text",
            "text": "dummy anthropic output"
        }],
        "usage": { "input_tokens": 1, "output_tokens": 1 }
    }))
}

async fn start_dummy_upstream() -> (u16, DummyUpstreamState, tokio::task::JoinHandle<()>) {
    let state = DummyUpstreamState::default();
    let app = Router::new()
        .route("/v1/models", get(dummy_models))
        .route("/v1/chat/completions", post(dummy_chat))
        .route("/v1/responses", post(dummy_responses))
        .route("/v1/messages", post(dummy_anthropic))
        .with_state(state.clone());

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("failed to bind dummy upstream");
    let port = listener.local_addr().unwrap().port();
    let handle = tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("dummy upstream server failed");
    });

    tokio::time::sleep(Duration::from_millis(200)).await;
    (port, state, handle)
}

#[tokio::test]
async fn test_store_recall_update_forget() {
    let port = free_port();
    let tmp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let data_dir = tmp_dir.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();

    let config_content = format!(
        "{}\n[embeddings]\nmodel = \"bge-base-en-v1.5\"\n",
        test_config(port, data_dir.to_str().unwrap())
    );
    let config_path = tmp_dir.path().join("test.toml");
    std::fs::write(&config_path, &config_content).unwrap();

    let mut child = start_server(config_path.to_str().unwrap()).await;

    let client = reqwest::Client::builder()
        .danger_accept_invalid_certs(true)
        .build()
        .unwrap();
    let base = format!("https://127.0.0.1:{port}");

    // 1. Health check
    let resp = client.get(format!("{base}/health")).send().await.unwrap();
    assert_eq!(resp.status(), 200, "health check failed");

    // 2. Store a memory
    let store_resp = client
        .post(format!("{base}/v1/store"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({
            "content": "Rust is a systems programming language focused on safety.",
            "agent": "test-agent",
            "tags": ["rust", "programming"]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(store_resp.status(), 200, "store failed");
    let store_body: serde_json::Value = store_resp.json().await.unwrap();
    let memory_id = store_body["id"].as_str().expect("no id in store response");

    // Give the async indexer time to process
    tokio::time::sleep(Duration::from_secs(3)).await;

    // 3. Recall the memory
    let recall_resp = client
        .post(format!("{base}/v1/recall"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({
            "query": "What is Rust?"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(recall_resp.status(), 200, "recall failed");
    let recall_body: serde_json::Value = recall_resp.json().await.unwrap();
    let memories = recall_body["memories"]
        .as_array()
        .expect("no memories array");
    assert!(!memories.is_empty(), "recall returned no memories");
    assert!(
        memories.iter().any(|m| mem_id(m) == Some(memory_id)),
        "stored memory not found in recall"
    );

    // 4. Update the memory
    let update_resp = client
        .patch(format!("{base}/v1/update"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({
            "id": memory_id,
            "content": "Rust is a blazingly fast systems language with memory safety guarantees."
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(update_resp.status(), 200, "update failed");

    // Give the async indexer time to process the update
    tokio::time::sleep(Duration::from_secs(3)).await;

    // 5. Recall again — should get updated content
    let recall2_resp = client
        .post(format!("{base}/v1/recall"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({
            "query": "Rust memory safety"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(recall2_resp.status(), 200);
    let recall2_body: serde_json::Value = recall2_resp.json().await.unwrap();
    let updated = recall2_body["memories"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| mem_id(m) == Some(memory_id))
        .expect("updated memory not found");
    assert!(
        mem_content(updated).unwrap().contains("blazingly fast"),
        "content not updated"
    );

    // 6. Forget the memory
    let forget_resp = client
        .delete(format!("{base}/v1/forget"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({
            "id": memory_id
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(forget_resp.status(), 200, "forget failed");

    // Give the async indexer time to process the deletion
    tokio::time::sleep(Duration::from_secs(3)).await;

    // 7. Recall again — should be empty
    let recall3_resp = client
        .post(format!("{base}/v1/recall"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({
            "query": "Rust"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(recall3_resp.status(), 200);
    let recall3_body: serde_json::Value = recall3_resp.json().await.unwrap();
    let remaining = recall3_body["memories"].as_array().unwrap();
    assert!(
        !remaining.iter().any(|m| mem_id(m) == Some(memory_id)),
        "forgotten memory still returned"
    );

    // Cleanup
    child.kill().await.ok();
}

#[tokio::test]
async fn test_server_can_run_plain_http_without_dev_mode() {
    let port = free_port();
    let tmp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let data_dir = tmp_dir.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();

    let config_content = test_config_http(port, data_dir.to_str().unwrap());
    let config_path = tmp_dir.path().join("http-test.toml");
    std::fs::write(&config_path, &config_content).unwrap();

    let mut child = start_server(config_path.to_str().unwrap()).await;
    let client = reqwest::Client::new();
    let base = format!("http://127.0.0.1:{port}");

    let health = client
        .get(format!("{base}/health"))
        .send()
        .await
        .expect("health request failed");
    assert!(health.status().is_success());

    let store = client
        .post(format!("{base}/v1/store"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({
            "content": "plain http product mode works",
            "namespace": "test"
        }))
        .send()
        .await
        .expect("store request failed");
    assert!(
        store.status().is_success(),
        "store status {}",
        store.status()
    );

    child.kill().await.ok();
}

#[tokio::test]
async fn test_batch_store_handles_zero_knowledge_and_source_provenance() {
    let port = free_port();
    let tmp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let data_dir = tmp_dir.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let config_content = test_config(port, data_dir.to_str().unwrap());
    let config_path = tmp_dir.path().join("test.toml");
    std::fs::write(&config_path, &config_content).unwrap();

    let mut child = start_server(config_path.to_str().unwrap()).await;
    let client = test_client();
    let base = format!("https://127.0.0.1:{port}");

    let batch_resp = client
        .post(format!("{base}/v1/store/batch"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({
            "memories": [
                {
                    "content": "ciphertext-alpha-zero-knowledge",
                    "tags": ["zk", "batch"],
                    "zero_knowledge": true,
                    "embedding": sparse_embedding(7)
                },
                {
                    "content": "plain batch memory beta",
                    "tags": ["plain", "batch"]
                }
            ]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(batch_resp.status(), 200, "batch store failed");
    let batch_body: serde_json::Value = batch_resp.json().await.unwrap();
    let stored = batch_body["stored"]
        .as_array()
        .expect("stored array missing");
    assert_eq!(stored.len(), 2);
    let zk_id = stored[0]["id"].as_str().expect("zk id missing");

    let export_resp = client
        .get(format!("{base}/v1/export"))
        .header("Authorization", "Bearer test-key-integration")
        .send()
        .await
        .unwrap();
    assert_eq!(export_resp.status(), 200);
    let export_body: serde_json::Value = export_resp.json().await.unwrap();
    let memories = export_body["memories"]
        .as_array()
        .expect("memories missing");
    let zk_memory = memories
        .iter()
        .find(|memory| memory["id"].as_str() == Some(zk_id))
        .expect("zero-knowledge batch memory missing from export");

    assert_eq!(zk_memory["content_hash"], serde_json::Value::Null);
    assert!(
        zk_memory["source_key_id"].as_str().is_some(),
        "batch store should preserve source provenance"
    );
    assert_ne!(
        zk_memory["source_key_id"].as_str(),
        Some("test-key-integration"),
        "source provenance must use opaque key ids, not raw API keys"
    );

    let recall_resp = client
        .post(format!("{base}/v1/recall"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({
            "query": "ciphertext alpha zero knowledge",
            "query_embedding": sparse_embedding(7),
            "limit": 5
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(recall_resp.status(), 200);
    let recall_body: serde_json::Value = recall_resp.json().await.unwrap();
    let recall_memories = recall_body["memories"]
        .as_array()
        .expect("recall memories missing");
    assert!(
        recall_memories
            .iter()
            .any(|entry| mem_content(entry) == Some("ciphertext-alpha-zero-knowledge")),
        "zero-knowledge batch memory should be recallable with its provided embedding"
    );

    child.kill().await.ok();
}

#[tokio::test]
async fn test_query_explain_returns_real_score_breakdown() {
    let port = free_port();
    let tmp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let data_dir = tmp_dir.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();

    let config_content = test_config(port, data_dir.to_str().unwrap());
    let config_path = tmp_dir.path().join("test.toml");
    std::fs::write(&config_path, &config_content).unwrap();

    let mut child = start_server(config_path.to_str().unwrap()).await;

    let client = reqwest::Client::builder()
        .danger_accept_invalid_certs(true)
        .build()
        .unwrap();
    let base = format!("https://127.0.0.1:{port}");

    let store_resp = client
        .post(format!("{base}/v1/store"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({
            "content": "The file src/server/routes.rs handles X-Memory-Mode headers and namespace enforcement.",
            "agent": "test-agent",
            "tags": ["routes", "headers"]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(store_resp.status(), 200, "store failed");

    tokio::time::sleep(Duration::from_secs(3)).await;

    let explain_resp = client
        .post(format!("{base}/v1/admin/query-explain"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({
            "query": "src/server/routes.rs X-Memory-Mode namespace",
            "limit": 5
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(explain_resp.status(), 200, "query explain failed");

    let explain_body: serde_json::Value = explain_resp.json().await.unwrap();
    assert!(
        explain_body["idf_boost"].as_f64().is_some(),
        "missing idf_boost"
    );
    assert!(
        explain_body["identifiers"].as_array().is_some(),
        "missing identifiers"
    );

    let final_results = explain_body["final_results"]
        .as_array()
        .expect("missing final_results array");
    assert!(
        !final_results.is_empty(),
        "query explain returned no final results"
    );

    let first = &final_results[0];
    assert!(
        first["channels"]["exact"].as_f64().is_some(),
        "missing exact channel score"
    );
    assert!(
        first["trust_multiplier"].as_f64().is_some(),
        "missing trust multiplier"
    );
    assert!(
        first["trust_confidence_low"].as_f64().is_some(),
        "missing trust confidence low"
    );
    assert!(
        first["trust_confidence_high"].as_f64().is_some(),
        "missing trust confidence high"
    );
    assert!(
        first["trust_signals"]["outcome_learning"]
            .as_f64()
            .is_some(),
        "missing outcome_learning trust signal"
    );
    assert!(
        first["final_score"].as_f64().is_some(),
        "missing final score"
    );
    assert!(
        first["memory"]["content"]
            .as_str()
            .unwrap_or("")
            .contains("src/server/routes.rs"),
        "expected stored memory in explain results"
    );
    assert_eq!(
        explain_body["retrieval_gate"]["decision"].as_str(),
        Some("inject"),
        "strong exact retrieval should pass the confidence gate"
    );
    let summary_results = explain_body["summary_results"]
        .as_array()
        .expect("missing summary_results array");
    assert!(
        !summary_results.is_empty(),
        "missing summary-level explain results"
    );
    let first_summary = &summary_results[0];
    assert!(
        first_summary["summary"].as_str().is_some(),
        "missing summary text"
    );
    assert!(
        first_summary["provenance"].as_array().is_some(),
        "missing summary provenance"
    );
    assert!(
        first_summary["evidence"]
            .as_array()
            .map(|items| !items.is_empty())
            .unwrap_or(false),
        "missing evidence previews"
    );
    assert!(
        first_summary["evidence"][0]["preview"]
            .as_str()
            .unwrap_or("")
            .contains("X-Memory-Mode"),
        "evidence preview should preserve supporting detail"
    );

    let recall_resp = client
        .post(format!("{base}/v1/recall"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({
            "query": "src/server/routes.rs X-Memory-Mode namespace",
            "limit": 5
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(recall_resp.status(), 200, "recall failed");
    let recall_body: serde_json::Value = recall_resp.json().await.unwrap();
    assert!(
        recall_body["summaries"]
            .as_array()
            .map(|items| !items.is_empty())
            .unwrap_or(false),
        "recall should expose summary/evidence view alongside raw memories"
    );

    child.kill().await.ok();
}

#[tokio::test]
async fn test_query_explain_reports_need_more_evidence_and_abstain() {
    let port = free_port();
    let tmp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let data_dir = tmp_dir.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();

    let config_content = test_config(port, data_dir.to_str().unwrap());
    let config_path = tmp_dir.path().join("gate-explain.toml");
    std::fs::write(&config_path, &config_content).unwrap();

    let mut child = start_server(config_path.to_str().unwrap()).await;
    let client = test_client();
    let base = format!("https://127.0.0.1:{port}");

    let store_resp = client
        .post(format!("{base}/v1/store/batch"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({
            "memories": [
                {
                    "content": "Deploy smoke rule: after smoke passes, continue the staged rollout to production.",
                    "tags": ["deploy", "smoke", "rollout"]
                },
                {
                    "content": "Release smoke rule: after smoke passes, publish the docker image to ghcr.io/memoryosscom/memoryoss.",
                    "tags": ["release", "smoke", "docker"]
                }
            ]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(store_resp.status(), 200);

    tokio::time::sleep(Duration::from_secs(3)).await;

    let ambiguous_resp = client
        .post(format!("{base}/v1/admin/query-explain"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({
            "query": "what should happen after smoke passes?",
            "limit": 5
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(ambiguous_resp.status(), 200);
    let ambiguous_body: serde_json::Value = ambiguous_resp.json().await.unwrap();
    assert_eq!(
        ambiguous_body["retrieval_gate"]["decision"].as_str(),
        Some("need_more_evidence")
    );
    assert!(
        ambiguous_body["retrieval_gate"]["reasons"]
            .as_array()
            .unwrap_or(&Vec::new())
            .iter()
            .any(|reason| reason.as_str() == Some("top_candidates_too_close"))
    );

    let abstain_resp = client
        .post(format!("{base}/v1/admin/query-explain"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({
            "query": "tell me a joke about deployments",
            "limit": 5
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(abstain_resp.status(), 200);
    let abstain_body: serde_json::Value = abstain_resp.json().await.unwrap();
    assert_eq!(
        abstain_body["retrieval_gate"]["decision"].as_str(),
        Some("abstain")
    );

    child.kill().await.ok();
}

#[tokio::test]
async fn test_query_explain_needs_more_evidence_for_shared_smoke_anchor_with_extra_memories() {
    let port = free_port();
    let tmp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let data_dir = tmp_dir.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();

    let config_content = test_config(port, data_dir.to_str().unwrap());
    let config_path = tmp_dir.path().join("gate-explain-shared-anchor.toml");
    std::fs::write(&config_path, &config_content).unwrap();

    let mut child = start_server(config_path.to_str().unwrap()).await;
    let client = test_client();
    let base = format!("https://127.0.0.1:{port}");

    let store_resp = client
        .post(format!("{base}/v1/store/batch"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({
            "memories": [
                {
                    "content": "Deploy smoke rule: after smoke passes, continue the staged rollout to production.",
                    "tags": ["deploy", "smoke", "rollout"]
                },
                {
                    "content": "Release smoke rule: after smoke passes, publish the docker image to ghcr.io/memoryosscom/memoryoss.",
                    "tags": ["release", "smoke", "docker"]
                },
                {
                    "content": "Auth review checklist: require tests and security review before merging sensitive changes.",
                    "tags": ["review", "security", "checklist"]
                },
                {
                    "content": "For review responses, keep findings first and make missing evidence explicit.",
                    "tags": ["review", "style", "findings-first"]
                }
            ]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(store_resp.status(), 200);

    tokio::time::sleep(Duration::from_secs(3)).await;

    let ambiguous_resp = client
        .post(format!("{base}/v1/admin/query-explain"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({
            "query": "what should happen after smoke passes?",
            "limit": 5
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(ambiguous_resp.status(), 200);
    let ambiguous_body: serde_json::Value = ambiguous_resp.json().await.unwrap();
    assert_eq!(
        ambiguous_body["retrieval_gate"]["decision"].as_str(),
        Some("need_more_evidence")
    );
    assert!(
        ambiguous_body["retrieval_gate"]["reasons"]
            .as_array()
            .unwrap_or(&Vec::new())
            .iter()
            .any(|reason| reason.as_str() == Some("shared_query_anchor_across_candidates"))
    );

    child.kill().await.ok();
}

#[tokio::test]
async fn test_query_explain_prioritizes_task_context_for_deploy_bugfix_and_review() {
    let port = free_port();
    let tmp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let data_dir = tmp_dir.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();

    let config_content = test_config(port, data_dir.to_str().unwrap());
    let config_path = tmp_dir.path().join("test.toml");
    std::fs::write(&config_path, &config_content).unwrap();

    let mut child = start_server(config_path.to_str().unwrap()).await;
    let client = test_client();
    let base = format!("https://127.0.0.1:{port}");

    let store_resp = client
        .post(format!("{base}/v1/store/batch"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({
            "memories": [
                {
                    "content": "Payment deploy rule: staging approval and smoke tests are required before production rollout.",
                    "tags": ["deploy", "approval", "smoke"]
                },
                {
                    "content": "Payment incident fix: a stale feature flag caused rollout errors; clear flags before retrying the job.",
                    "tags": ["bugfix", "incident", "feature-flag"]
                },
                {
                    "content": "Checkout incident root cause: stale cart cache triggered 500 errors; fix by invalidating cache on retry.",
                    "tags": ["bugfix", "incident", "root-cause"]
                },
                {
                    "content": "Checkout deployment checklist: validate the cart cache before production release.",
                    "tags": ["deploy", "checklist"]
                },
                {
                    "content": "Auth review checklist: require tests and security review before merging sensitive changes.",
                    "tags": ["review", "security", "checklist"]
                },
                {
                    "content": "Auth hotfix note: merge the rollback patch only after flushing the token cache.",
                    "tags": ["bugfix", "hotfix"]
                }
            ]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(store_resp.status(), 200);

    tokio::time::sleep(Duration::from_secs(3)).await;

    let deploy_resp = client
        .post(format!("{base}/v1/admin/query-explain"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({
            "query": "Need the payment rollout steps and staging approval before production deploy.",
            "limit": 5
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(deploy_resp.status(), 200);
    let deploy_body: serde_json::Value = deploy_resp.json().await.unwrap();
    assert_eq!(deploy_body["task_context"]["kind"].as_str(), Some("deploy"));
    assert_eq!(deploy_body["task_state"]["kind"].as_str(), Some("deploy"));
    assert!(
        deploy_body["task_state"]["constraints"]
            .as_array()
            .is_some_and(|items| !items.is_empty()),
        "deploy query should compile explicit constraints"
    );
    assert!(
        deploy_body["task_state"]["facts"]
            .as_array()
            .is_some_and(|items| !items.is_empty()),
        "deploy query should compile explicit facts"
    );
    assert!(
        deploy_body["task_state"]["decisions"]
            .as_array()
            .is_some_and(|items| !items.is_empty()),
        "deploy task state should expose condensation decisions"
    );
    let deploy_results = deploy_body["final_results"].as_array().unwrap();
    assert!(
        deploy_results[0]["memory"]["content"]
            .as_str()
            .unwrap_or("")
            .contains("staging approval"),
        "deploy query should prioritize deployment guidance"
    );
    let deploy_provenance = deploy_results[0]["provenance"].as_array().unwrap();
    assert!(
        deploy_provenance
            .iter()
            .any(|entry| entry.as_str() == Some("task_context:deploy"))
    );
    assert!(deploy_provenance.iter().any(|entry| {
        entry
            .as_str()
            .map(|value| value.starts_with("task_match:"))
            .unwrap_or(false)
    }));

    let bugfix_resp = client
        .post(format!("{base}/v1/admin/query-explain"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({
            "query": "Debug the checkout regression and find the root cause of the 500 error.",
            "limit": 5
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(bugfix_resp.status(), 200);
    let bugfix_body: serde_json::Value = bugfix_resp.json().await.unwrap();
    assert_eq!(bugfix_body["task_context"]["kind"].as_str(), Some("bugfix"));
    let bugfix_results = bugfix_body["final_results"].as_array().unwrap();
    assert!(
        bugfix_results[0]["memory"]["content"]
            .as_str()
            .unwrap_or("")
            .contains("root cause"),
        "bugfix query should prioritize incident/root-cause memory"
    );
    let bugfix_provenance = bugfix_results[0]["provenance"].as_array().unwrap();
    assert!(
        bugfix_provenance
            .iter()
            .any(|entry| entry.as_str() == Some("task_context:bugfix"))
    );

    let review_resp = client
        .post(format!("{base}/v1/admin/query-explain"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({
            "query": "Review the auth changes before merge and audit anything risky.",
            "limit": 5
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(review_resp.status(), 200);
    let review_body: serde_json::Value = review_resp.json().await.unwrap();
    assert_eq!(review_body["task_context"]["kind"].as_str(), Some("review"));
    let review_results = review_body["final_results"].as_array().unwrap();
    assert!(
        review_results[0]["memory"]["content"]
            .as_str()
            .unwrap_or("")
            .contains("security review"),
        "review query should prioritize review checklist memory"
    );
    let review_provenance = review_results[0]["provenance"].as_array().unwrap();
    assert!(
        review_provenance
            .iter()
            .any(|entry| entry.as_str() == Some("task_context:review"))
    );

    child.kill().await.ok();
}

#[tokio::test]
async fn test_query_explain_surfaces_primitive_algebra_and_collapses_dependency_duplicates() {
    let port = free_port();
    let tmp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let data_dir = tmp_dir.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();

    let extra_sections = r#"
[proxy]
primitive_algebra = true
"#;
    let config_content = test_config_with_sections(
        port,
        data_dir.to_str().unwrap(),
        r#"
[[auth.api_keys]]
key = "test-key-integration"
role = "admin"
namespace = "test"
"#,
        extra_sections,
    );
    let config_path = tmp_dir.path().join("primitive-algebra.toml");
    std::fs::write(&config_path, &config_content).unwrap();

    let mut child = start_server(config_path.to_str().unwrap()).await;
    let client = test_client();
    let base = format!("https://127.0.0.1:{port}");

    let store_resp = client
        .post(format!("{base}/v1/store/batch"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({
            "memories": [
                {
                    "content": "Auth hotfix dependency: flush the token cache before production deploy.",
                    "tags": ["auth", "hotfix", "dependency"]
                },
                {
                    "content": "Auth hotfix incident: token cache failures blocked rollout last week until the cache was cleared.",
                    "tags": ["auth", "hotfix", "incident"]
                },
                {
                    "content": "Auth rollback prerequisite: token cache flush must happen before the patch ships.",
                    "tags": ["auth", "hotfix", "dependency"]
                }
            ]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(store_resp.status(), 200);

    tokio::time::sleep(Duration::from_secs(3)).await;

    let explain_resp = client
        .post(format!("{base}/v1/admin/query-explain"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({
            "query": "Which auth hotfix memory is the blocking dependency before production deploy?",
            "limit": 5
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(explain_resp.status(), 200);
    let explain_body: serde_json::Value = explain_resp.json().await.unwrap();
    assert_eq!(
        explain_body["primitive_algebra"]["enabled"].as_bool(),
        Some(true)
    );
    assert!(
        explain_body["primitive_algebra"]["query_primitives"]
            .as_array()
            .is_some_and(|items| items
                .iter()
                .any(|item| item["kind"].as_str() == Some("dependency"))),
        "query explain should surface dependency primitives for the query"
    );
    assert!(
        explain_body["primitive_algebra"]["matched_memories"]
            .as_array()
            .is_some_and(|items| !items.is_empty()),
        "primitive explain should surface matched memories when the lane is enabled"
    );
    let top_result = &explain_body["final_results"][0];
    let top_content = top_result["memory"]["content"].as_str().unwrap_or("");
    assert!(
        top_content.contains("flush the token cache"),
        "dependency memory should rank first under primitive algebra"
    );
    assert!(
        top_content.contains("before production deploy")
            && top_content.contains("before the patch ships"),
        "primitive collapse should fuse same-merge-key dependency memories"
    );
    assert!(
        top_result["primitive_decomposition"]["transfer_operators"]
            .as_array()
            .is_some_and(|items| items
                .iter()
                .any(|item| { item["operator"].as_str() == Some("carry_forward_dependency") })),
        "top result should expose dependency transfer operators"
    );
    assert!(
        top_result["provenance"]
            .as_array()
            .is_some_and(|items| items
                .iter()
                .any(|item| { item.as_str() == Some("primitive_kind_match:dependency") })),
        "primitive provenance should explain the rerank"
    );

    child.kill().await.ok();
}

#[tokio::test]
async fn test_proxy_injection_prefers_review_context_memory() {
    let (upstream_port, upstream_state, upstream_handle) = start_dummy_upstream().await;

    let port = free_port();
    let tmp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let data_dir = tmp_dir.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();

    let auth_entries = r#"
[[auth.api_keys]]
key = "test-key-integration"
role = "admin"
namespace = "test"
"#;
    let extra_sections = format!(
        r#"
[proxy]
enabled = true
passthrough_auth = false
upstream_url = "http://127.0.0.1:{upstream_port}/v1"
upstream_api_key = "upstream-openai-key"
default_memory_mode = "full"
extraction_enabled = false

[[proxy.key_mapping]]
proxy_key = "test-key-proxy"
namespace = "test"
"#
    );
    let config_content = test_config_with_sections(
        port,
        data_dir.to_str().unwrap(),
        auth_entries,
        &extra_sections,
    );
    let config_path = tmp_dir.path().join("proxy-review.toml");
    std::fs::write(&config_path, &config_content).unwrap();

    let mut child = start_server(config_path.to_str().unwrap()).await;
    let client = test_client();
    let base = format!("https://127.0.0.1:{port}");

    let store_resp = client
        .post(format!("{base}/v1/store/batch"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({
            "memories": [
                {
                    "content": "Auth review checklist: require tests and security review before merging sensitive changes.",
                    "tags": ["review", "security", "checklist"]
                },
                {
                    "content": "Auth hotfix note: merge the rollback patch only after flushing the token cache.",
                    "tags": ["bugfix", "hotfix"]
                }
            ]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(store_resp.status(), 200);

    tokio::time::sleep(Duration::from_secs(3)).await;

    let query = "Review the auth changes before merge and audit anything risky.";
    let explain_resp = client
        .post(format!("{base}/v1/admin/query-explain"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({
            "query": query,
            "limit": 5
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(explain_resp.status(), 200);
    let explain_body: serde_json::Value = explain_resp.json().await.unwrap();
    assert_eq!(
        explain_body["task_context"]["kind"].as_str(),
        Some("review")
    );
    assert_eq!(explain_body["task_state"]["kind"].as_str(), Some("review"));
    let explain_results = explain_body["final_results"].as_array().unwrap();
    assert!(
        explain_results[0]["memory"]["content"]
            .as_str()
            .unwrap_or("")
            .contains("security review"),
        "review explain path should rank checklist memory first"
    );

    let proxy_resp = client
        .post(format!("{base}/proxy/v1/chat/completions"))
        .header("Authorization", "Bearer test-key-proxy")
        .json(&serde_json::json!({
            "model": "gpt-4o-mini",
            "messages": [{"role": "user", "content": query}]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(proxy_resp.status(), 200);
    assert!(
        proxy_resp
            .headers()
            .get("x-memory-injected-count")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0)
            >= 1,
        "proxy should inject at least one contextual memory"
    );
    assert_eq!(
        proxy_resp
            .headers()
            .get("x-memory-gate-decision")
            .and_then(|v| v.to_str().ok()),
        Some("inject")
    );
    let requests = upstream_state.requests.lock().unwrap().clone();
    let upstream_req = requests
        .iter()
        .find(|req| {
            req["path"].as_str() == Some("/v1/chat/completions")
                && req["body"]["messages"][0]["role"].as_str() == Some("system")
        })
        .expect("missing upstream chat request with injected system prompt");
    let system_content = upstream_req["body"]["messages"][0]["content"]
        .as_str()
        .expect("system content missing");
    let review_memory = "Auth review checklist: require tests and security review before merging sensitive changes.";
    assert!(
        system_content.contains(review_memory),
        "review memory should be present in injected context"
    );
    assert!(
        system_content.contains("<task_state kind=\"review\">"),
        "review queries should inject an explicit compiled task state"
    );
    assert!(
        system_content.contains("<constraints>"),
        "task state should separate constraints from other context"
    );

    child.kill().await.ok();
    upstream_handle.abort();
}

#[tokio::test]
async fn test_proxy_confidence_gate_skips_ambiguous_memory_injection() {
    let (upstream_port, upstream_state, upstream_handle) = start_dummy_upstream().await;

    let port = free_port();
    let tmp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let data_dir = tmp_dir.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();

    let auth_entries = r#"
[[auth.api_keys]]
key = "test-key-integration"
role = "admin"
namespace = "test"
"#;
    let extra_sections = format!(
        r#"
[proxy]
enabled = true
passthrough_auth = false
upstream_url = "http://127.0.0.1:{upstream_port}/v1"
upstream_api_key = "upstream-openai-key"
default_memory_mode = "full"
confidence_gate = true
extraction_enabled = false

[[proxy.key_mapping]]
proxy_key = "test-key-proxy"
namespace = "test"
"#
    );
    let config_content = test_config_with_sections(
        port,
        data_dir.to_str().unwrap(),
        auth_entries,
        &extra_sections,
    );
    let config_path = tmp_dir.path().join("proxy-gate.toml");
    std::fs::write(&config_path, &config_content).unwrap();

    let mut child = start_server(config_path.to_str().unwrap()).await;
    let client = test_client();
    let base = format!("https://127.0.0.1:{port}");

    let store_resp = client
        .post(format!("{base}/v1/store/batch"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({
            "memories": [
                {
                    "content": "Deploy smoke rule: after smoke passes, continue the staged rollout to production.",
                    "tags": ["deploy", "smoke", "rollout"]
                },
                {
                    "content": "Release smoke rule: after smoke passes, publish the docker image to ghcr.io/memoryosscom/memoryoss.",
                    "tags": ["release", "smoke", "docker"]
                }
            ]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(store_resp.status(), 200);

    tokio::time::sleep(Duration::from_secs(3)).await;

    let proxy_resp = client
        .post(format!("{base}/proxy/v1/chat/completions"))
        .header("Authorization", "Bearer test-key-proxy")
        .json(&serde_json::json!({
            "model": "gpt-4o-mini",
            "messages": [{"role": "user", "content": "what should happen after smoke passes?"}]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(proxy_resp.status(), 200);
    assert_eq!(
        proxy_resp
            .headers()
            .get("x-memory-gate-decision")
            .and_then(|v| v.to_str().ok()),
        Some("need_more_evidence")
    );
    assert_eq!(
        proxy_resp
            .headers()
            .get("x-memory-injected-count")
            .and_then(|v| v.to_str().ok()),
        Some("0")
    );

    let requests = upstream_state.requests.lock().unwrap().clone();
    let upstream_req = requests
        .iter()
        .find(|req| req["path"].as_str() == Some("/v1/chat/completions"))
        .expect("missing upstream proxy request");
    let upstream_messages = upstream_req["body"]["messages"]
        .as_array()
        .expect("missing upstream messages");
    assert!(
        upstream_messages.iter().all(|message| {
            message["role"].as_str() != Some("system")
                || !message["content"]
                    .as_str()
                    .unwrap_or("")
                    .contains("<memory_context")
        }),
        "ambiguous proxy query should not inject memory context"
    );

    let stats_resp = client
        .get(format!("{base}/proxy/v1/debug/stats"))
        .header("Authorization", "Bearer test-key-proxy")
        .send()
        .await
        .unwrap();
    assert_eq!(stats_resp.status(), 200);
    let stats_body: serde_json::Value = stats_resp.json().await.unwrap();
    assert_eq!(stats_body["confidence_gate"].as_bool(), Some(true));
    assert_eq!(
        stats_body["metrics"]["gate_need_more_evidence"].as_u64(),
        Some(1)
    );

    child.kill().await.ok();
    upstream_handle.abort();
}

#[tokio::test]
async fn test_proxy_policy_firewall_blocks_risky_delete_and_surfaces_policy() {
    let (upstream_port, upstream_state, upstream_handle) = start_dummy_upstream().await;

    let port = free_port();
    let tmp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let data_dir = tmp_dir.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();

    let auth_entries = r#"
[[auth.api_keys]]
key = "test-key-integration"
role = "admin"
namespace = "test"
"#;
    let extra_sections = format!(
        r#"
[proxy]
enabled = true
passthrough_auth = false
upstream_url = "http://127.0.0.1:{upstream_port}/v1"
upstream_api_key = "upstream-openai-key"
default_memory_mode = "full"
confidence_gate = true
extraction_enabled = false

[[proxy.key_mapping]]
proxy_key = "test-key-proxy"
namespace = "test"
"#
    );
    let config_content = test_config_with_sections(
        port,
        data_dir.to_str().unwrap(),
        auth_entries,
        &extra_sections,
    );
    let config_path = tmp_dir.path().join("proxy-policy-block.toml");
    std::fs::write(&config_path, &config_content).unwrap();

    let mut child = start_server(config_path.to_str().unwrap()).await;
    let client = test_client();
    let base = format!("https://127.0.0.1:{port}");

    let store_resp = client
        .post(format!("{base}/v1/store"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({
            "content": "Retention policy: never delete audit logs or production backups from chat.",
            "tags": ["policy", "delete", "retention"]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(store_resp.status(), 200);

    tokio::time::sleep(Duration::from_secs(3)).await;

    let explain_resp = client
        .post(format!("{base}/v1/admin/query-explain"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({
            "query": "Delete the audit logs from production right now.",
            "limit": 5
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(explain_resp.status(), 200);
    let explain_body: serde_json::Value = explain_resp.json().await.unwrap();
    assert_eq!(
        explain_body["policy_firewall"]["decision"].as_str(),
        Some("block")
    );
    assert!(
        explain_body["policy_firewall"]["matched_policies"][0]["content"]
            .as_str()
            .unwrap_or("")
            .contains("never delete audit logs"),
        "query explain should surface the blocking policy memory"
    );

    let proxy_resp = client
        .post(format!("{base}/proxy/v1/chat/completions"))
        .header("Authorization", "Bearer test-key-proxy")
        .json(&serde_json::json!({
            "model": "gpt-4o-mini",
            "messages": [{"role": "user", "content": "Delete the audit logs from production right now."}]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(proxy_resp.status(), 403);
    assert_eq!(
        proxy_resp
            .headers()
            .get("x-memory-policy-decision")
            .and_then(|v| v.to_str().ok()),
        Some("block")
    );
    assert_eq!(
        proxy_resp
            .headers()
            .get("x-memory-policy-actions")
            .and_then(|v| v.to_str().ok()),
        Some("delete")
    );
    let proxy_body: serde_json::Value = proxy_resp.json().await.unwrap();
    assert_eq!(
        proxy_body["error"]["policy_firewall"]["decision"].as_str(),
        Some("block")
    );

    let requests = upstream_state.requests.lock().unwrap().clone();
    assert!(
        requests.is_empty(),
        "blocked policy request should never reach upstream"
    );

    child.kill().await.ok();
    upstream_handle.abort();
}

#[tokio::test]
async fn test_proxy_policy_firewall_requires_confirmation_for_deploy() {
    let (upstream_port, upstream_state, upstream_handle) = start_dummy_upstream().await;

    let port = free_port();
    let tmp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let data_dir = tmp_dir.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();

    let auth_entries = r#"
[[auth.api_keys]]
key = "test-key-integration"
role = "admin"
namespace = "test"
"#;
    let extra_sections = format!(
        r#"
[proxy]
enabled = true
passthrough_auth = false
upstream_url = "http://127.0.0.1:{upstream_port}/v1"
upstream_api_key = "upstream-openai-key"
default_memory_mode = "full"
confidence_gate = true
extraction_enabled = false

[[proxy.key_mapping]]
proxy_key = "test-key-proxy"
namespace = "test"
"#
    );
    let config_content = test_config_with_sections(
        port,
        data_dir.to_str().unwrap(),
        auth_entries,
        &extra_sections,
    );
    let config_path = tmp_dir.path().join("proxy-policy-confirm.toml");
    std::fs::write(&config_path, &config_content).unwrap();

    let mut child = start_server(config_path.to_str().unwrap()).await;
    let client = test_client();
    let base = format!("https://127.0.0.1:{port}");

    let store_resp = client
        .post(format!("{base}/v1/store"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({
            "content": "Release policy: staging approval is mandatory before production deploys.",
            "tags": ["policy", "deploy", "approval"]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(store_resp.status(), 200);

    tokio::time::sleep(Duration::from_secs(3)).await;

    let explain_resp = client
        .post(format!("{base}/v1/admin/query-explain"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({
            "query": "Deploy the checkout service to production tonight.",
            "limit": 5
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(explain_resp.status(), 200);
    let explain_body: serde_json::Value = explain_resp.json().await.unwrap();
    assert_eq!(
        explain_body["policy_firewall"]["decision"].as_str(),
        Some("require_confirmation")
    );

    let blocked_resp = client
        .post(format!("{base}/proxy/v1/chat/completions"))
        .header("Authorization", "Bearer test-key-proxy")
        .json(&serde_json::json!({
            "model": "gpt-4o-mini",
            "messages": [{"role": "user", "content": "Deploy the checkout service to production tonight."}]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(blocked_resp.status(), 428);
    assert_eq!(
        blocked_resp
            .headers()
            .get("x-memory-policy-decision")
            .and_then(|v| v.to_str().ok()),
        Some("require_confirmation")
    );
    assert_eq!(
        blocked_resp
            .headers()
            .get("x-memory-policy-confirmed")
            .and_then(|v| v.to_str().ok()),
        Some("false")
    );
    let blocked_body: serde_json::Value = blocked_resp.json().await.unwrap();
    assert_eq!(
        blocked_body["error"]["policy_firewall"]["confirmation_header"].as_str(),
        Some("x-memory-policy-confirm")
    );
    assert!(
        upstream_state.requests.lock().unwrap().is_empty(),
        "unconfirmed deploy should not reach upstream"
    );

    let confirmed_resp = client
        .post(format!("{base}/proxy/v1/chat/completions"))
        .header("Authorization", "Bearer test-key-proxy")
        .header("x-memory-policy-confirm", "true")
        .json(&serde_json::json!({
            "model": "gpt-4o-mini",
            "messages": [{"role": "user", "content": "Deploy the checkout service to production tonight."}]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(confirmed_resp.status(), 200);
    assert_eq!(
        confirmed_resp
            .headers()
            .get("x-memory-policy-decision")
            .and_then(|v| v.to_str().ok()),
        Some("require_confirmation")
    );
    assert_eq!(
        confirmed_resp
            .headers()
            .get("x-memory-policy-confirmed")
            .and_then(|v| v.to_str().ok()),
        Some("true")
    );

    let requests = upstream_state.requests.lock().unwrap().clone();
    assert_eq!(
        requests.len(),
        1,
        "confirmed deploy should reach upstream once"
    );

    child.kill().await.ok();
    upstream_handle.abort();
}

#[tokio::test]
async fn test_query_explain_policy_firewall_warns_on_soft_policy() {
    let port = free_port();
    let tmp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let data_dir = tmp_dir.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let config_path = tmp_dir.path().join("policy-warn.toml");
    std::fs::write(
        &config_path,
        test_config_http(port, data_dir.to_str().unwrap()),
    )
    .unwrap();

    let mut child = start_server(config_path.to_str().unwrap()).await;
    let client = reqwest::Client::builder().build().unwrap();
    let base = format!("http://127.0.0.1:{port}");

    let store_resp = client
        .post(format!("{base}/v1/store"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({
            "content": "Ops warning: Friday deploys need extra care and a canary note.",
            "tags": ["policy", "deploy", "warn"]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(store_resp.status(), 200);

    tokio::time::sleep(Duration::from_secs(3)).await;

    let explain_resp = client
        .post(format!("{base}/v1/admin/query-explain"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({
            "query": "Deploy the canary on Friday evening.",
            "limit": 5
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(explain_resp.status(), 200);
    let explain_body: serde_json::Value = explain_resp.json().await.unwrap();
    assert_eq!(
        explain_body["policy_firewall"]["decision"].as_str(),
        Some("warn")
    );

    child.kill().await.ok();
}

#[tokio::test]
async fn test_query_explain_policy_firewall_fixed_scenario_eval() {
    let port = free_port();
    let tmp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let data_dir = tmp_dir.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let config_path = tmp_dir.path().join("policy-eval.toml");
    std::fs::write(
        &config_path,
        test_config_http(port, data_dir.to_str().unwrap()),
    )
    .unwrap();

    let mut child = start_server(config_path.to_str().unwrap()).await;
    let client = reqwest::Client::builder().build().unwrap();
    let base = format!("http://127.0.0.1:{port}");

    for payload in [
        serde_json::json!({
            "content": "Release policy: never deploy to production without staging approval.",
            "tags": ["policy", "deploy", "approval"]
        }),
        serde_json::json!({
            "content": "Retention policy: never delete audit logs or production backups from chat.",
            "tags": ["policy", "delete", "retention"]
        }),
        serde_json::json!({
            "content": "Privacy policy: never export customer data or secrets to public gists or shared chats.",
            "tags": ["policy", "exfiltrate", "security"]
        }),
        serde_json::json!({
            "content": "Safety policy: never bypass review, auth, or approval guardrails.",
            "tags": ["policy", "override", "security"]
        }),
    ] {
        let resp = client
            .post(format!("{base}/v1/store"))
            .header("Authorization", "Bearer test-key-integration")
            .json(&payload)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
    }

    tokio::time::sleep(Duration::from_secs(3)).await;

    let bad_queries = [
        "Deploy the checkout service to production without staging approval.",
        "Delete the audit logs from production right now.",
        "Export the customer data to a public gist.",
        "Bypass review and merge the patch now.",
    ];
    let good_queries = [
        "What is our deploy policy before production rollout?",
        "Summarize the retention policy for audit logs.",
        "Explain why customer data must not be exported.",
        "Which review guardrails are currently required?",
    ];

    let mut blocked_bad_actions = 0usize;
    for query in bad_queries {
        let resp = client
            .post(format!("{base}/v1/admin/query-explain"))
            .header("Authorization", "Bearer test-key-integration")
            .json(&serde_json::json!({ "query": query, "limit": 5 }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let body: serde_json::Value = resp.json().await.unwrap();
        if body["policy_firewall"]["decision"].as_str() == Some("block") {
            blocked_bad_actions += 1;
        }
    }

    let mut false_blocks = 0usize;
    for query in good_queries {
        let resp = client
            .post(format!("{base}/v1/admin/query-explain"))
            .header("Authorization", "Bearer test-key-integration")
            .json(&serde_json::json!({ "query": query, "limit": 5 }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let body: serde_json::Value = resp.json().await.unwrap();
        if body["policy_firewall"]["decision"].as_str() == Some("block") {
            false_blocks += 1;
        }
        assert_eq!(
            body["policy_firewall"]["decision"].as_str(),
            Some("allow"),
            "informational scenario should not trigger the firewall"
        );
    }

    let false_block_rate = false_blocks as f64 / good_queries.len() as f64;
    assert_eq!(blocked_bad_actions, bad_queries.len());
    assert_eq!(false_block_rate, 0.0);

    child.kill().await.ok();
}

#[tokio::test]
async fn test_identifier_first_routing_prefers_matching_endpoint_and_collapses_fragments() {
    let (upstream_port, upstream_state, upstream_handle) = start_dummy_upstream().await;

    let port = free_port();
    let tmp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let data_dir = tmp_dir.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();

    let auth_entries = r#"
[[auth.api_keys]]
key = "test-key-integration"
role = "admin"
namespace = "test"
"#;
    let extra_sections = format!(
        r#"
[proxy]
enabled = true
passthrough_auth = false
upstream_url = "http://127.0.0.1:{upstream_port}/v1"
upstream_api_key = "upstream-openai-key"
default_memory_mode = "full"
identifier_first_routing = true
extraction_enabled = false

[[proxy.key_mapping]]
proxy_key = "test-key-proxy"
namespace = "test"
"#
    );
    let config_content = test_config_with_sections(
        port,
        data_dir.to_str().unwrap(),
        auth_entries,
        &extra_sections,
    );
    let config_path = tmp_dir.path().join("proxy-identifiers.toml");
    std::fs::write(&config_path, &config_content).unwrap();

    let mut child = start_server(config_path.to_str().unwrap()).await;
    let client = test_client();
    let base = format!("https://127.0.0.1:{port}");

    let store_resp = client
        .post(format!("{base}/v1/store/batch"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({
            "memories": [
                {
                    "content": "Claude proxy endpoint is /proxy/anthropic/v1/messages and Claude proxy mode should export ANTHROPIC_BASE_URL.",
                    "tags": ["proxy", "claude", "endpoint"]
                },
                {
                    "content": "Use /proxy/anthropic/v1/messages for Anthropic Messages API requests through the proxy.",
                    "tags": ["proxy", "anthropic", "endpoint"]
                },
                {
                    "content": "OpenAI chat proxy requests go to /proxy/v1/chat/completions and use OPENAI_BASE_URL.",
                    "tags": ["proxy", "openai", "endpoint"]
                }
            ]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(store_resp.status(), 200);

    tokio::time::sleep(Duration::from_secs(3)).await;

    let query = "which endpoint handles Anthropic messages through the proxy?";
    let explain_resp = client
        .post(format!("{base}/v1/admin/query-explain"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({
            "query": query,
            "limit": 5
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(explain_resp.status(), 200);
    let explain_body: serde_json::Value = explain_resp.json().await.unwrap();
    assert_eq!(
        explain_body["identifier_route"]["enabled"].as_bool(),
        Some(true)
    );
    let route_kinds = explain_body["identifier_route"]["kinds"]
        .as_array()
        .expect("missing identifier route kinds");
    assert!(
        route_kinds
            .iter()
            .any(|kind| kind.as_str() == Some("endpoint")),
        "query explain should detect endpoint-first route"
    );
    let final_results = explain_body["final_results"].as_array().unwrap();
    assert!(
        final_results[0]["memory"]["content"]
            .as_str()
            .unwrap_or("")
            .contains("/proxy/anthropic/v1/messages"),
        "endpoint route should rank anthropic endpoint first"
    );
    assert_eq!(
        explain_body["retrieval_gate"]["decision"].as_str(),
        Some("inject"),
        "query explain should inject for the strong endpoint match"
    );

    let terse_query = "anthropic proxy endpoint";
    let terse_explain_resp = client
        .post(format!("{base}/v1/admin/query-explain"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({
            "query": terse_query,
            "limit": 5
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(terse_explain_resp.status(), 200);
    let terse_explain_body: serde_json::Value = terse_explain_resp.json().await.unwrap();
    assert_eq!(
        terse_explain_body["retrieval_gate"]["decision"].as_str(),
        Some("inject"),
        "terse endpoint queries should stay aligned with proxy injection"
    );
    let terse_results = terse_explain_body["final_results"]
        .as_array()
        .expect("missing terse final_results");
    assert!(
        terse_results[0]["memory"]["content"]
            .as_str()
            .unwrap_or("")
            .contains("/proxy/anthropic/v1/messages"),
        "terse endpoint queries should still rank the anthropic endpoint first"
    );

    let literal_explain_resp = client
        .post(format!("{base}/v1/admin/query-explain"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({
            "query": "what is /proxy/anthropic/v1/messages used for?",
            "limit": 5
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(literal_explain_resp.status(), 200);
    let literal_explain_body: serde_json::Value = literal_explain_resp.json().await.unwrap();
    let literal_results = literal_explain_body["final_results"].as_array().unwrap();
    let anthropic_hits = literal_results
        .iter()
        .filter(|entry| {
            entry["memory"]["content"]
                .as_str()
                .unwrap_or("")
                .contains("/proxy/anthropic/v1/messages")
        })
        .count();
    assert_eq!(
        anthropic_hits, 1,
        "literal endpoint queries should collapse fragmented anthropic endpoint memories"
    );

    let proxy_resp = client
        .post(format!("{base}/proxy/v1/chat/completions"))
        .header("Authorization", "Bearer test-key-proxy")
        .json(&serde_json::json!({
            "model": "gpt-4o-mini",
            "messages": [{"role": "user", "content": query}]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(proxy_resp.status(), 200);
    assert_eq!(
        proxy_resp
            .headers()
            .get("x-memory-gate-decision")
            .and_then(|v| v.to_str().ok()),
        Some("inject")
    );
    assert_eq!(
        proxy_resp
            .headers()
            .get("x-memory-injected-count")
            .and_then(|v| v.to_str().ok()),
        Some("1"),
        "identifier-first routing should collapse duplicate endpoint fragments before injection"
    );

    let terse_proxy_resp = client
        .post(format!("{base}/proxy/v1/chat/completions"))
        .header("Authorization", "Bearer test-key-proxy")
        .json(&serde_json::json!({
            "model": "gpt-4o-mini",
            "messages": [{"role": "user", "content": terse_query}]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(terse_proxy_resp.status(), 200);
    assert_eq!(
        terse_proxy_resp
            .headers()
            .get("x-memory-gate-decision")
            .and_then(|v| v.to_str().ok()),
        Some("inject")
    );
    assert_eq!(
        terse_proxy_resp
            .headers()
            .get("x-memory-injected-count")
            .and_then(|v| v.to_str().ok()),
        Some("1"),
        "terse endpoint proxy query should inject the same endpoint memory"
    );

    let requests = upstream_state.requests.lock().unwrap().clone();
    let upstream_req = requests
        .iter()
        .find(|req| {
            req["path"].as_str() == Some("/v1/chat/completions")
                && req["body"]["messages"][0]["role"].as_str() == Some("system")
        })
        .expect("missing upstream chat request with injected system prompt");
    let system_content = upstream_req["body"]["messages"][0]["content"]
        .as_str()
        .expect("system content missing");
    assert!(
        system_content.contains("/proxy/anthropic/v1/messages"),
        "matching anthropic endpoint should be injected"
    );
    assert!(
        system_content.contains("<summary>"),
        "proxy injection should include summary blocks"
    );
    assert!(
        system_content.contains("<evidence"),
        "proxy injection should include evidence previews"
    );
    if system_content.contains("/proxy/v1/chat/completions") {
        let anthropic_pos = system_content.find("/proxy/anthropic/v1/messages").unwrap();
        let openai_pos = system_content.find("/proxy/v1/chat/completions").unwrap();
        assert!(
            anthropic_pos < openai_pos,
            "matching endpoint should appear before the openai distractor"
        );
    }

    let terse_upstream_req = requests
        .iter()
        .find(|req| {
            req["path"].as_str() == Some("/v1/chat/completions")
                && req["body"]["messages"][0]["role"].as_str() == Some("system")
                && req["body"]["messages"][1]["content"].as_str() == Some(terse_query)
        })
        .expect("missing terse upstream chat request with injected system prompt");
    let terse_system_content = terse_upstream_req["body"]["messages"][0]["content"]
        .as_str()
        .expect("terse system content missing");
    assert!(
        terse_system_content.contains("/proxy/anthropic/v1/messages"),
        "terse proxy query should inject the anthropic endpoint"
    );

    child.kill().await.ok();
    upstream_handle.abort();
}

#[tokio::test]
async fn test_feedback_updates_memory_lifecycle() {
    let port = free_port();
    let tmp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let data_dir = tmp_dir.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();

    let config_content = test_config(port, data_dir.to_str().unwrap());
    let config_path = tmp_dir.path().join("test.toml");
    std::fs::write(&config_path, &config_content).unwrap();

    let mut child = start_server(config_path.to_str().unwrap()).await;

    let client = reqwest::Client::builder()
        .danger_accept_invalid_certs(true)
        .build()
        .unwrap();
    let base = format!("https://127.0.0.1:{port}");

    let first_store = client
        .post(format!("{base}/v1/store"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({
            "content": "The deployment runbook requires staging before production.",
            "tags": ["deploy"]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(first_store.status(), 200);
    let first_id = first_store.json::<serde_json::Value>().await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    let second_store = client
        .post(format!("{base}/v1/store"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({
            "content": "The updated deployment runbook requires staging, smoke tests, and production approval.",
            "tags": ["deploy", "runbook"]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(second_store.status(), 200);
    let second_id = second_store.json::<serde_json::Value>().await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    tokio::time::sleep(Duration::from_secs(3)).await;

    let reject_resp = client
        .post(format!("{base}/v1/feedback"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({
            "id": first_id,
            "action": "reject"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(reject_resp.status(), 200, "reject feedback failed");
    let reject_body: serde_json::Value = reject_resp.json().await.unwrap();
    assert_eq!(reject_body["status"].as_str(), Some("contested"));
    assert_eq!(reject_body["reject_count"].as_u64(), Some(1));

    let supersede_resp = client
        .post(format!("{base}/v1/feedback"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({
            "id": first_id,
            "action": "supersede",
            "superseded_by": second_id
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(supersede_resp.status(), 200, "supersede feedback failed");
    let supersede_body: serde_json::Value = supersede_resp.json().await.unwrap();
    assert_eq!(supersede_body["status"].as_str(), Some("stale"));
    assert_eq!(
        supersede_body["superseded_by"].as_str(),
        Some(second_id.as_str())
    );
    assert_eq!(supersede_body["reject_count"].as_u64(), Some(1));
    assert_eq!(supersede_body["supersede_count"].as_u64(), Some(1));

    tokio::time::sleep(Duration::from_secs(2)).await;

    let inspect_first = client
        .get(format!("{base}/v1/inspect/{first_id}"))
        .header("Authorization", "Bearer test-key-integration")
        .send()
        .await
        .unwrap();
    assert_eq!(inspect_first.status(), 200);
    let inspect_first_body: serde_json::Value = inspect_first.json().await.unwrap();
    assert_eq!(inspect_first_body["status"].as_str(), Some("stale"));
    assert_eq!(inspect_first_body["reject_count"].as_u64(), Some(1));
    assert_eq!(inspect_first_body["supersede_count"].as_u64(), Some(1));
    assert!(inspect_first_body["last_outcome_at"].as_str().is_some());
    assert!(
        inspect_first_body["trust_signals"]["outcome_learning"]
            .as_f64()
            .is_some(),
        "inspect should expose outcome trust signals for stale memory"
    );

    let inspect_second = client
        .get(format!("{base}/v1/inspect/{second_id}"))
        .header("Authorization", "Bearer test-key-integration")
        .send()
        .await
        .unwrap();
    assert_eq!(inspect_second.status(), 200);
    let inspect_second_body: serde_json::Value = inspect_second.json().await.unwrap();
    assert_eq!(inspect_second_body["status"].as_str(), Some("active"));
    assert!(
        inspect_second_body["confirm_count"].as_u64().unwrap_or(0) >= 1,
        "superseding memory should record confirm outcomes"
    );
    assert!(
        inspect_second_body["evidence_count"].as_u64().unwrap_or(0) >= 2,
        "superseding memory should gain evidence"
    );
    assert!(inspect_second_body["last_outcome_at"].as_str().is_some());
    assert!(
        inspect_second_body["trust_signals"]["outcome_learning"]
            .as_f64()
            .is_some(),
        "inspect should expose outcome trust signals for active memory"
    );

    child.kill().await.ok();
}

#[tokio::test]
async fn test_lifecycle_view_filters_and_summarizes() {
    let port = free_port();
    let tmp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let data_dir = tmp_dir.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();

    let config_content = test_config(port, data_dir.to_str().unwrap());
    let config_path = tmp_dir.path().join("test.toml");
    std::fs::write(&config_path, &config_content).unwrap();

    let mut child = start_server(config_path.to_str().unwrap()).await;

    let client = reqwest::Client::builder()
        .danger_accept_invalid_certs(true)
        .build()
        .unwrap();
    let base = format!("https://127.0.0.1:{port}");

    let stale_store = client
        .post(format!("{base}/v1/store"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({
            "content": "Old deployment note: staging used to be optional.",
            "tags": ["deploy", "legacy"]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(stale_store.status(), 200);
    let stale_id = stale_store.json::<serde_json::Value>().await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    let active_store = client
        .post(format!("{base}/v1/store"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({
            "content": "Current deployment rule: always validate on staging first.",
            "tags": ["deploy", "current"]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(active_store.status(), 200);
    let active_id = active_store.json::<serde_json::Value>().await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    tokio::time::sleep(Duration::from_secs(3)).await;

    let supersede_resp = client
        .post(format!("{base}/v1/feedback"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({
            "id": stale_id,
            "action": "supersede",
            "superseded_by": active_id
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(supersede_resp.status(), 200, "supersede feedback failed");

    tokio::time::sleep(Duration::from_secs(2)).await;

    let lifecycle_resp = client
        .get(format!("{base}/v1/admin/lifecycle?status=stale&limit=10"))
        .header("Authorization", "Bearer test-key-integration")
        .send()
        .await
        .unwrap();
    assert_eq!(lifecycle_resp.status(), 200, "lifecycle view failed");

    let lifecycle_body: serde_json::Value = lifecycle_resp.json().await.unwrap();
    assert_eq!(lifecycle_body["namespace"].as_str(), Some("test"));
    assert_eq!(lifecycle_body["summary"]["total"].as_u64(), Some(2));
    assert_eq!(lifecycle_body["summary"]["active"].as_u64(), Some(1));
    assert_eq!(lifecycle_body["summary"]["stale"].as_u64(), Some(1));

    let memories = lifecycle_body["memories"]
        .as_array()
        .expect("missing memories");
    assert_eq!(
        memories.len(),
        1,
        "status filter should return one stale memory"
    );
    assert_eq!(memories[0]["id"].as_str(), Some(stale_id.as_str()));
    assert_eq!(memories[0]["status"].as_str(), Some("stale"));

    child.kill().await.ok();
}

#[tokio::test]
async fn test_contradiction_detection_marks_memories_contested() {
    let port = free_port();
    let tmp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let data_dir = tmp_dir.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();

    let config_content = test_config(port, data_dir.to_str().unwrap());
    let config_path = tmp_dir.path().join("test.toml");
    std::fs::write(&config_path, &config_content).unwrap();

    let mut child = start_server(config_path.to_str().unwrap()).await;

    let client = test_client();
    let base = format!("https://127.0.0.1:{port}");

    let first_store = client
        .post(format!("{base}/v1/store"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({
            "content": "Production deploys require staging approval before rollout.",
            "tags": ["deploy", "approval"]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(first_store.status(), 200);
    let first_id = first_store.json::<serde_json::Value>().await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    let second_store = client
        .post(format!("{base}/v1/store"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({
            "content": "Production deploys do not require staging approval before rollout.",
            "tags": ["deploy", "approval"]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(second_store.status(), 200);
    let second_id = second_store.json::<serde_json::Value>().await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    tokio::time::sleep(Duration::from_secs(2)).await;

    let inspect_first = client
        .get(format!("{base}/v1/inspect/{first_id}"))
        .header("Authorization", "Bearer test-key-integration")
        .send()
        .await
        .unwrap();
    assert_eq!(inspect_first.status(), 200);
    let inspect_first_body: serde_json::Value = inspect_first.json().await.unwrap();
    assert_eq!(inspect_first_body["status"].as_str(), Some("contested"));
    assert_eq!(
        inspect_first_body["eligible_for_injection"].as_bool(),
        Some(false)
    );
    let first_conflicts = inspect_first_body["contradicts_with"]
        .as_array()
        .expect("missing contradicts_with");
    assert_eq!(first_conflicts.len(), 1);
    assert_eq!(first_conflicts[0].as_str(), Some(second_id.as_str()));

    let inspect_second = client
        .get(format!("{base}/v1/inspect/{second_id}"))
        .header("Authorization", "Bearer test-key-integration")
        .send()
        .await
        .unwrap();
    assert_eq!(inspect_second.status(), 200);
    let inspect_second_body: serde_json::Value = inspect_second.json().await.unwrap();
    assert_eq!(inspect_second_body["status"].as_str(), Some("contested"));
    assert_eq!(
        inspect_second_body["eligible_for_injection"].as_bool(),
        Some(false)
    );
    let second_conflicts = inspect_second_body["conflicts"]
        .as_array()
        .expect("missing conflicts");
    assert_eq!(second_conflicts.len(), 1);
    assert_eq!(second_conflicts[0]["id"].as_str(), Some(first_id.as_str()));

    let lifecycle_resp = client
        .get(format!(
            "{base}/v1/admin/lifecycle?status=contested&limit=10"
        ))
        .header("Authorization", "Bearer test-key-integration")
        .send()
        .await
        .unwrap();
    assert_eq!(lifecycle_resp.status(), 200);
    let lifecycle_body: serde_json::Value = lifecycle_resp.json().await.unwrap();
    assert_eq!(lifecycle_body["summary"]["contested"].as_u64(), Some(2));
    let memories = lifecycle_body["memories"]
        .as_array()
        .expect("missing memories");
    assert_eq!(memories.len(), 2);
    assert!(
        memories
            .iter()
            .all(|memory| memory["eligible_for_injection"] == false)
    );
    assert!(memories.iter().all(|memory| {
        memory["contradicts_with"]
            .as_array()
            .map(|ids| ids.len() == 1)
            .unwrap_or(false)
    }));

    child.kill().await.ok();
}

#[tokio::test]
async fn test_batch_contradiction_detection_against_existing_memory() {
    let port = free_port();
    let tmp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let data_dir = tmp_dir.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();

    let config_content = test_config(port, data_dir.to_str().unwrap());
    let config_path = tmp_dir.path().join("test.toml");
    std::fs::write(&config_path, &config_content).unwrap();

    let mut child = start_server(config_path.to_str().unwrap()).await;
    let client = test_client();
    let base = format!("https://127.0.0.1:{port}");

    let first_store = client
        .post(format!("{base}/v1/store"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({
            "content": "Production deploys require staging approval before rollout.",
            "tags": ["deploy", "approval"]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(first_store.status(), 200);
    let first_id = first_store.json::<serde_json::Value>().await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    let batch_resp = client
        .post(format!("{base}/v1/store/batch"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({
            "memories": [{
                "content": "Production deploys do not require staging approval before rollout.",
                "tags": ["deploy", "approval"]
            }]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(batch_resp.status(), 200);
    let batch_body: serde_json::Value = batch_resp.json().await.unwrap();
    let second_id = batch_body["stored"][0]["id"]
        .as_str()
        .expect("batch store id missing")
        .to_string();

    assert_contested_pair(
        &client,
        &base,
        "test-key-integration",
        &first_id,
        &second_id,
    )
    .await;

    child.kill().await.ok();
}

#[tokio::test]
async fn test_batch_contradiction_detection_within_same_batch() {
    let port = free_port();
    let tmp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let data_dir = tmp_dir.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();

    let config_content = test_config(port, data_dir.to_str().unwrap());
    let config_path = tmp_dir.path().join("test.toml");
    std::fs::write(&config_path, &config_content).unwrap();

    let mut child = start_server(config_path.to_str().unwrap()).await;
    let client = test_client();
    let base = format!("https://127.0.0.1:{port}");

    let batch_resp = client
        .post(format!("{base}/v1/store/batch"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({
            "memories": [
                {
                    "content": "Production deploys require staging approval before rollout.",
                    "tags": ["deploy", "approval"]
                },
                {
                    "content": "Production deploys do not require staging approval before rollout.",
                    "tags": ["deploy", "approval"]
                }
            ]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(batch_resp.status(), 200);
    let batch_body: serde_json::Value = batch_resp.json().await.unwrap();
    let stored = batch_body["stored"]
        .as_array()
        .expect("stored array missing");
    assert_eq!(stored.len(), 2);
    let first_id = stored[0]["id"].as_str().expect("first batch id missing");
    let second_id = stored[1]["id"].as_str().expect("second batch id missing");

    assert_contested_pair(&client, &base, "test-key-integration", first_id, second_id).await;

    child.kill().await.ok();
}

#[tokio::test]
async fn test_update_recomputes_contradictions_when_content_changes() {
    let port = free_port();
    let tmp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let data_dir = tmp_dir.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();

    let config_content = test_config(port, data_dir.to_str().unwrap());
    let config_path = tmp_dir.path().join("test.toml");
    std::fs::write(&config_path, &config_content).unwrap();

    let mut child = start_server(config_path.to_str().unwrap()).await;
    let client = test_client();
    let base = format!("https://127.0.0.1:{port}");

    let first_store = client
        .post(format!("{base}/v1/store"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({
            "content": "Production deploys require staging approval before rollout.",
            "tags": ["deploy", "approval"]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(first_store.status(), 200);
    let first_id = first_store.json::<serde_json::Value>().await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    let second_store = client
        .post(format!("{base}/v1/store"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({
            "content": "Production deploys require smoke tests before rollout.",
            "tags": ["deploy", "smoke"]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(second_store.status(), 200);
    let second_id = second_store.json::<serde_json::Value>().await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    let update_resp = client
        .patch(format!("{base}/v1/update"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({
            "id": second_id,
            "content": "Production deploys do not require staging approval before rollout."
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(update_resp.status(), 200);
    let update_body: serde_json::Value = update_resp.json().await.unwrap();
    assert_eq!(update_body["status"].as_str(), Some("contested"));
    assert_eq!(update_body["eligible_for_injection"].as_bool(), Some(false));
    assert!(
        update_body["contradicts_with"]
            .as_array()
            .map(|ids| ids.iter().any(|id| id.as_str() == Some(first_id.as_str())))
            .unwrap_or(false),
        "updated memory should reference the original contradictory memory"
    );

    assert_contested_pair(
        &client,
        &base,
        "test-key-integration",
        &first_id,
        &second_id,
    )
    .await;

    child.kill().await.ok();
}

#[tokio::test]
async fn test_proxy_extraction_marks_existing_and_extracted_memories_contested() {
    let (upstream_port, upstream_state, upstream_handle) = start_dummy_upstream().await;

    let port = free_port();
    let tmp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let data_dir = tmp_dir.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();

    let auth_entries = r#"
[[auth.api_keys]]
key = "test-key-integration"
role = "admin"
namespace = "test"
"#;
    let extra_sections = format!(
        r#"
[proxy]
enabled = true
passthrough_auth = false
anthropic_upstream_url = "http://127.0.0.1:{upstream_port}/v1/messages"
anthropic_api_key = "anthropic-upstream-key"
default_memory_mode = "full"
extraction_enabled = true
extract_provider = "claude"
extract_model = "claude-test-extract"

[[proxy.key_mapping]]
proxy_key = "test-key-proxy"
namespace = "test"
"#
    );
    let config_content = test_config_with_sections(
        port,
        data_dir.to_str().unwrap(),
        auth_entries,
        &extra_sections,
    );
    let config_path = tmp_dir.path().join("proxy-extraction.toml");
    std::fs::write(&config_path, &config_content).unwrap();

    let mut child = start_server(config_path.to_str().unwrap()).await;
    let client = test_client();
    let base = format!("https://127.0.0.1:{port}");

    let first_store = client
        .post(format!("{base}/v1/store"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({
            "content": "Production deploys require staging approval before rollout.",
            "tags": ["deploy", "approval"]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(first_store.status(), 200);
    let first_id = first_store.json::<serde_json::Value>().await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    let proxy_resp = client
        .post(format!("{base}/proxy/anthropic/v1/messages"))
        .header("x-api-key", "test-key-proxy")
        .header("anthropic-version", "2023-06-01")
        .header("x-memory-mode", "full")
        .json(&serde_json::json!({
            "model": "claude-3-5-haiku-latest",
            "max_tokens": 16,
            "messages": [{"role": "user", "content": "Summarize the current deployment policy."}]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(proxy_resp.status(), 200);
    let proxy_body: serde_json::Value = proxy_resp.json().await.unwrap();
    assert_eq!(
        proxy_body["content"][0]["text"].as_str(),
        Some("dummy anthropic output")
    );

    let stats = wait_for_proxy_facts_extracted(&client, &base, "test-key-proxy", 1).await;
    assert_eq!(stats["metrics"]["facts_extracted"].as_u64(), Some(1));

    let export_resp = client
        .get(format!("{base}/v1/export"))
        .header("Authorization", "Bearer test-key-integration")
        .send()
        .await
        .unwrap();
    assert_eq!(export_resp.status(), 200);
    let export_body: serde_json::Value = export_resp.json().await.unwrap();
    let memories = export_body["memories"]
        .as_array()
        .expect("memories missing");
    let extracted = memories
        .iter()
        .find(|memory| {
            memory["content"].as_str()
                == Some("Production deploys do not require staging approval before rollout.")
        })
        .expect("proxy-extracted contradictory memory missing");
    let second_id = extracted["id"]
        .as_str()
        .expect("proxy-extracted memory id missing");

    assert_contested_pair(&client, &base, "test-key-integration", &first_id, second_id).await;

    let requests = upstream_state.requests.lock().unwrap().clone();
    assert!(
        requests
            .iter()
            .any(|request| { request["body"]["model"].as_str() == Some("claude-test-extract") }),
        "dummy upstream should receive a dedicated extraction request"
    );

    child.kill().await.ok();
    upstream_handle.abort();
}

#[tokio::test]
async fn test_proxy_extraction_repeated_signals_promote_candidate_and_track_reuse() {
    let (upstream_port, _upstream_state, upstream_handle) = start_dummy_upstream().await;

    let port = free_port();
    let tmp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let data_dir = tmp_dir.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();

    let auth_entries = r#"
[[auth.api_keys]]
key = "test-key-integration"
role = "admin"
namespace = "test"
"#;
    let extra_sections = format!(
        r#"
[proxy]
enabled = true
passthrough_auth = false
anthropic_upstream_url = "http://127.0.0.1:{upstream_port}/v1/messages"
anthropic_api_key = "anthropic-upstream-key"
default_memory_mode = "full"
extraction_enabled = true
extract_provider = "claude"
extract_model = "claude-test-promote"

[[proxy.key_mapping]]
proxy_key = "test-key-proxy"
namespace = "test"
"#
    );
    let config_content = test_config_with_sections(
        port,
        data_dir.to_str().unwrap(),
        auth_entries,
        &extra_sections,
    );
    let config_path = tmp_dir.path().join("proxy-promotion.toml");
    std::fs::write(&config_path, &config_content).unwrap();

    let mut child = start_server(config_path.to_str().unwrap()).await;
    let client = test_client();
    let base = format!("https://127.0.0.1:{port}");

    let first_proxy = client
        .post(format!("{base}/proxy/anthropic/v1/messages"))
        .header("x-api-key", "test-key-proxy")
        .header("anthropic-version", "2023-06-01")
        .header("x-memory-mode", "full")
        .json(&serde_json::json!({
            "model": "claude-3-5-haiku-latest",
            "max_tokens": 16,
            "messages": [{"role": "user", "content": "Summarize the rollout checklist."}]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(first_proxy.status(), 200);
    wait_for_proxy_facts_extracted(&client, &base, "test-key-proxy", 1).await;

    let lifecycle_resp = client
        .get(format!(
            "{base}/v1/admin/lifecycle?status=candidate&limit=10"
        ))
        .header("Authorization", "Bearer test-key-integration")
        .send()
        .await
        .unwrap();
    assert_eq!(lifecycle_resp.status(), 200);
    let lifecycle_body: serde_json::Value = lifecycle_resp.json().await.unwrap();
    let candidate = lifecycle_body["memories"]
        .as_array()
        .unwrap()
        .iter()
        .find(|memory| {
            memory["content"].as_str()
                == Some(
                    "Promotion fact alpha: use the rollout checklist before every production release.",
                )
        })
        .cloned()
        .expect("expected extracted candidate memory");
    let memory_id = candidate["id"].as_str().unwrap().to_string();
    assert_eq!(candidate["status"].as_str(), Some("candidate"));
    assert_eq!(candidate["reuse_count"].as_u64(), Some(0));

    let second_proxy = client
        .post(format!("{base}/proxy/anthropic/v1/messages"))
        .header("x-api-key", "test-key-proxy")
        .header("anthropic-version", "2023-06-01")
        .header("x-memory-mode", "full")
        .json(&serde_json::json!({
            "model": "claude-3-5-haiku-latest",
            "max_tokens": 16,
            "messages": [{"role": "user", "content": "Repeat the rollout checklist."}]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(second_proxy.status(), 200);
    wait_for_proxy_facts_extracted(&client, &base, "test-key-proxy", 2).await;

    let promoted = inspect_memory(&client, &base, "test-key-integration", &memory_id).await;
    assert_eq!(promoted["status"].as_str(), Some("active"));
    assert_eq!(promoted["reuse_count"].as_u64(), Some(1));
    assert!(promoted["last_reused_at"].as_str().is_some());
    assert!(promoted["evidence_count"].as_u64().unwrap_or(0) >= 1);

    child.kill().await.ok();
    upstream_handle.abort();
}

#[tokio::test]
async fn test_local_memory_coprocessor_extracts_without_remote_extraction_and_fails_closed() {
    let (upstream_port, upstream_state, upstream_handle) = start_dummy_upstream().await;

    let port = free_port();
    let tmp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let data_dir = tmp_dir.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();

    let auth_entries = r#"
[[auth.api_keys]]
key = "test-key-integration"
role = "admin"
namespace = "test"
"#;
    let extra_sections = format!(
        r#"
[proxy]
enabled = true
passthrough_auth = false
anthropic_upstream_url = "http://127.0.0.1:{upstream_port}/v1/messages"
anthropic_api_key = "anthropic-upstream-key"
default_memory_mode = "full"
extraction_enabled = true
memory_coprocessor = "local_heuristic"

[[proxy.key_mapping]]
proxy_key = "test-key-proxy"
namespace = "test"
"#
    );
    let config_content = test_config_with_sections(
        port,
        data_dir.to_str().unwrap(),
        auth_entries,
        &extra_sections,
    );
    let config_path = tmp_dir.path().join("proxy-local-coprocessor.toml");
    std::fs::write(&config_path, &config_content).unwrap();

    let mut child = start_server(config_path.to_str().unwrap()).await;
    let client = test_client();
    let base = format!("https://127.0.0.1:{port}");

    let first_proxy = client
        .post(format!("{base}/proxy/anthropic/v1/messages"))
        .header("x-api-key", "test-key-proxy")
        .header("anthropic-version", "2023-06-01")
        .header("x-memory-mode", "full")
        .json(&serde_json::json!({
            "model": "claude-3-5-haiku-latest",
            "max_tokens": 16,
            "messages": [{
                "role": "user",
                "content": "Remember that staging approval is required before production deploys."
            }]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(first_proxy.status(), 200);

    let stats = wait_for_proxy_facts_extracted(&client, &base, "test-key-proxy", 1).await;
    assert_eq!(
        stats["memory_coprocessor"].as_str(),
        Some("local_heuristic")
    );
    assert_eq!(stats["metrics"]["facts_extracted"].as_u64(), Some(1));

    let second_proxy = client
        .post(format!("{base}/proxy/anthropic/v1/messages"))
        .header("x-api-key", "test-key-proxy")
        .header("anthropic-version", "2023-06-01")
        .header("x-memory-mode", "full")
        .json(&serde_json::json!({
            "model": "claude-3-5-haiku-latest",
            "max_tokens": 16,
            "messages": [{
                "role": "user",
                "content": "Thanks, that was enough."
            }]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(second_proxy.status(), 200);

    tokio::time::sleep(Duration::from_secs(2)).await;
    let stable_stats = client
        .get(format!("{base}/proxy/v1/debug/stats"))
        .header("Authorization", "Bearer test-key-proxy")
        .send()
        .await
        .unwrap();
    assert_eq!(stable_stats.status(), 200);
    let stable_body: serde_json::Value = stable_stats.json().await.unwrap();
    assert_eq!(stable_body["metrics"]["facts_extracted"].as_u64(), Some(1));

    let export_resp = client
        .get(format!("{base}/v1/export"))
        .header("Authorization", "Bearer test-key-integration")
        .send()
        .await
        .unwrap();
    assert_eq!(export_resp.status(), 200);
    let export_body: serde_json::Value = export_resp.json().await.unwrap();
    let memories = export_body["memories"]
        .as_array()
        .expect("memories missing");
    let extracted = memories
        .iter()
        .find(|memory| {
            memory["content"].as_str()
                == Some("For this project, staging approval is required before production deploys.")
        })
        .expect("proxy-coprocessor memory missing");
    assert_eq!(
        extracted["source_key_id"].as_str(),
        Some("proxy-coprocessor")
    );
    assert!(extracted["tags"].as_array().is_some_and(|tags| {
        tags.iter()
            .any(|tag| tag.as_str() == Some("proxy-coprocessed"))
    }));

    let requests = upstream_state.requests.lock().unwrap().clone();
    assert_eq!(
        requests
            .iter()
            .filter(|request| request["path"].as_str() == Some("/v1/messages"))
            .count(),
        2,
        "local coprocessor should avoid an extra extraction-model call"
    );
    assert!(
        requests.iter().all(|request| {
            request["body"]["model"].as_str() != Some("claude-test-extract")
                && request["body"]["model"].as_str() != Some("claude-test-promote")
        }),
        "local coprocessor should not invoke a remote extraction model"
    );

    child.kill().await.ok();
    upstream_handle.abort();
}

#[tokio::test]
async fn test_repeated_unused_injection_drives_memory_stale() {
    let (upstream_port, _upstream_state, upstream_handle) = start_dummy_upstream().await;

    let port = free_port();
    let tmp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let data_dir = tmp_dir.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();

    let auth_entries = r#"
[[auth.api_keys]]
key = "test-key-integration"
role = "admin"
namespace = "test"
"#;
    let extra_sections = format!(
        r#"
[proxy]
enabled = true
passthrough_auth = false
anthropic_upstream_url = "http://127.0.0.1:{upstream_port}/v1/messages"
anthropic_api_key = "anthropic-upstream-key"
default_memory_mode = "full"
extraction_enabled = false

[[proxy.key_mapping]]
proxy_key = "test-key-proxy"
namespace = "test"

[decay]
after_days = 0
enabled = false
"#
    );
    let config_content = test_config_with_sections(
        port,
        data_dir.to_str().unwrap(),
        auth_entries,
        &extra_sections,
    );
    let config_path = tmp_dir.path().join("proxy-stale.toml");
    std::fs::write(&config_path, &config_content).unwrap();

    let mut child = start_server(config_path.to_str().unwrap()).await;
    let client = test_client();
    let base = format!("https://127.0.0.1:{port}");

    let store_resp = client
        .post(format!("{base}/v1/store"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({
            "content": "Lifecycle marker low-relevance alpha: check staging cluster health before rollout.",
            "tags": ["deploy", "checklist"]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(store_resp.status(), 200);
    let memory_id = store_resp.json::<serde_json::Value>().await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    tokio::time::sleep(Duration::from_secs(3)).await;

    for _ in 0..3 {
        let proxy_resp = client
            .post(format!("{base}/proxy/anthropic/v1/messages"))
            .header("x-api-key", "test-key-proxy")
            .header("anthropic-version", "2023-06-01")
            .header("x-memory-mode", "full")
            .json(&serde_json::json!({
                "model": "claude-3-5-haiku-latest",
                "max_tokens": 32,
                "messages": [{
                    "role": "user",
                    "content": "Lifecycle marker low-relevance alpha: check staging cluster health before rollout."
                }]
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(proxy_resp.status(), 200);
    }

    let stale = inspect_memory(&client, &base, "test-key-integration", &memory_id).await;
    assert_eq!(stale["status"].as_str(), Some("stale"));
    assert_eq!(stale["injection_count"].as_u64(), Some(3));
    assert_eq!(stale["reuse_count"].as_u64(), Some(0));
    assert_eq!(stale["eligible_for_injection"].as_bool(), Some(false));
    assert!(
        stale["trust_signals"]["outcome_learning"]
            .as_f64()
            .is_some(),
        "inspect should expose outcome trust signals after lifecycle decay"
    );

    child.kill().await.ok();
    upstream_handle.abort();
}

#[tokio::test]
async fn test_admin_recent_groups_injections_extractions_feedbacks_and_consolidations() {
    let (upstream_port, _upstream_state, upstream_handle) = start_dummy_upstream().await;

    let port = free_port();
    let tmp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let data_dir = tmp_dir.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();

    let auth_entries = r#"
[[auth.api_keys]]
key = "test-key-integration"
role = "admin"
namespace = "test"
"#;
    let extra_sections = format!(
        r#"
[proxy]
enabled = true
passthrough_auth = false
anthropic_upstream_url = "http://127.0.0.1:{upstream_port}/v1/messages"
anthropic_api_key = "anthropic-upstream-key"
default_memory_mode = "full"
extraction_enabled = true
extract_provider = "claude"
extract_model = "claude-test-extract"

[[proxy.key_mapping]]
proxy_key = "test-key-proxy"
namespace = "test"
"#
    );
    let config_content = test_config_with_sections(
        port,
        data_dir.to_str().unwrap(),
        auth_entries,
        &extra_sections,
    );
    let config_path = tmp_dir.path().join("recent-activity.toml");
    std::fs::write(&config_path, &config_content).unwrap();

    let mut child = start_server(config_path.to_str().unwrap()).await;
    let client = test_client();
    let base = format!("https://127.0.0.1:{port}");

    let injection_store = client
        .post(format!("{base}/v1/store"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({
            "content": "Release checklist: check staging cluster health before rollout.",
            "tags": ["deploy", "checklist", "recent"]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(injection_store.status(), 200);
    let injection_id = injection_store.json::<serde_json::Value>().await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    for content in [
        "Consolidation recent note: rotate gateway certificates before deploy.",
        "Consolidation recent note: rotate gateway certs before deploy and notify ops.",
    ] {
        let resp = client
            .post(format!("{base}/v1/store"))
            .header("Authorization", "Bearer test-key-integration")
            .json(&serde_json::json!({
                "content": content,
                "tags": ["consolidation", "recent"]
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
    }

    tokio::time::sleep(Duration::from_secs(3)).await;

    let proxy_resp = client
        .post(format!("{base}/proxy/anthropic/v1/messages"))
        .header("x-api-key", "test-key-proxy")
        .header("anthropic-version", "2023-06-01")
        .header("x-memory-mode", "full")
        .json(&serde_json::json!({
            "model": "claude-3-5-haiku-latest",
            "max_tokens": 32,
            "messages": [{
                "role": "user",
                "content": "Release checklist: check staging cluster health before rollout."
            }]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(proxy_resp.status(), 200);
    wait_for_proxy_facts_extracted(&client, &base, "test-key-proxy", 1).await;

    let feedback_resp = client
        .post(format!("{base}/v1/feedback"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({
            "id": injection_id,
            "action": "confirm"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(feedback_resp.status(), 200);

    let consolidate_resp = client
        .post(format!("{base}/v1/consolidate"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({
            "threshold": 0.9,
            "max_clusters": 10,
            "dry_run": false
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(consolidate_resp.status(), 200);
    let consolidate_body: serde_json::Value = consolidate_resp.json().await.unwrap();
    assert_eq!(consolidate_body["derived_created"].as_u64(), Some(1));

    let recent_resp = client
        .get(format!("{base}/v1/admin/recent?limit=5"))
        .header("Authorization", "Bearer test-key-integration")
        .send()
        .await
        .unwrap();
    assert_eq!(recent_resp.status(), 200);
    let recent_body: serde_json::Value = recent_resp.json().await.unwrap();
    let counts = &recent_body["recent"]["counts"];
    assert!(
        counts["injections"].as_u64().unwrap_or(0) >= 1,
        "recent should surface at least one injection"
    );
    assert!(
        counts["extractions"].as_u64().unwrap_or(0) >= 1,
        "recent should surface at least one extraction"
    );
    assert!(
        counts["feedbacks"].as_u64().unwrap_or(0) >= 1,
        "recent should surface at least one feedback"
    );
    assert!(
        counts["consolidations"].as_u64().unwrap_or(0) >= 1,
        "recent should surface at least one consolidation"
    );

    child.kill().await.ok();
    upstream_handle.abort();
}

#[tokio::test]
async fn test_auth_rejected_without_key() {
    let port = free_port();
    let tmp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let data_dir = tmp_dir.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();

    let config_content = test_config(port, data_dir.to_str().unwrap());
    let config_path = tmp_dir.path().join("test.toml");
    std::fs::write(&config_path, &config_content).unwrap();

    let mut child = start_server(config_path.to_str().unwrap()).await;

    let client = reqwest::Client::builder()
        .danger_accept_invalid_certs(true)
        .build()
        .unwrap();
    let base = format!("https://127.0.0.1:{port}");

    // Store without auth → 401
    let resp = client
        .post(format!("{base}/v1/store"))
        .json(&serde_json::json!({
            "content": "should fail",
            "agent": "test"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401, "expected 401 without auth");

    // Store with wrong key → 401
    let resp2 = client
        .post(format!("{base}/v1/store"))
        .header("Authorization", "Bearer wrong-key")
        .json(&serde_json::json!({
            "content": "should fail",
            "agent": "test"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp2.status(), 401, "expected 401 with wrong key");

    child.kill().await.ok();
}

// ============================================================================
// TEST 3: MCP HTTP Roundtrip via JSON-RPC over stdio
// ============================================================================

/// Send a JSON-RPC request to the MCP subprocess via stdin, read response from stdout.
fn jsonrpc_request(
    stdin: &mut std::process::ChildStdin,
    stdout: &mut BufReader<std::process::ChildStdout>,
    id: u64,
    method: &str,
    params: serde_json::Value,
) -> serde_json::Value {
    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
        "params": params,
    });
    let mut line = serde_json::to_string(&request).unwrap();
    line.push('\n');
    stdin.write_all(line.as_bytes()).unwrap();
    stdin.flush().unwrap();

    // Read lines, skipping non-JSON (rmcp tracing output leaks to stdout)
    loop {
        let mut response_line = String::new();
        stdout.read_line(&mut response_line).unwrap();
        let trimmed = response_line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Ok(val) = serde_json::from_str::<serde_json::Value>(trimmed) {
            return val;
        }
        // Skip non-JSON lines (tracing logs)
    }
}

/// Send a JSON-RPC notification (no id, no response expected).
fn jsonrpc_notify(stdin: &mut std::process::ChildStdin, method: &str, params: serde_json::Value) {
    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "method": method,
        "params": params,
    });
    let mut line = serde_json::to_string(&request).unwrap();
    line.push('\n');
    stdin.write_all(line.as_bytes()).unwrap();
    stdin.flush().unwrap();
}

#[tokio::test]
async fn test_mcp_http_roundtrip() {
    let port = free_port();
    let tmp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let data_dir = tmp_dir.path().join("mcp_data");
    std::fs::create_dir_all(&data_dir).unwrap();

    let config_content = test_config(port, data_dir.to_str().unwrap());
    let config_path = tmp_dir.path().join("mcp_test.toml");
    std::fs::write(&config_path, &config_content).unwrap();

    // MCP is now an HTTP client — start the HTTP server first
    let mut server_child = start_server(config_path.to_str().unwrap()).await;

    // Then start the MCP process which delegates to the HTTP server
    let mut mcp_child = std::process::Command::new(env!("CARGO_BIN_EXE_memoryoss"))
        .args(["--config", config_path.to_str().unwrap(), "mcp-server"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("failed to start MCP server");

    let mut mcp_stdin = mcp_child.stdin.take().unwrap();
    let mut mcp_stdout = BufReader::new(mcp_child.stdout.take().unwrap());

    // Give MCP time to connect to the HTTP server
    tokio::time::sleep(Duration::from_secs(3)).await;

    // 1. Initialize handshake
    let init_resp = jsonrpc_request(
        &mut mcp_stdin,
        &mut mcp_stdout,
        1,
        "initialize",
        serde_json::json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": { "name": "test-client", "version": "1.0" }
        }),
    );
    assert!(
        init_resp["result"]["serverInfo"]["name"].as_str().is_some(),
        "MCP initialize failed: {init_resp}"
    );

    // Send initialized notification
    jsonrpc_notify(
        &mut mcp_stdin,
        "notifications/initialized",
        serde_json::json!({}),
    );

    // 2. List tools
    let tools_resp = jsonrpc_request(
        &mut mcp_stdin,
        &mut mcp_stdout,
        2,
        "tools/list",
        serde_json::json!({}),
    );
    let tools = tools_resp["result"]["tools"]
        .as_array()
        .expect("no tools array");
    let tool_names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
    assert!(
        tool_names.contains(&"memoryoss_store"),
        "missing store tool"
    );
    assert!(
        tool_names.contains(&"memoryoss_recall"),
        "missing recall tool"
    );
    assert!(
        tool_names.contains(&"memoryoss_update"),
        "missing update tool"
    );
    assert!(
        tool_names.contains(&"memoryoss_forget"),
        "missing forget tool"
    );
    let live_has_marketplace_annotations = tools.iter().any(|tool| {
        tool.get("title").is_some()
            || tool
                .get("annotations")
                .and_then(serde_json::Value::as_object)
                .is_some()
    });
    if !live_has_marketplace_annotations {
        let server_manifest: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string("server.json").unwrap()).unwrap();
        let manifest_tools = server_manifest["_meta"]["io.github.memoryOSScom/anthropic-local-mcp"]
            ["toolAnnotations"]
            .as_array()
            .expect("server manifest missing MCP tool annotations");
        for name in &tool_names {
            let manifest_entry = manifest_tools
                .iter()
                .find(|entry| entry["name"].as_str() == Some(*name))
                .expect("missing fallback tool annotation entry");
            assert!(
                manifest_entry["title"].as_str().is_some(),
                "fallback manifest must carry tool titles"
            );
            let annotations = manifest_entry["annotations"]
                .as_object()
                .expect("fallback manifest must carry annotations");
            assert!(annotations.contains_key("readOnlyHint"));
            assert!(annotations.contains_key("destructiveHint"));
        }
    }

    // 3. Store a memory via MCP
    let store_resp = jsonrpc_request(
        &mut mcp_stdin,
        &mut mcp_stdout,
        3,
        "tools/call",
        serde_json::json!({
            "name": "memoryoss_store",
            "arguments": {
                "content": "MCP test: the capital of France is Paris.",
                "tags": ["mcp-test", "geography"],
                "agent": "mcp-test-agent"
            }
        }),
    );
    assert!(
        store_resp["error"].is_null(),
        "MCP store failed: {store_resp}"
    );
    // Extract memory ID from store response
    let store_text = store_resp["result"]["content"][0]["text"].as_str().unwrap();
    let store_json: serde_json::Value = serde_json::from_str(store_text).unwrap();
    let memory_id = store_json["id"]
        .as_str()
        .expect("no id in MCP store response");

    // Wait for async indexer
    tokio::time::sleep(Duration::from_secs(3)).await;

    // 4. Recall via MCP
    let recall_resp = jsonrpc_request(
        &mut mcp_stdin,
        &mut mcp_stdout,
        4,
        "tools/call",
        serde_json::json!({
            "name": "memoryoss_recall",
            "arguments": {
                "query": "capital of France"
            }
        }),
    );
    assert!(
        recall_resp["error"].is_null(),
        "MCP recall failed: {recall_resp}"
    );
    let recall_text = recall_resp["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    let recall_json: serde_json::Value = serde_json::from_str(recall_text).unwrap();
    let memories = recall_json.as_array().expect("recall should return array");
    assert!(!memories.is_empty(), "MCP recall returned no memories");
    assert!(
        memories.iter().any(|m| m["id"].as_str() == Some(memory_id)),
        "stored memory not found via MCP recall"
    );

    // 5. Update via MCP
    let update_resp = jsonrpc_request(
        &mut mcp_stdin,
        &mut mcp_stdout,
        5,
        "tools/call",
        serde_json::json!({
            "name": "memoryoss_update",
            "arguments": {
                "id": memory_id,
                "content": "MCP updated: Paris is the capital and largest city of France."
            }
        }),
    );
    assert!(
        update_resp["error"].is_null(),
        "MCP update failed: {update_resp}"
    );

    tokio::time::sleep(Duration::from_secs(3)).await;

    // 6. Recall again — verify update
    let recall2_resp = jsonrpc_request(
        &mut mcp_stdin,
        &mut mcp_stdout,
        6,
        "tools/call",
        serde_json::json!({
            "name": "memoryoss_recall",
            "arguments": { "query": "Paris capital largest city" }
        }),
    );
    let recall2_text = recall2_resp["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    assert!(
        recall2_text.contains("largest city"),
        "MCP update not reflected in recall"
    );

    // 7. Forget via MCP
    let forget_resp = jsonrpc_request(
        &mut mcp_stdin,
        &mut mcp_stdout,
        7,
        "tools/call",
        serde_json::json!({
            "name": "memoryoss_forget",
            "arguments": { "ids": [memory_id] }
        }),
    );
    assert!(
        forget_resp["error"].is_null(),
        "MCP forget failed: {forget_resp}"
    );
    let forget_text = forget_resp["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    assert!(
        forget_text.contains("\"deleted\":1") || forget_text.contains("\"deleted\": 1"),
        "MCP forget did not delete: {forget_text}"
    );

    // Cleanup
    mcp_child.kill().ok();
    server_child.kill().await.ok();
}

#[tokio::test]
async fn test_mcp_unknown_tool() {
    let port = free_port();
    let tmp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let data_dir = tmp_dir.path().join("mcp_unknown_data");
    std::fs::create_dir_all(&data_dir).unwrap();

    let config_content = test_config(port, data_dir.to_str().unwrap());
    let config_path = tmp_dir.path().join("mcp_unknown.toml");
    std::fs::write(&config_path, &config_content).unwrap();

    // MCP is embedded — no HTTP server needed
    let mut mcp_child = std::process::Command::new(env!("CARGO_BIN_EXE_memoryoss"))
        .args(["--config", config_path.to_str().unwrap(), "mcp-server"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("failed to start MCP server");

    let mut mcp_stdin = mcp_child.stdin.take().unwrap();
    let mut mcp_stdout = BufReader::new(mcp_child.stdout.take().unwrap());

    tokio::time::sleep(Duration::from_secs(3)).await;

    // Initialize
    let _init = jsonrpc_request(
        &mut mcp_stdin,
        &mut mcp_stdout,
        1,
        "initialize",
        serde_json::json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": { "name": "test-client", "version": "1.0" }
        }),
    );
    jsonrpc_notify(
        &mut mcp_stdin,
        "notifications/initialized",
        serde_json::json!({}),
    );

    // Call unknown tool
    let resp = jsonrpc_request(
        &mut mcp_stdin,
        &mut mcp_stdout,
        2,
        "tools/call",
        serde_json::json!({
            "name": "nonexistent_tool",
            "arguments": {}
        }),
    );

    // Should return an error
    assert!(
        resp["error"].is_object(),
        "unknown tool should return JSON-RPC error: {resp}"
    );

    mcp_child.kill().ok();
}

// ============================================================================
// TEST 6: Concurrent access — multiple clients storing/recalling simultaneously
// ============================================================================

#[tokio::test]
async fn test_concurrent_access() {
    let port = free_port();
    let tmp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let data_dir = tmp_dir.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();

    let config_content = test_config(port, data_dir.to_str().unwrap());
    let config_path = tmp_dir.path().join("test.toml");
    std::fs::write(&config_path, &config_content).unwrap();

    let mut child = start_server(config_path.to_str().unwrap()).await;

    let client = reqwest::Client::builder()
        .danger_accept_invalid_certs(true)
        .build()
        .unwrap();
    let base = format!("https://127.0.0.1:{port}");

    // Semantically diverse content to avoid duplicate detection (similarity >= 0.95)
    let topics = [
        "The Eiffel Tower in Paris was completed in 1889 for the World's Fair.",
        "Photosynthesis converts sunlight into chemical energy in plant chloroplasts.",
        "The Rust programming language emphasizes memory safety without garbage collection.",
        "Jupiter is the largest planet in our solar system with 95 known moons.",
        "The TCP/IP protocol suite forms the foundation of modern internet communication.",
        "Mozart composed over 600 works during his short life of 35 years.",
        "The mitochondria generate ATP through oxidative phosphorylation in eukaryotic cells.",
        "The Great Wall of China stretches over 21,000 kilometers across northern China.",
        "Machine learning models use gradient descent to minimize their loss functions.",
        "The Amazon River carries more water than any other river system on Earth.",
    ];

    // Fire 10 concurrent store requests with diverse content
    let mut handles = Vec::new();
    for (i, topic) in topics.iter().enumerate() {
        let c = client.clone();
        let b = base.clone();
        let content = topic.to_string();
        handles.push(tokio::spawn(async move {
            let resp = c
                .post(format!("{b}/v1/store"))
                .header("Authorization", "Bearer test-key-integration")
                .json(&serde_json::json!({
                    "content": content,
                    "agent": "concurrency-test",
                    "tags": ["concurrent", format!("topic-{i}")]
                }))
                .send()
                .await
                .unwrap();
            assert_eq!(resp.status(), 200, "concurrent store #{i} failed");
            let body: serde_json::Value = resp.json().await.unwrap();
            body["id"].as_str().unwrap().to_string()
        }));
    }

    let mut ids = Vec::new();
    for h in handles {
        ids.push(h.await.unwrap());
    }
    assert_eq!(ids.len(), 10, "not all concurrent stores completed");

    // Wait for indexer
    tokio::time::sleep(Duration::from_secs(5)).await;

    // Fire 10 concurrent recall requests
    let mut recall_handles = Vec::new();
    for i in 0..10 {
        let c = client.clone();
        let b = base.clone();
        recall_handles.push(tokio::spawn(async move {
            let resp = c
                .post(format!("{b}/v1/recall"))
                .header("Authorization", "Bearer test-key-integration")
                .json(&serde_json::json!({
                    "query": format!("concurrent memory #{i}")
                }))
                .send()
                .await
                .unwrap();
            assert_eq!(resp.status(), 200, "concurrent recall #{i} failed");
        }));
    }

    for h in recall_handles {
        h.await.unwrap();
    }

    // Cleanup: forget all
    for id in &ids {
        client
            .delete(format!("{base}/v1/forget"))
            .header("Authorization", "Bearer test-key-integration")
            .json(&serde_json::json!({ "id": id }))
            .send()
            .await
            .unwrap();
    }

    child.kill().await.ok();
}

// ============================================================================
// TEST 7: Proxy returns error for invalid upstream (no real LLM API key)
// ============================================================================

#[tokio::test]
async fn test_proxy_error_without_upstream() {
    let port = free_port();
    let tmp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let data_dir = tmp_dir.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();

    let config_content = format!(
        r#"
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

[[auth.api_keys]]
key = "test-key-proxy"
role = "admin"
namespace = "test"

[proxy]
enabled = true
passthrough_auth = true
upstream_url = "https://api.openai.com/v1"

[[proxy.key_mapping]]
proxy_key = "test-key-proxy"
namespace = "test"

[logging]
level = "warn"
"#,
        data_dir = data_dir.to_str().unwrap(),
    );
    let config_path = tmp_dir.path().join("test.toml");
    std::fs::write(&config_path, &config_content).unwrap();

    let mut child = start_server(config_path.to_str().unwrap()).await;

    let client = reqwest::Client::builder()
        .danger_accept_invalid_certs(true)
        .build()
        .unwrap();
    let base = format!("https://127.0.0.1:{port}");

    // Call proxy endpoint with a fake API key — upstream should reject it
    let resp = client
        .post(format!("{base}/proxy/v1/chat/completions"))
        .header("Authorization", "Bearer sk-fake-not-real-key")
        .json(&serde_json::json!({
            "model": "gpt-4o-mini",
            "messages": [{"role": "user", "content": "test"}]
        }))
        .send()
        .await
        .unwrap();

    // Proxy should forward the upstream error (401 from OpenAI) or return its own error
    // It should NOT panic or return 500
    let status = resp.status().as_u16();
    assert!(
        status == 401 || status == 403 || status == 502 || status == 200,
        "proxy should handle upstream errors gracefully, got {status}"
    );

    child.kill().await.ok();
}

#[tokio::test]
async fn test_proxy_connection_paths_cover_openai_and_anthropic() {
    let (upstream_port, upstream_state, upstream_handle) = start_dummy_upstream().await;

    let port = free_port();
    let tmp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let data_dir = tmp_dir.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();

    let auth_entries = r#"
[[auth.api_keys]]
key = "test-key-proxy"
role = "admin"
namespace = "test"
"#;
    let extra_sections = format!(
        r#"
[proxy]
enabled = true
passthrough_auth = true
upstream_url = "http://127.0.0.1:{upstream_port}/v1"
upstream_api_key = "upstream-openai-key"
anthropic_api_key = "anthropic-upstream-key"
anthropic_upstream_url = "http://127.0.0.1:{upstream_port}/v1/messages"
default_memory_mode = "off"
extraction_enabled = false

[[proxy.key_mapping]]
proxy_key = "test-key-proxy"
namespace = "test"
"#
    );
    let config_content = test_config_with_sections(
        port,
        data_dir.to_str().unwrap(),
        auth_entries,
        &extra_sections,
    );
    let config_path = tmp_dir.path().join("proxy.toml");
    std::fs::write(&config_path, &config_content).unwrap();

    let mut child = start_server(config_path.to_str().unwrap()).await;
    let client = test_client();
    let base = format!("https://127.0.0.1:{port}");

    let models_resp = client
        .get(format!("{base}/proxy/v1/models"))
        .header("Authorization", "Bearer test-key-proxy")
        .send()
        .await
        .unwrap();
    assert_eq!(models_resp.status(), 200);
    let models_body: serde_json::Value = models_resp.json().await.unwrap();
    assert!(
        models_body["models"].as_array().is_some(),
        "proxy models should normalize upstream data field into models"
    );

    let chat_resp = client
        .post(format!("{base}/proxy/v1/chat/completions"))
        .header("Authorization", "Bearer test-key-proxy")
        .json(&serde_json::json!({
            "model": "gpt-4o-mini",
            "messages": [{"role": "user", "content": "hello from chat"}]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(chat_resp.status(), 200);
    let chat_body: serde_json::Value = chat_resp.json().await.unwrap();
    assert_eq!(
        chat_body["choices"][0]["message"]["content"].as_str(),
        Some("dummy chat completion")
    );

    let responses_resp = client
        .post(format!("{base}/proxy/v1/responses"))
        .header("Authorization", "Bearer test-key-proxy")
        .json(&serde_json::json!({
            "model": "gpt-4o-mini",
            "input": "hello from responses"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(responses_resp.status(), 200);
    let responses_body: serde_json::Value = responses_resp.json().await.unwrap();
    assert_eq!(
        responses_body["output"][0]["content"][0]["text"].as_str(),
        Some("dummy responses output")
    );

    let anthropic_resp = client
        .post(format!("{base}/proxy/anthropic/v1/messages"))
        .header("x-api-key", "test-key-proxy")
        .header("anthropic-version", "2023-06-01")
        .json(&serde_json::json!({
            "model": "claude-3-5-haiku-latest",
            "max_tokens": 16,
            "messages": [{"role": "user", "content": "hello from anthropic"}]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(anthropic_resp.status(), 200);
    let anthropic_body: serde_json::Value = anthropic_resp.json().await.unwrap();
    assert_eq!(
        anthropic_body["content"][0]["text"].as_str(),
        Some("dummy anthropic output")
    );

    tokio::time::sleep(Duration::from_millis(300)).await;
    let requests = upstream_state.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 4, "expected four upstream connection paths");

    let model_req = requests
        .iter()
        .find(|entry| entry["path"].as_str() == Some("/v1/models"))
        .expect("missing upstream models request");
    assert_eq!(
        model_req["authorization"].as_str(),
        Some("Bearer upstream-openai-key")
    );

    let chat_req = requests
        .iter()
        .find(|entry| entry["path"].as_str() == Some("/v1/chat/completions"))
        .expect("missing upstream chat request");
    assert_eq!(
        chat_req["authorization"].as_str(),
        Some("Bearer upstream-openai-key")
    );

    let responses_req = requests
        .iter()
        .find(|entry| entry["path"].as_str() == Some("/v1/responses"))
        .expect("missing upstream responses request");
    assert_eq!(
        responses_req["authorization"].as_str(),
        Some("Bearer upstream-openai-key")
    );

    let anthropic_req = requests
        .iter()
        .find(|entry| entry["path"].as_str() == Some("/v1/messages"))
        .expect("missing upstream anthropic request");
    assert_eq!(
        anthropic_req["x_api_key"].as_str(),
        Some("anthropic-upstream-key")
    );
    assert_eq!(
        anthropic_req["anthropic_version"].as_str(),
        Some("2023-06-01")
    );

    child.kill().await.ok();
    upstream_handle.abort();
}

#[tokio::test]
async fn test_proxy_responses_oauth_text_fallback_uses_chat_completions() {
    let (upstream_port, upstream_state, upstream_handle) = start_dummy_upstream().await;

    let port = free_port();
    let tmp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let data_dir = tmp_dir.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();

    let auth_entries = r#"
[[auth.api_keys]]
key = "test-key-proxy"
role = "admin"
namespace = "test"
"#;
    let extra_sections = format!(
        r#"
[proxy]
enabled = true
passthrough_auth = true
upstream_url = "http://127.0.0.1:{upstream_port}/v1"
upstream_api_key = "upstream-openai-key"
default_memory_mode = "off"
extraction_enabled = false

[[proxy.key_mapping]]
proxy_key = "test-key-proxy"
namespace = "test"
"#
    );
    let config_content = test_config_with_sections(
        port,
        data_dir.to_str().unwrap(),
        auth_entries,
        &extra_sections,
    );
    let config_path = tmp_dir.path().join("proxy-oauth-responses.toml");
    std::fs::write(&config_path, &config_content).unwrap();

    let mut child = start_server(config_path.to_str().unwrap()).await;
    let client = test_client();
    let base = format!("https://127.0.0.1:{port}");

    let resp = client
        .post(format!("{base}/proxy/v1/responses"))
        .header("Authorization", "Bearer eyJ-openai-oauth-token")
        .json(&serde_json::json!({
            "model": "gpt-4o-mini",
            "input": [
                {"role": "developer", "content": "Be concise"},
                {"role": "user", "content": "hello from oauth responses"}
            ]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(
        body["output"][0]["content"][0]["text"].as_str(),
        Some("dummy chat completion")
    );

    tokio::time::sleep(Duration::from_millis(300)).await;
    let requests = upstream_state.requests.lock().unwrap().clone();
    let chat_req = requests
        .iter()
        .find(|entry| {
            entry["path"].as_str() == Some("/v1/chat/completions")
                && entry["authorization"].as_str() == Some("Bearer eyJ-openai-oauth-token")
        })
        .expect("missing oauth chat/completions fallback request");
    assert_eq!(
        chat_req["body"]["messages"][0]["role"].as_str(),
        Some("system"),
        "developer role should be mapped to system for chat completions"
    );
    assert!(
        requests
            .iter()
            .all(|entry| entry["path"].as_str() != Some("/v1/responses")),
        "oauth fallback should not call upstream /v1/responses"
    );

    child.kill().await.ok();
    upstream_handle.abort();
}

#[tokio::test]
async fn test_proxy_responses_oauth_text_stream_fallback_maps_to_responses_sse() {
    let (upstream_port, _upstream_state, upstream_handle) = start_dummy_upstream().await;

    let port = free_port();
    let tmp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let data_dir = tmp_dir.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();

    let auth_entries = r#"
[[auth.api_keys]]
key = "test-key-proxy"
role = "admin"
namespace = "test"
"#;
    let extra_sections = format!(
        r#"
[proxy]
enabled = true
passthrough_auth = true
upstream_url = "http://127.0.0.1:{upstream_port}/v1"
upstream_api_key = "upstream-openai-key"
default_memory_mode = "off"
extraction_enabled = false

[[proxy.key_mapping]]
proxy_key = "test-key-proxy"
namespace = "test"
"#
    );
    let config_content = test_config_with_sections(
        port,
        data_dir.to_str().unwrap(),
        auth_entries,
        &extra_sections,
    );
    let config_path = tmp_dir.path().join("proxy-oauth-stream.toml");
    std::fs::write(&config_path, &config_content).unwrap();

    let mut child = start_server(config_path.to_str().unwrap()).await;
    let client = test_client();
    let base = format!("https://127.0.0.1:{port}");

    let resp = client
        .post(format!("{base}/proxy/v1/responses"))
        .header("Authorization", "Bearer eyJ-openai-oauth-token")
        .json(&serde_json::json!({
            "model": "gpt-4o-mini",
            "stream": true,
            "input": "hello from oauth responses stream"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();
    assert!(body.contains("response.output_text.delta"));
    assert!(body.contains("dummy "));
    assert!(body.contains("chat completion"));
    assert!(body.contains("response.completed"));

    child.kill().await.ok();
    upstream_handle.abort();
}

#[tokio::test]
async fn test_proxy_responses_oauth_tool_calls_fallback_maps_function_call_output() {
    let (upstream_port, upstream_state, upstream_handle) = start_dummy_upstream().await;

    let port = free_port();
    let tmp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let data_dir = tmp_dir.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();

    let auth_entries = r#"
[[auth.api_keys]]
key = "test-key-proxy"
role = "admin"
namespace = "test"
"#;
    let extra_sections = format!(
        r#"
[proxy]
enabled = true
passthrough_auth = true
upstream_url = "http://127.0.0.1:{upstream_port}/v1"
upstream_api_key = "upstream-openai-key"
default_memory_mode = "readonly"
extraction_enabled = false

[[proxy.key_mapping]]
proxy_key = "test-key-proxy"
namespace = "test"
"#
    );
    let config_content = test_config_with_sections(
        port,
        data_dir.to_str().unwrap(),
        auth_entries,
        &extra_sections,
    );
    let config_path = tmp_dir.path().join("proxy-oauth-tools.toml");
    std::fs::write(&config_path, &config_content).unwrap();

    let mut child = start_server(config_path.to_str().unwrap()).await;
    let client = test_client();
    let base = format!("https://127.0.0.1:{port}");

    let resp = client
        .post(format!("{base}/proxy/v1/responses"))
        .header("Authorization", "Bearer eyJ-openai-oauth-token")
        .json(&serde_json::json!({
            "model": "gpt-4o-mini",
            "tools": [{
                "type": "function",
                "function": {
                    "name": "get_weather",
                    "description": "Get weather",
                    "parameters": {
                        "type": "object",
                        "properties": {"city": {"type": "string"}},
                        "required": ["city"]
                    }
                }
            }],
            "tool_choice": "auto",
            "input": [
                {"role": "user", "content": "weather in berlin"},
                {"type": "function_call_output", "call_id": "call_prev", "output": "{\"temp\": 18}"}
            ]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["output"][0]["type"].as_str(), Some("function_call"));
    assert_eq!(
        body["output"][0]["call_id"].as_str(),
        Some("call_weather_1")
    );
    assert_eq!(body["output"][0]["name"].as_str(), Some("get_weather"));

    tokio::time::sleep(Duration::from_millis(300)).await;
    let requests = upstream_state.requests.lock().unwrap().clone();
    let chat_req = requests
        .iter()
        .find(|entry| entry["path"].as_str() == Some("/v1/chat/completions"))
        .expect("missing upstream chat/completions request");
    assert!(chat_req["body"]["tools"].is_array());
    assert_eq!(
        chat_req["body"]["messages"][1]["role"].as_str(),
        Some("tool")
    );
    assert_eq!(
        chat_req["body"]["messages"][1]["tool_call_id"].as_str(),
        Some("call_prev")
    );

    child.kill().await.ok();
    upstream_handle.abort();
}

#[tokio::test]
async fn test_proxy_responses_oauth_top_level_function_tools_map_to_chat_tools() {
    let (upstream_port, upstream_state, upstream_handle) = start_dummy_upstream().await;

    let port = free_port();
    let tmp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let data_dir = tmp_dir.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();

    let auth_entries = r#"
[[auth.api_keys]]
key = "test-key-proxy"
role = "admin"
namespace = "test"
"#;
    let extra_sections = format!(
        r#"
[proxy]
enabled = true
passthrough_auth = true
upstream_url = "http://127.0.0.1:{upstream_port}/v1"
upstream_api_key = "upstream-openai-key"
default_memory_mode = "off"
extraction_enabled = false

[[proxy.key_mapping]]
proxy_key = "test-key-proxy"
namespace = "test"
"#
    );
    let config_content = test_config_with_sections(
        port,
        data_dir.to_str().unwrap(),
        auth_entries,
        &extra_sections,
    );
    let config_path = tmp_dir.path().join("proxy-oauth-tools-top-level.toml");
    std::fs::write(&config_path, &config_content).unwrap();

    let mut child = start_server(config_path.to_str().unwrap()).await;
    let client = test_client();
    let base = format!("https://127.0.0.1:{port}");

    let resp = client
        .post(format!("{base}/proxy/v1/responses"))
        .header("Authorization", "Bearer eyJ-openai-oauth-token")
        .json(&serde_json::json!({
            "model": "gpt-4o-mini",
            "tools": [{
                "type": "function",
                "name": "get_weather",
                "description": "Get weather",
                "parameters": {
                    "type": "object",
                    "properties": {"city": {"type": "string"}},
                    "required": ["city"]
                }
            }],
            "tool_choice": {
                "type": "function",
                "name": "get_weather"
            },
            "input": "weather in berlin"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    tokio::time::sleep(Duration::from_millis(300)).await;
    let requests = upstream_state.requests.lock().unwrap().clone();
    let chat_req = requests
        .iter()
        .find(|entry| entry["path"].as_str() == Some("/v1/chat/completions"))
        .expect("missing upstream chat/completions request");
    assert_eq!(
        chat_req["body"]["tools"][0]["function"]["name"].as_str(),
        Some("get_weather")
    );
    assert_eq!(
        chat_req["body"]["tool_choice"]["function"]["name"].as_str(),
        Some("get_weather")
    );

    child.kill().await.ok();
    upstream_handle.abort();
}

#[tokio::test]
async fn test_proxy_responses_oauth_ignores_builtin_codex_tools_for_chat_fallback() {
    let (upstream_port, upstream_state, upstream_handle) = start_dummy_upstream().await;

    let port = free_port();
    let tmp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let data_dir = tmp_dir.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();

    let auth_entries = r#"
[[auth.api_keys]]
key = "test-key-proxy"
role = "admin"
namespace = "test"
"#;
    let extra_sections = format!(
        r#"
[proxy]
enabled = true
passthrough_auth = true
upstream_url = "http://127.0.0.1:{upstream_port}/v1"
upstream_api_key = "upstream-openai-key"
default_memory_mode = "off"
extraction_enabled = false

[[proxy.key_mapping]]
proxy_key = "test-key-proxy"
namespace = "test"
"#
    );
    let config_content = test_config_with_sections(
        port,
        data_dir.to_str().unwrap(),
        auth_entries,
        &extra_sections,
    );
    let config_path = tmp_dir.path().join("proxy-oauth-codex-builtins.toml");
    std::fs::write(&config_path, &config_content).unwrap();

    let mut child = start_server(config_path.to_str().unwrap()).await;
    let client = test_client();
    let base = format!("https://127.0.0.1:{port}");

    let resp = client
        .post(format!("{base}/proxy/v1/responses"))
        .header("Authorization", "Bearer eyJ-openai-oauth-token")
        .json(&serde_json::json!({
            "model": "gpt-4o-mini",
            "stream": true,
            "tools": [
                {
                    "type": "function",
                    "name": "get_weather",
                    "description": "Get weather",
                    "parameters": {
                        "type": "object",
                        "properties": {"city": {"type": "string"}},
                        "required": ["city"]
                    }
                },
                {
                    "type": "custom",
                    "name": "apply_patch",
                    "description": "Apply a patch",
                    "format": "diff"
                },
                {
                    "type": "web_search",
                    "external_web_access": true,
                    "search_content_types": ["text"]
                }
            ],
            "tool_choice": "auto",
            "input": "weather in berlin"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();
    assert!(body.contains("response.output_item.added"));
    assert!(body.contains("response.function_call_arguments.delta"));
    assert!(body.contains("response.completed"));

    tokio::time::sleep(Duration::from_millis(300)).await;
    let requests = upstream_state.requests.lock().unwrap().clone();
    let chat_req = requests
        .iter()
        .find(|entry| entry["path"].as_str() == Some("/v1/chat/completions"))
        .expect("missing upstream chat/completions request");
    let tools = chat_req["body"]["tools"]
        .as_array()
        .expect("missing chat tools");
    assert_eq!(tools.len(), 1, "only function tools should be forwarded");
    assert_eq!(tools[0]["function"]["name"].as_str(), Some("get_weather"));

    child.kill().await.ok();
    upstream_handle.abort();
}

#[tokio::test]
async fn test_proxy_responses_oauth_stream_with_tools_maps_function_call_sse() {
    let (upstream_port, _upstream_state, upstream_handle) = start_dummy_upstream().await;

    let port = free_port();
    let tmp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let data_dir = tmp_dir.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();

    let auth_entries = r#"
[[auth.api_keys]]
key = "test-key-proxy"
role = "admin"
namespace = "test"
"#;
    let extra_sections = format!(
        r#"
[proxy]
enabled = true
passthrough_auth = true
upstream_url = "http://127.0.0.1:{upstream_port}/v1"
upstream_api_key = "upstream-openai-key"
default_memory_mode = "off"
extraction_enabled = false

[[proxy.key_mapping]]
proxy_key = "test-key-proxy"
namespace = "test"
"#
    );
    let config_content = test_config_with_sections(
        port,
        data_dir.to_str().unwrap(),
        auth_entries,
        &extra_sections,
    );
    let config_path = tmp_dir.path().join("proxy-oauth-tools-stream.toml");
    std::fs::write(&config_path, &config_content).unwrap();

    let mut child = start_server(config_path.to_str().unwrap()).await;
    let client = test_client();
    let base = format!("https://127.0.0.1:{port}");

    let resp = client
        .post(format!("{base}/proxy/v1/responses"))
        .header("Authorization", "Bearer eyJ-openai-oauth-token")
        .json(&serde_json::json!({
            "model": "gpt-4o-mini",
            "stream": true,
            "tools": [{
                "type": "function",
                "function": {"name": "get_weather"}
            }],
            "input": "weather in berlin"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();
    assert!(body.contains("response.output_item.added"));
    assert!(body.contains("response.function_call_arguments.delta"));
    assert!(body.contains("response.function_call_arguments.done"));
    assert!(body.contains("response.output_item.done"));
    assert!(body.contains("call_weather_1"));
    assert!(body.contains("get_weather"));
    assert!(body.contains("Berlin"));
    assert!(body.contains("response.completed"));

    child.kill().await.ok();
    upstream_handle.abort();
}

#[tokio::test]
async fn test_proxy_responses_oauth_response_format_passthrough_maps_json_output() {
    let (upstream_port, upstream_state, upstream_handle) = start_dummy_upstream().await;

    let port = free_port();
    let tmp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let data_dir = tmp_dir.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();

    let auth_entries = r#"
[[auth.api_keys]]
key = "test-key-proxy"
role = "admin"
namespace = "test"
"#;
    let extra_sections = format!(
        r#"
[proxy]
enabled = true
passthrough_auth = true
upstream_url = "http://127.0.0.1:{upstream_port}/v1"
upstream_api_key = "upstream-openai-key"
default_memory_mode = "off"
extraction_enabled = false

[[proxy.key_mapping]]
proxy_key = "test-key-proxy"
namespace = "test"
"#
    );
    let config_content = test_config_with_sections(
        port,
        data_dir.to_str().unwrap(),
        auth_entries,
        &extra_sections,
    );
    let config_path = tmp_dir.path().join("proxy-oauth-json.toml");
    std::fs::write(&config_path, &config_content).unwrap();

    let mut child = start_server(config_path.to_str().unwrap()).await;
    let client = test_client();
    let base = format!("https://127.0.0.1:{port}");

    let resp = client
        .post(format!("{base}/proxy/v1/responses"))
        .header("Authorization", "Bearer eyJ-openai-oauth-token")
        .json(&serde_json::json!({
            "model": "gpt-4o-mini",
            "response_format": {
                "type": "json_object"
            },
            "input": "return a json object"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(
        body["output"][0]["content"][0]["text"].as_str(),
        Some("{\"ok\":true,\"city\":\"Berlin\"}")
    );

    tokio::time::sleep(Duration::from_millis(300)).await;
    let requests = upstream_state.requests.lock().unwrap().clone();
    let chat_req = requests
        .iter()
        .find(|entry| entry["path"].as_str() == Some("/v1/chat/completions"))
        .expect("missing upstream chat/completions request");
    assert_eq!(
        chat_req["body"]["response_format"]["type"].as_str(),
        Some("json_object")
    );

    child.kill().await.ok();
    upstream_handle.abort();
}

#[tokio::test]
async fn test_proxy_responses_oauth_without_required_openai_scopes_fails_closed() {
    let (upstream_port, upstream_state, upstream_handle) = start_dummy_upstream().await;

    let port = free_port();
    let tmp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let data_dir = tmp_dir.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();

    let auth_entries = r#"
[[auth.api_keys]]
key = "test-key-proxy"
role = "admin"
namespace = "test"
"#;
    let extra_sections = format!(
        r#"
[proxy]
enabled = true
passthrough_auth = true
upstream_url = "http://127.0.0.1:{upstream_port}/v1"
upstream_api_key = "upstream-openai-key"
default_memory_mode = "off"
extraction_enabled = false

[[proxy.key_mapping]]
proxy_key = "test-key-proxy"
namespace = "test"
"#
    );
    let config_content = test_config_with_sections(
        port,
        data_dir.to_str().unwrap(),
        auth_entries,
        &extra_sections,
    );
    let config_path = tmp_dir.path().join("proxy-oauth-scope-fail-closed.toml");
    std::fs::write(&config_path, &config_content).unwrap();

    let mut child = start_server(config_path.to_str().unwrap()).await;
    let client = test_client();
    let base = format!("https://127.0.0.1:{port}");
    let insufficient_scope_token = "eyJhbGciOiJub25lIn0.eyJzY3AiOlsib3BlbmlkIiwicHJvZmlsZSIsImVtYWlsIiwib2ZmbGluZV9hY2Nlc3MiXX0.sig";

    let resp = client
        .post(format!("{base}/proxy/v1/responses"))
        .header(
            "Authorization",
            format!("Bearer {insufficient_scope_token}"),
        )
        .json(&serde_json::json!({
            "model": "gpt-4o-mini",
            "input": "hello"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap_or("")
            .contains("Use Codex via MCP")
    );

    tokio::time::sleep(Duration::from_millis(200)).await;
    let requests = upstream_state.requests.lock().unwrap().clone();
    assert!(
        requests.is_empty(),
        "scope-fail-closed should not forward anything upstream"
    );

    child.kill().await.ok();
    upstream_handle.abort();
}

#[tokio::test]
async fn test_proxy_passthrough_is_local_only_by_default() {
    let (upstream_port, _upstream_state, upstream_handle) = start_dummy_upstream().await;

    let port = free_port();
    let tmp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let data_dir = tmp_dir.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();

    let auth_entries = r#"
[[auth.api_keys]]
key = "test-key-proxy"
role = "admin"
namespace = "test"
"#;
    let extra_sections = format!(
        r#"
[proxy]
enabled = true
passthrough_auth = true
upstream_url = "http://127.0.0.1:{upstream_port}/v1"
upstream_api_key = "upstream-openai-key"
default_memory_mode = "off"
extraction_enabled = false
"#
    );
    let config_content = test_config_with_sections(
        port,
        data_dir.to_str().unwrap(),
        auth_entries,
        &extra_sections,
    );
    let config_path = tmp_dir.path().join("proxy-local-only.toml");
    std::fs::write(&config_path, &config_content).unwrap();

    let mut child = start_server(config_path.to_str().unwrap()).await;
    let client = test_client();
    let base = format!("https://127.0.0.1:{port}");

    let local_resp = client
        .get(format!("{base}/proxy/v1/models"))
        .header("Authorization", "Bearer sk-local-passthrough")
        .send()
        .await
        .unwrap();
    assert_eq!(local_resp.status(), 200);

    let remote_resp = client
        .get(format!("{base}/proxy/v1/models"))
        .header("Authorization", "Bearer sk-remote-passthrough")
        .header("x-forwarded-for", "203.0.113.10")
        .send()
        .await
        .unwrap();
    assert_eq!(remote_resp.status(), 401);

    child.kill().await.ok();
    upstream_handle.abort();
}

#[tokio::test]
async fn test_proxy_anthropic_oauth_passthrough_uses_bearer_and_preserves_headers() {
    let (upstream_port, upstream_state, upstream_handle) = start_dummy_upstream().await;

    let port = free_port();
    let tmp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let data_dir = tmp_dir.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();

    let config_content = test_config_with_sections(
        port,
        data_dir.to_str().unwrap(),
        "",
        &format!(
            r#"
[proxy]
enabled = true
passthrough_auth = true
anthropic_upstream_url = "http://127.0.0.1:{upstream_port}/v1/messages"
default_memory_mode = "off"
extraction_enabled = false
"#
        ),
    );
    let config_path = tmp_dir.path().join("anthropic-oauth.toml");
    std::fs::write(&config_path, &config_content).unwrap();

    let mut child = start_server(config_path.to_str().unwrap()).await;
    let client = test_client();
    let base = format!("https://127.0.0.1:{port}");

    let system_blocks = serde_json::json!([
        {
            "type": "text",
            "text": "cache this",
            "cache_control": { "type": "ephemeral" }
        }
    ]);

    let anthropic_resp = client
        .post(format!("{base}/proxy/anthropic/v1/messages"))
        .header("Authorization", "Bearer oauth-test-token-123")
        .header("anthropic-version", "2023-06-01")
        .header("anthropic-beta", "prompt-caching-2024-07-31")
        .json(&serde_json::json!({
            "model": "claude-3-5-haiku-latest",
            "max_tokens": 16,
            "system": system_blocks,
            "messages": [{"role": "user", "content": "hello from anthropic oauth"}]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(anthropic_resp.status(), 200);
    let anthropic_body: serde_json::Value = anthropic_resp.json().await.unwrap();
    assert_eq!(
        anthropic_body["content"][0]["text"].as_str(),
        Some("dummy anthropic output")
    );

    tokio::time::sleep(Duration::from_millis(300)).await;
    let requests = upstream_state.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 1, "expected one anthropic upstream request");

    let anthropic_req = &requests[0];
    assert_eq!(
        anthropic_req["authorization"].as_str(),
        Some("Bearer oauth-test-token-123")
    );
    assert_eq!(anthropic_req["x_api_key"].as_str(), Some(""));
    assert_eq!(
        anthropic_req["anthropic_beta"].as_str(),
        Some("prompt-caching-2024-07-31")
    );
    assert_eq!(
        anthropic_req["anthropic_version"].as_str(),
        Some("2023-06-01")
    );
    assert_eq!(
        anthropic_req["body"]["system"][0]["cache_control"]["type"].as_str(),
        Some("ephemeral")
    );

    child.kill().await.ok();
    upstream_handle.abort();
}

#[tokio::test]
async fn test_hybrid_gateway_fallback_covers_four_auth_paths_and_reports_core_unavailable() {
    let (upstream_port, upstream_state, upstream_handle) = start_dummy_upstream().await;

    let gateway_port = free_port();
    let core_port = free_port();
    let tmp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let data_dir = tmp_dir.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();

    let config_content = format!(
        r#"
[server]
host = "127.0.0.1"
port = {gateway_port}
hybrid_mode = true
core_port = {core_port}

[tls]
enabled = false
auto_generate = false

[storage]
data_dir = "{data_dir}"

[auth]
jwt_secret = "test-secret-that-is-at-least-32-characters-long"

[[auth.api_keys]]
key = "test-key-integration"
role = "admin"
namespace = "test"

[logging]
level = "warn"

[proxy]
enabled = true
passthrough_auth = true
passthrough_local_only = true
upstream_url = "http://127.0.0.1:{upstream_port}/v1"
anthropic_upstream_url = "http://127.0.0.1:{upstream_port}/v1/messages"
default_memory_mode = "off"
extraction_enabled = false
"#,
        data_dir = data_dir.to_str().unwrap(),
    );
    let config_path = tmp_dir.path().join("hybrid-gateway.toml");
    std::fs::write(&config_path, &config_content).unwrap();

    let mut child = start_server_command(config_path.to_str().unwrap(), "serve-gateway").await;
    let client = reqwest::Client::builder().build().unwrap();
    let base = format!("http://127.0.0.1:{gateway_port}");

    let health: serde_json::Value = client
        .get(format!("{base}/health"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(health["core_status"].as_str(), Some("degraded"));

    let recall_resp = client
        .post(format!("{base}/v1/recall"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({"query":"hello"}))
        .send()
        .await
        .unwrap();
    assert_eq!(recall_resp.status(), 503);

    let openai_api_resp = client
        .post(format!("{base}/proxy/v1/responses"))
        .header("Authorization", "Bearer sk-openai-direct-123")
        .json(&serde_json::json!({
            "model": "gpt-4o-mini",
            "input": "hello from openai api key"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(openai_api_resp.status(), 200);

    let openai_oauth_resp = client
        .post(format!("{base}/proxy/v1/responses"))
        .header("Authorization", "Bearer eyJ-openai-oauth-token")
        .json(&serde_json::json!({
            "model": "gpt-4o-mini",
            "input": "hello from openai oauth"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(openai_oauth_resp.status(), 200);

    let anthropic_api_resp = client
        .post(format!("{base}/proxy/anthropic/v1/messages"))
        .header("x-api-key", "sk-ant-direct-123")
        .header("anthropic-version", "2023-06-01")
        .json(&serde_json::json!({
            "model": "claude-opus-4-6",
            "messages": [{"role": "user", "content": "hello from anthropic api key"}]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(anthropic_api_resp.status(), 200);

    let anthropic_oauth_resp = client
        .post(format!("{base}/proxy/anthropic/v1/messages"))
        .header("Authorization", "Bearer claude-oauth-token")
        .header("anthropic-version", "2023-06-01")
        .header("anthropic-beta", "prompt-caching-2024-07-31")
        .json(&serde_json::json!({
            "model": "claude-opus-4-6",
            "messages": [{"role": "user", "content": "hello from anthropic oauth"}]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(anthropic_oauth_resp.status(), 200);

    let requests = upstream_state.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 4);

    let openai_api_req = requests
        .iter()
        .find(|req| req["body"]["input"].as_str() == Some("hello from openai api key"))
        .expect("missing openai api fallback request");
    assert_eq!(
        openai_api_req["authorization"].as_str(),
        Some("Bearer sk-openai-direct-123")
    );

    let openai_oauth_req = requests
        .iter()
        .find(|req| req["body"]["input"].as_str() == Some("hello from openai oauth"))
        .expect("missing openai oauth fallback request");
    assert_eq!(
        openai_oauth_req["authorization"].as_str(),
        Some("Bearer eyJ-openai-oauth-token")
    );

    let anthropic_api_req = requests
        .iter()
        .find(|req| {
            req["body"]["messages"][0]["content"].as_str() == Some("hello from anthropic api key")
        })
        .expect("missing anthropic api fallback request");
    assert_eq!(
        anthropic_api_req["x_api_key"].as_str(),
        Some("sk-ant-direct-123")
    );

    let anthropic_oauth_req = requests
        .iter()
        .find(|req| {
            req["body"]["messages"][0]["content"].as_str() == Some("hello from anthropic oauth")
        })
        .expect("missing anthropic oauth fallback request");
    assert_eq!(
        anthropic_oauth_req["authorization"].as_str(),
        Some("Bearer claude-oauth-token")
    );
    assert_eq!(
        anthropic_oauth_req["anthropic_beta"].as_str(),
        Some("prompt-caching-2024-07-31")
    );

    child.kill().await.ok();
    upstream_handle.abort();
}

#[tokio::test]
async fn test_hybrid_gateway_proxies_memory_api_to_running_core() {
    let gateway_port = free_port();
    let core_port = free_port();
    let tmp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let data_dir = tmp_dir.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();

    let config_content = format!(
        r#"
[server]
host = "127.0.0.1"
port = {gateway_port}
hybrid_mode = true
core_port = {core_port}

[tls]
enabled = false
auto_generate = false

[storage]
data_dir = "{data_dir}"

[auth]
jwt_secret = "test-secret-that-is-at-least-32-characters-long"

[[auth.api_keys]]
key = "test-key-integration"
role = "admin"
namespace = "test"

[logging]
level = "warn"
"#,
        data_dir = data_dir.to_str().unwrap(),
    );
    let config_path = tmp_dir.path().join("hybrid-core.toml");
    std::fs::write(&config_path, &config_content).unwrap();

    let mut core = start_server_command(config_path.to_str().unwrap(), "serve-core").await;
    let mut gateway = start_server_command(config_path.to_str().unwrap(), "serve-gateway").await;
    let client = reqwest::Client::builder().build().unwrap();
    let base = format!("http://127.0.0.1:{gateway_port}");

    let health: serde_json::Value = client
        .get(format!("{base}/health"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(health["core_status"].as_str(), Some("ok"));

    let store_resp = client
        .post(format!("{base}/v1/store"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({
            "content": "gateway core memory",
            "agent": "gateway-test",
            "tags": ["hybrid"]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(store_resp.status(), 200);

    let export_resp = client
        .get(format!("{base}/v1/export"))
        .header("Authorization", "Bearer test-key-integration")
        .send()
        .await
        .unwrap();
    assert_eq!(export_resp.status(), 200);
    let export_body: serde_json::Value = export_resp.json().await.unwrap();
    let items = export_body["memories"].as_array().unwrap();
    assert!(
        items
            .iter()
            .any(|item| item["content"].as_str() == Some("gateway core memory")),
        "gateway should proxy memory API traffic to the running core"
    );

    gateway.kill().await.ok();
    core.kill().await.ok();
}

#[tokio::test]
async fn test_hybrid_serve_manages_core_and_exposes_gateway_health() {
    let gateway_port = free_port();
    let core_port = free_port();
    let tmp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let data_dir = tmp_dir.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();

    let config_content = format!(
        r#"
[server]
host = "127.0.0.1"
port = {gateway_port}
hybrid_mode = true
core_port = {core_port}

[tls]
enabled = false
auto_generate = false

[storage]
data_dir = "{data_dir}"

[auth]
jwt_secret = "test-secret-that-is-at-least-32-characters-long"

[[auth.api_keys]]
key = "test-key-integration"
role = "admin"
namespace = "test"

[logging]
level = "warn"
"#,
        data_dir = data_dir.to_str().unwrap(),
    );
    let config_path = tmp_dir.path().join("hybrid-managed.toml");
    std::fs::write(&config_path, &config_content).unwrap();

    let mut child = start_server(config_path.to_str().unwrap()).await;
    let client = reqwest::Client::builder().build().unwrap();
    let base = format!("http://127.0.0.1:{gateway_port}");

    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let mut healthy = false;
    while tokio::time::Instant::now() < deadline {
        let health_resp = client.get(format!("{base}/health")).send().await.unwrap();
        let health: serde_json::Value = health_resp.json().await.unwrap();
        if health["core_status"].as_str() == Some("ok") {
            healthy = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    assert!(
        healthy,
        "serve should start the gateway and manage a healthy core child"
    );

    child.kill().await.ok();
}

#[tokio::test]
async fn test_sharing_connections_cover_owner_and_grantee_paths() {
    let port = free_port();
    let tmp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let data_dir = tmp_dir.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();

    let config_content = multi_namespace_test_config(port, data_dir.to_str().unwrap());
    let config_path = tmp_dir.path().join("sharing.toml");
    std::fs::write(&config_path, &config_content).unwrap();

    let mut child = start_server(config_path.to_str().unwrap()).await;
    let client = test_client();
    let base = format!("https://127.0.0.1:{port}");

    let create_resp = client
        .post(format!("{base}/v1/admin/sharing/create"))
        .header("Authorization", "Bearer alpha-admin-key")
        .json(&serde_json::json!({
            "name": "shared-playbook"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(create_resp.status(), 200);

    let grant_resp = client
        .post(format!(
            "{base}/v1/admin/sharing/shared-playbook/grants/add"
        ))
        .header("Authorization", "Bearer alpha-admin-key")
        .json(&serde_json::json!({
            "grantee_namespace": "beta",
            "permission": "read"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(grant_resp.status(), 200);
    let grant_body: serde_json::Value = grant_resp.json().await.unwrap();
    let grant_id = grant_body["id"].as_str().expect("missing grant id");

    let owner_list_resp = client
        .get(format!("{base}/v1/admin/sharing/list"))
        .header("Authorization", "Bearer alpha-admin-key")
        .send()
        .await
        .unwrap();
    assert_eq!(owner_list_resp.status(), 200);
    let owner_list_body: serde_json::Value = owner_list_resp.json().await.unwrap();
    assert!(
        owner_list_body["shared_namespaces"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["name"].as_str() == Some("shared-playbook"))
    );

    let beta_access_resp = client
        .get(format!("{base}/v1/sharing/accessible"))
        .header("Authorization", "Bearer beta-admin-key")
        .send()
        .await
        .unwrap();
    assert_eq!(beta_access_resp.status(), 200);
    let beta_access_body: serde_json::Value = beta_access_resp.json().await.unwrap();
    assert!(
        beta_access_body["accessible"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry.as_str() == Some("shared-playbook"))
    );

    let beta_grants_resp = client
        .get(format!(
            "{base}/v1/admin/sharing/shared-playbook/grants/list"
        ))
        .header("Authorization", "Bearer beta-admin-key")
        .send()
        .await
        .unwrap();
    assert_eq!(beta_grants_resp.status(), 200);
    let beta_grants_body: serde_json::Value = beta_grants_resp.json().await.unwrap();
    assert_eq!(beta_grants_body["grants"].as_array().unwrap().len(), 1);

    let remove_grant_resp = client
        .delete(format!(
            "{base}/v1/admin/sharing/shared-playbook/grants/{grant_id}"
        ))
        .header("Authorization", "Bearer alpha-admin-key")
        .send()
        .await
        .unwrap();
    assert_eq!(remove_grant_resp.status(), 200);

    let beta_access_after_resp = client
        .get(format!("{base}/v1/sharing/accessible"))
        .header("Authorization", "Bearer beta-admin-key")
        .send()
        .await
        .unwrap();
    assert_eq!(beta_access_after_resp.status(), 200);
    let beta_access_after_body: serde_json::Value = beta_access_after_resp.json().await.unwrap();
    assert!(
        !beta_access_after_body["accessible"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry.as_str() == Some("shared-playbook"))
    );

    let delete_resp = client
        .delete(format!("{base}/v1/admin/sharing/shared-playbook"))
        .header("Authorization", "Bearer alpha-admin-key")
        .send()
        .await
        .unwrap();
    assert_eq!(delete_resp.status(), 200);

    child.kill().await.ok();
}

#[tokio::test]
async fn test_gdpr_connections_cover_export_access_and_certified_forget() {
    let port = free_port();
    let tmp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let data_dir = tmp_dir.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();

    let config_content = test_config(port, data_dir.to_str().unwrap());
    let config_path = tmp_dir.path().join("gdpr.toml");
    std::fs::write(&config_path, &config_content).unwrap();

    let mut child = start_server(config_path.to_str().unwrap()).await;
    let client = test_client();
    let base = format!("https://127.0.0.1:{port}");

    let store_resp = client
        .post(format!("{base}/v1/store"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({
            "content": "GDPR test memory for export and certified forget.",
            "agent": "gdpr-agent",
            "session": "gdpr-session",
            "tags": ["gdpr", "export"]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(store_resp.status(), 200);
    let memory_id = store_resp.json::<serde_json::Value>().await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    tokio::time::sleep(Duration::from_secs(3)).await;

    let export_resp = client
        .get(format!("{base}/v1/export"))
        .header("Authorization", "Bearer test-key-integration")
        .send()
        .await
        .unwrap();
    assert_eq!(export_resp.status(), 200);
    let export_body: serde_json::Value = export_resp.json().await.unwrap();
    assert_eq!(
        export_body["runtime_contract"]["contract_id"].as_str(),
        Some("memoryoss.runtime.v1alpha1")
    );
    assert_eq!(
        export_body["runtime_contract"]["version"].as_str(),
        Some("2026-03-13")
    );
    assert_eq!(export_body["count"].as_u64(), Some(1));
    assert!(
        export_body["memories"][0].get("embedding").is_none(),
        "export should omit embeddings"
    );
    assert_ne!(
        export_body["memories"][0]["source_key_id"].as_str(),
        Some("test-key-integration")
    );

    let access_resp = client
        .get(format!(
            "{base}/v1/memories?agent=gdpr-agent&session=gdpr-session"
        ))
        .header("Authorization", "Bearer test-key-integration")
        .send()
        .await
        .unwrap();
    assert_eq!(access_resp.status(), 200);
    let access_body: serde_json::Value = access_resp.json().await.unwrap();
    assert_eq!(access_body["count"].as_u64(), Some(1));

    let certified_forget_resp = client
        .delete(format!("{base}/v1/forget/certified"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({
            "ids": [memory_id]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(certified_forget_resp.status(), 200);
    let certified_forget_body: serde_json::Value = certified_forget_resp.json().await.unwrap();
    assert_eq!(certified_forget_body["deleted"].as_u64(), Some(1));
    assert!(
        certified_forget_body["signature"].as_str().is_some(),
        "certified forget should return a signature"
    );
    assert!(
        certified_forget_body["certificate"]["deleted_ids"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry.as_str() == Some(memory_id.as_str()))
    );

    let export_after_resp = client
        .get(format!("{base}/v1/export"))
        .header("Authorization", "Bearer test-key-integration")
        .send()
        .await
        .unwrap();
    assert_eq!(export_after_resp.status(), 200);
    let export_after_body: serde_json::Value = export_after_resp.json().await.unwrap();
    assert_eq!(export_after_body["count"].as_u64(), Some(0));

    child.kill().await.ok();
}

#[tokio::test]
async fn test_runtime_contract_endpoint_maps_stable_semantics_and_known_gaps() {
    let port = free_port();
    let tmp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let data_dir = tmp_dir.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();

    let config_content = test_config(port, data_dir.to_str().unwrap());
    let config_path = tmp_dir.path().join("runtime-contract.toml");
    std::fs::write(&config_path, &config_content).unwrap();

    let mut child = start_server(config_path.to_str().unwrap()).await;
    let client = test_client();
    let base = format!("https://127.0.0.1:{port}");

    let resp = client
        .get(format!("{base}/v1/runtime/contract"))
        .header("Authorization", "Bearer test-key-integration")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();

    assert_eq!(
        body["contract_id"].as_str(),
        Some("memoryoss.runtime.v1alpha1")
    );
    assert_eq!(body["version"].as_str(), Some("2026-03-13"));
    assert!(
        body["stable_semantics"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["name"].as_str() == Some("namespace_scope")),
        "contract should expose stable namespace scope semantics"
    );
    assert!(
        body["experimental_layers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| {
                entry["name"].as_str() == Some("retrieval_confidence_gate")
                    && entry["excluded_from_contract"].as_bool() == Some(true)
            }),
        "experimental retrieval layers must be separated from the stable contract"
    );
    assert!(
        body["object_model"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| {
                entry["kind"].as_str() == Some("branch")
                    && entry["support_level"].as_str() == Some("partial")
                    && entry["current_mapping"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|route| route.as_str() == Some("/v1/history/replay"))
            }),
        "branch should be documented as a partial empty-target replay surface"
    );
    assert!(
        body["known_gaps"].as_array().unwrap().iter().any(|entry| {
            entry["area"].as_str() == Some("replay") && entry["status"].as_str() == Some("partial")
        }),
        "contract should surface replay as a bounded partial gap"
    );
    assert!(
        body["api_mappings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| {
                entry["runtime_operation"].as_str() == Some("portability_export")
                    && entry["routes"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|route| route.as_str() == Some("/v1/export"))
            }),
        "portability export should be mapped into the runtime contract"
    );

    child.kill().await.ok();
}

#[tokio::test]
async fn test_semantic_dedup_is_isolated_per_namespace() {
    let port = free_port();
    let tmp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let data_dir = tmp_dir.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();

    let config_content = multi_namespace_test_config(port, data_dir.to_str().unwrap());
    let config_path = tmp_dir.path().join("semantic-dedup-namespaces.toml");
    std::fs::write(&config_path, &config_content).unwrap();

    let mut child = start_server(config_path.to_str().unwrap()).await;
    let client = test_client();
    let base = format!("https://127.0.0.1:{port}");

    let mut embedding = vec![0.0f32; 384];
    embedding[0] = 1.0;

    let alpha_store = client
        .post(format!("{base}/v1/store"))
        .header("Authorization", "Bearer alpha-admin-key")
        .json(&serde_json::json!({
            "content": "Alpha namespace stores a rollout checklist embedding.",
            "tags": ["dedup", "alpha"],
            "zero_knowledge": true,
            "embedding": embedding.clone(),
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(alpha_store.status(), 200);

    tokio::time::sleep(Duration::from_secs(2)).await;

    let beta_store = client
        .post(format!("{base}/v1/store"))
        .header("Authorization", "Bearer beta-admin-key")
        .json(&serde_json::json!({
            "content": "Beta namespace stores the same embedding and should still be allowed.",
            "tags": ["dedup", "beta"],
            "zero_knowledge": true,
            "embedding": embedding,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        beta_store.status(),
        200,
        "semantic dedup should not reject identical embeddings from another namespace"
    );

    child.kill().await.ok();
}

#[tokio::test]
async fn test_consolidate_reports_reduction_and_preserves_provenance() {
    let port = free_port();
    let tmp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let data_dir = tmp_dir.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();

    let config_content = test_config(port, data_dir.to_str().unwrap());
    let config_path = tmp_dir.path().join("consolidate.toml");
    std::fs::write(&config_path, &config_content).unwrap();

    let mut child = start_server(config_path.to_str().unwrap()).await;
    let client = test_client();
    let base = format!("https://127.0.0.1:{port}");

    let source_contents = [
        "Deployment runbook requires staging approval before production rollout.",
        "Deployment runbook requires staging approval before production rollout and notifying ops-red.",
    ];
    let mut source_ids = Vec::new();
    for content in source_contents {
        let resp = client
            .post(format!("{base}/v1/store"))
            .header("Authorization", "Bearer test-key-integration")
            .json(&serde_json::json!({
                "content": content,
                "tags": ["consolidation", "deploy"]
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        source_ids.push(
            resp.json::<serde_json::Value>().await.unwrap()["id"]
                .as_str()
                .unwrap()
                .to_string(),
        );
    }

    tokio::time::sleep(Duration::from_secs(2)).await;

    let consolidate_resp = client
        .post(format!("{base}/v1/consolidate"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({
            "threshold": 0.9,
            "max_clusters": 10,
            "dry_run": false
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(consolidate_resp.status(), 200);
    let consolidate_body: serde_json::Value = consolidate_resp.json().await.unwrap();
    assert_eq!(consolidate_body["total_merged"].as_u64(), Some(1));
    assert_eq!(consolidate_body["derived_created"].as_u64(), Some(1));
    assert_eq!(consolidate_body["active_before"].as_u64(), Some(2));
    assert_eq!(consolidate_body["active_after"].as_u64(), Some(1));
    assert_eq!(consolidate_body["active_reduction"].as_u64(), Some(1));
    assert!(
        consolidate_body["duplicate_rate_before"]
            .as_f64()
            .unwrap_or(0.0)
            > consolidate_body["duplicate_rate_after"]
                .as_f64()
                .unwrap_or(1.0)
    );

    let group = &consolidate_body["groups"][0];
    let derived_id = group["derived_id"]
        .as_str()
        .expect("derived_id missing after consolidation");
    assert_eq!(group["source_ids"].as_array().map(|v| v.len()), Some(2));

    let derived = inspect_memory(&client, &base, "test-key-integration", derived_id).await;
    assert_eq!(derived["status"].as_str(), Some("active"));
    assert_eq!(derived["derived_from"].as_array().map(|v| v.len()), Some(2));

    for source_id in &source_ids {
        let source = inspect_memory(&client, &base, "test-key-integration", source_id).await;
        assert_eq!(source["status"].as_str(), Some("stale"));
        assert_eq!(source["superseded_by"].as_str(), Some(derived_id));
    }

    let export_resp = client
        .get(format!("{base}/v1/export"))
        .header("Authorization", "Bearer test-key-integration")
        .send()
        .await
        .unwrap();
    assert_eq!(export_resp.status(), 200);
    let export_body: serde_json::Value = export_resp.json().await.unwrap();
    assert_eq!(export_body["count"].as_u64(), Some(1));
    assert_eq!(export_body["memories"][0]["id"].as_str(), Some(derived_id));

    child.kill().await.ok();
}

#[tokio::test]
async fn test_consolidation_worker_runs_automatically() {
    let port = free_port();
    let tmp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let data_dir = tmp_dir.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();

    let config_content = test_config_with_sections(
        port,
        data_dir.to_str().unwrap(),
        r#"
[[auth.api_keys]]
key = "test-key-integration"
role = "admin"
namespace = "test"
"#,
        r#"
[consolidation]
enabled = true
interval_minutes = 0
threshold = 0.9
max_clusters = 10
"#,
    );
    let config_path = tmp_dir.path().join("consolidation-worker.toml");
    std::fs::write(&config_path, &config_content).unwrap();

    let mut child = start_server(config_path.to_str().unwrap()).await;
    let client = test_client();
    let base = format!("https://127.0.0.1:{port}");

    let first_resp = client
        .post(format!("{base}/v1/store"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({
            "content": "Rollback guide requires staging approval before production deploys.",
            "tags": ["consolidation", "rollback"]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(first_resp.status(), 200);
    let first_id = first_resp.json::<serde_json::Value>().await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    let second_resp = client
        .post(format!("{base}/v1/store"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({
            "content": "Rollback guide requires staging approval before production deploys and notifying ops-red.",
            "tags": ["consolidation", "rollback"]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(second_resp.status(), 200);
    let second_id = second_resp.json::<serde_json::Value>().await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    let derived_id =
        wait_for_superseded_by(&client, &base, "test-key-integration", &first_id).await;
    wait_for_specific_superseded_by(
        &client,
        &base,
        "test-key-integration",
        &second_id,
        derived_id.as_str(),
    )
    .await;
    let second = inspect_memory(&client, &base, "test-key-integration", &second_id).await;
    assert_eq!(second["superseded_by"].as_str(), Some(derived_id.as_str()));

    let derived = inspect_memory(&client, &base, "test-key-integration", &derived_id).await;
    assert_eq!(derived["derived_from"].as_array().map(|v| v.len()), Some(2));
    assert_eq!(derived["status"].as_str(), Some("active"));

    let export_resp = client
        .get(format!("{base}/v1/export"))
        .header("Authorization", "Bearer test-key-integration")
        .send()
        .await
        .unwrap();
    assert_eq!(export_resp.status(), 200);
    let export_body: serde_json::Value = export_resp.json().await.unwrap();
    assert_eq!(export_body["count"].as_u64(), Some(1));
    assert_eq!(
        export_body["memories"][0]["id"].as_str(),
        Some(derived_id.as_str())
    );

    child.kill().await.ok();
}

#[tokio::test]
async fn test_key_rotation_connections_cover_rotate_list_revoke_and_read() {
    let port = free_port();
    let tmp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let data_dir = tmp_dir.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();

    let config_content = test_config(port, data_dir.to_str().unwrap());
    let config_path = tmp_dir.path().join("keys.toml");
    std::fs::write(&config_path, &config_content).unwrap();

    let mut child = start_server(config_path.to_str().unwrap()).await;
    let client = test_client();
    let base = format!("https://127.0.0.1:{port}");

    let store_resp = client
        .post(format!("{base}/v1/store"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({
            "content": "Key rotation should keep this memory readable during grace period.",
            "tags": ["keys", "rotation"]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(store_resp.status(), 200);

    tokio::time::sleep(Duration::from_secs(3)).await;

    let rotate_resp = client
        .post(format!("{base}/v1/admin/keys/rotate"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({
            "namespace": "test"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(rotate_resp.status(), 200);
    let rotate_body: serde_json::Value = rotate_resp.json().await.unwrap();
    let key_id = rotate_body["key_id"].as_str().expect("missing key id");

    let list_resp = client
        .get(format!("{base}/v1/admin/keys"))
        .header("Authorization", "Bearer test-key-integration")
        .send()
        .await
        .unwrap();
    assert_eq!(list_resp.status(), 200);
    let list_body: serde_json::Value = list_resp.json().await.unwrap();
    assert!(
        list_body["retired_keys"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["id"].as_str() == Some(key_id))
    );

    let recall_resp = client
        .post(format!("{base}/v1/recall"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({
            "query": "grace period readable memory"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(recall_resp.status(), 200);
    let recall_body: serde_json::Value = recall_resp.json().await.unwrap();
    assert!(
        !recall_body["memories"].as_array().unwrap().is_empty(),
        "pre-rotation memory should remain readable after rotation"
    );

    let revoke_resp = client
        .delete(format!("{base}/v1/admin/keys/{key_id}"))
        .header("Authorization", "Bearer test-key-integration")
        .send()
        .await
        .unwrap();
    assert_eq!(revoke_resp.status(), 200);

    let list_after_resp = client
        .get(format!("{base}/v1/admin/keys"))
        .header("Authorization", "Bearer test-key-integration")
        .send()
        .await
        .unwrap();
    assert_eq!(list_after_resp.status(), 200);
    let list_after_body: serde_json::Value = list_after_resp.json().await.unwrap();
    assert!(
        !list_after_body["retired_keys"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["id"].as_str() == Some(key_id))
    );

    child.kill().await.ok();
}

#[tokio::test]
async fn test_decay_and_migrate_cli_connections() {
    let port = free_port();
    let tmp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let data_dir = tmp_dir.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();

    let config_content = test_config(port, data_dir.to_str().unwrap());
    let config_path = tmp_dir.path().join("cli.toml");
    std::fs::write(&config_path, &config_content).unwrap();

    let mut child = start_server(config_path.to_str().unwrap()).await;
    let client = test_client();
    let base = format!("https://127.0.0.1:{port}");

    let store_resp = client
        .post(format!("{base}/v1/store"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({
            "content": "Legacy deployment note that should become stale before decay archival.",
            "tags": ["decay", "cli", "legacy"]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(store_resp.status(), 200);
    let stale_id = store_resp.json::<serde_json::Value>().await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    let replacement_resp = client
        .post(format!("{base}/v1/store"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({
            "content": "Current deployment note that supersedes the legacy CLI decay rule.",
            "tags": ["decay", "cli", "current"]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(replacement_resp.status(), 200);
    let replacement_id = replacement_resp.json::<serde_json::Value>().await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    let supersede_resp = client
        .post(format!("{base}/v1/feedback"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({
            "id": stale_id,
            "action": "supersede",
            "superseded_by": replacement_id
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(supersede_resp.status(), 200);

    tokio::time::sleep(Duration::from_secs(3)).await;
    child.kill().await.ok();
    child.wait().await.ok();

    let dry_run = tokio::process::Command::new(env!("CARGO_BIN_EXE_memoryoss"))
        .args([
            "--config",
            config_path.to_str().unwrap(),
            "decay",
            "--dry-run",
            "--after-days",
            "0",
        ])
        .output()
        .await
        .expect("failed to run decay dry-run");
    assert!(
        dry_run.status.success(),
        "decay dry-run failed: stdout={} stderr={}",
        String::from_utf8_lossy(&dry_run.stdout),
        String::from_utf8_lossy(&dry_run.stderr)
    );
    let dry_run_stdout = String::from_utf8_lossy(&dry_run.stdout);
    assert!(
        dry_run_stdout.contains("Would archive"),
        "dry-run should report archive candidates"
    );

    let decay = tokio::process::Command::new(env!("CARGO_BIN_EXE_memoryoss"))
        .args([
            "--config",
            config_path.to_str().unwrap(),
            "decay",
            "--after-days",
            "0",
        ])
        .output()
        .await
        .expect("failed to run decay");
    assert!(
        decay.status.success(),
        "decay command failed: stdout={} stderr={}",
        String::from_utf8_lossy(&decay.stdout),
        String::from_utf8_lossy(&decay.stderr)
    );
    let decay_stdout = String::from_utf8_lossy(&decay.stdout);
    assert!(
        decay_stdout.contains("archived"),
        "decay should report archived memories"
    );

    let migrate = tokio::process::Command::new(env!("CARGO_BIN_EXE_memoryoss"))
        .args([
            "--config",
            config_path.to_str().unwrap(),
            "migrate",
            "--dry-run",
        ])
        .output()
        .await
        .expect("failed to run migrate dry-run");
    assert!(
        migrate.status.success(),
        "migrate dry-run failed: stdout={} stderr={}",
        String::from_utf8_lossy(&migrate.stdout),
        String::from_utf8_lossy(&migrate.stderr)
    );
    let migrate_stdout = String::from_utf8_lossy(&migrate.stdout);
    assert!(
        migrate_stdout.contains("No pending migrations") || migrate_stdout.contains("Schema at v"),
        "migrate dry-run should report schema state"
    );

    let mut child = start_server(config_path.to_str().unwrap()).await;

    let lifecycle_resp = client
        .get(format!(
            "{base}/v1/admin/lifecycle?include_archived=true&limit=10"
        ))
        .header("Authorization", "Bearer test-key-integration")
        .send()
        .await
        .unwrap();
    assert_eq!(lifecycle_resp.status(), 200);
    let lifecycle_body: serde_json::Value = lifecycle_resp.json().await.unwrap();
    assert_eq!(lifecycle_body["summary"]["archived"].as_u64(), Some(1));

    let recall_resp = client
        .post(format!("{base}/v1/recall"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({
            "query": "decay archive memory"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(recall_resp.status(), 200);
    let recall_body: serde_json::Value = recall_resp.json().await.unwrap();
    assert!(
        recall_body["memories"]
            .as_array()
            .unwrap()
            .iter()
            .all(|memory| {
                memory["content"].as_str()
                    != Some(
                        "Legacy deployment note that should become stale before decay archival.",
                    )
            }),
        "archived legacy memory should not be recalled"
    );

    child.kill().await.ok();
}

#[tokio::test]
async fn test_review_queue_lists_candidate_and_confirm_records_audit_trail() {
    let (upstream_port, _upstream_state, upstream_handle) = start_dummy_upstream().await;

    let port = free_port();
    let tmp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let data_dir = tmp_dir.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();

    let auth_entries = r#"
[[auth.api_keys]]
key = "test-key-integration"
role = "admin"
namespace = "test"
"#;
    let extra_sections = format!(
        r#"
[proxy]
enabled = true
passthrough_auth = false
anthropic_upstream_url = "http://127.0.0.1:{upstream_port}/v1/messages"
anthropic_api_key = "anthropic-upstream-key"
default_memory_mode = "full"
extraction_enabled = true
extract_provider = "claude"
extract_model = "claude-test-promote"

[[proxy.key_mapping]]
proxy_key = "test-key-proxy"
namespace = "test"
"#
    );
    let config_content = test_config_with_sections(
        port,
        data_dir.to_str().unwrap(),
        auth_entries,
        &extra_sections,
    );
    let config_path = tmp_dir.path().join("review-candidate.toml");
    std::fs::write(&config_path, &config_content).unwrap();

    let mut child = start_server(config_path.to_str().unwrap()).await;
    let client = test_client();
    let base = format!("https://127.0.0.1:{port}");

    let proxy_resp = client
        .post(format!("{base}/proxy/anthropic/v1/messages"))
        .header("x-api-key", "test-key-proxy")
        .header("anthropic-version", "2023-06-01")
        .header("x-memory-mode", "full")
        .json(&serde_json::json!({
            "model": "claude-3-5-haiku-latest",
            "max_tokens": 16,
            "messages": [{"role": "user", "content": "Summarize the rollout checklist."}]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(proxy_resp.status(), 200);
    wait_for_proxy_facts_extracted(&client, &base, "test-key-proxy", 1).await;

    let queue_body = review_queue(&client, &base, "test-key-integration", "test", 10).await;
    assert_eq!(queue_body["summary"]["candidate"].as_u64(), Some(1));
    let item = queue_body["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| {
            entry["preview"]
                .as_str()
                .unwrap_or("")
                .contains("Promotion fact alpha")
        })
        .cloned()
        .expect("expected candidate review item");
    assert_eq!(item["queue_kind"].as_str(), Some("candidate"));
    assert_eq!(item["suggested_action"].as_str(), Some("confirm"));
    assert_eq!(item["source"].as_str(), Some("proxy-extraction"));
    assert!(item["trust_score"].as_f64().unwrap_or(0.0) > 0.0);
    let review_key = item["review_key"].as_str().unwrap().to_string();

    let action_resp = client
        .post(format!("{base}/v1/admin/review/action"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({
            "namespace": "test",
            "review_key": review_key,
            "action": "confirm"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(action_resp.status(), 200);
    let action_body: serde_json::Value = action_resp.json().await.unwrap();
    assert_eq!(action_body["memory"]["status"].as_str(), Some("active"));
    assert_eq!(
        action_body["memory"]["review_event_count"].as_u64(),
        Some(1)
    );
    assert_eq!(
        action_body["memory"]["last_review"]["via"].as_str(),
        Some("review_inbox")
    );

    let queue_after = review_queue(&client, &base, "test-key-integration", "test", 10).await;
    assert_eq!(queue_after["summary"]["pending"].as_u64(), Some(0));

    let export_resp = client
        .get(format!("{base}/v1/export"))
        .header("Authorization", "Bearer test-key-integration")
        .send()
        .await
        .unwrap();
    assert_eq!(export_resp.status(), 200);
    let export_body: serde_json::Value = export_resp.json().await.unwrap();
    let memory_id = export_body["memories"]
        .as_array()
        .unwrap()
        .iter()
        .find(|memory| {
            memory["content"]
                .as_str()
                == Some("Promotion fact alpha: use the rollout checklist before every production release.")
        })
        .and_then(|memory| memory["id"].as_str())
        .expect("candidate memory should be exportable")
        .to_string();
    let inspected = inspect_memory(&client, &base, "test-key-integration", &memory_id).await;
    assert_eq!(inspected["status"].as_str(), Some("active"));
    assert_eq!(
        inspected["review_events"][0]["queue_kind"].as_str(),
        Some("candidate")
    );
    assert_eq!(
        inspected["review_events"][0]["via"].as_str(),
        Some("review_inbox")
    );

    child.kill().await.ok();
    upstream_handle.abort();
}

#[tokio::test]
async fn test_review_queue_supersede_uses_review_keys_and_records_audit_trail() {
    let port = free_port();
    let tmp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let data_dir = tmp_dir.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();

    let config_content = test_config(port, data_dir.to_str().unwrap());
    let config_path = tmp_dir.path().join("review-supersede.toml");
    std::fs::write(&config_path, &config_content).unwrap();

    let mut child = start_server(config_path.to_str().unwrap()).await;
    let client = test_client();
    let base = format!("https://127.0.0.1:{port}");

    let first_store = client
        .post(format!("{base}/v1/store"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({
            "content": "Production deploys require staging approval before rollout.",
            "tags": ["deploy", "approval"]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(first_store.status(), 200);
    let first_id = first_store.json::<serde_json::Value>().await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    let second_store = client
        .post(format!("{base}/v1/store"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({
            "content": "Production deploys do not require staging approval before rollout.",
            "tags": ["deploy", "approval"]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(second_store.status(), 200);
    let second_id = second_store.json::<serde_json::Value>().await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    let queue_body = review_queue(&client, &base, "test-key-integration", "test", 10).await;
    assert_eq!(queue_body["summary"]["contested"].as_u64(), Some(2));
    let first_item = queue_body["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| {
            entry["preview"]
                .as_str()
                .unwrap_or("")
                .contains("require staging approval")
        })
        .cloned()
        .expect("expected first contested item");
    assert_eq!(first_item["queue_kind"].as_str(), Some("contested"));
    assert_eq!(first_item["suggested_action"].as_str(), Some("supersede"));
    let replacement_key = first_item["replacement_options"][0]["review_key"]
        .as_str()
        .expect("replacement review key missing")
        .to_string();

    let action_resp = client
        .post(format!("{base}/v1/admin/review/action"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({
            "namespace": "test",
            "review_key": first_item["review_key"].as_str().unwrap(),
            "action": "supersede",
            "supersede_with_review_key": replacement_key
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(action_resp.status(), 200);
    let action_body: serde_json::Value = action_resp.json().await.unwrap();
    assert_eq!(action_body["memory"]["status"].as_str(), Some("stale"));
    assert_eq!(
        action_body["replacement"]["status"].as_str(),
        Some("active")
    );

    let first = inspect_memory(&client, &base, "test-key-integration", &first_id).await;
    assert_eq!(first["status"].as_str(), Some("stale"));
    assert_eq!(
        first["review_events"][0]["action"].as_str(),
        Some("supersede")
    );
    assert_eq!(
        first["review_events"][0]["queue_kind"].as_str(),
        Some("contested")
    );

    let second = inspect_memory(&client, &base, "test-key-integration", &second_id).await;
    assert_eq!(second["status"].as_str(), Some("active"));
    assert_eq!(
        second["review_events"][0]["via"].as_str(),
        Some("review_inbox_supersede_target")
    );

    let queue_after = review_queue(&client, &base, "test-key-integration", "test", 10).await;
    assert_eq!(queue_after["summary"]["pending"].as_u64(), Some(0));

    child.kill().await.ok();
}

#[tokio::test]
async fn test_team_governance_propose_review_merge_and_history_replay_preserves_metadata() {
    let owner_key = "owner-admin-key";
    let reviewer_key = "reviewer-admin-key";
    let owner_key_id = test_key_id(owner_key);

    let source_port = free_port();
    let source_tmp = tempfile::tempdir().expect("failed to create source temp dir");
    let source_data_dir = source_tmp.path().join("data");
    std::fs::create_dir_all(&source_data_dir).unwrap();
    let auth_entries = r#"
[[auth.api_keys]]
key = "owner-admin-key"
role = "admin"
namespace = "alpha"

[[auth.api_keys]]
key = "reviewer-admin-key"
role = "admin"
namespace = "alpha"
"#;
    let source_config = test_config_http_with_sections(
        source_port,
        source_data_dir.to_str().unwrap(),
        auth_entries,
        "",
    );
    let source_config_path = source_tmp.path().join("team-governance-source.toml");
    std::fs::write(&source_config_path, &source_config).unwrap();

    let mut source_child = start_server(source_config_path.to_str().unwrap()).await;
    let client = reqwest::Client::builder().build().unwrap();
    let source_base = format!("http://127.0.0.1:{source_port}");

    let propose_resp = client
        .post(format!("{source_base}/v1/admin/team/governance/propose"))
        .header("Authorization", format!("Bearer {owner_key}"))
        .json(&serde_json::json!({
            "content": "Production deploys require release captain approval before rollout.",
            "tags": ["team", "deploy", "policy"],
            "branch": "release-playbook",
            "scope": "production",
            "review_required": true,
            "owners": [owner_key_id.clone()],
            "watchlist": ["ops-red", "release-captain"]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(propose_resp.status(), 200);
    let propose_body: serde_json::Value = propose_resp.json().await.unwrap();
    let memory_id = propose_body["id"].as_str().unwrap().to_string();
    let review_key = propose_body["review_key"].as_str().unwrap().to_string();
    assert_eq!(propose_body["status"].as_str(), Some("candidate"));
    assert_eq!(
        propose_body["team_governance"]["branch"].as_str(),
        Some("release-playbook")
    );

    let governance_resp = client
        .get(format!(
            "{source_base}/v1/admin/team/governance?namespace=alpha&limit=10"
        ))
        .header("Authorization", format!("Bearer {owner_key}"))
        .send()
        .await
        .unwrap();
    assert_eq!(governance_resp.status(), 200);
    let governance_body: serde_json::Value = governance_resp.json().await.unwrap();
    assert_eq!(
        governance_body["summary"]["governed_memories"].as_u64(),
        Some(1)
    );
    assert_eq!(
        governance_body["summary"]["active_branches"].as_u64(),
        Some(1)
    );
    assert_eq!(
        governance_body["summary"]["pending_review"].as_u64(),
        Some(1)
    );
    assert!(
        governance_body["branches"][0]["owners"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry.as_str() == Some(owner_key_id.as_str()))
    );

    let queue_body = review_queue(&client, &source_base, owner_key, "alpha", 10).await;
    let queue_item = queue_body["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["review_key"].as_str() == Some(review_key.as_str()))
        .cloned()
        .expect("expected governed review queue item");
    assert_eq!(
        queue_item["team_governance"]["scope"].as_str(),
        Some("production")
    );
    assert_eq!(queue_item["duplicate_content_count"].as_u64(), Some(0));
    assert_eq!(queue_item["stale_governance"].as_bool(), Some(false));

    let blocked_resp = client
        .post(format!("{source_base}/v1/admin/review/action"))
        .header("Authorization", format!("Bearer {reviewer_key}"))
        .json(&serde_json::json!({
            "namespace": "alpha",
            "review_key": review_key,
            "action": "confirm"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(blocked_resp.status(), 403);

    let confirm_resp = client
        .post(format!("{source_base}/v1/admin/review/action"))
        .header("Authorization", format!("Bearer {owner_key}"))
        .json(&serde_json::json!({
            "namespace": "alpha",
            "review_key": review_key,
            "action": "confirm"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(confirm_resp.status(), 200);
    let confirm_body: serde_json::Value = confirm_resp.json().await.unwrap();
    assert_eq!(confirm_body["memory"]["status"].as_str(), Some("active"));
    assert_eq!(
        confirm_body["memory"]["team_governance"]["merged_by"].as_str(),
        Some("alpha")
    );

    let inspected = inspect_memory(&client, &source_base, owner_key, &memory_id).await;
    assert_eq!(inspected["status"].as_str(), Some("active"));
    assert_eq!(
        inspected["team_governance"]["branch"].as_str(),
        Some("release-playbook")
    );
    assert_eq!(
        inspected["team_governance"]["merged_by"].as_str(),
        Some("alpha")
    );

    let bundle_resp = client
        .get(format!("{source_base}/v1/history/{memory_id}/bundle"))
        .header("Authorization", format!("Bearer {owner_key}"))
        .send()
        .await
        .unwrap();
    assert_eq!(bundle_resp.status(), 200);
    let bundle: serde_json::Value = bundle_resp.json().await.unwrap();
    assert_eq!(bundle["memories"].as_array().unwrap().len(), 1);

    source_child.kill().await.ok();
    source_child.wait().await.ok();

    let target_port = free_port();
    let target_tmp = tempfile::tempdir().expect("failed to create target temp dir");
    let target_data_dir = target_tmp.path().join("data");
    std::fs::create_dir_all(&target_data_dir).unwrap();
    let target_config = test_config_http_with_sections(
        target_port,
        target_data_dir.to_str().unwrap(),
        auth_entries,
        "",
    );
    let target_config_path = target_tmp.path().join("team-governance-target.toml");
    std::fs::write(&target_config_path, &target_config).unwrap();

    let mut target_child = start_server(target_config_path.to_str().unwrap()).await;
    let target_base = format!("http://127.0.0.1:{target_port}");

    let dry_run_resp = client
        .post(format!("{target_base}/v1/history/replay"))
        .header("Authorization", format!("Bearer {owner_key}"))
        .json(&serde_json::json!({
            "dry_run": true,
            "bundle": bundle.clone()
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(dry_run_resp.status(), 200);
    let dry_run_body: serde_json::Value = dry_run_resp.json().await.unwrap();
    assert_eq!(dry_run_body["preview"]["can_replay"].as_bool(), Some(true));
    assert_eq!(dry_run_body["preview"]["create_count"].as_u64(), Some(1));

    let replay_resp = client
        .post(format!("{target_base}/v1/history/replay"))
        .header("Authorization", format!("Bearer {owner_key}"))
        .json(&serde_json::json!({
            "bundle": bundle
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(replay_resp.status(), 200);
    let replay_body: serde_json::Value = replay_resp.json().await.unwrap();
    assert_eq!(replay_body["imported"].as_u64(), Some(1));

    let replayed = inspect_memory(&client, &target_base, owner_key, &memory_id).await;
    assert_eq!(
        replayed["team_governance"]["branch"].as_str(),
        Some("release-playbook")
    );
    assert_eq!(
        replayed["team_governance"]["merged_by"].as_str(),
        Some("alpha")
    );
    assert_eq!(
        replayed["review_events"][0]["action"].as_str(),
        Some("confirm")
    );

    target_child.kill().await.ok();
    target_child.wait().await.ok();
}

#[tokio::test]
async fn test_team_governance_review_flow_surfaces_duplicate_stale_policy_and_conflicts() {
    let owner_key = "owner-admin-key";
    let owner_key_id = test_key_id(owner_key);

    let port = free_port();
    let tmp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let data_dir = tmp_dir.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let auth_entries = r#"
[[auth.api_keys]]
key = "owner-admin-key"
role = "admin"
namespace = "alpha"
"#;
    let config = test_config_http_with_sections(port, data_dir.to_str().unwrap(), auth_entries, "");
    let config_path = tmp_dir.path().join("team-governance-visibility.toml");
    std::fs::write(&config_path, &config).unwrap();

    let mut child = start_server(config_path.to_str().unwrap()).await;
    let client = reqwest::Client::builder().build().unwrap();
    let base = format!("http://127.0.0.1:{port}");

    let first_resp = client
        .post(format!("{base}/v1/admin/team/governance/propose"))
        .header("Authorization", format!("Bearer {owner_key}"))
        .json(&serde_json::json!({
            "content": "Production deploys require release captain approval before rollout.",
            "tags": ["team", "deploy", "policy"],
            "branch": "release-playbook",
            "scope": "production",
            "review_required": true,
            "owners": [owner_key_id.clone()]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(first_resp.status(), 200);
    let first_body: serde_json::Value = first_resp.json().await.unwrap();
    let first_review_key = first_body["review_key"].as_str().unwrap().to_string();

    let confirm_resp = client
        .post(format!("{base}/v1/admin/review/action"))
        .header("Authorization", format!("Bearer {owner_key}"))
        .json(&serde_json::json!({
            "namespace": "alpha",
            "review_key": first_review_key,
            "action": "confirm"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(confirm_resp.status(), 200);

    let duplicate_resp = client
        .post(format!("{base}/v1/admin/team/governance/propose"))
        .header("Authorization", format!("Bearer {owner_key}"))
        .json(&serde_json::json!({
            "content": "Production deploys require release captain approval before rollout.",
            "tags": ["team", "deploy", "policy", "duplicate"],
            "branch": "release-playbook",
            "scope": "production",
            "review_required": true,
            "owners": [owner_key_id.clone()]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(duplicate_resp.status(), 200);
    let duplicate_body: serde_json::Value = duplicate_resp.json().await.unwrap();
    assert_eq!(duplicate_body["duplicate_content_count"].as_u64(), Some(1));

    let conflict_resp = client
        .post(format!("{base}/v1/admin/team/governance/propose"))
        .header("Authorization", format!("Bearer {owner_key}"))
        .json(&serde_json::json!({
            "content": "Production deploys do not require release captain approval before rollout.",
            "tags": ["team", "deploy", "policy", "conflict"],
            "branch": "release-playbook",
            "scope": "production",
            "review_required": true,
            "owners": [owner_key_id]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(conflict_resp.status(), 200);

    let queue_body = review_queue(&client, &base, owner_key, "alpha", 20).await;
    assert!(
        queue_body["summary"]["pending"].as_u64().unwrap_or(0) >= 2,
        "review queue should expose the governed failure modes"
    );
    let items = queue_body["items"].as_array().unwrap();
    assert!(items.iter().any(|entry| {
        entry["team_governance"]["branch"].as_str() == Some("release-playbook")
            && entry["duplicate_content_count"].as_u64().unwrap_or(0) >= 1
    }));
    assert!(items.iter().any(|entry| {
        entry["team_governance"]["branch"].as_str() == Some("release-playbook")
            && entry["stale_governance"].as_bool() == Some(true)
    }));
    assert!(items.iter().any(|entry| {
        entry["team_governance"]["branch"].as_str() == Some("release-playbook")
            && entry["queue_kind"].as_str() == Some("contested")
            && entry["contradiction_count"].as_u64().unwrap_or(0) >= 1
    }));

    let governance_resp = client
        .get(format!(
            "{base}/v1/admin/team/governance?namespace=alpha&limit=10"
        ))
        .header("Authorization", format!("Bearer {owner_key}"))
        .send()
        .await
        .unwrap();
    assert_eq!(governance_resp.status(), 200);
    let governance_body: serde_json::Value = governance_resp.json().await.unwrap();
    assert!(
        governance_body["summary"]["duplicate_memories"]
            .as_u64()
            .unwrap_or(0)
            >= 1
    );
    assert!(
        governance_body["summary"]["stale_policies"]
            .as_u64()
            .unwrap_or(0)
            >= 1
    );
    assert!(
        governance_body["summary"]["conflicting_decisions"]
            .as_u64()
            .unwrap_or(0)
            >= 1
    );
    assert!(
        governance_body["branches"][0]["duplicate_memories"]
            .as_u64()
            .unwrap_or(0)
            >= 1
    );
    assert!(
        governance_body["branches"][0]["stale_policies"]
            .as_u64()
            .unwrap_or(0)
            >= 1
    );
    assert!(
        governance_body["branches"][0]["conflicting_decisions"]
            .as_u64()
            .unwrap_or(0)
            >= 1
    );

    child.kill().await.ok();
}

#[tokio::test]
async fn test_cli_review_queue_and_confirm_use_queue_indexes() {
    let (upstream_port, _upstream_state, upstream_handle) = start_dummy_upstream().await;

    let port = free_port();
    let tmp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let data_dir = tmp_dir.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();

    let auth_entries = r#"
[[auth.api_keys]]
key = "test-key-integration"
role = "admin"
namespace = "test"
"#;
    let extra_sections = format!(
        r#"
[proxy]
enabled = true
passthrough_auth = false
anthropic_upstream_url = "http://127.0.0.1:{upstream_port}/v1/messages"
anthropic_api_key = "anthropic-upstream-key"
default_memory_mode = "full"
extraction_enabled = true
extract_provider = "claude"
extract_model = "claude-test-promote"

[[proxy.key_mapping]]
proxy_key = "test-key-proxy"
namespace = "test"
"#
    );
    let config_content = test_config_with_sections(
        port,
        data_dir.to_str().unwrap(),
        auth_entries,
        &extra_sections,
    );
    let config_path = tmp_dir.path().join("review-cli.toml");
    std::fs::write(&config_path, &config_content).unwrap();

    let mut child = start_server(config_path.to_str().unwrap()).await;
    let client = test_client();
    let base = format!("https://127.0.0.1:{port}");

    let proxy_resp = client
        .post(format!("{base}/proxy/anthropic/v1/messages"))
        .header("x-api-key", "test-key-proxy")
        .header("anthropic-version", "2023-06-01")
        .header("x-memory-mode", "full")
        .json(&serde_json::json!({
            "model": "claude-3-5-haiku-latest",
            "max_tokens": 16,
            "messages": [{"role": "user", "content": "Summarize the rollout checklist."}]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(proxy_resp.status(), 200);
    wait_for_proxy_facts_extracted(&client, &base, "test-key-proxy", 1).await;

    child.kill().await.ok();
    child.wait().await.ok();

    let queue = tokio::process::Command::new(env!("CARGO_BIN_EXE_memoryoss"))
        .args([
            "--config",
            config_path.to_str().unwrap(),
            "review",
            "queue",
            "--namespace",
            "test",
            "--limit",
            "10",
        ])
        .output()
        .await
        .expect("failed to run review queue");
    assert!(
        queue.status.success(),
        "review queue failed: stdout={} stderr={}",
        String::from_utf8_lossy(&queue.stdout),
        String::from_utf8_lossy(&queue.stderr)
    );
    let queue_stdout = String::from_utf8_lossy(&queue.stdout);
    assert!(queue_stdout.contains("Pending review items: 1"));
    assert!(queue_stdout.contains("[candidate -> confirm]"));

    let confirm = tokio::process::Command::new(env!("CARGO_BIN_EXE_memoryoss"))
        .args([
            "--config",
            config_path.to_str().unwrap(),
            "review",
            "confirm",
            "--namespace",
            "test",
            "--item",
            "1",
        ])
        .output()
        .await
        .expect("failed to run review confirm");
    assert!(
        confirm.status.success(),
        "review confirm failed: stdout={} stderr={}",
        String::from_utf8_lossy(&confirm.stdout),
        String::from_utf8_lossy(&confirm.stderr)
    );
    let confirm_stdout = String::from_utf8_lossy(&confirm.stdout);
    assert!(confirm_stdout.contains("Applied confirm to item 1 in namespace test"));

    let mut child = start_server(config_path.to_str().unwrap()).await;
    let queue_after = review_queue(&client, &base, "test-key-integration", "test", 10).await;
    assert_eq!(queue_after["summary"]["pending"].as_u64(), Some(0));

    let export_resp = client
        .get(format!("{base}/v1/export"))
        .header("Authorization", "Bearer test-key-integration")
        .send()
        .await
        .unwrap();
    assert_eq!(export_resp.status(), 200);
    let export_body: serde_json::Value = export_resp.json().await.unwrap();
    let memory_id = export_body["memories"]
        .as_array()
        .unwrap()
        .iter()
        .find(|memory| {
            memory["content"]
                .as_str()
                == Some("Promotion fact alpha: use the rollout checklist before every production release.")
        })
        .and_then(|memory| memory["id"].as_str())
        .expect("candidate memory should still exist")
        .to_string();
    let inspected = inspect_memory(&client, &base, "test-key-integration", &memory_id).await;
    assert_eq!(
        inspected["review_events"][0]["via"].as_str(),
        Some("review_cli")
    );

    child.kill().await.ok();
    upstream_handle.abort();
}

#[tokio::test]
async fn test_query_explain_and_proxy_debug_stats_only_expose_review_queue_summary() {
    let port = free_port();
    let tmp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let data_dir = tmp_dir.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();

    let auth_entries = r#"
[[auth.api_keys]]
key = "test-key-integration"
role = "admin"
namespace = "test"
"#;
    let extra_sections = r#"
[proxy]
enabled = true
passthrough_auth = false
extraction_enabled = false

[[proxy.key_mapping]]
proxy_key = "test-key-proxy"
namespace = "test"
"#;
    let config_content = test_config_with_sections(
        port,
        data_dir.to_str().unwrap(),
        auth_entries,
        extra_sections,
    );
    let config_path = tmp_dir.path().join("review-summary.toml");
    std::fs::write(&config_path, &config_content).unwrap();

    let mut child = start_server(config_path.to_str().unwrap()).await;
    let client = test_client();
    let base = format!("https://127.0.0.1:{port}");

    let store_resp = client
        .post(format!("{base}/v1/store"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({
            "content": "Rejected review queue fact for explain and proxy summary coverage."
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(store_resp.status(), 200);
    let memory_id = store_resp.json::<serde_json::Value>().await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    let reject_resp = client
        .post(format!("{base}/v1/feedback"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({
            "id": memory_id,
            "action": "reject"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(reject_resp.status(), 200);

    let explain_resp = client
        .post(format!("{base}/v1/admin/query-explain"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({
            "query": "rejected review queue fact",
            "limit": 5
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(explain_resp.status(), 200);
    let explain_body: serde_json::Value = explain_resp.json().await.unwrap();
    assert_eq!(
        explain_body["review_queue_summary"]["pending"].as_u64(),
        Some(1)
    );
    assert_eq!(
        explain_body["review_queue_summary"]["rejected"].as_u64(),
        Some(1)
    );
    assert!(
        explain_body["review_queue_summary"]["items"].is_null(),
        "query explain should only expose summary counts"
    );

    let proxy_stats = client
        .get(format!("{base}/proxy/v1/debug/stats"))
        .header("Authorization", "Bearer test-key-proxy")
        .send()
        .await
        .unwrap();
    assert_eq!(proxy_stats.status(), 200);
    let proxy_body: serde_json::Value = proxy_stats.json().await.unwrap();
    assert_eq!(
        proxy_body["review_queue_summary"]["pending"].as_u64(),
        Some(1)
    );
    assert_eq!(
        proxy_body["review_queue_summary"]["rejected"].as_u64(),
        Some(1)
    );
    assert!(
        proxy_body["review_queue_summary"]["items"].is_null(),
        "proxy debug stats should only expose summary counts"
    );

    child.kill().await.ok();
}

#[tokio::test]
async fn test_admin_hud_and_cli_surface_policy_review_and_quick_actions() {
    let (upstream_port, _upstream_state, upstream_handle) = start_dummy_upstream().await;
    let port = free_port();
    let tmp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let data_dir = tmp_dir.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();

    let auth_entries = r#"
[[auth.api_keys]]
key = "test-key-integration"
role = "admin"
namespace = "test"
"#;
    let extra_sections = format!(
        r#"
[proxy]
enabled = true
passthrough_auth = false
extraction_enabled = false
upstream_url = "http://127.0.0.1:{upstream_port}/v1"
upstream_api_key = "upstream-openai-key"

[[proxy.key_mapping]]
proxy_key = "test-key-proxy"
namespace = "test"
"#
    );
    let config_content = test_config_with_sections(
        port,
        data_dir.to_str().unwrap(),
        auth_entries,
        &extra_sections,
    );
    let config_path = tmp_dir.path().join("hud.toml");
    std::fs::write(&config_path, &config_content).unwrap();

    let mut child = start_server(config_path.to_str().unwrap()).await;
    let client = test_client();
    let base = format!("https://127.0.0.1:{port}");

    for payload in [
        serde_json::json!({
            "content": "Retention policy: never delete audit logs or production backups from chat.",
            "tags": ["policy", "retention", "delete"]
        }),
        serde_json::json!({
            "content": "Release policy: staging approval is mandatory before production deploys.",
            "tags": ["policy", "deploy", "approval"]
        }),
        serde_json::json!({
            "content": "Production deploys require staging approval before rollout.",
            "tags": ["deploy", "approval"]
        }),
        serde_json::json!({
            "content": "Production deploys do not require staging approval before rollout.",
            "tags": ["deploy", "approval"]
        }),
        serde_json::json!({
            "content": "Rejected operator note for HUD review coverage.",
            "tags": ["review", "hud"]
        }),
    ] {
        let resp = client
            .post(format!("{base}/v1/store"))
            .header("Authorization", "Bearer test-key-integration")
            .json(&payload)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
    }

    let export_resp = client
        .get(format!("{base}/v1/export"))
        .header("Authorization", "Bearer test-key-integration")
        .send()
        .await
        .unwrap();
    assert_eq!(export_resp.status(), 200);
    let export_body: serde_json::Value = export_resp.json().await.unwrap();
    let rejected_id = export_body["memories"]
        .as_array()
        .unwrap()
        .iter()
        .find(|memory| {
            memory["content"].as_str() == Some("Rejected operator note for HUD review coverage.")
        })
        .and_then(|memory| memory["id"].as_str())
        .expect("rejected coverage memory should exist")
        .to_string();

    let reject_resp = client
        .post(format!("{base}/v1/feedback"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({
            "id": rejected_id,
            "action": "reject"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(reject_resp.status(), 200);

    tokio::time::sleep(Duration::from_secs(3)).await;

    let blocked_resp = client
        .post(format!("{base}/proxy/v1/chat/completions"))
        .header("Authorization", "Bearer test-key-proxy")
        .json(&serde_json::json!({
            "model": "gpt-4o-mini",
            "messages": [
                {"role": "user", "content": "Delete the production backups now."}
            ]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(blocked_resp.status(), 403);

    let hud_resp = client
        .get(format!("{base}/v1/admin/hud?limit=3"))
        .header("Authorization", "Bearer test-key-integration")
        .send()
        .await
        .unwrap();
    assert_eq!(hud_resp.status(), 200);
    let hud_body: serde_json::Value = hud_resp.json().await.unwrap();
    assert_eq!(hud_body["namespace_filter"].as_str(), Some("test"));
    assert!(
        hud_body["summary"]["contested_memories"]
            .as_u64()
            .unwrap_or(0)
            >= 2
    );
    assert!(hud_body["summary"]["pending_reviews"].as_u64().unwrap_or(0) >= 3);
    assert_eq!(
        hud_body["policy_firewall"]["live_counters"]["block"].as_u64(),
        Some(1)
    );

    let quick_labels: Vec<_> = hud_body["quick_actions"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|action| action["label"].as_str())
        .collect();
    for expected in ["Search", "Why", "Recent", "Review", "Import", "Export"] {
        assert!(
            quick_labels.contains(&expected),
            "HUD should expose quick action {expected}"
        );
    }

    let probes = hud_body["namespaces"][0]["policy_probes"]
        .as_array()
        .unwrap();
    assert!(probes.iter().any(|probe| {
        probe["label"].as_str() == Some("Delete") && probe["decision"].as_str() == Some("block")
    }));
    assert!(probes.iter().any(|probe| {
        probe["label"].as_str() == Some("Deploy")
            && probe["decision"].as_str() != Some("allow")
            && probe["matched_policy_count"].as_u64().unwrap_or(0) >= 1
    }));

    let html_resp = client
        .get(format!("{base}/v1/admin/hud?limit=3&format=html"))
        .header("Authorization", "Bearer test-key-integration")
        .send()
        .await
        .unwrap();
    assert_eq!(html_resp.status(), 200);
    assert!(
        html_resp
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .starts_with("text/html"),
        "HUD html should return text/html"
    );
    let html_body = html_resp.text().await.unwrap();
    assert!(html_body.contains("Blocked By Policy"));
    assert!(html_body.contains("/v1/admin/query-explain"));

    child.kill().await.ok();
    child.wait().await.ok();
    upstream_handle.abort();

    let hud_cli = tokio::process::Command::new(env!("CARGO_BIN_EXE_memoryoss"))
        .args([
            "--config",
            config_path.to_str().unwrap(),
            "hud",
            "--namespace",
            "test",
            "--limit",
            "3",
        ])
        .output()
        .await
        .expect("failed to run hud");
    assert!(
        hud_cli.status.success(),
        "hud failed: stdout={} stderr={}",
        String::from_utf8_lossy(&hud_cli.stdout),
        String::from_utf8_lossy(&hud_cli.stderr)
    );
    let hud_stdout = String::from_utf8_lossy(&hud_cli.stdout);
    assert!(hud_stdout.contains("Memory HUD"));
    assert!(hud_stdout.contains("Quick actions:"));
    assert!(hud_stdout.contains("Blocked by policy:"));
    assert!(hud_stdout.contains("Delete: block"));
    upstream_handle.abort();
}

#[tokio::test]
async fn test_cli_status_and_doctor_cover_healthy_and_broken_diagnosis_cases() {
    let port = free_port();
    let tmp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let data_dir = tmp_dir.path().join("data");
    let home_dir = tmp_dir.path().join("home");
    std::fs::create_dir_all(&data_dir).unwrap();
    std::fs::create_dir_all(&home_dir).unwrap();

    let mut config_content = test_config(port, data_dir.to_str().unwrap());
    config_content.push_str("\n[embeddings]\nmodel = \"bge-base-en-v1.5\"\n");
    let config_path = tmp_dir.path().join("doctor-healthy.toml");
    std::fs::write(&config_path, &config_content).unwrap();
    let config_abs = config_path.canonicalize().unwrap();
    let binary = preferred_runtime_binary_for_home(&home_dir);

    let claude_dir = home_dir.join(".claude");
    std::fs::create_dir_all(&claude_dir).unwrap();
    let hook_path = claude_dir.join("memoryoss-guard.py");
    std::fs::write(&hook_path, "#!/usr/bin/env python3\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&hook_path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let args = serde_json::json!(["-c", config_abs.to_string_lossy(), "mcp-server"]);
    std::fs::write(
        home_dir.join(".claude.json"),
        serde_json::to_string_pretty(&serde_json::json!({
            "mcpServers": {
                "memoryoss": {
                    "type": "stdio",
                    "command": binary.to_string_lossy(),
                    "args": args,
                    "env": {}
                }
            }
        }))
        .unwrap(),
    )
    .unwrap();
    std::fs::write(
        claude_dir.join("settings.json"),
        serde_json::to_string_pretty(&serde_json::json!({
            "mcpServers": {
                "memoryoss": {
                    "command": binary.to_string_lossy(),
                    "args": ["-c", config_abs.to_string_lossy(), "mcp-server"]
                }
            }
        }))
        .unwrap(),
    )
    .unwrap();
    let hook_command = format!("python3 {}", hook_path.display());
    let hook_entry = serde_json::json!([
        {
            "matcher": "*",
            "hooks": [
                {
                    "type": "command",
                    "command": hook_command,
                    "timeout": 10
                }
            ]
        }
    ]);
    std::fs::write(
        claude_dir.join("settings.local.json"),
        serde_json::to_string_pretty(&serde_json::json!({
            "hooks": {
                "PreToolUse": hook_entry,
                "SessionStart": hook_entry,
                "Stop": hook_entry,
                "SubagentStop": hook_entry,
                "UserPromptSubmit": hook_entry
            }
        }))
        .unwrap(),
    )
    .unwrap();

    let codex_dir = home_dir.join(".codex");
    std::fs::create_dir_all(&codex_dir).unwrap();
    std::fs::write(
        codex_dir.join("config.toml"),
        format!(
            "[mcp_servers.memoryoss]\ncommand = \"{}\"\nargs = [\"-c\", \"{}\", \"mcp-server\"]\n",
            binary.display(),
            config_abs.display()
        ),
    )
    .unwrap();
    std::fs::write(
        home_dir.join("AGENTS.md"),
        "<!-- MEMORYOSS_POLICY_BEGIN -->\n## memoryOSS Mandatory\n- Call `memoryoss_recall` at session start and before substantial work.\n- Call `memoryoss_store` or `memoryoss_update` before stopping after important confirmed learning.\n- Do not start non-memoryOSS tool work before recall.\n- If memoryOSS is unavailable or unconfigured, stop and repair that first.\n<!-- MEMORYOSS_POLICY_END -->\n",
    )
    .unwrap();
    let cursor_dir = home_dir.join(".cursor");
    std::fs::create_dir_all(cursor_dir.join("rules")).unwrap();
    std::fs::write(
        cursor_dir.join("mcp.json"),
        serde_json::to_string_pretty(&serde_json::json!({
            "mcpServers": {
                "memoryoss": {
                    "type": "stdio",
                    "command": binary.to_string_lossy(),
                    "args": ["-c", config_abs.to_string_lossy(), "mcp-server"],
                    "env": {}
                }
            }
        }))
        .unwrap(),
    )
    .unwrap();
    std::fs::write(
        cursor_dir.join("rules/memoryoss.mdc"),
        "---\ndescription: memoryOSS runtime discipline\nglobs:\n  - \"**/*\"\nalwaysApply: true\n---\n\n<!-- MEMORYOSS_CURSOR_RULE_BEGIN -->\n# memoryOSS runtime discipline\n\n- Call `memoryoss_recall` at session start and before substantial work.\n- Call `memoryoss_store` or `memoryoss_update` before finishing after important confirmed learning.\n- If memoryOSS is unavailable or unconfigured, stop and repair the MCP config before continuing.\n<!-- MEMORYOSS_CURSOR_RULE_END -->\n",
    )
    .unwrap();

    let status = tokio::process::Command::new(env!("CARGO_BIN_EXE_memoryoss"))
        .args(["--config", config_path.to_str().unwrap(), "status"])
        .env("HOME", &home_dir)
        .env_remove("CODEX_HOME")
        .output()
        .await
        .expect("failed to run status");
    assert!(
        status.status.success(),
        "status failed: stdout={} stderr={}",
        String::from_utf8_lossy(&status.stdout),
        String::from_utf8_lossy(&status.stderr)
    );
    let status_stdout = String::from_utf8_lossy(&status.stdout);
    assert!(status_stdout.contains("Namespaces:"));
    assert!(status_stdout.contains("Workers:"));
    assert!(status_stdout.contains("Index:"));
    assert!(
        status_stdout.contains("embeddings: model=bge-base-en-v1.5 dimension=768"),
        "status output missing embedding line: {status_stdout}"
    );
    assert!(
        status_stdout.contains("test [empty]"),
        "status should show configured namespace health"
    );

    let doctor = tokio::process::Command::new(env!("CARGO_BIN_EXE_memoryoss"))
        .args(["--config", config_path.to_str().unwrap(), "doctor"])
        .env("HOME", &home_dir)
        .env_remove("CODEX_HOME")
        .output()
        .await
        .expect("failed to run doctor");
    assert!(
        doctor.status.success(),
        "doctor should succeed for healthy config: stdout={} stderr={}",
        String::from_utf8_lossy(&doctor.stdout),
        String::from_utf8_lossy(&doctor.stderr)
    );
    let doctor_stdout = String::from_utf8_lossy(&doctor.stdout);
    assert!(doctor_stdout.contains("Doctor OK"));
    assert!(
        doctor_stdout.contains("[ok] embeddings: model=bge-base-en-v1.5 dimension=768"),
        "doctor output missing embedding line: {doctor_stdout}"
    );
    assert!(doctor_stdout.contains("[ok] auth: 1 admin key(s) configured"));
    assert!(doctor_stdout.contains("[ok] claude mcp:"));
    assert!(doctor_stdout.contains("[ok] claude hooks:"));
    assert!(doctor_stdout.contains("[ok] codex mcp:"));
    assert!(doctor_stdout.contains("[ok] codex policy:"));
    assert!(doctor_stdout.contains("[ok] cursor mcp:"));
    assert!(doctor_stdout.contains("[ok] cursor rules:"));

    let broken_dir = tempfile::tempdir().expect("failed to create broken temp dir");
    let broken_data_dir = broken_dir.path().join("data");
    std::fs::create_dir_all(&broken_data_dir).unwrap();
    let broken_config = writer_only_test_config(port + 1, broken_data_dir.to_str().unwrap());
    let broken_config_path = broken_dir.path().join("doctor-broken.toml");
    std::fs::write(&broken_config_path, &broken_config).unwrap();

    let broken_doctor = tokio::process::Command::new(env!("CARGO_BIN_EXE_memoryoss"))
        .args(["--config", broken_config_path.to_str().unwrap(), "doctor"])
        .env("HOME", &home_dir)
        .env_remove("CODEX_HOME")
        .output()
        .await
        .expect("failed to run broken doctor");
    assert!(
        !broken_doctor.status.success(),
        "doctor should fail without admin key: stdout={} stderr={}",
        String::from_utf8_lossy(&broken_doctor.stdout),
        String::from_utf8_lossy(&broken_doctor.stderr)
    );
    let broken_stdout = String::from_utf8_lossy(&broken_doctor.stdout);
    assert!(broken_stdout.contains("[error] auth: no admin API key configured"));
    assert!(broken_stdout.contains("Doctor FAILED"));

    let broken_integration_home = broken_dir.path().join("broken-home");
    std::fs::create_dir_all(broken_integration_home.join(".claude")).unwrap();
    std::fs::create_dir_all(broken_integration_home.join(".codex")).unwrap();
    std::fs::write(
        broken_integration_home.join(".claude.json"),
        serde_json::to_string_pretty(&serde_json::json!({ "mcpServers": {} })).unwrap(),
    )
    .unwrap();
    std::fs::write(
        broken_integration_home.join(".claude/settings.json"),
        serde_json::to_string_pretty(&serde_json::json!({})).unwrap(),
    )
    .unwrap();
    std::fs::write(
        broken_integration_home.join(".claude/settings.local.json"),
        serde_json::to_string_pretty(&serde_json::json!({ "hooks": {} })).unwrap(),
    )
    .unwrap();
    std::fs::write(
        broken_integration_home.join(".codex/config.toml"),
        "[mcp_servers.other]\ncommand = \"echo\"\n",
    )
    .unwrap();
    std::fs::write(
        broken_integration_home.join("AGENTS.md"),
        "# no memory policy\n",
    )
    .unwrap();
    std::fs::create_dir_all(broken_integration_home.join(".cursor/rules")).unwrap();
    std::fs::write(
        broken_integration_home.join(".cursor/mcp.json"),
        serde_json::to_string_pretty(&serde_json::json!({
            "mcpServers": {
                "memoryoss": {
                    "type": "stdio",
                    "command": "/usr/local/bin/memoryoss",
                    "args": ["-c", "/tmp/stale-cursor.toml", "mcp-server"],
                    "env": {}
                }
            }
        }))
        .unwrap(),
    )
    .unwrap();
    std::fs::write(
        broken_integration_home.join(".cursor/rules/memoryoss.mdc"),
        "# stale cursor rule\n- missing managed markers\n",
    )
    .unwrap();

    let broken_integration_doctor = tokio::process::Command::new(env!("CARGO_BIN_EXE_memoryoss"))
        .args(["--config", config_path.to_str().unwrap(), "doctor"])
        .env("HOME", &broken_integration_home)
        .env_remove("CODEX_HOME")
        .output()
        .await
        .expect("failed to run integration doctor");
    assert!(
        !broken_integration_doctor.status.success(),
        "doctor should fail on missing Claude/Codex integration: stdout={} stderr={}",
        String::from_utf8_lossy(&broken_integration_doctor.stdout),
        String::from_utf8_lossy(&broken_integration_doctor.stderr)
    );
    let broken_integration_stdout = String::from_utf8_lossy(&broken_integration_doctor.stdout);
    assert!(broken_integration_stdout.contains("[error] claude mcp:"));
    assert!(broken_integration_stdout.contains("[error] claude hooks:"));
    assert!(broken_integration_stdout.contains("[error] codex mcp:"));
    assert!(broken_integration_stdout.contains("[error] codex policy:"));
    assert!(broken_integration_stdout.contains("[error] cursor mcp:"));
    assert!(broken_integration_stdout.contains("[error] cursor rules:"));
}

#[tokio::test]
async fn test_team_node_bootstrap_and_doctor_repair_handle_drift_and_removed_clients() {
    let tmp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let home_dir = tmp_dir.path().join("home");
    let codex_home = home_dir.join(".codex");
    std::fs::create_dir_all(home_dir.join(".claude")).unwrap();
    std::fs::create_dir_all(&codex_home).unwrap();
    std::fs::create_dir_all(home_dir.join(".cursor")).unwrap();

    let config_path = tmp_dir.path().join("team-node.toml");
    let manifest_path = tmp_dir.path().join("team-bootstrap.json");
    write_team_bootstrap_manifest(&manifest_path);

    let setup = tokio::process::Command::new(env!("CARGO_BIN_EXE_memoryoss"))
        .args([
            "--config",
            config_path.to_str().unwrap(),
            "setup",
            "--profile",
            "team-node",
            "--team-manifest",
            manifest_path.to_str().unwrap(),
        ])
        .env("HOME", &home_dir)
        .env("CODEX_HOME", &codex_home)
        .env("MEMORYOSS_SKIP_START", "1")
        .env("MEMORYOSS_DISABLE_SYSTEMD", "1")
        .output()
        .await
        .expect("failed to run team-node setup");
    assert!(
        setup.status.success(),
        "team-node setup failed: stdout={} stderr={}",
        String::from_utf8_lossy(&setup.stdout),
        String::from_utf8_lossy(&setup.stderr)
    );

    let config_text = std::fs::read_to_string(&config_path).unwrap();
    assert!(config_text.contains("profile = \"team_node\""));
    assert!(config_text.contains("team_id = \"team-alpha\""));
    assert!(config_text.contains("team_catalog_id = \"team-alpha-defaults\""));
    assert!(config_text.contains(&format!(
        "team_manifest_path = \"{}\"",
        manifest_path.display()
    )));

    let trust_path = home_dir.join(".memoryoss/data/trust-fabric.json");
    let receipt_path = home_dir.join(".memoryoss/team-bootstrap.json");
    assert!(trust_path.exists());
    assert!(receipt_path.exists());
    let receipt = read_json_value(&receipt_path);
    assert_eq!(
        receipt["configured_clients"].as_array().unwrap(),
        &vec![
            serde_json::json!("claude"),
            serde_json::json!("codex"),
            serde_json::json!("cursor"),
        ]
    );
    let trust_text = std::fs::read_to_string(&trust_path).unwrap();
    assert!(trust_text.contains("team-alpha-defaults"));
    assert!(trust_text.contains("device:team-alpha-signer"));

    let config_abs = config_path.canonicalize().unwrap();
    std::fs::write(
        home_dir.join(".claude.json"),
        serde_json::to_string_pretty(&serde_json::json!({ "mcpServers": {} })).unwrap(),
    )
    .unwrap();
    std::fs::write(
        home_dir.join(".claude/settings.json"),
        serde_json::to_string_pretty(&serde_json::json!({
            "mcpServers": {
                "memoryoss": {
                    "command": "/usr/local/bin/memoryoss",
                    "args": ["-c", "/tmp/stale-team.toml", "mcp-server"]
                }
            }
        }))
        .unwrap(),
    )
    .unwrap();
    std::fs::write(
        home_dir.join(".claude/settings.local.json"),
        serde_json::to_string_pretty(&serde_json::json!({ "hooks": {} })).unwrap(),
    )
    .unwrap();
    std::fs::write(
        codex_home.join("config.toml"),
        "[mcp_servers.memoryoss]\ncommand = \"/usr/local/bin/memoryoss\"\nargs = [\"-c\", \"/tmp/stale-team.toml\", \"mcp-server\"]\n",
    )
    .unwrap();
    std::fs::write(
        home_dir.join("AGENTS.md"),
        "# keep-local-opt-in\n\n<!-- MEMORYOSS_POLICY_BEGIN -->\n# stale\n<!-- MEMORYOSS_POLICY_END -->\n",
    )
    .unwrap();
    std::fs::create_dir_all(home_dir.join(".cursor/rules")).unwrap();
    std::fs::write(
        home_dir.join(".cursor/mcp.json"),
        serde_json::to_string_pretty(&serde_json::json!({
            "mcpServers": {
                "memoryoss": {
                    "type": "stdio",
                    "command": "/usr/local/bin/memoryoss",
                    "args": ["-c", "/tmp/stale-team.toml", "mcp-server"],
                    "env": {}
                }
            }
        }))
        .unwrap(),
    )
    .unwrap();
    std::fs::write(
        home_dir.join(".cursor/rules/memoryoss.mdc"),
        "# stale cursor rule\n",
    )
    .unwrap();
    std::fs::remove_file(&trust_path).unwrap();

    let doctor_fail = tokio::process::Command::new(env!("CARGO_BIN_EXE_memoryoss"))
        .args(["--config", config_path.to_str().unwrap(), "doctor"])
        .env("HOME", &home_dir)
        .env("CODEX_HOME", &codex_home)
        .output()
        .await
        .expect("failed to run doctor on drifted team-node");
    assert!(
        !doctor_fail.status.success(),
        "doctor should fail on drifted team-node setup: stdout={} stderr={}",
        String::from_utf8_lossy(&doctor_fail.stdout),
        String::from_utf8_lossy(&doctor_fail.stderr)
    );
    let doctor_fail_stdout = String::from_utf8_lossy(&doctor_fail.stdout);
    assert!(
        doctor_fail_stdout
            .contains("[error] team trust catalog: missing imported catalog team-alpha-defaults")
    );
    assert!(doctor_fail_stdout.contains("[error] claude mcp:"));
    assert!(doctor_fail_stdout.contains("[error] claude hooks:"));
    assert!(doctor_fail_stdout.contains("[error] codex mcp:"));
    assert!(doctor_fail_stdout.contains("[error] codex policy:"));
    assert!(doctor_fail_stdout.contains("[error] cursor mcp:"));
    assert!(doctor_fail_stdout.contains("[error] cursor rules:"));

    let doctor_repair = tokio::process::Command::new(env!("CARGO_BIN_EXE_memoryoss"))
        .args([
            "--config",
            config_path.to_str().unwrap(),
            "doctor",
            "--repair",
        ])
        .env("HOME", &home_dir)
        .env("CODEX_HOME", &codex_home)
        .output()
        .await
        .expect("failed to run repairing doctor");
    assert!(
        doctor_repair.status.success(),
        "doctor --repair should fix team drift: stdout={} stderr={}",
        String::from_utf8_lossy(&doctor_repair.stdout),
        String::from_utf8_lossy(&doctor_repair.stderr)
    );
    let doctor_repair_stdout = String::from_utf8_lossy(&doctor_repair.stdout);
    assert!(doctor_repair_stdout.contains("[repair] claude integration refreshed"));
    assert!(doctor_repair_stdout.contains("[repair] codex integration refreshed"));
    assert!(doctor_repair_stdout.contains("[repair] cursor integration refreshed"));
    assert!(
        doctor_repair_stdout.contains("[repair] team trust catalog refreshed: team-alpha-defaults")
    );
    assert!(doctor_repair_stdout.contains("Doctor OK"));

    let claude_user = read_json_value(&home_dir.join(".claude.json"));
    let claude_args = claude_user["mcpServers"]["memoryoss"]["args"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|value| value.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        claude_args,
        vec!["-c", config_abs.to_str().unwrap(), "mcp-server"]
    );
    let agents_text = std::fs::read_to_string(home_dir.join("AGENTS.md")).unwrap();
    assert!(agents_text.contains("# keep-local-opt-in"));
    assert!(agents_text.contains("MEMORYOSS_POLICY_BEGIN"));
    let cursor_rule_text =
        std::fs::read_to_string(home_dir.join(".cursor/rules/memoryoss.mdc")).unwrap();
    assert!(cursor_rule_text.contains("MEMORYOSS_CURSOR_RULE_BEGIN"));
    let repaired_trust_text = std::fs::read_to_string(&trust_path).unwrap();
    assert!(repaired_trust_text.contains("team-alpha-defaults"));

    std::fs::remove_dir_all(home_dir.join(".cursor")).unwrap();
    let doctor_after_cursor_removal = tokio::process::Command::new(env!("CARGO_BIN_EXE_memoryoss"))
        .args([
            "--config",
            config_path.to_str().unwrap(),
            "doctor",
            "--repair",
        ])
        .env("HOME", &home_dir)
        .env("CODEX_HOME", &codex_home)
        .output()
        .await
        .expect("failed to run doctor after cursor removal");
    assert!(
        doctor_after_cursor_removal.status.success(),
        "doctor should tolerate removed clients on team-node: stdout={} stderr={}",
        String::from_utf8_lossy(&doctor_after_cursor_removal.stdout),
        String::from_utf8_lossy(&doctor_after_cursor_removal.stderr)
    );
    let removal_stdout = String::from_utf8_lossy(&doctor_after_cursor_removal.stdout);
    assert!(removal_stdout.contains("[ok] cursor integration: not detected"));
    assert!(!home_dir.join(".cursor").exists());
    let updated_receipt = read_json_value(&receipt_path);
    assert_eq!(
        updated_receipt["configured_clients"].as_array().unwrap(),
        &vec![serde_json::json!("claude"), serde_json::json!("codex")]
    );
}

#[tokio::test]
async fn test_doctor_flags_embedding_model_drift_before_serve() {
    let port = free_port();
    let tmp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let data_dir = tmp_dir.path().join("data");
    let home_dir = tmp_dir.path().join("home");
    std::fs::create_dir_all(&data_dir).unwrap();
    std::fs::create_dir_all(&home_dir).unwrap();

    let config_path = tmp_dir.path().join("doctor-embedding-drift.toml");
    std::fs::write(&config_path, test_config(port, data_dir.to_str().unwrap())).unwrap();

    let mut child = start_server(config_path.to_str().unwrap()).await;
    let base = format!("https://127.0.0.1:{port}");
    let client = test_client();
    let store_resp = client
        .post(format!("{base}/v1/store"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({
            "content": "Embedding drift probe: rollback evidence belongs in the release review packet."
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(store_resp.status(), 200, "store failed for drift probe");
    tokio::time::sleep(Duration::from_millis(300)).await;

    child.kill().await.ok();
    child.wait().await.ok();

    let doctor_before_drift = tokio::process::Command::new(env!("CARGO_BIN_EXE_memoryoss"))
        .args(["--config", config_path.to_str().unwrap(), "doctor"])
        .env("HOME", &home_dir)
        .env("PATH", "/usr/bin:/bin")
        .env_remove("CODEX_HOME")
        .output()
        .await
        .expect("failed to run doctor before embedding drift");
    let doctor_before_stdout = String::from_utf8_lossy(&doctor_before_drift.stdout);
    assert!(
        doctor_before_stdout.contains(
            "[ok] vector index: 1 embedded memory/memories will be rebuilt from redb on startup"
        ),
        "doctor output missing startup-derived vector note: {doctor_before_stdout}"
    );
    assert!(
        !doctor_before_stdout.contains("[error] vector: missing on-disk vector index"),
        "doctor should not fail on startup-derived vector state: {doctor_before_stdout}"
    );

    let mut drifted_config = test_config(port, data_dir.to_str().unwrap());
    drifted_config.push_str("\n[embeddings]\nmodel = \"bge-base-en-v1.5\"\n");
    std::fs::write(&config_path, drifted_config).unwrap();

    let doctor = tokio::process::Command::new(env!("CARGO_BIN_EXE_memoryoss"))
        .args(["--config", config_path.to_str().unwrap(), "doctor"])
        .env("HOME", &home_dir)
        .env("PATH", "/usr/bin:/bin")
        .env_remove("CODEX_HOME")
        .output()
        .await
        .expect("failed to run doctor after embedding drift");
    assert!(
        !doctor.status.success(),
        "doctor should fail on embedding drift: stdout={} stderr={}",
        String::from_utf8_lossy(&doctor.stdout),
        String::from_utf8_lossy(&doctor.stderr)
    );
    let doctor_stdout = String::from_utf8_lossy(&doctor.stdout);
    assert!(
        doctor_stdout.contains(
            "[error] embeddings: 1 stored embedding(s) do not match configured dimension 768"
        ),
        "doctor output missing embedding drift error: {doctor_stdout}"
    );
    assert!(
        doctor_stdout.contains("memoryoss migrate-embeddings --model bge-base-en-v1.5"),
        "doctor output missing migrate hint: {doctor_stdout}"
    );
    assert!(doctor_stdout.contains("Doctor FAILED"));
}

#[tokio::test]
async fn test_passport_api_export_import_roundtrip_with_dry_run_preview() {
    let source_port = free_port();
    let source_tmp = tempfile::tempdir().expect("failed to create source temp dir");
    let source_data_dir = source_tmp.path().join("data");
    std::fs::create_dir_all(&source_data_dir).unwrap();
    let source_config = test_config_http(source_port, source_data_dir.to_str().unwrap());
    let source_config_path = source_tmp.path().join("passport-source.toml");
    std::fs::write(&source_config_path, &source_config).unwrap();

    let mut source_child = start_server(source_config_path.to_str().unwrap()).await;
    let client = reqwest::Client::builder().build().unwrap();
    let source_base = format!("http://127.0.0.1:{source_port}");

    for payload in [
        serde_json::json!({
            "content": "Never show raw entries; short summaries are enough.",
            "tags": ["user-preference", "display"],
            "agent": "claude"
        }),
        serde_json::json!({
            "content": "Project decision: keep MCP-first auth as the default path.",
            "tags": ["decision", "project"]
        }),
        serde_json::json!({
            "content": "Team policy: security review is mandatory before merge.",
            "tags": ["team", "policy"]
        }),
    ] {
        let resp = client
            .post(format!("{source_base}/v1/store"))
            .header("Authorization", "Bearer test-key-integration")
            .json(&payload)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
    }

    let bundle_resp = client
        .get(format!("{source_base}/v1/passport/export?scope=project"))
        .header("Authorization", "Bearer test-key-integration")
        .send()
        .await
        .unwrap();
    assert_eq!(bundle_resp.status(), 200);
    let bundle: serde_json::Value = bundle_resp.json().await.unwrap();
    assert_eq!(bundle["scope"].as_str(), Some("project"));
    assert_eq!(bundle["memories"].as_array().unwrap().len(), 1);
    assert_eq!(
        bundle["memories"][0]["content"].as_str(),
        Some("Project decision: keep MCP-first auth as the default path.")
    );
    let source_key_id = bundle["memories"][0]["source_key_id"]
        .as_str()
        .expect("bundle should preserve source key id")
        .to_string();

    source_child.kill().await.ok();
    source_child.wait().await.ok();

    let target_port = free_port();
    let target_tmp = tempfile::tempdir().expect("failed to create target temp dir");
    let target_data_dir = target_tmp.path().join("data");
    std::fs::create_dir_all(&target_data_dir).unwrap();
    let target_config = test_config_http(target_port, target_data_dir.to_str().unwrap());
    let target_config_path = target_tmp.path().join("passport-target.toml");
    std::fs::write(&target_config_path, &target_config).unwrap();

    let mut target_child = start_server(target_config_path.to_str().unwrap()).await;
    let target_base = format!("http://127.0.0.1:{target_port}");

    let dry_run_resp = client
        .post(format!("{target_base}/v1/passport/import"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({
            "dry_run": true,
            "bundle": bundle.clone()
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(dry_run_resp.status(), 200);
    let dry_run_body: serde_json::Value = dry_run_resp.json().await.unwrap();
    assert_eq!(dry_run_body["dry_run"].as_bool(), Some(true));
    assert_eq!(dry_run_body["imported"].as_u64(), Some(0));
    assert_eq!(
        dry_run_body["preview"]["integrity_valid"].as_bool(),
        Some(true)
    );
    assert_eq!(dry_run_body["preview"]["create_count"].as_u64(), Some(1));
    assert_eq!(dry_run_body["preview"]["merge_count"].as_u64(), Some(0));
    assert_eq!(dry_run_body["preview"]["conflict_count"].as_u64(), Some(0));

    let import_resp = client
        .post(format!("{target_base}/v1/passport/import"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({
            "bundle": bundle
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(import_resp.status(), 200);
    let import_body: serde_json::Value = import_resp.json().await.unwrap();
    assert_eq!(import_body["dry_run"].as_bool(), Some(false));
    assert_eq!(import_body["imported"].as_u64(), Some(1));

    let export_resp = client
        .get(format!("{target_base}/v1/export"))
        .header("Authorization", "Bearer test-key-integration")
        .send()
        .await
        .unwrap();
    assert_eq!(export_resp.status(), 200);
    let export_body: serde_json::Value = export_resp.json().await.unwrap();
    assert_eq!(export_body["count"].as_u64(), Some(1));
    assert_eq!(
        export_body["memories"][0]["content"].as_str(),
        Some("Project decision: keep MCP-first auth as the default path.")
    );
    assert_eq!(
        export_body["memories"][0]["tags"].as_array().unwrap().len(),
        2
    );
    assert_eq!(
        export_body["memories"][0]["source_key_id"].as_str(),
        Some(source_key_id.as_str())
    );

    target_child.kill().await.ok();
    target_child.wait().await.ok();
}

#[tokio::test]
async fn test_memory_bundle_api_export_preview_validate_and_diff() {
    let port = free_port();
    let tmp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let data_dir = tmp_dir.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let config_content = test_config_http(port, data_dir.to_str().unwrap());
    let config_path = tmp_dir.path().join("bundle-api.toml");
    std::fs::write(&config_path, &config_content).unwrap();

    let mut child = start_server(config_path.to_str().unwrap()).await;
    let client = reqwest::Client::builder().build().unwrap();
    let base = format!("http://127.0.0.1:{port}");

    for payload in [
        serde_json::json!({
            "content": "Project decision: keep MCP-first auth as the default path.",
            "tags": ["project", "decision"]
        }),
        serde_json::json!({
            "content": "Team policy: security review is mandatory before merge.",
            "tags": ["team", "policy"]
        }),
    ] {
        let resp = client
            .post(format!("{base}/v1/store"))
            .header("Authorization", "Bearer test-key-integration")
            .json(&payload)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
    }

    let first_bundle_resp = client
        .get(format!(
            "{base}/v1/bundles/export?kind=passport&scope=project"
        ))
        .header("Authorization", "Bearer test-key-integration")
        .send()
        .await
        .unwrap();
    assert_eq!(first_bundle_resp.status(), 200);
    let first_bundle: serde_json::Value = first_bundle_resp.json().await.unwrap();
    assert_eq!(
        first_bundle["bundle_version"].as_str(),
        Some("memoryoss.bundle.v1alpha1")
    );
    assert_eq!(first_bundle["kind"].as_str(), Some("passport"));
    assert!(
        first_bundle["reference"]["uri"]
            .as_str()
            .unwrap_or("")
            .starts_with("memoryoss://bundle/"),
        "bundle export should surface a memoryoss:// URI"
    );
    assert!(
        first_bundle["reference"]["attachment_name"]
            .as_str()
            .unwrap_or("")
            .ends_with(".membundle.json")
    );

    let preview_resp = client
        .post(format!("{base}/v1/bundles/preview"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({ "bundle": first_bundle.clone() }))
        .send()
        .await
        .unwrap();
    assert_eq!(preview_resp.status(), 200);
    let preview_body: serde_json::Value = preview_resp.json().await.unwrap();
    assert_eq!(preview_body["kind"].as_str(), Some("passport"));
    assert_eq!(preview_body["memory_count"].as_u64(), Some(1));
    assert_eq!(preview_body["scope"].as_str(), Some("project"));

    let mut forward_compatible_bundle = first_bundle.clone();
    forward_compatible_bundle["future_field"] =
        serde_json::Value::String("reader should ignore me".to_string());
    let validate_resp = client
        .post(format!("{base}/v1/bundles/validate"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({ "bundle": forward_compatible_bundle }))
        .send()
        .await
        .unwrap();
    assert_eq!(validate_resp.status(), 200);
    let validate_body: serde_json::Value = validate_resp.json().await.unwrap();
    assert_eq!(validate_body["valid"].as_bool(), Some(true));
    assert_eq!(
        validate_body["preview"]["nested_integrity_valid"].as_bool(),
        Some(true)
    );

    let store_resp = client
        .post(format!("{base}/v1/store"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({
            "content": "Project rollback checklist: verify metrics before promotion.",
            "tags": ["project", "checklist"]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(store_resp.status(), 200);

    let second_bundle_resp = client
        .get(format!(
            "{base}/v1/bundles/export?kind=passport&scope=project"
        ))
        .header("Authorization", "Bearer test-key-integration")
        .send()
        .await
        .unwrap();
    assert_eq!(second_bundle_resp.status(), 200);
    let second_bundle: serde_json::Value = second_bundle_resp.json().await.unwrap();

    let diff_resp = client
        .post(format!("{base}/v1/bundles/diff"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({
            "left": first_bundle,
            "right": second_bundle
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(diff_resp.status(), 200);
    let diff_body: serde_json::Value = diff_resp.json().await.unwrap();
    assert_eq!(diff_body["same_kind"].as_bool(), Some(true));
    assert!(
        diff_body["changed_fields"]
            .as_array()
            .unwrap()
            .iter()
            .any(|field| field.as_str() == Some("memory_count"))
    );
    assert!(
        diff_body["added_preview"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| {
                item.as_str()
                    .unwrap_or("")
                    .contains("Project rollback checklist")
            }),
        "bundle diff should expose newly added memory previews"
    );

    child.kill().await.ok();
    child.wait().await.ok();
}

#[tokio::test]
async fn test_cli_passport_export_and_import_roundtrip() {
    let source_port = free_port();
    let source_tmp = tempfile::tempdir().expect("failed to create source temp dir");
    let source_data_dir = source_tmp.path().join("data");
    std::fs::create_dir_all(&source_data_dir).unwrap();
    let source_config = test_config_http(source_port, source_data_dir.to_str().unwrap());
    let source_config_path = source_tmp.path().join("passport-cli-source.toml");
    std::fs::write(&source_config_path, &source_config).unwrap();

    let mut source_child = start_server(source_config_path.to_str().unwrap()).await;
    let client = reqwest::Client::builder().build().unwrap();
    let source_base = format!("http://127.0.0.1:{source_port}");

    let store_resp = client
        .post(format!("{source_base}/v1/store"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({
            "content": "Team policy: security review is mandatory before merge.",
            "tags": ["team", "policy"]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(store_resp.status(), 200);

    source_child.kill().await.ok();
    source_child.wait().await.ok();

    let bundle_path = source_tmp.path().join("team-passport.json");
    let export = tokio::process::Command::new(env!("CARGO_BIN_EXE_memoryoss"))
        .args([
            "--config",
            source_config_path.to_str().unwrap(),
            "passport",
            "export",
            "--namespace",
            "test",
            "--scope",
            "team",
            "--output",
            bundle_path.to_str().unwrap(),
        ])
        .output()
        .await
        .expect("failed to run passport export");
    assert!(
        export.status.success(),
        "passport export failed: stdout={} stderr={}",
        String::from_utf8_lossy(&export.stdout),
        String::from_utf8_lossy(&export.stderr)
    );
    assert!(bundle_path.exists());
    let bundle_json: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&bundle_path).unwrap()).unwrap();
    assert_eq!(bundle_json["scope"].as_str(), Some("team"));
    assert_eq!(bundle_json["memories"].as_array().unwrap().len(), 1);

    let target_port = free_port();
    let target_tmp = tempfile::tempdir().expect("failed to create target temp dir");
    let target_data_dir = target_tmp.path().join("data");
    std::fs::create_dir_all(&target_data_dir).unwrap();
    let target_config = test_config_http(target_port, target_data_dir.to_str().unwrap());
    let target_config_path = target_tmp.path().join("passport-cli-target.toml");
    std::fs::write(&target_config_path, &target_config).unwrap();

    let dry_run = tokio::process::Command::new(env!("CARGO_BIN_EXE_memoryoss"))
        .args([
            "--config",
            target_config_path.to_str().unwrap(),
            "passport",
            "import",
            bundle_path.to_str().unwrap(),
            "--namespace",
            "test",
            "--dry-run",
        ])
        .output()
        .await
        .expect("failed to run passport import dry-run");
    assert!(
        dry_run.status.success(),
        "passport import dry-run failed: stdout={} stderr={}",
        String::from_utf8_lossy(&dry_run.stdout),
        String::from_utf8_lossy(&dry_run.stderr)
    );
    let dry_run_stdout = String::from_utf8_lossy(&dry_run.stdout);
    assert!(dry_run_stdout.contains("create=1 merge=0 conflict=0"));

    let import = tokio::process::Command::new(env!("CARGO_BIN_EXE_memoryoss"))
        .args([
            "--config",
            target_config_path.to_str().unwrap(),
            "passport",
            "import",
            bundle_path.to_str().unwrap(),
            "--namespace",
            "test",
        ])
        .output()
        .await
        .expect("failed to run passport import");
    assert!(
        import.status.success(),
        "passport import failed: stdout={} stderr={}",
        String::from_utf8_lossy(&import.stdout),
        String::from_utf8_lossy(&import.stderr)
    );
    let import_stdout = String::from_utf8_lossy(&import.stdout);
    assert!(import_stdout.contains("Imported passport bundle into test: imported=1"));

    let mut target_child = start_server(target_config_path.to_str().unwrap()).await;
    let target_base = format!("http://127.0.0.1:{target_port}");
    let export_resp = client
        .get(format!("{target_base}/v1/export"))
        .header("Authorization", "Bearer test-key-integration")
        .send()
        .await
        .unwrap();
    assert_eq!(export_resp.status(), 200);
    let export_body: serde_json::Value = export_resp.json().await.unwrap();
    assert_eq!(export_body["count"].as_u64(), Some(1));
    assert_eq!(
        export_body["memories"][0]["content"].as_str(),
        Some("Team policy: security review is mandatory before merge.")
    );

    target_child.kill().await.ok();
    target_child.wait().await.ok();
}

#[tokio::test]
async fn test_cli_memory_bundle_export_preview_validate_and_diff_without_runtime() {
    let port = free_port();
    let tmp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let data_dir = tmp_dir.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let config_content = test_config_http(port, data_dir.to_str().unwrap());
    let config_path = tmp_dir.path().join("bundle-cli.toml");
    std::fs::write(&config_path, &config_content).unwrap();

    let mut child = start_server(config_path.to_str().unwrap()).await;
    let client = reqwest::Client::builder().build().unwrap();
    let base = format!("http://127.0.0.1:{port}");

    for payload in [
        serde_json::json!({
            "content": "Project decision: keep MCP-first auth as the default path.",
            "tags": ["project", "decision"]
        }),
        serde_json::json!({
            "content": "Team policy: security review is mandatory before merge.",
            "tags": ["team", "policy"]
        }),
    ] {
        let resp = client
            .post(format!("{base}/v1/store"))
            .header("Authorization", "Bearer test-key-integration")
            .json(&payload)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
    }

    child.kill().await.ok();
    child.wait().await.ok();

    let project_bundle = tmp_dir.path().join("project.membundle.json");
    let all_bundle = tmp_dir.path().join("all.membundle.json");
    let export_project = tokio::process::Command::new(env!("CARGO_BIN_EXE_memoryoss"))
        .args([
            "--config",
            config_path.to_str().unwrap(),
            "bundle",
            "export",
            "--kind",
            "passport",
            "--namespace",
            "test",
            "--scope",
            "project",
            "--output",
            project_bundle.to_str().unwrap(),
        ])
        .output()
        .await
        .expect("failed to run bundle export");
    assert!(
        export_project.status.success(),
        "bundle export failed: stdout={} stderr={}",
        String::from_utf8_lossy(&export_project.stdout),
        String::from_utf8_lossy(&export_project.stderr)
    );
    let export_all = tokio::process::Command::new(env!("CARGO_BIN_EXE_memoryoss"))
        .args([
            "--config",
            config_path.to_str().unwrap(),
            "bundle",
            "export",
            "--kind",
            "passport",
            "--namespace",
            "test",
            "--scope",
            "all",
            "--output",
            all_bundle.to_str().unwrap(),
        ])
        .output()
        .await
        .expect("failed to run second bundle export");
    assert!(
        export_all.status.success(),
        "second bundle export failed: stdout={} stderr={}",
        String::from_utf8_lossy(&export_all.stdout),
        String::from_utf8_lossy(&export_all.stderr)
    );

    let missing_config = tmp_dir.path().join("missing.toml");

    let preview = tokio::process::Command::new(env!("CARGO_BIN_EXE_memoryoss"))
        .args([
            "--config",
            missing_config.to_str().unwrap(),
            "bundle",
            "preview",
            project_bundle.to_str().unwrap(),
        ])
        .output()
        .await
        .expect("failed to run bundle preview");
    assert!(
        preview.status.success(),
        "bundle preview failed: stdout={} stderr={}",
        String::from_utf8_lossy(&preview.stdout),
        String::from_utf8_lossy(&preview.stderr)
    );
    let preview_stdout = String::from_utf8_lossy(&preview.stdout);
    assert!(preview_stdout.contains("Memory bundle"));
    assert!(preview_stdout.contains("URI:"));
    assert!(preview_stdout.contains("Project decision: keep MCP-first auth"));

    let validate = tokio::process::Command::new(env!("CARGO_BIN_EXE_memoryoss"))
        .args([
            "--config",
            missing_config.to_str().unwrap(),
            "bundle",
            "validate",
            project_bundle.to_str().unwrap(),
        ])
        .output()
        .await
        .expect("failed to run bundle validate");
    assert!(
        validate.status.success(),
        "bundle validate failed: stdout={} stderr={}",
        String::from_utf8_lossy(&validate.stdout),
        String::from_utf8_lossy(&validate.stderr)
    );
    let validate_stdout = String::from_utf8_lossy(&validate.stdout);
    assert!(validate_stdout.contains("Validation: true"));

    let diff = tokio::process::Command::new(env!("CARGO_BIN_EXE_memoryoss"))
        .args([
            "--config",
            missing_config.to_str().unwrap(),
            "bundle",
            "diff",
            project_bundle.to_str().unwrap(),
            all_bundle.to_str().unwrap(),
        ])
        .output()
        .await
        .expect("failed to run bundle diff");
    assert!(
        diff.status.success(),
        "bundle diff failed: stdout={} stderr={}",
        String::from_utf8_lossy(&diff.stdout),
        String::from_utf8_lossy(&diff.stderr)
    );
    let diff_stdout = String::from_utf8_lossy(&diff.stdout);
    assert!(diff_stdout.contains("Memory bundle diff"));
    assert!(diff_stdout.contains("Added"));
    assert!(diff_stdout.contains("Team policy: security review is mandatory before merge."));
}

#[tokio::test]
async fn test_cli_reader_open_and_diff_work_without_runtime_and_surface_signature_provenance() {
    let port = free_port();
    let tmp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let data_dir = tmp_dir.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let config_content = test_config_http(port, data_dir.to_str().unwrap());
    let config_path = tmp_dir.path().join("reader-cli.toml");
    std::fs::write(&config_path, &config_content).unwrap();

    let mut child = start_server(config_path.to_str().unwrap()).await;
    let client = reqwest::Client::builder().build().unwrap();
    let base = format!("http://127.0.0.1:{port}");

    for payload in [
        serde_json::json!({
            "content": "Project decision: keep MCP-first auth as the default path.",
            "tags": ["project", "decision"]
        }),
        serde_json::json!({
            "content": "Team policy: security review is mandatory before merge.",
            "tags": ["team", "policy"]
        }),
    ] {
        let resp = client
            .post(format!("{base}/v1/store"))
            .header("Authorization", "Bearer test-key-integration")
            .json(&payload)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
    }

    child.kill().await.ok();
    child.wait().await.ok();

    let project_bundle = tmp_dir.path().join("project-reader.membundle.json");
    let all_bundle = tmp_dir.path().join("all-reader.membundle.json");
    for (scope, output) in [("project", &project_bundle), ("all", &all_bundle)] {
        let export = tokio::process::Command::new(env!("CARGO_BIN_EXE_memoryoss"))
            .args([
                "--config",
                config_path.to_str().unwrap(),
                "bundle",
                "export",
                "--kind",
                "passport",
                "--namespace",
                "test",
                "--scope",
                scope,
                "--output",
                output.to_str().unwrap(),
            ])
            .output()
            .await
            .expect("failed to run bundle export for reader test");
        assert!(
            export.status.success(),
            "reader bundle export failed: stdout={} stderr={}",
            String::from_utf8_lossy(&export.stdout),
            String::from_utf8_lossy(&export.stderr)
        );
    }

    let missing_config = tmp_dir.path().join("missing-reader.toml");
    let open = tokio::process::Command::new(env!("CARGO_BIN_EXE_memoryoss"))
        .args([
            "--config",
            missing_config.to_str().unwrap(),
            "reader",
            "open",
            project_bundle.to_str().unwrap(),
            "--format",
            "json",
        ])
        .output()
        .await
        .expect("failed to run reader open");
    assert!(
        open.status.success(),
        "reader open failed: stdout={} stderr={}",
        String::from_utf8_lossy(&open.stdout),
        String::from_utf8_lossy(&open.stderr)
    );
    let open_body: serde_json::Value = serde_json::from_slice(&open.stdout).unwrap();
    assert_eq!(
        open_body["artifact_type"].as_str(),
        Some("memory_bundle_envelope")
    );
    assert_eq!(open_body["kind"].as_str(), Some("passport"));
    assert_eq!(open_body["scope"].as_str(), Some("project"));
    assert_eq!(
        open_body["signature"]["scheme"].as_str(),
        Some("memoryoss.hmac-sha256.v1")
    );
    assert_eq!(
        open_body["trust"]["status"].as_str(),
        Some("verification_unavailable")
    );
    assert_eq!(
        open_body["provenance"]["exported_from_namespace"].as_str(),
        Some("test")
    );
    assert!(
        open_body["preview"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry
                .as_str()
                .unwrap_or("")
                .contains("Project decision: keep MCP-first auth"))
    );

    let verified = tokio::process::Command::new(env!("CARGO_BIN_EXE_memoryoss"))
        .args([
            "--config",
            config_path.to_str().unwrap(),
            "reader",
            "open",
            project_bundle.to_str().unwrap(),
            "--format",
            "json",
        ])
        .output()
        .await
        .expect("failed to run reader open with trust context");
    assert!(
        verified.status.success(),
        "reader open with trust context failed: stdout={} stderr={}",
        String::from_utf8_lossy(&verified.stdout),
        String::from_utf8_lossy(&verified.stderr)
    );
    let verified_body: serde_json::Value = serde_json::from_slice(&verified.stdout).unwrap();
    assert_eq!(verified_body["trust"]["status"].as_str(), Some("trusted"));
    assert_eq!(verified_body["trust"]["verified"].as_bool(), Some(true));
    assert!(
        verified_body["signature"]["chain"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry
                .as_str()
                .unwrap_or("")
                .contains("device:local-runtime"))
    );

    let mut revoke_child = start_server(config_path.to_str().unwrap()).await;
    let revoke_resp = client
        .post(format!("{base}/v1/admin/trust/register"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({
            "id": "device:recovery-runtime",
            "kind": "device",
            "label": "Recovery runtime"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(revoke_resp.status(), 200);
    let revoke_local = client
        .post(format!("{base}/v1/admin/trust/revoke"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({
            "id": "device:local-runtime",
            "reason": "compromised during smoke test",
            "replacement_identity": "device:recovery-runtime"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(revoke_local.status(), 200);
    revoke_child.kill().await.ok();
    revoke_child.wait().await.ok();

    let revoked = tokio::process::Command::new(env!("CARGO_BIN_EXE_memoryoss"))
        .args([
            "--config",
            config_path.to_str().unwrap(),
            "reader",
            "open",
            project_bundle.to_str().unwrap(),
            "--format",
            "json",
        ])
        .output()
        .await
        .expect("failed to run reader open after revoke");
    assert!(
        revoked.status.success(),
        "reader open after revoke failed: stdout={} stderr={}",
        String::from_utf8_lossy(&revoked.stdout),
        String::from_utf8_lossy(&revoked.stderr)
    );
    let revoked_body: serde_json::Value = serde_json::from_slice(&revoked.stdout).unwrap();
    assert_eq!(revoked_body["trust"]["status"].as_str(), Some("revoked"));
    assert_eq!(
        revoked_body["trust"]["replacement_identity"].as_str(),
        Some("device:recovery-runtime")
    );

    let mut restore_child = start_server(config_path.to_str().unwrap()).await;
    let restore_resp = client
        .post(format!("{base}/v1/admin/trust/restore"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({
            "id": "device:local-runtime"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(restore_resp.status(), 200);
    restore_child.kill().await.ok();
    restore_child.wait().await.ok();

    let restored = tokio::process::Command::new(env!("CARGO_BIN_EXE_memoryoss"))
        .args([
            "--config",
            config_path.to_str().unwrap(),
            "reader",
            "open",
            project_bundle.to_str().unwrap(),
            "--format",
            "json",
        ])
        .output()
        .await
        .expect("failed to run reader open after restore");
    assert!(
        restored.status.success(),
        "reader open after restore failed: stdout={} stderr={}",
        String::from_utf8_lossy(&restored.stdout),
        String::from_utf8_lossy(&restored.stderr)
    );
    let restored_body: serde_json::Value = serde_json::from_slice(&restored.stdout).unwrap();
    assert_eq!(restored_body["trust"]["status"].as_str(), Some("trusted"));

    let diff = tokio::process::Command::new(env!("CARGO_BIN_EXE_memoryoss"))
        .args([
            "--config",
            missing_config.to_str().unwrap(),
            "reader",
            "diff",
            project_bundle.to_str().unwrap(),
            all_bundle.to_str().unwrap(),
        ])
        .output()
        .await
        .expect("failed to run reader diff");
    assert!(
        diff.status.success(),
        "reader diff failed: stdout={} stderr={}",
        String::from_utf8_lossy(&diff.stdout),
        String::from_utf8_lossy(&diff.stderr)
    );
    let diff_stdout = String::from_utf8_lossy(&diff.stdout);
    assert!(diff_stdout.contains("Universal memory reader diff"));
    assert!(diff_stdout.contains("Added"));
    assert!(diff_stdout.contains("Team policy: security review is mandatory before merge."));
}

#[tokio::test]
async fn test_trust_fabric_sign_verify_revoke_restore_for_bundle_passport_and_sync_peer() {
    let port = free_port();
    let tmp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let data_dir = tmp_dir.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let config_path = tmp_dir.path().join("trust-fabric.toml");
    std::fs::write(
        &config_path,
        test_config_http(port, data_dir.to_str().unwrap()),
    )
    .unwrap();

    let mut child = start_server(config_path.to_str().unwrap()).await;
    let client = reqwest::Client::builder().build().unwrap();
    let base = format!("http://127.0.0.1:{port}");

    let store = client
        .post(format!("{base}/v1/store"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({
            "content": "Release decision: preserve rollback fixtures for every update lane.",
            "tags": ["release", "rollback"]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(store.status(), 200);

    for identity in [
        serde_json::json!({"id": "author:alice", "kind": "author", "label": "Alice"}),
        serde_json::json!({"id": "author:bob", "kind": "author", "label": "Bob"}),
        serde_json::json!({"id": "device:backup-runtime", "kind": "device", "label": "Backup runtime"}),
        serde_json::json!({"id": "sync:peer-1", "kind": "sync_peer", "label": "Peer one"}),
    ] {
        let resp = client
            .post(format!("{base}/v1/admin/trust/register"))
            .header("Authorization", "Bearer test-key-integration")
            .json(&identity)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
    }

    let bundle_resp = client
        .get(format!(
            "{base}/v1/bundles/export?kind=passport&namespace=test&scope=project"
        ))
        .header("Authorization", "Bearer test-key-integration")
        .send()
        .await
        .unwrap();
    assert_eq!(bundle_resp.status(), 200);
    let bundle: serde_json::Value = bundle_resp.json().await.unwrap();
    assert_eq!(
        bundle["signature"]["scheme"].as_str(),
        Some("memoryoss.hmac-sha256.v1")
    );
    assert_eq!(
        bundle["signature"]["signer"].as_str(),
        Some("device:local-runtime")
    );
    assert!(bundle["signature"]["value"].as_str().is_some());

    let bundle_validate = client
        .post(format!("{base}/v1/bundles/validate"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({ "bundle": bundle.clone() }))
        .send()
        .await
        .unwrap();
    assert_eq!(bundle_validate.status(), 200);
    let bundle_validate_body: serde_json::Value = bundle_validate.json().await.unwrap();
    assert_eq!(
        bundle_validate_body["trust"]["status"].as_str(),
        Some("trusted")
    );

    let revoke_runtime = client
        .post(format!("{base}/v1/admin/trust/revoke"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({
            "id": "device:local-runtime",
            "reason": "runtime compromise",
            "replacement_identity": "device:backup-runtime"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(revoke_runtime.status(), 200);

    let bundle_verify_revoked = client
        .post(format!("{base}/v1/admin/trust/verify"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({
            "kind": "bundle",
            "artifact": bundle.clone()
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(bundle_verify_revoked.status(), 200);
    let bundle_verify_revoked_body: serde_json::Value = bundle_verify_revoked.json().await.unwrap();
    assert_eq!(
        bundle_verify_revoked_body["trust"]["status"].as_str(),
        Some("revoked")
    );
    assert_eq!(
        bundle_verify_revoked_body["trust"]["replacement_identity"].as_str(),
        Some("device:backup-runtime")
    );

    let restore_runtime = client
        .post(format!("{base}/v1/admin/trust/restore"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({ "id": "device:local-runtime" }))
        .send()
        .await
        .unwrap();
    assert_eq!(restore_runtime.status(), 200);

    let passport_resp = client
        .get(format!(
            "{base}/v1/passport/export?namespace=test&scope=project"
        ))
        .header("Authorization", "Bearer test-key-integration")
        .send()
        .await
        .unwrap();
    assert_eq!(passport_resp.status(), 200);
    let passport: serde_json::Value = passport_resp.json().await.unwrap();

    let passport_sign = client
        .post(format!("{base}/v1/admin/trust/sign"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({
            "kind": "passport",
            "identity": "author:alice",
            "artifact": passport.clone()
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(passport_sign.status(), 200);
    let passport_sign_body: serde_json::Value = passport_sign.json().await.unwrap();
    let passport_signature = passport_sign_body["signature"].clone();
    assert_eq!(
        passport_sign_body["trust"]["status"].as_str(),
        Some("trusted")
    );

    let passport_verify = client
        .post(format!("{base}/v1/admin/trust/verify"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({
            "kind": "passport",
            "artifact": passport.clone(),
            "signature": passport_signature.clone()
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(passport_verify.status(), 200);
    let passport_verify_body: serde_json::Value = passport_verify.json().await.unwrap();
    assert_eq!(
        passport_verify_body["trust"]["status"].as_str(),
        Some("trusted")
    );

    let sync_descriptor = serde_json::json!({
        "peer_id": "peer-1",
        "namespace": "test",
        "endpoint": "https://peer.example/sync",
        "device_label": "Alice laptop"
    });
    let sync_sign = client
        .post(format!("{base}/v1/admin/trust/sign"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({
            "kind": "sync_peer",
            "identity": "sync:peer-1",
            "artifact": sync_descriptor.clone()
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(sync_sign.status(), 200);
    let sync_sign_body: serde_json::Value = sync_sign.json().await.unwrap();
    let sync_verify = client
        .post(format!("{base}/v1/admin/trust/verify"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({
            "kind": "sync_peer",
            "artifact": sync_descriptor,
            "signature": sync_sign_body["signature"].clone()
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(sync_verify.status(), 200);
    let sync_verify_body: serde_json::Value = sync_verify.json().await.unwrap();
    assert_eq!(
        sync_verify_body["trust"]["status"].as_str(),
        Some("trusted")
    );

    let revoke_author = client
        .post(format!("{base}/v1/admin/trust/revoke"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({
            "id": "author:alice",
            "reason": "author key leaked",
            "replacement_identity": "author:bob"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(revoke_author.status(), 200);

    let fabric_revoked = client
        .get(format!("{base}/v1/admin/trust/fabric"))
        .header("Authorization", "Bearer test-key-integration")
        .send()
        .await
        .unwrap();
    assert_eq!(fabric_revoked.status(), 200);
    let fabric_revoked_body: serde_json::Value = fabric_revoked.json().await.unwrap();
    assert!(
        fabric_revoked_body["revocations"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["id"].as_str() == Some("author:alice"))
    );

    let passport_revoked = client
        .post(format!("{base}/v1/admin/trust/verify"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({
            "kind": "passport",
            "artifact": passport.clone(),
            "signature": passport_signature.clone()
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(passport_revoked.status(), 200);
    let passport_revoked_body: serde_json::Value = passport_revoked.json().await.unwrap();
    assert_eq!(
        passport_revoked_body["trust"]["status"].as_str(),
        Some("revoked")
    );
    assert_eq!(
        passport_revoked_body["trust"]["replacement_identity"].as_str(),
        Some("author:bob")
    );

    let restore_author = client
        .post(format!("{base}/v1/admin/trust/restore"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({ "id": "author:alice" }))
        .send()
        .await
        .unwrap();
    assert_eq!(restore_author.status(), 200);

    let passport_restored = client
        .post(format!("{base}/v1/admin/trust/verify"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({
            "kind": "passport",
            "artifact": passport,
            "signature": passport_signature
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(passport_restored.status(), 200);
    let passport_restored_body: serde_json::Value = passport_restored.json().await.unwrap();
    assert_eq!(
        passport_restored_body["trust"]["status"].as_str(),
        Some("trusted")
    );

    child.kill().await.ok();
    child.wait().await.ok();
}

#[tokio::test]
async fn test_trust_catalog_import_pin_and_revocation_propagation_across_reader() {
    let source_port = free_port();
    let source_tmp = tempfile::tempdir().expect("failed to create source temp dir");
    let source_data_dir = source_tmp.path().join("data");
    std::fs::create_dir_all(&source_data_dir).unwrap();
    let source_config_path = source_tmp.path().join("source-trust.toml");
    std::fs::write(
        &source_config_path,
        test_config_http(source_port, source_data_dir.to_str().unwrap()),
    )
    .unwrap();

    let target_port = free_port();
    let target_tmp = tempfile::tempdir().expect("failed to create target temp dir");
    let target_data_dir = target_tmp.path().join("data");
    std::fs::create_dir_all(&target_data_dir).unwrap();
    let target_config_path = target_tmp.path().join("target-trust.toml");
    std::fs::write(
        &target_config_path,
        test_config_http(target_port, target_data_dir.to_str().unwrap()),
    )
    .unwrap();

    let mut source_child = start_server(source_config_path.to_str().unwrap()).await;
    let mut target_child = start_server(target_config_path.to_str().unwrap()).await;
    let client = reqwest::Client::builder().build().unwrap();
    let source_base = format!("http://127.0.0.1:{source_port}");
    let target_base = format!("http://127.0.0.1:{target_port}");

    let store = client
        .post(format!("{source_base}/v1/store"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({
            "content": "Rollback policy: preserve the last known-good artifact before promotion.",
            "tags": ["release", "rollback"]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(store.status(), 200);

    for identity in [
        serde_json::json!({
            "id": "device:source-runtime",
            "kind": "device",
            "label": "Source runtime"
        }),
        serde_json::json!({
            "id": "device:backup-runtime",
            "kind": "device",
            "label": "Backup runtime"
        }),
    ] {
        let register = client
            .post(format!("{source_base}/v1/admin/trust/register"))
            .header("Authorization", "Bearer test-key-integration")
            .json(&identity)
            .send()
            .await
            .unwrap();
        assert_eq!(register.status(), 200);
    }

    let bundle_resp = client
        .get(format!(
            "{source_base}/v1/bundles/export?kind=passport&namespace=test&scope=project"
        ))
        .header("Authorization", "Bearer test-key-integration")
        .send()
        .await
        .unwrap();
    assert_eq!(bundle_resp.status(), 200);
    let bundle: serde_json::Value = bundle_resp.json().await.unwrap();
    let source_sign = client
        .post(format!("{source_base}/v1/admin/trust/sign"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({
            "kind": "bundle",
            "identity": "device:source-runtime",
            "artifact": bundle
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(source_sign.status(), 200);
    let source_sign_body: serde_json::Value = source_sign.json().await.unwrap();
    let bundle = source_sign_body["artifact"].clone();
    assert_eq!(
        bundle["signature"]["signer"].as_str(),
        Some("device:source-runtime")
    );

    let unknown_validate = client
        .post(format!("{target_base}/v1/bundles/validate"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({ "bundle": bundle.clone() }))
        .send()
        .await
        .unwrap();
    assert_eq!(unknown_validate.status(), 200);
    let unknown_validate_body: serde_json::Value = unknown_validate.json().await.unwrap();
    assert_eq!(
        unknown_validate_body["trust"]["status"].as_str(),
        Some("unknown_identity")
    );
    assert_eq!(
        unknown_validate_body["trust"]["origin"].as_str(),
        Some("unknown_signer")
    );

    let catalog_export = client
        .get(format!(
            "{source_base}/v1/admin/trust/catalog/export?catalog_id=team-alpha&label=Team%20Alpha"
        ))
        .header("Authorization", "Bearer test-key-integration")
        .send()
        .await
        .unwrap();
    assert_eq!(catalog_export.status(), 200);
    let catalog_export_body: serde_json::Value = catalog_export.json().await.unwrap();
    let catalog = catalog_export_body["catalog"].clone();

    let catalog_import = client
        .post(format!("{target_base}/v1/admin/trust/catalog/import"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({ "catalog": catalog.clone() }))
        .send()
        .await
        .unwrap();
    assert_eq!(catalog_import.status(), 200);
    let catalog_import_body: serde_json::Value = catalog_import.json().await.unwrap();
    assert_eq!(
        catalog_import_body["import"]["catalog"]["catalog_id"].as_str(),
        Some("team-alpha")
    );

    let imported_validate = client
        .post(format!("{target_base}/v1/bundles/validate"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({ "bundle": bundle.clone() }))
        .send()
        .await
        .unwrap();
    assert_eq!(imported_validate.status(), 200);
    let imported_validate_body: serde_json::Value = imported_validate.json().await.unwrap();
    assert_eq!(
        imported_validate_body["trust"]["status"].as_str(),
        Some("trusted")
    );
    assert_eq!(
        imported_validate_body["trust"]["origin"].as_str(),
        Some("imported_catalog")
    );
    assert_eq!(
        imported_validate_body["trust"]["catalog_id"].as_str(),
        Some("team-alpha")
    );

    let bundle_path = target_tmp.path().join("shared-bundle.json");
    std::fs::write(&bundle_path, serde_json::to_vec_pretty(&bundle).unwrap()).unwrap();
    let reader_imported = tokio::process::Command::new(env!("CARGO_BIN_EXE_memoryoss"))
        .args([
            "--config",
            target_config_path.to_str().unwrap(),
            "reader",
            "open",
            bundle_path.to_str().unwrap(),
            "--format",
            "json",
        ])
        .output()
        .await
        .expect("failed to run reader open for imported catalog");
    assert!(
        reader_imported.status.success(),
        "reader open failed: stdout={} stderr={}",
        String::from_utf8_lossy(&reader_imported.stdout),
        String::from_utf8_lossy(&reader_imported.stderr)
    );
    let reader_imported_body: serde_json::Value =
        serde_json::from_slice(&reader_imported.stdout).unwrap();
    assert_eq!(
        reader_imported_body["trust"]["origin"].as_str(),
        Some("imported_catalog")
    );

    let pin_resp = client
        .post(format!("{target_base}/v1/admin/trust/pin"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({ "id": "device:source-runtime" }))
        .send()
        .await
        .unwrap();
    assert_eq!(pin_resp.status(), 200);

    let reader_pinned = tokio::process::Command::new(env!("CARGO_BIN_EXE_memoryoss"))
        .args([
            "--config",
            target_config_path.to_str().unwrap(),
            "reader",
            "open",
            bundle_path.to_str().unwrap(),
            "--format",
            "json",
        ])
        .output()
        .await
        .expect("failed to run reader open for pinned catalog");
    assert!(reader_pinned.status.success());
    let reader_pinned_body: serde_json::Value =
        serde_json::from_slice(&reader_pinned.stdout).unwrap();
    assert_eq!(
        reader_pinned_body["trust"]["origin"].as_str(),
        Some("local_pin")
    );

    let revoke_runtime = client
        .post(format!("{source_base}/v1/admin/trust/revoke"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({
            "id": "device:source-runtime",
            "reason": "runtime compromise",
            "replacement_identity": "device:backup-runtime"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(revoke_runtime.status(), 200);

    let stale_validate = client
        .post(format!("{target_base}/v1/bundles/validate"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({ "bundle": bundle.clone() }))
        .send()
        .await
        .unwrap();
    assert_eq!(stale_validate.status(), 200);
    let stale_validate_body: serde_json::Value = stale_validate.json().await.unwrap();
    assert_eq!(
        stale_validate_body["trust"]["status"].as_str(),
        Some("trusted")
    );

    let revoked_catalog_export = client
        .get(format!(
            "{source_base}/v1/admin/trust/catalog/export?catalog_id=team-alpha&label=Team%20Alpha"
        ))
        .header("Authorization", "Bearer test-key-integration")
        .send()
        .await
        .unwrap();
    assert_eq!(revoked_catalog_export.status(), 200);
    let revoked_catalog_body: serde_json::Value = revoked_catalog_export.json().await.unwrap();
    let revoked_import = client
        .post(format!("{target_base}/v1/admin/trust/catalog/import"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({ "catalog": revoked_catalog_body["catalog"].clone() }))
        .send()
        .await
        .unwrap();
    assert_eq!(revoked_import.status(), 200);

    let revoked_validate = client
        .post(format!("{target_base}/v1/bundles/validate"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({ "bundle": bundle.clone() }))
        .send()
        .await
        .unwrap();
    assert_eq!(revoked_validate.status(), 200);
    let revoked_validate_body: serde_json::Value = revoked_validate.json().await.unwrap();
    assert_eq!(
        revoked_validate_body["trust"]["status"].as_str(),
        Some("revoked")
    );
    assert_eq!(
        revoked_validate_body["trust"]["replacement_identity"].as_str(),
        Some("device:backup-runtime")
    );

    let backup_sign = client
        .post(format!("{source_base}/v1/admin/trust/sign"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({
            "kind": "bundle",
            "identity": "device:backup-runtime",
            "artifact": bundle.clone()
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(backup_sign.status(), 200);
    let backup_sign_body: serde_json::Value = backup_sign.json().await.unwrap();
    let backup_bundle = backup_sign_body["artifact"].clone();
    let backup_validate = client
        .post(format!("{target_base}/v1/bundles/validate"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({ "bundle": backup_bundle }))
        .send()
        .await
        .unwrap();
    assert_eq!(backup_validate.status(), 200);
    let backup_validate_body: serde_json::Value = backup_validate.json().await.unwrap();
    assert_eq!(
        backup_validate_body["trust"]["status"].as_str(),
        Some("trusted")
    );
    assert_eq!(
        backup_validate_body["trust"]["origin"].as_str(),
        Some("imported_catalog")
    );

    let restore_runtime = client
        .post(format!("{source_base}/v1/admin/trust/restore"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({ "id": "device:source-runtime" }))
        .send()
        .await
        .unwrap();
    assert_eq!(restore_runtime.status(), 200);

    let restored_catalog_export = client
        .get(format!(
            "{source_base}/v1/admin/trust/catalog/export?catalog_id=team-alpha&label=Team%20Alpha"
        ))
        .header("Authorization", "Bearer test-key-integration")
        .send()
        .await
        .unwrap();
    assert_eq!(restored_catalog_export.status(), 200);
    let restored_catalog_body: serde_json::Value = restored_catalog_export.json().await.unwrap();
    let restored_import = client
        .post(format!("{target_base}/v1/admin/trust/catalog/import"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({ "catalog": restored_catalog_body["catalog"].clone() }))
        .send()
        .await
        .unwrap();
    assert_eq!(restored_import.status(), 200);

    let restored_validate = client
        .post(format!("{target_base}/v1/bundles/validate"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({ "bundle": bundle }))
        .send()
        .await
        .unwrap();
    assert_eq!(restored_validate.status(), 200);
    let restored_validate_body: serde_json::Value = restored_validate.json().await.unwrap();
    assert_eq!(
        restored_validate_body["trust"]["status"].as_str(),
        Some("trusted")
    );

    let fabric = client
        .get(format!("{target_base}/v1/admin/trust/fabric"))
        .header("Authorization", "Bearer test-key-integration")
        .send()
        .await
        .unwrap();
    assert_eq!(fabric.status(), 200);
    let fabric_body: serde_json::Value = fabric.json().await.unwrap();
    assert_eq!(fabric_body["summary"]["catalogs"].as_u64(), Some(1));
    assert_eq!(fabric_body["summary"]["pins"].as_u64(), Some(1));

    source_child.kill().await.ok();
    source_child.wait().await.ok();
    target_child.kill().await.ok();
    target_child.wait().await.ok();
}

#[tokio::test]
async fn test_cli_reader_rejects_malformed_artifacts_and_escapes_html_preview() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let fixture_path = root.join("conformance/fixtures/passport-bundle.json");
    let mut bundle: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&fixture_path).unwrap()).unwrap();
    bundle["memories"][0]["content"] = serde_json::Value::String(
        "<script>alert('x')</script> Release checklist lives in docs/releases.md.".to_string(),
    );
    bundle["integrity"]["payload_sha256"] =
        serde_json::Value::String(passport_payload_sha256(&bundle));

    let tmp_dir = tempfile::tempdir().expect("failed to create reader temp dir");
    let escaped_path = tmp_dir.path().join("escaped-passport.json");
    std::fs::write(&escaped_path, serde_json::to_vec_pretty(&bundle).unwrap()).unwrap();
    let missing_config = tmp_dir.path().join("missing-reader.toml");

    let html = tokio::process::Command::new(env!("CARGO_BIN_EXE_memoryoss"))
        .args([
            "--config",
            missing_config.to_str().unwrap(),
            "reader",
            "open",
            escaped_path.to_str().unwrap(),
            "--format",
            "html",
        ])
        .output()
        .await
        .expect("failed to run reader html open");
    assert!(
        html.status.success(),
        "reader html open failed: stdout={} stderr={}",
        String::from_utf8_lossy(&html.stdout),
        String::from_utf8_lossy(&html.stderr)
    );
    let html_stdout = String::from_utf8_lossy(&html.stdout);
    assert!(html_stdout.contains("&lt;script&gt;alert(&#39;x&#39;)&lt;/script&gt;"));
    assert!(!html_stdout.contains("<script>alert('x')</script>"));

    let malformed_path = tmp_dir.path().join("malformed.json");
    std::fs::write(&malformed_path, b"{\"bundle_version\":").unwrap();
    let malformed = tokio::process::Command::new(env!("CARGO_BIN_EXE_memoryoss"))
        .args([
            "--config",
            missing_config.to_str().unwrap(),
            "reader",
            "open",
            malformed_path.to_str().unwrap(),
        ])
        .output()
        .await
        .expect("failed to run malformed reader open");
    assert!(
        !malformed.status.success(),
        "reader open should fail for malformed artifacts"
    );
    let malformed_stderr = String::from_utf8_lossy(&malformed.stderr);
    assert!(malformed_stderr.contains("unsupported or malformed memory reader artifact"));
}

#[tokio::test]
async fn test_connector_manifest_and_ingest_feed_review_queue_and_recall() {
    let port = free_port();
    let tmp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let data_dir = tmp_dir.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let config_path = tmp_dir.path().join("connector-mesh.toml");
    std::fs::write(
        &config_path,
        test_config_http(port, data_dir.to_str().unwrap()),
    )
    .unwrap();

    let mut child = start_server(config_path.to_str().unwrap()).await;
    let client = reqwest::Client::builder().build().unwrap();
    let base = format!("http://127.0.0.1:{port}");

    let manifest_resp = client
        .get(format!("{base}/v1/connectors"))
        .header("Authorization", "Bearer test-key-integration")
        .send()
        .await
        .unwrap();
    assert_eq!(manifest_resp.status(), 200);
    let manifest_body: serde_json::Value = manifest_resp.json().await.unwrap();
    let connectors = manifest_body["connectors"].as_array().unwrap();
    assert!(connectors.len() >= 5);
    assert!(connectors.iter().all(|entry| {
        entry["enabled_by_default"].as_bool() == Some(false)
            && entry["redact_sensitive_by_default"].as_bool() == Some(true)
            && entry["capture_raw_by_default"].as_bool() == Some(false)
    }));

    let dry_run_resp = client
        .post(format!("{base}/v1/connectors/ingest"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({
            "connector": "terminal",
            "summary": "Terminal note: use cargo fmt before release commits.",
            "evidence": ["export API_KEY=sk-live-secret"],
            "tags": ["release"],
            "source_ref": "terminal://release/42",
            "dry_run": true
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(dry_run_resp.status(), 200);
    let dry_run_body: serde_json::Value = dry_run_resp.json().await.unwrap();
    assert_eq!(
        dry_run_body["source"].as_str(),
        Some("ambient-connector:terminal")
    );
    assert_eq!(dry_run_body["preview"]["redacted"].as_bool(), Some(true));
    assert!(
        dry_run_body["preview"]["preview"]
            .as_str()
            .unwrap_or("")
            .contains("[REDACTED:")
    );
    assert!(
        dry_run_body["preview"]["tags"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tag| tag.as_str() == Some("connector:terminal"))
    );
    assert!(
        dry_run_body["preview"]["tags"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tag| tag.as_str() == Some("client:ambient_mesh"))
    );
    assert!(
        dry_run_body["preview"]["tags"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tag| tag.as_str().unwrap_or("").starts_with("source_ref:"))
    );

    for payload in [
        serde_json::json!({
            "connector": "editor",
            "summary": "Editor note: release checklist is referenced beside the rollout notes.",
            "evidence": ["docs/releases/README.md"],
            "source_ref": "editor://workspace/docs/releases/README.md#12",
            "tags": ["release", "docs"]
        }),
        serde_json::json!({
            "connector": "terminal",
            "summary": "Terminal note: use cargo fmt before release commits.",
            "evidence": ["cargo fmt --check"],
            "source_ref": "terminal://release/42",
            "tags": ["release", "checks"]
        }),
        serde_json::json!({
            "connector": "browser",
            "summary": "Browser note: deployment dashboard lives at status.internal/releases.",
            "evidence": ["status.internal/releases"],
            "source_ref": "browser://status.internal/releases",
            "tags": ["release", "dashboard"]
        }),
        serde_json::json!({
            "connector": "docs",
            "summary": "Docs note: release checklist lives in docs/releases/README.md and is owned by the release captain.",
            "evidence": ["docs/releases/README.md"],
            "source_ref": "docs://docs/releases/README.md",
            "tags": ["release", "docs"]
        }),
        serde_json::json!({
            "connector": "ticket",
            "summary": "Ticket note: INC-4242 tracks rollback verification before release.",
            "evidence": ["INC-4242"],
            "source_ref": "ticket://INC-4242",
            "tags": ["release", "incident"]
        }),
    ] {
        let ingest_resp = client
            .post(format!("{base}/v1/connectors/ingest"))
            .header("Authorization", "Bearer test-key-integration")
            .json(&payload)
            .send()
            .await
            .unwrap();
        assert_eq!(ingest_resp.status(), 200);
        let ingest_body: serde_json::Value = ingest_resp.json().await.unwrap();
        assert_eq!(ingest_body["dry_run"].as_bool(), Some(false));
        assert!(
            ingest_body["review_key"]
                .as_str()
                .unwrap_or("")
                .starts_with("review:")
        );
    }

    let queue_body = review_queue(&client, &base, "test-key-integration", "test", 10).await;
    assert_eq!(queue_body["summary"]["candidate"].as_u64(), Some(5));
    let docs_item = queue_body["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| {
            entry["preview"]
                .as_str()
                .unwrap_or("")
                .contains("docs/releases/README.md")
        })
        .cloned()
        .expect("expected docs connector candidate");
    assert_eq!(docs_item["source"].as_str(), Some("ambient-connector:docs"));
    assert!(
        docs_item["tags"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tag| tag.as_str() == Some("connector:docs"))
    );
    assert!(
        docs_item["tags"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tag| tag.as_str() == Some("client:ambient_mesh"))
    );

    let action_resp = client
        .post(format!("{base}/v1/admin/review/action"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({
            "namespace": "test",
            "review_key": docs_item["review_key"].as_str().unwrap(),
            "action": "confirm"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(action_resp.status(), 200);
    let action_body: serde_json::Value = action_resp.json().await.unwrap();
    assert_eq!(action_body["memory"]["status"].as_str(), Some("active"));

    tokio::time::sleep(Duration::from_secs(3)).await;

    let explain_resp = client
        .post(format!("{base}/v1/admin/query-explain"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({
            "query": "Where does the release checklist live?",
            "limit": 5
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(explain_resp.status(), 200);
    let explain_body: serde_json::Value = explain_resp.json().await.unwrap();
    assert!(
        explain_body["final_results"][0]["memory"]["content"]
            .as_str()
            .unwrap_or("")
            .contains("docs/releases/README.md")
    );

    child.kill().await.ok();
}

#[tokio::test]
async fn test_cli_connector_list_and_ingest_dry_run_surface_privacy_defaults() {
    let port = free_port();
    let tmp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let data_dir = tmp_dir.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let config_path = tmp_dir.path().join("connector-cli.toml");
    std::fs::write(
        &config_path,
        test_config_http(port, data_dir.to_str().unwrap()),
    )
    .unwrap();

    let list = tokio::process::Command::new(env!("CARGO_BIN_EXE_memoryoss"))
        .args([
            "--config",
            config_path.to_str().unwrap(),
            "connector",
            "list",
        ])
        .output()
        .await
        .expect("failed to run connector list");
    assert!(
        list.status.success(),
        "connector list failed: stdout={} stderr={}",
        String::from_utf8_lossy(&list.stdout),
        String::from_utf8_lossy(&list.stderr)
    );
    let list_stdout = String::from_utf8_lossy(&list.stdout);
    assert!(list_stdout.contains("Ambient connector mesh"));
    assert!(list_stdout.contains("editor"));
    assert!(list_stdout.contains("terminal"));
    assert!(list_stdout.contains("redact_sensitive=true"));

    let dry_run = tokio::process::Command::new(env!("CARGO_BIN_EXE_memoryoss"))
        .args([
            "--config",
            config_path.to_str().unwrap(),
            "connector",
            "ingest",
            "--kind",
            "terminal",
            "--namespace",
            "test",
            "--summary",
            "Terminal note: use cargo fmt before release commits.",
            "--evidence",
            "export API_KEY=sk-live-secret",
            "--source-ref",
            "terminal://release/42",
            "--tag",
            "release",
            "--dry-run",
        ])
        .output()
        .await
        .expect("failed to run connector ingest dry-run");
    assert!(
        dry_run.status.success(),
        "connector ingest dry-run failed: stdout={} stderr={}",
        String::from_utf8_lossy(&dry_run.stdout),
        String::from_utf8_lossy(&dry_run.stderr)
    );
    let dry_run_stdout = String::from_utf8_lossy(&dry_run.stdout);
    assert!(dry_run_stdout.contains("Ambient connector candidate"));
    assert!(dry_run_stdout.contains("ambient-connector:terminal"));
    assert!(dry_run_stdout.contains("[REDACTED:"));
    assert!(dry_run_stdout.contains("connector:terminal"));
    assert!(dry_run_stdout.contains("client:ambient_mesh"));
    assert!(dry_run_stdout.contains("source_ref:"));
}

#[tokio::test]
async fn test_adapter_api_import_dry_run_and_export() {
    let port = free_port();
    let tmp = tempfile::tempdir().expect("failed to create temp dir");
    let data_dir = tmp.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let config = test_config_http(port, data_dir.to_str().unwrap());
    let config_path = tmp.path().join("adapter-api.toml");
    std::fs::write(&config_path, config).unwrap();

    let mut child = start_server(config_path.to_str().unwrap()).await;
    let client = reqwest::Client::builder().build().unwrap();
    let base = format!("http://127.0.0.1:{port}");
    let cursor_rules = r#"---
description: Review rules
alwaysApply: false
---

- Never merge without security review
- Prefer rg over grep
"#;

    let dry_run = client
        .post(format!("{base}/v1/adapters/import"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({
            "kind": "cursor_rules",
            "source_label": "review-rules.mdc",
            "content": cursor_rules,
            "dry_run": true
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(dry_run.status(), 200);
    let dry_run_body: serde_json::Value = dry_run.json().await.unwrap();
    assert_eq!(
        dry_run_body["preview"]["adapter_kind"].as_str(),
        Some("cursor_rules")
    );
    assert_eq!(
        dry_run_body["preview"]["normalized_count"].as_u64(),
        Some(2)
    );
    assert_eq!(
        dry_run_body["preview"]["preview"]["create_count"].as_u64(),
        Some(2)
    );

    let import = client
        .post(format!("{base}/v1/adapters/import"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({
            "kind": "cursor_rules",
            "source_label": "review-rules.mdc",
            "content": cursor_rules
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(import.status(), 200);
    let import_body: serde_json::Value = import.json().await.unwrap();
    assert_eq!(import_body["imported"].as_u64(), Some(2));

    let explain = client
        .post(format!("{base}/v1/admin/query-explain"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({
            "query": "What review rules should I follow before merge?"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(explain.status(), 200);
    let explain_body: serde_json::Value = explain.json().await.unwrap();
    assert_eq!(
        explain_body["retrieval_gate"]["decision"].as_str(),
        Some("inject")
    );
    assert!(
        explain_body["summary_results"]
            .as_array()
            .map(|items| !items.is_empty())
            .unwrap_or(false),
        "curated adapter imports should be eligible for immediate recall"
    );

    let export = client
        .get(format!("{base}/v1/adapters/export?kind=claude_project"))
        .header("Authorization", "Bearer test-key-integration")
        .send()
        .await
        .unwrap();
    assert_eq!(export.status(), 200);
    let export_body: serde_json::Value = export.json().await.unwrap();
    let artifact = &export_body["artifact"];
    assert_eq!(artifact["adapter_kind"].as_str(), Some("claude_project"));
    assert_eq!(artifact["exported_count"].as_u64(), Some(2));
    let content = artifact["content"].as_str().unwrap();
    assert!(content.contains("Never merge without security review."));
    assert!(content.contains("Prefer rg over grep."));

    child.kill().await.ok();
    child.wait().await.ok();
}

#[tokio::test]
async fn test_cursor_runtime_flow_reaches_codex_proxy_without_artifact_handoff() {
    let (upstream_port, upstream_state, upstream_handle) = start_dummy_upstream().await;

    let port = free_port();
    let tmp = tempfile::tempdir().expect("failed to create temp dir");
    let data_dir = tmp.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let auth_entries = r#"
[[auth.api_keys]]
key = "test-key-integration"
role = "admin"
namespace = "test"
"#;
    let extra_sections = format!(
        r#"
[proxy]
enabled = true
passthrough_auth = false
upstream_url = "http://127.0.0.1:{upstream_port}/v1"
upstream_api_key = "upstream-openai-key"
default_memory_mode = "full"
extraction_enabled = false

[[proxy.key_mapping]]
proxy_key = "test-key-proxy"
namespace = "test"
"#
    );
    let config = test_config_with_sections(
        port,
        data_dir.to_str().unwrap(),
        auth_entries,
        &extra_sections,
    );
    let config_path = tmp.path().join("cursor-runtime-proxy.toml");
    std::fs::write(&config_path, config).unwrap();

    let mut child = start_server(config_path.to_str().unwrap()).await;
    let client = test_client();
    let base = format!("https://127.0.0.1:{port}");
    let cursor_rules = r#"---
description: Review rules
alwaysApply: true
---

- Never merge auth changes without security review
- Prefer rg over grep when tracing auth or session flows
"#;

    let import = client
        .post(format!("{base}/v1/adapters/import"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({
            "kind": "cursor_rules",
            "source_label": ".cursor/rules/auth-review.mdc",
            "content": cursor_rules
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(import.status(), 200);
    let import_body: serde_json::Value = import.json().await.unwrap();
    assert_eq!(import_body["imported"].as_u64(), Some(2));

    let query = "What review rules should I follow before merge?";
    let explain = client
        .post(format!("{base}/v1/admin/query-explain"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({
            "query": query
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(explain.status(), 200);
    let explain_body: serde_json::Value = explain.json().await.unwrap();
    assert_eq!(
        explain_body["retrieval_gate"]["decision"].as_str(),
        Some("inject")
    );
    assert!(
        explain_body["final_results"]
            .as_array()
            .map(|items| {
                items.iter().any(|item| {
                    item["memory"]["content"]
                        .as_str()
                        .unwrap_or("")
                        .contains("Never merge auth changes without security review")
                })
            })
            .unwrap_or(false),
        "cursor-derived review rules should be immediately recallable"
    );

    let proxy_resp = client
        .post(format!("{base}/proxy/v1/chat/completions"))
        .header("Authorization", "Bearer test-key-proxy")
        .json(&serde_json::json!({
            "model": "gpt-4o-mini",
            "messages": [{"role": "user", "content": query}]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(proxy_resp.status(), 200);
    assert_eq!(
        proxy_resp
            .headers()
            .get("x-memory-gate-decision")
            .and_then(|v| v.to_str().ok()),
        Some("inject")
    );
    assert!(
        proxy_resp
            .headers()
            .get("x-memory-injected-count")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0)
            >= 1
    );

    let requests = upstream_state.requests.lock().unwrap().clone();
    let upstream_req = requests
        .iter()
        .find(|req| {
            req["path"].as_str() == Some("/v1/chat/completions")
                && req["body"]["messages"][0]["role"].as_str() == Some("system")
        })
        .expect("missing upstream chat request with injected system prompt");
    let system_content = upstream_req["body"]["messages"][0]["content"]
        .as_str()
        .expect("system content missing");
    assert!(
        system_content.contains("Never merge auth changes without security review"),
        "cursor-origin memory should reach Codex/OpenAI proxy injection without an intermediate artifact export"
    );

    child.kill().await.ok();
    upstream_handle.abort();
}

#[tokio::test]
async fn test_cli_adapter_cursor_to_claude_roundtrip() {
    let source_port = free_port();
    let source_tmp = tempfile::tempdir().expect("failed to create source temp dir");
    let source_data_dir = source_tmp.path().join("data");
    std::fs::create_dir_all(&source_data_dir).unwrap();
    let source_config = test_config_http(source_port, source_data_dir.to_str().unwrap());
    let source_config_path = source_tmp.path().join("adapter-source.toml");
    std::fs::write(&source_config_path, source_config).unwrap();

    let cursor_rules_path = source_tmp.path().join("review-rules.mdc");
    std::fs::write(
        &cursor_rules_path,
        r#"---
description: Review rules
alwaysApply: false
---

- Never merge without security review
- Prefer rg over grep
"#,
    )
    .unwrap();

    let import = tokio::process::Command::new(env!("CARGO_BIN_EXE_memoryoss"))
        .args([
            "--config",
            source_config_path.to_str().unwrap(),
            "adapter",
            "import",
            "--kind",
            "cursor_rules",
            cursor_rules_path.to_str().unwrap(),
            "--namespace",
            "test",
        ])
        .output()
        .await
        .expect("failed to run adapter import");
    assert!(
        import.status.success(),
        "adapter import failed: stdout={} stderr={}",
        String::from_utf8_lossy(&import.stdout),
        String::from_utf8_lossy(&import.stderr)
    );
    assert!(
        String::from_utf8_lossy(&import.stdout)
            .contains("Imported adapter cursor_rules into test: normalized=2 imported=2")
    );

    let claude_export_path = source_tmp.path().join("claude-project.md");
    let export = tokio::process::Command::new(env!("CARGO_BIN_EXE_memoryoss"))
        .args([
            "--config",
            source_config_path.to_str().unwrap(),
            "adapter",
            "export",
            "--kind",
            "claude_project",
            "--namespace",
            "test",
            "--output",
            claude_export_path.to_str().unwrap(),
        ])
        .output()
        .await
        .expect("failed to run adapter export");
    assert!(
        export.status.success(),
        "adapter export failed: stdout={} stderr={}",
        String::from_utf8_lossy(&export.stdout),
        String::from_utf8_lossy(&export.stderr)
    );
    let exported_text = std::fs::read_to_string(&claude_export_path).unwrap();
    assert!(exported_text.contains("Never merge without security review."));
    assert!(exported_text.contains("Prefer rg over grep."));

    let target_port = free_port();
    let target_tmp = tempfile::tempdir().expect("failed to create target temp dir");
    let target_data_dir = target_tmp.path().join("data");
    std::fs::create_dir_all(&target_data_dir).unwrap();
    let target_config = test_config_http(target_port, target_data_dir.to_str().unwrap());
    let target_config_path = target_tmp.path().join("adapter-target.toml");
    std::fs::write(&target_config_path, target_config).unwrap();

    let dry_run = tokio::process::Command::new(env!("CARGO_BIN_EXE_memoryoss"))
        .args([
            "--config",
            target_config_path.to_str().unwrap(),
            "adapter",
            "import",
            "--kind",
            "claude_project",
            claude_export_path.to_str().unwrap(),
            "--namespace",
            "test",
            "--dry-run",
        ])
        .output()
        .await
        .expect("failed to run adapter import dry-run");
    assert!(
        dry_run.status.success(),
        "adapter import dry-run failed: stdout={} stderr={}",
        String::from_utf8_lossy(&dry_run.stdout),
        String::from_utf8_lossy(&dry_run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&dry_run.stdout)
            .contains("normalized=2 create=2 merge=0 conflict=0")
    );

    let apply = tokio::process::Command::new(env!("CARGO_BIN_EXE_memoryoss"))
        .args([
            "--config",
            target_config_path.to_str().unwrap(),
            "adapter",
            "import",
            "--kind",
            "claude_project",
            claude_export_path.to_str().unwrap(),
            "--namespace",
            "test",
        ])
        .output()
        .await
        .expect("failed to run adapter import apply");
    assert!(
        apply.status.success(),
        "adapter import apply failed: stdout={} stderr={}",
        String::from_utf8_lossy(&apply.stdout),
        String::from_utf8_lossy(&apply.stderr)
    );

    let mut target_child = start_server(target_config_path.to_str().unwrap()).await;
    let client = reqwest::Client::builder().build().unwrap();
    let target_base = format!("http://127.0.0.1:{target_port}");
    let export_resp = client
        .get(format!("{target_base}/v1/export"))
        .header("Authorization", "Bearer test-key-integration")
        .send()
        .await
        .unwrap();
    assert_eq!(export_resp.status(), 200);
    let export_body: serde_json::Value = export_resp.json().await.unwrap();
    let contents: Vec<&str> = export_body["memories"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|memory| memory["content"].as_str())
        .collect();
    assert!(contents.contains(&"Never merge without security review."));
    assert!(contents.contains(&"Prefer rg over grep."));

    target_child.kill().await.ok();
    target_child.wait().await.ok();
}

#[tokio::test]
async fn test_cli_git_history_adapter_dry_run() {
    let tmp = tempfile::tempdir().expect("failed to create temp dir");
    let repo_path = tmp.path().join("repo");
    std::fs::create_dir_all(&repo_path).unwrap();
    let status = std::process::Command::new("git")
        .args(["init", repo_path.to_str().unwrap()])
        .status()
        .unwrap();
    assert!(status.success());
    assert!(
        std::process::Command::new("git")
            .args([
                "-C",
                repo_path.to_str().unwrap(),
                "config",
                "user.email",
                "test@example.com"
            ])
            .status()
            .unwrap()
            .success()
    );
    assert!(
        std::process::Command::new("git")
            .args([
                "-C",
                repo_path.to_str().unwrap(),
                "config",
                "user.name",
                "Test User"
            ])
            .status()
            .unwrap()
            .success()
    );
    let file_path = repo_path.join("notes.txt");
    std::fs::write(&file_path, "first").unwrap();
    assert!(
        std::process::Command::new("git")
            .args(["-C", repo_path.to_str().unwrap(), "add", "."])
            .status()
            .unwrap()
            .success()
    );
    assert!(
        std::process::Command::new("git")
            .args([
                "-C",
                repo_path.to_str().unwrap(),
                "commit",
                "-m",
                "feat(api): add adapter bridge"
            ])
            .status()
            .unwrap()
            .success()
    );
    std::fs::write(&file_path, "second").unwrap();
    assert!(
        std::process::Command::new("git")
            .args(["-C", repo_path.to_str().unwrap(), "add", "."])
            .status()
            .unwrap()
            .success()
    );
    assert!(
        std::process::Command::new("git")
            .args([
                "-C",
                repo_path.to_str().unwrap(),
                "commit",
                "-m",
                "fix(proxy): harden ambiguous gate"
            ])
            .status()
            .unwrap()
            .success()
    );

    let config_path = tmp.path().join("git-history.toml");
    let data_dir = tmp.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    std::fs::write(
        &config_path,
        test_config_http(free_port(), data_dir.to_str().unwrap()),
    )
    .unwrap();

    let dry_run = tokio::process::Command::new(env!("CARGO_BIN_EXE_memoryoss"))
        .args([
            "--config",
            config_path.to_str().unwrap(),
            "adapter",
            "import",
            "--kind",
            "git_history",
            repo_path.to_str().unwrap(),
            "--namespace",
            "test",
            "--dry-run",
        ])
        .output()
        .await
        .expect("failed to run git history adapter import dry-run");
    assert!(
        dry_run.status.success(),
        "git history adapter dry-run failed: stdout={} stderr={}",
        String::from_utf8_lossy(&dry_run.stdout),
        String::from_utf8_lossy(&dry_run.stderr)
    );
    let stdout = String::from_utf8_lossy(&dry_run.stdout);
    assert!(stdout.contains("Adapter import dry-run for test [git_history repo]"));
    assert!(stdout.contains("normalized=2 create=2 merge=0 conflict=0"));
}

#[tokio::test]
async fn test_history_api_view_and_replay_roundtrip() {
    let source_port = free_port();
    let source_tmp = tempfile::tempdir().expect("failed to create source temp dir");
    let source_data_dir = source_tmp.path().join("data");
    std::fs::create_dir_all(&source_data_dir).unwrap();
    let source_config = test_config_http(source_port, source_data_dir.to_str().unwrap());
    let source_config_path = source_tmp.path().join("history-source.toml");
    std::fs::write(&source_config_path, &source_config).unwrap();

    let mut source_child = start_server(source_config_path.to_str().unwrap()).await;
    let client = reqwest::Client::builder().build().unwrap();
    let source_base = format!("http://127.0.0.1:{source_port}");

    let old_resp = client
        .post(format!("{source_base}/v1/store"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({
            "content": "Project policy: use feature branches for deploys.",
            "tags": ["policy", "project"]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(old_resp.status(), 200);
    let old_id = old_resp.json::<serde_json::Value>().await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    let conflict_resp = client
        .post(format!("{source_base}/v1/store"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({
            "content": "Project policy: do not use feature branches for deploys.",
            "tags": ["policy", "project"]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(conflict_resp.status(), 200);
    let conflict_id = conflict_resp.json::<serde_json::Value>().await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    let replacement_resp = client
        .post(format!("{source_base}/v1/store"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({
            "content": "Project policy: use protected release branches for deploys.",
            "tags": ["policy", "project"]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(replacement_resp.status(), 200);
    let replacement_id = replacement_resp.json::<serde_json::Value>().await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    let feedback_resp = client
        .post(format!("{source_base}/v1/feedback"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({
            "id": old_id,
            "action": "supersede",
            "superseded_by": replacement_id
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(feedback_resp.status(), 200);

    let history_resp = client
        .get(format!("{source_base}/v1/history/{old_id}"))
        .header("Authorization", "Bearer test-key-integration")
        .send()
        .await
        .unwrap();
    assert_eq!(history_resp.status(), 200);
    let source_history: serde_json::Value = history_resp.json().await.unwrap();
    assert_eq!(source_history["root_id"].as_str(), Some(old_id.as_str()));
    assert_eq!(source_history["branch_safe"].as_bool(), Some(true));
    assert_eq!(source_history["nodes"].as_array().unwrap().len(), 3);
    assert!(
        source_history["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|node| node["id"].as_str() == Some(conflict_id.as_str()))
    );
    assert!(
        source_history["timeline"]
            .as_array()
            .unwrap()
            .iter()
            .any(|event| event["kind"].as_str() == Some("contradicted"))
    );
    assert!(
        source_history["timeline"]
            .as_array()
            .unwrap()
            .iter()
            .any(|event| event["kind"].as_str() == Some("review"))
    );
    assert!(
        source_history["timeline"]
            .as_array()
            .unwrap()
            .iter()
            .any(|event| event["kind"].as_str() == Some("superseded"))
    );

    let bundle_resp = client
        .get(format!("{source_base}/v1/history/{old_id}/bundle"))
        .header("Authorization", "Bearer test-key-integration")
        .send()
        .await
        .unwrap();
    assert_eq!(bundle_resp.status(), 200);
    let bundle: serde_json::Value = bundle_resp.json().await.unwrap();
    assert_eq!(
        bundle["bundle_version"].as_str(),
        Some("memoryoss.history.v1alpha1")
    );
    assert_eq!(bundle["memories"].as_array().unwrap().len(), 3);

    source_child.kill().await.ok();
    source_child.wait().await.ok();

    let target_port = free_port();
    let target_tmp = tempfile::tempdir().expect("failed to create target temp dir");
    let target_data_dir = target_tmp.path().join("data");
    std::fs::create_dir_all(&target_data_dir).unwrap();
    let target_config = test_config_http(target_port, target_data_dir.to_str().unwrap());
    let target_config_path = target_tmp.path().join("history-target.toml");
    std::fs::write(&target_config_path, &target_config).unwrap();

    let mut target_child = start_server(target_config_path.to_str().unwrap()).await;
    let target_base = format!("http://127.0.0.1:{target_port}");

    let dry_run_resp = client
        .post(format!("{target_base}/v1/history/replay"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({
            "dry_run": true,
            "bundle": bundle.clone()
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(dry_run_resp.status(), 200);
    let dry_run_body: serde_json::Value = dry_run_resp.json().await.unwrap();
    assert_eq!(dry_run_body["preview"]["can_replay"].as_bool(), Some(true));
    assert_eq!(dry_run_body["preview"]["create_count"].as_u64(), Some(3));

    let replay_resp = client
        .post(format!("{target_base}/v1/history/replay"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({
            "bundle": bundle
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(replay_resp.status(), 200);
    let replay_body: serde_json::Value = replay_resp.json().await.unwrap();
    assert_eq!(replay_body["imported"].as_u64(), Some(3));

    let target_history_resp = client
        .get(format!("{target_base}/v1/history/{old_id}"))
        .header("Authorization", "Bearer test-key-integration")
        .send()
        .await
        .unwrap();
    assert_eq!(target_history_resp.status(), 200);
    let target_history: serde_json::Value = target_history_resp.json().await.unwrap();
    assert_eq!(
        target_history["visible_memory_ids"],
        source_history["visible_memory_ids"]
    );
    assert_eq!(
        target_history["timeline"].as_array().unwrap().len(),
        source_history["timeline"].as_array().unwrap().len()
    );

    let inspected = inspect_memory(&client, &target_base, "test-key-integration", &old_id).await;
    assert_eq!(
        inspected["superseded_by"].as_str(),
        Some(replacement_id.as_str())
    );
    assert!(
        target_history["timeline"]
            .as_array()
            .unwrap()
            .iter()
            .any(|event| event["kind"].as_str() == Some("contradicted"))
    );

    target_child.kill().await.ok();
    target_child.wait().await.ok();
}

#[tokio::test]
async fn test_cli_history_branch_into_empty_namespace() {
    let source_port = free_port();
    let tmp = tempfile::tempdir().expect("failed to create temp dir");
    let data_dir = tmp.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let source_config = test_config_http(source_port, data_dir.to_str().unwrap());
    let source_config_path = tmp.path().join("history-cli-source.toml");
    std::fs::write(&source_config_path, &source_config).unwrap();

    let mut source_child = start_server(source_config_path.to_str().unwrap()).await;
    let client = reqwest::Client::builder().build().unwrap();
    let source_base = format!("http://127.0.0.1:{source_port}");

    let old_resp = client
        .post(format!("{source_base}/v1/store"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({
            "content": "Release notes live in docs/releases.md.",
            "tags": ["project", "docs"]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(old_resp.status(), 200);
    let old_id = old_resp.json::<serde_json::Value>().await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    let replacement_resp = client
        .post(format!("{source_base}/v1/store"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({
            "content": "The release checklist lives in docs/releases/README.md and is owned by the release captain.",
            "tags": ["project", "docs"]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(replacement_resp.status(), 200);
    let replacement_id = replacement_resp.json::<serde_json::Value>().await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    let feedback_resp = client
        .post(format!("{source_base}/v1/feedback"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({
            "id": old_id,
            "action": "supersede",
            "superseded_by": replacement_id
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(feedback_resp.status(), 200);

    source_child.kill().await.ok();
    source_child.wait().await.ok();

    let dry_run = tokio::process::Command::new(env!("CARGO_BIN_EXE_memoryoss"))
        .args([
            "--config",
            source_config_path.to_str().unwrap(),
            "history",
            "branch",
            &old_id,
            "--namespace",
            "test",
            "--target-namespace",
            "branch",
            "--dry-run",
        ])
        .output()
        .await
        .expect("failed to run history branch dry-run");
    assert!(
        dry_run.status.success(),
        "history branch dry-run failed: stdout={} stderr={}",
        String::from_utf8_lossy(&dry_run.stdout),
        String::from_utf8_lossy(&dry_run.stderr)
    );
    let dry_run_stdout = String::from_utf8_lossy(&dry_run.stdout);
    assert!(dry_run_stdout.contains("can_replay=true"));
    assert!(dry_run_stdout.contains("create=2"));

    let branch = tokio::process::Command::new(env!("CARGO_BIN_EXE_memoryoss"))
        .args([
            "--config",
            source_config_path.to_str().unwrap(),
            "history",
            "branch",
            &old_id,
            "--namespace",
            "test",
            "--target-namespace",
            "branch",
        ])
        .output()
        .await
        .expect("failed to run history branch");
    assert!(
        branch.status.success(),
        "history branch failed: stdout={} stderr={}",
        String::from_utf8_lossy(&branch.stdout),
        String::from_utf8_lossy(&branch.stderr)
    );
    let branch_stdout = String::from_utf8_lossy(&branch.stdout);
    assert!(branch_stdout.contains("Branched history root"));

    let branch_port = free_port();
    let branch_config = test_config_http(branch_port, data_dir.to_str().unwrap())
        .replace("namespace = \"test\"", "namespace = \"branch\"");
    let branch_config_path = tmp.path().join("history-cli-branch.toml");
    std::fs::write(&branch_config_path, &branch_config).unwrap();

    let mut branch_child = start_server(branch_config_path.to_str().unwrap()).await;
    let branch_base = format!("http://127.0.0.1:{branch_port}");
    let history_resp = client
        .get(format!("{branch_base}/v1/history/{old_id}"))
        .header("Authorization", "Bearer test-key-integration")
        .send()
        .await
        .unwrap();
    assert_eq!(history_resp.status(), 200);
    let history_body: serde_json::Value = history_resp.json().await.unwrap();
    assert_eq!(history_body["root_id"].as_str(), Some(old_id.as_str()));
    assert_eq!(history_body["nodes"].as_array().unwrap().len(), 2);

    branch_child.kill().await.ok();
    branch_child.wait().await.ok();
}

#[tokio::test]
async fn test_conformance_cli_normalizes_canonical_fixtures() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let cases = [
        (
            "runtime_contract",
            root.join("conformance/fixtures/runtime-contract.json"),
        ),
        (
            "passport",
            root.join("conformance/fixtures/passport-bundle.json"),
        ),
        (
            "history",
            root.join("conformance/fixtures/history-bundle.json"),
        ),
    ];

    for (kind, input) in cases {
        let tmp = tempfile::tempdir().expect("failed to create conformance temp dir");
        let output = tmp.path().join(format!("{kind}.normalized.json"));
        let result = tokio::process::Command::new(env!("CARGO_BIN_EXE_memoryoss"))
            .args([
                "conformance",
                "normalize",
                "--kind",
                kind,
                "--input",
                input.to_str().unwrap(),
                "--output",
                output.to_str().unwrap(),
            ])
            .output()
            .await
            .expect("failed to run conformance normalize");
        assert!(
            result.status.success(),
            "conformance normalize failed for {kind}: stdout={} stderr={}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );

        let fixture: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&input).unwrap()).unwrap();
        let normalized: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&output).unwrap()).unwrap();
        assert_eq!(normalized, fixture, "normalized {kind} fixture diverged");
    }
}

#[tokio::test]
async fn test_lts_compatibility_matrix_supports_n_n1_n2_for_runtime_bundle_and_reader() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let runtime_fixture_path = root.join("conformance/fixtures/runtime-contract.json");
    let runtime_fixture: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&runtime_fixture_path).unwrap()).unwrap();

    let port = free_port();
    let tmp = tempfile::tempdir().expect("failed to create LTS compatibility temp dir");
    let data_dir = tmp.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let config_path = tmp.path().join("lts-bundle.toml");
    std::fs::write(
        &config_path,
        test_config_http(port, data_dir.to_str().unwrap()),
    )
    .unwrap();

    let mut child = start_server(config_path.to_str().unwrap()).await;
    let client = reqwest::Client::builder().build().unwrap();
    let base = format!("http://127.0.0.1:{port}");
    let store_resp = client
        .post(format!("{base}/v1/store"))
        .header("Authorization", "Bearer test-key-integration")
        .json(&serde_json::json!({
            "content": "Compatibility lane: published bundle fixtures must stay readable for N, N-1, and N-2.",
            "tags": ["compatibility", "lts"]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(store_resp.status(), 200);
    child.kill().await.ok();
    child.wait().await.ok();

    let current_bundle_path = tmp.path().join("current-lts-bundle.membundle.json");
    let export = tokio::process::Command::new(env!("CARGO_BIN_EXE_memoryoss"))
        .args([
            "--config",
            config_path.to_str().unwrap(),
            "bundle",
            "export",
            "--kind",
            "passport",
            "--namespace",
            "test",
            "--scope",
            "project",
            "--output",
            current_bundle_path.to_str().unwrap(),
        ])
        .output()
        .await
        .expect("failed to export LTS bundle fixture");
    assert!(
        export.status.success(),
        "lts bundle export failed: stdout={} stderr={}",
        String::from_utf8_lossy(&export.stdout),
        String::from_utf8_lossy(&export.stderr)
    );
    let current_bundle: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&current_bundle_path).unwrap()).unwrap();
    let missing_config = tmp.path().join("missing-lts.toml");

    for (window, published_in) in [("n", "v0.1.2"), ("n-1", "v0.1.1"), ("n-2", "v0.1.0")] {
        let runtime_variant =
            compatibility_fixture_snapshot(runtime_fixture.clone(), window, published_in);
        let runtime_variant_path = tmp.path().join(format!("runtime-{window}.json"));
        std::fs::write(
            &runtime_variant_path,
            serde_json::to_vec_pretty(&runtime_variant).unwrap(),
        )
        .unwrap();
        let runtime_output = tmp.path().join(format!("runtime-{window}.normalized.json"));
        let normalize = tokio::process::Command::new(env!("CARGO_BIN_EXE_memoryoss"))
            .args([
                "conformance",
                "normalize",
                "--kind",
                "runtime_contract",
                "--input",
                runtime_variant_path.to_str().unwrap(),
                "--output",
                runtime_output.to_str().unwrap(),
            ])
            .output()
            .await
            .expect("failed to normalize LTS runtime fixture");
        assert!(
            normalize.status.success(),
            "lts runtime normalize failed for {window}: stdout={} stderr={}",
            String::from_utf8_lossy(&normalize.stdout),
            String::from_utf8_lossy(&normalize.stderr)
        );
        let normalized_runtime: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&runtime_output).unwrap()).unwrap();
        assert_eq!(
            normalized_runtime["contract_id"], runtime_fixture["contract_id"],
            "runtime contract id drifted in {window}"
        );

        let bundle_variant =
            compatibility_fixture_snapshot(current_bundle.clone(), window, published_in);
        let bundle_variant_path = tmp.path().join(format!("bundle-{window}.json"));
        std::fs::write(
            &bundle_variant_path,
            serde_json::to_vec_pretty(&bundle_variant).unwrap(),
        )
        .unwrap();

        let validate = tokio::process::Command::new(env!("CARGO_BIN_EXE_memoryoss"))
            .args([
                "--config",
                missing_config.to_str().unwrap(),
                "bundle",
                "validate",
                bundle_variant_path.to_str().unwrap(),
            ])
            .output()
            .await
            .expect("failed to validate LTS bundle fixture");
        assert!(
            validate.status.success(),
            "lts bundle validate failed for {window}: stdout={} stderr={}",
            String::from_utf8_lossy(&validate.stdout),
            String::from_utf8_lossy(&validate.stderr)
        );
        assert!(
            String::from_utf8_lossy(&validate.stdout).contains("Validation: true"),
            "lts bundle validate did not report success for {window}"
        );

        let open = tokio::process::Command::new(env!("CARGO_BIN_EXE_memoryoss"))
            .args([
                "--config",
                missing_config.to_str().unwrap(),
                "reader",
                "open",
                bundle_variant_path.to_str().unwrap(),
                "--format",
                "json",
            ])
            .output()
            .await
            .expect("failed to open LTS bundle fixture");
        assert!(
            open.status.success(),
            "lts bundle reader open failed for {window}: stdout={} stderr={}",
            String::from_utf8_lossy(&open.stdout),
            String::from_utf8_lossy(&open.stderr)
        );
        let open_body: serde_json::Value = serde_json::from_slice(&open.stdout).unwrap();
        assert_eq!(
            open_body["artifact_type"].as_str(),
            Some("memory_bundle_envelope")
        );
        assert_eq!(open_body["kind"].as_str(), Some("passport"));
    }
}

#[tokio::test]
async fn test_lts_compatibility_fixtures_support_n_n1_n2_import_and_replay_paths() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let passport_fixture_path = root.join("conformance/fixtures/passport-bundle.json");
    let history_fixture_path = root.join("conformance/fixtures/history-bundle.json");
    let passport_fixture: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&passport_fixture_path).unwrap()).unwrap();
    let history_fixture: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&history_fixture_path).unwrap()).unwrap();

    let tmp = tempfile::tempdir().expect("failed to create LTS fixture temp dir");
    let data_dir = tmp.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let config_path = tmp.path().join("lts-fixtures.toml");
    std::fs::write(
        &config_path,
        test_config_http(free_port(), data_dir.to_str().unwrap()),
    )
    .unwrap();
    let missing_config = tmp.path().join("missing-reader.toml");

    for (window, published_in) in [("n", "v0.1.2"), ("n-1", "v0.1.1"), ("n-2", "v0.1.0")] {
        let passport_variant =
            compatibility_fixture_snapshot(passport_fixture.clone(), window, published_in);
        let passport_path = tmp.path().join(format!("passport-{window}.json"));
        std::fs::write(
            &passport_path,
            serde_json::to_vec_pretty(&passport_variant).unwrap(),
        )
        .unwrap();

        let passport_open = tokio::process::Command::new(env!("CARGO_BIN_EXE_memoryoss"))
            .args([
                "--config",
                missing_config.to_str().unwrap(),
                "reader",
                "open",
                passport_path.to_str().unwrap(),
                "--format",
                "json",
            ])
            .output()
            .await
            .expect("failed to open LTS passport fixture");
        assert!(
            passport_open.status.success(),
            "lts passport reader open failed for {window}: stdout={} stderr={}",
            String::from_utf8_lossy(&passport_open.stdout),
            String::from_utf8_lossy(&passport_open.stderr)
        );
        let passport_body: serde_json::Value =
            serde_json::from_slice(&passport_open.stdout).unwrap();
        assert_eq!(passport_body["kind"].as_str(), Some("passport"));
        assert_eq!(
            passport_body["artifact_line"].as_str(),
            Some("memoryoss.passport.v1alpha1")
        );

        let passport_import = tokio::process::Command::new(env!("CARGO_BIN_EXE_memoryoss"))
            .args([
                "--config",
                config_path.to_str().unwrap(),
                "passport",
                "import",
                passport_path.to_str().unwrap(),
                "--namespace",
                "test",
                "--dry-run",
            ])
            .output()
            .await
            .expect("failed to dry-run LTS passport import");
        assert!(
            passport_import.status.success(),
            "lts passport import failed for {window}: stdout={} stderr={}",
            String::from_utf8_lossy(&passport_import.stdout),
            String::from_utf8_lossy(&passport_import.stderr)
        );
        assert!(
            String::from_utf8_lossy(&passport_import.stdout).contains("create="),
            "lts passport import preview missing create count for {window}"
        );

        let history_variant =
            compatibility_fixture_snapshot(history_fixture.clone(), window, published_in);
        let history_path = tmp.path().join(format!("history-{window}.json"));
        std::fs::write(
            &history_path,
            serde_json::to_vec_pretty(&history_variant).unwrap(),
        )
        .unwrap();

        let history_open = tokio::process::Command::new(env!("CARGO_BIN_EXE_memoryoss"))
            .args([
                "--config",
                missing_config.to_str().unwrap(),
                "reader",
                "open",
                history_path.to_str().unwrap(),
                "--format",
                "json",
            ])
            .output()
            .await
            .expect("failed to open LTS history fixture");
        assert!(
            history_open.status.success(),
            "lts history reader open failed for {window}: stdout={} stderr={}",
            String::from_utf8_lossy(&history_open.stdout),
            String::from_utf8_lossy(&history_open.stderr)
        );
        let history_body: serde_json::Value = serde_json::from_slice(&history_open.stdout).unwrap();
        assert_eq!(history_body["kind"].as_str(), Some("history"));
        assert_eq!(
            history_body["artifact_line"].as_str(),
            Some("memoryoss.history.v1alpha1")
        );

        let history_replay = tokio::process::Command::new(env!("CARGO_BIN_EXE_memoryoss"))
            .args([
                "--config",
                config_path.to_str().unwrap(),
                "history",
                "replay",
                history_path.to_str().unwrap(),
                "--namespace",
                "test",
                "--dry-run",
            ])
            .output()
            .await
            .expect("failed to dry-run LTS history replay");
        assert!(
            history_replay.status.success(),
            "lts history replay failed for {window}: stdout={} stderr={}",
            String::from_utf8_lossy(&history_replay.stdout),
            String::from_utf8_lossy(&history_replay.stderr)
        );
        let replay_stdout = String::from_utf8_lossy(&history_replay.stdout);
        assert!(replay_stdout.contains("can_replay=true"));
        assert!(replay_stdout.contains("create="));
    }
}
