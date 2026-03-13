// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 memoryOSS Contributors

use std::collections::HashSet;

use crate::memory::{
    Memory, MemoryStatus, MemorySummaryEntry, ScoreExplainEntry, ScoredMemory, SummaryEvidenceItem,
};

const MIN_SUBSTRING_TOKENS: usize = 5;
const MIN_JACCARD_TOKENS: usize = 6;
const JACCARD_DUP_THRESHOLD: f64 = 0.92;

#[derive(Debug, Clone)]
struct TextShape {
    normalized: String,
    tokens: Vec<String>,
    token_set: HashSet<String>,
}

fn text_shape(content: &str) -> TextShape {
    let mut tokens = Vec::new();
    let mut current = String::new();

    for ch in content.chars() {
        if ch.is_alphanumeric() {
            current.extend(ch.to_lowercase());
        } else if !current.is_empty() {
            tokens.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }

    let normalized = tokens.join(" ");
    let token_set = tokens.iter().cloned().collect();

    TextShape {
        normalized,
        tokens,
        token_set,
    }
}

fn token_jaccard(a: &TextShape, b: &TextShape) -> f64 {
    if a.token_set.is_empty() || b.token_set.is_empty() {
        return 0.0;
    }
    let intersection = a.token_set.intersection(&b.token_set).count();
    let union = a.token_set.union(&b.token_set).count();
    if union == 0 {
        0.0
    } else {
        intersection as f64 / union as f64
    }
}

fn substantially_contains(a: &TextShape, b: &TextShape) -> bool {
    let (shorter, longer) = if a.tokens.len() <= b.tokens.len() {
        (a, b)
    } else {
        (b, a)
    };
    shorter.tokens.len() >= MIN_SUBSTRING_TOKENS
        && !shorter.normalized.is_empty()
        && longer.normalized.contains(&shorter.normalized)
}

fn split_sentences(content: &str) -> Vec<String> {
    content
        .split(['.', '!', '?', '\n'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToString::to_string)
        .collect()
}

fn truncate_preview(content: &str, max_chars: usize) -> String {
    let mut truncated = String::new();
    for ch in content.chars() {
        if truncated.chars().count() >= max_chars {
            break;
        }
        truncated.push(ch);
    }
    if truncated.len() < content.len() {
        truncated.push_str("...");
    }
    truncated
}

fn summary_sentence(content: &str) -> String {
    let sentences = split_sentences(content);
    if let Some(first) = sentences.first() {
        let first = first.trim();
        if first.chars().count() <= 140 {
            if first.ends_with('.') {
                first.to_string()
            } else {
                format!("{first}.")
            }
        } else {
            truncate_preview(first, 140)
        }
    } else {
        truncate_preview(content.trim(), 140)
    }
}

fn evidence_previews(content: &str) -> Vec<String> {
    let sentences = split_sentences(content);
    if sentences.is_empty() {
        return vec![truncate_preview(content.trim(), 120)];
    }

    let mut previews: Vec<String> = sentences
        .into_iter()
        .skip(1)
        .take(2)
        .map(|sentence| truncate_preview(sentence.trim(), 90))
        .filter(|preview| !preview.is_empty())
        .collect();

    if previews.is_empty() {
        previews.push(truncate_preview(content.trim(), 120));
    }

    previews
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

pub fn are_structural_duplicates(a: &str, b: &str) -> bool {
    let a_shape = text_shape(a);
    let b_shape = text_shape(b);

    if a_shape.normalized.is_empty() || b_shape.normalized.is_empty() {
        return false;
    }

    if a_shape.normalized == b_shape.normalized {
        return true;
    }

    if substantially_contains(&a_shape, &b_shape) {
        return true;
    }

    a_shape.tokens.len() >= MIN_JACCARD_TOKENS
        && b_shape.tokens.len() >= MIN_JACCARD_TOKENS
        && token_jaccard(&a_shape, &b_shape) >= JACCARD_DUP_THRESHOLD
}

pub fn fuse_contents(primary: &str, secondary: &str) -> String {
    let primary_shape = text_shape(primary);
    let secondary_shape = text_shape(secondary);

    if primary_shape.normalized == secondary_shape.normalized {
        return if primary.len() >= secondary.len() {
            primary.to_string()
        } else {
            secondary.to_string()
        };
    }

    if substantially_contains(&primary_shape, &secondary_shape) {
        return if primary_shape.tokens.len() >= secondary_shape.tokens.len() {
            primary.to_string()
        } else {
            secondary.to_string()
        };
    }

    let mut seen = HashSet::new();
    let mut merged = Vec::new();
    for sentence in split_sentences(primary)
        .into_iter()
        .chain(split_sentences(secondary).into_iter())
    {
        let canon = text_shape(&sentence).normalized;
        if canon.len() < 3 {
            continue;
        }
        if seen.insert(canon) {
            merged.push(sentence);
        }
    }

    if merged.len() > 1 {
        let fused = merged.join(". ");
        let fused = if fused.ends_with('.') {
            fused
        } else {
            format!("{fused}.")
        };
        if fused.len() <= primary.len().max(secondary.len()) + 240 {
            return fused;
        }
    }

    if primary.len() >= secondary.len() {
        primary.to_string()
    } else {
        secondary.to_string()
    }
}

fn merge_unique_strings(target: &mut Vec<String>, additions: &[String]) {
    let mut seen: HashSet<String> = target.iter().cloned().collect();
    for value in additions {
        if seen.insert(value.clone()) {
            target.push(value.clone());
        }
    }
}

fn prefer_scored(candidate: &ScoredMemory, existing: &ScoredMemory) -> bool {
    candidate.score > existing.score
        || ((candidate.score - existing.score).abs() < f64::EPSILON
            && candidate.memory.updated_at > existing.memory.updated_at)
        || ((candidate.score - existing.score).abs() < f64::EPSILON
            && candidate.memory.updated_at == existing.memory.updated_at
            && candidate.memory.content.len() > existing.memory.content.len())
}

fn prefer_explained(candidate: &ScoreExplainEntry, existing: &ScoreExplainEntry) -> bool {
    candidate.final_score > existing.final_score
        || ((candidate.final_score - existing.final_score).abs() < f64::EPSILON
            && candidate.memory.updated_at > existing.memory.updated_at)
        || ((candidate.final_score - existing.final_score).abs() < f64::EPSILON
            && candidate.memory.updated_at == existing.memory.updated_at
            && candidate.memory.content.len() > existing.memory.content.len())
}

fn content_matches_route_identifier(content: &str, identifier: &str) -> bool {
    let lowered = content.to_lowercase();
    let ident = identifier.trim().to_lowercase();
    if ident.is_empty() {
        return false;
    }
    if lowered.contains(&ident) {
        return true;
    }
    let parts: Vec<String> = ident
        .split(|ch: char| !ch.is_alphanumeric())
        .filter(|part| part.len() >= 2)
        .map(ToString::to_string)
        .collect();
    parts.len() >= 2 && parts.iter().all(|part| lowered.contains(part))
}

fn content_identifier_anchors(
    content: &str,
    route: &crate::scoring::IdentifierRouteProfile,
) -> HashSet<String> {
    crate::scoring::extract_identifiers(content)
        .into_iter()
        .map(|identifier| identifier.trim().to_lowercase())
        .filter(|identifier| !identifier.is_empty())
        .filter(|identifier| {
            route
                .kinds
                .iter()
                .any(|kind| crate::scoring::identifier_matches_kind(identifier, *kind))
        })
        .collect()
}

fn are_identifier_route_duplicates(
    a: &str,
    b: &str,
    route: &crate::scoring::IdentifierRouteProfile,
) -> bool {
    if are_structural_duplicates(a, b) {
        return true;
    }
    let shared_anchor = route.identifiers.iter().any(|ident| {
        content_matches_route_identifier(a, ident) && content_matches_route_identifier(b, ident)
    });
    if shared_anchor {
        return true;
    }

    let a_anchors = content_identifier_anchors(a, route);
    if a_anchors.is_empty() {
        return false;
    }

    let b_anchors = content_identifier_anchors(b, route);
    !b_anchors.is_empty() && !a_anchors.is_disjoint(&b_anchors)
}

pub fn collapse_scored_memories(scored: Vec<ScoredMemory>) -> Vec<ScoredMemory> {
    collapse_scored_memories_for_query(scored, None)
}

pub fn collapse_scored_memories_for_query(
    scored: Vec<ScoredMemory>,
    route: Option<&crate::scoring::IdentifierRouteProfile>,
) -> Vec<ScoredMemory> {
    let mut collapsed: Vec<ScoredMemory> = Vec::new();

    for candidate in scored {
        if let Some(existing) = collapsed.iter_mut().find(|entry| {
            are_structural_duplicates(&entry.memory.content, &candidate.memory.content)
                || route.is_some_and(|route| {
                    are_identifier_route_duplicates(
                        &entry.memory.content,
                        &candidate.memory.content,
                        route,
                    )
                })
        }) {
            let mut merged = if prefer_scored(&candidate, existing) {
                candidate.clone()
            } else {
                existing.clone()
            };
            let other = if merged.memory.id == candidate.memory.id {
                existing.clone()
            } else {
                candidate.clone()
            };

            merged.memory.content = fuse_contents(&merged.memory.content, &other.memory.content);
            merged.memory.content_hash =
                Some(crate::memory::Memory::compute_hash(&merged.memory.content));
            merge_unique_strings(&mut merged.memory.tags, &other.memory.tags);
            merge_unique_strings(&mut merged.provenance, &other.provenance);
            if !merged.provenance.iter().any(|p| p == "fused_duplicate") {
                merged.provenance.push("fused_duplicate".to_string());
            }
            merged.score = merged.score.max(other.score);
            merged.trust_score = merged.trust_score.max(other.trust_score);
            merged.low_trust = merged.low_trust && other.low_trust;
            *existing = merged;
        } else {
            collapsed.push(candidate);
        }
    }

    collapsed
}

#[allow(dead_code)]
pub fn collapse_explained_entries(explained: Vec<ScoreExplainEntry>) -> Vec<ScoreExplainEntry> {
    collapse_explained_entries_for_query(explained, None)
}

pub fn collapse_explained_entries_for_query(
    explained: Vec<ScoreExplainEntry>,
    route: Option<&crate::scoring::IdentifierRouteProfile>,
) -> Vec<ScoreExplainEntry> {
    let mut collapsed: Vec<ScoreExplainEntry> = Vec::new();

    for candidate in explained {
        if let Some(existing) = collapsed.iter_mut().find(|entry| {
            are_structural_duplicates(&entry.memory.content, &candidate.memory.content)
                || route.is_some_and(|route| {
                    are_identifier_route_duplicates(
                        &entry.memory.content,
                        &candidate.memory.content,
                        route,
                    )
                })
        }) {
            let mut merged = if prefer_explained(&candidate, existing) {
                candidate.clone()
            } else {
                existing.clone()
            };
            let other = if merged.memory.id == candidate.memory.id {
                existing.clone()
            } else {
                candidate.clone()
            };

            merged.memory.content = fuse_contents(&merged.memory.content, &other.memory.content);
            merged.memory.content_hash =
                Some(crate::memory::Memory::compute_hash(&merged.memory.content));
            merge_unique_strings(&mut merged.memory.tags, &other.memory.tags);
            merge_unique_strings(&mut merged.provenance, &other.provenance);
            if !merged.provenance.iter().any(|p| p == "fused_duplicate") {
                merged.provenance.push("fused_duplicate".to_string());
            }
            merged.max_channel_score = merged.max_channel_score.max(other.max_channel_score);
            merged.trust_score = merged.trust_score.max(other.trust_score);
            merged.trust_multiplier = merged.trust_multiplier.max(other.trust_multiplier);
            merged.final_score = merged.final_score.max(other.final_score);
            merged.low_trust = merged.low_trust && other.low_trust;
            merged.channels.vector = merged.channels.vector.max(other.channels.vector);
            merged.channels.fts = merged.channels.fts.max(other.channels.fts);
            merged.channels.exact = merged.channels.exact.max(other.channels.exact);
            *existing = merged;
        } else {
            collapsed.push(candidate);
        }
    }

    collapsed
}

pub fn build_scored_memory_summaries(scored: &[ScoredMemory]) -> Vec<MemorySummaryEntry> {
    scored
        .iter()
        .map(|entry| MemorySummaryEntry {
            memory_id: entry.memory.id,
            summary: summary_sentence(&entry.memory.content),
            score: entry.score,
            trust_score: entry.trust_score,
            low_trust: entry.low_trust,
            status: entry.memory.status,
            tags: entry.memory.tags.clone(),
            provenance: entry.provenance.clone(),
            evidence: evidence_previews(&entry.memory.content)
                .into_iter()
                .map(|preview| SummaryEvidenceItem {
                    preview,
                    provenance: entry.provenance.clone(),
                })
                .collect(),
        })
        .collect()
}

pub fn build_explained_memory_summaries(
    explained: &[ScoreExplainEntry],
) -> Vec<MemorySummaryEntry> {
    explained
        .iter()
        .map(|entry| MemorySummaryEntry {
            memory_id: entry.memory.id,
            summary: summary_sentence(&entry.memory.content),
            score: entry.final_score,
            trust_score: entry.trust_score,
            low_trust: entry.low_trust,
            status: entry.memory.status,
            tags: entry.memory.tags.clone(),
            provenance: entry.provenance.clone(),
            evidence: evidence_previews(&entry.memory.content)
                .into_iter()
                .map(|preview| SummaryEvidenceItem {
                    preview,
                    provenance: entry.provenance.clone(),
                })
                .collect(),
        })
        .collect()
}

pub fn should_cluster_memories(
    a: &crate::memory::Memory,
    b: &crate::memory::Memory,
    embedding_similarity: Option<f64>,
    threshold: f64,
) -> bool {
    are_structural_duplicates(&a.content, &b.content)
        || embedding_similarity.is_some_and(|sim| sim >= threshold)
}

pub fn prefer_consolidation_candidate(
    candidate: &crate::memory::Memory,
    existing: &crate::memory::Memory,
) -> bool {
    let candidate_status = candidate.status_factor();
    let existing_status = existing.status_factor();
    let candidate_conf = candidate.confidence.unwrap_or(1.0);
    let existing_conf = existing.confidence.unwrap_or(1.0);

    candidate_status > existing_status
        || ((candidate_status - existing_status).abs() < f64::EPSILON
            && candidate_conf > existing_conf)
        || ((candidate_status - existing_status).abs() < f64::EPSILON
            && (candidate_conf - existing_conf).abs() < f64::EPSILON
            && candidate.updated_at > existing.updated_at)
        || ((candidate_status - existing_status).abs() < f64::EPSILON
            && (candidate_conf - existing_conf).abs() < f64::EPSILON
            && candidate.updated_at == existing.updated_at
            && candidate.content.len() > existing.content.len())
        || ((candidate_status - existing_status).abs() < f64::EPSILON
            && (candidate_conf - existing_conf).abs() < f64::EPSILON
            && candidate.updated_at == existing.updated_at
            && candidate.content.len() == existing.content.len()
            && candidate.tags.len() > existing.tags.len())
}

#[derive(Debug, Clone)]
pub struct ConsolidationCluster {
    pub members: Vec<usize>,
    pub avg_similarity: f64,
}

fn eligible_for_consolidation(memory: &Memory) -> bool {
    !memory.archived
        && memory.superseded_by.is_none()
        && memory.status != MemoryStatus::Contested
        && memory.contradicts_with.is_empty()
}

pub fn count_active_memories(memories: &[Memory]) -> usize {
    memories
        .iter()
        .filter(|memory| memory.status == MemoryStatus::Active && !memory.archived)
        .count()
}

pub fn build_consolidation_clusters(
    memories: &[Memory],
    threshold: f64,
    max_clusters: usize,
) -> Vec<ConsolidationCluster> {
    let mut assigned = std::collections::HashSet::new();
    let mut clusters = Vec::new();

    for i in 0..memories.len() {
        if assigned.contains(&memories[i].id) || !eligible_for_consolidation(&memories[i]) {
            continue;
        }
        if clusters.len() >= max_clusters {
            break;
        }

        let mut cluster = vec![i];
        let mut sims = Vec::new();
        for j in (i + 1)..memories.len() {
            if assigned.contains(&memories[j].id) || !eligible_for_consolidation(&memories[j]) {
                continue;
            }
            let sim = match (&memories[i].embedding, &memories[j].embedding) {
                (Some(a), Some(b)) => Some(cosine_similarity(a, b)),
                _ => None,
            };
            if should_cluster_memories(&memories[i], &memories[j], sim, threshold) {
                cluster.push(j);
                sims.push(sim.unwrap_or(1.0));
            }
        }

        if cluster.len() < 2 {
            continue;
        }

        for &idx in &cluster {
            assigned.insert(memories[idx].id);
        }

        let avg_similarity = if sims.is_empty() {
            1.0
        } else {
            sims.iter().sum::<f64>() / sims.len() as f64
        };
        clusters.push(ConsolidationCluster {
            members: cluster,
            avg_similarity,
        });
    }

    clusters
}

pub fn duplicate_rate_for_active_memories(memories: &[Memory], threshold: f64) -> f64 {
    let active_count = count_active_memories(memories);
    if active_count == 0 {
        return 0.0;
    }

    let merged = build_consolidation_clusters(memories, threshold, usize::MAX)
        .into_iter()
        .map(|cluster| cluster.members.len().saturating_sub(1))
        .sum::<usize>();

    merged as f64 / active_count as f64
}

#[cfg(test)]
mod tests {
    use crate::memory::{Memory, ScoreChannels, ScoreExplainEntry, ScoredMemory};

    use super::{
        are_structural_duplicates, collapse_explained_entries, collapse_scored_memories,
        collapse_scored_memories_for_query, fuse_contents,
    };

    #[test]
    fn test_structural_duplicate_by_containment() {
        let a = "Rust uses ownership and borrowing for memory safety.";
        let b = "Rust uses ownership and borrowing for memory safety without a garbage collector.";
        assert!(are_structural_duplicates(a, b));
    }

    #[test]
    fn test_fuse_contents_unions_unique_sentences() {
        let a = "Rust uses ownership for safety. Axum is used for the API.";
        let b = "Rust uses ownership for safety. Tests should run before deployment.";
        let fused = fuse_contents(a, b);
        assert!(fused.contains("Axum is used for the API"));
        assert!(fused.contains("Tests should run before deployment"));
    }

    #[test]
    fn test_collapse_scored_memories_merges_duplicates() {
        let mut first = Memory::new("Rust uses ownership for safety.".to_string());
        first.tags = vec!["rust".to_string()];
        let mut second =
            Memory::new("Rust uses ownership for safety without a garbage collector.".to_string());
        second.tags = vec!["memory".to_string()];

        let collapsed = collapse_scored_memories(vec![
            ScoredMemory {
                memory: first,
                score: 0.7,
                provenance: vec!["vector".to_string()],
                trust_score: 0.8,
                low_trust: false,
            },
            ScoredMemory {
                memory: second,
                score: 0.8,
                provenance: vec!["exact".to_string()],
                trust_score: 0.9,
                low_trust: false,
            },
        ]);

        assert_eq!(collapsed.len(), 1);
        assert!(collapsed[0].memory.tags.contains(&"rust".to_string()));
        assert!(collapsed[0].memory.tags.contains(&"memory".to_string()));
        assert!(
            collapsed[0]
                .provenance
                .contains(&"fused_duplicate".to_string())
        );
    }

    #[test]
    fn test_collapse_explained_entries_merges_duplicates() {
        let first = ScoreExplainEntry {
            memory: Memory::new("X-Memory-Mode header controls recall.".to_string()),
            provenance: vec!["fts".to_string()],
            channels: ScoreChannels {
                vector: 0.2,
                fts: 0.7,
                exact: 0.0,
            },
            max_channel_score: 0.7,
            base_score: 0.4,
            recency_score: 0.1,
            confidence_factor: 1.0,
            status_factor: 1.0,
            trust_score: 0.8,
            trust_confidence_low: 0.7,
            trust_confidence_high: 0.9,
            trust_signals: crate::security::trust::TrustSignals {
                recency: 0.8,
                source_reputation: 0.8,
                embedding_coherence: 0.8,
                access_frequency: 0.8,
                outcome_learning: 0.8,
            },
            trust_multiplier: 0.92,
            final_score: 0.46,
            low_trust: false,
        };
        let second = ScoreExplainEntry {
            memory: Memory::new(
                "X-Memory-Mode header controls recall and can disable memory injection."
                    .to_string(),
            ),
            provenance: vec!["exact".to_string()],
            channels: ScoreChannels {
                vector: 0.1,
                fts: 0.4,
                exact: 1.0,
            },
            max_channel_score: 1.0,
            base_score: 0.6,
            recency_score: 0.1,
            confidence_factor: 1.0,
            status_factor: 1.0,
            trust_score: 0.9,
            trust_confidence_low: 0.8,
            trust_confidence_high: 0.95,
            trust_signals: crate::security::trust::TrustSignals {
                recency: 0.9,
                source_reputation: 0.9,
                embedding_coherence: 0.9,
                access_frequency: 0.9,
                outcome_learning: 0.9,
            },
            trust_multiplier: 0.96,
            final_score: 0.7,
            low_trust: false,
        };

        let collapsed = collapse_explained_entries(vec![first, second]);
        assert_eq!(collapsed.len(), 1);
        assert!(collapsed[0].channels.exact > 0.0);
        assert!(
            collapsed[0]
                .provenance
                .contains(&"fused_duplicate".to_string())
        );
    }

    #[test]
    fn test_identifier_route_collapse_merges_shared_endpoint_anchor_without_literal_query() {
        let route = crate::scoring::detect_identifier_route(
            "which endpoint handles Anthropic messages through the proxy?",
        )
        .expect("expected identifier route");

        let collapsed = collapse_scored_memories_for_query(
            vec![
                ScoredMemory {
                    memory: Memory::new(
                        "Claude proxy endpoint is /proxy/anthropic/v1/messages and Claude proxy mode should export ANTHROPIC_BASE_URL."
                            .to_string(),
                    ),
                    score: 0.8,
                    provenance: vec!["fts".to_string()],
                    trust_score: 0.9,
                    low_trust: false,
                },
                ScoredMemory {
                    memory: Memory::new(
                        "Use /proxy/anthropic/v1/messages for Anthropic Messages API requests through the proxy."
                            .to_string(),
                    ),
                    score: 0.82,
                    provenance: vec!["exact".to_string()],
                    trust_score: 0.91,
                    low_trust: false,
                },
                ScoredMemory {
                    memory: Memory::new(
                        "OpenAI chat proxy requests go to /proxy/v1/chat/completions and use OPENAI_BASE_URL."
                            .to_string(),
                    ),
                    score: 0.5,
                    provenance: vec!["fts".to_string()],
                    trust_score: 0.8,
                    low_trust: false,
                },
            ],
            Some(&route),
        );

        assert_eq!(collapsed.len(), 2);
        let anthropic = collapsed
            .iter()
            .find(|entry| {
                entry
                    .memory
                    .content
                    .contains("/proxy/anthropic/v1/messages")
            })
            .expect("missing anthropic endpoint entry");
        assert!(
            anthropic.memory.content.contains("ANTHROPIC_BASE_URL")
                || anthropic.memory.content.contains("Anthropic Messages API"),
            "collapsed content should preserve details from both fragments"
        );
    }
}
