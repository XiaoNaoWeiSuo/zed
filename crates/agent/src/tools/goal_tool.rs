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

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
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
/// declared source dependencies; inspect rechecks receipts without rerunning tools.
/// Complete only after auditing every original criterion against current evidence.
/// A passed tool and unchanged inputs prove execution and freshness, not semantic
/// coverage. Historical observations must state their limits. Pause on user request;
/// record blocked only when progress needs user input or an external change.
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
    /// One assessment per criterion, in order; required for complete.
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
}

#[derive(Debug, Serialize, Deserialize)]
pub struct GoalToolOutput {
    pub message: String,
    pub goal: Option<ThreadGoal>,
    pub verification_count: usize,
    pub verification: Option<GoalVerificationResult>,
}

impl GoalToolOutput {
    fn error(error: impl ToString) -> Self {
        Self {
            message: error.to_string(),
            goal: None,
            verification_count: 0,
            verification: None,
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
            }),
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
        let (goal_id, project) = self.thread.read_with(cx, |thread, _| {
            let goal = thread
                .goal()
                .ok_or_else(|| anyhow::anyhow!("No goal is set"))?;
            anyhow::ensure!(
                goal.status == ThreadGoalStatus::Active,
                "Resume the goal before verifying"
            );
            anyhow::Ok((goal.id.clone(), thread.project().clone()))
        })??;
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
        };
        let result = GoalVerificationResult {
            receipt_id: receipt.id.clone(),
            output: output.raw_output,
            successful,
            outcome,
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

    async fn audit_receipts(
        &self,
        selected: Option<&[String]>,
        cx: &mut AsyncApp,
    ) -> anyhow::Result<collections::HashSet<String>> {
        let (goal_id, project, receipts) = self.thread.read_with(cx, |thread, _| {
            let goal = thread
                .goal()
                .ok_or_else(|| anyhow::anyhow!("No goal is set"))?;
            let receipts = goal
                .verification_receipts
                .iter()
                .filter(|receipt| selected.is_none_or(|ids| ids.contains(&receipt.id)))
                .cloned()
                .collect::<Vec<_>>();
            anyhow::Ok((goal.id.clone(), thread.project().clone(), receipts))
        })??;
        let mut cached = HashMap::new();
        let mut audited = collections::HashSet::default();
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
            let reason = if receipt.outcome != GoalVerificationOutcome::Passed {
                Some("Verification execution did not pass".to_string())
            } else {
                match cached.get(&paths) {
                    Some(Ok(snapshot)) if snapshot == &receipt.dependencies => {
                        audited.insert(receipt.id.clone());
                        None
                    }
                    Some(Ok(_)) => Some(
                        "Declared dependencies changed since verification; rerun this check".into(),
                    ),
                    Some(Err(error)) => Some(error.clone()),
                    None => Some("Dependency audit is unavailable".into()),
                }
            };
            statuses.push((receipt.id, reason));
        }
        self.thread.update(cx, |thread, cx| {
            thread.update_goal_verification_audit(&goal_id, statuses, cx)
        })??;
        Ok(audited)
    }
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
                    .map_err(GoalToolOutput::error)?
            } else if input.action == "complete" {
                let selected = input
                    .criterion_evidence
                    .iter()
                    .flat_map(|criterion| criterion.verification_ids.iter().cloned())
                    .collect::<Vec<_>>();
                self.audit_receipts(Some(&selected), cx)
                    .await
                    .map_err(GoalToolOutput::error)?
            } else {
                collections::HashSet::default()
            };
            self.thread
                .update(cx, |thread, cx| -> anyhow::Result<_> {
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
                            let mut goal =
                                ThreadGoal::new(input.objective, input.success_criteria)?;
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
                            thread.complete_goal(input.criterion_evidence, &audited, cx)?
                        }
                        "blocked" => {
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
                        "pause" => thread.set_goal_status(ThreadGoalStatus::Paused, None, cx)?,
                        "resume" => thread.set_goal_status(ThreadGoalStatus::Active, None, cx)?,
                        _ => anyhow::bail!("Unknown goal action"),
                    }
                    Ok(GoalToolOutput::snapshot(
                        thread,
                        "Goal state updated".into(),
                        None,
                        input.verification_id.as_deref(),
                        if input.action == "inspect" { 5 } else { 0 },
                    ))
                })
                .map_err(GoalToolOutput::error)?
                .map_err(GoalToolOutput::error)
        })
    }
}
