use crate::{
    InfinityContext, InfinityConversation, InfinityJobs, InfinitySend, InfinityStop,
    InfinityToggleToolTraffic,
};
use agent::infinity::{Entry, Infinity, JobStatus};
use editor::Editor;
use gpui::{
    Anchor, App, Context, Entity, EventEmitter, FocusHandle, Focusable, KeyContext, ScrollHandle,
    SharedString, Subscription, Window, prelude::*,
};
use language_model::{LanguageModelRegistry, MessageContent, Role};
use markdown::{Markdown, MarkdownElement, MarkdownFont, MarkdownStyle};
use project::Project;
use ui::{
    Button, ButtonStyle, Color, ContextMenu, Icon, IconName, IconSize, Label, LabelSize,
    PopoverMenu, h_flex, prelude::*, v_flex,
};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Surface {
    Conversation,
    Jobs,
    Context,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ContextPage {
    Main,
    Evidence,
    Usage,
}

pub(crate) struct InfinityViewEvent;
impl EventEmitter<InfinityViewEvent> for InfinityView {}

pub(crate) struct InfinityView {
    session: Entity<Infinity>,
    languages: std::sync::Arc<language::LanguageRegistry>,
    rendered_messages: Vec<Option<Entity<Markdown>>>,
    rendered_stream: Entity<Markdown>,
    sidebar_title: String,
    sidebar_running: bool,
    composer: Entity<Editor>,
    navigation_focus: FocusHandle,
    surface: Surface,
    context_page: ContextPage,
    tool_traffic_only: bool,
    error: Option<String>,
    selected_evidence: Option<String>,
    selected_message: Option<usize>,
    selected_job_result: Option<String>,
    conversation_scroll: ScrollHandle,
    _session_observation: Subscription,
}

impl InfinityView {
    pub(crate) fn new(
        project: Entity<Project>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let languages = project.read(cx).languages().clone();
        let rendered_stream =
            cx.new(|cx| Markdown::new(SharedString::default(), Some(languages.clone()), None, cx));
        let session = cx.new(|cx| Infinity::new(project, cx));
        let composer = cx.new(|cx| {
            let mut editor = Editor::auto_height(2, 7, window, cx);
            editor.set_placeholder_text("Message Infinity…", window, cx);
            editor
        });
        let _session_observation = cx.observe(&session, |this: &mut Self, _, cx| {
            this.sync_markdown(cx);
            if this.surface == Surface::Conversation
                && this.conversation_scroll.offset().y.abs()
                    >= this.conversation_scroll.max_offset().y - gpui::px(24.)
            {
                this.conversation_scroll.scroll_to_bottom();
            }
            cx.notify();
        });
        Self {
            session,
            languages,
            rendered_messages: Vec::new(),
            rendered_stream,
            sidebar_title: "New Infinity session".into(),
            sidebar_running: false,
            composer,
            navigation_focus: cx.focus_handle(),
            surface: Surface::Conversation,
            context_page: ContextPage::Main,
            tool_traffic_only: false,
            error: None,
            selected_evidence: None,
            selected_message: None,
            selected_job_result: None,
            conversation_scroll: ScrollHandle::new(),
            _session_observation,
        }
    }

    pub(crate) fn is_running(&self, cx: &App) -> bool {
        self.session.read(cx).is_running()
    }

    fn sync_markdown(&mut self, cx: &mut Context<Self>) {
        let session = self.session.read(cx);
        let new_entries = session
            .entries
            .iter()
            .skip(self.rendered_messages.len())
            .map(|entry| match entry {
                Entry::User(text) | Entry::Assistant(text) => {
                    Some(SharedString::from(text.clone()))
                }
                Entry::Notice(_) => None,
            })
            .collect::<Vec<_>>();
        let streaming = SharedString::from(session.streaming.clone());
        let running = session.is_running();
        let title = self.title(cx);
        self.rendered_messages
            .extend(new_entries.into_iter().map(|text| {
                text.map(|text| {
                    let languages = self.languages.clone();
                    cx.new(|cx| Markdown::new(text, Some(languages), None, cx))
                })
            }));
        if self.rendered_stream.read(cx).source() != &streaming {
            self.rendered_stream
                .update(cx, |markdown, cx| markdown.replace(streaming, cx));
        }
        if title != self.sidebar_title || running != self.sidebar_running {
            self.sidebar_title = title;
            self.sidebar_running = running;
            cx.emit(InfinityViewEvent);
        }
    }

    pub(crate) fn title(&self, cx: &App) -> String {
        self.session
            .read(cx)
            .entries
            .iter()
            .find_map(|entry| match entry {
                Entry::User(text) => Some(
                    text.lines()
                        .next()
                        .unwrap_or_default()
                        .chars()
                        .take(64)
                        .collect(),
                ),
                _ => None,
            })
            .unwrap_or_else(|| "New Infinity session".into())
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_running(cx) {
            return;
        }
        let input = self.composer.read(cx).text(cx);
        if input.trim().is_empty() {
            return;
        }
        match self
            .session
            .update(cx, |session, cx| session.submit(input, cx))
        {
            Ok(()) => {
                self.error = None;
                self.surface = Surface::Conversation;
                self.conversation_scroll.scroll_to_bottom();
                self.composer
                    .update(cx, |editor, cx| editor.set_text("", window, cx));
            }
            Err(error) => self.error = Some(error.to_string()),
        }
        cx.notify();
    }

    fn select_surface(&mut self, surface: Surface, window: &mut Window, cx: &mut Context<Self>) {
        self.surface = surface;
        self.focus_handle(cx).focus(window, cx);
        cx.notify();
    }

    fn tab(
        &self,
        surface: Surface,
        label: impl Into<gpui::SharedString>,
        cx: &Context<Self>,
    ) -> Button {
        Button::new(
            match surface {
                Surface::Conversation => "infinity-conversation",
                Surface::Jobs => "infinity-jobs",
                Surface::Context => "infinity-context",
            },
            label,
        )
        .style(if self.surface == surface {
            ButtonStyle::Filled
        } else {
            ButtonStyle::Subtle
        })
        .label_size(LabelSize::Small)
        .on_click(cx.listener(move |this, _, window, cx| this.select_surface(surface, window, cx)))
    }

    fn conversation(&self, window: &Window, cx: &Context<Self>) -> impl IntoElement {
        let session = self.session.read(cx);
        v_flex()
            .id("infinity-conversation-scroll")
            .min_h_0()
            .flex_1()
            .overflow_y_scroll()
            .track_scroll(&self.conversation_scroll)
            .p_3()
            .gap_3()
            .when(session.entries.is_empty(), |this| {
                this.child(
                    v_flex()
                        .gap_2()
                        .p_3()
                        .child(Icon::new(IconName::Sparkle).color(Color::Accent))
                        .child(Label::new("What are you investigating?"))
                        .child(
                            Label::new(
                                "Ask Infinity to inspect code, investigate logs, review a change, or research a question.",
                            )
                            .color(Color::Muted),
                        ),
                )
            })
            .children(session.entries.iter().enumerate().map(|(index,entry)| {
                let (label,text,user) = match entry {
                    Entry::User(text) => ("You",text,true),
                    Entry::Assistant(text) => ("Infinity",text,false),
                    Entry::Notice(text) => ("Session",text,false),
                };
                match self.rendered_messages.get(index).and_then(Option::as_ref) {
                    Some(markdown) => Self::message_card(label,user,cx).child(MarkdownElement::new(
                        markdown.clone(),MarkdownStyle::themed(MarkdownFont::Agent,window,cx)
                    )).into_any_element(),
                    None => Self::message(label,text,user,cx).into_any_element(),
                }
            }))
            .when(!session.streaming.is_empty(), |this| {
                this.child(Self::message_card("Infinity",false,cx).child(MarkdownElement::new(
                    self.rendered_stream.clone(),MarkdownStyle::themed(MarkdownFont::Agent,window,cx)
                )))
            })
            .when_some(session.question.as_ref(), |this, question| {
                this.child(Self::message("Infinity asks", question, false, cx))
            })
    }

    fn message(label: &str, text: &str, user: bool, cx: &App) -> impl IntoElement {
        Self::message_card(label, user, cx).child(Label::new(text.to_string()))
    }

    fn message_card(label: &str, user: bool, cx: &App) -> gpui::Div {
        v_flex()
            .w_full()
            .gap_1()
            .p_2()
            .rounded_md()
            .when(user, |this| this.bg(cx.theme().colors().editor_background))
            .child(
                Label::new(label.to_string())
                    .size(LabelSize::Small)
                    .color(Color::Muted),
            )
    }

    fn jobs(&self, cx: &Context<Self>) -> impl IntoElement {
        let session = self.session.read(cx);
        v_flex()
            .id("infinity-jobs-scroll")
            .min_h_0()
            .flex_1()
            .overflow_y_scroll()
            .p_3()
            .gap_2()
            .when(session.jobs.is_empty(), |this| {
                this.child(Label::new("Delegated jobs will appear here.").color(Color::Muted))
            })
            .children(session.jobs.iter().rev().map(|job| {
                v_flex()
                    .w_full()
                    .p_2()
                    .gap_1()
                    .rounded_md()
                    .border_1()
                    .border_color(cx.theme().colors().border_variant)
                    .child(
                        h_flex()
                            .justify_between()
                            .gap_2()
                            .child(
                                Label::new(job.role.label())
                                    .size(LabelSize::Small)
                                    .color(Color::Muted),
                            )
                            .child(Label::new(job.status.label()).size(LabelSize::Small).color(
                                match job.status {
                                    JobStatus::Ok => Color::Success,
                                    JobStatus::Running => Color::Accent,
                                    JobStatus::Partial => Color::Warning,
                                    JobStatus::Cancelled => Color::Muted,
                                    _ => Color::Error,
                                },
                            )),
                    )
                    .child(Label::new(job.goal.clone()))
                    .when_some(job.data.as_ref(), |this, data| {
                        let selected = self.selected_job_result.as_ref() == Some(&job.id);
                        let id = job.id.clone();
                        this.child(
                            Button::new(
                                format!("infinity-result-{}", job.id),
                                if selected {
                                    "Hide result"
                                } else {
                                    "View result"
                                },
                            )
                            .label_size(LabelSize::Small)
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    this.selected_job_result = (this.selected_job_result.as_ref()
                                        != Some(&id))
                                    .then(|| id.clone());
                                    cx.notify();
                                },
                            )),
                        )
                        .when(selected, |this| {
                            let text = serde_json::to_string_pretty(data)
                                .unwrap_or_else(|error| format!("Result preview failed: {error}"));
                            this.child(
                                div()
                                    .p_2()
                                    .text_sm()
                                    .font_family("monospace")
                                    .bg(cx.theme().colors().editor_background)
                                    .child(text),
                            )
                        })
                    })
                    .children(job.paths.iter().map(|path| {
                        Label::new(path.clone())
                            .size(LabelSize::Small)
                            .color(Color::Muted)
                    }))
                    .children(
                        job.findings
                            .iter()
                            .map(|finding| Label::new(finding.text.clone())),
                    )
                    .children(job.limitations.iter().map(|limit| {
                        Label::new(limit.clone())
                            .size(LabelSize::Small)
                            .color(Color::Muted)
                    }))
                    .children(job.evidence_refs.iter().filter_map(|reference| {
                        let evidence = session.evidence(reference, cx)?;
                        let reference = reference.clone();
                        Some(
                            Button::new(
                                reference.clone(),
                                format!("{}:{}", evidence.path, evidence.start_line),
                            )
                            .label_size(LabelSize::Small)
                            .on_click(cx.listener(
                                move |this, _, window, cx| {
                                    this.selected_evidence = Some(reference.clone());
                                    this.selected_message = None;
                                    this.context_page = ContextPage::Evidence;
                                    this.select_surface(Surface::Context, window, cx);
                                },
                            )),
                        )
                    }))
            }))
    }

    fn approval(&self, cx: &Context<Self>) -> impl IntoElement {
        let session = self.session.read(cx);
        v_flex()
            .gap_2()
            .p_2()
            .border_b_1()
            .border_color(cx.theme().colors().border_variant)
            .children(session.approvals.iter().take(1).map(|approval| {
                let allow = approval.job_id.clone();
                let deny = approval.job_id.clone();
                v_flex()
                    .gap_2()
                    .child(Label::new("Approval required").color(Color::Warning))
                    .child(
                        div()
                            .id("infinity-approval-description")
                            .max_h(gpui::px(220.))
                            .overflow_y_scroll()
                            .child(Label::new(approval.description.clone())),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .justify_end()
                            .child(Button::new("deny-infinity-approval", "Deny").on_click(
                                cx.listener(move |this, _, _, cx| {
                                    this.session.update(cx, |session, cx| {
                                        session.respond_to_approval(&deny, false, cx)
                                    });
                                }),
                            ))
                            .child(
                                Button::new("allow-infinity-approval", "Allow once")
                                    .style(ButtonStyle::Filled)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.session.update(cx, |session, cx| {
                                            session.respond_to_approval(&allow, true, cx)
                                        });
                                    })),
                            ),
                    )
            }))
    }

    fn context(&self, cx: &Context<Self>) -> impl IntoElement {
        let session = self.session.read(cx);
        let (current, limit) = session.context_bytes();
        let percent = (current as f32 / limit as f32 * 100.).clamp(0., 100.);
        let pages = [
            (ContextPage::Main, "Main request"),
            (ContextPage::Evidence, "Evidence"),
            (ContextPage::Usage, "Usage"),
        ];
        v_flex()
            .debug_selector(|| "infinity-context-inspector".into())
            .min_h_0()
            .flex_1()
            .child(
                v_flex()
                    .p_3()
                    .gap_2()
                    .border_b_1()
                    .border_color(cx.theme().colors().border_variant)
                    .child(Label::new("Context inspector").size(LabelSize::Large))
                    .child(
                        Label::new(format!(
                            "{:.1} / {:.0} KiB · {} committed messages",
                            current as f32 / 1024.,
                            limit as f32 / 1024.,
                            session.context_messages().len()
                        ))
                        .size(LabelSize::Small)
                        .color(Color::Muted),
                    )
                    .child(
                        div()
                            .w_full()
                            .h(gpui::px(4.))
                            .rounded_full()
                            .overflow_hidden()
                            .bg(cx.theme().colors().border_variant)
                            .child(
                                div()
                                    .h_full()
                                    .w(gpui::relative(percent / 100.))
                                    .bg(cx.theme().colors().text_accent),
                            ),
                    )
                    .child(h_flex().gap_1().children(pages.into_iter().enumerate().map(
                        |(index, (page, title))| {
                            Button::new(("infinity-context-page", index), title)
                                .label_size(LabelSize::Small)
                                .style(if self.context_page == page {
                                    ButtonStyle::Filled
                                } else {
                                    ButtonStyle::Subtle
                                })
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.context_page = page;
                                    cx.notify();
                                }))
                        },
                    ))),
            )
            .child(
                v_flex()
                    .id("infinity-context-scroll")
                    .min_h_0()
                    .flex_1()
                    .overflow_y_scroll()
                    .p_3()
                    .gap_3()
                    .child(match self.context_page {
                        ContextPage::Main => self.context_main(cx).into_any_element(),
                        ContextPage::Evidence => self.context_evidence(cx).into_any_element(),
                        ContextPage::Usage => self.context_usage(cx).into_any_element(),
                    }),
            )
    }

    fn context_main(&self, cx: &Context<Self>) -> impl IntoElement {
        let session = self.session.read(cx);
        let active_jobs = session
            .jobs
            .iter()
            .filter(|job| job.status == JobStatus::Running)
            .count();
        v_flex()
            .gap_2()
            .child(
                h_flex()
                    .gap_2()
                    .justify_between()
                    .child(Label::new("Committed history"))
                    .child(
                        Button::new(
                            "infinity-tool-traffic",
                            if self.tool_traffic_only {
                                "Show all"
                            } else {
                                "Tool traffic only"
                            },
                        )
                        .label_size(LabelSize::Small)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.tool_traffic_only = !this.tool_traffic_only;
                            cx.notify();
                        })),
                    ),
            )
            .when(active_jobs > 0, |this| {
                this.child(
                    Label::new(format!(
                        "{active_jobs} active jobs · results enter the main request together"
                    ))
                    .size(LabelSize::Small)
                    .color(Color::Accent),
                )
            })
            .children(session.context_messages().iter().enumerate().filter_map(
                |(index, message)| {
                    let tool = message.content.iter().find_map(|content| match content {
                        MessageContent::ToolUse(call) => Some(format!("Offer · {}", call.name)),
                        MessageContent::ToolResult(result) => {
                            Some(format!("Result · {}", result.tool_name))
                        }
                        _ => None,
                    });
                    if self.tool_traffic_only && tool.is_none() {
                        return None;
                    }
                    let kind = tool.unwrap_or_else(|| match message.role {
                        Role::System => "System prompt and protocol".into(),
                        Role::Assistant => "Main model response".into(),
                        Role::User => "User message".into(),
                    });
                    let selected = self.selected_message == Some(index);
                    Some(
                        v_flex()
                            .p_1()
                            .gap_1()
                            .rounded_md()
                            .border_1()
                            .border_color(cx.theme().colors().border_variant)
                            .child(
                                Button::new(
                                    ("infinity-context-message", index),
                                    format!("{} · {kind}", index + 1),
                                )
                                .label_size(LabelSize::Small)
                                .style(if selected {
                                    ButtonStyle::Filled
                                } else {
                                    ButtonStyle::Subtle
                                })
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        this.selected_message =
                                            (this.selected_message != Some(index)).then_some(index);
                                        cx.notify();
                                    },
                                )),
                            )
                            .when(selected, |this| {
                                let pretty = |text: &str| {
                                    serde_json::from_str::<serde_json::Value>(text)
                                        .ok()
                                        .and_then(|value| serde_json::to_string_pretty(&value).ok())
                                        .unwrap_or_else(|| text.to_string())
                                };
                                let preview = message
                                    .content
                                    .iter()
                                    .filter_map(|part| match part {
                                        MessageContent::Text(text) => Some(text.clone()),
                                        MessageContent::ToolUse(call) => {
                                            Some(pretty(&call.raw_input))
                                        }
                                        MessageContent::ToolResult(result) => {
                                            Some(pretty(&result.text_contents()))
                                        }
                                        _ => None,
                                    })
                                    .collect::<Vec<_>>()
                                    .join("\n");
                                let mut characters = preview.chars();
                                let mut text: String = characters.by_ref().take(6000).collect();
                                if characters.next().is_some() {
                                    text.push('…');
                                }
                                this.child(
                                    v_flex()
                                        .gap_1()
                                        .p_2()
                                        .rounded_md()
                                        .bg(cx.theme().colors().editor_background)
                                        .child(
                                            h_flex()
                                                .justify_between()
                                                .child(
                                                    Label::new("Message payload")
                                                        .size(LabelSize::Small)
                                                        .color(Color::Muted),
                                                )
                                                .child(
                                                    Button::new(
                                                        ("copy-infinity-payload", index),
                                                        "Copy",
                                                    )
                                                    .label_size(LabelSize::Small)
                                                    .on_click(move |_, _, cx| {
                                                        cx.write_to_clipboard(
                                                            gpui::ClipboardItem::new_string(
                                                                preview.clone(),
                                                            ),
                                                        );
                                                    }),
                                                ),
                                        )
                                        .child(
                                            div()
                                                .id(("infinity-context-payload", index))
                                                .max_h(gpui::px(320.))
                                                .overflow_scroll()
                                                .font_family("monospace")
                                                .text_sm()
                                                .child(text),
                                        ),
                                )
                            }),
                    )
                },
            ))
    }

    fn context_evidence(&self, cx: &Context<Self>) -> impl IntoElement {
        let session = self.session.read(cx);
        let evidence = self
            .selected_evidence
            .as_ref()
            .and_then(|reference| session.evidence(reference, cx));
        v_flex()
            .gap_3()
            .child(
                Label::new(
                    "Evidence stays with its job until the main model explicitly requests it.",
                )
                .size(LabelSize::Small)
                .color(Color::Muted),
            )
            .when_some(evidence, |this, evidence| {
                this.child(
                    v_flex()
                        .gap_2()
                        .p_2()
                        .rounded_md()
                        .bg(cx.theme().colors().editor_background)
                        .child(Label::new(evidence.path.clone()).color(Color::Accent))
                        .child(
                            Label::new(format!(
                                "Line {} · captured {}",
                                evidence.start_line,
                                evidence
                                    .captured_at
                                    .with_timezone(&chrono::Local)
                                    .format("%H:%M:%S")
                            ))
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                        )
                        .child(
                            div()
                                .text_sm()
                                .font_family("monospace")
                                .child(evidence.text.clone()),
                        ),
                )
            })
            .children(session.jobs.iter().rev().filter_map(|job| {
                let refs = job
                    .evidence_refs
                    .iter()
                    .filter_map(|reference| session.evidence(reference, cx))
                    .collect::<Vec<_>>();
                if refs.is_empty() {
                    return None;
                }
                Some(
                    v_flex()
                        .gap_1()
                        .p_2()
                        .border_1()
                        .rounded_md()
                        .border_color(cx.theme().colors().border_variant)
                        .child(
                            Label::new(job.role.label())
                                .color(Color::Muted)
                                .size(LabelSize::Small),
                        )
                        .child(Label::new(job.goal.clone()))
                        .children(refs.into_iter().map(|item| {
                            let reference = item.reference.clone();
                            Button::new(item.reference.clone(), item.path.clone())
                                .label_size(LabelSize::Small)
                                .truncate(true)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.selected_evidence = Some(reference.clone());
                                    cx.notify();
                                }))
                        })),
                )
            }))
    }

    fn context_usage(&self, cx: &Context<Self>) -> impl IntoElement {
        let session = self.session.read(cx);
        let zero_token_jobs = session
            .jobs
            .iter()
            .filter(|job| job.status == JobStatus::Ok && job.usage.total_tokens() == 0)
            .count();
        let mut roles = std::collections::BTreeMap::<_, language_model::TokenUsage>::new();
        for job in &session.jobs {
            if job.usage.total_tokens() > 0 {
                roles
                    .entry(job.role.label())
                    .and_modify(|usage| *usage = *usage + job.usage)
                    .or_insert(job.usage);
            }
        }
        v_flex()
            .gap_2()
            .child(
                Label::new("Provider-reported token usage")
                    .color(Color::Muted)
                    .size(LabelSize::Small),
            )
            .child(Self::usage("Main model", session.main_usage, cx))
            .child(Self::usage("Tool models", session.worker_usage, cx))
            .child(
                Label::new(format!(
                    "{zero_token_jobs} completed jobs reported no model tokens"
                ))
                .size(LabelSize::Small)
                .color(Color::Muted),
            )
            .when(!roles.is_empty(), |this| {
                this.child(
                    Label::new("By role")
                        .size(LabelSize::Small)
                        .color(Color::Muted),
                )
            })
            .children(
                roles
                    .into_iter()
                    .map(|(role, usage)| Self::usage(role, usage, cx)),
            )
    }

    fn usage(label: &str, usage: language_model::TokenUsage, cx: &App) -> impl IntoElement {
        let input =
            usage.input_tokens + usage.cache_read_input_tokens + usage.cache_creation_input_tokens;
        v_flex()
            .p_2()
            .gap_1()
            .rounded_md()
            .bg(cx.theme().colors().editor_background)
            .child(
                h_flex()
                    .gap_2()
                    .justify_between()
                    .child(Label::new(label.to_string()))
                    .when(input > 0, |this| {
                        this.child(
                            Label::new(format!(
                                "{:.0}% cache read",
                                100. * usage.cache_read_input_tokens as f64 / input as f64
                            ))
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                        )
                    }),
            )
            .child(
                Label::new(format!(
                    "{} uncached input · {} output · {} cache read · {} cache write",
                    usage.input_tokens,
                    usage.output_tokens,
                    usage.cache_read_input_tokens,
                    usage.cache_creation_input_tokens
                ))
                .size(LabelSize::Small)
                .color(Color::Muted),
            )
    }

    fn worker_picker(&self, cx: &Context<Self>) -> impl IntoElement {
        let session = self.session.read(cx);
        let label = session.worker_model.as_ref().map_or_else(
            || "Tool model: off".to_string(),
            |model| format!("Tool: {}", model.name().0),
        );
        let weak_session = self.session.downgrade();
        PopoverMenu::new("infinity-worker-model")
            .anchor(Anchor::TopRight)
            .trigger(
                Button::new("select-infinity-worker-model", label)
                    .label_size(LabelSize::Small)
                    .truncate(true)
                    .disabled(session.is_running()),
            )
            .menu(move |window, cx| {
                let mut models: Vec<_> = LanguageModelRegistry::read_global(cx)
                    .available_models(cx)
                    .filter(|model| Infinity::supports_worker_model(model, cx))
                    .collect();
                models.sort_by_cached_key(|model| {
                    format!("{} / {}", model.provider_name().0, model.name().0)
                });
                Some(ContextMenu::build(window, cx, |menu, _, _| {
                    let clear = weak_session.clone();
                    models.into_iter().fold(
                        menu.entry(
                            "Off · deterministic tools stay available",
                            None,
                            move |_, cx| {
                                if let Err(error) = clear.update(cx, |session, cx| {
                                    session.worker_model = None;
                                    cx.notify();
                                }) {
                                    log::debug!("Infinity session closed: {error}");
                                }
                            },
                        ),
                        |menu, model| {
                            let session = weak_session.clone();
                            menu.entry(
                                format!("{} / {}", model.provider_name().0, model.name().0),
                                None,
                                move |_, cx| {
                                    if let Err(error) = session.update(cx, |session, cx| {
                                        if !session.is_running() {
                                            session.worker_model = Some(model.clone());
                                            cx.notify();
                                        }
                                    }) {
                                        log::debug!("Infinity session closed: {error}");
                                    }
                                },
                            )
                        },
                    )
                }))
            })
    }
}

impl Focusable for InfinityView {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        if self.surface == Surface::Conversation {
            self.composer.focus_handle(cx)
        } else {
            self.navigation_focus.clone()
        }
    }
}

impl Render for InfinityView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let session = self.session.read(cx);
        let running = session.is_running();
        let waiting = session.question.is_some();
        let model = session.main_model(cx).map_or_else(
            || "No main model".to_string(),
            |model| model.name().0.to_string(),
        );
        let mut keys = KeyContext::new_with_defaults(cx.platform_info().os);
        keys.add("Infinity");
        v_flex()
            .id("infinity")
            .key_context(keys)
            .track_focus(&self.navigation_focus)
            .on_action(cx.listener(|this, _: &InfinitySend, window, cx| {
                if this.surface == Surface::Conversation {
                    cx.stop_propagation();
                    this.submit(window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &InfinityStop, _, cx| {
                this.session.update(cx, |session, cx| session.stop(cx));
            }))
            .on_action(cx.listener(|this, _: &InfinityConversation, window, cx| {
                this.select_surface(Surface::Conversation, window, cx)
            }))
            .on_action(cx.listener(|this, _: &InfinityJobs, window, cx| {
                this.select_surface(Surface::Jobs, window, cx)
            }))
            .on_action(cx.listener(|this, _: &InfinityContext, window, cx| {
                this.select_surface(Surface::Context, window, cx)
            }))
            .on_action(
                cx.listener(|this, _: &InfinityToggleToolTraffic, window, cx| {
                    this.context_page = ContextPage::Main;
                    this.tool_traffic_only = !this.tool_traffic_only;
                    this.select_surface(Surface::Context, window, cx);
                }),
            )
            .flex_1()
            .min_h_0()
            .w_full()
            .child(
                h_flex()
                    .gap_1()
                    .px_2()
                    .py_1()
                    .border_b_1()
                    .border_color(cx.theme().colors().border_variant)
                    .child(self.tab(Surface::Conversation, "Conversation", cx))
                    .child(self.tab(Surface::Jobs, format!("Jobs {}", session.jobs.len()), cx))
                    .child(
                        div()
                            .debug_selector(|| "infinity-context-tab".into())
                            .child(self.tab(Surface::Context, "Context", cx)),
                    ),
            )
            .when(!session.approvals.is_empty(), |this| {
                this.child(self.approval(cx))
            })
            .child(match self.surface {
                Surface::Conversation => self.conversation(window, cx).into_any_element(),
                Surface::Jobs => self.jobs(cx).into_any_element(),
                Surface::Context => self.context(cx).into_any_element(),
            })
            .child(
                v_flex()
                    .p_2()
                    .gap_1()
                    .border_t_1()
                    .border_color(cx.theme().colors().border_variant)
                    .when_some(self.error.clone(), |this, error| {
                        this.child(Label::new(error).color(Color::Error))
                    })
                    .when(self.surface == Surface::Conversation, |this| {
                        this.child(
                            div()
                                .p_2()
                                .rounded_md()
                                .bg(cx.theme().colors().editor_background)
                                .child(self.composer.clone()),
                        )
                    })
                    .child(
                        h_flex()
                            .gap_1()
                            .child(
                                Icon::new(IconName::Sparkle)
                                    .size(IconSize::Small)
                                    .color(Color::Muted),
                            )
                            .child(
                                Label::new(model)
                                    .size(LabelSize::Small)
                                    .truncate()
                                    .color(Color::Muted),
                            )
                            .child(div().flex_1())
                            .child(self.worker_picker(cx))
                            .when(
                                running || waiting || self.surface == Surface::Conversation,
                                |this| {
                                    this.child(if running || waiting {
                                        Button::new("stop-infinity", "Stop").on_click(cx.listener(
                                            |this, _, _, cx| {
                                                this.session
                                                    .update(cx, |session, cx| session.stop(cx));
                                            },
                                        ))
                                    } else {
                                        Button::new("send-infinity", "Send")
                                            .style(ButtonStyle::Filled)
                                            .on_click(cx.listener(|this, _, window, cx| {
                                                this.submit(window, cx)
                                            }))
                                    })
                                },
                            )
                            .when(
                                waiting && !running && self.surface == Surface::Conversation,
                                |this| {
                                    this.child(
                                        Button::new("answer-infinity", "Reply")
                                            .style(ButtonStyle::Filled)
                                            .on_click(cx.listener(|this, _, window, cx| {
                                                this.submit(window, cx)
                                            })),
                                    )
                                },
                            ),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;
    use std::path::Path;

    #[gpui::test]
    async fn infinity_composer_sends_with_enter_and_preserves_drafts_while_running(
        cx: &mut TestAppContext,
    ) {
        crate::test_support::init_test(cx);
        let (provider, model) = cx.update(|cx| {
            let keys = settings::KeymapFile::load_asset_allow_partial_failure(
                settings::DEFAULT_KEYMAP_PATH,
                cx,
            )
            .expect("load the bundled default keymap");
            cx.bind_keys(keys);
            let provider = LanguageModelRegistry::test(cx);
            let model = provider.update_model("infinity-main", |model| model.supports_tools = true);
            LanguageModelRegistry::global(cx).update(cx, |registry, cx| {
                registry.set_default_model(Some(model.clone()), cx);
            });
            (provider, model)
        });
        let fs = fs::FakeFs::new(cx.executor());
        let root = Path::new("/project");
        fs.insert_tree(root, serde_json::json!({"README.md": ""}))
            .await;
        let project = Project::test(fs, [root], cx).await;
        let (view, cx) = cx.add_window_view(|window, cx| InfinityView::new(project, window, cx));
        let composer = view.read_with(cx, |view, _| view.composer.clone());
        view.update_in(cx, |view, window, cx| {
            view.focus_handle(cx).focus(window, cx)
        });

        cx.simulate_keystrokes("enter");
        assert!(provider.pending_completions_for(&model).is_empty());
        assert!(view.read_with(cx, |view, _| view.error.is_none()));
        assert!(composer.read_with(cx, |editor, cx| editor.text(cx).is_empty()));

        cx.simulate_input("first line");
        cx.simulate_keystrokes("shift-enter");
        cx.simulate_input("second line");
        assert_eq!(
            composer.read_with(cx, |editor, cx| editor.text(cx)),
            "first line\nsecond line"
        );
        assert!(provider.pending_completions_for(&model).is_empty());

        cx.simulate_keystrokes("enter");
        let requests = provider.pending_completions_for(&model);
        assert_eq!(requests.len(), 1, "Enter should send exactly one request");
        assert!(requests[0].messages.iter().any(|message| {
            message.role == Role::User
                && message
                    .string_contents()
                    .starts_with("first line\nsecond line\n\n")
        }));
        assert!(composer.read_with(cx, |editor, cx| editor.text(cx).is_empty()));

        cx.simulate_input("next draft");
        cx.simulate_keystrokes("enter");
        assert_eq!(provider.pending_completions_for(&model).len(), 1);
        assert_eq!(
            composer.read_with(cx, |editor, cx| editor.text(cx)),
            "next draft"
        );

        provider.send_last_text(&model, "OK");
        provider.end_last(&model);
        cx.run_until_parked();

        cx.simulate_keystrokes("alt-3");
        assert!(view.read_with(cx, |view, _| view.surface == Surface::Context));
        cx.simulate_keystrokes("enter");
        assert!(provider.pending_completions_for(&model).is_empty());
        cx.simulate_keystrokes("alt-1");
        assert!(view.read_with(cx, |view, _| view.surface == Surface::Conversation));
        cx.simulate_keystrokes("enter");
        let requests = provider.pending_completions_for(&model);
        assert_eq!(requests.len(), 1, "Enter should send the preserved draft");
        assert!(requests[0].messages.iter().any(|message| {
            message.role == Role::User && message.string_contents().starts_with("next draft\n\n")
        }));
        assert!(composer.read_with(cx, |editor, cx| editor.text(cx).is_empty()));
    }
}
