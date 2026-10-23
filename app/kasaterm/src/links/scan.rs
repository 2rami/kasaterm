use super::*;

fn boundary(ch: char) -> bool {
    ch.is_whitespace()
        || matches!(
            ch,
            '(' | '[' | '<' | '"' | '\'' | '`' | '“' | '‘' | '=' | ':' | '：'
        )
}

fn bare_host(chars: &[char]) -> bool {
    let host: String = chars
        .iter()
        .take_while(|c| !c.is_whitespace() && !matches!(c, '/' | ':' | '?' | '#' | ')' | ']'))
        .collect();
    let host = &host[..korean_suffix_start(&host).unwrap_or(host.len())];
    let Some((name, tld)) = host.rsplit_once('.') else {
        return false;
    };
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.'))
        && matches!(
            tld.to_ascii_lowercase().as_str(),
            "com"
                | "org"
                | "net"
                | "io"
                | "dev"
                | "app"
                | "ai"
                | "co"
                | "kr"
                | "jp"
                | "edu"
                | "gov"
        )
}

fn web_prefix(chars: &[char]) -> bool {
    let prefix: String = chars
        .iter()
        .take(12)
        .collect::<String>()
        .to_ascii_lowercase();
    [
        "https://",
        "http://",
        "www.",
        "mailto:",
        "file://",
        "localhost:",
        "127.0.0.1:",
    ]
    .iter()
    .any(|s| prefix.starts_with(s))
        || bare_host(chars)
}

fn file_prefix(chars: &[char]) -> bool {
    chars.first() == Some(&'/')
        || chars.starts_with(&['~', '/'])
        || chars.starts_with(&['.', '/'])
        || chars.starts_with(&['.', '.', '/'])
        || (chars.first().is_some_and(char::is_ascii_alphabetic)
            && chars.get(1) == Some(&':')
            && matches!(chars.get(2), Some('/' | '\\')))
}

fn known_extension(ext: &str) -> bool {
    matches!(
        ext.to_ascii_lowercase().as_str(),
        "md" | "txt"
            | "pdf"
            | "png"
            | "jpg"
            | "jpeg"
            | "webp"
            | "gif"
            | "svg"
            | "mp4"
            | "mov"
            | "mp3"
            | "wav"
            | "rs"
            | "py"
            | "js"
            | "mjs"
            | "cjs"
            | "jsx"
            | "ts"
            | "tsx"
            | "sh"
            | "log"
            | "json"
            | "yaml"
            | "yml"
            | "toml"
            | "html"
            | "css"
            | "zip"
            | "csv"
            | "xlsx"
            | "docx"
    )
}

fn relative_file(value: &str) -> bool {
    let path = strip_location(value);
    !path.contains("://")
        && (path.contains('/')
            || path
                .rsplit_once('.')
                .is_some_and(|(_, ext)| known_extension(ext)))
        && path.rsplit('/').next().is_some_and(|name| {
            name.rsplit_once('.').is_some_and(|(stem, ext)| {
                !stem.is_empty() && !ext.is_empty() && ext.chars().all(char::is_alphanumeric)
            })
        })
}

pub(super) fn strip_location(value: &str) -> &str {
    let mut end = value.len();
    for _ in 0..2 {
        if let Some((before, digits)) = value[..end].rsplit_once(':') {
            if !digits.is_empty() && digits.bytes().all(|c| c.is_ascii_digit()) {
                end = before.len();
            } else {
                break;
            }
        }
    }
    &value[..end]
}

fn korean_suffix_start(value: &str) -> Option<usize> {
    let end = value
        .char_indices()
        .rev()
        .find(|(_, ch)| !('\u{ac00}'..='\u{d7a3}').contains(ch))
        .map(|(i, ch)| i + ch.len_utf8())?;
    if !matches!(
        &value[end..],
        "은" | "는"
            | "이"
            | "가"
            | "을"
            | "를"
            | "의"
            | "에"
            | "에서"
            | "에는"
            | "에서는"
            | "에도"
            | "에만"
            | "에서만"
            | "와"
            | "과"
            | "로"
            | "으로"
            | "로는"
            | "으로는"
            | "로도"
            | "으로도"
            | "부터"
            | "까지"
            | "부터는"
            | "까지는"
            | "도"
            | "만"
            | "처럼"
            | "보다"
            | "랑"
            | "이랑"
            | "하고"
            | "라고"
            | "이라고"
            | "라는"
            | "이라는"
            | "이며"
            | "이고"
            | "이고요"
            | "이나"
            | "이든"
            | "인데"
            | "인가"
            | "인지"
            | "이야"
            | "야"
            | "이에요"
            | "예요"
            | "입니다"
            | "입니다만"
            | "이다"
            | "이네요"
            | "이군요"
            | "이거든요"
            | "이잖아요"
            | "이라서"
            | "이므로"
            | "이니까"
            | "이면서"
            | "이라면"
            | "이더라"
            | "이었어요"
            | "였어요"
            | "였다"
            | "이었다"
            | "이래요"
            | "이랍니다"
    ) {
        return None;
    }
    Some(end)
}

fn korean_file_suffix(value: &str) -> Option<usize> {
    let end = korean_suffix_start(value)?;
    let path = strip_location(&value[..end]);
    let name = path.rsplit(['/', '\\']).next()?;
    let (stem, extension) = name.rsplit_once('.')?;
    // 한글 파일명·URL 경로도 유효하므로 확장자 뒤의 정확한 문장 어미만 분리한다. 확장자 목록은 짧은 이름을 파일로
    // 볼지 가르는 데 쓰고, 여기서는 ASCII 확장자면 된다 — 목록에 없던 `.mjs` 뒤의 「입니다」가 밑줄에 들어갔다.
    (!stem.is_empty() && ascii_extension(extension)).then_some(end)
}

fn korean_web_suffix(value: &str) -> Option<usize> {
    let end = korean_suffix_start(value)?;
    let head = &value[..end];
    let lower = head.to_ascii_lowercase();
    let authority = if lower.starts_with("https://") {
        &head[8..]
    } else if lower.starts_with("http://") {
        &head[7..]
    } else {
        head
    };
    // 경로·질의의 한글은 주소 자체일 수 있지만, 숫자 포트 뒤 한글은 포트가 될 수 없다.
    if authority.contains(['/', '\\', '?', '#', '@']) {
        return None;
    }
    let numeric_port = authority.rsplit_once(':').is_some_and(|(_, port)| {
        !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit())
    });
    (numeric_port || authority.eq_ignore_ascii_case("localhost")
        || bare_host(&authority.chars().collect::<Vec<_>>())).then_some(end)
}

pub(super) struct Link {
    pub start: usize,
    pub end: usize,
    pub target: String,
    pub suffix_end: usize,
}

fn ascii_extension(ext: &str) -> bool {
    (1..=10).contains(&ext.len())
        && ext.bytes().all(|b| b.is_ascii_alphanumeric())
        && ext.bytes().any(|b| b.is_ascii_alphabetic())
}

fn closing_delimiter(open: char) -> Option<char> {
    match open {
        '"' | '\'' | '`' => Some(open),
        '<' => Some('>'),
        '“' => Some('”'),
        '‘' => Some('’'),
        _ => None,
    }
}

fn explicitly_delimited(chars: &[char], start: usize, end: usize) -> bool {
    start
        .checked_sub(1)
        .and_then(|i| closing_delimiter(chars[i]))
        .is_some_and(|close| chars.get(end) == Some(&close))
}

fn token_end(chars: &[char], start: usize, web: bool) -> usize {
    let enclosing = start.checked_sub(1).map(|i| chars[i]);
    let spaces = !web && enclosing.is_some_and(|ch| closing_delimiter(ch).is_some() || ch == '(');
    let mut parentheses = 0;
    let mut brackets = 0;
    let mut end = start;
    while let Some(&ch) = chars.get(end) {
        if ch.is_control()
            || (ch.is_whitespace() && !spaces)
            || matches!(
                ch,
                '<' | '>'
                    | '"'
                    | '\''
                    | '`'
                    | '|'
                    | '“'
                    | '”'
                    | '‘'
                    | '’'
                    | '。'
                    | '，'
                    | '、'
                    | '（'
                    | '）'
            )
        {
            break;
        }
        match ch {
            '(' => parentheses += 1,
            ')' if parentheses == 0 => break,
            ')' => parentheses -= 1,
            '[' => brackets += 1,
            ']' if brackets == 0 => break,
            ']' => brackets -= 1,
            _ => {}
        }
        end += 1;
    }
    if explicitly_delimited(chars, start, end) {
        return end;
    }
    while end > start
        && (chars[end - 1].is_whitespace()
            || matches!(chars[end - 1], '.' | ',' | ';' | ':' | '!' | '?'))
    {
        end -= 1;
    }
    end
}

pub(super) fn scan(chars: &[char]) -> Vec<Link> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if i > 0 && !boundary(chars[i - 1]) {
            i += 1;
            continue;
        }
        let web = web_prefix(&chars[i..]);
        let file = file_prefix(&chars[i..]);
        let mut end = token_end(chars, i, web);
        if end <= i {
            i += 1;
            continue;
        }
        let mut value: String = chars[i..end].iter().collect();
        let mut suffix_end = None;
        if web && !explicitly_delimited(chars, i, end) {
            if let Some(byte_end) = korean_web_suffix(&value) {
                suffix_end = Some(end);
                end = i + value[..byte_end].chars().count();
                value.truncate(byte_end);
            }
        }
        if (!web
            || (value.to_ascii_lowercase().starts_with("file://") && !value.contains(['?', '#'])))
            && !explicitly_delimited(chars, i, end)
        {
            if let Some(byte_end) = korean_file_suffix(&value) {
                end = i + value[..byte_end].chars().count();
                value.truncate(byte_end);
            }
        }
        let target = if web {
            let lower = value.to_ascii_lowercase();
            let url = if lower.starts_with("www.") || bare_host(&chars[i..end]) {
                format!("https://{value}")
            } else if lower.starts_with("localhost:") || lower.starts_with("127.0.0.1:") {
                format!("http://{value}")
            } else {
                value.clone()
            };
            url::Url::parse(&url)
                .ok()
                .filter(|u| match u.scheme() {
                    "http" | "https" => u.host_str().is_some(),
                    "file" => !u.path().is_empty(),
                    "mailto" => u.path().contains('@'),
                    _ => false,
                })
                .map(|_| url)
        } else if (file && value != "/" && value != "./" && value != "../") || relative_file(&value)
        {
            Some(value)
        } else {
            None
        };
        if let Some(target) = target {
            out.push(Link { start: i, end, target, suffix_end: suffix_end.unwrap_or(end) });
            i = end;
        } else if web {
            // 잘못된 URL의 `//host`를 로컬 파일 경로로 다시 해석하면 다른 대상을 연다.
            i = end;
        } else {
            i += 1;
        }
    }
    out
}

fn visible(cell: &GridCell) -> bool {
    cell.ch != ' ' && cell.ch != '\0' && !cell.hidden
}

pub(super) fn web_row_join(prev: &[GridCell], next: &[GridCell]) -> Option<(usize, usize)> {
    let end = prev.iter().rposition(visible)? + 1;
    let start = next.iter().position(visible)?;
    if prev.len() != next.len() || prev.len() - end > 4 || start > 4 {
        return None;
    }
    let after: Vec<char> = next[start..]
        .iter()
        .map(|c| if c.hidden { ' ' } else { c.ch })
        .collect();
    if web_prefix(&after) {
        return None;
    }
    let stop = token_end(&after, 0, true);
    let fragment = &after[..stop];
    if fragment.is_empty()
        || (file_prefix(fragment) && relative_file(&fragment.iter().collect::<String>()))
        || !fragment.iter().all(|c| {
            c.is_ascii_alphanumeric()
                || matches!(
                    c,
                    '-' | '.'
                        | '_'
                        | '~'
                        | '/'
                        | ':'
                        | '?'
                        | '#'
                        | '@'
                        | '%'
                        | '='
                        | '&'
                        | '+'
                        | '('
                        | ')'
                )
        })
    {
        return None;
    }
    let alone = after[stop..].iter().all(|c| {
        c.is_whitespace() || matches!(c, '\0' | ')' | ']' | '>' | '.' | ',' | ';' | '!' | '?')
    });
    let path = fragment.iter().any(char::is_ascii_alphanumeric)
        && fragment
            .iter()
            .any(|c| matches!(c, '/' | '-' | '_' | '%' | '=' | '&' | '?' | '#'));
    (alone || path).then_some((end, start))
}

pub(super) fn ends_in_web_link(chars: &[char]) -> bool {
    scan(chars).last().is_some_and(|link| {
        (link.target
            .get(..8)
            .is_some_and(|s| s.eq_ignore_ascii_case("https://"))
            || link.target
                .get(..7)
                .is_some_and(|s| s.eq_ignore_ascii_case("http://")))
            && chars[link.end..].iter().all(|c| matches!(c, '.' | ':' | '?'))
    })
}

pub(super) fn row_join(prev: &[GridCell], next: &[GridCell]) -> Option<(usize, usize)> {
    let end = prev.iter().rposition(visible)? + 1;
    let start = next.iter().position(visible)?;
    if prev.last().is_some_and(|cell| cell.wrapped) {
        return Some((prev.len(), 0));
    }
    if web_prefix(&next[start..].iter().map(|cell| cell.ch).collect::<Vec<_>>()) {
        return None;
    }
    let trailing = prev.len() - end;
    let last = prev[end - 1].ch;
    let first = next[start].ch;
    if matches!(last, ')' | ']' | '>' | '.' | ',' | ';' | '!' | '?')
        || boundary(first)
        || matches!(first, ')' | ']' | '>')
    {
        return None;
    }
    if trailing == 0 && start == 0 {
        return Some((end, start));
    }
    // TUI 여백을 실제 주소 공백으로 읽지 않되, 보통 문단까지 이어 붙이지 않는다.
    if trailing <= 4
        && start <= 4
        && prev[end - 1].underline
        && next[start].underline
        && prev[end - 1].fg == next[start].fg
    {
        return Some((end, start));
    }
    None
}
