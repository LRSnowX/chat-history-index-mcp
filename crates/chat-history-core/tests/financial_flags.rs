use std::fs;
use std::path::Path;

#[test]
fn summary_prompts_do_not_request_financial_sensitive_flags() {
    let source =
        fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/openai.rs")).unwrap();

    assert!(
        !source.contains("financial_sensitive"),
        "summary prompts should not ask the summarizer to emit financial_sensitive"
    );
}
