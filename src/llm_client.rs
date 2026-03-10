// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 memoryOSS Contributors

//! Shared LLM client for calling Claude, OpenAI, and Ollama APIs.
//! Used by both decompose.rs and the proxy fact extraction pipeline.

use serde::{Deserialize, Serialize};

/// Configuration for an LLM provider call.
#[derive(Debug, Clone)]
pub struct LlmRequest<'a> {
    pub provider: &'a str,
    pub model: &'a str,
    pub api_key: Option<&'a str>,
    pub endpoint: Option<&'a str>,
    pub auth_scheme: Option<&'a str>,
    pub prompt: &'a str,
    pub max_tokens: u32,
    pub timeout_secs: u64,
}

/// Unified response from any LLM provider.
#[derive(Debug, Clone)]
pub struct LlmResponse {
    pub text: String,
}

/// Call the appropriate LLM provider and return the response text.
pub async fn call_llm(req: &LlmRequest<'_>) -> anyhow::Result<LlmResponse> {
    let text = match req.provider {
        "claude" => call_claude(req).await?,
        "openai" => call_openai(req).await?,
        "ollama" => call_ollama(req).await?,
        other => anyhow::bail!("unsupported LLM provider: {other}"),
    };
    Ok(LlmResponse { text })
}

async fn call_claude(req: &LlmRequest<'_>) -> anyhow::Result<String> {
    let api_key = req
        .api_key
        .ok_or_else(|| anyhow::anyhow!("Claude API key required"))?;
    let endpoint = req
        .endpoint
        .unwrap_or("https://api.anthropic.com/v1/messages");

    let client = reqwest::Client::new();
    let mut request = client
        .post(endpoint)
        .header("anthropic-version", "2023-06-01")
        .header("content-type", "application/json")
        .json(&serde_json::json!({
            "model": req.model,
            "max_tokens": req.max_tokens,
            "messages": [{"role": "user", "content": req.prompt}]
        }))
        .timeout(std::time::Duration::from_secs(req.timeout_secs));

    request = match req.auth_scheme {
        Some("bearer") => request.header("Authorization", format!("Bearer {api_key}")),
        _ => request.header("x-api-key", api_key),
    };

    let resp = request.send().await?;

    if !resp.status().is_success() {
        let status = resp.status();
        let _ = resp.text().await;
        anyhow::bail!("Claude API error {status}");
    }

    let body: serde_json::Value = resp.json().await?;
    let text = body["content"][0]["text"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("unexpected Claude response format"))?;

    Ok(text.to_string())
}

async fn call_openai(req: &LlmRequest<'_>) -> anyhow::Result<String> {
    let api_key = req
        .api_key
        .ok_or_else(|| anyhow::anyhow!("OpenAI API key required"))?;
    let endpoint = req
        .endpoint
        .unwrap_or("https://api.openai.com/v1/chat/completions");

    let client = reqwest::Client::new();
    let resp = client
        .post(endpoint)
        .header("Authorization", format!("Bearer {api_key}"))
        .header("content-type", "application/json")
        .json(&serde_json::json!({
            "model": req.model,
            "max_tokens": req.max_tokens,
            "messages": [{"role": "user", "content": req.prompt}]
        }))
        .timeout(std::time::Duration::from_secs(req.timeout_secs))
        .send()
        .await?;

    if !resp.status().is_success() {
        let status = resp.status();
        let _ = resp.text().await;
        anyhow::bail!("OpenAI API error {status}");
    }

    let body: serde_json::Value = resp.json().await?;
    let text = body["choices"][0]["message"]["content"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("unexpected OpenAI response format"))?;

    Ok(text.to_string())
}

async fn call_ollama(req: &LlmRequest<'_>) -> anyhow::Result<String> {
    let endpoint = req
        .endpoint
        .unwrap_or("http://localhost:11434/api/generate");

    let client = reqwest::Client::new();
    let resp = client
        .post(endpoint)
        .header("content-type", "application/json")
        .json(&serde_json::json!({
            "model": req.model,
            "prompt": req.prompt,
            "stream": false
        }))
        .timeout(std::time::Duration::from_secs(req.timeout_secs))
        .send()
        .await?;

    if !resp.status().is_success() {
        let status = resp.status();
        let _ = resp.text().await;
        anyhow::bail!("Ollama API error {status}");
    }

    let body: serde_json::Value = resp.json().await?;
    let text = body["response"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("unexpected Ollama response format"))?;

    Ok(text.to_string())
}

/// Extract the first JSON array from a string (handles LLM wrapping text).
pub fn extract_json_array(text: &str) -> Option<&str> {
    let start = text.find('[')?;
    let mut depth = 0;
    for (i, ch) in text[start..].char_indices() {
        match ch {
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&text[start..start + i + 1]);
                }
            }
            _ => {}
        }
    }
    None
}

/// Hash a conversation messages array for delta-turn detection.
/// Returns a hex-encoded SHA-256 hash of the serialized messages.
pub fn hash_messages(messages: &[serde_json::Value]) -> String {
    use sha2::{Digest, Sha256};
    let serialized = serde_json::to_string(messages).unwrap_or_default();
    let mut hasher = Sha256::new();
    hasher.update(serialized.as_bytes());
    hex::encode(hasher.finalize())
}

/// Detect which messages are new (delta) vs previously seen.
/// Compares the current messages array against a hash of the previous messages.
/// Returns the index from which new messages start.
/// Used for extracting facts only from new turns in multi-turn conversations.
#[allow(dead_code)] // Planned: use in extract_and_store_facts for per-turn extraction
pub fn detect_delta_turns(messages: &[serde_json::Value], previous_hash: Option<&str>) -> usize {
    match previous_hash {
        None => 0, // all messages are new
        Some(prev) => {
            for i in (0..messages.len()).rev() {
                let prefix_hash = hash_messages(&messages[..=i]);
                if prefix_hash == prev {
                    return i + 1;
                }
            }
            0 // no match found, treat all as new
        }
    }
}

/// Known model context sizes (tokens). Used for token budget calculation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelContextSizes {
    #[serde(flatten)]
    pub sizes: std::collections::HashMap<String, u64>,
}

impl Default for ModelContextSizes {
    fn default() -> Self {
        let mut sizes = std::collections::HashMap::new();
        // OpenAI models
        sizes.insert("gpt-4o".into(), 128_000);
        sizes.insert("gpt-4o-mini".into(), 128_000);
        sizes.insert("gpt-4-turbo".into(), 128_000);
        sizes.insert("gpt-4".into(), 8_192);
        sizes.insert("gpt-3.5-turbo".into(), 16_385);
        sizes.insert("o1".into(), 200_000);
        sizes.insert("o1-mini".into(), 128_000);
        sizes.insert("o3".into(), 200_000);
        sizes.insert("o3-mini".into(), 200_000);
        sizes.insert("o4-mini".into(), 200_000);
        // Anthropic models
        sizes.insert("claude-opus-4-6".into(), 200_000);
        sizes.insert("claude-sonnet-4-6".into(), 200_000);
        sizes.insert("claude-haiku-4-5-20251001".into(), 200_000);
        sizes.insert("claude-3-5-sonnet-20241022".into(), 200_000);
        sizes.insert("claude-3-haiku-20240307".into(), 200_000);
        // Open-source / Ollama
        sizes.insert("llama3".into(), 8_192);
        sizes.insert("llama3.1".into(), 128_000);
        sizes.insert("mistral".into(), 32_000);
        sizes.insert("mixtral".into(), 32_000);
        sizes.insert("gemma2".into(), 8_192);
        Self { sizes }
    }
}

impl ModelContextSizes {
    /// Look up context size for a model. Falls back to 8192 if unknown.
    pub fn get(&self, model: &str) -> u64 {
        // Try exact match first, then prefix match
        if let Some(&size) = self.sizes.get(model) {
            return size;
        }
        // Prefix matching for versioned model names (e.g. "gpt-4o-2024-08-06")
        for (key, &size) in &self.sizes {
            if model.starts_with(key) {
                return size;
            }
        }
        8_192 // conservative fallback
    }
}
