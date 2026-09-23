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

fn write_json(path: &Path, value: Value) {
    fs::write(path, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
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

    let seeded = run_cli(&data_home, &["chatgpt-seed-from-index"]);
    assert_eq!(seeded["update_time"], json!(120.0));
    assert_eq!(seeded["state"]["last_successful_update_time"], json!(120.0));
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
