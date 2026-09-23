use std::{fs, io::Write, path::Path};

use chat_history_core::embedding::EmbeddingVector;
use chat_history_core::{
    DataHome, ImportMode, ImportOptions, IndexService, SearchMode, SearchOptions, encode_embedding,
};
use serde_json::Value;
use tempfile::TempDir;
use zip::write::SimpleFileOptions;

fn fixture_text(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
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
        let nested_bytes = fs::read(&nested_zip_path)?;
        writer.write_all(&nested_bytes)?;
        writer.start_file("report.html", options)?;
        writer.write_all(b"<html><body>fixture export</body></html>")?;
        writer.finish()?;
    }
    Ok(outer_zip_path)
}

fn seed_embeddings(service: &IndexService) -> anyhow::Result<()> {
    let db_path = service.managed_db_path();
    let conn = chat_history_core::db::open_database(&db_path)?;
    conn.execute(
        "UPDATE conversations SET summary_json = ?2 WHERE conversation_id = ?1",
        rusqlite::params![
            "conv-rust-index",
            serde_json::json!({
                "abstract_text": "A discussion about building a Rust and SQLite archive index.",
                "key_points": ["Rust", "SQLite", "FTS5"],
                "candidate_topics": ["rust", "search"],
                "entities": ["SQLite"],
                "risk_flags": [],
                "site_usefulness": "Useful for public technical writing.",
                "redaction_notes": []
            })
            .to_string()
        ],
    )?;
    conn.execute(
        "UPDATE conversations SET summary_json = ?2 WHERE conversation_id = ?1",
        rusqlite::params![
            "conv-voice-note",
            serde_json::json!({
                "abstract_text": "A conversation with a voice memo attachment and SQLite quote.",
                "key_points": ["audio", "transcript", "SQLite"],
                "candidate_topics": ["audio", "sqlite"],
                "entities": ["SQLite"],
                "risk_flags": [],
                "site_usefulness": "Useful as a multimodal example.",
                "redaction_notes": []
            })
            .to_string()
        ],
    )?;
    conn.execute(
        "UPDATE conversations SET embedding_blob = ?2, embedding_dimensions = 3, embedding_model = 'fixture-model' WHERE conversation_id = ?1",
        rusqlite::params!["conv-rust-index", encode_embedding(&[0.9, 0.1, 0.0])],
    )?;
    conn.execute(
        "UPDATE conversations SET embedding_blob = ?2, embedding_dimensions = 3, embedding_model = 'fixture-model' WHERE conversation_id = ?1",
        rusqlite::params!["conv-voice-note", encode_embedding(&[0.2, 0.8, 0.1])],
    )?;
    conn.execute(
        "INSERT INTO conversation_embedding_chunks (conversation_id, chunk_index, embedding_blob, embedding_dimensions, embedding_model) VALUES (?1, 0, ?2, 3, 'fixture-model')",
        rusqlite::params!["conv-rust-index", encode_embedding(&[0.9, 0.1, 0.0])],
    )?;
    conn.execute(
        "INSERT INTO conversation_embedding_chunks (conversation_id, chunk_index, embedding_blob, embedding_dimensions, embedding_model) VALUES (?1, 0, ?2, 3, 'fixture-model')",
        rusqlite::params!["conv-voice-note", encode_embedding(&[0.2, 0.8, 0.1])],
    )?;
    Ok(())
}

#[tokio::test]
async fn imports_nested_zip_and_supports_search() -> anyhow::Result<()> {
    let tempdir = TempDir::new()?;
    let export_zip = build_fixture_export(&tempdir)?;
    let data_home = tempdir.path().join("managed-data");
    let service = IndexService::new(DataHome::new(data_home), None);

    let report = service
        .import_archive(ImportOptions {
            source_archive: export_zip,
            mode: ImportMode::Copy,
            run_api_jobs: false,
            force_summaries: false,
            force_embeddings: false,
        })
        .await?;

    assert_eq!(report.conversations_indexed, 2);
    assert_eq!(report.attachments_indexed, 1);

    let stats = service.stats()?;
    assert_eq!(stats.conversations, 2);
    assert_eq!(stats.attachments, 1);

    let detail = service
        .get_conversation("conv-voice-note", true)?
        .expect("conversation detail");
    assert_eq!(detail.attachments.len(), 1);
    assert_eq!(
        detail.attachments[0].source_ref,
        "file_00000000abcdef1234567890"
    );
    let raw = detail.raw_json.expect("raw json");
    assert_eq!(
        raw["title"],
        Value::String("Voice note with attachment".to_string())
    );

    let fts_results = service
        .search(SearchOptions {
            query: Some("Rust SQLite FTS5".to_string()),
            mode: Some(SearchMode::Fts),
            limit: Some(5),
            ..SearchOptions::default()
        })
        .await?;
    assert!(!fts_results.is_empty());
    assert_eq!(fts_results[0].conversation_id, "conv-rust-index");

    seed_embeddings(&service)?;

    let conn = chat_history_core::db::open_database(&service.managed_db_path())?;
    let semantic = chat_history_core::search::search(
        &conn,
        SearchOptions {
            query: Some("Rust indexing".to_string()),
            mode: Some(SearchMode::Semantic),
            limit: Some(5),
            ..SearchOptions::default()
        },
        Some(EmbeddingVector {
            values: vec![1.0, 0.0, 0.0],
            model_id: "fixture-model".to_string(),
        }),
    )?;
    assert_eq!(semantic[0].conversation_id, "conv-rust-index");

    let related = service.related_conversations("conv-rust-index", 5)?;
    assert_eq!(related.len(), 1);
    assert_eq!(related[0].conversation_id, "conv-voice-note");

    Ok(())
}
