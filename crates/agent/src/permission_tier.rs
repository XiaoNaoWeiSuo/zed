/// How much autonomy the agent gets when a tool asks for approval.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionTier {
    /// Prompt for every action that needs approval; the user decides each one.
    ReadOnly,
    /// Follow the configured tool-permission rules.
    #[default]
    Assisted,
    /// Allow actions unless a permission rule denies them.
    FullAccess,
}

impl PermissionTier {
    pub const ALL: [Self; 3] = [Self::ReadOnly, Self::Assisted, Self::FullAccess];

    pub fn label(self) -> &'static str {
        match self {
            Self::ReadOnly => "Confirm actions",
            Self::Assisted => "Assisted",
            Self::FullAccess => "Full access",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::ReadOnly => "Ask me before every action that needs approval.",
            Self::Assisted => "Follow configured tool permission rules.",
            Self::FullAccess => "Allow actions without prompting, except those explicitly denied.",
        }
    }

    pub fn index(self) -> usize {
        match self {
            Self::ReadOnly => 0,
            Self::Assisted => 1,
            Self::FullAccess => 2,
        }
    }

    pub fn apply(self, decision: crate::ToolPermissionDecision) -> crate::ToolPermissionDecision {
        use crate::ToolPermissionDecision;
        match (self, decision) {
            (_, decision @ ToolPermissionDecision::Deny(_)) => decision,
            (Self::ReadOnly, _) => ToolPermissionDecision::Confirm,
            (Self::FullAccess, _) => ToolPermissionDecision::Allow,
            (Self::Assisted, decision) => decision,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::PermissionTier;

    #[test]
    fn tiers_preserve_denials_and_assisted_confirmations() {
        use crate::ToolPermissionDecision;
        for tier in PermissionTier::ALL {
            assert_eq!(
                tier.apply(ToolPermissionDecision::Deny("blocked".into())),
                ToolPermissionDecision::Deny("blocked".into())
            );
        }
        assert_eq!(
            PermissionTier::Assisted.apply(ToolPermissionDecision::Confirm),
            ToolPermissionDecision::Confirm
        );
        assert_eq!(
            PermissionTier::ReadOnly.apply(ToolPermissionDecision::Allow),
            ToolPermissionDecision::Confirm
        );
        assert_eq!(
            PermissionTier::FullAccess.apply(ToolPermissionDecision::Confirm),
            ToolPermissionDecision::Allow
        );
    }
}
