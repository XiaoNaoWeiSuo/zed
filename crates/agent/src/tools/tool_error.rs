use language_model::LanguageModelToolResultContent;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Stable categories for recovery decisions; callers must not classify message text.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ToolErrorCategory {
    InvalidInput,
    NotFound,
    WrongResourceType,
    PermissionDenied,
    Conflict,
    Cancelled,
    Timeout,
    Unavailable,
    Internal,
}

/// A suggested next call, not authorization to execute it or a promise of success.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ToolRecovery {
    pub tool: String,
    pub arguments: Value,
    pub reason: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ToolError {
    pub category: ToolErrorCategory,
    pub message: String,
    /// Whether retrying the unchanged call may succeed. Recovery calls are separate.
    pub retryable: bool,
    #[serde(default)]
    pub recover_with: Vec<ToolRecovery>,
    /// Optional metadata for callers that need to know whether a failed operation
    /// may have changed the target. It is omitted for backwards-compatible errors
    /// where no side effect was possible.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub side_effect: Option<ToolSideEffect>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ToolSideEffect {
    None,
    Unknown,
    Present,
}

impl ToolError {
    pub fn new(category: ToolErrorCategory, message: impl Into<String>, retryable: bool) -> Self {
        Self {
            category,
            message: message.into(),
            retryable,
            recover_with: Vec::new(),
            side_effect: None,
        }
    }

    /// Convenience for tools whose model-facing error payload is a plain
    /// string: the JSON remains the message transport, but carries recovery
    /// metadata instead of only prose.
    pub fn to_json_string(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|serialization_error| {
            log::error!("Failed to serialize tool error: {serialization_error}");
            self.message.clone()
        })
    }
}

/// Builds the model-facing JSON payload for a tool whose output type is a
/// plain string. Recovery suggestions are advisory: nothing is executed
/// automatically and no permission is implied.
pub fn tool_error_json(
    category: ToolErrorCategory,
    message: impl std::fmt::Display,
    retryable: bool,
    recover_with: Vec<ToolRecovery>,
    side_effect: Option<ToolSideEffect>,
) -> String {
    let mut error = ToolError::new(category, message.to_string(), retryable);
    error.recover_with = recover_with;
    error.side_effect = side_effect;
    error.to_json_string()
}

/// A read-only composition plan. It describes calls for the model to make; it
/// does not authorize or execute any of them.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct InspectAndReadPlan {
    pub path: String,
    pub steps: Vec<ToolRecovery>,
}

impl InspectAndReadPlan {
    /// Builds the minimal safe plan for reading a known project path.
    ///
    /// Path resolution and all permissions remain the responsibility of
    /// `read_file`, so this helper cannot broaden the caller's authority.
    pub fn for_path(path: impl Into<String>) -> Result<Self, ToolError> {
        let path = path.into();
        if path.trim().is_empty() {
            return Err(ToolError::new(
                ToolErrorCategory::InvalidInput,
                "A project path is required",
                false,
            ));
        }

        Ok(Self {
            path: path.clone(),
            steps: vec![ToolRecovery {
                tool: "read_file".into(),
                arguments: serde_json::json!({ "path": path }),
                reason: "Read the existing file through the normal path and permission checks"
                    .into(),
            }],
        })
    }
}

/// Structured state that a caller can return after executing a composition
/// plan. `content` is supplied by the existing read tool; this type performs
/// no filesystem access and never implies that a suggested follow-up ran.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct InspectAndReadResult {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(default)]
    pub next_steps: Vec<ToolRecovery>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ToolError>,
}

impl From<ToolError> for InspectAndReadResult {
    fn from(error: ToolError) -> Self {
        Self {
            path: String::new(),
            content: None,
            next_steps: error.recover_with.clone(),
            error: Some(error),
        }
    }
}

impl From<ToolError> for LanguageModelToolResultContent {
    fn from(error: ToolError) -> Self {
        // Keep the existing error transport and is_error handling: only the text
        // payload changes, so providers and the thread ledger need no new variant.
        match serde_json::to_string(&error) {
            Ok(json) => json.into(),
            Err(serialization_error) => {
                log::error!("Failed to serialize tool error: {serialization_error}");
                error.message.into()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn structured_error_round_trip() -> anyhow::Result<()> {
        let error = ToolError {
            category: ToolErrorCategory::WrongResourceType,
            message: "root/directory is a directory, not a file".into(),
            retryable: false,
            recover_with: vec![ToolRecovery {
                tool: "list_directory".into(),
                arguments: json!({"path": "root/directory"}),
                reason: "Inspect the directory before choosing a file to read".into(),
            }],
            side_effect: None,
        };
        let content = LanguageModelToolResultContent::from(error.clone());
        let LanguageModelToolResultContent::Text(text) = content else {
            anyhow::bail!("Expected a text payload");
        };
        let value: Value = serde_json::from_str(&text)?;
        assert_eq!(value["category"], "wrong_resource_type");
        assert_eq!(value["retryable"], false);
        assert_eq!(value["recover_with"][0]["tool"], "list_directory");
        assert_eq!(serde_json::from_value::<ToolError>(value)?, error);
        Ok(())
    }

    #[test]
    fn recovery_defaults_to_empty_and_unknown_fields_are_ignored() -> anyhow::Result<()> {
        let error: ToolError = serde_json::from_value(json!({
            "category": "cancelled",
            "message": "Cancelled by user",
            "retryable": false,
            "future_field": true
        }))?;
        assert!(error.recover_with.is_empty());
        assert_eq!(error.side_effect, None);
        assert!(!error.retryable);
        Ok(())
    }

    #[test]
    fn side_effect_metadata_is_optional_and_structured() -> anyhow::Result<()> {
        let mut error = ToolError::new(
            ToolErrorCategory::Conflict,
            "edit may have changed the file",
            false,
        );
        error.side_effect = Some(ToolSideEffect::Unknown);
        let value = serde_json::to_value(&error)?;
        assert_eq!(value["side_effect"], "unknown");
        assert_eq!(serde_json::from_value::<ToolError>(value)?, error);
        Ok(())
    }

    #[test]
    fn inspect_and_read_plan_only_suggests_read_file() {
        let plan = InspectAndReadPlan::for_path("root/src/lib.rs").expect("valid path");
        assert_eq!(plan.path, "root/src/lib.rs");
        assert_eq!(plan.steps.len(), 1);
        assert_eq!(plan.steps[0].tool, "read_file");
        assert_eq!(plan.steps[0].arguments, json!({"path": "root/src/lib.rs"}));
    }

    #[test]
    fn inspect_and_read_plan_rejects_blank_paths() {
        let error = InspectAndReadPlan::for_path("  ").expect_err("blank path");
        assert_eq!(error.category, ToolErrorCategory::InvalidInput);
        assert!(!error.retryable);
        assert!(error.recover_with.is_empty());
    }

    #[test]
    fn inspect_and_read_result_preserves_error_and_recovery() {
        let mut error = ToolError::new(ToolErrorCategory::NotFound, "missing", false);
        error.recover_with.push(ToolRecovery {
            tool: "list_directory".into(),
            arguments: json!({"path": "root"}),
            reason: "Inspect the parent".into(),
        });
        let result = InspectAndReadResult::from(error.clone());
        assert_eq!(result.content, None);
        assert_eq!(result.next_steps, error.recover_with);
        assert_eq!(result.error, Some(error));
    }
}
