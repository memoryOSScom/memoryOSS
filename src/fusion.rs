// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 memoryOSS Contributors

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::memory::{
    Memory, MemoryStatus, MemorySummaryEntry, ScoreExplainEntry, ScoredMemory, SummaryEvidenceItem,
};
use crate::scoring::TaskContext;

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

fn escape_xml(content: &str) -> String {
    content
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompiledTaskStateItem {
    pub memory_id: Uuid,
    pub summary: String,
    pub score: f64,
    pub trust_score: f64,
    pub status: MemoryStatus,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub provenance: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompiledTaskStateEvidence {
    pub memory_id: Uuid,
    pub preview: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub provenance: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompiledTaskStateInputs {
    pub candidate_memory_ids: Vec<Uuid>,
    pub selected_memory_ids: Vec<Uuid>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub omitted_memory_ids: Vec<Uuid>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompiledTaskState {
    pub kind: String,
    pub goal: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub facts: Vec<CompiledTaskStateItem>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub constraints: Vec<CompiledTaskStateItem>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub recent_actions: Vec<CompiledTaskStateItem>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub open_questions: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<CompiledTaskStateEvidence>,
    pub inputs: CompiledTaskStateInputs,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub decisions: Vec<String>,
}

#[derive(Debug, Clone)]
struct TaskStateCandidate {
    memory_id: Uuid,
    content: String,
    summary: String,
    score: f64,
    trust_score: f64,
    status: MemoryStatus,
    tags: Vec<String>,
    provenance: Vec<String>,
    evidence: Vec<SummaryEvidenceItem>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TaskStateBucket {
    Fact,
    Constraint,
    RecentAction,
    OpenQuestion,
}

fn contains_hint(text: &str, hints: &[&str]) -> bool {
    let lowered = text.to_lowercase();
    hints.iter().any(|hint| {
        let hint = hint.to_lowercase();
        lowered.contains(&hint)
    })
}

fn is_normative_text(text: &str) -> bool {
    let lowered = text.to_lowercase();
    [
        " must ",
        " should ",
        " required ",
        " requires ",
        " never ",
        " do not ",
        " before ",
        " only after ",
        " mandatory ",
        " policy ",
        " preference ",
        " checklist ",
        " source of truth ",
        " canonical ",
    ]
    .iter()
    .any(|needle| lowered.contains(needle))
}

fn is_action_text(text: &str) -> bool {
    let lowered = text.to_lowercase();
    [
        " rerun ",
        " retry ",
        " roll back ",
        " rollback ",
        " notify ",
        " merge ",
        " deploy ",
        " release ",
        " validate ",
        " review ",
        " audit ",
        " flush ",
        " clear ",
        " invalidate ",
        " patch ",
        " fix ",
    ]
    .iter()
    .any(|needle| lowered.contains(needle))
}

fn bucket_candidate(candidate: &TaskStateCandidate, task_context: &TaskContext) -> TaskStateBucket {
    if matches!(
        candidate.status,
        MemoryStatus::Candidate | MemoryStatus::Contested
    ) {
        return TaskStateBucket::OpenQuestion;
    }

    let tag_text = candidate.tags.join(" ");
    if is_normative_text(&candidate.content)
        || contains_hint(
            &candidate.content,
            task_context.kind.task_state_constraint_hints(),
        )
        || contains_hint(&tag_text, task_context.kind.task_state_constraint_hints())
    {
        return TaskStateBucket::Constraint;
    }

    if is_action_text(&candidate.content)
        || contains_hint(
            &candidate.content,
            task_context.kind.task_state_action_hints(),
        )
        || contains_hint(&tag_text, task_context.kind.task_state_action_hints())
    {
        return TaskStateBucket::RecentAction;
    }

    TaskStateBucket::Fact
}

fn candidate_to_item(candidate: &TaskStateCandidate) -> CompiledTaskStateItem {
    CompiledTaskStateItem {
        memory_id: candidate.memory_id,
        summary: candidate.summary.clone(),
        score: candidate.score,
        trust_score: candidate.trust_score,
        status: candidate.status,
        tags: candidate.tags.clone(),
        provenance: candidate.provenance.clone(),
    }
}

fn compile_task_state(
    candidates: Vec<TaskStateCandidate>,
    task_context: &TaskContext,
) -> Option<CompiledTaskState> {
    if candidates.is_empty() {
        return None;
    }

    let mut facts = Vec::new();
    let mut constraints = Vec::new();
    let mut recent_actions = Vec::new();
    let mut open_questions = Vec::new();
    let mut evidence = Vec::new();
    let mut selected_ids = Vec::new();
    let mut decisions = Vec::new();

    for candidate in &candidates {
        let bucket = bucket_candidate(candidate, task_context);
        let item = candidate_to_item(candidate);
        let mut selected = false;

        match bucket {
            TaskStateBucket::Constraint if constraints.len() < 2 => {
                constraints.push(item);
                selected = true;
            }
            TaskStateBucket::RecentAction if recent_actions.len() < 2 => {
                recent_actions.push(item);
                selected = true;
            }
            TaskStateBucket::Fact if facts.len() < 2 => {
                facts.push(item);
                selected = true;
            }
            TaskStateBucket::OpenQuestion if open_questions.len() < 2 => {
                let question = match candidate.status {
                    MemoryStatus::Candidate => {
                        format!(
                            "Confirm candidate memory before relying on it: {}",
                            candidate.summary
                        )
                    }
                    MemoryStatus::Contested => {
                        format!(
                            "Resolve contested memory before relying on it: {}",
                            candidate.summary
                        )
                    }
                    _ => format!(
                        "Clarify whether this memory should stay active: {}",
                        candidate.summary
                    ),
                };
                open_questions.push(question);
                selected = true;
            }
            _ => {}
        }

        if selected {
            selected_ids.push(candidate.memory_id);
            if let Some(first_evidence) = candidate.evidence.first() {
                if evidence.len() < 4 {
                    evidence.push(CompiledTaskStateEvidence {
                        memory_id: candidate.memory_id,
                        preview: first_evidence.preview.clone(),
                        provenance: first_evidence.provenance.clone(),
                    });
                }
            }
        }
    }

    if facts.is_empty()
        && let Some(candidate) = candidates
            .iter()
            .find(|candidate| !selected_ids.contains(&candidate.memory_id))
    {
        facts.push(candidate_to_item(candidate));
        selected_ids.push(candidate.memory_id);
    }

    if facts.is_empty() && recent_actions.is_empty() && constraints.len() > 1 {
        if let Some(downgraded_constraint) = constraints.pop() {
            facts.push(downgraded_constraint);
            decisions.push(
                "Demoted the lowest-priority constraint into facts to keep the working state balanced."
                    .to_string(),
            );
        }
    }

    if constraints.is_empty() || (facts.is_empty() && recent_actions.is_empty()) {
        open_questions.push(task_context.kind.task_state_missing_question().to_string());
    }
    open_questions.sort();
    open_questions.dedup();

    if !constraints.is_empty() {
        decisions
            .push("Promoted policy-like or preference-like memories into constraints.".to_string());
    }
    if !recent_actions.is_empty() {
        decisions.push("Promoted action-oriented memories into recent actions.".to_string());
    }
    if !open_questions.is_empty() {
        decisions.push(
            "Turned unresolved or low-confidence signals into explicit open questions.".to_string(),
        );
    }
    if !evidence.is_empty() {
        decisions.push(
            "Kept one bounded evidence preview per selected memory for drill-down.".to_string(),
        );
    }

    let candidate_memory_ids = candidates
        .iter()
        .map(|candidate| candidate.memory_id)
        .collect();
    let omitted_memory_ids = candidates
        .iter()
        .map(|candidate| candidate.memory_id)
        .filter(|id| !selected_ids.contains(id))
        .collect();

    Some(CompiledTaskState {
        kind: task_context.label().to_string(),
        goal: task_context.kind.task_state_goal().to_string(),
        facts,
        constraints,
        recent_actions,
        open_questions,
        evidence,
        inputs: CompiledTaskStateInputs {
            candidate_memory_ids,
            selected_memory_ids: selected_ids,
            omitted_memory_ids,
        },
        decisions,
    })
}

pub fn compile_scored_task_state(
    scored: &[ScoredMemory],
    task_context: &TaskContext,
) -> Option<CompiledTaskState> {
    compile_task_state(
        scored
            .iter()
            .map(|entry| TaskStateCandidate {
                memory_id: entry.memory.id,
                content: entry.memory.content.clone(),
                summary: summary_sentence(&entry.memory.content),
                score: entry.score,
                trust_score: entry.trust_score,
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
            .collect(),
        task_context,
    )
}

pub fn compile_explained_task_state(
    explained: &[ScoreExplainEntry],
    task_context: &TaskContext,
) -> Option<CompiledTaskState> {
    compile_task_state(
        explained
            .iter()
            .map(|entry| TaskStateCandidate {
                memory_id: entry.memory.id,
                content: entry.memory.content.clone(),
                summary: summary_sentence(&entry.memory.content),
                score: entry.final_score,
                trust_score: entry.trust_score,
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
            .collect(),
        task_context,
    )
}

pub fn render_task_state_xml(task_state: &CompiledTaskState) -> String {
    fn render_items(tag: &str, item_tag: &str, items: &[CompiledTaskStateItem], body: &mut String) {
        if items.is_empty() {
            return;
        }
        body.push_str(&format!("<{tag}>\n"));
        for item in items {
            body.push_str(&format!(
                "<{item_tag}>{}</{item_tag}>\n",
                escape_xml(&item.summary)
            ));
        }
        body.push_str(&format!("</{tag}>\n"));
    }

    let mut body = format!(
        "<task_state kind=\"{}\">\n<goal>{}</goal>\n",
        task_state.kind,
        escape_xml(&task_state.goal)
    );
    render_items("facts", "fact", &task_state.facts, &mut body);
    render_items(
        "constraints",
        "constraint",
        &task_state.constraints,
        &mut body,
    );
    render_items(
        "recent_actions",
        "recent_action",
        &task_state.recent_actions,
        &mut body,
    );
    if !task_state.open_questions.is_empty() {
        body.push_str("<open_questions>\n");
        for question in &task_state.open_questions {
            body.push_str(&format!("<question>{}</question>\n", escape_xml(question)));
        }
        body.push_str("</open_questions>\n");
    }
    if !task_state.evidence.is_empty() {
        body.push_str("<evidence>\n");
        for item in &task_state.evidence {
            body.push_str(&format!("<item>{}</item>\n", escape_xml(&item.preview)));
        }
        body.push_str("</evidence>\n");
    }
    body.push_str("</task_state>\n");
    body
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
    collapse_scored_memories_with_options(scored, route, false, None)
}

pub fn collapse_scored_memories_with_options(
    scored: Vec<ScoredMemory>,
    route: Option<&crate::scoring::IdentifierRouteProfile>,
    primitive_algebra: bool,
    task_context: Option<&TaskContext>,
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
                || (primitive_algebra
                    && crate::scoring::primitive_merge_key_for_content(
                        &entry.memory.content,
                        &entry.memory.tags,
                        task_context,
                    )
                    .zip(crate::scoring::primitive_merge_key_for_content(
                        &candidate.memory.content,
                        &candidate.memory.tags,
                        task_context,
                    ))
                    .is_some_and(|(left, right)| left == right))
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
    collapse_explained_entries_with_options(explained, route, false, None)
}

pub fn collapse_explained_entries_with_options(
    explained: Vec<ScoreExplainEntry>,
    route: Option<&crate::scoring::IdentifierRouteProfile>,
    primitive_algebra: bool,
    task_context: Option<&TaskContext>,
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
                || (primitive_algebra
                    && crate::scoring::primitive_merge_key_for_content(
                        &entry.memory.content,
                        &entry.memory.tags,
                        task_context,
                    )
                    .zip(crate::scoring::primitive_merge_key_for_content(
                        &candidate.memory.content,
                        &candidate.memory.tags,
                        task_context,
                    ))
                    .is_some_and(|(left, right)| left == right))
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
            merged.primitive_score = merged.primitive_score.max(other.primitive_score);
            if merged.primitive_decomposition.is_none() {
                merged.primitive_decomposition = other.primitive_decomposition.clone();
            }
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
    use crate::memory::{Memory, MemoryStatus, ScoreChannels, ScoreExplainEntry, ScoredMemory};
    use crate::scoring::{TaskContext, TaskContextKind};
    use uuid::Uuid;

    use super::{
        are_structural_duplicates, collapse_explained_entries,
        collapse_explained_entries_with_options, collapse_scored_memories,
        collapse_scored_memories_for_query, compile_scored_task_state, fuse_contents,
        render_task_state_xml,
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
            primitive_score: 0.0,
            primitive_decomposition: None,
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
            primitive_score: 0.0,
            primitive_decomposition: None,
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
    fn test_primitive_collapse_merges_shared_dependency_key() {
        let mut first_memory = Memory::new(
            "Auth hotfix dependency: flush the token cache before production deploy.".to_string(),
        );
        first_memory.tags = vec!["auth".into(), "hotfix".into(), "dependency".into()];
        let mut second_memory = Memory::new(
            "Auth rollback prerequisite: token cache flush must happen before the patch ships."
                .to_string(),
        );
        second_memory.tags = vec!["auth".into(), "hotfix".into(), "dependency".into()];
        let context = TaskContext {
            kind: crate::scoring::TaskContextKind::Deploy,
            matched_terms: vec!["deploy".into()],
        };
        let collapsed = collapse_explained_entries_with_options(
            vec![
                ScoreExplainEntry {
                    memory: first_memory,
                    provenance: vec!["vector".to_string()],
                    channels: ScoreChannels {
                        vector: 0.6,
                        fts: 0.0,
                        exact: 0.0,
                    },
                    max_channel_score: 0.6,
                    base_score: 0.4,
                    recency_score: 0.0,
                    confidence_factor: 1.0,
                    status_factor: 1.0,
                    trust_score: 0.9,
                    trust_confidence_low: 0.8,
                    trust_confidence_high: 1.0,
                    trust_signals: crate::security::trust::TrustSignals {
                        recency: 1.0,
                        source_reputation: 1.0,
                        embedding_coherence: 1.0,
                        access_frequency: 0.5,
                        outcome_learning: 0.5,
                    },
                    trust_multiplier: 1.0,
                    primitive_score: 0.1,
                    primitive_decomposition: None,
                    final_score: 0.5,
                    low_trust: false,
                },
                ScoreExplainEntry {
                    memory: second_memory,
                    provenance: vec!["fts".to_string()],
                    channels: ScoreChannels {
                        vector: 0.5,
                        fts: 0.0,
                        exact: 0.0,
                    },
                    max_channel_score: 0.5,
                    base_score: 0.35,
                    recency_score: 0.0,
                    confidence_factor: 1.0,
                    status_factor: 1.0,
                    trust_score: 0.85,
                    trust_confidence_low: 0.8,
                    trust_confidence_high: 0.9,
                    trust_signals: crate::security::trust::TrustSignals {
                        recency: 1.0,
                        source_reputation: 1.0,
                        embedding_coherence: 1.0,
                        access_frequency: 0.5,
                        outcome_learning: 0.5,
                    },
                    trust_multiplier: 1.0,
                    primitive_score: 0.12,
                    primitive_decomposition: None,
                    final_score: 0.48,
                    low_trust: false,
                },
            ],
            None,
            true,
            Some(&context),
        );
        assert_eq!(collapsed.len(), 1);
        assert!(
            collapsed[0]
                .memory
                .content
                .contains("before production deploy")
        );
        assert!(
            collapsed[0]
                .memory
                .content
                .contains("before the patch ships")
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

    #[test]
    fn test_compile_scored_task_state_separates_constraints_actions_and_questions() {
        let mut constraint =
            Memory::new("Production deploys require staging approval before rollout.".to_string());
        constraint.tags = vec!["deploy".to_string(), "approval".to_string()];

        let mut action = Memory::new(
            "Latest deploy action: rerun the smoke test and notify ops-red once rollout finishes."
                .to_string(),
        );
        action.tags = vec!["deploy".to_string(), "runbook".to_string()];

        let mut candidate = Memory::new(
            "Candidate deploy note: maybe skip smoke tests when the canary looks healthy."
                .to_string(),
        );
        candidate.status = MemoryStatus::Candidate;
        candidate.tags = vec!["deploy".to_string(), "candidate".to_string()];

        let task_state = compile_scored_task_state(
            &[
                ScoredMemory {
                    memory: constraint,
                    score: 0.93,
                    provenance: vec!["fts".to_string()],
                    trust_score: 0.91,
                    low_trust: false,
                },
                ScoredMemory {
                    memory: action,
                    score: 0.88,
                    provenance: vec!["vector".to_string()],
                    trust_score: 0.87,
                    low_trust: false,
                },
                ScoredMemory {
                    memory: candidate,
                    score: 0.7,
                    provenance: vec!["vector".to_string()],
                    trust_score: 0.6,
                    low_trust: false,
                },
            ],
            &TaskContext {
                kind: TaskContextKind::Deploy,
                matched_terms: vec!["deploy".to_string()],
            },
        )
        .expect("expected compiled task state");

        assert_eq!(task_state.kind, "deploy");
        assert!(!task_state.constraints.is_empty());
        assert!(
            !task_state.recent_actions.is_empty() || !task_state.facts.is_empty(),
            "compiler should keep at least one non-constraint working-state item"
        );
        assert!(!task_state.open_questions.is_empty());
        assert!(
            task_state
                .decisions
                .iter()
                .any(|entry| entry.contains("constraints")),
            "compiler should explain why constraints were promoted"
        );
    }

    #[test]
    fn test_render_task_state_xml_emits_explicit_sections() {
        let task_state = super::CompiledTaskState {
            kind: "review".to_string(),
            goal: "Compile the minimum review state for this task.".to_string(),
            facts: Vec::new(),
            constraints: vec![super::CompiledTaskStateItem {
                memory_id: Uuid::now_v7(),
                summary: "Require tests and security review before merge.".to_string(),
                score: 0.9,
                trust_score: 0.9,
                status: MemoryStatus::Active,
                tags: vec!["review".to_string()],
                provenance: vec!["fts".to_string()],
            }],
            recent_actions: Vec::new(),
            open_questions: vec![
                "Confirm whether the rollback diff needs another reviewer.".to_string(),
            ],
            evidence: vec![super::CompiledTaskStateEvidence {
                memory_id: Uuid::now_v7(),
                preview: "Security review before merge.".to_string(),
                provenance: vec!["fts".to_string()],
            }],
            inputs: super::CompiledTaskStateInputs {
                candidate_memory_ids: Vec::new(),
                selected_memory_ids: Vec::new(),
                omitted_memory_ids: Vec::new(),
            },
            decisions: Vec::new(),
        };

        let xml = render_task_state_xml(&task_state);
        assert!(xml.contains("<task_state kind=\"review\">"));
        assert!(xml.contains("<constraints>"));
        assert!(xml.contains("<open_questions>"));
        assert!(xml.contains("<evidence>"));
    }
}
