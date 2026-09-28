//! 떠 있는 알림 — 오른쪽 위에 잠깐 서거나(복사·완료·안내), 답할 때까지 서 있는(승인) 한 장.
//!
//! 완료 배너(`notify_banner.rs`)와 같은 판에 얹는다: 패널 바닥색 판, 채움에서 끌어온 테두리,
//! 뜻 색은 표지 한 자리에만. 예전엔 글 전체를 뜻 색 굵은 글씨로 칠해 초록·주황 문장이 터미널
//! 위에서 튀었고, 반투명 판이라 뒤 글자가 비쳤다(2026-09-28 「토스트 디자인 이쁘게」).
//! 치수는 `docs/design.md` 「떠 있는 알림」.

use super::*;

type Rect = (f32, f32, f32, f32);

const EDGE: f32 = 16.0;
const TOP_GAP: f32 = 12.0;
const PAD_L: f32 = 11.0;
const PAD_R: f32 = 14.0;
const PAD_Y: f32 = 11.0;
const BADGE: f32 = 26.0;
const BADGE_ICON: f32 = 14.0;
const BADGE_GAP: f32 = 10.0;
const TITLE_F: f32 = 12.0;
const TITLE_LINE: f32 = 18.0;
const DETAIL_F: f32 = 10.5;
const DETAIL_LINE: f32 = 16.0;
const TEXT_MAX: f32 = 360.0;
const CHIP_GAP: f32 = 6.0;
const CHIPS_LEAD: f32 = 12.0;
const ENTER_MS: f32 = 180.0;
const ENTER_RISE: f32 = 6.0;

/// 알림 한 줄을 제목과 설명으로 가른다 — 앞머리가 「무슨 일」, 나머지가 「무엇이」다.
/// 「나쵸네코에서 복사됨 · 앞머리」, 「미도리 — 권한 요청」, 「저장 실패: 까닭」 처럼 이 앱의
/// 알림은 이미 그렇게 쓰여 있어, 가름표에서 끊기만 하면 두 단이 된다.
pub(crate) fn split_notice(message: &str) -> (&str, Option<&str>) {
    let cut = [" · ", " — ", ": "]
        .iter()
        .filter_map(|sep| message.find(sep).map(|at| (at, sep.len())))
        .min_by_key(|(at, _)| *at);
    match cut {
        Some((at, len)) => {
            let title = message[..at].trim();
            let detail = message[at + len..].trim();
            if title.is_empty() {
                (detail, None)
            } else if detail.is_empty() {
                (title, None)
            } else {
                (title, Some(detail))
            }
        }
        None => (message.trim(), None),
    }
}

/// 표지 원 안의 글리프. 원 안에 또 테두리 있는 아이콘(네모 체크·말풍선)을 넣으면 틀이
/// 두 겹이 되어, 선 하나짜리 글리프로 고른다. 복사는 「끝났다」보다 「담겼다」가 할 말이다.
fn badge_icon(tone: theme::NoticeTone, title: &str) -> &'static str {
    use theme::NoticeTone::*;
    match tone {
        Success if title.contains("복사") => "copy",
        Success => "check",
        Error => "x",
        Warning | Attention => "triangle-alert",
        Info => "info",
    }
}

/// 들어오는 진행(0..1, 끝이 느린 곡선). 판이 위에서 살짝 내려앉으며 선다.
pub(crate) fn enter_progress(elapsed_ms: f32) -> f32 {
    let t = (elapsed_ms / ENTER_MS).clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(3)
}

/// 판의 자리와 크기 — 창 폭·글 줄 수·글 폭·칩 폭만으로 정해진다(그리지 않고 잰다).
pub(crate) fn notice_card(
    win_w: f32,
    text_w: f32,
    title_lines: usize,
    detail_lines: usize,
    chips_w: f32,
) -> Rect {
    let chips = if chips_w > 0.0 { CHIPS_LEAD + chips_w } else { 0.0 };
    let w = PAD_L + BADGE + BADGE_GAP + text_w + chips + PAD_R;
    let text_h = title_lines as f32 * TITLE_LINE + detail_lines as f32 * DETAIL_LINE;
    let h = (text_h + PAD_Y * 2.0).max(BADGE + PAD_Y * 2.0);
    let x = (win_w - w - EDGE).max(EDGE);
    (x, TITLE_HEIGHT + TOP_GAP, w, h)
}

/// 글 칸이 쓸 수 있는 최대 폭 — 좁은 창에서도 판이 창 밖으로 안 나간다.
fn text_room(win_w: f32, chips_w: f32) -> f32 {
    let chips = if chips_w > 0.0 { CHIPS_LEAD + chips_w } else { 0.0 };
    (win_w - EDGE * 2.0 - PAD_L - BADGE - BADGE_GAP - chips - PAD_R)
        .min(TEXT_MAX)
        .max(40.0)
}

/// 판 한 장 — 흐린 그림자 셋, 불투명한 바닥, 채움 기준 테두리. 글자 뒤는 늘 안정된
/// 면이어야 한다(`design.md` 「비에 젖은 유리」: 투명은 장식이지 가독성을 대신하지 않는다).
fn surface(g: &mut gpu::GpuRenderer, (x, y, w, h): Rect, r: f32, alpha: f32) {
    if theme::shadow_offset() <= 0.0 {
        for (spread, a) in [(6.0_f32, 6.0_f32), (4.0, 12.0), (2.0, 20.0)] {
            round_rect(
                g,
                x - spread,
                y - spread + 2.0,
                w + spread * 2.0,
                h + spread * 2.0,
                r + spread,
                [0, 0, 0, (a * alpha).round() as u8],
            );
        }
    }
    let fill = theme::panel_bg();
    let a = (255.0 * alpha).round() as u8;
    panel_rect(g, x, y, w, h, r, theme::with_alpha(fill, a));
    g.round_rect_stroke(x, y, w, h, r, 1.0, theme::with_alpha(theme::edge_on(fill), a));
}

pub(crate) struct NoticeHits {
    pub card: Rect,
    pub approve: Option<Rect>,
    pub deny: Option<Rect>,
}

pub(crate) struct Notice<'a> {
    pub message: &'a str,
    pub alpha: f32,
    pub elapsed_ms: f32,
    /// 결정을 받는 알림(승인·업데이트)이면 두 칩의 이름.
    pub actions: Option<(&'a str, &'a str)>,
    /// 둘째 칩이 되돌릴 수 없는 거절인가(승인 「거부」). 「나중에」는 아니다.
    pub decline_is_danger: bool,
}

/// 오른쪽 위 알림. 돌려주는 사각형은 **다 내려앉은 자리**다 — 들어오는 0.18초 동안
/// 눌러도 판이 설 자리를 친다.
pub(crate) fn paint_notice(
    g: &mut gpu::GpuRenderer,
    win_w: f32,
    cursor: (f32, f32),
    notice: &Notice<'_>,
) -> NoticeHits {
    let enter = enter_progress(notice.elapsed_ms);
    let has_action = notice.actions.is_some();
    // 결정 칩은 공통 버튼이라 흐리게 못 그린다 — 판만 흐려지며 들어오면 칩이 먼저 떠
    // 보인다. 결정을 받는 알림은 미끄러지기만 한다.
    let alpha = if has_action { notice.alpha } else { notice.alpha * enter };
    let tone = theme::notice_tone(notice.message, has_action);
    let (title, detail) = split_notice(theme::clean_notice_text(notice.message));
    let limit = theme::notice_line_limit(tone);
    let keep_tail = theme::notice_keeps_tail(tone);

    let chip_w = |g: &mut gpu::GpuRenderer, label: &str| {
        g.measure_chrome_text(label, 12.0, true) + native_controls::CONTROL_PADDING_X * 2.0
    };
    let chips = notice.actions.map(|(ok, no)| (chip_w(g, ok), chip_w(g, no)));
    let chips_w = chips.map(|(a, b)| a + CHIP_GAP + b).unwrap_or(0.0);
    let room = text_room(win_w, chips_w);

    let (title_lines, detail_lines) = match detail {
        Some(d) => (
            vec![crate::info::fit_text(g, title, room, TITLE_F, true)],
            crate::info::fit_text_lines(g, d, room, DETAIL_F, false, limit, keep_tail),
        ),
        None => (
            crate::info::fit_text_lines(g, title, room, TITLE_F, true, limit, keep_tail),
            Vec::new(),
        ),
    };
    let mut text_w = 0.0_f32;
    for l in &title_lines {
        text_w = text_w.max(g.measure_chrome_text(l, TITLE_F, true));
    }
    for l in &detail_lines {
        text_w = text_w.max(g.measure_chrome_text(l, DETAIL_F, false));
    }
    let card = notice_card(win_w, text_w, title_lines.len(), detail_lines.len(), chips_w);
    let (x, y, w, h) = card;
    let y_drawn = y - ENTER_RISE * (1.0 - enter);
    let r = theme::radius_md();
    surface(g, (x, y_drawn, w, h), r, alpha);

    let ink = |c: [u8; 4]| theme::with_alpha(c, (255.0 * alpha).round() as u8);
    let tone_color = theme::enforce_contrast_at(theme::notice_tone_color(tone), theme::panel_bg(), 3.0);
    let bx = x + PAD_L;
    let by = y_drawn + (h - BADGE) / 2.0;
    circle_rect(g, bx, by, BADGE, theme::with_alpha(tone_color, (40.0 * alpha).round() as u8));
    g.queue_icon(
        badge_icon(tone, title),
        bx + (BADGE - BADGE_ICON) / 2.0,
        by + (BADGE - BADGE_ICON) / 2.0,
        BADGE_ICON,
        ink(tone_color),
    );

    let tx = bx + BADGE + BADGE_GAP;
    let block = title_lines.len() as f32 * TITLE_LINE + detail_lines.len() as f32 * DETAIL_LINE;
    let mut ly = y_drawn + (h - block) / 2.0;
    for line in &title_lines {
        g.draw_text(
            tx,
            ly + (TITLE_LINE - TITLE_F) / 2.0 - 0.5,
            line,
            gpu::DrawOpts { font_size: TITLE_F, color: ink(theme::text()), bold: true, italic: false },
        );
        ly += TITLE_LINE;
    }
    for line in &detail_lines {
        g.draw_text(
            tx,
            ly + (DETAIL_LINE - DETAIL_F) / 2.0 - 0.5,
            line,
            gpu::DrawOpts { font_size: DETAIL_F, color: ink(theme::text_dim()), bold: false, italic: false },
        );
        ly += DETAIL_LINE;
    }

    let mut hits = NoticeHits { card, approve: None, deny: None };
    if let (Some((ok, no)), Some((ok_w, no_w))) = (notice.actions, chips) {
        let cy = y_drawn + (h - native_controls::CONTROL_HEIGHT) / 2.0;
        let ch = native_controls::CONTROL_HEIGHT;
        let ox = x + w - PAD_R - chips_w;
        let primary = native_controls::Style { primary: true, ..Default::default() };
        hits.approve = Some(native_controls::text_button(g, (ox, cy, ok_w, ch), cursor, ok, primary));
        let nx = ox + ok_w + CHIP_GAP;
        let deny = native_controls::Style { danger: notice.decline_is_danger, ..Default::default() };
        hits.deny = Some(native_controls::text_button(g, (nx, cy, no_w, ch), cursor, no, deny));
    }
    hits
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notices_split_at_the_first_separator() {
        assert_eq!(
            split_notice("나쵸네코에서 복사됨 · 앞머리 · 더"),
            ("나쵸네코에서 복사됨", Some("앞머리 · 더"))
        );
        assert_eq!(split_notice("미도리 — Bash 실행 권한"), ("미도리", Some("Bash 실행 권한")));
        assert_eq!(
            split_notice("창을 닫았어요 · 10초 뒤 실행 종료 — ⌘⇧T"),
            ("창을 닫았어요", Some("10초 뒤 실행 종료 — ⌘⇧T"))
        );
        assert_eq!(
            split_notice("PR 만들기 실패: gh 가 로그인돼 있지 않아요"),
            ("PR 만들기 실패", Some("gh 가 로그인돼 있지 않아요"))
        );
        assert_eq!(split_notice("주소 https://a.b/c 열기"), ("주소 https://a.b/c 열기", None));
        assert_eq!(split_notice("화면 새로고침"), ("화면 새로고침", None));
        assert_eq!(split_notice(" · 설명만"), ("설명만", None));
    }

    #[test]
    fn a_one_line_notice_is_as_tall_as_its_badge_and_hugs_the_right_edge() {
        let (x, y, w, h) = notice_card(1200.0, 100.0, 1, 0, 0.0);
        assert_eq!(h, BADGE + PAD_Y * 2.0);
        assert_eq!(x + w, 1200.0 - EDGE);
        assert_eq!(y, TITLE_HEIGHT + TOP_GAP);
        let two = notice_card(1200.0, 100.0, 1, 2, 0.0);
        assert_eq!(two.3, TITLE_LINE + DETAIL_LINE * 2.0 + PAD_Y * 2.0);
    }

    #[test]
    fn chips_widen_the_card_but_a_narrow_window_keeps_it_inside() {
        let plain = notice_card(1200.0, 100.0, 1, 0, 0.0);
        let with_chips = notice_card(1200.0, 100.0, 1, 0, 90.0);
        assert_eq!(with_chips.2 - plain.2, CHIPS_LEAD + 90.0);
        let room = text_room(300.0, 90.0);
        let (x, _, w, _) = notice_card(300.0, room, 1, 0, 90.0);
        assert!(x >= EDGE && x + w <= 300.0 - EDGE, "{x} + {w}");
    }

    #[test]
    fn badges_hold_a_single_line_glyph() {
        use theme::NoticeTone::*;
        assert_eq!(badge_icon(Success, "나쵸네코에서 복사됨"), "copy");
        assert_eq!(badge_icon(Success, "저장했어요"), "check");
        assert_eq!(badge_icon(Attention, "미도리"), "triangle-alert");
    }

    #[test]
    fn entering_settles_with_a_slow_finish() {
        assert_eq!(enter_progress(0.0), 0.0);
        assert!(enter_progress(ENTER_MS / 2.0) > 0.5);
        assert_eq!(enter_progress(ENTER_MS * 2.0), 1.0);
    }
}
