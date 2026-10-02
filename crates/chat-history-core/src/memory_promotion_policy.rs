use std::collections::{BTreeSet, HashMap};

use anyhow::ensure;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    IndexService, MemoryCandidate, MemoryCandidateOperation, MemoryCandidatePayload,
    MemoryEvidenceKind, MemoryKind,
};

pub const AUTO_PROMOTION_POLICY_VERSION: &str = "conservative-v1";
const MIN_AUTO_IMPORTANCE: u8 = 80;
const MIN_AUTO_CONFIDENCE: f64 = 0.99;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MemoryAutoPromotionClass {
    UserAssertedRule,
    VerifiedOperationalMemory,
    VerifiedOperationalResolution,
    VerifiedArtifactReference,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MemoryAutoPromotionEvaluation {
    pub candidate_id: String,
    pub operation: MemoryCandidateOperation,
    pub eligible: bool,
    pub class: Option<MemoryAutoPromotionClass>,
    pub blocker_codes: Vec<String>,
    pub revalidation_problem: Option<String>,
    pub evidence_kinds: Vec<String>,
    pub importance: Option<u8>,
    pub confidence: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MemoryAutoPromotionPlan {
    pub project: String,
    pub policy_version: String,
    pub pending_count: usize,
    pub eligible_count: usize,
    pub requires_review_count: usize,
    pub evaluations: Vec<MemoryAutoPromotionEvaluation>,
}

impl IndexService {
    pub fn memory_auto_promotion_plan(
        &self,
        project: &str,
    ) -> anyhow::Result<MemoryAutoPromotionPlan> {
        ensure!(!project.trim().is_empty(), "project cannot be empty");
        let candidates = self.pending_memory_candidates(project)?;
        let revalidation_problems = self
            .pending_memory_candidate_revalidation_problems(project)?
            .into_iter()
            .map(|problem| (problem.candidate_id, problem.reason))
            .collect::<HashMap<_, _>>();
        let mut evaluations = Vec::with_capacity(candidates.len());
        for candidate in &candidates {
            evaluations.push(self.evaluate_auto_promotion_candidate(
                candidate,
                revalidation_problems.get(&candidate.candidate_id),
            )?);
        }
        let eligible_count = evaluations.iter().filter(|item| item.eligible).count();
        Ok(MemoryAutoPromotionPlan {
            project: project.to_string(),
            policy_version: AUTO_PROMOTION_POLICY_VERSION.to_string(),
            pending_count: evaluations.len(),
            eligible_count,
            requires_review_count: evaluations.len() - eligible_count,
            evaluations,
        })
    }

    fn evaluate_auto_promotion_candidate(
        &self,
        candidate: &MemoryCandidate,
        revalidation_problem: Option<&String>,
    ) -> anyhow::Result<MemoryAutoPromotionEvaluation> {
        let mut blocker_codes = Vec::new();
        let evidence_kinds = evidence_kind_names(candidate);
        if revalidation_problem.is_some() {
            blocker_codes.push("revalidation_failed".to_string());
        }

        let has_user_statement = has_evidence(candidate, MemoryEvidenceKind::UserStatement);
        let has_operational_evidence = candidate.evidence.iter().any(|evidence| {
            matches!(
                evidence.kind,
                MemoryEvidenceKind::GitCommit
                    | MemoryEvidenceKind::RepositoryState
                    | MemoryEvidenceKind::DevspaceResult
            )
        });
        let has_artifact_evidence = candidate.evidence.iter().any(|evidence| {
            matches!(
                evidence.kind,
                MemoryEvidenceKind::Document
                    | MemoryEvidenceKind::GitCommit
                    | MemoryEvidenceKind::RepositoryState
            )
        });

        let (class, importance, confidence) = match &candidate.payload {
            MemoryCandidatePayload::Add {
                kind,
                importance,
                confidence,
                ..
            }
            | MemoryCandidatePayload::Supersede {
                kind,
                importance,
                confidence,
                ..
            } => {
                if *importance < MIN_AUTO_IMPORTANCE {
                    blocker_codes.push("importance_below_policy_threshold".to_string());
                }
                if *confidence < MIN_AUTO_CONFIDENCE {
                    blocker_codes.push("confidence_below_policy_threshold".to_string());
                }
                let class = classify_new_memory(
                    *kind,
                    has_user_statement,
                    has_operational_evidence,
                    has_artifact_evidence,
                    &mut blocker_codes,
                );
                (class, Some(*importance), Some(*confidence))
            }
            MemoryCandidatePayload::Resolve { target_memory_id } => {
                let class = if let Some(target) = self.get_memory_item(target_memory_id)? {
                    classify_resolution(
                        target.kind,
                        has_user_statement,
                        has_operational_evidence,
                        has_artifact_evidence,
                        &mut blocker_codes,
                    )
                } else {
                    blocker_codes.push("target_memory_unavailable".to_string());
                    None
                };
                (class, None, None)
            }
            MemoryCandidatePayload::Archive { .. } => {
                blocker_codes.push("archive_requires_review".to_string());
                (None, None, None)
            }
        };

        Ok(MemoryAutoPromotionEvaluation {
            candidate_id: candidate.candidate_id.clone(),
            operation: candidate.operation,
            eligible: blocker_codes.is_empty() && class.is_some(),
            class,
            blocker_codes,
            revalidation_problem: revalidation_problem.cloned(),
            evidence_kinds,
            importance,
            confidence,
        })
    }
}

fn classify_new_memory(
    kind: MemoryKind,
    has_user_statement: bool,
    has_operational_evidence: bool,
    has_artifact_evidence: bool,
    blockers: &mut Vec<String>,
) -> Option<MemoryAutoPromotionClass> {
    match kind {
        MemoryKind::Invariant | MemoryKind::Preference | MemoryKind::Decision => {
            if has_user_statement {
                Some(MemoryAutoPromotionClass::UserAssertedRule)
            } else {
                blockers.push("rule_requires_user_statement".to_string());
                None
            }
        }
        MemoryKind::State | MemoryKind::Blocker | MemoryKind::Task | MemoryKind::Result => {
            if has_operational_evidence {
                Some(MemoryAutoPromotionClass::VerifiedOperationalMemory)
            } else {
                blockers.push("operational_memory_requires_verified_evidence".to_string());
                None
            }
        }
        MemoryKind::ArtifactReference => {
            if has_artifact_evidence {
                Some(MemoryAutoPromotionClass::VerifiedArtifactReference)
            } else {
                blockers.push("artifact_reference_requires_verified_evidence".to_string());
                None
            }
        }
        MemoryKind::Hypothesis => {
            blockers.push("hypothesis_requires_review".to_string());
            None
        }
    }
}

fn classify_resolution(
    kind: MemoryKind,
    has_user_statement: bool,
    has_operational_evidence: bool,
    has_artifact_evidence: bool,
    blockers: &mut Vec<String>,
) -> Option<MemoryAutoPromotionClass> {
    match kind {
        MemoryKind::Invariant | MemoryKind::Preference | MemoryKind::Decision => {
            if has_user_statement {
                Some(MemoryAutoPromotionClass::UserAssertedRule)
            } else {
                blockers.push("rule_resolution_requires_user_statement".to_string());
                None
            }
        }
        MemoryKind::State | MemoryKind::Blocker | MemoryKind::Task | MemoryKind::Result => {
            if has_operational_evidence {
                Some(MemoryAutoPromotionClass::VerifiedOperationalResolution)
            } else {
                blockers.push("resolution_requires_verified_operational_evidence".to_string());
                None
            }
        }
        MemoryKind::ArtifactReference => {
            if has_artifact_evidence {
                Some(MemoryAutoPromotionClass::VerifiedArtifactReference)
            } else {
                blockers.push("artifact_resolution_requires_verified_evidence".to_string());
                None
            }
        }
        MemoryKind::Hypothesis => {
            blockers.push("hypothesis_requires_review".to_string());
            None
        }
    }
}

fn has_evidence(candidate: &MemoryCandidate, kind: MemoryEvidenceKind) -> bool {
    candidate
        .evidence
        .iter()
        .any(|evidence| evidence.kind == kind)
}

fn evidence_kind_names(candidate: &MemoryCandidate) -> Vec<String> {
    candidate
        .evidence
        .iter()
        .map(|evidence| evidence.kind.as_str().to_string())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}
