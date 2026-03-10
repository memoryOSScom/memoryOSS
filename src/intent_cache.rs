// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 memoryOSS Contributors

//! Intent Canonicalization Cache
//!
//! Normalizes recall queries to a canonical form (stopword removal, term sorting)
//! and caches full recall results. Keyed by (session, namespace, canonical_intent).
//! TTL configurable (default 60s), per agent session.
//!
//! Paper #23: intent canonicalization > embedding similarity (91.1% vs 37.9% hit rate).

use std::collections::HashMap;
use std::time::{Duration, Instant};

use tokio::sync::Mutex;

use crate::memory::ScoredMemory;

// ── Stopwords (English, compact set) ──────────────────────────────────

const STOPWORDS: &[&str] = &[
    "a", "an", "the", "is", "are", "was", "were", "be", "been", "being", "have", "has", "had",
    "do", "does", "did", "will", "would", "could", "should", "may", "might", "shall", "can",
    "need", "must", "ought", "i", "me", "my", "we", "our", "you", "your", "he", "she", "it",
    "they", "them", "their", "this", "that", "these", "those", "in", "on", "at", "to", "for", "of",
    "with", "by", "from", "about", "into", "through", "during", "before", "after", "above",
    "below", "and", "but", "or", "nor", "not", "so", "yet", "both", "either", "what", "which",
    "who", "whom", "how", "when", "where", "why", "all", "each", "every", "any", "few", "more",
    "most", "some", "no", "only", "very", "just", "also", "than", "too", "if", "then",
];

// ── Canonicalization ──────────────────────────────────────────────────

/// Normalize a query to its canonical intent form:
/// 1. Lowercase
/// 2. Remove punctuation (keep alphanumeric + spaces)
/// 3. Remove stopwords
/// 4. Sort remaining terms alphabetically
/// 5. Deduplicate
///
/// "How do I deploy the service?" → "deploy service"
/// "deployment instructions for service" → "deployment instructions service"
pub fn canonicalize(query: &str) -> String {
    let lower = query.to_lowercase();

    let cleaned: String = lower
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c.is_whitespace() {
                c
            } else {
                ' '
            }
        })
        .collect();

    let mut terms: Vec<&str> = cleaned
        .split_whitespace()
        .filter(|w| w.len() > 1 && !STOPWORDS.contains(w))
        .collect();

    terms.sort_unstable();
    terms.dedup();
    terms.join(" ")
}

/// Build a cache key from session, namespace, canonical query, and filters.
fn cache_key(
    session: Option<&str>,
    namespace: &str,
    canonical: &str,
    agent: Option<&str>,
    tags: &[String],
) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    session.unwrap_or("_global").hash(&mut hasher);
    namespace.hash(&mut hasher);
    canonical.hash(&mut hasher);
    agent.hash(&mut hasher);
    tags.hash(&mut hasher);
    hasher.finish()
}

// ── Cache entry ───────────────────────────────────────────────────────

struct CacheEntry {
    results: Vec<ScoredMemory>,
    inserted_at: Instant,
    canonical_query: String,
}

// ── IntentCache ───────────────────────────────────────────────────────

pub struct IntentCache {
    entries: Mutex<HashMap<u64, CacheEntry>>,
    ttl: Duration,
    max_entries: usize,
    hits: std::sync::atomic::AtomicU64,
}

impl IntentCache {
    pub fn new(ttl_secs: u64, max_entries: usize) -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            ttl: Duration::from_secs(ttl_secs),
            max_entries,
            hits: std::sync::atomic::AtomicU64::new(0),
        }
    }

    /// Look up cached recall results for a query.
    /// Returns Some(results) on cache hit, None on miss.
    pub async fn get(
        &self,
        query: &str,
        session: Option<&str>,
        namespace: &str,
        agent: Option<&str>,
        tags: &[String],
    ) -> Option<Vec<ScoredMemory>> {
        let canonical = canonicalize(query);
        if canonical.is_empty() {
            return None; // All-stopword queries bypass cache
        }

        let key = cache_key(session, namespace, &canonical, agent, tags);
        let mut map = self.entries.lock().await;

        if let Some(entry) = map.get(&key) {
            if entry.inserted_at.elapsed() < self.ttl {
                self.hits.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                tracing::debug!(canonical = entry.canonical_query, "intent cache HIT");
                return Some(entry.results.clone());
            }
            // Expired
            map.remove(&key);
        }
        None
    }

    /// Store recall results in the cache.
    pub async fn put(
        &self,
        query: &str,
        session: Option<&str>,
        namespace: &str,
        agent: Option<&str>,
        tags: &[String],
        results: Vec<ScoredMemory>,
    ) {
        let canonical = canonicalize(query);
        if canonical.is_empty() {
            return;
        }

        let key = cache_key(session, namespace, &canonical, agent, tags);
        let mut map = self.entries.lock().await;

        // Evict expired entries if at capacity
        if map.len() >= self.max_entries {
            let ttl = self.ttl;
            map.retain(|_, v| v.inserted_at.elapsed() < ttl);
        }

        // If still at capacity, remove oldest
        if map.len() >= self.max_entries
            && let Some(oldest_key) = map
                .iter()
                .min_by_key(|(_, v)| v.inserted_at)
                .map(|(k, _)| *k)
        {
            map.remove(&oldest_key);
        }

        map.insert(
            key,
            CacheEntry {
                results,
                inserted_at: Instant::now(),
                canonical_query: canonical,
            },
        );
    }

    /// Invalidate all cache entries for a given namespace.
    /// Called when data changes (store/forget) to prevent stale results.
    pub async fn invalidate_namespace(&self, namespace: &str) {
        let mut map = self.entries.lock().await;
        // We can't filter by namespace directly from the hash key,
        // so we clear all entries. This is safe — worst case is extra misses.
        // For a production system, we'd store namespace in the entry.
        let before = map.len();
        map.clear();
        if before > 0 {
            tracing::debug!(evicted = before, namespace, "intent cache invalidated");
        }
    }

    /// Get cache statistics: (hits_possible, total_entries).
    pub async fn stats(&self) -> IntentCacheStats {
        let map = self.entries.lock().await;
        let now = Instant::now();
        let valid = map
            .values()
            .filter(|v| now.duration_since(v.inserted_at) < self.ttl)
            .count();
        IntentCacheStats {
            valid_entries: valid,
            total_entries: map.len(),
            entries: valid,
            hits: self.hits.load(std::sync::atomic::Ordering::Relaxed),
        }
    }

    /// Flush all entries.
    pub async fn flush(&self) -> usize {
        let mut map = self.entries.lock().await;
        let count = map.len();
        map.clear();
        count
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct IntentCacheStats {
    pub valid_entries: usize,
    pub total_entries: usize,
    pub entries: usize,
    pub hits: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_canonicalize_basic() {
        assert_eq!(
            canonicalize("How do I deploy the service?"),
            "deploy service"
        );
    }

    #[test]
    fn test_canonicalize_sorts_and_deduplicates() {
        assert_eq!(
            canonicalize("service deploy deploy service"),
            "deploy service"
        );
    }

    #[test]
    fn test_canonicalize_strips_punctuation() {
        // Hyphens become spaces, splitting "deployment-status" into two terms
        assert_eq!(
            canonicalize("what's the deployment-status?"),
            "deployment status"
        );
    }

    #[test]
    fn test_canonicalize_all_stopwords() {
        assert_eq!(canonicalize("what is the"), "");
    }

    #[test]
    fn test_canonicalize_preserves_meaningful_terms() {
        let c1 = canonicalize("show me the database configuration");
        let c2 = canonicalize("show database configuration");
        assert_eq!(c1, c2);
    }

    #[tokio::test]
    async fn test_cache_hit_miss() {
        let cache = IntentCache::new(60, 100);
        let results = vec![];

        // Miss
        assert!(
            cache
                .get("deploy service", None, "ns", None, &[])
                .await
                .is_none()
        );

        // Put
        cache
            .put("deploy service", None, "ns", None, &[], results.clone())
            .await;

        // Hit (same canonical form)
        assert!(
            cache
                .get("How do I deploy the service?", None, "ns", None, &[])
                .await
                .is_some()
        );

        // Miss (different namespace)
        assert!(
            cache
                .get("deploy service", None, "other", None, &[])
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn test_cache_session_isolation() {
        let cache = IntentCache::new(60, 100);
        let results = vec![];

        cache
            .put("deploy", Some("s1"), "ns", None, &[], results.clone())
            .await;

        // Hit with same session
        assert!(
            cache
                .get("deploy", Some("s1"), "ns", None, &[])
                .await
                .is_some()
        );

        // Miss with different session
        assert!(
            cache
                .get("deploy", Some("s2"), "ns", None, &[])
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn test_cache_invalidation() {
        let cache = IntentCache::new(60, 100);
        cache.put("deploy", None, "ns", None, &[], vec![]).await;

        assert!(cache.get("deploy", None, "ns", None, &[]).await.is_some());
        cache.invalidate_namespace("ns").await;
        assert!(cache.get("deploy", None, "ns", None, &[]).await.is_none());
    }
}
