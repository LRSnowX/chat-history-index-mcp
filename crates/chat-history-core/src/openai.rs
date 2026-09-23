use std::{
    ffi::OsString,
    fs::File,
    io::Write,
    path::PathBuf,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, bail};

use crate::{
    embedding::{EmbeddingVector, LocalEmbeddingClient},
    models::SummaryRecord,
};

const DIRECT_SUMMARY_MAX_CHARS: usize = 16_000;
const CHUNK_SUMMARY_MAX_CHARS: usize = 12_000;
const CODEX_SUMMARY_TIMEOUT: Duration = Duration::from_secs(150);
const CODEX_SYNTHESIS_TIMEOUT: Duration = Duration::from_secs(120);
const CODEX_FALLBACK_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, Clone)]
pub struct OpenAiClient {
    codex_bin: PathBuf,
    summary_model: Option<String>,
    embedding: LocalEmbeddingClient,
}

impl OpenAiClient {
    pub fn from_env(embedding_cache_dir: PathBuf) -> anyhow::Result<Self> {
        let codex_bin = std::env::var_os("CODEX_BIN")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/opt/homebrew/bin/codex"));
        if !codex_bin.exists() {
            bail!("Codex CLI not found at {}", codex_bin.display());
        }
        let summary_model = normalize_summary_model(std::env::var_os("CHAT_HISTORY_SUMMARY_MODEL"));
        let embedding = LocalEmbeddingClient::from_env(embedding_cache_dir)?;
        Ok(Self {
            codex_bin,
            summary_model,
            embedding,
        })
    }

    pub async fn summarize_conversation(&self, transcript: &str) -> anyhow::Result<SummaryRecord> {
        let codex_bin = self.codex_bin.clone();
        let summary_model = self.summary_model.clone();
        let transcript = transcript.to_string();
        tokio::task::spawn_blocking(move || {
            summarize_with_codex(&codex_bin, summary_model.as_deref(), &transcript)
        })
        .await?
    }

    pub fn summary_model_label(&self) -> String {
        match self.summary_model.as_deref() {
            Some(model) => format!("{model} via codex exec"),
            None => "codex-default via codex exec".to_string(),
        }
    }

    pub fn embedding_model_id(&self) -> &'static str {
        self.embedding.model_id()
    }

    pub async fn embed_query(&self, text: &str) -> anyhow::Result<EmbeddingVector> {
        self.embedding.embed_query(text).await
    }

    pub async fn embed_passage(&self, text: &str) -> anyhow::Result<EmbeddingVector> {
        self.embedding.embed_passage(text).await
    }

    pub async fn embed_passage_chunks(&self, text: &str) -> anyhow::Result<Vec<EmbeddingVector>> {
        self.embedding.embed_passage_chunks(text).await
    }
}

fn normalize_summary_model(value: Option<OsString>) -> Option<String> {
    value
        .map(|value| value.to_string_lossy().trim().to_string())
        .filter(|value| !value.is_empty())
}

fn summarize_with_codex(
    codex_bin: &PathBuf,
    summary_model: Option<&str>,
    transcript: &str,
) -> anyhow::Result<SummaryRecord> {
    if transcript.len() <= DIRECT_SUMMARY_MAX_CHARS {
        let transcript = summarize_input_window(transcript, DIRECT_SUMMARY_MAX_CHARS);
        let prompt = build_direct_summary_prompt(&transcript);
        return summarize_with_fallback(
            codex_bin,
            summary_model,
            &prompt,
            transcript.as_str(),
            "direct summary",
        );
    }

    let chunks = split_transcript_chunks(transcript, CHUNK_SUMMARY_MAX_CHARS);
    let mut chunk_summaries = Vec::with_capacity(chunks.len());
    for (index, chunk) in chunks.iter().enumerate() {
        let prompt = build_chunk_summary_prompt(index + 1, chunks.len(), chunk);
        chunk_summaries.push(summarize_with_fallback(
            codex_bin,
            summary_model,
            &prompt,
            chunk,
            &format!("chunk summary {}/{}", index + 1, chunks.len()),
        )?);
    }

    let prompt = build_synthesis_prompt(&chunk_summaries)?;
    parse_summary_json(
        &run_codex_json(
            codex_bin,
            summary_model,
            &prompt,
            CODEX_SYNTHESIS_TIMEOUT,
            "medium",
        )?,
        "synthesized summary",
    )
}

fn build_direct_summary_prompt(transcript: &str) -> String {
    format!(
        "Summarize the following ChatGPT conversation for a local archive index.\n\
         Return JSON only with keys: abstract_text, key_points, candidate_topics, entities, risk_flags, site_usefulness, redaction_notes.\n\
         risk_flags must be an array using only: employer_sensitive, launch_sensitive, personal_sensitive, family_sensitive, mental_health, health_sensitive, legal_sensitive, contains_pii. Do not flag ordinary finance, investing, trading, tax, banking, housing, markets, or resource-allocation topics as sensitive solely because they are financial.\n\
         key_points, candidate_topics, entities, risk_flags, redaction_notes must each be arrays of strings.\n\
         site_usefulness must be a short sentence.\n\
         Keep the abstract under 90 words and keep arrays concise.\n\
         Conversation:\n{transcript}"
    )
}

fn build_chunk_summary_prompt(index: usize, total: usize, transcript_chunk: &str) -> String {
    format!(
        "You are summarizing chunk {index} of {total} from a ChatGPT conversation for later synthesis.\n\
         Return JSON only with keys: abstract_text, key_points, candidate_topics, entities, risk_flags, site_usefulness, redaction_notes.\n\
         risk_flags must be an array using only: employer_sensitive, launch_sensitive, personal_sensitive, family_sensitive, mental_health, health_sensitive, legal_sensitive, contains_pii. Do not flag ordinary finance, investing, trading, tax, banking, housing, markets, or resource-allocation topics as sensitive solely because they are financial.\n\
         key_points, candidate_topics, entities, risk_flags, redaction_notes must each be arrays of strings.\n\
         Focus only on this chunk, keep the abstract under 70 words, and keep arrays concise.\n\
         Conversation chunk:\n{transcript_chunk}"
    )
}

fn build_synthesis_prompt(chunk_summaries: &[SummaryRecord]) -> anyhow::Result<String> {
    let chunk_json = serde_json::to_string_pretty(chunk_summaries)?;
    Ok(format!(
        "Synthesize the following chunk-level summaries into one final conversation summary for a local archive index.\n\
         Return JSON only with keys: abstract_text, key_points, candidate_topics, entities, risk_flags, site_usefulness, redaction_notes.\n\
         risk_flags must be an array using only: employer_sensitive, launch_sensitive, personal_sensitive, family_sensitive, mental_health, health_sensitive, legal_sensitive, contains_pii. Do not flag ordinary finance, investing, trading, tax, banking, housing, markets, or resource-allocation topics as sensitive solely because they are financial.\n\
         Merge overlaps, keep the abstract under 90 words, and keep arrays concise.\n\
         Chunk summaries:\n{chunk_json}"
    ))
}

fn summarize_with_fallback(
    codex_bin: &PathBuf,
    summary_model: Option<&str>,
    prompt: &str,
    transcript: &str,
    context: &str,
) -> anyhow::Result<SummaryRecord> {
    match run_codex_json(
        codex_bin,
        summary_model,
        prompt,
        CODEX_SUMMARY_TIMEOUT,
        "medium",
    )
    .and_then(|json_text| parse_summary_json(&json_text, context))
    {
        Ok(summary) => Ok(summary),
        Err(primary_error) => {
            let condensed = condense_transcript_for_retry(transcript, 24, 220);
            let fallback_prompt = build_direct_summary_prompt(&condensed);
            run_codex_json(
                codex_bin,
                summary_model,
                &fallback_prompt,
                CODEX_FALLBACK_TIMEOUT,
                "low",
            )
            .and_then(|json_text| parse_summary_json(&json_text, &format!("{context} fallback")))
            .with_context(|| format!("{context} failed before fallback: {primary_error}"))
        }
    }
}

fn parse_summary_json(json_text: &str, context: &str) -> anyhow::Result<SummaryRecord> {
    serde_json::from_str(json_text)
        .with_context(|| format!("failed to parse {context} JSON: {json_text}"))
}

fn run_codex_json(
    codex_bin: &PathBuf,
    summary_model: Option<&str>,
    prompt: &str,
    timeout: Duration,
    reasoning_effort: &str,
) -> anyhow::Result<String> {
    let output_file = tempfile::NamedTempFile::new().context("creating Codex output file")?;
    let stdout_file = tempfile::NamedTempFile::new().context("creating Codex stdout file")?;
    let stderr_file = tempfile::NamedTempFile::new().context("creating Codex stderr file")?;
    let output_path = output_file.path().to_path_buf();
    let stdout_path = stdout_file.path().to_path_buf();
    let stderr_path = stderr_file.path().to_path_buf();

    let mut command = build_codex_command(codex_bin, summary_model, &output_path, reasoning_effort);
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::from(
            File::create(&stdout_path).context("opening Codex stdout capture")?,
        ))
        .stderr(Stdio::from(
            File::create(&stderr_path).context("opening Codex stderr capture")?,
        ))
        .spawn()
        .with_context(|| format!("starting Codex CLI at {}", codex_bin.display()))?;

    {
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow::anyhow!("failed to open Codex stdin"))?;
        stdin
            .write_all(prompt.as_bytes())
            .context("writing prompt to Codex stdin")?;
    }

    let start = Instant::now();
    let output = loop {
        if let Some(status) = child.try_wait().context("polling Codex summary run")? {
            let stdout = std::fs::read(&stdout_path).unwrap_or_default();
            let stderr = std::fs::read(&stderr_path).unwrap_or_default();
            break std::process::Output {
                status,
                stdout,
                stderr,
            };
        }
        if start.elapsed() > timeout {
            let _ = child.kill();
            let _ = child.wait();
            bail!(
                "Codex summary run timed out after {} seconds",
                timeout.as_secs()
            );
        }
        thread::sleep(Duration::from_millis(500));
    };
    let raw_message = std::fs::read_to_string(&output_path).unwrap_or_default();
    if !output.status.success() && raw_message.trim().is_empty() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("Codex summary run failed: {}", stderr.trim());
    }
    let json_text = normalize_json_response(&raw_message)
        .or_else(|| normalize_json_response(&String::from_utf8_lossy(&output.stdout)))
        .ok_or_else(|| anyhow::anyhow!("Codex summary output was not valid JSON"))?;
    Ok(json_text)
}

fn build_codex_command(
    codex_bin: &PathBuf,
    summary_model: Option<&str>,
    output_path: &std::path::Path,
    reasoning_effort: &str,
) -> Command {
    let mut command = Command::new(codex_bin);
    command
        .current_dir(std::env::temp_dir())
        .arg("exec")
        .arg("--ephemeral")
        .arg("--skip-git-repo-check")
        .arg("--sandbox")
        .arg("read-only");
    if let Some(model) = summary_model {
        command.arg("--model").arg(model);
    }
    command
        .arg("-c")
        .arg(format!("model_reasoning_effort=\"{reasoning_effort}\""))
        .arg("--output-last-message")
        .arg(output_path)
        .arg("-");
    command
}

fn summarize_input_window(transcript: &str, max_chars: usize) -> String {
    if transcript.len() <= max_chars {
        return transcript.to_string();
    }

    let head_budget = max_chars / 2;
    let tail_budget = max_chars / 2;
    let head = truncate_on_char_boundary(transcript, head_budget);
    let tail = truncate_tail_on_char_boundary(transcript, tail_budget);

    format!("{head}\n\n[conversation truncated for summary; middle content omitted]\n\n{tail}")
}

fn condense_transcript_for_retry(
    transcript: &str,
    max_lines: usize,
    max_chars_per_line: usize,
) -> String {
    let mut lines: Vec<String> = transcript
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| truncate_on_char_boundary(line, max_chars_per_line).to_string())
        .collect();
    if lines.len() <= max_lines {
        return lines.join("\n");
    }

    let head_count = max_lines / 2;
    let tail_count = max_lines - head_count;
    let tail = lines.split_off(lines.len() - tail_count);
    let head = &lines[..head_count];
    let mut condensed = head.to_vec();
    condensed.push("[conversation condensed for retry; middle content omitted]".to_string());
    condensed.extend(tail);
    condensed.join("\n")
}

fn split_transcript_chunks(transcript: &str, max_chars: usize) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut current = String::new();
    for line in transcript.lines() {
        let line_len = line.len() + 1;
        if !current.is_empty() && current.len() + line_len > max_chars {
            chunks.push(std::mem::take(&mut current));
        }
        if line_len > max_chars {
            if !current.is_empty() {
                chunks.push(std::mem::take(&mut current));
            }
            let mut remaining = line;
            while remaining.len() > max_chars {
                let part = truncate_on_char_boundary(remaining, max_chars).to_string();
                let consumed = part.len();
                chunks.push(part);
                remaining = &remaining[consumed..];
            }
            if !remaining.is_empty() {
                current.push_str(remaining);
                current.push('\n');
            }
            continue;
        }
        current.push_str(line);
        current.push('\n');
    }
    if !current.trim().is_empty() {
        chunks.push(current);
    }
    if chunks.is_empty() {
        chunks.push(summarize_input_window(transcript, max_chars));
    }
    chunks
}

fn truncate_on_char_boundary(text: &str, max_len: usize) -> &str {
    if text.len() <= max_len {
        return text;
    }
    let mut end = max_len;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

fn truncate_tail_on_char_boundary(text: &str, max_len: usize) -> &str {
    if text.len() <= max_len {
        return text;
    }
    let mut start = text.len() - max_len;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    &text[start..]
}

fn normalize_json_response(text: &str) -> Option<String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.starts_with('{') && trimmed.ends_with('}') {
        return Some(trimmed.to_string());
    }
    if let Some(stripped) = trimmed
        .strip_prefix("```json")
        .and_then(|value| value.strip_suffix("```"))
    {
        return Some(stripped.trim().to_string());
    }
    if let Some(stripped) = trimmed
        .strip_prefix("```")
        .and_then(|value| value.strip_suffix("```"))
    {
        return Some(stripped.trim().to_string());
    }
    let start = trimmed.find('{')?;
    let end = trimmed.rfind('}')?;
    Some(trimmed[start..=end].to_string())
}

#[cfg(test)]
mod tests {
    use super::{
        build_codex_command, condense_transcript_for_retry, normalize_summary_model,
        split_transcript_chunks, summarize_input_window,
    };
    use std::{ffi::OsString, path::PathBuf};

    #[test]
    fn summary_model_normalizes_unset_empty_and_configured_values() {
        assert_eq!(normalize_summary_model(None), None);
        assert_eq!(normalize_summary_model(Some(OsString::from("   "))), None);
        assert_eq!(
            normalize_summary_model(Some(OsString::from("  gpt-future  "))),
            Some("gpt-future".to_string())
        );
    }

    #[test]
    fn codex_command_omits_model_when_not_configured() {
        let command = build_codex_command(
            &PathBuf::from("/usr/bin/codex"),
            None,
            std::path::Path::new("/tmp/result.json"),
            "medium",
        );
        let args = command
            .get_args()
            .map(|arg| arg.to_string_lossy().to_string())
            .collect::<Vec<_>>();
        assert!(!args.iter().any(|arg| arg == "--model"));
    }

    #[test]
    fn codex_command_passes_configured_model() {
        let command = build_codex_command(
            &PathBuf::from("/usr/bin/codex"),
            Some("gpt-future"),
            std::path::Path::new("/tmp/result.json"),
            "medium",
        );
        let args = command
            .get_args()
            .map(|arg| arg.to_string_lossy().to_string())
            .collect::<Vec<_>>();
        let index = args.iter().position(|arg| arg == "--model").unwrap();
        assert_eq!(args.get(index + 1).map(String::as_str), Some("gpt-future"));
    }

    #[test]
    fn summary_window_truncates_long_transcripts() {
        let transcript = "abc123 ".repeat(12_000);
        let compact = summarize_input_window(&transcript, 48_000);
        assert!(compact.len() < transcript.len());
        assert!(compact.contains("[conversation truncated for summary; middle content omitted]"));
    }

    #[test]
    fn transcript_is_split_into_multiple_chunks() {
        let transcript = (0..4000)
            .map(|index| format!("user: line {index}"))
            .collect::<Vec<_>>()
            .join("\n");
        let chunks = split_transcript_chunks(&transcript, 2_000);
        assert!(chunks.len() > 1);
        assert!(chunks.iter().all(|chunk| chunk.len() <= 2_001));
    }

    #[test]
    fn retry_condensation_keeps_head_and_tail() {
        let transcript = (0..60)
            .map(|index| format!("assistant: line {index}"))
            .collect::<Vec<_>>()
            .join("\n");
        let condensed = condense_transcript_for_retry(&transcript, 10, 40);
        assert!(condensed.contains("assistant: line 0"));
        assert!(condensed.contains("assistant: line 59"));
        assert!(condensed.contains("[conversation condensed for retry; middle content omitted]"));
    }
}
