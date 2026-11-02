use super::*;
use winit::keyboard::{KeyCode, PhysicalKey};
mod probe;
pub(crate) use probe::clipboard as verification_clipboard;

fn modifier_number(mods: ModifiersState) -> u8 {
    1 + u8::from(mods.shift_key()) + 2 * u8::from(mods.alt_key())
        + 4 * u8::from(mods.control_key()) + 8 * u8::from(mods.super_key())
}

fn prefix_to(cells: &[Vec<GridCell>], mark: (u16, u16), point: (u16, u16)) -> Option<String> {
    let ((row, col), (end_col, end_row)) = (mark, point);
    if end_row < row || (end_row == row && end_col < col) { return None; }
    let mut text = String::new();
    for r in row..=end_row {
        let cells = cells.get(r as usize)?;
        let start = if r == row { col as usize } else { 0 };
        let end = if r == end_row { end_col as usize } else { cells.len() };
        if start > end || end > cells.len() { return None; }
        let mut line = String::new();
        append_cells_text(cells.iter().take(end).skip(start), &mut line);
        if r < end_row && !cells.last().is_some_and(|c| c.wrapped) {
            line.truncate(line.trim_end_matches(' ').len());
            line.push('\n');
        }
        text.push_str(&line);
        if text.len() > 16_384 { return None; }
    }
    Some(text)
}

fn selection_range(cells: &[Vec<GridCell>], mark: (u16, u16), selection: Selection) -> Option<(usize, usize, String)> {
    let (mut start, mut end) = (selection.anchor, selection.end);
    if (start.1, start.0) > (end.1, end.0) { std::mem::swap(&mut start, &mut end); }
    if start.1 < mark.0 { return None; }
    if start.1 == mark.0 { start.0 = start.0.max(mark.1); }
    let row = cells.get(start.1 as usize)?;
    if start.0 > 0 && row.get(start.0 as usize - 1).is_some_and(|c| gpu::is_wide_char(c.ch)) {
        start.0 -= 1;
    }
    let before = prefix_to(cells, mark, start)?;
    let prefix = prefix_to(cells, mark, (end.0.checked_add(1)?, end.1))?;
    let (start, end) = (before.chars().count(), prefix.chars().count());
    (start < end).then_some((start, end, prefix))
}

fn selection_packet(range: (usize, usize, String), mode: &str, replacement: &str) -> Option<Vec<u8>> {
    if replacement.contains('\0') { return None; }
    let hex = |s: &str| s.as_bytes().iter().map(|b| format!("{b:02x}")).collect::<String>();
    let (start, end, prefix) = range;
    Some(format!("\x1b[9001~{start};{end};{mode};{};{}\x1b[201~", hex(&prefix), hex(replacement)).into_bytes())
}

pub(crate) fn arrow_bytes(letter: char, mods: ModifiersState, app_cursor: bool, alt_screen: bool) -> Vec<u8> {
    let shift = mods.shift_key();
    let ctrl = mods.control_key();
    let alt = mods.alt_key();
    let command = mods.super_key();
    if command && !shift && !ctrl && !alt {
        match letter {
            'D' => return if alt_screen { b"\x1b[H".to_vec() } else { vec![1] },
            'C' => return if alt_screen { b"\x1b[F".to_vec() } else { vec![5] },
            _ => {}
        }
    }
    if alt && !shift && !ctrl && !command && !alt_screen {
        match letter {
            'D' => return b"\x1bb".to_vec(),
            'C' => return b"\x1bf".to_vec(),
            _ => {}
        }
    }
    let modifier = modifier_number(mods);
    if modifier == 1 {
        format!("\x1b{}{letter}", if app_cursor { 'O' } else { '[' }).into_bytes()
    } else {
        format!("\x1b[1;{modifier}{letter}").into_bytes()
    }
}

fn editor_bytes(key: &Key, physical: PhysicalKey, mods: ModifiersState) -> Option<Vec<u8>> {
    // These chords belong to the pane layout, including when ZLE owns the input line.
    if mods.super_key() && mods.alt_key() { return None; }
    if mods.super_key() && !mods.control_key() {
        if let PhysicalKey::Code(code) = physical {
            let letter = match code {
                KeyCode::KeyA if !mods.shift_key() => Some(97),
                KeyCode::KeyC if !mods.shift_key() => Some(99),
                KeyCode::KeyX if !mods.shift_key() => Some(120),
                KeyCode::KeyZ => Some(122),
                _ => None,
            };
            if let Some(letter) = letter {
                return Some(format!("\x1b[{letter};{}u", modifier_number(mods)).into_bytes());
            }
        }
    }
    let bytes = match key {
        Key::Named(NamedKey::ArrowLeft | NamedKey::ArrowRight | NamedKey::ArrowUp | NamedKey::ArrowDown) => {
            let letter = match key {
                Key::Named(NamedKey::ArrowLeft) => 'D',
                Key::Named(NamedKey::ArrowRight) => 'C',
                Key::Named(NamedKey::ArrowUp) => 'A',
                _ => 'B',
            };
            if mods.is_empty() { format!("\x1b[{letter}").into_bytes() }
            else if mods.alt_key() && !mods.shift_key() && !mods.control_key() && !mods.super_key() {
                arrow_bytes(letter, mods, false, false)
            } else { format!("\x1b[1;{}{letter}", modifier_number(mods)).into_bytes() }
        }
        Key::Named(NamedKey::Home | NamedKey::End) => {
            let end = matches!(key, Key::Named(NamedKey::End));
            let letter = if mods.control_key() || mods.super_key() {
                if end { 'B' } else { 'A' }
            } else if end { 'C' } else { 'D' };
            format!("\x1b[1;{}{letter}", 9 + u8::from(mods.shift_key())).into_bytes()
        }
        Key::Named(NamedKey::Backspace) if mods.super_key() => b"\x1b[127;9u".to_vec(),
        Key::Named(NamedKey::Delete) if mods.super_key() => b"\x1b[3;9~".to_vec(),
        Key::Named(NamedKey::Delete) if mods.alt_key() || mods.control_key() => b"\x1b[3;5~".to_vec(),
        Key::Named(NamedKey::Enter) if !mods.super_key() && !mods.control_key() && (mods.shift_key() || mods.alt_key()) => {
            b"\x1b[13;2u".to_vec()
        }
        Key::Named(NamedKey::Escape) if mods.is_empty() => b"\x1b[27;1u".to_vec(),
        _ => return None,
    };
    Some(bytes)
}

impl App {
    pub(crate) fn active_shell_editor(&self) -> Option<String> {
        let surface = self.target_surface()?;
        if !self.pty.get(&surface).is_some_and(|p| p.shell_editor_active()) { return None; }
        let ws = self.ws.lock().ok()?;
        let outer = ws.outer_for_pty(&surface)?;
        let term = ws.panes.get(&outer)?.tab_for_pid(&surface).term()?;
        (!term.alt_screen).then_some(surface)
    }

    pub(crate) fn shell_editor_has_selection(&self) -> bool {
        self.active_shell_editor().and_then(|id| self.pty.get(&id))
            .is_some_and(|p| p.shell_editor_selection())
    }

    pub(crate) fn cancel_shell_editor_selection(&self) {
        if self.shell_editor_has_selection() { self.send_bytes(b"\x1b[27;1u"); }
    }

    pub(crate) fn shell_accept_suggestion(&self, suffix: &str, motion: &str) -> bool {
        let Some(surface) = self.active_shell_editor() else { return false };
        let packet = self.shell_prompt_grid(&surface)
            .and_then(|prefix| selection_packet((0, prefix.chars().count(), prefix), &format!("suggest-{motion}"), suffix));
        let fallback: &[u8] = match motion {
            "word" => b"\x1bf",
            "end" => b"\x1b[F",
            _ => b"\x1b[C",
        };
        self.send_bytes_to_surface(Some(&surface), packet.as_deref().unwrap_or(fallback));
        true
    }

    fn shell_mouse_range(&self) -> Option<(usize, usize, String)> {
        let selection = self.selection?;
        let surface = self.active_shell_editor()?;
        let ws = self.ws.lock().ok()?;
        let outer = ws.outer_for_pty(&surface)?;
        let term = ws.panes.get(&outer)?.tab_for_pid(&surface).term()?;
        if term.scroll_offset != 0 { return None; }
        selection_range(&term.cells, term.prompt_end?, selection)
    }

    pub(crate) fn shell_edit_insert_text(&mut self, surface: Option<&str>, text: &str) -> bool {
        if self.selection.is_none() { return false; }
        let Some(active) = self.active_shell_editor() else { return false };
        if surface.is_some_and(|surface| surface != active) { return false; }
        let packet = self.shell_mouse_range().and_then(|r| selection_packet(r, "replace", text));
        self.selection = None;
        self.chrome_dirty = true;
        if let Some(packet) = packet {
            self.current_suggestion = None;
            self.input_buf.clear();
            self.send_bytes_to_surface(Some(&active), &packet);
            true
        } else { false }
    }

    pub(crate) fn shell_edit_key(&mut self, event: &KeyEvent) -> bool {
        let Some(surface) = self.active_shell_editor() else { return false };
        if matches!(event.logical_key, Key::Named(NamedKey::Backspace)) && !self.preedit.is_empty() { return false; }
        let code = match event.physical_key { PhysicalKey::Code(code) => Some(code), _ => None };
        if self.modifiers.super_key() && code == Some(KeyCode::KeyC) && self.selection.is_some() {
            return false;
        }
        if self.selection.is_some() && (matches!(event.logical_key, Key::Named(NamedKey::Backspace | NamedKey::Delete))
            || (self.modifiers.super_key() && code == Some(KeyCode::KeyX))) {
            let mode = if code == Some(KeyCode::KeyX) { "cut" } else { "replace" };
            let packet = self.shell_mouse_range().and_then(|r| selection_packet(r, mode, ""));
            self.selection = None;
            self.chrome_dirty = true;
            self.current_suggestion = None;
            if let Some(packet) = packet { self.send_bytes_to_surface(Some(&surface), &packet); }
            return true;
        }
        if self.selection.is_none() && self.modifiers.is_empty() && matches!(event.logical_key, Key::Named(NamedKey::ArrowRight | NamedKey::End))
            && !self.shell_editor_has_selection() && self.current_suggestion.is_some() {
            return false;
        }
        let Some(bytes) = editor_bytes(&event.logical_key, event.physical_key, self.modifiers) else { return false };
        if event.repeat && self.modifiers.super_key() && matches!(code, Some(KeyCode::KeyA | KeyCode::KeyC | KeyCode::KeyX)) {
            return true;
        }
        let mouse_selection = self.shell_mouse_range().and_then(|r| selection_packet(r, "select", ""));
        let mut payload = mouse_selection.unwrap_or_default();
        payload.extend_from_slice(self.hangul.flush().unwrap_or_default().as_bytes());
        payload.extend_from_slice(&bytes);
        self.preedit.clear();
        self.in_preedit = false;
        self.input_buf.clear();
        self.current_suggestion = None;
        self.selection = None;
        self.follow_live_tail_now();
        self.send_bytes_to_surface(Some(&surface), &payload);
        self.chrome_dirty = true;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(text: &str) -> Vec<GridCell> {
        text.chars().map(|ch| GridCell { ch, ..GridCell::blank() }).collect()
    }

    #[test]
    fn mouse_ranges_count_input_characters_across_wraps_and_reject_output() {
        let mut first = row("p> abcde");
        first.last_mut().unwrap().wrapped = true;
        let cells = vec![first, row("fgh xyz ")];
        let selection = Selection { anchor: (6, 0), end: (2, 1) };
        assert_eq!(selection_range(&cells, (0, 3), selection), Some((3, 8, "abcdefgh".into())));
        assert_eq!(selection_range(&cells, (0, 3), Selection { anchor: selection.end, end: selection.anchor }),
            Some((3, 8, "abcdefgh".into())));
        assert!(selection_range(&cells, (1, 3), selection).is_none());
        assert_eq!(selection_range(&[row("p> 가\0나\0")], (0, 3), Selection { anchor: (4, 0), end: (6, 0) }),
            Some((0, 2, "가나".into())));
    }

    #[test]
    fn mouse_edit_packets_never_contain_a_command_submission_even_for_multiline_paste() {
        let packet = selection_packet((0, 3, "abc".into()), "replace", "새 글\nnext\rline").unwrap();
        assert!(packet.starts_with(b"\x1b[9001~0;3;replace;"));
        assert!(packet.ends_with(b"\x1b[201~"));
        assert!(!packet.iter().any(|b| matches!(b, b'\r' | b'\n')));
    }

    #[test]
    fn shifted_arrows_keep_every_modifier_instead_of_turning_into_history_or_word_motion() {
        let shift = ModifiersState::SHIFT;
        assert_eq!(arrow_bytes('A', shift, true, false), b"\x1b[1;2A");
        assert_eq!(arrow_bytes('D', shift | ModifiersState::ALT, false, false), b"\x1b[1;4D");
        assert_eq!(arrow_bytes('C', shift | ModifiersState::CONTROL, false, false), b"\x1b[1;6C");
        assert_eq!(arrow_bytes('A', ModifiersState::SUPER, false, false), b"\x1b[1;9A");
        assert_eq!(arrow_bytes('A', ModifiersState::empty(), true, false), b"\x1bOA");
        assert_eq!(arrow_bytes('D', ModifiersState::ALT, false, false), b"\x1bb");
    }

    #[test]
    fn shell_editing_preserves_pane_chords_and_uses_physical_letters_for_korean_layouts() {
        let left = Key::Named(NamedKey::ArrowLeft);
        assert!(editor_bytes(&left, PhysicalKey::Code(KeyCode::ArrowLeft), ModifiersState::SUPER | ModifiersState::ALT).is_none());
        assert_eq!(editor_bytes(&Key::Character("ㅁ".into()), PhysicalKey::Code(KeyCode::KeyA), ModifiersState::SUPER),
            Some(b"\x1b[97;9u".to_vec()));
        assert_eq!(editor_bytes(&Key::Named(NamedKey::Enter), PhysicalKey::Code(KeyCode::Enter), ModifiersState::SHIFT),
            Some(b"\x1b[13;2u".to_vec()));
    }
}
