// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 memoryOSS Contributors

use std::collections::HashSet;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::memory::{
    MEMORY_RUNTIME_CONTRACT_ID, Memory, MemoryStatus, MemoryType, PassportImportPreview,
    PassportScope, build_memory_passport_bundle, plan_memory_passport_import,
};

pub const GIT_HISTORY_FIELD_SEP: char = '\u{1f}';
pub const GIT_HISTORY_RECORD_SEP: char = '\u{1e}';
const MAX_MARKDOWN_ITEMS: usize = 24;
const MAX_GIT_COMMITS: usize = 24;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryAdapterKind {
    ClaudeProject,
    CursorRules,
    GitHistory,
}

impl MemoryAdapterKind {
    pub fn client_name(&self) -> &'static str {
        match self {
            Self::ClaudeProject => "claude",
            Self::CursorRules => "cursor",
            Self::GitHistory => "git",
        }
    }

    pub fn content_type(&self) -> &'static str {
        match self {
            Self::ClaudeProject => "text/markdown",
            Self::CursorRules => "text/markdown",
            Self::GitHistory => "text/plain",
        }
    }

    pub fn default_extension(&self) -> &'static str {
        match self {
            Self::ClaudeProject => "md",
            Self::CursorRules => "mdc",
            Self::GitHistory => "txt",
        }
    }

    pub fn supports_export(&self) -> bool {
        !matches!(self, Self::GitHistory)
    }

    pub fn imported_status(&self) -> MemoryStatus {
        match self {
            Self::ClaudeProject | Self::CursorRules => MemoryStatus::Active,
            Self::GitHistory => MemoryStatus::Candidate,
        }
    }
}

impl std::fmt::Display for MemoryAdapterKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ClaudeProject => write!(f, "claude_project"),
            Self::CursorRules => write!(f, "cursor_rules"),
            Self::GitHistory => write!(f, "git_history"),
        }
    }
}

impl FromStr for MemoryAdapterKind {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "claude_project" | "claude-project" => Ok(Self::ClaudeProject),
            "cursor_rules" | "cursor-rules" => Ok(Self::CursorRules),
            "git_history" | "git-history" => Ok(Self::GitHistory),
            other => Err(format!("unsupported adapter kind: {other}")),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdapterImportPreview {
    pub adapter_kind: MemoryAdapterKind,
    pub source_label: String,
    pub runtime_contract_id: String,
    pub normalized_count: usize,
    pub preview: PassportImportPreview,
}

#[derive(Debug, Clone)]
pub struct AdapterImportPlan {
    pub preview: AdapterImportPreview,
    pub staged_memories: Vec<Memory>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdapterExportArtifact {
    pub adapter_kind: MemoryAdapterKind,
    pub namespace: String,
    pub exported_count: usize,
    pub content_type: String,
    pub content: String,
}

pub fn plan_adapter_import(
    target_namespace: &str,
    kind: MemoryAdapterKind,
    source_label: &str,
    source: &str,
    existing: &[Memory],
) -> AdapterImportPlan {
    let normalized = normalize_adapter_memories(target_namespace, kind, source_label, source);
    let bundle = build_memory_passport_bundle(target_namespace, PassportScope::All, &normalized);
    let passport_plan = plan_memory_passport_import(target_namespace, &bundle, existing);
    AdapterImportPlan {
        preview: AdapterImportPreview {
            adapter_kind: kind,
            source_label: source_label.to_string(),
            runtime_contract_id: MEMORY_RUNTIME_CONTRACT_ID.to_string(),
            normalized_count: normalized.len(),
            preview: passport_plan.preview,
        },
        staged_memories: passport_plan.staged_memories,
    }
}

pub fn render_adapter_export(
    kind: MemoryAdapterKind,
    namespace: &str,
    memories: &[Memory],
) -> Result<AdapterExportArtifact, String> {
    if !kind.supports_export() {
        return Err(format!("{kind} does not support export"));
    }

    let selected = exportable_memories(memories);
    if selected.is_empty() {
        return Err("no non-contested memories available for adapter export".to_string());
    }

    let content = match kind {
        MemoryAdapterKind::ClaudeProject => render_claude_project_export(namespace, &selected),
        MemoryAdapterKind::CursorRules => render_cursor_rules_export(namespace, &selected),
        MemoryAdapterKind::GitHistory => unreachable!("git history export is blocked above"),
    };

    Ok(AdapterExportArtifact {
        adapter_kind: kind,
        namespace: namespace.to_string(),
        exported_count: selected.len(),
        content_type: kind.content_type().to_string(),
        content,
    })
}

fn normalize_adapter_memories(
    target_namespace: &str,
    kind: MemoryAdapterKind,
    source_label: &str,
    source: &str,
) -> Vec<Memory> {
    let adapter_source_key = format!(
        "adapter:{}",
        &Memory::compute_hash(&format!("{kind}:{source_label}"))[..16]
    );
    let base_tags = [
        format!("adapter:{kind}"),
        format!("client:{}", kind.client_name()),
        format!("source_ref:{}", sanitize_tag_component(source_label)),
    ];

    let records = match kind {
        MemoryAdapterKind::ClaudeProject => normalize_markdown_records(source, false)
            .into_iter()
            .map(|content| (content, MemoryType::Semantic, Vec::<String>::new()))
            .collect(),
        MemoryAdapterKind::CursorRules => normalize_markdown_records(source, true)
            .into_iter()
            .map(|content| (content, MemoryType::Procedural, vec!["rule".to_string()]))
            .collect(),
        MemoryAdapterKind::GitHistory => normalize_git_history_records(source),
    };

    let mut seen_hashes = HashSet::new();
    let mut normalized = Vec::new();
    for (content, memory_type, extra_tags) in records {
        let mut memory = Memory::new(content);
        if !seen_hashes.insert(memory.content_hash.clone().unwrap_or_default()) {
            continue;
        }
        memory.namespace = Some(target_namespace.to_string());
        memory.memory_type = memory_type;
        memory.status = kind.imported_status();
        memory.tags = base_tags
            .iter()
            .cloned()
            .chain(extra_tags.into_iter())
            .collect();
        memory.agent = Some(format!("adapter/{kind}"));
        memory.session = Some(source_label.to_string());
        memory.source_key = Some(adapter_source_key.clone());
        normalized.push(memory);
    }
    normalized
}

fn normalize_markdown_records(source: &str, rule_bias: bool) -> Vec<String> {
    let stripped = strip_fenced_blocks(&strip_frontmatter(source));
    let mut items = Vec::new();
    let mut paragraph = Vec::new();
    let mut saw_list = false;

    for raw_line in stripped.lines() {
        let trimmed = raw_line.trim();
        if trimmed.is_empty() {
            if !paragraph.is_empty() && !saw_list {
                items.push(clean_markdown_text(&paragraph.join(" ")));
                paragraph.clear();
            }
            continue;
        }
        if trimmed.starts_with('#') {
            if !paragraph.is_empty() && !saw_list {
                items.push(clean_markdown_text(&paragraph.join(" ")));
                paragraph.clear();
            }
            continue;
        }
        if is_frontmatter_metadata_line(trimmed) {
            continue;
        }
        if let Some(item) = list_item_body(trimmed) {
            saw_list = true;
            items.push(clean_markdown_text(item));
            continue;
        }
        if rule_bias && is_rule_like(trimmed) {
            items.push(clean_markdown_text(trimmed));
            continue;
        }
        if !saw_list {
            paragraph.push(trimmed.to_string());
        }
    }
    if !paragraph.is_empty() && !saw_list {
        items.push(clean_markdown_text(&paragraph.join(" ")));
    }

    let mut deduped = Vec::new();
    let mut seen = HashSet::new();
    for item in items {
        if item.len() < 8 {
            continue;
        }
        let normalized = ensure_sentence(item);
        if is_adapter_scaffolding_line(&normalized) {
            continue;
        }
        if seen.insert(normalized.to_ascii_lowercase()) {
            deduped.push(normalized);
        }
        if deduped.len() >= MAX_MARKDOWN_ITEMS {
            break;
        }
    }
    deduped
}

fn normalize_git_history_records(source: &str) -> Vec<(String, MemoryType, Vec<String>)> {
    let mut records = Vec::new();
    let mut seen = HashSet::new();
    for raw_record in source.split(GIT_HISTORY_RECORD_SEP) {
        let record = raw_record.trim();
        if record.is_empty() {
            continue;
        }
        let mut fields = record.split(GIT_HISTORY_FIELD_SEP);
        let sha = fields.next().unwrap_or_default().trim();
        let subject = fields.next().unwrap_or_default().trim();
        let body = fields.next().unwrap_or_default().trim();
        if sha.is_empty() || subject.is_empty() {
            continue;
        }
        let content = normalize_git_subject(subject, body);
        if !seen.insert(content.to_ascii_lowercase()) {
            continue;
        }
        records.push((
            content,
            MemoryType::Episodic,
            vec![
                "git_history".to_string(),
                format!("git_commit:{}", &sha[..sha.len().min(12)]),
            ],
        ));
        if records.len() >= MAX_GIT_COMMITS {
            break;
        }
    }
    records
}

fn normalize_git_subject(subject: &str, body: &str) -> String {
    let subject = clean_markdown_text(subject);
    if let Some((prefix, rest)) = subject.split_once(':') {
        let rest = rest.trim();
        if !rest.is_empty() {
            let kind = prefix.trim().split('(').next().unwrap_or(prefix).trim();
            let scope = prefix
                .split('(')
                .nth(1)
                .and_then(|rest| rest.split(')').next())
                .map(str::trim)
                .filter(|scope| !scope.is_empty());
            let label = match kind.to_ascii_lowercase().as_str() {
                "feat" => "Git feature",
                "fix" => "Git fix",
                "revert" => "Git revert",
                "docs" => "Git docs change",
                "refactor" => "Git refactor",
                "test" => "Git test change",
                "chore" => "Git maintenance",
                _ => "Git change",
            };
            let scope_suffix = scope.map(|scope| format!(" ({scope})")).unwrap_or_default();
            return ensure_sentence(format!("{label}{scope_suffix}: {rest}"));
        }
    }

    let first_body_line = body
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("");
    if !first_body_line.is_empty() && !subject.eq_ignore_ascii_case(first_body_line) {
        return ensure_sentence(format!(
            "Git change: {} — {}",
            subject,
            clean_markdown_text(first_body_line)
        ));
    }
    ensure_sentence(format!("Git change: {subject}"))
}

fn exportable_memories(memories: &[Memory]) -> Vec<Memory> {
    let mut seen = HashSet::new();
    let mut selected: Vec<Memory> = memories
        .iter()
        .filter(|memory| !memory.archived)
        .filter(|memory| memory.status != MemoryStatus::Contested)
        .filter_map(|memory| {
            let key = memory.content.to_ascii_lowercase();
            if seen.insert(key) {
                Some(memory.clone())
            } else {
                None
            }
        })
        .collect();
    selected.sort_by(|left, right| {
        left.created_at
            .cmp(&right.created_at)
            .then_with(|| left.content.cmp(&right.content))
    });
    selected
}

fn render_claude_project_export(namespace: &str, memories: &[Memory]) -> String {
    let mut output = String::new();
    output.push_str("# memoryOSS Project Context\n\n");
    output.push_str(&format!(
        "Generated from namespace `{namespace}` via the portable runtime contract.\n\n"
    ));
    output.push_str("## Current context\n");
    for memory in memories {
        output.push_str("- ");
        output.push_str(memory.content.trim());
        output.push('\n');
    }
    output.push_str("\n## Provenance\n");
    output.push_str(
        "Each item originated from memoryOSS and carries runtime-contract provenance when re-imported.\n",
    );
    output
}

fn render_cursor_rules_export(namespace: &str, memories: &[Memory]) -> String {
    let mut output = String::new();
    output.push_str("---\n");
    output.push_str(&format!(
        "description: Generated from memoryOSS namespace {namespace}\n"
    ));
    output.push_str("globs:\n  - \"**/*\"\n");
    output.push_str("alwaysApply: false\n");
    output.push_str("---\n\n");
    output.push_str("# memoryOSS synchronized rules\n\n");
    for memory in memories {
        output.push_str("- ");
        output.push_str(memory.content.trim());
        output.push('\n');
    }
    output
}

fn strip_frontmatter(source: &str) -> String {
    let trimmed = source.trim_start();
    if !trimmed.starts_with("---\n") {
        return source.to_string();
    }
    let rest = &trimmed[4..];
    if let Some(end) = rest.find("\n---\n") {
        return rest[end + 5..].to_string();
    }
    source.to_string()
}

fn strip_fenced_blocks(source: &str) -> String {
    let mut output = String::new();
    let mut in_block = false;
    for line in source.lines() {
        if line.trim_start().starts_with("```") {
            in_block = !in_block;
            continue;
        }
        if !in_block {
            output.push_str(line);
            output.push('\n');
        }
    }
    output
}

fn is_frontmatter_metadata_line(line: &str) -> bool {
    let lowered = line.to_ascii_lowercase();
    lowered.starts_with("description:")
        || lowered.starts_with("globs:")
        || lowered.starts_with("alwaysapply:")
        || lowered.starts_with("files:")
        || lowered.starts_with("trigger:")
}

fn list_item_body(line: &str) -> Option<&str> {
    let bytes = line.as_bytes();
    if let Some(rest) = line.strip_prefix("- ").or_else(|| line.strip_prefix("* ")) {
        return Some(rest);
    }
    let mut idx = 0usize;
    while idx < bytes.len() && bytes[idx].is_ascii_digit() {
        idx += 1;
    }
    if idx > 0 && idx + 1 < bytes.len() && bytes[idx] == b'.' && bytes[idx + 1] == b' ' {
        return Some(&line[idx + 2..]);
    }
    None
}

fn clean_markdown_text(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut last_space = false;
    for ch in input.chars() {
        let mapped = match ch {
            '`' | '*' | '_' | '[' | ']' => None,
            '>' | '#' => Some(' '),
            '\t' | '\r' | '\n' => Some(' '),
            _ => Some(ch),
        };
        if let Some(ch) = mapped {
            if ch.is_whitespace() {
                if !last_space {
                    out.push(' ');
                }
                last_space = true;
            } else {
                out.push(ch);
                last_space = false;
            }
        }
    }
    out.trim().trim_matches('-').trim().to_string()
}

fn ensure_sentence(text: impl AsRef<str>) -> String {
    let trimmed = text.as_ref().trim();
    if trimmed.is_empty() {
        return String::new();
    }
    if matches!(trimmed.chars().last(), Some('.' | '!' | '?')) {
        trimmed.to_string()
    } else {
        format!("{trimmed}.")
    }
}

fn is_rule_like(text: &str) -> bool {
    let lowered = text.to_ascii_lowercase();
    [
        "must ",
        "must not",
        "never ",
        "always ",
        "do not",
        "don't ",
        "should ",
        "should not",
        "prefer ",
        "required",
    ]
    .iter()
    .any(|needle| lowered.contains(needle))
}

fn sanitize_tag_component(label: &str) -> String {
    let mut out = String::new();
    let mut last_dash = false;
    for ch in label.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    out.trim_matches('-').to_string()
}

fn is_adapter_scaffolding_line(text: &str) -> bool {
    let lowered = text.to_ascii_lowercase();
    lowered.contains("generated from namespace")
        || lowered.contains("portable runtime contract")
        || lowered.contains("originated from memoryoss")
        || lowered.contains("memoryoss project context")
        || lowered.contains("memoryoss synchronized rules")
}

#[cfg(test)]
mod tests {
    use super::{
        MemoryAdapterKind, normalize_git_history_records, normalize_markdown_records,
        plan_adapter_import,
    };

    #[test]
    fn test_cursor_rule_markdown_normalizes_frontmatter_and_bullets() {
        let source = r#"---
description: Review rules
globs:
  - "**/*"
alwaysApply: false
---

- Never merge without security review
- Prefer rg over grep
"#;
        let records = normalize_markdown_records(source, true);
        assert_eq!(records.len(), 2);
        assert_eq!(records[0], "Never merge without security review.");
        assert_eq!(records[1], "Prefer rg over grep.");
    }

    #[test]
    fn test_git_history_normalizes_conventional_commits() {
        let source = "abc123\u{1f}fix(api): reject empty tokens\u{1f}\u{1e}";
        let records = normalize_git_history_records(source);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].0, "Git fix (api): reject empty tokens.");
    }

    #[test]
    fn test_adapter_import_plan_maps_to_contract_preview() {
        let plan = plan_adapter_import(
            "test",
            MemoryAdapterKind::ClaudeProject,
            "project.md",
            "- Keep MCP-first auth as the default path\n- Prefer short summaries",
            &[],
        );
        assert_eq!(
            plan.preview.runtime_contract_id,
            "memoryoss.runtime.v1alpha1"
        );
        assert_eq!(plan.preview.normalized_count, 2);
        assert_eq!(plan.preview.preview.create_count, 2);
        assert_eq!(plan.staged_memories.len(), 2);
        assert!(
            plan.staged_memories
                .iter()
                .all(|memory| memory.status == crate::memory::MemoryStatus::Active)
        );
    }

    #[test]
    fn test_git_history_imports_stay_candidate() {
        let plan = plan_adapter_import(
            "test",
            MemoryAdapterKind::GitHistory,
            "repo",
            "abc123\u{1f}feat(api): add adapter bridge\u{1f}\u{1e}",
            &[],
        );
        assert_eq!(plan.staged_memories.len(), 1);
        assert_eq!(
            plan.staged_memories[0].status,
            crate::memory::MemoryStatus::Candidate
        );
    }
}
