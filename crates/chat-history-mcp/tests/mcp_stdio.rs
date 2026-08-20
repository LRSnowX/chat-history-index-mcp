use std::{fs, io::Write, process::Stdio};

use chat_history_core::{DataHome, ImportMode, ImportOptions, IndexService};
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

    let transport = TokioChildProcess::builder(
        tokio::process::Command::new(env!("CARGO_BIN_EXE_chat-history-mcp")).configure(|cmd| {
            cmd.env("CHAT_HISTORY_DATA_HOME", &data_home)
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

    client.cancel().await?;
    Ok(())
}
