// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 memoryOSS Contributors

use std::sync::Arc;

use axum::{
    Json,
    extract::State,
    http::{StatusCode, request::Parts},
    response::{IntoResponse, Response},
    routing::{delete, get, patch, post},
};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::adapters::{
    AdapterExportArtifact, AdapterImportPreview, MemoryAdapterKind, plan_adapter_import,
    render_adapter_export,
};
use crate::config::{Config, Role};
use crate::embedding::EmbeddingEngine;
use crate::engines::document::DocumentEngine;
use crate::engines::fts::FtsEngine;
use crate::engines::group_commit::GroupCommitter;
use crate::engines::indexer::IndexerState;
use crate::engines::space_index::SpaceIndex;
use crate::engines::vector::VectorEngine;
use crate::intent_cache::IntentCache;
use crate::memory::{
    BatchRecallRequest, BatchRecallResponse, BatchStoreRequest, BatchStoreResponse,
    ConsolidateRequest, ConsolidateResponse, ConsolidationGroup, FeedbackRequest, FeedbackResponse,
    ForgetRequest, ForgetResponse, LifecycleSummary, Memory, MemoryFeedbackAction,
    MemoryHistoryBundle, MemoryHistoryReplayPlan, MemoryPassportBundle, MemoryStatus,
    PassportImportPlan, PassportScope, RecallRequest, RecallResponse, ReviewQueueKind,
    ScoreChannels, ScoreExplainEntry, ScoredMemory, StoreRequest, StoreResponse, UpdateRequest,
    build_memory_history_bundle, build_memory_history_view, build_memory_passport_bundle,
    contradiction_signature, contradiction_signatures_conflict, memories_contradict,
    plan_memory_history_replay, plan_memory_passport_import, runtime_contract_document,
    runtime_contract_export_metadata, verify_memory_history_bundle, verify_memory_passport_bundle,
};
use crate::merger::IdfIndex;
use crate::prefetch::SessionPrefetcher;
use crate::security::auth::{Claims, create_jwt, extract_bearer_token, find_api_key, validate_jwt};
use crate::security::rbac;
use crate::security::trust::TrustScorer;
use crate::server::middleware::apply_security_headers;
use crate::server::rate_limit::RateLimiter;
use crate::sharing::SharingStore;
use crate::validation;

/// Maximum outbox lag before store() returns 429 (backpressure).
const BACKPRESSURE_THRESHOLD: u64 = 1000;
const FILTER_DELETE_SCAN_LIMIT: usize = 10_000;
pub(crate) const DEFAULT_RECENT_ACTIVITY_LIMIT: usize = 10;
const MAX_RECENT_ACTIVITY_LIMIT: usize = 100;
pub(crate) const DEFAULT_REVIEW_QUEUE_LIMIT: usize = 20;
const MAX_REVIEW_QUEUE_LIMIT: usize = 100;
pub(crate) const DEFAULT_HUD_LIMIT: usize = 5;
const MAX_HUD_LIMIT: usize = 25;
pub(crate) const MEMORY_BUNDLE_FORMAT_VERSION: &str = "memoryoss.bundle.v1alpha1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MemoryBundleKind {
    Passport,
    History,
}

impl std::fmt::Display for MemoryBundleKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Passport => write!(f, "passport"),
            Self::History => write!(f, "history"),
        }
    }
}

impl std::str::FromStr for MemoryBundleKind {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "passport" => Ok(Self::Passport),
            "history" => Ok(Self::History),
            other => Err(format!("unsupported memory bundle kind: {other}")),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct MemoryBundleReference {
    pub(crate) uri: String,
    pub(crate) attachment_name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct MemoryBundleIntegrity {
    pub(crate) algorithm: String,
    pub(crate) payload_sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct MemoryBundleSignature {
    pub(crate) scheme: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) signer: Option<String>,
    pub(crate) signable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct MemoryBundleEnvelope {
    pub(crate) bundle_version: String,
    pub(crate) bundle_id: uuid::Uuid,
    pub(crate) kind: MemoryBundleKind,
    pub(crate) namespace: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) scope: Option<String>,
    pub(crate) runtime_contract_id: String,
    pub(crate) exported_at: chrono::DateTime<chrono::Utc>,
    pub(crate) reference: MemoryBundleReference,
    pub(crate) integrity: MemoryBundleIntegrity,
    pub(crate) signature: MemoryBundleSignature,
    pub(crate) payload: serde_json::Value,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct MemoryBundlePreview {
    pub(crate) bundle_id: uuid::Uuid,
    pub(crate) bundle_version: String,
    pub(crate) kind: MemoryBundleKind,
    pub(crate) namespace: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) scope: Option<String>,
    pub(crate) runtime_contract_id: String,
    pub(crate) uri: String,
    pub(crate) attachment_name: String,
    pub(crate) memory_count: usize,
    pub(crate) visible_count: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) root_id: Option<uuid::Uuid>,
    pub(crate) integrity_valid: bool,
    pub(crate) nested_integrity_valid: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) preview: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct MemoryBundleValidation {
    pub(crate) valid: bool,
    pub(crate) preview: MemoryBundlePreview,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) errors: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct MemoryBundleDiff {
    pub(crate) same_kind: bool,
    pub(crate) left: MemoryBundlePreview,
    pub(crate) right: MemoryBundlePreview,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) changed_fields: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) added_preview: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) removed_preview: Vec<String>,
    pub(crate) shared_preview_count: usize,
}

pub struct SharedState {
    pub config: Config,
    pub config_path: std::path::PathBuf,
    pub doc_engine: Arc<DocumentEngine>,
    pub vector_engine: Arc<VectorEngine>,
    pub fts_engine: Arc<FtsEngine>,
    pub embedding: Arc<EmbeddingEngine>,
    pub rate_limiter: RateLimiter,
    pub indexer_state: Arc<IndexerState>,
    pub group_committer: GroupCommitter,
    pub idf_index: Arc<IdfIndex>,
    pub space_index: Arc<SpaceIndex>,
    pub trust_scorer: Arc<TrustScorer>,
    pub sharing_store: Arc<SharingStore>,
    pub intent_cache: Arc<IntentCache>,
    pub prefetcher: Arc<SessionPrefetcher>,
    pub metrics: Arc<MetricsCounters>,
    pub review_queue_summaries:
        std::sync::RwLock<std::collections::HashMap<String, ReviewQueueSummary>>,
    /// Delta-turn detection: last seen messages hash per namespace.
    /// Prevents re-extracting facts from messages already processed.
    pub last_messages_hash: std::sync::RwLock<std::collections::HashMap<String, String>>,
}

impl SharedState {
    /// Hot-reload config from disk (triggered by SIGHUP).
    /// Only reloads safe-to-change fields: rate limits, trust threshold, IP allowlists.
    pub fn reload_config(&self) {
        match Config::load(&self.config_path) {
            Ok(new_config) => {
                self.rate_limiter
                    .set_rate(new_config.limits.rate_limit_per_sec);
                self.trust_scorer.set_threshold(new_config.trust.threshold);
                // Reload IP allowlists atomically so removed namespaces do not linger.
                let mut allowlists = std::collections::HashMap::new();
                for (ns, ips) in &new_config.trust.ip_allowlists {
                    let mut parsed = Vec::new();
                    for ip_str in ips {
                        match ip_str.parse::<std::net::IpAddr>() {
                            Ok(ip) => parsed.push(ip),
                            Err(e) => tracing::error!(
                                "Invalid IP in allowlist for namespace '{}': '{}' — {e}",
                                ns,
                                ip_str
                            ),
                        }
                    }
                    if !parsed.is_empty() {
                        allowlists.insert(ns.clone(), parsed);
                    }
                }
                self.trust_scorer.replace_ip_allowlists(allowlists);
                tracing::info!(
                    "Config hot-reloaded: rate_limit={}/s, trust_threshold={}",
                    new_config.limits.rate_limit_per_sec,
                    new_config.trust.threshold
                );
            }
            Err(e) => {
                tracing::error!("Failed to reload config from {:?}: {e}", self.config_path);
            }
        }
    }
}

/// Atomic request counters for Prometheus metrics.
pub struct MetricsCounters {
    pub stores: std::sync::atomic::AtomicU64,
    pub recalls: std::sync::atomic::AtomicU64,
    pub forgets: std::sync::atomic::AtomicU64,
    pub proxy_requests: std::sync::atomic::AtomicU64,
    pub proxy_memories_injected: std::sync::atomic::AtomicU64,
    pub proxy_gate_inject: std::sync::atomic::AtomicU64,
    pub proxy_gate_abstain: std::sync::atomic::AtomicU64,
    pub proxy_gate_need_more_evidence: std::sync::atomic::AtomicU64,
    pub proxy_policy_block: std::sync::atomic::AtomicU64,
    pub proxy_policy_require_confirmation: std::sync::atomic::AtomicU64,
    pub proxy_policy_warn: std::sync::atomic::AtomicU64,
    pub proxy_facts_extracted: std::sync::atomic::AtomicU64,
    pub proxy_upstream_errors: std::sync::atomic::AtomicU64,
}

impl MetricsCounters {
    pub fn new() -> Self {
        Self {
            stores: std::sync::atomic::AtomicU64::new(0),
            recalls: std::sync::atomic::AtomicU64::new(0),
            forgets: std::sync::atomic::AtomicU64::new(0),
            proxy_requests: std::sync::atomic::AtomicU64::new(0),
            proxy_memories_injected: std::sync::atomic::AtomicU64::new(0),
            proxy_gate_inject: std::sync::atomic::AtomicU64::new(0),
            proxy_gate_abstain: std::sync::atomic::AtomicU64::new(0),
            proxy_gate_need_more_evidence: std::sync::atomic::AtomicU64::new(0),
            proxy_policy_block: std::sync::atomic::AtomicU64::new(0),
            proxy_policy_require_confirmation: std::sync::atomic::AtomicU64::new(0),
            proxy_policy_warn: std::sync::atomic::AtomicU64::new(0),
            proxy_facts_extracted: std::sync::atomic::AtomicU64::new(0),
            proxy_upstream_errors: std::sync::atomic::AtomicU64::new(0),
        }
    }
}

pub type AppState = Arc<SharedState>;

pub(crate) fn apply_contradiction_detection(
    state: &AppState,
    namespace: &str,
    candidate: &mut Memory,
    subject: &str,
    skip_ids: &[uuid::Uuid],
) -> anyhow::Result<usize> {
    let Some(candidate_signature) = contradiction_signature(&candidate.content) else {
        return Ok(0);
    };

    let mut updates = 0usize;
    for mut existing in state.doc_engine.list_all_including_archived(namespace)? {
        if existing.id == candidate.id
            || skip_ids.contains(&existing.id)
            || existing.archived
            || existing.superseded_by.is_some()
        {
            continue;
        }

        let Some(existing_signature) = contradiction_signature(&existing.content) else {
            continue;
        };
        if !contradiction_signatures_conflict(&candidate_signature, &existing_signature) {
            continue;
        }

        if existing.mark_contradicted_by(candidate.id) {
            existing.updated_at = chrono::Utc::now();
            existing.version += 1;
            state.doc_engine.replace(&existing, subject)?;
            updates += 1;
        }
        candidate.mark_contradicted_by(existing.id);
    }

    Ok(updates)
}

fn apply_loaded_contradiction_detection(
    existing_memories: &mut [Memory],
    candidate: &mut Memory,
    skip_ids: &[uuid::Uuid],
) -> Vec<uuid::Uuid> {
    let Some(candidate_signature) = contradiction_signature(&candidate.content) else {
        return Vec::new();
    };

    let now = chrono::Utc::now();
    let mut changed_ids = Vec::new();
    for existing in existing_memories.iter_mut() {
        if existing.id == candidate.id
            || skip_ids.contains(&existing.id)
            || existing.archived
            || existing.superseded_by.is_some()
        {
            continue;
        }

        let Some(existing_signature) = contradiction_signature(&existing.content) else {
            continue;
        };
        if !contradiction_signatures_conflict(&candidate_signature, &existing_signature) {
            continue;
        }

        if existing.mark_contradicted_by(candidate.id) {
            existing.updated_at = now;
            existing.version += 1;
            changed_ids.push(existing.id);
        }
        candidate.mark_contradicted_by(existing.id);
    }

    changed_ids
}

fn leaf_source_ids(memory: &Memory) -> Vec<uuid::Uuid> {
    if memory.derived_from.is_empty() {
        vec![memory.id]
    } else {
        memory.derived_from.clone()
    }
}

fn aggregate_confidence(memories: &[&Memory]) -> Option<f64> {
    if memories.iter().any(|memory| memory.confidence.is_none()) {
        return None;
    }
    memories
        .iter()
        .filter_map(|memory| memory.confidence)
        .max_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
}

fn latest_timestamp(
    memories: &[&Memory],
    selector: fn(&Memory) -> Option<chrono::DateTime<chrono::Utc>>,
) -> Option<chrono::DateTime<chrono::Utc>> {
    memories.iter().filter_map(|memory| selector(memory)).max()
}

fn build_derived_memory(
    namespace: &str,
    kept: &Memory,
    members: &[&Memory],
    fused_content: String,
    fused_embedding: Vec<f32>,
    source_ids: Vec<uuid::Uuid>,
) -> Memory {
    let mut derived = Memory::new(fused_content.clone());
    let mut all_tags = std::collections::BTreeSet::new();
    for memory in members {
        for tag in &memory.tags {
            all_tags.insert(tag.clone());
        }
    }

    derived.namespace = Some(namespace.to_string());
    derived.memory_type = kept.memory_type;
    derived.status = kept.status;
    derived.tags = all_tags.into_iter().collect();
    derived.agent = kept.agent.clone();
    derived.session = None;
    derived.embedding = Some(fused_embedding);
    derived.content_hash = Some(crate::memory::Memory::compute_hash(&fused_content));
    derived.source_key = None;
    derived.confidence = aggregate_confidence(members);
    derived.evidence_count = members.iter().map(|memory| memory.evidence_count).sum();
    derived.last_verified_at = latest_timestamp(members, |memory| memory.last_verified_at);
    derived.derived_from = source_ids;
    derived.injection_count = members.iter().map(|memory| memory.injection_count).sum();
    derived.reuse_count = members.iter().map(|memory| memory.reuse_count).sum();
    derived.confirm_count = members.iter().map(|memory| memory.confirm_count).sum();
    derived.reject_count = members.iter().map(|memory| memory.reject_count).sum();
    derived.supersede_count = members.iter().map(|memory| memory.supersede_count).sum();
    derived.last_injected_at = latest_timestamp(members, |memory| memory.last_injected_at);
    derived.last_reused_at = latest_timestamp(members, |memory| memory.last_reused_at);
    derived.last_outcome_at = latest_timestamp(members, |memory| memory.last_outcome_at);
    derived
}

pub(crate) async fn run_namespace_consolidation(
    state: &AppState,
    namespace: &str,
    threshold: f64,
    max_clusters: usize,
    dry_run: bool,
    subject: &str,
) -> anyhow::Result<ConsolidateResponse> {
    let all_memories = state.doc_engine.search(namespace, None, None, None, &[])?;
    let active_before = crate::fusion::count_active_memories(&all_memories);
    let duplicate_rate_before =
        crate::fusion::duplicate_rate_for_active_memories(&all_memories, threshold);

    if all_memories.len() < 2 {
        return Ok(ConsolidateResponse {
            groups: vec![],
            total_merged: 0,
            derived_created: 0,
            active_before,
            active_after: active_before,
            active_reduction: 0,
            duplicate_rate_before,
            duplicate_rate_after: duplicate_rate_before,
            dry_run,
        });
    }

    let clusters =
        crate::fusion::build_consolidation_clusters(&all_memories, threshold, max_clusters);
    if clusters.is_empty() {
        return Ok(ConsolidateResponse {
            groups: vec![],
            total_merged: 0,
            derived_created: 0,
            active_before,
            active_after: active_before,
            active_reduction: 0,
            duplicate_rate_before,
            duplicate_rate_after: duplicate_rate_before,
            dry_run,
        });
    }

    let mut groups = Vec::with_capacity(clusters.len());
    let mut derived_created = 0usize;
    let mut outbox_events = 0u64;
    let mut derived_ids = Vec::new();

    for cluster in clusters {
        let kept_idx = cluster
            .members
            .iter()
            .copied()
            .max_by(|&a, &b| {
                use std::cmp::Ordering;
                if crate::fusion::prefer_consolidation_candidate(&all_memories[a], &all_memories[b])
                {
                    Ordering::Greater
                } else if crate::fusion::prefer_consolidation_candidate(
                    &all_memories[b],
                    &all_memories[a],
                ) {
                    Ordering::Less
                } else {
                    all_memories[a].id.cmp(&all_memories[b].id)
                }
            })
            .unwrap();

        let kept_id = all_memories[kept_idx].id;
        let merged_ids: Vec<uuid::Uuid> = cluster
            .members
            .iter()
            .filter(|&&idx| idx != kept_idx)
            .map(|&idx| all_memories[idx].id)
            .collect();

        let source_memories: Vec<&Memory> = cluster
            .members
            .iter()
            .map(|&idx| &all_memories[idx])
            .collect();
        let mut source_ids = source_memories
            .iter()
            .flat_map(|memory| leaf_source_ids(memory))
            .collect::<Vec<_>>();
        source_ids.sort_unstable();
        source_ids.dedup();

        let mut derived_id = None;
        if !dry_run {
            let kept_memory = &all_memories[kept_idx];
            let mut fused_content = kept_memory.content.clone();
            for memory in &source_memories {
                if memory.id == kept_memory.id {
                    continue;
                }
                fused_content = crate::fusion::fuse_contents(&fused_content, &memory.content);
            }
            let fused_embedding = state.embedding.embed_one(&fused_content).await?;
            let derived = build_derived_memory(
                namespace,
                kept_memory,
                &source_memories,
                fused_content,
                fused_embedding,
                source_ids.clone(),
            );
            derived_id = Some(derived.id);
            derived_created += 1;
            derived_ids.push(derived.id);
            state
                .doc_engine
                .store_batch_tx(&[(derived.clone(), subject.to_string())])?;
            outbox_events += 1;

            for source_id in cluster.members.iter().map(|&idx| all_memories[idx].id) {
                if let Some(mut source) = state.doc_engine.get(source_id, namespace)? {
                    source.mark_consolidated_into(derived.id);
                    state.doc_engine.replace(&source, subject)?;
                    outbox_events += 1;
                    if state.doc_engine.archive(source_id, namespace, subject)? {
                        outbox_events += 1;
                    }
                }
            }
        }

        groups.push(ConsolidationGroup {
            kept: kept_id,
            merged: merged_ids,
            avg_similarity: cluster.avg_similarity,
            derived_id,
            source_ids,
        });
    }

    if !dry_run && outbox_events > 0 {
        state
            .indexer_state
            .write_seq
            .fetch_add(outbox_events, std::sync::atomic::Ordering::Relaxed);
        state.indexer_state.wake();
        state.intent_cache.invalidate_namespace(namespace).await;
        if let Ok(shared_nss) = state.sharing_store.accessible_namespaces(namespace) {
            for sns in &shared_nss {
                for derived_id in &derived_ids {
                    state.sharing_store.fire_webhook(sns, *derived_id);
                }
            }
        }
    }

    let current_memories = if dry_run {
        all_memories.clone()
    } else {
        state.doc_engine.search(namespace, None, None, None, &[])?
    };
    let active_after = crate::fusion::count_active_memories(&current_memories);
    let duplicate_rate_after =
        crate::fusion::duplicate_rate_for_active_memories(&current_memories, threshold);
    let total_merged = groups.iter().map(|group| group.merged.len()).sum();

    Ok(ConsolidateResponse {
        groups,
        total_merged,
        derived_created,
        active_before,
        active_after,
        active_reduction: active_before.saturating_sub(active_after),
        duplicate_rate_before,
        duplicate_rate_after,
        dry_run,
    })
}

pub(crate) fn apply_memory_lifecycle(
    state: &AppState,
    namespace: &str,
    memory: &mut Memory,
    subject: &str,
    after_days: u64,
) -> anyhow::Result<bool> {
    let trust = state.trust_scorer.score_memory(memory, namespace);
    let decision = memory.apply_lifecycle_policy(chrono::Utc::now(), after_days, trust.score);
    let mut changed = false;

    if decision.changed {
        state.doc_engine.replace(memory, subject)?;
        changed = true;
    }

    if decision.archive && state.doc_engine.archive(memory.id, namespace, subject)? {
        memory.archived = true;
        memory.updated_at = chrono::Utc::now();
        state
            .indexer_state
            .write_seq
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        state.indexer_state.wake();
        changed = true;
    }

    Ok(changed)
}

pub(crate) fn run_namespace_lifecycle(
    state: &AppState,
    namespace: &str,
    subject: &str,
    after_days: u64,
) -> anyhow::Result<usize> {
    let mut changed = 0usize;
    for mut memory in state.doc_engine.list_all_including_archived(namespace)? {
        if apply_memory_lifecycle(state, namespace, &mut memory, subject, after_days)? {
            changed += 1;
        }
    }
    Ok(changed)
}

pub fn router(state: AppState) -> axum::Router {
    axum::Router::new()
        .route("/health", get(health))
        .route("/v1/auth/token", post(auth_token))
        .route("/v1/store", post(store))
        .route("/v1/store/batch", post(store_batch))
        .route("/v1/recall", post(recall))
        .route("/v1/recall/batch", post(recall_batch))
        .route("/v1/consolidate", post(consolidate))
        .route("/v1/update", patch(update))
        .route("/v1/feedback", post(feedback))
        .route("/v1/forget", delete(forget))
        .route("/v1/admin/tokens", post(create_scoped_token))
        .route("/v1/admin/cache/flush", post(flush_cache))
        .route("/v1/admin/cache/stats", get(cache_stats))
        .route("/v1/admin/index-health", get(index_health))
        .route("/v1/admin/idf-stats", get(idf_stats))
        .route("/v1/admin/query-explain", post(query_explain))
        .route("/v1/inspect/{id}", get(inspect_memory))
        .route("/v1/peek/{id}", get(peek_memory))
        .route("/v1/admin/space-stats", get(space_stats))
        .route("/v1/admin/keys/rotate", post(rotate_key))
        .route("/v1/admin/keys/{id}", delete(revoke_key))
        .route("/v1/admin/keys", get(list_keys))
        .route("/v1/admin/trust-stats", get(trust_stats))
        .route("/v1/admin/lifecycle", get(lifecycle_view))
        .route("/v1/admin/hud", get(hud_view))
        .route("/v1/admin/recent", get(recent_activity_view))
        .route("/v1/admin/review-queue", get(review_queue_view))
        .route("/v1/admin/review/action", post(review_queue_action))
        .route("/v1/admin/intent-cache/stats", get(intent_cache_stats))
        .route("/v1/admin/intent-cache/flush", post(flush_intent_cache))
        .route("/v1/admin/prefetch/stats", get(prefetch_stats))
        .route("/v1/admin/sharing/create", post(create_shared_ns))
        .route("/v1/admin/sharing/list", get(list_shared_ns))
        .route("/v1/admin/sharing/{name}", delete(delete_shared_ns))
        .route(
            "/v1/admin/sharing/{name}/grants/add",
            post(add_sharing_grant),
        )
        .route(
            "/v1/admin/sharing/{name}/grants/list",
            get(list_sharing_grants),
        )
        .route(
            "/v1/admin/sharing/{name}/grants/{grant_id}",
            delete(remove_sharing_grant),
        )
        .route("/v1/sharing/accessible", get(accessible_shared_ns))
        .route("/v1/export", get(gdpr_export))
        .route("/v1/history/replay", post(history_replay))
        .route("/v1/history/{id}/bundle", get(history_bundle))
        .route("/v1/history/{id}", get(history_view))
        .route("/v1/adapters/export", get(adapter_export))
        .route("/v1/adapters/import", post(adapter_import))
        .route("/v1/bundles/export", get(bundle_export))
        .route("/v1/bundles/preview", post(bundle_preview))
        .route("/v1/bundles/validate", post(bundle_validate))
        .route("/v1/bundles/diff", post(bundle_diff))
        .route("/v1/passport/export", get(passport_export))
        .route("/v1/passport/import", post(passport_import))
        .route("/v1/runtime/contract", get(runtime_contract))
        .route("/v1/memories", get(gdpr_access))
        .route("/v1/forget/certified", delete(gdpr_forget_certified))
        .route("/metrics", get(metrics))
        .route("/v1/source", get(agpl_source))
        // Proxy routes: OpenAI-compatible memory injection
        // Proxy sub-router: isolated fallback only catches /proxy/* paths
        .nest(
            "/proxy",
            axum::Router::new()
                .route(
                    "/v1/chat/completions",
                    post(super::proxy::proxy_chat_completions),
                )
                .route("/v1/responses", post(super::proxy::proxy_responses))
                .route("/v1/models", get(super::proxy::proxy_models))
                .route("/v1/debug/stats", get(super::proxy::proxy_debug_stats))
                .route(
                    "/anthropic/v1/messages",
                    post(super::proxy::proxy_anthropic_messages),
                )
                // SDKs append /v1/messages to ANTHROPIC_BASE_URL which already contains /v1
                .route(
                    "/anthropic/v1/v1/messages",
                    post(super::proxy::proxy_anthropic_messages),
                )
                .fallback(super::proxy::proxy_passthrough)
                .layer(axum::extract::DefaultBodyLimit::max(2 * 1024 * 1024)) // 2MB cap on proxy requests
                .with_state(state.clone()),
        )
        .layer(axum::extract::DefaultBodyLimit::max(2 * 1024 * 1024)) // B5 FIX: explicit 2MB limit on all routes
        .layer(axum::middleware::map_response(add_security_headers))
        .with_state(state)
}

async fn add_security_headers(response: Response) -> Response {
    apply_security_headers(response)
}

async fn health(State(state): State<Arc<SharedState>>) -> Response {
    let (total_memories, _) = state.space_index.global_stats();
    let memory_mode = &state.config.proxy.default_memory_mode;
    Json(json!({
        "status": "ok",
        "memory_mode": memory_mode,
        "total_memories": total_memories
    }))
    .into_response()
}

pub(crate) fn lifecycle_summary_from_memories(memories: &[Memory]) -> LifecycleSummary {
    LifecycleSummary {
        total: memories.len(),
        active: memories
            .iter()
            .filter(|memory| memory.status == MemoryStatus::Active)
            .count(),
        candidate: memories
            .iter()
            .filter(|memory| memory.status == MemoryStatus::Candidate)
            .count(),
        contested: memories
            .iter()
            .filter(|memory| memory.status == MemoryStatus::Contested)
            .count(),
        stale: memories
            .iter()
            .filter(|memory| memory.status == MemoryStatus::Stale)
            .count(),
        archived: memories.iter().filter(|memory| memory.archived).count(),
    }
}

fn preview_text(content: &str, max_chars: usize) -> String {
    let mut preview: String = content.chars().take(max_chars).collect();
    if content.chars().count() > max_chars {
        preview.push_str("...");
    }
    preview
}

fn feedback_action_label(memory: &Memory) -> &'static str {
    if memory.supersede_count > 0 {
        "supersede"
    } else if memory.reject_count > 0 {
        "reject"
    } else if memory.confirm_count > 0 {
        "confirm"
    } else {
        "feedback"
    }
}

fn build_recent_entries<F>(
    memories: &[Memory],
    limit: usize,
    mut mapper: F,
) -> Vec<serde_json::Value>
where
    F: FnMut(&Memory) -> Option<(chrono::DateTime<chrono::Utc>, serde_json::Value)>,
{
    let mut entries: Vec<_> = memories.iter().filter_map(&mut mapper).collect();
    entries.sort_by(|a, b| b.0.cmp(&a.0));
    entries
        .into_iter()
        .take(limit)
        .map(|(_, entry)| entry)
        .collect()
}

pub(crate) fn build_recent_activity(memories: &[Memory], limit: usize) -> serde_json::Value {
    let limit = limit.clamp(1, MAX_RECENT_ACTIVITY_LIMIT);

    let injections = build_recent_entries(memories, limit, |memory| {
        memory.last_injected_at.map(|at| {
            (
                at,
                json!({
                    "id": memory.id,
                    "at": at,
                    "preview": preview_text(&memory.content, 100),
                    "status": memory.status,
                    "archived": memory.archived,
                    "namespace": memory.namespace,
                    "agent": memory.agent,
                    "session": memory.session,
                    "injection_count": memory.injection_count,
                    "reuse_count": memory.reuse_count,
                    "last_reused_at": memory.last_reused_at,
                }),
            )
        })
    });

    let extractions = build_recent_entries(memories, limit, |memory| {
        (memory.source_key.as_deref() == Some("proxy-extraction")).then_some(())?;
        Some((
            memory.created_at,
            json!({
                "id": memory.id,
                "at": memory.created_at,
                "preview": preview_text(&memory.content, 100),
                "status": memory.status,
                "archived": memory.archived,
                "namespace": memory.namespace,
                "confidence": memory.confidence,
                "evidence_count": memory.evidence_count,
                "source_key": memory.source_key,
            }),
        ))
    });

    let feedbacks = build_recent_entries(memories, limit, |memory| {
        memory.last_outcome_at.map(|at| {
            (
                at,
                json!({
                    "id": memory.id,
                    "at": at,
                    "preview": preview_text(&memory.content, 100),
                    "status": memory.status,
                    "archived": memory.archived,
                    "namespace": memory.namespace,
                    "action": feedback_action_label(memory),
                    "confirm_count": memory.confirm_count,
                    "reject_count": memory.reject_count,
                    "supersede_count": memory.supersede_count,
                    "superseded_by": memory.superseded_by,
                }),
            )
        })
    });

    let consolidations = build_recent_entries(memories, limit, |memory| {
        if memory.derived_from.is_empty() {
            return None;
        }
        Some((
            memory.created_at,
            json!({
                "id": memory.id,
                "at": memory.created_at,
                "preview": preview_text(&memory.content, 100),
                "status": memory.status,
                "archived": memory.archived,
                "namespace": memory.namespace,
                "derived_from": memory.derived_from,
                "derived_count": memory.derived_from.len(),
                "superseded_by": memory.superseded_by,
            }),
        ))
    });

    json!({
        "counts": {
            "injections": injections.len(),
            "extractions": extractions.len(),
            "feedbacks": feedbacks.len(),
            "consolidations": consolidations.len(),
        },
        "injections": injections,
        "extractions": extractions,
        "feedbacks": feedbacks,
        "consolidations": consolidations,
    })
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ReviewQueueReplacementOption {
    pub(crate) review_key: String,
    pub(crate) preview: String,
    pub(crate) status: MemoryStatus,
    pub(crate) relation: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ReviewQueueItem {
    pub(crate) review_key: String,
    pub(crate) queue_kind: ReviewQueueKind,
    pub(crate) status: MemoryStatus,
    pub(crate) suggested_action: MemoryFeedbackAction,
    pub(crate) available_actions: Vec<MemoryFeedbackAction>,
    pub(crate) age_hours: i64,
    pub(crate) preview: String,
    pub(crate) source: String,
    pub(crate) confidence: Option<f64>,
    pub(crate) evidence_count: u32,
    pub(crate) trust_score: f64,
    pub(crate) trust_confidence_low: f64,
    pub(crate) trust_confidence_high: f64,
    pub(crate) low_trust: bool,
    pub(crate) tags: Vec<String>,
    pub(crate) agent: Option<String>,
    pub(crate) session: Option<String>,
    pub(crate) contradiction_count: usize,
    pub(crate) review_event_count: usize,
    pub(crate) last_review_at: Option<chrono::DateTime<chrono::Utc>>,
    pub(crate) last_review_action: Option<MemoryFeedbackAction>,
    pub(crate) replacement_options: Vec<ReviewQueueReplacementOption>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub(crate) struct ReviewQueueSummary {
    pub(crate) pending: usize,
    pub(crate) candidate: usize,
    pub(crate) contested: usize,
    pub(crate) rejected: usize,
    pub(crate) suggested_confirm: usize,
    pub(crate) suggested_reject: usize,
    pub(crate) suggested_supersede: usize,
    pub(crate) oldest_age_hours: i64,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ReviewQueueView {
    pub(crate) summary: ReviewQueueSummary,
    pub(crate) items: Vec<ReviewQueueItem>,
}

pub(crate) fn encode_review_key(id: uuid::Uuid) -> String {
    format!("review:{id}")
}

pub(crate) fn decode_review_key(review_key: &str) -> Result<uuid::Uuid, String> {
    let raw = review_key
        .strip_prefix("review:")
        .or_else(|| review_key.strip_prefix("rq:"))
        .unwrap_or(review_key);
    raw.parse::<uuid::Uuid>()
        .map_err(|_| format!("invalid review key: {review_key}"))
}

fn review_available_actions(memory: &Memory) -> Vec<MemoryFeedbackAction> {
    let mut actions = vec![MemoryFeedbackAction::Confirm, MemoryFeedbackAction::Reject];
    if !memory.contradicts_with.is_empty() || memory.superseded_by.is_some() {
        actions.push(MemoryFeedbackAction::Supersede);
    }
    actions
}

fn review_priority(kind: ReviewQueueKind) -> u8 {
    match kind {
        ReviewQueueKind::Contested => 0,
        ReviewQueueKind::Rejected => 1,
        ReviewQueueKind::Candidate => 2,
    }
}

fn review_source_label(memory: &Memory) -> String {
    memory
        .source_key
        .clone()
        .or_else(|| memory.agent.clone())
        .unwrap_or_else(|| "manual".to_string())
}

pub(crate) fn build_review_queue_summary(memories: &[Memory]) -> ReviewQueueSummary {
    let now = chrono::Utc::now();
    let mut summary = ReviewQueueSummary {
        pending: 0,
        candidate: 0,
        contested: 0,
        rejected: 0,
        suggested_confirm: 0,
        suggested_reject: 0,
        suggested_supersede: 0,
        oldest_age_hours: 0,
    };

    for memory in memories {
        let Some(kind) = memory.review_queue_kind() else {
            continue;
        };
        summary.pending += 1;
        match kind {
            ReviewQueueKind::Candidate => summary.candidate += 1,
            ReviewQueueKind::Contested => summary.contested += 1,
            ReviewQueueKind::Rejected => summary.rejected += 1,
        }
        match memory.suggested_review_action() {
            MemoryFeedbackAction::Confirm => summary.suggested_confirm += 1,
            MemoryFeedbackAction::Reject => summary.suggested_reject += 1,
            MemoryFeedbackAction::Supersede => summary.suggested_supersede += 1,
        }
        let age_hours = (now - memory.lifecycle_anchor()).num_hours().max(0);
        summary.oldest_age_hours = summary.oldest_age_hours.max(age_hours);
    }

    summary
}

pub(crate) fn merge_review_queue_summaries<'a>(
    summaries: impl IntoIterator<Item = &'a ReviewQueueSummary>,
) -> ReviewQueueSummary {
    let mut merged = ReviewQueueSummary::default();
    for summary in summaries {
        merged.pending += summary.pending;
        merged.candidate += summary.candidate;
        merged.contested += summary.contested;
        merged.rejected += summary.rejected;
        merged.suggested_confirm += summary.suggested_confirm;
        merged.suggested_reject += summary.suggested_reject;
        merged.suggested_supersede += summary.suggested_supersede;
        merged.oldest_age_hours = merged.oldest_age_hours.max(summary.oldest_age_hours);
    }
    merged
}

pub(crate) fn cached_review_queue_summary(
    state: &SharedState,
    namespace: &str,
) -> ReviewQueueSummary {
    state
        .review_queue_summaries
        .read()
        .ok()
        .and_then(|summaries| summaries.get(namespace).cloned())
        .unwrap_or_default()
}

pub(crate) fn cached_global_review_queue_summary(state: &SharedState) -> ReviewQueueSummary {
    state
        .review_queue_summaries
        .read()
        .ok()
        .map(|summaries| merge_review_queue_summaries(summaries.values()))
        .unwrap_or_default()
}

pub(crate) fn refresh_review_queue_summary(
    state: &SharedState,
    namespace: &str,
) -> anyhow::Result<()> {
    let memories = state.doc_engine.list_all_including_archived(namespace)?;
    let summary = build_review_queue_summary(&memories);
    if let Ok(mut summaries) = state.review_queue_summaries.write() {
        summaries.insert(namespace.to_string(), summary);
    }
    Ok(())
}

fn note_indexer_writes(state: &AppState, write_count: usize) {
    if write_count == 0 {
        return;
    }

    state
        .indexer_state
        .write_seq
        .fetch_add(write_count as u64, std::sync::atomic::Ordering::Relaxed);
    state.indexer_state.wake();
}

async fn wait_for_indexer_catchup(state: &AppState, target_seq: u64) -> Result<(), AppError> {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        let vector_seq = state
            .indexer_state
            .vector_seq
            .load(std::sync::atomic::Ordering::Relaxed);
        let fts_seq = state
            .indexer_state
            .fts_seq
            .load(std::sync::atomic::Ordering::Relaxed);
        if vector_seq >= target_seq && fts_seq >= target_seq {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(AppError::Internal(anyhow::anyhow!(
                "derived indexes did not catch up after import"
            )));
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
}

pub(crate) fn build_review_queue(
    memories: &[Memory],
    trust_scorer: &TrustScorer,
    namespace: &str,
    limit: usize,
) -> ReviewQueueView {
    let now = chrono::Utc::now();
    let summary = build_review_queue_summary(memories);
    let lookup: std::collections::HashMap<uuid::Uuid, &Memory> =
        memories.iter().map(|memory| (memory.id, memory)).collect();

    let mut items: Vec<(u8, chrono::DateTime<chrono::Utc>, ReviewQueueItem)> = memories
        .iter()
        .filter_map(|memory| {
            let queue_kind = memory.review_queue_kind()?;
            let trust = trust_scorer.score_memory(memory, namespace);
            let replacement_options: Vec<_> = memory
                .contradicts_with
                .iter()
                .filter_map(|conflict_id| lookup.get(conflict_id).copied())
                .take(3)
                .map(|candidate| ReviewQueueReplacementOption {
                    review_key: encode_review_key(candidate.id),
                    preview: preview_text(&candidate.content, 80),
                    status: candidate.status,
                    relation: "contradiction",
                })
                .collect();
            let last_review = memory.review_events.last();
            Some((
                review_priority(queue_kind),
                memory.updated_at,
                ReviewQueueItem {
                    review_key: encode_review_key(memory.id),
                    queue_kind,
                    status: memory.status,
                    suggested_action: memory.suggested_review_action(),
                    available_actions: review_available_actions(memory),
                    age_hours: (now - memory.lifecycle_anchor()).num_hours().max(0),
                    preview: preview_text(&memory.content, 100),
                    source: review_source_label(memory),
                    confidence: memory.confidence,
                    evidence_count: memory.evidence_count,
                    trust_score: trust.score,
                    trust_confidence_low: trust.confidence_low,
                    trust_confidence_high: trust.confidence_high,
                    low_trust: trust.low_trust,
                    tags: memory.tags.clone(),
                    agent: memory.agent.clone(),
                    session: memory.session.clone(),
                    contradiction_count: memory.contradicts_with.len(),
                    review_event_count: memory.review_events.len(),
                    last_review_at: last_review.map(|event| event.at),
                    last_review_action: last_review.map(|event| event.action),
                    replacement_options,
                },
            ))
        })
        .collect();

    items.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| b.1.cmp(&a.1)));
    let items = items
        .into_iter()
        .take(limit.max(1))
        .map(|(_, _, item)| item)
        .collect();

    ReviewQueueView { summary, items }
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct HudQuickAction {
    pub(crate) id: String,
    pub(crate) label: String,
    pub(crate) description: String,
    pub(crate) method: String,
    pub(crate) path: String,
    pub(crate) cli: String,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct HudPolicyCounterSnapshot {
    pub(crate) block: u64,
    pub(crate) require_confirmation: u64,
    pub(crate) warn: u64,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct HudPolicyProbe {
    pub(crate) id: String,
    pub(crate) label: String,
    pub(crate) query: String,
    pub(crate) decision: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) risky_actions: Vec<String>,
    pub(crate) matched_policy_count: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) matched_policy_previews: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct HudPolicyOverview {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) live_counters: Option<HudPolicyCounterSnapshot>,
    pub(crate) actionable_probes: usize,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct HudNamespaceView {
    pub(crate) namespace: String,
    pub(crate) lifecycle: LifecycleSummary,
    pub(crate) recent: serde_json::Value,
    pub(crate) review_queue: ReviewQueueView,
    pub(crate) policy_probes: Vec<HudPolicyProbe>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct HudSummary {
    pub(crate) namespaces: usize,
    pub(crate) total_memories: usize,
    pub(crate) active_memories: usize,
    pub(crate) contested_memories: usize,
    pub(crate) pending_reviews: usize,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct HudView {
    pub(crate) generated_at: chrono::DateTime<chrono::Utc>,
    pub(crate) limit: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) namespace_filter: Option<String>,
    pub(crate) summary: HudSummary,
    pub(crate) policy_firewall: HudPolicyOverview,
    pub(crate) quick_actions: Vec<HudQuickAction>,
    pub(crate) namespaces: Vec<HudNamespaceView>,
}

fn hud_policy_counter_snapshot(metrics: &MetricsCounters) -> HudPolicyCounterSnapshot {
    HudPolicyCounterSnapshot {
        block: metrics
            .proxy_policy_block
            .load(std::sync::atomic::Ordering::Relaxed),
        require_confirmation: metrics
            .proxy_policy_require_confirmation
            .load(std::sync::atomic::Ordering::Relaxed),
        warn: metrics
            .proxy_policy_warn
            .load(std::sync::atomic::Ordering::Relaxed),
    }
}

fn hud_namespace_hint(namespace_memories: &[(String, Vec<Memory>)]) -> String {
    match namespace_memories {
        [(namespace, _)] => namespace.clone(),
        _ => "<namespace>".to_string(),
    }
}

fn build_hud_quick_actions(namespace_hint: &str, limit: usize) -> Vec<HudQuickAction> {
    vec![
        HudQuickAction {
            id: "search".to_string(),
            label: "Search".to_string(),
            description: "Query recall without leaving the HUD.".to_string(),
            method: "POST".to_string(),
            path: "/v1/recall".to_string(),
            cli: format!(
                "curl -s \"$BASE/v1/recall\" -H \"Authorization: Bearer $KEY\" -H \"content-type: application/json\" -d '{{\"namespace\":\"{namespace_hint}\",\"query\":\"...\"}}'"
            ),
        },
        HudQuickAction {
            id: "why".to_string(),
            label: "Why".to_string(),
            description: "Inspect recall evidence and policy reasoning.".to_string(),
            method: "POST".to_string(),
            path: "/v1/admin/query-explain".to_string(),
            cli: format!(
                "curl -s \"$BASE/v1/admin/query-explain\" -H \"Authorization: Bearer $KEY\" -H \"content-type: application/json\" -d '{{\"query\":\"...\",\"limit\":{limit}}}'"
            ),
        },
        HudQuickAction {
            id: "recent".to_string(),
            label: "Recent".to_string(),
            description: "See the latest injections, feedback, and consolidations.".to_string(),
            method: "GET".to_string(),
            path: format!("/v1/admin/recent?namespace={namespace_hint}&limit={limit}"),
            cli: format!("memoryoss recent --namespace {namespace_hint} --limit {limit}"),
        },
        HudQuickAction {
            id: "review".to_string(),
            label: "Review".to_string(),
            description: "Open the pending candidate, contested, and rejected inbox.".to_string(),
            method: "GET".to_string(),
            path: format!("/v1/admin/review-queue?namespace={namespace_hint}&limit={limit}"),
            cli: format!("memoryoss review queue --namespace {namespace_hint} --limit {limit}"),
        },
        HudQuickAction {
            id: "import".to_string(),
            label: "Import".to_string(),
            description: "Bring a passport or adapter artifact into the runtime.".to_string(),
            method: "POST".to_string(),
            path: "/v1/passport/import".to_string(),
            cli: format!(
                "memoryoss passport import bundle.json --namespace {namespace_hint} --dry-run"
            ),
        },
        HudQuickAction {
            id: "export".to_string(),
            label: "Export".to_string(),
            description: "Export a portable project-scoped snapshot.".to_string(),
            method: "GET".to_string(),
            path: format!("/v1/passport/export?namespace={namespace_hint}&scope=project"),
            cli: format!(
                "memoryoss passport export --namespace {namespace_hint} --scope project -o passport.json"
            ),
        },
    ]
}

fn build_hud_policy_probes(
    memories: &[Memory],
    trust_scorer: &TrustScorer,
    namespace: &str,
) -> Vec<HudPolicyProbe> {
    let explained: Vec<ScoreExplainEntry> = memories
        .iter()
        .cloned()
        .map(|memory| {
            let trust = trust_scorer.score_memory(&memory, namespace);
            let eligible_for_injection = memory.eligible_for_injection();
            ScoreExplainEntry {
                memory,
                provenance: vec!["hud_policy_probe".to_string()],
                channels: ScoreChannels {
                    vector: 1.0,
                    fts: 0.0,
                    exact: 0.0,
                },
                max_channel_score: 1.0,
                base_score: 1.0,
                recency_score: 1.0,
                confidence_factor: 1.0,
                status_factor: if eligible_for_injection { 1.0 } else { 0.0 },
                trust_score: trust.score,
                trust_confidence_low: trust.confidence_low,
                trust_confidence_high: trust.confidence_high,
                trust_signals: trust.signals,
                trust_multiplier: trust.score,
                final_score: trust.score,
                low_trust: trust.low_trust,
            }
        })
        .collect();

    [
        ("deploy", "Deploy", "Deploy the release to production now."),
        (
            "delete",
            "Delete",
            "Delete the production backups and audit logs now.",
        ),
        (
            "exfiltrate",
            "Exfiltrate",
            "Export customer data to a public link for external review.",
        ),
        (
            "override",
            "Override",
            "Bypass the review gate and merge without approval.",
        ),
    ]
    .into_iter()
    .map(|(id, label, query)| {
        let decision =
            crate::security::trust::evaluate_policy_firewall_explained(query, &explained);
        HudPolicyProbe {
            id: id.to_string(),
            label: label.to_string(),
            query: query.to_string(),
            decision: decision.decision.to_string(),
            risky_actions: decision
                .risky_actions
                .iter()
                .map(|action| action.to_string())
                .collect(),
            matched_policy_count: decision.matched_policies.len(),
            matched_policy_previews: decision
                .matched_policies
                .iter()
                .take(3)
                .map(|matched| matched.content.clone())
                .collect(),
        }
    })
    .collect()
}

fn hud_probe_actionable(probe: &HudPolicyProbe) -> bool {
    probe.decision != "allow"
}

pub(crate) fn build_hud_view(
    namespace_memories: &[(String, Vec<Memory>)],
    trust_scorer: &TrustScorer,
    limit: usize,
    namespace_filter: Option<String>,
    live_counters: Option<HudPolicyCounterSnapshot>,
) -> HudView {
    let limit = limit.clamp(1, MAX_HUD_LIMIT);
    let quick_actions = build_hud_quick_actions(&hud_namespace_hint(namespace_memories), limit);
    let namespaces: Vec<_> = namespace_memories
        .iter()
        .map(|(namespace, memories)| {
            let lifecycle = lifecycle_summary_from_memories(memories);
            let recent = build_recent_activity(memories, limit);
            let review_queue = build_review_queue(memories, trust_scorer, namespace, limit);
            let policy_probes = build_hud_policy_probes(memories, trust_scorer, namespace);
            HudNamespaceView {
                namespace: namespace.clone(),
                lifecycle,
                recent,
                review_queue,
                policy_probes,
            }
        })
        .collect();

    let summary = HudSummary {
        namespaces: namespaces.len(),
        total_memories: namespaces.iter().map(|view| view.lifecycle.total).sum(),
        active_memories: namespaces.iter().map(|view| view.lifecycle.active).sum(),
        contested_memories: namespaces.iter().map(|view| view.lifecycle.contested).sum(),
        pending_reviews: namespaces
            .iter()
            .map(|view| view.review_queue.summary.pending)
            .sum(),
    };
    let actionable_probes = namespaces
        .iter()
        .flat_map(|view| view.policy_probes.iter())
        .filter(|probe| hud_probe_actionable(probe))
        .count();

    HudView {
        generated_at: chrono::Utc::now(),
        limit,
        namespace_filter,
        summary,
        policy_firewall: HudPolicyOverview {
            live_counters,
            actionable_probes,
        },
        quick_actions,
        namespaces,
    }
}

fn html_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn render_hud_html(view: &HudView) -> String {
    use std::fmt::Write as _;

    let mut html = String::new();
    let _ = write!(
        html,
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width, initial-scale=1\"><title>memoryOSS HUD</title><style>\
        :root{{--bg:#f6f1e8;--panel:#fffdf8;--ink:#17231f;--muted:#5a6b63;--accent:#c55c32;--accent-soft:#f0d3c4;--line:#d8c8b7;--ok:#2f6b57;--warn:#8b5a2b;--danger:#8f2d24;--shadow:0 18px 40px rgba(23,35,31,.08);}}\
        *{{box-sizing:border-box}}body{{margin:0;font-family:Georgia,\"Iowan Old Style\",serif;background:radial-gradient(circle at top,#fff9f0 0,#f6f1e8 48%,#ebe1d3 100%);color:var(--ink)}}\
        .shell{{max-width:1180px;margin:0 auto;padding:28px 18px 48px}}\
        .hero{{display:grid;gap:18px;margin-bottom:24px}}\
        .hero h1{{margin:0;font-size:clamp(2.1rem,6vw,4rem);letter-spacing:-.04em}}\
        .hero p{{margin:0;color:var(--muted);max-width:760px;line-height:1.45}}\
        .summary{{display:grid;grid-template-columns:repeat(auto-fit,minmax(140px,1fr));gap:12px}}\
        .card,.action,.namespace{{background:var(--panel);border:1px solid var(--line);border-radius:20px;box-shadow:var(--shadow)}}\
        .card{{padding:16px}}.summary strong{{display:block;font-size:1.7rem}}.summary span{{color:var(--muted);font-size:.92rem}}\
        .section{{margin-top:24px}}.section h2{{margin:0 0 12px;font-size:1.2rem;letter-spacing:.04em;text-transform:uppercase}}\
        .actions{{display:grid;grid-template-columns:repeat(auto-fit,minmax(220px,1fr));gap:12px}}\
        .action{{padding:16px;display:grid;gap:10px}}.eyebrow{{font-size:.72rem;letter-spacing:.12em;text-transform:uppercase;color:var(--accent)}}\
        .action code,.policy code{{display:block;background:#f8f2ea;border-radius:10px;padding:10px;overflow:auto;font-size:.85rem}}\
        .policy-grid,.namespaces{{display:grid;gap:14px}}.policy-grid{{grid-template-columns:repeat(auto-fit,minmax(220px,1fr))}}\
        .policy{{padding:16px;background:var(--panel);border:1px solid var(--line);border-radius:18px}}\
        .pill{{display:inline-block;padding:4px 10px;border-radius:999px;font-size:.76rem;background:var(--accent-soft);margin-right:6px;margin-top:6px}}\
        .decision-allow{{color:var(--ok)}}.decision-warn{{color:var(--warn)}}.decision-require_confirmation{{color:var(--accent)}}.decision-block{{color:var(--danger)}}\
        .namespace{{padding:18px}}.meta{{display:flex;flex-wrap:wrap;gap:8px;margin:10px 0 14px}}\
        ul{{margin:0;padding-left:18px}}li+li{{margin-top:8px}}small{{color:var(--muted)}}\
        @media (max-width:720px){{.shell{{padding:20px 14px 36px}}}}\
        </style></head><body><div class=\"shell\">"
    );
    let _ = write!(
        html,
        "<section class=\"hero\"><div><div class=\"eyebrow\">Universal Memory HUD</div><h1>memoryOSS at a glance</h1><p>Always-on operator surface for search, explain, recent, review, import, export, and policy visibility. Generated {}</p></div><div class=\"summary\">",
        html_escape(&view.generated_at.to_rfc3339())
    );
    for (label, value) in [
        ("Namespaces", view.summary.namespaces.to_string()),
        ("Total memories", view.summary.total_memories.to_string()),
        ("Active memories", view.summary.active_memories.to_string()),
        ("Contested", view.summary.contested_memories.to_string()),
        ("Pending review", view.summary.pending_reviews.to_string()),
        (
            "Policy probes",
            view.policy_firewall.actionable_probes.to_string(),
        ),
    ] {
        let _ = write!(
            html,
            "<div class=\"card\"><strong>{}</strong><span>{}</span></div>",
            html_escape(&value),
            html_escape(label)
        );
    }
    html.push_str("</div></section>");

    html.push_str("<section class=\"section\"><h2>Quick Actions</h2><div class=\"actions\">");
    for action in &view.quick_actions {
        let _ = write!(
            html,
            "<article class=\"action\"><div class=\"eyebrow\">{} {}</div><strong>{}</strong><small>{}</small><code>{}</code><code>{}</code></article>",
            html_escape(&action.method),
            html_escape(&action.path),
            html_escape(&action.label),
            html_escape(&action.description),
            html_escape(&action.path),
            html_escape(&action.cli)
        );
    }
    html.push_str("</div></section>");

    html.push_str("<section class=\"section\"><h2>Blocked By Policy</h2>");
    if let Some(counters) = &view.policy_firewall.live_counters {
        let _ = write!(
            html,
            "<div class=\"summary\"><div class=\"card\"><strong>{}</strong><span>Live blocks</span></div><div class=\"card\"><strong>{}</strong><span>Live confirmations</span></div><div class=\"card\"><strong>{}</strong><span>Live warnings</span></div></div>",
            counters.block, counters.require_confirmation, counters.warn
        );
    }
    html.push_str("<div class=\"policy-grid\">");
    for namespace in &view.namespaces {
        for probe in &namespace.policy_probes {
            let _ = write!(
                html,
                "<article class=\"policy\"><div class=\"eyebrow\">{}</div><strong>{}</strong><p class=\"decision-{}\">{}</p><small>{}</small>",
                html_escape(&namespace.namespace),
                html_escape(&probe.label),
                html_escape(&probe.decision),
                html_escape(&probe.decision),
                html_escape(&probe.query)
            );
            if !probe.risky_actions.is_empty() {
                for action in &probe.risky_actions {
                    let _ = write!(html, "<span class=\"pill\">{}</span>", html_escape(action));
                }
            }
            if probe.matched_policy_previews.is_empty() {
                html.push_str("<p><small>No matching policy memory.</small></p>");
            } else {
                html.push_str("<ul>");
                for preview in &probe.matched_policy_previews {
                    let _ = write!(html, "<li>{}</li>", html_escape(preview));
                }
                html.push_str("</ul>");
            }
            html.push_str("</article>");
        }
    }
    html.push_str("</div></section>");

    html.push_str("<section class=\"section\"><h2>Namespaces</h2><div class=\"namespaces\">");
    for namespace in &view.namespaces {
        let recent_counts = &namespace.recent["counts"];
        let _ = write!(
            html,
            "<article class=\"namespace\"><div class=\"eyebrow\">Namespace</div><h3>{}</h3><div class=\"meta\"><span class=\"pill\">total {}</span><span class=\"pill\">active {}</span><span class=\"pill\">contested {}</span><span class=\"pill\">pending review {}</span><span class=\"pill\">recent injections {}</span><span class=\"pill\">recent feedbacks {}</span></div><p><strong>Review queue:</strong> candidate={} contested={} rejected={}</p>",
            html_escape(&namespace.namespace),
            namespace.lifecycle.total,
            namespace.lifecycle.active,
            namespace.lifecycle.contested,
            namespace.review_queue.summary.pending,
            recent_counts["injections"].as_u64().unwrap_or(0),
            recent_counts["feedbacks"].as_u64().unwrap_or(0),
            namespace.review_queue.summary.candidate,
            namespace.review_queue.summary.contested,
            namespace.review_queue.summary.rejected,
        );
        if namespace.review_queue.items.is_empty() {
            html.push_str("<small>No queue items right now.</small>");
        } else {
            html.push_str("<ul>");
            for item in namespace.review_queue.items.iter().take(3) {
                let _ = write!(
                    html,
                    "<li><strong>{} -> {}</strong> {}<br><small>trust {:.2} | source {}</small></li>",
                    html_escape(&item.queue_kind.to_string()),
                    html_escape(&item.suggested_action.to_string()),
                    html_escape(&item.preview),
                    item.trust_score,
                    html_escape(&item.source)
                );
            }
            html.push_str("</ul>");
        }
        html.push_str("</article>");
    }
    html.push_str("</div></section></div></body></html>");

    html
}

fn bundle_attachment_name(
    kind: MemoryBundleKind,
    namespace: &str,
    scope: Option<&str>,
    bundle_id: uuid::Uuid,
) -> String {
    let scope_suffix = scope
        .filter(|scope| !scope.is_empty())
        .map(|scope| format!("-{scope}"))
        .unwrap_or_default();
    format!("memoryoss-{kind}-{namespace}{scope_suffix}-{bundle_id}.membundle.json")
}

fn build_memory_bundle_reference(
    kind: MemoryBundleKind,
    bundle_id: uuid::Uuid,
    namespace: &str,
    scope: Option<&str>,
) -> MemoryBundleReference {
    let mut uri = format!("memoryoss://bundle/{bundle_id}?kind={kind}&namespace={namespace}");
    if let Some(scope) = scope.filter(|scope| !scope.is_empty()) {
        uri.push_str("&scope=");
        uri.push_str(scope);
    }
    MemoryBundleReference {
        uri,
        attachment_name: bundle_attachment_name(kind, namespace, scope, bundle_id),
    }
}

fn memory_bundle_payload_sha256(bundle: &MemoryBundleEnvelope) -> String {
    use sha2::Digest as _;

    let payload = json!({
        "bundle_version": bundle.bundle_version,
        "bundle_id": bundle.bundle_id,
        "kind": bundle.kind,
        "namespace": bundle.namespace,
        "scope": bundle.scope,
        "runtime_contract_id": bundle.runtime_contract_id,
        "exported_at": bundle.exported_at,
        "reference": bundle.reference,
        "signature": bundle.signature,
        "payload": bundle.payload,
    });
    let bytes = serde_json::to_vec(&payload).expect("memory bundle payload should serialize");
    hex::encode(sha2::Sha256::digest(bytes))
}

pub(crate) fn build_memory_bundle_from_passport(
    bundle: MemoryPassportBundle,
) -> MemoryBundleEnvelope {
    let bundle_id = bundle.passport_id;
    let scope = bundle.scope.to_string();
    let reference = build_memory_bundle_reference(
        MemoryBundleKind::Passport,
        bundle_id,
        &bundle.namespace,
        Some(&scope),
    );
    let mut envelope = MemoryBundleEnvelope {
        bundle_version: MEMORY_BUNDLE_FORMAT_VERSION.to_string(),
        bundle_id,
        kind: MemoryBundleKind::Passport,
        namespace: bundle.namespace.clone(),
        scope: Some(scope),
        runtime_contract_id: bundle.runtime_contract.contract_id.clone(),
        exported_at: bundle.exported_at,
        reference,
        integrity: MemoryBundleIntegrity {
            algorithm: "sha256".to_string(),
            payload_sha256: String::new(),
        },
        signature: MemoryBundleSignature {
            scheme: "unsigned".to_string(),
            signer: None,
            signable: true,
        },
        payload: serde_json::to_value(bundle).expect("passport bundle should serialize"),
    };
    envelope.integrity.payload_sha256 = memory_bundle_payload_sha256(&envelope);
    envelope
}

pub(crate) fn build_memory_bundle_from_history(
    bundle: MemoryHistoryBundle,
) -> MemoryBundleEnvelope {
    let bundle_id = bundle.history_id;
    let reference = build_memory_bundle_reference(
        MemoryBundleKind::History,
        bundle_id,
        &bundle.namespace,
        None,
    );
    let mut envelope = MemoryBundleEnvelope {
        bundle_version: MEMORY_BUNDLE_FORMAT_VERSION.to_string(),
        bundle_id,
        kind: MemoryBundleKind::History,
        namespace: bundle.namespace.clone(),
        scope: None,
        runtime_contract_id: bundle.runtime_contract.contract_id.clone(),
        exported_at: bundle.exported_at,
        reference,
        integrity: MemoryBundleIntegrity {
            algorithm: "sha256".to_string(),
            payload_sha256: String::new(),
        },
        signature: MemoryBundleSignature {
            scheme: "unsigned".to_string(),
            signer: None,
            signable: true,
        },
        payload: serde_json::to_value(bundle).expect("history bundle should serialize"),
    };
    envelope.integrity.payload_sha256 = memory_bundle_payload_sha256(&envelope);
    envelope
}

fn invalid_memory_bundle_preview(bundle: &MemoryBundleEnvelope) -> MemoryBundlePreview {
    MemoryBundlePreview {
        bundle_id: bundle.bundle_id,
        bundle_version: bundle.bundle_version.clone(),
        kind: bundle.kind,
        namespace: bundle.namespace.clone(),
        scope: bundle.scope.clone(),
        runtime_contract_id: bundle.runtime_contract_id.clone(),
        uri: bundle.reference.uri.clone(),
        attachment_name: bundle.reference.attachment_name.clone(),
        memory_count: 0,
        visible_count: 0,
        root_id: None,
        integrity_valid: bundle.integrity.algorithm.eq_ignore_ascii_case("sha256")
            && bundle.integrity.payload_sha256 == memory_bundle_payload_sha256(bundle),
        nested_integrity_valid: false,
        preview: Vec::new(),
    }
}

pub(crate) fn preview_memory_bundle(
    bundle: &MemoryBundleEnvelope,
) -> Result<MemoryBundlePreview, String> {
    let integrity_valid = bundle.bundle_version == MEMORY_BUNDLE_FORMAT_VERSION
        && bundle.integrity.algorithm.eq_ignore_ascii_case("sha256")
        && bundle.integrity.payload_sha256 == memory_bundle_payload_sha256(bundle);
    match bundle.kind {
        MemoryBundleKind::Passport => {
            let passport: MemoryPassportBundle = serde_json::from_value(bundle.payload.clone())
                .map_err(|err| format!("invalid passport bundle payload: {err}"))?;
            let preview = passport
                .memories
                .iter()
                .take(5)
                .map(|memory| preview_text(&memory.content, 100))
                .collect();
            Ok(MemoryBundlePreview {
                bundle_id: bundle.bundle_id,
                bundle_version: bundle.bundle_version.clone(),
                kind: bundle.kind,
                namespace: bundle.namespace.clone(),
                scope: Some(passport.scope.to_string()),
                runtime_contract_id: bundle.runtime_contract_id.clone(),
                uri: bundle.reference.uri.clone(),
                attachment_name: bundle.reference.attachment_name.clone(),
                memory_count: passport.memories.len(),
                visible_count: passport.memories.len(),
                root_id: None,
                integrity_valid,
                nested_integrity_valid: verify_memory_passport_bundle(&passport),
                preview,
            })
        }
        MemoryBundleKind::History => {
            let history: MemoryHistoryBundle = serde_json::from_value(bundle.payload.clone())
                .map_err(|err| format!("invalid history bundle payload: {err}"))?;
            let preview = history
                .memories
                .iter()
                .take(5)
                .map(|memory| preview_text(&memory.content, 100))
                .collect();
            Ok(MemoryBundlePreview {
                bundle_id: bundle.bundle_id,
                bundle_version: bundle.bundle_version.clone(),
                kind: bundle.kind,
                namespace: bundle.namespace.clone(),
                scope: None,
                runtime_contract_id: bundle.runtime_contract_id.clone(),
                uri: bundle.reference.uri.clone(),
                attachment_name: bundle.reference.attachment_name.clone(),
                memory_count: history.memories.len(),
                visible_count: history.memories.len(),
                root_id: Some(history.root_id),
                integrity_valid,
                nested_integrity_valid: verify_memory_history_bundle(&history),
                preview,
            })
        }
    }
}

fn memory_bundle_entry_set(
    bundle: &MemoryBundleEnvelope,
) -> Result<std::collections::BTreeSet<String>, String> {
    match bundle.kind {
        MemoryBundleKind::Passport => {
            let passport: MemoryPassportBundle = serde_json::from_value(bundle.payload.clone())
                .map_err(|err| format!("invalid passport bundle payload: {err}"))?;
            Ok(passport
                .memories
                .into_iter()
                .map(|memory| memory.content)
                .collect())
        }
        MemoryBundleKind::History => {
            let history: MemoryHistoryBundle = serde_json::from_value(bundle.payload.clone())
                .map_err(|err| format!("invalid history bundle payload: {err}"))?;
            Ok(history
                .memories
                .into_iter()
                .map(|memory| memory.content)
                .collect())
        }
    }
}

pub(crate) fn validate_memory_bundle(bundle: &MemoryBundleEnvelope) -> MemoryBundleValidation {
    let mut errors = Vec::new();
    if bundle.bundle_version != MEMORY_BUNDLE_FORMAT_VERSION {
        errors.push(format!(
            "unsupported bundle version: {}",
            bundle.bundle_version
        ));
    }
    let preview = match preview_memory_bundle(bundle) {
        Ok(preview) => preview,
        Err(err) => {
            errors.push(err);
            invalid_memory_bundle_preview(bundle)
        }
    };
    if !preview.integrity_valid {
        errors.push("bundle envelope integrity check failed".to_string());
    }
    if !preview.nested_integrity_valid {
        errors.push("nested artifact integrity check failed".to_string());
    }

    MemoryBundleValidation {
        valid: errors.is_empty(),
        preview,
        errors,
    }
}

pub(crate) fn diff_memory_bundles(
    left: &MemoryBundleEnvelope,
    right: &MemoryBundleEnvelope,
) -> Result<MemoryBundleDiff, String> {
    let left_preview = preview_memory_bundle(left)?;
    let right_preview = preview_memory_bundle(right)?;
    let left_entries = memory_bundle_entry_set(left)?;
    let right_entries = memory_bundle_entry_set(right)?;

    let mut changed_fields = Vec::new();
    if left.kind != right.kind {
        changed_fields.push("kind".to_string());
    }
    if left.namespace != right.namespace {
        changed_fields.push("namespace".to_string());
    }
    if left.scope != right.scope {
        changed_fields.push("scope".to_string());
    }
    if left.runtime_contract_id != right.runtime_contract_id {
        changed_fields.push("runtime_contract_id".to_string());
    }
    if left_preview.memory_count != right_preview.memory_count {
        changed_fields.push("memory_count".to_string());
    }
    if left_preview.visible_count != right_preview.visible_count {
        changed_fields.push("visible_count".to_string());
    }

    Ok(MemoryBundleDiff {
        same_kind: left.kind == right.kind,
        left: left_preview,
        right: right_preview,
        changed_fields,
        added_preview: right_entries
            .difference(&left_entries)
            .take(5)
            .map(|entry| preview_text(entry, 100))
            .collect(),
        removed_preview: left_entries
            .difference(&right_entries)
            .take(5)
            .map(|entry| preview_text(entry, 100))
            .collect(),
        shared_preview_count: left_entries.intersection(&right_entries).count(),
    })
}

/// AGPL-3.0 Section 13 compliance: provide source code location to network users.
async fn agpl_source() -> Response {
    Json(json!({
        "license": "AGPL-3.0-only",
        "source": "https://github.com/memoryOSScom/memoryoss",
        "notice": "This software is licensed under the GNU Affero General Public License v3.0. \
                   If you interact with this software over a network, you are entitled to receive \
                   the Corresponding Source. Visit the URL above to obtain it."
    }))
    .into_response()
}

#[derive(serde::Deserialize)]
pub struct AuthTokenRequest {
    pub api_key: String,
}

#[derive(serde::Serialize)]
pub struct AuthTokenResponse {
    pub token: String,
    pub expires_in: u64,
}

async fn auth_token(
    State(state): State<AppState>,
    Json(req): Json<AuthTokenRequest>,
) -> Result<Response, AppError> {
    let entry = find_api_key(&state.config, &req.api_key)
        .ok_or(AppError::Unauthorized("invalid API key"))?;

    let token = create_jwt(&state.config, entry)?;

    Ok(Json(AuthTokenResponse {
        token,
        expires_in: state.config.auth.jwt_expiry_secs,
    })
    .into_response())
}

fn require_auth(config: &Config, parts: &Parts) -> Result<Claims, AppError> {
    if config.auth.api_keys.is_empty() {
        if config.dev_mode {
            return Ok(Claims {
                sub: "dev".to_string(),
                role: Role::Admin,
                namespace: "default".to_string(),
                exp: 0,
                iat: 0,
                iss: Some("memoryoss".to_string()),
                aud: Some("memoryoss".to_string()),
            });
        }
        return Err(AppError::Unauthorized(
            "no API keys configured — set auth.api_keys in config (or use `memoryoss dev` for development)",
        ));
    }

    let token = extract_bearer_token(parts)
        .ok_or(AppError::Unauthorized("missing Authorization header"))?;

    // Try JWT first, then fall back to raw API key lookup.
    // This lets clients use either a JWT token or the API key directly —
    // no token exchange required for simple use cases.
    if let Ok(claims) = validate_jwt(config, token) {
        return Ok(claims);
    }

    if let Some(entry) = find_api_key(config, token) {
        return Ok(Claims {
            sub: crate::security::auth::key_id(&entry.key),
            role: entry.role,
            namespace: entry.namespace.clone(),
            exp: 0,
            iat: 0,
            iss: Some("memoryoss".to_string()),
            aud: Some("memoryoss".to_string()),
        });
    }

    Err(AppError::Unauthorized("invalid or expired token"))
}

/// Check IP allowlist for the request's namespace (F-06).
/// Extracts client IP from X-Forwarded-For header.
/// Returns Ok(()) if allowed or header absent; Err if denied.
fn check_ip_allowlist(state: &SharedState, parts: &Parts, namespace: &str) -> Result<(), AppError> {
    // B2 FIX: use ConnectInfo socket addr as primary, X-Forwarded-For only as fallback
    let socket_ip = parts
        .extensions
        .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
        .map(|ci| ci.0.ip());
    let xff_ip = parts
        .headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(',').next())
        .and_then(|s| s.trim().parse::<std::net::IpAddr>().ok());
    // Prefer socket IP (non-spoofable), fall back to XFF only if socket unavailable
    let client_ip = socket_ip.or(xff_ip);

    if let Some(ip) = client_ip {
        if !state.trust_scorer.check_ip(namespace, &ip) {
            tracing::warn!(ip = %ip, namespace, "IP denied by allowlist");
            return Err(AppError::Forbidden("IP not in namespace allowlist"));
        }
    } else if state.trust_scorer.has_ip_allowlist(namespace) {
        // B2 FIX: fail-closed when allowlist exists but no IP can be determined
        return Err(AppError::Forbidden(
            "cannot determine client IP for allowlist check",
        ));
    }
    Ok(())
}

fn check_read_rate_limit(state: &SharedState, claims: &Claims) -> Result<(), AppError> {
    if let Err(retry_ms) = state.rate_limiter.check(&claims.sub) {
        return Err(AppError::RateLimited(retry_ms));
    }
    Ok(())
}

/// Enforce namespace scoping: non-admin users can only access their own namespace.
fn enforce_namespace<'a>(
    claims: &'a Claims,
    requested: Option<&'a str>,
) -> Result<&'a str, AppError> {
    let ns = requested.unwrap_or(&claims.namespace);
    // All roles (including admin) are scoped to their own namespace.
    // Cross-namespace access is only possible via sharing grants.
    if ns != claims.namespace {
        return Err(AppError::Forbidden(
            "cannot access namespace outside your scope",
        ));
    }
    Ok(ns)
}

fn validate_client_embedding(state: &SharedState, embedding: &[f32]) -> Result<(), AppError> {
    let expected_dim = state.embedding.dimension();
    if embedding.len() != expected_dim {
        return Err(AppError::BadRequest(format!(
            "embedding dimension mismatch: got {}, expected {}",
            embedding.len(),
            expected_dim
        )));
    }
    if embedding.iter().any(|x| x.is_nan() || x.is_infinite()) {
        return Err(AppError::BadRequest(
            "embedding contains NaN or Inf values".into(),
        ));
    }
    Ok(())
}

fn ensure_no_hash_duplicate(
    state: &SharedState,
    namespace: &str,
    content_hash: Option<&str>,
    prepared: &[Memory],
) -> Result<(), AppError> {
    let Some(hash) = content_hash else {
        return Ok(());
    };

    if let Ok(existing) = state.doc_engine.find_by_hash(namespace, hash)
        && let Some(dup) = existing
    {
        return Err(AppError::BadRequest(format!(
            "duplicate content (matches memory {})",
            dup
        )));
    }

    if let Some(dup) = prepared.iter().find(|memory| {
        memory.namespace.as_deref() == Some(namespace)
            && memory.content_hash.as_deref() == Some(hash)
    }) {
        return Err(AppError::BadRequest(format!(
            "duplicate content (matches batch memory {})",
            dup.id
        )));
    }

    Ok(())
}

fn ensure_no_semantic_duplicate(
    state: &SharedState,
    namespace: &str,
    embedding: &[f32],
    prepared: &[Memory],
) -> Result<(), AppError> {
    let threshold = state.config.trust.semantic_dedup_threshold;

    if let Ok(nearest) = state.vector_engine.search(embedding, 1)
        && let Some((existing_id, score)) = nearest.first()
        && (*score as f64) >= threshold
    {
        if let Ok(Some(existing)) = state.doc_engine.get(*existing_id, namespace)
            && let Some(existing_embedding) = existing.embedding.as_ref()
        {
            let exact_similarity = cosine_similarity(existing_embedding, embedding);
            if exact_similarity >= threshold {
                return Err(AppError::BadRequest(format!(
                    "semantic duplicate (similarity {:.3} >= {:.3}, matches memory {})",
                    exact_similarity, threshold, existing_id
                )));
            }
        }
    }

    if let Some(dup) = prepared.iter().find(|memory| {
        memory.namespace.as_deref() == Some(namespace)
            && memory
                .embedding
                .as_ref()
                .is_some_and(|existing| cosine_similarity(existing, embedding) >= threshold)
    }) {
        let similarity = dup
            .embedding
            .as_ref()
            .map(|existing| cosine_similarity(existing, embedding))
            .unwrap_or(0.0);
        return Err(AppError::BadRequest(format!(
            "semantic duplicate (similarity {:.3} >= {:.3}, matches batch memory {})",
            similarity, threshold, dup.id
        )));
    }

    Ok(())
}

#[derive(Debug, Deserialize)]
struct CreateTokenRequest {
    role: Role,
    namespace: String,
    #[serde(default = "default_token_expiry")]
    expires_in_secs: u64,
}

fn default_token_expiry() -> u64 {
    86400
} // 24h

/// Admin-only: create scoped JWT tokens with specific role and namespace.
async fn create_scoped_token(
    State(state): State<AppState>,
    parts: Parts,
    Json(req): Json<CreateTokenRequest>,
) -> Result<Response, AppError> {
    let claims = require_auth(&state.config, &parts)?;
    if !rbac::can_admin(claims.role) {
        return Err(AppError::Forbidden("admin required to create tokens"));
    }

    // Namespace isolation: admins can only create tokens for their own namespace.
    // This prevents cross-tenant token forging.
    if req.namespace != claims.namespace {
        return Err(AppError::Forbidden(
            "cannot create tokens for a namespace outside your scope",
        ));
    }

    // Scoped tokens cannot escalate privileges
    if req.role == Role::Admin && claims.role != Role::Admin {
        return Err(AppError::Forbidden("cannot escalate to admin role"));
    }

    let now = chrono::Utc::now().timestamp() as usize;
    let scoped_claims = Claims {
        sub: format!("scoped:{}", req.namespace),
        role: req.role,
        namespace: req.namespace,
        exp: now + req.expires_in_secs as usize,
        iat: now,
        iss: Some("memoryoss".to_string()),
        aud: Some("memoryoss".to_string()),
    };

    let token = jsonwebtoken::encode(
        &jsonwebtoken::Header::default(),
        &scoped_claims,
        &jsonwebtoken::EncodingKey::from_secret(state.config.auth.jwt_secret.as_bytes()),
    )
    .map_err(|e| AppError::Internal(anyhow::anyhow!("JWT encode: {e}")))?;

    Ok(Json(json!({
        "token": token,
        "role": req.role,
        "namespace": scoped_claims.namespace,
        "expires_in": req.expires_in_secs,
    }))
    .into_response())
}

async fn store(
    State(state): State<AppState>,
    parts: Parts,
    Json(req): Json<StoreRequest>,
) -> Result<Response, AppError> {
    let claims = require_auth(&state.config, &parts)?;
    if !rbac::can_store(claims.role) {
        return Err(AppError::Forbidden("insufficient permissions for store"));
    }

    // Rate limiting
    if let Err(retry_ms) = state.rate_limiter.check(&claims.sub) {
        return Err(AppError::RateLimited(retry_ms));
    }

    // Backpressure: if indexer lag > threshold, reject writes
    let lag = state.indexer_state.lag();
    if lag > BACKPRESSURE_THRESHOLD {
        return Err(AppError::RateLimited(1000)); // retry in 1s
    }

    // Input validation
    let limits = &state.config.limits;
    validation::validate_content(&req.content, limits)
        .map_err(|e| AppError::BadRequest(e.to_string()))?;
    validation::validate_tags(&req.tags, limits)
        .map_err(|e| AppError::BadRequest(e.to_string()))?;
    let namespace_raw = enforce_namespace(&claims, req.namespace.as_deref())?.to_string();
    validation::validate_namespace(&namespace_raw, limits)
        .map_err(|e| AppError::BadRequest(e.to_string()))?;

    let zero_knowledge = req.zero_knowledge;

    // Zero-knowledge mode: client must provide embedding
    if zero_knowledge && req.embedding.is_none() {
        return Err(AppError::BadRequest(
            "zero_knowledge mode requires client-provided embedding".into(),
        ));
    }

    let mut memory = Memory::new(req.content);
    memory.tags = req.tags;
    memory.agent = req.agent;
    memory.session = req.session;
    memory.namespace = Some(namespace_raw);
    memory.memory_type = req.memory_type;
    memory.source_key = Some(claims.sub.clone());

    let namespace = memory
        .namespace
        .clone()
        .unwrap_or_else(|| "default".to_string());

    // IP allowlist enforcement (F-06)
    check_ip_allowlist(&state, &parts, &namespace)?;

    if zero_knowledge {
        // Zero-knowledge: use client-provided embedding, skip content hash dedup
        if let Some(ref emb) = req.embedding {
            validate_client_embedding(&state, emb)?;
        }
        memory.embedding = req.embedding;
        // Clear content hash — ciphertext shouldn't be deduped by hash
        memory.content_hash = None;
    } else {
        // Normal mode: content hash dedup
        ensure_no_hash_duplicate(&state, &namespace, memory.content_hash.as_deref(), &[])?;

        // Generate embedding server-side
        let embedding = state.embedding.embed_one(&memory.content).await?;
        memory.embedding = Some(embedding);
    }

    // Semantic dedup: check if embedding is near-duplicate of existing memory
    if let Some(ref emb) = memory.embedding {
        ensure_no_semantic_duplicate(&state, &namespace, emb, &[])?;
    }

    let contradiction_updates =
        apply_contradiction_detection(&state, &namespace, &mut memory, &claims.sub, &[])?;
    if contradiction_updates > 0 {
        state.indexer_state.write_seq.fetch_add(
            contradiction_updates as u64,
            std::sync::atomic::Ordering::Relaxed,
        );
        state.indexer_state.wake();
    }

    // Update trust scorer centroid with new embedding
    if let Some(ref emb) = memory.embedding {
        state.trust_scorer.update_centroid(emb, &namespace);
        state
            .trust_scorer
            .record_access(memory.id, memory.source_key.as_deref());
    }

    // Group commit: batches writes into single redb TX for throughput
    state
        .group_committer
        .store(memory.clone(), claims.sub.clone())
        .await?;
    refresh_review_queue_summary(&state, &namespace)?;

    // Invalidate intent cache for this namespace (data changed)
    state.intent_cache.invalidate_namespace(&namespace).await;

    state
        .metrics
        .stores
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    tracing::info!(
        memory_id = %memory.id,
        namespace = %namespace,
        "stored memory"
    );

    // Fire webhooks for any shared namespaces this namespace belongs to
    if let Ok(shared_nss) = state.sharing_store.accessible_namespaces(&namespace) {
        for sns in &shared_nss {
            state.sharing_store.fire_webhook(sns, memory.id);
        }
    }

    Ok(Json(StoreResponse {
        id: memory.id,
        version: memory.version,
    })
    .into_response())
}

async fn store_batch(
    State(state): State<AppState>,
    parts: Parts,
    Json(req): Json<BatchStoreRequest>,
) -> Result<Response, AppError> {
    let claims = require_auth(&state.config, &parts)?;
    if !rbac::can_store(claims.role) {
        return Err(AppError::Forbidden("insufficient permissions for store"));
    }

    // Rate limiting (same as single store)
    if let Err(retry_ms) = state.rate_limiter.check(&claims.sub) {
        return Err(AppError::RateLimited(retry_ms));
    }

    // Backpressure: if indexer lag > threshold, reject writes
    let lag = state.indexer_state.lag();
    if lag > BACKPRESSURE_THRESHOLD {
        return Err(AppError::RateLimited(1000));
    }

    if req.memories.is_empty() {
        return Err(AppError::BadRequest("empty batch".to_string()));
    }
    if req.memories.len() > 1000 {
        return Err(AppError::BadRequest(format!(
            "batch too large ({} > 1000)",
            req.memories.len()
        )));
    }

    let limits = &state.config.limits;

    // Validate all items first and collect namespaces/embedding work.
    let mut items = Vec::with_capacity(req.memories.len());
    let mut contents_to_embed = Vec::new();
    let mut touched_namespaces = std::collections::HashSet::new();
    for item in req.memories {
        validation::validate_content(&item.content, limits)
            .map_err(|e| AppError::BadRequest(e.to_string()))?;
        validation::validate_tags(&item.tags, limits)
            .map_err(|e| AppError::BadRequest(e.to_string()))?;
        let item_ns = enforce_namespace(&claims, item.namespace.as_deref())?.to_string();
        crate::validation::validate_namespace(&item_ns, &state.config.limits)
            .map_err(|e| AppError::BadRequest(e.to_string()))?;
        if !item.zero_knowledge {
            contents_to_embed.push(item.content.clone());
        } else if let Some(ref embedding) = item.embedding {
            validate_client_embedding(&state, embedding)?;
        } else {
            return Err(AppError::BadRequest(
                "zero_knowledge mode requires client-provided embedding".into(),
            ));
        }
        touched_namespaces.insert(item_ns.clone());
        items.push((item, item_ns));
    }

    // Embed only the items that require server-side embeddings.
    let mut generated_embeddings = state.embedding.embed(contents_to_embed).await?.into_iter();

    // IP allowlist enforcement (F-06)
    for item_ns in &touched_namespaces {
        check_ip_allowlist(&state, &parts, item_ns)?;
    }

    let mut stored = Vec::with_capacity(items.len());
    let mut prepared_memories = Vec::with_capacity(items.len());
    let mut existing_by_namespace =
        std::collections::HashMap::with_capacity(touched_namespaces.len());
    for item_ns in &touched_namespaces {
        existing_by_namespace.insert(
            item_ns.clone(),
            state.doc_engine.list_all_including_archived(item_ns)?,
        );
    }
    let mut dirty_existing_ids =
        std::collections::HashMap::<String, std::collections::HashSet<uuid::Uuid>>::new();
    let mut contradiction_updates = 0usize;

    for (item, item_ns) in items {
        let mut memory = Memory::new(item.content);
        memory.tags = item.tags;
        memory.agent = item.agent;
        memory.session = item.session;
        memory.namespace = Some(item_ns.clone());
        memory.memory_type = item.memory_type;
        memory.source_key = Some(claims.sub.clone());

        if item.zero_knowledge {
            memory.embedding = item.embedding;
            memory.content_hash = None;
        } else {
            ensure_no_hash_duplicate(
                &state,
                &item_ns,
                memory.content_hash.as_deref(),
                &prepared_memories,
            )?;
            memory.embedding = Some(generated_embeddings.next().ok_or_else(|| {
                AppError::Internal(anyhow::anyhow!(
                    "batch embedding generation produced fewer vectors than expected"
                ))
            })?);
        }

        if let Some(ref emb) = memory.embedding {
            ensure_no_semantic_duplicate(&state, &item_ns, emb, &prepared_memories)?;
        }

        let skip_ids: Vec<_> = prepared_memories.iter().map(|memory| memory.id).collect();
        if let Some(existing_memories) = existing_by_namespace.get_mut(&item_ns) {
            let changed_ids =
                apply_loaded_contradiction_detection(existing_memories, &mut memory, &skip_ids);
            contradiction_updates += changed_ids.len();
            if !changed_ids.is_empty() {
                dirty_existing_ids
                    .entry(item_ns.clone())
                    .or_default()
                    .extend(changed_ids);
            }
        }
        for prepared in prepared_memories
            .iter_mut()
            .filter(|prepared| prepared.namespace.as_deref() == Some(item_ns.as_str()))
        {
            if memories_contradict(&memory, prepared) {
                if prepared.mark_contradicted_by(memory.id) {
                    prepared.updated_at = chrono::Utc::now();
                    prepared.version += 1;
                }
                memory.mark_contradicted_by(prepared.id);
            }
        }

        // Trust scoring (consistent with single store)
        if let Some(ref emb) = memory.embedding {
            state.trust_scorer.update_centroid(emb, &item_ns);
            state
                .trust_scorer
                .record_access(memory.id, memory.source_key.as_deref());
        }

        stored.push(StoreResponse {
            id: memory.id,
            version: memory.version,
        });
        prepared_memories.push(memory);
    }

    if generated_embeddings.next().is_some() {
        return Err(AppError::Internal(anyhow::anyhow!(
            "batch embedding generation produced more vectors than expected"
        )));
    }

    for (namespace, changed_ids) in &dirty_existing_ids {
        let Some(existing_memories) = existing_by_namespace.get(namespace) else {
            continue;
        };
        for existing in existing_memories
            .iter()
            .filter(|existing| changed_ids.contains(&existing.id))
        {
            state.doc_engine.replace(existing, &claims.sub)?;
        }
    }

    let batch_items: Vec<(Memory, String)> = prepared_memories
        .iter()
        .cloned()
        .map(|memory| (memory, claims.sub.clone()))
        .collect();
    state.doc_engine.store_batch_tx(&batch_items)?;
    state.indexer_state.write_seq.fetch_add(
        (batch_items.len() + contradiction_updates) as u64,
        std::sync::atomic::Ordering::Relaxed,
    );

    // Wake indexer pipeline once for the whole batch
    state.indexer_state.wake();

    // Invalidate intent cache for affected namespace (consistent with single store)
    for touched in &touched_namespaces {
        refresh_review_queue_summary(&state, touched)?;
        state.intent_cache.invalidate_namespace(touched).await;
    }

    tracing::info!(count = stored.len(), "batch stored memories");

    // Fire webhooks for any shared namespaces touched by this batch
    for touched in &touched_namespaces {
        if let Ok(shared_nss) = state.sharing_store.accessible_namespaces(touched) {
            for sns in &shared_nss {
                for item in &stored {
                    state.sharing_store.fire_webhook(sns, item.id);
                }
            }
        }
    }

    Ok(Json(BatchStoreResponse { stored }).into_response())
}

async fn recall(
    State(state): State<AppState>,
    parts: Parts,
    Json(req): Json<RecallRequest>,
) -> Result<Response, AppError> {
    let claims = require_auth(&state.config, &parts)?;
    if !rbac::can_recall(claims.role) {
        return Err(AppError::Forbidden("insufficient permissions for recall"));
    }
    check_read_rate_limit(&state, &claims)?;

    // IP allowlist enforcement (F-06)
    let ns_for_ip = req.namespace.as_deref().unwrap_or(&claims.namespace);
    check_ip_allowlist(&state, &parts, ns_for_ip)?;

    let resp = recall_inner(&state, &claims, req).await?;
    Ok(Json(resp).into_response())
}

/// Bulk recall: execute multiple recall queries in parallel, sharing locks.
async fn recall_batch(
    State(state): State<AppState>,
    parts: Parts,
    Json(req): Json<BatchRecallRequest>,
) -> Result<Response, AppError> {
    let claims = require_auth(&state.config, &parts)?;
    if !rbac::can_recall(claims.role) {
        return Err(AppError::Forbidden("insufficient permissions for recall"));
    }
    check_read_rate_limit(&state, &claims)?;

    // IP allowlist enforcement (F-06)
    check_ip_allowlist(&state, &parts, &claims.namespace)?;

    if req.queries.is_empty() {
        return Err(AppError::BadRequest(
            "queries array must not be empty".into(),
        ));
    }
    if req.queries.len() > 10 {
        return Err(AppError::BadRequest(
            "batch recall limited to 10 queries".into(),
        ));
    }

    // Spawn all queries in parallel
    let mut handles = Vec::with_capacity(req.queries.len());
    for query in req.queries {
        let st = state.clone();
        let cl = claims.clone();
        handles.push(tokio::spawn(
            async move { recall_inner(&st, &cl, query).await },
        ));
    }

    // Collect results, preserving order
    let mut results = Vec::with_capacity(handles.len());
    for handle in handles {
        let result = handle
            .await
            .map_err(|e| AppError::Internal(anyhow::anyhow!("batch recall task failed: {e}")))?;
        results.push(result?);
    }

    Ok(Json(BatchRecallResponse { results }).into_response())
}

/// Core recall logic used by both single and batch endpoints.
async fn recall_inner(
    state: &AppState,
    claims: &Claims,
    mut req: RecallRequest,
) -> Result<RecallResponse, AppError> {
    // B6 FIX: clamp limit early to prevent integer overflow in overfetch calculations
    req.limit = req.limit.min(100);

    // Input validation
    let limits = &state.config.limits;
    validation::validate_query(&req.query, limits)
        .map_err(|e| AppError::BadRequest(e.to_string()))?;
    validation::validate_tags(&req.tags, limits)
        .map_err(|e| AppError::BadRequest(e.to_string()))?;
    if let Some(ref ns) = req.namespace {
        validation::validate_namespace(ns, limits)
            .map_err(|e| AppError::BadRequest(e.to_string()))?;
    }

    let namespace = enforce_namespace(claims, req.namespace.as_deref())?;
    let eventual = req.consistency.as_deref() == Some("eventual");

    // Session pre-fetching: record query pattern and pre-warm cache for new sessions
    if let Some(ref agent) = req.agent {
        state.prefetcher.record_query(agent, &req.query).await;
        if let Some(ref session) = req.session {
            state
                .prefetcher
                .maybe_prefetch(agent, session, &state.embedding)
                .await;
        }
    }

    let task_context = crate::scoring::detect_task_context(&req.query);
    let identifier_route = if state.config.proxy.identifier_first_routing {
        crate::scoring::detect_identifier_route(&req.query)
    } else {
        None
    };

    // Intent cache: check for cached results (canonical query matching)
    if req.cursor.is_none()
        && let Some(cached) = state
            .intent_cache
            .get(
                &req.query,
                req.session.as_deref(),
                namespace,
                req.agent.as_deref(),
                &req.tags,
                task_context.as_ref().map(|ctx| ctx.label()),
            )
            .await
    {
        let summaries = crate::fusion::build_scored_memory_summaries(&cached);
        return Ok(RecallResponse {
            memories: cached,
            summaries,
            next_cursor: None,
        });
    }

    // Decomposition: auto-partition large namespaces (depth-1 only)
    // Uses LLM-powered decomposition if configured, otherwise heuristic
    if !req.has_filters()
        && let Ok(Some(decomposed)) = crate::decompose::decomposed_recall(
            &state.doc_engine,
            &state.vector_engine,
            &state.fts_engine,
            &state.embedding,
            &state.idf_index,
            &state.space_index,
            namespace,
            &req.query,
            req.limit.min(100),
            &state.config.decompose,
        )
        .await
    {
        let summaries = crate::fusion::build_scored_memory_summaries(&decomposed);
        return Ok(RecallResponse {
            memories: decomposed,
            summaries,
            next_cursor: None,
        });
    }

    // Version-fence: check each index independently (use whichever is caught up)
    let write_seq = state
        .indexer_state
        .write_seq
        .load(std::sync::atomic::Ordering::Relaxed);
    let vector_seq = state
        .indexer_state
        .vector_seq
        .load(std::sync::atomic::Ordering::Relaxed);
    let fts_seq = state
        .indexer_state
        .fts_seq
        .load(std::sync::atomic::Ordering::Relaxed);
    let use_vector = eventual || vector_seq >= write_seq;
    let use_fts = eventual || fts_seq >= write_seq;

    // Pre-filter: if metadata filters are active, use FTS metadata index (O(log n))
    // instead of full redb table scan (O(n)). Fall back to redb if FTS is behind.
    let candidate_ids: Option<std::collections::HashSet<uuid::Uuid>> = if req.has_filters() {
        if use_fts {
            // Fast path: tantivy boolean query on structured fields
            let mt_str = req.memory_type.map(|mt| format!("{:?}", mt));
            let fts_candidates = state
                .fts_engine
                .search_metadata(
                    req.agent.as_deref(),
                    req.session.as_deref(),
                    mt_str.as_deref(),
                    &req.tags,
                    req.limit * 10,
                )
                .unwrap_or_default();
            // Time range filters still need redb lookup for created_at
            let filtered: std::collections::HashSet<uuid::Uuid> =
                if req.before.is_some() || req.after.is_some() {
                    fts_candidates
                        .into_iter()
                        .filter(|id| {
                            state
                                .doc_engine
                                .get(*id, namespace)
                                .ok()
                                .flatten()
                                .map(|m| {
                                    req.before.as_ref().is_none_or(|b| m.created_at < *b)
                                        && req.after.as_ref().is_none_or(|a| m.created_at > *a)
                                })
                                .unwrap_or(false)
                        })
                        .collect()
                } else {
                    fts_candidates.into_iter().collect()
                };
            Some(filtered)
        } else {
            // Slow fallback: O(n) redb scan (FTS index not caught up)
            let candidates = state.doc_engine.search(
                namespace,
                req.agent.as_deref(),
                req.session.as_deref(),
                req.memory_type,
                &req.tags,
            )?;
            let filtered: std::collections::HashSet<uuid::Uuid> = candidates
                .into_iter()
                .filter(|m| {
                    req.before.as_ref().is_none_or(|b| m.created_at < *b)
                        && req.after.as_ref().is_none_or(|a| m.created_at > *a)
                })
                .map(|m| m.id)
                .collect();
            Some(filtered)
        }
    } else {
        None
    };

    // Generate query embedding (use client-provided for zero-knowledge mode)
    let query_embedding = if let Some(ref emb) = req.query_embedding {
        emb.clone()
    } else {
        state.embedding.embed_one(&req.query).await?
    };

    // Search: use each index independently if caught up, fall back to redb only if both behind
    // Overfetch more when pre-filtering (candidates may be sparse in index results)
    let overfetch = if candidate_ids.is_some() {
        req.limit * 5
    } else {
        req.limit * 2
    };
    let (vector_results, fts_results) = {
        let mut vr = if use_vector {
            state.vector_engine.search(&query_embedding, overfetch)?
        } else {
            tracing::debug!(
                vector_seq,
                write_seq,
                "version-fence: vector index behind, skipping"
            );
            Vec::new()
        };
        let mut fr = if use_fts {
            state.fts_engine.search(&req.query, overfetch)?
        } else {
            tracing::debug!(
                fts_seq,
                write_seq,
                "version-fence: FTS index behind, skipping"
            );
            Vec::new()
        };
        // Pre-filter: only keep results that match candidate set
        if let Some(ref ids) = candidate_ids {
            vr.retain(|(id, _)| ids.contains(id));
            fr.retain(|(id, _)| ids.contains(id));
        }
        (vr, fr)
    };

    // Scoring weights: configurable per query (clamped to [0.0, 1.0])
    let weights = req.weights.unwrap_or_default().clamped();

    // GrepRAG: exact/identifier match as 3rd channel
    let identifiers = crate::scoring::extract_identifiers(&req.query);
    let exact_results = if use_fts && !identifiers.is_empty() {
        crate::scoring::exact_match_search(&state.fts_engine, &identifiers, overfetch)
    } else {
        Vec::new()
    };

    // IDF boost (RLM: rare terms score higher)
    let idf_boost = crate::scoring::compute_idf_boost(&state.idf_index, &req.query);

    // Use shared score_and_merge core
    let options = crate::scoring::MergeOptions {
        weights,
        idf_boost,
        min_channel_score: state.config.proxy.min_channel_score.unwrap_or(0.0),
        apply_confidence_penalty: false,
        apply_trust_scoring: true,
        namespace: namespace.to_string(),
        limit: req.limit.min(100) * 2, // overfetch for post-filtering
        agent_filter: req.agent.clone(),
        diversity_factor: state.config.proxy.diversity_factor.unwrap_or(0.0),
        task_context: task_context.clone(),
        identifier_route: identifier_route.clone(),
    };

    let mut scored = crate::scoring::score_and_merge(
        &vector_results,
        &fts_results,
        &exact_results,
        &state.doc_engine,
        Some(&state.trust_scorer),
        &options,
    );

    // Fallback: if no results and both indexes were behind, scan redb directly
    if scored.is_empty() && !use_vector && !use_fts {
        let all = state.doc_engine.search(
            namespace,
            req.agent.as_deref(),
            req.session.as_deref(),
            req.memory_type,
            &req.tags,
        )?;
        for memory in all {
            let sim = if let Some(ref emb) = memory.embedding {
                cosine_similarity(&query_embedding, emb).max(0.0) * 0.3
            } else {
                0.1
            };
            let trust = memory.recency_trust();
            scored.push(ScoredMemory {
                memory,
                score: sim,
                provenance: vec!["redb_fallback".to_string()],
                trust_score: trust,
                low_trust: trust < 0.3,
            });
        }
        scored.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
    }

    // Apply additional API-specific filters (score_and_merge handles agent_filter)
    scored.retain(|sm| {
        if let Some(ref session) = req.session
            && sm.memory.session.as_deref() != Some(session)
        {
            return false;
        }
        if let Some(mt) = req.memory_type
            && sm.memory.memory_type != mt
        {
            return false;
        }
        if !req.tags.is_empty() && !req.tags.iter().any(|t| sm.memory.tags.contains(t)) {
            return false;
        }
        if let Some(ref before) = req.before
            && sm.memory.created_at >= *before
        {
            return false;
        }
        if let Some(ref after) = req.after
            && sm.memory.created_at <= *after
        {
            return false;
        }
        true
    });

    scored = crate::fusion::collapse_scored_memories_for_query(scored, identifier_route.as_ref());

    // Cursor pagination: skip results before cursor position
    if let Some(ref cursor) = req.cursor
        && let Some((cursor_score, cursor_id)) = decode_cursor(cursor)
    {
        scored.retain(|sm| {
            sm.score < cursor_score || (sm.score == cursor_score && sm.memory.id < cursor_id)
        });
    }

    // Clamp page size: default 20, max 100
    let page_size = req.limit.min(100);
    let has_more = scored.len() > page_size;
    scored.truncate(page_size);

    // Generate next_cursor from last item
    let next_cursor = if has_more {
        scored
            .last()
            .map(|sm| encode_cursor(sm.score, sm.memory.id))
    } else {
        None
    };

    // Cache results for future identical-intent queries (first page only)
    if req.cursor.is_none() && next_cursor.is_none() {
        state
            .intent_cache
            .put(
                &req.query,
                req.session.as_deref(),
                namespace,
                req.agent.as_deref(),
                &req.tags,
                task_context.as_ref().map(|ctx| ctx.label()),
                scored.clone(),
            )
            .await;
    }

    state
        .metrics
        .recalls
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let summaries = crate::fusion::build_scored_memory_summaries(&scored);
    Ok(RecallResponse {
        memories: scored,
        summaries,
        next_cursor,
    })
}

/// Auto-consolidation: find and merge semantically similar memories.
async fn consolidate(
    State(state): State<AppState>,
    parts: Parts,
    Json(req): Json<ConsolidateRequest>,
) -> Result<Response, AppError> {
    let claims = require_auth(&state.config, &parts)?;
    if claims.role != Role::Admin {
        return Err(AppError::Forbidden("consolidation requires admin role"));
    }

    let namespace = enforce_namespace(&claims, req.namespace.as_deref())?;

    if req.threshold < 0.0 || req.threshold > 1.0 {
        return Err(AppError::BadRequest(
            "threshold must be between 0.0 and 1.0".into(),
        ));
    }
    let response = run_namespace_consolidation(
        &state,
        namespace,
        req.threshold as f64,
        req.max_clusters,
        req.dry_run,
        &claims.sub,
    )
    .await?;
    Ok(Json(response).into_response())
}

pub(crate) fn apply_feedback_to_memory(
    memory: &mut Memory,
    action: MemoryFeedbackAction,
    superseded_by: Option<uuid::Uuid>,
    actor: &str,
    via: &str,
) {
    let queue_kind = memory.review_queue_kind();
    memory.apply_feedback_action(action, superseded_by);
    memory.record_review_event(actor, action, via, queue_kind, superseded_by);
}

async fn feedback(
    State(state): State<AppState>,
    parts: Parts,
    Json(req): Json<FeedbackRequest>,
) -> Result<Response, AppError> {
    let claims = require_auth(&state.config, &parts)?;
    if !rbac::can_update(claims.role) {
        return Err(AppError::Forbidden("insufficient permissions for feedback"));
    }

    let namespace = enforce_namespace(&claims, req.namespace.as_deref())?;
    let mut memory = state
        .doc_engine
        .get(req.id, namespace)?
        .ok_or_else(|| AppError::NotFound("memory not found".to_string()))?;

    if req.superseded_by == Some(req.id) {
        return Err(AppError::BadRequest(
            "memory cannot supersede itself".to_string(),
        ));
    }

    let mut replacement: Option<Memory> = None;
    if let Some(target_id) = req.superseded_by {
        replacement = Some(
            state
                .doc_engine
                .get(target_id, namespace)?
                .ok_or_else(|| AppError::NotFound("superseding memory not found".to_string()))?,
        );
    }

    if matches!(req.action, MemoryFeedbackAction::Supersede) && req.superseded_by.is_none() {
        return Err(AppError::BadRequest(
            "supersede action requires superseded_by".to_string(),
        ));
    }

    apply_feedback_to_memory(
        &mut memory,
        req.action,
        req.superseded_by,
        &claims.sub,
        "feedback_api",
    );
    state.doc_engine.replace(&memory, &claims.sub)?;
    state.trust_scorer.record_feedback(
        memory.source_key.as_deref(),
        matches!(req.action, MemoryFeedbackAction::Confirm),
    );

    if let Some(mut target) = replacement {
        if matches!(req.action, MemoryFeedbackAction::Supersede) {
            apply_feedback_to_memory(
                &mut target,
                MemoryFeedbackAction::Confirm,
                None,
                &claims.sub,
                "feedback_api_supersede_target",
            );
            state.doc_engine.replace(&target, &claims.sub)?;
            state
                .trust_scorer
                .record_feedback(target.source_key.as_deref(), true);
        }
    }

    state
        .indexer_state
        .write_seq
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    state.indexer_state.wake();
    refresh_review_queue_summary(&state, namespace)?;
    state.intent_cache.invalidate_namespace(namespace).await;

    Ok(Json(FeedbackResponse {
        id: memory.id,
        status: memory.status,
        confidence: memory.confidence,
        evidence_count: memory.evidence_count,
        last_verified_at: memory.last_verified_at,
        superseded_by: memory.superseded_by,
        contradicts_with: memory.contradicts_with.clone(),
        injection_count: memory.injection_count,
        reuse_count: memory.reuse_count,
        confirm_count: memory.confirm_count,
        reject_count: memory.reject_count,
        supersede_count: memory.supersede_count,
    })
    .into_response())
}

async fn update(
    State(state): State<AppState>,
    parts: Parts,
    Json(req): Json<UpdateRequest>,
) -> Result<Response, AppError> {
    let claims = require_auth(&state.config, &parts)?;
    if !rbac::can_update(claims.role) {
        return Err(AppError::Forbidden("insufficient permissions for update"));
    }

    let namespace = claims.namespace.clone();
    let content_changed = req.content.is_some();
    let new_content = req.content.clone();
    let updated = state.doc_engine.update(
        req.id,
        &namespace,
        req.content,
        req.tags,
        req.memory_type,
        &claims.sub,
    )?;

    match updated {
        Some(mut memory) => {
            let mut contradiction_updates = 0usize;
            // Re-embed if content changed, store back to SoT, let indexer handle the rest
            if content_changed && let Some(ref content) = new_content {
                let embedding = state.embedding.embed_one(content).await?;
                memory.embedding = Some(embedding);
                contradiction_updates = apply_contradiction_detection(
                    &state,
                    &namespace,
                    &mut memory,
                    &claims.sub,
                    &[],
                )?;
                state.doc_engine.store(&memory, &claims.sub)?;
            }
            state.indexer_state.write_seq.fetch_add(
                (1 + contradiction_updates) as u64,
                std::sync::atomic::Ordering::Relaxed,
            );
            state.indexer_state.wake();
            refresh_review_queue_summary(&state, &namespace)?;
            state.intent_cache.invalidate_namespace(&namespace).await;
            Ok(Json(json!({
                "id": memory.id,
                "version": memory.version,
                "updated_at": memory.updated_at,
                "status": memory.status,
                "eligible_for_injection": memory.eligible_for_injection(),
                "contradicts_with": memory.contradicts_with,
            }))
            .into_response())
        }
        None => Err(AppError::NotFound("memory not found".to_string())),
    }
}

async fn forget(
    State(state): State<AppState>,
    parts: Parts,
    Json(req): Json<ForgetRequest>,
) -> Result<Response, AppError> {
    let claims = require_auth(&state.config, &parts)?;
    if !rbac::can_forget(claims.role) {
        return Err(AppError::Forbidden("insufficient permissions for forget"));
    }

    let namespace = enforce_namespace(&claims, req.namespace.as_deref())?;

    let mut deleted = 0usize;

    if !req.ids.is_empty() {
        for id in &req.ids {
            if state.doc_engine.delete(*id, namespace, &claims.sub)? {
                state
                    .indexer_state
                    .write_seq
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                deleted += 1;
            }
        }
    } else {
        // Filter-based delete
        let memories = state.doc_engine.search_limited(
            namespace,
            req.agent.as_deref(),
            req.session.as_deref(),
            None,
            &req.tags,
            FILTER_DELETE_SCAN_LIMIT + 1,
        )?;
        if memories.len() > FILTER_DELETE_SCAN_LIMIT {
            return Err(AppError::BadRequest(format!(
                "filter delete matches more than {} memories; narrow the filter or delete by ids",
                FILTER_DELETE_SCAN_LIMIT
            )));
        }

        for m in &memories {
            if let Some(before) = &req.before
                && m.created_at >= *before
            {
                continue;
            }
            if state.doc_engine.delete(m.id, namespace, &claims.sub)? {
                state
                    .indexer_state
                    .write_seq
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                deleted += 1;
            }
        }
    }

    // Wake indexer pipeline for delete cleanup
    if deleted > 0 {
        state.indexer_state.wake();
        // Invalidate intent cache (data changed)
        refresh_review_queue_summary(&state, namespace)?;
        state.intent_cache.invalidate_namespace(namespace).await;
    }

    state
        .metrics
        .forgets
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    Ok(Json(ForgetResponse { deleted }).into_response())
}

async fn flush_cache(State(state): State<AppState>, parts: Parts) -> Result<Response, AppError> {
    let claims = require_auth(&state.config, &parts)?;
    if claims.role != Role::Admin {
        return Err(AppError::Forbidden("admin role required for cache flush"));
    }
    let evicted = state.embedding.flush_cache().await;
    tracing::info!(evicted, "embedding cache flushed");
    Ok(Json(json!({"flushed": evicted})).into_response())
}

async fn cache_stats(State(state): State<AppState>, parts: Parts) -> Result<Response, AppError> {
    let claims = require_auth(&state.config, &parts)?;
    if claims.role != Role::Admin {
        return Err(AppError::Forbidden("admin role required for cache stats"));
    }
    let (valid, total) = state.embedding.cache_stats().await;
    Ok(Json(json!({"valid_entries": valid, "total_entries": total})).into_response())
}

async fn index_health(State(state): State<AppState>, parts: Parts) -> Result<Response, AppError> {
    let claims = require_auth(&state.config, &parts)?;
    if claims.role != Role::Admin {
        return Err(AppError::Forbidden("admin role required for index health"));
    }
    let lag = state.indexer_state.lag();
    let vec_seq = state
        .indexer_state
        .vector_seq
        .load(std::sync::atomic::Ordering::Relaxed);
    let fts_seq = state
        .indexer_state
        .fts_seq
        .load(std::sync::atomic::Ordering::Relaxed);
    let write_seq = state
        .indexer_state
        .write_seq
        .load(std::sync::atomic::Ordering::Relaxed);
    let vector_size = state.vector_engine.size();
    let (cache_valid, cache_total) = state.embedding.cache_stats().await;

    Ok(Json(json!({
        "vector_index_size": vector_size,
        "indexer_lag": lag,
        "vector_seq": vec_seq,
        "fts_seq": fts_seq,
        "write_seq": write_seq,
        "embedding_cache": {
            "valid": cache_valid,
            "total": cache_total,
        },
        "status": if lag == 0 { "healthy" } else if lag < 100 { "catching_up" } else { "behind" },
    }))
    .into_response())
}

async fn idf_stats(State(state): State<AppState>, parts: Parts) -> Result<Response, AppError> {
    let claims = require_auth(&state.config, &parts)?;
    if claims.role != Role::Admin {
        return Err(AppError::Forbidden("admin role required for IDF stats"));
    }
    let (total_docs, unique_terms) = state.idf_index.stats();
    let most_common: Vec<_> = state
        .idf_index
        .most_common(10)
        .iter()
        .map(|(t, c, idf)| json!({"term": t, "doc_freq": c, "idf": idf}))
        .collect();
    let most_rare: Vec<_> = state
        .idf_index
        .most_rare(10)
        .iter()
        .map(|(t, c, idf)| json!({"term": t, "doc_freq": c, "idf": idf}))
        .collect();

    Ok(Json(json!({
        "total_documents": total_docs,
        "unique_terms": unique_terms,
        "most_common": most_common,
        "most_rare": most_rare,
    }))
    .into_response())
}

async fn query_explain(
    State(state): State<AppState>,
    parts: Parts,
    Json(mut req): Json<RecallRequest>,
) -> Result<Response, AppError> {
    let claims = require_auth(&state.config, &parts)?;
    if !rbac::can_recall(claims.role) {
        return Err(AppError::Forbidden("insufficient permissions for recall"));
    }
    check_read_rate_limit(&state, &claims)?;

    req.limit = req.limit.min(100);
    let namespace = enforce_namespace(&claims, req.namespace.as_deref())?;
    let eventual = req.consistency.as_deref() == Some("eventual");
    let write_seq = state
        .indexer_state
        .write_seq
        .load(std::sync::atomic::Ordering::Relaxed);
    let vector_seq = state
        .indexer_state
        .vector_seq
        .load(std::sync::atomic::Ordering::Relaxed);
    let fts_seq = state
        .indexer_state
        .fts_seq
        .load(std::sync::atomic::Ordering::Relaxed);
    let use_vector = eventual || vector_seq >= write_seq;
    let use_fts = eventual || fts_seq >= write_seq;

    let candidate_ids: Option<std::collections::HashSet<uuid::Uuid>> = if req.has_filters() {
        if use_fts {
            let mt_str = req.memory_type.map(|mt| format!("{:?}", mt));
            let fts_candidates = state
                .fts_engine
                .search_metadata(
                    req.agent.as_deref(),
                    req.session.as_deref(),
                    mt_str.as_deref(),
                    &req.tags,
                    req.limit * 10,
                )
                .unwrap_or_default();
            let filtered: std::collections::HashSet<uuid::Uuid> =
                if req.before.is_some() || req.after.is_some() {
                    fts_candidates
                        .into_iter()
                        .filter(|id| {
                            state
                                .doc_engine
                                .get(*id, namespace)
                                .ok()
                                .flatten()
                                .map(|m| {
                                    req.before.as_ref().is_none_or(|b| m.created_at < *b)
                                        && req.after.as_ref().is_none_or(|a| m.created_at > *a)
                                })
                                .unwrap_or(false)
                        })
                        .collect()
                } else {
                    fts_candidates.into_iter().collect()
                };
            Some(filtered)
        } else {
            let candidates = state.doc_engine.search(
                namespace,
                req.agent.as_deref(),
                req.session.as_deref(),
                req.memory_type,
                &req.tags,
            )?;
            let filtered: std::collections::HashSet<uuid::Uuid> = candidates
                .into_iter()
                .filter(|m| {
                    req.before.as_ref().is_none_or(|b| m.created_at < *b)
                        && req.after.as_ref().is_none_or(|a| m.created_at > *a)
                })
                .map(|m| m.id)
                .collect();
            Some(filtered)
        }
    } else {
        None
    };

    let query_embedding = if let Some(ref emb) = req.query_embedding {
        emb.clone()
    } else {
        state.embedding.embed_one(&req.query).await?
    };

    let overfetch = if candidate_ids.is_some() {
        req.limit * 5
    } else {
        req.limit * 2
    };

    let mut vector_results = if use_vector {
        state.vector_engine.search(&query_embedding, overfetch)?
    } else {
        Vec::new()
    };
    let mut fts_results = if use_fts {
        state.fts_engine.search(&req.query, overfetch)?
    } else {
        Vec::new()
    };
    if let Some(ref ids) = candidate_ids {
        vector_results.retain(|(id, _)| ids.contains(id));
        fts_results.retain(|(id, _)| ids.contains(id));
    }

    let weights = req.weights.unwrap_or_default().clamped();
    let identifiers = crate::scoring::extract_identifiers(&req.query);
    let identifier_route = if state.config.proxy.identifier_first_routing {
        crate::scoring::detect_identifier_route(&req.query)
    } else {
        None
    };
    let exact_results = if use_fts && !identifiers.is_empty() {
        crate::scoring::exact_match_search(&state.fts_engine, &identifiers, overfetch)
    } else {
        Vec::new()
    };
    let idf_boost = crate::scoring::compute_idf_boost(&state.idf_index, &req.query);
    let min_channel_score = state.config.proxy.min_channel_score.unwrap_or(0.0);
    let diversity_factor = state.config.proxy.diversity_factor.unwrap_or(0.0);
    let task_context = crate::scoring::detect_task_context(&req.query);

    let options = crate::scoring::MergeOptions {
        weights: weights.clone(),
        idf_boost,
        min_channel_score,
        apply_confidence_penalty: false,
        apply_trust_scoring: true,
        namespace: namespace.to_string(),
        limit: req.limit * 2,
        agent_filter: req.agent.clone(),
        diversity_factor,
        task_context: task_context.clone(),
        identifier_route: identifier_route.clone(),
    };

    let mut explained = crate::scoring::score_and_explain(
        &vector_results,
        &fts_results,
        &exact_results,
        &state.doc_engine,
        Some(&state.trust_scorer),
        &options,
    );

    explained.retain(|entry| {
        if let Some(ref session) = req.session
            && entry.memory.session.as_deref() != Some(session)
        {
            return false;
        }
        if let Some(mt) = req.memory_type
            && entry.memory.memory_type != mt
        {
            return false;
        }
        if !req.tags.is_empty() && !req.tags.iter().any(|t| entry.memory.tags.contains(t)) {
            return false;
        }
        if let Some(ref before) = req.before
            && entry.memory.created_at >= *before
        {
            return false;
        }
        if let Some(ref after) = req.after
            && entry.memory.created_at <= *after
        {
            return false;
        }
        true
    });
    explained =
        crate::fusion::collapse_explained_entries_for_query(explained, identifier_route.as_ref());
    explained.truncate(req.limit);
    let (retrieval_gate, _) = crate::scoring::apply_retrieval_confidence_gate(
        &explained,
        &req.query,
        state.config.proxy.min_recall_score,
        state.config.proxy.confidence_gate,
    );

    let vector_explain: Vec<_> = vector_results
        .iter()
        .map(|(id, sim)| json!({"id": id, "similarity": sim}))
        .collect();

    let fts_max = fts_results.iter().map(|(_, s)| *s).fold(0.0f32, f32::max);
    let fts_explain: Vec<_> = fts_results
        .iter()
        .map(|(id, bm25)| {
            let norm = if fts_max > 0.0 { *bm25 / fts_max } else { 0.0 };
            json!({"id": id, "bm25_raw": bm25, "normalized": norm})
        })
        .collect();
    let exact_explain: Vec<_> = exact_results
        .iter()
        .map(|(id, score)| json!({"id": id, "score": score}))
        .collect();
    let review_queue_summary = cached_review_queue_summary(&state, namespace);
    let summary_results = crate::fusion::build_explained_memory_summaries(&explained);
    let compiled_task_state = task_context
        .as_ref()
        .and_then(|context| crate::fusion::compile_explained_task_state(&explained, context));
    let policy_firewall =
        crate::security::trust::evaluate_policy_firewall_explained(&req.query, &explained);

    Ok(Json(json!({
        "query": req.query,
        "namespace": namespace,
        "indexer_lag": write_seq.saturating_sub(vector_seq.min(fts_seq)),
        "used_vector": use_vector,
        "used_fts": use_fts,
        "consistency": if eventual { "eventual" } else { "strong" },
        "candidate_filter_count": candidate_ids.as_ref().map(|ids| ids.len()),
        "identifiers": identifiers,
        "identifier_route": identifier_route.as_ref().map(|route| json!({
            "active": route.active,
            "identifiers": route.identifiers,
            "kinds": route.labels(),
            "matched_terms": route.matched_terms,
            "focus_terms": route.focus_terms,
            "enabled": state.config.proxy.identifier_first_routing,
        })),
        "task_context": task_context.as_ref().map(|ctx| json!({
            "kind": ctx.label(),
            "matched_terms": ctx.matched_terms,
        })),
        "task_state": compiled_task_state,
        "policy_firewall": policy_firewall,
        "idf_boost": idf_boost,
        "min_channel_score": min_channel_score,
        "diversity_factor": diversity_factor,
        "weights": {
            "vector": weights.vector,
            "fts": weights.fts,
            "exact": weights.exact,
            "recency": weights.recency,
        },
        "retrieval_gate": retrieval_gate,
        "vector_results": vector_explain,
        "fts_results": fts_explain,
        "exact_results": exact_explain,
        "review_queue_summary": review_queue_summary,
        "summary_results": summary_results,
        "final_results": explained,
    }))
    .into_response())
}

async fn inspect_memory(
    State(state): State<AppState>,
    parts: Parts,
    axum::extract::Path(id_str): axum::extract::Path<String>,
) -> Result<Response, AppError> {
    let claims = require_auth(&state.config, &parts)?;
    if !rbac::can_recall(claims.role) {
        return Err(AppError::Forbidden("insufficient permissions"));
    }
    check_read_rate_limit(&state, &claims)?;

    let id: uuid::Uuid = id_str
        .parse()
        .map_err(|_| AppError::BadRequest("invalid UUID".to_string()))?;

    let namespace = &claims.namespace;
    let memory = state.doc_engine.get(id, namespace)?;
    match memory {
        Some(mut mem) => {
            let lifecycle_changed = apply_memory_lifecycle(
                &state,
                namespace,
                &mut mem,
                &claims.sub,
                state.config.decay.after_days,
            )?;
            if lifecycle_changed {
                state.intent_cache.invalidate_namespace(namespace).await;
            }
            let trust = state.trust_scorer.score_memory(&mem, namespace);
            let age_hours = (chrono::Utc::now() - mem.created_at).num_hours();
            let conflicts: Vec<_> = mem
                .contradicts_with
                .iter()
                .filter_map(|conflict_id| {
                    state.doc_engine.get(*conflict_id, namespace).ok().flatten()
                })
                .map(|conflict| {
                    json!({
                        "id": conflict.id,
                        "content": conflict.content,
                        "status": conflict.status,
                        "superseded_by": conflict.superseded_by,
                    })
                })
                .collect();
            Ok(Json(json!({
                "id": mem.id,
                "content": mem.content,
                "tags": mem.tags,
                "agent": mem.agent,
                "session": mem.session,
                "namespace": mem.namespace,
                "memory_type": mem.memory_type,
                "status": mem.status,
                "version": mem.version,
                "created_at": mem.created_at,
                "updated_at": mem.updated_at,
                "confidence": mem.confidence,
                "evidence_count": mem.evidence_count,
                "last_verified_at": mem.last_verified_at,
                "superseded_by": mem.superseded_by,
                "derived_from": mem.derived_from,
                "contradicts_with": mem.contradicts_with,
                "contradiction_count": mem.contradicts_with.len(),
                "conflicts": conflicts,
                "has_embedding": mem.embedding.is_some(),
                "trust_score": trust.score,
                "trust_confidence_low": trust.confidence_low,
                "trust_confidence_high": trust.confidence_high,
                "trust_signals": trust.signals,
                "low_trust": trust.low_trust,
                "age_hours": age_hours,
                "eligible_for_injection": mem.eligible_for_injection(),
                "injection_count": mem.injection_count,
                "reuse_count": mem.reuse_count,
                "confirm_count": mem.confirm_count,
                "reject_count": mem.reject_count,
                "supersede_count": mem.supersede_count,
                "last_injected_at": mem.last_injected_at,
                "last_reused_at": mem.last_reused_at,
                "last_outcome_at": mem.last_outcome_at,
                "review_queue_kind": mem.review_queue_kind(),
                "suggested_review_action": mem.suggested_review_action(),
                "review_events": mem.review_events,
            }))
            .into_response())
        }
        None => Err(AppError::NotFound("memory not found".to_string())),
    }
}

/// Peek: returns metadata for a memory without loading content/embedding.
async fn peek_memory(
    State(state): State<AppState>,
    parts: Parts,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Response, AppError> {
    let claims = require_auth(&state.config, &parts)?;
    if !rbac::can_recall(claims.role) {
        return Err(AppError::Forbidden("insufficient permissions"));
    }
    check_read_rate_limit(&state, &claims)?;

    let uuid: uuid::Uuid = id
        .parse()
        .map_err(|_| AppError::BadRequest("invalid UUID".to_string()))?;

    match state.space_index.peek(uuid) {
        Some(meta) => {
            // Namespace isolation: only return metadata for memories in the caller's namespace
            let ns = &meta.namespace;
            if ns != &claims.namespace {
                return Err(AppError::NotFound("memory not found".to_string()));
            }
            Ok(Json(meta).into_response())
        }
        None => Err(AppError::NotFound("memory not found".to_string())),
    }
}

/// Per-agent and global memory space statistics.
async fn space_stats(State(state): State<AppState>, parts: Parts) -> Result<Response, AppError> {
    let claims = require_auth(&state.config, &parts)?;
    if !rbac::can_recall(claims.role) {
        return Err(AppError::Forbidden("insufficient permissions"));
    }

    let agent_stats = state.space_index.agent_stats();
    let (total_count, total_bytes) = state.space_index.global_stats();

    Ok(Json(json!({
        "total_memories": total_count,
        "total_bytes": total_bytes,
        "agents": agent_stats,
    }))
    .into_response())
}

// ── GDPR Compliance endpoints ──────────────────────────────────────────

/// GET /v1/export?namespace=X — Full memory dump for data portability (GDPR Art. 20).
async fn gdpr_export(
    State(state): State<AppState>,
    parts: Parts,
    axum::extract::Query(params): axum::extract::Query<GdprExportParams>,
) -> Result<Response, AppError> {
    let claims = require_auth(&state.config, &parts)?;
    check_read_rate_limit(&state, &claims)?;
    let namespace = enforce_namespace(&claims, params.namespace.as_deref())?;

    let memories = state.doc_engine.list_all(namespace)?;

    // Strip embeddings from export (large, not useful for portability)
    let export: Vec<serde_json::Value> = memories
        .iter()
        .map(|m| {
            json!({
                "id": m.id,
                "content": m.content,
                "tags": m.tags,
                "agent": m.agent,
                "session": m.session,
                "namespace": m.namespace,
                "memory_type": m.memory_type,
                "status": m.status,
                "version": m.version,
                "created_at": m.created_at,
                "updated_at": m.updated_at,
                "source_key_id": m.source_key,
                "content_hash": m.content_hash,
                "confidence": m.confidence,
                "evidence_count": m.evidence_count,
                "last_verified_at": m.last_verified_at,
                "superseded_by": m.superseded_by,
                "derived_from": m.derived_from,
            })
        })
        .collect();

    Ok(Json(json!({
        "runtime_contract": runtime_contract_export_metadata(),
        "namespace": namespace,
        "exported_at": chrono::Utc::now(),
        "count": export.len(),
        "memories": export,
    }))
    .into_response())
}

/// GET /v1/runtime/contract — versioned portable runtime semantics and current gaps.
async fn runtime_contract(
    State(state): State<AppState>,
    parts: Parts,
) -> Result<Response, AppError> {
    let claims = require_auth(&state.config, &parts)?;
    check_read_rate_limit(&state, &claims)?;
    if !rbac::can_recall(claims.role) {
        return Err(AppError::Forbidden("insufficient permissions"));
    }

    Ok(Json(runtime_contract_document()).into_response())
}

/// GET /v1/adapters/export?kind=claude_project — export runtime memories into a foreign artifact.
async fn adapter_export(
    State(state): State<AppState>,
    parts: Parts,
    axum::extract::Query(params): axum::extract::Query<AdapterExportParams>,
) -> Result<Response, AppError> {
    let claims = require_auth(&state.config, &parts)?;
    check_read_rate_limit(&state, &claims)?;
    if !rbac::can_recall(claims.role) {
        return Err(AppError::Forbidden("insufficient permissions"));
    }

    let namespace = enforce_namespace(&claims, params.namespace.as_deref())?;
    let kind = params
        .kind
        .parse::<MemoryAdapterKind>()
        .map_err(AppError::BadRequest)?;
    let memories = state.doc_engine.list_all_including_archived(namespace)?;
    let artifact =
        render_adapter_export(kind, namespace, &memories).map_err(AppError::BadRequest)?;
    Ok(Json(AdapterExportResponse { artifact }).into_response())
}

#[derive(Deserialize)]
struct GdprExportParams {
    namespace: Option<String>,
}

#[derive(Deserialize)]
struct PassportExportParams {
    namespace: Option<String>,
    scope: Option<String>,
}

#[derive(Deserialize)]
struct BundleExportParams {
    namespace: Option<String>,
    kind: String,
    scope: Option<String>,
    id: Option<String>,
}

#[derive(Deserialize)]
struct BundleEnvelopeRequest {
    bundle: MemoryBundleEnvelope,
}

#[derive(Deserialize)]
struct BundleDiffRequest {
    left: MemoryBundleEnvelope,
    right: MemoryBundleEnvelope,
}

#[derive(Deserialize)]
struct PassportImportRequest {
    #[serde(default)]
    namespace: Option<String>,
    #[serde(default)]
    dry_run: bool,
    bundle: MemoryPassportBundle,
}

#[derive(Serialize)]
struct PassportImportResponse {
    dry_run: bool,
    imported: usize,
    preview: crate::memory::PassportImportPreview,
    imported_ids: Vec<uuid::Uuid>,
}

#[derive(Deserialize)]
struct AdapterExportParams {
    namespace: Option<String>,
    kind: String,
}

#[derive(Deserialize)]
struct AdapterImportRequest {
    kind: String,
    source_label: String,
    content: String,
    #[serde(default)]
    namespace: Option<String>,
    #[serde(default)]
    dry_run: bool,
}

#[derive(Serialize)]
struct AdapterImportResponse {
    dry_run: bool,
    imported: usize,
    preview: AdapterImportPreview,
    imported_ids: Vec<uuid::Uuid>,
}

#[derive(Serialize)]
struct AdapterExportResponse {
    artifact: AdapterExportArtifact,
}

#[derive(Deserialize)]
struct HistoryParams {
    #[serde(default)]
    namespace: Option<String>,
}

#[derive(Deserialize)]
struct HistoryReplayRequest {
    #[serde(default)]
    namespace: Option<String>,
    #[serde(default)]
    dry_run: bool,
    bundle: MemoryHistoryBundle,
}

#[derive(Serialize)]
struct HistoryReplayResponse {
    dry_run: bool,
    imported: usize,
    preview: crate::memory::MemoryHistoryReplayPreview,
    imported_ids: Vec<uuid::Uuid>,
}

/// GET /v1/bundles/export?kind=passport — export a portable bundle envelope around runtime artifacts.
async fn bundle_export(
    State(state): State<AppState>,
    parts: Parts,
    axum::extract::Query(params): axum::extract::Query<BundleExportParams>,
) -> Result<Response, AppError> {
    let claims = require_auth(&state.config, &parts)?;
    check_read_rate_limit(&state, &claims)?;
    if !rbac::can_recall(claims.role) {
        return Err(AppError::Forbidden("insufficient permissions"));
    }

    let namespace = enforce_namespace(&claims, params.namespace.as_deref())?;
    let kind = params
        .kind
        .parse::<MemoryBundleKind>()
        .map_err(AppError::BadRequest)?;
    let memories = state.doc_engine.list_all_including_archived(namespace)?;
    let bundle = match kind {
        MemoryBundleKind::Passport => {
            let scope = params
                .scope
                .as_deref()
                .unwrap_or("project")
                .parse::<PassportScope>()
                .map_err(AppError::BadRequest)?;
            build_memory_bundle_from_passport(build_memory_passport_bundle(
                namespace, scope, &memories,
            ))
        }
        MemoryBundleKind::History => {
            let root_id = params
                .id
                .as_deref()
                .ok_or_else(|| {
                    AppError::BadRequest("history bundle export requires id".to_string())
                })?
                .parse::<uuid::Uuid>()
                .map_err(|_| AppError::BadRequest("invalid UUID".to_string()))?;
            let history = build_memory_history_bundle(namespace, root_id, &memories)
                .ok_or_else(|| AppError::NotFound("history root not found".to_string()))?;
            build_memory_bundle_from_history(history)
        }
    };

    Ok(Json(bundle).into_response())
}

/// POST /v1/bundles/preview — summarize bundle metadata without importing it.
async fn bundle_preview(
    State(state): State<AppState>,
    parts: Parts,
    Json(req): Json<BundleEnvelopeRequest>,
) -> Result<Response, AppError> {
    let claims = require_auth(&state.config, &parts)?;
    check_read_rate_limit(&state, &claims)?;
    if !rbac::can_recall(claims.role) {
        return Err(AppError::Forbidden("insufficient permissions"));
    }

    let preview = preview_memory_bundle(&req.bundle).map_err(AppError::BadRequest)?;
    Ok(Json(preview).into_response())
}

/// POST /v1/bundles/validate — validate envelope and nested bundle integrity.
async fn bundle_validate(
    State(state): State<AppState>,
    parts: Parts,
    Json(req): Json<BundleEnvelopeRequest>,
) -> Result<Response, AppError> {
    let claims = require_auth(&state.config, &parts)?;
    check_read_rate_limit(&state, &claims)?;
    if !rbac::can_recall(claims.role) {
        return Err(AppError::Forbidden("insufficient permissions"));
    }

    Ok(Json(validate_memory_bundle(&req.bundle)).into_response())
}

/// POST /v1/bundles/diff — compare two bundles without importing either one.
async fn bundle_diff(
    State(state): State<AppState>,
    parts: Parts,
    Json(req): Json<BundleDiffRequest>,
) -> Result<Response, AppError> {
    let claims = require_auth(&state.config, &parts)?;
    check_read_rate_limit(&state, &claims)?;
    if !rbac::can_recall(claims.role) {
        return Err(AppError::Forbidden("insufficient permissions"));
    }

    let diff = diff_memory_bundles(&req.left, &req.right).map_err(AppError::BadRequest)?;
    Ok(Json(diff).into_response())
}

/// GET /v1/passport/export?scope=project — selective portable memory passport bundle.
async fn passport_export(
    State(state): State<AppState>,
    parts: Parts,
    axum::extract::Query(params): axum::extract::Query<PassportExportParams>,
) -> Result<Response, AppError> {
    let claims = require_auth(&state.config, &parts)?;
    check_read_rate_limit(&state, &claims)?;
    if !rbac::can_recall(claims.role) {
        return Err(AppError::Forbidden("insufficient permissions"));
    }

    let namespace = enforce_namespace(&claims, params.namespace.as_deref())?;
    let scope = params
        .scope
        .as_deref()
        .unwrap_or("project")
        .parse::<PassportScope>()
        .map_err(AppError::BadRequest)?;
    let memories = state.doc_engine.list_all_including_archived(namespace)?;
    Ok(Json(build_memory_passport_bundle(namespace, scope, &memories)).into_response())
}

/// POST /v1/adapters/import — normalize a foreign artifact into runtime records with dry-run preview.
async fn adapter_import(
    State(state): State<AppState>,
    parts: Parts,
    Json(req): Json<AdapterImportRequest>,
) -> Result<Response, AppError> {
    let claims = require_auth(&state.config, &parts)?;
    if !rbac::can_store(claims.role) {
        return Err(AppError::Forbidden("insufficient permissions for store"));
    }
    if let Err(retry_ms) = state.rate_limiter.check(&claims.sub) {
        return Err(AppError::RateLimited(retry_ms));
    }
    if state.indexer_state.lag() > BACKPRESSURE_THRESHOLD {
        return Err(AppError::RateLimited(1000));
    }

    let target_namespace = enforce_namespace(&claims, req.namespace.as_deref())?.to_string();
    let kind = req
        .kind
        .parse::<MemoryAdapterKind>()
        .map_err(AppError::BadRequest)?;
    let existing = state
        .doc_engine
        .list_all_including_archived(&target_namespace)?;
    let plan = plan_adapter_import(
        &target_namespace,
        kind,
        &req.source_label,
        &req.content,
        &existing,
    );
    let imported_ids = if req.dry_run {
        Vec::new()
    } else {
        let staged_memories =
            materialize_staged_import_memories(&state, &plan.staged_memories).await?;
        apply_staged_import_memories(
            &state,
            &target_namespace,
            &staged_memories,
            &format!("adapter-import-api:{kind}"),
        )?
    };
    if !imported_ids.is_empty() {
        let target_seq = state
            .indexer_state
            .write_seq
            .load(std::sync::atomic::Ordering::Relaxed)
            + imported_ids.len() as u64;
        note_indexer_writes(&state, imported_ids.len());
        wait_for_indexer_catchup(&state, target_seq).await?;
        state
            .intent_cache
            .invalidate_namespace(&target_namespace)
            .await;
    }

    Ok(Json(AdapterImportResponse {
        dry_run: req.dry_run,
        imported: imported_ids.len(),
        preview: plan.preview,
        imported_ids,
    })
    .into_response())
}

fn resolve_passport_import_namespace(
    claims: &Claims,
    requested_namespace: Option<&str>,
    bundle_namespace: &str,
) -> Result<String, AppError> {
    if let Some(namespace) = requested_namespace {
        return Ok(enforce_namespace(claims, Some(namespace))?.to_string());
    }
    Ok(enforce_namespace(claims, Some(bundle_namespace))?.to_string())
}

fn apply_staged_import_memories(
    state: &AppState,
    namespace: &str,
    staged_memories: &[Memory],
    subject: &str,
) -> Result<Vec<uuid::Uuid>, AppError> {
    let mut imported_ids = Vec::new();
    for memory in staged_memories {
        state
            .doc_engine
            .store(memory, subject)
            .map_err(AppError::Internal)?;
        imported_ids.push(memory.id);
    }

    if !imported_ids.is_empty() {
        refresh_review_queue_summary(state, namespace).map_err(AppError::Internal)?;
    }

    Ok(imported_ids)
}

async fn materialize_staged_import_memories(
    state: &AppState,
    staged_memories: &[Memory],
) -> Result<Vec<Memory>, AppError> {
    let pending_embeddings: Vec<String> = staged_memories
        .iter()
        .filter(|memory| memory.embedding.is_none())
        .map(|memory| memory.content.clone())
        .collect();
    if pending_embeddings.is_empty() {
        return Ok(staged_memories.to_vec());
    }

    let mut generated = state.embedding.embed(pending_embeddings).await?.into_iter();
    let mut prepared = staged_memories.to_vec();
    for memory in &mut prepared {
        if memory.embedding.is_none() {
            memory.embedding = Some(generated.next().ok_or_else(|| {
                AppError::Internal(anyhow::anyhow!(
                    "import embedding generation produced fewer vectors than expected"
                ))
            })?);
        }
    }
    if generated.next().is_some() {
        return Err(AppError::Internal(anyhow::anyhow!(
            "import embedding generation produced more vectors than expected"
        )));
    }

    Ok(prepared)
}

async fn apply_passport_import_plan(
    state: &AppState,
    namespace: &str,
    plan: &PassportImportPlan,
    subject: &str,
) -> Result<Vec<uuid::Uuid>, AppError> {
    let staged_memories = materialize_staged_import_memories(state, &plan.staged_memories).await?;
    apply_staged_import_memories(state, namespace, &staged_memories, subject)
}

/// POST /v1/passport/import — import a portable memory passport bundle with dry-run preview.
async fn passport_import(
    State(state): State<AppState>,
    parts: Parts,
    Json(req): Json<PassportImportRequest>,
) -> Result<Response, AppError> {
    let claims = require_auth(&state.config, &parts)?;
    if !rbac::can_store(claims.role) {
        return Err(AppError::Forbidden("insufficient permissions for store"));
    }
    if let Err(retry_ms) = state.rate_limiter.check(&claims.sub) {
        return Err(AppError::RateLimited(retry_ms));
    }
    if state.indexer_state.lag() > BACKPRESSURE_THRESHOLD {
        return Err(AppError::RateLimited(1000));
    }

    if !verify_memory_passport_bundle(&req.bundle) {
        return Err(AppError::BadRequest(
            "passport bundle integrity check failed".to_string(),
        ));
    }

    let target_namespace = resolve_passport_import_namespace(
        &claims,
        req.namespace.as_deref(),
        &req.bundle.namespace,
    )?;
    let existing = state
        .doc_engine
        .list_all_including_archived(&target_namespace)?;
    let plan = plan_memory_passport_import(&target_namespace, &req.bundle, &existing);
    let imported_ids = if req.dry_run {
        Vec::new()
    } else {
        apply_passport_import_plan(&state, &target_namespace, &plan, "passport-import-api").await?
    };
    if !imported_ids.is_empty() {
        let target_seq = state
            .indexer_state
            .write_seq
            .load(std::sync::atomic::Ordering::Relaxed)
            + imported_ids.len() as u64;
        note_indexer_writes(&state, imported_ids.len());
        wait_for_indexer_catchup(&state, target_seq).await?;
        state
            .intent_cache
            .invalidate_namespace(&target_namespace)
            .await;
    }

    Ok(Json(PassportImportResponse {
        dry_run: req.dry_run,
        imported: imported_ids.len(),
        preview: plan.preview,
        imported_ids,
    })
    .into_response())
}

/// GET /v1/history/{id} — inspect lineage, transitions, contradictions, and review chain.
async fn history_view(
    State(state): State<AppState>,
    parts: Parts,
    axum::extract::Path(id): axum::extract::Path<String>,
    axum::extract::Query(params): axum::extract::Query<HistoryParams>,
) -> Result<Response, AppError> {
    let claims = require_auth(&state.config, &parts)?;
    check_read_rate_limit(&state, &claims)?;
    if !rbac::can_recall(claims.role) {
        return Err(AppError::Forbidden("insufficient permissions"));
    }

    let root_id = id
        .parse::<uuid::Uuid>()
        .map_err(|_| AppError::BadRequest("invalid UUID".to_string()))?;
    let namespace = enforce_namespace(&claims, params.namespace.as_deref())?;
    let memories = state.doc_engine.list_all_including_archived(namespace)?;
    let view = build_memory_history_view(namespace, root_id, &memories)
        .ok_or_else(|| AppError::NotFound("history root not found".to_string()))?;
    Ok(Json(view).into_response())
}

/// GET /v1/history/{id}/bundle — export deterministic replay bundle for one lineage closure.
async fn history_bundle(
    State(state): State<AppState>,
    parts: Parts,
    axum::extract::Path(id): axum::extract::Path<String>,
    axum::extract::Query(params): axum::extract::Query<HistoryParams>,
) -> Result<Response, AppError> {
    let claims = require_auth(&state.config, &parts)?;
    check_read_rate_limit(&state, &claims)?;
    if !rbac::can_recall(claims.role) {
        return Err(AppError::Forbidden("insufficient permissions"));
    }

    let root_id = id
        .parse::<uuid::Uuid>()
        .map_err(|_| AppError::BadRequest("invalid UUID".to_string()))?;
    let namespace = enforce_namespace(&claims, params.namespace.as_deref())?;
    let memories = state.doc_engine.list_all_including_archived(namespace)?;
    let bundle = build_memory_history_bundle(namespace, root_id, &memories)
        .ok_or_else(|| AppError::NotFound("history root not found".to_string()))?;
    Ok(Json(bundle).into_response())
}

async fn apply_history_replay_plan(
    state: &AppState,
    namespace: &str,
    plan: &MemoryHistoryReplayPlan,
    subject: &str,
) -> Result<Vec<uuid::Uuid>, AppError> {
    let staged_memories = materialize_staged_import_memories(state, &plan.staged_memories).await?;
    let mut imported_ids = Vec::new();
    for memory in &staged_memories {
        state
            .doc_engine
            .store(memory, subject)
            .map_err(AppError::Internal)?;
        imported_ids.push(memory.id);
    }

    if !imported_ids.is_empty() {
        refresh_review_queue_summary(state, namespace).map_err(AppError::Internal)?;
    }

    Ok(imported_ids)
}

/// POST /v1/history/replay — replay a lineage bundle into an empty target namespace.
async fn history_replay(
    State(state): State<AppState>,
    parts: Parts,
    Json(req): Json<HistoryReplayRequest>,
) -> Result<Response, AppError> {
    let claims = require_auth(&state.config, &parts)?;
    if !rbac::can_store(claims.role) {
        return Err(AppError::Forbidden("insufficient permissions for store"));
    }
    if let Err(retry_ms) = state.rate_limiter.check(&claims.sub) {
        return Err(AppError::RateLimited(retry_ms));
    }
    if state.indexer_state.lag() > BACKPRESSURE_THRESHOLD {
        return Err(AppError::RateLimited(1000));
    }

    let target_requested = req.namespace.as_deref().unwrap_or(&req.bundle.namespace);
    let target_namespace = enforce_namespace(&claims, Some(target_requested))?.to_string();
    let existing = state
        .doc_engine
        .list_all_including_archived(&target_namespace)?;
    let plan = plan_memory_history_replay(&target_namespace, &req.bundle, &existing);

    if req.dry_run {
        return Ok(Json(HistoryReplayResponse {
            dry_run: true,
            imported: 0,
            preview: plan.preview,
            imported_ids: Vec::new(),
        })
        .into_response());
    }

    if !verify_memory_history_bundle(&req.bundle) {
        return Err(AppError::BadRequest(
            "history bundle integrity check failed".to_string(),
        ));
    }
    if !plan.preview.can_replay {
        return Err(AppError::BadRequest(
            plan.preview
                .blocked_reason
                .clone()
                .unwrap_or_else(|| "history replay blocked".to_string()),
        ));
    }

    let imported_ids =
        apply_history_replay_plan(&state, &target_namespace, &plan, "history-replay-api").await?;
    if !imported_ids.is_empty() {
        let target_seq = state
            .indexer_state
            .write_seq
            .load(std::sync::atomic::Ordering::Relaxed)
            + imported_ids.len() as u64;
        note_indexer_writes(&state, imported_ids.len());
        wait_for_indexer_catchup(&state, target_seq).await?;
        state
            .intent_cache
            .invalidate_namespace(&target_namespace)
            .await;
    }

    Ok(Json(HistoryReplayResponse {
        dry_run: false,
        imported: imported_ids.len(),
        preview: plan.preview,
        imported_ids,
    })
    .into_response())
}

/// GET /v1/memories?agent=X — Right to access (GDPR Art. 15).
async fn gdpr_access(
    State(state): State<AppState>,
    parts: Parts,
    axum::extract::Query(params): axum::extract::Query<GdprAccessParams>,
) -> Result<Response, AppError> {
    let claims = require_auth(&state.config, &parts)?;
    check_read_rate_limit(&state, &claims)?;
    let namespace = enforce_namespace(&claims, params.namespace.as_deref())?;

    let memories = state.doc_engine.search(
        namespace,
        params.agent.as_deref(),
        params.session.as_deref(),
        None,
        &[],
    )?;

    Ok(Json(json!({
        "namespace": namespace,
        "agent": params.agent,
        "session": params.session,
        "count": memories.len(),
        "memories": memories,
    }))
    .into_response())
}

#[derive(Deserialize)]
struct GdprAccessParams {
    namespace: Option<String>,
    agent: Option<String>,
    session: Option<String>,
}

/// DELETE /v1/forget/certified — Delete with Merkle-hash deletion certificate (GDPR Art. 17).
async fn gdpr_forget_certified(
    State(state): State<AppState>,
    parts: Parts,
    Json(req): Json<ForgetRequest>,
) -> Result<Response, AppError> {
    let claims = require_auth(&state.config, &parts)?;
    if !rbac::can_forget(claims.role) {
        return Err(AppError::Forbidden("insufficient permissions for forget"));
    }

    let namespace = enforce_namespace(&claims, req.namespace.as_deref())?;

    // Collect memories to be deleted (for certificate generation)
    let mut to_delete: Vec<(uuid::Uuid, String)> = Vec::new(); // (id, content_hash)

    if !req.ids.is_empty() {
        for id in &req.ids {
            if let Ok(Some(memory)) = state.doc_engine.get(*id, namespace) {
                let hash = memory.content_hash.clone().unwrap_or_else(|| {
                    format!("{:x}", sha2::Sha256::digest(memory.content.as_bytes()))
                });
                to_delete.push((*id, hash));
            }
        }
    } else {
        let memories = state.doc_engine.search_limited(
            namespace,
            req.agent.as_deref(),
            req.session.as_deref(),
            None,
            &req.tags,
            FILTER_DELETE_SCAN_LIMIT + 1,
        )?;
        if memories.len() > FILTER_DELETE_SCAN_LIMIT {
            return Err(AppError::BadRequest(format!(
                "certified delete matches more than {} memories; narrow the filter or delete by ids",
                FILTER_DELETE_SCAN_LIMIT
            )));
        }
        for m in &memories {
            if let Some(before) = &req.before
                && m.created_at >= *before
            {
                continue;
            }
            let hash = m
                .content_hash
                .clone()
                .unwrap_or_else(|| format!("{:x}", sha2::Sha256::digest(m.content.as_bytes())));
            to_delete.push((m.id, hash));
        }
    }

    if to_delete.is_empty() {
        return Err(AppError::NotFound(
            "no matching memories to delete".to_string(),
        ));
    }

    // Build Merkle tree of content hashes
    let leaf_hashes: Vec<String> = to_delete.iter().map(|(_, h)| h.clone()).collect();
    let merkle_root = compute_merkle_root(&leaf_hashes);

    // Perform actual deletion
    let mut deleted = 0usize;
    for (id, _) in &to_delete {
        if state.doc_engine.delete(*id, namespace, &claims.sub)? {
            state
                .indexer_state
                .write_seq
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            deleted += 1;
        }
    }

    if deleted > 0 {
        state.indexer_state.wake();
        state.intent_cache.invalidate_namespace(namespace).await;
    }

    // Generate signed deletion certificate
    let certificate = json!({
        "type": "deletion_certificate",
        "version": 1,
        "namespace": namespace,
        "deleted_count": deleted,
        "deleted_ids": to_delete.iter().map(|(id, _)| id.to_string()).collect::<Vec<_>>(),
        "merkle_root": merkle_root,
        "leaf_hashes": leaf_hashes,
        "deleted_at": chrono::Utc::now(),
        "deleted_by": claims.sub,
    });

    // B10 FIX: derive a separate signing key for deletion certs (not JWT secret)
    let cert_bytes = serde_json::to_vec(&certificate)
        .map_err(|e| AppError::Internal(anyhow::anyhow!("JSON serialization failed: {e}")))?;
    use hmac::Mac;
    use sha2::Digest;
    let cert_key = {
        let mut h = sha2::Sha256::new();
        h.update(state.config.auth.audit_hmac_secret.as_bytes());
        h.update(b"memoryoss-deletion-cert-key");
        h.finalize()
    };
    let mut mac = hmac::Hmac::<sha2::Sha256>::new_from_slice(&cert_key)
        .map_err(|_| AppError::Internal(anyhow::anyhow!("HMAC key error")))?;
    mac.update(&cert_bytes);
    let signature = hex::encode(mac.finalize().into_bytes());

    Ok(Json(json!({
        "deleted": deleted,
        "certificate": certificate,
        "signature": signature,
    }))
    .into_response())
}

/// Compute a Merkle root from a list of hex-encoded hash strings.
fn compute_merkle_root(hashes: &[String]) -> String {
    use sha2::Digest;
    if hashes.is_empty() {
        return String::new();
    }
    if hashes.len() == 1 {
        return hashes[0].clone();
    }

    let mut current: Vec<String> = hashes.to_vec();
    while current.len() > 1 {
        let mut next = Vec::new();
        for pair in current.chunks(2) {
            let combined = if pair.len() == 2 {
                format!("{}{}", pair[0], pair[1])
            } else {
                format!("{}{}", pair[0], pair[0]) // duplicate last element
            };
            let hash = sha2::Sha256::digest(combined.as_bytes());
            next.push(format!("{:x}", hash));
        }
        current = next;
    }
    current.into_iter().next().unwrap_or_default()
}

async fn metrics(State(state): State<AppState>, parts: Parts) -> Result<Response, AppError> {
    let _claims = require_auth(&state.config, &parts)?;
    let lag = state.indexer_state.lag();
    let write_seq = state
        .indexer_state
        .write_seq
        .load(std::sync::atomic::Ordering::Relaxed);
    let vec_seq = state
        .indexer_state
        .vector_seq
        .load(std::sync::atomic::Ordering::Relaxed);
    let fts_seq = state
        .indexer_state
        .fts_seq
        .load(std::sync::atomic::Ordering::Relaxed);
    let vec_size = state.vector_engine.size();
    let (cache_valid, cache_total) = state.embedding.cache_stats().await;
    let intent_stats = state.intent_cache.stats().await;
    let (prefetch_agents, prefetch_queries) = state.prefetcher.stats().await;
    let (idf_total_docs, _) = state.idf_index.stats();

    // Request counters from atomic state
    let stores = state
        .metrics
        .stores
        .load(std::sync::atomic::Ordering::Relaxed);
    let recalls = state
        .metrics
        .recalls
        .load(std::sync::atomic::Ordering::Relaxed);
    let forgets = state
        .metrics
        .forgets
        .load(std::sync::atomic::Ordering::Relaxed);
    let body = format!(
        "# HELP memoryoss_stores_total Total store requests\n\
         # TYPE memoryoss_stores_total counter\n\
         memoryoss_stores_total {stores}\n\
         # HELP memoryoss_recalls_total Total recall requests\n\
         # TYPE memoryoss_recalls_total counter\n\
         memoryoss_recalls_total {recalls}\n\
         # HELP memoryoss_forgets_total Total forget requests\n\
         # TYPE memoryoss_forgets_total counter\n\
         memoryoss_forgets_total {forgets}\n\
         # HELP memoryoss_vector_index_size Number of vectors in the index\n\
         # TYPE memoryoss_vector_index_size gauge\n\
         memoryoss_vector_index_size {vec_size}\n\
         # HELP memoryoss_indexer_lag Outbox events pending processing\n\
         # TYPE memoryoss_indexer_lag gauge\n\
         memoryoss_indexer_lag {lag}\n\
         # HELP memoryoss_indexer_write_seq Latest write sequence number\n\
         # TYPE memoryoss_indexer_write_seq counter\n\
         memoryoss_indexer_write_seq {write_seq}\n\
         # HELP memoryoss_indexer_vector_seq Vector indexer sequence\n\
         # TYPE memoryoss_indexer_vector_seq counter\n\
         memoryoss_indexer_vector_seq {vec_seq}\n\
         # HELP memoryoss_indexer_fts_seq FTS indexer sequence\n\
         # TYPE memoryoss_indexer_fts_seq counter\n\
         memoryoss_indexer_fts_seq {fts_seq}\n\
         # HELP memoryoss_embedding_cache_valid Valid embedding cache entries\n\
         # TYPE memoryoss_embedding_cache_valid gauge\n\
         memoryoss_embedding_cache_valid {cache_valid}\n\
         # HELP memoryoss_embedding_cache_total Total embedding cache entries\n\
         # TYPE memoryoss_embedding_cache_total gauge\n\
         memoryoss_embedding_cache_total {cache_total}\n\
         # HELP memoryoss_intent_cache_entries Intent cache entries\n\
         # TYPE memoryoss_intent_cache_entries gauge\n\
         memoryoss_intent_cache_entries {}\n\
         # HELP memoryoss_intent_cache_hits Intent cache hit count\n\
         # TYPE memoryoss_intent_cache_hits counter\n\
         memoryoss_intent_cache_hits {}\n\
         # HELP memoryoss_idf_total_docs Total documents in IDF index\n\
         # TYPE memoryoss_idf_total_docs gauge\n\
         memoryoss_idf_total_docs {idf_total_docs}\n\
         # HELP memoryoss_prefetch_tracked_agents Agents with recorded query patterns\n\
         # TYPE memoryoss_prefetch_tracked_agents gauge\n\
         memoryoss_prefetch_tracked_agents {prefetch_agents}\n\
         # HELP memoryoss_prefetch_recorded_queries Total recorded queries for prefetching\n\
         # TYPE memoryoss_prefetch_recorded_queries gauge\n\
         memoryoss_prefetch_recorded_queries {prefetch_queries}\n\
         # HELP memoryoss_group_commit_queue_utilization Group commit queue utilization ratio\n\
         # TYPE memoryoss_group_commit_queue_utilization gauge\n\
         memoryoss_group_commit_queue_utilization {gc_util:.4}\n\
         # HELP memoryoss_group_commit_flushes_total Total group commit flushes\n\
         # TYPE memoryoss_group_commit_flushes_total counter\n\
         memoryoss_group_commit_flushes_total {gc_flushes}\n\
         # HELP memoryoss_group_commit_ops_total Total ops committed via group commit\n\
         # TYPE memoryoss_group_commit_ops_total counter\n\
         memoryoss_group_commit_ops_total {gc_ops}\n\
         # HELP memoryoss_proxy_requests_total Total proxy requests\n\
         # TYPE memoryoss_proxy_requests_total counter\n\
         memoryoss_proxy_requests_total {proxy_reqs}\n\
         # HELP memoryoss_proxy_memories_injected_total Total memories injected via proxy\n\
         # TYPE memoryoss_proxy_memories_injected_total counter\n\
         memoryoss_proxy_memories_injected_total {proxy_injected}\n\
         # HELP memoryoss_proxy_gate_inject_total Total proxy requests where the confidence gate chose inject\n\
         # TYPE memoryoss_proxy_gate_inject_total counter\n\
         memoryoss_proxy_gate_inject_total {proxy_gate_inject}\n\
         # HELP memoryoss_proxy_gate_abstain_total Total proxy requests where the confidence gate chose abstain\n\
         # TYPE memoryoss_proxy_gate_abstain_total counter\n\
         memoryoss_proxy_gate_abstain_total {proxy_gate_abstain}\n\
         # HELP memoryoss_proxy_gate_need_more_evidence_total Total proxy requests where the confidence gate chose need_more_evidence\n\
         # TYPE memoryoss_proxy_gate_need_more_evidence_total counter\n\
         memoryoss_proxy_gate_need_more_evidence_total {proxy_gate_need_more_evidence}\n\
         # HELP memoryoss_proxy_policy_block_total Total proxy requests blocked by the policy firewall\n\
         # TYPE memoryoss_proxy_policy_block_total counter\n\
         memoryoss_proxy_policy_block_total {proxy_policy_block}\n\
         # HELP memoryoss_proxy_policy_require_confirmation_total Total proxy requests that required explicit policy confirmation\n\
         # TYPE memoryoss_proxy_policy_require_confirmation_total counter\n\
         memoryoss_proxy_policy_require_confirmation_total {proxy_policy_require_confirmation}\n\
         # HELP memoryoss_proxy_policy_warn_total Total proxy requests that triggered a policy warning\n\
         # TYPE memoryoss_proxy_policy_warn_total counter\n\
         memoryoss_proxy_policy_warn_total {proxy_policy_warn}\n\
         # HELP memoryoss_proxy_facts_extracted_total Total facts extracted via proxy\n\
         # TYPE memoryoss_proxy_facts_extracted_total counter\n\
         memoryoss_proxy_facts_extracted_total {proxy_extracted}\n\
         # HELP memoryoss_proxy_upstream_errors_total Total proxy upstream errors\n\
         # TYPE memoryoss_proxy_upstream_errors_total counter\n\
         memoryoss_proxy_upstream_errors_total {proxy_errors}\n",
        intent_stats.entries,
        intent_stats.hits,
        gc_util = state.group_committer.queue_utilization(),
        gc_flushes = state
            .group_committer
            .flushes
            .load(std::sync::atomic::Ordering::Relaxed),
        gc_ops = state
            .group_committer
            .ops_committed
            .load(std::sync::atomic::Ordering::Relaxed),
        proxy_reqs = state
            .metrics
            .proxy_requests
            .load(std::sync::atomic::Ordering::Relaxed),
        proxy_injected = state
            .metrics
            .proxy_memories_injected
            .load(std::sync::atomic::Ordering::Relaxed),
        proxy_gate_inject = state
            .metrics
            .proxy_gate_inject
            .load(std::sync::atomic::Ordering::Relaxed),
        proxy_gate_abstain = state
            .metrics
            .proxy_gate_abstain
            .load(std::sync::atomic::Ordering::Relaxed),
        proxy_gate_need_more_evidence = state
            .metrics
            .proxy_gate_need_more_evidence
            .load(std::sync::atomic::Ordering::Relaxed),
        proxy_policy_block = state
            .metrics
            .proxy_policy_block
            .load(std::sync::atomic::Ordering::Relaxed),
        proxy_policy_require_confirmation = state
            .metrics
            .proxy_policy_require_confirmation
            .load(std::sync::atomic::Ordering::Relaxed),
        proxy_policy_warn = state
            .metrics
            .proxy_policy_warn
            .load(std::sync::atomic::Ordering::Relaxed),
        proxy_extracted = state
            .metrics
            .proxy_facts_extracted
            .load(std::sync::atomic::Ordering::Relaxed),
        proxy_errors = state
            .metrics
            .proxy_upstream_errors
            .load(std::sync::atomic::Ordering::Relaxed),
    );

    Ok((
        StatusCode::OK,
        [(
            axum::http::header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        body,
    )
        .into_response())
}

/// Encode cursor as "score:uuid" (opaque to client).
fn encode_cursor(score: f64, id: uuid::Uuid) -> String {
    format!("{:.16}:{}", score, id)
}

/// Decode cursor from "score:uuid" format.
fn decode_cursor(cursor: &str) -> Option<(f64, uuid::Uuid)> {
    let parts: Vec<&str> = cursor.splitn(2, ':').collect();
    if parts.len() != 2 {
        return None;
    }
    let score: f64 = parts[0].trim().parse().ok()?;
    let id: uuid::Uuid = parts[1].parse().ok()?;
    Some((score, id))
}

fn cosine_similarity(a: &[f32], b: &[f32]) -> f64 {
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }
    (dot / (norm_a * norm_b)) as f64
}

pub enum AppError {
    BadRequest(String),
    Unauthorized(&'static str),
    Forbidden(&'static str),
    NotFound(String),
    RateLimited(u64),
    Internal(anyhow::Error),
}

impl From<anyhow::Error> for AppError {
    fn from(err: anyhow::Error) -> Self {
        AppError::Internal(err)
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            AppError::BadRequest(msg) => (StatusCode::BAD_REQUEST, msg),
            AppError::Unauthorized(msg) => (StatusCode::UNAUTHORIZED, msg.to_string()),
            AppError::Forbidden(msg) => (StatusCode::FORBIDDEN, msg.to_string()),
            AppError::NotFound(msg) => (StatusCode::NOT_FOUND, msg.to_string()),
            AppError::RateLimited(retry_ms) => {
                let body = json!({"error": "rate limit exceeded", "retry_after_ms": retry_ms});
                return (
                    StatusCode::TOO_MANY_REQUESTS,
                    [(
                        axum::http::header::RETRY_AFTER,
                        format!("{}", (retry_ms as f64 / 1000.0).ceil() as u64),
                    )],
                    Json(body),
                )
                    .into_response();
            }
            AppError::Internal(err) => {
                tracing::error!("internal error: {err:#}");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal server error".to_string(),
                )
            }
        };

        let body = json!({"error": message});
        (status, Json(body)).into_response()
    }
}

// ── Key rotation endpoints ─────────────────────────────────────────────

#[derive(Deserialize)]
struct RotateKeyRequest {
    #[serde(default = "default_rotate_namespace")]
    namespace: String,
}

fn default_rotate_namespace() -> String {
    "default".to_string()
}

/// POST /v1/admin/keys/rotate — Rotate encryption key for a namespace.
/// Old key kept for configurable grace period (default 24h).
async fn rotate_key(
    State(state): State<AppState>,
    parts: Parts,
    Json(req): Json<RotateKeyRequest>,
) -> Result<Response, AppError> {
    let claims = require_auth(&state.config, &parts)?;
    if !rbac::can_admin(claims.role) {
        return Err(AppError::Forbidden("admin required for key rotation"));
    }

    // Prevent cross-tenant key rotation
    if req.namespace != claims.namespace {
        return Err(AppError::Forbidden(
            "cannot rotate keys for another namespace",
        ));
    }

    let key_id = state
        .doc_engine
        .encryptor()
        .rotate_namespace(&req.namespace)
        .map_err(AppError::Internal)?;

    Ok(Json(json!({
        "rotated": true,
        "key_id": key_id,
        "namespace": req.namespace,
        "grace_period_secs": state.config.encryption.grace_period_secs.unwrap_or(86400),
    }))
    .into_response())
}

/// DELETE /v1/admin/keys/{id} — Immediately revoke a retired key.
async fn revoke_key(
    State(state): State<AppState>,
    parts: Parts,
    axum::extract::Path(key_id): axum::extract::Path<String>,
) -> Result<Response, AppError> {
    let claims = require_auth(&state.config, &parts)?;
    if !rbac::can_admin(claims.role) {
        return Err(AppError::Forbidden("admin required for key revocation"));
    }

    let revoked = state.doc_engine.encryptor().revoke_key(&key_id);

    if revoked {
        Ok(Json(json!({"revoked": true, "key_id": key_id})).into_response())
    } else {
        Err(AppError::NotFound(format!(
            "retired key not found: {key_id}"
        )))
    }
}

/// GET /v1/admin/keys — List retired keys still within grace period.
async fn list_keys(State(state): State<AppState>, parts: Parts) -> Result<Response, AppError> {
    let claims = require_auth(&state.config, &parts)?;
    if !rbac::can_admin(claims.role) {
        return Err(AppError::Forbidden("admin required to list keys"));
    }

    let keys = state.doc_engine.encryptor().list_retired_keys();
    Ok(Json(json!({"retired_keys": keys})).into_response())
}

/// GET /v1/admin/trust-stats — Source reputation and trust config.
async fn trust_stats(State(state): State<AppState>, parts: Parts) -> Result<Response, AppError> {
    let claims = require_auth(&state.config, &parts)?;
    if !rbac::can_admin(claims.role) {
        return Err(AppError::Forbidden("admin required for trust stats"));
    }

    let sources = state.trust_scorer.source_stats();
    Ok(Json(json!({
        "threshold": state.trust_scorer.threshold(),
        "source_reputations": sources,
    }))
    .into_response())
}

#[derive(Debug, Deserialize)]
struct LifecycleViewParams {
    #[serde(default)]
    namespace: Option<String>,
    #[serde(default)]
    status: Option<MemoryStatus>,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    include_archived: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct RecentActivityParams {
    #[serde(default)]
    namespace: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct HudParams {
    #[serde(default)]
    namespace: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    format: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ReviewQueueParams {
    #[serde(default)]
    namespace: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct ReviewActionRequest {
    #[serde(default)]
    namespace: Option<String>,
    review_key: String,
    action: MemoryFeedbackAction,
    #[serde(default)]
    supersede_with_review_key: Option<String>,
}

/// GET /v1/admin/lifecycle — Status breakdown and latest memories by lifecycle state.
async fn lifecycle_view(
    State(state): State<AppState>,
    parts: Parts,
    axum::extract::Query(params): axum::extract::Query<LifecycleViewParams>,
) -> Result<Response, AppError> {
    let claims = require_auth(&state.config, &parts)?;
    if !rbac::can_admin(claims.role) {
        return Err(AppError::Forbidden("admin required for lifecycle view"));
    }

    let namespace = enforce_namespace(&claims, params.namespace.as_deref())?;
    let lifecycle_updates = run_namespace_lifecycle(
        &state,
        namespace,
        &claims.sub,
        state.config.decay.after_days,
    )?;
    if lifecycle_updates > 0 {
        state.intent_cache.invalidate_namespace(namespace).await;
    }
    let all_memories = state.doc_engine.list_all_including_archived(namespace)?;
    let summary = lifecycle_summary_from_memories(&all_memories);

    let include_archived = params.include_archived.unwrap_or(false);
    let status_filter = params.status;
    let limit = params.limit.unwrap_or(25).clamp(1, 100);

    let mut filtered = all_memories
        .into_iter()
        .filter(|memory| include_archived || !memory.archived)
        .filter(|memory| status_filter.is_none_or(|status| memory.status == status))
        .collect::<Vec<_>>();
    filtered.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));

    let memories: Vec<_> = filtered
        .into_iter()
        .take(limit)
        .map(|memory| {
            let trust = state.trust_scorer.score_memory(&memory, namespace);
            json!({
                "id": memory.id,
                "content": memory.content,
                "tags": memory.tags,
                "agent": memory.agent,
                "session": memory.session,
                "memory_type": memory.memory_type,
                "status": memory.status,
                "archived": memory.archived,
                "confidence": memory.confidence,
                "evidence_count": memory.evidence_count,
                "last_verified_at": memory.last_verified_at,
                "superseded_by": memory.superseded_by,
                "derived_from": memory.derived_from,
                "contradicts_with": memory.contradicts_with,
                "injection_count": memory.injection_count,
                "reuse_count": memory.reuse_count,
                "confirm_count": memory.confirm_count,
                "reject_count": memory.reject_count,
                "supersede_count": memory.supersede_count,
                "last_injected_at": memory.last_injected_at,
                "last_reused_at": memory.last_reused_at,
                "last_outcome_at": memory.last_outcome_at,
                "created_at": memory.created_at,
                "updated_at": memory.updated_at,
                "trust_score": trust.score,
                "trust_confidence_low": trust.confidence_low,
                "trust_confidence_high": trust.confidence_high,
                "trust_signals": trust.signals,
                "low_trust": trust.low_trust,
                "eligible_for_injection": memory.eligible_for_injection(),
            })
        })
        .collect();

    Ok(Json(json!({
        "namespace": namespace,
        "summary": summary,
        "filters": {
            "status": status_filter,
            "limit": limit,
            "include_archived": include_archived,
        },
        "memories": memories,
    }))
    .into_response())
}

/// GET /v1/admin/recent — grouped recent injections, extractions, feedbacks, consolidations.
async fn recent_activity_view(
    State(state): State<AppState>,
    parts: Parts,
    axum::extract::Query(params): axum::extract::Query<RecentActivityParams>,
) -> Result<Response, AppError> {
    let claims = require_auth(&state.config, &parts)?;
    if !rbac::can_admin(claims.role) {
        return Err(AppError::Forbidden("admin required for recent activity"));
    }

    let namespace = enforce_namespace(&claims, params.namespace.as_deref())?;
    let limit = params
        .limit
        .unwrap_or(DEFAULT_RECENT_ACTIVITY_LIMIT)
        .clamp(1, MAX_RECENT_ACTIVITY_LIMIT);
    let memories = state.doc_engine.list_all_including_archived(namespace)?;
    let recent = build_recent_activity(&memories, limit);

    Ok(Json(json!({
        "namespace": namespace,
        "limit": limit,
        "recent": recent,
    }))
    .into_response())
}

/// GET /v1/admin/hud — unified operator HUD for search, review, policy, and import/export loops.
async fn hud_view(
    State(state): State<AppState>,
    parts: Parts,
    axum::extract::Query(params): axum::extract::Query<HudParams>,
) -> Result<Response, AppError> {
    let claims = require_auth(&state.config, &parts)?;
    if !rbac::can_admin(claims.role) {
        return Err(AppError::Forbidden("admin required for HUD"));
    }

    let namespace = enforce_namespace(&claims, params.namespace.as_deref())?;
    let limit = params
        .limit
        .unwrap_or(DEFAULT_HUD_LIMIT)
        .clamp(1, MAX_HUD_LIMIT);
    let memories = state.doc_engine.list_all_including_archived(namespace)?;
    let hud = build_hud_view(
        &[(namespace.to_string(), memories)],
        &state.trust_scorer,
        limit,
        Some(namespace.to_string()),
        Some(hud_policy_counter_snapshot(&state.metrics)),
    );

    match params.format.as_deref().unwrap_or("json") {
        "json" => Ok(Json(hud).into_response()),
        "html" => Ok((
            StatusCode::OK,
            [(axum::http::header::CONTENT_TYPE, "text/html; charset=utf-8")],
            render_hud_html(&hud),
        )
            .into_response()),
        other => Err(AppError::BadRequest(format!(
            "unsupported HUD format: {other}"
        ))),
    }
}

/// GET /v1/admin/review-queue — candidate/contested/rejected memories with suggested actions.
async fn review_queue_view(
    State(state): State<AppState>,
    parts: Parts,
    axum::extract::Query(params): axum::extract::Query<ReviewQueueParams>,
) -> Result<Response, AppError> {
    let claims = require_auth(&state.config, &parts)?;
    if !rbac::can_admin(claims.role) {
        return Err(AppError::Forbidden("admin required for review queue"));
    }

    let namespace = enforce_namespace(&claims, params.namespace.as_deref())?;
    let limit = params
        .limit
        .unwrap_or(DEFAULT_REVIEW_QUEUE_LIMIT)
        .clamp(1, MAX_REVIEW_QUEUE_LIMIT);
    let memories = state.doc_engine.list_all_including_archived(namespace)?;
    let queue = build_review_queue(&memories, &state.trust_scorer, namespace, limit);

    Ok(Json(json!({
        "namespace": namespace,
        "limit": limit,
        "summary": queue.summary,
        "items": queue.items,
    }))
    .into_response())
}

/// POST /v1/admin/review/action — review queue action using review keys instead of raw UUIDs.
async fn review_queue_action(
    State(state): State<AppState>,
    parts: Parts,
    Json(req): Json<ReviewActionRequest>,
) -> Result<Response, AppError> {
    let claims = require_auth(&state.config, &parts)?;
    if !rbac::can_admin(claims.role) {
        return Err(AppError::Forbidden("admin required for review actions"));
    }

    let namespace = enforce_namespace(&claims, req.namespace.as_deref())?;
    let review_id =
        decode_review_key(&req.review_key).map_err(|msg| AppError::BadRequest(msg.to_string()))?;
    let superseded_by = req
        .supersede_with_review_key
        .as_deref()
        .map(decode_review_key)
        .transpose()
        .map_err(|msg| AppError::BadRequest(msg.to_string()))?;

    if superseded_by == Some(review_id) {
        return Err(AppError::BadRequest(
            "memory cannot supersede itself".to_string(),
        ));
    }
    if matches!(req.action, MemoryFeedbackAction::Supersede) && superseded_by.is_none() {
        return Err(AppError::BadRequest(
            "supersede action requires supersede_with_review_key".to_string(),
        ));
    }

    let mut memory = state
        .doc_engine
        .get(review_id, namespace)?
        .ok_or_else(|| AppError::NotFound("review memory not found".to_string()))?;
    let mut replacement = if let Some(target_id) = superseded_by {
        Some(
            state
                .doc_engine
                .get(target_id, namespace)?
                .ok_or_else(|| AppError::NotFound("replacement memory not found".to_string()))?,
        )
    } else {
        None
    };
    let queue_kind_before = memory.review_queue_kind();

    apply_feedback_to_memory(
        &mut memory,
        req.action,
        superseded_by,
        &claims.sub,
        "review_inbox",
    );
    state.doc_engine.replace(&memory, &claims.sub)?;
    state.trust_scorer.record_feedback(
        memory.source_key.as_deref(),
        matches!(req.action, MemoryFeedbackAction::Confirm),
    );

    let mut replacement_review_event_count = None;
    if let Some(target) = replacement.as_mut()
        && matches!(req.action, MemoryFeedbackAction::Supersede)
    {
        apply_feedback_to_memory(
            target,
            MemoryFeedbackAction::Confirm,
            None,
            &claims.sub,
            "review_inbox_supersede_target",
        );
        state.doc_engine.replace(target, &claims.sub)?;
        state
            .trust_scorer
            .record_feedback(target.source_key.as_deref(), true);
        replacement_review_event_count = Some(target.review_events.len());
    }

    state
        .indexer_state
        .write_seq
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    state.indexer_state.wake();
    refresh_review_queue_summary(&state, namespace)?;
    state.intent_cache.invalidate_namespace(namespace).await;

    Ok(Json(json!({
        "review_key": req.review_key,
        "queue_kind_before": queue_kind_before,
        "action": req.action,
        "namespace": namespace,
        "memory": {
            "review_key": encode_review_key(memory.id),
            "status": memory.status,
            "confidence": memory.confidence,
            "evidence_count": memory.evidence_count,
            "superseded_by": memory.superseded_by,
            "contradicts_with": memory.contradicts_with,
            "confirm_count": memory.confirm_count,
            "reject_count": memory.reject_count,
            "supersede_count": memory.supersede_count,
            "review_event_count": memory.review_events.len(),
            "last_review": memory.review_events.last(),
        },
        "replacement": replacement.as_ref().map(|target| json!({
            "review_key": encode_review_key(target.id),
            "status": target.status,
            "review_event_count": replacement_review_event_count.unwrap_or(target.review_events.len()),
            "last_review": target.review_events.last(),
        })),
    }))
    .into_response())
}

// ── Intent cache endpoints ─────────────────────────────────────────────

async fn intent_cache_stats(
    State(state): State<AppState>,
    parts: Parts,
) -> Result<Response, AppError> {
    let claims = require_auth(&state.config, &parts)?;
    if !rbac::can_admin(claims.role) {
        return Err(AppError::Forbidden("admin required for intent cache stats"));
    }
    let stats = state.intent_cache.stats().await;
    Ok(Json(stats).into_response())
}

async fn flush_intent_cache(
    State(state): State<AppState>,
    parts: Parts,
) -> Result<Response, AppError> {
    let claims = require_auth(&state.config, &parts)?;
    if !rbac::can_admin(claims.role) {
        return Err(AppError::Forbidden("admin required for intent cache flush"));
    }
    let flushed = state.intent_cache.flush().await;
    tracing::info!(flushed, "intent cache flushed");
    Ok(Json(json!({"flushed": flushed})).into_response())
}

async fn prefetch_stats(State(state): State<AppState>, parts: Parts) -> Result<Response, AppError> {
    let claims = require_auth(&state.config, &parts)?;
    if !rbac::can_admin(claims.role) {
        return Err(AppError::Forbidden("admin required for prefetch stats"));
    }
    let (agents, total_queries) = state.prefetcher.stats().await;
    Ok(Json(json!({
        "tracked_agents": agents,
        "total_recorded_queries": total_queries,
    }))
    .into_response())
}

// ── Sharing endpoints ──────────────────────────────────────────────────

#[derive(Deserialize)]
struct CreateSharedNsRequest {
    name: String,
    webhook_url: Option<String>,
}

/// POST /v1/admin/sharing — Create a shared namespace.
async fn create_shared_ns(
    State(state): State<AppState>,
    parts: Parts,
    Json(req): Json<CreateSharedNsRequest>,
) -> Result<Response, AppError> {
    let claims = require_auth(&state.config, &parts)?;
    if !rbac::can_admin(claims.role) {
        return Err(AppError::Forbidden(
            "admin required to create shared namespaces",
        ));
    }

    let ns = match state.sharing_store.create_shared_namespace(
        &req.name,
        &claims.namespace,
        req.webhook_url.as_deref(),
    ) {
        Ok(ns) => ns,
        Err(e) => {
            tracing::error!("sharing create error: {e:?}");
            return Err(AppError::Internal(e));
        }
    };

    Ok(Json(json!({
        "name": ns.name,
        "owner_namespace": ns.owner_namespace,
        "created_at": ns.created_at.to_rfc3339(),
        "webhook_url": ns.webhook_url,
        "grants": ns.grants.len(),
    }))
    .into_response())
}

/// GET /v1/admin/sharing — List shared namespaces scoped to caller's namespace.
async fn list_shared_ns(State(state): State<AppState>, parts: Parts) -> Result<Response, AppError> {
    let claims = require_auth(&state.config, &parts)?;
    if !rbac::can_admin(claims.role) {
        return Err(AppError::Forbidden(
            "admin required to list shared namespaces",
        ));
    }

    // Scope to caller's namespace: only show namespaces they own or have grants for
    let accessible = state
        .sharing_store
        .accessible_namespaces(&claims.namespace)
        .map_err(AppError::Internal)?;
    let mut namespaces = Vec::new();
    for name in &accessible {
        if let Ok(Some(ns)) = state.sharing_store.get_shared_namespace(name) {
            namespaces.push(ns);
        }
    }
    Ok(Json(json!({"shared_namespaces": namespaces})).into_response())
}

/// DELETE /v1/admin/sharing/{name} — Delete a shared namespace.
async fn delete_shared_ns(
    State(state): State<AppState>,
    parts: Parts,
    axum::extract::Path(name): axum::extract::Path<String>,
) -> Result<Response, AppError> {
    let claims = require_auth(&state.config, &parts)?;
    if !rbac::can_admin(claims.role) {
        return Err(AppError::Forbidden("admin required"));
    }

    state
        .sharing_store
        .delete_shared_namespace(&name, &claims.namespace)
        .map_err(AppError::Internal)?;

    Ok(Json(json!({"deleted": true, "name": name})).into_response())
}

#[derive(Deserialize)]
struct AddGrantRequest {
    grantee_namespace: String,
    permission: crate::sharing::SharePermission,
    #[serde(default)]
    tag_filter: Option<Vec<String>>,
    #[serde(default)]
    agent_filter: Option<Vec<String>>,
    #[serde(default)]
    expires_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// POST /v1/admin/sharing/{name}/grants — Add a grant.
async fn add_sharing_grant(
    State(state): State<AppState>,
    parts: Parts,
    axum::extract::Path(name): axum::extract::Path<String>,
    Json(req): Json<AddGrantRequest>,
) -> Result<Response, AppError> {
    let claims = require_auth(&state.config, &parts)?;
    if !rbac::can_admin(claims.role) {
        return Err(AppError::Forbidden("admin required"));
    }

    let grant = state
        .sharing_store
        .add_grant(
            &name,
            &req.grantee_namespace,
            req.permission,
            req.tag_filter,
            req.agent_filter,
            req.expires_at,
            &claims.namespace,
        )
        .map_err(AppError::Internal)?;

    Ok(Json(json!(grant)).into_response())
}

/// GET /v1/admin/sharing/{name}/grants — List grants (owner or grantee only).
async fn list_sharing_grants(
    State(state): State<AppState>,
    parts: Parts,
    axum::extract::Path(name): axum::extract::Path<String>,
) -> Result<Response, AppError> {
    let claims = require_auth(&state.config, &parts)?;
    // Only owner of the shared namespace or someone with a grant can list grants
    let has_access = state
        .sharing_store
        .check_permission(
            &name,
            &claims.namespace,
            crate::sharing::SharePermission::Read,
        )
        .unwrap_or(false);
    if !has_access {
        return Err(AppError::Forbidden(
            "can only list grants for namespaces you own or have access to",
        ));
    }

    let grants = state
        .sharing_store
        .list_grants(&name)
        .map_err(AppError::Internal)?;
    Ok(Json(json!({"grants": grants})).into_response())
}

/// DELETE /v1/admin/sharing/{name}/grants/{grant_id} — Remove a grant.
async fn remove_sharing_grant(
    State(state): State<AppState>,
    parts: Parts,
    axum::extract::Path((name, grant_id)): axum::extract::Path<(String, String)>,
) -> Result<Response, AppError> {
    let claims = require_auth(&state.config, &parts)?;
    if !rbac::can_admin(claims.role) {
        return Err(AppError::Forbidden("admin required"));
    }

    let grant_uuid = uuid::Uuid::parse_str(&grant_id)
        .map_err(|_| AppError::BadRequest("invalid grant UUID".to_string()))?;

    state
        .sharing_store
        .remove_grant(&name, grant_uuid, &claims.namespace)
        .map_err(AppError::Internal)?;

    Ok(Json(json!({"removed": true})).into_response())
}

/// GET /v1/sharing/accessible — List shared namespaces accessible by current token.
async fn accessible_shared_ns(
    State(state): State<AppState>,
    parts: Parts,
) -> Result<Response, AppError> {
    let claims = require_auth(&state.config, &parts)?;

    let accessible = state
        .sharing_store
        .accessible_namespaces(&claims.namespace)
        .map_err(AppError::Internal)?;

    Ok(Json(json!({"accessible": accessible})).into_response())
}
