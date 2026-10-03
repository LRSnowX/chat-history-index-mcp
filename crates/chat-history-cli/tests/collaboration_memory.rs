use std::{path::Path, process::Command};

use serde_json::Value;
use tempfile::TempDir;

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

fn run_cli_failure(data_home: &Path, args: &[&str]) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_chat-history-cli"))
        .args(args)
        .env("CHAT_HISTORY_DATA_HOME", data_home)
        .output()
        .expect("run chat-history-cli");
    assert!(
        !output.status.success(),
        "CLI unexpectedly succeeded for {args:?}: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn collaboration_memory_cli_requires_explicit_operator_review_lifecycle() {
    let temp = TempDir::new().unwrap();
    let data_home = temp.path().join("data");

    let first = run_cli(
        &data_home,
        &[
            "memory-collaboration-author",
            "--kind",
            "preference",
            "--key",
            "upstream_compatibility",
            "--value",
            "Preserve upstream compatibility where practical.",
            "--reason",
            "The user established this as a stable cross-project rule.",
            "--evidence",
            "user_statement:conversation:user-policy",
        ],
    );
    let first_id = first["memory_id"]
        .as_str()
        .expect("first collaboration memory id")
        .to_string();
    assert_eq!(first["status"], "active");
    assert_eq!(first["scope"]["type"], "global");
    assert_eq!(
        first["evidence"][0]["detail"]["review_reason"],
        "The user established this as a stable cross-project rule."
    );

    let listed = run_cli(&data_home, &["memory-collaboration-list"]);
    assert_eq!(listed["items"].as_array().map(Vec::len), Some(1));
    assert_eq!(listed["items"][0]["memory_id"], first_id);

    let conflict = run_cli_failure(
        &data_home,
        &[
            "memory-collaboration-author",
            "--kind",
            "decision",
            "--key",
            "upstream_compatibility",
            "--value",
            "Ignore upstream compatibility.",
            "--reason",
            "Attempted replacement without explicit supersession.",
            "--evidence",
            "user_statement:conversation:conflict",
        ],
    );
    assert!(conflict.contains("explicitly supersede"));

    let replacement = run_cli(
        &data_home,
        &[
            "memory-collaboration-author",
            "--kind",
            "decision",
            "--key",
            "upstream_compatibility",
            "--value-json",
            "{\"text\":\"Preserve upstream compatibility unless the user explicitly changes policy.\"}",
            "--supersedes",
            &first_id,
            "--reason",
            "Operator reviewed a clarified stable decision.",
            "--evidence",
            "document:docs/policy.md",
        ],
    );
    let replacement_id = replacement["memory_id"]
        .as_str()
        .expect("replacement collaboration memory id")
        .to_string();
    assert_ne!(replacement_id, first_id);
    assert_eq!(replacement["supersedes_memory_id"], first_id);

    let listed = run_cli(&data_home, &["memory-collaboration-list"]);
    assert_eq!(listed["items"].as_array().map(Vec::len), Some(1));
    assert_eq!(listed["items"][0]["memory_id"], replacement_id);

    let retired = run_cli(
        &data_home,
        &[
            "memory-collaboration-retire",
            &replacement_id,
            "--reason",
            "The operator explicitly retired the global rule.",
            "--evidence",
            "user_statement:conversation:retirement",
        ],
    );
    assert_eq!(retired["status"], "archived");
    assert_eq!(retired["evidence"].as_array().map(Vec::len), Some(2));
    let listed = run_cli(&data_home, &["memory-collaboration-list"]);
    assert_eq!(listed["items"].as_array().map(Vec::len), Some(0));

    let missing_evidence = run_cli_failure(
        &data_home,
        &[
            "memory-collaboration-author",
            "--kind",
            "preference",
            "--key",
            "missing_evidence",
            "--value",
            "Should fail.",
            "--reason",
            "No evidence supplied.",
        ],
    );
    assert!(missing_evidence.contains("--evidence"));
}

#[test]
fn project_memory_confirmation_cli_requires_explicit_user_confirmation_lifecycle() {
    let temp = TempDir::new().unwrap();
    let data_home = temp.path().join("data");

    let first = run_cli(
        &data_home,
        &[
            "memory-project-confirm",
            "--project",
            "LEMonX",
            "--kind",
            "preference",
            "--key",
            "subagent_policy",
            "--value",
            "Codex and other subagents require explicit user approval.",
            "--reason",
            "The user explicitly reconfirmed the historical project rule.",
            "--user-confirmation",
            "current_chat:user_confirmed_subagent_policy",
        ],
    );
    let first_id = first["memory_id"]
        .as_str()
        .expect("first confirmed project memory id")
        .to_string();
    assert!(first_id.starts_with("project-confirmed-memory-v1:"));
    assert_eq!(first["status"], "active");
    assert_eq!(first["scope"]["type"], "project");
    assert_eq!(first["scope"]["project"], "LEMonX");
    assert_eq!(
        first["evidence"][0]["detail"]["source"],
        "operator_project_confirmation"
    );
    assert_eq!(
        first["evidence"][0]["detail"]["review_reason"],
        "The user explicitly reconfirmed the historical project rule."
    );

    let replay = run_cli(
        &data_home,
        &[
            "memory-project-confirm",
            "--project",
            "LEMonX",
            "--kind",
            "preference",
            "--key",
            "subagent_policy",
            "--value",
            "Codex and other subagents require explicit user approval.",
            "--reason",
            "The user explicitly reconfirmed the historical project rule.",
            "--user-confirmation",
            "current_chat:user_confirmed_subagent_policy",
        ],
    );
    assert_eq!(replay["memory_id"], first_id);
    assert_eq!(replay["created_at"], first["created_at"]);

    let conflict = run_cli_failure(
        &data_home,
        &[
            "memory-project-confirm",
            "--project",
            "LEMonX",
            "--kind",
            "decision",
            "--key",
            "subagent_policy",
            "--value",
            "Subagents may be started automatically.",
            "--reason",
            "Attempted replacement without explicit supersession.",
            "--user-confirmation",
            "current_chat:conflicting_confirmation",
        ],
    );
    assert!(conflict.contains("explicitly supersede"));

    let replacement = run_cli(
        &data_home,
        &[
            "memory-project-confirm",
            "--project",
            "LEMonX",
            "--kind",
            "decision",
            "--key",
            "subagent_policy",
            "--value-json",
            "{\"text\":\"Explicit approval remains required unless the user changes the rule.\"}",
            "--supersedes",
            &first_id,
            "--reason",
            "The user confirmed the clarified canonical project rule.",
            "--user-confirmation",
            "current_chat:user_confirmed_clarified_subagent_policy",
            "--evidence",
            "document:AGENTS.md",
        ],
    );
    let replacement_id = replacement["memory_id"]
        .as_str()
        .expect("replacement confirmed project memory id")
        .to_string();
    assert_ne!(replacement_id, first_id);
    assert_eq!(replacement["supersedes_memory_id"], first_id);
    assert_eq!(replacement["evidence"].as_array().map(Vec::len), Some(2));

    let retired = run_cli(
        &data_home,
        &[
            "memory-project-retire",
            "--project",
            "LEMonX",
            &replacement_id,
            "--reason",
            "The user explicitly retired the project-local rule.",
            "--user-confirmation",
            "current_chat:user_retired_subagent_policy",
        ],
    );
    assert_eq!(retired["status"], "archived");
    assert_eq!(retired["evidence"].as_array().map(Vec::len), Some(3));

    let missing_confirmation = run_cli_failure(
        &data_home,
        &[
            "memory-project-confirm",
            "--project",
            "LEMonX",
            "--kind",
            "preference",
            "--key",
            "missing_confirmation",
            "--value",
            "Should fail.",
            "--reason",
            "No current user confirmation supplied.",
        ],
    );
    assert!(missing_confirmation.contains("--user-confirmation"));

    let invalid_kind = run_cli_failure(
        &data_home,
        &[
            "memory-project-confirm",
            "--project",
            "LEMonX",
            "--kind",
            "state",
            "--key",
            "current_state",
            "--value",
            "Should fail.",
            "--reason",
            "Operational state is outside the historical rule confirmation gate.",
            "--user-confirmation",
            "current_chat:invalid_kind",
        ],
    );
    assert!(invalid_kind.contains("must be invariant, preference, or decision"));
}
