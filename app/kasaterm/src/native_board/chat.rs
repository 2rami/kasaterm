use super::assistant::{key_present, rows, string};
pub(crate) use super::assistant::{ChatSnapshot, ChatState};
use super::*;

use super::route::{self, HoldReason, RouteBasis, RouteState, RouteTarget};

fn send_label(state: &RouteState) -> String {
    match state.target() {
        Some(RouteTarget::Student(student)) => format!("{}에게 보내기", student.name),
        Some(RouteTarget::NewTask) => "새 일 맡기기".into(),
        _ => "나쵸에게 묻기".into(),
    }
}

fn target_parts(target: &RouteTarget, s: &Snapshot) -> (String, String) {
    match target {
        RouteTarget::Student(student) => {
            let work = s.data.overview.panes.iter()
                .find(|row| row.address.machine_id == student.machine_id && row.address.surface_id == student.surface_id)
                .map(|row| board_plain(&row.title, 60))
                .unwrap_or_default();
            (student.name.clone(), work)
        }
        RouteTarget::NewTask => ("새 일".into(), "맡을 학생은 디스패처가 정해요".into()),
        RouteTarget::Nacho => ("나쵸".into(), "일이 아니라 질문으로 받아요".into()),
    }
}

/// 입력칸 아래 받는 곳 줄(26) — 「→ 받는 곳 · 지금 일 · 근거」, 확신이 낮으면 후보 칩.
fn paint_route_line(g: &mut gpu::GpuRenderer, s: &Snapshot, hits: &mut Vec<Hit>, x: f32, y: f32, w: f32) {
    let right = x + w;
    let mut cx = x;
    let put = |g: &mut gpu::GpuRenderer, cx: &mut f32, value: &str, size: f32, color: [u8; 4], bold: bool| {
        let shown = fit(g, value, (right - *cx).max(0.0), size, bold);
        if shown.is_empty() {
            return;
        }
        text(g, *cx, y + (26.0 - size) / 2.0 - 1.0, &shown, size, color, bold);
        *cx += g.measure_chrome_text(&shown, size, bold) + 8.0;
    };
    put(g, &mut cx, "→", 12.0, theme::text_mute(), false);
    let pending = if s.route_pending { " · 다시 보는 중" } else { "" };
    match &s.route {
        RouteState::Empty => put(g, &mut cx, "받는 곳은 치는 동안 정해져요 · Enter 보내기 · Shift+Enter 줄바꿈", 10.5, theme::text_mute(), false),
        RouteState::Hold(HoldReason::TooShort) => put(g, &mut cx, "조금 더 쓰면 받는 곳을 정해요", 10.5, theme::text_mute(), false),
        RouteState::Hold(HoldReason::UnknownName(name)) => put(g, &mut cx, &format!("「{name}」 학생이 판에 없어요"), 10.5, theme::attention(), false),
        RouteState::Thinking => put(g, &mut cx, "받는 곳을 보는 중…", 10.5, theme::text_mute(), false),
        RouteState::Failed(reason) => put(g, &mut cx, reason, 10.5, theme::attention(), false),
        RouteState::Decided { target, basis, .. } => {
            let (who, work) = target_parts(target, s);
            put(g, &mut cx, &who, 12.0, theme::text(), true);
            if !work.is_empty() {
                put(g, &mut cx, &work, 11.0, theme::text_dim(), false);
            }
            let why = match basis {
                RouteBasis::Mention => "@이름".to_owned(),
                RouteBasis::Command => "명령".to_owned(),
                RouteBasis::Selected => "지금 보는 창".to_owned(),
                RouteBasis::LastSent => "방금 보낸 학생".to_owned(),
                RouteBasis::NamedInText => "글 속 이름".to_owned(),
                RouteBasis::Picked => "직접 고름".to_owned(),
                RouteBasis::Jev { probability, latency_ms } => format!("제브 {} · {latency_ms}ms", (probability * 100.0).round() as u32),
            };
            put(g, &mut cx, &format!("{why}{pending}"), 10.5, theme::text_mute(), false);
        }
        RouteState::Pick { options, latency_ms } => {
            put(g, &mut cx, "어디로 갈까요?", 10.5, theme::text_dim(), false);
            let twins = |name: &str| options.iter().filter(|(t, _)| matches!(t, RouteTarget::Student(o) if o.name == name)).count() > 1;
            for (index, (target, probability)) in options.iter().enumerate() {
                let (mut who, _) = target_parts(target, s);
                // 같은 학생 이름이 두 기기에 있으면 기기 이름이 없인 칩을 못 가른다.
                if let RouteTarget::Student(student) = target {
                    if twins(&student.name) {
                        who = format!("{}@{}", student.name, student.machine_label);
                    }
                }
                let label = format!("{who} {}", (probability * 100.0).round() as u32);
                let cw = g.measure_chrome_text(&label, 11.0, false) + 16.0;
                if cx + cw > right {
                    break;
                }
                let rect = (cx, y + 2.0, cw, 22.0);
                let hover = contains(rect, s.cursor);
                stroke(g, rect, if index == 0 { theme::accent() } else if hover { theme::text_dim() } else { theme::border() });
                text(g, rect.0 + 8.0, rect.1 + 4.0, &label, 11.0, if index == 0 { theme::accent() } else { theme::text_dim() }, false);
                hit(g, hits, Target::RouteChoose(index), rect, false);
                g.hover_pointer |= hover;
                cx += cw + 6.0;
            }
            if let Some(ms) = latency_ms {
                put(g, &mut cx, &format!("제브 · {ms}ms{pending}"), 10.5, theme::text_mute(), false);
            }
        }
    }
}

/// 대화 끝에 붙는 줄 — 이 판에서 보낸 지시와 지금 사람을 기다리는 학생 보고.
struct Tail {
    head: String,
    head_color: [u8; 4],
    lines: Vec<String>,
    foot: Option<(String, [u8; 4])>,
    buttons: Vec<(&'static str, Target)>,
    report: bool,
}

impl Tail {
    fn height(&self) -> f32 {
        18.0 + self.lines.len() as f32 * 18.0
            + if self.foot.is_some() { 18.0 } else { 0.0 }
            + if self.buttons.is_empty() { 0.0 } else { 32.0 }
            + 16.0
    }

    fn paint(&self, g: &mut gpu::GpuRenderer, s: &Snapshot, hits: &mut Vec<Hit>, x: f32, top: f32, w: f32) {
        let inset = if self.report { 12.0 } else { 10.0 };
        if self.report {
            g.rect(x + 2.0, top, 2.0, self.height() - 16.0, theme::border());
        }
        let head = fit(g, &self.head, (w - inset - 10.0).max(0.0), 10.5, false);
        text(g, x + inset, top, &head, 10.5, self.head_color, false);
        let mut y = top + 18.0;
        for line in &self.lines {
            text(g, x + inset, y, line, 12.0, theme::text(), false);
            y += 18.0;
        }
        if let Some((foot, color)) = &self.foot {
            let foot = fit(g, foot, (w - inset - 10.0).max(0.0), 10.5, false);
            text(g, x + inset, y, &foot, 10.5, *color, false);
            y += 18.0;
        }
        let mut bx = x + inset;
        for (label, target) in &self.buttons {
            let bw = g.measure_chrome_text(label, 12.0, false) + 20.0;
            button(g, s, hits, (bx, y + 6.0, bw, 26.0), label, target.clone(), false);
            bx += bw + 6.0;
        }
    }
}

fn tail_rows(g: &mut gpu::GpuRenderer, s: &Snapshot, w: f32) -> Vec<Tail> {
    let measure = |g: &mut gpu::GpuRenderer, value: &str| {
        wrap(value, (w - 22.0).max(1.0), |text| g.measure_chrome_text(text, 12.0, false)).into_iter().take(6).collect::<Vec<_>>()
    };
    let mut tails = Vec::new();
    for note in s.sent.iter() {
        let (foot, color) = match &note.receipt {
            None => ("보내는 중…".to_owned(), theme::text_mute()),
            Some((true, receipt)) => (receipt.clone(), theme::text_mute()),
            Some((false, error)) => (error.clone(), theme::danger()),
        };
        tails.push(Tail { head: format!("나 → {}", note.to), head_color: theme::accent(), lines: measure(g, &note.body), foot: Some((foot, color)), buttons: Vec::new(), report: false });
    }
    let mirror = |row: &OverviewPane| row.status_reason.as_deref() == Some(crate::socket::REMOTE_MIRROR_REASON);
    for row in s.data.overview.panes.iter().filter(|row| !mirror(row)) {
        let Some(name) = row.character.as_deref().filter(|name| !name.is_empty()) else { continue };
        let (state, rank) = overview_status(row);
        if rank != 0 {
            continue;
        }
        let body = row.done_summary.as_deref().filter(|v| !v.is_empty()).unwrap_or(&row.progress);
        let mut buttons = vec![("답하기", Target::RouteReply(name.to_owned()))];
        if overview_is_local(&s.data.overview, &row.address) {
            buttons.push(("창으로 가기", Target::OverviewFocus(row.address.clone())));
        }
        tails.push(Tail {
            head: format!("{name} · {} · {state}", board_plain(&row.title, 40)),
            head_color: theme::attention(),
            lines: measure(g, &board_plain(if body.is_empty() { &row.request } else { body }, 400)),
            foot: Some((waiting_line(row), theme::text_mute())),
            buttons,
            report: true,
        });
    }
    tails
}

#[derive(Default)]
pub(crate) struct Layout {
    key: (u64, u32, u32),
    rows: Vec<(f32, String, Vec<String>, bool)>,
    height: f32,
}

pub(super) fn wrap(value: &str, width: f32, mut measure: impl FnMut(&str) -> f32) -> Vec<String> {
    let mut output = Vec::new();
    for paragraph in value.split('\n') {
        let mut line = String::new();
        let mut advance = 0.0;
        let mut count = 0;
        for ch in paragraph.chars().filter(|ch| *ch != '\r') {
            let mut bytes = [0; 4];
            let next = measure(ch.encode_utf8(&mut bytes));
            if count >= 256 || (!line.is_empty() && advance + next > width.max(1.0)) {
                output.push(std::mem::take(&mut line));
                advance = 0.0;
                count = 0;
            }
            line.push(ch);
            advance += next;
            count += 1;
        }
        output.push(line);
    }
    output
}

pub(super) fn paint_setup(
    g: &mut gpu::GpuRenderer,
    s: &Snapshot,
    hits: &mut Vec<Hit>,
    caret: &mut Option<Rect>,
    x: f32,
    y: &mut f32,
    w: f32,
) -> bool {
    let chat = &s.chat;
    if !chat.authenticated {
        text(g, x, *y, "개인 작업 공간", 20.0, theme::text(), true);
        *y += 34.0;
        overview_note(
            g,
            x,
            y,
            w,
            if chat.loading {
                "계정을 확인하고 있어요…"
            } else {
                "로그인하면 프로젝트와 원래 요청, 진행 기록을 같은 계정의 기기에서 이어 봐요."
            },
            theme::text_dim(),
        );
        button(
            g,
            s,
            hits,
            (x, *y, w.min(128.0), 26.0),
            "로그인",
            Target::NachoLogin,
            true,
        );
        *y += 40.0;
        return true;
    }
    if !key_present(&chat.workspace) || chat.key_editor {
        text(g, x, *y, "나쵸 연결", 20.0, theme::text(), true);
        *y += 34.0;
        overview_note(g,x,y,w,"OpenGateway 키로 개인 비서를 연결해요. 키는 계정 서버에 암호화해 저장하며 같은 계정의 기기에서 함께 사용해요.",theme::text_dim());
        field(
            g,
            s,
            hits,
            caret,
            (x, *y, w, 40.0),
            "OpenGateway API 키",
            &chat.key_mask,
            BoardInput::AssistantKey,
        );
        *y += 52.0;
        if chat.busy {
            text(g, x, *y, "등록 중…", 12.0, theme::text_dim(), false);
        } else {
            button(
                g,
                s,
                hits,
                (x, *y, w.min(100.0), 26.0),
                "키 등록",
                Target::AssistantKeySave,
                true,
            );
        }
        *y += 38.0;
        if let Some(error) = &chat.error {
            overview_note(g, x, y, w, error, theme::danger());
        }
        if key_present(&chat.workspace) {
            text_button(
                g,
                s,
                hits,
                (x, *y, 80.0_f32.min(w), 26.0),
                "돌아가기",
                Target::AssistantKeyEdit,
                false,
            );
            *y += 38.0;
        }
        return true;
    }
    false
}

pub(super) fn paint(
    g: &mut gpu::GpuRenderer,
    s: &Snapshot,
    hits: &mut Vec<Hit>,
    caret: &mut Option<Rect>,
    area: Rect,
) {
    let (x, y, w, h) = area;
    if w <= 0.0 || h <= 0.0 {
        return;
    }
    g.push_clip(x, y, w, h);
    if !s.chat.authenticated || !key_present(&s.chat.workspace) || s.chat.key_editor {
        let mut top = y;
        if s.tab == BoardTab::Chat && hub_layout(s.area, s.tab, y).work.is_none() {
            paint_setup(g, s, hits, caret, x, &mut top, w);
        } else {
            overview_note(
                g,
                x,
                &mut top,
                w,
                "연결 후 이곳에서 나쵸와 이야기해요.",
                theme::text_dim(),
            );
        }
        g.pop_clip();
        return;
    }
    let chat = &s.chat;
    let status = if chat.busy {
        "응답 확인 중…"
    } else if chat.loading {
        "동기화 중…"
    } else if chat.online {
        "계정 연결"
    } else {
        "연결 확인"
    };
    let status = fit(g, status, (w - 80.0).max(0.0), 10.5, false);
    text(g, x, y + 8.0, &status, 10.5, theme::text_dim(), false);
    text_button(g, s, hits, (x + w - 56.0, y + 1.0, 56.0, 26.0), "도구 ▾", Target::Tools, false);
    divider(g, x, y + 32.0, w);
    let context_h = if let Some((_, title)) = &chat.context {
        let label = fit(g, title, (w - 86.0).max(0.0), 10.5, false);
        text(g, x, y + 48.0, &label, 10.5, theme::text_dim(), false);
        text_button(
            g,
            s,
            hits,
            (x + w - 76.0, y + 40.0, 76.0, 26.0),
            "연결 해제",
            Target::NachoClearContext,
            false,
        );
        40.0
    } else {
        0.0
    };
    let hints = chat
        .error
        .as_deref()
        .map(|error| wrap(error, w, |text| g.measure_chrome_text(text, 10.5, false)))
        .unwrap_or_default();
    // 이름18·입력40·받는 곳26(입력칸 아래 6)·버튼26·간격 — 오류가 있으면 줄마다 18.
    let composer_h = 134.0 + hints.len() as f32 * 18.0;
    let composer_y = (y + h - composer_h).max(y + 40.0 + context_h);
    let transcript = (
        x,
        y + 40.0 + context_h,
        w,
        (composer_y - y - 46.0 - context_h).max(0.0),
    );
    g.push_clip(transcript.0, transcript.1, transcript.2, transcript.3);
    if let Ok(mut layout) = chat.layout.lock() {
        let revision = chat.workspace["status"]["revision"].as_u64().unwrap_or(0);
        let key = (revision, w.to_bits(), theme::ui_font_gen());
        if layout.key != key {
            layout.rows.clear();
            layout.height = 0.0;
            for message in rows(&chat.workspace, "conversation")
                .iter()
                .rev()
                .take(100)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
            {
                for (label, value, own) in [
                    ("나", string(message, "prompt"), true),
                    ("나쵸", string(message, "reply"), false),
                ] {
                    if value.is_empty() {
                        continue;
                    }
                    let lines = wrap(value, (w - 20.0).max(1.0), |text| {
                        g.measure_chrome_text(text, 12.0, false)
                    });
                    let top = layout.height;
                    layout.height += 34.0 + lines.len() as f32 * 18.0;
                    layout.rows.push((top, label.into(), lines, own));
                }
                let notice = match string(message, "status") {
                    "pending" => Some("응답을 준비하고 있어요."),
                    "model_setup" => Some(
                        "사용 가능한 대화 모델이 없어요. OpenGateway 모델 권한을 확인해 주세요.",
                    ),
                    "failed" => Some("응답을 완료하지 못했어요. 원래 요청은 보관했어요."),
                    _ => None,
                };
                if let Some(notice) = notice {
                    let lines = wrap(notice, (w - 20.0).max(1.0), |text| {
                        g.measure_chrome_text(text, 12.0, false)
                    });
                    let top = layout.height;
                    layout.height += 34.0 + lines.len() as f32 * 18.0;
                    layout.rows.push((top, "상태".into(), lines, false));
                }
            }
            layout.key = key;
        }
        let tails = tail_rows(g, s, w);
        let tails_h: f32 = tails.iter().map(Tail::height).sum();
        let max = (layout.height + tails_h - transcript.3).max(0.0);
        if let Ok(mut value) = chat.scroll_max.lock() {
            *value = max;
        }
        let offset = max - chat.scroll.min(max);
        for (top, label, lines, own) in &layout.rows {
            let top = transcript.1 + top - offset;
            if top + 34.0 + lines.len() as f32 * 18.0 < transcript.1
                || top > transcript.1 + transcript.3
            {
                continue;
            }
            text(g, x + 10.0, top, label, 10.5, if *own { theme::accent() } else { theme::text_dim() }, false);
            for (index, line) in lines.iter().enumerate() {
                let ly = top + 18.0 + index as f32 * 18.0;
                if ly + 18.0 >= transcript.1 && ly < transcript.1 + transcript.3 {
                    text(g, x + 10.0, ly, line, 12.0, theme::text(), false);
                }
            }
        }
        let mut top = transcript.1 + layout.height - offset;
        for tail in &tails {
            let height = tail.height();
            if top + height >= transcript.1 && top <= transcript.1 + transcript.3 {
                tail.paint(g, s, hits, x, top, w);
            }
            top += height;
        }
        if layout.rows.is_empty() && tails.is_empty() {
            text(g, x + 10.0, transcript.1 + 12.0, "무엇을 할까요? 학생에게 보낼 말도, 나쵸에게 물을 말도 여기에 써요.", 12.0, theme::text_dim(), false);
        }
    }
    g.pop_clip();
    divider(g, x, composer_y, w);
    field(
        g,
        s,
        hits,
        caret,
        (x, composer_y + 18.0, w, 40.0),
        "무엇을 할까요 — @이름으로 바로 보낼 수도 있어요",
        &chat.draft,
        BoardInput::NachoMessage,
    );
    paint_route_line(g, s, hits, x, composer_y + 64.0, w);
    for (index, line) in hints.iter().enumerate() {
        text(g, x, composer_y + 94.0 + index as f32 * 18.0, line, 10.5, theme::danger(), false);
    }
    let control_y = composer_y + 98.0 + hints.len() as f32 * 18.0;
    if chat.retryable {
        text_button(g, s, hits, (x, control_y, (w - 130.0).min(150.0), 26.0), "같은 요청 재확인", Target::NachoRetry, false);
    }
    let label = send_label(&s.route);
    let send_w = (g.measure_chrome_text(&label, 12.0, false) + 20.0).max(116.0).min(w);
    let to_nacho = !matches!(s.route.target(), Some(route::RouteTarget::Student(_) | route::RouteTarget::NewTask));
    let blocked = if to_nacho { chat.busy || chat.retryable } else { s.route_busy };
    if !blocked && !chat.draft.trim().is_empty() {
        button(g, s, hits, (x + w - send_w, control_y, send_w, 26.0), &label, Target::NachoSend, true);
    } else {
        let shown = if blocked { "보내는 중…".to_owned() } else { label };
        let tw = g.measure_chrome_text(&shown, 12.0, false);
        text(g, x + w - tw - 10.0, control_y + 6.0, &shown, 12.0, theme::text_mute(), false);
    }
    g.pop_clip();
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn wrapping_keeps_original_paragraphs_and_bounds_unbroken_runs() {
        assert_eq!(wrap("가나다\n둘째", 2.0, |_| 1.0), ["가나", "다", "둘째"]);
        assert!(wrap(&"a".repeat(1000), 10000.0, |_| 0.0)
            .iter()
            .all(|line| line.len() <= 256));
    }
}
