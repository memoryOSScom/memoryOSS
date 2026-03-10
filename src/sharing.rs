// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 memoryOSS Contributors

//! Multi-Agent Sharing — Full Implementation
//!
//! Shared namespaces with ACL, cross-namespace read via scoped tokens,
//! and optional webhook propagation on new shared memories.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::RwLock;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use url::Url;
use uuid::Uuid;

/// Validate that a webhook URL is safe (no SSRF to internal networks unless explicitly allowed).
fn is_safe_url(url_str: &str, allow_private: bool) -> bool {
    let url = match Url::parse(url_str) {
        Ok(u) => u,
        Err(_) => return false,
    };

    // Must be http or https
    match url.scheme() {
        "http" | "https" => {}
        _ => return false,
    }

    let host = match url.host_str() {
        Some(h) => h,
        None => return false,
    };

    // Block localhost variants unless explicitly allowed for local/testing setups.
    if !allow_private && (host == "localhost" || host == "[::1]") {
        return false;
    }

    // Parse as IP and block private/link-local ranges
    if let Ok(ip) = host.parse::<IpAddr>() {
        match ip {
            IpAddr::V4(v4) => {
                if !allow_private
                    && (v4.is_loopback()   // 127.0.0.0/8
                    || v4.is_private()      // 10.0.0.0/8, 172.16.0.0/12, 192.168.0.0/16
                    || v4.is_link_local()   // 169.254.0.0/16
                    || v4.is_unspecified()  // 0.0.0.0
                    || v4.is_broadcast())
                {
                    return false;
                }
            }
            IpAddr::V6(v6) => {
                if !allow_private && (v6.is_loopback() || v6.is_unspecified()) {
                    return false;
                }
            }
        }
    }

    true
}

// ── ACL Model ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SharePermission {
    Read,
    Write,
    Admin,
}

impl SharePermission {
    fn level(self) -> u8 {
        match self {
            Self::Read => 1,
            Self::Write => 2,
            Self::Admin => 3,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShareGrant {
    pub id: Uuid,
    pub grantee_namespace: String,
    pub permission: SharePermission,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag_filter: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_filter: Option<Vec<String>>,
    pub created_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SharedNamespace {
    pub name: String,
    pub owner_namespace: String,
    pub grants: Vec<ShareGrant>,
    pub created_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub webhook_url: Option<String>,
}

// ── In-Memory SharingManager ───────────────────────────────────────────

pub struct SharingStore {
    namespaces: RwLock<HashMap<String, SharedNamespace>>,
    config: SharingConfig,
    http_client: reqwest::Client,
}

impl SharingStore {
    pub fn new(config: SharingConfig) -> Self {
        let http_client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .expect("failed to build HTTP client for SharingStore");
        Self {
            namespaces: RwLock::new(HashMap::new()),
            config,
            http_client,
        }
    }

    pub fn create_shared_namespace(
        &self,
        name: &str,
        owner_namespace: &str,
        webhook_url: Option<&str>,
    ) -> anyhow::Result<SharedNamespace> {
        let mut ns_map = self
            .namespaces
            .write()
            .map_err(|_| anyhow::anyhow!("lock poisoned"))?;

        if ns_map.contains_key(name) {
            anyhow::bail!("shared namespace '{}' already exists", name);
        }

        // Check per-owner limit
        let owner_count = ns_map
            .values()
            .filter(|ns| ns.owner_namespace == owner_namespace)
            .count();
        if owner_count >= self.config.max_shared_namespaces {
            anyhow::bail!(
                "owner '{}' has reached the maximum of {} shared namespaces",
                owner_namespace,
                self.config.max_shared_namespaces
            );
        }

        let shared_ns = SharedNamespace {
            name: name.to_string(),
            owner_namespace: owner_namespace.to_string(),
            grants: Vec::new(),
            created_at: Utc::now(),
            webhook_url: webhook_url.map(|s| s.to_string()),
        };

        ns_map.insert(name.to_string(), shared_ns.clone());
        tracing::info!(name, owner_namespace, "Created shared namespace");
        Ok(shared_ns)
    }

    pub fn delete_shared_namespace(
        &self,
        name: &str,
        requesting_namespace: &str,
    ) -> anyhow::Result<()> {
        let mut ns_map = self
            .namespaces
            .write()
            .map_err(|_| anyhow::anyhow!("lock poisoned"))?;

        let ns = ns_map
            .get(name)
            .ok_or_else(|| anyhow::anyhow!("shared namespace '{}' not found", name))?;

        if ns.owner_namespace != requesting_namespace {
            anyhow::bail!(
                "only owner '{}' can delete this shared namespace",
                ns.owner_namespace
            );
        }

        ns_map.remove(name);
        tracing::info!(name, "Deleted shared namespace");
        Ok(())
    }

    pub fn add_grant(
        &self,
        shared_ns: &str,
        grantee_namespace: &str,
        permission: SharePermission,
        tag_filter: Option<Vec<String>>,
        agent_filter: Option<Vec<String>>,
        expires_at: Option<DateTime<Utc>>,
        requesting_namespace: &str,
    ) -> anyhow::Result<ShareGrant> {
        let mut ns_map = self
            .namespaces
            .write()
            .map_err(|_| anyhow::anyhow!("lock poisoned"))?;

        let ns = ns_map
            .get_mut(shared_ns)
            .ok_or_else(|| anyhow::anyhow!("shared namespace '{}' not found", shared_ns))?;

        // Only owner or admin grantees can add grants
        if ns.owner_namespace != requesting_namespace {
            let has_admin = ns.grants.iter().any(|g| {
                g.grantee_namespace == requesting_namespace
                    && g.permission == SharePermission::Admin
                    && !is_expired(g)
            });
            if !has_admin {
                anyhow::bail!("insufficient permission to add grants");
            }
        }

        if ns.grants.len() >= self.config.max_grants {
            anyhow::bail!("maximum grants ({}) reached", self.config.max_grants);
        }

        let grant = ShareGrant {
            id: Uuid::now_v7(),
            grantee_namespace: grantee_namespace.to_string(),
            permission,
            tag_filter,
            agent_filter,
            created_at: Utc::now(),
            expires_at,
        };

        ns.grants.push(grant.clone());
        tracing::info!(shared_ns, grantee_namespace, ?permission, "Added grant");
        Ok(grant)
    }

    pub fn remove_grant(
        &self,
        shared_ns: &str,
        grant_id: Uuid,
        requesting_namespace: &str,
    ) -> anyhow::Result<()> {
        let mut ns_map = self
            .namespaces
            .write()
            .map_err(|_| anyhow::anyhow!("lock poisoned"))?;

        let ns = ns_map
            .get_mut(shared_ns)
            .ok_or_else(|| anyhow::anyhow!("shared namespace '{}' not found", shared_ns))?;

        if ns.owner_namespace != requesting_namespace {
            anyhow::bail!("only owner can remove grants");
        }

        let before = ns.grants.len();
        ns.grants.retain(|g| g.id != grant_id);
        if ns.grants.len() == before {
            anyhow::bail!("grant not found");
        }

        Ok(())
    }

    pub fn list_grants(&self, shared_ns: &str) -> anyhow::Result<Vec<ShareGrant>> {
        let ns_map = self
            .namespaces
            .read()
            .map_err(|_| anyhow::anyhow!("lock poisoned"))?;
        let ns = ns_map
            .get(shared_ns)
            .ok_or_else(|| anyhow::anyhow!("shared namespace '{}' not found", shared_ns))?;
        Ok(ns.grants.clone())
    }

    pub fn check_permission(
        &self,
        shared_ns: &str,
        namespace: &str,
        required: SharePermission,
    ) -> anyhow::Result<bool> {
        let ns_map = self
            .namespaces
            .read()
            .map_err(|_| anyhow::anyhow!("lock poisoned"))?;

        let ns = match ns_map.get(shared_ns) {
            Some(ns) => ns,
            None => return Ok(false),
        };

        // Owner always has full access
        if ns.owner_namespace == namespace {
            return Ok(true);
        }

        Ok(ns.grants.iter().any(|g| {
            g.grantee_namespace == namespace
                && g.permission.level() >= required.level()
                && !is_expired(g)
        }))
    }

    pub fn accessible_namespaces(&self, namespace: &str) -> anyhow::Result<Vec<String>> {
        let ns_map = self
            .namespaces
            .read()
            .map_err(|_| anyhow::anyhow!("lock poisoned"))?;

        let mut result = Vec::new();
        for (name, ns) in ns_map.iter() {
            if ns.owner_namespace == namespace {
                result.push(name.clone());
                continue;
            }
            if ns
                .grants
                .iter()
                .any(|g| g.grantee_namespace == namespace && !is_expired(g))
            {
                result.push(name.clone());
            }
        }
        Ok(result)
    }

    pub fn get_shared_namespace(&self, name: &str) -> anyhow::Result<Option<SharedNamespace>> {
        let ns_map = self
            .namespaces
            .read()
            .map_err(|_| anyhow::anyhow!("lock poisoned"))?;
        Ok(ns_map.get(name).cloned())
    }

    pub fn list_all(&self) -> anyhow::Result<Vec<SharedNamespace>> {
        let ns_map = self
            .namespaces
            .read()
            .map_err(|_| anyhow::anyhow!("lock poisoned"))?;
        Ok(ns_map.values().cloned().collect())
    }

    /// Fire webhook for a shared namespace (async, non-blocking).
    pub fn fire_webhook(&self, shared_ns: &str, memory_id: Uuid) {
        let ns_map = match self.namespaces.read() {
            Ok(m) => m,
            Err(_) => return,
        };

        let webhook_url = match ns_map.get(shared_ns) {
            Some(ns) => match &ns.webhook_url {
                Some(url) => url.clone(),
                None => return,
            },
            None => return,
        };

        // H7: SSRF validation — block internal/private network targets
        if !is_safe_url(&webhook_url, self.config.allow_private_webhooks) {
            tracing::warn!(
                shared_ns,
                webhook_url,
                "Webhook URL blocked by SSRF validation (private/internal address)"
            );
            return;
        }

        let ns_name = shared_ns.to_string();
        let client = self.http_client.clone();
        tokio::spawn(async move {
            let result = client
                .post(&webhook_url)
                .json(&serde_json::json!({
                    "event": "memory_stored",
                    "shared_namespace": ns_name,
                    "memory_id": memory_id,
                    "timestamp": Utc::now().to_rfc3339(),
                }))
                .timeout(std::time::Duration::from_secs(10))
                .send()
                .await;

            match result {
                Ok(resp) if resp.status().is_success() => {
                    tracing::debug!(shared_ns = ns_name, "Webhook delivered");
                }
                Ok(resp) => {
                    tracing::warn!(
                        shared_ns = ns_name,
                        status = %resp.status(),
                        "Webhook returned non-success"
                    );
                }
                Err(e) => {
                    tracing::warn!(shared_ns = ns_name, error = %e, "Webhook delivery failed");
                }
            }
        });
    }
}

fn is_expired(grant: &ShareGrant) -> bool {
    match grant.expires_at {
        Some(exp) => Utc::now() > exp,
        None => false,
    }
}

// ── Config ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SharingConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub allow_private_webhooks: bool,
    #[serde(default = "default_max_grants")]
    pub max_grants: usize,
    #[serde(default = "default_max_shared_ns")]
    pub max_shared_namespaces: usize,
}

impl Default for SharingConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            allow_private_webhooks: false,
            max_grants: default_max_grants(),
            max_shared_namespaces: default_max_shared_ns(),
        }
    }
}

fn default_max_grants() -> usize {
    100
}
fn default_max_shared_ns() -> usize {
    10
}
