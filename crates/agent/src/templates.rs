use anyhow::Result;
use gpui::SharedString;
use handlebars::Handlebars;
use serde::Serialize;
use std::sync::Arc;

// Dev builds read the checkout's templates at runtime instead of embedding
// them; see the `assets` crate for the rationale.
util::fs_embed! {
    struct Assets,
    crate_relative = "src/templates",
    root_relative = "crates/agent/src/templates",
    include = ["*.hbs"],
}

pub struct Templates(Handlebars<'static>);

impl Templates {
    pub fn new() -> Arc<Self> {
        let mut handlebars = Handlebars::new();
        handlebars.set_strict_mode(true);
        handlebars.register_helper("contains", Box::new(contains));
        handlebars.register_embed_templates::<Assets>().unwrap();
        Arc::new(Self(handlebars))
    }
}

pub trait Template: Sized {
    const TEMPLATE_NAME: &'static str;

    fn render(&self, templates: &Templates) -> Result<String>
    where
        Self: Serialize + Sized,
    {
        Ok(templates.0.render(Self::TEMPLATE_NAME, self)?)
    }
}

/// The system prompt carries instructions only. Everything that can change
/// during a session (date, project roots, rules, skills, sandbox state, model,
/// goals, terminal capabilities, runtime state) lives in
/// [`HarnessContextTemplate`] and travels in its own per-request message, so the
/// system prompt stays byte-identical and the provider's prefix cache keeps
/// hitting.
#[derive(Serialize)]
pub struct SystemPromptTemplate {
    pub available_tools: Vec<SharedString>,
    /// Whether this thread is a delegated subagent. Constant for the lifetime
    /// of a thread, so it can stay in the system prompt.
    pub is_subagent: bool,
}

impl Template for SystemPromptTemplate {
    const TEMPLATE_NAME: &'static str = "system_prompt.hbs";
}

/// Per-request state rendered next to the current user message, outside the
/// stable system-prompt prefix.
#[derive(Serialize)]
pub struct HarnessContextTemplate<'a> {
    #[serde(flatten)]
    pub project: &'a prompt_store::ProjectContext,
    pub available_tools: Vec<SharedString>,
    pub model_name: Option<String>,
    pub date: String,
    /// Contents of the user-global `~/.config/zed/AGENTS.md` file (or the
    /// platform equivalent), if present and non-empty.
    pub user_agents_md: Option<SharedString>,
    /// Whether agent-run terminal commands are wrapped in an OS-level
    /// sandbox for this thread. When `true` — and the `terminal` tool is
    /// in `available_tools` — the rendered context describes the sandbox's
    /// read/write/network rules and the per-command flags the model can
    /// request to relax them. Otherwise the sandbox section is omitted.
    pub sandboxing: bool,
    /// Whether the host is Linux. The writable-temp story differs by
    /// platform (Linux exposes an ephemeral `tmpfs` over `/tmp`; other
    /// platforms provide a persistent per-thread `$TMPDIR`), so the sandbox
    /// section describes the right one rather than advertising a `$TMPDIR`
    /// that doesn't behave as stated.
    pub is_linux: bool,
    /// Whether sandboxed terminal commands run through WSL on Windows.
    pub is_windows: bool,
}

impl Template for HarnessContextTemplate<'_> {
    const TEMPLATE_NAME: &'static str = "harness_context.hbs";
}

/// Handlebars helper for checking if an item is in a list
fn contains(
    h: &handlebars::Helper,
    _: &handlebars::Handlebars,
    _: &handlebars::Context,
    _: &mut handlebars::RenderContext,
    out: &mut dyn handlebars::Output,
) -> handlebars::HelperResult {
    let list = h
        .param(0)
        .and_then(|v| v.value().as_array())
        .ok_or_else(|| {
            handlebars::RenderError::new("contains: missing or invalid list parameter")
        })?;
    let query = h.param(1).map(|v| v.value()).ok_or_else(|| {
        handlebars::RenderError::new("contains: missing or invalid query parameter")
    })?;

    if list.contains(query) {
        out.write("true")?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use prompt_store::{ProjectContext, RulesFileContext, WorktreeContext};
    use util::rel_path::RelPath;

    fn tool_names(tools: &[&str]) -> Vec<SharedString> {
        tools.iter().map(|tool| SharedString::from(*tool)).collect()
    }

    fn render_system_prompt(tools: &[&str], is_subagent: bool) -> String {
        SystemPromptTemplate {
            available_tools: tool_names(tools),
            is_subagent,
        }
        .render(&Templates::new())
        .expect("system prompt")
    }

    fn render_harness_context(
        project: &ProjectContext,
        tools: &[&str],
        user_agents_md: Option<&str>,
        sandboxing: bool,
        is_linux: bool,
        is_windows: bool,
    ) -> String {
        HarnessContextTemplate {
            project,
            available_tools: tool_names(tools),
            model_name: Some("test-model".to_string()),
            date: "2026-01-01".to_string(),
            user_agents_md: user_agents_md.map(SharedString::from),
            sandboxing,
            is_linux,
            is_windows,
        }
        .render(&Templates::new())
        .expect("harness context")
    }

    #[test]
    fn system_prompt_carries_only_stable_instructions() {
        let rendered = render_system_prompt(&["echo"], false);
        assert!(rendered.contains("You are the Lin coding agent"));
        assert!(rendered.contains("## Capabilities and help"));
        assert!(rendered.contains(
            "Authority order: system and tool constraints, the user's latest explicit task"
        ));
        assert!(!rendered.contains("Use `help` only when uncertain about a capability"));
        assert!(
            rendered
                .contains("Verification earns its cost only when it can change your conclusion")
        );
        assert!(rendered.contains("If no plausible failure exists, run nothing"));
        assert!(rendered.contains("directly inspect, change, and check"));
        assert!(
            rendered.contains(
                "Create a goal only when the user explicitly requests a sustained target"
            )
        );
        assert!(rendered.contains("For authorized implementation work"));
        assert!(
            rendered.contains("Tool schemas and current authorization state are authoritative")
        );
        assert!(rendered.contains("The newest user message is the current task"));
        assert!(!rendered.contains("ask_user"));
        assert!(!rendered.contains("manage_goal"));
        assert!(!rendered.contains("1-2 focused attempts"));

        // Volatile session state must never live in the system prompt, or the
        // provider's prefix cache breaks whenever it changes.
        assert!(!rendered.contains("Today's Date"));
        assert!(!rendered.contains("Operating System"));
        assert!(!rendered.contains("## System Information"));
        assert!(!rendered.contains("test-model"));
        assert!(!rendered.contains("## Model Information"));
        assert!(!rendered.contains("## Runtime permissions"));
        assert!(!rendered.contains("Terminal execution capabilities"));
        assert!(!rendered.contains("Active user goal"));
        assert!(!rendered.contains("Project checkpoint data"));
        assert!(!rendered.contains("Harness runtime snapshot"));
        assert!(!rendered.contains("/tmp/alpha"));
    }

    #[test]
    fn prompt_variants_render_identical_static_instructions() {
        let templates = Templates::new();
        for tools in [
            Vec::new(),
            vec!["read_file".to_string()],
            vec!["skill".to_string()],
        ] {
            let data = serde_json::json!({
                "available_tools": tools,
                "is_subagent": false,
            });
            let system = templates
                .0
                .render("system_prompt.hbs", &data)
                .expect("system prompt");
            let experimental = templates
                .0
                .render("experimental_system_prompt.hbs", &data)
                .expect("experimental prompt");
            assert_eq!(system, experimental);
        }
    }

    #[test]
    fn prompt_variants_stay_compact() {
        let templates = Templates::new();
        let data = serde_json::json!({
            "available_tools": [
                "read_file", "terminal", "ask_user", "manage_goal",
                "project_checkpoint", "spawn_agent", "create_directory", "skill",
            ],
            "is_subagent": true,
        });
        for name in ["system_prompt.hbs", "experimental_system_prompt.hbs"] {
            let rendered = templates.0.render(name, &data).expect("render prompt");
            assert!(
                rendered.chars().count() <= 6000,
                "{name} grew to {} chars",
                rendered.chars().count()
            );
            for (tool, phrase) in [
                ("ask_user", "Use `ask_user`"),
                ("project_checkpoint", "Use `project_checkpoint`"),
                ("spawn_agent", "Delegate only"),
            ] {
                assert!(!rendered.contains(phrase), "{name}: {tool}");
            }
            assert!(!rendered.contains("Use `help` only when uncertain"));
            assert!(rendered.contains("## Subagent boundary"));
            assert!(rendered.contains("## Agent Skills"));
            assert!(rendered.contains(
                "Create a goal only when the user explicitly requests a sustained target"
            ));
        }
    }

    #[test]
    fn prompt_variants_load_only_available_workflows() {
        let rendered = render_system_prompt(&[], false);
        assert!(rendered.contains("supplied conversation only"));
        assert!(rendered.contains(
            "Authority order: system and tool constraints, the user's latest explicit task"
        ));
        assert!(rendered.contains("For authorized implementation work"));
        assert!(
            rendered
                .contains("Verification earns its cost only when it can change your conclusion")
        );
        assert!(!rendered.contains("For a greeting alone"));
        assert!(!rendered.contains("## Capabilities and help"));
        assert!(!rendered.contains("## Agent Skills"));
        assert!(!rendered.contains("## Subagent boundary"));

        let rendered = render_system_prompt(&["ask_user", "manage_goal"], false);
        assert!(rendered.contains("directly inspect, change, and check"));
        assert!(
            rendered.contains(
                "Create a goal only when the user explicitly requests a sustained target"
            )
        );
        assert!(!rendered.contains("Use `ask_user` for blocking decisions"));
        assert!(!rendered.contains("Never guess an answer"));
        assert!(!rendered.contains("Use `project_checkpoint`"));
        assert!(!rendered.contains("Use `help` only when uncertain"));

        let rendered = render_system_prompt(&["skill"], false);
        assert!(rendered.contains("## Agent Skills"));
        assert!(rendered.contains("use the `skill` tool to retrieve the full instructions"));
    }

    #[test]
    fn system_prompt_renders_subagent_boundary() {
        let rendered = render_system_prompt(&["read_file"], true);
        assert!(rendered.contains("## Subagent boundary"));
        assert!(rendered.contains("You work for the parent agent"));
        assert!(!render_system_prompt(&["read_file"], false).contains("## Subagent boundary"));
    }

    #[test]
    fn harness_context_renders_environment_and_model() {
        let worktrees = vec![WorktreeContext {
            root_name: "alpha".to_string(),
            abs_path: std::path::Path::new("/tmp/alpha").into(),
            rules_file: None,
        }];
        let project = ProjectContext::new(worktrees);
        let rendered = render_harness_context(&project, &["read_file"], None, false, false, false);

        assert!(rendered.starts_with("<harness_context>"));
        assert!(rendered.trim_end().ends_with("</harness_context>"));
        assert!(rendered.contains("not authored by the user"));
        assert!(rendered.contains("Today's Date: 2026-01-01"));
        assert!(rendered.contains("`/tmp/alpha`"));
        assert!(rendered.contains("You are powered by the model named test-model"));
        assert!(!rendered.contains("## Runtime permissions"));
    }

    #[test]
    fn harness_context_renders_skills_catalog() {
        let project = ProjectContext::default().with_skills(vec![agent_skills::SkillSummary {
            name: "gpui-test".to_string(),
            description: "Test GPUI code".to_string(),
            location: "/tmp/skills/gpui-test/SKILL.md".to_string(),
        }]);
        let rendered = render_harness_context(&project, &["skill"], None, false, false, false);
        assert!(rendered.contains("<available_skills>"));
        assert!(rendered.contains("<name>gpui-test</name>"));
        assert!(rendered.contains("/tmp/skills/gpui-test/SKILL.md"));
    }

    #[test]
    fn harness_context_renders_user_agents_md_before_project_rules() {
        let worktrees = vec![WorktreeContext {
            root_name: "my-project".to_string(),
            abs_path: std::path::Path::new("/tmp/my-project").into(),
            rules_file: Some(RulesFileContext {
                path_in_worktree: RelPath::from_unix_str("AGENTS.md").unwrap().into(),
                text: "project-specific guidance".to_string(),
                project_entry_id: 1,
            }),
        }];
        let project = ProjectContext::new(worktrees);
        let rendered = render_harness_context(
            &project,
            &["echo"],
            Some("always be concise"),
            false,
            false,
            false,
        );

        assert!(rendered.contains("### Personal `AGENTS.md`"));
        assert!(rendered.contains("always be concise"));
        assert!(rendered.contains("### Project Rules"));
        assert!(rendered.contains("project-specific guidance"));

        let personal_idx = rendered.find("### Personal `AGENTS.md`").unwrap();
        let project_idx = rendered.find("### Project Rules").unwrap();
        assert!(
            personal_idx < project_idx,
            "personal AGENTS.md should render before project rules so project rules can override it"
        );
    }

    #[test]
    fn harness_context_omits_custom_instructions_when_absent() {
        let project = ProjectContext::default();
        let rendered = render_harness_context(&project, &["echo"], None, false, false, false);
        assert!(!rendered.contains("### Personal `AGENTS.md`"));
        assert!(!rendered.contains("### Project Rules"));
        assert!(!rendered.contains("## User's Custom Instructions"));
    }

    #[test]
    fn harness_context_omits_sandbox_section_when_sandboxing_disabled() {
        let project = ProjectContext::default();
        let rendered = render_harness_context(&project, &["terminal"], None, false, false, false);
        assert!(!rendered.contains("## Runtime permissions"));
        assert!(!rendered.contains("allow_hosts"));
    }

    #[test]
    fn harness_context_renders_sandbox_section_with_worktrees_when_enabled() {
        let worktrees = vec![
            WorktreeContext {
                root_name: "alpha".to_string(),
                abs_path: std::path::Path::new("/tmp/alpha").into(),
                rules_file: None,
            },
            WorktreeContext {
                root_name: "beta".to_string(),
                abs_path: std::path::Path::new("/tmp/beta").into(),
                rules_file: None,
            },
        ];
        let project = ProjectContext::new(worktrees);
        let rendered = render_harness_context(
            &project,
            &["echo", "terminal", "create_directory"],
            None,
            true,
            false,
            false,
        );

        assert!(rendered.contains("## Runtime permissions"));
        assert!(rendered.contains("`/tmp/alpha`"));
        assert!(rendered.contains("`/tmp/beta`"));
        assert!(!rendered.contains("`.git` directories remain protected"));
        assert!(!rendered.contains("Git metadata writes cannot be granted inside the sandbox"));
        assert!(rendered.contains("Requests outside active grants require approval"));
        // macOS tolerates granting a not-yet-existing path, so the
        // existing-directory requirement must not be stated there; the
        // `create_directory` flow is the preferred guidance instead.
        assert!(!rendered.contains("Write grants require an existing directory"));
        assert!(!rendered.contains("first use `create_directory`"));
    }

    #[test]
    fn harness_context_linux_sandbox_section_omits_tmpdir() {
        let worktrees = vec![WorktreeContext {
            root_name: "alpha".to_string(),
            abs_path: std::path::Path::new("/tmp/alpha").into(),
            rules_file: None,
        }];
        let project = ProjectContext::new(worktrees);
        let rendered =
            render_harness_context(&project, &["echo", "terminal"], None, true, true, false);

        assert!(rendered.contains("## Runtime permissions"));
        assert!(!rendered.contains("$TMPDIR"));
        assert!(!rendered.contains("`/tmp` is writable"));
        assert!(rendered.contains("`/tmp/alpha`"));
    }

    #[test]
    fn harness_context_windows_sandbox_section_rejects_host_specific_network() {
        let worktrees = vec![WorktreeContext {
            root_name: "alpha".to_string(),
            abs_path: std::path::Path::new("C:/Users/me/project").into(),
            rules_file: None,
        }];
        let project = ProjectContext::new(worktrees);
        let rendered =
            render_harness_context(&project, &["echo", "terminal"], None, true, false, true);

        assert!(rendered.contains("## Runtime permissions"));
        assert!(!rendered.contains("commands run inside WSL"));
        assert!(!rendered.contains("Host-specific network grants"));
    }

    #[test]
    fn harness_context_sandbox_section_handles_zero_worktrees() {
        let project = ProjectContext::default();
        let rendered =
            render_harness_context(&project, &["echo", "terminal"], None, true, false, false);

        assert!(rendered.contains("## Runtime permissions"));
        assert!(!rendered.contains("No project directories are currently writable"));
    }

    #[test]
    fn harness_context_omits_sandbox_section_when_terminal_tool_unavailable() {
        // A profile can disable the terminal tool entirely; the context must
        // not describe a sandboxed `terminal` tool the model doesn't have.
        let project = ProjectContext::default();
        let rendered = render_harness_context(&project, &["echo"], None, true, false, false);
        assert!(!rendered.contains("## Runtime permissions"));
        assert!(!rendered.contains("allow_hosts"));
    }

    #[test]
    fn prompt_variants_do_not_render_legacy_zed_rules_section() {
        let project = ProjectContext::default();
        let rendered = render_harness_context(&project, &["echo"], None, false, false, false);
        assert!(!rendered.contains("The user has specified the following rules"));
        assert!(!rendered.contains("Rules title:"));
    }
}
