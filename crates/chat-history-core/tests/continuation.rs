use chat_history_core::continuation::{ContinuationImport, baseline, continuation_proof};
use chat_history_core::{ChatGptBridgeTranscript, ChatGptSyncState, DataHome, IndexService};
use serde_json::json;

const OLD: f64 = 1791268075.055259;
const NEW: f64 = 1791431199.636;
const ID: &str = "synthetic-arcos-continuation";

fn fixture() -> (tempfile::TempDir, IndexService, ContinuationImport) {
    let temp = tempfile::tempdir().unwrap();
    let home = DataHome::new(temp.path().join("data"));
    let service = IndexService::new(home.clone(), None);
    let messages = (0..10).map(|index| json!({"message_id":format!("m{index}"), "role":if index%2==0 {"user"} else {"assistant"}, "text":if index==3 {"trusted old body ".repeat(2500)} else {format!("Arcos synthetic Foundation B {index}")}, "stable_identity":true})).collect::<Vec<_>>();
    let full = json!({"thread_id":ID,"title":"Arcos synthetic regression","update_time":OLD,"pages":[{"request_cursor":null,"next_cursor":null,"has_more":false,"messages":messages.iter().rev().collect::<Vec<_>>()}]});
    service
        .import_normalized(
            vec![
                serde_json::from_value::<ChatGptBridgeTranscript>(full)
                    .unwrap()
                    .into_normalized()
                    .unwrap(),
            ],
            None,
        )
        .unwrap();
    let mut replay = messages;
    replay[3]["text"] = json!("trusted old body ".repeat(1300));
    replay[3]["truncated"] = json!(true);
    replay.extend((10..12).map(|index| json!({"message_id":format!("m{index}"),"role":if index%2==0 {"user"} else {"assistant"},"text":format!("Arcos synthetic later refinement {index}"),"stable_identity":true})));
    let observation = json!({"thread_id":ID,"kind":"chatgpt","title":"Arcos synthetic regression","update_time":NEW,"status":"idle","observed_at":NEW+10.0});
    let mut state = ChatGptSyncState::default();
    state
        .plan_recent(
            serde_json::from_value(json!({"requested_limit":50,"threads":[observation.clone()]}))
                .unwrap(),
        )
        .unwrap();
    state.mark_blocked(
        ID,
        "read_thread reached the 20000-character per-message safety limit",
    );
    state.save(&home).unwrap();
    let request = ContinuationImport {
        baseline:service.continuation_baseline(ID).unwrap(),
        provider_before:serde_json::from_value(observation.clone()).unwrap(),
        provider_after:serde_json::from_value(observation).unwrap(),
        transcript:serde_json::from_value(json!({"thread_id":ID,"title":"Arcos synthetic regression","update_time":NEW,"pages":[{"request_cursor":null,"next_cursor":null,"has_more":false,"provider_revision":NEW,"messages":replay.into_iter().rev().collect::<Vec<_>>()}]})).unwrap(),
    };
    (temp, service, request)
}

#[test]
fn verified_append_preserves_prefix_and_only_then_allows_alignment_and_tail_proof() {
    let (_temp, service, request) = fixture();
    let before = service.get_conversation(ID, true).unwrap().unwrap();
    service.import_verified_continuation(&request).unwrap();
    let after = service.get_conversation(ID, true).unwrap().unwrap();
    assert_eq!(after.messages.len(), 12);
    assert_eq!(after.conversation.update_time, Some(NEW));
    assert_eq!(
        serde_json::to_value(&after.messages[..10]).unwrap(),
        serde_json::to_value(&before.messages).unwrap()
    );
    let mut state = ChatGptSyncState::load(service.data_home()).unwrap();
    assert!(
        state.blocked.contains_key(ID),
        "core publication cannot clear the blocker early"
    );
    let blocked = state.source_health("chatgpt", ID, Some(NEW));
    assert_eq!(
        continuation_proof(&after, &blocked, 10, 2, false).state,
        "unverified"
    );
    state.mark_imported_at(ID, Some(NEW));
    state.save(service.data_home()).unwrap();
    let aligned = state.source_health("chatgpt", ID, Some(NEW));
    assert_eq!(
        continuation_proof(&after, &aligned, 10, 2, false).state,
        "verified"
    );
    assert_eq!(
        continuation_proof(&after, &aligned, 0, 2, false).state,
        "unverified"
    );
    assert_eq!(
        continuation_proof(&after, &aligned, 12, 0, false).state,
        "unverified"
    );
    for health_state in [
        chat_history_core::ConversationSourceHealthState::Pending,
        chat_history_core::ConversationSourceHealthState::Stale,
        chat_history_core::ConversationSourceHealthState::Unknown,
    ] {
        let mut health = aligned.clone();
        health.state = health_state;
        assert_eq!(
            continuation_proof(&after, &health, 10, 2, false).state,
            "unverified"
        );
    }
    let conn = chat_history_core::db::open_database(&service.managed_db_path()).unwrap();
    let indexed: String = conn
        .query_row(
            "SELECT transcript_text FROM conversation_fts WHERE conversation_id=?1",
            [ID],
            |row| row.get(0),
        )
        .unwrap();
    assert!(indexed.contains("later refinement 11"));
    let snapshots: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM conversation_snapshots WHERE conversation_id=?1",
            [ID],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        snapshots, 2,
        "old canonical is retained as superseded evidence"
    );
}

#[test]
fn raw_mapping_shape_alone_cannot_spoof_native_export_provenance() {
    let (_temp, service, _request) = fixture();
    let mut detail = service.get_conversation(ID, true).unwrap().unwrap();
    detail.raw_json = Some(json!({"mapping": {}}));

    assert!(
        baseline(&detail, false).is_err(),
        "arbitrary normalized raw.mapping must not establish trusted provenance"
    );
    assert!(
        baseline(&detail, true).is_ok(),
        "native archive provenance may trust the already-parsed canonical snapshot"
    );
}

#[test]
fn every_ambiguous_replay_fails_without_partial_canonical_mutation() {
    for case in 0..15 {
        let (_temp, service, mut request) = fixture();
        let messages = &mut request.transcript.pages[0].messages;
        match case {
            0 => messages[0].truncated = true,
            1 => {
                messages.pop();
            }
            2 => messages.swap(8, 9),
            3 => messages[8].message_id = "diverged".to_string(),
            4 => messages[0].message_id = messages[1].message_id.clone(),
            5 => request.provider_after.update_time = Some(NEW + 1.0),
            6 => request.provider_after.status = Some("active".to_string()),
            7 => messages[7].text = "edited old message".to_string(),
            8 => messages[0].stable_identity = false,
            9 => request.transcript.pages[0].provider_revision = Some(NEW + 1.0),
            10 => messages[8].inaccessible = true,
            11 => request.provider_before.status = Some("unknown".to_string()),
            12 => request.provider_after.observed_at = None,
            13 => request.transcript.pages[0].has_more = true,
            14 => request.transcript.pages[0].provider_revision = None,
            _ => unreachable!(),
        }
        let before = service.get_conversation(ID, true).unwrap().unwrap();
        assert!(
            service.import_verified_continuation(&request).is_err(),
            "case {case}"
        );
        assert_eq!(
            serde_json::to_value(service.get_conversation(ID, true).unwrap()).unwrap(),
            serde_json::to_value(Some(before)).unwrap()
        );
        assert!(
            ChatGptSyncState::load(service.data_home())
                .unwrap()
                .blocked
                .contains_key(ID)
        );
    }
}

#[test]
fn failure_after_canonical_and_fts_writes_rolls_back_the_entire_publication() {
    let (_temp, service, request) = fixture();
    let before = serde_json::to_value(service.get_conversation(ID, true).unwrap()).unwrap();
    let conn = chat_history_core::db::open_database(&service.managed_db_path()).unwrap();
    conn.execute_batch("CREATE TRIGGER reject_repair_completion BEFORE UPDATE ON runs WHEN NEW.run_kind='verified_continuation' AND NEW.status='complete' BEGIN SELECT RAISE(ABORT, 'injected publication failure'); END;").unwrap();
    assert!(service.import_verified_continuation(&request).is_err());
    assert_eq!(
        serde_json::to_value(service.get_conversation(ID, true).unwrap()).unwrap(),
        before
    );
    let snapshots: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM conversation_snapshots WHERE conversation_id=?1",
            [ID],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(snapshots, 1);
    let runs: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM runs WHERE run_kind='verified_continuation'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(runs, 0);
    let transcript: String = conn
        .query_row(
            "SELECT transcript_text FROM conversation_fts WHERE conversation_id=?1",
            [ID],
            |row| row.get(0),
        )
        .unwrap();
    assert!(!transcript.contains("later refinement"));
    assert!(
        ChatGptSyncState::load(service.data_home())
            .unwrap()
            .blocked
            .contains_key(ID)
    );
}

#[test]
fn canonical_and_durable_provider_changes_invalidate_the_publication_baseline() {
    let (_temp, service, request) = fixture();
    let mut changed = request.transcript.clone();
    changed.pages[0].messages.retain(|m| !m.truncated);
    // Simulate another accepted canonical change through normal import.
    changed.pages[0].has_more = false;
    service
        .import_normalized(vec![changed.into_normalized().unwrap()], None)
        .unwrap();
    let current = serde_json::to_value(service.get_conversation(ID, true).unwrap()).unwrap();
    assert!(service.import_verified_continuation(&request).is_err());
    assert_eq!(
        serde_json::to_value(service.get_conversation(ID, true).unwrap()).unwrap(),
        current
    );

    let (_temp, service, request) = fixture();
    let mut state = ChatGptSyncState::load(service.data_home()).unwrap();
    state
        .provider_observations
        .get_mut(ID)
        .unwrap()
        .provider_revision = Some(NEW + 1.0);
    state.save(service.data_home()).unwrap();
    assert!(service.import_verified_continuation(&request).is_err());
    assert_eq!(
        service
            .get_conversation(ID, false)
            .unwrap()
            .unwrap()
            .messages
            .len(),
        10
    );
}
