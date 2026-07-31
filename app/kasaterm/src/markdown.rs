//! 마크다운 pane 에디터 입력 + md 링크/블록 열기·복사.
use super::*;

/// Undo history depth per editor pane. Whole-buffer snapshots, so this also
/// bounds memory (~cap × file size worst case).
pub(crate) const UNDO_CAP: usize = 100;

/// One indent step. Spaces, not a tab: nested list markers only line up under
/// their parent by column count, and a tab's width is the viewer's opinion.
const INDENT: &str = "  ";

/// `[[토픽이름]]` 이 가리키는 파일을 `dir` 아래에서 찾는다. 인덱스와 토픽이 같은
/// 폴더에 있지 않으므로(볼트가 `sionic/` `life/` 처럼 주제 폴더로 갈라져 있다)
/// 바로 아래 한 단계까지 훑는다. 그 이상 재귀하지 않는 이유는 볼트 구조가 두
/// 단계고, 무제한 재귀는 큰 폴더에서 클릭 한 번이 디스크를 통째로 뒤지게 만든다.
pub(crate) fn wiki_target_in(
    dir: &std::path::Path,
    name: &str,
) -> Option<std::path::PathBuf> {
    let file = format!("{name}.md");
    let direct = dir.join(&file);
    if direct.exists() {
        return Some(direct);
    }
    let mut subs: Vec<std::path::PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter(|e| e.file_type().map_or(false, |t| t.is_dir()))
        .map(|e| e.path())
        .collect();
    // 폴더 순서는 OS 가 주는 대로라 판마다 다를 수 있다 — 같은 이름이 두 폴더에
    // 있을 때 열리는 파일이 바뀌면 버그로 보인다.
    subs.sort();
    subs.into_iter().map(|d| d.join(&file)).find(|p| p.exists())
}

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
        // 버퍼가 바뀌는 **유일한** 관문이라 최장 줄 캐시를 여기서 버린다.
        // `touch()` 에 걸면 안 된다 — touch 를 안 지나는 경로(스냅샷 복원)에서
        // 캐시가 살아남아 가로 스크롤 상한이 옛 버퍼를 가리킨다.
        self.longest_cache = None;
        self.edit_gen = self.edit_gen.wrapping_add(1);
        Arc::make_mut(&mut self.edit_lines)
    }

    /// 지금 유효한 접힘 목록. 편집으로 블록 모양이 바뀐 구간은 여기서 걷어낸다
    /// — 줄이 늘거나 줄면 접힘이 엉뚱한 자리를 가리키는데, 그 상태로 그리면
    /// 멀쩡한 줄이 사라진 것처럼 보인다.
    pub(crate) fn folds_valid(&mut self) -> &[(usize, usize)] {
        if !self.folds.is_empty() && self.folds_gen != self.edit_gen {
            self.folds_gen = self.edit_gen;
            // Arc 라 clone 은 포인터 하나 — retain 클로저가 self 를 다시 빌리는
            // 것만 피하면 된다.
            let lines = Arc::clone(&self.edit_lines);
            self.folds.retain(|&(s, e)| fold_end(&lines, s) == Some(e));
        }
        &self.folds
    }

    /// 이 줄의 블록을 접거나 편다. 접을 게 없으면 false.
    pub(crate) fn toggle_fold(&mut self, li: usize) -> bool {
        let Some(end) = fold_end(&self.edit_lines, li) else {
            return false;
        };
        let folded = fold_toggle(&mut self.folds, li, end);
        self.folds_gen = self.edit_gen;
        // 접은 안쪽에 캐럿이 있으면 머리로 끌어올린다 — 안 그러면 보이지 않는
        // 줄에 커서가 남아 타이핑이 화면 밖에서 일어난다.
        if folded && self.cur_line > li && self.cur_line <= end {
            self.cur_line = li;
            self.cur_col = 0;
            self.sel_anchor = None;
        }
        true
    }

    /// 가장 긴 줄의 **칸** 수 — 가로 스크롤 상한을 여기서 얻는다. 글자 수가 아니라
    /// 칸 수인 이유는 편집기가 격자에 그리기 때문이다(한글 한 글자가 2칸).
    ///
    /// 캐시한다. 원래는 트랙패드 제스처가 오는 프레임마다 버퍼 전체를 훑어
    /// 최장 줄을 다시 셌다 — 한 번의 스와이프가 수십 프레임이라 5천 줄 파일에서
    /// 같은 스캔을 수십 번 반복했다.
    pub(crate) fn longest_cols(&mut self) -> usize {
        if let Some(n) = self.longest_cache {
            return n;
        }
        let n = self
            .edit_lines
            .iter()
            .map(|l| crate::gpu::cell_cols(l))
            .max()
            .unwrap_or(0);
        self.longest_cache = Some(n);
        n
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
        // 버퍼를 통째로 갈아끼우는 경로다 — `lines_mut` 를 안 지나므로 캐시를
        // 직접 버려야 한다(안 버리면 undo 후 가로 스크롤 상한이 옛 버퍼 값).
        self.longest_cache = None;
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
            self.longest_cache = None;
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
    /// 캐럿 앞 낱말로 자동완성 후보를 다시 만든다. 낱말이 짧거나 후보가 없으면
    /// 팝업을 닫는다.
    ///
    /// 두 글자부터 연다 — 한 글자로는 후보가 버퍼 절반이라 고를 거리가 아니라
    /// 방해다(VS Code 도 같은 문턱을 쓴다).
    pub(crate) fn complete_refresh(&mut self) {
        const MIN_PREFIX: usize = 2;
        const LIMIT: usize = 8;
        if self.edit_lines.is_empty() {
            self.complete = None;
            return;
        }
        let line = self.cur_line.min(self.edit_lines.len() - 1);
        let chars: Vec<char> = self.edit_lines[line].chars().collect();
        let col = self.cur_col.min(chars.len());
        let wordish = |c: char| c.is_alphanumeric() || c == '_';
        let mut from = col;
        while from > 0 && wordish(chars[from - 1]) {
            from -= 1;
        }
        let prefix: String = chars[from..col].iter().collect();
        if prefix.chars().count() < MIN_PREFIX {
            self.complete = None;
            return;
        }
        let items = word_completions(&self.edit_lines, &prefix, line, LIMIT);
        self.complete = (!items.is_empty()).then_some(CompleteState {
            items,
            sel: 0,
            from_col: from,
            lsp_req: None,
        });
    }

    /// 고른 후보로 낱말을 갈아끼운다. 팝업이 닫혀 있으면 아무 일도 없고 false.
    pub(crate) fn complete_accept(&mut self) -> bool {
        let Some(c) = self.complete.take() else {
            return false;
        };
        let Some(word) = c.items.get(c.sel).cloned() else {
            return false;
        };
        if self.edit_lines.is_empty() {
            return false;
        }
        self.push_undo(EditKind::Other);
        let line = self.cur_line.min(self.edit_lines.len() - 1);
        let to = self.cur_col;
        let s = &mut self.lines_mut()[line];
        let b0 = char_byte(s, c.from_col);
        let b1 = char_byte(s, to);
        s.replace_range(b0..b1, &word);
        self.cur_col = c.from_col + word.chars().count();
        self.sel_anchor = None;
        self.touch();
        true
    }

    /// 팝업이 열려 있는 동안 먼저 먹는 키. 먹었으면 true.
    ///
    /// Tab·Enter 로 확정하고 Esc 로 닫는다. 팝업이 이 키를 먹지 않으면 Enter 가
    /// 줄을 끼우고 Tab 이 들여쓰기를 해 버려서, 후보를 고르려던 손이 문서를
    /// 망가뜨린다.
    pub(crate) fn complete_key(&mut self, event: &KeyEvent) -> bool {
        use winit::keyboard::{Key, NamedKey};
        let Some(c) = self.complete.as_mut() else {
            return false;
        };
        // 후보가 비어 있는 팝업 = 서버 답을 기다리는 자리표시자. 화면에 아무것도
        // 없는데 키를 먹으면 Enter·Tab 이 통째로 사라진 것처럼 보인다.
        if c.items.is_empty() {
            return false;
        }
        let n = c.items.len();
        match &event.logical_key {
            Key::Named(NamedKey::Escape) => {
                self.complete = None;
                true
            }
            Key::Named(NamedKey::Tab | NamedKey::Enter) => self.complete_accept(),
            Key::Named(NamedKey::ArrowDown) => {
                c.sel = (c.sel + 1) % n.max(1);
                true
            }
            Key::Named(NamedKey::ArrowUp) => {
                c.sel = (c.sel + n.saturating_sub(1)) % n.max(1);
                true
            }
            _ => false,
        }
    }

    /// 키가 버퍼를 바꾼 뒤 후보를 다시 만든다. 낱말과 무관한 키(방향키·Enter 등)
    /// 뒤에는 팝업을 닫는다 — 캐럿이 딴 데로 갔는데 후보가 남아 있으면 엉뚱한
    /// 자리에 채워 넣힌다.
    ///
    /// 한글은 이 경로로 오지 않는다(조합은 `md_feed_jamo`) — 조합 중에 후보를
    /// 세우면 미확정 글자로 목록이 흔들리므로 일부러 뺐다.
    pub(crate) fn complete_after_key(&mut self, event: &KeyEvent) {
        use winit::keyboard::{Key, NamedKey};
        match &event.logical_key {
            Key::Character(_) | Key::Named(NamedKey::Backspace) => self.complete_refresh(),
            _ => self.complete = None,
        }
    }

    /// 선택을 괄호·따옴표로 감싼다. 선택을 지우고 덮어쓰지 않는 게 VS Code
    /// 동작이고, 감싼 뒤에도 안쪽 내용이 선택으로 남아 연속으로 감쌀 수 있다.
    pub(crate) fn wrap_selection(&mut self, open: char, close: char) {
        let Some((s, e)) = self.sel_range() else { return };
        if self.edit_lines.is_empty() {
            return;
        }
        self.push_undo(EditKind::Other);
        // 끝을 먼저 끼운다 — 앞을 먼저 끼우면 같은 줄의 끝 col 이 한 칸 밀린다.
        {
            let l = &mut self.lines_mut()[e.0];
            let b = char_byte(l, e.1.min(l.chars().count()));
            l.insert(b, close);
        }
        {
            let l = &mut self.lines_mut()[s.0];
            let b = char_byte(l, s.1.min(l.chars().count()));
            l.insert(b, open);
        }
        self.sel_anchor = Some((s.0, s.1 + 1));
        self.cur_line = e.0;
        self.cur_col = if e.0 == s.0 { e.1 + 1 } else { e.1 };
        self.touch();
    }

    pub(crate) fn newline(&mut self) {
        if self.edit_lines.is_empty() {
            self.lines_mut().push(String::new());
        }
        self.push_undo(EditKind::Other);
        self.delete_selection();
        let line = self.cur_line.min(self.edit_lines.len() - 1);
        let col = self.cur_col.min(self.edit_lines[line].chars().count());
        // 괄호 **사이**의 Enter 는 블록을 연다: 한 단계 들여쓴 빈 줄에 캐럿을
        // 두고 닫는 괄호는 원래 들여쓰기로 내린다. 따옴표는 제외한다(여닫이가
        // 같은 글자라 여러 줄로 갈라 놓으면 문자열이 깨진다).
        let cs: Vec<char> = self.edit_lines[line].chars().collect();
        let block = matches!(
            (col.checked_sub(1).and_then(|i| cs.get(i)), cs.get(col)),
            (Some(&o), Some(&c)) if o != c && auto_close_for(o) == Some(c)
        );
        if block {
            let pad: String = cs
                .iter()
                .take_while(|c| **c == ' ' || **c == '\t')
                .collect();
            let s = &mut self.lines_mut()[line];
            let b = char_byte(s, col);
            let tail = s.split_off(b);
            let inner = format!("{pad}{INDENT}");
            self.lines_mut().insert(line + 1, inner.clone());
            self.lines_mut().insert(line + 2, format!("{pad}{tail}"));
            self.cur_line = line + 1;
            self.cur_col = inner.chars().count();
            self.touch();
            return;
        }
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
        // Opt+↑↓ 는 이동이 아니라 **줄 편집**(이동·복제)이다 — motion 으로 두면
        // 아래 Shift 분기가 선택 앵커를 세워 Shift+Opt+↑↓ 줄 복제를 가로챈다.
        let is_line_cmd = alt
            && matches!(&event.logical_key, Key::Named(NamedKey::ArrowUp | NamedKey::ArrowDown));
        let is_motion = !is_line_cmd
            && matches!(
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
                    // 자동으로 들어온 짝은 같이 지운다 — 여는 쪽만 사라지고
                    // 닫는 쪽이 남으면 손으로 또 지워야 한다.
                    let cs: Vec<char> = s.chars().collect();
                    let paired = matches!(
                        (cs.get(col - 1), cs.get(col)),
                        (Some(&o), Some(&c)) if auto_close_for(o) == Some(c)
                    );
                    let b0 = char_byte(s, col - 1);
                    let b1 = char_byte(s, col + usize::from(paired));
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
            // Opt+↑↓ = 줄 이동, Shift+Opt+↑↓ = 줄 복제. 이 두 팔이 없어 `alt` 가
            // 버려지고 "커서만 한 줄 이동" 이 되던 자리다.
            Key::Named(NamedKey::ArrowUp) if is_line_cmd => {
                if shift {
                    self.duplicate_lines(true);
                } else {
                    self.move_lines(true);
                }
                line = self.cur_line;
                col = self.cur_col;
                edited = true;
            }
            Key::Named(NamedKey::ArrowDown) if is_line_cmd => {
                if shift {
                    self.duplicate_lines(false);
                } else {
                    self.move_lines(false);
                }
                line = self.cur_line;
                col = self.cur_col;
                edited = true;
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
                // 자동 닫기는 **키로 친 한 글자**에만 붙는다. 붙여넣기
                // (`md_editor_paste`)와 한글 확정(`md_insert_into`)은 다른
                // 경로라 여기 오지 않으므로, 붙여넣은 코드에 괄호가 있어도
                // 짝이 덧붙지 않는다.
                let plan = match txt.chars().next() {
                    Some(ch) if txt.chars().count() == 1 => {
                        let cs: Vec<char> = self.edit_lines[line].chars().collect();
                        plan_typed(
                            ch,
                            col.checked_sub(1).and_then(|i| cs.get(i).copied()),
                            cs.get(col).copied(),
                            sel.is_some(),
                        )
                    }
                    _ => TypeAction::Plain,
                };
                match plan {
                    TypeAction::Wrap(close) => {
                        self.wrap_selection(txt.chars().next().unwrap_or('('), close);
                        return;
                    }
                    // 넘어가기는 버퍼를 안 바꾼다 — undo 를 쌓지도, 저장 대상으로
                    // 표시하지도 않는다.
                    TypeAction::Overtype => {
                        self.sel_anchor = None;
                        self.cur_line = line;
                        self.cur_col = col + 1;
                        return;
                    }
                    _ => {}
                }
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
                if let TypeAction::Pair(close) = plan {
                    let s = &mut self.lines_mut()[line];
                    let b = char_byte(s, col);
                    s.insert(b, close);
                }
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

    /// 캐럿이 놓인 단어를 선택한다 — Cmd+D 첫 누름(그리고 나중의 더블클릭).
    /// 경계는 Opt+←→ 와 **같은** 판정(`prev_word_col`/`next_word_col`)을 쓴다:
    /// 두 조작이 각자 경계를 두면 같은 줄에서 결과가 갈려 어느 쪽이 맞는지
    /// 화면이 설명해 주지 못한다. 캐럿이 공백 위면 왼쪽 단어를 집는다(방금
    /// 타이핑한 낱말을 잡으려는 게 대개의 의도).
    /// 잡을 단어가 없으면 선택을 건드리지 않고 false.
    pub(crate) fn select_word_at(&mut self) -> bool {
        let Some(line) = self.edit_lines.get(self.cur_line) else { return false };
        let chars: Vec<char> = line.chars().collect();
        let start = prev_word_col(&chars, self.cur_col.min(chars.len()));
        let end = next_word_col(&chars, start);
        if end <= start {
            return false;
        }
        self.sel_anchor = Some((self.cur_line, start));
        self.cur_col = end;
        self.last_edit = EditKind::Break;
        true
    }

    /// 줄 단위 명령(줄 이동·복제, 나중의 줄 삭제·주석 토글)이 대상으로 삼는 줄
    /// 범위. 선택이 있으면 그것이 걸친 줄 전부, 없으면 캐럿 줄 하나.
    /// 트리플클릭 — 캐럿이 선 줄을 처음부터 끝까지 선택한다. 빈 줄이면 앵커와
    /// 캐럿이 같아져 release 경로가 알아서 선택을 접는다.
    pub(crate) fn select_line_at(&mut self) -> bool {
        let Some(len) = self
            .edit_lines
            .get(self.cur_line)
            .map(|l| l.chars().count())
        else {
            return false;
        };
        self.sel_anchor = Some((self.cur_line, 0));
        self.cur_col = len;
        self.last_edit = EditKind::Break;
        true
    }

    fn line_block(&self) -> (usize, usize) {
        match self.sel_range() {
            Some((s, e)) => (s.0, e.0),
            None => {
                let l = self.cur_line.min(self.edit_lines.len().saturating_sub(1));
                (l, l)
            }
        }
    }

    /// 캐럿과 선택 앵커의 **줄 번호만** `d` 만큼 옮긴다(열은 유지). 줄이 통째로
    /// 움직이는 명령에서 선택이 따라오지 않으면 방금 옮긴 줄이 선택에서 벗어난다.
    fn shift_caret_lines(&mut self, d: i64) {
        let last = self.edit_lines.len().saturating_sub(1);
        let mv = |l: usize| ((l as i64 + d).max(0) as usize).min(last);
        self.cur_line = mv(self.cur_line);
        if let Some((al, ac)) = self.sel_anchor {
            self.sel_anchor = Some((mv(al), ac));
        }
    }

    /// Opt+↑↓ — 캐럿 줄(선택이 있으면 그 블록)을 한 줄 위/아래로 옮긴다.
    /// 경계에 닿으면 아무것도 하지 않고 false — 예전엔 이 키가 `alt` 를 아예
    /// 무시해 "커서만 한 줄 이동" 으로 조용히 처리됐다(거노: 아무 일도 안 난 것
    /// 처럼 보임).
    pub(crate) fn move_lines(&mut self, up: bool) -> bool {
        if self.edit_lines.is_empty() {
            return false;
        }
        let (s, e) = self.line_block();
        if up && s == 0 {
            return false;
        }
        if !up && e + 1 >= self.edit_lines.len() {
            return false;
        }
        self.push_undo(EditKind::Other);
        // 블록을 옮기는 대신 **이웃 한 줄을 블록 반대편으로** 넘긴다 — 결과가
        // 같고 블록 길이와 무관하게 remove+insert 한 번이다.
        let lines = self.lines_mut();
        if up {
            let moved = lines.remove(s - 1);
            lines.insert(e, moved);
        } else {
            let moved = lines.remove(e + 1);
            lines.insert(s, moved);
        }
        self.shift_caret_lines(if up { -1 } else { 1 });
        self.last_edit = EditKind::Other;
        true
    }

    /// Shift+Opt+↑↓ — 캐럿 줄(선택이 있으면 그 블록)을 복제한다.
    pub(crate) fn duplicate_lines(&mut self, up: bool) -> bool {
        if self.edit_lines.is_empty() {
            return false;
        }
        let (s, e) = self.line_block();
        self.push_undo(EditKind::Other);
        let block: Vec<String> = self.edit_lines[s..=e].to_vec();
        let n = block.len();
        let at = if up { s } else { e + 1 };
        let lines = self.lines_mut();
        for (i, l) in block.into_iter().enumerate() {
            lines.insert(at + i, l);
        }
        // 아래로 복제하면 캐럿을 복제본으로 옮겨 연달아 누르는 게 자연스럽게
        // 이어진다. 위로 복제하면 삽입이 원본을 밀어내므로 캐럿을 그대로 두면
        // 이미 복제본 위에 있다.
        if !up {
            self.shift_caret_lines(n as i64);
        }
        self.last_edit = EditKind::Other;
        true
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
    /// under its own undo unit.
    ///
    /// 선택이 없으면 **캐럿이 선 줄 전체**를 개행까지 붙여 집는다 — VS Code 의
    /// Cmd+C/Cmd+X 가 그렇게 동작한다. 이 폴백이 없으면 줄을 옮기려고 Cmd+X 를
    /// 누른 사람에게 아무 일도 일어나지 않는다(선택부터 하라고 요구하는 셈).
    pub(crate) fn take_copy(&mut self, cut: bool) -> Option<String> {
        if let Some(text) = self.selected_text() {
            if cut {
                self.push_undo(EditKind::Other);
                self.delete_selection();
            }
            return Some(text);
        }
        let li = self.cur_line;
        let line = self.edit_lines.get(li)?.clone();
        if cut {
            self.push_undo(EditKind::Other);
            let lines = self.lines_mut();
            // 마지막 한 줄은 지우지 않고 비운다 — 버퍼가 완전히 비면 이후
            // 줄·열 인덱싱이 전부 무너진다.
            if lines.len() > 1 {
                lines.remove(li);
            } else {
                lines[0].clear();
            }
            self.cur_line = li.min(self.edit_lines.len().saturating_sub(1));
            self.cur_col = 0;
            self.last_edit = EditKind::Break;
            self.touch();
        }
        Some(format!("{line}\n"))
    }

    /// Cmd+Shift+K — 선택이 걸친 줄들(선택이 없으면 캐럿 줄)을 통째로 지운다.
    pub(crate) fn delete_lines(&mut self) -> bool {
        let (s, e) = self.line_block();
        self.push_undo(EditKind::Break);
        let lines = self.lines_mut();
        let e = e.min(lines.len().saturating_sub(1));
        if s > e {
            return false;
        }
        lines.drain(s..=e);
        if lines.is_empty() {
            lines.push(String::new());
        }
        self.sel_anchor = None;
        self.cur_line = s.min(self.edit_lines.len().saturating_sub(1));
        self.cur_col = 0;
        self.last_edit = EditKind::Break;
        self.touch();
        true
    }

    /// Cmd+Enter — 캐럿이 줄 어디에 있든 **아래에** 새 줄을 열고 내려간다.
    /// `above` 면 위에 연다(Cmd+Shift+Enter). 들여쓰기만 현재 줄에서 물려받는다
    /// — `line_prefix` 는 `- `/`> ` 같은 마크다운 마커까지 이어붙이므로 코드
    /// 편집기에서는 쓰지 않는다.
    pub(crate) fn open_line(&mut self, above: bool) {
        self.push_undo(EditKind::Break);
        let indent: String = self
            .edit_lines
            .get(self.cur_line)
            .map(|l| l.chars().take_while(|c| *c == ' ' || *c == '\t').collect())
            .unwrap_or_default();
        let at = if above { self.cur_line } else { self.cur_line + 1 };
        let lines = self.lines_mut();
        let at = at.min(lines.len());
        lines.insert(at, indent.clone());
        self.sel_anchor = None;
        self.cur_line = at;
        self.cur_col = indent.chars().count();
        self.last_edit = EditKind::Break;
        self.touch();
    }

    /// Cmd+/ — 선택이 걸친 줄들(없으면 캐럿 줄)의 줄 주석을 켜고 끈다.
    ///
    /// 블록 안 **한 줄이라도** 주석이 아니면 전체를 주석 처리하고, 전부 주석일
    /// 때만 해제한다(VS Code 규칙). 넣을 때는 블록에서 가장 얕은 들여쓰기에
    /// 맞춰 세로로 정렬하고, 빈 줄은 건드리지 않는다 — 빈 줄에 주석 기호만
    /// 남으면 지운 뒤에도 흔적이 남는다.
    pub(crate) fn toggle_comment(&mut self, prefix: &str) -> bool {
        let (s, e) = self.line_block();
        let rows: Vec<usize> = (s..=e)
            .filter(|&i| {
                self.edit_lines
                    .get(i)
                    .is_some_and(|l| !l.trim().is_empty())
            })
            .collect();
        if rows.is_empty() {
            return false;
        }
        let on = rows
            .iter()
            .all(|&i| self.edit_lines[i].trim_start().starts_with(prefix));
        self.push_undo(EditKind::Break);
        // 들여쓰기는 공백·탭뿐이고 주석 기호도 ASCII 라 바이트 인덱스와 char
        // 인덱스가 같다 — 그래서 아래 슬라이싱이 안전하다.
        let head = prefix.len() + 1;
        let lines = self.lines_mut();
        if on {
            for &i in &rows {
                let ws = lines[i].len() - lines[i].trim_start().len();
                let rest = lines[i][ws + prefix.len()..].to_string();
                let rest = rest.strip_prefix(' ').unwrap_or(&rest).to_string();
                lines[i] = format!("{}{}", &lines[i][..ws], rest);
            }
        } else {
            let col = rows
                .iter()
                .map(|&i| lines[i].len() - lines[i].trim_start().len())
                .min()
                .unwrap_or(0);
            for &i in &rows {
                lines[i].insert_str(col, &format!("{prefix} "));
            }
        }
        // 캐럿·앵커가 주석 기호만큼 밀린다. 주석을 넣은 열보다 왼쪽에 있던
        // 캐럿은 그대로 둔다.
        let shift = |col: usize, line: &str| -> usize {
            if on {
                col.saturating_sub(head).min(line.chars().count())
            } else {
                (col + head).min(line.chars().count())
            }
        };
        if rows.contains(&self.cur_line) {
            let l = self.edit_lines[self.cur_line].clone();
            self.cur_col = shift(self.cur_col, &l);
        }
        if let Some((al, ac)) = self.sel_anchor {
            if rows.contains(&al) {
                let l = self.edit_lines[al].clone();
                self.sel_anchor = Some((al, shift(ac, &l)));
            }
        }
        self.last_edit = EditKind::Break;
        self.touch();
        true
    }
}

// 멀티커서 배선(struct MarkdownPane 을 커서 벡터로 바꾸는 단계) 전까지는
// 호출부가 없다. 테스트는 이미 붙어 있고 규칙이 확정된 로직이다.
#[allow(dead_code)]
/// 커서 하나 — 캐럿 위치와, 선택 중이면 그 앵커.
///
/// 멀티커서의 단위. `MarkdownPane` 이 아직 단일 `cur_line`/`cur_col` 을 들고
/// 있어서 이 타입은 먼저 순수 로직만 세워 둔 것이다 — struct 배선은 별개 단계다.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Caret {
    pub line: usize,
    pub col: usize,
    /// 선택 앵커. `None` 이면 선택 없이 캐럿만.
    pub anchor: Option<(usize, usize)>,
}

impl Caret {
    pub(crate) fn at(line: usize, col: usize) -> Self {
        Self { line, col, anchor: None }
    }

    /// 이 커서가 덮는 범위를 문서 순으로 — 앵커가 캐럿보다 뒤일 수 있다.
    pub(crate) fn span(&self) -> ((usize, usize), (usize, usize)) {
        let head = (self.line, self.col);
        match self.anchor {
            Some(a) if a <= head => (a, head),
            Some(a) => (head, a),
            None => (head, head),
        }
    }
}

#[allow(dead_code)]
/// 커서 목록을 정규화한다 — 문서 순으로 정렬하고, 같은 자리이거나 범위가
/// 겹치는 커서를 하나로 합친다.
///
/// 편집마다 이걸 통과시켜야 한다: 두 커서가 같은 자리에 남으면 타이핑 한 번에
/// 글자가 두 번 들어가고, 겹친 선택을 각각 지우면 두 번째 삭제가 이미 사라진
/// 범위를 가리켜 엉뚱한 글자를 먹는다.
///
/// 병합된 커서의 **방향은 앞 커서를 따른다** — 위에서 아래로 Shift 드래그하던
/// 사람이 병합 후 갑자기 반대로 자라는 걸 보면 안 된다.
pub(crate) fn normalize_carets(mut carets: Vec<Caret>) -> Vec<Caret> {
    if carets.len() < 2 {
        return carets;
    }
    carets.sort_by_key(|c| c.span());
    let mut out: Vec<Caret> = Vec::with_capacity(carets.len());
    for c in carets {
        let (cs, ce) = c.span();
        let Some(prev) = out.last_mut() else {
            out.push(c);
            continue;
        };
        let (ps, pe) = prev.span();
        if cs > pe {
            out.push(c);
            continue;
        }
        // 겹친다 — 앞 커서의 방향을 유지한 채 범위만 넓힌다.
        let (ns, ne) = (ps.min(cs), pe.max(ce));
        let forward = prev.anchor.is_none_or(|a| a <= (prev.line, prev.col));
        if ns == ne {
            *prev = Caret::at(ns.0, ns.1);
        } else if forward {
            *prev = Caret { line: ne.0, col: ne.1, anchor: Some(ns) };
        } else {
            *prev = Caret { line: ns.0, col: ns.1, anchor: Some(ne) };
        }
    }
    out
}

/// 버퍼 안 낱말로 만드는 자동완성 후보 — `prefix` 로 시작하는 서로 다른 낱말을
/// **캐럿에서 가까운 줄 순서**로 최대 `limit` 개.
///
/// LSP 가 붙기 전에도 쓸 수 있고, 붙은 뒤에도 서버 응답이 오기 전 한 프레임을
/// 메운다(VS Code 의 word-based suggestion 과 같은 자리). 가까운 줄을 앞세우는
/// 이유는 방금 쓴 이름이 다시 쓸 이름일 확률이 가장 높기 때문이다.
///
/// `prefix` 와 똑같은 낱말은 넣지 않는다 — 고를 이유가 없는 후보다.
pub(crate) fn word_completions(
    lines: &[String],
    prefix: &str,
    near: usize,
    limit: usize,
) -> Vec<String> {
    if prefix.is_empty() {
        return Vec::new();
    }
    let wordish = |c: char| c.is_alphanumeric() || c == '_';
    let mut hits: Vec<(usize, usize, String)> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    for (li, l) in lines.iter().enumerate() {
        for w in l.split(|c: char| !wordish(c)) {
            if w.len() < prefix.len() || !w.starts_with(prefix) || w == prefix {
                continue;
            }
            if seen.iter().any(|s| s == w) {
                continue;
            }
            seen.push(w.to_string());
            hits.push((li.abs_diff(near), li, w.to_string()));
        }
    }
    hits.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
    hits.into_iter().take(limit).map(|(_, _, w)| w).collect()
}

// Cmd+D 두 번째 누름(다음 출현에 커서 추가) 배선 전까지 호출부가 없다.
#[allow(dead_code)]
/// `needle` 이 `from` **뒤에서** 처음 나오는 자리(줄, 열). 문서 끝까지 없으면
/// 처음부터 `from` 까지 다시 훑는다 — Cmd+D 를 계속 누르면 문서를 한 바퀴 돌아야
/// 마지막 출현에서 멈춰 버리지 않는다.
///
/// 대소문자를 구분한다(코드에서 `Foo` 와 `foo` 는 다른 것이다) 그리고 단어
/// 경계를 보지 않는다 — VS Code 의 Cmd+D 도 고른 글자열 그대로 찾는다.
pub(crate) fn find_after(
    lines: &[String],
    needle: &str,
    from: (usize, usize),
) -> Option<(usize, usize)> {
    let n: Vec<char> = needle.chars().collect();
    if n.is_empty() || lines.is_empty() {
        return None;
    }
    let hit = |li: usize, lo: usize, hi: usize| -> Option<usize> {
        let hay: Vec<char> = lines[li].chars().collect();
        if hay.len() < n.len() {
            return None;
        }
        (lo..=hay.len() - n.len()).filter(|&i| i < hi).find(|&i| hay[i..i + n.len()] == n[..])
    };
    let start = from.0.min(lines.len() - 1);
    if let Some(c) = hit(start, from.1, usize::MAX) {
        return Some((start, c));
    }
    for li in (start + 1)..lines.len() {
        if let Some(c) = hit(li, 0, usize::MAX) {
            return Some((li, c));
        }
    }
    for li in 0..start {
        if let Some(c) = hit(li, 0, usize::MAX) {
            return Some((li, c));
        }
    }
    // 마지막으로 시작 줄의 `from` 앞쪽 — 한 바퀴를 여기서 닫는다.
    hit(start, 0, from.1).map(|c| (start, c))
}

/// 들여쓰기 한 단계가 몇 칸인지. 가이드 선을 그리는 쪽(`draw_raw_editor`)이
/// 편집기와 같은 눈금을 써야 선이 글자 사이에 정확히 떨어진다.
pub(crate) fn indent_step_cols() -> usize {
    INDENT.chars().count().max(1)
}

/// 그 줄에 들여쓰기 가이드를 몇 개 그릴지.
///
/// 빈 줄은 위아래에서 가장 가까운 실줄 중 **얕은 쪽**을 물려받는다 — 블록
/// 사이 빈 줄에서 선이 끊기면 오히려 눈에 걸리고, 깊은 쪽을 쓰면 블록이 끝난
/// 뒤에도 선이 남는다. 들여쓰기에 탭이 섞인 줄은 0 — 탭 폭을 모르는 채로 그으면
/// 글자와 어긋난 선만 남는다.
pub(crate) fn indent_guide_depth(lines: &[String], li: usize) -> usize {
    /// 빈 줄에서 실줄을 찾아 올라갈/내려갈 상한. 화면 한 폭을 넘게 비어 있으면
    /// 어차피 잇는 의미가 없다.
    const LOOK: usize = 200;
    let step = indent_step_cols();
    // None = 공백뿐인 줄(물려받아야 하는 줄).
    let lead = |l: &String| -> Option<usize> {
        let ws: Vec<char> = l.chars().take_while(|c| c.is_whitespace()).collect();
        if ws.len() == l.chars().count() {
            return None;
        }
        if ws.contains(&'\t') {
            return Some(0);
        }
        Some(ws.len() / step)
    };
    let Some(cur) = lines.get(li) else { return 0 };
    if let Some(d) = lead(cur) {
        return d;
    }
    let up = lines[..li].iter().rev().take(LOOK).find_map(&lead);
    let down = lines
        .get(li + 1..)
        .into_iter()
        .flatten()
        .take(LOOK)
        .find_map(&lead);
    match (up, down) {
        (Some(a), Some(b)) => a.min(b),
        _ => 0,
    }
}

// 폴딩 배선(거터 삼각형 + 접힌 줄 건너뛰기) 전까지 호출부가 없다.
#[allow(dead_code)]
/// 접기용 들여쓰기 깊이. `None` 은 공백뿐인 줄.
///
/// 가이드(`indent_guide_depth`)와 탭 처리가 다르다 — 가이드는 탭이 섞이면 선을
/// 안 그려야 하지만(폭을 몰라 어긋난다), 접기는 **상대** 깊이만 보므로 탭을 한
/// 단계로 세면 그만이다.
pub(crate) fn fold_depth(l: &str) -> Option<usize> {
    let ws: Vec<char> = l.chars().take_while(|c| c.is_whitespace()).collect();
    if ws.len() == l.chars().count() {
        return None;
    }
    let step = indent_step_cols();
    Some(ws.iter().map(|c| if *c == '\t' { step } else { 1 }).sum::<usize>() / step)
}

#[allow(dead_code)]
/// 그 줄에서 접을 수 있는 블록의 마지막 줄. 접을 게 없으면 `None`.
///
/// 들여쓰기로 잡는다 — 언어 문법 없이 모든 파일에서 동작하고, tree-sitter
/// 스팬은 타이핑 중 일부러 낡은 상태로 두므로(`raw_editor_ts_spans`) 접기 범위의
/// 근거로 쓰면 방금 만든 블록이 안 접힌다.
///
/// 블록은 "다음 실줄이 더 깊은" 줄에서 시작해 그 깊이 이하로 돌아오는 줄 **앞**
/// 까지다. 중간 빈 줄은 블록을 끊지 않지만(끊으면 함수 안 빈 줄마다 접기가
/// 잘린다) 블록 **끝**의 빈 줄은 포함하지 않는다 — 접었을 때 남는 꼬리가 된다.
pub(crate) fn fold_end(lines: &[String], li: usize) -> Option<usize> {
    let base = fold_depth(lines.get(li)?)?;
    let mut end = None;
    for (i, l) in lines.iter().enumerate().skip(li + 1) {
        let Some(d) = fold_depth(l) else { continue };
        if d <= base {
            break;
        }
        end = Some(i);
    }
    end
}

/// 접힌 구간 하나 = `(머리 줄, 마지막 숨은 줄)`. 머리 줄은 **보인다** — 접힌
/// 표시를 그 줄에 얹어야 어디가 접혔는지 알 수 있다.
///
/// 구간들은 항상 머리 줄 오름차순이고 서로 겹치지 않는다(`fold_insert` 가
/// 지킨다). 그래야 아래 변환들이 한 번의 순회로 끝난다.
pub(crate) type Folds = Vec<(usize, usize)>;

/// 이 줄이 접혀서 화면에 없는가.
pub(crate) fn is_hidden(folds: &[(usize, usize)], line: usize) -> bool {
    folds.iter().any(|&(s, e)| line > s && line <= e)
}

/// 버퍼 줄 → 화면 행. 접힘이 없으면 그대로다.
///
/// 숨은 줄을 물어보면 그 줄이 속한 구간의 **머리 줄** 행을 준다 — 접힌 안쪽을
/// 가리키는 좌표(캐럿·진단)가 화면 밖으로 사라지는 대신 접힌 머리에 얹힌다.
pub(crate) fn visual_row(folds: &[(usize, usize)], line: usize) -> usize {
    let mut skipped = 0usize;
    for &(s, e) in folds {
        if s >= line {
            break;
        }
        // 구간이 이 줄을 품고 있으면 이 줄까지만 센다 — 그러면 숨은 줄이
        // 머리 줄의 행으로 접힌다.
        skipped += e.min(line) - s;
    }
    line - skipped
}

/// 화면 행 → 버퍼 줄. `visual_row` 의 역.
pub(crate) fn buffer_line(folds: &[(usize, usize)], row: usize, total: usize) -> usize {
    let mut line = row;
    for &(s, e) in folds {
        if s >= line {
            break;
        }
        line += e - s;
    }
    line.min(total.saturating_sub(1))
}

/// 구간 하나를 접힘 목록에 넣는다. 이미 그 머리가 접혀 있으면 **펴고** false.
///
/// 겹치는 구간은 통째로 걷어낸다 — 바깥 블록을 접었는데 안쪽 접힘이 남아 있으면,
/// 바깥을 펴는 순간 안쪽이 접힌 채로 튀어나와 사람이 접은 적 없는 모양이 된다.
pub(crate) fn fold_toggle(folds: &mut Folds, s: usize, e: usize) -> bool {
    if let Some(i) = folds.iter().position(|&(fs, _)| fs == s) {
        folds.remove(i);
        return false;
    }
    folds.retain(|&(fs, fe)| fe < s || fs > e);
    folds.push((s, e));
    folds.sort_by_key(|&(fs, _)| fs);
    true
}

/// 짝을 맞추는 괄호 쌍.
const BRACKET_PAIRS: [(char, char); 3] = [('(', ')'), ('[', ']'), ('{', '}')];

/// 타이핑하면 짝이 따라 들어오는 글자. 따옴표는 여닫이가 같아서 판단 규칙이
/// 다르므로(`plan_typed`) 여기선 같은 글자를 짝으로 돌려준다.
pub(crate) fn auto_close_for(ch: char) -> Option<char> {
    BRACKET_PAIRS
        .iter()
        .find_map(|&(o, c)| (ch == o).then_some(c))
        .or_else(|| matches!(ch, '"' | '\'' | '`').then_some(ch))
}

/// 글자 하나를 쳤을 때 편집기가 할 일.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub(crate) enum TypeAction {
    /// 그냥 그 글자만 넣는다.
    Plain,
    /// 넣고 짝도 넣고 캐럿은 사이에.
    Pair(char),
    /// 이미 있는 닫는 글자를 넘어간다(글자를 새로 넣지 않는다).
    Overtype,
    /// 선택을 지우지 않고 앞뒤로 감싼다.
    Wrap(char),
}

/// 자동 닫기 판단. `before`/`after` 는 캐럿 좌우 글자.
///
/// VS Code 규칙을 따른다: **오른쪽에 낱말이 붙어 있으면 짝을 넣지 않는다**
/// (`foo` 앞에서 `(` 를 치면 `(foo` 지 `()foo` 가 아니다). 따옴표는 추가로
/// 왼쪽도 본다 — `don't` 의 `'` 가 짝을 끌고 오면 못 쓴다.
pub(crate) fn plan_typed(
    ch: char,
    before: Option<char>,
    after: Option<char>,
    has_sel: bool,
) -> TypeAction {
    let close = auto_close_for(ch);
    if has_sel {
        return close.map_or(TypeAction::Plain, TypeAction::Wrap);
    }
    // 닫는 글자를 쳤는데 그 자리에 이미 같은 글자가 있으면 넘어간다 — 자동으로
    // 들어온 짝을 손으로 또 치는 게 사람의 자연스러운 습관이다.
    let closing = BRACKET_PAIRS.iter().any(|&(_, c)| ch == c) || matches!(ch, '"' | '\'' | '`');
    if closing && after == Some(ch) {
        return TypeAction::Overtype;
    }
    let Some(close) = close else {
        return TypeAction::Plain;
    };
    let word = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '_');
    if word(after) {
        return TypeAction::Plain;
    }
    if ch == close && (word(before) || before == Some(ch)) {
        return TypeAction::Plain;
    }
    TypeAction::Pair(close)
}

/// 캐럿에 붙어 있는 괄호와 그 짝의 위치. 캐럿 **왼쪽**을 먼저 보고 없으면
/// 오른쪽을 본다 — 닫는 괄호를 막 타이핑한 순간에 짝이 보여야 하기 때문이다.
///
/// 문자열·주석 안의 괄호는 걸러내지 않는다. 정확히 하려면 tree-sitter 스팬과
/// 교차 검사해야 하는데, 그 스팬은 타이핑 중 의도적으로 낡은 상태로 두므로
/// (`raw_editor_ts_spans`) 오히려 어긋난 강조가 나온다. 짝이 틀릴 수 있는
/// 경우는 문자열 안에 홀괄호를 쓸 때뿐이고, 그때는 강조가 안 뜨거나 엉뚱한
/// 곳에 뜰 뿐 편집을 망가뜨리지 않는다.
pub(crate) fn match_bracket(
    lines: &[String],
    line: usize,
    col: usize,
) -> Option<((usize, usize), (usize, usize))> {
    /// 훑기 상한 — 매 프레임 도는 자리라 큰 파일에서 버퍼 끝까지 가면 안 된다.
    /// 짝이 이보다 멀면 강조를 포기한다(화면 밖이라 보이지도 않는다).
    const MAX_SCAN: usize = 5_000;
    let row: Vec<char> = lines.get(line)?.chars().collect();
    let (at, ch) = [col.checked_sub(1), Some(col)]
        .into_iter()
        .flatten()
        .find_map(|i| {
            let c = *row.get(i)?;
            BRACKET_PAIRS
                .iter()
                .any(|&(o, cl)| c == o || c == cl)
                .then_some((i, c))
        })?;
    let (open, close, forward) = BRACKET_PAIRS.iter().find_map(|&(o, cl)| {
        if ch == o {
            Some((o, cl, true))
        } else if ch == cl {
            Some((o, cl, false))
        } else {
            None
        }
    })?;
    let mut depth = 0i32;
    let mut scanned = 0usize;
    if forward {
        for l in line..lines.len() {
            let cur: Vec<char> = lines[l].chars().collect();
            let from = if l == line { at } else { 0 };
            for (c, &x) in cur.iter().enumerate().skip(from) {
                scanned += 1;
                if scanned > MAX_SCAN {
                    return None;
                }
                if x == open {
                    depth += 1;
                } else if x == close {
                    depth -= 1;
                    if depth == 0 {
                        return Some(((line, at), (l, c)));
                    }
                }
            }
        }
    } else {
        for l in (0..=line).rev() {
            let cur: Vec<char> = lines[l].chars().collect();
            let upto = if l == line { at + 1 } else { cur.len() };
            for c in (0..upto.min(cur.len())).rev() {
                scanned += 1;
                if scanned > MAX_SCAN {
                    return None;
                }
                if cur[c] == close {
                    depth += 1;
                } else if cur[c] == open {
                    depth -= 1;
                    if depth == 0 {
                        return Some(((line, at), (l, c)));
                    }
                }
            }
        }
    }
    None
}

/// 같은 자리 연타를 센다 — 2 = 더블클릭, 3 = 트리플클릭, 네 번째는 다시 1 로
/// 감는다. winit 은 더블클릭 이벤트를 주지 않고, 이 레포에는 판정 코드가 아예
/// 없었다(터미널 pane 도 없다). `now` 를 인자로 받는 이유는 테스트에서 시간을
/// 넘겨 볼 수 있게 하려고다.
pub(crate) fn click_streak(
    prev: Option<(std::time::Instant, f32, f32, u8)>,
    now: std::time::Instant,
    px: f32,
    py: f32,
) -> u8 {
    /// 연타로 볼 최대 간격. macOS 기본 더블클릭 속도(0.5s)보다 조금 좁게.
    const GAP_MS: u128 = 450;
    /// 손이 미세하게 흔들려도 같은 자리로 본다.
    const SLOP: f32 = 4.0;
    match prev {
        Some((t, x, y, n))
            if now.duration_since(t).as_millis() <= GAP_MS
                && (px - x).abs() <= SLOP
                && (py - y).abs() <= SLOP =>
        {
            if n >= 3 {
                1
            } else {
                n + 1
            }
        }
        _ => 1,
    }
}

/// 줄 주석 접두사. **css·html 은 일부러 None** — 줄 주석 문법이 아예 없어서
/// `//` 를 넣으면 스타일시트가 조용히 깨진다(css 는 `/* */`, html 은
/// `<!-- -->` 만 유효). 블록 주석으로 감싸는 건 별개 작업이라 여기서는
/// 무동작으로 두고 부르는 쪽이 토스트를 띄운다.
pub(crate) fn line_comment_prefix(lang: &str) -> Option<&'static str> {
    Some(match lang {
        "rust" | "javascript" | "typescript" | "tsx" | "go" | "c" | "c++" | "json" => "//",
        "python" | "bash" | "toml" | "yaml" => "#",
        "sql" => "--",
        _ => return None,
    })
}

impl App {
    /// Insert text at the active markdown editor's cursor (committed Hangul or
    /// pasted text). Multi-char safe; advances the cursor by char count.
    pub(crate) fn md_editor_insert(&mut self, text: &str) {
        let active = self.ws.lock().ok().and_then(|ws| ws.active_pane.clone());
        let Some(id) = active else { return };
        self.md_insert_into(&id, text);
    }

    /// `md_editor_insert` 의 pane 지정판. 조합기 주인이 바뀔 때(`ime_retarget`)
    /// 남은 음절은 **떠나는** 편집기에 떨궈야 하는데, 그 시점엔 그 pane 이 이미
    /// 활성이 아니다 — 활성 기준으로 넣으면 새 문맥에 오배달된다.
    pub(crate) fn md_insert_into(&mut self, id: &str, text: &str) {
        if text.is_empty() {
            return;
        }
        {
            let mut ws = self.ws.lock().unwrap();
            // `pane_mut` 는 없으면 만들어 버린다 — 닫힌 pane 에 유령을 남기지
            // 않도록 조회만 한다.
            let Some(pane) = ws.panes.get_mut(id) else { return };
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
        // 캐럿 스크롤은 활성 pane 기준이라 떠난 pane 엔 의미가 없다(돌아올 때
        // 다시 맞춰진다).
        let active = self.ws.lock().ok().and_then(|ws| ws.active_pane.clone());
        if active.as_deref() == Some(id) {
            self.md_ensure_caret_visible();
        }
    }
    /// Raw-editor key entry point with Hangul composition. macOS hands jamo
    /// (U+3130..318F) through `event.text`; we feed the local composer (same as
    /// the terminal path), insert committed syllables, and keep the preedit in
    /// `self.preedit` for the editor overlay. Non-jamo flushes then edits.
    /// 자모 한 글자를 편집기 조합기에 먹인다. `c` 가 호환 자모가 아니면 아무
    /// 일도 안 하고 `false` — 호출자는 평소 키 처리로 넘어가면 된다.
    ///
    /// 키 경로에서 따로 뗀 이유는 **검증 때문**이다. winit `KeyEvent` 는 밖에서
    /// 만들 수 없어 조합을 헤드리스로 재현할 길이 없었는데, 진짜 경로가 이
    /// 함수 하나로 모이면 하네스(`mdscript` 의 `jamo` 단계)가 같은 코드를 탄다.
    /// 조합 버그를 "거노가 직접 쳐 봐야만" 아는 상태를 벗어나는 게 목적이다.
    pub(crate) fn md_feed_jamo(&mut self, c: char) -> bool {
        if !(0x3130..=0x318F).contains(&(c as u32)) {
            return false;
        }
        if let Some(commit) = self.hangul.feed(c) {
            self.md_editor_insert(&commit);
        }
        self.preedit = self.hangul.preedit().unwrap_or_default();
        self.in_preedit = !self.preedit.is_empty();
        self.chrome_dirty = true;
        true
    }

    pub(crate) fn md_editor_input(&mut self, event: &KeyEvent) {
        use winit::keyboard::{Key, NamedKey};
        // 조합기 주인을 이 편집기로. `ime_retarget` 은 ws 를 다시 잠그는데
        // 2021 에디션에선 `if let` 조건식의 임시 MutexGuard 가 body 끝까지
        // 살아 **같은 스레드가 자기 락에 물린다** — id 는 별도 문으로 꺼내
        // 락을 확실히 놓고 부른다.
        let active = self.ws.lock().ok().and_then(|ws| ws.active_pane.clone());
        if let Some(id) = active {
            self.ime_retarget(crate::ImeFocus::Editor(id));
        }
        #[cfg(target_os = "macos")]
        if let Some(t) = &event.text {
            // 자모가 낱자로 버퍼에 박히는 사고는 **코드포인트가 예상 범위 밖**일
            // 때만 나는데, 그건 로그 없이는 화면만 봐선 구분이 안 된다.
            if std::env::var_os("KASATERM_IME_DEBUG").is_some() {
                let cps: Vec<String> = t.chars().map(|c| format!("U+{:04X}", c as u32)).collect();
                eprintln!("[md-key] text={t:?} {cps:?} focus={:?}", self.ime_focus);
            }
            if t.chars().count() == 1 {
                if let Some(c) = t.chars().next() {
                    if self.md_feed_jamo(c) {
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
            } else if m.complete_key(event) {
                // 자동완성 팝업이 먹었다 — 버퍼로 흘려보내지 않는다.
            } else {
                m.apply_edit_key(event, shift, alt, page_lines);
                m.complete_after_key(event);
            }
        }
        self.md_ensure_caret_visible();
        // 팝업이 열렸으면 서버에도 물어 둔다(응답은 다음 틱에 `lsp_complete_pump`
        // 가 받는다). 위 ws lock 을 놓은 뒤라야 한다 — 이 안에서 다시 잠근다.
        let id = { self.ws.lock().ok().and_then(|w| w.active_pane.clone()) };
        if let Some(id) = id {
            self.lsp_complete_request(&id);
        }
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
    /// 조합 중인 한글 음절을 버퍼에 확정한다. 캐럿을 옮기거나 버퍼를 읽는
    /// 동작 **앞에서** 부른다 — 안 그러면 조합 중이던 글자가 파일에서 빠지거나
    /// (Cmd+S) 옮겨 간 캐럿 자리에 남는다. 조합 중이 아니면 아무 일도 없다.
    pub(crate) fn md_flush_preedit(&mut self) {
        if let Some(flushed) = self.hangul.flush() {
            self.md_editor_insert(&flushed);
        }
        self.preedit.clear();
        self.in_preedit = false;
    }

    pub(crate) fn md_editor_shortcut(&mut self, event: &KeyEvent) -> bool {
        use winit::keyboard::{KeyCode, PhysicalKey};
        if !self.host_mod() {
            return false;
        }
        let PhysicalKey::Code(code) = event.physical_key else {
            return false;
        };
        // 확정은 **모든** 단축키 앞에서 한 번. 예전엔 팔마다 같은 네 줄을 복붙해
        // S/V/X/A/Z 만 확정했고 Cmd+C·Cmd+화살표·Cmd+F/G 는 빠져 있어, 조합 중
        // 그 키를 누르면 음절이 유실되거나 옮겨 간 캐럿 자리에 남았다.
        self.md_flush_preedit();
        match code {
            KeyCode::KeyS => {
                self.save_active_editor();
                true
            }
            KeyCode::KeyV => {
                self.md_editor_paste();
                true
            }
            KeyCode::KeyC => {
                self.md_copy_selection(false);
                true
            }
            KeyCode::KeyX => {
                self.md_copy_selection(true);
                true
            }
            KeyCode::KeyA => {
                self.md_select_all();
                true
            }
            // Cmd+D = 캐럿 단어 선택(VS Code 첫 누름). 이 팔이 없어 전역 폴백으로
            // 새면서 **편집 중에 pane 이 쪼개졌다**. Shift 조합은 흘려보내
            // Cmd+Shift+D 세로 분할 경로를 남긴다.
            KeyCode::KeyD if !self.modifiers.shift_key() => {
                self.md_with_editor(|m| m.select_word_at());
                true
            }
            KeyCode::KeyZ => {
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
            // Cmd+Enter = 아래에 새 줄, Cmd+Shift+Enter = 위에 새 줄. 캐럿이 줄
            // 중간에 있어도 줄을 쪼개지 않는다 — 그게 평범한 Enter 와 다른 점이다.
            KeyCode::Enter | KeyCode::NumpadEnter => {
                let above = self.modifiers.shift_key();
                self.md_with_editor(|m| m.open_line(above));
                true
            }
            // Cmd+Shift+K = 줄 삭제. Shift 없는 Cmd+K 는 전역으로 흘려보낸다.
            KeyCode::KeyK if self.modifiers.shift_key() => {
                self.md_with_editor(|m| m.delete_lines());
                true
            }
            // Cmd+/ 주석 토글. 접두사는 파일 확장자에서 뽑고, 줄 주석이 없는
            // 형식(css·html)은 아무것도 하지 않고 이유를 알린다 — 조용히
            // 무시하면 단축키가 고장난 것처럼 보인다.
            KeyCode::Slash => {
                let done = self.md_with_editor(|m| {
                    let lang = code_lang_for_path(std::path::Path::new(m.doc.path.as_str()));
                    match line_comment_prefix(lang) {
                        Some(p) => {
                            m.toggle_comment(p);
                            true
                        }
                        None => false,
                    }
                });
                if done == Some(false) {
                    self.set_toast("이 형식엔 줄 주석이 없어요".into());
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
            (id, m.edit_lines.len(), line, prefix, m.scroll, m.h_scroll, m.folds.clone())
        };
        let (id, line_count, cur_line, prefix, scroll, h_scroll, folds) = snap;
        let Some(&(_bx, _by, bw, bh)) = self.md_body_rects.get(&id) else { return };
        let Some(gpu) = self.gpu.as_mut() else { return };
        let (ns, nh) =
            gpu.raw_editor_ensure_visible(
            line_count, cur_line, &prefix, bw, bh, scroll, h_scroll, &folds,
        );
        if (ns - scroll).abs() > 0.5 || (nh - h_scroll).abs() > 0.5 {
            let mut ws = self.ws.lock().unwrap();
            if let Some(pane) = ws.panes.get_mut(&id) {
                pane.dirty = true;
                if let Some(m) = pane.markdown_mut() {
                    m.scroll = ns.max(0.0);
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
    /// 클릭 연타 상태를 갱신하고 이번 클릭이 몇 번째인지 돌려준다.
    pub(crate) fn md_click_count(&mut self, px: f32, py: f32) -> u8 {
        let now = std::time::Instant::now();
        let n = click_streak(self.md_click_streak, now, px, py);
        self.md_click_streak = Some((now, px, py, n));
        n
    }

    /// 편집기 본문에 press 한 번이 들어왔을 때의 전체 처리: 캐럿 이동 → 드래그
    /// 앵커 → 연타 선택(2 = 단어, 3 = 줄). 돌려주는 값은 이번 클릭이 몇 번째인지.
    ///
    /// **순서가 계약이다** — 앵커를 연타 선택보다 나중에 세우면 더블클릭으로 잡은
    /// 선택을 그 앵커가 지워 버린다. handler 의 press 경로와 헤드리스 하네스가
    /// 같은 함수를 써야 검증이 실물과 어긋나지 않으므로 여기 한 곳에 둔다.
    /// rust 파일이 편집기로 열렸으면 rust-analyzer 를 붙인다(없으면 띄운다).
    ///
    /// 서버는 프로젝트 루트당 하나만 띄우고, 루트는 `Cargo.toml` 을 찾아 올라가
    /// 정한다. rust 가 아니거나 rust-analyzer 가 없으면 조용히 아무 일도 안 한다 —
    /// 편집기는 LSP 없이도 온전히 동작해야 한다.
    pub(crate) fn lsp_attach(&mut self, id: &str) {
        // 틱마다 불리는 자리라 **경로와 세대**만 먼저 본다. 버퍼를 이어 붙이는
        // 건 정말 보낼 때뿐이다 — 5천 줄 join 이 곧 프레임 예산이다.
        let Some((path, gen)) = ({
            let Ok(ws) = self.ws.lock() else { return };
            match ws.panes.get(id).and_then(|p| p.markdown()) {
                Some(m) if m.raw_mode && m.doc.path.ends_with(".rs") => {
                    Some((std::path::PathBuf::from(&m.doc.path), m.edit_gen))
                }
                _ => None,
            }
        }) else {
            return;
        };
        // 이미 알린 파일이면 남은 일은 재전송뿐. 타이핑이 멎은 뒤에만 보낸다.
        if let Some(c) = self.lsp.as_mut() {
            if c.is_open(&path) {
                if !c.change_due(&path, gen) {
                    return;
                }
                let Some(text) = ({
                    let Ok(ws) = self.ws.lock() else { return };
                    ws.panes
                        .get(id)
                        .and_then(|p| p.markdown())
                        .map(|m| m.edit_lines.join("\n"))
                }) else {
                    return;
                };
                c.did_change(&path, &text, gen);
                return;
            }
        }
        let Some(text) = ({
            let Ok(ws) = self.ws.lock() else { return };
            ws.panes
                .get(id)
                .and_then(|p| p.markdown())
                .map(|m| m.edit_lines.join("\n"))
        }) else {
            return;
        };
        if self.lsp.is_none() {
            // Cargo.toml 이 있는 가장 가까운 조상이 루트. 못 찾으면 붙이지 않는다 —
            // 루트를 잘못 주면 rust-analyzer 가 홈 디렉토리를 인덱싱하러 든다.
            let root = path.ancestors().skip(1).find(|d| d.join("Cargo.toml").is_file());
            let Some(root) = root else { return };
            self.lsp = crate::lsp::LspClient::spawn(root);
        }
        if let Some(c) = self.lsp.as_mut() {
            c.did_open(&path, &text, gen);
        }
    }

    /// 활성 편집기 캐럿 자리 — (경로, 버퍼 세대, 줄, 열, 그 줄 텍스트).
    /// rust 파일이 편집기 모드로 열려 있을 때만 Some.
    fn lsp_caret_at(&self, id: &str) -> Option<(std::path::PathBuf, u64, usize, usize, String)> {
        let ws = self.ws.lock().ok()?;
        let m = ws.panes.get(id)?.markdown()?;
        if !m.raw_mode || !m.doc.path.ends_with(".rs") {
            return None;
        }
        let li = m.cur_line.min(m.edit_lines.len().saturating_sub(1));
        let text = m.edit_lines.get(li).cloned().unwrap_or_default();
        let col = m.cur_col.min(text.chars().count());
        Some((
            std::path::PathBuf::from(&m.doc.path),
            m.edit_gen,
            li,
            col,
            text,
        ))
    }

    /// 서버가 아는 본문이 지금 버퍼보다 낡았으면 **디바운스를 건너뛰고** 먼저
    /// 맞춘다. 방금 친 글자를 모르는 문서에 대고 물으면 서버는 그 자리에
    /// 아무것도 없다고 답한다 — 자동완성·정의 이동 둘 다 이게 급소다.
    fn lsp_sync_now(&mut self, id: &str, path: &std::path::Path, gen: u64) {
        let stale = self
            .lsp
            .as_ref()
            .is_some_and(|c| c.is_open(path) && c.sent_gen(path) != Some(gen));
        if !stale {
            return;
        }
        let Some(text) = ({
            let Ok(ws) = self.ws.lock() else { return };
            ws.panes
                .get(id)
                .and_then(|p| p.markdown())
                .map(|m| m.edit_lines.join("\n"))
        }) else {
            return;
        };
        if let Some(c) = self.lsp.as_mut() {
            c.did_change(path, &text, gen);
        }
    }

    /// 이 화면 좌표를 품은 편집기 pane.
    fn md_pane_at_px(&self, px: f32, py: f32) -> Option<String> {
        self.md_body_rects
            .iter()
            .find(|(_, &(x, y, w, h))| px >= x && px < x + w && py >= y && py < y + h)
            .map(|(id, _)| id.clone())
    }

    /// 화면 좌표가 가리키는 (줄, 열, 그 줄 텍스트) — **캐럿은 건드리지 않는다**.
    /// 호버가 쓴다: 마우스가 지나가는 자리마다 캐럿이 따라가면 편집이 불가능하다.
    fn md_pos_at_px(&mut self, id: &str, px: f32, py: f32) -> Option<(usize, usize, String)> {
        let &(bx, by, _, _) = self.md_body_rects.get(id)?;
        let snap = {
            let ws = self.ws.lock().ok()?;
            let m = ws.panes.get(id)?.markdown()?;
            m.raw_mode
                .then(|| (m.edit_lines.clone(), m.scroll, m.h_scroll, m.folds.clone()))?
        };
        let (lines, scroll, h_scroll, folds) = snap;
        let gpu = self.gpu.as_mut()?;
        let (line, col) =
            gpu.raw_editor_caret_at(&lines, bx, by, scroll, h_scroll, px, py, &folds);
        Some((line, col, lines.get(line).cloned().unwrap_or_default()))
    }

    /// 마우스가 멎었으면 그 자리를 묻고, 답이 왔으면 툴팁에 담는다. 틱마다 부른다.
    pub(crate) fn lsp_hover_tick(&mut self) {
        // 사람이 "여기 뭐지" 하고 멈추는 시간. 더 짧으면 지나가다 툴팁이 튀고,
        // 더 길면 기다리는 게 느껴진다.
        const DELAY: std::time::Duration = std::time::Duration::from_millis(450);
        let Some(h) = self.hover.as_ref() else { return };
        if h.text.is_some() {
            return;
        }
        // 답을 기다리는 중이면 받기만 한다.
        if let Some(rid) = h.req {
            if let Some(t) = self.lsp.as_ref().and_then(|c| c.take_hover(rid)) {
                if let Some(h) = self.hover.as_mut() {
                    h.text = Some(t);
                }
                self.chrome_dirty = true;
            }
            return;
        }
        if h.since.elapsed() < DELAY {
            return;
        }
        let (px, py) = h.at;
        // 여기서부터는 한 번만 시도한다 — 실패하면 상태를 지워, 마우스가 다시
        // 움직일 때까지 조용하다(안 그러면 틱마다 같은 자리를 되묻는다).
        self.hover = None;
        let Some(id) = self.md_pane_at_px(px, py) else { return };
        let Some((line, col, line_text)) = self.md_pos_at_px(&id, px, py) else { return };
        let Some((path, gen, _, _, _)) = self.lsp_caret_at(&id) else { return };
        self.lsp_sync_now(&id, &path, gen);
        let utf16 = crate::lsp::char_col_to_utf16(&line_text, col);
        let Some(rid) = self
            .lsp
            .as_mut()
            .and_then(|c| c.request_hover(&path, line, utf16))
        else {
            return;
        };
        self.hover = Some(crate::HoverState {
            at: (px, py),
            since: std::time::Instant::now(),
            req: Some(rid),
            text: None,
        });
    }

    /// Cmd+클릭이 닿은 자리의 정의를 묻는다. 답이 오면 `lsp_goto_pump` 가 그
    /// 파일을 열고 캐럿을 옮긴다.
    pub(crate) fn lsp_goto_request(&mut self, id: &str) {
        let Some((path, gen, line, col, line_text)) = self.lsp_caret_at(id) else {
            return;
        };
        self.lsp_sync_now(id, &path, gen);
        let utf16 = crate::lsp::char_col_to_utf16(&line_text, col);
        self.lsp_goto = self
            .lsp
            .as_mut()
            .and_then(|c| c.request_definition(&path, line, utf16));
    }

    /// 정의 응답이 왔으면 그 파일을 열고 그 줄로 간다. 틱마다 부른다.
    pub(crate) fn lsp_goto_pump(&mut self) {
        let Some(rid) = self.lsp_goto else { return };
        let Some((path, line, utf16)) = self.lsp.as_ref().and_then(|c| c.take_definition(rid))
        else {
            return;
        };
        self.lsp_goto = None;
        self.open_file(path.clone(), None, false);
        let want = path.to_string_lossy().to_string();
        {
            let Ok(mut ws) = self.ws.lock() else { return };
            let Some(id) = ws.active_pane.clone() else { return };
            let Some(pane) = ws.panes.get_mut(&id) else { return };
            pane.dirty = true;
            let Some(m) = pane.markdown_mut() else { return };
            // 설정에 따라 외부 앱으로 열렸을 수도 있다 — 그때는 엉뚱한 pane 의
            // 캐럿을 옮기지 않는다.
            if m.doc.path != want {
                return;
            }
            let li = line.min(m.edit_lines.len().saturating_sub(1));
            m.cur_line = li;
            // 대상 파일의 그 줄로 UTF-16 을 되돌린다 — 응답 좌표도 UTF-16 이다.
            m.cur_col = m
                .edit_lines
                .get(li)
                .map(|l| crate::lsp::utf16_col_to_char(l, utf16))
                .unwrap_or(0);
            m.sel_anchor = None;
        }
        self.md_ensure_caret_visible();
    }

    /// 자동완성 팝업이 열려 있으면 서버에도 물어본다.
    ///
    /// 버퍼 낱말로 **이미 채워 둔** 팝업을 나중에 서버 답으로 갈아끼우는 구조다.
    /// 요청-응답은 왕복이라 즉시 오지 않는데, 그동안 팝업이 비어 있으면 타이핑이
    /// 끊긴 것처럼 읽힌다. VS Code 도 같은 순서로 채운다.
    ///
    /// 팝업이 **닫혀 있어도** 묻는다. 버퍼 낱말이 없으면 팝업이 안 열리는데,
    /// 정작 자동완성이 가장 필요한 자리(`s.` 뒤의 메서드처럼 버퍼 어디에도 없는
    /// 이름)가 전부 거기라 그대로 두면 LSP 후보를 영영 볼 수 없다. 답이 오면
    /// `lsp_complete_pump` 가 그때 팝업을 채운다.
    pub(crate) fn lsp_complete_request(&mut self, id: &str) {
        // 낱말 두 글자, 또는 멤버 접근 직후(`.`/`::`). 후자는 prefix 가 비어도
        // 물어야 한다 — 그 자리가 바로 서버만 아는 후보가 나오는 곳이다.
        const MIN_PREFIX: usize = 2;
        let Some((path, gen, line, col, line_text)) = self.lsp_caret_at(id) else {
            return;
        };
        let chars: Vec<char> = line_text.chars().collect();
        let wordish = |c: char| c.is_alphanumeric() || c == '_';
        let mut from = col;
        while from > 0 && wordish(chars[from - 1]) {
            from -= 1;
        }
        let trigger = from > 0 && matches!(chars[from - 1], '.' | ':');
        if col - from < MIN_PREFIX && !trigger {
            return;
        }
        self.lsp_sync_now(id, &path, gen);
        let utf16 = crate::lsp::char_col_to_utf16(&line_text, col);
        let Some(rid) = self
            .lsp
            .as_mut()
            .and_then(|c| c.request_completion(&path, line, utf16))
        else {
            return;
        };
        if let Ok(mut ws) = self.ws.lock() {
            if let Some(m) = ws.panes.get_mut(id).and_then(|p| p.markdown_mut()) {
                match m.complete.as_mut() {
                    Some(cs) => cs.lsp_req = Some(rid),
                    // 아직 팝업이 없으면 자리표시자만 세운다 — 후보가 비어 있는
                    // 동안은 그리지도, 키를 먹지도 않는다.
                    None => {
                        m.complete = Some(CompleteState {
                            items: Vec::new(),
                            sel: 0,
                            from_col: from,
                            lsp_req: Some(rid),
                        })
                    }
                }
            }
        }
    }

    /// 도착한 서버 후보로 팝업을 갈아끼운다. 틱마다 부른다 — 응답이 언제 올지
    /// 모르므로 키 경로에서 기다릴 수는 없다.
    pub(crate) fn lsp_complete_pump(&mut self, id: &str) {
        const LIMIT: usize = 8;
        let Some(rid) = ({
            let Ok(ws) = self.ws.lock() else { return };
            ws.panes
                .get(id)
                .and_then(|p| p.markdown())
                .and_then(|m| m.complete.as_ref())
                .and_then(|c| c.lsp_req)
        }) else {
            return;
        };
        let Some(items) = self.lsp.as_ref().and_then(|c| c.take_completion(rid)) else {
            return;
        };
        let Ok(mut ws) = self.ws.lock() else { return };
        let Some(pane) = ws.panes.get_mut(id) else { return };
        pane.dirty = true;
        let Some(m) = pane.markdown_mut() else { return };
        let Some(cs) = m.complete.as_ref() else { return };
        let prefix: String = m
            .edit_lines
            .get(m.cur_line)
            .map(|l| {
                l.chars()
                    .skip(cs.from_col)
                    .take(m.cur_col.saturating_sub(cs.from_col))
                    .collect()
            })
            .unwrap_or_default();
        let picked: Vec<String> = items
            .into_iter()
            .filter(|it| it.starts_with(&prefix))
            .take(LIMIT)
            .collect();
        let empty = {
            let Some(cs) = m.complete.as_mut() else { return };
            // 한 번 받은 요청은 다시 묻지 않는다 — 안 지우면 틱마다 같은 id 를
            // 되물어 팝업이 계속 흔들린다.
            cs.lsp_req = None;
            // 서버가 아무것도 못 주면 버퍼 낱말 후보를 그대로 둔다. 갑자기 비면
            // 팝업이 깜빡이는데, 그게 "후보 없음"보다 훨씬 거슬린다.
            if !picked.is_empty() {
                cs.items = picked;
                cs.sel = 0;
            }
            cs.items.is_empty()
        };
        // 자리표시자로 열어 뒀는데 서버도 줄 게 없었다 — 흔적을 남기지 않는다.
        if empty {
            m.complete = None;
        }
    }

    /// 이 파일의 진단 — 렌더가 매 프레임 부른다. 잠금을 짧게 잡으려고 복사한다.
    pub(crate) fn lsp_diags(&self, path: &str) -> Vec<crate::lsp::Diag> {
        let Some(c) = self.lsp.as_ref() else {
            return Vec::new();
        };
        let Ok(m) = c.diags.lock() else {
            return Vec::new();
        };
        m.get(std::path::Path::new(path)).cloned().unwrap_or_default()
    }

    /// 거터의 접기 삼각형을 눌렀으면 접거나 펴고 true. 본문을 눌렀으면 false —
    /// 호출자는 평소 캐럿 배치로 넘어간다.
    pub(crate) fn md_fold_click(&mut self, id: &str, px: f32, py: f32) -> bool {
        let Some(&(bx, by, _, _)) = self.md_body_rects.get(id) else {
            return false;
        };
        let snap = {
            let ws = self.ws.lock().ok();
            ws.and_then(|w| {
                w.panes.get(id).and_then(|p| p.markdown()).and_then(|m| {
                    m.raw_mode
                        .then(|| (m.edit_lines.clone(), m.scroll, m.folds.clone()))
                })
            })
        };
        let Some((lines, scroll, folds)) = snap else {
            return false;
        };
        let Some(gpu) = self.gpu.as_mut() else {
            return false;
        };
        let Some(li) = gpu.raw_editor_fold_hit(&lines, bx, by, scroll, px, py, &folds) else {
            return false;
        };
        let Ok(mut ws) = self.ws.lock() else { return false };
        let Some(pane) = ws.panes.get_mut(id) else { return false };
        pane.dirty = true;
        let Some(m) = pane.markdown_mut() else { return false };
        // 접을 게 없는 줄을 눌렀어도 **true** 다 — 거터를 누른 것이지 본문을 누른
        // 게 아니라서, 여기서 false 를 주면 캐럿이 엉뚱하게 튄다.
        m.toggle_fold(li);
        true
    }

    /// 미니맵을 눌렀으면 그 자리로 스크롤하고 true. 본문을 눌렀으면 false —
    /// 호출자는 평소 캐럿 배치로 넘어간다.
    ///
    /// 폭·줄높이는 그리는 쪽과 **같은 함수**에서 얻는다(`raw_editor_mini_*`).
    /// 같은 식을 두 벌 두면 폰트 크기를 바꾸는 순간 띠와 히트 영역이 갈린다.
    pub(crate) fn md_mini_jump(&mut self, id: &str, px: f32, py: f32) -> bool {
        let Some(&(bx, by, bw, bh)) = self.md_body_rects.get(id) else {
            return false;
        };
        let n = {
            let Ok(ws) = self.ws.lock() else { return false };
            match ws.panes.get(id).and_then(|p| p.markdown()) {
                Some(m) if m.raw_mode => m.edit_lines.len(),
                _ => return false,
            }
        };
        let (mini_w, per, lh) = {
            let Some(g) = self.gpu.as_mut() else { return false };
            (
                g.raw_editor_mini_w(bw),
                g.raw_editor_mini_per(bh, n),
                g.raw_editor_line_h(),
            )
        };
        if mini_w <= 0.0 || px < bx + bw - mini_w {
            return false;
        }
        // 누른 줄이 화면 **가운데**로 오게 옮긴다 — 맨 위에 놓으면 그 위 문맥이
        // 잘려서 왜 그 자리를 눌렀는지 확인할 수가 없다.
        let want = (((py - by) / per).max(0.0) * lh - bh * 0.5).max(0.0);
        if let Ok(mut ws) = self.ws.lock() {
            if let Some(pane) = ws.panes.get_mut(id) {
                pane.dirty = true;
                if let Some(m) = pane.markdown_mut() {
                    m.scroll = want.min((n as f32 * lh - bh).max(0.0));
                }
            }
        }
        true
    }

    pub(crate) fn md_press_caret(&mut self, id: &str, px: f32, py: f32) -> u8 {
        self.md_click_caret(id, px, py);
        let clicks = self.md_click_count(px, py);
        if let Ok(mut ws) = self.ws.lock() {
            if let Some(m) = ws.panes.get_mut(id).and_then(|p| p.markdown_mut()) {
                m.sel_anchor = Some((m.cur_line, m.cur_col));
                match clicks {
                    2 => {
                        m.select_word_at();
                    }
                    n if n >= 3 => {
                        m.select_line_at();
                    }
                    _ => {}
                }
            }
        }
        clicks
    }

    pub(crate) fn md_click_caret(&mut self, id: &str, px: f32, py: f32) {
        // 조합 중 클릭은 **캐럿을 옮기기 전에** 옛 자리에 확정해야 한다.
        // `ime_retarget` 은 조합기 주인이 바뀔 때만 일하고 같은 대상이면 조기
        // 반환하므로(input.rs), 같은 편집기 안 클릭은 그 경로로 안 잡힌다 —
        // preedit 렌더는 매 프레임 현재 캐럿을 따라가니 화면은 멀쩡해 보이는데
        // 다음 자모가 완성되는 순간 **클릭한 새 자리에** 커밋됐다.
        let mine = matches!(self.ime_focus.as_ref(), Some(crate::ImeFocus::Editor(e)) if e == id);
        if mine {
            self.md_flush_preedit();
        }
        let Some(&(bx, by, _bw, _bh)) = self.md_body_rects.get(id) else { return };
        // Pull the lines + pan out under a short lock so the GPU borrow below
        // doesn't overlap the workspace borrow.
        let snapshot = {
            let ws = self.ws.lock().unwrap();
            ws.panes.get(id).and_then(|p| p.markdown()).and_then(|m| {
                m.raw_mode
                    .then(|| (m.edit_lines.clone(), m.scroll, m.h_scroll, m.folds.clone()))
            })
        };
        let Some((lines, scroll, h_scroll, folds)) = snapshot else { return };
        let Some(gpu) = self.gpu.as_mut() else { return };
        let (line, col) =
            gpu.raw_editor_caret_at(&lines, bx, by, scroll, h_scroll, px, py, &folds);
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
        drop(ws);
        // 다른 문맥(터미널·다른 편집기·설정 필드)에서 조합 중이었다면 그 **옛
        // 대상에** 확정하고 조합기 주인을 이 편집기로 넘긴다. 이미 이 편집기가
        // 주인이면 위에서 비웠으므로 부를 필요가 없다(드래그마다 도는 자리라
        // 불필요한 String 할당도 피한다).
        if !mine {
            self.ime_retarget(crate::ImeFocus::Editor(id.to_string()));
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
            let scroll = m.scroll;
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
                    m.scroll = guess.unwrap_or(0.0).max(0.0);
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
            m.scroll = raw_metrics.map_or(0.0, |(pad, lh)| pad + line as f32 * lh);
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
    pub(crate) fn try_open_md_link(&mut self) -> bool {
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
    /// `[[토픽이름]]` 이 가리키는 파일.
    fn find_wiki_target(&self, name: &str) -> Option<std::path::PathBuf> {
        wiki_target_in(&self.active_markdown_dir()?, name)
    }
    /// Open a markdown link destination: http(s)/mailto go to the default
    /// app (browser/mail); a local path is revealed in Finder (`open -R`),
    /// resolving relative paths against the markdown file's directory.
    pub(crate) fn open_md_dest(&mut self, dest: &str) {
        // 문서 사이 링크는 **이 뷰 안에서** 열어야 이동이 이동으로 읽힌다 — 파일
        // 열기 설정(VS Code 등)을 타면 클릭마다 다른 앱으로 튄다. 못 찾으면 조용히
        // 넘기지 말고 어느 이름을 못 찾았는지 알려 준다(표기 오타가 흔하다).
        if let Some(name) = dest.strip_prefix("wiki:") {
            match self.find_wiki_target(name) {
                Some(p) => self.open_file(p, None, true),
                None => self.set_toast(format!("{name} 을 못 찾았습니다")),
            }
            return;
        }
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
            scroll: 0.0,
            h_scroll: 0.0,
            modified: false,
            sel_anchor: None,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            last_edit: EditKind::Break,
            find: None,
            complete: None,
            longest_cache: None,
            edit_gen: 0,
            folds: Vec::new(),
            folds_gen: 0,
            edited_at: None,
        }
    }

    #[test]
    fn select_word_at_grabs_the_run_under_the_caret() {
        // 단어 중간
        let mut p = pane(&["let value = 1;"]);
        p.cur_col = 6;
        assert!(p.select_word_at());
        assert_eq!(p.sel_range(), Some(((0, 4), (0, 9))));
        // 단어 끝에 붙은 캐럿(방금 타이핑한 뒤)도 그 단어를 잡는다
        let mut p = pane(&["let value = 1;"]);
        p.cur_col = 9;
        assert!(p.select_word_at());
        assert_eq!(p.sel_range(), Some(((0, 4), (0, 9))));
        // 줄 시작
        let mut p = pane(&["let value = 1;"]);
        p.cur_col = 0;
        assert!(p.select_word_at());
        assert_eq!(p.sel_range(), Some(((0, 0), (0, 3))));
    }

    #[test]
    fn select_word_at_on_an_empty_line_changes_nothing() {
        let mut p = pane(&[""]);
        assert!(!p.select_word_at());
        assert_eq!(p.sel_anchor, None);
    }

    #[test]
    fn move_lines_swaps_with_the_neighbour_and_carries_the_caret() {
        let mut p = pane(&["a", "b", "c"]);
        p.cur_line = 1;
        assert!(p.move_lines(true));
        assert_eq!(p.edit_lines.join("\n"), "b\na\nc");
        assert_eq!(p.cur_line, 0);
        assert!(p.move_lines(false));
        assert_eq!(p.edit_lines.join("\n"), "a\nb\nc");
        assert_eq!(p.cur_line, 1);
    }

    #[test]
    fn select_line_at_grabs_the_whole_line() {
        let mut p = pane(&["  hello world", "x"]);
        p.cur_col = 5;
        assert!(p.select_line_at());
        assert_eq!(p.sel_anchor, Some((0, 0)));
        assert_eq!(p.cur_col, 13);
    }

    #[test]
    fn click_streak_counts_only_the_same_spot_and_wraps_after_three() {
        use std::time::{Duration, Instant};
        let t0 = Instant::now();
        assert_eq!(click_streak(None, t0, 10.0, 10.0), 1);
        let one = Some((t0, 10.0, 10.0, 1));
        assert_eq!(
            click_streak(one, t0 + Duration::from_millis(100), 10.0, 10.0),
            2
        );
        // 너무 늦었으면 새 클릭
        assert_eq!(
            click_streak(one, t0 + Duration::from_millis(600), 10.0, 10.0),
            1
        );
        // 자리가 멀면 새 클릭
        assert_eq!(
            click_streak(one, t0 + Duration::from_millis(100), 40.0, 10.0),
            1
        );
        // 트리플 다음은 다시 1
        let three = Some((t0, 10.0, 10.0, 3));
        assert_eq!(
            click_streak(three, t0 + Duration::from_millis(100), 10.0, 10.0),
            1
        );
    }

    #[test]
    fn toggle_comment_turns_the_caret_line_on_and_off() {
        let mut p = pane(&["let x = 1;"]);
        p.cur_col = 4;
        assert!(p.toggle_comment("//"));
        assert_eq!(p.edit_lines.join("\n"), "// let x = 1;");
        assert_eq!(p.cur_col, 7);
        assert!(p.toggle_comment("//"));
        assert_eq!(p.edit_lines.join("\n"), "let x = 1;");
        assert_eq!(p.cur_col, 4);
    }

    #[test]
    fn toggle_comment_aligns_a_block_to_its_shallowest_indent() {
        let mut p = pane(&["    a", "  b", "      c"]);
        p.sel_anchor = Some((0, 0));
        p.cur_line = 2;
        assert!(p.toggle_comment("//"));
        assert_eq!(p.edit_lines.join("\n"), "  //   a\n  // b\n  //     c");
    }

    #[test]
    fn toggle_comment_leaves_blank_lines_alone() {
        let mut p = pane(&["a", "", "b"]);
        p.sel_anchor = Some((0, 0));
        p.cur_line = 2;
        assert!(p.toggle_comment("#"));
        assert_eq!(p.edit_lines.join("\n"), "# a\n\n# b");
    }

    #[test]
    fn toggle_comment_uncomments_only_when_every_line_already_is() {
        let mut p = pane(&["// a", "b"]);
        p.sel_anchor = Some((0, 0));
        p.cur_line = 1;
        assert!(p.toggle_comment("//"));
        assert_eq!(p.edit_lines.join("\n"), "// // a\n// b");
    }

    #[test]
    fn delete_lines_removes_the_block_and_never_empties_the_buffer() {
        let mut p = pane(&["a", "b", "c"]);
        p.sel_anchor = Some((0, 0));
        p.cur_line = 1;
        assert!(p.delete_lines());
        assert_eq!(p.edit_lines.join("\n"), "c");
        assert_eq!((p.cur_line, p.cur_col), (0, 0));
        assert!(p.delete_lines());
        assert_eq!(p.edit_lines.len(), 1);
        assert_eq!(p.edit_lines[0], "");
    }

    #[test]
    fn open_line_inherits_indent_without_splitting_the_line() {
        let mut p = pane(&["    let x = 1;"]);
        p.cur_col = 8;
        p.open_line(false);
        assert_eq!(p.edit_lines.join("\n"), "    let x = 1;\n    ");
        assert_eq!((p.cur_line, p.cur_col), (1, 4));
        p.open_line(true);
        assert_eq!(p.edit_lines.len(), 3);
        assert_eq!((p.cur_line, p.cur_col), (1, 4));
    }

    #[test]
    fn cut_without_a_selection_takes_the_whole_line() {
        let mut p = pane(&["a", "b"]);
        assert_eq!(p.take_copy(true).as_deref(), Some("a\n"));
        assert_eq!(p.edit_lines.join("\n"), "b");
        assert_eq!(p.take_copy(false).as_deref(), Some("b\n"));
        assert_eq!(p.edit_lines.join("\n"), "b");
    }

    #[test]
    fn line_comment_prefix_refuses_formats_that_have_none() {
        assert_eq!(line_comment_prefix("rust"), Some("//"));
        assert_eq!(line_comment_prefix("python"), Some("#"));
        assert_eq!(line_comment_prefix("sql"), Some("--"));
        assert_eq!(line_comment_prefix("css"), None);
        assert_eq!(line_comment_prefix("html"), None);
        assert_eq!(line_comment_prefix(""), None);
    }

    #[test]
    fn longest_cols_counts_cells_and_survives_only_until_the_buffer_moves() {
        let mut m = pane(&["ab", "가나다"]);
        // 한글 3자 = 6칸 > ASCII 2칸.
        assert_eq!(m.longest_cols(), 6);
        assert_eq!(m.longest_cache, Some(6));
        // 편집은 lines_mut 을 지나므로 캐시가 버려진다.
        m.lines_mut()[0] = "abcdefghij".to_string();
        assert_eq!(m.longest_cache, None);
        assert_eq!(m.longest_cols(), 10);
        // undo 는 버퍼를 통째로 갈아끼우는데, 그 경로도 캐시를 버려야 한다.
        m.push_undo(EditKind::Other);
        m.lines_mut()[0] = "x".to_string();
        m.do_undo();
        assert_eq!(m.longest_cache, None);
        assert_eq!(m.longest_cols(), 10);
    }

    #[test]
    fn complete_refresh_needs_two_chars_and_accept_swaps_the_word() {
        let mut m = pane(&["let counter = 0;", "let cost = 1;", "co"]);
        m.cur_line = 2;
        m.cur_col = 2;
        m.complete_refresh();
        let c = m.complete.as_ref().expect("두 글자면 팝업이 열려야 한다");
        // 가까운 줄이 앞 — cost(1줄 차) 가 counter(2줄 차) 보다 먼저.
        assert_eq!(c.items, vec!["cost", "counter"]);
        assert_eq!((c.sel, c.from_col), (0, 0));
        assert!(m.complete_accept());
        assert_eq!(*m.edit_lines, vec!["let counter = 0;", "let cost = 1;", "cost"]);
        assert_eq!(m.cur_col, 4);
        assert!(m.complete.is_none());
        // 확정할 팝업이 없으면 아무 일도 없다.
        assert!(!m.complete_accept());
    }

    #[test]
    fn complete_refresh_stays_shut_for_one_char_or_no_match() {
        let mut m = pane(&["let counter = 0;", "c"]);
        m.cur_line = 1;
        m.cur_col = 1;
        m.complete_refresh();
        assert!(m.complete.is_none(), "한 글자로는 후보가 버퍼 절반이라 방해다");
        let mut z = pane(&["let counter = 0;", "zz"]);
        z.cur_line = 1;
        z.cur_col = 2;
        z.complete_refresh();
        assert!(z.complete.is_none());
    }

    #[test]
    fn complete_accept_keeps_the_tail_of_the_line() {
        let mut m = pane(&["fn handler() {}", "ha();"]);
        m.cur_line = 1;
        m.cur_col = 2;
        m.complete_refresh();
        assert!(m.complete_accept());
        // 낱말만 갈아끼우고 뒤에 이미 친 `();` 는 그대로 남는다.
        assert_eq!(m.edit_lines[1], "handler();");
        assert_eq!(m.cur_col, 7);
    }

    #[test]
    fn word_completions_prefers_nearby_lines_and_drops_the_prefix_itself() {
        let lines: Vec<String> = [
            "let counter = 0;",   // 0
            "let cost = 1;",      // 1
            "co",                 // 2  캐럿 줄 — 자기 자신은 후보가 아니다
            "let coffee = 2;",    // 3
            "counter += 1;",      // 4  이미 나온 낱말은 중복으로 안 넣는다
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert_eq!(
            word_completions(&lines, "co", 2, 10),
            vec!["cost", "coffee", "counter"]
        );
        // limit 을 지킨다.
        assert_eq!(word_completions(&lines, "co", 2, 1), vec!["cost"]);
        // 빈 prefix 로는 후보를 만들지 않는다 — 버퍼 전체가 쏟아진다.
        assert!(word_completions(&lines, "", 0, 10).is_empty());
    }

    #[test]
    fn word_completions_takes_hangul_identifiers() {
        let lines: Vec<String> = ["let 한글변수 = 1;", "let 한글이름 = 2;"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(
            word_completions(&lines, "한글", 0, 10),
            vec!["한글변수", "한글이름"]
        );
    }

    #[test]
    fn find_after_walks_forward_then_wraps_around() {
        let lines: Vec<String> = ["foo bar foo", "baz", "foo"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        // 같은 줄 뒤쪽
        assert_eq!(find_after(&lines, "foo", (0, 1)), Some((0, 8)));
        // 아래 줄로
        assert_eq!(find_after(&lines, "foo", (0, 9)), Some((2, 0)));
        // 문서 끝에서 한 바퀴 돌아 앞으로
        assert_eq!(find_after(&lines, "foo", (2, 1)), Some((0, 0)));
        // 시작 줄의 from 앞쪽에서 한 바퀴가 닫힌다
        assert_eq!(find_after(&lines, "bar", (0, 5)), Some((0, 4)));
        assert_eq!(find_after(&lines, "nope", (0, 0)), None);
        assert_eq!(find_after(&lines, "", (0, 0)), None);
    }

    #[test]
    fn find_after_is_case_sensitive_and_counts_chars_not_bytes() {
        let lines: Vec<String> = ["Foo foo", "가나다 nap"].iter().map(|s| s.to_string()).collect();
        assert_eq!(find_after(&lines, "foo", (0, 0)), Some((0, 4)));
        // 열은 char 인덱스 — 한글이 앞에 있어도 바이트로 밀리지 않는다.
        assert_eq!(find_after(&lines, "nap", (1, 0)), Some((1, 4)));
    }

    #[test]
    fn fold_mapping_skips_hidden_lines_both_ways() {
        // 줄 2 를 머리로 3·4·5 가 접혔다.
        let f = vec![(2usize, 5usize)];
        assert!(!is_hidden(&f, 2));
        assert!(is_hidden(&f, 3) && is_hidden(&f, 5));
        assert!(!is_hidden(&f, 6));
        // 보이는 줄: 0 1 2 6 7 8 9 → 행 0..6
        assert_eq!(visual_row(&f, 0), 0);
        assert_eq!(visual_row(&f, 2), 2);
        assert_eq!(visual_row(&f, 6), 3);
        assert_eq!(visual_row(&f, 9), 6);
        // 숨은 줄은 머리 줄의 행에 얹힌다 — 캐럿이 화면 밖으로 사라지지 않는다.
        assert_eq!(visual_row(&f, 3), 2);
        assert_eq!(visual_row(&f, 5), 2);
        // 역변환은 보이는 줄만 가리킨다.
        assert_eq!(buffer_line(&f, 2, 10), 2);
        assert_eq!(buffer_line(&f, 3, 10), 6);
        assert_eq!(buffer_line(&f, 6, 10), 9);
    }

    #[test]
    fn fold_mapping_handles_several_ranges() {
        let f = vec![(1usize, 2usize), (5usize, 7usize)];
        // 보이는 줄: 0 1 3 4 5 8 9
        assert_eq!(visual_row(&f, 8), 5);
        assert_eq!(buffer_line(&f, 5, 10), 8);
        // 접힘이 없으면 항등 — 이 경로가 평소의 전부다.
        assert_eq!(visual_row(&[], 42), 42);
        assert_eq!(buffer_line(&[], 42, 100), 42);
    }

    #[test]
    fn fold_toggle_folds_unfolds_and_swallows_inner_ranges() {
        let mut f: Folds = Vec::new();
        assert!(fold_toggle(&mut f, 5, 9));
        assert_eq!(f, vec![(5, 9)]);
        // 같은 머리를 다시 누르면 펴진다.
        assert!(!fold_toggle(&mut f, 5, 9));
        assert!(f.is_empty());
        // 안쪽을 접은 뒤 바깥을 접으면 안쪽은 삼켜진다 — 안 그러면 바깥을 펼 때
        // 사람이 접은 적 없는 접힘이 남아 있다.
        fold_toggle(&mut f, 6, 8);
        fold_toggle(&mut f, 2, 12);
        assert_eq!(f, vec![(2, 12)]);
        // 겹치지 않는 구간은 나란히 산다(머리 줄 순서).
        fold_toggle(&mut f, 20, 25);
        fold_toggle(&mut f, 14, 16);
        assert_eq!(f, vec![(2, 12), (14, 16), (20, 25)]);
    }

    #[test]
    fn fold_end_spans_the_block_and_drops_the_trailing_blank() {
        let lines: Vec<String> = [
            "fn a() {",   // 0
            "  one();",   // 1
            "",           // 2  블록 중간 빈 줄 — 끊지 않는다
            "  two();",   // 3
            "",           // 4  블록 끝 빈 줄 — 포함하지 않는다
            "}",          // 5
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert_eq!(fold_end(&lines, 0), Some(3));
        // 자기보다 깊은 줄이 없으면 접을 게 없다.
        assert_eq!(fold_end(&lines, 3), None);
        assert_eq!(fold_end(&lines, 5), None);
        // 빈 줄에서는 접기가 시작되지 않는다.
        assert_eq!(fold_end(&lines, 2), None);
    }

    #[test]
    fn fold_end_nests_and_counts_a_tab_as_one_step() {
        let lines: Vec<String> = ["a", "  b", "    c", "  d", "e"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(fold_end(&lines, 0), Some(3));
        assert_eq!(fold_end(&lines, 1), Some(2));
        let tabs: Vec<String> = ["a", "\tb", "\t\tc", "d"].iter().map(|s| s.to_string()).collect();
        assert_eq!(fold_end(&tabs, 0), Some(2));
        assert_eq!(fold_end(&tabs, 1), Some(2));
    }

    #[test]
    fn normalize_carets_merges_the_same_spot() {
        let out = normalize_carets(vec![Caret::at(3, 2), Caret::at(1, 0), Caret::at(3, 2)]);
        assert_eq!(out, vec![Caret::at(1, 0), Caret::at(3, 2)]);
    }

    #[test]
    fn normalize_carets_joins_overlapping_selections_keeping_direction() {
        // 아래로 자라던 선택 + 겹치는 선택 → 하나로 넓히고 방향은 앞 것.
        let a = Caret { line: 1, col: 5, anchor: Some((0, 2)) };
        let b = Caret { line: 2, col: 1, anchor: Some((1, 3)) };
        assert_eq!(
            normalize_carets(vec![a, b]),
            vec![Caret { line: 2, col: 1, anchor: Some((0, 2)) }]
        );
        // 위로 자라던 선택이 앞이면 병합 후에도 위로 자란다.
        let c = Caret { line: 0, col: 2, anchor: Some((1, 5)) };
        assert_eq!(
            normalize_carets(vec![c, b]),
            vec![Caret { line: 0, col: 2, anchor: Some((2, 1)) }]
        );
    }

    #[test]
    fn normalize_carets_leaves_disjoint_ones_alone() {
        let a = Caret { line: 0, col: 3, anchor: Some((0, 1)) };
        let b = Caret { line: 5, col: 2, anchor: Some((5, 0)) };
        assert_eq!(normalize_carets(vec![b, a]), vec![a, b]);
        // 맞닿기만 한 두 범위는 합친다 — 사이에 글자가 없으면 두 커서를 따로
        // 둘 이유가 없고, 삭제 때 경계가 겹쳐 사고가 난다.
        let c = Caret { line: 0, col: 5, anchor: Some((0, 3)) };
        assert_eq!(
            normalize_carets(vec![a, c]),
            vec![Caret { line: 0, col: 5, anchor: Some((0, 1)) }]
        );
    }

    #[test]
    fn indent_guide_depth_counts_steps_and_skips_tabs() {
        let lines: Vec<String> = ["fn a() {", "  let b = 1;", "    if b {", "\tlegacy", ""]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(indent_guide_depth(&lines, 0), 0);
        assert_eq!(indent_guide_depth(&lines, 1), 1);
        assert_eq!(indent_guide_depth(&lines, 2), 2);
        // 탭이 섞인 줄은 그리지 않는다 — 탭 폭을 모르면 선이 글자와 어긋난다.
        assert_eq!(indent_guide_depth(&lines, 3), 0);
    }

    #[test]
    fn indent_guide_depth_carries_the_shallower_side_across_a_blank_line() {
        let lines: Vec<String> = ["    a", "", "  b"].iter().map(|s| s.to_string()).collect();
        // 위는 2단계, 아래는 1단계 → 얕은 쪽. 깊은 쪽을 쓰면 블록이 끝난 뒤에도
        // 선이 남는다.
        assert_eq!(indent_guide_depth(&lines, 1), 1);
        // 문서 끝의 빈 줄은 아래에 실줄이 없으니 잇지 않는다.
        let tail: Vec<String> = ["    a", ""].iter().map(|s| s.to_string()).collect();
        assert_eq!(indent_guide_depth(&tail, 1), 0);
    }

    #[test]
    fn plan_typed_pairs_only_when_the_right_side_is_free() {
        assert_eq!(plan_typed('(', None, None, false), TypeAction::Pair(')'));
        assert_eq!(plan_typed('{', Some(' '), Some(')'), false), TypeAction::Pair('}'));
        // 오른쪽에 낱말이 붙어 있으면 짝을 넣지 않는다.
        assert_eq!(plan_typed('(', None, Some('f'), false), TypeAction::Plain);
        assert_eq!(plan_typed('[', None, Some('가'), false), TypeAction::Plain);
        // 짝이 없는 글자는 그냥 글자다.
        assert_eq!(plan_typed('a', None, None, false), TypeAction::Plain);
    }

    #[test]
    fn plan_typed_overtypes_an_existing_closer() {
        assert_eq!(plan_typed(')', Some('a'), Some(')'), false), TypeAction::Overtype);
        assert_eq!(plan_typed('"', Some('a'), Some('"'), false), TypeAction::Overtype);
        // 오른쪽이 다른 글자면 넘어가지 않고 그대로 넣는다.
        assert_eq!(plan_typed(')', Some('a'), Some(';'), false), TypeAction::Plain);
    }

    #[test]
    fn plan_typed_leaves_an_apostrophe_inside_a_word_alone() {
        // don't 의 ' 가 짝을 끌고 오면 못 쓴다.
        assert_eq!(plan_typed('\'', Some('n'), Some('t'), false), TypeAction::Plain);
        assert_eq!(plan_typed('\'', Some('n'), None, false), TypeAction::Plain);
        // 낱말 밖이면 짝을 넣는다.
        assert_eq!(plan_typed('\'', Some(' '), None, false), TypeAction::Pair('\''));
    }

    #[test]
    fn plan_typed_wraps_a_selection_instead_of_replacing_it() {
        assert_eq!(plan_typed('(', None, Some('f'), true), TypeAction::Wrap(')'));
        assert_eq!(plan_typed('"', Some('n'), Some('t'), true), TypeAction::Wrap('"'));
        // 짝 없는 글자는 선택이 있어도 그냥 덮어쓴다.
        assert_eq!(plan_typed('a', None, None, true), TypeAction::Plain);
    }

    #[test]
    fn wrap_selection_keeps_the_text_and_the_selection() {
        let mut m = pane(&["foo bar"]);
        m.sel_anchor = Some((0, 4));
        m.cur_col = 7;
        m.wrap_selection('(', ')');
        assert_eq!(*m.edit_lines, vec!["foo (bar)"]);
        assert_eq!(m.sel_anchor, Some((0, 5)));
        assert_eq!((m.cur_line, m.cur_col), (0, 8));
        // 남은 선택이 감싼 내용 그대로여서 연속으로 감쌀 수 있다.
        m.wrap_selection('"', '"');
        assert_eq!(*m.edit_lines, vec!["foo (\"bar\")"]);
    }

    #[test]
    fn wrap_selection_spans_lines_without_shifting_the_far_end() {
        let mut m = pane(&["a", "b"]);
        m.sel_anchor = Some((0, 0));
        m.cur_line = 1;
        m.cur_col = 1;
        m.wrap_selection('{', '}');
        assert_eq!(*m.edit_lines, vec!["{a", "b}"]);
        assert_eq!((m.cur_line, m.cur_col), (1, 1));
    }

    #[test]
    fn newline_between_brackets_opens_a_block() {
        let mut m = pane(&["  fn a() {}"]);
        m.cur_col = 10;
        m.newline();
        assert_eq!(*m.edit_lines, vec!["  fn a() {", "    ", "  }"]);
        assert_eq!((m.cur_line, m.cur_col), (1, 4));
        // 따옴표는 블록이 아니다 — 문자열이 두 줄로 갈리면 깨진다.
        let mut q = pane(&["\"\""]);
        q.cur_col = 1;
        q.newline();
        assert_eq!(*q.edit_lines, vec!["\"", "\""]);
    }

    #[test]
    fn match_bracket_prefers_the_glyph_left_of_the_caret() {
        let lines = vec!["foo(bar)".to_string()];
        // 캐럿이 ')' 바로 오른쪽 — 방금 닫은 괄호의 짝이 보여야 한다.
        assert_eq!(match_bracket(&lines, 0, 8), Some(((0, 7), (0, 3))));
        // 캐럿이 '(' 왼쪽 — 왼쪽엔 괄호가 없으니 오른쪽을 본다.
        assert_eq!(match_bracket(&lines, 0, 3), Some(((0, 3), (0, 7))));
        assert_eq!(match_bracket(&lines, 0, 1), None);
    }

    #[test]
    fn match_bracket_counts_nesting_across_lines() {
        let lines = vec![
            "fn a() {".to_string(),
            "    if b {".to_string(),
            "    }".to_string(),
            "}".to_string(),
        ];
        assert_eq!(match_bracket(&lines, 0, 8), Some(((0, 7), (3, 0))));
        assert_eq!(match_bracket(&lines, 3, 1), Some(((3, 0), (0, 7))));
        assert_eq!(match_bracket(&lines, 1, 10), Some(((1, 9), (2, 4))));
    }

    #[test]
    fn match_bracket_gives_up_when_the_pair_is_missing() {
        let lines = vec!["a(b".to_string(), "c)d)".to_string()];
        // 뒤쪽 ')' 는 짝이 없다 — 앞의 ')' 가 유일한 '(' 를 이미 먹었다.
        assert_eq!(match_bracket(&lines, 1, 4), None);
        assert_eq!(match_bracket(&lines, 1, 2), Some(((1, 1), (0, 1))));
        let orphan = vec!["}".to_string()];
        assert_eq!(match_bracket(&orphan, 0, 1), None);
    }

    #[test]
    fn move_lines_at_the_edge_is_a_no_op() {
        let mut p = pane(&["a", "b"]);
        p.cur_line = 0;
        assert!(!p.move_lines(true));
        p.cur_line = 1;
        assert!(!p.move_lines(false));
        assert_eq!(p.edit_lines.join("\n"), "a\nb");
    }

    #[test]
    fn move_lines_moves_a_whole_selected_block_and_keeps_it_selected() {
        let mut p = pane(&["a", "b", "c", "d"]);
        p.sel_anchor = Some((1, 0));
        p.cur_line = 2;
        p.cur_col = 1;
        assert!(p.move_lines(false));
        assert_eq!(p.edit_lines.join("\n"), "a\nd\nb\nc");
        // 앵커까지 따라와야 옮긴 블록이 계속 선택된 채로 남는다.
        assert_eq!(p.sel_anchor, Some((2, 0)));
        assert_eq!(p.cur_line, 3);
    }

    #[test]
    fn duplicate_lines_lands_the_caret_on_the_copy_going_down() {
        let mut p = pane(&["x", "y"]);
        p.cur_line = 0;
        assert!(p.duplicate_lines(false));
        assert_eq!(p.edit_lines.join("\n"), "x\nx\ny");
        assert_eq!(p.cur_line, 1);
    }

    #[test]
    fn duplicate_lines_going_up_leaves_the_caret_on_the_upper_copy() {
        let mut p = pane(&["x", "y"]);
        p.cur_line = 1;
        assert!(p.duplicate_lines(true));
        assert_eq!(p.edit_lines.join("\n"), "x\ny\ny");
        assert_eq!(p.cur_line, 1);
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
        // 선택이 없으면 캐럿이 선 줄을 집는다(VS Code 의 Cmd+C/Cmd+X). 여기선
        // 방금 전부 잘라내 빈 줄 하나만 남았으니 개행 하나가 나온다.
        assert_eq!(m.take_copy(false).as_deref(), Some("\n"));
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
            scroll: 0.0,
            h_scroll: 0.0,
            modified: false,
            sel_anchor: None,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            last_edit: EditKind::Break,
            find: None,
            complete: None,
            longest_cache: None,
            edit_gen: 0,
            folds: Vec::new(),
            folds_gen: 0,
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

    /// 같은 폴더 우선, 없으면 한 단계 아래 폴더. 볼트가 주제 폴더로 갈라져 있어
    /// 인덱스의 `[[이름]]` 은 대개 아래 폴더를 가리킨다.
    #[test]
    fn wiki_target_prefers_same_dir_then_one_level_down() {
        let dir =
            std::env::temp_dir().join(format!("kasaterm-wiki-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("life")).unwrap();
        std::fs::create_dir_all(dir.join("aaa")).unwrap();
        std::fs::write(dir.join("here.md"), "").unwrap();
        std::fs::write(dir.join("life/deep.md"), "").unwrap();
        // 같은 이름이 두 폴더에 — 정렬 순서상 aaa 가 이겨야 한다(OS 폴더 순서에
        // 좌우되면 클릭마다 열리는 파일이 바뀐다).
        std::fs::write(dir.join("aaa/dup.md"), "").unwrap();
        std::fs::write(dir.join("life/dup.md"), "").unwrap();

        assert_eq!(wiki_target_in(&dir, "here"), Some(dir.join("here.md")));
        assert_eq!(wiki_target_in(&dir, "deep"), Some(dir.join("life/deep.md")));
        assert_eq!(wiki_target_in(&dir, "dup"), Some(dir.join("aaa/dup.md")));
        // 두 단계 아래는 일부러 안 찾는다.
        std::fs::create_dir_all(dir.join("life/inner")).unwrap();
        std::fs::write(dir.join("life/inner/buried.md"), "").unwrap();
        assert_eq!(wiki_target_in(&dir, "buried"), None);
        assert_eq!(wiki_target_in(&dir, "없는이름"), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
