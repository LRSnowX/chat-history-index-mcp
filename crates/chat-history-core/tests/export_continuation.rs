use std::{fs, io::Write, path::Path};

use chat_history_core::continuation::{ContinuationImport, RepairFailureCode, continuation_proof};
use chat_history_core::{ChatGptBridgeTranscript, ChatGptSyncState, DataHome, IndexService};
use serde_json::{Value, json};
use zip::write::SimpleFileOptions;

const ID: &str = "synthetic-oversized-continuation";
const OLD: f64 = 1791268075.055259;
const NEW: f64 = 1791431199.636;

fn bodies() -> Vec<String> {
    (0..12)
        .map(|i| {
            if i == 10 {
                format!("  {}\n\n  ", "无损 oversized body 🦀 ".repeat(1800))
            } else {
                format!("synthetic conversation message {i}")
            }
        })
        .collect()
}

fn official_export() -> Value {
    let mut mapping = serde_json::Map::new();
    mapping.insert(
        "root".into(),
        json!({"id":"root","parent":null,"children":["node-0"],"message":null}),
    );
    for (i, text) in bodies().into_iter().enumerate() {
        mapping.insert(format!("node-{i}"),json!({"id":format!("node-{i}"),"parent":if i==0 {"root".to_string()} else {format!("node-{}",i-1)},"children":if i==11 {vec![]} else {vec![format!("node-{}",i+1)]},
            "message":{"id":format!("m{i}"),"author":{"role":if i%2==0 {"user"} else {"assistant"}},"content":{"content_type":"text","parts":[text]},"status":"finished_successfully","create_time":100.0-i as f64}}));
    }
    json!({"id":ID,"conversation_id":ID,"title":"Synthetic oversized recovery","update_time":NEW,"current_node":"node-11","mapping":mapping})
}

fn write_archive(path: &Path, text: &str) {
    let mut inner = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    inner
        .start_file("conversations-000.json", SimpleFileOptions::default())
        .unwrap();
    inner.write_all(text.as_bytes()).unwrap();
    let bytes = inner.finish().unwrap().into_inner();
    let mut outer = zip::ZipWriter::new(fs::File::create(path).unwrap());
    outer
        .start_file(
            "User Online Activity/Conversations__part-0001.zip",
            SimpleFileOptions::default(),
        )
        .unwrap();
    outer.write_all(&bytes).unwrap();
    outer.finish().unwrap();
}

fn fixture() -> (tempfile::TempDir, IndexService) {
    let temp = tempfile::tempdir().unwrap();
    let service = IndexService::new(DataHome::new(temp.path().join("data")), None);
    let messages = bodies().into_iter().take(10).enumerate().map(|(i,text)|json!({"message_id":format!("m{i}"),"role":if i%2==0 {"user"} else {"assistant"},"text":text})).rev().collect::<Vec<_>>();
    let transcript: ChatGptBridgeTranscript = serde_json::from_value(json!({"thread_id":ID,"title":"Synthetic oversized recovery","update_time":OLD,"pages":[{"has_more":false,"messages":messages}]})).unwrap();
    service
        .import_normalized(vec![transcript.into_normalized().unwrap()], None)
        .unwrap();
    let mut state = ChatGptSyncState::default();
    state.plan_recent(serde_json::from_value(json!({"requested_limit":50,"threads":[{"thread_id":ID,"kind":"chatgpt","title":"synthetic","status":"idle","update_time":NEW,"observed_at":NEW+1.0}]})).unwrap()).unwrap();
    state.mark_blocked(
        ID,
        "read_thread reached the 20000-character per-message safety limit",
    );
    state.save(service.data_home()).unwrap();
    // This existing bootstrap artifact must never be reused/replaced by recovery.
    fs::write(
        service.data_home().paths().archive_path,
        b"previous official export evidence",
    )
    .unwrap();
    (temp, service)
}

#[test]
fn lossless_offline_recovery_then_live_replay_restores_verified_continuation() {
    let (temp, service) = fixture();
    let original = service.get_conversation(ID, true).unwrap().unwrap();
    let baseline = service.continuation_baseline(ID).unwrap();
    let before_state = fs::read(chat_history_core::chatgpt::sync_state_path(
        service.data_home(),
    ))
    .unwrap();
    let archive = temp.path().join("official.zip");
    let mut export = official_export();
    // Repeated parts and whitespace are retained exactly, not deduped/trimmed.
    export["mapping"]["node-10"]["message"]["content"]["parts"] =
        json!([bodies()[10], bodies()[10]]);
    let oversized = format!("{}\n{}", bodies()[10], bodies()[10]);
    write_archive(&archive, &json!([export]).to_string());
    let report = service
        .import_export_continuation(&baseline, &archive)
        .unwrap();
    let offline = service.get_conversation(ID, true).unwrap().unwrap();
    assert_eq!(offline.messages.len(), 12);
    assert_eq!(offline.messages[10].normalized_text, oversized);
    assert!(oversized.chars().count() > 20_000);
    assert_eq!(
        serde_json::to_value(&offline.messages[..10]).unwrap(),
        serde_json::to_value(&original.messages).unwrap()
    );
    assert_eq!(
        fs::read(chat_history_core::chatgpt::sync_state_path(
            service.data_home()
        ))
        .unwrap(),
        before_state
    );
    assert_eq!(
        fs::read(service.data_home().paths().archive_path).unwrap(),
        b"previous official export evidence"
    );
    assert_eq!(
        fs::read(&report.archive_path).unwrap(),
        fs::read(&archive).unwrap()
    );
    let native = service
        .native_export_provenance_for_detail(ID, offline.raw_json.as_ref())
        .unwrap();
    assert!(native);
    let mut state = ChatGptSyncState::load(service.data_home()).unwrap();
    let health = state.source_health("chatgpt", ID, Some(NEW));
    assert_eq!(
        health.state,
        chat_history_core::ConversationSourceHealthState::Blocked
    );
    assert_eq!(
        continuation_proof(&offline, &health, 10, 2, native).state,
        "unverified"
    );
    let observation = json!({"thread_id":ID,"kind":"chatgpt","title":"synthetic","status":"idle","update_time":NEW+5.0,"observed_at":NEW+10.0});
    state
        .plan_recent(
            serde_json::from_value(json!({"requested_limit":50,"threads":[observation.clone()]}))
                .unwrap(),
        )
        .unwrap();
    state.save(service.data_home()).unwrap();
    let mut messages = offline.messages.iter().map(|m| json!({"message_id":m.message_id,"role":m.role,"text":if m.turn_index==10 {"truncated provider copy"} else {m.normalized_text.as_str()},"truncated":m.turn_index==10,"stable_identity":true})).collect::<Vec<_>>();
    messages.push(json!({"message_id":"m12","role":"user","text":"complete later tail","stable_identity":true}));
    let request = ContinuationImport {
        baseline:service.continuation_baseline(ID).unwrap(), provider_before:serde_json::from_value(observation.clone()).unwrap(), provider_after:serde_json::from_value(observation).unwrap(),
        transcript:serde_json::from_value(json!({"thread_id":ID,"title":"synthetic","update_time":NEW+5.0,"pages":[{"has_more":false,"provider_revision":NEW+5.0,"messages":messages.into_iter().rev().collect::<Vec<_>>()}]})).unwrap(),
    };
    service.import_verified_continuation(&request).unwrap();
    let after = service.get_conversation(ID, true).unwrap().unwrap();
    assert_eq!(
        serde_json::to_value(&after.messages[..12]).unwrap(),
        serde_json::to_value(&offline.messages).unwrap()
    );
    assert_eq!(after.messages[10].normalized_text, oversized);
    assert!(
        ChatGptSyncState::load(service.data_home())
            .unwrap()
            .blocked
            .contains_key(ID)
    );
    state.mark_imported_at(ID, Some(NEW + 5.0));
    state.save(service.data_home()).unwrap();
    let health = state.source_health("chatgpt", ID, Some(NEW + 5.0));
    assert_eq!(
        health.state,
        chat_history_core::ConversationSourceHealthState::Aligned
    );
    assert_eq!(
        continuation_proof(&after, &health, 11, 2, false).state,
        "verified"
    );
    assert!(
        service.continuation_baseline(ID).is_ok(),
        "native-export provenance survives verified bridge publication"
    );
    let conn = chat_history_core::db::open_database(&service.managed_db_path()).unwrap();
    let fts: String = conn
        .query_row(
            "SELECT transcript_text FROM conversation_fts WHERE conversation_id=?1",
            [ID],
            |r| r.get(0),
        )
        .unwrap();
    assert!(fts.contains(&oversized));
    let snapshots: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM conversation_snapshots WHERE conversation_id=?1",
            [ID],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(snapshots, 3);
}

#[test]
fn malformed_ambiguous_divergent_and_stale_exports_never_mutate_canonical() {
    for case in 0..18 {
        let (temp, service) = fixture();
        let mut baseline = service.continuation_baseline(ID).unwrap();
        let mut export = official_export();
        match case {
            0 => {
                export["id"] = json!("foreign");
                export["conversation_id"] = json!("foreign");
            }
            1 => export["id"] = json!("contradiction"),
            2 => export["update_time"] = json!(OLD - 1.0),
            3 => {
                export["mapping"]["node-4"]["message"]["content"]["parts"] =
                    json!(["changed trusted body"])
            }
            4 => {
                export["mapping"]["node-1"]["message"]["id"] = json!("m2");
                export["mapping"]["node-2"]["message"]["id"] = json!("m1");
            }
            5 => {
                export["mapping"]["node-3"]["children"] = json!(["node-5"]);
                export["mapping"]["node-5"]["parent"] = json!("node-3");
                export["mapping"].as_object_mut().unwrap().remove("node-4");
            }
            6 => export["mapping"]["node-11"]["message"]["id"] = json!("m10"),
            7 => {
                export["mapping"]["node-10"]["message"]
                    .as_object_mut()
                    .unwrap()
                    .remove("id");
            }
            8 => export["mapping"]["node-11"]["message"]["status"] = json!("in_progress"),
            9 => export["mapping"]["node-3"]["children"] = json!(["node-4", "node-5"]),
            10 => export["current_node"] = json!("node-8"),
            11 => baseline.baseline_token.push('x'),
            12 => {} // duplicate matching conversation across export records
            13 => {} // duplicate JSON object key, not merely duplicate message IDs
            14 => {} // incomplete JSON after a complete target record
            15 => {} // trailing JSON beyond the export array
            16 => {
                export.as_object_mut().unwrap().remove("current_node");
            }
            17 => export["update_time"] = json!(OLD),
            _ => unreachable!(),
        }
        let text = match case {
            12 => json!([export.clone(), export]).to_string(),
            13 => json!([export])
                .to_string()
                .replace("\"id\":\"m10\"", "\"id\":\"m10\",\"id\":\"m10\""),
            14 => {
                let mut text = json!([export]).to_string();
                text.pop();
                text
            }
            15 => format!("{} {{}}", json!([export])),
            _ => json!([export]).to_string(),
        };
        let archive = temp.path().join("official.zip");
        write_archive(&archive, &text);
        let before = serde_json::to_value(service.get_conversation(ID, true).unwrap()).unwrap();
        let state = fs::read(chat_history_core::chatgpt::sync_state_path(
            service.data_home(),
        ))
        .unwrap();
        let error = service
            .import_export_continuation(&baseline, &archive)
            .unwrap_err();
        assert!(
            !error.to_string().contains("trusted body"),
            "content-free rejection {case}"
        );
        assert_eq!(
            serde_json::to_value(service.get_conversation(ID, true).unwrap()).unwrap(),
            before,
            "case {case}"
        );
        assert_eq!(
            fs::read(chat_history_core::chatgpt::sync_state_path(
                service.data_home()
            ))
            .unwrap(),
            state
        );
    }
}

#[test]
fn absent_live_block_and_late_publication_failure_cannot_manufacture_alignment() {
    let (temp, service) = fixture();
    let archive = temp.path().join("official.zip");
    write_archive(&archive, &json!([official_export()]).to_string());
    let baseline = service.continuation_baseline(ID).unwrap();
    let mut state = ChatGptSyncState::load(service.data_home()).unwrap();
    state.blocked.clear();
    state.pending.clear();
    state.save(service.data_home()).unwrap();
    let error = service
        .import_export_continuation(&baseline, &archive)
        .unwrap_err();
    assert_eq!(
        RepairFailureCode::from_error(&error),
        RepairFailureCode::LiveBlockRequired
    );
    assert_eq!(
        service
            .get_conversation(ID, false)
            .unwrap()
            .unwrap()
            .messages
            .len(),
        10
    );
    state.mark_blocked(ID, "original blocker");
    state.save(service.data_home()).unwrap();
    let conn = chat_history_core::db::open_database(&service.managed_db_path()).unwrap();
    conn.execute_batch("CREATE TRIGGER fail_export BEFORE UPDATE ON runs WHEN NEW.run_kind='official_export_continuation' AND NEW.status='complete' BEGIN SELECT RAISE(ABORT,'injected'); END;").unwrap();
    assert!(
        service
            .import_export_continuation(&baseline, &archive)
            .is_err()
    );
    assert_eq!(
        service
            .get_conversation(ID, false)
            .unwrap()
            .unwrap()
            .messages
            .len(),
        10
    );
    let snapshots: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM conversation_snapshots WHERE conversation_id=?1",
            [ID],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(snapshots, 1);
}

#[test]
fn a_real_canonical_change_invalidates_an_export_baseline_before_publication() {
    let (temp, service) = fixture();
    let baseline = service.continuation_baseline(ID).unwrap();
    let archive = temp.path().join("official.zip");
    write_archive(&archive, &json!([official_export()]).to_string());
    let messages = bodies().into_iter().take(11).enumerate().map(|(i,text)|json!({"message_id":format!("m{i}"),"role":if i%2==0 {"user"} else {"assistant"},"text":text})).rev().collect::<Vec<_>>();
    let current: ChatGptBridgeTranscript = serde_json::from_value(json!({"thread_id":ID,"title":"synthetic changed canonical","update_time":NEW+5.0,"pages":[{"has_more":false,"messages":messages}]})).unwrap();
    service
        .import_normalized(vec![current.into_normalized().unwrap()], None)
        .unwrap();
    let before = serde_json::to_value(service.get_conversation(ID, true).unwrap()).unwrap();
    let error = service
        .import_export_continuation(&baseline, &archive)
        .unwrap_err();
    assert_eq!(
        RepairFailureCode::from_error(&error),
        RepairFailureCode::BaselineChanged
    );
    assert_eq!(
        serde_json::to_value(service.get_conversation(ID, true).unwrap()).unwrap(),
        before
    );
}

fn modern_fixture() -> (tempfile::TempDir, IndexService, Value) {
    let (temp, service) = fixture();
    let mut export = official_export();
    let mapping = export["mapping"].as_object_mut().unwrap();
    mapping.get_mut("root").unwrap()["children"] = json!(["node-0", "off-branch"]);
    // Unsupported/incomplete off-branch messages are not active continuation evidence.
    mapping.insert("off-branch".into(), json!({"id":"off-branch","parent":"root","children":[],"message":{"id":"discarded-message","author":{"role":"tool"},"content":{"content_type":"code","text":"OFF_BRANCH_SENTINEL"},"status":"in_progress"}}));
    mapping.get_mut("node-3").unwrap()["children"] = json!(["thoughts"]);
    mapping.get_mut("node-4").unwrap()["parent"] = json!("recap");
    mapping.insert("thoughts".into(), json!({"id":"thoughts","parent":"node-3","children":["recap"],"message":{"id":"internal-thoughts","author":{"role":"assistant"},"content":{"content_type":"thoughts","thoughts":[{"summary":"synthetic internal","content":"INTERNAL_SENTINEL"}]},"status":"finished_successfully"}}));
    mapping.insert("recap".into(), json!({"id":"recap","parent":"thoughts","children":["node-4"],"message":{"id":"internal-recap","author":{"role":"assistant"},"content":{"content_type":"reasoning_recap","content":"INTERNAL_SENTINEL"},"status":"finished_successfully"}}));
    for i in [1, 11] {
        mapping.get_mut(&format!("node-{i}")).unwrap()["message"]["metadata"] =
            json!({"content_references":[{"type":"synthetic_reference"}]});
        mapping.get_mut(&format!("node-{i}")).unwrap()["message"]["content"]["parts"] =
            json!([format!("complete export citation body {i}")]);
    }
    mapping.get_mut("node-2").unwrap()["message"]["metadata"] = json!({"attachments":[{"id":"file-a","mime_type":"application/pdf","name":"synthetic-a.pdf"},{"id":"file-b","mime_type":"text/plain","name":"synthetic-b.txt"}]});
    mapping.get_mut("node-4").unwrap()["message"]["content"] = json!({"content_type":"multimodal_text","parts":[{"content_type":"image_asset_pointer","asset_pointer":"file-service://synthetic-image"},bodies()[4]]});
    mapping.get_mut("node-4").unwrap()["message"]["metadata"] =
        json!({"attachments":[{"id":"synthetic-image","mime_type":"image/png"}]});
    mapping.get_mut("node-11").unwrap()["children"] = json!(["node-12"]);
    mapping.insert("node-12".into(), json!({"id":"node-12","parent":"node-11","children":["node-13"],"message":{"id":"m12","author":{"role":"user"},"content":{"content_type":"text","parts":["full new file instruction"]},"metadata":{"attachments":[{"id":"file-c","mime_type":"application/pdf"}]},"status":"finished_successfully"}}));
    mapping.insert("node-13".into(), json!({"id":"node-13","parent":"node-12","children":["node-14"],"message":{"id":"m13","author":{"role":"assistant"},"content":{"content_type":"text","parts":["complete ordinary new assistant"]},"status":"finished_successfully"}}));
    mapping.insert("node-14".into(), json!({"id":"node-14","parent":"node-13","children":[],"message":{"id":"m14","author":{"role":"user"},"content":{"content_type":"multimodal_text","parts":[{"content_type":"image_asset_pointer","asset_pointer":"file-service://image-a"},"full new image instruction",{"content_type":"image_asset_pointer","asset_pointer":"file-service://image-b"}]},"status":"finished_successfully"}}));
    export["current_node"] = json!("node-14");
    let original = service.get_conversation(ID, true).unwrap().unwrap();
    let messages = original.messages.iter().map(|m| json!({"message_id":m.message_id,"role":m.role,"text":match m.turn_index {
        1 => "trusted App Tools rendered [citation](https://example.invalid)".to_string(),
        2 => format!("{}\n\n[User attached 2 files; file contents were not included]",m.normalized_text),
        4 => format!("{}\n\n[User attached 1 image; image contents were not included]",m.normalized_text),
        _ => m.normalized_text.clone(),
    }})).rev().collect::<Vec<_>>();
    let bridge: ChatGptBridgeTranscript = serde_json::from_value(json!({"thread_id":ID,"title":"synthetic modern baseline","update_time":OLD,"pages":[{"has_more":false,"messages":messages}]})).unwrap();
    service
        .import_normalized(vec![bridge.into_normalized().unwrap()], None)
        .unwrap();
    (temp, service, export)
}

fn modern_live_request(service: &IndexService) -> ContinuationImport {
    let detail = service.get_conversation(ID, true).unwrap().unwrap();
    let observation = json!({"thread_id":ID,"kind":"chatgpt","title":"synthetic modern continuation","status":"idle","update_time":NEW+5.0,"observed_at":NEW+10.0});
    let mut state = ChatGptSyncState::load(service.data_home()).unwrap();
    state
        .plan_recent(
            serde_json::from_value(json!({"requested_limit":50,"threads":[observation.clone()]}))
                .unwrap(),
        )
        .unwrap();
    state.save(service.data_home()).unwrap();
    let mut messages = detail
        .messages
        .iter()
        .map(|m| {
            json!({"message_id":m.message_id,"role":m.role,"text":match m.turn_index {
        10 => "truncated live oversized copy",
        11 => "live differently rendered markdown citation",
        _ => m.normalized_text.as_str(),
    },"stable_identity":true,"truncated":m.turn_index==10})
        })
        .collect::<Vec<_>>();
    messages.push(json!({"message_id":"m15","role":"assistant","text":"complete later tail","stable_identity":true}));
    ContinuationImport {
        baseline:service.continuation_baseline(ID).unwrap(), provider_before:serde_json::from_value(observation.clone()).unwrap(), provider_after:serde_json::from_value(observation).unwrap(),
        transcript:serde_json::from_value(json!({"thread_id":ID,"title":"synthetic modern continuation","update_time":NEW+5.0,"pages":[{"has_more":false,"provider_revision":NEW+5.0,"messages":messages.into_iter().rev().collect::<Vec<_>>()}]})).unwrap(),
    }
}

#[test]
fn modern_active_lineage_preserves_bridge_prefix_and_complete_export_bodies_through_live_replay() {
    let (temp, service, export) = modern_fixture();
    let before = service.get_conversation(ID, true).unwrap().unwrap();
    let state_before = fs::read(chat_history_core::chatgpt::sync_state_path(
        service.data_home(),
    ))
    .unwrap();
    let archive = temp.path().join("modern.zip");
    write_archive(&archive, &json!([export]).to_string());
    service
        .import_export_continuation(&service.continuation_baseline(ID).unwrap(), &archive)
        .unwrap();
    let repaired = service.get_conversation(ID, true).unwrap().unwrap();
    assert_eq!(repaired.messages.len(), 15);
    assert_eq!(
        serde_json::to_value(&repaired.messages[..10]).unwrap(),
        serde_json::to_value(&before.messages).unwrap()
    );
    assert_eq!(repaired.messages[10].normalized_text, bodies()[10]);
    assert_eq!(
        repaired.messages[11].normalized_text,
        "complete export citation body 11"
    );
    assert_eq!(
        repaired.messages[12].normalized_text,
        "full new file instruction\n\n[User attached 1 file; file contents were not included]"
    );
    assert_eq!(
        repaired.messages[14].normalized_text,
        "full new image instruction\n\n[User attached 2 images; image contents were not included]"
    );
    assert_eq!(
        repaired.messages[14].raw_message_json["content"]["parts"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    assert!(
        !repaired
            .messages
            .iter()
            .any(|m| m.normalized_text.contains("SENTINEL"))
    );
    assert_eq!(
        fs::read(chat_history_core::chatgpt::sync_state_path(
            service.data_home()
        ))
        .unwrap(),
        state_before
    );
    let request = modern_live_request(&service);
    service.import_verified_continuation(&request).unwrap();
    let after = service.get_conversation(ID, true).unwrap().unwrap();
    assert_eq!(
        serde_json::to_value(&after.messages[..15]).unwrap(),
        serde_json::to_value(&repaired.messages).unwrap()
    );
    let mut state = ChatGptSyncState::load(service.data_home()).unwrap();
    assert!(state.blocked.contains_key(ID));
    state.mark_imported_at(ID, Some(NEW + 5.0));
    state.save(service.data_home()).unwrap();
    let health = state.source_health("chatgpt", ID, Some(NEW + 5.0));
    assert_eq!(
        health.state,
        chat_history_core::ConversationSourceHealthState::Aligned
    );
    assert_eq!(
        continuation_proof(&after, &health, 14, 2, false).state,
        "verified"
    );
}

#[test]
fn current_node_is_an_active_selection_boundary_not_a_graph_leaf() {
    let (temp, service, mut export) = modern_fixture();
    export["mapping"]["node-14"]["children"] = json!(["future-descendant"]);
    export["mapping"]["future-descendant"] = json!({
        "id":"future-descendant",
        "parent":"node-14",
        "children":[],
        "message":{
            "id":"future-descendant-message",
            "author":{"role":"assistant"},
            "content":{"content_type":"thoughts","thoughts":[{"summary":"off selection","content":"OFF_SELECTION_SENTINEL"}]},
            "status":"finished_successfully"
        }
    });
    assert_eq!(export["current_node"], json!("node-14"));
    let archive = temp.path().join("modern-current-selection.zip");
    write_archive(&archive, &json!([export]).to_string());
    service
        .import_export_continuation(&service.continuation_baseline(ID).unwrap(), &archive)
        .unwrap();
    let repaired = service.get_conversation(ID, true).unwrap().unwrap();
    assert_eq!(repaired.messages.len(), 15);
    assert!(
        !repaired
            .messages
            .iter()
            .any(|m| m.normalized_text.contains("OFF_SELECTION_SENTINEL"))
    );
}

#[test]
fn statusless_modern_export_is_accepted_but_mixed_completion_schema_is_rejected() {
    let (temp, service, mut export) = modern_fixture();
    for node in export["mapping"].as_object_mut().unwrap().values_mut() {
        if let Some(message) = node.get_mut("message").and_then(Value::as_object_mut) {
            message.remove("status");
        }
    }
    let archive = temp.path().join("modern-statusless.zip");
    write_archive(&archive, &json!([export]).to_string());
    service
        .import_export_continuation(&service.continuation_baseline(ID).unwrap(), &archive)
        .unwrap();
    assert_eq!(
        service
            .get_conversation(ID, false)
            .unwrap()
            .unwrap()
            .messages
            .len(),
        15
    );

    let (temp, service, mut mixed) = modern_fixture();
    mixed["mapping"]["node-12"]["message"]
        .as_object_mut()
        .unwrap()
        .remove("status");
    let before = serde_json::to_value(service.get_conversation(ID, true).unwrap()).unwrap();
    let archive = temp.path().join("modern-mixed-status.zip");
    write_archive(&archive, &json!([mixed]).to_string());
    assert!(
        service
            .import_export_continuation(&service.continuation_baseline(ID).unwrap(), &archive)
            .is_err()
    );
    assert_eq!(
        serde_json::to_value(service.get_conversation(ID, true).unwrap()).unwrap(),
        before
    );
}

#[test]
fn parent_only_modern_mapping_is_accepted_but_mixed_children_schema_is_rejected() {
    let (temp, service, mut export) = modern_fixture();
    for node in export["mapping"].as_object_mut().unwrap().values_mut() {
        node.as_object_mut().unwrap().remove("children");
        if let Some(message) = node.get_mut("message").and_then(Value::as_object_mut) {
            message.remove("status");
        }
    }
    let archive = temp.path().join("modern-parent-only.zip");
    write_archive(&archive, &json!([export.clone()]).to_string());
    service
        .import_export_continuation(&service.continuation_baseline(ID).unwrap(), &archive)
        .unwrap();
    assert_eq!(
        service
            .get_conversation(ID, false)
            .unwrap()
            .unwrap()
            .messages
            .len(),
        15
    );

    let (temp, service, mut mixed) = modern_fixture();
    mixed["mapping"]["node-12"]
        .as_object_mut()
        .unwrap()
        .remove("children");
    let archive = temp.path().join("modern-mixed-children.zip");
    write_archive(&archive, &json!([mixed]).to_string());
    assert!(
        service
            .import_export_continuation(&service.continuation_baseline(ID).unwrap(), &archive)
            .is_err()
    );
}

#[test]
fn modern_graph_and_visible_content_failures_cannot_mutate_canonical() {
    for case in 0..20 {
        let (temp, service, mut export) = modern_fixture();
        match case {
            0 => {
                export.as_object_mut().unwrap().remove("current_node");
            }
            1 => export["current_node"] = json!("absent"),
            2 => export["current_node"] = json!("node-3"),
            3 => export["mapping"]["node-4"]["parent"] = json!("node-2"),
            4 => {
                export["mapping"]["root"]["children"] = json!(["off-branch"]);
                export["mapping"]["node-0"]["parent"] = json!("node-14");
                export["mapping"]["node-14"]["children"] = json!(["node-0"]);
            }
            5 => export["mapping"]["off-branch"]["parent"] = Value::Null,
            6 => export["mapping"]["off-branch"]["message"]["id"] = json!("m2"),
            7 => export["mapping"]["node-4"]["id"] = json!("node-5"),
            8 => export["mapping"]["node-12"]["message"]["content"]["content_type"] = json!("code"),
            9 => {
                export["mapping"]["node-2"]["message"]["content"]["parts"] =
                    json!(["changed prefix instruction"])
            }
            10 => {
                export["mapping"]["node-4"]["message"]["content"]["parts"][1] =
                    json!("changed image prefix")
            }
            11 => export["mapping"]["thoughts"]["message"]["author"]["role"] = json!("user"),
            12 => {
                export["mapping"]["node-14"]["message"]["content"]["parts"][0]["content_type"] =
                    json!("audio_asset_pointer")
            }
            13 => {
                export["mapping"]["node-13"]["message"]["content"]["content_type"] =
                    json!("future_visible_type")
            }
            14 => export["mapping"]["node-3"]["children"] = json!(["thoughts", "thoughts"]),
            15 => {
                export["mapping"]["node-13"]["message"]["metadata"] =
                    json!({"content_references":"not-an-array"})
            }
            16 => export["mapping"]["node-12"]["message"]["status"] = json!("in_progress"),
            17 => {
                export["mapping"]["node-11"]["message"]["metadata"]["content_references"] =
                    json!([null])
            }
            18 => {
                export["mapping"]["node-11"]["message"]["metadata"]["content_references"] =
                    json!([{}])
            }
            19 => {
                export["mapping"]["node-4"]["message"]["metadata"]["attachments"][0]["id"] =
                    json!("different-image-id")
            }
            _ => unreachable!(),
        }
        let before = serde_json::to_value(service.get_conversation(ID, true).unwrap()).unwrap();
        let archive = temp.path().join("modern-invalid.zip");
        write_archive(&archive, &json!([export]).to_string());
        assert!(
            service
                .import_export_continuation(&service.continuation_baseline(ID).unwrap(), &archive)
                .is_err(),
            "case {case}"
        );
        assert_eq!(
            serde_json::to_value(service.get_conversation(ID, true).unwrap()).unwrap(),
            before,
            "case {case}"
        );
    }
}

#[test]
fn export_origin_content_and_deterministic_live_projections_still_require_equality() {
    let (temp, service, mut export) = modern_fixture();
    let archive = temp.path().join("modern.zip");
    write_archive(&archive, &json!([export.clone()]).to_string());
    service
        .import_export_continuation(&service.continuation_baseline(ID).unwrap(), &archive)
        .unwrap();
    let before = serde_json::to_value(service.get_conversation(ID, true).unwrap()).unwrap();
    export["update_time"] = json!(NEW + 10.0);
    export["mapping"]["node-14"]["children"] = json!(["node-15"]);
    export["mapping"]["node-15"] = json!({"id":"node-15","parent":"node-14","children":[],"message":{"id":"m15","author":{"role":"assistant"},"content":{"content_type":"text","parts":["new complete message"]},"status":"finished_successfully"}});
    export["current_node"] = json!("node-15");
    // Citation identity-only applies across surfaces, NOT export -> export.
    export["mapping"]["node-11"]["message"]["content"]["parts"] =
        json!(["changed native citation body"]);
    write_archive(&archive, &json!([export]).to_string());
    assert!(
        service
            .import_export_continuation(&service.continuation_baseline(ID).unwrap(), &archive)
            .is_err()
    );
    assert_eq!(
        serde_json::to_value(service.get_conversation(ID, true).unwrap()).unwrap(),
        before
    );
    let request = modern_live_request(&service);
    for id in ["m0", "m1", "m2", "m4", "m12", "m13", "m14"] {
        let mut invalid = request.clone();
        invalid.transcript.pages[0]
            .messages
            .iter_mut()
            .find(|m| m.message_id == id)
            .unwrap()
            .text = "unexplained visible body change".into();
        assert!(
            service.import_verified_continuation(&invalid).is_err(),
            "{id}"
        );
        assert_eq!(
            serde_json::to_value(service.get_conversation(ID, true).unwrap()).unwrap(),
            before
        );
    }
}
