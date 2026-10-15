use super::*;

fn boundary(ch: char) -> bool {
    ch.is_whitespace() || matches!(ch, '(' | '[' | '<' | '"' | '\'' | '`' | '=' | ':' | '：')
}

fn bare_host(chars: &[char]) -> bool {
    let host: String = chars
        .iter()
        .take_while(|c| !c.is_whitespace() && !matches!(c, '/' | ':' | '?' | '#' | ')' | ']'))
        .collect();
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

fn relative_file(value: &str) -> bool {
    let path = strip_location(value);
    !path.contains("://")
        && (path.contains('/')
            || path.rsplit_once('.').is_some_and(|(_, ext)| {
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
                        | "ts"
                        | "tsx"
                        | "json"
                        | "yaml"
                        | "toml"
                        | "html"
                        | "css"
                        | "zip"
                        | "csv"
                        | "xlsx"
                        | "docx"
                )
            }))
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

fn token_end(chars: &[char], start: usize, web: bool) -> usize {
    let enclosing = start.checked_sub(1).map(|i| chars[i]);
    let spaces = !web && matches!(enclosing, Some('"' | '\'' | '`' | '<' | '('));
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
    while end > start
        && (chars[end - 1].is_whitespace()
            || matches!(chars[end - 1], '.' | ',' | ';' | ':' | '!' | '?'))
    {
        end -= 1;
    }
    end
}

pub(super) fn scan(chars: &[char]) -> Vec<(usize, usize, String)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if i > 0 && !boundary(chars[i - 1]) {
            i += 1;
            continue;
        }
        let web = web_prefix(&chars[i..]);
        let file = file_prefix(&chars[i..]);
        let end = token_end(chars, i, web);
        if end <= i {
            i += 1;
            continue;
        }
        let value: String = chars[i..end].iter().collect();
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
            out.push((i, end, target));
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

pub(super) fn row_join(prev: &[GridCell], next: &[GridCell]) -> Option<(usize, usize)> {
    let end = prev.iter().rposition(visible)? + 1;
    let start = next.iter().position(visible)?;
    if prev.last().is_some_and(|cell| cell.wrapped) {
        return Some((prev.len(), 0));
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
