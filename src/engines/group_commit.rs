// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 memoryOSS Contributors

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use tokio::sync::{mpsc, oneshot};

use crate::engines::document::DocumentEngine;
use crate::engines::indexer::IndexerState;
use crate::memory::Memory;

struct WriteOp {
    memory: Memory,
    subject: String,
    reply: oneshot::Sender<anyhow::Result<()>>,
}

/// Async write batcher: collects store ops and flushes them in a single redb TX.
pub struct GroupCommitter {
    tx: mpsc::Sender<WriteOp>,
    /// Queue capacity (for metrics)
    capacity: usize,
    /// Total flushes completed
    pub flushes: Arc<AtomicU64>,
    /// Total ops committed
    pub ops_committed: Arc<AtomicU64>,
}

impl GroupCommitter {
    pub fn spawn(
        doc_engine: Arc<DocumentEngine>,
        indexer_state: Arc<IndexerState>,
        batch_size: usize,
        flush_ms: u64,
    ) -> Self {
        let capacity = batch_size * 4;
        let (tx, rx) = mpsc::channel::<WriteOp>(capacity);
        let flushes = Arc::new(AtomicU64::new(0));
        let ops_committed = Arc::new(AtomicU64::new(0));
        tokio::spawn(flush_loop(
            rx,
            doc_engine,
            indexer_state,
            batch_size,
            flush_ms,
            flushes.clone(),
            ops_committed.clone(),
        ));
        Self {
            tx,
            capacity,
            flushes,
            ops_committed,
        }
    }

    /// Current queue utilization as a fraction (0.0 to 1.0).
    pub fn queue_utilization(&self) -> f64 {
        let used = self.capacity - self.tx.capacity();
        used as f64 / self.capacity as f64
    }

    /// Submit a store operation. Returns when the group commit TX is flushed.
    pub async fn store(&self, memory: Memory, subject: String) -> anyhow::Result<()> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(WriteOp {
                memory,
                subject,
                reply: reply_tx,
            })
            .await
            .map_err(|_| anyhow::anyhow!("group committer closed"))?;
        reply_rx
            .await
            .map_err(|_| anyhow::anyhow!("group commit reply dropped"))?
    }
}

async fn flush_loop(
    mut rx: mpsc::Receiver<WriteOp>,
    doc_engine: Arc<DocumentEngine>,
    indexer_state: Arc<IndexerState>,
    batch_size: usize,
    flush_ms: u64,
    flushes: Arc<AtomicU64>,
    ops_committed: Arc<AtomicU64>,
) {
    let flush_interval = Duration::from_millis(flush_ms);

    loop {
        // Wait for first write op
        let first = match rx.recv().await {
            Some(op) => op,
            None => return, // channel closed
        };

        let mut batch = Vec::with_capacity(batch_size);
        batch.push(first);

        // Collect more ops until batch full or timeout
        let deadline = tokio::time::Instant::now() + flush_interval;
        while batch.len() < batch_size {
            match tokio::time::timeout_at(deadline, rx.recv()).await {
                Ok(Some(op)) => batch.push(op),
                Ok(None) => break, // channel closed
                Err(_) => break,   // timeout
            }
        }

        // Flush batch in single TX
        let items: Vec<(Memory, String)> = batch
            .iter()
            .map(|op| (op.memory.clone(), op.subject.clone()))
            .collect();

        let count = items.len();
        let result = doc_engine.store_batch_tx(&items);

        // Notify indexer about new writes
        if result.is_ok() {
            indexer_state
                .write_seq
                .fetch_add(count as u64, std::sync::atomic::Ordering::Relaxed);
            indexer_state.wake();
            flushes.fetch_add(1, Ordering::Relaxed);
            ops_committed.fetch_add(count as u64, Ordering::Relaxed);
        }

        // Reply to all callers
        let is_ok = result.is_ok();
        if let Err(ref e) = result {
            tracing::error!("Group commit failed ({count} ops): {e}");
        } else {
            tracing::debug!("Group commit flushed {count} ops");
        }

        for op in batch {
            let reply = if is_ok {
                Ok(())
            } else {
                Err(anyhow::anyhow!("group commit TX failed"))
            };
            let _ = op.reply.send(reply);
        }
    }
}
