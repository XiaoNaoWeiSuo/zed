use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;
use std::sync::Arc;

use crate::{AgentTool, ToolCallEventStream, ToolInput};
use agent_client_protocol::schema::v2 as acp_v2;
use anyhow::Result;
use futures::{FutureExt, channel::oneshot, future::Shared};
use gpui::{App, SharedString, Task};
use language_model::LanguageModelToolResultContent;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The single-select field key used when presenting a fixed list of options.
const CHOICE_FIELD: &str = "choice";
/// The free-text field key used when the user may type their own answer.
const OTHER_FIELD: &str = "other";

/// Collect missing user decisions in a form. Use questions for a batch mixing
/// single-select, multi-select, and text fields; the legacy fields ask one question.
/// Group related decisions and avoid asking for information already in context.
/// Set defer when independent work can continue. Retrieve the returned request_id
/// later; wait=true waits, while false checks once. Native sessions also deliver
/// replies at the next safe boundary and can resume an idle session. Withdraw
/// obsolete requests using request_id and withdraw=true. Avoid repeated polling.
/// Never guess a pending answer or proceed with work that depends on it.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct AskUserToolInput {
    /// The question to ask the user. Keep it short and specific.
    #[serde(default)]
    pub question: String,
    /// The answer choices to present as selectable options. Each entry is the
    /// exact label shown, and is returned verbatim when chosen. Provide at
    /// least two options, or leave empty and set `allow_free_text` to `true`.
    #[serde(default)]
    pub options: Vec<String>,
    /// Whether the user may type their own free-form answer instead of (or in
    /// addition to) picking an option. Set to `true` when the options may not
    /// be exhaustive, or when you want a free-form reply.
    #[serde(default)]
    pub allow_free_text: bool,
    /// Optional questions to present together. When provided, the legacy question fields are ignored.
    #[serde(default)]
    pub questions: Vec<AskUserQuestion>,
    /// Start a question without waiting for its answer.
    #[serde(default)]
    pub defer: bool,
    /// Retrieve an earlier deferred question instead of starting another.
    #[serde(default)]
    pub request_id: Option<String>,
    /// Wait for the deferred answer; otherwise return its current status.
    #[serde(default)]
    pub wait: bool,
    /// Withdraw a deferred request that is no longer needed. Requires request_id.
    #[serde(default)]
    pub withdraw: bool,
}

/// One question in a form containing multiple decisions.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct AskUserQuestion {
    /// The question to display.
    pub question: String,
    /// Selectable answers, if applicable.
    #[serde(default)]
    pub options: Vec<String>,
    /// Allow more than one option to be selected.
    #[serde(default)]
    pub multiple: bool,
    /// Allow a free-text answer instead of, or in addition to, the choices.
    #[serde(default)]
    pub allow_free_text: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AskUserToolOutput {
    Answered {
        selected: String,
    },
    Answers {
        answers: BTreeMap<String, serde_json::Value>,
    },
    Pending {
        request_id: String,
    },
    Withdrawn {
        request_id: String,
    },
    Error {
        error: String,
    },
}

impl From<AskUserToolOutput> for LanguageModelToolResultContent {
    fn from(value: AskUserToolOutput) -> Self {
        match value {
            AskUserToolOutput::Answered { selected } => {
                format!("The user selected: {selected}").into()
            }
            AskUserToolOutput::Answers { answers } => format!(
                "The user's answers: {}",
                serde_json::Value::from_iter(answers)
            )
            .into(),
            AskUserToolOutput::Pending { request_id } => format!("Question {request_id} is pending. Continue independent work. Native sessions deliver replies automatically; use ask_user with this request_id and wait=true when dependent work needs the answer. Withdraw it if no longer applicable.").into(),
            AskUserToolOutput::Withdrawn { request_id } => format!("Question {request_id} was withdrawn.").into(),
            AskUserToolOutput::Error { error } => error.into(),
        }
    }
}

type QuestionTask = Shared<Task<Result<AskUserToolOutput, AskUserToolOutput>>>;

#[derive(Default)]
pub struct AskUserTool {
    questions: Rc<RefCell<HashMap<String, QuestionTask>>>,
}

impl AgentTool for AskUserTool {
    type Input = AskUserToolInput;
    type Output = AskUserToolOutput;

    const NAME: &'static str = "ask_user";

    fn kind() -> acp_v2::ToolKind {
        acp_v2::ToolKind::Other
    }

    fn initial_title(
        &self,
        input: Result<Self::Input, serde_json::Value>,
        _cx: &mut App,
    ) -> SharedString {
        match input {
            Ok(input) if !input.question.is_empty() => SharedString::from(input.question),
            Ok(input) if input.questions.len() == 1 => input
                .questions
                .first()
                .map(|question| question.question.clone().into())
                .unwrap_or_else(|| "Asking a question".into()),
            _ => "Asking a question".into(),
        }
    }

    fn run(
        self: Arc<Self>,
        input: ToolInput<Self::Input>,
        event_stream: ToolCallEventStream,
        cx: &mut App,
    ) -> Task<Result<Self::Output, Self::Output>> {
        cx.spawn(async move |cx| {
            let input = input
                .recv()
                .await
                .map_err(|error| AskUserToolOutput::Error {
                    error: error.to_string(),
                })?;

            if let Some(request_id) = &input.request_id {
                if let Some(answer) = cx.update(|cx| event_stream.deferred_answer(request_id.clone(), input.wait, input.withdraw, cx)) {
                    return answer.await;
                }
                if input.withdraw { return Err(AskUserToolOutput::Error { error: "Only a retained session question can be withdrawn.".into() }); }
                let task = self
                    .questions
                    .borrow()
                    .get(request_id)
                    .cloned()
                    .ok_or_else(|| AskUserToolOutput::Error {
                        error: format!("Question {request_id} is unavailable in this session. It may have been lost when the session was reloaded; ask the user again if an answer is still needed."),
                    })?;
                if input.wait {
                    return task.await;
                }
                return task.now_or_never().unwrap_or_else(|| {
                    Ok(AskUserToolOutput::Pending {
                        request_id: request_id.clone(),
                    })
                });
            }
            if input.defer {
                let request_id = uuid::Uuid::new_v4().to_string();
                let event_stream = event_stream.with_deferred_question(request_id.clone());
                let (started_tx, started_rx) = oneshot::channel();
                let task = cx
                    .spawn(async move |cx| {
                        execute_question(input, event_stream, Some(started_tx), cx).await
                    })
                    .shared();
                if started_rx.await.is_err() {
                    return task.await;
                }
                self.questions.borrow_mut().insert(request_id.clone(), task);
                return Ok(AskUserToolOutput::Pending { request_id });
            }
            execute_question(input, event_stream, None, cx).await
        })
    }
}

async fn execute_question(
    input: AskUserToolInput,
    event_stream: ToolCallEventStream,
    started: Option<oneshot::Sender<()>>,
    cx: &mut gpui::AsyncApp,
) -> Result<AskUserToolOutput, AskUserToolOutput> {
    if !input.questions.is_empty() {
        return ask_multiple_questions(input.questions, event_stream, started, cx).await;
    }
    if input.question.trim().is_empty() {
        return Err(AskUserToolOutput::Error {
            error: "A question is required.".into(),
        });
    }
    if !input.allow_free_text && input.options.len() < 2 {
        return Err(AskUserToolOutput::Error {
            error: "The `ask_user` tool needs at least two `options`, or \
                            `allow_free_text` set to true. Ask an open-ended question \
                            in prose instead."
                .to_string(),
        });
    }

    let schema = build_schema(&input.options, input.allow_free_text);

    let prompt =
        cx.update(|cx| event_stream.request_elicitation(input.question.clone(), schema, cx));
    if let Some(started) = started {
        if started.send(()).is_err() {
            return Err(AskUserToolOutput::Error {
                error: "Question request was cancelled before it started.".into(),
            });
        }
    }
    let response = prompt.await.map_err(|error| AskUserToolOutput::Error {
        error: error.to_string(),
    })?;

    let selected = match response.action {
        acp_v2::ElicitationAction::Accept(accept) => {
            let content = accept.content.unwrap_or_default();
            // A typed answer takes precedence over a picked option.
            string_field(&content, OTHER_FIELD)
                .or_else(|| string_field(&content, CHOICE_FIELD))
                .ok_or_else(|| AskUserToolOutput::Error {
                    error: "The user submitted the form without providing an answer.".to_string(),
                })?
        }
        acp_v2::ElicitationAction::Decline => {
            return Err(AskUserToolOutput::Error {
                error: "The user declined to answer the question.".to_string(),
            });
        }
        acp_v2::ElicitationAction::Cancel => {
            return Err(AskUserToolOutput::Error {
                error: "The user cancelled the question without answering.".to_string(),
            });
        }
        _ => {
            return Err(AskUserToolOutput::Error {
                error: "The question was dismissed without an answer.".to_string(),
            });
        }
    };

    event_stream.update_fields(|update| update.title(format!("Answered: {selected}")));

    Ok(AskUserToolOutput::Answered { selected })
}

async fn ask_multiple_questions(
    questions: Vec<AskUserQuestion>,
    event_stream: ToolCallEventStream,
    started: Option<oneshot::Sender<()>>,
    cx: &mut gpui::AsyncApp,
) -> Result<AskUserToolOutput, AskUserToolOutput> {
    if questions.len() > 10
        || questions.iter().any(|question| {
            question.question.trim().is_empty()
                || (!question.allow_free_text && question.options.len() < 2)
                || (question.multiple && question.options.is_empty())
        })
    {
        return Err(AskUserToolOutput::Error {
            error: "Provide 1 to 10 non-empty questions, each with at least two options or free text; multi-select requires options.".into(),
        });
    }

    let mut schema = acp_v2::ElicitationSchema::new();
    for (index, question) in questions.iter().enumerate() {
        let key = format!("q{index:02}_choice");
        if !question.options.is_empty() {
            if question.multiple {
                schema = schema.property(
                    key,
                    acp_v2::MultiSelectPropertySchema::new(question.options.clone())
                        .title(question.question.clone()),
                    !question.allow_free_text,
                );
            } else {
                let options: Vec<_> = question
                    .options
                    .iter()
                    .map(|option| acp_v2::EnumOption::new(option.clone(), option.clone()))
                    .collect();
                schema = schema.property(
                    key,
                    acp_v2::StringPropertySchema::new()
                        .title(question.question.clone())
                        .one_of(options),
                    !question.allow_free_text,
                );
            }
        }
        if question.allow_free_text {
            let title = if question.options.is_empty() {
                question.question.clone()
            } else {
                format!("{} — other", question.question)
            };
            schema = schema.property(
                format!("q{index:02}_text"),
                acp_v2::StringPropertySchema::new().title(title),
                question.options.is_empty(),
            );
        }
    }
    let prompt = cx.update(|cx| {
        event_stream.request_elicitation("Please answer the following questions".into(), schema, cx)
    });
    if let Some(started) = started {
        if started.send(()).is_err() {
            return Err(AskUserToolOutput::Error {
                error: "Question request was cancelled before it started.".into(),
            });
        }
    }
    let response = prompt.await.map_err(|error| AskUserToolOutput::Error {
        error: error.to_string(),
    })?;
    let content = match response.action {
        acp_v2::ElicitationAction::Accept(accept) => accept.content.unwrap_or_default(),
        acp_v2::ElicitationAction::Decline => {
            return Err(AskUserToolOutput::Error {
                error: "The user declined to answer the questions.".into(),
            });
        }
        _ => {
            return Err(AskUserToolOutput::Error {
                error: "The questions were cancelled without an answer.".into(),
            });
        }
    };
    let mut answers = BTreeMap::new();
    for (index, question) in questions.iter().enumerate() {
        let choice = content.get(&format!("q{index:02}_choice"));
        let text = string_field(&content, &format!("q{index:02}_text"));
        let selected = match choice {
            Some(acp_v2::ElicitationContentValue::String(value))
                if question.options.contains(value) =>
            {
                vec![value.clone()]
            }
            Some(acp_v2::ElicitationContentValue::StringArray(values))
                if question.multiple
                    && values.iter().all(|value| question.options.contains(value)) =>
            {
                values.clone()
            }
            _ => Vec::new(),
        };
        if selected.is_empty() && text.is_none() {
            return Err(AskUserToolOutput::Error {
                error: format!("No answer was provided for question {}.", index + 1),
            });
        }
        answers.insert(
            format!("{}: {}", index + 1, question.question),
            serde_json::json!({"selected": selected, "text": text}),
        );
    }
    event_stream.update_fields(|update| update.title("Questions answered"));
    Ok(AskUserToolOutput::Answers { answers })
}

/// Builds the elicitation form schema for a question.
///
/// - Presenting a fixed list of options adds a required single-select field,
///   unless free text is also allowed (in which case picking is optional).
/// - Allowing free text adds a text field, required only when there are no
///   options to choose from.
fn build_schema(options: &[String], allow_free_text: bool) -> acp_v2::ElicitationSchema {
    let mut schema = acp_v2::ElicitationSchema::new();

    if !options.is_empty() {
        let enum_options = options
            .iter()
            .map(|label| acp_v2::EnumOption::new(label.clone(), label.clone()))
            .collect::<Vec<_>>();
        schema = schema.property(
            CHOICE_FIELD,
            acp_v2::StringPropertySchema::new()
                .title("Choose an option")
                .one_of(enum_options),
            !allow_free_text,
        );
    }

    if allow_free_text {
        let title = if options.is_empty() {
            "Your answer"
        } else {
            "Or type your own answer"
        };
        schema = schema.property(
            OTHER_FIELD,
            acp_v2::StringPropertySchema::new().title(title),
            options.is_empty(),
        );
    }

    schema
}

/// Extracts a non-empty string value for `key` from an elicitation response.
fn string_field(
    content: &BTreeMap<String, acp_v2::ElicitationContentValue>,
    key: &str,
) -> Option<String> {
    match content.get(key) {
        Some(acp_v2::ElicitationContentValue::String(value)) if !value.is_empty() => {
            Some(value.clone())
        }
        _ => None,
    }
}

#[cfg(test)]
// AgentTool::run requires Arc, while GPUI tasks stay on the foreground thread.
#[allow(clippy::arc_with_non_send_sync)]
mod tests {
    use super::*;
    use gpui::TestAppContext;

    fn accept_with(
        entries: impl IntoIterator<Item = (&'static str, &'static str)>,
    ) -> acp_v2::CreateElicitationResponse {
        let content: BTreeMap<String, acp_v2::ElicitationContentValue> = entries
            .into_iter()
            .map(|(key, value)| {
                (
                    key.to_string(),
                    acp_v2::ElicitationContentValue::String(value.to_string()),
                )
            })
            .collect();
        acp_v2::CreateElicitationResponse::new(acp_v2::ElicitationAction::Accept(
            acp_v2::ElicitationAcceptAction::new().content(content),
        ))
    }

    #[gpui::test]
    async fn test_deferred_question_can_be_checked_and_awaited(cx: &mut TestAppContext) {
        let tool = Arc::new(AskUserTool::default());
        let (stream, mut events) = ToolCallEventStream::test();
        let start = cx.update(|cx| {
            tool.clone().run(
                ToolInput::resolved(AskUserToolInput {
                    question: "Choose?".into(),
                    options: vec!["A".into(), "B".into()],
                    allow_free_text: false,
                    questions: Vec::new(),
                    defer: true,
                    request_id: None,
                    wait: false,
                    withdraw: false,
                }),
                stream.clone(),
                cx,
            )
        });
        let question = events.expect_elicitation().await;
        let Ok(AskUserToolOutput::Pending { request_id }) = start.await else {
            panic!("expected pending question")
        };
        let check = |wait| AskUserToolInput {
            question: String::new(),
            options: Vec::new(),
            allow_free_text: false,
            questions: Vec::new(),
            defer: false,
            request_id: Some(request_id.clone()),
            wait,
            withdraw: false,
        };
        let result = cx
            .update(|cx| {
                tool.clone()
                    .run(ToolInput::resolved(check(false)), stream.clone(), cx)
            })
            .await;
        assert!(matches!(result, Ok(AskUserToolOutput::Pending { .. })));
        let waiting = cx.update(|cx| {
            tool.clone()
                .run(ToolInput::resolved(check(true)), stream.clone(), cx)
        });
        question
            .response
            .send(accept_with([(CHOICE_FIELD, "B")]))
            .expect("question is pending");
        assert!(
            matches!(waiting.await, Ok(AskUserToolOutput::Answered { selected }) if selected == "B")
        );
        let result = cx
            .update(|cx| tool.run(ToolInput::resolved(check(false)), stream, cx))
            .await;
        assert!(matches!(result, Ok(AskUserToolOutput::Answered { selected }) if selected == "B"));
    }

    #[gpui::test]
    async fn test_deferred_question_reports_cancellation(cx: &mut TestAppContext) {
        let tool = Arc::new(AskUserTool::default());
        let (stream, mut events) = ToolCallEventStream::test();
        let start = cx.update(|cx| {
            tool.clone().run(
                ToolInput::resolved(AskUserToolInput {
                    question: "Choose?".into(),
                    options: Vec::new(),
                    allow_free_text: true,
                    questions: Vec::new(),
                    defer: true,
                    request_id: None,
                    wait: false,
                    withdraw: false,
                }),
                stream.clone(),
                cx,
            )
        });
        let question = events.expect_elicitation().await;
        let Ok(AskUserToolOutput::Pending { request_id }) = start.await else {
            panic!("expected pending question")
        };
        question
            .response
            .send(acp_v2::CreateElicitationResponse::new(
                acp_v2::ElicitationAction::Cancel,
            ))
            .expect("question is pending");
        let result = cx
            .update(|cx| {
                tool.run(
                    ToolInput::resolved(AskUserToolInput {
                        question: String::new(),
                        options: Vec::new(),
                        allow_free_text: false,
                        questions: Vec::new(),
                        defer: false,
                        request_id: Some(request_id),
                        wait: true,
                        withdraw: false,
                    }),
                    stream,
                    cx,
                )
            })
            .await;
        assert!(
            matches!(result, Err(AskUserToolOutput::Error { error }) if error.contains("cancel"))
        );
    }

    #[gpui::test]
    async fn test_deferred_questions_have_distinct_ids_when_tool_ids_are_reused(
        cx: &mut TestAppContext,
    ) {
        let tool = Arc::new(AskUserTool::default());
        let (stream, mut events) = ToolCallEventStream::test();
        let input = || AskUserToolInput {
            question: "Choose?".into(),
            options: vec!["A".into(), "B".into()],
            allow_free_text: false,
            questions: Vec::new(),
            defer: true,
            request_id: None,
            wait: false,
            withdraw: false,
        };
        let first = cx.update(|cx| {
            tool.clone()
                .run(ToolInput::resolved(input()), stream.clone(), cx)
        });
        let first_question = events.expect_elicitation().await;
        let Ok(AskUserToolOutput::Pending {
            request_id: first_id,
        }) = first.await
        else {
            panic!("first question")
        };
        let second = cx.update(|cx| tool.clone().run(ToolInput::resolved(input()), stream, cx));
        let second_question = events.expect_elicitation().await;
        let Ok(AskUserToolOutput::Pending {
            request_id: second_id,
        }) = second.await
        else {
            panic!("second question")
        };
        assert_ne!(first_id, second_id);
        first_question
            .response
            .send(accept_with([(CHOICE_FIELD, "A")]))
            .expect("first pending");
        second_question
            .response
            .send(accept_with([(CHOICE_FIELD, "B")]))
            .expect("second pending");
        cx.run_until_parked();
    }

    #[gpui::test]
    async fn test_ask_user_mixed_questions(cx: &mut TestAppContext) {
        let (event_stream, mut event_rx) = ToolCallEventStream::test();
        let task = cx.update(|cx| {
            Arc::new(AskUserTool::default()).run(
                ToolInput::resolved(AskUserToolInput {
                    question: String::new(),
                    options: Vec::new(),
                    allow_free_text: false,
                    defer: false,
                    request_id: None,
                    wait: false,
                    withdraw: false,
                    questions: vec![
                        AskUserQuestion {
                            question: "Which approach?".into(),
                            options: vec!["A".into(), "B".into()],
                            multiple: false,
                            allow_free_text: false,
                        },
                        AskUserQuestion {
                            question: "Which platforms?".into(),
                            options: vec!["macOS".into(), "Linux".into()],
                            multiple: true,
                            allow_free_text: true,
                        },
                        AskUserQuestion {
                            question: "What name?".into(),
                            options: Vec::new(),
                            multiple: false,
                            allow_free_text: true,
                        },
                    ],
                }),
                event_stream,
                cx,
            )
        });
        let request = event_rx.expect_elicitation().await;
        assert_eq!(request.schema.properties.len(), 4);
        let content = BTreeMap::from([
            (
                "q00_choice".into(),
                acp_v2::ElicitationContentValue::from("B"),
            ),
            (
                "q01_choice".into(),
                acp_v2::ElicitationContentValue::from(vec![
                    "macOS".to_string(),
                    "Linux".to_string(),
                ]),
            ),
            (
                "q01_text".into(),
                acp_v2::ElicitationContentValue::from("Windows"),
            ),
            (
                "q02_text".into(),
                acp_v2::ElicitationContentValue::from("Lin"),
            ),
        ]);
        request
            .response
            .send(acp_v2::CreateElicitationResponse::new(
                acp_v2::ElicitationAction::Accept(
                    acp_v2::ElicitationAcceptAction::new().content(content),
                ),
            ))
            .expect("question is pending");
        let Ok(AskUserToolOutput::Answers { answers }) = task.await else {
            panic!("expected answers")
        };
        assert_eq!(
            answers["1: Which approach?"]["selected"],
            serde_json::json!(["B"])
        );
        assert_eq!(
            answers["2: Which platforms?"]["selected"],
            serde_json::json!(["macOS", "Linux"])
        );
        assert_eq!(answers["2: Which platforms?"]["text"], "Windows");
        assert_eq!(answers["3: What name?"]["text"], "Lin");
    }

    #[gpui::test]
    async fn test_ask_user_returns_selected_option(cx: &mut TestAppContext) {
        let (event_stream, mut event_rx) = ToolCallEventStream::test();
        let tool_call_id = event_stream.tool_call_id().clone();
        let tool = Arc::new(AskUserTool::default());
        let task = cx.update(|cx| {
            tool.run(
                ToolInput::resolved(AskUserToolInput {
                    question: "Which approach?".into(),
                    options: vec!["Approach A".into(), "Approach B".into()],
                    allow_free_text: false,
                    questions: Vec::new(),
                    defer: false,
                    request_id: None,
                    wait: false,
                    withdraw: false,
                }),
                event_stream,
                cx,
            )
        });

        let request = event_rx.expect_elicitation().await;
        assert_eq!(request.tool_call_id.0, tool_call_id.0);
        assert!(Arc::ptr_eq(&request.tool_call_id.0, &tool_call_id.0));
        assert_eq!(request.message, "Which approach?");
        assert!(request.schema.properties.contains_key(CHOICE_FIELD));
        assert!(!request.schema.properties.contains_key(OTHER_FIELD));

        request
            .response
            .send(accept_with([(CHOICE_FIELD, "Approach B")]))
            .unwrap();

        match task.await {
            Ok(AskUserToolOutput::Answered { selected }) => assert_eq!(selected, "Approach B"),
            other => panic!("expected an answer, got {other:?}"),
        }
    }

    #[gpui::test]
    async fn test_ask_user_free_text_answer(cx: &mut TestAppContext) {
        let (event_stream, mut event_rx) = ToolCallEventStream::test();
        let tool = Arc::new(AskUserTool::default());
        let task = cx.update(|cx| {
            tool.run(
                ToolInput::resolved(AskUserToolInput {
                    question: "What should I name it?".into(),
                    options: vec![],
                    allow_free_text: true,
                    questions: Vec::new(),
                    defer: false,
                    request_id: None,
                    wait: false,
                    withdraw: false,
                }),
                event_stream,
                cx,
            )
        });

        let request = event_rx.expect_elicitation().await;
        assert!(request.schema.properties.contains_key(OTHER_FIELD));
        assert!(!request.schema.properties.contains_key(CHOICE_FIELD));

        request
            .response
            .send(accept_with([(OTHER_FIELD, "widget")]))
            .unwrap();

        match task.await {
            Ok(AskUserToolOutput::Answered { selected }) => assert_eq!(selected, "widget"),
            other => panic!("expected an answer, got {other:?}"),
        }
    }

    #[gpui::test]
    async fn test_ask_user_free_text_takes_precedence(cx: &mut TestAppContext) {
        let (event_stream, mut event_rx) = ToolCallEventStream::test();
        let tool = Arc::new(AskUserTool::default());
        let task = cx.update(|cx| {
            tool.run(
                ToolInput::resolved(AskUserToolInput {
                    question: "Pick or type".into(),
                    options: vec!["A".into(), "B".into()],
                    allow_free_text: true,
                    questions: Vec::new(),
                    defer: false,
                    request_id: None,
                    wait: false,
                    withdraw: false,
                }),
                event_stream,
                cx,
            )
        });

        let request = event_rx.expect_elicitation().await;
        assert!(request.schema.properties.contains_key(CHOICE_FIELD));
        assert!(request.schema.properties.contains_key(OTHER_FIELD));

        request
            .response
            .send(accept_with([(CHOICE_FIELD, "A"), (OTHER_FIELD, "custom")]))
            .unwrap();

        match task.await {
            Ok(AskUserToolOutput::Answered { selected }) => assert_eq!(selected, "custom"),
            other => panic!("expected the typed answer, got {other:?}"),
        }
    }

    #[gpui::test]
    async fn test_ask_user_decline_is_an_error(cx: &mut TestAppContext) {
        let (event_stream, mut event_rx) = ToolCallEventStream::test();
        let tool = Arc::new(AskUserTool::default());
        let task = cx.update(|cx| {
            tool.run(
                ToolInput::resolved(AskUserToolInput {
                    question: "Proceed?".into(),
                    options: vec!["Yes".into(), "No".into()],
                    allow_free_text: false,
                    questions: Vec::new(),
                    defer: false,
                    request_id: None,
                    wait: false,
                    withdraw: false,
                }),
                event_stream,
                cx,
            )
        });

        let request = event_rx.expect_elicitation().await;
        request
            .response
            .send(acp_v2::CreateElicitationResponse::new(
                acp_v2::ElicitationAction::Decline,
            ))
            .unwrap();

        match task.await {
            Err(AskUserToolOutput::Error { error }) => {
                assert!(error.contains("declined"), "got: {error}");
            }
            other => panic!("expected an error, got {other:?}"),
        }
    }

    #[gpui::test]
    async fn test_ask_user_requires_options_or_free_text(cx: &mut TestAppContext) {
        let (event_stream, _event_rx) = ToolCallEventStream::test();
        let tool = Arc::new(AskUserTool::default());
        let result = cx
            .update(|cx| {
                tool.run(
                    ToolInput::resolved(AskUserToolInput {
                        question: "Proceed?".into(),
                        options: vec!["Only one".into()],
                        allow_free_text: false,
                        questions: Vec::new(),
                        defer: false,
                        request_id: None,
                        wait: false,
                        withdraw: false,
                    }),
                    event_stream,
                    cx,
                )
            })
            .await;

        match result {
            Err(AskUserToolOutput::Error { error }) => {
                assert!(error.contains("at least two"), "got: {error}");
            }
            other => panic!("expected an error, got {other:?}"),
        }
    }
}
