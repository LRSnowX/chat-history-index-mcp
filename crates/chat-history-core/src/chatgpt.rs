use std::{
    collections::{BTreeMap, HashSet},
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::{Context, anyhow, ensure};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{DataHome, NormalizedConversation, NormalizedMessage};

const CHATGPT_SYNC_STATE_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct ChatGptBridgeThread {
    pub thread_id: String,
    pub kind: String,
    pub title: String,
    pub create_time: Option<f64>,
    pub update_time: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct ChatGptThreadListSnapshot {
    pub requested_limit: usize,
    pub threads: Vec<ChatGptBridgeThread>,
    #[serde(default)]
    pub pinned_threads: Vec<ChatGptBridgeThread>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct ChatGptBridgeMessage {
    pub message_id: String,
    pub role: String,
    pub create_time: Option<f64>,
    pub text: String,
    #[serde(default)]
    pub truncated: bool,
    #[serde(default)]
    pub inaccessible: bool,
    #[serde(default)]
    pub raw: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct ChatGptBridgePage {
    pub request_cursor: Option<String>,
    pub next_cursor: Option<String>,
    pub has_more: bool,
    #[serde(default)]
    pub messages: Vec<ChatGptBridgeMessage>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct ChatGptBridgeTranscript {
    pub thread_id: String,
    pub title: String,
    pub create_time: Option<f64>,
    pub update_time: Option<f64>,
    pub model: Option<String>,
    pub source_url: Option<String>,
    #[serde(default)]
    pub attachment_metadata: Vec<Value>,
    #[serde(default)]
    pub pages: Vec<ChatGptBridgePage>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct ChatGptPendingThread {
    pub thread_id: String,
    pub title: String,
    pub create_time: Option<f64>,
    pub update_time: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct ChatGptBlockedThread {
    pub thread_id: String,
    pub title: String,
    pub update_time: Option<f64>,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct ChatGptSyncState {
    pub version: u32,
    pub last_successful_update_time: Option<f64>,
    pub active_high_watermark: Option<f64>,
    pub discovery_overflow: bool,
    #[serde(default)]
    pub pending: BTreeMap<String, ChatGptPendingThread>,
    #[serde(default)]
    pub blocked: BTreeMap<String, ChatGptBlockedThread>,
    #[serde(default)]
    pub completed_since_cursor: BTreeMap<String, f64>,
}

impl Default for ChatGptSyncState {
    fn default() -> Self {
        Self {
            version: CHATGPT_SYNC_STATE_VERSION,
            last_successful_update_time: None,
            active_high_watermark: None,
            discovery_overflow: false,
            pending: BTreeMap::new(),
            blocked: BTreeMap::new(),
            completed_since_cursor: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct ChatGptDiscoveryPlan {
    pub cursor_before: Option<f64>,
    pub high_watermark: Option<f64>,
    pub discovery_overflow: bool,
    pub selected: Vec<ChatGptPendingThread>,
    pub skipped_blocked_ids: Vec<String>,
}

impl ChatGptSyncState {
    pub fn load(data_home: &DataHome) -> anyhow::Result<Self> {
        let path = sync_state_path(data_home);
        if !path.exists() {
            return Ok(Self::default());
        }
        let bytes = fs::read(&path)
            .with_context(|| format!("reading ChatGPT sync state at {}", path.display()))?;
        let state: Self = serde_json::from_slice(&bytes)
            .with_context(|| format!("parsing ChatGPT sync state at {}", path.display()))?;
        ensure!(
            state.version == CHATGPT_SYNC_STATE_VERSION,
            "unsupported ChatGPT sync state version {}; expected {}",
            state.version,
            CHATGPT_SYNC_STATE_VERSION
        );
        Ok(state)
    }

    pub fn save(&self, data_home: &DataHome) -> anyhow::Result<PathBuf> {
        let paths = data_home.paths();
        paths.ensure()?;
        let target = sync_state_path(data_home);
        let parent = target
            .parent()
            .ok_or_else(|| anyhow!("ChatGPT sync state path has no parent"))?;
        let mut staged = tempfile::NamedTempFile::new_in(parent)
            .context("creating staged ChatGPT sync state")?;
        serde_json::to_writer_pretty(&mut staged, self)
            .context("serializing ChatGPT sync state")?;
        staged.write_all(b"\n")?;
        staged.as_file().sync_all()?;
        staged
            .persist(&target)
            .map_err(|error| error.error)
            .with_context(|| format!("persisting ChatGPT sync state to {}", target.display()))?;
        Ok(target)
    }

    pub fn plan_recent(
        &mut self,
        snapshot: ChatGptThreadListSnapshot,
    ) -> anyhow::Result<ChatGptDiscoveryPlan> {
        ensure!(
            snapshot.requested_limit > 0,
            "requested_limit must be greater than zero"
        );

        let cursor_before = self.last_successful_update_time;
        let listed_count = snapshot.threads.len();
        let newest_seen = snapshot
            .threads
            .iter()
            .chain(snapshot.pinned_threads.iter())
            .filter_map(|thread| thread.update_time)
            .max_by(|left, right| left.total_cmp(right));

        let all_listed_newer_than_cursor = match cursor_before {
            Some(cursor) if listed_count >= snapshot.requested_limit => snapshot
                .threads
                .iter()
                .all(|thread| thread.update_time.is_some_and(|updated| updated > cursor)),
            None if listed_count >= snapshot.requested_limit => true,
            _ => false,
        };
        if all_listed_newer_than_cursor {
            self.discovery_overflow = true;
        }
        self.active_high_watermark = match (self.active_high_watermark, newest_seen) {
            (Some(existing), Some(newest)) => Some(existing.max(newest)),
            (existing, newest) => existing.or(newest),
        };

        let mut selected = Vec::new();
        let mut skipped_blocked_ids = Vec::new();
        for thread in snapshot
            .threads
            .into_iter()
            .chain(snapshot.pinned_threads.into_iter())
        {
            if thread.kind != "chatgpt" {
                continue;
            }
            let update_time = thread.update_time.ok_or_else(|| {
                anyhow!("ChatGPT thread {} is missing update_time", thread.thread_id)
            })?;
            if cursor_before.is_some_and(|cursor| update_time <= cursor) {
                continue;
            }
            if self.blocked.contains_key(&thread.thread_id) {
                skipped_blocked_ids.push(thread.thread_id);
                continue;
            }
            if self
                .completed_since_cursor
                .get(&thread.thread_id)
                .is_some_and(|completed| *completed >= update_time)
            {
                continue;
            }
            let pending = ChatGptPendingThread {
                thread_id: thread.thread_id.clone(),
                title: thread.title,
                create_time: thread.create_time,
                update_time,
            };
            self.pending
                .insert(thread.thread_id.clone(), pending.clone());
            selected.push(pending);
        }
        selected.sort_by(|left, right| right.update_time.total_cmp(&left.update_time));

        Ok(ChatGptDiscoveryPlan {
            cursor_before,
            high_watermark: self.active_high_watermark,
            discovery_overflow: self.discovery_overflow,
            selected,
            skipped_blocked_ids,
        })
    }

    pub fn mark_blocked(
        &mut self,
        thread_id: &str,
        reason: impl Into<String>,
    ) -> ChatGptBlockedThread {
        let pending = self.pending.remove(thread_id);
        let blocked = ChatGptBlockedThread {
            thread_id: thread_id.to_string(),
            title: pending
                .as_ref()
                .map(|value| value.title.clone())
                .unwrap_or_default(),
            update_time: pending.as_ref().map(|value| value.update_time),
            reason: reason.into(),
        };
        self.blocked.insert(thread_id.to_string(), blocked.clone());
        blocked
    }

    pub fn mark_imported(&mut self, thread_id: &str) {
        self.mark_imported_at(thread_id, None);
    }

    pub fn mark_imported_at(&mut self, thread_id: &str, imported_update_time: Option<f64>) {
        let pending_update_time = self
            .pending
            .remove(thread_id)
            .map(|value| value.update_time)
            .or_else(|| {
                self.blocked
                    .remove(thread_id)
                    .and_then(|value| value.update_time)
            });
        let update_time = match (pending_update_time, imported_update_time) {
            (Some(pending), Some(imported)) => Some(pending.max(imported)),
            (pending, imported) => pending.or(imported),
        };
        self.blocked.remove(thread_id);
        if let Some(update_time) = update_time {
            self.completed_since_cursor
                .insert(thread_id.to_string(), update_time);
        }
        self.maybe_advance_cursor();
    }

    pub fn seed_cursor(&mut self, update_time: f64) -> anyhow::Result<()> {
        ensure!(update_time.is_finite(), "ChatGPT cursor must be finite");
        self.last_successful_update_time = Some(update_time);
        self.active_high_watermark = None;
        self.discovery_overflow = false;
        self.pending.clear();
        self.completed_since_cursor.clear();
        Ok(())
    }

    fn maybe_advance_cursor(&mut self) {
        if self.discovery_overflow || !self.pending.is_empty() || !self.blocked.is_empty() {
            return;
        }
        if let Some(high_watermark) = self.active_high_watermark.take() {
            self.last_successful_update_time = Some(
                self.last_successful_update_time
                    .map_or(high_watermark, |cursor| cursor.max(high_watermark)),
            );
            self.completed_since_cursor.clear();
        }
    }
}

impl ChatGptBridgeTranscript {
    pub fn into_normalized(self) -> anyhow::Result<NormalizedConversation> {
        ensure!(
            !self.thread_id.trim().is_empty(),
            "ChatGPT thread_id is empty"
        );
        ensure!(!self.pages.is_empty(), "ChatGPT transcript has no pages");

        let mut expected_request_cursor: Option<String> = None;
        let mut seen_message_ids = HashSet::new();
        let mut newest_first = Vec::new();

        for (index, page) in self.pages.iter().enumerate() {
            ensure!(
                page.request_cursor == expected_request_cursor,
                "ChatGPT page {} cursor chain mismatch: expected {:?}, got {:?}",
                index,
                expected_request_cursor,
                page.request_cursor
            );
            if page.has_more {
                ensure!(
                    page.next_cursor
                        .as_deref()
                        .is_some_and(|cursor| !cursor.is_empty()),
                    "ChatGPT page {} has_more=true but next_cursor is missing",
                    index
                );
            } else {
                ensure!(
                    page.next_cursor.is_none(),
                    "ChatGPT final page {} has next_cursor despite has_more=false",
                    index
                );
                ensure!(
                    index + 1 == self.pages.len(),
                    "ChatGPT transcript contains pages after terminal page {}",
                    index
                );
            }

            for message in &page.messages {
                ensure!(
                    !message.truncated,
                    "ChatGPT message {} is truncated",
                    message.message_id
                );
                ensure!(
                    !message.inaccessible,
                    "ChatGPT message {} is inaccessible",
                    message.message_id
                );
                ensure!(
                    !message.message_id.trim().is_empty(),
                    "ChatGPT transcript contains an empty message_id"
                );
                ensure!(
                    seen_message_ids.insert(message.message_id.clone()),
                    "ChatGPT transcript contains duplicate message_id {}",
                    message.message_id
                );
                newest_first.push(message.clone());
            }
            expected_request_cursor = page.next_cursor.clone();
        }

        ensure!(
            self.pages.last().is_some_and(|page| !page.has_more),
            "ChatGPT transcript is incomplete: last page still has_more=true"
        );

        newest_first.reverse();
        let messages = newest_first
            .into_iter()
            .map(|message| NormalizedMessage {
                message_id: message.message_id,
                role: message.role,
                create_time: message.create_time,
                text: message.text,
                raw: message.raw,
            })
            .collect();

        Ok(NormalizedConversation {
            source: "chatgpt".to_string(),
            source_instance: None,
            source_conversation_id: self.thread_id,
            title: self.title,
            create_time: self.create_time,
            update_time: self.update_time,
            model: self.model,
            source_url: self.source_url,
            source_path: None,
            messages,
            raw: serde_json::json!({
                "collector": "chatgpt-app-bridge-v1",
                "attachment_metadata": self.attachment_metadata,
            }),
        })
    }
}

pub fn sync_state_path(data_home: &DataHome) -> PathBuf {
    data_home.paths().cache_dir.join("chatgpt-sync-state.json")
}

pub fn read_json_input<T: for<'de> Deserialize<'de>>(path: &Path) -> anyhow::Result<T> {
    let bytes = fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    serde_json::from_slice(&bytes).with_context(|| format!("parsing {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn thread(id: &str, kind: &str, updated: f64) -> ChatGptBridgeThread {
        ChatGptBridgeThread {
            thread_id: id.to_string(),
            kind: kind.to_string(),
            title: id.to_string(),
            create_time: Some(updated - 1.0),
            update_time: Some(updated),
        }
    }

    fn message(id: &str, text: &str) -> ChatGptBridgeMessage {
        ChatGptBridgeMessage {
            message_id: id.to_string(),
            role: if id.starts_with('u') {
                "user"
            } else {
                "assistant"
            }
            .to_string(),
            create_time: None,
            text: text.to_string(),
            truncated: false,
            inaccessible: false,
            raw: Value::Null,
        }
    }

    #[test]
    fn recent_plan_filters_kind_and_detects_overflow_without_advancing_cursor() {
        let mut state = ChatGptSyncState {
            last_successful_update_time: Some(100.0),
            ..ChatGptSyncState::default()
        };
        let snapshot = ChatGptThreadListSnapshot {
            requested_limit: 3,
            threads: vec![
                thread("chat-new", "chatgpt", 130.0),
                thread("codex-new", "codex", 120.0),
                thread("chat-mid", "chatgpt", 110.0),
            ],
            pinned_threads: Vec::new(),
        };
        let plan = state.plan_recent(snapshot).unwrap();
        assert!(plan.discovery_overflow);
        assert_eq!(plan.selected.len(), 2);
        assert_eq!(state.last_successful_update_time, Some(100.0));
    }

    #[test]
    fn pinned_threads_are_selected_without_masking_recent_list_overflow() {
        let mut state = ChatGptSyncState {
            last_successful_update_time: Some(100.0),
            ..ChatGptSyncState::default()
        };
        let plan = state
            .plan_recent(ChatGptThreadListSnapshot {
                requested_limit: 2,
                threads: vec![
                    thread("recent-a", "chatgpt", 130.0),
                    thread("recent-b", "chatgpt", 120.0),
                ],
                pinned_threads: vec![
                    thread("pinned-new", "chatgpt", 125.0),
                    thread("pinned-old", "chatgpt", 90.0),
                ],
            })
            .unwrap();
        assert!(plan.discovery_overflow);
        let ids = plan
            .selected
            .iter()
            .map(|thread| thread.thread_id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(ids, vec!["recent-a", "pinned-new", "recent-b"]);
    }

    #[test]
    fn blocked_thread_is_skipped_and_prevents_cursor_advancement() {
        let mut state = ChatGptSyncState {
            last_successful_update_time: Some(100.0),
            ..ChatGptSyncState::default()
        };
        state.blocked.insert(
            "blocked".to_string(),
            ChatGptBlockedThread {
                thread_id: "blocked".to_string(),
                title: "blocked".to_string(),
                update_time: Some(105.0),
                reason: "too large".to_string(),
            },
        );
        let plan = state
            .plan_recent(ChatGptThreadListSnapshot {
                requested_limit: 50,
                threads: vec![
                    thread("new", "chatgpt", 110.0),
                    thread("blocked", "chatgpt", 105.0),
                ],
                pinned_threads: Vec::new(),
            })
            .unwrap();
        assert_eq!(plan.selected.len(), 1);
        assert_eq!(plan.skipped_blocked_ids, vec!["blocked"]);
        state.mark_imported("new");
        assert_eq!(state.last_successful_update_time, Some(100.0));
    }

    #[test]
    fn complete_transcript_reverses_newest_first_pages_to_chronological_order() {
        let transcript = ChatGptBridgeTranscript {
            thread_id: "thread-1".to_string(),
            title: "Test".to_string(),
            create_time: Some(1.0),
            update_time: Some(4.0),
            model: None,
            source_url: None,
            attachment_metadata: Vec::new(),
            pages: vec![
                ChatGptBridgePage {
                    request_cursor: None,
                    next_cursor: Some("older-1".to_string()),
                    has_more: true,
                    messages: vec![message("a2", "newest"), message("u2", "middle-new")],
                },
                ChatGptBridgePage {
                    request_cursor: Some("older-1".to_string()),
                    next_cursor: None,
                    has_more: false,
                    messages: vec![message("a1", "middle-old"), message("u1", "oldest")],
                },
            ],
        };
        let normalized = transcript.into_normalized().unwrap();
        let ids = normalized
            .messages
            .iter()
            .map(|message| message.message_id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(ids, vec!["u1", "a1", "u2", "a2"]);
    }

    #[test]
    fn incomplete_or_truncated_transcript_is_rejected() {
        let mut truncated = message("u1", "partial");
        truncated.truncated = true;
        let transcript = ChatGptBridgeTranscript {
            thread_id: "thread-1".to_string(),
            title: "Test".to_string(),
            create_time: None,
            update_time: None,
            model: None,
            source_url: None,
            attachment_metadata: Vec::new(),
            pages: vec![ChatGptBridgePage {
                request_cursor: None,
                next_cursor: None,
                has_more: false,
                messages: vec![truncated],
            }],
        };
        assert!(transcript.into_normalized().is_err());
    }

    #[test]
    fn cursor_advances_only_after_clean_non_overflow_batch() {
        let mut state = ChatGptSyncState {
            last_successful_update_time: Some(100.0),
            ..ChatGptSyncState::default()
        };
        state
            .plan_recent(ChatGptThreadListSnapshot {
                requested_limit: 50,
                threads: vec![
                    thread("a", "chatgpt", 120.0),
                    thread("b", "chatgpt", 110.0),
                    thread("old", "chatgpt", 90.0),
                ],
                pinned_threads: Vec::new(),
            })
            .unwrap();
        state.mark_imported("a");
        assert_eq!(state.last_successful_update_time, Some(100.0));
        state.mark_imported("b");
        assert_eq!(state.last_successful_update_time, Some(120.0));
        assert_eq!(state.active_high_watermark, None);
        assert!(state.completed_since_cursor.is_empty());
    }

    #[test]
    fn imported_threads_are_not_requeued_while_another_thread_blocks_cursor() {
        let mut state = ChatGptSyncState {
            last_successful_update_time: Some(100.0),
            ..ChatGptSyncState::default()
        };
        state
            .plan_recent(ChatGptThreadListSnapshot {
                requested_limit: 50,
                threads: vec![
                    thread("done", "chatgpt", 120.0),
                    thread("blocked", "chatgpt", 110.0),
                    thread("old", "chatgpt", 90.0),
                ],
                pinned_threads: Vec::new(),
            })
            .unwrap();
        state.mark_imported("done");
        state.mark_blocked("blocked", "incomplete");
        let plan = state
            .plan_recent(ChatGptThreadListSnapshot {
                requested_limit: 50,
                threads: vec![
                    thread("done", "chatgpt", 120.0),
                    thread("blocked", "chatgpt", 110.0),
                    thread("old", "chatgpt", 90.0),
                ],
                pinned_threads: Vec::new(),
            })
            .unwrap();
        assert!(plan.selected.is_empty());
        assert_eq!(plan.skipped_blocked_ids, vec!["blocked"]);
        assert_eq!(state.last_successful_update_time, Some(100.0));
    }

    #[test]
    fn imported_thread_is_requeued_if_it_changes_before_cursor_advances() {
        let mut state = ChatGptSyncState {
            last_successful_update_time: Some(100.0),
            ..ChatGptSyncState::default()
        };
        state
            .plan_recent(ChatGptThreadListSnapshot {
                requested_limit: 50,
                threads: vec![
                    thread("changing", "chatgpt", 120.0),
                    thread("blocked", "chatgpt", 110.0),
                ],
                pinned_threads: Vec::new(),
            })
            .unwrap();
        state.mark_imported_at("changing", Some(121.0));
        state.mark_blocked("blocked", "incomplete");

        let unchanged = state
            .plan_recent(ChatGptThreadListSnapshot {
                requested_limit: 50,
                threads: vec![thread("changing", "chatgpt", 121.0)],
                pinned_threads: Vec::new(),
            })
            .unwrap();
        assert!(unchanged.selected.is_empty());

        let changed = state
            .plan_recent(ChatGptThreadListSnapshot {
                requested_limit: 50,
                threads: vec![thread("changing", "chatgpt", 130.0)],
                pinned_threads: Vec::new(),
            })
            .unwrap();
        assert_eq!(changed.selected.len(), 1);
        assert_eq!(changed.selected[0].update_time, 130.0);
    }
}
