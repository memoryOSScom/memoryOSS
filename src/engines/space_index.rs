// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 memoryOSS Contributors

use std::collections::HashMap;
use std::sync::RwLock;

use chrono::{DateTime, Utc};
use serde::Serialize;
use uuid::Uuid;

use crate::memory::Memory;

/// Per-agent metadata stats.
#[derive(Debug, Clone, Serialize, Default)]
pub struct AgentStats {
    pub count: u64,
    pub total_bytes: u64,
    pub topics: Vec<String>,
    pub earliest: Option<DateTime<Utc>>,
    pub latest: Option<DateTime<Utc>>,
}

/// Lightweight metadata for peek() — no content or embedding loaded.
#[derive(Debug, Clone, Serialize)]
pub struct MemoryMeta {
    pub id: Uuid,
    pub agent: Option<String>,
    pub session: Option<String>,
    pub namespace: String,
    pub tags: Vec<String>,
    pub memory_type: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub content_bytes: usize,
}

impl From<&Memory> for MemoryMeta {
    fn from(m: &Memory) -> Self {
        Self {
            id: m.id,
            agent: m.agent.clone(),
            session: m.session.clone(),
            namespace: m.namespace.as_deref().unwrap_or("default").to_string(),
            tags: m.tags.clone(),
            memory_type: m.memory_type.to_string(),
            created_at: m.created_at,
            updated_at: m.updated_at,
            content_bytes: m.content.len(),
        }
    }
}

/// In-memory metadata index: per-agent stats + lightweight peek data.
/// Updated incrementally on store/update/forget.
pub struct SpaceIndex {
    /// agent_name -> stats
    agents: RwLock<HashMap<String, AgentStats>>,
    /// memory_id -> lightweight metadata (no content/embedding)
    metas: RwLock<HashMap<Uuid, MemoryMeta>>,
}

impl SpaceIndex {
    pub fn new() -> Self {
        Self {
            agents: RwLock::new(HashMap::new()),
            metas: RwLock::new(HashMap::new()),
        }
    }

    /// Rebuild from all memories (startup).
    pub fn rebuild(&self, memories: &[Memory]) {
        let Ok(mut agents) = self.agents.write() else {
            return;
        };
        let Ok(mut metas) = self.metas.write() else {
            return;
        };
        agents.clear();
        metas.clear();

        for m in memories {
            let agent_key = m.agent.clone().unwrap_or_else(|| "_none".to_string());
            let stats = agents.entry(agent_key).or_default();
            stats.count += 1;
            stats.total_bytes += m.content.len() as u64;

            // Track unique topics from tags
            for tag in &m.tags {
                if !stats.topics.contains(tag) {
                    stats.topics.push(tag.clone());
                }
            }

            // Time range
            match stats.earliest {
                Some(e) if m.created_at < e => stats.earliest = Some(m.created_at),
                None => stats.earliest = Some(m.created_at),
                _ => {}
            }
            match stats.latest {
                Some(l) if m.created_at > l => stats.latest = Some(m.created_at),
                None => stats.latest = Some(m.created_at),
                _ => {}
            }

            metas.insert(m.id, MemoryMeta::from(m));
        }

        tracing::info!(
            "Space index rebuilt: {} agents, {} memories",
            agents.len(),
            metas.len()
        );
    }

    /// Add a memory to the index (on store).
    pub fn add(&self, memory: &Memory) {
        let agent_key = memory.agent.clone().unwrap_or_else(|| "_none".to_string());

        let Ok(mut agents) = self.agents.write() else {
            return;
        };
        let stats = agents.entry(agent_key).or_default();
        stats.count += 1;
        stats.total_bytes += memory.content.len() as u64;
        for tag in &memory.tags {
            if !stats.topics.contains(tag) {
                stats.topics.push(tag.clone());
            }
        }
        match stats.earliest {
            Some(e) if memory.created_at < e => stats.earliest = Some(memory.created_at),
            None => stats.earliest = Some(memory.created_at),
            _ => {}
        }
        match stats.latest {
            Some(l) if memory.created_at > l => stats.latest = Some(memory.created_at),
            None => stats.latest = Some(memory.created_at),
            _ => {}
        }

        if let Ok(mut metas) = self.metas.write() {
            metas.insert(memory.id, MemoryMeta::from(memory));
        }
    }

    /// Remove a memory from the index (on forget).
    pub fn remove(&self, id: Uuid) {
        let meta = self.metas.write().ok().and_then(|mut m| m.remove(&id));
        if let Some(meta) = meta {
            let agent_key = meta.agent.unwrap_or_else(|| "_none".to_string());
            let Ok(mut agents) = self.agents.write() else {
                return;
            };
            if let Some(stats) = agents.get_mut(&agent_key) {
                stats.count = stats.count.saturating_sub(1);
                stats.total_bytes = stats.total_bytes.saturating_sub(meta.content_bytes as u64);
                if stats.count == 0 {
                    agents.remove(&agent_key);
                }
            }
        }
    }

    /// Update metadata for a memory.
    pub fn update(&self, memory: &Memory) {
        // Remove old, add new
        self.remove(memory.id);
        self.add(memory);
    }

    /// Peek: get metadata for a memory without loading content.
    pub fn peek(&self, id: Uuid) -> Option<MemoryMeta> {
        self.metas.read().ok()?.get(&id).cloned()
    }

    /// Get per-agent stats.
    pub fn agent_stats(&self) -> HashMap<String, AgentStats> {
        self.agents
            .read()
            .ok()
            .map(|a| a.clone())
            .unwrap_or_default()
    }

    /// Get global stats.
    pub fn global_stats(&self) -> (u64, u64) {
        let Ok(agents) = self.agents.read() else {
            return (0, 0);
        };
        let count: u64 = agents.values().map(|s| s.count).sum();
        let bytes: u64 = agents.values().map(|s| s.total_bytes).sum();
        (count, bytes)
    }
}
