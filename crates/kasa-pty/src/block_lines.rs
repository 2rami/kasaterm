//! 명령 블록 출력(OSC 133 C..D 사이 raw 바이트)을 **논리 줄**로 푼다. 거울(PC·폰)이
//! 원본 폭과 상관없이 제 폭으로 다시 접어 그리도록 줄바꿈은 셸이 낸 `\n` 에서만
//! 끊고, 색·굵기는 줄 안의 조각(span)으로 남긴다.
//!
//! 격자 에뮬레이터가 아니라 줄 해석기다 — `\r` 덮어쓰기(진행 막대)·커서 위로(여러 줄
//! 진행 표시)·줄 지우기·화면 지우기까지만 따른다. 절대 좌표 이동은 셸 출력에서 드물고
//! 논리 줄로 옮길 수 없어 버린다. 대체 화면(vim·less) 구간은 화면을 그리는 바이트라
//! 통째로 버린다 — 그 명령은 `is_tui` 로 따로 알린다.

use kasa_screen::screen::Color;

// 격자 전송(`kasa-mcp` gridwire)과 같은 비트 — 폰이 두 길을 같은 규칙으로 푼다.
pub const SPAN_BOLD: u8 = 1;
pub const SPAN_ITALIC: u8 = 2;
pub const SPAN_UNDERLINE: u8 = 4;
pub const SPAN_INVERSE: u8 = 8;
pub const SPAN_DIM: u8 = 16;

/// 한 줄 안에서 같은 모양이 이어지는 글 조각.
#[derive(Clone, Debug, PartialEq)]
pub struct StyledSpan {
    pub text: String,
    pub fg: Color,
    pub bg: Color,
    pub flags: u8,
}

pub type StyledLine = Vec<StyledSpan>;

/// 한 블록이 아무리 길어도 이 줄 수까지만 편다(저장소가 256KB 로 묶여 평소엔 안 닿는다).
const MAX_LINES: usize = 20_000;

/// `Color` 는 Copy 가 아니라 셀마다 복제가 붙는다 — 해석 중엔 Copy 꼴로 들고 끝에 바꾼다.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Ink {
    Default,
    Idx(u8),
    Rgb(u8, u8, u8),
}

impl Ink {
    fn color(self) -> Color {
        match self {
            Ink::Default => Color::Default,
            Ink::Idx(i) => Color::Idx(i),
            Ink::Rgb(r, g, b) => Color::Rgb(r, g, b),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Style {
    fg: Ink,
    bg: Ink,
    flags: u8,
}

const PLAIN: Style = Style { fg: Ink::Default, bg: Ink::Default, flags: 0 };

#[derive(Default)]
struct Sheet {
    lines: Vec<Vec<(char, Style)>>,
    row: usize,
    col: usize,
    saved: (usize, usize),
}

impl Sheet {
    fn line(&mut self) -> &mut Vec<(char, Style)> {
        while self.lines.len() <= self.row {
            self.lines.push(Vec::new());
        }
        &mut self.lines[self.row]
    }

    fn put(&mut self, ch: char, style: Style) {
        let col = self.col;
        let line = self.line();
        while line.len() < col {
            line.push((' ', PLAIN));
        }
        if col < line.len() {
            line[col] = (ch, style);
        } else {
            line.push((ch, style));
        }
        self.col += 1;
    }

    fn newline(&mut self) {
        if self.lines.len() >= MAX_LINES {
            self.lines.remove(0);
            self.row = self.row.saturating_sub(1);
        }
        self.row += 1;
        self.line();
    }

    fn erase_line(&mut self, mode: u16) {
        let col = self.col;
        let line = self.line();
        match mode {
            0 => line.truncate(col),
            1 => {
                for cell in line.iter_mut().take(col + 1) {
                    *cell = (' ', PLAIN);
                }
            }
            _ => line.clear(),
        }
    }

    fn erase_display(&mut self, mode: u16) {
        match mode {
            0 => {
                self.erase_line(0);
                self.lines.truncate(self.row + 1);
            }
            1 => {
                for line in self.lines.iter_mut().take(self.row) {
                    line.clear();
                }
                self.erase_line(1);
            }
            _ => {
                self.lines.clear();
                self.row = 0;
                self.col = 0;
            }
        }
    }
}

/// raw 출력 → 논리 줄. 앞뒤 빈 줄과 줄 끝의 빈칸은 걷는다.
pub fn block_lines(raw: &str) -> Vec<StyledLine> {
    let mut sheet = Sheet::default();
    let mut style = PLAIN;
    let mut alt = false;
    let chars: Vec<char> = raw.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        i += 1;
        match c {
            '\x1b' => {
                let Some(&next) = chars.get(i) else { break };
                i += 1;
                match next {
                    '[' => {
                        let start = i;
                        while i < chars.len() && !('\x40'..='\x7e').contains(&chars[i]) {
                            i += 1;
                        }
                        let Some(&fin) = chars.get(i) else { break };
                        i += 1;
                        let params: String = chars[start..i - 1].iter().collect();
                        csi(&mut sheet, &mut style, &mut alt, &params, fin);
                    }
                    // OSC·DCS·APC·PM·SOS: 끝(BEL 또는 ST)까지 건너뛴다.
                    ']' | 'P' | '_' | '^' | 'X' => {
                        while i < chars.len() {
                            if chars[i] == '\x07' {
                                i += 1;
                                break;
                            }
                            if chars[i] == '\x1b' && chars.get(i + 1) == Some(&'\\') {
                                i += 2;
                                break;
                            }
                            i += 1;
                        }
                    }
                    '(' | ')' | '*' | '+' | '#' | '%' => i += 1,
                    '7' => sheet.saved = (sheet.row, sheet.col),
                    '8' => (sheet.row, sheet.col) = sheet.saved,
                    'M' if !alt => sheet.row = sheet.row.saturating_sub(1),
                    'c' if !alt => {
                        sheet.erase_display(2);
                        style = PLAIN;
                    }
                    _ => {}
                }
            }
            _ if alt => {}
            '\n' => {
                sheet.newline();
                // 셸 출력은 onlcr 로 `\r\n` 이 오지만 맨 `\n` 도 새 줄 맨 앞으로 친다.
                sheet.col = 0;
            }
            '\r' => sheet.col = 0,
            '\x08' => sheet.col = sheet.col.saturating_sub(1),
            '\t' => {
                let to = (sheet.col / 8 + 1) * 8;
                while sheet.col < to {
                    sheet.put(' ', style);
                }
            }
            c if (c as u32) < 0x20 || c == '\x7f' => {}
            c => sheet.put(c, style),
        }
    }
    finish(sheet.lines)
}

fn csi(sheet: &mut Sheet, style: &mut Style, alt: &mut bool, params: &str, fin: char) {
    if let Some(private) = params.strip_prefix('?') {
        if fin == 'h' || fin == 'l' {
            let modes = private.split(';').filter_map(|p| p.parse::<u16>().ok());
            if modes.into_iter().any(|m| matches!(m, 47 | 1047 | 1049)) {
                *alt = fin == 'h';
            }
        }
        return;
    }
    if *alt || params.starts_with(['>', '<', '=']) {
        return;
    }
    let nums: Vec<u16> = params
        .split(';')
        .map(|p| p.split(':').next().unwrap_or("").parse::<u16>().unwrap_or(0))
        .collect();
    let n = |k: usize| nums.get(k).copied().filter(|v| *v > 0).unwrap_or(1) as usize;
    match fin {
        'm' => sgr(style, params),
        'K' => sheet.erase_line(nums.first().copied().unwrap_or(0)),
        'J' => sheet.erase_display(nums.first().copied().unwrap_or(0)),
        'A' => sheet.row = sheet.row.saturating_sub(n(0)),
        'B' => {
            for _ in 0..n(0) {
                sheet.newline();
            }
        }
        'C' => sheet.col += n(0),
        'D' => sheet.col = sheet.col.saturating_sub(n(0)),
        'G' | '`' => sheet.col = n(0) - 1,
        'E' => {
            for _ in 0..n(0) {
                sheet.newline();
            }
            sheet.col = 0;
        }
        'F' => {
            sheet.row = sheet.row.saturating_sub(n(0));
            sheet.col = 0;
        }
        's' => sheet.saved = (sheet.row, sheet.col),
        'u' => (sheet.row, sheet.col) = sheet.saved,
        _ => {}
    }
}

fn sgr(style: &mut Style, params: &str) {
    if params.is_empty() {
        *style = PLAIN;
        return;
    }
    // `38:2::r:g:b` 같은 콜론 꼴도 세미콜론 꼴과 같은 목록으로 펴 읽는다.
    let parts: Vec<Vec<u16>> = params
        .split(';')
        .map(|p| p.split(':').map(|v| v.parse::<u16>().unwrap_or(0)).collect())
        .collect();
    let mut k = 0;
    while k < parts.len() {
        let sub = &parts[k];
        let code = sub.first().copied().unwrap_or(0);
        match code {
            0 => *style = PLAIN,
            1 => style.flags |= SPAN_BOLD,
            2 => style.flags |= SPAN_DIM,
            3 => style.flags |= SPAN_ITALIC,
            4 => {
                if sub.get(1) == Some(&0) {
                    style.flags &= !SPAN_UNDERLINE;
                } else {
                    style.flags |= SPAN_UNDERLINE;
                }
            }
            7 => style.flags |= SPAN_INVERSE,
            21 | 22 => style.flags &= !(SPAN_BOLD | SPAN_DIM),
            23 => style.flags &= !SPAN_ITALIC,
            24 => style.flags &= !SPAN_UNDERLINE,
            27 => style.flags &= !SPAN_INVERSE,
            30..=37 => style.fg = Ink::Idx((code - 30) as u8),
            39 => style.fg = Ink::Default,
            40..=47 => style.bg = Ink::Idx((code - 40) as u8),
            49 => style.bg = Ink::Default,
            90..=97 => style.fg = Ink::Idx((code - 90 + 8) as u8),
            100..=107 => style.bg = Ink::Idx((code - 100 + 8) as u8),
            38 | 48 => {
                let (color, used) = if sub.len() > 1 {
                    (extended(&sub[1..]), 0)
                } else {
                    let rest: Vec<u16> = parts[k + 1..].iter().map(|p| p.first().copied().unwrap_or(0)).collect();
                    let color = extended(&rest);
                    let used = match rest.first() {
                        Some(5) => 2,
                        Some(2) => 4,
                        _ => 0,
                    };
                    (color, used)
                };
                if let Some(color) = color {
                    if code == 38 {
                        style.fg = color;
                    } else {
                        style.bg = color;
                    }
                }
                k += used;
            }
            _ => {}
        }
        k += 1;
    }
}

/// `5;n` 또는 `2;r;g;b`(콜론 꼴은 색 공간 자리가 비어 `2;;r;g;b` 로 올 수 있다).
fn extended(v: &[u16]) -> Option<Ink> {
    match v.first()? {
        5 => Some(Ink::Idx(*v.get(1)? as u8)),
        2 => {
            let rgb = if v.len() >= 5 { &v[2..5] } else { v.get(1..4)? };
            Some(Ink::Rgb(rgb[0] as u8, rgb[1] as u8, rgb[2] as u8))
        }
        _ => None,
    }
}

fn finish(lines: Vec<Vec<(char, Style)>>) -> Vec<StyledLine> {
    let mut out: Vec<StyledLine> = lines
        .into_iter()
        .map(|mut cells| {
            while cells.last().is_some_and(|(c, s)| *c == ' ' && s.bg == Ink::Default && s.flags & SPAN_INVERSE == 0) {
                cells.pop();
            }
            let mut spans: StyledLine = Vec::new();
            for (ch, s) in cells {
                match spans.last_mut() {
                    Some(last) if last.fg == s.fg.color() && last.bg == s.bg.color() && last.flags == s.flags => last.text.push(ch),
                    _ => spans.push(StyledSpan { text: ch.to_string(), fg: s.fg.color(), bg: s.bg.color(), flags: s.flags }),
                }
            }
            spans
        })
        .collect();
    while out.last().is_some_and(|l| l.is_empty()) {
        out.pop();
    }
    let lead = out.iter().take_while(|l| l.is_empty()).count();
    out.drain(..lead);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(lines: &[StyledLine]) -> Vec<String> {
        lines.iter().map(|l| l.iter().map(|s| s.text.as_str()).collect()).collect()
    }

    #[test]
    fn colors_survive_as_spans() {
        let lines = block_lines("\x1b[01;34mdocs\x1b[0m  \x1b[32mrun.sh\x1b[0m\r\n");
        assert_eq!(text(&lines), vec!["docs  run.sh"]);
        assert_eq!(lines[0][0], StyledSpan { text: "docs".into(), fg: Color::Idx(4), bg: Color::Default, flags: SPAN_BOLD });
        assert_eq!(lines[0][2].fg, Color::Idx(2));
    }

    #[test]
    fn carriage_return_overwrites_progress() {
        let lines = block_lines("받는 중  10%\r받는 중  55%\r받는 중 100%\r\n끝\r\n");
        assert_eq!(text(&lines), vec!["받는 중 100%", "끝"]);
    }

    #[test]
    fn cursor_up_redraws_multi_line_progress() {
        let lines = block_lines("a 1/3\r\nb 1/3\r\n\x1b[2A\x1b[Ka 3/3\r\n\x1b[Kb 3/3\r\n");
        assert_eq!(text(&lines), vec!["a 3/3", "b 3/3"]);
    }

    #[test]
    fn alt_screen_run_is_dropped() {
        let lines = block_lines("before\r\n\x1b[?1049h\x1b[H\x1b[2Jvim screen\x1b[?1049lafter\r\n");
        assert_eq!(text(&lines), vec!["before", "after"]);
    }

    #[test]
    fn clear_inside_block_resets() {
        let lines = block_lines("old\r\n\x1b[H\x1b[2Jnew\r\n");
        assert_eq!(text(&lines), vec!["new"]);
    }

    #[test]
    fn truecolor_and_256_and_osc_links() {
        let lines = block_lines("\x1b[38;2;255;0;10mR\x1b[48;5;236mB\x1b[0m \x1b]8;;http://x\x1b\\link\x1b]8;;\x1b\\\r\n");
        assert_eq!(text(&lines), vec!["RB link"]);
        assert_eq!(lines[0][0].fg, Color::Rgb(255, 0, 10));
        assert_eq!(lines[0][1].bg, Color::Idx(236));
    }

    #[test]
    fn tabs_and_backspace() {
        let lines = block_lines("a\tb\r\nxy\x08z\r\n");
        assert_eq!(text(&lines), vec!["a       b", "xz"]);
    }
}
