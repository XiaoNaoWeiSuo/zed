use std::sync::Arc;

use agent_client_protocol::schema::v2 as acp;
use gpui::{App, SharedString, Task, WeakEntity};
use language_model::LanguageModelToolResultContent;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{AgentTool, Thread, ToolCallEventStream, ToolInput};

/// Retrieve retained tool observations without rerunning tools. Omit tool_call_id
/// to list completed calls newest first. Use their exact scoped IDs to retrieve
/// input and output or cite goal evidence. A successful call proves execution,
/// not that the user's requirement is satisfied. Check relevance and freshness.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct InspectToolResultInput {
    #[serde(default)]
    pub tool_call_id: Option<String>,
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
            })
        })
    }
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
