// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 memoryOSS Contributors

//! Shared scoring and merging logic for recall pipelines.
//! Used by: proxy recall, API recall, and decompose sub-queries.
//! Implements GrepRAG multi-channel scoring + RLM IDF boost + precision gate + diversity.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

use uuid::Uuid;

use crate::engines::document::DocumentEngine;
use crate::engines::fts::FtsEngine;
use crate::memory::{ScoreChannels, ScoreExplainEntry, ScoredMemory, ScoringWeights};
use crate::merger::IdfIndex;
use crate::security::trust::TrustScorer;

/// Per-channel raw scores before merge (for precision gate).
#[derive(Debug, Clone, Default)]
struct ChannelScores {
    vector: f64,
    fts: f64,
    exact: f64,
    provenance: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskContextKind {
    Deploy,
    Bugfix,
    Review,
    Style,
}

impl TaskContextKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Deploy => "deploy",
            Self::Bugfix => "bugfix",
            Self::Review => "review",
            Self::Style => "style",
        }
    }

    fn query_hints(self) -> &'static [&'static str] {
        match self {
            Self::Deploy => &[
                "deploy",
                "deployment",
                "release",
                "rollout",
                "staging",
                "production",
                "prod",
                "ship",
                "shipping",
                "launch",
                "rollback",
                "canary",
                "migrate",
                "migration",
            ],
            Self::Bugfix => &[
                "bug",
                "bugfix",
                "fix",
                "debug",
                "debugging",
                "incident",
                "issue",
                "failure",
                "failing",
                "error",
                "crash",
                "exception",
                "regression",
                "workaround",
                "root cause",
                "patch",
            ],
            Self::Review => &[
                "review",
                "audit",
                "inspect",
                "analysis",
                "analyze",
                "assess",
                "evaluate",
                "diff",
                "pull request",
                "pr",
                "security",
                "lint",
                "checklist",
                "qa",
            ],
            Self::Style => &[
                "style",
                "tone",
                "wording",
                "phrasing",
                "format",
                "formatting",
                "rewrite",
                "rephrase",
                "concise",
                "verbosity",
                "bullet",
                "bullets",
                "display",
                "summary",
                "summaries",
            ],
        }
    }

    fn memory_hints(self) -> &'static [&'static str] {
        match self {
            Self::Deploy => &[
                "deploy",
                "deployment",
                "release",
                "rollout",
                "staging",
                "production",
                "approval",
                "smoke",
                "migration",
                "rollback",
                "runbook",
            ],
            Self::Bugfix => &[
                "bug",
                "bugfix",
                "fix",
                "incident",
                "issue",
                "failure",
                "error",
                "exception",
                "regression",
                "workaround",
                "root cause",
                "postmortem",
                "rollback",
            ],
            Self::Review => &[
                "review",
                "audit",
                "security",
                "checklist",
                "tests",
                "lint",
                "approval",
                "diff",
                "merge",
                "pr",
            ],
            Self::Style => &[
                "style",
                "tone",
                "wording",
                "format",
                "bullet",
                "verbosity",
                "display",
                "summary",
                "response",
                "rewrite",
            ],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskContext {
    pub kind: TaskContextKind,
    pub matched_terms: Vec<String>,
}

impl TaskContext {
    pub fn label(&self) -> &'static str {
        self.kind.as_str()
    }
}

fn normalize_task_text(text: &str) -> String {
    text.to_lowercase()
        .chars()
        .map(|ch| {
            if ch.is_alphanumeric() || ch.is_whitespace() {
                ch
            } else {
                ' '
            }
        })
        .collect()
}

fn collect_context_hits(text: &str, hints: &[&str]) -> Vec<String> {
    let normalized = normalize_task_text(text);
    let padded = format!(" {normalized} ");
    let mut hits = Vec::new();

    for hint in hints {
        let normalized_hint = normalize_task_text(hint).trim().to_string();
        if normalized_hint.is_empty() {
            continue;
        }
        let pattern = format!(" {normalized_hint} ");
        if padded.contains(&pattern)
            || (normalized_hint.len() > 4 && normalized.contains(&normalized_hint))
        {
            hits.push(normalized_hint);
        }
    }

    hits.sort_unstable();
    hits.dedup();
    hits
}

pub fn detect_task_context(query: &str) -> Option<TaskContext> {
    let mut candidates = [
        TaskContextKind::Deploy,
        TaskContextKind::Bugfix,
        TaskContextKind::Review,
        TaskContextKind::Style,
    ]
    .into_iter()
    .map(|kind| (kind, collect_context_hits(query, kind.query_hints())))
    .collect::<Vec<_>>();

    candidates.sort_by(|a, b| b.1.len().cmp(&a.1.len()));
    let (kind, matched_terms) = candidates.first()?.clone();
    if matched_terms.is_empty() {
        return None;
    }
    if candidates
        .get(1)
        .is_some_and(|(_, hits)| hits.len() == matched_terms.len())
    {
        return None;
    }

    Some(TaskContext {
        kind,
        matched_terms,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdentifierKind {
    Path,
    Endpoint,
    EnvVar,
    BranchPattern,
    Commit,
    Policy,
}

impl IdentifierKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Path => "path",
            Self::Endpoint => "endpoint",
            Self::EnvVar => "env_var",
            Self::BranchPattern => "branch_pattern",
            Self::Commit => "commit",
            Self::Policy => "policy",
        }
    }
}

pub(crate) fn identifier_matches_kind(identifier: &str, kind: IdentifierKind) -> bool {
    let lowered = normalize_identifier_text(identifier);
    match kind {
        IdentifierKind::Path => {
            lowered.starts_with('/')
                || lowered.contains(".rs")
                || lowered.contains(".toml")
                || lowered.contains(".json")
                || lowered.contains(".yaml")
                || lowered.contains(".yml")
        }
        IdentifierKind::Endpoint => {
            lowered.contains("/v1/")
                || lowered.contains("/proxy/")
                || lowered.starts_with("http://")
                || lowered.starts_with("https://")
        }
        IdentifierKind::EnvVar => contains_env_var(identifier),
        IdentifierKind::BranchPattern => {
            lowered.starts_with("feat/") || lowered.starts_with("fix/")
        }
        IdentifierKind::Commit => contains_hex_commit(&lowered),
        IdentifierKind::Policy => false,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdentifierRouteProfile {
    pub active: bool,
    pub identifiers: Vec<String>,
    pub kinds: Vec<IdentifierKind>,
    pub matched_terms: Vec<String>,
    pub focus_terms: Vec<String>,
}

impl IdentifierRouteProfile {
    pub fn labels(&self) -> Vec<&'static str> {
        self.kinds.iter().map(|kind| kind.as_str()).collect()
    }
}

fn normalize_identifier_text(text: &str) -> String {
    text.trim().to_lowercase()
}

fn tokenized_identifier_parts(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    for ch in text.chars() {
        if ch.is_alphanumeric() {
            current.push(ch.to_ascii_lowercase());
        } else if !current.is_empty() {
            if current.len() >= 2 {
                tokens.push(std::mem::take(&mut current));
            } else {
                current.clear();
            }
        }
    }
    if current.len() >= 2 {
        tokens.push(current);
    }
    tokens.sort_unstable();
    tokens.dedup();
    tokens
}

fn contains_hex_commit(text: &str) -> bool {
    text.split(|ch: char| !ch.is_ascii_hexdigit())
        .any(|token| token.len() >= 7 && token.len() <= 40)
}

fn contains_env_var(text: &str) -> bool {
    text.split(|ch: char| !ch.is_ascii_alphanumeric() && ch != '_')
        .any(|token| {
            token.len() >= 6
                && token.contains('_')
                && token
                    .chars()
                    .all(|ch| ch.is_ascii_uppercase() || ch.is_ascii_digit() || ch == '_')
        })
}

fn query_keyword_present(query: &str, keywords: &[&str]) -> Vec<String> {
    collect_context_hits(query, keywords)
}

pub fn detect_identifier_route(query: &str) -> Option<IdentifierRouteProfile> {
    let identifiers = extract_identifiers(query);
    let mut kinds = Vec::new();
    let mut matched_terms = Vec::new();
    let normalized = normalize_task_text(query);
    let padded = format!(" {normalized} ");

    let path_terms = query_keyword_present(
        query,
        &[
            "path",
            "paths",
            "file",
            "config",
            "binary",
            "directory",
            "location",
        ],
    );
    if !path_terms.is_empty()
        || identifiers.iter().any(|ident| {
            let lowered = ident.to_lowercase();
            lowered.starts_with('/') || lowered.contains(".rs") || lowered.contains(".toml")
        })
    {
        kinds.push(IdentifierKind::Path);
        matched_terms.extend(path_terms);
    }

    let endpoint_terms = query_keyword_present(
        query,
        &[
            "endpoint",
            "endpoints",
            "route",
            "routes",
            "url",
            "urls",
            "api",
        ],
    );
    if !endpoint_terms.is_empty()
        || identifiers.iter().any(|ident| {
            let lowered = ident.to_lowercase();
            lowered.contains("/v1/") || lowered.contains("http://") || lowered.contains("https://")
        })
    {
        kinds.push(IdentifierKind::Endpoint);
        matched_terms.extend(endpoint_terms);
    }

    let env_terms = query_keyword_present(
        query,
        &[
            "env",
            "environment variable",
            "environment variables",
            "variable",
            "variables",
            "export",
        ],
    );
    if !env_terms.is_empty() || identifiers.iter().any(|ident| ident.contains('_')) {
        kinds.push(IdentifierKind::EnvVar);
        matched_terms.extend(env_terms);
    }

    let branch_terms = query_keyword_present(query, &["branch", "branches", "branching"]);
    if !branch_terms.is_empty()
        || identifiers.iter().any(|ident| {
            let lowered = ident.to_lowercase();
            lowered.starts_with("feat/") || lowered.starts_with("fix/")
        })
    {
        kinds.push(IdentifierKind::BranchPattern);
        matched_terms.extend(branch_terms);
    }

    let commit_terms = query_keyword_present(query, &["commit", "sha", "revision", "hash"]);
    if !commit_terms.is_empty()
        || identifiers
            .iter()
            .any(|ident| contains_hex_commit(&normalize_identifier_text(ident)))
    {
        kinds.push(IdentifierKind::Commit);
        matched_terms.extend(commit_terms);
    }

    let policy_terms = query_keyword_present(
        query,
        &[
            "policy",
            "rule",
            "rules",
            "preference",
            "preferences",
            "convention",
            "conventions",
            "canonical",
            "source of truth",
        ],
    );
    if !policy_terms.is_empty()
        || (padded.contains(" required ")
            || padded.contains(" never ")
            || padded.contains(" must ")
            || padded.contains(" canonical ")
            || padded.contains(" source of truth "))
    {
        kinds.push(IdentifierKind::Policy);
        matched_terms.extend(policy_terms);
    }

    kinds.sort_by_key(|kind| kind.as_str());
    kinds.dedup();
    matched_terms.sort_unstable();
    matched_terms.dedup();

    if identifiers.is_empty() && kinds.is_empty() {
        return None;
    }

    let matched_terms_set: std::collections::HashSet<&str> =
        matched_terms.iter().map(String::as_str).collect();
    let stop_terms = [
        "which", "what", "should", "there", "here", "through", "after", "before", "about", "proxy",
        "mode", "messages", "message", "store", "stores",
    ];
    let mut focus_terms: Vec<String> = normalize_task_text(query)
        .split_whitespace()
        .filter(|term| term.len() >= 5)
        .filter(|term| !matched_terms_set.contains(*term))
        .filter(|term| !stop_terms.contains(term))
        .map(ToString::to_string)
        .collect();
    focus_terms.sort_unstable();
    focus_terms.dedup();

    Some(IdentifierRouteProfile {
        active: true,
        identifiers,
        kinds,
        matched_terms,
        focus_terms,
    })
}

pub(crate) fn content_matches_identifier_kind(content: &str, kind: IdentifierKind) -> bool {
    let lowered = normalize_identifier_text(content);
    match kind {
        IdentifierKind::Path => {
            lowered.contains('/')
                || lowered.contains(".rs")
                || lowered.contains(".toml")
                || lowered.contains(".json")
                || lowered.contains(".yaml")
                || lowered.contains(".yml")
        }
        IdentifierKind::Endpoint => {
            lowered.contains("/v1/")
                || lowered.contains("/proxy/")
                || lowered.contains("http://")
                || lowered.contains("https://")
        }
        IdentifierKind::EnvVar => contains_env_var(content),
        IdentifierKind::BranchPattern => {
            lowered.contains("feat/") || lowered.contains("fix/") || lowered.contains("<ticket>")
        }
        IdentifierKind::Commit => contains_hex_commit(&lowered),
        IdentifierKind::Policy => {
            lowered.contains(" must ")
                || lowered.starts_with("must ")
                || lowered.contains(" never ")
                || lowered.starts_with("never ")
                || lowered.contains(" always ")
                || lowered.contains(" should ")
                || lowered.contains(" do not ")
                || lowered.contains(" don't ")
                || lowered.contains("source of truth")
                || lowered.contains("canonical")
                || lowered.contains("preferred")
                || lowered.contains("required")
        }
    }
}

#[derive(Debug, Default)]
struct IdentifierRouteSignal {
    literal_match_count: usize,
    fragment_match_count: usize,
    kind_match_count: usize,
    focus_term_match_count: usize,
    partial_literal_only: bool,
    matched_literals: Vec<String>,
    matched_kinds: Vec<IdentifierKind>,
    matched_focus_terms: Vec<String>,
}

fn analyze_identifier_signal(
    memory: &crate::memory::Memory,
    route: &IdentifierRouteProfile,
) -> IdentifierRouteSignal {
    let text = format!(
        "{} {} {} {} {}",
        memory.content,
        memory.tags.join(" "),
        memory.agent.as_deref().unwrap_or(""),
        memory.session.as_deref().unwrap_or(""),
        memory.source_key.as_deref().unwrap_or("")
    );
    let lowered = normalize_identifier_text(&text);
    let mut signal = IdentifierRouteSignal::default();

    for ident in &route.identifiers {
        let normalized_ident = normalize_identifier_text(ident);
        if normalized_ident.len() < 3 {
            continue;
        }
        if lowered.contains(&normalized_ident) {
            signal.literal_match_count += 1;
            signal.matched_literals.push(normalized_ident);
            continue;
        }

        let parts = tokenized_identifier_parts(&normalized_ident);
        if parts.len() >= 2 {
            let matched = parts
                .iter()
                .filter(|part| lowered.contains(part.as_str()))
                .count();
            if matched >= parts.len().min(3) {
                signal.fragment_match_count += 1;
            }
        }
    }

    signal.partial_literal_only =
        signal.literal_match_count == 0 && signal.fragment_match_count > 0;

    for kind in &route.kinds {
        if content_matches_identifier_kind(&text, *kind) {
            signal.kind_match_count += 1;
            signal.matched_kinds.push(*kind);
        }
    }

    for focus_term in &route.focus_terms {
        if lowered.contains(focus_term) {
            signal.focus_term_match_count += 1;
            signal.matched_focus_terms.push(focus_term.clone());
        }
    }

    signal.matched_literals.sort_unstable();
    signal.matched_literals.dedup();
    signal.matched_kinds.sort_by_key(|kind| kind.as_str());
    signal.matched_kinds.dedup();
    signal.matched_focus_terms.sort_unstable();
    signal.matched_focus_terms.dedup();
    signal
}

fn task_context_boost(
    memory: &crate::memory::Memory,
    task_context: &TaskContext,
    max_channel: f64,
    base_score: f64,
) -> (f64, Vec<String>) {
    let tag_text = memory.tags.join(" ");
    let tag_hits = collect_context_hits(&tag_text, task_context.kind.memory_hints());
    let content_hits = collect_context_hits(&memory.content, task_context.kind.memory_hints());

    if tag_hits.is_empty() && content_hits.is_empty() {
        return (0.0, Vec::new());
    }

    // Fail closed: task context is only allowed to re-rank plausible candidates,
    // not to resurrect semantically irrelevant memories from weak retrieval noise.
    if max_channel < 0.05 && base_score < 0.05 && tag_hits.is_empty() {
        return (0.0, Vec::new());
    }

    let boost = (tag_hits.len() as f64 * 0.08 + content_hits.len() as f64 * 0.035).min(0.18);
    let mut provenance = vec![format!("task_context:{}", task_context.label())];
    provenance.extend(
        tag_hits
            .into_iter()
            .map(|hit| format!("task_match:tag:{hit}")),
    );
    provenance.extend(
        content_hits
            .into_iter()
            .map(|hit| format!("task_match:content:{hit}")),
    );

    (boost, provenance)
}

fn looks_like_non_memory_query(query: &str) -> bool {
    let normalized = normalize_task_text(query);
    let padded = format!(" {normalized} ");
    [
        " how are you ",
        " joke ",
        " jokes ",
        " poem ",
        " poems ",
        " story ",
        " stories ",
        " coffee ",
        " be back ",
        " grab coffee ",
    ]
    .iter()
    .any(|pattern| padded.contains(pattern))
}

fn is_under_specified_query(query: &str) -> bool {
    let normalized = normalize_task_text(query);
    let informative_tokens = normalized
        .split_whitespace()
        .filter(|token| {
            token.len() >= 4
                && !matches!(
                    *token,
                    "what"
                        | "when"
                        | "where"
                        | "which"
                        | "should"
                        | "after"
                        | "before"
                        | "here"
                        | "there"
                        | "this"
                        | "that"
                        | "with"
                        | "from"
                        | "about"
                        | "into"
                        | "your"
                        | "have"
                        | "will"
                )
        })
        .count();
    let has_specific_marker = query
        .chars()
        .any(|ch| matches!(ch, '/' | '_' | '.' | ':' | '-' | '`' | '"' | '\''));
    !has_specific_marker && informative_tokens <= 2
}

fn looks_like_next_step_query(query: &str) -> bool {
    let normalized = normalize_task_text(query);
    let padded = format!(" {normalized} ");
    [
        " what should happen after ",
        " what happens after ",
        " what should happen next ",
        " what should we do next ",
    ]
    .iter()
    .any(|pattern| padded.contains(pattern))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetrievalConfidenceDecision {
    Inject,
    Abstain,
    NeedMoreEvidence,
}

impl std::fmt::Display for RetrievalConfidenceDecision {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Inject => write!(f, "inject"),
            Self::Abstain => write!(f, "abstain"),
            Self::NeedMoreEvidence => write!(f, "need_more_evidence"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetrievalConfidenceGate {
    pub enabled: bool,
    pub decision: RetrievalConfidenceDecision,
    pub reasons: Vec<String>,
    pub considered_count: usize,
    pub qualified_count: usize,
    pub unsafe_filtered: usize,
    pub ineligible_filtered: usize,
    pub low_trust_filtered: usize,
    pub below_threshold_filtered: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_score: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub second_score: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub score_gap: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_max_channel_score: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_trust_score: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_confidence: Option<f64>,
}

const GATE_AMBIGUOUS_GAP: f64 = 0.12;
const GATE_LOW_SUPPORT_MAX_CHANNEL: f64 = 0.22;
const GATE_LOW_CONFIDENCE_THRESHOLD: f64 = 0.55;
const GATE_WEAK_MARGIN: f64 = 0.08;
const GATE_NEAR_THRESHOLD_MARGIN: f64 = 0.03;

/// Content-policy filter: reject memories that look like prompt injection attempts.
/// Uses multi-layer defense: blocklist + structural patterns + NFKC unicode normalization.
pub fn is_safe_for_injection(content: &str) -> bool {
    use unicode_normalization::UnicodeNormalization;

    let normalized: String = content
        .nfkc()
        .filter(|c| {
            !matches!(
                c,
                '\u{200B}'
                    | '\u{200C}'
                    | '\u{200D}'
                    | '\u{FEFF}'
                    | '\u{00AD}'
                    | '\u{2060}'
                    | '\u{180E}'
            )
        })
        .collect::<String>()
        .to_lowercase();

    let structural_patterns = [
        "system:",
        "assistant:",
        "human:",
        "[system]",
        "[/system]",
        "<|im_end|>",
        "<|im_start|>",
        "[inst]",
        "[/inst]",
        "<|endoftext|>",
        "<|begin_of_text|>",
        "</s>",
        "<s>",
        "ignore previous",
        "ignore all instruction",
        "ignore above",
        "disregard above",
        "disregard previous",
        "disregard all",
        "forget everything",
        "forget all instruction",
        "new instructions:",
        "override your",
        "override the",
        "you are now",
        "act as if",
        "pretend you are",
        "do not follow",
        "stop being",
        "system prompt:",
        "reveal your prompt",
        "show your instructions",
        "<!--",
        "-->",
        "### system",
        "## system",
        "# system",
        "### assistant",
        "## assistant",
        "# assistant",
    ];

    if structural_patterns.iter().any(|p| normalized.contains(p)) {
        return false;
    }

    let special_ratio = content
        .chars()
        .filter(|c| matches!(c, '<' | '>' | '{' | '}' | '|' | '\\'))
        .count() as f64
        / content.len().max(1) as f64;
    special_ratio <= 0.15
}

fn qualifies_for_proxy_injection(entry: &ScoreExplainEntry, min_recall_score: f64) -> bool {
    entry.final_score >= min_recall_score
        && is_safe_for_injection(&entry.memory.content)
        && entry.memory.eligible_for_injection()
        && !entry.low_trust
}

fn explained_to_scored(entry: &ScoreExplainEntry) -> ScoredMemory {
    ScoredMemory {
        memory: entry.memory.clone(),
        score: entry.final_score,
        provenance: entry.provenance.clone(),
        trust_score: entry.trust_score,
        low_trust: entry.low_trust,
    }
}

fn build_gate_summary(
    entries: &[ScoreExplainEntry],
    query: &str,
    min_recall_score: f64,
    enabled: bool,
) -> RetrievalConfidenceGate {
    let considered_count = entries.len();
    let qualified: Vec<&ScoreExplainEntry> = entries
        .iter()
        .filter(|entry| qualifies_for_proxy_injection(entry, min_recall_score))
        .collect();
    let unsafe_filtered = entries
        .iter()
        .filter(|entry| !is_safe_for_injection(&entry.memory.content))
        .count();
    let ineligible_filtered = entries
        .iter()
        .filter(|entry| {
            is_safe_for_injection(&entry.memory.content) && !entry.memory.eligible_for_injection()
        })
        .count();
    let low_trust_filtered = entries
        .iter()
        .filter(|entry| {
            is_safe_for_injection(&entry.memory.content)
                && entry.memory.eligible_for_injection()
                && entry.low_trust
        })
        .count();
    let below_threshold_filtered = entries
        .iter()
        .filter(|entry| {
            is_safe_for_injection(&entry.memory.content)
                && entry.memory.eligible_for_injection()
                && !entry.low_trust
                && entry.final_score < min_recall_score
        })
        .count();

    let top = qualified.first().copied().or_else(|| entries.first());
    let second = qualified.get(1).copied();
    let score_gap =
        top.and_then(|top_entry| second.map(|other| top_entry.final_score - other.final_score));

    let mut reasons = Vec::new();
    let actual_decision = if enabled && looks_like_non_memory_query(query) {
        reasons.push("query_not_memory_seeking".to_string());
        RetrievalConfidenceDecision::Abstain
    } else if let Some(top_entry) = top {
        if qualified.is_empty() {
            let plausible =
                top_entry.final_score >= (min_recall_score - GATE_NEAR_THRESHOLD_MARGIN).max(0.0);
            if !is_safe_for_injection(&top_entry.memory.content) {
                reasons.push("top_candidate_unsafe".to_string());
            } else if !top_entry.memory.eligible_for_injection() {
                reasons.push("top_candidate_ineligible".to_string());
            } else if top_entry.low_trust {
                reasons.push("top_candidate_low_trust".to_string());
            } else if plausible {
                reasons.push("top_candidate_below_threshold".to_string());
            } else {
                reasons.push("no_plausible_candidate".to_string());
            }

            if enabled && plausible {
                RetrievalConfidenceDecision::NeedMoreEvidence
            } else {
                RetrievalConfidenceDecision::Abstain
            }
        } else {
            let top_has_identifier = has_strong_identifier_provenance(&top_entry.provenance);
            let second_has_identifier =
                second.is_some_and(|other| has_strong_identifier_provenance(&other.provenance));
            if (qualified.len() >= 2 || (entries.len() > 1 && looks_like_next_step_query(query)))
                && is_under_specified_query(query)
            {
                reasons.push("query_under_specified".to_string());
            }
            if score_gap.is_some_and(|gap| gap < GATE_AMBIGUOUS_GAP)
                && (!top_has_identifier || second_has_identifier)
            {
                reasons.push("top_candidates_too_close".to_string());
            }
            if top_entry.max_channel_score < GATE_LOW_SUPPORT_MAX_CHANNEL {
                reasons.push("top_candidate_low_channel_support".to_string());
            }
            if top_entry.final_score < min_recall_score + GATE_WEAK_MARGIN
                && !has_strong_identifier_provenance(&top_entry.provenance)
            {
                reasons.push("top_candidate_near_threshold".to_string());
            }
            if top_entry.memory.confidence.unwrap_or(1.0) < GATE_LOW_CONFIDENCE_THRESHOLD
                && top_entry.memory.confirm_count == 0
                && top_entry.memory.evidence_count == 0
            {
                reasons.push("top_candidate_low_confidence".to_string());
            }

            if enabled && !reasons.is_empty() {
                RetrievalConfidenceDecision::NeedMoreEvidence
            } else {
                if reasons.is_empty() {
                    reasons.push("strong_top_candidate".to_string());
                } else if !enabled {
                    reasons.insert(0, "confidence_gate_disabled".to_string());
                }
                RetrievalConfidenceDecision::Inject
            }
        }
    } else {
        reasons.push("no_candidates".to_string());
        RetrievalConfidenceDecision::Abstain
    };

    RetrievalConfidenceGate {
        enabled,
        decision: actual_decision,
        reasons,
        considered_count,
        qualified_count: qualified.len(),
        unsafe_filtered,
        ineligible_filtered,
        low_trust_filtered,
        below_threshold_filtered,
        top_score: top.map(|entry| entry.final_score),
        second_score: second.map(|entry| entry.final_score),
        score_gap,
        top_max_channel_score: top.map(|entry| entry.max_channel_score),
        top_trust_score: top.map(|entry| entry.trust_score),
        top_confidence: top.map(|entry| entry.memory.confidence.unwrap_or(1.0)),
    }
}

fn has_strong_identifier_provenance(provenance: &[String]) -> bool {
    provenance.iter().any(|entry| {
        entry.starts_with("identifier_match:")
            || entry == "identifier_kind_match:endpoint"
            || entry == "identifier_kind_match:env_var"
            || entry == "identifier_kind_match:path"
            || entry == "identifier_kind_match:branch_pattern"
            || entry == "identifier_kind_match:commit"
            || entry == "identifier_kind_match:policy"
    })
}

pub fn apply_retrieval_confidence_gate(
    entries: &[ScoreExplainEntry],
    query: &str,
    min_recall_score: f64,
    enabled: bool,
) -> (RetrievalConfidenceGate, Vec<ScoredMemory>) {
    let gate = build_gate_summary(entries, query, min_recall_score, enabled);
    let qualified = if gate.decision == RetrievalConfidenceDecision::Inject {
        entries
            .iter()
            .filter(|entry| qualifies_for_proxy_injection(entry, min_recall_score))
            .map(explained_to_scored)
            .collect()
    } else {
        Vec::new()
    };
    (gate, qualified)
}

pub fn apply_scored_retrieval_confidence_gate(
    memories: &[ScoredMemory],
    query: &str,
    min_recall_score: f64,
    enabled: bool,
) -> (RetrievalConfidenceGate, Vec<ScoredMemory>) {
    let qualified: Vec<ScoredMemory> = memories
        .iter()
        .filter(|sm| {
            sm.score >= min_recall_score
                && is_safe_for_injection(&sm.memory.content)
                && sm.memory.eligible_for_injection()
                && !sm.low_trust
        })
        .cloned()
        .collect();
    let top = qualified.first().or_else(|| memories.first());
    let second = qualified.get(1);
    let score_gap = top.and_then(|top_entry| second.map(|other| top_entry.score - other.score));

    let mut reasons = Vec::new();
    let decision = if enabled && looks_like_non_memory_query(query) {
        reasons.push("query_not_memory_seeking".to_string());
        RetrievalConfidenceDecision::Abstain
    } else if let Some(top_entry) = top {
        if qualified.is_empty() {
            let plausible =
                top_entry.score >= (min_recall_score - GATE_NEAR_THRESHOLD_MARGIN).max(0.0);
            if !is_safe_for_injection(&top_entry.memory.content) {
                reasons.push("top_candidate_unsafe".to_string());
            } else if !top_entry.memory.eligible_for_injection() {
                reasons.push("top_candidate_ineligible".to_string());
            } else if top_entry.low_trust {
                reasons.push("top_candidate_low_trust".to_string());
            } else if plausible {
                reasons.push("top_candidate_below_threshold".to_string());
            } else {
                reasons.push("no_plausible_candidate".to_string());
            }

            if enabled && plausible {
                RetrievalConfidenceDecision::NeedMoreEvidence
            } else {
                RetrievalConfidenceDecision::Abstain
            }
        } else {
            let top_has_identifier = has_strong_identifier_provenance(&top_entry.provenance);
            let second_has_identifier =
                second.is_some_and(|other| has_strong_identifier_provenance(&other.provenance));
            if (qualified.len() >= 2 || (memories.len() > 1 && looks_like_next_step_query(query)))
                && is_under_specified_query(query)
            {
                reasons.push("query_under_specified".to_string());
            }
            if score_gap.is_some_and(|gap| gap < GATE_AMBIGUOUS_GAP)
                && (!top_has_identifier || second_has_identifier)
            {
                reasons.push("top_candidates_too_close".to_string());
            }
            if top_entry.score < min_recall_score + GATE_WEAK_MARGIN
                && !has_strong_identifier_provenance(&top_entry.provenance)
            {
                reasons.push("top_candidate_near_threshold".to_string());
            }
            if top_entry.memory.confidence.unwrap_or(1.0) < GATE_LOW_CONFIDENCE_THRESHOLD
                && top_entry.memory.confirm_count == 0
                && top_entry.memory.evidence_count == 0
            {
                reasons.push("top_candidate_low_confidence".to_string());
            }

            if enabled && !reasons.is_empty() {
                RetrievalConfidenceDecision::NeedMoreEvidence
            } else {
                if reasons.is_empty() {
                    reasons.push("strong_top_candidate".to_string());
                } else if !enabled {
                    reasons.insert(0, "confidence_gate_disabled".to_string());
                }
                RetrievalConfidenceDecision::Inject
            }
        }
    } else {
        reasons.push("no_candidates".to_string());
        RetrievalConfidenceDecision::Abstain
    };

    (
        RetrievalConfidenceGate {
            enabled,
            decision,
            reasons,
            considered_count: memories.len(),
            qualified_count: qualified.len(),
            unsafe_filtered: memories
                .iter()
                .filter(|sm| !is_safe_for_injection(&sm.memory.content))
                .count(),
            ineligible_filtered: memories
                .iter()
                .filter(|sm| {
                    is_safe_for_injection(&sm.memory.content) && !sm.memory.eligible_for_injection()
                })
                .count(),
            low_trust_filtered: memories
                .iter()
                .filter(|sm| {
                    is_safe_for_injection(&sm.memory.content)
                        && sm.memory.eligible_for_injection()
                        && sm.low_trust
                })
                .count(),
            below_threshold_filtered: memories
                .iter()
                .filter(|sm| {
                    is_safe_for_injection(&sm.memory.content)
                        && sm.memory.eligible_for_injection()
                        && !sm.low_trust
                        && sm.score < min_recall_score
                })
                .count(),
            top_score: top.map(|sm| sm.score),
            second_score: second.map(|sm| sm.score),
            score_gap,
            top_max_channel_score: None,
            top_trust_score: top.map(|sm| sm.trust_score),
            top_confidence: top.map(|sm| sm.memory.confidence.unwrap_or(1.0)),
        },
        if decision == RetrievalConfidenceDecision::Inject {
            qualified
        } else {
            Vec::new()
        },
    )
}

/// Options for score_and_merge — configurable per caller.
#[derive(Debug, Clone)]
pub struct MergeOptions {
    pub weights: ScoringWeights,
    /// IDF boost factor (1.0 = no boost). Computed from query terms.
    pub idf_boost: f64,
    /// Minimum score in at least one channel to pass precision gate.
    pub min_channel_score: f64,
    /// Confidence penalty: apply quadratic curve (0.3 + 0.7 * conf²).
    pub apply_confidence_penalty: bool,
    /// Trust scoring: compute and attach trust_score/low_trust.
    pub apply_trust_scoring: bool,
    /// Namespace for doc lookups.
    pub namespace: String,
    /// Maximum results to return.
    pub limit: usize,
    /// Agent filter (for decompose sub-queries).
    pub agent_filter: Option<String>,
    /// Diversity factor: 0.0 = pure relevance, >0 = spread across agents/tags.
    pub diversity_factor: f64,
    /// Optional lightweight task context classifier used for re-ranking.
    pub task_context: Option<TaskContext>,
    /// Optional identifier-first lexical routing profile for code-, path-, and policy-heavy queries.
    pub identifier_route: Option<IdentifierRouteProfile>,
}

impl Default for MergeOptions {
    fn default() -> Self {
        Self {
            weights: ScoringWeights::default(),
            idf_boost: 1.0,
            min_channel_score: 0.0,
            apply_confidence_penalty: false,
            apply_trust_scoring: false,
            namespace: "default".to_string(),
            limit: 10,
            agent_filter: None,
            diversity_factor: 0.0,
            task_context: None,
            identifier_route: None,
        }
    }
}

/// Compute IDF boost factor from query terms.
pub fn compute_idf_boost(idf_index: &IdfIndex, query: &str) -> f64 {
    let query_terms: Vec<&str> = query.split_whitespace().collect();
    let idf_scores = idf_index.idf_batch(&query_terms);
    let idf_sum: f64 = idf_scores.iter().map(|(_, s)| *s).sum();
    if idf_sum > 0.0 && !idf_scores.is_empty() {
        idf_sum / idf_scores.len() as f64
    } else {
        1.0
    }
}

/// Extract identifier-like tokens from query for exact matching.
/// Matches: snake_case, CamelCase, paths, URLs, quoted strings.
pub fn extract_identifiers(query: &str) -> Vec<String> {
    let mut identifiers = Vec::new();

    // Quoted strings
    let mut in_quote = false;
    let mut current = String::new();
    for ch in query.chars() {
        if ch == '"' || ch == '\'' || ch == '`' {
            if in_quote && !current.is_empty() {
                identifiers.push(current.clone());
                current.clear();
            }
            in_quote = !in_quote;
            continue;
        }
        if in_quote {
            current.push(ch);
        }
    }

    // snake_case and CamelCase tokens (3+ chars with underscore or mixed case)
    for word in query.split(|c: char| c.is_whitespace() || c == ',' || c == ';') {
        let trimmed = word.trim_matches(|c: char| {
            !c.is_alphanumeric() && c != '_' && c != '-' && c != '.' && c != '/' && c != ':'
        });
        if trimmed.len() < 3 {
            continue;
        }
        // Path-like: contains / or .rs or .py etc
        if trimmed.contains('/') || trimmed.contains("::") {
            identifiers.push(trimmed.to_string());
            continue;
        }
        // URL-like
        if trimmed.starts_with("http") || trimmed.contains("://") {
            identifiers.push(trimmed.to_string());
            continue;
        }
        // snake_case: contains underscore
        if trimmed.contains('_') {
            identifiers.push(trimmed.to_string());
            continue;
        }
        // CamelCase: has uppercase after lowercase
        let has_camel = trimmed
            .chars()
            .zip(trimmed.chars().skip(1))
            .any(|(a, b)| a.is_lowercase() && b.is_uppercase());
        if has_camel {
            identifiers.push(trimmed.to_string());
            continue;
        }
        // kebab-case with 2+ segments
        if trimmed.contains('-') && trimmed.split('-').count() >= 2 {
            identifiers.push(trimmed.to_string());
        }
    }

    identifiers
}

/// Perform exact/substring matching against FTS engine for identifier tokens.
/// Returns Vec<(Uuid, score)> where score is 1.0 for exact matches.
pub fn exact_match_search(
    fts_engine: &FtsEngine,
    identifiers: &[String],
    limit: usize,
) -> Vec<(Uuid, f32)> {
    if identifiers.is_empty() {
        return Vec::new();
    }

    // Build a tantivy phrase query for each identifier
    let mut results: HashMap<Uuid, f32> = HashMap::new();
    for ident in identifiers {
        let specificity = identifier_specificity(ident);
        if let Ok(hits) = fts_engine.search_identifier(ident, limit) {
            for (uuid, score) in hits {
                let entry = results.entry(uuid).or_insert(0.0);
                *entry = (*entry).max(score * specificity);
            }
        }
    }

    let mut sorted: Vec<(Uuid, f32)> = results.into_iter().collect();
    sorted.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    sorted.truncate(limit);
    sorted
}

/// Minimum number of results in a channel before normalization is reliable.
/// Below this threshold, the channel score is zeroed to avoid batch-relative distortion.
const MIN_CHANNEL_RESULTS: usize = 3;

/// Absolute floor for vector cosine similarity (AllMiniLM-L6-V2 range).
/// Results below this are considered noise even if they're the "best" in the batch.
const VEC_ABS_FLOOR: f32 = 0.25;

/// Absolute floor for BM25 raw scores. Below this, FTS match is noise.
const FTS_ABS_FLOOR: f32 = 1.0;

/// Maximum IDF boost multiplier to prevent rare-term domination.
const MAX_IDF_BOOST: f64 = 1.5;

fn identifier_specificity(identifier: &str) -> f32 {
    let lowered = normalize_identifier_text(identifier);
    if lowered.contains("/v1/") || lowered.contains("http://") || lowered.contains("https://") {
        1.65
    } else if lowered.starts_with('/') || lowered.contains(".rs") || lowered.contains(".toml") {
        1.5
    } else if contains_env_var(identifier) {
        1.45
    } else if lowered.starts_with("feat/") || lowered.starts_with("fix/") {
        1.35
    } else if contains_hex_commit(&lowered) {
        1.5
    } else if lowered.contains("policy") || lowered.contains("preference") {
        1.2
    } else {
        1.0
    }
}

fn route_adjusted_channel_weights(
    weights: &ScoringWeights,
    identifier_route: Option<&IdentifierRouteProfile>,
) -> (f64, f64, f64) {
    let mut vector = weights.vector;
    let mut fts = weights.fts;
    let mut exact = weights.exact;
    if identifier_route.is_some_and(|route| route.active) {
        vector *= 0.68;
        fts *= 1.2;
        exact *= 1.55;
    }
    let original_total = weights.vector + weights.fts + weights.exact;
    let routed_total = vector + fts + exact;
    let scale = if routed_total > 0.0 && original_total > 0.0 {
        original_total / routed_total
    } else {
        1.0
    };
    (vector * scale, fts * scale, exact * scale)
}

/// Core scoring and explanation function.
/// Takes raw results from vector, FTS, and exact-match channels,
/// merges them with configurable weights, applies IDF boost, precision gate,
/// confidence penalty, trust scoring, and diversity.
///
/// Calibration fixes applied:
/// - P1: Floor-based normalization (not pure batch-relative)
/// - P2: 7-day recency time scale (consistent with trust)
/// - P3: IDF boost capped at 1.5x
/// - P4: Quadratic confidence penalty
/// - P5: Trust as score multiplier (not just filter)
/// - P6: Cold-start guard (skip normalization with < MIN_CHANNEL_RESULTS)
pub fn score_and_explain(
    vector_results: &[(Uuid, f32)],
    fts_results: &[(Uuid, f32)],
    exact_results: &[(Uuid, f32)],
    doc_engine: &Arc<DocumentEngine>,
    trust_scorer: Option<&Arc<TrustScorer>>,
    options: &MergeOptions,
) -> Vec<ScoreExplainEntry> {
    let weights = &options.weights;
    let identifier_route = options.identifier_route.as_ref();
    let (vector_weight, fts_weight, exact_weight) =
        route_adjusted_channel_weights(weights, identifier_route);

    // P1+P6: Floor-based normalization with cold-start guard.
    // If a channel has fewer than MIN_CHANNEL_RESULTS results, its scores are zeroed
    // to avoid a single poor result being normalized to 1.0.
    // The floor ensures that low-quality raw scores don't inflate after normalization.
    let vec_max = vector_results
        .iter()
        .map(|(_, s)| *s)
        .fold(0.0f32, f32::max);
    let fts_max = fts_results.iter().map(|(_, s)| *s).fold(0.0f32, f32::max);
    let exact_max = exact_results.iter().map(|(_, s)| *s).fold(0.0f32, f32::max);

    let vec_reliable = vector_results.len() >= MIN_CHANNEL_RESULTS;
    let fts_reliable = fts_results.len() >= MIN_CHANNEL_RESULTS;
    let identifier_route_has_strong_support = identifier_route.is_some_and(|route| {
        route.active
            && (!route.identifiers.is_empty()
                || exact_max > 0.0
                || fts_results.len() >= MIN_CHANNEL_RESULTS)
    });

    let mut channel_map: HashMap<Uuid, ChannelScores> = HashMap::new();

    // Vector channel: floor-based normalization
    for (uuid, sim) in vector_results {
        let normalized = if vec_reliable && vec_max > VEC_ABS_FLOOR {
            // Shift by floor, then normalize to [0, 1]
            ((*sim - VEC_ABS_FLOOR).max(0.0) / (vec_max - VEC_ABS_FLOOR).max(0.01)) as f64
        } else if !vec_reliable {
            // Cold-start: use raw similarity (clamped) instead of batch-relative
            (*sim as f64).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let entry = channel_map.entry(*uuid).or_default();
        entry.vector = normalized;
        entry.provenance.push("vector".to_string());
    }

    // P3: Cap IDF boost to prevent FTS channel domination
    let capped_idf = options.idf_boost.min(MAX_IDF_BOOST);

    // FTS channel: floor-based normalization + capped IDF boost
    for (uuid, bm25) in fts_results {
        let normalized = if fts_reliable && fts_max > FTS_ABS_FLOOR {
            ((*bm25 - FTS_ABS_FLOOR).max(0.0) / (fts_max - FTS_ABS_FLOOR).max(0.01)) as f64
        } else if !fts_reliable {
            if fts_max > 0.0 {
                (*bm25 / fts_max) as f64
            } else {
                0.0
            }
        } else {
            0.0
        };
        let boosted = (normalized * capped_idf).min(1.0);
        let entry = channel_map.entry(*uuid).or_default();
        entry.fts = boosted;
        if !entry.provenance.contains(&"fts".to_string()) {
            entry.provenance.push("fts".to_string());
        }
    }

    // Exact match channel (binary-ish, no floor needed)
    for (uuid, score) in exact_results {
        let normalized = if exact_max > 0.0 {
            (*score / exact_max).max(0.0) as f64
        } else {
            0.0
        };
        let entry = channel_map.entry(*uuid).or_default();
        entry.exact = normalized;
        if !entry.provenance.contains(&"exact".to_string()) {
            entry.provenance.push("exact".to_string());
        }
    }

    // Fetch memories, apply filters, compute final scores
    let mut explained = Vec::new();
    for (uuid, channels) in channel_map {
        // Precision gate: at least one channel must exceed threshold
        let max_channel = channels.vector.max(channels.fts).max(channels.exact);
        if options.min_channel_score > 0.0 {
            if max_channel < options.min_channel_score {
                continue;
            }
        }

        if let Ok(Some(memory)) = doc_engine.get(uuid, &options.namespace) {
            if memory.archived {
                continue;
            }

            // Agent filter
            if let Some(ref agent) = options.agent_filter {
                if agent != "_none" {
                    if memory.agent.as_deref() != Some(agent) {
                        continue;
                    }
                } else if memory.agent.is_some() {
                    continue;
                }
            }

            // Weighted score
            let mut provenance = channels.provenance.clone();
            let base_score = channels.vector * vector_weight
                + channels.fts * fts_weight
                + channels.exact * exact_weight;

            // P2: Recency with 7-day time scale (consistent with trust recency)
            let age_hours = (chrono::Utc::now() - memory.created_at).num_hours().max(0) as f64;
            let recency = (1.0 / (1.0 + age_hours / 168.0)) * weights.recency;

            let mut task_boost = 0.0;
            if let Some(task_context) = options.task_context.as_ref() {
                let (boost, task_provenance) =
                    task_context_boost(&memory, task_context, max_channel, base_score);
                if boost > 0.0 {
                    task_boost = boost;
                    for signal in task_provenance {
                        if !provenance.contains(&signal) {
                            provenance.push(signal);
                        }
                    }
                }
            }

            let mut final_score = base_score + recency + task_boost;

            if let Some(route) = identifier_route {
                let signal = analyze_identifier_signal(&memory, route);
                if signal.literal_match_count > 0 {
                    final_score += 0.08 * signal.literal_match_count.min(2) as f64;
                    for literal in signal.matched_literals.iter().take(2) {
                        let label = literal.chars().take(48).collect::<String>();
                        let provenance_signal = format!("identifier_match:{label}");
                        if !provenance.contains(&provenance_signal) {
                            provenance.push(provenance_signal);
                        }
                    }
                }
                if signal.kind_match_count > 0 {
                    let kind_bonus: f64 = signal
                        .matched_kinds
                        .iter()
                        .map(|kind| match kind {
                            IdentifierKind::EnvVar
                            | IdentifierKind::Endpoint
                            | IdentifierKind::Path => 0.07,
                            IdentifierKind::BranchPattern | IdentifierKind::Commit => 0.055,
                            IdentifierKind::Policy => 0.04,
                        })
                        .sum::<f64>()
                        .min(0.16);
                    final_score += kind_bonus;
                    for kind in signal.matched_kinds.iter().take(2) {
                        let provenance_signal = format!("identifier_kind_match:{}", kind.as_str());
                        if !provenance.contains(&provenance_signal) {
                            provenance.push(provenance_signal);
                        }
                    }
                }
                if signal.focus_term_match_count > 0 {
                    final_score += 0.03 * signal.focus_term_match_count.min(3) as f64;
                    for term in signal.matched_focus_terms.iter().take(2) {
                        let provenance_signal = format!("identifier_focus_match:{term}");
                        if !provenance.contains(&provenance_signal) {
                            provenance.push(provenance_signal);
                        }
                    }
                }

                let has_lexical_support = channels.exact > 0.0
                    || channels.fts >= 0.2
                    || signal.literal_match_count > 0
                    || signal.kind_match_count > 0
                    || signal.focus_term_match_count > 0;

                if signal.partial_literal_only {
                    final_score *= 0.82;
                    if !provenance
                        .iter()
                        .any(|entry| entry == "identifier_route:ambiguous_fragment")
                    {
                        provenance.push("identifier_route:ambiguous_fragment".to_string());
                    }
                }

                if identifier_route_has_strong_support && !has_lexical_support {
                    final_score *= 0.64;
                    if !provenance
                        .iter()
                        .any(|entry| entry == "identifier_route:vector_demoted")
                    {
                        provenance.push("identifier_route:vector_demoted".to_string());
                    }
                } else if !route.identifiers.is_empty()
                    && signal.literal_match_count == 0
                    && signal.fragment_match_count == 0
                    && channels.exact == 0.0
                {
                    final_score *= 0.78;
                    if !provenance
                        .iter()
                        .any(|entry| entry == "identifier_route:missing_literal")
                    {
                        provenance.push("identifier_route:missing_literal".to_string());
                    }
                } else if route.identifiers.is_empty()
                    && !route.kinds.is_empty()
                    && signal.kind_match_count == 0
                {
                    final_score *= 0.65;
                    if !provenance
                        .iter()
                        .any(|entry| entry == "identifier_route:kind_mismatch")
                    {
                        provenance.push("identifier_route:kind_mismatch".to_string());
                    }
                } else if !route.focus_terms.is_empty() && signal.focus_term_match_count == 0 {
                    final_score *= 0.78;
                    if !provenance
                        .iter()
                        .any(|entry| entry == "identifier_route:focus_mismatch")
                    {
                        provenance.push("identifier_route:focus_mismatch".to_string());
                    }
                }
            }

            // P4: Quadratic confidence penalty (harsher on low-confidence proxy-extracted memories)
            // conf=0.3 → 0.363 (was 0.65), conf=0.7 → 0.643, conf=1.0 → 1.0
            let mut confidence_factor = 1.0;
            if options.apply_confidence_penalty {
                let cf = memory.confidence.unwrap_or(1.0).clamp(0.0, 1.0);
                confidence_factor = 0.3 + 0.7 * cf * cf;
                final_score *= confidence_factor;
            }

            let status_factor = memory.status_factor();
            final_score *= status_factor;

            // Trust scoring
            let (
                trust_score,
                low_trust,
                trust_confidence_low,
                trust_confidence_high,
                trust_signals,
            ) = if options.apply_trust_scoring {
                if let Some(ts) = trust_scorer {
                    let result = ts.score_memory(&memory, &options.namespace);
                    (
                        result.score,
                        result.low_trust,
                        result.confidence_low,
                        result.confidence_high,
                        result.signals,
                    )
                } else {
                    (
                        1.0,
                        false,
                        1.0,
                        1.0,
                        crate::security::trust::TrustSignals {
                            recency: 1.0,
                            source_reputation: 1.0,
                            embedding_coherence: 1.0,
                            access_frequency: 1.0,
                            outcome_learning: 1.0,
                        },
                    )
                }
            } else {
                let trust = memory.recency_trust();
                (
                    trust,
                    trust < 0.3,
                    trust,
                    trust,
                    crate::security::trust::TrustSignals {
                        recency: trust,
                        source_reputation: 0.5,
                        embedding_coherence: 0.5,
                        access_frequency: 0.0,
                        outcome_learning: memory.outcome_signal(),
                    },
                )
            };

            // P5: Trust as score multiplier (not just a binary filter).
            // Range [0.6, 1.0]: trust=0.31 → 0.72, trust=0.95 → 0.98
            let mut trust_multiplier = 1.0;
            if options.apply_trust_scoring {
                trust_multiplier = 0.6 + 0.4 * trust_score;
                final_score *= trust_multiplier;
            }

            explained.push(ScoreExplainEntry {
                memory,
                provenance,
                channels: ScoreChannels {
                    vector: channels.vector,
                    fts: channels.fts,
                    exact: channels.exact,
                },
                max_channel_score: max_channel,
                base_score,
                recency_score: recency,
                confidence_factor,
                status_factor,
                trust_score,
                trust_confidence_low,
                trust_confidence_high,
                trust_signals,
                trust_multiplier,
                final_score,
                low_trust,
            });
        }
    }

    // Sort by score descending
    explained.sort_by(|a, b| {
        b.final_score
            .partial_cmp(&a.final_score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| b.memory.id.cmp(&a.memory.id))
    });

    // Diversity pass: if enabled, spread across agents/tags
    if options.diversity_factor > 0.0 && explained.len() > options.limit {
        explained = apply_diversity_explained(explained, options.limit, options.diversity_factor);
    } else {
        explained.truncate(options.limit);
    }

    explained
}

/// Compatibility wrapper for the existing recall paths.
pub fn score_and_merge(
    vector_results: &[(Uuid, f32)],
    fts_results: &[(Uuid, f32)],
    exact_results: &[(Uuid, f32)],
    doc_engine: &Arc<DocumentEngine>,
    trust_scorer: Option<&Arc<TrustScorer>>,
    options: &MergeOptions,
) -> Vec<ScoredMemory> {
    score_and_explain(
        vector_results,
        fts_results,
        exact_results,
        doc_engine,
        trust_scorer,
        options,
    )
    .into_iter()
    .map(|entry| ScoredMemory {
        memory: entry.memory,
        score: entry.final_score,
        provenance: entry.provenance,
        trust_score: entry.trust_score,
        low_trust: entry.low_trust,
    })
    .collect()
}

/// MMR-like diversity on metadata: penalize results from same agent/tag cluster.
fn apply_diversity_explained(
    mut candidates: Vec<ScoreExplainEntry>,
    limit: usize,
    diversity_factor: f64,
) -> Vec<ScoreExplainEntry> {
    let mut selected: Vec<ScoreExplainEntry> = Vec::with_capacity(limit);
    let max_per_agent = 3usize; // No more than 3 from same agent

    while selected.len() < limit && !candidates.is_empty() {
        // Find best candidate considering diversity penalty
        let mut best_idx = 0;
        let mut best_adjusted = f64::NEG_INFINITY;

        for (i, candidate) in candidates.iter().enumerate() {
            let mut penalty = 0.0;
            let candidate_agent = candidate.memory.agent.as_deref().unwrap_or("");

            // Agent overlap penalty
            let agent_count = selected
                .iter()
                .filter(|s| s.memory.agent.as_deref().unwrap_or("") == candidate_agent)
                .count();
            if !candidate_agent.is_empty() && agent_count >= max_per_agent {
                penalty += 1.0; // Hard penalty: skip if 3+ from same agent
            } else if agent_count > 0 {
                penalty += 0.1 * agent_count as f64;
            }

            // Tag overlap penalty
            if !candidate.memory.tags.is_empty() {
                let tag_overlap = selected
                    .iter()
                    .filter(|s| {
                        s.memory
                            .tags
                            .iter()
                            .any(|t| candidate.memory.tags.contains(t))
                    })
                    .count();
                penalty += 0.05 * tag_overlap as f64;
            }

            let adjusted = candidate.final_score - penalty * diversity_factor;
            if adjusted > best_adjusted {
                best_adjusted = adjusted;
                best_idx = i;
            }
        }

        selected.push(candidates.remove(best_idx));
    }

    selected
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::{Memory, ScoreChannels, ScoreExplainEntry};
    use crate::security::trust::TrustSignals;

    #[test]
    fn test_detect_task_context_prefers_deploy_keywords() {
        let context = detect_task_context(
            "Need the staging approval and rollout steps before production deploy",
        )
        .expect("expected deploy task context");
        assert_eq!(context.label(), "deploy");
        assert!(context.matched_terms.iter().any(|term| term == "staging"));
    }

    #[test]
    fn test_detect_task_context_fails_closed_on_ambiguous_query() {
        let context = detect_task_context("Review the rollout and debug the production issue");
        assert!(
            context.is_none(),
            "ambiguous query should not force a task context"
        );
    }

    #[test]
    fn test_detect_identifier_route_finds_env_and_endpoint_queries() {
        let route = detect_identifier_route("which env var should Claude proxy mode use?")
            .expect("expected identifier route");
        assert!(route.active);
        assert!(route.kinds.contains(&IdentifierKind::EnvVar));

        let endpoint_route =
            detect_identifier_route("which endpoint handles Anthropic messages through the proxy?")
                .expect("expected endpoint route");
        assert!(endpoint_route.kinds.contains(&IdentifierKind::Endpoint));
    }

    #[test]
    fn test_task_context_boost_prefers_matching_tags() {
        let context = TaskContext {
            kind: TaskContextKind::Review,
            matched_terms: vec!["review".to_string()],
        };
        let mut memory =
            crate::memory::Memory::new("Require tests and security review before merge.".into());
        memory.tags = vec!["review".into(), "security".into()];

        let (boost, provenance) = task_context_boost(&memory, &context, 0.12, 0.15);
        assert!(boost > 0.0);
        assert!(
            provenance
                .iter()
                .any(|entry| entry == "task_context:review")
        );
        assert!(
            provenance
                .iter()
                .any(|entry| entry == "task_match:tag:review")
        );
    }

    fn explained_entry(
        content: &str,
        final_score: f64,
        max_channel_score: f64,
        confidence: Option<f64>,
    ) -> ScoreExplainEntry {
        let mut memory = Memory::new(content.to_string());
        memory.confidence = confidence;
        ScoreExplainEntry {
            memory,
            provenance: vec!["vector".into()],
            channels: ScoreChannels {
                vector: max_channel_score,
                fts: 0.0,
                exact: 0.0,
            },
            max_channel_score,
            base_score: final_score,
            recency_score: 0.0,
            confidence_factor: 1.0,
            status_factor: 1.0,
            trust_score: 0.9,
            trust_confidence_low: 0.8,
            trust_confidence_high: 1.0,
            trust_signals: TrustSignals {
                recency: 1.0,
                source_reputation: 1.0,
                embedding_coherence: 1.0,
                access_frequency: 0.5,
                outcome_learning: 0.5,
            },
            trust_multiplier: 1.0,
            final_score,
            low_trust: false,
        }
    }

    #[test]
    fn test_retrieval_confidence_gate_injects_on_strong_candidate() {
        let entries = vec![explained_entry(
            "Deploys must pass smoke checks before rollout.",
            0.72,
            0.68,
            None,
        )];
        let (gate, qualified) =
            apply_retrieval_confidence_gate(&entries, "deploy rollout checklist", 0.4, true);
        assert_eq!(gate.decision, RetrievalConfidenceDecision::Inject);
        assert_eq!(qualified.len(), 1);
    }

    #[test]
    fn test_retrieval_confidence_gate_needs_more_evidence_when_top_candidates_are_close() {
        let entries = vec![
            explained_entry(
                "Use feat/<ticket>-slug for feature branches.",
                0.56,
                0.44,
                None,
            ),
            explained_entry(
                "Use fix/<ticket>-slug for bugfix branches.",
                0.51,
                0.42,
                None,
            ),
        ];
        let (gate, qualified) = apply_retrieval_confidence_gate(
            &entries,
            "what should happen after rollout?",
            0.4,
            true,
        );
        assert_eq!(gate.decision, RetrievalConfidenceDecision::NeedMoreEvidence);
        assert!(qualified.is_empty());
        assert!(
            gate.reasons
                .iter()
                .any(|reason| reason == "top_candidates_too_close")
        );
    }

    #[test]
    fn test_retrieval_confidence_gate_abstains_without_plausible_candidate() {
        let entries = vec![explained_entry(
            "General advice about database performance with no repo context.",
            0.18,
            0.09,
            None,
        )];
        let (gate, qualified) = apply_retrieval_confidence_gate(
            &entries,
            "tell me a joke about deployments",
            0.4,
            true,
        );
        assert_eq!(gate.decision, RetrievalConfidenceDecision::Abstain);
        assert!(qualified.is_empty());
    }

    #[test]
    fn test_identifier_route_analysis_detects_literal_and_kind_matches() {
        let route =
            detect_identifier_route("which endpoint handles Anthropic messages through the proxy?")
                .expect("expected identifier route");
        let memory =
            Memory::new("Claude proxy endpoint is /proxy/anthropic/v1/messages.".to_string());
        let signal = analyze_identifier_signal(&memory, &route);
        assert!(signal.kind_match_count >= 1);

        let literal_route =
            detect_identifier_route("what is /proxy/anthropic/v1/messages used for?")
                .expect("expected literal route");
        let literal_signal = analyze_identifier_signal(&memory, &literal_route);
        assert!(literal_signal.literal_match_count >= 1);
    }
}
