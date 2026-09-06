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

use std::sync::{Mutex, OnceLock};

/// 들고 있을 개수. 하단바 팝오버가 한눈에 담을 만큼만 — 더 쌓으면 고르는 일이
/// 붙여넣기보다 오래 걸린다.
pub(crate) const CAP: usize = 20;

/// 한 칸이 삼킬 최대 글자. 화면 한 판을 복사하면 수천 자가 오는데, 그걸 통째로 여러 벌
/// 들고 있을 이유가 없다 — 목록은 **고르는 자리**지 보관함이 아니다. 자른 것은 다시
/// 복사할 때도 잘린 채 나가므로, 자르는 길이를 넉넉히 둔다.
const MAX_CHARS: usize = 4000;

fn store() -> &'static Mutex<Vec<String>> {
    static S: OnceLock<Mutex<Vec<String>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(Vec::new()))
}

/// 폴링이 「이건 이미 봤다」를 가리는 기준. 마지막으로 목록에 담긴 값이다 — 사람이
/// 같은 것을 두 번 복사해도 목록이 늘지 않는다.
fn last_seen() -> &'static Mutex<String> {
    static S: OnceLock<Mutex<String>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(String::new()))
}

/// 목록에 담는다. 이미 있는 값이면 **지우고 맨 앞으로** 올린다 — 같은 것을 다시
/// 복사하면 그건 지금 쓰는 것이라, 목록 아래에 묻혀 있으면 안 된다.
pub(crate) fn remember(text: &str) {
    let text = text.trim_end_matches(['\n', '\r']);
    if text.trim().is_empty() {
        return;
    }
    let text: String = if text.chars().count() > MAX_CHARS {
        text.chars().take(MAX_CHARS).collect()
    } else {
        text.to_string()
    };
    *last_seen().lock().unwrap() = text.clone();
    let mut v = store().lock().unwrap();
    v.retain(|t| t != &text);
    v.insert(0, text);
    v.truncate(CAP);
}

/// 지금 목록. 최근 것이 앞이다.
pub(crate) fn history() -> Vec<String> {
    store().lock().unwrap().clone()
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

/// 목록의 한 칸을 다시 클립보드로. 성공하면 그 글을 돌려준다(부른 쪽이 띄울 수 있게).
pub(crate) fn pick(idx: usize) -> Option<String> {
    let text = store().lock().unwrap().get(idx).cloned()?;
    let mut cb = arboard::Clipboard::new().ok()?;
    cb.set_text(text.clone()).ok()?;
    // 고른 것이 맨 앞으로 올라온다 — 방금 쓴 것이 목록 아래에 있으면 다음에 또 찾아야
    // 한다. `remember` 가 last_seen 도 갱신하므로 폴링이 이것을 새 복사로 또 담지 않는다.
    remember(&text);
    Some(text)
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 같은 것을 다시 복사하면 목록이 늘지 않고 **맨 앞으로 올라온다** — 지금 쓰는
    /// 것이 아래에 묻히면 다음에 또 찾아야 한다.
    #[test]
    fn re_copying_moves_the_entry_to_the_front_without_growing() {
        store().lock().unwrap().clear();
        remember("가");
        remember("나");
        remember("가");
        assert_eq!(history(), vec!["가".to_string(), "나".to_string()]);
    }

    /// 빈 글과 공백뿐인 글은 안 담는다 — 클립보드가 잠깐 비는 순간이 목록을 채우면
    /// 정작 찾던 것이 상한에 밀려 빠진다.
    #[test]
    fn blank_copies_never_enter_the_list() {
        store().lock().unwrap().clear();
        remember("");
        remember("   \n ");
        assert!(history().is_empty());
    }

    /// 상한을 넘으면 오래된 것부터 빠진다.
    #[test]
    fn the_list_stops_at_the_cap() {
        store().lock().unwrap().clear();
        for i in 0..(CAP + 5) {
            remember(&format!("항목{i}"));
        }
        let h = history();
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
}
