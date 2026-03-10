// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 memoryOSS Contributors

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::body::{Body, Bytes, to_bytes};
use axum::extract::{DefaultBodyLimit, OriginalUri, State};
use axum::http::{HeaderMap, Method, Request, StatusCode, Uri, request::Parts};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get};
use futures_util::TryStreamExt;
use serde_json::json;
use tokio::net::TcpListener;

use crate::config::{Config, ProxyConfig};

#[derive(Clone)]
struct GatewayState {
    config: Config,
    core_base_url: String,
    core_client: reqwest::Client,
    upstream_client: reqwest::Client,
}

pub async fn run_gateway(
    config: Config,
    config_path: PathBuf,
    manage_core: bool,
) -> anyhow::Result<()> {
    let bind_addr = config.bind_addr();
    let core_base_url = format!("http://{}", config.core_bind_addr());
    let core_client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()?;
    let upstream_client = reqwest::Client::builder()
        .timeout(Duration::from_secs(300))
        .build()?;

    if manage_core {
        spawn_managed_core(config_path.clone());
    }

    let state = Arc::new(GatewayState {
        config: config.clone(),
        core_base_url,
        core_client,
        upstream_client,
    });

    let app = axum::Router::new()
        .route("/health", get(health))
        .route("/metrics", any(core_only))
        .nest("/v1", axum::Router::new().route("/{*path}", any(core_only)))
        .nest(
            "/proxy",
            axum::Router::new().route("/{*path}", any(proxy_or_fallback)),
        )
        .layer(DefaultBodyLimit::max(2 * 1024 * 1024))
        .with_state(state);

    let listener = TcpListener::bind(&bind_addr).await?;

    let shutdown = async {
        let ctrl_c = tokio::signal::ctrl_c();
        #[cfg(unix)]
        let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to register SIGTERM handler");
        #[cfg(unix)]
        let sigterm_recv = sigterm.recv();
        #[cfg(not(unix))]
        let sigterm_recv = std::future::pending::<Option<()>>();

        tokio::select! {
            _ = ctrl_c => tracing::info!("Received CTRL-C, shutting down hybrid gateway..."),
            _ = sigterm_recv => tracing::info!("Received SIGTERM, shutting down hybrid gateway..."),
        }
    };
    tokio::pin!(shutdown);

    if config.tls.enabled {
        let tls_acceptor = super::tls::build_tls_acceptor(&config.tls)?;
        tracing::info!("Hybrid gateway listening on https://{bind_addr}");

        loop {
            tokio::select! {
                result = listener.accept() => {
                    let (stream, addr) = result?;
                    let acceptor = tls_acceptor.clone();
                    let app = app.clone().layer(axum::Extension(axum::extract::ConnectInfo(addr)));

                    tokio::spawn(async move {
                        match acceptor.accept(stream).await {
                            Ok(tls_stream) => {
                                let io = hyper_util::rt::TokioIo::new(tls_stream);
                                let service = hyper_util::service::TowerToHyperService::new(app);
                                if let Err(err) =
                                    hyper_util::server::conn::auto::Builder::new(hyper_util::rt::TokioExecutor::new())
                                        .serve_connection(io, service)
                                        .await
                                {
                                    tracing::debug!("Gateway connection error from {addr}: {err}");
                                }
                            }
                            Err(err) => {
                                tracing::debug!("Gateway TLS handshake failed from {addr}: {err}");
                            }
                        }
                    });
                }
                _ = &mut shutdown => break,
            }
        }
    } else {
        tracing::info!("Hybrid gateway listening on http://{bind_addr} (TLS disabled)");

        loop {
            tokio::select! {
                result = listener.accept() => {
                    let (stream, addr) = result?;
                    let app = app.clone().layer(axum::Extension(axum::extract::ConnectInfo(addr)));

                    tokio::spawn(async move {
                        let io = hyper_util::rt::TokioIo::new(stream);
                        let service = hyper_util::service::TowerToHyperService::new(app);
                        if let Err(err) =
                            hyper_util::server::conn::auto::Builder::new(hyper_util::rt::TokioExecutor::new())
                                .serve_connection(io, service)
                                .await
                        {
                            tracing::debug!("Gateway connection error from {addr}: {err}");
                        }
                    });
                }
                _ = &mut shutdown => break,
            }
        }
    }
    Ok(())
}

fn spawn_managed_core(config_path: PathBuf) {
    tokio::spawn(async move {
        loop {
            let binary = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("memoryoss"));
            let mut child = match tokio::process::Command::new(&binary)
                .args(["--config", &config_path.to_string_lossy(), "serve-core"])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .kill_on_drop(true)
                .spawn()
            {
                Ok(child) => child,
                Err(e) => {
                    tracing::error!(error = %e, "failed to spawn memory core");
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    continue;
                }
            };

            match child.wait().await {
                Ok(status) => {
                    tracing::warn!(
                        ?status,
                        "memory core exited; gateway staying in passthrough mode"
                    );
                }
                Err(e) => {
                    tracing::warn!(error = %e, "memory core wait failed; gateway staying in passthrough mode");
                }
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    });
}

async fn health(State(state): State<Arc<GatewayState>>) -> Response {
    let core_status = if core_health(&state).await {
        "ok"
    } else {
        "degraded"
    };
    (
        StatusCode::OK,
        axum::Json(json!({
            "status": "ok",
            "mode": "hybrid",
            "core_status": core_status,
        })),
    )
        .into_response()
}

async fn core_only(
    State(state): State<Arc<GatewayState>>,
    OriginalUri(uri): OriginalUri,
    request: Request<Body>,
) -> Response {
    let (parts, body) = request.into_parts();
    let body = match to_bytes(body, 2 * 1024 * 1024).await {
        Ok(bytes) => bytes,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                axum::Json(json!({"error": "request body too large"})),
            )
                .into_response();
        }
    };

    match forward_to_core(&state, &parts.method, &parts.headers, &uri, body).await {
        Ok(resp) => resp,
        Err(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            axum::Json(json!({
                "error": "memoryOSS core unavailable",
                "hint": "proxy fallback keeps model traffic working, but memory API and MCP need the core"
            })),
        )
            .into_response(),
    }
}

async fn proxy_or_fallback(
    State(state): State<Arc<GatewayState>>,
    OriginalUri(uri): OriginalUri,
    request: Request<Body>,
) -> Response {
    let (parts, body) = request.into_parts();
    let body = match to_bytes(body, 2 * 1024 * 1024).await {
        Ok(bytes) => bytes,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                axum::Json(json!({"error": "request body too large"})),
            )
                .into_response();
        }
    };

    if let Ok(resp) =
        forward_to_core(&state, &parts.method, &parts.headers, &uri, body.clone()).await
    {
        return resp;
    }

    match direct_upstream_fallback(&state, &parts, &uri, body).await {
        Ok(resp) => resp,
        Err((status, message)) => (status, axum::Json(json!({ "error": message }))).into_response(),
    }
}

async fn core_health(state: &GatewayState) -> bool {
    state
        .core_client
        .get(format!("{}/health", state.core_base_url))
        .send()
        .await
        .ok()
        .map(|resp| resp.status().is_success())
        .unwrap_or(false)
}

async fn forward_to_core(
    state: &GatewayState,
    method: &Method,
    headers: &HeaderMap,
    uri: &Uri,
    body: Bytes,
) -> anyhow::Result<Response> {
    let url = format!("{}{}", state.core_base_url, uri);
    let mut req = state.core_client.request(method.clone(), &url);
    for (name, value) in headers {
        if name.as_str().eq_ignore_ascii_case("host")
            || name.as_str().eq_ignore_ascii_case("content-length")
        {
            continue;
        }
        req = req.header(name, value);
    }
    let resp = req.body(body).send().await?;
    Ok(reqwest_to_response(resp))
}

async fn direct_upstream_fallback(
    state: &GatewayState,
    parts: &Parts,
    uri: &Uri,
    body: Bytes,
) -> Result<Response, (StatusCode, String)> {
    let allow_passthrough =
        super::proxy::passthrough_allowed_for_request(&state.config.proxy, parts);
    let method = &parts.method;
    let headers = &parts.headers;
    let path = uri.path();

    if path == "/proxy/v1/models" {
        let auth_header = headers.get("authorization").and_then(|v| v.to_str().ok());
        let auth =
            super::proxy::resolve_openai_auth(&state.config.proxy, auth_header, allow_passthrough)
                .ok_or((
                    StatusCode::UNAUTHORIZED,
                    "proxy authentication required".to_string(),
                ))?;
        let url = format!(
            "{}/models",
            state.config.proxy.upstream_url.trim_end_matches('/')
        );
        let req = state
            .upstream_client
            .request(method.clone(), &url)
            .header("Authorization", openai_authorization(&auth));
        let resp = req.send().await.map_err(upstream_error)?;
        return Ok(reqwest_to_response(resp));
    }

    if path == "/proxy/v1/chat/completions" || path == "/proxy/v1/responses" {
        let auth_header = headers.get("authorization").and_then(|v| v.to_str().ok());
        let auth =
            super::proxy::resolve_openai_auth(&state.config.proxy, auth_header, allow_passthrough)
                .ok_or((
                    StatusCode::UNAUTHORIZED,
                    "proxy authentication required".to_string(),
                ))?;
        let body = normalize_openai_body(path, &auth, body)?;
        let upstream_url = openai_upstream_url(&state.config.proxy, &auth, path);
        let req = state
            .upstream_client
            .request(method.clone(), &upstream_url)
            .header("Authorization", openai_authorization(&auth))
            .header("Content-Type", "application/json")
            .body(body);
        let resp = req.send().await.map_err(upstream_error)?;
        return Ok(reqwest_to_response(resp));
    }

    if path == "/proxy/anthropic/v1/messages" || path == "/proxy/anthropic/v1/v1/messages" {
        let auth =
            super::proxy::resolve_anthropic_auth(&state.config.proxy, headers, allow_passthrough)
                .ok_or((
                StatusCode::UNAUTHORIZED,
                "proxy authentication required".to_string(),
            ))?;
        let body = normalize_anthropic_body(&auth, body)?;
        let mut req = state
            .upstream_client
            .request(
                method.clone(),
                state
                    .config
                    .proxy
                    .anthropic_upstream_url
                    .as_deref()
                    .unwrap_or("https://api.anthropic.com/v1/messages"),
            )
            .header("content-type", "application/json")
            .header("anthropic-version", anthropic_version(headers, &auth))
            .body(body);
        req = match auth {
            super::proxy::AnthropicAuth::ApiKey { upstream_key, .. } => {
                req.header("x-api-key", upstream_key)
            }
            super::proxy::AnthropicAuth::OAuthPassthrough { token, .. } => {
                let mut req = req.header("Authorization", format!("Bearer {token}"));
                if let Some(beta) = headers.get("anthropic-beta") {
                    req = req.header("anthropic-beta", beta);
                }
                req
            }
        };
        let resp = req.send().await.map_err(upstream_error)?;
        return Ok(reqwest_to_response(resp));
    }

    Err((
        StatusCode::NOT_FOUND,
        format!("unsupported fallback route: {path}"),
    ))
}

fn openai_authorization(auth: &super::proxy::OpenAIAuth) -> String {
    match auth {
        super::proxy::OpenAIAuth::ApiKey { upstream_key, .. } => {
            format!("Bearer {upstream_key}")
        }
        super::proxy::OpenAIAuth::OAuthPassthrough { token, .. } => {
            format!("Bearer {token}")
        }
    }
}

fn openai_upstream_url(
    proxy_config: &ProxyConfig,
    auth: &super::proxy::OpenAIAuth,
    path: &str,
) -> String {
    let suffix = if path.ends_with("/responses") {
        "responses"
    } else {
        "chat/completions"
    };
    match auth {
        super::proxy::OpenAIAuth::OAuthPassthrough { .. } => {
            if proxy_config.upstream_url == "https://api.openai.com/v1" {
                format!("https://api.openai.com/v1/{suffix}")
            } else {
                format!(
                    "{}/{}",
                    proxy_config.upstream_url.trim_end_matches('/'),
                    suffix
                )
            }
        }
        super::proxy::OpenAIAuth::ApiKey { .. } => {
            format!(
                "{}/{}",
                proxy_config.upstream_url.trim_end_matches('/'),
                suffix
            )
        }
    }
}

fn normalize_openai_body(
    path: &str,
    auth: &super::proxy::OpenAIAuth,
    body: Bytes,
) -> Result<Bytes, (StatusCode, String)> {
    if !path.ends_with("/responses") {
        return Ok(body);
    }
    let mut json: serde_json::Value = serde_json::from_slice(&body)
        .map_err(|_| (StatusCode::BAD_REQUEST, "invalid JSON body".to_string()))?;
    if !matches!(auth, super::proxy::OpenAIAuth::OAuthPassthrough { .. })
        && let Some(obj) = json.as_object_mut()
    {
        obj.remove("context_management");
    }
    serde_json::to_vec(&json).map(Bytes::from).map_err(|_| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "failed to normalize request".to_string(),
        )
    })
}

fn normalize_anthropic_body(
    auth: &super::proxy::AnthropicAuth,
    body: Bytes,
) -> Result<Bytes, (StatusCode, String)> {
    if matches!(auth, super::proxy::AnthropicAuth::OAuthPassthrough { .. }) {
        return Ok(body);
    }
    let mut json: serde_json::Value = serde_json::from_slice(&body)
        .map_err(|_| (StatusCode::BAD_REQUEST, "invalid JSON body".to_string()))?;
    if let Some(obj) = json.as_object_mut() {
        obj.remove("context_management");
    }
    serde_json::to_vec(&json).map(Bytes::from).map_err(|_| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "failed to normalize request".to_string(),
        )
    })
}

fn anthropic_version(headers: &HeaderMap, auth: &super::proxy::AnthropicAuth) -> String {
    if matches!(auth, super::proxy::AnthropicAuth::OAuthPassthrough { .. }) {
        headers
            .get("anthropic-version")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("2023-06-01")
            .to_string()
    } else {
        "2023-06-01".to_string()
    }
}

fn upstream_error(error: reqwest::Error) -> (StatusCode, String) {
    tracing::warn!(%error, "gateway upstream fallback failed");
    (
        StatusCode::BAD_GATEWAY,
        "upstream request failed while memory core was unavailable".to_string(),
    )
}

fn reqwest_to_response(resp: reqwest::Response) -> Response {
    let status = resp.status();
    let headers = resp.headers().clone();
    let stream = resp.bytes_stream().map_err(std::io::Error::other);
    let mut builder = Response::builder().status(status);
    for (name, value) in &headers {
        if name.as_str().eq_ignore_ascii_case("content-length")
            || name.as_str().eq_ignore_ascii_case("transfer-encoding")
        {
            continue;
        }
        builder = builder.header(name, value);
    }
    builder
        .body(Body::from_stream(stream))
        .unwrap_or_else(|_| StatusCode::BAD_GATEWAY.into_response())
}
