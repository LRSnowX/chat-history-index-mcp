use chat_history_core::{
    DataHome, IndexService, MemoryCandidateDecision, MemoryCandidateInput, MemoryCandidatePayload,
    MemoryCandidateStatus, MemoryCompilationBatch, MemoryEvidence, MemoryEvidenceKind, MemoryItem,
    MemoryKind, MemoryScope, MemoryStatus, NormalizedConversation, NormalizedMessage,
    db::{open_database, restore_database},
};
use rusqlite::params;
use serde_json::json;
use tempfile::TempDir;

fn service(temp: &TempDir) -> IndexService {
    IndexService::new(DataHome::new(temp.path().join("managed")), None)
}

fn message(id: &str, role: &str, text: &str, time: f64) -> NormalizedMessage {
    NormalizedMessage {
        message_id: id.to_string(),
        role: role.to_string(),
        create_time: Some(time),
        text: text.to_string(),
        raw: json!({"id": id, "role": role}),
    }
}

fn chatgpt_conversation(
    id: &str,
    update_time: f64,
    messages: Vec<NormalizedMessage>,
) -> NormalizedConversation {
    NormalizedConversation {
        source: "chatgpt".to_string(),
        source_instance: None,
        source_conversation_id: id.to_string(),
        title: "LEMonX development".to_string(),
        create_time: Some(1.0),
        update_time: Some(update_time),
        model: None,
        source_url: None,
        source_path: Some("chatgpt-app-bridge".to_string()),
        messages,
        raw: json!({"collector": "test"}),
    }
}

#[test]
fn normalized_import_refuses_an_obvious_conversation_quality_regression() -> anyhow::Result<()> {
    let temp = TempDir::new()?;
    let service = service(&temp);
    let id = "conversation-1";

    let initial = chatgpt_conversation(
        id,
        10.0,
        vec![
            message("u1", "user", "start", 1.0),
            message("a1", "assistant", "answer one", 2.0),
            message("u2", "user", "continue", 3.0),
            message("a2", "assistant", "answer two", 4.0),
        ],
    );
    let report = service.import_normalized(vec![initial], None)?;
    assert_eq!(report.conversations_indexed, 1);
    assert_eq!(report.conversations_skipped_lower_quality, 0);
    assert_snapshot_statuses(&service, id, &[("canonical", 1)])?;

    let degraded = chatgpt_conversation(
        id,
        11.0,
        vec![
            message("u1", "user", "start", 1.0),
            message("u2", "user", "continue", 3.0),
        ],
    );
    let report = service.import_normalized(vec![degraded], None)?;
    assert_eq!(report.conversations_indexed, 0);
    assert_eq!(report.conversations_skipped_lower_quality, 1);
    assert_snapshot_statuses(
        &service,
        id,
        &[("canonical", 1), ("rejected_lower_quality", 1)],
    )?;

    let retained = service
        .get_conversation(id, false)?
        .expect("conversation remains indexed");
    assert_eq!(retained.conversation.message_count, 4);
    assert_eq!(retained.conversation.user_message_count, 2);
    assert_eq!(retained.conversation.assistant_message_count, 2);
    assert_eq!(
        retained
            .messages
            .iter()
            .map(|message| message.message_id.as_str())
            .collect::<Vec<_>>(),
        vec!["u1", "a1", "u2", "a2"]
    );

    let improved = chatgpt_conversation(
        id,
        12.0,
        vec![
            message("u1", "user", "start", 1.0),
            message("a1", "assistant", "answer one", 2.0),
            message("u2", "user", "continue", 3.0),
            message("a2", "assistant", "answer two", 4.0),
            message("u3", "user", "next", 5.0),
            message("a3", "assistant", "answer three", 6.0),
        ],
    );
    let report = service.import_normalized(vec![improved], None)?;
    assert_eq!(report.conversations_indexed, 1);
    assert_eq!(report.conversations_skipped_lower_quality, 0);
    let retained = service
        .get_conversation(id, false)?
        .expect("conversation remains indexed");
    assert_eq!(retained.conversation.message_count, 6);
    assert_eq!(retained.conversation.assistant_message_count, 3);
    assert_snapshot_statuses(
        &service,
        id,
        &[
            ("canonical", 1),
            ("rejected_lower_quality", 1),
            ("superseded", 1),
        ],
    )?;
    Ok(())
}

#[test]
fn first_phase_three_reimport_snapshots_the_legacy_canonical_before_rejecting() -> anyhow::Result<()>
{
    let temp = TempDir::new()?;
    let service = service(&temp);
    let id = "legacy-conversation";

    let initial = chatgpt_conversation(
        id,
        10.0,
        vec![
            message("u1", "user", "start", 1.0),
            message("a1", "assistant", "answer one", 2.0),
            message("u2", "user", "continue", 3.0),
            message("a2", "assistant", "answer two", 4.0),
        ],
    );
    service.import_normalized(vec![initial], None)?;

    {
        let conn = open_database(&service.managed_db_path())?;
        conn.execute("DELETE FROM conversation_snapshot_messages", [])?;
        conn.execute("DELETE FROM conversation_snapshots", [])?;
        conn.pragma_update(None, "user_version", 2)?;
    }

    let degraded = chatgpt_conversation(
        id,
        11.0,
        vec![
            message("u1", "user", "start", 1.0),
            message("u2", "user", "continue", 3.0),
        ],
    );
    let report = service.import_normalized(vec![degraded], None)?;
    assert_eq!(report.conversations_indexed, 0);
    assert_eq!(report.conversations_skipped_lower_quality, 1);
    assert_snapshot_statuses(
        &service,
        id,
        &[("canonical", 1), ("rejected_lower_quality", 1)],
    )?;

    let conn = open_database(&service.managed_db_path())?;
    let canonical_messages: i64 = conn.query_row(
        r#"
        SELECT COUNT(*)
        FROM conversation_snapshot_messages m
        JOIN conversation_snapshots s ON s.snapshot_id = m.snapshot_id
        WHERE s.conversation_id = ?1 AND s.selection_status = 'canonical'
        "#,
        params![id],
        |row| row.get(0),
    )?;
    assert_eq!(canonical_messages, 4);
    Ok(())
}

fn assert_snapshot_statuses(
    service: &IndexService,
    conversation_id: &str,
    expected: &[(&str, i64)],
) -> anyhow::Result<()> {
    let conn = open_database(&service.managed_db_path())?;
    let total: i64 = conn.query_row(
        "SELECT COUNT(*) FROM conversation_snapshots WHERE conversation_id = ?1",
        params![conversation_id],
        |row| row.get(0),
    )?;
    assert_eq!(total, expected.iter().map(|(_, count)| *count).sum::<i64>());
    for (status, count) in expected {
        let actual: i64 = conn.query_row(
            "SELECT COUNT(*) FROM conversation_snapshots WHERE conversation_id = ?1 AND selection_status = ?2",
            params![conversation_id, status],
            |row| row.get(0),
        )?;
        assert_eq!(actual, *count, "unexpected snapshot count for {status}");
    }
    Ok(())
}

fn project_memory(
    id: &str,
    value: &str,
    supersedes_memory_id: Option<&str>,
    updated_at: f64,
) -> MemoryItem {
    MemoryItem {
        memory_id: id.to_string(),
        scope: MemoryScope::Project {
            project: "LEMonX".to_string(),
        },
        kind: MemoryKind::State,
        key: "current_goal".to_string(),
        value: json!({"text": value}),
        status: MemoryStatus::Active,
        importance: 95,
        confidence: 1.0,
        valid_from: Some(updated_at),
        valid_until: None,
        supersedes_memory_id: supersedes_memory_id.map(str::to_string),
        created_at: updated_at,
        updated_at,
        last_verified_at: Some(updated_at),
        evidence: vec![MemoryEvidence {
            kind: MemoryEvidenceKind::UserStatement,
            reference: format!("conversation:test:{id}"),
            detail: json!({"turn": 1}),
            created_at: updated_at,
        }],
    }
}

#[test]
fn project_working_memory_requires_explicit_supersession() -> anyhow::Result<()> {
    let temp = TempDir::new()?;
    let service = service(&temp);

    let first = project_memory("mem-1", "finish checkpoint A", None, 10.0);
    service.put_memory_item(&first)?;

    let conflicting = project_memory("mem-conflict", "silently overwrite", None, 11.0);
    let error = service
        .put_memory_item(&conflicting)
        .expect_err("two active values for one key must fail");
    assert!(error.to_string().contains("writing memory item"));

    let replacement = project_memory("mem-2", "run repository acceptance", Some("mem-1"), 12.0);
    service.put_memory_item(&replacement)?;
    service.put_memory_item(&replacement)?;

    let old = service
        .get_memory_item("mem-1")?
        .expect("superseded item retained as history");
    assert_eq!(old.status, MemoryStatus::Superseded);
    assert_eq!(old.evidence.len(), 1);

    let working = service.project_working_memory("LEMonX")?;
    assert_eq!(working.items.len(), 1);
    assert_eq!(working.items[0].memory_id, "mem-2");
    assert_eq!(working.items[0].status, MemoryStatus::Active);
    assert_eq!(
        working.items[0].value,
        json!({"text": "run repository acceptance"})
    );
    Ok(())
}

fn canonical_snapshot_id(service: &IndexService, conversation_id: &str) -> anyhow::Result<String> {
    let conn = open_database(&service.managed_db_path())?;
    Ok(conn.query_row(
        "SELECT snapshot_id FROM conversation_snapshots WHERE conversation_id = ?1 AND selection_status = 'canonical'",
        params![conversation_id],
        |row| row.get(0),
    )?)
}

#[test]
fn memory_compilation_stages_candidates_without_mutating_working_memory() -> anyhow::Result<()> {
    let temp = TempDir::new()?;
    let service = service(&temp);
    let conversation_id = "compile-conversation";
    service.import_normalized(
        vec![chatgpt_conversation(
            conversation_id,
            10.0,
            vec![
                message("u1", "user", "start", 1.0),
                message("a1", "assistant", "answer one", 2.0),
                message("u2", "user", "continue", 3.0),
                message("a2", "assistant", "answer two", 4.0),
            ],
        )],
        None,
    )?;
    let snapshot_one = canonical_snapshot_id(&service, conversation_id)?;
    let batch_one = MemoryCompilationBatch {
        project: "LEMonX".to_string(),
        conversation_id: conversation_id.to_string(),
        source_snapshot_id: snapshot_one.clone(),
        through_turn_index: 3,
        through_message_id: "a2".to_string(),
        compiler_version: "memory-compiler-v1".to_string(),
        model_label: Some("test-model".to_string()),
        created_at: 20.0,
        candidates: vec![MemoryCandidateInput {
            candidate_id: "candidate-1".to_string(),
            payload: MemoryCandidatePayload::Add {
                memory_id: "candidate-memory-1".to_string(),
                kind: MemoryKind::State,
                key: "current_goal".to_string(),
                value: json!({"text": "continue checkpoint"}),
                importance: 90,
                confidence: 0.95,
                valid_from: Some(20.0),
                valid_until: None,
                last_verified_at: Some(20.0),
            },
            rationale: "The latest accepted dialogue states the current goal.".to_string(),
            evidence: vec![MemoryEvidence {
                kind: MemoryEvidenceKind::ConversationTurn,
                reference: "conversation:compile-conversation:message:a2".to_string(),
                detail: json!({"turn_index": 3}),
                created_at: 20.0,
            }],
        }],
    };

    let staged = service.stage_memory_compilation(&batch_one)?;
    assert_eq!(staged.candidate_ids, vec!["candidate-1"]);
    assert!(!staged.checkpoint.prefix_sha256_hex.is_empty());
    service.stage_memory_compilation(&batch_one)?;

    let pending = service.pending_memory_candidates("LEMonX")?;
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].candidate_id, "candidate-1");
    assert_eq!(pending[0].evidence.len(), 1);
    assert!(service.project_working_memory("LEMonX")?.items.is_empty());
    let promoted = service.promote_memory_candidate("candidate-1", 22.0)?;
    assert_eq!(
        promoted,
        MemoryCandidateDecision::Promoted {
            candidate_id: "candidate-1".to_string(),
            memory_id: "candidate-memory-1".to_string(),
        }
    );
    assert_eq!(
        service.promote_memory_candidate("candidate-1", 23.0)?,
        promoted
    );
    let promoted_candidate = service
        .memory_candidate("candidate-1")?
        .expect("candidate remains auditable");
    assert_eq!(promoted_candidate.status, MemoryCandidateStatus::Promoted);
    assert_eq!(
        promoted_candidate.decision_reason.as_deref(),
        Some("validated and promoted")
    );
    let working = service.project_working_memory("LEMonX")?;
    assert_eq!(working.items.len(), 1);
    assert_eq!(working.items[0].memory_id, "candidate-memory-1");

    let backward = MemoryCompilationBatch {
        project: "LEMonX".to_string(),
        conversation_id: conversation_id.to_string(),
        source_snapshot_id: snapshot_one,
        through_turn_index: 2,
        through_message_id: "u2".to_string(),
        compiler_version: "memory-compiler-v1".to_string(),
        model_label: Some("test-model".to_string()),
        created_at: 21.0,
        candidates: Vec::new(),
    };
    let error = service
        .stage_memory_compilation(&backward)
        .expect_err("checkpoint must not move backward");
    assert!(error.to_string().contains("cannot move backward"));

    service.import_normalized(
        vec![chatgpt_conversation(
            conversation_id,
            11.0,
            vec![
                message("u1", "user", "start", 1.0),
                message("a1", "assistant", "answer one", 2.0),
                message("u2", "user", "continue", 3.0),
                message("a2", "assistant", "answer two", 4.0),
                message("u3", "user", "next", 5.0),
                message("a3", "assistant", "answer three", 6.0),
            ],
        )],
        None,
    )?;
    let snapshot_two = canonical_snapshot_id(&service, conversation_id)?;
    let replayed = service.stage_memory_compilation(&batch_one)?;
    assert_eq!(replayed.candidate_ids, vec!["candidate-1"]);
    assert_eq!(
        replayed.checkpoint.source_snapshot_id,
        batch_one.source_snapshot_id
    );
    assert_eq!(
        replayed.checkpoint.model_label.as_deref(),
        Some("test-model")
    );
    let advance = MemoryCompilationBatch {
        project: "LEMonX".to_string(),
        conversation_id: conversation_id.to_string(),
        source_snapshot_id: snapshot_two,
        through_turn_index: 5,
        through_message_id: "a3".to_string(),
        compiler_version: "memory-compiler-v1".to_string(),
        model_label: Some("test-model".to_string()),
        created_at: 30.0,
        candidates: vec![MemoryCandidateInput {
            candidate_id: "candidate-2".to_string(),
            payload: MemoryCandidatePayload::Supersede {
                memory_id: "candidate-memory-2".to_string(),
                target_memory_id: "candidate-memory-1".to_string(),
                kind: MemoryKind::State,
                key: "current_goal".to_string(),
                value: json!({"text": "new candidate goal"}),
                importance: 95,
                confidence: 0.95,
                valid_from: Some(30.0),
                valid_until: None,
                last_verified_at: Some(30.0),
            },
            rationale: "The newer continuation proposes a replacement goal.".to_string(),
            evidence: vec![MemoryEvidence {
                kind: MemoryEvidenceKind::ConversationTurn,
                reference: "conversation:compile-conversation:message:a3".to_string(),
                detail: json!({"turn_index": 5}),
                created_at: 30.0,
            }],
        }],
    };
    service.stage_memory_compilation(&advance)?;
    let checkpoint = service
        .memory_compile_checkpoint("LEMonX", conversation_id)?
        .expect("checkpoint exists");
    assert_eq!(checkpoint.through_turn_index, 5);
    assert_eq!(checkpoint.through_message_id, "a3");
    assert_eq!(service.pending_memory_candidates("LEMonX")?.len(), 1);

    service.import_normalized(
        vec![chatgpt_conversation(
            conversation_id,
            12.0,
            vec![
                message("u1", "user", "start", 1.0),
                message("a1", "assistant", "answer one", 2.0),
                message("u2-branch", "user", "changed earlier turn", 3.0),
                message("a2", "assistant", "answer two", 4.0),
                message("u3", "user", "next", 5.0),
                message("a3", "assistant", "answer three", 6.0),
            ],
        )],
        None,
    )?;
    let branched_snapshot = canonical_snapshot_id(&service, conversation_id)?;
    let branched = MemoryCompilationBatch {
        project: "LEMonX".to_string(),
        conversation_id: conversation_id.to_string(),
        source_snapshot_id: branched_snapshot,
        through_turn_index: 5,
        through_message_id: "a3".to_string(),
        compiler_version: "memory-compiler-v1".to_string(),
        model_label: Some("test-model".to_string()),
        created_at: 40.0,
        candidates: Vec::new(),
    };
    let error = service
        .stage_memory_compilation(&branched)
        .expect_err("changed compiled prefix must fail closed");
    assert!(
        error
            .to_string()
            .contains("does not preserve the previously compiled prefix")
    );
    let stale = service.promote_memory_candidate("candidate-2", 41.0)?;
    assert!(matches!(
        stale,
        MemoryCandidateDecision::Stale { ref candidate_id, .. }
            if candidate_id == "candidate-2"
    ));
    let stale_candidate = service
        .memory_candidate("candidate-2")?
        .expect("stale candidate remains auditable");
    assert_eq!(stale_candidate.status, MemoryCandidateStatus::Stale);
    assert!(
        stale_candidate
            .decision_reason
            .as_deref()
            .is_some_and(|reason| reason.contains("no longer preserves"))
    );
    let working = service.project_working_memory("LEMonX")?;
    assert_eq!(working.items.len(), 1);
    assert_eq!(working.items[0].memory_id, "candidate-memory-1");
    Ok(())
}

#[test]
fn memory_candidate_promotion_covers_lifecycle_operations_and_rejection() -> anyhow::Result<()> {
    let temp = TempDir::new()?;
    let service = service(&temp);
    let conversation_id = "lifecycle-conversation";
    service.import_normalized(
        vec![chatgpt_conversation(
            conversation_id,
            10.0,
            vec![
                message("u1", "user", "set goal", 1.0),
                message("a1", "assistant", "goal acknowledged", 2.0),
            ],
        )],
        None,
    )?;
    let snapshot = canonical_snapshot_id(&service, conversation_id)?;

    let stage = |candidate: MemoryCandidateInput,
                 created_at: f64|
     -> anyhow::Result<MemoryCompilationBatch> {
        Ok(MemoryCompilationBatch {
            project: "LEMonX".to_string(),
            conversation_id: conversation_id.to_string(),
            source_snapshot_id: snapshot.clone(),
            through_turn_index: 1,
            through_message_id: "a1".to_string(),
            compiler_version: "memory-compiler-v1".to_string(),
            model_label: Some("test-model".to_string()),
            created_at,
            candidates: vec![candidate],
        })
    };
    let evidence = |label: &str, created_at: f64| MemoryEvidence {
        kind: MemoryEvidenceKind::ConversationTurn,
        reference: format!("conversation:lifecycle-conversation:{label}"),
        detail: json!({"turn_index": 1}),
        created_at,
    };

    service.stage_memory_compilation(&stage(
        MemoryCandidateInput {
            candidate_id: "life-add".to_string(),
            payload: MemoryCandidatePayload::Add {
                memory_id: "life-memory-1".to_string(),
                kind: MemoryKind::State,
                key: "current_goal".to_string(),
                value: json!({"text": "goal one"}),
                importance: 90,
                confidence: 1.0,
                valid_from: Some(10.0),
                valid_until: None,
                last_verified_at: Some(10.0),
            },
            rationale: "Initial durable goal.".to_string(),
            evidence: vec![evidence("add", 10.0)],
        },
        10.0,
    )?)?;
    service.promote_memory_candidate("life-add", 11.0)?;
    assert_eq!(
        service.project_working_memory("LEMonX")?.items[0].memory_id,
        "life-memory-1"
    );

    service.stage_memory_compilation(&stage(
        MemoryCandidateInput {
            candidate_id: "life-supersede".to_string(),
            payload: MemoryCandidatePayload::Supersede {
                memory_id: "life-memory-2".to_string(),
                target_memory_id: "life-memory-1".to_string(),
                kind: MemoryKind::State,
                key: "current_goal".to_string(),
                value: json!({"text": "goal two"}),
                importance: 95,
                confidence: 1.0,
                valid_from: Some(20.0),
                valid_until: None,
                last_verified_at: Some(20.0),
            },
            rationale: "New evidence explicitly replaces the goal.".to_string(),
            evidence: vec![evidence("supersede", 20.0)],
        },
        20.0,
    )?)?;
    service.promote_memory_candidate("life-supersede", 21.0)?;
    assert_eq!(
        service.get_memory_item("life-memory-1")?.unwrap().status,
        MemoryStatus::Superseded
    );
    assert_eq!(
        service.project_working_memory("LEMonX")?.items[0].memory_id,
        "life-memory-2"
    );

    service.stage_memory_compilation(&stage(
        MemoryCandidateInput {
            candidate_id: "life-resolve".to_string(),
            payload: MemoryCandidatePayload::Resolve {
                target_memory_id: "life-memory-2".to_string(),
            },
            rationale: "The active goal is complete.".to_string(),
            evidence: vec![evidence("resolve", 30.0)],
        },
        30.0,
    )?)?;
    service.promote_memory_candidate("life-resolve", 31.0)?;
    assert_eq!(
        service.get_memory_item("life-memory-2")?.unwrap().status,
        MemoryStatus::Resolved
    );
    assert!(service.project_working_memory("LEMonX")?.items.is_empty());

    service.stage_memory_compilation(&stage(
        MemoryCandidateInput {
            candidate_id: "life-archive".to_string(),
            payload: MemoryCandidatePayload::Archive {
                target_memory_id: "life-memory-2".to_string(),
            },
            rationale: "Resolved goal is no longer useful in active history.".to_string(),
            evidence: vec![evidence("archive", 40.0)],
        },
        40.0,
    )?)?;
    service.promote_memory_candidate("life-archive", 41.0)?;
    let archived = service.get_memory_item("life-memory-2")?.unwrap();
    assert_eq!(archived.status, MemoryStatus::Archived);
    assert_eq!(archived.evidence.len(), 3);

    service.stage_memory_compilation(&stage(
        MemoryCandidateInput {
            candidate_id: "life-reject".to_string(),
            payload: MemoryCandidatePayload::Add {
                memory_id: "life-memory-rejected".to_string(),
                kind: MemoryKind::Hypothesis,
                key: "unverified_guess".to_string(),
                value: json!({"text": "do not persist"}),
                importance: 10,
                confidence: 0.2,
                valid_from: None,
                valid_until: None,
                last_verified_at: None,
            },
            rationale: "Low-confidence hypothesis.".to_string(),
            evidence: vec![evidence("reject", 50.0)],
        },
        50.0,
    )?)?;
    service.reject_memory_candidate("life-reject", "insufficient evidence", 51.0)?;
    let rejected = service.memory_candidate("life-reject")?.unwrap();
    assert_eq!(rejected.status, MemoryCandidateStatus::Rejected);
    assert_eq!(
        rejected.decision_reason.as_deref(),
        Some("insufficient evidence")
    );
    assert!(service.get_memory_item("life-memory-rejected")?.is_none());
    Ok(())
}

#[test]
fn legacy_database_restores_and_upgrades_to_memory_schema() -> anyhow::Result<()> {
    let temp = TempDir::new()?;
    let legacy = temp.path().join("legacy-v1.sqlite3");
    {
        let conn = open_database(&legacy)?;
        conn.execute("DROP TABLE memory_candidate_evidence", [])?;
        conn.execute("DROP TABLE memory_candidates", [])?;
        conn.execute("DROP TABLE memory_compile_checkpoints", [])?;
        conn.execute("DROP TABLE conversation_snapshot_messages", [])?;
        conn.execute("DROP TABLE conversation_snapshots", [])?;
        conn.execute("DROP TABLE memory_evidence", [])?;
        conn.execute("DROP TABLE memory_items", [])?;
        conn.pragma_update(None, "user_version", 1)?;
    }

    let destination = temp.path().join("restored").join("index.sqlite3");
    let report = restore_database(&legacy, &destination)?;
    assert_eq!(report.health.schema_version, 4);

    let conn = open_database(&destination)?;
    let foundation_tables: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name IN ('memory_items', 'memory_evidence', 'conversation_snapshots', 'conversation_snapshot_messages', 'memory_compile_checkpoints', 'memory_candidates', 'memory_candidate_evidence')",
        params![],
        |row| row.get(0),
    )?;
    assert_eq!(foundation_tables, 7);
    Ok(())
}
