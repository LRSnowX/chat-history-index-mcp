use std::collections::BTreeSet;

use anyhow::{anyhow, ensure};
use rusqlite::{OptionalExtension, Transaction, params};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::{
    db::open_database,
    ingest::IndexService,
    memory::{
        MemoryEvidence, MemoryEvidenceKind, MemoryItem, MemoryKind, MemoryScope, MemoryStatus,
        put_memory_item_tx,
    },
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MemoryCandidateOperation {
    Add,
    Supersede,
    Resolve,
    Archive,
}

impl MemoryCandidateOperation {
    fn as_str(self) -> &'static str {
        match self {
            Self::Add => "add",
            Self::Supersede => "supersede",
            Self::Resolve => "resolve",
            Self::Archive => "archive",
        }
    }

    fn from_db(value: &str) -> anyhow::Result<Self> {
        match value {
            "add" => Ok(Self::Add),
            "supersede" => Ok(Self::Supersede),
            "resolve" => Ok(Self::Resolve),
            "archive" => Ok(Self::Archive),
            other => Err(anyhow!("unknown memory candidate operation: {other}")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MemoryCandidateStatus {
    Pending,
    Promoted,
    Rejected,
    Stale,
}

impl MemoryCandidateStatus {
    fn from_db(value: &str) -> anyhow::Result<Self> {
        match value {
            "pending" => Ok(Self::Pending),
            "promoted" => Ok(Self::Promoted),
            "rejected" => Ok(Self::Rejected),
            "stale" => Ok(Self::Stale),
            other => Err(anyhow!("unknown memory candidate status: {other}")),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MemoryCandidatePayload {
    Add {
        memory_id: String,
        kind: MemoryKind,
        key: String,
        value: Value,
        importance: u8,
        confidence: f64,
        valid_from: Option<f64>,
        valid_until: Option<f64>,
        last_verified_at: Option<f64>,
    },
    Supersede {
        memory_id: String,
        target_memory_id: String,
        kind: MemoryKind,
        key: String,
        value: Value,
        importance: u8,
        confidence: f64,
        valid_from: Option<f64>,
        valid_until: Option<f64>,
        last_verified_at: Option<f64>,
    },
    Resolve {
        target_memory_id: String,
    },
    Archive {
        target_memory_id: String,
    },
}

impl MemoryCandidatePayload {
    fn operation(&self) -> MemoryCandidateOperation {
        match self {
            Self::Add { .. } => MemoryCandidateOperation::Add,
            Self::Supersede { .. } => MemoryCandidateOperation::Supersede,
            Self::Resolve { .. } => MemoryCandidateOperation::Resolve,
            Self::Archive { .. } => MemoryCandidateOperation::Archive,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MemoryCandidateInput {
    pub candidate_id: String,
    pub payload: MemoryCandidatePayload,
    pub rationale: String,
    #[serde(default)]
    pub evidence: Vec<MemoryEvidence>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MemoryCompilationBatch {
    pub project: String,
    pub conversation_id: String,
    pub source_snapshot_id: String,
    pub through_turn_index: i64,
    pub through_message_id: String,
    pub compiler_version: String,
    pub model_label: Option<String>,
    pub created_at: f64,
    #[serde(default)]
    pub candidates: Vec<MemoryCandidateInput>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MemoryCompileCheckpoint {
    pub project: String,
    pub conversation_id: String,
    pub source_snapshot_id: String,
    pub through_turn_index: i64,
    pub through_message_id: String,
    pub prefix_sha256_hex: String,
    pub compiler_version: String,
    pub model_label: Option<String>,
    pub updated_at: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MemoryCandidate {
    pub candidate_id: String,
    pub project: String,
    pub conversation_id: String,
    pub source_snapshot_id: String,
    pub operation: MemoryCandidateOperation,
    pub payload: MemoryCandidatePayload,
    pub status: MemoryCandidateStatus,
    pub rationale: String,
    pub compiler_version: String,
    pub model_label: Option<String>,
    pub through_turn_index: i64,
    pub created_at: f64,
    pub decided_at: Option<f64>,
    pub decision_reason: Option<String>,
    pub promoted_memory_id: Option<String>,
    pub evidence: Vec<MemoryEvidence>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MemoryCompilationStageResult {
    pub checkpoint: MemoryCompileCheckpoint,
    pub candidate_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum MemoryCandidateDecision {
    Promoted {
        candidate_id: String,
        memory_id: String,
    },
    Stale {
        candidate_id: String,
        reason: String,
    },
}

impl IndexService {
    pub fn stage_memory_compilation(
        &self,
        batch: &MemoryCompilationBatch,
    ) -> anyhow::Result<MemoryCompilationStageResult> {
        validate_batch(batch)?;
        let conn = open_database(&self.data_home.paths().db_path)?;
        let tx = conn.unchecked_transaction()?;
        if let Some(result) = exact_compilation_replay(&tx, batch)? {
            return Ok(result);
        }
        validate_snapshot_boundary(&tx, batch)?;
        validate_checkpoint_progression(&tx, batch)?;
        let prefix_sha256_hex =
            snapshot_prefix_sha256_hex(&tx, &batch.source_snapshot_id, batch.through_turn_index)?;

        for candidate in &batch.candidates {
            stage_candidate(&tx, batch, candidate)?;
        }

        tx.execute(
            r#"
            INSERT INTO memory_compile_checkpoints (
              project, conversation_id, source_snapshot_id, through_turn_index,
              through_message_id, prefix_sha256_hex, compiler_version, model_label, updated_at
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
            ON CONFLICT(project, conversation_id) DO UPDATE SET
              source_snapshot_id = excluded.source_snapshot_id,
              through_turn_index = excluded.through_turn_index,
              through_message_id = excluded.through_message_id,
              prefix_sha256_hex = excluded.prefix_sha256_hex,
              compiler_version = excluded.compiler_version,
              model_label = excluded.model_label,
              updated_at = excluded.updated_at
            "#,
            params![
                batch.project,
                batch.conversation_id,
                batch.source_snapshot_id,
                batch.through_turn_index,
                batch.through_message_id,
                prefix_sha256_hex,
                batch.compiler_version,
                batch.model_label,
                batch.created_at,
            ],
        )?;
        tx.commit()?;

        Ok(MemoryCompilationStageResult {
            checkpoint: MemoryCompileCheckpoint {
                project: batch.project.clone(),
                conversation_id: batch.conversation_id.clone(),
                source_snapshot_id: batch.source_snapshot_id.clone(),
                through_turn_index: batch.through_turn_index,
                through_message_id: batch.through_message_id.clone(),
                prefix_sha256_hex,
                compiler_version: batch.compiler_version.clone(),
                model_label: batch.model_label.clone(),
                updated_at: batch.created_at,
            },
            candidate_ids: batch
                .candidates
                .iter()
                .map(|candidate| candidate.candidate_id.clone())
                .collect(),
        })
    }

    pub fn memory_compile_checkpoint(
        &self,
        project: &str,
        conversation_id: &str,
    ) -> anyhow::Result<Option<MemoryCompileCheckpoint>> {
        let conn = open_database(&self.data_home.paths().db_path)?;
        conn.query_row(
            r#"
            SELECT source_snapshot_id, through_turn_index, through_message_id,
                   prefix_sha256_hex, compiler_version, model_label, updated_at
            FROM memory_compile_checkpoints
            WHERE project = ?1 AND conversation_id = ?2
            "#,
            params![project, conversation_id],
            |row| {
                Ok(MemoryCompileCheckpoint {
                    project: project.to_string(),
                    conversation_id: conversation_id.to_string(),
                    source_snapshot_id: row.get(0)?,
                    through_turn_index: row.get(1)?,
                    through_message_id: row.get(2)?,
                    prefix_sha256_hex: row.get(3)?,
                    compiler_version: row.get(4)?,
                    model_label: row.get(5)?,
                    updated_at: row.get(6)?,
                })
            },
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn pending_memory_candidates(&self, project: &str) -> anyhow::Result<Vec<MemoryCandidate>> {
        ensure!(!project.trim().is_empty(), "project cannot be empty");
        let conn = open_database(&self.data_home.paths().db_path)?;
        let mut stmt = conn.prepare(
            r#"
            SELECT candidate_id
            FROM memory_candidates
            WHERE project = ?1 AND status = 'pending'
            ORDER BY created_at ASC, candidate_id ASC
            "#,
        )?;
        let ids = stmt
            .query_map(params![project], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        ids.into_iter()
            .map(|id| load_candidate(&conn, &id)?.ok_or_else(|| anyhow!("missing candidate {id}")))
            .collect()
    }

    pub fn memory_candidate(&self, candidate_id: &str) -> anyhow::Result<Option<MemoryCandidate>> {
        let conn = open_database(&self.data_home.paths().db_path)?;
        load_candidate(&conn, candidate_id)
    }

    pub fn promote_memory_candidate(
        &self,
        candidate_id: &str,
        decided_at: f64,
    ) -> anyhow::Result<MemoryCandidateDecision> {
        ensure!(
            !candidate_id.trim().is_empty(),
            "candidate_id cannot be empty"
        );
        ensure!(decided_at.is_finite(), "decided_at must be finite");
        let conn = open_database(&self.data_home.paths().db_path)?;
        let tx = conn.unchecked_transaction()?;
        let candidate = load_candidate(&tx, candidate_id)?
            .ok_or_else(|| anyhow!("memory candidate does not exist: {candidate_id}"))?;
        if candidate.status == MemoryCandidateStatus::Promoted {
            let memory_id = candidate
                .promoted_memory_id
                .ok_or_else(|| anyhow!("promoted candidate is missing promoted_memory_id"))?;
            return Ok(MemoryCandidateDecision::Promoted {
                candidate_id: candidate_id.to_string(),
                memory_id,
            });
        }
        ensure!(
            candidate.status == MemoryCandidateStatus::Pending,
            "only a pending memory candidate can be promoted"
        );

        if let Some(reason) = candidate_stale_reason(&tx, &candidate)? {
            tx.execute(
                r#"
                UPDATE memory_candidates
                SET status = 'stale', decided_at = ?2, decision_reason = ?3
                WHERE candidate_id = ?1 AND status = 'pending'
                "#,
                params![candidate_id, decided_at, reason],
            )?;
            tx.commit()?;
            return Ok(MemoryCandidateDecision::Stale {
                candidate_id: candidate_id.to_string(),
                reason,
            });
        }

        let memory_id = apply_candidate(&tx, &candidate, decided_at)?;
        tx.execute(
            r#"
            UPDATE memory_candidates
            SET status = 'promoted', decided_at = ?2,
                decision_reason = 'validated and promoted',
                promoted_memory_id = ?3
            WHERE candidate_id = ?1 AND status = 'pending'
            "#,
            params![candidate_id, decided_at, memory_id],
        )?;
        tx.commit()?;
        Ok(MemoryCandidateDecision::Promoted {
            candidate_id: candidate_id.to_string(),
            memory_id,
        })
    }

    pub fn reject_memory_candidate(
        &self,
        candidate_id: &str,
        reason: &str,
        decided_at: f64,
    ) -> anyhow::Result<()> {
        ensure!(
            !candidate_id.trim().is_empty(),
            "candidate_id cannot be empty"
        );
        ensure!(
            !reason.trim().is_empty(),
            "rejection reason cannot be empty"
        );
        ensure!(decided_at.is_finite(), "decided_at must be finite");
        let conn = open_database(&self.data_home.paths().db_path)?;
        let changed = conn.execute(
            r#"
            UPDATE memory_candidates
            SET status = 'rejected', decided_at = ?2, decision_reason = ?3
            WHERE candidate_id = ?1 AND status = 'pending'
            "#,
            params![candidate_id, decided_at, reason],
        )?;
        ensure!(
            changed == 1,
            "only a pending memory candidate can be rejected"
        );
        Ok(())
    }
}

fn candidate_stale_reason(
    tx: &Transaction<'_>,
    candidate: &MemoryCandidate,
) -> anyhow::Result<Option<String>> {
    let current_snapshot: Option<String> = tx
        .query_row(
            "SELECT snapshot_id FROM conversation_snapshots WHERE conversation_id = ?1 AND selection_status = 'canonical'",
            params![candidate.conversation_id],
            |row| row.get(0),
        )
        .optional()?;
    let Some(current_snapshot) = current_snapshot else {
        return Ok(Some(
            "conversation no longer has a canonical snapshot".to_string(),
        ));
    };
    if current_snapshot != candidate.source_snapshot_id {
        let max_turn: Option<i64> = tx
            .query_row(
                "SELECT MAX(turn_index) FROM conversation_snapshot_messages WHERE snapshot_id = ?1",
                params![current_snapshot],
                |row| row.get(0),
            )
            .optional()?
            .flatten();
        if max_turn.is_none_or(|turn| turn < candidate.through_turn_index) {
            return Ok(Some(
                "current canonical snapshot is shorter than the candidate evidence boundary"
                    .to_string(),
            ));
        }
        let source_prefix = snapshot_prefix_sha256_hex(
            tx,
            &candidate.source_snapshot_id,
            candidate.through_turn_index,
        )?;
        let current_prefix =
            snapshot_prefix_sha256_hex(tx, &current_snapshot, candidate.through_turn_index)?;
        if source_prefix != current_prefix {
            return Ok(Some(
                "current canonical snapshot no longer preserves the candidate evidence prefix"
                    .to_string(),
            ));
        }
    }

    match &candidate.payload {
        MemoryCandidatePayload::Add { key, .. } => {
            let active: Option<String> = tx
                .query_row(
                    "SELECT memory_id FROM memory_items WHERE scope_type = 'project' AND scope_id = ?1 AND memory_key = ?2 AND status = 'active'",
                    params![candidate.project, key],
                    |row| row.get(0),
                )
                .optional()?;
            if active.is_some() {
                return Ok(Some(
                    "an active memory already exists for the proposed key".to_string(),
                ));
            }
        }
        MemoryCandidatePayload::Supersede {
            target_memory_id,
            key,
            ..
        } => {
            let target = target_memory_identity(tx, target_memory_id)?;
            let Some((project, target_key, status)) = target else {
                return Ok(Some("superseded target no longer exists".to_string()));
            };
            if project != candidate.project || target_key != *key || status != "active" {
                return Ok(Some(
                    "superseded target is no longer the active memory for this project/key"
                        .to_string(),
                ));
            }
        }
        MemoryCandidatePayload::Resolve { target_memory_id } => {
            let target = target_memory_identity(tx, target_memory_id)?;
            let Some((project, _, status)) = target else {
                return Ok(Some("resolve target no longer exists".to_string()));
            };
            if project != candidate.project || status != "active" {
                return Ok(Some(
                    "resolve target is no longer an active project memory".to_string(),
                ));
            }
        }
        MemoryCandidatePayload::Archive { target_memory_id } => {
            let target = target_memory_identity(tx, target_memory_id)?;
            let Some((project, _, status)) = target else {
                return Ok(Some("archive target no longer exists".to_string()));
            };
            if project != candidate.project || !matches!(status.as_str(), "active" | "resolved") {
                return Ok(Some(
                    "archive target is no longer an archivable project memory".to_string(),
                ));
            }
        }
    }
    Ok(None)
}

fn target_memory_identity(
    tx: &Transaction<'_>,
    memory_id: &str,
) -> anyhow::Result<Option<(String, String, String)>> {
    tx.query_row(
        r#"
        SELECT scope_id, memory_key, status
        FROM memory_items
        WHERE memory_id = ?1 AND scope_type = 'project'
        "#,
        params![memory_id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )
    .optional()
    .map_err(Into::into)
}

fn apply_candidate(
    tx: &Transaction<'_>,
    candidate: &MemoryCandidate,
    decided_at: f64,
) -> anyhow::Result<String> {
    match &candidate.payload {
        MemoryCandidatePayload::Add {
            memory_id,
            kind,
            key,
            value,
            importance,
            confidence,
            valid_from,
            valid_until,
            ..
        } => {
            let item = MemoryItem {
                memory_id: memory_id.clone(),
                scope: MemoryScope::Project {
                    project: candidate.project.clone(),
                },
                kind: *kind,
                key: key.clone(),
                value: value.clone(),
                status: MemoryStatus::Active,
                importance: *importance,
                confidence: *confidence,
                valid_from: *valid_from,
                valid_until: *valid_until,
                supersedes_memory_id: None,
                created_at: decided_at,
                updated_at: decided_at,
                last_verified_at: Some(decided_at),
                evidence: candidate.evidence.clone(),
            };
            put_memory_item_tx(tx, &item)?;
            Ok(memory_id.clone())
        }
        MemoryCandidatePayload::Supersede {
            memory_id,
            target_memory_id,
            kind,
            key,
            value,
            importance,
            confidence,
            valid_from,
            valid_until,
            ..
        } => {
            let item = MemoryItem {
                memory_id: memory_id.clone(),
                scope: MemoryScope::Project {
                    project: candidate.project.clone(),
                },
                kind: *kind,
                key: key.clone(),
                value: value.clone(),
                status: MemoryStatus::Active,
                importance: *importance,
                confidence: *confidence,
                valid_from: *valid_from,
                valid_until: *valid_until,
                supersedes_memory_id: Some(target_memory_id.clone()),
                created_at: decided_at,
                updated_at: decided_at,
                last_verified_at: Some(decided_at),
                evidence: candidate.evidence.clone(),
            };
            put_memory_item_tx(tx, &item)?;
            Ok(memory_id.clone())
        }
        MemoryCandidatePayload::Resolve { target_memory_id } => {
            transition_target_memory(
                tx,
                target_memory_id,
                MemoryStatus::Resolved,
                decided_at,
                &candidate.evidence,
            )?;
            Ok(target_memory_id.clone())
        }
        MemoryCandidatePayload::Archive { target_memory_id } => {
            transition_target_memory(
                tx,
                target_memory_id,
                MemoryStatus::Archived,
                decided_at,
                &candidate.evidence,
            )?;
            Ok(target_memory_id.clone())
        }
    }
}

fn transition_target_memory(
    tx: &Transaction<'_>,
    memory_id: &str,
    status: MemoryStatus,
    decided_at: f64,
    evidence: &[MemoryEvidence],
) -> anyhow::Result<()> {
    let changed = tx.execute(
        "UPDATE memory_items SET status = ?2, updated_at = ?3, last_verified_at = ?3 WHERE memory_id = ?1",
        params![memory_id, status.as_str(), decided_at],
    )?;
    ensure!(changed == 1, "target memory disappeared during promotion");
    for item in evidence {
        tx.execute(
            r#"
            INSERT OR IGNORE INTO memory_evidence (
              memory_id, evidence_kind, evidence_ref, detail_json, created_at
            )
            VALUES (?1, ?2, ?3, ?4, ?5)
            "#,
            params![
                memory_id,
                item.kind.as_str(),
                item.reference,
                serde_json::to_string(&item.detail)?,
                item.created_at,
            ],
        )?;
    }
    Ok(())
}

fn exact_compilation_replay(
    tx: &Transaction<'_>,
    batch: &MemoryCompilationBatch,
) -> anyhow::Result<Option<MemoryCompilationStageResult>> {
    let checkpoint: Option<(String, i64, String, String, String, Option<String>, f64)> = tx
        .query_row(
            r#"
            SELECT source_snapshot_id, through_turn_index, through_message_id,
                   prefix_sha256_hex, compiler_version, model_label, updated_at
            FROM memory_compile_checkpoints
            WHERE project = ?1 AND conversation_id = ?2
            "#,
            params![batch.project, batch.conversation_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                ))
            },
        )
        .optional()?;
    let Some((
        source_snapshot_id,
        through_turn_index,
        through_message_id,
        prefix_sha256_hex,
        compiler_version,
        model_label,
        updated_at,
    )) = checkpoint
    else {
        return Ok(None);
    };
    if source_snapshot_id != batch.source_snapshot_id
        || through_turn_index != batch.through_turn_index
        || through_message_id != batch.through_message_id
        || compiler_version != batch.compiler_version
        || model_label != batch.model_label
        || updated_at != batch.created_at
    {
        return Ok(None);
    }

    let mut stmt = tx.prepare(
        r#"
        SELECT candidate_id
        FROM memory_candidates
        WHERE project = ?1
          AND conversation_id = ?2
          AND source_snapshot_id = ?3
          AND through_turn_index = ?4
          AND compiler_version = ?5
          AND model_label IS ?6
          AND created_at = ?7
        ORDER BY candidate_id
        "#,
    )?;
    let existing_ids = stmt
        .query_map(
            params![
                batch.project,
                batch.conversation_id,
                batch.source_snapshot_id,
                batch.through_turn_index,
                batch.compiler_version,
                batch.model_label,
                batch.created_at,
            ],
            |row| row.get::<_, String>(0),
        )?
        .collect::<Result<Vec<_>, _>>()?;
    let mut expected_ids = batch
        .candidates
        .iter()
        .map(|candidate| candidate.candidate_id.clone())
        .collect::<Vec<_>>();
    expected_ids.sort();
    if existing_ids != expected_ids {
        return Ok(None);
    }
    for candidate in &batch.candidates {
        stage_candidate(tx, batch, candidate)?;
    }
    Ok(Some(MemoryCompilationStageResult {
        checkpoint: MemoryCompileCheckpoint {
            project: batch.project.clone(),
            conversation_id: batch.conversation_id.clone(),
            source_snapshot_id,
            through_turn_index,
            through_message_id,
            prefix_sha256_hex,
            compiler_version,
            model_label,
            updated_at,
        },
        candidate_ids: batch
            .candidates
            .iter()
            .map(|candidate| candidate.candidate_id.clone())
            .collect(),
    }))
}

fn validate_batch(batch: &MemoryCompilationBatch) -> anyhow::Result<()> {
    ensure!(!batch.project.trim().is_empty(), "project cannot be empty");
    ensure!(
        !batch.conversation_id.trim().is_empty(),
        "conversation_id cannot be empty"
    );
    ensure!(
        !batch.source_snapshot_id.trim().is_empty(),
        "source_snapshot_id cannot be empty"
    );
    ensure!(
        batch.through_turn_index >= 0,
        "through_turn_index cannot be negative"
    );
    ensure!(
        !batch.through_message_id.trim().is_empty(),
        "through_message_id cannot be empty"
    );
    ensure!(
        !batch.compiler_version.trim().is_empty(),
        "compiler_version cannot be empty"
    );
    ensure!(
        batch.created_at.is_finite(),
        "batch created_at must be finite"
    );
    let mut ids = BTreeSet::new();
    for candidate in &batch.candidates {
        validate_candidate(candidate)?;
        ensure!(
            ids.insert(candidate.candidate_id.as_str()),
            "duplicate candidate_id in compilation batch: {}",
            candidate.candidate_id
        );
    }
    Ok(())
}

fn validate_candidate(candidate: &MemoryCandidateInput) -> anyhow::Result<()> {
    ensure!(
        !candidate.candidate_id.trim().is_empty(),
        "candidate_id cannot be empty"
    );
    ensure!(
        !candidate.rationale.trim().is_empty(),
        "candidate rationale cannot be empty"
    );
    match &candidate.payload {
        MemoryCandidatePayload::Add {
            memory_id,
            key,
            importance,
            confidence,
            valid_from,
            valid_until,
            ..
        }
        | MemoryCandidatePayload::Supersede {
            memory_id,
            key,
            importance,
            confidence,
            valid_from,
            valid_until,
            ..
        } => {
            ensure!(
                !memory_id.trim().is_empty(),
                "proposed memory_id cannot be empty"
            );
            ensure!(
                !key.trim().is_empty(),
                "proposed memory key cannot be empty"
            );
            ensure!(
                confidence.is_finite() && (0.0..=1.0).contains(confidence),
                "candidate confidence must be between 0 and 1"
            );
            ensure!(*importance <= 100, "candidate importance must be <= 100");
            if let (Some(from), Some(until)) = (valid_from, valid_until) {
                ensure!(until >= from, "candidate validity interval is inverted");
            }
        }
        MemoryCandidatePayload::Resolve { target_memory_id }
        | MemoryCandidatePayload::Archive { target_memory_id } => {
            ensure!(
                !target_memory_id.trim().is_empty(),
                "target_memory_id cannot be empty"
            );
        }
    }
    if let MemoryCandidatePayload::Supersede {
        target_memory_id, ..
    } = &candidate.payload
    {
        ensure!(
            !target_memory_id.trim().is_empty(),
            "target_memory_id cannot be empty"
        );
    }
    for evidence in &candidate.evidence {
        ensure!(
            !evidence.reference.trim().is_empty(),
            "candidate evidence reference cannot be empty"
        );
        ensure!(
            evidence.created_at.is_finite(),
            "candidate evidence timestamp must be finite"
        );
    }
    Ok(())
}

fn validate_snapshot_boundary(
    tx: &Transaction<'_>,
    batch: &MemoryCompilationBatch,
) -> anyhow::Result<()> {
    let snapshot: Option<(String, String)> = tx
        .query_row(
            r#"
            SELECT conversation_id, selection_status
            FROM conversation_snapshots
            WHERE snapshot_id = ?1
            "#,
            params![batch.source_snapshot_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((conversation_id, selection_status)) = snapshot else {
        return Err(anyhow!(
            "source snapshot does not exist: {}",
            batch.source_snapshot_id
        ));
    };
    ensure!(
        conversation_id == batch.conversation_id,
        "source snapshot belongs to a different conversation"
    );
    ensure!(
        selection_status == "canonical",
        "memory compilation requires the current canonical conversation snapshot"
    );
    let message_id: Option<String> = tx
        .query_row(
            r#"
            SELECT message_id
            FROM conversation_snapshot_messages
            WHERE snapshot_id = ?1 AND turn_index = ?2
            "#,
            params![batch.source_snapshot_id, batch.through_turn_index],
            |row| row.get(0),
        )
        .optional()?;
    ensure!(
        message_id.as_deref() == Some(batch.through_message_id.as_str()),
        "through_message_id does not match source snapshot turn"
    );
    Ok(())
}

fn validate_checkpoint_progression(
    tx: &Transaction<'_>,
    batch: &MemoryCompilationBatch,
) -> anyhow::Result<()> {
    let existing: Option<(String, i64, String, String)> = tx
        .query_row(
            r#"
            SELECT source_snapshot_id, through_turn_index, through_message_id, prefix_sha256_hex
            FROM memory_compile_checkpoints
            WHERE project = ?1 AND conversation_id = ?2
            "#,
            params![batch.project, batch.conversation_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?;
    let Some((
        previous_snapshot_id,
        previous_turn_index,
        previous_message_id,
        previous_prefix_sha256_hex,
    )) = existing
    else {
        return Ok(());
    };
    ensure!(
        batch.through_turn_index >= previous_turn_index,
        "memory compile checkpoint cannot move backward"
    );
    if batch.through_turn_index == previous_turn_index {
        ensure!(
            batch.through_message_id == previous_message_id,
            "memory compile checkpoint cannot change message identity at the same turn"
        );
    }
    if batch.source_snapshot_id != previous_snapshot_id {
        let prefix_sha256_hex =
            snapshot_prefix_sha256_hex(tx, &batch.source_snapshot_id, previous_turn_index)?;
        ensure!(
            prefix_sha256_hex == previous_prefix_sha256_hex,
            "new canonical snapshot does not preserve the previously compiled prefix"
        );
    }
    Ok(())
}

fn snapshot_prefix_sha256_hex(
    tx: &Transaction<'_>,
    snapshot_id: &str,
    through_turn_index: i64,
) -> anyhow::Result<String> {
    let mut stmt = tx.prepare(
        r#"
        SELECT message_id, role, turn_index, normalized_text
        FROM conversation_snapshot_messages
        WHERE snapshot_id = ?1 AND turn_index <= ?2
        ORDER BY turn_index ASC, message_id ASC
        "#,
    )?;
    let rows = stmt
        .query_map(params![snapshot_id, through_turn_index], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    ensure!(
        rows.last().is_some_and(|row| row.2 == through_turn_index),
        "snapshot does not contain the requested compile prefix"
    );
    let mut digest = Sha256::new();
    digest.update(b"memory-compile-prefix-v1");
    for (message_id, role, turn_index, text) in rows {
        let turn_bytes = turn_index.to_le_bytes();
        for bytes in [
            message_id.as_bytes(),
            role.as_bytes(),
            turn_bytes.as_slice(),
            text.as_bytes(),
        ] {
            digest.update((bytes.len() as u64).to_le_bytes());
            digest.update(bytes);
        }
    }
    Ok(hex::encode(digest.finalize()))
}

fn stage_candidate(
    tx: &Transaction<'_>,
    batch: &MemoryCompilationBatch,
    candidate: &MemoryCandidateInput,
) -> anyhow::Result<()> {
    let operation = candidate.payload.operation();
    let payload_json = serde_json::to_string(&candidate.payload)?;
    let existing: Option<(
        String,
        String,
        String,
        String,
        String,
        String,
        Option<String>,
        i64,
        f64,
    )> = tx
        .query_row(
            r#"
            SELECT project, conversation_id, source_snapshot_id, operation, payload_json,
                   compiler_version, model_label, through_turn_index, created_at
            FROM memory_candidates
            WHERE candidate_id = ?1
            "#,
            params![candidate.candidate_id],
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
                ))
            },
        )
        .optional()?;
    if let Some((
        project,
        conversation_id,
        source_snapshot_id,
        existing_operation,
        existing_payload_json,
        compiler_version,
        model_label,
        through_turn_index,
        created_at,
    )) = existing
    {
        ensure!(project == batch.project, "candidate project is immutable");
        ensure!(
            conversation_id == batch.conversation_id,
            "candidate conversation is immutable"
        );
        ensure!(
            source_snapshot_id == batch.source_snapshot_id,
            "candidate source snapshot is immutable"
        );
        ensure!(
            existing_operation == operation.as_str() && existing_payload_json == payload_json,
            "candidate operation/payload is immutable"
        );
        ensure!(
            compiler_version == batch.compiler_version,
            "candidate compiler version is immutable"
        );
        ensure!(
            model_label == batch.model_label,
            "candidate model label is immutable"
        );
        ensure!(
            through_turn_index == batch.through_turn_index && created_at == batch.created_at,
            "candidate compile boundary is immutable"
        );
        let existing_rationale: String = tx.query_row(
            "SELECT rationale FROM memory_candidates WHERE candidate_id = ?1",
            params![candidate.candidate_id],
            |row| row.get(0),
        )?;
        ensure!(
            existing_rationale == candidate.rationale,
            "candidate rationale is immutable"
        );
        ensure_candidate_evidence_matches(tx, candidate)?;
        return Ok(());
    }

    tx.execute(
        r#"
        INSERT INTO memory_candidates (
          candidate_id, project, conversation_id, source_snapshot_id, operation,
          payload_json, status, rationale, compiler_version, model_label,
          through_turn_index, created_at
        )
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'pending', ?7, ?8, ?9, ?10, ?11)
        "#,
        params![
            candidate.candidate_id,
            batch.project,
            batch.conversation_id,
            batch.source_snapshot_id,
            operation.as_str(),
            payload_json,
            candidate.rationale,
            batch.compiler_version,
            batch.model_label,
            batch.through_turn_index,
            batch.created_at,
        ],
    )?;
    for evidence in &candidate.evidence {
        tx.execute(
            r#"
            INSERT INTO memory_candidate_evidence (
              candidate_id, evidence_kind, evidence_ref, detail_json, created_at
            )
            VALUES (?1, ?2, ?3, ?4, ?5)
            "#,
            params![
                candidate.candidate_id,
                evidence.kind.as_str(),
                evidence.reference,
                serde_json::to_string(&evidence.detail)?,
                evidence.created_at,
            ],
        )?;
    }
    Ok(())
}

fn ensure_candidate_evidence_matches(
    tx: &Transaction<'_>,
    candidate: &MemoryCandidateInput,
) -> anyhow::Result<()> {
    let mut stmt = tx.prepare(
        r#"
        SELECT evidence_kind, evidence_ref, detail_json, created_at
        FROM memory_candidate_evidence
        WHERE candidate_id = ?1
        ORDER BY evidence_kind, evidence_ref
        "#,
    )?;
    let existing = stmt
        .query_map(params![candidate.candidate_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, f64>(3)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let mut expected = candidate
        .evidence
        .iter()
        .map(|evidence| {
            Ok((
                evidence.kind.as_str().to_string(),
                evidence.reference.clone(),
                serde_json::to_string(&evidence.detail)?,
                evidence.created_at,
            ))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    expected.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));
    ensure!(existing == expected, "candidate evidence is immutable");
    Ok(())
}

fn load_candidate(
    conn: &rusqlite::Connection,
    candidate_id: &str,
) -> anyhow::Result<Option<MemoryCandidate>> {
    let row: Option<(
        String,
        String,
        String,
        String,
        String,
        String,
        String,
        String,
        Option<String>,
        i64,
        f64,
        Option<f64>,
        Option<String>,
        Option<String>,
    )> = conn
        .query_row(
            r#"
            SELECT project, conversation_id, source_snapshot_id, operation, payload_json,
                   status, rationale, compiler_version, model_label, through_turn_index,
                   created_at, decided_at, decision_reason, promoted_memory_id
            FROM memory_candidates
            WHERE candidate_id = ?1
            "#,
            params![candidate_id],
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
        project,
        conversation_id,
        source_snapshot_id,
        operation,
        payload_json,
        status,
        rationale,
        compiler_version,
        model_label,
        through_turn_index,
        created_at,
        decided_at,
        decision_reason,
        promoted_memory_id,
    )) = row
    else {
        return Ok(None);
    };
    let mut stmt = conn.prepare(
        r#"
        SELECT evidence_kind, evidence_ref, detail_json, created_at
        FROM memory_candidate_evidence
        WHERE candidate_id = ?1
        ORDER BY evidence_kind, evidence_ref
        "#,
    )?;
    let evidence = stmt
        .query_map(params![candidate_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, f64>(3)?,
            ))
        })?
        .map(|row| -> anyhow::Result<MemoryEvidence> {
            let (kind, reference, detail_json, created_at) = row?;
            Ok(MemoryEvidence {
                kind: evidence_kind_from_db(&kind)?,
                reference,
                detail: serde_json::from_str(&detail_json)?,
                created_at,
            })
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    let payload: MemoryCandidatePayload = serde_json::from_str(&payload_json)?;
    let operation_value = MemoryCandidateOperation::from_db(&operation)?;
    ensure!(
        payload.operation() == operation_value,
        "candidate operation does not match payload"
    );
    Ok(Some(MemoryCandidate {
        candidate_id: candidate_id.to_string(),
        project,
        conversation_id,
        source_snapshot_id,
        operation: operation_value,
        payload,
        status: MemoryCandidateStatus::from_db(&status)?,
        rationale,
        compiler_version,
        model_label,
        through_turn_index,
        created_at,
        decided_at,
        decision_reason,
        promoted_memory_id,
        evidence,
    }))
}

fn evidence_kind_from_db(value: &str) -> anyhow::Result<MemoryEvidenceKind> {
    match value {
        "user_statement" => Ok(MemoryEvidenceKind::UserStatement),
        "conversation_turn" => Ok(MemoryEvidenceKind::ConversationTurn),
        "document" => Ok(MemoryEvidenceKind::Document),
        "git_commit" => Ok(MemoryEvidenceKind::GitCommit),
        "repository_state" => Ok(MemoryEvidenceKind::RepositoryState),
        "devspace_result" => Ok(MemoryEvidenceKind::DevspaceResult),
        other => Err(anyhow!("unknown memory evidence kind: {other}")),
    }
}
