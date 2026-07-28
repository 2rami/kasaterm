//! 마크다운 pane 에디터 입력 + md 링크/블록 열기·복사.
use super::*;

/// Undo history depth per editor pane. Whole-buffer snapshots, so this also
/// bounds memory (~cap × file size worst case).
pub(crate) const UNDO_CAP: usize = 100;

/// One indent step. Spaces, not a tab: nested list markers only line up under
/// their parent by column count, and a tab's width is the viewer's opinion.
const INDENT: &str = "  ";

/// Write a text file **atomically** — sibling temp file, fsync, rename.
///
/// `fs::write` truncates the destination first, so anything that interrupts the
/// write (crash, power loss, a full disk) leaves the user's document half
/// erased. `rename(2)` within one filesystem is atomic: whatever happens, the
/// path holds either the whole old file or the whole new one. `sync_all` before
/// the rename is the power-loss half — a rename can otherwise land while the
/// bytes are still only in the page cache, making an empty file the survivor.
///
/// Two things this deliberately does beyond `write_session_state`, because
/// here the destination is the **user's own file** and not our scratch state:
///
/// - **Symlinks are followed.** Renaming onto a symlink would replace the link
///   itself with a regular file — a real way to quietly break a dotfile farm.
///   The temp file is created next to the *resolved* target.
/// - **Permissions are carried over.** A fresh temp file is 0644 minus umask,
///   so without this a 0600 note would come back world-readable.
///
/// Hard links do break (the target gets a new inode). That is inherent to the
/// rename approach and the trade every editor with a safe-write mode makes.
pub(crate) fn write_atomic(path: &str, text: &str) -> std::io::Result<()> {
    use std::io::{Error, ErrorKind, Write};
    let path = std::fs::canonicalize(path).unwrap_or_else(|_| std::path::PathBuf::from(path));
    let dir = path
        .parent()
        .ok_or_else(|| Error::new(ErrorKind::InvalidInput, "부모 디렉터리 없음"))?;
    let stem = path
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| Error::new(ErrorKind::InvalidInput, "파일 이름 없음"))?;
    // 같은 디렉터리여야 rename 이 원자적이다(파일시스템을 넘으면 복사가 된다).
    // pid 를 붙여 두 창이 같은 파일을 저장해도 서로의 임시파일을 안 밟는다.
    let tmp = dir.join(format!(".{stem}.kasaterm-{}.tmp", std::process::id()));
    let mode = std::fs::metadata(&path).ok().map(|m| {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            m.permissions().mode()
        }
        #[cfg(not(unix))]
        {
            let _ = &m;
            0u32
        }
    });
    // 중간에 실패하면 임시파일을 치우고 나간다 — 목적지는 손대지 않았으니
    // 사용자 파일은 그대로다.
    let write = || -> std::io::Result<()> {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(text.as_bytes())?;
        f.sync_all()?;
        drop(f);
        #[cfg(unix)]
        if let Some(mode) = mode {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(mode))?;
        }
        #[cfg(not(unix))]
        let _ = mode;
        std::fs::rename(&tmp, &path)
    };
    write().inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

/// Would this key change the buffer if the pane were in Raw mode? Rendered
/// mode uses it to decide whether a keypress means "I want to edit" (switch to
/// Raw and keep the key) or just navigation (leave the view alone).
///
/// Kept in step with `apply_edit_key`'s mutating arms — a key that edits there
/// but is missing here gets silently eaten in Rendered mode, which is exactly
/// the behaviour this replaces.
pub(crate) fn md_mutating_key(event: &KeyEvent) -> bool {
    use winit::keyboard::{Key, NamedKey};
    matches!(
        &event.logical_key,
        Key::Character(_)
            | Key::Named(
                NamedKey::Space
                    | NamedKey::Enter
                    | NamedKey::Tab
                    | NamedKey::Backspace
                    | NamedKey::Delete
            )
    )
}

/// One line split into "leading whitespace + list/quote marker" and the rest.
/// Enter uses it to carry the prefix onto the next line; Tab uses `list` to
/// decide whether to indent the whole item or just insert spaces at the caret.
pub(crate) struct LinePrefix {
    /// Prefix length in chars (indent + marker). A caret before this sits
    /// inside the marker, where continuing it would be wrong.
    pub len: usize,
    /// What the next line starts with — indent, plus the marker when there is
    /// one (ordered markers increment, task markers reset to unchecked).
    pub next: String,
    /// A quote or list marker was present.
    pub marker: bool,
    /// …and it was a list (bullet or ordered), not a quote.
    pub list: bool,
}

/// Split a line into its continuation prefix. Recognizes `- `/`* `/`+ `,
/// `1. `/`1) `, task items (`- [ ] `), and `> ` quotes — the markers this
/// repo's documents actually use. A quote's contents aren't re-scanned for a
/// nested bullet; that nesting is rare enough that guessing wrong costs more
/// than not guessing.
pub(crate) fn line_prefix(line: &str) -> LinePrefix {
    let indent: String = line.chars().take_while(|c| *c == ' ' || *c == '\t').collect();
    let rest = &line[indent.len()..];
    let ind_len = indent.chars().count();
    let plain = LinePrefix { len: ind_len, next: indent.clone(), marker: false, list: false };

    if rest.starts_with('>') {
        let q: String = rest.chars().take_while(|c| *c == '>' || *c == ' ').collect();
        // `>` 뒤 공백이 없으면 붙여 준다 — 이어 쓴 줄이 `>foo` 로 나오면
        // 파서에 따라 인용으로 안 잡힌다.
        let norm = if q.ends_with(' ') { q.clone() } else { format!("{q} ") };
        return LinePrefix {
            len: ind_len + q.chars().count(),
            next: format!("{indent}{norm}"),
            marker: true,
            list: false,
        };
    }

    let bullet = rest.chars().next().filter(|c| matches!(c, '-' | '*' | '+'));
    if let Some(b) = bullet {
        if rest[1..].starts_with(' ') {
            let after = &rest[2..];
            // 체크박스는 이어 쓸 때 비운다 — 완료 표시까지 물려받으면
            // 새 항목이 처음부터 끝난 것으로 보인다.
            let task = ["[ ] ", "[x] ", "[X] "].iter().any(|t| after.starts_with(t));
            let mark = if task { format!("{b} [ ] ") } else { format!("{b} ") };
            return LinePrefix {
                len: ind_len + if task { 6 } else { 2 },
                next: format!("{indent}{mark}"),
                marker: true,
                list: true,
            };
        }
        return plain;
    }

    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    if !digits.is_empty() {
        let tail = &rest[digits.len()..];
        let sep = tail.chars().next();
        if matches!(sep, Some('.') | Some(')')) && tail[1..].starts_with(' ') {
            let n: u64 = digits.parse().unwrap_or(0);
            return LinePrefix {
                len: ind_len + digits.len() + 2,
                next: format!("{indent}{}{} ", n + 1, sep.unwrap()),
                marker: true,
                list: true,
            };
        }
    }
    plain
}

/// Every match of `query` in `lines` as (line, start col, end col) in chars,
/// document order, non-overlapping. Smart case: an all-lowercase query ignores
/// case, one with any uppercase matches exactly — the ripgrep/Vim rule, which
/// buys the useful half of a case toggle without a button to find.
///
/// Comparison is char-wise rather than on a lowercased copy of the line:
/// `to_lowercase` can change a string's length, which would slide every column
/// after it and land the highlight on the wrong glyph.
pub(crate) fn find_hits(lines: &[String], query: &str) -> Vec<(usize, usize, usize)> {
    let q: Vec<char> = query.chars().collect();
    if q.is_empty() {
        return Vec::new();
    }
    let cased = q.iter().any(|c| c.is_uppercase());
    let eq = |a: char, b: char| {
        if cased {
            a == b
        } else {
            a.to_lowercase().eq(b.to_lowercase())
        }
    };
    let mut out = Vec::new();
    for (li, line) in lines.iter().enumerate() {
        let cs: Vec<char> = line.chars().collect();
        let mut i = 0;
        while i + q.len() <= cs.len() {
            if (0..q.len()).all(|k| eq(cs[i + k], q[k])) {
                out.push((li, i, i + q.len()));
                i += q.len();
            } else {
                i += 1;
            }
        }
    }
    out
}

/// Chars one outdent step removes from the head of `line`: a tab, or up to one
/// `INDENT` worth of spaces. Markdown written elsewhere may use tabs, and
/// Shift+Tab should still bite on those lines.
fn outdent_width(line: &str) -> usize {
    if line.starts_with('\t') {
        1
    } else {
        line.chars().take(INDENT.len()).take_while(|c| *c == ' ').count()
    }
}

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
            let line = &mut self.lines_mut()[s.0];
            let b0 = char_byte(line, s.1);
            let b1 = char_byte(line, e.1);
            line.replace_range(b0..b1, "");
        } else {
            let tail = {
                let last = &self.edit_lines[e.0];
                last[char_byte(last, e.1)..].to_string()
            };
            let keep = char_byte(&self.edit_lines[s.0], s.1);
            self.lines_mut()[s.0].truncate(keep);
            self.lines_mut()[s.0].push_str(&tail);
            self.lines_mut().drain(s.0 + 1..=e.0);
        }
        self.cur_line = s.0;
        self.cur_col = s.1;
        self.sel_anchor = None;
        self.touch();
        true
    }
    /// The buffer, mutably. Every write goes through here so the copy-on-write
    /// is in one place: `make_mut` clones the lines only while an undo snapshot
    /// or an in-flight frame still points at them, which is once per edit run.
    pub(crate) fn lines_mut(&mut self) -> &mut Vec<String> {
        Arc::make_mut(&mut self.edit_lines)
    }
    /// O(1) now that the buffer is shared — this used to deep-copy the file.
    fn snapshot(&self) -> EditSnapshot {
        EditSnapshot { lines: Arc::clone(&self.edit_lines), cur: (self.cur_line, self.cur_col) }
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
            self.lines_mut().push(String::new());
        }
        self.cur_line = snap.cur.0.min(self.edit_lines.len() - 1);
        self.cur_col = snap.cur.1.min(self.edit_lines[self.cur_line].chars().count());
        self.sel_anchor = None;
        self.last_edit = EditKind::Break;
        self.touch();
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
            self.edit_lines = Arc::new(self.doc.raw.split('\n').map(String::from).collect());
            if self.edit_lines.is_empty() {
                self.lines_mut().push(String::new());
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
            self.lines_mut().push(String::new());
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
        let s = &mut self.lines_mut()[line];
        let b = char_byte(s, col);
        s.insert_str(b, text);
        self.cur_line = line;
        self.cur_col = col + text.chars().count();
        self.touch();
    }

    /// Move one line by a single indent step; returns the caret-column delta
    /// for any caret sitting on it.
    fn shift_line(&mut self, li: usize, outdent: bool) -> i64 {
        let l = &mut self.lines_mut()[li];
        if outdent {
            let w = outdent_width(l);
            let b = char_byte(l, w);
            l.replace_range(..b, "");
            -(w as i64)
        } else if l.is_empty() {
            // 빈 줄엔 공백을 심지 않는다 — 화면에 안 보이는 잡티가 diff 에만
            // 남는다.
            0
        } else {
            l.insert_str(0, INDENT);
            INDENT.len() as i64
        }
    }

    /// Tab / Shift+Tab. With a selection every line it touches moves one step
    /// (line-wise, like VS Code) and the selection keeps covering the same
    /// text. Without one, Shift+Tab outdents the caret's line; Tab indents the
    /// whole line when it's a list item — that's how a bullet nests — and
    /// otherwise inserts a step at the caret.
    pub(crate) fn indent(&mut self, outdent: bool) {
        if self.edit_lines.is_empty() {
            self.lines_mut().push(String::new());
        }
        if let Some((s, e)) = self.sel_range() {
            self.push_undo(EditKind::Other);
            let deltas: Vec<i64> = (s.0..=e.0).map(|li| self.shift_line(li, outdent)).collect();
            let fix = |(l, c): (usize, usize)| -> (usize, usize) {
                let Some(d) = l.checked_sub(s.0).and_then(|i| deltas.get(i)) else {
                    return (l, c);
                };
                (l, (c as i64 + d).max(0) as usize)
            };
            if let Some(a) = self.sel_anchor {
                self.sel_anchor = Some(fix(a));
            }
            (self.cur_line, self.cur_col) = fix((self.cur_line, self.cur_col));
        } else {
            let line = self.cur_line.min(self.edit_lines.len() - 1);
            self.push_undo(EditKind::Other);
            if outdent || line_prefix(&self.edit_lines[line]).list {
                let d = self.shift_line(line, outdent);
                self.cur_col = (self.cur_col as i64 + d).max(0) as usize;
            } else {
                let col = self.cur_col.min(self.edit_lines[line].chars().count());
                let s = &mut self.lines_mut()[line];
                let b = char_byte(s, col);
                s.insert_str(b, INDENT);
                self.cur_col = col + INDENT.len();
            }
            self.cur_line = line;
        }
        self.touch();
    }

    /// Enter — split the line, carrying the indent and list/quote marker onto
    /// the new one. On an item holding nothing but its marker, Enter wipes the
    /// marker instead of stacking another empty one; that's how a list ends
    /// without reaching for Backspace (VS Code and Obsidian both do this).
    pub(crate) fn newline(&mut self) {
        if self.edit_lines.is_empty() {
            self.lines_mut().push(String::new());
        }
        self.push_undo(EditKind::Other);
        self.delete_selection();
        let line = self.cur_line.min(self.edit_lines.len() - 1);
        let col = self.cur_col.min(self.edit_lines[line].chars().count());
        let p = line_prefix(&self.edit_lines[line]);
        // 캐럿이 마커 안에 있으면(col < len) 이어쓰기를 하지 않는다 — 그 자리의
        // Enter 는 "이 항목 위에 빈 줄" 이라는 뜻이지 새 항목이 아니다.
        let at_body = col >= p.len;
        if p.marker && at_body && self.edit_lines[line].chars().skip(p.len).all(char::is_whitespace)
        {
            self.lines_mut()[line].clear();
            self.cur_line = line;
            self.cur_col = 0;
            self.touch();
            return;
        }
        let carry = if at_body { p.next } else { String::new() };
        let s = &mut self.lines_mut()[line];
        let b = char_byte(s, col);
        let mut rest = s.split_off(b);
        rest.insert_str(0, &carry);
        self.lines_mut().insert(line + 1, rest);
        self.cur_line = line + 1;
        self.cur_col = carry.chars().count();
        self.touch();
    }

    // ── Find / replace bar. Open == it owns typing, so the buffer can only
    // change under it through replace; that's why the hit list is a cache
    // refreshed at the few points that can invalidate it.

    /// Buffer changed: raise the unsaved dot and restart the autosave quiet
    /// period. Every mutation goes through here so no edit can slip past
    /// autosave or the close guard by forgetting one of the two flags.
    pub(crate) fn touch(&mut self) {
        self.modified = true;
        self.edited_at = Some(Instant::now());
    }

    /// Buffer matches disk again.
    pub(crate) fn mark_saved(&mut self) {
        self.modified = false;
        self.edited_at = None;
    }

    /// Open the bar, or expand it to replace if it's already open. The query
    /// seeds from the selection — "find what I just highlighted" is the common
    /// case, and re-pressing Cmd+F on a new selection re-seeds it.
    pub(crate) fn find_open(&mut self, replacing: bool) {
        let seed = self.selected_text().filter(|s| !s.contains('\n'));
        let f = self.find.get_or_insert_with(|| FindState {
            query: String::new(),
            replace: String::new(),
            replacing: false,
            focus_replace: false,
            hits: Vec::new(),
            idx: 0,
        });
        if let Some(s) = seed {
            f.query = s;
        }
        f.replacing |= replacing;
        f.focus_replace = replacing && !f.query.is_empty();
        self.find_refresh(true);
    }

    /// Close the bar and hand typing back to the buffer.
    pub(crate) fn find_close(&mut self) {
        self.find = None;
        self.sel_anchor = None;
    }

    /// Rebuild the hit list. `seek` parks the highlight on the first match at
    /// or after the caret (a fresh query starts from where you are, not from
    /// the top of the file); otherwise the current index is only clamped, so a
    /// replace doesn't throw you back to match one.
    pub(crate) fn find_refresh(&mut self, seek: bool) {
        let Some(f) = self.find.as_mut() else { return };
        f.hits = find_hits(&self.edit_lines, &f.query);
        if f.hits.is_empty() {
            f.idx = 0;
            return;
        }
        if seek {
            let from = (self.cur_line, self.cur_col);
            f.idx = f.hits.iter().position(|h| (h.0, h.1) >= from).unwrap_or(0);
        } else {
            f.idx = f.idx.min(f.hits.len() - 1);
        }
        self.find_reveal();
    }

    /// Put the caret on the highlighted match and select it, so Esc leaves you
    /// exactly where the search landed.
    fn find_reveal(&mut self) {
        let Some(&(l, c0, c1)) = self.find.as_ref().and_then(|f| f.hits.get(f.idx)) else {
            return;
        };
        self.sel_anchor = Some((l, c0));
        self.cur_line = l;
        self.cur_col = c1;
    }

    /// Next (or previous) match, wrapping at the ends.
    pub(crate) fn find_step(&mut self, back: bool) {
        let Some(f) = self.find.as_mut() else { return };
        let n = f.hits.len();
        if n == 0 {
            return;
        }
        f.idx = if back { (f.idx + n - 1) % n } else { (f.idx + 1) % n };
        self.find_reveal();
    }

    /// Replace the highlighted match, then land on the next one.
    pub(crate) fn find_replace_one(&mut self) {
        let Some(f) = self.find.as_ref() else { return };
        let Some(&(l, c0, c1)) = f.hits.get(f.idx) else { return };
        let with = f.replace.clone();
        self.push_undo(EditKind::Other);
        let s = &mut self.lines_mut()[l];
        let (b0, b1) = (char_byte(s, c0), char_byte(s, c1));
        s.replace_range(b0..b1, &with);
        self.cur_line = l;
        self.cur_col = c0 + with.chars().count();
        self.sel_anchor = None;
        self.touch();
        // 바꾼 글자가 새 검색어와 겹칠 수 있으니(`a`→`aa`) 목록을 다시 만들고,
        // 캐럿 뒤 첫 매치로 간다 — 안 그러면 방금 넣은 글자를 또 바꾼다.
        self.find_refresh(true);
    }

    /// Replace every match as one undo unit. Returns how many.
    pub(crate) fn find_replace_all(&mut self) -> usize {
        let Some(f) = self.find.as_ref() else { return 0 };
        let hits = f.hits.clone();
        let with = f.replace.clone();
        if hits.is_empty() {
            return 0;
        }
        self.push_undo(EditKind::Other);
        // 뒤에서부터 — 앞을 먼저 바꾸면 같은 줄 뒤쪽 열 번호가 전부 밀린다.
        for &(l, c0, c1) in hits.iter().rev() {
            let s = &mut self.lines_mut()[l];
            let (b0, b1) = (char_byte(s, c0), char_byte(s, c1));
            s.replace_range(b0..b1, &with);
        }
        let (l, c0, _) = hits[0];
        self.cur_line = l;
        self.cur_col = c0;
        self.sel_anchor = None;
        self.touch();
        self.find_refresh(true);
        hits.len()
    }

    /// Type into the focused field (a committed Hangul syllable arrives here
    /// too, which is why it isn't folded into the key handler).
    pub(crate) fn find_type(&mut self, text: &str) {
        let Some(f) = self.find.as_mut() else { return };
        if f.focus_replace {
            f.replace.push_str(text);
            return;
        }
        f.query.push_str(text);
        self.find_refresh(true);
    }

    /// One key for the open bar. Returns false when the key isn't the bar's —
    /// the caller swallows it rather than letting it reach the buffer, since
    /// the bar holds focus.
    pub(crate) fn find_key(&mut self, event: &KeyEvent, shift: bool) -> bool {
        use winit::keyboard::{Key, NamedKey};
        let Some(f) = self.find.as_mut() else { return false };
        match &event.logical_key {
            Key::Named(NamedKey::Escape) => self.find_close(),
            Key::Named(NamedKey::Enter) => {
                if f.focus_replace && !shift {
                    self.find_replace_one();
                } else {
                    self.find_step(shift);
                }
            }
            Key::Named(NamedKey::Tab) => {
                if f.replacing {
                    f.focus_replace = !f.focus_replace;
                }
            }
            // 칸 안에서 캐럿을 옮기는 편집은 없으니(끝에 붙이고 지우는 게 전부)
            // 위아래는 결과 이동에 준다 — 삼키면 죽은 키가 된다.
            Key::Named(NamedKey::ArrowUp) => self.find_step(true),
            Key::Named(NamedKey::ArrowDown) => self.find_step(false),
            Key::Named(NamedKey::Backspace) => {
                let field = if f.focus_replace { &mut f.replace } else { &mut f.query };
                field.pop();
                if !f.focus_replace {
                    self.find_refresh(true);
                }
            }
            Key::Named(NamedKey::Space) => self.find_type(" "),
            Key::Character(t) => self.find_type(t),
            _ => return false,
        }
        true
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
            self.lines_mut().push(String::new());
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
                    let s = &mut self.lines_mut()[line];
                    let b0 = char_byte(s, col - 1);
                    let b1 = char_byte(s, col);
                    s.replace_range(b0..b1, "");
                    col -= 1;
                    edited = true;
                } else if line > 0 {
                    self.push_undo(EditKind::Other);
                    let cur = self.lines_mut().remove(line);
                    line -= 1;
                    col = self.edit_lines[line].chars().count();
                    self.lines_mut()[line].push_str(&cur);
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
                    let s = &mut self.lines_mut()[line];
                    let b0 = char_byte(s, col);
                    let b1 = char_byte(s, col + 1);
                    s.replace_range(b0..b1, "");
                    edited = true;
                } else if line + 1 < self.edit_lines.len() {
                    self.push_undo(EditKind::Other);
                    let next = self.lines_mut().remove(line + 1);
                    self.lines_mut()[line].push_str(&next);
                    edited = true;
                }
            }
            Key::Named(NamedKey::Enter) => {
                self.newline();
                line = self.cur_line;
                col = self.cur_col;
                edited = true;
            }
            Key::Named(NamedKey::Tab) => {
                self.indent(shift);
                line = self.cur_line;
                col = self.cur_col;
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
                let s = &mut self.lines_mut()[line];
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
                let s = &mut self.lines_mut()[line];
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
            self.touch();
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
            self.lines_mut().push(String::new());
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
            self.lines_mut().push(String::new());
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
            self.lines_mut().push(String::new());
        }
        self.push_undo(EditKind::Other);
        self.delete_selection();
        let line = self.cur_line.min(self.edit_lines.len() - 1);
        let col = self.cur_col.min(self.edit_lines[line].chars().count());
        let b = char_byte(&self.edit_lines[line], col);
        let tail = self.lines_mut()[line].split_off(b);
        let mut segs = text.split('\n');
        if let Some(first) = segs.next() {
            self.lines_mut()[line].push_str(first);
        }
        let mut cur = line;
        for seg in segs {
            cur += 1;
            self.lines_mut().insert(cur, seg.to_string());
        }
        self.cur_line = cur;
        self.cur_col = self.edit_lines[cur].chars().count();
        self.lines_mut()[cur].push_str(&tail);
        self.touch();
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
            // 찾기 바가 열려 있으면 타이핑은 검색어로 간다 — 조합이 끝난 한글
            // 음절도 이 문을 지나므로 여기서 갈라야 한글 검색이 된다.
            if m.find.is_some() {
                m.find_type(text);
            } else {
                m.insert_at_caret(text);
            }
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
            // 찾기 바가 열려 있는 동안은 그쪽이 키보드를 가진다. 바가 안 쓰는
            // 키도 버퍼로 흘려보내지 않는다 — Enter 한 번에 검색 결과로 가려다
            // 문서에 줄이 끼는 일이 없어야 한다.
            if m.find.is_some() {
                m.find_key(event, shift);
            } else {
                m.apply_edit_key(event, shift, alt, page_lines);
            }
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
            // Cmd+F 찾기, Cmd+Opt+F 는 바꾸기 행까지. 이미 열려 있으면 선택으로
            // 검색어를 다시 채운다 — VS Code 와 같은 결.
            KeyCode::KeyF => {
                let replacing = self.modifiers.alt_key();
                self.md_with_editor(|m| m.find_open(replacing));
                true
            }
            KeyCode::KeyG => {
                let back = self.modifiers.shift_key();
                self.md_with_editor(|m| m.find_step(back));
                true
            }
            KeyCode::Enter | KeyCode::NumpadEnter if self.modifiers.alt_key() => {
                let n = self.md_with_editor(|m| m.find_replace_all()).unwrap_or(0);
                if n > 0 {
                    self.set_toast(format!("✓ {n}곳 바꿈"));
                }
                true
            }
            _ => false,
        }
    }

    /// A press inside `pane_id`'s find bar: run the button under the cursor and
    /// report that the click was consumed. False means the bar didn't want it.
    pub(crate) fn md_find_click(&mut self, pane_id: &str) -> bool {
        let (cx, cy) = self.cursor_px;
        let Some(btn) = self
            .md_find_rects
            .iter()
            .find(|(id, _, r)| {
                id == pane_id && cx >= r.0 && cx <= r.0 + r.2 && cy >= r.1 && cy <= r.1 + r.3
            })
            .map(|(_, b, _)| *b)
        else {
            return false;
        };
        match btn {
            FindBtn::ToggleReplace => self.md_with_editor(|m| {
                if let Some(f) = m.find.as_mut() {
                    f.replacing = !f.replacing;
                    f.focus_replace = f.replacing;
                }
            }),
            FindBtn::Prev => self.md_with_editor(|m| m.find_step(true)),
            FindBtn::Next => self.md_with_editor(|m| m.find_step(false)),
            FindBtn::Close => self.md_with_editor(|m| m.find_close()),
            FindBtn::ReplaceOne => self.md_with_editor(|m| m.find_replace_one()),
            FindBtn::ReplaceAll => {
                let n = self.md_with_editor(|m| m.find_replace_all()).unwrap_or(0);
                if n > 0 {
                    self.set_toast(format!("✓ {n}곳 바꿈"));
                }
                None
            }
        };
        true
    }

    /// Run `f` against the active pane's editor, marking it dirty. Returns None
    /// when the active pane isn't one.
    fn md_with_editor<T>(&mut self, f: impl FnOnce(&mut MarkdownPane) -> T) -> Option<T> {
        let out = {
            let mut ws = self.ws.lock().ok()?;
            let pane = ws.active_mut()?;
            pane.dirty = true;
            Some(f(pane.markdown_mut()?))
        };
        self.md_ensure_caret_visible();
        out
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
            match write_atomic(&path, &text) {
                Ok(()) => {
                    if let Some(m) = pane.markdown_mut() {
                        m.mark_saved();
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
    /// Source line currently at the top of a markdown pane's viewport, whatever
    /// mode it is in. This is the intermediate currency for mode switching: the
    /// two modes scroll in different coordinate systems (block layout vs. fixed
    /// row height), and a line number is the only thing they both agree on.
    pub(crate) fn md_anchor_line(&mut self, id: &str) -> Option<usize> {
        let (raw_mode, scroll, block_line_at) = {
            let ws = self.ws.lock().unwrap();
            let m = ws.panes.get(id)?.markdown()?;
            let scroll = m.scroll as f32;
            if m.raw_mode {
                (true, scroll, None)
            } else {
                // Last block whose top is at or above the viewport top.
                let ys = self.md_block_ys.get(id)?;
                let i = ys.partition_point(|&y| y <= scroll).saturating_sub(1);
                (false, scroll, m.doc.block_lines.get(i).copied())
            }
        };
        if !raw_mode {
            return block_line_at;
        }
        let (pad, lh) = self.gpu.as_mut()?.raw_editor_metrics();
        Some((((scroll - pad) / lh).floor().max(0.0)) as usize)
    }

    /// Set a markdown pane's view mode from the header "Rendered | Raw" toggle.
    /// No-op if already in `want_raw`. Render → Raw seeds the edit buffer from
    /// the doc source; Raw → Render writes the buffer back to disk and re-parses
    /// so the laid-out view reflects the edits.
    ///
    /// Both directions carry the reading position across. Resetting to the top
    /// was the single most grating thing about the editor — you lost your place
    /// every time you switched to fix a typo.
    pub(crate) fn set_md_mode(&mut self, id: &str, want_raw: bool) {
        let anchor = self.md_anchor_line(id);
        let raw_metrics = self.gpu.as_mut().map(|g| g.raw_editor_metrics());
        let old_ys = self.md_block_ys.get(id).cloned();
        let mut pending: Option<usize> = None;
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
                // 저장이 실패했는데 modified 를 내리면 dirty 표시도 닫기 확인도
                // 사라져 편집분이 조용히 증발한다 — 실패하면 dirty 인 채로 둔다.
                let saved = match write_atomic(&path, &text) {
                    Ok(()) => true,
                    Err(e) => {
                        eprintln!("[editor] 저장 실패 {path}: {e}");
                        false
                    }
                };
                let doc = build_markdown_doc(std::path::Path::new(&path), &text);
                // 새 레이아웃의 블록 y 는 아직 없다 — 그려봐야 나온다. 옛 y 로
                // 일단 근사해 한 프레임짜리 튐을 줄이고, 정확한 위치는 렌더가
                // y 를 채운 뒤 pending 앵커가 바로잡는다.
                let guess = anchor.zip(old_ys.as_ref()).and_then(|(line, ys)| {
                    let i = doc.block_lines.partition_point(|&l| l <= line).saturating_sub(1);
                    ys.get(i).copied()
                });
                if let Some(m) = pane.markdown_mut() {
                    m.doc = Arc::new(doc);
                    m.raw_mode = false;
                    if saved {
                        m.mark_saved();
                    } else {
                        m.touch();
                    }
                    m.scroll = guess.unwrap_or(0.0).max(0.0) as usize;
                }
                pending = anchor;
            }
        } else if let Some(m) = pane.markdown_mut() {
            // Render → Raw: seed the edit buffer from the source.
            m.edit_lines = Arc::new(m.doc.raw.split('\n').map(String::from).collect());
            if m.edit_lines.is_empty() {
                m.lines_mut().push(String::new());
            }
            let line = anchor.unwrap_or(0).min(m.edit_lines.len().saturating_sub(1));
            // 커서도 보던 줄에 둔다 — 여기서 0 으로 되돌리면 "고치려고" 연 raw
            // 모드가 매번 파일 맨 위에서 시작한다.
            m.cur_line = line;
            m.cur_col = 0;
            m.scroll = raw_metrics.map_or(0, |(pad, lh)| (pad + line as f32 * lh) as usize);
            m.raw_mode = true;
        }
        drop(ws);
        if let Some(line) = pending {
            self.md_scroll_anchor.insert(id.to_string(), line);
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
            doc: Arc::new(build_markdown_doc(std::path::Path::new("/tmp/t.rs"), "")),
            is_md_doc: false,
            raw_mode: true,
            edit_lines: lines.iter().map(|s| s.to_string()).collect::<Vec<_>>().into(),
            cur_line: 0,
            cur_col: 0,
            scroll: 0,
            h_scroll: 0.0,
            modified: false,
            sel_anchor: None,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            last_edit: EditKind::Break,
            find: None,
            edited_at: None,
        }
    }

    /// 이어쓰기 접두사 인식 — Enter 와 Tab 이 둘 다 이 판단에 얹혀 있다.
    #[test]
    fn line_prefix_reads_markers() {
        let case = |s: &str| {
            let p = line_prefix(s);
            (p.len, p.next, p.marker, p.list)
        };
        assert_eq!(case("문단"), (0, String::new(), false, false));
        assert_eq!(case("    들여쓴 문단"), (4, "    ".into(), false, false));
        assert_eq!(case("- 항목"), (2, "- ".into(), true, true));
        assert_eq!(case("  * 중첩"), (4, "  * ".into(), true, true));
        // 번호는 이어 붙일 때 하나 올린다.
        assert_eq!(case("3. 셋째"), (3, "4. ".into(), true, true));
        assert_eq!(case("10) 열째"), (4, "11) ".into(), true, true));
        // 체크박스는 비운 채로 물려받는다.
        assert_eq!(case("- [x] 끝난 일"), (6, "- [ ] ".into(), true, true));
        // 인용은 마커지만 리스트는 아니다(Tab 이 줄째로 들여쓰지 않는다).
        assert_eq!(case("> 인용"), (2, "> ".into(), true, false));
        assert_eq!(case(">인용"), (1, "> ".into(), true, false));
        // 마커처럼 생겼지만 공백이 없으면 그냥 글자다.
        assert_eq!(case("-대시로 시작"), (0, String::new(), false, false));
        assert_eq!(case("2026년"), (0, String::new(), false, false));
    }

    /// Enter 는 접두사를 물려받고, 마커만 남은 줄에선 그 마커를 지운다.
    #[test]
    fn newline_continues_and_ends_lists() {
        let mut m = pane(&["- 하나"]);
        m.cur_col = 4;
        m.newline();
        assert_eq!(*m.edit_lines, vec!["- 하나", "- "]);
        assert_eq!((m.cur_line, m.cur_col), (1, 2));
        // 빈 항목에서 한 번 더 → 줄이 늘지 않고 마커만 사라진다.
        m.newline();
        assert_eq!(*m.edit_lines, vec!["- 하나", ""]);
        assert_eq!((m.cur_line, m.cur_col), (1, 0));
        // 줄 가운데서 자르면 뒷부분이 새 항목이 된다.
        let mut m = pane(&["  1. 하나둘"]);
        m.cur_col = 7;
        m.newline();
        assert_eq!(*m.edit_lines, vec!["  1. 하나", "  2. 둘"]);
        assert_eq!((m.cur_line, m.cur_col), (1, 5));
        // 캐럿이 마커 안이면 이어쓰기 없이 그냥 쪼갠다.
        let mut m = pane(&["- 하나"]);
        m.cur_col = 0;
        m.newline();
        assert_eq!(*m.edit_lines, vec!["", "- 하나"]);
    }

    /// Tab 은 선택이 있으면 줄 단위, 없으면 리스트 줄만 통째로.
    #[test]
    fn indent_is_line_wise_over_a_selection() {
        let mut m = pane(&["가", "", "나"]);
        m.sel_anchor = Some((0, 1));
        m.cur_line = 2;
        m.cur_col = 1;
        m.indent(false);
        // 빈 줄은 건드리지 않는다 — 안 보이는 공백만 남는다.
        assert_eq!(*m.edit_lines, vec!["  가", "", "  나"]);
        // 선택은 같은 글자를 계속 덮는다.
        assert_eq!(m.sel_anchor, Some((0, 3)));
        assert_eq!((m.cur_line, m.cur_col), (2, 3));
        m.indent(true);
        assert_eq!(*m.edit_lines, vec!["가", "", "나"]);
        assert_eq!(m.sel_anchor, Some((0, 1)));
        // 탭 문자로 들여쓴 줄도 Shift+Tab 이 문다.
        let mut m = pane(&["\t탭"]);
        m.indent(true);
        assert_eq!(*m.edit_lines, vec!["탭"]);
    }

    #[test]
    fn indent_nests_a_list_item_but_types_spaces_elsewhere() {
        // 리스트 줄은 캐럿이 어디 있든 항목째로 한 단 내려간다.
        let mut m = pane(&["- 항목"]);
        m.cur_col = 4;
        m.indent(false);
        assert_eq!(*m.edit_lines, vec!["  - 항목"]);
        assert_eq!(m.cur_col, 6);
        // 평범한 문단이면 캐럿 자리에 두 칸.
        let mut m = pane(&["문단"]);
        m.cur_col = 1;
        m.indent(false);
        assert_eq!(*m.edit_lines, vec!["문  단"]);
        assert_eq!(m.cur_col, 3);
    }

    #[test]
    fn find_hits_are_smart_cased_and_non_overlapping() {
        let l: Vec<String> = ["Foo foo FOO", "aaaa", ""].iter().map(|s| s.to_string()).collect();
        // 소문자 검색어 → 대소문자 무시.
        assert_eq!(find_hits(&l, "foo"), vec![(0, 0, 3), (0, 4, 7), (0, 8, 11)]);
        // 대문자가 하나라도 있으면 그대로 맞춘다.
        assert_eq!(find_hits(&l, "Foo"), vec![(0, 0, 3)]);
        // 겹치지 않는다 — `aa` 는 aaaa 에서 둘.
        assert_eq!(find_hits(&l, "aa"), vec![(1, 0, 2), (1, 2, 4)]);
        assert!(find_hits(&l, "").is_empty());
        // 한글도 열 번호가 문자 기준이라 바이트에 밀리지 않는다.
        let k = vec!["가나다 나다".to_string()];
        assert_eq!(find_hits(&k, "나다"), vec![(0, 1, 3), (0, 4, 6)]);
    }

    #[test]
    fn find_steps_wrap_and_carry_the_caret() {
        let mut m = pane(&["foo", "bar foo"]);
        m.find_open(false);
        m.find_type("foo");
        // 캐럿이 0,0 이라 첫 매치부터.
        assert_eq!(m.find.as_ref().unwrap().hits.len(), 2);
        assert_eq!((m.cur_line, m.cur_col), (0, 3));
        assert_eq!(m.sel_anchor, Some((0, 0)));
        m.find_step(false);
        assert_eq!((m.cur_line, m.cur_col), (1, 7));
        // 끝에서 한 번 더 → 처음으로 돈다.
        m.find_step(false);
        assert_eq!(m.find.as_ref().unwrap().idx, 0);
        m.find_step(true);
        assert_eq!(m.find.as_ref().unwrap().idx, 1);
    }

    #[test]
    fn replace_all_keeps_later_columns_valid() {
        let mut m = pane(&["ab ab ab", "ab"]);
        m.find_open(false);
        m.find_type("ab");
        m.find.as_mut().unwrap().replace = "xyzw".into();
        assert_eq!(m.find_replace_all(), 4);
        assert_eq!(*m.edit_lines, vec!["xyzw xyzw xyzw", "xyzw"]);
        // 한 번 되돌리면 통째로 원복 — 전체 바꾸기는 undo 한 단위다.
        m.do_undo();
        assert_eq!(*m.edit_lines, vec!["ab ab ab", "ab"]);
    }

    /// 바꾼 결과가 검색어를 다시 품으면(`a`→`aa`) 방금 넣은 글자를 또 잡아
    /// 제자리걸음 한다 — 한 번 바꾼 뒤엔 캐럿 뒤로 넘어가야 한다.
    #[test]
    fn replace_one_does_not_rematch_what_it_just_wrote() {
        let mut m = pane(&["a a"]);
        m.find_open(false);
        m.find_type("a");
        m.find.as_mut().unwrap().replace = "aa".into();
        m.find_replace_one();
        assert_eq!(*m.edit_lines, vec!["aa a"]);
        m.find_replace_one();
        assert_eq!(*m.edit_lines, vec!["aa aa"]);
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
        assert_eq!(*m.edit_lines, vec!["helrld".to_string()]);
        assert_eq!((m.cur_line, m.cur_col), (0, 3));
        assert_eq!(m.sel_anchor, None);
        assert!(m.modified);
        // Single-line case.
        let mut m = pane(&["hangul 한글 tail"]);
        m.sel_anchor = Some((0, 7));
        m.cur_col = 9;
        assert!(m.delete_selection());
        assert_eq!(*m.edit_lines, vec!["hangul  tail".to_string()]);
    }

    #[test]
    fn undo_coalesces_typing_runs() {
        let mut m = pane(&["ab"]);
        // Two same-kind pushes in a row = one snapshot (a typing run).
        m.push_undo(EditKind::Typing);
        m.lines_mut()[0].push('c');
        m.push_undo(EditKind::Typing);
        m.lines_mut()[0].push('d');
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
        m.edit_lines = Arc::new(vec!["two".into(), "three".into()]);
        m.cur_line = 1;
        m.cur_col = 5;
        let before = m.snapshot();
        let snap = m.undo_stack.pop().unwrap();
        m.redo_stack.push(before);
        m.apply_snapshot(snap);
        assert_eq!(*m.edit_lines, vec!["one".to_string()]);
        assert_eq!((m.cur_line, m.cur_col), (0, 3));
        assert_eq!(m.last_edit, EditKind::Break);
        // Redo restores the edited state.
        let snap = m.redo_stack.pop().unwrap();
        m.apply_snapshot(snap);
        assert_eq!(*m.edit_lines, vec!["two".to_string(), "three".to_string()]);
        assert_eq!((m.cur_line, m.cur_col), (1, 5));
    }

    #[test]
    fn undo_cap_bounds_stack() {
        let mut m = pane(&["x"]);
        for i in 0..(UNDO_CAP + 20) {
            m.push_undo(EditKind::Other);
            m.lines_mut()[0] = format!("{i}");
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
        assert_eq!(*m.edit_lines, vec!["hello!".to_string()]);
        assert_eq!(m.cur_col, 6);
        assert!(m.modified);
        // Insert over a selection replaces the range.
        let mut m = pane(&["hello world"]);
        m.sel_anchor = Some((0, 0));
        m.cur_col = 5; // select "hello"
        m.insert_at_caret("bye");
        assert_eq!(*m.edit_lines, vec!["bye world".to_string()]);
        assert_eq!((m.cur_line, m.cur_col), (0, 3));
    }

    #[test]
    fn paste_at_caret_splices_multiline() {
        let mut m = pane(&["abcd"]);
        m.cur_col = 2; // caret between b and c
        m.paste_at_caret("X\nY");
        assert_eq!(*m.edit_lines, vec!["abX".to_string(), "Ycd".to_string()]);
        assert_eq!((m.cur_line, m.cur_col), (1, 1));
    }

    #[test]
    fn do_undo_redo_roundtrip() {
        let mut m = pane(&["a"]);
        m.cur_col = 1;
        m.insert_at_caret("b"); // "ab"
        assert_eq!(*m.edit_lines, vec!["ab".to_string()]);
        assert!(m.do_undo());
        assert_eq!(*m.edit_lines, vec!["a".to_string()]);
        assert!(m.do_redo());
        assert_eq!(*m.edit_lines, vec!["ab".to_string()]);
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
        assert_eq!(*m.edit_lines, vec!["one".to_string(), "two".to_string()]);
        m.select_all_buf();
        assert_eq!(m.take_copy(true).as_deref(), Some("one\ntwo"));
        assert_eq!(*m.edit_lines, vec![String::new()]);
        // No selection = None.
        assert_eq!(m.take_copy(false), None);
    }

    #[test]
    fn ensure_raw_seeded_from_doc_source() {
        // A render-mode .md pane (empty edit buffer) seeds from doc.raw on pop-out.
        let doc = Arc::new(build_markdown_doc(
            std::path::Path::new("/tmp/t.md"),
            "line1\nline2",
        ));
        let mut m = MarkdownPane {
            doc,
            is_md_doc: true,
            raw_mode: false,
            edit_lines: Arc::default(),
            cur_line: 0,
            cur_col: 0,
            scroll: 0,
            h_scroll: 0.0,
            modified: false,
            sel_anchor: None,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            last_edit: EditKind::Break,
            find: None,
            edited_at: None,
        };
        m.ensure_raw_seeded();
        assert!(m.raw_mode);
        assert_eq!(*m.edit_lines, vec!["line1".to_string(), "line2".to_string()]);
        // Already-seeded buffers are left untouched.
        let mut m2 = pane(&["kept"]);
        m2.ensure_raw_seeded();
        assert_eq!(*m2.edit_lines, vec!["kept".to_string()]);
    }

    /// 원자적 쓰기가 지켜야 하는 것: 내용이 맞을 것, 권한을 잃지 않을 것,
    /// 심볼릭 링크를 파일로 갈아치우지 않을 것, 임시파일을 안 남길 것.
    #[test]
    fn atomic_write_keeps_mode_and_follows_symlinks() {
        let dir = std::env::temp_dir().join(format!("kasaterm-aw-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let real = dir.join("note.md");
        std::fs::write(&real, "before").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o600)).unwrap();
        }

        write_atomic(real.to_str().unwrap(), "after").unwrap();
        assert_eq!(std::fs::read_to_string(&real).unwrap(), "after");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&real).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "권한이 임시파일 기본값으로 넓어졌다");
        }

        #[cfg(unix)]
        {
            let link = dir.join("link.md");
            std::os::unix::fs::symlink(&real, &link).unwrap();
            write_atomic(link.to_str().unwrap(), "via link").unwrap();
            assert!(
                std::fs::symlink_metadata(&link).unwrap().file_type().is_symlink(),
                "링크가 일반 파일로 바뀌었다"
            );
            assert_eq!(std::fs::read_to_string(&real).unwrap(), "via link");
        }

        // 새 파일(아직 없는 경로)도 만들어져야 한다 — canonicalize 가 실패하는 쪽.
        let fresh = dir.join("fresh.md");
        write_atomic(fresh.to_str().unwrap(), "new").unwrap();
        assert_eq!(std::fs::read_to_string(&fresh).unwrap(), "new");

        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().into_owned()))
            .filter(|n| n.ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "임시파일이 남았다: {leftovers:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
