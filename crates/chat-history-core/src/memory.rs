use anyhow::{Context, anyhow, ensure};
use chrono::Utc;
use rusqlite::{OptionalExtension, Transaction, params};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{db::open_database, ingest::IndexService};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MemoryScope {
    Global,
    Project { project: String },
}

impl MemoryScope {
    fn db_parts(&self) -> (&'static str, &str) {
        match self {
            Self::Global => ("global", ""),
            Self::Project { project } => ("project", project.as_str()),
        }
    }

    fn from_db(scope_type: &str, scope_id: String) -> anyhow::Result<Self> {
        match scope_type {
            "global" => Ok(Self::Global),
            "project" => Ok(Self::Project { project: scope_id }),
            other => Err(anyhow!("unknown memory scope type: {other}")),
        }
    }
}

macro_rules! string_enum {
    ($name:ident { $($variant:ident => $value:literal),+ $(,)? }) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
        #[serde(rename_all = "snake_case")]
        pub enum $name {
            $($variant),+
        }

        impl $name {
            pub fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $value),+
                }
            }

            fn from_db(value: &str) -> anyhow::Result<Self> {
                match value {
                    $($value => Ok(Self::$variant),)+
                    other => Err(anyhow!(
                        "unknown {} value: {other}",
                        stringify!($name)
                    )),
                }
            }
        }
    };
}

string_enum!(MemoryKind {
    Invariant => "invariant",
    Preference => "preference",
    Decision => "decision",
    State => "state",
    Blocker => "blocker",
    Task => "task",
    Result => "result",
    Hypothesis => "hypothesis",
    ArtifactReference => "artifact_reference",
});

string_enum!(MemoryStatus {
    Active => "active",
    Resolved => "resolved",
    Superseded => "superseded",
    Archived => "archived",
});

string_enum!(MemoryEvidenceKind {
    UserStatement => "user_statement",
    ConversationTurn => "conversation_turn",
    Document => "document",
    GitCommit => "git_commit",
    RepositoryState => "repository_state",
    DevspaceResult => "devspace_result",
});

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MemoryEvidence {
    pub kind: MemoryEvidenceKind,
    pub reference: String,
    #[serde(default)]
    pub detail: Value,
    pub created_at: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MemoryItem {
    pub memory_id: String,
    pub scope: MemoryScope,
    pub kind: MemoryKind,
    pub key: String,
    pub value: Value,
    pub status: MemoryStatus,
    pub importance: u8,
    pub confidence: f64,
    pub valid_from: Option<f64>,
    pub valid_until: Option<f64>,
    pub supersedes_memory_id: Option<String>,
    pub created_at: f64,
    pub updated_at: f64,
    pub last_verified_at: Option<f64>,
    #[serde(default)]
    pub evidence: Vec<MemoryEvidence>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ProjectWorkingMemory {
    pub project: String,
    pub generated_at: f64,
    pub items: Vec<MemoryItem>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CollaborationMemory {
    pub generated_at: f64,
    pub items: Vec<MemoryItem>,
}

impl IndexService {
    pub fn put_memory_item(&self, item: &MemoryItem) -> anyhow::Result<()> {
        let conn = open_database(&self.data_home.paths().db_path)?;
        let tx = conn.unchecked_transaction()?;
        put_memory_item_tx(&tx, item)?;
        tx.commit()?;
        Ok(())
    }

    pub fn get_memory_item(&self, memory_id: &str) -> anyhow::Result<Option<MemoryItem>> {
        let conn = open_database(&self.data_home.paths().db_path)?;
        load_memory_item(&conn, memory_id)
    }

    pub fn project_working_memory(&self, project: &str) -> anyhow::Result<ProjectWorkingMemory> {
        ensure!(
            !project.trim().is_empty(),
            "project memory scope cannot be empty"
        );
        let conn = open_database(&self.data_home.paths().db_path)?;
        let mut stmt = conn.prepare(
            r#"
            SELECT memory_id
            FROM memory_items
            WHERE scope_type = 'project' AND scope_id = ?1 AND status = 'active'
            ORDER BY importance DESC, updated_at DESC, memory_id ASC
            "#,
        )?;
        let ids = stmt
            .query_map(params![project], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        let mut items = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(item) = load_memory_item(&conn, &id)? {
                items.push(item);
            }
        }
        Ok(ProjectWorkingMemory {
            project: project.to_string(),
            generated_at: now_epoch(),
            items,
        })
    }

    pub fn collaboration_memory(&self) -> anyhow::Result<CollaborationMemory> {
        let conn = open_database(&self.data_home.paths().db_path)?;
        let mut stmt = conn.prepare(
            r#"
            SELECT memory_id
            FROM memory_items
            WHERE scope_type = 'global'
              AND scope_id = ''
              AND status = 'active'
              AND kind IN ('invariant', 'preference', 'decision')
            ORDER BY importance DESC, updated_at DESC, memory_id ASC
            "#,
        )?;
        let ids = stmt
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        let mut items = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(item) = load_memory_item(&conn, &id)? {
                items.push(item);
            }
        }
        Ok(CollaborationMemory {
            generated_at: now_epoch(),
            items,
        })
    }
}

pub(crate) fn put_memory_item_tx(tx: &Transaction<'_>, item: &MemoryItem) -> anyhow::Result<()> {
    validate_memory_item(item)?;
    let (scope_type, scope_id) = item.scope.db_parts();

    let existing_identity: Option<(String, String, String, Option<String>)> = tx
        .query_row(
            "SELECT scope_type, scope_id, memory_key, supersedes_memory_id FROM memory_items WHERE memory_id = ?1",
            params![item.memory_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?;
    if let Some((
        existing_scope_type,
        existing_scope_id,
        existing_key,
        existing_supersedes_memory_id,
    )) = existing_identity.as_ref()
    {
        ensure!(
            existing_scope_type == scope_type
                && existing_scope_id == scope_id
                && existing_key == &item.key,
            "memory identity is immutable for {}",
            item.memory_id
        );
        ensure!(
            existing_supersedes_memory_id.as_deref() == item.supersedes_memory_id.as_deref(),
            "memory supersession identity is immutable for {}",
            item.memory_id
        );
    }

    if let Some(supersedes) = item.supersedes_memory_id.as_deref() {
        ensure!(
            supersedes != item.memory_id,
            "memory item cannot supersede itself"
        );
        let old: Option<(String, String, String, String)> = tx
            .query_row(
                "SELECT scope_type, scope_id, memory_key, status FROM memory_items WHERE memory_id = ?1",
                params![supersedes],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()?;
        let Some((old_scope_type, old_scope_id, old_key, old_status)) = old else {
            return Err(anyhow!(
                "superseded memory item does not exist: {supersedes}"
            ));
        };
        ensure!(
            old_scope_type == scope_type && old_scope_id == scope_id && old_key == item.key,
            "superseded memory item must have the same scope and key"
        );
        if old_status == MemoryStatus::Active.as_str() {
            tx.execute(
                "UPDATE memory_items SET status = 'superseded', updated_at = ?2 WHERE memory_id = ?1",
                params![supersedes, item.updated_at],
            )?;
        } else {
            ensure!(
                old_status == MemoryStatus::Superseded.as_str() && existing_identity.is_some(),
                "only an active memory item can be superseded"
            );
        }
    }

    tx.execute(
        r#"
        INSERT INTO memory_items (
          memory_id, scope_type, scope_id, kind, memory_key, value_json, status,
          importance, confidence, valid_from, valid_until, supersedes_memory_id,
          created_at, updated_at, last_verified_at
        )
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
        ON CONFLICT(memory_id) DO UPDATE SET
          kind = excluded.kind,
          value_json = excluded.value_json,
          status = excluded.status,
          importance = excluded.importance,
          confidence = excluded.confidence,
          valid_from = excluded.valid_from,
          valid_until = excluded.valid_until,
          supersedes_memory_id = excluded.supersedes_memory_id,
          updated_at = excluded.updated_at,
          last_verified_at = excluded.last_verified_at
        "#,
        params![
            item.memory_id,
            scope_type,
            scope_id,
            item.kind.as_str(),
            item.key,
            serde_json::to_string(&item.value)?,
            item.status.as_str(),
            i64::from(item.importance),
            item.confidence,
            item.valid_from,
            item.valid_until,
            item.supersedes_memory_id,
            item.created_at,
            item.updated_at,
            item.last_verified_at,
        ],
    )
    .with_context(|| format!("writing memory item {}", item.memory_id))?;

    tx.execute(
        "DELETE FROM memory_evidence WHERE memory_id = ?1",
        params![item.memory_id],
    )?;
    for evidence in &item.evidence {
        tx.execute(
            r#"
            INSERT INTO memory_evidence (
              memory_id, evidence_kind, evidence_ref, detail_json, created_at
            )
            VALUES (?1, ?2, ?3, ?4, ?5)
            "#,
            params![
                item.memory_id,
                evidence.kind.as_str(),
                evidence.reference,
                serde_json::to_string(&evidence.detail)?,
                evidence.created_at,
            ],
        )?;
    }
    Ok(())
}

fn validate_memory_item(item: &MemoryItem) -> anyhow::Result<()> {
    ensure!(
        !item.memory_id.trim().is_empty(),
        "memory_id cannot be empty"
    );
    ensure!(!item.key.trim().is_empty(), "memory key cannot be empty");
    if let MemoryScope::Project { project } = &item.scope {
        ensure!(
            !project.trim().is_empty(),
            "project memory scope cannot be empty"
        );
    }
    ensure!(
        item.confidence.is_finite() && (0.0..=1.0).contains(&item.confidence),
        "memory confidence must be between 0 and 1"
    );
    ensure!(
        item.created_at.is_finite() && item.updated_at.is_finite(),
        "memory timestamps must be finite"
    );
    if let (Some(valid_from), Some(valid_until)) = (item.valid_from, item.valid_until) {
        ensure!(
            valid_until >= valid_from,
            "memory validity interval is inverted"
        );
    }
    for evidence in &item.evidence {
        ensure!(
            !evidence.reference.trim().is_empty(),
            "memory evidence reference cannot be empty"
        );
        ensure!(
            evidence.created_at.is_finite(),
            "memory evidence timestamp must be finite"
        );
    }
    Ok(())
}

fn load_memory_item(
    conn: &rusqlite::Connection,
    memory_id: &str,
) -> anyhow::Result<Option<MemoryItem>> {
    let row: Option<(
        String,
        String,
        String,
        String,
        String,
        String,
        i64,
        f64,
        Option<f64>,
        Option<f64>,
        Option<String>,
        f64,
        f64,
        Option<f64>,
    )> = conn
        .query_row(
            r#"
            SELECT scope_type, scope_id, kind, memory_key, value_json, status,
                   importance, confidence, valid_from, valid_until, supersedes_memory_id,
                   created_at, updated_at, last_verified_at
            FROM memory_items
            WHERE memory_id = ?1
            "#,
            params![memory_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                    row.get(8)?,
                    row.get(9)?,
                    row.get(10)?,
                    row.get(11)?,
                    row.get(12)?,
                    row.get(13)?,
                ))
            },
        )
        .optional()?;
    let Some((
        scope_type,
        scope_id,
        kind,
        key,
        value_json,
        status,
        importance,
        confidence,
        valid_from,
        valid_until,
        supersedes_memory_id,
        created_at,
        updated_at,
        last_verified_at,
    )) = row
    else {
        return Ok(None);
    };

    let mut stmt = conn.prepare(
        r#"
        SELECT evidence_kind, evidence_ref, detail_json, created_at
        FROM memory_evidence
        WHERE memory_id = ?1
        ORDER BY created_at ASC, evidence_kind ASC, evidence_ref ASC
        "#,
    )?;
    let evidence = stmt
        .query_map(params![memory_id], |row| {
            let kind: String = row.get(0)?;
            let detail_json: String = row.get(2)?;
            Ok((
                kind,
                row.get::<_, String>(1)?,
                detail_json,
                row.get::<_, f64>(3)?,
            ))
        })?
        .map(|row| -> anyhow::Result<MemoryEvidence> {
            let (kind, reference, detail_json, created_at) = row?;
            Ok(MemoryEvidence {
                kind: MemoryEvidenceKind::from_db(&kind)?,
                reference,
                detail: serde_json::from_str(&detail_json)?,
                created_at,
            })
        })
        .collect::<anyhow::Result<Vec<_>>>()?;

    Ok(Some(MemoryItem {
        memory_id: memory_id.to_string(),
        scope: MemoryScope::from_db(&scope_type, scope_id)?,
        kind: MemoryKind::from_db(&kind)?,
        key,
        value: serde_json::from_str(&value_json)?,
        status: MemoryStatus::from_db(&status)?,
        importance: u8::try_from(importance)
            .map_err(|_| anyhow!("invalid memory importance: {importance}"))?,
        confidence,
        valid_from,
        valid_until,
        supersedes_memory_id,
        created_at,
        updated_at,
        last_verified_at,
        evidence,
    }))
}

fn now_epoch() -> f64 {
    Utc::now().timestamp_millis() as f64 / 1000.0
}
