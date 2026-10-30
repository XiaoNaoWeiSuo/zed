use std::{collections::BTreeMap, path::PathBuf};

use anyhow::{Context as _, Result, anyhow, ensure};
use chrono::{DateTime, Utc};
use futures::StreamExt as _;
use gpui::{AppContext as _, AsyncApp, Entity};
use project::{Project, WorktreeSettings};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use settings::Settings as _;
use sha2::{Digest as _, Sha256};

use super::tool_permissions::{
    ResolvedProjectPath, canonicalize_worktree_roots, resolve_project_path,
};

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct GoalVerificationInput {
    /// An enabled read, search, or terminal tool. Its usual permissions still apply.
    pub tool_name: String,
    /// Arguments matching that tool's schema.
    pub tool_input: serde_json::Value,
    /// Project files or directories on which this check depends, including relevant manifests and configuration.
    pub dependencies: Vec<String>,
    /// Zero-based indices of the success criteria this check covers. Omit when
    /// the goal has one criterion; multi-criterion receipts need this to be
    /// selected automatically at completion.
    #[serde(default)]
    pub criterion_indices: Vec<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoalDependencySnapshot {
    pub path: String,
    pub digest: String,
    pub file_count: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalVerificationOutcome {
    Passed,
    Failed,
    Changed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoalCommandStatus {
    pub exit_code: Option<i64>,
    pub timed_out: bool,
    pub user_stopped: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoalVerificationReceipt {
    pub id: String,
    pub tool_call_id: String,
    pub tool_name: String,
    pub tool_input: serde_json::Value,
    pub observed_at: DateTime<Utc>,
    pub dependencies: Vec<GoalDependencySnapshot>,
    pub outcome: GoalVerificationOutcome,
    #[serde(default)]
    pub command_status: Option<GoalCommandStatus>,
    pub output_digest: String,
    pub preview: String,
    pub stale_reason: Option<String>,
    /// Criteria declared when the check was recorded. Older persisted
    /// receipts have no association; a single-criterion goal treats them as
    /// covering criterion 0.
    #[serde(default)]
    pub criterion_indices: Vec<usize>,
}

pub(crate) async fn capture_goal_dependencies(
    project: &Entity<Project>,
    paths: &[String],
    cx: &mut AsyncApp,
) -> Result<Vec<GoalDependencySnapshot>> {
    ensure!(
        !paths.is_empty() && paths.len() <= 32,
        "Declare 1 to 32 dependency files or directories"
    );
    let filesystem = project.read_with(cx, |project, _| project.fs().clone());
    let canonical_roots = canonicalize_worktree_roots(project, &filesystem, cx).await;
    let worktree_roots = project.read_with(cx, |project, cx| {
        project
            .worktrees(cx)
            .map(|worktree| worktree.read(cx).abs_path().to_path_buf())
            .collect::<Vec<_>>()
    });
    let mut roots = Vec::new();
    for path in paths {
        let absolute = project.read_with(cx, |project, cx| {
            let ResolvedProjectPath::Safe(project_path) =
                resolve_project_path(project, path, &canonical_roots, cx)?
            else {
                return Err(anyhow!(
                    "Dependency {path} follows a symlink outside the project"
                ));
            };
            project
                .absolute_path(&project_path, cx)
                .context("Dependency has no local filesystem path")
        })?;
        roots.push((path.clone(), absolute));
    }
    ensure_saved_dependencies(project, &roots, cx)?;
    let mut result = Vec::new();
    let mut resolved_files = Vec::new();
    let mut total_bytes = 0_u64;
    let mut total_entries = 0_usize;
    for (display, root) in &roots {
        let mut pending = vec![root.clone()];
        let mut entries = BTreeMap::new();
        let mut file_count = 0;
        while let Some(path) = pending.pop() {
            total_entries += 1;
            ensure!(
                total_entries <= 10_000,
                "Dependency scan exceeds 10000 entries; choose narrower source directories"
            );
            let metadata = filesystem
                .metadata(&path)
                .await?
                .with_context(|| format!("Dependency {} no longer exists", path.display()))?;
            ensure!(
                !metadata.is_fifo,
                "Dependency {} is not a regular file or directory",
                path.display()
            );
            let canonical = filesystem.canonicalize(&path).await?;
            let (canonical_root, worktree_root) = canonical_roots
                .iter()
                .zip(&worktree_roots)
                .filter(|(root, _)| canonical.starts_with(root))
                .max_by_key(|(root, _)| root.components().count())
                .with_context(|| {
                    format!("Dependency {} resolves outside the project", path.display())
                })?;
            let target = worktree_root.join(canonical.strip_prefix(canonical_root)?);
            project.read_with(cx, |project, cx| -> Result<()> {
                for candidate in [&path, &target] {
                    let project_path = project
                        .project_path_for_absolute_path(candidate, cx)
                        .context("Dependency left the project")?;
                    for settings in [
                        WorktreeSettings::get_global(cx),
                        WorktreeSettings::get(Some((&project_path).into()), cx),
                    ] {
                        ensure!(
                            !settings.is_path_excluded(&project_path.path)
                                && !settings.is_path_private(&project_path.path),
                            "Dependency {} is private or excluded; choose narrower dependencies",
                            candidate.display()
                        );
                    }
                }
                Ok(())
            })?;
            let relative = path
                .strip_prefix(root)
                .context("Invalid dependency subtree")?
                .to_str()
                .context("Dependency paths must be valid UTF-8")?
                .to_string();
            let canonical_text = canonical
                .to_str()
                .context("Dependency targets must be valid UTF-8")?;
            if metadata.is_dir {
                ensure!(
                    !metadata.is_symlink,
                    "Use the real project directory instead of directory symlink {}",
                    path.display()
                );
                entries.insert(relative, format!("directory:{canonical_text}"));
                let mut children = filesystem.read_dir(&canonical).await?;
                while let Some(child) = children.next().await {
                    let child = child?;
                    let name = child
                        .file_name()
                        .context("Dependency entry has no filename")?;
                    pending.push(path.join(name));
                }
            } else {
                resolved_files.push((display.clone(), target));
                total_bytes = total_bytes
                    .checked_add(metadata.len)
                    .context("Dependency size overflow")?;
                ensure!(
                    total_bytes <= 64 * 1024 * 1024,
                    "Dependency scan exceeds 64 MiB; choose narrower source dependencies"
                );
                let bytes = filesystem.load_bytes(&canonical).await?;
                let after = filesystem
                    .metadata(&path)
                    .await?
                    .context("Dependency disappeared while reading")?;
                ensure!(
                    metadata.inode == after.inode
                        && metadata.mtime == after.mtime
                        && metadata.len == after.len
                        && metadata.is_symlink == after.is_symlink
                        && metadata.is_executable == after.is_executable
                        && canonical == filesystem.canonicalize(&path).await?,
                    "Dependency {} changed while reading; retry the check",
                    path.display()
                );
                let digest = cx
                    .background_spawn(async move { format!("{:x}", Sha256::digest(bytes)) })
                    .await;
                entries.insert(
                    relative,
                    format!("file:{canonical_text}:{}:{digest}", metadata.is_executable),
                );
                file_count += 1;
            }
        }
        let digest = format!("{:x}", Sha256::digest(serde_json::to_vec(&entries)?));
        result.push(GoalDependencySnapshot {
            path: display.clone(),
            digest,
            file_count,
        });
    }
    ensure_saved_dependencies(project, &roots, cx)?;
    ensure_saved_dependencies(project, &resolved_files, cx)?;
    result.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(result)
}

fn ensure_saved_dependencies(
    project: &Entity<Project>,
    roots: &[(String, PathBuf)],
    cx: &AsyncApp,
) -> Result<()> {
    project.read_with(cx, |project, cx| {
        for buffer in project.opened_buffers(cx) {
            let buffer = buffer.read(cx);
            if buffer.is_dirty()
                && let Some(file) = buffer.file().and_then(|file| file.as_local())
            {
                let absolute = file.abs_path(cx);
                ensure!(
                    !roots
                        .iter()
                        .any(|(_, root)| absolute == *root || absolute.starts_with(root)),
                    "Save unsaved dependency {} before verifying it",
                    absolute.display()
                );
            }
        }
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use fs::{FakeFs, Fs as _, RemoveOptions};
    use gpui::TestAppContext;
    use serde_json::json;

    #[gpui::test]
    async fn dependency_snapshots_track_content_and_directory_membership(cx: &mut TestAppContext) {
        crate::tests::init_test(cx);
        let filesystem = FakeFs::new(cx.executor());
        filesystem.insert_tree("/root", json!({"src": {"source.txt": "before"}, "manifest": "config", "notes": "unrelated"})).await;
        let project = Project::test(filesystem.clone(), [std::path::Path::new("/root")], cx).await;
        let capture = |cx: &mut TestAppContext| {
            let project = project.clone();
            cx.update(|cx| {
                cx.spawn(async move |cx| {
                    capture_goal_dependencies(
                        &project,
                        &["root/src".into(), "root/manifest".into()],
                        cx,
                    )
                    .await
                })
            })
        };
        let original = capture(cx).await.expect("snapshot");
        filesystem
            .write(
                std::path::Path::new("/root/notes"),
                b"changed outside dependencies",
            )
            .await
            .expect("unrelated edit");
        assert_eq!(capture(cx).await.expect("unchanged dependencies"), original);
        filesystem
            .write(std::path::Path::new("/root/src/source.txt"), b"after!")
            .await
            .expect("source edit");
        assert_ne!(capture(cx).await.expect("modified dependencies"), original);
        filesystem
            .write(std::path::Path::new("/root/src/source.txt"), b"before")
            .await
            .expect("restore source");
        assert_eq!(capture(cx).await.expect("restored dependency"), original);
        filesystem
            .insert_tree("/root/src", json!({"added.txt": "new input"}))
            .await;
        assert_ne!(capture(cx).await.expect("directory membership"), original);
        filesystem
            .remove_file(
                std::path::Path::new("/root/manifest"),
                RemoveOptions::default(),
            )
            .await
            .expect("remove input");
        assert!(
            capture(cx).await.is_err(),
            "missing inputs cannot be treated as fresh"
        );
    }

    #[gpui::test]
    async fn dependency_snapshots_reject_unsaved_private_and_escaped_inputs(
        cx: &mut TestAppContext,
    ) {
        crate::tests::init_test(cx);
        let filesystem = FakeFs::new(cx.executor());
        filesystem
            .insert_tree("/root", json!({"source.txt": "saved", ".env": "private"}))
            .await;
        filesystem
            .insert_tree("/outside", json!({"secret": "external"}))
            .await;
        let project = Project::test(filesystem.clone(), [std::path::Path::new("/root")], cx).await;
        let capture = |path: &str, cx: &mut TestAppContext| {
            let project = project.clone();
            let path = path.to_string();
            cx.update(|cx| {
                cx.spawn(async move |cx| capture_goal_dependencies(&project, &[path], cx).await)
            })
        };
        assert!(
            capture("root/.env", cx).await.is_err(),
            "private files are not a hashing bypass"
        );
        filesystem
            .create_symlink(
                std::path::Path::new("/root/escape"),
                std::path::PathBuf::from("/outside/secret"),
            )
            .await
            .expect("symlink");
        assert!(
            capture("root/escape", cx).await.is_err(),
            "hashing cannot follow external symlinks"
        );
        filesystem
            .create_symlink(
                std::path::Path::new("/root/private_alias"),
                std::path::PathBuf::from("/root/.env"),
            )
            .await
            .expect("private symlink");
        assert!(
            capture("root/private_alias", cx).await.is_err(),
            "symlinks cannot expose private dependencies"
        );
        filesystem
            .create_symlink(
                std::path::Path::new("/root/source_alias"),
                std::path::PathBuf::from("/root/source.txt"),
            )
            .await
            .expect("source symlink");
        let buffer = project
            .update(cx, |project, cx| {
                let path = project
                    .find_project_path("root/source.txt", cx)
                    .expect("source path");
                project.open_buffer(path, cx)
            })
            .await
            .expect("buffer");
        buffer.update(cx, |buffer, cx| buffer.edit([(0..0, "unsaved ")], None, cx));
        assert!(
            capture("root/source.txt", cx).await.is_err(),
            "a command cannot verify unsaved editor contents"
        );
        assert!(
            capture("root/source_alias", cx).await.is_err(),
            "a symlink must not hide an unsaved target"
        );
    }
}
