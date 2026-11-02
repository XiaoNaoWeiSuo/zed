use std::{
    collections::BTreeMap,
    io::Read as _,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result, ensure};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use sqlez::connection::Connection;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckpointEvidence {
    pub description: String,
    pub source: String,
    #[serde(default)]
    pub observed_at: Option<DateTime<Utc>>,
    pub revision: Option<String>,
    #[serde(default)]
    pub digest: Option<String>,
}

pub fn checkpoint_evidence_digest(
    tool_use: &language_model::LanguageModelToolUse,
    result: &language_model::LanguageModelToolResult,
) -> Result<String> {
    Ok(format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&(tool_use, result))?)
    ))
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckpointFile {
    pub path: PathBuf,
    pub fingerprint: Option<String>,
    #[serde(default)]
    pub unsaved: bool,
    #[serde(default)]
    pub observation_error: Option<String>,
}

impl CheckpointFile {
    pub fn observe(root: &Path, path: &Path) -> Result<Self> {
        ensure!(
            !path.as_os_str().is_empty()
                && path
                    .components()
                    .all(|component| matches!(component, std::path::Component::Normal(_))),
            "Checkpoint file paths must be relative and remain within the project"
        );
        let root = std::fs::canonicalize(root)?;
        let full_path = root.join(path);
        let fingerprint = match std::fs::canonicalize(&full_path) {
            Ok(resolved) => {
                ensure!(
                    resolved.starts_with(&root),
                    "Checkpoint file resolves outside the project"
                );
                let metadata = std::fs::metadata(&resolved)?;
                ensure!(
                    metadata.is_file(),
                    "Checkpoint fingerprints require a regular file"
                );
                const MAX_FINGERPRINT_BYTES: u64 = 8 * 1024 * 1024;
                ensure!(
                    metadata.len() <= MAX_FINGERPRINT_BYTES,
                    "Checkpoint file exceeds the 8 MiB fingerprint limit"
                );
                let mut file = std::fs::File::open(&resolved)
                    .with_context(|| format!("Could not read {}", full_path.display()))?;
                let mut digest = Sha256::new();
                let mut buffer = [0_u8; 65536];
                let mut bytes_read = 0_u64;
                loop {
                    let count = file
                        .read(&mut buffer)
                        .with_context(|| format!("Could not read {}", full_path.display()))?;
                    if count == 0 {
                        break;
                    }
                    bytes_read += count as u64;
                    ensure!(
                        bytes_read <= MAX_FINGERPRINT_BYTES,
                        "Checkpoint file exceeds the 8 MiB fingerprint limit"
                    );
                    digest.update(&buffer[..count]);
                }
                Some(format!("{:x}", digest.finalize()))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("Could not read {}", full_path.display()));
            }
        };
        Ok(Self {
            path: path.to_path_buf(),
            fingerprint,
            unsaved: false,
            observation_error: None,
        })
    }

    pub fn has_changed(&self, root: &Path) -> Result<bool> {
        ensure!(
            self.observation_error.is_none(),
            "The file could not be observed when the checkpoint was saved"
        );
        ensure!(
            !self.unsaved,
            "The file had unsaved changes when the checkpoint was saved"
        );
        Ok(Self::observe(root, &self.path)?.fingerprint != self.fingerprint)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectCheckpoint {
    pub id: String,
    pub session_id: String,
    pub scope: String,
    pub created_at: DateTime<Utc>,
    pub base_revision: Option<String>,
    #[serde(default)]
    pub branch: Option<String>,
    #[serde(default)]
    pub git_scan_id: Option<u64>,
    pub completed: Vec<String>,
    pub remaining: Vec<String>,
    pub decisions: Vec<String>,
    pub next_steps: Vec<String>,
    pub changed_files: Vec<CheckpointFile>,
    pub evidence: Vec<CheckpointEvidence>,
    pub interrupted: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckpointScopeSummary {
    pub scope: String,
    pub created_at: DateTime<Utc>,
    pub remaining_count: usize,
    pub next_step: Option<String>,
    /// Branch of the newest checkpoint in this scope. Used to rank scopes for
    /// injection; older records may predate branch tracking.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
}

impl From<ProjectCheckpoint> for CheckpointScopeSummary {
    fn from(checkpoint: ProjectCheckpoint) -> Self {
        Self {
            scope: checkpoint.scope,
            created_at: checkpoint.created_at,
            remaining_count: checkpoint.remaining.len(),
            next_step: checkpoint
                .next_steps
                .first()
                .map(|step| truncate_with_marker(step, 180)),
            branch: checkpoint.branch,
        }
    }
}

/// A project's checkpoint history, stored separately for each checkout.
pub struct ProjectCheckpointStore {
    connection: Connection,
}

/// Persisted metadata for a spawned runtime task.
///
/// This deliberately contains lifecycle metadata only. Prompt, output, and secrets must remain
/// in the runtime and are never written to the checkpoint database.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeTaskRecord {
    pub owner_session_id: String,
    pub child_session_id: String,
    pub generation: u64,
    pub state: String,
    pub result_returned_by_tool: Option<bool>,
    pub archived: bool,
    #[serde(default)]
    pub parent_goal_id: Option<String>,
    #[serde(default)]
    pub created_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub updated_at: Option<DateTime<Utc>>,
}

/// States accepted by the runtime task ledger. Anything else is rejected on
/// write so a typo cannot create a state no query knows how to interpret.
pub const RUNTIME_TASK_STATES: &[&str] = &[
    "pending",
    "running",
    "cancelling",
    "completed",
    "failed",
    "cancelled",
    "abandoned",
    "unknown",
];

/// States that may still have work in flight. These are the only states the
/// reconciliation pass can convert to `abandoned`.
pub const NON_TERMINAL_RUNTIME_TASK_STATES: &[&str] = &["pending", "running", "cancelling"];

pub const RUNTIME_TASK_RETENTION: usize = 200;
pub const CHECKPOINT_RETENTION_PER_SCOPE: usize = 50;
/// Internal `@automatic/` and `@compaction/` scopes are created once per
/// session, so bounding per-scope history alone still lets the database grow
/// with the number of sessions.
pub const INTERNAL_SCOPE_RETENTION: usize = 20;

/// The single worktree whose database owns session-wide runtime task records.
///
/// Checkpoints are per-worktree by design, but a session's subagent ledger is
/// not: writing it into every visible root duplicated rows and let the copies
/// drift apart. `visible_worktrees` is ordered by the project's worktree store,
/// so the first entry stays stable while the workspace is open.
pub fn canonical_runtime_task_root(roots: impl IntoIterator<Item = PathBuf>) -> Option<PathBuf> {
    roots.into_iter().next()
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeTaskMaintenance {
    pub abandoned: usize,
    pub pruned: usize,
}

/// The lifecycle state shared by goals, checkpoints, and verification receipts.
/// `Unknown` is intentionally retained when the source record cannot establish a
/// lifecycle state without making a semantic-coverage claim.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeLedgerStatus {
    Active,
    Completed,
    Stale,
    Blocked,
    #[default]
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeLedgerEntryKind {
    Goal,
    Checkpoint,
    Receipt,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeLedgerEntry {
    pub id: String,
    pub kind: RuntimeLedgerEntryKind,
    pub status: RuntimeLedgerStatus,
    /// This field is never inferred from execution success. It is `unknown`
    /// until an explicit semantic assessment is recorded elsewhere.
    #[serde(default)]
    pub semantic_coverage: RuntimeLedgerStatus,
    #[serde(default)]
    pub scope: Option<String>,
    #[serde(default)]
    pub observed_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeLedgerSnapshot {
    #[serde(default)]
    pub entries: Vec<RuntimeLedgerEntry>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeLedgerQuery {
    #[serde(default)]
    pub kind: Option<RuntimeLedgerEntryKind>,
    #[serde(default)]
    pub status: Option<RuntimeLedgerStatus>,
    #[serde(default)]
    pub id: Option<String>,
}

impl RuntimeLedgerSnapshot {
    pub fn from_records(
        goal: Option<&crate::ThreadGoal>,
        checkpoint: Option<&ProjectCheckpoint>,
        receipts: &[crate::GoalVerificationReceipt],
    ) -> Self {
        let mut entries = Vec::new();
        if let Some(goal) = goal {
            entries.push(RuntimeLedgerEntry {
                id: goal.id.clone(),
                kind: RuntimeLedgerEntryKind::Goal,
                status: match goal.status {
                    crate::ThreadGoalStatus::Active => RuntimeLedgerStatus::Active,
                    crate::ThreadGoalStatus::Completed => RuntimeLedgerStatus::Completed,
                    crate::ThreadGoalStatus::Blocked | crate::ThreadGoalStatus::Paused => {
                        RuntimeLedgerStatus::Blocked
                    }
                },
                semantic_coverage: RuntimeLedgerStatus::Unknown,
                scope: None,
                observed_at: None,
            });
        }
        if let Some(checkpoint) = checkpoint {
            entries.push(RuntimeLedgerEntry {
                id: checkpoint.id.clone(),
                kind: RuntimeLedgerEntryKind::Checkpoint,
                status: RuntimeLedgerStatus::Completed,
                semantic_coverage: RuntimeLedgerStatus::Unknown,
                scope: Some(checkpoint.scope.clone()),
                observed_at: Some(checkpoint.created_at),
            });
        }
        entries.extend(receipts.iter().map(|receipt| RuntimeLedgerEntry {
            id: receipt.id.clone(),
            kind: RuntimeLedgerEntryKind::Receipt,
            status: if receipt.stale_reason.is_some() {
                RuntimeLedgerStatus::Stale
            } else {
                match receipt.outcome {
                    crate::GoalVerificationOutcome::Passed => RuntimeLedgerStatus::Completed,
                    crate::GoalVerificationOutcome::Failed
                    | crate::GoalVerificationOutcome::Changed => RuntimeLedgerStatus::Unknown,
                }
            },
            semantic_coverage: RuntimeLedgerStatus::Unknown,
            scope: None,
            observed_at: Some(receipt.observed_at),
        }));
        Self { entries }
    }

    pub fn query(&self, query: &RuntimeLedgerQuery) -> Vec<&RuntimeLedgerEntry> {
        self.entries
            .iter()
            .filter(|entry| query.kind.map_or(true, |kind| entry.kind == kind))
            .filter(|entry| query.status.map_or(true, |status| entry.status == status))
            .filter(|entry| query.id.as_deref().map_or(true, |id| entry.id == id))
            .collect()
    }
}

#[derive(Clone, Debug)]
pub struct CheckpointRoot {
    pub path: PathBuf,
    pub git: Option<CheckpointGitSnapshot>,
}

#[derive(Clone, Debug)]
pub struct CheckpointGitSnapshot {
    pub revision: Option<String>,
    pub branch: Option<String>,
    pub scan_id: u64,
    pub dirty_files: Vec<PathBuf>,
}

pub const AUTOMATIC_CHECKPOINT_SCOPE_PREFIX: &str = "@automatic/";
pub const COMPACTION_CHECKPOINT_SCOPE_PREFIX: &str = "@compaction/";

pub struct AutomaticCheckpoint {
    pub root: PathBuf,
    pub session_id: String,
    pub files: BTreeMap<PathBuf, bool>,
    pub git: Option<CheckpointGitSnapshot>,
    pub evidence: Vec<CheckpointEvidence>,
    pub interrupted: bool,
}

impl AutomaticCheckpoint {
    pub fn save(self) -> Result<()> {
        self.save_with_summary(None)
    }

    pub fn save_compaction(self, summary: String) -> Result<()> {
        let summary = summary.trim();
        if summary.is_empty() {
            return Ok(());
        }
        self.save_with_summary(Some(summary.to_owned()))
    }

    fn save_with_summary(self, summary: Option<String>) -> Result<()> {
        if self.files.is_empty() && self.evidence.is_empty() && summary.is_none() {
            return Ok(());
        }
        ensure!(
            self.root.is_dir(),
            "Project root {} is unavailable",
            self.root.display()
        );
        let mut changed_files = Vec::new();
        for (path, unsaved) in self.files {
            let mut file = match CheckpointFile::observe(&self.root, &path) {
                Ok(file) => file,
                Err(error) => CheckpointFile {
                    path,
                    fingerprint: None,
                    unsaved: false,
                    observation_error: Some(format!("{error:#}")),
                },
            };
            file.unsaved = unsaved;
            changed_files.push(file);
        }
        let checkpoint = ProjectCheckpoint {
            id: uuid::Uuid::new_v4().to_string(),
            scope: if summary.is_some() {
                format!("{COMPACTION_CHECKPOINT_SCOPE_PREFIX}{}", self.session_id)
            } else {
                format!("{AUTOMATIC_CHECKPOINT_SCOPE_PREFIX}{}", self.session_id)
            },
            session_id: self.session_id,
            created_at: Utc::now(),
            base_revision: self.git.as_ref().and_then(|git| git.revision.clone()),
            branch: self.git.as_ref().and_then(|git| git.branch.clone()),
            git_scan_id: self.git.as_ref().map(|git| git.scan_id),
            completed: Vec::new(),
            remaining: Vec::new(),
            decisions: summary.into_iter().map(|summary| format!("Harness-saved context compaction summary (historical model assessment, not verified completion):\n{summary}")).collect(),
            next_steps: Vec::new(),
            changed_files,
            evidence: self.evidence,
            interrupted: self.interrupted,
        };
        ProjectCheckpointStore::open(&self.root)?.save(&checkpoint)
    }
}

fn is_checkpoint_database_path(path: &Path) -> bool {
    let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    path.parent() == Some(Path::new(paths::local_settings_folder_name()))
        && matches!(
            file_name,
            "checkpoints.sqlite3"
                | "checkpoints.sqlite3-journal"
                | "checkpoints.sqlite3-wal"
                | "checkpoints.sqlite3-shm"
        )
}

pub fn checkpoint_git_snapshot(
    project: &project::Project,
    root: &Path,
    cx: &gpui::App,
) -> Option<CheckpointGitSnapshot> {
    let git_store = project.git_store().read(cx);
    git_store
        .repositories()
        .values()
        .map(|repository| repository.read(cx).snapshot())
        .filter(|snapshot| root.starts_with(snapshot.work_directory_abs_path.as_ref()))
        .max_by_key(|snapshot| snapshot.work_directory_abs_path.components().count())
        .map(|snapshot| {
            let dirty_files = snapshot
                .status()
                .filter_map(|entry| {
                    if matches!(entry.status, git::status::FileStatus::Ignored) {
                        return None;
                    }
                    let absolute = snapshot.repo_path_to_abs_path(&entry.repo_path);
                    let relative = absolute.strip_prefix(root).ok()?;
                    if is_checkpoint_database_path(relative) {
                        return None;
                    }
                    Some(relative.to_path_buf())
                })
                .collect();
            CheckpointGitSnapshot {
                revision: snapshot
                    .head_commit
                    .as_ref()
                    .map(|commit| commit.sha.to_string()),
                branch: snapshot
                    .branch
                    .as_ref()
                    .map(|branch| branch.ref_name.to_string()),
                scan_id: snapshot.scan_id,
                dirty_files,
            }
        })
}

/// Truncates text for model context, preferring a word boundary, and reports
/// how many characters were dropped. A bare cut hides the loss and can end the
/// model's view inside an unrelated sentence.
pub fn truncate_for_model(text: &str, max_chars: usize) -> (String, Option<usize>) {
    let total = text.chars().count();
    if total <= max_chars {
        return (text.to_owned(), None);
    }
    let mut truncated = text.chars().take(max_chars).collect::<String>();
    if let Some((index, _)) = truncated
        .char_indices()
        .rev()
        .find(|(_, character)| character.is_whitespace())
        && truncated[..index].chars().count() >= max_chars / 2
    {
        truncated.truncate(index);
    }
    let kept = truncated.chars().count();
    (truncated, Some(total - kept))
}

/// Marks truncated entries inline so a model can tell a complete note from a
/// cut one without a parallel metadata array per field.
pub(crate) fn truncate_with_marker(text: &str, max_chars: usize) -> String {
    match truncate_for_model(text, max_chars) {
        (text, None) => text,
        (text, Some(omitted)) => format!("{text} …[+{omitted} chars]"),
    }
}

/// Ranks saved scopes for injection: scopes last touched on the current branch
/// first, then most recent. The primary handoff is excluded because it is
/// already injected as its own section.
fn rank_scopes(
    mut scopes: Vec<CheckpointScopeSummary>,
    current_branch: Option<&str>,
    primary_scope: &str,
) -> Vec<CheckpointScopeSummary> {
    scopes.retain(|scope| scope.scope != primary_scope);
    scopes.sort_by(|left, right| {
        let on_branch = |scope: &CheckpointScopeSummary| {
            matches!(
                (current_branch, scope.branch.as_deref()),
                (Some(current), Some(saved)) if current == saved
            )
        };
        on_branch(right)
            .cmp(&on_branch(left))
            .then_with(|| right.created_at.cmp(&left.created_at))
            .then_with(|| left.scope.cmp(&right.scope))
    });
    scopes
}

pub fn checkpoint_model_context(context: &str) -> String {
    fn compact(value: &mut serde_json::Value) {
        match value {
            serde_json::Value::Object(fields) => {
                if let Some(evidence) = fields.remove("evidence") {
                    fields.insert(
                        "evidence_available_on_demand".into(),
                        serde_json::json!(
                            evidence
                                .as_array()
                                .is_some_and(|entries| !entries.is_empty())
                        ),
                    );
                }
                for value in fields.values_mut() {
                    compact(value);
                }
            }
            serde_json::Value::Array(entries) => {
                for value in entries {
                    compact(value);
                }
            }
            _ => {}
        }
    }
    let Ok(mut value) = serde_json::from_str::<serde_json::Value>(context) else {
        return serde_json::json!({"checkpoint_unavailable": true, "reason": "Invalid checkpoint context; read the saved scope if needed"}).to_string();
    };
    compact(&mut value);
    let context = value.to_string();
    if context.chars().count() > 16_000 {
        return serde_json::json!({"handoff_details_omitted": true, "reason": "Checkpoint context exceeds the model budget; use project_checkpoint list/read if relevant"}).to_string();
    }
    context
}

pub fn load_checkpoint_context(roots: &[CheckpointRoot]) -> Result<Option<String>> {
    load_checkpoint_context_for_session(roots, None)
}

pub fn load_checkpoint_context_for_session(
    roots: &[CheckpointRoot],
    session_id: Option<&str>,
) -> Result<Option<String>> {
    let mut sections = Vec::new();
    for root in roots {
        let checkpoints =
            ProjectCheckpointStore::open_existing(&root.path).and_then(|store| match store {
                Some(store) => {
                    if let Some(session_id) = session_id {
                        Ok((
                            store.latest_handoff_for_session(session_id)?,
                            store.latest_observation_for_session(session_id)?,
                            store.latest(&format!(
                                "{COMPACTION_CHECKPOINT_SCOPE_PREFIX}{session_id}"
                            ))?,
                            store.list_scopes(0, 20)?,
                            store.count_scopes()?,
                        ))
                    } else {
                        Ok((
                            store.latest_any()?,
                            store.latest_automatic()?,
                            None,
                            store.list_scopes(0, 20)?,
                            store.count_scopes()?,
                        ))
                    }
                }
                None => Ok((None, None, None, Vec::new(), 0)),
            });
        let (checkpoint, interruption, compaction, scopes, scope_count) = match checkpoints {
            Ok((None, None, None, _, _)) => continue,
            Ok(records) => records,
            Err(error) => {
                log::error!(
                    "Failed to load checkpoint for {}: {error:#}",
                    root.path.display()
                );
                sections
                    .push(serde_json::json!({"root": root.path, "checkpoint_unavailable": true}));
                continue;
            }
        };
        let checkpoint = checkpoint.filter(|record| {
            !record.scope.starts_with(AUTOMATIC_CHECKPOINT_SCOPE_PREFIX)
                || !record.changed_files.is_empty()
                || !record.evidence.is_empty()
        });
        let interruption = interruption
            .filter(|record| !record.changed_files.is_empty() || !record.evidence.is_empty());
        let Some(checkpoint) = checkpoint
            .or_else(|| compaction.clone())
            .or_else(|| interruption.clone())
        else {
            continue;
        };
        if sections.len() >= 4 {
            sections.push(serde_json::json!({"additional_roots_omitted": true}));
            break;
        }
        let mut changed = Vec::new();
        let mut unknown = Vec::new();
        for file in &checkpoint.changed_files {
            match file.has_changed(&root.path) {
                Ok(true) => changed.push(file.path.display().to_string()),
                Ok(false) => {}
                Err(_) => unknown.push(file.path.display().to_string()),
            }
        }
        let new_git_paths = root.git.as_ref().map(|git| {
            git.dirty_files
                .iter()
                .filter(|path| {
                    !checkpoint
                        .changed_files
                        .iter()
                        .any(|file| &file.path == *path)
                })
                .collect::<Vec<_>>()
        });
        let git_revision_changed = root.git.as_ref().and_then(|git| {
            checkpoint
                .base_revision
                .as_ref()
                .zip(git.revision.as_ref())
                .map(|(previous, current)| previous != current)
        });
        let branch_changed = root.git.as_ref().and_then(|git| {
            checkpoint
                .branch
                .as_ref()
                .zip(git.branch.as_ref())
                .map(|(previous, current)| previous != current)
        });
        let bounded = |entries: &[String]| {
            entries
                .iter()
                .take(5)
                .map(|entry| truncate_with_marker(entry, 180))
                .collect::<Vec<_>>()
        };
        let bounded_path =
            |path: &Path| path.to_string_lossy().chars().take(180).collect::<String>();
        let path_too_long = |path: &Path| path.to_string_lossy().chars().count() > 180;
        let current_branch = root.git.as_ref().and_then(|git| git.branch.as_deref());
        let primary_is_handoff = !checkpoint
            .scope
            .starts_with(AUTOMATIC_CHECKPOINT_SCOPE_PREFIX)
            && !checkpoint
                .scope
                .starts_with(COMPACTION_CHECKPOINT_SCOPE_PREFIX);
        let mut scopes = rank_scopes(scopes, current_branch, &checkpoint.scope);
        scopes.truncate(3);
        let available_scope_count = scope_count.saturating_sub(usize::from(primary_is_handoff));
        let more_scopes_available = available_scope_count > scopes.len();
        let mut data = serde_json::json!({
            "root": root.path, "scope": checkpoint.scope, "created_at": checkpoint.created_at,
            "record_kind": if checkpoint.scope.starts_with(AUTOMATIC_CHECKPOINT_SCOPE_PREFIX) { "execution_observation" } else if checkpoint.scope.starts_with(COMPACTION_CHECKPOINT_SCOPE_PREFIX) { "context_compaction" } else { "task_handoff" },
            "evidence_count": checkpoint.evidence.len(),
            "historical_state": true,
            "recent_context_compaction": compaction.as_ref().filter(|record| record.id != checkpoint.id).map(|record| serde_json::json!({
                "scope": record.scope,
                "created_at": record.created_at,
                "record_kind": "context_compaction",
                "summary": bounded(&record.decisions),
                "summary_may_be_truncated": record.decisions.iter().any(|entry| entry.chars().count() > 180),
            })),
            "available_scopes": scopes,
            "available_scope_count": available_scope_count,
            "more_scopes_available": more_scopes_available,
            "completed": bounded(&checkpoint.completed), "remaining": bounded(&checkpoint.remaining),
            "decisions": bounded(&checkpoint.decisions), "next_steps": bounded(&checkpoint.next_steps),
            "evidence": checkpoint.evidence.iter().take(5).map(|entry| serde_json::json!({"description": truncate_with_marker(&entry.description, 180), "source": truncate_with_marker(&entry.source, 240), "observed_at": entry.observed_at, "revision": entry.revision})).collect::<Vec<_>>(),
            "more_evidence_available": checkpoint.evidence.len() > 5,
            "summary_may_be_truncated": checkpoint.completed.len() > 5 || checkpoint.remaining.len() > 5 || checkpoint.decisions.len() > 5 || checkpoint.next_steps.len() > 5 || checkpoint.completed.iter().chain(&checkpoint.remaining).chain(&checkpoint.decisions).chain(&checkpoint.next_steps).any(|entry| entry.chars().count() > 180),
            "interrupted": checkpoint.interrupted, "changed_files": changed.iter().take(10).map(|path| bounded_path(Path::new(path))).collect::<Vec<_>>(),
            "files_not_checked": unknown.iter().take(10).map(|path| bounded_path(Path::new(path))).collect::<Vec<_>>(), "base_revision": checkpoint.base_revision,
            "file_lists_may_be_truncated": changed.len() > 10 || unknown.len() > 10 || changed.iter().chain(&unknown).any(|path| path_too_long(Path::new(path))),
            "branch": checkpoint.branch, "git_scan_id": checkpoint.git_scan_id,
            "current_revision_changed": git_revision_changed, "current_branch_changed": branch_changed,
            "new_dirty_files": new_git_paths.as_ref().map(|paths| paths.iter().take(10).map(|path| bounded_path(path)).collect::<Vec<_>>()),
            "new_dirty_files_may_be_truncated": new_git_paths.as_ref().is_some_and(|paths| paths.len() > 10 || paths.iter().any(|path| path_too_long(path))), "current_git_scan_id": root.git.as_ref().map(|git| git.scan_id),
            "recent_automatic_checkpoint": interruption.as_ref().filter(|interruption| interruption.id != checkpoint.id).map(|interruption| serde_json::json!({
                "session_id": interruption.session_id,
                "created_at": interruption.created_at,
                "interrupted": interruption.interrupted,
                "files": interruption.changed_files.iter().take(10).map(|file| serde_json::json!({"path": bounded_path(&file.path), "unsaved": file.unsaved, "observation_error": file.observation_error.is_some(), "changed_since_checkpoint": match file.has_changed(&root.path) { Ok(changed) => Some(changed), Err(_) => None }})).collect::<Vec<_>>(),
                "files_may_be_truncated": interruption.changed_files.len() > 10 || interruption.changed_files.iter().any(|file| path_too_long(&file.path)),
                "evidence": interruption.evidence.iter().take(5).map(|entry| serde_json::json!({"description": truncate_with_marker(&entry.description, 180), "source": truncate_with_marker(&entry.source, 240)})).collect::<Vec<_>>(),
            })),
        });
        if root.git.is_none() {
            for field in [
                "current_revision_changed",
                "current_branch_changed",
                "new_dirty_files",
                "new_dirty_files_may_be_truncated",
                "current_git_scan_id",
            ] {
                data.as_object_mut()
                    .context("Checkpoint context must be an object")?
                    .remove(field);
            }
        }
        sections.push(data);
    }
    if sections.is_empty() {
        return Ok(None);
    }
    Ok(Some(serde_json::to_string(&sections)?))
}

fn parse_timestamp(raw: Option<String>) -> Option<DateTime<Utc>> {
    raw.and_then(|raw| {
        DateTime::parse_from_rfc3339(&raw)
            .ok()
            .map(|timestamp| timestamp.with_timezone(&Utc))
    })
}

fn add_column_if_missing(
    connection: &Connection,
    table: &str,
    column: &str,
    sql: &str,
) -> Result<()> {
    // SQLite has no `ADD COLUMN IF NOT EXISTS`, and the prepare step fails on a
    // duplicate column, so inspect the schema first instead of matching error text.
    let existing = connection.select_bound::<(String, String), i64>(
        "SELECT COUNT(*) FROM pragma_table_info(?1) WHERE name = ?2",
    )?((table.to_owned(), column.to_owned()))?;
    if existing.first().copied().unwrap_or(0) > 0 {
        return Ok(());
    }
    connection.exec(sql)?()?;
    Ok(())
}

impl ProjectCheckpointStore {
    pub fn open_existing(root: &Path) -> Result<Option<Self>> {
        let database_path = root
            .join(paths::local_settings_folder_name())
            .join("checkpoints.sqlite3");
        match std::fs::metadata(&database_path) {
            Ok(_) => Self::open(root).map(Some),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => {
                Err(error).with_context(|| format!("Could not inspect {}", database_path.display()))
            }
        }
    }
    pub fn open(root: &Path) -> Result<Self> {
        let project_root = std::fs::canonicalize(root)
            .with_context(|| format!("Could not resolve project root {}", root.display()))?;
        let directory = root.join(paths::local_settings_folder_name());
        std::fs::create_dir_all(&directory)
            .with_context(|| format!("Could not create {}", directory.display()))?;
        let resolved_directory = std::fs::canonicalize(&directory)
            .with_context(|| format!("Could not resolve {}", directory.display()))?;
        ensure!(
            resolved_directory.starts_with(&project_root),
            "Checkpoint directory resolves outside the project"
        );
        let database_path = directory.join("checkpoints.sqlite3");
        match std::fs::symlink_metadata(&database_path) {
            Ok(metadata) => ensure!(
                !metadata.file_type().is_symlink(),
                "Checkpoint database must not be a symbolic link"
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("Could not inspect {}", database_path.display()));
            }
        }
        let path = database_path
            .to_str()
            .context("Checkpoint database path is not valid UTF-8")?;
        let connection = Connection::open_file(path);
        ensure!(
            connection.persistent(),
            "Could not open checkpoint database at {}",
            database_path.display()
        );
        // `sqlite3_exec` tolerates pragmas that return a row, which is why the
        // existing statement plumbing can set these directly.
        connection.exec("PRAGMA busy_timeout = 5000;")?()?;
        connection.exec("PRAGMA journal_mode = WAL;")?()?;
        connection.exec("PRAGMA synchronous = NORMAL;")?()?;
        connection.exec(
            "CREATE TABLE IF NOT EXISTS checkpoints (
                sequence INTEGER PRIMARY KEY AUTOINCREMENT,
                id TEXT NOT NULL UNIQUE,
                session_id TEXT NOT NULL,
                scope TEXT NOT NULL,
                created_at TEXT NOT NULL,
                data TEXT NOT NULL
            )",
        )?()?;
        connection.exec(
            "CREATE INDEX IF NOT EXISTS checkpoints_scope_sequence ON checkpoints (scope, sequence DESC)",
        )?()?;
        connection.exec(
            "CREATE INDEX IF NOT EXISTS checkpoints_session_sequence ON checkpoints (session_id, sequence DESC)",
        )?()?;
        connection.exec(
            "CREATE TABLE IF NOT EXISTS runtime_task_records (
                owner_session_id TEXT NOT NULL,
                child_session_id TEXT NOT NULL,
                generation INTEGER NOT NULL,
                state TEXT NOT NULL,
                result_returned_by_tool INTEGER,
                archived INTEGER NOT NULL,
                parent_goal_id TEXT,
                created_at TEXT,
                updated_at TEXT,
                PRIMARY KEY (owner_session_id, child_session_id, generation)
            )",
        )?()?;
        // Older databases predate the lifecycle columns; migrate them in place
        // instead of assuming a fresh schema.
        add_column_if_missing(
            &connection,
            "runtime_task_records",
            "created_at",
            "ALTER TABLE runtime_task_records ADD COLUMN created_at TEXT",
        )?;
        add_column_if_missing(
            &connection,
            "runtime_task_records",
            "updated_at",
            "ALTER TABLE runtime_task_records ADD COLUMN updated_at TEXT",
        )?;
        connection.exec(
            "UPDATE runtime_task_records
             SET created_at = COALESCE(created_at, strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        )?()?;
        connection.exec(
            "UPDATE runtime_task_records
             SET updated_at = COALESCE(updated_at, created_at)",
        )?()?;
        connection.exec("PRAGMA user_version = 1;")?()?;
        connection.exec(
            "CREATE INDEX IF NOT EXISTS runtime_task_records_owner_child
             ON runtime_task_records (owner_session_id, child_session_id)",
        )?()?;
        Ok(Self { connection })
    }

    pub fn upsert_runtime_task_record(&self, record: &RuntimeTaskRecord) -> Result<()> {
        ensure!(
            !record.owner_session_id.trim().is_empty(),
            "Runtime task owner session ID is required"
        );
        ensure!(
            !record.child_session_id.trim().is_empty(),
            "Runtime task child session ID is required"
        );
        ensure!(
            !record.state.trim().is_empty(),
            "Runtime task state is required"
        );
        ensure!(
            RUNTIME_TASK_STATES.contains(&record.state.as_str()),
            "Unsupported runtime task state: {}",
            record.state
        );
        let generation =
            i64::try_from(record.generation).context("Runtime task generation is too large")?;
        let result_returned_by_tool = record.result_returned_by_tool.map(i64::from);
        let now = Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        self.connection.exec_bound::<(
            String,
            String,
            i64,
            String,
            Option<i64>,
            i64,
            Option<String>,
            String,
        )>(
            "INSERT INTO runtime_task_records
                (owner_session_id, child_session_id, generation, state,
                 result_returned_by_tool, archived, parent_goal_id, created_at, updated_at)
             SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8
             WHERE NOT EXISTS (
                 SELECT 1 FROM runtime_task_records
                 WHERE owner_session_id = ?1
                   AND child_session_id = ?2
                   AND generation > ?3
             )
             ON CONFLICT (owner_session_id, child_session_id, generation) DO UPDATE SET
                state = excluded.state,
                result_returned_by_tool = excluded.result_returned_by_tool,
                archived = excluded.archived,
                parent_goal_id = excluded.parent_goal_id,
                updated_at = excluded.updated_at",
        )?((
            record.owner_session_id.clone(),
            record.child_session_id.clone(),
            generation,
            record.state.clone(),
            result_returned_by_tool,
            i64::from(record.archived),
            record.parent_goal_id.clone(),
            now,
        ))?;
        Ok(())
    }

    pub fn query_runtime_task_records(
        &self,
        owner_session_id: &str,
    ) -> Result<Vec<RuntimeTaskRecord>> {
        let records = self.connection.select_bound::<String, (
            String,
            String,
            i64,
            String,
            Option<i64>,
            i64,
            Option<String>,
            Option<String>,
            Option<String>,
        )>(
            "SELECT owner_session_id, child_session_id, generation, state,
                    result_returned_by_tool, archived, parent_goal_id,
                    created_at, updated_at
             FROM runtime_task_records
             WHERE owner_session_id = ?1
             ORDER BY child_session_id, generation DESC",
        )?(owner_session_id.to_owned())?;
        records
            .into_iter()
            .map(
                |(
                    owner_session_id,
                    child_session_id,
                    generation,
                    state,
                    result_returned_by_tool,
                    archived,
                    parent_goal_id,
                    created_at,
                    updated_at,
                )| {
                    Ok(RuntimeTaskRecord {
                        owner_session_id,
                        child_session_id,
                        generation: u64::try_from(generation)
                            .context("Invalid runtime task generation")?,
                        state,
                        result_returned_by_tool: result_returned_by_tool.map(|value| value != 0),
                        archived: archived != 0,
                        parent_goal_id,
                        created_at: parse_timestamp(created_at),
                        updated_at: parse_timestamp(updated_at),
                    })
                },
            )
            .collect()
    }

    /// Converts non-terminal rows that the owning thread no longer knows about
    /// into `abandoned`. `live` holds the (child, generation) pairs still held
    /// in memory, so a crashed turn cannot leave tasks "running" forever.
    pub fn reconcile_runtime_tasks(
        &self,
        owner_session_id: &str,
        live: &[(String, u64)],
    ) -> Result<usize> {
        let now = Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let mut abandoned = 0;
        for record in self.query_runtime_task_records(owner_session_id)? {
            if !NON_TERMINAL_RUNTIME_TASK_STATES.contains(&record.state.as_str()) {
                continue;
            }
            if live.iter().any(|(child, generation)| {
                child == &record.child_session_id && *generation == record.generation
            }) {
                continue;
            }
            let generation =
                i64::try_from(record.generation).context("Runtime task generation is too large")?;
            self.connection
                .exec_bound::<(String, String, String, i64)>(
                    "UPDATE runtime_task_records
                 SET state = 'abandoned', updated_at = ?1
                 WHERE owner_session_id = ?2 AND child_session_id = ?3 AND generation = ?4",
                )?((
                now.clone(),
                owner_session_id.to_owned(),
                record.child_session_id,
                generation,
            ))?;
            abandoned += 1;
        }
        Ok(abandoned)
    }

    /// Reconciles orphaned rows and prunes old ones in a single call so callers
    /// cannot run one half of the maintenance and leave the ledger inconsistent.
    pub fn maintain_runtime_tasks(
        &self,
        owner_session_id: &str,
        live: &[(String, u64)],
    ) -> Result<RuntimeTaskMaintenance> {
        let abandoned = self.reconcile_runtime_tasks(owner_session_id, live)?;
        let pruned = self.prune_runtime_tasks(owner_session_id, RUNTIME_TASK_RETENTION)?;
        Ok(RuntimeTaskMaintenance { abandoned, pruned })
    }

    /// Keeps the most recently updated rows for one owner and deletes the rest.
    pub fn prune_runtime_tasks(&self, owner_session_id: &str, keep: usize) -> Result<usize> {
        let keep = i64::try_from(keep.max(1)).context("Runtime task retention is too large")?;
        let before = self.query_runtime_task_records(owner_session_id)?.len();
        self.connection.exec_bound::<(String, i64)>(
            "DELETE FROM runtime_task_records
             WHERE owner_session_id = ?1 AND rowid NOT IN (
                 SELECT rowid FROM runtime_task_records
                 WHERE owner_session_id = ?1
                 ORDER BY COALESCE(updated_at, '') DESC, generation DESC
                 LIMIT ?2
             )",
        )?((owner_session_id.to_owned(), keep))?;
        Ok(before.saturating_sub(self.query_runtime_task_records(owner_session_id)?.len()))
    }

    /// Keeps the newest handoffs per scope and deletes older ones.
    pub fn prune_checkpoints(&self, keep_per_scope: usize) -> Result<usize> {
        let keep =
            i64::try_from(keep_per_scope.max(1)).context("Checkpoint retention is too large")?;
        let before = self.count_checkpoints()?;
        self.connection.exec_bound::<i64>(
            "DELETE FROM checkpoints WHERE rowid NOT IN (
                 SELECT rowid FROM (
                     SELECT rowid,
                            ROW_NUMBER() OVER (PARTITION BY scope ORDER BY sequence DESC) AS rank
                     FROM checkpoints
                 ) WHERE rank <= ?1
             )",
        )?(keep)?;
        Ok(before.saturating_sub(self.count_checkpoints()?))
    }

    /// Applies every retention limit. Callers use this instead of pruning one
    /// dimension and leaving the database to grow through the others.
    pub fn maintain_checkpoints(&self) -> Result<usize> {
        let pruned = self.prune_checkpoints(CHECKPOINT_RETENTION_PER_SCOPE)?;
        let pruned_internal = self.prune_internal_scopes(INTERNAL_SCOPE_RETENTION)?;
        Ok(pruned.saturating_add(pruned_internal))
    }

    /// Drops the oldest internal session scopes, keeping the newest `keep` of
    /// each kind. User-named handoff scopes are never removed.
    pub fn prune_internal_scopes(&self, keep: usize) -> Result<usize> {
        let before = self.count_checkpoints()?;
        self.prune_scope_prefix(AUTOMATIC_CHECKPOINT_SCOPE_PREFIX, keep)?;
        self.prune_scope_prefix(COMPACTION_CHECKPOINT_SCOPE_PREFIX, keep)?;
        Ok(before.saturating_sub(self.count_checkpoints()?))
    }

    fn prune_scope_prefix(&self, prefix: &str, keep: usize) -> Result<()> {
        let keep = i64::try_from(keep.max(1)).context("Scope retention is too large")?;
        let pattern = format!("{prefix}%");
        self.connection.exec_bound::<(String, i64)>(
            "DELETE FROM checkpoints
             WHERE scope LIKE ?1 AND scope NOT IN (
                 SELECT scope FROM (
                     SELECT scope, MAX(sequence) AS newest
                     FROM checkpoints
                     WHERE scope LIKE ?1
                     GROUP BY scope
                     ORDER BY newest DESC
                     LIMIT ?2
                 )
             )",
        )?((pattern, keep))?;
        Ok(())
    }

    /// Deletes every checkpoint in one scope and reports how many were removed.
    pub fn delete_scope(&self, scope: &str) -> Result<usize> {
        let before = self.count_scope(scope)?;
        self.connection
            .exec_bound::<String>("DELETE FROM checkpoints WHERE scope = ?1")?(
            scope.to_owned()
        )?;
        Ok(before.saturating_sub(self.count_scope(scope)?))
    }

    pub fn count_scope(&self, scope: &str) -> Result<usize> {
        let rows =
            self.connection.select_bound::<String, i64>(
                "SELECT COUNT(*) FROM checkpoints WHERE scope = ?1",
            )?(scope.to_owned())?;
        Ok(rows.first().copied().unwrap_or(0) as usize)
    }

    pub fn count_checkpoints(&self) -> Result<usize> {
        let rows = self
            .connection
            .select_bound::<(), i64>("SELECT COUNT(*) FROM checkpoints")?(())?;
        Ok(rows.first().copied().unwrap_or(0) as usize)
    }

    /// Number of readable handoff scopes, excluding internal session scopes.
    pub fn count_scopes(&self) -> Result<usize> {
        let rows = self.connection.select_bound::<(), i64>(
            "SELECT COUNT(DISTINCT scope) FROM checkpoints
             WHERE scope NOT GLOB '@automatic/*' AND scope NOT GLOB '@compaction/*'",
        )?(())?;
        Ok(rows.first().copied().unwrap_or(0) as usize)
    }

    pub fn query_runtime_task_record(
        &self,
        owner_session_id: &str,
        child_session_id: &str,
    ) -> Result<Option<RuntimeTaskRecord>> {
        Ok(self
            .query_runtime_task_records(owner_session_id)?
            .into_iter()
            .find(|record| record.child_session_id == child_session_id))
    }

    pub fn save(&self, checkpoint: &ProjectCheckpoint) -> Result<()> {
        ensure!(
            !checkpoint.id.trim().is_empty(),
            "Checkpoint ID is required"
        );
        ensure!(
            !checkpoint.session_id.trim().is_empty(),
            "Session ID is required"
        );
        ensure!(
            !checkpoint.scope.trim().is_empty(),
            "Checkpoint scope is required"
        );
        let data = serde_json::to_string(checkpoint)?;
        self.connection.exec_bound::<(String, String, String, String, String)>(
            "INSERT INTO checkpoints (id, session_id, scope, created_at, data) VALUES (?1, ?2, ?3, ?4, ?5)",
        )?((checkpoint.id.clone(), checkpoint.session_id.clone(), checkpoint.scope.clone(), checkpoint.created_at.to_rfc3339(), data))?;
        Ok(())
    }

    pub fn latest(&self, scope: &str) -> Result<Option<ProjectCheckpoint>> {
        let records = self.connection.select_bound::<String, String>(
            "SELECT data FROM checkpoints WHERE scope = ?1 ORDER BY sequence DESC LIMIT 1",
        )?(scope.to_string())?;
        records
            .into_iter()
            .next()
            .map(|data| serde_json::from_str(&data).context("Invalid checkpoint data"))
            .transpose()
    }

    pub fn latest_for_session(&self, session_id: &str) -> Result<Option<ProjectCheckpoint>> {
        let records = self.connection.select_bound::<String, String>(
            "SELECT data FROM checkpoints WHERE session_id = ?1 ORDER BY sequence DESC LIMIT 1",
        )?(session_id.to_string())?;
        records
            .into_iter()
            .next()
            .map(|data| serde_json::from_str(&data).context("Invalid checkpoint data"))
            .transpose()
    }

    pub fn latest_handoff_for_session(
        &self,
        session_id: &str,
    ) -> Result<Option<ProjectCheckpoint>> {
        let records = self.connection.select_bound::<String, String>(
            "SELECT data FROM checkpoints WHERE session_id = ?1 AND scope NOT GLOB '@automatic/*' AND scope NOT GLOB '@compaction/*' ORDER BY sequence DESC LIMIT 1",
        )?(session_id.to_string())?;
        records
            .into_iter()
            .next()
            .map(|data| serde_json::from_str(&data).context("Invalid checkpoint data"))
            .transpose()
    }

    pub fn latest_observation_for_session(
        &self,
        session_id: &str,
    ) -> Result<Option<ProjectCheckpoint>> {
        let records = self.connection.select_bound::<String, String>(
            "SELECT data FROM checkpoints WHERE session_id = ?1 AND scope GLOB '@automatic/*' ORDER BY sequence DESC LIMIT 1",
        )?(session_id.to_string())?;
        records
            .into_iter()
            .next()
            .map(|data| serde_json::from_str(&data).context("Invalid checkpoint data"))
            .transpose()
    }

    pub fn latest_any(&self) -> Result<Option<ProjectCheckpoint>> {
        let records = self.connection.select_bound::<(), String>(
            "SELECT data FROM checkpoints WHERE scope NOT GLOB '@automatic/*' AND scope NOT GLOB '@compaction/*' ORDER BY sequence DESC LIMIT 1",
        )?(())?;
        records
            .into_iter()
            .next()
            .map(|data| serde_json::from_str(&data).context("Invalid checkpoint data"))
            .transpose()
    }

    pub fn list_scopes(&self, offset: usize, limit: usize) -> Result<Vec<CheckpointScopeSummary>> {
        let offset = i64::try_from(offset).context("Checkpoint scope offset is too large")?;
        let limit = i64::try_from(limit).context("Checkpoint scope limit is too large")?;
        let records = self.connection.select_bound::<(i64, i64), String>(
            "SELECT data FROM checkpoints WHERE sequence IN (SELECT MAX(sequence) FROM checkpoints WHERE scope NOT GLOB '@automatic/*' AND scope NOT GLOB '@compaction/*' GROUP BY scope) ORDER BY sequence DESC LIMIT ?1 OFFSET ?2",
        )?((limit, offset))?;
        records
            .into_iter()
            .map(|data| {
                let checkpoint: ProjectCheckpoint =
                    serde_json::from_str(&data).context("Invalid checkpoint data")?;
                Ok(checkpoint.into())
            })
            .collect()
    }

    pub fn list_history(&self, offset: usize, limit: usize) -> Result<Vec<ProjectCheckpoint>> {
        let offset = i64::try_from(offset).context("Checkpoint offset is too large")?;
        let limit = i64::try_from(limit).context("Checkpoint limit is too large")?;
        let records = self.connection.select_bound::<(i64, i64), String>(
            "SELECT data FROM checkpoints ORDER BY sequence DESC LIMIT ?1 OFFSET ?2",
        )?((limit, offset))?;
        records
            .into_iter()
            .map(|data| serde_json::from_str(&data).context("Invalid checkpoint data"))
            .collect()
    }

    pub fn latest_automatic(&self) -> Result<Option<ProjectCheckpoint>> {
        let records = self.connection.select_bound::<(), String>(
            "SELECT data FROM checkpoints AS interruption WHERE scope GLOB '@automatic/*' AND sequence > COALESCE((SELECT MAX(sequence) FROM checkpoints AS handoff WHERE handoff.scope NOT GLOB '@automatic/*' AND handoff.scope NOT GLOB '@compaction/*' AND handoff.session_id = interruption.session_id), 0) ORDER BY sequence DESC LIMIT 1",
        )?(())?;
        records
            .into_iter()
            .next()
            .map(|data| serde_json::from_str(&data).context("Invalid checkpoint data"))
            .transpose()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn checkpoint(id: &str, scope: &str) -> ProjectCheckpoint {
        ProjectCheckpoint {
            id: id.into(),
            session_id: "session".into(),
            scope: scope.into(),
            created_at: Utc::now(),
            base_revision: None,
            branch: None,
            git_scan_id: None,
            completed: Vec::new(),
            remaining: Vec::new(),
            decisions: Vec::new(),
            next_steps: Vec::new(),
            changed_files: Vec::new(),
            evidence: Vec::new(),
            interrupted: false,
        }
    }

    #[test]
    fn runtime_ledger_preserves_unknown_coverage_and_queries_records() -> Result<()> {
        let checkpoint = ProjectCheckpoint {
            id: "checkpoint".into(),
            session_id: "session".into(),
            scope: "task".into(),
            created_at: Utc::now(),
            base_revision: None,
            branch: None,
            git_scan_id: None,
            completed: Vec::new(),
            remaining: vec!["verify".into()],
            decisions: Vec::new(),
            next_steps: Vec::new(),
            changed_files: Vec::new(),
            evidence: Vec::new(),
            interrupted: false,
        };
        let snapshot = RuntimeLedgerSnapshot::from_records(None, Some(&checkpoint), &[]);
        assert_eq!(snapshot.entries[0].status, RuntimeLedgerStatus::Completed);
        assert_eq!(
            snapshot.entries[0].semantic_coverage,
            RuntimeLedgerStatus::Unknown
        );
        let encoded = serde_json::to_string(&snapshot)?;
        let decoded: RuntimeLedgerSnapshot = serde_json::from_str(&encoded)?;
        assert_eq!(decoded, snapshot);
        assert_eq!(
            snapshot
                .query(&RuntimeLedgerQuery {
                    kind: Some(RuntimeLedgerEntryKind::Checkpoint),
                    ..Default::default()
                })
                .len(),
            1
        );
        Ok(())
    }

    #[test]
    fn runtime_ledger_maps_lifecycle_states_without_semantic_inference() -> Result<()> {
        let mut goal = crate::ThreadGoal::new("objective".into(), vec!["criterion".into()])?;
        goal.status = crate::ThreadGoalStatus::Active;
        let stale_receipt = crate::GoalVerificationReceipt {
            id: "stale".into(),
            tool_call_id: "tool".into(),
            tool_name: "read_file".into(),
            tool_input: serde_json::Value::Null,
            observed_at: Utc::now(),
            dependencies: Vec::new(),
            outcome: crate::GoalVerificationOutcome::Passed,
            command_status: None,
            output_digest: "digest".into(),
            preview: String::new(),
            stale_reason: Some("dependency changed".into()),
            criterion_indices: vec![0],
        };
        let snapshot = RuntimeLedgerSnapshot::from_records(Some(&goal), None, &[stale_receipt]);
        assert_eq!(snapshot.entries[0].status, RuntimeLedgerStatus::Active);
        assert_eq!(snapshot.entries[1].status, RuntimeLedgerStatus::Stale);
        assert!(
            snapshot
                .entries
                .iter()
                .all(|entry| entry.semantic_coverage == RuntimeLedgerStatus::Unknown)
        );

        goal.status = crate::ThreadGoalStatus::Blocked;
        assert_eq!(
            RuntimeLedgerSnapshot::from_records(Some(&goal), None, &[]).entries[0].status,
            RuntimeLedgerStatus::Blocked
        );
        goal.status = crate::ThreadGoalStatus::Completed;
        assert_eq!(
            RuntimeLedgerSnapshot::from_records(Some(&goal), None, &[]).entries[0].status,
            RuntimeLedgerStatus::Completed
        );
        Ok(())
    }

    #[test]
    fn old_checkpoint_json_keeps_defaulted_fields_compatible() -> Result<()> {
        let checkpoint: ProjectCheckpoint = serde_json::from_value(serde_json::json!({
            "id": "checkpoint", "session_id": "session", "scope": "task",
            "created_at": "2026-10-29T00:00:00Z", "base_revision": null,
            "completed": [], "remaining": [], "decisions": [], "next_steps": [],
            "changed_files": [], "evidence": [], "interrupted": false
        }))?;
        assert_eq!(checkpoint.branch, None);
        assert_eq!(checkpoint.git_scan_id, None);
        assert!(checkpoint.evidence.is_empty());
        Ok(())
    }

    #[test]
    fn empty_automatic_checkpoints_do_not_create_metadata() -> Result<()> {
        let root = tempfile::tempdir()?;
        for summary in [None, Some(" \n ".to_owned())] {
            let checkpoint = AutomaticCheckpoint {
                root: root.path().to_path_buf(),
                session_id: "session".into(),
                files: BTreeMap::new(),
                git: None,
                evidence: Vec::new(),
                interrupted: true,
            };
            match summary {
                Some(summary) => checkpoint.save_compaction(summary)?,
                None => checkpoint.save()?,
            }
        }
        assert!(
            !root
                .path()
                .join(paths::local_settings_folder_name())
                .exists()
        );
        Ok(())
    }

    #[test]
    fn checkpoints_persist_per_scope_and_reject_duplicate_ids() -> Result<()> {
        let root = tempfile::tempdir()?;
        let store = ProjectCheckpointStore::open(root.path())?;
        let mut checkpoint = ProjectCheckpoint {
            id: "first".into(),
            session_id: "session".into(),
            scope: "agent".into(),
            created_at: Utc::now(),
            base_revision: Some("revision".into()),
            branch: Some("main".into()),
            git_scan_id: Some(1),
            completed: vec!["Implemented".into()],
            remaining: vec!["Verify".into()],
            decisions: vec![],
            next_steps: vec![],
            changed_files: vec![CheckpointFile {
                path: "src/main.rs".into(),
                fingerprint: None,
                unsaved: false,
                observation_error: None,
            }],
            evidence: vec![],
            interrupted: false,
        };
        store.save(&checkpoint)?;
        assert!(store.save(&checkpoint).is_err());
        checkpoint.id = "second".into();
        checkpoint.interrupted = true;
        store.save(&checkpoint)?;
        drop(store);
        let reopened = ProjectCheckpointStore::open(root.path())?;
        assert_eq!(reopened.latest("agent")?, Some(checkpoint.clone()));
        assert_eq!(
            reopened.latest_for_session("session")?,
            Some(checkpoint.clone())
        );
        let session_handoff = checkpoint.clone();
        checkpoint.id = "unrelated".into();
        checkpoint.session_id = "another-session".into();
        checkpoint.remaining = vec!["Unrelated task".into()];
        reopened.save(&checkpoint)?;
        let context = load_checkpoint_context_for_session(
            &[CheckpointRoot {
                path: root.path().to_path_buf(),
                git: None,
            }],
            Some("session"),
        )?
        .context("session handoff")?;
        assert!(!context.contains("Unrelated task"));
        assert!(context.contains("Verify"));
        assert!(
            load_checkpoint_context_for_session(
                &[CheckpointRoot {
                    path: root.path().to_path_buf(),
                    git: None,
                }],
                Some("new-session")
            )?
            .is_none()
        );
        assert_eq!(reopened.latest("other")?, None);
        AutomaticCheckpoint {
            root: root.path().to_path_buf(),
            session_id: "session".into(),
            files: BTreeMap::new(),
            git: None,
            evidence: Vec::new(),
            interrupted: false,
        }
        .save()?;
        assert!(
            reopened
                .latest_observation_for_session("session")?
                .is_none()
        );
        AutomaticCheckpoint {
            root: root.path().to_path_buf(),
            session_id: "session".into(),
            files: BTreeMap::new(),
            git: None,
            evidence: Vec::new(),
            interrupted: false,
        }
        .save_compaction("Decision: preserve task state".into())?;
        assert_eq!(
            reopened.latest_handoff_for_session("session")?,
            Some(session_handoff)
        );
        assert_eq!(reopened.latest("agent")?, Some(checkpoint.clone()));
        assert_eq!(
            reopened.latest_handoff_for_session("another-session")?,
            Some(checkpoint)
        );
        let compaction = reopened
            .latest("@compaction/session")?
            .context("compaction summary")?;
        assert!(
            compaction
                .decisions
                .iter()
                .any(|entry| entry.contains("Decision: preserve task state"))
        );
        assert!(compaction.completed.is_empty());
        AutomaticCheckpoint {
            root: root.path().to_path_buf(),
            session_id: "session".into(),
            files: BTreeMap::new(),
            git: None,
            evidence: vec![CheckpointEvidence {
                description: "terminal result".into(),
                source: "session:session/tool:one".into(),
                observed_at: None,
                revision: None,
                digest: None,
            }],
            interrupted: true,
        }
        .save()?;
        let combined = load_checkpoint_context_for_session(
            &[CheckpointRoot {
                path: root.path().to_path_buf(),
                git: None,
            }],
            Some("session"),
        )?
        .context("combined state")?;
        assert!(combined.contains("Decision: preserve task state"));
        assert!(combined.contains("Verify"));
        assert!(combined.contains("recent_context_compaction"));
        assert!(combined.contains("recent_automatic_checkpoint"));
        assert!(combined.contains("terminal result"));
        assert!(root.path().join(".lin/checkpoints.sqlite3").exists());
        Ok(())
    }

    #[test]
    fn runtime_task_records_survive_reopen_and_are_isolated_by_owner_session() -> Result<()> {
        let root = tempfile::tempdir()?;
        let store = ProjectCheckpointStore::open(root.path())?;
        store.upsert_runtime_task_record(&RuntimeTaskRecord {
            owner_session_id: "owner-a".into(),
            child_session_id: "child".into(),
            generation: 1,
            state: "completed".into(),
            result_returned_by_tool: Some(false),
            archived: true,
            parent_goal_id: Some("goal-a".into()),
            ..Default::default()
        })?;
        store.upsert_runtime_task_record(&RuntimeTaskRecord {
            owner_session_id: "owner-b".into(),
            child_session_id: "child".into(),
            generation: 1,
            state: "failed".into(),
            result_returned_by_tool: Some(true),
            archived: false,
            parent_goal_id: None,
            ..Default::default()
        })?;
        drop(store);

        let reopened = ProjectCheckpointStore::open(root.path())?;
        let owner_a = reopened.query_runtime_task_records("owner-a")?;
        assert_eq!(owner_a.len(), 1);
        let record = &owner_a[0];
        assert_eq!(record.owner_session_id, "owner-a");
        assert_eq!(record.child_session_id, "child");
        assert_eq!(record.generation, 1);
        assert_eq!(record.state, "completed");
        assert_eq!(record.result_returned_by_tool, Some(false));
        assert!(record.archived);
        assert_eq!(record.parent_goal_id.as_deref(), Some("goal-a"));
        assert!(record.created_at.is_some());
        assert!(record.updated_at.is_some());
        assert_eq!(
            reopened
                .query_runtime_task_record("owner-b", "child")?
                .map(|record| record.state),
            Some("failed".into())
        );
        assert!(
            reopened
                .query_runtime_task_records("missing-owner")?
                .is_empty()
        );
        Ok(())
    }

    #[test]
    fn runtime_task_records_reject_stale_generations_without_overwriting_current_state()
    -> Result<()> {
        let root = tempfile::tempdir()?;
        let store = ProjectCheckpointStore::open(root.path())?;
        let current = RuntimeTaskRecord {
            owner_session_id: "owner".into(),
            child_session_id: "child".into(),
            generation: 2,
            state: "completed".into(),
            result_returned_by_tool: Some(false),
            archived: true,
            parent_goal_id: Some("goal".into()),
            ..Default::default()
        };
        store.upsert_runtime_task_record(&current)?;
        store.upsert_runtime_task_record(&RuntimeTaskRecord {
            generation: 1,
            state: "running".into(),
            result_returned_by_tool: None,
            archived: false,
            parent_goal_id: None,
            ..current
        })?;
        let stored = store
            .query_runtime_task_record("owner", "child")?
            .context("stored runtime task")?;
        assert_eq!(stored.generation, 2);
        assert_eq!(stored.state, "completed");
        assert_eq!(store.query_runtime_task_records("owner")?.len(), 1);

        store.upsert_runtime_task_record(&RuntimeTaskRecord {
            generation: 2,
            state: "completed".into(),
            result_returned_by_tool: Some(true),
            archived: true,
            parent_goal_id: Some("goal".into()),
            ..store
                .query_runtime_task_record("owner", "child")?
                .context("current runtime task")?
        })?;
        let updated = store
            .query_runtime_task_record("owner", "child")?
            .context("updated runtime task")?;
        assert_eq!(updated.generation, 2);
        assert_eq!(updated.result_returned_by_tool, Some(true));
        assert!(updated.archived);
        Ok(())
    }

    #[test]
    fn canonical_runtime_task_root_prefers_the_first_visible_root() {
        assert_eq!(
            canonical_runtime_task_root([
                PathBuf::from("/workspace/main"),
                PathBuf::from("/workspace/linked"),
            ]),
            Some(PathBuf::from("/workspace/main"))
        );
        assert_eq!(canonical_runtime_task_root(Vec::new()), None);
    }

    #[test]
    fn runtime_task_reconciliation_abandons_orphans_but_keeps_live_tasks() -> Result<()> {
        let root = tempfile::tempdir()?;
        let store = ProjectCheckpointStore::open(root.path())?;
        for (child, state) in [
            ("live", "running"),
            ("crashed", "running"),
            ("done", "completed"),
        ] {
            store.upsert_runtime_task_record(&RuntimeTaskRecord {
                owner_session_id: "owner".into(),
                child_session_id: child.into(),
                generation: 1,
                state: state.into(),
                ..Default::default()
            })?;
        }

        let maintenance = store.maintain_runtime_tasks("owner", &[("live".into(), 1)])?;
        assert_eq!(maintenance.abandoned, 1);
        assert_eq!(maintenance.pruned, 0);

        let states = store
            .query_runtime_task_records("owner")?
            .into_iter()
            .map(|record| (record.child_session_id, record.state))
            .collect::<BTreeMap<_, _>>();
        assert_eq!(states.get("live").map(String::as_str), Some("running"));
        assert_eq!(states.get("crashed").map(String::as_str), Some("abandoned"));
        assert_eq!(states.get("done").map(String::as_str), Some("completed"));
        Ok(())
    }

    #[test]
    fn runtime_task_retention_keeps_the_most_recent_rows() -> Result<()> {
        let root = tempfile::tempdir()?;
        let store = ProjectCheckpointStore::open(root.path())?;
        for generation in 1..=5u64 {
            store.upsert_runtime_task_record(&RuntimeTaskRecord {
                owner_session_id: "owner".into(),
                child_session_id: format!("task-{generation}"),
                generation,
                state: "completed".into(),
                ..Default::default()
            })?;
        }

        assert_eq!(store.prune_runtime_tasks("owner", 2)?, 3);
        let remaining = store.query_runtime_task_records("owner")?;
        assert_eq!(remaining.len(), 2);
        assert!(remaining.iter().all(|record| record.generation >= 4));
        Ok(())
    }

    #[test]
    fn runtime_task_records_reject_unknown_states() -> Result<()> {
        let root = tempfile::tempdir()?;
        let store = ProjectCheckpointStore::open(root.path())?;
        let Err(error) = store.upsert_runtime_task_record(&RuntimeTaskRecord {
            owner_session_id: "owner".into(),
            child_session_id: "child".into(),
            generation: 1,
            state: "exploded".into(),
            ..Default::default()
        }) else {
            anyhow::bail!("an unknown runtime task state must be rejected");
        };
        assert!(error.to_string().contains("Unsupported runtime task state"));
        assert!(store.query_runtime_task_records("owner")?.is_empty());
        Ok(())
    }

    #[test]
    fn checkpoint_retention_and_scope_deletion_reclaim_rows() -> Result<()> {
        let root = tempfile::tempdir()?;
        let store = ProjectCheckpointStore::open(root.path())?;
        for index in 0..4 {
            store.save(&checkpoint(&format!("task-{index}"), "task"))?;
        }
        store.save(&checkpoint("other", "other"))?;

        assert_eq!(store.prune_checkpoints(2)?, 2);
        assert_eq!(store.count_scope("task")?, 2);
        assert_eq!(store.count_scope("other")?, 1);

        assert_eq!(store.delete_scope("task")?, 2);
        assert_eq!(store.count_scope("task")?, 0);
        assert_eq!(store.count_checkpoints()?, 1);
        Ok(())
    }

    #[test]
    fn internal_session_scopes_are_bounded_without_touching_handoffs() -> Result<()> {
        let root = tempfile::tempdir()?;
        let store = ProjectCheckpointStore::open(root.path())?;
        for index in 0..4 {
            store.save(&checkpoint(
                &format!("automatic-{index}"),
                &format!("{AUTOMATIC_CHECKPOINT_SCOPE_PREFIX}session-{index}"),
            ))?;
            store.save(&checkpoint(
                &format!("compaction-{index}"),
                &format!("{COMPACTION_CHECKPOINT_SCOPE_PREFIX}session-{index}"),
            ))?;
        }
        store.save(&checkpoint("handoff", "design-review"))?;

        assert_eq!(store.prune_internal_scopes(2)?, 4);
        assert_eq!(store.count_scope("design-review")?, 1);
        assert_eq!(store.count_checkpoints()?, 5);
        Ok(())
    }

    #[test]
    fn legacy_runtime_task_table_gains_lifecycle_columns() -> Result<()> {
        let root = tempfile::tempdir()?;
        let directory = root.path().join(paths::local_settings_folder_name());
        std::fs::create_dir_all(&directory)?;
        let database_path = directory.join("checkpoints.sqlite3");
        {
            let connection = Connection::open_file(
                database_path
                    .to_str()
                    .context("database path is not valid UTF-8")?,
            );
            assert!(connection.persistent());
            connection.exec(
                "CREATE TABLE runtime_task_records (
                    owner_session_id TEXT NOT NULL,
                    child_session_id TEXT NOT NULL,
                    generation INTEGER NOT NULL,
                    state TEXT NOT NULL,
                    result_returned_by_tool INTEGER,
                    archived INTEGER NOT NULL,
                    parent_goal_id TEXT,
                    PRIMARY KEY (owner_session_id, child_session_id, generation)
                )",
            )?()?;
            connection.exec(
                "INSERT INTO runtime_task_records
                 VALUES ('owner', 'child', 1, 'running', NULL, 0, NULL)",
            )?()?;
        }

        let store = ProjectCheckpointStore::open(root.path())?;
        let records = store.query_runtime_task_records("owner")?;
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].state, "running");
        assert!(records[0].created_at.is_some());
        assert!(records[0].updated_at.is_some());

        let journal_mode = store.connection.select::<String>("PRAGMA journal_mode")?()?;
        assert_eq!(
            journal_mode.first().map(|mode| mode.to_ascii_lowercase()),
            Some("wal".to_owned())
        );
        let version = store.connection.select::<i64>("PRAGMA user_version")?()?;
        assert_eq!(version, vec![1]);
        Ok(())
    }

    #[test]
    fn scope_index_lists_latest_handoffs_in_pages_without_internal_records() -> Result<()> {
        let root = tempfile::tempdir()?;
        let store = ProjectCheckpointStore::open(root.path())?;
        let mut checkpoint = ProjectCheckpoint {
            id: "one".into(),
            session_id: "session".into(),
            scope: "agent".into(),
            created_at: Utc::now(),
            base_revision: None,
            branch: None,
            git_scan_id: None,
            completed: vec![],
            remaining: vec!["verify".into()],
            decisions: vec![],
            next_steps: vec!["run tests".into()],
            changed_files: vec![],
            evidence: vec![],
            interrupted: false,
        };
        store.save(&checkpoint)?;
        checkpoint.id = "two".into();
        checkpoint.scope = "editor".into();
        store.save(&checkpoint)?;
        checkpoint.id = "three".into();
        checkpoint.scope = "agent".into();
        checkpoint.remaining.clear();
        store.save(&checkpoint)?;
        checkpoint.id = "four".into();
        checkpoint.scope = "@automatic/session".into();
        store.save(&checkpoint)?;
        for index in 0..8 {
            AutomaticCheckpoint {
                root: root.path().to_path_buf(),
                session_id: if index == 0 {
                    "session".into()
                } else {
                    format!("unrelated-{index}")
                },
                files: BTreeMap::new(),
                git: None,
                evidence: Vec::new(),
                interrupted: false,
            }
            .save_compaction(format!("Compaction summary {index}"))?;
        }
        let compaction = store
            .latest("@compaction/session")?
            .context("compaction remains readable by scope")?;
        assert_eq!(store.list_history(0, 20)?.len(), 12);
        let first = store.list_scopes(0, 1)?;
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].scope, "agent");
        assert_eq!(first[0].remaining_count, 0);
        let second = store.list_scopes(1, 1)?;
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].scope, "editor");
        assert_eq!(second[0].remaining_count, 1);
        assert!(store.list_scopes(2, 1)?.is_empty());
        let context = load_checkpoint_context(&[CheckpointRoot {
            path: root.path().to_path_buf(),
            git: None,
        }])?
        .context("context")?;
        let context: serde_json::Value = serde_json::from_str(&context)?;
        // The primary handoff is injected as its own section, so the scope list
        // only carries the other readable scopes.
        assert_eq!(
            context[0]["available_scopes"].as_array().map(Vec::len),
            Some(1)
        );
        assert_eq!(context[0]["available_scopes"][0]["scope"], "editor");
        assert_eq!(context[0]["available_scope_count"], 1);
        assert_eq!(context[0]["more_scopes_available"], false);
        assert!(context[0]["recent_context_compaction"].is_null());

        let session_context = load_checkpoint_context_for_session(
            &[CheckpointRoot {
                path: root.path().to_path_buf(),
                git: None,
            }],
            Some("session"),
        )?
        .context("session context")?;
        let projected: serde_json::Value =
            serde_json::from_str(&checkpoint_model_context(&session_context))?;
        let entry = projected.get(0).context("root context")?;
        assert_eq!(entry["available_scopes"].as_array().map(Vec::len), Some(1));
        assert_eq!(entry["available_scopes"][0]["scope"], "editor");
        assert_eq!(entry["available_scope_count"], 1);
        assert_eq!(entry["more_scopes_available"], false);
        assert_eq!(entry["historical_state"], true);
        assert_eq!(entry["record_kind"], "task_handoff");
        assert_eq!(
            entry["recent_context_compaction"]["scope"],
            compaction.scope
        );
        assert_eq!(
            entry["recent_context_compaction"]["created_at"],
            serde_json::to_value(compaction.created_at)?
        );
        assert_eq!(
            entry["recent_context_compaction"]["record_kind"],
            "context_compaction"
        );
        assert_eq!(
            entry["recent_context_compaction"]["summary"],
            serde_json::to_value(&compaction.decisions)?
        );
        assert!(!session_context.contains("unrelated-"));
        Ok(())
    }

    #[test]
    fn model_context_keeps_handoff_but_expands_evidence_only_on_demand() -> Result<()> {
        let original = serde_json::json!([{
            "root": "project", "scope": "agent", "created_at": "historical time",
            "remaining": ["Verify recovery"],
            "evidence": [{"source": "session:old/tool:one", "description": "old result"}],
            "recent_automatic_checkpoint": {"evidence": [{"source": "session:old/tool:two"}]}
        }]);
        let projected = checkpoint_model_context(&original.to_string());
        let projected: serde_json::Value = serde_json::from_str(&projected)?;
        assert_eq!(projected[0]["scope"], "agent");
        assert_eq!(projected[0]["remaining"][0], "Verify recovery");
        assert_eq!(projected[0]["created_at"], "historical time");
        assert_eq!(projected[0]["evidence_available_on_demand"], true);
        assert_eq!(
            projected[0]["recent_automatic_checkpoint"]["evidence_available_on_demand"],
            true
        );
        assert!(!projected.to_string().contains("session:old"));
        assert!(original[0]["evidence"].is_array());
        let oversized = serde_json::json!({"remaining": ["x".repeat(20_000)]});
        let bounded: serde_json::Value =
            serde_json::from_str(&checkpoint_model_context(&oversized.to_string()))?;
        assert_eq!(bounded["handoff_details_omitted"], true);
        let invalid: serde_json::Value =
            serde_json::from_str(&checkpoint_model_context("invalid"))?;
        assert_eq!(invalid["checkpoint_unavailable"], true);
        Ok(())
    }

    #[test]
    fn file_fingerprints_detect_changes_and_deletions() -> Result<()> {
        let root = tempfile::tempdir()?;
        std::fs::write(root.path().join("source"), "before")?;
        let observed = CheckpointFile::observe(root.path(), Path::new("source"))?;
        assert!(!observed.has_changed(root.path())?);
        std::fs::write(root.path().join("source"), "after")?;
        assert!(observed.has_changed(root.path())?);
        std::fs::remove_file(root.path().join("source"))?;
        assert!(observed.has_changed(root.path())?);
        assert!(CheckpointFile::observe(root.path(), Path::new("../outside")).is_err());
        Ok(())
    }

    #[test]
    fn reading_missing_checkpoints_does_not_create_project_metadata() -> Result<()> {
        let root = tempfile::tempdir()?;
        assert!(ProjectCheckpointStore::open_existing(root.path())?.is_none());
        assert!(
            load_checkpoint_context(&[CheckpointRoot {
                path: root.path().to_path_buf(),
                git: None,
            }])?
            .is_none()
        );
        assert!(
            !root
                .path()
                .join(paths::local_settings_folder_name())
                .exists()
        );
        Ok(())
    }

    #[test]
    fn fingerprints_skip_large_and_non_regular_files() -> Result<()> {
        let root = tempfile::tempdir()?;
        std::fs::create_dir(root.path().join("directory"))?;
        assert!(CheckpointFile::observe(root.path(), Path::new("directory")).is_err());
        let file = std::fs::File::create(root.path().join("large"))?;
        file.set_len(8 * 1024 * 1024 + 1)?;
        assert!(CheckpointFile::observe(root.path(), Path::new("large")).is_err());
        Ok(())
    }

    #[test]
    fn git_tracking_excludes_only_checkpoint_database_files() {
        for name in [
            "checkpoints.sqlite3",
            "checkpoints.sqlite3-journal",
            "checkpoints.sqlite3-wal",
            "checkpoints.sqlite3-shm",
        ] {
            assert!(is_checkpoint_database_path(&Path::new(".lin").join(name)));
        }
        for path in [
            ".lin/settings.json",
            ".lin/tasks.json",
            ".lin/checkpoints.sqlite3.backup",
            "nested/.lin/checkpoints.sqlite3",
            "checkpoints.sqlite3",
        ] {
            assert!(!is_checkpoint_database_path(Path::new(path)));
        }
    }

    #[cfg(unix)]
    #[test]
    fn checkpoint_database_rejects_links_outside_project() -> Result<()> {
        let root = tempfile::tempdir()?;
        let outside = tempfile::tempdir()?;
        std::os::unix::fs::symlink(outside.path(), root.path().join(".lin"))?;
        assert!(ProjectCheckpointStore::open(root.path()).is_err());
        assert!(!outside.path().join("checkpoints.sqlite3").exists());
        std::fs::remove_file(root.path().join(".lin"))?;
        std::fs::create_dir(root.path().join(".lin"))?;
        let outside_database = outside.path().join("database");
        std::fs::write(&outside_database, b"unchanged")?;
        std::os::unix::fs::symlink(
            &outside_database,
            root.path().join(".lin/checkpoints.sqlite3"),
        )?;
        assert!(ProjectCheckpointStore::open(root.path()).is_err());
        assert_eq!(std::fs::read(outside_database)?, b"unchanged");
        Ok(())
    }

    #[test]
    fn interrupted_snapshot_preserves_semantic_handoff() -> Result<()> {
        let root = tempfile::tempdir()?;
        std::fs::write(root.path().join("file.rs"), "contents")?;
        std::fs::write(root.path().join("saved.rs"), "before")?;
        std::fs::write(root.path().join("unchanged.rs"), "unchanged")?;
        std::fs::create_dir(root.path().join("unreadable"))?;
        let store = ProjectCheckpointStore::open(root.path())?;
        let handoff = ProjectCheckpoint {
            id: "handoff".into(),
            session_id: "first".into(),
            scope: "agent".into(),
            created_at: Utc::now(),
            base_revision: None,
            branch: None,
            git_scan_id: None,
            completed: vec![],
            remaining: vec!["verify behavior".into()],
            decisions: vec![],
            next_steps: vec![],
            changed_files: vec![],
            evidence: vec![],
            interrupted: false,
        };
        store.save(&handoff)?;
        AutomaticCheckpoint {
            root: root.path().to_path_buf(),
            session_id: "second".into(),
            files: BTreeMap::from([
                (PathBuf::from("file.rs"), true),
                (PathBuf::from("saved.rs"), false),
                (PathBuf::from("unchanged.rs"), false),
                (PathBuf::from("unreadable"), false),
            ]),
            git: None,
            evidence: vec![],
            interrupted: true,
        }
        .save()?;
        assert_eq!(store.latest_any()?, Some(handoff.clone()));
        let interruption = store.latest_automatic()?.context("interruption")?;
        assert!(interruption.interrupted);
        assert!(interruption.changed_files.first().context("file")?.unsaved);
        std::fs::write(root.path().join("saved.rs"), "changed later")?;
        let context = load_checkpoint_context(&[CheckpointRoot {
            path: root.path().to_path_buf(),
            git: None,
        }])?
        .context("context")?;
        let context: serde_json::Value = serde_json::from_str(&context)?;
        assert_eq!(
            context[0]["remaining"],
            serde_json::json!(["verify behavior"])
        );
        assert_eq!(
            context[0]["recent_automatic_checkpoint"]["session_id"],
            "second"
        );
        assert_eq!(
            context[0]["recent_automatic_checkpoint"]["files"][0]["unsaved"],
            true
        );
        assert!(
            context[0]["recent_automatic_checkpoint"]["files"][0]["changed_since_checkpoint"]
                .is_null()
        );
        assert_eq!(
            context[0]["recent_automatic_checkpoint"]["files"][1]["changed_since_checkpoint"],
            true
        );
        assert_eq!(
            context[0]["recent_automatic_checkpoint"]["files"][2]["changed_since_checkpoint"],
            false
        );
        assert!(
            context[0]["recent_automatic_checkpoint"]["files"][3]["changed_since_checkpoint"]
                .is_null()
        );
        let mut later_handoff = handoff;
        later_handoff.id = "unrelated".into();
        later_handoff.session_id = "third".into();
        store.save(&later_handoff)?;
        assert!(store.latest_automatic()?.is_some());
        later_handoff.id = "resumed".into();
        later_handoff.session_id = "second".into();
        store.save(&later_handoff)?;
        assert!(store.latest_automatic()?.is_none());
        let session_context = load_checkpoint_context_for_session(
            &[CheckpointRoot {
                path: root.path().to_path_buf(),
                git: None,
            }],
            Some("second"),
        )?
        .context("session context")?;
        let session_context: serde_json::Value = serde_json::from_str(&session_context)?;

        assert!(session_context[0].get("current_revision_changed").is_none());
        assert!(session_context[0].get("new_dirty_files").is_none());
        assert!(session_context[0].get("current_git_scan_id").is_none());
        let scopes = session_context[0]["available_scopes"]
            .as_array()
            .context("available_scopes must stay an array")?;
        assert!(
            scopes.iter().all(|scope| !scope["scope"]
                .as_str()
                .is_some_and(|scope| scope.starts_with(AUTOMATIC_CHECKPOINT_SCOPE_PREFIX))),
            "observation records stay out of the scope index: {scopes:?}"
        );
        // Every readable scope is already injected as the primary handoff, so
        // there is nothing left to hint at.
        assert!(scopes.is_empty());
        assert_eq!(session_context[0]["available_scope_count"], 0);
        Ok(())
    }

    #[test]
    fn truncation_prefers_word_boundaries_and_reports_omitted_characters() {
        let (text, omitted) = truncate_for_model("alpha beta gamma delta", 12);
        assert_eq!(text, "alpha beta");
        assert_eq!(omitted, Some(12));

        let (text, omitted) = truncate_for_model("short", 10);
        assert_eq!(text, "short");
        assert_eq!(omitted, None);

        let (text, omitted) = truncate_for_model(&"x".repeat(7000), 180);
        assert_eq!(text.chars().count(), 180);
        assert_eq!(omitted, Some(6820));
        assert_eq!(
            truncate_with_marker("alpha beta gamma delta", 12),
            "alpha beta …[+12 chars]"
        );
    }

    #[test]
    fn scope_ranking_prefers_the_current_branch_then_recency() {
        let now = Utc::now();
        let summary = |scope: &str, branch: Option<&str>, seconds: i64| CheckpointScopeSummary {
            scope: scope.into(),
            created_at: now + chrono::Duration::seconds(seconds),
            remaining_count: 0,
            next_step: None,
            branch: branch.map(str::to_owned),
        };
        let ranked = rank_scopes(
            vec![
                summary("old-main", Some("main"), 0),
                summary("new-feature", Some("feature"), 10),
                summary("new-main", Some("main"), 5),
                summary("unknown", None, 20),
                summary("primary", Some("main"), 30),
            ],
            Some("main"),
            "primary",
        );
        let scopes = ranked
            .iter()
            .map(|summary| summary.scope.as_str())
            .collect::<Vec<_>>();
        assert_eq!(scopes, ["new-main", "old-main", "unknown", "new-feature"]);
    }

    #[test]
    fn handoff_reports_new_changes_and_keeps_json_valid_when_summaries_are_long() -> Result<()> {
        let root = tempfile::tempdir()?;
        std::fs::write(root.path().join("edited.rs"), "before")?;
        let file = CheckpointFile::observe(root.path(), Path::new("edited.rs"))?;
        let store = ProjectCheckpointStore::open(root.path())?;
        store.save(&ProjectCheckpoint {
            id: "handoff".into(),
            session_id: "session".into(),
            scope: "agent".into(),
            created_at: Utc::now(),
            base_revision: Some("old".into()),
            branch: Some("main".into()),
            git_scan_id: Some(5),
            completed: vec!["x".repeat(7000)],
            remaining: vec!["unfinished".into()],
            decisions: vec![],
            next_steps: vec![],
            changed_files: vec![file],
            evidence: vec![],
            interrupted: false,
        })?;
        std::fs::write(root.path().join("edited.rs"), "after")?;
        let context = load_checkpoint_context(&[CheckpointRoot {
            path: root.path().to_path_buf(),
            git: Some(CheckpointGitSnapshot {
                revision: Some("new".into()),
                branch: Some("feature".into()),
                scan_id: 6,
                dirty_files: std::iter::once(PathBuf::from("edited.rs"))
                    .chain(std::iter::once(PathBuf::from("new.rs")))
                    .chain(
                        (0..12)
                            .map(|index| PathBuf::from(format!("{}-{index}", "long".repeat(60)))),
                    )
                    .collect(),
            }),
        }])?
        .context("checkpoint context")?;
        let context: serde_json::Value = serde_json::from_str(&context)?;
        let entry = context.get(0).context("root context")?;
        assert_eq!(entry["current_revision_changed"], true);
        assert_eq!(entry["current_branch_changed"], true);
        assert_eq!(entry["changed_files"], serde_json::json!(["edited.rs"]));
        assert_eq!(entry["new_dirty_files"][0], "new.rs");
        assert_eq!(entry["new_dirty_files"].as_array().map(Vec::len), Some(10));
        assert_eq!(
            entry["new_dirty_files"][1].as_str().map(str::len),
            Some(180)
        );
        assert_eq!(entry["new_dirty_files_may_be_truncated"], true);
        assert_eq!(entry["summary_may_be_truncated"], true);
        assert_eq!(entry["remaining"], serde_json::json!(["unfinished"]));
        Ok(())
    }
}
