//! 마크다운 pane 에디터 입력 + md 링크/블록 열기·복사.
use super::*;

impl App {
    /// Insert text at the active markdown editor's cursor (committed Hangul or
    /// pasted text). Multi-char safe; advances the cursor by char count.
    pub(crate) fn md_editor_insert(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        let mut ws = self.ws.lock().unwrap();
        let Some(pane) = ws.active_mut() else { return };
        pane.dirty = true;
        let Some(m) = pane.markdown_mut() else { return };
        if m.edit_lines.is_empty() {
            m.edit_lines.push(String::new());
        }
        let line = m.cur_line.min(m.edit_lines.len() - 1);
        let col = m.cur_col;
        let s = &mut m.edit_lines[line];
        let b = char_byte(s, col);
        s.insert_str(b, text);
        m.cur_line = line;
        m.cur_col = col + text.chars().count();
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
    /// backspace, enter (line split), arrow navigation. Hangul composition is
    /// handled by `md_editor_input` before this. Edits the active pane buffer.
    pub(crate) fn md_editor_key(&mut self, event: &KeyEvent) {
        use winit::keyboard::{Key, NamedKey};
        let mut ws = self.ws.lock().unwrap();
        let Some(pane) = ws.active_mut() else { return };
        pane.dirty = true;
        let Some(m) = pane.markdown_mut() else { return };
        if m.edit_lines.is_empty() {
            m.edit_lines.push(String::new());
        }
        let mut line = m.cur_line.min(m.edit_lines.len() - 1);
        let mut col = m.cur_col;
        match &event.logical_key {
            Key::Named(NamedKey::Backspace) => {
                if col > 0 {
                    let s = &mut m.edit_lines[line];
                    let b0 = char_byte(s, col - 1);
                    let b1 = char_byte(s, col);
                    s.replace_range(b0..b1, "");
                    col -= 1;
                } else if line > 0 {
                    let cur = m.edit_lines.remove(line);
                    line -= 1;
                    col = m.edit_lines[line].chars().count();
                    m.edit_lines[line].push_str(&cur);
                }
            }
            Key::Named(NamedKey::Enter) => {
                let s = &mut m.edit_lines[line];
                let b = char_byte(s, col);
                let rest = s.split_off(b);
                m.edit_lines.insert(line + 1, rest);
                line += 1;
                col = 0;
            }
            Key::Named(NamedKey::ArrowLeft) => {
                if col > 0 {
                    col -= 1;
                } else if line > 0 {
                    line -= 1;
                    col = m.edit_lines[line].chars().count();
                }
            }
            Key::Named(NamedKey::ArrowRight) => {
                let len = m.edit_lines[line].chars().count();
                if col < len {
                    col += 1;
                } else if line + 1 < m.edit_lines.len() {
                    line += 1;
                    col = 0;
                }
            }
            Key::Named(NamedKey::ArrowUp) => {
                if line > 0 {
                    line -= 1;
                    col = col.min(m.edit_lines[line].chars().count());
                }
            }
            Key::Named(NamedKey::ArrowDown) => {
                if line + 1 < m.edit_lines.len() {
                    line += 1;
                    col = col.min(m.edit_lines[line].chars().count());
                }
            }
            Key::Named(NamedKey::Space) => {
                let s = &mut m.edit_lines[line];
                let b = char_byte(s, col);
                s.insert(b, ' ');
                col += 1;
            }
            Key::Character(txt) => {
                let s = &mut m.edit_lines[line];
                let b = char_byte(s, col);
                s.insert_str(b, txt);
                col += txt.chars().count();
            }
            _ => {}
        }
        m.cur_line = line;
        m.cur_col = col;
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
