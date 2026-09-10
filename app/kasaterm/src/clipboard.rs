//! 클립보드 이력 — 하단바가 펼쳐 보이는 「최근에 복사한 것들」.
//!
//! 클립보드는 **한 칸짜리 그릇**이라 다음 복사가 앞의 것을 지운다. 방금 복사한 것을
//! 붙여넣기 전에 다른 것을 복사하면 앞의 것은 되찾을 길이 없고, 그 사고는 조용하다 —
//! 붙여넣어 봐야 안다. 그래서 지나간 것을 앱이 대신 들고 있다가 하단바에서 골라 다시
//! 복사하게 한다(2026-09-06 지시: 「하단바에 만들자 클립보드기능 — 최근 복사한 것들
//! 목록」).
//!
//! **담는 것은 사람의 복사도 포함한다.** 캐릭터가 `kasaterm-cli copy` 로 넣은 것만
//! 쌓으면 정작 사람이 Cmd+C 한 것이 목록에 없어, 「최근 복사한 것」이라는 이름이
//! 거짓이 된다. 그래서 값을 밀어 넣는 문(`remember`)과 별개로, 틱에서 클립보드를
//! 들여다보다 바뀌었으면 그것도 같은 목록에 담는다(`poll`).
//!
//! **디스크에 안 남긴다.** 클립보드에는 비밀번호·토큰이 지나간다 — 앱이 그것을 파일로
//! 옮겨 두면 사람이 지운 뒤에도 남는다. 앱이 도는 동안만 기억하고 끄면 잊는다.
//!
//! **비밀값은 가린다.** 토큰·키처럼 생긴 것(`looks_secret`)이나 `copy --secret` 으로
//! 들어온 것은 목록·칩·폰에 앞머리를 안 보이고 꼬리 넉 자만 보인다. 캐릭터는
//! `kasaterm-cli paste --into`·`--env` 로 값을 **안 보고** 쓴다(2026-09-10 지시 「env
//! 키 같은 것도 복사해서 클립보드에 있어 하면 안전하게 쓸 수 있게」) — usemap CLI 의
//! `--clipboard` 와 같은 생각이다: 값이 대화에 한 번 찍히면 되돌릴 수 없다.

use std::sync::{Mutex, OnceLock};

/// 들고 있을 개수. 하단바 팝오버가 한눈에 담을 만큼만 — 더 쌓으면 고르는 일이
/// 붙여넣기보다 오래 걸린다.
pub(crate) const CAP: usize = 20;

/// 한 칸이 삼킬 최대 글자. 화면 한 판을 복사하면 수천 자가 오는데, 그걸 통째로 여러 벌
/// 들고 있을 이유가 없다 — 목록은 **고르는 자리**지 보관함이 아니다. 자른 것은 다시
/// 복사할 때도 잘린 채 나가므로, 자르는 길이를 넉넉히 둔다.
const MAX_CHARS: usize = 4000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Item {
    /// 앱이 도는 동안만 유일하다 — 폰이 「그것」을 집어 갈 손잡이.
    pub id: u64,
    pub text: String,
    pub secret: bool,
    pub when: u64,
}

fn store() -> &'static Mutex<Vec<Item>> {
    static S: OnceLock<Mutex<Vec<Item>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(Vec::new()))
}

fn next_id() -> u64 {
    static N: Mutex<u64> = Mutex::new(0);
    let mut n = N.lock().unwrap();
    *n += 1;
    *n
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 폴링이 「이건 이미 봤다」를 가리는 기준. 마지막으로 목록에 담긴 값이다 — 사람이
/// 같은 것을 두 번 복사해도 목록이 늘지 않는다.
fn last_seen() -> &'static Mutex<String> {
    static S: OnceLock<Mutex<String>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(String::new()))
}

/// 토큰·키처럼 생겼나 — 한 덩어리(공백 없음)에 글자와 숫자가 섞여 길거나, 알려진
/// 머리(`sk-`·`ghp_`·`xox`·`AKIA`·JWT·PEM)로 시작한다. 놓치는 쪽보다 과하게 잡는
/// 쪽이 싸다 — 가려진 것은 `paste --show` 로 풀 수 있지만 찍힌 것은 못 지운다.
pub(crate) fn looks_secret(text: &str) -> bool {
    let t = text.trim();
    if t.is_empty() {
        return false;
    }
    if t.starts_with("-----BEGIN") {
        return true;
    }
    let token = match t.split_once('=') {
        // `KEY=value` 한 줄은 값 쪽을 본다.
        Some((k, v)) if !k.contains(char::is_whitespace) && !v.trim().is_empty() => v.trim(),
        _ => t,
    };
    if token.contains(char::is_whitespace) {
        return false;
    }
    const HEADS: [&str; 10] = [
        "sk-", "ghp_", "gho_", "github_pat_", "xox", "AKIA", "ya29.", "eyJ", "AIza", "glpat-",
    ];
    if HEADS.iter().any(|h| token.starts_with(h)) {
        return true;
    }
    let n = token.chars().count();
    if n < 20 || n > 512 {
        return false;
    }
    // 주소·경로는 길어도 비밀이 아니다.
    if token.contains("://") || token.starts_with('/') || token.starts_with('~') {
        return false;
    }
    let letters = token.chars().filter(|c| c.is_ascii_alphabetic()).count();
    let digits = token.chars().filter(|c| c.is_ascii_digit()).count();
    let plain = token.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '+' | '/' | '='));
    plain && letters >= 4 && digits >= 3
}

/// 목록에 담는다 — 비밀 여부는 생김새로 정한다.
pub(crate) fn remember(text: &str) {
    remember_as(text, None);
}

/// 목록에 담는다. 이미 있는 값이면 **지우고 맨 앞으로** 올린다 — 같은 것을 다시
/// 복사하면 그건 지금 쓰는 것이라, 목록 아래에 묻혀 있으면 안 된다. `secret` 이
/// `None` 이면 생김새로 판정하고, 한 번 비밀이었던 것은 다시 담겨도 비밀로 남는다.
pub(crate) fn remember_as(text: &str, secret: Option<bool>) -> Option<Item> {
    let text = text.trim_end_matches(['\n', '\r']);
    if text.trim().is_empty() {
        return None;
    }
    let text: String = if text.chars().count() > MAX_CHARS {
        text.chars().take(MAX_CHARS).collect()
    } else {
        text.to_string()
    };
    *last_seen().lock().unwrap() = text.clone();
    let mut v = store().lock().unwrap();
    let was_secret = v.iter().any(|i| i.text == text && i.secret);
    v.retain(|i| i.text != text);
    let item = Item {
        id: next_id(),
        secret: secret.unwrap_or_else(|| looks_secret(&text)) || was_secret,
        text,
        when: now_secs(),
    };
    v.insert(0, item.clone());
    v.truncate(CAP);
    Some(item)
}

/// 지금 목록. 최근 것이 앞이다.
pub(crate) fn history() -> Vec<Item> {
    store().lock().unwrap().clone()
}

pub(crate) fn get(id: u64) -> Option<Item> {
    store().lock().unwrap().iter().find(|i| i.id == id).cloned()
}

/// 폰·CLI 가 받는 목록 — 본문은 안 싣고 미리보기만(비밀은 가린 채).
pub(crate) fn json_list() -> Vec<serde_json::Value> {
    history()
        .iter()
        .map(|i| {
            serde_json::json!({
                "id": i.id,
                "preview": preview_item(i, 80),
                "secret": i.secret,
                "chars": i.text.chars().count(),
                "when": i.when,
            })
        })
        .collect()
}

/// 클립보드를 들여다보고 바뀌었으면 담는다. 틱에서 부른다.
///
/// 반환은 「목록이 늘었나」 — 부른 쪽이 화면을 다시 그릴지 정하는 데 쓴다. 매 틱
/// 다시 그리면 노는 화면이 계속 깨어난다.
pub(crate) fn poll() -> bool {
    let Ok(mut cb) = arboard::Clipboard::new() else {
        return false;
    };
    // 글이 없는 클립보드(그림만 있거나 빈 것)는 조용히 넘긴다 — 오류가 아니다.
    let Ok(text) = cb.get_text() else {
        return false;
    };
    if text.trim().is_empty() || *last_seen().lock().unwrap() == text {
        return false;
    }
    remember(&text);
    true
}

fn set_system(text: &str) -> Option<()> {
    let mut cb = arboard::Clipboard::new().ok()?;
    cb.set_text(text.to_string()).ok()
}

/// 목록의 한 칸을 다시 클립보드로. 성공하면 그 글을 돌려준다(부른 쪽이 띄울 수 있게).
pub(crate) fn pick(idx: usize) -> Option<String> {
    let item = store().lock().unwrap().get(idx).cloned()?;
    set_system(&item.text)?;
    // 고른 것이 맨 앞으로 올라온다 — 방금 쓴 것이 목록 아래에 있으면 다음에 또 찾아야
    // 한다. `remember` 가 last_seen 도 갱신하므로 폴링이 이것을 새 복사로 또 담지 않는다.
    remember_as(&item.text, Some(item.secret));
    Some(item.text)
}

/// id 로 고른다 — 폰이 「그것을 PC 클립보드로」 할 때.
pub(crate) fn pick_id(id: u64) -> Option<Item> {
    let item = get(id)?;
    set_system(&item.text)?;
    remember_as(&item.text, Some(item.secret))
}

/// 지금 클립보드(맨 앞)가 비밀인가 — `paste` 가 값을 찍기 전에 묻는다.
pub(crate) fn current_is_secret(text: &str) -> bool {
    history().first().is_some_and(|i| i.text == text && i.secret) || looks_secret(text)
}

/// 한 줄로 눕힌 미리보기 — 하단바 칩과 팝오버 줄이 함께 쓴다. 줄바꿈·연속 공백을
/// 한 칸으로 접는다: 목록은 내용을 **알아보는** 자리지 읽는 자리가 아니고, 접지 않으면
/// 화면 한 판을 복사한 칸이 목록을 통째로 밀어낸다.
pub(crate) fn preview(text: &str, max: usize) -> String {
    let flat: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        return flat;
    }
    let head: String = flat.chars().take(max.saturating_sub(1)).collect();
    format!("{head}…")
}

/// 비밀은 꼬리 넉 자만 — 「어느 키인지」는 알아보되 값은 안 새게.
pub(crate) fn masked(text: &str) -> String {
    let flat: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let n = flat.chars().count();
    if n <= 8 {
        return "••••••".to_string();
    }
    let tail: String = flat.chars().skip(n - 4).collect();
    format!("••••{tail}")
}

pub(crate) fn preview_item(item: &Item, max: usize) -> String {
    if item.secret {
        format!("비밀 · {}", masked(&item.text))
    } else {
        preview(&item.text, max)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 목록은 전역 하나라 테스트끼리 겹치면 서로의 항목을 본다 — 한 번에 하나만.
    fn lock() -> std::sync::MutexGuard<'static, ()> {
        static L: Mutex<()> = Mutex::new(());
        let g = L.lock().unwrap_or_else(|e| e.into_inner());
        store().lock().unwrap().clear();
        g
    }

    fn texts() -> Vec<String> {
        history().into_iter().map(|i| i.text).collect()
    }

    /// 같은 것을 다시 복사하면 목록이 늘지 않고 **맨 앞으로 올라온다** — 지금 쓰는
    /// 것이 아래에 묻히면 다음에 또 찾아야 한다.
    #[test]
    fn re_copying_moves_the_entry_to_the_front_without_growing() {
        let _g = lock();
        remember("가");
        remember("나");
        remember("가");
        assert_eq!(texts(), vec!["가".to_string(), "나".to_string()]);
    }

    /// 빈 글과 공백뿐인 글은 안 담는다 — 클립보드가 잠깐 비는 순간이 목록을 채우면
    /// 정작 찾던 것이 상한에 밀려 빠진다.
    #[test]
    fn blank_copies_never_enter_the_list() {
        let _g = lock();
        remember("");
        remember("   \n ");
        assert!(history().is_empty());
    }

    /// 상한을 넘으면 오래된 것부터 빠진다.
    #[test]
    fn the_list_stops_at_the_cap() {
        let _g = lock();
        for i in 0..(CAP + 5) {
            remember(&format!("항목{i}"));
        }
        let h = texts();
        assert_eq!(h.len(), CAP);
        assert_eq!(h[0], format!("항목{}", CAP + 4), "최근 것이 앞");
    }

    /// 미리보기는 줄바꿈을 눕히고 길면 자른다 — 화면 한 판을 복사한 칸이 목록을
    /// 밀어내지 않게.
    #[test]
    fn preview_flattens_and_truncates() {
        assert_eq!(preview("한 줄\n둘째 줄", 20), "한 줄 둘째 줄");
        assert_eq!(preview("가나다라마바사", 4), "가나다…");
        // 꼬리 줄바꿈만 있는 글은 자를 것이 없다.
        assert_eq!(preview("짧다\n", 20), "짧다");
    }

    /// 토큰처럼 생긴 것은 비밀로 잡고, 문장·주소·짧은 낱말은 아니다.
    #[test]
    fn secrets_are_recognised_by_shape() {
        assert!(looks_secret("sk-ant-api03-abcdefghijklmnop"));
        assert!(looks_secret("OPENAI_KEY=abcdefghij1234567890KLMNOP"));
        assert!(looks_secret("eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.abc"));
        assert!(looks_secret("A1b2C3d4E5f6G7h8I9j0K1l2"));
        assert!(!looks_secret("https://example.com/a/very/long/path/with/digits/123456"));
        assert!(!looks_secret("이건 그냥 문장이다 1234567890 abcdefg"));
        assert!(!looks_secret("git commit -m 'fix'"));
        assert!(!looks_secret("/Users/kasa/Desktop/momewomo/kasaterm/app/src/main.rs"));
    }

    /// 비밀은 목록에서 꼬리만 보이고, 한 번 비밀이면 다시 담겨도 비밀이다.
    #[test]
    fn secret_items_stay_masked() {
        let _g = lock();
        let item = remember_as("plain-looking-value", Some(true)).unwrap();
        assert!(item.secret);
        assert_eq!(preview_item(&item, 40), "비밀 · ••••alue");
        let again = remember_as("plain-looking-value", None).unwrap();
        assert!(again.secret, "한 번 비밀이면 계속 비밀");
        assert_eq!(history().len(), 1);
        assert_eq!(masked("short"), "••••••");
    }
}
