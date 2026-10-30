use acp_thread::subagents::{SubagentLifecycle, SubagentTaskSnapshot};
use acp_thread::{SUBAGENT_SESSION_INFO_META_KEY, SubagentSessionInfo};
use agent_client_protocol::schema::v2 as acp_v2;
use anyhow::Result;
use futures::{FutureExt, future::Shared};
use gpui::{App, SharedString, Task};
use language_model::LanguageModelToolResultContent;
use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use acp_thread::AgentModelId;

use crate::{AgentTool, ThreadEnvironment, ToolCallEventStream, ToolInput};

/// Spawn a sub-agent for a well-scoped task.
///
/// ### Designing delegated subtasks
/// - An agent does not see your conversation history. Include all relevant context (file paths, requirements, constraints) in the message.
/// - Subtasks must be concrete, well-defined, and self-contained.
/// - Delegated subtasks must materially advance the main task.
/// - Do not duplicate work between your work and delegated subtasks.
/// - Do not use this tool for tasks you could accomplish directly with one or two tool calls. For example, don't ask the agent to read a single file and return the contents, you can do this yourself.
/// - When you delegate work, focus on coordinating and synthesizing results instead of duplicating the same work yourself.
/// - Avoid issuing multiple delegate calls for the same unresolved subproblem unless the new delegated task is genuinely different and necessary.
/// - Narrow the delegated ask to the concrete output you need next.
/// - For code-edit subtasks, decompose work so each delegated task has a disjoint write set.
/// - When sending a follow-up using an existing agent session_id, the agent already has the context from the previous turn. Send only a short, direct message. Do NOT repeat the original task or context.
///
/// ### Parallel delegation patterns
/// - Run multiple independent information-seeking subtasks in parallel when you have distinct questions that can be answered independently.
/// - Split implementation into disjoint codebase slices and spawn multiple agents for them in parallel when the write scopes do not overlap.
/// - When a plan has multiple independent steps, prefer delegating those steps in parallel rather than serializing them unnecessarily.
/// - Reuse the returned session_id when you want to follow up on the same delegated subproblem instead of creating a duplicate session.
///
/// ### Model selection
/// - When the user requests a particular model or asks you to choose based on cost or capability, call `list_agents_and_models` first, then pass the exact `models[].id` from the native Lin agent entry (`is_native: true`) in `model`.
/// - Omit `model` to use the user's configured subagent model, or the parent model when no subagent model is configured.
/// - Do not silently choose a different model when an explicit model is unavailable unless the user allowed fallback.
/// - A resumed session keeps its existing model, so `model` cannot be combined with `session_id`.
///
/// ### Output
/// - By default, wait for the final message. Set background=true to return a running task.
/// - Use action=status/result/cancel with session_id to control retained tasks.
/// - result with wait=true joins existing work without sending a duplicate prompt.
/// - Resume with a new message only after the prior turn finishes or cancellation settles.
/// - Successful calls return a session_id that you can use for follow-up messages.
/// - Error results may also include a session_id if a session was already created.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct SpawnAgentToolInput {
    /// Short label displayed in the UI while the agent runs (e.g., "Researching alternatives")
    #[serde(default)]
    pub label: String,
    /// The prompt for the agent. For new sessions, include full context needed for the task. For follow-ups (with session_id), you can rely on the agent already having the previous message.
    #[serde(default)]
    pub message: String,
    /// Session ID of an existing agent session to continue instead of creating a new one. Omit to create a new agent.
    #[serde(default, deserialize_with = "deserialize_session_id")]
    pub session_id: Option<acp_v2::SessionId>,
    /// Optional model override. Pass the exact `models[].id` returned for the
    /// native Lin agent (`is_native: true`) by `list_agents_and_models`.
    /// Omit to preserve default behavior.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Start work and return immediately instead of waiting for its final result.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub background: bool,
    /// Start/resume (default), inspect status, retrieve a result, or cancel a running turn.
    #[serde(default, skip_serializing_if = "SpawnAgentAction::is_default")]
    pub action: SpawnAgentAction,
    /// Wait for completion when retrieving a result. Never sends another prompt.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub wait: bool,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SpawnAgentAction {
    #[default]
    Start,
    Status,
    Result,
    Cancel,
}

impl SpawnAgentAction {
    fn is_default(&self) -> bool {
        *self == Self::Start
    }
}

fn deserialize_session_id<'de, D>(deserializer: D) -> Result<Option<acp_v2::SessionId>, D::Error>
where
    D: Deserializer<'de>,
{
    let Some(value) = Option::<serde_json::Value>::deserialize(deserializer)? else {
        return Ok(None);
    };

    if value
        .as_str()
        .is_some_and(|session_id| session_id.trim().is_empty())
    {
        return Ok(None);
    }

    serde_json::from_value(value)
        .map(Some)
        .map_err(serde::de::Error::custom)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
#[serde(rename_all = "snake_case")]
pub enum SpawnAgentToolOutput {
    Task {
        task: SubagentTaskSnapshot,
    },
    Success {
        session_id: acp_v2::SessionId,
        output: String,
        session_info: SubagentSessionInfo,
    },
    Error {
        #[serde(skip_serializing_if = "Option::is_none")]
        #[serde(default)]
        session_id: Option<acp_v2::SessionId>,
        error: String,
        session_info: Option<SubagentSessionInfo>,
    },
}

impl From<SpawnAgentToolOutput> for LanguageModelToolResultContent {
    fn from(output: SpawnAgentToolOutput) -> Self {
        match output {
            SpawnAgentToolOutput::Task { task } => serde_json::to_string(&task)
                .unwrap_or_else(|error| format!("Failed to serialize subagent task: {error}"))
                .into(),
            SpawnAgentToolOutput::Success {
                session_id,
                output,
                session_info: _, // Don't show this to the model
            } => serde_json::to_string(
                &serde_json::json!({ "session_id": session_id, "output": output }),
            )
            .unwrap_or_else(|e| format!("Failed to serialize spawn_agent output: {e}"))
            .into(),
            SpawnAgentToolOutput::Error {
                session_id,
                error,
                session_info: _, // Don't show this to the model
            } => serde_json::to_string(
                &serde_json::json!({ "session_id": session_id, "error": error }),
            )
            .unwrap_or_else(|e| format!("Failed to serialize spawn_agent output: {e}"))
            .into(),
        }
    }
}

/// Tool that spawns an agent thread to work on a task.
#[derive(Clone)]
pub struct SpawnAgentTool {
    environment: Rc<dyn ThreadEnvironment>,
    tasks: Rc<RefCell<HashMap<acp_v2::SessionId, SubagentTask>>>,
}

struct SubagentTask {
    snapshot: SubagentTaskSnapshot,
    handle: Option<Rc<dyn crate::SubagentHandle>>,
    completion: Option<Shared<Task<SubagentTaskSnapshot>>>,
    driver: Option<Task<()>>,
    disposal_context: gpui::AsyncApp,
}

impl Drop for SubagentTask {
    fn drop(&mut self) {
        if !self.snapshot.state.is_active() {
            return;
        }
        if let Some(handle) = self.handle.take() {
            // Dropping the waiter does not cancel the native model turn.
            self.disposal_context
                .spawn(async move |cx| {
                    if let Err(error) = cx.update(|cx| handle.cancel(cx)) {
                        log::error!(
                            "Failed to cancel disposed subagent {}: {error:#}",
                            handle.id()
                        );
                    }
                })
                .detach();
        }
    }
}

impl SpawnAgentTool {
    pub fn snapshots(&self) -> Vec<SubagentTaskSnapshot> {
        let mut snapshots: Vec<_> = self
            .tasks
            .borrow()
            .values()
            .map(|task| task.snapshot.clone())
            .collect();
        snapshots.sort_by(|left, right| left.session_id.0.cmp(&right.session_id.0));
        snapshots
    }

    pub fn snapshot(&self, session_id: &acp_v2::SessionId) -> Option<SubagentTaskSnapshot> {
        self.tasks
            .borrow()
            .get(session_id)
            .map(|task| task.snapshot.clone())
    }

    pub fn cancel(
        &self,
        session_id: &acp_v2::SessionId,
        cx: &mut App,
    ) -> Result<SubagentTaskSnapshot> {
        let mut tasks = self.tasks.borrow_mut();
        let task = tasks
            .get_mut(session_id)
            .ok_or_else(|| anyhow::anyhow!("No retained task for session {session_id}"))?;
        if task.snapshot.state == SubagentLifecycle::Running {
            let handle = task
                .handle
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("Running subagent has no handle"))?;
            handle.cancel(cx)?;
            task.snapshot.state = SubagentLifecycle::Cancelling;
        }
        Ok(task.snapshot.clone())
    }

    pub fn forget(&self, session_id: &acp_v2::SessionId) -> Result<()> {
        let mut tasks = self.tasks.borrow_mut();
        if tasks
            .get(session_id)
            .is_some_and(|task| task.snapshot.state.is_active())
        {
            anyhow::bail!(
                "Cannot forget a running subagent; cancel it and wait for completion first"
            );
        }
        tasks.remove(session_id);
        Ok(())
    }

    pub fn new(environment: Rc<dyn ThreadEnvironment>) -> Self {
        Self {
            environment,
            tasks: Rc::default(),
        }
    }
}

impl AgentTool for SpawnAgentTool {
    type Input = SpawnAgentToolInput;
    type Output = SpawnAgentToolOutput;

    const NAME: &'static str = "spawn_agent";

    fn kind() -> acp_v2::ToolKind {
        acp_v2::ToolKind::Other
    }

    fn initial_title(
        &self,
        input: Result<Self::Input, serde_json::Value>,
        _cx: &mut App,
    ) -> SharedString {
        match input {
            Ok(i) => i.label.into(),
            Err(value) => value
                .get("label")
                .and_then(|v| v.as_str())
                .map(|s| SharedString::from(s.to_owned()))
                .unwrap_or_else(|| "Spawning agent".into()),
        }
    }

    fn run(
        self: Arc<Self>,
        input: ToolInput<Self::Input>,
        event_stream: ToolCallEventStream,
        cx: &mut App,
    ) -> Task<Result<Self::Output, Self::Output>> {
        cx.spawn(async move |cx| {
            let input = input
                .recv()
                .await
                .map_err(|e| SpawnAgentToolOutput::Error {
                    session_id: None,
                    error: e.to_string(),
                    session_info: None,
                })?;

            let error = |session_id, error: String| SpawnAgentToolOutput::Error {
                session_id,
                error,
                session_info: None,
            };
            if input.action != SpawnAgentAction::Start {
                let session_id = input.session_id.clone().ok_or_else(|| {
                    error(None, "session_id is required for control operations".into())
                })?;
                if input.wait && input.action != SpawnAgentAction::Result {
                    return Err(error(
                        Some(session_id),
                        "wait is only supported for result".into(),
                    ));
                }
                if input.model.is_some() || !input.message.is_empty() || input.background {
                    return Err(error(
                        Some(session_id),
                        "Control operations cannot include model, message, or background".into(),
                    ));
                }
                let (snapshot, completion) = {
                    let tasks = self.tasks.borrow();
                    let task = tasks.get(&session_id).ok_or_else(|| {
                        error(
                            Some(session_id.clone()),
                            "No retained task for this session".into(),
                        )
                    })?;
                    (task.snapshot.clone(), task.completion.clone())
                };
                if input.action == SpawnAgentAction::Cancel {
                    cx.update(|cx| self.cancel(&session_id, cx))
                        .map_err(|failure| error(Some(session_id.clone()), failure.to_string()))?;
                }
                let snapshot = if input.action == SpawnAgentAction::Result && input.wait {
                    match completion {
                        Some(completion) => completion.await,
                        None => snapshot,
                    }
                } else {
                    self.snapshot(&session_id)
                        .ok_or_else(|| error(Some(session_id), "Task was released".into()))?
                };
                return Ok(SpawnAgentToolOutput::Task { task: snapshot });
            }
            if input.message.trim().is_empty() || input.wait {
                return Err(error(
                    input.session_id,
                    "Start requires a nonempty message; wait is only for result".into(),
                ));
            }
            let SpawnAgentToolInput {
                label,
                message,
                session_id,
                model,
                background,
                ..
            } = input;
            let (subagent, session_info, generation) = cx.update(|cx| {
                if let Some(session_id) = &session_id {
                    if self
                        .snapshot(session_id)
                        .is_some_and(|task| task.state.is_active())
                    {
                        return Err(error(
                            Some(session_id.clone()),
                            "Subagent already has a running turn; use status/result/cancel".into(),
                        ));
                    }
                }
                let subagent = match (session_id.clone(), model) {
                    (Some(_), Some(_)) => Err(anyhow::anyhow!(
                        "model cannot be changed when resuming a subagent session"
                    )),
                    (Some(session_id), None) => self.environment.resume_subagent(session_id, cx),
                    (None, model) => self.environment.create_subagent(
                        label.clone(),
                        model.map(AgentModelId::from),
                        cx,
                    ),
                }
                .map_err(|failure| error(session_id, failure.to_string()))?;
                let session_info = SubagentSessionInfo {
                    session_id: subagent.id(),
                    message_start_index: subagent.num_entries(cx),
                    message_end_index: None,
                };
                let generation = self
                    .snapshot(&subagent.id())
                    .map_or(1, |task| task.generation.saturating_add(1));
                Ok((subagent, session_info, generation))
            })?;
            event_stream.update_fields(|update| {
                update.meta(acp_v2::Meta::from_iter([(
                    SUBAGENT_SESSION_INFO_META_KEY.into(),
                    serde_json::json!(&session_info),
                )]))
            });
            let session_id = subagent.id();
            let snapshot = SubagentTaskSnapshot {
                session_id: session_id.clone(),
                label,
                generation,
                state: SubagentLifecycle::Running,
                output: None,
                error: None,
                session_info,
            };
            self.tasks.borrow_mut().insert(
                session_id.clone(),
                SubagentTask {
                    snapshot: snapshot.clone(),
                    handle: Some(subagent.clone()),
                    completion: None,
                    driver: None,
                    disposal_context: cx.clone(),
                },
            );
            let tasks = Rc::downgrade(&self.tasks);
            let mut finished = snapshot.clone();
            let completion = cx
                .spawn(async move |cx| {
                    let cancelled_before_start = tasks.upgrade().is_none_or(|tasks| {
                        tasks
                            .borrow()
                            .get(&finished.session_id)
                            .is_some_and(|task| {
                                task.snapshot.state == SubagentLifecycle::Cancelling
                            })
                    });
                    let response = if cancelled_before_start {
                        Err(anyhow::anyhow!("Subagent cancelled before starting"))
                    } else {
                        subagent.send(message, cx).await
                    };
                    let cancelled = tasks
                        .upgrade()
                        .and_then(|tasks| {
                            tasks
                                .borrow()
                                .get(&finished.session_id)
                                .map(|task| task.snapshot.state == SubagentLifecycle::Cancelling)
                        })
                        .unwrap_or(false);
                    finished.state = if cancelled {
                        SubagentLifecycle::Cancelled
                    } else if response.is_ok() {
                        SubagentLifecycle::Completed
                    } else {
                        SubagentLifecycle::Failed
                    };
                    match response {
                        Ok(output) if !cancelled => finished.output = Some(output),
                        Ok(_) => {}
                        Err(error) => finished.error = Some(error.to_string()),
                    }
                    telemetry::event!(
                        "Subagent Completed",
                        subagent_session = finished.session_id.to_string(),
                        status = match finished.state {
                            SubagentLifecycle::Completed => "completed",
                            SubagentLifecycle::Cancelled => "cancelled",
                            _ => "error",
                        },
                    );
                    finished.session_info.message_end_index =
                        cx.update(|cx| Some(subagent.num_entries(cx).saturating_sub(1)));
                    if let Some(tasks) = tasks.upgrade() {
                        if let Some(task) = tasks.borrow_mut().get_mut(&finished.session_id) {
                            if task.snapshot.generation == finished.generation {
                                task.snapshot = finished.clone();
                                task.handle = None;
                                task.completion = None;
                                task.driver = None;
                            }
                        }
                    }
                    finished
                })
                .shared();
            let driver = cx.spawn({
                let completion = completion.clone();
                async move |_| {
                    completion.await;
                }
            });
            if let Some(task) = self.tasks.borrow_mut().get_mut(&session_id) {
                task.completion = Some(completion.clone());
                task.driver = Some(driver);
            }
            if background {
                return Ok(SpawnAgentToolOutput::Task { task: snapshot });
            }
            let finished = completion.await;
            let output = match finished.state {
                SubagentLifecycle::Completed => Ok(SpawnAgentToolOutput::Success {
                    session_id,
                    output: finished.output.unwrap_or_default(),
                    session_info: finished.session_info,
                }),
                _ => Err(SpawnAgentToolOutput::Error {
                    session_id: Some(session_id),
                    error: finished
                        .error
                        .unwrap_or_else(|| "Subagent cancelled".into()),
                    session_info: Some(finished.session_info),
                }),
            };
            let replay_output = match &output {
                Ok(output) | Err(output) => output.clone(),
            };
            cx.update(|cx| {
                self.replay(
                    SpawnAgentToolInput {
                        label: String::new(),
                        message: String::new(),
                        session_id: None,
                        model: None,
                        background: false,
                        action: SpawnAgentAction::Start,
                        wait: false,
                    },
                    replay_output,
                    event_stream,
                    cx,
                )
            })
            .map_err(|failure| error(None, failure.to_string()))?;
            output
        })
    }

    fn replay(
        &self,
        _input: Self::Input,
        output: Self::Output,
        event_stream: ToolCallEventStream,
        _cx: &mut App,
    ) -> Result<()> {
        let (content, session_info) = match output {
            SpawnAgentToolOutput::Task { task } => (
                serde_json::to_string(&task)?.into(),
                Some(task.session_info),
            ),
            SpawnAgentToolOutput::Success {
                output,
                session_info,
                ..
            } => (output.into(), Some(session_info)),
            SpawnAgentToolOutput::Error {
                error,
                session_info,
                ..
            } => (error.into(), session_info),
        };

        let meta = session_info.map(|session_info| {
            acp_v2::Meta::from_iter([(
                SUBAGENT_SESSION_INFO_META_KEY.into(),
                serde_json::json!(&session_info),
            )])
        });
        event_stream.update_fields(|update| {
            let update = update.content(vec![content]);
            match meta {
                Some(meta) => update.meta(meta),
                None => update,
            }
        });

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    struct TestHandle {
        cancellations: Rc<std::cell::Cell<usize>>,
        sends: Rc<std::cell::Cell<usize>>,
        response: Shared<Task<String>>,
    }

    impl crate::SubagentHandle for TestHandle {
        fn id(&self) -> acp_v2::SessionId {
            acp_v2::SessionId::new("test-child")
        }
        fn num_entries(&self, _cx: &App) -> usize {
            0
        }
        fn cancel(&self, _cx: &mut App) -> Result<()> {
            self.cancellations.set(self.cancellations.get() + 1);
            Ok(())
        }
        fn send(&self, _message: String, cx: &gpui::AsyncApp) -> Task<Result<String>> {
            self.sends.set(self.sends.get() + 1);
            let response = self.response.clone();
            cx.spawn(async move |_| Ok(response.await))
        }
    }

    struct TestEnvironment(Rc<TestHandle>);

    impl ThreadEnvironment for TestEnvironment {
        fn create_terminal(
            &self,
            _command: String,
            _environment: Vec<(String, String)>,
            _cwd: Option<std::path::PathBuf>,
            _limit: Option<u64>,
            _sandbox: Option<acp_thread::SandboxWrap>,
            _cx: &mut gpui::AsyncApp,
        ) -> Task<Result<Rc<dyn crate::TerminalHandle>>> {
            Task::ready(Err(anyhow::anyhow!("Not used by this test")))
        }
        fn create_subagent(
            &self,
            _label: String,
            _model: Option<AgentModelId>,
            _cx: &mut App,
        ) -> Result<Rc<dyn crate::SubagentHandle>> {
            Ok(self.0.clone())
        }
        fn resume_subagent(
            &self,
            _session_id: acp_v2::SessionId,
            _cx: &mut App,
        ) -> Result<Rc<dyn crate::SubagentHandle>> {
            Ok(self.0.clone())
        }
    }

    async fn call(
        tool: &Arc<SpawnAgentTool>,
        value: serde_json::Value,
        cx: &mut gpui::TestAppContext,
    ) -> Result<SpawnAgentToolOutput, SpawnAgentToolOutput> {
        let input: SpawnAgentToolInput = serde_json::from_value(value).expect("valid input");
        let (events, _receiver) = ToolCallEventStream::test();
        cx.update(|cx| tool.clone().run(ToolInput::resolved(input), events, cx))
            .await
    }

    #[gpui::test(iterations = 10)]
    async fn background_lifecycle_retains_tasks_and_prevents_duplicate_turns(
        cx: &mut gpui::TestAppContext,
    ) {
        let (sender, receiver) = futures::channel::oneshot::channel::<String>();
        let response = cx
            .spawn(async move |_| receiver.await.expect("response"))
            .shared();
        let sends = Rc::new(std::cell::Cell::new(0));
        let environment = Rc::new(TestEnvironment(Rc::new(TestHandle {
            cancellations: Rc::default(),
            sends: sends.clone(),
            response,
        })));
        #[allow(clippy::arc_with_non_send_sync)]
        let tool = Arc::new(SpawnAgentTool::new(environment));
        assert!(
            call(&tool, json!({"message":"work", "background":true}), cx)
                .await
                .is_ok()
        );
        cx.run_until_parked();
        assert_eq!(sends.get(), 1);
        assert!(
            call(
                &tool,
                json!({"session_id":"test-child", "message":"duplicate"}),
                cx
            )
            .await
            .is_err()
        );
        assert!(
            call(
                &tool,
                json!({"session_id":"test-child", "action":"status"}),
                cx
            )
            .await
            .is_ok()
        );
        assert!(tool.forget(&acp_v2::SessionId::new("test-child")).is_err());
        sender.send("done".into()).expect("retained receiver");
        cx.run_until_parked();
        let snapshot = tool
            .snapshot(&acp_v2::SessionId::new("test-child"))
            .expect("snapshot");
        assert_eq!(snapshot.state, SubagentLifecycle::Completed);
        assert_eq!(snapshot.output.as_deref(), Some("done"));
        assert!(
            tool.tasks
                .borrow()
                .values()
                .all(|task| task.handle.is_none()
                    && task.completion.is_none()
                    && task.driver.is_none())
        );
        assert!(
            call(
                &tool,
                json!({"session_id":"test-child", "action":"result", "wait":true}),
                cx
            )
            .await
            .is_ok()
        );
        assert_eq!(sends.get(), 1);
        assert!(
            call(
                &tool,
                json!({"session_id":"test-child", "message":"follow up"}),
                cx
            )
            .await
            .is_ok()
        );
        assert_eq!(sends.get(), 2);
        assert_eq!(tool.snapshots().first().expect("snapshot").generation, 2);
        tool.forget(&acp_v2::SessionId::new("test-child"))
            .expect("forget completed task");
        assert!(tool.snapshots().is_empty());
    }

    #[gpui::test(iterations = 10)]
    async fn last_tool_clone_disposal_explicitly_cancels_active_subagent(
        cx: &mut gpui::TestAppContext,
    ) {
        let (sender, receiver) = futures::channel::oneshot::channel::<String>();
        let response = cx
            .spawn(async move |_| receiver.await.expect("response"))
            .shared();
        let cancellations = Rc::new(std::cell::Cell::new(0));
        let environment = Rc::new(TestEnvironment(Rc::new(TestHandle {
            cancellations: cancellations.clone(),
            sends: Rc::default(),
            response,
        })));
        #[allow(clippy::arc_with_non_send_sync)]
        let tool = Arc::new(SpawnAgentTool::new(environment));
        assert!(
            call(&tool, json!({"message":"work", "background":true}), cx)
                .await
                .is_ok()
        );
        cx.run_until_parked();
        let dropdown_tool = tool.as_ref().clone();
        let registry = Rc::downgrade(&tool.tasks);
        assert!(Rc::ptr_eq(&tool.tasks, &dropdown_tool.tasks));
        drop(tool);
        cx.run_until_parked();
        assert_eq!(cancellations.get(), 0);
        assert_eq!(dropdown_tool.snapshots().len(), 1);
        drop(dropdown_tool);
        assert!(
            registry.upgrade().is_none(),
            "running tasks must not retain their registry"
        );
        cx.run_until_parked();
        assert_eq!(cancellations.get(), 1);
        drop(sender);
    }

    #[gpui::test]
    async fn cancellation_blocks_resume_until_send_finishes(cx: &mut gpui::TestAppContext) {
        let (sender, receiver) = futures::channel::oneshot::channel::<String>();
        let response = cx
            .spawn(async move |_| receiver.await.expect("response"))
            .shared();
        let environment = Rc::new(TestEnvironment(Rc::new(TestHandle {
            cancellations: Rc::default(),
            sends: Rc::default(),
            response,
        })));
        #[allow(clippy::arc_with_non_send_sync)]
        let tool = Arc::new(SpawnAgentTool::new(environment));
        assert!(
            call(&tool, json!({"message":"work", "background":true}), cx)
                .await
                .is_ok()
        );
        cx.run_until_parked();
        assert!(
            call(
                &tool,
                json!({"session_id":"test-child", "action":"cancel"}),
                cx
            )
            .await
            .is_ok()
        );
        assert_eq!(
            tool.snapshots().first().expect("snapshot").state,
            SubagentLifecycle::Cancelling
        );
        assert!(
            call(
                &tool,
                json!({"session_id":"test-child", "message":"too soon"}),
                cx
            )
            .await
            .is_err()
        );
        sender.send("partial".into()).expect("receiver");
        cx.run_until_parked();
        let snapshot = tool.snapshots().pop().expect("snapshot");
        assert_eq!(snapshot.state, SubagentLifecycle::Cancelled);
        assert!(snapshot.output.is_none());
    }

    #[test]
    fn session_ids_preserve_legacy_tool_json() -> Result<(), serde_json::Error> {
        let session_id = "  session/雪:\"quoted\"\\opaque  ";
        let input_json = json!({
            "label": "label",
            "message": "message",
            "session_id": session_id,
        });
        let input: SpawnAgentToolInput = serde_json::from_value(input_json.clone())?;
        assert_eq!(
            input.session_id.as_ref().map(|id| id.0.as_ref()),
            Some(session_id)
        );
        assert_eq!(serde_json::to_value(input)?, input_json);

        let output_json = json!({
            "session_id": session_id,
            "error": "failed after session creation",
            "session_info": null,
        });
        let output: SpawnAgentToolOutput = serde_json::from_value(output_json.clone())?;
        assert_eq!(serde_json::to_value(output)?, output_json);
        Ok(())
    }

    #[test]
    fn deserializes_blank_session_id_as_absent() {
        for session_id in [json!(null), json!(""), json!("   ")] {
            let input: SpawnAgentToolInput = serde_json::from_value(json!({
                "label": "label",
                "message": "message",
                "session_id": session_id,
            }))
            .unwrap();

            assert!(input.session_id.is_none());
        }

        let input: SpawnAgentToolInput = serde_json::from_value(json!({
            "label": "label",
            "message": "message",
        }))
        .unwrap();
        assert!(input.session_id.is_none());

        let input: SpawnAgentToolInput = serde_json::from_value(json!({
            "label": "label",
            "message": "message",
            "session_id": "existing-session",
        }))
        .unwrap();
        assert_eq!(input.session_id.unwrap().to_string(), "existing-session");
    }
}
