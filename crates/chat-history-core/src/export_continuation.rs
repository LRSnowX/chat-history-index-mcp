//! Strict lineage validation for the existing official OpenAI export format.
use std::collections::{HashMap, HashSet};

use anyhow::ensure;
use serde::de::{MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer};
use serde_json::{Map, Value};

use crate::continuation::{ContinuationBaseline, RepairFailureCode, verify_baseline};
use crate::{ConversationDetail, NormalizedConversation, NormalizedMessage};

pub(crate) fn verify_export(
    detail: &ConversationDetail,
    expected: &ContinuationBaseline,
    trusted_export_provenance: bool,
    export: Value,
) -> anyhow::Result<NormalizedConversation> {
    let current = verify_baseline(detail, expected, trusted_export_provenance)?;
    let (revision, mut messages) = export_messages(&export, &current.source_thread_id)?;
    // G-B.1 remains append-only with strict revision advancement. G-B.2 reuses
    // parsing and overlap validation without changing this accepted contract.
    ensure!(
        revision > current.indexed_revision,
        RepairFailureCode::StaleExport
    );
    ensure!(
        messages.len() > detail.messages.len(),
        RepairFailureCode::MissingOverlap
    );
    for (old, incoming) in detail.messages.iter().zip(&mut messages) {
        validate_shared(old, incoming)?;
        incoming.text = old.normalized_text.clone();
        incoming.raw = old.raw_message_json.clone();
        incoming.create_time = old.create_time;
    }
    let old = &detail.conversation;
    Ok(NormalizedConversation {
        source: old.source.clone(),
        source_instance: old.source_instance.clone(),
        source_conversation_id: old.source_conversation_id.clone(),
        title: export
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or(&old.title)
            .to_string(),
        create_time: old.create_time,
        update_time: Some(revision),
        model: old.default_model_slug.clone(),
        source_url: old.source_url.clone(),
        source_path: old.source_path.clone(),
        messages,
        raw: export,
    })
}

pub(crate) fn export_messages(
    export: &Value,
    thread_id: &str,
) -> anyhow::Result<(f64, Vec<NormalizedMessage>)> {
    let id = export
        .get("conversation_id")
        .or_else(|| export.get("id"))
        .and_then(Value::as_str);
    ensure!(id == Some(thread_id), RepairFailureCode::ExportIdentity);
    for key in ["id", "conversation_id"] {
        if let Some(value) = export.get(key) {
            ensure!(value.as_str() == id, RepairFailureCode::ExportIdentity);
        }
    }
    let revision = export
        .get("update_time")
        .and_then(Value::as_f64)
        .filter(|v| v.is_finite())
        .ok_or(RepairFailureCode::InvalidExport)?;
    let lineage = active_lineage(export)?;
    let mapping = export
        .get("mapping")
        .and_then(Value::as_object)
        .ok_or(RepairFailureCode::InvalidExport)?;
    // OpenAI's newer official export schema omits message.status globally.
    // Treat that as one explicit export-wide mode, not as a per-message escape
    // hatch: if any message in the mapping carries a status field, every active
    // message must retain the legacy finished_successfully marker.
    let statusless_export = mapping.values().all(|node| match node.get("message") {
        Some(Value::Null) | None => true,
        Some(Value::Object(message)) => !message.contains_key("status"),
        Some(_) => false,
    });
    let mut messages = Vec::new();
    for node in lineage {
        match node.get("message") {
            Some(Value::Null) if node["parent"].is_null() => {}
            Some(message @ Value::Object(_)) => {
                if !statusless_export {
                    ensure!(
                        message.get("status").and_then(Value::as_str)
                            == Some("finished_successfully"),
                        RepairFailureCode::InvalidExport
                    );
                }
                let Some(projection) = visible_projection(message)? else {
                    continue;
                };
                messages.push(NormalizedMessage {
                    message_id: message["id"].as_str().unwrap().to_string(),
                    role: message["author"]["role"].as_str().unwrap().to_string(),
                    create_time: message.get("create_time").and_then(Value::as_f64),
                    text: projection.text,
                    raw: message.clone(),
                });
            }
            _ => return Err(RepairFailureCode::InvalidExport.into()),
        }
    }
    Ok((revision, messages))
}

pub(crate) fn validate_shared(
    old: &crate::models::ConversationMessage,
    incoming: &NormalizedMessage,
) -> anyhow::Result<()> {
    ensure!(
        old.message_id == incoming.message_id && old.role == incoming.role,
        RepairFailureCode::PrefixDivergence
    );
    // Native canonical text is a search projection; compare its complete raw
    // content too, so whitespace/deduplication cannot conceal historical edits.
    if let Some(content) = old.raw_message_json.get("content") {
        ensure!(
            incoming.raw.get("content") == Some(content),
            RepairFailureCode::PrefixDivergence
        );
    } else {
        let projection =
            visible_projection(&incoming.raw)?.ok_or(RepairFailureCode::InvalidExport)?;
        if !projection.citation_identity_only {
            ensure!(
                old.normalized_text == projection.text,
                RepairFailureCode::PrefixDivergence
            );
        }
    }
    Ok(())
}

/// Validate the entire mapping, but publish only the explicitly selected active path.
/// Stable identities are global, including branches and internal nodes.
fn active_lineage(export: &Value) -> anyhow::Result<Vec<&Value>> {
    use RepairFailureCode::{AmbiguousIdentity, InvalidExport};
    let mapping = export
        .get("mapping")
        .and_then(Value::as_object)
        .filter(|m| !m.is_empty())
        .ok_or(InvalidExport)?;
    let end = export
        .get("current_node")
        .and_then(Value::as_str)
        .ok_or(InvalidExport)?;
    let children_present = mapping
        .values()
        .filter(|node| node.get("children").is_some())
        .count();
    ensure!(
        children_present == 0 || children_present == mapping.len(),
        InvalidExport
    );
    let explicit_children = children_present == mapping.len();
    let mut derived_children: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut roots = Vec::new();
    let mut ids = HashSet::new();
    for (key, node) in mapping {
        ensure!(
            !key.trim().is_empty() && node.get("id").and_then(Value::as_str) == Some(key),
            AmbiguousIdentity
        );
        if explicit_children {
            let children = node
                .get("children")
                .and_then(Value::as_array)
                .ok_or(InvalidExport)?;
            let mut unique_children = HashSet::new();
            for child in children {
                let child = child.as_str().ok_or(InvalidExport)?;
                ensure!(unique_children.insert(child), InvalidExport);
                let next = mapping.get(child).ok_or(InvalidExport)?;
                ensure!(
                    next.get("parent").and_then(Value::as_str) == Some(key),
                    InvalidExport
                );
            }
        }
        match node.get("parent") {
            Some(Value::Null) => roots.push(key.as_str()),
            Some(Value::String(parent)) => {
                ensure!(mapping.contains_key(parent), InvalidExport);
                derived_children
                    .entry(parent.as_str())
                    .or_default()
                    .push(key.as_str());
                if explicit_children {
                    let siblings = mapping
                        .get(parent)
                        .and_then(|n| n.get("children"))
                        .and_then(Value::as_array)
                        .ok_or(InvalidExport)?;
                    ensure!(
                        siblings.iter().any(|id| id.as_str() == Some(key)),
                        InvalidExport
                    );
                }
            }
            _ => return Err(InvalidExport.into()),
        }
        match node.get("message") {
            Some(Value::Null) => {}
            Some(Value::Object(message)) => {
                let id = message
                    .get("id")
                    .and_then(Value::as_str)
                    .filter(|id| !id.trim().is_empty())
                    .ok_or(AmbiguousIdentity)?;
                ensure!(ids.insert(id), AmbiguousIdentity);
            }
            _ => return Err(InvalidExport.into()),
        }
    }
    ensure!(roots.len() == 1, InvalidExport);
    // Iterative traversal also rejects disconnected cycles without recursive depth risk.
    let mut visited = HashSet::new();
    let mut stack = vec![roots[0]];
    while let Some(id) = stack.pop() {
        ensure!(visited.insert(id), InvalidExport);
        if explicit_children {
            stack.extend(
                mapping[id]["children"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|id| id.as_str().unwrap()),
            );
        } else if let Some(children) = derived_children.get(id) {
            stack.extend(children.iter().copied());
        }
    }
    ensure!(visited.len() == mapping.len(), InvalidExport);
    ensure!(mapping.contains_key(end), InvalidExport);
    let mut lineage = Vec::new();
    let mut cursor = Some(end);
    while let Some(id) = cursor {
        let node = &mapping[id];
        lineage.push(node);
        cursor = node["parent"].as_str();
    }
    lineage.reverse();
    Ok(lineage)
}

pub(crate) struct VisibleProjection {
    pub text: String,
    pub citation_identity_only: bool,
}

/// One shared export -> App Tools projection for both offline and live overlap.
/// None is reserved for recognized non-visible nodes, never arbitrary content.
pub(crate) fn visible_projection(message: &Value) -> anyhow::Result<Option<VisibleProjection>> {
    use RepairFailureCode::InvalidExport;
    let role = message
        .pointer("/author/role")
        .and_then(Value::as_str)
        .ok_or(InvalidExport)?;
    let content = message
        .get("content")
        .and_then(Value::as_object)
        .ok_or(InvalidExport)?;
    let kind = content
        .get("content_type")
        .and_then(Value::as_str)
        .ok_or(InvalidExport)?;
    if role == "assistant" && matches!(kind, "thoughts" | "reasoning_recap") {
        ensure!(
            match kind {
                "thoughts" => content.get("thoughts").is_some_and(Value::is_array),
                _ => content.get("content").is_some_and(Value::is_string),
            },
            InvalidExport
        );
        return Ok(None);
    }
    let parts = content
        .get("parts")
        .and_then(Value::as_array)
        .filter(|p| !p.is_empty())
        .ok_or(InvalidExport)?;
    let mut text = Vec::new();
    let mut images = HashSet::new();
    for part in parts {
        if let Some(part) = part.as_str() {
            text.push(part);
        } else {
            ensure!(
                role == "user"
                    && kind == "multimodal_text"
                    && part.get("content_type").and_then(Value::as_str)
                        == Some("image_asset_pointer"),
                InvalidExport
            );
            let asset = part
                .get("asset_pointer")
                .and_then(Value::as_str)
                .filter(|id| !id.trim().is_empty())
                .ok_or(InvalidExport)?;
            ensure!(images.insert(asset), InvalidExport);
        }
    }
    // Preserve whitespace and repeated text parts. Do not use the search projection.
    let mut text = text.join("\n");
    let metadata = match message.get("metadata") {
        None | Some(Value::Null) => None,
        Some(Value::Object(metadata)) => Some(metadata),
        _ => return Err(InvalidExport.into()),
    };
    let references = match metadata.and_then(|m| m.get("content_references")) {
        None | Some(Value::Null) => false,
        Some(Value::Array(references)) => {
            for reference in references {
                let reference = reference.as_object().ok_or(InvalidExport)?;
                ensure!(
                    reference
                        .get("type")
                        .and_then(Value::as_str)
                        .is_some_and(|kind| !kind.trim().is_empty()),
                    InvalidExport
                );
            }
            !references.is_empty()
        }
        _ => return Err(InvalidExport.into()),
    };
    let attachments = match metadata.and_then(|m| m.get("attachments")) {
        None | Some(Value::Null) => &[][..],
        Some(Value::Array(attachments)) => attachments.as_slice(),
        _ => return Err(InvalidExport.into()),
    };
    if role == "system"
        && kind == "text"
        && text.is_empty()
        && attachments.is_empty()
        && !references
    {
        return Ok(None); // Existing empty scaffold contract.
    }
    ensure!(matches!(role, "user" | "assistant"), InvalidExport);
    ensure!(
        kind == "text" || (role == "user" && kind == "multimodal_text" && !images.is_empty()),
        InvalidExport
    );
    ensure!(role == "user" || attachments.is_empty(), InvalidExport);
    let mut attachment_ids = HashSet::new();
    for attachment in attachments {
        let id = attachment
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.trim().is_empty())
            .ok_or(InvalidExport)?;
        let mime = attachment
            .get("mime_type")
            .and_then(Value::as_str)
            .ok_or(InvalidExport)?;
        ensure!(attachment_ids.insert(id), InvalidExport);
        // Mixed image/file payloads have no evidenced deterministic rendering here.
        ensure!(
            mime.starts_with("image/") == !images.is_empty(),
            InvalidExport
        );
    }
    if !images.is_empty() {
        ensure!(
            attachments.is_empty() || attachments.len() == images.len(),
            InvalidExport
        );
        if !attachments.is_empty() {
            let pointer_ids = images
                .iter()
                .map(|pointer| {
                    pointer
                        .split_once("://")
                        .map(|(_, id)| id)
                        .filter(|id| !id.is_empty() && !id.contains('/'))
                        .ok_or(InvalidExport)
                })
                .collect::<Result<HashSet<_>, _>>()?;
            ensure!(pointer_ids == attachment_ids, InvalidExport);
        }
        append_attachment_marker(&mut text, images.len(), "image");
    } else if !attachments.is_empty() {
        append_attachment_marker(&mut text, attachments.len(), "file");
    }
    Ok(Some(VisibleProjection {
        text,
        citation_identity_only: role == "assistant" && kind == "text" && references,
    }))
}

fn append_attachment_marker(text: &mut String, count: usize, kind: &str) {
    if !text.is_empty() {
        text.push_str("\n\n");
    }
    text.push_str(&format!(
        "[User attached {count} {kind}{}; {kind} contents were not included]",
        if count == 1 { "" } else { "s" }
    ));
}

/// serde_json::Value otherwise silently overwrites duplicate JSON object keys.
pub(crate) struct ExportValue(pub Value);
impl<'de> Deserialize<'de> for ExportValue {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct StrictVisitor;
        impl<'de> Visitor<'de> for StrictVisitor {
            type Value = ExportValue;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("unambiguous export JSON")
            }
            fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
                Ok(ExportValue(Value::Null))
            }
            fn visit_bool<E: serde::de::Error>(self, v: bool) -> Result<Self::Value, E> {
                Ok(ExportValue(v.into()))
            }
            fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<Self::Value, E> {
                Ok(ExportValue(v.into()))
            }
            fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<Self::Value, E> {
                Ok(ExportValue(v.into()))
            }
            fn visit_f64<E: serde::de::Error>(self, v: f64) -> Result<Self::Value, E> {
                Ok(ExportValue(v.into()))
            }
            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Self::Value, E> {
                Ok(ExportValue(v.into()))
            }
            fn visit_string<E: serde::de::Error>(self, v: String) -> Result<Self::Value, E> {
                Ok(ExportValue(v.into()))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                let mut values = Vec::new();
                while let Some(ExportValue(value)) = seq.next_element()? {
                    values.push(value);
                }
                Ok(ExportValue(Value::Array(values)))
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut values = Map::new();
                let mut seen = HashMap::new();
                while let Some((key, ExportValue(value))) =
                    map.next_entry::<String, ExportValue>()?
                {
                    if seen.insert(key.clone(), ()).is_some() {
                        return Err(serde::de::Error::custom("duplicate export JSON key"));
                    }
                    values.insert(key, value);
                }
                Ok(ExportValue(Value::Object(values)))
            }
        }
        d.deserialize_any(StrictVisitor)
    }
}
