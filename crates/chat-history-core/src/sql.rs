pub const SCHEMA: &str = r#"
PRAGMA journal_mode = WAL;
PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS archives (
  id INTEGER PRIMARY KEY,
  archive_path TEXT NOT NULL,
  source_path TEXT NOT NULL,
  sha256_hex TEXT NOT NULL,
  size_bytes INTEGER NOT NULL,
  import_mode TEXT NOT NULL,
  imported_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE IF NOT EXISTS conversations (
  id INTEGER PRIMARY KEY,
  conversation_id TEXT NOT NULL UNIQUE,
  archive_id INTEGER NOT NULL REFERENCES archives(id) ON DELETE CASCADE,
  archive_member TEXT NOT NULL,
  source_member TEXT NOT NULL,
  title TEXT NOT NULL,
  create_time REAL,
  update_time REAL,
  default_model_slug TEXT,
  message_count INTEGER NOT NULL DEFAULT 0,
  user_message_count INTEGER NOT NULL DEFAULT 0,
  assistant_message_count INTEGER NOT NULL DEFAULT 0,
  transcript_text TEXT NOT NULL DEFAULT '',
  transcript_digest TEXT NOT NULL DEFAULT '',
  raw_conversation_zstd BLOB NOT NULL,
  raw_json_sha256_hex TEXT NOT NULL,
  summary_json TEXT,
  summary_model TEXT,
  summary_completed_at TEXT,
  embedding_blob BLOB,
  embedding_dimensions INTEGER,
  embedding_model TEXT,
  embedding_completed_at TEXT,
  risk_flags_json TEXT NOT NULL DEFAULT '[]',
  topic_tags_json TEXT NOT NULL DEFAULT '[]',
  review_status TEXT,
  publish_candidate INTEGER NOT NULL DEFAULT 0,
  site_category TEXT,
  redaction_notes_json TEXT NOT NULL DEFAULT '[]',
  era_bucket TEXT
  ,source TEXT NOT NULL DEFAULT 'chatgpt'
  ,source_instance TEXT
  ,source_conversation_id TEXT
  ,source_url TEXT
  ,source_path TEXT
  ,parent_conversation_id TEXT
  ,ingested_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE IF NOT EXISTS messages (
  id INTEGER PRIMARY KEY,
  conversation_id TEXT NOT NULL REFERENCES conversations(conversation_id) ON DELETE CASCADE,
  message_id TEXT NOT NULL,
  role TEXT NOT NULL,
  create_time REAL,
  turn_index INTEGER NOT NULL,
  normalized_text TEXT NOT NULL,
  raw_message_json TEXT NOT NULL,
  UNIQUE(conversation_id, message_id)
);

CREATE TABLE IF NOT EXISTS conversation_embedding_chunks (
  conversation_id TEXT NOT NULL REFERENCES conversations(conversation_id) ON DELETE CASCADE,
  chunk_index INTEGER NOT NULL,
  embedding_blob BLOB NOT NULL,
  embedding_dimensions INTEGER NOT NULL,
  embedding_model TEXT NOT NULL,
  PRIMARY KEY(conversation_id, chunk_index)
);

CREATE TABLE IF NOT EXISTS attachments (
  id INTEGER PRIMARY KEY,
  conversation_id TEXT NOT NULL REFERENCES conversations(conversation_id) ON DELETE CASCADE,
  attachment_id TEXT NOT NULL,
  archive_path TEXT NOT NULL,
  extension TEXT,
  size_bytes INTEGER,
  source_ref TEXT NOT NULL,
  linkage_json TEXT NOT NULL,
  UNIQUE(conversation_id, attachment_id, archive_path)
);

CREATE TABLE IF NOT EXISTS jobs (
  id INTEGER PRIMARY KEY,
  conversation_id TEXT NOT NULL REFERENCES conversations(conversation_id) ON DELETE CASCADE,
  kind TEXT NOT NULL,
  status TEXT NOT NULL,
  attempts INTEGER NOT NULL DEFAULT 0,
  last_error TEXT,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  completed_at TEXT,
  UNIQUE(conversation_id, kind)
);

CREATE TABLE IF NOT EXISTS runs (
  id INTEGER PRIMARY KEY,
  run_kind TEXT NOT NULL,
  archive_path TEXT,
  started_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  completed_at TEXT,
  status TEXT NOT NULL,
  counters_json TEXT NOT NULL DEFAULT '{}',
  notes_json TEXT NOT NULL DEFAULT '{}'
);

CREATE VIRTUAL TABLE IF NOT EXISTS conversation_fts USING fts5(
  conversation_id UNINDEXED,
  title,
  transcript_text,
  summary_text,
  topic_tags,
  tokenize = 'unicode61'
);

CREATE INDEX IF NOT EXISTS idx_conversations_create_time ON conversations(create_time);
CREATE INDEX IF NOT EXISTS idx_conversations_update_time ON conversations(update_time);
CREATE INDEX IF NOT EXISTS idx_messages_conversation_turn ON messages(conversation_id, turn_index);
CREATE INDEX IF NOT EXISTS idx_jobs_kind_status ON jobs(kind, status);
CREATE INDEX IF NOT EXISTS idx_embedding_chunks_model ON conversation_embedding_chunks(embedding_model, embedding_dimensions);
"#;
