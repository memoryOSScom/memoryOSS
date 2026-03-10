// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 memoryOSS Contributors

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Instant;

const MAX_BUCKETS: usize = 10_000;
const EVICT_AGE_SECS: f64 = 120.0;

pub struct RateLimiter {
    max_per_sec: AtomicU32,
    buckets: Mutex<HashMap<String, Bucket>>,
}

struct Bucket {
    tokens: f64,
    last_refill: Instant,
}

impl RateLimiter {
    pub fn new(max_per_sec: u32) -> Self {
        Self {
            max_per_sec: AtomicU32::new(max_per_sec),
            buckets: Mutex::new(HashMap::new()),
        }
    }

    /// Update the rate limit (for config hot-reload).
    pub fn set_rate(&self, max_per_sec: u32) {
        self.max_per_sec.store(max_per_sec, Ordering::Relaxed);
    }

    /// Returns Ok(()) if allowed, Err(retry_after_ms) if rate limited.
    pub fn check(&self, key: &str) -> Result<(), u64> {
        let rate = self.max_per_sec.load(Ordering::Relaxed);
        if rate == 0 {
            return Ok(());
        }

        let mut buckets = self.buckets.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        let max = rate as f64;

        // A6 FIX: evict stale buckets to prevent unbounded memory growth
        if buckets.len() > MAX_BUCKETS {
            buckets.retain(|_, b| now.duration_since(b.last_refill).as_secs_f64() < EVICT_AGE_SECS);
        }

        let bucket = buckets.entry(key.to_string()).or_insert(Bucket {
            tokens: max,
            last_refill: now,
        });

        // Refill tokens based on elapsed time
        let elapsed = now.duration_since(bucket.last_refill).as_secs_f64();
        bucket.tokens = (bucket.tokens + elapsed * max).min(max);
        bucket.last_refill = now;

        if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            Ok(())
        } else {
            let wait_secs = (1.0 - bucket.tokens) / max;
            Err((wait_secs * 1000.0).ceil() as u64)
        }
    }
}
