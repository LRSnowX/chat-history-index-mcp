//! Operator-only, audited stable-ID historical gap restoration. No provider calls.
use crate::continuation::{ContinuationBaseline, baseline};
use crate::export_continuation::{ExportValue, export_messages, validate_shared};
use crate::ingest::{CanonicalContinuationDetail, canonical_detail, stream_json_array};
use crate::models::ConversationMessage;
use crate::{ImportReport, IndexService, NormalizedConversation, NormalizedMessage};
use anyhow::ensure;
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
};

pub const METHOD: &str = "official-export-stable-id-gaps-v1";
const MAX_SOURCES: usize = 8;
const MAX_ENTRIES: usize = 256;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Provenance {
    NativeExport,
    Bridge,
    HistoricalRestore,
    Unknown,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum RestoreCode {
    SafeRestoration,
    SafeRestorationWithLiveTail,
    AlreadyTrustedNative,
    AlreadyComplete,
    SourceMissing,
    InsufficientOverlap,
    ReorderedOverlap,
    AmbiguousGap,
    SourceConflict,
    UnsupportedExport,
    UntrustedCanonical,
    StalePlan,
    BaselineChanged,
    InvalidPlan,
    InvalidSource,
    PublicationFailed,
}
impl std::fmt::Display for RestoreCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "CHIM_HISTORY_RESTORE_{}",
            serde_json::to_value(self).unwrap().as_str().unwrap()
        )
    }
}
impl std::error::Error for RestoreCode {}
fn mask(error: anyhow::Error, fallback: RestoreCode) -> anyhow::Error {
    error
        .downcast_ref::<RestoreCode>()
        .copied()
        .unwrap_or(fallback)
        .into()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SourceEvidence {
    pub path: PathBuf,
    pub sha256: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RestoreEntry {
    pub conversation_id: String,
    pub title: String,
    pub provenance: Provenance,
    pub canonical_revision: Option<f64>,
    pub canonical_count: usize,
    pub baseline: Option<ContinuationBaseline>,
    pub chosen_source: Option<usize>,
    pub export_record_sha256: Option<String>,
    pub export_revision: Option<f64>,
    pub export_visible_count: Option<usize>,
    pub merged_count: Option<usize>,
    pub shared_count: usize,
    pub export_only_count: usize,
    pub canonical_only_count: usize,
    pub merge_relation: Option<String>,
    pub mergeable: bool,
    pub reason: RestoreCode,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RestorePlan {
    pub version: u32,
    pub title_prefix: String,
    pub sources: Vec<SourceEvidence>,
    pub entries: Vec<RestoreEntry>,
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PendingHistoryRestore {
    pub conversation_id: String,
    pub title: String,
    pub update_time: Option<f64>,
}
struct EvidenceFile {
    name: String,
    bytes: Vec<u8>,
}
struct LoadedSource {
    evidence: SourceEvidence,
    files: Vec<EvidenceFile>,
    records: BTreeMap<String, Vec<Value>>,
}

fn digest_files(files: &[EvidenceFile]) -> String {
    let mut hash = Sha256::new();
    hash.update(b"chim-transcript-evidence-v1\0");
    for file in files {
        hash.update((file.name.len() as u64).to_le_bytes());
        hash.update(file.name.as_bytes());
        hash.update((file.bytes.len() as u64).to_le_bytes());
        hash.update(&file.bytes);
    }
    hex::encode(hash.finalize())
}
fn json_name(name: &str) -> bool {
    name.starts_with("conversations-")
        && name.ends_with(".json")
        && !name.contains('/')
        && !name.contains('\\')
}
fn read_source_files(path: &Path) -> anyhow::Result<Vec<EvidenceFile>> {
    let mut files = Vec::new();
    if path.is_dir() {
        // Only transcript shards + exact manifest bytes. Never enumerate/read .dat assets.
        let mut names = vec!["export_manifest.json".to_string()];
        for entry in fs::read_dir(path)? {
            let name = entry?
                .file_name()
                .into_string()
                .map_err(|_| RestoreCode::InvalidSource)?;
            if json_name(&name) {
                names.push(name);
            }
        }
        ensure!(names.len() > 1, RestoreCode::InvalidSource);
        for name in names {
            let file = path.join(&name);
            ensure!(
                fs::symlink_metadata(&file)?.file_type().is_file(),
                RestoreCode::InvalidSource
            );
            files.push(EvidenceFile {
                name,
                bytes: fs::read(file)?,
            });
        }
        let manifest: ExportValue = serde_json::from_slice(&files[0].bytes)?;
        ensure!(
            manifest.0.get("version").and_then(Value::as_u64) == Some(1),
            RestoreCode::InvalidSource
        );
    } else {
        let mut outer = zip::ZipArchive::new(File::open(path)?)?;
        let mut names = Vec::new();
        let mut unique = HashSet::new();
        for i in 0..outer.len() {
            let name = outer.by_index(i)?.name().to_string();
            ensure!(unique.insert(name.clone()), RestoreCode::InvalidSource);
            names.push(name);
        }
        for name in names
            .into_iter()
            .filter(|n| n.contains("Conversations__") && n.ends_with(".zip"))
        {
            let mut temporary = tempfile::NamedTempFile::new()?;
            std::io::copy(&mut outer.by_name(&name)?, &mut temporary)?;
            let mut inner = zip::ZipArchive::new(temporary.as_file())?;
            let mut members = Vec::new();
            let mut unique = HashSet::new();
            for i in 0..inner.len() {
                let member = inner.by_index(i)?.name().to_string();
                ensure!(unique.insert(member.clone()), RestoreCode::InvalidSource);
                members.push(member);
            }
            for member in members.into_iter().filter(|n| json_name(n)) {
                let mut bytes = Vec::new();
                inner.by_name(&member)?.read_to_end(&mut bytes)?;
                files.push(EvidenceFile {
                    name: format!("{name}/{member}"),
                    bytes,
                });
            }
        }
        ensure!(!files.is_empty(), RestoreCode::InvalidSource);
    }
    files.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(files)
}
fn load_sources(paths: &[PathBuf], targets: &HashSet<String>) -> anyhow::Result<Vec<LoadedSource>> {
    ensure!(
        !paths.is_empty() && paths.len() <= MAX_SOURCES,
        RestoreCode::InvalidPlan
    );
    let mut result = Vec::new();
    let mut unique = HashSet::new();
    for path in paths {
        let path =
            fs::canonicalize(path).map_err(|e| mask(e.into(), RestoreCode::InvalidSource))?;
        ensure!(
            path.as_os_str().len() <= 4096 && unique.insert(path.clone()),
            RestoreCode::InvalidSource
        );
        let files = read_source_files(&path).map_err(|e| mask(e, RestoreCode::InvalidSource))?;
        let evidence = SourceEvidence {
            path,
            sha256: digest_files(&files),
        };
        let mut records: BTreeMap<String, Vec<Value>> = BTreeMap::new();
        for file in files.iter().filter(|f| f.name != "export_manifest.json") {
            stream_json_array(
                file.bytes.as_slice(),
                &mut |ExportValue(value): ExportValue| {
                    ensure!(value.is_object(), RestoreCode::InvalidSource);
                    // A contradictory pair of aliases is retained under each matching ID
                    // and rejected by export_messages; never silently prefer one alias.
                    let ids = ["id", "conversation_id"]
                        .iter()
                        .filter_map(|key| value.get(key).and_then(Value::as_str))
                        .filter(|id| targets.contains(*id))
                        .collect::<HashSet<_>>();
                    for id in ids {
                        records
                            .entry(id.to_string())
                            .or_default()
                            .push(value.clone());
                    }
                    Ok(())
                },
            )
            .map_err(|e| mask(e, RestoreCode::InvalidSource))?;
        }
        result.push(LoadedSource {
            evidence,
            files,
            records,
        });
    }
    Ok(result)
}

fn as_canonical(messages: &[NormalizedMessage]) -> Vec<ConversationMessage> {
    messages
        .iter()
        .enumerate()
        .map(|(i, m)| ConversationMessage {
            message_id: m.message_id.clone(),
            conversation_id: String::new(),
            role: m.role.clone(),
            create_time: m.create_time,
            turn_index: i as i64,
            normalized_text: m.text.clone(),
            raw_message_json: m.raw.clone(),
        })
        .collect()
}
struct GapMerge {
    messages: Vec<NormalizedMessage>,
    shared: usize,
    export_only: usize,
    canonical_only: usize,
    canonical_tail: bool,
    relation: String,
}
fn gap_merge(
    export: &[NormalizedMessage],
    canonical: &[ConversationMessage],
) -> anyhow::Result<GapMerge> {
    let positions = canonical
        .iter()
        .enumerate()
        .map(|(i, m)| (m.message_id.as_str(), i))
        .collect::<HashMap<_, _>>();
    ensure!(
        positions.len() == canonical.len(),
        RestoreCode::SourceConflict
    );
    let mut shared = Vec::new();
    let mut export_ids = HashSet::new();
    for (i, m) in export.iter().enumerate() {
        ensure!(
            export_ids.insert(m.message_id.as_str()),
            RestoreCode::SourceConflict
        );
        if let Some(&j) = positions.get(m.message_id.as_str()) {
            if let Some(&(_, previous)) = shared.last() {
                ensure!(j > previous, RestoreCode::ReorderedOverlap);
            }
            validate_shared(&canonical[j], m).map_err(|_| RestoreCode::SourceConflict)?;
            shared.push((i, j));
        }
    }
    ensure!(
        shared.len() >= if canonical.len() == 1 { 1 } else { 2 },
        RestoreCode::InsufficientOverlap
    );
    let canonical_normalized = canonical
        .iter()
        .map(|m| NormalizedMessage {
            message_id: m.message_id.clone(),
            role: m.role.clone(),
            create_time: m.create_time,
            text: m.normalized_text.clone(),
            raw: m.raw_message_json.clone(),
        })
        .collect::<Vec<_>>();
    let mut messages = Vec::new();
    let (mut ei, mut ci) = (0, 0);
    let mut canonical_tail = false;
    for &(e, c) in shared
        .iter()
        .chain(std::iter::once(&(export.len(), canonical.len())))
    {
        ensure!(e == ei || c == ci, RestoreCode::AmbiguousGap);
        messages.extend(export[ei..e].iter().cloned());
        messages.extend(canonical_normalized[ci..c].iter().cloned());
        if e == export.len() {
            canonical_tail = c > ci;
            break;
        }
        messages.push(canonical_normalized[c].clone());
        ei = e + 1;
        ci = c + 1;
    }
    let relation = if shared.len() == canonical.len() {
        if shared.iter().enumerate().all(|(i, (e, _))| *e == i) {
            "prefix"
        } else if shared
            .iter()
            .enumerate()
            .all(|(i, (e, _))| *e == export.len() - canonical.len() + i)
        {
            "suffix"
        } else {
            "ordered_subsequence"
        }
    } else {
        "complementary_gaps"
    };
    Ok(GapMerge {
        messages,
        shared: shared.len(),
        export_only: export.len() - shared.len(),
        canonical_only: canonical.len() - shared.len(),
        canonical_tail,
        relation: relation.to_string(),
    })
}
fn record_hash(raw: &Value) -> String {
    hex::encode(Sha256::digest(serde_json::to_vec(raw).unwrap()))
}

fn reconcile(
    canonical: &CanonicalContinuationDetail,
    sources: &[LoadedSource],
) -> (RestoreEntry, Option<NormalizedConversation>) {
    let d = &canonical.detail;
    let c = &d.conversation;
    let mut entry = RestoreEntry {
        conversation_id: c.conversation_id.clone(),
        title: c.title.chars().take(256).collect(),
        provenance: canonical.provenance,
        canonical_revision: c.update_time,
        canonical_count: d.messages.len(),
        baseline: None,
        chosen_source: None,
        export_record_sha256: None,
        export_revision: None,
        export_visible_count: None,
        merged_count: None,
        shared_count: 0,
        export_only_count: 0,
        canonical_only_count: 0,
        merge_relation: None,
        mergeable: false,
        reason: RestoreCode::UntrustedCanonical,
    };
    let result = (|| -> anyhow::Result<NormalizedConversation> {
        ensure!(
            c.conversation_id.len() <= 256 && c.source_conversation_id.len() <= 256,
            RestoreCode::UntrustedCanonical
        );
        entry.baseline = Some(
            baseline(d, canonical.trusted_export_provenance)
                .map_err(|_| RestoreCode::UntrustedCanonical)?,
        );
        let mut candidates = Vec::new();
        for (i, source) in sources.iter().enumerate() {
            if let Some(records) = source.records.get(&c.source_conversation_id) {
                ensure!(records.len() == 1, RestoreCode::SourceConflict);
                let raw = &records[0];
                let revision = raw
                    .get("update_time")
                    .and_then(Value::as_f64)
                    .filter(|v| v.is_finite())
                    .ok_or(RestoreCode::UnsupportedExport)?;
                candidates.push((i, revision, raw));
            }
        }
        ensure!(!candidates.is_empty(), RestoreCode::SourceMissing);
        candidates.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
        let (source, revision, raw) = candidates[0];
        entry.chosen_source = Some(source);
        entry.export_revision = Some(revision);
        entry.export_record_sha256 = Some(record_hash(raw));
        for candidate in &candidates {
            let id = candidate
                .2
                .get("conversation_id")
                .or_else(|| candidate.2.get("id"))
                .and_then(Value::as_str);
            ensure!(
                id == Some(c.source_conversation_id.as_str())
                    && ["id", "conversation_id"].iter().all(|key| candidate
                        .2
                        .get(key)
                        .is_none_or(|value| value.as_str() == id)),
                RestoreCode::SourceConflict
            );
        }
        // Do not rewrite accepted old native imports merely for a changed projection.
        if canonical.provenance == Provenance::NativeExport
            && c.update_time.is_some_and(|v| v >= revision)
        {
            entry.export_visible_count = export_messages(raw, &c.source_conversation_id)
                .ok()
                .map(|(_, messages)| messages.len());
            return Err(RestoreCode::AlreadyTrustedNative.into());
        }
        for candidate in &candidates {
            if candidate.1 == revision {
                ensure!(
                    record_hash(candidate.2) == record_hash(raw),
                    RestoreCode::SourceConflict
                );
            }
        }
        let (_, export) = export_messages(raw, &c.source_conversation_id)
            .map_err(|_| RestoreCode::UnsupportedExport)?;
        entry.export_visible_count = Some(export.len());
        for candidate in candidates.iter().skip(1).filter(|v| v.1 != revision) {
            let (_, older) = export_messages(candidate.2, &c.source_conversation_id)
                .map_err(|_| RestoreCode::SourceConflict)?;
            gap_merge(&export, &as_canonical(&older)).map_err(|_| RestoreCode::SourceConflict)?;
        }
        let merged = gap_merge(&export, &d.messages)?;
        entry.merged_count = Some(merged.messages.len());
        entry.shared_count = merged.shared;
        entry.export_only_count = merged.export_only;
        entry.canonical_only_count = merged.canonical_only;
        entry.merge_relation = Some(merged.relation);
        ensure!(merged.export_only > 0, RestoreCode::AlreadyComplete);
        entry.reason = if merged.canonical_tail && c.update_time.is_some_and(|v| v > revision) {
            RestoreCode::SafeRestorationWithLiveTail
        } else {
            RestoreCode::SafeRestoration
        };
        entry.mergeable = true;
        let mut raw = raw.clone();
        raw["chim_historical_restore"] = json!({"method":METHOD,"version":1,"source_evidence_sha256":sources[source].evidence.sha256,"prior_canonical_baseline_token":entry.baseline.as_ref().unwrap().baseline_token,"export_revision":revision,"prior_canonical_revision":c.update_time,"export_only_count":merged.export_only,"shared_count":merged.shared,"canonical_only_count":merged.canonical_only,"requires_live_verification":true});
        Ok(NormalizedConversation {
            source: c.source.clone(),
            source_instance: c.source_instance.clone(),
            source_conversation_id: c.source_conversation_id.clone(),
            title: c.title.clone(),
            create_time: c.create_time,
            update_time: Some(c.update_time.unwrap().max(revision)),
            model: c.default_model_slug.clone(),
            source_url: c.source_url.clone(),
            source_path: c.source_path.clone(),
            messages: merged.messages,
            raw,
        })
    })();
    match result {
        Ok(normalized) => (entry, Some(normalized)),
        Err(error) => {
            entry.reason = error
                .downcast_ref::<RestoreCode>()
                .copied()
                .unwrap_or(RestoreCode::UntrustedCanonical);
            (entry, None)
        }
    }
}

impl IndexService {
    pub fn pending_history_restores(
        &self,
        limit: usize,
    ) -> anyhow::Result<Vec<PendingHistoryRestore>> {
        ensure!((1..=64).contains(&limit), RestoreCode::InvalidPlan);
        let conn = Connection::open_with_flags(
            self.managed_db_path(),
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        let mut stmt = conn.prepare(
            r#"
            SELECT c.conversation_id, c.title, c.update_time
            FROM conversations c
            JOIN archives a ON a.id = c.archive_id
            WHERE c.source = 'chatgpt' AND a.import_mode = 'historical_restore'
            ORDER BY c.update_time DESC, c.conversation_id
            LIMIT ?1
            "#,
        )?;
        let rows = stmt.query_map([limit as i64], |row| {
            Ok(PendingHistoryRestore {
                conversation_id: row.get(0)?,
                title: row.get(1)?,
                update_time: row.get(2)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn history_restore_plan(
        &self,
        paths: &[PathBuf],
        prefix: &str,
    ) -> anyhow::Result<RestorePlan> {
        ensure!(
            !prefix.is_empty() && prefix.chars().count() <= 256,
            RestoreCode::InvalidPlan
        );
        // No ensure(), migrations, schema writes, or persistent evidence in planning.
        let conn =
            Connection::open_with_flags(self.managed_db_path(), OpenFlags::SQLITE_OPEN_READ_ONLY)
                .map_err(|e| mask(e.into(), RestoreCode::UntrustedCanonical))?;
        let tx = conn.unchecked_transaction()?;
        let mut statement=tx.prepare("SELECT conversation_id FROM conversations WHERE source='chatgpt' AND source_instance IS NULL AND substr(title,1,length(?1))=?1 ORDER BY conversation_id LIMIT ?2")?;
        let mut ids = statement
            .query_map(rusqlite::params![prefix, (MAX_ENTRIES + 1) as i64], |row| {
                row.get::<_, String>(0)
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let truncated = ids.len() > MAX_ENTRIES;
        ids.truncate(MAX_ENTRIES);
        let canonical = ids
            .iter()
            .map(|id| canonical_detail(&tx, id))
            .collect::<anyhow::Result<Vec<_>>>()
            .map_err(|e| mask(e, RestoreCode::UntrustedCanonical))?;
        let targets = canonical
            .iter()
            .map(|c| c.detail.conversation.source_conversation_id.clone())
            .collect();
        let sources = load_sources(paths, &targets)?;
        let entries = canonical.iter().map(|c| reconcile(c, &sources).0).collect();
        Ok(RestorePlan {
            version: 1,
            title_prefix: prefix.to_string(),
            sources: sources.into_iter().map(|s| s.evidence).collect(),
            entries,
            truncated,
        })
    }

    pub fn apply_history_restore(
        &self,
        plan: &RestorePlan,
        conversation_id: &str,
    ) -> anyhow::Result<ImportReport> {
        self.apply_history_restore_inner(plan, conversation_id)
            .map_err(|e| mask(e, RestoreCode::PublicationFailed))
    }
    fn apply_history_restore_inner(
        &self,
        plan: &RestorePlan,
        id: &str,
    ) -> anyhow::Result<ImportReport> {
        ensure!(
            plan.version == 1 && plan.entries.len() <= MAX_ENTRIES,
            RestoreCode::InvalidPlan
        );
        let entries = plan
            .entries
            .iter()
            .filter(|e| e.conversation_id == id)
            .collect::<Vec<_>>();
        ensure!(entries.len() == 1, RestoreCode::InvalidPlan);
        let expected = entries[0];
        ensure!(
            expected.mergeable && expected.baseline.is_some(),
            RestoreCode::InvalidPlan
        );
        let target = expected.baseline.as_ref().unwrap().source_thread_id.clone();
        let paths = plan
            .sources
            .iter()
            .map(|s| s.path.clone())
            .collect::<Vec<_>>();
        let sources =
            load_sources(&paths, &HashSet::from([target])).map_err(|_| RestoreCode::StalePlan)?;
        ensure!(
            sources
                .iter()
                .map(|s| s.evidence.clone())
                .collect::<Vec<_>>()
                == plan.sources,
            RestoreCode::StalePlan
        );
        let mut conn = crate::db::open_database(&self.managed_db_path())?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let canonical = canonical_detail(&tx, id).map_err(|_| RestoreCode::BaselineChanged)?;
        let current = baseline(&canonical.detail, canonical.trusted_export_provenance)
            .map_err(|_| RestoreCode::BaselineChanged)?;
        ensure!(
            Some(&current) == expected.baseline.as_ref(),
            RestoreCode::BaselineChanged
        );
        let (entry, normalized) = reconcile(&canonical, &sources);
        ensure!(&entry == expected, RestoreCode::StalePlan);
        let mut normalized = normalized.ok_or(RestoreCode::InvalidPlan)?;
        let source = &sources[entry.chosen_source.unwrap()];
        let bundle = persist_bundle(self, source)?;
        normalized.raw["chim_historical_restore"]["bundle_sha256"] = json!(bundle.sha256_hex);
        let report =
            self.publish_history_restore(&tx, normalized, &bundle, &source.evidence.path)?;
        // Recheck mutable inputs immediately before commit, including directory file-set changes.
        for source in &sources {
            let files =
                read_source_files(&source.evidence.path).map_err(|_| RestoreCode::StalePlan)?;
            ensure!(
                digest_files(&files) == source.evidence.sha256,
                RestoreCode::StalePlan
            );
        }
        tx.commit()?;
        Ok(report)
    }
}

fn persist_bundle(
    service: &IndexService,
    source: &LoadedSource,
) -> anyhow::Result<crate::archive::ManagedArchive> {
    let paths = service.data_home().paths();
    paths.ensure()?;
    let mut temporary = tempfile::NamedTempFile::new_in(&paths.sources_dir)?;
    {
        let mut zip = zip::ZipWriter::new(temporary.as_file_mut());
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored)
            .last_modified_time(zip::DateTime::default())
            .unix_permissions(0o600);
        zip.start_file("evidence-format.json", options)?;
        zip.write_all(&serde_json::to_vec(&json!({"format":"chim-historical-transcript-evidence-v1","generated":true,"source_evidence_sha256":source.evidence.sha256}))?)?;
        for (i, file) in source.files.iter().enumerate() {
            zip.start_file(format!("transcripts/{i:04}.json"), options)?;
            zip.write_all(&file.bytes)?;
        }
        zip.start_file("source-files.json", options)?;
        zip.write_all(&serde_json::to_vec(&source.files.iter().enumerate().map(|(i,f)|json!({"member":format!("transcripts/{i:04}.json"),"source_member":f.name,"sha256":hex::encode(Sha256::digest(&f.bytes))})).collect::<Vec<_>>())?)?;
        zip.finish()?;
    }
    let digest = crate::archive::digest_file(temporary.path())?;
    let destination = paths
        .sources_dir
        .join(format!("historical-restore-{}.zip", digest.sha256_hex));
    match temporary.persist_noclobber(&destination) {
        Ok(_) => {}
        Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.error.into()),
    }
    ensure!(
        fs::symlink_metadata(&destination)?.file_type().is_file(),
        RestoreCode::PublicationFailed
    );
    let persisted = crate::archive::digest_file(&destination)?;
    ensure!(
        persisted.sha256_hex == digest.sha256_hex,
        RestoreCode::PublicationFailed
    );
    Ok(persisted)
}
