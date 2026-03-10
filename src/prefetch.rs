// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 memoryOSS Contributors

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::embedding::EmbeddingEngine;

/// Max recent queries tracked per agent.
const MAX_QUERIES_PER_AGENT: usize = 20;
/// How many top queries to pre-warm on session start.
const PREFETCH_TOP_N: usize = 5;

/// Tracks recent queries per agent and pre-warms embedding cache for new sessions.
pub struct SessionPrefetcher {
    /// agent_id -> ring buffer of recent queries
    agent_queries: Mutex<HashMap<String, VecDeque<String>>>,
    /// session_id -> bool (tracks whether we already prefetched for this session)
    seen_sessions: Mutex<HashMap<String, bool>>,
}

impl SessionPrefetcher {
    pub fn new() -> Self {
        Self {
            agent_queries: Mutex::new(HashMap::new()),
            seen_sessions: Mutex::new(HashMap::new()),
        }
    }

    /// Record a query for an agent. Called from recall handler.
    pub async fn record_query(&self, agent: &str, query: &str) {
        let mut map = self.agent_queries.lock().await;
        let buf = map
            .entry(agent.to_string())
            .or_insert_with(|| VecDeque::with_capacity(MAX_QUERIES_PER_AGENT + 1));
        // Avoid recording duplicate consecutive queries
        if buf.back().map(|s| s.as_str()) == Some(query) {
            return;
        }
        buf.push_back(query.to_string());
        if buf.len() > MAX_QUERIES_PER_AGENT {
            buf.pop_front();
        }
    }

    /// Check if this is a new session for the agent, and if so, trigger prefetch.
    /// Returns true if prefetch was triggered.
    pub async fn maybe_prefetch(
        &self,
        agent: &str,
        session: &str,
        embedding: &Arc<EmbeddingEngine>,
    ) -> bool {
        // Check if we already prefetched for this session
        {
            let mut seen = self.seen_sessions.lock().await;
            if seen.contains_key(session) {
                return false;
            }
            seen.insert(session.to_string(), true);
            // Evict old sessions (keep last 1000)
            if seen.len() > 1000 {
                let keys: Vec<String> = seen.keys().take(100).cloned().collect();
                for k in keys {
                    seen.remove(&k);
                }
            }
        }

        // Get top queries for this agent
        let queries = {
            let map = self.agent_queries.lock().await;
            match map.get(agent) {
                Some(buf) => {
                    // Take most recent unique queries (deduplicated, most recent first)
                    let mut seen = std::collections::HashSet::new();
                    let mut top: Vec<String> = Vec::new();
                    for q in buf.iter().rev() {
                        if seen.insert(q.clone()) {
                            top.push(q.clone());
                            if top.len() >= PREFETCH_TOP_N {
                                break;
                            }
                        }
                    }
                    top
                }
                None => return false,
            }
        };

        if queries.is_empty() {
            return false;
        }

        // Pre-warm embedding cache in background (fire-and-forget)
        let emb = embedding.clone();
        let agent_name = agent.to_string();
        tokio::spawn(async move {
            for query in &queries {
                if let Err(e) = emb.embed_one(query).await {
                    tracing::warn!(agent = %agent_name, "prefetch embedding failed: {e}");
                }
            }
            tracing::debug!(
                agent = %agent_name,
                count = queries.len(),
                "pre-warmed embedding cache for session"
            );
        });

        true
    }

    /// Stats: number of tracked agents and total queries.
    pub async fn stats(&self) -> (usize, usize) {
        let map = self.agent_queries.lock().await;
        let total: usize = map.values().map(|v| v.len()).sum();
        (map.len(), total)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_record_and_dedup() {
        let pf = SessionPrefetcher::new();
        pf.record_query("agent-1", "hello world").await;
        pf.record_query("agent-1", "hello world").await; // duplicate
        pf.record_query("agent-1", "another query").await;

        let (agents, total) = pf.stats().await;
        assert_eq!(agents, 1);
        assert_eq!(total, 2); // deduped consecutive
    }

    #[tokio::test]
    async fn test_ring_buffer_eviction() {
        let pf = SessionPrefetcher::new();
        for i in 0..25 {
            pf.record_query("agent-1", &format!("query {i}")).await;
        }
        let (_, total) = pf.stats().await;
        assert_eq!(total, MAX_QUERIES_PER_AGENT);
    }

    #[tokio::test]
    async fn test_session_seen_tracking() {
        let pf = SessionPrefetcher::new();
        // No queries recorded yet — should not prefetch
        let mut seen = pf.seen_sessions.lock().await;
        assert!(!seen.contains_key("session-1"));
        seen.insert("session-1".to_string(), true);
        assert!(seen.contains_key("session-1"));
    }
}
