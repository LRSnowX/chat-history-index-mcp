use anyhow::ensure;
use chrono::Utc;
use rusqlite::{OptionalExtension, params};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    db::open_database,
    ingest::IndexService,
    memory_compile::{MemoryCandidateRevalidationProblem, snapshot_prefix_sha256_hex},
};

const MAX_HEALTH_DETAIL_ITEMS: usize = 20;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
pub struct MemoryStatusCounts {
    pub active: usize,
    pub resolved: usize,
    pub superseded: usize,
    pub archived: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
pub struct MemoryCandidateStatusCounts {
    pub pending: usize,
    pub promoted: usize,
    pub rejected: usize,
    pub stale: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MemoryCheckpointPrefixStatus {
    Valid,
    Changed,
    MissingCanonical,
    MissingPrefix,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MemoryCheckpointHealth {
    pub conversation_id: String,
    pub source_snapshot_id: String,
    pub current_snapshot_id: Option<String>,
    pub through_turn_index: i64,
    pub current_max_turn_index: Option<i64>,
    pub behind_turns: Option<i64>,
    pub prefix_status: MemoryCheckpointPrefixStatus,
    pub compiler_version: String,
    pub model_label: Option<String>,
    pub updated_at: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct IncompleteCanonicalConversationHealth {
    pub conversation_id: String,
    pub source: String,
    pub title: String,
    pub update_time: Option<f64>,
    pub message_count: i64,
    pub user_message_count: i64,
    pub assistant_message_count: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MemoryHealthReport {
    pub project: String,
    pub generated_at: f64,
    pub stale_after_days: u32,
    pub memory_items: MemoryStatusCounts,
    pub active_unverified: usize,
    pub active_stale_or_unverified: usize,
    pub oldest_active_updated_at: Option<f64>,
    pub candidates: MemoryCandidateStatusCounts,
    pub oldest_pending_created_at: Option<f64>,
    pub checkpoints: Vec<MemoryCheckpointHealth>,
    pub checkpoint_count: usize,
    pub checkpoint_caught_up: usize,
    pub checkpoint_behind: usize,
    pub checkpoint_prefix_problem: usize,
    pub tracked_canonical_snapshots: usize,
    pub tracked_rejected_lower_quality_snapshots: usize,
    pub strong_project_conversation_count: usize,
    pub incomplete_canonical_conversation_count: usize,
    pub incomplete_canonical_conversations_truncated: bool,
    pub incomplete_canonical_conversations: Vec<IncompleteCanonicalConversationHealth>,
    pub pending_revalidation_problem_count: usize,
    pub pending_revalidation_problems_truncated: bool,
    pub pending_revalidation_problems: Vec<MemoryCandidateRevalidationProblem>,
}

impl IndexService {
    pub fn memory_health(
        &self,
        project: &str,
        stale_after_days: u32,
    ) -> anyhow::Result<MemoryHealthReport> {
        ensure!(!project.trim().is_empty(), "project cannot be empty");
        ensure!(
            (1..=3_650).contains(&stale_after_days),
            "stale_after_days must be between 1 and 3650"
        );
        let conn = open_database(&self.data_home.paths().db_path)?;
        let now = now_epoch();
        let stale_before = now - f64::from(stale_after_days) * 86_400.0;

        let mut memory_items = MemoryStatusCounts::default();
        {
            let mut stmt = conn.prepare(
                r#"
                SELECT status, COUNT(*)
                FROM memory_items
                WHERE scope_type = 'project' AND scope_id = ?1
                GROUP BY status
                "#,
            )?;
            for row in stmt.query_map(params![project], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })? {
                let (status, count) = row?;
                let count = usize::try_from(count)?;
                match status.as_str() {
                    "active" => memory_items.active = count,
                    "resolved" => memory_items.resolved = count,
                    "superseded" => memory_items.superseded = count,
                    "archived" => memory_items.archived = count,
                    _ => {}
                }
            }
        }
        let (active_unverified, active_stale_or_unverified, oldest_active_updated_at): (
            i64,
            i64,
            Option<f64>,
        ) = conn.query_row(
            r#"
            SELECT
              COALESCE(SUM(CASE WHEN last_verified_at IS NULL THEN 1 ELSE 0 END), 0),
              COALESCE(SUM(CASE
                    WHEN last_verified_at IS NULL OR last_verified_at < ?2 THEN 1
                    ELSE 0
                  END), 0),
              MIN(updated_at)
            FROM memory_items
            WHERE scope_type = 'project' AND scope_id = ?1 AND status = 'active'
            "#,
            params![project, stale_before],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;

        let mut candidates = MemoryCandidateStatusCounts::default();
        {
            let mut stmt = conn.prepare(
                r#"
                SELECT status, COUNT(*)
                FROM memory_candidates
                WHERE project = ?1
                GROUP BY status
                "#,
            )?;
            for row in stmt.query_map(params![project], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })? {
                let (status, count) = row?;
                let count = usize::try_from(count)?;
                match status.as_str() {
                    "pending" => candidates.pending = count,
                    "promoted" => candidates.promoted = count,
                    "rejected" => candidates.rejected = count,
                    "stale" => candidates.stale = count,
                    _ => {}
                }
            }
        }
        let oldest_pending_created_at: Option<f64> = conn.query_row(
            "SELECT MIN(created_at) FROM memory_candidates WHERE project = ?1 AND status = 'pending'",
            params![project],
            |row| row.get(0),
        )?;

        let mut checkpoints = Vec::new();
        {
            let mut stmt = conn.prepare(
                r#"
                SELECT conversation_id, source_snapshot_id, through_turn_index,
                       prefix_sha256_hex, compiler_version, model_label, updated_at
                FROM memory_compile_checkpoints
                WHERE project = ?1
                ORDER BY updated_at ASC, conversation_id ASC
                "#,
            )?;
            let rows = stmt
                .query_map(params![project], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, Option<String>>(5)?,
                        row.get::<_, f64>(6)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            for (
                conversation_id,
                source_snapshot_id,
                through_turn_index,
                prefix_sha256_hex,
                compiler_version,
                model_label,
                updated_at,
            ) in rows
            {
                let current_snapshot_id: Option<String> = conn
                    .query_row(
                        r#"
                        SELECT snapshot_id
                        FROM conversation_snapshots
                        WHERE conversation_id = ?1 AND selection_status = 'canonical'
                        "#,
                        params![conversation_id],
                        |row| row.get(0),
                    )
                    .optional()?;
                let (current_max_turn_index, behind_turns, prefix_status) = if let Some(
                    current_snapshot_id,
                ) =
                    current_snapshot_id.as_deref()
                {
                    let current_max_turn_index: Option<i64> = conn.query_row(
                            "SELECT MAX(turn_index) FROM conversation_snapshot_messages WHERE snapshot_id = ?1",
                            params![current_snapshot_id],
                            |row| row.get(0),
                        )?;
                    let behind_turns = current_max_turn_index
                        .map(|max_turn| (max_turn - through_turn_index).max(0));
                    let prefix_status = if current_max_turn_index
                        .is_none_or(|max_turn| max_turn < through_turn_index)
                    {
                        MemoryCheckpointPrefixStatus::MissingPrefix
                    } else {
                        let current_prefix = snapshot_prefix_sha256_hex(
                            &conn,
                            current_snapshot_id,
                            through_turn_index,
                        )?;
                        if current_prefix == prefix_sha256_hex {
                            MemoryCheckpointPrefixStatus::Valid
                        } else {
                            MemoryCheckpointPrefixStatus::Changed
                        }
                    };
                    (current_max_turn_index, behind_turns, prefix_status)
                } else {
                    (None, None, MemoryCheckpointPrefixStatus::MissingCanonical)
                };
                checkpoints.push(MemoryCheckpointHealth {
                    conversation_id,
                    source_snapshot_id,
                    current_snapshot_id,
                    through_turn_index,
                    current_max_turn_index,
                    behind_turns,
                    prefix_status,
                    compiler_version,
                    model_label,
                    updated_at,
                });
            }
        }

        let checkpoint_count = checkpoints.len();
        let checkpoint_caught_up = checkpoints
            .iter()
            .filter(|checkpoint| {
                checkpoint.prefix_status == MemoryCheckpointPrefixStatus::Valid
                    && checkpoint.behind_turns == Some(0)
            })
            .count();
        let checkpoint_behind = checkpoints
            .iter()
            .filter(|checkpoint| {
                checkpoint.prefix_status == MemoryCheckpointPrefixStatus::Valid
                    && checkpoint.behind_turns.is_some_and(|turns| turns > 0)
            })
            .count();
        let checkpoint_prefix_problem = checkpoints
            .iter()
            .filter(|checkpoint| checkpoint.prefix_status != MemoryCheckpointPrefixStatus::Valid)
            .count();

        let tracked_canonical_snapshots: i64 = conn.query_row(
            r#"
            SELECT COUNT(*)
            FROM conversation_snapshots
            WHERE selection_status = 'canonical'
              AND conversation_id IN (
                SELECT conversation_id
                FROM memory_compile_checkpoints
                WHERE project = ?1
              )
            "#,
            params![project],
            |row| row.get(0),
        )?;
        let tracked_rejected_lower_quality_snapshots: i64 = conn.query_row(
            r#"
            SELECT COUNT(*)
            FROM conversation_snapshots
            WHERE selection_status = 'rejected_lower_quality'
              AND conversation_id IN (
                SELECT conversation_id
                FROM memory_compile_checkpoints
                WHERE project = ?1
              )
            "#,
            params![project],
            |row| row.get(0),
        )?;
        let strong_project_conversations = self.strong_project_conversations(project)?;
        let strong_project_conversation_count = strong_project_conversations.len();
        let mut incomplete_canonical_conversations = strong_project_conversations
            .into_iter()
            .filter(|conversation| {
                conversation.source == "chatgpt"
                    && conversation.message_count >= 2
                    && conversation.assistant_message_count == 0
            })
            .map(|conversation| IncompleteCanonicalConversationHealth {
                conversation_id: conversation.conversation_id,
                source: conversation.source,
                title: conversation.title,
                update_time: conversation.update_time,
                message_count: conversation.message_count,
                user_message_count: conversation.user_message_count,
                assistant_message_count: conversation.assistant_message_count,
            })
            .collect::<Vec<_>>();
        let incomplete_canonical_conversation_count = incomplete_canonical_conversations.len();
        incomplete_canonical_conversations.truncate(MAX_HEALTH_DETAIL_ITEMS);

        let mut pending_revalidation_problems =
            self.pending_memory_candidate_revalidation_problems(project)?;
        let pending_revalidation_problem_count = pending_revalidation_problems.len();
        pending_revalidation_problems.truncate(MAX_HEALTH_DETAIL_ITEMS);

        Ok(MemoryHealthReport {
            project: project.to_string(),
            generated_at: now,
            stale_after_days,
            memory_items,
            active_unverified: usize::try_from(active_unverified)?,
            active_stale_or_unverified: usize::try_from(active_stale_or_unverified)?,
            oldest_active_updated_at,
            candidates,
            oldest_pending_created_at,
            checkpoints,
            checkpoint_count,
            checkpoint_caught_up,
            checkpoint_behind,
            checkpoint_prefix_problem,
            tracked_canonical_snapshots: usize::try_from(tracked_canonical_snapshots)?,
            tracked_rejected_lower_quality_snapshots: usize::try_from(
                tracked_rejected_lower_quality_snapshots,
            )?,
            strong_project_conversation_count,
            incomplete_canonical_conversation_count,
            incomplete_canonical_conversations_truncated: incomplete_canonical_conversation_count
                > MAX_HEALTH_DETAIL_ITEMS,
            incomplete_canonical_conversations,
            pending_revalidation_problem_count,
            pending_revalidation_problems_truncated: pending_revalidation_problem_count
                > MAX_HEALTH_DETAIL_ITEMS,
            pending_revalidation_problems,
        })
    }
}

fn now_epoch() -> f64 {
    Utc::now().timestamp_millis() as f64 / 1000.0
}
