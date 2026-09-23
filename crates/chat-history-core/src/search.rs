use std::collections::{HashMap, HashSet};

use anyhow::anyhow;
use rusqlite::{Connection, OptionalExtension, params};

use crate::{
    embedding::EmbeddingVector,
    ingest::decode_embedding,
    models::{SearchMode, SearchOptions, SearchResult},
    ranking::{cosine_similarity, reciprocal_rank_fusion},
};

pub fn search(
    conn: &Connection,
    options: SearchOptions,
    query_embedding: Option<EmbeddingVector>,
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
        SearchMode::Semantic => semantic_search(conn, &options, query_embedding.as_ref()),
        SearchMode::Hybrid => hybrid_search(conn, &options, query_embedding.as_ref()),
    }
}

pub fn related_conversations(
    conn: &Connection,
    conversation_id: &str,
    limit: usize,
) -> anyhow::Result<Vec<SearchResult>> {
    let target: Option<(Vec<u8>, String, i64)> = conn
        .query_row(
            r#"
            SELECT embedding_blob, embedding_model, embedding_dimensions
            FROM conversations
            WHERE conversation_id = ?1
              AND embedding_blob IS NOT NULL
              AND embedding_model IS NOT NULL
              AND embedding_dimensions IS NOT NULL
            "#,
            params![conversation_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let Some((bytes, embedding_model, embedding_dimensions)) = target else {
        return Ok(Vec::new());
    };
    let target = decode_embedding(&bytes);
    if target.len() != embedding_dimensions as usize {
        return Ok(Vec::new());
    }
    let mut stmt = conn.prepare(
        r#"
        SELECT conversation_id, title, create_time, update_time, default_model_slug, embedding_blob, risk_flags_json, topic_tags_json,
               source, source_instance, source_conversation_id
        FROM conversations
        WHERE embedding_blob IS NOT NULL
          AND embedding_model = ?2
          AND embedding_dimensions = ?3
          AND conversation_id != ?1
        "#,
    )?;
    let rows = stmt.query_map(
        params![conversation_id, embedding_model, embedding_dimensions],
        |row| {
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
        },
    )?;
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
    query_embedding: Option<&EmbeddingVector>,
) -> anyhow::Result<Vec<SearchResult>> {
    let query_embedding =
        query_embedding.ok_or_else(|| anyhow!("semantic search requires a query embedding"))?;
    let query_dimensions = query_embedding.values.len();
    anyhow::ensure!(query_dimensions > 0, "semantic query embedding is empty");
    let mut stmt = conn.prepare(
        r#"
        SELECT ec.conversation_id, c.parent_conversation_id, ec.chunk_index, ec.embedding_blob
        FROM conversation_embedding_chunks ec
        JOIN conversations c ON c.conversation_id = ec.conversation_id
        WHERE ec.embedding_model = ?1
          AND ec.embedding_dimensions = ?2
        "#,
    )?;
    let rows = stmt.query_map(
        params![query_embedding.model_id, query_dimensions as i64],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, Vec<u8>>(3)?,
            ))
        },
    )?;
    let mut best_by_anchor: HashMap<String, (f64, String, i64)> = HashMap::new();
    for row in rows {
        let (evidence_id, parent_id, chunk_index, embedding_blob) = row?;
        let stored = decode_embedding(&embedding_blob);
        if stored.len() != query_dimensions {
            continue;
        }
        let score = cosine_similarity(&query_embedding.values, &stored).unwrap_or(0.0);
        let anchor_id = parent_id.unwrap_or_else(|| evidence_id.clone());
        match best_by_anchor.get_mut(&anchor_id) {
            Some((best_score, best_evidence, best_chunk)) if score > *best_score => {
                *best_score = score;
                *best_evidence = evidence_id;
                *best_chunk = chunk_index;
            }
            None => {
                best_by_anchor.insert(anchor_id, (score, evidence_id, chunk_index));
            }
            _ => {}
        }
    }

    let mut results = Vec::new();
    for (anchor_id, (score, evidence_id, chunk_index)) in best_by_anchor {
        let Some(mut result) = load_search_result(conn, &anchor_id)? else {
            continue;
        };
        result.score = Some(score);
        result.snippet = Some(format!(
            "semantic evidence: {evidence_id} chunk {chunk_index}"
        ));
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

fn load_search_result(
    conn: &Connection,
    conversation_id: &str,
) -> anyhow::Result<Option<SearchResult>> {
    conn.query_row(
        r#"
        SELECT conversation_id, title, create_time, update_time, default_model_slug,
               risk_flags_json, topic_tags_json, source, source_instance, source_conversation_id
        FROM conversations
        WHERE conversation_id = ?1
        "#,
        params![conversation_id],
        |row| {
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
        },
    )
    .optional()
    .map_err(Into::into)
}

fn hybrid_search(
    conn: &Connection,
    options: &SearchOptions,
    query_embedding: Option<&EmbeddingVector>,
) -> anyhow::Result<Vec<SearchResult>> {
    let requested_limit = options.limit.unwrap_or(25);
    let candidate_limit = requested_limit.saturating_mul(5).max(50);
    let mut candidate_options = options.clone();
    candidate_options.limit = Some(candidate_limit);

    let lexical = hybrid_lexical_search(conn, &candidate_options).unwrap_or_else(|_| Vec::new());
    let semantic =
        semantic_search(conn, &candidate_options, query_embedding).unwrap_or_else(|_| Vec::new());
    let mut by_id = HashMap::new();
    for result in lexical.iter().chain(semantic.iter()) {
        by_id
            .entry(result.conversation_id.clone())
            .or_insert_with(|| result.clone());
    }
    let lexical_ranked = lexical
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
    let fused = reciprocal_rank_fusion(&[lexical_ranked, semantic_ranked], 60.0);
    let mut results = Vec::new();
    for (conversation_id, score) in fused.into_iter().take(requested_limit) {
        if let Some(mut result) = by_id.remove(&conversation_id) {
            result.score = Some(score);
            results.push(result);
        }
    }
    Ok(results)
}

fn hybrid_lexical_search(
    conn: &Connection,
    options: &SearchOptions,
) -> anyhow::Result<Vec<SearchResult>> {
    let query = options
        .query
        .as_deref()
        .ok_or_else(|| anyhow!("hybrid lexical search requires a query"))?;
    let limit = options.limit.unwrap_or(50);

    let ascii = hybrid_ascii_fts_search(conn, options, query, limit)?;
    let cjk = hybrid_cjk_substring_search(conn, options, query, limit)?;
    if ascii.is_empty() {
        return Ok(cjk);
    }
    if cjk.is_empty() {
        return Ok(ascii);
    }

    let mut by_id = HashMap::new();
    for result in ascii.iter().chain(cjk.iter()) {
        by_id
            .entry(result.conversation_id.clone())
            .or_insert_with(|| result.clone());
    }
    let ascii_ranked = ascii
        .iter()
        .map(|result| {
            (
                result.conversation_id.clone(),
                result.score.unwrap_or_default(),
            )
        })
        .collect::<Vec<_>>();
    let cjk_ranked = cjk
        .iter()
        .map(|result| {
            (
                result.conversation_id.clone(),
                result.score.unwrap_or_default(),
            )
        })
        .collect::<Vec<_>>();
    let fused = reciprocal_rank_fusion(&[ascii_ranked, cjk_ranked], 20.0);
    let mut results = Vec::new();
    for (conversation_id, score) in fused.into_iter().take(limit) {
        if let Some(mut result) = by_id.remove(&conversation_id) {
            result.score = Some(score);
            results.push(result);
        }
    }
    Ok(results)
}

fn hybrid_ascii_fts_search(
    conn: &Connection,
    options: &SearchOptions,
    query: &str,
    limit: usize,
) -> anyhow::Result<Vec<SearchResult>> {
    let Some(fts_query) = build_hybrid_fts_query(query) else {
        return Ok(Vec::new());
    };
    let fetch_limit = limit.saturating_mul(4).max(100);
    let mut stmt = conn.prepare(
        r#"
        SELECT c.conversation_id, c.parent_conversation_id,
               snippet(conversation_fts, 2, '[', ']', '…', 24) AS snippet_text,
               bm25(conversation_fts) AS rank
        FROM conversation_fts
        JOIN conversations c ON c.conversation_id = conversation_fts.conversation_id
        WHERE conversation_fts MATCH ?1
        ORDER BY rank
        LIMIT ?2
        "#,
    )?;
    let rows = stmt.query_map(params![fts_query, fetch_limit as i64], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, Option<String>>(1)?,
            row.get::<_, Option<String>>(2)?,
            row.get::<_, f64>(3)?,
        ))
    })?;

    let mut best_by_anchor = HashMap::<String, (usize, String, Option<String>, f64)>::new();
    for (rank_index, row) in rows.enumerate() {
        let (evidence_id, parent_id, snippet, bm25) = row?;
        let anchor_id = parent_id.unwrap_or_else(|| evidence_id.clone());
        best_by_anchor
            .entry(anchor_id)
            .and_modify(|current| {
                if rank_index < current.0 {
                    *current = (rank_index, evidence_id.clone(), snippet.clone(), bm25);
                }
            })
            .or_insert((rank_index, evidence_id, snippet, bm25));
    }

    let mut ranked = best_by_anchor.into_iter().collect::<Vec<_>>();
    ranked.sort_by_key(|(_, (rank, _, _, _))| *rank);
    lexical_anchor_results(conn, options, ranked, limit, "fts")
}

fn hybrid_cjk_substring_search(
    conn: &Connection,
    options: &SearchOptions,
    query: &str,
    limit: usize,
) -> anyhow::Result<Vec<SearchResult>> {
    let total_docs: f64 = conn.query_row("SELECT COUNT(*) FROM conversations", [], |row| {
        row.get::<_, i64>(0).map(|value| value as f64)
    })?;
    if total_docs == 0.0 {
        return Ok(Vec::new());
    }

    let mut weighted_terms = Vec::<(String, f64)>::new();
    for term in extract_cjk_terms(query) {
        let document_frequency: i64 = conn.query_row(
            r#"
            SELECT COUNT(*)
            FROM conversation_fts
            WHERE instr(title, ?1) > 0
               OR instr(transcript_text, ?1) > 0
               OR instr(summary_text, ?1) > 0
               OR instr(topic_tags, ?1) > 0
            "#,
            params![term],
            |row| row.get(0),
        )?;
        if document_frequency == 0 {
            continue;
        }
        let idf = ((total_docs + 1.0) / (document_frequency as f64 + 1.0)).ln() + 1.0;
        let length_weight = term.chars().count() as f64;
        weighted_terms.push((term, idf * length_weight));
    }
    weighted_terms.sort_by(|left, right| {
        right
            .1
            .partial_cmp(&left.1)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    weighted_terms.truncate(16);
    if weighted_terms.is_empty() {
        return Ok(Vec::new());
    }

    let mut evidence_scores = HashMap::<String, (Option<String>, f64, Vec<String>)>::new();
    for (term, weight) in &weighted_terms {
        let mut stmt = conn.prepare(
            r#"
            SELECT c.conversation_id, c.parent_conversation_id
            FROM conversation_fts
            JOIN conversations c ON c.conversation_id = conversation_fts.conversation_id
            WHERE instr(conversation_fts.title, ?1) > 0
               OR instr(conversation_fts.transcript_text, ?1) > 0
               OR instr(conversation_fts.summary_text, ?1) > 0
               OR instr(conversation_fts.topic_tags, ?1) > 0
            "#,
        )?;
        let rows = stmt.query_map(params![term], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
        })?;
        for row in rows {
            let (evidence_id, parent_id) = row?;
            let entry = evidence_scores
                .entry(evidence_id)
                .or_insert_with(|| (parent_id, 0.0, Vec::new()));
            entry.1 += *weight;
            entry.2.push(term.clone());
        }
    }

    let mut best_by_anchor = HashMap::<String, (f64, String, Vec<String>)>::new();
    for (evidence_id, (parent_id, score, terms)) in evidence_scores {
        let anchor_id = parent_id.unwrap_or_else(|| evidence_id.clone());
        match best_by_anchor.get_mut(&anchor_id) {
            Some((best_score, best_evidence, best_terms)) if score > *best_score => {
                *best_score = score;
                *best_evidence = evidence_id;
                *best_terms = terms;
            }
            None => {
                best_by_anchor.insert(anchor_id, (score, evidence_id, terms));
            }
            _ => {}
        }
    }

    let mut ranked = best_by_anchor.into_iter().collect::<Vec<_>>();
    ranked.sort_by(|left, right| {
        right
            .1
            .0
            .partial_cmp(&left.1.0)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut results = Vec::new();
    for (anchor_id, (score, evidence_id, terms)) in ranked.into_iter().take(limit) {
        let Some(mut result) = load_search_result(conn, &anchor_id)? else {
            continue;
        };
        result.score = Some(score);
        result.snippet = Some(format!(
            "lexical evidence: {evidence_id}; matched CJK terms: {}",
            terms.join(", ")
        ));
        if matches_filters(&result, options) {
            results.push(result);
        }
    }
    Ok(results)
}

fn lexical_anchor_results(
    conn: &Connection,
    options: &SearchOptions,
    ranked: Vec<(String, (usize, String, Option<String>, f64))>,
    limit: usize,
    evidence_kind: &str,
) -> anyhow::Result<Vec<SearchResult>> {
    let mut results = Vec::new();
    for (anchor_id, (rank, evidence_id, snippet, bm25)) in ranked.into_iter().take(limit) {
        let Some(mut result) = load_search_result(conn, &anchor_id)? else {
            continue;
        };
        result.score = Some(-(rank as f64) + bm25.signum() * 1e-9);
        result.snippet = Some(match snippet {
            Some(snippet) if !snippet.trim().is_empty() => {
                format!("{evidence_kind} evidence: {evidence_id}; {snippet}")
            }
            _ => format!("{evidence_kind} evidence: {evidence_id}"),
        });
        if matches_filters(&result, options) {
            results.push(result);
        }
    }
    Ok(results)
}

fn build_hybrid_fts_query(query: &str) -> Option<String> {
    let stopwords = [
        "a", "an", "and", "are", "as", "at", "be", "been", "but", "by", "decide", "decided", "did",
        "do", "does", "for", "from", "how", "i", "in", "is", "it", "not", "of", "old", "on", "or",
        "our", "the", "to", "we", "what", "when", "where", "why", "with",
    ]
    .into_iter()
    .collect::<HashSet<_>>();
    let mut terms = Vec::new();
    let mut seen = HashSet::new();
    let mut current = String::new();
    let flush = |current: &mut String, terms: &mut Vec<String>, seen: &mut HashSet<String>| {
        if current.is_empty() {
            return;
        }
        let token = std::mem::take(current);
        if token.len() < 2 || stopwords.contains(token.as_str()) {
            return;
        }
        for variant in ascii_lexical_variants(&token) {
            if seen.insert(variant.clone()) {
                terms.push(variant);
            }
        }
    };
    for ch in query.chars().flat_map(|ch| ch.to_lowercase()) {
        if ch.is_ascii_alphanumeric() {
            current.push(ch);
        } else {
            flush(&mut current, &mut terms, &mut seen);
        }
    }
    flush(&mut current, &mut terms, &mut seen);
    if terms.is_empty() {
        None
    } else {
        Some(terms.join(" OR "))
    }
}

fn ascii_lexical_variants(token: &str) -> Vec<String> {
    let mut variants = vec![token.to_string()];
    if token.len() > 4 && token.ends_with('s') && !token.ends_with("ss") {
        variants.push(token[..token.len() - 1].to_string());
    }
    if token.len() > 5 && token.ends_with("ed") {
        variants.push(token[..token.len() - 2].to_string());
    }
    if token.len() > 6 && token.ends_with("ing") {
        variants.push(token[..token.len() - 3].to_string());
    }
    variants
}

fn extract_cjk_terms(query: &str) -> Vec<String> {
    let mut runs = Vec::<Vec<char>>::new();
    let mut current = Vec::new();
    for ch in query.chars() {
        if is_cjk_ideograph(ch) {
            current.push(ch);
        } else if !current.is_empty() {
            runs.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        runs.push(current);
    }

    let mut terms = Vec::new();
    let mut seen = HashSet::new();
    for run in runs {
        for width in (2..=4).rev() {
            if run.len() < width {
                continue;
            }
            for window in run.windows(width) {
                let term = window.iter().collect::<String>();
                if seen.insert(term.clone()) {
                    terms.push(term);
                }
            }
        }
    }
    terms
}

fn is_cjk_ideograph(ch: char) -> bool {
    matches!(
        ch as u32,
        0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF | 0x20000..=0x2FA1F
    )
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

#[cfg(test)]
mod tests {
    use rusqlite::{Connection, params};

    use super::{
        build_hybrid_fts_query, extract_cjk_terms, hybrid_lexical_search, semantic_search,
    };
    use crate::{
        embedding::EmbeddingVector,
        ingest::encode_embedding,
        models::{SearchMode, SearchOptions},
        sql::SCHEMA,
    };

    fn insert_conversation(
        conn: &Connection,
        id: &str,
        model: &str,
        dimensions: i64,
        vector: &[f32],
    ) {
        conn.execute(
            r#"
            INSERT INTO conversations (
              conversation_id, archive_id, archive_member, source_member, title,
              transcript_text, transcript_digest, raw_conversation_zstd,
              raw_json_sha256_hex, embedding_blob, embedding_dimensions, embedding_model,
              source, source_conversation_id
            ) VALUES (?1, 1, 'member', 'source', ?1, '', '', X'00', 'sha', ?2, ?3, ?4, 'codex', ?1)
            "#,
            params![id, encode_embedding(vector), dimensions, model],
        )
        .unwrap();
        conn.execute(
            r#"
            INSERT INTO conversation_embedding_chunks (
              conversation_id, chunk_index, embedding_blob, embedding_dimensions, embedding_model
            ) VALUES (?1, 0, ?2, ?3, ?4)
            "#,
            params![id, encode_embedding(vector), dimensions, model],
        )
        .unwrap();
    }

    fn insert_lexical_fixture(
        conn: &Connection,
        id: &str,
        parent_id: Option<&str>,
        title: &str,
        transcript: &str,
    ) {
        conn.execute(
            r#"
            INSERT INTO conversations (
              conversation_id, archive_id, archive_member, source_member, title,
              transcript_text, transcript_digest, raw_conversation_zstd,
              raw_json_sha256_hex, source, source_conversation_id, parent_conversation_id
            ) VALUES (?1, 1, 'member', 'source', ?2, ?3, ?3, X'00', 'sha', 'codex', ?1, ?4)
            "#,
            params![id, title, transcript, parent_id],
        )
        .unwrap();
        conn.execute(
            r#"
            INSERT INTO conversation_fts (
              conversation_id, title, transcript_text, summary_text, topic_tags
            ) VALUES (?1, ?2, ?3, '', '')
            "#,
            params![id, title, transcript],
        )
        .unwrap();
    }

    #[test]
    fn semantic_search_rejects_mismatched_embedding_model_and_dimensions() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(SCHEMA).unwrap();
        conn.execute(
            "INSERT INTO archives (id, archive_path, source_path, sha256_hex, size_bytes, import_mode) VALUES (1, 'a', 'b', 'c', 0, 'copy')",
            [],
        )
        .unwrap();

        insert_conversation(&conn, "compatible", "model-a", 2, &[1.0, 0.0]);
        insert_conversation(&conn, "wrong-model", "model-b", 2, &[1.0, 0.0]);
        insert_conversation(&conn, "wrong-dimensions", "model-a", 3, &[1.0, 0.0, 0.0]);

        let options = SearchOptions {
            query: Some("migration".to_string()),
            mode: Some(SearchMode::Semantic),
            limit: Some(10),
            ..SearchOptions::default()
        };
        let query = EmbeddingVector {
            values: vec![1.0, 0.0],
            model_id: "model-a".to_string(),
        };

        let results = semantic_search(&conn, &options, Some(&query)).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].conversation_id, "compatible");
    }

    #[test]
    fn semantic_child_chunk_scores_are_collapsed_to_parent_anchor() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(SCHEMA).unwrap();
        conn.execute(
            "INSERT INTO archives (id, archive_path, source_path, sha256_hex, size_bytes, import_mode) VALUES (1, 'a', 'b', 'c', 0, 'copy')",
            [],
        )
        .unwrap();

        insert_conversation(&conn, "parent", "model-a", 2, &[0.0, 1.0]);
        insert_conversation(&conn, "child", "model-a", 2, &[1.0, 0.0]);
        conn.execute(
            "UPDATE conversations SET parent_conversation_id = 'parent' WHERE conversation_id = 'child'",
            [],
        )
        .unwrap();

        let options = SearchOptions {
            query: Some("migration".to_string()),
            mode: Some(SearchMode::Semantic),
            limit: Some(10),
            ..SearchOptions::default()
        };
        let query = EmbeddingVector {
            values: vec![1.0, 0.0],
            model_id: "model-a".to_string(),
        };

        let results = semantic_search(&conn, &options, Some(&query)).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].conversation_id, "parent");
        assert!(
            results[0]
                .snippet
                .as_deref()
                .is_some_and(|snippet| snippet.contains("child chunk 0"))
        );
    }

    #[test]
    fn hybrid_fts_query_removes_stopwords_and_expands_plural_migrations() {
        let query = build_hybrid_fts_query("why did we decide not to rewrite old migrations")
            .expect("normalized FTS query");
        assert!(query.contains("rewrite"));
        assert!(query.contains("migrations"));
        assert!(query.contains("migration"));
        assert!(!query.split(" OR ").any(|term| term == "why"));
        assert!(!query.split(" OR ").any(|term| term == "did"));
        assert!(!query.split(" OR ").any(|term| term == "decide"));
        assert!(!query.split(" OR ").any(|term| term == "old"));
    }

    #[test]
    fn cjk_query_generates_database_migration_and_rewrite_terms() {
        let terms = extract_cjk_terms("为什么旧的数据库迁移记录不能重写");
        assert!(terms.iter().any(|term| term == "数据库"));
        assert!(terms.iter().any(|term| term == "迁移"));
        assert!(terms.iter().any(|term| term == "重写"));
    }

    #[test]
    fn hybrid_lexical_search_collapses_child_evidence_and_prefers_specific_cjk_terms() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(SCHEMA).unwrap();
        conn.execute(
            "INSERT INTO archives (id, archive_path, source_path, sha256_hex, size_bytes, import_mode) VALUES (1, 'a', 'b', 'c', 0, 'copy')",
            [],
        )
        .unwrap();

        insert_lexical_fixture(&conn, "parent", None, "LEMonX", "主线程");
        insert_lexical_fixture(
            &conn,
            "migration-child",
            Some("parent"),
            "Migration task",
            "已经应用的数据库迁移历史不得修改。不要重写旧 migration，后续 schema change 应新增 migration。",
        );
        insert_lexical_fixture(
            &conn,
            "annotation",
            None,
            "Annotation history",
            "事故标注历史不能重写，正式标注流程需要保留旧记录。",
        );

        let options = SearchOptions {
            query: Some("为什么旧的数据库迁移记录不能重写".to_string()),
            mode: Some(SearchMode::Hybrid),
            limit: Some(10),
            ..SearchOptions::default()
        };
        let results = hybrid_lexical_search(&conn, &options).unwrap();
        assert!(!results.is_empty());
        assert_eq!(results[0].conversation_id, "parent");
        assert!(
            results[0]
                .snippet
                .as_deref()
                .is_some_and(|snippet| snippet.contains("migration-child"))
        );
    }

    #[test]
    fn hybrid_lexical_search_english_natural_language_finds_migration_parent() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(SCHEMA).unwrap();
        conn.execute(
            "INSERT INTO archives (id, archive_path, source_path, sha256_hex, size_bytes, import_mode) VALUES (1, 'a', 'b', 'c', 0, 'copy')",
            [],
        )
        .unwrap();

        insert_lexical_fixture(&conn, "parent", None, "LEMonX", "main thread");
        insert_lexical_fixture(
            &conn,
            "migration-child",
            Some("parent"),
            "Migration history",
            "Do not rewrite old migrations. Applied database migration history is immutable and later schema changes need a new migration.",
        );
        insert_lexical_fixture(
            &conn,
            "archery",
            None,
            "Archery notes",
            "Old arrow tuning notes and compound bow setup history.",
        );

        let options = SearchOptions {
            query: Some("why did we decide not to rewrite old migrations".to_string()),
            mode: Some(SearchMode::Hybrid),
            limit: Some(10),
            ..SearchOptions::default()
        };
        let results = hybrid_lexical_search(&conn, &options).unwrap();
        assert!(!results.is_empty());
        assert_eq!(results[0].conversation_id, "parent");
    }
}
