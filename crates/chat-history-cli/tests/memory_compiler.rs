use std::{fs, os::unix::fs::PermissionsExt, path::Path, process::Command};

use chat_history_core::{DataHome, IndexService, NormalizedConversation, NormalizedMessage};
use serde_json::{Value, json};
use tempfile::TempDir;

fn message(id: &str, role: &str, text: &str, time: f64) -> NormalizedMessage {
    NormalizedMessage {
        message_id: id.to_string(),
        role: role.to_string(),
        create_time: Some(time),
        text: text.to_string(),
        raw: json!({"id": id}),
    }
}

fn run_cli(data_home: &Path, args: &[&str], extra_env: &[(&str, &str)]) -> Value {
    let mut command = Command::new(env!("CARGO_BIN_EXE_chat-history-cli"));
    command.args(args).env("CHAT_HISTORY_DATA_HOME", data_home);
    for (key, value) in extra_env {
        command.env(key, value);
    }
    let output = command.output().expect("run chat-history-cli");
    assert!(
        output.status.success(),
        "CLI failed for {args:?}:\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("CLI JSON output")
}

#[test]
fn memory_compiler_cli_stages_only_and_invokes_codex_read_only_medium() {
    let temp = TempDir::new().unwrap();
    let data_home = temp.path().join("data");
    let service = IndexService::new(DataHome::new(data_home.clone()), None);
    service
        .import_normalized(
            vec![NormalizedConversation {
                source: "chatgpt".to_string(),
                source_instance: None,
                source_conversation_id: "compiler-cli-chat".to_string(),
                title: "LEMonX compiler CLI".to_string(),
                create_time: Some(1.0),
                update_time: Some(2.0),
                model: None,
                source_url: None,
                source_path: Some("test".to_string()),
                messages: vec![
                    message("u1", "user", "Checkpoint A is now active.", 1.0),
                    message(
                        "a1",
                        "assistant",
                        "Checkpoint A will be the current state.",
                        2.0,
                    ),
                ],
                raw: json!({"source": "test"}),
            }],
            None,
        )
        .unwrap();

    let fake_codex = temp.path().join("fake-codex");
    let args_log = temp.path().join("codex-args.txt");
    let prompt_log = temp.path().join("codex-prompt.txt");
    fs::write(
        &fake_codex,
        r#"#!/bin/sh
set -eu
printf '%s\n' "$@" > "$FAKE_CODEX_ARGS"
cat > "$FAKE_CODEX_PROMPT"
out=''
previous=''
for arg in "$@"; do
  if [ "$previous" = '--output-last-message' ]; then
    out="$arg"
  fi
  previous="$arg"
done
test -n "$out"
printf '%s' "$FAKE_CODEX_RESPONSE" > "$out"
"#,
    )
    .unwrap();
    let mut permissions = fs::metadata(&fake_codex).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&fake_codex, permissions).unwrap();

    let response = json!({
        "proposals": [{
            "operation": "add",
            "kind": "state",
            "key": "checkpoint",
            "value": {"name": "A"},
            "importance": 90,
            "confidence": 0.95,
            "rationale": "The supplied dialogue explicitly establishes checkpoint A.",
            "evidence_message_ids": ["a1"]
        }]
    })
    .to_string();
    let result = run_cli(
        &data_home,
        &[
            "memory-compile-conversation",
            "--project",
            "LEMonX",
            "compiler-cli-chat",
            "--max-messages",
            "8",
        ],
        &[
            ("CODEX_BIN", fake_codex.to_str().unwrap()),
            ("CHAT_HISTORY_MEMORY_MODEL", "fake-memory-model"),
            ("FAKE_CODEX_ARGS", args_log.to_str().unwrap()),
            ("FAKE_CODEX_PROMPT", prompt_log.to_str().unwrap()),
            ("FAKE_CODEX_RESPONSE", &response),
        ],
    );
    assert_eq!(result["status"], "staged");
    assert_eq!(
        result["result"]["model_label"],
        "fake-memory-model via codex exec"
    );
    assert_eq!(
        result["result"]["staged"]["candidate_ids"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    let args = fs::read_to_string(&args_log).unwrap();
    assert!(args.lines().any(|arg| arg == "--ephemeral"));
    assert!(args.lines().any(|arg| arg == "--sandbox"));
    assert!(args.lines().any(|arg| arg == "read-only"));
    let args_lines = args.lines().collect::<Vec<_>>();
    let model_index = args_lines.iter().position(|arg| *arg == "--model").unwrap();
    assert_eq!(args_lines[model_index + 1], "fake-memory-model");
    assert!(
        args.lines()
            .any(|arg| arg == "model_reasoning_effort=\"medium\"")
    );

    let prompt = fs::read_to_string(&prompt_log).unwrap();
    assert!(prompt.contains("UNTRUSTED DATA"));
    assert!(prompt.contains("working_memory values"));
    assert!(prompt.contains("\"message_id\": \"a1\""));
    assert!(prompt.contains("Never invent candidate IDs"));

    let pending = service.pending_memory_candidates("LEMonX").unwrap();
    assert_eq!(pending.len(), 1);
    assert!(
        service
            .project_working_memory("LEMonX")
            .unwrap()
            .items
            .is_empty()
    );

    let candidates = run_cli(
        &data_home,
        &["memory-candidates", "--project", "LEMonX"],
        &[],
    );
    assert_eq!(candidates["pending"].as_array().unwrap().len(), 1);
}
