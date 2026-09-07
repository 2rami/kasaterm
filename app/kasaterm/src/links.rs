//! 터미널 셀 본문에서 URL 을 감지한다. OSC 8 하이퍼링크는 vt 파서가
//! 보존하지 않으므로, 보이는 행의 텍스트를 직접 스캔해 클릭 가능한
//! 범위를 뽑아낸다. 한 셀 == 한 char(col) 매핑이라 col 인덱스가 그대로
//! 스캔 위치다(와이드 글자의 '\0' spacer 는 공백으로 친다).
use super::*;

/// 렌더 밑줄용 URL 범위. `col_start..col_end` (exclusive) 가 밑줄 대상
/// 셀 범위다. 실제 여는 주소(url)는 hover/click 이 그때그때 행을 다시
/// 스캔해 얻으므로 여기엔 담지 않는다.
#[derive(Debug, Clone)]
pub(crate) struct LinkSpan {
    pub row: u16,
    pub col_start: u16,
    pub col_end: u16,
}

/// URL 본문에 들어갈 수 있는 글자. 공백·제어문자와 셸/마크업이 경계로
/// 쓰는 따옴표류·꺾쇠는 여기서 끊는다.
fn is_url_char(c: char) -> bool {
    !c.is_whitespace()
        && !c.is_control()
        && !matches!(c, '"' | '\'' | '`' | '<' | '>' | '{' | '}' | '|' | '\\' | '^')
}

/// 문장 끝에 붙은 URL 의 후행 구두점은 링크에서 뗀다. 닫는 괄호는
/// URL 안에 짝이 있으면(예: 위키 경로) 살리고, 없으면 잘라낸다.
fn trim_trailing(chars: &[char], start: usize, mut end: usize) -> usize {
    while end > start {
        let c = chars[end - 1];
        let strip = match c {
            '.' | ',' | ';' | ':' | '!' | '?' => true,
            ')' => !chars[start..end].contains(&'('),
            ']' => !chars[start..end].contains(&'['),
            '}' => !chars[start..end].contains(&'{'),
            _ => false,
        };
        if strip {
            end -= 1;
        } else {
            break;
        }
    }
    end
}

/// 한 행을 스캔해 (col_start, col_end, url) 목록을 돌려준다. 줄이 안 꺾인 짧은
/// 주소용 — hover/click 은 `detect_links_rows` 로 이웃 행까지 잇는다.
pub(crate) fn detect_links_row(row: &[GridCell]) -> Vec<(u16, u16, String)> {
    detect_links_rows(&[row])
        .into_iter()
        .filter_map(|(segs, url)| segs.first().map(|(_, s, e)| (*s, *e, url)))
        .collect()
}

/// 앞 행이 다음 행으로 **이어지는가** — 좁은 창에서 긴 주소가 꺾인 자리.
///
/// 정본은 앞 행 마지막 칸의 소프트 랩 표식(`Cell::wrapped`, alacritty `WRAPLINE`)이다.
/// 표식이 없는 글자판(옛 빌드의 거울·레거시 브리지·저장 스냅샷)을 위해 채움 휴리스틱을
/// 함께 둔다: 앞 행이 **끝 칸까지** URL 글자로 차 있고 다음 행이 URL 글자로 시작하면
/// 잇는다. 하드 개행은 거의 늘 끝에 빈 칸을 남기므로 오판은 「주소가 폭에 딱 맞게
/// 끝나고 다음 줄이 곧장 글자로 시작」할 때뿐이다.
fn joins(prev: &[GridCell], next: &[GridCell]) -> bool {
    let (Some(last), Some(first)) = (prev.last(), next.first()) else {
        return false;
    };
    if last.wrapped {
        return true;
    }
    let url_glyph = |c: char| c != ' ' && c != '\0' && is_url_char(c);
    url_glyph(last.ch) && url_glyph(first.ch)
}

/// 이웃한 행들을 이어 붙여 스캔한다. 한 링크는 (세그먼트들, url) 이고, 세그먼트는
/// `(rows 안의 행 인덱스, col_start, col_end)` — 여러 행에 걸친 주소는 행마다 하나씩
/// 나온다(밑줄은 행 단위로 그리므로). 안 이어지는 행 사이엔 공백을 끼워 주소가 그
/// 경계에서 끊기게 한다.
pub(crate) fn detect_links_rows(rows: &[&[GridCell]]) -> Vec<(Vec<(usize, u16, u16)>, String)> {
    let mut chars: Vec<char> = Vec::new();
    let mut pos: Vec<Option<(usize, usize)>> = Vec::new();
    for (r, row) in rows.iter().enumerate() {
        for (c, cell) in row.iter().enumerate() {
            chars.push(if cell.ch == '\0' { ' ' } else { cell.ch });
            pos.push(Some((r, c)));
        }
        if rows.get(r + 1).is_some_and(|next| !joins(row, next)) {
            chars.push(' ');
            pos.push(None);
        }
    }
    let n = chars.len();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < n {
        // 스킴 후보 첫 글자에서만 부분 문자열을 만들어 O(n²) 를 피한다.
        let starts = matches!(chars[i], 'h' | 'f' | 'w');
        if starts {
            let tail: String = chars[i..(i + 8).min(n)].iter().collect();
            let is_www = tail.starts_with("www.");
            let scheme = tail.starts_with("https://")
                || tail.starts_with("http://")
                || tail.starts_with("file://");
            if scheme || is_www {
                let mut j = i;
                while j < n && is_url_char(chars[j]) {
                    j += 1;
                }
                let end = trim_trailing(&chars, i, j);
                // 스킴/www. 만으로는 링크로 치지 않는다(최소 도메인 몸통 요구).
                if end > i + 8 || (is_www && end > i + 5) {
                    let body: String = chars[i..end].iter().collect();
                    let url = if is_www { format!("https://{body}") } else { body };
                    let mut segs: Vec<(usize, u16, u16)> = Vec::new();
                    for p in pos[i..end].iter().flatten() {
                        match segs.last_mut() {
                            Some((r, _, e)) if *r == p.0 && *e as usize == p.1 => *e += 1,
                            _ => segs.push((p.0, p.1 as u16, p.1 as u16 + 1)),
                        }
                    }
                    out.push((segs, url));
                }
                i = j.max(i + 1);
                continue;
            }
        }
        i += 1;
    }
    out
}

impl App {
    /// 픽셀 좌표 아래에 클릭 가능한 URL 이 있으면 그 pane·밑줄 셀범위들·주소를
    /// 돌려준다. hover 밑줄/커서·클릭 모두 이걸 쓴다. 커서가 올라간 행에서 위아래로
    /// **이어진 행**(줄이 꺾인 주소)만 더 보므로 CursorMoved 마다 불러도 가볍다.
    pub(crate) fn link_hit(&self, px: f32, py: f32) -> Option<(String, Vec<LinkSpan>, String)> {
        let (pid, col, row) = self.px_to_pane_cell(px, py)?;
        let ws = self.ws.lock().unwrap();
        let term = ws.panes.get(&pid)?.term()?;
        // 화면은 렌더가 옮겨 그린 것이라 원본 글자판과 행이 어긋난다. 클릭 좌표는
        // 화면 기준이므로 같은 옮김을 되짚어야 눈에 보이는 그 링크가 열린다 —
        // 원본을 그대로 보면 당긴 줄 수만큼 아래 줄에서 주소를 찾는다(복사와 같은
        // 뿌리, 2026-09-05).
        let shift = self.pane_view_shift.get(&pid);
        let view = |r: usize| -> Option<&Vec<GridCell>> {
            match shift {
                Some(shift) => shift.row(r, &term.cells),
                None => term.cells.get(r),
            }
        };
        let row = row as usize;
        // 이어진 행을 위아래로 모은다 — 상한은 화면 폭이 아주 좁아도 긴 주소가 드는 만큼.
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
                (pid.clone(), spans, url)
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(s: &str) -> Vec<GridCell> {
        s.chars().map(|ch| GridCell { ch, ..GridCell::blank() }).collect()
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
}
