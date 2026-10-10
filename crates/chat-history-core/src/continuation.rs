//! CHIM-owned, identity-based continuation. No provider text substitutes a trusted anchor.
use anyhow::{anyhow, ensure};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    ChatGptBridgeThread, ChatGptBridgeTranscript, ConversationDetail, ConversationSourceHealth,
    ConversationSourceHealthState, NormalizedConversation,
};

pub const CONTINUATION_METHOD: &str = "ordered-canonical-prefix-v1";
pub const HISTORICAL_DIRECT_METHOD: &str = "historical-transcript-direct-v1";

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ContinuationVerificationScope {
    #[default]
    DiscoveryAligned,
    HistoricalTranscriptDirect,
}

/// Content-free diagnostics: never carry provider text, message IDs or parser output.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum RepairFailureCode {
    TruncatedNewTail,
    MissingOverlap,
    PrefixDivergence,
    AmbiguousIdentity,
    ProviderChanged,
    ReplayIncomplete,
    BaselineChanged,
    InvalidExport,
    ExportIdentity,
    StaleExport,
    LiveBlockRequired,
    PublicationFailed,
}

impl RepairFailureCode {
    pub fn reason(self) -> &'static str {
        match self {
            Self::TruncatedNewTail => {
                "New tail message is truncated; official export recovery is required"
            }
            Self::MissingOverlap => "Trusted ordered overlap is missing",
            Self::PrefixDivergence => {
                "Provider/export prefix differs from trusted canonical history"
            }
            Self::AmbiguousIdentity => "Stable message identity is missing or duplicated",
            Self::ProviderChanged => "Provider identity, revision or eligible status changed",
            Self::ReplayIncomplete => "Replay is inaccessible, malformed or cursor-incomplete",
            Self::BaselineChanged => "Trusted canonical baseline changed before publication",
            Self::InvalidExport => {
                "Official export evidence is malformed, incomplete or unsupported"
            }
            Self::ExportIdentity => "Export conversation identity is wrong, missing or ambiguous",
            Self::StaleExport => {
                "Export revision does not advance beyond the trusted canonical revision"
            }
            Self::LiveBlockRequired => {
                "Offline recovery requires an existing durable live-verification blocker"
            }
            Self::PublicationFailed => {
                "Repair publication or maintenance failed; authority remains unchanged"
            }
        }
    }

    pub fn from_error(error: &anyhow::Error) -> Self {
        error
            .downcast_ref::<Self>()
            .copied()
            .unwrap_or(Self::PublicationFailed)
    }
}

impl std::fmt::Display for RepairFailureCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let code = serde_json::to_value(self).expect("unit enum serialization");
        write!(
            f,
            "CHIM_REPAIR_{}: {}",
            code.as_str().unwrap(),
            self.reason()
        )
    }
}
impl std::error::Error for RepairFailureCode {}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct RepairDiagnostic {
    pub code: RepairFailureCode,
    pub reason: String,
}
impl RepairDiagnostic {
    pub fn new(code: RepairFailureCode) -> Self {
        Self {
            code,
            reason: code.reason().to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct ContinuationBaseline {
    pub conversation_id: String,
    pub source_thread_id: String,
    pub indexed_revision: f64,
    pub total_messages: usize,
    pub baseline_token: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ContinuationImport {
    #[serde(default)]
    pub verification_scope: ContinuationVerificationScope,
    pub baseline: ContinuationBaseline,
    pub provider_before: ChatGptBridgeThread,
    pub provider_after: ChatGptBridgeThread,
    pub transcript_before: ChatGptBridgeThread,
    pub transcript_after: ChatGptBridgeThread,
    pub transcript: ChatGptBridgeTranscript,
}

pub fn baseline(
    detail: &ConversationDetail,
    trusted_export_provenance: bool,
) -> anyhow::Result<ContinuationBaseline> {
    let c = &detail.conversation;
    ensure!(
        c.source == "chatgpt" && c.source_instance.is_none(),
        "Unsupported continuation source"
    );
    let revision = c
        .update_time
        .filter(|v| v.is_finite())
        .ok_or_else(|| anyhow!("Missing canonical revision"))?;
    ensure!(
        !detail.messages.is_empty() && detail.messages.len() as i64 == c.message_count,
        "Missing canonical messages"
    );
    let raw = detail
        .raw_json
        .as_ref()
        .ok_or_else(|| anyhow!("Missing canonical provenance"))?;
    let trusted_bridge =
        raw.get("collector").and_then(serde_json::Value::as_str) == Some("chatgpt-app-bridge-v1");
    ensure!(
        trusted_bridge || trusted_export_provenance,
        "Canonical snapshot has no trusted complete-import provenance"
    );
    let mut ids = std::collections::HashSet::new();
    for (index, m) in detail.messages.iter().enumerate() {
        ensure!(
            m.turn_index == index as i64 && !m.message_id.is_empty() && ids.insert(&m.message_id),
            "Ambiguous canonical message identities/order"
        );
    }
    let bytes = serde_json::to_vec(&(c, &detail.messages, raw))?;
    Ok(ContinuationBaseline {
        conversation_id: c.conversation_id.clone(),
        source_thread_id: c.source_conversation_id.clone(),
        indexed_revision: revision,
        total_messages: detail.messages.len(),
        baseline_token: format!("sha256:{}", hex::encode(Sha256::digest(bytes))),
    })
}

pub(crate) fn verify_baseline(
    detail: &ConversationDetail,
    expected: &ContinuationBaseline,
    trusted_export_provenance: bool,
) -> anyhow::Result<ContinuationBaseline> {
    let current = baseline(detail, trusted_export_provenance)?;
    ensure!(
        current.baseline_token == expected.baseline_token
            && current.conversation_id == expected.conversation_id
            && current.source_thread_id == expected.source_thread_id
            && current.indexed_revision == expected.indexed_revision
            && current.total_messages == expected.total_messages,
        RepairFailureCode::BaselineChanged
    );
    Ok(current)
}

pub fn verify_replay(
    detail: &ConversationDetail,
    request: &ContinuationImport,
    trusted_export_provenance: bool,
) -> anyhow::Result<NormalizedConversation> {
    let current = verify_baseline(detail, &request.baseline, trusted_export_provenance)?;
    let direct_historical =
        request.verification_scope == ContinuationVerificationScope::HistoricalTranscriptDirect;
    if direct_historical {
        ensure!(
            trusted_export_provenance
                && detail
                    .raw_json
                    .as_ref()
                    .and_then(|raw| raw.get("chim_historical_restore"))
                    .and_then(|marker| marker.get("requires_live_verification"))
                    .and_then(serde_json::Value::as_bool)
                    == Some(true),
            RepairFailureCode::ReplayIncomplete
        );
    }
    let before = &request.provider_before;
    let after = &request.provider_after;
    let provider_revision = if direct_historical {
        current.indexed_revision
    } else {
        let revision = before
            .update_time
            .filter(|v| v.is_finite())
            .ok_or(RepairFailureCode::ProviderChanged)?;
        ensure!(
            before.kind == "chatgpt"
                && after.kind == "chatgpt"
                && before.thread_id == current.source_thread_id
                && after.thread_id == current.source_thread_id
                && before.status.as_deref() == Some("idle")
                && after.status.as_deref() == Some("idle")
                && after.update_time == Some(revision),
            RepairFailureCode::ProviderChanged
        );
        ensure!(
            before.observed_at.is_some_and(f64::is_finite)
                && after.observed_at.is_some_and(f64::is_finite)
                && before.observed_at <= after.observed_at,
            RepairFailureCode::ProviderChanged
        );
        revision
    };
    let transcript_before = &request.transcript_before;
    let transcript_after = &request.transcript_after;
    let transcript_revision = transcript_before
        .update_time
        .filter(|v| v.is_finite())
        .ok_or(RepairFailureCode::ProviderChanged)?;
    ensure!(
        transcript_before.kind == "chatgpt"
            && transcript_after.kind == "chatgpt"
            && transcript_before.thread_id == current.source_thread_id
            && transcript_after.thread_id == current.source_thread_id
            && transcript_after.update_time == Some(transcript_revision)
            && if direct_historical {
                transcript_before
                    .status
                    .as_deref()
                    .is_none_or(|status| status == "idle")
                    && transcript_after
                        .status
                        .as_deref()
                        .is_none_or(|status| status == "idle")
            } else {
                transcript_before.status.as_deref() == Some("idle")
                    && transcript_after.status.as_deref() == Some("idle")
            },
        RepairFailureCode::ProviderChanged
    );
    ensure!(
        transcript_before.observed_at.is_some_and(f64::is_finite)
            && transcript_after.observed_at.is_some_and(f64::is_finite)
            && transcript_before.observed_at <= transcript_after.observed_at,
        RepairFailureCode::ProviderChanged
    );
    let mut transcript = request.transcript.clone();
    ensure!(
        transcript.thread_id == current.source_thread_id
            && transcript.update_time == Some(transcript_revision),
        RepairFailureCode::ProviderChanged
    );
    let replay = transcript
        .pages
        .iter()
        .flat_map(|page| page.messages.iter())
        .rev()
        .collect::<Vec<_>>();
    ensure!(
        replay.len() >= detail.messages.len(),
        RepairFailureCode::MissingOverlap
    );
    let mut seen = std::collections::HashSet::new();
    for (index, message) in replay.iter().enumerate() {
        ensure!(
            message.stable_identity
                && !message.message_id.is_empty()
                && seen.insert(&message.message_id),
            RepairFailureCode::AmbiguousIdentity
        );
        ensure!(!message.inaccessible, RepairFailureCode::ReplayIncomplete);
        if let Some(old) = detail.messages.get(index) {
            ensure!(
                message.message_id == old.message_id && message.role == old.role,
                RepairFailureCode::PrefixDivergence
            );
            if !message.truncated {
                if old.raw_message_json.get("content").is_some() {
                    let projection =
                        crate::export_continuation::visible_projection(&old.raw_message_json)?
                            .ok_or(RepairFailureCode::ReplayIncomplete)?;
                    if !projection.citation_identity_only {
                        ensure!(
                            message.text == projection.text,
                            RepairFailureCode::PrefixDivergence
                        );
                    }
                } else {
                    ensure!(
                        message.text == old.normalized_text,
                        RepairFailureCode::PrefixDivergence
                    );
                }
            }
        } else {
            ensure!(!message.truncated, RepairFailureCode::TruncatedNewTail);
        }
    }
    for page in &mut transcript.pages {
        ensure!(
            page.provider_revision == Some(transcript_revision),
            RepairFailureCode::ProviderChanged
        );
        for message in &mut page.messages {
            if let Some(old) = detail
                .messages
                .iter()
                .find(|old| old.message_id == message.message_id)
            {
                message.text = old.normalized_text.clone();
                message.raw = old.raw_message_json.clone();
                message.create_time = old.create_time;
                message.truncated = false;
            }
        }
    }
    // Reuse the full-import cursor-chain, terminal-page and message checks.
    let mut normalized = transcript
        .into_normalized()
        .map_err(|_| RepairFailureCode::ReplayIncomplete)?;
    normalized.source_instance = detail.conversation.source_instance.clone();
    normalized.source_url = detail.conversation.source_url.clone();
    normalized.source_path = detail.conversation.source_path.clone();
    normalized.model = detail.conversation.default_model_slug.clone();
    normalized.create_time = detail.conversation.create_time;
    // Canonical/G-A revision authority is the discovery/list surface. The
    // transcript surface is independently stability-checked above and retained
    // in the audit marker, but its updatedAt is not numerically comparable.
    normalized.update_time = Some(provider_revision);
    normalized.raw = detail.raw_json.clone().unwrap();
    // A successful full identity replay is a complete bridge publication even
    // when its trusted historical prefix originally came from a native export.
    normalized.raw["collector"] = serde_json::json!("chatgpt-app-bridge-v1");
    if let Some(marker) = normalized.raw.get_mut("chim_historical_restore") {
        ensure!(marker.is_object(), RepairFailureCode::ReplayIncomplete);
        marker["requires_live_verification"] = serde_json::json!(false);
        marker["verified_by"] = serde_json::json!(if direct_historical {
            HISTORICAL_DIRECT_METHOD
        } else {
            CONTINUATION_METHOD
        });
        marker["verification_scope"] = serde_json::json!(request.verification_scope);
        if !direct_historical {
            marker["verification_provider_revision"] = serde_json::json!(provider_revision);
        }
        marker["verification_transcript_revision"] = serde_json::json!(transcript_revision);
    }
    let mut continuation_marker = serde_json::json!({
        "method":if direct_historical { HISTORICAL_DIRECT_METHOD } else { CONTINUATION_METHOD },
        "verification_scope":request.verification_scope,
        "baseline_token":current.baseline_token,
        "transcript_before":transcript_before,
        "transcript_after":transcript_after,
        "appended_messages":normalized.messages.len()-detail.messages.len()
    });
    if !direct_historical {
        continuation_marker["provider_before"] = serde_json::json!(before);
        continuation_marker["provider_after"] = serde_json::json!(after);
    }
    normalized.raw["chim_continuation"] = continuation_marker;
    Ok(normalized)
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ContinuationProof {
    pub state: String,
    pub indexed_revision: Option<f64>,
    pub provider_revision: Option<f64>,
    pub observed_at: Option<f64>,
    pub total_messages: usize,
    pub final_message_id: Option<String>,
    pub final_turn_index: Option<i64>,
    pub reason: String,
    pub method: String,
}

pub fn continuation_proof(
    detail: &ConversationDetail,
    health: &ConversationSourceHealth,
    offset: usize,
    returned: usize,
    trusted_export_provenance: bool,
) -> ContinuationProof {
    let final_message = detail.messages.last();
    let reaches_end = returned > 0 && offset.checked_add(returned) == Some(detail.messages.len());
    let eligible = health.state == ConversationSourceHealthState::Aligned
        && health.provider_status.as_deref() == Some("idle")
        && health.indexed_revision == detail.conversation.update_time
        && health.indexed_revision.is_some_and(f64::is_finite)
        && health.provider_revision == health.indexed_revision
        && health.observed_at.is_some_and(f64::is_finite);
    let bounded_identity = final_message.is_some_and(|m| {
        !m.message_id.is_empty()
            && m.message_id.len() <= 256
            && m.turn_index == detail.messages.len() as i64 - 1
    });
    let trusted = baseline(detail, trusted_export_provenance).is_ok();
    let unresolved_restore = detail
        .raw_json
        .as_ref()
        .and_then(|raw| raw.get("chim_historical_restore"))
        .is_some_and(|marker| {
            marker
                .get("requires_live_verification")
                .and_then(serde_json::Value::as_bool)
                != Some(false)
        });
    let direct_historical_verified = trusted_export_provenance
        && detail.raw_json.as_ref().is_some_and(|raw| {
            raw.get("chim_historical_restore").is_some_and(|marker| {
                marker
                    .get("requires_live_verification")
                    .and_then(serde_json::Value::as_bool)
                    == Some(false)
                    && marker
                        .get("verified_by")
                        .and_then(serde_json::Value::as_str)
                        == Some(HISTORICAL_DIRECT_METHOD)
                    && marker
                        .get("verification_scope")
                        .and_then(serde_json::Value::as_str)
                        == Some("historical_transcript_direct")
            }) && raw.get("chim_continuation").is_some_and(|marker| {
                marker.get("method").and_then(serde_json::Value::as_str)
                    == Some(HISTORICAL_DIRECT_METHOD)
                    && marker
                        .get("verification_scope")
                        .and_then(serde_json::Value::as_str)
                        == Some("historical_transcript_direct")
            })
        });
    let verified = (eligible || direct_historical_verified)
        && reaches_end
        && bounded_identity
        && trusted
        && !unresolved_restore;
    ContinuationProof {
        state: if verified { "verified" } else { "unverified" }.to_string(),
        indexed_revision: health.indexed_revision, provider_revision: health.provider_revision, observed_at: health.observed_at,
        total_messages:detail.messages.len(), final_message_id:final_message.filter(|m| m.message_id.len() <= 256).map(|m| m.message_id.clone()), final_turn_index:final_message.map(|m| m.turn_index),
        reason: if direct_historical_verified && verified {
            "Stable direct transcript replay verified historical restoration; discovery health remains independent"
        } else if verified {
            "Eligible source alignment at last observation; returned range reaches trusted canonical end"
        } else {
            "Unaligned, untrusted, ambiguous or non-terminal canonical range"
        }.to_string(),
        method:if direct_historical_verified { HISTORICAL_DIRECT_METHOD } else { CONTINUATION_METHOD }.to_string(),
    }
}
