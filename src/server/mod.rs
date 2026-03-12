// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 memoryOSS Contributors

pub mod gateway;
pub mod middleware;
pub mod proxy;
pub mod rate_limit;
pub mod routes;
pub mod tls;

use std::sync::Arc;

use crate::config::Config;
use crate::embedding::EmbeddingEngine;
use crate::engines::document::DocumentEngine;
use crate::engines::fts::FtsEngine;
use crate::engines::group_commit::GroupCommitter;
use crate::engines::indexer::{self, IndexerState};
use crate::engines::space_index::SpaceIndex;
use crate::engines::vector::VectorEngine;
use crate::intent_cache::IntentCache;
use crate::merger::IdfIndex;
use crate::security::trust::TrustScorer;
use crate::sharing::SharingStore;
use axum::Extension;
use routes::SharedState;
use tokio::net::TcpListener;
use tokio::time::{self, Duration};

/// Initialize engines, rebuild indexes, spawn async indexer pipeline.
/// Returns shared state ready for the HTTP server.
pub fn init_engines(
    config: &Config,
) -> anyhow::Result<(
    Arc<DocumentEngine>,
    Arc<VectorEngine>,
    Arc<FtsEngine>,
    Arc<EmbeddingEngine>,
    Arc<IdfIndex>,
    Arc<SpaceIndex>,
)> {
    std::fs::create_dir_all(&config.storage.data_dir)?;
    let doc_engine = Arc::new(DocumentEngine::open_with_config(
        &config.storage.data_dir,
        &config.encryption,
        config.auth.audit_hmac_secret.as_bytes(),
    )?);

    tracing::info!("Loading embedding model...");
    let embedding = Arc::new(EmbeddingEngine::with_cache_config(
        config.limits.embedding_cache_ttl_secs,
        config.limits.embedding_cache_max_size,
    )?);

    let vector_engine = Arc::new(VectorEngine::open(
        &config.storage.data_dir,
        embedding.dimension(),
    )?);
    let fts_engine = Arc::new(FtsEngine::open(&config.storage.data_dir)?);

    // Crash recovery: rebuild derived indexes from redb (SoT) for ALL namespaces
    let namespaces = doc_engine.list_namespaces()?;
    let mut all_memories = Vec::new();
    for ns in &namespaces {
        all_memories.extend(doc_engine.list_all(ns)?);
    }

    if !all_memories.is_empty() {
        let vec_data: Vec<_> = all_memories
            .iter()
            .filter_map(|m| m.embedding.as_ref().map(|e| (m.id, e.clone())))
            .collect();
        vector_engine.rebuild(&vec_data)?;
        fts_engine.rebuild_from_memories(&all_memories)?;
        tracing::info!(
            "Rebuilt {} memories across {} namespaces from SoT on startup",
            all_memories.len(),
            namespaces.len()
        );
    } else {
        tracing::info!("No memories found, fresh start");
    }

    // Load IDF index from materialized view in redb, or rebuild if not available
    let idf_index = Arc::new(IdfIndex::new());
    if !idf_index.load_from_redb(doc_engine.db()) {
        tracing::info!("IDF materialized view not found, rebuilding from documents...");
        let contents: Vec<String> = all_memories.iter().map(|m| m.content.clone()).collect();
        idf_index.rebuild(&contents);
        // Persist the freshly built index
        if let Err(e) = idf_index.persist_to_redb(doc_engine.db()) {
            tracing::warn!("Failed to persist IDF index: {e}");
        }
    }

    // Rebuild space index from all memories across all namespaces
    let space_index = Arc::new(SpaceIndex::new());
    space_index.rebuild(&all_memories);

    Ok((
        doc_engine,
        vector_engine,
        fts_engine,
        embedding,
        idf_index,
        space_index,
    ))
}

pub fn build_shared_state(
    config: &Config,
    config_path: std::path::PathBuf,
    doc_engine: Arc<DocumentEngine>,
    vector_engine: Arc<VectorEngine>,
    fts_engine: Arc<FtsEngine>,
    embedding: Arc<EmbeddingEngine>,
    indexer_state: Arc<IndexerState>,
    idf_index: Arc<IdfIndex>,
    space_index: Arc<SpaceIndex>,
) -> Arc<SharedState> {
    let rate_limiter = rate_limit::RateLimiter::new(config.limits.rate_limit_per_sec);
    let group_committer = GroupCommitter::spawn(
        doc_engine.clone(),
        indexer_state.clone(),
        config.limits.group_commit_batch_size,
        config.limits.group_commit_flush_ms,
    );

    let trust_scorer = Arc::new(TrustScorer::new(config.trust.threshold));
    trust_scorer.load_from_redb(doc_engine.db());
    let sharing_store = Arc::new(SharingStore::new(config.sharing.clone()));
    let intent_cache = Arc::new(IntentCache::new(
        config.limits.intent_cache_ttl_secs,
        config.limits.intent_cache_max_entries,
    ));

    // Load IP allowlists from config
    for (ns, ips) in &config.trust.ip_allowlists {
        let parsed: Vec<std::net::IpAddr> = ips.iter().filter_map(|s| s.parse().ok()).collect();
        if !parsed.is_empty() {
            trust_scorer.set_ip_allowlist(ns, parsed);
        }
    }

    Arc::new(SharedState {
        config: config.clone(),
        config_path,
        doc_engine,
        vector_engine,
        fts_engine,
        embedding,
        rate_limiter,
        indexer_state,
        group_committer,
        idf_index,
        space_index,
        trust_scorer,
        sharing_store,
        intent_cache,
        prefetcher: Arc::new(crate::prefetch::SessionPrefetcher::new()),
        metrics: Arc::new(routes::MetricsCounters::new()),
        last_messages_hash: std::sync::RwLock::new(std::collections::HashMap::new()),
    })
}

pub async fn run(config: Config, config_path: std::path::PathBuf) -> anyhow::Result<()> {
    rustls::crypto::ring::default_provider()
        .install_default()
        .map_err(|_| anyhow::anyhow!("failed to install rustls crypto provider"))?;

    // Security warnings
    if config.dev_mode {
        tracing::warn!("⚠ DEV MODE ACTIVE — authentication disabled, do NOT use in production");
    }
    if config.proxy.passthrough_auth {
        if config.proxy.passthrough_local_only {
            tracing::info!(
                "proxy.passthrough_auth=true with passthrough_local_only=true — loopback clients may use passthrough auth"
            );
        } else {
            tracing::warn!(
                "⚠ proxy.passthrough_auth=true — ANY bearer token will be accepted for proxy endpoints. Disable in production."
            );
        }
    }
    if config.auth.api_keys.is_empty() && !config.dev_mode {
        tracing::warn!(
            "⚠ No API keys configured — all endpoints will reject requests. Set auth.api_keys or use `memoryoss dev`."
        );
    }

    let (doc_engine, vector_engine, fts_engine, embedding, idf_index, space_index) =
        init_engines(&config)?;

    // Spawn async indexer pipeline
    let indexer_state = Arc::new(IndexerState::new());
    indexer::spawn_indexer(
        doc_engine.clone(),
        vector_engine.clone(),
        fts_engine.clone(),
        embedding.clone(),
        indexer_state.clone(),
        idf_index.clone(),
        space_index.clone(),
    );

    // Clone refs for shutdown cleanup before moving into shared state
    let idf_for_shutdown = idf_index.clone();
    let doc_for_shutdown = doc_engine.clone();

    let state = build_shared_state(
        &config,
        config_path,
        doc_engine,
        vector_engine,
        fts_engine,
        embedding,
        indexer_state,
        idf_index,
        space_index,
    );
    let trust_for_shutdown = state.trust_scorer.clone();

    if config.decay.enabled {
        let decay_state = state.clone();
        let after_days = config.decay.after_days;
        let interval = if after_days == 0 {
            Duration::from_secs(1)
        } else {
            Duration::from_secs(15 * 60)
        };
        tokio::spawn(async move {
            let mut ticker = time::interval(interval);
            ticker.set_missed_tick_behavior(time::MissedTickBehavior::Skip);
            loop {
                ticker.tick().await;
                let namespaces = match decay_state.doc_engine.list_namespaces() {
                    Ok(namespaces) => namespaces,
                    Err(e) => {
                        tracing::warn!(error = %e, "automatic lifecycle sweep failed to list namespaces");
                        continue;
                    }
                };
                for namespace in namespaces {
                    match routes::run_namespace_lifecycle(
                        &decay_state,
                        &namespace,
                        "auto-lifecycle",
                        after_days,
                    ) {
                        Ok(changed) if changed > 0 => {
                            decay_state
                                .intent_cache
                                .invalidate_namespace(&namespace)
                                .await;
                            tracing::info!(
                                namespace,
                                changed,
                                "automatic lifecycle sweep updated memories"
                            );
                        }
                        Ok(_) => {}
                        Err(e) => {
                            tracing::warn!(namespace, error = %e, "automatic lifecycle sweep failed");
                        }
                    }
                }
            }
        });
    }

    if config.consolidation.enabled {
        let consolidation_state = state.clone();
        let interval = if config.consolidation.interval_minutes == 0 {
            Duration::from_secs(1)
        } else {
            Duration::from_secs(config.consolidation.interval_minutes.saturating_mul(60))
        };
        let threshold = config.consolidation.threshold as f64;
        let max_clusters = config.consolidation.max_clusters;
        tokio::spawn(async move {
            let mut ticker = time::interval(interval);
            ticker.set_missed_tick_behavior(time::MissedTickBehavior::Skip);
            loop {
                ticker.tick().await;
                let namespaces = match consolidation_state.doc_engine.list_namespaces() {
                    Ok(namespaces) => namespaces,
                    Err(e) => {
                        tracing::warn!(error = %e, "automatic consolidation failed to list namespaces");
                        continue;
                    }
                };
                for namespace in namespaces {
                    match routes::run_namespace_consolidation(
                        &consolidation_state,
                        &namespace,
                        threshold,
                        max_clusters,
                        false,
                        "auto-consolidation",
                    )
                    .await
                    {
                        Ok(result) if result.total_merged > 0 => {
                            tracing::info!(
                                namespace,
                                total_merged = result.total_merged,
                                derived_created = result.derived_created,
                                active_before = result.active_before,
                                active_after = result.active_after,
                                duplicate_rate_before = result.duplicate_rate_before,
                                duplicate_rate_after = result.duplicate_rate_after,
                                "automatic consolidation sweep merged memories"
                            );
                        }
                        Ok(_) => {}
                        Err(e) => {
                            tracing::warn!(namespace, error = %e, "automatic consolidation sweep failed");
                        }
                    }
                }
            }
        });
    }

    let bind_addr = config.bind_addr();
    let app = routes::router(state.clone());
    let listener = TcpListener::bind(&bind_addr).await?;

    // SIGHUP handler for config hot-reload
    #[cfg(unix)]
    {
        let reload_state = state;
        tokio::spawn(async move {
            let mut sighup = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())
                .expect("failed to register SIGHUP handler");
            loop {
                sighup.recv().await;
                tracing::info!("Received SIGHUP, reloading config...");
                reload_state.reload_config();
            }
        });
    }

    // Graceful shutdown: listen for SIGTERM and CTRL-C
    let shutdown = async {
        let ctrl_c = tokio::signal::ctrl_c();
        #[cfg(unix)]
        let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to register SIGTERM handler");
        #[cfg(unix)]
        let sigterm_recv = sigterm.recv();
        #[cfg(not(unix))]
        let sigterm_recv = std::future::pending::<Option<()>>();

        tokio::select! {
            _ = ctrl_c => tracing::info!("Received CTRL-C, shutting down..."),
            _ = sigterm_recv => tracing::info!("Received SIGTERM, shutting down..."),
        }
    };
    tokio::pin!(shutdown);

    if config.tls.enabled {
        let tls_acceptor = tls::build_tls_acceptor(&config.tls)?;
        tracing::info!("Listening on https://{bind_addr}");

        loop {
            tokio::select! {
                result = listener.accept() => {
                    let (stream, addr) = result?;
                    let acceptor = tls_acceptor.clone();
                    let app = app.clone().layer(Extension(axum::extract::ConnectInfo(addr)));

                    tokio::spawn(async move {
                        match acceptor.accept(stream).await {
                            Ok(tls_stream) => {
                                let io = hyper_util::rt::TokioIo::new(tls_stream);
                                let service = hyper_util::service::TowerToHyperService::new(app);
                                if let Err(err) =
                                    hyper_util::server::conn::auto::Builder::new(hyper_util::rt::TokioExecutor::new())
                                        .serve_connection(io, service)
                                        .await
                                {
                                    tracing::debug!("Connection error from {addr}: {err}");
                                }
                            }
                            Err(err) => {
                                tracing::debug!("TLS handshake failed from {addr}: {err}");
                            }
                        }
                    });
                }
                _ = &mut shutdown => {
                    break;
                }
            }
        }
    } else {
        tracing::info!("Listening on http://{bind_addr} (TLS disabled)");

        loop {
            tokio::select! {
                result = listener.accept() => {
                    let (stream, addr) = result?;
                    let app = app.clone().layer(Extension(axum::extract::ConnectInfo(addr)));

                    tokio::spawn(async move {
                        let io = hyper_util::rt::TokioIo::new(stream);
                        let service = hyper_util::service::TowerToHyperService::new(app);
                        if let Err(err) =
                            hyper_util::server::conn::auto::Builder::new(hyper_util::rt::TokioExecutor::new())
                                .serve_connection(io, service)
                                .await
                        {
                            tracing::debug!("Connection error from {addr}: {err}");
                        }
                    });
                }
                _ = &mut shutdown => {
                    break;
                }
            }
        }
    }

    // Flush IDF materialized view before exit
    tracing::info!("Flushing state to disk...");
    if idf_for_shutdown.is_dirty()
        && let Err(e) = idf_for_shutdown.persist_to_redb(doc_for_shutdown.db())
    {
        tracing::warn!("Failed to persist IDF on shutdown: {e}");
    }
    // Persist trust scores (source reputations + access counts)
    if trust_for_shutdown.has_state()
        && let Err(e) = trust_for_shutdown.persist_to_redb(doc_for_shutdown.db())
    {
        tracing::warn!("Failed to persist trust state on shutdown: {e}");
    }
    tracing::info!("Shutdown complete.");
    Ok(())
}

/// Internal memory core for hybrid mode: always loopback + plain HTTP.
pub async fn run_core(mut config: Config, config_path: std::path::PathBuf) -> anyhow::Result<()> {
    config.server.host = "127.0.0.1".to_string();
    config.server.port = config.server.core_port();
    config.tls.enabled = false;
    config.tls.auto_generate = false;
    config.server.hybrid_mode = false;
    run(config, config_path).await
}

/// Dev mode: plain HTTP (no TLS), relaxed auth, same engines.
pub async fn run_dev(config: Config, config_path: std::path::PathBuf) -> anyhow::Result<()> {
    rustls::crypto::ring::default_provider()
        .install_default()
        .map_err(|_| anyhow::anyhow!("failed to install rustls crypto provider"))?;

    let (doc_engine, vector_engine, fts_engine, embedding, idf_index, space_index) =
        init_engines(&config)?;

    let indexer_state = Arc::new(IndexerState::new());
    indexer::spawn_indexer(
        doc_engine.clone(),
        vector_engine.clone(),
        fts_engine.clone(),
        embedding.clone(),
        indexer_state.clone(),
        idf_index.clone(),
        space_index.clone(),
    );

    // Clone refs for shutdown cleanup before moving into shared state
    let idf_for_shutdown = idf_index.clone();
    let doc_for_shutdown = doc_engine.clone();

    let state = build_shared_state(
        &config,
        config_path,
        doc_engine,
        vector_engine,
        fts_engine,
        embedding,
        indexer_state,
        idf_index,
        space_index,
    );
    let trust_for_shutdown = state.trust_scorer.clone();

    let bind_addr = config.bind_addr();
    let app = routes::router(state.clone());

    let listener = TcpListener::bind(&bind_addr).await?;
    tracing::warn!("DEV MODE: plain HTTP (no TLS), listening on http://{bind_addr}");

    // SIGHUP handler for config hot-reload
    #[cfg(unix)]
    {
        let reload_state = state;
        tokio::spawn(async move {
            let mut sighup = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())
                .expect("failed to register SIGHUP handler");
            loop {
                sighup.recv().await;
                tracing::info!("Received SIGHUP, reloading config...");
                reload_state.reload_config();
            }
        });
    }

    // Graceful shutdown: listen for SIGTERM and CTRL-C
    let shutdown = async {
        let ctrl_c = tokio::signal::ctrl_c();
        #[cfg(unix)]
        let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to register SIGTERM handler");
        #[cfg(unix)]
        let sigterm_recv = sigterm.recv();
        #[cfg(not(unix))]
        let sigterm_recv = std::future::pending::<Option<()>>();

        tokio::select! {
            _ = ctrl_c => tracing::info!("Received CTRL-C, shutting down..."),
            _ = sigterm_recv => tracing::info!("Received SIGTERM, shutting down..."),
        }
    };
    tokio::pin!(shutdown);

    loop {
        tokio::select! {
            result = listener.accept() => {
                let (stream, addr) = result?;
                let app = app.clone();

                tokio::spawn(async move {
                    let io = hyper_util::rt::TokioIo::new(stream);
                    let service = hyper_util::service::TowerToHyperService::new(app);
                    if let Err(err) =
                        hyper_util::server::conn::auto::Builder::new(hyper_util::rt::TokioExecutor::new())
                            .serve_connection(io, service)
                            .await
                    {
                        tracing::debug!("Connection error from {addr}: {err}");
                    }
                });
            }
            _ = &mut shutdown => {
                break;
            }
        }
    }

    // Flush state before exit
    tracing::info!("Flushing state to disk...");
    if idf_for_shutdown.is_dirty()
        && let Err(e) = idf_for_shutdown.persist_to_redb(doc_for_shutdown.db())
    {
        tracing::warn!("Failed to persist IDF on shutdown: {e}");
    }
    if trust_for_shutdown.has_state()
        && let Err(e) = trust_for_shutdown.persist_to_redb(doc_for_shutdown.db())
    {
        tracing::warn!("Failed to persist trust state on shutdown: {e}");
    }
    tracing::info!("Shutdown complete.");
    Ok(())
}
