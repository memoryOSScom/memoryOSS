// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 memoryOSS Contributors

use std::path::Path;
use std::sync::Arc;

use chrono::Utc;
use redb::{Database, ReadableTable, TableDefinition, TableHandle};
use uuid::Uuid;

use crate::memory::{Memory, MemoryType};
use crate::security::audit::{AuditAction, AuditLog};
use crate::security::encryption::Encryptor;

// Outbox for async indexers
const OUTBOX_TABLE: TableDefinition<u64, &[u8]> = TableDefinition::new("_outbox");
const OUTBOX_SEQ_TABLE: TableDefinition<&str, u64> = TableDefinition::new("_outbox_seq");
// Indexer checkpoint: last processed outbox sequence (survives crash)
const INDEXER_CHECKPOINT_TABLE: TableDefinition<&str, u64> =
    TableDefinition::new("_indexer_checkpoint");

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum OutboxEvent {
    Store { memory_id: Uuid, namespace: String },
    Update { memory_id: Uuid, namespace: String },
    Delete { memory_id: Uuid, namespace: String },
}

pub struct DocumentEngine {
    db: Arc<Database>,
    encryptor: Encryptor,
    hmac_secret: Vec<u8>,
}

impl DocumentEngine {
    pub fn open(data_dir: &Path) -> anyhow::Result<Self> {
        Self::open_with_config(data_dir, &crate::config::EncryptionConfig::default(), &[])
    }

    pub fn open_with_config(
        data_dir: &Path,
        encryption_config: &crate::config::EncryptionConfig,
        hmac_secret: &[u8],
    ) -> anyhow::Result<Self> {
        std::fs::create_dir_all(data_dir)?;
        let db_path = data_dir.join("memoryoss.redb");
        let db = Database::create(&db_path)?;
        let encryptor = Encryptor::from_config(encryption_config, data_dir)?;

        // Run migrations
        crate::migration::auto_migrate(&db)?;

        Ok(Self {
            db: Arc::new(db),
            encryptor,
            hmac_secret: hmac_secret.to_vec(),
        })
    }

    pub fn db(&self) -> &Database {
        &self.db
    }

    pub fn encryptor(&self) -> &Encryptor {
        &self.encryptor
    }

    fn table_name(namespace: &str) -> String {
        format!("docs_{namespace}")
    }

    /// List all namespaces by discovering docs_* tables in redb.
    pub fn list_namespaces(&self) -> anyhow::Result<Vec<String>> {
        let tx = self.db.begin_read()?;
        let mut namespaces = Vec::new();
        for table in tx.list_tables()? {
            let name = table.name();
            if let Some(ns) = name.strip_prefix("docs_") {
                namespaces.push(ns.to_string());
            }
        }
        Ok(namespaces)
    }

    pub fn store(&self, memory: &Memory, subject: &str) -> anyhow::Result<()> {
        let namespace = memory.namespace.as_deref().unwrap_or("default");
        let table_name = Self::table_name(namespace);
        let table_def: TableDefinition<&[u8; 16], &[u8]> = TableDefinition::new(&table_name);

        // Serialize and encrypt
        let serialized = serde_json::to_vec(memory)?;
        let encrypted = self.encryptor.encrypt_ns(&serialized, namespace)?;

        let tx = self.db.begin_write()?;
        {
            let mut doc_table = tx.open_table(table_def)?;
            doc_table.insert(memory.id.as_bytes(), encrypted.as_slice())?;

            // Outbox event
            let mut seq_table = tx.open_table(OUTBOX_SEQ_TABLE)?;
            let seq = seq_table.get("seq")?.map(|v| v.value()).unwrap_or(0) + 1;
            let event = OutboxEvent::Store {
                memory_id: memory.id,
                namespace: namespace.to_string(),
            };
            let event_bytes = serde_json::to_vec(&event)?;
            let mut outbox = tx.open_table(OUTBOX_TABLE)?;
            outbox.insert(seq, event_bytes.as_slice())?;
            seq_table.insert("seq", seq)?;
        }
        tx.commit()?;

        // Audit log (separate TX — audit failure shouldn't block writes)
        if let Err(e) = AuditLog::append(
            &self.db,
            AuditAction::Store,
            namespace,
            subject,
            Some(memory.id),
            &self.hmac_secret,
        ) {
            tracing::error!("Audit log write failed: {e}");
        }

        Ok(())
    }

    pub fn get(&self, id: Uuid, namespace: &str) -> anyhow::Result<Option<Memory>> {
        let table_name = Self::table_name(namespace);
        let table_def: TableDefinition<&[u8; 16], &[u8]> = TableDefinition::new(&table_name);

        let tx = self.db.begin_read()?;
        let table = match tx.open_table(table_def) {
            Ok(t) => t,
            Err(redb::TableError::TableDoesNotExist(_)) => return Ok(None),
            Err(e) => return Err(e.into()),
        };

        match table.get(id.as_bytes())? {
            Some(data) => {
                let decrypted = self.encryptor.decrypt_ns(data.value(), namespace)?;
                let memory: Memory = serde_json::from_slice(&decrypted)?;
                Ok(Some(memory))
            }
            None => Ok(None),
        }
    }

    pub fn update(
        &self,
        id: Uuid,
        namespace: &str,
        content: Option<String>,
        tags: Option<Vec<String>>,
        memory_type: Option<MemoryType>,
        subject: &str,
    ) -> anyhow::Result<Option<Memory>> {
        let table_name = Self::table_name(namespace);
        let table_def: TableDefinition<&[u8; 16], &[u8]> = TableDefinition::new(&table_name);

        // Read-modify-write in a single write transaction to prevent TOCTOU
        let tx = self.db.begin_write()?;

        // Phase 1: Read existing memory within write TX
        let raw = {
            let doc_table = tx.open_table(table_def)?;
            let result = doc_table.get(id.as_bytes())?;
            match result {
                Some(v) => v.value().to_vec(),
                None => {
                    drop(result);
                    drop(doc_table);
                    let _ = tx.abort();
                    return Ok(None);
                }
            }
        };

        let decrypted = self.encryptor.decrypt_ns(&raw, namespace)?;
        let mut memory: Memory = serde_json::from_slice(&decrypted)?;

        if let Some(c) = content {
            memory.content = c;
            memory.embedding = None; // needs re-embedding
        }
        if let Some(t) = tags {
            memory.tags = t;
        }
        if let Some(mt) = memory_type {
            memory.memory_type = mt;
        }
        memory.version += 1;
        memory.updated_at = Utc::now();

        let serialized = serde_json::to_vec(&memory)?;
        let encrypted = self.encryptor.encrypt_ns(&serialized, namespace)?;

        // Phase 2: Write back within same TX
        {
            let mut doc_table = tx.open_table(table_def)?;
            doc_table.insert(id.as_bytes(), encrypted.as_slice())?;

            // Outbox event
            let mut seq_table = tx.open_table(OUTBOX_SEQ_TABLE)?;
            let seq = seq_table.get("seq")?.map(|v| v.value()).unwrap_or(0) + 1;
            let event = OutboxEvent::Update {
                memory_id: id,
                namespace: namespace.to_string(),
            };
            let event_bytes = serde_json::to_vec(&event)?;
            let mut outbox = tx.open_table(OUTBOX_TABLE)?;
            outbox.insert(seq, event_bytes.as_slice())?;
            seq_table.insert("seq", seq)?;
        }
        tx.commit()?;

        if let Err(e) = AuditLog::append(
            &self.db,
            AuditAction::Update,
            namespace,
            subject,
            Some(id),
            &self.hmac_secret,
        ) {
            tracing::error!("Audit log write failed: {e}");
        }

        Ok(Some(memory))
    }

    pub fn replace(&self, memory: &Memory, subject: &str) -> anyhow::Result<()> {
        let namespace = memory.namespace.as_deref().unwrap_or("default");
        let table_name = Self::table_name(namespace);
        let table_def: TableDefinition<&[u8; 16], &[u8]> = TableDefinition::new(&table_name);

        let serialized = serde_json::to_vec(memory)?;
        let encrypted = self.encryptor.encrypt_ns(&serialized, namespace)?;

        let tx = self.db.begin_write()?;
        {
            let mut doc_table = tx.open_table(table_def)?;
            doc_table.insert(memory.id.as_bytes(), encrypted.as_slice())?;

            let mut seq_table = tx.open_table(OUTBOX_SEQ_TABLE)?;
            let seq = seq_table.get("seq")?.map(|v| v.value()).unwrap_or(0) + 1;
            let event = OutboxEvent::Update {
                memory_id: memory.id,
                namespace: namespace.to_string(),
            };
            let event_bytes = serde_json::to_vec(&event)?;
            let mut outbox = tx.open_table(OUTBOX_TABLE)?;
            outbox.insert(seq, event_bytes.as_slice())?;
            seq_table.insert("seq", seq)?;
        }
        tx.commit()?;

        if let Err(e) = AuditLog::append(
            &self.db,
            AuditAction::Update,
            namespace,
            subject,
            Some(memory.id),
            &self.hmac_secret,
        ) {
            tracing::error!("Audit log write failed: {e}");
        }

        Ok(())
    }

    /// Update only the embedding vector for a memory (used by re-embedding migration).
    pub fn set_embedding(
        &self,
        id: Uuid,
        namespace: &str,
        embedding: Vec<f32>,
    ) -> anyhow::Result<bool> {
        let mut memory = match self.get(id, namespace)? {
            Some(m) => m,
            None => return Ok(false),
        };
        memory.embedding = Some(embedding);

        let table_name = Self::table_name(namespace);
        let table_def: TableDefinition<&[u8; 16], &[u8]> = TableDefinition::new(&table_name);
        let serialized = serde_json::to_vec(&memory)?;
        let encrypted = self.encryptor.encrypt_ns(&serialized, namespace)?;

        let tx = self.db.begin_write()?;
        {
            let mut doc_table = tx.open_table(table_def)?;
            doc_table.insert(id.as_bytes(), encrypted.as_slice())?;
        }
        tx.commit()?;
        Ok(true)
    }

    pub fn delete(&self, id: Uuid, namespace: &str, subject: &str) -> anyhow::Result<bool> {
        let table_name = Self::table_name(namespace);
        let table_def: TableDefinition<&[u8; 16], &[u8]> = TableDefinition::new(&table_name);

        let tx = self.db.begin_write()?;
        let removed = {
            let mut doc_table = tx.open_table(table_def)?;
            let existed = doc_table.remove(id.as_bytes())?.is_some();

            if existed {
                let mut seq_table = tx.open_table(OUTBOX_SEQ_TABLE)?;
                let seq = seq_table.get("seq")?.map(|v| v.value()).unwrap_or(0) + 1;
                let event = OutboxEvent::Delete {
                    memory_id: id,
                    namespace: namespace.to_string(),
                };
                let event_bytes = serde_json::to_vec(&event)?;
                let mut outbox = tx.open_table(OUTBOX_TABLE)?;
                outbox.insert(seq, event_bytes.as_slice())?;
                seq_table.insert("seq", seq)?;
            }

            existed
        };
        tx.commit()?;

        if removed
            && let Err(e) = AuditLog::append(
                &self.db,
                AuditAction::Forget,
                namespace,
                subject,
                Some(id),
                &self.hmac_secret,
            )
        {
            tracing::error!("Audit log write failed: {e}");
        }

        Ok(removed)
    }

    /// Archive a memory: mark as archived in redb and emit Delete event to remove from indexes.
    pub fn archive(&self, id: Uuid, namespace: &str, _subject: &str) -> anyhow::Result<bool> {
        let table_name = Self::table_name(namespace);
        let table_def: TableDefinition<&[u8; 16], &[u8]> = TableDefinition::new(&table_name);

        // Atomic read-modify-write in a single write transaction to avoid TOCTOU.
        // In redb, dropping a WriteTransaction without commit() auto-aborts.
        let tx = self.db.begin_write()?;
        let should_commit = {
            let mut doc_table = match tx.open_table(table_def) {
                Ok(t) => t,
                Err(redb::TableError::TableDoesNotExist(_)) => return Ok(false),
                Err(e) => return Err(e.into()),
            };
            let current = match doc_table.get(id.as_bytes())? {
                Some(data) => data.value().to_vec(),
                None => return Ok(false),
            };
            let decrypted = self.encryptor.decrypt_ns(&current, namespace)?;
            let mut memory: Memory = serde_json::from_slice(&decrypted)?;
            if memory.archived {
                return Ok(false);
            }
            memory.archived = true;
            memory.updated_at = chrono::Utc::now();
            let serialized = serde_json::to_vec(&memory)?;
            let encrypted = self.encryptor.encrypt_ns(&serialized, namespace)?;
            doc_table.insert(id.as_bytes(), encrypted.as_slice())?;

            // Emit Delete event to remove from vector/FTS indexes
            let mut seq_table = tx.open_table(OUTBOX_SEQ_TABLE)?;
            let seq = seq_table.get("seq")?.map(|v| v.value()).unwrap_or(0) + 1;
            let event = OutboxEvent::Delete {
                memory_id: id,
                namespace: namespace.to_string(),
            };
            let event_bytes = serde_json::to_vec(&event)?;
            let mut outbox = tx.open_table(OUTBOX_TABLE)?;
            outbox.insert(seq, event_bytes.as_slice())?;
            seq_table.insert("seq", seq)?;
            true
        };
        if should_commit {
            tx.commit()?;
        }
        Ok(should_commit)
    }

    pub fn search(
        &self,
        namespace: &str,
        agent: Option<&str>,
        session: Option<&str>,
        memory_type: Option<MemoryType>,
        tags: &[String],
    ) -> anyhow::Result<Vec<Memory>> {
        self.search_internal(namespace, agent, session, memory_type, tags, false, None)
    }

    pub fn search_limited(
        &self,
        namespace: &str,
        agent: Option<&str>,
        session: Option<&str>,
        memory_type: Option<MemoryType>,
        tags: &[String],
        limit: usize,
    ) -> anyhow::Result<Vec<Memory>> {
        self.search_internal(
            namespace,
            agent,
            session,
            memory_type,
            tags,
            false,
            Some(limit),
        )
    }

    pub fn list_all(&self, namespace: &str) -> anyhow::Result<Vec<Memory>> {
        self.search(namespace, None, None, None, &[])
    }

    pub fn list_all_including_archived(&self, namespace: &str) -> anyhow::Result<Vec<Memory>> {
        self.search_internal(namespace, None, None, None, &[], true, None)
    }

    fn search_internal(
        &self,
        namespace: &str,
        agent: Option<&str>,
        session: Option<&str>,
        memory_type: Option<MemoryType>,
        tags: &[String],
        include_archived: bool,
        limit: Option<usize>,
    ) -> anyhow::Result<Vec<Memory>> {
        let table_name = Self::table_name(namespace);
        let table_def: TableDefinition<&[u8; 16], &[u8]> = TableDefinition::new(&table_name);

        let tx = self.db.begin_read()?;
        let table = match tx.open_table(table_def) {
            Ok(t) => t,
            Err(redb::TableError::TableDoesNotExist(_)) => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };

        let mut results = Vec::new();
        let iter = table.iter()?;
        for entry in iter {
            let (_key, value) = entry?;
            let decrypted = self.encryptor.decrypt_ns(value.value(), namespace)?;
            let memory: Memory = serde_json::from_slice(&decrypted)?;

            // Skip archived memories
            if memory.archived && !include_archived {
                continue;
            }

            // Filter
            if let Some(a) = agent
                && memory.agent.as_deref() != Some(a)
            {
                continue;
            }
            if let Some(s) = session
                && memory.session.as_deref() != Some(s)
            {
                continue;
            }
            if let Some(mt) = memory_type
                && memory.memory_type != mt
            {
                continue;
            }
            if !tags.is_empty() && !tags.iter().any(|t| memory.tags.contains(t)) {
                continue;
            }

            results.push(memory);
            if let Some(limit) = limit
                && results.len() >= limit
            {
                break;
            }
        }

        Ok(results)
    }

    /// Find a memory by content hash in a namespace. Returns the ID if found.
    pub fn find_by_hash(&self, namespace: &str, hash: &str) -> anyhow::Result<Option<Uuid>> {
        let table_name = Self::table_name(namespace);
        let table_def: TableDefinition<&[u8; 16], &[u8]> = TableDefinition::new(&table_name);

        let tx = self.db.begin_read()?;
        let table = match tx.open_table(table_def) {
            Ok(t) => t,
            Err(redb::TableError::TableDoesNotExist(_)) => return Ok(None),
            Err(e) => return Err(e.into()),
        };

        for entry in table.iter()? {
            let (_key, value) = entry?;
            let decrypted = self.encryptor.decrypt_ns(value.value(), namespace)?;
            let memory: Memory = serde_json::from_slice(&decrypted)?;
            if memory.content_hash.as_deref() == Some(hash) {
                return Ok(Some(memory.id));
            }
        }

        Ok(None)
    }

    /// Store multiple memories in a single redb TX (group commit).
    pub fn store_batch_tx(&self, items: &[(Memory, String)]) -> anyhow::Result<()> {
        if items.is_empty() {
            return Ok(());
        }

        let tx = self.db.begin_write()?;
        {
            let mut seq_table = tx.open_table(OUTBOX_SEQ_TABLE)?;
            let mut seq = seq_table.get("seq")?.map(|v| v.value()).unwrap_or(0);
            let mut outbox = tx.open_table(OUTBOX_TABLE)?;

            for (memory, _subject) in items {
                let namespace = memory.namespace.as_deref().unwrap_or("default");
                let table_name = Self::table_name(namespace);
                let table_def: TableDefinition<&[u8; 16], &[u8]> =
                    TableDefinition::new(&table_name);

                let serialized = serde_json::to_vec(memory)?;
                let encrypted = self.encryptor.encrypt_ns(&serialized, namespace)?;

                let mut doc_table = tx.open_table(table_def)?;
                doc_table.insert(memory.id.as_bytes(), encrypted.as_slice())?;

                // Outbox event
                seq += 1;
                let event = OutboxEvent::Store {
                    memory_id: memory.id,
                    namespace: namespace.to_string(),
                };
                let event_bytes = serde_json::to_vec(&event)?;
                outbox.insert(seq, event_bytes.as_slice())?;
            }
            seq_table.insert("seq", seq)?;
        }
        tx.commit()?;

        // Audit log (batch, separate TX)
        for (memory, subject) in items {
            let namespace = memory.namespace.as_deref().unwrap_or("default");
            if let Err(e) = AuditLog::append(
                &self.db,
                AuditAction::Store,
                namespace,
                subject,
                Some(memory.id),
                &self.hmac_secret,
            ) {
                tracing::error!("Audit log write failed: {e}");
            }
        }

        Ok(())
    }

    /// Save indexer checkpoint (last processed outbox seq) to redb.
    pub fn save_indexer_checkpoint(&self, last_processed: u64) -> anyhow::Result<()> {
        let tx = self.db.begin_write()?;
        {
            let mut table = tx.open_table(INDEXER_CHECKPOINT_TABLE)?;
            table.insert("last_processed", last_processed)?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Load indexer checkpoint from redb. Returns 0 if not found.
    pub fn load_indexer_checkpoint(&self) -> u64 {
        let tx = match self.db.begin_read() {
            Ok(t) => t,
            Err(_) => return 0,
        };
        let table = match tx.open_table(INDEXER_CHECKPOINT_TABLE) {
            Ok(t) => t,
            Err(_) => return 0,
        };
        match table.get("last_processed") {
            Ok(Some(val)) => val.value(),
            _ => 0,
        }
    }

    pub fn consume_outbox(&self, from_seq: u64) -> anyhow::Result<Vec<(u64, OutboxEvent)>> {
        let tx = self.db.begin_read()?;
        let table = match tx.open_table(OUTBOX_TABLE) {
            Ok(t) => t,
            Err(redb::TableError::TableDoesNotExist(_)) => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };

        let mut events = Vec::new();
        let range = table.range(from_seq..)?;
        for entry in range {
            let (seq, data) = entry?;
            let event: OutboxEvent = serde_json::from_slice(data.value())?;
            events.push((seq.value(), event));
        }

        Ok(events)
    }

    /// Delete outbox entries up to and including `through_seq` (garbage collection).
    pub fn gc_outbox(&self, through_seq: u64) -> anyhow::Result<usize> {
        let tx = self.db.begin_write()?;
        let mut count = 0;
        {
            let mut table = tx.open_table(OUTBOX_TABLE)?;
            let keys: Vec<u64> = table
                .range(..=through_seq)?
                .map(|e| e.map(|(k, _)| k.value()))
                .collect::<Result<_, _>>()?;
            for key in &keys {
                table.remove(key)?;
                count += 1;
            }
        }
        tx.commit()?;
        Ok(count)
    }
}
