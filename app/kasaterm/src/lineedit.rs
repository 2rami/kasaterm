//! 입력칸의 편집 조작 — 커서 있는 삽입·삭제·이동과 macOS 표준 키.
//!
//! 앱에는 입력칸이 여럿 있다(방 이름 · 파일트리 이름짓기 · 경로 검색 · git 커밋 메시지 ·
//! 웹 주소 · 설정 칸). 편집기(`markdown.rs`)는 진짜 커서를 갖는데, 이 작은 칸들은 각자
//! `push_str`/`pop` 만 갖고 있어 **끝에만 붙고 끝에서만 지워졌다** — 가운데 오타 하나에
//! 뒤를 다 지워야 했다(사용자 2026-08-07: "편집기는 잘되려나 그걸 붙이면 좋을텐데").
//! 그 뒤에도 ⌘·⌥·⇧ 조합은 칸마다 삼켜서 ⌥← 낱말 이동·⇧ 선택·⌘C/⌘V 가 어느 칸에서도
//! 안 됐다(2026-10-19 「방 이름 바꾸는 거나 다른 키보드 상호작용 다 해 줘」).
//!
//! 칸마다 다시 짜지 않도록 조작을 여기 한 벌만 둔다. 각 칸은 자기 버퍼(`String`)와
//! 커서(문자 단위 인덱스)를 넘기고, Enter·Esc 처럼 칸마다 뜻이 다른 키만
//! [`LineEditAction`] 으로 돌려받아 스스로 처리한다.
//!
//! 커서는 **문자 단위**다. 바이트로 두면 한글에서 커서가 글자 가운데로 들어간다.
//!
//! 선택의 다른 끝(앵커)과 되돌리기 기록은 칸의 상태 구조체가 아니라 여기 하나만 둔다.
//! 키보드 초점은 한 번에 한 칸뿐이라 그 칸의 것만 있으면 되고, 칸마다 구조체에 필드를
//! 늘리면 칸이 늘 때마다 같은 배선을 또 해야 한다. 다른 칸 열쇠로 부르면 버린다.

use std::cell::RefCell;
use winit::keyboard::{Key, KeyCode, ModifiersState, NamedKey, PhysicalKey};

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
    /// 커서나 선택만 움직였다 — 내용은 그대로.
    Moved,
    /// Enter — 칸이 확정 동작을 해야 한다.
    Submit,
    /// Esc — 칸이 취소 동작을 해야 한다.
    Cancel,
    /// 이 칸이 다룰 키가 아니다.
    Ignored,
}

/// 편집에 쓰는 수정키. 맥은 ⌘ 가 단축키, ⌥ 가 낱말, ⌃ 가 이맥스 줄 이동이고, 그 밖의
/// OS 는 Ctrl 하나가 단축키와 낱말을 같이 맡는다(Ctrl+← 낱말, Ctrl+A 전체).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Mods {
    pub(crate) host: bool,
    pub(crate) word: bool,
    pub(crate) shift: bool,
    pub(crate) emacs: bool,
}

impl Mods {
    pub(crate) fn of(m: ModifiersState) -> Self {
        Self::on(m, cfg!(target_os = "macos"))
    }

    pub(crate) fn on(m: ModifiersState, macos: bool) -> Self {
        if macos {
            Self { host: m.super_key(), word: m.alt_key(), shift: m.shift_key(), emacs: m.control_key() }
        } else {
            Self { host: m.control_key(), word: m.control_key(), shift: m.shift_key(), emacs: false }
        }
    }

    /// 글자로 넣으면 안 되는 조합 — 단축키 자리다.
    pub(crate) fn chord(self) -> bool {
        self.host || self.emacs
    }
}

/// 편집 대상 칸. `id` 가 바뀌면 선택·되돌리기 기록을 버린다.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Field<'a> {
    pub(crate) id: &'a str,
    /// 여러 줄 칸이면 붙여넣은 줄바꿈을 살리고, ⌘←→ 가 그 줄의 처음·끝으로 간다.
    pub(crate) multiline: bool,
}

pub(crate) const fn line(id: &str) -> Field<'_> {
    Field { id, multiline: false }
}

/// 클립보드. 검증 실행(`bootstrap::verification_run`)에서는 사람 클립보드를 안 건드리고
/// `KASATERM_TEST_COPY_PATH` 파일에 쓰고 `KASATERM_TEST_PASTE_TEXT` 를 붙여넣는다.
pub(crate) trait Clip {
    fn read(&mut self) -> Option<String>;
    fn write(&mut self, text: &str);
}

pub(crate) struct SystemClip;

impl Clip for SystemClip {
    fn read(&mut self) -> Option<String> {
        if crate::verification_run() {
            return std::env::var("KASATERM_TEST_PASTE_TEXT").ok();
        }
        arboard::Clipboard::new().ok()?.get_text().ok()
    }

    fn write(&mut self, text: &str) {
        if crate::verification_run() {
            if let Some(path) = std::env::var_os("KASATERM_TEST_COPY_PATH") {
                let _ = std::fs::write(path, text);
            }
            return;
        }
        if let Ok(mut clipboard) = arboard::Clipboard::new() {
            let _ = clipboard.set_text(text.to_string());
        }
    }
}

/// 칸 몫의 글을 클립보드에 — 검증 실행이면 시험 파일로.
pub(crate) fn copy_text(text: &str) {
    SystemClip.write(text);
}

/// 초점 칸 하나의 선택·되돌리기 기록.
#[derive(Default)]
struct State {
    anchor: Option<usize>,
    undo: Vec<(String, usize)>,
    redo: Vec<(String, usize)>,
    /// 이어 치는 글자는 되돌리기 한 번에 함께 물린다 — 한 글자씩 물리면 ⌘Z 를 수십 번 눌러야 한다.
    typing: bool,
}

const UNDO_KEEP: usize = 100;

thread_local! {
    /// 키 입력은 GUI 스레드에서만 온다. 스레드마다 두면 병렬 시험이 서로의 초점 칸을 덮지 않는다.
    static FOCUS: RefCell<Option<(String, State)>> = const { RefCell::new(None) };
}

fn with_state<R>(id: &str, f: impl FnOnce(&mut State) -> R) -> R {
    FOCUS.with_borrow_mut(|focus| {
        if focus.as_ref().map(|(at, _)| at.as_str()) != Some(id) {
            *focus = Some((id.to_string(), State::default()));
        }
        f(&mut focus.as_mut().expect("just set").1)
    })
}

/// 칸이 닫히거나 마우스로 커서를 옮겼을 때 — 남은 선택이 다음에 연 같은 칸에 묻어나지 않게.
pub(crate) fn forget(id: &str) {
    FOCUS.with_borrow_mut(|focus| {
        if focus.as_ref().is_some_and(|(at, _)| at == id) {
            *focus = None;
        }
    });
}

/// 그 칸의 선택(문자 범위, 시작 < 끝). 그리기가 바탕을 깐다.
pub(crate) fn selection(id: &str, cursor: usize, len: usize) -> Option<(usize, usize)> {
    let anchor = FOCUS.with_borrow(|focus| focus.as_ref().and_then(|(at, state)| state.anchor.filter(|_| at == id)))?;
    let (cursor, anchor) = (cursor.min(len), anchor.min(len));
    (anchor != cursor).then(|| (anchor.min(cursor), anchor.max(cursor)))
}

/// 전부 고른다(칸에 들어설 때 바로 바꿔 쓰게 하는 칸이 부른다). 커서는 끝으로.
pub(crate) fn select_all(id: &str, text: &str, cursor: &mut usize) {
    let len = text.chars().count();
    *cursor = len;
    with_state(id, |state| state.anchor = (len > 0).then_some(0));
}

/// `anchor` 부터 지금 커서까지 고른다(이름 바꾸기가 확장자 앞까지만 고를 때).
pub(crate) fn select_from(id: &str, anchor: usize) {
    with_state(id, |state| state.anchor = Some(anchor));
}

/// 고른 글을 지운다(밖에서 글을 넣기 직전 — 붙여넣기·입력기). 지웠으면 true.
pub(crate) fn delete_selection(id: &str, text: &mut String, cursor: &mut usize) -> bool {
    let Some((start, end)) = selection(id, *cursor, text.chars().count()) else { return false };
    with_state(id, |state| {
        remember(state, text, *cursor, false);
        state.anchor = None;
    });
    remove(text, start, end);
    *cursor = start;
    true
}

/// 입력칸 선택 바탕의 짙기(accent 알파). 설정 칸 전체 선택(42)보다 짙다 — 몇 글자만
/// 고른 띠는 좁아서 그 알파로는 글 아래 깔린 줄 모른다.
pub(crate) const BAND_ALPHA: u8 = 0x48;

/// 고른 글 아래 바탕을 깐다. `text` 는 그려질 글 그대로, `range` 는 그 안의 문자 범위.
/// `right` 를 넘는 부분(칸 밖으로 잘린 글)은 안 깐다.
#[allow(clippy::too_many_arguments)]
pub(crate) fn paint_band(
    g: &mut crate::gpu::GpuRenderer,
    text: &str,
    (start, end): (usize, usize),
    x: f32,
    y: f32,
    size: f32,
    bold: bool,
    right: f32,
) {
    let head: String = text.chars().take(start).collect();
    let upto: String = text.chars().take(end).collect();
    let x0 = x + g.measure_chrome_text(&head, size, bold);
    let x1 = (x + g.measure_chrome_text(&upto, size, bold)).min(right);
    if x1 > x0 {
        g.rect(x0, y - 2.0, x1 - x0, size + 4.0, crate::theme::with_alpha(crate::theme::accent(), BAND_ALPHA));
    }
}

/// 라벨 자리에 그대로 그리는 칸(방 이름)이 고른 범위를 글에 실어 보내는 표식. 글 안에서만
/// 오가므로 사람이 쓰지 않는 사용자 영역 글자를 쓴다.
const MARK_OPEN: char = '\u{E000}';
const MARK_CLOSE: char = '\u{E001}';

/// 커서 자리에 조합 중인 글자와 캐럿(▌)을 넣고, 고른 데가 있으면 표식으로 감싼 라벨 글.
pub(crate) fn caret_label(id: &str, text: &str, cursor: usize, composing: &str) -> String {
    let len = text.chars().count();
    let cursor = cursor.min(len);
    let (before, after) = split(text, cursor);
    match selection(id, cursor, len) {
        Some((start, end)) if end == cursor => {
            let (head, picked) = split(&before, start);
            format!("{head}{MARK_OPEN}{picked}{MARK_CLOSE}{composing}\u{258c}{after}")
        }
        Some((_, end)) => {
            let (picked, tail) = split(&after, end - cursor);
            format!("{before}{composing}\u{258c}{MARK_OPEN}{picked}{MARK_CLOSE}{tail}")
        }
        None => format!("{before}{composing}\u{258c}{after}"),
    }
}

/// [`caret_label`] 의 표식을 걷은 글과 고른 문자 범위.
pub(crate) fn unmark(label: &str) -> (String, Option<(usize, usize)>) {
    if !label.contains(MARK_OPEN) {
        return (label.to_string(), None);
    }
    let (mut clean, mut start, mut end, mut n) = (String::new(), None, None, 0);
    for ch in label.chars() {
        match ch {
            MARK_OPEN => start = Some(n),
            MARK_CLOSE => end = Some(n),
            _ => {
                clean.push(ch);
                n += 1;
            }
        }
    }
    (clean, start.zip(end))
}

/// 커서 자리에 글자를 넣고 커서를 그만큼 민다. 선택은 모르는 낮은 층 — 칸이 고른 것을
/// 바꿔 쓰게 하려면 [`insert_in`].
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

/// 조합이 끝난 한글·입력기 글자를 넣는다. 고른 것이 있으면 그것을 바꾼다.
pub(crate) fn insert_in(field: Field<'_>, text: &mut String, cursor: &mut usize, s: &str) {
    if s.is_empty() {
        return;
    }
    with_state(field.id, |state| {
        remember(state, text, *cursor, true);
        let len = text.chars().count();
        if let Some((start, end)) = state.anchor.take().map(|a| a.min(len)).filter(|a| *a != (*cursor).min(len))
            .map(|a| (a.min((*cursor).min(len)), a.max((*cursor).min(len))))
        {
            remove(text, start, end);
            *cursor = start;
        }
        insert(text, cursor, s);
    });
}

fn remove(text: &mut String, start: usize, end: usize) {
    let b0 = crate::char_byte(text, start);
    let b1 = crate::char_byte(text, end);
    text.replace_range(b0..b1, "");
}

/// 그 조합을 편집기가 맡나 — 수정키를 쥔 키를 칸에 줄지 앱 단축키로 둘지 가를 때.
pub(crate) fn handles(key: &Key, code: Option<KeyCode>, mods: Mods) -> bool {
    if mods.host && matches!(code, Some(KeyCode::KeyA | KeyCode::KeyC | KeyCode::KeyX | KeyCode::KeyV | KeyCode::KeyZ)) {
        return true;
    }
    if mods.emacs && !mods.host {
        return matches!(code, Some(KeyCode::KeyA | KeyCode::KeyE | KeyCode::KeyB | KeyCode::KeyF | KeyCode::KeyD | KeyCode::KeyH | KeyCode::KeyK));
    }
    matches!(key, Key::Named(NamedKey::ArrowLeft | NamedKey::ArrowRight | NamedKey::Home | NamedKey::End | NamedKey::Backspace | NamedKey::Delete))
}

/// 키 이벤트를 그 칸에 넣는다 — 한글 조합을 먼저 흘려보낸 뒤에 부를 것.
pub(crate) fn edit_event(
    field: Field<'_>,
    text: &mut String,
    cursor: &mut usize,
    event: &winit::event::KeyEvent,
    mods: Mods,
) -> LineEditAction {
    let code = match event.physical_key {
        PhysicalKey::Code(code) => Some(code),
        PhysicalKey::Unidentified(_) => None,
    };
    // 붙여넣기를 누르고 있으면 OS 가 되풀이를 보낸다 — 한 번만 붙인다.
    if event.repeat && mods.host && code == Some(KeyCode::KeyV) {
        return LineEditAction::Moved;
    }
    edit(field, text, cursor, &event.logical_key, code, mods)
}

/// 키 하나를 그 칸에 넣는다. 단축키는 글자 대신 물리 키로 본다 — 한글 입력 상태에서 ⌘A 의
/// 글자는 「ㅁ」이다.
pub(crate) fn edit(
    field: Field<'_>,
    text: &mut String,
    cursor: &mut usize,
    key: &Key,
    code: Option<KeyCode>,
    mods: Mods,
) -> LineEditAction {
    with_state(field.id, |state| apply(state, field.multiline, text, cursor, key, code, mods, &mut SystemClip))
}

/// 수정키 없는 편집 키 하나 — 선택·클립보드를 안 쓰는 낮은 층(검증 도구가 쓴다).
pub(crate) fn key(text: &mut String, cursor: &mut usize, k: &Key) -> LineEditAction {
    let mut state = State::default();
    apply(&mut state, false, text, cursor, k, None, Mods::default(), &mut SystemClip)
}

/// 커서 앞/뒤로 가른 조각. 렌더가 「앞 → (조합 중 글자) → 뒤」 순으로 그리고 앞의
/// 끝에 캐럿을 세우면 커서가 가운데 있어도 자리가 맞는다.
pub(crate) fn split(text: &str, cursor: usize) -> (String, String) {
    let col = cursor.min(text.chars().count());
    let at = crate::char_byte(text, col);
    (text[..at].to_string(), text[at..].to_string())
}

fn remember(state: &mut State, text: &str, cursor: usize, typing: bool) {
    if typing && state.typing {
        return;
    }
    state.undo.push((text.to_string(), cursor));
    if state.undo.len() > UNDO_KEEP {
        state.undo.remove(0);
    }
    state.redo.clear();
    state.typing = typing;
}

fn line_start(chars: &[char], col: usize) -> usize {
    chars[..col].iter().rposition(|c| *c == '\n').map_or(0, |at| at + 1)
}

fn line_end(chars: &[char], col: usize) -> usize {
    chars[col..].iter().position(|c| *c == '\n').map_or(chars.len(), |at| col + at)
}

/// 붙여넣을 글. 한 줄 칸은 줄바꿈을 빈칸으로 접고 끝의 줄바꿈은 버린다 — 복사한 경로·명령의
/// 꼬리 줄바꿈이 칸에서 Enter 처럼 굴면 안 된다.
fn pasteable(raw: &str, multiline: bool) -> String {
    let raw = raw.replace("\r\n", "\n");
    if multiline {
        return raw;
    }
    raw.trim_end_matches(['\n', '\r']).replace(['\n', '\r'], " ")
}

#[allow(clippy::too_many_arguments)]
fn apply(
    state: &mut State,
    multiline: bool,
    text: &mut String,
    cursor: &mut usize,
    key: &Key,
    code: Option<KeyCode>,
    mods: Mods,
    clip: &mut dyn Clip,
) -> LineEditAction {
    let chars: Vec<char> = text.chars().collect();
    let len = chars.len();
    let col = (*cursor).min(len);
    *cursor = col;
    if state.anchor.is_some_and(|a| a > len) {
        state.anchor = None;
    }
    let sel = state.anchor.filter(|a| *a != col).map(|a| (a.min(col), a.max(col)));
    let picked = |(start, end): (usize, usize)| chars[start..end].iter().collect::<String>();

    // 바꿔 쓰기: 고른 것을 지우고 그 자리에 넣는다. 지울 것도 넣을 것도 없으면 그대로.
    let replace = |state: &mut State, text: &mut String, cursor: &mut usize, range: Option<(usize, usize)>, with: &str, typing: bool| {
        if range.is_none_or(|(s, e)| s == e) && with.is_empty() {
            return LineEditAction::Moved;
        }
        remember(state, text, *cursor, typing);
        if let Some((start, end)) = range {
            remove(text, start, end);
            *cursor = start;
        }
        insert(text, cursor, with);
        state.anchor = None;
        LineEditAction::Edited
    };
    let go = |state: &mut State, cursor: &mut usize, to: usize| {
        if mods.shift {
            state.anchor.get_or_insert(col);
        } else {
            state.anchor = None;
        }
        *cursor = to;
        state.typing = false;
        LineEditAction::Moved
    };

    if mods.host {
        match code {
            Some(KeyCode::KeyA) => {
                state.anchor = (len > 0).then_some(0);
                *cursor = len;
                return LineEditAction::Moved;
            }
            Some(KeyCode::KeyC) => {
                if let Some(range) = sel {
                    clip.write(&picked(range));
                }
                return LineEditAction::Moved;
            }
            Some(KeyCode::KeyX) => {
                let Some(range) = sel else { return LineEditAction::Moved };
                clip.write(&picked(range));
                return replace(state, text, cursor, Some(range), "", false);
            }
            Some(KeyCode::KeyV) => {
                let with = clip.read().map(|raw| pasteable(&raw, multiline)).unwrap_or_default();
                return replace(state, text, cursor, sel, &with, false);
            }
            Some(KeyCode::KeyZ) => {
                let (from, to) = if mods.shift { (&mut state.redo, &mut state.undo) } else { (&mut state.undo, &mut state.redo) };
                let Some((before, at)) = from.pop() else { return LineEditAction::Moved };
                to.push((std::mem::replace(text, before), col));
                *cursor = at.min(text.chars().count());
                state.anchor = None;
                state.typing = false;
                return LineEditAction::Edited;
            }
            _ => {}
        }
    }
    if mods.emacs && !mods.host {
        return match code {
            Some(KeyCode::KeyA) => go(state, cursor, line_start(&chars, col)),
            Some(KeyCode::KeyE) => go(state, cursor, line_end(&chars, col)),
            Some(KeyCode::KeyB) => go(state, cursor, col.saturating_sub(1)),
            Some(KeyCode::KeyF) => go(state, cursor, (col + 1).min(len)),
            Some(KeyCode::KeyD) => replace(state, text, cursor, Some(sel.unwrap_or((col, (col + 1).min(len)))), "", false),
            Some(KeyCode::KeyH) => replace(state, text, cursor, Some(sel.unwrap_or((col.saturating_sub(1), col))), "", false),
            Some(KeyCode::KeyK) => replace(state, text, cursor, Some(sel.unwrap_or((col, line_end(&chars, col)))), "", false),
            _ => LineEditAction::Ignored,
        };
    }
    match key {
        Key::Named(NamedKey::Enter) => LineEditAction::Submit,
        Key::Named(NamedKey::Escape) => {
            state.anchor = None;
            LineEditAction::Cancel
        }
        Key::Named(NamedKey::ArrowLeft) => {
            let to = if mods.word {
                crate::markdown::prev_word_col(&chars, col)
            } else if mods.host {
                line_start(&chars, col)
            } else if let (Some((start, _)), false) = (sel, mods.shift) {
                start
            } else {
                col.saturating_sub(1)
            };
            go(state, cursor, to)
        }
        Key::Named(NamedKey::ArrowRight) => {
            let to = if mods.word {
                crate::markdown::next_word_col(&chars, col)
            } else if mods.host {
                line_end(&chars, col)
            } else if let (Some((_, end)), false) = (sel, mods.shift) {
                end
            } else {
                (col + 1).min(len)
            };
            go(state, cursor, to)
        }
        Key::Named(NamedKey::Home) => go(state, cursor, if multiline { line_start(&chars, col) } else { 0 }),
        Key::Named(NamedKey::End) => go(state, cursor, if multiline { line_end(&chars, col) } else { len }),
        Key::Named(NamedKey::Backspace) => {
            let range = sel.unwrap_or_else(|| {
                let from = if mods.host {
                    line_start(&chars, col)
                } else if mods.word {
                    crate::markdown::prev_word_col(&chars, col)
                } else {
                    col.saturating_sub(1)
                };
                (from, col)
            });
            replace(state, text, cursor, Some(range), "", false)
        }
        Key::Named(NamedKey::Delete) => {
            let range = sel.unwrap_or_else(|| {
                let to = if mods.host {
                    line_end(&chars, col)
                } else if mods.word {
                    crate::markdown::next_word_col(&chars, col)
                } else {
                    (col + 1).min(len)
                };
                (col, to)
            });
            replace(state, text, cursor, Some(range), "", false)
        }
        _ if mods.host => LineEditAction::Ignored,
        Key::Named(NamedKey::Space) => replace(state, text, cursor, sel, " ", true),
        Key::Character(c) => replace(state, text, cursor, sel, c, true),
        _ => LineEditAction::Ignored,
    }
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
        let bs = Key::Named(NamedKey::Backspace);
        assert_eq!(key(&mut t, &mut c, &bs), LineEditAction::Edited);
        assert_eq!((t.as_str(), c), ("한", 1));
        assert_eq!(key(&mut t, &mut c, &bs), LineEditAction::Edited);
        assert_eq!((t.as_str(), c), ("", 0));
        assert_eq!(key(&mut t, &mut c, &bs), LineEditAction::Moved);
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

    #[derive(Default)]
    struct Board(Option<String>);
    impl Clip for Board {
        fn read(&mut self) -> Option<String> {
            self.0.clone()
        }
        fn write(&mut self, text: &str) {
            self.0 = Some(text.to_string());
        }
    }

    /// 칸 하나에 키를 차례로 넣는 시험대.
    struct Pad {
        state: State,
        text: String,
        cursor: usize,
        clip: Board,
        multiline: bool,
    }

    const OPT: Mods = Mods { host: false, word: true, shift: false, emacs: false };
    const CMD: Mods = Mods { host: true, word: false, shift: false, emacs: false };
    const SHIFT: Mods = Mods { host: false, word: false, shift: true, emacs: false };
    const CTRL: Mods = Mods { host: false, word: false, shift: false, emacs: true };

    fn shift(m: Mods) -> Mods {
        Mods { shift: true, ..m }
    }

    impl Pad {
        fn new(text: &str, cursor: usize) -> Self {
            Self { state: State::default(), text: text.into(), cursor, clip: Board::default(), multiline: false }
        }
        fn named(&mut self, k: NamedKey, mods: Mods) -> LineEditAction {
            apply(&mut self.state, self.multiline, &mut self.text, &mut self.cursor, &Key::Named(k), None, mods, &mut self.clip)
        }
        fn chord(&mut self, code: KeyCode, letter: &str, mods: Mods) -> LineEditAction {
            apply(&mut self.state, self.multiline, &mut self.text, &mut self.cursor, &Key::Character(letter.into()), Some(code), mods, &mut self.clip)
        }
        fn typed(&mut self, s: &str) {
            for c in s.chars() {
                self.chord(KeyCode::KeyQ, &c.to_string(), Mods::default());
            }
        }
        fn picked(&self) -> Option<(usize, usize)> {
            self.state.anchor.filter(|a| *a != self.cursor).map(|a| (a.min(self.cursor), a.max(self.cursor)))
        }
    }

    #[test]
    fn option_arrows_hop_words_and_command_arrows_hit_the_ends() {
        let mut p = Pad::new("방 이름 kasa-term 바꾸기", 0);
        p.named(NamedKey::ArrowRight, OPT);
        assert_eq!(p.cursor, 1, "한글 낱말 끝");
        p.named(NamedKey::ArrowRight, OPT);
        assert_eq!(p.cursor, 4);
        p.named(NamedKey::ArrowRight, OPT);
        assert_eq!(p.cursor, 9, "하이픈 앞에서 멈춘다");
        p.named(NamedKey::ArrowLeft, OPT);
        assert_eq!(p.cursor, 5);
        p.named(NamedKey::ArrowRight, CMD);
        assert_eq!(p.cursor, p.text.chars().count());
        p.named(NamedKey::ArrowLeft, CMD);
        assert_eq!(p.cursor, 0);
        assert_eq!(p.chord(KeyCode::KeyE, "e", CTRL), LineEditAction::Moved);
        assert_eq!(p.cursor, p.text.chars().count());
        p.chord(KeyCode::KeyA, "a", CTRL);
        assert_eq!(p.cursor, 0);
    }

    #[test]
    fn shift_extends_and_plain_arrows_collapse_to_the_edge() {
        let mut p = Pad::new("alpha beta gamma", 6);
        p.named(NamedKey::ArrowRight, shift(OPT));
        assert_eq!(p.picked(), Some((6, 10)));
        p.named(NamedKey::ArrowRight, shift(OPT));
        assert_eq!(p.picked(), Some((6, 16)));
        p.named(NamedKey::ArrowLeft, SHIFT);
        assert_eq!(p.picked(), Some((6, 15)));
        p.named(NamedKey::ArrowLeft, Mods::default());
        assert_eq!((p.cursor, p.picked()), (6, None), "고른 채 ← 는 선택 앞끝으로 접힌다");
        p.named(NamedKey::ArrowLeft, shift(CMD));
        assert_eq!(p.picked(), Some((0, 6)));
    }

    #[test]
    fn typing_or_deleting_replaces_the_selection() {
        let mut p = Pad::new("alpha beta gamma", 6);
        p.named(NamedKey::ArrowRight, shift(OPT));
        p.typed("B");
        assert_eq!((p.text.as_str(), p.cursor), ("alpha B gamma", 7));
        p.named(NamedKey::ArrowLeft, shift(OPT));
        assert_eq!(p.named(NamedKey::Backspace, Mods::default()), LineEditAction::Edited);
        assert_eq!(p.text, "alpha  gamma");
    }

    #[test]
    fn word_and_line_deletes() {
        let mut p = Pad::new("git commit message", 18);
        p.named(NamedKey::Backspace, OPT);
        assert_eq!(p.text, "git commit ");
        p.named(NamedKey::Backspace, CMD);
        assert_eq!((p.text.as_str(), p.cursor), ("", 0));
        let mut p = Pad::new("one two three", 4);
        p.named(NamedKey::Delete, OPT);
        assert_eq!(p.text, "one  three");
        p.chord(KeyCode::KeyK, "k", CTRL);
        assert_eq!(p.text, "one ");
        let mut p = Pad::new("ab", 0);
        p.chord(KeyCode::KeyD, "d", CTRL);
        assert_eq!(p.text, "b");
    }

    #[test]
    fn select_all_copy_cut_paste_go_through_the_clipboard() {
        let mut p = Pad::new("첫 방", 1);
        // 한글 입력 상태의 ⌘A 는 글자가 「ㅁ」이다 — 물리 키로 본다.
        p.chord(KeyCode::KeyA, "ㅁ", CMD);
        assert_eq!(p.picked(), Some((0, 3)));
        p.chord(KeyCode::KeyC, "ㅊ", CMD);
        assert_eq!(p.clip.0.as_deref(), Some("첫 방"));
        assert_eq!(p.chord(KeyCode::KeyX, "ㅌ", CMD), LineEditAction::Edited);
        assert_eq!(p.text, "");
        p.clip.0 = Some("/Users/me/repo\n".into());
        p.chord(KeyCode::KeyV, "ㅍ", CMD);
        assert_eq!((p.text.as_str(), p.cursor), ("/Users/me/repo", 14), "꼬리 줄바꿈은 버린다");
        p.clip.0 = Some("a\nb".into());
        p.chord(KeyCode::KeyV, "v", CMD);
        assert_eq!(p.text, "/Users/me/repoa b", "한 줄 칸은 줄바꿈을 빈칸으로");
        let mut m = Pad::new("", 0);
        m.multiline = true;
        m.clip.0 = Some("a\r\nb".into());
        m.chord(KeyCode::KeyV, "v", CMD);
        assert_eq!(m.text, "a\nb");
    }

    #[test]
    fn unknown_command_chords_are_not_typed() {
        let mut p = Pad::new("x", 1);
        assert_eq!(p.chord(KeyCode::KeyT, "t", CMD), LineEditAction::Ignored);
        assert_eq!(p.chord(KeyCode::KeyL, "l", CTRL), LineEditAction::Ignored);
        assert_eq!(p.text, "x");
    }

    #[test]
    fn undo_takes_back_a_typing_run_at_once_and_redo_returns_it() {
        let mut p = Pad::new("방", 1);
        p.typed(" 이름");
        p.named(NamedKey::Backspace, OPT);
        assert_eq!(p.text, "방 ");
        p.chord(KeyCode::KeyZ, "z", CMD);
        assert_eq!(p.text, "방 이름");
        p.chord(KeyCode::KeyZ, "z", CMD);
        assert_eq!((p.text.as_str(), p.cursor), ("방", 1), "이어 친 글은 한 번에");
        p.chord(KeyCode::KeyZ, "z", shift(CMD));
        assert_eq!(p.text, "방 이름");
    }

    #[test]
    fn multiline_line_ends_stop_at_the_line() {
        let mut p = Pad::new("첫 줄\n둘째 줄", 7);
        p.multiline = true;
        p.named(NamedKey::ArrowLeft, CMD);
        assert_eq!(p.cursor, 4);
        p.named(NamedKey::End, Mods::default());
        assert_eq!(p.cursor, 8);
        p.named(NamedKey::ArrowLeft, CMD);
        p.named(NamedKey::Backspace, CMD);
        assert_eq!(p.text, "첫 줄\n둘째 줄", "줄 처음에서 ⌘⌫ 는 지울 게 없다");
    }

    #[test]
    fn caret_label_marks_the_picked_side_of_the_caret() {
        let (t, mut c) = ("방 이름".to_string(), 4);
        select_all("test:label", &t, &mut c);
        let label = caret_label("test:label", &t, c, "");
        assert_eq!(unmark(&label), ("방 이름\u{258c}".to_string(), Some((0, 4))));
        let mut c = 0;
        with_state("test:label", |s| s.anchor = Some(2));
        let label = caret_label("test:label", &t, c, "ㄱ");
        assert_eq!(unmark(&label), ("ㄱ\u{258c}방 이름".to_string(), Some((2, 4))), "캐럿 뒤를 고르면 캐럿 다음부터");
        c = 4;
        forget("test:label");
        assert_eq!(unmark(&caret_label("test:label", &t, c, "")), ("방 이름\u{258c}".to_string(), None));
    }

    #[test]
    fn focus_state_belongs_to_one_field() {
        let (mut t, mut c) = ("abc".to_string(), 3);
        select_all("test:a", &t, &mut c);
        assert_eq!(selection("test:a", c, 3), Some((0, 3)));
        assert_eq!(selection("test:b", c, 3), None);
        insert_in(line("test:a"), &mut t, &mut c, "Z");
        assert_eq!((t.as_str(), c), ("Z", 1));
        assert_eq!(selection("test:a", c, 1), None);
        forget("test:a");
    }
}
