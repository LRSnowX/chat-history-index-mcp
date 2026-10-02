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

CREATE TABLE IF NOT EXISTS conversation_snapshots (
  snapshot_id TEXT PRIMARY KEY,
  conversation_id TEXT NOT NULL,
  archive_id INTEGER NOT NULL REFERENCES archives(id) ON DELETE CASCADE,
  archive_member TEXT NOT NULL,
  source_member TEXT NOT NULL,
  title TEXT NOT NULL,
  create_time REAL,
  update_time REAL,
  default_model_slug TEXT,
  message_count INTEGER NOT NULL,
  user_message_count INTEGER NOT NULL,
  assistant_message_count INTEGER NOT NULL,
  transcript_text TEXT NOT NULL,
  raw_conversation_zstd BLOB NOT NULL,
  raw_json_sha256_hex TEXT NOT NULL,
  source TEXT NOT NULL,
  source_instance TEXT,
  source_conversation_id TEXT NOT NULL,
  source_url TEXT,
  source_path TEXT,
  parent_conversation_id TEXT,
  selection_status TEXT NOT NULL CHECK(selection_status IN (
    'candidate',
    'canonical',
    'superseded',
    'rejected_lower_quality'
  )),
  selection_reason TEXT,
  captured_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE IF NOT EXISTS conversation_snapshot_messages (
  snapshot_id TEXT NOT NULL REFERENCES conversation_snapshots(snapshot_id) ON DELETE CASCADE,
  message_id TEXT NOT NULL,
  role TEXT NOT NULL,
  create_time REAL,
  turn_index INTEGER NOT NULL,
  normalized_text TEXT NOT NULL,
  raw_message_json TEXT NOT NULL,
  PRIMARY KEY(snapshot_id, message_id)
);

CREATE TABLE IF NOT EXISTS memory_items (
  memory_id TEXT PRIMARY KEY,
  scope_type TEXT NOT NULL CHECK(scope_type IN ('global', 'project')),
  scope_id TEXT NOT NULL,
  kind TEXT NOT NULL CHECK(kind IN (
    'invariant',
    'preference',
    'decision',
    'state',
    'blocker',
    'task',
    'result',
    'hypothesis',
    'artifact_reference'
  )),
  memory_key TEXT NOT NULL,
  value_json TEXT NOT NULL,
  status TEXT NOT NULL CHECK(status IN ('active', 'resolved', 'superseded', 'archived')),
  importance INTEGER NOT NULL CHECK(importance BETWEEN 0 AND 100),
  confidence REAL NOT NULL CHECK(confidence >= 0.0 AND confidence <= 1.0),
  valid_from REAL,
  valid_until REAL,
  supersedes_memory_id TEXT REFERENCES memory_items(memory_id),
  created_at REAL NOT NULL,
  updated_at REAL NOT NULL,
  last_verified_at REAL,
  CHECK(
    (scope_type = 'global' AND scope_id = '')
    OR
    (scope_type = 'project' AND length(trim(scope_id)) > 0)
  )
);

CREATE TABLE IF NOT EXISTS memory_evidence (
  memory_id TEXT NOT NULL REFERENCES memory_items(memory_id) ON DELETE CASCADE,
  evidence_kind TEXT NOT NULL CHECK(evidence_kind IN (
    'user_statement',
    'conversation_turn',
    'document',
    'git_commit',
    'repository_state',
    'devspace_result'
  )),
  evidence_ref TEXT NOT NULL,
  detail_json TEXT NOT NULL DEFAULT '{}',
  created_at REAL NOT NULL,
  PRIMARY KEY(memory_id, evidence_kind, evidence_ref)
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
CREATE INDEX IF NOT EXISTS idx_conversation_snapshots_conversation
  ON conversation_snapshots(conversation_id, captured_at);
CREATE UNIQUE INDEX IF NOT EXISTS idx_conversation_snapshots_canonical
  ON conversation_snapshots(conversation_id)
  WHERE selection_status = 'canonical';
CREATE INDEX IF NOT EXISTS idx_conversation_snapshot_messages_turn
  ON conversation_snapshot_messages(snapshot_id, turn_index);
CREATE INDEX IF NOT EXISTS idx_memory_items_scope_status ON memory_items(scope_type, scope_id, status);
CREATE INDEX IF NOT EXISTS idx_memory_items_scope_kind ON memory_items(scope_type, scope_id, kind);
CREATE INDEX IF NOT EXISTS idx_memory_items_key ON memory_items(memory_key);
CREATE UNIQUE INDEX IF NOT EXISTS idx_memory_items_active_key
  ON memory_items(scope_type, scope_id, memory_key)
  WHERE status = 'active';
CREATE INDEX IF NOT EXISTS idx_memory_evidence_memory_id ON memory_evidence(memory_id);
"#;
