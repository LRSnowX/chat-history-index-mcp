use chat_history_core::{
    DataHome, IndexService, MemoryEvidence, MemoryEvidenceKind, MemoryItem, MemoryKind,
    MemoryScope, MemoryStatus, NormalizedConversation, NormalizedMessage,
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
fn legacy_database_restores_and_upgrades_to_memory_schema() -> anyhow::Result<()> {
    let temp = TempDir::new()?;
    let legacy = temp.path().join("legacy-v1.sqlite3");
    {
        let conn = open_database(&legacy)?;
        conn.execute("DROP TABLE memory_evidence", [])?;
        conn.execute("DROP TABLE memory_items", [])?;
        conn.pragma_update(None, "user_version", 1)?;
    }

    let destination = temp.path().join("restored").join("index.sqlite3");
    let report = restore_database(&legacy, &destination)?;
    assert_eq!(report.health.schema_version, 2);

    let conn = open_database(&destination)?;
    let memory_tables: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name IN ('memory_items', 'memory_evidence')",
        params![],
        |row| row.get(0),
    )?;
    assert_eq!(memory_tables, 2);
    Ok(())
}
