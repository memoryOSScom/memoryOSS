// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 memoryOSS Contributors
#![allow(
    clippy::too_many_arguments,
    clippy::type_complexity,
    clippy::collapsible_if,
    clippy::collapsible_match,
    clippy::manual_async_fn
)]

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
    /// Start in dev mode (mock embeddings, no TLS, relaxed auth)
    Dev,
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
    Setup,
    /// Re-embed all memories with a new model
    MigrateEmbeddings {
        /// Target model (e.g. "all-minilm-l6-v2", "bge-small-en-v1.5")
        #[arg(long, default_value = "all-minilm-l6-v2")]
        model: String,
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

async fn run_setup_wizard(config_path: &std::path::Path) -> anyhow::Result<()> {
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
    if has_any_openai_key && !has_codex {
        println!("    ✓ OPENAI_API_KEY (configured)");
    }
    if claude_has_api_key && !has_claude_code {
        println!("    ✓ ANTHROPIC_API_KEY (configured)");
    }
    if !has_claude_code && !has_codex && env_openai.is_none() && env_anthropic.is_none() {
        println!("    (no AI tools found — will configure for manual use)");
    }

    // Then ask auth choices where both methods are available
    let claude_uses_api_key = if has_claude_code && claude_has_oauth && claude_has_api_key {
        println!();
        println!("  Auth method for Claude Code:");
        println!("    1) Subscription (OAuth) — no extra cost (recommended)");
        println!("    2) API key — pay per token");
        let auth_choice = prompt_choice("  Choose: ", &["oauth", "apikey"], 0);
        auth_choice == 1
    } else {
        claude_has_api_key && !claude_has_oauth
    };

    let codex_uses_api_key = if codex_has_both {
        println!();
        println!("  Auth method for Codex CLI:");
        println!("    1) Subscription (OAuth) — no extra cost (recommended)");
        println!("    2) API key — pay per token");
        let auth_choice = prompt_choice("  Choose: ", &["oauth", "apikey"], 0);
        auth_choice == 1
    } else {
        has_any_openai_key && !codex_has_oauth
    };
    println!();
    if has_codex && codex_has_oauth && !codex_uses_api_key {
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

[logging]
level = "info"
json = false

[decay]
enabled = {decay_enabled}
strategy = "age"
after_days = {decay_days}
"#,
        timestamp = chrono::Utc::now().format("%Y-%m-%d %H:%M UTC"),
        audit_hmac_secret = audit_hmac_secret,
        extract_model = extract_model,
        extract_provider = extract_provider,
        core_port = core_port,
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

        let claude_proxy_safe = claude_has_api_key && (!claude_has_oauth || claude_uses_api_key);
        let codex_proxy_safe = has_any_openai_key && (!codex_has_oauth || codex_uses_api_key);

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

    // --- MCP configuration ---
    // MCP stays enabled for explicit memory tools and Marketplace requirements.
    // The proxy now runs in front of a fail-open gateway, so transparent memory and
    // explicit MCP tools coexist under one setup.
    let config_path_abs =
        std::fs::canonicalize(config_path).unwrap_or_else(|_| config_path.to_path_buf());
    let memoryoss_bin = preferred_runtime_binary();

    if has_claude_code {
        let status = std::process::Command::new("claude")
            .args(["mcp", "add", "--transport", "stdio", "memoryoss", "--"])
            .arg(&memoryoss_bin)
            .args(["-c", &config_path_abs.to_string_lossy()])
            .arg("mcp-server")
            .output();

        match status {
            Ok(out) if out.status.success() => {
                println!("  ✓ MCP configured for Claude Code");
            }
            Ok(out) => {
                let stderr = String::from_utf8_lossy(&out.stderr);
                if stderr.contains("already exists") || stderr.contains("already configured") {
                    println!("  ✓ Claude Code MCP already configured");
                } else {
                    eprintln!("  ✗ Failed to configure Claude Code MCP: {}", stderr.trim());
                }
            }
            Err(e) => {
                eprintln!("  ✗ Could not run 'claude mcp add': {e}");
            }
        }
    }

    if has_codex {
        let mut skip_add = false;
        let mut replaced_stale = false;

        match std::process::Command::new("codex")
            .args(["mcp", "get", "memoryoss"])
            .output()
        {
            Ok(out) if out.status.success() => {
                let stdout = String::from_utf8_lossy(&out.stdout);
                if codex_mcp_matches_desired(&stdout, &memoryoss_bin, &config_path_abs) {
                    println!("  ✓ Codex MCP already configured");
                    skip_add = true;
                } else {
                    match std::process::Command::new("codex")
                        .args(["mcp", "remove", "memoryoss"])
                        .output()
                    {
                        Ok(remove_out) if remove_out.status.success() => {
                            replaced_stale = true;
                        }
                        Ok(remove_out) => {
                            let stderr = String::from_utf8_lossy(&remove_out.stderr);
                            eprintln!(
                                "  ✗ Failed to replace stale Codex MCP config: {}",
                                stderr.trim()
                            );
                            skip_add = true;
                        }
                        Err(e) => {
                            eprintln!("  ✗ Could not run 'codex mcp remove': {e}");
                            skip_add = true;
                        }
                    }
                }
            }
            Ok(_) => {}
            Err(_) => {}
        }

        if !skip_add {
            let status = std::process::Command::new("codex")
                .args(["mcp", "add", "memoryoss", "--"])
                .arg(&memoryoss_bin)
                .args(["-c", &config_path_abs.to_string_lossy()])
                .arg("mcp-server")
                .output();

            match status {
                Ok(out) if out.status.success() => {
                    if replaced_stale {
                        println!("  ✓ Codex MCP updated");
                    } else {
                        println!("  ✓ MCP configured for Codex CLI");
                    }
                }
                Ok(out) => {
                    let stderr = String::from_utf8_lossy(&out.stderr);
                    if stderr.contains("already exists") || stderr.contains("already configured") {
                        println!("  ✓ Codex MCP already configured");
                    } else {
                        eprintln!("  ✗ Failed to configure Codex MCP: {}", stderr.trim());
                    }
                }
                Err(e) => {
                    eprintln!("  ✗ Could not run 'codex mcp add': {e}");
                }
            }
        }
    }

    // --- Claude Code statusline indicator ---
    if has_claude_code {
        let claude_dir = std::env::var("HOME")
            .ok()
            .map(|h| std::path::PathBuf::from(h).join(".claude"))
            .unwrap_or_else(|| std::path::PathBuf::from(".claude"));

        let health_url = format!("http://{}:{}/health", bind_host, port);
        let script_path = claude_dir.join("statusline-command.sh");
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

        std::fs::create_dir_all(&claude_dir).ok();
        match std::fs::write(&script_path, &script) {
            Ok(_) => {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755))
                        .ok();
                }

                // Add statusLine to Claude Code settings.json
                let settings_path = claude_dir.join("settings.json");
                let mut settings: serde_json::Value = std::fs::read_to_string(&settings_path)
                    .ok()
                    .and_then(|s| serde_json::from_str(&s).ok())
                    .unwrap_or_else(|| serde_json::json!({}));

                settings["statusLine"] = serde_json::json!({
                    "type": "command",
                    "command": format!("bash {}", script_path.display()),
                });

                match std::fs::write(
                    &settings_path,
                    serde_json::to_string_pretty(&settings).unwrap_or_default(),
                ) {
                    Ok(_) => println!("  ✓ Claude Code statusline configured"),
                    Err(e) => eprintln!("  ✗ Failed to update Claude Code settings: {e}"),
                }
            }
            Err(e) => eprintln!("  ✗ Failed to write statusline script: {e}"),
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

    if !started {
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
    println!("  Setup done. Start your AI agent as usual — memory works automatically.");
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

fn codex_mcp_matches_desired(
    current: &str,
    binary: &std::path::Path,
    config: &std::path::Path,
) -> bool {
    let binary_str = binary.to_string_lossy();
    let config_str = config.to_string_lossy();
    current.contains(binary_str.as_ref())
        && current.contains(&format!("-c {}", config_str))
        && current.contains("mcp-server")
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

    // Auto-run setup wizard if no config exists and no explicit command given
    if !cli.config.exists() && !matches!(command, Commands::Setup) {
        println!();
        println!(
            "  No config found at '{}'. Starting setup wizard...",
            cli.config.display()
        );
        println!();
        run_setup_wizard(&cli.config).await?;
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
        Commands::Dev => {
            config.dev_mode = true;
            server::run_dev(config, cli.config).await?;
        }
        Commands::Inspect { id } => {
            let uuid: uuid::Uuid = id
                .parse()
                .map_err(|_| anyhow::anyhow!("invalid UUID: {id}"))?;
            std::fs::create_dir_all(&config.storage.data_dir)?;
            let doc_engine = engines::document::DocumentEngine::open(&config.storage.data_dir)?;
            match doc_engine.get(uuid, "default")? {
                Some(mem) => {
                    let trust = mem.recency_trust();
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
                    println!("Source key:   {}", mem.source_key.as_deref().unwrap_or("-")); // already hashed key_id
                    println!(
                        "Content hash: {}",
                        mem.content_hash.as_deref().unwrap_or("-")
                    );
                    println!("Has embedding:{}", mem.embedding.is_some());
                    println!("Trust score:  {:.4}", trust);
                    println!("Low trust:    {}", trust < 0.3);
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
            let cutoff = chrono::Utc::now() - chrono::Duration::days(after_days as i64);

            std::fs::create_dir_all(&config.storage.data_dir)?;
            let doc_engine = engines::document::DocumentEngine::open_with_config(
                &config.storage.data_dir,
                &config.encryption,
                config.auth.audit_hmac_secret.as_bytes(),
            )?;

            // Determine namespaces to scan
            let namespaces = if let Some(ns) = namespace {
                vec![ns]
            } else {
                // Collect unique namespaces from API key config + "default"
                let mut ns_set: std::collections::HashSet<String> = config
                    .auth
                    .api_keys
                    .iter()
                    .map(|k| k.namespace.clone())
                    .collect();
                ns_set.insert("default".to_string());
                ns_set.into_iter().collect()
            };

            if namespaces.is_empty() {
                println!("No namespaces found.");
                return Ok(());
            }

            let mut total_scanned = 0usize;
            let mut total_archived = 0usize;

            for ns in &namespaces {
                let memories = doc_engine.search(ns, None, None, None, &[])?;
                let mut ns_archived = 0;

                for mem in &memories {
                    total_scanned += 1;
                    // Decay condition: updated_at older than cutoff (proxy for "never interacted with")
                    if mem.updated_at < cutoff {
                        if dry_run {
                            println!(
                                "[DRY-RUN] Would archive: {} (ns={}, age={}d, content={})",
                                mem.id,
                                ns,
                                (chrono::Utc::now() - mem.updated_at).num_days(),
                                if mem.content.len() > 60 {
                                    let truncated: String = mem.content.chars().take(60).collect();
                                    format!("{truncated}...")
                                } else {
                                    mem.content.clone()
                                },
                            );
                        } else if doc_engine.archive(mem.id, ns, "decay-policy")? {
                            ns_archived += 1;
                        }
                        total_archived += 1;
                    }
                }

                if ns_archived > 0 || (dry_run && total_archived > 0) {
                    println!(
                        "Namespace '{}': {} memories scanned, {} {}",
                        ns,
                        memories.len(),
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
                "\nTotal: {} scanned, {} {} across {} namespace(s) (cutoff: {}d)",
                total_scanned,
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
        Commands::Setup => {
            run_setup_wizard(&cli.config).await?;
            return Ok(());
        }
        Commands::MigrateEmbeddings {
            model,
            batch_size,
            namespace,
            dry_run,
        } => {
            use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};

            // Map model name string to fastembed enum
            let emb_model = match model.as_str() {
                "all-minilm-l6-v2" | "AllMiniLML6V2" => EmbeddingModel::AllMiniLML6V2,
                "bge-small-en-v1.5" | "BGESmallENV15" => EmbeddingModel::BGESmallENV15,
                "bge-base-en-v1.5" | "BGEBaseENV15" => EmbeddingModel::BGEBaseENV15,
                "bge-large-en-v1.5" | "BGELargeENV15" => EmbeddingModel::BGELargeENV15,
                other => anyhow::bail!(
                    "unsupported model: {other}. Supported: all-minilm-l6-v2, bge-small-en-v1.5, bge-base-en-v1.5, bge-large-en-v1.5"
                ),
            };

            println!("Loading model: {model} ...");
            let mut opts = InitOptions::default();
            opts.model_name = emb_model;
            opts.show_download_progress = true;
            let text_model = TextEmbedding::try_new(opts)?;

            // Detect dimension
            let test = text_model.embed(vec!["test"], None)?;
            let dim = test.first().map(|v| v.len()).unwrap_or(0);
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
