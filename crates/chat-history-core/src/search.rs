use std::collections::HashMap;

use anyhow::anyhow;
use rusqlite::{Connection, OptionalExtension, params};

use crate::{
    ingest::decode_embedding,
    models::{SearchMode, SearchOptions, SearchResult},
    ranking::{cosine_similarity, reciprocal_rank_fusion},
};

pub fn search(
    conn: &Connection,
    options: SearchOptions,
    query_embedding: Option<Vec<f32>>,
) -> anyhow::Result<Vec<SearchResult>> {
    let mode = options.mode.unwrap_or_else(|| {
        if options.query.is_some() {
            SearchMode::Hybrid
        } else {
            SearchMode::Metadata
        }
    });
    match mode {
        SearchMode::Metadata => metadata_search(conn, &options),
        SearchMode::Fts => fts_search(conn, &options),
        SearchMode::Semantic => semantic_search(conn, &options, query_embedding.as_deref()),
        SearchMode::Hybrid => hybrid_search(conn, &options, query_embedding.as_deref()),
    }
}

pub fn related_conversations(
    conn: &Connection,
    conversation_id: &str,
    limit: usize,
) -> anyhow::Result<Vec<SearchResult>> {
    let bytes: Option<Vec<u8>> = conn
        .query_row(
            "SELECT embedding_blob FROM conversations WHERE conversation_id = ?1",
            params![conversation_id],
            |row| row.get(0),
        )
        .optional()?;
    let Some(bytes) = bytes else {
        return Ok(Vec::new());
    };
    let target = decode_embedding(&bytes);
    let mut stmt = conn.prepare(
        r#"
        SELECT conversation_id, title, create_time, update_time, default_model_slug, embedding_blob, risk_flags_json, topic_tags_json,
               source, source_instance, source_conversation_id
        FROM conversations
        WHERE embedding_blob IS NOT NULL AND conversation_id != ?1
        "#,
    )?;
    let rows = stmt.query_map(params![conversation_id], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Option<f64>>(2)?,
            row.get::<_, Option<f64>>(3)?,
            row.get::<_, Option<String>>(4)?,
            row.get::<_, Vec<u8>>(5)?,
            row.get::<_, String>(6)?,
            row.get::<_, String>(7)?,
            row.get::<_, String>(8)?,
            row.get::<_, Option<String>>(9)?,
            row.get::<_, String>(10)?,
        ))
    })?;
    let mut scored = Vec::new();
    for row in rows {
        let (
            id,
            title,
            create_time,
            update_time,
            model,
            embedding_blob,
            risk_flags_json,
            topic_tags_json,
            source,
            source_instance,
            source_conversation_id,
        ) = row?;
        let score = cosine_similarity(&target, &decode_embedding(&embedding_blob)).unwrap_or(0.0);
        scored.push(SearchResult {
            conversation_id: id,
            source,
            source_instance,
            source_conversation_id,
            title,
            create_time,
            update_time,
            default_model_slug: model,
            score: Some(score),
            snippet: None,
            risk_flags: serde_json::from_str(&risk_flags_json).unwrap_or_default(),
            topic_tags: serde_json::from_str(&topic_tags_json).unwrap_or_default(),
        });
    }
    scored.sort_by(|a, b| {
        b.score
            .unwrap_or_default()
            .partial_cmp(&a.score.unwrap_or_default())
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    scored.truncate(limit);
    Ok(scored)
}

fn metadata_search(
    conn: &Connection,
    options: &SearchOptions,
) -> anyhow::Result<Vec<SearchResult>> {
    let limit = options.limit.unwrap_or(25);
    let mut stmt = conn.prepare(
        r#"
        SELECT conversation_id, title, create_time, update_time, default_model_slug, risk_flags_json, topic_tags_json,
               source, source_instance, source_conversation_id
        FROM conversations
        ORDER BY COALESCE(update_time, create_time) DESC
        LIMIT ?1
        "#,
    )?;
    let rows = stmt.query_map(params![limit as i64], |row| {
        Ok(SearchResult {
            conversation_id: row.get(0)?,
            source: row.get(7)?,
            source_instance: row.get(8)?,
            source_conversation_id: row.get(9)?,
            title: row.get(1)?,
            create_time: row.get(2)?,
            update_time: row.get(3)?,
            default_model_slug: row.get(4)?,
            score: None,
            snippet: None,
            risk_flags: serde_json::from_str::<Vec<String>>(&row.get::<_, String>(5)?)
                .unwrap_or_default(),
            topic_tags: serde_json::from_str::<Vec<String>>(&row.get::<_, String>(6)?)
                .unwrap_or_default(),
        })
    })?;
    let mut results = Vec::new();
    for row in rows {
        let result = row?;
        if matches_filters(&result, options) {
            results.push(result);
        }
    }
    Ok(results)
}

fn fts_search(conn: &Connection, options: &SearchOptions) -> anyhow::Result<Vec<SearchResult>> {
    let query = options
        .query
        .as_deref()
        .ok_or_else(|| anyhow!("fts search requires a query"))?;
    let limit = options.limit.unwrap_or(25);
    let mut stmt = conn.prepare(
        r#"
        SELECT c.conversation_id, c.title, c.create_time, c.update_time, c.default_model_slug,
               snippet(conversation_fts, 2, '[', ']', '…', 18) AS snippet_text,
               bm25(conversation_fts) AS rank,
               c.risk_flags_json, c.topic_tags_json, c.source, c.source_instance, c.source_conversation_id
        FROM conversation_fts
        JOIN conversations c ON c.conversation_id = conversation_fts.conversation_id
        WHERE conversation_fts MATCH ?1
        ORDER BY rank
        LIMIT ?2
        "#,
    )?;
    let rows = stmt.query_map(params![query, limit as i64], |row| {
        Ok(SearchResult {
            conversation_id: row.get(0)?,
            source: row.get(9)?,
            source_instance: row.get(10)?,
            source_conversation_id: row.get(11)?,
            title: row.get(1)?,
            create_time: row.get(2)?,
            update_time: row.get(3)?,
            default_model_slug: row.get(4)?,
            score: row.get::<_, Option<f64>>(6)?,
            snippet: row.get(5)?,
            risk_flags: serde_json::from_str::<Vec<String>>(&row.get::<_, String>(7)?)
                .unwrap_or_default(),
            topic_tags: serde_json::from_str::<Vec<String>>(&row.get::<_, String>(8)?)
                .unwrap_or_default(),
        })
    })?;
    let mut results = Vec::new();
    for row in rows {
        let result = row?;
        if matches_filters(&result, options) {
            results.push(result);
        }
    }
    Ok(results)
}

fn semantic_search(
    conn: &Connection,
    options: &SearchOptions,
    query_embedding: Option<&[f32]>,
) -> anyhow::Result<Vec<SearchResult>> {
    let query_embedding =
        query_embedding.ok_or_else(|| anyhow!("semantic search requires a query embedding"))?;
    let mut stmt = conn.prepare(
        r#"
        SELECT conversation_id, title, create_time, update_time, default_model_slug, embedding_blob, risk_flags_json, topic_tags_json,
               source, source_instance, source_conversation_id
        FROM conversations
        WHERE embedding_blob IS NOT NULL
        "#,
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Option<f64>>(2)?,
            row.get::<_, Option<f64>>(3)?,
            row.get::<_, Option<String>>(4)?,
            row.get::<_, Vec<u8>>(5)?,
            row.get::<_, String>(6)?,
            row.get::<_, String>(7)?,
            row.get::<_, String>(8)?,
            row.get::<_, Option<String>>(9)?,
            row.get::<_, String>(10)?,
        ))
    })?;
    let mut results = Vec::new();
    for row in rows {
        let (
            id,
            title,
            create_time,
            update_time,
            model,
            embedding_blob,
            risk_flags_json,
            topic_tags_json,
            source,
            source_instance,
            source_conversation_id,
        ) = row?;
        let score =
            cosine_similarity(query_embedding, &decode_embedding(&embedding_blob)).unwrap_or(0.0);
        let result = SearchResult {
            conversation_id: id,
            source,
            source_instance,
            source_conversation_id,
            title,
            create_time,
            update_time,
            default_model_slug: model,
            score: Some(score),
            snippet: None,
            risk_flags: serde_json::from_str::<Vec<String>>(&risk_flags_json).unwrap_or_default(),
            topic_tags: serde_json::from_str::<Vec<String>>(&topic_tags_json).unwrap_or_default(),
        };
        if matches_filters(&result, options) {
            results.push(result);
        }
    }
    results.sort_by(|a, b| {
        b.score
            .unwrap_or_default()
            .partial_cmp(&a.score.unwrap_or_default())
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    results.truncate(options.limit.unwrap_or(25));
    Ok(results)
}

fn hybrid_search(
    conn: &Connection,
    options: &SearchOptions,
    query_embedding: Option<&[f32]>,
) -> anyhow::Result<Vec<SearchResult>> {
    let fts = fts_search(conn, options).unwrap_or_else(|_| Vec::new());
    let semantic = semantic_search(conn, options, query_embedding).unwrap_or_else(|_| Vec::new());
    let mut by_id = HashMap::new();
    for result in fts.iter().chain(semantic.iter()) {
        by_id
            .entry(result.conversation_id.clone())
            .or_insert_with(|| result.clone());
    }
    let fts_ranked = fts
        .iter()
        .enumerate()
        .map(|(idx, item)| {
            (
                item.conversation_id.clone(),
                item.score.unwrap_or(-(idx as f64)),
            )
        })
        .collect::<Vec<_>>();
    let semantic_ranked = semantic
        .iter()
        .enumerate()
        .map(|(idx, item)| {
            (
                item.conversation_id.clone(),
                item.score.unwrap_or(-(idx as f64)),
            )
        })
        .collect::<Vec<_>>();
    let fused = reciprocal_rank_fusion(&[fts_ranked, semantic_ranked], 60.0);
    let mut results = Vec::new();
    for (conversation_id, score) in fused.into_iter().take(options.limit.unwrap_or(25)) {
        if let Some(mut result) = by_id.remove(&conversation_id) {
            result.score = Some(score);
            results.push(result);
        }
    }
    Ok(results)
}

fn matches_filters(result: &SearchResult, options: &SearchOptions) -> bool {
    if !options.sources.is_empty()
        && !options
            .sources
            .iter()
            .any(|source| source == &result.source)
    {
        return false;
    }
    if let Some(date_from) = options.date_from {
        let timestamp = result
            .update_time
            .or(result.create_time)
            .unwrap_or_default();
        if timestamp < date_from {
            return false;
        }
    }
    if let Some(date_to) = options.date_to {
        let timestamp = result
            .update_time
            .or(result.create_time)
            .unwrap_or_default();
        if timestamp > date_to {
            return false;
        }
    }
    if let Some(model) = &options.model
        && result.default_model_slug.as_deref() != Some(model.as_str())
    {
        return false;
    }
    if !options.risk_flags.is_empty()
        && !options
            .risk_flags
            .iter()
            .all(|flag| result.risk_flags.iter().any(|item| item == flag))
    {
        return false;
    }
    if !options.topic_tags.is_empty()
        && !options
            .topic_tags
            .iter()
            .all(|tag| result.topic_tags.iter().any(|item| item == tag))
    {
        return false;
    }
    true
}
