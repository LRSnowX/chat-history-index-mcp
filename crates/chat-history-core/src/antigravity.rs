use std::{
    collections::HashSet,
    fs::{self, File},
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
};

use anyhow::{Context, anyhow};
use chrono::DateTime;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::models::{NormalizedConversation, NormalizedMessage};

/// Discover plaintext Antigravity transcript artifacts only. Undocumented `.pb` and `.db`
/// application stores are deliberately never decoded by this collector.
pub fn discover_transcripts(roots: &[PathBuf]) -> anyhow::Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    for root in roots {
        visit(root, &mut paths)?;
    }
    paths.retain(|path| {
        path.file_name().and_then(|name| name.to_str()) != Some("transcript.jsonl")
            || !path.with_file_name("transcript_full.jsonl").is_file()
    });
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
            .unwrap_or_default()
            .to_ascii_lowercase();
        if name == "transcript.jsonl"
            || name == "transcript_full.jsonl"
            || name == "transcript.json"
            || ((name.contains("conversation") || name.contains("export"))
                && (name.ends_with(".json") || name.ends_with(".jsonl")))
        {
            paths.push(path.to_path_buf());
        }
        return Ok(());
    }
    for entry in fs::read_dir(path).with_context(|| format!("reading {}", path.display()))? {
        visit(&entry?.path(), paths)?;
    }
    Ok(())
}

pub fn parse_transcript(
    path: &Path,
    since: Option<f64>,
) -> anyhow::Result<Option<NormalizedConversation>> {
    let (metadata, items) = read_transcript(path)?;
    let session_id = id_from(&metadata)
        .or_else(|| conversation_id_from_path(path))
        .unwrap_or_else(|| stable_path_id(path));
    let mut create_time = timestamp(&metadata);
    let mut update_time = timestamp(&metadata);
    let mut model = metadata
        .get("model")
        .or_else(|| metadata.get("modelName"))
        .and_then(Value::as_str)
        .map(ToString::to_string);
    let mut messages = Vec::new();
    let mut seen = HashSet::new();
    for (position, item) in items.iter().enumerate() {
        let Some(role) = role_from(item) else {
            continue;
        };
        let text = text_from(item);
        if text.is_empty() {
            continue;
        }
        let message_time = timestamp(item);
        if let Some(time) = message_time {
            create_time = Some(create_time.map_or(time, |current| current.min(time)));
            update_time = Some(update_time.map_or(time, |current| current.max(time)));
        }
        if model.is_none() {
            model = item
                .get("model")
                .or_else(|| item.get("modelName"))
                .and_then(Value::as_str)
                .map(ToString::to_string);
        }
        let message_id = item
            .get("id")
            .or_else(|| item.get("messageId"))
            .and_then(Value::as_str)
            .map(ToString::to_string)
            .unwrap_or_else(|| stable_message_id(&session_id, role, message_time, &text, position));
        if !seen.insert(message_id.clone()) {
            continue;
        }
        messages.push(NormalizedMessage {
            message_id,
            role: role.to_string(),
            create_time: message_time,
            text,
            raw: item.clone(),
        });
    }
    let effective = update_time.or(create_time).unwrap_or_default();
    if since.is_some_and(|cutoff| effective < cutoff) {
        return Ok(None);
    }
    if messages.is_empty() {
        return Ok(None);
    }
    let title = metadata
        .get("title")
        .and_then(Value::as_str)
        .map(ToString::to_string)
        .or_else(|| {
            messages
                .iter()
                .find(|message| message.role == "user")
                .map(|message| concise_title(&message.text))
        })
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "Antigravity conversation".to_string());
    Ok(Some(NormalizedConversation {
        source: "antigravity".to_string(),
        source_instance: None,
        source_conversation_id: session_id,
        title,
        create_time,
        update_time,
        model,
        source_url: None,
        source_path: Some(path.display().to_string()),
        messages,
        raw: json!({"format":"antigravity-transcript", "source_path":path.display().to_string()}),
    }))
}

fn read_transcript(path: &Path) -> anyhow::Result<(Value, Vec<Value>)> {
    let text = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    if let Ok(value) = serde_json::from_str::<Value>(&text) {
        let object = value
            .as_object()
            .ok_or_else(|| anyhow!("Antigravity transcript root must be an object"))?;
        let messages = object
            .get("messages")
            .or_else(|| object.get("turns"))
            .and_then(Value::as_array)
            .ok_or_else(|| anyhow!("Antigravity JSON transcript is missing messages"))?
            .clone();
        return Ok((value, messages));
    }
    let mut values = Vec::new();
    for line in BufReader::new(File::open(path)?).lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        values.push(
            serde_json::from_str::<Value>(&line)
                .with_context(|| format!("parsing JSONL in {}", path.display()))?,
        );
    }
    let Some(first) = values.first().cloned() else {
        return Err(anyhow!("Antigravity JSONL transcript is empty"));
    };
    if let Some(messages) = first.get("messages").and_then(Value::as_array).cloned() {
        return Ok((first, messages));
    }
    let mut metadata = first;
    let start = if id_from(&metadata).is_some() || metadata.get("title").is_some() {
        1
    } else {
        0
    };
    if start == 0 {
        metadata = json!({});
    }
    Ok((metadata, values.into_iter().skip(start).collect()))
}

fn id_from(value: &Value) -> Option<String> {
    [
        "conversationId",
        "conversation_id",
        "sessionId",
        "session_id",
        "cascadeId",
        "id",
    ]
    .iter()
    .find_map(|key| {
        value
            .get(key)
            .and_then(Value::as_str)
            .map(ToString::to_string)
    })
}

fn role_from(value: &Value) -> Option<&'static str> {
    let direct = match value
        .get("role")
        .or_else(|| value.get("type"))
        .and_then(Value::as_str)
    {
        Some("user") | Some("userMessage") | Some("user_message") => Some("user"),
        Some("assistant")
        | Some("agent")
        | Some("model")
        | Some("agentMessage")
        | Some("agent_message") => Some("assistant"),
        _ => None,
    };
    if direct.is_some() {
        return direct;
    }
    match (
        value.get("source").and_then(Value::as_str),
        value.get("type").and_then(Value::as_str),
    ) {
        (Some("USER"), Some("USER_INPUT")) => Some("user"),
        (Some("MODEL"), Some("PLANNER_RESPONSE" | "MODEL_RESPONSE" | "ASSISTANT_RESPONSE")) => {
            Some("assistant")
        }
        _ => None,
    }
}

fn text_from(value: &Value) -> String {
    for key in ["text", "message", "content", "parts"] {
        match value.get(key) {
            Some(Value::String(text)) => return text.clone(),
            Some(Value::Array(items)) => {
                let text = items
                    .iter()
                    .filter_map(|item| item.get("text").and_then(Value::as_str))
                    .collect::<Vec<_>>()
                    .join("\n");
                if !text.is_empty() {
                    return text;
                }
            }
            Some(Value::Object(item)) => {
                if let Some(text) = item.get("text").and_then(Value::as_str) {
                    return text.to_string();
                }
            }
            _ => {}
        }
    }
    String::new()
}

fn timestamp(value: &Value) -> Option<f64> {
    [
        "timestamp",
        "createdAt",
        "created_at",
        "updatedAt",
        "updated_at",
        "time",
    ]
    .iter()
    .find_map(|key| match value.get(*key) {
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
    })
}

fn stable_path_id(path: &Path) -> String {
    let mut digest = Sha256::new();
    digest.update(path.display().to_string().as_bytes());
    format!("path-{}", hex::encode(digest.finalize()))
}

fn conversation_id_from_path(path: &Path) -> Option<String> {
    let mut components = path
        .components()
        .filter_map(|component| component.as_os_str().to_str());
    while let Some(component) = components.next() {
        if component == "brain" {
            return components.next().map(ToString::to_string);
        }
    }
    None
}
fn stable_message_id(
    session: &str,
    role: &str,
    time: Option<f64>,
    text: &str,
    position: usize,
) -> String {
    let mut digest = Sha256::new();
    digest.update(session.as_bytes());
    digest.update(role.as_bytes());
    digest.update(time.unwrap_or_default().to_le_bytes());
    digest.update(text.as_bytes());
    digest.update(position.to_le_bytes());
    format!("antigravity-{}", hex::encode(digest.finalize()))
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
    use super::parse_transcript;
    use std::{fs, io::Write};
    use tempfile::TempDir;
    #[test]
    fn parses_plaintext_transcript_jsonl() -> anyhow::Result<()> {
        let temp = TempDir::new()?;
        let path = temp
            .path()
            .join("brain/antigravity-a/.system_generated/logs/transcript.jsonl");
        fs::create_dir_all(path.parent().expect("parent"))?;
        let mut file = fs::File::create(&path)?;
        writeln!(
            file,
            "{{\"source\":\"USER\",\"type\":\"USER_INPUT\",\"created_at\":\"2026-08-21T00:00:00Z\",\"content\":\"Index this\"}}"
        )?;
        writeln!(
            file,
            "{{\"source\":\"MODEL\",\"type\":\"PLANNER_RESPONSE\",\"status\":\"DONE\",\"modelName\":\"gemini-test\",\"created_at\":\"2026-08-21T00:00:01Z\",\"content\":\"Imported.\"}}"
        )?;
        let parsed = parse_transcript(&path, None)?.expect("conversation");
        assert_eq!(parsed.canonical_id(), "antigravity:antigravity-a");
        assert_eq!(parsed.messages.len(), 2);
        assert_eq!(parsed.model.as_deref(), Some("gemini-test"));
        Ok(())
    }
}
