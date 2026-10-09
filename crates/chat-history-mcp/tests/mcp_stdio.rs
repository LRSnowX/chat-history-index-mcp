use std::{fs, io::Write, process::Stdio};

use chat_history_core::{
    ChatGptSyncState, ChatGptThreadListSnapshot, DataHome, ImportMode, ImportOptions, IndexService,
    MemoryCandidateInput, MemoryCandidatePayload, MemoryCompilationBatch, MemoryItem, MemoryKind,
    MemoryScope, MemoryStatus,
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

#[tokio::test]
async fn historical_restore_proof_is_unverified_on_mcp_until_complete_live_replay()
-> anyhow::Result<()> {
    use serde_json::json;
    let temp = TempDir::new()?;
    let home = DataHome::new(temp.path().join("data"));
    let service = IndexService::new(home.clone(), None);
    let id = "synthetic-mcp-history-restore";
    let input: chat_history_core::ChatGptBridgeTranscript = serde_json::from_value(
        json!({"thread_id":id,"title":"Arcos synthetic history","update_time":20.0,"pages":[{"has_more":false,"messages":[{"message_id":"d","role":"assistant","text":"Arcos synthetic continuation"},{"message_id":"c","role":"user","text":"Arcos synthetic continuation"}]}]}),
    )?;
    service.import_normalized(vec![input.into_normalized()?], None)?;
    let source = temp.path().join("export");
    fs::create_dir(&source)?;
    fs::write(source.join("export_manifest.json"), br#"{"version":1}"#)?;
    let mut mapping = serde_json::Map::new();
    mapping.insert(
        "root".into(),
        json!({"id":"root","parent":null,"children":["a"],"message":null}),
    );
    let ids = ["a", "b", "c", "d"];
    for (i, id) in ids.iter().enumerate() {
        mapping.insert((*id).into(),json!({"id":id,"parent":if i==0{"root"}else{ids[i-1]},"children":if i==3{vec![]}else{vec![ids[i+1]]},"message":{"id":id,"author":{"role":if i%2==0{"user"}else{"assistant"}},"content":{"content_type":"text","parts":["Arcos synthetic continuation"]}}}));
    }
    fs::write(
        source.join("conversations-000.json"),
        serde_json::to_vec(
            &json!([{"id":id,"title":"Arcos synthetic history","update_time":20.0,"current_node":"d","mapping":mapping}]),
        )?,
    )?;
    let observation = json!({"thread_id":id,"kind":"chatgpt","title":"Arcos synthetic history","status":"idle","update_time":20.0,"observed_at":30.0});
    let mut state = ChatGptSyncState::default();
    state.plan_recent(serde_json::from_value(
        json!({"requested_limit":50,"threads":[observation.clone()]}),
    )?)?;
    state.mark_imported_at(id, Some(20.0));
    state.save(&home)?;
    let plan = service.history_restore_plan(&[source], "Arcos")?;
    service.apply_history_restore(&plan, id)?;
    let transport = TokioChildProcess::builder(
        tokio::process::Command::new(env!("CARGO_BIN_EXE_chat-history-mcp")).configure(|cmd| {
            cmd.env("CHAT_HISTORY_DATA_HOME", home.root())
                .env("CHAT_HISTORY_EMBEDDING_PROVIDER", "hashed-v1")
                .stderr(Stdio::inherit());
        }),
    )
    .spawn()?
    .0;
    let client = ().serve(transport).await?;
    let tools = client.list_all_tools().await?;
    assert!(!tools.iter().any(|t| t.name.contains("history_restore")));
    for expected in ["unverified", "verified"] {
        if expected == "verified" {
            let detail = service.get_conversation(id, true)?.unwrap();
            let request: chat_history_core::continuation::ContinuationImport =
                serde_json::from_value(
                    json!({"baseline":service.continuation_baseline(id)?,"provider_before":observation,"provider_after":observation,"transcript":{"thread_id":id,"title":"Arcos synthetic history","update_time":20.0,"pages":[{"has_more":false,"provider_revision":20.0,"messages":detail.messages.iter().rev().map(|m|json!({"message_id":m.message_id,"role":m.role,"text":m.normalized_text,"stable_identity":true})).collect::<Vec<_>>()}]}}),
                )?;
            service.import_verified_continuation(&request)?;
        }
        let thread = client
            .call_tool(
                CallToolRequestParams::new("memory_get_thread").with_arguments(
                    serde_json::from_value(json!({"conversation_id":id,"message_limit":16}))?,
                ),
            )
            .await?
            .structured_content
            .unwrap();
        assert_eq!(thread["total_messages"], 4);
        assert_eq!(thread["thread"]["source_health"]["state"], "aligned");
        assert_eq!(thread["thread"]["continuation_proof"]["state"], expected);
        let context = client
            .call_tool(
                CallToolRequestParams::new("memory_project_context").with_arguments(
                    serde_json::from_value(
                        json!({"project":"Arcos","relevant_limit":0,"recent_limit":3}),
                    )?,
                ),
            )
            .await?
            .structured_content
            .unwrap();
        assert_eq!(
            context["continuations"][0]["continuation_proof"]["state"],
            expected
        );
        assert_eq!(
            context["continuations"][0]["source_health"]["state"],
            "aligned"
        );
    }
    client.cancel().await?;
    Ok(())
}

#[tokio::test]
async fn arcos_source_integrity_is_additive_on_real_mcp_reads() -> anyhow::Result<()> {
    let temp = TempDir::new()?;
    let home = DataHome::new(temp.path().join("data"));
    let service = IndexService::new(home.clone(), None);
    let id = "6ac4773c-b8e8-83e8-b954-9b0d49f08bb4";
    let normalized = serde_json::from_value(serde_json::json!({
        "source":"chatgpt", "source_conversation_id":id,
        "title":"⭐Arcos开发辅助-5", "create_time":10.0,"update_time":20.0,
        "raw":{"collector":"chatgpt-app-bridge-v1"},
        "messages": (0..10).map(|index| serde_json::json!({
            "message_id":format!("message-{index}"),"role":if index % 2 == 0 { "user" } else { "assistant" },
            "text":"Arcos Foundation B / fourth-live-fire readiness", "create_time":10.0 + f64::from(index)
        })).collect::<Vec<_>>()
    }))?;
    service.import_normalized(vec![normalized], None)?;
    let mut state = ChatGptSyncState::default();
    // Cursor intentionally higher: observation of a known mismatch still matters
    // even when discovery is not requeued. Do not manufacture later transcript text.
    state.seed_cursor(100.0)?;
    state.plan_recent(serde_json::from_value::<ChatGptThreadListSnapshot>(serde_json::json!({
        "requested_limit":50,"threads":[{"thread_id":id,"kind":"chatgpt","title":"Arcos","update_time":30.0,"status":"idle","observed_at":40.0}]
    }))?)?;
    state.save(&home)?;
    let transport = TokioChildProcess::builder(
        tokio::process::Command::new(env!("CARGO_BIN_EXE_chat-history-mcp")).configure(|cmd| {
            cmd.env("CHAT_HISTORY_DATA_HOME", home.root())
                .env("CHAT_HISTORY_EMBEDDING_PROVIDER", "hashed-v1")
                .stderr(Stdio::inherit());
        }),
    )
    .spawn()?
    .0;
    let client = ().serve(transport).await?;
    for expected in ["stale", "pending", "blocked", "aligned", "unknown"] {
        match expected {
            "pending" => {
                state.pending.insert(
                    id.to_string(),
                    chat_history_core::ChatGptPendingThread {
                        thread_id: id.to_string(),
                        title: "Arcos".to_string(),
                        create_time: Some(10.0),
                        update_time: 30.0,
                    },
                );
            }
            "blocked" => {
                state.mark_blocked(id, "newer transcript inaccessible");
            }
            "aligned" => {
                state.pending.clear();
                state.blocked.clear();
                state
                    .provider_observations
                    .get_mut(id)
                    .unwrap()
                    .provider_revision = Some(20.0);
            }
            "unknown" => {
                state.provider_observations.clear();
            }
            _ => {}
        }
        state.save(&home)?;
        let result = client
            .call_tool(
                CallToolRequestParams::new("memory_get_thread").with_arguments(
                    serde_json::from_value(
                        serde_json::json!({"conversation_id":id,"message_limit":16}),
                    )?,
                ),
            )
            .await?;
        let wire = result
            .structured_content
            .expect("thread structured response");
        assert_eq!(wire["total_messages"], 10);
        assert_eq!(wire["truncated"], false);
        assert_eq!(wire["thread"]["source_health"]["state"], expected);
        assert_eq!(
            wire["thread"]["continuation_proof"]["state"],
            if expected == "aligned" {
                "verified"
            } else {
                "unverified"
            }
        );
        assert_eq!(
            wire["thread"]["continuation_proof"]["final_message_id"],
            "message-9"
        );
        assert_eq!(wire["thread"]["continuation_proof"]["total_messages"], 10);
        if expected == "blocked" {
            assert_eq!(
                wire["thread"]["source_health"]["reason"],
                "newer transcript inaccessible"
            );
        }
        let context = client
            .call_tool(
                CallToolRequestParams::new("memory_project_context").with_arguments(
                    serde_json::from_value(
                        serde_json::json!({"project":"Arcos","relevant_limit":0,"recent_limit":3}),
                    )?,
                ),
            )
            .await?;
        let packet = context
            .structured_content
            .expect("context structured response");
        assert_eq!(
            packet["continuations"][0]["source_health"],
            wire["thread"]["source_health"]
        );
        assert_eq!(
            packet["continuations"][0]["continuation_proof"],
            wire["thread"]["continuation_proof"]
        );
        if expected == "aligned" {
            let older = client.call_tool(CallToolRequestParams::new("memory_get_thread").with_arguments(serde_json::from_value(
                serde_json::json!({"conversation_id":id,"message_offset":0,"message_limit":2})
            )?)).await?.structured_content.unwrap();
            assert_eq!(older["thread"]["continuation_proof"]["state"], "unverified");
            let tail = client
                .call_tool(
                    CallToolRequestParams::new("memory_get_thread").with_arguments(
                        serde_json::from_value(
                            serde_json::json!({"conversation_id":id,"tail":true,"message_limit":2}),
                        )?,
                    ),
                )
                .await?
                .structured_content
                .unwrap();
            assert_eq!(tail["message_offset"], 8);
            assert_eq!(tail["thread"]["continuation_proof"]["state"], "verified");
        }
    }
    // Malformed durable telemetry fails closed without losing indexed evidence.
    fs::write(
        chat_history_core::chatgpt::sync_state_path(&home),
        b"{partial",
    )?;
    let result = client
        .call_tool(
            CallToolRequestParams::new("memory_get_thread").with_arguments(serde_json::from_value(
                serde_json::json!({"conversation_id":id}),
            )?),
        )
        .await?;
    assert_eq!(
        result.structured_content.unwrap()["thread"]["source_health"]["state"],
        "unknown"
    );
    client.cancel().await?;
    Ok(())
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
    service.put_memory_item(&MemoryItem {
        memory_id: "global-collaboration-preference".to_string(),
        scope: MemoryScope::Global,
        kind: MemoryKind::Preference,
        key: "collaboration_style".to_string(),
        value: serde_json::json!({"text": "preserve upstream compatibility"}),
        status: MemoryStatus::Active,
        importance: 100,
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
    let compiler_input = service
        .prepare_memory_compilation("Rust", "conv-rust-index", 8)?
        .expect("fixture conversation is compilable");
    let mut candidates = (0..14)
        .map(|index| MemoryCandidateInput {
            candidate_id: format!("pending-{index:02}"),
            payload: MemoryCandidatePayload::Add {
                memory_id: format!("pending-memory-{index:02}"),
                kind: MemoryKind::State,
                key: format!("pending_key_{index:02}"),
                value: serde_json::json!({"text": format!("pending proposal {index:02}")}),
                importance: 70,
                confidence: 0.8,
                valid_from: Some(1_800_000_100.0),
                valid_until: None,
                last_verified_at: Some(1_800_000_100.0),
            },
            rationale: format!("private rationale {index:02}"),
            evidence: Vec::new(),
        })
        .collect::<Vec<_>>();
    candidates.push(MemoryCandidateInput {
        candidate_id: "pending-invalid".to_string(),
        payload: MemoryCandidatePayload::Add {
            memory_id: "pending-memory-invalid".to_string(),
            kind: MemoryKind::State,
            key: "current_goal".to_string(),
            value: serde_json::json!({"text": "conflicts with active memory"}),
            importance: 70,
            confidence: 0.8,
            valid_from: Some(1_800_000_100.0),
            valid_until: None,
            last_verified_at: Some(1_800_000_100.0),
        },
        rationale: "private invalid rationale".to_string(),
        evidence: Vec::new(),
    });
    service.stage_memory_compilation(&MemoryCompilationBatch {
        project: "Rust".to_string(),
        conversation_id: compiler_input.conversation_id,
        source_snapshot_id: compiler_input.source_snapshot_id,
        through_turn_index: compiler_input.through_turn_index,
        through_message_id: compiler_input.through_message_id,
        compiler_version: "memory-compiler-v1".to_string(),
        model_label: Some("private-model-label".to_string()),
        created_at: 1_800_000_100.0,
        candidates,
    })?;

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
        serde_json::json!({"query": "preserve migration", "project": "Rust", "limit": 5}),
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
        "query_ranked_working_memory_then_hybrid_evidence"
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
    assert_eq!(
        memory_search_json["authority_policy"]["current_state_priority"][0],
        "live_repository_or_authoritative_project_files"
    );
    assert_eq!(
        memory_search_json["authority_policy"]["current_state_priority"][1],
        "active_project_working_memory"
    );
    assert_eq!(
        memory_search_json["authority_policy"]["current_state_priority"][2],
        "historical_conversation_evidence"
    );
    assert!(
        memory_search_json["authority_policy"]["historical_question_rule"]
            .as_str()
            .is_some_and(|value| value.contains("does not retroactively overwrite history"))
    );

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
    assert_eq!(
        global_memory_search_json["authority_policy"]["current_state_priority"][0],
        "live_repository_or_authoritative_project_files"
    );
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
    assert_eq!(
        memory_tail_json["thread"]["source_health"]["state"],
        "unknown"
    );

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
    assert_eq!(
        project_context_json["continuations"][0]["source_health"]["state"],
        "unknown"
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
    assert_eq!(
        project_context_json["working_memory"]["verification"][0]["memory_id"],
        "rust-current-goal"
    );
    assert_eq!(
        project_context_json["working_memory"]["verification"][0]["state"],
        "current_by_evidence"
    );
    assert_eq!(
        project_context_json["working_memory"]["verification"][0]["evidence_strength"],
        "none"
    );
    assert_eq!(
        project_context_json["collaboration_memory"]["items"][0]["memory_id"],
        "global-collaboration-preference"
    );
    assert_eq!(
        project_context_json["collaboration_memory"]["items"][0]["value"]["text"],
        "preserve upstream compatibility"
    );
    assert_eq!(project_context_json["pending_memory"]["project"], "Rust");
    assert_eq!(
        project_context_json["pending_memory"]["items"]
            .as_array()
            .map(Vec::len),
        Some(8)
    );
    assert_eq!(
        project_context_json["pending_memory"]["items"][0]["candidate_id"],
        "pending-13"
    );
    assert_eq!(
        project_context_json["pending_memory"]["revalidation_excluded_count"],
        1
    );
    assert!(
        project_context_json["pending_memory"]["generated_at"]
            .as_f64()
            .is_some()
    );
    let pending_item = &project_context_json["pending_memory"]["items"][0];
    for field in [
        "candidate_id",
        "operation",
        "payload",
        "created_at",
        "conversation_id",
        "source_snapshot_id",
        "through_turn_index",
    ] {
        assert!(pending_item.get(field).is_some(), "missing {field}");
    }
    for field in ["rationale", "model_label", "decision_reason", "reviews"] {
        assert!(pending_item.get(field).is_none(), "unexpected {field}");
    }
    assert_eq!(
        project_context_json["authority_policy"]["current_state_priority"],
        serde_json::json!([
            "live_repository_or_authoritative_project_files",
            "active_project_working_memory",
            "pending_memory",
            "raw_conversation_continuations"
        ])
    );
    assert!(
        project_context_json["authority_policy"]["pending_memory_rule"]
            .as_str()
            .is_some_and(|rule| rule.contains("untrusted proposals"))
    );

    let args: serde_json::Map<String, serde_json::Value> =
        serde_json::from_value(serde_json::json!({
            "project": "Rust",
            "relevant_limit": 0,
            "recent_limit": 1,
            "pending_limit": 99
        }))?;
    let bounded_high = client
        .call_tool(CallToolRequestParams::new("memory_project_context").with_arguments(args))
        .await?;
    assert_eq!(
        bounded_high.structured_content.as_ref().unwrap()["pending_memory"]["items"]
            .as_array()
            .map(Vec::len),
        Some(12)
    );

    let args: serde_json::Map<String, serde_json::Value> =
        serde_json::from_value(serde_json::json!({
            "project": "Rust",
            "relevant_limit": 0,
            "recent_limit": 1,
            "pending_limit": 0
        }))?;
    let bounded_low = client
        .call_tool(CallToolRequestParams::new("memory_project_context").with_arguments(args))
        .await?;
    assert_eq!(
        bounded_low.structured_content.as_ref().unwrap()["pending_memory"]["items"]
            .as_array()
            .map(Vec::len),
        Some(0)
    );

    let args: serde_json::Map<String, serde_json::Value> =
        serde_json::from_value(serde_json::json!({
            "project": "Rust",
            "stale_after_days": 30
        }))?;
    let memory_health = client
        .call_tool(CallToolRequestParams::new("memory_health").with_arguments(args))
        .await?;
    assert_eq!(memory_health.is_error, Some(false));
    let memory_health_json = memory_health
        .structured_content
        .as_ref()
        .expect("memory health structured");
    assert_eq!(memory_health_json["project"], "Rust");
    assert_eq!(memory_health_json["stale_after_days"], 30);
    assert_eq!(memory_health_json["memory_items"]["active"], 13);
    assert_eq!(memory_health_json["candidates"]["pending"], 15);
    assert_eq!(memory_health_json["pending_revalidation_problem_count"], 1);
    assert_eq!(memory_health_json["checkpoint_count"], 1);

    let args: serde_json::Map<String, serde_json::Value> =
        serde_json::from_value(serde_json::json!({
            "project": "Rust",
            "max_conversations": 3,
            "max_messages": 8
        }))?;
    let bootstrap_plan = client
        .call_tool(CallToolRequestParams::new("memory_bootstrap_plan").with_arguments(args))
        .await?;
    assert_eq!(bootstrap_plan.is_error, Some(false));
    let bootstrap_plan_json = bootstrap_plan
        .structured_content
        .as_ref()
        .expect("memory bootstrap plan structured");
    assert_eq!(bootstrap_plan_json["project"], "Rust");
    assert_eq!(bootstrap_plan_json["bootstrap_required"], false);
    assert_eq!(bootstrap_plan_json["estimated_model_attempts"], 0);

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
