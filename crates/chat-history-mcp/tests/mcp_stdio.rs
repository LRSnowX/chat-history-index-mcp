use std::{fs, io::Write, process::Stdio};

use chat_history_core::{
    DataHome, ImportMode, ImportOptions, IndexService, MemoryItem, MemoryKind, MemoryScope,
    MemoryStatus,
};
use rmcp::{
    ServiceExt,
    model::CallToolRequestParams,
    transport::{ConfigureCommandExt, TokioChildProcess},
};
use tempfile::TempDir;
use zip::write::SimpleFileOptions;

fn fixture_text(name: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../chat-history-core/tests/fixtures")
        .join(name);
    fs::read_to_string(path).expect("fixture text")
}

fn build_fixture_export(tempdir: &TempDir) -> anyhow::Result<std::path::PathBuf> {
    let nested_zip_path = tempdir.path().join("nested-conversations.zip");
    {
        let file = fs::File::create(&nested_zip_path)?;
        let mut writer = zip::ZipWriter::new(file);
        let options = SimpleFileOptions::default();
        writer.start_file("conversations-000.json", options)?;
        writer.write_all(fixture_text("conversations-000.json").as_bytes())?;
        writer.start_file(
            "690e4893-d784-8327-a208-ee13aca9598b/audio/file_00000000abcdef1234567890.wav",
            options,
        )?;
        writer.write_all(b"fixture-audio")?;
        writer.finish()?;
    }

    let outer_zip_path = tempdir.path().join("openai-export.zip");
    {
        let file = fs::File::create(&outer_zip_path)?;
        let mut writer = zip::ZipWriter::new(file);
        let options = SimpleFileOptions::default();
        writer.start_file(
            "User Online Activity/Conversations__fixture-chatgpt-0001-part-0001.zip",
            options,
        )?;
        writer.write_all(&fs::read(&nested_zip_path)?)?;
        writer.finish()?;
    }
    Ok(outer_zip_path)
}

#[tokio::test]
async fn serves_mcp_tools_over_stdio() -> anyhow::Result<()> {
    let tempdir = TempDir::new()?;
    let export_zip = build_fixture_export(&tempdir)?;
    let data_home = tempdir.path().join("managed-data");
    let service = IndexService::new(DataHome::new(data_home.clone()), None);
    service
        .import_archive(ImportOptions {
            source_archive: export_zip,
            mode: ImportMode::Copy,
            run_api_jobs: false,
            force_summaries: false,
            force_embeddings: false,
        })
        .await?;
    service.put_memory_item(&MemoryItem {
        memory_id: "rust-current-goal".to_string(),
        scope: MemoryScope::Project {
            project: "Rust".to_string(),
        },
        kind: MemoryKind::State,
        key: "current_goal".to_string(),
        value: serde_json::json!({"text": "preserve migration history"}),
        status: MemoryStatus::Active,
        importance: 95,
        confidence: 1.0,
        valid_from: Some(1_800_000_000.0),
        valid_until: None,
        supersedes_memory_id: None,
        created_at: 1_800_000_000.0,
        updated_at: 1_800_000_000.0,
        last_verified_at: Some(1_800_000_000.0),
        evidence: Vec::new(),
    })?;
    for index in 0..12 {
        service.put_memory_item(&MemoryItem {
            memory_id: format!("rust-secondary-{index}"),
            scope: MemoryScope::Project {
                project: "Rust".to_string(),
            },
            kind: MemoryKind::State,
            key: format!("secondary_{index}"),
            value: serde_json::json!({"text": format!("secondary memory {index}")}),
            status: MemoryStatus::Active,
            importance: 10 + index,
            confidence: 1.0,
            valid_from: Some(1_800_000_001.0 + f64::from(index)),
            valid_until: None,
            supersedes_memory_id: None,
            created_at: 1_800_000_001.0 + f64::from(index),
            updated_at: 1_800_000_001.0 + f64::from(index),
            last_verified_at: Some(1_800_000_001.0 + f64::from(index)),
            evidence: Vec::new(),
        })?;
    }

    let transport = TokioChildProcess::builder(
        tokio::process::Command::new(env!("CARGO_BIN_EXE_chat-history-mcp")).configure(|cmd| {
            cmd.env("CHAT_HISTORY_DATA_HOME", &data_home)
                .env("CHAT_HISTORY_EMBEDDING_PROVIDER", "hashed-v1")
                .stderr(Stdio::inherit());
        }),
    )
    .spawn()?
    .0;

    let client = ().serve(transport).await?;
    let stats = client
        .call_tool(CallToolRequestParams::new("index_stats"))
        .await?;
    assert_eq!(stats.is_error, Some(false));

    let args: serde_json::Map<String, serde_json::Value> = serde_json::from_value(
        serde_json::json!({"query": "Rust SQLite", "mode": "fts", "limit": 5}),
    )?;
    let results = client
        .call_tool(CallToolRequestParams::new("search_conversations").with_arguments(args))
        .await?;
    assert_eq!(results.is_error, Some(false));

    let args: serde_json::Map<String, serde_json::Value> = serde_json::from_value(
        serde_json::json!({"query": "Rust SQLite", "project": "Rust", "limit": 5}),
    )?;
    let memory_search = client
        .call_tool(CallToolRequestParams::new("memory_search").with_arguments(args))
        .await?;
    assert_eq!(memory_search.is_error, Some(false));
    let memory_search_json = memory_search
        .structured_content
        .as_ref()
        .expect("memory search structured");
    assert_eq!(
        memory_search_json["retrieval_mode"],
        "working_memory_then_hybrid_evidence"
    );
    assert_eq!(memory_search_json["working_memory"]["project"], "Rust");
    assert_eq!(
        memory_search_json["working_memory"]["items"][0]["memory_id"],
        "rust-current-goal"
    );
    assert_eq!(
        memory_search_json["working_memory"]["items"]
            .as_array()
            .map(Vec::len),
        Some(12)
    );
    assert_eq!(memory_search_json["working_memory_truncated"], true);

    let args: serde_json::Map<String, serde_json::Value> =
        serde_json::from_value(serde_json::json!({"query": "Rust SQLite", "limit": 5}))?;
    let global_memory_search = client
        .call_tool(CallToolRequestParams::new("memory_search").with_arguments(args))
        .await?;
    assert_eq!(global_memory_search.is_error, Some(false));
    let global_memory_search_json = global_memory_search
        .structured_content
        .as_ref()
        .expect("global memory search structured");
    assert_eq!(global_memory_search_json["retrieval_mode"], "hybrid");
    assert!(global_memory_search_json["working_memory"].is_null());
    assert_eq!(global_memory_search_json["working_memory_truncated"], false);

    let args: serde_json::Map<String, serde_json::Value> =
        serde_json::from_value(serde_json::json!({"project": "Rust", "limit": 5}))?;
    let memory_recent = client
        .call_tool(CallToolRequestParams::new("memory_recent").with_arguments(args))
        .await?;
    assert_eq!(memory_recent.is_error, Some(false));

    let args: serde_json::Map<String, serde_json::Value> = serde_json::from_value(
        serde_json::json!({"conversation_id": "conv-rust-index", "message_limit": 5}),
    )?;
    let memory_thread = client
        .call_tool(CallToolRequestParams::new("memory_get_thread").with_arguments(args))
        .await?;
    assert_eq!(memory_thread.is_error, Some(false));

    let args: serde_json::Map<String, serde_json::Value> =
        serde_json::from_value(serde_json::json!({
            "conversation_id": "conv-rust-index",
            "message_limit": 1,
            "tail": true
        }))?;
    let memory_tail = client
        .call_tool(CallToolRequestParams::new("memory_get_thread").with_arguments(args))
        .await?;
    assert_eq!(memory_tail.is_error, Some(false));
    let memory_tail_json = memory_tail
        .structured_content
        .as_ref()
        .expect("tail structured");
    assert_eq!(memory_tail_json["message_offset"], 1);
    assert_eq!(memory_tail_json["returned_messages"], 1);
    assert_eq!(memory_tail_json["thread"]["messages"][0]["turn_index"], 1);

    let args: serde_json::Map<String, serde_json::Value> =
        serde_json::from_value(serde_json::json!({
            "project": "Rust",
            "query": "SQLite",
            "relevant_limit": 0,
            "recent_limit": 3
        }))?;
    let project_context = client
        .call_tool(CallToolRequestParams::new("memory_project_context").with_arguments(args))
        .await?;
    assert_eq!(project_context.is_error, Some(false));
    let project_context_json = project_context
        .structured_content
        .as_ref()
        .expect("project context structured");
    assert_eq!(
        project_context_json["continuation"]["conversation_id"],
        "conv-rust-index"
    );
    assert_eq!(project_context_json["continuation"]["returned_messages"], 2);
    assert_eq!(
        project_context_json["continuations"]
            .as_array()
            .map(Vec::len),
        Some(1)
    );
    assert_eq!(
        project_context_json["continuations"][0]["conversation_id"],
        "conv-rust-index"
    );
    assert_eq!(
        project_context_json["relevant"].as_array().map(Vec::len),
        Some(0)
    );
    assert_eq!(project_context_json["working_memory"]["project"], "Rust");
    assert_eq!(
        project_context_json["working_memory"]["items"][0]["memory_id"],
        "rust-current-goal"
    );
    assert_eq!(
        project_context_json["working_memory"]["items"][0]["value"]["text"],
        "preserve migration history"
    );

    let collector_state = client
        .call_tool(CallToolRequestParams::new("chatgpt_state"))
        .await?;
    assert_eq!(collector_state.is_error, Some(false));

    let args: serde_json::Map<String, serde_json::Value> =
        serde_json::from_value(serde_json::json!({
            "snapshot": {
                "requested_limit": 50,
                "threads": [
                    {
                        "thread_id": "bridge-thread",
                        "kind": "chatgpt",
                        "title": "Bridge collector test",
                        "create_time": 1800000000.0,
                        "update_time": 1800000100.0
                    },
                    {
                        "thread_id": "ignore-codex",
                        "kind": "codex",
                        "title": "Ignore",
                        "create_time": 1800000000.0,
                        "update_time": 1800000050.0
                    }
                ]
            }
        }))?;
    let plan = client
        .call_tool(CallToolRequestParams::new("chatgpt_plan_recent").with_arguments(args))
        .await?;
    assert_eq!(plan.is_error, Some(false));

    let args: serde_json::Map<String, serde_json::Value> =
        serde_json::from_value(serde_json::json!({
            "embed": false,
            "transcript": {
                "thread_id": "bridge-thread",
                "title": "Bridge collector test",
                "create_time": 1800000000.0,
                "update_time": 1800000100.0,
                "model": "fixture-model",
                "source_url": null,
                "pages": [
                    {
                        "request_cursor": null,
                        "next_cursor": null,
                        "has_more": false,
                        "messages": [
                            {
                                "message_id": "bridge-a1",
                                "role": "assistant",
                                "create_time": 1800000100.0,
                                "text": "Use the complete paged ChatGPT transcript.",
                                "raw": {}
                            },
                            {
                                "message_id": "bridge-u1",
                                "role": "user",
                                "create_time": 1800000000.0,
                                "text": "Can the collector preserve this history?",
                                "raw": {}
                            }
                        ]
                    }
                ]
            }
        }))?;
    let imported = client
        .call_tool(CallToolRequestParams::new("chatgpt_import_thread").with_arguments(args))
        .await?;
    assert_eq!(imported.is_error, Some(false));

    let args: serde_json::Map<String, serde_json::Value> = serde_json::from_value(
        serde_json::json!({"conversation_id": "bridge-thread", "include_raw": false}),
    )?;
    let imported_detail = client
        .call_tool(CallToolRequestParams::new("get_conversation").with_arguments(args))
        .await?;
    assert_eq!(imported_detail.is_error, Some(false));

    let seeded = client
        .call_tool(CallToolRequestParams::new("chatgpt_seed_from_index"))
        .await?;
    assert_eq!(seeded.is_error, Some(false));

    client.cancel().await?;
    Ok(())
}
