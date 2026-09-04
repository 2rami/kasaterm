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

/// 한 행을 스캔해 (col_start, col_end, url) 목록을 돌려준다. hover/click
/// 은 커서가 올라간 행 하나만 이걸로 검사하면 된다.
pub(crate) fn detect_links_row(row: &[GridCell]) -> Vec<(u16, u16, String)> {
    let chars: Vec<char> = row
        .iter()
        .map(|c| if c.ch == '\0' { ' ' } else { c.ch })
        .collect();
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
                    out.push((i as u16, end as u16, url));
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
    /// 픽셀 좌표 아래에 클릭 가능한 URL 이 있으면 그 pane·셀범위·주소를
    /// 돌려준다. hover 밑줄/커서·클릭 모두 이걸 쓴다. 커서가 올라간 pane 의
    /// 해당 행만 스캔하므로 CursorMoved 마다 불러도 가볍다.
    pub(crate) fn link_hit(&self, px: f32, py: f32) -> Option<(String, LinkSpan, String)> {
        let (pid, col, row) = self.px_to_pane_cell(px, py)?;
        let ws = self.ws.lock().unwrap();
        let term = ws.panes.get(&pid)?.term()?;
        // 화면은 렌더가 옮겨 그린 것이라 원본 글자판과 행이 어긋난다. 클릭 좌표는
        // 화면 기준이므로 같은 옮김을 되짚어야 눈에 보이는 그 링크가 열린다 —
        // 원본을 그대로 보면 당긴 줄 수만큼 아래 줄에서 주소를 찾는다(복사와 같은
        // 뿌리, 2026-09-05).
        let cells = match self.pane_view_shift.get(&pid) {
            Some(shift) => shift.row(row as usize, &term.cells)?,
            None => term.cells.get(row as usize)?,
        };
        detect_links_row(cells)
            .into_iter()
            .find(|(s, e, _)| col >= *s && col < *e)
            .map(|(s, e, url)| (pid.clone(), LinkSpan { row, col_start: s, col_end: e }, url))
    }
}
