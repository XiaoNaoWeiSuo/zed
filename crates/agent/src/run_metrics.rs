use std::time::Duration;

use gpui::{App, AppContext as _, Context, Entity, Global, SharedString};
use language_model::LanguageModelProviderId;

/// Live metrics for the most recent native-agent completion request.
#[derive(Clone, Debug, Default)]
pub struct AgentRunMetrics {
    pub running: bool,
    pub provider: Option<LanguageModelProviderId>,
    pub model: Option<SharedString>,
    /// Fraction of input tokens served from the provider's prompt cache.
    pub cache_hit_rate: Option<f32>,
    pub tokens_per_second: Option<f32>,
    pub time_to_first_token: Option<Duration>,
}

struct GlobalAgentRunMetrics(Entity<AgentRunMetrics>);

impl Global for GlobalAgentRunMetrics {}

impl AgentRunMetrics {
    pub fn global(cx: &mut App) -> Entity<Self> {
        if let Some(global) = cx.try_global::<GlobalAgentRunMetrics>() {
            return global.0.clone();
        }
        let entity = cx.new(|_| Self::default());
        cx.set_global(GlobalAgentRunMetrics(entity.clone()));
        entity
    }

    pub fn update(&mut self, cx: &mut Context<Self>, update: impl FnOnce(&mut Self)) {
        update(self);
        cx.notify();
    }
}
