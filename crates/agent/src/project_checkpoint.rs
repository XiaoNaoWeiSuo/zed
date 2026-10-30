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
                .map(|step| step.chars().take(180).collect()),
        }
    }
}

/// A project's checkpoint history, stored separately for each checkout.
pub struct ProjectCheckpointStore {
    connection: Connection,
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
                            store.list_scopes(0, 6)?,
                        ))
                    } else {
                        Ok((
                            store.latest_any()?,
                            store.latest_automatic()?,
                            None,
                            store.list_scopes(0, 6)?,
                        ))
                    }
                }
                None => Ok((None, None, None, Vec::new())),
            });
        let (checkpoint, interruption, compaction, scopes) = match checkpoints {
            Ok((None, None, None, _)) => continue,
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
                .map(|entry| entry.chars().take(180).collect::<String>())
                .collect::<Vec<_>>()
        };
        let bounded_path =
            |path: &Path| path.to_string_lossy().chars().take(180).collect::<String>();
        let path_too_long = |path: &Path| path.to_string_lossy().chars().count() > 180;
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
            "available_scopes": scopes.iter().take(5).collect::<Vec<_>>(),
            "more_scopes_available": scopes.len() > 5,
            "completed": bounded(&checkpoint.completed), "remaining": bounded(&checkpoint.remaining),
            "decisions": bounded(&checkpoint.decisions), "next_steps": bounded(&checkpoint.next_steps),
            "evidence": checkpoint.evidence.iter().take(5).map(|entry| serde_json::json!({"description": entry.description.chars().take(180).collect::<String>(), "source": entry.source.chars().take(240).collect::<String>(), "observed_at": entry.observed_at, "revision": entry.revision})).collect::<Vec<_>>(),
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
                "evidence": interruption.evidence.iter().take(5).map(|entry| serde_json::json!({"description": entry.description.chars().take(180).collect::<String>(), "source": entry.source.chars().take(240).collect::<String>()})).collect::<Vec<_>>(),
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
        Ok(Self { connection })
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
            "SELECT data FROM checkpoints WHERE sequence IN (SELECT MAX(sequence) FROM checkpoints WHERE scope NOT GLOB '@automatic/*' GROUP BY scope) ORDER BY sequence DESC LIMIT ?1 OFFSET ?2",
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
    fn scope_index_lists_latest_handoffs_in_pages_without_automatic_records() -> Result<()> {
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
        assert_eq!(context[0]["available_scopes"][1]["scope"], "editor");
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
            .context("a session that can retrieve on demand must list the readable scopes")?;
        assert!(
            !scopes.is_empty(),
            "available_scopes must carry retrieval hints, otherwise on-demand lookup has no entry point"
        );
        assert!(
            scopes.iter().all(|scope| !scope["scope"]
                .as_str()
                .is_some_and(|scope| scope.starts_with(AUTOMATIC_CHECKPOINT_SCOPE_PREFIX))),
            "observation records stay out of the scope index: {scopes:?}"
        );
        Ok(())
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
