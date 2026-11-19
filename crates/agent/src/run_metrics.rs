use std::collections::HashMap;
use std::time::{Duration, Instant};

use db::kvp::KeyValueStore;
use gpui::{App, AppContext as _, Context, Entity, Global, WindowId};
use language_model::LanguageModelProviderId;
use serde::{Deserialize, Serialize};

const METRICS_KEY_PREFIX: &str = "agent-metrics/";
pub const METRICS_VERSION: u32 = 3;
const LEGACY_MAX_TOKENS_PER_SECOND: f32 = 5_000.0;
/// A persisted rate above this can only come from corrupt data, not a real
/// generation speed. This is not a performance cap.
const MAX_PERSISTED_TOKENS_PER_SECOND: f32 = 1_000_000.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RequestPhase {
    Preparing,
    Compacting,
    Connecting,
    AwaitingOutput,
    Thinking,
    Streaming,
    Retrying,
    Tools,
}

#[derive(Default)]
pub(crate) struct TurnLatency {
    started_at: Option<Instant>,
    preparation_ms: Option<u64>,
    response_ms: Option<u64>,
    first_activity_ms: Option<u64>,
    first_text_ms: Option<u64>,
    request_attempts: u32,
    phase: Option<RequestPhase>,
}

impl TurnLatency {
    pub(crate) fn start(&mut self, now: Instant) {
        *self = Self {
            started_at: Some(now),
            phase: Some(RequestPhase::Preparing),
            ..Self::default()
        };
    }

    fn elapsed_ms(&self, now: Instant) -> Option<u64> {
        self.started_at.map(|started_at| {
            now.saturating_duration_since(started_at)
                .as_millis()
                .min(u64::MAX as u128) as u64
        })
    }

    pub(crate) fn begin_request(&mut self, now: Instant) {
        self.request_attempts = self.request_attempts.saturating_add(1);
        if self.preparation_ms.is_none() {
            self.preparation_ms = self.elapsed_ms(now);
        }
        self.phase = Some(RequestPhase::Connecting);
    }

    pub(crate) fn response_received(&mut self, now: Instant) {
        if self.response_ms.is_none() {
            self.response_ms = self.elapsed_ms(now);
        }
        self.phase = Some(RequestPhase::AwaitingOutput);
    }

    pub(crate) fn note_activity(&mut self, now: Instant) {
        if self.first_activity_ms.is_none() {
            self.first_activity_ms = self.elapsed_ms(now);
        }
    }

    pub(crate) fn note_text(&mut self, now: Instant) -> bool {
        self.note_activity(now);
        let first_text = self.first_text_ms.is_none();
        if first_text {
            self.first_text_ms = self.elapsed_ms(now);
        }
        self.phase = Some(RequestPhase::Streaming);
        first_text
    }

    pub(crate) fn set_phase(&mut self, phase: RequestPhase) {
        self.phase = Some(phase);
    }

    pub(crate) fn populate(&self, snapshot: &mut AgentMetricsSnapshot) {
        snapshot.preparation_ms = self.preparation_ms;
        snapshot.response_ms = self.response_ms;
        snapshot.first_activity_ms = self.first_activity_ms;
        snapshot.time_to_first_token_ms = self.first_text_ms;
        snapshot.request_attempts = self.request_attempts;
        snapshot.phase = self.phase;
    }
}

/// Metrics for the most recent turn in one agent session.
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
    /// Time from turn start to the first nonempty text, excluding reasoning.
    pub time_to_first_token_ms: Option<u64>,
    #[serde(default)]
    pub preparation_ms: Option<u64>,
    #[serde(default)]
    pub response_ms: Option<u64>,
    #[serde(default)]
    pub first_activity_ms: Option<u64>,
    #[serde(default)]
    pub request_attempts: u32,
    #[serde(default)]
    pub phase: Option<RequestPhase>,
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

    fn sanitize_persisted(&mut self) {
        self.running = false;
        self.phase = None;
        if self.version < METRICS_VERSION {
            self.time_to_first_token_ms = None;
        }
        let has_corrupt_rate = self.tokens_per_second.is_some_and(|rate| {
            !rate.is_finite() || rate <= 0.0 || rate > MAX_PERSISTED_TOKENS_PER_SECOND
        });
        let has_legacy_rate = self.version < 2
            && self
                .tokens_per_second
                .is_some_and(|rate| rate > LEGACY_MAX_TOKENS_PER_SECOND);
        if has_corrupt_rate || has_legacy_rate {
            self.tokens_per_second = None;
        }
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
                snapshot.sanitize_persisted();
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

    #[test]
    fn turn_latency_preserves_first_events_across_retries() {
        let start = Instant::now();
        let mut latency = TurnLatency::default();
        let mut snapshot = AgentMetricsSnapshot::default();
        latency.start(start);
        latency.begin_request(start + Duration::from_millis(100));
        latency.set_phase(RequestPhase::Retrying);
        latency.begin_request(start + Duration::from_millis(300));
        latency.response_received(start + Duration::from_millis(400));
        latency.note_text(start + Duration::from_millis(600));
        latency.begin_request(start + Duration::from_millis(700));
        latency.response_received(start + Duration::from_millis(800));
        latency.note_text(start + Duration::from_millis(900));
        latency.populate(&mut snapshot);

        assert_eq!(snapshot.preparation_ms, Some(100));
        assert_eq!(snapshot.response_ms, Some(400));
        assert_eq!(snapshot.first_activity_ms, Some(600));
        assert_eq!(snapshot.time_to_first_token_ms, Some(600));
        assert_eq!(snapshot.request_attempts, 3);
        assert_eq!(snapshot.phase, Some(RequestPhase::Streaming));
    }

    #[test]
    fn thinking_is_activity_but_not_first_text() {
        let start = Instant::now();
        let mut latency = TurnLatency::default();
        let mut snapshot = AgentMetricsSnapshot {
            time_to_first_token_ms: Some(1),
            ..Default::default()
        };
        latency.start(start);
        latency.begin_request(start + Duration::from_millis(100));
        latency.populate(&mut snapshot);
        assert_eq!(snapshot.phase, Some(RequestPhase::Connecting));
        latency.response_received(start + Duration::from_millis(200));
        latency.populate(&mut snapshot);
        assert_eq!(snapshot.phase, Some(RequestPhase::AwaitingOutput));
        latency.note_activity(start + Duration::from_millis(300));
        latency.set_phase(RequestPhase::Thinking);
        latency.populate(&mut snapshot);
        assert_eq!(snapshot.first_activity_ms, Some(300));
        assert_eq!(snapshot.time_to_first_token_ms, None);
        assert_eq!(snapshot.phase, Some(RequestPhase::Thinking));

        latency.note_text(start + Duration::from_millis(1_000));
        latency.populate(&mut snapshot);
        assert_eq!(snapshot.first_activity_ms, Some(300));
        assert_eq!(snapshot.time_to_first_token_ms, Some(1_000));
    }

    #[test]
    fn starting_next_turn_clears_latency() {
        let start = Instant::now();
        let mut latency = TurnLatency::default();
        let mut snapshot = AgentMetricsSnapshot::default();
        latency.start(start);
        latency.begin_request(start + Duration::from_millis(100));
        latency.response_received(start + Duration::from_millis(200));
        latency.note_text(start + Duration::from_millis(300));
        latency.populate(&mut snapshot);

        let next_start = start + Duration::from_secs(1);
        latency.start(next_start);
        latency.populate(&mut snapshot);
        assert_eq!(snapshot.preparation_ms, None);
        assert_eq!(snapshot.response_ms, None);
        assert_eq!(snapshot.first_activity_ms, None);
        assert_eq!(snapshot.time_to_first_token_ms, None);
        assert_eq!(snapshot.request_attempts, 0);
        assert_eq!(snapshot.phase, Some(RequestPhase::Preparing));

        latency.begin_request(next_start + Duration::from_millis(50));
        latency.note_text(next_start + Duration::from_millis(150));
        latency.populate(&mut snapshot);
        assert_eq!(snapshot.preparation_ms, Some(50));
        assert_eq!(snapshot.time_to_first_token_ms, Some(150));
        assert_eq!(snapshot.request_attempts, 1);
    }

    #[test]
    fn persisted_metrics_clear_legacy_first_token_time_only() {
        let mut snapshot = AgentMetricsSnapshot {
            version: 2,
            running: true,
            time_to_first_token_ms: Some(100),
            tokens_per_second: Some(6_000.0),
            ..Default::default()
        };
        snapshot.sanitize_persisted();
        assert!(!snapshot.running);
        assert_eq!(snapshot.time_to_first_token_ms, None);
        assert_eq!(snapshot.tokens_per_second, Some(6_000.0));

        snapshot.version = METRICS_VERSION;
        snapshot.time_to_first_token_ms = Some(200);
        snapshot.sanitize_persisted();
        assert_eq!(snapshot.time_to_first_token_ms, Some(200));

        snapshot.version = 1;
        snapshot.sanitize_persisted();
        assert_eq!(snapshot.tokens_per_second, None);
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
