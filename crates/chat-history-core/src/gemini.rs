use std::{
    fs::{self, File},
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
};

use anyhow::{Context, anyhow};
use chrono::DateTime;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::models::{NormalizedConversation, NormalizedMessage};

/// Discover only Gemini CLI session recordings. Configuration, credentials, and shell history are
/// intentionally out of scope.
pub fn discover_sessions(roots: &[PathBuf]) -> anyhow::Result<Vec<PathBuf>> {
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
        let name = path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or_default();
        if name.starts_with("session-") && (name.ends_with(".json") || name.ends_with(".jsonl")) {
            paths.push(path.to_path_buf());
        }
        return Ok(());
    }
    for entry in fs::read_dir(path).with_context(|| format!("reading {}", path.display()))? {
        visit(&entry?.path(), paths)?;
    }
    Ok(())
}

pub fn parse_session(
    path: &Path,
    since: Option<f64>,
) -> anyhow::Result<Option<NormalizedConversation>> {
    let value = read_session_value(path)?;
    let metadata = value
        .as_object()
        .ok_or_else(|| anyhow!("Gemini session root must be an object"))?;
    let session_id = metadata
        .get("sessionId")
        .or_else(|| metadata.get("session_id"))
        .and_then(Value::as_str)
        .map(ToString::to_string)
        .unwrap_or_else(|| stable_path_id(path));
    let create_time = timestamp(
        metadata
            .get("startTime")
            .or_else(|| metadata.get("start_time")),
    );
    let update_time = timestamp(
        metadata
            .get("lastUpdated")
            .or_else(|| metadata.get("last_updated")),
    );
    let effective = update_time.or(create_time).unwrap_or_default();
    if since.is_some_and(|cutoff| effective < cutoff) {
        return Ok(None);
    }
    let mut messages = Vec::new();
    let mut model = metadata
        .get("model")
        .and_then(Value::as_str)
        .map(ToString::to_string);
    let items = metadata
        .get("messages")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("Gemini session is missing its messages array"))?;
    for (position, item) in items.iter().enumerate() {
        let Some(kind) = item.get("type").and_then(Value::as_str) else {
            continue;
        };
        let role = match kind {
            "user" => "user",
            "gemini" | "model" => "assistant",
            _ => continue,
        };
        let text = content_text(item.get("content").or_else(|| item.get("parts")));
        if text.is_empty() {
            continue;
        }
        if model.is_none() {
            model = item
                .get("model")
                .and_then(Value::as_str)
                .map(ToString::to_string);
        }
        let create_time = timestamp(item.get("timestamp").or_else(|| item.get("createdAt")));
        let message_id = item
            .get("id")
            .and_then(Value::as_str)
            .map(ToString::to_string)
            .unwrap_or_else(|| stable_message_id(&session_id, role, create_time, &text, position));
        messages.push(NormalizedMessage {
            message_id,
            role: role.to_string(),
            create_time,
            text,
            raw: item.clone(),
        });
    }
    if messages.is_empty() {
        return Ok(None);
    }
    let title = messages
        .iter()
        .find(|message| message.role == "user")
        .map(|message| concise_title(&message.text))
        .filter(|title| !title.is_empty())
        .unwrap_or_else(|| "Gemini CLI session".to_string());
    Ok(Some(NormalizedConversation {
        source: "gemini".to_string(),
        source_instance: Some("gemini-cli".to_string()),
        source_conversation_id: session_id,
        title,
        create_time,
        update_time,
        model,
        source_url: None,
        source_path: Some(path.display().to_string()),
        messages,
        raw: json!({"format":"gemini-cli-session", "source_path":path.display().to_string()}),
    }))
}

fn read_session_value(path: &Path) -> anyhow::Result<Value> {
    let text = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    if let Ok(value) = serde_json::from_str(&text) {
        return Ok(value);
    }
    let mut lines = Vec::new();
    for line in BufReader::new(File::open(path)?).lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        lines.push(serde_json::from_str::<Value>(&line)?);
    }
    let Some(metadata) = lines.first().cloned() else {
        return Err(anyhow!("Gemini JSONL session is empty"));
    };
    let messages = if metadata.get("messages").is_some() {
        return Ok(metadata);
    } else {
        lines.split_off(1)
    };
    let mut session = metadata;
    session
        .as_object_mut()
        .ok_or_else(|| anyhow!("Gemini JSONL metadata must be an object"))?
        .insert("messages".to_string(), Value::Array(messages));
    Ok(session)
}

fn content_text(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|item| item.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        Some(Value::Object(item)) => item
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        _ => String::new(),
    }
}

fn timestamp(value: Option<&Value>) -> Option<f64> {
    match value {
        Some(Value::Number(number)) => number.as_f64().map(|value| {
            if value > 10_000_000_000.0 {
                value / 1000.0
            } else {
                value
            }
        }),
        Some(Value::String(value)) => DateTime::parse_from_rfc3339(value)
            .ok()
            .map(|time| time.timestamp_millis() as f64 / 1000.0),
        _ => None,
    }
}

fn stable_path_id(path: &Path) -> String {
    let mut digest = Sha256::new();
    digest.update(path.display().to_string().as_bytes());
    format!("path-{}", hex::encode(digest.finalize()))
}

fn stable_message_id(
    session: &str,
    role: &str,
    create_time: Option<f64>,
    text: &str,
    position: usize,
) -> String {
    let mut digest = Sha256::new();
    digest.update(session.as_bytes());
    digest.update(role.as_bytes());
    digest.update(create_time.unwrap_or_default().to_le_bytes());
    digest.update(text.as_bytes());
    digest.update(position.to_le_bytes());
    format!("gemini-{}", hex::encode(digest.finalize()))
}

fn concise_title(value: &str) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(120)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::parse_session;
    use std::{fs, io::Write};
    use tempfile::TempDir;

    #[test]
    fn parses_json_and_jsonl_sessions() -> anyhow::Result<()> {
        let temp = TempDir::new()?;
        let json = temp.path().join("session-a.json");
        fs::write(
            &json,
            r#"{"sessionId":"gemini-a","startTime":"2026-08-21T00:00:00Z","lastUpdated":"2026-08-21T00:01:00Z","model":"gemini-test","messages":[{"id":"u1","type":"user","timestamp":"2026-08-21T00:00:00Z","content":[{"text":"Plan a collector"}]},{"id":"a1","type":"gemini","timestamp":"2026-08-21T00:00:01Z","content":"Use stable ids."}]}"#,
        )?;
        let parsed = parse_session(&json, None)?.expect("session");
        assert_eq!(parsed.canonical_id(), "gemini:gemini-cli:gemini-a");
        assert_eq!(parsed.messages.len(), 2);
        assert_eq!(parsed.model.as_deref(), Some("gemini-test"));
        let jsonl = temp.path().join("session-b.jsonl");
        let mut output = fs::File::create(&jsonl)?;
        writeln!(
            output,
            "{{\"sessionId\":\"gemini-b\",\"startTime\":\"2026-08-21T00:00:00Z\"}}"
        )?;
        writeln!(
            output,
            "{{\"id\":\"u1\",\"type\":\"user\",\"timestamp\":\"2026-08-21T00:00:00Z\",\"content\":\"Hello\"}}"
        )?;
        writeln!(
            output,
            "{{\"id\":\"a1\",\"type\":\"gemini\",\"timestamp\":\"2026-08-21T00:00:01Z\",\"content\":\"Hi\"}}"
        )?;
        assert_eq!(
            parse_session(&jsonl, None)?.expect("jsonl").messages.len(),
            2
        );
        Ok(())
    }
}
