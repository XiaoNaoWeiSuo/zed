use std::collections::HashMap;
use std::time::Duration;

use db::kvp::KeyValueStore;
use gpui::{App, AppContext as _, Context, Entity, Global};
use language_model::LanguageModelProviderId;
use serde::{Deserialize, Serialize};

const METRICS_KEY_PREFIX: &str = "agent-metrics/";

/// Metrics for the most recent completion request in one agent session.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct AgentMetricsSnapshot {
    pub running: bool,
    pub provider: Option<String>,
    pub model: Option<String>,
    /// Fraction of input tokens served from the provider's prompt cache.
    pub cache_hit_rate: Option<f32>,
    pub tokens_per_second: Option<f32>,
    pub time_to_first_token_ms: Option<u64>,
}

impl AgentMetricsSnapshot {
    pub fn provider_id(&self) -> Option<LanguageModelProviderId> {
        self.provider.clone().map(LanguageModelProviderId::from)
    }

    pub fn time_to_first_token(&self) -> Option<Duration> {
        self.time_to_first_token_ms.map(Duration::from_millis)
    }
}

/// Per-session metrics plus the currently viewed session.
#[derive(Default)]
pub struct AgentRunMetrics {
    active_session: Option<String>,
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

    pub fn active_snapshot(&self) -> Option<&AgentMetricsSnapshot> {
        self.active_session
            .as_ref()
            .and_then(|session_id| self.snapshots.get(session_id))
    }

    pub fn snapshot_for(&self, session_id: &str) -> Option<&AgentMetricsSnapshot> {
        self.snapshots.get(session_id)
    }

    pub fn set_active_session(&mut self, session_id: String, cx: &mut Context<Self>) {
        if self.active_session.as_deref() == Some(&session_id) {
            return;
        }
        self.active_session = Some(session_id.clone());
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
