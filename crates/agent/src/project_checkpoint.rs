use std::{
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
    pub observed_at: DateTime<Utc>,
    pub revision: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckpointFile {
    pub path: PathBuf,
    pub fingerprint: Option<String>,
    #[serde(default)]
    pub unsaved: bool,
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
                let mut file = std::fs::File::open(&resolved)
                    .with_context(|| format!("Could not read {}", full_path.display()))?;
                let mut digest = Sha256::new();
                let mut buffer = [0_u8; 65536];
                loop {
                    let count = file
                        .read(&mut buffer)
                        .with_context(|| format!("Could not read {}", full_path.display()))?;
                    if count == 0 {
                        break;
                    }
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
        })
    }

    pub fn has_changed(&self, root: &Path) -> Result<bool> {
        Ok(self.unsaved || Self::observe(root, &self.path)?.fingerprint != self.fingerprint)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectCheckpoint {
    pub id: String,
    pub session_id: String,
    pub scope: String,
    pub created_at: DateTime<Utc>,
    pub base_revision: Option<String>,
    pub completed: Vec<String>,
    pub remaining: Vec<String>,
    pub decisions: Vec<String>,
    pub next_steps: Vec<String>,
    pub changed_files: Vec<CheckpointFile>,
    pub evidence: Vec<CheckpointEvidence>,
    pub interrupted: bool,
}

/// A project's checkpoint history, stored separately for each checkout.
pub struct ProjectCheckpointStore {
    connection: Connection,
}

pub fn load_checkpoint_context(roots: &[PathBuf]) -> Result<Option<String>> {
    let mut sections = Vec::new();
    for root in roots {
        let Some(store) = ProjectCheckpointStore::open_existing(root)? else {
            continue;
        };
        let Some(checkpoint) = store.latest_any()? else {
            continue;
        };
        let mut changed = Vec::new();
        let mut unknown = Vec::new();
        for file in &checkpoint.changed_files {
            match file.has_changed(root) {
                Ok(true) => changed.push(file.path.display().to_string()),
                Ok(false) => {}
                Err(_) => unknown.push(file.path.display().to_string()),
            }
        }
        let data = serde_json::json!({
            "root": root, "scope": checkpoint.scope, "created_at": checkpoint.created_at,
            "completed": checkpoint.completed, "remaining": checkpoint.remaining,
            "decisions": checkpoint.decisions, "next_steps": checkpoint.next_steps,
            "interrupted": checkpoint.interrupted, "changed_files": changed,
            "files_not_checked": unknown, "base_revision": checkpoint.base_revision,
        });
        sections.push(data);
    }
    if sections.is_empty() {
        return Ok(None);
    }
    let text = serde_json::to_string(&sections)?;
    Ok(Some(text.chars().take(6000).collect()))
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
        let directory = root.join(paths::local_settings_folder_name());
        std::fs::create_dir_all(&directory)
            .with_context(|| format!("Could not create {}", directory.display()))?;
        let database_path = directory.join("checkpoints.sqlite3");
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

    pub fn latest_any(&self) -> Result<Option<ProjectCheckpoint>> {
        let records = self.connection.select_bound::<(), String>(
            "SELECT data FROM checkpoints ORDER BY sequence DESC LIMIT 1",
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
    fn checkpoints_persist_per_scope_and_reject_duplicate_ids() -> Result<()> {
        let root = tempfile::tempdir()?;
        let store = ProjectCheckpointStore::open(root.path())?;
        let mut checkpoint = ProjectCheckpoint {
            id: "first".into(),
            session_id: "session".into(),
            scope: "agent".into(),
            created_at: Utc::now(),
            base_revision: Some("revision".into()),
            completed: vec!["Implemented".into()],
            remaining: vec!["Verify".into()],
            decisions: vec![],
            next_steps: vec![],
            changed_files: vec![CheckpointFile {
                path: "src/main.rs".into(),
                fingerprint: None,
                unsaved: false,
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
        assert_eq!(reopened.latest("agent")?, Some(checkpoint));
        assert_eq!(reopened.latest("other")?, None);
        assert!(root.path().join(".lin/checkpoints.sqlite3").exists());
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
}
