use gpui::{App, AppContext as _, Context, WeakEntity, Window};
use notifications::status_toast::StatusToast;
use settings::{Settings as _, StorageSettings};
use std::path::PathBuf;
use std::sync::Arc;
use ui::{Color, Icon, IconName, IconSize, SharedString};
use util::ResultExt;
use workspace::{self, Workspace};

pub fn clone_and_open(
    repo_url: SharedString,
    workspace: WeakEntity<Workspace>,
    window: &mut Window,
    cx: &mut App,
    on_success: Arc<
        dyn Fn(&mut Workspace, &mut Window, &mut Context<Workspace>) + Send + Sync + 'static,
    >,
) {
    let configured_directory = StorageSettings::get_global(cx)
        .default_project_directory
        .clone()
        .map(|directory| {
            if directory == "~" {
                util::paths::home_dir().to_path_buf()
            } else if let Some(relative) = directory
                .strip_prefix("~/")
                .or_else(|| directory.strip_prefix("~\\"))
            {
                util::paths::home_dir().join(relative)
            } else {
                PathBuf::from(directory)
            }
        });

    window
        .spawn(cx, async move |cx| {
            let selected_directory = if let Some(directory) = configured_directory {
                let answer = cx.update(|window, cx| window.prompt(
                    gpui::PromptLevel::Info,
                    "Choose clone destination",
                    Some(&format!("Clone into {}?", directory.display())),
                    &["Use Default", "Choose Folder", "Cancel"], cx,
                )).ok()?.await.ok()?;
                match answer {
                    0 => Some(directory),
                    1 => None,
                    _ => return None,
                }
            } else { None };
            let mut destination_dir = if let Some(directory) = selected_directory {
                let check = directory.clone();
                let valid = cx.background_spawn(async move { check.is_absolute() && check.is_dir() }).await;
                if !valid {
                    workspace.update(cx, |workspace, cx| {
                        let toast = StatusToast::new("The default clone directory must be an existing absolute directory.", cx, |this, _| {
                            this.icon(Icon::new(IconName::XCircle).size(IconSize::Small).color(Color::Error)).dismiss_button(true)
                        });
                        workspace.toggle_status_toast(toast, cx);
                    }).log_err();
                    return None;
                }
                directory
            } else {
                let destination_prompt = cx.update(|_, cx| cx.prompt_for_paths(gpui::PathPromptOptions {
                    files: false, directories: true, multiple: false,
                    prompt: Some("Select as Repository Destination".into()),
                })).ok()?;
                let mut paths = destination_prompt.await.ok()?.ok()??;
                paths.pop()?
            };

            let repo_name = repo_url
                .split('/')
                .next_back()
                .map(|name| name.strip_suffix(".git").unwrap_or(name))
                .unwrap_or("repository")
                .to_owned();

            let clone_task = workspace
                .update(cx, |workspace, cx| {
                    let fs = workspace.app_state().fs.clone();
                    let destination_dir = destination_dir.clone();
                    let repo_url = repo_url.clone();
                    cx.spawn(async move |_workspace, _cx| {
                        fs.git_clone(destination_dir.as_path(), &repo_url).await
                    })
                })
                .ok()?;

            if let Err(error) = clone_task.await {
                workspace
                    .update(cx, |workspace, cx| {
                        let toast = StatusToast::new(error.to_string(), cx, |this, _| {
                            this.icon(
                                Icon::new(IconName::XCircle)
                                    .size(IconSize::Small)
                                    .color(Color::Error),
                            )
                            .dismiss_button(true)
                        });
                        workspace.toggle_status_toast(toast, cx);
                    })
                    .log_err();
                return None;
            }

            let has_worktrees = workspace
                .read_with(cx, |workspace, cx| {
                    workspace.project().read(cx).worktrees(cx).next().is_some()
                })
                .ok()?;

            let prompt_answer = if has_worktrees {
                cx.update(|window, cx| {
                    window.prompt(
                        gpui::PromptLevel::Info,
                        &format!("Git Clone: {}", repo_name),
                        None,
                        &["Add repo to project", "Open repo in new project"],
                        cx,
                    )
                })
                .ok()?
                .await
                .ok()?
            } else {
                // Don't ask if project is empty
                0
            };

            destination_dir.push(&repo_name);

            match prompt_answer {
                0 => {
                    workspace
                        .update_in(cx, |workspace, window, cx| {
                            let create_task = workspace.project().update(cx, |project, cx| {
                                project.create_worktree(destination_dir.as_path(), true, cx)
                            });

                            let workspace_weak = cx.weak_entity();
                            let on_success = on_success.clone();
                            cx.spawn_in(window, async move |_window, cx| {
                                if create_task.await.log_err().is_some() {
                                    workspace_weak
                                        .update_in(cx, |workspace, window, cx| {
                                            (on_success)(workspace, window, cx);
                                        })
                                        .ok();
                                }
                            })
                            .detach();
                        })
                        .ok()?;
                }
                1 => {
                    workspace
                        .update(cx, move |workspace, cx| {
                            let app_state = workspace.app_state().clone();
                            let destination_path = destination_dir.clone();
                            let on_success = on_success.clone();

                            workspace::open_new(
                                Default::default(),
                                app_state,
                                cx,
                                move |workspace, window, cx| {
                                    cx.activate(true);

                                    let create_task =
                                        workspace.project().update(cx, |project, cx| {
                                            project.create_worktree(
                                                destination_path.as_path(),
                                                true,
                                                cx,
                                            )
                                        });

                                    let workspace_weak = cx.weak_entity();
                                    cx.spawn_in(window, async move |_window, cx| {
                                        if create_task.await.log_err().is_some() {
                                            workspace_weak
                                                .update_in(cx, |workspace, window, cx| {
                                                    (on_success)(workspace, window, cx);
                                                })
                                                .ok();
                                        }
                                    })
                                    .detach();
                                },
                            )
                            .detach();
                        })
                        .ok();
                }
                _ => {}
            }

            Some(())
        })
        .detach();
}
