/// How much autonomy the agent gets when a tool asks for approval.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PermissionTier {
    /// Prompt for every action that needs approval; the user decides each one.
    ReadOnly,
    /// Auto-approve low-risk actions and only prompt for actions likely to mutate.
    #[default]
    Assisted,
    /// Never prompt; allow every action the active profile makes available.
    FullAccess,
}

impl PermissionTier {
    pub const ALL: [Self; 3] = [Self::ReadOnly, Self::Assisted, Self::FullAccess];

    pub fn label(self) -> &'static str {
        match self {
            Self::ReadOnly => "Read-only",
            Self::Assisted => "Assisted",
            Self::FullAccess => "Full access",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::ReadOnly => "Ask me before every action that needs approval.",
            Self::Assisted => "Auto-approve low-risk actions and ask only when it matters.",
            Self::FullAccess => "Allow every action without prompting.",
        }
    }

    pub fn index(self) -> usize {
        match self {
            Self::ReadOnly => 0,
            Self::Assisted => 1,
            Self::FullAccess => 2,
        }
    }

    /// Conservative risk check used by [`Self::Assisted`].
    ///
    /// Read-only tools are always safe. Terminal commands are only approved
    /// when every chained segment is a known read-only command and the command
    /// contains no redirection or shell substitution.
    pub fn assisted_auto_approve(tool_name: &str, input_values: &[String]) -> bool {
        const READ_ONLY_TOOLS: [&str; 10] = [
            "read_file",
            "grep",
            "list_directory",
            "find_path",
            "diagnostics",
            "get_code_actions",
            "go_to_definition",
            "find_references",
            "list_agents_and_models",
            "ask_user",
        ];
        const READ_ONLY_COMMANDS: [&str; 14] = [
            "ls", "cat", "head", "tail", "rg", "grep", "find", "pwd", "wc", "tree", "which",
            "echo", "sed", "git",
        ];

        if READ_ONLY_TOOLS.contains(&tool_name) {
            return true;
        }
        if !matches!(tool_name, "terminal" | "sandboxed_terminal") {
            return false;
        }
        let Some(command) = input_values.first() else {
            return false;
        };
        if ["<", ">", "`", "$(", "&>"]
            .iter()
            .any(|token| command.contains(token))
        {
            return false;
        }

        command.split([';', '|', '&']).all(|segment| {
            let mut words = segment.split_whitespace();
            let Some(program) = words.next() else {
                return false;
            };
            if !READ_ONLY_COMMANDS.contains(&program) {
                return false;
            }
            if program == "git" {
                matches!(
                    words.next(),
                    Some("status" | "diff" | "log" | "show" | "branch" | "rev-parse")
                )
            } else {
                true
            }
        })
    }
}
