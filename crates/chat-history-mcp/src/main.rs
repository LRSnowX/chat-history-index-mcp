use std::{path::PathBuf, sync::Arc};

use axum::{
    Router,
    extract::{Request, State},
    http::{StatusCode, header::AUTHORIZATION},
    middleware::{self, Next},
    response::Response,
};
use chat_history_core::{
    ChatGptBlockedThread, ChatGptBridgeTranscript, ChatGptDiscoveryPlan, ChatGptSyncState,
    ChatGptThreadListSnapshot, ConversationRecord, DataHome, ImportMode, ImportOptions,
    IndexService, NormalizedConversation, SearchMode, SearchOptions, SearchResult, SummaryRecord,
};
use clap::{Parser, ValueEnum};
use rmcp::Json;
use rmcp::{
    ServerHandler, ServiceExt,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{ServerCapabilities, ServerInfo},
    tool, tool_handler, tool_router,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone)]
struct ChatHistoryMcp {
    service: IndexService,
    access: AccessMode,
    tool_router: ToolRouter<Self>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AccessMode {
    Full,
    ReadOnly,
    Collector,
}

impl ChatHistoryMcp {
    fn new(service: IndexService, access: AccessMode) -> Self {
        let mut tool_router = Self::tool_router();
        match access {
            AccessMode::Full => {}
            AccessMode::ReadOnly => {
                for name in [
                    "chatgpt_block",
                    "chatgpt_import_thread",
                    "chatgpt_plan_recent",
                    "chatgpt_seed_cursor",
                    "chatgpt_seed_from_index",
                    "index_export",
                    "import_conversations",
                    "rebuild_embeddings",
                    "rebuild_summaries",
                ] {
                    tool_router.remove_route(name);
                }
            }
            AccessMode::Collector => {
                for name in [
                    "get_conversation",
                    "index_export",
                    "index_stats",
                    "memory_get_thread",
                    "memory_project_context",
                    "memory_recent",
                    "memory_search",
                    "rebuild_embeddings",
                    "rebuild_summaries",
                    "related_conversations",
                    "search_conversations",
                ] {
                    tool_router.remove_route(name);
                }
            }
        }
        Self {
            service,
            access,
            tool_router,
        }
    }

    fn require_write(&self) -> Result<(), String> {
        if self.access == AccessMode::ReadOnly {
            Err("this MCP endpoint is read-only".to_string())
        } else {
            Ok(())
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
struct IndexExportRequest {
    zip_path: Option<String>,
    mode: Option<String>,
    run_api_jobs: Option<bool>,
    force_summaries: Option<bool>,
    force_embeddings: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct SearchRequest {
    query: Option<String>,
    mode: Option<SearchMode>,
    date_from: Option<f64>,
    date_to: Option<f64>,
    model: Option<String>,
    sources: Option<Vec<String>>,
    risk_flags: Option<Vec<String>>,
    topic_tags: Option<Vec<String>>,
    limit: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct GetConversationRequest {
    conversation_id: String,
    include_raw: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct RelatedRequest {
    conversation_id: String,
    limit: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct RebuildRequest {
    conversation_ids: Option<Vec<String>>,
    limit: Option<usize>,
    force: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ImportConversationsRequest {
    conversations: Vec<NormalizedConversation>,
    source_label: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct MemorySearchRequest {
    query: String,
    project: Option<String>,
    sources: Option<Vec<String>>,
    limit: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct MemoryRecentRequest {
    project: Option<String>,
    sources: Option<Vec<String>>,
    since: Option<f64>,
    limit: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct MemoryGetThreadRequest {
    conversation_id: String,
    message_offset: Option<usize>,
    message_limit: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct MemoryProjectContextRequest {
    project: String,
    query: Option<String>,
    sources: Option<Vec<String>>,
    relevant_limit: Option<usize>,
    recent_limit: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ChatGptPlanRecentRequest {
    snapshot: ChatGptThreadListSnapshot,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ChatGptImportThreadRequest {
    transcript: ChatGptBridgeTranscript,
    embed: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ChatGptBlockRequest {
    thread_id: String,
    reason: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ChatGptSeedCursorRequest {
    update_time: f64,
}

#[tool_router]
impl ChatHistoryMcp {
    #[tool(description = "Inspect durable ChatGPT.app incremental collector state.")]
    async fn chatgpt_state(&self) -> Result<Json<ChatGptSyncState>, String> {
        ChatGptSyncState::load(self.service.data_home())
            .map(Json)
            .map_err(|error| error.to_string())
    }

    #[tool(
        description = "Plan one recent ChatGPT.app discovery snapshot. This mutates only durable collector state; it does not import transcripts."
    )]
    async fn chatgpt_plan_recent(
        &self,
        Parameters(request): Parameters<ChatGptPlanRecentRequest>,
    ) -> Result<Json<ChatGptPlanRecentResponse>, String> {
        self.require_write()?;
        let mut state =
            ChatGptSyncState::load(self.service.data_home()).map_err(|error| error.to_string())?;
        let plan = state
            .plan_recent(request.snapshot)
            .map_err(|error| error.to_string())?;
        let state_path = state
            .save(self.service.data_home())
            .map_err(|error| error.to_string())?;
        Ok(Json(ChatGptPlanRecentResponse {
            plan,
            state_path,
            state,
        }))
    }

    #[tool(
        description = "Validate and import one fully paged ChatGPT.app transcript. Rejects incomplete, truncated, inaccessible, or cursor-broken transcripts and optionally builds its local embedding."
    )]
    async fn chatgpt_import_thread(
        &self,
        Parameters(request): Parameters<ChatGptImportThreadRequest>,
    ) -> Result<Json<ChatGptImportThreadResponse>, String> {
        self.require_write()?;
        let thread_id = request.transcript.thread_id.clone();
        let update_time = request.transcript.update_time;
        let normalized = request
            .transcript
            .into_normalized()
            .map_err(|error| error.to_string())?;
        let import = self
            .service
            .import_normalized(
                vec![normalized],
                Some(std::path::Path::new("chatgpt-app-bridge")),
            )
            .map_err(|error| error.to_string())?;
        let embeddings_completed = if request.embed.unwrap_or(true) {
            self.service
                .rebuild_embeddings(false, Some(vec![thread_id.clone()]), None)
                .await
                .map_err(|error| error.to_string())?
        } else {
            0
        };
        let mut state =
            ChatGptSyncState::load(self.service.data_home()).map_err(|error| error.to_string())?;
        state.mark_imported_at(&thread_id, update_time);
        let state_path = state
            .save(self.service.data_home())
            .map_err(|error| error.to_string())?;
        Ok(Json(ChatGptImportThreadResponse {
            thread_id,
            import,
            embeddings_completed,
            state_path,
            state,
        }))
    }

    #[tool(
        description = "Mark one ChatGPT.app thread blocked/incomplete without advancing the safe collector cursor."
    )]
    async fn chatgpt_block(
        &self,
        Parameters(request): Parameters<ChatGptBlockRequest>,
    ) -> Result<Json<ChatGptBlockResponse>, String> {
        self.require_write()?;
        let mut state =
            ChatGptSyncState::load(self.service.data_home()).map_err(|error| error.to_string())?;
        let blocked = state.mark_blocked(&request.thread_id, request.reason);
        let state_path = state
            .save(self.service.data_home())
            .map_err(|error| error.to_string())?;
        Ok(Json(ChatGptBlockResponse {
            blocked,
            state_path,
            state,
        }))
    }

    #[tool(
        description = "Seed the ChatGPT.app collector cursor after a trusted complete bootstrap or backfill."
    )]
    async fn chatgpt_seed_cursor(
        &self,
        Parameters(request): Parameters<ChatGptSeedCursorRequest>,
    ) -> Result<Json<ChatGptSeedResponse>, String> {
        self.require_write()?;
        let mut state =
            ChatGptSyncState::load(self.service.data_home()).map_err(|error| error.to_string())?;
        state
            .seed_cursor(request.update_time)
            .map_err(|error| error.to_string())?;
        let state_path = state
            .save(self.service.data_home())
            .map_err(|error| error.to_string())?;
        Ok(Json(ChatGptSeedResponse { state_path, state }))
    }

    #[tool(
        description = "Seed the ChatGPT.app collector cursor from the newest already indexed ChatGPT conversation."
    )]
    async fn chatgpt_seed_from_index(&self) -> Result<Json<ChatGptSeedFromIndexResponse>, String> {
        self.require_write()?;
        let health = chat_history_core::db::inspect_database(&self.service.managed_db_path())
            .map_err(|error| error.to_string())?;
        let newest = health
            .sources
            .get("chatgpt")
            .and_then(|source| source.newest_update_time)
            .ok_or_else(|| {
                "no indexed ChatGPT conversations are available to seed the cursor".to_string()
            })?;
        let mut state =
            ChatGptSyncState::load(self.service.data_home()).map_err(|error| error.to_string())?;
        state
            .seed_cursor(newest)
            .map_err(|error| error.to_string())?;
        let state_path = state
            .save(self.service.data_home())
            .map_err(|error| error.to_string())?;
        Ok(Json(ChatGptSeedFromIndexResponse {
            seeded_from: "indexed-chatgpt-source".to_string(),
            update_time: newest,
            state_path,
            state,
        }))
    }

    #[tool(
        description = "Adopt or copy a ChatGPT export ZIP and build the local conversation index."
    )]
    async fn index_export(
        &self,
        Parameters(request): Parameters<IndexExportRequest>,
    ) -> Result<Json<chat_history_core::ingest::ImportReport>, String> {
        self.require_write()?;
        let mode = match request.mode.as_deref() {
            Some("copy") => ImportMode::Copy,
            _ => ImportMode::Adopt,
        };
        self.service
            .import_archive(ImportOptions {
                source_archive: request.zip_path.map(PathBuf::from).unwrap_or_else(|| {
                    std::env::var_os("HOME")
                        .map(PathBuf::from)
                        .unwrap_or_else(|| PathBuf::from("."))
                        .join("Downloads")
                        .join("openai-export.zip")
                }),
                mode,
                run_api_jobs: request.run_api_jobs.unwrap_or(true),
                force_summaries: request.force_summaries.unwrap_or(false),
                force_embeddings: request.force_embeddings.unwrap_or(false),
            })
            .await
            .map(Json)
            .map_err(|error| error.to_string())
    }

    #[tool(
        description = "Import source-normalized conversations from ChatGPT, Codex, Gemini, Claude, Grok, or another collector."
    )]
    async fn import_conversations(
        &self,
        Parameters(request): Parameters<ImportConversationsRequest>,
    ) -> Result<Json<chat_history_core::ingest::ImportReport>, String> {
        self.require_write()?;
        if request.conversations.len() > 100 {
            return Err("at most 100 conversations may be imported per MCP call".to_string());
        }
        let source_label = request
            .source_label
            .unwrap_or_else(|| "mcp-import".to_string());
        self.service
            .import_normalized(
                request.conversations,
                Some(std::path::Path::new(&source_label)),
            )
            .map(Json)
            .map_err(|error| error.to_string())
    }

    #[tool(
        description = "Search indexed conversations by metadata, keyword, semantic, or hybrid retrieval."
    )]
    async fn search_conversations(
        &self,
        Parameters(request): Parameters<SearchRequest>,
    ) -> Result<Json<SearchResponse>, String> {
        let results = self
            .service
            .search(SearchOptions {
                query: request.query,
                mode: request.mode,
                date_from: request.date_from,
                date_to: request.date_to,
                model: request.model,
                sources: request.sources.unwrap_or_default(),
                risk_flags: request.risk_flags.unwrap_or_default(),
                topic_tags: request.topic_tags.unwrap_or_default(),
                limit: request.limit,
                sort: None,
            })
            .await
            .map_err(|error| error.to_string())?;
        Ok(Json(SearchResponse { results }))
    }

    #[tool(
        description = "Get one indexed conversation, including normalized messages and optional raw JSON."
    )]
    async fn get_conversation(
        &self,
        Parameters(request): Parameters<GetConversationRequest>,
    ) -> Result<Json<GetConversationResponse>, String> {
        let conversation = self
            .service
            .get_conversation(
                &request.conversation_id,
                request.include_raw.unwrap_or(false),
            )
            .map_err(|error| error.to_string())?;
        Ok(Json(GetConversationResponse { conversation }))
    }

    #[tool(description = "Find semantically related indexed conversations by conversation id.")]
    async fn related_conversations(
        &self,
        Parameters(request): Parameters<RelatedRequest>,
    ) -> Result<Json<RelatedResponse>, String> {
        let results = self
            .service
            .related_conversations(&request.conversation_id, request.limit.unwrap_or(10))
            .map_err(|error| error.to_string())?;
        Ok(Json(RelatedResponse { results }))
    }

    #[tool(
        description = "Generate missing or forced structured summaries for indexed conversations."
    )]
    async fn rebuild_summaries(
        &self,
        Parameters(request): Parameters<RebuildRequest>,
    ) -> Result<Json<RebuildResponse>, String> {
        self.require_write()?;
        let completed = self
            .service
            .rebuild_summaries(
                request.force.unwrap_or(false),
                request.conversation_ids,
                request.limit,
            )
            .await
            .map_err(|error| error.to_string())?;
        Ok(Json(RebuildResponse { completed }))
    }

    #[tool(description = "Generate missing or forced embeddings for indexed conversations.")]
    async fn rebuild_embeddings(
        &self,
        Parameters(request): Parameters<RebuildRequest>,
    ) -> Result<Json<RebuildResponse>, String> {
        self.require_write()?;
        let completed = self
            .service
            .rebuild_embeddings(
                request.force.unwrap_or(false),
                request.conversation_ids,
                request.limit,
            )
            .await
            .map_err(|error| error.to_string())?;
        Ok(Json(RebuildResponse { completed }))
    }

    #[tool(description = "Get index counts and high-level completion status.")]
    async fn index_stats(&self) -> Result<Json<chat_history_core::models::IndexStats>, String> {
        self.service
            .stats()
            .map(Json)
            .map_err(|error| error.to_string())
    }

    #[tool(
        description = "Search long-term AI conversation memory. Uses multilingual hybrid retrieval by default and optionally scopes results to a project."
    )]
    async fn memory_search(
        &self,
        Parameters(request): Parameters<MemorySearchRequest>,
    ) -> Result<Json<MemorySearchResponse>, String> {
        let limit = bounded_limit(request.limit, 8, 20);
        let results = self
            .memory_search_results(
                &request.query,
                request.project.as_deref(),
                request.sources.unwrap_or_default(),
                limit,
            )
            .await?;
        Ok(Json(MemorySearchResponse {
            retrieval_mode: "hybrid".to_string(),
            project: request.project,
            hits: results.into_iter().map(memory_hit).collect(),
        }))
    }

    #[tool(
        description = "List recent long-term memories, optionally scoped to a project. This is metadata-recency retrieval and does not invoke semantic embeddings."
    )]
    async fn memory_recent(
        &self,
        Parameters(request): Parameters<MemoryRecentRequest>,
    ) -> Result<Json<MemoryRecentResponse>, String> {
        let limit = bounded_limit(request.limit, 10, 50);
        let results = self
            .memory_recent_results(
                request.project.as_deref(),
                request.sources.unwrap_or_default(),
                request.since,
                limit,
            )
            .await?;
        Ok(Json(MemoryRecentResponse {
            project: request.project,
            hits: results.into_iter().map(memory_hit).collect(),
        }))
    }

    #[tool(
        description = "Read one memory thread as normalized messages with pagination. Use the evidence_conversation_id returned by memory_search when you need the exact supporting child/subtask thread."
    )]
    async fn memory_get_thread(
        &self,
        Parameters(request): Parameters<MemoryGetThreadRequest>,
    ) -> Result<Json<MemoryThreadResponse>, String> {
        let detail = self
            .service
            .get_conversation(&request.conversation_id, false)
            .map_err(|error| error.to_string())?;
        let Some(detail) = detail else {
            return Ok(Json(MemoryThreadResponse {
                thread: None,
                message_offset: 0,
                returned_messages: 0,
                total_messages: 0,
                truncated: false,
            }));
        };
        let total_messages = detail.messages.len();
        let offset = request.message_offset.unwrap_or(0).min(total_messages);
        let limit = bounded_limit(request.message_limit, 80, 250);
        let end = offset.saturating_add(limit).min(total_messages);
        let messages = detail.messages[offset..end]
            .iter()
            .map(|message| MemoryMessage {
                message_id: message.message_id.clone(),
                role: message.role.clone(),
                create_time: message.create_time,
                turn_index: message.turn_index,
                text: message.normalized_text.clone(),
            })
            .collect::<Vec<_>>();
        let summary = detail.conversation.summary.clone();
        Ok(Json(MemoryThreadResponse {
            thread: Some(MemoryThread {
                conversation: detail.conversation,
                summary,
                messages,
            }),
            message_offset: offset,
            returned_messages: end.saturating_sub(offset),
            total_messages,
            truncated: end < total_messages,
        }))
    }

    #[tool(
        description = "Build a compact project memory view by combining relevant hybrid hits with recent project conversations. Read-only; it does not summarize or write memory."
    )]
    async fn memory_project_context(
        &self,
        Parameters(request): Parameters<MemoryProjectContextRequest>,
    ) -> Result<Json<MemoryProjectContextResponse>, String> {
        let relevant_limit = bounded_limit(request.relevant_limit, 8, 20);
        let recent_limit = bounded_limit(request.recent_limit, 6, 20);
        let explicit_sources = request.sources;
        let query = request
            .query
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or(&request.project)
            .to_string();
        let (source_policy, relevant, recent) = if let Some(sources) = explicit_sources {
            let relevant = self
                .memory_search_results(
                    &query,
                    Some(&request.project),
                    sources.clone(),
                    relevant_limit,
                )
                .await?;
            let recent = self
                .memory_recent_results(Some(&request.project), sources, None, recent_limit)
                .await?;
            ("explicit".to_string(), relevant, recent)
        } else {
            let primary_relevant = self
                .memory_search_results(
                    &query,
                    Some(&request.project),
                    vec!["chatgpt".to_string()],
                    relevant_limit,
                )
                .await?;
            let fallback_relevant = if primary_relevant.len() < relevant_limit {
                self.memory_search_results(
                    &query,
                    Some(&request.project),
                    Vec::new(),
                    relevant_limit,
                )
                .await?
            } else {
                Vec::new()
            };
            let relevant =
                merge_unique_results(primary_relevant, fallback_relevant, relevant_limit);

            let primary_recent = self
                .memory_recent_results(
                    Some(&request.project),
                    vec!["chatgpt".to_string()],
                    None,
                    recent_limit,
                )
                .await?;
            let fallback_recent = if primary_recent.len() < recent_limit {
                self.memory_recent_results(Some(&request.project), Vec::new(), None, recent_limit)
                    .await?
            } else {
                Vec::new()
            };
            let recent = merge_unique_results(primary_recent, fallback_recent, recent_limit);
            ("chatgpt-first-fallback-all".to_string(), relevant, recent)
        };
        Ok(Json(MemoryProjectContextResponse {
            project: request.project,
            query,
            source_policy,
            relevant: relevant.into_iter().map(memory_hit).collect(),
            recent: recent.into_iter().map(memory_hit).collect(),
        }))
    }
}

impl ChatHistoryMcp {
    async fn memory_search_results(
        &self,
        query: &str,
        project: Option<&str>,
        sources: Vec<String>,
        limit: usize,
    ) -> Result<Vec<SearchResult>, String> {
        let candidate_limit = if project.is_some() {
            limit.saturating_mul(10).clamp(50, 200)
        } else {
            limit
        };
        let candidates = self
            .service
            .search(SearchOptions {
                query: Some(query.to_string()),
                mode: Some(SearchMode::Hybrid),
                sources,
                limit: Some(candidate_limit),
                ..SearchOptions::default()
            })
            .await
            .map_err(|error| error.to_string())?;
        self.filter_project(candidates, project, limit)
    }

    async fn memory_recent_results(
        &self,
        project: Option<&str>,
        sources: Vec<String>,
        since: Option<f64>,
        limit: usize,
    ) -> Result<Vec<SearchResult>, String> {
        let candidate_limit = if project.is_some() {
            limit.saturating_mul(100).clamp(500, 2_000)
        } else {
            limit
        };
        let candidates = self
            .service
            .search(SearchOptions {
                query: None,
                mode: Some(SearchMode::Metadata),
                date_from: since,
                sources,
                limit: Some(candidate_limit),
                ..SearchOptions::default()
            })
            .await
            .map_err(|error| error.to_string())?;
        self.filter_recent_project(candidates, project, limit)
    }

    fn filter_recent_project(
        &self,
        candidates: Vec<SearchResult>,
        project: Option<&str>,
        limit: usize,
    ) -> Result<Vec<SearchResult>, String> {
        let Some(project) = project.map(str::trim).filter(|value| !value.is_empty()) else {
            return Ok(candidates.into_iter().take(limit).collect());
        };
        let mut filtered = Vec::new();
        for candidate in candidates {
            if self.conversation_matches_project_strong(&candidate.conversation_id, project)? {
                filtered.push(candidate);
                if filtered.len() >= limit {
                    break;
                }
            }
        }
        Ok(filtered)
    }

    fn filter_project(
        &self,
        candidates: Vec<SearchResult>,
        project: Option<&str>,
        limit: usize,
    ) -> Result<Vec<SearchResult>, String> {
        let Some(project) = project.map(str::trim).filter(|value| !value.is_empty()) else {
            return Ok(candidates.into_iter().take(limit).collect());
        };
        let mut filtered = Vec::new();
        for candidate in candidates {
            let evidence_id = parse_evidence(&candidate).map(|value| value.0);
            let anchor_matches =
                self.conversation_matches_project(&candidate.conversation_id, project)?;
            let evidence_matches = match evidence_id.as_deref() {
                Some(evidence_id) if evidence_id != candidate.conversation_id => {
                    self.conversation_matches_project(evidence_id, project)?
                }
                _ => false,
            };
            if anchor_matches || evidence_matches {
                filtered.push(candidate);
                if filtered.len() >= limit {
                    break;
                }
            }
        }
        Ok(filtered)
    }

    fn conversation_matches_project(
        &self,
        conversation_id: &str,
        project: &str,
    ) -> Result<bool, String> {
        let detail = self
            .service
            .get_conversation(conversation_id, false)
            .map_err(|error| error.to_string())?;
        let Some(detail) = detail else {
            return Ok(false);
        };
        let needle = normalize_project_text(project);
        if needle.is_empty() {
            return Ok(true);
        }
        let mut haystacks = vec![detail.conversation.title];
        if let Some(path) = detail.conversation.source_path {
            haystacks.push(path);
        }
        if let Some(url) = detail.conversation.source_url {
            haystacks.push(url);
        }
        haystacks.extend(
            detail
                .messages
                .iter()
                .take(30)
                .map(|message| message.normalized_text.clone()),
        );
        Ok(haystacks
            .iter()
            .any(|value| normalize_project_text(value).contains(&needle)))
    }

    fn conversation_matches_project_strong(
        &self,
        conversation_id: &str,
        project: &str,
    ) -> Result<bool, String> {
        let detail = self
            .service
            .get_conversation(conversation_id, false)
            .map_err(|error| error.to_string())?;
        let Some(detail) = detail else {
            return Ok(false);
        };
        let needle = normalize_project_text(project);
        if needle.is_empty() {
            return Ok(true);
        }
        let title_matches = normalize_project_text(&detail.conversation.title).contains(&needle);
        let source_path_matches = detail
            .conversation
            .source_path
            .as_deref()
            .is_some_and(|path| normalize_project_text(path).contains(&needle));
        let source_url_matches = detail
            .conversation
            .source_url
            .as_deref()
            .is_some_and(|url| normalize_project_text(url).contains(&needle));
        Ok(title_matches || source_path_matches || source_url_matches)
    }
}

#[derive(Debug, Serialize, JsonSchema)]
struct RebuildResponse {
    completed: usize,
}

#[derive(Debug, Serialize, JsonSchema)]
struct SearchResponse {
    results: Vec<chat_history_core::models::SearchResult>,
}

#[derive(Debug, Serialize, JsonSchema)]
struct RelatedResponse {
    results: Vec<chat_history_core::models::SearchResult>,
}

#[derive(Debug, Serialize, JsonSchema)]
struct GetConversationResponse {
    conversation: Option<chat_history_core::models::ConversationDetail>,
}

#[derive(Debug, Serialize, JsonSchema)]
struct ChatGptPlanRecentResponse {
    plan: ChatGptDiscoveryPlan,
    state_path: PathBuf,
    state: ChatGptSyncState,
}

#[derive(Debug, Serialize, JsonSchema)]
struct ChatGptImportThreadResponse {
    thread_id: String,
    import: chat_history_core::ingest::ImportReport,
    embeddings_completed: usize,
    state_path: PathBuf,
    state: ChatGptSyncState,
}

#[derive(Debug, Serialize, JsonSchema)]
struct ChatGptBlockResponse {
    blocked: ChatGptBlockedThread,
    state_path: PathBuf,
    state: ChatGptSyncState,
}

#[derive(Debug, Serialize, JsonSchema)]
struct ChatGptSeedResponse {
    state_path: PathBuf,
    state: ChatGptSyncState,
}

#[derive(Debug, Serialize, JsonSchema)]
struct ChatGptSeedFromIndexResponse {
    seeded_from: String,
    update_time: f64,
    state_path: PathBuf,
    state: ChatGptSyncState,
}

#[derive(Debug, Serialize, JsonSchema)]
struct MemorySearchResponse {
    retrieval_mode: String,
    project: Option<String>,
    hits: Vec<MemoryHit>,
}

#[derive(Debug, Serialize, JsonSchema)]
struct MemoryRecentResponse {
    project: Option<String>,
    hits: Vec<MemoryHit>,
}

#[derive(Debug, Serialize, JsonSchema)]
struct MemoryProjectContextResponse {
    project: String,
    query: String,
    source_policy: String,
    relevant: Vec<MemoryHit>,
    recent: Vec<MemoryHit>,
}

#[derive(Debug, Serialize, JsonSchema)]
struct MemoryHit {
    result: SearchResult,
    evidence_conversation_id: Option<String>,
    evidence_chunk_index: Option<i64>,
}

#[derive(Debug, Serialize, JsonSchema)]
struct MemoryThreadResponse {
    thread: Option<MemoryThread>,
    message_offset: usize,
    returned_messages: usize,
    total_messages: usize,
    truncated: bool,
}

#[derive(Debug, Serialize, JsonSchema)]
struct MemoryThread {
    conversation: ConversationRecord,
    summary: Option<SummaryRecord>,
    messages: Vec<MemoryMessage>,
}

#[derive(Debug, Serialize, JsonSchema)]
struct MemoryMessage {
    message_id: String,
    role: String,
    create_time: Option<f64>,
    turn_index: i64,
    text: String,
}

fn bounded_limit(value: Option<usize>, default: usize, max: usize) -> usize {
    value.unwrap_or(default).clamp(1, max)
}

fn merge_unique_results(
    primary: Vec<SearchResult>,
    fallback: Vec<SearchResult>,
    limit: usize,
) -> Vec<SearchResult> {
    let mut seen = std::collections::HashSet::new();
    primary
        .into_iter()
        .chain(fallback)
        .filter(|result| seen.insert(result.conversation_id.clone()))
        .take(limit)
        .collect()
}

fn memory_hit(result: SearchResult) -> MemoryHit {
    let evidence = parse_evidence(&result);
    MemoryHit {
        result,
        evidence_conversation_id: evidence.as_ref().map(|value| value.0.clone()),
        evidence_chunk_index: evidence.and_then(|value| value.1),
    }
}

fn parse_evidence(result: &SearchResult) -> Option<(String, Option<i64>)> {
    let snippet = result.snippet.as_deref()?;
    if let Some(rest) = snippet.strip_prefix("semantic evidence: ") {
        let (conversation_id, chunk) = rest.split_once(" chunk ")?;
        let chunk_index = chunk.split_whitespace().next()?.parse().ok();
        return Some((conversation_id.to_string(), chunk_index));
    }
    for prefix in ["lexical evidence: ", "fts evidence: "] {
        if let Some(rest) = snippet.strip_prefix(prefix) {
            let conversation_id = rest.split(';').next()?.trim();
            if !conversation_id.is_empty() {
                return Some((conversation_id.to_string(), None));
            }
        }
    }
    None
}

fn normalize_project_text(value: &str) -> String {
    value
        .chars()
        .filter(|ch| ch.is_alphanumeric())
        .flat_map(|ch| ch.to_lowercase())
        .collect()
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for ChatHistoryMcp {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_instructions(
                "Search and inspect a local ChatGPT export index, rebuild summaries and embeddings, and retrieve related conversations.",
            )
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum Transport {
    Stdio,
    Http,
}

#[derive(Debug, Parser)]
struct Args {
    #[arg(long, env = "CHAT_HISTORY_MCP_TRANSPORT", value_enum, default_value_t = Transport::Stdio)]
    transport: Transport,
    #[arg(long, env = "CHAT_HISTORY_HTTP_BIND", default_value = "127.0.0.1:8765")]
    bind: String,
    #[arg(long, env = "CHAT_HISTORY_HTTP_TOKEN")]
    token: Option<String>,
    #[arg(
        long = "allowed-host",
        env = "CHAT_HISTORY_ALLOWED_HOSTS",
        value_delimiter = ','
    )]
    allowed_hosts: Vec<String>,
    #[arg(long, env = "CHAT_HISTORY_ALLOW_WRITES", default_value_t = false)]
    allow_writes: bool,
    #[arg(long, env = "CHAT_HISTORY_COLLECTOR_ONLY", default_value_t = false)]
    collector_only: bool,
}

#[derive(Clone)]
struct AuthState {
    token: Arc<String>,
}

async fn require_bearer(
    State(state): State<AuthState>,
    request: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let expected = format!("Bearer {}", state.token);
    let authorized = request
        .headers()
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value == expected);
    if !authorized {
        return Err(StatusCode::UNAUTHORIZED);
    }
    Ok(next.run(request).await)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            std::env::var("RUST_LOG").unwrap_or_else(|_| "info,rmcp=warn,reqwest=warn".to_string()),
        )
        .without_time()
        .init();

    let args = Args::parse();
    let data_home = std::env::var_os("CHAT_HISTORY_DATA_HOME").map(PathBuf::from);
    let service = IndexService::with_env(DataHome::from_option(data_home));
    match args.transport {
        Transport::Stdio => {
            ChatHistoryMcp::new(service, AccessMode::Full)
                .serve(rmcp::transport::stdio())
                .await?
                .waiting()
                .await?;
        }
        Transport::Http => serve_http(service, args).await?,
    }
    Ok(())
}

async fn serve_http(service: IndexService, args: Args) -> anyhow::Result<()> {
    use rmcp::transport::streamable_http_server::{
        StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
    };
    use tokio_util::sync::CancellationToken;

    let token = args
        .token
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| anyhow::anyhow!("CHAT_HISTORY_HTTP_TOKEN is required for HTTP transport"))?;
    let bind_host = args
        .bind
        .split(':')
        .next()
        .unwrap_or("127.0.0.1")
        .to_string();
    let allowed_hosts = if args.allowed_hosts.is_empty() {
        vec!["localhost".to_string(), "127.0.0.1".to_string(), bind_host]
    } else {
        args.allowed_hosts
    };
    let cancellation = CancellationToken::new();
    let config = StreamableHttpServerConfig::default()
        .with_stateful_mode(false)
        .with_json_response(true)
        .with_allowed_hosts(allowed_hosts)
        .with_cancellation_token(cancellation.child_token());
    let access = if args.collector_only {
        AccessMode::Collector
    } else if args.allow_writes {
        AccessMode::Full
    } else {
        AccessMode::ReadOnly
    };
    let mcp_service: StreamableHttpService<ChatHistoryMcp, LocalSessionManager> =
        StreamableHttpService::new(
            move || Ok(ChatHistoryMcp::new(service.clone(), access)),
            Default::default(),
            config,
        );
    let auth_state = AuthState {
        token: Arc::new(token),
    };
    let router = Router::new()
        .nest_service("/mcp", mcp_service)
        .layer(middleware::from_fn_with_state(auth_state, require_bearer));
    let listener = tokio::net::TcpListener::bind(&args.bind).await?;
    tracing::info!(bind = %args.bind, "chat history MCP HTTP server listening");
    axum::serve(listener, router)
        .with_graceful_shutdown(async move {
            let _ = tokio::signal::ctrl_c().await;
            cancellation.cancel();
        })
        .await?;
    Ok(())
}
