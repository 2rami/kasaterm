//! URL·파일 경로의 실제 화면 셀을 함께 보존해야 줄바꿈·한글 뒤에서도 같은 대상을 연다.
use super::*;
mod probe;
mod scan;

/// 렌더 밑줄용 URL 범위. 실제 여는 주소(url)는 hover/click 이 그때그때 행을 다시
/// 스캔해 얻으므로 여기엔 담지 않는다.
pub(crate) use kasa_gridview::CellSpan as LinkSpan;

/// 한 행을 스캔해 (col_start, col_end, url) 목록을 돌려준다. 줄이 안 꺾인 짧은
/// 주소용 — hover/click 은 `detect_links_rows` 로 이웃 행까지 잇는다.
pub(crate) fn detect_links_row(row: &[GridCell]) -> Vec<(u16, u16, String)> {
    detect_links_rows(&[row])
        .into_iter()
        .filter_map(|(segs, url)| segs.first().map(|(_, s, e)| (*s, *e, url)))
        .collect()
}

fn joins(prev: &[GridCell], next: &[GridCell]) -> bool {
    scan::row_join(prev, next).or_else(|| scan::web_row_join(prev, next)).is_some()
}

type CellPosition = (usize, u16, u16);

fn indexed_rows(rows: &[&[GridCell]]) -> (Vec<char>, Vec<Option<CellPosition>>) {
    let mut chars = Vec::new();
    let mut positions = Vec::new();
    let mut start = 0;
    for (r, row) in rows.iter().enumerate() {
        let next = rows.get(r + 1);
        let trusted = next.and_then(|next| scan::row_join(row, next));
        let candidate = trusted.or_else(|| next.and_then(|next| scan::web_row_join(row, next)));
        let end = candidate.map_or(row.len(), |(e, _)| e);
        for c in start..end {
            let cell = &row[c];
            if cell.leading_wide_spacer {
                continue;
            }
            if matches!(cell.ch, ' ' | '\0')
                && c > 0
                && unicode_width::UnicodeWidthChar::width(row[c - 1].ch) == Some(2)
            {
                continue;
            }
            chars.push(if cell.hidden || cell.ch == '\0' {
                ' '
            } else {
                cell.ch
            });
            let width = if unicode_width::UnicodeWidthChar::width(cell.ch) == Some(2)
                && row
                    .get(c + 1)
                    .is_some_and(|cell| matches!(cell.ch, ' ' | '\0'))
            {
                2
            } else {
                1
            };
            positions.push(Some((r, c as u16, (c + width) as u16)));
        }
        // TUI가 색과 밑줄을 지운 행은 앞에서 시작한 HTTP 주소가 있을 때만 잇는다.
        // 경로 조각끼리 모양이 같다는 이유로 서로 다른 파일·문장을 합치면 안 된다.
        let join = trusted.or_else(|| candidate.filter(|_| scan::ends_in_web_link(&chars)));
        start = join.map_or(0, |(_, start)| start);
        if join.is_none() {
            chars.push(' ');
            positions.push(None);
        }
    }
    (chars, positions)
}

pub(crate) fn detect_links_rows(rows: &[&[GridCell]]) -> Vec<(Vec<(usize, u16, u16)>, String)> {
    let (chars, positions) = indexed_rows(rows);
    scan::scan(&chars)
        .into_iter()
        .map(|link| {
            let mut spans: Vec<(usize, u16, u16)> = Vec::new();
            for &(row, start, end) in positions[link.start..link.end].iter().flatten() {
                match spans.last_mut() {
                    Some((r, _, e)) if *r == row && *e == start => *e = end,
                    _ => spans.push((row, start, end)),
                }
            }
            (spans, link.target)
        })
        .collect()
}

pub(crate) fn restyle_web_suffixes(rows: &mut [Vec<GridCell>]) {
    if !rows.iter().flatten().any(|c| c.underline && ('\u{ac00}'..='\u{d7a3}').contains(&c.ch)) {
        return;
    }
    let (chars, positions) = indexed_rows(&rows.iter().map(Vec::as_slice).collect::<Vec<_>>());
    for link in scan::scan(&chars).into_iter().filter(|l| l.suffix_end > l.end) {
        let Some((r, _, end)) = positions[link.end - 1] else { continue };
        let style = rows[r][end as usize - 1].clone();
        if !style.underline || style.inverse || style.hidden {
            continue;
        }
        let text_fg = positions.get(link.suffix_end).copied().flatten()
            .map(|(r, c, _)| &rows[r][c as usize])
            .filter(|c| !c.underline && !c.inverse && !c.hidden && c.bg == style.bg)
            .map_or(GridCell::blank().fg, |c| c.fg.clone());
        // 하네스가 조사까지 링크색을 입혔어도 문자·배경·선택 좌표는 그대로 둔다.
        for &(r, start, end) in positions[link.end..link.suffix_end].iter().flatten() {
            for cell in &mut rows[r][start as usize..end as usize] {
                if cell.underline && !cell.inverse && !cell.hidden && cell.fg == style.fg && cell.bg == style.bg {
                    cell.underline = false;
                    cell.fg = text_fg.clone();
                }
            }
        }
    }
}

pub(crate) fn click_modifier(modifiers: winit::keyboard::ModifiersState) -> bool {
    if cfg!(target_os = "macos") {
        modifiers.super_key()
    } else {
        modifiers.control_key()
    }
}

fn resolve_path(
    raw: &str,
    cwd: Option<&std::path::Path>,
    home: Option<&std::path::Path>,
) -> Option<std::path::PathBuf> {
    let raw = scan::strip_location(raw);
    if raw.to_ascii_lowercase().starts_with("file://") {
        return url::Url::parse(raw).ok()?.to_file_path().ok();
    }
    if let Some(tail) = raw.strip_prefix("~/") {
        return Some(home?.join(tail));
    }
    let path = std::path::PathBuf::from(raw);
    if path.is_absolute()
        || (raw.as_bytes().get(1) == Some(&b':')
            && matches!(raw.as_bytes().get(2), Some(b'/' | b'\\')))
    {
        Some(path)
    } else {
        Some(cwd?.join(path))
    }
}

fn download_link_file(base: &str, path: &std::path::Path) -> anyhow::Result<std::path::PathBuf> {
    let query = format!(
        "/term/file?path={}",
        kasa_mcp::remote::urlencode(&path.to_string_lossy())
    );
    let bytes = kasa_mcp::remote::remote_get_bytes(base, &query)?;
    let name = path
        .to_string_lossy()
        .rsplit(['/', '\\'])
        .next()
        .filter(|name| !name.is_empty() && *name != "." && *name != "..")
        .unwrap_or("download")
        .to_owned();
    // 원격 경로의 ..·드라이브·기기 이름을 로컬 저장 경로에 합치지 않는다.
    let root = std::env::temp_dir().join(format!("kasaterm-link-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root)?;
    let local = root.join(name);
    std::fs::write(&local, bytes)?;
    Ok(local)
}

impl App {
    pub(crate) fn open_terminal_link(&mut self, pane: &str, target: &str) {
        if self.record_terminal_link_probe(pane, target) {
            return;
        }
        if let Ok(url) = url::Url::parse(target) {
            if matches!(url.scheme(), "http" | "https") {
                self.open_url_for_pane(url.as_str(), Some(pane));
                return;
            }
            if url.scheme() == "mailto" {
                crate::chrome::open_url_in_browser(url.as_str());
                return;
            }
        }
        let remote = kasa_mcp::remote::remote_info(pane).filter(|info| info.view);
        let cwd = remote
            .as_ref()
            .and_then(|r| r.remote_cwd.as_deref())
            .map(std::path::PathBuf::from)
            .or_else(|| self.pane_view_cwd.get(pane).cloned())
            .or_else(|| self.pane_current_cwd(pane));
        let home = if remote.is_none() {
            kasa_socket::home_dir()
        } else {
            None
        };
        let Some(path) = resolve_path(target, cwd.as_deref(), home.as_deref()) else {
            self.set_toast("파일 경로를 이 작업의 폴더에서 찾을 수 없어요".to_string());
            return;
        };
        if let Some(remote) = remote {
            let proxy = self.proxy.clone();
            std::thread::spawn(move || match download_link_file(&remote.base, &path) {
                Ok(local) => crate::proc::open_path_default(&local),
                Err(error) => {
                    let _ = proxy.send_event(UserEvent::SocketToast(format!(
                        "파일을 열지 못했어요: {error}"
                    )));
                }
            });
        } else if path.exists() {
            crate::proc::open_path_default(&path);
        } else {
            self.set_toast(format!("파일을 찾을 수 없어요: {}", path.display()));
        }
    }

    /// 픽셀 좌표 아래에 클릭 가능한 URL 이 있으면 그 pane·밑줄 셀범위들·주소를
    /// 돌려준다. hover 밑줄/커서·클릭 모두 이걸 쓴다. 커서가 올라간 행에서 위아래로
    /// **이어진 행**(줄이 꺾인 주소)만 더 보므로 CursorMoved 마다 불러도 가볍다.
    pub(crate) fn link_hit(&self, px: f32, py: f32) -> Option<(String, Vec<LinkSpan>, String)> {
        // 대화로 보는 칸(거울의 기본 보기)은 화면이 대화 보기가 지은 격자다. 원본 글자판을 보면 밑줄과
        // 손가락 커서가 엉뚱한 자리에 서고 다른 주소가 열렸다(2026-10-15).
        if let Some((pid, col, row, rows)) = self.chat_view_cell(px, py) {
            return link_in(&pid, |r| rows.get(r), col, row);
        }
        let (pid, col, row) = self.px_to_pane_cell(px, py)?;
        let ws = self.ws.lock().unwrap();
        let term = ws.panes.get(&pid)?.term()?;
        // 화면은 렌더가 옮겨 그린 것이라 원본 글자판과 행이 어긋난다. 클릭 좌표는
        // 화면 기준이므로 같은 옮김을 되짚어야 눈에 보이는 그 링크가 열린다 —
        // 원본을 그대로 보면 당긴 줄 수만큼 아래 줄에서 주소를 찾는다(복사와 같은
        // 뿌리, 2026-09-05).
        let shift = self.pane_view_shift.get(&pid);
        link_in(&pid, |r| match shift {
            Some(shift) => shift.row(r, &term.cells),
            None => term.cells.get(r),
        }, col, row)
    }
}

/// 화면 행 `view` 에서 (`col`, `row`) 칸에 걸린 주소. 이어진 행(꺾인 주소)을 위아래로 모아 본다 — 상한은
/// 화면 폭이 아주 좁아도 긴 주소가 드는 만큼.
fn link_in<'a>(pid: &str, view: impl Fn(usize) -> Option<&'a Vec<GridCell>>, col: u16, row: u16) -> Option<(String, Vec<LinkSpan>, String)> {
    let row = row as usize;
    let mut start = row;
    while start > 0 && row - start < 16 {
        match (view(start - 1), view(start)) {
            (Some(p), Some(c)) if joins(p, c) => start -= 1,
            _ => break,
        }
    }
    let mut end = row;
    while end - start < 32 {
        match (view(end), view(end + 1)) {
            (Some(c), Some(n)) if joins(c, n) => end += 1,
            _ => break,
        }
    }
    let rows: Vec<&[GridCell]> = (start..=end).filter_map(|r| view(r).map(Vec::as_slice)).collect();
    let rel = row - start;
    detect_links_rows(&rows)
        .into_iter()
        .find(|(segs, _)| segs.iter().any(|(r, s, e)| *r == rel && col >= *s && col < *e))
        .map(|(segs, url)| {
            let spans = segs
                .into_iter()
                .map(|(r, s, e)| LinkSpan { row: (start + r) as u16, col_start: s, col_end: e })
                .collect();
            (pid.to_string(), spans, url)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(s: &str) -> Vec<GridCell> {
        let mut cells = Vec::new();
        for ch in s.chars() {
            cells.push(GridCell {
                ch,
                ..GridCell::blank()
            });
            if unicode_width::UnicodeWidthChar::width(ch) == Some(2) {
                cells.push(GridCell::blank());
            }
        }
        cells
    }

    /// 대화 보기 격자처럼 원본과 다른 화면 행에서도 누른 그 칸의 주소를 그 행 자리로 돌려준다.
    #[test]
    fn a_link_is_found_in_whatever_rows_the_screen_shows() {
        let rows = vec![row("⏺ 답"), row("  see https://a.b/c tail")];
        let (pid, spans, url) = link_in("%8", |r| rows.get(r), 10, 1).expect("link");
        assert_eq!((pid.as_str(), url.as_str()), ("%8", "https://a.b/c"));
        assert_eq!((spans[0].row, spans[0].col_start, spans[0].col_end), (1, 6, 19));
        assert!(link_in("%8", |r| rows.get(r), 1, 0).is_none());
    }

    #[test]
    fn joins_a_url_split_by_the_wrap_flag() {
        let mut a = row("see https://a.b/c");
        a.last_mut().unwrap().wrapped = true;
        let b = row("d/e?x=1 tail");
        let links = detect_links_rows(&[&a, &b]);
        assert_eq!(links.len(), 1);
        let (segs, url) = &links[0];
        assert_eq!(url, "https://a.b/cd/e?x=1");
        assert_eq!(segs, &vec![(0, 4, 17), (1, 0, 7)]);
    }

    #[test]
    fn joins_by_fill_when_the_flag_is_missing() {
        // 옛 빌드의 거울·레거시 브리지 — 앞 행이 끝 칸까지 차 있고 다음 행이 곧장 이어진다.
        let a = row("https://a.b/c");
        let b = row("d/e");
        let links = detect_links_rows(&[&a, &b]);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].1, "https://a.b/cd/e");
    }

    #[test]
    fn keeps_rows_apart_when_the_first_ends_in_a_blank() {
        let a = row("https://a.b/c ");
        let b = row("d/e");
        let links = detect_links_rows(&[&a, &b]);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].1, "https://a.b/c");
        assert_eq!(links[0].0, vec![(0, 0, 13)]);
    }

    #[test]
    fn single_row_scan_still_trims_trailing_punctuation() {
        let links = detect_links_row(&row("go https://x.y/z."));
        assert_eq!(links, vec![(3, 16, "https://x.y/z".to_string())]);
    }
    #[test]
    fn screenshot_url_stops_before_closing_parenthesis_and_korean_suffix() {
        let a = row("Finder의 스마트 폴더 (https://support.apple.com/guide/");
        let b = row("mac-help/organize-your-files-in-the-finder-mchle9f0a1b2/mac)처럼 저장 위치와");
        let links = detect_links_rows(&[&a, &b]);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].1, "https://support.apple.com/guide/mac-help/organize-your-files-in-the-finder-mchle9f0a1b2/mac");
        assert_eq!(
            links[0].0[1].2 as usize,
            b.iter().position(|c| c.ch == ')').unwrap()
        );
    }

    #[test]
    fn styled_tui_margin_is_skipped_but_plain_paragraphs_stay_separate() {
        let mut a = row("  (https://support.apple.com/guide/   ");
        let mut b = row("  mac-help/example)처럼 다음 설명");
        for cell in a.iter_mut().chain(b.iter_mut()) {
            cell.underline = cell.ch != ' ';
        }
        let links = detect_links_rows(&[&a, &b]);
        assert_eq!(
            links[0].1,
            "https://support.apple.com/guide/mac-help/example"
        );
        assert_eq!(links[0].0[1].1, 2);
        for cell in a.iter_mut().chain(b.iter_mut()) {
            cell.underline = false;
        }
        assert_eq!(
            detect_links_rows(&[&a, &b])[0].1,
            "https://support.apple.com/guide/"
        );
    }

    #[test]
    fn web_delimiters_and_common_address_formats() {
        for (text, expected) in [
            (
                "(https://en.wikipedia.org/wiki/Foo_(bar))처럼",
                "https://en.wikipedia.org/wiki/Foo_(bar)",
            ),
            (
                "[https://example.com/a?x=1&y=2#part]입니다",
                "https://example.com/a?x=1&y=2#part",
            ),
            ("URL:HTTPS://EXAMPLE.COM/a", "HTTPS://EXAMPLE.COM/a"),
            ("www.example.com/a.", "https://www.example.com/a"),
            (
                "github.com/2rami/kasaterm",
                "https://github.com/2rami/kasaterm",
            ),
            ("localhost:3000/a?x=1", "http://localhost:3000/a?x=1"),
            ("http://[::1]:3000/a", "http://[::1]:3000/a"),
            ("mailto:hello@example.com", "mailto:hello@example.com"),
        ] {
            assert_eq!(
                detect_links_row(&row(text))
                    .iter()
                    .map(|l| l.2.as_str())
                    .collect::<Vec<_>>(),
                vec![expected],
                "{text}"
            );
        }
    }

    #[test]
    fn web_authority_excludes_korean_sentence_suffix() {
        for (text, expected) in [
            ("http://localhost:8795를", "http://localhost:8795"),
            ("http://localhost:8795입니다.", "http://localhost:8795"),
            ("localhost:8795에서", "http://localhost:8795"),
            ("127.0.0.1:8795로", "http://127.0.0.1:8795"),
            ("http://192.168.0.2:8795는", "http://192.168.0.2:8795"),
            ("http://[::1]:8795를", "http://[::1]:8795"),
            ("https://example.com:443에서", "https://example.com:443"),
            ("https://example.com을", "https://example.com"),
            ("www.example.com에서", "https://www.example.com"),
            ("www.example.com:8795를", "https://www.example.com:8795"),
            ("example.com:8795에서", "https://example.com:8795"),
            ("github.com을", "https://github.com"),
        ] {
            let cells = row(&format!("열기 {text} 다음"));
            let links = detect_links_row(&cells);
            assert_eq!(links.len(), 1, "{text}: {links:?}");
            assert_eq!(links[0].2, expected, "{text}");
            let rows = [cells];
            assert!(link_in("%test", |r| rows.get(r), links[0].1 - 1, 0).is_some());
            assert!(link_in("%test", |r| rows.get(r), links[0].1, 0).is_none());
            assert!(link_in("%test", |r| rows.get(r), links[0].1 + 1, 0).is_none());
        }
    }

    #[test]
    fn web_suffix_preserves_explicit_and_unicode_addresses() {
        for target in [
            "http://localhost:8795/를", "https://example.com/결과.html이야",
            "https://example.com/?q=를", "https://example.com/#를",
            "https://예제.한국", "https://example.com한글", "https://example.com%EB%A5%BC",
        ] {
            let found = detect_links_row(&row(target));
            assert_eq!(found.iter().map(|l| l.2.as_str()).collect::<Vec<_>>(), vec![target]);
        }
        for text in ["`https://example.com를`", "<https://example.com를>"] {
            assert_eq!(detect_links_row(&row(text))[0].2, "https://example.com를");
        }
        for text in ["http://localhost:8795abc", "http://localhost:99999를", "`http://localhost:8795를`"] {
            assert!(detect_links_row(&row(text)).is_empty(), "{text}");
        }
    }

    #[test]
    fn wrapped_web_suffix_keeps_hit_and_style_boundaries() {
        use kasa_bridge::screen::Color;
        let mut first = row("http://localhost:8795이");
        first.last_mut().unwrap().wrapped = true;
        let second = row("에요 다음");
        let mut rows = [first, second];
        for cell in rows.iter_mut().flatten().take(27) {
            cell.fg = Color::Rgb(40, 90, 220);
            cell.underline = true;
        }
        let before = rows.clone();
        restyle_web_suffixes(&mut rows);
        assert_eq!(detect_links_rows(&rows.iter().map(Vec::as_slice).collect::<Vec<_>>()),
            vec![(vec![(0, 0, 21)], "http://localhost:8795".to_owned())]);
        for (r, cells) in rows.iter().enumerate() {
            for (c, cell) in cells.iter().enumerate() {
                assert_eq!(cell.ch, before[r][c].ch);
                assert_eq!(cell.bg, before[r][c].bg);
                if (r == 0 && c >= 21) || (r == 1 && c < 4) {
                    assert!(!cell.underline, "{r}:{c}");
                    assert_eq!(cell.fg, GridCell::blank().fg);
                    assert!(link_in("%test", |r| rows.get(r), c as u16, r as u16).is_none());
                } else if r == 0 {
                    assert!(cell.underline);
                }
            }
        }
    }

    #[test]
    fn web_suffix_style_preserves_foreign_background_and_inverse() {
        use kasa_bridge::screen::Color;
        for protected in [0, 1, 2] {
            let mut cells = row("http://localhost:8795를 다음");
            for cell in &mut cells[..23] {
                cell.fg = Color::Rgb(40, 90, 220);
                cell.underline = true;
            }
            match protected {
                0 => cells[21].inverse = true,
                1 => cells[21].bg = Color::Rgb(150, 0, 0),
                _ => cells[21].fg = Color::Rgb(150, 0, 0),
            }
            let before = cells[21].clone();
            let mut rows = [cells];
            restyle_web_suffixes(&mut rows);
            assert_eq!(rows[0][21], before);
        }
    }

    #[test]
    fn hard_wrapped_web_link_survives_tui_style_reset_and_margins() {
        let url = "https://tympanus.net/codrops/2026/02/18/joffrey-spitzer-portfolio-a-minimalist-astro-gsap-build-with-reveals-flip-transitions-and-subtle-motion/";
        for width in 54..=150 {
            let mut rows = vec![row("  Joffrey Spitzer 제작기 — ")];
            for ch in url.chars() {
                if rows.last().unwrap().len() == width - 2 {
                    rows.last_mut().unwrap().resize(width, GridCell::blank());
                    rows.push(row("  "));
                }
                let mut cell = GridCell::blank();
                cell.ch = ch;
                cell.underline = rows.len() == 1;
                rows.last_mut().unwrap().push(cell);
            }
            for cells in &mut rows {
                cells.resize(width, GridCell::blank());
            }
            rows.push(row("  메뉴 글자가 커지면서 페이지 제목이 돼요."));
            let links = detect_links_rows(&rows.iter().map(Vec::as_slice).collect::<Vec<_>>());
            assert_eq!(links.len(), 1, "width={width}");
            assert_eq!(links[0].1, url, "width={width}");
            for &(r, s, e) in &links[0].0 {
                for col in s..e {
                    assert_eq!(link_in("%test", |r| rows.get(r), col, r as u16).unwrap().2, url);
                }
            }
            assert!(link_in("%test", |r| rows.get(r), 2, (rows.len() - 1) as u16).is_none());
        }
    }

    #[test]
    fn padded_links_do_not_swallow_new_urls_or_prose() {
        for next in [
            "  ordinary explanation follows",
            "  메뉴 글자가 커지면서 페이지 제목이 돼요.",
            "  https://another.example/next-page",
            "  - next-item in a list",
            "  /tmp/another-file.txt",
        ] {
            let mut rows = [row("  https://example.com/long-path/  "), row(next)];
            let width = rows[0].len();
            rows[1].resize(width, GridCell::blank());
            let links = detect_links_rows(&rows.iter().map(Vec::as_slice).collect::<Vec<_>>());
            assert_eq!(links[0].1, "https://example.com/long-path/", "{next}");
            assert_eq!(links[0].0.len(), 1, "{next}");
            if next.contains("https://") {
                for cell in rows.iter_mut().flatten() {
                    cell.underline = cell.ch != ' ';
                }
                let links = detect_links_rows(&rows.iter().map(Vec::as_slice).collect::<Vec<_>>());
                assert_eq!(links[0].1, "https://example.com/long-path/");
                assert_eq!(links.len(), 2);
            }
        }
        for path in ["/tmp/local-file.txt", "src/relative-file.rs", "~/local-file.txt"] {
            let mut rows = [row(&format!("  {path}  ")), row("  other-path/segment")];
            let width = rows[0].len();
            rows[1].resize(width, GridCell::blank());
            let links = detect_links_rows(&rows.iter().map(Vec::as_slice).collect::<Vec<_>>());
            assert_eq!(links[0].1, path);
            assert_eq!(links[0].0.len(), 1);
        }
    }

    #[test]
    fn file_paths_keep_spaces_unicode_and_location_suffixes() {
        for (text, expected) in [
            ("`/tmp/한글 폴더/시안 A.png`", "/tmp/한글 폴더/시안 A.png"),
            ("[문서](</tmp/My Report.md>)", "/tmp/My Report.md"),
            ("~/Desktop/result.png", "~/Desktop/result.png"),
            ("src/main.rs:42:7", "src/main.rs:42:7"),
            ("../notes/result.md", "../notes/result.md"),
            ("screen.png", "screen.png"),
            (
                r#""C:\Users\Name\My File.png""#,
                r"C:\Users\Name\My File.png",
            ),
            ("file:///tmp/a%20b.png", "file:///tmp/a%20b.png"),
        ] {
            assert_eq!(
                detect_links_row(&row(text))
                    .iter()
                    .map(|l| l.2.as_str())
                    .collect::<Vec<_>>(),
                vec![expected],
                "{text}"
            );
        }
        use std::path::Path;
        assert_eq!(
            resolve_path("src/main.rs:42:7", Some(Path::new("/project")), None).unwrap(),
            Path::new("/project/src/main.rs")
        );
        assert_eq!(
            resolve_path("~/한글 폴더/a.png", None, Some(Path::new("/home/user"))).unwrap(),
            Path::new("/home/user/한글 폴더/a.png")
        );
        #[cfg(unix)]
        assert_eq!(
            resolve_path("file:///tmp/a%20b.png", None, None).unwrap(),
            Path::new("/tmp/a b.png")
        );
        assert!(resolve_path("relative.md", None, None).is_none());
    }

    #[test]
    fn file_link_excludes_korean_sentence_suffix_from_target_and_hit_area() {
        let prefix = "파일은 ";
        let path = "~/Desktop/asr-compare/index.html";
        let cells = row(&format!("{prefix}{path}이야. 바로 열어봐"));
        let start = row(prefix).len() as u16;
        let end = start + path.len() as u16;
        assert_eq!(
            detect_links_row(&cells),
            vec![(start, end, path.to_owned())]
        );
        let rows = [cells];
        assert_eq!(
            link_in("%test", |r| rows.get(r), end - 1, 0).unwrap().2,
            path
        );
        for col in end..end + 4 {
            assert!(link_in("%test", |r| rows.get(r), col, 0).is_none());
        }
        assert_eq!(
            resolve_path(path, None, Some(std::path::Path::new("/home/user"))).unwrap(),
            std::path::Path::new("/home/user/Desktop/asr-compare/index.html")
        );
    }

    #[test]
    fn file_suffixes_follow_extensions_and_source_locations() {
        for (text, expected) in [
            ("/tmp/결과.html입니다.", "/tmp/결과.html"),
            ("./보고서.pdf를", "./보고서.pdf"),
            ("../이미지/최종.PNG이에요", "../이미지/최종.PNG"),
            ("사진.png는", "사진.png"),
            ("보고서.docx라고", "보고서.docx"),
            ("src/main.rs:42에서", "src/main.rs:42"),
            ("src/main.rs:42:7이고요.", "src/main.rs:42:7"),
            ("file:///tmp/결과.html이야.", "file:///tmp/결과.html"),
            ("file:///tmp/a%20b.png를", "file:///tmp/a%20b.png"),
            (r"C:\Users\Name\결과.html입니다", r"C:\Users\Name\결과.html"),
            ("/tmp/huohuo-rec/record.mjs입니다.", "/tmp/huohuo-rec/record.mjs"),
            ("/tmp/build/run.sh를", "/tmp/build/run.sh"),
            ("/var/log/app.log이에요", "/var/log/app.log"),
            ("~/notes/plan.drawio입니다", "~/notes/plan.drawio"),
            ("record.mjs입니다", "record.mjs"),
        ] {
            assert_eq!(
                detect_links_row(&row(text)),
                vec![(0, row(expected).len() as u16, expected.to_owned())],
                "{text}"
            );
        }
    }

    #[test]
    fn explicit_names_and_ambiguous_unicode_paths_are_not_shortened() {
        for (text, expected) in [
            ("`~/Desktop/index.html이야`", "~/Desktop/index.html이야"),
            ("\"/tmp/시안 A.png는\"", "/tmp/시안 A.png는"),
            ("'/tmp/시안 A.png는'", "/tmp/시안 A.png는"),
            ("[결과](</tmp/시안 A.png는>)", "/tmp/시안 A.png는"),
            ("“/tmp/시안 A.png는”", "/tmp/시안 A.png는"),
            ("‘/tmp/시안 A.png는’", "/tmp/시안 A.png는"),
            ("`file:///tmp/결과.html이야`", "file:///tmp/결과.html이야"),
            ("/tmp/이야.png", "/tmp/이야.png"),
            ("/tmp/자료.html이야.png", "/tmp/자료.html이야.png"),
            ("/tmp/자료.html이야/보고서", "/tmp/자료.html이야/보고서"),
            ("/tmp/결과.html한글", "/tmp/결과.html한글"),
            ("/tmp/파일.자료", "/tmp/파일.자료"),
            ("https://example.com/이야", "https://example.com/이야"),
            ("https://example.com/결과.html이야", "https://example.com/결과.html이야"),
            ("https://example.com/?q=결과.html이야", "https://example.com/?q=결과.html이야"),
            ("https://example.com/#결과.html이야", "https://example.com/#결과.html이야"),
            ("file:///tmp/a?q=결과.html이야", "file:///tmp/a?q=결과.html이야"),
            ("file:///tmp/a#결과.html이야", "file:///tmp/a#결과.html이야"),
        ] {
            assert_eq!(
                detect_links_row(&row(text))
                    .iter()
                    .map(|(_, _, target)| target.as_str())
                    .collect::<Vec<_>>(),
                vec![expected],
                "{text}"
            );
        }
    }

    #[test]
    fn wrapped_korean_file_suffix_is_outside_every_clickable_span() {
        let mut first = row("파일 ~/Desktop/한글/");
        first.last_mut().unwrap().wrapped = true;
        let mut second = row("결과.html이");
        second.last_mut().unwrap().wrapped = true;
        let third = row("야. 다음 설명");
        let path = "~/Desktop/한글/결과.html";
        let expected = vec![
            (0, row("파일 ").len() as u16, first.len() as u16),
            (1, 0, row("결과.html").len() as u16),
        ];
        assert_eq!(
            detect_links_rows(&[&first, &second, &third]),
            vec![(expected.clone(), path.to_owned())]
        );
        let rows = [first, second, third];
        for (r, cells) in rows.iter().enumerate() {
            for col in 0..cells.len() as u16 {
                let expected_hit = expected.iter().any(|(row, start, end)| {
                    *row == r && (*start..*end).contains(&col)
                });
                let hit = link_in("%test", |r| rows.get(r), col, r as u16);
                assert_eq!(hit.is_some(), expected_hit, "row={r}, col={col}");
                if let Some((_, _, target)) = hit {
                    assert_eq!(target, path);
                }
            }
        }
    }

    #[test]
    fn wide_glyph_spacers_do_not_break_a_path_and_remain_clickable() {
        for spacer in [' ', '\0'] {
            let mut cells = Vec::new();
            for ch in "`/tmp/한글 폴더/시안 A.png`".chars() {
                cells.push(GridCell {
                    ch,
                    ..GridCell::blank()
                });
                if unicode_width::UnicodeWidthChar::width(ch) == Some(2) {
                    cells.push(GridCell {
                        ch: spacer,
                        ..GridCell::blank()
                    });
                }
            }
            let links = detect_links_row(&cells);
            assert_eq!(
                links,
                vec![(
                    1,
                    cells.len() as u16 - 1,
                    "/tmp/한글 폴더/시안 A.png".to_string()
                )]
            );
        }
    }

    #[test]
    fn plain_text_and_hidden_text_are_not_links() {
        assert!(detect_links_row(&row("일반 문장 / 1.2.3 hello world")).is_empty());
        let mut cells = row("https://secret.example.com");
        for cell in &mut cells {
            cell.hidden = true;
        }
        assert!(detect_links_row(&cells).is_empty());
    }
}
