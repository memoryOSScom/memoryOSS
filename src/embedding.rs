// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 memoryOSS Contributors

use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

struct CacheEntry {
    embedding: Vec<f32>,
    inserted_at: Instant,
}

pub struct EmbeddingCache {
    entries: Mutex<HashMap<u64, CacheEntry>>,
    ttl: Duration,
    max_size: usize,
}

impl EmbeddingCache {
    pub fn new(ttl_secs: u64, max_size: usize) -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            ttl: Duration::from_secs(ttl_secs),
            max_size,
        }
    }

    pub async fn get(&self, key: u64) -> Option<Vec<f32>> {
        let mut map = self.entries.lock().await;
        if let Some(entry) = map.get(&key) {
            if entry.inserted_at.elapsed() < self.ttl {
                return Some(entry.embedding.clone());
            }
            // Expired — remove
            map.remove(&key);
        }
        None
    }

    pub async fn put(&self, key: u64, embedding: Vec<f32>) {
        let mut map = self.entries.lock().await;
        // Evict expired entries if we're at capacity
        if map.len() >= self.max_size {
            let now = Instant::now();
            map.retain(|_, v| now.duration_since(v.inserted_at) < self.ttl);
        }
        // If still at capacity after eviction, remove oldest
        if map.len() >= self.max_size
            && let Some(oldest_key) = map
                .iter()
                .min_by_key(|(_, v)| v.inserted_at)
                .map(|(k, _)| *k)
        {
            map.remove(&oldest_key);
        }
        map.insert(
            key,
            CacheEntry {
                embedding,
                inserted_at: Instant::now(),
            },
        );
    }

    pub async fn flush(&self) -> usize {
        let mut map = self.entries.lock().await;
        let count = map.len();
        map.clear();
        count
    }

    pub async fn stats(&self) -> (usize, usize) {
        let map = self.entries.lock().await;
        let now = Instant::now();
        let valid = map
            .values()
            .filter(|v| now.duration_since(v.inserted_at) < self.ttl)
            .count();
        (valid, map.len())
    }

    fn hash_key(text: &str) -> u64 {
        use std::hash::{Hash, Hasher};
        let normalized = text.trim().to_lowercase();
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        normalized.hash(&mut hasher);
        hasher.finish()
    }
}

pub struct EmbeddingEngine {
    model: Arc<Mutex<TextEmbedding>>,
    model_id: crate::config::EmbeddingModelId,
    dimension: usize,
    cache: EmbeddingCache,
}

impl EmbeddingEngine {
    pub fn new(config: &crate::config::EmbeddingsConfig) -> anyhow::Result<Self> {
        Self::with_cache_config(config, 300, 10_000)
    }

    pub fn with_cache_config(
        config: &crate::config::EmbeddingsConfig,
        ttl_secs: u64,
        max_size: usize,
    ) -> anyhow::Result<Self> {
        let (model, dimension) = load_text_embedding(config.model, true)?;
        tracing::info!(
            "Embedding engine ready: {}-dim ({})",
            dimension,
            config.model
        );

        Ok(Self {
            model: Arc::new(Mutex::new(model)),
            model_id: config.model,
            dimension,
            cache: EmbeddingCache::new(ttl_secs, max_size),
        })
    }

    pub fn model_id(&self) -> crate::config::EmbeddingModelId {
        self.model_id
    }

    pub fn dimension(&self) -> usize {
        self.dimension
    }

    pub async fn embed(&self, texts: Vec<String>) -> anyhow::Result<Vec<Vec<f32>>> {
        let model = self.model.clone();
        // fastembed is CPU-bound — run in blocking task
        tokio::task::spawn_blocking(move || {
            let model = model.blocking_lock();
            let embeddings = model.embed(texts, None)?;
            Ok(embeddings)
        })
        .await?
    }

    /// Embed a single text, using cache for lookups.
    pub async fn embed_one(&self, text: &str) -> anyhow::Result<Vec<f32>> {
        let key = EmbeddingCache::hash_key(text);

        // Check cache first
        if let Some(cached) = self.cache.get(key).await {
            tracing::debug!("embedding cache hit");
            return Ok(cached);
        }

        // Cache miss — compute embedding
        let texts = vec![text.to_string()];
        let mut embeddings = self.embed(texts).await?;
        let embedding = embeddings
            .pop()
            .ok_or_else(|| anyhow::anyhow!("no embedding returned"))?;

        // Store in cache
        self.cache.put(key, embedding.clone()).await;

        Ok(embedding)
    }

    /// Flush the embedding cache. Returns number of evicted entries.
    pub async fn flush_cache(&self) -> usize {
        self.cache.flush().await
    }

    /// Get cache stats: (valid_entries, total_entries).
    pub async fn cache_stats(&self) -> (usize, usize) {
        self.cache.stats().await
    }
}

pub(crate) fn load_text_embedding(
    model_id: crate::config::EmbeddingModelId,
    show_download_progress: bool,
) -> anyhow::Result<(TextEmbedding, usize)> {
    let mut opts = InitOptions::default();
    opts.model_name = model_id.fastembed_model();
    opts.show_download_progress = show_download_progress;
    let model = TextEmbedding::try_new(opts)?;

    let test = model.embed(vec!["test"], None)?;
    let dimension = test
        .first()
        .map(|vector| vector.len())
        .unwrap_or(model_id.expected_dimension());
    if dimension != model_id.expected_dimension() {
        anyhow::bail!(
            "embedding model {} reported dimension {} but registry expects {}",
            model_id,
            dimension,
            model_id.expected_dimension()
        );
    }
    Ok((model, dimension))
}

impl crate::config::EmbeddingModelId {
    pub(crate) fn fastembed_model(self) -> EmbeddingModel {
        match self {
            Self::AllMiniLML6V2 => EmbeddingModel::AllMiniLML6V2,
            Self::BgeSmallEnV15 => EmbeddingModel::BGESmallENV15,
            Self::BgeBaseEnV15 => EmbeddingModel::BGEBaseENV15,
            Self::BgeLargeEnV15 => EmbeddingModel::BGELargeENV15,
        }
    }
}

/// Mock embedding engine for dev mode — random vectors, instant startup
pub struct MockEmbeddingEngine {
    dimension: usize,
}

impl MockEmbeddingEngine {
    pub fn new(dimension: usize) -> Self {
        tracing::info!("Mock embedding engine: {dimension}-dim (random vectors)");
        Self { dimension }
    }

    pub fn dimension(&self) -> usize {
        self.dimension
    }

    pub async fn embed_one(&self, _text: &str) -> anyhow::Result<Vec<f32>> {
        use rand::Rng;
        let mut rng = rand::thread_rng();
        let vec: Vec<f32> = (0..self.dimension)
            .map(|_| rng.r#gen::<f32>() * 2.0 - 1.0)
            .collect();
        Ok(vec)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn embedding_model_registry_exposes_supported_runtime_models() {
        let supported = crate::config::EmbeddingModelId::supported();
        assert_eq!(supported.len(), 4);
        assert_eq!(supported[0].to_string(), "all-minilm-l6-v2");
        assert_eq!(supported[1].expected_dimension(), 384);
        assert_eq!(supported[2].expected_dimension(), 768);
        assert_eq!(supported[3].expected_dimension(), 1024);
    }
}
