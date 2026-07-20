//! 마크다운 pane 에디터 입력 + md 링크/블록 열기·복사.
use super::*;

/// Undo history depth per editor pane. Whole-buffer snapshots, so this also
/// bounds memory (~cap × file size worst case).
pub(crate) const UNDO_CAP: usize = 100;

/// Word-motion character class: identifier chars group together, everything
/// else (symbols) groups separately, whitespace separates both.
fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Column of the previous word boundary (Opt+Left): skip trailing whitespace,
/// then consume one run of same-class chars.
fn prev_word_col(chars: &[char], col: usize) -> usize {
    let mut i = col.min(chars.len());
    while i > 0 && chars[i - 1].is_whitespace() {
        i -= 1;
    }
    if i == 0 {
        return 0;
    }
    let w = is_word_char(chars[i - 1]);
    while i > 0 && !chars[i - 1].is_whitespace() && is_word_char(chars[i - 1]) == w {
        i -= 1;
    }
    i
}

/// Column of the next word boundary (Opt+Right): mirror of `prev_word_col`.
fn next_word_col(chars: &[char], col: usize) -> usize {
    let n = chars.len();
    let mut i = col.min(n);
    while i < n && chars[i].is_whitespace() {
        i += 1;
    }
    if i == n {
        return n;
    }
    let w = is_word_char(chars[i]);
    while i < n && !chars[i].is_whitespace() && is_word_char(chars[i]) == w {
        i += 1;
    }
    i
}

impl MarkdownPane {
    /// Normalized selection as (start, end) in (line, col), start < end.
    /// None when there's no anchor or the selection is empty. Out-of-range
    /// endpoints (buffer shrank under a stale anchor) clamp to the buffer.
    pub(crate) fn sel_range(&self) -> Option<((usize, usize), (usize, usize))> {
        let anchor = self.sel_anchor?;
        let last = self.edit_lines.len().saturating_sub(1);
        let clamp = |(l, c): (usize, usize)| {
            let l = l.min(last);
            (l, c.min(self.edit_lines.get(l).map_or(0, |s| s.chars().count())))
        };
        let a = clamp(anchor);
        let b = clamp((self.cur_line, self.cur_col));
        if a == b {
            return None;
        }
        Some(if a < b { (a, b) } else { (b, a) })
    }
    /// The selected text with `\n` between lines; None when no selection.
    pub(crate) fn selected_text(&self) -> Option<String> {
        let (s, e) = self.sel_range()?;
        if s.0 == e.0 {
            let line = &self.edit_lines[s.0];
            return Some(line[char_byte(line, s.1)..char_byte(line, e.1)].to_string());
        }
        let mut out = String::new();
        let first = &self.edit_lines[s.0];
        out.push_str(&first[char_byte(first, s.1)..]);
        for li in s.0 + 1..e.0 {
            out.push('\n');
            out.push_str(&self.edit_lines[li]);
        }
        out.push('\n');
        let last = &self.edit_lines[e.0];
        out.push_str(&last[..char_byte(last, e.1)]);
        Some(out)
    }
    /// Remove the selected range, land the cursor at its start, drop the
    /// anchor. Returns false (and just clears the anchor) with no selection.
    /// Callers own the undo snapshot — push before calling.
    pub(crate) fn delete_selection(&mut self) -> bool {
        let Some((s, e)) = self.sel_range() else {
            self.sel_anchor = None;
            return false;
        };
        if s.0 == e.0 {
            let line = &mut self.edit_lines[s.0];
            let b0 = char_byte(line, s.1);
            let b1 = char_byte(line, e.1);
            line.replace_range(b0..b1, "");
        } else {
            let tail = {
                let last = &self.edit_lines[e.0];
                last[char_byte(last, e.1)..].to_string()
            };
            let keep = char_byte(&self.edit_lines[s.0], s.1);
            self.edit_lines[s.0].truncate(keep);
            self.edit_lines[s.0].push_str(&tail);
            self.edit_lines.drain(s.0 + 1..=e.0);
        }
        self.cur_line = s.0;
        self.cur_col = s.1;
        self.sel_anchor = None;
        self.modified = true;
        true
    }
    fn snapshot(&self) -> EditSnapshot {
        EditSnapshot { lines: self.edit_lines.clone(), cur: (self.cur_line, self.cur_col) }
    }
    /// Push the pre-edit state onto the undo stack. Consecutive same-kind
    /// edits (a typing run, a backspace run) coalesce into the run's first
    /// snapshot; `Other` is always its own boundary. Any redo history dies
    /// here — a fresh edit forks the timeline.
    pub(crate) fn push_undo(&mut self, kind: EditKind) {
        if kind != EditKind::Other && self.last_edit == kind {
            return;
        }
        self.undo_stack.push(self.snapshot());
        if self.undo_stack.len() > UNDO_CAP {
            self.undo_stack.remove(0);
        }
        self.redo_stack.clear();
        self.last_edit = kind;
    }
    /// Restore a snapshot (undo/redo target), clamping the cursor to it.
    fn apply_snapshot(&mut self, snap: EditSnapshot) {
        self.edit_lines = snap.lines;
        if self.edit_lines.is_empty() {
            self.edit_lines.push(String::new());
        }
        self.cur_line = snap.cur.0.min(self.edit_lines.len() - 1);
        self.cur_col = snap.cur.1.min(self.edit_lines[self.cur_line].chars().count());
        self.sel_anchor = None;
        self.last_edit = EditKind::Break;
        self.modified = true;
    }

    // ── Pure buffer-mutation core (shared by the active-pane path in `impl App`
    // below and the pop-out editor window in auxwin.rs). These operate only on
    // the pane's own fields — no `App`/`ws` — so both drivers reuse identical
    // edit semantics; the driver owns undo/caret-scroll wiring around them.

    /// Force raw-editor mode and seed the edit buffer from the doc source if it
    /// isn't populated yet (a `.md` opened in Render mode carries no lines). A
    /// pop-out window always edits raw, so this runs on the way out.
    pub(crate) fn ensure_raw_seeded(&mut self) {
        if !self.raw_mode {
            self.raw_mode = true;
        }
        if self.edit_lines.is_empty() {
            self.edit_lines = self.doc.raw.split('\n').map(String::from).collect();
            if self.edit_lines.is_empty() {
                self.edit_lines.push(String::new());
            }
        }
    }

    /// Insert `text` (a committed Hangul syllable or a single typed segment) at
    /// the caret as one typing run — replacing any active selection. No-op on
    /// empty text.
    pub(crate) fn insert_at_caret(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        if self.edit_lines.is_empty() {
            self.edit_lines.push(String::new());
        }
        // Typing over a selection replaces it — delete + insert as one undo
        // unit (the Other snapshot covers both).
        if self.sel_range().is_some() {
            self.push_undo(EditKind::Other);
            self.delete_selection();
        } else {
            self.push_undo(EditKind::Typing);
        }
        let line = self.cur_line.min(self.edit_lines.len() - 1);
        let col = self.cur_col.min(self.edit_lines[line].chars().count());
        let s = &mut self.edit_lines[line];
        let b = char_byte(s, col);
        s.insert_str(b, text);
        self.cur_line = line;
        self.cur_col = col + text.chars().count();
        self.modified = true;
    }

    /// Apply one editing/motion key to the buffer. `shift`/`alt` are the live
    /// modifier state; `page_lines` is the pre-computed PageUp/Down step (the
    /// driver measures it against its own renderer). Selection, undo runs, and
    /// caret math match the terminal-window editor exactly.
    pub(crate) fn apply_edit_key(
        &mut self,
        event: &KeyEvent,
        shift: bool,
        alt: bool,
        page_lines: usize,
    ) {
        use winit::keyboard::{Key, NamedKey};
        if self.edit_lines.is_empty() {
            self.edit_lines.push(String::new());
        }
        let is_motion = matches!(
            &event.logical_key,
            Key::Named(
                NamedKey::ArrowLeft
                    | NamedKey::ArrowRight
                    | NamedKey::ArrowUp
                    | NamedKey::ArrowDown
                    | NamedKey::Home
                    | NamedKey::End
                    | NamedKey::PageUp
                    | NamedKey::PageDown
            )
        );
        // Shift+motion grows a selection from the current caret; plain motion
        // drops it (Left/Right collapse to its edges first).
        if is_motion && shift && self.sel_anchor.is_none() {
            self.sel_anchor = Some((self.cur_line, self.cur_col));
        }
        let sel = self.sel_range();
        let mut line = self.cur_line.min(self.edit_lines.len() - 1);
        let mut col = self.cur_col.min(self.edit_lines[line].chars().count());
        // 버퍼를 실제로 바꾼 키만 modified 로 기록 — 방향키는 저장할 게 없다.
        let mut edited = false;
        match &event.logical_key {
            Key::Named(NamedKey::Backspace) => {
                if sel.is_some() {
                    self.push_undo(EditKind::Other);
                    self.delete_selection();
                    line = self.cur_line;
                    col = self.cur_col;
                    edited = true;
                } else if col > 0 {
                    self.push_undo(EditKind::Deleting);
                    let s = &mut self.edit_lines[line];
                    let b0 = char_byte(s, col - 1);
                    let b1 = char_byte(s, col);
                    s.replace_range(b0..b1, "");
                    col -= 1;
                    edited = true;
                } else if line > 0 {
                    self.push_undo(EditKind::Other);
                    let cur = self.edit_lines.remove(line);
                    line -= 1;
                    col = self.edit_lines[line].chars().count();
                    self.edit_lines[line].push_str(&cur);
                    edited = true;
                }
            }
            Key::Named(NamedKey::Delete) => {
                if sel.is_some() {
                    self.push_undo(EditKind::Other);
                    self.delete_selection();
                    line = self.cur_line;
                    col = self.cur_col;
                    edited = true;
                } else if col < self.edit_lines[line].chars().count() {
                    self.push_undo(EditKind::Deleting);
                    let s = &mut self.edit_lines[line];
                    let b0 = char_byte(s, col);
                    let b1 = char_byte(s, col + 1);
                    s.replace_range(b0..b1, "");
                    edited = true;
                } else if line + 1 < self.edit_lines.len() {
                    self.push_undo(EditKind::Other);
                    let next = self.edit_lines.remove(line + 1);
                    self.edit_lines[line].push_str(&next);
                    edited = true;
                }
            }
            Key::Named(NamedKey::Enter) => {
                self.push_undo(EditKind::Other);
                if self.delete_selection() {
                    line = self.cur_line;
                    col = self.cur_col;
                }
                let s = &mut self.edit_lines[line];
                let b = char_byte(s, col);
                let rest = s.split_off(b);
                self.edit_lines.insert(line + 1, rest);
                line += 1;
                col = 0;
                edited = true;
            }
            Key::Named(NamedKey::ArrowLeft) => {
                if !shift && sel.is_some() {
                    (line, col) = sel.unwrap().0;
                } else if alt {
                    if col == 0 && line > 0 {
                        line -= 1;
                        col = self.edit_lines[line].chars().count();
                    } else {
                        let chars: Vec<char> = self.edit_lines[line].chars().collect();
                        col = prev_word_col(&chars, col);
                    }
                } else if col > 0 {
                    col -= 1;
                } else if line > 0 {
                    line -= 1;
                    col = self.edit_lines[line].chars().count();
                }
            }
            Key::Named(NamedKey::ArrowRight) => {
                let len = self.edit_lines[line].chars().count();
                if !shift && sel.is_some() {
                    (line, col) = sel.unwrap().1;
                } else if alt {
                    if col >= len && line + 1 < self.edit_lines.len() {
                        line += 1;
                        col = 0;
                    } else {
                        let chars: Vec<char> = self.edit_lines[line].chars().collect();
                        col = next_word_col(&chars, col);
                    }
                } else if col < len {
                    col += 1;
                } else if line + 1 < self.edit_lines.len() {
                    line += 1;
                    col = 0;
                }
            }
            Key::Named(NamedKey::ArrowUp) => {
                if line > 0 {
                    line -= 1;
                    col = col.min(self.edit_lines[line].chars().count());
                }
            }
            Key::Named(NamedKey::ArrowDown) => {
                if line + 1 < self.edit_lines.len() {
                    line += 1;
                    col = col.min(self.edit_lines[line].chars().count());
                }
            }
            Key::Named(NamedKey::Home) => {
                col = 0;
            }
            Key::Named(NamedKey::End) => {
                col = self.edit_lines[line].chars().count();
            }
            Key::Named(NamedKey::PageUp) => {
                line = line.saturating_sub(page_lines);
                col = col.min(self.edit_lines[line].chars().count());
            }
            Key::Named(NamedKey::PageDown) => {
                line = (line + page_lines).min(self.edit_lines.len() - 1);
                col = col.min(self.edit_lines[line].chars().count());
            }
            Key::Named(NamedKey::Space) => {
                if sel.is_some() {
                    self.push_undo(EditKind::Other);
                    self.delete_selection();
                    line = self.cur_line;
                    col = self.cur_col;
                } else {
                    self.push_undo(EditKind::Typing);
                }
                let s = &mut self.edit_lines[line];
                let b = char_byte(s, col);
                s.insert(b, ' ');
                col += 1;
                edited = true;
            }
            Key::Character(txt) => {
                if sel.is_some() {
                    self.push_undo(EditKind::Other);
                    self.delete_selection();
                    line = self.cur_line;
                    col = self.cur_col;
                } else {
                    self.push_undo(EditKind::Typing);
                }
                let s = &mut self.edit_lines[line];
                let b = char_byte(s, col);
                s.insert_str(b, txt);
                col += txt.chars().count();
                edited = true;
            }
            _ => {}
        }
        self.cur_line = line;
        self.cur_col = col;
        if edited {
            self.modified = true;
        }
        if is_motion {
            if !shift {
                self.sel_anchor = None;
            }
            self.last_edit = EditKind::Break;
        }
    }

    /// Cmd+arrow jumps (line start/end, doc start/end). Shift extends the
    /// selection like the plain motions.
    pub(crate) fn apply_cmd_arrow(&mut self, code: winit::keyboard::KeyCode, shift: bool) {
        use winit::keyboard::KeyCode;
        if self.edit_lines.is_empty() {
            self.edit_lines.push(String::new());
        }
        if shift {
            if self.sel_anchor.is_none() {
                self.sel_anchor = Some((self.cur_line, self.cur_col));
            }
        } else {
            self.sel_anchor = None;
        }
        let line = self.cur_line.min(self.edit_lines.len() - 1);
        match code {
            KeyCode::ArrowLeft => {
                self.cur_line = line;
                self.cur_col = 0;
            }
            KeyCode::ArrowRight => {
                self.cur_line = line;
                self.cur_col = self.edit_lines[line].chars().count();
            }
            KeyCode::ArrowUp => {
                self.cur_line = 0;
                self.cur_col = 0;
            }
            KeyCode::ArrowDown => {
                self.cur_line = self.edit_lines.len() - 1;
                self.cur_col = self.edit_lines[self.cur_line].chars().count();
            }
            _ => {}
        }
        self.last_edit = EditKind::Break;
    }

    /// Undo: pop the undo stack, stashing the present on redo. Returns whether
    /// anything moved.
    pub(crate) fn do_undo(&mut self) -> bool {
        let Some(snap) = self.undo_stack.pop() else { return false };
        let now = self.snapshot();
        self.redo_stack.push(now);
        self.apply_snapshot(snap);
        true
    }

    /// Redo: inverse of `do_undo`.
    pub(crate) fn do_redo(&mut self) -> bool {
        let Some(snap) = self.redo_stack.pop() else { return false };
        let now = self.snapshot();
        self.undo_stack.push(now);
        if self.undo_stack.len() > UNDO_CAP {
            self.undo_stack.remove(0);
        }
        self.apply_snapshot(snap);
        true
    }

    /// Cmd+A: anchor at the top, caret at the very end.
    pub(crate) fn select_all_buf(&mut self) {
        if self.edit_lines.is_empty() {
            self.edit_lines.push(String::new());
        }
        self.sel_anchor = Some((0, 0));
        self.cur_line = self.edit_lines.len() - 1;
        self.cur_col = self.edit_lines[self.cur_line].chars().count();
        self.last_edit = EditKind::Break;
    }

    /// Splice already-normalized (`\n`-only) clipboard text at the caret as one
    /// undo unit, replacing any selection.
    pub(crate) fn paste_at_caret(&mut self, text: &str) {
        if self.edit_lines.is_empty() {
            self.edit_lines.push(String::new());
        }
        self.push_undo(EditKind::Other);
        self.delete_selection();
        let line = self.cur_line.min(self.edit_lines.len() - 1);
        let col = self.cur_col.min(self.edit_lines[line].chars().count());
        let b = char_byte(&self.edit_lines[line], col);
        let tail = self.edit_lines[line].split_off(b);
        let mut segs = text.split('\n');
        if let Some(first) = segs.next() {
            self.edit_lines[line].push_str(first);
        }
        let mut cur = line;
        for seg in segs {
            cur += 1;
            self.edit_lines.insert(cur, seg.to_string());
        }
        self.cur_line = cur;
        self.cur_col = self.edit_lines[cur].chars().count();
        self.edit_lines[cur].push_str(&tail);
        self.modified = true;
    }

    /// Copy (or cut) the selection, returning its text. Cut deletes the range
    /// under its own undo unit. None when there's no selection.
    pub(crate) fn take_copy(&mut self, cut: bool) -> Option<String> {
        let text = self.selected_text()?;
        if cut {
            self.push_undo(EditKind::Other);
            self.delete_selection();
        }
        Some(text)
    }
}

impl App {
    /// Insert text at the active markdown editor's cursor (committed Hangul or
    /// pasted text). Multi-char safe; advances the cursor by char count.
    pub(crate) fn md_editor_insert(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        {
            let mut ws = self.ws.lock().unwrap();
            let Some(pane) = ws.active_mut() else { return };
            pane.dirty = true;
            let Some(m) = pane.markdown_mut() else { return };
            m.insert_at_caret(text);
        }
        self.md_ensure_caret_visible();
    }
    /// Raw-editor key entry point with Hangul composition. macOS hands jamo
    /// (U+3130..318F) through `event.text`; we feed the local composer (same as
    /// the terminal path), insert committed syllables, and keep the preedit in
    /// `self.preedit` for the editor overlay. Non-jamo flushes then edits.
    pub(crate) fn md_editor_input(&mut self, event: &KeyEvent) {
        use winit::keyboard::{Key, NamedKey};
        #[cfg(target_os = "macos")]
        if let Some(t) = &event.text {
            if t.chars().count() == 1 {
                if let Some(c) = t.chars().next() {
                    if (0x3130..=0x318F).contains(&(c as u32)) {
                        if let Some(commit) = self.hangul.feed(c) {
                            self.md_editor_insert(&commit);
                        }
                        self.preedit = self.hangul.preedit().unwrap_or_default();
                        self.in_preedit = !self.preedit.is_empty();
                        self.chrome_dirty = true;
                        return;
                    }
                }
            }
        }
        // Mid-composition backspace chips a jamo off the preedit.
        if matches!(event.logical_key, Key::Named(NamedKey::Backspace)) && self.hangul.backspace() {
            self.preedit = self.hangul.preedit().unwrap_or_default();
            self.in_preedit = !self.preedit.is_empty();
            self.chrome_dirty = true;
            return;
        }
        // Any other key: flush the pending syllable into the buffer first.
        if let Some(flushed) = self.hangul.flush() {
            self.md_editor_insert(&flushed);
        }
        self.preedit.clear();
        self.in_preedit = false;
        self.md_editor_key(event);
    }
    /// Handle a keypress in a Raw-mode markdown editor pane: char insert,
    /// backspace/delete, enter (line split), caret motion (arrows, Home/End,
    /// PageUp/Down, Opt+←→ word hops), Shift+motion selection. Hangul
    /// composition is handled by `md_editor_input` before this.
    pub(crate) fn md_editor_key(&mut self, event: &KeyEvent) {
        use winit::keyboard::{Key, NamedKey};
        let shift = self.modifiers.shift_key();
        let alt = self.modifiers.alt_key();
        // PageUp/Down 페이지 줄 수는 gpu 줄높이가 필요해 ws lock 밖에서 계산.
        let page_lines = if matches!(
            event.logical_key,
            Key::Named(NamedKey::PageUp) | Key::Named(NamedKey::PageDown)
        ) {
            self.md_page_lines()
        } else {
            0
        };
        {
            let mut ws = self.ws.lock().unwrap();
            let Some(pane) = ws.active_mut() else { return };
            pane.dirty = true;
            let Some(m) = pane.markdown_mut() else { return };
            m.apply_edit_key(event, shift, alt, page_lines);
        }
        self.md_ensure_caret_visible();
    }
    /// Lines per PageUp/Down step for the active raw editor: the body height
    /// over the gpu line height, minus one line of overlap.
    fn md_page_lines(&mut self) -> usize {
        let id = { self.ws.lock().unwrap().active_pane.clone() };
        let Some(id) = id else { return 1 };
        let Some(&(_, _, _, bh)) = self.md_body_rects.get(&id) else { return 1 };
        let Some(gpu) = self.gpu.as_mut() else { return 1 };
        let lh = gpu.raw_editor_line_h();
        (((bh / lh).floor() as usize).saturating_sub(1)).max(1)
    }
    /// Cmd/Ctrl(host-mod) shortcuts the raw editor owns. Returns true when the
    /// event was consumed here; anything else falls through to the global
    /// shortcut block so Cmd+W/D/T keep working with an editor focused.
    pub(crate) fn md_editor_shortcut(&mut self, event: &KeyEvent) -> bool {
        use winit::keyboard::{KeyCode, PhysicalKey};
        if !self.host_mod() {
            return false;
        }
        let PhysicalKey::Code(code) = event.physical_key else {
            return false;
        };
        match code {
            KeyCode::KeyS => {
                // 조합 중인 한글 음절을 버퍼에 넣고 저장 — 안 그러면 마지막
                // 글자가 파일에서 빠진다.
                if let Some(flushed) = self.hangul.flush() {
                    self.md_editor_insert(&flushed);
                }
                self.preedit.clear();
                self.in_preedit = false;
                self.save_active_editor();
                true
            }
            KeyCode::KeyV => {
                if let Some(flushed) = self.hangul.flush() {
                    self.md_editor_insert(&flushed);
                }
                self.preedit.clear();
                self.in_preedit = false;
                self.md_editor_paste();
                true
            }
            KeyCode::KeyC => {
                self.md_copy_selection(false);
                true
            }
            KeyCode::KeyX => {
                if let Some(flushed) = self.hangul.flush() {
                    self.md_editor_insert(&flushed);
                }
                self.preedit.clear();
                self.in_preedit = false;
                self.md_copy_selection(true);
                true
            }
            KeyCode::KeyA => {
                if let Some(flushed) = self.hangul.flush() {
                    self.md_editor_insert(&flushed);
                }
                self.preedit.clear();
                self.in_preedit = false;
                self.md_select_all();
                true
            }
            KeyCode::KeyZ => {
                // 조합 중이던 음절은 undo 대상 버퍼에 먼저 확정시킨다.
                if let Some(flushed) = self.hangul.flush() {
                    self.md_editor_insert(&flushed);
                }
                self.preedit.clear();
                self.in_preedit = false;
                if self.modifiers.shift_key() {
                    self.md_redo();
                } else {
                    self.md_undo();
                }
                true
            }
            KeyCode::ArrowLeft | KeyCode::ArrowRight | KeyCode::ArrowUp | KeyCode::ArrowDown => {
                // Cmd+←→ = line start/end, Cmd+↑↓ = document start/end;
                // Shift extends the selection like the plain motions.
                self.md_cmd_arrow(code);
                true
            }
            _ => false,
        }
    }
    /// Cmd+arrow jumps for the raw editor (see `md_editor_shortcut`).
    fn md_cmd_arrow(&mut self, code: winit::keyboard::KeyCode) {
        let shift = self.modifiers.shift_key();
        {
            let mut ws = self.ws.lock().unwrap();
            let Some(pane) = ws.active_mut() else { return };
            pane.dirty = true;
            let Some(m) = pane.markdown_mut() else { return };
            m.apply_cmd_arrow(code, shift);
        }
        self.md_ensure_caret_visible();
    }
    /// Cmd+C / Cmd+X on the raw editor. No selection = quietly consumed (the
    /// terminal Cmd+C path must not fire with an editor focused).
    fn md_copy_selection(&mut self, cut: bool) {
        let text = {
            let mut ws = self.ws.lock().unwrap();
            let Some(pane) = ws.active_mut() else { return };
            let Some(m) = pane.markdown_mut() else { return };
            let Some(text) = m.take_copy(cut) else { return };
            if cut {
                pane.dirty = true;
            }
            text
        };
        match arboard::Clipboard::new() {
            Ok(mut cb) => {
                let _ = cb.set_text(text);
            }
            Err(e) => eprintln!("[editor] clipboard open failed: {e}"),
        }
        if cut {
            self.md_ensure_caret_visible();
        }
    }
    /// Cmd+A: anchor at the top, caret at the very end.
    fn md_select_all(&mut self) {
        let mut ws = self.ws.lock().unwrap();
        let Some(pane) = ws.active_mut() else { return };
        pane.dirty = true;
        let Some(m) = pane.markdown_mut() else { return };
        m.select_all_buf();
    }
    /// Cmd+Z: pop the undo stack, stashing the present on the redo stack.
    pub(crate) fn md_undo(&mut self) {
        {
            let mut ws = self.ws.lock().unwrap();
            let Some(pane) = ws.active_mut() else { return };
            let Some(m) = pane.markdown_mut() else { return };
            if m.do_undo() {
                pane.dirty = true;
            }
        }
        self.md_ensure_caret_visible();
    }
    /// Cmd+Shift+Z: inverse of `md_undo`.
    pub(crate) fn md_redo(&mut self) {
        {
            let mut ws = self.ws.lock().unwrap();
            let Some(pane) = ws.active_mut() else { return };
            let Some(m) = pane.markdown_mut() else { return };
            if m.do_redo() {
                pane.dirty = true;
            }
        }
        self.md_ensure_caret_visible();
    }
    /// Scroll the active raw editor so the caret stays in view (both axes).
    /// Called after every edit/motion; the metric math lives in the gpu layer
    /// so it can't drift from `draw_raw_editor`.
    pub(crate) fn md_ensure_caret_visible(&mut self) {
        let snap = {
            let ws = self.ws.lock().unwrap();
            let Some(id) = ws.active_pane.clone() else { return };
            let Some(m) = ws.panes.get(&id).and_then(|p| p.markdown()) else { return };
            if !m.raw_mode {
                return;
            }
            let line = m.cur_line.min(m.edit_lines.len().saturating_sub(1));
            let prefix: String = m
                .edit_lines
                .get(line)
                .map(|l| l.chars().take(m.cur_col).collect())
                .unwrap_or_default();
            (id, m.edit_lines.len(), line, prefix, m.scroll as f32, m.h_scroll)
        };
        let (id, line_count, cur_line, prefix, scroll, h_scroll) = snap;
        let Some(&(_bx, _by, bw, bh)) = self.md_body_rects.get(&id) else { return };
        let Some(gpu) = self.gpu.as_mut() else { return };
        let (ns, nh) =
            gpu.raw_editor_ensure_visible(line_count, cur_line, &prefix, bw, bh, scroll, h_scroll);
        if (ns - scroll).abs() > 0.5 || (nh - h_scroll).abs() > 0.5 {
            let mut ws = self.ws.lock().unwrap();
            if let Some(pane) = ws.panes.get_mut(&id) {
                pane.dirty = true;
                if let Some(m) = pane.markdown_mut() {
                    m.scroll = ns.max(0.0) as usize;
                    m.h_scroll = nh.max(0.0);
                }
            }
        }
    }
    /// Cmd+S: write the active raw-editor buffer back to its file. This is the
    /// only save path for code/text files (the .md Raw→Render toggle is the
    /// other, .md-only one). Returns true when the active pane was a raw
    /// editor, whether or not the write succeeded (the event is consumed).
    pub(crate) fn save_active_editor(&mut self) -> bool {
        let outcome = {
            let mut ws = self.ws.lock().unwrap();
            let Some(pane) = ws.active_mut() else { return false };
            let job = pane.markdown().and_then(|m| {
                m.raw_mode
                    .then(|| (m.edit_lines.join("\n"), m.doc.path.clone()))
            });
            let Some((text, path)) = job else { return false };
            match std::fs::write(&path, &text) {
                Ok(()) => {
                    if let Some(m) = pane.markdown_mut() {
                        m.modified = false;
                    }
                    pane.dirty = true;
                    Ok(path)
                }
                Err(e) => Err((path, e)),
            }
        };
        match outcome {
            Ok(path) => {
                let name = std::path::Path::new(&path)
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or(&path)
                    .to_string();
                self.set_toast(format!("✓ {name} 저장됨"));
            }
            Err((path, e)) => {
                eprintln!("[editor] 저장 실패 {path}: {e}");
                self.set_toast(format!("⚠ 저장 실패: {e}"));
            }
        }
        self.chrome_dirty = true;
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
        true
    }
    /// Cmd+V into the raw editor. `md_editor_insert` is single-line only, so
    /// multi-line clipboard text is spliced manually: first segment joins the
    /// current line, the rest become new lines, and the tail of the original
    /// line reattaches after the last pasted segment.
    pub(crate) fn md_editor_paste(&mut self) {
        let mut cb = match arboard::Clipboard::new() {
            Ok(cb) => cb,
            Err(e) => {
                eprintln!("[editor] clipboard open failed: {e}");
                return;
            }
        };
        let Ok(text) = cb.get_text() else { return };
        if text.is_empty() {
            return;
        }
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        {
            let mut ws = self.ws.lock().unwrap();
            let Some(pane) = ws.active_mut() else { return };
            pane.dirty = true;
            let Some(m) = pane.markdown_mut() else { return };
            m.paste_at_caret(&text);
        }
        self.md_ensure_caret_visible();
    }
    /// Place the raw-editor caret at a click. Reads the pane's body box (stashed
    /// by the renderer), hit-tests the pixel to a (line, col) via the GPU shaper,
    /// then writes it back. No-op unless `id` is a raw-mode markdown pane.
    pub(crate) fn md_click_caret(&mut self, id: &str, px: f32, py: f32) {
        let Some(&(bx, by, _bw, _bh)) = self.md_body_rects.get(id) else { return };
        // Pull the lines + pan out under a short lock so the GPU borrow below
        // doesn't overlap the workspace borrow.
        let snapshot = {
            let ws = self.ws.lock().unwrap();
            ws.panes.get(id).and_then(|p| p.markdown()).and_then(|m| {
                m.raw_mode
                    .then(|| (m.edit_lines.clone(), m.scroll as f32, m.h_scroll))
            })
        };
        let Some((lines, scroll, h_scroll)) = snapshot else { return };
        let Some(gpu) = self.gpu.as_mut() else { return };
        let (line, col) = gpu.raw_editor_caret_at(&lines, bx, by, scroll, h_scroll, px, py);
        let mut ws = self.ws.lock().unwrap();
        if let Some(pane) = ws.panes.get_mut(id) {
            pane.dirty = true;
            if let Some(m) = pane.markdown_mut() {
                m.cur_line = line;
                m.cur_col = col;
                // A click always breaks a typing/deleting coalesce run.
                m.last_edit = EditKind::Break;
            }
        }
    }
    /// Set a markdown pane's view mode from the header "Rendered | Raw" toggle.
    /// No-op if already in `want_raw`. Render → Raw seeds the edit buffer from
    /// the doc source; Raw → Render writes the buffer back to disk and re-parses
    /// so the laid-out view reflects the edits.
    pub(crate) fn set_md_mode(&mut self, id: &str, want_raw: bool) {
        let mut ws = self.ws.lock().unwrap();
        let Some(pane) = ws.panes.get_mut(id) else { return };
        let is_raw = pane.markdown().map_or(false, |m| m.raw_mode);
        if is_raw == want_raw {
            return;
        }
        pane.dirty = true;
        if is_raw {
            // Raw → Render: persist the edits first, then re-parse.
            let save = pane
                .markdown()
                .map(|m| (m.edit_lines.join("\n"), m.doc.path.clone()));
            if let Some((text, path)) = save {
                let _ = std::fs::write(&path, &text);
                let doc = build_markdown_doc(id, std::path::Path::new(&path), &text);
                if let Some(m) = pane.markdown_mut() {
                    m.doc = Arc::new(doc);
                    m.scroll = 0;
                    m.raw_mode = false;
                    m.modified = false;
                }
            }
        } else if let Some(m) = pane.markdown_mut() {
            // Render → Raw: seed the edit buffer from the source.
            m.edit_lines = m.doc.raw.split('\n').map(String::from).collect();
            if m.edit_lines.is_empty() {
                m.edit_lines.push(String::new());
            }
            m.cur_line = 0;
            m.cur_col = 0;
            m.scroll = 0;
            m.raw_mode = true;
        }
    }

    /// Directory of the active markdown pane's source file, for resolving
    /// relative link destinations.
    pub(crate) fn active_markdown_dir(&self) -> Option<std::path::PathBuf> {
        let ws = self.ws.lock().unwrap();
        let active = ws.active_pane.as_ref()?;
        let md = ws.panes.get(active)?.markdown()?;
        std::path::Path::new(&md.doc.path)
            .parent()
            .map(|d| d.to_path_buf())
    }
    /// Hit-test the cursor against the markdown code-block copy buttons; copy
    /// the block's text if one is under it. Returns true if a copy happened.
    pub(crate) fn try_copy_md_block(&mut self) -> bool {
        let (cx, cy) = self.cursor_px;
        let code = {
            let Some(g) = self.gpu.as_ref() else { return false };
            g.md_copy_rects
                .iter()
                .find(|(x, y, w, h, _)| cx >= *x && cx <= *x + *w && cy >= *y && cy <= *y + *h)
                .map(|(_, _, _, _, c)| c.clone())
        };
        match code {
            Some(c) => {
                self.copy_block_text(&c);
                true
            }
            None => false,
        }
    }
    /// Hit-test the cursor against the link rects the renderer recorded for
    /// the last markdown frame; open the destination if one is under it.
    /// Returns true if a link was opened (so the caller skips other handling).
    pub(crate) fn try_open_md_link(&self) -> bool {
        let (cx, cy) = self.cursor_px;
        let Some(g) = self.gpu.as_ref() else { return false };
        let dest = g
            .md_link_rects
            .iter()
            .find(|(x, y, w, h, _)| cx >= *x && cx <= *x + *w && cy >= *y && cy <= *y + *h)
            .map(|(_, _, _, _, d)| d.clone());
        match dest {
            Some(d) => {
                self.open_md_dest(&d);
                true
            }
            None => false,
        }
    }
    /// Open a markdown link destination: http(s)/mailto go to the default
    /// app (browser/mail); a local path is revealed in Finder (`open -R`),
    /// resolving relative paths against the markdown file's directory.
    pub(crate) fn open_md_dest(&self, dest: &str) {
        if dest.starts_with("http://")
            || dest.starts_with("https://")
            || dest.starts_with("mailto:")
        {
            let _ = crate::proc::command("open").arg(dest).spawn();
            return;
        }
        let raw = dest.strip_prefix("file://").unwrap_or(dest);
        let mut path = std::path::PathBuf::from(raw);
        if path.is_relative() {
            if let Some(dir) = self.active_markdown_dir() {
                path = dir.join(raw);
            }
        }
        if path.exists() {
            let _ = crate::proc::command("open").arg("-R").arg(&path).spawn();
        } else {
            // Unknown scheme or missing file — let the OS try to interpret it.
            let _ = crate::proc::command("open").arg(dest).spawn();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pane(lines: &[&str]) -> MarkdownPane {
        MarkdownPane {
            doc: Arc::new(build_markdown_doc("%t", std::path::Path::new("/tmp/t.rs"), "")),
            is_md_doc: false,
            raw_mode: true,
            edit_lines: lines.iter().map(|s| s.to_string()).collect(),
            cur_line: 0,
            cur_col: 0,
            scroll: 0,
            h_scroll: 0.0,
            modified: false,
            sel_anchor: None,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            last_edit: EditKind::Break,
        }
    }

    #[test]
    fn word_motion_boundaries() {
        let chars: Vec<char> = "foo bar_baz  += qux".chars().collect();
        // next: from 0 lands after "foo"; from there skips space through "bar_baz".
        assert_eq!(next_word_col(&chars, 0), 3);
        assert_eq!(next_word_col(&chars, 3), 11);
        // "+=" is a symbol run — its own hop.
        assert_eq!(next_word_col(&chars, 11), 15);
        assert_eq!(next_word_col(&chars, 15), 19);
        assert_eq!(next_word_col(&chars, 19), 19);
        // prev mirrors.
        assert_eq!(prev_word_col(&chars, 19), 16);
        assert_eq!(prev_word_col(&chars, 16), 13);
        assert_eq!(prev_word_col(&chars, 13), 4);
        assert_eq!(prev_word_col(&chars, 4), 0);
        assert_eq!(prev_word_col(&chars, 0), 0);
    }

    #[test]
    fn sel_range_normalizes_and_clamps() {
        let mut m = pane(&["hello", "world"]);
        // Backward selection (anchor after cursor) normalizes.
        m.sel_anchor = Some((1, 3));
        m.cur_line = 0;
        m.cur_col = 2;
        assert_eq!(m.sel_range(), Some(((0, 2), (1, 3))));
        // Empty selection reads as none.
        m.cur_line = 1;
        m.cur_col = 3;
        assert_eq!(m.sel_range(), None);
        // Stale out-of-range anchor clamps to the buffer.
        m.sel_anchor = Some((9, 99));
        m.cur_line = 0;
        m.cur_col = 0;
        assert_eq!(m.sel_range(), Some(((0, 0), (1, 5))));
    }

    #[test]
    fn selected_text_multiline() {
        let mut m = pane(&["hello", "mid", "world"]);
        m.sel_anchor = Some((0, 3));
        m.cur_line = 2;
        m.cur_col = 2;
        assert_eq!(m.selected_text().as_deref(), Some("lo\nmid\nwo"));
    }

    #[test]
    fn delete_selection_joins_lines() {
        let mut m = pane(&["hello", "mid", "world"]);
        m.sel_anchor = Some((0, 3));
        m.cur_line = 2;
        m.cur_col = 2;
        assert!(m.delete_selection());
        assert_eq!(m.edit_lines, vec!["helrld".to_string()]);
        assert_eq!((m.cur_line, m.cur_col), (0, 3));
        assert_eq!(m.sel_anchor, None);
        assert!(m.modified);
        // Single-line case.
        let mut m = pane(&["hangul 한글 tail"]);
        m.sel_anchor = Some((0, 7));
        m.cur_col = 9;
        assert!(m.delete_selection());
        assert_eq!(m.edit_lines, vec!["hangul  tail".to_string()]);
    }

    #[test]
    fn undo_coalesces_typing_runs() {
        let mut m = pane(&["ab"]);
        // Two same-kind pushes in a row = one snapshot (a typing run).
        m.push_undo(EditKind::Typing);
        m.edit_lines[0].push('c');
        m.push_undo(EditKind::Typing);
        m.edit_lines[0].push('d');
        assert_eq!(m.undo_stack.len(), 1);
        // A break (caret move) reopens the boundary.
        m.last_edit = EditKind::Break;
        m.push_undo(EditKind::Typing);
        assert_eq!(m.undo_stack.len(), 2);
        // Other always pushes, even back-to-back.
        m.push_undo(EditKind::Other);
        m.push_undo(EditKind::Other);
        assert_eq!(m.undo_stack.len(), 4);
    }

    #[test]
    fn undo_snapshot_roundtrip() {
        let mut m = pane(&["one"]);
        m.cur_col = 3;
        m.push_undo(EditKind::Other);
        m.edit_lines = vec!["two".into(), "three".into()];
        m.cur_line = 1;
        m.cur_col = 5;
        let before = m.snapshot();
        let snap = m.undo_stack.pop().unwrap();
        m.redo_stack.push(before);
        m.apply_snapshot(snap);
        assert_eq!(m.edit_lines, vec!["one".to_string()]);
        assert_eq!((m.cur_line, m.cur_col), (0, 3));
        assert_eq!(m.last_edit, EditKind::Break);
        // Redo restores the edited state.
        let snap = m.redo_stack.pop().unwrap();
        m.apply_snapshot(snap);
        assert_eq!(m.edit_lines, vec!["two".to_string(), "three".to_string()]);
        assert_eq!((m.cur_line, m.cur_col), (1, 5));
    }

    #[test]
    fn undo_cap_bounds_stack() {
        let mut m = pane(&["x"]);
        for i in 0..(UNDO_CAP + 20) {
            m.push_undo(EditKind::Other);
            m.edit_lines[0] = format!("{i}");
        }
        assert_eq!(m.undo_stack.len(), UNDO_CAP);
        // Oldest entries fell off the bottom — the floor moved up by 20.
        assert_eq!(m.undo_stack[0].lines[0], "19");
    }

    // ── Pure-core methods shared with the pop-out editor window (auxwin.rs) ──

    #[test]
    fn insert_at_caret_advances_and_replaces_selection() {
        let mut m = pane(&["hello"]);
        m.cur_col = 5;
        m.insert_at_caret("!");
        assert_eq!(m.edit_lines, vec!["hello!".to_string()]);
        assert_eq!(m.cur_col, 6);
        assert!(m.modified);
        // Insert over a selection replaces the range.
        let mut m = pane(&["hello world"]);
        m.sel_anchor = Some((0, 0));
        m.cur_col = 5; // select "hello"
        m.insert_at_caret("bye");
        assert_eq!(m.edit_lines, vec!["bye world".to_string()]);
        assert_eq!((m.cur_line, m.cur_col), (0, 3));
    }

    #[test]
    fn paste_at_caret_splices_multiline() {
        let mut m = pane(&["abcd"]);
        m.cur_col = 2; // caret between b and c
        m.paste_at_caret("X\nY");
        assert_eq!(m.edit_lines, vec!["abX".to_string(), "Ycd".to_string()]);
        assert_eq!((m.cur_line, m.cur_col), (1, 1));
    }

    #[test]
    fn do_undo_redo_roundtrip() {
        let mut m = pane(&["a"]);
        m.cur_col = 1;
        m.insert_at_caret("b"); // "ab"
        assert_eq!(m.edit_lines, vec!["ab".to_string()]);
        assert!(m.do_undo());
        assert_eq!(m.edit_lines, vec!["a".to_string()]);
        assert!(m.do_redo());
        assert_eq!(m.edit_lines, vec!["ab".to_string()]);
        // Empty stacks return false.
        assert!(m.do_undo());
        assert!(!m.do_undo());
    }

    #[test]
    fn select_all_and_take_copy_cut() {
        let mut m = pane(&["one", "two"]);
        m.select_all_buf();
        assert_eq!(m.sel_anchor, Some((0, 0)));
        assert_eq!((m.cur_line, m.cur_col), (1, 3));
        // Copy leaves the buffer; cut removes the selection.
        assert_eq!(m.take_copy(false).as_deref(), Some("one\ntwo"));
        assert_eq!(m.edit_lines, vec!["one".to_string(), "two".to_string()]);
        m.select_all_buf();
        assert_eq!(m.take_copy(true).as_deref(), Some("one\ntwo"));
        assert_eq!(m.edit_lines, vec![String::new()]);
        // No selection = None.
        assert_eq!(m.take_copy(false), None);
    }

    #[test]
    fn ensure_raw_seeded_from_doc_source() {
        // A render-mode .md pane (empty edit buffer) seeds from doc.raw on pop-out.
        let doc = Arc::new(build_markdown_doc(
            "%t",
            std::path::Path::new("/tmp/t.md"),
            "line1\nline2",
        ));
        let mut m = MarkdownPane {
            doc,
            is_md_doc: true,
            raw_mode: false,
            edit_lines: Vec::new(),
            cur_line: 0,
            cur_col: 0,
            scroll: 0,
            h_scroll: 0.0,
            modified: false,
            sel_anchor: None,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            last_edit: EditKind::Break,
        };
        m.ensure_raw_seeded();
        assert!(m.raw_mode);
        assert_eq!(m.edit_lines, vec!["line1".to_string(), "line2".to_string()]);
        // Already-seeded buffers are left untouched.
        let mut m2 = pane(&["kept"]);
        m2.ensure_raw_seeded();
        assert_eq!(m2.edit_lines, vec!["kept".to_string()]);
    }
}
