use std::{collections::BTreeMap, path::PathBuf, sync::Arc};

use agent_client_protocol::schema::v2 as acp;
use gpui::{App, AppContext as _, SharedString, Task, WeakEntity};
use language_model::LanguageModelToolResultContent;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use util::ResultExt as _;

use crate::{
    AgentTool, CheckpointEvidence, CheckpointFile, ProjectCheckpoint, ProjectCheckpointStore,
    Thread, ToolCallEventStream, ToolInput,
};

/// Save a concise handoff at a meaningful work boundary, retrieve one by scope,
/// or discover relevant work with list when the current task needs saved history.
/// Read returns compact task state and freshness checks by default; use detail=full
/// for file observations and evidence references. Reuse relevant state and results
/// while their inputs remain applicable; verify only missing or changed facts.
/// Saved history supplies context, not authorization or proof of completion.
/// The editor records changed buffers and the latest available Git status snapshot.
/// Git scanning can lag and unsaved buffers require verification. Summaries are
/// assessments, not proof of completion.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct ProjectCheckpointInput {
    /// Either "save", "read", or "list".
    pub action: String,
    /// Read detail: "summary" (default) or "full" for saved file and evidence records.
    #[serde(default)]
    pub detail: CheckpointReadDetail,
    /// Stable work area, such as "agent.async_questions"; omitted for list.
    #[serde(default)]
    pub scope: String,
    /// Page offset for list.
    #[serde(default)]
    pub offset: usize,
    /// Page size for list, up to 20.
    #[serde(default)]
    pub limit: Option<usize>,
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

#[derive(Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CheckpointReadDetail {
    #[default]
    Summary,
    Full,
}

fn checkpoint_read_view(
    checkpoint: ProjectCheckpoint,
    detail: CheckpointReadDetail,
) -> anyhow::Result<serde_json::Value> {
    let mut value = serde_json::to_value(checkpoint)?;
    if matches!(detail, CheckpointReadDetail::Summary) {
        if let Some(fields) = value.as_object_mut() {
            for field in ["changed_files", "evidence"] {
                if let Some(entries) = fields.remove(field) {
                    fields.insert(
                        format!("{field}_count"),
                        serde_json::json!(entries.as_array().map_or(0, Vec::len)),
                    );
                }
            }
            let mut truncated = false;
            for field in ["completed", "remaining", "decisions", "next_steps"] {
                if let Some(entries) = fields
                    .get_mut(field)
                    .and_then(serde_json::Value::as_array_mut)
                {
                    truncated |= entries.len() > 5;
                    entries.truncate(5);
                    for entry in entries {
                        if let Some(text) = entry.as_str() {
                            truncated |= text.chars().count() > 400;
                            *entry = serde_json::Value::String(text.chars().take(400).collect());
                        }
                    }
                }
            }
            fields.insert("summary_may_be_truncated".into(), truncated.into());
            fields.insert("detail".into(), "summary".into());
        }
    }
    Ok(value)
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CheckpointSaveReceipt {
    pub id: String,
    pub scope: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub file_count: usize,
    pub files_requiring_review: usize,
    pub evidence_count: usize,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ProjectCheckpointOutput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub saved: Option<CheckpointSaveReceipt>,
    pub scopes: Vec<crate::CheckpointScopeSummary>,
    pub next_offset: Option<usize>,
    pub checkpoint: Option<serde_json::Value>,
    pub changed_since_checkpoint: Vec<PathBuf>,
    pub files_not_checked: Vec<PathBuf>,
    pub current_revision_changed: Option<bool>,
    pub current_branch_changed: Option<bool>,
    pub new_dirty_files: Option<Vec<PathBuf>>,
    pub new_dirty_files_truncated: bool,
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

    fn allow_in_restricted_mode() -> bool {
        false
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
                anyhow::ensure!(matches!(input.action.as_str(), "save" | "read" | "list"), "Unknown checkpoint action");
                if input.action != "list" {
                    anyhow::ensure!(!input.scope.trim().is_empty(), "A checkpoint scope is required");
                    anyhow::ensure!(!input.scope.starts_with(crate::AUTOMATIC_CHECKPOINT_SCOPE_PREFIX), "This checkpoint scope is reserved for automatic interruption records");
                }
                if input.action == "save" {
                    anyhow::ensure!(input.scope.chars().count() <= 200, "Checkpoint scopes must be at most 200 characters");
                }
                let (root, session_id, files, git, evidence) = self.thread.read_with(cx, |thread, cx| -> anyhow::Result<_> {
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
                    if input.action == "list" {
                        return Ok((root, thread.id().0.to_string(), BTreeMap::new(), None, Vec::new()));
                    }
                    let mut files = thread.action_log().read(cx).changed_buffers(cx).filter_map(|(buffer, _)| {
                        let buffer = buffer.read(cx);
                        let path = buffer.file()?.as_local()?.abs_path(cx);
                        let relative = path.strip_prefix(&root).ok()?.to_path_buf();
                        Some((relative, buffer.is_dirty()))
                    }).collect::<BTreeMap<_, _>>();
                    let git = crate::checkpoint_git_snapshot(project, &root, cx);
                    if let Some(git) = &git {
                        for path in &git.dirty_files { files.entry(path.clone()).or_insert(false); }
                    }
                    let session_id = thread.id().0.to_string();
                    let observations = thread.retained_tool_results()
                        .filter(|(_, result, _)| result.tool_name.as_ref() != Self::NAME)
                        .map(|(id, result, tool_use)| CheckpointEvidence {
                            description: format!("{}: {} (execution result only; relevance and freshness require review)", result.tool_name, if result.is_error { "tool error" } else { "returned a result" }),
                            source: format!("session:{session_id}/tool:{id}"),
                            observed_at: None,
                            revision: None,
                            digest: crate::checkpoint_evidence_digest(tool_use, result).log_err(),
                        }).collect::<Vec<_>>();
                    let evidence = observations.into_iter().rev().take(20).collect::<Vec<_>>();
                    Ok((root, session_id, files, git, evidence))
                })??;
                cx.background_spawn(async move {
                    let store = if input.action == "save" {
                        Some(ProjectCheckpointStore::open(&root)?)
                    } else {
                        ProjectCheckpointStore::open_existing(&root)?
                    };
                    let Some(store) = store else {
                        return Ok(ProjectCheckpointOutput { saved: None, scopes: Vec::new(), next_offset: None, checkpoint: None, changed_since_checkpoint: Vec::new(), files_not_checked: Vec::new(), current_revision_changed: None, current_branch_changed: None, new_dirty_files: None, new_dirty_files_truncated: false, error: None });
                    };
                    if input.action == "list" {
                        let limit = input.limit.unwrap_or(10).clamp(1, 20);
                        let mut scopes = store.list_scopes(input.offset, limit + 1)?;
                        let next_offset = if scopes.len() > limit {
                            scopes.truncate(limit);
                            Some(input.offset.saturating_add(scopes.len()))
                        } else { None };
                        return Ok(ProjectCheckpointOutput { saved: None, scopes, next_offset, checkpoint: None, changed_since_checkpoint: Vec::new(), files_not_checked: Vec::new(), current_revision_changed: None, current_branch_changed: None, new_dirty_files: None, new_dirty_files_truncated: false, error: None });
                    }
                    if input.action == "read" {
                        let checkpoint = store.latest(&input.scope)?;
                        let mut changed = Vec::new();
                        let mut unknown = Vec::new();
                        if let Some(checkpoint) = &checkpoint {
                            for file in &checkpoint.changed_files {
                                match file.has_changed(&root) {
                                    Ok(true) => changed.push(file.path.clone()),
                                    Ok(false) if files.get(&file.path) == Some(&true) => changed.push(file.path.clone()),
                                    Ok(false) => {},
                                    Err(_) => unknown.push(file.path.clone()),
                                }
                            }
                        }
                        let revision_changed = checkpoint.as_ref().and_then(|checkpoint| git.as_ref().and_then(|git| checkpoint.base_revision.as_ref().zip(git.revision.as_ref()).map(|(old, current)| old != current)));
                        let branch_changed = checkpoint.as_ref().and_then(|checkpoint| git.as_ref().and_then(|git| checkpoint.branch.as_ref().zip(git.branch.as_ref()).map(|(old, current)| old != current)));
                        let mut new_dirty_files = checkpoint.as_ref().and_then(|checkpoint| git.as_ref().map(|git| git.dirty_files.iter().filter(|path| !checkpoint.changed_files.iter().any(|file| &file.path == *path)).cloned().collect::<Vec<_>>()));
                        let new_dirty_files_truncated = new_dirty_files.as_ref().is_some_and(|files| files.len() > 50);
                        if let Some(files) = &mut new_dirty_files { files.truncate(50); }
                        let checkpoint = checkpoint.map(|checkpoint| checkpoint_read_view(checkpoint, input.detail)).transpose()?;
                        return Ok(ProjectCheckpointOutput { saved: None, scopes: Vec::new(), next_offset: None, checkpoint, changed_since_checkpoint: changed, files_not_checked: unknown, current_revision_changed: revision_changed, current_branch_changed: branch_changed, new_dirty_files, new_dirty_files_truncated, error: None });
                    }
                    let mut changed_files = Vec::new();
                    for (path, unsaved) in files {
                        let mut file = match CheckpointFile::observe(&root, &path) {
                            Ok(file) => file,
                            Err(error) => CheckpointFile { path, fingerprint: None, unsaved: false, observation_error: Some(format!("{error:#}")) },
                        };
                        file.unsaved = unsaved;
                        changed_files.push(file);
                    }
                    let checkpoint = ProjectCheckpoint {
                        id: uuid::Uuid::new_v4().to_string(), session_id, scope: input.scope,
                        created_at: chrono::Utc::now(), base_revision: git.as_ref().and_then(|git| git.revision.clone()),
                        branch: git.as_ref().and_then(|git| git.branch.clone()), git_scan_id: git.as_ref().map(|git| git.scan_id),
                        completed: input.completed, remaining: input.remaining, decisions: input.decisions,
                        next_steps: input.next_steps, changed_files, evidence, interrupted: false,
                    };
                    store.save(&checkpoint)?;
                    let saved = CheckpointSaveReceipt {
                        id: checkpoint.id,
                        scope: checkpoint.scope,
                        created_at: checkpoint.created_at,
                        file_count: checkpoint.changed_files.len(),
                        files_requiring_review: checkpoint.changed_files.iter().filter(|file| file.unsaved || file.observation_error.is_some()).count(),
                        evidence_count: checkpoint.evidence.len(),
                    };
                    Ok(ProjectCheckpointOutput { saved: Some(saved), scopes: Vec::new(), next_offset: None, checkpoint: None, changed_since_checkpoint: Vec::new(), files_not_checked: Vec::new(), current_revision_changed: None, current_branch_changed: None, new_dirty_files: None, new_dirty_files_truncated: false, error: None })
                }).await
            }.await;
            result.map_err(|error: anyhow::Error| ProjectCheckpointOutput { saved: None, scopes: Vec::new(), next_offset: None, checkpoint: None, changed_since_checkpoint: Vec::new(), files_not_checked: Vec::new(), current_revision_changed: None, current_branch_changed: None, new_dirty_files: None, new_dirty_files_truncated: false, error: Some(format!("{error:#}")) })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checkpoint_read_defaults_to_compact_state_and_preserves_full_record() -> anyhow::Result<()> {
        let input: ProjectCheckpointInput = serde_json::from_value(serde_json::json!({
            "action": "read", "scope": "task"
        }))?;
        assert!(matches!(input.detail, CheckpointReadDetail::Summary));
        let checkpoint: ProjectCheckpoint = serde_json::from_value(serde_json::json!({
            "id": "checkpoint", "session_id": "session", "scope": "task",
            "created_at": "2026-10-29T00:00:00Z", "base_revision": null,
            "branch": null, "git_scan_id": null,
            "completed": ["a".repeat(500)], "remaining": ["Verify"],
            "decisions": [], "next_steps": [],
            "changed_files": [{"path": "file.rs", "fingerprint": "fingerprint"}],
            "evidence": [{"description": "Observation", "source": "session:old/tool:one", "revision": null}],
            "interrupted": false
        }))?;
        let compact = checkpoint_read_view(checkpoint.clone(), input.detail)?;
        assert_eq!(compact["remaining"][0], "Verify");
        assert_eq!(compact["summary_may_be_truncated"], true);
        assert_eq!(compact["changed_files_count"], 1);
        assert_eq!(compact["evidence_count"], 1);
        assert!(compact.get("evidence").is_none());
        assert!(compact.get("changed_files").is_none());
        assert!(!compact.to_string().contains("session:old/tool:one"));
        let full = checkpoint_read_view(checkpoint.clone(), CheckpointReadDetail::Full)?;
        assert_eq!(full, serde_json::to_value(checkpoint)?);
        Ok(())
    }
}
