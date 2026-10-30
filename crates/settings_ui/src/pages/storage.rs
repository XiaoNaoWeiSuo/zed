use std::{
    cell::Cell,
    path::Path,
    rc::Rc,
    time::{Duration, SystemTime},
};

use gpui::{
    AnyElement, App, Bounds, Entity, Hsla, PathBuilder, PathPromptOptions, PromptLevel,
    ScrollHandle, SharedString, Window, canvas, point, prelude::*, px, relative,
};
use human_bytes::human_bytes;
use ui::{
    Banner, Button, ButtonStyle, Color, Divider, Headline, HeadlineSize, Icon, IconButton,
    IconName, IconSize, Label, LabelSize, ProgressBar, Severity, TintColor, ToggleButtonGroup,
    ToggleButtonGroupStyle, ToggleButtonSimple, Tooltip, prelude::*,
};

use crate::{
    SettingsWindow,
    components::SettingsSectionHeader,
    storage::{
        CleanupConfirmation, CleanupPolicy, CleanupScope, DataMigrationState, ProjectStorage,
        StorageCategory, StorageCategoryKind, StorageItemId, StoragePage, StoragePageSnapshot,
        StorageView, WorktreeStorage,
    },
};

const TOP_CHART_ITEMS: usize = 10;
const PIE_RADIUS: f32 = 74.0;

pub(crate) fn render_checkpoint_history_page(
    _settings_window: &SettingsWindow,
    scroll_handle: &ScrollHandle,
    _window: &mut Window,
    cx: &mut Context<SettingsWindow>,
) -> AnyElement {
    let page = crate::storage::global_or_init(cx);
    page.update(cx, |page, cx| page.ensure_history_scanned(cx));
    let snapshot = page.read(cx).snapshot();
    let loading = snapshot.projects_scanning || snapshot.history_scanning;
    let colors = cx.theme().colors();
    v_flex()
        .id("storage-checkpoint-history")
        .size_full().pt_2p5().px_8().pb_16().gap_4()
        .overflow_y_scroll().track_scroll(scroll_handle)
        .child(h_flex().justify_between().gap_3()
            .child(SettingsSectionHeader::new("History & Recovery").no_padding(true))
            .child(Button::new("checkpoint-history-refresh", "Rescan").label_size(LabelSize::Small)
                .disabled(loading || snapshot.cleanup.running)
                .on_click({ let page = page.clone(); move |_, _, cx| page.update(cx, |page, cx| page.rescan(cx)) })))
        .child(Label::new("Recent checkpoints from open and recently opened local projects. Up to 100 records are shown per checkout.")
            .size(LabelSize::Small).color(Color::Muted))
        .when(loading, |this| this.child(Label::new("Loading checkpoints…").size(LabelSize::Small).color(Color::Muted)))
        .when(!loading && snapshot.history.is_empty(), |this| this.child(Label::new("No checkpoints found.").size(LabelSize::Small).color(Color::Muted)))
        .children(snapshot.history_errors.iter().map(|error| Label::new(error.clone()).size(LabelSize::Small).color(Color::Error)))
        .children(snapshot.history.iter().enumerate().map(|(index, entry)| {
            let checkpoint = entry.checkpoint.clone();
            let export_checkpoint = checkpoint.clone();
            let page = page.clone();
            let root = entry.root.clone();
            v_flex().gap_2().p_3().rounded_lg().border_1().border_color(colors.border_variant).bg(colors.elevated_surface_background)
                .child(h_flex().w_full().justify_between().gap_3()
                    .child(v_flex().min_w_0().gap_0p5()
                        .child(Label::new(format!("{} · {}", entry.project, checkpoint.scope)).size(LabelSize::Small).truncate())
                        .child(Label::new(format!("{} · {}", checkpoint.created_at, display_path(&root))).size(LabelSize::XSmall).color(Color::Muted).truncate()))
                    .child(Button::new(format!("checkpoint-export-{index}"), "Export JSON")
                        .style(ButtonStyle::OutlinedGhost).label_size(LabelSize::XSmall)
                        .on_click(move |_, window, cx| {
                            let data = match serde_json::to_vec_pretty(&export_checkpoint) {
                                Ok(data) => data,
                                Err(error) => { page.update(cx, |page, cx| page.report_history_error(format!("Could not prepare checkpoint export: {error}"), cx)); return; }
                            };
                            let destination = cx.prompt_for_new_path(paths::home_dir(), Some("checkpoint.json"));
                            let page = page.clone();
                            window.spawn(cx, async move |cx| {
                                let path = match destination.await {
                                    Ok(Ok(Some(path))) => path,
                                    Ok(Ok(None)) => return,
                                    Ok(Err(error)) => { page.update(cx, |page, cx| page.report_history_error(format!("Could not choose an export path: {error:#}"), cx)); return; }
                                    Err(error) => { page.update(cx, |page, cx| page.report_history_error(format!("Export dialog failed: {error}"), cx)); return; }
                                };
                                let result = cx.background_spawn(async move {
                                    use std::io::Write as _;
                                    let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(path)?;
                                    file.write_all(&data)
                                }).await;
                                if let Err(error) = result {
                                    page.update(cx, |page, cx| page.report_history_error(format!("Could not export checkpoint: {error}. Choose a new filename."), cx));
                                }
                            }).detach();
                        })))
                .child(Label::new(format!("Session: {} · {} remaining", checkpoint.session_id, checkpoint.remaining.len())).size(LabelSize::XSmall).color(Color::Muted))
                .when_some(checkpoint.next_steps.first().cloned(), |this, next| this.child(Label::new(format!("Next: {next}")).size(LabelSize::Small)))
                .into_any_element()
        }))
        .into_any_element()
}

pub(crate) fn render_storage_page(
    _settings_window: &SettingsWindow,
    scroll_handle: &ScrollHandle,
    _window: &mut Window,
    cx: &mut Context<SettingsWindow>,
) -> AnyElement {
    let page = crate::storage::global_or_init(cx);
    page.update(cx, |page, cx| page.ensure_scanned(cx));

    let snapshot = page.read(cx).snapshot();
    let total = snapshot.total_bytes();

    v_flex()
        .id("storage-settings-page")
        .size_full()
        .pt_2p5()
        .px_8()
        .pb_16()
        .gap_6()
        .overflow_y_scroll()
        .track_scroll(scroll_handle)
        .child(render_summary(&snapshot, total, &page, cx))
        .when_some(snapshot.confirmation.clone(), |this, confirmation| {
            this.child(render_confirmation(&confirmation, &page))
        })
        .when(snapshot.cleanup.running, |this| {
            this.child(render_cleanup_progress(&snapshot, cx))
        })
        .when(!snapshot.cleanup.errors.is_empty(), |this| {
            this.child(render_cleanup_errors(&snapshot, &page))
        })
        .child(render_charts(&snapshot, &page, cx))
        .child(match snapshot.view {
            StorageView::Categories => render_categories(&snapshot, &page, cx),
            StorageView::Projects => render_projects(&snapshot, &page, cx),
        })
        .into_any_element()
}

fn render_summary(
    snapshot: &StoragePageSnapshot,
    total: u64,
    page: &Entity<StoragePage>,
    cx: &mut App,
) -> AnyElement {
    let colors = cx.theme().colors();
    let scanning = snapshot.categories_scanning || snapshot.projects_scanning;

    v_flex()
        .p_4()
        .gap_4()
        .rounded_lg()
        .border_1()
        .border_color(colors.border_variant)
        .bg(colors.elevated_surface_background)
        .child(
            h_flex()
                .w_full()
                .justify_between()
                .items_start()
                .gap_4()
                .child(
                    v_flex()
                        .gap_1()
                        .child(
                            Label::new("Tracked storage")
                                .size(LabelSize::Small)
                                .color(Color::Muted),
                        )
                        .child(Headline::new(human_bytes(total as f64)).size(HeadlineSize::XLarge))
                        .child(
                            Label::new(storage_summary_line(snapshot, total))
                                .size(LabelSize::Small)
                                .color(Color::Muted),
                        ),
                )
                .child(
                    h_flex()
                        .gap_2()
                        .flex_shrink_0()
                        .child(
                            Button::new("storage-rescan", "Rescan")
                                .style(ButtonStyle::OutlinedGhost)
                                .label_size(LabelSize::Small)
                                .start_icon(Icon::new(IconName::RotateCw).size(IconSize::XSmall))
                                .disabled(snapshot.cleanup.running)
                                .tooltip(Tooltip::text(
                                    "Measure caches and project worktrees again.",
                                ))
                                .on_click({
                                    let page = page.clone();
                                    move |_, _, cx| {
                                        page.update(cx, |page, cx| page.rescan(cx));
                                    }
                                }),
                        )
                        .child(
                            Button::new("storage-clean-safe", "Clean Up")
                                .style(ButtonStyle::Tinted(TintColor::Warning))
                                .label_size(LabelSize::Small)
                                .start_icon(Icon::new(IconName::Trash).size(IconSize::XSmall))
                                .disabled(
                                    snapshot.cleanup.running
                                        || matches!(
                                            snapshot.data_migration.as_ref(),
                                            Some(DataMigrationState::Copying)
                                        )
                                        || snapshot.safe_to_clean_bytes() == 0,
                                )
                                .tooltip(Tooltip::text(
                                    "Removes logs, hang traces and temporary files. Projects, \
                                     conversations, databases, extensions and downloaded tools \
                                     are not touched.",
                                ))
                                .on_click({
                                    let page = page.clone();
                                    move |_, _, cx| {
                                        page.update(cx, |page, cx| {
                                            page.request_cleanup(CleanupScope::SafeCaches, cx)
                                        });
                                    }
                                }),
                        ),
                ),
        )
        .child(Divider::horizontal())
        .child(
            h_flex()
                .w_full()
                .flex_wrap()
                .gap_4()
                .child(storage_location("App data", paths::data_dir()))
                .child(storage_location("Settings", paths::config_dir()))
                .child(
                    Button::new("storage-migrate-data", "Copy App Data")
                        .style(ButtonStyle::OutlinedGhost)
                        .label_size(LabelSize::XSmall)
                        .disabled(matches!(
                            snapshot.data_migration.as_ref(),
                            Some(DataMigrationState::Copying)
                        ))
                        .tooltip(Tooltip::text(
                            "Copy application data to an empty directory, then restart using the copy.",
                        ))
                        .on_click({
                            let page = page.clone();
                            move |_, window, cx| {
                                let destination = cx.prompt_for_paths(PathPromptOptions {
                                    files: false,
                                    directories: true,
                                    multiple: false,
                                    prompt: Some("Choose an empty directory for Lin data".into()),
                                });
                                let page = page.clone();
                                window
                                    .spawn(cx, async move |cx| {
                                        match destination.await {
                                            Ok(Ok(Some(mut paths))) => {
                                                if let Some(path) = paths.pop() {
                                    page.update(cx, |page, cx| {
                                        page.start_data_migration(path, cx)
                                    });
                                                }
                                            }
                                            Ok(Ok(None)) => {}
                                            Ok(Err(error)) => {
                                    page.update(cx, |page, cx| {
                                                    page.set_data_migration_error(
                                                        format!("Could not choose a destination: {error:#}"),
                                                        cx,
                                                    )
                                    });
                                            }
                                            Err(error) => {
                                page.update(cx, |page, cx| {
                                                    page.set_data_migration_error(
                                                        format!("Destination picker failed: {error}"),
                                                        cx,
                                                    )
                                });
                                            }
                                        }
                                        anyhow::Ok(())
                                    })
                                    .detach_and_log_err(cx);
                            }
                        }),
                ),
        )
        .when_some(snapshot.data_migration.clone(), |this, state| match state {
            DataMigrationState::Copying => this.child(
                Label::new("Copying application data…")
                    .size(LabelSize::Small)
                    .color(Color::Muted),
            ),
            DataMigrationState::Ready(destination) => {
                let page = page.clone();
                this.child(
                    h_flex()
                        .gap_2()
                        .child(
                            Label::new(format!(
                                "Data copied to {}. Restart to use the new location.",
                                display_path(&destination)
                            ))
                            .size(LabelSize::Small)
                            .color(Color::Success),
                        )
                        .child(
                            Button::new("storage-restart-after-migration", "Restart Now")
                                .style(ButtonStyle::Tinted(TintColor::Accent))
                                .label_size(LabelSize::XSmall)
                                .on_click(move |_, _, cx| {
                                    page.update(cx, |page, cx| {
                                        page.restart_after_data_migration(cx)
                                    });
                                }),
                        ),
                )
            }
            DataMigrationState::Failed(error) => this.child(
                Label::new(format!("Data migration failed: {error}"))
                    .size(LabelSize::Small)
                    .color(Color::Error),
            ),
        })
        .child(Divider::horizontal())
        .child(
            h_flex()
                .w_full()
                .flex_wrap()
                .gap_6()
                .child(stat(
                    "Safe to clean",
                    human_bytes(snapshot.safe_to_clean_bytes() as f64),
                    Color::Default,
                ))
                .child(stat(
                    "Downloaded tools",
                    human_bytes(snapshot.redownloadable_bytes() as f64),
                    Color::Muted,
                ))
                .child(stat(
                    "Protected user data",
                    human_bytes(snapshot.protected_bytes() as f64),
                    Color::Muted,
                ))
                .child(stat(
                    "Project worktrees",
                    human_bytes(snapshot.worktree_bytes() as f64),
                    Color::Muted,
                ))
                .child(stat(
                    "Project checkpoints",
                    human_bytes(snapshot.checkpoint_bytes() as f64),
                    Color::Muted,
                ))
                .when_some(snapshot.last_scan_finished, |this, finished| {
                    this.child(stat("Last scanned", relative_time(finished), Color::Muted))
                })
                .when_some(snapshot.change_since_scan, |this, change| {
                    let amount = human_bytes(change.unsigned_abs() as f64);
                    this.child(stat(
                        "Since previous scan",
                        if change > 0 {
                            format!("+{amount}")
                        } else if change < 0 {
                            format!("−{amount}")
                        } else {
                            "No change".to_string()
                        },
                        Color::Muted,
                    ))
                })
                .when_some(snapshot.last_cleanup.as_ref(), |this, cleanup| {
                    this.child(stat(
                        "Last cleanup",
                        format!(
                            "{} freed · {}{}",
                            human_bytes(cleanup.freed_bytes as f64),
                            relative_time(cleanup.finished_at),
                            if cleanup.errors.is_empty() {
                                String::new()
                            } else {
                                format!(" · {} errors", cleanup.errors.len())
                            }
                        ),
                        Color::Muted,
                    ))
                })
                .children(snapshot.disks.iter().map(|disk| {
                    stat(
                        "Available on disk",
                        format!(
                            "{} of {} · {}",
                            human_bytes(disk.available_bytes as f64),
                            human_bytes(disk.total_bytes as f64),
                            display_path(&disk.mount_point)
                        ),
                        Color::Muted,
                    )
                })),
        )
        .when(scanning, |this| {
            this.child(
                v_flex()
                    .gap_1()
                    .child(
                        Label::new("Scanning disk usage…")
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    )
                    .when(
                        snapshot.projects_scanning && snapshot.project_scan_progress.1 > 0,
                        |this| {
                            let (completed, total) = snapshot.project_scan_progress;
                            this.child(
                                div().w(px(220.)).child(
                                    ProgressBar::new(
                                        "storage-scan-progress",
                                        completed as f32,
                                        total as f32,
                                        cx,
                                    )
                                    .bg_color(colors.background)
                                    .fg_color(colors.text_accent),
                                ),
                            )
                        },
                    ),
            )
        })
        .into_any_element()
}

fn storage_location(label: &'static str, path: &Path) -> AnyElement {
    let path = path.to_path_buf();
    h_flex()
        .gap_1()
        .min_w_0()
        .child(
            Label::new(format!("{label}: {}", display_path(&path)))
                .size(LabelSize::XSmall)
                .color(Color::Muted),
        )
        .child(
            IconButton::new(format!("storage-location-{label}"), IconName::FolderOpen)
                .icon_size(IconSize::XSmall)
                .aria_label(format!("Open {label} directory"))
                .on_click(move |_, _, cx| reveal_path(cx, &path)),
        )
        .into_any_element()
}

fn stat(label: &'static str, value: String, color: Color) -> AnyElement {
    v_flex()
        .gap_0p5()
        .child(
            Label::new(label)
                .size(LabelSize::XSmall)
                .color(Color::Muted),
        )
        .child(Label::new(value).size(LabelSize::Small).color(color))
        .into_any_element()
}

fn storage_summary_line(snapshot: &StoragePageSnapshot, total: u64) -> String {
    let local_projects = snapshot
        .projects
        .iter()
        .filter(|project| !project.remote)
        .count();
    match snapshot.view {
        StorageView::Categories => format!(
            "{} categories · {}",
            snapshot.categories.len(),
            human_bytes(total as f64)
        ),
        StorageView::Projects => {
            if snapshot.projects.is_empty() {
                "No projects are open.".to_string()
            } else if snapshot.projects.iter().any(|project| project.remote) {
                format!("{local_projects} local projects · remote worktrees are on the host")
            } else {
                format!("{local_projects} local projects")
            }
        }
    }
}

fn render_confirmation(
    confirmation: &CleanupConfirmation,
    page: &Entity<StoragePage>,
) -> AnyElement {
    let scope_label = match confirmation.scope {
        CleanupScope::SafeCaches => format!(
            "Clean up {} of safe caches?",
            human_bytes(confirmation.bytes as f64)
        ),
        CleanupScope::Category(kind) => {
            format!("Remove {}?", kind.label())
        }
    };
    let detail = match confirmation.scope {
        CleanupScope::SafeCaches => {
            "Logs, hang traces and temporary files will be removed. Projects, conversation \
             history, databases, extensions and downloaded tools are left untouched."
                .to_string()
        }
        CleanupScope::Category(kind) => kind.description().to_string(),
    };

    Banner::new()
        .severity(Severity::Warning)
        .wrap_content(true)
        .child(
            v_flex().gap_1().child(Label::new(scope_label)).child(
                Label::new(detail)
                    .size(LabelSize::Small)
                    .color(Color::Muted),
            ),
        )
        .action_slot(
            h_flex()
                .gap_2()
                .flex_shrink_0()
                .child(
                    Button::new("storage-clean-cancel", "Cancel")
                        .style(ButtonStyle::OutlinedGhost)
                        .label_size(LabelSize::Small)
                        .on_click({
                            let page = page.clone();
                            move |_, _, cx| {
                                page.update(cx, |page, cx| page.cancel_cleanup(cx));
                            }
                        }),
                )
                .child(
                    Button::new("storage-clean-confirm", "Remove")
                        .style(ButtonStyle::Tinted(TintColor::Warning))
                        .label_size(LabelSize::Small)
                        .on_click({
                            let page = page.clone();
                            move |_, _, cx| {
                                page.update(cx, |page, cx| page.confirm_cleanup(cx));
                            }
                        }),
                ),
        )
        .into_any_element()
}

fn render_cleanup_progress(snapshot: &StoragePageSnapshot, cx: &mut App) -> AnyElement {
    let colors = cx.theme().colors();
    let current = snapshot
        .cleanup
        .current
        .clone()
        .unwrap_or_else(|| "caches".into());
    let (completed, total) = (snapshot.cleanup.completed, snapshot.cleanup.total);

    v_flex()
        .gap_2()
        .p_3()
        .rounded_lg()
        .border_1()
        .border_color(colors.border_variant)
        .bg(colors.elevated_surface_background)
        .child(
            h_flex()
                .w_full()
                .justify_between()
                .child(Label::new(format!("Cleaning {current}…")).size(LabelSize::Small))
                .child(
                    Label::new(format!(
                        "{} freed",
                        human_bytes(snapshot.cleanup.freed_bytes as f64)
                    ))
                    .size(LabelSize::Small)
                    .color(Color::Muted),
                ),
        )
        .child(
            div().w_full().child(
                ProgressBar::new(
                    "storage-cleanup-progress",
                    completed as f32,
                    total.max(1) as f32,
                    cx,
                )
                .bg_color(colors.background)
                .fg_color(colors.text_accent),
            ),
        )
        .into_any_element()
}

fn render_cleanup_errors(snapshot: &StoragePageSnapshot, page: &Entity<StoragePage>) -> AnyElement {
    let errors = &snapshot.cleanup.errors;
    let mut message = errors.first().cloned().unwrap_or_default();
    if errors.len() > 1 {
        message.push_str(&format!(" (and {} more)", errors.len() - 1));
    }

    Banner::new()
        .severity(Severity::Error)
        .wrap_content(true)
        .child(
            v_flex()
                .gap_1()
                .child(Label::new("Some files could not be removed"))
                .child(
                    Label::new(message)
                        .size(LabelSize::Small)
                        .color(Color::Muted),
                ),
        )
        .action_slot(
            Button::new("storage-clean-errors-dismiss", "Dismiss")
                .style(ButtonStyle::OutlinedGhost)
                .label_size(LabelSize::Small)
                .on_click({
                    let page = page.clone();
                    move |_, _, cx| {
                        page.update(cx, |page, cx| page.dismiss_cleanup_errors(cx));
                    }
                }),
        )
        .into_any_element()
}

fn render_charts(
    snapshot: &StoragePageSnapshot,
    page: &Entity<StoragePage>,
    cx: &mut App,
) -> AnyElement {
    let entries = chart_entries(snapshot);
    let chart_total: u64 = entries.iter().map(|entry| entry.bytes).sum();
    let colors: Vec<Hsla> = cx.theme().accents().0.to_vec();
    let hover_background = cx.theme().colors().element_hover;

    chart_card(
        match snapshot.view {
            StorageView::Categories => "Space by category",
            StorageView::Projects => "Space by project",
        },
        render_pie(
            &entries,
            chart_total,
            snapshot.hovered,
            &colors,
            hover_background,
            cx.theme().colors().elevated_surface_background,
            page,
        ),
        "Hover a slice or legend entry to highlight it.",
        cx,
    )
}

fn chart_card(
    title: &'static str,
    content: AnyElement,
    caption: &'static str,
    cx: &App,
) -> AnyElement {
    let colors = cx.theme().colors();
    v_flex()
        .w_full()
        .gap_3()
        .p_4()
        .rounded_lg()
        .border_1()
        .border_color(colors.border_variant)
        .bg(colors.elevated_surface_background)
        .child(
            h_flex()
                .w_full()
                .justify_between()
                .child(Label::new(title).size(LabelSize::Small))
                .child(
                    Label::new(caption)
                        .size(LabelSize::XSmall)
                        .color(Color::Muted),
                ),
        )
        .child(content)
        .into_any_element()
}

#[derive(Clone, Debug)]
struct ChartEntry {
    id: StorageItemId,
    label: SharedString,
    bytes: u64,
    color_index: usize,
}

fn chart_entries(snapshot: &StoragePageSnapshot) -> Vec<ChartEntry> {
    let mut items: Vec<(Option<StorageItemId>, SharedString, u64)> = match snapshot.view {
        StorageView::Categories => {
            let mut items: Vec<_> = snapshot
                .categories
                .iter()
                .filter(|category| category.bytes > 0)
                .map(|category| {
                    (
                        Some(StorageItemId::Category(category.kind)),
                        category.kind.label().into(),
                        category.bytes,
                    )
                })
                .collect();
            let worktrees = snapshot.worktree_bytes();
            if worktrees > 0 {
                items.push((
                    Some(StorageItemId::ProjectWorktrees),
                    StorageCategoryKind::ProjectWorktrees.label().into(),
                    worktrees,
                ));
            }
            let checkpoints = snapshot.checkpoint_bytes();
            if checkpoints > 0 {
                items.push((
                    Some(StorageItemId::ProjectCheckpoints),
                    StorageCategoryKind::ProjectCheckpoints.label().into(),
                    checkpoints,
                ));
            }
            items
        }
        StorageView::Projects => snapshot
            .projects
            .iter()
            .enumerate()
            .filter(|(_, project)| !project.remote && project.bytes + project.checkpoints > 0)
            .map(|(index, project)| {
                (
                    Some(StorageItemId::Project(index)),
                    project.name.clone(),
                    project.bytes.saturating_add(project.checkpoints),
                )
            })
            .collect(),
    };
    items.sort_by_key(|item| std::cmp::Reverse(item.2));

    let mut entries: Vec<ChartEntry> = items
        .iter()
        .take(TOP_CHART_ITEMS)
        .enumerate()
        .filter_map(|(index, (id, label, bytes))| {
            Some(ChartEntry {
                id: (*id)?,
                label: label.clone(),
                bytes: *bytes,
                color_index: index,
            })
        })
        .collect();

    let rest: u64 = items
        .iter()
        .skip(TOP_CHART_ITEMS)
        .map(|(_, _, bytes)| *bytes)
        .sum();
    if rest > 0 {
        let color_index = entries.len();
        entries.push(ChartEntry {
            id: StorageItemId::Other,
            label: "Other".into(),
            bytes: rest,
            color_index,
        });
    }

    entries
}

struct PieSlice {
    id: StorageItemId,
    sweep: f32,
    color: Hsla,
    highlighted: bool,
}

fn render_pie(
    entries: &[ChartEntry],
    total: u64,
    hovered: Option<StorageItemId>,
    colors: &[Hsla],
    hover_background: Hsla,
    center_background: Hsla,
    page: &Entity<StoragePage>,
) -> AnyElement {
    if entries.is_empty() || total == 0 {
        return empty_chart("No disk usage recorded yet.");
    }

    let mut slices = Vec::with_capacity(entries.len());
    for entry in entries {
        let sweep = (entry.bytes as f32 / total as f32).clamp(0.0, 1.0) * std::f32::consts::TAU;
        let color = chart_color(entry, colors);
        let highlighted = hovered == Some(entry.id);
        let color = if hovered.is_some() && !highlighted {
            color.opacity(0.45)
        } else {
            color
        };
        slices.push(PieSlice {
            id: entry.id,
            sweep,
            color,
            highlighted,
        });
    }

    let single_slice = slices.len() == 1;

    let legend = v_flex()
        .w_full()
        .gap_1()
        .children(entries.iter().enumerate().map(|(index, entry)| {
            let color = chart_color(entry, colors);
            let highlighted = hovered == Some(entry.id);
            let percentage = if total == 0 {
                0.0
            } else {
                entry.bytes as f32 / total as f32 * 100.0
            };
            h_flex()
                .id(format!("storage-legend-{index}"))
                .w_full()
                .gap_2()
                .px_1()
                .py_0p5()
                .rounded_sm()
                .hover(|style| style.bg(hover_background.opacity(0.6)))
                .when(highlighted, |this| this.bg(hover_background))
                .child(div().size(px(8.)).rounded_full().bg(color))
                .child(
                    div().flex_1().min_w_0().child(
                        Label::new(entry.label.clone())
                            .size(LabelSize::XSmall)
                            .truncate(),
                    ),
                )
                .child(
                    Label::new(format!(
                        "{} · {:.0}%",
                        human_bytes(entry.bytes as f64),
                        percentage
                    ))
                    .size(LabelSize::XSmall)
                    .color(Color::Muted),
                )
                .on_hover({
                    let page = page.clone();
                    let id = entry.id;
                    move |is_hovered, _, cx| {
                        page.update(cx, |page, cx| {
                            page.set_hovered(is_hovered.then_some(id), cx)
                        });
                    }
                })
                .into_any_element()
        }));

    let pie_bounds: Rc<Cell<Option<Bounds<gpui::Pixels>>>> = Rc::new(Cell::new(None));
    let pie_bounds_for_hover = pie_bounds.clone();
    let slices_for_hover: Vec<(f32, StorageItemId)> =
        slices.iter().map(|slice| (slice.sweep, slice.id)).collect();

    h_flex()
        .w_full()
        .gap_6()
        .items_center()
        .child(
            div()
                .id("storage-pie")
                .relative()
                .flex_shrink_0()
                .size(px(PIE_RADIUS * 2.0 + 24.0))
                .aria_label(format!(
                    "Disk usage distribution: {}",
                    entries
                        .iter()
                        .map(|entry| format!("{} {}", entry.label, human_bytes(entry.bytes as f64)))
                        .collect::<Vec<_>>()
                        .join(", ")
                ))
                .on_mouse_move({
                    let page = page.clone();
                    move |event, _, cx| {
                        let Some(bounds) = pie_bounds_for_hover.get() else {
                            return;
                        };
                        let diameter = bounds.size.width.min(bounds.size.height);
                        let center = point(
                            bounds.origin.x + bounds.size.width / 2.0,
                            bounds.origin.y + bounds.size.height / 2.0,
                        );
                        let dx = (event.position.x - center.x).as_f32();
                        let dy = (event.position.y - center.y).as_f32();
                        let distance = (dx * dx + dy * dy).sqrt();
                        let outer_radius = (diameter / 2.0 - px(8.0)).as_f32();
                        let inner_radius = outer_radius * 0.55;

                        if distance < inner_radius || distance > outer_radius {
                            page.update(cx, |page, cx| page.set_hovered(None, cx));
                            return;
                        }

                        let mut angle = (dy.atan2(dx) + std::f32::consts::FRAC_PI_2)
                            .rem_euclid(std::f32::consts::TAU);
                        let mut hovered = None;
                        for (sweep, id) in &slices_for_hover {
                            if angle >= 0.0 && angle < *sweep {
                                hovered = Some(*id);
                                break;
                            }
                            angle -= sweep;
                        }
                        page.update(cx, |page, cx| page.set_hovered(hovered, cx));
                    }
                })
                .on_hover({
                    let page = page.clone();
                    move |is_hovered, _, cx| {
                        if !is_hovered {
                            page.update(cx, |page, cx| page.set_hovered(None, cx));
                        }
                    }
                })
                .child(
                    canvas(
                        move |bounds, _, _| {
                            pie_bounds.set(Some(bounds));
                        },
                        move |bounds, _, window, _cx| {
                            let diameter = bounds.size.width.min(bounds.size.height);
                            let base_radius = diameter / 2.0 - px(8.0);
                            let center = point(
                                bounds.origin.x + bounds.size.width / 2.0,
                                bounds.origin.y + bounds.size.height / 2.0,
                            );

                            let mut angle = -std::f32::consts::FRAC_PI_2;
                            for slice in &slices {
                                if slice.sweep <= 0.0 {
                                    continue;
                                }
                                let radius = if slice.highlighted {
                                    base_radius + px(4.0)
                                } else {
                                    base_radius
                                };
                                let mut builder = PathBuilder::fill();
                                if single_slice {
                                    builder.move_to(point(center.x + radius, center.y));
                                    builder.arc_to(
                                        point(radius, radius),
                                        px(0.),
                                        false,
                                        true,
                                        point(center.x - radius, center.y),
                                    );
                                    builder.arc_to(
                                        point(radius, radius),
                                        px(0.),
                                        false,
                                        true,
                                        point(center.x + radius, center.y),
                                    );
                                    builder.close();
                                } else {
                                    let end_angle = angle + slice.sweep;
                                    let start_point = point(
                                        center.x + radius * angle.cos(),
                                        center.y + radius * angle.sin(),
                                    );
                                    let end_point = point(
                                        center.x + radius * end_angle.cos(),
                                        center.y + radius * end_angle.sin(),
                                    );
                                    builder.move_to(center);
                                    builder.line_to(start_point);
                                    builder.arc_to(
                                        point(radius, radius),
                                        px(0.),
                                        slice.sweep > std::f32::consts::PI,
                                        true,
                                        end_point,
                                    );
                                    builder.close();
                                }

                                if let Ok(path) = builder.build() {
                                    window.paint_path(path, slice.color);
                                }
                                angle += slice.sweep;
                            }

                            let inner_radius = base_radius * 0.55;
                            let mut builder = PathBuilder::fill();
                            builder.move_to(point(center.x + inner_radius, center.y));
                            builder.arc_to(
                                point(inner_radius, inner_radius),
                                px(0.),
                                false,
                                true,
                                point(center.x - inner_radius, center.y),
                            );
                            builder.arc_to(
                                point(inner_radius, inner_radius),
                                px(0.),
                                false,
                                true,
                                point(center.x + inner_radius, center.y),
                            );
                            builder.close();
                            if let Ok(path) = builder.build() {
                                window.paint_path(path, center_background);
                            }
                        },
                    )
                    .size_full(),
                )
                .child(
                    div()
                        .absolute()
                        .top_0()
                        .left_0()
                        .size_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(
                            v_flex()
                                .items_center()
                                .child(Label::new(human_bytes(total as f64)).size(LabelSize::Small))
                                .child(
                                    Label::new("total")
                                        .size(LabelSize::XSmall)
                                        .color(Color::Muted),
                                ),
                        ),
                ),
        )
        .child(legend)
        .into_any_element()
}

fn empty_chart(message: &'static str) -> AnyElement {
    div()
        .w_full()
        .h(px(150.))
        .flex()
        .items_center()
        .justify_center()
        .child(
            Label::new(message)
                .size(LabelSize::Small)
                .color(Color::Muted),
        )
        .into_any_element()
}

fn chart_color(entry: &ChartEntry, colors: &[Hsla]) -> Hsla {
    colors
        .get(entry.color_index % colors.len().max(1))
        .copied()
        .unwrap_or_else(|| palette_fallback())
}

fn palette_fallback() -> Hsla {
    Hsla {
        h: 0.6,
        s: 0.5,
        l: 0.5,
        a: 1.0,
    }
}

fn render_categories(
    snapshot: &StoragePageSnapshot,
    page: &Entity<StoragePage>,
    cx: &mut App,
) -> AnyElement {
    let total = snapshot.total_bytes();
    let worktree_count: usize = snapshot
        .projects
        .iter()
        .map(|project| project.worktrees.len())
        .sum();
    let project_count = snapshot
        .projects
        .iter()
        .filter(|project| !project.remote)
        .count();

    let mut rows: Vec<AnyElement> = Vec::new();
    if project_count > 0 {
        rows.push(render_project_storage_row(
            StorageItemId::ProjectWorktrees,
            IconName::GitBranch,
            StorageCategoryKind::ProjectWorktrees.label(),
            StorageCategoryKind::ProjectWorktrees.description(),
            format!("{worktree_count} worktrees across {project_count} projects"),
            snapshot.worktree_bytes(),
            total,
            page,
            cx,
        ));
        rows.push(render_project_storage_row(
            StorageItemId::ProjectCheckpoints,
            IconName::Archive,
            StorageCategoryKind::ProjectCheckpoints.label(),
            StorageCategoryKind::ProjectCheckpoints.description(),
            format!("{project_count} projects · .lin/checkpoints.sqlite3"),
            snapshot.checkpoint_bytes(),
            total,
            page,
            cx,
        ));
    }

    for (index, category) in snapshot.categories.iter().enumerate() {
        let expanded = snapshot.expanded_downloads.contains(&category.kind);
        rows.push(
            v_flex()
                .child(render_category_row(index, category, total, expanded, page, cx))
                .when(expanded, |this| {
                    this.children(category.downloads.iter().enumerate().map(|(download_index, download)| {
                        let name = download.name.clone();
                        let name_text = name.to_string_lossy().into_owned();
                        let kind = category.kind;
                        let page = page.clone();
                        h_flex()
                            .w_full().pl_8().pr_3().py_1().gap_3()
                            .child(Label::new(name_text.clone()).size(LabelSize::Small).truncate().flex_1())
                            .child(Label::new(if download.skipped_entries > 0 {
                                format!("{} (incomplete)", human_bytes(download.bytes as f64))
                            } else { human_bytes(download.bytes as f64) }).size(LabelSize::XSmall).color(Color::Muted))
                            .child(Button::new(format!("storage-download-remove-{index}-{download_index}"), "Remove")
                                .style(ButtonStyle::OutlinedGhost).label_size(LabelSize::XSmall)
                                .disabled(snapshot.cleanup.running)
                                .on_click(move |_, window, cx| {
                                    let answer = window.prompt(PromptLevel::Warning, &format!("Remove {name_text}?"),
                                        Some("The download may be needed by a running tool. It will need to be downloaded or installed again."),
                                        &["Remove", "Cancel"], cx);
                                    let page = page.clone();
                                    let name = name.clone();
                                    window.spawn(cx, async move |cx| {
                                        if answer.await == Ok(0) {
                                            page.update(cx, |page, cx| page.remove_download(kind, name, cx));
                                        }
                                        anyhow::Ok(())
                                    }).detach_and_log_err(cx);
                                }))
                            .into_any_element()
                    }))
                })
                .into_any_element(),
        );
    }

    let colors = cx.theme().colors();

    v_flex()
        .w_full()
        .gap_2()
        .child(
            h_flex()
                .w_full()
                .justify_between()
                .gap_4()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(SettingsSectionHeader::new("Categories").no_padding(true)),
                )
                .child(render_view_toggle(snapshot, page)),
        )
        .child(
            v_flex()
                .w_full()
                .rounded_lg()
                .border_1()
                .border_color(colors.border_variant)
                .bg(colors.elevated_surface_background)
                .children(rows.into_iter().enumerate().map(|(index, row)| {
                    v_flex()
                        .when(index > 0, |this| this.child(Divider::horizontal()))
                        .child(row)
                        .into_any_element()
                })),
        )
        .into_any_element()
}

fn render_project_storage_row(
    id: StorageItemId,
    icon: IconName,
    title: &'static str,
    description: &'static str,
    detail: String,
    bytes: u64,
    total: u64,
    page: &Entity<StoragePage>,
    cx: &mut App,
) -> AnyElement {
    let colors = cx.theme().colors();

    h_flex()
        .id(format!("storage-project-storage-{title}"))
        .w_full()
        .gap_3()
        .px_3()
        .py_2()
        .rounded_md()
        .hover(|style| style.bg(colors.element_hover))
        .child(Icon::new(icon).size(IconSize::Small).color(Color::Muted))
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap_0p5()
                .child(Label::new(title))
                .child(
                    Label::new(description)
                        .size(LabelSize::XSmall)
                        .color(Color::Muted)
                        .truncate(),
                )
                .child(
                    Label::new(detail.clone())
                        .size(LabelSize::XSmall)
                        .color(Color::Muted)
                        .truncate(),
                ),
        )
        .child(share_bar(
            bytes,
            total,
            colors.text_accent,
            colors.border_variant,
        ))
        .child(
            v_flex()
                .items_end()
                .gap_0p5()
                .child(Label::new(human_bytes(bytes as f64)).size(LabelSize::Small)),
        )
        .child(
            Button::new(format!("storage-view-projects-{title}"), "View")
                .style(ButtonStyle::OutlinedGhost)
                .label_size(LabelSize::XSmall)
                .tooltip(Tooltip::text("Open the per-project breakdown."))
                .on_click({
                    let page = page.clone();
                    move |_, _, cx| {
                        page.update(cx, |page, cx| page.set_view(StorageView::Projects, cx));
                    }
                }),
        )
        .tooltip(Tooltip::text(format!("{title}\n{description}\n{detail}")))
        .on_hover({
            let page = page.clone();
            move |is_hovered, _, cx| {
                page.update(cx, |page, cx| {
                    page.set_hovered(is_hovered.then_some(id), cx)
                });
            }
        })
        .into_any_element()
}

fn render_category_row(
    index: usize,
    category: &StorageCategory,
    total: u64,
    expanded: bool,
    page: &Entity<StoragePage>,
    cx: &mut App,
) -> AnyElement {
    let colors = cx.theme().colors();
    let path = category.path.clone();
    let size = category.bytes;
    let files = category.files;

    let reveal_button = IconButton::new(("storage-category-reveal", index), IconName::FolderOpen)
        .icon_size(IconSize::Small)
        .icon_color(Color::Muted)
        .aria_label(if cfg!(target_os = "macos") {
            "Reveal in Finder"
        } else {
            "Reveal in file manager"
        })
        .tooltip(Tooltip::text(if cfg!(target_os = "macos") {
            "Reveal in Finder"
        } else {
            "Reveal in file manager"
        }))
        .on_click(move |_, _, cx| reveal_path(cx, &path));

    let clean_button = match category.policy() {
        CleanupPolicy::Protected => div()
            .id(("storage-protected", index))
            .child(
                Label::new("Protected")
                    .size(LabelSize::XSmall)
                    .color(Color::Muted),
            )
            .tooltip(Tooltip::text(
                "This is user data. Removing it would lose work, so it is never cleaned here.",
            ))
            .into_any_element(),
        policy => Button::new(format!("storage-category-clean-{index}"), "Clean")
            .style(ButtonStyle::OutlinedGhost)
            .label_size(LabelSize::XSmall)
            .disabled(category.bytes == 0)
            .tooltip(Tooltip::text(match policy {
                CleanupPolicy::Safe => "Safe to remove.",
                _ => "Removed now and downloaded again when needed.",
            }))
            .on_click({
                let page = page.clone();
                let kind = category.kind;
                move |_, _, cx| {
                    page.update(cx, |page, cx| {
                        page.request_cleanup(CleanupScope::Category(kind), cx)
                    });
                }
            })
            .into_any_element(),
    };

    h_flex()
        .id(("storage-category-row", index))
        .w_full()
        .gap_3()
        .px_3()
        .py_2()
        .rounded_md()
        .hover(|style| style.bg(colors.element_hover))
        .child(
            Icon::new(category_icon(category.kind))
                .size(IconSize::Small)
                .color(Color::Muted),
        )
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap_0p5()
                .child(
                    h_flex()
                        .gap_2()
                        .child(Label::new(category.kind.label()))
                        .child(
                            Label::new(policy_label(category.policy()))
                                .size(LabelSize::XSmall)
                                .color(Color::Muted),
                        ),
                )
                .child(
                    Label::new(category.kind.description())
                        .size(LabelSize::XSmall)
                        .color(Color::Muted)
                        .truncate(),
                )
                .child(
                    Label::new(display_path(&category.path))
                        .size(LabelSize::XSmall)
                        .color(Color::Muted)
                        .truncate(),
                )
                .when(category.skipped_entries > 0, |this| {
                    this.child(
                        Label::new(format!(
                            "{} entries could not be read; size is incomplete",
                            category.skipped_entries
                        ))
                        .size(LabelSize::XSmall)
                        .color(Color::Warning),
                    )
                }),
        )
        .child(share_bar(
            category.bytes,
            total,
            colors.text_accent,
            colors.border_variant,
        ))
        .child(
            v_flex()
                .items_end()
                .gap_0p5()
                .child(Label::new(human_bytes(size as f64)).size(LabelSize::Small))
                .child(
                    Label::new(format!("{files} files"))
                        .size(LabelSize::XSmall)
                        .color(Color::Muted),
                ),
        )
        .when(!category.downloads.is_empty(), |this| {
            this.child(
                Button::new(
                    format!("storage-downloads-{index}"),
                    if expanded { "Hide" } else { "Details" },
                )
                .style(ButtonStyle::OutlinedGhost)
                .label_size(LabelSize::XSmall)
                .on_click({
                    let page = page.clone();
                    let kind = category.kind;
                    move |_, _, cx| {
                        page.update(cx, |page, cx| page.toggle_downloads(kind, cx));
                    }
                }),
            )
        })
        .child(reveal_button)
        .child(clean_button)
        .tooltip(Tooltip::text(format!(
            "{}\n{}\n{files} files",
            category.kind.label(),
            display_path(&category.path),
        )))
        .on_hover({
            let page = page.clone();
            let id = StorageItemId::Category(category.kind);
            move |is_hovered, _, cx| {
                page.update(cx, |page, cx| {
                    page.set_hovered(is_hovered.then_some(id), cx)
                });
            }
        })
        .into_any_element()
}

fn render_projects(
    snapshot: &StoragePageSnapshot,
    page: &Entity<StoragePage>,
    cx: &mut App,
) -> AnyElement {
    let projects_total = snapshot
        .worktree_bytes()
        .saturating_add(snapshot.checkpoint_bytes());
    let visible_projects: Vec<AnyElement> = snapshot
        .projects
        .iter()
        .enumerate()
        .map(|(index, project)| render_project_card(index, project, projects_total, page, cx))
        .collect();

    let colors = cx.theme().colors();

    v_flex()
        .w_full()
        .gap_2()
        .child(
            h_flex()
                .w_full()
                .justify_between()
                .gap_4()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(SettingsSectionHeader::new("Projects").no_padding(true)),
                )
                .child(render_view_toggle(snapshot, page)),
        )
        .when(visible_projects.is_empty(), |this| {
            this.child(
                v_flex()
                    .w_full()
                    .p_6()
                    .gap_1()
                    .items_center()
                    .rounded_lg()
                    .border_1()
                    .border_color(colors.border_variant)
                    .bg(colors.elevated_surface_background)
                    .child(Label::new(if snapshot.projects_scanning {
                        "Looking for projects…"
                    } else {
                        "No projects found"
                    }))
                    .child(
                        Label::new(if snapshot.projects_scanning {
                            "Reading open projects, recent projects and their git worktrees."
                        } else {
                            "Open a project, or open one from the recent-projects list, to see \
                             the worktrees Lin and other tools created for it."
                        })
                        .size(LabelSize::Small)
                        .color(Color::Muted),
                    ),
            )
        })
        .children(visible_projects)
        .into_any_element()
}

fn render_project_card(
    project_index: usize,
    project: &ProjectStorage,
    total: u64,
    page: &Entity<StoragePage>,
    cx: &mut App,
) -> AnyElement {
    let colors = cx.theme().colors();
    let root = project.root.clone();

    v_flex()
        .w_full()
        .rounded_lg()
        .border_1()
        .border_color(colors.border_variant)
        .bg(colors.elevated_surface_background)
        .child(
            h_flex()
                .w_full()
                .gap_3()
                .px_3()
                .py_3()
                .child(Icon::new(IconName::Folder).size(IconSize::Small).color(Color::Muted))
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .gap_0p5()
                        .child(
                            h_flex()
                                .gap_2()
                                .child(Label::new(project.name.clone()))
                                .child(
                                    Label::new(if project.open { "Open" } else { "Recorded" })
                                        .size(LabelSize::XSmall)
                                        .color(if project.open {
                                            Color::Accent
                                        } else {
                                            Color::Muted
                                        }),
                                )
                                .when(project.remote, |this| {
                                    this.child(
                                        Label::new("Remote")
                                            .size(LabelSize::XSmall)
                                            .color(Color::Muted),
                                    )
                                }),
                        )
                        .child(
                            Label::new(display_path(&project.root))
                                .size(LabelSize::XSmall)
                                .color(Color::Muted)
                                .truncate(),
                        )
                        .when(project.checkpoints > 0, |this| {
                            this.child(
                                Label::new(format!(
                                    "Includes {} of agent checkpoints",
                                    human_bytes(project.checkpoints as f64)
                                ))
                                .size(LabelSize::XSmall)
                                .color(Color::Muted)
                                .truncate(),
                            )
                        }),
                )
                .child(share_bar(
                    project.bytes.saturating_add(project.checkpoints),
                    total,
                    colors.text_accent,
                    colors.border_variant,
                ))
                .child(
                    Label::new(if project.remote {
                        "On host".to_string()
                    } else {
                        human_bytes(project.bytes.saturating_add(project.checkpoints) as f64)
                    })
                    .size(LabelSize::Small),
                )
                .when(!project.remote, |this| {
                    this.child(
                        IconButton::new(("storage-project-reveal", project_index), IconName::FolderOpen)
                            .icon_size(IconSize::Small)
                            .icon_color(Color::Muted)
                            .aria_label("Reveal project in file manager")
                            .tooltip(Tooltip::text("Reveal project in file manager"))
                            .on_click(move |_, _, cx| reveal_path(cx, &root)),
                    )
                }),
        )
        .when(project.remote, |this| {
            this.child(
                v_flex()
                    .px_3()
                    .pb_3()
                    .child(Divider::horizontal())
                    .child(
                        Label::new(
                            "This project lives on a remote host. Its worktrees are not scanned from this machine.",
                        )
                        .size(LabelSize::Small)
                        .color(Color::Muted),
                    ),
            )
        })
        .when(!project.remote && project.worktrees.is_empty(), |this| {
            this.child(
                v_flex()
                    .px_3()
                    .pb_3()
                    .gap_1()
                    .child(Divider::horizontal())
                    .child(
                        Label::new("No linked worktrees")
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    ),
            )
        })
        .when(!project.remote && !project.worktrees.is_empty(), |this| {
            this.children(project.worktrees.iter().enumerate().map(
                |(worktree_index, worktree)| {
                    render_worktree_row(project_index, worktree_index, &project.root, worktree, page, cx)
                },
            ))
        })
        .into_any_element()
}

fn render_worktree_row(
    project_index: usize,
    worktree_index: usize,
    project_root: &Path,
    worktree: &WorktreeStorage,
    page: &Entity<StoragePage>,
    cx: &mut App,
) -> AnyElement {
    let colors = cx.theme().colors();
    let path = worktree.path.clone();
    let delete_path = worktree.path.clone();
    let project_root = project_root.to_path_buf();
    let deletion_blocked = worktree.open
        || worktree.references.as_ref().is_none_or(|references| {
            !references.conversations.is_empty() || !references.terminals.is_empty()
        });
    let id = StorageItemId::Worktree {
        project: project_index,
        worktree: worktree_index,
    };

    h_flex()
        .id(format!("storage-worktree-{project_index}-{worktree_index}"))
        .w_full()
        .gap_3()
        .px_3()
        .py_2()
        .border_t_1()
        .border_color(colors.border_variant)
        .hover(|style| style.bg(colors.element_hover))
        .child(
            Icon::new(IconName::GitBranch)
                .size(IconSize::XSmall)
                .color(Color::Muted),
        )
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap_0p5()
                .child(
                    h_flex()
                        .gap_2()
                        .child(Label::new(worktree.name.clone()).size(LabelSize::Small))
                        .when(worktree.managed, |this| {
                            this.child(
                                Label::new("In configured directory")
                                    .size(LabelSize::XSmall)
                                    .color(Color::Muted),
                            )
                        }),
                )
                .when(worktree.open, |this| {
                    this.child(
                        Label::new("Open in a workspace")
                            .size(LabelSize::XSmall)
                            .color(Color::Accent),
                    )
                })
                .when_some(worktree.branch.clone(), |this, branch| {
                    this.child(
                        Label::new(format!("Branch: {branch}"))
                            .size(LabelSize::XSmall)
                            .color(Color::Muted),
                    )
                })
                .when_some(worktree.references.as_ref(), |this, references| {
                    let mut names = references
                        .conversations
                        .iter()
                        .map(|title| title.to_string())
                        .collect::<Vec<_>>();
                    names.extend(references.terminals.iter().map(|title| title.to_string()));
                    if names.is_empty() {
                        this
                    } else {
                        this.child(
                            Label::new(format!("Used by: {}", names.join(", ")))
                                .size(LabelSize::XSmall)
                                .color(Color::Muted)
                                .truncate(),
                        )
                    }
                })
                .when(worktree.references.is_none(), |this| {
                    this.child(
                        Label::new("Conversation references unavailable; rescan to retry")
                            .size(LabelSize::XSmall)
                            .color(Color::Muted),
                    )
                })
                .child(
                    Label::new(display_path(&worktree.path))
                        .size(LabelSize::XSmall)
                        .color(Color::Muted)
                        .truncate(),
                ),
        )
        .child(
            v_flex()
                .items_end()
                .gap_0p5()
                .child(
                    Label::new(match worktree.bytes {
                        Some(bytes) => human_bytes(bytes as f64),
                        None => "Scanning…".to_string(),
                    })
                    .size(LabelSize::Small)
                    .color(if worktree.bytes.is_some() {
                        Color::Default
                    } else {
                        Color::Muted
                    }),
                )
                .when_some(
                    worktree.checkpoints.filter(|bytes| *bytes > 0),
                    |this, bytes| {
                        this.child(
                            Label::new(format!("{} checkpoints", human_bytes(bytes as f64)))
                                .size(LabelSize::XSmall)
                                .color(Color::Muted),
                        )
                    },
                ),
        )
        .child(
            IconButton::new(
                format!("storage-worktree-reveal-{project_index}-{worktree_index}"),
                IconName::FolderOpen,
            )
            .icon_size(IconSize::Small)
            .icon_color(Color::Muted)
            .aria_label("Reveal worktree in file manager")
            .tooltip(Tooltip::text("Reveal worktree in file manager"))
            .on_click(move |_, _, cx| reveal_path(cx, &path)),
        )
        .child(
            IconButton::new(format!("storage-worktree-remove-{project_index}-{worktree_index}"), IconName::Trash)
                .icon_size(IconSize::Small)
                .aria_label("Remove worktree")
                .disabled(deletion_blocked)
                .tooltip(Tooltip::text(if deletion_blocked {
                    "Close associated workspaces, conversations and terminals before removing. Rescan if reference information is unavailable."
                } else {
                    "Remove this worktree"
                }))
                .on_click({
                    let page = page.clone();
                    move |_, window, cx| {
                        let answer = window.prompt(
                            PromptLevel::Warning,
                            "Remove this worktree?",
                            Some("Git will refuse to remove a worktree with uncommitted or untracked changes. Ignored files are also removed. Branches and tags are kept."),
                            &["Remove", "Cancel"], cx,
                        );
                        let page = page.clone();
                        let path = delete_path.clone();
                        let project_root = project_root.clone();
                        window.spawn(cx, async move |cx| {
                            if answer.await == Ok(0) {
                                page.update(cx, |page, cx| page.remove_worktree(project_root, path, cx));
                            }
                            anyhow::Ok(())
                        }).detach_and_log_err(cx);
                    }
                }),
        )
        .tooltip(Tooltip::text(format!(
            "{}{}\n{}",
            worktree.name,
            worktree
                .branch
                .as_ref()
                .map(|branch| format!(" · {branch}"))
                .unwrap_or_default(),
            display_path(&worktree.path)
        )))
        .on_hover({
            let page = page.clone();
            move |is_hovered, _, cx| {
                page.update(cx, |page, cx| {
                    page.set_hovered(is_hovered.then_some(id), cx)
                });
            }
        })
        .into_any_element()
}

fn render_view_toggle(snapshot: &StoragePageSnapshot, page: &Entity<StoragePage>) -> AnyElement {
    let selected = match snapshot.view {
        StorageView::Categories => 0,
        StorageView::Projects => 1,
    };

    ToggleButtonGroup::single_row(
        "storage-view",
        [
            ToggleButtonSimple::new("By Category", {
                let page = page.clone();
                move |_, _, cx| {
                    page.update(cx, |page, cx| page.set_view(StorageView::Categories, cx));
                }
            }),
            ToggleButtonSimple::new("By Project", {
                let page = page.clone();
                move |_, _, cx| {
                    page.update(cx, |page, cx| page.set_view(StorageView::Projects, cx));
                }
            }),
        ],
    )
    .style(ToggleButtonGroupStyle::Outlined)
    .auto_width()
    .selected_index(selected)
    .into_any_element()
}

fn share_bar(bytes: u64, total: u64, color: Hsla, background: Hsla) -> AnyElement {
    let share = if total == 0 {
        0.0
    } else {
        (bytes as f32 / total as f32).clamp(0.0, 1.0)
    };

    div()
        .w(px(88.))
        .h(px(6.))
        .rounded_full()
        .bg(background)
        .when(share > 0.0, |this| {
            this.child(div().w(relative(share)).h_full().rounded_full().bg(color))
        })
        .into_any_element()
}

fn category_icon(kind: StorageCategoryKind) -> IconName {
    match kind {
        StorageCategoryKind::Logs => IconName::FileTextOutlined,
        StorageCategoryKind::HangTraces => IconName::Warning,
        StorageCategoryKind::TemporaryFiles => IconName::Archive,
        StorageCategoryKind::LanguageServers => IconName::Code,
        StorageCategoryKind::AgentServers => IconName::Agent,
        StorageCategoryKind::NodeRuntime => IconName::Terminal,
        StorageCategoryKind::Prettier => IconName::Book,
        StorageCategoryKind::DebugAdapters => IconName::ToolHammer,
        StorageCategoryKind::Copilot => IconName::Sparkle,
        StorageCategoryKind::Embeddings => IconName::MagnifyingGlass,
        StorageCategoryKind::RemoteServers => IconName::Server,
        StorageCategoryKind::DevContainers => IconName::Blocks,
        StorageCategoryKind::RemoteExtensions => IconName::CloudDownload,
        StorageCategoryKind::ConversationHistory => IconName::Thread,
        StorageCategoryKind::Databases => IconName::DatabaseZap,
        StorageCategoryKind::Extensions => IconName::Box,
        StorageCategoryKind::ProjectWorktrees => IconName::GitBranch,
        StorageCategoryKind::ProjectCheckpoints => IconName::Archive,
        StorageCategoryKind::TemporaryWorkingFolders => IconName::FolderSearch,
        StorageCategoryKind::DebuggerCompanionState => IconName::ToolHammer,
        StorageCategoryKind::RemoteServerState => IconName::Server,
    }
}

fn policy_label(policy: CleanupPolicy) -> &'static str {
    match policy {
        CleanupPolicy::Safe => "Safe",
        CleanupPolicy::Reinstallable => "Redownloadable",
        CleanupPolicy::Protected => "Protected",
    }
}

fn reveal_path(cx: &mut App, path: &Path) {
    if path.exists() {
        cx.reveal_path(path);
    } else if let Some(parent) = path.parent() {
        cx.reveal_path(parent);
    }
}

fn display_path(path: &Path) -> String {
    let home = paths::home_dir();
    match path.strip_prefix(&home) {
        Ok(relative) => format!("~/{}", relative.display()),
        Err(_) => path.display().to_string(),
    }
}

fn relative_time(time: SystemTime) -> String {
    match SystemTime::now().duration_since(time) {
        Ok(duration) if duration < Duration::from_secs(5) => "just now".to_string(),
        Ok(duration) if duration < Duration::from_secs(60) => {
            format!("{}s ago", duration.as_secs())
        }
        Ok(duration) if duration < Duration::from_secs(60 * 60) => {
            format!("{}m ago", duration.as_secs() / 60)
        }
        Ok(duration) => format!("{}h ago", duration.as_secs() / (60 * 60)),
        Err(_) => "just now".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{
        CleanupState, ProjectStorage, StorageCategory, StorageCategoryKind, StoragePageSnapshot,
        StorageView,
    };

    fn snapshot(view: StorageView, categories: Vec<StorageCategory>) -> StoragePageSnapshot {
        StoragePageSnapshot {
            view,
            categories,
            projects: Vec::new(),
            categories_scanning: false,
            projects_scanning: false,
            project_scan_progress: (0, 0),
            cleanup: CleanupState::default(),
            confirmation: None,
            hovered: None,
            last_scan_finished: None,
            disks: Vec::new(),
            expanded_downloads: Default::default(),
            last_cleanup: None,
            change_since_scan: None,
            history: Vec::new(),
            history_errors: Vec::new(),
            history_scanning: false,
            data_migration: None,
        }
    }

    fn category(kind: StorageCategoryKind, bytes: u64) -> StorageCategory {
        StorageCategory {
            kind,
            path: std::path::PathBuf::from("/tmp"),
            bytes,
            files: 1,
            skipped_entries: 0,
            exists: true,
            downloads: Vec::new(),
        }
    }

    #[test]
    fn chart_entries_keep_the_largest_items_and_bucket_the_rest() {
        let kinds = [
            StorageCategoryKind::Logs,
            StorageCategoryKind::HangTraces,
            StorageCategoryKind::TemporaryFiles,
            StorageCategoryKind::LanguageServers,
            StorageCategoryKind::AgentServers,
            StorageCategoryKind::NodeRuntime,
            StorageCategoryKind::Prettier,
            StorageCategoryKind::DebugAdapters,
            StorageCategoryKind::Copilot,
            StorageCategoryKind::Embeddings,
        ];
        let categories: Vec<_> = kinds
            .iter()
            .enumerate()
            .map(|(index, kind)| category(*kind, (index as u64 + 1) * 10))
            .collect();
        let total: u64 = categories.iter().map(|category| category.bytes).sum();

        let entries = chart_entries(&snapshot(StorageView::Categories, categories));

        assert_eq!(entries.len(), TOP_CHART_ITEMS);
        assert_eq!(entries.iter().map(|entry| entry.bytes).sum::<u64>(), total);
        assert!(entries[0].bytes >= entries[1].bytes);
    }

    #[test]
    fn chart_entries_are_empty_without_usage() {
        let entries = chart_entries(&snapshot(StorageView::Categories, Vec::new()));
        assert!(entries.is_empty());
    }

    #[test]
    fn project_chart_ignores_remote_projects() {
        let mut snapshot = snapshot(StorageView::Projects, Vec::new());
        snapshot.projects = vec![
            ProjectStorage {
                name: "local".into(),
                root: "/local".into(),
                remote: false,
                open: true,
                worktrees: Vec::new(),
                checkpoint_roots: Vec::new(),
                bytes: 100,
                checkpoints: 0,
            },
            ProjectStorage {
                name: "remote".into(),
                root: "/remote".into(),
                remote: true,
                open: false,
                worktrees: Vec::new(),
                checkpoint_roots: Vec::new(),
                bytes: 1_000,
                checkpoints: 0,
            },
        ];

        let entries = chart_entries(&snapshot);

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].label, "local");
    }
}
