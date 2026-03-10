// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 memoryOSS Contributors

use std::path::Path;
use std::sync::RwLock;

use tantivy::collector::TopDocs;
use tantivy::query::{QueryParser, QueryParserError};
use tantivy::schema::*;
use tantivy::{Index, IndexReader, IndexWriter, TantivyDocument};
use uuid::Uuid;

/// Metadata fields for structured FTS indexing (GrepRAG: identifier-weighted search).
pub struct MemoryMetadata<'a> {
    pub agent: Option<&'a str>,
    pub session: Option<&'a str>,
    pub memory_type: Option<&'a str>,
    pub source_key: Option<&'a str>,
}

pub struct FtsEngine {
    index: Index,
    reader: IndexReader,
    writer: RwLock<IndexWriter>,
    id_field: Field,
    content_field: Field,
    tags_field: Field,
    agent_field: Field,
    session_field: Field,
    memory_type_field: Field,
    source_key_field: Field,
}

impl FtsEngine {
    pub fn open(data_dir: &Path) -> anyhow::Result<Self> {
        let fts_dir = data_dir.join("fts");
        std::fs::create_dir_all(&fts_dir)?;

        let mut schema_builder = Schema::builder();
        let id_field = schema_builder.add_text_field("id", STRING | STORED);
        let content_field = schema_builder.add_text_field("content", TEXT | STORED);
        let tags_field = schema_builder.add_text_field("tags", TEXT);
        // Structured fields for GrepRAG identifier-weighted search
        let agent_field = schema_builder.add_text_field("agent", TEXT);
        let session_field = schema_builder.add_text_field("session", TEXT);
        let memory_type_field = schema_builder.add_text_field("memory_type", TEXT);
        let source_key_field = schema_builder.add_text_field("source_key", TEXT);
        let schema = schema_builder.build();

        let dir = tantivy::directory::MmapDirectory::open(&fts_dir)?;
        // Check if existing index has different schema — rebuild if needed
        let index = match Index::open(dir.clone()) {
            Ok(existing) => {
                if existing.schema() == schema {
                    existing
                } else {
                    tracing::info!("FTS schema changed, rebuilding index");
                    // Drop and recreate
                    drop(existing);
                    let fts_path = data_dir.join("fts");
                    let _ = std::fs::remove_dir_all(&fts_path);
                    std::fs::create_dir_all(&fts_path)?;
                    let _new_dir = tantivy::directory::MmapDirectory::open(&fts_path)?;
                    Index::create_in_dir(&fts_path, schema.clone())?
                }
            }
            Err(_) => Index::open_or_create(dir, schema.clone())?,
        };
        let reader = index.reader()?;
        let writer = index.writer(50_000_000)?; // 50MB heap

        tracing::info!("FTS engine ready (with structured fields)");

        Ok(Self {
            index,
            reader,
            writer: RwLock::new(writer),
            id_field,
            content_field,
            tags_field,
            agent_field,
            session_field,
            memory_type_field,
            source_key_field,
        })
    }

    /// Add a document with full metadata (structured fields for GrepRAG search).
    /// Does NOT commit automatically — call `commit()` after a batch of adds.
    pub fn add_with_metadata(
        &self,
        id: Uuid,
        content: &str,
        tags: &[String],
        meta: &MemoryMetadata,
    ) -> anyhow::Result<()> {
        let writer = self
            .writer
            .write()
            .map_err(|e| anyhow::anyhow!("lock: {e}"))?;

        let id_str = id.to_string();
        let id_term = tantivy::Term::from_field_text(self.id_field, &id_str);
        writer.delete_term(id_term);

        let mut doc = TantivyDocument::new();
        doc.add_text(self.id_field, &id_str);
        doc.add_text(self.content_field, content);
        for tag in tags {
            doc.add_text(self.tags_field, tag);
        }
        if let Some(agent) = meta.agent {
            doc.add_text(self.agent_field, agent);
        }
        if let Some(session) = meta.session {
            doc.add_text(self.session_field, session);
        }
        if let Some(mt) = meta.memory_type {
            doc.add_text(self.memory_type_field, mt);
        }
        if let Some(sk) = meta.source_key {
            doc.add_text(self.source_key_field, sk);
        }
        writer.add_document(doc)?;

        Ok(())
    }

    /// Commit pending writes to the FTS index. Call after a batch of add/add_with_metadata.
    pub fn commit(&self) -> anyhow::Result<()> {
        let mut writer = self
            .writer
            .write()
            .map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        writer.commit()?;
        Ok(())
    }

    /// Legacy add (backwards compatible — no structured metadata).
    #[allow(dead_code)]
    pub fn add(&self, id: Uuid, content: &str, tags: &[String]) -> anyhow::Result<()> {
        self.add_with_metadata(
            id,
            content,
            tags,
            &MemoryMetadata {
                agent: None,
                session: None,
                memory_type: None,
                source_key: None,
            },
        )
    }

    pub fn search(&self, query: &str, limit: usize) -> anyhow::Result<Vec<(Uuid, f32)>> {
        self.reader.reload()?;
        let searcher = self.reader.searcher();

        // Search across all text fields (content, tags, agent, session, memory_type, source_key)
        let query_parser = QueryParser::for_index(
            &self.index,
            vec![
                self.content_field,
                self.tags_field,
                self.agent_field,
                self.session_field,
                self.memory_type_field,
                self.source_key_field,
            ],
        );
        let parsed = match query_parser.parse_query(query) {
            Ok(parsed) => parsed,
            Err(QueryParserError::FieldDoesNotExist(_)) | Err(QueryParserError::SyntaxError(_)) => {
                let sanitized = sanitize_user_query(query);
                if sanitized.is_empty() {
                    return Ok(Vec::new());
                }
                query_parser.parse_query(&sanitized)?
            }
            Err(err) => return Err(err.into()),
        };

        let top_docs = searcher.search(&parsed, &TopDocs::with_limit(limit))?;

        let mut results = Vec::with_capacity(top_docs.len());
        for (score, doc_address) in top_docs {
            let doc: TantivyDocument = searcher.doc(doc_address)?;
            if let Some(id_value) = doc.get_first(self.id_field)
                && let Some(id_str) = id_value.as_str()
                && let Ok(uuid) = Uuid::parse_str(id_str)
            {
                results.push((uuid, score));
            }
        }

        Ok(results)
    }

    /// Filter by structured metadata fields — returns matching UUIDs.
    /// Uses tantivy boolean queries instead of O(n) redb scan.
    pub fn search_metadata(
        &self,
        agent: Option<&str>,
        session: Option<&str>,
        memory_type: Option<&str>,
        tags: &[String],
        limit: usize,
    ) -> anyhow::Result<Vec<Uuid>> {
        use tantivy::query::{BooleanQuery, TermQuery};
        use tantivy::schema::IndexRecordOption;

        self.reader.reload()?;
        let searcher = self.reader.searcher();

        let mut clauses: Vec<(tantivy::query::Occur, Box<dyn tantivy::query::Query>)> = Vec::new();

        if let Some(a) = agent {
            clauses.push((
                tantivy::query::Occur::Must,
                Box::new(TermQuery::new(
                    tantivy::Term::from_field_text(self.agent_field, a),
                    IndexRecordOption::Basic,
                )),
            ));
        }
        if let Some(s) = session {
            clauses.push((
                tantivy::query::Occur::Must,
                Box::new(TermQuery::new(
                    tantivy::Term::from_field_text(self.session_field, s),
                    IndexRecordOption::Basic,
                )),
            ));
        }
        if let Some(mt) = memory_type {
            clauses.push((
                tantivy::query::Occur::Must,
                Box::new(TermQuery::new(
                    tantivy::Term::from_field_text(self.memory_type_field, mt),
                    IndexRecordOption::Basic,
                )),
            ));
        }
        for tag in tags {
            clauses.push((
                tantivy::query::Occur::Should,
                Box::new(TermQuery::new(
                    tantivy::Term::from_field_text(self.tags_field, tag),
                    IndexRecordOption::Basic,
                )),
            ));
        }

        if clauses.is_empty() {
            return Ok(Vec::new());
        }

        let query = BooleanQuery::new(clauses);
        let top_docs = searcher.search(&query, &TopDocs::with_limit(limit))?;

        let mut results = Vec::with_capacity(top_docs.len());
        for (_score, doc_address) in top_docs {
            let doc: TantivyDocument = searcher.doc(doc_address)?;
            if let Some(id_value) = doc.get_first(self.id_field)
                && let Some(id_str) = id_value.as_str()
                && let Ok(uuid) = Uuid::parse_str(id_str)
            {
                results.push(uuid);
            }
        }

        Ok(results)
    }

    pub fn remove(&self, id: Uuid) -> anyhow::Result<()> {
        let mut writer = self
            .writer
            .write()
            .map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        let id_str = id.to_string();
        let id_term = tantivy::Term::from_field_text(self.id_field, &id_str);
        writer.delete_term(id_term);
        writer.commit()?;
        Ok(())
    }

    #[allow(dead_code)]
    pub fn rebuild(&self, memories: &[(Uuid, String, Vec<String>)]) -> anyhow::Result<()> {
        let mut writer = self
            .writer
            .write()
            .map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        writer.delete_all_documents()?;

        for (id, content, tags) in memories {
            let mut doc = TantivyDocument::new();
            doc.add_text(self.id_field, id.to_string());
            doc.add_text(self.content_field, content);
            for tag in tags {
                doc.add_text(self.tags_field, tag);
            }
            writer.add_document(doc)?;
        }

        writer.commit()?;
        tracing::info!("Rebuilt FTS index: {} documents", memories.len());
        Ok(())
    }

    /// Rebuild from full Memory objects, including structured metadata fields.
    pub fn rebuild_from_memories(&self, memories: &[crate::memory::Memory]) -> anyhow::Result<()> {
        let mut writer = self
            .writer
            .write()
            .map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        writer.delete_all_documents()?;

        for m in memories {
            let mut doc = TantivyDocument::new();
            doc.add_text(self.id_field, m.id.to_string());
            doc.add_text(self.content_field, &m.content);
            for tag in &m.tags {
                doc.add_text(self.tags_field, tag);
            }
            if let Some(ref agent) = m.agent {
                doc.add_text(self.agent_field, agent);
            }
            if let Some(ref session) = m.session {
                doc.add_text(self.session_field, session);
            }
            doc.add_text(self.memory_type_field, format!("{:?}", m.memory_type));
            if let Some(ref sk) = m.source_key {
                doc.add_text(self.source_key_field, sk);
            }
            writer.add_document(doc)?;
        }

        writer.commit()?;
        tracing::info!(
            "Rebuilt FTS index with metadata: {} documents",
            memories.len()
        );
        Ok(())
    }
}

fn sanitize_user_query(query: &str) -> String {
    let mut out = String::with_capacity(query.len());
    for ch in query.chars() {
        let keep = ch.is_alphanumeric() || ch.is_whitespace() || matches!(ch, '_' | '.' | '#');
        if keep {
            out.push(ch);
        } else {
            out.push(' ');
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_user_query_removes_parser_syntax() {
        assert_eq!(
            sanitize_user_query("User request: reply with exactly one word: blue"),
            "User request reply with exactly one word blue"
        );
    }

    #[test]
    fn sanitize_user_query_removes_angle_brackets() {
        assert_eq!(
            sanitize_user_query("<system-reminder> only"),
            "system reminder only"
        );
    }

    #[test]
    fn search_falls_back_for_colon_queries() {
        let tmp = tempfile::tempdir().unwrap();
        let engine = FtsEngine::open(tmp.path()).unwrap();
        let id = Uuid::now_v7();
        engine
            .add_with_metadata(
                id,
                "Reply with exactly one word blue",
                &[],
                &MemoryMetadata {
                    agent: Some("selftest"),
                    session: None,
                    memory_type: Some("semantic"),
                    source_key: None,
                },
            )
            .unwrap();
        engine.commit().unwrap();

        let hits = engine
            .search("User request: reply with exactly one word: blue", 10)
            .unwrap();
        assert!(hits.iter().any(|(uuid, _)| *uuid == id));
    }
}
