// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 memoryOSS Contributors
#![allow(
    clippy::too_many_arguments,
    clippy::type_complexity,
    clippy::collapsible_if,
    clippy::collapsible_match,
    clippy::manual_async_fn
)]

mod adapters;
mod config;
mod decompose;
#[allow(dead_code)]
mod embedding;
mod engines;
mod fusion;
mod intent_cache;
mod llm_client;
mod mcp;
mod memory;
mod merger;
mod migration;
#[allow(dead_code)]
mod prefetch;
mod scoring;
#[allow(dead_code)]
mod security;
mod server;
#[allow(dead_code)]
mod sharing;
mod validation;

use clap::{Parser, Subcommand};
use std::path::{Path, PathBuf};
use std::str::FromStr;

#[derive(Parser)]
#[command(
    name = "memoryoss",
    version,
    about = "The Open Source Memory Layer for AI Agents"
)]
struct Cli {
    #[arg(short, long, default_value = "memoryoss.toml")]
    config: PathBuf,

    #[arg(short, long, action = clap::ArgAction::Count)]
    verbose: u8,

    /// Print license information and exit
    #[arg(long)]
    license: bool,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Start the HTTP server
    Serve,
    /// Start the internal memory core (managed by the hybrid gateway)
    #[command(hide = true)]
    ServeCore,
    /// Start the external gateway only (managed or tested separately)
    #[command(hide = true)]
    ServeGateway,
    /// Run database migrations
    Migrate {
        /// Show pending migrations without applying
        #[arg(long)]
        dry_run: bool,
    },
    /// Start MCP server (stdio)
    McpServer,
    /// Read and re-write canonical runtime conformance fixtures
    Conformance {
        #[command(subcommand)]
        command: ConformanceCommands,
    },
    /// Start in dev mode (mock embeddings, no TLS, relaxed auth)
    Dev,
    /// Show namespace health, lifecycle counts, worker state, and index health
    Status {
        /// Only inspect a specific namespace
        #[arg(long)]
        namespace: Option<String>,
    },
    /// Diagnose config, auth, database, and index issues
    Doctor {
        /// Repair managed client/team drift before evaluating the final state
        #[arg(long)]
        repair: bool,
    },
    /// Show recent injections, extractions, feedbacks, and consolidations
    Recent {
        /// Only inspect a specific namespace
        #[arg(long)]
        namespace: Option<String>,
        /// Maximum entries per activity group
        #[arg(long, default_value_t = server::routes::DEFAULT_RECENT_ACTIVITY_LIMIT)]
        limit: usize,
    },
    /// Open the universal memory HUD for operator loops
    Hud {
        /// Only inspect a specific namespace
        #[arg(long)]
        namespace: Option<String>,
        /// Maximum items per HUD section
        #[arg(long, default_value_t = server::routes::DEFAULT_HUD_LIMIT)]
        limit: usize,
    },
    /// Review candidate, contested, and rejected memories without raw UUIDs
    Review {
        #[command(subcommand)]
        command: ReviewCommands,
    },
    /// Export or import portable memory passport bundles
    Passport {
        #[command(subcommand)]
        command: PassportCommands,
    },
    /// Normalize or export cross-app memory adapter artifacts
    Adapter {
        #[command(subcommand)]
        command: AdapterCommands,
    },
    /// Inspect or ingest ambient connector signals
    Connector {
        #[command(subcommand)]
        command: ConnectorCommands,
    },
    /// Inspect, export, replay, or branch memory history
    History {
        #[command(subcommand)]
        command: HistoryCommands,
    },
    /// Export, preview, diff, or validate portable memory bundle envelopes
    Bundle {
        #[command(subcommand)]
        command: BundleCommands,
    },
    /// Open and diff portable memory artifacts in a read-only universal reader
    Reader {
        #[command(subcommand)]
        command: ReaderCommands,
    },
    /// Inspect a memory by ID
    Inspect {
        /// Memory UUID
        id: String,
    },
    /// Create an encrypted backup of the data directory
    Backup {
        /// Output path for backup file (default: memoryoss-backup-{timestamp}.tar.zst)
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// Include the local data-encryption key in the backup archive.
        /// By default this is excluded so a leaked backup cannot decrypt stored memories.
        #[arg(long)]
        include_key: bool,
    },
    /// Restore from an encrypted backup
    Restore {
        /// Path to backup file
        path: PathBuf,
        /// Force overwrite existing data
        #[arg(long)]
        force: bool,
    },
    /// Run decay policy: archive old, untouched memories
    Decay {
        /// Show what would be archived without making changes
        #[arg(long)]
        dry_run: bool,
        /// Override after_days from config
        #[arg(long)]
        after_days: Option<u64>,
        /// Only process a specific namespace
        #[arg(long)]
        namespace: Option<String>,
    },
    /// Interactive setup wizard — generates config and starts the server
    Setup {
        /// Install profile: auto, claude, codex, cursor, or team-node
        #[arg(long, value_enum, default_value_t = crate::config::SetupProfile::Auto)]
        profile: crate::config::SetupProfile,
        /// Optional team bootstrap manifest that seeds shared trust/catalog defaults
        #[arg(long)]
        team_manifest: Option<PathBuf>,
    },
    /// Re-embed all memories with a new model
    MigrateEmbeddings {
        /// Target model (e.g. "all-minilm-l6-v2", "bge-base-en-v1.5")
        #[arg(long, value_enum, default_value_t = crate::config::EmbeddingModelId::AllMiniLML6V2)]
        model: crate::config::EmbeddingModelId,
        /// Batch size for embedding (default 32)
        #[arg(long, default_value = "32")]
        batch_size: usize,
        /// Only process a specific namespace (default: all from config)
        #[arg(long)]
        namespace: Option<String>,
        /// Show progress without making changes
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Subcommand)]
enum ReviewCommands {
    /// List the current review inbox
    Queue {
        /// Only inspect a specific namespace
        #[arg(long)]
        namespace: Option<String>,
        /// Maximum entries per namespace
        #[arg(long, default_value_t = server::routes::DEFAULT_REVIEW_QUEUE_LIMIT)]
        limit: usize,
    },
    /// Confirm a queue item by its inbox position
    Confirm {
        /// Namespace to inspect
        #[arg(long)]
        namespace: String,
        /// 1-based queue position from `memoryoss review queue`
        #[arg(long)]
        item: usize,
    },
    /// Reject a queue item by its inbox position
    Reject {
        /// Namespace to inspect
        #[arg(long)]
        namespace: String,
        /// 1-based queue position from `memoryoss review queue`
        #[arg(long)]
        item: usize,
    },
    /// Supersede a queue item with another queue item
    Supersede {
        /// Namespace to inspect
        #[arg(long)]
        namespace: String,
        /// 1-based queue position to supersede
        #[arg(long)]
        item: usize,
        /// 1-based queue position that should replace the target
        #[arg(long = "with-item")]
        with_item: usize,
    },
}

#[derive(Subcommand)]
enum PassportCommands {
    /// Export a selective portable memory passport bundle to disk
    Export {
        /// Namespace to export
        #[arg(long)]
        namespace: Option<String>,
        /// Passport scope: all, personal, project, or team
        #[arg(long, default_value = "project")]
        scope: String,
        /// Output path for bundle JSON
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// Import a portable memory passport bundle from disk
    Import {
        /// Path to bundle JSON
        path: PathBuf,
        /// Override target namespace
        #[arg(long)]
        namespace: Option<String>,
        /// Preview changes without applying them
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Subcommand)]
enum AdapterCommands {
    /// Import a foreign client artifact through the runtime contract
    Import {
        /// Adapter kind: claude_project, cursor_rules, or git_history
        #[arg(long)]
        kind: String,
        /// Path to the foreign artifact or repository
        path: PathBuf,
        /// Override target namespace
        #[arg(long)]
        namespace: Option<String>,
        /// Preview merge/conflict decisions without applying them
        #[arg(long)]
        dry_run: bool,
    },
    /// Export current runtime memories into a foreign client artifact
    Export {
        /// Adapter kind: claude_project or cursor_rules
        #[arg(long)]
        kind: String,
        /// Namespace to export
        #[arg(long)]
        namespace: Option<String>,
        /// Output path for the generated artifact
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
enum ConnectorCommands {
    /// List supported ambient connector kinds and privacy defaults
    List,
    /// Preview or store one ambient connector candidate locally
    Ingest {
        /// Connector kind: editor, terminal, browser, docs, ticket, calendar, pull_request, incident
        #[arg(long)]
        kind: String,
        /// Namespace to ingest into
        #[arg(long)]
        namespace: Option<String>,
        /// Summary of the captured signal
        #[arg(long)]
        summary: String,
        /// Evidence fragments captured from the connector
        #[arg(long = "evidence")]
        evidence: Vec<String>,
        /// Extra classification tags to preserve with the candidate
        #[arg(long = "tag")]
        tags: Vec<String>,
        /// Optional connector-local reference such as a file path or URL slug
        #[arg(long)]
        source_ref: Option<String>,
        /// Keep raw evidence instead of applying redaction defaults
        #[arg(long)]
        allow_raw: bool,
        /// Preview the candidate without writing it to disk
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Subcommand)]
enum HistoryCommands {
    /// Show the lineage, transitions, and review chain for one memory
    Show {
        /// Root memory UUID
        id: String,
        /// Namespace to inspect
        #[arg(long)]
        namespace: String,
    },
    /// Export a deterministic history replay bundle to disk
    Export {
        /// Root memory UUID
        id: String,
        /// Namespace to inspect
        #[arg(long)]
        namespace: String,
        /// Output path for bundle JSON
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// Replay a history bundle into an empty target namespace
    Replay {
        /// Path to bundle JSON
        path: PathBuf,
        /// Override target namespace
        #[arg(long)]
        namespace: Option<String>,
        /// Preview replay safety without applying it
        #[arg(long)]
        dry_run: bool,
    },
    /// Branch one memory lineage into a new empty target namespace
    Branch {
        /// Root memory UUID
        id: String,
        /// Source namespace to branch from
        #[arg(long)]
        namespace: String,
        /// Target namespace for the new branch
        #[arg(long = "target-namespace")]
        target_namespace: String,
        /// Preview replay safety without applying it
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Subcommand)]
enum BundleCommands {
    /// Export a portable memory bundle envelope to disk
    Export {
        /// Bundle kind: passport or history
        #[arg(long, default_value = "passport")]
        kind: String,
        /// Namespace to export
        #[arg(long)]
        namespace: Option<String>,
        /// Passport scope when exporting passport bundles
        #[arg(long, default_value = "project")]
        scope: String,
        /// Root memory UUID when exporting a history bundle
        #[arg(long)]
        id: Option<String>,
        /// Output path for bundle JSON
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// Preview a portable memory bundle without importing it
    Preview {
        /// Path to bundle JSON
        path: PathBuf,
    },
    /// Validate a portable memory bundle envelope and nested payload
    Validate {
        /// Path to bundle JSON
        path: PathBuf,
    },
    /// Diff two portable memory bundle envelopes without importing either one
    Diff {
        /// Left-hand bundle JSON
        left: PathBuf,
        /// Right-hand bundle JSON
        right: PathBuf,
    },
}

#[derive(Subcommand)]
enum ReaderCommands {
    /// Open a portable memory artifact read-only
    Open {
        /// Path to a memory bundle envelope or raw passport/history artifact
        path: PathBuf,
        /// Output format: text, json, or html
        #[arg(long, default_value = "text")]
        format: String,
    },
    /// Diff two portable memory artifacts read-only
    Diff {
        /// Left-hand artifact path
        left: PathBuf,
        /// Right-hand artifact path
        right: PathBuf,
        /// Output format: text, json, or html
        #[arg(long, default_value = "text")]
        format: String,
    },
}

#[derive(Subcommand)]
enum ConformanceCommands {
    /// Read and re-write a canonical runtime artifact
    Normalize {
        /// Artifact kind: runtime_contract, passport, or history
        #[arg(long)]
        kind: String,
        /// Input fixture JSON path
        #[arg(long)]
        input: PathBuf,
        /// Output JSON path
        #[arg(long)]
        output: PathBuf,
    },
}

#[derive(Debug, Clone, Copy)]
enum ConformanceArtifactKind {
    RuntimeContract,
    Passport,
    History,
}

impl FromStr for ConformanceArtifactKind {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "runtime_contract" => Ok(Self::RuntimeContract),
            "passport" => Ok(Self::Passport),
            "history" => Ok(Self::History),
            other => Err(format!("unsupported conformance artifact kind: {other}")),
        }
    }
}

fn append_backup_tree<W: std::io::Write>(
    tar: &mut tar::Builder<W>,
    source: &Path,
    archive_path: &Path,
    include_key: bool,
) -> anyhow::Result<()> {
    let metadata = std::fs::symlink_metadata(source)?;
    if metadata.file_type().is_symlink() {
        return Ok(());
    }

    if metadata.is_dir() {
        tar.append_dir(archive_path, source)?;
        for entry in std::fs::read_dir(source)? {
            let entry = entry?;
            let path = entry.path();
            let archive_child = archive_path.join(entry.file_name());
            append_backup_tree(tar, &path, &archive_child, include_key)?;
        }
        return Ok(());
    }

    if !include_key && source.file_name().and_then(|name| name.to_str()) == Some("memoryoss.key") {
        return Ok(());
    }

    tar.append_path_with_name(source, archive_path)?;
    Ok(())
}

fn prompt_line(prompt: &str) -> String {
    use std::io::Write;
    print!("{prompt}");
    let _ = std::io::stdout().flush();
    let mut buf = String::new();
    if std::io::stdin().read_line(&mut buf).is_err() {
        return String::new();
    }
    buf.trim().to_string()
}

fn prompt_choice(prompt: &str, options: &[&str], default: usize) -> usize {
    loop {
        let input = prompt_line(prompt);
        if input.is_empty() {
            return default;
        }
        if let Ok(n) = input.parse::<usize>()
            && n > 0
            && n <= options.len()
        {
            return n - 1;
        }
        println!("  Please enter 1-{}.", options.len());
    }
}

fn shell_config_has_var(home_dir: &str, var_name: &str) -> bool {
    [".bashrc", ".bash_profile", ".profile", ".zshrc"]
        .iter()
        .any(|name| {
            let path = std::path::Path::new(home_dir).join(name);
            let Ok(text) = std::fs::read_to_string(path) else {
                return false;
            };
            text.lines().any(|line| {
                let trimmed = line.trim();
                if trimmed.is_empty() || trimmed.starts_with('#') {
                    return false;
                }
                let body = trimmed
                    .strip_prefix("export ")
                    .map(str::trim_start)
                    .unwrap_or(trimmed);
                let Some(rest) = body.strip_prefix(var_name) else {
                    return false;
                };
                let Some(value) = rest.strip_prefix('=') else {
                    return false;
                };
                let value = value.trim();
                !(value.is_empty() || value == "\"\"" || value == "''")
            })
        })
}

const CLAUDE_HOOK_EVENTS: [&str; 5] = [
    "PreToolUse",
    "SessionStart",
    "Stop",
    "SubagentStop",
    "UserPromptSubmit",
];

const MEMORYOSS_POLICY_BEGIN: &str = "<!-- MEMORYOSS_POLICY_BEGIN -->";
const MEMORYOSS_POLICY_END: &str = "<!-- MEMORYOSS_POLICY_END -->";
const MEMORYOSS_POLICY_BLOCK: &str = r#"<!-- MEMORYOSS_POLICY_BEGIN -->
## memoryOSS Mandatory
- Call `memoryoss_recall` at session start and before substantial work.
- Call `memoryoss_store` or `memoryoss_update` before stopping after important confirmed learning.
- Do not start non-memoryOSS tool work before recall.
- If memoryOSS is unavailable or unconfigured, stop and repair that first.
<!-- MEMORYOSS_POLICY_END -->"#;
const MEMORYOSS_CURSOR_RULE_BEGIN: &str = "<!-- MEMORYOSS_CURSOR_RULE_BEGIN -->";
const MEMORYOSS_CURSOR_RULE_END: &str = "<!-- MEMORYOSS_CURSOR_RULE_END -->";
const MEMORYOSS_CURSOR_RULE: &str = r#"---
description: memoryOSS runtime discipline
globs:
  - "**/*"
alwaysApply: true
---

<!-- MEMORYOSS_CURSOR_RULE_BEGIN -->
# memoryOSS runtime discipline

- Call `memoryoss_recall` at session start and before substantial work.
- Call `memoryoss_store` or `memoryoss_update` before finishing after important confirmed learning.
- If memoryOSS is unavailable or unconfigured, stop and repair the MCP config before continuing.
<!-- MEMORYOSS_CURSOR_RULE_END -->
"#;

const CLAUDE_MEMORYOSS_GUARD: &str = r#"#!/usr/bin/env python3
"""Enforce memoryOSS recall/store discipline for Claude project sessions."""

from __future__ import annotations

import json
import os
import sys
from pathlib import Path

MEMORY_RECALL_MARKERS = (
    "memoryoss_recall",
    "mcp__memoryoss__memoryoss_recall",
)
MEMORY_STORE_MARKERS = (
    "memoryoss_store",
    "mcp__memoryoss__memoryoss_store",
    "memoryoss_update",
    "mcp__memoryoss__memoryoss_update",
)
MEMORY_TOOL_MARKERS = (
    "memoryoss",
    "mcp__memoryoss__",
)


def _read_stdin() -> dict:
    try:
        return json.load(sys.stdin)
    except Exception:
        return {}


def _read_transcript(path_value: str | None) -> str:
    if not path_value:
        return ""
    try:
        return Path(path_value).read_text(encoding="utf-8")
    except Exception:
        return ""


def _iter_transcript_tool_names(transcript: str) -> list[str]:
    names: list[str] = []
    for line in transcript.splitlines():
        stripped = line.strip()
        if not stripped:
            continue
        try:
            payload = json.loads(stripped)
        except Exception:
            continue
        if payload.get("type") != "assistant":
            continue
        message = payload.get("message", {})
        if not isinstance(message, dict):
            continue
        content = message.get("content", [])
        if not isinstance(content, list):
            continue
        for item in content:
            if not isinstance(item, dict):
                continue
            if item.get("type") != "tool_use":
                continue
            name = str(item.get("name", "") or "")
            if name:
                names.append(name)
    return names


def _contains_any(haystack: str, needles: tuple[str, ...]) -> bool:
    lower = haystack.lower()
    return any(needle.lower() in lower for needle in needles)


def _tool_name(payload: dict) -> str:
    return str(payload.get("tool_name", "") or "")


def _is_memory_tool(payload: dict) -> bool:
    tool_name = _tool_name(payload).lower()
    if _contains_any(tool_name, MEMORY_TOOL_MARKERS):
        return True
    tool_input = json.dumps(payload.get("tool_input", {}), sort_keys=True).lower()
    return _contains_any(tool_input, MEMORY_TOOL_MARKERS)


def _has_recall(transcript: str) -> bool:
    for name in _iter_transcript_tool_names(transcript):
        if _contains_any(name, MEMORY_RECALL_MARKERS):
            return True
    return _contains_any(transcript, MEMORY_RECALL_MARKERS)


def _has_store(transcript: str) -> bool:
    for name in _iter_transcript_tool_names(transcript):
        if _contains_any(name, MEMORY_STORE_MARKERS):
            return True
    return _contains_any(transcript, MEMORY_STORE_MARKERS)


def _has_non_memory_tool_use(transcript: str) -> bool:
    tool_names = _iter_transcript_tool_names(transcript)
    if tool_names:
        return any(not _contains_any(name, MEMORY_TOOL_MARKERS) for name in tool_names)
    lower = transcript.lower()
    if '"tool_name"' not in lower and "<function_calls>" not in lower:
        return False
    if "memoryoss" not in lower:
        return True
    segments = [segment for segment in lower.splitlines() if "tool_name" in segment]
    return any("memoryoss" not in segment for segment in segments)


def _allow(message: str | None = None) -> dict:
    payload: dict[str, object] = {"continue": True}
    if message:
        payload["systemMessage"] = message
    return payload


def _deny(event_name: str, message: str) -> dict:
    if event_name in {"PreToolUse", "PostToolUse"}:
        return {
            "continue": True,
            "hookSpecificOutput": {
                "hookEventName": event_name,
                "permissionDecision": "deny",
            },
            "systemMessage": message,
        }
    return {
        "continue": False,
        "decision": "block",
        "reason": message,
        "systemMessage": message,
    }


def main() -> int:
    payload = _read_stdin()
    event_name = str(payload.get("hook_event_name", "") or "")
    transcript = _read_transcript(payload.get("transcript_path"))
    project_dir = os.environ.get("CLAUDE_PROJECT_DIR", "")
    policy_note = (
        "memoryOSS is mandatory in this project. Call `memoryoss_recall` at session start "
        "and before substantial work. Call `memoryoss_store` or `memoryoss_update` before stopping "
        "when you learned or confirmed something important."
    )

    if event_name == "SessionStart":
        print(json.dumps(_allow(policy_note)))
        return 0

    if event_name == "UserPromptSubmit":
        print(json.dumps(_allow(policy_note)))
        return 0

    if event_name == "PreToolUse":
        if _is_memory_tool(payload):
            print(json.dumps(_allow()))
            return 0
        if _has_recall(transcript):
            print(json.dumps(_allow()))
            return 0
        message = (
            f"{policy_note} Current project: {project_dir or 'unknown'}. "
            "This tool call is blocked until `memoryoss_recall` runs."
        )
        print(json.dumps(_deny(event_name, message)))
        return 0

    if event_name in {"Stop", "SubagentStop"}:
        if not _has_non_memory_tool_use(transcript):
            print(json.dumps(_allow()))
            return 0
        if _has_store(transcript):
            print(json.dumps(_allow()))
            return 0
        message = (
            f"{policy_note} Current project: {project_dir or 'unknown'}. "
            "This session used tools but has no `memoryoss_store`/`memoryoss_update` yet."
        )
        print(json.dumps(_deny(event_name, message)))
        return 0

    print(json.dumps(_allow()))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
"#;

fn command_exists(name: &str) -> bool {
    std::process::Command::new("which")
        .arg(name)
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

fn home_dir_path() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
}

fn claude_dir(home_dir: &std::path::Path) -> PathBuf {
    home_dir.join(".claude")
}

fn claude_user_config_path(home_dir: &std::path::Path) -> PathBuf {
    home_dir.join(".claude.json")
}

fn claude_settings_path(home_dir: &std::path::Path) -> PathBuf {
    claude_dir(home_dir).join("settings.json")
}

fn claude_settings_local_path(home_dir: &std::path::Path) -> PathBuf {
    claude_dir(home_dir).join("settings.local.json")
}

fn claude_guard_script_path(home_dir: &std::path::Path) -> PathBuf {
    claude_dir(home_dir).join("memoryoss-guard.py")
}

fn codex_home_dir(home_dir: &std::path::Path) -> PathBuf {
    std::env::var("CODEX_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| home_dir.join(".codex"))
}

fn codex_config_path(home_dir: &std::path::Path) -> PathBuf {
    codex_home_dir(home_dir).join("config.toml")
}

fn cursor_dir(home_dir: &std::path::Path) -> PathBuf {
    home_dir.join(".cursor")
}

fn cursor_mcp_path(home_dir: &std::path::Path) -> PathBuf {
    cursor_dir(home_dir).join("mcp.json")
}

fn cursor_rules_dir(home_dir: &std::path::Path) -> PathBuf {
    cursor_dir(home_dir).join("rules")
}

fn cursor_rule_path(home_dir: &std::path::Path) -> PathBuf {
    cursor_rules_dir(home_dir).join("memoryoss.mdc")
}

fn agents_policy_path(home_dir: &std::path::Path) -> PathBuf {
    home_dir.join("AGENTS.md")
}

#[derive(Debug, Clone, serde::Deserialize)]
struct TeamBootstrapManifest {
    team_id: String,
    team_label: String,
    catalog: crate::security::trust::PortableTrustCatalog,
}

#[derive(Debug, Clone, serde::Serialize)]
struct TeamBootstrapReceipt {
    team_id: String,
    team_label: String,
    catalog_id: String,
    manifest_path: String,
    applied_at: chrono::DateTime<chrono::Utc>,
    profile: String,
    configured_clients: Vec<String>,
}

fn team_bootstrap_receipt_path(home_dir: &std::path::Path) -> PathBuf {
    home_dir.join(".memoryoss").join("team-bootstrap.json")
}

fn read_team_bootstrap_manifest(path: &std::path::Path) -> anyhow::Result<TeamBootstrapManifest> {
    let bytes = std::fs::read(path)?;
    let manifest: TeamBootstrapManifest = serde_json::from_slice(&bytes)?;
    if manifest.team_id.trim().is_empty() || manifest.team_label.trim().is_empty() {
        anyhow::bail!("team manifest must include non-empty team_id and team_label");
    }
    Ok(manifest)
}

fn apply_team_bootstrap_manifest(
    manifest: &TeamBootstrapManifest,
    manifest_path: &std::path::Path,
    config: &crate::config::Config,
    home_dir: &std::path::Path,
    profile: crate::config::SetupProfile,
    configured_clients: &[&str],
) -> anyhow::Result<crate::security::trust::PortableTrustImportSummary> {
    let registry = crate::security::trust::PortableTrustRegistry::open(&config.storage.data_dir)?;
    let summary = registry.import_catalog(manifest.catalog.clone())?;
    let receipt = TeamBootstrapReceipt {
        team_id: manifest.team_id.clone(),
        team_label: manifest.team_label.clone(),
        catalog_id: manifest.catalog.catalog_id.clone(),
        manifest_path: manifest_path.display().to_string(),
        applied_at: chrono::Utc::now(),
        profile: profile.to_string(),
        configured_clients: configured_clients
            .iter()
            .map(|entry| entry.to_string())
            .collect(),
    };
    write_json_file(
        &team_bootstrap_receipt_path(home_dir),
        &serde_json::to_value(&receipt)?,
    )?;
    Ok(summary)
}

fn read_json_file(path: &std::path::Path) -> serde_json::Value {
    match std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
    {
        Some(value) if value.is_object() => value,
        _ => serde_json::json!({}),
    }
}

fn write_json_file(path: &std::path::Path, value: &serde_json::Value) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, serde_json::to_string_pretty(value)?)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

fn write_text_file(path: &std::path::Path, contents: &str, mode: u32) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, contents)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode));
    }
    Ok(())
}

fn memoryoss_command_args(config_path: &std::path::Path) -> Vec<String> {
    vec![
        "-c".to_string(),
        config_path.to_string_lossy().to_string(),
        "mcp-server".to_string(),
    ]
}

fn claude_user_mcp_value(
    binary: &std::path::Path,
    config_path: &std::path::Path,
) -> serde_json::Value {
    serde_json::json!({
        "type": "stdio",
        "command": binary.to_string_lossy().to_string(),
        "args": memoryoss_command_args(config_path),
        "env": {}
    })
}

fn claude_legacy_mcp_value(
    binary: &std::path::Path,
    config_path: &std::path::Path,
) -> serde_json::Value {
    serde_json::json!({
        "command": binary.to_string_lossy().to_string(),
        "args": memoryoss_command_args(config_path),
    })
}

fn claude_hook_command(script_path: &std::path::Path) -> String {
    format!("python3 {}", script_path.display())
}

fn claude_hook_entry(command: &str) -> serde_json::Value {
    serde_json::json!([
        {
            "matcher": "*",
            "hooks": [
                {
                    "type": "command",
                    "command": command,
                    "timeout": 10
                }
            ]
        }
    ])
}

fn is_claude_installed(home_dir: &std::path::Path) -> bool {
    home_dir.join(".claude").exists()
        || home_dir.join(".claude.json").exists()
        || command_exists("claude")
}

fn is_codex_installed(home_dir: &std::path::Path) -> bool {
    codex_home_dir(home_dir).exists() || command_exists("codex")
}

fn is_cursor_installed(home_dir: &std::path::Path) -> bool {
    cursor_dir(home_dir).exists() || command_exists("cursor") || command_exists("cursor-agent")
}

fn cursor_mcp_matches(
    home_dir: &std::path::Path,
    binary: &std::path::Path,
    config: &std::path::Path,
) -> bool {
    let value = read_json_file(&cursor_mcp_path(home_dir));
    value
        .get("mcpServers")
        .and_then(|entry| entry.get("memoryoss"))
        .map(|entry| json_mcp_matches(entry, binary, config))
        == Some(true)
}

fn cursor_rules_match(home_dir: &std::path::Path) -> bool {
    let Ok(text) = std::fs::read_to_string(cursor_rule_path(home_dir)) else {
        return false;
    };
    text.contains(MEMORYOSS_CURSOR_RULE_BEGIN)
        && text.contains(MEMORYOSS_CURSOR_RULE_END)
        && text.contains("memoryoss_recall")
        && text.contains("memoryoss_store")
}

fn json_mcp_matches(
    value: &serde_json::Value,
    binary: &std::path::Path,
    config: &std::path::Path,
) -> bool {
    let command = value.get("command").and_then(|entry| entry.as_str());
    let args = value.get("args").and_then(|entry| entry.as_array());
    let expected_args = memoryoss_command_args(config);
    command == Some(binary.to_string_lossy().as_ref())
        && args.map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str())
                .map(str::to_string)
                .collect::<Vec<_>>()
                == expected_args
        }) == Some(true)
}

fn claude_user_mcp_matches(
    home_dir: &std::path::Path,
    binary: &std::path::Path,
    config: &std::path::Path,
) -> bool {
    let value = read_json_file(&claude_user_config_path(home_dir));
    value
        .get("mcpServers")
        .and_then(|entry| entry.get("memoryoss"))
        .map(|entry| json_mcp_matches(entry, binary, config))
        == Some(true)
}

fn claude_legacy_mcp_matches(
    home_dir: &std::path::Path,
    binary: &std::path::Path,
    config: &std::path::Path,
) -> bool {
    let value = read_json_file(&claude_settings_path(home_dir));
    value
        .get("mcpServers")
        .and_then(|entry| entry.get("memoryoss"))
        .map(|entry| json_mcp_matches(entry, binary, config))
        == Some(true)
}

fn claude_hooks_match(home_dir: &std::path::Path) -> bool {
    let script_path = claude_guard_script_path(home_dir);
    if !script_path.is_file() {
        return false;
    }
    let expected_command = claude_hook_command(&script_path);
    let value = read_json_file(&claude_settings_local_path(home_dir));
    let Some(hooks) = value.get("hooks") else {
        return false;
    };
    CLAUDE_HOOK_EVENTS.iter().all(|event| {
        hooks
            .get(*event)
            .and_then(|entries| entries.as_array())
            .and_then(|entries| entries.first())
            .and_then(|entry| entry.get("hooks"))
            .and_then(|entries| entries.as_array())
            .and_then(|entries| entries.first())
            .map(|hook| {
                hook.get("type").and_then(|entry| entry.as_str()) == Some("command")
                    && hook.get("command").and_then(|entry| entry.as_str())
                        == Some(expected_command.as_str())
            })
            == Some(true)
    })
}

fn codex_mcp_matches(
    home_dir: &std::path::Path,
    binary: &std::path::Path,
    config: &std::path::Path,
) -> bool {
    let Ok(text) = std::fs::read_to_string(codex_config_path(home_dir)) else {
        return false;
    };
    let Ok(value) = text.parse::<toml::Value>() else {
        return false;
    };
    let Some(entry) = value
        .get("mcp_servers")
        .and_then(|table| table.get("memoryoss"))
    else {
        return false;
    };
    let command = entry.get("command").and_then(|item| item.as_str());
    let args = entry.get("args").and_then(|item| item.as_array());
    let expected_args = memoryoss_command_args(config);
    command == Some(binary.to_string_lossy().as_ref())
        && args.map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str())
                .map(str::to_string)
                .collect::<Vec<_>>()
                == expected_args
        }) == Some(true)
}

fn codex_policy_matches(home_dir: &std::path::Path) -> bool {
    let Ok(text) = std::fs::read_to_string(agents_policy_path(home_dir)) else {
        return false;
    };
    text.contains(MEMORYOSS_POLICY_BEGIN)
        && text.contains(MEMORYOSS_POLICY_END)
        && text.contains("memoryoss_recall")
        && text.contains("memoryoss_store")
}

fn upsert_policy_block(existing: &str) -> String {
    let trimmed = existing.trim_end();
    let mut without_block = Vec::new();
    let mut in_block = false;
    for line in trimmed.lines() {
        let current = line.trim();
        if current == MEMORYOSS_POLICY_BEGIN {
            in_block = true;
            continue;
        }
        if current == MEMORYOSS_POLICY_END {
            in_block = false;
            continue;
        }
        if !in_block {
            without_block.push(line);
        }
    }
    let mut rendered = without_block.join("\n").trim_end().to_string();
    if !rendered.is_empty() {
        rendered.push_str("\n\n");
    }
    rendered.push_str(MEMORYOSS_POLICY_BLOCK);
    rendered.push('\n');
    rendered
}

fn configure_claude_integration(
    home_dir: &std::path::Path,
    binary: &std::path::Path,
    config_path: &std::path::Path,
    bind_host: &str,
    port: &str,
) -> anyhow::Result<()> {
    let user_config_path = claude_user_config_path(home_dir);
    let settings_path = claude_settings_path(home_dir);
    let settings_local_path = claude_settings_local_path(home_dir);
    let guard_script_path = claude_guard_script_path(home_dir);

    let mut user_config = read_json_file(&user_config_path);
    user_config["mcpServers"]["memoryoss"] = claude_user_mcp_value(binary, config_path);
    write_json_file(&user_config_path, &user_config)?;

    write_text_file(&guard_script_path, CLAUDE_MEMORYOSS_GUARD, 0o755)?;

    let mut settings = read_json_file(&settings_path);
    settings["mcpServers"]["memoryoss"] = claude_legacy_mcp_value(binary, config_path);
    settings["statusLine"] = serde_json::json!({
        "type": "command",
        "command": format!("bash {}", claude_dir(home_dir).join("statusline-command.sh").display()),
    });

    let script_path = claude_dir(home_dir).join("statusline-command.sh");
    let health_url = format!("http://{}:{}/health", bind_host, port);
    let script = format!(
        r#"#!/usr/bin/env bash
# Claude Code status line — memoryOSS health indicator
input=$(cat)
cwd=$(echo "$input" | jq -r '.workspace.current_dir // .cwd // empty')
model=$(echo "$input" | jq -r '.model.display_name // empty')
used_pct=$(echo "$input" | jq -r '.context_window.used_percentage // empty')
dir_segment=""
[ -n "$cwd" ] && dir_segment="$(basename "$cwd")"
model_segment=""
[ -n "$model" ] && model_segment="$model"
ctx_segment=""
[ -n "$used_pct" ] && ctx_segment="ctx:${{used_pct}}%"
MEMORY_STATUS=""
response=$(curl -sf --max-time 1 {health_url} 2>/dev/null)
if [ $? -eq 0 ] && [ -n "$response" ]; then
  status=$(echo "$response" | jq -r '.status // empty' 2>/dev/null)
  if [ "$status" = "ok" ]; then
    MEMORY_STATUS=$(printf '\033[32mmemoryOSS ●\033[0m')
  else
    MEMORY_STATUS=$(printf '\033[31mmemoryOSS ●\033[0m')
  fi
else
  MEMORY_STATUS=$(printf '\033[31mmemoryOSS ●\033[0m')
fi
parts=()
[ -n "$dir_segment" ] && parts+=("$dir_segment")
[ -n "$model_segment" ] && parts+=("$model_segment")
[ -n "$ctx_segment" ] && parts+=("$ctx_segment")
parts+=("$MEMORY_STATUS")
printf '%s' "$(IFS=' | '; echo "${{parts[*]}}")"
"#
    );
    write_text_file(&script_path, &script, 0o755)?;
    write_json_file(&settings_path, &settings)?;

    let hook_command = claude_hook_command(&guard_script_path);
    let mut settings_local = read_json_file(&settings_local_path);
    for event in CLAUDE_HOOK_EVENTS {
        settings_local["hooks"][event] = claude_hook_entry(&hook_command);
    }
    write_json_file(&settings_local_path, &settings_local)?;
    Ok(())
}

fn configure_codex_integration(
    home_dir: &std::path::Path,
    binary: &std::path::Path,
    config_path: &std::path::Path,
) -> anyhow::Result<()> {
    let config_file = codex_config_path(home_dir);
    if let Some(parent) = config_file.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut table = std::fs::read_to_string(&config_file)
        .ok()
        .and_then(|text| text.parse::<toml::Value>().ok())
        .and_then(|value| value.as_table().cloned())
        .unwrap_or_default();

    let mut mcp_servers = table
        .remove("mcp_servers")
        .and_then(|value| value.as_table().cloned())
        .unwrap_or_default();
    let mut memoryoss = toml::Table::new();
    memoryoss.insert(
        "command".to_string(),
        toml::Value::String(binary.to_string_lossy().to_string()),
    );
    memoryoss.insert(
        "args".to_string(),
        toml::Value::Array(
            memoryoss_command_args(config_path)
                .into_iter()
                .map(toml::Value::String)
                .collect(),
        ),
    );
    mcp_servers.insert("memoryoss".to_string(), toml::Value::Table(memoryoss));
    table.insert("mcp_servers".to_string(), toml::Value::Table(mcp_servers));

    std::fs::write(
        &config_file,
        toml::to_string_pretty(&toml::Value::Table(table))?,
    )?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&config_file, std::fs::Permissions::from_mode(0o600));
    }

    let agents_path = agents_policy_path(home_dir);
    let existing = std::fs::read_to_string(&agents_path).unwrap_or_default();
    write_text_file(&agents_path, &upsert_policy_block(&existing), 0o644)?;
    Ok(())
}

fn configure_cursor_integration(
    home_dir: &std::path::Path,
    binary: &std::path::Path,
    config_path: &std::path::Path,
) -> anyhow::Result<()> {
    let mcp_path = cursor_mcp_path(home_dir);
    let mut config = read_json_file(&mcp_path);
    config["mcpServers"]["memoryoss"] = claude_user_mcp_value(binary, config_path);
    write_json_file(&mcp_path, &config)?;
    write_text_file(&cursor_rule_path(home_dir), MEMORYOSS_CURSOR_RULE, 0o644)?;
    Ok(())
}

fn remove_cursor_integration(home_dir: &std::path::Path) -> anyhow::Result<()> {
    let mcp_path = cursor_mcp_path(home_dir);
    if mcp_path.exists() {
        let mut config = read_json_file(&mcp_path);
        if let Some(mcp_servers) = config
            .get_mut("mcpServers")
            .and_then(serde_json::Value::as_object_mut)
        {
            mcp_servers.remove("memoryoss");
        }
        write_json_file(&mcp_path, &config)?;
    }
    let rule_path = cursor_rule_path(home_dir);
    if rule_path.exists() {
        std::fs::remove_file(&rule_path)?;
    }
    Ok(())
}

fn profile_configures_claude(profile: crate::config::SetupProfile, detected_claude: bool) -> bool {
    matches!(profile, crate::config::SetupProfile::Claude)
        || matches!(profile, crate::config::SetupProfile::TeamNode) && detected_claude
        || matches!(profile, crate::config::SetupProfile::Auto) && detected_claude
}

fn profile_configures_codex(profile: crate::config::SetupProfile, detected_codex: bool) -> bool {
    matches!(profile, crate::config::SetupProfile::Codex)
        || matches!(profile, crate::config::SetupProfile::TeamNode) && detected_codex
        || matches!(profile, crate::config::SetupProfile::Auto) && detected_codex
}

fn profile_configures_cursor(profile: crate::config::SetupProfile, detected_cursor: bool) -> bool {
    matches!(profile, crate::config::SetupProfile::Cursor)
        || matches!(profile, crate::config::SetupProfile::TeamNode) && detected_cursor
        || matches!(profile, crate::config::SetupProfile::Auto) && detected_cursor
}

fn profile_enables_shell_proxy_exports(profile: crate::config::SetupProfile) -> bool {
    !matches!(
        profile,
        crate::config::SetupProfile::Cursor | crate::config::SetupProfile::TeamNode
    )
}

fn decay_namespaces(
    config: &config::Config,
    stored_namespaces: impl IntoIterator<Item = String>,
) -> Vec<String> {
    let mut namespaces = std::collections::BTreeSet::from(["default".to_string()]);
    namespaces.extend(
        config
            .auth
            .api_keys
            .iter()
            .map(|entry| entry.namespace.clone()),
    );
    namespaces.extend(stored_namespaces.into_iter().filter(|ns| !ns.is_empty()));
    namespaces.into_iter().collect()
}

fn namespace_health(summary: &crate::memory::LifecycleSummary) -> &'static str {
    if summary.total == 0 {
        "empty"
    } else if summary.contested > 0 {
        "contested"
    } else if summary.candidate > 0 {
        "review"
    } else if summary.stale > 0 {
        "maintenance"
    } else {
        "healthy"
    }
}

fn index_health_status(pending_outbox: usize) -> &'static str {
    if pending_outbox == 0 {
        "healthy"
    } else if pending_outbox < 100 {
        "catching_up"
    } else {
        "behind"
    }
}

fn derived_index_materialization_label(materialized: bool, derived_items: usize) -> &'static str {
    if materialized {
        "present"
    } else if derived_items > 0 {
        "startup-derived"
    } else {
        "not yet materialized"
    }
}

#[derive(Debug)]
struct LocalIndexHealth {
    checkpoint: u64,
    pending_outbox: usize,
    status: &'static str,
    fts_dir_exists: bool,
    vector_index_exists: bool,
    vector_mapping_exists: bool,
    embedded_memories: usize,
    embedding_dimension_mismatches: usize,
}

fn open_operator_doc_engine(
    config: &config::Config,
) -> anyhow::Result<engines::document::DocumentEngine> {
    engines::document::DocumentEngine::open_with_config(
        &config.storage.data_dir,
        &config.encryption,
        config.auth.audit_hmac_secret.as_bytes(),
    )
}

fn collect_namespace_memories(
    config: &config::Config,
    doc_engine: &engines::document::DocumentEngine,
    namespace_filter: Option<&str>,
) -> anyhow::Result<Vec<(String, Vec<crate::memory::Memory>)>> {
    let namespaces = if let Some(namespace) = namespace_filter {
        vec![namespace.to_string()]
    } else {
        decay_namespaces(config, doc_engine.list_namespaces()?)
    };

    namespaces
        .into_iter()
        .map(|namespace| {
            let memories = doc_engine.list_all_including_archived(&namespace)?;
            Ok((namespace, memories))
        })
        .collect()
}

fn snapshot_local_index_health(
    config: &config::Config,
    doc_engine: &engines::document::DocumentEngine,
    namespace_memories: &[(String, Vec<crate::memory::Memory>)],
) -> anyhow::Result<LocalIndexHealth> {
    let checkpoint = doc_engine.load_indexer_checkpoint();
    let pending_outbox = doc_engine
        .consume_outbox(checkpoint.saturating_add(1))?
        .len();
    let embedded_memories = namespace_memories
        .iter()
        .flat_map(|(_, memories)| memories.iter())
        .filter(|memory| memory.embedding.is_some())
        .count();
    let expected_embedding_dimension = config.embeddings.model.expected_dimension();
    let embedding_dimension_mismatches = namespace_memories
        .iter()
        .flat_map(|(_, memories)| memories.iter())
        .filter_map(|memory| memory.embedding.as_ref())
        .filter(|embedding| embedding.len() != expected_embedding_dimension)
        .count();

    #[cfg(target_os = "windows")]
    let vector_index_exists = true;
    #[cfg(not(target_os = "windows"))]
    let vector_index_exists = config.storage.data_dir.join("vectors.usearch").exists();

    #[cfg(target_os = "windows")]
    let vector_mapping_exists = true;
    #[cfg(not(target_os = "windows"))]
    let vector_mapping_exists = config.storage.data_dir.join("vector_keys.json").exists();

    Ok(LocalIndexHealth {
        checkpoint,
        pending_outbox,
        status: index_health_status(pending_outbox),
        fts_dir_exists: config.storage.data_dir.join("fts").exists(),
        vector_index_exists,
        vector_mapping_exists,
        embedded_memories,
        embedding_dimension_mismatches,
    })
}

fn render_status_report(
    config: &config::Config,
    config_path: &Path,
    namespace_memories: &[(String, Vec<crate::memory::Memory>)],
    index_health: &LocalIndexHealth,
) -> String {
    use std::fmt::Write as _;

    let mut output = String::new();
    let _ = writeln!(output, "Config: {}", config_path.display());
    let _ = writeln!(
        output,
        "Server: {}:{} ({}, tls={})",
        config.server.host,
        config.server.port,
        if config.server.hybrid_mode {
            "hybrid"
        } else {
            "standalone"
        },
        if config.tls.enabled {
            "enabled"
        } else {
            "disabled"
        }
    );
    let _ = writeln!(output, "Data dir: {}", config.storage.data_dir.display());
    let _ = writeln!(output);
    let _ = writeln!(output, "Namespaces:");
    for (namespace, memories) in namespace_memories {
        let summary = server::routes::lifecycle_summary_from_memories(memories);
        let _ = writeln!(
            output,
            "- {} [{}]: total={} active={} candidate={} contested={} stale={} archived={}",
            namespace,
            namespace_health(&summary),
            summary.total,
            summary.active,
            summary.candidate,
            summary.contested,
            summary.stale,
            summary.archived
        );
    }

    let _ = writeln!(output);
    let _ = writeln!(output, "Workers:");
    let _ = writeln!(
        output,
        "- indexer: {} (checkpoint={} pending_outbox={})",
        index_health.status, index_health.checkpoint, index_health.pending_outbox
    );
    let _ = writeln!(
        output,
        "- decay: {} (after_days={})",
        if config.decay.enabled {
            "enabled"
        } else {
            "disabled"
        },
        config.decay.after_days
    );
    let _ = writeln!(
        output,
        "- consolidation: {} (interval={}m threshold={:.2})",
        if config.consolidation.enabled {
            "enabled"
        } else {
            "disabled"
        },
        config.consolidation.interval_minutes,
        config.consolidation.threshold
    );
    let _ = writeln!(
        output,
        "- proxy extraction: {} (provider={} model={})",
        if config.proxy.enabled && config.proxy.extraction_enabled {
            "enabled"
        } else if config.proxy.enabled {
            "disabled"
        } else {
            "proxy disabled"
        },
        config.proxy.extract_provider,
        config.proxy.extract_model
    );
    let _ = writeln!(
        output,
        "- group commit: batch={} flush={}ms",
        config.limits.group_commit_batch_size, config.limits.group_commit_flush_ms
    );
    let _ = writeln!(
        output,
        "- embeddings: model={} dimension={}",
        config.embeddings.model,
        config.embeddings.model.expected_dimension()
    );

    let _ = writeln!(output);
    let _ = writeln!(output, "Index:");
    let _ = writeln!(output, "- status: {}", index_health.status);
    let _ = writeln!(output, "- checkpoint: {}", index_health.checkpoint);
    let _ = writeln!(output, "- pending_outbox: {}", index_health.pending_outbox);
    let _ = writeln!(
        output,
        "- embedded_memories: {}",
        index_health.embedded_memories
    );
    let _ = writeln!(
        output,
        "- embedding_dimension_mismatches: {}",
        index_health.embedding_dimension_mismatches
    );
    let _ = writeln!(
        output,
        "- fts_dir: {}",
        derived_index_materialization_label(
            index_health.fts_dir_exists,
            index_health.embedded_memories,
        )
    );
    let _ = writeln!(
        output,
        "- vector_index: {}",
        derived_index_materialization_label(
            index_health.vector_index_exists,
            index_health.embedded_memories,
        )
    );
    let _ = writeln!(
        output,
        "- vector_mappings: {}",
        derived_index_materialization_label(
            index_health.vector_mapping_exists,
            index_health.embedded_memories,
        )
    );

    output
}

fn render_recent_report(activities: &[(String, serde_json::Value)], limit: usize) -> String {
    use std::fmt::Write as _;

    let mut output = String::new();
    let _ = writeln!(output, "Recent activity (limit {})", limit);

    for (namespace, recent) in activities {
        let _ = writeln!(output);
        let _ = writeln!(output, "Namespace: {}", namespace);
        for (title, key) in [
            ("Injections", "injections"),
            ("Extractions", "extractions"),
            ("Feedbacks", "feedbacks"),
            ("Consolidations", "consolidations"),
        ] {
            let count = recent["counts"][key].as_u64().unwrap_or(0);
            let _ = writeln!(output, "  {}: {}", title, count);
            let Some(events) = recent[key].as_array() else {
                let _ = writeln!(output, "    - none");
                continue;
            };
            if events.is_empty() {
                let _ = writeln!(output, "    - none");
                continue;
            }
            for event in events {
                let detail = match key {
                    "injections" => format!(
                        "injections={} reuse={}",
                        event["injection_count"].as_u64().unwrap_or(0),
                        event["reuse_count"].as_u64().unwrap_or(0)
                    ),
                    "extractions" => format!(
                        "confidence={:.2} evidence={}",
                        event["confidence"].as_f64().unwrap_or(0.0),
                        event["evidence_count"].as_u64().unwrap_or(0)
                    ),
                    "feedbacks" => format!(
                        "action={} confirm={} reject={} supersede={}",
                        event["action"].as_str().unwrap_or("-"),
                        event["confirm_count"].as_u64().unwrap_or(0),
                        event["reject_count"].as_u64().unwrap_or(0),
                        event["supersede_count"].as_u64().unwrap_or(0)
                    ),
                    "consolidations" => format!(
                        "derived_from={}",
                        event["derived_count"].as_u64().unwrap_or(0)
                    ),
                    _ => String::new(),
                };
                let _ = writeln!(
                    output,
                    "    - {} {} {} {}",
                    event["at"].as_str().unwrap_or("-"),
                    event["id"].as_str().unwrap_or("-"),
                    detail,
                    event["preview"].as_str().unwrap_or("")
                );
            }
        }
    }

    output
}

fn render_review_queue_report(
    queues: &[(String, crate::server::routes::ReviewQueueView)],
    limit: usize,
) -> String {
    use std::fmt::Write as _;

    let mut output = String::new();
    let _ = writeln!(output, "Review inbox (limit {})", limit);

    for (namespace, queue) in queues {
        let _ = writeln!(output);
        let _ = writeln!(output, "Namespace: {}", namespace);
        let _ = writeln!(
            output,
            "  Pending review items: {} (candidate={} contested={} rejected={})",
            queue.summary.pending,
            queue.summary.candidate,
            queue.summary.contested,
            queue.summary.rejected
        );
        let _ = writeln!(
            output,
            "  Suggested actions: confirm={} reject={} supersede={}",
            queue.summary.suggested_confirm,
            queue.summary.suggested_reject,
            queue.summary.suggested_supersede
        );
        if queue.items.is_empty() {
            let _ = writeln!(output, "  - none");
            continue;
        }

        for (idx, item) in queue.items.iter().enumerate() {
            let _ = writeln!(
                output,
                "  {}. [{} -> {}] trust={:.2} age={}h source={}",
                idx + 1,
                item.queue_kind,
                item.suggested_action,
                item.trust_score,
                item.age_hours,
                item.source
            );
            let _ = writeln!(output, "     {}", item.preview);
            if !item.replacement_options.is_empty() {
                let replacements = item
                    .replacement_options
                    .iter()
                    .map(|option| option.preview.clone())
                    .collect::<Vec<_>>()
                    .join(" | ");
                let _ = writeln!(output, "     replacements: {}", replacements);
            }
        }
    }

    output
}

fn render_hud_report(config_path: &Path, hud: &crate::server::routes::HudView) -> String {
    use std::fmt::Write as _;

    let mut output = String::new();
    let _ = writeln!(output, "Memory HUD");
    let _ = writeln!(output, "Config: {}", config_path.display());
    let _ = writeln!(output, "Generated: {}", hud.generated_at.to_rfc3339());
    if let Some(namespace) = &hud.namespace_filter {
        let _ = writeln!(output, "Namespace filter: {}", namespace);
    }

    let _ = writeln!(output);
    let _ = writeln!(output, "Overview:");
    let _ = writeln!(
        output,
        "- namespaces={} total={} active={} contested={} pending_review={}",
        hud.summary.namespaces,
        hud.summary.total_memories,
        hud.summary.active_memories,
        hud.summary.contested_memories,
        hud.summary.pending_reviews
    );
    if let Some(counters) = &hud.policy_firewall.live_counters {
        let _ = writeln!(
            output,
            "- live policy: block={} require_confirmation={} warn={}",
            counters.block, counters.require_confirmation, counters.warn
        );
    } else {
        let _ = writeln!(output, "- live policy: unavailable in offline CLI mode");
    }
    let _ = writeln!(
        output,
        "- actionable policy probes={}",
        hud.policy_firewall.actionable_probes
    );

    let _ = writeln!(output);
    let _ = writeln!(output, "Quick actions:");
    for action in &hud.quick_actions {
        let _ = writeln!(
            output,
            "- {} [{} {}]",
            action.label, action.method, action.path
        );
        let _ = writeln!(output, "  {}", action.description);
        let _ = writeln!(output, "  {}", action.cli);
    }

    for namespace in &hud.namespaces {
        let counts = &namespace.recent["counts"];
        let _ = writeln!(output);
        let _ = writeln!(output, "Namespace: {}", namespace.namespace);
        let _ = writeln!(
            output,
            "  Lifecycle: total={} active={} candidate={} contested={} stale={} archived={}",
            namespace.lifecycle.total,
            namespace.lifecycle.active,
            namespace.lifecycle.candidate,
            namespace.lifecycle.contested,
            namespace.lifecycle.stale,
            namespace.lifecycle.archived
        );
        let _ = writeln!(
            output,
            "  Recent: injections={} extractions={} feedbacks={} consolidations={}",
            counts["injections"].as_u64().unwrap_or(0),
            counts["extractions"].as_u64().unwrap_or(0),
            counts["feedbacks"].as_u64().unwrap_or(0),
            counts["consolidations"].as_u64().unwrap_or(0)
        );
        let _ = writeln!(
            output,
            "  Review: pending={} candidate={} contested={} rejected={}",
            namespace.review_queue.summary.pending,
            namespace.review_queue.summary.candidate,
            namespace.review_queue.summary.contested,
            namespace.review_queue.summary.rejected
        );
        if namespace.review_queue.items.is_empty() {
            let _ = writeln!(output, "  Queue items: none");
        } else {
            let _ = writeln!(output, "  Queue items:");
            for (idx, item) in namespace.review_queue.items.iter().take(3).enumerate() {
                let _ = writeln!(
                    output,
                    "    {}. [{} -> {}] trust={:.2} {}",
                    idx + 1,
                    item.queue_kind,
                    item.suggested_action,
                    item.trust_score,
                    item.preview
                );
            }
        }
        let _ = writeln!(output, "  Blocked by policy:");
        for probe in &namespace.policy_probes {
            let actions = if probe.risky_actions.is_empty() {
                "-".to_string()
            } else {
                probe.risky_actions.join(",")
            };
            let _ = writeln!(
                output,
                "    - {}: {} matches={} actions={}",
                probe.label, probe.decision, probe.matched_policy_count, actions
            );
            for preview in probe.matched_policy_previews.iter().take(2) {
                let _ = writeln!(output, "      {}", preview);
            }
        }
    }

    output
}

fn run_status(
    config: &config::Config,
    config_path: &Path,
    namespace_filter: Option<&str>,
) -> anyhow::Result<()> {
    let doc_engine = open_operator_doc_engine(config)?;
    let namespace_memories = collect_namespace_memories(config, &doc_engine, namespace_filter)?;
    let index_health = snapshot_local_index_health(config, &doc_engine, &namespace_memories)?;
    print!(
        "{}",
        render_status_report(config, config_path, &namespace_memories, &index_health)
    );
    Ok(())
}

fn run_recent(
    config: &config::Config,
    namespace_filter: Option<&str>,
    limit: usize,
) -> anyhow::Result<()> {
    let doc_engine = open_operator_doc_engine(config)?;
    let namespace_memories = collect_namespace_memories(config, &doc_engine, namespace_filter)?;
    let activities: Vec<_> = namespace_memories
        .into_iter()
        .map(|(namespace, memories)| {
            (
                namespace,
                server::routes::build_recent_activity(&memories, limit),
            )
        })
        .collect();
    print!("{}", render_recent_report(&activities, limit));
    Ok(())
}

fn run_hud(
    config: &config::Config,
    config_path: &Path,
    namespace_filter: Option<&str>,
    limit: usize,
) -> anyhow::Result<()> {
    let doc_engine = open_operator_doc_engine(config)?;
    let trust_scorer = crate::security::trust::TrustScorer::new(config.trust.threshold);
    let _ = trust_scorer.load_from_redb(doc_engine.db());
    let namespace_memories = collect_namespace_memories(config, &doc_engine, namespace_filter)?;
    let hud = crate::server::routes::build_hud_view(
        &namespace_memories,
        &trust_scorer,
        limit,
        namespace_filter.map(str::to_string),
        None,
    );
    print!("{}", render_hud_report(config_path, &hud));
    Ok(())
}

fn run_review_queue(
    config: &config::Config,
    namespace_filter: Option<&str>,
    limit: usize,
) -> anyhow::Result<()> {
    let doc_engine = open_operator_doc_engine(config)?;
    let trust_scorer = crate::security::trust::TrustScorer::new(config.trust.threshold);
    let _ = trust_scorer.load_from_redb(doc_engine.db());
    let namespace_memories = collect_namespace_memories(config, &doc_engine, namespace_filter)?;
    let queues: Vec<_> = namespace_memories
        .into_iter()
        .map(|(namespace, memories)| {
            let queue =
                server::routes::build_review_queue(&memories, &trust_scorer, &namespace, limit);
            (namespace, queue)
        })
        .collect();
    print!("{}", render_review_queue_report(&queues, limit));
    Ok(())
}

fn run_review_action(
    config: &config::Config,
    namespace: &str,
    item: usize,
    action: crate::memory::MemoryFeedbackAction,
    supersede_with_item: Option<usize>,
) -> anyhow::Result<()> {
    if item == 0 {
        anyhow::bail!("item must be >= 1");
    }
    if matches!(action, crate::memory::MemoryFeedbackAction::Supersede)
        && supersede_with_item.is_none()
    {
        anyhow::bail!("supersede requires --with-item");
    }

    let doc_engine = open_operator_doc_engine(config)?;
    let trust_scorer = crate::security::trust::TrustScorer::new(config.trust.threshold);
    let _ = trust_scorer.load_from_redb(doc_engine.db());
    let memories = doc_engine.list_all_including_archived(namespace)?;
    let queue = server::routes::build_review_queue(&memories, &trust_scorer, namespace, usize::MAX);
    if queue.items.is_empty() {
        anyhow::bail!("review inbox is empty for namespace {namespace}");
    }
    let target = queue.items.get(item - 1).ok_or_else(|| {
        anyhow::anyhow!(
            "item {} is out of range ({} queue items)",
            item,
            queue.items.len()
        )
    })?;
    let target_id =
        server::routes::decode_review_key(&target.review_key).map_err(anyhow::Error::msg)?;

    let mut memory = doc_engine
        .get(target_id, namespace)?
        .ok_or_else(|| anyhow::anyhow!("review target not found"))?;

    let mut superseded_by = None;
    let mut replacement_preview = None;
    if let Some(with_item) = supersede_with_item {
        if with_item == 0 {
            anyhow::bail!("with-item must be >= 1");
        }
        let replacement = queue.items.get(with_item - 1).ok_or_else(|| {
            anyhow::anyhow!(
                "with-item {} is out of range ({} queue items)",
                with_item,
                queue.items.len()
            )
        })?;
        let replacement_id = server::routes::decode_review_key(&replacement.review_key)
            .map_err(anyhow::Error::msg)?;
        if replacement_id == target_id {
            anyhow::bail!("item cannot supersede itself");
        }
        superseded_by = Some(replacement_id);
        replacement_preview = Some(replacement.preview.clone());
        let mut replacement_memory = doc_engine
            .get(replacement_id, namespace)?
            .ok_or_else(|| anyhow::anyhow!("replacement memory not found"))?;
        server::routes::apply_feedback_to_memory(
            &mut replacement_memory,
            crate::memory::MemoryFeedbackAction::Confirm,
            None,
            "local-review-cli",
            "review_cli_supersede_target",
        );
        doc_engine.replace(&replacement_memory, "local-review-cli")?;
    }

    server::routes::apply_feedback_to_memory(
        &mut memory,
        action,
        superseded_by,
        "local-review-cli",
        "review_cli",
    );
    doc_engine.replace(&memory, "local-review-cli")?;

    if let Some(replacement_preview) = replacement_preview {
        println!(
            "Applied {} to item {} in namespace {} using replacement: {}",
            action, item, namespace, replacement_preview
        );
    } else {
        println!(
            "Applied {} to item {} in namespace {}",
            action, item, namespace
        );
    }

    Ok(())
}

fn run_passport_export(
    config: &config::Config,
    namespace_filter: Option<&str>,
    scope: crate::memory::PassportScope,
    output: Option<PathBuf>,
) -> anyhow::Result<()> {
    let doc_engine = open_operator_doc_engine(config)?;
    let namespaces = if let Some(namespace) = namespace_filter {
        vec![namespace.to_string()]
    } else {
        decay_namespaces(config, doc_engine.list_namespaces()?)
    };

    let namespace = namespaces
        .into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("no namespace available for passport export"))?;
    let memories = doc_engine.list_all_including_archived(&namespace)?;
    let bundle = crate::memory::build_memory_passport_bundle(&namespace, scope, &memories);
    let timestamp = chrono::Utc::now().format("%Y%m%d-%H%M%S");
    let output_path = output.unwrap_or_else(|| {
        PathBuf::from(format!(
            "memoryoss-passport-{}-{}-{timestamp}.json",
            namespace, scope
        ))
    });
    std::fs::write(&output_path, serde_json::to_vec_pretty(&bundle)?)?;
    println!(
        "Exported passport bundle {} (scope={} memories={})",
        output_path.display(),
        scope,
        bundle.memories.len()
    );
    Ok(())
}

fn run_passport_import(
    config: &config::Config,
    path: &Path,
    namespace_override: Option<&str>,
    dry_run: bool,
) -> anyhow::Result<()> {
    let doc_engine = open_operator_doc_engine(config)?;
    let bundle: crate::memory::MemoryPassportBundle =
        serde_json::from_slice(&std::fs::read(path)?)?;
    if !crate::memory::verify_memory_passport_bundle(&bundle) {
        anyhow::bail!("passport bundle integrity check failed");
    }
    let target_namespace = namespace_override
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| bundle.namespace.clone());
    let existing = doc_engine.list_all_including_archived(&target_namespace)?;
    let plan = crate::memory::plan_memory_passport_import(&target_namespace, &bundle, &existing);

    if dry_run {
        println!(
            "Passport import dry-run for {}: create={} merge={} conflict={}",
            target_namespace,
            plan.preview.create_count,
            plan.preview.merge_count,
            plan.preview.conflict_count
        );
        for item in plan.preview.items.iter().take(10) {
            println!("- [{}] {} ({})", item.decision, item.preview, item.reason);
        }
        return Ok(());
    }

    let mut imported = 0usize;
    for memory in &plan.staged_memories {
        doc_engine.store(memory, "passport-import-cli")?;
        imported += 1;
    }
    println!(
        "Imported passport bundle into {}: imported={} merge={} conflict={}",
        target_namespace, imported, plan.preview.merge_count, plan.preview.conflict_count
    );
    Ok(())
}

fn resolve_operator_namespace(
    config: &config::Config,
    doc_engine: &engines::document::DocumentEngine,
    namespace_override: Option<&str>,
    purpose: &str,
) -> anyhow::Result<String> {
    if let Some(namespace) = namespace_override {
        return Ok(namespace.to_string());
    }
    decay_namespaces(config, doc_engine.list_namespaces()?)
        .into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("no namespace available for {purpose}"))
}

fn adapter_source_label(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| path.display().to_string())
}

fn load_adapter_source(kind: adapters::MemoryAdapterKind, path: &Path) -> anyhow::Result<String> {
    match kind {
        adapters::MemoryAdapterKind::ClaudeProject | adapters::MemoryAdapterKind::CursorRules => {
            Ok(std::fs::read_to_string(path)?)
        }
        adapters::MemoryAdapterKind::GitHistory => {
            let output = std::process::Command::new("git")
                .args([
                    "-C",
                    path.to_str()
                        .ok_or_else(|| anyhow::anyhow!("invalid git path"))?,
                    "log",
                    "--max-count=24",
                    "--format=%H%x1f%s%x1f%b%x1e",
                ])
                .output()?;
            if !output.status.success() {
                anyhow::bail!(
                    "git log failed: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                );
            }
            Ok(String::from_utf8(output.stdout)?)
        }
    }
}

fn run_adapter_import(
    config: &config::Config,
    kind: adapters::MemoryAdapterKind,
    path: &Path,
    namespace_override: Option<&str>,
    dry_run: bool,
) -> anyhow::Result<()> {
    let doc_engine = open_operator_doc_engine(config)?;
    let target_namespace =
        resolve_operator_namespace(config, &doc_engine, namespace_override, "adapter import")?;
    let source_label = adapter_source_label(path);
    let source = load_adapter_source(kind, path)?;
    let existing = doc_engine.list_all_including_archived(&target_namespace)?;
    let plan =
        adapters::plan_adapter_import(&target_namespace, kind, &source_label, &source, &existing);

    if dry_run {
        println!(
            "Adapter import dry-run for {} [{} {}]: normalized={} create={} merge={} conflict={}",
            target_namespace,
            kind,
            source_label,
            plan.preview.normalized_count,
            plan.preview.preview.create_count,
            plan.preview.preview.merge_count,
            plan.preview.preview.conflict_count
        );
        for item in plan.preview.preview.items.iter().take(10) {
            println!("- [{}] {} ({})", item.decision, item.preview, item.reason);
        }
        return Ok(());
    }

    let mut imported = 0usize;
    for memory in &plan.staged_memories {
        doc_engine.store(memory, &format!("adapter-import-cli:{kind}"))?;
        imported += 1;
    }
    println!(
        "Imported adapter {} into {}: normalized={} imported={} merge={} conflict={}",
        kind,
        target_namespace,
        plan.preview.normalized_count,
        imported,
        plan.preview.preview.merge_count,
        plan.preview.preview.conflict_count
    );
    Ok(())
}

fn run_adapter_export(
    config: &config::Config,
    kind: adapters::MemoryAdapterKind,
    namespace_override: Option<&str>,
    output: Option<PathBuf>,
) -> anyhow::Result<()> {
    let doc_engine = open_operator_doc_engine(config)?;
    let namespace =
        resolve_operator_namespace(config, &doc_engine, namespace_override, "adapter export")?;
    let memories = doc_engine.list_all_including_archived(&namespace)?;
    let artifact =
        adapters::render_adapter_export(kind, &namespace, &memories).map_err(anyhow::Error::msg)?;
    let timestamp = chrono::Utc::now().format("%Y%m%d-%H%M%S");
    let output_path = output.unwrap_or_else(|| {
        PathBuf::from(format!(
            "memoryoss-{}-{}-{timestamp}.{}",
            kind,
            namespace,
            kind.default_extension()
        ))
    });
    std::fs::write(&output_path, artifact.content)?;
    println!(
        "Exported adapter artifact {} [{} memories={}]",
        output_path.display(),
        kind,
        artifact.exported_count
    );
    Ok(())
}

fn run_connector_list() {
    println!("Ambient connector mesh");
    for connector in crate::server::routes::ambient_connector_definitions() {
        println!(
            "- {:<12} opt_in={} redact_sensitive={} capture_raw={}",
            connector.kind,
            connector.enabled_by_default,
            connector.redact_sensitive_by_default,
            connector.capture_raw_by_default
        );
        println!("  {}", connector.description);
    }
}

fn render_connector_preview(
    namespace: &str,
    preview: &crate::server::routes::AmbientConnectorPreview,
) -> String {
    use std::fmt::Write as _;

    let mut output = String::new();
    let _ = writeln!(output, "Ambient connector candidate");
    let _ = writeln!(output, "Namespace: {}", namespace);
    let _ = writeln!(output, "Connector: {}", preview.connector);
    let _ = writeln!(output, "Source:    {}", preview.source);
    let _ = writeln!(output, "Redacted:  {}", preview.redacted);
    let _ = writeln!(output, "Evidence:  {}", preview.evidence_count);
    let _ = writeln!(output, "Tags:      {}", preview.tags.join(", "));
    let _ = writeln!(output, "Preview:   {}", preview.preview);
    output
}

fn run_connector_ingest(
    config: &config::Config,
    kind: crate::server::routes::AmbientConnectorKind,
    namespace_override: Option<&str>,
    summary: String,
    evidence: Vec<String>,
    tags: Vec<String>,
    source_ref: Option<String>,
    allow_raw: bool,
    dry_run: bool,
) -> anyhow::Result<()> {
    std::fs::create_dir_all(&config.storage.data_dir)?;
    let doc_engine = open_operator_doc_engine(config)?;
    let namespace =
        resolve_operator_namespace(config, &doc_engine, namespace_override, "connector ingest")?;
    let signal = crate::server::routes::AmbientConnectorSignal {
        connector: kind,
        summary,
        evidence,
        tags,
        source_ref,
        redact_sensitive: !allow_raw,
    };
    let prepared = crate::server::routes::prepare_ambient_connector_candidate(&namespace, &signal)
        .map_err(anyhow::Error::msg)?;
    print!(
        "{}",
        render_connector_preview(&namespace, &prepared.preview)
    );

    if dry_run {
        println!("Dry-run: no candidate written. Review path remains the same inbox.");
        return Ok(());
    }

    doc_engine.store(
        &prepared.memory,
        prepared
            .memory
            .source_key
            .as_deref()
            .unwrap_or("ambient-connector"),
    )?;
    println!(
        "Stored ambient connector candidate into {}. Review it with `memoryoss review queue --namespace {}`.",
        namespace, namespace
    );
    Ok(())
}

fn render_history_report(view: &crate::memory::MemoryHistoryView) -> String {
    use std::fmt::Write as _;

    let mut output = String::new();
    let _ = writeln!(output, "Memory history");
    let _ = writeln!(output, "Namespace: {}", view.namespace);
    let _ = writeln!(output, "Root:      {}", view.root_id);
    let _ = writeln!(output, "Nodes:     {}", view.nodes.len());
    let _ = writeln!(output, "Visible:   {}", view.visible_memory_ids.len());
    let _ = writeln!(
        output,
        "Branch:    {}",
        if view.branch_safe { "safe" } else { "unsafe" }
    );

    let _ = writeln!(output);
    let _ = writeln!(output, "Nodes");
    for node in &view.nodes {
        let _ = writeln!(
            output,
            "- {} [{} visible={} reviews={}] {}",
            node.id, node.status, node.visible, node.review_event_count, node.preview
        );
    }

    let _ = writeln!(output);
    let _ = writeln!(output, "Edges");
    if view.edges.is_empty() {
        let _ = writeln!(output, "- none");
    } else {
        for edge in &view.edges {
            let _ = writeln!(output, "- {} {} {}", edge.from, edge.kind, edge.to);
        }
    }

    let _ = writeln!(output);
    let _ = writeln!(output, "Timeline");
    if view.timeline.is_empty() {
        let _ = writeln!(output, "- none");
    } else {
        for event in &view.timeline {
            let _ = writeln!(
                output,
                "- {} [{}] {} {}",
                event.at, event.kind, event.memory_id, event.summary
            );
        }
    }

    output
}

fn run_history_show(
    config: &config::Config,
    namespace: &str,
    id: uuid::Uuid,
) -> anyhow::Result<()> {
    let doc_engine = open_operator_doc_engine(config)?;
    let memories = doc_engine.list_all_including_archived(namespace)?;
    let view = crate::memory::build_memory_history_view(namespace, id, &memories)
        .ok_or_else(|| anyhow::anyhow!("history root not found in namespace {namespace}"))?;
    print!("{}", render_history_report(&view));
    Ok(())
}

fn run_history_export(
    config: &config::Config,
    namespace: &str,
    id: uuid::Uuid,
    output: Option<PathBuf>,
) -> anyhow::Result<()> {
    let doc_engine = open_operator_doc_engine(config)?;
    let memories = doc_engine.list_all_including_archived(namespace)?;
    let bundle = crate::memory::build_memory_history_bundle(namespace, id, &memories)
        .ok_or_else(|| anyhow::anyhow!("history root not found in namespace {namespace}"))?;
    let timestamp = chrono::Utc::now().format("%Y%m%d-%H%M%S");
    let output_path = output.unwrap_or_else(|| {
        PathBuf::from(format!(
            "memoryoss-history-{}-{}-{timestamp}.json",
            namespace, id
        ))
    });
    std::fs::write(&output_path, serde_json::to_vec_pretty(&bundle)?)?;
    println!(
        "Exported history bundle {} (root={} memories={})",
        output_path.display(),
        id,
        bundle.memories.len()
    );
    Ok(())
}

fn run_history_replay(
    config: &config::Config,
    path: &Path,
    namespace_override: Option<&str>,
    dry_run: bool,
) -> anyhow::Result<()> {
    let doc_engine = open_operator_doc_engine(config)?;
    let bundle: crate::memory::MemoryHistoryBundle = serde_json::from_slice(&std::fs::read(path)?)?;
    let target_namespace = namespace_override
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| bundle.namespace.clone());
    let existing = doc_engine.list_all_including_archived(&target_namespace)?;
    let plan = crate::memory::plan_memory_history_replay(&target_namespace, &bundle, &existing);

    if dry_run || !plan.preview.can_replay {
        println!(
            "History replay preview for {}: can_replay={} create={} visible={} blocked_reason={}",
            target_namespace,
            plan.preview.can_replay,
            plan.preview.create_count,
            plan.preview.visible_count,
            plan.preview.blocked_reason.as_deref().unwrap_or("-")
        );
        if !plan.preview.can_replay && !dry_run {
            anyhow::bail!(
                "{}",
                plan.preview
                    .blocked_reason
                    .as_deref()
                    .unwrap_or("history replay blocked")
            );
        }
        return Ok(());
    }

    for memory in &plan.staged_memories {
        doc_engine.store(memory, "history-replay-cli")?;
    }
    println!(
        "Replayed history bundle into {}: imported={} root={}",
        target_namespace,
        plan.staged_memories.len(),
        bundle.root_id
    );
    Ok(())
}

fn run_history_branch(
    config: &config::Config,
    source_namespace: &str,
    target_namespace: &str,
    id: uuid::Uuid,
    dry_run: bool,
) -> anyhow::Result<()> {
    let doc_engine = open_operator_doc_engine(config)?;
    let source_memories = doc_engine.list_all_including_archived(source_namespace)?;
    let bundle = crate::memory::build_memory_history_bundle(source_namespace, id, &source_memories)
        .ok_or_else(|| anyhow::anyhow!("history root not found in namespace {source_namespace}"))?;
    let existing = doc_engine.list_all_including_archived(target_namespace)?;
    let plan = crate::memory::plan_memory_history_replay(target_namespace, &bundle, &existing);

    if dry_run || !plan.preview.can_replay {
        println!(
            "History branch preview {} -> {}: can_replay={} create={} blocked_reason={}",
            source_namespace,
            target_namespace,
            plan.preview.can_replay,
            plan.preview.create_count,
            plan.preview.blocked_reason.as_deref().unwrap_or("-")
        );
        if !plan.preview.can_replay && !dry_run {
            anyhow::bail!(
                "{}",
                plan.preview
                    .blocked_reason
                    .as_deref()
                    .unwrap_or("history branch blocked")
            );
        }
        return Ok(());
    }

    for memory in &plan.staged_memories {
        doc_engine.store(memory, "history-branch-cli")?;
    }
    println!(
        "Branched history root {} from {} into {}: imported={}",
        id,
        source_namespace,
        target_namespace,
        plan.staged_memories.len()
    );
    Ok(())
}

fn read_memory_bundle(path: &Path) -> anyhow::Result<crate::server::routes::MemoryBundleEnvelope> {
    Ok(serde_json::from_slice(&std::fs::read(path)?)?)
}

#[derive(Debug, Clone, Copy)]
enum ReaderFormat {
    Text,
    Json,
    Html,
}

impl FromStr for ReaderFormat {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "text" => Ok(Self::Text),
            "json" => Ok(Self::Json),
            "html" => Ok(Self::Html),
            other => Err(format!("unsupported reader format: {other}")),
        }
    }
}

#[derive(Debug, Clone)]
enum ReaderArtifact {
    Envelope(crate::server::routes::MemoryBundleEnvelope),
    Passport(crate::memory::MemoryPassportBundle),
    History(crate::memory::MemoryHistoryBundle),
}

#[derive(Debug, Clone, serde::Serialize)]
struct ReaderIntegrityView {
    envelope_valid: bool,
    nested_valid: bool,
    algorithm: String,
    payload_sha256: String,
}

#[derive(Debug, Clone, serde::Serialize)]
struct ReaderSignatureView {
    scheme: String,
    signer: Option<String>,
    signable: bool,
    identity_kind: Option<String>,
    signed_at: Option<chrono::DateTime<chrono::Utc>>,
    value_present: bool,
    chain: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
struct ReaderTrustView {
    status: String,
    verified: bool,
    reason: String,
    origin: Option<String>,
    catalog_id: Option<String>,
    replacement_identity: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
struct ReaderProvenanceView {
    exported_from_namespace: String,
    source_key_ids: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
struct ReaderOpenView {
    source_path: String,
    artifact_line: String,
    artifact_type: String,
    kind: String,
    namespace: String,
    scope: Option<String>,
    runtime_contract_id: String,
    exported_at: chrono::DateTime<chrono::Utc>,
    memory_count: usize,
    visible_count: usize,
    root_id: Option<uuid::Uuid>,
    reference_uri: Option<String>,
    attachment_name: Option<String>,
    integrity: ReaderIntegrityView,
    signature: Option<ReaderSignatureView>,
    trust: Option<ReaderTrustView>,
    provenance: Option<ReaderProvenanceView>,
    preview: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
struct ReaderDiffView {
    left: ReaderOpenView,
    right: ReaderOpenView,
    same_kind: bool,
    changed_fields: Vec<String>,
    added_preview: Vec<String>,
    removed_preview: Vec<String>,
    shared_preview_count: usize,
}

struct ReaderTrustContext {
    registry: crate::security::trust::PortableTrustRegistry,
    secret: Vec<u8>,
}

fn reader_preview_text(content: &str, max_chars: usize) -> String {
    let trimmed = content.trim();
    let mut chars = trimmed.chars();
    let preview: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{preview}...")
    } else {
        preview
    }
}

fn escape_reader_html(raw: &str) -> String {
    raw.chars()
        .map(|ch| match ch {
            '&' => "&amp;".to_string(),
            '<' => "&lt;".to_string(),
            '>' => "&gt;".to_string(),
            '"' => "&quot;".to_string(),
            '\'' => "&#39;".to_string(),
            _ => ch.to_string(),
        })
        .collect()
}

fn read_reader_artifact(path: &Path) -> anyhow::Result<ReaderArtifact> {
    let bytes = std::fs::read(path)?;
    if let Ok(bundle) =
        serde_json::from_slice::<crate::server::routes::MemoryBundleEnvelope>(&bytes)
    {
        return Ok(ReaderArtifact::Envelope(bundle));
    }
    if let Ok(passport) = serde_json::from_slice::<crate::memory::MemoryPassportBundle>(&bytes) {
        return Ok(ReaderArtifact::Passport(passport));
    }
    if let Ok(history) = serde_json::from_slice::<crate::memory::MemoryHistoryBundle>(&bytes) {
        return Ok(ReaderArtifact::History(history));
    }
    anyhow::bail!(
        "unsupported or malformed memory reader artifact: {}",
        path.display()
    )
}

fn load_reader_trust_context(config_path: &Path) -> Option<ReaderTrustContext> {
    if !config_path.exists() {
        return None;
    }
    let config = config::Config::load(config_path).ok()?;
    let registry =
        crate::security::trust::PortableTrustRegistry::open(&config.storage.data_dir).ok()?;
    Some(ReaderTrustContext {
        registry,
        secret: config.auth.audit_hmac_secret.into_bytes(),
    })
}

fn reader_entry_set(
    artifact: &ReaderArtifact,
) -> anyhow::Result<std::collections::BTreeSet<String>> {
    match artifact {
        ReaderArtifact::Envelope(bundle) => match bundle.kind {
            crate::server::routes::MemoryBundleKind::Passport => {
                let passport: crate::memory::MemoryPassportBundle =
                    serde_json::from_value(bundle.payload.clone())?;
                Ok(passport
                    .memories
                    .into_iter()
                    .map(|memory| memory.content)
                    .collect())
            }
            crate::server::routes::MemoryBundleKind::History => {
                let history: crate::memory::MemoryHistoryBundle =
                    serde_json::from_value(bundle.payload.clone())?;
                Ok(history
                    .memories
                    .into_iter()
                    .map(|memory| memory.content)
                    .collect())
            }
        },
        ReaderArtifact::Passport(passport) => Ok(passport
            .memories
            .iter()
            .map(|memory| memory.content.clone())
            .collect()),
        ReaderArtifact::History(history) => Ok(history
            .memories
            .iter()
            .map(|memory| memory.content.clone())
            .collect()),
    }
}

fn build_reader_open_view(
    path: &Path,
    artifact: &ReaderArtifact,
    trust_context: Option<&ReaderTrustContext>,
) -> anyhow::Result<ReaderOpenView> {
    let source_path = path.display().to_string();
    match artifact {
        ReaderArtifact::Envelope(bundle) => {
            let preview =
                crate::server::routes::preview_memory_bundle(bundle).map_err(anyhow::Error::msg)?;
            let nested_valid = match bundle.kind {
                crate::server::routes::MemoryBundleKind::Passport => {
                    let passport: crate::memory::MemoryPassportBundle =
                        serde_json::from_value(bundle.payload.clone())?;
                    crate::memory::verify_memory_passport_bundle(&passport)
                }
                crate::server::routes::MemoryBundleKind::History => {
                    let history: crate::memory::MemoryHistoryBundle =
                        serde_json::from_value(bundle.payload.clone())?;
                    crate::memory::verify_memory_history_bundle(&history)
                }
            };
            let provenance = match bundle.kind {
                crate::server::routes::MemoryBundleKind::Passport => {
                    let passport: crate::memory::MemoryPassportBundle =
                        serde_json::from_value(bundle.payload.clone())?;
                    Some(ReaderProvenanceView {
                        exported_from_namespace: passport.provenance.exported_from_namespace,
                        source_key_ids: passport.provenance.source_key_ids,
                    })
                }
                crate::server::routes::MemoryBundleKind::History => None,
            };
            let trust = if bundle.signature.value.is_some() {
                Some(match trust_context {
                    Some(context) => {
                        let decision = crate::server::routes::verify_memory_bundle_signature(
                            bundle,
                            &context.registry,
                            &context.secret,
                        );
                        ReaderTrustView {
                            status: serde_json::to_value(decision.status)?
                                .as_str()
                                .unwrap_or("unknown")
                                .to_string(),
                            verified: decision.verified,
                            reason: decision.reason,
                            origin: decision
                                .origin
                                .and_then(|origin| serde_json::to_value(origin).ok())
                                .and_then(|value| value.as_str().map(str::to_string)),
                            catalog_id: decision.catalog_id,
                            replacement_identity: decision.replacement_identity,
                        }
                    }
                    None => ReaderTrustView {
                        status: "verification_unavailable".to_string(),
                        verified: false,
                        reason: "trust config unavailable; provide --config to verify signatures"
                            .to_string(),
                        origin: None,
                        catalog_id: None,
                        replacement_identity: None,
                    },
                })
            } else {
                None
            };
            Ok(ReaderOpenView {
                source_path,
                artifact_line: bundle.bundle_version.clone(),
                artifact_type: "memory_bundle_envelope".to_string(),
                kind: bundle.kind.to_string(),
                namespace: bundle.namespace.clone(),
                scope: preview.scope,
                runtime_contract_id: bundle.runtime_contract_id.clone(),
                exported_at: bundle.exported_at,
                memory_count: preview.memory_count,
                visible_count: preview.visible_count,
                root_id: preview.root_id,
                reference_uri: Some(bundle.reference.uri.clone()),
                attachment_name: Some(bundle.reference.attachment_name.clone()),
                integrity: ReaderIntegrityView {
                    envelope_valid: preview.integrity_valid,
                    nested_valid,
                    algorithm: bundle.integrity.algorithm.clone(),
                    payload_sha256: bundle.integrity.payload_sha256.clone(),
                },
                signature: Some(ReaderSignatureView {
                    scheme: bundle.signature.scheme.clone(),
                    signer: bundle.signature.signer.clone(),
                    signable: bundle.signature.signable,
                    identity_kind: bundle.signature.identity_kind.map(|kind| kind.to_string()),
                    signed_at: bundle.signature.signed_at,
                    value_present: bundle.signature.value.is_some(),
                    chain: bundle
                        .signature
                        .chain
                        .iter()
                        .map(|link| {
                            format!("{} {} {}", link.at.to_rfc3339(), link.kind, link.identity)
                        })
                        .collect(),
                }),
                trust,
                provenance,
                preview: preview.preview,
            })
        }
        ReaderArtifact::Passport(passport) => Ok(ReaderOpenView {
            source_path,
            artifact_line: passport.bundle_version.clone(),
            artifact_type: "passport_bundle".to_string(),
            kind: "passport".to_string(),
            namespace: passport.namespace.clone(),
            scope: Some(passport.scope.to_string()),
            runtime_contract_id: passport.runtime_contract.contract_id.clone(),
            exported_at: passport.exported_at,
            memory_count: passport.memories.len(),
            visible_count: passport.memories.len(),
            root_id: None,
            reference_uri: None,
            attachment_name: None,
            integrity: ReaderIntegrityView {
                envelope_valid: true,
                nested_valid: crate::memory::verify_memory_passport_bundle(passport),
                algorithm: passport.integrity.algorithm.clone(),
                payload_sha256: passport.integrity.payload_sha256.clone(),
            },
            signature: Some(ReaderSignatureView {
                scheme: if passport.integrity.signed {
                    "embedded_passport_integrity".to_string()
                } else {
                    "unsigned".to_string()
                },
                signer: None,
                signable: false,
                identity_kind: None,
                signed_at: None,
                value_present: false,
                chain: Vec::new(),
            }),
            trust: None,
            provenance: Some(ReaderProvenanceView {
                exported_from_namespace: passport.provenance.exported_from_namespace.clone(),
                source_key_ids: passport.provenance.source_key_ids.clone(),
            }),
            preview: passport
                .memories
                .iter()
                .take(5)
                .map(|memory| reader_preview_text(&memory.content, 100))
                .collect(),
        }),
        ReaderArtifact::History(history) => Ok(ReaderOpenView {
            source_path,
            artifact_line: history.bundle_version.clone(),
            artifact_type: "history_bundle".to_string(),
            kind: "history".to_string(),
            namespace: history.namespace.clone(),
            scope: None,
            runtime_contract_id: history.runtime_contract.contract_id.clone(),
            exported_at: history.exported_at,
            memory_count: history.memories.len(),
            visible_count: history.memories.len(),
            root_id: Some(history.root_id),
            reference_uri: None,
            attachment_name: None,
            integrity: ReaderIntegrityView {
                envelope_valid: true,
                nested_valid: crate::memory::verify_memory_history_bundle(history),
                algorithm: history.integrity.algorithm.clone(),
                payload_sha256: history.integrity.payload_sha256.clone(),
            },
            signature: Some(ReaderSignatureView {
                scheme: "unsigned".to_string(),
                signer: None,
                signable: false,
                identity_kind: None,
                signed_at: None,
                value_present: false,
                chain: Vec::new(),
            }),
            trust: None,
            provenance: None,
            preview: history
                .memories
                .iter()
                .take(5)
                .map(|memory| reader_preview_text(&memory.content, 100))
                .collect(),
        }),
    }
}

fn build_reader_diff_view(
    left_path: &Path,
    left: &ReaderArtifact,
    right_path: &Path,
    right: &ReaderArtifact,
    trust_context: Option<&ReaderTrustContext>,
) -> anyhow::Result<ReaderDiffView> {
    let left_view = build_reader_open_view(left_path, left, trust_context)?;
    let right_view = build_reader_open_view(right_path, right, trust_context)?;
    let left_entries = reader_entry_set(left)?;
    let right_entries = reader_entry_set(right)?;

    let mut changed_fields = Vec::new();
    if left_view.artifact_line != right_view.artifact_line {
        changed_fields.push("artifact_line".to_string());
    }
    if left_view.artifact_type != right_view.artifact_type {
        changed_fields.push("artifact_type".to_string());
    }
    if left_view.kind != right_view.kind {
        changed_fields.push("kind".to_string());
    }
    if left_view.namespace != right_view.namespace {
        changed_fields.push("namespace".to_string());
    }
    if left_view.scope != right_view.scope {
        changed_fields.push("scope".to_string());
    }
    if left_view.runtime_contract_id != right_view.runtime_contract_id {
        changed_fields.push("runtime_contract_id".to_string());
    }
    if left_view.memory_count != right_view.memory_count {
        changed_fields.push("memory_count".to_string());
    }
    if left_view.root_id != right_view.root_id {
        changed_fields.push("root_id".to_string());
    }
    if left_view.signature.as_ref().map(|sig| {
        (
            &sig.scheme,
            &sig.signer,
            sig.signable,
            &sig.identity_kind,
            sig.signed_at,
        )
    }) != right_view.signature.as_ref().map(|sig| {
        (
            &sig.scheme,
            &sig.signer,
            sig.signable,
            &sig.identity_kind,
            sig.signed_at,
        )
    }) {
        changed_fields.push("signature".to_string());
    }
    if left_view.trust.as_ref().map(|trust| &trust.status)
        != right_view.trust.as_ref().map(|trust| &trust.status)
    {
        changed_fields.push("trust".to_string());
    }

    Ok(ReaderDiffView {
        same_kind: left_view.kind == right_view.kind,
        left: left_view,
        right: right_view,
        changed_fields,
        added_preview: right_entries
            .difference(&left_entries)
            .take(5)
            .map(|entry| reader_preview_text(entry, 100))
            .collect(),
        removed_preview: left_entries
            .difference(&right_entries)
            .take(5)
            .map(|entry| reader_preview_text(entry, 100))
            .collect(),
        shared_preview_count: left_entries.intersection(&right_entries).count(),
    })
}

fn render_reader_open_text(view: &ReaderOpenView) -> String {
    use std::fmt::Write as _;

    let mut output = String::new();
    let _ = writeln!(output, "Universal memory reader");
    let _ = writeln!(output, "Path:       {}", view.source_path);
    let _ = writeln!(output, "Artifact:   {}", view.artifact_type);
    let _ = writeln!(output, "Line:       {}", view.artifact_line);
    let _ = writeln!(output, "Kind:       {}", view.kind);
    let _ = writeln!(output, "Namespace:  {}", view.namespace);
    if let Some(scope) = &view.scope {
        let _ = writeln!(output, "Scope:      {}", scope);
    }
    let _ = writeln!(output, "Contract:   {}", view.runtime_contract_id);
    let _ = writeln!(output, "Exported:   {}", view.exported_at.to_rfc3339());
    let _ = writeln!(output, "Memories:   {}", view.memory_count);
    let _ = writeln!(output, "Visible:    {}", view.visible_count);
    if let Some(root_id) = view.root_id {
        let _ = writeln!(output, "Root:       {}", root_id);
    }
    if let Some(uri) = &view.reference_uri {
        let _ = writeln!(output, "URI:        {}", uri);
    }
    if let Some(attachment_name) = &view.attachment_name {
        let _ = writeln!(output, "Attachment: {}", attachment_name);
    }
    let _ = writeln!(
        output,
        "Integrity:  envelope={} nested={} algo={}",
        view.integrity.envelope_valid, view.integrity.nested_valid, view.integrity.algorithm
    );
    if let Some(signature) = &view.signature {
        let _ = writeln!(
            output,
            "Signature:  scheme={} signer={} signable={} kind={} signed_at={} value_present={}",
            signature.scheme,
            signature.signer.as_deref().unwrap_or("none"),
            signature.signable,
            signature.identity_kind.as_deref().unwrap_or("none"),
            signature
                .signed_at
                .map(|at| at.to_rfc3339())
                .unwrap_or_else(|| "none".to_string()),
            signature.value_present
        );
        if signature.chain.is_empty() {
            let _ = writeln!(output, "Chain:      - none");
        } else {
            let _ = writeln!(output, "Chain:      {}", signature.chain[0]);
            for entry in signature.chain.iter().skip(1) {
                let _ = writeln!(output, "            {}", entry);
            }
        }
    }
    if let Some(trust) = &view.trust {
        let _ = writeln!(
            output,
            "Trust:      status={} verified={} reason={}",
            trust.status, trust.verified, trust.reason
        );
        if let Some(origin) = &trust.origin {
            let _ = writeln!(output, "Origin:     {}", origin);
        }
        if let Some(catalog_id) = &trust.catalog_id {
            let _ = writeln!(output, "Catalog:    {}", catalog_id);
        }
        if let Some(replacement) = &trust.replacement_identity {
            let _ = writeln!(output, "Replacement: {}", replacement);
        }
    }
    if let Some(provenance) = &view.provenance {
        let _ = writeln!(
            output,
            "Provenance: namespace={} source_keys={}",
            provenance.exported_from_namespace,
            provenance.source_key_ids.len()
        );
    }
    let _ = writeln!(output);
    let _ = writeln!(output, "Preview");
    if view.preview.is_empty() {
        let _ = writeln!(output, "- none");
    } else {
        for item in &view.preview {
            let _ = writeln!(output, "- {}", item);
        }
    }
    output
}

fn render_reader_open_html(view: &ReaderOpenView) -> String {
    let preview_items = if view.preview.is_empty() {
        "<li>none</li>".to_string()
    } else {
        view.preview
            .iter()
            .map(|item| format!("<li>{}</li>", escape_reader_html(item)))
            .collect::<Vec<_>>()
            .join("")
    };
    let scope_row = view
        .scope
        .as_ref()
        .map(|scope| {
            format!(
                "<tr><th>Scope</th><td>{}</td></tr>",
                escape_reader_html(scope)
            )
        })
        .unwrap_or_default();
    let root_row = view
        .root_id
        .map(|root_id| format!("<tr><th>Root</th><td>{root_id}</td></tr>"))
        .unwrap_or_default();
    let uri_row = view
        .reference_uri
        .as_ref()
        .map(|uri| format!("<tr><th>URI</th><td>{}</td></tr>", escape_reader_html(uri)))
        .unwrap_or_default();
    let attachment_row = view
        .attachment_name
        .as_ref()
        .map(|name| {
            format!(
                "<tr><th>Attachment</th><td>{}</td></tr>",
                escape_reader_html(name)
            )
        })
        .unwrap_or_default();
    let signature_row = view
        .signature
        .as_ref()
        .map(|signature| {
            format!(
                "<tr><th>Signature</th><td>scheme={} signer={} signable={} kind={} signed_at={} value_present={}<br>{}</td></tr>",
                escape_reader_html(&signature.scheme),
                escape_reader_html(signature.signer.as_deref().unwrap_or("none")),
                signature.signable,
                escape_reader_html(signature.identity_kind.as_deref().unwrap_or("none")),
                escape_reader_html(
                    &signature
                        .signed_at
                        .map(|at| at.to_rfc3339())
                        .unwrap_or_else(|| "none".to_string())
                ),
                signature.value_present,
                if signature.chain.is_empty() {
                    "chain=none".to_string()
                } else {
                    format!(
                        "chain={}",
                        escape_reader_html(&signature.chain.join(" | "))
                    )
                }
            )
        })
        .unwrap_or_default();
    let trust_row = view
        .trust
        .as_ref()
        .map(|trust| {
            format!(
                "<tr><th>Trust</th><td>status={} verified={} reason={} origin={} catalog={} replacement={}</td></tr>",
                escape_reader_html(&trust.status),
                trust.verified,
                escape_reader_html(&trust.reason),
                escape_reader_html(trust.origin.as_deref().unwrap_or("none")),
                escape_reader_html(trust.catalog_id.as_deref().unwrap_or("none")),
                escape_reader_html(trust.replacement_identity.as_deref().unwrap_or("none"))
            )
        })
        .unwrap_or_default();
    let provenance_row = view
        .provenance
        .as_ref()
        .map(|provenance| {
            format!(
                "<tr><th>Provenance</th><td>namespace={} source_keys={}</td></tr>",
                escape_reader_html(&provenance.exported_from_namespace),
                provenance.source_key_ids.len()
            )
        })
        .unwrap_or_default();

    format!(
        concat!(
            "<!doctype html><html><head><meta charset=\"utf-8\">",
            "<title>memoryOSS Reader</title>",
            "<style>",
            ":root{{font-family:'IBM Plex Sans',sans-serif;color:#17202a;background:#f5f1e8;}}",
            "body{{max-width:900px;margin:0 auto;padding:32px;}}",
            "h1{{font-size:2rem;margin-bottom:1rem;}}",
            "table{{width:100%;border-collapse:collapse;background:#fffdf8;}}",
            "th,td{{padding:10px 12px;border-bottom:1px solid #d8d2c7;text-align:left;vertical-align:top;}}",
            "th{{width:180px;color:#6b4f2a;}}",
            "ul{{background:#fffdf8;padding:16px 24px;border:1px solid #d8d2c7;}}",
            "</style></head><body>",
            "<h1>Universal memory reader</h1>",
            "<table>",
            "<tr><th>Path</th><td>{path}</td></tr>",
            "<tr><th>Artifact</th><td>{artifact}</td></tr>",
            "<tr><th>Line</th><td>{line}</td></tr>",
            "<tr><th>Kind</th><td>{kind}</td></tr>",
            "<tr><th>Namespace</th><td>{namespace}</td></tr>",
            "{scope_row}",
            "<tr><th>Contract</th><td>{contract}</td></tr>",
            "<tr><th>Exported</th><td>{exported}</td></tr>",
            "<tr><th>Memories</th><td>{memory_count}</td></tr>",
            "<tr><th>Visible</th><td>{visible_count}</td></tr>",
            "{root_row}{uri_row}{attachment_row}",
            "<tr><th>Integrity</th><td>envelope={envelope_valid} nested={nested_valid} algo={algo}</td></tr>",
            "{signature_row}{trust_row}{provenance_row}",
            "</table><h2>Preview</h2><ul>{preview_items}</ul></body></html>"
        ),
        path = escape_reader_html(&view.source_path),
        artifact = escape_reader_html(&view.artifact_type),
        line = escape_reader_html(&view.artifact_line),
        kind = escape_reader_html(&view.kind),
        namespace = escape_reader_html(&view.namespace),
        scope_row = scope_row,
        contract = escape_reader_html(&view.runtime_contract_id),
        exported = escape_reader_html(&view.exported_at.to_rfc3339()),
        memory_count = view.memory_count,
        visible_count = view.visible_count,
        root_row = root_row,
        uri_row = uri_row,
        attachment_row = attachment_row,
        envelope_valid = view.integrity.envelope_valid,
        nested_valid = view.integrity.nested_valid,
        algo = escape_reader_html(&view.integrity.algorithm),
        signature_row = signature_row,
        trust_row = trust_row,
        provenance_row = provenance_row,
        preview_items = preview_items,
    )
}

fn render_reader_diff_text(diff: &ReaderDiffView) -> String {
    use std::fmt::Write as _;

    let mut output = String::new();
    let _ = writeln!(output, "Universal memory reader diff");
    let _ = writeln!(output, "Kinds match: {}", diff.same_kind);
    let _ = writeln!(output, "Shared entries: {}", diff.shared_preview_count);
    if diff.changed_fields.is_empty() {
        let _ = writeln!(output, "Changed fields: none");
    } else {
        let _ = writeln!(output, "Changed fields: {}", diff.changed_fields.join(", "));
    }
    let _ = writeln!(output);
    let _ = writeln!(output, "Left");
    let _ = writeln!(
        output,
        "  {} {} memories={}",
        diff.left.kind, diff.left.source_path, diff.left.memory_count
    );
    let _ = writeln!(output, "Right");
    let _ = writeln!(
        output,
        "  {} {} memories={}",
        diff.right.kind, diff.right.source_path, diff.right.memory_count
    );
    let _ = writeln!(output);
    let _ = writeln!(output, "Added");
    if diff.added_preview.is_empty() {
        let _ = writeln!(output, "- none");
    } else {
        for item in &diff.added_preview {
            let _ = writeln!(output, "- {}", item);
        }
    }
    let _ = writeln!(output);
    let _ = writeln!(output, "Removed");
    if diff.removed_preview.is_empty() {
        let _ = writeln!(output, "- none");
    } else {
        for item in &diff.removed_preview {
            let _ = writeln!(output, "- {}", item);
        }
    }
    output
}

fn render_reader_diff_html(diff: &ReaderDiffView) -> String {
    let changed = if diff.changed_fields.is_empty() {
        "none".to_string()
    } else {
        diff.changed_fields.join(", ")
    };
    let added = if diff.added_preview.is_empty() {
        "<li>none</li>".to_string()
    } else {
        diff.added_preview
            .iter()
            .map(|item| format!("<li>{}</li>", escape_reader_html(item)))
            .collect::<Vec<_>>()
            .join("")
    };
    let removed = if diff.removed_preview.is_empty() {
        "<li>none</li>".to_string()
    } else {
        diff.removed_preview
            .iter()
            .map(|item| format!("<li>{}</li>", escape_reader_html(item)))
            .collect::<Vec<_>>()
            .join("")
    };
    format!(
        concat!(
            "<!doctype html><html><head><meta charset=\"utf-8\">",
            "<title>memoryOSS Reader Diff</title>",
            "<style>",
            ":root{{font-family:'IBM Plex Sans',sans-serif;color:#17202a;background:#f5f1e8;}}",
            "body{{max-width:900px;margin:0 auto;padding:32px;}}",
            "section{{background:#fffdf8;border:1px solid #d8d2c7;padding:16px 20px;margin-bottom:16px;}}",
            "ul{{padding-left:20px;}}",
            "</style></head><body>",
            "<h1>Universal memory reader diff</h1>",
            "<section><strong>Changed fields:</strong> {changed}</section>",
            "<section><strong>Left:</strong> {left} ({left_count} memories)<br><strong>Right:</strong> {right} ({right_count} memories)</section>",
            "<section><h2>Added</h2><ul>{added}</ul></section>",
            "<section><h2>Removed</h2><ul>{removed}</ul></section>",
            "</body></html>"
        ),
        changed = escape_reader_html(&changed),
        left = escape_reader_html(&diff.left.source_path),
        left_count = diff.left.memory_count,
        right = escape_reader_html(&diff.right.source_path),
        right_count = diff.right.memory_count,
        added = added,
        removed = removed,
    )
}

fn run_reader_open(
    path: &Path,
    format: ReaderFormat,
    trust_context: Option<&ReaderTrustContext>,
) -> anyhow::Result<()> {
    let artifact = read_reader_artifact(path)?;
    let view = build_reader_open_view(path, &artifact, trust_context)?;
    match format {
        ReaderFormat::Text => print!("{}", render_reader_open_text(&view)),
        ReaderFormat::Json => println!("{}", serde_json::to_string_pretty(&view)?),
        ReaderFormat::Html => print!("{}", render_reader_open_html(&view)),
    }
    Ok(())
}

fn run_reader_diff(
    left: &Path,
    right: &Path,
    format: ReaderFormat,
    trust_context: Option<&ReaderTrustContext>,
) -> anyhow::Result<()> {
    let left_artifact = read_reader_artifact(left)?;
    let right_artifact = read_reader_artifact(right)?;
    let diff = build_reader_diff_view(left, &left_artifact, right, &right_artifact, trust_context)?;
    match format {
        ReaderFormat::Text => print!("{}", render_reader_diff_text(&diff)),
        ReaderFormat::Json => println!("{}", serde_json::to_string_pretty(&diff)?),
        ReaderFormat::Html => print!("{}", render_reader_diff_html(&diff)),
    }
    Ok(())
}

fn render_memory_bundle_preview(preview: &crate::server::routes::MemoryBundlePreview) -> String {
    use std::fmt::Write as _;

    let mut output = String::new();
    let _ = writeln!(output, "Memory bundle");
    let _ = writeln!(output, "ID:         {}", preview.bundle_id);
    let _ = writeln!(output, "Version:    {}", preview.bundle_version);
    let _ = writeln!(output, "Kind:       {}", preview.kind);
    let _ = writeln!(output, "Namespace:  {}", preview.namespace);
    if let Some(scope) = &preview.scope {
        let _ = writeln!(output, "Scope:      {}", scope);
    }
    let _ = writeln!(output, "Contract:   {}", preview.runtime_contract_id);
    let _ = writeln!(output, "URI:        {}", preview.uri);
    let _ = writeln!(output, "Attachment: {}", preview.attachment_name);
    let _ = writeln!(output, "Memories:   {}", preview.memory_count);
    let _ = writeln!(output, "Visible:    {}", preview.visible_count);
    if let Some(root_id) = preview.root_id {
        let _ = writeln!(output, "Root:       {}", root_id);
    }
    let _ = writeln!(
        output,
        "Integrity:  envelope={} nested={}",
        preview.integrity_valid, preview.nested_integrity_valid
    );

    let _ = writeln!(output);
    let _ = writeln!(output, "Preview");
    if preview.preview.is_empty() {
        let _ = writeln!(output, "- none");
    } else {
        for item in &preview.preview {
            let _ = writeln!(output, "- {}", item);
        }
    }

    output
}

fn render_memory_bundle_validation(
    validation: &crate::server::routes::MemoryBundleValidation,
) -> String {
    use std::fmt::Write as _;

    let mut output = render_memory_bundle_preview(&validation.preview);
    let _ = writeln!(output);
    let _ = writeln!(output, "Validation: {}", validation.valid);
    if validation.errors.is_empty() {
        let _ = writeln!(output, "- no errors");
    } else {
        for error in &validation.errors {
            let _ = writeln!(output, "- {}", error);
        }
    }
    if let Some(trust) = &validation.trust {
        let status = serde_json::to_value(trust.status)
            .ok()
            .and_then(|value| value.as_str().map(str::to_string))
            .unwrap_or_else(|| "unknown".to_string());
        let _ = writeln!(
            output,
            "Trust:      status={} verified={} reason={}",
            status, trust.verified, trust.reason
        );
        if let Some(origin) = &trust.origin {
            let rendered = serde_json::to_value(origin)
                .ok()
                .and_then(|value| value.as_str().map(str::to_string))
                .unwrap_or_else(|| "unknown".to_string());
            let _ = writeln!(output, "Origin:     {}", rendered);
        }
        if let Some(catalog_id) = &trust.catalog_id {
            let _ = writeln!(output, "Catalog:    {}", catalog_id);
        }
        if let Some(replacement) = &trust.replacement_identity {
            let _ = writeln!(output, "Replacement: {}", replacement);
        }
    }
    output
}

fn render_memory_bundle_diff(diff: &crate::server::routes::MemoryBundleDiff) -> String {
    use std::fmt::Write as _;

    let mut output = String::new();
    let _ = writeln!(output, "Memory bundle diff");
    let _ = writeln!(output, "Kinds match: {}", diff.same_kind);
    let _ = writeln!(output, "Shared entries: {}", diff.shared_preview_count);
    if diff.changed_fields.is_empty() {
        let _ = writeln!(output, "Changed fields: none");
    } else {
        let _ = writeln!(output, "Changed fields: {}", diff.changed_fields.join(", "));
    }
    let _ = writeln!(output);
    let _ = writeln!(output, "Left");
    let _ = writeln!(output, "  {} {}", diff.left.kind, diff.left.uri);
    let _ = writeln!(
        output,
        "  memories={} visible={}",
        diff.left.memory_count, diff.left.visible_count
    );
    let _ = writeln!(output, "Right");
    let _ = writeln!(output, "  {} {}", diff.right.kind, diff.right.uri);
    let _ = writeln!(
        output,
        "  memories={} visible={}",
        diff.right.memory_count, diff.right.visible_count
    );
    let _ = writeln!(output);
    let _ = writeln!(output, "Added");
    if diff.added_preview.is_empty() {
        let _ = writeln!(output, "- none");
    } else {
        for item in &diff.added_preview {
            let _ = writeln!(output, "- {}", item);
        }
    }
    let _ = writeln!(output);
    let _ = writeln!(output, "Removed");
    if diff.removed_preview.is_empty() {
        let _ = writeln!(output, "- none");
    } else {
        for item in &diff.removed_preview {
            let _ = writeln!(output, "- {}", item);
        }
    }
    output
}

fn run_bundle_export(
    config: &config::Config,
    kind: crate::server::routes::MemoryBundleKind,
    namespace_override: Option<&str>,
    scope: &str,
    history_id: Option<&str>,
    output: Option<PathBuf>,
) -> anyhow::Result<()> {
    let doc_engine = open_operator_doc_engine(config)?;
    let namespace =
        resolve_operator_namespace(config, &doc_engine, namespace_override, "bundle export")?;
    let memories = doc_engine.list_all_including_archived(&namespace)?;
    let bundle = match kind {
        crate::server::routes::MemoryBundleKind::Passport => {
            let scope = scope
                .parse::<crate::memory::PassportScope>()
                .map_err(anyhow::Error::msg)?;
            crate::server::routes::build_memory_bundle_from_passport(
                crate::memory::build_memory_passport_bundle(&namespace, scope, &memories),
            )
        }
        crate::server::routes::MemoryBundleKind::History => {
            let id =
                history_id.ok_or_else(|| anyhow::anyhow!("history bundle export requires --id"))?;
            let id: uuid::Uuid = id
                .parse()
                .map_err(|_| anyhow::anyhow!("invalid UUID: {id}"))?;
            let bundle = crate::memory::build_memory_history_bundle(&namespace, id, &memories)
                .ok_or_else(|| {
                    anyhow::anyhow!("history root not found in namespace {namespace}")
                })?;
            crate::server::routes::build_memory_bundle_from_history(bundle)
        }
    };
    let registry = crate::security::trust::PortableTrustRegistry::open(&config.storage.data_dir)?;
    let mut bundle = bundle;
    if bundle.signature.signable {
        crate::server::routes::sign_memory_bundle(
            &mut bundle,
            &registry,
            config.auth.audit_hmac_secret.as_bytes(),
            "device:local-runtime",
        )?;
    }
    let output_path =
        output.unwrap_or_else(|| PathBuf::from(bundle.reference.attachment_name.clone()));
    let preview =
        crate::server::routes::preview_memory_bundle(&bundle).map_err(anyhow::Error::msg)?;
    std::fs::write(&output_path, serde_json::to_vec_pretty(&bundle)?)?;
    println!(
        "Exported memory bundle {} [{} memories={} uri={}]",
        output_path.display(),
        kind,
        preview.memory_count,
        preview.uri
    );
    Ok(())
}

fn run_bundle_preview(path: &Path) -> anyhow::Result<()> {
    let bundle = read_memory_bundle(path)?;
    let preview =
        crate::server::routes::preview_memory_bundle(&bundle).map_err(anyhow::Error::msg)?;
    print!("{}", render_memory_bundle_preview(&preview));
    Ok(())
}

fn run_bundle_validate(path: &Path, config_path: &Path) -> anyhow::Result<()> {
    let bundle = read_memory_bundle(path)?;
    let mut validation = crate::server::routes::validate_memory_bundle(&bundle);
    if let Some(context) = load_reader_trust_context(config_path) {
        validation.trust = Some(crate::server::routes::verify_memory_bundle_signature(
            &bundle,
            &context.registry,
            &context.secret,
        ));
    }
    print!("{}", render_memory_bundle_validation(&validation));
    if !validation.valid {
        anyhow::bail!("memory bundle validation failed");
    }
    Ok(())
}

fn run_bundle_diff(left: &Path, right: &Path) -> anyhow::Result<()> {
    let left_bundle = read_memory_bundle(left)?;
    let right_bundle = read_memory_bundle(right)?;
    let diff = crate::server::routes::diff_memory_bundles(&left_bundle, &right_bundle)
        .map_err(anyhow::Error::msg)?;
    print!("{}", render_memory_bundle_diff(&diff));
    Ok(())
}

fn run_conformance_normalize(
    kind: ConformanceArtifactKind,
    input: &Path,
    output: &Path,
) -> anyhow::Result<()> {
    let bytes = std::fs::read(input)?;
    let normalized = match kind {
        ConformanceArtifactKind::RuntimeContract => {
            let artifact: crate::memory::RuntimeContractDocument = serde_json::from_slice(&bytes)?;
            serde_json::to_vec_pretty(&artifact)?
        }
        ConformanceArtifactKind::Passport => {
            let artifact: crate::memory::MemoryPassportBundle = serde_json::from_slice(&bytes)?;
            if !crate::memory::verify_memory_passport_bundle(&artifact) {
                anyhow::bail!("passport fixture integrity check failed");
            }
            serde_json::to_vec_pretty(&artifact)?
        }
        ConformanceArtifactKind::History => {
            let artifact: crate::memory::MemoryHistoryBundle = serde_json::from_slice(&bytes)?;
            if !crate::memory::verify_memory_history_bundle(&artifact) {
                anyhow::bail!("history fixture integrity check failed");
            }
            serde_json::to_vec_pretty(&artifact)?
        }
    };
    std::fs::write(output, normalized)?;
    Ok(())
}

fn run_doctor(config: &config::Config, config_path: &Path, repair: bool) -> anyhow::Result<()> {
    let mut issues = 0usize;
    println!("Running doctor for {}", config_path.display());
    println!("[ok] config: loaded and validated");
    let home_dir = home_dir_path();
    let detected_claude = is_claude_installed(&home_dir);
    let detected_codex = is_codex_installed(&home_dir);
    let detected_cursor = is_cursor_installed(&home_dir);
    let setup_profile = config.setup.profile;
    let expect_claude = profile_configures_claude(setup_profile, detected_claude);
    let expect_codex = profile_configures_codex(setup_profile, detected_codex);
    let expect_cursor = profile_configures_cursor(setup_profile, detected_cursor);
    let config_path_abs =
        std::fs::canonicalize(config_path).unwrap_or_else(|_| config_path.to_path_buf());
    let memoryoss_bin = preferred_runtime_binary();
    println!("[ok] setup profile: {}", setup_profile);
    println!(
        "[ok] embeddings: model={} dimension={}",
        config.embeddings.model,
        config.embeddings.model.expected_dimension()
    );
    if repair {
        println!(
            "[ok] repair mode: managed client/team drift will be repaired before final checks"
        );
    }

    let mut configured_clients = Vec::new();
    if expect_claude {
        configured_clients.push("claude");
    }
    if expect_codex {
        configured_clients.push("codex");
    }
    if expect_cursor {
        configured_clients.push("cursor");
    }

    if repair {
        let bind_host = config.server.host.as_str();
        let port = config.server.port.to_string();
        if expect_claude
            && (!claude_user_mcp_matches(&home_dir, &memoryoss_bin, &config_path_abs)
                || !claude_legacy_mcp_matches(&home_dir, &memoryoss_bin, &config_path_abs)
                || !claude_hooks_match(&home_dir))
        {
            match configure_claude_integration(
                &home_dir,
                &memoryoss_bin,
                &config_path_abs,
                bind_host,
                &port,
            ) {
                Ok(()) => println!("[repair] claude integration refreshed"),
                Err(err) => eprintln!("[repair] failed to refresh claude integration: {err}"),
            }
        }
        if expect_codex
            && (!codex_mcp_matches(&home_dir, &memoryoss_bin, &config_path_abs)
                || !codex_policy_matches(&home_dir))
        {
            match configure_codex_integration(&home_dir, &memoryoss_bin, &config_path_abs) {
                Ok(()) => println!("[repair] codex integration refreshed"),
                Err(err) => eprintln!("[repair] failed to refresh codex integration: {err}"),
            }
        }
        if expect_cursor
            && (!cursor_mcp_matches(&home_dir, &memoryoss_bin, &config_path_abs)
                || !cursor_rules_match(&home_dir))
        {
            match configure_cursor_integration(&home_dir, &memoryoss_bin, &config_path_abs) {
                Ok(()) => println!("[repair] cursor integration refreshed"),
                Err(err) => eprintln!("[repair] failed to refresh cursor integration: {err}"),
            }
        }
        if let Some(manifest_path) = config
            .setup
            .team_manifest_path
            .as_deref()
            .map(std::path::PathBuf::from)
        {
            let manifest = read_team_bootstrap_manifest(&manifest_path)?;
            let summary = apply_team_bootstrap_manifest(
                &manifest,
                &manifest_path,
                config,
                &home_dir,
                setup_profile,
                &configured_clients,
            )?;
            println!(
                "[repair] team trust catalog refreshed: {} identities={} revocations={}",
                summary.catalog.catalog_id,
                summary.imported_identities,
                summary.imported_revocations
            );
        }
    }

    let admin_keys = config
        .auth
        .api_keys
        .iter()
        .filter(|entry| entry.role == crate::config::Role::Admin)
        .count();
    if admin_keys == 0 {
        println!("[error] auth: no admin API key configured");
        issues += 1;
    } else {
        println!("[ok] auth: {admin_keys} admin key(s) configured");
    }

    let doc_engine = match open_operator_doc_engine(config) {
        Ok(engine) => engine,
        Err(err) => {
            println!(
                "[error] database: failed to open {} ({err})",
                config.storage.data_dir.display()
            );
            anyhow::bail!("doctor found 1 issue(s)");
        }
    };
    let stored_namespaces = doc_engine.list_namespaces()?;
    println!(
        "[ok] database: opened {} ({} namespace table(s))",
        config.storage.data_dir.display(),
        stored_namespaces.len()
    );

    let namespace_memories = collect_namespace_memories(config, &doc_engine, None)?;
    let index_health = snapshot_local_index_health(config, &doc_engine, &namespace_memories)?;
    if index_health.pending_outbox > 0 {
        println!(
            "[error] index: pending_outbox={} (checkpoint={} status={})",
            index_health.pending_outbox, index_health.checkpoint, index_health.status
        );
        issues += 1;
    } else {
        println!(
            "[ok] index: checkpoint={} pending_outbox=0",
            index_health.checkpoint
        );
    }

    if namespace_memories
        .iter()
        .flat_map(|(_, memories)| memories.iter())
        .any(|memory| !memory.archived)
        && !index_health.fts_dir_exists
    {
        println!("[error] index: missing fts directory");
        issues += 1;
    } else {
        println!(
            "[ok] fts: {}",
            if index_health.fts_dir_exists {
                "present"
            } else {
                "not yet materialized"
            }
        );
    }

    if index_health.embedded_memories > 0 && !index_health.vector_index_exists {
        println!(
            "[ok] vector index: {} embedded memory/memories will be rebuilt from redb on startup",
            index_health.embedded_memories
        );
    } else {
        println!(
            "[ok] vector index: {}",
            if index_health.vector_index_exists {
                "present"
            } else {
                "not yet materialized"
            }
        );
    }

    if index_health.embedding_dimension_mismatches > 0 {
        println!(
            "[error] embeddings: {} stored embedding(s) do not match configured dimension {} (run `memoryoss migrate-embeddings --model {}` and then `memoryoss serve`)",
            index_health.embedding_dimension_mismatches,
            config.embeddings.model.expected_dimension(),
            config.embeddings.model
        );
        issues += 1;
    } else {
        println!(
            "[ok] embeddings: stored vectors match configured dimension {}",
            config.embeddings.model.expected_dimension()
        );
    }

    if index_health.embedded_memories > 0 && !index_health.vector_mapping_exists {
        println!(
            "[ok] vector mappings: {} embedded memory/memories will rebuild mappings from redb on startup",
            index_health.embedded_memories
        );
    } else {
        println!(
            "[ok] vector mappings: {}",
            if index_health.vector_mapping_exists {
                "present"
            } else {
                "not required"
            }
        );
    }

    if let Some(team_catalog_id) = config.setup.team_catalog_id.as_deref() {
        let registry =
            crate::security::trust::PortableTrustRegistry::open(&config.storage.data_dir)?;
        let snapshot = registry.snapshot();
        if snapshot
            .catalogs
            .iter()
            .any(|catalog| catalog.catalog_id == team_catalog_id)
        {
            println!(
                "[ok] team trust catalog: {} imported into {}",
                team_catalog_id,
                config.storage.data_dir.display()
            );
        } else {
            println!(
                "[error] team trust catalog: missing imported catalog {}",
                team_catalog_id
            );
            issues += 1;
        }
    }

    if expect_claude {
        if claude_user_mcp_matches(&home_dir, &memoryoss_bin, &config_path_abs)
            && claude_legacy_mcp_matches(&home_dir, &memoryoss_bin, &config_path_abs)
        {
            println!(
                "[ok] claude mcp: user and compatibility registrations point to {}",
                config_path_abs.display()
            );
        } else {
            println!(
                "[error] claude mcp: missing or stale registration in {} or {}",
                claude_user_config_path(&home_dir).display(),
                claude_settings_path(&home_dir).display()
            );
            issues += 1;
        }

        if claude_hooks_match(&home_dir) {
            println!(
                "[ok] claude hooks: required events point to {}",
                claude_guard_script_path(&home_dir).display()
            );
        } else {
            println!(
                "[error] claude hooks: missing required events in {} or missing {}",
                claude_settings_local_path(&home_dir).display(),
                claude_guard_script_path(&home_dir).display()
            );
            issues += 1;
        }
    } else if detected_claude {
        println!("[ok] claude integration: present but not selected by setup profile");
    } else {
        println!("[ok] claude integration: not detected");
    }

    if expect_codex {
        if codex_mcp_matches(&home_dir, &memoryoss_bin, &config_path_abs) {
            println!(
                "[ok] codex mcp: config points to {}",
                config_path_abs.display()
            );
        } else {
            println!(
                "[error] codex mcp: missing or stale {}",
                codex_config_path(&home_dir).display()
            );
            issues += 1;
        }

        if codex_policy_matches(&home_dir) {
            println!(
                "[ok] codex policy: {} contains the memoryOSS policy block",
                agents_policy_path(&home_dir).display()
            );
        } else {
            println!(
                "[error] codex policy: {} is missing the memoryOSS policy block",
                agents_policy_path(&home_dir).display()
            );
            issues += 1;
        }
    } else if detected_codex {
        println!("[ok] codex integration: present but not selected by setup profile");
    } else {
        println!("[ok] codex integration: not detected");
    }

    if expect_cursor {
        if cursor_mcp_matches(&home_dir, &memoryoss_bin, &config_path_abs) {
            println!(
                "[ok] cursor mcp: config points to {}",
                config_path_abs.display()
            );
        } else {
            println!(
                "[error] cursor mcp: missing or stale {} (run `memoryoss setup --profile cursor`)",
                cursor_mcp_path(&home_dir).display()
            );
            issues += 1;
        }

        if cursor_rules_match(&home_dir) {
            println!(
                "[ok] cursor rules: managed runtime rule present at {}",
                cursor_rule_path(&home_dir).display()
            );
        } else {
            println!(
                "[error] cursor rules: missing or stale {} (run `memoryoss setup --profile cursor`)",
                cursor_rule_path(&home_dir).display()
            );
            issues += 1;
        }
    } else if detected_cursor {
        println!("[ok] cursor integration: present but not selected by setup profile");
    } else {
        println!("[ok] cursor integration: not detected");
    }

    if issues > 0 {
        println!("Doctor FAILED: {issues} issue(s)");
        anyhow::bail!("doctor found {issues} issue(s)");
    }

    println!("Doctor OK");
    Ok(())
}

async fn run_setup_wizard(
    config_path: &std::path::Path,
    profile: crate::config::SetupProfile,
    team_manifest: Option<&std::path::Path>,
) -> anyhow::Result<()> {
    println!();
    println!("╔════════════════════════════════════════════════════╗");
    println!("║  memoryOSS — Setup                                ║");
    println!("║  The Open Source Memory Layer for AI Agents        ║");
    println!("╚════════════════════════════════════════════════════╝");
    println!();

    // Check for existing config — offer reconfigure
    if config_path.exists() {
        println!("  Existing config found: {}", config_path.display());
        println!("    1) Keep current config and exit");
        println!("    2) Reconfigure from scratch");
        let choice = prompt_choice("  Choose: ", &["keep", "reconfigure"], 0);
        if choice == 0 {
            println!("  Keeping existing config.");
            return Ok(());
        }
        println!();
    }

    // --- Auto-detect installed tools ---
    let has_claude_code = std::env::var("HOME")
        .ok()
        .map(|h| std::path::Path::new(&h).join(".claude").exists())
        .unwrap_or(false)
        || std::process::Command::new("which")
            .arg("claude")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);

    let has_codex = std::env::var("CODEX_HOME").ok().is_some()
        || std::env::var("HOME")
            .ok()
            .map(|h| std::path::Path::new(&h).join(".codex").exists())
            .unwrap_or(false)
        || std::process::Command::new("which")
            .arg("codex")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);

    let has_cursor = std::env::var("HOME")
        .ok()
        .map(|h| std::path::Path::new(&h).join(".cursor").exists())
        .unwrap_or(false)
        || std::process::Command::new("which")
            .arg("cursor")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
        || std::process::Command::new("which")
            .arg("cursor-agent")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);

    let configure_claude = profile_configures_claude(profile, has_claude_code);
    let configure_codex = profile_configures_codex(profile, has_codex);
    let configure_cursor = profile_configures_cursor(profile, has_cursor);
    let team_manifest_data = match team_manifest {
        Some(path) => Some(read_team_bootstrap_manifest(path)?),
        None => None,
    };

    let home_dir = std::env::var("HOME").unwrap_or_default();
    let env_openai = std::env::var("OPENAI_API_KEY")
        .ok()
        .filter(|k| k.starts_with("sk-") && k.len() > 10);
    let env_anthropic = std::env::var("ANTHROPIC_API_KEY")
        .ok()
        .filter(|k| k.starts_with("sk-ant-") && k.len() > 10);
    let shell_openai = shell_config_has_var(&home_dir, "OPENAI_API_KEY");
    let shell_anthropic = shell_config_has_var(&home_dir, "ANTHROPIC_API_KEY");

    // Read credential files for detailed detection
    // Detect auth method per tool: API key vs OAuth
    // Codex: check auth.json for auth_mode
    let codex_home = std::env::var("CODEX_HOME")
        .ok()
        .unwrap_or_else(|| format!("{}/.codex", home_dir));
    let codex_auth_path = std::path::Path::new(&codex_home).join("auth.json");
    let codex_auth: Option<serde_json::Value> = std::fs::read_to_string(&codex_auth_path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok());
    let codex_has_oauth = codex_auth
        .as_ref()
        .map(|v| v.get("tokens").is_some())
        .unwrap_or(false);
    let codex_has_api_key_in_file = codex_auth
        .as_ref()
        .and_then(|v| v.get("OPENAI_API_KEY"))
        .and_then(|v| v.as_str())
        .filter(|k| !k.is_empty())
        .is_some();
    // Combined: API key available from any source
    let has_any_openai_key = env_openai.is_some() || shell_openai || codex_has_api_key_in_file;

    // Claude Code: check for OAuth login
    let claude_dir = std::path::Path::new(&home_dir).join(".claude");
    let claude_creds_path = if claude_dir.join(".credentials.json").exists() {
        Some(claude_dir.join(".credentials.json"))
    } else if claude_dir.join("credentials.json").exists() {
        Some(claude_dir.join("credentials.json"))
    } else {
        None
    };
    let claude_creds: Option<serde_json::Value> = claude_creds_path
        .as_ref()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok());
    let claude_has_oauth = has_claude_code
        && claude_creds
            .as_ref()
            .map(|v| v.get("claudeAiOauth").is_some())
            .unwrap_or(false);
    let claude_subscription = claude_creds
        .as_ref()
        .and_then(|v| v.get("claudeAiOauth"))
        .and_then(|v| v.get("subscriptionType"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let claude_has_api_key = env_anthropic.is_some() || shell_anthropic;
    let codex_has_both = has_codex && codex_has_oauth && has_any_openai_key;

    // Show what was found first
    println!("  Selected profile: {}", profile);
    println!("  Detected:");
    if has_claude_code {
        if claude_has_oauth && claude_has_api_key {
            println!("    ✓ Claude Code (OAuth + API key)");
        } else if claude_has_oauth {
            let sub = claude_subscription.as_deref().unwrap_or("active");
            println!("    ✓ Claude Code (OAuth — {sub})");
        } else if claude_has_api_key {
            println!("    ✓ Claude Code (API key)");
        } else {
            println!("    ✓ Claude Code");
        }
    }
    if has_codex {
        if codex_has_both {
            println!("    ✓ Codex CLI (OAuth + API key)");
        } else if codex_has_oauth {
            println!("    ✓ Codex CLI (OAuth)");
        } else if has_any_openai_key {
            println!("    ✓ Codex CLI (API key)");
        } else {
            println!("    ✓ Codex CLI");
        }
    }
    if has_cursor {
        println!("    ✓ Cursor");
    }
    if has_any_openai_key && !has_codex {
        println!("    ✓ OPENAI_API_KEY (configured)");
    }
    if claude_has_api_key && !has_claude_code {
        println!("    ✓ ANTHROPIC_API_KEY (configured)");
    }
    if matches!(profile, crate::config::SetupProfile::Auto)
        && !has_claude_code
        && !has_codex
        && !has_cursor
        && env_openai.is_none()
        && env_anthropic.is_none()
    {
        println!("    (no AI tools found — will configure for manual use)");
    }
    if !matches!(profile, crate::config::SetupProfile::Auto)
        && !has_claude_code
        && !has_codex
        && !has_cursor
    {
        println!(
            "    (explicit profile selected — setup will write the chosen client surfaces even if the tool is not yet installed)"
        );
    }
    println!();
    println!("  Profile targets:");
    println!(
        "    - Claude: {}",
        if configure_claude {
            "configure"
        } else {
            "skip"
        }
    );
    println!(
        "    - Codex: {}",
        if configure_codex { "configure" } else { "skip" }
    );
    println!(
        "    - Cursor: {}",
        if configure_cursor {
            "configure"
        } else {
            "skip"
        }
    );
    println!(
        "    - Shell proxy exports: {}",
        if profile_enables_shell_proxy_exports(profile) {
            "enabled when auth is proxy-safe"
        } else {
            "disabled for this profile"
        }
    );
    if let Some(manifest) = &team_manifest_data {
        println!(
            "    - Team bootstrap: {} ({}) via {}",
            manifest.team_label,
            manifest.catalog.catalog_id,
            team_manifest
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "inline".to_string())
        );
    }

    // Then ask auth choices where both methods are available
    let claude_uses_api_key =
        if configure_claude && has_claude_code && claude_has_oauth && claude_has_api_key {
            println!();
            println!("  Auth method for Claude Code:");
            println!("    1) Subscription (OAuth) — no extra cost (recommended)");
            println!("    2) API key — pay per token");
            let auth_choice = prompt_choice("  Choose: ", &["oauth", "apikey"], 0);
            auth_choice == 1
        } else {
            configure_claude && claude_has_api_key && !claude_has_oauth
        };

    let codex_uses_api_key = if configure_codex && codex_has_both {
        println!();
        println!("  Auth method for Codex CLI:");
        println!("    1) Subscription (OAuth) — no extra cost (recommended)");
        println!("    2) API key — pay per token");
        let auth_choice = prompt_choice("  Choose: ", &["oauth", "apikey"], 0);
        auth_choice == 1
    } else {
        configure_codex && has_any_openai_key && !codex_has_oauth
    };
    println!();
    if configure_codex && has_codex && codex_has_oauth && !codex_uses_api_key {
        println!(
            "  Note: Codex OAuth uses MCP by default. Proxy mode for Codex requires an OpenAI API key."
        );
        println!();
    }

    // Determine data_dir early so we can check for existing data
    let data_dir = std::env::var("HOME")
        .ok()
        .map(|h| format!("{}/.memoryoss/data", h))
        .unwrap_or_else(|| "data".to_string());

    // Memory mode: only ask if existing data found, otherwise default to full
    let has_existing_data = std::path::Path::new(&data_dir)
        .join("memoryoss.redb")
        .exists();
    let memory_mode_str = if has_existing_data {
        println!("  Existing memories detected in {}", data_dir);
        println!("  Memory mode:");
        println!("    1) Full — store & recall automatically (recommended)");
        println!("    2) Read-only — recall only, don't store");
        println!("    3) Write-only — store only, don't recall");
        let choice = prompt_choice("  Choose: ", &["full", "readonly", "writeonly"], 0);
        match choice {
            0 => "full",
            1 => "readonly",
            _ => "writeonly",
        }
    } else {
        println!("  Memory mode: Full (store & recall)");
        "full"
    };

    // Noise cleanup: default 14 days, no question needed
    let (decay_enabled, decay_days) = (true, 14);
    let (consolidation_enabled, consolidation_interval_minutes, consolidation_threshold) =
        (true, 60, 0.9);

    // Everything else is auto-configured
    let bind_host = "127.0.0.1";
    let port = std::env::var("MEMORYOSS_PORT").unwrap_or_else(|_| "8000".to_string());
    let core_port = std::env::var("MEMORYOSS_CORE_PORT").unwrap_or_else(|_| {
        port.parse::<u16>()
            .ok()
            .and_then(|p| p.checked_add(1))
            .unwrap_or(8001)
            .to_string()
    });

    // Generate internal API key (for admin/MCP, not for the user to configure)
    let mut key_bytes = [0u8; 32];
    rand::Rng::fill(&mut rand::thread_rng(), &mut key_bytes);
    let api_key: String = format!("ek_{}", hex::encode(key_bytes));

    let min_recall_score = if memory_mode_str == "writeonly" {
        "0.40"
    } else {
        "0.35"
    };

    // Generate a persistent jwt_secret so JWTs survive restarts
    let mut jwt_bytes = [0u8; 32];
    rand::Rng::fill(&mut rand::thread_rng(), &mut jwt_bytes);
    let jwt_secret = hex::encode(jwt_bytes);
    let mut audit_hmac_bytes = [0u8; 32];
    rand::Rng::fill(&mut rand::thread_rng(), &mut audit_hmac_bytes);
    let audit_hmac_secret = hex::encode(audit_hmac_bytes);

    std::fs::create_dir_all(&data_dir).ok();

    // Only enable extraction when a real provider credential is available.
    // OAuth login alone is enough for passthrough traffic, but not for reliable
    // background extraction calls.
    let extract_provider = if has_any_openai_key {
        "openai"
    } else if claude_has_api_key {
        "claude"
    } else {
        "openai"
    };
    let extract_model = match extract_provider {
        "claude" => "claude-haiku-4-5-20251001",
        "ollama" => "llama3.1",
        _ => "gpt-4o-mini",
    };
    let extraction_enabled = if has_any_openai_key || claude_has_api_key {
        "true"
    } else {
        "false"
    };

    // --- Generate config: pure passthrough, no keys needed ---
    let mut setup_lines = vec![format!("profile = \"{}\"", profile)];
    if let Some(manifest) = &team_manifest_data {
        setup_lines.push(format!("team_id = {:?}", manifest.team_id));
        setup_lines.push(format!("team_label = {:?}", manifest.team_label));
        setup_lines.push(format!(
            "team_catalog_id = {:?}",
            manifest.catalog.catalog_id
        ));
        if let Some(path) = team_manifest {
            setup_lines.push(format!(
                "team_manifest_path = {:?}",
                path.display().to_string()
            ));
        }
    }
    let setup_section = setup_lines.join("\n");

    let config_toml = format!(
        r#"# memoryOSS — auto-generated by setup
# {timestamp}
#
# Zero-config: your existing auth (API keys, OAuth tokens) passes through
# automatically. Extraction only turns on when a real provider API key is available.

[server]
host = "{bind_host}"
port = {port}
hybrid_mode = true
core_port = {core_port}

[tls]
enabled = false
# The setup wizard runs in dev mode (HTTP) and leaves TLS disabled by default.
# For HTTPS, set enabled = true and auto_generate = true (or provide cert/key paths).
auto_generate = false

[auth]
jwt_secret = "{jwt_secret}"
jwt_expiry_secs = 3600
audit_hmac_secret = "{audit_hmac_secret}"

[[auth.api_keys]]
key = "{api_key}"
role = "admin"
namespace = "default"

[storage]
data_dir = "{data_dir}"

[proxy]
enabled = true
passthrough_auth = true
passthrough_local_only = true
upstream_url = "https://api.openai.com/v1"
default_memory_mode = "{memory_mode_str}"
min_recall_score = {min_recall_score}
extraction_enabled = {extraction_enabled}
extract_model = "{extract_model}"
extract_provider = "{extract_provider}"

[[proxy.key_mapping]]
proxy_key = "{api_key}"
namespace = "default"

[setup]
{setup_section}

[logging]
level = "info"
json = false

[decay]
enabled = {decay_enabled}
strategy = "age"
after_days = {decay_days}

[consolidation]
enabled = {consolidation_enabled}
interval_minutes = {consolidation_interval_minutes}
threshold = {consolidation_threshold}
max_clusters = 25
"#,
        timestamp = chrono::Utc::now().format("%Y-%m-%d %H:%M UTC"),
        audit_hmac_secret = audit_hmac_secret,
        extract_model = extract_model,
        extract_provider = extract_provider,
        setup_section = setup_section,
        core_port = core_port,
        consolidation_enabled = consolidation_enabled,
        consolidation_interval_minutes = consolidation_interval_minutes,
        consolidation_threshold = consolidation_threshold,
    );

    // Validate generated TOML before writing
    if let Err(e) = config_toml.parse::<toml::Table>() {
        eprintln!("  ✗ Generated config is invalid TOML: {e}");
        return Err(anyhow::anyhow!("invalid generated TOML: {e}"));
    }

    std::fs::write(config_path, &config_toml)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(config_path, std::fs::Permissions::from_mode(0o600));
    }
    println!("  ✓ Config written: {}", config_path.display());

    // --- Shell integration ---
    //
    // Hybrid mode: keep MCP registered for explicit memory tools, and also point
    // supported clients at the local gateway so transparent memory works when the
    // core is healthy. The gateway itself fail-opens to direct upstream passthrough
    // if the memory core is down, so the client does not get locked out.
    {
        let shell = std::env::var("SHELL").unwrap_or_default();
        let rc_file = if shell.contains("zsh") {
            ".zshrc"
        } else {
            ".bashrc"
        };
        let bashrc_path = std::env::var("HOME")
            .ok()
            .map(|h| std::path::PathBuf::from(h).join(rc_file))
            .unwrap_or_else(|| std::path::PathBuf::from(rc_file));

        let existing = std::fs::read_to_string(&bashrc_path).unwrap_or_default();
        let block_start = "# >>> memoryOSS proxy >>>";
        let block_end = "# <<< memoryOSS proxy <<<";
        let mut cleaned_lines = Vec::new();
        let mut in_old_block = false;
        for line in existing.lines() {
            let trimmed = line.trim();
            if trimmed == block_start {
                in_old_block = true;
                continue;
            }
            if trimmed == block_end {
                in_old_block = false;
                continue;
            }
            if in_old_block {
                continue;
            }
            if trimmed.contains("OPENAI_BASE_URL") || trimmed.contains("ANTHROPIC_BASE_URL") {
                continue;
            }
            cleaned_lines.push(line.to_string());
        }

        // Clean up old memoryOSS shell banner if present — status is shown via MCP, not the shell
        let cleaned_lines: Vec<String> = cleaned_lines
            .into_iter()
            .filter(|line| {
                let trimmed = line.trim();
                !trimmed.contains("memoryOSS")
                    && !trimmed.contains("__memoryoss_init")
                    && !trimmed.contains("__mossans")
            })
            .collect();

        let claude_proxy_safe = profile_enables_shell_proxy_exports(profile)
            && claude_has_api_key
            && (!claude_has_oauth || claude_uses_api_key)
            && (matches!(profile, crate::config::SetupProfile::Auto) || configure_claude);
        let codex_proxy_safe = profile_enables_shell_proxy_exports(profile)
            && has_any_openai_key
            && (!codex_has_oauth || codex_uses_api_key)
            && (matches!(profile, crate::config::SetupProfile::Auto) || configure_codex);

        let mut proxy_exports = Vec::new();
        if codex_proxy_safe {
            proxy_exports.push(format!(
                "export OPENAI_BASE_URL=http://{}:{}/proxy/v1",
                bind_host, port
            ));
        }
        if claude_proxy_safe {
            proxy_exports.push(format!(
                "export ANTHROPIC_BASE_URL=http://{}:{}/proxy/anthropic/v1",
                bind_host, port
            ));
        }

        let mut new_shell = cleaned_lines.join("\n");
        new_shell = new_shell.trim_end().to_string();
        if !proxy_exports.is_empty() {
            if !new_shell.is_empty() {
                new_shell.push_str("\n\n");
            }
            new_shell.push_str(block_start);
            new_shell.push('\n');
            new_shell.push_str("# memoryOSS hybrid mode: transparent proxy + MCP fallback");
            new_shell.push('\n');
            for export in &proxy_exports {
                new_shell.push_str(export);
                new_shell.push('\n');
            }
            new_shell.push_str(block_end);
            new_shell.push('\n');
        } else if !new_shell.is_empty() {
            new_shell.push('\n');
        }

        if new_shell != existing {
            match std::fs::write(&bashrc_path, &new_shell) {
                Ok(_) => println!("  ✓ Environment configured in {}", bashrc_path.display()),
                Err(e) => eprintln!(
                    "  ✗ Failed to update shell configuration {}: {e}",
                    bashrc_path.display()
                ),
            }
        }
    }

    // --- Client integration ---
    // Setup must leave Claude and Codex in a deterministic enforced state instead of
    // relying on client-side CLI side effects.
    let config_path_abs =
        std::fs::canonicalize(config_path).unwrap_or_else(|_| config_path.to_path_buf());
    let memoryoss_bin = preferred_runtime_binary();

    if configure_claude {
        match configure_claude_integration(
            &home_dir_path(),
            &memoryoss_bin,
            &config_path_abs,
            bind_host,
            &port,
        ) {
            Ok(()) => {
                println!("  ✓ Claude Code MCP configured (user + compatibility scope)");
                println!("  ✓ Claude Code hooks enforced (recall before tools, store before stop)");
            }
            Err(e) => eprintln!("  ✗ Failed to configure Claude Code integration: {e}"),
        }
    }

    if configure_codex {
        match configure_codex_integration(&home_dir_path(), &memoryoss_bin, &config_path_abs) {
            Ok(()) => {
                println!("  ✓ Codex MCP configured");
                println!("  ✓ Codex policy fallback configured in ~/AGENTS.md");
            }
            Err(e) => eprintln!("  ✗ Failed to configure Codex integration: {e}"),
        }
    }

    if configure_cursor {
        match configure_cursor_integration(&home_dir_path(), &memoryoss_bin, &config_path_abs) {
            Ok(()) => {
                println!("  ✓ Cursor MCP configured");
                println!("  ✓ Cursor config written to ~/.cursor/mcp.json");
                println!("  ✓ Cursor runtime rule written to ~/.cursor/rules/memoryoss.mdc");
            }
            Err(e) => eprintln!("  ✗ Failed to configure Cursor integration: {e}"),
        }
    } else if !matches!(profile, crate::config::SetupProfile::Auto) && has_cursor {
        match remove_cursor_integration(&home_dir_path()) {
            Ok(()) => println!("  ✓ Cursor opt-out removed managed MCP and rule surfaces"),
            Err(e) => eprintln!("  ✗ Failed to prune Cursor integration: {e}"),
        }
    }

    if let (Some(manifest), Some(manifest_path)) = (&team_manifest_data, team_manifest) {
        let generated_config = crate::config::Config::load(config_path)?;
        let mut configured_clients = Vec::new();
        if configure_claude {
            configured_clients.push("claude");
        }
        if configure_codex {
            configured_clients.push("codex");
        }
        if configure_cursor {
            configured_clients.push("cursor");
        }
        match apply_team_bootstrap_manifest(
            manifest,
            manifest_path,
            &generated_config,
            &home_dir_path(),
            profile,
            &configured_clients,
        ) {
            Ok(summary) => {
                println!(
                    "  ✓ Team trust catalog imported: {} (identities={} revocations={})",
                    summary.catalog.catalog_id,
                    summary.imported_identities,
                    summary.imported_revocations
                );
                println!(
                    "  ✓ Team bootstrap receipt written to {}",
                    team_bootstrap_receipt_path(&home_dir_path()).display()
                );
            }
            Err(err) => eprintln!("  ✗ Failed to apply team bootstrap manifest: {err}"),
        }
    }

    // --- Start server as background service ---
    println!();

    let binary = preferred_runtime_binary();
    let config_abs = std::fs::canonicalize(config_path)?;

    // Tests and container-like environments can opt out of systemd installation.
    let disable_systemd = std::env::var("MEMORYOSS_DISABLE_SYSTEMD")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);

    // Try systemd first, fall back to background process
    let started = if cfg!(target_os = "linux") && !disable_systemd {
        install_systemd_service(&binary, &config_abs).is_ok()
    } else {
        false
    };
    let skip_start = std::env::var("MEMORYOSS_SKIP_START")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);

    if skip_start {
        println!("  ✓ memoryOSS server start skipped by MEMORYOSS_SKIP_START");
    } else if !started {
        // Fork a background process
        let child = std::process::Command::new(&binary)
            .arg("-c")
            .arg(&config_abs)
            .arg("serve")
            .stdout({
                let f = std::fs::File::create("/tmp/memoryoss.log")
                    .unwrap_or_else(|_| std::fs::File::open("/dev/null").unwrap());
                f.try_clone()
                    .unwrap_or_else(|_| std::fs::File::open("/dev/null").unwrap())
            })
            .stderr(
                std::fs::OpenOptions::new()
                    .append(true)
                    .open("/tmp/memoryoss.log")
                    .unwrap_or_else(|_| std::fs::File::open("/dev/null").unwrap()),
            )
            .spawn();

        match child {
            Ok(_) => {
                println!("  ✓ memoryOSS server started in background");
                println!("    Logs: /tmp/memoryoss.log");
            }
            Err(e) => {
                eprintln!("  ✗ Could not start server: {e}");
                eprintln!(
                    "    Start manually: memoryoss -c {} serve",
                    config_abs.display()
                );
            }
        }
    }

    println!();
    println!(
        "  Setup done. Active profile: {}. Start your selected client(s) as usual — memory is enforced via MCP and client-side guardrails where configured.",
        profile
    );
    println!();

    Ok(())
}

fn preferred_runtime_binary() -> PathBuf {
    let current = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("memoryoss"));
    let home = std::env::var("HOME").ok();
    let candidates = stable_binary_candidates(home.as_deref());
    choose_preferred_binary(&current, &candidates)
}

fn stable_binary_candidates(home: Option<&str>) -> Vec<PathBuf> {
    let mut candidates = vec![
        PathBuf::from("/usr/local/bin/memoryoss"),
        PathBuf::from("/usr/bin/memoryoss"),
    ];
    if let Some(home) = home {
        candidates.push(PathBuf::from(home).join(".cargo/bin/memoryoss"));
    }
    candidates
}

fn choose_preferred_binary(current: &std::path::Path, candidates: &[PathBuf]) -> PathBuf {
    if !is_target_build_path(current) && is_viable_binary(current) {
        return current.to_path_buf();
    }

    for candidate in candidates {
        if candidate != current && is_viable_binary(candidate) {
            return candidate.clone();
        }
    }

    if is_viable_binary(current) {
        current.to_path_buf()
    } else {
        PathBuf::from("memoryoss")
    }
}

fn is_target_build_path(path: &std::path::Path) -> bool {
    path.components()
        .any(|component| component.as_os_str() == "target")
}

fn is_viable_binary(path: &std::path::Path) -> bool {
    path.is_file()
}

fn install_systemd_service(
    binary: &std::path::Path,
    config_path: &std::path::Path,
) -> anyhow::Result<()> {
    let unit = format!(
        "[Unit]\n\
         Description=memoryOSS — Memory Layer for AI Agents\n\
         After=network.target\n\
         \n\
         [Service]\n\
         Type=simple\n\
         ExecStart=\"{binary}\" -c \"{config}\" serve\n\
         Restart=on-failure\n\
         RestartSec=5\n\
         \n\
         [Install]\n\
         WantedBy=multi-user.target\n",
        binary = binary.display(),
        config = config_path.display(),
    );

    let service_path = std::path::Path::new("/etc/systemd/system/memoryoss.service");

    // Need root for systemd — try writing, fall back if no permission
    if std::fs::write(service_path, &unit).is_err() {
        return Err(anyhow::anyhow!("no permission for systemd"));
    }

    let reload = std::process::Command::new("systemctl")
        .args(["daemon-reload"])
        .status();
    let enable = std::process::Command::new("systemctl")
        .args(["enable", "--now", "memoryoss"])
        .status();

    if reload.map(|s| s.success()).unwrap_or(false) && enable.map(|s| s.success()).unwrap_or(false)
    {
        println!("  ✓ memoryOSS installed as systemd service (auto-starts on boot)");
        println!("    Status: systemctl status memoryoss");
        println!("    Logs:   journalctl -u memoryoss -f");
        Ok(())
    } else {
        // Clean up if it didn't work
        let _ = std::fs::remove_file(service_path);
        Err(anyhow::anyhow!("systemctl failed"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn prefers_current_binary_when_not_in_target_tree() {
        let tmp = tempfile::tempdir().unwrap();
        let current = tmp.path().join("memoryoss");
        fs::write(&current, b"#!/bin/sh\n").unwrap();
        let chosen = choose_preferred_binary(&current, &[]);
        assert_eq!(chosen, current);
    }

    #[test]
    fn prefers_stable_candidate_over_target_build_output() {
        let tmp = tempfile::tempdir().unwrap();
        let current = tmp.path().join("target/release/memoryoss");
        let candidate = tmp.path().join("usr/local/bin/memoryoss");
        fs::create_dir_all(current.parent().unwrap()).unwrap();
        fs::create_dir_all(candidate.parent().unwrap()).unwrap();
        fs::write(&current, b"#!/bin/sh\n").unwrap();
        fs::write(&candidate, b"#!/bin/sh\n").unwrap();
        let chosen = choose_preferred_binary(&current, std::slice::from_ref(&candidate));
        assert_eq!(chosen, candidate);
    }

    #[test]
    fn shell_config_detection_finds_exported_key() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join(".bashrc"),
            "export OPENAI_API_KEY=\"sk-test-shell-openai-key-1234567890\"\n",
        )
        .unwrap();
        assert!(shell_config_has_var(
            tmp.path().to_str().unwrap(),
            "OPENAI_API_KEY"
        ));
    }

    #[test]
    fn shell_config_detection_ignores_commented_or_empty_keys() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join(".bashrc"),
            "# export ANTHROPIC_API_KEY=\"sk-ant-disabled\"\nexport ANTHROPIC_API_KEY=\"\"\n",
        )
        .unwrap();
        assert!(!shell_config_has_var(
            tmp.path().to_str().unwrap(),
            "ANTHROPIC_API_KEY"
        ));
    }

    #[test]
    fn policy_block_upsert_replaces_old_block_once() {
        let existing = format!(
            "before\n{}\nold\n{}\nafter\n",
            MEMORYOSS_POLICY_BEGIN, MEMORYOSS_POLICY_END
        );
        let updated = upsert_policy_block(&existing);
        assert_eq!(updated.matches(MEMORYOSS_POLICY_BEGIN).count(), 1);
        assert!(updated.contains("before"));
        assert!(updated.contains("after"));
        assert!(updated.contains("memoryoss_recall"));
    }

    #[test]
    fn claude_hook_match_requires_all_events() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let guard = claude_guard_script_path(home);
        write_text_file(&guard, "#!/usr/bin/env python3\n", 0o755).unwrap();
        let command = claude_hook_command(&guard);
        let mut settings = serde_json::json!({ "hooks": {} });
        for event in CLAUDE_HOOK_EVENTS {
            settings["hooks"][event] = claude_hook_entry(&command);
        }
        write_json_file(&claude_settings_local_path(home), &settings).unwrap();
        assert!(claude_hooks_match(home));
    }

    #[test]
    fn cursor_rule_match_requires_managed_markers() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        write_text_file(&cursor_rule_path(home), MEMORYOSS_CURSOR_RULE, 0o644).unwrap();
        assert!(cursor_rules_match(home));

        write_text_file(
            &cursor_rule_path(home),
            "# custom rule without memoryOSS markers\n",
            0o644,
        )
        .unwrap();
        assert!(!cursor_rules_match(home));
    }

    #[test]
    fn remove_cursor_integration_prunes_managed_surfaces() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let binary = std::path::Path::new("/usr/local/bin/memoryoss");
        let config = std::path::Path::new("/tmp/memoryoss.toml");
        configure_cursor_integration(home, binary, config).unwrap();
        assert!(cursor_mcp_matches(home, binary, config));
        assert!(cursor_rules_match(home));

        remove_cursor_integration(home).unwrap();
        let config_json = read_json_file(&cursor_mcp_path(home));
        assert!(
            config_json
                .get("mcpServers")
                .and_then(|entry| entry.get("memoryoss"))
                .is_none()
        );
        assert!(!cursor_rule_path(home).exists());
    }

    #[test]
    fn decay_namespace_set_includes_stored_namespaces_not_in_config() {
        let mut config = config::Config::default();
        config.auth.api_keys.push(config::ApiKeyEntry {
            key: "ek_test".to_string(),
            role: crate::config::Role::Admin,
            namespace: "configured".to_string(),
        });

        let namespaces = decay_namespaces(
            &config,
            vec!["stored-only".to_string(), "configured".to_string()],
        );

        assert_eq!(
            namespaces,
            vec![
                "configured".to_string(),
                "default".to_string(),
                "stored-only".to_string()
            ]
        );
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    if cli.license {
        println!("memoryOSS v{}", env!("CARGO_PKG_VERSION"));
        println!("License: GNU Affero General Public License v3.0 (AGPL-3.0-only)");
        println!("Source:  https://github.com/memoryOSScom/memoryoss");
        println!();
        println!("This software is free software: you can redistribute it and/or modify");
        println!("it under the terms of the GNU Affero General Public License as published");
        println!("by the Free Software Foundation, version 3 of the License.");
        println!();
        println!("If you interact with this software over a network, you are entitled to");
        println!("receive the Corresponding Source under AGPL-3.0 Section 13.");
        println!();
        println!("Commercial licenses available: hello@memoryoss.com");
        return Ok(());
    }

    let command = cli.command.unwrap_or(Commands::Serve);
    if let Commands::Conformance { command } = &command {
        match command {
            ConformanceCommands::Normalize {
                kind,
                input,
                output,
            } => {
                let kind = kind
                    .parse::<ConformanceArtifactKind>()
                    .map_err(anyhow::Error::msg)?;
                run_conformance_normalize(kind, input, output)?;
                return Ok(());
            }
        }
    }

    if let Commands::Bundle { command } = &command {
        match command {
            BundleCommands::Preview { path } => {
                run_bundle_preview(path)?;
                return Ok(());
            }
            BundleCommands::Validate { path } => {
                run_bundle_validate(path, &cli.config)?;
                return Ok(());
            }
            BundleCommands::Diff { left, right } => {
                run_bundle_diff(left, right)?;
                return Ok(());
            }
            BundleCommands::Export { .. } => {}
        }
    }

    if let Commands::Reader { command } = &command {
        let trust_context = load_reader_trust_context(&cli.config);
        match command {
            ReaderCommands::Open { path, format } => {
                let format = format.parse::<ReaderFormat>().map_err(anyhow::Error::msg)?;
                run_reader_open(path, format, trust_context.as_ref())?;
                return Ok(());
            }
            ReaderCommands::Diff {
                left,
                right,
                format,
            } => {
                let format = format.parse::<ReaderFormat>().map_err(anyhow::Error::msg)?;
                run_reader_diff(left, right, format, trust_context.as_ref())?;
                return Ok(());
            }
        }
    }

    let operator_command = matches!(
        &command,
        Commands::Status { .. }
            | Commands::Doctor { .. }
            | Commands::Recent { .. }
            | Commands::Hud { .. }
            | Commands::Passport { .. }
            | Commands::Adapter { .. }
            | Commands::Connector { .. }
            | Commands::History { .. }
            | Commands::Bundle {
                command: BundleCommands::Export { .. }
            }
    );

    if operator_command && !cli.config.exists() {
        anyhow::bail!("config not found at '{}'", cli.config.display());
    }

    // Auto-run setup wizard if no config exists and no explicit command given
    if !cli.config.exists() && !matches!(&command, Commands::Setup { .. }) {
        println!();
        println!(
            "  No config found at '{}'. Starting setup wizard...",
            cli.config.display()
        );
        println!();
        run_setup_wizard(&cli.config, crate::config::SetupProfile::Auto, None).await?;
        return Ok(());
    }

    let mut config = config::Config::load(&cli.config)?;

    // Initialize logging — always write to stderr (critical for MCP stdio mode)
    let log_level = if cli.verbose > 0 {
        "debug"
    } else {
        &config.logging.level
    };
    if config.logging.json {
        tracing_subscriber::fmt()
            .json()
            .with_writer(std::io::stderr)
            .with_env_filter(log_level)
            .init();
    } else {
        tracing_subscriber::fmt()
            .with_writer(std::io::stderr)
            .with_env_filter(log_level)
            .init();
    }

    match command {
        Commands::Serve => {
            if config.server.hybrid_mode {
                server::gateway::run_gateway(config, cli.config, true).await?;
            } else {
                server::run(config, cli.config).await?;
            }
        }
        Commands::ServeCore => {
            server::run_core(config, cli.config).await?;
        }
        Commands::ServeGateway => {
            server::gateway::run_gateway(config, cli.config, false).await?;
        }
        Commands::Migrate { dry_run } => {
            std::fs::create_dir_all(&config.storage.data_dir)?;
            let db_path = config.storage.data_dir.join("memoryoss.redb");
            let db = redb::Database::create(&db_path)?;

            if dry_run {
                let pending = migration::pending_migrations(&db)?;
                if pending.is_empty() {
                    println!(
                        "No pending migrations. Schema at v{}",
                        migration::CURRENT_VERSION
                    );
                } else {
                    println!("Pending migrations:");
                    for (version, desc) in &pending {
                        println!("  v{version}: {desc}");
                    }
                }
            } else {
                let result = migration::run_migrations(&db)?;
                if result.applied.is_empty() {
                    println!("No migrations needed. Schema at v{}", result.to_version);
                } else {
                    println!("Applied {} migration(s):", result.applied.len());
                    for m in &result.applied {
                        println!("  {m}");
                    }
                    println!("Schema now at v{}", result.to_version);
                }
            }
        }
        Commands::McpServer => {
            mcp::run_mcp_server(config, cli.config).await?;
        }
        Commands::Conformance { .. } => {
            unreachable!("conformance commands are handled before config loading");
        }
        Commands::Dev => {
            config.dev_mode = true;
            server::run_dev(config, cli.config).await?;
        }
        Commands::Status { namespace } => {
            run_status(&config, &cli.config, namespace.as_deref())?;
        }
        Commands::Doctor { repair } => {
            run_doctor(&config, &cli.config, repair)?;
        }
        Commands::Recent { namespace, limit } => {
            run_recent(&config, namespace.as_deref(), limit)?;
        }
        Commands::Hud { namespace, limit } => {
            run_hud(&config, &cli.config, namespace.as_deref(), limit)?;
        }
        Commands::Review { command } => match command {
            ReviewCommands::Queue { namespace, limit } => {
                run_review_queue(&config, namespace.as_deref(), limit)?;
            }
            ReviewCommands::Confirm { namespace, item } => {
                run_review_action(
                    &config,
                    &namespace,
                    item,
                    crate::memory::MemoryFeedbackAction::Confirm,
                    None,
                )?;
            }
            ReviewCommands::Reject { namespace, item } => {
                run_review_action(
                    &config,
                    &namespace,
                    item,
                    crate::memory::MemoryFeedbackAction::Reject,
                    None,
                )?;
            }
            ReviewCommands::Supersede {
                namespace,
                item,
                with_item,
            } => {
                run_review_action(
                    &config,
                    &namespace,
                    item,
                    crate::memory::MemoryFeedbackAction::Supersede,
                    Some(with_item),
                )?;
            }
        },
        Commands::Passport { command } => match command {
            PassportCommands::Export {
                namespace,
                scope,
                output,
            } => {
                let scope = scope
                    .parse::<crate::memory::PassportScope>()
                    .map_err(anyhow::Error::msg)?;
                run_passport_export(&config, namespace.as_deref(), scope, output)?;
            }
            PassportCommands::Import {
                path,
                namespace,
                dry_run,
            } => {
                run_passport_import(&config, &path, namespace.as_deref(), dry_run)?;
            }
        },
        Commands::Adapter { command } => match command {
            AdapterCommands::Import {
                kind,
                path,
                namespace,
                dry_run,
            } => {
                let kind = kind
                    .parse::<adapters::MemoryAdapterKind>()
                    .map_err(anyhow::Error::msg)?;
                run_adapter_import(&config, kind, &path, namespace.as_deref(), dry_run)?;
            }
            AdapterCommands::Export {
                kind,
                namespace,
                output,
            } => {
                let kind = kind
                    .parse::<adapters::MemoryAdapterKind>()
                    .map_err(anyhow::Error::msg)?;
                run_adapter_export(&config, kind, namespace.as_deref(), output)?;
            }
        },
        Commands::Connector { command } => match command {
            ConnectorCommands::List => {
                run_connector_list();
            }
            ConnectorCommands::Ingest {
                kind,
                namespace,
                summary,
                evidence,
                tags,
                source_ref,
                allow_raw,
                dry_run,
            } => {
                let kind = kind
                    .parse::<crate::server::routes::AmbientConnectorKind>()
                    .map_err(anyhow::Error::msg)?;
                run_connector_ingest(
                    &config,
                    kind,
                    namespace.as_deref(),
                    summary,
                    evidence,
                    tags,
                    source_ref,
                    allow_raw,
                    dry_run,
                )?;
            }
        },
        Commands::History { command } => match command {
            HistoryCommands::Show { id, namespace } => {
                let uuid: uuid::Uuid = id
                    .parse()
                    .map_err(|_| anyhow::anyhow!("invalid UUID: {id}"))?;
                run_history_show(&config, &namespace, uuid)?;
            }
            HistoryCommands::Export {
                id,
                namespace,
                output,
            } => {
                let uuid: uuid::Uuid = id
                    .parse()
                    .map_err(|_| anyhow::anyhow!("invalid UUID: {id}"))?;
                run_history_export(&config, &namespace, uuid, output)?;
            }
            HistoryCommands::Replay {
                path,
                namespace,
                dry_run,
            } => {
                run_history_replay(&config, &path, namespace.as_deref(), dry_run)?;
            }
            HistoryCommands::Branch {
                id,
                namespace,
                target_namespace,
                dry_run,
            } => {
                let uuid: uuid::Uuid = id
                    .parse()
                    .map_err(|_| anyhow::anyhow!("invalid UUID: {id}"))?;
                run_history_branch(&config, &namespace, &target_namespace, uuid, dry_run)?;
            }
        },
        Commands::Bundle { command } => match command {
            BundleCommands::Export {
                kind,
                namespace,
                scope,
                id,
                output,
            } => {
                let kind = kind
                    .parse::<crate::server::routes::MemoryBundleKind>()
                    .map_err(anyhow::Error::msg)?;
                run_bundle_export(
                    &config,
                    kind,
                    namespace.as_deref(),
                    &scope,
                    id.as_deref(),
                    output,
                )?;
            }
            BundleCommands::Preview { .. }
            | BundleCommands::Validate { .. }
            | BundleCommands::Diff { .. } => {
                unreachable!("bundle preview/validate/diff are handled before config loading")
            }
        },
        Commands::Reader { .. } => {
            unreachable!("reader commands are handled before config loading")
        }
        Commands::Inspect { id } => {
            let uuid: uuid::Uuid = id
                .parse()
                .map_err(|_| anyhow::anyhow!("invalid UUID: {id}"))?;
            std::fs::create_dir_all(&config.storage.data_dir)?;
            let doc_engine = engines::document::DocumentEngine::open(&config.storage.data_dir)?;
            match doc_engine.get(uuid, "default")? {
                Some(mem) => {
                    let trust_scorer =
                        crate::security::trust::TrustScorer::new(config.trust.threshold);
                    let _ = trust_scorer.load_from_redb(doc_engine.db());
                    let trust = trust_scorer
                        .score_memory(&mem, mem.namespace.as_deref().unwrap_or("default"));
                    let age = chrono::Utc::now() - mem.created_at;
                    println!("ID:           {}", mem.id);
                    println!(
                        "Content:      {}",
                        if mem.content.len() > 100 {
                            let truncated: String = mem.content.chars().take(100).collect();
                            format!("{truncated}...")
                        } else {
                            mem.content.clone()
                        }
                    );
                    println!("Tags:         {:?}", mem.tags);
                    println!("Agent:        {}", mem.agent.as_deref().unwrap_or("-"));
                    println!("Session:      {}", mem.session.as_deref().unwrap_or("-"));
                    println!(
                        "Namespace:    {}",
                        mem.namespace.as_deref().unwrap_or("default")
                    );
                    println!("Type:         {}", mem.memory_type);
                    println!("Status:       {}", mem.status);
                    println!("Version:      {}", mem.version);
                    println!("Created:      {}", mem.created_at);
                    println!("Updated:      {}", mem.updated_at);
                    println!("Confidence:   {:?}", mem.confidence);
                    println!("Evidence:     {}", mem.evidence_count);
                    println!("Verified at:  {:?}", mem.last_verified_at);
                    println!("Superseded by:{:?}", mem.superseded_by);
                    println!("Injected:     {}", mem.injection_count);
                    println!("Reused:       {}", mem.reuse_count);
                    println!("Confirmed:    {}", mem.confirm_count);
                    println!("Rejected:     {}", mem.reject_count);
                    println!("Superseded:   {}", mem.supersede_count);
                    println!("Last injected:{:?}", mem.last_injected_at);
                    println!("Last reused:  {:?}", mem.last_reused_at);
                    println!("Last outcome: {:?}", mem.last_outcome_at);
                    println!("Source key:   {}", mem.source_key.as_deref().unwrap_or("-")); // already hashed key_id
                    println!(
                        "Content hash: {}",
                        mem.content_hash.as_deref().unwrap_or("-")
                    );
                    println!("Has embedding:{}", mem.embedding.is_some());
                    println!("Trust score:  {:.4}", trust.score);
                    println!(
                        "Trust CI:     {:.4} .. {:.4}",
                        trust.confidence_low, trust.confidence_high
                    );
                    println!("Low trust:    {}", trust.low_trust);
                    println!(
                        "Signals:      recency={:.3} source={:.3} embedding={:.3} access={:.3} outcome={:.3}",
                        trust.signals.recency,
                        trust.signals.source_reputation,
                        trust.signals.embedding_coherence,
                        trust.signals.access_frequency,
                        trust.signals.outcome_learning,
                    );
                    println!(
                        "Age:          {}h {}m",
                        age.num_hours(),
                        age.num_minutes() % 60
                    );
                }
                None => {
                    eprintln!("Memory not found: {uuid}");
                    std::process::exit(1);
                }
            }
        }
        Commands::Backup {
            output,
            include_key,
        } => {
            let data_dir = &config.storage.data_dir;
            if !data_dir.exists() {
                anyhow::bail!("data directory does not exist: {}", data_dir.display());
            }

            let timestamp = chrono::Utc::now().format("%Y%m%d-%H%M%S");
            let output_path = output
                .unwrap_or_else(|| PathBuf::from(format!("memoryoss-backup-{timestamp}.tar.zst")));

            println!("Creating backup of {} ...", data_dir.display());

            // Create compressed tar archive
            let file = std::fs::File::create(&output_path)?;
            let zstd_encoder = zstd::Encoder::new(file, 3)?;
            let mut tar = tar::Builder::new(zstd_encoder);

            // Exclude the local master key by default so a leaked backup cannot
            // decrypt the encrypted memory store.
            append_backup_tree(&mut tar, data_dir, Path::new("data"), include_key)?;

            // Add backup metadata
            let metadata = serde_json::json!({
                "version": env!("CARGO_PKG_VERSION"),
                "created_at": chrono::Utc::now().to_rfc3339(),
                "data_dir": data_dir.display().to_string(),
                "key_included": include_key,
            });
            let meta_bytes = serde_json::to_vec_pretty(&metadata)?;
            let mut header = tar::Header::new_gnu();
            header.set_size(meta_bytes.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            tar.append_data(&mut header, "backup-meta.json", &meta_bytes[..])?;

            let encoder = tar.into_inner()?;
            encoder.finish()?;

            let size = std::fs::metadata(&output_path)?.len();
            println!(
                "Backup created: {} ({:.1} MB)",
                output_path.display(),
                size as f64 / 1_048_576.0
            );
        }
        Commands::Restore { path, force } => {
            let data_dir = &config.storage.data_dir;

            if data_dir.exists() && std::fs::read_dir(data_dir)?.next().is_some() {
                if !force {
                    eprintln!(
                        "Data directory {} is not empty. Use --force to overwrite.",
                        data_dir.display()
                    );
                    std::process::exit(1);
                }
                println!(
                    "Warning: overwriting existing data in {}",
                    data_dir.display()
                );
            }

            println!("Restoring from {} ...", path.display());

            let file = std::fs::File::open(&path)?;
            let zstd_decoder = zstd::Decoder::new(file)?;
            let mut archive = tar::Archive::new(zstd_decoder);

            // Extract — the archive has "data/" prefix, we need to strip it
            for entry in archive.entries()? {
                let mut entry = entry?;
                let entry_path = entry.path()?.into_owned();

                if entry_path.to_str() == Some("backup-meta.json") {
                    // Read and display metadata
                    let mut buf = Vec::new();
                    std::io::Read::read_to_end(&mut entry, &mut buf)?;
                    if let Ok(meta) = serde_json::from_slice::<serde_json::Value>(&buf) {
                        println!(
                            "Backup version: {}",
                            meta.get("version")
                                .and_then(|v| v.as_str())
                                .unwrap_or("unknown")
                        );
                        println!(
                            "Backup created: {}",
                            meta.get("created_at")
                                .and_then(|v| v.as_str())
                                .unwrap_or("unknown")
                        );
                    }
                    continue;
                }

                // Strip "data/" prefix and extract into data_dir
                if let Ok(relative) = entry_path.strip_prefix("data") {
                    // Prevent path traversal: reject entries with ".." components
                    if relative
                        .components()
                        .any(|c| matches!(c, std::path::Component::ParentDir))
                    {
                        eprintln!(
                            "Skipping dangerous path in backup: {}",
                            entry_path.display()
                        );
                        continue;
                    }
                    let target = data_dir.join(relative);
                    if let Some(parent) = target.parent() {
                        std::fs::create_dir_all(parent)?;
                    }
                    if entry.header().entry_type().is_dir() {
                        std::fs::create_dir_all(&target)?;
                    } else {
                        let mut out = std::fs::File::create(&target)?;
                        std::io::copy(&mut entry, &mut out)?;
                    }
                }
            }

            println!("Restore complete. Data written to {}", data_dir.display());
        }
        Commands::Decay {
            dry_run,
            after_days,
            namespace,
        } => {
            let after_days = after_days.unwrap_or(config.decay.after_days);
            let now = chrono::Utc::now();

            std::fs::create_dir_all(&config.storage.data_dir)?;
            let doc_engine = engines::document::DocumentEngine::open_with_config(
                &config.storage.data_dir,
                &config.encryption,
                config.auth.audit_hmac_secret.as_bytes(),
            )?;
            let trust_scorer = crate::security::trust::TrustScorer::new(config.trust.threshold);
            let _ = trust_scorer.load_from_redb(doc_engine.db());

            // Determine namespaces to scan
            let namespaces = if let Some(ns) = namespace {
                vec![ns]
            } else {
                decay_namespaces(&config, doc_engine.list_namespaces()?)
            };

            if namespaces.is_empty() {
                println!("No namespaces found.");
                return Ok(());
            }

            let mut total_scanned = 0usize;
            let mut total_stale = 0usize;
            let mut total_archived = 0usize;

            for ns in &namespaces {
                let memories = doc_engine.list_all_including_archived(ns)?;
                let scanned_count = memories.len();
                let mut ns_stale = 0usize;
                let mut ns_archived = 0;

                for mut mem in memories {
                    total_scanned += 1;
                    let trust = trust_scorer.score_memory(&mem, ns);
                    let decision = mem.apply_lifecycle_policy(now, after_days, trust.score);

                    if dry_run && decision.archive {
                        println!(
                            "[DRY-RUN] Would archive: {} (ns={}, idle={}d, trust={:.3}, content={})",
                            mem.id,
                            ns,
                            (now - mem.lifecycle_anchor()).num_days(),
                            trust.score,
                            if mem.content.len() > 60 {
                                let truncated: String = mem.content.chars().take(60).collect();
                                format!("{truncated}...")
                            } else {
                                mem.content.clone()
                            },
                        );
                        total_archived += 1;
                        continue;
                    }

                    if dry_run && decision.changed {
                        println!(
                            "[DRY-RUN] Would mark stale: {} (ns={}, idle={}d, trust={:.3}, content={})",
                            mem.id,
                            ns,
                            (now - mem.lifecycle_anchor()).num_days(),
                            trust.score,
                            if mem.content.len() > 60 {
                                let truncated: String = mem.content.chars().take(60).collect();
                                format!("{truncated}...")
                            } else {
                                mem.content.clone()
                            },
                        );
                        total_stale += 1;
                        continue;
                    }

                    if !dry_run && decision.changed {
                        doc_engine.replace(&mem, "decay-policy")?;
                        ns_stale += 1;
                        total_stale += 1;
                    }

                    if !dry_run
                        && decision.archive
                        && doc_engine.archive(mem.id, ns, "decay-policy")?
                    {
                        ns_archived += 1;
                        total_archived += 1;
                    }
                }

                if ns_stale > 0
                    || ns_archived > 0
                    || (dry_run && (total_stale > 0 || total_archived > 0))
                {
                    println!(
                        "Namespace '{}': {} memories scanned, {} stale, {} {}",
                        ns,
                        scanned_count,
                        ns_stale,
                        ns_archived,
                        if dry_run {
                            "would be archived"
                        } else {
                            "archived"
                        },
                    );
                }
            }

            println!(
                "\nTotal: {} scanned, {} stale, {} {} across {} namespace(s) (threshold: {}d)",
                total_scanned,
                total_stale,
                total_archived,
                if dry_run {
                    "would be archived"
                } else {
                    "archived"
                },
                namespaces.len(),
                after_days,
            );
        }
        Commands::Setup {
            profile,
            team_manifest,
        } => {
            run_setup_wizard(&cli.config, profile, team_manifest.as_deref()).await?;
            return Ok(());
        }
        Commands::MigrateEmbeddings {
            model,
            batch_size,
            namespace,
            dry_run,
        } => {
            println!("Loading model: {model} ...");
            let (text_model, dim) = crate::embedding::load_text_embedding(model, true)?;
            println!("Model ready: {dim}-dim");

            std::fs::create_dir_all(&config.storage.data_dir)?;
            let doc_engine = engines::document::DocumentEngine::open_with_config(
                &config.storage.data_dir,
                &config.encryption,
                config.auth.audit_hmac_secret.as_bytes(),
            )?;

            // Collect namespaces
            let namespaces = if let Some(ns) = namespace {
                vec![ns]
            } else {
                let mut ns_set: std::collections::HashSet<String> = config
                    .auth
                    .api_keys
                    .iter()
                    .map(|k| k.namespace.clone())
                    .collect();
                ns_set.insert("default".to_string());
                ns_set.into_iter().collect()
            };

            let mut total_processed = 0usize;
            let mut total_skipped = 0usize;
            let mut total_errors = 0usize;

            for ns in &namespaces {
                let memories = doc_engine.search(ns, None, None, None, &[])?;
                if memories.is_empty() {
                    continue;
                }

                println!("Namespace '{}': {} memories to process", ns, memories.len());

                // Process in batches
                for chunk in memories.chunks(batch_size) {
                    let texts: Vec<String> = chunk.iter().map(|m| m.content.clone()).collect();
                    let ids: Vec<uuid::Uuid> = chunk.iter().map(|m| m.id).collect();

                    if dry_run {
                        total_processed += texts.len();
                        continue;
                    }

                    match text_model.embed(texts.clone(), None) {
                        Ok(embeddings) => {
                            for (id, emb) in ids.iter().zip(embeddings) {
                                if emb.len() != dim {
                                    total_errors += 1;
                                    continue;
                                }
                                match doc_engine.set_embedding(*id, ns, emb) {
                                    Ok(true) => total_processed += 1,
                                    Ok(false) => total_skipped += 1,
                                    Err(e) => {
                                        eprintln!("Error updating {id}: {e}");
                                        total_errors += 1;
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            eprintln!("Embedding batch failed: {e}");
                            total_errors += texts.len();
                        }
                    }

                    if total_processed.is_multiple_of(100) && total_processed > 0 {
                        println!("  Progress: {} processed", total_processed);
                    }
                }
            }

            println!(
                "\nMigration {}: {} processed, {} skipped, {} errors across {} namespace(s)",
                if dry_run { "preview" } else { "complete" },
                total_processed,
                total_skipped,
                total_errors,
                namespaces.len(),
            );

            if !dry_run && total_processed > 0 {
                println!(
                    "Note: Run `memoryoss serve` to rebuild vector/FTS indexes from updated embeddings."
                );
            }
        }
    }

    Ok(())
}
