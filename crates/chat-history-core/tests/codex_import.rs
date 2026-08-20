use std::{fs, io::Write};

use chat_history_core::{DataHome, IndexService, codex::parse_rollout};
use rusqlite::params;
use tempfile::TempDir;

#[test]
fn parses_codex_rollout_and_invalidates_changed_derivatives() -> anyhow::Result<()> {
    let tempdir = TempDir::new()?;
    let rollout = tempdir.path().join("rollout.jsonl");
    let mut file = fs::File::create(&rollout)?;
    writeln!(
        file,
        r#"{{"timestamp":"2026-08-20T12:00:00Z","type":"session_meta","payload":{{"id":"session-test","timestamp":"2026-08-20T12:00:00Z"}}}}"#
    )?;
    writeln!(
        file,
        r#"{{"timestamp":"2026-08-20T12:00:01Z","type":"turn_context","payload":{{"model":"gpt-test"}}}}"#
    )?;
    writeln!(
        file,
        r#"{{"timestamp":"2026-08-20T12:00:02Z","type":"event_msg","payload":{{"type":"user_message","message":"Build a universal conversation index"}}}}"#
    )?;
    writeln!(
        file,
        r#"{{"timestamp":"2026-08-20T12:00:03Z","type":"event_msg","payload":{{"type":"agent_message","message":"Starting the index."}}}}"#
    )?;
    writeln!(
        file,
        r#"{{"timestamp":"2026-08-20T12:00:03Z","type":"event_msg","payload":{{"type":"agent_message","message":"Starting the index."}}}}"#
    )?;
    writeln!(
        file,
        "{{\"timestamp\":\"2026-08-20T12:00:03Z\",\"type\":\"response_item\",\"payload\":{{\"type\":\"function_call_output\",\"output\":\"{}\"}}}}",
        "x".repeat(16_384)
    )?;

    let conversation = parse_rollout(&rollout, None)?.expect("conversation");
    assert_eq!(conversation.canonical_id(), "codex:session-test");
    assert_eq!(conversation.messages.len(), 3);
    assert_eq!(conversation.model.as_deref(), Some("gpt-test"));
    assert_eq!(conversation.raw["event_count"], 6);

    let service = IndexService::new(DataHome::new(tempdir.path().join("data")), None);
    service.import_normalized(vec![conversation], Some(&rollout))?;
    let stats = service.stats()?;
    assert_eq!(stats.conversations_by_source.get("codex"), Some(&1));

    let conn = chat_history_core::db::open_database(&service.managed_db_path())?;
    conn.execute(
        "UPDATE conversations SET summary_json = '{}', embedding_blob = X'00000000' WHERE conversation_id = ?1",
        params!["codex:session-test"],
    )?;
    conn.execute(
        "UPDATE jobs SET status = 'complete' WHERE conversation_id = ?1",
        params!["codex:session-test"],
    )?;
    drop(conn);

    writeln!(
        file,
        r#"{{"timestamp":"2026-08-20T12:00:04Z","type":"event_msg","payload":{{"type":"user_message","message":"Include another turn."}}}}"#
    )?;
    drop(file);
    let changed = parse_rollout(&rollout, None)?.expect("changed conversation");
    service.import_normalized(vec![changed], Some(&rollout))?;

    let conn = chat_history_core::db::open_database(&service.managed_db_path())?;
    let (summary_is_null, embedding_is_null): (i64, i64) = conn.query_row(
        "SELECT summary_json IS NULL, embedding_blob IS NULL FROM conversations WHERE conversation_id = ?1",
        params!["codex:session-test"],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    assert_eq!((summary_is_null, embedding_is_null), (1, 1));
    let pending: i64 = conn.query_row(
        "SELECT COUNT(*) FROM jobs WHERE conversation_id = ?1 AND status = 'pending'",
        params!["codex:session-test"],
        |row| row.get(0),
    )?;
    assert_eq!(pending, 2);
    Ok(())
}
