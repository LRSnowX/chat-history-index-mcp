use std::{collections::BTreeMap, path::PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SummaryRecord {
    pub abstract_text: String,
    pub key_points: Vec<String>,
    pub candidate_topics: Vec<String>,
    pub entities: Vec<String>,
    pub risk_flags: Vec<String>,
    pub site_usefulness: String,
    pub redaction_notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ConversationRecord {
    pub conversation_id: String,
    pub source: String,
    pub source_instance: Option<String>,
    pub source_conversation_id: String,
    pub source_url: Option<String>,
    pub source_path: Option<String>,
    pub title: String,
    pub create_time: Option<f64>,
    pub update_time: Option<f64>,
    pub default_model_slug: Option<String>,
    pub summary: Option<SummaryRecord>,
    pub risk_flags: Vec<String>,
    pub topic_tags: Vec<String>,
    pub review_status: Option<String>,
    pub publish_candidate: bool,
    pub site_category: Option<String>,
    pub era_bucket: Option<String>,
    pub message_count: i64,
    pub user_message_count: i64,
    pub assistant_message_count: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ConversationMessage {
    pub message_id: String,
    pub conversation_id: String,
    pub role: String,
    pub create_time: Option<f64>,
    pub turn_index: i64,
    pub normalized_text: String,
    pub raw_message_json: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AttachmentRecord {
    pub attachment_id: String,
    pub conversation_id: String,
    pub archive_path: String,
    pub extension: Option<String>,
    pub size_bytes: Option<i64>,
    pub source_ref: String,
    pub linkage_json: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ConversationDetail {
    pub conversation: ConversationRecord,
    pub summary_json: Option<Value>,
    pub messages: Vec<ConversationMessage>,
    pub attachments: Vec<AttachmentRecord>,
    pub raw_json: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SearchResult {
    pub conversation_id: String,
    pub source: String,
    pub source_instance: Option<String>,
    pub source_conversation_id: String,
    pub title: String,
    pub create_time: Option<f64>,
    pub update_time: Option<f64>,
    pub default_model_slug: Option<String>,
    pub score: Option<f64>,
    pub snippet: Option<String>,
    pub risk_flags: Vec<String>,
    pub topic_tags: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SearchMode {
    Metadata,
    Fts,
    Semantic,
    Hybrid,
}

impl SearchMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Metadata => "metadata",
            Self::Fts => "fts",
            Self::Semantic => "semantic",
            Self::Hybrid => "hybrid",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, JsonSchema)]
pub struct SearchOptions {
    pub query: Option<String>,
    pub mode: Option<SearchMode>,
    pub date_from: Option<f64>,
    pub date_to: Option<f64>,
    pub model: Option<String>,
    pub sources: Vec<String>,
    pub risk_flags: Vec<String>,
    pub topic_tags: Vec<String>,
    pub limit: Option<usize>,
    pub sort: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct IndexStats {
    pub archive_path: Option<PathBuf>,
    pub conversations: i64,
    pub messages: i64,
    pub attachments: i64,
    pub summaries_complete: i64,
    pub embeddings_complete: i64,
    pub latest_update_time: Option<f64>,
    pub conversations_by_source: BTreeMap<String, i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SourceHealth {
    pub conversations: i64,
    pub messages: i64,
    pub newest_update_time: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct DatabaseHealth {
    pub database_path: PathBuf,
    pub schema_version: i64,
    pub integrity_check: String,
    pub journal_mode: String,
    pub conversations: i64,
    pub messages: i64,
    pub sources: BTreeMap<String, SourceHealth>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RestoreReport {
    pub restored_from: PathBuf,
    pub database_path: PathBuf,
    pub previous_database_backup: Option<PathBuf>,
    pub health: DatabaseHealth,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct NormalizedMessage {
    pub message_id: String,
    pub role: String,
    pub create_time: Option<f64>,
    pub text: String,
    #[serde(default)]
    pub raw: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct NormalizedConversation {
    pub source: String,
    pub source_instance: Option<String>,
    pub source_conversation_id: String,
    pub title: String,
    pub create_time: Option<f64>,
    pub update_time: Option<f64>,
    pub model: Option<String>,
    pub source_url: Option<String>,
    pub source_path: Option<String>,
    #[serde(default)]
    pub messages: Vec<NormalizedMessage>,
    #[serde(default)]
    pub raw: Value,
}

impl NormalizedConversation {
    pub fn canonical_id(&self) -> String {
        if self.source == "chatgpt" && self.source_instance.is_none() {
            return self.source_conversation_id.clone();
        }
        match self
            .source_instance
            .as_deref()
            .filter(|value| !value.is_empty())
        {
            Some(instance) => format!(
                "{}:{}:{}",
                self.source, instance, self.source_conversation_id
            ),
            None => format!("{}:{}", self.source, self.source_conversation_id),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum JobKind {
    Summary,
    Embedding,
}

impl JobKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Summary => "summary",
            Self::Embedding => "embedding",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    Pending,
    Running,
    Complete,
    Failed,
}

impl JobStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Complete => "complete",
            Self::Failed => "failed",
        }
    }
}
