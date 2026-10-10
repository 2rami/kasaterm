//! alacritty 격자 → `kasa_screen` 행·프레임 변환과 스크롤백 ANSI 재생. 세션 상태 없이
//! `Term` 만 읽는 순수 함수들이다.

use super::*;
#[cfg(test)]
use super::reader::Utf8Buffer;
#[cfg(test)]
use super::vt::make_term;

/// 에이전트가 확정된 사용자 프롬프트 행 머리에 남기는 마커 — claude 는 U+276F,
/// codex 는 U+203A 를 같은 자리에 쓴다(2026-09-03 실측).
///
/// ASCII `>` 는 **일부러 제외한다** — diff·인용·다른 TUI 가 행 머리에 흔히 써서,
/// 그것까지 마커로 치면 대화와 무관한 줄이 턴 목록에 섞인다(같은 이유로
/// `screenread::prompt_box` 도 `❯`/`›` 만 인정한다).
const PROMPT_MARKERS: [char; 2] = ['\u{276f}', '\u{203a}'];

/// 그리드 전체(스크롤백+화면)에서 프롬프트 줄을 훑는다. `PtySession::prompt_anchors`
/// 의 알맹이이자, 시험이 살아 있는 PTY 없이 부를 수 있는 진입점이다.
pub(super) fn scan_prompt_anchors(term: &Term<PtyEventForwarder>) -> Vec<PromptAnchor> {
    use alacritty_terminal::index::{Column, Line};
    use alacritty_terminal::term::cell::Flags;
    let grid = term.grid();
    let cols = grid.columns();
    let hist = grid.history_size();
    let screen = grid.screen_lines();
    // 커서가 앉아 있는 줄 = 지금 치고 있는 입력줄. **codex 를 가르는 유일한 단서다.**
    // claude 는 입력줄 마커 뒤에 NBSP 를 두어 아래 규칙으로 갈리지만, codex 는 확정된
    // 질문과 똑같이 일반 공백을 써서(2026-09-03 실측) 그 규칙이 안 통한다. 그래서
    // 마커별로 다른 단서를 본다 — 각 에이전트가 실제로 남기는 표시가 다르니 판정도
    // 그것을 따른다. 이 한 줄이 자리표시자(`› Ask Codex to do anything`)와 타이핑
    // 중인 글이 지나간 질문 목록에 끼는 것을 막는다.
    let cursor_line = grid.cursor.point.line.0;
    let mut out = Vec::new();
    for i in 0..(hist + screen) {
        let line = i as i32 - hist as i32;
        let marker = grid[Point::new(Line(line), Column(0))].c;
        if !PROMPT_MARKERS.contains(&marker) {
            continue;
        }
        if cols > 1 && grid[Point::new(Line(line), Column(1))].c == '\u{a0}' {
            continue;
        }
        if marker == '\u{203a}' && line == cursor_line {
            continue;
        }
        let mut text = String::new();
        for c in 1..cols {
            let cell = &grid[Point::new(Line(line), Column(c))];
            // wide 글리프가 차지한 **뒤칸을 건너뛴다**. 그 칸의 문자는 `\0` 이 아니라
            // 진짜 `' '` 이고 구분은 플래그에만 있어서, 문자만 보고 거르면 한글마다
            // 한 칸씩 벌어진 「질 문  1」이 나온다(2026-08-15 실측). 웹터미널이 같은
            // 자리에서 물렸던 함정과 같은 것이다.
            if cell.flags.contains(Flags::WIDE_CHAR_SPACER) || cell.c == '\0' {
                continue;
            }
            text.push(cell.c);
        }
        let text = text.trim().to_string();
        if text.is_empty() {
            continue;
        }
        out.push(PromptAnchor { abs_line: i as i64, text });
    }
    out
}

/// 절대 줄 `abs` 의 셀들 — `PtySession::row_at_abs` 의 알맹이.
///
/// 좌표 규약은 `scan_prompt_anchors` 와 같다(그리드 줄 = `abs - history_size`). 두
/// 셈이 어긋나면 머리줄이 **한 줄 옆을 그린다** — 눌러 가 보기 전에는 티가 안 나는
/// 부류라, 앵커를 만드는 쪽과 읽는 쪽이 같은 식을 쓰게 붙여 둔다.
pub(super) fn read_row_at_abs(term: &Term<PtyEventForwarder>, abs: i64) -> Option<Row> {
    let g = term.grid();
    let hist = g.history_size() as i64;
    let line = abs - hist;
    if line < -hist || line >= g.screen_lines() as i64 {
        return None;
    }
    let cols = g.columns();
    let mut row: Row = Vec::with_capacity(cols);
    for c in 0..cols {
        row.push(convert_cell(
            &g[Point::new(alacritty_terminal::index::Line(line as i32), alacritty_terminal::index::Column(c))],
        ));
    }
    Some(row)
}

/// 뷰포트 바로 위 스크롤백 행들 — `PtySession::rows_above` 의 알맹이이자, 시험이
/// 살아 있는 PTY 없이 부를 수 있는 진입점(`scan_prompt_anchors` 와 같은 모양).
/// 뷰포트 첫 행은 그리드 줄 `-display_offset` 이므로(스냅샷과 같은 셈) 그 위는
/// `-display_offset - 1` 부터 `-history_size` 까지, 가까운 순으로 담는다.
pub(super) fn read_rows_above(term: &Term<PtyEventForwarder>, n: usize) -> Vec<Row> {
    let g = term.grid();
    let cols = g.columns();
    let bottom = -(g.history_size() as i32);
    let mut line = -(g.display_offset() as i32) - 1;
    let mut out = Vec::new();
    while line >= bottom && out.len() < n {
        let mut row: Row = Vec::with_capacity(cols);
        for c in 0..cols {
            let point = Point::new(
                alacritty_terminal::index::Line(line),
                alacritty_terminal::index::Column(c),
            );
            row.push(convert_cell(&g[point]));
        }
        out.push(row);
        line -= 1;
    }
    out
}

/// 살아 있는 화면 **바로 위**의 스크롤백 행들 — `read_rows_above` 와 같되 스크롤 위치를
/// 무시한다. 폰 거울이 쓴다: 데스크톱 사람이 스크롤을 올려 둔 것과 상관없이, 프로그램이
/// 지금 그리는 화면 위로 지난 줄이 이어져야 폰에서 위로 넘길 수 있다.
pub(super) fn read_rows_above_live(term: &Term<PtyEventForwarder>, n: usize) -> Vec<Row> {
    let g = term.grid();
    let cols = g.columns();
    let bottom = -(g.history_size() as i32);
    let mut line = -1;
    let mut out = Vec::new();
    while line >= bottom && out.len() < n {
        let mut row: Row = Vec::with_capacity(cols);
        for c in 0..cols {
            let point = Point::new(
                alacritty_terminal::index::Line(line),
                alacritty_terminal::index::Column(c),
            );
            row.push(convert_cell(&g[point]));
        }
        out.push(row);
        line -= 1;
    }
    out
}

/// 살아 있는 화면(스크롤 위치와 무관한 맨 아래 화면)의 마지막 `n` 행 — 위→아래 순.
///
/// `read_rows_above` 의 거울이다. 그쪽은 뷰포트 **위** 스크롤백을 보고, 이쪽은
/// 스크롤을 얼마나 올렸든 지금 프로그램이 그리고 있는 화면의 **꼬리**를 본다.
/// 그리드 줄 번호는 display_offset 과 무관하므로(뷰포트 r 행 = 줄 `r - offset`)
/// 화면 마지막 줄은 언제나 `screen_lines - 1` 이다.
pub(super) fn read_live_tail(term: &Term<PtyEventForwarder>, n: usize) -> Vec<Row> {
    let g = term.grid();
    let cols = g.columns();
    let lines = g.screen_lines();
    let start = lines.saturating_sub(n);
    let mut out = Vec::with_capacity(lines - start);
    for line in start..lines {
        let mut row: Row = Vec::with_capacity(cols);
        for c in 0..cols {
            let point = Point::new(
                alacritty_terminal::index::Line(line as i32),
                alacritty_terminal::index::Column(c),
            );
            row.push(convert_cell(&g[point]));
        }
        out.push(row);
    }
    out
}

/// 스크롤백을 접속 스냅샷에 싣는 ANSI. 히스토리 줄을 보통 출력처럼 위에서부터
/// 흘리고, 화면에 걸쳐 남은 꼬리를 바닥 행 개행으로 밀어낸 뒤(바닥에서의 개행만
/// 스크롤을 만든다) 이어지는 `to_ansi` 가 화면을 다시 그린다 — 그러면 받는 쪽
/// xterm 은 히스토리를 자기 스크롤백으로 쌓는다. 이게 없으면 미러는 뷰포트만
/// 받아서 폰에서 스와이프해도 올라갈 데가 없다(2026-08-20 확정).
///
/// Native mirrors retain more than xterm's old 1000-line default. A long tool
/// turn must not lose its question on attach; bound both rows and cell volume.
fn exported_history_rows(history: usize, cols: u16) -> usize {
    history.min(10_000).min(1_000_000 / usize::from(cols.max(1)))
}

pub(super) fn history_ansi(term: &Term<PtyEventForwarder>, cols: u16, rows: u16) -> Vec<u8> {
    let hist = exported_history_rows(term.grid().history_size(), cols);
    if hist == 0 {
        return Vec::new();
    }
    let grid = term.grid();
    let grid_cols = grid.columns().min(cols as usize);
    let mut out = String::new();
    for line in -(hist as i32)..0 {
        let mut row: Row = Vec::with_capacity(grid_cols);
        for c in 0..grid_cols {
            let point = Point::new(
                alacritty_terminal::index::Line(line),
                alacritty_terminal::index::Column(c),
            );
            row.push(convert_cell(&grid[point]));
        }
        if let Some(body) = kasa_screen::screen::row_ansi(&row) {
            out.push_str(&body);
        }
        if row.last().is_some_and(|cell| cell.wrapped) {
            // Full-width row_ansi + HT commits the source's actual soft wrap.
            // Hard CRLF rows remain hard breaks, never guessed from text width.
            let leading_wide = grid[Point::new(
                alacritty_terminal::index::Line(line),
                alacritty_terminal::index::Column(grid_cols - 1),
            )].flags.contains(alacritty_terminal::term::cell::Flags::LEADING_WIDE_CHAR_SPACER);
            out.push_str(&kasa_screen::screen::row_wrap_ansi(&row, leading_wide));
        } else {
            out.push_str("\r\n");
        }
    }
    out.push_str(&format!("\x1b[{};1H", rows.max(1)));
    // Every serialized history row already advanced once. At most rows-1
    // historical rows remain visible; scrolling rows would add a phantom blank.
    for _ in 0..hist.min(rows.saturating_sub(1) as usize) {
        out.push('\n');
    }
    out.into_bytes()
}

/// The frame carries its own wide-wrap metadata, independent of the source
/// GUI's display offset. Reconstruct those flags from ordinary ANSI on replay.
pub(super) fn raw_screen_ansi(frame: &ScreenUpdate) -> Vec<u8> {
    let leading_wide_rows: Vec<u16> = frame.dirty.iter().filter_map(|(row, cells)| {
        cells.last()?.leading_wide_spacer.then_some(*row)
    }).collect();
    frame.to_ansi_with_wide_spacers(&leading_wide_rows)
}

pub(super) fn snapshot(
    term: &mut Term<PtyEventForwarder>,
    cols: u16,
    rows: u16,
    pane_id: &str,
    last_title: &Arc<Mutex<Option<String>>>,
    // When false, only the lines alacritty marked damaged since the last
    // reset are rebuilt — a 1-char echo touches ~1 line instead of the
    // whole grid (180us → ~10us). The renderer keys ScreenUpdate.dirty by
    // row and leaves untouched rows alone, so a partial list is correct.
    // Callers that change the *whole* view (scroll, resize) pass true.
    force_full: bool,
) -> ScreenUpdate {
    // 히스토리로 몇 줄 올라가 있는지 + 스크롤백이 얼마나 깊은지. 화면 행 r 은 그리드
    // 줄 `r - display_offset` 이고, 그 값이 음수면 히스토리다(0 이 화면 첫 줄).
    // `damage()` 의 &mut 대여 전에 읽는다.
    let display_offset = term.grid().display_offset() as i32;
    // Which visual rows to rebuild. damage() yields viewport-relative
    // line numbers (already display_offset-adjusted), and returns Full
    // on first frame / resize / scroll, which we expand to every row.
    let damaged: Vec<u16> = if force_full {
        (0..rows).collect()
    } else {
        match term.damage() {
            TermDamage::Full => (0..rows).collect(),
            TermDamage::Partial(iter) => {
                let mut v: Vec<u16> =
                    iter.map(|b| b.line as u16).filter(|&r| r < rows).collect();
                v.sort_unstable();
                v.dedup();
                v
            }
        }
    };
    term.reset_damage();
    build_update(term, cols, rows, pane_id, last_title, &damaged, display_offset)
}

/// 뷰포트(`display_offset`)와 무관하게 **살아 있는 화면** 전체를 담는다 — 거울(폰·웹텀)용.
/// GUI 스트림은 데스크톱이 스크롤백을 올려다보면 그 창을 싣는데, 거울은 스크롤이 따로
/// 있어서 그걸 받으면 입력상자·상태줄이 통째로 사라진다(2026-09-06 「맥북에서 올리면
/// 폰에서 하단바도 없어져」). damage 는 건드리지 않는다 — 그건 GUI 스트림의 것.
pub(super) fn live_snapshot(
    term: &Term<PtyEventForwarder>,
    cols: u16,
    rows: u16,
    pane_id: &str,
    last_title: &Arc<Mutex<Option<String>>>,
) -> ScreenUpdate {
    let damaged: Vec<u16> = (0..rows).collect();
    build_update(term, cols, rows, pane_id, last_title, &damaged, 0)
}

pub(super) fn build_update(
    term: &Term<PtyEventForwarder>,
    cols: u16,
    rows: u16,
    pane_id: &str,
    last_title: &Arc<Mutex<Option<String>>>,
    damaged: &[u16],
    display_offset: i32,
) -> ScreenUpdate {
    let topmost = -(term.grid().history_size() as i32);
    let grid = term.grid();
    let mut dirty: Vec<(u16, Row)> = Vec::with_capacity(damaged.len());
    for &r in damaged {
        let mut row: Row = Vec::with_capacity(cols as usize);
        // Clamp to the grid's real dimensions: a resize updates `size` and the
        // Term grid under separate locks, so for a frame they can disagree by a
        // column/line. Indexing `cols` (from `size`) into a grid that's one
        // smaller panics (OOB). Fill the overshoot with blanks — the next frame
        // repaints correctly, and we never crash on the race.
        let grid_cols = grid.columns();
        let grid_lines = grid.screen_lines();
        // ⚠️**음수 line 을 막지 마라.** alacritty 는 스크롤백을 음수 `Line` 으로
        // 노출한다(`topmost_line()` = `-history_size`). 예전엔 `line >= 0` 만
        // 인정해 히스토리를 통째로 빈칸으로 채웠고, 그래서 위로 N줄 올리면 윗줄
        // N개가 비고 화면 높이보다 많이 올리면 화면이 통째로 비었다 —
        // 「위로 올려도 위에 게 없어진다」의 정체였다(2026-08-11 확정).
        let line = r as i32 - display_offset;
        let line_ok = line >= topmost && line < grid_lines as i32;
        for c in 0..cols {
            if line_ok && (c as usize) < grid_cols {
                let point = Point::new(
                    alacritty_terminal::index::Line(line),
                    alacritty_terminal::index::Column(c as usize),
                );
                row.push(convert_cell(&grid[point]));
            } else {
                row.push(Cell::blank());
            }
        }
        dirty.push((r, row));
    }
    let cursor = term.grid().cursor.point;
    let cursor_row = cursor.line.0.max(0) as u16;
    let cursor_col = cursor.column.0 as u16;
    let mode = term.mode();
    // Hide the cursor while scrolled into history — the live cursor
    // sits at the bottom of the active area, which isn't where the
    // user is looking, so drawing it over scrollback is misleading.
    let cursor_visible = display_offset == 0
        && mode.contains(alacritty_terminal::term::TermMode::SHOW_CURSOR);
    let alt_screen = mode.contains(alacritty_terminal::term::TermMode::ALT_SCREEN);
    let mouse_enabled = mode.contains(alacritty_terminal::term::TermMode::MOUSE_REPORT_CLICK)
        || mode.contains(alacritty_terminal::term::TermMode::MOUSE_DRAG)
        || mode.contains(alacritty_terminal::term::TermMode::MOUSE_MOTION);
    let mouse_sgr = mode.contains(alacritty_terminal::term::TermMode::SGR_MOUSE);
    let mouse_motion = mode.contains(alacritty_terminal::term::TermMode::MOUSE_MOTION);
    let app_cursor = mode.contains(alacritty_terminal::term::TermMode::APP_CURSOR);
    let bracketed_paste =
        mode.contains(alacritty_terminal::term::TermMode::BRACKETED_PASTE);
    // OSC 0 / OSC 2 title pushed by the inner program. Cached in the
    // forwarder so we can return the latest value on every snapshot
    // rather than draining alacritty's pending-title queue once and
    // losing it.
    let title: Option<String> = last_title.lock().ok().and_then(|t| t.clone());
    ScreenUpdate {
        live_output: false,
        output_generation: 0,
        pane_id: pane_id.to_string(),
        rows,
        cols,
        dirty,
        cursor_row,
        cursor_col,
        cursor_visible,
        alt_screen,
        mouse_enabled,
        mouse_sgr,
        mouse_motion,
        app_cursor,
        bracketed_paste,
        title,
        eof: false,
        // Filled in by the reader thread when this batch carried an
        // OSC 133 `B` mark — snapshot() itself doesn't parse the stream.
        prompt_end: None,
        // Likewise stamped by the reader when an OSC 777 notify was sniffed.
        notify: None,
        // 뷰포트 배치는 스냅샷 뒤 attach_inline_views 가 채운다 — snapshot 은
        // Term 만 알고 이미지 기록(InlineImgs)을 모른다.
        inline_images: Vec::new(),
    }
}

fn convert_cell(cell: &alacritty_terminal::term::cell::Cell) -> Cell {
    // kitty 자리표시는 그림이 덮는 빈칸이다. 글자로 넘기면 GUI·거울·폰이 없는
    // 글리프(두부)를 그린다 — 그림이 없는 쪽(원격 거울·웹)도 자리만 비워 둔다.
    let ch = if cell.c == '\0' || cell.c == crate::kitty::PLACEHOLDER { ' ' } else { cell.c };
    Cell {
        ch,
        fg: convert_color(cell.fg),
        bg: convert_color(cell.bg),
        bold: cell
            .flags
            .contains(alacritty_terminal::term::cell::Flags::BOLD),
        italic: cell
            .flags
            .contains(alacritty_terminal::term::cell::Flags::ITALIC),
        underline: cell
            .flags
            .contains(alacritty_terminal::term::cell::Flags::UNDERLINE),
        inverse: cell
            .flags
            .contains(alacritty_terminal::term::cell::Flags::INVERSE),
        dim: cell
            .flags
            .contains(alacritty_terminal::term::cell::Flags::DIM),
        hidden: cell
            .flags
            .contains(alacritty_terminal::term::cell::Flags::HIDDEN),
        wrapped: cell
            .flags
            .contains(alacritty_terminal::term::cell::Flags::WRAPLINE),
        leading_wide_spacer: cell
            .flags
            .contains(alacritty_terminal::term::cell::Flags::LEADING_WIDE_CHAR_SPACER),
    }
}

fn convert_color(c: VtColor) -> Color {
    match c {
        VtColor::Named(NamedColor::Foreground) | VtColor::Named(NamedColor::Background) => {
            Color::Default
        }
        VtColor::Named(n) => Color::Idx(n as u8),
        VtColor::Spec(rgb) => Color::Rgb(rgb.r, rgb.g, rgb.b),
        VtColor::Indexed(i) => Color::Idx(i),
    }
}

#[cfg(test)]
mod leading_wide_spacer_conversion_tests {
    use super::convert_cell;
    use alacritty_terminal::term::cell::{Cell, Flags};

    #[test]
    fn source_padding_flag_is_not_confused_with_real_spaces_or_trailing_spacers() {
        let mut source = Cell::default();
        assert!(!convert_cell(&source).leading_wide_spacer);
        source.flags.insert(Flags::WIDE_CHAR_SPACER);
        assert!(!convert_cell(&source).leading_wide_spacer);
        source.flags = Flags::LEADING_WIDE_CHAR_SPACER | Flags::WRAPLINE;
        let converted = convert_cell(&source);
        assert!(converted.leading_wide_spacer);
        assert!(converted.wrapped);
    }
}

#[cfg(test)]
mod raw_snapshot_wrap_tests {
    use super::*;
    use alacritty_terminal::index::{Column, Line};
    use alacritty_terminal::term::cell::Flags;

    fn parser(cols: u16, rows: u16) -> Term<PtyEventForwarder> {
        make_term(cols, rows, PtyEventForwarder {
            respond: false,
            writer: Arc::new(Mutex::new(Box::new(std::io::sink()))),
            size: Arc::new(Mutex::new((cols, rows))),
            last_title: Arc::new(Mutex::new(None)),
        })
    }

    fn feed(term: &mut Term<PtyEventForwarder>, raw: &[u8]) {
        let mut processor: Processor<StdSyncHandler> = Processor::new();
        processor.advance(term, raw);
    }

    fn assert_snapshot(source: &mut Term<PtyEventForwarder>, cols: u16, rows: u16, history: bool) {
        let title = Arc::new(Mutex::new(None));
        let frame = snapshot(source, cols, rows, "%wrap-test", &title, true);
        let mut bytes = if history && !frame.alt_screen { history_ansi(source, cols, rows) } else { Vec::new() };
        bytes.extend(raw_screen_ansi(&frame));
        let mut replay = parser(cols, rows);
        feed(&mut replay, &bytes);
        let actual = snapshot(&mut replay, cols, rows, "%wrap-test", &title, true);
        assert_eq!((actual.cursor_row, actual.cursor_col, actual.cursor_visible, actual.alt_screen),
            (frame.cursor_row, frame.cursor_col, frame.cursor_visible, frame.alt_screen));
        let mut expected = frame.dirty.clone();
        // A wrap into a row outside this viewport is intentionally not committed:
        // doing so would scroll away content, including on 1- and 2-row viewers.
        if let Some((_, row)) = expected.last_mut() {
            if let Some(last) = row.last_mut() { last.wrapped = false; }
        }
        assert_eq!(actual.dirty, expected, "raw ANSI roundtrip changed screen cells");
        for line in 0..rows as i32 {
            for col in 0..cols as usize {
                let point = Point::new(Line(line), Column(col));
                let mut expected = source.grid()[point].clone();
                if line == rows as i32 - 1 {
                    expected.flags.remove(Flags::WRAPLINE | Flags::LEADING_WIDE_CHAR_SPACER);
                }
                assert_eq!(replay.grid()[point], expected, "raw grid cell {line}:{col} changed");
            }
        }
        if history && !frame.alt_screen {
            let count = exported_history_rows(source.grid().history_size(), cols);
            assert_eq!(replay.grid().history_size(), count, "history gained/lost a row");
            for line in -(count as i32)..0 {
                for col in 0..cols as usize {
                    let point = Point::new(Line(line), Column(col));
                    assert_eq!(replay.grid()[point], source.grid()[point], "history cell {line}:{col} changed");
                }
            }
        }
    }

    #[test]
    fn preserves_soft_wraps_but_not_hard_crlf() {
        let mut source = parser(8, 6);
        feed(&mut source, b"abcdefghIJ\r\n12345678\r\nHARD\x1b[2;3H");
        assert!(source.grid()[Line(0)][Column(7)].flags.contains(Flags::WRAPLINE));
        assert!(!source.grid()[Line(2)][Column(7)].flags.contains(Flags::WRAPLINE));
        assert_snapshot(&mut source, 8, 6, false);
    }

    #[test]
    fn mirror_attach_keeps_question_before_a_long_tool_turn() {
        let mut source = parser(70, 12);
        feed(&mut source, "› recent question\r\n".as_bytes());
        for _ in 0..1500 { feed(&mut source, b"tool output\r\n"); }
        let bytes = history_ansi(&source, 70, 12);
        assert!(String::from_utf8_lossy(&bytes).contains("recent question"));
        assert_snapshot(&mut source, 70, 12, true);
        assert_eq!(exported_history_rows(100_000, 80), 10_000);
        assert_eq!(exported_history_rows(100_000, 400), 2_500);
    }

    #[test]
    fn preserves_wrap_into_blank_row_without_inserting_a_glyph() {
        let mut source = parser(8, 4);
        feed(&mut source, b"abcdefghi\x1b[2;1H\x1b[2K\x1b[1;2H");
        assert_snapshot(&mut source, 8, 4, false);
    }

    #[test]
    fn preserves_wrapped_blank_rows_and_styled_trailing_spaces() {
        let mut source = parser(8, 5);
        feed(&mut source, b"        X\r\n\x1b[1;3;31mA   \x1b[0m\x1b[1;1H");
        assert_snapshot(&mut source, 8, 5, false);
    }

    #[test]
    fn preserves_cjk_margin_spacers_and_style_runs() {
        let mut source = parser(5, 6);
        feed(&mut source, "abcd한글\x1b[1;4;38;2;40;50;60m가나\x1b[0mZ\x1b[2;2H".as_bytes());
        assert_snapshot(&mut source, 5, 6, false);
    }

    #[test]
    fn preserves_leading_wide_spacers_in_history_and_tiny_views() {
        for cols in [2, 3, 5] {
            for rows in [1, 2, 6] {
                let mut source = parser(cols, rows);
                feed(&mut source, "\x1b[1;3;4;7;38;2;10;20;30;48;2;40;50;60mabcd한글가나다라마바사아자차\x1b[0m".as_bytes());
                assert_snapshot(&mut source, cols, rows, true);
            }
        }
    }

    #[test]
    fn preserves_leading_wide_spacer_when_next_row_was_erased() {
        let mut source = parser(5, 3);
        feed(&mut source, "abcd한\r\x1b[2K\x1b[1;2H".as_bytes());
        assert!(source.grid()[Line(0)][Column(4)].flags.contains(Flags::LEADING_WIDE_CHAR_SPACER));
        assert_snapshot(&mut source, 5, 3, false);
    }

    #[test]
    fn preserves_historical_wraps_and_boundary_into_visible_screen() {
        let mut source = parser(8, 3);
        feed(&mut source, b"abcdefghABCDEFGHijklmnopIJKLMNOPqrstuvwxQRSTUVWXyz");
        assert_snapshot(&mut source, 8, 3, true);
    }

    #[test]
    fn keeps_hard_history_lines_separate() {
        let mut source = parser(8, 3);
        feed(&mut source, b"aaaaaaaa\r\nbbbbbbbb\r\ncccccccc\r\ndddddddd\r\neeeeeeee\r\n");
        assert_snapshot(&mut source, 8, 3, true);
    }

    #[test]
    fn tiny_viewports_never_scroll_away_last_row() {
        for rows in [1, 2] {
            let mut source = parser(8, rows);
            feed(&mut source, b"abcdefghABCDEFGHijklmnopIJKLMNOP");
            assert_snapshot(&mut source, 8, rows, true);
            source.grid_mut()[Line(rows as i32 - 1)][Column(7)].flags.insert(Flags::WRAPLINE);
            assert_snapshot(&mut source, 8, rows, false);
        }
    }

    #[test]
    fn alternate_screen_wraps_do_not_populate_primary_history() {
        let mut source = parser(8, 4);
        feed(&mut source, b"old primary\x1b[?1049habcdefghi\x1b[?25l\x1b[2;2H");
        assert_snapshot(&mut source, 8, 4, true);
    }

    #[test]
    fn capped_history_roundtrip_has_no_extra_blank_line() {
        let mut source = parser(8, 4);
        for n in 0..1010 { feed(&mut source, format!("H{n:04}\r\n").as_bytes()); }
        assert_snapshot(&mut source, 8, 4, true);
    }
}

#[cfg(test)]
mod snapshot_fidelity_tests {
    use super::*;

    /// 픽스처를 뜬 pane 의 실제 크기. 폭이 어긋나면 재생 자체가 무의미해진다
    /// (넓은 화면에서 뜬 녹음을 좁은 화면에 틀면 divider 가 줄바꿈돼 겹친다).
    fn fixture_size() -> (u16, u16) {
        let g = |k: &str, d: u16| {
            std::env::var(k).ok().and_then(|v| v.parse().ok()).unwrap_or(d)
        };
        (g("KASATERM_SNAPSHOT_COLS", 363), g("KASATERM_SNAPSHOT_ROWS", 39))
    }

    /// 우리 `snapshot()` 이 alacritty 그리드를 그대로 옮기는가.
    ///
    /// agy(antigravity CLI) 화면이 kasaterm 에서만 뒤엉키는 걸 쫓다 여기까지 왔다
    /// (2026-08-11). 같은 바이트를 alacritty Term 에 먹이면 그리드는 **정확한데**
    /// pane 에 보이는 건 두 줄이 한 줄로 뭉쳐 있었다 — 파서가 아니라 그 위 변환층
    /// 이 범인이라는 뜻이다. 이 테스트가 그 경계를 못박는다.
    ///
    /// 입력은 `KASATERM_SNAPSHOT_FIXTURE` 로 준 실제 녹음 파일. 없으면 건너뛴다
    /// (녹음은 크고 사람 대화가 들어 있어 레포에 넣지 않는다).
    #[test]
    fn snapshot_rows_match_the_alacritty_grid() {
        let Some(path) = std::env::var_os("KASATERM_SNAPSHOT_FIXTURE") else { return };
        let raw = std::fs::read(&path).expect("fixture");
        let (cols, rows) = fixture_size();

        let listener = PtyEventForwarder {
            respond: true,
            writer: Arc::new(Mutex::new(Box::new(std::io::sink()))),
            size: Arc::new(Mutex::new((cols, rows))),
            last_title: Arc::new(Mutex::new(None)),
        };
        let mut term = make_term(cols, rows, listener);
        let mut proc: Processor<StdSyncHandler> = Processor::new();
        proc.advance(&mut term, &raw);

        // 기준 = alacritty 그리드에서 직접 읽은 줄.
        let truth: Vec<String> = {
            let g = term.grid();
            (0..rows as usize)
                .map(|r| {
                    let line = alacritty_terminal::index::Line(r as i32);
                    (0..cols as usize)
                        .map(|c| g[line][alacritty_terminal::index::Column(c)].c)
                        .collect::<String>()
                        .trim_end()
                        .to_string()
                })
                .collect()
        };

        let upd = snapshot(&mut term, cols, rows, "%t", &Arc::new(Mutex::new(None)), true);
        let mut got = vec![String::new(); rows as usize];
        for (r, row) in &upd.dirty {
            got[*r as usize] =
                row.iter().map(|c| c.ch).collect::<String>().trim_end().to_string();
        }

        check(&truth, &got, "한 번에 먹였을 때");
    }

    /// 같은 바이트를 **live 처럼 잘게 나눠** 먹이고 damage 기반 스냅샷을 누적한다.
    ///
    /// 실제 reader 가 하는 그대로다 — 읽을 때마다 `advance` → `scroll_display(Bottom)`
    /// → `snapshot(force_full=false)` → 돌아온 dirty 행만 화면에 반영. 한 번에 먹이면
    /// 멀쩡한데 이렇게 하면 어긋난다면, 범인은 파서도 스냅샷도 아니고 **부분갱신 누적**이다.
    #[test]
    fn chunked_damage_snapshots_still_match_the_grid() {
        let Some(path) = std::env::var_os("KASATERM_SNAPSHOT_FIXTURE") else { return };
        let raw = std::fs::read(&path).expect("fixture");
        // ★높이를 좁혀 **스크롤이 일어나게** 한다. live pane 은 앞선 셸 출력 때문에
        // agy 화면이 늘 스크롤 상태고, damage 는 뷰포트 기준이라 스크롤이 섞이면
        // "안 damaged 된 행"이 실은 다른 내용을 가리키게 된다.
        let (cols, base_rows) = fixture_size();
        for rows in [base_rows, base_rows / 2, 12] {
        for chunk in [65536usize, 4096, 1024, 512, 128] {
            let listener = PtyEventForwarder {
                respond: true,
                writer: Arc::new(Mutex::new(Box::new(std::io::sink()))),
                size: Arc::new(Mutex::new((cols, rows))),
                last_title: Arc::new(Mutex::new(None)),
            };
            let mut term = make_term(cols, rows, listener);
            let mut proc: Processor<StdSyncHandler> = Processor::new();
            let title = Arc::new(Mutex::new(None));
            let mut mirror = vec![String::new(); rows as usize];
            // ★reader 와 **똑같이** NFC 정규화를 거쳐 넘긴다. 이걸 빼면 실제 경로가
            // 아니다 — 비-ASCII 청크만 정규화되므로 한글이 든 스트림에서만 갈린다.
            let mut u8buf = Utf8Buffer::new();
            for part in raw.chunks(chunk) {
                use unicode_normalization::UnicodeNormalization;
                let batch = u8buf.process(part);
                let nfc_holder: Option<String> = if batch.is_ascii() {
                    None
                } else {
                    std::str::from_utf8(&batch).ok().map(|s| s.nfc().collect())
                };
                let fed: &[u8] =
                    nfc_holder.as_deref().map(str::as_bytes).unwrap_or(batch.as_slice());
                proc.advance(&mut term, fed);
                if proc.sync_bytes_count() > 0 {
                    continue; // reader 와 같은 규칙
                }
                term.scroll_display(alacritty_terminal::grid::Scroll::Bottom);
                let upd = snapshot(&mut term, cols, rows, "%t", &title, false);
                for (r, row) in &upd.dirty {
                    mirror[*r as usize] =
                        row.iter().map(|c| c.ch).collect::<String>().trim_end().to_string();
                }
            }
            let truth = grid_rows(&term, cols, rows);
            check(&truth, &mirror, &format!("rows={rows} chunk={chunk}"));
        }
        }
    }

    /// 위로 올린 화면에 **히스토리가 실제로 보이는가**.
    ///
    /// 기준값을 그리드에서 뽑으면 동어반복이 되므로 내용으로 못박는다: 40줄을 흘린
    /// 10행 화면은 L31..L40 을 보여주고, 5줄 올리면 L26..L35 여야 한다. 예전 코드는
    /// 히스토리를 음수 `Line` 으로 읽지 못하게 막아 윗 5줄이 빈칸이 됐다.
    #[test]
    fn scrolling_up_actually_shows_history() {
        let (cols, rows) = (40u16, 10u16);
        let listener = PtyEventForwarder {
            respond: true,
            writer: Arc::new(Mutex::new(Box::new(std::io::sink()))),
            size: Arc::new(Mutex::new((cols, rows))),
            last_title: Arc::new(Mutex::new(None)),
        };
        let mut term = make_term(cols, rows, listener);
        let mut proc: Processor<StdSyncHandler> = Processor::new();
        let mut feed = String::new();
        for i in 1..=40 {
            feed.push_str(&format!("L{i}\r\n"));
        }
        proc.advance(&mut term, feed.as_bytes());
        let title = Arc::new(Mutex::new(None));

        let view = |t: &mut Term<PtyEventForwarder>| -> Vec<String> {
            let upd = snapshot(t, cols, rows, "%t", &title, true);
            let mut got = vec![String::new(); rows as usize];
            for (r, row) in &upd.dirty {
                got[*r as usize] =
                    row.iter().map(|c| c.ch).collect::<String>().trim_end().to_string();
            }
            got
        };

        let live = view(&mut term);
        assert_eq!(&live[..9], &["L32", "L33", "L34", "L35", "L36", "L37", "L38", "L39", "L40"]);

        term.scroll_display(alacritty_terminal::grid::Scroll::Delta(5));
        assert_eq!(term.grid().display_offset(), 5, "스크롤이 안 걸렸다");
        let scrolled = view(&mut term);
        assert_eq!(
            &scrolled[..],
            &["L27", "L28", "L29", "L30", "L31", "L32", "L33", "L34", "L35", "L36"],
            "위로 올린 화면에 히스토리가 안 보인다"
        );
    }

    #[test]
    fn new_output_does_not_pull_a_scrolled_view_to_the_bottom() {
        let (cols, rows) = (40u16, 10u16);
        let listener = PtyEventForwarder {
            respond: true,
            writer: Arc::new(Mutex::new(Box::new(std::io::sink()))),
            size: Arc::new(Mutex::new((cols, rows))),
            last_title: Arc::new(Mutex::new(None)),
        };
        let mut term = make_term(cols, rows, listener);
        let mut proc: Processor<StdSyncHandler> = Processor::new();
        let mut initial = String::new();
        for i in 1..=40 {
            initial.push_str(&format!("L{i}\r\n"));
        }
        proc.advance(&mut term, initial.as_bytes());
        term.scroll_display(alacritty_terminal::grid::Scroll::Delta(5));

        let title = Arc::new(Mutex::new(None));
        let visible = |t: &mut Term<PtyEventForwarder>| -> Vec<String> {
            let update = snapshot(t, cols, rows, "%t", &title, true);
            update
                .dirty
                .into_iter()
                .map(|(_, row)| row.iter().map(|cell| cell.ch).collect::<String>())
                .collect()
        };
        let before_offset = term.grid().display_offset();
        let before_view = visible(&mut term);
        let follow_live_tail = before_offset == 0;
        proc.advance(&mut term, b"L41\r\n");
        if follow_live_tail {
            term.scroll_display(alacritty_terminal::grid::Scroll::Bottom);
        }
        let after_view = visible(&mut term);

        assert!(before_offset > 0);
        assert!(
            term.grid().display_offset() > 0,
            "새 출력이 위로 올린 화면을 맨 아래로 끌어내렸다"
        );
        assert_eq!(
            after_view, before_view,
            "새 출력이 읽고 있던 줄을 움직였다"
        );
    }

    /// 뷰포트 위 행 읽기 — 가까운 순이고, 스크롤을 따라가며, 히스토리 바닥에서
    /// 캡된다. 렌더러의 팀메시지 이어칠하기(헤더가 화면 위로 나간 본문)가 이
    /// 순서를 전제로 위로 걷는다.
    #[test]
    fn rows_above_walks_history_nearest_first() {
        let (cols, rows) = (40u16, 10u16);
        let listener = PtyEventForwarder {
            respond: true,
            writer: Arc::new(Mutex::new(Box::new(std::io::sink()))),
            size: Arc::new(Mutex::new((cols, rows))),
            last_title: Arc::new(Mutex::new(None)),
        };
        let mut term = make_term(cols, rows, listener);
        let mut proc: Processor<StdSyncHandler> = Processor::new();
        let mut feed = String::new();
        for i in 1..=40 {
            feed.push_str(&format!("L{i}\r\n"));
        }
        proc.advance(&mut term, feed.as_bytes());
        let texts = |rows: &[Row]| -> Vec<String> {
            rows.iter()
                .map(|r| r.iter().map(|c| c.ch).collect::<String>().trim_end().to_string())
                .collect()
        };
        // 라이브 바닥: 화면 첫 줄이 L32 이므로 그 위는 L31 부터 가까운 순.
        assert_eq!(texts(&read_rows_above(&term, 3)), ["L31", "L30", "L29"]);
        term.scroll_display(alacritty_terminal::grid::Scroll::Delta(5));
        assert_eq!(texts(&read_rows_above(&term, 3)), ["L26", "L25", "L24"]);
        // 히스토리 바닥 캡 — 남은 것보다 많이 달라면 있는 만큼만
        // (히스토리 L1~L31 = 31행, 스크롤 5 를 빼면 26행).
        assert_eq!(read_rows_above(&term, 1000).len(), 26);
    }

    fn grid_rows(term: &Term<PtyEventForwarder>, cols: u16, rows: u16) -> Vec<String> {
        let g = term.grid();
        (0..rows as usize)
            .map(|r| {
                let line = alacritty_terminal::index::Line(r as i32);
                (0..cols as usize)
                    .map(|c| g[line][alacritty_terminal::index::Column(c)].c)
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect()
    }

    fn check(truth: &[String], got: &[String], what: &str) {
        let rows = truth.len();
        let mut bad = Vec::new();
        for r in 0..rows {
            // 넓은 글자 뒷칸을 어느 쪽이 어떻게 채우든 **글자 자체**는 같아야 한다.
            let norm = |s: &str| s.replace('\0', "").replace(' ', "");
            if norm(&truth[r]) != norm(&got[r]) {
                bad.push(format!("  행 {r}\n    그리드: {:?}\n    스냅샷: {:?}", truth[r], got[r]));
            }
        }
        assert!(bad.is_empty(), "[{what}] 그리드와 스냅샷이 어긋난다:\n{}", bad.join("\n"));
    }
}

#[cfg(test)]
mod prompt_anchor_tests {
    use super::*;

    /// 주어진 줄들을 그대로 그리드에 찍은 Term. 실제 PTY 없이 스캔만 검증한다.
    fn term_with(lines: &[&str]) -> Term<PtyEventForwarder> {
        let (cols, rows) = (60u16, 10u16);
        let listener = PtyEventForwarder {
            respond: true,
            writer: Arc::new(Mutex::new(Box::new(std::io::sink()))),
            size: Arc::new(Mutex::new((cols, rows))),
            last_title: Arc::new(Mutex::new(None)),
        };
        let mut term = make_term(cols, rows, listener);
        let mut proc: Processor<StdSyncHandler> = Processor::new();
        proc.advance(&mut term, lines.join("\r\n").as_bytes());
        term
    }

    /// 확정된 프롬프트와 **입력 중인 줄**을 가르는 것은 마커 뒤 한 글자뿐이다 —
    /// claude 는 확정된 것에 일반 공백(U+0020), 화면 하단 입력창에 NBSP(U+00A0)를
    /// 쓴다(2026-08-15 살아 있는 pane 9개 대조로 확정).
    ///
    /// 이 시험이 지키는 것: claude 가 렌더를 바꿔 그 규칙이 깨지면 **여기서** 터진다.
    /// 안 그러면 「지금 치고 있는 줄」이 지나간 질문 목록 끝에 조용히 끼어드는
    /// 형태로만 드러나고, 그건 화면을 한참 보고서야 알아챈다.
    #[test]
    fn input_box_line_is_not_a_past_prompt() {
        let t = term_with(&[
            "\u{276f} 확정된 질문",
            "  답변 줄",
            "\u{276f}\u{a0}지금 치고 있는 줄",
        ]);
        let got: Vec<String> = scan_prompt_anchors(&t).into_iter().map(|a| a.text).collect();
        assert_eq!(got, vec!["확정된 질문".to_string()]);
    }

    /// 위 시험의 **대조군** — 같은 문장이 NBSP 대신 일반 공백이면 잡혀야 한다.
    /// 둘을 짝으로 둬야 「NBSP 한 글자가 유일한 차이」가 증명된다. 짝이 없으면
    /// 판정이 엉뚱한 이유(길이·위치)로 걸러도 앞 시험만 보고 통과로 읽는다.
    #[test]
    fn the_same_line_with_a_plain_space_is_a_prompt() {
        let t = term_with(&["\u{276f} 지금 치고 있는 줄"]);
        let got: Vec<String> = scan_prompt_anchors(&t).into_iter().map(|a| a.text).collect();
        assert_eq!(got, vec!["지금 치고 있는 줄".to_string()]);
    }

    /// wide 글리프(한글)의 뒤칸은 `\0` 이 아니라 진짜 공백이고 구분은 플래그에만
    /// 있다. 문자만 보고 거르면 「질 문  1」처럼 글자마다 벌어진다(실측으로 겪음).
    #[test]
    fn wide_glyph_spacers_do_not_leak_into_the_text() {
        let t = term_with(&["\u{276f} 질문 1 한글이 섞인 프롬프트"]);
        let got = scan_prompt_anchors(&t);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].text, "질문 1 한글이 섞인 프롬프트");
    }

    /// ASCII `>` 는 마커가 아니다 — diff·인용·다른 TUI 가 행 머리에 흔히 써서,
    /// 그것까지 세면 대화와 무관한 줄이 턴 목록에 들어찬다.
    #[test]
    fn ascii_angle_bracket_is_not_a_marker() {
        let t = term_with(&["> 인용문이거나 diff 한 줄", "\u{276f} 진짜 질문"]);
        let got: Vec<String> = scan_prompt_anchors(&t).into_iter().map(|a| a.text).collect();
        assert_eq!(got, vec!["진짜 질문".to_string()]);
    }

    /// 마커만 있고 본문이 빈 줄(비어 있는 입력창)은 갈 곳이 못 된다.
    #[test]
    fn empty_prompt_line_is_skipped() {
        let t = term_with(&["\u{276f}", "\u{276f} 내용 있는 질문"]);
        assert_eq!(scan_prompt_anchors(&t).len(), 1);
    }

    /// 절대 줄 번호는 화면 첫 줄이 아니라 **세션 시작**부터 센다 — 스크롤 위치를
    /// 그 번호로 되돌리는 계산(`hist - abs`)이 그 전제 위에 있다.
    #[test]
    fn anchor_line_numbers_count_from_the_session_start() {
        let t = term_with(&["첫 줄", "\u{276f} 둘째 줄의 질문"]);
        let got = scan_prompt_anchors(&t);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].abs_line, 1);
    }

    /// codex 는 같은 자리에 U+203A 를 쓴다(2026-09-03 실측 화면: `› 1+1은?`).
    /// 대체화면을 안 써서 대화가 이 터미널의 스크롤백에 그대로 쌓이므로, 마커만
    /// 인정하면 claude 가 못 쓰는 **정확한 절대 줄 점프**가 codex 에 그대로 붙는다.
    #[test]
    fn codex_marker_is_a_prompt_too() {
        let t = term_with(&["\u{203a} 코덱스에 던진 질문", "  2예요, 선생님.", "다른 줄"]);
        let got: Vec<String> = scan_prompt_anchors(&t).into_iter().map(|a| a.text).collect();
        assert_eq!(got, vec!["코덱스에 던진 질문".to_string()]);
    }

    /// codex 의 입력줄은 확정된 질문과 **글자로는 구별이 안 된다** — claude 와 달리
    /// NBSP 를 안 써서 `› Ask Codex to do anything` 이 그냥 질문처럼 보인다. 커서가
    /// 그 줄에 앉아 있다는 것만이 단서고, 그것을 안 보면 자리표시자가 「지나간 질문」
    /// 으로 목록 끝에 끼어 ↓ 를 누를 때마다 거기로 끌려간다.
    #[test]
    fn codex_input_line_under_the_cursor_is_not_a_past_prompt() {
        let t = term_with(&["\u{203a} 확정된 질문", "  답변 줄", "\u{203a} Ask Codex to do anything"]);
        let got: Vec<String> = scan_prompt_anchors(&t).into_iter().map(|a| a.text).collect();
        assert_eq!(got, vec!["확정된 질문".to_string()]);
    }

    /// 위 시험의 **대조군** — 커서가 딴 줄로 옮겨가면 같은 문장이 잡혀야 한다.
    /// 짝이 없으면 판정이 엉뚱한 이유(맨 아랫줄이라서 등)로 걸러도 통과로 읽힌다.
    #[test]
    fn the_same_codex_line_away_from_the_cursor_is_a_prompt() {
        let t = term_with(&["\u{203a} 질문 하나", "\u{203a} 질문 둘", "  커서는 여기"]);
        let got: Vec<String> = scan_prompt_anchors(&t).into_iter().map(|a| a.text).collect();
        assert_eq!(got, vec!["질문 하나".to_string(), "질문 둘".to_string()]);
    }
}
