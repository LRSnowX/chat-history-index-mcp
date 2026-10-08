use std::{fs, io::Write, path::Path, process::Command};

use serde_json::{Value, json};
use tempfile::TempDir;
use zip::write::SimpleFileOptions;

fn run_cli(data_home: &Path, args: &[&str]) -> Value {
    let output = Command::new(env!("CARGO_BIN_EXE_chat-history-cli"))
        .args(args)
        .env("CHAT_HISTORY_DATA_HOME", data_home)
        .output()
        .expect("run chat-history-cli");
    assert!(
        output.status.success(),
        "CLI failed for {args:?}:\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("CLI JSON output")
}

#[test]
fn operator_continuation_repair_preserves_trusted_body_and_clears_block_only_after_success() {
    let temp = TempDir::new().unwrap();
    let home = temp.path().join("data");
    let input = temp.path().join("input.json");
    let old_body = "trusted historical body ".repeat(1500);
    write_json(
        &input,
        json!({"thread_id":"synthetic-repair","title":"Arcos synthetic","update_time":20.0,"pages":[{"has_more":false,"messages":[
            {"message_id":"old-assistant","role":"assistant","text":old_body},
            {"message_id":"old-user","role":"user","text":"Question"}
        ]}]}),
    );
    run_cli(
        &home,
        &[
            "chatgpt-import-thread",
            "--path",
            input.to_str().unwrap(),
            "--embed",
            "false",
        ],
    );
    let baseline = run_cli(
        &home,
        &["chatgpt-continuation-baseline", "synthetic-repair"],
    );
    let observation = json!({"thread_id":"synthetic-repair","kind":"chatgpt","title":"Arcos synthetic","update_time":30.0,"status":"idle","observed_at":40.0});
    write_json(
        &input,
        json!({"requested_limit":50,"threads":[observation.clone()]}),
    );
    run_cli(
        &home,
        &["chatgpt-plan-recent", "--path", input.to_str().unwrap()],
    );
    run_cli(
        &home,
        &[
            "chatgpt-block",
            "synthetic-repair",
            "--reason",
            "20000-character safety limit",
        ],
    );
    let mut repair = json!({"baseline":baseline,"provider_before":observation,"provider_after":observation,
    "transcript":{"thread_id":"synthetic-repair","title":"Arcos synthetic","update_time":30.0,"pages":[{"has_more":false,"provider_revision":30.0,"messages":[
        {"message_id":"new","role":"user","text":"Complete new tail","stable_identity":true,"truncated":true},
        {"message_id":"old-assistant","role":"assistant","text":"truncated old copy","stable_identity":true,"truncated":true},
        {"message_id":"old-user","role":"user","text":"Question","stable_identity":true}
    ]}]}});
    write_json(&input, repair.clone());
    let failed = Command::new(env!("CARGO_BIN_EXE_chat-history-cli"))
        .args([
            "chatgpt-repair-continuation",
            "--path",
            input.to_str().unwrap(),
            "--embed",
            "false",
        ])
        .env("CHAT_HISTORY_DATA_HOME", &home)
        .output()
        .unwrap();
    assert!(!failed.status.success());
    let state = run_cli(&home, &["chatgpt-state"]);
    assert!(state["blocked"]["synthetic-repair"].is_object());
    let service =
        chat_history_core::IndexService::new(chat_history_core::DataHome::new(home.clone()), None);
    assert_eq!(
        service
            .get_conversation("synthetic-repair", false)
            .unwrap()
            .unwrap()
            .messages
            .len(),
        2
    );
    repair["transcript"]["pages"][0]["messages"][0]["truncated"] = json!(false);
    write_json(&input, repair);
    let repaired = run_cli(
        &home,
        &[
            "chatgpt-repair-continuation",
            "--path",
            input.to_str().unwrap(),
            "--embed",
            "false",
        ],
    );
    assert!(repaired["state"]["blocked"].as_object().unwrap().is_empty());
    let after = service
        .get_conversation("synthetic-repair", true)
        .unwrap()
        .unwrap();
    assert_eq!(after.messages.len(), 3);
    assert_eq!(after.messages[1].normalized_text, old_body);
    assert_eq!(after.conversation.update_time, Some(30.0));
    let state = chat_history_core::ChatGptSyncState::load(service.data_home()).unwrap();
    assert_eq!(
        state
            .source_health("chatgpt", "synthetic-repair", Some(30.0))
            .state,
        chat_history_core::ConversationSourceHealthState::Aligned
    );
}

fn write_json(path: &Path, value: Value) {
    fs::write(path, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
}

#[test]
fn official_export_operator_then_live_repair_preserves_block_and_full_new_body() {
    let temp = TempDir::new().unwrap();
    let home = temp.path().join("data");
    let input = temp.path().join("input.json");
    write_json(
        &input,
        json!({"thread_id":"oversized-cli","title":"synthetic","update_time":20.0,"pages":[{"has_more":false,"messages":[
            {"message_id":"m1","role":"assistant","text":"answer"},{"message_id":"m0","role":"user","text":"Question"}
        ]}]}),
    );
    run_cli(
        &home,
        &[
            "chatgpt-import-thread",
            "--path",
            input.to_str().unwrap(),
            "--embed",
            "false",
        ],
    );
    let baseline_path = temp.path().join("baseline.json");
    write_json(
        &baseline_path,
        run_cli(&home, &["chatgpt-continuation-baseline", "oversized-cli"]),
    );
    let observation = json!({"thread_id":"oversized-cli","kind":"chatgpt","title":"synthetic","status":"idle","update_time":30.0,"observed_at":31.0});
    write_json(
        &input,
        json!({"requested_limit":50,"threads":[observation]}),
    );
    run_cli(
        &home,
        &["chatgpt-plan-recent", "--path", input.to_str().unwrap()],
    );
    run_cli(
        &home,
        &[
            "chatgpt-block",
            "oversized-cli",
            "--reason",
            "original oversized blocker",
        ],
    );
    let oversized = format!("  {}  ", "synthetic Unicode 完整正文🦀 ".repeat(2000));
    let bodies = ["Question", "answer", oversized.as_str()];
    let mut mapping = serde_json::Map::new();
    mapping.insert(
        "root".into(),
        json!({"id":"root","parent":null,"children":["n0"],"message":null}),
    );
    for (i, text) in bodies.iter().enumerate() {
        mapping.insert(format!("n{i}"),json!({"id":format!("n{i}"),"parent":if i==0 {"root".to_string()} else {format!("n{}",i-1)},"children":if i==2 {vec![]} else {vec![format!("n{}",i+1)]},"message":{"id":format!("m{i}"),"author":{"role":if i%2==0 {"user"} else {"assistant"}},"status":"finished_successfully","content":{"content_type":"text","parts":[text]}}}));
    }
    let archive_path = temp.path().join("official.zip");
    let mut inner = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    inner
        .start_file("conversations-000.json", SimpleFileOptions::default())
        .unwrap();
    inner.write_all(json!([{"id":"oversized-cli","conversation_id":"oversized-cli","title":"synthetic","update_time":30.0,"mapping":mapping,"current_node":"n2"}]).to_string().as_bytes()).unwrap();
    let bytes = inner.finish().unwrap().into_inner();
    let mut archive = zip::ZipWriter::new(fs::File::create(&archive_path).unwrap());
    archive
        .start_file(
            "User Online Activity/Conversations__fixture.zip",
            SimpleFileOptions::default(),
        )
        .unwrap();
    archive.write_all(&bytes).unwrap();
    archive.finish().unwrap();
    let before = run_cli(&home, &["chatgpt-state"]);
    let imported = run_cli(
        &home,
        &[
            "chatgpt-repair-export",
            "--archive",
            archive_path.to_str().unwrap(),
            "--baseline",
            baseline_path.to_str().unwrap(),
            "--embed",
            "false",
        ],
    );
    assert_eq!(imported["status"], "awaiting_live_verification");
    assert_eq!(run_cli(&home, &["chatgpt-state"]), before);
    write_json(
        &baseline_path,
        run_cli(&home, &["chatgpt-continuation-baseline", "oversized-cli"]),
    );
    let observation = json!({"thread_id":"oversized-cli","kind":"chatgpt","title":"synthetic","status":"idle","update_time":40.0,"observed_at":41.0});
    write_json(
        &input,
        json!({"requested_limit":50,"threads":[observation.clone()]}),
    );
    run_cli(
        &home,
        &["chatgpt-plan-recent", "--path", input.to_str().unwrap()],
    );
    let mut replay = json!({"baseline":run_cli(&home,&["chatgpt-continuation-baseline","oversized-cli"]),"provider_before":observation,"provider_after":observation,"transcript":{"thread_id":"oversized-cli","title":"synthetic","update_time":40.0,"pages":[{"has_more":false,"provider_revision":40.0,"messages":[
        {"message_id":"m3","role":"assistant","text":"PRIVATE_BODY_SENTINEL","truncated":true,"stable_identity":true},
        {"message_id":"m2","role":"user","text":"truncated provider representation","truncated":true,"stable_identity":true},
        {"message_id":"m1","role":"assistant","text":"answer","stable_identity":true},
        {"message_id":"m0","role":"user","text":"Question","stable_identity":true}
    ]}]}});
    write_json(&input, replay.clone());
    let failed = Command::new(env!("CARGO_BIN_EXE_chat-history-cli"))
        .args([
            "chatgpt-repair-continuation",
            "--path",
            input.to_str().unwrap(),
            "--embed",
            "false",
        ])
        .env("CHAT_HISTORY_DATA_HOME", &home)
        .output()
        .unwrap();
    assert!(!failed.status.success());
    let stderr = String::from_utf8_lossy(&failed.stderr);
    assert!(stderr.contains("CHIM_REPAIR_TRUNCATED_NEW_TAIL"));
    assert!(!stderr.contains("PRIVATE_BODY_SENTINEL"));
    let blocked = run_cli(&home, &["chatgpt-state"]);
    assert_eq!(
        blocked["blocked"]["oversized-cli"]["reason"],
        "original oversized blocker"
    );
    assert_eq!(
        blocked["blocked"]["oversized-cli"]["last_repair_failure"]["code"],
        "TRUNCATED_NEW_TAIL"
    );
    assert!(
        blocked["blocked"]["oversized-cli"]["last_repair_failure"]["reason"]
            .as_str()
            .unwrap()
            .len()
            <= 240
    );
    replay["transcript"]["pages"][0]["messages"][0]["truncated"] = json!(false);
    write_json(&input, replay);
    let repaired = run_cli(
        &home,
        &[
            "chatgpt-repair-continuation",
            "--path",
            input.to_str().unwrap(),
            "--embed",
            "false",
        ],
    );
    assert!(repaired["state"]["blocked"].as_object().unwrap().is_empty());
    let service =
        chat_history_core::IndexService::new(chat_history_core::DataHome::new(home), None);
    let after = service
        .get_conversation("oversized-cli", true)
        .unwrap()
        .unwrap();
    assert_eq!(after.messages.len(), 4);
    assert_eq!(after.messages[2].normalized_text, oversized);
    let state = chat_history_core::ChatGptSyncState::load(service.data_home()).unwrap();
    let health = state.source_health("chatgpt", "oversized-cli", Some(40.0));
    assert_eq!(
        chat_history_core::continuation::continuation_proof(&after, &health, 2, 2, false).state,
        "verified"
    );
}

fn build_fixture_export(temp: &TempDir) -> std::path::PathBuf {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../chat-history-core/tests/fixtures/conversations-000.json");
    let nested_zip = temp.path().join("nested-conversations.zip");
    {
        let file = fs::File::create(&nested_zip).unwrap();
        let mut writer = zip::ZipWriter::new(file);
        writer
            .start_file("conversations-000.json", SimpleFileOptions::default())
            .unwrap();
        writer.write_all(&fs::read(&fixture).unwrap()).unwrap();
        writer.finish().unwrap();
    }
    let outer_zip = temp.path().join("openai-export.zip");
    {
        let file = fs::File::create(&outer_zip).unwrap();
        let mut writer = zip::ZipWriter::new(file);
        writer
            .start_file(
                "User Online Activity/Conversations__fixture-part-0001.zip",
                SimpleFileOptions::default(),
            )
            .unwrap();
        writer.write_all(&fs::read(&nested_zip).unwrap()).unwrap();
        writer.finish().unwrap();
    }
    outer_zip
}

#[test]
fn chatgpt_cli_imports_complete_threads_and_advances_cursor_only_when_batch_is_clean() {
    let temp = TempDir::new().unwrap();
    let data_home = temp.path().join("data");
    let input_dir = temp.path().join("input");
    fs::create_dir_all(&input_dir).unwrap();

    let discovery = input_dir.join("discovery.json");
    write_json(
        &discovery,
        json!({
            "requested_limit": 50,
            "threads": [
                {"thread_id":"chat-a","kind":"chatgpt","title":"LEMonX migration decision","create_time":100.0,"update_time":120.0},
                {"thread_id":"codex-ignore","kind":"codex","title":"Codex task","create_time":100.0,"update_time":115.0},
                {"thread_id":"chat-b","kind":"chatgpt","title":"Arcos direction","create_time":100.0,"update_time":110.0}
            ]
        }),
    );
    let plan = run_cli(
        &data_home,
        &["chatgpt-plan-recent", "--path", discovery.to_str().unwrap()],
    );
    assert_eq!(plan["plan"]["selected"].as_array().unwrap().len(), 2);
    assert_eq!(plan["state"]["last_successful_update_time"], Value::Null);

    let first = input_dir.join("chat-a.json");
    write_json(
        &first,
        json!({
            "thread_id":"chat-a",
            "title":"LEMonX migration decision",
            "create_time":100.0,
            "update_time":120.0,
            "model":"gpt-test",
            "source_url":null,
            "pages":[
                {
                    "request_cursor":null,
                    "next_cursor":"older-a",
                    "has_more":true,
                    "messages":[
                        {"message_id":"a2","role":"assistant","create_time":120.0,"text":"Do not rewrite applied migrations; add a new migration.","raw":{}}
                    ]
                },
                {
                    "request_cursor":"older-a",
                    "next_cursor":null,
                    "has_more":false,
                    "messages":[
                        {"message_id":"u1","role":"user","create_time":100.0,"text":"为什么不能修改旧 migration？","raw":{}}
                    ]
                }
            ]
        }),
    );
    let first_import = run_cli(
        &data_home,
        &[
            "chatgpt-import-thread",
            "--path",
            first.to_str().unwrap(),
            "--embed",
            "false",
        ],
    );
    assert_eq!(
        first_import["state"]["last_successful_update_time"],
        Value::Null
    );
    assert_eq!(
        first_import["state"]["pending"].as_object().unwrap().len(),
        1
    );
    assert_eq!(
        first_import["state"]["completed_since_cursor"]["chat-a"],
        json!(120.0)
    );

    let second = input_dir.join("chat-b.json");
    write_json(
        &second,
        json!({
            "thread_id":"chat-b",
            "title":"Arcos direction",
            "create_time":100.0,
            "update_time":110.0,
            "model":"gpt-test",
            "source_url":null,
            "pages":[
                {
                    "request_cursor":null,
                    "next_cursor":null,
                    "has_more":false,
                    "messages":[
                        {"message_id":"a1","role":"assistant","create_time":110.0,"text":"Continue the current Arcos product direction.","raw":{}},
                        {"message_id":"u2","role":"user","create_time":100.0,"text":"Arcos 最新方向是什么？","raw":{}}
                    ]
                }
            ]
        }),
    );
    let second_import = run_cli(
        &data_home,
        &[
            "chatgpt-import-thread",
            "--path",
            second.to_str().unwrap(),
            "--embed",
            "false",
        ],
    );
    assert_eq!(
        second_import["state"]["last_successful_update_time"],
        json!(120.0)
    );
    assert!(
        second_import["state"]["pending"]
            .as_object()
            .unwrap()
            .is_empty()
    );
    assert!(
        second_import["state"]["completed_since_cursor"]
            .as_object()
            .unwrap()
            .is_empty()
    );

    let stats = run_cli(&data_home, &["stats"]);
    assert_eq!(stats["conversations_by_source"]["chatgpt"], json!(2));
    assert_eq!(stats["conversations"], json!(2));
    assert_eq!(stats["messages"], json!(4));

    let detail = run_cli(&data_home, &["show", "chat-a"]);
    let messages = detail["messages"].as_array().unwrap();
    assert_eq!(messages[0]["message_id"], "u1");
    assert_eq!(messages[1]["message_id"], "a2");

    // A newer but incomplete/truncated snapshot never replaces the good canonical.
    for invalid in [
        json!({"request_cursor":null,"next_cursor":"missing-page","has_more":true,"messages":[]}),
        json!({"request_cursor":null,"next_cursor":null,"has_more":false,"messages":[
            {"message_id":"a-new","role":"assistant","text":"partial newer content","truncated":true}
        ]}),
        json!({"request_cursor":"wrong-cursor","next_cursor":null,"has_more":false,"messages":[]}),
    ] {
        write_json(
            &first,
            json!({"thread_id":"chat-a","title":"LEMonX","update_time":130.0,"pages":[invalid]}),
        );
        let rejected = Command::new(env!("CARGO_BIN_EXE_chat-history-cli"))
            .args([
                "chatgpt-import-thread",
                "--path",
                first.to_str().unwrap(),
                "--embed",
                "false",
            ])
            .env("CHAT_HISTORY_DATA_HOME", &data_home)
            .output()
            .unwrap();
        assert!(!rejected.status.success());
        let retained = run_cli(&data_home, &["show", "chat-a"]);
        assert_eq!(retained["conversation"]["update_time"], json!(120.0));
        assert_eq!(retained["messages"], detail["messages"]);
    }

    let seeded = run_cli(&data_home, &["chatgpt-seed-from-index"]);
    assert_eq!(seeded["update_time"], json!(120.0));
    assert_eq!(seeded["state"]["last_successful_update_time"], json!(120.0));

    // Invalid latest observation is durable even when planning fails; old idle
    // observation cannot keep looking aligned after missing provider revision.
    write_json(
        &discovery,
        json!({"requested_limit":50,"threads":[
            {"thread_id":"chat-a","kind":"chatgpt","title":"LEMonX","update_time":null,"status":"idle"}
        ]}),
    );
    let invalid_plan = Command::new(env!("CARGO_BIN_EXE_chat-history-cli"))
        .args(["chatgpt-plan-recent", "--path", discovery.to_str().unwrap()])
        .env("CHAT_HISTORY_DATA_HOME", &data_home)
        .output()
        .unwrap();
    assert!(!invalid_plan.status.success());
    let observed = run_cli(&data_home, &["chatgpt-state"]);
    assert_eq!(
        observed["provider_observations"]["chat-a"]["provider_revision"],
        Value::Null
    );
    assert_eq!(observed["last_successful_update_time"], json!(120.0));
}

#[test]
fn chatgpt_bootstrap_export_backs_up_existing_db_imports_history_and_seeds_cursor() {
    let temp = TempDir::new().unwrap();
    let data_home = temp.path().join("data");
    let export = build_fixture_export(&temp);

    let empty_stats = run_cli(&data_home, &["stats"]);
    assert_eq!(empty_stats["conversations"], json!(0));

    let bootstrap = run_cli(
        &data_home,
        &[
            "chatgpt-bootstrap-export",
            "--archive",
            export.to_str().unwrap(),
            "--embed",
            "false",
        ],
    );
    assert_eq!(bootstrap["status"], "ok");
    assert_eq!(bootstrap["import"]["conversations_indexed"], json!(2));
    assert_eq!(bootstrap["chatgpt_source"]["conversations"], json!(2));
    assert_eq!(
        bootstrap["state"]["last_successful_update_time"],
        json!(1735948800.0)
    );
    let backup = bootstrap["backup_path"].as_str().expect("backup path");
    assert!(Path::new(backup).exists());
    assert!(
        export.exists(),
        "bootstrap must copy, not move, the user export"
    );

    let stats = run_cli(&data_home, &["stats"]);
    assert_eq!(stats["conversations_by_source"]["chatgpt"], json!(2));
    assert_eq!(stats["messages"], json!(5));
}
