// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 memoryOSS Contributors

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::Notify;

use crate::embedding::EmbeddingEngine;
use crate::engines::document::{DocumentEngine, OutboxEvent};
use crate::engines::fts::FtsEngine;
use crate::engines::space_index::SpaceIndex;
use crate::engines::vector::VectorEngine;
use crate::merger::IdfIndex;

/// Maximum retries for failed indexer operations before skipping.
const MAX_INDEX_RETRIES: usize = 3;

/// Tracks the last processed outbox sequence for each derived index engine.
pub struct IndexerState {
    pub vector_seq: AtomicU64,
    pub fts_seq: AtomicU64,
    pub write_seq: AtomicU64,
    notify: Notify,
}

impl IndexerState {
    pub fn new() -> Self {
        Self {
            vector_seq: AtomicU64::new(0),
            fts_seq: AtomicU64::new(0),
            write_seq: AtomicU64::new(0),
            notify: Notify::new(),
        }
    }

    /// Signal that new outbox events are available.
    pub fn wake(&self) {
        self.notify.notify_one();
    }

    /// Current lag: write_seq - min(vector_seq, fts_seq).
    pub fn lag(&self) -> u64 {
        let w = self.write_seq.load(Ordering::Relaxed);
        let v = self.vector_seq.load(Ordering::Relaxed);
        let f = self.fts_seq.load(Ordering::Relaxed);
        w.saturating_sub(v.min(f))
    }
}

/// Spawn the background indexer pipeline. Returns a handle to the indexer state.
pub fn spawn_indexer(
    doc_engine: Arc<DocumentEngine>,
    vector_engine: Arc<VectorEngine>,
    fts_engine: Arc<FtsEngine>,
    embedding: Arc<EmbeddingEngine>,
    state: Arc<IndexerState>,
    idf_index: Arc<IdfIndex>,
    space_index: Arc<SpaceIndex>,
) -> tokio::task::JoinHandle<()> {
    let state2 = state.clone();
    tokio::spawn(async move {
        indexer_loop(
            doc_engine,
            vector_engine,
            fts_engine,
            embedding,
            state2,
            idf_index,
            space_index,
        )
        .await;
    })
}

async fn indexer_loop(
    doc_engine: Arc<DocumentEngine>,
    vector_engine: Arc<VectorEngine>,
    fts_engine: Arc<FtsEngine>,
    _embedding: Arc<EmbeddingEngine>,
    state: Arc<IndexerState>,
    idf_index: Arc<IdfIndex>,
    space_index: Arc<SpaceIndex>,
) {
    // Resume from last checkpoint (avoids replaying entire outbox on restart)
    let checkpoint = doc_engine.load_indexer_checkpoint();
    let mut last_processed: u64 = checkpoint;
    if checkpoint > 0 {
        tracing::info!("Indexer: resuming from checkpoint seq={checkpoint}");
        state.vector_seq.store(checkpoint, Ordering::Relaxed);
        state.fts_seq.store(checkpoint, Ordering::Relaxed);
    }
    // Skip IDF updates for events already factored into the materialized view
    let idf_skip_until = idf_index.materialized_seq.load(Ordering::Relaxed);

    loop {
        // Wait for notification or poll every 100ms
        tokio::select! {
            _ = state.notify.notified() => {},
            _ = tokio::time::sleep(std::time::Duration::from_millis(100)) => {},
        }

        // Consume outbox events
        let events = match doc_engine.consume_outbox(last_processed + 1) {
            Ok(events) => events,
            Err(e) => {
                tracing::warn!("Indexer: failed to read outbox: {e}");
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                continue;
            }
        };

        if events.is_empty() {
            continue;
        }

        for (seq, event) in &events {
            match event {
                OutboxEvent::Store {
                    memory_id,
                    namespace,
                }
                | OutboxEvent::Update {
                    memory_id,
                    namespace,
                } => {
                    // Fetch full memory from SoT (with retry)
                    let memory = {
                        let mut result = None;
                        for attempt in 0..MAX_INDEX_RETRIES {
                            match doc_engine.get(*memory_id, namespace) {
                                Ok(Some(m)) => {
                                    result = Some(m);
                                    break;
                                }
                                Ok(None) => {
                                    tracing::debug!(
                                        "Indexer: memory {} not found (likely deleted), skipping",
                                        memory_id
                                    );
                                    break;
                                }
                                Err(e) => {
                                    tracing::warn!(
                                        "Indexer: failed to fetch memory {} (attempt {}/{}): {e}",
                                        memory_id,
                                        attempt + 1,
                                        MAX_INDEX_RETRIES
                                    );
                                    if attempt + 1 < MAX_INDEX_RETRIES {
                                        tokio::time::sleep(std::time::Duration::from_millis(
                                            50 * (attempt as u64 + 1),
                                        ))
                                        .await;
                                    }
                                }
                            }
                        }
                        result
                    };

                    if let Some(memory) = memory {
                        // Vector index (with retry)
                        if let Some(ref emb) = memory.embedding {
                            let mut vec_ok = false;
                            for attempt in 0..MAX_INDEX_RETRIES {
                                match vector_engine.add(memory.id, emb) {
                                    Ok(()) => {
                                        vec_ok = true;
                                        break;
                                    }
                                    Err(e) => {
                                        tracing::warn!(
                                            "Indexer: vector add failed for {} (attempt {}/{}): {e}",
                                            memory.id,
                                            attempt + 1,
                                            MAX_INDEX_RETRIES
                                        );
                                    }
                                }
                            }
                            if !vec_ok {
                                tracing::error!(
                                    "Indexer: vector add permanently failed for {}, skipping",
                                    memory.id
                                );
                            }
                        }
                        state.vector_seq.store(*seq, Ordering::Relaxed);

                        // FTS index (with retry, using full metadata)
                        let mut fts_ok = false;
                        for attempt in 0..MAX_INDEX_RETRIES {
                            let meta = crate::engines::fts::MemoryMetadata {
                                agent: memory.agent.as_deref(),
                                session: memory.session.as_deref(),
                                memory_type: Some(&format!("{:?}", memory.memory_type)),
                                source_key: memory.source_key.as_deref(),
                            };
                            match fts_engine.add_with_metadata(
                                memory.id,
                                &memory.content,
                                &memory.tags,
                                &meta,
                            ) {
                                Ok(()) => {
                                    fts_ok = true;
                                    break;
                                }
                                Err(e) => {
                                    tracing::warn!(
                                        "Indexer: FTS add failed for {} (attempt {}/{}): {e}",
                                        memory.id,
                                        attempt + 1,
                                        MAX_INDEX_RETRIES
                                    );
                                }
                            }
                        }
                        if !fts_ok {
                            tracing::error!(
                                "Indexer: FTS add permanently failed for {}, skipping",
                                memory.id
                            );
                        }
                        state.fts_seq.store(*seq, Ordering::Relaxed);

                        // IDF index — incremental update (skip if replaying)
                        if *seq > idf_skip_until {
                            idf_index.add_document(&memory.content);
                        }

                        // Space index — metadata tracking
                        if matches!(event, OutboxEvent::Update { .. }) {
                            space_index.update(&memory);
                        } else {
                            space_index.add(&memory);
                        }
                    } else {
                        // Memory not found or fetch permanently failed — advance seqs
                        state.vector_seq.store(*seq, Ordering::Relaxed);
                        state.fts_seq.store(*seq, Ordering::Relaxed);
                    }
                }
                OutboxEvent::Delete {
                    memory_id,
                    namespace,
                } => {
                    // IDF removal (skip if replaying materialized events)
                    if *seq > idf_skip_until
                        && let Ok(Some(memory)) = doc_engine.get(*memory_id, namespace)
                    {
                        idf_index.remove_document(&memory.content);
                    }
                    let _ = vector_engine.remove(*memory_id);
                    state.vector_seq.store(*seq, Ordering::Relaxed);
                    let _ = fts_engine.remove(*memory_id);
                    state.fts_seq.store(*seq, Ordering::Relaxed);
                    space_index.remove(*memory_id);
                }
            }

            last_processed = *seq;
        }

        // Batch commit FTS index (M15: avoid per-document commits)
        if let Err(e) = fts_engine.commit() {
            tracing::warn!("Indexer: FTS batch commit failed: {e}");
        }

        // Persist vector key mappings to survive restarts
        if let Err(e) = vector_engine.persist_mappings() {
            tracing::warn!("Indexer: vector key mapping persist failed: {e}");
        }

        // Update materialized seq for IDF persistence
        idf_index
            .materialized_seq
            .store(last_processed, Ordering::Relaxed);

        // Persist IDF materialized view every batch (minimize crash window)
        if idf_index.is_dirty()
            && let Err(e) = idf_index.persist_to_redb(doc_engine.db())
        {
            tracing::warn!("Failed to persist IDF index: {e}");
        }

        // Save indexer checkpoint to redb (faster crash recovery)
        if let Err(e) = doc_engine.save_indexer_checkpoint(last_processed) {
            tracing::warn!("Failed to save indexer checkpoint: {e}");
        }

        // GC: delete processed outbox entries to prevent unbounded growth
        if let Err(e) = doc_engine.gc_outbox(last_processed) {
            tracing::warn!("Failed to GC outbox: {e}");
        }

        tracing::debug!(
            "Indexer: processed {} events, seq now at {}",
            events.len(),
            last_processed
        );
    }
}
