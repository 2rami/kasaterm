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
    scan::row_join(prev, next).is_some()
}

pub(crate) fn detect_links_rows(rows: &[&[GridCell]]) -> Vec<(Vec<(usize, u16, u16)>, String)> {
    let mut chars = Vec::new();
    let mut positions = Vec::new();
    for (r, row) in rows.iter().enumerate() {
        let start = if r > 0 {
            scan::row_join(rows[r - 1], row).map_or(0, |(_, s)| s)
        } else {
            0
        };
        let join = rows.get(r + 1).and_then(|next| scan::row_join(row, next));
        let end = join.map_or(row.len(), |(e, _)| e);
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
        if join.is_none() {
            chars.push(' ');
            positions.push(None);
        }
    }
    scan::scan(&chars)
        .into_iter()
        .map(|(start, end, target)| {
            let mut spans: Vec<(usize, u16, u16)> = Vec::new();
            for &(row, start, end) in positions[start..end].iter().flatten() {
                match spans.last_mut() {
                    Some((r, _, e)) if *r == row && *e == start => *e = end,
                    _ => spans.push((row, start, end)),
                }
            }
            (spans, target)
        })
        .collect()
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
