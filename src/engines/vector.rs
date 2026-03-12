// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 memoryOSS Contributors

#[cfg(target_os = "windows")]
use std::cmp::Ordering;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

#[cfg(not(target_os = "windows"))]
use usearch::{Index, IndexOptions, MetricKind, ScalarKind};
use uuid::Uuid;

/// Internal state protected by a single Mutex to prevent lock-ordering issues.
#[cfg(not(target_os = "windows"))]
struct VectorState {
    index: Index,
    key_to_uuid: HashMap<u64, Uuid>,
    uuid_to_key: HashMap<Uuid, u64>,
    next_key: u64,
}

#[cfg(target_os = "windows")]
struct VectorState {
    dimension: usize,
    vectors: HashMap<Uuid, Vec<f32>>,
}

pub struct VectorEngine {
    state: Mutex<VectorState>,
    data_dir: std::path::PathBuf,
}

impl VectorEngine {
    #[cfg(not(target_os = "windows"))]
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

    #[cfg(target_os = "windows")]
    pub fn open(data_dir: &Path, dimension: usize) -> anyhow::Result<Self> {
        tracing::warn!(
            "Windows build uses the portable brute-force vector backend; large-memory recall will be slower than usearch-backed platforms"
        );
        Ok(Self {
            state: Mutex::new(VectorState {
                dimension,
                vectors: HashMap::new(),
            }),
            data_dir: data_dir.to_path_buf(),
        })
    }

    #[cfg(not(target_os = "windows"))]
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

    #[cfg(target_os = "windows")]
    pub fn add(&self, id: Uuid, embedding: &[f32]) -> anyhow::Result<()> {
        let mut st = self
            .state
            .lock()
            .map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        ensure_embedding_dimension(embedding, st.dimension)?;
        st.vectors.insert(id, embedding.to_vec());
        Ok(())
    }

    #[cfg(not(target_os = "windows"))]
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

    #[cfg(target_os = "windows")]
    pub fn search(&self, query: &[f32], limit: usize) -> anyhow::Result<Vec<(Uuid, f32)>> {
        let st = self
            .state
            .lock()
            .map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        ensure_embedding_dimension(query, st.dimension)?;

        let mut out: Vec<_> = st
            .vectors
            .iter()
            .filter_map(|(uuid, embedding)| {
                cosine_similarity(query, embedding).map(|similarity| (*uuid, similarity))
            })
            .collect();

        out.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(Ordering::Equal));
        out.truncate(limit);
        Ok(out)
    }

    #[cfg(not(target_os = "windows"))]
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

    #[cfg(target_os = "windows")]
    pub fn remove(&self, id: Uuid) -> anyhow::Result<bool> {
        let mut st = self
            .state
            .lock()
            .map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        Ok(st.vectors.remove(&id).is_some())
    }

    #[cfg(not(target_os = "windows"))]
    pub fn size(&self) -> usize {
        self.state.lock().map(|st| st.index.size()).unwrap_or(0)
    }

    #[cfg(target_os = "windows")]
    pub fn size(&self) -> usize {
        self.state.lock().map(|st| st.vectors.len()).unwrap_or(0)
    }

    /// Persist key mappings to disk. Called periodically by indexer.
    #[cfg(not(target_os = "windows"))]
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

    /// Derived indexes are rebuilt from redb on startup, so the portable Windows backend
    /// does not need a separate on-disk key-mapping file.
    #[cfg(target_os = "windows")]
    pub fn persist_mappings(&self) -> anyhow::Result<()> {
        Ok(())
    }

    /// Rebuild from document engine data. Call on startup to restore key mappings.
    #[cfg(not(target_os = "windows"))]
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

    /// Rebuild the portable Windows backend from SoT embeddings on startup.
    #[cfg(target_os = "windows")]
    pub fn rebuild(&self, memories: &[(Uuid, Vec<f32>)]) -> anyhow::Result<()> {
        let mut st = self
            .state
            .lock()
            .map_err(|e| anyhow::anyhow!("lock: {e}"))?;

        st.vectors.clear();
        for (uuid, embedding) in memories {
            ensure_embedding_dimension(embedding, st.dimension)?;
            st.vectors.insert(*uuid, embedding.clone());
        }

        tracing::info!(
            "Rebuilt portable vector index on Windows: {} vectors",
            memories.len()
        );
        Ok(())
    }
}

#[cfg(not(target_os = "windows"))]
#[derive(serde::Serialize, serde::Deserialize)]
struct SavedMappings {
    next_key: u64,
    mappings: Vec<(u64, Uuid)>,
}

#[cfg(target_os = "windows")]
fn ensure_embedding_dimension(embedding: &[f32], expected: usize) -> anyhow::Result<()> {
    if embedding.len() != expected {
        anyhow::bail!(
            "embedding dimension mismatch: expected {expected}, got {}",
            embedding.len()
        );
    }
    Ok(())
}

#[cfg(target_os = "windows")]
fn cosine_similarity(a: &[f32], b: &[f32]) -> Option<f32> {
    if a.len() != b.len() {
        return None;
    }

    let (mut dot, mut norm_a, mut norm_b) = (0.0f64, 0.0f64, 0.0f64);
    for (lhs, rhs) in a.iter().zip(b.iter()) {
        let lhs = *lhs as f64;
        let rhs = *rhs as f64;
        dot += lhs * rhs;
        norm_a += lhs * lhs;
        norm_b += rhs * rhs;
    }

    let denom = norm_a.sqrt() * norm_b.sqrt();
    if denom <= f64::EPSILON {
        return Some(0.0);
    }

    Some((dot / denom) as f32)
}

#[cfg(test)]
mod tests {
    use sha2::{Digest, Sha256};
    use tempfile::tempdir;

    use super::*;

    fn make_embedding(tokens: &[String]) -> Vec<f32> {
        let mut values = vec![0.0f32; 384];
        for token in tokens {
            let digest = Sha256::digest(token.as_bytes());
            for (idx, byte) in digest.iter().enumerate() {
                let pos = (*byte as usize + idx * 17) % values.len();
                values[pos] += (*byte as f32 / 255.0) - 0.5;
            }
        }

        let norm = values.iter().map(|v| v * v).sum::<f32>().sqrt().max(1.0);
        for value in &mut values {
            *value /= norm;
        }
        values
    }

    fn long_regression_embedding(i: usize) -> Vec<f32> {
        make_embedding(&[
            format!("topic:{}", i % 97),
            format!("module:{}", i % 41),
            "theme:background".to_string(),
            format!("id:{i}"),
        ])
    }

    #[test]
    fn long_regression_embeddings_do_not_hit_semantic_duplicate_threshold() {
        let tmp = tempdir().unwrap();
        let engine = VectorEngine::open(tmp.path(), 384).unwrap();
        let threshold = 0.9999f32;
        let mut worst = (-1.0f32, 0usize, Uuid::nil());

        for i in 0..800usize {
            let embedding = long_regression_embedding(i);
            if let Some((existing_id, similarity)) = engine.search(&embedding, 1).unwrap().first() {
                if *similarity > worst.0 {
                    worst = (*similarity, i, *existing_id);
                }
                assert!(
                    *similarity < threshold,
                    "embedding {i} unexpectedly hit semantic duplicate threshold with similarity {similarity:.6} against {existing_id}",
                );
            }

            engine.add(Uuid::now_v7(), &embedding).unwrap();
        }

        assert!(
            worst.0 > 0.0,
            "expected at least one nearest-neighbor comparison"
        );
    }
}
