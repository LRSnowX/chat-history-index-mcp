use std::{
    fs,
    io::{self, Read},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, ensure};
use chat_history_core::{
    ChatGptBridgeThread, ChatGptBridgeTranscript, ChatGptSyncState, ChatGptThreadListSnapshot,
    CollaborationMemoryAuthoringInput, CollaborationMemoryRetirementInput,
    DEFAULT_MEMORY_COMPILER_MESSAGES, DEFAULT_MEMORY_PROJECT_MAX_CONVERSATIONS,
    DEFAULT_MEMORY_PROJECT_SCAN_LIMIT, DataHome, ImportMode, ImportOptions, IndexService,
    MAX_MEMORY_COMPILER_MESSAGES, MemoryCompilerManualBundle, MemoryEvidence, MemoryEvidenceKind,
    MemoryKind, MemoryModelClient, MemoryPromotionReview, NormalizedConversation,
    ProjectMemoryConfirmationInput, ProjectMemoryRetirementInput, ProjectOperationalMemorySetInput,
    ProjectOperationalMemoryTransitionInput, SearchMode, SearchOptions,
};
use clap::{ArgAction, Parser, Subcommand, ValueEnum};

#[derive(Debug, Parser)]
#[command(name = "chat-history")]
#[command(about = "Local ChatGPT export index and MCP operator CLI")]
struct Cli {
    #[arg(long, env = "CHAT_HISTORY_DATA_HOME")]
    data_home: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Import {
        #[arg(long)]
        archive: Option<PathBuf>,
        #[arg(long, value_enum, default_value_t = ImportModeArg::Adopt)]
        mode: ImportModeArg,
        #[arg(long, default_value_t = true, action = ArgAction::Set)]
        run_api_jobs: bool,
        #[arg(long, default_value_t = false)]
        force_summaries: bool,
        #[arg(long, default_value_t = false)]
        force_embeddings: bool,
    },
    ImportCodex {
        #[arg(long = "root")]
        roots: Vec<PathBuf>,
        #[arg(long)]
        since: Option<String>,
        #[arg(long, default_value_t = 100)]
        batch_size: usize,
        #[arg(long, default_value_t = false)]
        skip_existing: bool,
    },
    /// Emit normalized local Codex conversations without writing an index.
    ExportCodex {
        #[arg(long = "root")]
        roots: Vec<PathBuf>,
        #[arg(long)]
        since: Option<String>,
    },
    SyncCodex {
        #[arg(long = "root")]
        roots: Vec<PathBuf>,
        #[arg(long, default_value = "2026-04-14")]
        initial_since: String,
        #[arg(long, default_value_t = 48)]
        overlap_hours: u64,
        #[arg(long, default_value_t = 100)]
        batch_size: usize,
    },
    /// Import Gemini CLI session recordings with a durable overlap cursor.
    SyncGemini {
        #[arg(long = "root")]
        roots: Vec<PathBuf>,
        #[arg(long, default_value = "2026-04-14")]
        initial_since: String,
        #[arg(long, default_value_t = 48)]
        overlap_hours: u64,
        #[arg(long, default_value_t = 100)]
        batch_size: usize,
    },
    /// Import plaintext Antigravity transcript exports with a durable overlap cursor.
    SyncAntigravity {
        #[arg(long = "root")]
        roots: Vec<PathBuf>,
        #[arg(long, default_value = "2026-04-14")]
        initial_since: String,
        #[arg(long, default_value_t = 48)]
        overlap_hours: u64,
        #[arg(long, default_value_t = 100)]
        batch_size: usize,
    },
    ImportNormalized {
        #[arg(long)]
        path: PathBuf,
        #[arg(long)]
        stdin_bytes: Option<u64>,
    },
    /// Inspect durable ChatGPT.app collector state.
    ChatgptState,
    /// Read a current trusted canonical continuation baseline (no provider reads).
    ChatgptContinuationBaseline {
        thread_id: String,
    },
    /// Publish an identity-verified append-only replay; preserve the blocker until success.
    ChatgptRepairContinuation {
        #[arg(long, default_value = "-")]
        path: PathBuf,
        #[arg(long)]
        stdin_bytes: Option<u64>,
        #[arg(long, default_value_t = true, action = ArgAction::Set)]
        embed: bool,
    },
    /// Append lossless official-export evidence to an already blocked canonical thread.
    ChatgptRepairExport {
        #[arg(long)]
        archive: PathBuf,
        #[arg(long)]
        baseline: PathBuf,
        #[arg(long, default_value_t = true, action = ArgAction::Set)]
        embed: bool,
    },
    /// Read-only, digest-bound historical gap restoration plan (no provider calls).
    ChatgptHistoryRestorePlan {
        #[arg(long = "source", required = true)]
        sources: Vec<PathBuf>,
        #[arg(long)]
        title_prefix: String,
    },
    /// Apply exactly one previously planned conversation; never clears live state.
    ChatgptHistoryRestoreApply {
        #[arg(long)]
        plan: PathBuf,
        #[arg(long)]
        conversation_id: String,
    },
    /// List unresolved historical restores awaiting live verification.
    ChatgptHistoryRestorePending {
        #[arg(long, default_value_t = 16)]
        limit: usize,
    },
    /// Record one provider observation without changing discovery cursor or pending state.
    ChatgptObserveThread {
        #[arg(long, default_value = "-")]
        path: PathBuf,
        #[arg(long)]
        stdin_bytes: Option<u64>,
    },
    /// Record one bounded content-free failure without removing the original blocker.
    ChatgptRecordRepairFailure {
        thread_id: String,
        #[arg(long)]
        code: String,
    },
    /// Plan one recent-50 ChatGPT.app discovery batch without importing transcripts.
    ChatgptPlanRecent {
        #[arg(long, default_value = "-")]
        path: PathBuf,
        #[arg(long)]
        stdin_bytes: Option<u64>,
    },
    /// Validate and import one fully paged ChatGPT.app transcript, then advance durable state.
    ChatgptImportThread {
        #[arg(long, default_value = "-")]
        path: PathBuf,
        #[arg(long)]
        stdin_bytes: Option<u64>,
        #[arg(long, default_value_t = true, action = ArgAction::Set)]
        embed: bool,
    },
    /// Mark one ChatGPT thread as blocked/incomplete so later batches can continue safely.
    ChatgptBlock {
        thread_id: String,
        #[arg(long)]
        reason: String,
    },
    /// Seed the live ChatGPT collector cursor after a trusted complete bootstrap/backfill.
    ChatgptSeedCursor {
        update_time: f64,
    },
    /// Seed the live ChatGPT collector cursor from the newest indexed ChatGPT conversation.
    ChatgptSeedFromIndex,
    /// One-time ChatGPT history bootstrap from an OpenAI export ZIP, with DB backup and local embeddings.
    ChatgptBootstrapExport {
        #[arg(long)]
        archive: PathBuf,
        #[arg(long, default_value_t = true, action = ArgAction::Set)]
        embed: bool,
    },
    /// Compile one bounded conversation delta into pending memory candidates. Does not promote.
    MemoryCompileConversation {
        #[arg(long)]
        project: String,
        conversation_id: String,
        #[arg(long, default_value_t = DEFAULT_MEMORY_COMPILER_MESSAGES)]
        max_messages: usize,
    },
    /// Export one bounded compiler prompt/bundle for manual Codex handoff. Never invokes a model.
    MemoryCompileManualExport {
        #[arg(long)]
        project: String,
        conversation_id: String,
        #[arg(long, default_value_t = DEFAULT_MEMORY_COMPILER_MESSAGES)]
        max_messages: usize,
        #[arg(long)]
        bundle_out: PathBuf,
        #[arg(long)]
        prompt_out: PathBuf,
        #[arg(long, default_value_t = false)]
        overwrite: bool,
    },
    /// Stage a manually returned compiler JSON response against its exact exported bundle.
    MemoryCompileManualStage {
        #[arg(long)]
        bundle: PathBuf,
        #[arg(long)]
        response: PathBuf,
        #[arg(long)]
        model_label: String,
    },
    /// Compile recent strong-match conversations for one project, bounded by model-call count.
    MemoryCompileProject {
        #[arg(long)]
        project: String,
        #[arg(long, default_value_t = DEFAULT_MEMORY_PROJECT_SCAN_LIMIT)]
        scan_limit: usize,
        #[arg(long, default_value_t = DEFAULT_MEMORY_PROJECT_MAX_CONVERSATIONS)]
        max_conversations: usize,
        #[arg(long, default_value_t = DEFAULT_MEMORY_COMPILER_MESSAGES)]
        max_messages: usize,
    },
    /// Plan a selective current-state bootstrap from recent complete ChatGPT project conversations.
    MemoryBootstrapPlan {
        #[arg(long)]
        project: String,
        #[arg(long, default_value_t = 3)]
        max_conversations: usize,
        #[arg(long, default_value_t = MAX_MEMORY_COMPILER_MESSAGES)]
        max_messages: usize,
    },
    /// Plan a bounded recent project compilation without invoking a model or writing candidates.
    MemoryCompilePlan {
        #[arg(long)]
        project: String,
        #[arg(long, default_value_t = DEFAULT_MEMORY_PROJECT_SCAN_LIMIT)]
        scan_limit: usize,
        #[arg(long, default_value_t = DEFAULT_MEMORY_PROJECT_MAX_CONVERSATIONS)]
        max_conversations: usize,
        #[arg(long, default_value_t = DEFAULT_MEMORY_COMPILER_MESSAGES)]
        max_messages: usize,
    },
    /// Check whether one indexed conversation strongly matches a canonical project identity.
    MemoryProjectMatch {
        #[arg(long)]
        project: String,
        conversation_id: String,
    },
    /// List explicit strong-match aliases for one canonical project identity.
    MemoryProjectAliases {
        #[arg(long)]
        project: String,
    },
    /// Add one explicit strong-match alias to a canonical project identity.
    MemoryProjectAliasAdd {
        #[arg(long)]
        project: String,
        alias: String,
    },
    /// Remove one explicit strong-match alias from a canonical project identity.
    MemoryProjectAliasRemove {
        #[arg(long)]
        project: String,
        alias: String,
    },
    /// Inspect pending staged memory candidates for one project.
    MemoryCandidates {
        #[arg(long)]
        project: String,
    },
    /// Inspect one staged memory candidate by id.
    MemoryCandidate {
        candidate_id: String,
    },
    /// Inspect immutable promotion/reverification review history for one candidate.
    MemoryCandidateReviews {
        candidate_id: String,
    },
    /// Explicitly promote one pending candidate after revalidation.
    MemoryCandidatePromote {
        candidate_id: String,
        #[arg(long)]
        reason: String,
        #[arg(long = "evidence")]
        evidence: Vec<String>,
    },
    /// Explicitly reject one pending candidate with an operator reason.
    MemoryCandidateReject {
        candidate_id: String,
        #[arg(long)]
        reason: String,
    },
    /// Inspect project memory, candidate, and compiler checkpoint health without model calls.
    MemoryHealth {
        #[arg(long)]
        project: String,
        #[arg(long, default_value_t = 30)]
        stale_after_days: u32,
    },
    /// Evaluate pending candidates against the conservative automatic-promotion policy. Read-only.
    MemoryAutoPromotionPlan {
        #[arg(long)]
        project: String,
    },
    /// List active stable cross-project collaboration rules.
    MemoryCollaborationList,
    /// Explicitly author or supersede one stable global collaboration rule.
    MemoryCollaborationAuthor {
        #[arg(long)]
        kind: String,
        #[arg(long)]
        key: String,
        #[arg(
            long,
            conflicts_with = "value_json",
            required_unless_present = "value_json"
        )]
        value: Option<String>,
        #[arg(long, conflicts_with = "value", required_unless_present = "value")]
        value_json: Option<String>,
        #[arg(long, default_value_t = 90)]
        importance: u8,
        #[arg(long, default_value_t = 1.0)]
        confidence: f64,
        #[arg(long)]
        supersedes: Option<String>,
        #[arg(long)]
        reason: String,
        #[arg(long = "evidence", required = true)]
        evidence: Vec<String>,
    },
    /// Explicitly retire one active global collaboration rule.
    MemoryCollaborationRetire {
        memory_id: String,
        #[arg(long)]
        reason: String,
        #[arg(long = "evidence", required = true)]
        evidence: Vec<String>,
    },
    /// Confirm one stable project-local historical rule after explicit user review.
    MemoryProjectConfirm {
        #[arg(long)]
        project: String,
        #[arg(long)]
        kind: String,
        #[arg(long)]
        key: String,
        #[arg(
            long,
            conflicts_with = "value_json",
            required_unless_present = "value_json"
        )]
        value: Option<String>,
        #[arg(long, conflicts_with = "value", required_unless_present = "value")]
        value_json: Option<String>,
        #[arg(long, default_value_t = 95)]
        importance: u8,
        #[arg(long, default_value_t = 1.0)]
        confidence: f64,
        #[arg(long)]
        supersedes: Option<String>,
        #[arg(long)]
        reason: String,
        #[arg(long)]
        user_confirmation: String,
        #[arg(long = "evidence")]
        evidence: Vec<String>,
    },
    /// Retire one previously user-confirmed project-local rule.
    MemoryProjectRetire {
        #[arg(long)]
        project: String,
        memory_id: String,
        #[arg(long)]
        reason: String,
        #[arg(long)]
        user_confirmation: String,
        #[arg(long = "evidence")]
        evidence: Vec<String>,
    },
    /// Explicitly set or supersede project-local operational Working Memory.
    MemoryProjectOperationalSet {
        #[arg(long)]
        project: String,
        #[arg(long, value_parser = ["state", "blocker", "task"])]
        kind: String,
        #[arg(long)]
        key: String,
        #[arg(
            long,
            conflicts_with = "value_json",
            required_unless_present = "value_json"
        )]
        value: Option<String>,
        #[arg(long, conflicts_with = "value", required_unless_present = "value")]
        value_json: Option<String>,
        #[arg(long, default_value_t = 90)]
        importance: u8,
        #[arg(long, default_value_t = 1.0)]
        confidence: f64,
        #[arg(long)]
        supersedes: Option<String>,
        #[arg(long)]
        reason: String,
        #[arg(long = "evidence", required = true)]
        evidence: Vec<String>,
    },
    /// Explicitly resolve active project-local operational memory.
    MemoryProjectOperationalResolve {
        #[arg(long)]
        project: String,
        memory_id: String,
        #[arg(long)]
        reason: String,
        #[arg(long = "evidence", required = true)]
        evidence: Vec<String>,
    },
    /// Explicitly archive active or resolved project-local operational memory.
    MemoryProjectOperationalArchive {
        #[arg(long)]
        project: String,
        memory_id: String,
        #[arg(long)]
        reason: String,
        #[arg(long = "evidence", required = true)]
        evidence: Vec<String>,
    },
    Resume,
    Stats,
    Search {
        query: Option<String>,
        #[arg(long, value_enum)]
        mode: Option<SearchModeArg>,
        #[arg(long)]
        date_from: Option<f64>,
        #[arg(long)]
        date_to: Option<f64>,
        #[arg(long)]
        model: Option<String>,
        #[arg(long = "source")]
        sources: Vec<String>,
        #[arg(long)]
        risk_flag: Vec<String>,
        #[arg(long)]
        topic_tag: Vec<String>,
        #[arg(long)]
        limit: Option<usize>,
    },
    Show {
        conversation_id: String,
        #[arg(long, default_value_t = false)]
        include_raw: bool,
    },
    Related {
        conversation_id: String,
        #[arg(long, default_value_t = 10)]
        limit: usize,
    },
    SummarizeMissing {
        #[arg(long, default_value_t = false)]
        force: bool,
        #[arg(long)]
        limit: Option<usize>,
    },
    EmbedMissing {
        #[arg(long, default_value_t = false)]
        force: bool,
        #[arg(long)]
        limit: Option<usize>,
    },
    ReindexFts,
    /// Create a consistent single-file SQLite backup, including committed WAL data.
    Backup {
        #[arg(long)]
        output: PathBuf,
        #[arg(long, default_value_t = false)]
        overwrite: bool,
    },
    /// Restore a backup after the canonical writer service has been stopped.
    Restore {
        #[arg(long)]
        input: PathBuf,
        #[arg(long, default_value_t = false)]
        confirm_writer_stopped: bool,
    },
    /// Run SQLite integrity, schema, source-count, and timestamp checks.
    Doctor,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum ImportModeArg {
    Adopt,
    Copy,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum SearchModeArg {
    Metadata,
    Fts,
    Semantic,
    Hybrid,
}

impl From<ImportModeArg> for ImportMode {
    fn from(value: ImportModeArg) -> Self {
        match value {
            ImportModeArg::Adopt => ImportMode::Adopt,
            ImportModeArg::Copy => ImportMode::Copy,
        }
    }
}

impl From<SearchModeArg> for SearchMode {
    fn from(value: SearchModeArg) -> Self {
        match value {
            SearchModeArg::Metadata => SearchMode::Metadata,
            SearchModeArg::Fts => SearchMode::Fts,
            SearchModeArg::Semantic => SearchMode::Semantic,
            SearchModeArg::Hybrid => SearchMode::Hybrid,
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            std::env::var("RUST_LOG").unwrap_or_else(|_| "info,rmcp=warn,reqwest=warn".to_string()),
        )
        .without_time()
        .init();

    let cli = Cli::parse();
    let data_home = DataHome::from_option(cli.data_home);
    let service = IndexService::with_env(data_home.clone());
    match cli.command {
        Command::Import {
            archive,
            mode,
            run_api_jobs,
            force_summaries,
            force_embeddings,
        } => {
            let report = service
                .import_archive(ImportOptions {
                    source_archive: archive.unwrap_or_else(default_export_path),
                    mode: mode.into(),
                    run_api_jobs,
                    force_summaries,
                    force_embeddings,
                })
                .await?;
            print_json(&report)?;
        }
        Command::ImportCodex {
            roots,
            since,
            batch_size,
            skip_existing,
        } => {
            let roots = codex_roots(roots);
            let cutoff = since.as_deref().map(parse_since).transpose()?;
            let mut paths = chat_history_core::codex::discover_rollouts(&roots)?;
            if skip_existing {
                let existing = service.source_conversation_ids("codex")?;
                paths.retain(|path| {
                    rollout_id_from_path(path).is_none_or(|id| !existing.contains(id))
                });
            }
            print_json(&import_codex_paths(
                &service, paths, cutoff, batch_size, false,
            )?)?;
        }
        Command::ExportCodex { roots, since } => {
            let cutoff = since.as_deref().map(parse_since).transpose()?;
            let paths = chat_history_core::codex::discover_rollouts(&codex_roots(roots))?;
            let discovered = paths.len();
            let mut conversations = Vec::new();
            let mut skipped = 0usize;
            let mut errors = Vec::new();
            for path in paths {
                match chat_history_core::codex::parse_rollout(&path, cutoff) {
                    Ok(Some(conversation)) => conversations.push(conversation),
                    Ok(None) => skipped += 1,
                    Err(error) => errors.push(format!("{}: {error}", path.display())),
                }
            }
            print_json(&CodexExportSummary {
                rollouts_discovered: discovered,
                conversations,
                skipped,
                errors,
            })?;
        }
        Command::SyncCodex {
            roots,
            initial_since,
            overlap_hours,
            batch_size,
        } => {
            let paths = data_home.paths();
            paths.ensure()?;
            let cursor_path = paths.cache_dir.join("codex-sync-cursor.json");
            let previous = read_cursor(&cursor_path)?;
            let started_at = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs_f64();
            let cutoff = previous
                .map(|epoch| epoch - overlap_hours as f64 * 3600.0)
                .unwrap_or(parse_since(&initial_since)?);
            let rollouts = chat_history_core::codex::discover_rollouts(&codex_roots(roots))?;
            let report = import_codex_paths(&service, rollouts, Some(cutoff), batch_size, true)?;
            if !report.errors.is_empty() {
                anyhow::bail!(
                    "Codex sync had {} parse errors; cursor was not advanced",
                    report.errors.len()
                );
            }
            fs::write(
                &cursor_path,
                serde_json::to_vec_pretty(&serde_json::json!({
                    "last_success_epoch": started_at,
                    "last_success_utc": chrono::DateTime::from_timestamp(started_at as i64, 0).map(|value| value.to_rfc3339()),
                }))?,
            )?;
            print_json(&report)?;
        }
        Command::SyncGemini {
            roots,
            initial_since,
            overlap_hours,
            batch_size,
        } => {
            let report = sync_provider(
                &data_home,
                &service,
                "gemini-sync-cursor.json",
                "gemini-cli-sessions",
                gemini_roots(roots),
                initial_since,
                overlap_hours,
                batch_size,
                chat_history_core::gemini::discover_sessions,
                chat_history_core::gemini::parse_session,
            )?;
            print_json(&report)?;
        }
        Command::SyncAntigravity {
            roots,
            initial_since,
            overlap_hours,
            batch_size,
        } => {
            let report = sync_provider(
                &data_home,
                &service,
                "antigravity-sync-cursor.json",
                "antigravity-transcripts",
                antigravity_roots(roots),
                initial_since,
                overlap_hours,
                batch_size,
                chat_history_core::antigravity::discover_transcripts,
                chat_history_core::antigravity::parse_transcript,
            )?;
            print_json(&report)?;
        }
        Command::ImportNormalized { path, stdin_bytes } => {
            let conversations = read_normalized(&path, stdin_bytes)?;
            let report = service.import_normalized(conversations, Some(&path))?;
            print_json(&report)?;
        }
        Command::ChatgptState => {
            let state = ChatGptSyncState::load(&data_home)?;
            print_json(&state)?;
        }
        Command::ChatgptContinuationBaseline { thread_id } => {
            print_json(&service.continuation_baseline(&thread_id)?)?;
        }
        Command::ChatgptRepairContinuation {
            path,
            stdin_bytes,
            embed,
        } => {
            use chat_history_core::continuation::RepairFailureCode;
            let request: chat_history_core::continuation::ContinuationImport =
                read_json_document(&path, stdin_bytes)
                    .map_err(|_| RepairFailureCode::ReplayIncomplete)?;
            let report = match service.import_verified_continuation(&request) {
                Ok(report) => report,
                Err(error) => {
                    let code = RepairFailureCode::from_error(&error);
                    let mut state = ChatGptSyncState::load(&data_home)?;
                    if state.record_repair_failure(&request.baseline.source_thread_id, code) {
                        state.save(&data_home)?;
                    }
                    return Err(code.into());
                }
            };
            let embeddings_completed = if embed {
                service
                    .rebuild_embeddings(
                        false,
                        Some(vec![request.baseline.conversation_id.clone()]),
                        None,
                    )
                    .await?
            } else {
                0
            };
            let mut state = ChatGptSyncState::load(&data_home)?;
            // Newer/contradictory observation after publication must not be erased.
            let observation = state
                .provider_observations
                .get(&request.baseline.source_thread_id);
            ensure!(
                observation.is_some_and(|o| o.provider_revision
                    == request.provider_after.update_time
                    && o.provider_status.as_deref() == Some("idle")
                    && o.observed_at == request.provider_after.observed_at),
                RepairFailureCode::ProviderChanged
            );
            state.mark_imported_at(
                &request.baseline.source_thread_id,
                request.provider_after.update_time,
            );
            state.save(&data_home)?;
            print_json(
                &serde_json::json!({"import":report,"embeddings_completed":embeddings_completed,"state":state}),
            )?;
        }
        Command::ChatgptRepairExport {
            archive,
            baseline,
            embed,
        } => {
            use chat_history_core::continuation::{ContinuationBaseline, RepairFailureCode};
            let expected: ContinuationBaseline = read_json_document(&baseline, None)
                .map_err(|_| RepairFailureCode::BaselineChanged)?;
            let report = service
                .import_export_continuation(&expected, &archive)
                .map_err(|error| RepairFailureCode::from_error(&error))?;
            let embeddings_completed = if embed {
                service
                    .rebuild_embeddings(false, Some(vec![expected.conversation_id.clone()]), None)
                    .await?
            } else {
                0
            };
            print_json(
                &serde_json::json!({"status":"awaiting_live_verification","import":report,"embeddings_completed":embeddings_completed}),
            )?;
        }
        Command::ChatgptHistoryRestorePlan {
            sources,
            title_prefix,
        } => {
            let plan = service
                .history_restore_plan(&sources, &title_prefix)
                .map_err(|error| {
                    error
                        .downcast_ref::<chat_history_core::history_restore::RestoreCode>()
                        .copied()
                        .unwrap_or(chat_history_core::history_restore::RestoreCode::InvalidPlan)
                })?;
            print_json(&plan)?;
        }
        Command::ChatgptHistoryRestoreApply {
            plan,
            conversation_id,
        } => {
            let plan: chat_history_core::history_restore::RestorePlan =
                read_json_document(&plan, None)
                    .map_err(|_| chat_history_core::history_restore::RestoreCode::InvalidPlan)?;
            let report = service.apply_history_restore(&plan, &conversation_id)?;
            print_json(
                &serde_json::json!({"status":"awaiting_live_verification","import":report}),
            )?;
        }
        Command::ChatgptHistoryRestorePending { limit } => {
            let pending = service.pending_history_restores(limit)?;
            print_json(&serde_json::json!({"pending":pending}))?;
        }
        Command::ChatgptObserveThread { path, stdin_bytes } => {
            let thread: ChatGptBridgeThread = read_json_document(&path, stdin_bytes)?;
            let mut state = ChatGptSyncState::load(&data_home)?;
            state.observe_thread(&thread)?;
            let state_path = state.save(&data_home)?;
            print_json(&serde_json::json!({
                "conversation_id": thread.thread_id,
                "provider_revision": thread.update_time,
                "provider_status": thread.status,
                "observed_at": thread.observed_at,
                "state_path": state_path
            }))?;
        }
        Command::ChatgptRecordRepairFailure { thread_id, code } => {
            let code: chat_history_core::continuation::RepairFailureCode =
                serde_json::from_value(serde_json::json!(code))
                    .map_err(|_| anyhow::anyhow!("Unknown repair failure code"))?;
            let mut state = ChatGptSyncState::load(&data_home)?;
            let recorded = state.record_repair_failure(&thread_id, code);
            if recorded {
                state.save(&data_home)?;
            }
            print_json(
                &serde_json::json!({"recorded":recorded,"diagnostic":chat_history_core::continuation::RepairDiagnostic::new(code)}),
            )?;
        }
        Command::ChatgptPlanRecent { path, stdin_bytes } => {
            let snapshot: ChatGptThreadListSnapshot = read_json_document(&path, stdin_bytes)?;
            let mut state = ChatGptSyncState::load(&data_home)?;
            let plan = state.plan_recent(snapshot);
            let state_path = state.save(&data_home)?;
            let plan = plan?;
            print_json(&serde_json::json!({
                "plan": plan,
                "state_path": state_path,
                "state": state,
            }))?;
        }
        Command::ChatgptImportThread {
            path,
            stdin_bytes,
            embed,
        } => {
            let transcript: ChatGptBridgeTranscript = read_json_document(&path, stdin_bytes)?;
            let thread_id = transcript.thread_id.clone();
            let update_time = transcript.update_time;
            let normalized = transcript.into_normalized()?;
            let report = service
                .import_normalized(vec![normalized], Some(Path::new("chatgpt-app-bridge")))?;
            let embeddings_completed = if embed {
                service
                    .rebuild_embeddings(false, Some(vec![thread_id.clone()]), None)
                    .await?
            } else {
                0
            };
            let mut state = ChatGptSyncState::load(&data_home)?;
            state.mark_imported_at(&thread_id, update_time);
            let state_path = state.save(&data_home)?;
            print_json(&serde_json::json!({
                "thread_id": thread_id,
                "import": report,
                "embeddings_completed": embeddings_completed,
                "state_path": state_path,
                "state": state,
            }))?;
        }
        Command::ChatgptBlock { thread_id, reason } => {
            let mut state = ChatGptSyncState::load(&data_home)?;
            let blocked = state.mark_blocked(&thread_id, reason);
            let state_path = state.save(&data_home)?;
            print_json(&serde_json::json!({
                "blocked": blocked,
                "state_path": state_path,
                "state": state,
            }))?;
        }
        Command::ChatgptSeedCursor { update_time } => {
            let mut state = ChatGptSyncState::load(&data_home)?;
            state.seed_cursor(update_time)?;
            let state_path = state.save(&data_home)?;
            print_json(&serde_json::json!({
                "state_path": state_path,
                "state": state,
            }))?;
        }
        Command::ChatgptSeedFromIndex => {
            let database = data_home.paths().db_path;
            drop(chat_history_core::db::open_database(&database)?);
            let health = chat_history_core::db::inspect_database(&database)?;
            let newest = health
                .sources
                .get("chatgpt")
                .and_then(|source| source.newest_update_time)
                .context("no indexed ChatGPT conversations are available to seed the cursor")?;
            let mut state = ChatGptSyncState::load(&data_home)?;
            state.seed_cursor(newest)?;
            let state_path = state.save(&data_home)?;
            print_json(&serde_json::json!({
                "seeded_from": "indexed-chatgpt-source",
                "update_time": newest,
                "state_path": state_path,
                "state": state,
            }))?;
        }
        Command::ChatgptBootstrapExport { archive, embed } => {
            let paths = data_home.paths();
            paths.ensure()?;
            let backup_path = if paths.db_path.exists() {
                let backup_dir = paths.db_dir.join("backups");
                fs::create_dir_all(&backup_dir)?;
                let stamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
                let output = backup_dir.join(format!("pre-chatgpt-bootstrap-{stamp}.sqlite3"));
                chat_history_core::db::backup_database(&paths.db_path, &output, false)?;
                Some(output)
            } else {
                None
            };

            let import = service
                .import_archive(ImportOptions {
                    source_archive: archive,
                    mode: ImportMode::Copy,
                    run_api_jobs: false,
                    force_summaries: false,
                    force_embeddings: false,
                })
                .await?;
            let embeddings_completed = if embed {
                service.rebuild_embeddings(false, None, None).await?
            } else {
                0
            };
            let health = chat_history_core::db::inspect_database(&paths.db_path)?;
            let chatgpt = health
                .sources
                .get("chatgpt")
                .context("OpenAI export imported no ChatGPT conversations")?;
            let newest = chatgpt
                .newest_update_time
                .context("indexed ChatGPT source has no update timestamp")?;
            let mut state = ChatGptSyncState::load(&data_home)?;
            state.seed_cursor(newest)?;
            let state_path = state.save(&data_home)?;
            print_json(&serde_json::json!({
                "status": "ok",
                "backup_path": backup_path,
                "import": import,
                "embeddings_completed": embeddings_completed,
                "chatgpt_source": chatgpt,
                "state_path": state_path,
                "state": state,
            }))?;
        }
        Command::MemoryCompileConversation {
            project,
            conversation_id,
            max_messages,
        } => {
            let model = MemoryModelClient::from_env()?;
            let result = service
                .compile_memory_conversation(&model, &project, &conversation_id, max_messages)
                .await?;
            print_json(&serde_json::json!({
                "status": if result.is_some() { "staged" } else { "caught_up" },
                "result": result,
            }))?;
        }
        Command::MemoryCompileManualExport {
            project,
            conversation_id,
            max_messages,
            bundle_out,
            prompt_out,
            overwrite,
        } => {
            anyhow::ensure!(
                bundle_out != prompt_out,
                "--bundle-out and --prompt-out must be different paths"
            );
            anyhow::ensure!(
                bundle_out != Path::new("-") && prompt_out != Path::new("-"),
                "manual compiler export requires real output file paths"
            );
            let Some(bundle) = service.prepare_manual_memory_compiler_bundle(
                &project,
                &conversation_id,
                max_messages,
            )?
            else {
                print_json(&serde_json::json!({
                    "status": "caught_up",
                    "project": project,
                    "conversation_id": conversation_id,
                }))?;
                return Ok(());
            };
            ensure_output_paths_available(&[&bundle_out, &prompt_out], overwrite)?;
            write_output_file(&bundle_out, &serde_json::to_vec_pretty(&bundle)?)?;
            write_output_file(&prompt_out, bundle.prompt.as_bytes())?;
            print_json(&serde_json::json!({
                "status": "exported",
                "project": bundle.input.project,
                "conversation_id": bundle.input.conversation_id,
                "source_snapshot_id": bundle.input.source_snapshot_id,
                "through_turn_index": bundle.input.through_turn_index,
                "prompt_sha256": bundle.prompt_sha256,
                "bundle_path": bundle_out,
                "prompt_path": prompt_out,
            }))?;
        }
        Command::MemoryCompileManualStage {
            bundle,
            response,
            model_label,
        } => {
            let bundle: MemoryCompilerManualBundle = read_json_document(&bundle, None)?;
            let output_json = read_input_text(&response, None)?;
            let created_at = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs_f64();
            let staged = service.stage_manual_memory_compiler_output(
                &bundle,
                &model_label,
                &output_json,
                created_at,
            )?;
            print_json(&serde_json::json!({
                "status": "staged",
                "project": bundle.input.project,
                "conversation_id": bundle.input.conversation_id,
                "source_snapshot_id": bundle.input.source_snapshot_id,
                "through_turn_index": bundle.input.through_turn_index,
                "model_label": model_label,
                "staged": staged,
            }))?;
        }
        Command::MemoryCompileProject {
            project,
            scan_limit,
            max_conversations,
            max_messages,
        } => {
            let model = MemoryModelClient::from_env()?;
            let result = service
                .compile_memory_project(
                    &model,
                    &project,
                    scan_limit,
                    max_conversations,
                    max_messages,
                )
                .await?;
            print_json(&result)?;
        }
        Command::MemoryBootstrapPlan {
            project,
            max_conversations,
            max_messages,
        } => {
            let plan = service.plan_memory_bootstrap(&project, max_conversations, max_messages)?;
            print_json(&plan)?;
        }
        Command::MemoryCompilePlan {
            project,
            scan_limit,
            max_conversations,
            max_messages,
        } => {
            let plan = service
                .plan_memory_project(&project, scan_limit, max_conversations, max_messages)
                .await?;
            print_json(&plan)?;
        }
        Command::MemoryProjectMatch {
            project,
            conversation_id,
        } => {
            let strong_match =
                service.conversation_matches_project_strong(&conversation_id, &project)?;
            print_json(&serde_json::json!({
                "project": project,
                "conversation_id": conversation_id,
                "strong_match": strong_match,
            }))?;
        }
        Command::MemoryProjectAliases { project } => {
            print_json(&serde_json::json!({
                "project": project,
                "aliases": service.project_aliases(&project)?,
            }))?;
        }
        Command::MemoryProjectAliasAdd { project, alias } => {
            print_json(&serde_json::json!({
                "project": project,
                "aliases": service.add_project_alias(&project, &alias)?,
            }))?;
        }
        Command::MemoryProjectAliasRemove { project, alias } => {
            let removed = service.remove_project_alias(&project, &alias)?;
            print_json(&serde_json::json!({
                "project": project,
                "removed": removed,
                "aliases": service.project_aliases(&project)?,
            }))?;
        }
        Command::MemoryCandidates { project } => {
            let candidates = service.pending_memory_candidates(&project)?;
            print_json(&serde_json::json!({
                "project": project,
                "pending": candidates,
            }))?;
        }
        Command::MemoryCandidate { candidate_id } => {
            let candidate = service
                .memory_candidate(&candidate_id)?
                .with_context(|| format!("memory candidate does not exist: {candidate_id}"))?;
            print_json(&candidate)?;
        }
        Command::MemoryCandidateReviews { candidate_id } => {
            service
                .memory_candidate(&candidate_id)?
                .with_context(|| format!("memory candidate does not exist: {candidate_id}"))?;
            let reviews = service.memory_candidate_reviews(&candidate_id)?;
            print_json(&serde_json::json!({
                "candidate_id": candidate_id,
                "reviews": reviews,
            }))?;
        }
        Command::MemoryCandidatePromote {
            candidate_id,
            reason,
            evidence,
        } => {
            let decided_at = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs_f64();
            let review = MemoryPromotionReview {
                reason: Some(reason),
                evidence: parse_promotion_evidence(&evidence, decided_at)?,
            };
            let decision =
                service.promote_memory_candidate_with_review(&candidate_id, decided_at, &review)?;
            let candidate = service
                .memory_candidate(&candidate_id)?
                .with_context(|| format!("memory candidate disappeared: {candidate_id}"))?;
            print_json(&serde_json::json!({
                "decision": decision,
                "candidate": candidate,
            }))?;
        }
        Command::MemoryCandidateReject {
            candidate_id,
            reason,
        } => {
            let decided_at = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs_f64();
            service.reject_memory_candidate(&candidate_id, &reason, decided_at)?;
            let candidate = service
                .memory_candidate(&candidate_id)?
                .with_context(|| format!("memory candidate disappeared: {candidate_id}"))?;
            print_json(&candidate)?;
        }
        Command::MemoryHealth {
            project,
            stale_after_days,
        } => {
            let report = service.memory_health(&project, stale_after_days)?;
            print_json(&report)?;
        }
        Command::MemoryAutoPromotionPlan { project } => {
            let plan = service.memory_auto_promotion_plan(&project)?;
            print_json(&plan)?;
        }
        Command::MemoryCollaborationList => {
            print_json(&service.collaboration_memory()?)?;
        }
        Command::MemoryCollaborationAuthor {
            kind,
            key,
            value,
            value_json,
            importance,
            confidence,
            supersedes,
            reason,
            evidence,
        } => {
            let authored_at = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs_f64();
            let value = parse_memory_value(value, value_json)?;
            let item = service.author_collaboration_memory(&CollaborationMemoryAuthoringInput {
                kind: MemoryKind::parse(kind.trim())?,
                key,
                value,
                importance,
                confidence,
                valid_from: None,
                valid_until: None,
                supersedes_memory_id: supersedes,
                review_reason: reason,
                evidence: parse_memory_evidence(
                    &evidence,
                    authored_at,
                    "operator_collaboration_authoring_cli",
                )?,
                authored_at,
            })?;
            print_json(&item)?;
        }
        Command::MemoryCollaborationRetire {
            memory_id,
            reason,
            evidence,
        } => {
            let retired_at = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs_f64();
            let item =
                service.retire_collaboration_memory(&CollaborationMemoryRetirementInput {
                    memory_id,
                    review_reason: reason,
                    evidence: parse_memory_evidence(
                        &evidence,
                        retired_at,
                        "operator_collaboration_retirement_cli",
                    )?,
                    retired_at,
                })?;
            print_json(&item)?;
        }
        Command::MemoryProjectConfirm {
            project,
            kind,
            key,
            value,
            value_json,
            importance,
            confidence,
            supersedes,
            reason,
            user_confirmation,
            evidence,
        } => {
            let confirmed_at = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs_f64();
            let value = parse_memory_value(value, value_json)?;
            let mut parsed_evidence = vec![MemoryEvidence {
                kind: MemoryEvidenceKind::UserStatement,
                reference: user_confirmation,
                detail: serde_json::json!({
                    "source": "operator_project_confirmation_cli",
                }),
                created_at: confirmed_at,
            }];
            parsed_evidence.extend(parse_memory_evidence(
                &evidence,
                confirmed_at,
                "operator_project_confirmation_cli",
            )?);
            let item = service.confirm_project_memory(&ProjectMemoryConfirmationInput {
                project,
                kind: MemoryKind::parse(kind.trim())?,
                key,
                value,
                importance,
                confidence,
                valid_from: None,
                valid_until: None,
                supersedes_memory_id: supersedes,
                review_reason: reason,
                evidence: parsed_evidence,
                confirmed_at,
            })?;
            print_json(&item)?;
        }
        Command::MemoryProjectRetire {
            project,
            memory_id,
            reason,
            user_confirmation,
            evidence,
        } => {
            let retired_at = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs_f64();
            let mut parsed_evidence = vec![MemoryEvidence {
                kind: MemoryEvidenceKind::UserStatement,
                reference: user_confirmation,
                detail: serde_json::json!({
                    "source": "operator_project_confirmation_cli",
                }),
                created_at: retired_at,
            }];
            parsed_evidence.extend(parse_memory_evidence(
                &evidence,
                retired_at,
                "operator_project_confirmation_cli",
            )?);
            let item = service.retire_confirmed_project_memory(&ProjectMemoryRetirementInput {
                project,
                memory_id,
                review_reason: reason,
                evidence: parsed_evidence,
                retired_at,
            })?;
            print_json(&item)?;
        }
        Command::MemoryProjectOperationalSet {
            project,
            kind,
            key,
            value,
            value_json,
            importance,
            confidence,
            supersedes,
            reason,
            evidence,
        } => {
            let set_at = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs_f64();
            let item =
                service.set_project_operational_memory(&ProjectOperationalMemorySetInput {
                    project,
                    kind: MemoryKind::parse(&kind)?,
                    key,
                    value: parse_memory_value(value, value_json)?,
                    importance,
                    confidence,
                    supersedes_memory_id: supersedes,
                    review_reason: reason,
                    evidence: parse_memory_evidence(
                        &evidence,
                        set_at,
                        "operator_project_operational_cli",
                    )?,
                    set_at,
                })?;
            print_json(&item)?;
        }
        Command::MemoryProjectOperationalResolve {
            project,
            memory_id,
            reason,
            evidence,
        } => {
            let transitioned_at = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs_f64();
            let item = service.resolve_project_operational_memory(
                &ProjectOperationalMemoryTransitionInput {
                    project,
                    memory_id,
                    review_reason: reason,
                    evidence: parse_memory_evidence(
                        &evidence,
                        transitioned_at,
                        "operator_project_operational_cli",
                    )?,
                    transitioned_at,
                },
            )?;
            print_json(&item)?;
        }
        Command::MemoryProjectOperationalArchive {
            project,
            memory_id,
            reason,
            evidence,
        } => {
            let transitioned_at = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs_f64();
            let item = service.archive_project_operational_memory(
                &ProjectOperationalMemoryTransitionInput {
                    project,
                    memory_id,
                    review_reason: reason,
                    evidence: parse_memory_evidence(
                        &evidence,
                        transitioned_at,
                        "operator_project_operational_cli",
                    )?,
                    transitioned_at,
                },
            )?;
            print_json(&item)?;
        }
        Command::Resume => {
            let report = service.resume().await?;
            print_json(&report)?;
        }
        Command::Stats => {
            let stats = service.stats()?;
            print_json(&stats)?;
        }
        Command::Search {
            query,
            mode,
            date_from,
            date_to,
            model,
            sources,
            risk_flag,
            topic_tag,
            limit,
        } => {
            let results = service
                .search(SearchOptions {
                    query,
                    mode: mode.map(Into::into),
                    date_from,
                    date_to,
                    model,
                    sources,
                    risk_flags: risk_flag,
                    topic_tags: topic_tag,
                    limit,
                    sort: None,
                })
                .await?;
            print_json(&results)?;
        }
        Command::Show {
            conversation_id,
            include_raw,
        } => {
            let detail = service
                .get_conversation(&conversation_id, include_raw)?
                .context("conversation not found")?;
            print_json(&detail)?;
        }
        Command::Related {
            conversation_id,
            limit,
        } => {
            let results = service.related_conversations(&conversation_id, limit)?;
            print_json(&results)?;
        }
        Command::SummarizeMissing { force, limit } => {
            let count = service.rebuild_summaries(force, None, limit).await?;
            print_json(&serde_json::json!({ "summaries_completed": count }))?;
        }
        Command::EmbedMissing { force, limit } => {
            let count = service.rebuild_embeddings(force, None, limit).await?;
            print_json(&serde_json::json!({ "embeddings_completed": count }))?;
        }
        Command::ReindexFts => {
            service.reindex_fts()?;
            print_json(&serde_json::json!({ "status": "ok" }))?;
        }
        Command::Backup { output, overwrite } => {
            let paths = data_home.paths();
            paths.ensure()?;
            let health =
                chat_history_core::db::backup_database(&paths.db_path, &output, overwrite)?;
            print_json(&serde_json::json!({
                "status": "ok",
                "backup_path": output,
                "health": health,
            }))?;
        }
        Command::Restore {
            input,
            confirm_writer_stopped,
        } => {
            anyhow::ensure!(
                confirm_writer_stopped,
                "restore requires --confirm-writer-stopped to prevent split-brain writes"
            );
            let paths = data_home.paths();
            paths.ensure()?;
            let report = chat_history_core::db::restore_database(&input, &paths.db_path)?;
            print_json(&report)?;
        }
        Command::Doctor => {
            let database = data_home.paths().db_path;
            drop(chat_history_core::db::open_database(&database)?);
            let health = chat_history_core::db::inspect_database(&database)?;
            print_json(&health)?;
        }
    }
    Ok(())
}

fn default_export_path() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("Downloads")
        .join("openai-export.zip")
}

fn print_json<T: serde::Serialize>(value: &T) -> anyhow::Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

fn ensure_output_paths_available(paths: &[&Path], overwrite: bool) -> anyhow::Result<()> {
    if overwrite {
        return Ok(());
    }
    for path in paths {
        anyhow::ensure!(
            !path.exists(),
            "output file already exists: {}; pass --overwrite to replace it",
            path.display()
        );
    }
    Ok(())
}

fn write_output_file(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, bytes).with_context(|| format!("writing {}", path.display()))
}

fn parse_promotion_evidence(
    values: &[String],
    created_at: f64,
) -> anyhow::Result<Vec<MemoryEvidence>> {
    parse_memory_evidence(values, created_at, "operator_promotion")
}

fn parse_memory_evidence(
    values: &[String],
    created_at: f64,
    source: &str,
) -> anyhow::Result<Vec<MemoryEvidence>> {
    values
        .iter()
        .map(|value| {
            let (kind, reference) = value.split_once(':').with_context(|| {
                format!("invalid memory evidence {value:?}; expected KIND:REFERENCE")
            })?;
            anyhow::ensure!(
                !reference.trim().is_empty(),
                "memory evidence reference cannot be empty"
            );
            Ok(MemoryEvidence {
                kind: MemoryEvidenceKind::parse(kind.trim())?,
                reference: reference.trim().to_string(),
                detail: serde_json::json!({ "source": source }),
                created_at,
            })
        })
        .collect()
}

fn parse_memory_value(
    value: Option<String>,
    value_json: Option<String>,
) -> anyhow::Result<serde_json::Value> {
    match (value, value_json) {
        (Some(value), None) => Ok(serde_json::Value::String(value)),
        (None, Some(value_json)) => {
            serde_json::from_str(&value_json).context("parsing --value-json")
        }
        _ => anyhow::bail!("exactly one of --value or --value-json is required"),
    }
}

fn parse_since(value: &str) -> anyhow::Result<f64> {
    if let Ok(epoch) = value.parse::<f64>() {
        return Ok(epoch);
    }
    let normalized = if value.len() == 10 {
        format!("{value}T00:00:00Z")
    } else {
        value.to_string()
    };
    Ok(chrono::DateTime::parse_from_rfc3339(&normalized)?.timestamp_millis() as f64 / 1000.0)
}

fn read_normalized(
    path: &Path,
    stdin_bytes: Option<u64>,
) -> anyhow::Result<Vec<NormalizedConversation>> {
    let text = read_input_text(path, stdin_bytes)?;
    if let Ok(items) = serde_json::from_str::<Vec<NormalizedConversation>>(&text) {
        return Ok(items);
    }
    if let Ok(item) = serde_json::from_str::<NormalizedConversation>(&text) {
        return Ok(vec![item]);
    }
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .map(serde_json::from_str)
        .collect::<Result<Vec<_>, _>>()
        .map_err(Into::into)
}

fn read_json_document<T: serde::de::DeserializeOwned>(
    path: &Path,
    stdin_bytes: Option<u64>,
) -> anyhow::Result<T> {
    let text = read_input_text(path, stdin_bytes)?;
    serde_json::from_str(&text)
        .with_context(|| format!("parsing JSON input from {}", path.display()))
}

fn read_input_text(path: &Path, stdin_bytes: Option<u64>) -> anyhow::Result<String> {
    if path != Path::new("-") {
        return fs::read_to_string(path).map_err(Into::into);
    }
    let mut text = String::new();
    match stdin_bytes {
        Some(length) => {
            let read = io::stdin().take(length).read_to_string(&mut text)? as u64;
            anyhow::ensure!(
                read == length,
                "expected {length} stdin bytes, received {read}"
            );
        }
        None => {
            io::stdin().read_to_string(&mut text)?;
        }
    }
    Ok(text)
}

#[derive(Debug, serde::Serialize)]
struct CodexImportSummary {
    rollouts_discovered: usize,
    conversations_indexed: usize,
    messages_indexed: usize,
    skipped: usize,
    errors: Vec<String>,
}

#[derive(Debug, serde::Serialize)]
struct CodexExportSummary {
    rollouts_discovered: usize,
    conversations: Vec<NormalizedConversation>,
    skipped: usize,
    errors: Vec<String>,
}

#[derive(Debug, serde::Serialize)]
struct ProviderSyncSummary {
    files_discovered: usize,
    conversations_indexed: usize,
    messages_indexed: usize,
    skipped: usize,
    errors: Vec<String>,
}

fn codex_roots(roots: Vec<PathBuf>) -> Vec<PathBuf> {
    if !roots.is_empty() {
        return roots;
    }
    let base = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    vec![
        base.join(".codex/sessions"),
        base.join(".codex/archived_sessions"),
    ]
}

fn gemini_roots(roots: Vec<PathBuf>) -> Vec<PathBuf> {
    if !roots.is_empty() {
        return roots;
    }
    let base = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    vec![base.join(".gemini/tmp")]
}

fn antigravity_roots(roots: Vec<PathBuf>) -> Vec<PathBuf> {
    if !roots.is_empty() {
        return roots;
    }
    let base = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    vec![
        base.join(".antigravity-cli/projects"),
        base.join(".gemini/antigravity-cli/brain"),
        base.join(".gemini/antigravity-ide/brain"),
        base.join(".gemini/antigravity/brain"),
    ]
}

type DiscoverProvider = fn(&[PathBuf]) -> anyhow::Result<Vec<PathBuf>>;
type ParseProvider = fn(&Path, Option<f64>) -> anyhow::Result<Option<NormalizedConversation>>;

#[allow(clippy::too_many_arguments)]
fn sync_provider(
    data_home: &DataHome,
    service: &IndexService,
    cursor_name: &str,
    source_label: &str,
    roots: Vec<PathBuf>,
    initial_since: String,
    overlap_hours: u64,
    batch_size: usize,
    discover: DiscoverProvider,
    parse: ParseProvider,
) -> anyhow::Result<ProviderSyncSummary> {
    let paths = data_home.paths();
    paths.ensure()?;
    let cursor_path = paths.cache_dir.join(cursor_name);
    let previous = read_cursor(&cursor_path)?;
    let started_at = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs_f64();
    let cutoff = previous
        .map(|epoch| epoch - overlap_hours as f64 * 3600.0)
        .unwrap_or(parse_since(&initial_since)?);
    let files = discover(&roots)?;
    let report = import_provider_paths(
        service,
        files,
        Some(cutoff),
        batch_size,
        source_label,
        parse,
    )?;
    if !report.errors.is_empty() {
        anyhow::bail!(
            "provider sync had {} parse errors; cursor was not advanced",
            report.errors.len()
        );
    }
    fs::write(
        &cursor_path,
        serde_json::to_vec_pretty(&serde_json::json!({
            "last_success_epoch": started_at,
            "last_success_utc": chrono::DateTime::from_timestamp(started_at as i64, 0).map(|value| value.to_rfc3339()),
        }))?,
    )?;
    Ok(report)
}

fn import_provider_paths(
    service: &IndexService,
    paths: Vec<PathBuf>,
    cutoff: Option<f64>,
    batch_size: usize,
    source_label: &str,
    parse: ParseProvider,
) -> anyhow::Result<ProviderSyncSummary> {
    let discovered = paths.len();
    let mut pending = Vec::new();
    let mut imported = 0usize;
    let mut messages = 0usize;
    let mut skipped = 0usize;
    let mut errors = Vec::new();
    let batch_size = batch_size.max(1);
    for path in paths {
        if cutoff.is_some_and(|value| file_mtime_epoch(&path).is_some_and(|mtime| mtime < value)) {
            skipped += 1;
            continue;
        }
        match parse(&path, cutoff) {
            Ok(Some(conversation)) => pending.push(conversation),
            Ok(None) => skipped += 1,
            Err(error) => errors.push(format!("{}: {error}", path.display())),
        }
        if pending.len() >= batch_size {
            let report = service.import_normalized_batch(
                std::mem::take(&mut pending),
                Some(Path::new(source_label)),
                false,
            )?;
            imported += report.conversations_indexed;
            messages += report.messages_indexed;
        }
    }
    if !pending.is_empty() {
        let report =
            service.import_normalized_batch(pending, Some(Path::new(source_label)), false)?;
        imported += report.conversations_indexed;
        messages += report.messages_indexed;
    }
    if imported > 0 {
        service.reindex_fts()?;
    }
    Ok(ProviderSyncSummary {
        files_discovered: discovered,
        conversations_indexed: imported,
        messages_indexed: messages,
        skipped,
        errors,
    })
}

fn import_codex_paths(
    service: &IndexService,
    paths: Vec<PathBuf>,
    cutoff: Option<f64>,
    batch_size: usize,
    prefilter_mtime: bool,
) -> anyhow::Result<CodexImportSummary> {
    let discovered = paths.len();
    let mut pending = Vec::new();
    let mut imported = 0usize;
    let mut messages = 0usize;
    let mut skipped = 0usize;
    let mut errors = Vec::new();
    let batch_size = batch_size.max(1);
    for path in paths {
        if prefilter_mtime
            && cutoff
                .is_some_and(|value| file_mtime_epoch(&path).is_some_and(|mtime| mtime < value))
        {
            skipped += 1;
            continue;
        }
        match chat_history_core::codex::parse_rollout(&path, cutoff) {
            Ok(Some(conversation)) => pending.push(conversation),
            Ok(None) => skipped += 1,
            Err(error) => errors.push(format!("{}: {error}", path.display())),
        }
        if pending.len() >= batch_size {
            let report = service.import_normalized_batch(
                std::mem::take(&mut pending),
                Some(Path::new("codex-rollouts")),
                false,
            )?;
            imported += report.conversations_indexed;
            messages += report.messages_indexed;
        }
    }
    if !pending.is_empty() {
        let report =
            service.import_normalized_batch(pending, Some(Path::new("codex-rollouts")), false)?;
        imported += report.conversations_indexed;
        messages += report.messages_indexed;
    }
    if imported > 0 {
        service.reindex_fts()?;
    }
    Ok(CodexImportSummary {
        rollouts_discovered: discovered,
        conversations_indexed: imported,
        messages_indexed: messages,
        skipped,
        errors,
    })
}

fn file_mtime_epoch(path: &Path) -> Option<f64> {
    fs::metadata(path)
        .ok()?
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|value| value.as_secs_f64())
}

fn read_cursor(path: &Path) -> anyhow::Result<Option<f64>> {
    if !path.exists() {
        return Ok(None);
    }
    let value: serde_json::Value = serde_json::from_slice(&fs::read(path)?)?;
    Ok(value
        .get("last_success_epoch")
        .and_then(serde_json::Value::as_f64))
}

fn rollout_id_from_path(path: &Path) -> Option<&str> {
    let stem = path.file_stem()?.to_str()?;
    let start = stem.len().checked_sub(36)?;
    let candidate = &stem[start..];
    (candidate.as_bytes().get(8) == Some(&b'-')
        && candidate.as_bytes().get(13) == Some(&b'-')
        && candidate.as_bytes().get(18) == Some(&b'-')
        && candidate.as_bytes().get(23) == Some(&b'-'))
    .then_some(candidate)
}
