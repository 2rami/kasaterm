use crate::{App, GridCell};

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Facts {
    pub(crate) model: String,
    pub(crate) effort: String,
    pub(crate) mode: String,
    pub(crate) context: Option<f64>,
}

impl Facts {
    fn remote(row: &serde_json::Value) -> Self {
        if row["state_fresh"] == false || row["harness"].as_str().is_none_or(|s| s.is_empty()) {
            return Self::default();
        }
        let text = |key: &str| row[key].as_str().unwrap_or_default().to_string();
        Self {
            model: row["model"]
                .as_str()
                .filter(|s| !s.is_empty())
                .or_else(|| row["model_label"].as_str())
                .unwrap_or_default()
                .to_string(),
            effort: text("effort"),
            mode: text("collaboration_mode"),
            // 구형 보드의 0은 사용량 없음과 미상을 구분하지 않는다.
            context: row["context_pct"].as_f64().filter(|p| *p > 0.0),
        }
    }

    fn parts(&self, live_context: Option<f64>) -> Vec<String> {
        let mut parts: Vec<_> = [&self.model, &self.effort]
            .into_iter()
            .map(|s| s.split_whitespace().collect::<Vec<_>>().join(" "))
            .filter(|s| !s.is_empty())
            .collect();
        if let Some(pct) = live_context
            .or(self.context)
            .filter(|p| p.is_finite() && (0.0..=100.0).contains(p))
        {
            parts.push(format!("컨텍스트 {pct:.0}%"));
        }
        let mode = self.mode.split_whitespace().collect::<Vec<_>>().join(" ");
        if !mode.is_empty() {
            parts.push(mode);
        }
        parts
    }
}

#[derive(Default)]
pub(crate) struct StatusLine {
    pub(crate) machine: String,
    pub(crate) icon: String,
    pub(crate) facts: Facts,
    pub(crate) session: Option<String>,
}

impl StatusLine {
    pub(super) fn live_context(&self, live: &super::live::ModLive) -> Option<f64> {
        (live.live
            && self
                .session
                .as_deref()
                .is_some_and(|s| !s.is_empty() && s == live.session))
        .then_some(live.context_percent)
        .flatten()
    }

    pub(crate) fn fit(
        &self,
        width: f32,
        context: Option<f64>,
        mut measure: impl FnMut(&str) -> f32,
    ) -> String {
        if width <= 0.0 {
            return String::new();
        }
        let machine = self
            .machine
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        let mut parts = vec![machine.clone()];
        for part in self.facts.parts(context) {
            parts.push(part);
            if measure(&parts.join(" · ")) > width {
                parts.pop();
                break;
            }
        }
        let text = parts.join(" · ");
        if measure(&text) <= width {
            return text;
        }
        if measure("…") > width {
            return String::new();
        }
        let mut short = String::new();
        for ch in machine.chars() {
            let candidate = format!("{short}{ch}…");
            if measure(&candidate) > width {
                break;
            }
            short.push(ch);
        }
        short.push('…');
        short
    }
}

type Rect = (f32, f32, f32, f32);

pub(super) struct Footer {
    pub(super) send: Rect,
    pub(super) stop: Option<Rect>,
    pub(super) terminal: Option<Rect>,
    pub(super) terminal_label: bool,
    pub(super) status: Rect,
}

pub(super) fn footer(
    left: f32,
    right: f32,
    mid: f32,
    working: bool,
    terminal_width: f32,
    status_min: f32,
) -> Footer {
    let send = (right - 28.0, mid - 14.0, 28.0, 28.0);
    let stop = (working && send.0 - 34.0 >= left).then_some((send.0 - 34.0, send.1, 28.0, 28.0));
    let edge = stop.map_or(send.0, |s| s.0) - 10.0;
    let room = (edge - left).max(0.0);
    let terminal_label = room >= terminal_width + 10.0 + status_min;
    let width = if terminal_label { terminal_width } else { 29.0 };
    let terminal = (room >= width).then_some((left, mid - 13.0, width, 26.0));
    let x = terminal.map_or(left, |r| r.0 + r.2 + 10.0);
    Footer {
        send,
        stop,
        terminal,
        terminal_label,
        status: (x, mid - 6.5, (edge - x).max(0.0), 13.0),
    }
}

impl App {
    pub(crate) fn chat_status_facts(
        &self,
        tab: &str,
        cells: &[Vec<GridCell>],
        reported: Option<&(String, String)>,
    ) -> Facts {
        // 거울에는 같은 번호의 로컬 캐시나 기본 계정 값을 섞지 않는다.
        if kasa_mcp::remote::is_remote_pane(tab) {
            return kasa_mcp::remote::cached_fresh_pane(tab)
                .map(|row| Facts::remote(&row))
                .unwrap_or_default();
        }
        let Some(kind) = self.pty.get(tab).and_then(|p| p.active_agent()) else {
            return Facts::default();
        };
        let (model, effort) = reported.cloned().unwrap_or_default();
        let mut facts = Facts {
            model,
            effort,
            ..Facts::default()
        };
        if kind == kasa_pty::AgentKind::Codex {
            if let Some(snapshot) = self
                .socket_backend
                .as_ref()
                .and_then(|b| b.codex_rollout_snapshot(tab))
            {
                facts.model = snapshot.model;
                facts.effort = snapshot.effort;
                facts.mode = snapshot.collaboration_mode;
            }
            if let Some(line) = crate::screenread::codex_status_line(cells) {
                facts.model = line.model;
                facts.effort = line.effort;
                facts.context = line
                    .context
                    .and_then(|s| s.trim_end_matches('%').parse().ok());
            }
        }
        facts
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_context_requires_the_reported_current_session() {
        let mut line = line();
        let mut live = super::super::live::ModLive {
            live: true,
            session: "new-session".into(),
            context_percent: Some(0.0),
            ..Default::default()
        };
        assert_eq!(line.live_context(&live), None);
        line.session = Some("old-session".into());
        assert_eq!(line.live_context(&live), None);
        line.session = Some("new-session".into());
        assert_eq!(line.live_context(&live), Some(0.0));
        live.live = false;
        assert_eq!(line.live_context(&live), None);
    }

    fn line() -> StatusLine {
        StatusLine {
            machine: "원격 맥미니".into(),
            icon: "server".into(),
            session: None,
            facts: Facts {
                model: "gpt-6.1-sol".into(),
                effort: "xhigh".into(),
                context: Some(32.0),
                mode: "plan".into(),
            },
        }
    }

    fn width(s: &str) -> f32 {
        s.chars().count() as f32
    }

    #[test]
    fn device_survives_when_status_fields_do_not_fit() {
        let line = line();
        assert_eq!(
            line.fit(100.0, None, width),
            "원격 맥미니 · gpt-6.1-sol · xhigh · 컨텍스트 32% · plan"
        );
        assert_eq!(line.fit(20.0, None, width), "원격 맥미니 · gpt-6.1-sol");
        assert_eq!(line.fit(8.0, None, width), "원격 맥미니");
        assert_eq!(line.fit(3.0, None, width), "원격…");
        for budget in 0..100 {
            assert!(width(&line.fit(budget as f32, None, width)) <= budget as f32);
        }
    }

    #[test]
    fn missing_values_are_not_guessed_and_live_context_can_be_zero() {
        let line = StatusLine {
            machine: "이 기기".into(),
            ..StatusLine::default()
        };
        assert_eq!(line.fit(100.0, None, width), "이 기기");
        assert_eq!(line.fit(100.0, Some(0.0), width), "이 기기 · 컨텍스트 0%");
        for invalid in [f64::NAN, f64::INFINITY, -1.0, 101.0] {
            assert_eq!(line.fit(100.0, Some(invalid), width), "이 기기");
        }
    }

    #[test]
    fn stale_or_stopped_remote_agents_do_not_keep_old_model_status() {
        let row = serde_json::json!({"harness":"claude", "model":"opus", "effort":"high", "context_pct":51});
        assert_eq!(
            Facts::remote(&row).parts(None),
            ["opus", "high", "컨텍스트 51%"]
        );
        for patch in [
            serde_json::json!({"state_fresh":false}),
            serde_json::json!({"harness":null}),
        ] {
            let mut stale = row.clone();
            for (key, value) in patch.as_object().unwrap() {
                stale[key] = value.clone();
            }
            assert_eq!(Facts::remote(&stale), Facts::default());
        }
        assert_eq!(Facts::remote(&serde_json::json!({})), Facts::default());
    }

    #[test]
    fn labels_cannot_add_a_second_line() {
        let line = StatusLine {
            machine: "서버\n이름".into(),
            facts: Facts {
                model: "model\r\nname".into(),
                ..Facts::default()
            },
            ..StatusLine::default()
        };
        assert_eq!(line.fit(100.0, None, width), "서버 이름 · model name");
    }

    #[test]
    fn footer_controls_and_status_do_not_overlap_at_narrow_widths() {
        for working in [false, true] {
            for available in 48..900 {
                let f = footer(12.0, 12.0 + available as f32, 32.0, working, 116.0, 63.0);
                let mut rects: Vec<_> = f
                    .terminal
                    .into_iter()
                    .chain((f.status.2 > 0.0).then_some(f.status))
                    .chain(f.stop)
                    .chain(Some(f.send))
                    .collect();
                rects.sort_by(|a, b| a.0.total_cmp(&b.0));
                for r in &rects {
                    assert!(r.0 >= 12.0 && r.0 + r.2 <= 12.0 + available as f32);
                }
                for pair in rects.windows(2) {
                    assert!(pair[0].0 + pair[0].2 <= pair[1].0);
                }
            }
        }
        assert!(!footer(0.0, 220.0, 32.0, true, 116.0, 63.0).terminal_label);
        assert!(footer(0.0, 400.0, 32.0, true, 116.0, 63.0).terminal_label);
    }
}
