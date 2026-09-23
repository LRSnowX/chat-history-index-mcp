use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};

use anyhow::{Context, bail};
use fastembed::{EmbeddingModel, TextEmbedding, TextInitOptions};
use sha2::{Digest, Sha256};

pub const MULTILINGUAL_MODEL_ID: &str = "local-multilingual:intfloat/multilingual-e5-small@fastembed-7.1.0:semantic-clean-v1:sampled-chunks64-max-v1";
pub const HASHED_MODEL_ID: &str = "hashed-v1:sha256-token-bigram-256";
const PASSAGE_SAMPLE_CHARS: usize = 3_000;
const MAX_PASSAGE_SAMPLES: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmbeddingProvider {
    LocalMultilingual,
    HashedV1,
}

impl EmbeddingProvider {
    pub fn from_config(value: Option<&str>) -> anyhow::Result<Self> {
        match value.map(str::trim).filter(|value| !value.is_empty()) {
            None | Some("local-multilingual") => Ok(Self::LocalMultilingual),
            Some("hashed-v1") => Ok(Self::HashedV1),
            Some(value) => bail!(
                "unsupported CHAT_HISTORY_EMBEDDING_PROVIDER={value:?}; expected local-multilingual or hashed-v1"
            ),
        }
    }

    pub fn model_id(self) -> &'static str {
        match self {
            Self::LocalMultilingual => MULTILINGUAL_MODEL_ID,
            Self::HashedV1 => HASHED_MODEL_ID,
        }
    }
}

#[derive(Debug, Clone)]
pub struct EmbeddingVector {
    pub values: Vec<f32>,
    pub model_id: String,
}

#[derive(Clone)]
pub struct LocalEmbeddingClient {
    provider: EmbeddingProvider,
    cache_dir: PathBuf,
    multilingual_model: Arc<Mutex<Option<TextEmbedding>>>,
}

impl std::fmt::Debug for LocalEmbeddingClient {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LocalEmbeddingClient")
            .field("provider", &self.provider)
            .field("cache_dir", &self.cache_dir)
            .finish_non_exhaustive()
    }
}

impl LocalEmbeddingClient {
    pub fn from_env(cache_dir: PathBuf) -> anyhow::Result<Self> {
        let provider = EmbeddingProvider::from_config(
            std::env::var("CHAT_HISTORY_EMBEDDING_PROVIDER")
                .ok()
                .as_deref(),
        )?;
        Ok(Self {
            provider,
            cache_dir,
            multilingual_model: Arc::new(Mutex::new(None)),
        })
    }

    pub fn model_id(&self) -> &'static str {
        self.provider.model_id()
    }

    pub async fn embed_query(&self, text: &str) -> anyhow::Result<EmbeddingVector> {
        let mut embeddings = self.embed_inputs(text, EmbeddingInputKind::Query).await?;
        anyhow::ensure!(
            embeddings.len() == 1,
            "query embedding must produce exactly one vector"
        );
        Ok(embeddings.remove(0))
    }

    pub async fn embed_passage(&self, text: &str) -> anyhow::Result<EmbeddingVector> {
        let chunks = self.embed_passage_chunks(text).await?;
        let model_id = chunks
            .first()
            .map(|chunk| chunk.model_id.clone())
            .ok_or_else(|| anyhow::anyhow!("passage embedding returned no chunks"))?;
        let vectors = chunks
            .iter()
            .map(|chunk| chunk.values.clone())
            .collect::<Vec<_>>();
        Ok(EmbeddingVector {
            values: mean_pool_embeddings(&vectors)?,
            model_id,
        })
    }

    pub async fn embed_passage_chunks(&self, text: &str) -> anyhow::Result<Vec<EmbeddingVector>> {
        self.embed_inputs(text, EmbeddingInputKind::Passage).await
    }

    async fn embed_inputs(
        &self,
        text: &str,
        input_kind: EmbeddingInputKind,
    ) -> anyhow::Result<Vec<EmbeddingVector>> {
        match self.provider {
            EmbeddingProvider::HashedV1 => {
                let values = local_hashed_embedding(text);
                ensure_nonzero(&values).context(
                    "hashed-v1 produced no lexical features; use local-multilingual for Chinese or other non-ASCII semantic retrieval",
                )?;
                Ok(vec![EmbeddingVector {
                    values,
                    model_id: HASHED_MODEL_ID.to_string(),
                }])
            }
            EmbeddingProvider::LocalMultilingual => {
                let model = Arc::clone(&self.multilingual_model);
                let cache_dir = self.cache_dir.clone();
                let inputs = input_kind.prefixed_inputs(text);
                let (vectors, revision) = tokio::task::spawn_blocking(move || {
                    let mut guard = model
                        .lock()
                        .map_err(|_| anyhow::anyhow!("multilingual embedding model lock poisoned"))?;
                    if guard.is_none() {
                        std::fs::create_dir_all(&cache_dir).with_context(|| {
                            format!("creating embedding cache at {}", cache_dir.display())
                        })?;
                        let options = TextInitOptions::new(EmbeddingModel::MultilingualE5Small)
                            .with_cache_dir(cache_dir.clone())
                            .with_show_download_progress(false);
                        let initialized = TextEmbedding::try_new(options).context(
                            "initializing multilingual-e5-small; set CHAT_HISTORY_EMBEDDING_PROVIDER=hashed-v1 only if explicit lexical fallback is acceptable",
                        )?;
                        *guard = Some(initialized);
                    }
                    let embeddings = guard
                        .as_mut()
                        .expect("model initialized")
                        .embed(inputs.clone(), Some(inputs.len()))
                        .context("running multilingual-e5-small inference")?;
                    let revision = read_multilingual_model_revision(&cache_dir)?;
                    Ok::<_, anyhow::Error>((embeddings, revision))
                })
                .await??;
                let model_id = format!("{MULTILINGUAL_MODEL_ID}:hf-{revision}");
                vectors
                    .into_iter()
                    .map(|values| {
                        ensure_nonzero(&values)?;
                        Ok(EmbeddingVector {
                            values,
                            model_id: model_id.clone(),
                        })
                    })
                    .collect()
            }
        }
    }
}

fn read_multilingual_model_revision(cache_dir: &std::path::Path) -> anyhow::Result<String> {
    let revision_path = cache_dir
        .join("models--intfloat--multilingual-e5-small")
        .join("refs")
        .join("main");
    let revision = std::fs::read_to_string(&revision_path).with_context(|| {
        format!(
            "reading multilingual-e5-small cache revision at {}",
            revision_path.display()
        )
    })?;
    let revision = revision.trim();
    anyhow::ensure!(
        !revision.is_empty(),
        "multilingual-e5-small cache revision is empty at {}",
        revision_path.display()
    );
    Ok(revision.to_string())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EmbeddingInputKind {
    Query,
    Passage,
}

impl EmbeddingInputKind {
    fn prefixed_inputs(self, text: &str) -> Vec<String> {
        match self {
            Self::Query => vec![format!("query: {text}")],
            Self::Passage => sample_passage_chunks(text, PASSAGE_SAMPLE_CHARS, MAX_PASSAGE_SAMPLES)
                .into_iter()
                .map(|chunk| format!("passage: {chunk}"))
                .collect(),
        }
    }
}

fn sample_passage_chunks(text: &str, chunk_chars: usize, max_samples: usize) -> Vec<String> {
    if text.is_empty() || chunk_chars == 0 || max_samples == 0 {
        return vec![text.to_string()];
    }
    let boundaries = text
        .char_indices()
        .map(|(byte, _)| byte)
        .chain(std::iter::once(text.len()))
        .collect::<Vec<_>>();
    let total_chars = boundaries.len().saturating_sub(1);
    if total_chars <= chunk_chars {
        return vec![text.to_string()];
    }

    let sample_count = max_samples.min(total_chars.div_ceil(chunk_chars)).max(2);
    let half = chunk_chars / 2;
    let mut chunks = Vec::with_capacity(sample_count);
    for index in 0..sample_count {
        let center = index * total_chars.saturating_sub(1) / (sample_count - 1);
        let start_char = center.saturating_sub(half).min(total_chars - chunk_chars);
        let end_char = (start_char + chunk_chars).min(total_chars);
        chunks.push(text[boundaries[start_char]..boundaries[end_char]].to_string());
    }
    chunks.dedup();
    chunks
}

fn mean_pool_embeddings(embeddings: &[Vec<f32>]) -> anyhow::Result<Vec<f32>> {
    let dimensions = embeddings
        .first()
        .map(Vec::len)
        .ok_or_else(|| anyhow::anyhow!("multilingual embedding model returned no vectors"))?;
    anyhow::ensure!(dimensions > 0, "multilingual embedding vector is empty");
    anyhow::ensure!(
        embeddings
            .iter()
            .all(|embedding| embedding.len() == dimensions),
        "multilingual embedding model returned inconsistent dimensions"
    );
    let mut pooled = vec![0.0_f32; dimensions];
    for embedding in embeddings {
        for (target, value) in pooled.iter_mut().zip(embedding) {
            *target += *value;
        }
    }
    let count = embeddings.len() as f32;
    for value in &mut pooled {
        *value /= count;
    }
    normalize_vector(&mut pooled);
    Ok(pooled)
}

fn ensure_nonzero(vector: &[f32]) -> anyhow::Result<()> {
    let norm = vector.iter().map(|value| value * value).sum::<f32>().sqrt();
    anyhow::ensure!(
        norm.is_finite() && norm > 0.0,
        "embedding provider returned a zero or non-finite vector"
    );
    Ok(())
}

fn local_hashed_embedding(text: &str) -> Vec<f32> {
    const DIMENSIONS: usize = 256;
    let mut vector = vec![0.0_f32; DIMENSIONS];
    let tokens = tokenize_ascii(text);
    for token in &tokens {
        apply_hashed_feature(token, 1.0, &mut vector);
    }
    for window in tokens.windows(2) {
        let bigram = format!("{}::{}", window[0], window[1]);
        apply_hashed_feature(&bigram, 1.5, &mut vector);
    }
    normalize_vector(&mut vector);
    vector
}

fn tokenize_ascii(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    for ch in text.chars().flat_map(|ch| ch.to_lowercase()) {
        if ch.is_ascii_alphanumeric() {
            current.push(ch);
        } else if !current.is_empty() {
            if current.len() > 1 {
                tokens.push(std::mem::take(&mut current));
            } else {
                current.clear();
            }
        }
    }
    if current.len() > 1 {
        tokens.push(current);
    }
    tokens
}

fn apply_hashed_feature(token: &str, weight: f32, vector: &mut [f32]) {
    let digest = Sha256::digest(token.as_bytes());
    let index = u64::from_le_bytes([
        digest[0], digest[1], digest[2], digest[3], digest[4], digest[5], digest[6], digest[7],
    ]) as usize
        % vector.len();
    let sign = if digest[8] & 1 == 0 { 1.0 } else { -1.0 };
    vector[index] += sign * weight;
}

fn normalize_vector(vector: &mut [f32]) {
    let norm = vector.iter().map(|value| value * value).sum::<f32>().sqrt();
    if norm > 0.0 {
        for value in vector {
            *value /= norm;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        EmbeddingInputKind, EmbeddingProvider, HASHED_MODEL_ID, LocalEmbeddingClient,
        MULTILINGUAL_MODEL_ID, local_hashed_embedding, mean_pool_embeddings, sample_passage_chunks,
    };
    use crate::ranking::cosine_similarity;

    #[test]
    fn multilingual_is_the_default_provider() {
        assert_eq!(
            EmbeddingProvider::from_config(None).unwrap(),
            EmbeddingProvider::LocalMultilingual
        );
        assert_eq!(
            EmbeddingProvider::from_config(Some("   ")).unwrap(),
            EmbeddingProvider::LocalMultilingual
        );
        assert_eq!(
            EmbeddingProvider::LocalMultilingual.model_id(),
            MULTILINGUAL_MODEL_ID
        );
    }

    #[test]
    fn hashed_provider_is_explicit_legacy_fallback() {
        assert_eq!(
            EmbeddingProvider::from_config(Some("hashed-v1")).unwrap(),
            EmbeddingProvider::HashedV1
        );
        assert_eq!(EmbeddingProvider::HashedV1.model_id(), HASHED_MODEL_ID);
    }

    #[test]
    fn invalid_provider_is_rejected() {
        assert!(EmbeddingProvider::from_config(Some("magic")).is_err());
    }

    #[test]
    fn e5_uses_distinct_query_and_passage_prefixes() {
        assert_eq!(
            EmbeddingInputKind::Query.prefixed_inputs("数据库迁移"),
            vec!["query: 数据库迁移"]
        );
        assert_eq!(
            EmbeddingInputKind::Passage.prefixed_inputs("database migration"),
            vec!["passage: database migration"]
        );
    }

    #[test]
    fn long_passages_are_sampled_across_head_middle_and_tail() {
        let text = (0..20_000)
            .map(|index| char::from(b'a' + (index % 26) as u8))
            .collect::<String>();
        let chunks = sample_passage_chunks(&text, 1_000, 8);
        assert_eq!(chunks.len(), 8);
        assert_eq!(chunks.first().unwrap().len(), 1_000);
        assert_eq!(chunks.last().unwrap().len(), 1_000);
        assert_eq!(chunks.first().unwrap(), &text[..1_000]);
        assert_eq!(chunks.last().unwrap(), &text[text.len() - 1_000..]);
    }

    #[test]
    fn sampled_embedding_vectors_are_mean_pooled_and_normalized() {
        let pooled = mean_pool_embeddings(&[vec![1.0, 0.0], vec![0.0, 1.0]]).unwrap();
        let expected = 1.0_f32 / 2.0_f32.sqrt();
        assert!((pooled[0] - expected).abs() < 1e-5);
        assert!((pooled[1] - expected).abs() < 1e-5);
    }

    #[test]
    fn hashed_embeddings_remain_deterministic_for_legacy_use() {
        let left = local_hashed_embedding("Rust SQLite MCP");
        let right = local_hashed_embedding("Rust SQLite MCP");
        assert_eq!(left, right);
        let norm = left.iter().map(|value| value * value).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-4);
    }

    #[tokio::test]
    async fn hashed_provider_rejects_pure_chinese_zero_vector() {
        let client = LocalEmbeddingClient {
            provider: EmbeddingProvider::HashedV1,
            cache_dir: std::env::temp_dir(),
            multilingual_model: Default::default(),
        };
        let error = client
            .embed_query("为什么旧的数据库迁移记录不能重写")
            .await
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("hashed-v1 produced no lexical features")
        );
    }

    #[tokio::test]
    #[ignore = "downloads/loads multilingual-e5-small for an explicit integration check"]
    async fn multilingual_model_supports_chinese_english_retrieval() {
        let cache_dir = std::env::var_os("CHAT_HISTORY_EMBEDDING_TEST_CACHE")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::env::temp_dir().join("chat-history-fastembed-test"));
        let client = LocalEmbeddingClient {
            provider: EmbeddingProvider::LocalMultilingual,
            cache_dir,
            multilingual_model: std::sync::Arc::new(std::sync::Mutex::new(None)),
        };

        let migration = client
            .embed_passage(
                "Accepted database migration history is immutable. Do not rewrite an old migration; add a new migration for later schema changes.",
            )
            .await
            .unwrap();
        let unrelated = client
            .embed_passage(
                "Compound archery tuning: adjust the arrow rest and sight after checking centershot.",
            )
            .await
            .unwrap();
        let chinese = client
            .embed_query("为什么旧的数据库迁移记录不能重写")
            .await
            .unwrap();
        let english = client
            .embed_query("why did we decide not to rewrite old migrations")
            .await
            .unwrap();

        assert!(migration.model_id.starts_with(MULTILINGUAL_MODEL_ID));
        assert!(migration.model_id.contains(":hf-"));
        assert_eq!(migration.values.len(), chinese.values.len());
        assert_eq!(migration.values.len(), english.values.len());

        let chinese_migration =
            cosine_similarity(&chinese.values, &migration.values).expect("nonzero vectors");
        let chinese_unrelated =
            cosine_similarity(&chinese.values, &unrelated.values).expect("nonzero vectors");
        let english_migration =
            cosine_similarity(&english.values, &migration.values).expect("nonzero vectors");
        let english_unrelated =
            cosine_similarity(&english.values, &unrelated.values).expect("nonzero vectors");

        eprintln!(
            "dimensions={} zh_migration={chinese_migration:.4} zh_unrelated={chinese_unrelated:.4} en_migration={english_migration:.4} en_unrelated={english_unrelated:.4}",
            migration.values.len()
        );
        assert!(chinese_migration > chinese_unrelated);
        assert!(english_migration > english_unrelated);
    }
}
