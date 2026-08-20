use std::{path::PathBuf, sync::Arc};

use axum::{
    Router,
    extract::{Request, State},
    http::{StatusCode, header::AUTHORIZATION},
    middleware::{self, Next},
    response::Response,
};
use chat_history_core::{
    DataHome, ImportMode, ImportOptions, IndexService, NormalizedConversation, SearchMode,
    SearchOptions,
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
    read_only: bool,
    tool_router: ToolRouter<Self>,
}

impl ChatHistoryMcp {
    fn new(service: IndexService, read_only: bool) -> Self {
        let mut tool_router = Self::tool_router();
        if read_only {
            for name in [
                "index_export",
                "import_conversations",
                "rebuild_embeddings",
                "rebuild_summaries",
            ] {
                tool_router.remove_route(name);
            }
        }
        Self {
            service,
            read_only,
            tool_router,
        }
    }

    fn require_write(&self) -> Result<(), String> {
        if self.read_only {
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

#[tool_router]
impl ChatHistoryMcp {
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
            ChatHistoryMcp::new(service, false)
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
    let read_only = !args.allow_writes;
    let mcp_service: StreamableHttpService<ChatHistoryMcp, LocalSessionManager> =
        StreamableHttpService::new(
            move || Ok(ChatHistoryMcp::new(service.clone(), read_only)),
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
