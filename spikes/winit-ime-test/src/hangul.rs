//! Minimal in-app Hangul input automaton (두벌식, no consonant/vowel
//! cluster combining yet). Bypasses ibus/winit IME entirely — we read
//! raw key chars and compose syllables ourselves. Sufficient for the
//! WSLg + IME blocker; real product will extend with cluster jamo.

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
    pub fn preedit(&self) -> Option<String> {
        self.buf.render()
    }

    /// Force commit any pending preedit. Use on mode-off / focus-out.
    pub fn flush(&mut self) -> Option<String> {
        self.buf.flush()
    }

    /// Backspace — chip one jamo off the current syllable. If empty,
    /// returns false so caller can forward the backspace to the terminal.
    pub fn backspace(&mut self) -> bool {
        if self.buf.jong.is_some() {
            self.buf.jong = None;
            true
        } else if self.buf.jung.is_some() {
            self.buf.jung = None;
            true
        } else if self.buf.cho.is_some() {
            self.buf.cho = None;
            true
        } else {
            false
        }
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
                // LVT + V → split: commit (c,j,0), start (jong-as-cho, v).
                let new_cho = JONG[jong]
                    .and_then(cho_idx)
                    .unwrap_or(11); // ㅇ fallback
                let prev = syllable(c, j, 0).to_string();
                self.buf = Buffer {
                    cho: Some(new_cho),
                    jung: Some(v),
                    jong: None,
                };
                Some(prev)
            }
            (Some(_), Some(_), None) => {
                // LV + V — no compound vowel yet; commit prev, start standalone vowel.
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
            (Some(_), Some(_), Some(_)) => {
                // already complete syllable, new consonant — commit, start new
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
        let mut c = Composer::default();
        assert_eq!(c.feed(dubeolsik('r').unwrap()), None);
        assert_eq!(c.preedit().as_deref(), Some("ㄱ"));
        assert_eq!(c.feed(dubeolsik('k').unwrap()), None);
        assert_eq!(c.preedit().as_deref(), Some("가"));
        assert_eq!(c.flush().as_deref(), Some("가"));
    }

    #[test]
    fn rkqkfk_makes_가밥아() {
        // "rkqkfk" = ㄱㅏㅂㅏㄹㅏ → 가, 바, 라
        let mut c = Composer::default();
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
        let mut c = Composer::default();
        for ch in "rks".chars() {
            c.feed(dubeolsik(ch).unwrap());
        }
        assert_eq!(c.flush().as_deref(), Some("간"));
    }

    #[test]
    fn rksk_makes_가나() {
        // 간 + ㅏ → 가나 (jong moves to next cho)
        let mut c = Composer::default();
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
