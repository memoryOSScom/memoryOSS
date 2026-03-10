// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 memoryOSS Contributors

use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use redb::{Database, ReadableTable, TableDefinition};
use serde::{Deserialize, Serialize};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;
use uuid::Uuid;

const AUDIT_TABLE: TableDefinition<u64, &[u8]> = TableDefinition::new("_audit_log");
const AUDIT_SEQ_TABLE: TableDefinition<&str, u64> = TableDefinition::new("_audit_seq");

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEntry {
    pub seq: u64,
    pub timestamp: DateTime<Utc>,
    pub action: AuditAction,
    pub namespace: String,
    pub subject: String,
    pub memory_id: Option<Uuid>,
    pub prev_hash: String,
    pub hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AuditAction {
    Store,
    Recall,
    Update,
    Forget,
}

impl std::fmt::Display for AuditAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Store => write!(f, "store"),
            Self::Recall => write!(f, "recall"),
            Self::Update => write!(f, "update"),
            Self::Forget => write!(f, "forget"),
        }
    }
}

pub struct AuditLog;

impl AuditLog {
    pub fn append(
        db: &Database,
        action: AuditAction,
        namespace: &str,
        subject: &str,
        memory_id: Option<Uuid>,
        secret: &[u8],
    ) -> anyhow::Result<AuditEntry> {
        let tx = db.begin_write()?;
        let entry = {
            let mut seq_table = tx.open_table(AUDIT_SEQ_TABLE)?;
            let mut log_table = tx.open_table(AUDIT_TABLE)?;

            let seq = seq_table.get("seq")?.map(|v| v.value()).unwrap_or(0) + 1;

            let prev_hash = if seq > 1 {
                let prev = log_table.get(seq - 1)?;
                match prev {
                    Some(data) => {
                        let prev_entry: AuditEntry = rmp_serde::from_slice(data.value())?;
                        prev_entry.hash
                    }
                    None => {
                        // Distinct marker for missing predecessor (not genesis)
                        let marker = format!("MISSING_SEQ_{}", seq - 1);
                        let mut mac = HmacSha256::new_from_slice(secret)
                            .map_err(|e| anyhow::anyhow!("HMAC init failed: {e}"))?;
                        mac.update(marker.as_bytes());
                        hex::encode(mac.finalize().into_bytes())
                    }
                }
            } else {
                "0".repeat(64)
            };

            let mut entry = AuditEntry {
                seq,
                timestamp: Utc::now(),
                action,
                namespace: namespace.to_string(),
                subject: subject.to_string(),
                memory_id,
                prev_hash: prev_hash.clone(),
                hash: String::new(),
            };

            // Compute HMAC-SHA256(seq || prev_hash || action || namespace || subject || memory_id || timestamp)
            if secret.is_empty() {
                anyhow::bail!("HMAC secret must not be empty — set auth.jwt_secret in config");
            }
            let mut mac = HmacSha256::new_from_slice(secret)
                .map_err(|e| anyhow::anyhow!("HMAC init failed: {e}"))?;
            mac.update(&seq.to_le_bytes());
            mac.update(prev_hash.as_bytes());
            mac.update(entry.action.to_string().as_bytes());
            mac.update(entry.namespace.as_bytes());
            mac.update(entry.subject.as_bytes());
            mac.update(
                entry
                    .memory_id
                    .map(|id| id.to_string())
                    .unwrap_or_default()
                    .as_bytes(),
            );
            mac.update(entry.timestamp.to_rfc3339().as_bytes());
            entry.hash = hex::encode(mac.finalize().into_bytes());

            let serialized = rmp_serde::to_vec_named(&entry)?;
            log_table.insert(seq, serialized.as_slice())?;
            seq_table.insert("seq", seq)?;

            entry
        };
        tx.commit()?;
        Ok(entry)
    }
}
