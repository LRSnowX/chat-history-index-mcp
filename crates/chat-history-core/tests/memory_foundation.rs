use chat_history_core::{
    CollaborationMemoryAuthoringInput, CollaborationMemoryRetirementInput,
    DEFAULT_MEMORY_COMPILER_MESSAGES, DataHome, IndexService, MemoryAutoPromotionClass,
    MemoryCandidateDecision, MemoryCandidateInput, MemoryCandidatePayload, MemoryCandidateStatus,
    MemoryCheckpointPrefixStatus, MemoryCompilationBatch, MemoryEvidence, MemoryEvidenceKind,
    MemoryItem, MemoryKind, MemoryPromotionReview, MemoryScope, MemoryStatus,
    NormalizedConversation, NormalizedMessage, WorkingMemoryEvidenceStrength,
    WorkingMemoryVerificationState,
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

#[test]
fn project_working_memory_query_ranking_promotes_relevant_items_without_filtering()
-> anyhow::Result<()> {
    let temp = TempDir::new()?;
    let service = service(&temp);
    let make = |id: &str,
                kind: MemoryKind,
                key: &str,
                value: &str,
                importance: u8,
                updated_at: f64|
     -> MemoryItem {
        MemoryItem {
            memory_id: id.to_string(),
            scope: MemoryScope::Project {
                project: "LEMonX".to_string(),
            },
            kind,
            key: key.to_string(),
            value: json!({"text": value}),
            status: MemoryStatus::Active,
            importance,
            confidence: 1.0,
            valid_from: None,
            valid_until: None,
            supersedes_memory_id: None,
            created_at: updated_at,
            updated_at,
            last_verified_at: Some(updated_at),
            evidence: Vec::new(),
        }
    };
    service.put_memory_item(&make(
        "high-decision",
        MemoryKind::Decision,
        "deployment_policy",
        "keep release deployment manual",
        100,
        30.0,
    ))?;
    service.put_memory_item(&make(
        "medium-state",
        MemoryKind::State,
        "current_phase",
        "当前阶段：memory foundation implementation",
        95,
        20.0,
    ))?;
    service.put_memory_item(&make(
        "low-blocker",
        MemoryKind::Blocker,
        "shared_equipment_playwright_blocker",
        "isolated rerun is still blocked",
        60,
        10.0,
    ))?;

    let ranked = service.project_working_memory_for_query("LEMonX", "playwright blocker")?;
    assert_eq!(ranked.items.len(), 3);
    assert_eq!(ranked.items[0].memory_id, "low-blocker");
    assert_eq!(ranked.items[1].memory_id, "high-decision");

    let chinese = service.project_working_memory_for_query("LEMonX", "当前阶段")?;
    assert_eq!(chinese.items[0].memory_id, "medium-state");

    let fallback = service.project_working_memory_for_query("LEMonX", "completely unrelated")?;
    assert_eq!(
        fallback
            .items
            .iter()
            .map(|item| item.memory_id.as_str())
            .collect::<Vec<_>>(),
        vec!["high-decision", "medium-state", "low-blocker"]
    );
    Ok(())
}

#[test]
fn project_working_memory_marks_operational_memory_for_revalidation_after_new_project_evidence()
-> anyhow::Result<()> {
    let temp = TempDir::new()?;
    let service = service(&temp);
    let make = |id: &str,
                kind: MemoryKind,
                evidence_kind: MemoryEvidenceKind,
                valid_until: Option<f64>|
     -> MemoryItem {
        MemoryItem {
            memory_id: id.to_string(),
            scope: MemoryScope::Project {
                project: "LEMonX".to_string(),
            },
            kind,
            key: id.to_string(),
            value: json!({"text": id}),
            status: MemoryStatus::Active,
            importance: 90,
            confidence: 1.0,
            valid_from: Some(if valid_until.is_some() { 1.0 } else { 10.0 }),
            valid_until,
            supersedes_memory_id: None,
            created_at: 10.0,
            updated_at: 10.0,
            last_verified_at: Some(10.0),
            evidence: vec![MemoryEvidence {
                kind: evidence_kind,
                reference: format!("evidence:{id}"),
                detail: json!({}),
                created_at: 10.0,
            }],
        }
    };

    service.put_memory_item(&make(
        "conversation-state",
        MemoryKind::State,
        MemoryEvidenceKind::ConversationTurn,
        None,
    ))?;
    service.put_memory_item(&make(
        "conversation-task",
        MemoryKind::Task,
        MemoryEvidenceKind::UserStatement,
        None,
    ))?;
    service.put_memory_item(&make(
        "repo-blocker",
        MemoryKind::Blocker,
        MemoryEvidenceKind::RepositoryState,
        None,
    ))?;
    service.put_memory_item(&make(
        "stable-result",
        MemoryKind::Result,
        MemoryEvidenceKind::ConversationTurn,
        None,
    ))?;
    service.put_memory_item(&make(
        "tentative-hypothesis",
        MemoryKind::Hypothesis,
        MemoryEvidenceKind::ConversationTurn,
        None,
    ))?;
    service.put_memory_item(&make(
        "expired-task",
        MemoryKind::Task,
        MemoryEvidenceKind::RepositoryState,
        Some(5.0),
    ))?;

    service.import_normalized(
        vec![chatgpt_conversation(
            "newer-project-evidence",
            20.0,
            vec![
                message("u-new", "user", "continue LEMonX", 19.0),
                message("a-new", "assistant", "newer project state", 20.0),
            ],
        )],
        None,
    )?;

    let working = service.project_working_memory("LEMonX")?;
    let verification = |memory_id: &str| {
        working
            .verification
            .iter()
            .find(|item| item.memory_id == memory_id)
            .expect("verification exists")
    };

    let state = verification("conversation-state");
    assert_eq!(
        state.evidence_strength,
        WorkingMemoryEvidenceStrength::ConversationOnly
    );
    assert_eq!(
        state.state,
        WorkingMemoryVerificationState::NeedsRevalidation
    );
    assert_eq!(state.latest_project_evidence_at, Some(20.0));

    let task = verification("conversation-task");
    assert_eq!(
        task.evidence_strength,
        WorkingMemoryEvidenceStrength::UserAsserted
    );
    assert_eq!(
        task.state,
        WorkingMemoryVerificationState::NeedsRevalidation
    );

    let strong = verification("repo-blocker");
    assert_eq!(
        strong.evidence_strength,
        WorkingMemoryEvidenceStrength::StrongIndependent
    );
    assert_eq!(
        strong.state,
        WorkingMemoryVerificationState::StronglyVerified
    );

    assert_eq!(
        verification("stable-result").state,
        WorkingMemoryVerificationState::CurrentByEvidence
    );
    assert_eq!(
        verification("tentative-hypothesis").state,
        WorkingMemoryVerificationState::Tentative
    );
    assert_eq!(
        verification("expired-task").state,
        WorkingMemoryVerificationState::Expired
    );
    Ok(())
}

#[test]
fn collaboration_memory_includes_only_active_global_rules() -> anyhow::Result<()> {
    let temp = TempDir::new()?;
    let service = service(&temp);
    let make = |id: &str, kind: MemoryKind, status: MemoryStatus| MemoryItem {
        memory_id: id.to_string(),
        scope: MemoryScope::Global,
        kind,
        key: id.to_string(),
        value: json!({"text": id}),
        status,
        importance: 90,
        confidence: 1.0,
        valid_from: None,
        valid_until: None,
        supersedes_memory_id: None,
        created_at: 1.0,
        updated_at: 1.0,
        last_verified_at: Some(1.0),
        evidence: Vec::new(),
    };
    service.put_memory_item(&make(
        "preference",
        MemoryKind::Preference,
        MemoryStatus::Active,
    ))?;
    service.put_memory_item(&make(
        "invariant",
        MemoryKind::Invariant,
        MemoryStatus::Active,
    ))?;
    service.put_memory_item(&make(
        "decision",
        MemoryKind::Decision,
        MemoryStatus::Active,
    ))?;
    service.put_memory_item(&make("state", MemoryKind::State, MemoryStatus::Active))?;
    service.put_memory_item(&make(
        "resolved",
        MemoryKind::Preference,
        MemoryStatus::Resolved,
    ))?;

    let memory = service.collaboration_memory()?;
    let ids = memory
        .items
        .iter()
        .map(|item| item.memory_id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(ids, vec!["decision", "invariant", "preference"]);
    Ok(())
}

#[test]
fn collaboration_memory_operator_authoring_requires_review_and_explicit_supersession()
-> anyhow::Result<()> {
    let temp = TempDir::new()?;
    let service = service(&temp);
    let evidence = |reference: &str, at: f64| MemoryEvidence {
        kind: MemoryEvidenceKind::UserStatement,
        reference: reference.to_string(),
        detail: json!({"turn": 1}),
        created_at: at,
    };
    let author = |kind: MemoryKind,
                  value: serde_json::Value,
                  supersedes_memory_id: Option<String>,
                  reason: &str,
                  at: f64|
     -> CollaborationMemoryAuthoringInput {
        CollaborationMemoryAuthoringInput {
            kind,
            key: "subagent_policy".to_string(),
            value,
            importance: 100,
            confidence: 1.0,
            valid_from: None,
            valid_until: None,
            supersedes_memory_id,
            review_reason: reason.to_string(),
            evidence: vec![evidence("conversation:user-policy", at)],
            authored_at: at,
        }
    };

    let invalid = author(
        MemoryKind::State,
        json!({"text": "transient state"}),
        None,
        "not a global stable rule",
        10.0,
    );
    let error = service
        .author_collaboration_memory(&invalid)
        .expect_err("transient kinds must not become collaboration memory");
    assert!(
        error
            .to_string()
            .contains("invariant, preference, or decision")
    );

    let mut missing_review = author(
        MemoryKind::Preference,
        json!({"text": "Do not use subagents without explicit approval."}),
        None,
        "",
        11.0,
    );
    missing_review.evidence.clear();
    assert!(
        service
            .author_collaboration_memory(&missing_review)
            .is_err()
    );

    let first_request = author(
        MemoryKind::Preference,
        json!({
            "text": "Do not use subagents without explicit approval.",
            "scope": "all development projects"
        }),
        None,
        "User explicitly established this as a cross-project collaboration rule.",
        20.0,
    );
    let first = service.author_collaboration_memory(&first_request)?;
    assert_eq!(first.scope, MemoryScope::Global);
    assert_eq!(first.status, MemoryStatus::Active);
    assert_eq!(first.evidence.len(), 1);
    assert_eq!(
        first.evidence[0].detail["review_reason"],
        "User explicitly established this as a cross-project collaboration rule."
    );

    let mut reordered_value = serde_json::Map::new();
    reordered_value.insert("scope".to_string(), json!("all development projects"));
    reordered_value.insert(
        "text".to_string(),
        json!("Do not use subagents without explicit approval."),
    );
    let replay = service.author_collaboration_memory(&CollaborationMemoryAuthoringInput {
        key: "  subagent_policy  ".to_string(),
        value: serde_json::Value::Object(reordered_value),
        authored_at: 99.0,
        ..first_request.clone()
    })?;
    assert_eq!(replay.memory_id, first.memory_id);
    assert_eq!(replay.created_at, first.created_at);
    assert_eq!(replay.evidence, first.evidence);

    let conflicting = author(
        MemoryKind::Preference,
        json!({"text": "Subagents may be used automatically."}),
        None,
        "Attempted silent replacement.",
        30.0,
    );
    let error = service
        .author_collaboration_memory(&conflicting)
        .expect_err("same active key requires explicit supersession");
    assert!(error.to_string().contains("explicitly supersede"));

    let replacement_request = author(
        MemoryKind::Decision,
        json!({"text": "Subagents remain explicit-approval only unless policy is changed by the user."}),
        Some(first.memory_id.clone()),
        "Operator reviewed the clarified global decision.",
        40.0,
    );
    let replacement = service.author_collaboration_memory(&replacement_request)?;
    assert_eq!(
        service.get_memory_item(&first.memory_id)?.unwrap().status,
        MemoryStatus::Superseded
    );
    let collaboration = service.collaboration_memory()?;
    assert_eq!(collaboration.items.len(), 1);
    assert_eq!(collaboration.items[0].memory_id, replacement.memory_id);

    let retired = service.retire_collaboration_memory(&CollaborationMemoryRetirementInput {
        memory_id: replacement.memory_id.clone(),
        review_reason: "The global rule was explicitly retired by the operator.".to_string(),
        evidence: vec![evidence("conversation:user-retirement", 50.0)],
        retired_at: 50.0,
    })?;
    assert_eq!(retired.status, MemoryStatus::Archived);
    assert_eq!(retired.evidence.len(), 2);
    assert!(service.collaboration_memory()?.items.is_empty());
    let replay_retire =
        service.retire_collaboration_memory(&CollaborationMemoryRetirementInput {
            memory_id: replacement.memory_id,
            review_reason: "Repeated retirement command.".to_string(),
            evidence: vec![evidence("conversation:user-retirement", 60.0)],
            retired_at: 60.0,
        })?;
    assert_eq!(replay_retire.status, MemoryStatus::Archived);
    assert_eq!(replay_retire.evidence.len(), 2);
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
fn project_pending_memory_is_newest_first_and_excludes_revalidation_problems() -> anyhow::Result<()>
{
    let temp = TempDir::new()?;
    let service = service(&temp);
    let conversation_id = "pending-memory-context";
    service.import_normalized(
        vec![chatgpt_conversation(
            conversation_id,
            10.0,
            vec![
                message("u1", "user", "start", 1.0),
                message("a1", "assistant", "answer", 2.0),
            ],
        )],
        None,
    )?;
    service.put_memory_item(&MemoryItem {
        memory_id: "active-conflict".to_string(),
        scope: MemoryScope::Project {
            project: "LEMonX".to_string(),
        },
        kind: MemoryKind::State,
        key: "conflicting-key".to_string(),
        value: json!({"text": "authoritative active value"}),
        status: MemoryStatus::Active,
        importance: 90,
        confidence: 1.0,
        valid_from: Some(1.0),
        valid_until: None,
        supersedes_memory_id: None,
        created_at: 1.0,
        updated_at: 1.0,
        last_verified_at: Some(1.0),
        evidence: Vec::new(),
    })?;

    let candidate = |candidate_id: &str, key: &str| MemoryCandidateInput {
        candidate_id: candidate_id.to_string(),
        payload: MemoryCandidatePayload::Add {
            memory_id: format!("memory-{candidate_id}"),
            kind: MemoryKind::State,
            key: key.to_string(),
            value: json!({"text": candidate_id}),
            importance: 80,
            confidence: 0.9,
            valid_from: Some(20.0),
            valid_until: None,
            last_verified_at: Some(20.0),
        },
        rationale: format!("private rationale for {candidate_id}"),
        evidence: Vec::new(),
    };
    service.stage_memory_compilation(&MemoryCompilationBatch {
        project: "LEMonX".to_string(),
        conversation_id: conversation_id.to_string(),
        source_snapshot_id: canonical_snapshot_id(&service, conversation_id)?,
        through_turn_index: 1,
        through_message_id: "a1".to_string(),
        compiler_version: "memory-compiler-v1".to_string(),
        model_label: Some("private-model-label".to_string()),
        created_at: 20.0,
        candidates: vec![
            candidate("candidate-old", "old-key"),
            candidate("candidate-middle", "middle-key"),
            candidate("candidate-newest", "newest-key"),
            candidate("candidate-invalid", "conflicting-key"),
            candidate("candidate-rejected", "rejected-key"),
        ],
    })?;
    service.reject_memory_candidate("candidate-rejected", "private review text", 60.0)?;
    let conn = open_database(&service.managed_db_path())?;
    for (candidate_id, created_at) in [
        ("candidate-old", 30.0),
        ("candidate-middle", 40.0),
        ("candidate-invalid", 45.0),
        ("candidate-newest", 50.0),
        ("candidate-rejected", 60.0),
    ] {
        conn.execute(
            "UPDATE memory_candidates SET created_at = ?2 WHERE candidate_id = ?1",
            params![candidate_id, created_at],
        )?;
    }

    let pending = service.project_pending_memory("LEMonX", 2)?;
    assert_eq!(pending.project, "LEMonX");
    assert!(pending.generated_at.is_finite());
    assert_eq!(pending.revalidation_excluded_count, 1);
    assert_eq!(
        pending
            .items
            .iter()
            .map(|item| item.candidate_id.as_str())
            .collect::<Vec<_>>(),
        vec!["candidate-newest", "candidate-middle"]
    );
    assert_eq!(pending.items[0].conversation_id, conversation_id);
    assert_eq!(pending.items[0].through_turn_index, 1);

    let serialized = serde_json::to_value(&pending.items[0])?;
    assert!(serialized.get("rationale").is_none());
    assert!(serialized.get("model_label").is_none());
    assert!(serialized.get("decision_reason").is_none());
    Ok(())
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
    let initial_reviews = service.memory_candidate_reviews("candidate-1")?;
    assert_eq!(initial_reviews.len(), 1);
    assert_eq!(initial_reviews[0].outcome, "promoted");
    assert_eq!(initial_reviews[0].reason, "validated and promoted");
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
    let healthy = service.memory_health("LEMonX", 30)?;
    assert_eq!(healthy.memory_items.active, 1);
    assert_eq!(healthy.candidates.pending, 1);
    assert_eq!(healthy.checkpoint_count, 1);
    assert_eq!(healthy.checkpoint_caught_up, 1);
    assert_eq!(healthy.checkpoint_behind, 0);
    assert_eq!(healthy.checkpoint_prefix_problem, 0);
    assert_eq!(
        healthy.checkpoints[0].prefix_status,
        MemoryCheckpointPrefixStatus::Valid
    );

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
    let stale = service.promote_memory_candidate_with_review(
        "candidate-2",
        41.0,
        &MemoryPromotionReview {
            reason: Some("operator checked current repository state".to_string()),
            evidence: vec![MemoryEvidence {
                kind: MemoryEvidenceKind::RepositoryState,
                reference: "LEMonX@branched".to_string(),
                detail: json!({"clean": true}),
                created_at: 41.0,
            }],
        },
    )?;
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
    let stale_reviews = service.memory_candidate_reviews("candidate-2")?;
    assert_eq!(stale_reviews.len(), 1);
    assert_eq!(stale_reviews[0].outcome, "stale");
    assert_eq!(
        stale_reviews[0].reason,
        "operator checked current repository state"
    );
    assert_eq!(stale_reviews[0].evidence.len(), 1);
    let working = service.project_working_memory("LEMonX")?;
    assert_eq!(working.items.len(), 1);
    assert_eq!(working.items[0].memory_id, "candidate-memory-1");
    let unhealthy = service.memory_health("LEMonX", 30)?;
    assert_eq!(unhealthy.candidates.stale, 1);
    assert_eq!(unhealthy.checkpoint_caught_up, 0);
    assert_eq!(unhealthy.checkpoint_prefix_problem, 1);
    assert_eq!(
        unhealthy.checkpoints[0].prefix_status,
        MemoryCheckpointPrefixStatus::Changed
    );
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

#[tokio::test]
async fn project_memory_compile_plan_is_bounded_and_does_not_stage_candidates() -> anyhow::Result<()>
{
    let temp = TempDir::new()?;
    let service = service(&temp);
    let matching_id = "plan-lemonx";
    let incomplete_id = "plan-lemonx-incomplete";
    let other_id = "plan-other";
    service.import_normalized(
        vec![
            chatgpt_conversation(
                matching_id,
                20.0,
                vec![
                    message("u1", "user", "start", 1.0),
                    message("a1", "assistant", "continue", 2.0),
                    message("u2", "user", "next", 3.0),
                    message("a2", "assistant", "done", 4.0),
                ],
            ),
            chatgpt_conversation(
                incomplete_id,
                19.5,
                vec![
                    message("iu1", "user", "continue", 1.0),
                    message("iu2", "user", "continue again", 2.0),
                ],
            ),
            {
                let mut conversation = chatgpt_conversation(
                    other_id,
                    19.0,
                    vec![
                        message("ou1", "user", "other", 1.0),
                        message("oa1", "assistant", "other", 2.0),
                    ],
                );
                conversation.title = "Unrelated project".to_string();
                conversation.source_path = Some("chatgpt-app-bridge".to_string());
                conversation
            },
        ],
        None,
    )?;

    let plan = service.plan_memory_project("LEMonX", 50, 2, 2).await?;
    assert_eq!(plan.matched, 2);
    assert_eq!(plan.caught_up, 0);
    assert_eq!(plan.ready.len(), 1);
    assert_eq!(plan.ready[0].conversation_id, matching_id);
    assert_eq!(plan.ready[0].from_turn_index, 0);
    assert_eq!(plan.ready[0].through_turn_index, 1);
    assert_eq!(plan.ready[0].message_count, 2);
    assert_eq!(plan.failures.len(), 1);
    assert_eq!(plan.failures[0].conversation_id, incomplete_id);
    assert!(
        plan.failures[0]
            .error
            .contains("multi-message transcript has no assistant messages")
    );
    let error = service
        .prepare_memory_compilation("LEMonX", incomplete_id, 2)
        .expect_err("incomplete ChatGPT evidence must fail before model invocation");
    assert!(
        error
            .to_string()
            .contains("multi-message transcript has no assistant messages")
    );
    assert!(service.pending_memory_candidates("LEMonX")?.is_empty());

    let input = service
        .prepare_memory_compilation("LEMonX", matching_id, 16)?
        .expect("matching conversation has work");
    service.stage_memory_compilation(&MemoryCompilationBatch {
        project: "LEMonX".to_string(),
        conversation_id: matching_id.to_string(),
        source_snapshot_id: input.source_snapshot_id,
        through_turn_index: 3,
        through_message_id: "a2".to_string(),
        compiler_version: "plan-test".to_string(),
        model_label: Some("none".to_string()),
        created_at: 30.0,
        candidates: Vec::new(),
    })?;

    let caught_up = service.plan_memory_project("LEMonX", 50, 2, 2).await?;
    assert_eq!(caught_up.matched, 2);
    assert_eq!(caught_up.caught_up, 1);
    assert!(caught_up.ready.is_empty());
    assert_eq!(caught_up.failures.len(), 1);
    assert_eq!(caught_up.failures[0].conversation_id, incomplete_id);
    assert!(service.pending_memory_candidates("LEMonX")?.is_empty());
    Ok(())
}

#[test]
fn model_compiler_stages_only_valid_delta_evidence_and_keeps_ids_deterministic()
-> anyhow::Result<()> {
    let temp = TempDir::new()?;
    let service = service(&temp);
    let conversation_id = "model-compiler-conversation";
    service.import_normalized(
        vec![chatgpt_conversation(
            conversation_id,
            10.0,
            vec![
                message("u1", "user", "Start checkpoint A.", 1.0),
                message("a1", "assistant", "Checkpoint A is active.", 2.0),
                message("u2", "user", "Do not use subagents.", 3.0),
                message(
                    "a2",
                    "assistant",
                    "Subagents require explicit approval.",
                    4.0,
                ),
            ],
        )],
        None,
    )?;

    let input = service
        .prepare_memory_compilation("LEMonX", conversation_id, 2)?
        .expect("compiler has an initial delta");
    assert_eq!(input.from_turn_index, 0);
    assert_eq!(input.through_turn_index, 1);
    assert_eq!(
        input
            .messages
            .iter()
            .map(|message| message.message_id.as_str())
            .collect::<Vec<_>>(),
        vec!["u1", "a1"]
    );
    assert!(input.working_memory.items.is_empty());

    let invalid = json!({
        "proposals": [{
            "operation": "add",
            "kind": "state",
            "key": "checkpoint",
            "value": {"name": "A"},
            "importance": 90,
            "confidence": 0.9,
            "rationale": "Current checkpoint.",
            "evidence_message_ids": ["outside-delta"]
        }]
    })
    .to_string();
    let error = service
        .stage_memory_compiler_output(&input, "test-model", &invalid, 20.0)
        .expect_err("evidence outside the supplied delta must be rejected");
    assert!(
        error
            .to_string()
            .contains("outside the supplied compiler delta")
    );
    assert!(
        service
            .memory_compile_checkpoint("LEMonX", conversation_id)?
            .is_none()
    );

    let missing_proposals = json!({}).to_string();
    let error = service
        .stage_memory_compiler_output(&input, "test-model", &missing_proposals, 20.0)
        .expect_err("proposals must be an explicit top-level key");
    assert!(format!("{error:#}").contains("missing field `proposals`"));
    assert!(
        service
            .memory_compile_checkpoint("LEMonX", conversation_id)?
            .is_none()
    );

    let unknown_field = json!({
        "proposals": [],
        "unexpected": true
    })
    .to_string();
    let error = service
        .stage_memory_compiler_output(&input, "test-model", &unknown_field, 20.0)
        .expect_err("unknown compiler fields must be rejected");
    assert!(format!("{error:#}").contains("unknown field"));
    assert!(
        service
            .memory_compile_checkpoint("LEMonX", conversation_id)?
            .is_none()
    );

    let duplicate_key = json!({
        "proposals": [
            {
                "operation": "add",
                "kind": "state",
                "key": "checkpoint",
                "value": {"name": "A"},
                "importance": 90,
                "confidence": 0.9,
                "rationale": "First duplicate.",
                "evidence_message_ids": ["a1"]
            },
            {
                "operation": "add",
                "kind": "state",
                "key": "checkpoint",
                "value": {"name": "A again"},
                "importance": 80,
                "confidence": 0.8,
                "rationale": "Second duplicate.",
                "evidence_message_ids": ["a1"]
            }
        ]
    })
    .to_string();
    let error = service
        .stage_memory_compiler_output(&input, "test-model", &duplicate_key, 20.0)
        .expect_err("one compiler batch must not propose the same key twice");
    assert!(
        error
            .to_string()
            .contains("multiple proposals for key checkpoint")
    );
    assert!(
        service
            .memory_compile_checkpoint("LEMonX", conversation_id)?
            .is_none()
    );

    let valid = json!({
        "proposals": [{
            "operation": "add",
            "kind": "state",
            "key": "checkpoint",
            "value": {"name": "A"},
            "importance": 90,
            "confidence": 0.9,
            "rationale": "The supplied dialogue establishes checkpoint A.",
            "evidence_message_ids": ["a1"]
        }]
    })
    .to_string();
    let staged_one = service.stage_memory_compiler_output(&input, "test-model", &valid, 20.0)?;
    let staged_two = service.stage_memory_compiler_output(&input, "test-model", &valid, 20.0)?;
    assert_eq!(staged_one.candidate_ids, staged_two.candidate_ids);
    assert_eq!(staged_one.candidate_ids.len(), 1);
    let candidate = service
        .memory_candidate(&staged_one.candidate_ids[0])?
        .expect("candidate staged");
    assert!(candidate.candidate_id.starts_with("memory-candidate-v1:"));
    match candidate.payload {
        MemoryCandidatePayload::Add { memory_id, .. } => {
            assert!(memory_id.starts_with("memory-item-v1:"));
        }
        _ => panic!("expected add candidate"),
    }
    assert_eq!(candidate.evidence.len(), 1);
    assert_eq!(
        candidate.evidence[0].reference,
        "conversation:model-compiler-conversation:message:a1"
    );
    assert!(service.project_working_memory("LEMonX")?.items.is_empty());

    service.import_normalized(
        vec![chatgpt_conversation(
            conversation_id,
            11.0,
            vec![
                message("u1", "user", "Start checkpoint A.", 1.0),
                message("a1", "assistant", "Checkpoint A is active.", 2.0),
                message("u2", "user", "Do not use subagents.", 3.0),
                message(
                    "a2",
                    "assistant",
                    "Subagents require explicit approval.",
                    4.0,
                ),
                message("u3", "user", "Continue implementation.", 5.0),
                message("a3", "assistant", "Continuing the bounded slice.", 6.0),
            ],
        )],
        None,
    )?;
    let next = service
        .prepare_memory_compilation("LEMonX", conversation_id, DEFAULT_MEMORY_COMPILER_MESSAGES)?
        .expect("new delta exists");
    assert_eq!(next.from_turn_index, 2);
    assert_eq!(next.messages[0].message_id, "u2");
    assert_eq!(next.pending_candidates.len(), 1);
    assert_eq!(
        next.pending_candidates[0].candidate_id,
        staged_one.candidate_ids[0]
    );

    service.import_normalized(
        vec![chatgpt_conversation(
            conversation_id,
            12.0,
            vec![
                message("u1-branch", "user", "Changed earlier history.", 1.0),
                message("a1", "assistant", "Checkpoint A is active.", 2.0),
                message("u2", "user", "Do not use subagents.", 3.0),
                message(
                    "a2",
                    "assistant",
                    "Subagents require explicit approval.",
                    4.0,
                ),
                message("u3", "user", "Continue implementation.", 5.0),
                message("a3", "assistant", "Continuing the bounded slice.", 6.0),
            ],
        )],
        None,
    )?;
    let error = service
        .prepare_memory_compilation("LEMonX", conversation_id, 2)
        .expect_err("edited compiled prefix must fail before model invocation");
    assert!(
        error
            .to_string()
            .contains("no longer preserves the compiled prefix")
    );
    Ok(())
}

#[test]
fn model_compiler_rejects_project_mismatch_and_revalidates_live_working_memory()
-> anyhow::Result<()> {
    let temp = TempDir::new()?;
    let service = service(&temp);
    service.import_normalized(
        vec![NormalizedConversation {
            source: "chatgpt".to_string(),
            source_instance: None,
            source_conversation_id: "other-project-conversation".to_string(),
            title: "OtherProject development".to_string(),
            create_time: Some(1.0),
            update_time: Some(2.0),
            model: None,
            source_url: None,
            source_path: Some("chatgpt-app-bridge".to_string()),
            messages: vec![
                message("u1", "user", "Start unrelated work.", 1.0),
                message("a1", "assistant", "Unrelated work active.", 2.0),
            ],
            raw: json!({"collector": "test"}),
        }],
        None,
    )?;
    let error = service
        .prepare_memory_compilation("LEMonX", "other-project-conversation", 2)
        .expect_err("compiler must not relabel an unrelated conversation");
    assert!(
        error
            .to_string()
            .contains("does not strongly match project LEMonX")
    );

    let conversation_id = "live-memory-revalidation";
    service.import_normalized(
        vec![chatgpt_conversation(
            conversation_id,
            10.0,
            vec![
                message("u1", "user", "Start checkpoint A.", 1.0),
                message("a1", "assistant", "Checkpoint A is active.", 2.0),
            ],
        )],
        None,
    )?;
    let input = service
        .prepare_memory_compilation("LEMonX", conversation_id, 2)?
        .expect("compiler delta exists");
    assert!(input.working_memory.items.is_empty());

    service.put_memory_item(&project_memory(
        "live-checkpoint",
        "already active",
        None,
        15.0,
    ))?;
    let add_conflict = json!({
        "proposals": [{
            "operation": "add",
            "kind": "state",
            "key": "current_goal",
            "value": {"text": "stale compiler view"},
            "importance": 90,
            "confidence": 0.9,
            "rationale": "The earlier compiler input did not contain the new live memory.",
            "evidence_message_ids": ["a1"]
        }]
    })
    .to_string();
    let error = service
        .stage_memory_compiler_output(&input, "test-model", &add_conflict, 20.0)
        .expect_err("stage must revalidate against current active memory");
    assert!(
        error
            .to_string()
            .contains("conflicts with active memory key")
    );
    assert!(
        service
            .memory_compile_checkpoint("LEMonX", conversation_id)?
            .is_none()
    );
    Ok(())
}

#[test]
fn memory_health_surfaces_incomplete_project_evidence_and_pending_revalidation_problems()
-> anyhow::Result<()> {
    let temp = TempDir::new()?;
    let service = service(&temp);

    service.import_normalized(
        vec![
            chatgpt_conversation(
                "health-complete",
                10.0,
                vec![
                    message("u1", "user", "Start LEMonX work.", 1.0),
                    message("a1", "assistant", "LEMonX work active.", 2.0),
                ],
            ),
            chatgpt_conversation(
                "health-incomplete",
                11.0,
                vec![
                    message("u1", "user", "continue", 1.0),
                    message("u2", "user", "continue again", 2.0),
                    message("u3", "user", "still waiting", 3.0),
                ],
            ),
            {
                let mut unrelated = chatgpt_conversation(
                    "health-unrelated",
                    12.0,
                    vec![
                        message("u1", "user", "other project", 1.0),
                        message("u2", "user", "still other project", 2.0),
                    ],
                );
                unrelated.title = "OtherProject development".to_string();
                unrelated
            },
        ],
        None,
    )?;

    service.put_memory_item(&project_memory(
        "health-existing-goal",
        "already active",
        None,
        15.0,
    ))?;
    service.put_memory_item(&MemoryItem {
        memory_id: "health-old-task".to_string(),
        scope: MemoryScope::Project {
            project: "LEMonX".to_string(),
        },
        kind: MemoryKind::Task,
        key: "older_operational_task".to_string(),
        value: json!({"text": "continue the older task"}),
        status: MemoryStatus::Active,
        importance: 70,
        confidence: 0.9,
        valid_from: Some(5.0),
        valid_until: None,
        supersedes_memory_id: None,
        created_at: 5.0,
        updated_at: 5.0,
        last_verified_at: Some(5.0),
        evidence: vec![MemoryEvidence {
            kind: MemoryEvidenceKind::ConversationTurn,
            reference: "conversation:older:message:a1".to_string(),
            detail: json!({"turn_index": 1}),
            created_at: 5.0,
        }],
    })?;
    let snapshot = canonical_snapshot_id(&service, "health-complete")?;
    service.stage_memory_compilation(&MemoryCompilationBatch {
        project: "LEMonX".to_string(),
        conversation_id: "health-complete".to_string(),
        source_snapshot_id: snapshot,
        through_turn_index: 1,
        through_message_id: "a1".to_string(),
        compiler_version: "memory-compiler-v1".to_string(),
        model_label: Some("test-model".to_string()),
        created_at: 20.0,
        candidates: vec![MemoryCandidateInput {
            candidate_id: "health-conflicting-candidate".to_string(),
            payload: MemoryCandidatePayload::Add {
                memory_id: "health-conflicting-memory".to_string(),
                kind: MemoryKind::State,
                key: "current_goal".to_string(),
                value: json!({"text": "conflicting goal"}),
                importance: 80,
                confidence: 0.9,
                valid_from: Some(20.0),
                valid_until: None,
                last_verified_at: Some(20.0),
            },
            rationale: "Candidate predates current active-memory revalidation.".to_string(),
            evidence: vec![MemoryEvidence {
                kind: MemoryEvidenceKind::ConversationTurn,
                reference: "conversation:health-complete:message:a1".to_string(),
                detail: json!({"turn_index": 1}),
                created_at: 20.0,
            }],
        }],
    })?;

    let health = service.memory_health("LEMonX", 30)?;
    assert_eq!(health.strong_project_conversation_count, 2);
    assert_eq!(health.working_memory_verification.needs_revalidation, 1);
    assert_eq!(health.working_memory_verification.current_by_evidence, 1);
    assert_eq!(health.working_memory_flagged_count, 1);
    assert_eq!(health.working_memory_flagged.len(), 1);
    assert_eq!(
        health.working_memory_flagged[0].memory_id,
        "health-old-task"
    );
    assert_eq!(
        health.working_memory_flagged[0].state,
        WorkingMemoryVerificationState::NeedsRevalidation
    );
    assert_eq!(health.incomplete_canonical_conversation_count, 1);
    assert_eq!(health.incomplete_canonical_conversations.len(), 1);
    assert_eq!(
        health.incomplete_canonical_conversations[0].conversation_id,
        "health-incomplete"
    );
    assert_eq!(
        health.incomplete_canonical_conversations[0].assistant_message_count,
        0
    );
    assert_eq!(health.pending_revalidation_problem_count, 1);
    assert_eq!(health.pending_revalidation_problems.len(), 1);
    assert_eq!(
        health.pending_revalidation_problems[0].candidate_id,
        "health-conflicting-candidate"
    );
    assert!(
        health.pending_revalidation_problems[0]
            .reason
            .contains("active memory already exists")
    );
    assert_eq!(
        service
            .memory_candidate("health-conflicting-candidate")?
            .unwrap()
            .status,
        MemoryCandidateStatus::Pending
    );
    Ok(())
}

#[test]
fn schema_five_backfills_legacy_promoted_candidate_review_once() -> anyhow::Result<()> {
    let temp = TempDir::new()?;
    let service = service(&temp);
    let conversation_id = "legacy-review-conversation";
    service.import_normalized(
        vec![chatgpt_conversation(
            conversation_id,
            10.0,
            vec![
                message("u1", "user", "start", 1.0),
                message("a1", "assistant", "answer", 2.0),
            ],
        )],
        None,
    )?;
    let snapshot_id = canonical_snapshot_id(&service, conversation_id)?;
    let db_path = service.managed_db_path();
    {
        let conn = open_database(&db_path)?;
        conn.execute(
            r#"
            INSERT INTO memory_candidates (
              candidate_id, project, conversation_id, source_snapshot_id, operation,
              payload_json, status, rationale, compiler_version, model_label,
              through_turn_index, created_at, decided_at, decision_reason
            )
            VALUES (
              'legacy-promoted', 'LEMonX', ?1, ?2, 'add',
              '{}', 'promoted', 'legacy rationale', 'legacy-compiler', NULL,
              1, 10.0, 11.0, 'legacy verified reason'
            )
            "#,
            params![conversation_id, snapshot_id],
        )?;
        conn.execute("DROP TABLE memory_candidate_reviews", [])?;
        conn.pragma_update(None, "user_version", 4)?;
    }

    drop(open_database(&db_path)?);
    let reviews = service.memory_candidate_reviews("legacy-promoted")?;
    assert_eq!(reviews.len(), 1);
    assert_eq!(reviews[0].outcome, "promoted");
    assert_eq!(reviews[0].reason, "legacy verified reason");
    assert!(reviews[0].evidence.is_empty());
    assert_eq!(reviews[0].decided_at, 11.0);

    drop(open_database(&db_path)?);
    assert_eq!(
        service.memory_candidate_reviews("legacy-promoted")?.len(),
        1
    );
    Ok(())
}

#[test]
fn auto_promotion_plan_is_conservative_explainable_and_read_only() -> anyhow::Result<()> {
    let temp = TempDir::new()?;
    let service = service(&temp);
    let conversation_id = "promotion-policy-conversation";
    service.import_normalized(
        vec![chatgpt_conversation(
            conversation_id,
            10.0,
            vec![
                message("u1", "user", "set policy and continue work", 1.0),
                message("a1", "assistant", "acknowledged", 2.0),
            ],
        )],
        None,
    )?;
    let snapshot = canonical_snapshot_id(&service, conversation_id)?;
    service.put_memory_item(&MemoryItem {
        memory_id: "policy-target".to_string(),
        scope: MemoryScope::Project {
            project: "LEMonX".to_string(),
        },
        kind: MemoryKind::Task,
        key: "policy_task".to_string(),
        value: json!({"text": "finish verification"}),
        status: MemoryStatus::Active,
        importance: 90,
        confidence: 1.0,
        valid_from: Some(5.0),
        valid_until: None,
        supersedes_memory_id: None,
        created_at: 5.0,
        updated_at: 5.0,
        last_verified_at: Some(5.0),
        evidence: Vec::new(),
    })?;

    let evidence = |kind, reference: &str| MemoryEvidence {
        kind,
        reference: reference.to_string(),
        detail: json!({}),
        created_at: 10.0,
    };
    let new_memory = |candidate_id: &str,
                      kind: MemoryKind,
                      key: &str,
                      confidence: f64,
                      evidence: Vec<MemoryEvidence>| {
        MemoryCandidateInput {
            candidate_id: candidate_id.to_string(),
            payload: MemoryCandidatePayload::Add {
                memory_id: format!("memory-{candidate_id}"),
                kind,
                key: key.to_string(),
                value: json!({"text": candidate_id}),
                importance: 90,
                confidence,
                valid_from: Some(10.0),
                valid_until: None,
                last_verified_at: Some(10.0),
            },
            rationale: format!("policy test {candidate_id}"),
            evidence,
        }
    };

    service.stage_memory_compilation(&MemoryCompilationBatch {
        project: "LEMonX".to_string(),
        conversation_id: conversation_id.to_string(),
        source_snapshot_id: snapshot,
        through_turn_index: 1,
        through_message_id: "a1".to_string(),
        compiler_version: "memory-compiler-v1".to_string(),
        model_label: Some("test-model".to_string()),
        created_at: 10.0,
        candidates: vec![
            new_memory(
                "policy-user-rule",
                MemoryKind::Decision,
                "explicit_rule",
                0.99,
                vec![evidence(
                    MemoryEvidenceKind::UserStatement,
                    "user:explicit-rule",
                )],
            ),
            new_memory(
                "policy-operational-result",
                MemoryKind::Result,
                "verified_result",
                0.99,
                vec![evidence(
                    MemoryEvidenceKind::DevspaceResult,
                    "devspace:verified-result",
                )],
            ),
            MemoryCandidateInput {
                candidate_id: "policy-resolve".to_string(),
                payload: MemoryCandidatePayload::Resolve {
                    target_memory_id: "policy-target".to_string(),
                },
                rationale: "verified task completion".to_string(),
                evidence: vec![evidence(
                    MemoryEvidenceKind::RepositoryState,
                    "repo:verified-task-state",
                )],
            },
            new_memory(
                "policy-conversation-only",
                MemoryKind::State,
                "conversation_only",
                0.99,
                vec![evidence(
                    MemoryEvidenceKind::ConversationTurn,
                    "conversation:promotion-policy-conversation:message:a1",
                )],
            ),
            new_memory(
                "policy-low-confidence",
                MemoryKind::Decision,
                "low_confidence_rule",
                0.95,
                vec![evidence(
                    MemoryEvidenceKind::UserStatement,
                    "user:low-confidence",
                )],
            ),
            new_memory(
                "policy-hypothesis",
                MemoryKind::Hypothesis,
                "hypothesis",
                0.99,
                vec![evidence(
                    MemoryEvidenceKind::UserStatement,
                    "user:hypothesis",
                )],
            ),
            MemoryCandidateInput {
                candidate_id: "policy-archive".to_string(),
                payload: MemoryCandidatePayload::Archive {
                    target_memory_id: "policy-target".to_string(),
                },
                rationale: "archive proposal requires judgement".to_string(),
                evidence: vec![evidence(
                    MemoryEvidenceKind::DevspaceResult,
                    "devspace:archive-signal",
                )],
            },
            new_memory(
                "policy-late-conflict",
                MemoryKind::State,
                "late_conflict",
                0.99,
                vec![evidence(
                    MemoryEvidenceKind::RepositoryState,
                    "repo:late-conflict",
                )],
            ),
        ],
    })?;

    service.put_memory_item(&MemoryItem {
        memory_id: "late-conflict-existing".to_string(),
        scope: MemoryScope::Project {
            project: "LEMonX".to_string(),
        },
        kind: MemoryKind::State,
        key: "late_conflict".to_string(),
        value: json!({"text": "existing"}),
        status: MemoryStatus::Active,
        importance: 90,
        confidence: 1.0,
        valid_from: Some(11.0),
        valid_until: None,
        supersedes_memory_id: None,
        created_at: 11.0,
        updated_at: 11.0,
        last_verified_at: Some(11.0),
        evidence: Vec::new(),
    })?;

    let plan = service.memory_auto_promotion_plan("LEMonX")?;
    assert_eq!(plan.policy_version, "conservative-v1");
    assert_eq!(plan.pending_count, 8);
    assert_eq!(plan.eligible_count, 3);
    assert_eq!(plan.requires_review_count, 5);
    let by_id = plan
        .evaluations
        .iter()
        .map(|evaluation| (evaluation.candidate_id.as_str(), evaluation))
        .collect::<std::collections::HashMap<_, _>>();

    assert_eq!(
        by_id["policy-user-rule"].class,
        Some(MemoryAutoPromotionClass::UserAssertedRule)
    );
    assert!(by_id["policy-user-rule"].eligible);
    assert_eq!(
        by_id["policy-operational-result"].class,
        Some(MemoryAutoPromotionClass::VerifiedOperationalMemory)
    );
    assert!(by_id["policy-operational-result"].eligible);
    assert_eq!(
        by_id["policy-resolve"].class,
        Some(MemoryAutoPromotionClass::VerifiedOperationalResolution)
    );
    assert!(by_id["policy-resolve"].eligible);
    assert_eq!(
        by_id["policy-conversation-only"].blocker_codes,
        vec!["operational_memory_requires_verified_evidence"]
    );
    assert_eq!(
        by_id["policy-low-confidence"].blocker_codes,
        vec!["confidence_below_policy_threshold"]
    );
    assert_eq!(
        by_id["policy-hypothesis"].blocker_codes,
        vec!["hypothesis_requires_review"]
    );
    assert_eq!(
        by_id["policy-archive"].blocker_codes,
        vec!["archive_requires_review"]
    );
    assert!(
        by_id["policy-late-conflict"]
            .blocker_codes
            .contains(&"revalidation_failed".to_string())
    );
    assert!(by_id["policy-late-conflict"].revalidation_problem.is_some());

    assert_eq!(service.pending_memory_candidates("LEMonX")?.len(), 8);
    assert_eq!(service.project_working_memory("LEMonX")?.items.len(), 2);
    assert!(
        service
            .get_memory_item("memory-policy-user-rule")?
            .is_none()
    );
    Ok(())
}

#[test]
fn legacy_database_restores_and_upgrades_to_memory_schema() -> anyhow::Result<()> {
    let temp = TempDir::new()?;
    let legacy = temp.path().join("legacy-v1.sqlite3");
    {
        let conn = open_database(&legacy)?;
        conn.execute("DROP TABLE memory_candidate_reviews", [])?;
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
    assert_eq!(report.health.schema_version, 5);

    let conn = open_database(&destination)?;
    let foundation_tables: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name IN ('memory_items', 'memory_evidence', 'conversation_snapshots', 'conversation_snapshot_messages', 'memory_compile_checkpoints', 'memory_candidates', 'memory_candidate_evidence', 'memory_candidate_reviews')",
        params![],
        |row| row.get(0),
    )?;
    assert_eq!(foundation_tables, 8);
    Ok(())
}
