use chat_history_core::{
    DataHome, IndexService, MemoryItem, MemoryKind, MemoryScope, MemoryStatus,
};
use serde_json::json;
use tempfile::TempDir;

fn service(temp: &TempDir) -> IndexService {
    IndexService::new(DataHome::new(temp.path().join("managed")), None)
}

fn item(
    id: &str,
    kind: MemoryKind,
    key: &str,
    value: &str,
    importance: u8,
    updated_at: f64,
) -> MemoryItem {
    MemoryItem {
        memory_id: id.to_string(),
        scope: MemoryScope::Project {
            project: "LEMonX".to_string(),
        },
        kind,
        key: key.to_string(),
        value: json!({"text": value}),
        status: MemoryStatus::Active,
        importance,
        confidence: 1.0,
        valid_from: None,
        valid_until: None,
        supersedes_memory_id: None,
        created_at: updated_at,
        updated_at,
        last_verified_at: Some(updated_at),
        evidence: Vec::new(),
    }
}

#[test]
fn working_memory_query_ranking_benchmark_covers_core_question_classes() -> anyhow::Result<()> {
    let temp = TempDir::new()?;
    let service = service(&temp);
    for memory in [
        item(
            "accepted-baseline",
            MemoryKind::Result,
            "role_oriented_home_recent_work_checkpoint_a",
            "Checkpoint A is the accepted baseline and repository quality passed.",
            100,
            60.0,
        ),
        item(
            "current-state",
            MemoryKind::State,
            "institutional_onboarding_access_candidate",
            "当前正在开发 institutional onboarding 访问管理候选实现，尚未正式验收。",
            78,
            50.0,
        ),
        item(
            "blocker",
            MemoryKind::Blocker,
            "institutional_onboarding_playwright_blocker",
            "The isolated Playwright rerun is still blocked and needs confirmation.",
            55,
            40.0,
        ),
        item(
            "next-task",
            MemoryKind::Task,
            "resume_institutional_onboarding_acceptance",
            "Resume institutional onboarding acceptance, then run full quality and exact-SHA acceptance.",
            60,
            30.0,
        ),
        item(
            "decision",
            MemoryKind::Decision,
            "memory_promotion_policy",
            "Keep memory candidate promotion manual until conservative automatic policy classes exist.",
            65,
            20.0,
        ),
        item(
            "artifact",
            MemoryKind::ArtifactReference,
            "accepted_design_document",
            "The accepted design document is docs/ROLE_ORIENTED_HOME_RECENT_WORK_ACCEPTANCE_V0.1.md.",
            70,
            10.0,
        ),
    ] {
        service.put_memory_item(&memory)?;
    }

    let cases = [
        ("current state onboarding implementation", "current-state"),
        ("现在的访问管理状态", "current-state"),
        ("playwright blocker", "blocker"),
        ("resume acceptance task", "next-task"),
        ("manual promotion decision", "decision"),
        ("completely unrelated query", "accepted-baseline"),
    ];

    for (query, expected_top) in cases {
        let ranked = service.project_working_memory_for_query("LEMonX", query)?;
        assert_eq!(
            ranked.items.first().map(|memory| memory.memory_id.as_str()),
            Some(expected_top),
            "unexpected top-ranked memory for query {query:?}"
        );
        assert_eq!(
            ranked.items.len(),
            6,
            "query ranking must reorder active memory, never filter it"
        );
    }

    Ok(())
}
