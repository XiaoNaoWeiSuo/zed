use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use db::kvp::KeyValueStore;
use gpui::{
    AnimationExt, AnyElement, App, Bounds, Context, Entity, FocusHandle, Focusable, Global,
    SharedString, Subscription, TaskExt, WeakEntity, Window, WindowBounds, WindowHandle, point,
    prelude::*, px, size,
};
use project::Project;
use serde::{Deserialize, Serialize};
use title_bar::agent_usage::AgentUsageIndicator;
use ui::{GradientFade, Tab, Tooltip, prelude::*};
use util::ResultExt as _;
use workspace::{AppState, MultiWorkspace, Workspace};

use crate::{AgentPanel, ConversationView, ThreadId};

/// The keyboard focus target and root view of a window that shows a single
/// agent conversation outside of the main workspace window.
pub struct CompanionWindow {
    conversation: Entity<ConversationView>,
    thread_id: ThreadId,
    source_workspace: WeakEntity<Workspace>,
    source_project: Entity<Project>,
    app_state: Arc<AppState>,
    title: SharedString,
    returned: bool,
    focus_handle: FocusHandle,
    agent_usage: Entity<AgentUsageIndicator>,
    _subscriptions: Vec<Subscription>,
}

impl CompanionWindow {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        conversation: Entity<ConversationView>,
        thread_id: ThreadId,
        source_workspace: WeakEntity<Workspace>,
        source_project: Entity<Project>,
        app_state: Arc<AppState>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let title = conversation.read(cx).title(cx);
        window.set_window_title(&title);

        let subscription =
            cx.observe_in(&conversation, window, |this, conversation, window, cx| {
                let title = conversation.read(cx).title(cx);
                if title != this.title {
                    this.title = title.clone();
                    window.set_window_title(&title);
                }
                cx.notify();
            });

        let agent_usage = cx.new(|cx| AgentUsageIndicator::new(window, cx));

        Self {
            conversation,
            thread_id,
            source_workspace,
            source_project,
            app_state,
            title,
            returned: false,
            focus_handle: cx.focus_handle(),
            agent_usage,
            _subscriptions: vec![subscription],
        }
    }

    pub fn conversation(&self) -> &Entity<ConversationView> {
        &self.conversation
    }

    pub fn thread_id(&self) -> ThreadId {
        self.thread_id
    }

    /// Moves the conversation back into the workspace window it was detached
    /// from, re-opening that window when it has been closed.
    pub(crate) fn return_to_source(&mut self, cx: &mut Context<Self>) {
        if self.returned {
            return;
        }
        if self.adopt_into_existing_source(cx) {
            self.returned = true;
            unregister_detached_thread(&self.thread_id, cx);
            return;
        }

        if let Err(error) = reopen_source_window(
            self.source_project.clone(),
            self.app_state.clone(),
            self.conversation.clone(),
            cx,
        ) {
            log::error!("failed to re-open the source window for a detached thread: {error:#}");
            return;
        }
        self.returned = true;
        unregister_detached_thread(&self.thread_id, cx);
    }

    /// Closing the window returns the conversation to its source workspace
    /// when that workspace is still around. It never opens a new window, so
    /// closing the app does not spawn one while it shuts down.
    fn release_on_close(&mut self, cx: &mut Context<Self>) {
        if self.returned {
            return;
        }
        if self.adopt_into_existing_source(cx) {
            self.returned = true;
        }
        unregister_detached_thread(&self.thread_id, cx);
    }

    fn adopt_into_existing_source(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(workspace) = self.source_workspace.upgrade() else {
            return false;
        };
        let Some(source_window) = window_for_workspace(cx, &workspace) else {
            return false;
        };
        adopt_in_window(&workspace, source_window, self.conversation.clone(), cx)
    }

    fn render_title_bar(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let thread_view = self.conversation.read(cx).root_thread_view();
        let show_title_edit = thread_view.as_ref().is_some_and(|thread_view| {
            !thread_view
                .read(cx)
                .title_editor
                .read(cx)
                .is_focused(window)
        });

        let running_tasks = thread_view.as_ref().map(|thread_view| {
            thread_view.update(cx, |thread_view, cx| {
                thread_view.render_running_tasks(window, cx)
            })
        });
        let project_memory_button = thread_view.as_ref().and_then(|thread_view| {
            thread_view
                .read(cx)
                .render_project_checkpoint_status(cx)
                .map(|(icon, tooltip)| {
                    let thread_view = thread_view.clone();
                    IconButton::new("project-memory-status", icon)
                        .icon_size(IconSize::Small)
                        .icon_color(if icon == IconName::Warning {
                            Color::Warning
                        } else {
                            Color::Muted
                        })
                        .tooltip(Tooltip::text(tooltip))
                        .on_click(cx.listener(move |_, _, _, cx| {
                            thread_view.update(cx, |thread_view, cx| {
                                thread_view.toggle_project_checkpoint(cx);
                            });
                        }))
                })
        });
        let sandbox_status = thread_view.as_ref().and_then(|thread_view| {
            thread_view.update(cx, |thread_view, cx| thread_view.render_sandbox_status(cx))
        });

        h_flex()
            .id("companion-titlebar")
            .window_control_area(gpui::WindowControlArea::Drag)
            .h(Tab::container_height(cx))
            .flex_shrink_0()
            .pl(px(80.0))
            .pr_2()
            .gap_1()
            .bg(cx.theme().colors().tab_bar_background)
            .border_b_1()
            .border_color(cx.theme().colors().border)
            .on_click(|event, window, _| {
                if event.click_count() == 2 {
                    window.titlebar_double_click();
                }
            })
            .child(div().flex_1().min_w_0().child(render_conversation_title(
                &self.conversation,
                &self.source_workspace,
                show_title_edit,
                cx,
            )))
            .children(running_tasks)
            .children(project_memory_button)
            .children(sandbox_status)
            .child(self.agent_usage.clone())
            .child(
                IconButton::new("return-to-main-window", IconName::ArrowLeft)
                    .icon_size(IconSize::Small)
                    .tooltip(Tooltip::text("Return to Main Window"))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.return_to_source(cx);
                        window.remove_window();
                    })),
            )
            .child(
                IconButton::new("close-companion-window", IconName::Close)
                    .icon_size(IconSize::Small)
                    .tooltip(Tooltip::text("Close Window"))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.release_on_close(cx);
                        window.remove_window();
                    })),
            )
            .into_any_element()
    }
}

impl Render for CompanionWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let title = self.conversation.read(cx).title(cx);
        if title != self.title {
            self.title = title.clone();
            window.set_window_title(&title);
        }

        v_flex()
            .size_full()
            .bg(cx.theme().colors().panel_background)
            .child(self.render_title_bar(window, cx))
            .child(div().flex_1().min_h_0().child(self.conversation.clone()))
    }
}

impl Focusable for CompanionWindow {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

/// Renders the editable title that both the panel toolbar and companion
/// windows share, so a rename is always applied to the single conversation.
pub(crate) fn render_conversation_title(
    conversation_view: &Entity<ConversationView>,
    workspace: &WeakEntity<Workspace>,
    show_edit: bool,
    cx: &App,
) -> AnyElement {
    let conversation = conversation_view.read(cx);
    let native_thread = conversation.as_native_thread(cx);
    let is_generating_title = native_thread
        .as_ref()
        .is_some_and(|thread| thread.read(cx).is_generating_title());
    let title_generation_error = native_thread
        .as_ref()
        .and_then(|thread| thread.read(cx).title_generation_error());

    let content = if let Some(title_editor) = conversation
        .root_thread_view()
        .map(|root| root.read(cx).title_editor.clone())
    {
        if is_generating_title {
            Label::new(conversation.title(cx))
                .color(Color::Muted)
                .truncate()
                .with_animation(
                    "generating_title",
                    gpui::Animation::new(std::time::Duration::from_secs(2))
                        .repeat()
                        .with_easing(gpui::pulsating_between(0.4, 0.8)),
                    |label, delta| label.alpha(delta),
                )
                .into_any_element()
        } else {
            let editable_title = div()
                .flex_1()
                .on_action({
                    let conversation_view = conversation_view.downgrade();
                    move |_: &menu::Confirm, window, cx| {
                        if let Some(conversation_view) = conversation_view.upgrade() {
                            conversation_view
                                .read(cx)
                                .activation_focus_handle(cx)
                                .focus(window, cx);
                        }
                    }
                })
                .on_action({
                    let conversation_view = conversation_view.downgrade();
                    move |_: &editor::actions::Cancel, window, cx| {
                        if let Some(conversation_view) = conversation_view.upgrade() {
                            conversation_view
                                .read(cx)
                                .activation_focus_handle(cx)
                                .focus(window, cx);
                        }
                    }
                })
                .child(title_editor);

            if let Some(title_generation_error) = title_generation_error {
                h_flex()
                    .w_full()
                    .gap_1()
                    .child(editable_title)
                    .child(
                        IconButton::new("retry-thread-title", IconName::XCircle)
                            .icon_color(Color::Error)
                            .icon_size(IconSize::Small)
                            .tooltip(move |_window, cx| {
                                Tooltip::with_meta(
                                    "Title generation failed. Click to retry.",
                                    None,
                                    title_generation_error.clone(),
                                    cx,
                                )
                            })
                            .on_click({
                                let conversation_view = conversation_view.clone();
                                let workspace = workspace.clone();
                                move |_event, _window, cx| {
                                    AgentPanel::handle_regenerate_thread_title(
                                        conversation_view.clone(),
                                        workspace.clone(),
                                        cx,
                                    );
                                }
                            }),
                    )
                    .into_any_element()
            } else {
                editable_title.w_full().into_any_element()
            }
        }
    } else {
        Label::new(conversation.title(cx))
            .color(Color::Muted)
            .truncate()
            .into_any_element()
    };

    let toolbar_bg = cx.theme().colors().tab_bar_background;
    let gradient_overlay = GradientFade::new(toolbar_bg, toolbar_bg, toolbar_bg)
        .width(px(64.0))
        .right(px(0.0))
        .gradient_stop(0.75);
    let opaque_window =
        cx.theme().window_background_appearance() == gpui::WindowBackgroundAppearance::Opaque;

    h_flex()
        .key_context("TitleEditor")
        .group("title_editor")
        .flex_grow_1()
        .w_full()
        .min_w_0()
        .max_w_full()
        .overflow_x_hidden()
        .child(content)
        .when(show_edit, |this| {
            this.when(opaque_window, |this| this.child(gradient_overlay))
                .child(
                    h_flex()
                        .visible_on_hover("title_editor")
                        .absolute()
                        .right_0()
                        .h_full()
                        .bg(toolbar_bg)
                        .child(
                            IconButton::new("edit_tile", IconName::Pencil)
                                .icon_size(IconSize::Small)
                                .tooltip(Tooltip::text("Edit Thread Title")),
                        ),
                )
        })
        .into_any()
}

/// Finds the window that currently displays `workspace`, if any.
pub fn window_for_workspace(
    cx: &App,
    workspace: &Entity<Workspace>,
) -> Option<WindowHandle<MultiWorkspace>> {
    cx.windows().into_iter().find_map(|handle| {
        let handle = handle.downcast::<MultiWorkspace>()?;
        let contains = handle
            .read_with(cx, |multi_workspace, _cx| {
                multi_workspace
                    .workspaces()
                    .any(|candidate| candidate == workspace)
            })
            .ok()?;
        contains.then_some(handle)
    })
}

fn adopt_in_window(
    workspace: &Entity<Workspace>,
    source_window: WindowHandle<MultiWorkspace>,
    conversation: Entity<ConversationView>,
    cx: &mut App,
) -> bool {
    source_window
        .update(cx, |multi_workspace, source_window, cx| {
            multi_workspace.activate(workspace.clone(), None, source_window, cx);
            source_window.activate_window();
            workspace.update(cx, |workspace, cx| {
                let Some(panel) = workspace.panel::<AgentPanel>(cx) else {
                    return false;
                };
                panel.update(cx, |panel, cx| {
                    panel.adopt_conversation_view(conversation, source_window, cx);
                });
                workspace.reveal_panel::<AgentPanel>(source_window, cx);
                workspace.focus_panel::<AgentPanel>(source_window, cx);
                true
            })
        })
        .unwrap_or(false)
}

/// Opens a workspace window backed by the same `Project` the companion window
/// already holds, so no second project (and no second language server) starts.
fn reopen_source_window(
    project: Entity<Project>,
    app_state: Arc<AppState>,
    conversation: Entity<ConversationView>,
    cx: &mut App,
) -> anyhow::Result<WindowHandle<MultiWorkspace>> {
    let mut options = (app_state.build_window_options)(None, cx);
    options.focus = true;
    options.show = true;

    let window = cx.open_window(options, {
        let app_state = app_state.clone();
        move |window, cx| {
            let workspace =
                cx.new(|cx| Workspace::new(None, project.clone(), app_state.clone(), window, cx));
            cx.new(|cx| MultiWorkspace::new(workspace, window, cx))
        }
    })?;

    window.update(cx, |multi_workspace, window, cx| {
        let workspace = multi_workspace.workspace().clone();
        cx.spawn_in(window, async move |multi_workspace, cx| {
            let panel = AgentPanel::load(workspace.downgrade(), cx.clone()).await?;
            multi_workspace.update_in(cx, |_multi_workspace, window, cx| {
                workspace.update(cx, |workspace, cx| {
                    workspace.add_panel(panel.clone(), window, cx);
                    workspace.reveal_panel::<AgentPanel>(window, cx);
                    panel.update(cx, |panel, cx| {
                        panel.adopt_conversation_view(conversation, window, cx);
                    });
                    workspace.focus_panel::<AgentPanel>(window, cx);
                });
            })?;
            anyhow::Ok(())
        })
        .detach_and_log_err(cx);
    })?;

    Ok(window)
}

const DETACHED_THREADS_KEY: &str = "agent_panel__detached_threads";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SerializedDetachedThread {
    pub thread_id: ThreadId,
    pub title: String,
    pub root_paths: Vec<PathBuf>,
}

#[derive(Clone)]
struct DetachedThread {
    thread_id: ThreadId,
    window: WindowHandle<CompanionWindow>,
    title: SharedString,
    root_paths: Vec<PathBuf>,
}

#[derive(Default)]
struct DetachedThreads {
    entries: HashMap<ThreadId, DetachedThread>,
}

impl Global for DetachedThreads {}

fn init_detached_threads(cx: &mut App) {
    if !cx.has_global::<DetachedThreads>() {
        cx.set_global(DetachedThreads::default());
    }
}

/// Returns the companion window that owns `thread_id`, if it is still open.
pub fn detached_thread_window(
    thread_id: &ThreadId,
    cx: &mut App,
) -> Option<WindowHandle<CompanionWindow>> {
    let window = cx
        .try_global::<DetachedThreads>()?
        .entries
        .get(thread_id)?
        .window;
    window.update(cx, |_, _, _| ()).ok().map(|()| window)
}

fn register_detached_thread(thread: DetachedThread, cx: &mut App) {
    init_detached_threads(cx);
    cx.global_mut::<DetachedThreads>()
        .entries
        .insert(thread.thread_id, thread);
    persist_detached_threads(cx);
}

fn unregister_detached_thread(thread_id: &ThreadId, cx: &mut App) {
    if !cx.has_global::<DetachedThreads>() {
        return;
    }
    if cx
        .global_mut::<DetachedThreads>()
        .entries
        .remove(thread_id)
        .is_some()
    {
        persist_detached_threads(cx);
    }
}

fn persist_detached_threads(cx: &mut App) {
    let entries = cx
        .try_global::<DetachedThreads>()
        .map(|threads| {
            threads
                .entries
                .values()
                .map(|entry| SerializedDetachedThread {
                    thread_id: entry.thread_id,
                    title: entry.title.to_string(),
                    root_paths: entry.root_paths.clone(),
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let json = match serde_json::to_string(&entries) {
        Ok(json) => json,
        Err(error) => {
            log::error!("failed to serialize detached agent threads: {error}");
            return;
        }
    };
    let kvp = KeyValueStore::global(cx);
    cx.background_spawn(async move {
        kvp.write_kvp(DETACHED_THREADS_KEY.to_string(), json)
            .await
            .log_err();
    })
    .detach();
}

pub fn read_persisted_detached_threads(cx: &App) -> Vec<SerializedDetachedThread> {
    KeyValueStore::global(cx)
        .read_kvp(DETACHED_THREADS_KEY)
        .log_err()
        .flatten()
        .and_then(|json| serde_json::from_str(&json).log_err())
        .unwrap_or_default()
}

/// Re-creates the companion windows that were open when the app last quit.
///
/// Thread views cannot survive a process restart, so this loads each thread
/// into the given workspace's panel and immediately detaches it again. That
/// keeps a single code path for "the companion window owns this conversation".
pub fn restore_detached_threads(workspace: &Entity<Workspace>, window: &mut Window, cx: &mut App) {
    let Some(panel) = workspace.read(cx).panel::<AgentPanel>(cx) else {
        return;
    };
    let root_paths = workspace
        .read(cx)
        .root_paths(cx)
        .into_iter()
        .map(|path| path.to_path_buf())
        .collect::<Vec<_>>();

    for entry in read_persisted_detached_threads(cx) {
        if entry.root_paths != root_paths || detached_thread_window(&entry.thread_id, cx).is_some()
        {
            continue;
        }
        panel.update(cx, |panel, cx| {
            panel.restore_detached_thread(entry.thread_id, entry.title.clone().into(), window, cx);
        });
    }
}

#[allow(clippy::too_many_arguments)]
pub fn open_companion_window(
    conversation: Entity<ConversationView>,
    thread_id: ThreadId,
    source_workspace: WeakEntity<Workspace>,
    source_project: Entity<Project>,
    app_state: Arc<AppState>,
    root_paths: Vec<PathBuf>,
    cx: &mut App,
) -> anyhow::Result<WindowHandle<CompanionWindow>> {
    init_detached_threads(cx);

    let title = conversation.read(cx).title(cx);
    let mut options = (app_state.build_window_options)(None, cx);
    options.titlebar = Some(gpui::TitlebarOptions {
        title: Some(title.clone()),
        appears_transparent: true,
        traffic_light_position: Some(point(px(9.0), px(9.0))),
    });
    options.window_bounds = Some(WindowBounds::Windowed(Bounds::new(
        point(px(160.0), px(160.0)),
        size(px(760.0), px(900.0)),
    )));
    options.window_min_size = Some(size(px(420.0), px(360.0)));
    options.focus = true;
    options.show = true;

    let handle = cx.open_window(options, move |window, cx| {
        cx.new(|cx| {
            CompanionWindow::new(
                conversation,
                thread_id,
                source_workspace,
                source_project,
                app_state,
                window,
                cx,
            )
        })
    })?;

    handle.update(cx, |companion, window, cx| {
        let weak = cx.entity().downgrade();
        window.on_window_should_close(cx, move |_window, cx| {
            if let Some(companion) = weak.upgrade() {
                companion.update(cx, |companion, cx| companion.release_on_close(cx));
            }
            true
        });
        companion.title.clone()
    })?;

    register_detached_thread(
        DetachedThread {
            thread_id,
            window: handle,
            title,
            root_paths,
        },
        cx,
    );

    Ok(handle)
}

#[cfg(test)]
mod tests {
    use super::SerializedDetachedThread;
    use crate::ThreadId;

    #[test]
    fn detached_thread_metadata_round_trips() {
        let entry = SerializedDetachedThread {
            thread_id: ThreadId::new(),
            title: "Conversation".to_string(),
            root_paths: vec![std::path::PathBuf::from("/project")],
        };

        let json = serde_json::to_string(&[entry.clone()]).expect("serialize detached thread");
        let decoded: Vec<SerializedDetachedThread> =
            serde_json::from_str(&json).expect("deserialize detached thread");

        assert_eq!(decoded.len(), 1);
        assert_eq!(decoded[0].thread_id, entry.thread_id);
        assert_eq!(decoded[0].title, entry.title);
        assert_eq!(decoded[0].root_paths, entry.root_paths);
    }
}
