mod apply_code_action_tool;
mod ask_user_tool;
mod context_server_registry;
mod copy_path_tool;
mod create_directory_tool;
mod create_thread_tool;
mod delete_path_tool;
mod diagnostics_tool;
mod edit_file_tool;
mod edit_session;
#[cfg(all(test, feature = "unit-eval"))]
mod evals;
mod fetch_tool;
mod find_path_tool;
mod find_references_tool;
mod get_code_actions_tool;
mod go_to_definition_tool;
mod goal_tool;
mod goal_verification;
mod grep_tool;
mod inspect_tool_result_tool;
mod list_agents_and_models_tool;
mod list_directory_tool;
mod move_path_tool;
mod project_checkpoint_tool;
mod read_file_tool;
mod rename_tool;
mod skill_tool;
mod spawn_agent_tool;
mod symbol_locator;
mod terminal_tool;
mod tool_permissions;
mod web_search_tool;
mod write_file_tool;

use crate::AgentTool;
use feature_flags::{CreateThreadToolFeatureFlag, FeatureFlag, FeatureFlagAppExt as _};
use gpui::App;
use language_model::LanguageModelRequestTool;
use serde::{
    Deserialize, Deserializer,
    de::{DeserializeOwned, Error as _},
};

/// Deserialize a value that may have been provided as a JSON-encoded string
/// instead of the structured value. Some models occasionally stringify nested
/// arguments, so we accept either form.
pub(crate) fn deserialize_maybe_stringified<'de, T, D>(deserializer: D) -> Result<T, D::Error>
where
    T: DeserializeOwned,
    D: Deserializer<'de>,
{
    fn to_custom_error<E>(e: serde_json::Error) -> E
    where
        E: serde::de::Error,
    {
        E::custom(format!("{e}"))
    }

    let raw_value = serde_json::Value::deserialize(deserializer)
        .map_err(|error| D::Error::custom(format!("invalid JSON: {error}")))?;

    match T::deserialize(&raw_value) {
        Ok(value) => Ok(value),
        Err(original_error) => {
            let Some(string) = raw_value.as_str() else {
                return Err(to_custom_error(original_error));
            };

            serde_json::from_str(string).map_err(to_custom_error)
        }
    }
}

pub use apply_code_action_tool::*;
pub use ask_user_tool::*;
pub use context_server_registry::*;
pub use copy_path_tool::*;
pub use create_directory_tool::*;
pub use create_thread_tool::*;
pub use delete_path_tool::*;
pub use diagnostics_tool::*;
pub use edit_file_tool::*;
pub use fetch_tool::*;
pub use find_path_tool::*;
pub use find_references_tool::*;
pub use get_code_actions_tool::*;
pub use go_to_definition_tool::*;
pub use goal_tool::*;
pub use goal_verification::*;
pub use grep_tool::*;
pub use inspect_tool_result_tool::*;
pub use list_agents_and_models_tool::*;
pub use list_directory_tool::*;
pub use move_path_tool::*;
pub use project_checkpoint_tool::*;
pub use read_file_tool::*;
pub use rename_tool::*;
pub use skill_tool::*;
pub use spawn_agent_tool::*;
pub use symbol_locator::*;

pub use terminal_tool::{
    SandboxedTerminalTool, SandboxedTerminalToolInput, TerminalSessionMetadata,
    TerminalSessionRequest, TerminalSessionSnapshot, TerminalSessionState, TerminalTool,
    TerminalToolInput,
};
pub use tool_permissions::*;
pub use web_search_tool::*;
pub use write_file_tool::*;

/// Availability metadata shares the schema catalog so tooling can distinguish
/// rollout policy from missing registration without assuming every schema is enabled.
#[derive(Clone, Copy)]
pub struct BuiltInTool {
    pub name: &'static str,
    pub feature_flag: Option<&'static str>,
    schema: fn() -> LanguageModelRequestTool,
    feature_enabled: fn(&App) -> bool,
    supports_provider: fn(&language_model::LanguageModelProviderId) -> bool,
    allowed_in_restricted_mode: fn() -> bool,
}

impl BuiltInTool {
    pub fn schema(&self) -> LanguageModelRequestTool {
        (self.schema)()
    }

    pub fn feature_enabled(&self, cx: &App) -> bool {
        (self.feature_enabled)(cx)
    }

    pub fn supports_provider(&self, provider: &language_model::LanguageModelProviderId) -> bool {
        (self.supports_provider)(provider)
    }

    pub fn allowed_in_restricted_mode(&self) -> bool {
        (self.allowed_in_restricted_mode)()
    }
}

#[derive(Clone, Copy)]
enum ToolRollout {
    General,
    SiblingThreads,
}

impl ToolRollout {
    const fn feature_flag(self) -> Option<&'static str> {
        match self {
            Self::General => None,
            Self::SiblingThreads => Some(CreateThreadToolFeatureFlag::NAME),
        }
    }

    fn enabled(self, cx: &App) -> bool {
        match self {
            Self::General => true,
            Self::SiblingThreads => cx.has_flag::<CreateThreadToolFeatureFlag>(),
        }
    }
}

fn language_model_tool<T: AgentTool>() -> LanguageModelRequestTool {
    let mut input_schema = T::input_schema().to_value();
    language_model::tool_schema::normalize_tool_schema(&mut input_schema);
    LanguageModelRequestTool::function(
        T::NAME.to_string(),
        T::description().to_string(),
        input_schema,
        T::supports_input_streaming(),
    )
}

macro_rules! tools {
    ($($tool:ty => $rollout:ident),* $(,)?) => {
        /// Every built-in tool name, determined at compile time.
        pub const ALL_TOOL_NAMES: &[&str] = &[
            $(<$tool>::NAME,)*
        ];

        const _: () = {
            const fn str_eq(a: &str, b: &str) -> bool {
                let a = a.as_bytes();
                let b = b.as_bytes();
                if a.len() != b.len() {
                    return false;
                }
                let mut i = 0;
                while i < a.len() {
                    if a[i] != b[i] {
                        return false;
                    }
                    i += 1;
                }
                true
            }

            const NAMES: &[&str] = ALL_TOOL_NAMES;
            let mut i = 0;
            while i < NAMES.len() {
                let mut j = i + 1;
                while j < NAMES.len() {
                    if str_eq(NAMES[i], NAMES[j]) {
                        panic!("Duplicate tool name in tools! macro");
                    }
                    j += 1;
                }
                i += 1;
            }
        };

        pub const BUILT_IN_TOOL_CATALOG: &[BuiltInTool] = &[
            $(BuiltInTool {
                name: <$tool>::NAME,
                feature_flag: ToolRollout::$rollout.feature_flag(),
                schema: language_model_tool::<$tool>,
                feature_enabled: |cx| ToolRollout::$rollout.enabled(cx),
                supports_provider: <$tool>::supports_provider,
                allowed_in_restricted_mode: <$tool>::allow_in_restricted_mode,
            },)*
        ];

        /// Returns whether the tool with the given name supports the given provider.
        pub fn tool_supports_provider(name: &str, provider: &language_model::LanguageModelProviderId) -> bool {
            built_in_tool(name).is_some_and(|tool| tool.supports_provider(provider))
        }

        /// Returns whether the tool with the given name may be provided to an
        /// agent in a restricted workspace. Unknown tools (e.g. MCP tools) are
        /// considered allowed.
        pub fn tool_allowed_in_restricted_mode(name: &str) -> bool {
            built_in_tool(name).is_none_or(|tool| tool.allowed_in_restricted_mode())
        }

        /// A list of all built-in tools
        pub fn built_in_tools() -> impl Iterator<Item = LanguageModelRequestTool> {
            BUILT_IN_TOOL_CATALOG.iter().map(BuiltInTool::schema)
        }
    };
}

tools! {
    ApplyCodeActionTool => General,
    AskUserTool => General,
    CopyPathTool => General,
    CreateDirectoryTool => General,
    CreateThreadTool => SiblingThreads,
    DeletePathTool => General,
    DiagnosticsTool => General,
    EditFileTool => General,
    FetchTool => General,
    FindPathTool => General,
    FindReferencesTool => General,
    GetCodeActionsTool => General,
    GoToDefinitionTool => General,
    GoalTool => General,
    InspectToolResultTool => General,
    GrepTool => General,
    ListAgentsAndModelsTool => General,
    ListDirectoryTool => General,
    MovePathTool => General,
    ProjectCheckpointTool => General,
    ReadFileTool => General,
    RenameTool => General,
    SkillTool => General,
    SpawnAgentTool => General,
    TerminalTool => General,
    WebSearchTool => General,
    WriteFileTool => General,
}

pub fn built_in_tool(name: &str) -> Option<&'static BuiltInTool> {
    BUILT_IN_TOOL_CATALOG.iter().find(|tool| tool.name == name)
}

/// Shared by request filtering and the profile UI. This only checks rollout;
/// profiles, provider support, registration and workspace trust still apply.
pub fn tool_feature_flag_enabled(tool_name: &str, cx: &App) -> bool {
    built_in_tool(tool_name).is_none_or(|tool| tool.feature_enabled(cx))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[gpui::test]
    fn lsp_tools_do_not_require_rollout_flags(cx: &mut App) {
        let store = settings::SettingsStore::test(cx);
        cx.set_global(store);
        cx.update_flags(false, Vec::new());
        for name in [
            FindReferencesTool::NAME,
            GoToDefinitionTool::NAME,
            GetCodeActionsTool::NAME,
            ApplyCodeActionTool::NAME,
            RenameTool::NAME,
        ] {
            assert!(tool_feature_flag_enabled(name, cx), "{name}");
            assert_eq!(
                built_in_tool(name).expect("catalog entry").feature_flag,
                None
            );
        }
        assert_eq!(
            tool_feature_flag_enabled(CreateThreadTool::NAME, cx),
            cx.has_flag::<CreateThreadToolFeatureFlag>()
        );
        assert_eq!(
            built_in_tool(CreateThreadTool::NAME)
                .expect("catalog entry")
                .feature_flag,
            Some(CreateThreadToolFeatureFlag::NAME)
        );
        assert!(tool_feature_flag_enabled("some_mcp_tool", cx));
    }

    #[gpui::test]
    fn default_profile_tools_have_schemas_and_preserve_read_only_policy(cx: &mut App) {
        use agent_settings::{AgentProfileId, AgentSettings, builtin_profiles};
        use settings::Settings as _;

        let store = settings::SettingsStore::test(cx);
        cx.set_global(store);
        project::DisableAiSettings::register(cx);
        AgentSettings::register(cx);
        let settings = AgentSettings::get_global(cx);
        assert_eq!(settings.default_profile.as_str(), builtin_profiles::WRITE);
        let write = settings
            .profiles
            .get(&AgentProfileId(builtin_profiles::WRITE.into()))
            .expect("default write profile");
        let ask = settings
            .profiles
            .get(&AgentProfileId(builtin_profiles::ASK.into()))
            .expect("default ask profile");
        for profile in settings.profiles.values() {
            for name in profile.tools.keys() {
                assert!(
                    built_in_tool(name).is_some(),
                    "profile tool `{name}` has no schema"
                );
            }
        }
        for name in ALL_TOOL_NAMES {
            assert!(write.is_tool_enabled(name), "write profile omits `{name}`");
        }
        for name in [
            FindReferencesTool::NAME,
            GoToDefinitionTool::NAME,
            GetCodeActionsTool::NAME,
        ] {
            assert!(ask.is_tool_enabled(name), "ask profile omits `{name}`");
        }
        for name in [ApplyCodeActionTool::NAME, RenameTool::NAME] {
            assert!(
                !ask.is_tool_enabled(name),
                "ask profile enables mutation `{name}`"
            );
        }
        assert!(built_in_tool(ProjectCheckpointTool::NAME).is_some());
        // A schema and profile toggle do not override a disabled backend.
        assert!(!tool_supports_provider(
            WebSearchTool::NAME,
            &language_model::LanguageModelProviderId::from("openai".to_string())
        ));
    }

    #[test]
    fn catalog_schemas_include_background_control_inputs() {
        for (name, fields) in [
            (TerminalTool::NAME, &["session"][..]),
            (
                SpawnAgentTool::NAME,
                &["background", "action", "wait", "session_id"][..],
            ),
        ] {
            let schema = built_in_tool(name).expect("catalog entry").schema();
            let language_model::LanguageModelRequestToolInput::Function { input_schema, .. } =
                schema.input
            else {
                panic!("built-in tool `{name}` must use a function schema");
            };
            let properties = input_schema
                .get("properties")
                .and_then(serde_json::Value::as_object)
                .expect("input properties");
            for field in fields {
                assert!(
                    properties.contains_key(*field),
                    "`{name}` schema omits `{field}`"
                );
            }
        }
    }

    #[test]
    fn built_in_tool_schemas_are_normalized() {
        let tools = built_in_tools().collect::<Vec<_>>();

        assert_eq!(tools.len(), ALL_TOOL_NAMES.len());
        for tool in tools {
            let language_model::LanguageModelRequestToolInput::Function { input_schema, .. } =
                tool.input
            else {
                panic!("built-in tool `{}` should use a JSON schema", tool.name);
            };
            assert_eq!(input_schema.get("$schema"), None, "tool `{}`", tool.name);
            assert_eq!(input_schema.get("title"), None, "tool `{}`", tool.name);
            assert_eq!(
                input_schema.get("description"),
                None,
                "tool `{}`",
                tool.name
            );
            assert!(
                input_schema["properties"].is_object(),
                "tool `{}` should have object properties",
                tool.name
            );
        }
    }

    #[test]
    fn restricted_mode_policy_matches_catalog() {
        assert!(!tool_allowed_in_restricted_mode(FetchTool::NAME));
        assert!(!tool_allowed_in_restricted_mode(TerminalTool::NAME));
        assert!(!tool_allowed_in_restricted_mode(
            ProjectCheckpointTool::NAME
        ));

        for name in ALL_TOOL_NAMES {
            let expected = ![
                FetchTool::NAME,
                TerminalTool::NAME,
                ProjectCheckpointTool::NAME,
            ]
            .contains(name);
            assert_eq!(
                tool_allowed_in_restricted_mode(name),
                expected,
                "unexpected restricted-mode policy for tool `{name}`"
            );
        }
        assert!(tool_allowed_in_restricted_mode("some_mcp_tool"));
    }
}
