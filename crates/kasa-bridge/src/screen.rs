//! GUI-agnostic screen snapshot types produced from a vt100 parser.

use serde::{Deserialize, Serialize};
use unicode_width::UnicodeWidthChar;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Color {
    Default,
    Idx(u8),
    Rgb(u8, u8, u8),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Cell {
    /// One glyph per cell. `'\0'` is the blank/wide-char-spacer sentinel.
    /// A single `char` (4 bytes, inline) instead of a `String` avoids a heap
    /// allocation per cell — with millions of cells across scrollback + diff
    /// copies, the per-cell `String` was the dominant RSS cost.
    pub ch: char,
    pub fg: Color,
    pub bg: Color,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub inverse: bool,
    pub dim: bool,
    /// SGR 8 (conceal) — 글리프를 그리지 않되 텍스트 추출(visible_text 등)에는 남는다.
    /// statusline 이 세션 id 마커(`⟦sid8⟧`)를 화면에 안 보이게 실어 보내는 채널:
    /// kasaterm 은 그리드에서 읽고, 사용자는 못 본다. 구버전 스냅샷 호환 위해 default.
    #[serde(default)]
    pub hidden: bool,
}

impl Cell {
    pub fn blank() -> Self {
        Self {
            ch: ' ',
            fg: Color::Default,
            bg: Color::Default,
            bold: false,
            italic: false,
            underline: false,
            inverse: false,
            dim: false,
            hidden: false,
        }
    }
}

pub type Row = Vec<Cell>;

/// OSC 1337 인라인 이미지 한 장의 이번 프레임 뷰포트 배치. PTY 쪽이 내용 절대
/// 줄 앵커를 쥐고 프레임마다 화면 좌표로 환산해 싣는다 — 스크롤 상태의 정본이
/// alacritty Term 이라, GUI 가 따로 계산하면 반드시 어긋난다. GUI 는 받은
/// 자리에 그리기만 한다.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InlineImageView {
    /// pane 안에서 안정적인 식별자 — 같은 그림의 재전송(자리 이동)에도 유지돼
    /// GUI 텍스처 캐시가 산다.
    pub id: u64,
    /// 원본 임시 파일(PNG/JPEG). GUI 가 읽어 텍스처로 올린다.
    pub path: String,
    /// 이미지 상단의 뷰포트 행 — 스크롤로 위가 잘리면 음수.
    pub row: i32,
    pub col: u16,
    /// 차지하는 셀 폭·높이(송신측 width=<cells> + 픽셀 비율).
    pub cols: u16,
    pub rows: u16,
}

/// Screen diff sent from the flusher thread to consumers.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ScreenUpdate {
    pub pane_id: String,
    pub rows: u16,
    pub cols: u16,
    /// Only changed rows. On size change this contains every row.
    pub dirty: Vec<(u16, Row)>,
    pub cursor_row: u16,
    pub cursor_col: u16,
    pub cursor_visible: bool,
    pub alt_screen: bool,
    /// True when the inner app enabled any xterm mouse-reporting mode
    /// (DECSET 1000/1002/1003/1006). Lets the host translate wheel
    /// events into proper mouse escapes instead of bouncing them off
    /// arrow keys or PgUp/PgDn, which page-jumps in claude TUI.
    pub mouse_enabled: bool,
    /// True when the app requested SGR encoding (DECSET 1006). Without
    /// this we'd have to fall back to the legacy X10 encoding which
    /// claude doesn't parse reliably.
    pub mouse_sgr: bool,
    /// True when the inner app enabled DECCKM (application cursor keys,
    /// DECSET 1). In this mode plain arrow keys must be sent as SS3
    /// (`ESC O A`) not CSI (`ESC [ A`) — claude code / vim / readline
    /// turn it on, and a CSI arrow silently no-ops their line navigation.
    pub app_cursor: bool,
    /// True when the inner app enabled bracketed paste (DECSET 2004).
    /// The host must wrap a paste in `ESC[200~ … ESC[201~` **only** then —
    /// an app that never asked for it receives those bytes as literal input
    /// (거노: `claude auth login` 의 코드 프롬프트가 "Invalid code" 로 튕겼다).
    pub bracketed_paste: bool,
    /// Window title set by shell OSC 0/2 (vt100 parser exposes it).
    pub title: Option<String>,
    /// Sentinel: the PTY reader hit EOF / error (shell or claude exited).
    /// Carries no screen data — it tells the host pump to reap this pane.
    /// Normal frames leave this false.
    pub eof: bool,
    /// Cursor (row, col) at the moment the shell emitted an OSC 133 `B`
    /// mark (prompt end / command-input start), if this frame contained
    /// one. The host uses it as the authoritative start of the editable
    /// command line for inline autosuggestion. `None` on frames without a
    /// fresh mark — the host keeps the last known one until it goes stale.
    pub prompt_end: Option<(u16, u16)>,
    /// OSC 777 `notify;Title;Body` payload captured this frame, if any. The
    /// host pump drains it into `UserEvent::Notify` (desktop alert + pane
    /// flash) and clears it. `None` on normal frames. This is the second
    /// notification entry point alongside the claude-hook → `kasaterm-cli
    /// notify` path — any shell command can fire one via
    /// `printf '\e]777;notify;Title;Body\a'`.
    pub notify: Option<(String, String)>,
    /// 이번 프레임에 뷰포트와 겹치는 인라인 이미지(OSC 1337)들. 셀-흐름 렌더가
    /// 꺼져 있거나(레거시 탭 모드) 그림이 없으면 빈 목록. 구버전 스냅샷(webterm
    /// 미러 등 직렬화 소비자) 호환 위해 default.
    #[serde(default)]
    pub inline_images: Vec<InlineImageView>,
}

impl Color {
    /// SGR 파라미터. `Default` 는 `None` — 호출부가 매 스타일 변경마다 `0`(reset)
    /// 을 먼저 쓰므로 기본색을 명시할 이유가 없다.
    fn sgr(&self, foreground: bool) -> Option<String> {
        let (base, bright, ext) = if foreground { (30, 90, 38) } else { (40, 100, 48) };
        match self {
            Color::Default => None,
            Color::Idx(i) if *i < 8 => Some((base + *i as u16).to_string()),
            Color::Idx(i) if *i < 16 => Some((bright + (*i as u16 - 8)).to_string()),
            Color::Idx(i) => Some(format!("{ext};5;{i}")),
            Color::Rgb(r, g, b) => Some(format!("{ext};2;{r};{g};{b}")),
        }
    }
}

impl Cell {
    /// 이 셀을 그리기 위한 완전한 SGR 시퀀스(항상 reset 으로 시작).
    ///
    /// 차분 갱신(이전 속성에서 바뀐 것만 끄고 켜기)이 더 짧지만, 끄는 코드를
    /// 하나라도 빠뜨리면 그 속성이 화면 끝까지 번진다. 스냅샷은 접속당 한 번만
    /// 나가므로 길이보다 정확성을 산다.
    fn sgr(&self) -> String {
        let mut p = vec!["0".to_string()];
        if self.bold {
            p.push("1".into());
        }
        if self.dim {
            p.push("2".into());
        }
        if self.italic {
            p.push("3".into());
        }
        if self.underline {
            p.push("4".into());
        }
        if self.inverse {
            p.push("7".into());
        }
        if self.hidden {
            p.push("8".into());
        }
        if let Some(c) = self.fg.sgr(true) {
            p.push(c);
        }
        if let Some(c) = self.bg.sgr(false) {
            p.push(c);
        }
        format!("\x1b[{}m", p.join(";"))
    }

    /// 행 끝에서 잘라내도 화면이 같은가. 배경색·밑줄·반전이 걸린 칸은 눈에
    /// 보이므로 남긴다.
    fn is_trailing_blank(&self) -> bool {
        (self.ch == ' ' || self.ch == '\0')
            && self.bg == Color::Default
            && !self.underline
            && !self.inverse
    }
}

impl ScreenUpdate {
    /// 이 스냅샷을 xterm 이 그대로 먹는 ANSI 바이트로 굽는다.
    ///
    /// 원격 미러가 붙는 순간 화면을 채우는 용도다. 받는 쪽은 자기 VT 파서를 가진
    /// 소비자(브라우저 xterm.js)라, 셀을 JSON 으로 실어 보내고 렌더러를 새로 짜는
    /// 대신 터미널이 이미 아는 언어로 말한다 — 그래서 클라이언트가 이걸 받는 데
    /// 필요한 코드가 0줄이다.
    ///
    /// `dirty` 에 담긴 행만 그리므로, 화면 전체를 원하면 `force_full` 로 뜬
    /// 스냅샷을 넘겨야 한다.
    pub fn to_ansi(&self) -> Vec<u8> {
        let mut out = String::new();
        // 앱이 대체 화면에 있으면 미러도 거기서 시작해야 한다. 안 그러면 앱이
        // 빠져나갈 때 보내는 `?1049l` 이 미러에선 짝이 없는 복귀가 된다.
        if self.alt_screen {
            out.push_str("\x1b[?1049h");
        }
        out.push_str("\x1b[H\x1b[2J");

        for (row, cells) in &self.dirty {
            let end = cells
                .iter()
                .rposition(|c| !c.is_trailing_blank())
                .map_or(0, |i| i + 1);
            if end == 0 {
                continue;
            }
            out.push_str(&format!("\x1b[{};1H", row + 1));
            let mut style: Option<String> = None;
            // 와이드 글자가 먹은 뒷칸을 건너뛰기 위한 표시.
            let mut spacer = false;
            for cell in &cells[..end] {
                // 와이드 글자(한글·CJK)는 두 칸을 차지하고 뒷칸은 스페이서다. 그 칸을
                // 또 쓰면 글자마다 한 칸씩 밀려 자간이 벌어진다.
                //
                // ⚠️ 스페이서를 **문자로는 못 가려낸다** — `convert_cell` 이 alacritty 의
                // `'\0'` 을 공백으로 바꿔 넘기기 때문에 진짜 공백과 똑같이 생겼다. 대신
                // 앞 글자의 폭으로 판정한다: 그리드에서 와이드 글자 다음 칸은 반드시
                // 스페이서이므로(터미널 그리드의 불변식) 이 추론은 항상 맞다.
                if spacer {
                    spacer = false;
                    continue;
                }
                if cell.ch == '\0' {
                    continue;
                }
                let sgr = cell.sgr();
                if style.as_deref() != Some(sgr.as_str()) {
                    out.push_str(&sgr);
                    style = Some(sgr);
                }
                out.push(cell.ch);
                spacer = UnicodeWidthChar::width(cell.ch).unwrap_or(1) > 1;
            }
            out.push_str("\x1b[0m");
        }

        out.push_str(&format!(
            "\x1b[{};{}H",
            self.cursor_row + 1,
            self.cursor_col + 1
        ));
        out.push_str(if self.cursor_visible {
            "\x1b[?25h"
        } else {
            "\x1b[?25l"
        });
        out.into_bytes()
    }
}

pub(crate) fn vt_color(c: vt100::Color) -> Color {
    match c {
        vt100::Color::Default => Color::Default,
        vt100::Color::Idx(i) => Color::Idx(i),
        vt100::Color::Rgb(r, g, b) => Color::Rgb(r, g, b),
    }
}

#[cfg(test)]
mod ansi_tests {
    use super::*;

    fn cell(ch: char) -> Cell {
        Cell { ch, ..Cell::blank() }
    }

    fn upd(dirty: Vec<(u16, Row)>) -> ScreenUpdate {
        ScreenUpdate {
            dirty,
            cursor_visible: true,
            ..Default::default()
        }
    }

    fn ansi(s: &ScreenUpdate) -> String {
        String::from_utf8(s.to_ansi()).unwrap()
    }

    #[test]
    fn clears_then_draws_each_row_at_an_absolute_position() {
        let a = ansi(&upd(vec![
            (0, vec![cell('h'), cell('i')]),
            (2, vec![cell('x')]),
        ]));
        assert!(a.starts_with("\x1b[H\x1b[2J"));
        assert!(a.contains("\x1b[1;1Hi") || a.contains("\x1b[1;1H\x1b[0mhi"));
        // 3번째 행은 1-based 로 3 — 중간 행을 건너뛰어도 자리가 밀리면 안 된다.
        assert!(a.contains("\x1b[3;1H"));
    }

    #[test]
    fn trims_trailing_blanks_but_keeps_visible_ones() {
        let a = ansi(&upd(vec![(0, vec![cell('h'), cell(' '), cell(' ')])]));
        assert!(a.contains('h') && !a.contains("h  "));

        // 배경색이 깔린 칸은 눈에 보이므로 살아남아야 한다.
        let painted = Cell {
            bg: Color::Idx(4),
            ..Cell::blank()
        };
        let a = ansi(&upd(vec![(0, vec![cell('h'), painted])]));
        assert!(a.contains("44"), "배경색 칸이 잘려나갔다: {a:?}");
    }

    #[test]
    fn skips_the_cell_a_wide_char_already_took() {
        // ⚠️ 실제 그리드는 스페이서를 **공백으로** 넘긴다(`convert_cell` 이 alacritty 의
        // '\0' 을 ' ' 로 바꾼다). 그래서 문자로는 진짜 공백과 구분이 안 되고, 앞 글자의
        // 폭으로만 판정할 수 있다. 이걸 놓치면 한글마다 한 칸씩 밀려 자간이 벌어진다.
        let a = ansi(&upd(vec![(0, vec![cell('한'), cell(' '), cell('글')])]));
        assert!(a.contains("한글"), "자간이 벌어졌다: {a:?}");

        // 좁은 글자 뒤의 공백은 진짜 공백이라 남아야 한다.
        let a = ansi(&upd(vec![(0, vec![cell('a'), cell(' '), cell('b')])]));
        assert!(a.contains("a b"), "진짜 공백이 먹혔다: {a:?}");

        // vt100 브리지 경로가 넘기는 옛 '\0' 형태도 계속 건너뛴다.
        let a = ansi(&upd(vec![(0, vec![cell('한'), cell('\0'), cell('x')])]));
        assert!(a.contains("한x"));
    }

    #[test]
    fn emits_one_sgr_per_run_not_per_cell() {
        let a = ansi(&upd(vec![(0, vec![cell('a'), cell('b'), cell('c')])]));
        // 같은 스타일 3칸 → 스타일 1회 + 행 끝 리셋 1회.
        assert_eq!(a.matches("\x1b[0m").count(), 2);
    }

    #[test]
    fn switches_style_mid_row() {
        let bold = Cell {
            ch: 'B',
            bold: true,
            ..Cell::blank()
        };
        let a = ansi(&upd(vec![(0, vec![cell('a'), bold])]));
        assert!(a.contains("\x1b[0;1mB"));
    }

    #[test]
    fn enters_alt_screen_when_the_app_is_there() {
        let mut s = upd(vec![(0, vec![cell('x')])]);
        s.alt_screen = true;
        assert!(ansi(&s).starts_with("\x1b[?1049h"));
        // 대체 화면이 아니면 붙이지 않는다 — 짝 없는 복귀를 만들면 안 된다.
        assert!(!ansi(&upd(vec![])).contains("1049"));
    }

    #[test]
    fn restores_cursor_position_and_visibility_last() {
        let mut s = upd(vec![(0, vec![cell('x')])]);
        s.cursor_row = 4;
        s.cursor_col = 9;
        let a = ansi(&s);
        assert!(a.contains("\x1b[5;10H"), "커서가 1-based 로 안 나왔다: {a:?}");
        assert!(a.ends_with("\x1b[?25h"));

        s.cursor_visible = false;
        assert!(ansi(&s).ends_with("\x1b[?25l"));
    }
}

pub(crate) fn vt_cell(c: &vt100::Cell) -> Cell {
    let contents = c.contents();
    // vt100 returns the cell's grapheme cluster; we keep only the base char
    // (the renderer already shapes a single char per cell). Empty → blank.
    let ch = contents.chars().next().unwrap_or(' ');
    Cell {
        ch,
        fg: vt_color(c.fgcolor()),
        bg: vt_color(c.bgcolor()),
        bold: c.bold(),
        italic: c.italic(),
        underline: c.underline(),
        inverse: c.inverse(),
        dim: false,
        // vt100 crate 는 conceal 미노출 — 이 경로(레거시 브리지)는 마커 채널 없음.
        hidden: false,
    }
}
