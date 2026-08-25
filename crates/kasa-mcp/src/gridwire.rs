//! 셀 그리드를 브라우저로 보내기 위한 조밀 인코딩.
//!
//! 셀 하나를 JSON object 로 펴면 196×50 화면 한 장이 1MB 에 육박한다. 터미널 행은
//! 같은 속성이 길게 이어지므로 속성이 바뀌는 자리에서만 끊어 런으로 묶는다 —
//! 프롬프트 한 줄이 보통 런 서너 개다.
//!
//! 이 인코딩이 있는 이유는 **웹이 VT 파서를 들지 않게 하려는 것**이다. PTY 쪽이
//! 이미 파싱해 둔 그리드를 ANSI 로 되돌려 보내면 받는 쪽이 그걸 또 파싱해야 하고,
//! 그 파서(xterm.js)가 자기 방식으로 키 입력까지 가로채 모바일 IME 를 깨뜨렸다.

use kasa_bridge::screen::{Cell, Color, ScreenUpdate};
use serde_json::{json, Value};
use unicode_width::UnicodeWidthChar;

/// `null` = 터미널 기본색(테마가 정한다) · 숫자 = 256 팔레트 · `[r,g,b]` = 트루컬러.
fn color(c: &Color) -> Value {
    match c {
        Color::Default => Value::Null,
        Color::Idx(i) => json!(i),
        Color::Rgb(r, g, b) => json!([r, g, b]),
    }
}

const BOLD: u8 = 1;
const ITALIC: u8 = 2;
const UNDERLINE: u8 = 4;
const INVERSE: u8 = 8;
const DIM: u8 = 16;

fn flags(c: &Cell) -> u8 {
    (if c.bold { BOLD } else { 0 })
        | (if c.italic { ITALIC } else { 0 })
        | (if c.underline { UNDERLINE } else { 0 })
        | (if c.inverse { INVERSE } else { 0 })
        | (if c.dim { DIM } else { 0 })
}

/// 런 하나의 시각적 정체성. 이게 같으면 같은 `<span>` 에 이어 붙인다.
fn style_of(c: &Cell) -> (Value, Value, u8) {
    (color(&c.fg), color(&c.bg), flags(c))
}

/// 한 행 → `[[텍스트, fg, bg, flags], ...]`.
///
/// ⚠️ wide 글자(한글)의 두 번째 칸을 빼야 한다 — 안 빼면 받는 쪽에서 한글 뒤에 빈칸이
/// 하나씩 끼어 「가 나 다」처럼 벌어진다. 브라우저는 monospace 에서 한글을 알아서 두 칸
/// 폭으로 그리므로 글자만 보내면 자리가 맞는다.
///
/// ⚠️⚠️ **그 칸을 `'\0'` 로 알아볼 수 없다.** `Cell::ch` 주석은 `'\0'` 을 sentinel 이라
/// 하지만 `snapshot()` 을 거친 그리드에는 이미 **공백으로 바뀌어** 도착한다(2026-08-25
/// 실측: `echo 가나다` → `'가 나 다'`). 문자로는 진짜 공백과 구분이 안 되므로 **앞 글자의
/// 폭**으로 판정해야 한다 — 폭 2짜리 뒤의 한 칸이 그 글자의 자리다.
fn encode_row(row: &[Cell]) -> Value {
    let mut runs: Vec<Value> = Vec::new();
    let mut text = String::new();
    let mut style: Option<(Value, Value, u8)> = None;

    let mut push = |text: &mut String, style: &mut Option<(Value, Value, u8)>| {
        if let Some((fg, bg, fl)) = style.take() {
            if !text.is_empty() {
                runs.push(json!([std::mem::take(text), fg, bg, fl]));
            } else {
                text.clear();
            }
        }
    };

    let mut spacer = false;
    for cell in row {
        if spacer {
            spacer = false;
            continue; // 앞 글자가 두 칸을 먹었다 — 이 칸은 그 글자의 자리다
        }
        if cell.ch == '\0' {
            continue; // 아직 sentinel 인 채로 오는 경로가 있으면 여기서 걸린다
        }
        if cell.ch.width().unwrap_or(1) == 2 {
            spacer = true;
        }
        let st = style_of(cell);
        if style.as_ref() != Some(&st) {
            push(&mut text, &mut style);
            style = Some(st);
        }
        // SGR 8(conceal)은 자리는 차지하되 글리프를 감춘다.
        text.push(if cell.hidden { ' ' } else { cell.ch });
    }
    push(&mut text, &mut style);

    // 행 끝의 빈 칸은 굳이 실어 보내지 않는다 — 대부분의 행이 그 상태다. 꼬리 공백은
    // 앞 글자와 **같은 런에 묶여 있으므로**(속성이 같다) 런 통째로 비교하면 안 걸린다.
    // ⚠️ 배경색이 칠해진 공백은 눈에 보이는 것이라 자르지 않는다 — 상태바·선택 영역.
    loop {
        let Some(Value::Array(a)) = runs.last() else { break };
        if !(a[1].is_null() && a[2].is_null() && a[3] == json!(0)) {
            break;
        }
        let trimmed = a[0].as_str().unwrap_or("").trim_end().to_string();
        if trimmed.is_empty() {
            runs.pop();
            continue;
        }
        let i = runs.len() - 1;
        runs[i] = json!([trimmed, Value::Null, Value::Null, 0]);
        break;
    }
    Value::Array(runs)
}

/// `ScreenUpdate` → 브라우저가 그대로 그릴 수 있는 프레임.
///
/// `dirty` 는 바뀐 행만 담긴다(크기가 바뀌면 전체). 받는 쪽은 그 행만 교체하면 된다.
pub fn encode(u: &ScreenUpdate) -> Value {
    json!({
        "t": "grid",
        "rows": u.rows,
        "cols": u.cols,
        "dirty": u.dirty.iter().map(|(i, row)| json!([i, encode_row(row)])).collect::<Vec<_>>(),
        "cursor": [u.cursor_row, u.cursor_col],
        "cursorVisible": u.cursor_visible,
        "alt": u.alt_screen,
        // 입력을 정확히 만들려면 받는 쪽이 이 모드들을 알아야 한다. xterm.js 는 이걸
        // 자기가 추적했지만, 여기서는 파싱한 쪽이 알려주므로 어긋날 여지가 없다.
        "appCursor": u.app_cursor,
        "mouse": u.mouse_enabled,
        "mouseSgr": u.mouse_sgr,
        // 앱이 요청했을 때만 붙여넣기를 감싼다 — 안 켠 앱은 그 바이트를 글자로 받는다
        // (`claude auth login` 의 코드 프롬프트가 "Invalid code" 로 튕긴 적이 있다).
        "bracketedPaste": u.bracketed_paste,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cell(ch: char) -> Cell {
        Cell { ch, ..Cell::blank() }
    }

    #[test]
    fn 한글_스페이서는_빠지고_글자만_남는다() {
        // "가나" = 각 글자마다 [글자, '\0'] 두 칸.
        let row = vec![cell('가'), cell('\0'), cell('나'), cell('\0')];
        let v = encode_row(&row);
        assert_eq!(v[0][0], json!("가나"), "스페이서를 공백으로 흘리면 자간이 벌어진다");
    }

    #[test]
    fn 공백으로_바뀐_스페이서도_빠진다() {
        // snapshot() 을 거친 그리드는 wide 글자 뒤 칸이 `'\0'` 이 아니라 공백이다.
        // 이걸 못 거르면 「가 나 다」로 벌어진다(2026-08-25 실측 회귀).
        let row = vec![cell('가'), cell(' '), cell('나'), cell(' '), cell('다'), cell(' ')];
        let v = encode_row(&row);
        assert_eq!(v[0][0], json!("가나다"));
    }

    #[test]
    fn 한글_뒤의_진짜_공백은_살아남는다() {
        // 폭 판정이 과하면 이번엔 낱말이 붙어버린다 — 한 칸만 건너뛰어야 한다.
        let row = vec![cell('가'), cell(' '), cell(' '), cell('A')];
        let v = encode_row(&row);
        assert_eq!(v[0][0], json!("가 A"));
    }

    #[test]
    fn 같은_속성은_한_런으로_묶인다() {
        let row: Vec<Cell> = "abc".chars().map(cell).collect();
        let v = encode_row(&row);
        assert_eq!(v.as_array().unwrap().len(), 1);
        assert_eq!(v[0][0], json!("abc"));
    }

    #[test]
    fn 속성이_바뀌면_런이_갈린다() {
        let mut red = cell('b');
        red.fg = Color::Idx(1);
        let row = vec![cell('a'), red, cell('c')];
        let v = encode_row(&row);
        assert_eq!(v.as_array().unwrap().len(), 3);
        assert_eq!(v[1][1], json!(1));
    }

    #[test]
    fn 행_끝_공백은_실리지_않는다() {
        let mut row: Vec<Cell> = "hi".chars().map(cell).collect();
        row.extend(std::iter::repeat(cell(' ')).take(180));
        let v = encode_row(&row);
        assert_eq!(v.as_array().unwrap().len(), 1, "빈 꼬리까지 보내면 프레임이 커진다");
        assert_eq!(v[0][0], json!("hi"));
    }
}
