// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 memoryOSS Contributors

//! Full Bayesian Trust Scoring for memories.
//!
//! Combines multiple signals into a trust score with confidence intervals:
//! - Recency decay (exponential, configurable half-life)
//! - Source reputation (per-API-key track record)
//! - Embedding anomaly detection (cosine distance to cluster centroid)
//! - Access frequency (more-accessed memories earn trust)
//!
//! Also provides:
//! - Semantic deduplication (embedding cosine similarity, not just hash)
//! - IP allowlisting per namespace

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::RwLock;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::memory::Memory;
use serde::{Deserialize, Serialize};

// ── Bayesian Trust Scorer ──────────────────────────────────────────────

/// Bayesian trust score with confidence interval.
#[derive(Debug, Clone, Serialize)]
pub struct TrustResult {
    /// Combined trust score [0.0, 1.0].
    pub score: f64,
    /// Lower bound of 95% confidence interval.
    pub confidence_low: f64,
    /// Upper bound of 95% confidence interval.
    pub confidence_high: f64,
    /// Whether flagged as low-trust (score < threshold).
    pub low_trust: bool,
    /// Individual signal contributions.
    pub signals: TrustSignals,
}

/// Individual trust signals before combination.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrustSignals {
    pub recency: f64,
    pub source_reputation: f64,
    pub embedding_coherence: f64,
    pub access_frequency: f64,
    pub outcome_learning: f64,
}

/// Per-source (API key) reputation tracker using Beta distribution.
/// Alpha = successes (valid memories), Beta = failures (anomalies).
#[derive(Debug, Clone)]
struct SourceReputation {
    alpha: f64, // successes
    beta: f64,  // failures
}

impl Default for SourceReputation {
    fn default() -> Self {
        // Prior: Beta(2, 1) — slightly trusting by default
        Self {
            alpha: 2.0,
            beta: 1.0,
        }
    }
}

impl SourceReputation {
    fn mean(&self) -> f64 {
        self.alpha / (self.alpha + self.beta)
    }

    /// 95% confidence interval using Beta distribution normal approximation.
    fn confidence_interval(&self) -> (f64, f64) {
        let n = self.alpha + self.beta;
        let p = self.mean();
        let std_err = (p * (1.0 - p) / n).sqrt();
        let low = (p - 1.96 * std_err).max(0.0);
        let high = (p + 1.96 * std_err).min(1.0);
        (low, high)
    }

    fn record_success(&mut self) {
        self.alpha += 1.0;
    }

    fn record_anomaly(&mut self) {
        self.beta += 1.0;
    }
}

pub struct TrustScorer {
    /// Per-source reputation: source_key → Beta distribution params.
    source_reputations: RwLock<HashMap<String, SourceReputation>>,
    /// Per-memory access count for frequency scoring.
    access_counts: RwLock<HashMap<uuid::Uuid, u64>>,
    /// Per-namespace embedding centroid (mean vector) for anomaly detection.
    centroids: RwLock<HashMap<String, Vec<f64>>>,
    /// Per-namespace centroid sample count.
    centroid_counts: RwLock<HashMap<String, u64>>,
    /// IP allowlists per namespace.
    ip_allowlists: RwLock<HashMap<String, Vec<IpAddr>>>,
    /// Trust threshold below which memories are flagged (stored as AtomicU64 bits for lock-free access).
    threshold_bits: AtomicU64,
}

impl TrustScorer {
    pub fn new(threshold: f64) -> Self {
        Self {
            source_reputations: RwLock::new(HashMap::new()),
            access_counts: RwLock::new(HashMap::new()),
            centroids: RwLock::new(HashMap::new()),
            centroid_counts: RwLock::new(HashMap::new()),
            ip_allowlists: RwLock::new(HashMap::new()),
            threshold_bits: AtomicU64::new(threshold.to_bits()),
        }
    }

    /// Get the current trust threshold.
    pub fn threshold(&self) -> f64 {
        f64::from_bits(self.threshold_bits.load(Ordering::Relaxed))
    }

    /// Update the trust threshold (for config hot-reload).
    pub fn set_threshold(&self, threshold: f64) {
        self.threshold_bits
            .store(threshold.to_bits(), Ordering::Relaxed);
    }

    /// Compute full Bayesian trust score for a memory.
    pub fn score_memory(&self, memory: &Memory, namespace: &str) -> TrustResult {
        // Signal 1: Recency decay (exponential, 7-day half-life)
        let age_hours = (chrono::Utc::now() - memory.lifecycle_anchor())
            .num_hours()
            .max(0) as f64;
        let half_life_hours = 7.0 * 24.0;
        let recency = (-0.693 * age_hours / half_life_hours).exp();

        // Signal 2: Source reputation (Beta distribution mean)
        let source_reputation = memory
            .source_key
            .as_deref()
            .and_then(|key| {
                self.source_reputations
                    .read()
                    .ok()
                    .and_then(|r| r.get(key).map(|sr| sr.mean()))
            })
            .unwrap_or(0.5); // Unknown source = neutral

        // Signal 3: Embedding coherence (cosine sim to namespace centroid)
        let embedding_coherence = memory
            .embedding
            .as_deref()
            .and_then(|emb| self.embedding_coherence(emb, namespace))
            .unwrap_or(0.5); // No embedding = neutral

        // Signal 4: Access frequency (log-scaled)
        let access_count = self
            .access_counts
            .read()
            .ok()
            .and_then(|ac| ac.get(&memory.id).copied())
            .unwrap_or(0);
        let access_frequency = (1.0 + access_count as f64).ln() / (1.0 + 100.0_f64).ln();
        let access_frequency = access_frequency.min(1.0);

        // Signal 5: Outcome learning from reuse/feedback counters.
        let outcome_learning = memory.outcome_signal();

        // Bayesian combination: weighted product
        let weights = [0.22, 0.22, 0.18, 0.13, 0.25];
        let signals = [
            recency,
            source_reputation,
            embedding_coherence,
            access_frequency,
            outcome_learning,
        ];
        let score: f64 = weights.iter().zip(signals.iter()).map(|(w, s)| w * s).sum();
        let score = score.clamp(0.0, 1.0);

        // Confidence interval from source reputation
        let (ci_low, ci_high) = memory
            .source_key
            .as_deref()
            .and_then(|key| {
                self.source_reputations
                    .read()
                    .ok()
                    .and_then(|r| r.get(key).map(|sr| sr.confidence_interval()))
            })
            .unwrap_or((0.2, 0.8));

        // Scale CI by overall score
        let confidence_low = (score * ci_low / source_reputation.max(0.01)).clamp(0.0, score);
        let confidence_high = (score * ci_high / source_reputation.max(0.01)).clamp(score, 1.0);

        TrustResult {
            score,
            confidence_low,
            confidence_high,
            low_trust: score < self.threshold(),
            signals: TrustSignals {
                recency,
                source_reputation,
                embedding_coherence,
                access_frequency,
                outcome_learning,
            },
        }
    }

    /// Compute cosine similarity between embedding and namespace centroid.
    fn embedding_coherence(&self, embedding: &[f32], namespace: &str) -> Option<f64> {
        let centroids = self.centroids.read().ok()?;
        let centroid = centroids.get(namespace)?;
        if centroid.len() != embedding.len() {
            return None;
        }

        let dot: f64 = embedding
            .iter()
            .zip(centroid.iter())
            .map(|(a, b)| *a as f64 * b)
            .sum();
        let norm_a: f64 = embedding
            .iter()
            .map(|a| (*a as f64).powi(2))
            .sum::<f64>()
            .sqrt();
        let norm_b: f64 = centroid.iter().map(|b| b.powi(2)).sum::<f64>().sqrt();

        if norm_a == 0.0 || norm_b == 0.0 {
            return None;
        }

        // Cosine similarity → [0, 1] (clamped, since embeddings can have negative sim)
        let cosine = dot / (norm_a * norm_b);
        Some(((cosine + 1.0) / 2.0).clamp(0.0, 1.0))
    }

    /// Update the namespace centroid with a new embedding (running mean).
    /// Max 10_000 namespaces to prevent unbounded memory growth.
    pub fn update_centroid(&self, embedding: &[f32], namespace: &str) {
        let mut centroids = match self.centroids.write() {
            Ok(c) => c,
            Err(_) => return,
        };
        let mut counts = match self.centroid_counts.write() {
            Ok(c) => c,
            Err(_) => return,
        };

        // Cap namespace count to prevent memory exhaustion
        if !counts.contains_key(namespace) && counts.len() >= 10_000 {
            return;
        }

        let count = counts.entry(namespace.to_string()).or_insert(0);
        *count += 1;
        let n = *count as f64;

        let centroid = centroids
            .entry(namespace.to_string())
            .or_insert_with(|| vec![0.0; embedding.len()]);

        if centroid.len() == embedding.len() {
            for (c, e) in centroid.iter_mut().zip(embedding.iter()) {
                // Running mean: new_mean = old_mean + (x - old_mean) / n
                *c += (*e as f64 - *c) / n;
            }
        }
    }

    /// Record an access event for frequency scoring.
    /// Evicts all entries if the map exceeds 100k entries to prevent unbounded memory growth.
    pub fn record_access(&self, memory_id: uuid::Uuid, _source_key: Option<&str>) {
        if let Ok(mut ac) = self.access_counts.write() {
            if ac.len() > 100_000 {
                tracing::warn!(
                    "access_counts exceeded 100k entries ({}), clearing to prevent memory exhaustion",
                    ac.len()
                );
                ac.clear();
            }
            *ac.entry(memory_id).or_insert(0) += 1;
        }
    }

    pub fn record_feedback(&self, source_key: Option<&str>, positive: bool) {
        if let Some(key) = source_key
            && let Ok(mut reps) = self.source_reputations.write()
        {
            let rep = reps
                .entry(key.to_string())
                .or_insert_with(SourceReputation::default);
            if positive {
                rep.record_success();
            } else {
                rep.record_anomaly();
            }
        }
    }

    /// Record an anomaly for a source (e.g., embedding outlier detected).
    pub fn record_anomaly(&self, source_key: &str) {
        if let Ok(mut reps) = self.source_reputations.write() {
            reps.entry(source_key.to_string())
                .or_insert_with(SourceReputation::default)
                .record_anomaly();
        }
    }

    // ── Semantic Dedup ─────────────────────────────────────────────────

    /// Check if an embedding is a near-duplicate of any existing embedding.
    /// Returns true if cosine similarity exceeds threshold (default: 0.95).
    pub fn is_semantic_duplicate(
        existing_embeddings: &[(uuid::Uuid, Vec<f32>)],
        new_embedding: &[f32],
        similarity_threshold: f64,
    ) -> Option<uuid::Uuid> {
        for (id, existing) in existing_embeddings {
            if existing.len() != new_embedding.len() {
                continue;
            }
            let sim = cosine_similarity(existing, new_embedding);
            if sim >= similarity_threshold {
                return Some(*id);
            }
        }
        None
    }

    // ── IP Allowlisting ────────────────────────────────────────────────

    /// Set the IP allowlist for a namespace. Empty list = no restriction.
    pub fn set_ip_allowlist(&self, namespace: &str, ips: Vec<IpAddr>) {
        if let Ok(mut lists) = self.ip_allowlists.write() {
            if ips.is_empty() {
                lists.remove(namespace);
            } else {
                lists.insert(namespace.to_string(), ips);
            }
        }
    }

    /// Check if an IP is allowed for a namespace.
    /// Returns true if no allowlist is set OR if the IP is in the list.
    pub fn check_ip(&self, namespace: &str, ip: &IpAddr) -> bool {
        match self.ip_allowlists.read() {
            Ok(lists) => match lists.get(namespace) {
                None => true, // No allowlist = allow all
                Some(allowed) => allowed.contains(ip),
            },
            Err(_) => {
                tracing::error!("IP allowlist lock poisoned — denying access (fail-closed)");
                false // B3 FIX: fail-closed on lock poisoning
            }
        }
    }

    /// Check if a namespace has an IP allowlist configured.
    pub fn has_ip_allowlist(&self, namespace: &str) -> bool {
        self.ip_allowlists
            .read()
            .ok()
            .map(|lists| lists.contains_key(namespace))
            .unwrap_or(true) // fail-closed: assume allowlist exists if lock poisoned
    }

    /// Get the allowlist for a namespace (for admin display).
    pub fn get_ip_allowlist(&self, namespace: &str) -> Vec<IpAddr> {
        self.ip_allowlists
            .read()
            .ok()
            .and_then(|lists| lists.get(namespace).cloned())
            .unwrap_or_default()
    }

    /// Get source reputation stats for admin display.
    pub fn source_stats(&self) -> Vec<serde_json::Value> {
        let mut result = Vec::new();
        if let Ok(reps) = self.source_reputations.read() {
            for (key, rep) in reps.iter() {
                let (ci_low, ci_high) = rep.confidence_interval();
                result.push(serde_json::json!({
                    "source_key": key,
                    "reputation": rep.mean(),
                    "confidence_low": ci_low,
                    "confidence_high": ci_high,
                    "alpha": rep.alpha,
                    "beta": rep.beta,
                }));
            }
        }
        result
    }

    /// Persist trust state (source reputations + access counts) to redb.
    pub fn persist_to_redb(&self, db: &redb::Database) -> anyhow::Result<()> {
        use redb::TableDefinition;
        const TRUST_TABLE: TableDefinition<&str, &[u8]> = TableDefinition::new("_trust_state");

        let reps: HashMap<String, (f64, f64)> = self
            .source_reputations
            .read()
            .map_err(|e| anyhow::anyhow!("lock: {e}"))?
            .iter()
            .map(|(k, v)| (k.clone(), (v.alpha, v.beta)))
            .collect();

        let access: HashMap<String, u64> = self
            .access_counts
            .read()
            .map_err(|e| anyhow::anyhow!("lock: {e}"))?
            .iter()
            .map(|(k, v)| (k.to_string(), *v))
            .collect();

        let state = serde_json::json!({
            "reputations": reps,
            "access_counts": access,
        });
        let bytes = serde_json::to_vec(&state)?;

        let tx = db.begin_write()?;
        {
            let mut table = tx.open_table(TRUST_TABLE)?;
            table.insert("state", bytes.as_slice())?;
        }
        tx.commit()?;
        tracing::info!(
            "Persisted trust state ({} sources, {} access records)",
            reps.len(),
            access.len()
        );
        Ok(())
    }

    /// Load trust state from redb. Returns true if loaded successfully.
    pub fn load_from_redb(&self, db: &redb::Database) -> bool {
        use redb::TableDefinition;
        const TRUST_TABLE: TableDefinition<&str, &[u8]> = TableDefinition::new("_trust_state");

        let tx = match db.begin_read() {
            Ok(tx) => tx,
            Err(_) => return false,
        };
        let table = match tx.open_table(TRUST_TABLE) {
            Ok(t) => t,
            Err(_) => return false,
        };
        let entry = match table.get("state") {
            Ok(Some(e)) => e,
            _ => return false,
        };
        let bytes = entry.value();
        let state: serde_json::Value = match serde_json::from_slice(bytes) {
            Ok(v) => v,
            Err(_) => return false,
        };

        // Restore reputations
        if let Some(reps) = state.get("reputations").and_then(|v| v.as_object())
            && let Ok(mut map) = self.source_reputations.write()
        {
            for (key, val) in reps {
                if let (Some(alpha), Some(beta)) = (
                    val.as_array()
                        .and_then(|a| a.first())
                        .and_then(|v| v.as_f64()),
                    val.as_array()
                        .and_then(|a| a.get(1))
                        .and_then(|v| v.as_f64()),
                ) {
                    map.insert(key.clone(), SourceReputation { alpha, beta });
                }
            }
            tracing::info!("Loaded {} source reputations from redb", map.len());
        }

        // Restore access counts
        if let Some(counts) = state.get("access_counts").and_then(|v| v.as_object())
            && let Ok(mut map) = self.access_counts.write()
        {
            for (key, val) in counts {
                if let (Ok(uuid), Some(count)) = (key.parse::<uuid::Uuid>(), val.as_u64()) {
                    map.insert(uuid, count);
                }
            }
            tracing::info!("Loaded {} access counts from redb", map.len());
        }

        true
    }

    /// Returns true if there's any trust state worth persisting.
    pub fn has_state(&self) -> bool {
        let reps = self
            .source_reputations
            .read()
            .map(|r| !r.is_empty())
            .unwrap_or(false);
        let access = self
            .access_counts
            .read()
            .map(|a| !a.is_empty())
            .unwrap_or(false);
        reps || access
    }
}

/// Cosine similarity between two f32 vectors.
fn cosine_similarity(a: &[f32], b: &[f32]) -> f64 {
    let dot: f64 = a
        .iter()
        .zip(b.iter())
        .map(|(x, y)| *x as f64 * *y as f64)
        .sum();
    let norm_a: f64 = a.iter().map(|x| (*x as f64).powi(2)).sum::<f64>().sqrt();
    let norm_b: f64 = b.iter().map(|x| (*x as f64).powi(2)).sum::<f64>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }
    dot / (norm_a * norm_b)
}

// ── Config ─────────────────────────────────────────────────────────────

/// Trust scoring configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrustConfig {
    /// Trust threshold below which memories are flagged (default: 0.3).
    #[serde(default = "default_trust_threshold")]
    pub threshold: f64,
    /// Semantic dedup similarity threshold (default: 0.95).
    #[serde(default = "default_semantic_dedup_threshold")]
    pub semantic_dedup_threshold: f64,
    /// Per-namespace IP allowlists.
    #[serde(default)]
    pub ip_allowlists: HashMap<String, Vec<String>>,
}

impl Default for TrustConfig {
    fn default() -> Self {
        Self {
            threshold: default_trust_threshold(),
            semantic_dedup_threshold: default_semantic_dedup_threshold(),
            ip_allowlists: HashMap::new(),
        }
    }
}

fn default_trust_threshold() -> f64 {
    0.3
}
fn default_semantic_dedup_threshold() -> f64 {
    0.95
}
