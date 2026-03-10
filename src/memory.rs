// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 memoryOSS Contributors

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum MemoryType {
    Episodic,
    #[default]
    Semantic,
    Procedural,
    Working,
}

impl std::fmt::Display for MemoryType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Episodic => write!(f, "episodic"),
            Self::Semantic => write!(f, "semantic"),
            Self::Procedural => write!(f, "procedural"),
            Self::Working => write!(f, "working"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum MemoryStatus {
    Candidate,
    #[default]
    Active,
    Contested,
    Stale,
}

impl std::fmt::Display for MemoryStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Candidate => write!(f, "candidate"),
            Self::Active => write!(f, "active"),
            Self::Contested => write!(f, "contested"),
            Self::Stale => write!(f, "stale"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Memory {
    pub id: Uuid,
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub embedding: Option<Vec<f32>>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,
    #[serde(default)]
    pub memory_type: MemoryType,
    #[serde(default)]
    pub status: MemoryStatus,
    pub version: u64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// Source provenance: opaque key ID (SHA-256 hash prefix) of the API key that created this memory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_key: Option<String>,
    /// SHA-256 content hash for dedup.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<String>,
    /// Archived memories are kept in redb but excluded from search indexes.
    #[serde(default)]
    pub archived: bool,
    /// Confidence score [0.0, 1.0] for proxy-extracted memories.
    /// None = manually stored (full confidence). 0.0 = unverified extraction.
    /// Memories with confidence < 0.5 are weighted lower in recall scoring.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f64>,
    #[serde(default = "default_evidence_count")]
    pub evidence_count: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_verified_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub superseded_by: Option<Uuid>,
}

impl Memory {
    pub fn new(content: String) -> Self {
        let hash = Self::compute_hash(&content);
        let now = Utc::now();
        Self {
            id: Uuid::now_v7(),
            content,
            embedding: None,
            tags: Vec::new(),
            agent: None,
            session: None,
            namespace: None,
            memory_type: MemoryType::default(),
            status: MemoryStatus::default(),
            version: 1,
            created_at: now,
            updated_at: now,
            source_key: None,
            content_hash: Some(hash),
            archived: false,
            confidence: None,
            evidence_count: default_evidence_count(),
            last_verified_at: Some(now),
            superseded_by: None,
        }
    }

    pub fn compute_hash(content: &str) -> String {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(content.as_bytes());
        hex::encode(hasher.finalize())
    }

    /// Recency decay: returns trust factor in [0.0, 1.0] based on age.
    /// Newer memories get higher trust. Half-life is ~7 days.
    pub fn recency_trust(&self) -> f64 {
        let age_hours = (Utc::now() - self.created_at).num_hours().max(0) as f64;
        let half_life_hours = 7.0 * 24.0; // 7 days
        (-0.693 * age_hours / half_life_hours).exp() // e^(-ln2 * t/T)
    }

    pub fn status_factor(&self) -> f64 {
        match self.status {
            MemoryStatus::Candidate => 0.75,
            MemoryStatus::Active => 1.0,
            MemoryStatus::Contested => 0.55,
            MemoryStatus::Stale => 0.7,
        }
    }

    pub fn eligible_for_injection(&self) -> bool {
        matches!(self.status, MemoryStatus::Active) && !self.archived
    }

    pub fn confirm_from_signal(&mut self) {
        let now = Utc::now();
        self.updated_at = now;
        self.evidence_count = self.evidence_count.saturating_add(1);
        self.last_verified_at = Some(now);

        if let Some(conf) = self.confidence {
            self.confidence = Some((conf + 0.35).min(1.0));
        }

        match self.status {
            MemoryStatus::Candidate | MemoryStatus::Contested => {
                if self.confidence.unwrap_or(1.0) >= 0.5 || self.evidence_count >= 1 {
                    self.status = MemoryStatus::Active;
                    self.superseded_by = None;
                }
            }
            MemoryStatus::Stale => {
                if self.superseded_by.is_none() {
                    self.status = MemoryStatus::Active;
                }
            }
            MemoryStatus::Active => {}
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoreRequest {
    pub content: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,
    #[serde(default)]
    pub memory_type: MemoryType,
    /// Zero-knowledge mode: content is client-encrypted ciphertext.
    /// Server cannot read content, skips FTS/IDF. Client must provide embedding.
    #[serde(default)]
    pub zero_knowledge: bool,
    /// Client-provided embedding vector (required in zero_knowledge mode).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embedding: Option<Vec<f32>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoreResponse {
    pub id: Uuid,
    pub version: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecallRequest {
    pub query: String,
    #[serde(default = "default_limit")]
    pub limit: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory_type: Option<MemoryType>,
    #[serde(default)]
    pub tags: Vec<String>,
    /// Consistency mode: "strong" (default) waits for indexers, "eventual" skips version-fence.
    #[serde(default)]
    pub consistency: Option<String>,
    /// Custom scoring weights per query. Defaults: vector=0.4, fts=0.4, recency=0.2.
    #[serde(default)]
    pub weights: Option<ScoringWeights>,
    /// Only return memories created before this timestamp.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<DateTime<Utc>>,
    /// Only return memories created after this timestamp.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<DateTime<Utc>>,
    /// Cursor for pagination (from previous response's `next_cursor`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    /// Client-provided query embedding for zero-knowledge recall (bypasses server-side embedding).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query_embedding: Option<Vec<f32>>,
}

impl RecallRequest {
    /// True if any metadata filter is set (agent, session, tags, memory_type, time_range).
    pub fn has_filters(&self) -> bool {
        self.agent.is_some()
            || self.session.is_some()
            || self.memory_type.is_some()
            || !self.tags.is_empty()
            || self.before.is_some()
            || self.after.is_some()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoringWeights {
    #[serde(default = "default_vector_weight")]
    pub vector: f64,
    #[serde(default = "default_fts_weight")]
    pub fts: f64,
    #[serde(default = "default_exact_weight")]
    pub exact: f64,
    #[serde(default = "default_recency_weight")]
    pub recency: f64,
}

impl ScoringWeights {
    /// Clamp all weight fields to [0.0, 1.0] to prevent malicious/accidental out-of-range values.
    pub fn clamped(self) -> Self {
        Self {
            vector: self.vector.clamp(0.0, 1.0),
            fts: self.fts.clamp(0.0, 1.0),
            exact: self.exact.clamp(0.0, 1.0),
            recency: self.recency.clamp(0.0, 1.0),
        }
    }
}

impl Default for ScoringWeights {
    fn default() -> Self {
        Self {
            vector: 0.30,
            fts: 0.30,
            exact: 0.25,
            recency: 0.15,
        }
    }
}

fn default_vector_weight() -> f64 {
    0.30
}
fn default_fts_weight() -> f64 {
    0.30
}
fn default_exact_weight() -> f64 {
    0.25
}
fn default_recency_weight() -> f64 {
    0.15
}

fn default_limit() -> usize {
    10
}

fn default_evidence_count() -> u32 {
    1
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecallResponse {
    pub memories: Vec<ScoredMemory>,
    /// Cursor for next page. None if no more results.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoredMemory {
    pub memory: Memory,
    pub score: f64,
    pub provenance: Vec<String>,
    /// Trust score [0.0, 1.0] — based on recency decay and source provenance.
    #[serde(default)]
    pub trust_score: f64,
    /// Flagged as low-trust (not rejected, just flagged).
    #[serde(default)]
    pub low_trust: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoreChannels {
    pub vector: f64,
    pub fts: f64,
    pub exact: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoreExplainEntry {
    pub memory: Memory,
    pub provenance: Vec<String>,
    pub channels: ScoreChannels,
    pub max_channel_score: f64,
    pub base_score: f64,
    pub recency_score: f64,
    pub confidence_factor: f64,
    pub status_factor: f64,
    pub trust_score: f64,
    pub trust_multiplier: f64,
    pub final_score: f64,
    pub low_trust: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MemoryFeedbackAction {
    Confirm,
    Reject,
    Supersede,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeedbackRequest {
    pub id: Uuid,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,
    pub action: MemoryFeedbackAction,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub superseded_by: Option<Uuid>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeedbackResponse {
    pub id: Uuid,
    pub status: MemoryStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f64>,
    pub evidence_count: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_verified_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub superseded_by: Option<Uuid>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LifecycleSummary {
    pub total: usize,
    pub active: usize,
    pub candidate: usize,
    pub contested: usize,
    pub stale: usize,
    pub archived: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateRequest {
    pub id: Uuid,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory_type: Option<MemoryType>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForgetRequest {
    #[serde(default)]
    pub ids: Vec<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub before: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForgetResponse {
    pub deleted: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchStoreRequest {
    pub memories: Vec<StoreRequest>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchStoreResponse {
    pub stored: Vec<StoreResponse>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchRecallRequest {
    pub queries: Vec<RecallRequest>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchRecallResponse {
    pub results: Vec<RecallResponse>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsolidateRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,
    /// Cosine similarity threshold for grouping (0.0-1.0, default 0.9)
    #[serde(default = "default_consolidation_threshold")]
    pub threshold: f32,
    /// Maximum number of clusters to process (default 50)
    #[serde(default = "default_max_clusters")]
    pub max_clusters: usize,
    /// If true, return what would be consolidated without making changes
    #[serde(default)]
    pub dry_run: bool,
}

fn default_consolidation_threshold() -> f32 {
    0.9
}
fn default_max_clusters() -> usize {
    50
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsolidationGroup {
    /// The memory that was kept (or would be kept in dry_run)
    pub kept: Uuid,
    /// Memories that were merged into the kept one (or would be)
    pub merged: Vec<Uuid>,
    /// Average similarity within the group
    pub avg_similarity: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsolidateResponse {
    pub groups: Vec<ConsolidationGroup>,
    pub total_merged: usize,
    pub dry_run: bool,
}

#[cfg(test)]
mod tests {
    use super::{Memory, MemoryStatus};

    #[test]
    fn test_confirm_from_signal_promotes_candidate() {
        let mut memory = Memory::new("proxy fact".to_string());
        memory.status = MemoryStatus::Candidate;
        memory.confidence = Some(0.2);
        memory.evidence_count = 0;
        memory.last_verified_at = None;

        memory.confirm_from_signal();

        assert_eq!(memory.status, MemoryStatus::Active);
        assert!(memory.confidence.unwrap_or_default() >= 0.5);
        assert_eq!(memory.evidence_count, 1);
        assert!(memory.last_verified_at.is_some());
    }

    #[test]
    fn test_confirm_from_signal_does_not_revive_superseded_stale_memory() {
        let mut memory = Memory::new("old fact".to_string());
        memory.status = MemoryStatus::Stale;
        memory.superseded_by = Some(uuid::Uuid::now_v7());

        memory.confirm_from_signal();

        assert_eq!(memory.status, MemoryStatus::Stale);
    }
}
