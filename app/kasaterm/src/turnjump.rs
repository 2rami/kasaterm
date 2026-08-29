//! 대화 턴 헤더 — 스크롤백을 올려다볼 때 「지금 보는 곳이 어느 질문의 답인가」를
//! pane 맨 위에 고정으로 띄우고, 그 질문 자리로 **정확히** 되돌려 보낸다.
//!
//! 이미 있는 `sticky_seek`(screenread.rs)와 무엇이 다른가 — 그쪽은 claude 가
//! mouse-tracking 으로 **자기 버퍼를** 스크롤할 때의 길이다. 그 세계에서는 kasaterm
//! 이 위치를 모르므로, 점프도 휠을 한 노치씩 쏘며 목표 텍스트가 화면에 나타나는지
//! 지켜보는 되짚기밖에 없다(2026-08-15 확인: 그래서 「정확히 그 자리」가 아니다).
//! 여기는 터미널 자신의 스크롤백(alacritty display_offset)이라 절대 줄 번호를
//! 알고, 한 번의 이동으로 닿는다.
//!
//! 상태는 pane 별 앵커 캐시 하나뿐이다. 스캔은 히스토리 길이가 달라졌을 때만
//! 다시 돌린다 — 스크롤을 보는 동안에는 그 길이가 거의 그대로라 사실상 한 번이다.

use std::collections::HashMap;
use std::time::Instant;

use kasa_bridge::screen::{Cell as GridCell, Color};
use kasa_pty::PromptAnchor;

/// 헤더에 그릴 것 한 벌. 이번 프레임에 pane 하나가 보여 줄 내용이 전부 여기 있다.
#[derive(Clone, Debug)]
pub(crate) struct TurnHeader {
    /// 지금 화면 맨 위가 속한 턴의 질문 한 줄.
    pub text: String,
    /// 그 질문이 있는 절대 줄 — 헤더를 누르면 여기로 간다.
    pub cur_abs: i64,
    /// 하나 위 질문. 없으면 ↑ 를 흐리게 둔다.
    pub prev_abs: Option<i64>,
    /// 하나 아래 질문. 없으면 ↓ 를 흐리게 둔다.
    pub next_abs: Option<i64>,
}

/// 헤더의 어느 부분을 눌렀나. 셋 다 결국 「절대 줄로 이동」이라 목적지를 함께 든다.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TurnHit {
    /// 바 본체 — 지금 그 질문 자리로.
    Jump(i64),
    Prev(i64),
    Next(i64),
    /// claude 가 **자기 버퍼를** 스크롤하는 세계의 앞/뒤 질문. 그쪽은 좌표가 없어
    /// 절대 줄로 말할 수가 없고, 「지금 맨 위에 붙은 질문이 바뀔 때까지 굴린다」로만
    /// 표현된다 — 목적지는 누를 때 화면에서 읽으므로 여기 담을 것이 없다.
    SeekPrev,
    SeekNext,
}

thread_local! {
    /// 이번 프레임에 그린 헤더의 클릭 영역 — (pane id, rect(logical px), 할 일).
    ///
    /// struct App 을 안 건드리려고 여기 둔다(병렬 작업 규칙). GUI 는 단일 스레드라
    /// 이것으로 충분하고, sticky pill 이 `STICKY_PILLS` 로 같은 방식을 쓴다.
    pub(crate) static TURN_HITS:
        std::cell::RefCell<Vec<(String, (f32, f32, f32, f32), TurnHit)>> =
        std::cell::RefCell::new(Vec::new());
}

/// 그 좌표에 헤더의 무엇이 있나.
///
/// **나중에 담긴 것부터** 본다 — 바를 먼저 담고 그 위에 얹은 ↑↓ 를 나중에 담으므로,
/// 역순으로 봐야 화살표를 눌렀을 때 바 클릭(그 자리로 점프)에 먹히지 않는다.
/// 화면에서 위에 있는 것이 클릭도 가져간다는, 눈에 보이는 대로의 규칙이다.
pub(crate) fn turn_hit_at(x: f32, y: f32) -> Option<(String, TurnHit)> {
    TURN_HITS.with(|s| {
        s.borrow()
            .iter()
            .rev()
            .find(|(_, (rx, ry, rw, rh), _)| x >= *rx && x <= rx + rw && y >= *ry && y <= ry + rh)
            .map(|(id, _, hit)| (id.clone(), *hit))
    })
}

#[derive(Default)]
pub(crate) struct TurnJump {
    /// pane id → (스캔했을 때의 히스토리 길이, 그때의 앵커들).
    ///
    /// 히스토리 길이를 키로 쓰는 이유: 줄이 늘면 앵커의 절대 번호가 그대로여도
    /// **새 질문이 생겼을 수 있고**, 상한에 닿아 회전하면 번호 자체가 밀린다.
    /// 길이가 그대로면 둘 다 아니므로 다시 훑을 필요가 없다.
    cache: HashMap<String, (usize, Vec<PromptAnchor>)>,
    /// `KASATERM_AUTOTURNCLICK` 예약 — (발사 시각, 누를 자리). struct App 을 안
    /// 늘리려고 여기 둔다(다른 하네스는 App 필드를 쓰지만 이쪽은 자기 통이 있다).
    autoclick: Option<(Instant, String)>,
}

impl TurnJump {
    /// pane 하나의 이번 프레임 헤더. 라이브 바닥(offset 0)이면 `None` — 평소 화면을
    /// 가리지 않는다는 규칙이 여기 한 줄로 걸린다.
    pub(crate) fn header(&mut self, pane_id: &str, sess: &kasa_pty::PtySession) -> Option<TurnHeader> {
        let (offset, hist) = sess.view_state();
        if std::env::var_os("KASATERM_TURN_DEBUG").is_some() && offset != 0 {
            eprintln!("[turn] pane={pane_id} offset={offset} hist={hist}");
        }
        if offset == 0 {
            return None;
        }
        let entry = self.cache.entry(pane_id.to_string());
        let (cached_hist, anchors) = entry.or_insert_with(|| (usize::MAX, Vec::new()));
        if *cached_hist != hist {
            *anchors = sess.prompt_anchors();
            *cached_hist = hist;
        }
        if std::env::var_os("KASATERM_TURN_DEBUG").is_some() {
            eprintln!("[turn]   anchors={}", anchors.len());
        }
        if anchors.is_empty() {
            return None;
        }
        // 화면 맨 윗줄의 절대 번호. 인라인 이미지가 뷰포트 배치를 낼 때 쓰는 것과
        // 같은 셈이다(`top_abs = hist - display_offset`).
        let top_abs = hist as i64 - offset as i64;
        // 그 줄이 속한 턴 = 그보다 위에 있는 마지막 질문. 화면 첫 줄이 질문 그
        // 자체일 때도 자기 자신이 잡히는 게 맞다(그 턴을 보고 있는 것이므로).
        let idx = anchors.partition_point(|a| a.abs_line <= top_abs);
        let cur = idx.checked_sub(1)?;
        Some(TurnHeader {
            text: anchors[cur].text.clone(),
            cur_abs: anchors[cur].abs_line,
            prev_abs: cur.checked_sub(1).map(|i| anchors[i].abs_line),
            next_abs: anchors.get(cur + 1).map(|a| a.abs_line),
        })
    }

    /// pane 이 사라졌으면 캐시도 버린다 — 닫힌 pane 의 앵커를 들고 있을 이유가 없고,
    /// 같은 id 가 재사용되면 남의 스크롤백을 가리키게 된다.
    pub(crate) fn retain_panes(&mut self, alive: impl Fn(&str) -> bool) {
        self.cache.retain(|id, _| alive(id));
    }
}

/// claude sticky pill 줄에서 ↑↓ 가 놓일 열. 줄이 너무 짧으면 `(None, None)`.
///
/// 헤더(`paint_header_row`)와 **같은 자리 규칙**을 쓴다 — 두 세계(터미널 스크롤백과
/// claude 자기 버퍼)에서 화살표가 다른 자리에 있으면 같은 기능으로 안 읽힌다.
pub(crate) fn sticky_arrow_cols(len: usize) -> (Option<usize>, Option<usize>) {
    if len < 6 {
        return (None, None);
    }
    (Some(len - 4), Some(len - 2))
}

/// 헤더 줄에서 화살표가 놓인 열 — 클릭 rect 를 그 자리에 맞추는 데 쓴다.
/// 갈 곳이 없어 흐리게만 둔 화살표는 `None` 이라 눌러도 아무 일이 없다.
pub(crate) struct HeaderCols {
    pub up: Option<usize>,
    pub down: Option<usize>,
}

/// 헤더 한 줄을 pane 첫 행 셀에 **직접** 써넣는다.
///
/// 그림으로 얹지 않고 셀을 고치는 이유: 이 터미널의 텍스트 그리기는 글자 잉크에
/// 맞춰 폭이 좁아지는 비례 경로라, 한글을 그리면 자간이 아래 대화와 어긋난다.
/// 등폭 셀에 써넣으면 글자 자리가 원본 그리드와 정확히 맞는다 — claude sticky pill
/// 이 「딱 안 맞아 자간 이상」이라는 지적을 받고 같은 방식으로 옮겨 갔다.
pub(crate) fn paint_header_row(row: &mut [GridCell], h: &TurnHeader) -> HeaderCols {
    use unicode_width::UnicodeWidthChar;
    let rgb = |c: [u8; 4]| Color::Rgb(c[0], c[1], c[2]);
    let bg = rgb(crate::theme::surface_hover());
    let base = {
        let mut c = GridCell::blank();
        c.ch = ' ';
        c.bg = bg.clone();
        c
    };
    for c in row.iter_mut() {
        *c = base.clone();
    }
    // 오른쪽 끝 화살표 자리부터 잡는다 — 본문은 그 앞까지만 쓴다.
    // `↑ ↓ ` 로 넉 칸. claude 가 "Jump to bottom ↓" 에 같은 계열 글리프를 쓰고 있어
    // 폰트 폴백이 확인된 문자다.
    let (mut up_col, mut down_col) = (None, None);
    let text_end = if let (Some(up), Some(down)) = sticky_arrow_cols(row.len()) {
        let put = |row: &mut [GridCell], at: usize, ch: char, on: bool| {
            let mut c = base.clone();
            c.ch = ch;
            c.fg = rgb(if on { crate::theme::text() } else { crate::theme::text_mute() });
            c.bold = on;
            row[at] = c;
        };
        put(row, up, '↑', h.prev_abs.is_some());
        put(row, down, '↓', h.next_abs.is_some());
        up_col = h.prev_abs.map(|_| up);
        down_col = h.next_abs.map(|_| down);
        up.saturating_sub(1)
    } else {
        row.len()
    };
    let mut w = 0usize;
    let put = |row: &mut [GridCell], ch: char, fg: [u8; 4], bold: bool, w: &mut usize| {
        let cw = ch.width().unwrap_or(1).max(1);
        if *w + cw > text_end {
            return false;
        }
        let mut c = base.clone();
        c.ch = ch;
        c.fg = rgb(fg);
        c.bold = bold;
        row[*w] = c;
        // wide 글리프의 뒤칸은 **공백 셀**로 둔다 — 이 레포의 셀 표현 관례다
        // (`replace_agent_header` 등이 같은 모양).
        if cw == 2 && *w + 1 < text_end {
            let mut sp = base.clone();
            sp.ch = ' ';
            row[*w + 1] = sp;
        }
        *w += cw;
        true
    };
    for ch in "❯ ".chars() {
        put(row, ch, crate::theme::accent(), true, &mut w);
    }
    for ch in h.text.chars() {
        if !put(row, ch, crate::theme::text(), false, &mut w) {
            // 넘치면 잘렸다는 표시를 마지막 칸에 남긴다 — 그냥 끊으면 원문이
            // 거기서 끝난 것처럼 읽힌다.
            if text_end > 0 {
                let mut c = base.clone();
                c.ch = '…';
                c.fg = rgb(crate::theme::text_dim());
                row[text_end - 1] = c;
            }
            break;
        }
    }
    HeaderCols { up: up_col, down: down_col }
}

impl crate::App {
    /// 헤더를 눌렀으면 그 자리로 옮기고 `true`. 헤더 밖이면 `false` 라 클릭이 그대로
    /// 아래(터미널 SGR 전달 등)로 흐른다.
    ///
    /// 진짜 클릭과 검증 하네스가 **이 한 함수**를 함께 쓴다. 하네스가 자기 사본을
    /// 들고 있으면 그 사본만 늘 통과하고 화면에서만 안 눌리는 일이 생긴다 — 이
    /// 레포에서 「같은 로직 두 벌은 한쪽만 고쳐진다」로 여러 번 물린 자리다.
    pub(crate) fn turn_header_click(&mut self, x: f32, y: f32) -> bool {
        let Some((pane_id, hit)) = turn_hit_at(x, y) else { return false };
        let dbg = std::env::var_os("KASATERM_TURN_DEBUG").is_some();
        match hit {
            // 터미널 스크롤백 세계 — 좌표가 확정이라 한 번에 닿는다.
            TurnHit::Jump(abs) | TurnHit::Prev(abs) | TurnHit::Next(abs) => {
                if let Some(pty) = self.pty_for_pane(&pane_id) {
                    let off = pty.scroll_to_abs(abs);
                    if dbg {
                        eprintln!("[turn] click {hit:?} pane={pane_id} → display_offset={off}");
                    }
                }
            }
            // claude 자기 버퍼 세계 — 좌표가 없어 「지금 맨 위 질문이 바뀔 때까지」
            // 굴리는 되짚기뿐이다. 목적지는 지금 화면에 붙어 있는 그 줄이므로 여기서
            // 읽는다. 그 줄이 없으면(이미 라이브 바닥) 할 일이 없다.
            TurnHit::SeekPrev | TurnHit::SeekNext => {
                let down = matches!(hit, TurnHit::SeekNext);
                let Some(target) = crate::render::sticky_text_for(&pane_id) else {
                    if dbg {
                        eprintln!("[turn] seek {hit:?} pane={pane_id} — 붙은 줄이 없어 무시");
                    }
                    return true;
                };
                // wheel 을 쏠 자리는 그 pane 안이어야 한다(클릭 지점이면 늘 그렇다).
                let cell = self.px_to_pane_cell(x, y).map(|(_, c, r)| (c, r)).unwrap_or((1, 1));
                if dbg {
                    eprintln!("[turn] seek {hit:?} pane={pane_id} down={down} target={target:?}");
                }
                crate::render::begin_sticky_seek(pane_id, target, cell, down);
            }
        }
        true
    }

    /// `KASATERM_AUTOTURNCLICK="bar|up|down"` (+ `_MS`) — 헤더의 그 부분을 눌러 본다.
    ///
    /// 클릭 자리를 **이번 프레임에 실제로 그려진 rect** 에서 가져오는 게 요점이다.
    /// 좌표를 손으로 적어 두면 헤더가 안 떠도 그 자리를 눌러 보고 「눌렀다」고 적는다.
    pub(crate) fn arm_autoturnclick(&mut self) {
        let Ok(spot) = std::env::var("KASATERM_AUTOTURNCLICK") else { return };
        let ms: u64 = std::env::var("KASATERM_AUTOTURNCLICK_MS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(6000);
        eprintln!("[turnclick] in {ms}ms: {spot}");
        self.turn.autoclick = Some((Instant::now() + std::time::Duration::from_millis(ms), spot));
    }

    pub(crate) fn run_pending_autoturnclick(&mut self) {
        let Some((due, spot)) = self.turn.autoclick.clone() else { return };
        if Instant::now() < due {
            return;
        }
        self.turn.autoclick = None;
        let target = TURN_HITS.with(|s| {
            s.borrow()
                .iter()
                .find(|(_, _, hit)| {
                    // 두 세계의 같은 자리를 한 이름으로 누른다 — 하네스가 「위 화살표」
                    // 를 말할 때 터미널 쪽인지 claude 쪽인지 알 필요가 없어야 한다.
                    matches!(
                        (spot.as_str(), hit),
                        ("bar", TurnHit::Jump(_))
                            | ("up", TurnHit::Prev(_) | TurnHit::SeekPrev)
                            | ("down", TurnHit::Next(_) | TurnHit::SeekNext)
                    )
                })
                .map(|(_, r, hit)| (*r, *hit))
        });
        let Some(((rx, ry, rw, rh), hit)) = target else {
            eprintln!("[turnclick] {spot} 자리가 이번 프레임에 없음 — 헤더가 안 떴다");
            return;
        };
        let (cx, cy) = (rx + rw / 2.0, ry + rh / 2.0);
        let handled = self.turn_header_click(cx, cy);
        eprintln!("[turnclick] {spot} ({cx:.0},{cy:.0}) {hit:?} handled={handled}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn anchors(lines: &[i64]) -> Vec<PromptAnchor> {
        lines
            .iter()
            .map(|l| PromptAnchor { abs_line: *l, text: format!("질문 {l}") })
            .collect()
    }

    /// 헤더가 가리킬 턴 고르기 — 화면 맨 윗줄보다 **위에 있는 마지막** 질문.
    /// `header()` 는 살아 있는 PTY 를 요구하므로 그 안의 셈만 떼어 검증한다.
    fn pick(anchors: &[PromptAnchor], top_abs: i64) -> Option<usize> {
        anchors.partition_point(|a| a.abs_line <= top_abs).checked_sub(1)
    }

    #[test]
    fn picks_the_turn_the_top_line_belongs_to() {
        let a = anchors(&[10, 50, 90]);
        // 두 질문 사이를 보고 있으면 위쪽 질문의 턴이다.
        assert_eq!(pick(&a, 70), Some(1));
        // 마지막 질문보다 아래면 마지막 턴.
        assert_eq!(pick(&a, 200), Some(2));
    }

    #[test]
    fn top_line_on_a_prompt_selects_that_prompt() {
        let a = anchors(&[10, 50]);
        // 질문 줄에 딱 맞춰 올라온 상태 — 그 턴을 보고 있는 것이므로 자기 자신.
        assert_eq!(pick(&a, 50), Some(1));
    }

    #[test]
    fn above_the_first_prompt_has_no_turn() {
        let a = anchors(&[10, 50]);
        // 첫 질문보다 위(로고·부팅 출력)에는 가리킬 턴이 없다.
        assert_eq!(pick(&a, 5), None);
    }
}
