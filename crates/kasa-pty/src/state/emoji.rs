//! 이모지 묶음을 받는 프로그램이 센 칸 수에 맞춘다.
//!
//! claude 는 `⚠️`·`👍🏻`·`👨‍👩‍👧` 를 글자 묶음 하나, 2칸으로 세고 다음 글자를 그 뒤 칸에 직접 놓는다. alacritty 는
//! 코드포인트마다 폭을 세어 `⚠`+VS16 은 1칸, `👍`+피부색은 4칸, 가족은 6칸이 된다. 그 차이만큼 claude 가 놓은
//! 글자가 피부색·가족 얼굴을 덮어 지우거나, 덮이지 않은 칸에 지난 화면 글자가 남았다(2026-11-18 리그 실측).
//! 파서에 넣기 전에 묶음을 2칸 하나로 줄인다 — 붙는 조각(피부색·ZWJ 와 그 뒤 얼굴)은 버리고, 1칸 바탕이 묶음이
//! 되면 빈칸 하나를 덧붙인다. 칸 데이터는 글자 하나만 싣기 때문에(`convert_cell`) 버린 조각은 원래도 안 그려졌다.
//! 결과는 다시 넣어도 그대로라 거울 쪽 파서가 한 번 더 거쳐도 칸이 같다.

const VS16: char = '\u{FE0F}';
const ZWJ: char = '\u{200D}';

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Vt { Ground, Esc, Csi, Str, StrEsc }

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Prev {
    /// 묶음이 될 수 없는 자리 — 줄 처음, 제어·이스케이프 뒤, 이모지가 아닌 글자 뒤.
    None,
    /// 이모지 바탕. `wide` 는 지금 2칸을 차지하는가.
    Emoji { wide: bool },
    /// 이모지 뒤 ZWJ 를 버렸다 — 다음 이모지는 이 묶음에 붙는다.
    Joining { wide: bool },
}

pub(super) struct EmojiClusters { vt: Vt, prev: Prev }

impl Default for EmojiClusters {
    fn default() -> Self { Self { vt: Vt::Ground, prev: Prev::None } }
}

impl EmojiClusters {
    /// 바뀐 것이 없으면 빌린 그대로 돌려준다. 입력은 UTF-8 이 이어 붙은 배치다(`Utf8Buffer`).
    pub(super) fn process<'a>(&mut self, bytes: &'a [u8]) -> std::borrow::Cow<'a, [u8]> {
        let mut out: Option<Vec<u8>> = None;
        let mut i = 0;
        while i < bytes.len() {
            let b = bytes[i];
            if self.vt == Vt::Ground && (0x20..0x7F).contains(&b) {
                self.step_char(b as char);
                if let Some(out) = out.as_mut() { out.push(b); }
                i += 1;
                continue;
            }
            if self.vt != Vt::Ground || b < 0x80 {
                self.step_byte(b);
                if let Some(out) = out.as_mut() { out.push(b); }
                i += 1;
                continue;
            }
            let len = match b { 0xC0..=0xDF => 2, 0xE0..=0xEF => 3, 0xF0..=0xF7 => 4, _ => 1 };
            let Some(c) = bytes.get(i..i + len).and_then(|s| std::str::from_utf8(s).ok()).and_then(|s| s.chars().next()) else {
                self.prev = Prev::None;
                if let Some(out) = out.as_mut() { out.push(b); }
                i += 1;
                continue;
            };
            let (keep, pad) = self.step_char(c);
            if (!keep || pad) && out.is_none() { out = Some(bytes[..i].to_vec()); }
            if let Some(out) = out.as_mut() {
                if keep { out.extend_from_slice(&bytes[i..i + len]); }
                if pad { out.push(b' '); }
            }
            i += len;
        }
        out.map_or(std::borrow::Cow::Borrowed(bytes), std::borrow::Cow::Owned)
    }

    fn step_byte(&mut self, b: u8) {
        self.vt = match (self.vt, b) {
            (Vt::Ground, 0x1B) => Vt::Esc,
            (Vt::Ground, _) => { self.prev = Prev::None; Vt::Ground }
            (Vt::Esc, b'[') => Vt::Csi,
            (Vt::Esc, b']' | b'P' | b'_' | b'^' | b'X') => Vt::Str,
            (Vt::Esc, 0x20..=0x2F) => Vt::Esc,
            (Vt::Esc, _) => Vt::Ground,
            (Vt::Csi, 0x40..=0x7E) => Vt::Ground,
            (Vt::Csi, 0x1B) => Vt::Esc,
            (Vt::Csi, _) => Vt::Csi,
            (Vt::Str, 0x07) => Vt::Ground,
            (Vt::Str, 0x1B) => Vt::StrEsc,
            (Vt::Str, _) => Vt::Str,
            (Vt::StrEsc, b'\\') => Vt::Ground,
            (Vt::StrEsc, 0x1B) => Vt::StrEsc,
            (Vt::StrEsc, _) => Vt::Str,
        };
        if self.vt != Vt::Ground { self.prev = Prev::None; }
    }

    /// (이 글자를 남기나, 뒤에 빈칸을 덧붙이나).
    fn step_char(&mut self, c: char) -> (bool, bool) {
        match (self.prev, c) {
            (Prev::Emoji { wide }, VS16) => {
                self.prev = Prev::Emoji { wide: true };
                (false, !wide)
            }
            (Prev::Emoji { wide }, '\u{1F3FB}'..='\u{1F3FF}') => {
                self.prev = Prev::Emoji { wide: true };
                (false, !wide)
            }
            (Prev::Emoji { wide }, ZWJ) => {
                self.prev = Prev::Joining { wide };
                (false, false)
            }
            (Prev::Joining { wide }, c) if is_emoji(c) => {
                self.prev = Prev::Emoji { wide: true };
                (false, !wide)
            }
            (_, c) => {
                self.prev = if is_emoji(c) { Prev::Emoji { wide: is_wide(c) } }
                    else if is_keycap_base(c) { Prev::Emoji { wide: false } }
                    else { Prev::None };
                (true, false)
            }
        }
    }
}

/// 숫자·`#`·`*` 는 VS16 이 붙어야 키캡 이모지다. 피부색·ZWJ 가 붙는 일은 없지만 VS16 만은 받는다.
fn is_keycap_base(c: char) -> bool { c.is_ascii_digit() || c == '#' || c == '*' }

/// 터미널이 처음부터 2칸으로 두는 이모지 — 그림 표시가 기본인 것들.
fn is_wide(c: char) -> bool {
    matches!(c as u32, 0x231A..=0x231B | 0x23E9..=0x23EC | 0x23F0 | 0x23F3 | 0x25FD..=0x25FE
        | 0x2614..=0x2615 | 0x2648..=0x2653 | 0x267F | 0x2693 | 0x26A1 | 0x26AA..=0x26AB | 0x26BD..=0x26BE
        | 0x26C4..=0x26C5 | 0x26CE | 0x26D4 | 0x26EA | 0x26F2..=0x26F3 | 0x26F5 | 0x26FA | 0x26FD
        | 0x2705 | 0x270A..=0x270B | 0x2728 | 0x274C | 0x274E | 0x2753..=0x2755 | 0x2757 | 0x2795..=0x2797
        | 0x27B0 | 0x27BF | 0x2B1B..=0x2B1C | 0x2B50 | 0x2B55)
        || (0x1F000..=0x1FAFF).contains(&(c as u32)) && !(0x1F1E6..=0x1F1FF).contains(&(c as u32)) && !is_text_default_smp(c)
}

/// 보조 평면에서 글자 표시가 기본이라 터미널이 1칸으로 두는 것들(Emoji_Presentation 이 아닌 이모지).
fn is_text_default_smp(c: char) -> bool {
    matches!(c as u32, 0x1F170..=0x1F171 | 0x1F17E..=0x1F17F | 0x1F202 | 0x1F237 | 0x1F321 | 0x1F324..=0x1F32C
        | 0x1F336 | 0x1F37D | 0x1F396..=0x1F397 | 0x1F399..=0x1F39B | 0x1F39E..=0x1F39F | 0x1F3CB..=0x1F3CE
        | 0x1F3D4..=0x1F3DF | 0x1F3F3 | 0x1F3F5 | 0x1F3F7 | 0x1F43F | 0x1F441 | 0x1F4FD | 0x1F549..=0x1F54A
        | 0x1F56F..=0x1F570 | 0x1F573..=0x1F579 | 0x1F587 | 0x1F58A..=0x1F58D | 0x1F590 | 0x1F5A5 | 0x1F5A8
        | 0x1F5B1..=0x1F5B2 | 0x1F5BC | 0x1F5C2..=0x1F5C4 | 0x1F5D1..=0x1F5D3 | 0x1F5DC..=0x1F5DE | 0x1F5E1
        | 0x1F5E3 | 0x1F5E8 | 0x1F5EF | 0x1F5F3 | 0x1F5FA | 0x1F6CB | 0x1F6CD..=0x1F6CF | 0x1F6E0..=0x1F6E5
        | 0x1F6E9 | 0x1F6F0 | 0x1F6F3)
}

/// 이모지 속성이 있는 글자 — 바탕이 되어 VS16·피부색·ZWJ 를 받을 수 있다.
fn is_emoji(c: char) -> bool {
    is_wide(c) || is_text_default_smp(c) || matches!(c as u32, 0x00A9 | 0x00AE | 0x203C | 0x2049 | 0x2122 | 0x2139
        | 0x2194..=0x2199 | 0x21A9..=0x21AA | 0x2328 | 0x23CF | 0x23ED..=0x23EF | 0x23F1..=0x23F2 | 0x23F8..=0x23FA
        | 0x24C2 | 0x25AA..=0x25AB | 0x25B6 | 0x25C0 | 0x25FB..=0x25FC | 0x2600..=0x2604 | 0x260E | 0x2611
        | 0x2618 | 0x261D | 0x2620 | 0x2622..=0x2623 | 0x2626 | 0x262A | 0x262E..=0x262F | 0x2638..=0x263A
        | 0x2640 | 0x2642 | 0x265F..=0x2660 | 0x2663 | 0x2665..=0x2666 | 0x2668 | 0x267B | 0x267E | 0x2692
        | 0x2694..=0x2697 | 0x2699 | 0x269B..=0x269C | 0x26A0 | 0x26A7 | 0x26B0..=0x26B1 | 0x26C8 | 0x26CF
        | 0x26D1 | 0x26D3 | 0x26E9 | 0x26F0..=0x26F1 | 0x26F4 | 0x26F7..=0x26F9 | 0x2702 | 0x2708..=0x2709
        | 0x270C..=0x270D | 0x270F | 0x2712 | 0x2714 | 0x2716 | 0x271D | 0x2721 | 0x2733..=0x2734 | 0x2744
        | 0x2747 | 0x2763..=0x2764 | 0x27A1 | 0x2934..=0x2935 | 0x2B05..=0x2B07 | 0x3030 | 0x303D | 0x3297 | 0x3299)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alacritty_terminal::event::VoidListener;
    use alacritty_terminal::term::{test::TermSize, Config, Term};
    use alacritty_terminal::vte::ansi::{Processor, StdSyncHandler};

    fn filtered(chunks: &[&str]) -> String {
        let mut f = EmojiClusters::default();
        chunks.iter().map(|c| String::from_utf8(f.process(c.as_bytes()).into_owned()).unwrap()).collect()
    }

    /// 칸 수 — 거른 바이트를 alacritty 에 넣고 끝 글자 뒤 커서 열.
    fn columns(text: &str) -> usize {
        let mut term = Term::new(Config::default(), &TermSize::new(80, 3), VoidListener);
        let mut processor: Processor<StdSyncHandler> = Processor::new();
        processor.advance(&mut term, EmojiClusters::default().process(text.as_bytes()).as_ref());
        term.grid().cursor.point.column.0
    }

    #[test]
    fn every_cluster_takes_the_two_columns_claude_counts() {
        for (text, want) in [("⚠️", 2), ("✔️", 2), ("❤️", 2), ("👍🏻", 2), ("✌🏻", 2), ("👨‍👩‍👧", 2), ("❤️‍🔥", 2),
            ("🏳️‍🌈", 2), ("1️⃣", 2), ("🇰🇷", 2), ("👍", 2), ("⚠", 1), ("가", 2), ("a", 1)] {
            assert_eq!(columns(&format!("{text}x")) - 1, want, "{text}");
        }
    }

    #[test]
    fn plain_text_and_escapes_pass_through_borrowed() {
        let mut f = EmojiClusters::default();
        for text in ["hello 한글 ✓ ⚠ 👍 🇰🇷", "\x1b]0;제목 👍🏻\x07", "\x1b[38;2;1;2;3m색\x1b[0m"] {
            assert!(matches!(f.process(text.as_bytes()), std::borrow::Cow::Borrowed(_)), "{text}");
        }
    }

    #[test]
    fn a_cluster_split_across_reads_still_joins() {
        assert_eq!(filtered(&["👍", "🏻 끝"]), "👍 끝");
        assert_eq!(filtered(&["⚠", "\u{FE0F}x"]), "⚠ x");
        assert_eq!(filtered(&["👨\u{200D}", "👩\u{200D}👧!"]), "👨!");
    }

    #[test]
    fn filtering_twice_changes_nothing_more() {
        for text in ["⚠️ 경고 👍🏻 👨‍👩‍👧 ✌🏻 1️⃣ 끝", "❤️‍🔥x"] {
            let once = filtered(&[text]);
            assert_eq!(filtered(&[&once]), once, "{text}");
        }
    }

    #[test]
    fn a_cursor_move_or_control_between_parts_breaks_the_cluster() {
        assert_eq!(filtered(&["👍\x1b[C🏻"]), "👍\x1b[C🏻");
        assert_eq!(filtered(&["⚠\r\u{FE0F}"]), "⚠\r\u{FE0F}");
        assert_eq!(filtered(&["a\u{200D}b"]), "a\u{200D}b", "ZWJ 는 이모지 뒤에서만 묶는다");
        assert_eq!(filtered(&["🏻"]), "🏻", "바탕 없는 피부색은 그대로");
    }

    #[test]
    fn osc_strings_are_not_touched() {
        let text = "\x1b]2;👍🏻 ⚠️\x1b\\👍🏻";
        assert_eq!(filtered(&[text]), "\x1b]2;👍🏻 ⚠️\x1b\\👍");
    }
}
