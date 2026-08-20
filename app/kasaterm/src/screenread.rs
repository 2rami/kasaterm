//! claude/codex 화면 그리드 **판독·재작성** 자유함수 — render.rs 에서 분리(2026-08-15).
//! 스피너·입력박스·배너·앵커·픽커 감지와 팀메시지(tell/SendMessage) 색칠처럼,
//! composed 그리드를 읽거나 고쳐 쓸 뿐 App 상태를 안 만지는 것들이다.
//! 외부 호출 경로는 render.rs 의 glob 재수출로 종전 `render::…` 그대로다.
use super::*;
use crate::sprites::*;

// ── Clawd 시작 배너 → 학생 도트 교체 헬퍼 ──────────────────────────────
// Claude Code 웰컴 박스의 Clawd 아트(블록문자 3행)를 감지해, 그 자리에
// 이 pane에 배정된 학생의 idle 도트(arona-ui walk 스프라이트 frame-00)를
// 그리기 위한 자유함수들. 감지는 캐릭터 배정 pane에 한정된다.

/// Clawd 아트가 차지하는 셀 박스 크기 (cols × rows).
/// diff 의 삭제 줄 수를 적는 빨강. git 칼럼과 커밋 모달이 한 화면에 같이 뜨는데
/// 둘이 각자 색을 들고 있으면 같은 뜻에 두 가지 빨강이 난다. MCP 탭의 지우기 확인도
/// 같은 이유로 이걸 쓴다 — "없애는 것"이라는 뜻이 같다.
pub(crate) const DIFF_RED: [u8; 4] = [229, 83, 75, 255];

pub(crate) const CLAWD_COLS: usize = 9;
pub(crate) const CLAWD_ROWS: usize = 3;
/// agy(Antigravity CLI) 시작 로고 — Clawd 와 아트도 크기도 달라 따로 잰다(실측 5행 12칸).
pub(crate) const AGY_COLS: usize = 12;
pub(crate) const AGY_ROWS: usize = 5;
/// 로고 옆 제품명 — 도트만 학생으로 바뀌면 남의 이름표를 달고 선 꼴이라 함께 바꾼다.
pub(crate) const CLAWD_TITLE: &[char] = &['C', 'l', 'a', 'u', 'd', 'e', ' ', 'C', 'o', 'd', 'e'];
pub(crate) const AGY_TITLE: &[char] =
    &['A', 'n', 't', 'i', 'g', 'r', 'a', 'v', 'i', 't', 'y', ' ', 'C', 'L', 'I'];
/// codex 는 마스코트 아트가 없다 — 시작 패널 한 장뿐이라 도트는 못 세우고 이름표만
/// 바꾼다. `>_` 까지 묶어 잡아야 대화 본문에 같은 낱말이 나와도 안 걸린다.
pub(crate) const CODEX_TITLE: &[char] =
    &['>', '_', ' ', 'O', 'p', 'e', 'n', 'A', 'I', ' ', 'C', 'o', 'd', 'e', 'x'];

/// 학생 도트 애니메이션 — idle(배너)·walk(로딩바) 모션별 프레임 수·주기.
pub(crate) const STUDENT_IDLE_FRAMES: usize = 4;
pub(crate) const STUDENT_ANIM_FRAME_MS: u64 = 200;
pub(crate) const STUDENT_WALK_FRAMES: usize = 6;
pub(crate) const STUDENT_WALK_FRAME_MS: f32 = 140.0;
/// 입력박스 위 스페이서 행에 서 있는 학생(전신 idle)의 키(행). 발은 입력박스
/// 윗 테두리에 닿고 위는 스크롤백 꼬리라 몇 행 덮여도 무해 — 배너와 같은 키.
pub(crate) const INPUT_STANDING_ROWS: usize = 3;

/// 직전 프레임에 학생 도트 배너가 화면에 있었는지. 배너 애니 타이머
/// 스레드(handler.rs)가 이걸 보고 배너가 보일 때만 redraw를 깨운다 —
/// 배너가 없으면 sleep 루프만 돌아 idle 비용이 0에 수렴한다.
pub(crate) static STUDENT_SPRITE_ANIMATING: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// ultracode 애니(혜성·숨쉬기) 프레임 주기. 도트 배너(200ms)와 달리 혜성은
/// 픽셀 이동이라 그 주기론 프레임당 8셀씩 순간이동으로 보인다 — 66ms(~15fps)면
/// 프레임당 ~2.8셀로 흐르는 빛으로 읽힌다.
pub(crate) const ULTRA_COMET_FRAME_MS: u64 = 66;

/// ultracode pane 이 하나라도 있는 동안 true. 혜성 타이머 스레드(handler.rs)가
/// 이걸 보고 redraw 를 깨운다 — 갱신은 `refresh_pane_ultracode`(input.rs, 마커
/// 스캔과 같은 손)에서. 없으면 sleep 루프만 돌아 idle 비용 0.
pub(crate) static ULTRA_COMET_ANIMATING: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);




/// 에이전트 TUI 의 입력 영역. 하네스마다 **모양이 다르다** — claude 는 `─` 보더
/// 두 줄이 입력행을 감싸고, codex 는 보더가 없는 대신 입력행 전체가 배경색으로
/// 칠해져 있다(실측 `bg=Rgb(63,69,77)`, 주변은 `Default`).
///
/// 판정을 한 곳에 모으는 이유는 `strip_agent_chip` 주석(아래)에 적힌 사고 그대로다:
/// 같은 일을 하는 사본이 각자 관문을 들면 한쪽만 고쳐져 조용히 어긋난다.
pub(crate) enum PromptBox {
    /// claude — `rows` 가 입력행, 그 바깥 `top`/`bottom` 이 대시 보더.
    Bordered { rows: std::ops::Range<usize>, top: usize, bottom: usize },
    /// codex — 보더가 없다. 칠할 것도 칩을 지울 자리도 입력행 자신뿐이다.
    Filled { rows: std::ops::Range<usize> },
}

impl PromptBox {
    pub(crate) fn rows(&self) -> std::ops::Range<usize> {
        match self {
            PromptBox::Bordered { rows, .. } | PromptBox::Filled { rows } => rows.clone(),
        }
    }
}

/// ultracode 턴의 입력박스 accent — 보라 숨쉬기. 학생 배정과 무관하게 모든
/// ultracode pane 이 이걸 쓴다(2026-08-16 「보라색 숨쉬기로」 — 한때는 학생색
/// 보더 위를 혜성이 돌았다). 누구 pane 인지는 pane 테두리 학생색이 계속 말한다.
///
/// 상태줄 배지(`ultra`)는 세그먼트 **맨 끝**이라 좁은 pane 에서 제일 먼저 잘리고,
/// 안 잘려도 눈이 잘 안 간다 — 여러 에이전트를 푸는 턴인지는 타이핑하는 자리에서
/// 보여야 한다(거노 2026-08-11: "상태줄말고 프롬프트입력창 보라색 glow").
///
/// **입력박스를 따로 칠하지 않고 accent 만 갈아끼우는 이유**: 그 박스를 칠하는 손은
/// `style_prompt_box` 하나뿐이고 pane 렌더 **뒤쪽**에서 돈다. 앞에서 셀을 직접
/// 물들이면 그 호출이 학생 accent 로 깨끗이 덮어쓴다(2026-08-11 실측 — 스샷의
/// 테두리가 보라가 아니라 학생 분홍 `d55580` 이었다).
///
/// 색은 `bb9af7`. `t`(초)에 따라 밝은 쪽으로 숨쉬듯 오르내려 정적인 테두리와
/// 구분된다 — 스피너 shimmer 와 같은 mix 방식이다.
pub(crate) fn ultracode_accent(t: f32) -> [u8; 4] {
    let g = 0.34 * (0.5 + 0.5 * (t * 2.2).sin());
    let base = [0xbbu8, 0x9a, 0xf7];
    let mut out = [255u8; 4];
    for i in 0..3 {
        let b = base[i] as f32;
        out[i] = (b + (255.0 - b) * g).round() as u8;
    }
    out
}

/// ultracode 숨쉬기 색 한 프레임 — 학생색이 있으면 **학생색↔보라 순환**(골=학생색
/// 그대로, 마루=보라. 2026-08-17 「학생색 유지되면서 순환하는 형식으로」), 미배정
/// pane 은 보라 밝기 숨쉬기(`ultracode_accent`)다. 입력박스 보더·「ultracode」
/// 라벨·/rename 세션명 테두리(2026-08-17 「리네임되는 부분 테두리도 같이」)가
/// 전부 이 하나를 불러 같은 sin 위상으로 함께 숨쉰다.
pub(crate) fn ultracode_breath(accent: Option<[u8; 4]>, t: f32) -> [u8; 4] {
    match accent {
        Some(a) => {
            let purple = [0xbbu8, 0x9a, 0xf7];
            let k = 0.5 + 0.5 * (t * 2.2).sin();
            let mix = |s: u8, p: u8| (s as f32 + (p as f32 - s as f32) * k).round() as u8;
            [mix(a[0], purple[0]), mix(a[1], purple[1]), mix(a[2], purple[2]), 255]
        }
        None => ultracode_accent(t),
    }
}

/// ultracode 입력박스의 「ultracode」 라벨 — 위보더 왼쪽 대시를 글자로 바꾼다.
/// **항상 떠 있다**: 혜성 시절엔 꼬리가 지나갈 때만, 보라 숨쉬기 시절엔 밝은
/// 반주기에만 드러났는데, 깜빡임이 거슬려 상시로 바꿨다(2026-08-17 「울트라코드
/// 텍스트 숨쉬기할때 그냥 안없어지게」). 색은 뒤 페인트가 입히므로 보더와 함께
/// 숨쉰다 — 글자 자체는 사라지지 않는다.
///
/// `style_prompt_box` **앞에** 부를 것 — 글자를 먼저 심어야 뒤 페인트가 보더와
/// 같은 색을 입혀 준다. 대시(─)인 칸만 바꾼다 — @칩·세션 제목 같은 실물 글자를
/// 지우면 안 된다.
pub(crate) fn overlay_ultracode_label(rows: &mut [Vec<GridCell>]) {
    let Some(PromptBox::Bordered { top, .. }) = prompt_box(rows) else { return };
    const LABEL: &[u8] = b"ultracode";
    const LABEL_AT: usize = 2;
    let w_top = rows[top].len();
    if w_top <= LABEL_AT + LABEL.len() + 2 {
        return;
    }
    for (i, &ch) in LABEL.iter().enumerate() {
        let cell = &mut rows[top][LABEL_AT + i];
        if cell.ch == '─' {
            cell.ch = ch as char;
        }
    }
}

/// 에이전트 TUI 입력 영역 탐지 — 화면 하단에서 위로 찾는다.
///
/// **claude**: `─` 보더 두 줄 사이. 그 사이에 `❯` 마커 행이 있어야 인정한다
/// (권한 메뉴 등 다른 풀폭 박스 오인 방지).
///
/// **codex**: 보더가 없다. 대신 입력행이 **명시 배경색으로 통째로 칠해져** 있어서
/// 그걸 시그니처로 쓴다 — `›` 로 시작하고 그 행의 모든 글리프가 같은 non-Default
/// `bg` 를 공유하는 행. 배경 없이 `›` 만 보면 인용문·diff 를 입력창으로 오인한다.
pub(crate) fn prompt_box(rows: &[Vec<GridCell>]) -> Option<PromptBox> {
    fn is_border(r: &[GridCell]) -> bool {
        let (mut dash, mut glyph) = (0usize, 0usize);
        for c in r {
            if c.ch == '\0' || c.ch == ' ' {
                continue;
            }
            glyph += 1;
            if c.ch == '─' {
                dash += 1;
            }
        }
        dash >= 10 && dash * 2 >= glyph
    }
    // claude 입력박스 마커는 `❯`(U+276F, 또는 옛 `›`)뿐 — ASCII `>` 는 제외한다.
    // diff·git·노트 TUI 는 대시줄 사이에 ASCII `>`(인용·프롬프트) 를 흔히 둬서,
    // `>` 까지 마커로 치면 그 대시줄 쌍을 입력박스로 오인해 뜬금없는 빈 초록
    // 사각형을 덧그렸다(거노 2026-07-22).
    fn marker_row(r: &[GridCell]) -> bool {
        r.iter().find(|c| c.ch != ' ' && c.ch != '\0').is_some_and(|c| matches!(c.ch, '❯' | '›'))
    }
    if let Some(b2) = rows.iter().rposition(|r| is_border(r)) {
        if let Some(b1) = rows[..b2].iter().rposition(|r| is_border(r)) {
            let range = (b1 + 1)..b2;
            if !range.is_empty() && rows[range.clone()].iter().any(|r| marker_row(r)) {
                return Some(PromptBox::Bordered { rows: range, top: b1, bottom: b2 });
            }
        }
    }
    // codex — 칠해진 입력행. 행 전체가 같은 non-Default bg 를 쓰는 것이 시그니처고,
    // 마커(`›`)를 함께 요구해 배경만 남은 여백 행과 구별한다.
    let uniform_fill = |r: &[GridCell]| -> Option<kasa_bridge::screen::Color> {
        let mut fill: Option<kasa_bridge::screen::Color> = None;
        let mut glyphs = 0usize;
        for c in r.iter().filter(|c| c.ch != '\0') {
            if matches!(c.bg, kasa_bridge::screen::Color::Default) {
                return None;
            }
            if fill.is_some_and(|f| f != c.bg) {
                return None;
            }
            fill = Some(c.bg.clone());
            glyphs += 1;
        }
        (glyphs >= 8).then_some(fill?)
    };
    let f = rows
        .iter()
        .rposition(|r| marker_row(r) && uniform_fill(r).is_some())?;
    let fill = uniform_fill(&rows[f])?;
    // 입력창은 **여러 줄이다** — 마커 행 위아래로 같은 채움색 여백 행이 붙고,
    // 여러 줄을 입력하면 그만큼 자란다(실측 0.146.0: 여백-입력-여백 3줄). 마커
    // 행만 칠하면 가운데 한 줄만 색이 바뀌어 상자가 아니라 밑줄로 보인다(거노).
    let same = |r: &[GridCell]| uniform_fill(r).is_some_and(|c| c == fill);
    let mut start = f;
    while start > 0 && same(&rows[start - 1]) {
        start -= 1;
    }
    let mut end = f + 1;
    while end < rows.len() && same(&rows[end]) {
        end += 1;
    }
    Some(PromptBox::Filled { rows: start..end })
}

/// 학생 pane 입력박스의 양끝 보더 행(─ 줄 + @배지)을 claude 가 /color·
/// --agent-color 로 그린 명시색을 **무시하고** 학생 accent 로 강제 도색한다 —
/// pane 정체성 색과 항상 일치. (본문 틴트가 있던 시절엔 사이 행의 입력 글자를
/// 틴트에서 빼는 처리도 여기 있었는데, 본문이 테마 기본 fg 로 돌아가며 폐기.)
pub(crate) fn style_prompt_box(rows: &mut [Vec<GridCell>], accent: [u8; 4]) {
    let Some(bx) = prompt_box(rows) else { return };
    let fg = kasa_bridge::screen::Color::Rgb(accent[0], accent[1], accent[2]);
    match &bx {
        PromptBox::Bordered { top, bottom, .. } => {
            for i in [*top, *bottom] {
                for c in rows[i].iter_mut() {
                    // 세션명/테두리 줄 배경(claude --agent-color 로 채운 accent 밴드)을
                    // 터미널색으로 되돌린다 — 아웃라인(─ 대시·세션명 글자)만 accent 로
                    // 두고 배경은 안 칠한다(거노: 배경까지 채우면 글자가 묻힌다).
                    c.bg = kasa_bridge::screen::Color::Default;
                    if c.ch != ' ' && c.ch != '\0' {
                        c.fg = fg.clone();
                    }
                }
            }
        }
        // codex 는 칠할 보더가 없다. 이미 배경으로 칠해진 그 줄을 학생색 쪽으로
        // 끌어당긴다 — 거노 선택(2026-08-05). 원래 배경을 버리지 않고 섞는 이유는
        // 입력 글자가 묻히지 않게 하기 위해서다(보더 도색이 배경을 비우는 것과
        // 같은 이유). 여기서 fg 는 건드리지 않는다.
        PromptBox::Filled { rows: r } => {
            for i in r.clone() {
                for c in rows[i].iter_mut() {
                    if let kasa_bridge::screen::Color::Rgb(br, bg_, bb) = c.bg {
                        c.bg = tint_toward([br, bg_, bb], accent, PROMPT_TINT);
                    }
                }
            }
        }
    }
    // 입력행 왼쪽 ❯ 프롬프트 마커도 학생 accent 로 — claude --agent-color(8색
    // 근사)가 남으면 보더와 화살표 색이 어긋난다(거노). 마커 글리프 한 칸만
    // 칠하고 입력 글자는 테마 기본 fg 유지.
    for r in bx.rows() {
        if let Some(c) = rows[r]
            .iter_mut()
            .find(|c| c.ch != ' ' && c.ch != '\0')
            .filter(|c| matches!(c.ch, '❯' | '›' | '>'))
        {
            c.fg = fg.clone();
        }
    }
}

/// codex 입력줄 배경을 학생색 쪽으로 끌어당기는 비율. 글자가 묻히지 않을 만큼만.
pub(crate) const PROMPT_TINT: f32 = 0.22;

/// `base` 를 `accent` 쪽으로 `amount` 만큼 섞는다. 셀 배경은 알파가 없어
/// (`Color::Rgb` 뿐) 미리 합성해야 한다 — `theme::with_alpha` 를 못 쓰는 이유.
pub(crate) fn tint_toward(base: [u8; 3], accent: [u8; 4], amount: f32) -> kasa_bridge::screen::Color {
    let mix = |b: u8, a: u8| (b as f32 + (a as f32 - b as f32) * amount).round().clamp(0.0, 255.0) as u8;
    kasa_bridge::screen::Color::Rgb(
        mix(base[0], accent[0]),
        mix(base[1], accent[1]),
        mix(base[2], accent[2]),
    )
}

/// claude v2.1.228 이 **펼쳐서** 그리는 팀메시지 헤더 — `@ <발신 라벨>❯`. 본문은
/// 다음 행부터 2칸 들여쓰기로 이미 화면에 있다(접힌 형태와 달리 전개가 필요 없다).
/// 반환은 (첫 글리프 col, `❯` col, 라벨). '@' 로 시작하는 아무 행이나 잡으면 사용자가
/// 직접 친 텍스트를 덮으므로, 호출부가 transcript 태그의 from_label 과 대조해
/// 일치할 때만 쓴다.
pub(crate) fn peer_native_header_line(row: &[GridCell]) -> Option<(usize, usize, String)> {
    let first = row.iter().position(|c| !matches!(c.ch, ' ' | '\0'))?;
    if row[first].ch != '@' || row.get(first + 1).map(|c| c.ch) != Some(' ') {
        return None;
    }
    let qcol = (first + 2..row.len()).find(|&i| row[i].ch == '❯')?;
    let label: String = row[first + 2..qcol]
        .iter()
        .filter(|c| c.ch != '\0')
        .map(|c| c.ch)
        .collect();
    let label = label.trim().to_string();
    if label.is_empty()
        || row[qcol + 1..].iter().any(|c| !matches!(c.ch, ' ' | '\0'))
    {
        return None;
    }
    Some((first, qcol, label))
}

/// verbose OFF 에서 접힌 팀메시지 행 탐지 — 단수 "› Message from @<이름>" 또는
/// 복수 "› <N> messages from @<이름>". 반환은 (첫 글리프 col, 메시지 수, 보낸이
/// agent 이름). 이름 뒤에 다른 글자가 있으면(본문 안 인용 등) 접힌 줄이 아니라고
/// 본다 — 오탐이 실제 출력 텍스트를 덮어쓰면 안 된다.
pub(crate) fn teammate_collapsed_line(row: &[GridCell]) -> Option<(usize, usize, String)> {
    let chars: Vec<char> = row
        .iter()
        .map(|c| if c.ch == '\0' { ' ' } else { c.ch })
        .collect();
    let first = chars.iter().position(|&c| c != ' ')?;
    if !matches!(chars[first], '›' | '>') {
        return None;
    }
    // '›' 뒤: 단수 " Message from @<이름>" 또는 복수 " <N> messages from @<이름>".
    let rest: String = chars[first + 1..].iter().collect();
    let (count, after) = if let Some(a) = rest.strip_prefix(" Message from @") {
        (1usize, a.to_string())
    } else {
        let a = rest.strip_prefix(' ')?;
        let digits = a.chars().take_while(|c| c.is_ascii_digit()).count();
        if digits == 0 {
            return None;
        }
        let n: usize = a[..digits].parse().ok()?;
        let a2 = a[digits..].strip_prefix(" messages from @")?;
        (n, a2.to_string())
    };
    let name: String = after
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    if name.is_empty() {
        return None;
    }
    // 이름 뒤는 공백, 또는 "(ctrl+o to expand)" 꼴 단축키 힌트 하나만 허용 —
    // claude v2.1.216+ 가 접힌 줄 끝에 힌트를 붙인다(chord 는 키바인딩 따라
    // 변주라 "to expand)" 종결로 판정). 그 외 텍스트는 본문 인용 오탐 방지.
    let tail = after[name.len()..].trim_matches(' ');
    if !tail.is_empty() && !(tail.starts_with('(') && tail.ends_with("to expand)")) {
        return None;
    }
    Some((first, count, name))
}

/// tell 주입 마커 `⟦캐릭터⟧ 본문` 감지 — kasaterm-cli tell 이 발신 pane 캐릭터를
/// 앞에 심는다(SendMessage 는 팀 경계 안이라 크로스-방 tell 만 화면에 발신자 앵커가
/// 필요). `character_accent` 유효 캐릭터만 인정해 거노가 우연히 친 `⟦…⟧` 오탐을
/// 막는다. 반환: (⟦ 시작 col, ⟧ 다음 col, 캐릭터명).
pub(crate) fn tell_marker_line(row: &[GridCell]) -> Option<(usize, usize, String)> {
    let chars: Vec<char> = row
        .iter()
        .map(|c| if c.ch == '\0' { ' ' } else { c.ch })
        .collect();
    let mut first = chars.iter().position(|&c| c != ' ')?;
    // claude TUI 는 제출된 user 턴을 `❯ ` 프롬프트 마커로 시작해 그린다 — 마커는
    // 그 뒤에 온다.
    if chars[first] == '❯' {
        first = chars[first + 1..]
            .iter()
            .position(|&c| c != ' ')
            .map(|i| first + 1 + i)?;
    }
    if chars[first] != '⟦' {
        return None;
    }
    let close_rel = chars[first + 1..].iter().position(|&c| c == '⟧')?;
    // wide 글자는 그리드에서 ch + blank 2셀이라, 이름 구간의 padding blank 를 뺀다.
    let name: String = chars[first + 1..first + 1 + close_rel]
        .iter()
        .filter(|&&c| c != ' ' && c != '\0')
        .collect();
    theme::character_accent(&name)?;
    Some((first, first + 1 + close_rel + 1, name))
}

/// tell·SendMessage 프사 셀 폭 — claude 가 접는 본문 들여쓰기(2칸)와 같아야
/// 본문 좌측선을 밀지 않는다. 셀이 세로 2:1 이라 2칸×1행이 곧 정사각 bust.
pub(crate) const TELL_FACE_COLS: usize = 2;

/// tell 마커 행을 발신 학생색으로 — 그 행 전체를 accent fg 로 칠해 SendMessage
/// 인라인과 시각을 맞춘다. 프사가 있는 캐릭터는 첫 줄 본문을 마커 시작 col 로
/// 당겨(= claude 의 `❯ ` 폭 2 = wrap 들여쓰기) 접힌 줄과 좌측선을 맞추고, 비워진
/// `❯` 자리 2칸에 아바타를 얹는다(호출측 이미지 패스) — 옛 배치는 첫 줄만 프사
/// 폭만큼 밀려 계단이 졌다(거노 2026-07-27). 위 행 헤더로 올리는 안은 claude 가
/// user 턴 앞에 빈 줄을 두지 않아 윗줄 글자를 덮어 기각(실측). slug 없는
/// 캐릭터만 `이름 ›` 인라인 폴백. 반환은 프사 rect 의 x 기준 col — 없으면 None.
/// `@ <발신 라벨>❯` 헤더의 라벨을 학생 이름으로 — `@ 이름❯` 만 남기고 뒤는 지운다.
/// 색은 tint_row 가 뒤에서 입히므로 여기선 글자만 놓는다.
pub(crate) fn restyle_peer_native_header(
    row: &mut [GridCell],
    c0: usize,
    qcol: usize,
    name: &str,
    accent: [u8; 4],
) {
    use unicode_width::UnicodeWidthChar;
    let fg = kasa_bridge::screen::Color::Rgb(accent[0], accent[1], accent[2]);
    let style = row[c0].clone();
    let text = format!("@ {name}❯");
    // 헤더 행은 `❯` 뒤가 전부 빈칸이다(탐지기가 보장) — 원 라벨이 이름보다 짧으면
    // (`@ 12889❯` 등 pid 라벨) 필요한 만큼 오른쪽으로 늘려 쓴다. 라벨 폭에 가두면
    // 이름이 잘리고 ❯ 가 사라진다(2026-08-12 실측: `@ kanna` 로 잘림).
    let need: usize = text.chars().map(|c| c.width().unwrap_or(1).max(1)).sum();
    let end = (qcol + 1).max(c0 + need).min(row.len());
    for c in row[c0..end].iter_mut() {
        let mut b = style.clone();
        b.ch = ' ';
        *c = b;
    }
    let mut w = c0;
    for ch in text.chars() {
        let cw = ch.width().unwrap_or(1).max(1);
        if w + cw > end {
            break;
        }
        // bold — 본문도 같은 학생색이라 이름이 색만으로는 안 튄다(2026-08-12
        // 지시 「이름은 bold로」). 접힌 경로(expand_teammate_message)의 헤더와 동일.
        let mut cell = style.clone();
        cell.ch = ch;
        cell.fg = fg.clone();
        cell.bold = true;
        row[w] = cell;
        if cw == 2 && w + 1 < end {
            let mut sp = style.clone();
            sp.ch = ' ';
            sp.fg = fg.clone();
            sp.bold = true;
            row[w + 1] = sp;
        }
        w += cw;
    }
}

pub(crate) fn restyle_tell_line(
    row: &mut [GridCell],
    marker_start: usize,
    marker_end: usize,
    name: &str,
    accent: [u8; 4],
) -> Option<usize> {
    use unicode_width::UnicodeWidthChar;
    let fg = kasa_bridge::screen::Color::Rgb(accent[0], accent[1], accent[2]);
    let end = marker_end.min(row.len());
    if theme::character_slug(name).is_some() {
        // 마커 뒤 본문을 마커 시작 col 로 당긴다(claude 의 `❯ ` 폭 = 2 = wrap
        // 들여쓰기라 첫 줄과 연속 줄이 같은 col 에 선다).
        let body_start = row[end..]
            .iter()
            .position(|c| c.ch != ' ' && c.ch != '\0')
            .map(|i| end + i)
            .unwrap_or(end);
        let shift = body_start.saturating_sub(marker_start);
        if shift > 0 {
            row[marker_start..].rotate_left(shift);
            let n = row.len();
            for c in row[n - shift..].iter_mut() {
                *c = GridCell::blank();
            }
        }
        // 비운 앞칸은 본문 셀의 배경(claude 가 깐 user 턴 하이라이트)을 물려받는다
        // — blank 로 두면 첫 줄만 배경이 늦게 시작해 wrap 연속 행과 계단이 진다.
        let pad = {
            let mut c = row[marker_start.min(row.len() - 1)].clone();
            c.ch = ' ';
            c
        };
        for c in row[..marker_start].iter_mut() {
            *c = pad.clone();
        }
        tint_row(row, accent);
        // 프사는 `❯` 가 있던 왼쪽 여백에 본문과 같은 행으로 — 위 행은 claude 가
        // 이전 블록(`✻ Worked for 5s` 등)으로 채워 두는 게 보통이라 헤더로 올리면
        // 그 글자를 덮는다(실측 2026-07-27). 여백이 프사 폭에 못 미치면(마커가
        // 행 머리) 프사를 포기하고 색만 입힌다.
        return (marker_start >= TELL_FACE_COLS).then_some(0);
    }
    let lead = row[..end]
        .iter()
        .position(|c| c.ch != ' ' && c.ch != '\0')
        .unwrap_or(0);
    for c in row[..end].iter_mut() {
        *c = GridCell::blank();
    }
    let label = format!("{name} ›");
    // 라벨을 본문 쪽(end)에 붙인다. 지우는 마커 `⟦이름⟧ ` 폭은 이름 길이에 따라
    // 가변인데 라벨은 고정폭이라, 왼쪽 정렬하면 남는 칸이 그대로 `›`—본문 사이
    // 갭으로 보였다(거노 2026-07-27: 이름이 길수록 더 벌어짐).
    let label_w: usize = label.chars().map(|c| c.width().unwrap_or(1).max(1)).sum();
    let start = end.saturating_sub(label_w + 1).max(lead);
    let mut w = start;
    for ch in label.chars() {
        let cw = ch.width().unwrap_or(1).max(1);
        if w + cw > end {
            break;
        }
        // bold — 본문도 같은 학생색이라 이름이 색만으로는 안 튄다(2026-08-12
        // 지시 「이름은 bold로」). 네이티브 헤더·접힌 경로 헤더와 동일.
        let mut cell = GridCell::blank();
        cell.ch = ch;
        cell.fg = fg.clone();
        cell.bold = true;
        row[w] = cell;
        if cw == 2 && w + 1 < end {
            let mut sp = GridCell::blank();
            sp.fg = fg.clone();
            sp.bold = true;
            row[w + 1] = sp;
        }
        w += cw;
    }
    // 본문(마커 뒤)도 학생색으로.
    tint_row(&mut row[end..], accent);
    None
}

/// 행의 비공백 글자 fg 를 accent 로 — tell 마커 행과 그 wrap 연속 행이 공유.
pub(crate) fn tint_row(row: &mut [GridCell], accent: [u8; 4]) {
    let fg = kasa_bridge::screen::Color::Rgb(accent[0], accent[1], accent[2]);
    for c in row.iter_mut() {
        if c.ch != ' ' && c.ch != '\0' {
            c.fg = fg.clone();
        }
    }
}

/// 학생 완료 보고 줄 감지 — `[완료] 미도리(%4) — …` / `[실패] …`. socket.rs
/// `pane_done` 이 부모 pane 입력창에 주입해 제출되는 형식이다. 큐잉(>)·제출(❯)
/// 프롬프트 마커 뒤에 올 수 있고, `(%N)` 괄호까지 요구해 사용자가 우연히 친
/// `[완료]` 텍스트 오탐을 줄인다. 반환: 보고한 캐릭터명.
pub(crate) fn done_report_line(row: &[GridCell]) -> Option<String> {
    // wide 글리프 스페이서는 경로 따라 '\0'(kasa-bridge) 또는 ' '(alacritty
    // composed, 실측) — 구분할 방법이 없으니 **공백까지 전부 지운** 문자열로
    // 본다. 안 지우면 "[완료]" 가 "완 료" 로 갈라져 prefix 매칭이 깨진다.
    // 이름(캐릭터명)엔 원래 공백이 없어 잃는 것이 없다.
    let flat: String = row
        .iter()
        .map(|c| c.ch)
        .filter(|&c| c != '\0' && c != ' ')
        .collect();
    let rest = flat
        .strip_prefix('❯')
        .or_else(|| flat.strip_prefix('>'))
        .unwrap_or(&flat);
    let tail = rest
        .strip_prefix("[완료]")
        .or_else(|| rest.strip_prefix("[실패]"))?;
    let open = tail.find('(')?;
    let close = tail[open..].find(')')? + open;
    if !tail[open + 1..close].starts_with('%') {
        return None;
    }
    let name = &tail[..open];
    (!name.is_empty()).then(|| name.to_string())
}

/// tell 마커 행의 wrap 연속 행 판정 — claude TUI 는 긴 user 턴을 2칸 들여쓰기
/// 행으로 wrap 한다. 들여쓰기가 2 **이상**이고 첫 글자가 TUI 구조 글리프가
/// 아니면 같은 메시지의 연속으로 본다(⎿·⏺ 등 다음 블록에서 끊김).
///
/// ⚠️ 「정확히 2」로 가두면 안 된다 — 목록 항목("- …"·"1. …")의 wrap 은 4~5칸
/// 들여쓰기로 떨어지고, 거기서 걸음이 끊기면 **그 아래 문단 전체가 무테마**로
/// 남는다(2026-08-20 거노 스샷: 사오리 브리프가 셋째 줄부터 흰색). 걸음은
/// 헤더부터 연속 행만 따라가므로, 다음 블록(⏺ col0·입력박스 보더)에서 어차피
/// 멈춘다 — 깊은 들여쓰기를 받아도 남의 출력까지 번지지 않는다.
pub(crate) fn tell_wrap_continuation(row: &[GridCell]) -> bool {
    match row.iter().position(|c| c.ch != ' ' && c.ch != '\0') {
        Some(n) if n >= 2 => !matches!(
            row[n].ch,
            '⏺' | '✻' | '⎿' | '│' | '⎢' | '❯' | '─' | '═' | '╌' | '⏵' | '·'
        ),
        _ => false,
    }
}

/// 사용자가 친 프롬프트의 배경 띠 감지 — claude 는 지난 user 턴을 `❯ 본문` 행에
/// **행 전폭 배경 띠**로 그린다(2026-08-15 실측: 라이트 테마 rgb(240,240,240)).
/// 그 색은 claude 테마 소관이라 kasaterm 테마와 어긋난다 — 라이트에선 씻겨
/// 보이고 다크에선 흰 띠로 뜬다(「내가 친 프롬프트 텍스트 배경 흰색되는거」).
/// 색을 열거하지 않고 **구조**로 잡는다: 첫 non-blank 가 `❯`(col 0~1) 이고 그
/// 행의 배경이 끝까지 균일한 non-default 색이면 프롬프트 띠다. 입력박스의 `❯`
/// 는 배경이 없어 안 걸리고, 메뉴 선택 강조는 대개 전폭이 아니며, 픽커 화면은
/// 호출측이 게이트한다.
pub(crate) fn user_prompt_band(row: &[GridCell]) -> Option<kasa_bridge::screen::Color> {
    let first = row.iter().position(|c| !matches!(c.ch, ' ' | '\0'))?;
    if first > 1 || row[first].ch != '❯' {
        return None;
    }
    band_bg(row)
}

/// 행 전폭이 같은 non-default 배경일 때 그 색 — 프롬프트 wrap 연속 행 판정도
/// 이걸 쓴다(띠색이 같으면 같은 블록).
pub(crate) fn band_bg(row: &[GridCell]) -> Option<kasa_bridge::screen::Color> {
    let bg = row.first()?.bg.clone();
    if matches!(bg, kasa_bridge::screen::Color::Default) {
        return None;
    }
    row.iter().all(|c| c.bg == bg).then_some(bg)
}

/// 프롬프트 띠 한 행을 kasaterm 디자인으로 재도색 — 띠는 **본문 폭까지만**
/// (전폭 띠의 꼬리는 기본 배경으로 되돌린다), 바탕은 `fill`, 앞머리 `❯` 는
/// accent 원색. 글자색은 claude 가 정한 그대로 둔다.
pub(crate) fn restyle_user_prompt_row(
    row: &mut [GridCell],
    fill: &kasa_bridge::screen::Color,
    accent: [u8; 4],
) {
    let last = row
        .iter()
        .rposition(|c| !matches!(c.ch, ' ' | '\0'))
        .unwrap_or(0);
    let pad_end = (last + 2).min(row.len());
    for (i, c) in row.iter_mut().enumerate() {
        if i < pad_end {
            c.bg = fill.clone();
            if i <= 1 && c.ch == '❯' {
                c.fg = kasa_bridge::screen::Color::Rgb(accent[0], accent[1], accent[2]);
            }
        } else {
            c.bg = kasa_bridge::screen::Color::Default;
        }
    }
}

fn msg_blank_row(row: &[GridCell]) -> bool {
    row.iter().all(|c| c.ch == ' ' || c.ch == '\0')
}

/// 문단 사이 빈 행에서 팀메시지가 계속되는지 — 빈 행 뒤 첫 non-blank 행이 여전히
/// wrap 연속 행이면 같은 메시지의 문단 구분이다. 빈 행을 무조건 끝으로 보면
/// 여러 문단짜리 SendMessage 는 첫 문단만 학생색이 입혀졌다(2026-08-15 신고).
/// 메시지가 실제로 끝나면 다음 블록은 구조 글리프(⏺·❯·╭ 박스)나 col 0 행이라
/// 연속 판정에 안 걸려 여기서 멈춘다.
pub(crate) fn msg_paragraph_gap(rows: &[Vec<GridCell>], at: usize) -> bool {
    msg_blank_row(&rows[at])
        && rows[at + 1..]
            .iter()
            .find(|r| !msg_blank_row(r))
            .is_some_and(|r| tell_wrap_continuation(r))
}

/// 팀원 agent 이름("aru-9c88")의 보낸 학생 accent — 로마자 앞부분(마지막 '-'
/// 앞)을 로스터로 역매핑. 로스터 밖(team-lead 등)은 transcript 태그의 color
/// 명 → 그것도 없으면 테마 accent.
/// 팀메시지 발신자 이름 → 학생 슬러그(프사 에셋 키). `from` 이 한글 표시명
/// ("프라나")인 경우와 agent-name 꼬리표가 붙은 슬러그("midori-2535") 둘 다 받는다.
pub(crate) fn teammate_sender_slug(name: &str) -> Option<&'static str> {
    if let Some(s) = theme::character_slug(name) {
        return Some(s);
    }
    theme::slug_character(sender_roman_head(name)).and_then(theme::character_slug)
}

/// agent 이름에서 로스터가 아는 로마자 부분만. **첫 토막**이다.
///
/// 전에는 마지막 `-` 앞을 뗐는데(`aru-9c88` → `aru`), 2026-08-04 에 이름이
/// `<슬러그>-p<pane 번호>-<접미>` 세 토막이 되면서 `himari-p2-1uc` → `himari-p2` 가
/// 되어 로스터에 없는 이름으로 떨어졌다 — 그래서 남이 보낸 메시지가 학생색도 프사도
/// 없이 떴다(거노 2026-08-11: "sendmessage 학생테마 안나오는거"). 첫 토막을 보면 두
/// 형식이 다 걸린다.
pub(crate) fn sender_roman_head(name: &str) -> &str {
    name.split_once('-').map(|(a, _)| a).unwrap_or(name)
}

/// `@ <라벨>❯` 의 라벨이 로스터 학생의 agent 이름꼴(`<슬러그>-p<번호>…`)인지.
///
/// transcript 대조가 불가능한 헤더의 보조 관문 — 발신 pane 을 dismiss 하면 명부
/// 파일(`~/.claude/sessions/<pid>.json`)이 사라져 발신자 복원이 통째로 실패하고,
/// 옛 메시지는 tail(256KB) 밖으로 밀려나 대조 자체가 안 된다. 둘 다 화면에는
/// 라벨이 그대로 남아 있으니 이름꼴로 판정한다(2026-08-20 거노 스샷:
/// `@ midori-p4-v32❯` 가 무테마로 남았다). `-p<번호>` 토막까지 요구해 사용자가
/// 우연히 친 텍스트("midori-chan" 등)는 안 걸린다.
pub(crate) fn label_is_roster_agent(label: &str) -> bool {
    let Some((head, rest)) = label.split_once('-') else {
        return false;
    };
    theme::slug_character(head).is_some()
        && rest.split('-').next().is_some_and(|p| {
            p.len() > 1 && p.starts_with('p') && p[1..].chars().all(|c| c.is_ascii_digit())
        })
}

pub(crate) fn teammate_sender_accent(name: &str, tag_color: Option<&str>) -> [u8; 4] {
    // 발신자가 한글 캐릭터 표시명인 경우(F-2 인박스 규칙의 `from` = 발신 캐릭터명)
    // 를 먼저 본다 — 슬러그 경로만 타면 "프라나" 같은 이름이 매칭에 실패해 학생색
    // 대신 tag_color 폴백으로 떨어졌다(거노 2026-07-27: SendMessage 도 학생 테마).
    if let Some(c) = theme::character_accent(name) {
        return c;
    }
    if let Some(c) = theme::slug_character(sender_roman_head(name)).and_then(theme::character_accent)
    {
        return c;
    }
    match tag_color {
        Some("red") => [224, 88, 78, 255],
        Some("orange") => [228, 140, 60, 255],
        Some("yellow") => [212, 180, 60, 255],
        Some("green") => [63, 170, 90, 255],
        Some("cyan") => [70, 180, 200, 255],
        Some("blue") => [90, 140, 230, 255],
        Some("purple") => [168, 118, 228, 255],
        Some("pink") => [228, 100, 160, 255],
        _ => theme::accent(),
    }
}

/// transcript 에서 회수한 팀메시지 원문(접힌 줄 전개·말풍선용).
#[derive(Clone)]
pub(crate) struct TeammateMsg {
    pub(crate) body: String,
    pub(crate) color: Option<String>,
    /// 화면에 뜬 이름이 쓸모없을 때(`@peer`) 태그에서 되찾은 진짜 발신자.
    /// 이게 있어야 학생색·프사·본문 조회가 이름으로 걸린다.
    pub(crate) sender: Option<String>,
    /// 태그 원문의 발신 라벨(teammate_id / from-name 그대로). claude 가 메시지를
    /// `@ <라벨>❯` 로 펼쳐 그릴 때 화면 제목과 대조하는 앵커다 — 사용자가 직접 친
    /// `@ …❯` 텍스트를 남의 메시지로 오인해 덮지 않기 위한 필수 관문.
    pub(crate) from_label: Option<String>,
    /// `from` 소켓 경로에서 뽑은 발신 pid. **발신 세션에 제목이 없으면 `from-name`
    /// 이 통째로 빠지고**(신생 pane 첫 메시지, 2026-08-12 실측) claude 는 이 pid 를
    /// 라벨로 그린다(`@ 12889❯`) — from_label 대조가 비는 그 경우의 보조 앵커.
    pub(crate) from_pid: Option<String>,
    /// 보낸 세션의 claude session id. **이름이 학생을 안 알려 줄 때의 정답**이다 —
    /// 명부의 이름은 세션 제목으로 덮이는 값이라(`mcp, skill사이드바`) 로스터에 없는
    /// 글자가 오기 일쑤인데, 세션 id 로는 그 pane 을 찾아 배정 학생을 직접 물을 수
    /// 있다. 제목을 뭐로 바꾸든 안 흔들린다.
    pub(crate) peer_sid: Option<String>,
}

/// `uds:/tmp/cc-socks/27516.sock` → 그 세션이 명부에 등록한 이름.
///
/// claude 는 cross-session 메시지를 `@peer` 라는 고정 라벨로 그린다(발신자 이름이
/// 명부에 멀쩡히 있어도 그렇다 — 2026-08-09 실측). 그 이름으로는 학생색도 프사도
/// 본문도 못 찾으므로, 태그가 실어 준 소켓 경로의 pid 로 명부를 되짚는다.
/// `from-name` 을 안 쓰는 이유는 그게 세션 이름이라 자동 제목에 덮이기 때문이다.
pub(crate) fn socket_pid(from: &str) -> Option<&str> {
    let pid = from.rsplit('/').next()?.strip_suffix(".sock")?;
    (!pid.is_empty() && pid.chars().all(|c| c.is_ascii_digit())).then_some(pid)
}

/// 소켓 경로 → 그 세션이 명부(`~/.claude/sessions/<pid>.json`)에 남긴 신원.
///
/// 둘을 같이 꺼내는 이유는 **이름만으로는 학생을 못 찾기 때문**이다. 명부의 `name`
/// 은 세션 제목이라 자동 요약에 덮인다 — 실측(2026-08-11)으로 모모이 pane 은
/// `mcp, skill사이드바` 였다. 거기엔 로스터가 아는 글자가 하나도 없어서 색도 프사도
/// 못 걸린다. `sessionId` 는 그런 일이 없고, 그걸로 pane 을 되짚으면 배정 학생을
/// 직접 물을 수 있다. 이름은 그래도 화면에 뭐라도 쓰기 위해 함께 들고 온다.
pub(crate) fn peer_ident_from_socket(from: &str) -> Option<(Option<String>, Option<String>)> {
    let pid = socket_pid(from)?;
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from)?;
    let path = home.join(".claude/sessions").join(format!("{pid}.json"));
    Some(peer_ident_from_json(&std::fs::read_to_string(path).ok()?))
}

/// 명부 파일 한 장에서 (이름, 세션 id). 파일 읽기와 갈라 둔 것은 검증 때문이다 —
/// 이 판정이 틀리면 남의 메시지가 조용히 기본 표시로 떨어질 뿐 아무 오류도 안 난다.
pub(crate) fn peer_ident_from_json(s: &str) -> (Option<String>, Option<String>) {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(s) else {
        return (None, None);
    };
    let pick = |k: &str| -> Option<String> {
        let s = v.get(k)?.as_str()?.trim();
        (!s.is_empty()).then(|| s.to_string())
    };
    (pick("name"), pick("sessionId"))
}

/// pane 에 도착한 남의 메시지 본문. 두 형식을 다 받는다.
///
/// 트리플을 걷어내기 전에는 팀 인박스만 있어 `<teammate-message teammate_id=… color=…>`
/// 하나였는데, cross-session 으로 옮기면서 `<cross-session-message from=… from-name=…>`
/// 이 새로 생겼다. 후자를 모르면 남의 메시지가 학생 테마 없이 claude 기본 표시
/// (`Message from @peer`)로만 뜬다 — 2026-08-09 회귀.
///
/// ⚠️ cross-session 태그엔 **`color` 가 없다.** 발신자 학생색은 여기서 못 얻으므로
/// 색 없이 돌려주고, 표시층이 이름으로 찾거나 기본색을 쓴다.
pub(crate) fn extract_teammate_msg(text: &str, sender: &str) -> Option<TeammateMsg> {
    extract_tagged_msg(text, sender, "<teammate-message", "teammate_id", "</teammate-message>")
        .or_else(|| {
            extract_tagged_msg(
                text,
                sender,
                "<cross-session-message",
                "from-name",
                "</cross-session-message>",
            )
        })
}

/// claude 가 cross-session 발신자를 부르는 고정 라벨. 이 이름으로는 아무것도 못 찾으므로
/// 태그를 이름 대조 없이 잡고 소켓 pid 로 진짜 발신자를 되찾는 신호로 쓴다.
pub(crate) const PEER_LABEL: &str = "peer";

/// 한 태그 형식에 대한 파싱 — 속성은 key="value" 나열(순서 무관).
pub(crate) fn extract_tagged_msg(
    text: &str,
    sender: &str,
    open: &str,
    id_attr: &str,
    close_tag: &str,
) -> Option<TeammateMsg> {
    let mut rest = text;
    loop {
        let s = rest.find(open)?;
        let after = &rest[s + open.len()..];
        let close = after.find('>')?;
        let attrs = &after[..close];
        let tail = &after[close + 1..];
        let attr = |key: &str| -> Option<String> {
            let pat = format!("{key}=\"");
            let a = attrs.find(&pat)? + pat.len();
            let e = attrs[a..].find('"')?;
            Some(attrs[a..a + e].to_string())
        };
        // `@peer` 로 뜬 줄은 이름 대조가 무의미하다 — 그 라벨은 발신자와 무관한
        // 고정값이라 어떤 태그와도 안 맞는다. 그래서 대조를 건너뛰고 최근 것을 잡되,
        // 소켓 pid 로 진짜 신원을 되찾아 함께 돌려준다(못 찾으면 라벨 그대로).
        let peer_probe = sender == PEER_LABEL && id_attr == "from-name";
        if peer_probe || attr(id_attr).as_deref() == Some(sender) {
            let end = tail.find(close_tag).unwrap_or(tail.len());
            let from = attr("from");
            let ident = peer_probe
                .then(|| from.clone())
                .flatten()
                .and_then(|f| peer_ident_from_socket(&f));
            return Some(TeammateMsg {
                body: tail[..end].trim().to_string(),
                color: attr("color"),
                sender: ident.as_ref().and_then(|(n, _)| n.clone()),
                from_label: attr(id_attr),
                from_pid: from.as_deref().and_then(socket_pid).map(str::to_string),
                peer_sid: ident.and_then(|(_, s)| s),
            });
        }
        rest = tail;
    }
}

/// jsonl 한 줄의 user 턴 텍스트 — content 가 문자열이면 그대로, 배열이면
/// text 블록들을 이어붙인다(팀메시지는 둘 다로 도착할 수 있다).
pub(crate) fn jsonl_user_text(v: &serde_json::Value) -> Option<String> {
    // 배달 attachment 는 본문이 여기 있다(위 게이트 주석 참고). `message` 필드가
    // 아예 없는 줄이라 아래 pointer 는 무조건 None 이 된다 — 폴백이 없으면 태그가
    // 눈앞에 있는데도 못 읽는다.
    if let Some(p) = v.pointer("/attachment/prompt").and_then(|p| p.as_str()) {
        return Some(p.to_string());
    }
    let c = v.pointer("/message/content")?;
    if let Some(s) = c.as_str() {
        return Some(s.to_string());
    }
    let mut out = String::new();
    for b in c.as_array()? {
        if let Some(t) = b.get("text").and_then(|t| t.as_str()) {
            out.push_str(t);
        }
    }
    (!out.is_empty()).then_some(out)
}

/// pane transcript tail 에서 sender 의 최신 팀메시지 — 파일 길이가 그대로면
/// 캐시 반환(프레임당 stat 1회), 대화가 자라 길이가 변했을 때만 재스캔.
pub(crate) fn latest_teammate_msg(path: &std::path::Path, sender: &str) -> Option<TeammateMsg> {
    type Cache =
        std::collections::HashMap<(std::path::PathBuf, String), (u64, Option<TeammateMsg>)>;
    static CACHE: std::sync::LazyLock<std::sync::Mutex<Cache>> =
        std::sync::LazyLock::new(Default::default);
    let len = std::fs::metadata(path).ok()?.len();
    let key = (path.to_path_buf(), sender.to_string());
    let mut map = CACHE.lock().ok()?;
    if let Some((l, m)) = map.get(&key) {
        if *l == len {
            return m.clone();
        }
    }
    let (tail, _) = crate::socket::read_tail(path, 256 * 1024);
    let found = tail.lines().rev().find_map(|l| {
        // ⚠️두 태그 형식을 **둘 다** 통과시켜야 한다. `<teammate-message` 만 보던
        // 동안 cross-session 배달(`<cross-session-message`)이 전부 걸러져, 272c508
        // 의 발신자 되짚기가 라이브에서 한 번도 돌지 않았다(2026-08-12 확정 —
        // 테스트는 이 프리필터를 우회해 extract 를 직접 불러서 못 잡았다).
        // `@peer` 라벨은 발신자와 무관한 고정값이라 이름 대조 자체를 건너뛴다.
        let tag_hit =
            l.contains("<teammate-message") || l.contains("<cross-session-message");
        let sender_hit = sender == PEER_LABEL || l.contains(sender);
        if !tag_hit || !sender_hit {
            return None;
        }
        let v: serde_json::Value = serde_json::from_str(l).ok()?;
        // user 턴만 — 수신측 assistant 가 프로즈에 태그 문자열을 인용하면("
        // <cross-session-message>에는 머리말만…" 같은 수신 확인) 역스캔이 그
        // 턴을 최신 메시지로 잡아 진짜 배달을 가린다(2026-08-12 실측: 그 인용
        // 하나로 7차 배달의 테마가 통째로 안 걸렸다).
        // ⚠️ claude 2.1.234 는 cross-session 배달을 `type:"user"` 가 아니라
        // **`type:"attachment"` + `attachment.type:"queued_command"`** 로 적는다
        // (그 줄엔 `message` 필드 자체가 없다). 이 게이트가 그걸 통째로 떨어뜨려
        // 발신자 되짚기·프사·학생색이 라이브에서 안 걸렸다(2026-08-18 실측).
        // 게이트의 목적은 **assistant 가 프로즈에 태그를 인용한 턴**을 막는 것이라,
        // 배달 attachment 를 통과시켜도 그 보호는 그대로다.
        let queued_delivery = v.get("type").and_then(|t| t.as_str()) == Some("attachment")
            && v.pointer("/attachment/type").and_then(|t| t.as_str()) == Some("queued_command");
        if v.get("type").and_then(|t| t.as_str()) != Some("user") && !queued_delivery {
            return None;
        }
        extract_teammate_msg(&jsonl_user_text(&v)?, sender)
    });
    map.insert(key, (len, found.clone()));
    found
}

/// 행 전체가 공백/blank 인가 — 팀메시지 줄바꿈 전개가 이어 쓸 수 있는 행.
pub(crate) fn row_is_blank(row: &[GridCell]) -> bool {
    row.iter().all(|c| matches!(c.ch, ' ' | '\0'))
}

/// 문자열의 셀 폭 합(와이드 글리프 2칸).
pub(crate) fn cell_width(s: &str) -> usize {
    use unicode_width::UnicodeWidthChar;
    s.chars().map(|c| c.width().unwrap_or(1).max(1)).sum()
}

/// 셀 폭 기준 word-wrap — 첫 줄은 first_w, 이후 줄은 cont_w 폭, 최대
/// max_lines 줄. 공백 경계 우선, 줄보다 긴 단어는 글자 단위 분할.
/// 반환 = (줄들, 본문이 남아 잘렸는지).
pub(crate) fn wrap_body_cells(
    text: &str,
    first_w: usize,
    cont_w: usize,
    max_lines: usize,
) -> (Vec<String>, bool) {
    let mut lines: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut cur_w = 0usize;
    for word in text.split(' ') {
        let ww = cell_width(word);
        let limit = if lines.is_empty() { first_w } else { cont_w };
        let need = if cur.is_empty() { ww } else { cur_w + 1 + ww };
        if need <= limit {
            if !cur.is_empty() {
                cur.push(' ');
                cur_w += 1;
            }
            cur.push_str(word);
            cur_w += ww;
            continue;
        }
        if !cur.is_empty() {
            let full = lines.len() + 1 >= max_lines;
            lines.push(std::mem::take(&mut cur));
            cur_w = 0;
            if full {
                return (lines, true);
            }
        }
        // 단어가 다음 줄에도 통째로 안 들어가면 글자 단위로 쪼갠다.
        let mut rest = word;
        loop {
            let limit = if lines.is_empty() { first_w } else { cont_w };
            if cell_width(rest) <= limit {
                cur = rest.to_string();
                cur_w = cell_width(&cur);
                break;
            }
            let mut take_b = 0usize;
            let mut tw = 0usize;
            for ch in rest.chars() {
                use unicode_width::UnicodeWidthChar;
                let cw = ch.width().unwrap_or(1).max(1);
                if tw + cw > limit {
                    break;
                }
                tw += cw;
                take_b += ch.len_utf8();
            }
            if take_b == 0 {
                // 폭 0/극단 — 무한루프 방지로 최소 한 글자는 넘긴다.
                take_b = rest.chars().next().map(|c| c.len_utf8()).unwrap_or(0);
                if take_b == 0 {
                    break;
                }
            }
            let full = lines.len() + 1 >= max_lines;
            lines.push(rest[..take_b].to_string());
            rest = &rest[take_b..];
            if full {
                return (lines, true);
            }
        }
    }
    if !cur.is_empty() || lines.is_empty() {
        lines.push(cur);
    }
    (lines, false)
}

/// 접힌 팀메시지를 학생색으로 전개(스냅샷 전용, 원본 그리드 무손상) — 본문이
/// 있으면 그 행을 "@ 이름❯ 본문"으로 갈아끼우고, **아래 blank 행이 있는 만큼
/// 줄바꿈으로 이어 쓴다**(거노: 한 줄 말줄임 말고 펼쳐서). 그리드는 reflow 가
/// 안 되니 빈 행 너머로 남는 본문은 '…' — 전문은 hover 말풍선이 담당. 다음
/// 항목과의 구분 blank 1행은 남기고, 뷰포트 바닥까지 전부 빈 경우엔 끝까지
/// 쓴다. 본문이 없으면 원문 글자에 색만. 와이드 글리프는 글자 + ' ' 스페이서
/// 2칸(배너 타이틀 치환과 같은 composed 경로 실측).
pub(crate) fn expand_teammate_message(
    rows: &mut [Vec<GridCell>],
    r: usize,
    start: usize,
    sender: &str,
    body: Option<&str>,
    accent: [u8; 4],
) -> Option<usize> {
    let fg = kasa_bridge::screen::Color::Rgb(accent[0], accent[1], accent[2]);
    let Some(body) = body else {
        for c in rows[r].iter_mut() {
            if c.ch != ' ' && c.ch != '\0' {
                c.fg = fg.clone();
            }
        }
        return None;
    };
    let cols = rows[r].len();
    if start >= cols || cols == 0 {
        return None;
    }
    let style = rows[r][start].clone();
    let blank_run = rows[r + 1..].iter().take_while(|w| row_is_blank(w)).count();
    let usable = if r + 1 + blank_run >= rows.len() {
        blank_run
    } else {
        blank_run.saturating_sub(1)
    };
    // 발신자가 배정 학생이면 이름 텍스트 대신 프사(bust) — tell 렌더와 같은 시각
    // 언어(거노 2026-07-27: SendMessage 도 학생 테마로). 프사는 첫 줄 왼쪽 여백
    // 2칸에 얹으므로(호출측 이미지 패스) 헤더는 그만큼 비운다 — 그 폭이 곧 이어
    // 쓰는 줄의 들여쓰기(indent = start+2)라 본문 좌측이 한 줄로 선다.
    let face_slug = teammate_sender_slug(sender);
    let head_start = start;
    let header = if face_slug.is_some() {
        "  ".to_string()
    } else {
        format!("@ {sender}❯ ")
    };
    let indent = start + 2;
    let flat = body.split_whitespace().collect::<Vec<_>>().join(" ");
    let (lines, truncated) = wrap_body_cells(
        &flat,
        cols.saturating_sub(head_start + cell_width(&header)),
        cols.saturating_sub(indent),
        1 + usable,
    );
    // 행 하나에 텍스트를 칠하는 공용 페인터 — 다음 칸 index 를 돌려준다.
    let put_line = |row: &mut [GridCell], mut w: usize, text: &str, bold: bool| -> usize {
        use unicode_width::UnicodeWidthChar;
        for ch in text.chars() {
            let cw = ch.width().unwrap_or(1).max(1);
            if w + cw > row.len() {
                break;
            }
            let mut cell = style.clone();
            cell.ch = ch;
            cell.fg = fg.clone();
            cell.bold = bold;
            row[w] = cell;
            if cw == 2 {
                let mut sp = style.clone();
                sp.ch = ' ';
                sp.fg = fg.clone();
                sp.bold = bold;
                row[w + 1] = sp;
            }
            w += cw;
        }
        w
    };
    let ellipsis = |row: &mut [GridCell], w: usize| {
        let p = w.min(row.len() - 1);
        let mut cell = style.clone();
        cell.ch = '…';
        cell.fg = fg.clone();
        row[p] = cell;
        p + 1
    };
    let old_end = rows[r]
        .iter()
        .rposition(|c| c.ch != ' ' && c.ch != '\0')
        .map(|p| p + 1)
        .unwrap_or(0);
    // 프사 자리(start..head_start)는 비워 둔다 — 원문 "› Message from @…" 잔재가
    // 프사 뒤로 비쳐 보이면 안 된다.
    for c in rows[r][start..head_start.min(cols)].iter_mut() {
        *c = GridCell::blank();
    }
    let mut w = put_line(&mut rows[r], head_start, &header, true);
    if let Some(first) = lines.first() {
        w = put_line(&mut rows[r], w, first, false);
    }
    if lines.len() == 1 && truncated {
        w = ellipsis(&mut rows[r], w);
    }
    // 새 텍스트가 원문("› Message from @…")보다 짧으면 잔재를 지운다.
    for c in rows[r][w..old_end.max(w)].iter_mut() {
        *c = GridCell::blank();
    }
    for (i, line) in lines.iter().enumerate().skip(1) {
        let row = &mut rows[r + i];
        let w = put_line(row, indent, line, false);
        if i == lines.len() - 1 && truncated {
            ellipsis(row, w);
        }
    }
    face_slug.map(|_| start)
}


/// agents 목록 뷰에서 SCHALE 로고를 얹을 위치 — "Claude Code" 헤더 행을 찾아 그
/// 왼쪽 여백(logo_cols + 2칸 갭 앞)의 top-left (row, col)을 돌려준다. Clawd 블록아트가
/// 없는 목록 뷰에서 startup 배너의 Clawd 자리와 같은 쪽(헤더 왼쪽)에 앵커한다.
pub(crate) fn find_agents_header_anchor(rows: &[Vec<GridCell>], logo_cols: usize) -> Option<(usize, usize)> {
    for (r, row) in rows.iter().enumerate() {
        let line: String = row.iter().map(|c| c.ch).collect();
        if let Some(idx) = line.find("Claude Code") {
            return Some((r, idx.saturating_sub(logo_cols + 2)));
        }
    }
    None
}

/// claude /resume 피커 행의 학생 태그(` · #학생이름`) 탐지 — (태그 '#' col,
/// 이름 끝 col, 학생 slug). resume_visibility 스위퍼가 세션 설명줄 끝에 스탬프한
/// 태그가 앵커다. 요건 3중: ① '#' 바로 앞이 " ·"(피커 구분자) ② 그 앞 어딘가
/// 또 다른 '·'(설명줄은 "날짜 · 크기 · #태그" 꼴로 '·' 2개 이상) ③ '#' 뒤 연속
/// 텍스트가 로스터 이름 — 이라 일반 터미널 출력 오탐은 사실상 없다. PR 번호
/// (`repo#12`) 같은 다른 '#' 는 이름 검증에서 떨어지므로 행의 모든 '#' 후보를
/// 순서대로 시도한다.
/// 그리드 행 → (스페이서를 흡수한 실제 텍스트, 각 텍스트 char 의 셀 col). 와이드
/// 문자(한글, ≥U+1100) 다음의 스페이서 셀('\0', 또는 alacritty composed 의 직후
/// ' ')을 소비해, 캐시된 세션 name 을 셀 텍스트에서 그대로 substring 검색할 수 있게
/// 한다(picker_student_tag 와 동일한 wide 스페이서 규칙). agents 뷰 세션 행 칩용.
pub(crate) fn row_text_cells(row: &[GridCell]) -> (String, Vec<usize>) {
    let mut text = String::new();
    let mut cols = Vec::new();
    let mut spacer_pending = false;
    for (i, cell) in row.iter().enumerate() {
        match cell.ch {
            '\0' => spacer_pending = false,
            ' ' if spacer_pending => spacer_pending = false,
            ch => {
                text.push(ch);
                cols.push(i);
                spacer_pending = (ch as u32) >= 0x1100;
            }
        }
    }
    (text, cols)
}

pub(crate) fn picker_student_tag(row: &[GridCell]) -> Option<(usize, usize, &'static str)> {
    for (c0, _) in row.iter().enumerate().filter(|(_, c)| c.ch == '#') {
        if c0 < 2 || row[c0 - 1].ch != ' ' || row[c0 - 2].ch != '·' {
            continue;
        }
        if !row[..c0 - 2].iter().any(|c| c.ch == '·') {
            continue;
        }
        let mut name = String::new();
        let mut end = c0;
        // 와이드 문자(한글) 다음 한 칸은 스페이서 셀 — 그리드 경로에 따라 '\0'
        // 또는 ' ' 로 온다(alacritty composed 는 ' ', 실측). 직전 문자가 와이드일
        // 때만 스페이서로 소비하고, 그 외 공백은 이름 종료.
        let mut spacer_pending = false;
        for (i, cell) in row.iter().enumerate().skip(c0 + 1) {
            match cell.ch {
                ' ' | '\0' if spacer_pending => {
                    end = i;
                    spacer_pending = false;
                }
                ' ' | '\0' => break,
                ch => {
                    name.push(ch);
                    end = i;
                    spacer_pending = (ch as u32) >= 0x1100;
                    if name.chars().count() > 6 {
                        break; // 로스터 이름은 최장 3자 — 과도하면 태그 아님
                    }
                }
            }
        }
        if let Some(slug) = theme::character_slug(&name) {
            return Some((c0, end, slug));
        }
    }
    None
}

/// 글 흐름 안에 얹을 그림 한 장 — `[[img:<경로>:<행수>]]` 표식이 잡힌 자리.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ImageBlock {
    /// 표식이 앉은 행. 이 행부터 `rows` 행이 그림 자리다.
    pub row: usize,
    pub path: String,
    /// 그림이 차지할 행 수(표식 행 포함).
    pub rows: usize,
}

/// 글 흐름 안 그림 표식 `[[img:<경로>:<행수>]]` 을 찾는다.
///
/// 터미널에 「여기 그림을 그려라」를 알리는 표준 신호(OSC 1337)가 있지만 claude
/// pane 에서는 못 쓴다 — claude code 가 도구 출력의 제어문자를 벗겨 내서 PTY 까지
/// 닿지 않는다(2026-08-15 실측: `ESC` 가 사라진 채 `]1337;…` 만 도착). 그래서
/// **화면에 남은 평범한 글자**를 신호로 삼는다. 그 대신 그림이 앉을 자리는 글을
/// 쓰는 쪽이 만들어야 한다(표식 아래에 그 행 수만큼 줄을 채워 둔다) — 격자를
/// 소유한 것은 claude code 라 우리가 줄을 밀어 낼 수 없기 때문이다.
///
/// 빈 줄이 아니라 점 같은 글자로 채워야 한다. 마크다운 렌더러가 연속 빈 줄을
/// 하나로 접어서, 빈 줄로 만든 자리는 화면에 남지 않는다.
pub(crate) fn find_image_blocks(rows: &[Vec<GridCell>]) -> Vec<ImageBlock> {
    const HEAD: &str = "[[img:";
    let mut out = Vec::new();
    for (r, row) in rows.iter().enumerate() {
        // 값싼 프리체크 — 대부분의 행에는 `[[` 가 없고, 여긴 매 프레임 모든
        // pane 의 모든 행을 지난다. 문자열을 만드는 건 그 다음이다.
        if !row.windows(2).any(|w| w[0].ch == '[' && w[1].ch == '[') {
            continue;
        }
        let (text, _) = row_text_cells(row);
        let Some(at) = text.find(HEAD) else { continue };
        let Some(end) = text[at..].find("]]") else { continue };
        let body = &text[at + HEAD.len()..at + end];
        // 경로에 `:` 가 들어갈 수 있으니 **마지막** 콜론에서 가른다.
        let Some(cut) = body.rfind(':') else { continue };
        let Ok(n) = body[cut + 1..].trim().parse::<usize>() else { continue };
        let path = body[..cut].trim();
        // 0 행은 자리가 없다는 뜻이라 그릴 데가 없고, 상한은 폭주 방지다.
        if path.is_empty() || n == 0 || n > 200 {
            continue;
        }
        out.push(ImageBlock { row: r, path: path.to_string(), rows: n });
    }
    out
}

/// 그림이 앉을 행들을 비운다 — 표식 글자와 자리를 채운 점이 그림 뒤로 비치지
/// 않게. `find_sticky_prompt` 가 감지 행을 지우고 pill 을 얹는 것과 같은 수순이다.
pub(crate) fn blank_image_block(rows: &mut [Vec<GridCell>], at: &ImageBlock) {
    for row in rows.iter_mut().skip(at.row).take(at.rows) {
        for cell in row.iter_mut() {
            *cell = GridCell::blank();
        }
    }
}

/// 화면에 남은 첨부 이미지 자리 — claude code 는 붙인 그림을 `[Image #6]` 이라는
/// 글자로만 표시해서, 무슨 그림이었는지 화면만 봐서는 알 수가 없다.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ImageRef {
    pub row: usize,
    /// 참조가 차지한 셀 구간(양끝 포함) — 호버 히트 박스.
    pub col0: usize,
    pub col1: usize,
    /// `#` 뒤 번호. transcript jsonl 의 `imagePasteIds` 와 같은 값이라(실측
    /// 2026-08-15) 이 숫자 하나로 원본 base64 를 되찾을 수 있다.
    pub n: u32,
}

/// 그리드에서 `[Image #N]` 참조를 전부 찾는다.
///
/// 한 행 안에서만 찾는다 — 좁은 pane 에서 랩되어 `[Image` 와 `#6]` 이 두 행에
/// 걸리면 놓친다. 이어 붙여 찾으면 본문에 우연히 인접한 두 조각까지 걸리는데,
/// 11자 참조가 랩 경계에 정확히 걸릴 일보다 그 오탐이 더 잦다.
pub(crate) fn find_image_refs(rows: &[Vec<GridCell>]) -> Vec<ImageRef> {
    const HEAD: &str = "[Image #";
    let mut out = Vec::new();
    for (r, row) in rows.iter().enumerate() {
        let (text, cols) = row_text_cells(row);
        let bytes = text.as_bytes();
        let mut from = 0usize;
        while let Some(rel) = text[from..].find(HEAD) {
            let start = from + rel;
            let mut i = start + HEAD.len();
            let digits = i;
            while bytes.get(i).is_some_and(u8::is_ascii_digit) {
                i += 1;
            }
            // `[Image #]` (숫자 없음)과 닫히지 않은 것은 참조가 아니다.
            if i > digits && bytes.get(i) == Some(&b']') {
                if let Ok(n) = text[digits..i].parse::<u32>() {
                    // find 는 바이트 오프셋, cols 는 char 인덱스 — HEAD 앞이 한글이면
                    // 둘이 어긋나므로 char 로 환산해서 셀을 짚는다.
                    let c_start = text[..start].chars().count();
                    let c_end = text[..=i].chars().count() - 1;
                    if let (Some(&col0), Some(&col1)) = (cols.get(c_start), cols.get(c_end)) {
                        out.push(ImageRef { row: r, col0, col1, n });
                    }
                }
            }
            from = i.max(start + 1);
        }
    }
    out
}

/// 인라인 이미지(OSC 1337)를 올리고 그린다 — 파일에서 한 번 디코드해 텍스처로
/// 올리고, 이번 프레임 배치에 없는 키는 텍스처를 놓는다. PTY 쪽이 뷰포트에
/// 겹치는 그림만 보내므로, 스크롤로 벗어난 그림의 GPU 메모리가 여기서 함께
/// 회수된다(안 놓으면 샌다). 디코드에 실패한 키는 false 로 남겨 매 프레임
/// 재시도하지 않는다.
///
/// 키 집합은 이번 프레임에 **렌더된 pane** 기준이다 — 워크스페이스를 전환하면
/// 그쪽 그림 텍스처가 놓였다가 돌아올 때 다시 디코드된다(전환은 드물고
/// 디코드는 ms 급이라 캐시를 창 넘어 유지할 이유가 없다).
/// `hug` 가 참이면 박스를 그림 비율만큼 좁혀 **왼쪽에 붙인다**. `queue_image` 의
/// contain-fit 은 박스 안 중앙 정렬이라, 준 박스가 그림보다 넓으면 글 흐름에서
/// 그림만 한가운데로 떨어져 나온다. OSC 1337 경로는 PTY 가 셀 수를 재어 주므로
/// 박스가 이미 맞아 이 손질이 필요 없다.
pub(crate) fn paint_inline_images(
    g: &mut gpu::GpuRenderer,
    slots: &[(String, String, f32, f32, f32, f32, f32, f32, bool)],
) {
    // 값은 디코드한 픽셀 크기 — `hug` 가 박스를 좁히는 데 쓴다. `None` 은 디코드
    // 실패라, 매 프레임 같은 파일을 다시 열지 않게 남겨 둔다.
    static UPLOADED: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<String, Option<(u32, u32)>>>,
    > = std::sync::OnceLock::new();
    let mut up = UPLOADED.get_or_init(Default::default).lock().unwrap();
    let live: std::collections::HashSet<&str> =
        slots.iter().map(|s| s.0.as_str()).collect();
    up.retain(|k, _| {
        let keep = live.contains(k.as_str());
        if !keep {
            g.drop_image(k);
        }
        keep
    });
    for (key, path, x, y, w, h, c0, c1, hug) in slots {
        if !up.contains_key(key) {
            let dims = std::fs::read(path)
                .ok()
                .and_then(|b| image::load_from_memory(&b).ok())
                .map(|img| {
                    let rgba = img.to_rgba8();
                    let (iw, ih) = rgba.dimensions();
                    g.upload_image(key, &rgba, iw, ih);
                    (iw, ih)
                });
            up.insert(key.clone(), dims);
        }
        let Some(Some((iw, ih))) = up.get(key).copied() else { continue };
        let bw = if *hug && ih > 0 {
            // no-upscale 캡이 있어 그림이 박스보다 작으면 원본 크기로 그려진다 —
            // 좁힐 폭도 그 실제 크기를 넘지 않아야 왼쪽에 붙는다.
            (h * iw as f32 / ih as f32).min(iw as f32).min(*w)
        } else {
            *w
        };
        g.push_clip(*x, *c0, bw, *c1 - *c0);
        g.queue_image(key, *x, *y, bw, *h, 1.0, 0.0, 0.0);
        g.pop_clip();
    }
}

/// claude 2.1.228 이 세션명 자리(입력박스 상단 보더 우측 끝)에 그리는 ` ultracode `
/// 배지를 보더 대시로 되메운다 — 모드는 입력박스 글로우가 이미 말하므로 글자는
/// 중복이고, 그 자리는 /rename 세션명 자리라 이름이 바뀐 것처럼 읽힌다(2026-08-12
/// 지적). ` ultracode  fast ` 처럼 이어 붙는 태그도 같은 섬이라 함께 지워진다.
/// 진짜 세션명이 이 단어로 시작하는 극단 케이스만 함께 잃는다 — find_titled_rule
/// 의 스킵과 같은 트레이드. 스킵은 이 소거가 안 도는 경로의 보험으로 남긴다.
pub(crate) fn erase_ultracode_badge(rows: &mut [Vec<GridCell>]) {
    let n = rows.len();
    for r in (n.saturating_sub(10)..n).rev() {
        let row = &mut rows[r];
        let dashes = row.iter().filter(|c| c.ch == '─').count();
        if dashes < row.len() / 2 {
            continue;
        }
        let is_name = |c: &GridCell| {
            !matches!(c.ch, ' ' | '\0') && !('\u{2500}'..='\u{257F}').contains(&c.ch)
        };
        let Some(first) = row.iter().position(is_name) else { continue };
        let Some(last) = row.iter().rposition(is_name) else { continue };
        let island: String = row[first..=last]
            .iter()
            .filter_map(|c| (c.ch != '\0').then_some(c.ch))
            .collect();
        if !island.trim().starts_with("ultracode") {
            continue;
        }
        // 섬과 양옆 공백(대시 경계 안쪽 전부)을 이웃 대시 셀 스타일로 되메운다 —
        // 색·배경이 보더와 이어져야 이음매가 안 보인다.
        let c0 = row[..first].iter().rposition(|c| c.ch == '─').map_or(first, |i| i + 1);
        let c1 = row[last + 1..]
            .iter()
            .position(|c| c.ch == '─')
            .map_or(last, |i| (last + 1 + i).saturating_sub(1));
        let Some(donor) = row[..c0]
            .iter()
            .rev()
            .find(|c| c.ch == '─')
            .or_else(|| row[c1 + 1..].iter().find(|c| c.ch == '─'))
            .cloned()
        else {
            continue;
        };
        for c in row[c0..=c1].iter_mut() {
            *c = donor.clone();
        }
        return;
    }
}

/// claude 입력박스 위 "── 세션명 ──" 구분선의 이름 구간 위치(거노: rename 아웃라인).
/// 하단 10행에서 대시가 지배적이고 비-대시 텍스트 섬이 있는 rule 행을 찾아, **좌우 대시
/// 런 사이**(양옆 공백 포함)의 (row, c0, c1)을 돌려준다. 이름 글자 셀이 아니라 대시 경계로
/// 잡아야 한글 같은 와이드(2셀) 문자의 둘째 셀까지 박스 안에 정확히 들어온다(거노: 칸 안맞음).
/// 순수 '─' rule·statusline·입력행은 걸러진다.
pub(crate) fn find_titled_rule(rows: &[Vec<GridCell>]) -> Option<(usize, usize, usize)> {
    let n = rows.len();
    for r in (n.saturating_sub(10)..n).rev() {
        let row = &rows[r];
        let dashes = row.iter().filter(|c| c.ch == '─').count();
        if dashes < row.len() / 2 {
            continue;
        }
        // 이름 섬이 없는 순수 '─' rule(입력박스 바닥 테두리 등)은 건너뛴다 — `?` 로 함수를
        // 끝내면 그 아래 순수 rule 이 세션명 줄보다 먼저 걸려 아웃라인이 통째 사라진다(거노).
        // box-drawing 문자(╭╮╰╯│…, U+2500-257F) 전체를 이름에서 제외 — 둥근 입력박스
        // 테두리 행(╭────╮)의 모서리가 이름 섬으로 오탐되어 행 전체에 사각형이 그려졌다(거노).
        let is_name = |c: &GridCell| {
            !matches!(c.ch, ' ' | '\0') && !('\u{2500}'..='\u{257F}').contains(&c.ch)
        };
        let Some(first) = row.iter().position(&is_name) else { continue };
        let Some(last) = row.iter().rposition(&is_name) else { continue };
        // teammate 칩(`──── @이름 ──`)은 claude 네이티브가 그리는 agent 배지지
        // 세션명이 아니다 — 아웃라인을 두르면 칩에 네모칸이 생긴다(거노 2026-07-27).
        if row[first].ch == '@' {
            continue;
        }
        // ` ultracode ` 배지도 마찬가지 — claude 2.1.228 이 세션명과 같은 자리
        // (상단 보더 우측 끝)에 그리는 모드 표시라, 아웃라인을 두르면 /rename 된
        // 것처럼 보인다(2026-08-12 지적). 진짜 세션명이 이 단어로 시작하는 극단
        // 케이스만 함께 잃는다.
        let island: String = row[first..=last]
            .iter()
            .filter_map(|c| (c.ch != '\0').then_some(c.ch))
            .collect();
        if island.trim().starts_with("ultracode") {
            continue;
        }
        // 이름 왼쪽의 마지막 '─' 다음 셀 = c0(선행 공백 포함), 오른쪽 첫 '─' 이전 셀 = c1
        // (와이드 문자 둘째 셀·후행 공백 포함). 대시 런이 없으면 이름 셀로 폴백.
        let c0 = row[..first].iter().rposition(|c| c.ch == '─').map_or(first, |i| i + 1);
        let c1 = row[last + 1..]
            .iter()
            .position(|c| c.ch == '─')
            .map_or(last, |i| (last + 1 + i).saturating_sub(1));
        return Some((r, c0, c1));
    }
    None
}

/// Clawd 시작 배너 감지. 결정행(몸통 2행째)의 9글리프 시퀀스를 찾고 바로
/// 윗행의 머리 7글리프로 확정한다 — 이 조합은 일반 텍스트에서 사실상
/// 나올 수 없다. 스크롤로 배너가 뷰포트 가장자리에 걸치면 보이는 행만으로
/// 감지한다(거노: 스크롤 살짝 내리면 Clawd 원본이 노출) — 위로 잘리면
/// top_row 가 음수, 아래로 잘리면 박스가 화면 밖까지 이어진다. 호출측은
/// blank 범위를 스냅샷 안으로 클램프하고 스프라이트를 pane 세로로 클립할 것.
/// 반환: 배너 박스의 (top_row, left_col) 목록.
/// 행 스캔은 첫 글리프 비교로 즉시 탈락하므로 프레임당 비용 미미.
/// 감지된 Claude Code 스크롤 sticky prompt 한 건(셀 좌표 + 보이는 텍스트).
pub(crate) struct StickyPrompt {
    pub row: usize,
    pub col_start: usize,
    pub col_end: usize, // exclusive
    pub text: String,
}

thread_local! {
    /// 이번 프레임에 그린 sticky pill 들의 (소속 pane id, 클릭 히트 rect(logical
    /// px), 보이는 텍스트). render 가 매 프레임 새로 채우고, mouse handler 는
    /// 클릭 판정에, seek 진행은 "지금 그 pane 의 sticky 텍스트"를 읽는 데 쓴다.
    /// struct App 무접촉(병렬 작업 규칙) — GUI 단일 스레드라 thread_local 로 충분.
    pub(crate) static STICKY_PILLS:
        std::cell::RefCell<Vec<(String, (f32, f32, f32, f32), String)>> =
        std::cell::RefCell::new(Vec::new());

    /// 진행 중인 sticky 클릭 seek. 클릭이 target(그 프롬프트 첫 줄 텍스트)을 잡고,
    /// about_to_wait 가 매 틱 wheel-up 한 노치씩 보내 화면을 관찰한다 — target 이
    /// 뷰포트로 들어와 sticky 텍스트가 바뀌거나(또는 최상단 도달로 사라지면) 멈춘다.
    pub(crate) static STICKY_SEEK: std::cell::RefCell<Option<StickySeek>> =
        std::cell::RefCell::new(None);
}

/// 클릭한 sticky 프롬프트를 화면으로 끌어오는 seek 상태(struct App 밖 — 무접촉).
pub(crate) struct StickySeek {
    pub pane_id: String,
    pub target: String,
    /// 아래(최신)로 갈지. claude 자기 버퍼라 좌표를 모르므로 방향과 종료 조건으로만
    /// 말할 수 있다 — 위로는 「이 줄이 바뀔 때까지」, 아래로는 그에 더해 「줄이 아예
    /// 사라질 때까지」(=라이브 바닥에 닿아 sticky 가 걷힌다).
    pub down: bool,
    /// wheel SGR 를 쏠 pane-local 셀(클릭 지점) — 노치마다 재사용.
    pub cell: (u16, u16),
    pub last_send: std::time::Instant,
    pub sent: u32,
}

/// 노치 간 최소 간격 — 33ms 펌프 틱보다 짧게 잡아 틱마다 한 노치가 나가되,
/// PTY 리페인트가 반영될 시간은 준다(로컬 리페인트는 보통 이보다 빠름).
pub(crate) const STICKY_SEEK_INTERVAL: std::time::Duration = std::time::Duration::from_millis(20);
/// 폭주 방지 상한(정상 종료가 먼저 걸린다). 500 노치면 어떤 화면이든 최상단 도달.
pub(crate) const STICKY_SEEK_MAX: u32 = 500;

/// seek 이 진행 중인가 — about_to_wait 의 30fps 펌프 게이트.
pub(crate) fn sticky_seek_active() -> bool {
    STICKY_SEEK.with(|s| s.borrow().is_some())
}

/// 이번 프레임 그 pane 에 그려진 sticky pill 텍스트(없으면 None = sticky 사라짐).
pub(crate) fn sticky_text_for(pane_id: &str) -> Option<String> {
    STICKY_PILLS.with(|s| {
        s.borrow()
            .iter()
            .find(|(id, _, _)| id == pane_id)
            .map(|(_, _, t)| t.clone())
    })
}

/// sticky 클릭 → seek 시작. target 은 클릭한 pill 텍스트, cell 은 wheel 을 쏠 위치.
pub(crate) fn begin_sticky_seek(pane_id: String, target: String, cell: (u16, u16), down: bool) {
    let now = std::time::Instant::now();
    STICKY_SEEK.with(|s| {
        *s.borrow_mut() = Some(StickySeek {
            pane_id,
            target,
            down,
            cell,
            // 첫 틱에 바로 한 노치 나가게 간격만큼 과거로.
            last_send: now.checked_sub(STICKY_SEEK_INTERVAL).unwrap_or(now),
            sent: 0,
        });
    });
}

/// seek 한 스텝. 다음 노치를 보내야 하면 (pane_id, col, row) 반환, 아니면 None
/// (대기 중이거나 종료). 종료 판정: 현재 sticky 텍스트가 target 과 다르면(타깃이
/// 뷰포트로 들어옴) 또는 없으면(최상단) 완료로 보고 상태를 지운다.
pub(crate) fn sticky_seek_step() -> Option<(String, u16, u16, bool)> {
    let now = std::time::Instant::now();
    STICKY_SEEK.with(|s| {
        let mut b = s.borrow_mut();
        let seek = b.as_mut()?;
        let reached = match sticky_text_for(&seek.pane_id) {
            None => true,
            Some(t) => t != seek.target,
        };
        if reached || seek.sent >= STICKY_SEEK_MAX {
            *b = None;
            return None;
        }
        if now.duration_since(seek.last_send) < STICKY_SEEK_INTERVAL {
            return None; // 직전 노치의 리페인트 대기
        }
        seek.last_send = now;
        seek.sent += 1;
        let (col, row) = seek.cell;
        Some((seek.pane_id.clone(), col, row, seek.down))
    })
}

/// 저채도·중간 밝기 = "흐릿한 회색" fg. Claude Code 가 dim SGR(2) 대신 회색
/// 전경색으로 sticky 를 흐리게 줄 때를 위한 폴백 판정(dim 플래그와 OR).
pub(crate) fn is_grayish_fg(fg: &kasa_bridge::screen::Color) -> bool {
    use kasa_bridge::screen::Color;
    match fg {
        Color::Idx(8) | Color::Idx(7) => true, // bright black / white-gray
        Color::Rgb(r, g, b) => {
            let (r, g, b) = (*r as i32, *g as i32, *b as i32);
            let mx = r.max(g).max(b);
            let mn = r.min(g).min(b);
            (mx - mn) < 36 && (56..=190).contains(&mx) // 저채도 + 중간 밝기
        }
        _ => false,
    }
}

/// 한 행의 보이는 텍스트 구간 요약: (text, first_col, last_col_excl, 글자수,
/// 흐릿한 글자수). 후행 공백은 텍스트에서 트림한다.
pub(crate) fn sticky_row_span(row: &[GridCell]) -> (String, usize, usize, usize, usize) {
    let mut text = String::new();
    let mut first: Option<usize> = None;
    let mut last = 0usize;
    let mut glyphs = 0usize;
    let mut dim = 0usize;
    for (i, c) in row.iter().enumerate() {
        let visible = c.ch != ' ' && c.ch != '\0';
        if visible {
            if first.is_none() {
                first = Some(i);
            }
            last = i + 1;
            glyphs += 1;
            if c.dim || is_grayish_fg(&c.fg) {
                dim += 1;
            }
        }
        if first.is_some() {
            text.push(if c.ch == '\0' { ' ' } else { c.ch });
        }
    }
    let first = first.unwrap_or(0);
    (text.trim_end().to_string(), first, last, glyphs, dim)
}

/// Claude Code 의 스크롤 sticky prompt 감지. mouse-tracking TUI 라 kasaterm 은
/// 뷰포트 스크롤 여부를 직접 못 안다 — 화면에 "Jump to bottom" 힌트(=위로
/// 스크롤된 상태)가 있을 때만, 최상단의 흐릿한 프롬프트 행을 sticky 로 본다.
/// 이 게이트가 평상시(맨 아래) 오탐을 막는다. `KASATERM_STICKY_DEBUG=1` 이면
/// 게이트 결과와 상단 행 스캔을 stderr 로 흘려 실측 튜닝을 돕는다.
pub(crate) fn find_sticky_prompt(rows: &[Vec<GridCell>]) -> Option<StickyPrompt> {
    let dbg = std::env::var_os("KASATERM_STICKY_DEBUG").is_some();
    // 스크롤 게이트: "jump to bottom" / ("bottom" & "click") 관대 매치.
    let scrolled = rows.iter().any(|r| {
        let s: String = r.iter().map(|c| c.ch).collect::<String>().to_lowercase();
        s.contains("jump to bottom") || (s.contains("bottom") && s.contains("click"))
    });
    if dbg {
        eprintln!("[sticky] scrolled_gate={scrolled} rows={}", rows.len());
    }
    if !scrolled {
        return None;
    }
    // 최상단 몇 행에서 "흐릿한 글자가 우세하고 실제 텍스트가 있는" 행.
    for ri in 0..rows.len().min(3) {
        let (text, first, last, glyphs, dim) = sticky_row_span(&rows[ri]);
        if dbg {
            eprintln!(
                "[sticky] row{ri} glyphs={glyphs} dim={dim} cols={first}..{last} text={:?}",
                text.chars().take(48).collect::<String>()
            );
        }
        if glyphs >= 2 && dim * 2 >= glyphs {
            return Some(StickyPrompt {
                row: ri,
                col_start: first,
                col_end: last,
                text,
            });
        }
    }
    None
}

/// agy 시작 로고 자리(왼쪽 위 칸, 스크롤로 잘렸으면 top 이 음수).
///
/// 아래로 갈수록 한 칸씩 넓어지는 5행 아트라, 가장 넓고 특징적인 **맨 아랫줄**로
/// 자리를 잡고 위 네 줄은 화면에 남아 있는 것만 대조한다 — Clawd 쪽과 같은 규약이다.
pub(crate) fn find_agy_banners(rows: &[Vec<GridCell>]) -> Vec<(isize, usize)> {
    // (맨 아랫줄 기준 들여쓰기, 글리프). 배열 순서는 로고 위→아래.
    const SHAPE: [(usize, &str); AGY_ROWS] = [
        (4, "▄▀▀▄"),
        (3, "▀▀▀▀▀▀"),
        (2, "▀▀▀▀▀▀▀▀"),
        (1, "▄▀▀    ▀▀▄"),
        (0, "▄▀▀      ▀▀▄"),
    ];
    let hit = |row: &[GridCell], at: usize, pat: &str| {
        at + pat.chars().count() <= row.len()
            && pat.chars().enumerate().all(|(i, p)| row[at + i].ch == p)
    };
    let mut out = Vec::new();
    for r in 0..rows.len() {
        let mut c = 0usize;
        while c + AGY_COLS <= rows[r].len() {
            if hit(&rows[r], c, SHAPE[AGY_ROWS - 1].1) {
                let top = r as isize - (AGY_ROWS as isize - 1);
                let ok = SHAPE[..AGY_ROWS - 1].iter().enumerate().all(|(i, &(ind, pat))| {
                    let gr = top + i as isize;
                    gr < 0 || hit(&rows[gr as usize], c + ind, pat)
                });
                if ok {
                    out.push((top, c));
                    c += AGY_COLS;
                    continue;
                }
            }
            c += 1;
        }
    }
    out
}

/// 로고 칸에 **Clawd 칸 비율(9x3)을 유지한 채** 최대로 맞춘 도트 상자.
/// Clawd 자신에겐 항등이라 기존 배치는 한 픽셀도 안 움직인다.
pub(crate) fn fit_sprite_box(cols: usize, rows: usize, cw: f32, ch: f32) -> (f32, f32) {
    let (aw, ah) = (CLAWD_COLS as f32 * cw, CLAWD_ROWS as f32 * ch);
    let s = ((cols as f32 * cw) / aw).min((rows as f32 * ch) / ah);
    (aw * s, ah * s)
}

pub(crate) fn find_clawd_banners(rows: &[Vec<GridCell>]) -> Vec<(isize, usize)> {
    // 세대별 (머리, 몸통, 발) — claude 는 배너 도트를 바꾼다. 2.1.23x 에서
    // 눈 요철이 생긴 새 아트로 갈렸는데 옛 글리프만 알던 동안 **새 배너가
    // 통째로 안 잡혀** 부팅 화면에 학생 테마가 안 붙었다(2026-08-20 거노
    // 스샷 + 격리 리그 실측: 컴팩트·박스형 웰컴 둘 다 같은 3행 아트).
    // 옛 버전으로 도는 pane 도 있을 수 있어 두 세대를 다 훑는다.
    const GENS: [(&[char], &[char], &[char]); 2] = [
        // 2.1.23x — 눈 달린 아트(2026-08-20 peek 실측).
        (
            &['▐', '▛', '█', '█', '█', '▛', '█'],
            &['▝', '▜', '█', '█', '█', '█', '█', '█', '▀'],
            // 발 행: 배너 좌단 기준 2칸 들여쓰기, 양옆은 공백.
            &['▝', '▝', ' ', '▝', '▝'],
        ),
        // ~2.1.212 — 민짜 아트.
        (
            &['▐', '▛', '█', '█', '█', '▜', '▌'],
            &['▝', '▜', '█', '█', '█', '█', '█', '▛', '▘'],
            &['▘', '▘', ' ', '▝', '▝'],
        ),
    ];
    let blank = |cell: &GridCell| matches!(cell.ch, ' ' | '\0');
    let matches_at = |row: &[GridCell], at: usize, pat: &[char]| {
        at + pat.len() <= row.len()
            && pat.iter().enumerate().all(|(i, &p)| row[at + i].ch == p)
    };
    let mut out = Vec::new();
    let n = rows.len();
    for (head, body, feet) in GENS {
        for r in 0..n {
            let row = &rows[r];
            let mut c = 0usize;
            while c + body.len() <= row.len() {
                if matches_at(row, c, body) {
                    if r == 0 {
                        // 몸통이 최상단 행 = 머리가 위로 잘림. 몸통 9글리프
                        // 단독으로도 일반 텍스트 오탐 여지가 사실상 없다.
                        out.push((-1, c));
                        c += body.len();
                        continue;
                    }
                    if matches_at(&rows[r - 1], c + 1, head) {
                        out.push((r as isize - 1, c));
                        c += body.len();
                        continue;
                    }
                }
                c += 1;
            }
        }
        // 위로 2행 잘림: 최상단에 발만 남은 경우. 발 글리프는 짧아 양옆
        // 공백(배너 폭 9칸 확보)까지 요구해 오탐을 줄인다.
        if let Some(row) = rows.first() {
            let mut p = 2usize;
            while p + feet.len() + 2 <= row.len() {
                if matches_at(row, p, feet)
                    && blank(&row[p - 2])
                    && blank(&row[p - 1])
                    && blank(&row[p + 5])
                    && blank(&row[p + 6])
                {
                    out.push((-2, p - 2));
                    p += feet.len();
                } else {
                    p += 1;
                }
            }
        }
        // 아래에서 진입: 최하단에 머리만 보이는 경우(몸통·발은 화면 밖).
        // 머리 7글리프 + 양옆 공백. 몸통행이 화면 안에 있으면 위 몸통 스캔이
        // 이미 잡으므로 마지막 행만 본다.
        if let Some(row) = rows.last().filter(|_| n >= 2) {
            let mut p = 1usize;
            while p + head.len() + 1 <= row.len() {
                if matches_at(row, p, head) && blank(&row[p - 1]) && blank(&row[p + 7]) {
                    out.push((n as isize - 1, p - 1));
                    p += head.len();
                } else {
                    p += 1;
                }
            }
        }
    }
    out
}

/// Clawd 배너 옆 타이틀의 "Claude Code" → pane 학생 이름 — 스냅샷 전용, 원본
/// 그리드 무손상(도트 교체와 같은 원칙). 배너 세로 범위에서 art 오른쪽의
/// "Claude Code" 글자 시퀀스를 찾아 한글 이름(와이드 글리프 + ' ' 스페이서)으로
/// 갈아끼우고, 뒤따르는 버전 텍스트를 이름 바로 뒤로 당긴다. 당겨서 남는 칸은
/// blank — 연속 공백 2칸 너머는 박스형 웰컴 변형의 오른쪽 테두리 영역이라
/// 건드리지 않는다(테두리 열이 밀리면 박스가 깨진다).
#[allow(clippy::too_many_arguments)]
pub(crate) fn replace_banner_title(
    rows: &mut [Vec<GridCell>],
    br: isize,
    bc: usize,
    lcols: usize,
    lrows: usize,
    title: &[char],
    name: &str,
    accent: Option<[u8; 4]>,
) {
    let r0 = br.max(0) as usize;
    let r1 = (br + lrows as isize).clamp(0, rows.len() as isize) as usize;
    for row in rows[r0..r1].iter_mut() {
        let start = bc + lcols;
        if start >= row.len() {
            continue;
        }
        let Some(tc) = (start..row.len().saturating_sub(title.len() - 1))
            .find(|&c| title.iter().enumerate().all(|(i, &p)| row[c + i].ch == p))
        else {
            continue;
        };
        // 이름 셀: 원 타이틀 스타일(bold 등) 승계, 색만 학생 accent 로 —
        // 테두리·스피너 텍스트와 같은 "이 pane 의 학생" 색 언어.
        let mut style = row[tc].clone();
        if let Some([r, g, b, _]) = accent {
            style.fg = kasa_bridge::screen::Color::Rgb(r, g, b);
        }
        let mut repl: Vec<GridCell> = Vec::with_capacity(title.len());
        for ch in name.chars() {
            let mut cell = style.clone();
            cell.ch = ch;
            repl.push(cell);
            // 와이드 글리프 다음 칸은 스페이서 — composed 경로 실측은 ' '.
            let mut sp = style.clone();
            sp.ch = ' ';
            repl.push(sp);
        }
        if repl.len() > title.len() {
            return; // 로스터 이름은 최대 3자(6칸) — 넘치면 원문 유지
        }
        let mut end = tc + title.len();
        let mut probe = end;
        while probe < row.len() {
            if matches!(row[probe].ch, ' ' | '\0') {
                if probe + 1 >= row.len() || matches!(row[probe + 1].ch, ' ' | '\0') {
                    break;
                }
            } else {
                end = probe + 1;
            }
            probe += 1;
        }
        let tail: Vec<GridCell> = row[tc + title.len()..end].to_vec();
        let mut w = tc;
        for cell in repl.into_iter().chain(tail) {
            row[w] = cell;
            w += 1;
        }
        for cell in row[w..end].iter_mut() {
            *cell = GridCell::blank();
        }
        return; // 타이틀은 배너당 한 줄
    }
    // 2.1.23x 박스형 웰컴: 타이틀이 아트 옆이 아니라 **상단 보더 줄**에 있다
    // ("╭─── Claude Code v2.1.237 ───…" — 2026-08-20 peek 실측). 아트 위
    // 최대 4행에서 첫 비공백이 ╭/┌ 인 줄만 골라 같은 치환을 하되,
    // - 버전 꼬리는 보더 대시가 다시 시작되기 전까지만 당기고,
    // - 당겨서 남는 칸은 blank 가 아니라 '─' 로 메운다 — 보더 줄의 빈칸은
    //   선이 끊긴 것으로 보이고, blank 를 쓰면 우측 ╮ 열이 밀려 박스가 깨진다.
    let is_box = |ch: char| (0x2500u32..=0x257F).contains(&(ch as u32));
    let lo = (br - 4).max(0) as usize;
    let hi = br.clamp(0, rows.len() as isize) as usize;
    for row in rows[lo..hi].iter_mut() {
        let Some(first) = row.iter().position(|c| !matches!(c.ch, ' ' | '\0')) else {
            continue;
        };
        if !matches!(row[first].ch, '╭' | '┌') {
            continue;
        }
        let Some(tc) = (first..row.len().saturating_sub(title.len().saturating_sub(1)))
            .find(|&c| title.iter().enumerate().all(|(i, &p)| row[c + i].ch == p))
        else {
            continue;
        };
        let mut style = row[tc].clone();
        if let Some([r, g, b, _]) = accent {
            style.fg = kasa_bridge::screen::Color::Rgb(r, g, b);
        }
        let mut repl: Vec<GridCell> = Vec::with_capacity(title.len());
        for ch in name.chars() {
            let mut cell = style.clone();
            cell.ch = ch;
            repl.push(cell);
            let mut sp = style.clone();
            sp.ch = ' ';
            repl.push(sp);
        }
        if repl.len() > title.len() {
            return; // 로스터 이름은 최대 3자(6칸) — 넘치면 원문 유지
        }
        // 버전 꼬리("v2.1.237")와 그 뒤 공백까지, 보더 대시가 다시 시작되기
        // 전 구간을 통째로 당긴다 — 공백을 남기면 「대시·공백·대시」로 선이
        // 끊긴 자리가 생긴다.
        let mut probe = tc + title.len();
        while probe < row.len() && !is_box(row[probe].ch) {
            probe += 1;
        }
        let dash = row
            .get(probe)
            .filter(|c| is_box(c.ch))
            .cloned()
            .unwrap_or_else(|| {
                let mut d = row[tc].clone();
                d.ch = '─';
                d
            });
        let tail: Vec<GridCell> = row[tc + title.len()..probe].to_vec();
        let mut w = tc;
        for cell in repl.into_iter().chain(tail) {
            row[w] = cell;
            w += 1;
        }
        for cell in row[w..probe].iter_mut() {
            *cell = dash.clone();
        }
        return; // 타이틀은 배너당 한 줄
    }
}

/// claude 웰컴 배너("Welcome back <user>!") → 배정 학생 인사말. Clawd 아트(=학생
/// 도트로 치환된 자리) 위쪽 박스 안의 "Welcome back " 행을 찾아, 사용자 이름을
/// 추출하고 그 행을 페르소나 인사말로 갈아끼운다(원 볼드 스타일 승계 + 학생 accent
/// 색, 박스 우측 보더 전까지 클립·초과 시 말줄임). async agents launcher 등 웰컴
/// 행이 없는 화면에선 자연 no-op(패시브). 스냅샷 전용 — 원본 그리드 무손상.
pub(crate) fn replace_welcome_greeting(
    rows: &mut [Vec<GridCell>],
    br: isize,
    name: &str,
    accent: Option<[u8; 4]>,
) {
    const PREFIX: [char; 13] =
        ['W', 'e', 'l', 'c', 'o', 'm', 'e', ' ', 'b', 'a', 'c', 'k', ' '];
    // "Welcome back" 은 아트(도트) 바로 위 박스 안, 중앙정렬. 아트 top(br) 기준
    // 위로 최대 4행만 본다. 아트가 위로 잘려(br<0) 웰컴 행이 화면 밖이면 자연 skip.
    let hi = br.clamp(0, rows.len() as isize) as usize;
    let lo = (br - 4).max(0) as usize;
    for r in lo..hi {
        // 불변 스캔: "Welcome back " 위치·이름·박스 우측 한계·원 스타일을 값으로.
        let (wc, excl, user, limit, mut style) = {
            let row = &rows[r];
            let Some(wc) = (0..row.len().saturating_sub(PREFIX.len()))
                .find(|&c| PREFIX.iter().enumerate().all(|(i, &p)| row[c + i].ch == p))
            else {
                continue;
            };
            let name_start = wc + PREFIX.len();
            let Some(excl_rel) = row[name_start..].iter().position(|c| c.ch == '!')
            else {
                continue;
            };
            let excl = name_start + excl_rel;
            if excl <= name_start {
                continue; // 빈 이름("Welcome back !") — 원문 유지
            }
            // 이름 추출: 와이드 글리프 바로 뒤 스페이서(' '/'\0') 셀 1칸을 흡수한다
            // — 단 실제 composed 는 스페이서가 있지만, 스페이서 없이 붙은 그리드
            // (테스트·비정상)에서도 다음 글자를 삼키지 않도록 "다음이 스페이서일
            // 때만" 건너뛴다.
            let mut user = String::new();
            let mut i = name_start;
            while i < excl {
                let ch = row[i].ch;
                if ch != '\0' {
                    user.push(ch);
                }
                i += 1;
                if crate::gpu::is_wide_char(ch)
                    && i < excl
                    && matches!(row[i].ch, ' ' | '\0')
                {
                    i += 1;
                }
            }
            // 우측 한계 = "!" 뒤 공백 구간이 끝나는 지점(=오른쪽 Tips 컬럼 또는 박스
            // 세로 보더의 시작). 2컬럼 배너는 같은 행 오른쪽에 Tips 가 있으므로 첫
            // 보더만 찾으면 그 사이 Tips 를 덮는다 — 다음 non-blank 전까지만 그린다.
            let limit = (excl + 1..row.len())
                .find(|&c| !matches!(row[c].ch, ' ' | '\0'))
                .unwrap_or(row.len());
            (wc, excl, user.trim().to_string(), limit, row[wc].clone())
        };
        let Some(greet) = crate::theme::character_welcome(name, &user) else {
            return; // 로스터 밖 이름 — 배너당 한 번, 원문 유지
        };
        if let Some([rr, gg, bb, _]) = accent {
            style.fg = kasa_bridge::screen::Color::Rgb(rr, gg, bb);
        }
        // 인사말 → 셀(한글은 글리프+스페이서 2칸).
        let mut cells: Vec<GridCell> = Vec::new();
        for ch in greet.chars() {
            let mut cell = style.clone();
            cell.ch = ch;
            cells.push(cell);
            if crate::gpu::is_wide_char(ch) {
                let mut sp = style.clone();
                sp.ch = ' ';
                cells.push(sp);
            }
        }
        let avail = limit.saturating_sub(wc);
        if avail == 0 {
            return;
        }
        if cells.len() > avail {
            cells.truncate(avail);
            if let Some(last) = cells.last_mut() {
                last.ch = '…';
            }
        }
        // 가변 쓰기: 인사말 그린 뒤 원문 잔여("!"까지)를 blank.
        {
            let row = &mut rows[r];
            for (k, cell) in cells.iter().enumerate() {
                row[wc + k] = cell.clone();
            }
            let written = wc + cells.len();
            let tail_end = excl.min(row.len().saturating_sub(1));
            for c in written..=tail_end {
                row[c] = GridCell::blank();
            }
        }
        // 보더 색칠은 여기서 하지 않는다 — 호출부(render.rs)가 인사말 성공
        // 여부와 무관하게 tint_welcome_box 를 부른다. 이 안에 뒀던 동안
        // 인사말 로스터에 없던 학생은 테두리까지 파랑으로 남았다(2026-08-20).
        return; // 웰컴 인사말은 배너당 한 줄
    }
}

/// 웰컴 배너 박스의 보더 문자(box-drawing U+2500~257F) fg 를 학생 accent 로 —
/// "Welcome back" 행 위 상단 코너(╭╮/┌┐)부터 아트 아래 하단 코너(╰╯/└┘)까지의
/// 박스에 한해서만 칠한다(다른 박스 오염 방지 — 코너로 이 배너 박스 범위 특정).
pub(crate) fn tint_welcome_box(
    rows: &mut [Vec<GridCell>],
    welcome_row: usize,
    art_bottom: usize,
    accent: [u8; 4],
) {
    let is_box = |ch: char| (0x2500u32..=0x257F).contains(&(ch as u32));
    let top = (0..welcome_row)
        .rev()
        .find(|&rr| rows[rr].iter().any(|c| matches!(c.ch, '╭' | '╮' | '┌' | '┐')));
    let bottom = (art_bottom.min(rows.len())..rows.len())
        .find(|&rr| rows[rr].iter().any(|c| matches!(c.ch, '╰' | '╯' | '└' | '┘')));
    let (Some(top), Some(bottom)) = (top, bottom) else {
        return;
    };
    let [r, g, b, _] = accent;
    let col = kasa_bridge::screen::Color::Rgb(r, g, b);
    for row in rows[top..=bottom].iter_mut() {
        for cell in row.iter_mut() {
            if is_box(cell.ch) {
                cell.fg = col.clone();
            }
        }
    }
}

/// 화면 전체를 한 줄로 접는다 — 공백류·U+0000 을 한 칸으로 눌러 wrap 에 무관하게
/// 매칭하기 위한 정규화. 좁은 창에선 한 문구가 여러 셀 행으로 갈리고 사이에 행끝
/// 패딩이 껴 직접 매칭이 깨진다(거노: 특정 창 크기에서만 사각형 잔상 재발).
fn squash_screen(rows: &[Vec<GridCell>]) -> String {
    let full: String = rows.iter().flat_map(|r| r.iter().map(|c| c.ch)).collect();
    full.split(|c: char| c.is_whitespace() || c == '\0')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// 조각들이 이 순서로, 각 조각이 직전 조각 끝에서 `gap` 자 이내에 오는지.
///
/// 화면 전체에서 낱말을 따로 `contains` 하면 대화 본문에 우연히 흩어져 있어도
/// 참이 된다. "한 줄에 나란히"가 진짜 조건인데 행 단위로 보면 wrap 에 깨지므로,
/// squash 한 문자열에서의 **근접**으로 근사한다. 첫 조각이 여러 번 나올 수 있어
/// (본문에 한 번, 통계줄에 한 번) 모든 등장 위치에서 시도한다.
fn seq_near(hay: &str, parts: &[&str], gap: usize) -> bool {
    let Some((first, rest)) = parts.split_first() else {
        return true;
    };
    let mut from = 0usize;
    while let Some(pos) = hay[from..].find(first) {
        let mut at = from + pos + first.len();
        // 다음 후보 시작점 — 매칭 길이만큼 건너뛴다(UTF-8 경계 보장).
        from = at;
        if rest.iter().all(|p| match hay[at..].find(p) {
            Some(off) if off <= gap => {
                at += off + p.len();
                true
            }
            _ => false,
        }) {
            return true;
        }
    }
    false
}

/// claude agents 목록 화면(FleetView)인지 화면 텍스트로 감지. argv(`is_claude_agents`)는
/// `claude agents` **명령**만 잡고, 세션 안에서 "← for agents"로 여는 목록 뷰는 같은
/// 프로세스라 argv 가 안 바뀌어 못 잡는다(거노: agents view 로고 안 뜸).
///
/// 신호는 목록 상단 통계줄 "N awaiting input · N working · N completed" 다. 세 조각이
/// **구분자로 붙어 한 줄**을 이루는 것이 이 화면 고유고, 조건부 렌더가 아니라 0 이어도
/// 생략되지 않으므로 목록이 비어 있어도 잡힌다(실측 2.1.237).
///
/// ⚠️ 예전엔 `contains("awaiting input") && contains("completed")` 였다. 그건 화면
/// **어디든** 두 낱말이 있으면 참이라, 그 두 단어가 스치는 대화 본문(에이전트 상태를
/// 설명하는 문서 등 — claude 번들 안에도 그런 문구가 여럿 있다)에서 오탐한다. 당시엔
/// 호출부의 `!has_profile_slot` 이 그 오탐을 가려 주고 있었을 뿐이고, 그 안전판을 떼려면
/// (세션 안에서 연 목록은 statusline 이 남아 그 조건에 걸려 판정 자체가 꺼졌다)
/// 판독이 먼저 홀로 서야 한다. 그래서 낱말 존재가 아니라 **인접 순서**로 본다.
pub(crate) fn screen_is_agents_list(rows: &[Vec<GridCell>]) -> bool {
    let squashed = squash_screen(rows);
    // 구분자와 개수를 사이에 두므로 gap 은 " · 9999 " 를 덮을 만큼만.
    seq_near(&squashed, &["awaiting input", "working", "completed"], 16)
        // 통계줄이 아직 안 그려진 첫 프레임 폴백 — 목록 자리 빈 상태 안내.
        || squashed.contains("Nothing running in the background.")
}

/// claude `--resume` 세션 피커 화면인지 감지. "Resume session (N of M)" 헤더가
/// 뜨는 시스템 UI라, 학생 pane 후처리(prompt box accent·세션 제목 인레이)를 여기서
/// 오발동하면 안 된다 — Search 박스(`╭─╮ ⌕ Search… ╰─╯`)가 pane 입력박스로 오인돼
/// 빈 초록 사각형이 그려졌다(거노). 일반 대화엔 statusline(U+FFFC)이 있어 호출부에서
/// !has_profile_slot 로 이미 걸러진다.
pub(crate) fn screen_is_resume_picker(rows: &[Vec<GridCell>]) -> bool {
    // "Resume session (N of M)" 헤더가 피커 고유 — 단순 "Resume session" 은
    // 대화 본문에 우연히 나올 수 있어 여는 괄호까지 확인한다. 피커도 맨 아래
    // statusline(U+FFFC) 한 줄이 남아 has_profile_slot 으로는 못 거른다
    // (거노: Search 아래 핑크 사각형 잔재 — accent 후처리 오발동).
    //
    // 좁은 창에선 "Resume session" 과 "(N of M)" 이 다른 셀 행으로 wrap 되며
    // 사이에 행끝 패딩(스페이스·U+0000)이 껴 "Resume session (" 직접 매칭이
    // 깨진다(거노: 특정 창 크기에서만 사각형 잔상 재발) — squash_screen 이 그걸 접는다.
    squash_screen(rows).contains("Resume session (")
}

/// AskUserQuestion picker 감지 — `❯ 1. …` 옵션 목록 + 하단 힌트 박스가 학생
/// 입력박스로 오인돼 accent 사각형이 남던 화면(거노: "question 이나 resume").
/// 고유 시그니처: 항상 마지막 옵션인 "Chat about this" + 하단 네비 힌트
/// ("Esc to cancel" 또는 "Enter to select"). resume 피커엔 없는 조합이라
/// 대화 본문 우연 등장을 힌트 AND 로 한 번 더 거른다. resume 와 같은 squash
/// 정규화로 wrap·다중 공백에 강건하게 매칭한다.
pub(crate) fn screen_is_ask_picker(rows: &[Vec<GridCell>]) -> bool {
    let squashed = squash_screen(rows);
    squashed.contains("Chat about this")
        && (squashed.contains("Esc to cancel") || squashed.contains("Enter to select"))
}

/// 한 창이 이번 프레임에 얹을 학생 스프라이트 자리들.
///
/// 셀을 훑어 모으는 쪽(메인 그리드 / 별도창)과 그리는 쪽을 가르는 경계다 —
/// 좌표계는 창마다 다르지만 **이미지 키와 업로드 규칙은 한 벌**이어야 한다.
/// 두 벌이 되면 한쪽만 프레임을 올리거나 캐시 키가 갈려, 같은 학생이 창마다
/// 다른 그림으로 뜬다.
#[derive(Default)]
pub(crate) struct StudentOverlays {
    /// Clawd 배너 자리 — `(slug, rect, (클립 위, 클립 아래))`. 스크롤로 잘리게 클립.
    pub(crate) banner: Vec<(&'static str, (f32, f32, f32, f32), (f32, f32))>,
    /// working 스피너 자리 — 제자리 걸음(walk).
    pub(crate) spinner: Vec<(&'static str, (f32, f32, f32, f32))>,
    /// 승인 대기 — 한 팔 인사(wave).
    pub(crate) waiting: Vec<(&'static str, (f32, f32, f32, f32))>,
    /// 입력박스 위 standing — `(slug, motion, rect)`.
    pub(crate) standing: Vec<(&'static str, &'static str, (f32, f32, f32, f32))>,
    /// statusline 프사(bust) 자리.
    pub(crate) profile: Vec<(&'static str, (f32, f32, f32, f32))>,
}

impl StudentOverlays {
    /// 다음 프레임을 스스로 불러야 하나 — 움직이는 스프라이트가 하나라도 있으면.
    /// 프사만 있는 창은 정적 1프레임이라 깨울 필요가 없다.
    pub(crate) fn animating(&self) -> bool {
        !self.banner.is_empty()
            || !self.spinner.is_empty()
            || !self.waiting.is_empty()
            || !self.standing.is_empty()
    }
}

/// 모은 자리에 스프라이트를 얹는다. `anim_ms` 는 창이 공유하는 애니 시계
/// (`version_anim_start` 경과). 프레임 업로드는 캐릭터×모션당 1회고, 창마다
/// GpuRenderer 가 달라 창별로 한 번씩 올라간다(같은 키, 같은 픽셀).
///
/// 전부 `queue_image_above` — 셀 **위** 패스다. 아래 패스로 내리면 statusline
/// 테두리 글리프가 얼굴을 가로지르고 blank 처리한 자리에 셀 배경이 덮인다.
pub(crate) fn paint_student_overlays(
    g: &mut gpu::GpuRenderer,
    slots: &StudentOverlays,
    anim_ms: u64,
) {
    let anim_idx = (anim_ms / STUDENT_ANIM_FRAME_MS) as usize % STUDENT_IDLE_FRAMES;
    let walk_idx = (anim_ms as f32 / STUDENT_WALK_FRAME_MS) as usize % STUDENT_WALK_FRAMES;
    let ensure_anim = |g: &mut gpu::GpuRenderer, slug: &str, motion: &str| {
        let pfx = sprite_key_prefix(motion);
        if !g.has_image(&format!("student:{slug}:{pfx}0")) {
            if let Some(frames) = student_sprite_frames(slug, motion) {
                for (i, (rgba, w, h)) in frames.iter().enumerate() {
                    g.upload_image(&format!("student:{slug}:{pfx}{i}"), rgba, *w, *h);
                }
            }
        }
    };
    for (slug, (bx, by, bw, bh), (clip_y0, clip_y1)) in &slots.banner {
        ensure_anim(g, slug, "idle");
        g.push_clip(*bx, *clip_y0, *bw, *clip_y1 - *clip_y0);
        g.queue_image(&format!("student:{slug}:f{anim_idx}"), *bx, *by, *bw, *bh, 1.0, 0.0, 0.0);
        g.pop_clip();
    }
    for (slug, (bx, by, bw, bh)) in &slots.spinner {
        ensure_anim(g, slug, "walk");
        g.queue_image_above(&format!("student:{slug}:walk{walk_idx}"), *bx, *by, *bw, *bh);
    }
    for (slug, (bx, by, bw, bh)) in &slots.waiting {
        ensure_anim(g, slug, "wave");
        g.queue_image_above(&format!("student:{slug}:wave{anim_idx}"), *bx, *by, *bw, *bh);
    }
    for (slug, motion, (bx, by, bw, bh)) in &slots.standing {
        ensure_anim(g, slug, motion);
        let pfx = sprite_key_prefix(motion);
        g.queue_image_above(&format!("student:{slug}:{pfx}{anim_idx}"), *bx, *by, *bw, *bh);
    }
    for (slug, (bx, by, bw, bh)) in &slots.profile {
        let key = format!("student:{slug}:profile");
        if !g.has_image(&key) {
            if let Some((rgba, w, h)) = student_profile_rgba(slug) {
                g.upload_image(&key, &rgba, w, h);
            }
        }
        g.queue_image_above(&key, *bx, *by, *bw, *bh);
    }
}



/// statusline 프사 자리표시자(U+FFFC 연속 셀) 위치 — `(행, 시작열, 칸수)`.
///
/// statusline.py 가 학생 이름 대신 이 문자를 내보낸다. **아래→위 스캔**인 것이
/// 중요하다: statusline 은 늘 화면 바닥 쪽인데, 대화 출력에 U+FFFC 원문이 섞이면
/// (statusline 디버그 출력 등) 위쪽 행이 앵커를 가로채 얼굴이 엉뚱한 데 붙는다
/// (실사고). 메인 창과 별도창이 같은 자리를 찍도록 한 곳에 둔다.
pub(crate) fn find_statusline_face(rows: &[Vec<GridCell>]) -> Option<(usize, usize, usize)> {
    rows.iter().enumerate().rev().find_map(|(r, row)| {
        row.iter().position(|c| c.ch == '\u{fffc}').map(|c0| {
            let n = row[c0..].iter().take_while(|c| c.ch == '\u{fffc}').count();
            (r, c0, n)
        })
    })
}

/// 입력박스 위 standing 학생의 앵커 — `(앵커 행, 학생 왼쪽 열)`.
///
/// `face_row` 는 statusline 행. 그 바로 위가 아래 테두리(순수 '─')면, 거기서
/// 위로 첫 rule 행이 입력박스 윗 테두리다 — ❯ 영역이 여러 줄로 자라도 스캔이라
/// 따라간다. 학생은 그 윗 테두리 줄에 발이 닿게 서고, 칩(effort·context 경고)이
/// 떠 있으면 그 왼쪽으로 비켜선다.
///
/// 윗 테두리는 `/rename` 세션명이 "── 학생 ──" 로 박힐 수 있어 짧은 텍스트 섬을
/// 인정한다(max_label 24) — 순수 rule 만 보면 이름 지은 세션에서 standing 이
/// 통째로 사라진다(거노 실사고). 아래 테두리는 항상 순수 '─'(0).
/// agy 입력창 위 standing 앵커.
///
/// agy 화면 아래는 `[윗보더][> 입력][아래보더][? for shortcuts …]` 로, claude 와
/// 같은 모양이다. 그래서 앵커 계산은 `find_standing_anchor` 를 그대로 쓰고, 그것이
/// 요구하는 「statusline 행」만 여기서 찾아 준다 — 맨 아래 보더 바로 다음 행.
///
/// 마커로 ASCII `>` 를 인정하는 것은 **여기뿐**이다. 공용 `prompt_box` 는 인용문·diff
/// 를 입력창으로 오인한 전례가 있어 `>` 를 뺐고, 그 판정은 건드리지 않는다.
pub(crate) fn find_agy_standing_anchor(rows: &[Vec<GridCell>], cols: usize) -> Option<(usize, f32)> {
    let border = |r: &Vec<GridCell>| {
        let dash = r.iter().filter(|c| c.ch == '─').count();
        let label = r.iter().filter(|c| !matches!(c.ch, '─' | ' ' | '\0')).count();
        dash >= 8 && label == 0
    };
    let bottom = rows.iter().rposition(border)?;
    let top = rows[..bottom].iter().rposition(border)?;
    let marked = rows[top + 1..bottom].iter().any(|r| {
        r.iter().find(|c| !matches!(c.ch, ' ' | '\0')).is_some_and(|c| c.ch == '>')
    });
    if !marked || bottom + 1 >= rows.len() {
        return None;
    }
    find_standing_anchor(rows, bottom + 1, cols)
}

pub(crate) fn find_standing_anchor(
    rows: &[Vec<GridCell>],
    face_row: usize,
    cols: usize,
) -> Option<(usize, f32)> {
    // 다수 판정을 **격자 전체 폭이 아니라 내용 폭**(마지막 non-blank 까지)으로 한다.
    // 전체 폭으로 재면 pane 이 claude 의 입력박스보다 넓을 때 테두리가 소수가 되어
    // `is_rule` 이 거짓이 되고, standing 이 통째로 사라진다 — 155칸 pane 에 60칸
    // 테두리로 실측(dash=60/155 → anchor=None). 내용 폭 기준이면 박스가 pane 보다
    // 좁아도 성립한다. 대신 짧은 구분선 조각을 테두리로 오인하지 않게 최소 길이를 둔다.
    let is_rule = |row: &[GridCell], max_label: usize| {
        let mut dashes = 0usize;
        let mut label = 0usize;
        let mut content_w = 0usize;
        for (i, c) in row.iter().enumerate() {
            match c.ch {
                '─' => {
                    dashes += 1;
                    content_w = i + 1;
                }
                ' ' | '\0' => {}
                _ => {
                    label += 1;
                    content_w = i + 1;
                    if label > max_label {
                        return false;
                    }
                }
            }
        }
        dashes >= 8 && dashes > content_w / 2
    };
    if face_row < 4 || !is_rule(&rows[face_row - 1], 0) {
        return None;
    }
    let tr = (face_row.saturating_sub(16)..face_row - 1)
        .rev()
        .find(|&r| is_rule(&rows[r], 24))
        .filter(|&tr| tr >= 1)?;
    let anchor = tr - 1;
    Some((anchor, stand_left_col(rows, anchor, cols)?))
}

/// 앵커 행이 정해진 뒤의 가로 자리 — 그 행에 이미 뭐가 떠 있으면(effort 칩·
/// context 경고) 그 왼쪽으로 비켜선다. 하네스마다 세로 앵커를 찾는 법은 다르지만
/// 가로 규칙은 같아서 여기 한 곳에만 둔다.
pub(crate) fn stand_left_col(rows: &[Vec<GridCell>], anchor: usize, cols: usize) -> Option<f32> {
    let first = rows[anchor].iter().position(|c| !matches!(c.ch, ' ' | '\0'));
    let right_c = match first {
        Some(f) => f as f32 - 1.5,
        None => cols as f32 - 1.0,
    };
    let left_c = right_c - STAND_CELLS;
    (left_c > 2.0).then_some(left_c)
}

/// 테두리 없는 입력창(`PromptBox::Filled`, codex) 위 standing 앵커.
///
/// claude 는 statusline 자리표시자(U+FFFC)에서 아래 테두리를 짚고 위로 스캔하지만
/// **codex 엔 자리표시자를 심을 데가 없다** — `[tui] status_line` 은 정해진 세그먼트
/// 이름 배열이고 모르는 항목은 `⚠ Ignored invalid status line item` 으로 버려진다
/// (0.146.0 실측). 커맨드 훅도 없다. 대신 입력행 자체는 `prompt_box` 가 배경 채움으로
/// 이미 정확히 집어내므로 그 바로 윗행을 앵커로 쓴다 — 테두리 스캔이 통째로 없어
/// claude 쪽이 밟았던 함정(dash 비율 오판)에서 자유롭다.
pub(crate) fn find_filled_standing_anchor(
    rows: &[Vec<GridCell>],
    cols: usize,
) -> Option<(usize, f32)> {
    let PromptBox::Filled { rows: r } = prompt_box(rows)? else {
        return None;
    };
    let anchor = r.start.checked_sub(1)?;
    Some((anchor, stand_left_col(rows, anchor, cols)?))
}

/// standing 학생이 차지하는 가로 칸수 — 앵커 계산과 그리기가 같은 값을 써야 한다.
pub(crate) const STAND_CELLS: f32 = 4.0;

/// Claude Code 라이브 스피너("✻ Verbing…" 별 dingbat, 또는 braille) 위치 감지 —
/// `rows_show_working`(input.rs)과 같은 신호를 행·열 좌표로 돌려준다. 마지막
/// non-blank 30행, 행 앞머리(col<8)만 본다(본문 인용 별표 오탐 방지). 스피너
/// 셀은 blank 처리하고 그 자리에 학생 working 도트를 얹는 용도.
pub(crate) fn find_claude_spinner(rows: &[Vec<GridCell>]) -> Option<(usize, usize)> {
    let last = rows
        .iter()
        .rposition(|row| row.iter().any(|cell| !matches!(cell.ch, ' ' | '\0')))?;
    // todo 트리가 뜨면 스피너 행이 statusline(=last)에서 멀어진다: todo ~7행 +
    // 입력박스(테두리·❯·테두리) ~4행이 사이에 껴 10행 창 밖으로 밀려나 walk
    // 도트가 사라졌다(거노). 앞머리 글리프(별/점/점자 col<8) + '…'/"esc to
    // interrupt" 라는 강한 시그니처라 30행으로 넓혀도 본문 오탐은 사실상 없다.
    let start = (last + 1).saturating_sub(30);
    // 스피너 애니메이션은 별(U+2720~274F)·점자(U+2800~28FF)·가운뎃점(·) 등
    // 여러 글리프를 순환한다. 특정 글리프만 잡으면 점 프레임에서 감지가 끊겨
    // 학생 도트가 프레임마다 깜빡인다 → `rows_show_working` 과 같은 문맥 기준
    // (별+…/점자/"esc to interrupt")으로 working 행을 찾고, 그 행 첫 글리프
    // (=스피너 자리) col 을 돌려준다. 스피너가 어떤 프레임이든 위치가 고정된다.
    for r in (start..=last).rev() {
        if let Some(c) = spinner_row_col(&rows[r]).or_else(|| spinner_tip_rescue(rows, r)) {
            if spinner_is_live(rows, r) {
                return Some((r, c));
            }
        }
    }
    None
}

/// `spinner_row_col` 의 경과시간-괄호 요구에 떨어진 행을, 바로 아래의 `Tip:` 행이
/// 구제한다. 부팅·재개 직후의 스피너는 `✻ Computing…` 뒤에 괄호가 아예 없이 뜨고
/// 그 아래 `⎿ Tip: Press …` 만 깔린다(2026-08-15 스샷 실측 — 이 상태에서 학생이
/// 스피너에 안 붙었다). Tip 행은 살아 있는 스피너 UI 에만 붙으므로, 별+줄임표
/// 행에 괄호가 없어도 아래 첫 non-blank 행이 Tip 이면 스피너로 본다.
///
/// 한국어 본문 오탐(spinner_row_col 주석의 ②를 세운 이유)이 되살아나지 않는 근거:
/// 본문 줄이 별·점으로 시작하고 줄임표로 끝나면서 **다음 줄이 `⎿ Tip:`** 인 조합은
/// 인용뿐이고, 인용이면 그 아래 어딘가의 대화 마커를 `spinner_is_live` 가 잡는다.
pub(crate) fn spinner_tip_rescue(rows: &[Vec<GridCell>], r: usize) -> Option<usize> {
    let row = &rows[r];
    let first = row.iter().position(|c| !matches!(c.ch, ' ' | '\0'))?;
    if first >= 8 {
        return None;
    }
    let g = row[first].ch as u32;
    if !((0x2720..=0x274F).contains(&g) || g == '·' as u32) {
        return None;
    }
    let rest: String = row[first + 1..]
        .iter()
        .map(|cell| if cell.ch == '\0' { ' ' } else { cell.ch })
        .collect();
    if !rest.contains('…') {
        return None;
    }
    // 스피너와 Tip 사이에 빈 행이 하나 끼는 변형까지만 본다 — 더 멀면 남의 줄이다.
    for below in rows.iter().skip(r + 1).take(2) {
        let Some(fi) = below.iter().position(|c| !matches!(c.ch, ' ' | '\0')) else {
            continue;
        };
        if !matches!(below[fi].ch, '⎿' | '└' | '╰') {
            return None;
        }
        let t: String = below[fi + 1..]
            .iter()
            .map(|cell| if cell.ch == '\0' { ' ' } else { cell.ch })
            .collect();
        return t.contains("Tip:").then_some(first);
    }
    None
}

/// 괄호도 esc 힌트도 Tip 도 아직 없는 「턴 시작 첫 프레임」 스피너 후보 —
/// `✢ Transmuting…` 별+줄임표뿐인 행. 실측(2026-08-15, 0.3s 간격 채집): 매 턴
/// **첫 ~3초**가 이 모양이고 경과시간 괄호는 3초께에야 붙는다. 그동안 본판정이
/// 거부해 학생이 매 턴 3초 늦게 붙었다(거노 「스피너 인식 바로 안 되나봐」).
///
/// 글자만으로는 이 모양을 인용문과 못 가른다(그 오탐을 막으려고 괄호 요구를
/// 세웠던 것). 그래서 이 함수는 **후보만** 대고, 확정은 App 쪽 프로브가
/// **글리프가 움직이는지**로 한다 — 진짜 스피너는 별 프레임(✢✶✽✻✳·)이 계속
/// 바뀌고 인용문은 멈춰 있다. (행, 열, 글리프)를 돌려준다.
pub(crate) fn unconfirmed_spinner_row(rows: &[Vec<GridCell>]) -> Option<(usize, usize, char)> {
    let last = rows
        .iter()
        .rposition(|row| row.iter().any(|cell| !matches!(cell.ch, ' ' | '\0')))?;
    let start = (last + 1).saturating_sub(30);
    for r in (start..=last).rev() {
        let row = &rows[r];
        let Some(first) = row.iter().position(|c| !matches!(c.ch, ' ' | '\0')) else {
            continue;
        };
        if first >= 8 {
            continue;
        }
        let g = row[first].ch;
        if !((0x2720..=0x274F).contains(&(g as u32)) || g == '·') {
            continue;
        }
        // 본판정이 잡는 행이면 프로브가 낄 자리가 아니다 — find_claude_spinner 몫.
        if spinner_row_col(row).is_some() || spinner_tip_rescue(rows, r).is_some() {
            return None;
        }
        let rest: String = row[first + 1..]
            .iter()
            .map(|cell| if cell.ch == '\0' { ' ' } else { cell.ch })
            .collect();
        if !rest.contains('…') {
            continue;
        }
        // 위치 검증은 본판정과 같은 자로 — 아래에 대화 마커가 있으면 옛 본문이다.
        return spinner_is_live(rows, r).then_some((r, first, g));
    }
    None
}

/// 그 스피너 행이 **지금 도는 것**인가, 아니면 스크롤백에 굳은 옛 텍스트인가.
///
/// 글자만으로는 못 가른다 — 답변이 스피너 형태를 **인용**하면 진짜와 한 글자도
/// 다르지 않다. 실제로 스피너 감지를 설명하는 답변 자체가 잡혀, 턴이 끝난 뒤에도
/// 그 인용줄 위에서 학생이 계속 걸었다(거노 2026-08-13 지적: "저기서 왜 걷고있어").
///
/// 가르는 축은 글자가 아니라 **위치**다. claude 화면에서 진짜 스피너 아래에는
/// 입력박스와 statusline 뿐이고, 대화 마커(`⏺` 응답 · `⎿` 도구 출력)는 언제나
/// 스피너 **위**에 쌓인다. 그러니 후보 행 아래에 마커가 하나라도 있으면 그건
/// 이미 지나간 본문이다. 실측(2026-08-13, pane 3개): 인용줄은 입력박스에서 16행
/// 위이고 그 사이에 `⎿` 가 있었으며, 살아 있는 스피너는 3행 위에 마커 없이 있었다.
///
/// 거리(N행 이내)로 자르지 않은 이유: todo 트리가 스피너와 입력박스 사이에 끼면
/// 그 거리가 통째로 흔들려, 넉넉히 잡으면 인용줄이 들어오고 좁게 잡으면 진짜가
/// 빠진다. 마커 유무는 todo 가 몇 행이든 영향을 안 받는다.
pub(crate) fn spinner_is_live(rows: &[Vec<GridCell>], r: usize) -> bool {
    !rows[r + 1..].iter().any(|row| {
        let Some(fi) = row.iter().position(|c| !matches!(c.ch, ' ' | '\0')) else {
            return false;
        };
        match row[fi].ch {
            '⏺' => true,
            // compact 화면은 알림 아래에 `⎿ Tip: …` 를 깐다(2026-08-13 스샷 실측).
            // 대화 마커와 같은 글리프지만 본문이 아니라 스피너 UI 의 일부다 — 이걸
            // 마커로 세면 compact 알림이 스스로를 죽인다. Tip 행만 예외였는데,
            // 태스크 목록 위젯도 스피너 바로 아래에 `⎿  ◻ 항목` 으로 뜬다는 게
            // 실측됐다(2026-08-15 peek: `✽ Ideating…` 아래 `⎿  ◻ 설정 다국어`).
            // 이 행을 마커로 세면 태스크를 쓰는 working pane 전부에서 스피너가
            // 죽어 학생이 걷다 말고 입력창 위에 서 버린다(거노 신고). 체크박스
            // 글리프로 시작하는 ⎿ 행은 위젯이다 — 도구 출력 인용(`⎿ Read 50
            // lines…`)은 일반 글자로 시작해 안 걸린다.
            '⎿' => {
                let text: String = row[fi + 1..]
                    .iter()
                    .map(|c| if c.ch == '\0' { ' ' } else { c.ch })
                    .collect();
                let widget = text.trim_start().starts_with([
                    '◻', '◼', '□', '■', '☐', '☑', '✔', '✘', '✖', '◉', '○', '●',
                ]);
                !(text.contains("Tip:") || widget)
            }
            _ => false,
        }
    })
}

/// 한 행이 claude 의 working 스피너 행인가 — 맞으면 스피너 글리프의 col.
/// `find_claude_spinner`(도트 위치)와 `rows_show_working`(busy 판정)이 **같은
/// 판정**을 써야 한다. 한쪽만 맞으면 도트가 도는데 헤더는 안 돌거나 그 반대다.
///
/// 앞머리 글리프(별·점자·가운뎃점)가 어딘가 있고 행에 줄임표가 있으면 스피너로
/// 보던 것이 한국어 본문을 통째로 걸었다 — 가운뎃점과 줄임표는 한국어에서 흔한
/// 문장부호라 「간·창별 막대) → … Manage Accounts…」 같은 평범한 답변 줄이 잡혀
/// 학생이 본문 위를 걸어다녔다(2026-08-12 지적). 그래서 두 가지를 못 박는다:
///   ① 글리프가 그 행의 **첫** non-blank 여야 한다(본문 중간의 점은 무시).
///   ② 줄임표 **뒤**에 `(3m 19s · ↓ 14.2k tokens)` 같은 경과시간 괄호가 와야 한다.
///
/// ②를 처음엔 "글리프와 줄임표 사이가 ASCII"로 뒀는데 그게 **진짜 스피너를 통째로
/// 죽였다** — 동사가 영어라는 전제가 틀렸다. claude 는 한국어로도 찍는다:
/// `· claude 테마 자동 연동 구현 중… (3m 19s · ↓ 14.2k tokens)`. 그래서 working 인
/// pane 의 학생이 안 걸었다(거노 2026-08-13 지적). 언어에 안 묶이는 표식은 동사가
/// 아니라 뒤에 붙는 경과시간이다.
pub(crate) fn spinner_row_col(row: &[GridCell]) -> Option<usize> {
    let first = row.iter().position(|c| !matches!(c.ch, ' ' | '\0'))?;
    if first >= 8 {
        return None;
    }
    let rest: String = row[first + 1..]
        .iter()
        .map(|cell| if cell.ch == '\0' { ' ' } else { cell.ch })
        .collect();
    // 옛 claude code 는 스피너 행에 이 힌트를 붙였다. 있으면 그것만으로 확정.
    if rest.contains("esc to interrupt") {
        return Some(first);
    }
    let g = row[first].ch;
    if (0x2800..=0x28FF).contains(&(g as u32)) {
        return Some(first);
    }
    // 최근 claude code(2.1.207 실측)는 힌트 없이 "· Verbing… (3m · ↓ 9k tokens)"
    // 만 찍는다 — 점(·) 프레임도 별과 같이 인정해야 감지가 프레임마다 끊기지 않는다.
    if !((0x2720..=0x274F).contains(&(g as u32)) || g == '·') {
        return None;
    }
    // compact 알림에는 경과시간 괄호가 아예 없는 변형이 있다 — `· Compacting
    // conversation…` 뒤에 아무것도 안 붙는다(2026-08-13 스샷 실측). 이 행이 그
    // 화면의 유일한 스피너 행이라, 아래 괄호 요구까지 내려보내면 compact 중인
    // pane 전체가 idle 로 읽혀 바도 완료 판정도 전부 죽는다.
    if rest.contains("ompacting") {
        return Some(first);
    }
    // 줄임표 **뒤**의 `(3m 19s · ↓ 14.2k tokens)` 가 스피너의 진짜 표식이다. 숫자로
    // 시작하고 초 단위가 들어 있는 괄호를 요구한다.
    let tail = rest.split_once('…')?.1;
    let inside = tail.split_once('(')?.1;
    let head = inside.split_once(')').map_or(inside, |(h, _)| h);
    // 경과시간은 괄호 **어딘가에** 있으면 된다 — 맨 앞이어야 한다고 못 박았더니
    // 토큰이 먼저 오는 변종 `(↓ 1.2k tokens · 3s)` 을 통째로 놓쳤다. 턴이 막
    // 시작해 경과시간이 아직 안 붙은 프레임이 이 꼴로 뜨는데, 그게 거노가 말한
    // 「바로 안 붙을 때도 있어」(2026-08-20)의 한 갈래다. 앞머리 글리프가 그 행의
    // **첫** non-blank(col<8)여야 한다는 관문은 그대로라 본문 오탐은 안 늘어난다.
    has_elapsed(head).then_some(first)
}

/// `3s` · `8m 18s` 같은 경과시간 토막이 들어 있나. 숫자 바로 뒤에 `s`/`m` 이
/// 붙은 조각을 찾는다 — `27.4k tokens` 의 `tokens` 처럼 **글자 뒤에 오는 s** 는
/// 세지 않는다(그걸 세면 토큰만 있는 괄호가 시간으로 읽혀 관문이 무의미해진다).
fn has_elapsed(head: &str) -> bool {
    let b = head.as_bytes();
    b.iter().enumerate().any(|(i, c)| {
        matches!(c, b's' | b'm') && i > 0 && b[i - 1].is_ascii_digit()
    })
}

/// 승인 대기 도트가 설 자리 — 질문 헤더 행("Do you want to proceed", 없으면 첫
/// ❯ 행, 그것도 없으면 마지막 non-blank 행)과 그 행의 텍스트 끝 col. pane
/// 우상단 고정은 윈도우 우상단의 collab 승인 토스트와 겹쳐서(거부 버튼 가림)
/// 프롬프트 자체에 앵커한다. 스캔 범위는 `rows_show_approval_prompt` 와 동일.
pub(crate) fn approval_anchor(rows: &[Vec<GridCell>]) -> Option<(usize, usize)> {
    let last = rows
        .iter()
        .rposition(|row| row.iter().any(|cell| !matches!(cell.ch, ' ' | '\0')))?;
    let start = (last + 1).saturating_sub(14);
    let end_col = |r: usize| {
        rows[r]
            .iter()
            .rposition(|cell| !matches!(cell.ch, ' ' | '\0'))
            .unwrap_or(0)
    };
    let mut chevron: Option<usize> = None;
    for r in start..=last {
        let line: String = rows[r]
            .iter()
            .map(|cell| if cell.ch == '\0' { ' ' } else { cell.ch })
            .collect();
        if line.to_lowercase().contains("do you want to proceed") {
            return Some((r, end_col(r)));
        }
        if chevron.is_none() && line.contains('❯') {
            chevron = Some(r);
        }
    }
    let r = chevron.unwrap_or(last);
    Some((r, end_col(r)))
}

/// Truncate a label to a *pixel* budget using the shaper's real metrics.
/// `clip_display_width` approximates with a fixed px-per-column constant, which
/// only holds for the CJK/ASCII mix it was tuned against — an all-ASCII title
/// measures far narrower than its column count implies, and an all-Hangul one
/// wider. Where a label sits next to another element, measure instead of guess.
pub(crate) fn clip_px(
    g: &mut gpu::GpuRenderer,
    s: &str,
    font_size: f32,
    bold: bool,
    budget: f32,
) -> String {
    if budget <= 0.0 {
        return String::new();
    }
    if g.measure_chrome_text(s, font_size, bold) <= budget {
        return s.to_string();
    }
    let mut out = s.to_string();
    while out.chars().count() > 1 {
        out.pop();
        if g.measure_chrome_text(&format!("{out}…"), font_size, bold) <= budget {
            break;
        }
    }
    out.push('…');
    out
}

/// Truncate a label to a *display-width* budget (CJK glyphs are double-width)
/// with a trailing ellipsis, so long Hangul/CJK titles never bleed past the
/// tab into neighboring chrome. Shared by the side strip and the top tab bar.
pub(crate) fn clip_display_width(s: &str, budget: usize) -> String {
    let total: usize = s.chars().map(cjk_display_w).sum();
    if total <= budget {
        return s.to_string();
    }
    let mut used = 0usize;
    let mut out = String::new();
    for c in s.chars() {
        let w = cjk_display_w(c);
        if used + w > budget.saturating_sub(1) {
            break;
        }
        used += w;
        out.push(c);
    }
    out.push('…');
    out
}

#[cfg(test)]
mod picker_tag_tests {
    use super::*;

    fn row_from(s: &str) -> Vec<GridCell> {
        s.chars()
            .map(|c| {
                let mut cell = GridCell::blank();
                cell.ch = c;
                cell
            })
            .collect()
    }

    /// 실제 그리드처럼 한글(와이드) 문자 뒤에 스페이서 셀을 끼운 행 — alacritty
    /// composed 경로는 ' '(실측), kasa-bridge 경로는 '\0' 이라 둘 다 만든다.
    fn row_wide(s: &str, spacer: char) -> Vec<GridCell> {
        let mut out = Vec::new();
        for c in s.chars() {
            let mut cell = GridCell::blank();
            cell.ch = c;
            out.push(cell);
            if (c as u32) >= 0x1100 {
                let mut sp = GridCell::blank();
                sp.ch = spacer;
                out.push(sp);
            }
        }
        out
    }

    // 실측 /resume 피커 설명줄: "    14 minutes ago · main · 23KB · #프라나"
    #[test]
    fn picker_row_detected_plain_and_wide() {
        for row in [
            row_from("    14 minutes ago · main · 23KB · #프라나"),
            row_wide("    14 minutes ago · main · 23KB · #프라나", ' '),
            row_wide("    14 minutes ago · main · 23KB · #프라나", '\0'),
        ] {
            let (c0, end, slug) = picker_student_tag(&row).expect("tag");
            assert_eq!(slug, "prana");
            assert_eq!(row[c0].ch, '#');
            assert!(end > c0 && end < row.len());
            // 이름 마지막 셀까지 범위에 포함(블랭크 처리 범위).
            assert!(row[c0..=end].iter().any(|c| c.ch == '나'));
        }
    }

    // 좁은 창서 헤더가 wrap 되면 "Resume session" 과 "(N of M)" 사이에 행끝
    // 패딩(스페이스·\0)이 껴 예전엔 감지가 끊겨 accent 사각형이 남았다
    // (거노: 특정 창 크기에서만 재발). 공백류를 접어 wrap 무관하게 잡는다.
    #[test]
    fn resume_picker_survives_wrapped_header() {
        // 한 행 안 다수 공백(직접 매칭이면 "Resume session   (" 로 깨짐)
        assert!(screen_is_resume_picker(&[row_from(
            "Resume session      (2 of 5)"
        )]));
        // 두 행으로 wrap + 행끝 패딩(스페이스/\0)
        let wrapped_sp = vec![row_from("Resume session   "), row_from("(2 of 5)")];
        assert!(screen_is_resume_picker(&wrapped_sp));
        let wrapped_null = vec![row_from("Resume session\0\0\0"), row_from("(2 of 5)")];
        assert!(screen_is_resume_picker(&wrapped_null));
        // 정상 한 칸 케이스 유지
        assert!(screen_is_resume_picker(&[row_from("Resume session (3 of 9)")]));
        // 본문 산문은 여전히 무시(여는 괄호 시퀀스 없음)
        assert!(!screen_is_resume_picker(&[row_from(
            "let's Resume session tomorrow"
        )]));
    }

    // AskUserQuestion picker 는 "Chat about this"(항상 마지막 옵션) + 하단
    // 네비 힌트로 감지한다 — 정상 입력박스(미도리 세션제목 보더든 @칩이든)는
    // 건드리지 않고 picker 만 accent 배제(거노: question 도 사각형 잔상).
    #[test]
    fn ask_picker_detected_by_signature() {
        // 실측 시그니처: 옵션 목록 + "Chat about this" + "Enter to select"·"Esc to cancel"
        let full = vec![
            row_from("❯ 1. 기존과 동일 스윕"),
            row_from("  2. 은은한 정적 바"),
            row_from("  3. Type something."),
            row_from("──────────────"),
            row_from("  4. Chat about this"),
            row_from("Enter to select · ↑/↓ to navigate · Esc to cancel"),
        ];
        assert!(screen_is_ask_picker(&full));
        // "Tab/Arrow keys to navigate" 변형(Enter to select 문구 없이 Esc 만)
        let variant = vec![
            row_from("  N. Chat about this"),
            row_from("Tab/Arrow keys to navigate · Esc to cancel"),
        ];
        assert!(screen_is_ask_picker(&variant));
        // resume 피커는 ask 아님("Chat about this" 없음)
        assert!(!screen_is_ask_picker(&[row_from("Resume session (2 of 5)")]));
        // 본문에 "Chat about this" 가 우연히 있어도 힌트 없으면 무시(AND 게이트)
        assert!(!screen_is_ask_picker(&[row_from(
            "We could Chat about this later"
        )]));
    }

    // agents 목록(FleetView)은 상단 통계줄로 감지한다 — 세 조각이 구분자로 붙어
    // 한 줄을 이루는 것이 이 화면 고유고, 개수가 0 이어도 생략되지 않는다.
    // 실측 화면(2.1.237): "  ▝▝ ▝▝    3 awaiting input · 0 working · 4 completed"
    #[test]
    fn agents_list_detected_by_stats_line() {
        assert!(screen_is_agents_list(&[
            row_from("▐▛███▛█   Claude Code v2.1.237"),
            row_from("  ▝▝ ▝▝    3 awaiting input · 0 working · 4 completed"),
            row_from("Needs input"),
        ]));
        // 전부 0 인 빈 목록도 통계줄은 그대로 렌더된다.
        assert!(screen_is_agents_list(&[row_from(
            "0 awaiting input · 0 working · 0 completed"
        )]));
        // 좁은 창서 통계줄이 wrap + 행끝 패딩(스페이스/\0)이 껴도 잡힌다.
        assert!(screen_is_agents_list(&[
            row_from("3 awaiting input · 0   "),
            row_from("working · 4 completed"),
        ]));
        assert!(screen_is_agents_list(&[
            row_from("12 awaiting input\0\0\0"),
            row_from("· 7 working · 132 completed"),
        ]));
        // 백그라운드가 하나도 없을 때의 안내도 이 화면 고유.
        assert!(screen_is_agents_list(&[row_from(
            "Nothing running in the background."
        )]));
    }

    // ★회귀 방어: 예전 판정은 `contains("awaiting input") && contains("completed")`
    // 라 화면 **어디든** 두 낱말이 있으면 참이었다. 호출부의 `!has_profile_slot` 이
    // 그 오탐을 가려 주고 있었을 뿐이라, 그 안전판을 뗀 지금은 여기서 막아야 한다 —
    // 안 막으면 두 단어가 스치는 멀쩡한 대화에서 학생이 통째로 사라진다.
    #[test]
    fn agents_list_ignores_scattered_words_in_conversation() {
        // claude 문서를 대화에서 읽는 화면(번들 안에도 이런 문구가 여럿 있다).
        assert!(!screen_is_agents_list(&[
            row_from("Thread is awaiting input — or paused at the shared budget."),
            row_from("The migration completed without errors."),
        ]));
        // 순서가 맞아도 사이가 멀면(한 줄이 아니면) 아니다.
        assert!(!screen_is_agents_list(&[row_from(
            "awaiting input from the user, and once the long running job is working \
             through its queue we can call the task completed"
        )]));
        // 한 조각만 있는 경우.
        assert!(!screen_is_agents_list(&[row_from("2 completed · 1 working")]));
        assert!(!screen_is_agents_list(&[row_from("awaiting input")]));
    }

    // PR 번호(`repo#12`)의 '#' 는 이름 검증에서 떨어지고, 뒤의 진짜 태그가 잡힌다.
    #[test]
    fn pr_number_hash_skipped() {
        let row = row_from("    2 days ago · main · 1MB · repo#12 · #시로코");
        let (_, _, slug) = picker_student_tag(&row).expect("tag");
        assert_eq!(slug, "shiroko");
    }

    // 오탐 방어: '·' 1개뿐(태그 구분자만) / 구분자 없는 해시태그 / 로스터 밖 이름.
    #[test]
    fn non_picker_rows_ignored() {
        assert!(picker_student_tag(&row_from(" · #시로코")).is_none());
        assert!(picker_student_tag(&row_from("echo #시로코 · done")).is_none());
        assert!(picker_student_tag(&row_from("  1 day ago · main · 2KB · #낯선이")).is_none());
        assert!(picker_student_tag(&row_from("plain text without tags")).is_none());
    }
}

#[cfg(test)]
mod clawd_banner_tests {
    use super::*;

    fn row_from(s: &str) -> Vec<GridCell> {
        s.chars()
            .map(|c| {
                let mut cell = GridCell::blank();
                cell.ch = c;
                cell
            })
            .collect()
    }

    // 실측 배너(claude code 2.1.212): 머리 1칸·발 2칸 들여쓰기.
    const HEAD: &str = " ▐▛███▜▌  Claude Code v2.1.212";
    const BODY: &str = "▝▜█████▛▘ Fable 5 · ~/Desktop";
    const FEET: &str = "  ▘▘ ▝▝   0 awaiting input";

    // 실측 agy 로고(Antigravity CLI 1.1.12) — 아래로 갈수록 한 칸씩 넓어진다.
    const AGY: [&str; 5] = [
        "      ▄▀▀▄        Antigravity CLI 1.1.12",
        "     ▀▀▀▀▀▀       goenho0613@example.com (Google AI Pro)",
        "    ▀▀▀▀▀▀▀▀      Gemini 3.5 Flash (High)",
        "   ▄▀▀    ▀▀▄     ~/Desktop/momewomo/tmuxify",
        "  ▄▀▀      ▀▀▄",
    ];

    /// 2.1.23x 의 눈 달린 새 아트 — 옛 글리프만 알던 동안 새 배너가 통째로
    /// 안 잡혀 부팅 화면에 학생 테마가 안 붙었다(2026-08-20 거노 스샷).
    /// 컴팩트·박스형 웰컴 둘 다 이 3행이다(격리 리그 peek 실측).
    #[test]
    fn new_gen_clawd_banner_detected() {
        let rows = vec![
            row_from(" ▐▛███▛█   Claude Code v2.1.237"),
            row_from("▝▜██████▀  Fable 5 with xhigh effort · Claude Max"),
            row_from("  ▝▝ ▝▝    ~/Desktop"),
        ];
        assert_eq!(find_clawd_banners(&rows), vec![(0, 0)]);
        // 박스형 웰컴(테두리 │ 안, 들여쓰기) — 같은 아트가 안쪽에 앉는다.
        let boxed = vec![
            row_from("│               Welcome back SIONIC AI!    │"),
            row_from("│                                          │"),
            row_from("│                ▐▛███▛█                   │"),
            row_from("│               ▝▜██████▀                  │"),
            row_from("│                 ▝▝ ▝▝                    │"),
        ];
        assert_eq!(find_clawd_banners(&boxed), vec![(2, 16)]);
    }

    #[test]
    fn agy_logo_detected() {
        let rows: Vec<_> = AGY.iter().map(|s| row_from(s)).collect();
        assert_eq!(find_agy_banners(&rows), vec![(0, 2)]);
    }

    /// 스크롤로 위 세 줄이 잘려도 남은 두 줄로 잡아야 한다 — 못 잡으면 원본
    /// 로고가 그대로 노출돼 학생이 사라진다(Clawd 쪽에서 이미 겪은 회귀).
    #[test]
    fn agy_logo_cropped_from_top_is_still_detected() {
        let rows: Vec<_> = AGY[3..].iter().map(|s| row_from(s)).collect();
        assert_eq!(find_agy_banners(&rows), vec![(-3, 2)]);
    }

    #[test]
    fn agy_detector_ignores_ordinary_text() {
        let rows: Vec<_> = ["사과는 무슨 색인가", "▀▀▀ 짧은 괘선 ▀▀▀", ""]
            .iter()
            .map(|s| row_from(s))
            .collect();
        assert!(find_agy_banners(&rows).is_empty());
    }

    /// Clawd 칸에 대해서는 **항등**이어야 한다 — 이 커밋이 기존 배치를 건드리지
    /// 않았다는 증명이고, 어긋나면 claude 배너 도트가 통째로 밀린다.
    #[test]
    fn clawd_sprite_box_is_unchanged() {
        let (w, h) = fit_sprite_box(CLAWD_COLS, CLAWD_ROWS, 8.0, 17.0);
        assert_eq!((w, h), (CLAWD_COLS as f32 * 8.0, CLAWD_ROWS as f32 * 17.0));
    }

    /// 넓은 agy 칸에서는 늘리지 않고 비율을 지킨 채 채운다(12x5 칸 → 12x4 도트).
    #[test]
    fn agy_sprite_box_keeps_the_clawd_aspect() {
        let (w, h) = fit_sprite_box(AGY_COLS, AGY_ROWS, 8.0, 17.0);
        assert_eq!((w, h), (12.0 * 8.0, 4.0 * 17.0));
        assert!(h <= AGY_ROWS as f32 * 17.0, "칸 밖으로 넘쳤다");
    }

    #[test]
    fn agy_title_becomes_the_student_name() {
        let mut rows: Vec<_> = AGY.iter().map(|s| row_from(s)).collect();
        replace_banner_title(&mut rows, 0, 2, AGY_COLS, AGY_ROWS, AGY_TITLE, "시로코", None);
        let line: String = rows[0].iter().map(|c| c.ch).collect();
        // 와이드 글자는 뒷칸에 스페이서가 붙어 `시 로 코` 로 읽힌다 — 글자만 본다.
        let packed = line.replace(' ', "");
        assert!(packed.contains("시로코"), "학생 이름이 안 들어갔다: {line:?}");
        assert!(line.contains("1.1.12"), "버전이 사라졌다: {line:?}");
        assert!(!line.contains("Antigravity"), "제품명이 남았다: {line:?}");
    }

    /// codex 시작 패널 실측 행. 마스코트가 없어 도트는 못 세우지만 이름표는 바꾼다.
    #[test]
    fn codex_panel_title_becomes_the_student_name() {
        let mut rows = vec![row_from(
            "│ >_ OpenAI Codex (v0.147.0)                     │",
        )];
        let n = rows.len();
        replace_banner_title(&mut rows, 0, 0, 0, n, CODEX_TITLE, "아리스", None);
        let line: String = rows[0].iter().map(|c| c.ch).collect();
        let packed = line.replace(' ', "");
        assert!(packed.contains("아리스"), "학생 이름이 안 들어갔다: {line:?}");
        assert!(line.contains("(v0.147.0)"), "버전이 사라졌다: {line:?}");
        assert!(!line.contains("OpenAI"), "제품명이 남았다: {line:?}");
    }

    /// 대화 본문에 "OpenAI Codex" 가 나와도 안 걸려야 한다 — `>_` 까지 묶어 잡는 이유.
    #[test]
    fn codex_title_ignores_the_words_in_prose() {
        let mut rows = vec![row_from("OpenAI Codex 는 어떤 도구인가요?")];
        let before: String = rows[0].iter().map(|c| c.ch).collect();
        let n = rows.len();
        replace_banner_title(&mut rows, 0, 0, 0, n, CODEX_TITLE, "아리스", None);
        let after: String = rows[0].iter().map(|c| c.ch).collect();
        assert_eq!(before, after, "본문이 바뀌었다");
    }

    /// agy 화면 아래 실측 모양 — 보더 두 줄 사이 ASCII `>`, 그 아래 상태줄.
    #[test]
    fn agy_standing_anchor_found_under_the_prompt() {
        let bar = "─".repeat(40);
        let rows: Vec<_> = [
            "  사과는 보통 빨간색이나 초록색이네요.",
            "",
            "",
            "",
            bar.as_str(),
            "> ",
            bar.as_str(),
            "? for shortcuts                 Gemini 3.5 Flash",
        ]
        .iter()
        .map(|s| row_from(s))
        .collect();
        assert!(
            find_agy_standing_anchor(&rows, 40).is_some(),
            "agy 입력창 위 앵커를 못 잡았다"
        );
    }

    /// 인용문(`> …`)이 대시 사이에 있어도 **claude·codex 판정은 안 건드린다**.
    /// agy 앵커는 여기서만 `>` 를 인정하므로, 공용 `prompt_box` 는 여전히 거절해야 한다.
    #[test]
    fn ascii_marker_still_rejected_by_the_shared_prompt_box() {
        let bar = "─".repeat(40);
        let rows: Vec<_> = [bar.as_str(), "> 인용된 문장", bar.as_str()]
            .iter()
            .map(|s| row_from(s))
            .collect();
        assert!(prompt_box(&rows).is_none(), "인용문을 입력창으로 잡았다");
    }

    #[test]
    fn full_banner_detected() {
        let rows = vec![row_from(""), row_from(HEAD), row_from(BODY), row_from(FEET)];
        assert_eq!(find_clawd_banners(&rows), vec![(1, 0)]);
    }

    // 스크롤로 머리 행이 위로 잘림 — 몸통이 최상단 행. top_row = -1 로
    // 잡혀야 몸통·발이 blank 되고 스프라이트가 클립돼 그려진다(거노:
    // 스크롤 살짝 내리면 Clawd 원본 노출 회귀 방지).
    #[test]
    fn body_at_top_row_detected_as_cropped() {
        let rows = vec![row_from(BODY), row_from(FEET), row_from("")];
        assert_eq!(find_clawd_banners(&rows), vec![(-1, 0)]);
    }

    // 머리·몸통까지 잘리고 발만 최상단에 남은 경우.
    #[test]
    fn feet_only_at_top_row_detected() {
        let rows = vec![row_from(FEET), row_from(""), row_from("")];
        assert_eq!(find_clawd_banners(&rows), vec![(-2, 0)]);
    }

    // 아래에서 진입: 최하단 행에 머리만 보임 — top_row = 마지막 행.
    #[test]
    fn head_only_at_bottom_row_detected() {
        let rows = vec![row_from(""), row_from(""), row_from(HEAD)];
        assert_eq!(find_clawd_banners(&rows), vec![(2, 0)]);
    }

    // 몸통이 최하단 행(발만 화면 밖) — 머리+몸통 조합으로 잡힌다.
    #[test]
    fn body_at_bottom_row_detected() {
        let rows = vec![row_from(""), row_from(HEAD), row_from(BODY)];
        assert_eq!(find_clawd_banners(&rows), vec![(1, 0)]);
    }

    // 일반 텍스트·비슷한 블록 글리프는 오탐하지 않는다.
    #[test]
    fn plain_text_not_detected() {
        let rows = vec![
            row_from("normal output line"),
            row_from("▝▜███▛▘ short art"),
            row_from("▘▘▝▝ no gap feet"),
        ];
        assert_eq!(find_clawd_banners(&rows), Vec::<(isize, usize)>::new());
    }

    // 발 패턴이 최상단이라도 양옆에 다른 글자가 붙어 있으면 배너가 아니다.
    #[test]
    fn feet_without_flanking_blanks_not_detected() {
        let rows = vec![row_from("ab▘▘ ▝▝cd"), row_from("")];
        assert_eq!(find_clawd_banners(&rows), Vec::<(isize, usize)>::new());
    }

    fn dim_row(s: &str) -> Vec<GridCell> {
        s.chars()
            .map(|c| {
                let mut cell = GridCell::blank();
                cell.ch = c;
                cell.dim = true;
                cell
            })
            .collect()
    }

    // 스크롤 게이트("Jump to bottom") 가 없으면 상단이 흐릿해도 sticky 아님 —
    // 평상시(맨 아래) 오탐 방지의 핵심.
    #[test]
    fn sticky_needs_scroll_gate() {
        let rows = vec![dim_row("> 이전 프롬프트"), row_from("본문 라인")];
        assert!(find_sticky_prompt(&rows).is_none());
    }

    // 게이트 + 최상단 흐릿한 프롬프트 → 그 행·텍스트로 감지.
    #[test]
    fn sticky_gated_dim_top_detected() {
        let rows = vec![
            dim_row("> 이전 프롬프트 미리보기"),
            row_from("작업 결과 라인"),
            row_from("  Jump to bottom (click) ↓"),
        ];
        let s = find_sticky_prompt(&rows).expect("sticky detected");
        assert_eq!(s.row, 0);
        assert!(s.text.contains("이전 프롬프트"));
        assert_eq!(s.col_start, 0);
    }

    // 게이트가 있어도 상단이 흐릿하지 않으면(일반 밝은 텍스트) 감지 안 함.
    #[test]
    fn sticky_gated_but_bright_ignored() {
        let rows = vec![
            row_from("밝은 일반 출력 라인"),
            row_from("more output"),
            row_from("Jump to bottom (click)"),
        ];
        assert!(find_sticky_prompt(&rows).is_none());
    }

    // 타이틀 치환: "Claude Code" → 학생 이름(와이드+스페이서 셀), 버전 텍스트는
    // 이름 바로 뒤로 당겨지고 남는 칸은 blank, 행 길이는 불변.
    #[test]
    fn banner_title_replaced_with_student_name() {
        let mut rows = vec![row_from(""), row_from(HEAD), row_from(BODY), row_from(FEET)];
        replace_banner_title(
            &mut rows, 1, 0, CLAWD_COLS, CLAWD_ROWS, CLAWD_TITLE, "아루",
            Some([255, 128, 0, 255]),
        );
        // HEAD 에서 "Claude Code" 는 col 10 부터 — 이름이 그 자리에 앉는다.
        assert_eq!(rows[1][10].ch, '아');
        assert_eq!(rows[1][11].ch, ' '); // 와이드 스페이서
        assert_eq!(rows[1][12].ch, '루');
        assert_eq!(
            rows[1][10].fg,
            kasa_bridge::screen::Color::Rgb(255, 128, 0)
        );
        let tail: String = rows[1][14..23].iter().map(|c| c.ch).collect();
        assert_eq!(tail, " v2.1.212");
        assert!(rows[1][23..].iter().all(|c| c.ch == ' '));
        assert_eq!(rows[1].len(), row_from(HEAD).len());
        // 몸통·발 행은 그대로.
        assert_eq!(rows[2], row_from(BODY));
    }

    // 박스형 웰컴 변형: 버전 뒤 연속 공백 너머의 오른쪽 테두리는 밀리지 않는다.
    #[test]
    fn boxed_variant_right_border_untouched() {
        let head_box = "│  ▐▛███▜▌  Claude Code v2.1.212    │";
        let mut rows = vec![row_from(""), row_from(head_box), row_from(""), row_from("")];
        let border = head_box.chars().count() - 1;
        replace_banner_title(
            &mut rows, 1, 1, CLAWD_COLS, CLAWD_ROWS, CLAWD_TITLE, "시로코", None,
        );
        assert_eq!(rows[1][border].ch, '│');
        assert_eq!(rows[1][12].ch, '시');
        // accent 없으면 원 타이틀 fg(blank 기본 = Default) 유지.
        assert_eq!(rows[1][12].fg, kasa_bridge::screen::Color::Default);
    }

    // 머리 행이 스크롤로 잘려 타이틀이 화면 밖이면 아무것도 안 바꾼다.
    #[test]
    fn cropped_banner_leaves_rows_unchanged() {
        let mut rows = vec![row_from(BODY), row_from(FEET), row_from("")];
        let before = rows.clone();
        replace_banner_title(
            &mut rows, -1, 0, CLAWD_COLS, CLAWD_ROWS, CLAWD_TITLE, "아루", None,
        );
        assert_eq!(rows, before);
    }

    // 2.1.23x 박스형 웰컴: 타이틀이 아트 옆이 아니라 **상단 보더 줄**에 있다
    // (2026-08-20 peek 실측 "╭─── Claude Code v2.1.237 ───…"). 이름 치환 뒤에도
    // 우측 ╮ 열이 제자리고, 당겨서 남는 칸은 blank 가 아니라 ─ 로 메워져
    // 보더 선이 끊기지 않는다.
    #[test]
    fn banner_title_on_top_border_row() {
        let top = "╭─── Claude Code v2.1.237 ─────╮";
        let mut rows = vec![
            row_from(top),
            row_from("│                             │"),
            row_from("│        ▐▛███▛█             │"),
            row_from("│       ▝▜██████▀            │"),
            row_from("│         ▝▝ ▝▝              │"),
            row_from("╰─────────────────────────────╯"),
        ];
        let corner = top.chars().count() - 1;
        replace_banner_title(
            &mut rows, 2, 8, CLAWD_COLS, CLAWD_ROWS, CLAWD_TITLE, "히나",
            Some([255, 128, 0, 255]),
        );
        let line: String = rows[0].iter().map(|c| c.ch).collect();
        assert!(line.contains('히') && line.contains('나'), "이름 치환: {line}");
        assert!(line.contains("v2.1.237"), "버전 꼬리 유지: {line}");
        assert!(!line.contains("Claude Code"), "원 타이틀 제거: {line}");
        assert_eq!(rows[0][corner].ch, '╮', "우측 코너 제자리");
        // 버전 뒤 원래의 한 칸 공백을 지나면 코너까지 전부 대시 — 빈칸이 남으면
        // 보더 선이 끊긴 것. (str::find 는 바이트 오프셋이라 와이드 글자가 섞인
        // 줄에선 문자 인덱스와 어긋난다 — 문자 벡터에서 직접 찾는다.)
        let seg: Vec<char> = line.chars().collect();
        let pat: Vec<char> = "v2.1.237".chars().collect();
        let ver_end = seg
            .windows(pat.len())
            .position(|w| w == pat.as_slice())
            .unwrap()
            + pat.len();
        assert!(
            seg[ver_end + 1..corner].iter().all(|&c| c == '─'),
            "보더 대시 연속: {line}"
        );
        assert_eq!(
            rows[0][5].fg,
            kasa_bridge::screen::Color::Rgb(255, 128, 0),
            "이름 accent",
        );
    }

    const ALL_SLUGS: [&str; 12] = [
        "arona", "prana", "midori", "momoi", "yuzu", "arisu", "yuuka", "shiroko", "hoshino",
        "koharu", "himari", "aru",
    ];

    // 상태 모션 3종(idle/wave/cheer)이 12명 전원에 배선돼 있고 프레임 수가 맞다.
    // (include_bytes 라 파일 부재면 컴파일 실패 — 여긴 arm 매칭 오타를 잡는다.)
    #[test]
    fn every_student_has_state_motions() {
        for slug in ALL_SLUGS {
            for motion in ["idle", "wave", "cheer"] {
                let f = student_sprite_png(slug, motion)
                    .unwrap_or_else(|| panic!("{slug} {motion} 프레임 미배선"));
                assert_eq!(f.len(), STUDENT_IDLE_FRAMES, "{slug} {motion} 프레임 수");
            }
            assert_eq!(
                student_sprite_png(slug, "walk").map(|f| f.len()),
                Some(STUDENT_WALK_FRAMES),
                "{slug} walk 프레임 수",
            );
        }
    }

    // 로스터 밖 슬러그·미지원 모션은 None.
    #[test]
    fn unknown_slug_or_motion_is_none() {
        assert!(student_sprite_png("nobody", "idle").is_none());
        assert!(student_sprite_png("koharu", "sob").is_none());
    }

    // 텍스처 캐시 키 접두: idle 만 "f"(기존 배너 캐시 호환), 나머지는 이름 그대로.
    #[test]
    fn motion_key_prefixes() {
        assert_eq!(sprite_key_prefix("idle"), "f");
        assert_eq!(sprite_key_prefix("wave"), "wave");
        assert_eq!(sprite_key_prefix("cheer"), "cheer");
        assert_eq!(sprite_key_prefix("walk"), "walk");
        // 미지의 모션은 idle 프레임 접두로 폴백(빈 화면보다 낫다).
        assert_eq!(sprite_key_prefix("???"), "f");
    }

    // 한글은 실제 composed 처럼 글리프+스페이서 2칸으로 배치하는 헬퍼.
    fn wide_row(s: &str) -> Vec<GridCell> {
        let mut row = Vec::new();
        for c in s.chars() {
            let mut cell = GridCell::blank();
            cell.ch = c;
            row.push(cell);
            if crate::gpu::is_wide_char(c) {
                let mut sp = GridCell::blank();
                sp.ch = ' ';
                row.push(sp);
            }
        }
        row
    }

    // 셀 행 → 텍스트(와이드 스페이서 제거) — 검증용.
    fn row_text(row: &[GridCell]) -> String {
        let mut s = String::new();
        let mut i = 0;
        while i < row.len() {
            let ch = row[i].ch;
            if ch != '\0' {
                s.push(ch);
            }
            i += 1;
            if crate::gpu::is_wide_char(ch) && i < row.len() && matches!(row[i].ch, ' ' | '\0') {
                i += 1;
            }
        }
        s
    }

    // 1컬럼(좁은 폭): 도트 위 "Welcome back 건호!" → 학생 인사말 + accent 색,
    // 이름("건호")은 그리드에서 추출해 인사말에 삽입된다. 폭은 넉넉히(클립 없음).
    #[test]
    fn welcome_greeting_single_column() {
        let pad = " ".repeat(50);
        let mut rows = vec![
            wide_row(&format!("  Welcome back 건호!{pad}")),
            row_from("   ▐▛███▜▌"),
            row_from("  ▝▜█████▛▘"),
            row_from("    ▘▘ ▝▝"),
        ];
        replace_welcome_greeting(&mut rows, 1, "코하루", Some([200, 50, 50, 255]));
        let line = row_text(&rows[0]);
        assert!(line.contains("어서오세요"), "인사말 치환됨: {line}");
        assert!(line.contains("건호"), "이름 추출·삽입: {line}");
        assert!(!line.contains("Welcome back"), "원문 제거: {line}");
        let first = rows[0].iter().find(|c| !matches!(c.ch, ' ' | '\0')).unwrap();
        assert_eq!(first.fg, kasa_bridge::screen::Color::Rgb(200, 50, 50));
    }

    // 2컬럼(넓은 폭): "Welcome back" 뒤 박스 세로 보더 │ 는 인사말이 길어도 안 밀린다.
    #[test]
    fn welcome_greeting_clipped_at_border() {
        let mut rows = vec![
            wide_row("│  Welcome back 건호!    │"),
            row_from(" ▐▛███▜▌"),
            row_from("▝▜█████▛▘"),
            row_from("  ▘▘ ▝▝"),
        ];
        let border = rows[0].iter().rposition(|c| c.ch == '│').unwrap();
        replace_welcome_greeting(&mut rows, 1, "아루", None); // 아루 인사말 = 긴 편
        assert_eq!(rows[0][border].ch, '│', "우측 보더 보존");
        assert!(
            rows[0][border + 1..].iter().all(|c| matches!(c.ch, ' ' | '\0')),
            "보더 너머 무변화",
        );
    }

    // 2컬럼: 같은 행 오른쪽 Tips 컬럼은 인사말이 길어도 침범하지 않는다.
    #[test]
    fn welcome_greeting_preserves_right_column() {
        let mut rows = vec![
            wide_row("  Welcome back 건호!      Tips for getting started"),
            row_from(" ▐▛███▜▌"),
            row_from("▝▜█████▛▘"),
        ];
        let tips_col = rows[0].iter().position(|c| c.ch == 'T').unwrap();
        let tips_before: Vec<char> = rows[0][tips_col..].iter().map(|c| c.ch).collect();
        replace_welcome_greeting(&mut rows, 1, "아루", None);
        let tips_after: Vec<char> = rows[0][tips_col..].iter().map(|c| c.ch).collect();
        assert_eq!(tips_before, tips_after, "오른쪽 Tips 컬럼 보존");
    }

    // launcher 등 "Welcome back" 행이 없으면 no-op(원본 그리드 무변경).
    #[test]
    fn welcome_greeting_noop_without_welcome() {
        let mut rows = vec![
            row_from("Claude Code v2.1.215"),
            row_from(" ▐▛███▜▌"),
            row_from("▝▜█████▛▘"),
        ];
        let before = rows.clone();
        replace_welcome_greeting(&mut rows, 1, "코하루", None);
        assert_eq!(rows, before, "웰컴 행 없으면 무변경");
    }

    // 개별 인사말이 없는 로스터 학생도 범용 존대 한 줄로 치환된다 — 12명만
    // 알고 None 을 돌려보내던 동안 그 학생 pane 은 테두리 학생색까지 통째로
    // 빠졌다(2026-08-20 히나 pane 실측).
    #[test]
    fn welcome_greeting_fallback_for_unlisted_student() {
        let pad = " ".repeat(40);
        let mut rows = vec![
            wide_row(&format!("  Welcome back 건호!{pad}")),
            row_from(" ▐▛███▛█"),
        ];
        replace_welcome_greeting(&mut rows, 1, "히나", Some([200, 50, 50, 255]));
        let line = row_text(&rows[0]);
        assert!(line.contains("어서 오세요"), "범용 인사말 치환: {line}");
        assert!(line.contains("건호"), "이름 추출·삽입: {line}");
        assert!(!line.contains("Welcome back"), "원문 제거: {line}");
    }

    // 배너 박스 보더가 학생 accent 로 틴트되고, 범위 밖 다른 박스는 오염 안 된다.
    // 호출 순서는 render.rs 와 같다 — 인사말 치환과 무관하게 tint 를 따로 부른다.
    #[test]
    fn welcome_box_border_tinted() {
        let acc = [80, 160, 240, 255];
        let mut rows = vec![
            row_from("╭─ Claude Code ─╮"),
            wide_row("│ Welcome back 건호! │"),
            row_from("│   ▐▛███▜▌   │"),
            row_from("│  ▝▜█████▛▘  │"),
            row_from("│    ▘▘ ▝▝    │"),
            row_from("╰───────────────╯"),
            row_from("╭─ other box ─╮"),
        ];
        replace_welcome_greeting(&mut rows, 2, "코하루", Some(acc));
        tint_welcome_box(&mut rows, 2, 5, acc);
        let want = kasa_bridge::screen::Color::Rgb(80, 160, 240);
        assert_eq!(rows[0][0].fg, want, "상단 보더 ╭ 틴트");
        assert_eq!(rows[5][0].fg, want, "하단 보더 ╰ 틴트");
        assert_eq!(rows[1][0].fg, want, "welcome 행 좌 │ 틴트");
        assert_eq!(
            rows[6][0].fg,
            kasa_bridge::screen::Color::Default,
            "범위 밖 다른 박스 미오염",
        );
    }

    // 2.1.23x 박스형 웰컴: 인사말 로스터에 없는 학생이어도 보더 틴트는 된다 —
    // 실물 레이아웃(2026-08-20 peek) 축약본으로 render.rs 호출 순서를 재현.
    #[test]
    fn box_welcome_tints_even_when_greeting_unlisted() {
        let acc = [120, 90, 200, 255];
        let mut rows = vec![
            row_from("╭─── Claude Code v2.1.237 ─────────────╮"),
            wide_row("│      Welcome back SIONIC AI!         │"),
            row_from("│                                      │"),
            row_from("│        ▐▛███▛█                       │"),
            row_from("│       ▝▜██████▀                      │"),
            row_from("│         ▝▝ ▝▝                        │"),
            row_from("╰──────────────────────────────────────╯"),
        ];
        let br = 3isize; // 아트 머리 행
        replace_welcome_greeting(&mut rows, br, "히나", Some(acc));
        tint_welcome_box(&mut rows, br as usize, (br + 3) as usize, acc);
        let want = kasa_bridge::screen::Color::Rgb(120, 90, 200);
        assert_eq!(rows[0][0].fg, want, "상단 보더 틴트");
        assert_eq!(rows[6][0].fg, want, "하단 보더 틴트");
        let line = row_text(&rows[1]);
        assert!(line.contains("어서 오세요"), "범용 인사말: {line}");
    }
}

#[cfg(test)]
mod spinner_tests {
    use super::*;

    fn row_from(s: &str) -> Vec<GridCell> {
        s.chars()
            .map(|c| {
                let mut cell = GridCell::blank();
                cell.ch = c;
                cell
            })
            .collect()
    }

    // 점(·) 프레임 회귀 방지: 예전엔 별/점자 글리프만 잡아 점 프레임에서
    // None 을 반환 → 학생 도트가 프레임마다 깜빡였다.
    #[test]
    fn spinner_detects_dot_frame() {
        let rows = vec![
            row_from(""),
            row_from("· Cerebrating… (esc to interrupt)"),
        ];
        assert_eq!(find_claude_spinner(&rows), Some((1, 0)));
    }

    // 라이브 실측(claude code 2.1.207): 스피너 행에 "esc to interrupt" 힌트가
    // 없다 — 점 프레임은 점+… 문맥만으로 잡혀야 한다.
    #[test]
    fn spinner_detects_dot_frame_without_esc_hint() {
        let rows = vec![row_from("· Caramelizing… (3m 39s · ↓ 9.7k tokens)")];
        assert_eq!(find_claude_spinner(&rows), Some((0, 0)));
    }

    /// 경과시간이 괄호 **맨 앞**이 아닌 변종. 토큰이 먼저 오는 프레임을 놓쳐
    /// 학생 색이 늦게 붙던 것(거노 2026-08-20 「바로 안 붙을 때도 있어」).
    #[test]
    fn spinner_detects_elapsed_after_tokens() {
        let rows = vec![row_from("✶ Skedaddling… (↓ 1.2k tokens · 3s)")];
        assert_eq!(find_claude_spinner(&rows), Some((0, 0)));
    }

    /// 그 완화가 관문을 무의미하게 만들면 안 된다 — 시간 없이 토큰만 있는 꼬리는
    /// 여전히 스피너가 아니다(`tokens` 의 s 는 앞이 글자라 시간으로 안 센다).
    /// 이쪽은 `spinner_probe`·제출 직후 신뢰 창이 따로 구제하는 몫이다.
    #[test]
    fn spinner_rejects_tokens_without_elapsed() {
        let rows = vec![row_from("✶ Skedaddling… (↓ 1.2k tokens)")];
        assert_eq!(find_claude_spinner(&rows), None);
    }

    #[test]
    fn spinner_detects_star_and_braille() {
        let star = vec![row_from("✻ Working… (esc to interrupt)")];
        assert!(find_claude_spinner(&star).is_some());
        let braille = vec![row_from("⠹ Loading")];
        assert!(find_claude_spinner(&braille).is_some());
    }

    #[test]
    fn spinner_ignores_plain_text() {
        let rows = vec![row_from("just some normal output line")];
        assert_eq!(find_claude_spinner(&rows), None);
    }

    // 2026-08-12 지적: 학생이 답변 본문 위를 걸어다녔다. 가운뎃점과 줄임표는
    // 한국어에서 흔한 문장부호라 「앞머리 8칸 안의 · + 행에 …」 규칙에 평범한
    // 답변 줄이 걸렸다. 아래 셋은 전부 실제로 오탐했던 줄이다.
    #[test]
    fn spinner_ignores_korean_prose() {
        // 줄바꿈된 본문 — 가운뎃점이 col 2, 줄 끝에 줄임표.
        let wrapped = vec![row_from("간·창별 막대) → Usage details & history / Manage Accounts…")];
        assert_eq!(find_claude_spinner(&wrapped), None);
        // 가운뎃점으로 시작하는 한국어 목록 줄.
        let bullet = vec![row_from("· 임계 60/80%, 문구까지 Orca 그대로…")];
        assert_eq!(find_claude_spinner(&bullet), None);
        // 별표로 시작해도 경과시간 괄호가 없으면 스피너가 아니다.
        let star = vec![row_from("✻ 계정 메뉴를 다시 그렸어요…")];
        assert_eq!(find_claude_spinner(&star), None);
        // 줄임표 뒤에 괄호가 있어도 경과시간이 아니면 본문이다.
        let paren = vec![row_from("· 셋을 고쳤어요… (아래 표 참고)")];
        assert_eq!(find_claude_spinner(&paren), None);
    }

    #[test]
    fn spinner_reads_korean_verb() {
        // 동사가 한국어인 진짜 스피너 — 뒤의 경과시간이 표식이다. 이걸 놓치면
        // working 인 pane 의 학생이 안 걷는다(2026-08-13 회귀).
        let ko = vec![row_from("· claude 테마 자동 연동 구현 중… (3m 19s · ↓ 14.2k tokens)")];
        assert_eq!(find_claude_spinner(&ko), Some((0, 0)));
        let en = vec![row_from("✻ Cerebrating… (12s · ↑ 2.1k tokens)")];
        assert_eq!(find_claude_spinner(&en), Some((0, 0)));
    }

    // 2026-08-13 지적("저기서 왜 걷고있어"): 스피너 감지를 설명하는 답변이 스피너
    // 형태를 인용하자 그 인용줄이 잡혀, 턴이 끝난 뒤에도 학생이 본문 위를 걸었다.
    // 글자는 진짜와 완전히 같으므로 위치로 가른다 — 아래에 대화 마커가 있으면 옛것.
    #[test]
    fn spinner_ignores_quoted_spinner_above_prose() {
        let quoted = vec![
            row_from("⏺ claude 는 한국어로도 찍어요:"),
            row_from("  · claude 테마 자동 연동 구현 중… (3m 19s · ↓ 14.2k tokens)"),
            row_from(""),
            row_from("  ⎿  Referenced file app/kasaterm/src/render.rs"),
            row_from("✻ Cogitated for 4m 3s"),
            row_from("❯"),
        ];
        assert_eq!(find_claude_spinner(&quoted), None);
        assert!(!crate::input::rows_show_working(&quoted));
    }

    // 같은 화면에 인용줄과 진짜 스피너가 둘 다 있으면 살아 있는 쪽만 잡아야 한다.
    #[test]
    fn spinner_picks_live_one_over_quote() {
        let mixed = vec![
            row_from("  ✢ Processing… (4m 10s · ↓ 5.5k tokens)"),
            row_from("  ⎿  $ kasaterm-cli peek %14"),
            row_from(""),
            row_from("✶ Processing… (5m 11s · ↓ 8.7k tokens)"),
            row_from("❯"),
        ];
        assert_eq!(find_claude_spinner(&mixed), Some((3, 0)));
        assert!(crate::input::rows_show_working(&mixed));
    }

    // todo 트리가 스피너와 입력박스 사이에 끼어 거리가 흔들려도 살아 있다고 봐야
    // 한다 — 거리 대신 마커 유무로 가르는 이유가 이것이다.
    #[test]
    fn spinner_survives_todo_tree_below() {
        let with_todo = vec![
            row_from("  ⎿  이전 도구 출력"),
            row_from(""),
            row_from("✢ Processing… (1m 2s · ↓ 900 tokens)"),
            row_from("  ☐ 첫째 할 일"),
            row_from("  ☒ 둘째 할 일"),
            row_from("❯"),
        ];
        assert_eq!(find_claude_spinner(&with_todo), Some((2, 0)));
        assert!(crate::input::rows_show_working(&with_todo));
    }

    // 태스크 목록 위젯은 스피너 **바로 아래** `⎿  ◻ 항목` 으로 뜬다(2026-08-15
    // 라이브 pane peek 실측). 이 ⎿ 를 대화 마커로 세면 태스크를 쓰는 working
    // pane 전부에서 스피너가 죽어 학생이 걷다 말고 입력창 위에 서 버린다.
    // 턴 시작 첫 ~3초의 괄호 없는 스피너(0.3s 채집 실측)는 **후보**로만 잡힌다 —
    // 확정은 프로브(글리프 변화) 몫. 본판정이 잡는 행·인용문(아래 마커)은 후보도
    // 아니어야 한다.
    #[test]
    fn parenless_turn_start_spinner_is_probe_candidate_only() {
        let boot = vec![
            row_from("✢ Transmuting…"),
            row_from(&"─".repeat(60)),
            row_from(&format!("❯{}", " ".repeat(59))),
            row_from(&"─".repeat(60)),
        ];
        assert_eq!(find_claude_spinner(&boot), None, "본판정은 여전히 거부해야 한다");
        assert_eq!(unconfirmed_spinner_row(&boot).map(|(r, c, g)| (r, c, g)), Some((0, 0, '✢')));
        // 괄호가 붙은 확정 스피너는 후보 경로가 아니라 본판정 몫이다.
        let confirmed = vec![row_from("✻ Clauding… (3s · ↓ 7 tokens)")];
        assert!(find_claude_spinner(&confirmed).is_some());
        assert_eq!(unconfirmed_spinner_row(&confirmed), None);
        // 아래에 대화 마커가 있으면 옛 본문 — 후보도 아니다.
        let quoted = vec![row_from("✢ Transmuting…"), row_from("⏺ 답변 마커")];
        assert_eq!(unconfirmed_spinner_row(&quoted), None);
    }

    // ★회귀: 부팅·재개 직후 스피너는 경과시간 괄호 없이 `✻ Computing…` + `⎿ Tip:`
    // 만 뜬다(2026-08-15 스샷). 괄호 요구에 걸려 학생이 스피너에 안 붙었다 —
    // Tip 행이 구제 신호다.
    #[test]
    fn boot_spinner_without_elapsed_parens_is_rescued_by_tip_row() {
        let boot = vec![
            row_from("✻ Computing…"),
            row_from("  ⎿  Tip: Press Shift+Tab to auto-accept"),
        ];
        assert_eq!(find_claude_spinner(&boot), Some((0, 0)));
        assert!(crate::input::rows_show_working(&boot));
        // 괄호도 Tip 도 없는 별+줄임표 행은 여전히 본문으로 본다 — 한국어 산문
        // 오탐(2026-08-12)이 이 요구를 세운 이유였다.
        let prose = vec![
            row_from("✻ 설정을 정리했다… 이어서 계정 쪽을 본다"),
            row_from("  다음 줄 본문"),
        ];
        assert_eq!(find_claude_spinner(&prose), None);
        // 인용된 부팅 화면(아래에 대화 마커)은 spinner_is_live 가 걸러낸다.
        let quoted_boot = vec![
            row_from("✻ Computing…"),
            row_from("  ⎿  Tip: Press Shift+Tab to auto-accept"),
            row_from("⏺ 이건 지난 턴의 답변 마커"),
        ];
        assert_eq!(find_claude_spinner(&quoted_boot), None);
    }

    #[test]
    fn spinner_survives_task_widget_below() {
        let with_widget = vec![
            row_from("✽ Ideating… (3m 55s · ↓ 11.4k tokens · esc to interrupt)"),
            row_from("  ⎿  ◻ 설정 다국어 — 문구 사전과 언어 훅"),
            row_from("     ◻ 다음 태스크"),
            row_from("❯"),
        ];
        assert_eq!(find_claude_spinner(&with_widget), Some((0, 0)));
        assert!(crate::input::rows_show_working(&with_widget));
        // 도구 출력 인용 ⎿ 는 여전히 스피너를 죽이는 마커다 — 지난 턴 화면에
        // 남은 스피너 문구를 살아 있다고 오인하지 않게 보호.
        let quoted = vec![
            row_from("✽ Ideating… (3m 55s · ↓ 11.4k tokens · esc to interrupt)"),
            row_from("  ⎿  Read 50 lines"),
            row_from("❯"),
        ];
        assert_eq!(find_claude_spinner(&quoted), None);
    }
}

#[cfg(test)]
mod teammate_msg_tests {
    use super::*;

    fn row_from(s: &str, cols: usize) -> Vec<GridCell> {
        let mut row = vec![GridCell::blank(); cols];
        for (i, c) in s.chars().enumerate() {
            row[i].ch = c;
        }
        row
    }

    fn row_text(row: &[GridCell]) -> String {
        row.iter()
            .map(|c| if c.ch == '\0' { ' ' } else { c.ch })
            .collect::<String>()
            .trim_end()
            .to_string()
    }

    #[test]
    fn detects_collapsed_line() {
        let row = row_from("  › Message from @aru-9c88", 80);
        assert_eq!(
            teammate_collapsed_line(&row),
            Some((2, 1, "aru-9c88".to_string()))
        );
        // v2.1.216+: 이름 뒤 "(ctrl+o to expand)" 단축키 힌트가 붙어도 접힌 줄.
        let hinted = row_from("  › Message from @aru-9c88 (ctrl+o to expand)", 80);
        assert_eq!(
            teammate_collapsed_line(&hinted),
            Some((2, 1, "aru-9c88".to_string()))
        );
        let plural_hinted = row_from("› 3 messages from @yuzu-1ba1 (ctrl+o to expand)", 80);
        assert_eq!(
            teammate_collapsed_line(&plural_hinted),
            Some((0, 3, "yuzu-1ba1".to_string()))
        );
    }

    // 복수형 "› N messages from @이름" → count=N, 이름 추출(여러 자릿수 포함).
    #[test]
    fn detects_plural_collapsed_line() {
        let row = row_from("  › 3 messages from @aru-9c88", 80);
        assert_eq!(
            teammate_collapsed_line(&row),
            Some((2, 3, "aru-9c88".to_string()))
        );
        let row2 = row_from("› 12 messages from @yuzu-1ba1", 80);
        assert_eq!(
            teammate_collapsed_line(&row2),
            Some((0, 12, "yuzu-1ba1".to_string()))
        );
    }

    // 이름 뒤에 다른 글자가 있으면 본문 안 인용 — 실출력 덮어쓰기 오탐 방지.
    // 단수·복수 양쪽 다.
    #[test]
    fn rejects_trailing_text_and_plain_lines() {
        let quoted = row_from("› Message from @aru-9c88 라고 떴다", 80);
        assert_eq!(teammate_collapsed_line(&quoted), None);
        let plain = row_from("Message from @aru-9c88", 80);
        assert_eq!(teammate_collapsed_line(&plain), None);
        // 복수형도 이름 뒤 텍스트면 거부.
        let plural_quoted = row_from("› 3 messages from @aru-9c88 이라고", 80);
        assert_eq!(teammate_collapsed_line(&plural_quoted), None);
    }

    #[test]
    fn extract_tag_attrs_and_body() {
        let text = "<teammate-message teammate_id=\"aru-9c88\" color=\"orange\" \
                    summary=\"확인 통지\">아루다. 확인했다.</teammate-message>";
        let m = extract_teammate_msg(text, "aru-9c88").unwrap();
        assert_eq!(m.body, "아루다. 확인했다.");
        assert_eq!(m.color.as_deref(), Some("orange"));
        // 다른 보낸이의 태그는 건너뛰고 일치하는 태그만.
        assert!(extract_teammate_msg(text, "yuzu-1ba1").is_none());
    }

    // 세션명 rule 검출: 진짜 "── 이름 ──" 만. 둥근 입력박스 테두리(╭────╮·╰────╯)의
    // 모서리는 box-drawing 이라 이름 섬이 아니다 — 행 전체 사각형 오탐 회귀 방지(거노).
    #[test]
    fn titled_rule_ignores_box_border_rows() {
        let dash = |n: usize| "─".repeat(n);
        let title = row_from(&format!("{} 세션명 {}", dash(30), dash(30)), 80);
        let (r, c0, c1) = find_titled_rule(&[title]).expect("이름 섬 있는 rule 인정");
        assert_eq!(r, 0);
        assert!(c0 > 0 && c1 < 79, "이름 구간만, 행 전체 아님");
        let top = row_from(&format!("╭{}╮", dash(70)), 80);
        let bottom = row_from(&format!("╰{}╯", dash(70)), 80);
        let plain = row_from(&dash(72), 80);
        assert!(find_titled_rule(&[top]).is_none(), "둥근 상단 테두리 무시");
        assert!(find_titled_rule(&[bottom]).is_none(), "둥근 하단 테두리 무시");
        assert!(find_titled_rule(&[plain]).is_none(), "순수 rule 무시");
    }

    // teammate 칩 행(`──── @이름 ──`)은 세션명이 아니다 — claude 네이티브가 그리는
    // agent 이름 배지라 아웃라인(사각 테두리)을 두르면 안 된다(거노 2026-07-27:
    // "칩 네모칸"). 세션명 rule 은 계속 인정.
    #[test]
    fn titled_rule_ignores_agent_chip_row() {
        let dash = |n: usize| "─".repeat(n);
        let chip = row_from(&format!("{} @model-check {}", dash(50), dash(4)), 80);
        assert!(find_titled_rule(&[chip]).is_none(), "@칩 행은 세션명 아님");
        // 칩과 무관한 진짜 세션명은 그대로 인정.
        let title = row_from(&format!("{} 세션명 {}", dash(30), dash(30)), 80);
        assert!(find_titled_rule(&[title]).is_some(), "세션명 rule 은 유지");
    }

    // ` ultracode ` 배지 행도 세션명이 아니다 — claude 2.1.228 이 세션명과 같은
    // 자리(상단 보더 우측 끝)에 그리는 모드 표시라, 아웃라인을 두르면 /rename 된
    // 것처럼 보인다(2026-08-12 지적).
    #[test]
    fn titled_rule_ignores_ultracode_badge() {
        let dash = |n: usize| "─".repeat(n);
        let badge = row_from(&format!("{} ultracode {}", dash(60), dash(4)), 80);
        assert!(find_titled_rule(&[badge]).is_none(), "ultracode 배지는 세션명 아님");
        // fast 모드 태그가 이어 붙는 형태도 같은 배지다.
        let both = row_from(&format!("{} ultracode  fast {}", dash(55), dash(4)), 80);
        assert!(find_titled_rule(&[both]).is_none(), "배지 + fast 태그도 무시");
        let title = row_from(&format!("{} 세션명 {}", dash(30), dash(30)), 80);
        assert!(find_titled_rule(&[title]).is_some(), "세션명 rule 은 유지");
    }

    /// 배지는 무시를 넘어 **지워진다** — 그 자리는 /rename 세션명 자리라 글자가
    /// 남아 있으면 이름이 바뀐 것처럼 읽힌다(2026-08-12 지적 「/rename 그자리에
    /// ultracode 써진다」). 세션명 rule 은 건드리면 안 된다.
    #[test]
    fn ultracode_badge_erased_from_rule() {
        let dash = |n: usize| "─".repeat(n);
        for badge_text in ["ultracode", "ultracode  fast"] {
            let badge =
                row_from(&format!("{} {badge_text} {}", dash(55), dash(4)), 80);
            let mut rows = vec![badge];
            erase_ultracode_badge(&mut rows);
            let text: String = rows[0].iter().map(|c| c.ch).collect();
            assert!(!text.contains("ultracode"), "배지가 남았다: {text:?}");
            assert!(
                rows[0].iter().all(|c| matches!(c.ch, '─' | ' ' | '\0')),
                "대시로 되메워져야 한다: {text:?}"
            );
        }
        let title = row_from(&format!("{} 세션명 {}", dash(30), dash(30)), 80);
        let mut rows = vec![title];
        erase_ultracode_badge(&mut rows);
        let text: String = rows[0].iter().map(|c| c.ch).collect();
        assert!(text.contains("세션명"), "세션명 rule 을 지우면 안 된다: {text:?}");
    }

    // 크로스-방 tell 마커: 유효 캐릭터 `⟦이름⟧` 만 인정, 거노 직접 입력(마커 없음)·
    // 오탐(`⟦…⟧` 이지만 캐릭터 아님)은 무시 = 무색.
    #[test]
    fn tell_marker_parsed_and_guarded() {
        let row = row_from("⟦미도리⟧ 안녕하세요", 80);
        let (start, _, name) = tell_marker_line(&row).expect("유효 캐릭터 마커");
        assert_eq!((start, name.as_str()), (0, "미도리"));
        // claude TUI 실화면: 제출된 user 턴은 `❯ ` 프롬프트 마커 뒤에 온다.
        let prompted = row_from("❯ ⟦프라나⟧ 검증 메시지", 80);
        let (start, _, name) = tell_marker_line(&prompted).expect("❯ 뒤 마커도 인정");
        assert_eq!((start, name.as_str()), (2, "프라나"), "마커 시작 = ❯ + 공백 뒤");
        assert!(tell_marker_line(&row_from("그냥 내 입력", 80)).is_none());
        assert!(tell_marker_line(&row_from("❯ 마커 없는 제출", 80)).is_none());
        assert!(tell_marker_line(&row_from("⟦없는캐릭⟧ x", 80)).is_none());
    }

    // 프사 캐릭터: 아바타는 본문 위 행으로 올라가고(반환 col 이 그 x 기준) 본문은
    // 마커 자리(= `❯ ` 뒤 col 2)로 당겨져 wrap 연속 행과 좌측이 맞는다. `❯` 자리엔
    // 인용 마커 `›` 만 남고 이름 텍스트는 프사가 대신한다.
    #[test]
    fn restyle_tell_lifts_face_and_aligns_body() {
        let mut row = row_from("❯ ⟦미도리⟧ 본문", 80);
        let (marker_start, marker_end, name) = tell_marker_line(&row).unwrap();
        let face_col =
            restyle_tell_line(&mut row, marker_start, marker_end, &name, [107, 207, 127, 255]);
        assert_eq!(face_col, Some(0), "프사 x = ❯ 가 있던 왼쪽 여백");
        assert_eq!(row[0].ch, ' ', "여백은 프사 자리로 비운다");
        assert_eq!(row[2].ch, '본', "본문은 col 2 = wrap 연속 행과 동일");
        assert!(!row_text(&row).contains("미도리"), "이름은 프사가 대신");
    }

    // 실제 화면은 한글이 2셀이라 마커 `⟦이름⟧ ` 폭이 이름 길이에 따라 가변이다 —
    // 본문 시작 col 이 그 폭에 휘둘리면 wrap 연속 행과 계단이 진다(거노 2026-07-27).
    #[test]
    fn tell_body_col_independent_of_name_width() {
        // wide 셀 재현: 한글 뒤에 스페이서 한 칸(composed 경로와 동일).
        let mut row = row_from("❯ ⟦호 시 노 ⟧ 본문", 80);
        let (marker_start, marker_end, name) = tell_marker_line(&row).expect("마커 인식");
        assert_eq!(name, "호시노");
        let face_col = restyle_tell_line(&mut row, marker_start, marker_end, &name, [107, 207, 127, 255])
            .expect("프사");
        assert_eq!(face_col, 0, "이름이 길어도 프사는 왼쪽 여백 고정");
        assert_eq!(row[2].ch, '본', "{}", row_text(&row));
    }

    // wrap 연속 행: 2칸 **이상** 들여쓰기 본문은 연속, TUI 구조 글리프(⎿·⏺)·
    // 빈 행에서 끊김. 목록 항목 wrap 은 4~5칸으로 떨어진다 — 「정확히 2」로
    // 가두면 그 행에서 걸음이 끊겨 아래 문단 전체가 무테마(2026-08-20 스샷).
    #[test]
    fn tell_wrap_continuation_bounds() {
        assert!(tell_wrap_continuation(&row_from("  짧게 답해줘.", 80)));
        assert!(tell_wrap_continuation(&row_from("   들여쓰기 3", 80)));
        assert!(tell_wrap_continuation(&row_from("    남아있음.", 80)));
        assert!(tell_wrap_continuation(&row_from("     view 772, gh pr diff", 80)));
        assert!(!tell_wrap_continuation(&row_from("  ⎿  4 skills available", 80)));
        assert!(!tell_wrap_continuation(&row_from("    ⎿ 깊은 구조 글리프도 끊김", 80)));
        assert!(!tell_wrap_continuation(&row_from("⏺ 확인", 80)));
        assert!(!tell_wrap_continuation(&row_from("", 80)));
        assert!(!tell_wrap_continuation(&row_from(" 들여쓰기 1", 80)));
    }

    // 내가 친 프롬프트의 배경 띠: `❯` 앞머리 + 행 전폭 균일 배경만 띠다.
    // 입력박스 `❯`(배경 없음)·코드블록(❯ 없음)·메뉴 선택(부분 폭)은 아니다.
    #[test]
    fn user_prompt_band_detects_full_width_only() {
        use kasa_bridge::screen::Color;
        let band = Color::Rgb(240, 240, 240);
        let banded = |s: &str| {
            let mut row = row_from(s, 60);
            for c in row.iter_mut() {
                c.bg = band.clone();
            }
            row
        };
        assert_eq!(user_prompt_band(&banded("❯ 내가 친 프롬프트")), Some(band.clone()));
        assert_eq!(user_prompt_band(&row_from("❯ 입력박스 줄", 60)), None, "배경 없으면 아니다");
        assert_eq!(user_prompt_band(&banded("  코드블록 줄")), None, "❯ 없으면 아니다");
        let mut partial = row_from("❯ 1. 메뉴 선택", 60);
        for c in partial.iter_mut().take(20) {
            c.bg = band.clone();
        }
        assert_eq!(user_prompt_band(&partial), None, "부분 폭 강조는 아니다");
    }

    // 재도색: 본문 폭까지만 fill, 전폭 띠의 꼬리는 기본 배경으로, ❯ 는 accent.
    #[test]
    fn restyle_user_prompt_trims_tail_and_paints_marker() {
        use kasa_bridge::screen::Color;
        let mut row = row_from("❯ 질문", 40);
        for c in row.iter_mut() {
            c.bg = Color::Rgb(240, 240, 240);
        }
        let fill = Color::Rgb(30, 34, 44);
        restyle_user_prompt_row(&mut row, &fill, [255, 128, 0, 255]);
        assert_eq!(row[0].fg, Color::Rgb(255, 128, 0), "❯ 는 accent");
        assert_eq!(row[3].bg, fill, "본문 구간은 fill");
        assert_eq!(row[30].bg, Color::Default, "꼬리는 기본 배경으로");
    }

    // 여러 문단 SendMessage: 문단 사이 빈 행은 메시지 끝이 아니다 — 빈 행 뒤
    // 첫 non-blank 가 여전히 연속 행이면 계속, 구조 글리프(⏺ 등)면 거기서 끝.
    // 빈 행에서 무조건 끊던 시절엔 첫 문단만 학생색이 입혀졌다(2026-08-15 신고).
    #[test]
    fn paragraph_gap_continues_multiparagraph_message() {
        let rows: Vec<Vec<GridCell>> = vec![
            row_from("  좋은 지적이다. (나) 로 간다", 80),
            row_from("", 80),
            row_from("  응답 형식은 이렇게 맞춰라.", 80),
            row_from("", 80),
            row_from("⏺ 다음 블록", 80),
        ];
        assert!(msg_paragraph_gap(&rows, 1), "문단 구분 빈 행은 계속");
        assert!(!msg_paragraph_gap(&rows, 3), "다음 블록 앞 빈 행은 끝");
        assert!(!msg_paragraph_gap(&rows, 0), "본문 행 자체는 gap 이 아니다");
    }

    // 인라인 재작성(학생 발신): 프사는 본문 위 행(호출측 이미지 패스)이고 첫 줄은
    // `› ` + 본문 — 그 폭이 이어 쓰는 줄의 들여쓰기와 같아 좌측이 한 줄로 선다.
    // 이름 텍스트는 프사가 대신하고 원문 "› Message from @…" 잔재는 지워진다.
    #[test]
    fn restyle_writes_inline_body_with_face_for_student() {
        let mut rows = vec![row_from("› Message from @aru-9c88", 60)];
        let face =
            expand_teammate_message(&mut rows, 0, 0, "aru-9c88", Some("아루다 확인"), [255, 128, 0, 255]);
        assert_eq!(face, Some(0), "프사 col 반환");
        assert_eq!(rows[0][0].ch, ' ', "첫 두 칸은 프사 자리");
        assert_eq!(rows[0][2].ch, '아', "본문은 이어 쓰는 줄과 같은 col 2");
        assert_eq!(
            rows[0][2].fg,
            kasa_bridge::screen::Color::Rgb(255, 128, 0),
            "학생 accent 로 도색"
        );
        let text = row_text(&rows[0]);
        assert!(text.contains("아 루 다"), "본문 와이드 스페이서 유지: {text}");
        assert!(!text.contains("aru-9c88"), "이름은 프사가 대신: {text}");
    }

    // 학생이 아닌 발신자(team-lead 등)는 프사가 없어 기존 `@이름❯` 헤더 유지.
    #[test]
    fn restyle_keeps_name_header_for_non_student() {
        let mut rows = vec![row_from("› Message from @team-lead", 60)];
        let face =
            expand_teammate_message(&mut rows, 0, 0, "team-lead", Some("확인"), [255, 128, 0, 255]);
        assert_eq!(face, None, "프사 없음");
        assert!(row_text(&rows[0]).starts_with("@ team-lead❯"), "{}", row_text(&rows[0]));
    }

    // 이어 쓸 blank 행이 없으면 말줄임으로 끝난다 — 다음 항목 침범 없음.
    #[test]
    fn restyle_truncates_with_ellipsis() {
        let mut rows = vec![
            row_from("› Message from @aru-9c88", 24),
            row_from("다음 항목", 24),
        ];
        expand_teammate_message(
            &mut rows, 0, 0, "aru-9c88",
            Some("긴 본문이 한 줄을 넘겨 잘린다"),
            [255, 0, 0, 255],
        );
        let text = row_text(&rows[0]);
        assert!(text.ends_with('…'), "{text}");
        assert_eq!(row_text(&rows[1]), "다음 항목", "다음 항목 무손상");
    }

    // 아래 blank 행이 있으면 줄바꿈으로 이어 쓴다(거노) — 다음 항목과의
    // 구분 blank 1행은 남긴다.
    #[test]
    fn expands_into_blank_rows_keeping_separator() {
        let mut rows = vec![
            row_from("› Message from @aru-9c88", 24),
            row_from("", 24),
            row_from("", 24),
            row_from("", 24),
            row_from("다음 항목", 24),
        ];
        expand_teammate_message(
            &mut rows, 0, 0, "aru-9c88",
            Some("긴 본문이 여러 줄에 걸쳐 이어진다"),
            [255, 0, 0, 255],
        );
        assert!(row_text(&rows[0]).contains('긴'), "{}", row_text(&rows[0]));
        // 이어 쓴 줄은 2칸 들여쓰기 + 학생색.
        assert!(row_text(&rows[1]).starts_with("  "), "{}", row_text(&rows[1]));
        assert!(!row_is_blank(&rows[1]));
        assert_eq!(
            rows[1].iter().find(|c| c.ch != ' ').unwrap().fg,
            kasa_bridge::screen::Color::Rgb(255, 0, 0)
        );
        // usable = blank_run(3) - 1 → 마지막 blank 는 구분용으로 남는다.
        assert!(row_is_blank(&rows[3]), "구분 blank 유지");
        assert_eq!(row_text(&rows[4]), "다음 항목", "다음 항목 무손상");
    }

    // 뷰포트 바닥까지 전부 빈 경우엔 구분행 없이 끝까지 쓴다.
    #[test]
    fn expands_to_viewport_bottom_without_separator() {
        let mut rows = vec![
            row_from("› Message from @aru-9c88", 24),
            row_from("", 24),
            row_from("", 24),
        ];
        expand_teammate_message(
            &mut rows, 0, 0, "aru-9c88",
            Some("본문이 바닥까지 이어져 내려간다 아주 길게 계속"),
            [255, 0, 0, 255],
        );
        assert!(!row_is_blank(&rows[1]));
        assert!(!row_is_blank(&rows[2]), "바닥 행까지 사용");
    }

    // 본문 회수 실패 시엔 원문 유지 + 색만.
    #[test]
    fn restyle_without_body_recolors_only() {
        let mut rows = vec![row_from("› Message from @aru-9c88", 40)];
        expand_teammate_message(&mut rows, 0, 0, "aru-9c88", None, [0, 255, 0, 255]);
        assert_eq!(row_text(&rows[0]), "› Message from @aru-9c88");
        assert_eq!(rows[0][0].fg, kasa_bridge::screen::Color::Rgb(0, 255, 0));
    }

    // 셀 폭 word-wrap: 첫 줄/이후 줄 폭 분리, 와이드 2칸, 긴 단어 글자 분할.
    #[test]
    fn wrap_body_cells_widths_and_split() {
        let (lines, trunc) = wrap_body_cells("가나 다라 마바", 6, 6, 10);
        // "가나"(4)+" "+"다라" = 9 > 6 → 줄마다 한 단어.
        assert_eq!(lines, vec!["가나", "다라", "마바"]);
        assert!(!trunc);
        let (lines, trunc) = wrap_body_cells("가나다라", 4, 4, 10);
        assert_eq!(lines, vec!["가나", "다라"], "긴 단어 글자 분할");
        assert!(!trunc);
        let (lines, trunc) = wrap_body_cells("하나 둘 셋 넷", 4, 4, 2);
        assert_eq!(lines.len(), 2);
        assert!(trunc, "max_lines 초과분은 잘림 표시");
    }

    // agent 이름 로마자 → 로스터 역매핑, 로스터 밖은 태그 color 폴백.
    #[test]
    fn sender_accent_roster_and_fallback() {
        assert_eq!(theme::slug_character("aru"), Some("아루"));
        assert_eq!(
            teammate_sender_accent("aru-9c88", None),
            theme::character_accent("아루").unwrap()
        );
        assert_eq!(
            teammate_sender_accent("team-lead", Some("orange")),
            [228, 140, 60, 255]
        );
    }
}


#[cfg(test)]
mod sticky_seek_tests {
    use super::*;

    fn set_pills(entries: &[(&str, &str)]) {
        STICKY_PILLS.with(|s| {
            *s.borrow_mut() = entries
                .iter()
                .map(|(id, text)| (id.to_string(), (0.0, 0.0, 10.0, 10.0), text.to_string()))
                .collect();
        });
    }

    // 클릭 → seek 시작, 첫 스텝은 그 pane 에 wheel-up 노치(클릭 셀)를 쏜다.
    #[test]
    fn first_step_emits_notch() {
        set_pills(&[("%1", "이전 프롬프트")]);
        begin_sticky_seek("%1".into(), "이전 프롬프트".into(), (5, 7), false);
        assert!(sticky_seek_active());
        assert_eq!(sticky_seek_step(), Some(("%1".to_string(), 5, 7, false)));
        assert!(sticky_seek_active()); // 아직 진행 중
    }

    // 스로틀: 방금 노치 직후 재호출은 대기(None)하되 seek 은 살아있다(리페인트 대기).
    #[test]
    fn throttled_between_notches() {
        set_pills(&[("%1", "T")]);
        begin_sticky_seek("%1".into(), "T".into(), (1, 1), false);
        assert!(sticky_seek_step().is_some()); // 첫 노치
        assert_eq!(sticky_seek_step(), None); // 간격 내 재호출 → 대기
        assert!(sticky_seek_active());
    }

    // sticky 텍스트가 target 과 달라지면(타깃이 뷰포트로 들어옴) 종료·상태 클리어.
    #[test]
    fn stops_when_target_enters_view() {
        set_pills(&[("%1", "타깃")]);
        begin_sticky_seek("%1".into(), "타깃".into(), (1, 1), false);
        set_pills(&[("%1", "더 이전 프롬프트")]); // sticky 가 이전 프롬프트로 교체됨
        assert_eq!(sticky_seek_step(), None);
        assert!(!sticky_seek_active());
    }

    // sticky 가 사라지면(최상단 도달) 종료.
    #[test]
    fn stops_when_sticky_gone() {
        set_pills(&[("%1", "타깃")]);
        begin_sticky_seek("%1".into(), "타깃".into(), (1, 1), false);
        set_pills(&[]); // 최상단 — pill 없음
        assert_eq!(sticky_seek_step(), None);
        assert!(!sticky_seek_active());
    }
}

#[cfg(test)]
mod prompt_box_tests {
    use super::*;

    fn row_from(s: &str) -> Vec<GridCell> {
        s.chars()
            .map(|c| {
                let mut cell = GridCell::blank();
                cell.ch = c;
                cell
            })
            .collect()
    }

    // 「ultracode」 라벨은 숨 위상과 무관하게 항상 위보더에 박힌다(2026-08-17
    // 「숨쉬기할때 그냥 안없어지게」 — 반주기마다 깜빡이던 옛 동작 반려).
    #[test]
    fn ultracode_label_is_always_inlaid() {
        let mk = || {
            vec![
                row_from(&"─".repeat(60)),
                row_from(&format!("❯ hi{}", " ".repeat(56))),
                row_from(&"─".repeat(60)),
            ]
        };
        let mut rows = mk();
        overlay_ultracode_label(&mut rows);
        let text: String = rows[0][2..11].iter().map(|c| c.ch).collect();
        assert_eq!(text, "ultracode");
        // 라벨 자리에 실물 글자(@칩 등)가 있으면 지우지 않는다.
        let mut rows = mk();
        rows[0][4].ch = '@';
        overlay_ultracode_label(&mut rows);
        assert_eq!(rows[0][4].ch, '@');
    }

    // 숨쉬기 색: 골(sin=-1)에서 학생색 그대로, 마루(sin=+1)에서 보라 — 학생색을
    // 잃지 않는 순환이 규칙이다(2026-08-17). 미배정 pane 은 보라 밝기 숨쉬기.
    #[test]
    fn breath_cycles_between_student_color_and_purple() {
        let student = [0x10u8, 0x20, 0x30, 255];
        // sin(t*2.2) = -1 근처: t = (3π/2)/2.2
        let trough = (3.0 * std::f32::consts::PI / 2.0) / 2.2;
        let peak = (std::f32::consts::PI / 2.0) / 2.2;
        assert_eq!(ultracode_breath(Some(student), trough), student);
        assert_eq!(ultracode_breath(Some(student), peak), [0xbb, 0x9a, 0xf7, 255]);
        assert_eq!(ultracode_breath(None, peak), ultracode_accent(peak));
    }

    // 진짜 claude 입력박스: 대시줄 사이 ❯ 마커행 → 감지된다(실제 composed 는
    // 모서리·세로선 없는 순수 대시줄이라 box_rows 와 동일 형태).
    #[test]
    fn real_prompt_box_detected() {
        let rows = vec![
            row_from("some output above"),
            row_from(&"─".repeat(28)),
            row_from(&format!("❯ hello{}", " ".repeat(21))),
            row_from(&"─".repeat(28)),
        ];
        assert!(matches!(
            prompt_box(&rows),
            Some(PromptBox::Bordered { ref rows, top: 1, bottom: 3 }) if *rows == (2..3)
        ));
    }

    // ultracode accent(미배정 pane 의 숨쉬기) — 학생 accent 와 **확실히 구분**돼야
    // 의미가 있다. 실기에서 테두리가 학생 분홍(d55580)으로 나온 적이 있어(덮어쓰기)
    // 색 순서(B>R>G)를 본다.
    #[test]
    fn ultracode_accent_stays_purple_across_the_breath() {
        let mut seen_dim = false;
        let mut seen_bright = false;
        for i in 0..80 {
            let t = i as f32 * 0.05;
            let [r, g, b, a] = ultracode_accent(t);
            assert_eq!(a, 255);
            // 언제나 보라 — 파랑이 가장 세고 초록이 가장 약하다. 이 순서가 깨지면
            // 학생 accent(분홍 계열: R 강, G 약, B 중간)와 헷갈린다.
            assert!(b > r && r > g, "보라가 아니다: {r},{g},{b} (t={t})");
            // 원색(bb9af7)보다 어두워지지 않는다 — 밝은 쪽으로만 숨쉰다.
            assert!(r >= 0xbb && g >= 0x9a && b >= 0xf7, "원색보다 어둡다: {r},{g},{b}");
            // 초록이 가장 크게 흔들리는 채널이다(0x9a 에서 시작해 흰빛이 가장 많이
            // 섞인다) — 실측 범위 154~188 의 양 끝을 각각 지나는지 본다.
            if g < 0xa0 {
                seen_dim = true;
            }
            if g > 0xb4 {
                seen_bright = true;
            }
        }
        // 실제로 숨쉬어야 한다 — 상수면 정적인 테두리와 구분이 안 된다.
        assert!(seen_dim && seen_bright, "밝기가 오르내리지 않는다");
    }

    // codex 입력줄: 보더가 없고 **줄 전체가 명시 배경색**이다(실측 bg=Rgb(63,69,77)).
    // 배경 없이 `›` 만 보면 인용문을 입력창으로 오인하므로 둘을 함께 요구한다.
    #[test]
    fn codex_filled_prompt_row_detected() {
        let filled = |s: &str| {
            let mut r = row_from(s);
            for c in r.iter_mut() {
                c.bg = kasa_bridge::screen::Color::Rgb(63, 69, 77);
            }
            r
        };
        let rows = vec![
            row_from("⚠ MCP startup incomplete"),
            filled("› Use /skills to list available skills"),
            row_from("gpt-5.5 medium · tmuxify · main · Context 0% used"),
        ];
        assert!(matches!(prompt_box(&rows), Some(PromptBox::Filled { ref rows }) if *rows == (1..2)));

        // 실제 codex 는 마커 행 위아래에 같은 채움색 여백 행을 둔다(실측 3줄).
        // 마커 행만 잡으면 가운데 한 줄만 칠해져 상자가 아니라 밑줄이 된다(거노).
        let boxed = vec![
            row_from("⚠ MCP startup incomplete"),
            filled(&" ".repeat(50)),
            filled("› Use /skills to list available skills"),
            filled(&" ".repeat(50)),
            row_from("gpt-5.5 medium · tmuxify · main · Context 0% used"),
        ];
        assert!(
            matches!(prompt_box(&boxed), Some(PromptBox::Filled { ref rows }) if *rows == (1..4)),
            "여백 행까지 한 상자로"
        );
        // 같은 줄이라도 배경이 없으면 입력창이 아니다 — 인용문 오인 방지.
        let plain = vec![row_from("› quoted line, not an input box at all")];
        assert!(prompt_box(&plain).is_none());
    }

    /// codex 학생은 입력행 **바로 위**에 선다. claude 처럼 statusline 자리표시자
    /// (U+FFFC)에서 출발할 수 없어서다 — `[tui] status_line` 은 정해진 세그먼트
    /// 이름 배열이라 모르는 항목을 넣으면 `Ignored invalid status line item` 으로
    /// 버려진다(0.146.0 실측). 앵커가 입력행에 직접 매이는지 못박는다.
    #[test]
    fn codex_student_stands_on_the_row_above_the_input() {
        let filled = |s: &str| {
            let mut r = row_from(s);
            for c in r.iter_mut() {
                c.bg = kasa_bridge::screen::Color::Rgb(63, 69, 77);
            }
            r
        };
        let rows = vec![
            row_from("⚠ MCP startup incomplete"),
            row_from(""),
            filled(&" ".repeat(50)),
            filled("› Write tests for @filename"),
            filled(&" ".repeat(50)),
            row_from("gpt-5.5 medium · tmuxify · main · Context 0% used"),
        ];
        let (anchor, left_c) = find_filled_standing_anchor(&rows, 80).expect("앵커");
        assert_eq!(anchor, 1, "여백 행까지 포함한 상자(2..5) 바로 위");
        // 앵커 행이 비어 있으면 오른쪽 끝에 선다.
        assert!((left_c - (80.0 - 1.0 - STAND_CELLS)).abs() < f32::EPSILON);

        // 입력창을 못 찾으면 아무 데도 안 세운다 — 빈 화면에 학생이 뜨는 회귀 방지.
        assert!(find_filled_standing_anchor(&[row_from("just text")], 80).is_none());

        // 입력행이 첫 줄이면(스크롤로 위가 잘림) 설 자리가 없다.
        let top = vec![filled("› Write tests for @filename")];
        assert!(find_filled_standing_anchor(&top, 80).is_none());
    }

    // diff·git·노트 TUI 의 대시 구분선 쌍은 사이에 ASCII '>'(인용·프롬프트)가
    // 있어도 입력박스로 오인하지 않는다 — 거노 2026-07-22: 뜬금없는 빈 초록
    // 사각형(style_prompt_box 오발동) 회귀 방지.
    #[test]
    fn plain_dash_rules_ignored() {
        let rows = vec![
            row_from("web/public/cast/ += 캐릭터 12장"),
            row_from(&"─".repeat(30)),
            row_from(" > some diff line here"),
            row_from(&"─".repeat(30)),
            row_from("Notes: press n to add notes"),
        ];
        assert!(prompt_box(&rows).is_none());
    }
}

#[cfg(test)]
mod cross_session_msg_tests {
    use super::*;

    /// 실제 transcript 에 박히는 원문 그대로(2026-08-09 채집).
    const REAL: &str = "Another Claude session sent a message:\n\
<cross-session-message from=\"uds:/tmp/cc-socks/27516.sock\" from-name=\"타이틀 생성 푸시\" from-mode=\"bypass\">\n\
ROUNDTRIP-OK\n\
</cross-session-message>\n\
This came from another Claude session";

    fn row_from(s: &str, cols: usize) -> Vec<GridCell> {
        let mut row = vec![GridCell::blank(); cols];
        for (i, c) in s.chars().enumerate() {
            row[i].ch = c;
        }
        row
    }

    /// ★프리필터 회귀 — 이 레벨 테스트가 없어서 272c508 이 라이브에서 죽은 채
    /// 통과했다. extract 직접 호출이 아니라 **latest_teammate_msg** 를 지나야 한다.
    #[test]
    fn latest_teammate_msg_passes_cross_session_lines() {
        let dir = std::env::temp_dir().join("kasaterm-prefilter-test");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("t.jsonl");
        let line = serde_json::json!({
            "type": "user",
            "message": { "role": "user", "content": REAL }
        });
        std::fs::write(&path, format!("{line}
")).unwrap();
        let m = latest_teammate_msg(&path, PEER_LABEL)
            .expect("cross-session 줄이 프리필터에서 걸러졌다");
        assert_eq!(m.body, "ROUNDTRIP-OK");
        assert_eq!(m.from_label.as_deref(), Some("타이틀 생성 푸시"));
        let _ = std::fs::remove_file(&path);
    }

    /// ★2026-08-18 실측 회귀 — claude 2.1.234 는 cross-session 배달을
    /// `type:"user"` + `message.content` 가 아니라 **`type:"attachment"` +
    /// `attachment.prompt`** 로 적는다(그 줄엔 `message` 필드가 아예 없다).
    /// 두 게이트가 나란히 떨어뜨려 SendMessage 의 프사·학생색이 통째로 안 붙었다.
    /// 검체는 실제 transcript 에서 그대로 떴다 — 축약하면 다음 형식 변경도 또 놓친다.
    #[test]
    fn latest_teammate_msg_passes_queued_command_attachments() {
        let dir = std::env::temp_dir().join("kasaterm-attach-test");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("t.jsonl");
        let line = serde_json::json!({
            "type": "attachment",
            "userType": "external",
            "attachment": {
                "type": "queued_command",
                "prompt": "<cross-session-message from=\"uds:/tmp/cc-socks/13455.sock\" from-name=\"yuuka-p18-ly2\" from-mode=\"bypass\">\n두번째확인\n</cross-session-message>",
                "origin": { "kind": "peer", "name": "yuuka-p18-ly2" }
            }
        });
        std::fs::write(&path, format!("{line}\n")).unwrap();
        let m = latest_teammate_msg(&path, PEER_LABEL)
            .expect("배달 attachment 가 프리필터에서 걸러졌다 — 테마가 통째로 안 붙는다");
        assert_eq!(m.body, "두번째확인");
        // 화면 라벨(`@ yuuka-p18-ly2❯`)과 대조되는 값 — 이게 비면 label_hit 가 실패해
        // 파싱이 됐어도 렌더가 그 줄을 건너뛴다.
        assert_eq!(m.from_label.as_deref(), Some("yuuka-p18-ly2"));
        let _ = std::fs::remove_file(&path);
    }

    /// assistant 가 프로즈에 태그 문자열을 인용해도(수신 확인 답변) 역스캔이
    /// 그걸 최신 메시지로 잡으면 안 된다 — 진짜 user 배달이 가려진다(2026-08-12
    /// 실측: 인용 하나로 다음 배달의 테마가 통째로 안 걸렸다).
    #[test]
    fn assistant_prose_quoting_the_tag_does_not_shadow_delivery() {
        let dir = std::env::temp_dir().join("kasaterm-prefilter-test");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("shadow.jsonl");
        let user = serde_json::json!({
            "type": "user",
            "message": { "role": "user", "content": REAL }
        });
        let quote = serde_json::json!({
            "type": "assistant",
            "message": { "role": "assistant", "content": [{
                "type": "text",
                "text": "<cross-session-message>에는 머리말만 실려 오고 본문이 없다"
            }] }
        });
        std::fs::write(&path, format!("{user}\n{quote}\n")).unwrap();
        let m = latest_teammate_msg(&path, PEER_LABEL)
            .expect("assistant 인용이 진짜 배달을 가렸다");
        assert_eq!(m.body, "ROUNDTRIP-OK");
        let _ = std::fs::remove_file(&path);
    }

    /// 발신 세션에 제목이 없으면 `from-name` 이 통째로 빠진다(2026-08-12 실측:
    /// 신생 pane 의 `@ 12889❯`) — 그때는 소켓 pid 가 보조 앵커로 남아야 한다.
    #[test]
    fn cross_session_without_from_name_keeps_pid_anchor() {
        let dir = std::env::temp_dir().join("kasaterm-prefilter-test");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("nofromname.jsonl");
        // pid 는 명부에 없을 값으로 — 있으면 이 머신의 실세션 신원이 섞여 든다.
        let content = "<cross-session-message from=\"uds:/tmp/cc-socks/99999912.sock\" \
                       hop-chain=\"abc\">\nPID-ANCHOR-OK\n</cross-session-message>";
        let line = serde_json::json!({
            "type": "user",
            "message": { "role": "user", "content": content }
        });
        std::fs::write(&path, format!("{line}\n")).unwrap();
        let m = latest_teammate_msg(&path, PEER_LABEL)
            .expect("from-name 없는 cross-session 태그를 놓쳤다");
        assert_eq!(m.body, "PID-ANCHOR-OK");
        assert_eq!(m.from_label, None, "없는 속성이 빈 값으로 잡히면 안 된다");
        assert_eq!(m.from_pid.as_deref(), Some("99999912"));
        let _ = std::fs::remove_file(&path);
    }

    /// claude v2.1.228 펼침 헤더 — `@ <라벨>❯` + 다음 행 2칸 들여쓴 본문.
    #[test]
    fn native_expanded_header_detected() {
        let row = row_from("@ sendmessage로 7유저에게 메시지 전송❯", 60);
        let (c0, qcol, label) =
            peer_native_header_line(&row).expect("펼침 헤더를 못 잡았다");
        assert_eq!(c0, 0);
        assert_eq!(row[qcol].ch, '❯');
        assert_eq!(label, "sendmessage로 7유저에게 메시지 전송");
    }

    /// 재작성된 헤더 이름은 bold 다 — 본문도 같은 학생색이라 색만으로는 이름이
    /// 안 튄다(2026-08-12 지시 「이름은 bold로」).
    #[test]
    fn restyled_header_name_is_bold() {
        let mut row = row_from("@ 12889❯", 40);
        let (c0, qcol, _) = peer_native_header_line(&row).unwrap();
        restyle_peer_native_header(&mut row, c0, qcol, "후부키", [0xa9, 0xd5, 0xea, 255]);
        // 한글 wide 셀 뒤엔 스페이서(' ')가 실리므로 공백 무시 대조.
        let text: String =
            row.iter().map(|c| c.ch).filter(|c| !c.is_whitespace() && *c != '\0').collect();
        assert!(text.ends_with("❯"), "❯ 유지: {text:?}");
        assert!(text.contains("후부키"), "이름 재작성: {text:?}");
        for (i, c) in row.iter().enumerate() {
            if c.ch != ' ' && c.ch != '\0' {
                assert!(c.bold, "col {i} ({:?}) 가 bold 아님", c.ch);
                assert_eq!(
                    c.fg,
                    kasa_bridge::screen::Color::Rgb(0xa9, 0xd5, 0xea),
                    "col {i} accent"
                );
            }
        }
    }

    /// `❯` 없이 '@' 로 시작하는 행(사용자 입력·멘션)은 잡으면 안 된다.
    #[test]
    fn native_header_rejects_plain_at_lines() {
        assert!(peer_native_header_line(&row_from("@ 아루 봐줘", 40)).is_none());
        assert!(peer_native_header_line(&row_from("@아루", 40)).is_none());
        // '❯' 뒤에 딴 글자가 이어지면 본문 인용이다.
        assert!(
            peer_native_header_line(&row_from("@ 제목❯ 이어지는 말", 40)).is_none()
        );
    }

    /// 완료 보고 줄(`[완료] 미도리(%4) — …`)은 두 스페이서 경로('\0'/' ') 모두에서
    /// 캐릭터명을 뽑아야 한다 — 색칠이 이 이름으로 학생을 찾는다.
    #[test]
    fn done_report_line_survives_wide_spacers() {
        // 실제 그리드처럼 wide 글리프 뒤에 스페이서 셀을 끼운 행.
        let wide = |s: &str, spacer: char| -> Vec<GridCell> {
            let mut out = Vec::new();
            for c in s.chars() {
                let mut cell = GridCell::blank();
                cell.ch = c;
                out.push(cell);
                if (c as u32) >= 0x1100 {
                    let mut sp = GridCell::blank();
                    sp.ch = spacer;
                    out.push(sp);
                }
            }
            out
        };
        for spacer in ['\0', ' '] {
            let row = wide("[완료] 미도리(%4) — 판독 24장 완료", spacer);
            assert_eq!(done_report_line(&row).as_deref(), Some("미도리"), "spacer {spacer:?}");
        }
        // 큐잉(>)·제출(❯) 마커 뒤에서도, 요약이 없어도 잡힌다.
        assert_eq!(done_report_line(&row_from("❯ [완료] 유즈(%12)", 40)).as_deref(), Some("유즈"));
        assert_eq!(done_report_line(&row_from("> [실패] 아루(%3) — 빌드 깨짐", 40)).as_deref(), Some("아루"));
        // 캐릭터 없이 pane id 만 남은 옛 형식 — 이름이 %4 로 나온다(색칠부가
        // 로스터 조회 실패로 원색 유지).
        assert_eq!(done_report_line(&row_from("[완료] %4(%4) — x", 40)).as_deref(), Some("%4"));
        // 오탐 방지 — (%N) 괄호가 없거나 [완료] 로 안 시작하면 안 잡는다.
        assert_eq!(done_report_line(&row_from("[완료] 미도리 — 괄호 없음", 40)), None);
        assert_eq!(done_report_line(&row_from("[완료] 미도리(4번) — % 없음", 40)), None);
        assert_eq!(done_report_line(&row_from("완료했어요 미도리(%4)", 40)), None);
    }

    /// transcript 대조가 불가능한(발신 pane dismiss·tail 밖) 헤더의 보조 관문 —
    /// 로스터 슬러그 + `-p<번호>` 이름꼴만 통과한다(2026-08-20 거노 스샷 재발 방지).
    #[test]
    fn roster_agent_label_shape_is_narrow() {
        assert!(label_is_roster_agent("midori-p4-v32"));
        assert!(label_is_roster_agent("yuuka-p18-ly2"));
        assert!(label_is_roster_agent("momoi-p98"));
        // 로스터 밖 머리, p<번호> 없음, 사용자가 칠 법한 텍스트 — 전부 거부.
        assert!(!label_is_roster_agent("team-lead"));
        assert!(!label_is_roster_agent("midori-abbb"));
        assert!(!label_is_roster_agent("midori-chan"));
        assert!(!label_is_roster_agent("midori"));
        assert!(!label_is_roster_agent("미도리"));
        assert!(!label_is_roster_agent("mcp, skill사이드바"));
        assert!(!label_is_roster_agent("midori-p"));
    }

    #[test]
    fn peer_label_picks_up_cross_session_body() {
        // 화면엔 `@peer` 로 뜨므로 sender 는 그 라벨이다 — 이름 대조로는 절대 안 걸린다.
        let m = extract_teammate_msg(REAL, PEER_LABEL).expect("본문을 못 뽑았다");
        assert_eq!(m.body, "ROUNDTRIP-OK");
        // color 는 cross-session 태그에 아예 없다.
        assert!(m.color.is_none());
    }

    #[test]
    fn real_sender_comes_from_socket_pid_not_from_name() {
        // from-name 은 세션 이름이라 자동 제목에 덮인다 — 이 검체가 그 실물이다
        // (진짜 발신자는 aru-p107-a2x 인데 「타이틀 생성 푸시」로 실려 왔다).
        // 그래서 되찾기는 소켓 경로의 pid 로만 한다.
        assert_eq!(socket_pid("uds:/tmp/cc-socks/27516.sock"), Some("27516"));
        assert_eq!(socket_pid("uds:/tmp/cc-socks/abc.sock"), None);
        assert_eq!(socket_pid("bridge:whatever"), None);
    }

    /// 명부 파일 실물(2026-08-11 채집, 모모이 pane 의 `~/.claude/sessions/78476.json`).
    /// **`name` 이 세션 제목이다** — agent 이름(`momoi-p98-rv8`)이 아니다.
    const ROSTER_FILE: &str = r#"{
        "pid": 78476,
        "sessionId": "53b6a9c9-b6e8-4f15-87ae-fbf9ee9d5b4b",
        "cwd": "/Users/kasa/Desktop/momewomo/tmuxify",
        "messagingSocketPath": "/tmp/cc-socks/78476.sock",
        "name": "mcp, skill사이드바",
        "status": "idle"
    }"#;

    #[test]
    fn roster_name_is_a_session_title_so_the_slug_must_come_from_elsewhere() {
        let (name, sid) = peer_ident_from_json(ROSTER_FILE);
        assert_eq!(name.as_deref(), Some("mcp, skill사이드바"));
        // 이 이름으로는 학생을 절대 못 찾는다 — 이것이 남의 메시지가 색도 프사도
        // 없이 뜨던 이유였다(거노 2026-08-11: "sm테마는 왜안됐어").
        assert_eq!(teammate_sender_slug("mcp, skill사이드바"), None);
        // 그래서 세션 id 를 같이 들고 온다. 이걸로 pane 을 되짚어 배정 학생을 묻는다.
        assert_eq!(sid.as_deref(), Some("53b6a9c9-b6e8-4f15-87ae-fbf9ee9d5b4b"));
    }

    #[test]
    fn roster_file_missing_fields_are_none_not_empty() {
        // 이름 없는 세션·깨진 파일에서 빈 문자열을 내보내면 그게 발신자 이름이 되어
        // 화면에 `@ ❯` 가 뜬다. 없으면 없다고 해야 위쪽 폴백이 돈다.
        assert_eq!(peer_ident_from_json(r#"{"name":"  ","sessionId":""}"#), (None, None));
        assert_eq!(peer_ident_from_json("{}"), (None, None));
        assert_eq!(peer_ident_from_json("not json at all"), (None, None));
    }

    #[test]
    fn cross_session_msg_carries_the_session_id_for_pane_lookup() {
        // 소켓 pid 27516 의 명부 파일은 이 테스트 머신에 없다 — 그래서 둘 다 None 이고,
        // 그게 맞다(못 찾으면 화면 라벨 그대로 둔다). 여기서 보는 것은 본문 회수가
        // 그 실패에 안 끌려간다는 점이다.
        let m = extract_teammate_msg(REAL, PEER_LABEL).expect("본문을 못 뽑았다");
        assert_eq!(m.body, "ROUNDTRIP-OK");
    }

    #[test]
    fn teammate_tag_still_matches_by_name() {
        // 옛 형식은 이름 대조 그대로 — 쌓인 transcript 를 거슬러 읽을 때 만난다.
        let t = "<teammate-message teammate_id=\"momoi\" color=\"red\">안녕</teammate-message>";
        let m = extract_teammate_msg(t, "momoi").expect("옛 형식이 깨졌다");
        assert_eq!(m.body, "안녕");
        assert_eq!(m.color.as_deref(), Some("red"));
        assert!(m.sender.is_none());
        assert!(extract_teammate_msg(t, "다른사람").is_none());
    }
}


#[cfg(test)]
mod image_ref_tests {
    use super::*;

    fn row_from(s: &str) -> Vec<GridCell> {
        s.chars()
            .map(|c| {
                let mut cell = GridCell::blank();
                cell.ch = c;
                cell
            })
            .collect()
    }

    /// 와이드 문자 뒤에 스페이서 셀이 끼는 실제 그리드 모양.
    fn row_wide(s: &str, spacer: char) -> Vec<GridCell> {
        let mut out = Vec::new();
        for c in s.chars() {
            let mut cell = GridCell::blank();
            cell.ch = c;
            out.push(cell);
            if (c as u32) >= 0x1100 {
                let mut sp = GridCell::blank();
                sp.ch = spacer;
                out.push(sp);
            }
        }
        out
    }

    #[test]
    fn finds_a_plain_reference() {
        let rows = vec![row_from("> [Image #6] 그리고 표 아직 이상해")];
        let got = find_image_refs(&rows);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].n, 6);
        assert_eq!((got[0].row, got[0].col0, got[0].col1), (0, 2, 11));
    }

    #[test]
    fn finds_several_on_one_row_and_two_digit_numbers() {
        let rows = vec![row_from("[Image #1] vs [Image #12]")];
        let got = find_image_refs(&rows);
        assert_eq!(got.iter().map(|r| r.n).collect::<Vec<_>>(), vec![1, 12]);
        assert_eq!((got[1].col0, got[1].col1), (14, 24));
    }

    // 앞에 한글이 있으면 char 인덱스와 셀 col 이 스페이서만큼 어긋난다 — 히트
    // 박스는 셀 기준이라 이게 틀리면 툴팁이 엉뚱한 자리에서 뜬다.
    #[test]
    fn columns_account_for_wide_char_spacers() {
        for spacer in ['\0', ' '] {
            let rows = vec![row_wide("사진 [Image #3]", spacer)];
            let got = find_image_refs(&rows);
            assert_eq!(got.len(), 1, "spacer {spacer:?}");
            // "사진 " = 셀 5칸(한글 2×2 + 공백) → 참조는 col 5 에서 시작.
            assert_eq!((got[0].col0, got[0].col1), (5, 14), "spacer {spacer:?}");
        }
    }

    #[test]
    fn ignores_malformed_or_unrelated_text() {
        let rows = vec![
            row_from("[Image #] 번호 없음"),
            row_from("[Image #7 닫히지 않음"),
            row_from("Image #7] 여는 괄호 없음"),
            row_from("[image #7] 소문자"),
        ];
        assert!(find_image_refs(&rows).is_empty());
    }

    #[test]
    fn reports_the_row_it_sat_on() {
        let rows = vec![row_from("첫 줄"), row_from(""), row_from("[Image #2]")];
        let got = find_image_refs(&rows);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].row, 2);
    }
}

#[cfg(test)]
mod image_block_tests {
    use super::*;

    fn row_from(s: &str) -> Vec<GridCell> {
        s.chars()
            .map(|c| {
                let mut cell = GridCell::blank();
                cell.ch = c;
                cell
            })
            .collect()
    }

    fn grid(lines: &[&str]) -> Vec<Vec<GridCell>> {
        lines.iter().map(|l| row_from(l)).collect()
    }

    // claude code 는 답의 모든 줄 앞에 두 칸을 넣는다 — 왼쪽에 붙어 있다고 보면 안 된다.
    #[test]
    fn finds_an_indented_marker() {
        let g = grid(&["  [[img:/tmp/a.png:12]]", "  .", "  ."]);
        assert_eq!(
            find_image_blocks(&g),
            vec![ImageBlock { row: 0, path: "/tmp/a.png".into(), rows: 12 }]
        );
    }

    // 경로에 콜론이 들어가도 마지막 콜론이 행 수를 가른다.
    #[test]
    fn splits_on_the_last_colon() {
        let g = grid(&["[[img:/tmp/a:b/c.png:8]]"]);
        assert_eq!(find_image_blocks(&g)[0].path, "/tmp/a:b/c.png");
        assert_eq!(find_image_blocks(&g)[0].rows, 8);
    }

    #[test]
    fn ignores_malformed_markers() {
        let g = grid(&[
            "[[img:/tmp/a.png]]",   // 행 수 없음
            "[[img::12]]",          // 경로 없음
            "[[img:/tmp/a.png:0]]", // 자리가 0
            "[[img:/tmp/a.png:12",  // 안 닫힘
            "[img:/tmp/a.png:12]",  // 대괄호 하나
            "[[img:/tmp/a.png:많이]]",
        ]);
        assert!(find_image_blocks(&g).is_empty());
    }

    // 폭주 방지 상한 — 화면에 우연히 큰 숫자가 있어도 그리드를 통째로 지우지 않는다.
    #[test]
    fn rejects_an_absurd_height() {
        let g = grid(&["[[img:/tmp/a.png:9999]]"]);
        assert!(find_image_blocks(&g).is_empty());
    }

    #[test]
    fn finds_several_blocks() {
        let g = grid(&["[[img:a.png:2]]", ".", "글", "[[img:b.png:3]]", ".", ".", "."]);
        let got = find_image_blocks(&g);
        assert_eq!(got.len(), 2);
        assert_eq!((got[0].row, got[1].row), (0, 3));
    }

    // 표식 행과 그 아래 자리 행이 함께 지워져야 그림 뒤로 글자가 안 비친다.
    #[test]
    fn blanks_the_marker_row_and_its_space() {
        let mut g = grid(&["[[img:a.png:3]]", "....", "....", "남는 줄"]);
        let b = find_image_blocks(&g).remove(0);
        blank_image_block(&mut g, &b);
        for r in 0..3 {
            assert!(row_is_blank(&g[r]), "행 {r} 이 안 비었다");
        }
        assert!(!row_is_blank(&g[3]), "자리 밖 행까지 지웠다");
    }

    // 자리가 화면 끝을 넘어가도(스크롤로 잘린 그림) 패닉하지 않는다.
    #[test]
    fn survives_a_block_running_past_the_last_row() {
        let mut g = grid(&["[[img:a.png:40]]", "."]);
        let b = find_image_blocks(&g).remove(0);
        blank_image_block(&mut g, &b);
        assert!(g.iter().all(|r| row_is_blank(r)));
    }
}
