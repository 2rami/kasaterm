//! 한 줄 입력칸의 편집 조작 — 커서 있는 삽입·삭제·이동.
//!
//! 앱에는 한 줄짜리 입력칸이 여럿 있다(방 이름 · 파일트리 이름짓기 · 경로 검색 ·
//! git 커밋 메시지). 편집기(`markdown.rs`)는 진짜 커서를 갖는데, 이
//! 작은 칸들은 각자 `push_str`/`pop` 만 갖고 있어 **끝에만 붙고 끝에서만 지워졌다** —
//! 가운데 오타 하나에 뒤를 다 지워야 했다(거노 2026-08-07: "편집기는 잘되려나 그걸
//! 붙이면 좋을텐데").
//!
//! 칸마다 다시 짜지 않도록 조작을 여기 한 벌만 둔다. 각 칸은 자기 버퍼(`String`)와
//! 커서(문자 단위 인덱스)를 넘기고, Enter·Esc 처럼 칸마다 뜻이 다른 키만
//! [`LineEditAction`] 으로 돌려받아 스스로 처리한다.
//!
//! 커서는 **문자 단위**다. 바이트로 두면 한글에서 커서가 글자 가운데로 들어간다.

use winit::keyboard::{Key, NamedKey};

/// 키 하나를 넣은 결과.
///
/// `Edited` 와 `Moved` 를 가르는 이유: 버퍼가 실제로 바뀌었을 때만 해야 하는 일이 있다
/// (검색칸은 트리를 다시 훑고 스크롤을 0 으로 되돌린다). 커서만 옮겼는데 그걸 돌리면
/// ←→ 를 누를 때마다 화면이 튄다. 부르는 쪽이 길이를 앞뒤로 재서 짐작하는 건 같은
/// 길이로 바뀌는 편집이 생기는 순간 조용히 틀린다.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub(crate) enum LineEditAction {
    /// 버퍼 내용이 바뀌었다.
    Edited,
    /// 커서만 움직였다 — 내용은 그대로.
    Moved,
    /// Enter — 칸이 확정 동작을 해야 한다.
    Submit,
    /// Esc — 칸이 취소 동작을 해야 한다.
    Cancel,
    /// 이 칸이 다룰 키가 아니다.
    Ignored,
}

/// 커서 자리에 글자를 넣고 커서를 그만큼 민다. 조합이 끝난 한글도 여기로 온다.
pub(crate) fn insert(text: &mut String, cursor: &mut usize, s: &str) {
    if s.is_empty() {
        return;
    }
    let len = text.chars().count();
    let col = (*cursor).min(len);
    let at = crate::char_byte(text, col);
    text.insert_str(at, s);
    *cursor = col + s.chars().count();
}

/// 커서 왼쪽 한 글자를 지운다. 지웠으면 true(맨 앞이면 false — 부르는 쪽이 그때
/// 편집을 닫거나 다른 일을 할 수 있게).
pub(crate) fn backspace(text: &mut String, cursor: &mut usize) -> bool {
    let len = text.chars().count();
    let col = (*cursor).min(len);
    if col == 0 {
        return false;
    }
    let b0 = crate::char_byte(text, col - 1);
    let b1 = crate::char_byte(text, col);
    text.replace_range(b0..b1, "");
    *cursor = col - 1;
    true
}

/// 편집 키 하나를 처리한다. 글자 입력(`Key::Character`)과 Space 도 여기서 받는다 —
/// **한글 조합을 먼저 흘려보낸 뒤에** 부를 것(조합 중인 자모까지 여기로 오면 안 된다).
pub(crate) fn key(text: &mut String, cursor: &mut usize, k: &Key) -> LineEditAction {
    let len = text.chars().count();
    let col = (*cursor).min(len);
    *cursor = col;
    match k {
        Key::Named(NamedKey::Enter) => return LineEditAction::Submit,
        Key::Named(NamedKey::Escape) => return LineEditAction::Cancel,
        Key::Named(NamedKey::Backspace) => {
            // 맨 앞에서 누른 백스페이스는 아무것도 안 바꾼다.
            return if backspace(text, cursor) {
                LineEditAction::Edited
            } else {
                LineEditAction::Moved
            };
        }
        Key::Named(NamedKey::Delete) => {
            if col >= len {
                return LineEditAction::Moved;
            }
            let b0 = crate::char_byte(text, col);
            let b1 = crate::char_byte(text, col + 1);
            text.replace_range(b0..b1, "");
        }
        Key::Named(NamedKey::ArrowLeft) => {
            *cursor = col.saturating_sub(1);
            return LineEditAction::Moved;
        }
        Key::Named(NamedKey::ArrowRight) => {
            *cursor = (col + 1).min(len);
            return LineEditAction::Moved;
        }
        Key::Named(NamedKey::Home) => {
            *cursor = 0;
            return LineEditAction::Moved;
        }
        Key::Named(NamedKey::End) => {
            *cursor = len;
            return LineEditAction::Moved;
        }
        Key::Named(NamedKey::Space) => insert(text, cursor, " "),
        Key::Character(c) => insert(text, cursor, c),
        _ => return LineEditAction::Ignored,
    }
    LineEditAction::Edited
}

/// 커서 앞/뒤로 가른 조각. 렌더가 「앞 → (조합 중 글자) → 뒤」 순으로 그리고 앞의
/// 끝에 캐럿을 세우면 커서가 가운데 있어도 자리가 맞는다.
pub(crate) fn split(text: &str, cursor: usize) -> (String, String) {
    let col = cursor.min(text.chars().count());
    let at = crate::char_byte(text, col);
    (text[..at].to_string(), text[at..].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inserts_at_the_cursor_not_the_end() {
        let (mut t, mut c) = ("가나".to_string(), 1);
        insert(&mut t, &mut c, "X");
        assert_eq!((t.as_str(), c), ("가X나", 2));
    }

    #[test]
    fn backspace_eats_one_hangul_syllable() {
        // 바이트로 세면 한글에서 커서가 글자 가운데로 들어간다.
        let (mut t, mut c) = ("한글".to_string(), 2);
        assert!(backspace(&mut t, &mut c));
        assert_eq!((t.as_str(), c), ("한", 1));
        assert!(backspace(&mut t, &mut c));
        assert_eq!((t.as_str(), c), ("", 0));
        assert!(!backspace(&mut t, &mut c));
    }

    #[test]
    fn delete_eats_forward_and_leaves_the_cursor() {
        let (mut t, mut c) = ("abc".to_string(), 1);
        assert_eq!(key(&mut t, &mut c, &Key::Named(NamedKey::Delete)), LineEditAction::Edited);
        assert_eq!((t.as_str(), c), ("ac", 1));
    }

    #[test]
    fn arrows_and_home_end_clamp() {
        let (mut t, mut c) = ("가나다".to_string(), 0);
        key(&mut t, &mut c, &Key::Named(NamedKey::ArrowLeft));
        assert_eq!(c, 0);
        key(&mut t, &mut c, &Key::Named(NamedKey::End));
        assert_eq!(c, 3);
        key(&mut t, &mut c, &Key::Named(NamedKey::ArrowRight));
        assert_eq!(c, 3);
        key(&mut t, &mut c, &Key::Named(NamedKey::Home));
        assert_eq!(c, 0);
    }

    #[test]
    fn moves_are_not_edits() {
        // 부르는 쪽이 「바뀌었나」로 무거운 일(트리 재수집·스크롤 리셋)을 건다.
        // 커서만 옮긴 키가 Edited 로 새면 ←→ 를 누를 때마다 화면이 튄다.
        let (mut t, mut c) = ("가나".to_string(), 1);
        for k in [NamedKey::ArrowLeft, NamedKey::ArrowRight, NamedKey::Home, NamedKey::End] {
            assert_eq!(key(&mut t, &mut c, &Key::Named(k)), LineEditAction::Moved, "{k:?}");
        }
        // 경계에서 아무것도 못 지운 것도 편집이 아니다.
        c = 0;
        assert_eq!(key(&mut t, &mut c, &Key::Named(NamedKey::Backspace)), LineEditAction::Moved);
        c = 2;
        assert_eq!(key(&mut t, &mut c, &Key::Named(NamedKey::Delete)), LineEditAction::Moved);
        assert_eq!(t, "가나");
        // 실제로 지운 것은 편집이다.
        assert_eq!(key(&mut t, &mut c, &Key::Named(NamedKey::Backspace)), LineEditAction::Edited);
        assert_eq!(key(&mut t, &mut c, &Key::Character("z".into())), LineEditAction::Edited);
        assert_eq!(t, "가z");
    }

    #[test]
    fn enter_and_escape_hand_back_to_the_field() {
        let (mut t, mut c) = (String::new(), 0);
        assert_eq!(key(&mut t, &mut c, &Key::Named(NamedKey::Enter)), LineEditAction::Submit);
        assert_eq!(key(&mut t, &mut c, &Key::Named(NamedKey::Escape)), LineEditAction::Cancel);
        assert_eq!(key(&mut t, &mut c, &Key::Named(NamedKey::Tab)), LineEditAction::Ignored);
    }

    #[test]
    fn stale_cursor_past_the_end_is_clamped_not_panicking() {
        // 버퍼가 밖에서 짧아진 뒤(되돌리기·붙여넣기) 커서만 남는 경우.
        let (mut t, mut c) = ("ab".to_string(), 99);
        insert(&mut t, &mut c, "!");
        assert_eq!((t.as_str(), c), ("ab!", 3));
    }

    #[test]
    fn split_cuts_on_char_boundaries() {
        assert_eq!(split("한글x", 1), ("한".to_string(), "글x".to_string()));
        assert_eq!(split("한글x", 99), ("한글x".to_string(), String::new()));
    }
}
