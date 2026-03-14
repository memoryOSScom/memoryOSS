// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 memoryOSS Contributors

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default = "default_server")]
    pub server: ServerConfig,
    #[serde(default)]
    pub tls: TlsConfig,
    #[serde(default)]
    pub auth: AuthConfig,
    #[serde(default)]
    pub storage: StorageConfig,
    #[serde(default)]
    pub logging: LoggingConfig,
    #[serde(default)]
    pub limits: LimitsConfig,
    #[serde(default)]
    pub decompose: DecomposeConfig,
    #[serde(default)]
    pub encryption: EncryptionConfig,
    #[serde(default)]
    pub trust: crate::security::trust::TrustConfig,
    #[serde(default)]
    pub decay: DecayConfig,
    #[serde(default)]
    pub consolidation: ConsolidationConfig,
    #[serde(default)]
    pub proxy: ProxyConfig,
    #[serde(default)]
    pub sharing: crate::sharing::SharingConfig,
    /// Runtime flag — true when started via `memoryoss dev`. Not serialized from config file.
    #[serde(skip)]
    pub dev_mode: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            server: default_server(),
            tls: TlsConfig::default(),
            auth: AuthConfig::default(),
            storage: StorageConfig::default(),
            logging: LoggingConfig::default(),
            limits: LimitsConfig::default(),
            decompose: DecomposeConfig::default(),
            encryption: EncryptionConfig::default(),
            trust: crate::security::trust::TrustConfig::default(),
            decay: DecayConfig::default(),
            consolidation: ConsolidationConfig::default(),
            proxy: ProxyConfig::default(),
            sharing: crate::sharing::SharingConfig::default(),
            dev_mode: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecayConfig {
    /// Strategy: "age" (archive after N days with no updates)
    #[serde(default = "default_decay_strategy")]
    pub strategy: String,
    /// Days after which untouched memories are archived (default 90)
    #[serde(default = "default_decay_after_days")]
    pub after_days: u64,
    /// Whether decay is enabled (default false — opt-in only)
    #[serde(default)]
    pub enabled: bool,
}

impl Default for DecayConfig {
    fn default() -> Self {
        Self {
            strategy: default_decay_strategy(),
            after_days: default_decay_after_days(),
            enabled: false,
        }
    }
}

fn default_decay_strategy() -> String {
    "age".to_string()
}
fn default_decay_after_days() -> u64 {
    90
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsolidationConfig {
    /// Whether automatic consolidation is enabled.
    #[serde(default)]
    pub enabled: bool,
    /// Worker interval in minutes. A value of 0 is treated as a 1s test interval.
    #[serde(default = "default_consolidation_interval_minutes")]
    pub interval_minutes: u64,
    /// Similarity threshold for clustering duplicate memories.
    #[serde(default = "default_consolidation_threshold")]
    pub threshold: f32,
    /// Maximum number of clusters to process per sweep.
    #[serde(default = "default_consolidation_max_clusters")]
    pub max_clusters: usize,
}

impl Default for ConsolidationConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            interval_minutes: default_consolidation_interval_minutes(),
            threshold: default_consolidation_threshold(),
            max_clusters: default_consolidation_max_clusters(),
        }
    }
}

fn default_consolidation_interval_minutes() -> u64 {
    60
}

fn default_consolidation_threshold() -> f32 {
    0.9
}

fn default_consolidation_max_clusters() -> usize {
    25
}

fn default_server() -> ServerConfig {
    ServerConfig {
        host: "127.0.0.1".to_string(),
        port: 8000,
        hybrid_mode: false,
        core_port: None,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    #[serde(default = "default_host")]
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    /// Run a stable frontdoor on `port` and the memory core on `core_port`.
    #[serde(default)]
    pub hybrid_mode: bool,
    /// Internal loopback port for the memory core when hybrid_mode is enabled.
    pub core_port: Option<u16>,
}

fn default_host() -> String {
    "127.0.0.1".to_string()
}
fn default_port() -> u16 {
    8000
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TlsConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    pub cert_path: Option<PathBuf>,
    pub key_path: Option<PathBuf>,
    #[serde(default = "default_true")]
    pub auto_generate: bool,
    /// Optional CA certificate for mTLS client verification.
    /// When set, clients must present a certificate signed by this CA.
    pub client_ca_path: Option<PathBuf>,
}

fn default_true() -> bool {
    true
}

impl Default for TlsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            cert_path: None,
            key_path: None,
            auto_generate: true,
            client_ca_path: None,
        }
    }
}

fn default_memory_mode() -> String {
    "readonly".to_string()
}

#[derive(Clone, Serialize, Deserialize)]
pub struct AuthConfig {
    #[serde(default)]
    pub api_keys: Vec<ApiKeyEntry>,
    #[serde(default = "default_jwt_secret")]
    pub jwt_secret: String,
    #[serde(default = "default_jwt_expiry_secs")]
    pub jwt_expiry_secs: u64,
    #[serde(default = "default_audit_hmac_secret")]
    pub audit_hmac_secret: String,
}

impl std::fmt::Debug for AuthConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthConfig")
            .field("api_keys", &self.api_keys)
            .field("jwt_secret", &"***")
            .field("jwt_expiry_secs", &self.jwt_expiry_secs)
            .field("audit_hmac_secret", &"***")
            .finish()
    }
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            api_keys: Vec::new(),
            jwt_secret: default_jwt_secret(),
            jwt_expiry_secs: default_jwt_expiry_secs(),
            audit_hmac_secret: default_audit_hmac_secret(),
        }
    }
}

fn default_audit_hmac_secret() -> String {
    if let Ok(val) = std::env::var("MEMORYOSS_AUDIT_HMAC_SECRET") {
        if !val.is_empty() {
            return val;
        }
    }
    tracing::warn!(
        "No audit_hmac_secret configured — using random secret. Audit chain verification will reset on restart. Set auth.audit_hmac_secret in config or MEMORYOSS_AUDIT_HMAC_SECRET env var."
    );
    use rand::Rng;
    let secret: [u8; 32] = rand::thread_rng().r#gen();
    hex::encode(secret)
}

fn default_jwt_secret() -> String {
    // Check env var first — if set, use it (Config::load also does this, but
    // default_jwt_secret runs during serde deserialization before env override).
    if let Ok(val) = std::env::var("MEMORYOSS_JWT_SECRET") {
        if !val.is_empty() {
            return val;
        }
    }
    // Generate a random secret but log a prominent warning.
    // In production, MEMORYOSS_JWT_SECRET or auth.jwt_secret MUST be set.
    tracing::warn!(
        "No jwt_secret configured — using random secret. JWTs will be invalidated on restart. Set auth.jwt_secret in config or MEMORYOSS_JWT_SECRET env var."
    );
    use rand::Rng;
    let secret: [u8; 32] = rand::thread_rng().r#gen();
    hex::encode(secret)
}

fn default_jwt_expiry_secs() -> u64 {
    3600
}

#[derive(Clone, Serialize, Deserialize)]
pub struct ApiKeyEntry {
    pub key: String,
    pub role: Role,
    #[serde(default = "default_namespace")]
    pub namespace: String,
}

impl std::fmt::Debug for ApiKeyEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApiKeyEntry")
            .field("key", &"***")
            .field("role", &self.role)
            .field("namespace", &self.namespace)
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Reader,
    Writer,
    Admin,
}

fn default_namespace() -> String {
    "default".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageConfig {
    #[serde(default = "default_data_dir")]
    pub data_dir: PathBuf,
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            data_dir: default_data_dir(),
        }
    }
}

fn default_data_dir() -> PathBuf {
    PathBuf::from("data")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoggingConfig {
    #[serde(default = "default_log_level")]
    pub level: String,
    #[serde(default)]
    pub json: bool,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            level: default_log_level(),
            json: false,
        }
    }
}

fn default_log_level() -> String {
    "info".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LimitsConfig {
    /// Maximum content size per memory in bytes (default: 32KB)
    #[serde(default = "default_max_content_bytes")]
    pub max_content_bytes: usize,
    /// Maximum number of tags per memory (default: 50)
    #[serde(default = "default_max_tags")]
    pub max_tags: usize,
    /// Maximum tag length in bytes (default: 256)
    #[serde(default = "default_max_tag_length")]
    pub max_tag_length: usize,
    /// Maximum store requests per second per API key (default: 100)
    #[serde(default = "default_rate_limit_per_sec")]
    pub rate_limit_per_sec: u32,
    /// Maximum namespace length (default: 128)
    #[serde(default = "default_max_namespace_length")]
    pub max_namespace_length: usize,
    /// Embedding cache TTL in seconds (default: 300 = 5min)
    #[serde(default = "default_embedding_cache_ttl_secs")]
    pub embedding_cache_ttl_secs: u64,
    /// Maximum embedding cache entries (default: 10000)
    #[serde(default = "default_embedding_cache_max_size")]
    pub embedding_cache_max_size: usize,
    /// Group commit batch size (default: 100)
    #[serde(default = "default_group_commit_batch_size")]
    pub group_commit_batch_size: usize,
    /// Group commit flush interval in milliseconds (default: 10)
    #[serde(default = "default_group_commit_flush_ms")]
    pub group_commit_flush_ms: u64,
    /// Intent cache TTL in seconds (default: 60)
    #[serde(default = "default_intent_cache_ttl_secs")]
    pub intent_cache_ttl_secs: u64,
    /// Maximum intent cache entries (default: 5000)
    #[serde(default = "default_intent_cache_max_entries")]
    pub intent_cache_max_entries: usize,
}

impl Default for LimitsConfig {
    fn default() -> Self {
        Self {
            max_content_bytes: default_max_content_bytes(),
            max_tags: default_max_tags(),
            max_tag_length: default_max_tag_length(),
            rate_limit_per_sec: default_rate_limit_per_sec(),
            max_namespace_length: default_max_namespace_length(),
            embedding_cache_ttl_secs: default_embedding_cache_ttl_secs(),
            embedding_cache_max_size: default_embedding_cache_max_size(),
            group_commit_batch_size: default_group_commit_batch_size(),
            group_commit_flush_ms: default_group_commit_flush_ms(),
            intent_cache_ttl_secs: default_intent_cache_ttl_secs(),
            intent_cache_max_entries: default_intent_cache_max_entries(),
        }
    }
}

fn default_max_content_bytes() -> usize {
    32 * 1024
}
fn default_max_tags() -> usize {
    50
}
fn default_max_tag_length() -> usize {
    256
}
fn default_rate_limit_per_sec() -> u32 {
    100
}
fn default_max_namespace_length() -> usize {
    128
}
fn default_embedding_cache_ttl_secs() -> u64 {
    300
}
fn default_embedding_cache_max_size() -> usize {
    10_000
}
fn default_group_commit_batch_size() -> usize {
    100
}
fn default_group_commit_flush_ms() -> u64 {
    10
}
fn default_intent_cache_ttl_secs() -> u64 {
    60
}
fn default_intent_cache_max_entries() -> usize {
    5000
}

/// Encryption key provider config.
/// Default: local file-based key with HKDF namespace derivation.
/// Supports AWS KMS and HashiCorp Vault for cloud deployments.
/// Key hierarchy: Master Key → Namespace Keys → Data Encryption Keys.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct EncryptionConfig {
    /// Key provider: "local" (default), "aws_kms", or "vault".
    pub provider: Option<String>,
    /// AWS KMS key ID (ARN or alias). Required for aws_kms provider.
    pub key_id: Option<String>,
    /// AWS region (default: "us-east-1"). Used with aws_kms.
    pub region: Option<String>,
    /// HashiCorp Vault address (e.g. "https://vault.example.com:8200").
    pub vault_address: Option<String>,
    /// Vault authentication token.
    pub vault_token: Option<String>,
    /// Vault Transit mount path (default: "transit").
    pub vault_mount: Option<String>,
    /// Vault Transit key name (default: "memoryoss").
    pub vault_key_name: Option<String>,
    /// Grace period in seconds for old keys after rotation (default: 86400 = 24h).
    pub grace_period_secs: Option<u64>,
}

/// LLM-powered decomposition config. When `provider` is set, the system
/// sends only metadata (agent stats, topics, time ranges) to the LLM to
/// generate better sub-queries. Falls back to heuristic on any error.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DecomposeConfig {
    /// LLM provider: "claude", "openai", or "ollama". None = heuristic only.
    pub provider: Option<String>,
    /// API key for the LLM provider (not needed for Ollama).
    pub api_key: Option<String>,
    /// Model name (e.g. "claude-sonnet-4-6", "gpt-4o", "llama3").
    #[serde(default = "default_decompose_model")]
    pub model: String,
    /// Custom endpoint URL (required for Ollama, optional for others).
    pub endpoint: Option<String>,
    /// Maximum tokens of metadata to send to the LLM (default: 2000).
    #[serde(default = "default_token_budget")]
    pub token_budget: usize,
}

fn default_decompose_model() -> String {
    "claude-sonnet-4-6".to_string()
}

fn default_token_budget() -> usize {
    2000
}

/// Proxy configuration for OpenAI-compatible LLM proxy.
/// Maps proxy API keys to upstream LLM providers + memoryOSS namespaces.
#[derive(Clone, Serialize, Deserialize)]
pub struct ProxyConfig {
    /// Enable the proxy endpoints at /proxy/v1/*
    #[serde(default)]
    pub enabled: bool,
    /// Upstream LLM base URL (e.g. "https://api.openai.com/v1")
    #[serde(default = "default_proxy_upstream")]
    pub upstream_url: String,
    /// Upstream API key (used when forwarding requests)
    pub upstream_api_key: Option<String>,
    /// LLM model for async fact extraction (e.g. "gpt-4o-mini", "claude-haiku-4-5-20251001")
    #[serde(default = "default_extract_model")]
    pub extract_model: String,
    /// LLM provider for extraction: "openai", "claude", "ollama"
    #[serde(default = "default_extract_provider")]
    pub extract_provider: String,
    /// API key for extraction model (defaults to upstream_api_key if not set)
    pub extract_api_key: Option<String>,
    /// Maximum percentage of context window to use for memory injection (default: 10%)
    #[serde(default = "default_max_memory_pct")]
    pub max_memory_pct: f64,
    /// Minimum recall score to inject a memory (default: 0.3)
    #[serde(default = "default_min_recall_score")]
    pub min_recall_score: f64,
    /// Proxy API key → namespace mapping. Each entry maps a proxy-facing API key
    /// to an upstream key and memoryOSS namespace for memory isolation.
    #[serde(default)]
    pub key_mapping: Vec<ProxyKeyMapping>,
    /// Model → context window size overrides (merged with built-in defaults)
    #[serde(default)]
    pub model_context_sizes: std::collections::HashMap<String, u64>,
    /// Privacy: log request metadata only, NOT prompt content (default: true)
    #[serde(default = "default_true")]
    pub privacy_mode: bool,
    /// Anthropic API key for Claude proxy (separate from OpenAI upstream)
    pub anthropic_api_key: Option<String>,
    /// Anthropic upstream URL override (default: https://api.anthropic.com/v1/messages)
    pub anthropic_upstream_url: Option<String>,
    /// Enable async fact extraction from conversations (default: true).
    /// When false, the proxy only injects existing memories but never extracts new ones.
    #[serde(default = "default_true")]
    pub extraction_enabled: bool,
    /// Whether clients can control memory mode via X-Memory-Mode header (default: true).
    /// Set to false to enforce server-side memory policy — clients cannot bypass.
    #[serde(default = "default_true")]
    pub allow_client_memory_control: bool,
    /// Accept any API key (e.g. OAuth tokens) and map to default namespace.
    /// Uses configured upstream keys for forwarding.
    #[serde(default)]
    pub passthrough_auth: bool,
    /// When passthrough_auth is enabled, only allow passthrough from loopback clients by default.
    /// This keeps zero-config local usage working while preventing remote anonymous passthrough.
    #[serde(default = "default_true")]
    pub passthrough_local_only: bool,
    /// Minimum score in any single retrieval channel to pass precision gate (default: 0.15).
    /// Prevents high-recency/low-relevance memories from being injected.
    #[serde(default)]
    pub min_channel_score: Option<f64>,
    /// Diversity factor for result spreading: 0.0 = pure relevance, 1.0 = max diversity (default: 0.3).
    /// Spreads results across different agents/tags to prevent aspect-blindness.
    #[serde(default)]
    pub diversity_factor: Option<f64>,
    /// Lightweight confidence gate before proxy injection.
    /// When enabled, the proxy explicitly chooses inject / abstain / need_more_evidence
    /// instead of relying only on a raw score threshold.
    #[serde(default = "default_true")]
    pub confidence_gate: bool,
    /// Route identifier-, path-, endpoint-, branch- and policy-heavy queries through
    /// a lexical-first reranking path before dense recall dominates.
    #[serde(default = "default_true")]
    pub identifier_first_routing: bool,
    /// Enable the experimental primitive algebra lane for recall/explain reranking.
    #[serde(default)]
    pub primitive_algebra: bool,
    /// Default memory mode: "full", "off", or "after" (default: "full").
    /// Overridden by X-Memory-Mode header if allow_client_memory_control is true.
    #[serde(default = "default_memory_mode")]
    pub default_memory_mode: String,
    /// Only inject memories after this date (ISO 8601). Used when default_memory_mode = "after".
    pub memory_after_date: Option<String>,
}

/// Maps a proxy-facing API key to upstream credentials + namespace.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyKeyMapping {
    /// The API key clients use when calling the proxy
    pub proxy_key: String,
    /// The upstream LLM API key to use (overrides proxy.upstream_api_key)
    pub upstream_key: Option<String>,
    /// memoryOSS namespace for this key's memories
    #[serde(default = "default_namespace")]
    pub namespace: String,
}

impl Default for ProxyConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            upstream_url: default_proxy_upstream(),
            upstream_api_key: None,
            extract_model: default_extract_model(),
            extract_provider: default_extract_provider(),
            extract_api_key: None,
            max_memory_pct: default_max_memory_pct(),
            min_recall_score: default_min_recall_score(),
            key_mapping: Vec::new(),
            model_context_sizes: std::collections::HashMap::new(),
            privacy_mode: true,
            anthropic_api_key: None,
            anthropic_upstream_url: None,
            extraction_enabled: true,
            allow_client_memory_control: true,
            passthrough_auth: false,
            passthrough_local_only: true,
            min_channel_score: None,
            diversity_factor: None,
            confidence_gate: true,
            identifier_first_routing: true,
            primitive_algebra: false,
            default_memory_mode: default_memory_mode(),
            memory_after_date: None,
        }
    }
}

impl std::fmt::Debug for ProxyConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProxyConfig")
            .field("enabled", &self.enabled)
            .field("upstream_url", &self.upstream_url)
            .field("upstream_api_key", &"***")
            .field("extract_model", &self.extract_model)
            .field("extract_provider", &self.extract_provider)
            .field("extract_api_key", &"***")
            .field("max_memory_pct", &self.max_memory_pct)
            .field("min_recall_score", &self.min_recall_score)
            .field("key_mapping", &self.key_mapping)
            .field("model_context_sizes", &self.model_context_sizes)
            .field("privacy_mode", &self.privacy_mode)
            .field("anthropic_api_key", &"***")
            .field("anthropic_upstream_url", &self.anthropic_upstream_url)
            .field("extraction_enabled", &self.extraction_enabled)
            .field(
                "allow_client_memory_control",
                &self.allow_client_memory_control,
            )
            .field("passthrough_auth", &self.passthrough_auth)
            .field("passthrough_local_only", &self.passthrough_local_only)
            .field("min_channel_score", &self.min_channel_score)
            .field("diversity_factor", &self.diversity_factor)
            .field("confidence_gate", &self.confidence_gate)
            .field("identifier_first_routing", &self.identifier_first_routing)
            .field("primitive_algebra", &self.primitive_algebra)
            .field("default_memory_mode", &self.default_memory_mode)
            .field("memory_after_date", &self.memory_after_date)
            .finish()
    }
}

fn default_proxy_upstream() -> String {
    "https://api.openai.com/v1".to_string()
}
fn default_extract_model() -> String {
    "gpt-4o-mini".to_string()
}
fn default_extract_provider() -> String {
    "openai".to_string()
}
fn default_max_memory_pct() -> f64 {
    0.10
}
fn default_min_recall_score() -> f64 {
    0.40
}

impl Config {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let mut config = if path.exists() {
            let content = std::fs::read_to_string(path)?;
            let config: Config = toml::from_str(&content)?;
            config
        } else {
            Config::default()
        };
        // Environment variable overrides for sensitive values
        if let Ok(val) = std::env::var("MEMORYOSS_JWT_SECRET") {
            config.auth.jwt_secret = val;
        }
        if let Ok(val) = std::env::var("MEMORYOSS_AUDIT_HMAC_SECRET") {
            config.auth.audit_hmac_secret = val;
        }
        // Proxy upstream keys: env vars override config, standard env vars as fallback.
        // Priority: MEMORYOSS_PROXY_UPSTREAM_KEY > MEMORYOSS_OPENAI_REAL_KEY > OPENAI_API_KEY
        // The REAL_KEY variant is set by the wizard when it overwrites OPENAI_API_KEY with ek_*
        // to avoid circular dependency (proxy would read its own ek_* key as upstream).
        if let Ok(val) = std::env::var("MEMORYOSS_PROXY_UPSTREAM_KEY")
            .or_else(|_| std::env::var("MEMORYOSS_OPENAI_REAL_KEY"))
            .or_else(|_| std::env::var("OPENAI_API_KEY"))
        {
            config.proxy.upstream_api_key = Some(val);
        }
        if let Ok(val) = std::env::var("MEMORYOSS_PROXY_ANTHROPIC_KEY")
            .or_else(|_| std::env::var("MEMORYOSS_ANTHROPIC_REAL_KEY"))
            .or_else(|_| std::env::var("ANTHROPIC_API_KEY"))
        {
            config.proxy.anthropic_api_key = Some(val);
        }

        config.validate()?;
        Ok(config)
    }

    /// Validate config values that serde can't check.
    fn validate(&self) -> anyhow::Result<()> {
        if self.auth.jwt_expiry_secs == 0 {
            anyhow::bail!("auth.jwt_expiry_secs must be > 0");
        }
        if self.auth.jwt_secret.len() < 32 {
            anyhow::bail!(
                "auth.jwt_secret too short ({}). Set a 32+ char secret in config or MEMORYOSS_JWT_SECRET env var.",
                self.auth.jwt_secret.len()
            );
        }
        if self.auth.audit_hmac_secret.len() < 32 {
            anyhow::bail!(
                "auth.audit_hmac_secret too short ({}). Set a 32+ char secret in config or MEMORYOSS_AUDIT_HMAC_SECRET env var.",
                self.auth.audit_hmac_secret.len()
            );
        }
        if self.trust.threshold < 0.0 || self.trust.threshold > 1.0 {
            anyhow::bail!("trust.threshold must be in [0.0, 1.0]");
        }
        if self.limits.rate_limit_per_sec == 0 {
            anyhow::bail!("limits.rate_limit_per_sec must be > 0");
        }
        if self.limits.max_content_bytes == 0 {
            anyhow::bail!("limits.max_content_bytes must be > 0");
        }
        if self.limits.group_commit_batch_size == 0 {
            anyhow::bail!("limits.group_commit_batch_size must be > 0");
        }
        if !(0.0..=1.0).contains(&self.consolidation.threshold) {
            anyhow::bail!("consolidation.threshold must be in [0.0, 1.0]");
        }
        if self.consolidation.max_clusters == 0 {
            anyhow::bail!("consolidation.max_clusters must be > 0");
        }
        if self.proxy.max_memory_pct < 0.0 || self.proxy.max_memory_pct > 1.0 {
            anyhow::bail!("proxy.max_memory_pct must be in [0.0, 1.0]");
        }
        if self.tls.cert_path.is_some() != self.tls.key_path.is_some() {
            anyhow::bail!("tls.cert_path and tls.key_path must be set together");
        }
        if self.tls.enabled
            && !self.tls.auto_generate
            && (self.tls.cert_path.is_none() || self.tls.key_path.is_none())
        {
            anyhow::bail!(
                "tls.enabled=true requires either tls.auto_generate=true or both tls.cert_path and tls.key_path"
            );
        }
        if self.server.hybrid_mode {
            let core_port = self.server.core_port();
            if core_port == self.server.port {
                anyhow::bail!(
                    "server.core_port must differ from server.port when hybrid_mode=true"
                );
            }
        }
        Ok(())
    }

    pub fn bind_addr(&self) -> String {
        format!("{}:{}", self.server.host, self.server.port)
    }

    pub fn core_bind_addr(&self) -> String {
        format!("127.0.0.1:{}", self.server.core_port())
    }
}

impl ServerConfig {
    pub fn core_port(&self) -> u16 {
        self.core_port.unwrap_or_else(|| {
            self.port
                .checked_add(1)
                .unwrap_or(self.port.saturating_sub(1))
        })
    }
}
