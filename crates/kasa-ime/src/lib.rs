//! Minimal in-app Hangul input automaton (두벌식). Bypasses ibus/winit
//! IME entirely — we read raw key chars and compose syllables ourselves.
//! Supports compound jongseong (ㄺ ㄻ ㄼ ㄳ ㄵ ㄶ ㅀ ㅄ ㄽ ㄾ ㄿ) and the
//! seven standard compound vowels (ㅘ ㅙ ㅚ ㅝ ㅞ ㅟ ㅢ).

const CHO: [char; 19] = [
    'ㄱ', 'ㄲ', 'ㄴ', 'ㄷ', 'ㄸ', 'ㄹ', 'ㅁ', 'ㅂ', 'ㅃ', 'ㅅ',
    'ㅆ', 'ㅇ', 'ㅈ', 'ㅉ', 'ㅊ', 'ㅋ', 'ㅌ', 'ㅍ', 'ㅎ',
];

const JUNG: [char; 21] = [
    'ㅏ', 'ㅐ', 'ㅑ', 'ㅒ', 'ㅓ', 'ㅔ', 'ㅕ', 'ㅖ', 'ㅗ', 'ㅘ',
    'ㅙ', 'ㅚ', 'ㅛ', 'ㅜ', 'ㅝ', 'ㅞ', 'ㅟ', 'ㅠ', 'ㅡ', 'ㅢ',
    'ㅣ',
];

const JONG: [Option<char>; 28] = [
    None,
    Some('ㄱ'), Some('ㄲ'), Some('ㄳ'), Some('ㄴ'), Some('ㄵ'),
    Some('ㄶ'), Some('ㄷ'), Some('ㄹ'), Some('ㄺ'), Some('ㄻ'),
    Some('ㄼ'), Some('ㄽ'), Some('ㄾ'), Some('ㄿ'), Some('ㅀ'),
    Some('ㅁ'), Some('ㅂ'), Some('ㅄ'), Some('ㅅ'), Some('ㅆ'),
    Some('ㅇ'), Some('ㅈ'), Some('ㅊ'), Some('ㅋ'), Some('ㅌ'),
    Some('ㅍ'), Some('ㅎ'),
];

fn cho_idx(c: char) -> Option<usize> {
    CHO.iter().position(|&x| x == c)
}
fn jung_idx(c: char) -> Option<usize> {
    JUNG.iter().position(|&x| x == c)
}
fn jong_idx(c: char) -> Option<usize> {
    JONG.iter().position(|&x| x == Some(c))
}

/// Standard 두벌식 jongseong cluster combinations. Given the current
/// jongseong index and a newly typed consonant's *jong* index, return
/// the composed cluster's jong index. Mirrors the KS X 5002 table the
/// platform IMEs use, so "맑" / "뷁" / "값" / "않" compose the way the
/// user expects from macOS / IBus.
fn compose_jong(head: usize, tail: usize) -> Option<usize> {
    Some(match (head, tail) {
        (1, 19) => 3,    // ㄱ + ㅅ → ㄳ
        (4, 22) => 5,    // ㄴ + ㅈ → ㄵ
        (4, 27) => 6,    // ㄴ + ㅎ → ㄶ
        (8, 1) => 9,     // ㄹ + ㄱ → ㄺ
        (8, 16) => 10,   // ㄹ + ㅁ → ㄻ
        (8, 17) => 11,   // ㄹ + ㅂ → ㄼ
        (8, 19) => 12,   // ㄹ + ㅅ → ㄽ
        (8, 25) => 13,   // ㄹ + ㅌ → ㄾ
        (8, 26) => 14,   // ㄹ + ㅍ → ㄿ
        (8, 27) => 15,   // ㄹ + ㅎ → ㅀ
        (17, 19) => 18,  // ㅂ + ㅅ → ㅄ
        _ => return None,
    })
}

/// Inverse of `compose_jong`. When a vowel arrives after a cluster
/// jongseong, the *tail* consonant peels off to become the next
/// syllable's choseong, while the *head* stays as the previous
/// syllable's jongseong ("맑" + ㅏ → "말" + "가"). Returns
/// (kept_jong_idx, peeled_cho_idx).
fn decompose_jong(j: usize) -> Option<(usize, usize)> {
    Some(match j {
        3 => (1, 9),    // ㄳ → ㄱ + ㅅ(cho 9)
        5 => (4, 12),   // ㄵ → ㄴ + ㅈ
        6 => (4, 18),   // ㄶ → ㄴ + ㅎ
        9 => (8, 0),    // ㄺ → ㄹ + ㄱ
        10 => (8, 6),   // ㄻ → ㄹ + ㅁ
        11 => (8, 7),   // ㄼ → ㄹ + ㅂ
        12 => (8, 9),   // ㄽ → ㄹ + ㅅ
        13 => (8, 16),  // ㄾ → ㄹ + ㅌ
        14 => (8, 17),  // ㄿ → ㄹ + ㅍ
        15 => (8, 18),  // ㅀ → ㄹ + ㅎ
        18 => (17, 9),  // ㅄ → ㅂ + ㅅ
        _ => return None,
    })
}

/// The seven 두벌식 이중모음 as (head, tail, composed) jung indices.
/// Each is typed as head key then tail key, so backspace returns to the
/// head rather than dropping the whole vowel ("되" → "도", not "ㄷ").
const JUNG_PAIRS: [(usize, usize, usize); 7] = [
    (8, 0, 9),    // ㅗ + ㅏ → ㅘ
    (8, 1, 10),   // ㅗ + ㅐ → ㅙ
    (8, 20, 11),  // ㅗ + ㅣ → ㅚ
    (13, 4, 14),  // ㅜ + ㅓ → ㅝ
    (13, 5, 15),  // ㅜ + ㅔ → ㅞ
    (13, 20, 16), // ㅜ + ㅣ → ㅟ
    (18, 20, 19), // ㅡ + ㅣ → ㅢ
];

fn compose_jung(head: usize, tail: usize) -> Option<usize> {
    JUNG_PAIRS
        .iter()
        .find(|p| (p.0, p.1) == (head, tail))
        .map(|p| p.2)
}

fn decompose_jung(j: usize) -> Option<usize> {
    JUNG_PAIRS.iter().find(|p| p.2 == j).map(|p| p.0)
}

fn syllable(cho: usize, jung: usize, jong: usize) -> char {
    let code = 0xAC00 + (cho * 21 + jung) * 28 + jong;
    char::from_u32(code as u32).unwrap()
}

/// Two-set (두벌식) KS X 5002 mapping. Returns the jamo for a
/// physical key char (lowercase = base, uppercase = shifted).
pub fn dubeolsik(c: char) -> Option<char> {
    Some(match c {
        'q' => 'ㅂ', 'Q' => 'ㅃ',
        'w' => 'ㅈ', 'W' => 'ㅉ',
        'e' => 'ㄷ', 'E' => 'ㄸ',
        'r' => 'ㄱ', 'R' => 'ㄲ',
        't' => 'ㅅ', 'T' => 'ㅆ',
        'y' => 'ㅛ',
        'u' => 'ㅕ',
        'i' => 'ㅑ',
        'o' => 'ㅐ', 'O' => 'ㅒ',
        'p' => 'ㅔ', 'P' => 'ㅖ',
        'a' => 'ㅁ',
        's' => 'ㄴ',
        'd' => 'ㅇ',
        'f' => 'ㄹ',
        'g' => 'ㅎ',
        'h' => 'ㅗ',
        'j' => 'ㅓ',
        'k' => 'ㅏ',
        'l' => 'ㅣ',
        'z' => 'ㅋ',
        'x' => 'ㅌ',
        'c' => 'ㅊ',
        'v' => 'ㅍ',
        'b' => 'ㅠ',
        'n' => 'ㅜ',
        'm' => 'ㅡ',
        _ => return None,
    })
}

#[derive(Debug, Default, Clone)]
struct Buffer {
    cho: Option<usize>,
    jung: Option<usize>,
    jong: Option<usize>,
}

impl Buffer {
    #[allow(dead_code)]
    fn is_empty(&self) -> bool {
        self.cho.is_none() && self.jung.is_none() && self.jong.is_none()
    }

    fn render(&self) -> Option<String> {
        match (self.cho, self.jung, self.jong) {
            (Some(c), Some(j), Some(jj)) => Some(syllable(c, j, jj).to_string()),
            (Some(c), Some(j), None) => Some(syllable(c, j, 0).to_string()),
            (Some(c), None, None) => Some(CHO[c].to_string()),
            (None, Some(j), None) => Some(JUNG[j].to_string()),
            _ => None,
        }
    }

    fn flush(&mut self) -> Option<String> {
        let out = self.render();
        *self = Buffer::default();
        out
    }
}

/// One step of feeding a jamo. Returns (committed_text, current_preedit).
/// Committed text is the now-finalized syllable(s) that should leave the
/// IME, preedit is the still-composing syllable.
#[derive(Debug, Default)]
pub struct Composer {
    buf: Buffer,
}

impl Composer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn preedit(&self) -> Option<String> {
        self.buf.render()
    }

    /// Force commit any pending preedit. Use on mode-off / focus-out.
    pub fn flush(&mut self) -> Option<String> {
        self.buf.flush()
    }

    /// Backspace — undo the last key of the current syllable. A
    /// compound vowel or cluster jongseong falls back to its first key
    /// ("뷁" → "뷀" → "붸" → "부"); single-key jamo (ㅐ ㅔ ㄲ ㅆ …) go
    /// whole. If empty, returns false so caller can forward the
    /// backspace to the terminal.
    pub fn backspace(&mut self) -> bool {
        let buf = &mut self.buf;
        if let Some(jong) = buf.jong {
            buf.jong = decompose_jong(jong).map(|(kept, _)| kept);
        } else if let Some(jung) = buf.jung {
            buf.jung = decompose_jung(jung);
        } else if buf.cho.is_some() {
            buf.cho = None;
        } else {
            return false;
        }
        true
    }

    /// Feed one jamo. Returns Some(committed) if a syllable was finalized.
    pub fn feed(&mut self, jamo: char) -> Option<String> {
        let is_cons = cho_idx(jamo).is_some() || jong_idx(jamo).is_some();
        let is_vow = jung_idx(jamo).is_some();

        // Vowel
        if is_vow {
            let v = jung_idx(jamo).unwrap();
            return self.feed_vowel(v);
        }
        // Consonant
        if is_cons {
            return self.feed_consonant(jamo);
        }
        // Unknown jamo — flush and treat as commit-only
        let pending = self.buf.flush();
        let mut s = pending.unwrap_or_default();
        s.push(jamo);
        Some(s)
    }

    fn feed_vowel(&mut self, v: usize) -> Option<String> {
        match (self.buf.cho, self.buf.jung, self.buf.jong) {
            (Some(c), Some(j), Some(jong)) => {
                // LVT + V split. If the jongseong is a cluster ("맑" +
                // ㅏ), keep the head consonant as the jong of the
                // committed syllable and peel the *tail* off as the
                // next cho ("말" + "가"). Otherwise the entire jong
                // moves to the next syllable's cho (existing path).
                let (prev_jong, new_cho) = if let Some((kept, peeled)) = decompose_jong(jong) {
                    (kept, peeled)
                } else {
                    let cho = JONG[jong].and_then(cho_idx).unwrap_or(11);
                    (0, cho)
                };
                let prev = syllable(c, j, prev_jong).to_string();
                self.buf = Buffer {
                    cho: Some(new_cho),
                    jung: Some(v),
                    jong: None,
                };
                Some(prev)
            }
            (Some(_), Some(j), None) => {
                // LV + V — try the compound vowels before falling back
                // to "commit prev, start standalone vowel".
                if let Some(jv) = compose_jung(j, v) {
                    self.buf.jung = Some(jv);
                    return None;
                }
                let prev = self.buf.flush();
                self.buf.jung = Some(v);
                prev
            }
            (Some(_), None, None) => {
                self.buf.jung = Some(v);
                None
            }
            (None, Some(_), None) => {
                let prev = self.buf.flush();
                self.buf.jung = Some(v);
                prev
            }
            (None, None, None) => {
                self.buf.jung = Some(v);
                None
            }
            _ => None,
        }
    }

    fn feed_consonant(&mut self, jamo: char) -> Option<String> {
        let cho = cho_idx(jamo);
        let jong = jong_idx(jamo);
        match (self.buf.cho, self.buf.jung, self.buf.jong) {
            (None, None, None) => {
                if let Some(c) = cho {
                    self.buf.cho = Some(c);
                }
                None
            }
            (Some(_), None, None) => {
                // already cho, another cho — commit prev as standalone, start new
                let prev = self.buf.flush();
                if let Some(c) = cho {
                    self.buf.cho = Some(c);
                }
                prev
            }
            (None, Some(_), None) => {
                // standalone vowel then consonant — commit vowel, start cho
                let prev = self.buf.flush();
                if let Some(c) = cho {
                    self.buf.cho = Some(c);
                }
                prev
            }
            (Some(_), Some(_), None) => {
                if let Some(j) = jong {
                    self.buf.jong = Some(j);
                    None
                } else {
                    // can't be jong (e.g. ㅃ ㄸ ㅉ) — commit current LV, start new cho
                    let prev = self.buf.flush();
                    if let Some(c) = cho {
                        self.buf.cho = Some(c);
                    }
                    prev
                }
            }
            (Some(_), Some(_), Some(head)) => {
                // LVT + cons. Try to extend the jongseong into a
                // cluster first ("말" + ㄱ → "맑"). The platform IME
                // keeps composing until the user types a vowel or a
                // cluster that doesn't exist, so we mirror that.
                if let Some(tail) = jong {
                    if let Some(combined) = compose_jong(head, tail) {
                        self.buf.jong = Some(combined);
                        return None;
                    }
                }
                // No cluster — commit current syllable, start new.
                let prev = self.buf.flush();
                if let Some(c) = cho {
                    self.buf.cho = Some(c);
                }
                prev
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rk_makes_ga() {
        let mut c = Composer::new();
        assert_eq!(c.feed(dubeolsik('r').unwrap()), None);
        assert_eq!(c.preedit().as_deref(), Some("ㄱ"));
        assert_eq!(c.feed(dubeolsik('k').unwrap()), None);
        assert_eq!(c.preedit().as_deref(), Some("가"));
        assert_eq!(c.flush().as_deref(), Some("가"));
    }

    #[test]
    fn rkqkfk_makes_가밥아() {
        // "rkqkfk" = ㄱㅏㅂㅏㄹㅏ → 가, 바, 라
        let mut c = Composer::new();
        let mut out = String::new();
        for ch in "rkqkfk".chars() {
            if let Some(s) = c.feed(dubeolsik(ch).unwrap()) {
                out.push_str(&s);
            }
        }
        if let Some(s) = c.flush() {
            out.push_str(&s);
        }
        assert_eq!(out, "가바라");
    }

    #[test]
    fn rks_makes_간() {
        // r=ㄱ k=ㅏ s=ㄴ → 간
        let mut c = Composer::new();
        for ch in "rks".chars() {
            c.feed(dubeolsik(ch).unwrap());
        }
        assert_eq!(c.flush().as_deref(), Some("간"));
    }

    #[test]
    fn mn_with_compound_vowel_makes_뭐() {
        // ㅁ + ㅜ + ㅓ → 뭐 (ㅁ + 복모음 ㅝ).
        // Direct keys: "ak" — wait, dubeolsik:
        // 'a' = ㅁ, 'n' = ㅜ, 'j' = ㅓ. So "anj" → 뭐.
        let mut c = Composer::new();
        for ch in "anj".chars() {
            c.feed(dubeolsik(ch).unwrap());
        }
        assert_eq!(c.preedit().as_deref(), Some("뭐"));
        assert_eq!(c.flush().as_deref(), Some("뭐"));
    }

    fn type_keys(c: &mut Composer, keys: &str) -> String {
        let mut out = String::new();
        for ch in keys.chars() {
            if let Some(s) = c.feed(dubeolsik(ch).unwrap()) {
                out.push_str(&s);
            }
        }
        out
    }

    /// Presses backspace until the composer gives up, recording the
    /// preedit after each press.
    fn unwind(c: &mut Composer) -> Vec<String> {
        let mut steps = Vec::new();
        while c.backspace() {
            steps.push(c.preedit().unwrap_or_default());
        }
        steps
    }

    const SEVEN_COMPOUND_VOWELS: [(&str, &str, &str); 7] = [
        ("dhk", "와", "오"),
        ("dho", "왜", "오"),
        ("dhl", "외", "오"),
        ("dnj", "워", "우"),
        ("dnp", "웨", "우"),
        ("dnl", "위", "우"),
        ("dml", "의", "으"),
    ];

    #[test]
    fn all_seven_compound_vowels() {
        for (keys, syllable, _) in SEVEN_COMPOUND_VOWELS {
            let mut c = Composer::new();
            assert_eq!(type_keys(&mut c, keys), "", "{keys:?} committed early");
            assert_eq!(c.preedit().as_deref(), Some(syllable), "{keys:?}");
            assert_eq!(c.flush().as_deref(), Some(syllable), "{keys:?}");
        }
    }

    #[test]
    fn backspace_peels_compound_vowel_to_its_first_key() {
        for (keys, syllable, head) in SEVEN_COMPOUND_VOWELS {
            let mut c = Composer::new();
            type_keys(&mut c, keys);
            assert_eq!(c.preedit().as_deref(), Some(syllable));
            assert_eq!(unwind(&mut c), [head, "ㅇ", ""], "{keys:?}");
        }
    }

    #[test]
    fn backspace_on_되_returns_to_도() {
        let mut c = Composer::new();
        type_keys(&mut c, "ehl");
        assert_eq!(c.preedit().as_deref(), Some("되"));
        assert!(c.backspace());
        assert_eq!(c.preedit().as_deref(), Some("도"));
        // The restored head vowel keeps composing like freshly typed.
        assert_eq!(type_keys(&mut c, "k"), "");
        assert_eq!(c.preedit().as_deref(), Some("돠"));
        assert!(c.backspace());
        assert_eq!(type_keys(&mut c, "s"), "");
        assert_eq!(c.flush().as_deref(), Some("돈"));
    }

    #[test]
    fn backspace_peels_cluster_jongseong_to_its_first_key() {
        let cases = [
            ("rkrt", "갃", "각"),
            ("dksw", "앉", "안"),
            ("dksg", "않", "안"),
            ("akfr", "맑", "말"),
            ("tkfa", "삶", "살"),
            ("dufq", "엷", "열"),
            ("dhft", "옰", "올"),
            ("gkfx", "핥", "할"),
            ("dmfv", "읊", "을"),
            ("dlfg", "잃", "일"),
            ("rkqt", "값", "갑"),
        ];
        for (keys, syllable, head) in cases {
            let mut c = Composer::new();
            type_keys(&mut c, keys);
            assert_eq!(c.preedit().as_deref(), Some(syllable), "{keys:?}");
            let steps = unwind(&mut c);
            assert_eq!(steps[0], head, "{keys:?}");
            assert_eq!(steps.len(), 4, "{keys:?} took {steps:?}");
        }
    }

    #[test]
    fn backspace_unwinds_뷁_one_key_per_press() {
        let mut c = Composer::new();
        type_keys(&mut c, "qnpfr");
        assert_eq!(unwind(&mut c), ["뷀", "붸", "부", "ㅂ", ""]);
        assert!(c.preedit().is_none());
    }

    #[test]
    fn backspace_after_peel_resumes_cluster_and_syllable_split() {
        let mut c = Composer::new();
        type_keys(&mut c, "akfr");
        assert!(c.backspace());
        assert_eq!(type_keys(&mut c, "q"), "");
        assert_eq!(c.preedit().as_deref(), Some("맓"));
        assert!(c.backspace());
        assert_eq!(c.preedit().as_deref(), Some("말"));
        assert_eq!(type_keys(&mut c, "k"), "마");
        assert_eq!(c.preedit().as_deref(), Some("라"));
    }

    #[test]
    fn backspace_does_not_split_single_key_jamo() {
        // ㅐ ㅔ ㅒ ㅖ and the Shift doubles are one key each.
        let cases = [
            ("ro", "개", "ㄱ"),
            ("rp", "게", "ㄱ"),
            ("rO", "걔", "ㄱ"),
            ("rP", "계", "ㄱ"),
            ("Rk", "까", "ㄲ"),
            ("Ek", "따", "ㄸ"),
            ("Qk", "빠", "ㅃ"),
            ("Wk", "짜", "ㅉ"),
            ("rkT", "갔", "가"),
            ("rkR", "갂", "가"),
        ];
        for (keys, syllable, after) in cases {
            let mut c = Composer::new();
            type_keys(&mut c, keys);
            assert_eq!(c.preedit().as_deref(), Some(syllable), "{keys:?}");
            assert!(c.backspace());
            assert_eq!(c.preedit().as_deref(), Some(after), "{keys:?}");
        }
        let mut c = Composer::new();
        type_keys(&mut c, "R");
        assert_eq!(unwind(&mut c), [""]);
    }

    #[test]
    fn backspace_never_reaches_committed_syllable() {
        // 맑 + ㅏ commits 말; backspace only edits the new 가.
        let mut c = Composer::new();
        assert_eq!(type_keys(&mut c, "akfrk"), "말");
        assert_eq!(unwind(&mut c), ["ㄱ", ""]);
        assert!(!c.backspace());
    }

    #[test]
    fn akfr_makes_맑() {
        // ㅁ + ㅏ + ㄹ + ㄱ → 맑 (cluster jongseong ㄺ).
        let mut c = Composer::new();
        for ch in "akfr".chars() {
            c.feed(dubeolsik(ch).unwrap());
        }
        assert_eq!(c.preedit().as_deref(), Some("맑"));
        assert_eq!(c.flush().as_deref(), Some("맑"));
    }

    #[test]
    fn akfrk_splits_to_말가() {
        // 맑 + ㅏ → 말 + 가 (cluster peels: ㄹ stays, ㄱ → next cho).
        let mut c = Composer::new();
        let mut out = String::new();
        for ch in "akfrk".chars() {
            if let Some(s) = c.feed(dubeolsik(ch).unwrap()) {
                out.push_str(&s);
            }
        }
        if let Some(s) = c.flush() {
            out.push_str(&s);
        }
        assert_eq!(out, "말가");
    }

    #[test]
    fn rkqt_makes_값() {
        // ㄱ + ㅏ + ㅂ + ㅅ → 값 (cluster ㅄ).
        let mut c = Composer::new();
        for ch in "rkqt".chars() {
            c.feed(dubeolsik(ch).unwrap());
        }
        assert_eq!(c.preedit().as_deref(), Some("값"));
    }

    #[test]
    fn qnpfr_makes_뷁() {
        // ㅂ + ㅜㅔ(ㅞ) + ㄹㄱ(ㄺ) → 뷁
        let mut c = Composer::new();
        for ch in "qnpfr".chars() {
            c.feed(dubeolsik(ch).unwrap());
        }
        assert_eq!(c.preedit().as_deref(), Some("뷁"));
    }

    #[test]
    fn rksk_makes_가나() {
        // 간 + ㅏ → 가나 (jong moves to next cho)
        let mut c = Composer::new();
        let mut out = String::new();
        for ch in "rksk".chars() {
            if let Some(s) = c.feed(dubeolsik(ch).unwrap()) {
                out.push_str(&s);
            }
        }
        if let Some(s) = c.flush() {
            out.push_str(&s);
        }
        assert_eq!(out, "가나");
    }
}
