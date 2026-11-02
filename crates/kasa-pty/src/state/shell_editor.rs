use super::*;
use std::sync::atomic::{AtomicU8, Ordering};

const PREFIX: &[u8] = b"\x1b]777;kasaterm;zle;";

pub(super) struct Scanner {
    active: Arc<AtomicU8>,
    matched: usize,
    value: u8,
}

impl Scanner {
    pub(super) fn new(active: Arc<AtomicU8>) -> Self {
        Self { active, matched: 0, value: 0 }
    }

    pub(super) fn feed(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            if self.matched < PREFIX.len() && byte == PREFIX[self.matched] {
                self.matched += 1;
                continue;
            }
            if self.matched == PREFIX.len() && matches!(byte, b'0' | b'1' | b'2') {
                self.value = byte - b'0';
                self.matched += 1;
                continue;
            }
            if self.matched == PREFIX.len() + 1 && byte == 7 {
                self.active.store(self.value, Ordering::Release);
            }
            self.matched = usize::from(byte == 0x1b);
        }
    }
}

impl Drop for Scanner {
    fn drop(&mut self) {
        self.active.store(0, Ordering::Release);
    }
}

impl PtySession {
    pub fn shell_editor_selection(&self) -> bool {
        self.shell_editor.load(Ordering::Acquire) == 2
    }

    pub fn shell_editor_active(&self) -> bool {
        self.shell_editor.load(Ordering::Acquire) > 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editor_readiness_survives_every_read_boundary_and_ends_with_the_reader() {
        let on = b"\x1b]777;kasaterm;zle;1\x07";
        for split in 0..=on.len() {
            let active = Arc::new(AtomicU8::new(0));
            let mut scanner = Scanner::new(active.clone());
            scanner.feed(&on[..split]);
            if split < on.len() { assert_eq!(active.load(Ordering::Acquire), 0); }
            scanner.feed(&on[split..]);
            assert_eq!(active.load(Ordering::Acquire), 1);
            scanner.feed(b"output\x1b]777;notify;title;body\x07");
            assert_eq!(active.load(Ordering::Acquire), 1);
            scanner.feed(b"\x1b]777;kasaterm;zle;2\x07");
            assert_eq!(active.load(Ordering::Acquire), 2);
            scanner.feed(b"\x1b]777;kasaterm;zle;0\x07");
            assert_eq!(active.load(Ordering::Acquire), 0);
            scanner.feed(on);
            drop(scanner);
            assert_eq!(active.load(Ordering::Acquire), 0);
        }
    }
}
