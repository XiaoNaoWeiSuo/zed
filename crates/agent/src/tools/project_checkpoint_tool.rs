use std::{path::PathBuf, sync::Arc};

use agent_client_protocol::schema::v2 as acp;
use gpui::{App, AppContext as _, SharedString, Task, WeakEntity};
use language_model::LanguageModelToolResultContent;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    AgentTool, CheckpointFile, ProjectCheckpoint, ProjectCheckpointStore, Thread,
    ToolCallEventStream, ToolInput,
};

/// Save a concise handoff at a meaningful work boundary, or retrieve one by scope.
/// The editor records changed buffers automatically; unsaved buffers are marked
/// as requiring verification. Summaries are assessments, not proof of completion.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct ProjectCheckpointInput {
    /// Either "save" or "read".
    pub action: String,
    /// Stable work area, such as "agent.async_questions".
    pub scope: String,
    /// Absolute project root, required when the workspace has multiple roots.
    #[serde(default)]
    pub root: Option<PathBuf>,
    /// Work completed and its limitations.
    #[serde(default)]
    pub completed: Vec<String>,
    /// Remaining work or unresolved issues.
    #[serde(default)]
    pub remaining: Vec<String>,
    /// Important decisions and superseded assumptions.
    #[serde(default)]
    pub decisions: Vec<String>,
    /// Recommended next actions.
    #[serde(default)]
    pub next_steps: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ProjectCheckpointOutput {
    pub checkpoint: Option<ProjectCheckpoint>,
    pub changed_since_checkpoint: Vec<PathBuf>,
    pub error: Option<String>,
}

impl From<ProjectCheckpointOutput> for LanguageModelToolResultContent {
    fn from(output: ProjectCheckpointOutput) -> Self {
        serde_json::to_string(&output)
            .unwrap_or_else(|error| format!("Could not serialize checkpoint: {error}"))
            .into()
    }
}

pub struct ProjectCheckpointTool {
    thread: WeakEntity<Thread>,
}

impl ProjectCheckpointTool {
    pub fn new(thread: WeakEntity<Thread>) -> Self {
        Self { thread }
    }
}

impl AgentTool for ProjectCheckpointTool {
    type Input = ProjectCheckpointInput;
    type Output = ProjectCheckpointOutput;
    const NAME: &'static str = "project_checkpoint";

    fn kind() -> acp::ToolKind {
        acp::ToolKind::Other
    }

    fn initial_title(
        &self,
        _: Result<Self::Input, serde_json::Value>,
        _: &mut App,
    ) -> SharedString {
        "Project checkpoint".into()
    }

    fn run(
        self: Arc<Self>,
        input: ToolInput<Self::Input>,
        _: ToolCallEventStream,
        cx: &mut App,
    ) -> Task<Result<Self::Output, Self::Output>> {
        cx.spawn(async move |cx| {
            let result = async {
                let input = input.recv().await?;
                anyhow::ensure!(!input.scope.trim().is_empty(), "A checkpoint scope is required");
                anyhow::ensure!(matches!(input.action.as_str(), "save" | "read"), "Unknown checkpoint action");
                let (root, session_id, files) = self.thread.read_with(cx, |thread, cx| -> anyhow::Result<_> {
                    let project = thread.project().read(cx);
                    anyhow::ensure!(project.is_local(), "Project checkpoints currently require a local project");
                    let roots: Vec<_> = project.visible_worktrees(cx).map(|worktree| worktree.read(cx).abs_path().to_path_buf()).collect();
                    let root = if let Some(root) = &input.root {
                        anyhow::ensure!(roots.contains(root), "The checkpoint root must be an open project root");
                        root.clone()
                    } else {
                        anyhow::ensure!(roots.len() == 1, "Specify a project root when the workspace does not have exactly one root");
                        roots.into_iter().next().ok_or_else(|| anyhow::anyhow!("No project root"))?
                    };
                    let files = thread.action_log().read(cx).changed_buffers(cx).filter_map(|(buffer, _)| {
                        let buffer = buffer.read(cx);
                        let path = buffer.file()?.as_local()?.abs_path(cx);
                        let relative = path.strip_prefix(&root).ok()?.to_path_buf();
                        Some((relative, buffer.is_dirty()))
                    }).collect::<Vec<_>>();
                    Ok((root, thread.id().0.to_string(), files))
                })??;
                cx.background_spawn(async move {
                    let store = ProjectCheckpointStore::open(&root)?;
                    if input.action == "read" {
                        let checkpoint = store.latest(&input.scope)?;
                        let mut changed = Vec::new();
                        if let Some(checkpoint) = &checkpoint {
                            for file in &checkpoint.changed_files {
                                if file.has_changed(&root)? || files.iter().any(|(path, unsaved)| *unsaved && path == &file.path) {
                                    changed.push(file.path.clone());
                                }
                            }
                        }
                        return Ok(ProjectCheckpointOutput { checkpoint, changed_since_checkpoint: changed, error: None });
                    }
                    let mut changed_files = Vec::new();
                    for (path, unsaved) in files {
                        let mut file = CheckpointFile::observe(&root, &path)?;
                        file.unsaved = unsaved;
                        changed_files.push(file);
                    }
                    let checkpoint = ProjectCheckpoint {
                        id: uuid::Uuid::new_v4().to_string(), session_id, scope: input.scope,
                        created_at: chrono::Utc::now(), base_revision: None,
                        completed: input.completed, remaining: input.remaining, decisions: input.decisions,
                        next_steps: input.next_steps, changed_files, evidence: Vec::new(), interrupted: false,
                    };
                    store.save(&checkpoint)?;
                    Ok(ProjectCheckpointOutput { checkpoint: Some(checkpoint), changed_since_checkpoint: Vec::new(), error: None })
                }).await
            }.await;
            result.map_err(|error: anyhow::Error| ProjectCheckpointOutput { checkpoint: None, changed_since_checkpoint: Vec::new(), error: Some(format!("{error:#}")) })
        })
    }
}
