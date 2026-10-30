use std::{collections::HashMap, sync::Arc};

use agent_client_protocol::schema::v2 as acp;
use gpui::{App, AsyncApp, SharedString, Task, WeakEntity};
use language_model::LanguageModelToolResultContent;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use super::goal_verification::capture_goal_dependencies;
use crate::{
    AgentTool, GoalVerificationInput, GoalVerificationOutcome, GoalVerificationReceipt, Thread,
    ThreadGoal, ThreadGoalStatus, ToolCallEventStream, ToolInput,
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum GoalEvidenceScope {
    #[default]
    Workspace,
    Historical,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct GoalCriterionEvidenceInput {
    /// Explain how the observations prove this criterion and what they actually cover.
    pub assessment: String,
    /// Original observation IDs from inspect_tool_result. Bookkeeping cannot prove completion.
    #[serde(default)]
    pub tool_call_ids: Vec<String>,
    /// Receipts returned by manage_goal action=verify. Required for current workspace claims.
    #[serde(default)]
    pub verification_ids: Vec<String>,
    /// Only for historical observations that make no claim about the current workspace.
    #[serde(default)]
    pub historical: bool,
    /// Explicit limits of historical evidence; required when historical=true.
    #[serde(default)]
    pub limitations: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoalCriterionEvidence {
    pub assessment: String,
    pub sources: Vec<GoalEvidenceSource>,
    #[serde(default)]
    pub historical: bool,
    #[serde(default)]
    pub limitations: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoalEvidenceSource {
    pub tool_call_id: String,
    pub tool_name: String,
    pub digest: String,
    #[serde(default)]
    pub verification_id: Option<String>,
}

/// Manage an explicitly requested sustained goal. Define observable criteria,
/// verification surfaces and constraints. Use verify to run an enabled tool with
/// declared source dependencies and `criterion_indices`; inspect rechecks receipts
/// without rerunning tools. Complete audits all receipts automatically and can
/// assemble per-criterion evidence without hand-maintained receipt IDs. A passed
/// tool and unchanged inputs prove execution and freshness, not semantic coverage.
/// Historical observations must state their limits. Keep working on all
/// independent parts of an active goal. Pause only on user request; record blocked
/// only when further progress requires user input or an external change. Model-requested
/// pause, blocked, and resume transitions require user confirmation.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct GoalToolInput {
    /// One of create, inspect, verify, complete, pause, resume, or blocked.
    pub action: String,
    #[serde(default)]
    pub objective: String,
    #[serde(default)]
    pub success_criteria: Vec<String>,
    /// Evidence scope for each criterion at creation. Defaults to workspace. Historical is only for observations independent of current files; scopes cannot be relaxed during completion.
    #[serde(default)]
    pub criterion_scopes: Vec<GoalEvidenceScope>,
    /// Concrete tests, artifacts, or source material that should establish success; required for create.
    #[serde(default)]
    pub verification_surfaces: Vec<String>,
    #[serde(default)]
    pub constraints: Vec<String>,
    /// The reason progress is blocked.
    #[serde(default)]
    pub evidence: String,
    /// Optional assessments, one per criterion in order. Omit this field
    /// entirely to let the harness assemble evidence from current valid
    /// receipts. When provided, `verification_ids` may be omitted and are
    /// filled automatically for the corresponding criterion.
    #[serde(default)]
    pub criterion_evidence: Vec<GoalCriterionEvidenceInput>,
    /// Required for verify. Use the original tool's argument schema and include all relevant source dependencies.
    #[serde(default)]
    pub verification: Option<GoalVerificationInput>,
    /// Inspect a particular receipt, including an older one omitted from the default recent-5 view.
    #[serde(default)]
    pub verification_id: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct GoalVerificationResult {
    pub receipt_id: String,
    pub output: serde_json::Value,
    pub successful: bool,
    pub outcome: GoalVerificationOutcome,
    #[serde(default)]
    pub covered_criteria: Vec<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coverage_note: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct GoalCompletionReport {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub satisfied_criteria: Vec<GoalSatisfiedCriterion>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub missing_evidence: Vec<GoalMissingCriterion>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stale_dependencies: Vec<GoalStaleDependency>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GoalSatisfiedCriterion {
    pub criterion_index: usize,
    pub criterion: String,
    pub verification_ids: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GoalMissingCriterion {
    pub criterion_index: usize,
    pub criterion: String,
    pub reason: String,
    pub suggested_action: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GoalStaleDependency {
    pub receipt_id: String,
    pub criterion_indices: Vec<usize>,
    pub dependencies: Vec<String>,
    pub reason: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct GoalToolOutput {
    pub message: String,
    pub goal: Option<ThreadGoal>,
    pub verification_count: usize,
    pub verification: Option<GoalVerificationResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion: Option<GoalCompletionReport>,
}

#[derive(Clone, Debug)]
struct GoalReceiptAudit {
    id: String,
    valid: bool,
    criterion_indices: Vec<usize>,
}

#[derive(Default)]
struct GoalAudit {
    valid: collections::HashSet<String>,
    receipts: Vec<GoalReceiptAudit>,
    stale_dependencies: Vec<GoalStaleDependency>,
}

impl GoalToolOutput {
    fn error(error: impl ToString) -> Self {
        Self {
            message: error.to_string(),
            goal: None,
            verification_count: 0,
            verification: None,
            completion: None,
        }
    }

    fn snapshot(
        thread: &Thread,
        message: String,
        verification: Option<GoalVerificationResult>,
        selected: Option<&str>,
        recent: usize,
    ) -> Self {
        let mut goal = thread.goal().cloned();
        let verification_count = goal
            .as_ref()
            .map_or(0, |goal| goal.verification_receipts.len());
        if let Some(goal) = goal.as_mut() {
            if let Some(selected) = selected {
                goal.verification_receipts
                    .retain(|receipt| receipt.id == selected);
            } else if let Some(verification) = &verification {
                goal.verification_receipts
                    .retain(|receipt| receipt.id == verification.receipt_id);
            } else {
                goal.verification_receipts = goal
                    .verification_receipts
                    .iter()
                    .rev()
                    .take(recent)
                    .cloned()
                    .map(|mut receipt| {
                        receipt.preview = receipt.preview.chars().take(200).collect();
                        receipt
                    })
                    .collect();
            }
        }
        Self {
            message,
            goal,
            verification_count,
            verification,
            completion: None,
        }
    }
}

impl From<GoalToolOutput> for LanguageModelToolResultContent {
    fn from(output: GoalToolOutput) -> Self {
        let result = (|| -> serde_json::Result<String> {
            let mut value = serde_json::to_value(&output)?;
            if let Some(verification) = value
                .get_mut("verification")
                .and_then(|value| value.as_object_mut())
                && let Some(raw_output) = verification.get("output")
            {
                let text = serde_json::to_string(raw_output)?;
                if text.chars().count() > 8000 {
                    verification.insert(
                        "output".into(),
                        serde_json::Value::String(text.chars().take(8000).collect()),
                    );
                    verification.insert("output_truncated".into(), serde_json::Value::Bool(true));
                }
            }
            serde_json::to_string(&value)
        })();
        result
            .unwrap_or_else(|error| format!("Failed to serialize goal: {error}"))
            .into()
    }
}

#[cfg(test)]
mod completion_tests {
    use super::*;

    fn receipt(id: &str, criterion_indices: Vec<usize>) -> GoalVerificationReceipt {
        GoalVerificationReceipt {
            id: id.into(),
            tool_call_id: format!("call-{id}"),
            tool_name: "test".into(),
            tool_input: serde_json::json!({}),
            observed_at: chrono::Utc::now(),
            dependencies: Vec::new(),
            outcome: GoalVerificationOutcome::Passed,
            command_status: None,
            output_digest: "digest".into(),
            preview: "passed".into(),
            stale_reason: None,
            criterion_indices,
        }
    }

    fn audit(receipts: &[(&str, Vec<usize>, bool)]) -> GoalAudit {
        let mut result = GoalAudit::default();
        for (id, indices, valid) in receipts {
            if *valid {
                result.valid.insert((*id).into());
            }
            result.receipts.push(GoalReceiptAudit {
                id: (*id).into(),
                valid: *valid,
                criterion_indices: indices.clone(),
            });
        }
        result
    }

    #[test]
    fn legacy_receipts_map_to_single_criterion_only() {
        assert_eq!(
            effective_criterion_indices(&receipt("legacy", Vec::new()), 1),
            [0]
        );
        assert!(effective_criterion_indices(&receipt("legacy", Vec::new()), 2).is_empty());
        assert_eq!(
            effective_criterion_indices(&receipt("mapped", vec![1, 1, 9]), 2),
            [1]
        );
    }

    #[test]
    fn completion_auto_associates_current_receipts() {
        let goal = ThreadGoal::new(
            "Ship it".into(),
            vec!["Source compiles".into(), "Tests pass".into()],
        )
        .expect("goal");
        let audit = audit(&[("compile", vec![0], true), ("tests", vec![1], true)]);
        let (evidence, report) = prepare_completion_evidence(&goal, &[], &audit);
        assert!(report.missing_evidence.is_empty(), "{report:?}");
        assert_eq!(report.satisfied_criteria.len(), 2);
        assert_eq!(evidence[0].verification_ids, ["compile"]);
        assert_eq!(evidence[1].verification_ids, ["tests"]);
        assert!(!evidence[0].assessment.trim().is_empty());
    }

    #[test]
    fn completion_reports_missing_evidence_and_affected_stale_receipt() {
        let goal = ThreadGoal::new(
            "Ship it".into(),
            vec!["Source compiles".into(), "Tests pass".into()],
        )
        .expect("goal");
        let mut audit = audit(&[("compile", vec![0], true), ("tests", vec![1], false)]);
        audit.stale_dependencies.push(GoalStaleDependency {
            receipt_id: "tests".into(),
            criterion_indices: vec![1],
            dependencies: vec!["src/lib.rs".into()],
            reason: "Declared dependencies changed since verification".into(),
        });
        let (_, report) = prepare_completion_evidence(&goal, &[], &audit);
        assert_eq!(report.missing_evidence.len(), 1);
        assert_eq!(
            report.missing_evidence[0].reason,
            "No current passing verification receipt covers this criterion"
        );
        assert_eq!(report.missing_evidence[0].criterion_index, 1);
        assert_eq!(report.stale_dependencies.len(), 1);
        assert_eq!(report.stale_dependencies[0].criterion_indices, [1]);
        assert!(
            report
                .message
                .as_deref()
                .is_some_and(|message| message.contains("lack current evidence"))
        );
    }

    #[test]
    fn completion_fills_ids_for_provided_assessment() {
        let goal = ThreadGoal::new("Ship it".into(), vec!["Tests pass".into()]).expect("goal");
        let audit = audit(&[("tests", vec![0], true)]);
        let provided = vec![GoalCriterionEvidenceInput {
            assessment: "The suite passed on the current tree".into(),
            ..Default::default()
        }];
        let (evidence, report) = prepare_completion_evidence(&goal, &provided, &audit);
        assert!(report.missing_evidence.is_empty(), "{report:?}");
        assert_eq!(evidence[0].verification_ids, ["tests"]);
        assert_eq!(
            evidence[0].assessment,
            "The suite passed on the current tree"
        );
    }
}

#[cfg(test)]
mod output_tests {
    use super::*;

    #[test]
    fn verification_output_is_bounded_for_model_but_retained_in_full() {
        let original = "测".repeat(12_000);
        let output = GoalToolOutput {
            message: "recorded".into(),
            goal: None,
            verification_count: 1,
            verification: Some(GoalVerificationResult {
                receipt_id: "receipt".into(),
                output: serde_json::json!(original),
                successful: true,
                outcome: GoalVerificationOutcome::Passed,
                covered_criteria: vec![0],
                coverage_note: None,
            }),
            completion: None,
        };
        let retained = serde_json::to_value(&output).expect("serialize retained result");
        assert_eq!(retained["verification"]["output"], original);
        let LanguageModelToolResultContent::Text(model_output) = output.into() else {
            panic!("expected text result");
        };
        let model_output: serde_json::Value =
            serde_json::from_str(&model_output).expect("model JSON");
        assert_eq!(model_output["verification"]["output_truncated"], true);
        assert!(
            model_output["verification"]["output"]
                .as_str()
                .expect("excerpt")
                .chars()
                .count()
                <= 8000
        );
    }
}

pub struct GoalTool {
    thread: WeakEntity<Thread>,
}

impl GoalTool {
    pub fn new(thread: WeakEntity<Thread>) -> Self {
        Self { thread }
    }

    async fn verify(
        &self,
        input: GoalVerificationInput,
        event_stream: ToolCallEventStream,
        cx: &mut AsyncApp,
    ) -> anyhow::Result<GoalToolOutput> {
        anyhow::ensure!(
            input.tool_input.is_object(),
            "tool_input must be a JSON object matching the original tool schema"
        );
        let (goal_id, project, criterion_count) = self.thread.read_with(cx, |thread, _| {
            let goal = thread
                .goal()
                .ok_or_else(|| anyhow::anyhow!("No goal is set"))?;
            anyhow::ensure!(
                goal.status == ThreadGoalStatus::Active,
                "Resume the goal before verifying"
            );
            anyhow::Ok((
                goal.id.clone(),
                thread.project().clone(),
                goal.success_criteria.len(),
            ))
        })??;
        let mut criterion_indices = input.criterion_indices.clone();
        criterion_indices.sort_unstable();
        criterion_indices.dedup();
        anyhow::ensure!(
            criterion_indices
                .iter()
                .all(|index| *index < criterion_count),
            "criterion_indices must refer to success criteria of the current goal"
        );
        if criterion_indices.is_empty() && criterion_count == 1 {
            criterion_indices.push(0);
        }
        let before = capture_goal_dependencies(&project, &input.dependencies, cx).await?;
        let task = self.thread.read_with(cx, |thread, cx| {
            thread.verification_tool(&input.tool_name, cx)
        })??;
        let title = cx.update(|cx| task.initial_title(input.tool_input.clone(), cx));
        let tool_kind = task.kind();
        event_stream
            .update_fields(|update| update.kind(task.kind()).title(format!("Verify: {title}")));
        let output = cx
            .update(|cx| {
                task.clone().run(
                    ToolInput::ready(input.tool_input.clone()),
                    event_stream.clone(),
                    cx,
                )
            })
            .await;
        let command_status = event_stream.command_status();
        let successful = output.is_ok()
            && if tool_kind == acp::ToolKind::Execute {
                command_status.as_ref().is_some_and(|status| {
                    status.exit_code == Some(0) && !status.timed_out && !status.user_stopped
                })
            } else {
                true
            };
        let output = match output {
            Ok(output) | Err(output) => output,
        };
        let after = capture_goal_dependencies(&project, &input.dependencies, cx).await;
        let stale_reason = match &after {
            Ok(after) if &before == after => None,
            Ok(_) => {
                Some("Dependencies changed during verification; rerun against the new state".into())
            }
            Err(error) => Some(error.to_string()),
        };
        let outcome = if !successful {
            GoalVerificationOutcome::Failed
        } else if stale_reason.is_some() {
            GoalVerificationOutcome::Changed
        } else {
            GoalVerificationOutcome::Passed
        };
        let serialized = serde_json::to_vec(&output.raw_output)?;
        let receipt = GoalVerificationReceipt {
            id: uuid::Uuid::new_v4().to_string(),
            tool_call_id: event_stream.tool_call_id().to_string(),
            tool_name: input.tool_name,
            tool_input: input.tool_input,
            observed_at: chrono::Utc::now(),
            dependencies: before,
            outcome,
            command_status,
            output_digest: format!("{:x}", Sha256::digest(&serialized)),
            preview: String::from_utf8(serialized)?.chars().take(1500).collect(),
            stale_reason,
            criterion_indices: criterion_indices.clone(),
        };
        let coverage_note = if criterion_indices.is_empty() && criterion_count > 1 {
            Some(
                "This receipt is not associated with a criterion; pass verification.criterion_indices to enable automatic completion".into(),
            )
        } else {
            None
        };
        let result = GoalVerificationResult {
            receipt_id: receipt.id.clone(),
            output: output.raw_output,
            successful,
            outcome,
            covered_criteria: criterion_indices,
            coverage_note,
        };
        self.thread.update(cx, |thread, cx| {
            thread.record_goal_verification(&goal_id, receipt, cx)?;
            anyhow::Ok(GoalToolOutput::snapshot(
                thread,
                "Verification recorded; assess its coverage before claiming completion".into(),
                Some(result),
                None,
                0,
            ))
        })?
    }

    fn completion_failure_output(
        &self,
        report: GoalCompletionReport,
        cx: &mut AsyncApp,
    ) -> anyhow::Result<GoalToolOutput> {
        let message = report
            .message
            .clone()
            .unwrap_or_else(|| "Goal is not complete".into());
        let mut output = self.thread.read_with(cx, |thread, _| {
            GoalToolOutput::snapshot(thread, message, None, None, 0)
        })?;
        output.completion = Some(report);
        Ok(output)
    }

    async fn audit_receipts(
        &self,
        selected: Option<&[String]>,
        cx: &mut AsyncApp,
    ) -> anyhow::Result<GoalAudit> {
        let (goal_id, project, criterion_count, receipts) =
            self.thread.read_with(cx, |thread, _| {
                let goal = thread
                    .goal()
                    .ok_or_else(|| anyhow::anyhow!("No goal is set"))?;
                let receipts = goal
                    .verification_receipts
                    .iter()
                    .filter(|receipt| selected.is_none_or(|ids| ids.contains(&receipt.id)))
                    .cloned()
                    .collect::<Vec<_>>();
                anyhow::Ok((
                    goal.id.clone(),
                    thread.project().clone(),
                    goal.success_criteria.len(),
                    receipts,
                ))
            })??;
        let mut cached: HashMap<Vec<String>, Result<Vec<crate::GoalDependencySnapshot>, String>> =
            HashMap::new();
        let mut audit = GoalAudit::default();
        let mut statuses = Vec::new();
        for receipt in receipts {
            let paths = receipt
                .dependencies
                .iter()
                .map(|dependency| dependency.path.clone())
                .collect::<Vec<_>>();
            if !cached.contains_key(&paths) {
                cached.insert(
                    paths.clone(),
                    capture_goal_dependencies(&project, &paths, cx)
                        .await
                        .map_err(|error| error.to_string()),
                );
            }
            let criterion_indices = effective_criterion_indices(&receipt, criterion_count);
            let (reason, changed_dependencies) = if receipt.outcome
                != GoalVerificationOutcome::Passed
            {
                (
                    Some("Verification execution did not pass".to_string()),
                    Vec::new(),
                )
            } else {
                match cached.get(&paths) {
                    Some(Ok(snapshot)) if snapshot == &receipt.dependencies => (None, Vec::new()),
                    Some(Ok(snapshot)) => {
                        let changed = receipt
                            .dependencies
                            .iter()
                            .filter(|before| !snapshot.iter().any(|after| after == *before))
                            .map(|dependency| dependency.path.clone())
                            .collect::<Vec<_>>();
                        (
                            Some(
                                "Declared dependencies changed since verification; rerun this check"
                                    .into(),
                            ),
                            changed,
                        )
                    }
                    Some(Err(error)) => (Some(error.clone()), paths.clone()),
                    None => (
                        Some("Dependency audit is unavailable".into()),
                        paths.clone(),
                    ),
                }
            };
            let valid = reason.is_none();
            if valid {
                audit.valid.insert(receipt.id.clone());
            } else if receipt.outcome == GoalVerificationOutcome::Passed {
                audit.stale_dependencies.push(GoalStaleDependency {
                    receipt_id: receipt.id.clone(),
                    criterion_indices: criterion_indices.clone(),
                    dependencies: if changed_dependencies.is_empty() {
                        paths.clone()
                    } else {
                        changed_dependencies.clone()
                    },
                    reason: reason.clone().unwrap_or_default(),
                });
            }
            audit.receipts.push(GoalReceiptAudit {
                id: receipt.id.clone(),
                valid,
                criterion_indices,
            });
            statuses.push((receipt.id, reason));
        }
        self.thread.update(cx, |thread, cx| {
            thread.update_goal_verification_audit(&goal_id, statuses, cx)
        })??;
        Ok(audit)
    }
}

fn effective_criterion_indices(
    receipt: &GoalVerificationReceipt,
    criterion_count: usize,
) -> Vec<usize> {
    if receipt.criterion_indices.is_empty() {
        return if criterion_count == 1 {
            vec![0]
        } else {
            Vec::new()
        };
    }
    let mut indices = receipt
        .criterion_indices
        .iter()
        .copied()
        .filter(|index| *index < criterion_count)
        .collect::<Vec<_>>();
    indices.sort_unstable();
    indices.dedup();
    indices
}

fn prepare_completion_evidence(
    goal: &ThreadGoal,
    provided: &[GoalCriterionEvidenceInput],
    audit: &GoalAudit,
) -> (Vec<GoalCriterionEvidenceInput>, GoalCompletionReport) {
    let mut report = GoalCompletionReport {
        stale_dependencies: audit.stale_dependencies.clone(),
        ..Default::default()
    };
    if !provided.is_empty() && provided.len() != goal.success_criteria.len() {
        report.message = Some(
            "Provide one evidence entry per criterion, or omit criterion_evidence so the harness can assemble it".into(),
        );
    }
    let mut evidence = Vec::with_capacity(goal.success_criteria.len());
    for (index, criterion) in goal.success_criteria.iter().enumerate() {
        let mut entry = provided.get(index).cloned().unwrap_or_default();
        let scope = goal
            .criterion_scopes
            .get(index)
            .copied()
            .unwrap_or(GoalEvidenceScope::Workspace);
        if scope == GoalEvidenceScope::Workspace && entry.verification_ids.is_empty() {
            let auto_ids = audit
                .receipts
                .iter()
                .rev()
                .filter(|receipt| receipt.valid && receipt.criterion_indices.contains(&index))
                .map(|receipt| receipt.id.clone())
                .collect::<Vec<_>>();
            if !auto_ids.is_empty() {
                entry.verification_ids = auto_ids.clone();
                if entry.assessment.trim().is_empty() {
                    entry.assessment = format!(
                        "Automatically associated with current verification receipt(s): {}.",
                        auto_ids.join(", ")
                    );
                }
            }
        }
        let invalid_ids = entry
            .verification_ids
            .iter()
            .filter(|id| !audit.valid.contains(*id))
            .cloned()
            .collect::<Vec<_>>();
        let missing_reason = if entry.historical != (scope == GoalEvidenceScope::Historical) {
            Some(
                "Evidence scope is fixed when the goal is created; do not downgrade workspace requirements to historical observations"
                    .into(),
            )
        } else if entry.historical && entry.limitations.trim().is_empty() {
            Some("State the limits of historical evidence".into())
        } else if entry.historical
            && entry.tool_call_ids.is_empty()
            && entry.verification_ids.is_empty()
        {
            Some("Historical criterion requires original observations".into())
        } else if !entry.historical && entry.verification_ids.is_empty() {
            Some("No current passing verification receipt covers this criterion".into())
        } else if !invalid_ids.is_empty() {
            Some(format!(
                "Verification {} is missing, failed, or stale; inspect and rerun it",
                invalid_ids.join(", ")
            ))
        } else if entry.assessment.trim().is_empty() {
            Some("Each criterion needs a coverage assessment".to_string())
        } else {
            None
        };
        match missing_reason {
            Some(reason) => report.missing_evidence.push(GoalMissingCriterion {
                criterion_index: index,
                criterion: criterion.clone(),
                reason,
                suggested_action: format!(
                    "Run manage_goal action=verify with verification.criterion_indices=[{index}] for this criterion"
                ),
            }),
            None => report.satisfied_criteria.push(GoalSatisfiedCriterion {
                criterion_index: index,
                criterion: criterion.clone(),
                verification_ids: entry.verification_ids.clone(),
            }),
        }
        evidence.push(entry);
    }
    if !report.missing_evidence.is_empty() && report.message.is_none() {
        report.message = Some(format!(
            "Goal is not complete: {} criteria lack current evidence",
            report.missing_evidence.len()
        ));
    }
    (evidence, report)
}

impl AgentTool for GoalTool {
    type Input = GoalToolInput;
    type Output = GoalToolOutput;
    const NAME: &'static str = "manage_goal";
    fn kind() -> acp::ToolKind {
        acp::ToolKind::Other
    }
    fn initial_title(
        &self,
        input: Result<Self::Input, serde_json::Value>,
        _: &mut App,
    ) -> SharedString {
        match input {
            Ok(input) => format!("Goal: {}", input.action).into(),
            Err(_) => "Manage goal".into(),
        }
    }
    fn run(
        self: Arc<Self>,
        input: ToolInput<Self::Input>,
        event_stream: ToolCallEventStream,
        cx: &mut App,
    ) -> Task<Result<Self::Output, Self::Output>> {
        cx.spawn(async move |cx| {
            let input = input.recv().await.map_err(GoalToolOutput::error)?;
            if input.action == "verify" {
                let verification = input
                    .verification
                    .ok_or_else(|| GoalToolOutput::error("Provide verification arguments"))?;
                let output = self
                    .verify(verification, event_stream, cx)
                    .await
                    .map_err(GoalToolOutput::error)?;
                return if output
                    .verification
                    .as_ref()
                    .is_some_and(|result| result.outcome == GoalVerificationOutcome::Passed)
                {
                    Ok(output)
                } else {
                    Err(output)
                };
            }
            let confirmed_goal_id = if matches!(input.action.as_str(), "pause" | "blocked" | "resume") {
                if input.action == "blocked" && input.evidence.trim().is_empty() {
                    return Err(GoalToolOutput::error("Provide a concrete blocking reason"));
                }
                let goal_id = self.thread.read_with(cx, |thread, _| -> anyhow::Result<_> {
                    let goal = thread.goal().ok_or_else(|| anyhow::anyhow!("No goal is set"))?;
                    if input.action == "resume" {
                        anyhow::ensure!(matches!(goal.status, ThreadGoalStatus::Paused | ThreadGoalStatus::Blocked), "The goal is not paused or blocked");
                    } else {
                        anyhow::ensure!(goal.status == ThreadGoalStatus::Active, "The goal is not active");
                    }
                    Ok(goal.id.clone())
                }).map_err(GoalToolOutput::error)?.map_err(GoalToolOutput::error)?;
                let (question, confirm_label, cancel_label) = match input.action.as_str() {
                    "blocked" => (format!("The agent reports that the goal is blocked: {}. Stop and mark it blocked?", input.evidence), "Stop", "Continue working"),
                    "resume" => ("Resume the goal?".to_string(), "Resume", "Keep paused"),
                    _ => ("Pause the active goal?".to_string(), "Stop", "Continue working"),
                };
                let schema = acp::ElicitationSchema::new().property(
                    "choice",
                    acp::StringPropertySchema::new().title("Choose an option").one_of(vec![
                        acp::EnumOption::new(confirm_label, confirm_label),
                        acp::EnumOption::new(cancel_label, cancel_label),
                    ]),
                    true,
                );
                let response = cx.update(|cx| event_stream.request_elicitation(question, schema, cx))
                    .await.map_err(GoalToolOutput::error)?;
                let confirmed = match response.action {
                    acp::ElicitationAction::Accept(accept) => accept.content
                        .and_then(|content| content.get("choice").cloned())
                        .is_some_and(|choice| matches!(choice, acp::ElicitationContentValue::String(value) if value == confirm_label)),
                    _ => false,
                };
                if !confirmed {
                    let message = if input.action == "resume" {
                        "The goal remains paused. Do not resume it without the user's approval."
                    } else {
                        "The goal remains active. Continue independent work; do not repeat this request without new information."
                    };
                    return Err(GoalToolOutput::error(message));
                }
                Some(goal_id)
            } else {
                None
            };
            let mut completion_evidence = input.criterion_evidence.clone();
            let mut completion_report = None;
            let audited = if input.action == "inspect" {
                let selected = self
                    .thread
                    .read_with(cx, |thread, _| {
                        let goal = thread
                            .goal()
                            .ok_or_else(|| anyhow::anyhow!("No goal is set"))?;
                        if let Some(id) = &input.verification_id {
                            anyhow::ensure!(
                                goal.verification_receipts
                                    .iter()
                                    .any(|receipt| &receipt.id == id),
                                "Unknown verification {id}"
                            );
                            Ok(vec![id.clone()])
                        } else {
                            Ok(goal
                                .verification_receipts
                                .iter()
                                .rev()
                                .take(5)
                                .map(|receipt| receipt.id.clone())
                                .collect())
                        }
                    })
                    .map_err(GoalToolOutput::error)?
                    .map_err(GoalToolOutput::error)?;
                self.audit_receipts(Some(&selected), cx)
                    .await
                    .map_err(GoalToolOutput::error)?;
                collections::HashSet::default()
            } else if input.action == "complete" {
                let audit = self
                    .audit_receipts(None, cx)
                    .await
                    .map_err(GoalToolOutput::error)?;
                let goal = self
                    .thread
                    .read_with(cx, |thread, _| thread.goal().cloned())
                    .map_err(GoalToolOutput::error)?
                    .ok_or_else(|| GoalToolOutput::error("No goal is set"))?;
                let (evidence, report) =
                    prepare_completion_evidence(&goal, &completion_evidence, &audit);
                if report.message.is_some() || !report.missing_evidence.is_empty() {
                    return Err(self
                        .completion_failure_output(report, cx)
                        .map_err(GoalToolOutput::error)?);
                }
                completion_evidence = evidence;
                completion_report = Some(report);
                audit.valid
            } else {
                collections::HashSet::default()
            };
            let updated = self.thread.update(cx, |thread, cx| -> anyhow::Result<_> {
                match input.action.as_str() {
                    "inspect" => {}
                    "create" => {
                        anyhow::ensure!(
                            !input.verification_surfaces.is_empty()
                                && input
                                    .verification_surfaces
                                    .iter()
                                    .all(|surface| !surface.trim().is_empty()),
                            "Describe a concrete verification surface"
                        );
                        anyhow::ensure!(
                            input
                                .constraints
                                .iter()
                                .all(|constraint| !constraint.trim().is_empty()),
                            "Constraints must be non-empty when provided"
                        );
                        let mut goal = ThreadGoal::new(input.objective, input.success_criteria)?;
                        if !input.criterion_scopes.is_empty() {
                            anyhow::ensure!(
                                input.criterion_scopes.len() == goal.success_criteria.len(),
                                "Provide one evidence scope per criterion"
                            );
                            goal.criterion_scopes = input.criterion_scopes;
                        }
                        goal.verification_surfaces = input.verification_surfaces;
                        goal.constraints = input.constraints;
                        thread.set_goal(goal, cx)?;
                    }
                    "complete" => {
                        thread.complete_goal(completion_evidence, &audited, cx)?
                    }
                    "blocked" => {
                        anyhow::ensure!(thread.goal().is_some_and(|goal| Some(&goal.id) == confirmed_goal_id.as_ref() && goal.status == ThreadGoalStatus::Active), "The goal changed while awaiting confirmation");
                        anyhow::ensure!(
                            !input.evidence.trim().is_empty(),
                            "Provide a blocking reason"
                        );
                        thread.set_goal_status(
                            ThreadGoalStatus::Blocked,
                            Some(input.evidence),
                            cx,
                        )?;
                    }
                    "pause" => {
                        anyhow::ensure!(thread.goal().is_some_and(|goal| Some(&goal.id) == confirmed_goal_id.as_ref() && goal.status == ThreadGoalStatus::Active), "The goal changed while awaiting confirmation");
                        thread.set_goal_status(ThreadGoalStatus::Paused, Some("Paused by user".into()), cx)?;
                    }
                    "resume" => {
                        anyhow::ensure!(thread.goal().is_some_and(|goal| Some(&goal.id) == confirmed_goal_id.as_ref() && matches!(goal.status, ThreadGoalStatus::Paused | ThreadGoalStatus::Blocked)), "The goal changed while awaiting confirmation");
                        thread.set_goal_status(ThreadGoalStatus::Active, None, cx)?;
                    }
                    _ => anyhow::bail!("Unknown goal action"),
                }
                Ok(GoalToolOutput::snapshot(
                    thread,
                    "Goal state updated".into(),
                    None,
                    input.verification_id.as_deref(),
                    if input.action == "inspect" { 5 } else { 0 },
                ))
            });
            let mut output = match updated {
                Ok(result) => result.map_err(GoalToolOutput::error)?,
                Err(error) => {
                    let mut report = completion_report.take().unwrap_or_default();
                    report.message = Some(error.to_string());
                    return Err(self
                        .completion_failure_output(report, cx)
                        .map_err(GoalToolOutput::error)?);
                }
            };
            if let Some(report) = completion_report {
                output.completion = Some(report);
            }
            Ok(output)
        })
    }
}
