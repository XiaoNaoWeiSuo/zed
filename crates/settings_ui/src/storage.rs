//! Disk usage accounting and cleanup for the data Lin owns on this machine.
//!
//! Everything in here works against well-known paths owned by the app (see the
//! `paths` crate) plus git worktrees that Lin created for agent threads. User
//! data such as conversations, installed extensions and databases is reported
//! but never cleaned up automatically.

use std::{
    collections::HashSet,
    fs as std_fs,
    io::ErrorKind,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use fs::Fs as FsTrait;
use gpui::{App, AppContext as _, Context, Entity, Global, SharedString};
use project::{git_store::worktrees_directory_for_repo, project_settings::ProjectSettings};
use settings::Settings as _;
use util::{ResultExt as _, paths::PathStyle};
use walkdir::WalkDir;

#[derive(Clone, Debug, Default)]
pub struct WorktreeReferences {
    pub conversations: Vec<SharedString>,
    pub terminals: Vec<SharedString>,
}

struct WorktreeReferenceProvider(fn(&Path, &App) -> Option<WorktreeReferences>);

impl Global for WorktreeReferenceProvider {}

pub fn register_worktree_reference_provider(
    provider: fn(&Path, &App) -> Option<WorktreeReferences>,
    cx: &mut App,
) {
    cx.set_global(WorktreeReferenceProvider(provider));
}

use settings::StorageSettings;

/// A group of files Lin owns, reported as a single row in the settings UI.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum StorageCategoryKind {
    Logs,
    HangTraces,
    TemporaryFiles,
    LanguageServers,
    AgentServers,
    NodeRuntime,
    Prettier,
    DebugAdapters,
    Copilot,
    Embeddings,
    RemoteServers,
    DevContainers,
    RemoteExtensions,
    ConversationHistory,
    Databases,
    Extensions,
    /// Aggregated git worktrees discovered across open and recorded projects.
    ProjectWorktrees,
    /// Aggregated agent checkpoint databases stored in each project's `.lin`
    /// directory.
    ProjectCheckpoints,
    /// Sandboxed terminal directories and extracted archives left behind in the
    /// system temporary directory.
    TemporaryWorkingFolders,
    /// State written by the js-debug companion process.
    DebuggerCompanionState,
    /// State uploaded to remote hosts when running the remote server.
    RemoteServerState,
}

impl StorageCategoryKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Logs => "Logs",
            Self::HangTraces => "Hang Traces",
            Self::TemporaryFiles => "Temporary Files",
            Self::LanguageServers => "Language Servers",
            Self::AgentServers => "Agent Servers",
            Self::NodeRuntime => "Node Runtime",
            Self::Prettier => "Prettier",
            Self::DebugAdapters => "Debug Adapters",
            Self::Copilot => "Copilot",
            Self::Embeddings => "Search Embeddings",
            Self::RemoteServers => "Remote Servers",
            Self::DevContainers => "Dev Containers",
            Self::RemoteExtensions => "Remote Extensions",
            Self::ConversationHistory => "Conversation History",
            Self::Databases => "Databases",
            Self::Extensions => "Extensions",
            Self::ProjectWorktrees => "Project Worktrees",
            Self::ProjectCheckpoints => "Project Checkpoints",
            Self::TemporaryWorkingFolders => "Temporary Working Folders",
            Self::DebuggerCompanionState => "Debugger Companion",
            Self::RemoteServerState => "Remote Server State",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::Logs => "Diagnostic logs written by Lin and the tools it runs.",
            Self::HangTraces => "Traces captured when the app stops responding.",
            Self::TemporaryFiles => "GPU shader caches, ephemeral files and other scratch data.",
            Self::LanguageServers => "Downloaded language servers and their bundled runtimes.",
            Self::AgentServers => "Downloaded binaries for external agents.",
            Self::NodeRuntime => {
                "The bundled Node.js runtime used by language servers and extensions."
            }
            Self::Prettier => "Downloaded Prettier versions used to format files.",
            Self::DebugAdapters => "Downloaded debug adapters.",
            Self::Copilot => "Files used by the GitHub Copilot integration.",
            Self::Embeddings => "Vectors used to power semantic search.",
            Self::RemoteServers => "Server binaries uploaded to SSH and remote hosts.",
            Self::DevContainers => "The devcontainer CLI downloaded for container support.",
            Self::RemoteExtensions => "Extensions installed on remote hosts.",
            Self::ConversationHistory => "Agent conversations, summaries and thread metadata.",
            Self::Databases => "Workspace layouts, key-value stores and other app databases.",
            Self::Extensions => "Installed extensions and their assets.",
            Self::ProjectWorktrees => {
                "Git worktrees checked out for projects, including the ones created by agent threads."
            }
            Self::ProjectCheckpoints => {
                "Agent checkpoints and project-local Lin metadata stored in each project's .lin folder."
            }
            Self::TemporaryWorkingFolders => {
                "Sandboxed terminal directories and extracted archives left in the system temp folder."
            }
            Self::DebuggerCompanionState => "State written by the js-debug companion process.",
            Self::RemoteServerState => "State uploaded to remote hosts for the Lin server.",
        }
    }

    pub fn policy(self) -> CleanupPolicy {
        match self {
            Self::Logs | Self::HangTraces | Self::TemporaryFiles => CleanupPolicy::Safe,
            Self::LanguageServers
            | Self::AgentServers
            | Self::NodeRuntime
            | Self::Prettier
            | Self::DebugAdapters
            | Self::Copilot
            | Self::Embeddings
            | Self::RemoteServers
            | Self::DevContainers
            | Self::RemoteExtensions => CleanupPolicy::Reinstallable,
            Self::TemporaryWorkingFolders | Self::DebuggerCompanionState => CleanupPolicy::Safe,
            Self::RemoteServerState => CleanupPolicy::Reinstallable,
            Self::ConversationHistory
            | Self::Databases
            | Self::Extensions
            | Self::ProjectWorktrees
            | Self::ProjectCheckpoints => CleanupPolicy::Protected,
        }
    }
}

/// How a category may be reclaimed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CleanupPolicy {
    /// Deleting this cannot affect the running app.
    Safe,
    /// Deleting this is safe, but the contents are downloaded again on demand.
    Reinstallable,
    /// User data. Never cleaned from this page.
    Protected,
}

impl CleanupPolicy {
    pub fn is_reclaimable(self) -> bool {
        !matches!(self, Self::Protected)
    }
}

#[derive(Clone, Debug)]
pub struct CategorySpec {
    pub kind: StorageCategoryKind,
    pub path: PathBuf,
    /// When non-empty, only entries whose file name starts with one of these
    /// prefixes are measured or removed. Used for app-owned directories inside
    /// the shared system temp folder.
    pub prefixes: &'static [&'static str],
}

impl CategorySpec {
    fn new(kind: StorageCategoryKind, path: impl Into<PathBuf>) -> Self {
        Self {
            kind,
            path: path.into(),
            prefixes: &[],
        }
    }

    fn with_prefixes(
        kind: StorageCategoryKind,
        path: impl Into<PathBuf>,
        prefixes: &'static [&'static str],
    ) -> Self {
        Self {
            kind,
            path: path.into(),
            prefixes,
        }
    }
}

/// Prefixes of directories and files Lin creates in the system temp folder.
///
/// These are not under `paths::temp_dir()`, so they are easy to miss when
/// accounting for an agent session's disk usage.
pub const TEMP_FOLDER_PREFIXES: &[&str] = &[
    "zed-agent-terminal-",
    "devcontainer-zed",
    "zed-mcp",
    "zed-askpass",
    "zed-ssh-kernel_",
];

/// Every location the storage page knows about, in display order.
pub fn category_specs() -> Vec<CategorySpec> {
    vec![
        CategorySpec::new(StorageCategoryKind::Logs, paths::logs_dir().clone()),
        CategorySpec::new(
            StorageCategoryKind::HangTraces,
            paths::hang_traces_dir().clone(),
        ),
        CategorySpec::new(
            StorageCategoryKind::TemporaryFiles,
            paths::temp_dir().clone(),
        ),
        CategorySpec::new(
            StorageCategoryKind::LanguageServers,
            paths::languages_dir().clone(),
        ),
        CategorySpec::new(
            StorageCategoryKind::AgentServers,
            paths::external_agents_dir().clone(),
        ),
        CategorySpec::new(
            StorageCategoryKind::NodeRuntime,
            paths::data_dir().join("node"),
        ),
        CategorySpec::new(
            StorageCategoryKind::Prettier,
            paths::default_prettier_dir().clone(),
        ),
        CategorySpec::new(
            StorageCategoryKind::DebugAdapters,
            paths::debug_adapters_dir().clone(),
        ),
        CategorySpec::new(StorageCategoryKind::Copilot, paths::copilot_dir().clone()),
        CategorySpec::new(
            StorageCategoryKind::Embeddings,
            paths::embeddings_dir().clone(),
        ),
        CategorySpec::new(
            StorageCategoryKind::RemoteServers,
            paths::remote_servers_dir().clone(),
        ),
        CategorySpec::new(
            StorageCategoryKind::DevContainers,
            paths::devcontainer_dir().clone(),
        ),
        CategorySpec::new(
            StorageCategoryKind::RemoteExtensions,
            paths::remote_extensions_dir().clone(),
        ),
        CategorySpec::new(
            StorageCategoryKind::ConversationHistory,
            paths::data_dir().join("threads"),
        ),
        CategorySpec::new(
            StorageCategoryKind::Databases,
            paths::database_dir().clone(),
        ),
        CategorySpec::new(
            StorageCategoryKind::Extensions,
            paths::extensions_dir().clone(),
        ),
        CategorySpec::new(
            StorageCategoryKind::DebuggerCompanionState,
            paths::data_dir().join("js_debug_companion_state"),
        ),
        CategorySpec::new(
            StorageCategoryKind::RemoteServerState,
            paths::remote_server_state_dir().clone(),
        ),
        CategorySpec::with_prefixes(
            StorageCategoryKind::TemporaryWorkingFolders,
            std::env::temp_dir(),
            TEMP_FOLDER_PREFIXES,
        ),
    ]
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DirUsage {
    pub bytes: u64,
    pub files: u64,
    pub skipped_entries: u64,
}

impl DirUsage {
    fn add(&mut self, other: DirUsage) {
        self.bytes = self.bytes.saturating_add(other.bytes);
        self.files = self.files.saturating_add(other.files);
        self.skipped_entries = self.skipped_entries.saturating_add(other.skipped_entries);
    }
}

/// Measures a directory tree without following symlinks.
///
/// Permission errors and files that disappear mid-scan are counted as skipped
/// rather than failing the whole scan; a storage report is still useful when a
/// few entries below it are unreadable.
pub fn measure_dir(path: &Path) -> DirUsage {
    let mut usage = DirUsage::default();
    if !path.exists() {
        return usage;
    }

    for entry in WalkDir::new(path).follow_links(false) {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                usage.skipped_entries += 1;
                log::debug!("failed to read entry while measuring {path:?}: {error}");
                continue;
            }
        };

        if !entry.file_type().is_file() {
            continue;
        }

        match entry.metadata() {
            Ok(metadata) => {
                usage.bytes = usage.bytes.saturating_add(metadata.len());
                usage.files += 1;
            }
            Err(error) => {
                usage.skipped_entries += 1;
                log::debug!("failed to stat {:?}: {error}", entry.path());
            }
        }
    }

    usage
}

/// Measures a category, honoring the prefix filter for shared directories.
fn measure_category(spec: &CategorySpec) -> DirUsage {
    if spec.prefixes.is_empty() {
        return measure_dir(&spec.path);
    }

    let mut usage = DirUsage::default();
    let entries = match std_fs::read_dir(&spec.path) {
        Ok(entries) => entries,
        Err(error) if error.kind() == ErrorKind::NotFound => return usage,
        Err(error) => {
            log::debug!("failed to read {}: {error}", spec.path.display());
            usage.skipped_entries += 1;
            return usage;
        }
    };

    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                usage.skipped_entries += 1;
                log::debug!("failed to read entry in {}: {error}", spec.path.display());
                continue;
            }
        };
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !spec.prefixes.iter().any(|prefix| name.starts_with(prefix)) {
            continue;
        }

        let path = entry.path();
        match entry.file_type() {
            Ok(file_type) if file_type.is_dir() => usage.add(measure_dir(&path)),
            Ok(_) => match entry.metadata() {
                Ok(metadata) => {
                    usage.bytes = usage.bytes.saturating_add(metadata.len());
                    usage.files += 1;
                }
                Err(error) => {
                    usage.skipped_entries += 1;
                    log::debug!("failed to stat {}: {error}", path.display());
                }
            },
            Err(error) => {
                usage.skipped_entries += 1;
                log::debug!("failed to inspect {}: {error}", path.display());
            }
        }
    }

    usage
}

#[derive(Clone, Debug)]
pub struct StorageCategory {
    pub kind: StorageCategoryKind,
    pub path: PathBuf,
    pub bytes: u64,
    pub files: u64,
    pub skipped_entries: u64,
    pub exists: bool,
    pub downloads: Vec<DownloadedItem>,
}

#[derive(Clone, Debug)]
pub struct DownloadedItem {
    pub name: std::ffi::OsString,
    pub bytes: u64,
    pub skipped_entries: u64,
}

fn scan_downloaded_items(spec: &CategorySpec) -> Vec<DownloadedItem> {
    if spec.kind.policy() != CleanupPolicy::Reinstallable || !spec.prefixes.is_empty() {
        return Vec::new();
    }
    let Ok(metadata) = std_fs::symlink_metadata(&spec.path) else {
        return Vec::new();
    };
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Vec::new();
    }
    let entries = match std_fs::read_dir(&spec.path) {
        Ok(entries) => entries,
        Err(error) => {
            log::warn!(
                "Could not list downloads at {}: {error}",
                spec.path.display()
            );
            return Vec::new();
        }
    };
    let mut downloads = Vec::new();
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                log::warn!(
                    "Could not read a download entry at {}: {error}",
                    spec.path.display()
                );
                continue;
            }
        };
        let metadata = match std_fs::symlink_metadata(entry.path()) {
            Ok(metadata) if !metadata.file_type().is_symlink() => metadata,
            Ok(_) => continue,
            Err(error) => {
                log::warn!("Could not inspect {}: {error}", entry.path().display());
                continue;
            }
        };
        if !metadata.is_file() && !metadata.is_dir() {
            continue;
        }
        let usage = if metadata.is_dir() {
            measure_dir(&entry.path())
        } else {
            DirUsage {
                bytes: metadata.len(),
                files: 1,
                skipped_entries: 0,
            }
        };
        downloads.push(DownloadedItem {
            name: entry.file_name(),
            bytes: usage.bytes,
            skipped_entries: usage.skipped_entries,
        });
    }
    downloads.sort_by(|left, right| {
        right
            .bytes
            .cmp(&left.bytes)
            .then_with(|| left.name.cmp(&right.name))
    });
    downloads
}

impl StorageCategory {
    pub fn policy(&self) -> CleanupPolicy {
        self.kind.policy()
    }
}

pub fn scan_categories(specs: &[CategorySpec]) -> Vec<StorageCategory> {
    let mut categories: Vec<StorageCategory> = specs
        .iter()
        .map(|spec| {
            let usage = measure_category(spec);
            StorageCategory {
                kind: spec.kind,
                path: spec.path.clone(),
                bytes: usage.bytes,
                files: usage.files,
                skipped_entries: usage.skipped_entries,
                exists: spec.path.exists(),
                downloads: scan_downloaded_items(spec),
            }
        })
        .filter(|category| category.exists || category.bytes > 0)
        .collect();
    categories.sort_by(|left, right| {
        right
            .bytes
            .cmp(&left.bytes)
            .then_with(|| left.kind.cmp(&right.kind))
    });
    categories
}

/// A git worktree that belongs to a project.
#[derive(Clone, Debug)]
pub struct WorktreeSpec {
    pub path: PathBuf,
    pub name: SharedString,
    pub branch: Option<SharedString>,
    /// Whether the worktree lives under the configured `git.worktree_directory`.
    pub managed: bool,
}

#[derive(Clone, Debug)]
pub struct ProjectSpec {
    pub name: SharedString,
    pub root: PathBuf,
    pub remote: bool,
    /// Whether the project is currently open, as opposed to only being present
    /// in the recent-projects database.
    pub open: bool,
    pub worktrees: Vec<WorktreeSpec>,
    /// `.lin` directories whose checkpoint databases are not part of a
    /// discovered worktree (usually the main checkout).
    pub checkpoint_paths: Vec<PathBuf>,
}

#[derive(Clone, Debug)]
pub struct WorktreeStorage {
    pub path: PathBuf,
    pub name: SharedString,
    pub branch: Option<SharedString>,
    pub managed: bool,
    pub open: bool,
    pub references: Option<WorktreeReferences>,
    /// Size of the checkout without its `.lin` checkpoint database.
    pub bytes: Option<u64>,
    pub files: Option<u64>,
    pub checkpoints: Option<u64>,
}

#[derive(Clone, Debug)]
pub struct ProjectStorage {
    pub name: SharedString,
    pub root: PathBuf,
    pub remote: bool,
    pub open: bool,
    pub worktrees: Vec<WorktreeStorage>,
    pub checkpoint_roots: Vec<PathBuf>,
    /// Combined size of every worktree checkout, excluding checkpoints.
    pub bytes: u64,
    /// Combined size of every `.lin` checkpoint database for this project.
    pub checkpoints: u64,
}

#[derive(Clone, Debug)]
pub struct CheckpointHistoryEntry {
    pub project: SharedString,
    pub root: PathBuf,
    pub checkpoint: agent::ProjectCheckpoint,
}

/// Creates the path of a worktree's `.lin` directory.
pub fn checkpoint_dir(root: &Path) -> PathBuf {
    root.join(paths::local_settings_folder_name())
}

/// Collects the git worktrees of every open project.
///
/// Worktrees are discovered through `git worktree list` (the `linked_worktrees`
/// snapshot on each repository), so worktrees created by other tools — including
/// the agent harness — show up here too, not just the ones Lin created.
pub fn collect_open_project_specs(cx: &App) -> Vec<ProjectSpec> {
    // `all_projects` requires the app state to be initialized. Settings windows
    // can be constructed in tests without one, so bail out instead of panicking.
    if workspace::AppState::try_global(cx).is_none() {
        return Vec::new();
    }

    let worktree_directory = ProjectSettings::get_global(cx)
        .git
        .worktree_directory
        .clone();
    let mut seen_worktrees = HashSet::new();
    let mut specs = Vec::new();

    for project in crate::all_projects(None, cx) {
        let project = project.read(cx);
        if project.is_via_collab() {
            continue;
        }

        let visible_worktrees: Vec<_> = project.visible_worktrees(cx).collect();
        let name: SharedString = visible_worktrees
            .first()
            .map(|worktree| worktree.read(cx).root_name_str().to_string())
            .unwrap_or_else(|| "Project".to_string())
            .into();
        let root = visible_worktrees
            .first()
            .map(|worktree| worktree.read(cx).abs_path().to_path_buf())
            .unwrap_or_default();
        let remote = project.remote_connection_options(cx).is_some();
        let mut roots: Vec<PathBuf> = visible_worktrees
            .iter()
            .map(|worktree| worktree.read(cx).abs_path().to_path_buf())
            .collect();

        let mut worktrees = Vec::new();
        if !remote {
            for repository in project.repositories(cx).values() {
                let repository = repository.read(cx);
                let managed_base = repository
                    .main_worktree_abs_path()
                    .and_then(|main_worktree| {
                        worktrees_directory_for_repo(
                            main_worktree,
                            &worktree_directory,
                            repository.path_style,
                        )
                        .ok()
                    });

                for worktree in repository
                    .linked_worktrees
                    .iter()
                    .filter(|worktree| !worktree.is_main)
                {
                    if !seen_worktrees.insert(worktree.path.clone()) {
                        continue;
                    }
                    let managed = managed_base
                        .as_ref()
                        .is_some_and(|base| worktree.path.starts_with(base));
                    worktrees.push(WorktreeSpec {
                        path: worktree.path.clone(),
                        name: worktree.display_name().to_string().into(),
                        branch: worktree
                            .branch_name()
                            .map(|branch| branch.to_string().into()),
                        managed,
                    });
                }
            }
        }

        let checkpoint_paths = roots
            .drain(..)
            .filter(|root| !remote && !worktrees.iter().any(|worktree| &worktree.path == root))
            .map(|root| checkpoint_dir(&root))
            .collect();

        specs.push(ProjectSpec {
            name,
            root,
            remote,
            open: true,
            worktrees,
            checkpoint_paths,
        });
    }

    specs
}

fn find_dot_git(path: &Path) -> Option<PathBuf> {
    let mut current = Some(path);
    while let Some(directory) = current {
        let candidate = directory.join(".git");
        if std_fs::symlink_metadata(&candidate).is_ok() {
            return Some(candidate);
        }
        current = directory.parent();
    }
    None
}

/// Adds projects remembered by the recent-projects database, opening their git
/// metadata to discover worktrees even when the project is not currently open.
pub async fn collect_recorded_project_specs(
    fs: std::sync::Arc<dyn FsTrait>,
    db: workspace::WorkspaceDb,
    worktree_directory: String,
    mut seen_roots: HashSet<PathBuf>,
    mut seen_worktrees: HashSet<PathBuf>,
) -> Vec<ProjectSpec> {
    let recent_workspaces = match db.recent_project_workspaces(fs.as_ref()).await {
        Ok(workspaces) => workspaces,
        Err(error) => {
            log::debug!("failed to read recent projects for storage scan: {error:#}");
            return Vec::new();
        }
    };

    let path_style = PathStyle::local();
    let mut specs = Vec::new();

    for recent in recent_workspaces {
        let remote = matches!(
            recent.location,
            workspace::SerializedWorkspaceLocation::Remote(_)
        );
        let roots: Vec<PathBuf> = recent
            .paths
            .ordered_paths()
            .filter(|root| !seen_roots.contains(*root) && (remote || root.is_dir()))
            .cloned()
            .collect();
        if roots.is_empty() {
            continue;
        }

        let name: SharedString = roots
            .first()
            .and_then(|root| root.file_name())
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Project".to_string())
            .into();
        let root = roots[0].clone();
        for project_root in &roots {
            seen_roots.insert(project_root.clone());
        }

        if remote {
            specs.push(ProjectSpec {
                name,
                root,
                remote: true,
                open: false,
                worktrees: Vec::new(),
                checkpoint_paths: Vec::new(),
            });
            continue;
        }

        let mut worktrees = Vec::new();
        let mut checkpoint_paths = Vec::new();

        for project_root in roots {
            let Some(dot_git) = find_dot_git(&project_root) else {
                checkpoint_paths.push(checkpoint_dir(&project_root));
                continue;
            };

            let backend = match fs.open_repo(&dot_git, None) {
                Ok(backend) => backend,
                Err(error) => {
                    log::debug!(
                        "failed to open repository at {} while scanning storage: {error:#}",
                        dot_git.display()
                    );
                    checkpoint_paths.push(checkpoint_dir(&project_root));
                    continue;
                }
            };

            let listed = match backend.worktrees().await {
                Ok(worktrees) => worktrees,
                Err(error) => {
                    log::debug!(
                        "failed to list worktrees for {}: {error:#}",
                        project_root.display()
                    );
                    checkpoint_paths.push(checkpoint_dir(&project_root));
                    continue;
                }
            };

            let main_path = listed
                .iter()
                .find(|worktree| worktree.is_main)
                .map(|worktree| worktree.path.clone());
            let managed_base = main_path.as_deref().and_then(|main_worktree| {
                worktrees_directory_for_repo(main_worktree, &worktree_directory, path_style).ok()
            });

            let mut root_is_worktree = false;
            for worktree in listed.iter().filter(|worktree| !worktree.is_main) {
                if worktree.path == project_root {
                    root_is_worktree = true;
                }
                if !seen_worktrees.insert(worktree.path.clone()) {
                    continue;
                }
                let managed = managed_base
                    .as_ref()
                    .is_some_and(|base| worktree.path.starts_with(base));
                worktrees.push(WorktreeSpec {
                    path: worktree.path.clone(),
                    name: worktree.display_name().to_string().into(),
                    branch: worktree
                        .branch_name()
                        .map(|branch| branch.to_string().into()),
                    managed,
                });
            }

            if !root_is_worktree {
                checkpoint_paths.push(checkpoint_dir(&project_root));
            }
        }

        specs.push(ProjectSpec {
            name,
            root,
            remote: false,
            open: false,
            worktrees,
            checkpoint_paths,
        });
    }

    specs
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CleanupScope {
    /// Categories whose contents are inert and regenerated by the app.
    SafeCaches,
    Category(StorageCategoryKind),
}

#[derive(Clone, Debug)]
pub struct CleanupTarget {
    pub kind: StorageCategoryKind,
    pub path: PathBuf,
    pub prefixes: &'static [&'static str],
    /// Skip entries modified more recently than this. Used for sandbox
    /// directories that may still belong to a running thread.
    pub min_age: Option<Duration>,
    pub files_only: bool,
}

/// Rebuilds cleanup targets from the static specs, never from data supplied by
/// the UI. This keeps a stale or tampered-with row from turning into a delete
/// of an arbitrary path.
pub fn cleanup_targets_for(kinds: &[StorageCategoryKind]) -> Vec<CleanupTarget> {
    let kinds: HashSet<_> = kinds.iter().copied().collect();
    category_specs()
        .into_iter()
        .filter(|spec| kinds.contains(&spec.kind) && spec.kind.policy().is_reclaimable())
        .map(|spec| CleanupTarget {
            min_age: (spec.kind == StorageCategoryKind::TemporaryWorkingFolders)
                .then_some(Duration::from_secs(6 * 60 * 60)),
            kind: spec.kind,
            path: spec.path,
            prefixes: spec.prefixes,
            files_only: false,
        })
        .collect()
}

#[derive(Clone, Debug, Default)]
pub struct CleanupOutcome {
    pub freed_bytes: u64,
    pub errors: Vec<String>,
}

/// Removes the contents of a category directory, but keeps the directory itself
/// (several of them are expected to exist at startup) and the log file Lin is
/// currently writing to.
pub fn remove_category_contents(target: &CleanupTarget) -> CleanupOutcome {
    let keep_active_log = target.kind == StorageCategoryKind::Logs;
    let keep_diagnostic_directories = target.min_age.is_some()
        && matches!(
            target.kind,
            StorageCategoryKind::Logs | StorageCategoryKind::HangTraces
        );
    remove_dir_contents(
        &target.path,
        |path| {
            (keep_active_log && path == paths::log_file().as_path())
                || ((keep_diagnostic_directories || target.files_only) && path.is_dir())
        },
        target.prefixes,
        target.min_age,
    )
}

fn remove_dir_contents(
    path: &Path,
    keep: impl Fn(&Path) -> bool,
    prefixes: &[&str],
    min_age: Option<Duration>,
) -> CleanupOutcome {
    let mut outcome = CleanupOutcome::default();
    match std_fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            outcome.errors.push(format!(
                "Refusing to clean {} because it is a symbolic link",
                path.display()
            ));
            return outcome;
        }
        Ok(_) => {}
        Err(error) if error.kind() == ErrorKind::NotFound => return outcome,
        Err(error) => {
            outcome
                .errors
                .push(format!("Could not inspect {}: {error}", path.display()));
            return outcome;
        }
    }

    let entries = match std_fs::read_dir(path) {
        Ok(entries) => entries,
        Err(error) if error.kind() == ErrorKind::NotFound => return outcome,
        Err(error) => {
            outcome
                .errors
                .push(format!("Could not read {}: {error}", path.display()));
            return outcome;
        }
    };

    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                outcome.errors.push(format!(
                    "Could not read an entry in {}: {error}",
                    path.display()
                ));
                continue;
            }
        };
        let entry_path = entry.path();
        if keep(&entry_path) {
            continue;
        }
        if !prefixes.is_empty() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            if !prefixes.iter().any(|prefix| name.starts_with(prefix)) {
                continue;
            }
        }

        let metadata = match std_fs::symlink_metadata(&entry_path) {
            Ok(metadata) => metadata,
            Err(error) => {
                outcome.errors.push(format!(
                    "Could not inspect {}: {error}",
                    entry_path.display()
                ));
                continue;
            }
        };

        if let Some(min_age) = min_age {
            match metadata.modified().and_then(|modified| {
                SystemTime::now()
                    .duration_since(modified)
                    .map_err(std::io::Error::other)
            }) {
                Ok(age) if age >= min_age => {}
                Ok(_) => continue,
                Err(error) => {
                    outcome.errors.push(format!(
                        "Could not determine the age of {}: {error}",
                        entry_path.display()
                    ));
                    continue;
                }
            }
        }

        if metadata.is_dir() {
            let freed = measure_dir(&entry_path).bytes;
            match std_fs::remove_dir_all(&entry_path) {
                Ok(()) => outcome.freed_bytes = outcome.freed_bytes.saturating_add(freed),
                Err(error) => outcome.errors.push(format!(
                    "Could not remove {}: {error}",
                    entry_path.display()
                )),
            }
        } else {
            match std_fs::remove_file(&entry_path) {
                Ok(()) => outcome.freed_bytes = outcome.freed_bytes.saturating_add(metadata.len()),
                Err(error) => outcome.errors.push(format!(
                    "Could not remove {}: {error}",
                    entry_path.display()
                )),
            }
        }
    }

    outcome
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum StorageView {
    Categories,
    Projects,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum StorageItemId {
    Category(StorageCategoryKind),
    Project(usize),
    Worktree {
        project: usize,
        worktree: usize,
    },
    ProjectWorktrees,
    ProjectCheckpoints,
    /// The aggregated tail of a chart, used when there are more items than
    /// slices.
    Other,
}

#[derive(Clone, Debug, Default)]
pub struct CleanupState {
    pub running: bool,
    pub current: Option<SharedString>,
    pub freed_bytes: u64,
    pub completed: usize,
    pub total: usize,
    pub errors: Vec<String>,
    pub finished_at: Option<SystemTime>,
}

const LAST_CLEANUP_KEY: &str = "storage.last_cleanup";

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct LastCleanup {
    pub freed_bytes: u64,
    pub finished_at: SystemTime,
    pub errors: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct StorageDisk {
    pub mount_point: PathBuf,
    pub available_bytes: u64,
    pub total_bytes: u64,
}

#[derive(Clone, Debug)]
pub enum DataMigrationState {
    Copying,
    Ready(PathBuf),
    Failed(SharedString),
}

fn copy_data_directory(source: &Path, destination: &Path) -> anyhow::Result<()> {
    anyhow::ensure!(
        source.is_dir(),
        "The application data directory is unavailable"
    );
    anyhow::ensure!(
        destination.is_absolute(),
        "The migration destination must be an absolute path"
    );
    if destination.exists() {
        let metadata = std_fs::symlink_metadata(destination)?;
        anyhow::ensure!(
            !metadata.file_type().is_symlink(),
            "Refusing to migrate through a symbolic-link destination"
        );
        anyhow::ensure!(
            destination.is_dir(),
            "The migration destination must be a directory"
        );
        let mut entries = std_fs::read_dir(destination)?;
        anyhow::ensure!(
            entries.next().is_none(),
            "Choose an empty directory for the migrated application data"
        );
    } else {
        std_fs::create_dir_all(destination)?;
    }

    fn copy_entry(source: &Path, destination: &Path) -> anyhow::Result<()> {
        let metadata = std_fs::symlink_metadata(source)?;
        anyhow::ensure!(
            !metadata.file_type().is_symlink(),
            "Refusing to migrate symbolic link {}",
            source.display()
        );
        if metadata.is_dir() {
            std_fs::create_dir_all(destination)?;
            for entry in std_fs::read_dir(source)? {
                let entry = entry?;
                copy_entry(&entry.path(), &destination.join(entry.file_name()))?;
            }
        } else if metadata.is_file() {
            if let Some(parent) = destination.parent() {
                std_fs::create_dir_all(parent)?;
            }
            std_fs::copy(source, destination)?;
        } else {
            anyhow::bail!("Unsupported file type in {}", source.display());
        }
        Ok(())
    }

    for entry in std_fs::read_dir(source)? {
        let entry = entry?;
        copy_entry(&entry.path(), &destination.join(entry.file_name()))?;
    }
    Ok(())
}

fn storage_disks(paths: &[PathBuf]) -> Vec<StorageDisk> {
    let disks = sysinfo::Disks::new_with_refreshed_list();
    let mut result = Vec::new();
    for path in paths {
        let mut existing = path.as_path();
        while !existing.exists() {
            let Some(parent) = existing.parent() else {
                break;
            };
            if parent == existing {
                break;
            }
            existing = parent;
        }
        let Ok(path) = existing.canonicalize() else {
            continue;
        };
        let Some(disk) = disks
            .list()
            .iter()
            .filter(|disk| path.starts_with(disk.mount_point()))
            .max_by_key(|disk| disk.mount_point().components().count())
        else {
            continue;
        };
        if result
            .iter()
            .any(|item: &StorageDisk| item.mount_point == disk.mount_point())
        {
            continue;
        }
        result.push(StorageDisk {
            mount_point: disk.mount_point().to_path_buf(),
            available_bytes: disk.available_space(),
            total_bytes: disk.total_space(),
        });
    }
    result.sort_by(|left, right| left.mount_point.cmp(&right.mount_point));
    result
}

#[derive(Clone, Debug)]
pub struct CleanupConfirmation {
    pub scope: CleanupScope,
    pub kinds: Vec<StorageCategoryKind>,
    pub bytes: u64,
}

#[derive(Clone, Debug)]
pub struct StoragePageSnapshot {
    pub view: StorageView,
    pub categories: Vec<StorageCategory>,
    pub projects: Vec<ProjectStorage>,
    pub categories_scanning: bool,
    pub projects_scanning: bool,
    pub project_scan_progress: (usize, usize),
    pub cleanup: CleanupState,
    pub confirmation: Option<CleanupConfirmation>,
    pub hovered: Option<StorageItemId>,
    pub last_scan_finished: Option<SystemTime>,
    pub disks: Vec<StorageDisk>,
    pub expanded_downloads: HashSet<StorageCategoryKind>,
    pub last_cleanup: Option<LastCleanup>,
    pub change_since_scan: Option<i128>,
    pub history: Vec<CheckpointHistoryEntry>,
    pub history_errors: Vec<String>,
    pub history_scanning: bool,
    pub data_migration: Option<DataMigrationState>,
}

impl StoragePageSnapshot {
    pub fn total_bytes(&self) -> u64 {
        let categories: u64 = self.categories.iter().map(|category| category.bytes).sum();
        categories
            .saturating_add(self.worktree_bytes())
            .saturating_add(self.checkpoint_bytes())
    }

    pub fn worktree_bytes(&self) -> u64 {
        self.projects
            .iter()
            .filter(|project| !project.remote)
            .map(|project| project.bytes)
            .sum()
    }

    pub fn checkpoint_bytes(&self) -> u64 {
        self.projects
            .iter()
            .filter(|project| !project.remote)
            .map(|project| project.checkpoints)
            .sum()
    }

    pub fn safe_to_clean_bytes(&self) -> u64 {
        self.categories
            .iter()
            .filter(|category| category.policy() == CleanupPolicy::Safe)
            .map(|category| category.bytes)
            .sum()
    }

    pub fn redownloadable_bytes(&self) -> u64 {
        self.categories
            .iter()
            .filter(|category| category.policy() == CleanupPolicy::Reinstallable)
            .map(|category| category.bytes)
            .sum()
    }

    pub fn protected_bytes(&self) -> u64 {
        self.categories
            .iter()
            .filter(|category| !category.policy().is_reclaimable())
            .map(|category| category.bytes)
            .sum()
    }
}

/// Shared state for the Storage settings page.
///
/// The page lives for as long as the app does (it is stored in a global) so a
/// scan can finish after the settings window was closed and reopened.
pub struct StoragePage {
    view: StorageView,
    expanded_downloads: HashSet<StorageCategoryKind>,
    last_cleanup: Option<LastCleanup>,
    last_total_bytes: Option<u64>,
    change_since_scan: Option<i128>,
    history: Vec<CheckpointHistoryEntry>,
    history_errors: Vec<String>,
    history_scanning: bool,
    history_scanned: bool,
    data_migration: Option<DataMigrationState>,
    categories: Vec<StorageCategory>,
    projects: Vec<ProjectStorage>,
    categories_scanned: bool,
    categories_scanning: bool,
    projects_scanned: bool,
    projects_scanning: bool,
    project_scan_progress: (usize, usize),
    cleanup: CleanupState,
    confirmation: Option<CleanupConfirmation>,
    hovered: Option<StorageItemId>,
    last_scan_finished: Option<SystemTime>,
    disks: Vec<StorageDisk>,
    scan_generation: u64,
}

fn project_storage_from_spec(spec: &ProjectSpec, cx: &App) -> ProjectStorage {
    ProjectStorage {
        name: spec.name.clone(),
        root: spec.root.clone(),
        remote: spec.remote,
        open: spec.open,
        worktrees: spec
            .worktrees
            .iter()
            .map(|worktree| WorktreeStorage {
                path: worktree.path.clone(),
                name: worktree.name.clone(),
                branch: worktree.branch.clone(),
                managed: worktree.managed,
                open: worktree_is_open(&worktree.path, cx),
                references: cx
                    .try_global::<WorktreeReferenceProvider>()
                    .and_then(|provider| (provider.0)(&worktree.path, cx)),
                bytes: None,
                files: None,
                checkpoints: None,
            })
            .collect(),
        checkpoint_roots: spec
            .checkpoint_paths
            .iter()
            .filter_map(|path| path.parent().map(Path::to_path_buf))
            .chain(spec.worktrees.iter().map(|worktree| worktree.path.clone()))
            .collect(),
        bytes: 0,
        checkpoints: 0,
    }
}

fn worktree_is_open(path: &Path, cx: &App) -> bool {
    workspace::AppState::try_global(cx).is_some()
        && crate::all_projects(None, cx).any(|project| {
            let project = project.read(cx);
            project.remote_connection_options(cx).is_none()
                && project
                    .visible_worktrees(cx)
                    .any(|tree| tree.read(cx).abs_path().as_ref() == path)
        })
}

fn worktree_removal_allowed(path: &Path, cx: &App) -> anyhow::Result<()> {
    anyhow::ensure!(
        !worktree_is_open(path, cx),
        "The worktree is open in a workspace. Close it before removing it."
    );
    let references = cx
        .try_global::<WorktreeReferenceProvider>()
        .and_then(|provider| (provider.0)(path, cx))
        .ok_or_else(|| {
            anyhow::anyhow!("Conversation information is unavailable. Rescan and try again.")
        })?;
    anyhow::ensure!(
        references.conversations.is_empty() && references.terminals.is_empty(),
        "The worktree is still used by a conversation or terminal."
    );
    Ok(())
}

impl StoragePage {
    fn new(cx: &mut Context<Self>) -> Self {
        let last_cleanup = if cx.has_global::<db::AppDatabase>() {
            db::kvp::KeyValueStore::global(cx)
                .read_kvp(LAST_CLEANUP_KEY)
                .log_err()
                .flatten()
                .and_then(|value| serde_json::from_str::<LastCleanup>(&value).log_err())
        } else {
            None
        };
        let cleanup = CleanupState {
            errors: last_cleanup
                .as_ref()
                .map(|result| result.errors.clone())
                .unwrap_or_default(),
            ..Default::default()
        };
        Self {
            view: StorageView::Categories,
            expanded_downloads: HashSet::new(),
            last_cleanup,
            last_total_bytes: None,
            change_since_scan: None,
            history: Vec::new(),
            history_errors: Vec::new(),
            history_scanning: false,
            history_scanned: false,
            data_migration: None,
            categories: Vec::new(),
            projects: Vec::new(),
            categories_scanned: false,
            categories_scanning: false,
            projects_scanned: false,
            projects_scanning: false,
            project_scan_progress: (0, 0),
            cleanup,
            confirmation: None,
            hovered: None,
            last_scan_finished: None,
            disks: Vec::new(),
            scan_generation: 0,
        }
    }

    pub fn snapshot(&self) -> StoragePageSnapshot {
        StoragePageSnapshot {
            view: self.view,
            categories: self.categories.clone(),
            projects: self.projects.clone(),
            categories_scanning: self.categories_scanning,
            projects_scanning: self.projects_scanning,
            project_scan_progress: self.project_scan_progress,
            cleanup: self.cleanup.clone(),
            confirmation: self.confirmation.clone(),
            hovered: self.hovered,
            last_scan_finished: self.last_scan_finished,
            disks: self.disks.clone(),
            expanded_downloads: self.expanded_downloads.clone(),
            last_cleanup: self.last_cleanup.clone(),
            change_since_scan: self.change_since_scan,
            history: self.history.clone(),
            history_errors: self.history_errors.clone(),
            history_scanning: self.history_scanning,
            data_migration: self.data_migration.clone(),
        }
    }

    pub fn ensure_scanned(&mut self, cx: &mut Context<Self>) {
        if self.cleanup.running {
            return;
        }
        if !self.categories_scanned && !self.categories_scanning {
            self.start_category_scan(cx);
        }
        if !self.projects_scanned && !self.projects_scanning {
            self.start_project_scan(cx);
        }
    }

    pub fn ensure_history_scanned(&mut self, cx: &mut Context<Self>) {
        self.ensure_scanned(cx);
        if self.cleanup.running
            || self.projects_scanning
            || !self.projects_scanned
            || self.history_scanning
            || self.history_scanned
        {
            return;
        }
        let roots: Vec<_> = self
            .projects
            .iter()
            .filter(|project| !project.remote)
            .flat_map(|project| {
                project
                    .checkpoint_roots
                    .iter()
                    .map(|root| (project.name.clone(), root.clone()))
            })
            .collect();
        self.history_scanning = true;
        let generation = self.scan_generation;
        cx.spawn(async move |this, cx| {
            let (history, errors) = cx
                .background_spawn(async move {
                    let mut history = Vec::new();
                    let mut errors = Vec::new();
                    let mut seen = HashSet::new();
                    for (project, root) in roots {
                        if !seen.insert(root.clone()) {
                            continue;
                        }
                        let result =
                            agent::ProjectCheckpointStore::open_existing(&root).and_then(|store| {
                                match store {
                                    Some(store) => store.list_history(0, 100),
                                    None => Ok(Vec::new()),
                                }
                            });
                        match result {
                            Ok(checkpoints) => {
                                history.extend(checkpoints.into_iter().map(|checkpoint| {
                                    CheckpointHistoryEntry {
                                        project: project.clone(),
                                        root: root.clone(),
                                        checkpoint,
                                    }
                                }))
                            }
                            Err(error) => errors.push(format!(
                                "Could not read checkpoints for {}: {error:#}",
                                root.display()
                            )),
                        }
                    }
                    history.sort_by(|left, right| {
                        right.checkpoint.created_at.cmp(&left.checkpoint.created_at)
                    });
                    (history, errors)
                })
                .await;
            this.update(cx, |this, cx| {
                if this.scan_generation != generation {
                    return;
                }
                this.history = history;
                this.history_errors = errors;
                this.history_scanning = false;
                this.history_scanned = true;
                cx.notify();
            })
            .log_err();
        })
        .detach();
    }

    fn finish_scan_if_ready(&mut self) {
        if self.categories_scanning
            || self.projects_scanning
            || !self.categories_scanned
            || !self.projects_scanned
        {
            return;
        }
        let total = self.snapshot().total_bytes();
        self.change_since_scan = self
            .last_total_bytes
            .map(|previous| i128::from(total) - i128::from(previous));
        self.last_total_bytes = Some(total);
        self.last_scan_finished = Some(SystemTime::now());
    }

    pub fn set_view(&mut self, view: StorageView, cx: &mut Context<Self>) {
        if self.view == view {
            return;
        }
        self.view = view;
        self.hovered = None;
        self.ensure_scanned(cx);
        cx.notify();
    }

    pub fn toggle_downloads(&mut self, kind: StorageCategoryKind, cx: &mut Context<Self>) {
        if !self.expanded_downloads.insert(kind) {
            self.expanded_downloads.remove(&kind);
        }
        cx.notify();
    }

    fn record_cleanup(&mut self, cx: &mut Context<Self>) {
        let Some(finished_at) = self.cleanup.finished_at else {
            return;
        };
        let result = LastCleanup {
            freed_bytes: self.cleanup.freed_bytes,
            finished_at,
            errors: self.cleanup.errors.clone(),
        };
        self.last_cleanup = Some(result.clone());
        if !cx.has_global::<db::AppDatabase>() {
            return;
        }
        let value = match serde_json::to_string(&result) {
            Ok(value) => value,
            Err(error) => {
                self.cleanup
                    .errors
                    .push(format!("Could not save cleanup history: {error}"));
                return;
            }
        };
        let database = db::kvp::KeyValueStore::global(cx);
        cx.spawn(async move |this, cx| {
            if let Err(error) = database
                .write_kvp(LAST_CLEANUP_KEY.to_string(), value)
                .await
            {
                this.update(cx, |this, cx| {
                    this.cleanup
                        .errors
                        .push(format!("Could not save cleanup history: {error:#}"));
                    cx.notify();
                })
                .log_err();
            }
        })
        .detach();
    }

    pub fn set_hovered(&mut self, hovered: Option<StorageItemId>, cx: &mut Context<Self>) {
        if self.hovered == hovered {
            return;
        }
        self.hovered = hovered;
        cx.notify();
    }

    pub fn rescan(&mut self, cx: &mut Context<Self>) {
        if self.cleanup.running {
            return;
        }
        self.scan_generation = self.scan_generation.wrapping_add(1);
        self.hovered = None;
        self.categories_scanned = false;
        self.categories_scanning = false;
        self.projects_scanned = false;
        self.projects_scanning = false;
        self.projects.clear();
        self.history_scanning = false;
        self.history_scanned = false;
        self.history.clear();
        self.history_errors.clear();
        self.project_scan_progress = (0, 0);
        self.ensure_scanned(cx);
        cx.notify();
    }

    fn start_category_scan(&mut self, cx: &mut Context<Self>) {
        if self.categories_scanning {
            return;
        }
        self.categories_scanning = true;
        let generation = self.scan_generation;
        let specs = category_specs();

        cx.spawn(async move |this, cx| {
            let categories = cx
                .background_spawn(async move { scan_categories(&specs) })
                .await;
            this.update(cx, |this, cx| {
                if this.scan_generation != generation {
                    return;
                }
                this.categories = categories;
                this.categories_scanning = false;
                this.categories_scanned = true;
                this.finish_scan_if_ready();
                cx.notify();
            })
            .log_err();
        })
        .detach();
    }

    fn start_project_scan(&mut self, cx: &mut Context<Self>) {
        if self.projects_scanning {
            return;
        }

        let open_specs = collect_open_project_specs(cx);
        let worktree_directory = ProjectSettings::get_global(cx)
            .git
            .worktree_directory
            .clone();
        let app_state = workspace::AppState::try_global(cx);
        let fs = app_state.as_ref().map(|state| state.fs.clone());
        let db = app_state
            .as_ref()
            .map(|_| workspace::WorkspaceDb::global(cx));

        let mut seen_roots: HashSet<PathBuf> =
            open_specs.iter().map(|spec| spec.root.clone()).collect();
        let mut seen_worktrees = HashSet::new();
        for spec in &open_specs {
            for worktree in &spec.worktrees {
                seen_roots.insert(worktree.path.clone());
                seen_worktrees.insert(worktree.path.clone());
            }
            for checkpoint in &spec.checkpoint_paths {
                if let Some(root) = checkpoint.parent() {
                    seen_roots.insert(root.to_path_buf());
                }
            }
        }

        self.projects = open_specs
            .iter()
            .map(|spec| project_storage_from_spec(spec, cx))
            .collect();
        self.projects_scanned = true;
        self.projects_scanning = true;
        self.project_scan_progress = (0, 0);
        cx.notify();

        let generation = self.scan_generation;
        cx.spawn(async move |this, cx| {
            let recorded = if let (Some(fs), Some(db)) = (fs, db) {
                cx.background_spawn(async move {
                    collect_recorded_project_specs(
                        fs,
                        db,
                        worktree_directory,
                        seen_roots,
                        seen_worktrees,
                    )
                    .await
                })
                .await
            } else {
                Vec::new()
            };

            let mut specs = open_specs;
            specs.extend(recorded);
            let mut disk_paths: Vec<_> =
                category_specs().into_iter().map(|spec| spec.path).collect();
            for spec in &specs {
                if !spec.remote {
                    disk_paths.push(spec.root.clone());
                    disk_paths.extend(spec.worktrees.iter().map(|worktree| worktree.path.clone()));
                }
            }
            let disks = cx
                .background_spawn(async move { storage_disks(&disk_paths) })
                .await;
            let total_units: usize = specs
                .iter()
                .map(|spec| spec.worktrees.len() + spec.checkpoint_paths.len())
                .sum();

            let current = this
                .update(cx, |this, cx| {
                    if this.scan_generation != generation {
                        return false;
                    }
                    this.projects = specs
                        .iter()
                        .map(|spec| project_storage_from_spec(spec, cx))
                        .collect();
                    this.disks = disks;
                    this.project_scan_progress = (0, total_units);
                    this.projects_scanning = total_units > 0;
                    cx.notify();
                    true
                })
                .unwrap_or(false);
            if !current {
                return;
            }
            if total_units == 0 {
                this.update(cx, |this, cx| {
                    if this.scan_generation != generation {
                        return;
                    }
                    this.projects_scanning = false;
                    this.finish_scan_if_ready();
                    cx.notify();
                })
                .log_err();
                return;
            }

            let mut completed = 0usize;
            for (project_index, spec) in specs.iter().enumerate() {
                for checkpoint in &spec.checkpoint_paths {
                    let path = checkpoint.clone();
                    let usage = cx.background_spawn(async move { measure_dir(&path) }).await;
                    completed += 1;

                    let current = this
                        .update(cx, |this, cx| {
                            if this.scan_generation != generation {
                                return false;
                            }
                            if let Some(project) = this.projects.get_mut(project_index) {
                                project.checkpoints =
                                    project.checkpoints.saturating_add(usage.bytes);
                            }
                            this.project_scan_progress = (completed, total_units);
                            cx.notify();
                            true
                        })
                        .unwrap_or(false);
                    if !current {
                        return;
                    }
                }

                for (worktree_index, worktree) in spec.worktrees.iter().enumerate() {
                    let path = worktree.path.clone();
                    let checkpoint_path = checkpoint_dir(&path);
                    let (usage, checkpoints) = cx
                        .background_spawn(async move {
                            (measure_dir(&path), measure_dir(&checkpoint_path))
                        })
                        .await;
                    completed += 1;

                    let current = this
                        .update(cx, |this, cx| {
                            if this.scan_generation != generation {
                                return false;
                            }
                            if let Some(project) = this.projects.get_mut(project_index) {
                                if let Some(worktree) = project.worktrees.get_mut(worktree_index) {
                                    worktree.bytes =
                                        Some(usage.bytes.saturating_sub(checkpoints.bytes));
                                    worktree.files =
                                        Some(usage.files.saturating_sub(checkpoints.files));
                                    worktree.checkpoints = Some(checkpoints.bytes);
                                }
                                project.bytes = project
                                    .worktrees
                                    .iter()
                                    .filter_map(|worktree| worktree.bytes)
                                    .sum();
                                project.checkpoints =
                                    project.checkpoints.saturating_add(checkpoints.bytes);
                            }
                            this.project_scan_progress = (completed, total_units);
                            cx.notify();
                            true
                        })
                        .unwrap_or(false);
                    if !current {
                        return;
                    }
                }
            }

            this.update(cx, |this, cx| {
                if this.scan_generation != generation {
                    return;
                }
                this.projects_scanning = false;
                this.finish_scan_if_ready();
                cx.notify();
            })
            .log_err();
        })
        .detach();
    }

    pub fn request_cleanup(&mut self, scope: CleanupScope, cx: &mut Context<Self>) {
        if self.cleanup.running
            || matches!(
                self.data_migration.as_ref(),
                Some(DataMigrationState::Copying)
            )
        {
            return;
        }

        let kinds: Vec<StorageCategoryKind> = match scope {
            CleanupScope::SafeCaches => self
                .categories
                .iter()
                .filter(|category| category.policy() == CleanupPolicy::Safe)
                .map(|category| category.kind)
                .collect(),
            CleanupScope::Category(kind) => vec![kind],
        };
        if kinds.is_empty() {
            return;
        }

        let bytes = self
            .categories
            .iter()
            .filter(|category| kinds.contains(&category.kind))
            .map(|category| category.bytes)
            .sum();

        self.confirmation = Some(CleanupConfirmation {
            scope,
            kinds,
            bytes,
        });
        cx.notify();
    }

    pub fn cancel_cleanup(&mut self, cx: &mut Context<Self>) {
        if self.cleanup.running {
            return;
        }
        self.confirmation = None;
        cx.notify();
    }

    pub fn dismiss_cleanup_errors(&mut self, cx: &mut Context<Self>) {
        self.cleanup.errors.clear();
        cx.notify();
    }

    pub fn report_history_error(&mut self, error: String, cx: &mut Context<Self>) {
        self.history_errors.push(error);
        cx.notify();
    }

    pub fn set_data_migration_error(&mut self, error: String, cx: &mut Context<Self>) {
        self.data_migration = Some(DataMigrationState::Failed(error.into()));
        cx.notify();
    }

    pub fn start_data_migration(&mut self, destination: PathBuf, cx: &mut Context<Self>) {
        if self.cleanup.running
            || matches!(
                self.data_migration.as_ref(),
                Some(DataMigrationState::Copying)
            )
        {
            return;
        }
        let source = paths::data_dir().clone();
        let destination = destination;
        if source == destination
            || source.starts_with(&destination)
            || destination.starts_with(&source)
        {
            self.data_migration = Some(DataMigrationState::Failed(
                "Choose a directory outside the current application data directory.".into(),
            ));
            cx.notify();
            return;
        }
        self.data_migration = Some(DataMigrationState::Copying);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let copy_destination = destination.clone();
            let result = cx
                .background_spawn(async move { copy_data_directory(&source, &copy_destination) })
                .await;
            this.update(cx, |this, cx| {
                this.data_migration = Some(match result {
                    Ok(()) => DataMigrationState::Ready(destination),
                    Err(error) => DataMigrationState::Failed(format!("{error:#}").into()),
                });
                cx.notify();
            })
            .log_err();
        })
        .detach();
    }

    pub fn restart_after_data_migration(&mut self, cx: &mut Context<Self>) {
        let Some(DataMigrationState::Ready(destination)) = self.data_migration.take() else {
            return;
        };
        gpui::App::set_restart_arguments(
            &mut **cx,
            vec![
                std::ffi::OsString::from("--user-data-dir"),
                destination.into_os_string(),
            ],
        );
        cx.restart();
    }

    pub fn remove_download(
        &mut self,
        kind: StorageCategoryKind,
        name: std::ffi::OsString,
        cx: &mut Context<Self>,
    ) {
        if self.cleanup.running {
            return;
        }
        let Some(spec) = category_specs().into_iter().find(|spec| {
            spec.kind == kind
                && kind.policy() == CleanupPolicy::Reinstallable
                && spec.prefixes.is_empty()
        }) else {
            self.cleanup.errors = vec!["This download cannot be removed from Storage.".into()];
            cx.notify();
            return;
        };
        let name_path = Path::new(&name);
        if !matches!(
            name_path.components().next(),
            Some(std::path::Component::Normal(_))
        ) || name_path.components().count() != 1
            || !self.categories.iter().any(|category| {
                category.kind == kind
                    && category
                        .downloads
                        .iter()
                        .any(|download| download.name == name)
            })
        {
            self.cleanup.errors =
                vec!["The selected download is no longer available. Rescan and try again.".into()];
            cx.notify();
            return;
        }
        self.cleanup = CleanupState {
            running: true,
            current: Some(format!("Removing {}", name.to_string_lossy()).into()),
            total: 1,
            ..Default::default()
        };
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    let root_metadata = std_fs::symlink_metadata(&spec.path)?;
                    anyhow::ensure!(
                        root_metadata.is_dir() && !root_metadata.file_type().is_symlink(),
                        "The download directory is not a regular directory"
                    );
                    let path = spec.path.join(&name);
                    let metadata = std_fs::symlink_metadata(&path)?;
                    anyhow::ensure!(
                        !metadata.file_type().is_symlink(),
                        "The download is a symbolic link"
                    );
                    anyhow::ensure!(
                        metadata.is_file() || metadata.is_dir(),
                        "The download is not a regular file or directory"
                    );
                    let bytes = if metadata.is_dir() {
                        measure_dir(&path).bytes
                    } else {
                        metadata.len()
                    };
                    if metadata.is_dir() {
                        std_fs::remove_dir_all(&path)?;
                    } else {
                        std_fs::remove_file(&path)?;
                    }
                    Ok::<_, anyhow::Error>(bytes)
                })
                .await;
            this.update(cx, |this, cx| {
                this.cleanup.running = false;
                this.cleanup.current = None;
                this.cleanup.completed = 1;
                match result {
                    Ok(bytes) => {
                        this.cleanup.freed_bytes = bytes;
                        this.cleanup.finished_at = Some(SystemTime::now());
                        this.record_cleanup(cx);
                    }
                    Err(error) => {
                        this.cleanup.errors =
                            vec![format!("Could not remove the download: {error:#}")]
                    }
                }
                this.rescan(cx);
            })
            .log_err();
        })
        .detach();
    }

    pub fn remove_worktree(
        &mut self,
        project_root: PathBuf,
        path: PathBuf,
        cx: &mut Context<Self>,
    ) {
        if self.cleanup.running {
            return;
        }
        if let Err(error) = worktree_removal_allowed(&path, cx) {
            self.cleanup.errors = vec![error.to_string()];
            cx.notify();
            return;
        }
        let Some(app_state) = workspace::AppState::try_global(cx) else {
            self.cleanup.errors = vec!["The workspace is unavailable.".to_string()];
            cx.notify();
            return;
        };
        let fs = app_state.fs.clone();
        self.cleanup = CleanupState {
            running: true,
            current: Some("Removing worktree".into()),
            total: 1,
            ..Default::default()
        };
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result: anyhow::Result<()> = async {
                let worktree_path = path.clone();
                let repository = cx.background_spawn(async move {
                    let dot_git = find_dot_git(&project_root).ok_or_else(|| anyhow::anyhow!("The project's Git repository could not be found."))?;
                    let repository = fs.open_repo(&dot_git, None)?;
                    let output = std_fs::canonicalize(&worktree_path).and_then(|directory| {
                        std::process::Command::new("git")
                            .arg("-C").arg(directory)
                            .args(["for-each-ref", "--format=%(refname)", "--contains=HEAD", "refs/heads", "refs/remotes", "refs/tags"])
                            .output()
                    })?;
                    anyhow::ensure!(output.status.success(), "Could not check whether the worktree's commits are retained: {}", String::from_utf8_lossy(&output.stderr));
                    anyhow::ensure!(!output.stdout.is_empty(), "No branch or tag retains this worktree's current commit. Create a branch before removing it.");
                    Ok::<_, anyhow::Error>(repository)
                }).await?;
                let worktrees = repository.worktrees().await?;
                anyhow::ensure!(worktrees.iter().any(|worktree| worktree.path == path && !worktree.is_main), "The selected worktree no longer belongs to this repository.");
                cx.update(|cx| worktree_removal_allowed(&path, cx))?;
                repository.remove_worktree(path.clone(), false).await?;
                Ok(())
            }.await;
            this.update(cx, |this, cx| {
                this.cleanup.running = false;
                this.cleanup.current = None;
                this.cleanup.completed = 1;
                if let Err(error) = result {
                    this.cleanup.errors = vec![format!("Could not remove {}: {error:#}", path.display())];
                }
                this.rescan(cx);
            }).log_err();
        }).detach();
    }

    pub fn confirm_cleanup(&mut self, cx: &mut Context<Self>) {
        if self.cleanup.running {
            return;
        }
        let Some(confirmation) = self.confirmation.take() else {
            return;
        };

        let targets = cleanup_targets_for(&confirmation.kinds);
        self.start_cleanup(targets, cx);
    }

    fn start_automatic_cleanup(&mut self, cx: &mut Context<Self>) {
        if self.cleanup.running {
            return;
        }
        let settings = StorageSettings::get_global(cx);
        if !settings.automatic_cleanup {
            return;
        }
        let minimum_age = Duration::from_secs(
            (settings.diagnostic_retention_days as u64).saturating_mul(24 * 60 * 60),
        );
        let mut targets =
            cleanup_targets_for(&[StorageCategoryKind::Logs, StorageCategoryKind::HangTraces]);
        for target in &mut targets {
            target.min_age = Some(minimum_age);
        }
        let temporary_age = Duration::from_secs(
            (settings.temporary_retention_days as u64).saturating_mul(24 * 60 * 60),
        );
        let mut temporary_targets = cleanup_targets_for(&[StorageCategoryKind::TemporaryFiles]);
        for target in &mut temporary_targets {
            target.min_age = Some(temporary_age);
            target.files_only = true;
        }
        targets.extend(temporary_targets);
        self.start_cleanup(targets, cx);
    }

    fn start_cleanup(&mut self, targets: Vec<CleanupTarget>, cx: &mut Context<Self>) {
        if targets.is_empty() {
            return;
        }

        self.scan_generation = self.scan_generation.wrapping_add(1);
        self.categories_scanned = false;
        self.categories_scanning = false;
        self.projects_scanned = false;
        self.projects_scanning = false;
        self.history_scanned = false;
        self.history_scanning = false;
        self.cleanup = CleanupState {
            running: true,
            current: None,
            freed_bytes: 0,
            completed: 0,
            total: targets.len(),
            errors: Vec::new(),
            finished_at: None,
        };
        cx.notify();

        let generation = self.scan_generation;
        cx.spawn(async move |this, cx| {
            let mut errors = Vec::new();
            let total = targets.len();

            for (index, target) in targets.into_iter().enumerate() {
                let name: SharedString = target.kind.label().into();
                this.update(cx, |this, cx| {
                    this.cleanup.current = Some(name);
                    cx.notify();
                })
                .log_err();

                let outcome = cx
                    .background_spawn(async move { remove_category_contents(&target) })
                    .await;
                errors.extend(outcome.errors);

                this.update(cx, |this, cx| {
                    this.cleanup.completed = index + 1;
                    this.cleanup.freed_bytes =
                        this.cleanup.freed_bytes.saturating_add(outcome.freed_bytes);
                    cx.notify();
                })
                .log_err();
            }

            this.update(cx, |this, cx| {
                this.cleanup.running = false;
                this.cleanup.current = None;
                this.cleanup.errors = errors;
                this.cleanup.finished_at = Some(SystemTime::now());
                this.cleanup.total = total;
                this.record_cleanup(cx);
                if this.scan_generation == generation {
                    this.ensure_scanned(cx);
                }
                cx.notify();
            })
            .log_err();
        })
        .detach();
    }
}

struct GlobalStoragePage(Entity<StoragePage>);

impl Global for GlobalStoragePage {}

pub fn init(cx: &mut App) {
    let page = cx.new(StoragePage::new);
    page.update(cx, |_, cx| {
        cx.spawn(async move |page, cx| {
            cx.background_executor()
                .timer(Duration::from_secs(30))
                .await;
            page.update(cx, |page, cx| page.start_automatic_cleanup(cx))
                .log_err();
        })
        .detach();
    });
    cx.set_global(GlobalStoragePage(page));
}

/// Returns the shared page, creating it if `init` has not run yet. Settings
/// windows are also constructed in tests that never call `init`.
pub fn global_or_init(cx: &mut App) -> Entity<StoragePage> {
    if let Some(global) = cx.try_global::<GlobalStoragePage>() {
        return global.0.clone();
    }
    let page = cx.new(StoragePage::new);
    cx.set_global(GlobalStoragePage(page.clone()));
    page
}

#[cfg(test)]
mod tests {
    use std::fs as std_fs;

    use tempfile::tempdir;

    use super::*;

    #[test]
    fn automatic_cleanup_defaults_off_and_clamps_retention() {
        let mut content = settings::SettingsContent::default();
        let settings = StorageSettings::from_settings(&content);
        assert!(!settings.automatic_cleanup);
        assert_eq!(settings.diagnostic_retention_days, 30);

        content.storage = Some(settings::StorageSettingsContent {
            automatic_cleanup: Some(true),
            diagnostic_retention_days: Some(0),
            temporary_retention_days: None,
            default_project_directory: None,
        });
        let settings = StorageSettings::from_settings(&content);
        assert!(settings.automatic_cleanup);
        assert_eq!(settings.diagnostic_retention_days, 1);
    }

    #[test]
    fn measures_nested_files() {
        let dir = tempdir().unwrap();
        std_fs::create_dir(dir.path().join("nested")).unwrap();
        std_fs::write(dir.path().join("a.txt"), b"1234").unwrap();
        std_fs::write(dir.path().join("nested/b.txt"), b"123456").unwrap();

        let usage = measure_dir(dir.path());

        assert_eq!(usage.bytes, 10);
        assert_eq!(usage.files, 2);
        assert_eq!(usage.skipped_entries, 0);
    }

    #[test]
    fn finds_git_metadata_from_nested_project_directories() {
        let root = tempdir().unwrap();
        let nested = root.path().join("crates/editor");
        std_fs::create_dir_all(&nested).unwrap();
        std_fs::create_dir(root.path().join(".git")).unwrap();

        let dot_git = find_dot_git(&nested).expect("finds the repository root");
        assert_eq!(dot_git, root.path().join(".git"));
    }

    #[test]
    fn measuring_a_missing_directory_is_empty() {
        let usage = measure_dir(Path::new("/definitely/not/a/directory"));
        assert_eq!(usage, DirUsage::default());
    }

    #[test]
    fn cleanup_removes_children_but_keeps_the_root() {
        let dir = tempdir().unwrap();
        std_fs::create_dir(dir.path().join("nested")).unwrap();
        std_fs::write(dir.path().join("a.txt"), b"1234").unwrap();
        std_fs::write(dir.path().join("nested/b.txt"), b"123456").unwrap();

        let outcome = remove_dir_contents(dir.path(), |_| false, &[], None);

        assert!(outcome.errors.is_empty());
        assert_eq!(outcome.freed_bytes, 10);
        assert!(dir.path().exists());
        assert_eq!(std_fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn cleanup_can_keep_a_specific_file() {
        let dir = tempdir().unwrap();
        let kept = dir.path().join("active.log");
        std_fs::write(&kept, b"keep me").unwrap();
        std_fs::write(dir.path().join("old.log"), b"remove me").unwrap();

        let outcome = remove_dir_contents(dir.path(), |path| path == kept, &[], None);

        assert!(outcome.errors.is_empty());
        assert_eq!(outcome.freed_bytes, 9);
        assert!(kept.exists());
        assert!(!dir.path().join("old.log").exists());
    }

    #[test]
    fn age_limited_cleanup_keeps_entries_with_future_timestamps() -> anyhow::Result<()> {
        let directory = tempdir()?;
        let path = directory.path().join("active");
        std_fs::write(&path, b"active")?;
        let file = std_fs::OpenOptions::new().write(true).open(&path)?;
        file.set_times(
            std_fs::FileTimes::new().set_modified(SystemTime::now() + Duration::from_secs(3600)),
        )?;

        let outcome = remove_dir_contents(
            directory.path(),
            |_| false,
            &[],
            Some(Duration::from_secs(60)),
        );

        assert!(path.exists());
        assert_eq!(outcome.freed_bytes, 0);
        assert_eq!(outcome.errors.len(), 1);
        Ok(())
    }

    #[test]
    fn automatic_diagnostic_cleanup_preserves_directories() -> anyhow::Result<()> {
        let directory = tempdir()?;
        let nested = directory.path().join("nested");
        std_fs::create_dir(&nested)?;
        std_fs::write(nested.join("active.log"), b"keep")?;
        std_fs::write(directory.path().join("old.log"), b"remove")?;
        let target = CleanupTarget {
            kind: StorageCategoryKind::Logs,
            path: directory.path().to_path_buf(),
            prefixes: &[],
            min_age: Some(Duration::ZERO),
            files_only: false,
        };

        let outcome = remove_category_contents(&target);

        assert!(outcome.errors.is_empty());
        assert_eq!(outcome.freed_bytes, 6);
        assert!(nested.join("active.log").exists());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn cleanup_refuses_to_follow_a_symlinked_directory() {
        use std::os::unix::fs::symlink;

        let root = tempdir().unwrap();
        let target = root.path().join("target");
        std_fs::create_dir(&target).unwrap();
        std_fs::write(target.join("file.txt"), b"keep").unwrap();
        let link = root.path().join("link");
        symlink(&target, &link).unwrap();

        let outcome = remove_dir_contents(&link, |_| false, &[], None);

        assert!(!outcome.errors.is_empty());
        assert_eq!(outcome.freed_bytes, 0);
        assert!(target.join("file.txt").exists());
    }

    #[test]
    fn prefixed_categories_only_measure_and_remove_matching_entries() {
        let dir = tempdir().unwrap();
        std_fs::create_dir(dir.path().join("zed-agent-terminal-old")).unwrap();
        std_fs::write(
            dir.path().join("zed-agent-terminal-old/file.txt"),
            b"123456",
        )
        .unwrap();
        std_fs::create_dir(dir.path().join("unrelated")).unwrap();
        std_fs::write(dir.path().join("unrelated/file.txt"), b"do not touch").unwrap();

        let spec = CategorySpec::with_prefixes(
            StorageCategoryKind::TemporaryWorkingFolders,
            dir.path(),
            &["zed-agent-terminal-"],
        );
        assert_eq!(measure_category(&spec).bytes, 6);

        let outcome = remove_dir_contents(dir.path(), |_| false, spec.prefixes, None);
        assert!(outcome.errors.is_empty());
        assert_eq!(outcome.freed_bytes, 6);
        assert!(!dir.path().join("zed-agent-terminal-old").exists());
        assert!(dir.path().join("unrelated/file.txt").exists());
    }

    #[test]
    fn category_specs_have_unique_kinds_and_paths() {
        let specs = category_specs();
        let kinds: HashSet<_> = specs.iter().map(|spec| spec.kind).collect();
        let paths: HashSet<_> = specs.iter().map(|spec| spec.path.clone()).collect();
        assert_eq!(kinds.len(), specs.len());
        assert_eq!(paths.len(), specs.len());
    }

    #[test]
    fn protected_categories_are_never_reclaimed() {
        let protected = [
            StorageCategoryKind::ConversationHistory,
            StorageCategoryKind::Databases,
            StorageCategoryKind::Extensions,
        ];
        assert!(cleanup_targets_for(&protected).is_empty());
    }

    #[test]
    fn snapshot_totals_sum_categories_and_local_worktrees() {
        let snapshot = StoragePageSnapshot {
            view: StorageView::Categories,
            categories: vec![
                test_category(StorageCategoryKind::Logs, 10),
                test_category(StorageCategoryKind::LanguageServers, 90),
                test_category(StorageCategoryKind::Databases, 500),
            ],
            projects: vec![
                ProjectStorage {
                    name: "local".into(),
                    root: PathBuf::from("/local"),
                    remote: false,
                    open: true,
                    worktrees: Vec::new(),
                    checkpoint_roots: Vec::new(),
                    bytes: 40,
                    checkpoints: 2,
                },
                ProjectStorage {
                    name: "remote".into(),
                    root: PathBuf::from("/remote"),
                    remote: true,
                    open: false,
                    worktrees: Vec::new(),
                    checkpoint_roots: Vec::new(),
                    bytes: 1_000,
                    checkpoints: 0,
                },
            ],
            categories_scanning: false,
            projects_scanning: false,
            project_scan_progress: (0, 0),
            cleanup: CleanupState::default(),
            confirmation: None,
            hovered: None,
            last_scan_finished: None,
            disks: Vec::new(),
            expanded_downloads: HashSet::new(),
            last_cleanup: None,
            change_since_scan: None,
            history: Vec::new(),
            history_errors: Vec::new(),
            history_scanning: false,
            data_migration: None,
        };

        assert_eq!(snapshot.total_bytes(), 642);
        assert_eq!(snapshot.safe_to_clean_bytes(), 10);
        assert_eq!(snapshot.redownloadable_bytes(), 90);
        assert_eq!(snapshot.protected_bytes(), 500);
    }

    fn test_category(kind: StorageCategoryKind, bytes: u64) -> StorageCategory {
        StorageCategory {
            kind,
            path: PathBuf::from("/tmp"),
            bytes,
            files: 1,
            skipped_entries: 0,
            exists: true,
            downloads: Vec::new(),
        }
    }
}
