use std::{
    fs::{self, File},
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
};

use anyhow::Context;
use chrono::DateTime;
use regex::Regex;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::models::{NormalizedConversation, NormalizedMessage};

pub fn review_parent_conversation_id(text: &str) -> Option<String> {
    let pattern = Regex::new(
        r#"\.codex/visualizations/[^\s<>\"]*/([0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})"#,
    )
    .ok()?;
    let capture = pattern.captures(text)?;
    Some(format!("codex:{}", capture.get(1)?.as_str()))
}

pub fn discover_rollouts(roots: &[PathBuf]) -> anyhow::Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    for root in roots {
        visit(root, &mut paths)?;
    }
    paths.sort();
    paths.dedup();
    Ok(paths)
}

fn visit(path: &Path, paths: &mut Vec<PathBuf>) -> anyhow::Result<()> {
    if !path.exists() {
        return Ok(());
    }
    if path.is_file() {
        if path.extension().and_then(|value| value.to_str()) == Some("jsonl") {
            paths.push(path.to_path_buf());
        }
        return Ok(());
    }
    for entry in fs::read_dir(path).with_context(|| format!("reading {}", path.display()))? {
        visit(&entry?.path(), paths)?;
    }
    Ok(())
}

pub fn parse_rollout(
    path: &Path,
    since: Option<f64>,
) -> anyhow::Result<Option<NormalizedConversation>> {
    let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut event_count = 0usize;
    let mut event_messages = Vec::new();
    let mut fallback_messages = Vec::new();
    let mut session_id = None;
    let mut create_time = None;
    let mut update_time: Option<f64> = None;
    let mut model = None;

    for line in BufReader::new(file).lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        event_count += 1;
        let mut probe_end = line.len().min(1024);
        while !line.is_char_boundary(probe_end) {
            probe_end -= 1;
        }
        let probe = &line[..probe_end];
        let response_message = probe.contains(r#""type":"response_item""#)
            && probe.contains(r#""payload":{"type":"message""#);
        let relevant = probe.contains(r#""type":"session_meta""#)
            || probe.contains(r#""type":"turn_context""#)
            || probe.contains(r#""type":"event_msg""#)
            || response_message;
        if !relevant {
            continue;
        }
        let value: Value = serde_json::from_str(&line)
            .with_context(|| format!("parsing JSONL in {}", path.display()))?;
        let timestamp = value
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(parse_timestamp);
        if let Some(timestamp) = timestamp {
            create_time =
                Some(create_time.map_or(timestamp, |current: f64| current.min(timestamp)));
            update_time = Some(update_time.map_or(timestamp, |current| current.max(timestamp)));
        }
        match value.get("type").and_then(Value::as_str) {
            Some("session_meta") => {
                let payload = value.get("payload").unwrap_or(&Value::Null);
                session_id = payload
                    .get("id")
                    .or_else(|| payload.get("session_id"))
                    .and_then(Value::as_str)
                    .map(ToString::to_string);
                if create_time.is_none() {
                    create_time = payload
                        .get("timestamp")
                        .and_then(Value::as_str)
                        .and_then(parse_timestamp);
                }
            }
            Some("turn_context") => {
                if model.is_none() {
                    model = value
                        .get("payload")
                        .and_then(|payload| payload.get("model"))
                        .and_then(Value::as_str)
                        .map(ToString::to_string);
                }
            }
            Some("event_msg") => {
                let payload = value.get("payload").unwrap_or(&Value::Null);
                let role = match payload.get("type").and_then(Value::as_str) {
                    Some("user_message") => Some("user"),
                    Some("agent_message") => Some("assistant"),
                    _ => None,
                };
                if let (Some(role), Some(text)) =
                    (role, payload.get("message").and_then(Value::as_str))
                {
                    push_message(&mut event_messages, role, text, timestamp, payload.clone());
                }
            }
            Some("response_item") => {
                let payload = value.get("payload").unwrap_or(&Value::Null);
                if payload.get("type").and_then(Value::as_str) == Some("message")
                    && let Some(role @ ("user" | "assistant")) =
                        payload.get("role").and_then(Value::as_str)
                {
                    let text = response_content_text(payload.get("content"));
                    if !text.is_empty() {
                        push_message(
                            &mut fallback_messages,
                            role,
                            &text,
                            timestamp,
                            payload.clone(),
                        );
                    }
                }
            }
            _ => {}
        }
    }

    let Some(session_id) = session_id else {
        return Ok(None);
    };
    let effective_time = update_time.or(create_time).unwrap_or_default();
    if since.is_some_and(|cutoff| effective_time < cutoff) {
        return Ok(None);
    }
    let messages = if event_messages.is_empty() {
        fallback_messages
    } else {
        event_messages
    };
    if messages.is_empty() {
        return Ok(None);
    }
    let title = messages
        .iter()
        .find(|message| message.role == "user")
        .map(|message| concise_title(&message.text))
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "Codex session".to_string());
    Ok(Some(NormalizedConversation {
        source: "codex".to_string(),
        source_instance: None,
        source_conversation_id: session_id,
        title,
        create_time,
        update_time,
        model,
        source_url: None,
        source_path: Some(path.display().to_string()),
        messages,
        raw: json!({
            "format": "codex-rollout-jsonl",
            "event_count": event_count,
            "source_path": path.display().to_string(),
            "note": "Full rollout remains in source_path; the index stores normalized conversation messages, not redundant tool-event payloads."
        }),
    }))
}

fn push_message(
    messages: &mut Vec<NormalizedMessage>,
    role: &str,
    text: &str,
    create_time: Option<f64>,
    raw: Value,
) {
    let position = messages.len();
    let mut digest = Sha256::new();
    digest.update(role.as_bytes());
    digest.update(create_time.unwrap_or_default().to_le_bytes());
    digest.update(text.as_bytes());
    digest.update(position.to_le_bytes());
    messages.push(NormalizedMessage {
        message_id: format!("codex-{}", hex::encode(digest.finalize())),
        role: role.to_string(),
        create_time,
        text: text.to_string(),
        raw,
    });
}

fn response_content_text(content: Option<&Value>) -> String {
    let Some(items) = content.and_then(Value::as_array) else {
        return String::new();
    };
    items
        .iter()
        .filter_map(|item| item.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("\n")
}

fn parse_timestamp(value: &str) -> Option<f64> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|timestamp| timestamp.timestamp_millis() as f64 / 1000.0)
}

fn concise_title(value: &str) -> String {
    let single_line = value.split_whitespace().collect::<Vec<_>>().join(" ");
    single_line.chars().take(120).collect()
}
