// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 memoryOSS Contributors
//
// MCP server — thin HTTP client that delegates to the running memoryOSS HTTP server.
// No direct DB access, no lock conflicts. Requires the HTTP server to be running.

use std::borrow::Cow;
use std::sync::Arc;

use rmcp::ServerHandler;
use rmcp::handler::server::tool::schema_for_type;
use rmcp::model::*;
use rmcp::service::{Peer, RequestContext, RoleServer};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::config::Config;

// -- Tool parameter types (define the MCP schema) --

#[allow(dead_code)]
#[derive(Debug, Clone, Copy)]
struct MarketplaceToolAnnotations {
    read_only_hint: bool,
    destructive_hint: bool,
}

impl MarketplaceToolAnnotations {
    const fn read_only() -> Self {
        Self {
            read_only_hint: true,
            destructive_hint: false,
        }
    }

    const fn destructive() -> Self {
        Self {
            read_only_hint: false,
            destructive_hint: true,
        }
    }
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy)]
struct MarketplaceToolDefinition {
    name: &'static str,
    title: &'static str,
    description: &'static str,
    annotations: MarketplaceToolAnnotations,
}

// rmcp 0.1.5 does not yet serialize MCP spec-era tool titles and safety hints on tools/list.
// Keep the Anthropic/marketplace metadata in one place so it stays testable and ready to wire
// through once the transport layer exposes the fields.
const MARKETPLACE_TOOLS: [MarketplaceToolDefinition; 4] = [
    MarketplaceToolDefinition {
        name: "memoryoss_recall",
        title: "Recall Relevant Memories",
        description: "Search your long-term memory. Call this FIRST at the start of every conversation turn to retrieve relevant context from previous sessions. Pass the user's question or topic as the query. Returns ranked results with content and relevance scores. Always check memory before answering — you may already know things the user told you before.",
        annotations: MarketplaceToolAnnotations::read_only(),
    },
    MarketplaceToolDefinition {
        name: "memoryoss_store",
        title: "Store New Memory",
        description: "Save important information to long-term memory so you can recall it in future sessions. Store facts, decisions, user preferences, project context, and key findings. Call this whenever you learn something worth remembering — if in doubt, store it. Memories persist across sessions and are searchable by content.",
        annotations: MarketplaceToolAnnotations::destructive(),
    },
    MarketplaceToolDefinition {
        name: "memoryoss_update",
        title: "Update Existing Memory",
        description: "Update an existing memory by ID. Use this when information has changed — e.g. a user preference was corrected or a fact is outdated. Pass the memory ID from a previous recall result.",
        annotations: MarketplaceToolAnnotations::destructive(),
    },
    MarketplaceToolDefinition {
        name: "memoryoss_forget",
        title: "Delete Stored Memory",
        description: "Delete memories by their IDs. Use when the user asks you to forget something or when stored information is wrong and should not be updated but removed entirely.",
        annotations: MarketplaceToolAnnotations::destructive(),
    },
];

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct StoreParams {
    /// The text content to store as a memory
    pub content: String,
    /// Optional tags for categorization
    #[serde(default)]
    pub tags: Vec<String>,
    /// Optional agent identifier
    pub agent: Option<String>,
    /// Optional session identifier
    pub session: Option<String>,
    /// Optional namespace (default: "default")
    pub namespace: Option<String>,
    /// Memory type: episodic, semantic, procedural, or working
    pub memory_type: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct RecallParams {
    /// Natural language query to search memories
    pub query: String,
    /// Maximum number of results (default: 10)
    pub limit: Option<usize>,
    /// Optional agent filter
    pub agent: Option<String>,
    /// Optional session filter
    pub session: Option<String>,
    /// Optional namespace (default: "default")
    pub namespace: Option<String>,
    /// Filter by tags
    #[serde(default)]
    pub tags: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct UpdateParams {
    /// UUID of the memory to update
    pub id: String,
    /// New content (optional)
    pub content: Option<String>,
    /// New tags (optional)
    pub tags: Option<Vec<String>>,
    /// Optional namespace (default: "default")
    pub namespace: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct ForgetParams {
    /// UUIDs of memories to delete
    #[serde(default)]
    pub ids: Vec<String>,
    /// Optional namespace (default: "default")
    pub namespace: Option<String>,
}

fn tool_schema<T: JsonSchema>() -> Arc<JsonObject> {
    let schema = schema_for_type::<T>();
    Arc::new(schema)
}

fn tool_schema_for(name: &str) -> Arc<JsonObject> {
    match name {
        "memoryoss_recall" => tool_schema::<RecallParams>(),
        "memoryoss_store" => tool_schema::<StoreParams>(),
        "memoryoss_update" => tool_schema::<UpdateParams>(),
        "memoryoss_forget" => tool_schema::<ForgetParams>(),
        _ => unreachable!("missing MCP schema for tool {name}"),
    }
}

fn build_marketplace_tool(definition: &MarketplaceToolDefinition) -> Tool {
    Tool {
        name: Cow::Borrowed(definition.name),
        description: Cow::Borrowed(definition.description),
        input_schema: tool_schema_for(definition.name),
    }
}

#[cfg(test)]
fn marketplace_tool_metadata() -> Vec<serde_json::Value> {
    MARKETPLACE_TOOLS
        .iter()
        .map(|definition| {
            serde_json::json!({
                "name": definition.name,
                "title": definition.title,
                "annotations": {
                    "readOnlyHint": definition.annotations.read_only_hint,
                    "destructiveHint": definition.annotations.destructive_hint,
                }
            })
        })
        .collect()
}

fn tools_list() -> Vec<Tool> {
    MARKETPLACE_TOOLS
        .iter()
        .map(build_marketplace_tool)
        .collect()
}

// ---------------------------------------------------------------------------
// MCP server — HTTP client mode (no direct DB access)
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct MemoryOssMcp {
    client: reqwest::Client,
    base_url: String,
    api_key: String,
    peer: Option<Peer<RoleServer>>,
}

impl MemoryOssMcp {
    pub fn new(base_url: String, api_key: String, accept_invalid_certs: bool) -> Self {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .danger_accept_invalid_certs(accept_invalid_certs)
            .build()
            .expect("failed to build HTTP client");
        Self {
            client,
            base_url,
            api_key,
            peer: None,
        }
    }

    async fn http_post(
        &self,
        path: &str,
        body: serde_json::Value,
    ) -> Result<serde_json::Value, rmcp::Error> {
        let url = format!("{}{}", self.base_url, path);
        let resp = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                rmcp::Error::internal_error(format!("HTTP request to {} failed: {e}", url), None)
            })?;

        let status = resp.status();
        let text = resp.text().await.map_err(|e| {
            rmcp::Error::internal_error(format!("failed to read response body: {e}"), None)
        })?;

        if !status.is_success() {
            return Err(rmcp::Error::internal_error(
                format!("HTTP {} from {}: {}", status, path, text),
                None,
            ));
        }

        serde_json::from_str(&text)
            .map_err(|e| rmcp::Error::internal_error(format!("invalid JSON response: {e}"), None))
    }

    async fn http_patch(
        &self,
        path: &str,
        body: serde_json::Value,
    ) -> Result<serde_json::Value, rmcp::Error> {
        let url = format!("{}{}", self.base_url, path);
        let resp = self
            .client
            .patch(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                rmcp::Error::internal_error(format!("HTTP request to {} failed: {e}", url), None)
            })?;

        let status = resp.status();
        let text = resp.text().await.map_err(|e| {
            rmcp::Error::internal_error(format!("failed to read response body: {e}"), None)
        })?;

        if !status.is_success() {
            return Err(rmcp::Error::internal_error(
                format!("HTTP {} from {}: {}", status, path, text),
                None,
            ));
        }

        serde_json::from_str(&text)
            .map_err(|e| rmcp::Error::internal_error(format!("invalid JSON response: {e}"), None))
    }

    async fn http_delete(
        &self,
        path: &str,
        body: serde_json::Value,
    ) -> Result<serde_json::Value, rmcp::Error> {
        let url = format!("{}{}", self.base_url, path);
        let resp = self
            .client
            .delete(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                rmcp::Error::internal_error(format!("HTTP request to {} failed: {e}", url), None)
            })?;

        let status = resp.status();
        let text = resp.text().await.map_err(|e| {
            rmcp::Error::internal_error(format!("failed to read response body: {e}"), None)
        })?;

        if !status.is_success() {
            return Err(rmcp::Error::internal_error(
                format!("HTTP {} from {}: {}", status, path, text),
                None,
            ));
        }

        serde_json::from_str(&text)
            .map_err(|e| rmcp::Error::internal_error(format!("invalid JSON response: {e}"), None))
    }

    async fn handle_store(&self, args: JsonObject) -> Result<CallToolResult, rmcp::Error> {
        let params: StoreParams = serde_json::from_value(serde_json::Value::Object(args))
            .map_err(|e| rmcp::Error::invalid_params(format!("invalid params: {e}"), None))?;

        let mut body = serde_json::json!({
            "content": params.content,
            "tags": params.tags,
        });

        if let Some(agent) = params.agent {
            body["agent"] = serde_json::json!(agent);
        }
        if let Some(session) = params.session {
            body["session"] = serde_json::json!(session);
        }
        if let Some(namespace) = params.namespace {
            body["namespace"] = serde_json::json!(namespace);
        }
        if let Some(memory_type) = params.memory_type {
            body["memory_type"] = serde_json::json!(memory_type);
        }

        let resp = self.http_post("/v1/store", body).await?;

        // The HTTP API returns { "id": "...", "version": N }
        let result = serde_json::json!({
            "id": resp.get("id").and_then(|v| v.as_str()).unwrap_or(""),
            "version": resp.get("version").and_then(|v| v.as_u64()).unwrap_or(0),
            "stored": true,
        });
        Ok(CallToolResult::success(vec![Content::text(
            result.to_string(),
        )]))
    }

    async fn handle_recall(&self, args: JsonObject) -> Result<CallToolResult, rmcp::Error> {
        let params: RecallParams = serde_json::from_value(serde_json::Value::Object(args))
            .map_err(|e| rmcp::Error::invalid_params(format!("invalid params: {e}"), None))?;

        let mut body = serde_json::json!({
            "query": params.query,
            "limit": params.limit.unwrap_or(10),
        });

        if let Some(agent) = params.agent {
            body["agent"] = serde_json::json!(agent);
        }
        if let Some(session) = params.session {
            body["session"] = serde_json::json!(session);
        }
        if let Some(namespace) = params.namespace {
            body["namespace"] = serde_json::json!(namespace);
        }
        if !params.tags.is_empty() {
            body["tags"] = serde_json::json!(params.tags);
        }

        let resp = self.http_post("/v1/recall", body).await?;

        // The HTTP API returns { "memories": [...] }
        // Simplify for LLM consumption
        let memories = resp
            .get("memories")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

        let simplified: Vec<serde_json::Value> = memories
            .iter()
            .filter_map(|sm| {
                let memory = sm.get("memory")?;
                Some(serde_json::json!({
                    "id": memory.get("id").and_then(|v| v.as_str()).unwrap_or(""),
                    "content": memory.get("content").and_then(|v| v.as_str()).unwrap_or(""),
                    "tags": memory.get("tags").unwrap_or(&serde_json::json!([])),
                    "agent": memory.get("agent"),
                    "score": sm.get("score").and_then(|v| v.as_f64())
                        .map(|s| format!("{:.3}", s)).unwrap_or_default(),
                    "created_at": memory.get("created_at").and_then(|v| v.as_str()).unwrap_or(""),
                    "memory_type": memory.get("memory_type").and_then(|v| v.as_str()).unwrap_or("episodic"),
                }))
            })
            .collect();

        let text = serde_json::to_string_pretty(&simplified).unwrap_or_else(|_| "[]".to_string());
        Ok(CallToolResult::success(vec![Content::text(text)]))
    }

    async fn handle_update(&self, args: JsonObject) -> Result<CallToolResult, rmcp::Error> {
        let params: UpdateParams = serde_json::from_value(serde_json::Value::Object(args))
            .map_err(|e| rmcp::Error::invalid_params(format!("invalid params: {e}"), None))?;

        let id = params.id.clone();
        let mut body = serde_json::json!({
            "id": params.id,
        });

        if let Some(content) = params.content {
            body["content"] = serde_json::json!(content);
        }
        if let Some(tags) = params.tags {
            body["tags"] = serde_json::json!(tags);
        }
        if let Some(namespace) = params.namespace {
            body["namespace"] = serde_json::json!(namespace);
        }

        let resp = self.http_patch("/v1/update", body).await?;

        let result = serde_json::json!({
            "id": resp.get("id").and_then(|v| v.as_str()).unwrap_or(&id),
            "version": resp.get("version").and_then(|v| v.as_u64()).unwrap_or(0),
            "updated": true,
        });
        Ok(CallToolResult::success(vec![Content::text(
            result.to_string(),
        )]))
    }

    async fn handle_forget(&self, args: JsonObject) -> Result<CallToolResult, rmcp::Error> {
        let params: ForgetParams = serde_json::from_value(serde_json::Value::Object(args))
            .map_err(|e| rmcp::Error::invalid_params(format!("invalid params: {e}"), None))?;

        let mut body = serde_json::json!({
            "ids": params.ids,
        });

        if let Some(namespace) = params.namespace {
            body["namespace"] = serde_json::json!(namespace);
        }

        let resp = self.http_delete("/v1/forget", body).await?;

        let result = serde_json::json!({
            "deleted": resp.get("deleted").and_then(|v| v.as_u64()).unwrap_or(0),
        });
        Ok(CallToolResult::success(vec![Content::text(
            result.to_string(),
        )]))
    }
}

impl ServerHandler for MemoryOssMcp {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            protocol_version: ProtocolVersion::default(),
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            server_info: Implementation {
                name: "memoryoss".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
            },
            instructions: Some(
                "memoryoss gives you persistent long-term memory across sessions. \
                IMPORTANT: Call memoryoss_recall at the START of every conversation turn to check \
                what you already know. Call memoryoss_store whenever you learn something worth \
                remembering (facts, preferences, decisions, context). Your memory is your \
                superpower — use it proactively."
                    .to_string(),
            ),
        }
    }

    fn list_tools(
        &self,
        _request: PaginatedRequestParam,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListToolsResult, rmcp::Error>> + Send + '_ {
        std::future::ready(Ok(ListToolsResult {
            next_cursor: None,
            tools: tools_list(),
        }))
    }

    fn call_tool(
        &self,
        request: CallToolRequestParam,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<CallToolResult, rmcp::Error>> + Send + '_ {
        async move {
            let args = request.arguments.unwrap_or_default();
            match request.name.as_ref() {
                "memoryoss_store" => self.handle_store(args).await,
                "memoryoss_recall" => self.handle_recall(args).await,
                "memoryoss_update" => self.handle_update(args).await,
                "memoryoss_forget" => self.handle_forget(args).await,
                _ => Err(rmcp::Error::invalid_params(
                    format!("unknown tool: {}", request.name),
                    None,
                )),
            }
        }
    }

    fn get_peer(&self) -> Option<Peer<RoleServer>> {
        self.peer.clone()
    }

    fn set_peer(&mut self, peer: Peer<RoleServer>) {
        self.peer = Some(peer);
    }
}

/// Run MCP server in HTTP client mode — connects to the running HTTP server.
/// No direct DB access, no lock conflicts.
pub async fn run_mcp_server(
    config: Config,
    _config_path: std::path::PathBuf,
) -> anyhow::Result<()> {
    let scheme = if config.tls.enabled { "https" } else { "http" };
    let base_url = format!("{}://{}", scheme, config.bind_addr());

    // Get the first API key from config for authentication
    let api_key = config
        .auth
        .api_keys
        .first()
        .map(|k| k.key.clone())
        .ok_or_else(|| anyhow::anyhow!(
            "No API keys configured. The MCP server needs an API key to authenticate with the HTTP server. \
             Add at least one key in [auth] api_keys."
        ))?;

    eprintln!("memoryoss MCP: connecting to HTTP server at {}", base_url);

    // Verify the HTTP server is reachable
    // Accept self-signed certs for auto_generate TLS (local loopback)
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .danger_accept_invalid_certs(config.tls.enabled && config.tls.auto_generate)
        .build()?;

    match client.get(format!("{}/health", base_url)).send().await {
        Ok(resp) if resp.status().is_success() => {
            eprintln!("memoryoss MCP: HTTP server is healthy");
        }
        Ok(resp) => {
            eprintln!(
                "memoryoss MCP: WARNING — HTTP server returned status {}. \
                 Make sure the memoryoss HTTP server is running.",
                resp.status()
            );
        }
        Err(e) => {
            eprintln!(
                "memoryoss MCP: WARNING — cannot reach HTTP server at {}: {}. \
                 Make sure the memoryoss HTTP server is running (e.g. systemctl start memoryoss).",
                base_url, e
            );
        }
    }

    let accept_invalid_certs = config.tls.enabled && config.tls.auto_generate;
    let server = MemoryOssMcp::new(base_url, api_key, accept_invalid_certs);
    let transport = rmcp::transport::io::stdio();

    eprintln!("memoryoss MCP server ready (HTTP client mode, stdio)");
    let service = rmcp::serve_server(server, transport)
        .await
        .map_err(|e: std::io::Error| anyhow::anyhow!("MCP server error: {e}"))?;

    service
        .waiting()
        .await
        .map_err(|e| anyhow::anyhow!("MCP server join error: {e}"))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{MARKETPLACE_TOOLS, marketplace_tool_metadata, tools_list};

    #[test]
    fn marketplace_tool_metadata_covers_every_tool() {
        let tool_names: Vec<String> = tools_list()
            .into_iter()
            .map(|tool| tool.name.into_owned())
            .collect();
        let metadata = marketplace_tool_metadata();

        assert_eq!(tool_names.len(), metadata.len());
        for definition in MARKETPLACE_TOOLS {
            assert!(!definition.title.is_empty(), "tool title must be non-empty");
            assert_ne!(
                definition.annotations.read_only_hint, definition.annotations.destructive_hint,
                "tool {} must set exactly one safety hint",
                definition.name
            );
            assert!(
                tool_names.iter().any(|name| name == definition.name),
                "tool {} missing from tools/list",
                definition.name
            );
        }

        for entry in metadata {
            let annotations = entry
                .get("annotations")
                .and_then(|value| value.as_object())
                .expect("metadata annotations missing");
            assert!(
                annotations.contains_key("readOnlyHint"),
                "metadata must include readOnlyHint"
            );
            assert!(
                annotations.contains_key("destructiveHint"),
                "metadata must include destructiveHint"
            );
        }
    }
}
