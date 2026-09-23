use std::{
    collections::{BTreeSet, HashMap, HashSet},
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
};

use anyhow::anyhow;
use rusqlite::{Connection, OptionalExtension, params};
use schemars::JsonSchema;
use serde::de::{DeserializeSeed, SeqAccess, Visitor};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use zstd::stream::{decode_all, encode_all};

use crate::{
    archive::{
        ManagedArchive, build_asset_index, digest_file, extract_file_tokens, list_nested_members,
        list_outer_members, materialize_nested_member,
    },
    data_home::{DataHome, ImportMode},
    db::{fetch_stats, open_database, upsert_job},
    embedding::EmbeddingVector,
    models::{
        AttachmentRecord, ConversationDetail, ConversationRecord, IndexStats, JobKind, JobStatus,
        NormalizedConversation, SearchOptions, SearchResult, SummaryRecord,
    },
    openai::OpenAiClient,
    search,
};

#[derive(Debug, Clone)]
pub struct ImportOptions {
    pub source_archive: PathBuf,
    pub mode: ImportMode,
    pub run_api_jobs: bool,
    pub force_summaries: bool,
    pub force_embeddings: bool,
}

impl Default for ImportOptions {
    fn default() -> Self {
        Self {
            source_archive: std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("."))
                .join("Downloads")
                .join("openai-export.zip"),
            mode: ImportMode::Adopt,
            run_api_jobs: true,
            force_summaries: false,
            force_embeddings: false,
        }
    }
}

#[derive(Debug, Clone, Default, serde::Serialize, JsonSchema)]
pub struct ImportReport {
    pub archive_path: PathBuf,
    pub conversations_indexed: usize,
    pub messages_indexed: usize,
    pub attachments_indexed: usize,
    pub summaries_completed: usize,
    pub embeddings_completed: usize,
}

#[derive(Debug, Clone)]
pub struct IndexService {
    pub(crate) data_home: DataHome,
    pub(crate) openai: Option<OpenAiClient>,
}

impl IndexService {
    pub fn new(data_home: DataHome, openai: Option<OpenAiClient>) -> Self {
        Self { data_home, openai }
    }

    pub fn with_env(data_home: DataHome) -> Self {
        let embedding_cache_dir = data_home.paths().cache_dir.join("fastembed");
        let openai = OpenAiClient::from_env(embedding_cache_dir).ok();
        Self { data_home, openai }
    }

    pub fn managed_db_path(&self) -> PathBuf {
        self.data_home.paths().db_path
    }

    pub fn data_home(&self) -> &DataHome {
        &self.data_home
    }

    pub fn stats(&self) -> anyhow::Result<IndexStats> {
        let conn = open_database(&self.managed_db_path())?;
        fetch_stats(&conn)
    }

    pub fn source_conversation_ids(&self, source: &str) -> anyhow::Result<HashSet<String>> {
        let conn = open_database(&self.managed_db_path())?;
        let mut statement =
            conn.prepare("SELECT source_conversation_id FROM conversations WHERE source = ?1")?;
        Ok(statement
            .query_map(params![source], |row| row.get(0))?
            .collect::<Result<HashSet<String>, _>>()?)
    }

    pub async fn import_archive(&self, options: ImportOptions) -> anyhow::Result<ImportReport> {
        let (archive_path, _actual_mode) = self
            .data_home
            .stage_archive(&options.source_archive, options.mode)?;
        let managed = digest_file(&archive_path)?;
        let paths = self.data_home.paths();
        paths.ensure()?;
        let conn = open_database(&paths.db_path)?;

        let archive_id =
            self.upsert_archive(&conn, &managed, &options.source_archive, options.mode)?;
        let run_id = self.insert_run(&conn, "import", Some(&archive_path))?;
        let outer_members = list_outer_members(&archive_path)?;
        let conversation_members: Vec<_> = outer_members
            .into_iter()
            .filter(|name| name.contains("Conversations__") && name.ends_with(".zip"))
            .collect();
        let mut report = ImportReport {
            archive_path: archive_path.clone(),
            ..ImportReport::default()
        };

        for outer_member in conversation_members {
            let materialized =
                materialize_nested_member(&archive_path, &outer_member, &paths.tmp_dir)?;
            let nested_members = list_nested_members(&materialized.temp_path)?;
            let asset_index = build_asset_index(&nested_members);
            let json_members: Vec<_> = nested_members
                .iter()
                .filter(|name| name.starts_with("conversations-") && name.ends_with(".json"))
                .cloned()
                .collect();
            for json_member in json_members {
                self.ingest_json_member(
                    &conn,
                    archive_id,
                    &materialized.temp_path,
                    &outer_member,
                    &json_member,
                    &asset_index,
                    &mut report,
                )?;
            }
            let _ = fs::remove_file(&materialized.temp_path);
        }

        self.reindex_fts()?;
        if options.run_api_jobs {
            report.summaries_completed = self
                .rebuild_summaries(options.force_summaries, None, None)
                .await?;
            report.embeddings_completed = self
                .rebuild_embeddings(options.force_embeddings, None, None)
                .await?;
            self.reindex_fts()?;
        }

        conn.execute(
            "UPDATE runs SET status = 'complete', completed_at = CURRENT_TIMESTAMP, counters_json = ?2 WHERE id = ?1",
            params![run_id, serde_json::to_string(&report)?],
        )?;
        Ok(report)
    }

    pub fn import_normalized(
        &self,
        conversations: Vec<NormalizedConversation>,
        source_path: Option<&Path>,
    ) -> anyhow::Result<ImportReport> {
        self.import_normalized_batch(conversations, source_path, true)
    }

    pub fn import_normalized_batch(
        &self,
        conversations: Vec<NormalizedConversation>,
        source_path: Option<&Path>,
        reindex: bool,
    ) -> anyhow::Result<ImportReport> {
        let paths = self.data_home.paths();
        paths.ensure()?;
        let conn = open_database(&paths.db_path)?;
        let mut digest = Sha256::new();
        for conversation in &conversations {
            digest.update(conversation.canonical_id().as_bytes());
            digest.update(conversation.update_time.unwrap_or_default().to_le_bytes());
        }
        let source_label = source_path
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "normalized-input".to_string());
        conn.execute(
            r#"
            INSERT INTO archives (archive_path, source_path, sha256_hex, size_bytes, import_mode)
            VALUES (?1, ?1, ?2, ?3, 'normalized')
            "#,
            params![
                source_label,
                hex::encode(digest.finalize()),
                conversations.len() as i64
            ],
        )?;
        let archive_id = conn.last_insert_rowid();
        let run_id = self.insert_run(&conn, "normalized_import", source_path)?;
        let mut report = ImportReport {
            archive_path: source_path
                .unwrap_or(Path::new("normalized-input"))
                .to_path_buf(),
            ..ImportReport::default()
        };
        let import_result: anyhow::Result<()> = (|| {
            for conversation in conversations {
                let prepared = PreparedConversation::from_normalized(archive_id, conversation)?;
                write_conversation(&conn, &prepared)?;
                report.conversations_indexed += 1;
                report.messages_indexed += prepared.messages.len();
            }
            if reindex {
                self.reindex_fts()?;
            }
            Ok(())
        })();
        if let Err(error) = import_result {
            let notes = serde_json::json!({"error": error.to_string()});
            conn.execute(
                "UPDATE runs SET status = 'failed', completed_at = CURRENT_TIMESTAMP, counters_json = ?2, notes_json = ?3 WHERE id = ?1",
                params![run_id, serde_json::to_string(&report)?, notes.to_string()],
            )?;
            return Err(error);
        }
        conn.execute(
            "UPDATE runs SET status = 'complete', completed_at = CURRENT_TIMESTAMP, counters_json = ?2 WHERE id = ?1",
            params![run_id, serde_json::to_string(&report)?],
        )?;
        Ok(report)
    }

    pub async fn resume(&self) -> anyhow::Result<ImportReport> {
        let summaries_completed = self.rebuild_summaries(false, None, None).await?;
        let embeddings_completed = self.rebuild_embeddings(false, None, None).await?;
        self.reindex_fts()?;
        Ok(ImportReport {
            archive_path: self.data_home.paths().archive_path,
            summaries_completed,
            embeddings_completed,
            ..ImportReport::default()
        })
    }

    pub async fn rebuild_summaries(
        &self,
        force: bool,
        conversation_ids: Option<Vec<String>>,
        limit: Option<usize>,
    ) -> anyhow::Result<usize> {
        let openai = self
            .openai
            .as_ref()
            .ok_or_else(|| anyhow!("Codex CLI summary client is not configured"))?;
        let conn = open_database(&self.managed_db_path())?;
        let ids = collect_summary_targets(&conn, force, conversation_ids, limit)?;
        let mut completed = 0;
        for conversation_id in ids {
            upsert_job(
                &conn,
                &conversation_id,
                JobKind::Summary,
                JobStatus::Running,
                None,
                1,
            )?;
            let transcript = load_transcript(&conn, &conversation_id)?;
            match openai.summarize_conversation(&transcript).await {
                Ok(summary) => {
                    let summary_model = openai.summary_model_label();
                    conn.execute(
                        r#"
                        UPDATE conversations
                        SET summary_json = ?2,
                            summary_model = ?3,
                            summary_completed_at = CURRENT_TIMESTAMP,
                            risk_flags_json = ?4,
                            topic_tags_json = ?5,
                            redaction_notes_json = ?6
                        WHERE conversation_id = ?1
                        "#,
                        params![
                            conversation_id,
                            serde_json::to_string(&summary)?,
                            summary_model,
                            serde_json::to_string(&summary.risk_flags)?,
                            serde_json::to_string(&summary.candidate_topics)?,
                            serde_json::to_string(&summary.redaction_notes)?,
                        ],
                    )?;
                    upsert_job(
                        &conn,
                        &conversation_id,
                        JobKind::Summary,
                        JobStatus::Complete,
                        None,
                        0,
                    )?;
                    completed += 1;
                }
                Err(error) => {
                    upsert_job(
                        &conn,
                        &conversation_id,
                        JobKind::Summary,
                        JobStatus::Failed,
                        Some(&error.to_string()),
                        0,
                    )?;
                }
            }
        }
        Ok(completed)
    }

    pub async fn rebuild_embeddings(
        &self,
        force: bool,
        conversation_ids: Option<Vec<String>>,
        limit: Option<usize>,
    ) -> anyhow::Result<usize> {
        let openai = self
            .openai
            .as_ref()
            .ok_or_else(|| anyhow!("Codex/local embedding client is not configured"))?;
        let conn = open_database(&self.managed_db_path())?;
        let ids = collect_embedding_targets(&conn, force, conversation_ids, limit)?;
        let mut completed = 0;
        for conversation_id in ids {
            upsert_job(
                &conn,
                &conversation_id,
                JobKind::Embedding,
                JobStatus::Running,
                None,
                1,
            )?;
            let canonical = load_embedding_input(&conn, &conversation_id)?;
            match openai.embed_passage_chunks(&canonical).await {
                Ok(chunks) => {
                    let embedding = pool_embedding_chunks(&chunks)?;
                    let tx = conn.unchecked_transaction()?;
                    tx.execute(
                        "DELETE FROM conversation_embedding_chunks WHERE conversation_id = ?1",
                        params![conversation_id],
                    )?;
                    for (chunk_index, chunk) in chunks.iter().enumerate() {
                        tx.execute(
                            r#"
                            INSERT INTO conversation_embedding_chunks (
                              conversation_id, chunk_index, embedding_blob,
                              embedding_dimensions, embedding_model
                            ) VALUES (?1, ?2, ?3, ?4, ?5)
                            "#,
                            params![
                                conversation_id,
                                chunk_index as i64,
                                encode_embedding(&chunk.values),
                                chunk.values.len() as i64,
                                chunk.model_id,
                            ],
                        )?;
                    }
                    tx.execute(
                        r#"
                        UPDATE conversations
                        SET embedding_blob = ?2,
                            embedding_dimensions = ?3,
                            embedding_model = ?4,
                            embedding_completed_at = CURRENT_TIMESTAMP
                        WHERE conversation_id = ?1
                        "#,
                        params![
                            conversation_id,
                            encode_embedding(&embedding.values),
                            embedding.values.len() as i64,
                            embedding.model_id,
                        ],
                    )?;
                    upsert_job(
                        &tx,
                        &conversation_id,
                        JobKind::Embedding,
                        JobStatus::Complete,
                        None,
                        0,
                    )?;
                    tx.commit()?;
                    completed += 1;
                }
                Err(error) => {
                    upsert_job(
                        &conn,
                        &conversation_id,
                        JobKind::Embedding,
                        JobStatus::Failed,
                        Some(&error.to_string()),
                        0,
                    )?;
                }
            }
        }
        Ok(completed)
    }

    pub fn reindex_fts(&self) -> anyhow::Result<()> {
        let conn = open_database(&self.managed_db_path())?;
        conn.execute("DELETE FROM conversation_fts", [])?;
        conn.execute(
            r#"
            INSERT INTO conversation_fts (conversation_id, title, transcript_text, summary_text, topic_tags)
            SELECT
              conversation_id,
              title,
              transcript_text,
              COALESCE(json_extract(summary_json, '$.abstract_text'), ''),
              COALESCE(topic_tags_json, '[]')
            FROM conversations
            "#,
            [],
        )?;
        Ok(())
    }

    pub async fn search(&self, options: SearchOptions) -> anyhow::Result<Vec<SearchResult>> {
        let mode = options.mode.unwrap_or_else(|| {
            if options.query.is_some() {
                crate::models::SearchMode::Hybrid
            } else {
                crate::models::SearchMode::Metadata
            }
        });
        let query_embedding = match mode {
            crate::models::SearchMode::Semantic | crate::models::SearchMode::Hybrid => {
                let query = options
                    .query
                    .as_deref()
                    .ok_or_else(|| anyhow!("semantic and hybrid search require a query"))?;
                let openai = self.openai.as_ref().ok_or_else(|| {
                    anyhow!("Local embedding client is required for semantic and hybrid search")
                })?;
                Some(openai.embed_query(query).await?)
            }
            _ => None,
        };
        let conn = open_database(&self.managed_db_path())?;
        search::search(&conn, options, query_embedding)
    }

    pub fn get_conversation(
        &self,
        conversation_id: &str,
        include_raw: bool,
    ) -> anyhow::Result<Option<ConversationDetail>> {
        let conn = open_database(&self.managed_db_path())?;
        let conversation = load_conversation(&conn, conversation_id)?;
        let Some(conversation) = conversation else {
            return Ok(None);
        };
        let summary_json = conn
            .query_row(
                "SELECT summary_json FROM conversations WHERE conversation_id = ?1",
                params![conversation_id],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten()
            .map(|text| serde_json::from_str(&text))
            .transpose()?;

        let messages = load_messages(&conn, conversation_id)?;
        let attachments = load_attachments(&conn, conversation_id)?;
        let raw_json = if include_raw {
            let raw_blob: Vec<u8> = conn.query_row(
                "SELECT raw_conversation_zstd FROM conversations WHERE conversation_id = ?1",
                params![conversation_id],
                |row| row.get(0),
            )?;
            let decoded = decode_all(raw_blob.as_slice())?;
            Some(serde_json::from_slice(&decoded)?)
        } else {
            None
        };
        Ok(Some(ConversationDetail {
            conversation,
            summary_json,
            messages,
            attachments,
            raw_json,
        }))
    }

    pub fn related_conversations(
        &self,
        conversation_id: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<SearchResult>> {
        let conn = open_database(&self.managed_db_path())?;
        search::related_conversations(&conn, conversation_id, limit)
    }

    fn upsert_archive(
        &self,
        conn: &Connection,
        managed: &ManagedArchive,
        source_archive: &Path,
        mode: ImportMode,
    ) -> anyhow::Result<i64> {
        conn.execute(
            r#"
            INSERT INTO archives (archive_path, source_path, sha256_hex, size_bytes, import_mode)
            VALUES (?1, ?2, ?3, ?4, ?5)
            "#,
            params![
                managed.path.display().to_string(),
                source_archive.display().to_string(),
                managed.sha256_hex,
                managed.size_bytes as i64,
                mode.as_str()
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub(crate) fn insert_run(
        &self,
        conn: &Connection,
        kind: &str,
        archive_path: Option<&Path>,
    ) -> anyhow::Result<i64> {
        conn.execute(
            "INSERT INTO runs (run_kind, archive_path, status) VALUES (?1, ?2, 'running')",
            params![kind, archive_path.map(|path| path.display().to_string())],
        )?;
        Ok(conn.last_insert_rowid())
    }

    #[allow(clippy::too_many_arguments)]
    fn ingest_json_member(
        &self,
        conn: &Connection,
        archive_id: i64,
        nested_archive_path: &Path,
        outer_member: &str,
        json_member: &str,
        asset_index: &HashMap<String, Vec<String>>,
        report: &mut ImportReport,
    ) -> anyhow::Result<()> {
        let file = File::open(nested_archive_path)?;
        let mut archive = zip::ZipArchive::new(file)?;
        let member = archive.by_name(json_member)?;
        let mut processor = |conversation: Value| -> anyhow::Result<()> {
            let prepared = PreparedConversation::from_value(
                archive_id,
                outer_member,
                json_member,
                conversation,
                asset_index,
            )?;
            write_conversation(conn, &prepared)?;
            report.conversations_indexed += 1;
            report.messages_indexed += prepared.messages.len();
            report.attachments_indexed += prepared.attachments.len();
            Ok(())
        };
        stream_json_array(member, &mut processor)?;
        Ok(())
    }
}

#[derive(Debug, Clone)]
struct PreparedConversation {
    conversation_id: String,
    archive_id: i64,
    archive_member: String,
    source_member: String,
    title: String,
    create_time: Option<f64>,
    update_time: Option<f64>,
    default_model_slug: Option<String>,
    message_count: i64,
    user_message_count: i64,
    assistant_message_count: i64,
    transcript_text: String,
    transcript_digest: String,
    raw_conversation_zstd: Vec<u8>,
    raw_json_sha256_hex: String,
    source: String,
    source_instance: Option<String>,
    source_conversation_id: String,
    source_url: Option<String>,
    source_path: Option<String>,
    parent_conversation_id: Option<String>,
    messages: Vec<PreparedMessage>,
    attachments: Vec<AttachmentRecord>,
}

#[derive(Debug, Clone)]
struct PreparedMessage {
    message_id: String,
    role: String,
    create_time: Option<f64>,
    turn_index: i64,
    normalized_text: String,
    raw_message_json: Value,
}

impl PreparedConversation {
    fn from_value(
        archive_id: i64,
        outer_member: &str,
        json_member: &str,
        conversation: Value,
        asset_index: &HashMap<String, Vec<String>>,
    ) -> anyhow::Result<Self> {
        let conversation_id = value_string(&conversation, "conversation_id")
            .or_else(|| value_string(&conversation, "id"))
            .ok_or_else(|| anyhow!("conversation missing conversation_id"))?;
        let title = value_string(&conversation, "title")
            .unwrap_or_else(|| "(untitled conversation)".to_string());
        let create_time = conversation.get("create_time").and_then(Value::as_f64);
        let update_time = conversation.get("update_time").and_then(Value::as_f64);
        let default_model_slug = value_string(&conversation, "default_model_slug");
        let raw_bytes = serde_json::to_vec(&conversation)?;
        let raw_json_sha256_hex = hex::encode(Sha256::digest(&raw_bytes));
        let raw_conversation_zstd = encode_all(raw_bytes.as_slice(), 9)?;

        let mut extracted_messages = Vec::new();
        let mut all_asset_tokens = BTreeSet::new();
        if let Some(mapping) = conversation.get("mapping").and_then(Value::as_object) {
            for node in mapping.values() {
                if let Some(message) = node.get("message")
                    && let Some(extracted) = extract_message(message)?
                {
                    for token in &extracted.asset_tokens {
                        all_asset_tokens.insert(token.clone());
                    }
                    extracted_messages.push(extracted);
                }
            }
        }

        extracted_messages.sort_by(|left, right| {
            left.create_time
                .partial_cmp(&right.create_time)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| left.message_id.cmp(&right.message_id))
        });

        let mut messages = Vec::new();
        let mut transcript_lines = Vec::new();
        let mut user_message_count = 0_i64;
        let mut assistant_message_count = 0_i64;
        for (turn_index, extracted) in extracted_messages.into_iter().enumerate() {
            if extracted.role == "user" {
                user_message_count += 1;
            }
            if extracted.role == "assistant" {
                assistant_message_count += 1;
            }
            if !extracted.normalized_text.is_empty() {
                transcript_lines.push(format!("{}: {}", extracted.role, extracted.normalized_text));
            }
            messages.push(PreparedMessage {
                message_id: extracted.message_id,
                role: extracted.role,
                create_time: extracted.create_time,
                turn_index: turn_index as i64,
                normalized_text: extracted.normalized_text,
                raw_message_json: extracted.raw_message_json,
            });
        }
        let transcript_text = transcript_lines.join("\n\n");
        let transcript_digest = transcript_text.chars().take(5000).collect::<String>();

        let mut attachments = Vec::new();
        for token in all_asset_tokens {
            if let Some(paths) = asset_index.get(&token) {
                for path in paths {
                    attachments.push(AttachmentRecord {
                        attachment_id: format!("{conversation_id}:{token}"),
                        conversation_id: conversation_id.clone(),
                        archive_path: path.clone(),
                        extension: Path::new(path)
                            .extension()
                            .map(|extension| extension.to_string_lossy().to_string()),
                        size_bytes: None,
                        source_ref: token.clone(),
                        linkage_json: json!({ "file_token": token, "archive_path": path }),
                    });
                }
            }
        }
        attachments.sort_by(|left, right| left.archive_path.cmp(&right.archive_path));
        attachments.dedup_by(|left, right| {
            left.archive_path == right.archive_path && left.source_ref == right.source_ref
        });

        let source_conversation_id = conversation_id.clone();
        Ok(Self {
            conversation_id,
            archive_id,
            archive_member: outer_member.to_string(),
            source_member: json_member.to_string(),
            title,
            create_time,
            update_time,
            default_model_slug,
            message_count: messages.len() as i64,
            user_message_count,
            assistant_message_count,
            transcript_text,
            transcript_digest,
            raw_conversation_zstd,
            raw_json_sha256_hex,
            source: "chatgpt".to_string(),
            source_instance: None,
            source_conversation_id,
            source_url: None,
            source_path: None,
            parent_conversation_id: None,
            messages,
            attachments,
        })
    }

    fn from_normalized(
        archive_id: i64,
        conversation: NormalizedConversation,
    ) -> anyhow::Result<Self> {
        let conversation_id = conversation.canonical_id();
        let parent_conversation_id = if conversation.source == "codex"
            && conversation.model.as_deref() == Some("codex-auto-review")
        {
            conversation
                .messages
                .first()
                .and_then(|message| crate::codex::review_parent_conversation_id(&message.text))
        } else {
            None
        };
        let raw_bytes = serde_json::to_vec(&conversation.raw)?;
        let raw_json_sha256_hex = hex::encode(Sha256::digest(&raw_bytes));
        let raw_conversation_zstd = encode_all(raw_bytes.as_slice(), 9)?;
        let mut messages = Vec::with_capacity(conversation.messages.len());
        let mut transcript_lines = Vec::new();
        let mut user_message_count = 0_i64;
        let mut assistant_message_count = 0_i64;
        for (turn_index, message) in conversation.messages.into_iter().enumerate() {
            if message.role == "user" {
                user_message_count += 1;
            } else if message.role == "assistant" {
                assistant_message_count += 1;
            }
            if !message.text.trim().is_empty() {
                transcript_lines.push(format!("{}: {}", message.role, message.text));
            }
            messages.push(PreparedMessage {
                message_id: message.message_id,
                role: message.role,
                create_time: message.create_time,
                turn_index: turn_index as i64,
                normalized_text: message.text,
                raw_message_json: message.raw,
            });
        }
        let transcript_text = transcript_lines.join("\n\n");
        let transcript_digest = transcript_text.chars().take(5000).collect::<String>();
        Ok(Self {
            conversation_id,
            archive_id,
            archive_member: conversation.source.clone(),
            source_member: conversation.source_path.clone().unwrap_or_default(),
            title: conversation.title,
            create_time: conversation.create_time,
            update_time: conversation.update_time,
            default_model_slug: conversation.model,
            message_count: messages.len() as i64,
            user_message_count,
            assistant_message_count,
            transcript_text,
            transcript_digest,
            raw_conversation_zstd,
            raw_json_sha256_hex,
            source: conversation.source,
            source_instance: conversation.source_instance,
            source_conversation_id: conversation.source_conversation_id,
            source_url: conversation.source_url,
            source_path: conversation.source_path,
            parent_conversation_id,
            messages,
            attachments: Vec::new(),
        })
    }
}

#[derive(Debug)]
struct ExtractedMessage {
    message_id: String,
    role: String,
    create_time: Option<f64>,
    normalized_text: String,
    asset_tokens: BTreeSet<String>,
    raw_message_json: Value,
}

fn extract_message(message: &Value) -> anyhow::Result<Option<ExtractedMessage>> {
    let message_id = value_string(message, "id").unwrap_or_else(|| "unknown-message".to_string());
    let role = message
        .get("author")
        .and_then(|author| author.get("role"))
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string();
    let create_time = message.get("create_time").and_then(Value::as_f64);
    let content = message.get("content");
    let normalized_text = content.map(extract_content_text).unwrap_or_default();
    let mut asset_tokens = BTreeSet::new();
    if let Some(content) = content {
        collect_asset_tokens(content, &mut asset_tokens);
    }

    if normalized_text.is_empty() && asset_tokens.is_empty() {
        return Ok(None);
    }

    Ok(Some(ExtractedMessage {
        message_id,
        role,
        create_time,
        normalized_text,
        asset_tokens,
        raw_message_json: message.clone(),
    }))
}

fn extract_content_text(content: &Value) -> String {
    let mut parts = Vec::new();
    let Some(kind) = content.get("content_type").and_then(Value::as_str) else {
        collect_text_fields(content, &mut parts);
        return dedupe_join(parts);
    };
    match kind {
        "text" => {
            if let Some(items) = content.get("parts").and_then(Value::as_array) {
                for item in items {
                    if let Some(text) = item.as_str() {
                        parts.push(text.to_string());
                    } else {
                        collect_text_fields(item, &mut parts);
                    }
                }
            }
        }
        "code" => {
            if let Some(text) = content.get("text").and_then(Value::as_str) {
                parts.push(text.to_string());
            }
        }
        "tether_quote" => {
            for key in ["title", "text", "url", "domain"] {
                if let Some(text) = content.get(key).and_then(Value::as_str) {
                    parts.push(text.to_string());
                }
            }
        }
        "tether_browsing_display" => {
            if let Some(text) = content.get("result").and_then(Value::as_str) {
                parts.push(text.to_string());
            }
        }
        "multimodal_text" => {
            if let Some(items) = content.get("parts").and_then(Value::as_array) {
                for item in items {
                    collect_text_fields(item, &mut parts);
                }
            }
        }
        "system_error" => collect_text_fields(content, &mut parts),
        _ => collect_text_fields(content, &mut parts),
    }
    dedupe_join(parts)
}

fn dedupe_join(parts: Vec<String>) -> String {
    let mut seen = BTreeSet::new();
    let mut ordered = Vec::new();
    for part in parts {
        let trimmed = part.trim();
        if trimmed.is_empty() {
            continue;
        }
        if seen.insert(trimmed.to_string()) {
            ordered.push(trimmed.to_string());
        }
    }
    ordered.join("\n")
}

fn collect_text_fields(value: &Value, parts: &mut Vec<String>) {
    match value {
        Value::String(text) => parts.push(text.clone()),
        Value::Array(items) => {
            for item in items {
                collect_text_fields(item, parts);
            }
        }
        Value::Object(map) => {
            for key in ["text", "title", "result", "url", "domain", "query"] {
                if let Some(text) = map.get(key).and_then(Value::as_str) {
                    parts.push(text.to_string());
                }
            }
            if let Some(items) = map.get("parts").and_then(Value::as_array) {
                for item in items {
                    collect_text_fields(item, parts);
                }
            }
        }
        _ => {}
    }
}

fn collect_asset_tokens(value: &Value, tokens: &mut BTreeSet<String>) {
    match value {
        Value::String(text) => {
            for token in extract_file_tokens(text) {
                tokens.insert(token);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_asset_tokens(item, tokens);
            }
        }
        Value::Object(map) => {
            for value in map.values() {
                collect_asset_tokens(value, tokens);
            }
        }
        _ => {}
    }
}

fn write_conversation(conn: &Connection, prepared: &PreparedConversation) -> anyhow::Result<()> {
    let tx = conn.unchecked_transaction()?;
    let existing: Option<(String, String, Option<String>)> = tx
        .query_row(
            "SELECT title, transcript_text, parent_conversation_id FROM conversations WHERE conversation_id = ?1",
            params![prepared.conversation_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let content_changed = existing
        .as_ref()
        .map(|(title, transcript, parent)| {
            title != &prepared.title
                || transcript != &prepared.transcript_text
                || parent != &prepared.parent_conversation_id
        })
        .unwrap_or(true);
    tx.execute(
        "DELETE FROM messages WHERE conversation_id = ?1",
        params![prepared.conversation_id],
    )?;
    tx.execute(
        "DELETE FROM attachments WHERE conversation_id = ?1",
        params![prepared.conversation_id],
    )?;
    tx.execute(
        r#"
        INSERT INTO conversations (
          conversation_id, archive_id, archive_member, source_member, title, create_time, update_time,
          default_model_slug, message_count, user_message_count, assistant_message_count, transcript_text,
          transcript_digest, raw_conversation_zstd, raw_json_sha256_hex,
          source, source_instance, source_conversation_id, source_url, source_path,
          parent_conversation_id, ingested_at
        )
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15,
                ?16, ?17, ?18, ?19, ?20, ?21, CURRENT_TIMESTAMP)
        ON CONFLICT(conversation_id) DO UPDATE SET
          archive_id = excluded.archive_id,
          archive_member = excluded.archive_member,
          source_member = excluded.source_member,
          title = excluded.title,
          create_time = excluded.create_time,
          update_time = excluded.update_time,
          default_model_slug = excluded.default_model_slug,
          message_count = excluded.message_count,
          user_message_count = excluded.user_message_count,
          assistant_message_count = excluded.assistant_message_count,
          transcript_text = excluded.transcript_text,
          transcript_digest = excluded.transcript_digest,
          raw_conversation_zstd = excluded.raw_conversation_zstd,
          raw_json_sha256_hex = excluded.raw_json_sha256_hex,
          source = excluded.source,
          source_instance = excluded.source_instance,
          source_conversation_id = excluded.source_conversation_id,
          source_url = excluded.source_url,
          source_path = excluded.source_path,
          parent_conversation_id = excluded.parent_conversation_id,
          ingested_at = CURRENT_TIMESTAMP
        "#,
        params![
            prepared.conversation_id,
            prepared.archive_id,
            prepared.archive_member,
            prepared.source_member,
            prepared.title,
            prepared.create_time,
            prepared.update_time,
            prepared.default_model_slug,
            prepared.message_count,
            prepared.user_message_count,
            prepared.assistant_message_count,
            prepared.transcript_text,
            prepared.transcript_digest,
            prepared.raw_conversation_zstd,
            prepared.raw_json_sha256_hex,
            prepared.source,
            prepared.source_instance,
            prepared.source_conversation_id,
            prepared.source_url,
            prepared.source_path,
            prepared.parent_conversation_id,
        ],
    )?;

    if content_changed && existing.is_some() {
        tx.execute(
            r#"
            UPDATE conversations
            SET summary_json = NULL, summary_model = NULL, summary_completed_at = NULL,
                embedding_blob = NULL, embedding_dimensions = NULL, embedding_model = NULL,
                embedding_completed_at = NULL, risk_flags_json = '[]', topic_tags_json = '[]',
                redaction_notes_json = '[]'
            WHERE conversation_id = ?1
            "#,
            params![prepared.conversation_id],
        )?;
        tx.execute(
            "DELETE FROM conversation_embedding_chunks WHERE conversation_id = ?1",
            params![prepared.conversation_id],
        )?;
    }

    if content_changed {
        let mut parents = BTreeSet::new();
        if let Some(parent) = existing.as_ref().and_then(|(_, _, parent)| parent.clone()) {
            parents.insert(parent);
        }
        if let Some(parent) = prepared.parent_conversation_id.clone() {
            parents.insert(parent);
        }
        for parent in parents {
            tx.execute(
                r#"
                UPDATE conversations
                SET embedding_blob = NULL,
                    embedding_dimensions = NULL,
                    embedding_model = NULL,
                    embedding_completed_at = NULL
                WHERE conversation_id = ?1
                "#,
                params![parent],
            )?;
            tx.execute(
                "DELETE FROM conversation_embedding_chunks WHERE conversation_id = ?1",
                params![parent],
            )?;
            upsert_job(
                &tx,
                &parent,
                JobKind::Embedding,
                JobStatus::Pending,
                None,
                0,
            )?;
        }
    }

    for message in &prepared.messages {
        tx.execute(
            r#"
            INSERT INTO messages (conversation_id, message_id, role, create_time, turn_index, normalized_text, raw_message_json)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
            "#,
            params![
                prepared.conversation_id,
                message.message_id,
                message.role,
                message.create_time,
                message.turn_index,
                message.normalized_text,
                serde_json::to_string(&message.raw_message_json)?,
            ],
        )?;
    }
    for attachment in &prepared.attachments {
        tx.execute(
            r#"
            INSERT INTO attachments (conversation_id, attachment_id, archive_path, extension, size_bytes, source_ref, linkage_json)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
            "#,
            params![
                prepared.conversation_id,
                attachment.attachment_id,
                attachment.archive_path,
                attachment.extension,
                attachment.size_bytes,
                attachment.source_ref,
                serde_json::to_string(&attachment.linkage_json)?,
            ],
        )?;
    }
    if content_changed {
        upsert_job(
            &tx,
            &prepared.conversation_id,
            JobKind::Summary,
            JobStatus::Pending,
            None,
            0,
        )?;
        upsert_job(
            &tx,
            &prepared.conversation_id,
            JobKind::Embedding,
            JobStatus::Pending,
            None,
            0,
        )?;
    }
    tx.commit()?;
    Ok(())
}

pub fn encode_embedding(values: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(values.len() * 4);
    for value in values {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    bytes
}

pub fn decode_embedding(bytes: &[u8]) -> Vec<f32> {
    let (chunks, _) = bytes.as_chunks::<4>();
    chunks
        .iter()
        .map(|chunk| f32::from_le_bytes(*chunk))
        .collect()
}

fn pool_embedding_chunks(chunks: &[EmbeddingVector]) -> anyhow::Result<EmbeddingVector> {
    let first = chunks
        .first()
        .ok_or_else(|| anyhow!("passage embedding returned no chunks"))?;
    let dimensions = first.values.len();
    anyhow::ensure!(dimensions > 0, "passage embedding chunk is empty");
    anyhow::ensure!(
        chunks
            .iter()
            .all(|chunk| { chunk.values.len() == dimensions && chunk.model_id == first.model_id }),
        "passage embedding chunks have inconsistent model or dimensions"
    );
    let mut values = vec![0.0_f32; dimensions];
    for chunk in chunks {
        for (target, value) in values.iter_mut().zip(&chunk.values) {
            *target += *value;
        }
    }
    let count = chunks.len() as f32;
    for value in &mut values {
        *value /= count;
    }
    let norm = values.iter().map(|value| value * value).sum::<f32>().sqrt();
    anyhow::ensure!(
        norm.is_finite() && norm > 0.0,
        "pooled embedding is zero or non-finite"
    );
    for value in &mut values {
        *value /= norm;
    }
    Ok(EmbeddingVector {
        values,
        model_id: first.model_id.clone(),
    })
}

fn value_string(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(ToString::to_string)
}

fn load_transcript(conn: &Connection, conversation_id: &str) -> anyhow::Result<String> {
    let text: String = conn.query_row(
        "SELECT transcript_text FROM conversations WHERE conversation_id = ?1",
        params![conversation_id],
        |row| row.get(0),
    )?;
    Ok(text)
}

fn load_embedding_input(conn: &Connection, conversation_id: &str) -> anyhow::Result<String> {
    let row: (String, String, Option<String>) = conn.query_row(
        r#"
        SELECT source, title, summary_json
        FROM conversations
        WHERE conversation_id = ?1
        "#,
        params![conversation_id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    let summary = row
        .2
        .as_deref()
        .map(serde_json::from_str::<SummaryRecord>)
        .transpose()?;
    let transcript = if row.0 == "codex" {
        let projected = load_codex_semantic_transcript(conn, conversation_id)?;
        if projected.trim().is_empty() {
            load_transcript(conn, conversation_id)?
        } else {
            projected
        }
    } else {
        load_transcript(conn, conversation_id)?
    };
    let mut parts = Vec::new();
    if !is_codex_boilerplate_title(&row.1) {
        parts.push(format!("Title: {}", row.1));
    }
    if let Some(summary) = summary {
        parts.push(format!("Abstract: {}", summary.abstract_text));
        if !summary.key_points.is_empty() {
            parts.push(format!("Key points: {}", summary.key_points.join("; ")));
        }
        if !summary.candidate_topics.is_empty() {
            parts.push(format!("Topics: {}", summary.candidate_topics.join(", ")));
        }
    }
    parts.push(format!("Transcript:\n{transcript}"));
    Ok(parts.join("\n"))
}

fn load_codex_semantic_transcript(
    conn: &Connection,
    conversation_id: &str,
) -> anyhow::Result<String> {
    let mut stmt = conn.prepare(
        r#"
        SELECT role, normalized_text
        FROM messages
        WHERE conversation_id = ?1
        ORDER BY turn_index
        "#,
    )?;
    let rows = stmt.query_map(params![conversation_id], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut projected = Vec::new();
    for row in rows {
        let (role, text) = row?;
        if is_codex_review_decision(&role, &text) {
            continue;
        }
        let cleaned = clean_codex_semantic_text(&text);
        if !cleaned.trim().is_empty() {
            projected.push(format!("{role}: {cleaned}"));
        }
    }
    Ok(projected.join("\n\n"))
}

fn is_codex_review_decision(role: &str, text: &str) -> bool {
    if role != "assistant" {
        return false;
    }
    let trimmed = text.trim();
    trimmed.starts_with('{')
        && trimmed.ends_with('}')
        && (trimmed.contains("\"risk_level\"") || trimmed.contains("\"outcome\""))
        && (trimmed.contains("\"allow\"")
            || trimmed.contains("\"deny\"")
            || trimmed.contains("\"rationale\""))
}

fn is_codex_boilerplate_title(title: &str) -> bool {
    let title = title.trim_start();
    title.starts_with("<environment_context")
        || title.starts_with("<recommended_plugins")
        || title.starts_with("<codex_internal_context")
        || title.starts_with("# AGENTS.md instructions for")
        || title.starts_with("The following is the Codex agent history")
}

fn clean_codex_semantic_text(text: &str) -> String {
    let mut cleaned = extract_codex_agent_history(text).unwrap_or_else(|| text.to_string());
    cleaned = extract_codex_objective(&cleaned).unwrap_or(cleaned);
    cleaned = remove_xml_blocks(&cleaned, "environment_context");
    cleaned = remove_xml_blocks(&cleaned, "recommended_plugins");
    cleaned = remove_xml_blocks(&cleaned, "codex_internal_context");
    cleaned.trim().to_string()
}

fn extract_codex_agent_history(text: &str) -> Option<String> {
    let prefix_window = text.chars().take(600).collect::<String>();
    if !prefix_window.contains("The following is the Codex agent history") {
        return None;
    }
    let markers = [">>> TRANSCRIPT DELTA START", ">>> TRANSCRIPT START"];
    let (start, start_marker) = markers
        .iter()
        .filter_map(|marker| text.find(marker).map(|index| (index, *marker)))
        .min_by_key(|(index, _)| *index)?;
    let content_start = start + start_marker.len();
    let tail = &text[content_start..];
    let end_markers = [">>> TRANSCRIPT DELTA END", ">>> TRANSCRIPT END"];
    let content_end = end_markers
        .iter()
        .filter_map(|marker| tail.find(marker))
        .min()
        .unwrap_or(tail.len());
    Some(tail[..content_end].trim().to_string())
}

fn extract_codex_objective(text: &str) -> Option<String> {
    if !text.contains("<codex_internal_context") {
        return None;
    }
    let start = text.find("<objective>")? + "<objective>".len();
    let tail = &text[start..];
    let end = tail.find("</objective>")?;
    let objective = tail[..end].trim();
    if objective.is_empty() {
        None
    } else {
        Some(objective.to_string())
    }
}

fn remove_xml_blocks(text: &str, tag: &str) -> String {
    let opening = format!("<{tag}");
    let closing = format!("</{tag}>");
    let mut output = text.to_string();
    loop {
        let Some(start) = output.find(&opening) else {
            break;
        };
        let Some(relative_end) = output[start..].find(&closing) else {
            if output[..start].trim().is_empty() {
                return String::new();
            }
            output.truncate(start);
            break;
        };
        let end = start + relative_end + closing.len();
        output.replace_range(start..end, " ");
    }
    output
}

fn collect_summary_targets(
    conn: &Connection,
    force: bool,
    conversation_ids: Option<Vec<String>>,
    limit: Option<usize>,
) -> anyhow::Result<Vec<String>> {
    collect_targets(conn, force, conversation_ids, limit, JobKind::Summary)
}

fn collect_embedding_targets(
    conn: &Connection,
    force: bool,
    conversation_ids: Option<Vec<String>>,
    limit: Option<usize>,
) -> anyhow::Result<Vec<String>> {
    collect_targets(conn, force, conversation_ids, limit, JobKind::Embedding)
}

fn collect_targets(
    conn: &Connection,
    force: bool,
    conversation_ids: Option<Vec<String>>,
    limit: Option<usize>,
    kind: JobKind,
) -> anyhow::Result<Vec<String>> {
    let mut ids = Vec::new();
    let sql = match (force, kind) {
        (true, JobKind::Summary) => {
            "SELECT c.conversation_id
             FROM conversations c
             LEFT JOIN jobs j ON j.conversation_id = c.conversation_id AND j.kind = 'summary'
             ORDER BY COALESCE(j.attempts, 0) ASC, LENGTH(c.transcript_text) ASC, c.create_time DESC"
        }
        (true, JobKind::Embedding) => {
            "SELECT c.conversation_id
             FROM conversations c
             LEFT JOIN jobs j ON j.conversation_id = c.conversation_id AND j.kind = 'embedding'
             ORDER BY COALESCE(j.attempts, 0) ASC, c.create_time DESC"
        }
        (false, JobKind::Summary) => {
            "SELECT c.conversation_id
             FROM conversations c
             LEFT JOIN jobs j ON j.conversation_id = c.conversation_id AND j.kind = 'summary'
             WHERE c.summary_json IS NULL
             ORDER BY COALESCE(j.attempts, 0) ASC, LENGTH(c.transcript_text) ASC, c.create_time DESC"
        }
        (false, JobKind::Embedding) => {
            "SELECT c.conversation_id
             FROM conversations c
             LEFT JOIN jobs j ON j.conversation_id = c.conversation_id AND j.kind = 'embedding'
             WHERE c.embedding_blob IS NULL
             ORDER BY COALESCE(j.attempts, 0) ASC, c.create_time DESC"
        }
    };
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
    for row in rows {
        ids.push(row?);
    }
    if let Some(filter_ids) = conversation_ids {
        let allowed: BTreeSet<_> = filter_ids.into_iter().collect();
        ids.retain(|id| allowed.contains(id));
    }
    if let Some(limit) = limit {
        ids.truncate(limit);
    }
    Ok(ids)
}

fn load_conversation(
    conn: &Connection,
    conversation_id: &str,
) -> anyhow::Result<Option<ConversationRecord>> {
    conn.query_row(
        r#"
        SELECT conversation_id, title, create_time, update_time, default_model_slug, summary_json,
               risk_flags_json, topic_tags_json, review_status, publish_candidate, site_category,
               era_bucket, message_count, user_message_count, assistant_message_count,
               source, source_instance, source_conversation_id, source_url, source_path
        FROM conversations
        WHERE conversation_id = ?1
        "#,
        params![conversation_id],
        |row| {
            let summary_json: Option<String> = row.get(5)?;
            let risk_flags_json: String = row.get(6)?;
            let topic_tags_json: String = row.get(7)?;
            Ok(ConversationRecord {
                conversation_id: row.get(0)?,
                source: row.get(15)?,
                source_instance: row.get(16)?,
                source_conversation_id: row.get(17)?,
                source_url: row.get(18)?,
                source_path: row.get(19)?,
                title: row.get(1)?,
                create_time: row.get(2)?,
                update_time: row.get(3)?,
                default_model_slug: row.get(4)?,
                summary: summary_json
                    .as_deref()
                    .map(serde_json::from_str)
                    .transpose()
                    .map_err(|error| {
                        rusqlite::Error::FromSqlConversionFailure(
                            5,
                            rusqlite::types::Type::Text,
                            Box::new(error),
                        )
                    })?,
                risk_flags: serde_json::from_str(&risk_flags_json).unwrap_or_default(),
                topic_tags: serde_json::from_str(&topic_tags_json).unwrap_or_default(),
                review_status: row.get(8)?,
                publish_candidate: row.get::<_, i64>(9)? != 0,
                site_category: row.get(10)?,
                era_bucket: row.get(11)?,
                message_count: row.get(12)?,
                user_message_count: row.get(13)?,
                assistant_message_count: row.get(14)?,
            })
        },
    )
    .optional()
    .map_err(Into::into)
}

fn load_messages(
    conn: &Connection,
    conversation_id: &str,
) -> anyhow::Result<Vec<crate::models::ConversationMessage>> {
    let mut stmt = conn.prepare(
        r#"
        SELECT message_id, role, create_time, turn_index, normalized_text, raw_message_json
        FROM messages
        WHERE conversation_id = ?1
        ORDER BY turn_index ASC
        "#,
    )?;
    let rows = stmt.query_map(params![conversation_id], |row| {
        let raw_message_json: String = row.get(5)?;
        Ok(crate::models::ConversationMessage {
            message_id: row.get(0)?,
            conversation_id: conversation_id.to_string(),
            role: row.get(1)?,
            create_time: row.get(2)?,
            turn_index: row.get(3)?,
            normalized_text: row.get(4)?,
            raw_message_json: serde_json::from_str(&raw_message_json).unwrap_or(Value::Null),
        })
    })?;
    let mut items = Vec::new();
    for row in rows {
        items.push(row?);
    }
    Ok(items)
}

fn load_attachments(
    conn: &Connection,
    conversation_id: &str,
) -> anyhow::Result<Vec<AttachmentRecord>> {
    let mut stmt = conn.prepare(
        r#"
        SELECT attachment_id, archive_path, extension, size_bytes, source_ref, linkage_json
        FROM attachments
        WHERE conversation_id = ?1
        ORDER BY archive_path ASC
        "#,
    )?;
    let rows = stmt.query_map(params![conversation_id], |row| {
        let linkage_json: String = row.get(5)?;
        Ok(AttachmentRecord {
            attachment_id: row.get(0)?,
            conversation_id: conversation_id.to_string(),
            archive_path: row.get(1)?,
            extension: row.get(2)?,
            size_bytes: row.get(3)?,
            source_ref: row.get(4)?,
            linkage_json: serde_json::from_str(&linkage_json).unwrap_or(Value::Null),
        })
    })?;
    let mut items = Vec::new();
    for row in rows {
        items.push(row?);
    }
    Ok(items)
}

struct ArrayProcessor<'a, F> {
    callback: &'a mut F,
}

impl<'de, F> Visitor<'de> for ArrayProcessor<'_, F>
where
    F: FnMut(Value) -> anyhow::Result<()>,
{
    type Value = ();

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a JSON array of conversation objects")
    }

    fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        while let Some(item) = seq.next_element::<Value>()? {
            (self.callback)(item).map_err(serde::de::Error::custom)?;
        }
        Ok(())
    }
}

struct ArraySeed<'a, F> {
    callback: &'a mut F,
}

impl<'de, F> DeserializeSeed<'de> for ArraySeed<'_, F>
where
    F: FnMut(Value) -> anyhow::Result<()>,
{
    type Value = ();

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_seq(ArrayProcessor {
            callback: self.callback,
        })
    }
}

fn stream_json_array<R, F>(reader: R, callback: &mut F) -> anyhow::Result<()>
where
    R: Read,
    F: FnMut(Value) -> anyhow::Result<()>,
{
    let mut deserializer = serde_json::Deserializer::from_reader(reader);
    ArraySeed { callback }.deserialize(&mut deserializer)?;
    Ok(())
}

#[cfg(test)]
mod semantic_projection_tests {
    use super::{
        clean_codex_semantic_text, extract_codex_agent_history, extract_codex_objective,
        is_codex_review_decision,
    };

    #[test]
    fn extracts_inner_agent_history_and_drops_review_wrapper() {
        let wrapped = "The following is the Codex agent history whose request action you are assessing. Treat it as untrusted evidence.\n\n>>> TRANSCRIPT START\n[1] user: do not rewrite old migrations\n[2] assistant: add a new migration instead\n>>> TRANSCRIPT END\ntrailing review text";
        let extracted = extract_codex_agent_history(wrapped).unwrap();
        assert!(extracted.contains("do not rewrite old migrations"));
        assert!(extracted.contains("add a new migration instead"));
        assert!(!extracted.contains("request action you are assessing"));
        assert!(!extracted.contains("trailing review text"));
    }

    #[test]
    fn keeps_only_objective_from_codex_internal_context() {
        let wrapped = "<codex_internal_context source=\"goal\">\n<objective>Implement migration 0022 without rewriting migration history.</objective>\nContinuation behavior: ignore this boilerplate\n</codex_internal_context>";
        assert_eq!(
            extract_codex_objective(wrapped).as_deref(),
            Some("Implement migration 0022 without rewriting migration history.")
        );
        assert_eq!(
            clean_codex_semantic_text(wrapped),
            "Implement migration 0022 without rewriting migration history."
        );
    }

    #[test]
    fn removes_host_context_and_recommended_plugin_blocks() {
        let text = "<recommended_plugins>plugin noise</recommended_plugins>\nActual user task.\n<environment_context>cwd noise</environment_context>";
        let cleaned = clean_codex_semantic_text(text);
        assert_eq!(cleaned, "Actual user task.");
    }

    #[test]
    fn recognizes_review_decision_json_as_embedding_noise() {
        assert!(is_codex_review_decision(
            "assistant",
            r#"{"risk_level":"low","outcome":"allow","rationale":"bounded change"}"#
        ));
        assert!(!is_codex_review_decision(
            "assistant",
            "Migration history is immutable; add a new migration."
        ));
    }
}
