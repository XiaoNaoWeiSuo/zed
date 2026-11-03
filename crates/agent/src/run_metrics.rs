use std::collections::HashMap;
use std::time::Duration;

use db::kvp::KeyValueStore;
use gpui::{App, AppContext as _, Context, Entity, Global, WindowId};
use language_model::LanguageModelProviderId;
use serde::{Deserialize, Serialize};

const METRICS_KEY_PREFIX: &str = "agent-metrics/";
/// Persisted snapshots from before decode-rate tracking used a different
/// algorithm; filter their implausible values without capping new ones.
pub const METRICS_VERSION: u32 = 2;
const LEGACY_MAX_TOKENS_PER_SECOND: f32 = 5_000.0;
/// A persisted rate above this can only come from corrupt data, not a real
/// generation speed. This is not a performance cap.
const MAX_PERSISTED_TOKENS_PER_SECOND: f32 = 1_000_000.0;

/// Metrics for the most recent completion request in one agent session.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct AgentMetricsSnapshot {
    #[serde(default)]
    pub version: u32,
    pub running: bool,
    pub provider: Option<String>,
    pub model: Option<String>,
    /// Fraction of input tokens served from the provider's prompt cache.
    pub cache_hit_rate: Option<f32>,
    /// Output tokens per second after the first token, excluding time to first
    /// token and including reasoning tokens.
    pub tokens_per_second: Option<f32>,
    pub time_to_first_token_ms: Option<u64>,
    pub context_used_tokens: Option<u64>,
    pub context_max_tokens: Option<u64>,
}

impl AgentMetricsSnapshot {
    pub fn provider_id(&self) -> Option<LanguageModelProviderId> {
        self.provider.clone().map(LanguageModelProviderId::from)
    }

    pub fn time_to_first_token(&self) -> Option<Duration> {
        self.time_to_first_token_ms.map(Duration::from_millis)
    }
}

/// Per-session metrics plus the session each window is currently viewing.
#[derive(Default)]
pub struct AgentRunMetrics {
    /// Keyed by window so a detached companion window and its source window
    /// do not overwrite each other's indicator state on every render.
    active_sessions: HashMap<WindowId, String>,
    snapshots: HashMap<String, AgentMetricsSnapshot>,
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

    pub fn active_snapshot(&self, window_id: WindowId) -> Option<&AgentMetricsSnapshot> {
        self.active_sessions
            .get(&window_id)
            .and_then(|session_id| self.snapshots.get(session_id))
    }

    pub fn snapshot_for(&self, session_id: &str) -> Option<&AgentMetricsSnapshot> {
        self.snapshots.get(session_id)
    }

    /// Drops the window's indicator state when the window no longer displays
    /// a conversation, so a detached thread's metrics do not linger in the
    /// window it came from.
    pub fn clear_active_session(&mut self, window_id: WindowId, cx: &mut Context<Self>) {
        if self.active_sessions.remove(&window_id).is_some() {
            cx.notify();
        }
    }

    pub fn set_active_session(
        &mut self,
        window_id: WindowId,
        session_id: String,
        cx: &mut Context<Self>,
    ) {
        if self.active_sessions.get(&window_id).map(String::as_str) == Some(session_id.as_str()) {
            return;
        }
        self.active_sessions.insert(window_id, session_id.clone());
        if !self.snapshots.contains_key(&session_id) {
            let kvp = KeyValueStore::global(cx);
            let key = format!("{METRICS_KEY_PREFIX}{session_id}");
            cx.spawn(async move |this, cx| {
                let Ok(Some(json)) = kvp.read_kvp(&key) else {
                    return;
                };
                let Ok(mut snapshot) = serde_json::from_str::<AgentMetricsSnapshot>(&json) else {
                    return;
                };
                snapshot.running = false;
                let has_corrupt_rate = snapshot.tokens_per_second.is_some_and(|rate| {
                    !rate.is_finite() || rate <= 0.0 || rate > MAX_PERSISTED_TOKENS_PER_SECOND
                });
                let has_legacy_rate = snapshot.version < METRICS_VERSION
                    && snapshot
                        .tokens_per_second
                        .is_some_and(|rate| rate > LEGACY_MAX_TOKENS_PER_SECOND);
                if has_corrupt_rate || has_legacy_rate {
                    snapshot.tokens_per_second = None;
                }
                this.update(cx, |this, cx| {
                    this.snapshots.entry(session_id).or_insert(snapshot);
                    cx.notify();
                })
                .ok();
            })
            .detach();
        }
        cx.notify();
    }

    pub fn update_session(
        &mut self,
        session_id: String,
        snapshot: AgentMetricsSnapshot,
        persist: bool,
        cx: &mut Context<Self>,
    ) {
        self.snapshots.insert(session_id.clone(), snapshot.clone());
        if persist {
            let kvp = KeyValueStore::global(cx);
            let key = format!("{METRICS_KEY_PREFIX}{session_id}");
            cx.background_spawn(async move {
                let Ok(json) = serde_json::to_string(&snapshot) else {
                    return;
                };
                if let Err(error) = kvp.write_kvp(key, json).await {
                    log::warn!("failed to persist agent metrics: {error:#}");
                }
            })
            .detach();
        }
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{TestAppContext, WindowId};

    fn snapshot(model: &str) -> AgentMetricsSnapshot {
        AgentMetricsSnapshot {
            model: Some(model.to_string()),
            ..Default::default()
        }
    }

    #[gpui::test]
    fn active_sessions_are_scoped_to_each_window(cx: &mut TestAppContext) {
        let metrics = cx.new(|_| AgentRunMetrics::default());
        let window_a = WindowId::from(1);
        let window_b = WindowId::from(2);

        cx.update(|cx| {
            metrics.update(cx, |metrics, cx| {
                metrics.update_session("session-a".to_string(), snapshot("a"), false, cx);
                metrics.update_session("session-b".to_string(), snapshot("b"), false, cx);
                metrics.set_active_session(window_a, "session-a".to_string(), cx);
                metrics.set_active_session(window_b, "session-b".to_string(), cx);
            });
        });

        metrics.read_with(cx, |metrics, _| {
            assert_eq!(
                metrics
                    .active_snapshot(window_a)
                    .and_then(|snapshot| snapshot.model.as_deref()),
                Some("a"),
                "each window must see its own conversation's metrics"
            );
            assert_eq!(
                metrics
                    .active_snapshot(window_b)
                    .and_then(|snapshot| snapshot.model.as_deref()),
                Some("b")
            );
        });

        cx.update(|cx| {
            metrics.update(cx, |metrics, cx| {
                metrics.clear_active_session(window_a, cx);
            });
        });
        metrics.read_with(cx, |metrics, _| {
            assert!(metrics.active_snapshot(window_a).is_none());
            assert!(metrics.active_snapshot(window_b).is_some());
        });
    }
}
