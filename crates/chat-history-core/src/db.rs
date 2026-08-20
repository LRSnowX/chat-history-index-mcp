use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::Context;
use chrono::Utc;
use rusqlite::{Connection, OpenFlags, OptionalExtension, backup::Backup, params};
use tempfile::NamedTempFile;

use crate::{
    models::{DatabaseHealth, IndexStats, JobKind, JobStatus, RestoreReport, SourceHealth},
    sql::SCHEMA,
};

const REQUIRED_TABLES: &[&str] = &["conversations", "messages", "attachments", "jobs"];

pub fn open_database(path: &Path) -> anyhow::Result<Connection> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let conn = Connection::open(path)
        .with_context(|| format!("opening database at {}", path.display()))?;
    conn.execute_batch(SCHEMA)?;
    migrate_conversation_sources(&conn)?;
    conn.pragma_update(None, "user_version", 1)?;
    Ok(conn)
}

pub fn backup_database(
    source: &Path,
    output: &Path,
    overwrite: bool,
) -> anyhow::Result<DatabaseHealth> {
    anyhow::ensure!(
        source.exists(),
        "database does not exist: {}",
        source.display()
    );
    drop(open_database(source)?);
    anyhow::ensure!(
        source != output,
        "backup output must differ from the live database"
    );
    if output.exists() && !overwrite {
        anyhow::bail!(
            "backup already exists: {} (use --overwrite to replace it)",
            output.display()
        );
    }
    let parent = output
        .parent()
        .context("backup output needs a parent directory")?;
    fs::create_dir_all(parent)?;

    let from = Connection::open_with_flags(source, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .with_context(|| format!("opening source database {}", source.display()))?;
    let mut temporary = NamedTempFile::new_in(parent)?;
    let mut to = Connection::open(temporary.path())?;
    {
        let backup = Backup::new(&from, &mut to)?;
        backup.run_to_completion(256, Duration::from_millis(10), None)?;
    }
    to.pragma_update(None, "journal_mode", "DELETE")?;
    to.close().map_err(|(_, error)| error)?;
    temporary.as_file_mut().sync_all()?;
    let health = inspect_database(temporary.path())?;
    anyhow::ensure!(
        health.integrity_check == "ok",
        "backup failed integrity_check"
    );
    if output.exists() {
        fs::remove_file(output)?;
    }
    let temporary_path = temporary.path().to_path_buf();
    temporary
        .persist(output)
        .map_err(|error| error.error)
        .with_context(|| format!("persisting backup to {}", output.display()))?;
    remove_sqlite_sidecars(&temporary_path)?;
    inspect_database(output)
}

pub fn restore_database(input: &Path, destination: &Path) -> anyhow::Result<RestoreReport> {
    anyhow::ensure!(
        input.exists(),
        "restore input does not exist: {}",
        input.display()
    );
    anyhow::ensure!(
        input != destination,
        "restore input must differ from the live database"
    );
    let input_health = inspect_database(input)?;
    anyhow::ensure!(
        input_health.integrity_check == "ok",
        "restore input failed integrity_check"
    );

    let parent = destination
        .parent()
        .context("database path needs a parent directory")?;
    fs::create_dir_all(parent)?;
    let previous_database_backup = if destination.exists() {
        let backups = parent.join("backups");
        fs::create_dir_all(&backups)?;
        let path = backups.join(format!(
            "pre-restore-{}.sqlite3",
            Utc::now().format("%Y%m%dT%H%M%SZ")
        ));
        backup_database(destination, &path, false)?;
        Some(path)
    } else {
        None
    };

    let from = Connection::open_with_flags(input, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let mut temporary = NamedTempFile::new_in(parent)?;
    let mut to = Connection::open(temporary.path())?;
    {
        let backup = Backup::new(&from, &mut to)?;
        backup.run_to_completion(256, Duration::from_millis(10), None)?;
    }
    to.pragma_update(None, "journal_mode", "DELETE")?;
    to.close().map_err(|(_, error)| error)?;
    temporary.as_file_mut().sync_all()?;
    anyhow::ensure!(
        inspect_database(temporary.path())?.integrity_check == "ok",
        "restored database failed integrity_check"
    );

    if destination.exists() {
        fs::remove_file(destination)?;
    }
    for suffix in ["-wal", "-shm"] {
        let sidecar = PathBuf::from(format!("{}{}", destination.display(), suffix));
        if sidecar.exists() {
            fs::remove_file(sidecar)?;
        }
    }
    let temporary_path = temporary.path().to_path_buf();
    temporary
        .persist(destination)
        .map_err(|error| error.error)
        .with_context(|| format!("restoring database to {}", destination.display()))?;
    remove_sqlite_sidecars(&temporary_path)?;
    drop(open_database(destination)?);
    let health = inspect_database(destination)?;
    Ok(RestoreReport {
        restored_from: input.to_path_buf(),
        database_path: destination.to_path_buf(),
        previous_database_backup,
        health,
    })
}

fn remove_sqlite_sidecars(path: &Path) -> anyhow::Result<()> {
    for suffix in ["-wal", "-shm"] {
        let sidecar = PathBuf::from(format!("{}{}", path.display(), suffix));
        if sidecar.exists() {
            fs::remove_file(sidecar)?;
        }
    }
    Ok(())
}

pub fn inspect_database(path: &Path) -> anyhow::Result<DatabaseHealth> {
    anyhow::ensure!(path.exists(), "database does not exist: {}", path.display());
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let integrity_check: String = conn.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
    let journal_mode: String = conn.query_row("PRAGMA journal_mode", [], |row| row.get(0))?;
    let schema_version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    for table in REQUIRED_TABLES {
        let found: i64 = conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
            [table],
            |row| row.get(0),
        )?;
        anyhow::ensure!(found == 1, "required table is missing: {table}");
    }
    let conversations =
        conn.query_row("SELECT COUNT(*) FROM conversations", [], |row| row.get(0))?;
    let messages = conn.query_row("SELECT COUNT(*) FROM messages", [], |row| row.get(0))?;
    let mut sources = BTreeMap::new();
    let mut stmt = conn.prepare(
        "SELECT c.source, COUNT(DISTINCT c.conversation_id), COUNT(m.message_id), MAX(c.update_time) \
         FROM conversations c LEFT JOIN messages m ON m.conversation_id = c.conversation_id \
         GROUP BY c.source ORDER BY c.source",
    )?;
    for row in stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            SourceHealth {
                conversations: row.get(1)?,
                messages: row.get(2)?,
                newest_update_time: row.get(3)?,
            },
        ))
    })? {
        let (source, health) = row?;
        sources.insert(source, health);
    }
    Ok(DatabaseHealth {
        database_path: path.to_path_buf(),
        schema_version,
        integrity_check,
        journal_mode,
        conversations,
        messages,
        sources,
    })
}

fn migrate_conversation_sources(conn: &Connection) -> anyhow::Result<()> {
    let mut stmt = conn.prepare("PRAGMA table_info(conversations)")?;
    let columns = stmt
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?;
    let additions = [
        (
            "source",
            "ALTER TABLE conversations ADD COLUMN source TEXT NOT NULL DEFAULT 'chatgpt'",
        ),
        (
            "source_instance",
            "ALTER TABLE conversations ADD COLUMN source_instance TEXT",
        ),
        (
            "source_conversation_id",
            "ALTER TABLE conversations ADD COLUMN source_conversation_id TEXT",
        ),
        (
            "source_url",
            "ALTER TABLE conversations ADD COLUMN source_url TEXT",
        ),
        (
            "source_path",
            "ALTER TABLE conversations ADD COLUMN source_path TEXT",
        ),
        (
            "ingested_at",
            "ALTER TABLE conversations ADD COLUMN ingested_at TEXT",
        ),
    ];
    for (name, sql) in additions {
        if !columns.iter().any(|column| column == name) {
            conn.execute(sql, [])?;
        }
    }
    conn.execute(
        "UPDATE conversations SET source_conversation_id = conversation_id WHERE source_conversation_id IS NULL",
        [],
    )?;
    conn.execute(
        "UPDATE conversations SET ingested_at = CURRENT_TIMESTAMP WHERE ingested_at IS NULL",
        [],
    )?;
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_conversations_source ON conversations(source, source_instance);\n\
         CREATE UNIQUE INDEX IF NOT EXISTS idx_conversations_source_identity ON conversations(source, COALESCE(source_instance, ''), source_conversation_id);",
    )?;
    Ok(())
}

pub fn upsert_job(
    conn: &Connection,
    conversation_id: &str,
    kind: JobKind,
    status: JobStatus,
    last_error: Option<&str>,
    attempts_delta: i64,
) -> anyhow::Result<()> {
    conn.execute(
        r#"
        INSERT INTO jobs (conversation_id, kind, status, attempts, last_error)
        VALUES (?1, ?2, ?3, ?4, ?5)
        ON CONFLICT(conversation_id, kind) DO UPDATE SET
          status = excluded.status,
          attempts = jobs.attempts + excluded.attempts,
          last_error = excluded.last_error,
          updated_at = CURRENT_TIMESTAMP,
          completed_at = CASE WHEN excluded.status = 'complete' THEN CURRENT_TIMESTAMP ELSE jobs.completed_at END
        "#,
        params![conversation_id, kind.as_str(), status.as_str(), attempts_delta, last_error],
    )?;
    Ok(())
}

pub fn fetch_stats(conn: &Connection) -> anyhow::Result<IndexStats> {
    let conversations: i64 =
        conn.query_row("SELECT COUNT(*) FROM conversations", [], |row| row.get(0))?;
    let messages: i64 = conn.query_row("SELECT COUNT(*) FROM messages", [], |row| row.get(0))?;
    let attachments: i64 =
        conn.query_row("SELECT COUNT(*) FROM attachments", [], |row| row.get(0))?;
    let summaries_complete: i64 = conn.query_row(
        "SELECT COUNT(*) FROM jobs WHERE kind = 'summary' AND status = 'complete'",
        [],
        |row| row.get(0),
    )?;
    let embeddings_complete: i64 = conn.query_row(
        "SELECT COUNT(*) FROM jobs WHERE kind = 'embedding' AND status = 'complete'",
        [],
        |row| row.get(0),
    )?;
    let latest_update_time = conn
        .query_row("SELECT MAX(update_time) FROM conversations", [], |row| {
            row.get(0)
        })
        .optional()?
        .flatten();
    let archive_path: Option<String> = conn
        .query_row(
            "SELECT archive_path FROM archives ORDER BY id DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .optional()?;
    let mut conversations_by_source = std::collections::BTreeMap::new();
    let mut stmt =
        conn.prepare("SELECT source, COUNT(*) FROM conversations GROUP BY source ORDER BY source")?;
    for row in stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
    })? {
        let (source, count) = row?;
        conversations_by_source.insert(source, count);
    }

    Ok(IndexStats {
        archive_path: archive_path.map(Into::into),
        conversations,
        messages,
        attachments,
        summaries_complete,
        embeddings_complete,
        latest_update_time,
        conversations_by_source,
    })
}
