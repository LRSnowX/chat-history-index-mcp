use chat_history_core::{
    DataHome, IndexService, NormalizedConversation, NormalizedMessage,
    db::{backup_database, inspect_database, restore_database},
};
use serde_json::json;

fn conversation(id: &str) -> NormalizedConversation {
    NormalizedConversation {
        source: "test-provider".to_string(),
        source_instance: None,
        source_conversation_id: id.to_string(),
        title: format!("Conversation {id}"),
        create_time: Some(1.0),
        update_time: Some(2.0),
        model: Some("test-model".to_string()),
        source_url: None,
        source_path: None,
        messages: vec![NormalizedMessage {
            message_id: format!("message-{id}"),
            role: "user".to_string(),
            create_time: Some(1.0),
            text: "hello".to_string(),
            raw: json!({}),
        }],
        raw: json!({}),
    }
}

#[test]
fn backup_and_restore_preserve_index_and_create_rollback_backup() {
    let temporary = tempfile::tempdir().unwrap();
    let data_home = DataHome::new(temporary.path().join("data"));
    let service = IndexService::with_env(data_home.clone());
    service
        .import_normalized(vec![conversation("one")], None)
        .unwrap();

    let paths = data_home.paths();
    let backup_path = temporary.path().join("migration.sqlite3");
    let backup_health = backup_database(&paths.db_path, &backup_path, false).unwrap();
    assert_eq!(backup_health.integrity_check, "ok");
    assert_eq!(backup_health.conversations, 1);

    service
        .import_normalized(vec![conversation("two")], None)
        .unwrap();
    assert_eq!(inspect_database(&paths.db_path).unwrap().conversations, 2);

    let report = restore_database(&backup_path, &paths.db_path).unwrap();
    assert_eq!(report.health.integrity_check, "ok");
    assert_eq!(report.health.conversations, 1);
    assert!(report.previous_database_backup.unwrap().is_file());
    let temporary_sidecars = std::fs::read_dir(&paths.db_dir)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().starts_with(".tmp"))
        .count();
    assert_eq!(temporary_sidecars, 0);
}
