use chat_history_core::{ChatGptBridgeTranscript, DataHome, IndexService};
use serde_json::{Value, json};
use std::{fs, path::Path, process::Command};

fn run(home: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_chat-history-cli"))
        .args(args)
        .env("CHAT_HISTORY_DATA_HOME", home)
        .output()
        .unwrap()
}

#[test]
fn readonly_plan_then_explicit_single_thread_apply_rechecks_identity_and_baseline() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("data");
    let service = IndexService::new(DataHome::new(home.clone()), None);
    let bridge: ChatGptBridgeTranscript = serde_json::from_value(json!({
        "thread_id":"synthetic-cli-history", "title":"⭐ synthetic LEMonX", "update_time":20.0,
        "pages":[{"has_more":false,"messages":[
            {"message_id":"d","role":"assistant","text":"PRIVATE_CLI_BODY"},
            {"message_id":"c","role":"user","text":"PRIVATE_CLI_BODY"}
        ]}]
    }))
    .unwrap();
    service
        .import_normalized(vec![bridge.into_normalized().unwrap()], None)
        .unwrap();
    let source = temp.path().join("export");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("export_manifest.json"), br#"{"version":1}"#).unwrap();
    let mut mapping = serde_json::Map::new();
    mapping.insert(
        "root".into(),
        json!({"id":"root","parent":null,"children":["a"],"message":null}),
    );
    let ids = ["a", "b", "c", "d"];
    for (i, id) in ids.iter().enumerate() {
        mapping.insert((*id).into(), json!({"id":id,"parent":if i==0{"root"}else{ids[i-1]},"children":if i==3{vec![]}else{vec![ids[i+1]]},"message":{"id":id,"author":{"role":if i%2==0{"user"}else{"assistant"}},"content":{"content_type":"text","parts":["PRIVATE_CLI_BODY"]}}}));
    }
    fs::write(source.join("conversations-000.json"), serde_json::to_vec(&json!([{"id":"synthetic-cli-history","title":"⭐ synthetic LEMonX","update_time":20.0,"current_node":"d","mapping":mapping}])).unwrap()).unwrap();
    let before = fs::read(service.managed_db_path()).unwrap();
    let planned = run(
        &home,
        &[
            "chatgpt-history-restore-plan",
            "--source",
            source.to_str().unwrap(),
            "--title-prefix",
            "⭐",
        ],
    );
    assert!(
        planned.status.success(),
        "{}",
        String::from_utf8_lossy(&planned.stderr)
    );
    assert!(!String::from_utf8_lossy(&planned.stdout).contains("PRIVATE_CLI_BODY"));
    assert_eq!(fs::read(service.managed_db_path()).unwrap(), before);
    let plan: Value = serde_json::from_slice(&planned.stdout).unwrap();
    assert_eq!(plan["entries"][0]["reason"], "SAFE_RESTORATION");
    let path = temp.path().join("plan.json");
    fs::write(&path, &planned.stdout).unwrap();
    let wrong = run(
        &home,
        &[
            "chatgpt-history-restore-apply",
            "--plan",
            path.to_str().unwrap(),
            "--conversation-id",
            "wrong-id",
        ],
    );
    assert!(!wrong.status.success());
    assert_eq!(fs::read(service.managed_db_path()).unwrap(), before);
    let args = [
        "chatgpt-history-restore-apply",
        "--plan",
        path.to_str().unwrap(),
        "--conversation-id",
        "synthetic-cli-history",
    ];
    let applied = run(&home, &args);
    assert!(
        applied.status.success(),
        "{}",
        String::from_utf8_lossy(&applied.stderr)
    );
    assert!(!String::from_utf8_lossy(&applied.stdout).contains("PRIVATE_CLI_BODY"));
    assert_eq!(
        serde_json::from_slice::<Value>(&applied.stdout).unwrap()["status"],
        "awaiting_live_verification"
    );
    assert_eq!(
        service
            .get_conversation("synthetic-cli-history", true)
            .unwrap()
            .unwrap()
            .messages
            .len(),
        4
    );
    let replay = run(&home, &args);
    assert!(!replay.status.success());
    assert!(
        String::from_utf8_lossy(&replay.stderr).contains("CHIM_HISTORY_RESTORE_BASELINE_CHANGED")
    );
}
