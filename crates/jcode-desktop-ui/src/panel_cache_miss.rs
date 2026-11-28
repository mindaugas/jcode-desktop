//! KV (prompt) cache miss notices reported by the daemon.
//!
//! A miss means a large part of the previously cached prompt had to be resent
//! and billed again. Harness-caused misses (the harness mutated the cached
//! prefix) are rendered in the warning tone because they indicate a bug;
//! switches, expiry and documented refreshes are informational.
use super::*;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CacheMissNotice {
    pub(super) reason: String,
    pub(super) harness_caused: bool,
    pub(super) documented_cause: Option<String>,
    pub(super) missed_tokens: u64,
    #[serde(default)]
    pub(super) expected_tokens: u64,
    #[serde(default)]
    pub(super) read_tokens: u64,
    pub(super) message: String,
}

impl CacheMissNotice {
    pub(super) fn from_event(event: &ApiEvent) -> Option<Self> {
        let ApiEvent::KvCacheMiss {
            reason,
            harness_caused,
            missed_tokens,
            expected_tokens,
            read_tokens,
            documented_cause,
            message,
            ..
        } = event
        else {
            return None;
        };
        Some(Self {
            reason: reason.clone(),
            harness_caused: *harness_caused,
            documented_cause: documented_cause.clone(),
            missed_tokens: *missed_tokens,
            expected_tokens: *expected_tokens,
            read_tokens: *read_tokens,
            message: message.clone(),
        })
    }

    /// Unexplained harness mutation of the cached prefix.
    pub(super) fn alarming(&self) -> bool {
        self.harness_caused && self.documented_cause.is_none()
    }

    fn is_refresh(&self) -> bool {
        self.message.contains("KV cache refresh")
    }

    /// Compact lead label matching Jcode TUI (`2.05K KV cache miss`).
    pub(super) fn label(&self) -> String {
        let kind = if self.is_refresh() {
            "KV cache refresh"
        } else {
            "KV cache miss"
        };
        if self.missed_tokens > 0 {
            format!("{} {kind}", format_kv_tokens(self.missed_tokens))
        } else {
            kind.to_string()
        }
    }

    /// Supporting evidence (` · low read · prefix unchanged · 98% hit`).
    pub(super) fn details(&self) -> String {
        let mut parts = Vec::new();
        let reason = self.reason.trim().replace('_', " ");
        if !reason.is_empty() {
            parts.push(reason);
        }
        if self.message.contains("[prefix unchanged]") {
            parts.push("prefix unchanged".to_string());
        }
        if self.expected_tokens > 0 {
            let pct = ((self.read_tokens as f64 / self.expected_tokens as f64) * 100.0)
                .round()
                .clamp(0.0, 100.0);
            parts.push(format!("{pct:.0}% hit"));
        }
        if parts.is_empty() {
            String::new()
        } else {
            format!(" · {}", parts.join(" · "))
        }
    }

    pub(super) fn text(&self) -> String {
        format!("{}{}", self.label(), self.details())
    }
}

fn format_kv_tokens(tokens: u64) -> String {
    if tokens >= 1_000_000 {
        trim_decimal(format!("{:.1}", tokens as f64 / 1e6)) + "M"
    } else if tokens >= 100_000 {
        format!("{}K", (tokens as f64 / 1e3).round() as u64)
    } else if tokens >= 10_000 {
        trim_decimal(format!("{:.1}", tokens as f64 / 1e3)) + "K"
    } else if tokens >= 1_000 {
        trim_decimal(format!("{:.2}", tokens as f64 / 1e3)) + "K"
    } else {
        tokens.to_string()
    }
}

fn trim_decimal(mut s: String) -> String {
    if s.contains('.') {
        while s.ends_with('0') {
            s.pop();
        }
        if s.ends_with('.') {
            s.pop();
        }
    }
    s
}

impl Panel {
    pub(super) fn render_cache_miss_notice(
        &self,
        index: usize,
        notice: &CacheMissNotice,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let theme = Theme::global();
        let label_color = if notice.is_refresh() {
            theme.TEXT_DIM
        } else if notice.missed_tokens >= 50_000 {
            theme.ERROR
        } else if notice.missed_tokens >= 10_000 || notice.alarming() {
            theme.WARN
        } else {
            theme.TEXT_DIM
        };
        let details = notice.details();
        div()
            .debug_selector(|| "kv-cache-miss-notice".into())
            .flex()
            .items_center()
            .min_w_0()
            .px_1()
            .py(px(1.))
            .text_size(px(11.))
            .text_color(theme.TEXT_DIM)
            .child(div().flex_none().text_color(label_color).child(notice.label()))
            .when(!details.is_empty(), |el| {
                el.child(div().min_w_0().flex_1().truncate().child(
                    text_selection::plain(
                        self.transcript_selection.clone(),
                        format!("{index}-kv-cache-miss"),
                        details,
                        window,
                        cx,
                    ),
                ))
            })
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn miss(harness_caused: bool, documented: Option<&str>) -> ApiEvent {
        ApiEvent::KvCacheMiss {
            session_id: "cache".into(),
            reason: "prefix_changed".into(),
            harness_caused,
            missed_tokens: 46_000,
            expected_tokens: 50_000,
            read_tokens: 4_000,
            documented_cause: documented.map(str::to_string),
            message: "KV cache miss: ~46K tokens resent (an earlier message was modified).".into(),
        }
    }

    #[test]
    fn only_undocumented_harness_misses_are_alarming() {
        let alarm = CacheMissNotice::from_event(&miss(true, None)).unwrap();
        assert!(alarm.alarming());
        let documented = CacheMissNotice::from_event(&miss(true, Some("config reload"))).unwrap();
        assert!(!documented.alarming());
        let expired = CacheMissNotice::from_event(&miss(false, None)).unwrap();
        assert!(!expired.alarming());
    }

    #[test]
    fn formats_compact_tui_style_summary() {
        let notice = CacheMissNotice::from_event(&ApiEvent::KvCacheMiss {
            session_id: "cache".into(),
            reason: "low_read".into(),
            harness_caused: false,
            missed_tokens: 2_050,
            expected_tokens: 84_300,
            read_tokens: 82_300,
            documented_cause: None,
            message: "KV cache miss [low_read] [prefix unchanged]: ~2.05K tokens resent".into(),
        })
        .unwrap();
        assert_eq!(notice.label(), "2.05K KV cache miss");
        assert_eq!(notice.details(), " · low read · prefix unchanged · 98% hit");
        assert_eq!(
            notice.text(),
            "2.05K KV cache miss · low read · prefix unchanged · 98% hit"
        );
    }

    #[gpui::test]
    fn kv_cache_miss_event_appends_visible_transcript_notice(cx: &mut gpui::TestAppContext) {
        let (workspace, vcx) = cx.add_window_view(|_, cx| {
            let mut workspace =
                crate::workspace::Workspace::for_test(crate::learning::Coach::new(), cx);
            workspace.push_test_panel("cache", cx);
            workspace
        });
        let panel = workspace.read_with(vcx, |workspace, _| workspace.test_panel(0).unwrap());
        panel.update(vcx, |panel, cx| {
            panel.items.clear();
            panel.history_loaded = true;
            panel.apply(
                &ApiEvent::TextDelta {
                    session_id: "cache".into(),
                    text: "partial answer".into(),
                    message_id: None,
                },
                cx,
            );
            panel.apply(&miss(true, None), cx);
        });
        vcx.run_until_parked();
        panel.read_with(vcx, |panel, _| {
            assert!(
                matches!(panel.items.last(), Some(Item::CacheMiss(notice)) if notice.alarming())
            );
            assert!(
                panel
                    .items
                    .iter()
                    .any(|item| matches!(item, Item::Assistant(text) if text == "partial answer")),
                "streamed text must settle before the notice"
            );
        });
        assert!(
            vcx.debug_bounds("kv-cache-miss-notice").is_some(),
            "notice must paint in the transcript"
        );
    }
}
