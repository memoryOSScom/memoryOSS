// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 memoryOSS Contributors

use std::collections::HashMap;
use std::sync::RwLock;
use std::sync::atomic::{AtomicU64, Ordering};

use redb::{Database, TableDefinition};

/// Redb table for persisted IDF state (single row: serialized blob).
const IDF_TABLE: TableDefinition<&str, &[u8]> = TableDefinition::new("_idf_state");

/// Global IDF (Inverse Document Frequency) index.
/// Maintains term→document_count mapping for all memories,
/// updated incrementally on store/update/forget.
/// State is materialized in redb for fast startup (no full rebuild needed).
pub struct IdfIndex {
    /// term -> number of documents containing the term
    doc_freq: RwLock<HashMap<String, u64>>,
    /// total number of documents
    total_docs: RwLock<u64>,
    /// Dirty counter: incremented on each mutation, used for async persistence
    dirty_count: AtomicU64,
    /// Last persisted dirty_count
    last_persisted: AtomicU64,
    /// Last outbox seq factored into this index (for replay skipping).
    pub materialized_seq: AtomicU64,
}

/// Serializable IDF snapshot for redb persistence.
#[derive(serde::Serialize, serde::Deserialize)]
struct IdfSnapshot {
    doc_freq: HashMap<String, u64>,
    total_docs: u64,
    /// Last outbox sequence that was factored into this snapshot.
    /// Used by the indexer to skip already-counted events on restart.
    #[serde(default)]
    last_seq: u64,
}

impl IdfIndex {
    pub fn new() -> Self {
        Self {
            doc_freq: RwLock::new(HashMap::new()),
            total_docs: RwLock::new(0),
            dirty_count: AtomicU64::new(0),
            last_persisted: AtomicU64::new(0),
            materialized_seq: AtomicU64::new(0),
        }
    }

    /// Try to load IDF state from redb. Returns true if loaded successfully.
    pub fn load_from_redb(&self, db: &Database) -> bool {
        let read_txn = match db.begin_read() {
            Ok(t) => t,
            Err(_) => return false,
        };
        let table = match read_txn.open_table(IDF_TABLE) {
            Ok(t) => t,
            Err(_) => return false,
        };
        let value = match table.get("idf_state") {
            Ok(Some(v)) => v.value().to_vec(),
            _ => return false,
        };
        match rmp_serde::from_slice::<IdfSnapshot>(&value) {
            Ok(snapshot) => {
                let mut df = self.doc_freq.write().unwrap();
                let mut total = self.total_docs.write().unwrap();
                *df = snapshot.doc_freq;
                *total = snapshot.total_docs;
                self.materialized_seq
                    .store(snapshot.last_seq, Ordering::Relaxed);
                tracing::info!(
                    total_docs = *total,
                    unique_terms = df.len(),
                    last_seq = snapshot.last_seq,
                    "IDF index loaded from redb (materialized view)"
                );
                true
            }
            Err(e) => {
                tracing::warn!("Failed to deserialize IDF snapshot: {e}");
                false
            }
        }
    }

    /// Persist current IDF state to redb (called async by indexer).
    /// `current_seq` is the last outbox sequence processed.
    pub fn persist_to_redb(&self, db: &Database) -> anyhow::Result<()> {
        let current_dirty = self.dirty_count.load(Ordering::Relaxed);
        let last = self.last_persisted.load(Ordering::Relaxed);
        if current_dirty == last {
            return Ok(()); // Nothing new to persist
        }

        let snapshot = {
            let df = self.doc_freq.read().unwrap();
            let total = *self.total_docs.read().unwrap();
            let last_seq = self.materialized_seq.load(Ordering::Relaxed);
            IdfSnapshot {
                doc_freq: df.clone(),
                total_docs: total,
                last_seq,
            }
        };

        let bytes = rmp_serde::to_vec(&snapshot)?;
        let write_txn = db.begin_write()?;
        {
            let mut table = write_txn.open_table(IDF_TABLE)?;
            table.insert("idf_state", bytes.as_slice())?;
        }
        write_txn.commit()?;

        self.last_persisted.store(current_dirty, Ordering::Relaxed);
        tracing::debug!(
            total_docs = snapshot.total_docs,
            unique_terms = snapshot.doc_freq.len(),
            "IDF index persisted to redb"
        );
        Ok(())
    }

    /// Check if there are unpersisted changes.
    pub fn is_dirty(&self) -> bool {
        self.dirty_count.load(Ordering::Relaxed) != self.last_persisted.load(Ordering::Relaxed)
    }

    /// Compute IDF for a term: log(N / df(t)), with smoothing.
    /// Returns 0.0 if term not found (infinite IDF capped).
    pub fn idf(&self, term: &str) -> f64 {
        let normalized = term.to_lowercase();
        let total = *self.total_docs.read().unwrap();
        let df = self.doc_freq.read().unwrap();
        let doc_count = df.get(&normalized).copied().unwrap_or(0);

        if total == 0 || doc_count == 0 {
            // Unknown term gets max IDF (very discriminative)
            return if total > 0 {
                (total as f64 + 1.0).ln()
            } else {
                1.0
            };
        }

        // Smoothed IDF: log((N + 1) / (df + 1)) + 1
        ((total as f64 + 1.0) / (doc_count as f64 + 1.0)).ln() + 1.0
    }

    /// Get IDF scores for multiple terms.
    pub fn idf_batch(&self, terms: &[&str]) -> Vec<(String, f64)> {
        terms.iter().map(|t| (t.to_string(), self.idf(t))).collect()
    }

    /// Add a document's terms to the index.
    pub fn add_document(&self, content: &str) {
        let terms = Self::extract_terms(content);
        let mut df = self.doc_freq.write().unwrap();
        let mut total = self.total_docs.write().unwrap();
        *total += 1;
        for term in terms {
            *df.entry(term).or_insert(0) += 1;
        }
        self.dirty_count.fetch_add(1, Ordering::Relaxed);
    }

    /// Remove a document's terms from the index.
    pub fn remove_document(&self, content: &str) {
        let terms = Self::extract_terms(content);
        let mut df = self.doc_freq.write().unwrap();
        let mut total = self.total_docs.write().unwrap();
        if *total > 0 {
            *total -= 1;
        }
        for term in terms {
            if let Some(count) = df.get_mut(&term) {
                *count = count.saturating_sub(1);
                if *count == 0 {
                    df.remove(&term);
                }
            }
        }
        self.dirty_count.fetch_add(1, Ordering::Relaxed);
    }

    /// Rebuild from a set of documents. Replaces all state.
    pub fn rebuild(&self, documents: &[String]) {
        let mut df = self.doc_freq.write().unwrap();
        let mut total = self.total_docs.write().unwrap();
        df.clear();
        *total = documents.len() as u64;

        for content in documents {
            let terms = Self::extract_terms(content);
            for term in terms {
                *df.entry(term).or_insert(0) += 1;
            }
        }

        tracing::info!(
            "IDF index rebuilt: {} docs, {} unique terms",
            *total,
            df.len()
        );
        self.dirty_count.fetch_add(1, Ordering::Relaxed);
    }

    /// Get stats: (total_docs, unique_terms).
    pub fn stats(&self) -> (u64, usize) {
        let total = *self.total_docs.read().unwrap();
        let terms = self.doc_freq.read().unwrap().len();
        (total, terms)
    }

    /// Top-K most common terms (lowest IDF = least discriminative).
    pub fn most_common(&self, k: usize) -> Vec<(String, u64, f64)> {
        let df = self.doc_freq.read().unwrap();
        let total = *self.total_docs.read().unwrap();
        let mut entries: Vec<_> = df.iter().map(|(t, c)| (t.clone(), *c)).collect();
        entries.sort_by(|a, b| b.1.cmp(&a.1));
        entries.truncate(k);
        entries
            .into_iter()
            .map(|(term, count)| {
                let idf = if total > 0 && count > 0 {
                    ((total as f64 + 1.0) / (count as f64 + 1.0)).ln() + 1.0
                } else {
                    1.0
                };
                (term, count, idf)
            })
            .collect()
    }

    /// Top-K rarest terms (highest IDF = most discriminative).
    pub fn most_rare(&self, k: usize) -> Vec<(String, u64, f64)> {
        let df = self.doc_freq.read().unwrap();
        let total = *self.total_docs.read().unwrap();
        let mut entries: Vec<_> = df.iter().map(|(t, c)| (t.clone(), *c)).collect();
        entries.sort_by(|a, b| a.1.cmp(&b.1));
        entries.truncate(k);
        entries
            .into_iter()
            .map(|(term, count)| {
                let idf = if total > 0 && count > 0 {
                    ((total as f64 + 1.0) / (count as f64 + 1.0)).ln() + 1.0
                } else {
                    1.0
                };
                (term, count, idf)
            })
            .collect()
    }

    /// Extract unique terms from content (lowercase, split on whitespace/punctuation).
    fn extract_terms(content: &str) -> std::collections::HashSet<String> {
        content
            .to_lowercase()
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| w.len() >= 2) // skip single chars
            .map(|w| w.to_string())
            .collect()
    }
}
