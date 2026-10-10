use chat_history_core::continuation::{ContinuationImport, continuation_proof};
use chat_history_core::history_restore::{Provenance, RestoreCode};
use chat_history_core::{
    ChatGptBridgeTranscript, ChatGptSyncState, DataHome, IndexService, MemoryCandidateDecision,
    MemoryCandidateInput, MemoryCandidatePayload, MemoryCompilationBatch, MemoryKind,
};
use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};
use zip::write::SimpleFileOptions;
const ID: &str = "synthetic-history";
const TITLE: &str = "MAIN LEMonX synthetic";
fn role(id: &str) -> &str {
    if id.as_bytes()[0] & 1 == 0 {
        "assistant"
    } else {
        "user"
    }
}
fn body(id: &str) -> String {
    if id == "b" {
        format!("  {}\n ", "PRIVATE_BODY_SENTINEL 🦀 ".repeat(1800))
    } else {
        format!("PRIVATE_BODY_SENTINEL {id}")
    }
}
fn export(ids: &[&str], revision: f64, statusless: bool) -> Value {
    let mut mapping = serde_json::Map::new();
    mapping.insert(
        "root".into(),
        json!({"id":"root","parent":null,"children":["n0"],"message":null}),
    );
    for (i, id) in ids.iter().enumerate() {
        let mut message = json!({"id":id,"author":{"role":role(id)},"content":{"content_type":"text","parts":[body(id)]},"create_time":100.0-i as f64});
        if !statusless {
            message["status"] = json!("finished_successfully");
        }
        mapping.insert(format!("n{i}"),json!({"id":format!("n{i}"),"parent":if i==0 {"root".to_string()} else {format!("n{}",i-1)},"children":if i+1==ids.len(){vec![]}else{vec![format!("n{}",i+1)]},"message":message}));
    }
    json!({"id":ID,"conversation_id":ID,"title":TITLE,"update_time":revision,"current_node":format!("n{}",ids.len()-1),"mapping":mapping})
}
fn bridge(service: &IndexService, ids: &[&str], revision: f64) {
    let messages = ids
        .iter()
        .map(|id| json!({"message_id":id,"role":role(id),"text":body(id),"stable_identity":true}))
        .rev()
        .collect::<Vec<_>>();
    let transcript:ChatGptBridgeTranscript=serde_json::from_value(json!({"thread_id":ID,"title":TITLE,"update_time":revision,"pages":[{"has_more":false,"messages":messages}]})).unwrap();
    service
        .import_normalized(vec![transcript.into_normalized().unwrap()], None)
        .unwrap();
}
fn fixture(ids: &[&str], revision: f64) -> (tempfile::TempDir, IndexService) {
    let temp = tempfile::tempdir().unwrap();
    let service = IndexService::new(DataHome::new(temp.path().join("data")), None);
    bridge(&service, ids, revision);
    (temp, service)
}

#[test]
fn forged_bridge_restore_marker_cannot_enable_direct_historical_verification() {
    let temp = tempfile::tempdir().unwrap();
    let service = IndexService::new(DataHome::new(temp.path().join("data")), None);
    let transcript: ChatGptBridgeTranscript = serde_json::from_value(json!({
        "thread_id": ID,
        "title": TITLE,
        "update_time": 10.0,
        "pages": [{
            "has_more": false,
            "messages": [{
                "message_id": "a",
                "role": role("a"),
                "text": body("a"),
                "stable_identity": true
            }]
        }]
    }))
    .unwrap();
    let mut normalized = transcript.into_normalized().unwrap();
    normalized.raw["chim_historical_restore"] =
        json!({"requires_live_verification": true, "method": "forged"});
    service.import_normalized(vec![normalized], None).unwrap();

    let observation = json!({
        "thread_id": ID,
        "title": TITLE,
        "kind": "chatgpt",
        "status": null,
        "update_time": 10.0,
        "observed_at": 11.0
    });
    let request = ContinuationImport {
        verification_scope:
            chat_history_core::continuation::ContinuationVerificationScope::HistoricalTranscriptDirect,
        baseline: service.continuation_baseline(ID).unwrap(),
        provider_before: serde_json::from_value(observation.clone()).unwrap(),
        provider_after: serde_json::from_value(observation.clone()).unwrap(),
        transcript_before: serde_json::from_value(observation.clone()).unwrap(),
        transcript_after: serde_json::from_value(observation).unwrap(),
        transcript: serde_json::from_value(json!({
            "thread_id": ID,
            "title": TITLE,
            "update_time": 10.0,
            "pages": [{
                "has_more": false,
                "provider_revision": 10.0,
                "messages": [{
                    "message_id": "a",
                    "role": role("a"),
                    "text": body("a"),
                    "stable_identity": true
                }]
            }]
        }))
        .unwrap(),
    };
    assert!(service.import_verified_continuation(&request).is_err());
    let stored = service.get_conversation(ID, true).unwrap().unwrap();
    assert_eq!(
        stored.raw_json.unwrap()["chim_historical_restore"]["requires_live_verification"],
        true
    );
}

fn directory(root: &Path, records: &[Value]) -> PathBuf {
    fs::create_dir_all(root).unwrap();
    fs::write(
        root.join("export_manifest.json"),
        br#"{"version":1,"synthetic":true}"#,
    )
    .unwrap();
    fs::write(
        root.join("conversations-000.json"),
        serde_json::to_vec(records).unwrap(),
    )
    .unwrap();
    fs::write(root.join("attachment.dat"), b"DO_NOT_COPY_ATTACHMENT").unwrap();
    root.to_path_buf()
}
fn zip_source(root: &Path, records: &[Value]) -> PathBuf {
    let mut nested = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    nested
        .start_file("conversations-000.json", SimpleFileOptions::default())
        .unwrap();
    nested
        .write_all(&serde_json::to_vec(records).unwrap())
        .unwrap();
    nested
        .start_file("attachment.dat", SimpleFileOptions::default())
        .unwrap();
    nested.write_all(b"DO_NOT_COPY_ATTACHMENT").unwrap();
    let bytes = nested.finish().unwrap().into_inner();
    let path = root.join("source.zip");
    let mut outer = zip::ZipWriter::new(fs::File::create(&path).unwrap());
    outer
        .start_file(
            "User Online Activity/Conversations__synthetic.zip",
            SimpleFileOptions::default(),
        )
        .unwrap();
    outer.write_all(&bytes).unwrap();
    outer.finish().unwrap();
    path
}
fn ids(service: &IndexService) -> Vec<String> {
    service
        .get_conversation(ID, true)
        .unwrap()
        .unwrap()
        .messages
        .into_iter()
        .map(|m| m.message_id)
        .collect()
}
fn snapshot(service: &IndexService) -> String {
    chat_history_core::db::open_database(&service.managed_db_path()).unwrap().query_row("SELECT snapshot_id FROM conversation_snapshots WHERE conversation_id=?1 AND selection_status='canonical'",[ID],|r|r.get(0)).unwrap()
}
#[test]
fn prefix_suffix_sparse_equal_older_newer_and_complementary_gap_restoration() {
    type MergeCase<'a> = (&'a [&'a str], &'a [&'a str], f64, f64, &'a [&'a str]);
    let cases: Vec<MergeCase<'_>> = vec![
        (
            &["a", "b"],
            &["a", "b", "c", "d"],
            10.0,
            20.0,
            &["a", "b", "c", "d"],
        ),
        (
            &["c", "d"],
            &["a", "b", "c", "d"],
            20.0,
            20.0,
            &["a", "b", "c", "d"],
        ),
        (
            &["b", "d"],
            &["a", "b", "c", "d", "e"],
            30.0,
            20.0,
            &["a", "b", "c", "d", "e"],
        ),
        (
            &["b", "d", "e", "f"],
            &["a", "b", "c", "d"],
            30.0,
            20.0,
            &["a", "b", "c", "d", "e", "f"],
        ),
        (
            &["a", "c", "d", "e"],
            &["a", "b", "c", "e", "f"],
            10.0,
            20.0,
            &["a", "b", "c", "d", "e", "f"],
        ),
        (&["b"], &["a", "b", "c"], 10.0, 20.0, &["a", "b", "c"]),
    ];
    for (i, (canonical, export_ids, cr, er, expected)) in cases.into_iter().enumerate() {
        let (temp, service) = fixture(canonical, cr);
        let raw = export(export_ids, er, i % 2 == 0);
        let source = if i % 2 == 0 {
            directory(&temp.path().join("export"), &[raw])
        } else {
            zip_source(temp.path(), &[raw])
        };
        let old = service.get_conversation(ID, true).unwrap().unwrap();
        let old_snapshot = snapshot(&service);
        let before_db = fs::read(service.managed_db_path()).unwrap();
        let before_sources = fs::read_dir(service.data_home().paths().sources_dir)
            .unwrap()
            .count();
        let plan = service.history_restore_plan(&[source], "MAIN").unwrap();
        assert_eq!(
            fs::read(service.managed_db_path()).unwrap(),
            before_db,
            "planner must not write DB"
        );
        assert_eq!(
            fs::read_dir(service.data_home().paths().sources_dir)
                .unwrap()
                .count(),
            before_sources
        );
        assert!(
            plan.entries[0].mergeable,
            "case {i}: {:?}",
            plan.entries[0].reason
        );
        assert!(
            !serde_json::to_string(&plan)
                .unwrap()
                .contains("PRIVATE_BODY_SENTINEL")
        );
        if i == 3 {
            assert_eq!(
                plan.entries[0].reason,
                RestoreCode::SafeRestorationWithLiveTail
            );
        }
        let report = service.apply_history_restore(&plan, ID).unwrap();
        assert_eq!(ids(&service), expected);
        let after = service.get_conversation(ID, true).unwrap().unwrap();
        assert_eq!(after.conversation.update_time, Some(cr.max(er)));
        for old in old.messages {
            let kept = after
                .messages
                .iter()
                .find(|m| m.message_id == old.message_id)
                .unwrap();
            assert_eq!(kept.normalized_text, old.normalized_text);
            assert_eq!(kept.raw_message_json, old.raw_message_json);
            assert_eq!(kept.create_time, old.create_time);
        }
        assert!(
            service
                .trusted_export_provenance_for_detail(ID, after.raw_json.as_ref())
                .unwrap()
        );
        assert!(
            !service
                .native_export_provenance_for_detail(ID, after.raw_json.as_ref())
                .unwrap()
        );
        assert!(service.continuation_baseline(ID).is_ok());
        let conn = chat_history_core::db::open_database(&service.managed_db_path()).unwrap();
        let status: String = conn
            .query_row(
                "SELECT selection_status FROM conversation_snapshots WHERE snapshot_id=?1",
                [old_snapshot],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(status, "superseded");
        let fts: String = conn
            .query_row(
                "SELECT transcript_text FROM conversation_fts WHERE conversation_id=?1",
                [ID],
                |r| r.get(0),
            )
            .unwrap();
        assert!(fts.contains(&body("b")));
        let mode:String=conn.query_row("SELECT a.import_mode FROM archives a JOIN conversations c ON c.archive_id=a.id WHERE c.conversation_id=?1",[ID],|r|r.get(0)).unwrap();
        assert_eq!(mode, "historical_restore");
        let mut bundle =
            zip::ZipArchive::new(fs::File::open(&report.archive_path).unwrap()).unwrap();
        assert!(bundle.by_name("evidence-format.json").is_ok());
        assert!(bundle.by_name("attachment.dat").is_err());
        assert!(
            !fs::read(report.archive_path)
                .unwrap()
                .windows(b"DO_NOT_COPY_ATTACHMENT".len())
                .any(|w| w == b"DO_NOT_COPY_ATTACHMENT")
        );
    }
}

#[test]
fn insufficient_reordered_ambiguous_duplicate_and_unsupported_sources_never_publish() {
    let cases: Vec<(&[&str], &[&str], RestoreCode)> = vec![
        (&["a", "b"], &["c", "d"], RestoreCode::InsufficientOverlap),
        (&["a", "b"], &["a", "c"], RestoreCode::InsufficientOverlap),
        (&["b", "a"], &["a", "b", "c"], RestoreCode::ReorderedOverlap),
        (
            &["a", "x", "d"],
            &["a", "b", "d"],
            RestoreCode::AmbiguousGap,
        ),
        (
            &["x", "a", "b"],
            &["y", "a", "b"],
            RestoreCode::AmbiguousGap,
        ),
    ];
    for (c, e, code) in cases {
        let (temp, service) = fixture(c, 10.0);
        let source = directory(&temp.path().join("export"), &[export(e, 20.0, true)]);
        let before = serde_json::to_value(service.get_conversation(ID, true).unwrap()).unwrap();
        let plan = service.history_restore_plan(&[source], "MAIN").unwrap();
        assert_eq!(plan.entries[0].reason, code);
        assert!(service.apply_history_restore(&plan, ID).is_err());
        assert_eq!(
            serde_json::to_value(service.get_conversation(ID, true).unwrap()).unwrap(),
            before
        );
    }
    for case in 0..5 {
        let (temp, service) = fixture(&["a", "b"], 10.0);
        let mut raw = export(&["a", "b", "c"], 20.0, true);
        match case {
            0 => raw["mapping"]["n2"]["message"]["id"] = json!("a"),
            1 => raw["mapping"]["n2"]["message"]["content"]["content_type"] = json!("audio"),
            2 => raw["id"] = json!("contradiction"),
            3 => raw["mapping"]["n0"]["message"]["status"] = json!("finished_successfully"),
            _ => {}
        };
        let records = if case == 4 {
            vec![raw.clone(), raw]
        } else {
            vec![raw]
        };
        let source = directory(&temp.path().join("export"), &records);
        let plan = service.history_restore_plan(&[source], "MAIN").unwrap();
        assert!(!plan.entries[0].mergeable);
    }
}

#[test]
fn source_baseline_and_entry_changes_invalidate_single_apply() {
    for case in 0..4 {
        let (temp, service) = fixture(&["a", "b"], 10.0);
        let source = directory(
            &temp.path().join("export"),
            &[export(&["a", "b", "c"], 20.0, true)],
        );
        let mut plan = service
            .history_restore_plan(std::slice::from_ref(&source), "MAIN")
            .unwrap();
        match case {
            0 => {
                let mut bytes = fs::read(source.join("conversations-000.json")).unwrap();
                bytes.push(b' ');
                fs::write(source.join("conversations-000.json"), bytes).unwrap();
            }
            1 => bridge(&service, &["a", "b", "x"], 30.0),
            2 => plan.entries[0].export_visible_count = Some(99),
            _ => {
                fs::write(source.join("conversations-001.json"), b"[]").unwrap();
            }
        };
        let before = serde_json::to_value(service.get_conversation(ID, true).unwrap()).unwrap();
        let error = service.apply_history_restore(&plan, ID).unwrap_err();
        assert_eq!(
            error.downcast_ref::<RestoreCode>().copied(),
            Some(if case == 1 {
                RestoreCode::BaselineChanged
            } else {
                RestoreCode::StalePlan
            })
        );
        assert!(!error.to_string().contains("PRIVATE_BODY"));
        assert_eq!(
            serde_json::to_value(service.get_conversation(ID, true).unwrap()).unwrap(),
            before
        );
    }
}

#[test]
fn freshest_compatible_multiple_sources_conflicts_and_missing_are_explicit() {
    let (temp, service) = fixture(&["a", "b"], 10.0);
    let older = directory(
        &temp.path().join("older"),
        &[export(&["a", "b"], 10.0, false)],
    );
    let newer = directory(
        &temp.path().join("newer"),
        &[export(&["a", "b", "c"], 20.0, true)],
    );
    let plan = service
        .history_restore_plan(&[older, newer.clone()], "MAIN")
        .unwrap();
    assert_eq!(plan.entries[0].chosen_source, Some(1));
    assert!(plan.entries[0].mergeable);
    let mut divergent = export(&["a", "b", "c"], 20.0, true);
    divergent["title"] = json!("same revision conflict");
    let conflict = directory(&temp.path().join("conflict"), &[divergent]);
    assert_eq!(
        service
            .history_restore_plan(&[newer, conflict], "MAIN")
            .unwrap()
            .entries[0]
            .reason,
        RestoreCode::SourceConflict
    );
    let missing = directory(&temp.path().join("missing"), &[]);
    assert_eq!(
        service
            .history_restore_plan(&[missing], "MAIN")
            .unwrap()
            .entries[0]
            .reason,
        RestoreCode::SourceMissing
    );
}

#[test]
fn offline_restore_gates_proof_and_only_complete_live_gb_resolves_it() {
    let (temp, service) = fixture(&["c", "d"], 20.0);
    let source = directory(
        &temp.path().join("export"),
        &[export(&["a", "b", "c", "d"], 20.0, true)],
    );
    let mut state = ChatGptSyncState::default();
    state.plan_recent(serde_json::from_value(json!({"requested_limit":50,"threads":[{"thread_id":ID,"title":TITLE,"kind":"chatgpt","status":"idle","update_time":20.0,"observed_at":21.0}]})).unwrap()).unwrap();
    state.mark_imported_at(ID, Some(20.0));
    state.save(service.data_home()).unwrap();
    let state_before = fs::read(chat_history_core::chatgpt::sync_state_path(
        service.data_home(),
    ))
    .unwrap();
    let plan = service.history_restore_plan(&[source], "MAIN").unwrap();
    service.apply_history_restore(&plan, ID).unwrap();
    assert_eq!(
        fs::read(chat_history_core::chatgpt::sync_state_path(
            service.data_home()
        ))
        .unwrap(),
        state_before
    );
    let restored = service.get_conversation(ID, true).unwrap().unwrap();
    assert_eq!(
        service.pending_history_restores(16).unwrap(),
        vec![chat_history_core::history_restore::PendingHistoryRestore {
            conversation_id: ID.to_string(),
            title: TITLE.to_string(),
            update_time: Some(20.0),
        }]
    );
    let health = state.source_health("chatgpt", ID, Some(20.0));
    assert_eq!(
        health.state,
        chat_history_core::ConversationSourceHealthState::Aligned
    );
    assert_eq!(
        continuation_proof(&restored, &health, 3, 1, true).state,
        "unverified"
    );
    // An ordinary import cannot erase the marker or bypass G-B verification.
    let input:ChatGptBridgeTranscript=serde_json::from_value(json!({"thread_id":ID,"title":TITLE,"update_time":20.0,"pages":[{"has_more":false,"messages":restored.messages.iter().rev().map(|m|json!({"message_id":m.message_id,"role":m.role,"text":m.normalized_text})).collect::<Vec<_>>()}]})).unwrap();
    assert!(
        service
            .import_normalized(vec![input.into_normalized().unwrap()], None)
            .is_err()
    );
    let observation = json!({"thread_id":ID,"title":TITLE,"kind":"chatgpt","status":"idle","update_time":20.0,"observed_at":21.0});
    let mut messages=restored.messages.iter().map(|m|json!({"message_id":m.message_id,"role":m.role,"text":if m.message_id=="b" {"truncated historical representation"}else{m.normalized_text.as_str()},"truncated":m.message_id=="b","stable_identity":true})).collect::<Vec<_>>();
    messages.reverse();
    let request=ContinuationImport{
        verification_scope: chat_history_core::continuation::ContinuationVerificationScope::DiscoveryAligned,
        baseline:service.continuation_baseline(ID).unwrap(),
        provider_before:serde_json::from_value(observation.clone()).unwrap(),
        provider_after:serde_json::from_value(observation.clone()).unwrap(),
        transcript_before:serde_json::from_value(observation.clone()).unwrap(),
        transcript_after:serde_json::from_value(observation).unwrap(),
        transcript:serde_json::from_value(json!({"thread_id":ID,"title":TITLE,"update_time":20.0,"pages":[{"has_more":false,"provider_revision":20.0,"messages":messages}]})).unwrap()
    };
    let mut invalid = request.clone();
    invalid.transcript.pages[0].has_more = true;
    assert!(service.import_verified_continuation(&invalid).is_err());
    assert_eq!(
        service
            .get_conversation(ID, true)
            .unwrap()
            .unwrap()
            .raw_json
            .unwrap()["chim_historical_restore"]["requires_live_verification"],
        true
    );
    let mut without_discovery = ChatGptSyncState::load(service.data_home()).unwrap();
    without_discovery.provider_observations.clear();
    without_discovery.save(service.data_home()).unwrap();
    assert!(service.import_verified_continuation(&request).is_err());
    let mut direct = request.clone();
    direct.verification_scope =
        chat_history_core::continuation::ContinuationVerificationScope::HistoricalTranscriptDirect;
    direct.transcript_before.status = Some("not_loaded".to_string());
    direct.transcript_after.status = Some("not_loaded".to_string());
    direct.provider_before = direct.transcript_before.clone();
    direct.provider_after = direct.transcript_after.clone();
    service.import_verified_continuation(&direct).unwrap();
    assert!(service.pending_history_restores(16).unwrap().is_empty());
    let verified = service.get_conversation(ID, true).unwrap().unwrap();
    assert_eq!(
        serde_json::to_value(&verified.messages).unwrap(),
        serde_json::to_value(&restored.messages).unwrap()
    );
    assert_eq!(
        verified.raw_json.as_ref().unwrap()["chim_historical_restore"]["requires_live_verification"],
        false
    );
    let direct_health = without_discovery.source_health("chatgpt", ID, Some(20.0));
    assert_ne!(
        direct_health.state,
        chat_history_core::ConversationSourceHealthState::Aligned
    );
    let verified_provenance = service
        .trusted_export_provenance_for_detail(ID, verified.raw_json.as_ref())
        .unwrap();
    assert!(verified_provenance);
    {
        let conn = rusqlite::Connection::open(service.managed_db_path()).unwrap();
        let mode: String = conn
            .query_row(
                "SELECT a.import_mode FROM conversations c JOIN archives a ON a.id=c.archive_id WHERE c.conversation_id=?1",
                [ID],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(mode, "verified_historical_restore");
    }
    let proof = continuation_proof(&verified, &direct_health, 3, 1, verified_provenance);
    assert_eq!(proof.state, "verified");
    assert_eq!(proof.method, "historical-transcript-direct-v1");
    let mut forged = restored.raw_json.unwrap();
    forged["chim_historical_restore"]["requires_live_verification"] = json!(false);
    assert!(
        !service
            .trusted_export_provenance_for_detail(ID, Some(&forged))
            .unwrap()
    );
    let mut forged_detail = verified.clone();
    forged_detail.raw_json = Some(forged);
    assert_eq!(
        continuation_proof(&forged_detail, &direct_health, 3, 1, false).state,
        "unverified"
    );
}

#[test]
fn old_snapshot_and_compiler_evidence_are_preserved_and_revalidation_detects_shift() {
    let (temp, service) = fixture(&["c", "d"], 10.0);
    let old = snapshot(&service);
    let source = directory(
        &temp.path().join("export"),
        &[export(&["a", "b", "c", "d"], 20.0, true)],
    );
    service
        .stage_memory_compilation(&MemoryCompilationBatch {
            project: "LEMonX".into(),
            conversation_id: ID.into(),
            source_snapshot_id: old.clone(),
            through_turn_index: 1,
            through_message_id: "d".into(),
            compiler_version: "test".into(),
            model_label: None,
            created_at: 11.0,
            candidates: vec![MemoryCandidateInput {
                candidate_id: "restoration-candidate".into(),
                payload: MemoryCandidatePayload::Add {
                    memory_id: "restoration-memory".into(),
                    kind: MemoryKind::State,
                    key: "current_goal".into(),
                    value: json!({"text":"synthetic accepted state"}),
                    importance: 80,
                    confidence: 0.8,
                    valid_from: None,
                    valid_until: None,
                    last_verified_at: None,
                },
                rationale: "synthetic".into(),
                evidence: vec![],
            }],
        })
        .unwrap();
    let checkpoint = service
        .memory_compile_checkpoint("LEMonX", ID)
        .unwrap()
        .unwrap();
    service
        .apply_history_restore(
            &service.history_restore_plan(&[source], "MAIN").unwrap(),
            ID,
        )
        .unwrap();
    let after = service
        .memory_compile_checkpoint("LEMonX", ID)
        .unwrap()
        .unwrap();
    assert_eq!(after.source_snapshot_id, checkpoint.source_snapshot_id);
    assert_eq!(after.prefix_sha256_hex, checkpoint.prefix_sha256_hex);
    assert!(
        service
            .memory_health("LEMonX", 30)
            .unwrap()
            .checkpoint_prefix_problem
            > 0
    );
    assert!(
        !service
            .pending_memory_candidate_revalidation_problems("LEMonX")
            .unwrap()
            .is_empty()
    );
    assert!(matches!(
        service
            .promote_memory_candidate("restoration-candidate", 30.0)
            .unwrap(),
        MemoryCandidateDecision::Stale { .. }
    ));
    let conn = chat_history_core::db::open_database(&service.managed_db_path()).unwrap();
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM conversation_snapshot_messages WHERE snapshot_id=?1",
            [old],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 2);
}

#[tokio::test]
async fn equal_or_newer_native_exports_are_not_mechanically_reprojected() {
    for mode in [
        chat_history_core::ImportMode::Copy,
        chat_history_core::ImportMode::Adopt,
    ] {
        let temp = tempfile::tempdir().unwrap();
        let service = IndexService::new(DataHome::new(temp.path().join("data")), None);
        let archive = zip_source(temp.path(), &[export(&["a", "b"], 30.0, false)]);
        service
            .import_archive(chat_history_core::ImportOptions {
                source_archive: archive,
                mode,
                run_api_jobs: false,
                force_summaries: false,
                force_embeddings: false,
            })
            .await
            .unwrap();
        let source = directory(
            &temp.path().join("export"),
            &[export(&["a", "b", "c"], 20.0, true)],
        );
        let before = serde_json::to_value(service.get_conversation(ID, true).unwrap()).unwrap();
        let plan = service.history_restore_plan(&[source], "MAIN").unwrap();
        assert_eq!(plan.entries[0].provenance, Provenance::NativeExport);
        assert_eq!(plan.entries[0].reason, RestoreCode::AlreadyTrustedNative);
        assert!(!plan.entries[0].mergeable);
        assert!(service.apply_history_restore(&plan, ID).is_err());
        assert_eq!(
            serde_json::to_value(service.get_conversation(ID, true).unwrap()).unwrap(),
            before
        );
    }
}

#[tokio::test]
async fn trusted_native_noop_precedes_same_revision_exporter_representation_conflict() {
    let temp = tempfile::tempdir().unwrap();
    let service = IndexService::new(DataHome::new(temp.path().join("data")), None);
    let native = zip_source(temp.path(), &[export(&["a", "b"], 30.0, false)]);
    service
        .import_archive(chat_history_core::ImportOptions {
            source_archive: native.clone(),
            mode: chat_history_core::ImportMode::Copy,
            run_api_jobs: false,
            force_summaries: false,
            force_embeddings: false,
        })
        .await
        .unwrap();

    let mut alternate = export(&["a", "b"], 30.0, true);
    alternate["title"] = json!("same trusted revision, different exporter representation");
    let extracted = directory(&temp.path().join("alternate"), &[alternate]);
    let plan = service
        .history_restore_plan(&[extracted, native], "MAIN")
        .unwrap();
    assert_eq!(plan.entries[0].provenance, Provenance::NativeExport);
    assert_eq!(plan.entries[0].reason, RestoreCode::AlreadyTrustedNative);
    assert!(!plan.entries[0].mergeable);
}

#[test]
fn citation_identity_and_deterministic_file_image_projection_preserve_shared_evidence() {
    let temp = tempfile::tempdir().unwrap();
    let service = IndexService::new(DataHome::new(temp.path().join("data")), None);
    let citation = "trusted rendered [citation](https://example.invalid)";
    let file = format!(
        "{}\n\n[User attached 1 file; file contents were not included]",
        body("c")
    );
    let image = format!(
        "{}\n\n[User attached 1 image; image contents were not included]",
        body("e")
    );
    let bridge: ChatGptBridgeTranscript = serde_json::from_value(json!({"thread_id":ID,"title":TITLE,"update_time":20.0,"pages":[{"has_more":false,"messages":[
        {"message_id":"e","role":"user","text":image},
        {"message_id":"c","role":"user","text":file},
        {"message_id":"b","role":"assistant","text":citation}
    ]}]})).unwrap();
    service
        .import_normalized(vec![bridge.into_normalized().unwrap()], None)
        .unwrap();
    let old = service.get_conversation(ID, true).unwrap().unwrap();
    let mut raw = export(&["a", "b", "c", "d", "e"], 20.0, true);
    raw["mapping"]["n1"]["message"]["metadata"] =
        json!({"content_references":[{"type":"synthetic_reference"}]});
    raw["mapping"]["n2"]["message"]["metadata"] =
        json!({"attachments":[{"id":"synthetic-file","mime_type":"application/pdf"}]});
    raw["mapping"]["n4"]["message"]["content"] = json!({"content_type":"multimodal_text","parts":[{"content_type":"image_asset_pointer","asset_pointer":"file-service://synthetic-image"},body("e")]});
    raw["mapping"]["n4"]["message"]["metadata"] =
        json!({"attachments":[{"id":"synthetic-image","mime_type":"image/png"}]});
    for (i, mut invalid) in [raw.clone(), raw.clone(), raw.clone()]
        .into_iter()
        .enumerate()
    {
        match i {
            0 => {
                invalid["mapping"]["n2"]["message"]["content"]["parts"] =
                    json!(["changed file prefix"])
            }
            1 => {
                invalid["mapping"]["n4"]["message"]["metadata"]["attachments"][0]["id"] =
                    json!("other-image")
            }
            _ => {
                invalid["mapping"]["n1"]["message"]["metadata"]["content_references"] = json!([{}])
            }
        }
        let source = directory(&temp.path().join(format!("invalid-{i}")), &[invalid]);
        assert!(
            !service
                .history_restore_plan(&[source], "MAIN")
                .unwrap()
                .entries[0]
                .mergeable
        );
    }
    let source = directory(&temp.path().join("good"), &[raw.clone()]);
    let plan = service.history_restore_plan(&[source], "MAIN").unwrap();
    assert!(plan.entries[0].mergeable, "{:?}", plan.entries[0].reason);
    service.apply_history_restore(&plan, ID).unwrap();
    let restored = service.get_conversation(ID, true).unwrap().unwrap();
    assert_eq!(ids(&service), ["a", "b", "c", "d", "e"]);
    for old in old.messages {
        let shared = restored
            .messages
            .iter()
            .find(|m| m.message_id == old.message_id)
            .unwrap();
        assert_eq!(shared.normalized_text, old.normalized_text);
        assert_eq!(shared.raw_message_json, old.raw_message_json);
    }
    // Restored export-only content is now native raw evidence, never citation fallback.
    let mut divergent = export(&["a", "b", "c", "d", "e", "f"], 30.0, true);
    divergent["mapping"] = raw["mapping"].clone();
    divergent["current_node"] = json!("n4");
    divergent["mapping"]["n0"]["message"]["content"]["parts"] =
        json!(["changed restored export body"]);
    let source = directory(&temp.path().join("native-divergence"), &[divergent]);
    assert_eq!(
        service
            .history_restore_plan(&[source], "MAIN")
            .unwrap()
            .entries[0]
            .reason,
        RestoreCode::SourceConflict
    );
}

#[test]
fn late_publication_failure_rolls_back_canonical_snapshots_fts_and_jobs() {
    let (temp, service) = fixture(&["c", "d"], 20.0);
    let source = directory(
        &temp.path().join("export"),
        &[export(&["a", "b", "c", "d"], 20.0, true)],
    );
    let plan = service.history_restore_plan(&[source], "MAIN").unwrap();
    let before = serde_json::to_value(service.get_conversation(ID, true).unwrap()).unwrap();
    let original_snapshot = snapshot(&service);
    let conn = chat_history_core::db::open_database(&service.managed_db_path()).unwrap();
    conn.execute_batch("CREATE TRIGGER fail_restore BEFORE UPDATE ON runs WHEN NEW.run_kind='historical_restore' AND NEW.status='complete' BEGIN SELECT RAISE(ABORT,'synthetic failure'); END;").unwrap();
    let before_db = fs::read(service.managed_db_path()).unwrap();
    let error = service.apply_history_restore(&plan, ID).unwrap_err();
    assert_eq!(
        error.downcast_ref::<RestoreCode>(),
        Some(&RestoreCode::PublicationFailed)
    );
    assert!(!error.to_string().contains("PRIVATE_BODY_SENTINEL"));
    assert_eq!(
        serde_json::to_value(service.get_conversation(ID, true).unwrap()).unwrap(),
        before
    );
    assert_eq!(snapshot(&service), original_snapshot);
    assert_eq!(fs::read(service.managed_db_path()).unwrap(), before_db);
    assert_eq!(
        conn.query_row(
            "SELECT count(*) FROM archives WHERE import_mode='historical_restore'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    conn.execute_batch("DROP TRIGGER fail_restore").unwrap();
    // Retry reuses deterministic evidence; no second content-addressed bundle appears.
    let bundles = fs::read_dir(service.data_home().paths().sources_dir)
        .unwrap()
        .count();
    // ZIP timestamps have two-second DOS granularity. Cross that boundary so
    // this regression proves the evidence bytes do not depend on wall clock.
    std::thread::sleep(std::time::Duration::from_millis(2100));
    service.apply_history_restore(&plan, ID).unwrap();
    assert_eq!(
        fs::read_dir(service.data_home().paths().sources_dir)
            .unwrap()
            .count(),
        bundles
    );
}
