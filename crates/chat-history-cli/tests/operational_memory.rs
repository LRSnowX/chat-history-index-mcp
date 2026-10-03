use std::{path::Path, process::Command};

use chat_history_core::{DataHome, IndexService, MemoryStatus};
use serde_json::Value;
use tempfile::TempDir;

fn cli(home: &Path, args: &[&str], succeeds: bool) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_chat-history-cli"))
        .args(args)
        .env("CHAT_HISTORY_DATA_HOME", home)
        .output()
        .expect("run operator CLI");
    assert_eq!(
        output.status.success(),
        succeeds,
        "args={args:?}, stdout={}, stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(if succeeds {
        output.stdout
    } else {
        output.stderr
    })
    .unwrap()
}

fn set_args<'a>(kind: &'a str, value: &'a str) -> Vec<&'a str> {
    vec![
        "memory-project-operational-set",
        "--project",
        "LEMonX",
        "--kind",
        kind,
        "--key",
        "current_goal",
        "--value",
        value,
        "--reason",
        "Checkpoint accepted",
        "--evidence",
        "document:docs/acceptance.md",
    ]
}

fn transition_args<'a>(command: &'a str, id: &'a str, project: &'a str) -> Vec<&'a str> {
    vec![
        command,
        "--project",
        project,
        id,
        "--reason",
        "Operator reviewed completion",
        "--evidence",
        "git_commit:git:complete",
    ]
}

#[test]
fn operational_cli_set_supersede_resolve_archive_are_auditable_and_idempotent() {
    let temp = TempDir::new().unwrap();
    let home = temp.path().join("data");
    let service = IndexService::new(DataHome::new(home.clone()), None);
    let first_args = set_args("state", "Current checkpoint");
    let first: Value = serde_json::from_str(&cli(&home, &first_args, true)).unwrap();
    let id = first["memory_id"].as_str().unwrap();
    assert!(id.starts_with("project-operational-memory-v1:"));
    assert_eq!(first["scope"]["project"], "LEMonX");
    assert_eq!(first["status"], "active");
    assert_eq!(first["evidence"][0]["detail"]["action"], "set");
    assert_eq!(
        first["evidence"][0]["detail"]["review_reason"],
        "Checkpoint accepted"
    );
    let replay: Value = serde_json::from_str(&cli(&home, &first_args, true)).unwrap();
    assert_eq!(replay, first);
    let mut replacement_args = set_args("task", "Next checkpoint");
    assert!(cli(&home, &replacement_args, false).contains("explicitly supersede"));
    replacement_args.extend(["--supersedes", id]);
    let replacement: Value = serde_json::from_str(&cli(&home, &replacement_args, true)).unwrap();
    let new_id = replacement["memory_id"].as_str().unwrap();
    assert_ne!(id, new_id);
    assert_eq!(replacement["supersedes_memory_id"], id);
    assert_eq!(replacement["evidence"][0]["detail"]["action"], "supersede");
    assert_eq!(
        service.get_memory_item(id).unwrap().unwrap().status,
        MemoryStatus::Superseded
    );
    assert_eq!(
        service.project_working_memory("LEMonX").unwrap().items[0].memory_id,
        new_id
    );
    let replay: Value = serde_json::from_str(&cli(&home, &replacement_args, true)).unwrap();
    assert_eq!(replay, replacement);
    for command in [
        "memory-project-operational-resolve",
        "memory-project-operational-archive",
    ] {
        assert!(
            cli(&home, &transition_args(command, id, "LEMonX"), false)
                .contains("invalid operational")
        );
        assert!(
            cli(&home, &transition_args(command, new_id, "Arcos"), false)
                .contains("requested project")
        );
    }
    let resolve = transition_args("memory-project-operational-resolve", new_id, "LEMonX");
    let resolved: Value = serde_json::from_str(&cli(&home, &resolve, true)).unwrap();
    assert_eq!(resolved["status"], "resolved");
    assert_eq!(resolved["evidence"].as_array().unwrap().len(), 2);
    assert_eq!(resolved["evidence"][0], replacement["evidence"][0]);
    assert_eq!(resolved["evidence"][1]["detail"]["action"], "resolve");
    assert!(
        service
            .project_working_memory("LEMonX")
            .unwrap()
            .items
            .is_empty()
    );
    assert_eq!(
        serde_json::from_str::<Value>(&cli(&home, &resolve, true)).unwrap(),
        resolved
    );
    let mut archive = transition_args("memory-project-operational-archive", new_id, "LEMonX");
    *archive.last_mut().unwrap() = "git_commit:git:archive-review";
    let archived: Value = serde_json::from_str(&cli(&home, &archive, true)).unwrap();
    assert_eq!(archived["status"], "archived");
    assert_eq!(archived["evidence"].as_array().unwrap().len(), 3);
    assert_eq!(archived["evidence"][2]["detail"]["action"], "archive");
    assert_eq!(
        serde_json::from_str::<Value>(&cli(&home, &archive, true)).unwrap(),
        archived
    );
    assert!(cli(&home, &resolve, false).contains("invalid operational"));
    assert_eq!(
        serde_json::from_str::<Value>(&cli(&home, &replacement_args, true)).unwrap(),
        archived
    );
}

#[test]
fn operational_cli_requires_reason_evidence_operational_kind_and_exactly_one_value() {
    let temp = TempDir::new().unwrap();
    let home = temp.path().join("data");
    for kind in [
        "invariant",
        "preference",
        "decision",
        "result",
        "hypothesis",
        "artifact_reference",
    ] {
        assert!(cli(&home, &set_args(kind, "bad"), false).contains("invalid value"));
    }
    for command in [
        "memory-project-operational-set",
        "memory-project-operational-resolve",
        "memory-project-operational-archive",
    ] {
        let args = if command.ends_with("set") {
            set_args("state", "value")
        } else {
            transition_args(command, "nonexistent", "LEMonX")
        };
        for flag in ["--reason", "--evidence"] {
            let mut missing = args.clone();
            let index = missing.iter().position(|arg| *arg == flag).unwrap();
            missing.drain(index..index + 2);
            assert!(cli(&home, &missing, false).contains(flag));
            let mut empty = args.clone();
            empty[index + 1] = if flag == "--reason" {
                " "
            } else {
                "document: "
            };
            assert!(cli(&home, &empty, false).contains(if flag == "--reason" {
                "reason cannot be empty"
            } else {
                "reference cannot be empty"
            }));
        }
    }
    let mut both = set_args("state", "value");
    both.extend(["--value-json", "{}"]);
    assert!(cli(&home, &both, false).contains("cannot be used"));
    let mut neither = set_args("state", "value");
    neither.drain(7..9);
    assert!(cli(&home, &neither, false).contains("required"));
    for (flag, value) in [("--importance", "101"), ("--confidence", "1.1")] {
        let mut bad = set_args("state", "value");
        bad.extend([flag, value]);
        assert!(cli(&home, &bad, false).contains(if flag == "--importance" {
            "importance"
        } else {
            "confidence"
        }));
    }
    for kind in ["state", "blocker", "task"] {
        let mut args = set_args(kind, "value");
        args[6] = kind;
        args[7] = "--value-json";
        args[8] = "{\"step\":1}";
        let item: Value = serde_json::from_str(&cli(&home, &args, true)).unwrap();
        assert_eq!(item["kind"], kind);
        assert_eq!(item["value"]["step"], 1);
        let id = item["memory_id"].as_str().unwrap();
        let archived: Value = serde_json::from_str(&cli(
            &home,
            &transition_args("memory-project-operational-archive", id, "LEMonX"),
            true,
        ))
        .unwrap();
        assert_eq!(archived["status"], "archived");
    }
}
