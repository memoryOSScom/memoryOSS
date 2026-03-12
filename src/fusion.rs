// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 memoryOSS Contributors

use std::collections::HashSet;

use crate::memory::{ScoreExplainEntry, ScoredMemory};

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

pub fn collapse_scored_memories(scored: Vec<ScoredMemory>) -> Vec<ScoredMemory> {
    let mut collapsed: Vec<ScoredMemory> = Vec::new();

    for candidate in scored {
        if let Some(existing) = collapsed.iter_mut().find(|entry| {
            are_structural_duplicates(&entry.memory.content, &candidate.memory.content)
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

pub fn collapse_explained_entries(explained: Vec<ScoreExplainEntry>) -> Vec<ScoreExplainEntry> {
    let mut collapsed: Vec<ScoreExplainEntry> = Vec::new();

    for candidate in explained {
        if let Some(existing) = collapsed.iter_mut().find(|entry| {
            are_structural_duplicates(&entry.memory.content, &candidate.memory.content)
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

#[cfg(test)]
mod tests {
    use crate::memory::{Memory, ScoreChannels, ScoreExplainEntry, ScoredMemory};

    use super::{
        are_structural_duplicates, collapse_explained_entries, collapse_scored_memories,
        fuse_contents,
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
}
