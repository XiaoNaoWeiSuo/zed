use std::time::{Duration, Instant};

use agent::run_metrics::AgentRunMetrics;
use chrono::TimeZone as _;
use gpui::{
    Context, Entity, PathBuilder, Pixels, Render, Subscription, Task, Window, canvas, point, px,
};
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
    ring_hovered: bool,
    pie_hovered: bool,
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
            ring_hovered: false,
            pie_hovered: false,
            _metrics_subscription,
            _refresh_task,
        }
    }

    fn refresh_usage(&mut self, cx: &mut Context<Self>) {
        let (running, provider_id) = {
            let metrics = self.metrics.read(cx);
            metrics
                .active_snapshot()
                .map(|snapshot| (snapshot.running, snapshot.provider_id()))
                .unwrap_or((false, None))
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
                let fetched = usage.is_some();
                this.usage = usage;
                if fetched {
                    this.last_usage_fetch = Some(Instant::now());
                } else {
                    // Retry later instead of caching a transient failure.
                    this.usage_provider = None;
                    this.last_usage_fetch = None;
                }
                cx.notify();
            })
            .ok();
        }));
    }

    fn render_chatgpt_gauge(
        &self,
        windows: &[language_model::ProviderUsageWindow],
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let weekly = windows.iter().find(|window| window.label == "Weekly");
        let five_hour = windows.iter().find(|window| window.label == "5h");
        let weekly_remaining = weekly
            .map(|window| (100.0 - window.used_percent).clamp(0.0, 100.0) / 100.0)
            .unwrap_or(0.0);
        let five_hour_remaining = five_hour
            .map(|window| (100.0 - window.used_percent).clamp(0.0, 100.0) / 100.0)
            .unwrap_or(0.0);
        let weekly_reset =
            weekly.and_then(|window| Self::semantic_reset_label(window.resets_at_unix, false));
        let five_hour_reset =
            five_hour.and_then(|window| Self::semantic_reset_label(window.resets_at_unix, true));
        let weekly_tooltip = weekly.map(|window| {
            format!(
                "Weekly limit · {:.1}% remaining{}",
                (100.0 - window.used_percent).max(0.0),
                weekly_reset
                    .as_deref()
                    .map(|reset| format!(" · resets {reset}"))
                    .unwrap_or_default()
            )
        });
        let five_hour_tooltip = five_hour.map(|window| {
            format!(
                "5-hour limit · {:.1}% remaining{}",
                (100.0 - window.used_percent).max(0.0),
                five_hour_reset
                    .as_deref()
                    .map(|reset| format!(" · resets {reset}"))
                    .unwrap_or_default()
            )
        });

        let ring_hovered = self.ring_hovered;
        let pie_hovered = self.pie_hovered;
        let weekly_color = Self::limit_color(weekly_remaining);
        let five_hour_color = Self::limit_color(five_hour_remaining);

        let ring = canvas(
            move |bounds, _, cx| {
                let width = f32::from(bounds.size.width);
                let stroke_width = if ring_hovered { 2.8 } else { 2.0 };
                let center = point(
                    bounds.origin.x + px(width / 2.0),
                    bounds.origin.y + px(width / 2.0),
                );
                let radius = (width - stroke_width) / 2.0;
                let track = arc_path(center, radius, stroke_width, 0.0, 360.0);
                let progress = arc_path(
                    center,
                    radius,
                    stroke_width,
                    -90.0,
                    360.0 * weekly_remaining,
                );
                (
                    track,
                    progress,
                    cx.theme().colors().border.opacity(0.5),
                    weekly_color.color(cx),
                )
            },
            |_bounds, (track, progress, track_color, progress_color), window, _| {
                if let Some(track) = track {
                    window.paint_path(track, track_color);
                }
                if let Some(progress) = progress {
                    window.paint_path(progress, progress_color);
                }
            },
        )
        .size(px(18.));

        let mut gauge = div()
            .id("chatgpt-weekly-gauge")
            .relative()
            .size(px(18.))
            .child(
                div()
                    .id("chatgpt-ring-hover")
                    .size_full()
                    .on_hover(cx.listener(|this, hovered, _, cx| {
                        this.ring_hovered = *hovered;
                        cx.notify();
                    }))
                    .when_some(weekly_tooltip, |this, tooltip| {
                        this.tooltip(Tooltip::text(tooltip))
                    })
                    .child(ring),
            );

        if five_hour.is_some() {
            let pie = canvas(
                move |bounds, _, cx| {
                    let width = f32::from(bounds.size.width);
                    let center = point(
                        bounds.origin.x + px(width / 2.0),
                        bounds.origin.y + px(width / 2.0),
                    );
                    let radius = width / 2.0;
                    let path = pie_path(center, radius, -90.0, 360.0 * five_hour_remaining);
                    (
                        path,
                        five_hour_color
                            .color(cx)
                            .opacity(if pie_hovered { 1.0 } else { 0.75 }),
                    )
                },
                |_bounds, (path, color), window, _| {
                    if let Some(path) = path {
                        window.paint_path(path, color);
                    }
                },
            )
            .size(px(10.));

            gauge = gauge.child(
                div()
                    .id("chatgpt-pie-hover")
                    .absolute()
                    .top(px(4.))
                    .left(px(4.))
                    .size(px(10.))
                    .on_hover(cx.listener(|this, hovered, _, cx| {
                        this.pie_hovered = *hovered;
                        cx.notify();
                    }))
                    .when_some(five_hour_tooltip, |this, tooltip| {
                        this.tooltip(Tooltip::text(tooltip))
                    })
                    .child(pie),
            );
        }

        gauge.into_any_element()
    }

    fn limit_color(remaining: f32) -> Color {
        if remaining <= 0.10 {
            Color::Error
        } else if remaining <= 0.25 {
            Color::Warning
        } else {
            Color::Accent
        }
    }

    fn semantic_reset_label(resets_at_unix: Option<i64>, relative_days: bool) -> Option<String> {
        use chrono::{Datelike as _, Local, TimeZone as _, Timelike as _};
        let reset = Local.timestamp_opt(resets_at_unix?, 0).single()?;
        let period = match reset.hour() {
            0..=5 => "凌晨",
            6..=11 => "上午",
            12..=17 => "下午",
            _ => "晚上",
        };
        if relative_days {
            let today = Local::now().date_naive();
            let days = (reset.date_naive() - today).num_days();
            let day = match days {
                0 => "今天".to_string(),
                1 => "明天".to_string(),
                2 => "后天".to_string(),
                _ => format!("{}月{}日", reset.month(), reset.day()),
            };
            Some(format!(
                "{day}{period} {:02}:{:02}",
                reset.hour(),
                reset.minute()
            ))
        } else {
            Some(format!(
                "{}月{}日 {:02}:{:02}",
                reset.month(),
                reset.day(),
                reset.hour(),
                reset.minute()
            ))
        }
    }

    fn reset_label(resets_at_unix: Option<i64>) -> Option<String> {
        let timestamp = resets_at_unix?;
        let reset = chrono::Local.timestamp_opt(timestamp, 0).single()?;
        Some(format!("Resets {}", reset.format("%Y-%m-%d %H:%M")))
    }
}

impl Render for AgentUsageIndicator {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(snapshot) = self.metrics.read(cx).active_snapshot().cloned() else {
            return div().into_any_element();
        };
        if !snapshot.running && snapshot.model.is_none() {
            return div().into_any_element();
        }
        let cache_hit_rate = snapshot.cache_hit_rate;
        let tokens_per_second = snapshot.tokens_per_second;
        let time_to_first_token = snapshot.time_to_first_token();
        let context = snapshot
            .context_used_tokens
            .zip(snapshot.context_max_tokens)
            .map(|(used, max)| format!("{}/{}", short_tokens(used), short_tokens(max)));

        let mut children: Vec<AnyElement> = Vec::new();
        if let Some(cache_hit_rate) = cache_hit_rate {
            children.push(
                Label::new(format!("Cache {:.1}%", cache_hit_rate * 100.0))
                    .size(LabelSize::XSmall)
                    .into_any_element(),
            );
        }
        if let Some(tokens_per_second) = tokens_per_second {
            children.push(
                Label::new(format!("{tokens_per_second:.0} tok/s"))
                    .size(LabelSize::XSmall)
                    .into_any_element(),
            );
        }
        if let Some(context) = context {
            children.push(
                Label::new(context)
                    .size(LabelSize::XSmall)
                    .into_any_element(),
            );
        }
        if let Some(time_to_first_token) = time_to_first_token {
            children.push(
                Label::new(format!("{:.2}s TTFT", time_to_first_token.as_secs_f32()))
                    .size(LabelSize::XSmall)
                    .into_any_element(),
            );
        }

        if let Some(usage) = &self.usage {
            if let Some(balance) = &usage.balance {
                children.push(
                    Label::new(balance.clone())
                        .size(LabelSize::XSmall)
                        .into_any_element(),
                );
            }
            if snapshot.provider.as_deref() == Some("openai-subscribed")
                && usage
                    .windows
                    .iter()
                    .any(|window| window.label == "5h" || window.label == "Weekly")
            {
                children.push(self.render_chatgpt_gauge(&usage.windows, cx));
            } else {
                for (index, window) in usage.windows.iter().enumerate() {
                    let remaining = (100.0 - window.used_percent).max(0.0);
                    let reset = Self::reset_label(window.resets_at_unix);
                    let label = Label::new(format!("{} {:.0}%", window.label, remaining))
                        .size(LabelSize::XSmall)
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
        }

        h_flex()
            .gap_2()
            .pr_1()
            .children(children)
            .into_any_element()
    }
}

fn short_tokens(tokens: u64) -> String {
    if tokens >= 1_000_000 {
        format!("{:.1}m", tokens as f64 / 1_000_000.0)
    } else if tokens >= 1_000 {
        format!("{:.0}k", tokens as f64 / 1_000.0)
    } else {
        tokens.to_string()
    }
}

fn arc_path(
    center: gpui::Point<Pixels>,
    radius: f32,
    stroke_width: f32,
    start_degrees: f32,
    sweep_degrees: f32,
) -> Option<gpui::Path<Pixels>> {
    let mut builder = PathBuilder::stroke(px(stroke_width));
    let steps = ((sweep_degrees.abs() / 4.0).ceil() as usize).max(1);
    builder.move_to(angle_point(center, radius, start_degrees));
    for step in 1..=steps {
        let angle = start_degrees + sweep_degrees * step as f32 / steps as f32;
        builder.line_to(angle_point(center, radius, angle));
    }
    builder.build().ok()
}

fn pie_path(
    center: gpui::Point<Pixels>,
    radius: f32,
    start_degrees: f32,
    sweep_degrees: f32,
) -> Option<gpui::Path<Pixels>> {
    let mut builder = PathBuilder::fill();
    builder.move_to(center);
    let steps = ((sweep_degrees.abs() / 4.0).ceil() as usize).max(1);
    for step in 0..=steps {
        let angle = start_degrees + sweep_degrees * step as f32 / steps as f32;
        builder.line_to(angle_point(center, radius, angle));
    }
    builder.close();
    builder.build().ok()
}

fn angle_point(center: gpui::Point<Pixels>, radius: f32, degrees: f32) -> gpui::Point<Pixels> {
    let radians = degrees.to_radians();
    point(
        center.x + px(radius * radians.cos()),
        center.y + px(radius * radians.sin()),
    )
}
