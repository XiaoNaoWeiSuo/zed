use std::time::{Duration, Instant};

use agent::run_metrics::AgentRunMetrics;
use chrono::TimeZone as _;
use gpui::{Context, Entity, Render, Subscription, Task, Window};
use language_model::{LanguageModelProviderId, LanguageModelRegistry, ProviderUsage};
use ui::{Tooltip, prelude::*};

const USAGE_REFRESH_INTERVAL: Duration = Duration::from_secs(60);

/// Compact agent throughput metrics and the active provider's usage limits.
pub struct AgentUsageIndicator {
    metrics: Entity<AgentRunMetrics>,
    usage: Option<ProviderUsage>,
    usage_provider: Option<LanguageModelProviderId>,
    last_usage_fetch: Option<Instant>,
    fetch_task: Option<Task<()>>,
    _metrics_subscription: Subscription,
    _refresh_task: Task<()>,
}

impl AgentUsageIndicator {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let metrics = AgentRunMetrics::global(cx);
        let _metrics_subscription = cx.observe(&metrics, |this, _, cx| {
            this.refresh_usage(cx);
            cx.notify();
        });

        let _refresh_task = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(USAGE_REFRESH_INTERVAL).await;
                if this.update(cx, |this, cx| this.refresh_usage(cx)).is_err() {
                    break;
                }
            }
        });

        Self {
            metrics,
            usage: None,
            usage_provider: None,
            last_usage_fetch: None,
            fetch_task: None,
            _metrics_subscription,
            _refresh_task,
        }
    }

    fn refresh_usage(&mut self, cx: &mut Context<Self>) {
        let (running, provider_id) = {
            let metrics = self.metrics.read(cx);
            (metrics.running, metrics.provider.clone())
        };
        let Some(provider_id) = provider_id else {
            return;
        };

        let provider_changed = self.usage_provider.as_ref() != Some(&provider_id);
        if !running && !provider_changed {
            return;
        }
        let cache_is_fresh = !provider_changed
            && self
                .last_usage_fetch
                .is_some_and(|fetched| fetched.elapsed() < USAGE_REFRESH_INTERVAL);
        if cache_is_fresh {
            return;
        }

        let Some(provider) = LanguageModelRegistry::read_global(cx).provider(&provider_id) else {
            return;
        };
        if provider_changed {
            self.usage = None;
        }
        let usage_task = provider.usage(cx);
        self.usage_provider = Some(provider_id);
        self.fetch_task = Some(cx.spawn(async move |this, cx| {
            let usage = usage_task.await;
            this.update(cx, |this, cx| {
                this.usage = usage;
                this.last_usage_fetch = Some(Instant::now());
                cx.notify();
            })
            .ok();
        }));
    }

    fn reset_label(resets_at_unix: Option<i64>) -> Option<String> {
        let timestamp = resets_at_unix?;
        let reset = chrono::Local.timestamp_opt(timestamp, 0).single()?;
        Some(format!("Resets {}", reset.format("%Y-%m-%d %H:%M")))
    }
}

impl Render for AgentUsageIndicator {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (running, model, cache_hit_rate, tokens_per_second, time_to_first_token) = {
            let metrics = self.metrics.read(cx);
            (
                metrics.running,
                metrics.model.clone(),
                metrics.cache_hit_rate,
                metrics.tokens_per_second,
                metrics.time_to_first_token,
            )
        };
        if !running && model.is_none() {
            return div().into_any_element();
        }

        let mut children: Vec<AnyElement> = Vec::new();
        if let Some(cache_hit_rate) = cache_hit_rate {
            children.push(
                Label::new(format!("Cache {:.0}%", cache_hit_rate * 100.0))
                    .size(LabelSize::Small)
                    .color(Color::Muted)
                    .into_any_element(),
            );
        }
        if let Some(tokens_per_second) = tokens_per_second {
            children.push(
                Label::new(format!("{tokens_per_second:.0} tok/s"))
                    .size(LabelSize::Small)
                    .color(Color::Muted)
                    .into_any_element(),
            );
        }
        if let Some(time_to_first_token) = time_to_first_token {
            children.push(
                Label::new(format!("{:.2}s TTFT", time_to_first_token.as_secs_f32()))
                    .size(LabelSize::Small)
                    .color(Color::Muted)
                    .into_any_element(),
            );
        }

        if let Some(usage) = &self.usage {
            if let Some(balance) = &usage.balance {
                children.push(
                    Label::new(balance.clone())
                        .size(LabelSize::Small)
                        .color(Color::Muted)
                        .into_any_element(),
                );
            }
            for (index, window) in usage.windows.iter().enumerate() {
                let remaining = (100.0 - window.used_percent).max(0.0);
                let reset = Self::reset_label(window.resets_at_unix);
                let label = Label::new(format!("{} {:.0}%", window.label, remaining))
                    .size(LabelSize::Small)
                    .color(if remaining <= 10.0 {
                        Color::Error
                    } else if remaining <= 25.0 {
                        Color::Warning
                    } else {
                        Color::Muted
                    });
                let element = div().id(("usage-window", index)).child(label);
                children.push(match reset {
                    Some(reset) => element.tooltip(Tooltip::text(reset)).into_any_element(),
                    None => element.into_any_element(),
                });
            }
        }

        h_flex()
            .gap_2()
            .pr_1()
            .children(children)
            .into_any_element()
    }
}
