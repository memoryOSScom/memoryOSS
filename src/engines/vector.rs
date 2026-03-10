// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 memoryOSS Contributors

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

use usearch::{Index, IndexOptions, MetricKind, ScalarKind};
use uuid::Uuid;

/// Internal state protected by a single Mutex to prevent lock-ordering issues.
struct VectorState {
    index: Index,
    key_to_uuid: HashMap<u64, Uuid>,
    uuid_to_key: HashMap<Uuid, u64>,
    next_key: u64,
}

pub struct VectorEngine {
    state: Mutex<VectorState>,
    data_dir: std::path::PathBuf,
}

impl VectorEngine {
    pub fn open(data_dir: &Path, dimension: usize) -> anyhow::Result<Self> {
        let path = data_dir.join("vectors.usearch");

        let options = IndexOptions {
            dimensions: dimension,
            metric: MetricKind::Cos,
            quantization: ScalarKind::F32,
            ..Default::default()
        };

        let index = Index::new(&options)?;

        if path.exists() {
            index.load(
                path.to_str()
                    .ok_or_else(|| anyhow::anyhow!("non-UTF8 vector index path"))?,
            )?;
            tracing::info!(
                "Loaded vector index: {} vectors, {dimension}-dim",
                index.size()
            );
        } else {
            index.reserve(10_000)?;
            tracing::info!("Created new vector index: {dimension}-dim");
        }

        // Load persisted key mappings if available
        let mappings_path = data_dir.join("vector_keys.json");
        let (key_to_uuid, uuid_to_key, next_key) = if mappings_path.exists() {
            let data = std::fs::read_to_string(&mappings_path)?;
            let saved: SavedMappings = serde_json::from_str(&data)?;
            let mut k2u = HashMap::with_capacity(saved.mappings.len());
            let mut u2k = HashMap::with_capacity(saved.mappings.len());
            for (k, u) in &saved.mappings {
                k2u.insert(*k, *u);
                u2k.insert(*u, *k);
            }
            tracing::info!("Restored {} vector key mappings", saved.mappings.len());
            (k2u, u2k, saved.next_key)
        } else {
            (HashMap::new(), HashMap::new(), 1)
        };

        Ok(Self {
            state: Mutex::new(VectorState {
                index,
                key_to_uuid,
                uuid_to_key,
                next_key,
            }),
            data_dir: data_dir.to_path_buf(),
        })
    }

    pub fn add(&self, id: Uuid, embedding: &[f32]) -> anyhow::Result<()> {
        let mut st = self
            .state
            .lock()
            .map_err(|e| anyhow::anyhow!("lock: {e}"))?;

        // If UUID already exists, remove old entry first
        if let Some(old_key) = st.uuid_to_key.remove(&id) {
            st.key_to_uuid.remove(&old_key);
            let _ = st.index.remove(old_key);
        }

        let key = st.next_key;
        st.next_key += 1;

        st.key_to_uuid.insert(key, id);
        st.uuid_to_key.insert(id, key);

        if st.index.size() >= st.index.capacity() {
            st.index.reserve(st.index.capacity() + 10_000)?;
        }

        st.index.add(key, embedding)?;
        Ok(())
    }

    pub fn search(&self, query: &[f32], limit: usize) -> anyhow::Result<Vec<(Uuid, f32)>> {
        let st = self
            .state
            .lock()
            .map_err(|e| anyhow::anyhow!("lock: {e}"))?;

        if st.index.size() == 0 {
            return Ok(Vec::new());
        }

        let results = st.index.search(query, limit)?;

        let mut out = Vec::with_capacity(results.keys.len());
        for (key, distance) in results.keys.iter().zip(results.distances.iter()) {
            if let Some(uuid) = st.key_to_uuid.get(key) {
                let similarity = 1.0 - distance;
                out.push((*uuid, similarity));
            }
        }

        Ok(out)
    }

    pub fn remove(&self, id: Uuid) -> anyhow::Result<bool> {
        let mut st = self
            .state
            .lock()
            .map_err(|e| anyhow::anyhow!("lock: {e}"))?;

        if let Some(key) = st.uuid_to_key.remove(&id) {
            st.key_to_uuid.remove(&key);
            let removed = st.index.remove(key).unwrap_or(0);
            Ok(removed > 0)
        } else {
            Ok(false)
        }
    }

    pub fn size(&self) -> usize {
        self.state.lock().map(|st| st.index.size()).unwrap_or(0)
    }

    /// Persist key mappings to disk. Called periodically by indexer.
    pub fn persist_mappings(&self) -> anyhow::Result<()> {
        let st = self
            .state
            .lock()
            .map_err(|e| anyhow::anyhow!("lock: {e}"))?;

        let saved = SavedMappings {
            next_key: st.next_key,
            mappings: st.key_to_uuid.iter().map(|(k, v)| (*k, *v)).collect(),
        };

        let path = self.data_dir.join("vector_keys.json");
        let tmp = self.data_dir.join("vector_keys.json.tmp");
        std::fs::write(&tmp, serde_json::to_string(&saved)?)?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }

    /// Rebuild from document engine data. Call on startup to restore key mappings.
    pub fn rebuild(&self, memories: &[(Uuid, Vec<f32>)]) -> anyhow::Result<()> {
        let mut st = self
            .state
            .lock()
            .map_err(|e| anyhow::anyhow!("lock: {e}"))?;

        st.key_to_uuid.clear();
        st.uuid_to_key.clear();
        st.next_key = 1;

        // Reset the usearch index to remove ghost vectors, then reserve
        st.index.reset()?;
        st.index.reserve(memories.len().max(1000))?;

        for (uuid, embedding) in memories {
            let key = st.next_key;
            st.next_key += 1;
            st.index.add(key, embedding)?;
            st.key_to_uuid.insert(key, *uuid);
            st.uuid_to_key.insert(*uuid, key);
        }

        tracing::info!("Rebuilt vector index: {} vectors", memories.len());
        Ok(())
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
struct SavedMappings {
    next_key: u64,
    mappings: Vec<(u64, Uuid)>,
}
