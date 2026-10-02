//! 원격 승인(docs/remote-approval.md)에 실리는 글 — 도구 입력을 사람이 읽는 칸으로 펴고, 비밀 값만
//! 가린다. 나머지는 한 글자도 바꾸지 않는다: 승인하는 사람이 보는 것이 실제로 돌 명령이어야 한다.
//!
//! 가리기는 보이기 전용이다. 실제 실행되는 입력은 Claude Code 안에 그대로 있고, 관문에는 가린 글만 간다.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// 가린 자리에 들어가는 표시. 길이를 남기지 않는다 — 길이도 단서가 된다.
pub const MASK: &str = "••••••";
/// 칸 하나·요청 하나에 싣는 글의 상한. 넘으면 자르고 `truncated` 를 켠다 — 잘린 요청은 허락할 수 없다.
pub const MAX_FIELD: usize = 48 * 1024;
pub const MAX_TOTAL: usize = 64 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Field {
    /// 도구 입력의 원래 키(`command`·`file_path` …). 표시 이름은 `label`.
    pub name: String,
    pub label: String,
    pub text: String,
}

/// 도구 입력 → 칸들(가린 뒤). 두 번째 값은 잘렸는지.
pub fn fields(tool: &str, input: &Value) -> (Vec<Field>, bool) {
    let mut out = Vec::new();
    let order: &[(&str, &str)] = match tool {
        "Bash" => &[("command", "명령"), ("description", "설명"), ("timeout", "시간 제한"), ("run_in_background", "백그라운드")],
        "Edit" => &[("file_path", "파일"), ("old_string", "바꿀 글"), ("new_string", "새 글"), ("replace_all", "모두 바꾸기")],
        "MultiEdit" => &[("file_path", "파일"), ("edits", "바꿀 것들")],
        "Write" => &[("file_path", "파일"), ("content", "내용")],
        "NotebookEdit" => &[("notebook_path", "노트북"), ("cell_id", "칸"), ("edit_mode", "방식"), ("new_source", "새 내용")],
        "WebFetch" => &[("url", "주소"), ("prompt", "물음")],
        "Read" => &[("file_path", "파일")],
        _ => &[],
    };
    let mut seen = std::collections::HashSet::new();
    for (name, label) in order {
        if let Some(value) = input.get(*name).filter(|v| !v.is_null()) {
            seen.insert(*name);
            out.push(Field { name: (*name).into(), label: (*label).into(), text: as_text(value) });
        }
    }
    // 모르는 키도 빠짐없이 — 표시에서 빠진 입력은 승인하는 사람이 못 본 입력이다.
    if let Some(map) = input.as_object() {
        for (name, value) in map {
            if seen.contains(name.as_str()) || value.is_null() {
                continue;
            }
            out.push(Field { name: name.clone(), label: name.clone(), text: as_text(value) });
        }
    } else if !input.is_null() {
        out.push(Field { name: "input".into(), label: "입력".into(), text: as_text(input) });
    }
    let mut truncated = false;
    let mut total = 0;
    for field in &mut out {
        field.text = mask(&field.text);
        let room = MAX_FIELD.min(MAX_TOTAL.saturating_sub(total));
        if field.text.len() > room {
            let mut cut = room;
            while !field.text.is_char_boundary(cut) {
                cut -= 1;
            }
            field.text.truncate(cut);
            truncated = true;
        }
        total += field.text.len();
    }
    (out, truncated)
}

fn as_text(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Bool(_) | Value::Number(_) => value.to_string(),
        _ => serde_json::to_string_pretty(value).unwrap_or_default(),
    }
}

/// 이름에 이 낱말이 들면 그 값은 비밀이다(대문자로 바꾸고 `_`·`-` 를 뺀 뒤 견준다).
const SECRET_WORDS: [&str; 10] = [
    "TOKEN", "SECRET", "PASSWORD", "PASSWD", "APIKEY", "ACCESSKEY", "PRIVATEKEY", "CREDENTIAL", "AUTHORIZATION", "COOKIE",
];
/// 이 머리로 시작하는 낱말은 이름 없이도 비밀이다(공급자들이 토큰에 붙이는 표식).
const TOKEN_PREFIXES: [&str; 18] = [
    "sk-", "ghp_", "gho_", "ghu_", "ghs_", "ghr_", "github_pat_", "xoxb-", "xoxp-", "xoxa-", "xoxs-", "xapp-", "glpat-",
    "AKIA", "ASIA", "AIza", "kdt_", "ya29.",
];

fn secret_name(name: &str) -> bool {
    let norm: String = name.chars().filter(|c| *c != '_' && *c != '-').collect::<String>().to_ascii_uppercase();
    if norm.is_empty() || norm == "PWD" || norm == "OLDPWD" {
        return false;
    }
    SECRET_WORDS.iter().any(|w| norm.contains(w)) || norm.ends_with("PASS") || norm == "PW" || norm == "AUTH"
}

fn is_name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.'
}

fn value_end(chars: &[char], start: usize) -> (usize, usize) {
    // (값 시작, 값 끝) — 따옴표면 따옴표 안만.
    match chars.get(start) {
        Some(&q) if q == '"' || q == '\'' => {
            let mut i = start + 1;
            while i < chars.len() && chars[i] != q {
                if chars[i] == '\\' && q == '"' {
                    i += 1;
                }
                i += 1;
            }
            (start + 1, i.min(chars.len()))
        }
        _ => {
            let mut i = start;
            while i < chars.len() && !chars[i].is_whitespace() && !";&|)},`'\"".contains(chars[i]) {
                i += 1;
            }
            (start, i)
        }
    }
}

/// 비밀로 보이는 값만 [`MASK`] 로 바꾼다. 나머지 글자는 그대로 둔다.
pub fn mask(text: &str) -> String {
    let text = mask_pem(text);
    let chars: Vec<char> = text.chars().collect();
    let mut hide = vec![false; chars.len()];
    let mark = |from: usize, to: usize, hide: &mut Vec<bool>| {
        for flag in hide.iter_mut().take(to).skip(from) {
            *flag = true;
        }
    };
    let mut i = 0;
    while i < chars.len() {
        let at_word = i == 0 || !is_name_char(chars[i - 1]);
        // 이름=값 · "이름": "값" · --이름 값 · --이름=값
        if at_word && (chars[i].is_ascii_alphabetic() || chars[i] == '_' || chars[i] == '-') {
            let start = i;
            let mut j = i;
            while j < chars.len() && is_name_char(chars[j]) {
                j += 1;
            }
            let name: String = chars[start..j].iter().collect();
            let bare = name.trim_start_matches('-');
            let flag = name.starts_with("--");
            let mut k = j;
            if k < chars.len() && (chars[k] == '"' || chars[k] == '\'') && start > 0 && chars[start - 1] == chars[k] {
                k += 1; // "이름" 의 닫는 따옴표
            }
            let sep = if k < chars.len() && chars[k] == '=' {
                Some(k + 1)
            } else if k < chars.len() && chars[k] == ':' {
                let mut s = k + 1;
                while s < chars.len() && chars[s] == ' ' {
                    s += 1;
                }
                Some(s)
            } else if flag && k < chars.len() && chars[k] == ' ' {
                Some(k + 1)
            } else {
                None
            };
            if let Some(v) = sep.filter(|_| secret_name(bare)) {
                // Authorization: Bearer xxx — 방식 낱말은 남기고 값만.
                let mut v = v;
                for scheme in ["Bearer ", "Basic ", "Token ", "bearer ", "basic ", "token "] {
                    let s: Vec<char> = scheme.chars().collect();
                    if chars.len() >= v + s.len() && chars[v..v + s.len()] == s[..] {
                        v += s.len();
                    }
                }
                let (from, to) = value_end(&chars, v);
                if to > from {
                    mark(from, to, &mut hide);
                }
                i = to.max(j);
                continue;
            }
            i = j.max(i + 1);
            continue;
        }
        i += 1;
    }
    // 머리 표식이 있는 토큰 · Bearer 뒤 · JWT · URL 의 비밀번호
    let joined: String = chars.iter().collect();
    for (ci, (byte, _)) in joined.char_indices().enumerate() {
        let rest = &joined[byte..];
        let word_start = ci == 0 || !is_name_char(chars[ci - 1]);
        if word_start {
            let token_len = chars[ci..]
                .iter()
                .take_while(|c| is_name_char(**c) || **c == '/' || **c == '+' || **c == '=')
                .count();
            let prefixed = TOKEN_PREFIXES.iter().any(|p| rest.starts_with(p)) && token_len >= 16;
            let jwt = rest.starts_with("eyJ")
                && token_len >= 24
                && chars[ci..ci + token_len].iter().filter(|c| **c == '.').count() >= 2;
            if prefixed || jwt {
                mark(ci, ci + token_len, &mut hide);
            }
        }
        if word_start && (rest.starts_with("Bearer ") || rest.starts_with("bearer ")) {
            let (from, to) = value_end(&chars, ci + 7);
            if to.saturating_sub(from) >= 8 {
                mark(from, to, &mut hide);
            }
        }
        if rest.starts_with("://") {
            // scheme://user:pass@host — pass 만.
            let s = ci + 3;
            let mut e = s;
            while e < chars.len() && !chars[e].is_whitespace() && !"/@\"'".contains(chars[e]) {
                e += 1;
            }
            if e < chars.len() && chars[e] == '@' {
                if let Some(colon) = (s..e).find(|&x| chars[x] == ':') {
                    if e > colon + 1 {
                        mark(colon + 1, e, &mut hide);
                    }
                }
            }
        }
    }
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        if hide[i] {
            out.push_str(MASK);
            while i < chars.len() && hide[i] {
                i += 1;
            }
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

fn mask_pem(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(begin) = rest.find("-----BEGIN ") {
        let Some(head_end) = rest[begin + 11..].find("-----").map(|e| begin + 11 + e + 5) else { break };
        let label = &rest[begin + 11..head_end - 5];
        if !label.contains("PRIVATE KEY") {
            out.push_str(&rest[..head_end]);
            rest = &rest[head_end..];
            continue;
        }
        let end_marker = format!("-----END {label}-----");
        out.push_str(&rest[..head_end]);
        match rest[head_end..].find(&end_marker) {
            Some(e) => {
                out.push('\n');
                out.push_str(MASK);
                out.push('\n');
                out.push_str(&end_marker);
                rest = &rest[head_end + e + end_marker.len()..];
            }
            None => {
                out.push('\n');
                out.push_str(MASK);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

/// 승인 요청의 지문 — 보인 것과 결정한 것이 같은지 견준다. 관문·데스크톱·폰이 같은 값을 내야 한다.
pub fn digest(parts: &[&str], fields: &[Field]) -> String {
    use sha2::Digest as _;
    let body = serde_json::json!({ "parts": parts, "fields": fields });
    sha2::Sha256::digest(body.to_string().as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinary_commands_pass_through_unchanged() {
        for cmd in [
            "cargo test -p kasa-mcp approvals",
            "git push origin HEAD:main",
            "cd /Users/kasa/Desktop && ls -la",
            "echo $PWD && export PATH=/usr/bin:$PATH",
            "rm -rf target/debug/kasaterm",
            "grep -n \"token_hash\" crates/kasa-mcp/src/relay_auth.rs",
        ] {
            assert_eq!(mask(cmd), cmd, "{cmd}");
        }
    }

    #[test]
    fn named_secrets_and_flags_are_hidden() {
        assert_eq!(mask("API_KEY=abc123 cargo run"), format!("API_KEY={MASK} cargo run"));
        assert_eq!(mask("export GITHUB_TOKEN='x y z'"), format!("export GITHUB_TOKEN='{MASK}'"));
        assert_eq!(mask("mysql --password=hunter2 db"), format!("mysql --password={MASK} db"));
        assert_eq!(mask("tool --api-key s3cr3t --verbose"), format!("tool --api-key {MASK} --verbose"));
        assert_eq!(mask("{\"client_secret\": \"abc\"}"), format!("{{\"client_secret\": \"{MASK}\"}}"));
        assert_eq!(
            mask("curl -H 'Authorization: Bearer abcdefghijkl' https://x"),
            format!("curl -H 'Authorization: Bearer {MASK}' https://x")
        );
    }

    #[test]
    fn prefixed_tokens_jwts_and_url_passwords_are_hidden() {
        assert_eq!(mask("use sk-ant-api03-AAAAAAAAAAAAAAAA now"), format!("use {MASK} now"));
        assert_eq!(mask("ghp_0123456789abcdefABCD"), MASK);
        assert_eq!(mask("t=eyJhbGciOiJ.eyJzdWIiOiIx.c2lnbmF0dXJl"), format!("t={MASK}"));
        assert_eq!(mask("git clone https://me:pa55@github.com/x"), format!("git clone https://me:{MASK}@github.com/x"));
        assert_eq!(mask("short sk-abc"), "short sk-abc");
    }

    #[test]
    fn private_key_blocks_are_hidden() {
        let pem = "a\n-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXk\n-----END OPENSSH PRIVATE KEY-----\nz";
        let out = mask(pem);
        assert!(!out.contains("b3BlbnNzaC1rZXk"));
        assert!(out.contains("-----END OPENSSH PRIVATE KEY-----"));
        assert!(out.ends_with("z"));
    }

    #[test]
    fn every_input_key_is_shown_and_long_input_is_marked_truncated() {
        let (f, cut) = fields("Bash", &serde_json::json!({"command":"ls","description":"목록","extra":"x"}));
        assert!(!cut);
        assert_eq!(f.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(), ["command", "description", "extra"]);
        assert_eq!(f[0].label, "명령");
        let big = "가".repeat(MAX_FIELD);
        let (f, cut) = fields("Write", &serde_json::json!({"file_path":"/a","content":big}));
        assert!(cut);
        assert!(f[1].text.len() <= MAX_FIELD);
    }
}
