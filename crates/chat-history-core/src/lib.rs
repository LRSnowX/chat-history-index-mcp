pub mod antigravity;
pub mod archive;
pub mod chatgpt;
pub mod codex;
pub mod data_home;
pub mod db;
pub mod embedding;
pub mod error;
pub mod gemini;
pub mod ingest;
pub mod memory;
pub mod memory_compile;
pub mod memory_compiler;
pub mod models;
pub mod openai;
pub mod ranking;
pub mod search;
pub mod sql;

pub use chatgpt::{
    ChatGptBlockedThread, ChatGptBridgeMessage, ChatGptBridgePage, ChatGptBridgeThread,
    ChatGptBridgeTranscript, ChatGptDiscoveryPlan, ChatGptPendingThread, ChatGptSyncState,
    ChatGptThreadListSnapshot,
};
pub use data_home::{DataHome, ImportMode, ManagedPaths};
pub use ingest::{ImportOptions, ImportReport, IndexService, decode_embedding, encode_embedding};
pub use memory::{
    MemoryEvidence, MemoryEvidenceKind, MemoryItem, MemoryKind, MemoryScope, MemoryStatus,
    ProjectWorkingMemory,
};
pub use memory_compile::{
    MemoryCandidate, MemoryCandidateDecision, MemoryCandidateInput, MemoryCandidateOperation,
    MemoryCandidatePayload, MemoryCandidateStatus, MemoryCompilationBatch,
    MemoryCompilationStageResult, MemoryCompileCheckpoint,
};
pub use memory_compiler::{
    DEFAULT_MEMORY_COMPILER_MESSAGES, MAX_MEMORY_COMPILER_MESSAGES, MEMORY_COMPILER_VERSION,
    MemoryCompilerInput, MemoryCompilerMessage, MemoryCompilerPendingCandidate,
    MemoryCompilerRunResult,
};
pub use models::{
    ConversationDetail, ConversationRecord, DatabaseHealth, IndexStats, JobKind, JobStatus,
    NormalizedConversation, NormalizedMessage, RestoreReport, SearchMode, SearchOptions,
    SearchResult, SourceHealth, SummaryRecord,
};
pub use openai::MemoryModelClient;
