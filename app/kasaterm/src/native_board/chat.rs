use super::assistant::{key_present, rows, string};
pub(crate) use super::assistant::{ChatSnapshot, ChatState};
use super::*;

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
    text(g, x, y + 6.0, "나쵸", 14.0, theme::text(), true);
    let status = if chat.busy {
        "응답 확인 중…"
    } else if chat.loading {
        "동기화 중…"
    } else if chat.online {
        "계정 연결"
    } else {
        "연결 확인"
    };
    let status = fit(g, status, (w - 100.0).max(0.0), 10.5, false);
    let status_x = x + w - g.measure_chrome_text(&status, 10.5, false);
    text(
        g,
        status_x,
        y + 8.0,
        &status,
        10.5,
        theme::text_dim(),
        false,
    );
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
    let hint = chat
        .error
        .as_deref()
        .unwrap_or("Enter 전송 · Shift+Enter 줄바꿈");
    let hints = wrap(hint, w, |text| g.measure_chrome_text(text, 10.5, false));
    let composer_h = 126.0 + hints.len().saturating_sub(1) as f32 * 18.0;
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
        let max = (layout.height - transcript.3).max(0.0);
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
            text(
                g,
                x + 10.0,
                top,
                label,
                10.5,
                if *own {
                    theme::accent()
                } else {
                    theme::text_dim()
                },
                false,
            );
            for (index, line) in lines.iter().enumerate() {
                let ly = top + 18.0 + index as f32 * 18.0;
                if ly + 18.0 >= transcript.1 && ly < transcript.1 + transcript.3 {
                    text(g, x + 10.0, ly, line, 12.0, theme::text(), false);
                }
            }
        }
        if layout.rows.is_empty() {
            text(
                g,
                x + 10.0,
                transcript.1 + 12.0,
                "어떤 일을 정리할까요?",
                12.0,
                theme::text_dim(),
                false,
            );
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
        "나쵸에게 이야기하기…",
        &chat.draft,
        BoardInput::NachoMessage,
    );
    for (index, line) in hints.iter().enumerate() {
        text(
            g,
            x,
            composer_y + 66.0 + index as f32 * 18.0,
            line,
            10.5,
            if chat.error.is_some() {
                theme::danger()
            } else {
                theme::text_dim()
            },
            false,
        );
    }
    let control_y = composer_y + 90.0 + hints.len().saturating_sub(1) as f32 * 18.0;
    if chat.retryable {
        text_button(
            g,
            s,
            hits,
            (x, control_y, (w - 90.0).min(150.0), 26.0),
            "같은 요청 재확인",
            Target::NachoRetry,
            false,
        );
    }
    if !chat.busy && !chat.draft.trim().is_empty() && !chat.retryable {
        button(
            g,
            s,
            hits,
            (x + w - 76.0, control_y, 76.0, 26.0),
            "전송",
            Target::NachoSend,
            true,
        );
    } else {
        text(
            g,
            x + w - 76.0,
            control_y + 6.0,
            if chat.busy { "확인 중…" } else { "전송" },
            12.0,
            theme::text_mute(),
            false,
        );
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
