use std::{fs, os::unix::fs::PermissionsExt, path::Path, process::Command};

use chat_history_core::{
    DataHome, IndexService, MemoryEvidenceKind, NormalizedConversation, NormalizedMessage,
};
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
        "proposals": [
            {
                "operation": "add",
                "kind": "state",
                "key": "checkpoint",
                "value": {"name": "A"},
                "importance": 90,
                "confidence": 0.95,
                "rationale": "The supplied dialogue explicitly establishes checkpoint A.",
                "evidence_message_ids": ["a1"]
            },
            {
                "operation": "add",
                "kind": "decision",
                "key": "compiler_review_mode",
                "value": {"mode": "operator_review"},
                "importance": 80,
                "confidence": 0.9,
                "rationale": "The compiler run is intentionally staged for operator review.",
                "evidence_message_ids": ["u1"]
            }
        ]
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
    let candidate_ids = result["result"]["staged"]["candidate_ids"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_str().unwrap().to_string())
        .collect::<Vec<_>>();
    assert_eq!(candidate_ids.len(), 2);

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
    assert!(prompt.contains("transient tool, plugin, connector, network"));
    assert!(prompt.contains("never label changing counts"));
    assert!(prompt.contains("do not also store verbose intermediate test matrices"));

    let pending = service.pending_memory_candidates("LEMonX").unwrap();
    assert_eq!(pending.len(), 2);
    let decision_candidate = service
        .memory_candidate(candidate_ids[1].as_str())
        .unwrap()
        .expect("decision candidate exists");
    assert_eq!(
        decision_candidate.evidence[0].kind,
        MemoryEvidenceKind::UserStatement
    );
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
    assert_eq!(candidates["pending"].as_array().unwrap().len(), 2);

    let inspected = run_cli(
        &data_home,
        &["memory-candidate", candidate_ids[0].as_str()],
        &[],
    );
    assert_eq!(inspected["status"], "pending");
    assert_eq!(inspected["candidate_id"], candidate_ids[0]);

    let missing_reason = Command::new(env!("CARGO_BIN_EXE_chat-history-cli"))
        .args(["memory-candidate-promote", candidate_ids[0].as_str()])
        .env("CHAT_HISTORY_DATA_HOME", &data_home)
        .output()
        .expect("run promotion without review reason");
    assert!(!missing_reason.status.success());
    assert!(String::from_utf8_lossy(&missing_reason.stderr).contains("--reason <REASON>"));

    let promoted = run_cli(
        &data_home,
        &[
            "memory-candidate-promote",
            candidate_ids[0].as_str(),
            "--reason",
            "verified against repository evidence",
            "--evidence",
            "git_commit:abc123",
            "--evidence",
            "document:docs/checkpoint-a.md",
        ],
        &[],
    );
    assert_eq!(promoted["decision"]["status"], "promoted");
    assert_eq!(promoted["candidate"]["status"], "promoted");
    assert_eq!(
        promoted["candidate"]["decision_reason"],
        "verified against repository evidence"
    );
    let promoted_memory_id = promoted["decision"]["memory_id"].as_str().unwrap();
    let promoted_item = service
        .get_memory_item(promoted_memory_id)
        .unwrap()
        .expect("promoted memory exists");
    assert_eq!(promoted_item.evidence.len(), 3);
    assert!(
        promoted_item.evidence.iter().any(
            |evidence| evidence.kind.as_str() == "git_commit" && evidence.reference == "abc123"
        )
    );
    assert!(
        promoted_item
            .evidence
            .iter()
            .any(|evidence| evidence.kind.as_str() == "document"
                && evidence.reference == "docs/checkpoint-a.md")
    );
    assert_eq!(
        service
            .project_working_memory("LEMonX")
            .unwrap()
            .items
            .len(),
        1
    );

    let reverified = run_cli(
        &data_home,
        &[
            "memory-candidate-promote",
            candidate_ids[0].as_str(),
            "--reason",
            "reverified against current repository state",
            "--evidence",
            "repository_state:LEMonX@abc123:clean",
        ],
        &[],
    );
    assert_eq!(reverified["decision"]["status"], "promoted");
    assert_eq!(
        reverified["candidate"]["decision_reason"],
        "reverified against current repository state"
    );
    let reverified_item = service
        .get_memory_item(promoted_memory_id)
        .unwrap()
        .expect("reverified memory exists");
    assert_eq!(reverified_item.evidence.len(), 4);
    assert!(
        reverified_item
            .evidence
            .iter()
            .any(|evidence| evidence.kind.as_str() == "repository_state"
                && evidence.reference == "LEMonX@abc123:clean")
    );
    let reviews = service
        .memory_candidate_reviews(candidate_ids[0].as_str())
        .unwrap();
    assert_eq!(reviews.len(), 2);
    assert_eq!(reviews[0].outcome, "promoted");
    assert_eq!(reviews[0].reason, "verified against repository evidence");
    assert_eq!(reviews[1].outcome, "reverified");
    assert_eq!(
        reviews[1].reason,
        "reverified against current repository state"
    );
    assert_eq!(reviews[0].evidence.len(), 2);
    assert_eq!(reviews[1].evidence.len(), 1);
    let reviewed = run_cli(
        &data_home,
        &["memory-candidate-reviews", candidate_ids[0].as_str()],
        &[],
    );
    assert_eq!(reviewed["candidate_id"], candidate_ids[0]);
    assert_eq!(reviewed["reviews"].as_array().unwrap().len(), 2);
    assert_eq!(reviewed["reviews"][0]["outcome"], "promoted");
    assert_eq!(reviewed["reviews"][1]["outcome"], "reverified");

    let rejected = run_cli(
        &data_home,
        &[
            "memory-candidate-reject",
            candidate_ids[1].as_str(),
            "--reason",
            "operator rejected test candidate",
        ],
        &[],
    );
    assert_eq!(rejected["status"], "rejected");
    assert_eq!(
        rejected["decision_reason"],
        "operator rejected test candidate"
    );
    assert!(
        service
            .pending_memory_candidates("LEMonX")
            .unwrap()
            .is_empty()
    );
}

#[test]
fn manual_memory_compiler_cli_exports_and_stages_without_invoking_codex() {
    let temp = TempDir::new().unwrap();
    let data_home = temp.path().join("data");
    let service = IndexService::new(DataHome::new(data_home.clone()), None);
    service
        .import_normalized(
            vec![NormalizedConversation {
                source: "chatgpt".to_string(),
                source_instance: None,
                source_conversation_id: "manual-compiler-cli-chat".to_string(),
                title: "LEMonX manual compiler CLI".to_string(),
                create_time: Some(1.0),
                update_time: Some(2.0),
                model: None,
                source_url: None,
                source_path: Some("test".to_string()),
                messages: vec![
                    message("manual-u1", "user", "Checkpoint B is now accepted.", 1.0),
                    message(
                        "manual-a1",
                        "assistant",
                        "Checkpoint B is the accepted baseline.",
                        2.0,
                    ),
                ],
                raw: json!({"source": "test"}),
            }],
            None,
        )
        .unwrap();

    let bundle = temp.path().join("manual.bundle.json");
    let prompt = temp.path().join("manual.prompt.txt");
    let impossible_codex = temp.path().join("codex-must-not-run");
    let exported = run_cli(
        &data_home,
        &[
            "memory-compile-manual-export",
            "--project",
            "LEMonX",
            "manual-compiler-cli-chat",
            "--max-messages",
            "8",
            "--bundle-out",
            bundle.to_str().unwrap(),
            "--prompt-out",
            prompt.to_str().unwrap(),
        ],
        &[("CODEX_BIN", impossible_codex.to_str().unwrap())],
    );
    assert_eq!(exported["status"], "exported");
    assert_eq!(exported["project"], "LEMonX");
    assert!(bundle.exists());
    assert!(prompt.exists());
    let prompt_text = fs::read_to_string(&prompt).unwrap();
    assert!(prompt_text.contains("UNTRUSTED DATA"));
    assert!(prompt_text.contains("manual-a1"));
    assert!(!impossible_codex.exists());

    let response = temp.path().join("manual.response.json");
    fs::write(
        &response,
        json!({
            "proposals": [{
                "operation": "add",
                "kind": "result",
                "key": "checkpoint_b_baseline",
                "value": {"status": "accepted"},
                "importance": 90,
                "confidence": 0.95,
                "rationale": "The supplied dialogue establishes checkpoint B.",
                "evidence_message_ids": ["manual-a1"]
            }]
        })
        .to_string(),
    )
    .unwrap();
    let staged = run_cli(
        &data_home,
        &[
            "memory-compile-manual-stage",
            "--bundle",
            bundle.to_str().unwrap(),
            "--response",
            response.to_str().unwrap(),
            "--model-label",
            "manual-test-model + medium",
        ],
        &[("CODEX_BIN", impossible_codex.to_str().unwrap())],
    );
    assert_eq!(staged["status"], "staged");
    assert_eq!(staged["model_label"], "manual-test-model + medium");
    assert_eq!(
        staged["staged"]["candidate_ids"].as_array().map(Vec::len),
        Some(1)
    );
    assert_eq!(
        service.pending_memory_candidates("LEMonX").unwrap().len(),
        1
    );
    assert!(!impossible_codex.exists());

    let repeat = Command::new(env!("CARGO_BIN_EXE_chat-history-cli"))
        .args([
            "memory-compile-manual-stage",
            "--bundle",
            bundle.to_str().unwrap(),
            "--response",
            response.to_str().unwrap(),
            "--model-label",
            "manual-test-model + medium",
        ])
        .env("CHAT_HISTORY_DATA_HOME", &data_home)
        .env("CODEX_BIN", &impossible_codex)
        .output()
        .expect("repeat manual stage");
    assert!(!repeat.status.success());
    assert!(String::from_utf8_lossy(&repeat.stderr).contains("stale or already caught up"));
}
