pub mod archive;
pub mod codex;
pub mod data_home;
pub mod db;
pub mod error;
pub mod ingest;
pub mod models;
pub mod openai;
pub mod ranking;
pub mod search;
pub mod sql;

pub use data_home::{DataHome, ImportMode, ManagedPaths};
pub use ingest::{ImportOptions, ImportReport, IndexService, decode_embedding, encode_embedding};
pub use models::{
    ConversationDetail, ConversationRecord, DatabaseHealth, IndexStats, JobKind, JobStatus,
    NormalizedConversation, NormalizedMessage, RestoreReport, SearchMode, SearchOptions,
    SearchResult, SourceHealth, SummaryRecord,
};
