use std::sync::Arc;

use agent_client_protocol::schema::v2 as acp;
use gpui::{App, AppContext as _, SharedString, Task, WeakEntity};
use language_model::LanguageModelToolResultContent;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{AgentTool, Thread, ToolCallEventStream, ToolInput};

/// Retrieve retained tool observations without rerunning tools. Omit tool_call_id
/// to list completed calls newest first. Use their exact scoped IDs to retrieve
/// input and output or cite goal evidence. A successful call proves execution,
/// not that the user's requirement is satisfied. Check relevance and freshness.
/// To inspect evidence from a previous session, provide checkpoint_source and
/// checkpoint_scope; the referenced checkpoint and saved session must belong to
/// the current project. Evidence can become unavailable if its session is removed.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct InspectToolResultInput {
    #[serde(default)]
    pub tool_call_id: Option<String>,
    /// A checkpoint evidence source, in the form returned by project_checkpoint.
    #[serde(default)]
    pub checkpoint_source: Option<String>,
    /// The scope containing that evidence.
    #[serde(default)]
    pub checkpoint_scope: Option<String>,
    /// Absolute project root when the workspace has multiple roots.
    #[serde(default)]
    pub checkpoint_root: Option<std::path::PathBuf>,
    /// List offset, or Unicode character offset within a selected result.
    #[serde(default)]
    pub offset: usize,
    /// Page size: up to 50 calls or 16000 characters. Defaults to 20 calls or 4000 characters.
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct InspectToolResultOutput {
    pub entries: Vec<ToolResultSummary>,
    pub text: Option<String>,
    pub next_offset: Option<usize>,
    pub error: Option<String>,
    pub source_integrity: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ToolResultSummary {
    pub tool_call_id: String,
    pub tool_name: String,
    pub is_error: bool,
}

impl From<InspectToolResultOutput> for LanguageModelToolResultContent {
    fn from(output: InspectToolResultOutput) -> Self {
        serde_json::to_string(&output)
            .unwrap_or_else(|error| format!("Failed to serialize tool observations: {error}"))
            .into()
    }
}

pub struct InspectToolResultTool {
    thread: WeakEntity<Thread>,
}

impl InspectToolResultTool {
    pub fn new(thread: WeakEntity<Thread>) -> Self {
        Self { thread }
    }
}

impl AgentTool for InspectToolResultTool {
    type Input = InspectToolResultInput;
    type Output = InspectToolResultOutput;
    const NAME: &'static str = "inspect_tool_result";

    fn kind() -> acp::ToolKind {
        acp::ToolKind::Read
    }

    fn initial_title(
        &self,
        _: Result<Self::Input, serde_json::Value>,
        _: &mut App,
    ) -> SharedString {
        "Inspect retained tool results".into()
    }

    fn run(
        self: Arc<Self>,
        input: ToolInput<Self::Input>,
        _: ToolCallEventStream,
        cx: &mut App,
    ) -> Task<Result<Self::Output, Self::Output>> {
        cx.spawn(async move |cx| {
            let result = async {
                let input = input.recv().await?;
                if input.checkpoint_source.is_some() {
                    anyhow::ensure!(
                        input.tool_call_id.is_none(),
                        "Use either checkpoint_source or tool_call_id"
                    );
                    return cx
                        .update(|cx| inspect_checkpoint_evidence(self.thread.clone(), input, cx))
                        .await;
                }
                self.thread
                    .read_with(cx, |thread, _| -> anyhow::Result<_> {
                        let summary = |id, result: &language_model::LanguageModelToolResult| {
                            ToolResultSummary {
                                tool_call_id: id,
                                tool_name: result.tool_name.to_string(),
                                is_error: result.is_error,
                            }
                        };
                        if let Some(id) = &input.tool_call_id {
                            let (_, result, tool_use) = thread
                                .retained_tool_results()
                                .find(|(source_id, _, _)| source_id == id)
                                .ok_or_else(|| {
                                    anyhow::anyhow!(
                                        "Tool result {id} is unavailable in retained history"
                                    )
                                })?;
                            let text = format!(
                                "Input:\n{}\nObservations:\n{}\nRaw output:\n{}",
                                tool_use.raw_input,
                                result.text_contents(),
                                result
                                    .output
                                    .as_ref()
                                    .map(ToString::to_string)
                                    .unwrap_or_default()
                            );
                            let (text, next_offset) = result_page(
                                &text,
                                input.offset,
                                input.limit.unwrap_or(4000).clamp(1, 16000),
                            )?;
                            Ok(InspectToolResultOutput {
                                entries: vec![summary(id.clone(), result)],
                                text: Some(text),
                                next_offset,
                                error: None,
                                source_integrity: None,
                            })
                        } else {
                            let results = thread
                                .retained_tool_results()
                                .filter(|(_, result, _)| result.tool_name.as_ref() != Self::NAME)
                                .collect::<Vec<_>>();
                            anyhow::ensure!(
                                input.offset <= results.len(),
                                "Offset exceeds retained result count"
                            );
                            let limit = input.limit.unwrap_or(20).clamp(1, 50);
                            let entries = results
                                .iter()
                                .rev()
                                .skip(input.offset)
                                .take(limit)
                                .map(|(id, result, _)| summary(id.clone(), result))
                                .collect::<Vec<_>>();
                            let end = input.offset.saturating_add(entries.len());
                            Ok(InspectToolResultOutput {
                                entries,
                                text: None,
                                next_offset: (end < results.len()).then_some(end),
                                error: None,
                                source_integrity: None,
                            })
                        }
                    })?
            }
            .await;
            result.map_err(|error: anyhow::Error| InspectToolResultOutput {
                entries: Vec::new(),
                text: None,
                next_offset: None,
                error: Some(error.to_string()),
                source_integrity: None,
            })
        })
    }
}

pub fn inspect_checkpoint_evidence(
    thread: WeakEntity<Thread>,
    input: InspectToolResultInput,
    cx: &mut App,
) -> Task<anyhow::Result<InspectToolResultOutput>> {
    cx.spawn(async move |cx| {
        let source = input
            .checkpoint_source
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("checkpoint_source is required"))?;
        anyhow::ensure!(
            input.tool_call_id.is_none(),
            "Use either checkpoint_source or tool_call_id"
        );
        let scope = input
            .checkpoint_scope
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("checkpoint_scope is required"))?
            .to_string();
        let (session_id, tool_id) = source
            .strip_prefix("session:")
            .and_then(|source| source.split_once("/tool:"))
            .ok_or_else(|| anyhow::anyhow!("Invalid checkpoint evidence source"))?;
        anyhow::ensure!(
            !session_id.is_empty() && !tool_id.is_empty(),
            "Invalid checkpoint evidence source"
        );
        let session_id = acp::SessionId::new(session_id.to_string());
        let tool_id = tool_id.to_string();
        let source = source.clone();
        let root = thread.read_with(cx, |thread, cx| -> anyhow::Result<_> {
            let project = thread.project().read(cx);
            anyhow::ensure!(
                project.is_local()
                    && !project::trusted_worktrees::TrustedWorktrees::has_restricted_worktrees(
                        &project.worktree_store(),
                        cx
                    ),
                "Checkpoint evidence requires a trusted local project"
            );
            let roots = project
                .visible_worktrees(cx)
                .map(|worktree| worktree.read(cx).abs_path().to_path_buf())
                .collect::<Vec<_>>();
            if let Some(root) = &input.checkpoint_root {
                anyhow::ensure!(
                    roots.contains(root),
                    "Checkpoint root must be an open project root"
                );
                Ok(root.clone())
            } else {
                anyhow::ensure!(
                    roots.len() == 1,
                    "Specify a checkpoint root for a multi-root workspace"
                );
                roots
                    .into_iter()
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("No project root"))
            }
        })??;
        let root_to_check = root.clone();
        let session_to_check = session_id.clone();
        let allowed = cx
            .background_spawn(async move {
                let checkpoint = crate::ProjectCheckpointStore::open_existing(&root_to_check)?
                    .ok_or_else(|| anyhow::anyhow!("No checkpoint database"))?
                    .latest(&scope)?
                    .ok_or_else(|| anyhow::anyhow!("Checkpoint not found"))?;
                anyhow::ensure!(
                    checkpoint.session_id == session_to_check.0.as_ref(),
                    "Evidence is not referenced by this checkpoint"
                );
                let evidence = checkpoint
                    .evidence
                    .iter()
                    .find(|item| item.source == source)
                    .ok_or_else(|| {
                        anyhow::anyhow!("Evidence is not referenced by this checkpoint")
                    })?;
                anyhow::Ok(evidence.digest.clone())
            })
            .await;
        let expected_digest = allowed?;
        let database = cx
            .update(crate::ThreadsDatabase::connect)
            .await
            .map_err(|error| anyhow::anyhow!("{error:#}"))?;
        let threads = database.list_threads().await?;
        let metadata = threads
            .iter()
            .find(|thread| thread.id == session_id)
            .ok_or_else(|| anyhow::anyhow!("The source session is unavailable"))?;
        anyhow::ensure!(
            metadata.folder_paths.paths().contains(&root),
            "The source session does not belong to this project"
        );
        let saved = database
            .load_thread(session_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("The source session is unavailable"))?;
        let (result, tool_use) = saved
            .messages
            .iter()
            .enumerate()
            .find_map(|(index, message)| {
                let agent = message.as_agent_message()?;
                agent.content.iter().find_map(|content| {
                    let crate::AgentMessageContent::ToolUse(tool_use) = content else {
                        return None;
                    };
                    if crate::thread::scoped_tool_call_id(index, &tool_use.id)
                        .0
                        .as_ref()
                        != tool_id
                    {
                        return None;
                    }
                    Some((agent.tool_results.get(&tool_use.id)?, tool_use))
                })
            })
            .ok_or_else(|| {
                anyhow::anyhow!("The source result is no longer retained in the session")
            })?;
        let integrity = if let Some(expected_digest) = expected_digest {
            anyhow::ensure!(
                crate::checkpoint_evidence_digest(tool_use, result)? == expected_digest,
                "The source result changed since the checkpoint"
            );
            "verified"
        } else {
            "unverified: checkpoint has no source digest"
        };
        let text = format!(
            "Input:\n{}\nObservations:\n{}\nRaw output:\n{}",
            tool_use.raw_input,
            result.text_contents(),
            result
                .output
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_default()
        );
        let (text, next_offset) = result_page(
            &text,
            input.offset,
            input.limit.unwrap_or(4000).clamp(1, 16000),
        )?;
        Ok(InspectToolResultOutput {
            entries: vec![ToolResultSummary {
                tool_call_id: tool_id,
                tool_name: result.tool_name.to_string(),
                is_error: result.is_error,
            }],
            text: Some(text),
            next_offset,
            error: None,
            source_integrity: Some(integrity.into()),
        })
    })
}

fn result_page(text: &str, offset: usize, limit: usize) -> anyhow::Result<(String, Option<usize>)> {
    let count = text.chars().count();
    anyhow::ensure!(offset <= count, "Offset exceeds result character count");
    let page = text.chars().skip(offset).take(limit).collect::<String>();
    let end = offset.saturating_add(page.chars().count());
    Ok((page, (end < count).then_some(end)))
}

#[cfg(test)]
mod tests {
    #[test]
    fn result_pages_preserve_unicode_and_report_exhaustion() {
        let text = "验证 🦀 output";
        let (first, next) = super::result_page(text, 0, 4).expect("first page");
        let (second, next) = super::result_page(text, next.expect("more"), 100).expect("last page");
        assert_eq!(first + &second, text);
        assert_eq!(next, None);
        assert!(super::result_page(text, usize::MAX, 10).is_err());
    }
}
