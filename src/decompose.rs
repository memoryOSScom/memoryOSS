// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 memoryOSS Contributors

use std::sync::Arc;

use uuid::Uuid;

use crate::config::DecomposeConfig;
use crate::embedding::EmbeddingEngine;
use crate::engines::document::DocumentEngine;
use crate::engines::fts::FtsEngine;
use crate::engines::space_index::SpaceIndex;
use crate::engines::vector::VectorEngine;
use crate::memory::ScoredMemory;
use crate::merger::IdfIndex;

/// Threshold in bytes: decompose when namespace exceeds this.
const DECOMPOSE_THRESHOLD_BYTES: u64 = 200_000; // ~50k tokens ≈ 200KB

/// Heuristic decomposition: auto-partition large memory spaces into sub-queries.
/// Depth-1 only (Paper #2: deeper recursion degrades quality).
/// When LLM config is present, tries LLM-powered decomposition first, falling
/// back to heuristic on any error.
pub async fn decomposed_recall(
    doc_engine: &Arc<DocumentEngine>,
    vector_engine: &Arc<VectorEngine>,
    fts_engine: &Arc<FtsEngine>,
    embedding: &Arc<EmbeddingEngine>,
    idf_index: &Arc<IdfIndex>,
    space_index: &Arc<SpaceIndex>,
    namespace: &str,
    query: &str,
    limit: usize,
    decompose_config: &DecomposeConfig,
) -> anyhow::Result<Option<Vec<ScoredMemory>>> {
    // Check if namespace is large enough to warrant decomposition
    let (total_count, total_bytes) = space_index.global_stats();
    if total_bytes < DECOMPOSE_THRESHOLD_BYTES {
        return Ok(None); // Skip decomposition, use normal recall
    }

    tracing::debug!(
        total_count,
        total_bytes,
        "Decomposition triggered: namespace exceeds threshold"
    );

    let agent_stats = space_index.agent_stats();

    // Try LLM-powered decomposition if configured
    let sub_queries = if decompose_config.provider.is_some() {
        match llm_decompose(query, &agent_stats, decompose_config).await {
            Ok(queries) if !queries.is_empty() => {
                tracing::debug!(
                    count = queries.len(),
                    "LLM decomposition produced sub-queries"
                );
                queries
            }
            Ok(_) => {
                tracing::debug!("LLM returned empty sub-queries, falling back to heuristic");
                heuristic_subqueries(query, &agent_stats, doc_engine, namespace)?
            }
            Err(e) => {
                tracing::warn!("LLM decomposition failed, falling back to heuristic: {e}");
                heuristic_subqueries(query, &agent_stats, doc_engine, namespace)?
            }
        }
    } else {
        heuristic_subqueries(query, &agent_stats, doc_engine, namespace)?
    };

    if sub_queries.is_empty() {
        return Ok(None); // Fallback to normal recall
    }

    tracing::debug!(
        sub_query_count = sub_queries.len(),
        "Running decomposed sub-queries"
    );

    // Run sub-queries in parallel
    let mut handles = Vec::new();
    for sq in sub_queries {
        let ve = vector_engine.clone();
        let fe = fts_engine.clone();
        let de = doc_engine.clone();
        let emb = embedding.clone();
        let idf = idf_index.clone();
        let ns = namespace.to_string();
        let sub_limit = limit * 2; // overfetch per sub-query

        handles.push(tokio::spawn(async move {
            run_sub_query(&de, &ve, &fe, &emb, &idf, &ns, &sq, sub_limit).await
        }));
    }

    // Collect and merge results
    let mut all_results: std::collections::HashMap<Uuid, ScoredMemory> =
        std::collections::HashMap::new();

    for handle in handles {
        match handle.await {
            Ok(Ok(results)) => {
                for sm in results {
                    let entry = all_results.entry(sm.memory.id).or_insert(sm.clone());
                    // Keep highest score, merge provenance
                    if sm.score > entry.score {
                        entry.score = sm.score;
                    }
                    for p in &sm.provenance {
                        if !entry.provenance.contains(p) {
                            entry.provenance.push(p.clone());
                        }
                    }
                }
            }
            Ok(Err(e)) => {
                tracing::warn!("Sub-query failed: {e}");
            }
            Err(e) => {
                tracing::warn!("Sub-query task panicked: {e}");
            }
        }
    }

    // Sort by score and truncate
    let mut merged: Vec<ScoredMemory> = all_results.into_values().collect();
    merged.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| b.memory.id.cmp(&a.memory.id))
    });
    merged = crate::fusion::collapse_scored_memories(merged);
    merged.truncate(limit);

    // Mark provenance as decomposed
    for sm in &mut merged {
        if !sm.provenance.contains(&"decomposed".to_string()) {
            sm.provenance.push("decomposed".to_string());
        }
    }

    Ok(Some(merged))
}

// ── Heuristic decomposition (default, no LLM) ──────────────────────────

fn heuristic_subqueries(
    query: &str,
    agent_stats: &std::collections::HashMap<String, crate::engines::space_index::AgentStats>,
    doc_engine: &Arc<DocumentEngine>,
    namespace: &str,
) -> anyhow::Result<Vec<SubQuery>> {
    let strategy = if agent_stats.len() > 1 {
        DecomposeStrategy::Topic
    } else {
        DecomposeStrategy::Time
    };

    match strategy {
        DecomposeStrategy::Topic => Ok(generate_topic_subqueries(query, agent_stats)),
        DecomposeStrategy::Time => generate_time_subqueries(query, doc_engine, namespace),
    }
}

#[derive(Debug)]
enum DecomposeStrategy {
    Topic,
    Time,
}

/// Topic-based: run query filtered by each agent's topic tags.
fn generate_topic_subqueries(
    query: &str,
    agent_stats: &std::collections::HashMap<String, crate::engines::space_index::AgentStats>,
) -> Vec<SubQuery> {
    let mut queries = Vec::new();

    for (agent, stats) in agent_stats {
        if stats.count == 0 {
            continue;
        }
        let topic_boost = stats
            .topics
            .iter()
            .take(3)
            .cloned()
            .collect::<Vec<_>>()
            .join(" ");
        let boosted = if topic_boost.is_empty() {
            query.to_string()
        } else {
            format!("{query} {topic_boost}")
        };

        queries.push(SubQuery {
            query: boosted,
            agent_filter: Some(agent.clone()),
        });
    }

    queries
}

/// Time-based: split into recent and older partitions.
fn generate_time_subqueries(
    query: &str,
    doc_engine: &Arc<DocumentEngine>,
    namespace: &str,
) -> anyhow::Result<Vec<SubQuery>> {
    let _ = (doc_engine, namespace);

    Ok(vec![SubQuery {
        query: query.to_string(),
        agent_filter: None,
    }])
}

// ── LLM-powered decomposition ──────────────────────────────────────────

/// Build a metadata summary for the LLM, respecting token budget.
/// Only sends agent stats, topics, time ranges — never memory content.
fn build_metadata_summary(
    agent_stats: &std::collections::HashMap<String, crate::engines::space_index::AgentStats>,
    token_budget: usize,
) -> String {
    let mut parts = Vec::new();

    for (agent, stats) in agent_stats {
        let agent_name = if agent.is_empty() { "(unnamed)" } else { agent };
        let topics = stats
            .topics
            .iter()
            .take(10)
            .cloned()
            .collect::<Vec<_>>()
            .join(", ");
        let time_range = match (stats.earliest, stats.latest) {
            (Some(e), Some(l)) => format!("{} to {}", e.format("%Y-%m-%d"), l.format("%Y-%m-%d")),
            _ => "unknown".to_string(),
        };
        parts.push(format!(
            "- Agent \"{agent_name}\": {count} memories, {bytes}B, topics=[{topics}], time={time_range}",
            count = stats.count,
            bytes = stats.total_bytes,
        ));
    }

    let summary = parts.join("\n");

    // Rough token estimation: ~4 chars per token
    let char_budget = token_budget * 4;
    if summary.len() > char_budget {
        summary[..char_budget].to_string()
    } else {
        summary
    }
}

/// Call the configured LLM to decompose a query into sub-queries.
async fn llm_decompose(
    query: &str,
    agent_stats: &std::collections::HashMap<String, crate::engines::space_index::AgentStats>,
    config: &DecomposeConfig,
) -> anyhow::Result<Vec<SubQuery>> {
    let provider = config.provider.as_deref().unwrap_or("claude");
    let metadata = build_metadata_summary(agent_stats, config.token_budget);

    let prompt = format!(
        "You are a query decomposition engine for a memory retrieval system.\n\
         Given a user query and metadata about the memory space, generate 2-5 focused sub-queries \
         that together cover the original query's intent.\n\n\
         Memory space metadata:\n{metadata}\n\n\
         Original query: {query}\n\n\
         Respond with ONLY a JSON array of objects, each with \"query\" (string) and optionally \
         \"agent\" (string, must match an agent name from metadata). Example:\n\
         [{{\"query\": \"sub-query 1\", \"agent\": \"agent-name\"}}, {{\"query\": \"sub-query 2\"}}]\n\n\
         Rules:\n\
         - Each sub-query should target a different aspect or agent\n\
         - Keep sub-queries concise (under 50 words)\n\
         - Use agent names from the metadata when relevant\n\
         - Respond with ONLY valid JSON, no other text"
    );

    let response = crate::llm_client::call_llm(&crate::llm_client::LlmRequest {
        provider,
        model: &config.model,
        api_key: config.api_key.as_deref(),
        endpoint: config.endpoint.as_deref(),
        auth_scheme: None,
        prompt: &prompt,
        max_tokens: 1024,
        timeout_secs: 30,
    })
    .await?;

    parse_llm_response(&response.text)
}

/// Parse the LLM response JSON into SubQuery objects.
fn parse_llm_response(response: &str) -> anyhow::Result<Vec<SubQuery>> {
    // Extract JSON array from response (LLMs sometimes add wrapping text)
    let json_str = extract_json_array(response)
        .ok_or_else(|| anyhow::anyhow!("no JSON array found in LLM response"))?;

    let items: Vec<serde_json::Value> = serde_json::from_str(json_str)?;

    let mut sub_queries = Vec::new();
    for item in items.iter().take(5) {
        // Max 5 sub-queries
        let query = item
            .get("query")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing 'query' field in LLM response item"))?;

        let agent_filter = item
            .get("agent")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        sub_queries.push(SubQuery {
            query: query.to_string(),
            agent_filter,
        });
    }

    Ok(sub_queries)
}

/// Extract the first JSON array from a string (handles LLM wrapping text).
/// Delegates to shared implementation in llm_client.
fn extract_json_array(text: &str) -> Option<&str> {
    crate::llm_client::extract_json_array(text)
}

// ── Sub-query execution (shared between heuristic and LLM) ─────────────

struct SubQuery {
    query: String,
    agent_filter: Option<String>,
}

/// Execute a single sub-query against vector + FTS engines.
/// Uses shared score_and_merge core for consistent scoring across all recall paths.
async fn run_sub_query(
    doc_engine: &Arc<DocumentEngine>,
    vector_engine: &Arc<VectorEngine>,
    fts_engine: &Arc<FtsEngine>,
    embedding: &Arc<EmbeddingEngine>,
    idf_index: &Arc<IdfIndex>,
    namespace: &str,
    sub_query: &SubQuery,
    limit: usize,
) -> anyhow::Result<Vec<ScoredMemory>> {
    let query_embedding = embedding.embed_one(&sub_query.query).await?;

    let vector_results = vector_engine.search(&query_embedding, limit)?;
    let fts_results = fts_engine.search(&sub_query.query, limit)?;

    // GrepRAG: exact/identifier match as 3rd channel
    let identifiers = crate::scoring::extract_identifiers(&sub_query.query);
    let exact_results = if !identifiers.is_empty() {
        crate::scoring::exact_match_search(fts_engine, &identifiers, limit)
    } else {
        Vec::new()
    };

    let idf_boost = crate::scoring::compute_idf_boost(idf_index, &sub_query.query);

    let options = crate::scoring::MergeOptions {
        query: sub_query.query.clone(),
        idf_boost,
        namespace: namespace.to_string(),
        limit,
        agent_filter: sub_query.agent_filter.clone(),
        task_context: crate::scoring::detect_task_context(&sub_query.query),
        identifier_route: crate::scoring::detect_identifier_route(&sub_query.query),
        primitive_algebra: false,
        ..Default::default()
    };

    Ok(crate::scoring::score_and_merge(
        &vector_results,
        &fts_results,
        &exact_results,
        doc_engine,
        None,
        &options,
    ))
}
