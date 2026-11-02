use agent_client_protocol::schema::v2 as acp;

use gpui::{App, SharedString, Task, WeakEntity};
use language_model::LanguageModelToolResultContent;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::{
    AgentTool, ToolCallEventStream, ToolError, ToolErrorCategory, ToolInput, ToolRecovery,
    ToolSideEffect,
};
use agent_client_protocol::schema::v2::ToolKind;
use anyhow::anyhow;

/// Discover the currently available tool capabilities and retrieve detailed
/// protocol information only when it is needed.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct HelpToolInput {
    /// A tool name or capability domain. Omit it to list capability domains.
    pub query: Option<String>,
    /// Run a bounded batch of enabled read/search tools. Calls are executed in order;
    /// terminal, mutation, and nested workflow calls are rejected.
    #[serde(default)]
    pub calls: Option<Vec<HelpToolCall>>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct HelpToolCall {
    /// The exact enabled tool name to invoke.
    pub tool: String,
    /// JSON input passed unchanged to the selected read/search tool.
    #[serde(default)]
    pub input: serde_json::Value,
}

impl From<HelpToolOutput> for LanguageModelToolResultContent {
    fn from(output: HelpToolOutput) -> Self {
        output.text.into()
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct HelpToolOutput {
    text: String,
}

/// Rejections carry the same structured envelope as other tool failures so the
/// model does not have to parse prose to learn the category or recovery path.
fn help_rejection(message: impl std::fmt::Display) -> HelpToolOutput {
    let mut error = ToolError::new(ToolErrorCategory::InvalidInput, message.to_string(), false);
    error.side_effect = Some(ToolSideEffect::None);
    error.recover_with.push(ToolRecovery {
        tool: "help".into(),
        arguments: serde_json::json!({}),
        reason: "List capability domains and choose only local read/search tools".into(),
    });
    HelpToolOutput {
        text: error.to_json_string(),
    }
}

fn domain(name: &str) -> &'static str {
    match name {
        "read_file" | "list_directory" | "find_path" | "grep" | "diagnostics"
        | "find_references" | "get_code_actions" | "go_to_definition" | "rename_symbol"
        | "apply_code_action" => "inspect_and_navigate",
        "edit_file" | "write_file" | "create_directory" | "delete_path" | "move_path"
        | "copy_path" => "change_files",
        "terminal" | "sandboxed_terminal" | "search_web" | "fetch" => "execute_and_fetch",
        "ask_user" | "manage_goal" | "project_checkpoint" | "inspect_tool_result" => {
            "coordinate_and_verify"
        }
        "skill" => "load_on_demand_instructions",
        "spawn_agent" | "create_thread" | "list_agents_and_models" => "delegate_and_coordinate",
        "help" => "discover_capabilities",
        _ => "other",
    }
}

const MAX_BATCH_CALLS: usize = 8;
const MAX_BATCH_OUTPUT_BYTES: usize = 32 * 1024;
const READ_BATCH_TOOLS: &[&str] = &[
    "read_file",
    "grep",
    "find_path",
    "list_directory",
    "find_references",
    "go_to_definition",
    "get_code_actions",
    "diagnostics",
];

fn is_read_batch_tool(name: &str) -> bool {
    READ_BATCH_TOOLS.contains(&name)
}

fn model_output_text(output: &crate::AgentToolOutput) -> String {
    output
        .llm_output
        .iter()
        .map(|part| match part {
            LanguageModelToolResultContent::Text(text) => text.to_string(),
            LanguageModelToolResultContent::Image(_) => "[image output omitted]".to_string(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn append_bounded(output: &mut String, text: &str) -> bool {
    let remaining = MAX_BATCH_OUTPUT_BYTES.saturating_sub(output.len());
    if remaining == 0 {
        return false;
    }
    if text.len() <= remaining {
        output.push_str(text);
        return true;
    }
    let marker = "\n[truncated; remaining calls not executed]\n";
    let mut end = remaining.saturating_sub(marker.len()).min(text.len());
    while !text.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    output.push_str(&text[..end]);
    if marker.len() <= MAX_BATCH_OUTPUT_BYTES.saturating_sub(output.len()) {
        output.push_str(marker);
    }
    false
}

async fn run_read_batch(
    thread: WeakEntity<crate::Thread>,
    calls: Vec<HelpToolCall>,
    event_stream: ToolCallEventStream,
    cx: &mut gpui::AsyncApp,
) -> Result<HelpToolOutput, HelpToolOutput> {
    let mut text = String::new();
    if calls.len() > MAX_BATCH_CALLS {
        return Err(help_rejection(format!(
            "Batch rejected: at most {MAX_BATCH_CALLS} read/search calls are allowed."
        )));
    }
    if calls.is_empty() {
        return Err(help_rejection(
            "Batch rejected: calls must contain at least one call.",
        ));
    }

    if calls.iter().any(|call| !is_read_batch_tool(&call.tool)) {
        return Err(help_rejection(
            "Batch rejected: tool not permitted; use only local read/search tools.",
        ));
    }
    for (index, call) in calls.into_iter().enumerate() {
        if event_stream.was_cancelled_by_user() {
            append_bounded(
                &mut text,
                &format!("### Call {}: `{}` — cancelled\n", index + 1, call.tool),
            );
            break;
        }
        let tool = match thread.read_with(cx, |thread, cx| {
            if !is_read_batch_tool(&call.tool) {
                return Err(anyhow!(
                    "tool `{}` is not permitted in a local read batch",
                    call.tool
                ));
            }
            let tool = thread.verification_tool(&call.tool, cx)?;
            if !matches!(tool.kind(), ToolKind::Read | ToolKind::Search) {
                return Err(anyhow!("only enabled read/search tools are allowed"));
            }
            Ok(tool)
        }) {
            Ok(Ok(tool)) => tool,
            Ok(Err(error)) | Err(error) => {
                let line = format!(
                    "### Call {}: `{}` — failed\n{}\n",
                    index + 1,
                    call.tool,
                    error
                );
                if !append_bounded(&mut text, &line) {
                    break;
                }
                continue;
            }
        };
        // Reuse the parent stream deliberately: tool authorization and sandbox
        // prompts must remain on the owning thread's permission path. The batch
        // itself never emits synthetic tool-call updates.
        let task = cx.update(|cx| {
            tool.run(
                ToolInput::ready(call.input.clone()),
                event_stream.clone(),
                cx,
            )
        });
        let result = task
            .await
            .map(|output| model_output_text(&output))
            .map_err(|output| model_output_text(&output));

        let line = match result {
            Ok(output) => format!(
                "### Call {}: `{}` — succeeded\n{}\n",
                index + 1,
                call.tool,
                output
            ),
            Err(error) => format!(
                "### Call {}: `{}` — failed\n{}\n",
                index + 1,
                call.tool,
                error
            ),
        };
        if !append_bounded(&mut text, &line) {
            break;
        }
        if event_stream.was_cancelled_by_user() {
            append_bounded(&mut text, "[batch cancelled]\n");
            break;
        }
    }
    Ok(HelpToolOutput { text })
}

fn render_help(query: Option<&str>, available_tools: Option<&[String]>) -> String {
    let query = query.map(str::trim).filter(|query| !query.is_empty());
    let tools = crate::BUILT_IN_TOOL_CATALOG
        .iter()
        .filter(|tool| tool.name != "sandboxed_terminal")
        .filter(|tool| {
            available_tools.is_none_or(|available| available.iter().any(|name| name == tool.name))
        })
        .filter(|tool| match query {
            None => true,
            Some(query) if query.eq_ignore_ascii_case("all") => true,
            Some(query) => {
                tool.name.eq_ignore_ascii_case(query)
                    || domain(tool.name).eq_ignore_ascii_case(query)
            }
        })
        .peekable();

    if query.is_none() {
        let mut domains = crate::BUILT_IN_TOOL_CATALOG
            .iter()
            .filter(|tool| {
                available_tools
                    .is_none_or(|available| available.iter().any(|name| name == tool.name))
            })
            .map(|tool| domain(tool.name))
            .filter(|domain| *domain != "other")
            .collect::<Vec<_>>();
        domains.sort_unstable();
        domains.dedup();
        return format!(
            "Capability domains available in this session:\n{}\n\nQuery a domain or tool with `help`, for example `{{\"query\":\"change_files\"}}` or `{{\"query\":\"read_file\"}}`. Tool schemas and permissions are authoritative; availability and approval still depend on the current session.\n\nAfter edits, `help` with `calls` can batch read-only verification, for example `diagnostics` for changed paths plus `read_file` for context. Batching never edits files, never retries, and never grants approval.",
            domains
                .iter()
                .map(|domain| format!("- `{domain}`"))
                .collect::<Vec<_>>()
                .join("\n")
        );
    }

    let Some(query) = query else {
        unreachable!("handled above")
    };

    let matching = tools.collect::<Vec<_>>();
    if !matching.is_empty() {
        return format!(
            "Capability domain `{query}` contains:\n{}\nQuery an exact tool name for its schema.",
            matching
                .iter()
                .map(|tool| format!("- `{}`", tool.name))
                .collect::<Vec<_>>()
                .join("\n")
        );
    }

    format!(
        "No available capability or tool matched `{query}`. Query `help` without a query for the available domains."
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use fs::FakeFs;
    use gpui::{AppContext as _, Entity, TestAppContext};
    use project::Project;
    use serde_json::json;

    use crate::{
        ContextServerRegistry, ListDirectoryTool, ProjectContext, ReadFileTool, Templates, Thread,
    };

    #[test]
    fn lists_domains_without_tool_protocols() {
        let output = render_help(None, None);
        assert!(output.contains("`change_files`"));
        assert!(output.contains("`inspect_and_navigate`"));
        assert!(!output.contains("Schema:"));
        assert!(
            output.contains("After edits, `help` with `calls` can batch read-only verification")
        );
        assert!(is_read_batch_tool("diagnostics"));
    }

    #[test]
    fn returns_exact_schema_for_a_tool_or_domain() {
        let available = vec!["read_file".to_string(), "edit_file".to_string()];
        let tool = render_help(Some("read_file"), Some(&available));
        assert!(tool.contains("Capability domain `read_file` contains"));
        assert!(!tool.contains("Schema:"));

        let domain = render_help(Some("change_files"), Some(&available));
        assert!(domain.contains("`edit_file`"));
        assert!(!domain.contains("`write_file`"));
        assert!(!domain.contains("Schema:"));
    }

    #[test]
    fn unavailable_exact_tool_does_not_leak_fallback() {
        let output = render_help(Some("write_file"), Some(&["read_file".to_string()]));
        assert!(output.contains("No available capability or tool matched"));
        assert!(!output.contains("Schema:"));
    }

    #[test]
    fn unknown_query_is_explicit() {
        let output = render_help(Some("not_a_capability"), None);
        assert!(output.contains("No available capability or tool matched"));
    }

    #[test]
    fn domain_queries_do_not_include_schemas() {
        let output = render_help(Some("inspect_and_navigate"), None);
        assert!(output.contains("`read_file`"));
        assert!(!output.contains("Schema:"));
    }

    #[test]
    fn unavailable_domains_do_not_leak_disabled_tools() {
        let output = render_help(Some("change_files"), Some(&["read_file".to_string()]));
        assert!(output.contains("No available capability or tool matched"));
        assert!(!output.contains("`edit_file`"));
    }

    #[test]
    fn batch_policy_is_local_and_bounded() {
        assert!(is_read_batch_tool("read_file"));
        assert!(is_read_batch_tool("go_to_definition"));
        assert!(!is_read_batch_tool("fetch"));
        assert!(!is_read_batch_tool("web_search"));
        assert!(!is_read_batch_tool("terminal"));
        let mut output = String::new();
        assert!(!append_bounded(
            &mut output,
            &"x".repeat(MAX_BATCH_OUTPUT_BYTES + 1)
        ));
        assert!(output.len() <= MAX_BATCH_OUTPUT_BYTES);
    }

    async fn setup_batch_thread(cx: &mut TestAppContext) -> (Entity<Thread>, Arc<FakeFs>) {
        crate::tests::init_test(cx);
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(
            "/root",
            json!({"present.txt": "batch content", "nested": {"child.txt": "child"}}),
        )
        .await;
        let project = Project::test(fs.clone(), [std::path::Path::new("/root")], cx).await;
        let context_server_store =
            project.read_with(cx, |project, _| project.context_server_store());
        let context_servers = cx.new(|cx| ContextServerRegistry::new(context_server_store, cx));
        let model = cx.update(|cx| language_model::LanguageModelRegistry::test(cx).model("fake"));
        let thread = cx.new(|cx| {
            Thread::new(
                project.clone(),
                cx.new(|_| ProjectContext::default()),
                context_servers,
                Templates::new(),
                Some(model),
                cx,
            )
        });
        thread.update(cx, |thread, cx| {
            let action_log = thread.action_log().clone();
            thread.add_tool(ReadFileTool::new(thread.project.clone(), action_log, true));
            thread.add_tool(ListDirectoryTool::new(thread.project.clone()));
            thread.add_tool(HelpTool::new(cx.weak_entity()));
        });
        (thread, fs)
    }

    #[gpui::test]
    async fn help_does_not_read_thread_during_tool_execution(cx: &mut TestAppContext) {
        let (thread, _) = setup_batch_thread(cx).await;
        let help = Arc::new(HelpTool::new(thread.downgrade()));
        // Tools execute while the owning Thread is leased by `update_entity`.
        // Reading it synchronously inside `run` panics in real sessions.
        let task = thread.update(cx, |_thread, cx| {
            help.run(
                ToolInput::resolved(HelpToolInput {
                    query: None,
                    calls: None,
                }),
                ToolCallEventStream::test().0,
                cx,
            )
        });
        let output = task.await.expect("help output").text;
        assert!(output.contains("Capability domains"));
    }

    #[gpui::test]
    async fn exact_help_uses_enabled_instance_and_batch_respects_profile(cx: &mut TestAppContext) {
        let (thread, _) = setup_batch_thread(cx).await;
        let help = Arc::new(HelpTool::new(thread.downgrade()));
        let output = cx
            .update(|cx| {
                help.clone().run(
                    ToolInput::resolved(HelpToolInput {
                        query: Some("read_file".into()),
                        calls: None,
                    }),
                    ToolCallEventStream::test().0,
                    cx,
                )
            })
            .await
            .expect("exact help")
            .text;
        assert!(output.contains("## `read_file`\n"));
        assert!(output.contains("Schema:"));
        assert!(output.contains("start_line"));
        thread.update(cx, |thread, _| {
            thread.remove_tool("read_file");
        });
        let output = cx
            .update(|cx| {
                help.run(
                    ToolInput::resolved(HelpToolInput {
                        query: None,
                        calls: Some(vec![HelpToolCall {
                            tool: "read_file".into(),
                            input: json!({"path":"root/present.txt"}),
                        }]),
                    }),
                    ToolCallEventStream::test().0,
                    cx,
                )
            })
            .await
            .expect("per-call rejection")
            .text;
        assert!(output.contains("not enabled"));
        assert!(!output.contains("batch content"));
    }

    #[gpui::test]
    async fn batch_executes_read_and_list_and_continues_after_failure(cx: &mut TestAppContext) {
        let (thread, _) = setup_batch_thread(cx).await;
        let help = Arc::new(HelpTool::new(thread.downgrade()));
        let calls = vec![
            HelpToolCall {
                tool: "read_file".into(),
                input: json!({"path": "root/missing.txt"}),
            },
            HelpToolCall {
                tool: "list_directory".into(),
                input: json!({"path": "root"}),
            },
            HelpToolCall {
                tool: "read_file".into(),
                input: json!({"path": "root/present.txt"}),
            },
        ];
        let task = cx.update(|cx| {
            help.run(
                ToolInput::resolved(HelpToolInput {
                    query: None,
                    calls: Some(calls),
                }),
                ToolCallEventStream::test().0,
                cx,
            )
        });
        let output = task
            .await
            .expect("batch should return per-call results")
            .text;
        assert!(output.contains("Call 1"));
        assert!(output.contains("failed"));
        assert!(output.contains("Call 2"));
        assert!(output.contains("present.txt"));
        assert!(output.contains("Call 3"));
        assert!(output.contains("batch content"));
    }

    #[gpui::test]
    async fn batch_rejects_empty_mixed_over_limit_and_non_read_calls(cx: &mut TestAppContext) {
        let (thread, _) = setup_batch_thread(cx).await;
        let help = Arc::new(HelpTool::new(thread.downgrade()));
        for input in [
            HelpToolInput {
                query: None,
                calls: Some(Vec::new()),
            },
            HelpToolInput {
                query: Some("read_file".into()),
                calls: Some(vec![]),
            },
            HelpToolInput {
                query: None,
                calls: Some(
                    (0..=MAX_BATCH_CALLS)
                        .map(|_| HelpToolCall {
                            tool: "read_file".into(),
                            input: json!({"path": "root/present.txt"}),
                        })
                        .collect(),
                ),
            },
            HelpToolInput {
                query: None,
                calls: Some(vec![HelpToolCall {
                    tool: "edit_file".into(),
                    input: json!({}),
                }]),
            },
            HelpToolInput {
                query: None,
                calls: Some(vec![HelpToolCall {
                    tool: "terminal".into(),
                    input: json!({}),
                }]),
            },
        ] {
            let task = cx.update(|cx| {
                help.clone().run(
                    ToolInput::resolved(input),
                    ToolCallEventStream::test().0,
                    cx,
                )
            });
            let output = task.await.expect_err("invalid batch should fail").text;
            assert!(
                output.contains("rejected")
                    || output.contains("mutually exclusive")
                    || output.contains("not permitted")
            );
            assert!(
                output.contains("\"category\":\"invalid_input\""),
                "batch rejection must stay structured: {output}"
            );
        }
    }

    #[gpui::test]
    async fn batch_output_is_bounded_and_cancellation_stops_execution(cx: &mut TestAppContext) {
        let (thread, fs) = setup_batch_thread(cx).await;
        fs.insert_file(
            "/root/large.txt",
            "large output\n".repeat(MAX_BATCH_OUTPUT_BYTES).into_bytes(),
        )
        .await;
        let help = Arc::new(HelpTool::new(thread.downgrade()));
        let calls = vec![HelpToolCall {
            tool: "read_file".into(),
            input: json!({"path": "root/large.txt"}),
        }];
        let (event_stream, _receiver, mut cancellation) =
            ToolCallEventStream::test_with_cancellation();
        ToolCallEventStream::signal_cancellation_with_sender(&mut cancellation);
        let task = cx.update(|cx| {
            help.run(
                ToolInput::resolved(HelpToolInput {
                    query: None,
                    calls: Some(calls),
                }),
                event_stream,
                cx,
            )
        });
        let output = task
            .await
            .expect("cancellation remains a batch result")
            .text;
        assert!(output.len() <= MAX_BATCH_OUTPUT_BYTES);
    }
}

pub struct HelpTool {
    thread: WeakEntity<crate::Thread>,
}

impl HelpTool {
    pub fn new(thread: WeakEntity<crate::Thread>) -> Self {
        Self { thread }
    }
}

impl AgentTool for HelpTool {
    type Input = HelpToolInput;
    type Output = HelpToolOutput;

    const NAME: &'static str = "help";

    fn kind() -> acp::ToolKind {
        acp::ToolKind::Other
    }

    fn initial_title(
        &self,
        _input: Result<Self::Input, serde_json::Value>,
        _cx: &mut App,
    ) -> SharedString {
        "Discover capabilities".into()
    }

    fn run(
        self: Arc<Self>,
        input: ToolInput<Self::Input>,
        event_stream: ToolCallEventStream,
        cx: &mut App,
    ) -> Task<Result<Self::Output, Self::Output>> {
        let thread = self.thread.clone();
        cx.spawn(async move |_cx| match input.recv().await {
            Ok(input) => {
                // Tools run while the owning Thread is leased by `update_entity`,
                // so every thread read must happen here, after execution returns
                // to the async context, never synchronously inside `run`.
                let available_tools = self
                    .thread
                    .read_with(_cx, |thread, cx| {
                        thread
                            .enabled_tools(cx)
                            .keys()
                            .map(|name| name.to_string())
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                if input.calls.is_some() && input.query.is_some() {
                    return Err(help_rejection(
                        "Invalid help input: query and calls are mutually exclusive.",
                    ));
                }
                if let Some(calls) = input.calls {
                    return run_read_batch(thread, calls, event_stream, _cx).await;
                }
                let query = input.query.as_deref();
                if let Some(query) = query {
                    if let Some((name, description, schema)) = self
                        .thread
                        .read_with(_cx, |thread, cx| {
                            thread
                                .enabled_tools(cx)
                                .into_iter()
                                .find_map(|(name, tool)| {
                                    (name.eq_ignore_ascii_case(query)).then(|| {
                                        (
                                            name.to_string(),
                                            tool.description().to_string(),
                                            tool.input_schema(),
                                        )
                                    })
                                })
                        })
                        .ok()
                        .flatten()
                    {
                        return Ok(HelpToolOutput {
                            text: format!(
                                "## `{name}`\nDomain: `{}`\n{}\nSchema:\n```json\n{}\n```",
                                domain(&name),
                                description,
                                serde_json::to_string_pretty(&schema).unwrap_or_else(
                                    |error| format!("{{\\\"error\\\":\\\"{error}\\\"}}")
                                )
                            ),
                        });
                    }
                }
                if query.is_some_and(|query| query.eq_ignore_ascii_case("terminal")) {
                    if let Some(schema) = self
                        .thread
                        .read_with(_cx, |thread, cx| {
                            thread
                                .enabled_tools(cx)
                                .remove("terminal")
                                .map(|tool| tool.input_schema())
                        })
                        .ok()
                        .flatten()
                    {
                        return Ok(HelpToolOutput {
                            text: format!(
                                "Active `terminal` schema:\n```json\n{}\n```",
                                serde_json::to_string_pretty(&schema)
                                    .unwrap_or_else(|error| format!("{{\"error\":\"{error}\"}}"))
                            ),
                        });
                    }
                }
                Ok(HelpToolOutput {
                    text: render_help(query, Some(&available_tools)),
                })
            }
            Err(error) => Err(help_rejection(format!("Invalid help input: {error}"))),
        })
    }
}
