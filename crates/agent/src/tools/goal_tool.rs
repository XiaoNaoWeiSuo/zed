use std::sync::Arc;

use agent_client_protocol::schema::v2 as acp;
use gpui::{App, SharedString, Task, WeakEntity};
use language_model::LanguageModelToolResultContent;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{AgentTool, Thread, ThreadGoal, ThreadGoalStatus, ToolCallEventStream, ToolInput};

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct GoalCriterionEvidenceInput {
    /// Explain how the cited observations prove this criterion, including any limits.
    pub assessment: String,
    /// Completed tool call IDs from inspect_tool_result. Failed calls and goal-management calls are not completion evidence.
    pub tool_call_ids: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoalCriterionEvidence {
    pub assessment: String,
    pub sources: Vec<GoalEvidenceSource>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoalEvidenceSource {
    pub tool_call_id: String,
    pub tool_name: String,
    pub digest: String,
}

/// Manage the current session goal. Create a goal only when the user explicitly
/// requests sustained goal-directed work. Define concrete, verifiable success
/// criteria, and continue until every criterion is verified. Report evidence
/// when completing it. Pause only on the user's request. Use blocked only when
/// progress cannot continue without user input or an external change.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct GoalToolInput {
    /// One of create, inspect, complete, pause, resume, or blocked.
    pub action: String,
    /// The objective, required for create.
    #[serde(default)]
    pub objective: String,
    /// Observable conditions that must all be satisfied, required for create.
    #[serde(default)]
    pub success_criteria: Vec<String>,
    /// Tests, artifacts, or source material that should establish success.
    #[serde(default)]
    pub verification_surfaces: Vec<String>,
    /// Conditions that must remain true while pursuing the goal.
    #[serde(default)]
    pub constraints: Vec<String>,
    /// The reason progress is blocked.
    #[serde(default)]
    pub evidence: String,
    /// Evidence for each success criterion, in the same order; required for completion.
    #[serde(default)]
    pub criterion_evidence: Vec<GoalCriterionEvidenceInput>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct GoalToolOutput {
    pub message: String,
    pub goal: Option<ThreadGoal>,
}

impl From<GoalToolOutput> for LanguageModelToolResultContent {
    fn from(output: GoalToolOutput) -> Self {
        serde_json::to_string(&output)
            .unwrap_or_else(|error| format!("Failed to serialize goal: {error}"))
            .into()
    }
}

pub struct GoalTool {
    thread: WeakEntity<Thread>,
}

impl GoalTool {
    pub fn new(thread: WeakEntity<Thread>) -> Self {
        Self { thread }
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
        _: ToolCallEventStream,
        cx: &mut App,
    ) -> Task<Result<Self::Output, Self::Output>> {
        cx.spawn(async move |cx| {
            let input = input.recv().await.map_err(|error| GoalToolOutput {
                message: error.to_string(),
                goal: None,
            })?;
            let result = self.thread.update(cx, |thread, cx| -> anyhow::Result<_> {
                match input.action.as_str() {
                    "inspect" => {}
                    "create" => {
                        anyhow::ensure!(
                            !input.verification_surfaces.is_empty()
                                && input.verification_surfaces.iter().all(|surface| !surface.trim().is_empty()),
                            "Describe at least one concrete verification surface, such as a test command, benchmark, artifact, or source to inspect"
                        );
                        anyhow::ensure!(
                            input.constraints.iter().all(|constraint| !constraint.trim().is_empty()),
                            "Constraints must be non-empty when provided"
                        );
                        let mut goal = ThreadGoal::new(input.objective, input.success_criteria)?;
                        goal.verification_surfaces = input.verification_surfaces;
                        goal.constraints = input.constraints;
                        thread.set_goal(goal, cx)?;
                    }
                    "complete" => {
                        thread.complete_goal(input.criterion_evidence, cx)?;
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
                Ok(thread.goal().cloned())
            });
            match result {
                Ok(Ok(goal)) => Ok(GoalToolOutput {
                    message: "Goal state updated".into(),
                    goal,
                }),
                Ok(Err(error)) | Err(error) => Err(GoalToolOutput {
                    message: error.to_string(),
                    goal: None,
                }),
            }
        })
    }
}
