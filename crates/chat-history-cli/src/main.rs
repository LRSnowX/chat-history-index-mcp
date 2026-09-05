use std::{
    fs,
    io::{self, Read},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::Context;
use chat_history_core::{
    DataHome, ImportMode, ImportOptions, IndexService, NormalizedConversation, SearchMode,
    SearchOptions,
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
    let text = if path == Path::new("-") {
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
        text
    } else {
        fs::read_to_string(path)?
    };
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
