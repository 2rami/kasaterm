//! wgpu 멀티윈도우 기반 — 편집기/파일뷰를 메인 창에서 떼어 별도 OS 창으로 띄운다.
//!
//! chrome.rs 의 보조 창들(session/board/arona 패널·preview)이 전부 wry webview 인 것과
//! 달리, 여기 별도창은 **자체 wgpu Surface(GpuRenderer)** 로 `draw_raw_editor` 를 직접
//! 그린다. 창 하나당 GpuRenderer 하나(자체 device·글리프 아틀라스) — v1 은 공유 device
//! 리팩토링 없이 창마다 새 인스턴스로 간다(아틀라스 중복 수십 MB 는 v1 트레이드오프).
//!
//! 렌더/입력 라우팅이 전부 `AuxWindowKind` match 한 군데를 지나가므로, 나중에
//! `Settings` variant 를 추가할 땐 각 match(render·key·mouse·title)의 새 팔만 채우면 된다.
//!
//! Drop 순서 주의: `AuxWindow.gpu` 는 `window` 보다 **먼저** 드롭돼야 한다 — surface 가
//! 창의 metal layer 를 참조하므로 창이 먼저 해제되면 surface drop 이 use-after-free 다.
//! 그래서 struct 필드 선언에서 `gpu` 를 앞에, `window` 를 맨 뒤에 둔다(필드는 선언 순서로
//! 드롭). GpuRenderer 내부는 `_window`(Arc clone)→`surface` 순이라, gpu drop 시 Arc
//! refcount 만 줄고 실제 Window 는 `aux.window` 가 아직 잡고 있어 살아있다.
use super::*;

/// macOS 창 탭 묶기를 끄고 창을 만든다.
///
/// 시스템 설정이 "탭 선호: 항상"(`AppleWindowTabbingMode=always`)이면 macOS 가 새
/// 창을 기존 창의 **탭으로 합쳐** 버린다. 그러면 pane 을 꺼내도 별도 창이 아니라
/// 탭 한 장이 되고, 그 탭을 떼면 같이 묶인 것들이 전부 딸려 나온다(거노 실측).
///
/// 창마다 다른 `tabbingIdentifier` 를 주는 것으론 **안 막힌다** — 그건 "묶을 짝"만
/// 가릴 뿐 창의 탭 참여 자체는 켜진 채라, 탭바 드래그·창 합치기 경로가 그대로
/// 살아 있다(거노: 새 빌드에서도 여전히 안 떼짐). 꺼야 하는 건 모드 자체다.
/// `NSWindowTabbingMode::Disallowed` 는 시스템 설정과 무관하게 그 창을 탭에서
/// 통째로 뺀다.
pub(crate) fn create_untabbed(
    event_loop: &ActiveEventLoop,
    attrs: WindowAttributes,
) -> Result<Window, winit::error::OsError> {
    let win = event_loop.create_window(attrs)?;
    #[cfg(target_os = "macos")]
    disallow_tabbing(&win);
    Ok(win)
}

/// 이미 만들어진 창의 탭 참여를 끈다. `create_untabbed` 를 못 쓰는 자리(winit 이
/// 만들어 준 창을 나중에 받는 경로)에서 쓴다.
#[cfg(target_os = "macos")]
pub(crate) fn disallow_tabbing(window: &Window) {
    use objc2_app_kit::{NSView, NSWindowTabbingMode};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let Ok(h) = window.window_handle() else { return };
    let RawWindowHandle::AppKit(h) = h.as_raw() else { return };
    // ns_view 는 살아 있는 NSView* 다(창이 이 함수 호출 동안 유지된다). 거기서
    // 얻는 window 는 방금 만든 그 NSWindow.
    unsafe {
        let view: &NSView = h.ns_view.cast().as_ref();
        if let Some(w) = view.window() {
            w.setTabbingMode(NSWindowTabbingMode::Disallowed);
        }
    }
}

/// 터미널·방 별도창을 만든다 — 탭 묶기 없이, OS 신호등 없이, 우리 헤더 자리를
/// 비운 채로. 편집기·설정 창은 본문이 상단 여백을 안 비워 이 경로를 안 쓴다.
fn create_aux_window(
    event_loop: &ActiveEventLoop,
    attrs: WindowAttributes,
) -> Result<Window, winit::error::OsError> {
    let win = create_untabbed(event_loop, with_aux_chrome(attrs))?;
    #[cfg(target_os = "macos")]
    hide_traffic_lights(&win);
    Ok(win)
}

/// OS 신호등(닫기·최소화·최대화) 세 개를 숨긴다.
///
/// 우리 헤더가 이미 같은 일을 한다 — `↶` 가 되돌리기(=닫기), `−` 가 접기다.
/// 둘이 나란히 있으면 같은 기능이 창마다 두 벌이고, 신호등이 헤더 왼쪽 78px 을
/// 붙박이로 먹어 방 이름·학생 칩이 그만큼 밀린다(거노).
///
/// ⚠️ **`with_decorations(false)` 로 하면 안 된다.** 그건 타이틀바 자체를 없애서
/// 창 이동·가장자리 리사이즈까지 통째로 앗아간다(옛 TODO 가 그래서 막혀 있었다).
/// 버튼 세 개만 `isHidden` 으로 지우면 타이틀바는 남아 드래그·리사이즈가 그대로다.
#[cfg(target_os = "macos")]
pub(crate) fn hide_traffic_lights(window: &Window) {
    use objc2_app_kit::{NSView, NSWindowButton};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let Ok(h) = window.window_handle() else { return };
    let RawWindowHandle::AppKit(h) = h.as_raw() else { return };
    unsafe {
        let view: &NSView = h.ns_view.cast().as_ref();
        let Some(w) = view.window() else { return };
        for b in [
            NSWindowButton::CloseButton,
            NSWindowButton::MiniaturizeButton,
            NSWindowButton::ZoomButton,
        ] {
            if let Some(btn) = w.standardWindowButton(b) {
                btn.setHidden(true);
            }
        }
    }
}

/// `(tabbingMode, 이 창이 묶여 있는 탭 수)`. 헤드리스 검증용 — "탭이 안 보인다"는
/// 눈으로 못 재고, 묶였는지는 `tabbedWindows` 가 nil 이 아닌지로만 확실해진다
/// (mode 만 보면 "끄긴 껐는데 이미 묶인 뒤"를 못 가른다).
#[cfg(target_os = "macos")]
pub(crate) fn tabbing_probe(window: &Window) -> (isize, usize) {
    use objc2_app_kit::NSView;
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let Ok(h) = window.window_handle() else { return (-1, 0) };
    let RawWindowHandle::AppKit(h) = h.as_raw() else { return (-1, 0) };
    unsafe {
        let view: &NSView = h.ns_view.cast().as_ref();
        match view.window() {
            Some(w) => (w.tabbingMode().0, w.tabbedWindows().map_or(0, |t| t.len())),
            None => (-1, 0),
        }
    }
}

/// 헤더 오른쪽 버튼 — [파일트리][접기][되돌리기]. 그리면서 히트 rect 을 창에 적어
/// 두므로 그림과 판정이 갈릴 수 없다. 되돌리기는 ⌘W 와 같은 동작이지만, 그
/// 단축키를 모르면 창에 갇힌다.
fn draw_aux_header_btns(a: &mut AuxWindow, w: f32) {
    const B: f32 = AUX_HEADER_BTN;
    const ICON: f32 = 14.0;
    // 트리 버튼은 켜져 있으면 accent 로 남긴다 — 토글은 눌린 상태가 안 보이면
    // 누를 때마다 "이게 켜진 거야 꺼진 거야"를 화면 밖에서 세게 된다.
    let on = a.tree_open;
    let pinned = a.pinned;
    // 파일트리는 **왼쪽**, 창 조작(접기·되돌리기)은 오른쪽. 메인 창 타이틀 스트립이
    // 같은 규칙이고(트리 토글이 맨 왼쪽), 트리 패널 자체가 왼쪽에서 열리니 버튼도
    // 그쪽에 있어야 무엇을 여는 버튼인지가 위치로 읽힌다(거노).
    let btns: [(AuxHeaderBtn, &str, bool); 4] = [
        (AuxHeaderBtn::FileTree, "folder-tree", true),
        (AuxHeaderBtn::Pin, "pin", false),
        (AuxHeaderBtn::Hide, "minus", false),
        // `corner-down-left` 은 에셋에도 gpu 아이콘 표에도 없어 **아무것도 안 그려졌다**
        // — 보이지 않는데 눌리기는 하는 26px 버튼이라, 헤더 오른쪽을 잘못 짚으면 창이
        // 영문 모르게 되돌아간다. 뜻이 같고 실재하는 undo-2 로 바꾼다.
        (AuxHeaderBtn::Dock, "undo-2", false),
    ];
    let n_right = btns.iter().filter(|(_, _, left)| !left).count();
    a.header_btns.clear();
    let (mx, my) = a.cursor_px;
    let mut right_i = 0usize;
    for (kind, icon, left) in btns.into_iter() {
        let bx = if left {
            AUX_HEADER_PAD
        } else {
            let x = w - B * (n_right - right_i) as f32 - AUX_HEADER_PAD;
            right_i += 1;
            x
        };
        let by = (AUX_HEADER_H - B) / 2.0;
        let hov = mx >= bx && mx <= bx + B && my >= by && my <= by + B;
        if hov {
            crate::round_rect(
                &mut a.gpu,
                bx,
                by,
                B,
                B,
                crate::theme::radius_sm(),
                crate::theme::surface_hover(),
            );
        }
        let lit = (kind == AuxHeaderBtn::FileTree && on) || (kind == AuxHeaderBtn::Pin && pinned);
        a.gpu.queue_icon(
            icon,
            bx + (B - ICON) / 2.0,
            by + (B - ICON) / 2.0,
            ICON,
            if lit {
                crate::theme::accent()
            } else if hov {
                crate::theme::text()
            } else {
                crate::theme::text_mute()
            },
        );
        a.header_btns.push((kind, (bx, by, B, B)));
    }
}

/// 마크다운 편집기 별도창 맨 위의 `Rendered | Raw` 띠. 메인 그리드 헤더의 알약과
/// **같은 두 칸**이다(render.rs 의 `is_markdown` 분기) — 별도창엔 이게 없어 새 창으로
/// 연 문서는 읽기 전용이나 마찬가지였다(거노).
///
/// 이 창은 자체 헤더가 없어(OS 타이틀바를 쓴다) 본문 위에 띠를 하나 얹고 그만큼
/// 원점을 민다. 히트 rect 는 `header_btns` 에 넣는다 — 그린 프레임이 곧 클릭 판정이라는
/// 이 파일의 규약을 그대로 따른다.
fn draw_aux_md_bar(a: &mut AuxWindow, w: f32, bar_h: f32) {
    let Some(m) = a.editor() else { return };
    let raw_now = m.raw_mode;
    let modified = m.modified;
    a.header_btns
        .retain(|(k, _)| !matches!(k, AuxHeaderBtn::MdRender | AuxHeaderBtn::MdRaw));
    a.gpu.rect(0.0, 0.0, w, bar_h, crate::theme::surface());
    a.gpu.rect(0.0, bar_h - 1.0, w, 1.0, crate::theme::border());
    let f = 11.0_f32;
    let pad = 8.0_f32;
    let seg_h = bar_h - 8.0;
    let seg_y = (bar_h - seg_h) / 2.0;
    let wr = a.gpu.measure_chrome_text("Rendered", f, false);
    let wraw = a.gpu.measure_chrome_text("Raw", f, false);
    let total = wr + wraw + pad * 4.0;
    let sx0 = (w - total - 10.0).max(10.0);
    // 저장 안 된 편집이 있으면 왼쪽에 점 하나 — Raw 에서 친 것은 Rendered 로 돌아갈
    // 때 디스크에 쓰이므로, 안 돌아가면 안 쓰인다는 걸 알아야 한다.
    if modified {
        a.gpu.draw_text(
            10.0,
            seg_y + (seg_h - f) / 2.0 - 1.0,
            "● 저장 안 됨",
            crate::gpu::DrawOpts {
                font_size: f,
                color: crate::theme::text_dim(),
                bold: false,
                italic: false,
            },
        );
    }
    crate::round_rect(
        &mut a.gpu, sx0, seg_y, total, seg_h,
        crate::theme::radius_sm(), crate::theme::panel_bg(),
    );
    let (mx, my) = a.cursor_px;
    let mut sx = sx0;
    for (label, lw, raw) in [("Rendered", wr, false), ("Raw", wraw, true)] {
        let cell_w = lw + pad * 2.0;
        let active = raw_now == raw;
        let hov = mx >= sx && mx <= sx + cell_w && my >= seg_y && my <= seg_y + seg_h;
        if active {
            crate::round_rect(
                &mut a.gpu, sx, seg_y, cell_w, seg_h,
                crate::theme::radius_sm(), crate::theme::surface_hover(),
            );
        } else if hov {
            crate::round_rect(
                &mut a.gpu, sx, seg_y, cell_w, seg_h,
                crate::theme::radius_sm(), crate::theme::surface_active(),
            );
        }
        a.gpu.draw_text(
            sx + pad,
            seg_y + (seg_h - f) / 2.0 - 1.0,
            label,
            crate::gpu::DrawOpts {
                font_size: f,
                color: if active { crate::theme::text() } else { crate::theme::text_dim() },
                bold: false,
                italic: false,
            },
        );
        let kind = if raw { AuxHeaderBtn::MdRaw } else { AuxHeaderBtn::MdRender };
        a.header_btns.push((kind, (sx, seg_y, cell_w, seg_h)));
        sx += cell_w;
    }
}

/// 커서가 statusline 프사 위면 큰 bust 를 팝업. 메인 창과 같은 그리기 함수를 쓰고,
/// 창 경계 클램프의 위쪽 한계만 이 창의 헤더(`AUX_HEADER_H`)로 준다.
fn paint_aux_face_hover(a: &mut AuxWindow, slots: &crate::render::StudentOverlays, w: f32) {
    // 히트 rect 를 남긴다 — CursorMoved 가 이걸 보고 재렌더를 걸어야 팝업이 뜨고
    // 진다. 그린 프레임이 곧 판정이라는 규약(header_btns·tree_rows)과 같다.
    a.face_rects = slots.faces.iter().map(|(_, _, r)| *r).collect();
    let (mx, my) = a.cursor_px;
    let Some((name, slug, r)) = slots
        .faces
        .iter()
        .find(|(_, _, r)| mx >= r.0 && mx <= r.0 + r.2 && my >= r.1 && my <= r.1 + r.3)
    else {
        return;
    };
    let cell_h = a.gpu.cell_h;
    crate::render::paint_face_popup(&mut a.gpu, name, slug, *r, cell_h, w, AUX_HEADER_H);
}

/// 별도창 왼쪽 파일트리 패널 한 프레임. 메인 창 트리에서 **행 그리기만** 옮겨 왔다
/// — 검색·이름바꾸기·새 파일·빠른파일·드래그·git 배지는 `App.file_tree` 의 편집
/// 상태를 같이 써야 하는데, 그건 메인 창 좌표로 적혀 있어 두 창이 한 상태를 두고
/// 싸운다. 여기선 읽기 전용으로 두고 펼치기·열기만 태운다.
///
/// 그린 행의 히트 rect 을 `a.tree_rows` 에 적어 클릭 판정이 그림과 갈리지 않게 한다.
fn draw_aux_tree(a: &mut AuxWindow, rows: &[AuxTreeRow], h: f32) {
    let w = AUX_TREE_W;
    let top = AUX_HEADER_H;
    a.tree_rows.clear();
    // `surface` 는 팔레트에서 본문(`bg`)보다 **어두운** 색이라, 트리가 판이 아니라
    // 검게 파인 구멍으로 보였다(거노: "파일트리 배경이 검정이야"). 메인 창 사이드바가
    // 쓰는 `panel_bg`(bg↔surface_hover 중간)로 맞춘다 — 대비로 가는 방향이 테마마다
    // 반대라 고정색으로는 여덟 테마를 다 못 맞춘다.
    a.gpu.rect(0.0, top, w, h - top, crate::theme::panel_bg());
    // 트리와 셀 사이 실선 — 배경색이 비슷해 경계가 없으면 글자가 패널 안에서
    // 시작하는 것처럼 읽힌다.
    a.gpu.rect(w - 1.0, top, 1.0, h - top, crate::theme::border());
    let body_h = (h - top).max(0.0);
    let max_scroll = (rows.len() as f32 * AUX_TREE_ROW_H - body_h).max(0.0);
    a.tree_scroll = a.tree_scroll.clamp(0.0, max_scroll);
    let (mx, my) = a.cursor_px;
    const STEP: f32 = 12.0;
    const ISZ: f32 = 15.0;
    for (i, node) in rows.iter().enumerate() {
        let y = top + i as f32 * AUX_TREE_ROW_H - a.tree_scroll;
        // 이 렌더러엔 scissor 가 없다 — 위아래로 벗어난 행은 아예 안 그린다.
        // 안 그러면 헤더 띠와 창 밖으로 글자가 삐져나온다.
        if y + AUX_TREE_ROW_H <= top || y >= h {
            continue;
        }
        let hov = mx >= 0.0 && mx < w && my >= y && my < y + AUX_TREE_ROW_H;
        if hov {
            crate::round_rect(
                &mut a.gpu,
                2.0,
                y,
                w - 6.0,
                AUX_TREE_ROW_H,
                crate::theme::radius_sm(),
                crate::theme::surface_hover(),
            );
        }
        let base_x = 6.0 + node.depth as f32 * STEP;
        if node.is_dir {
            let chev = if node.expanded { "chevron-down" } else { "chevron-right" };
            a.gpu.queue_icon(
                chev,
                base_x,
                y + (AUX_TREE_ROW_H - 11.0) / 2.0,
                11.0,
                if hov { crate::theme::text() } else { crate::theme::text_mute() },
            );
        }
        let icon_x = base_x + 15.0;
        let iy = y + (AUX_TREE_ROW_H - ISZ) / 2.0;
        let icon_col = if node.ignored {
            crate::theme::with_alpha(crate::theme::text_dim(), 0x99)
        } else if hov {
            crate::theme::text()
        } else {
            crate::theme::text_dim()
        };
        if node.is_dir {
            // 메인 창과 같은 규칙 — 레포는 폴더 대신 브랜치 아이콘.
            let ic = if node.is_repo { "git-branch" } else { "folder" };
            a.gpu.queue_icon(ic, icon_x, iy, ISZ, icon_col);
        } else if let Some(ft) = crate::file_icon(&node.name) {
            a.gpu
                .queue_icon_colored(ft, icon_x, iy, ISZ, if node.ignored { 0.35 } else { 0.9 });
        } else {
            a.gpu.queue_icon("file", icon_x, iy, ISZ, icon_col);
        }
        let text_x = icon_x + ISZ + 6.0;
        let font = 12.0_f32;
        let budget = (w - text_x - 6.0).max(0.0);
        let label = crate::render::clip_px(&mut a.gpu, &node.name, font, false, budget);
        let fg = if node.ignored {
            crate::theme::text_mute()
        } else if hov || node.is_dir {
            crate::theme::text()
        } else {
            crate::theme::text_dim()
        };
        a.gpu.draw_text(
            text_x,
            y + (AUX_TREE_ROW_H - font) / 2.0,
            &label,
            gpu::DrawOpts { font_size: font, color: fg, bold: false, italic: false },
        );
        a.tree_rows
            .push((node.path.clone(), node.is_dir, (0.0, y, w, AUX_TREE_ROW_H)));
    }
}

/// 별도 창 헤더 높이. macOS 에선 OS 타이틀바 **자리 위에** 그리므로 메인 창의
/// `TITLE_HEIGHT` 와 같아야 두 창이 같은 앱으로 읽힌다. 그 외 플랫폼은 OS
/// 타이틀바가 따로 있고 이 띠는 그 아래라, 더 얇게 둔다.
#[cfg(target_os = "macos")]
/// 마크다운 편집기 창의 `Rendered | Raw` 띠 높이. 자체 헤더가 없는 창이라(OS
/// 타이틀바를 쓴다) 본문 위에 이 띠를 얹고 그만큼 원점을 민다 — 별도창엔 토글이
/// 아예 없어 새 창으로 연 문서를 고칠 수가 없었다(거노).
const AUX_MD_BAR_H: f32 = 28.0;

const AUX_HEADER_H: f32 = TITLE_HEIGHT;
#[cfg(not(target_os = "macos"))]
const AUX_HEADER_H: f32 = 30.0;

/// 헤더 양끝 여백.
const AUX_HEADER_PAD: f32 = 6.0;
/// 헤더 버튼 한 변.
const AUX_HEADER_BTN: f32 = 26.0;

/// 라벨(방 이름·학생 이름)이 시작되는 x — 왼쪽 파일트리 버튼 바로 오른쪽.
///
/// 전엔 신호등 세 개(`TRAFFIC_LIGHT_WIDTH`)를 피해 78px 을 통째로 비웠다.
/// 이제 그 버튼들을 숨기므로(`hide_traffic_lights`) 그 자리가 우리 것이다 —
/// 라벨이 창 왼쪽으로 붙어 학생 이름이 헤더 한가운데로 밀리지 않는다.
const AUX_HEADER_X: f32 = AUX_HEADER_PAD * 2.0 + AUX_HEADER_BTN;

/// 셀 그리드가 시작되는 y — 헤더 띠 바로 아래.
///
/// 전엔 이 값을 쓰는 곳과 안 쓰는 곳이 갈려 있었다: 셀은 헤더 아래로 내려 그리면서
/// **행 수와 커서 위치는 창 꼭대기 기준**이라, 마지막 줄이 창 밖으로 밀리고 커서가
/// 헤더 위에 찍혔다. 상단 오프셋을 쓰는 자리는 전부 이 상수 하나를 지난다.
const AUX_CELL_TOP: f32 = PANE_INNER_Y + AUX_HEADER_H;

/// 별도창에 메인 창과 같은 크롬 정책을 건다 — macOS 는 OS 타이틀바를 투명하게
/// 비우고 콘텐츠를 그 위까지 끌어올려, **띠 하나**로 합친다.
///
/// 전엔 OS 타이틀바(회색 OS 테마) 바로 밑에 우리 헤더가 또 있어 띠가 두 겹이었다
/// (거노: "상단바가 os테마이고 바로밑에 방 뭐시기"). decorations 를 끄는 대신
/// 투명으로 가는 이유는 창 이동·리사이즈·신호등을 계속 OS 에 맡기기 위해서다 —
/// 끄면 손잡이 없는 창이 남는다.
fn with_aux_chrome(attrs: WindowAttributes) -> WindowAttributes {
    #[cfg(target_os = "macos")]
    {
        use winit::platform::macos::WindowAttributesExtMacOS;
        return attrs
            .with_titlebar_transparent(true)
            .with_title_hidden(true)
            .with_fullsize_content_view(true);
    }
    #[cfg(not(target_os = "macos"))]
    attrs
}

/// 별도창이 담는 내용물. 이 enum 을 match 하는 지점(render/key/mouse/title)의
/// 팔만 채우면 새 창 종류를 꽂을 수 있다. `Settings` 는 데이터를 안 들고 있다 —
/// 설정 상태(`settings_cat`/`set_*`/`students_*`)는 App 이 소유하고 이 창은 뷰라,
/// 렌더는 `aux_render` 가 App 스냅샷으로 `paint_settings` 를 재사용하고 이벤트는
/// `aux_window_event` 가 `settings_click`/`settings_key`/`settings_scroll` 로 위임한다.
pub(crate) enum AuxWindowKind {
    Editor(MarkdownPane),
    Settings,
    /// 터미널 pane 을 별도 OS 창으로 분리(undock). 데이터를 안 들고 `pane_id` 만 —
    /// 셀 그리드/커서는 App.ws 의 그 pane 이 소유하고 메인 루프 `pump_pty_screens` 가
    /// 계속 갱신하므로, 이 창은 `draw_cells` 로 그 스냅샷을 그리는 뷰다. `PtySession`
    /// 은 App.pty 에 그대로 살아 세션이 안 끊긴다(undock 은 레이아웃 트리에서 leaf 만
    /// 빼고 pty·ws.panes 는 유지). 렌더는 `aux_terminal_render`, 이벤트는
    /// `aux_terminal_event` 로 위임(Settings 가 paint_settings 를 재사용하는 것과 동형).
    /// pane 하나를 꺼낸 창. `window` 는 **어느 방에서 나왔는지** — 이게 없으면
    /// 되돌릴 때 원래 방이 아니라 그때 활성 pane 옆에 붙고, 헤더에 소속을 적을
    /// 수도 없다. 방 재배치를 따라 remap 된다(`reorder_window`).
    Terminal { pane_id: String, window: usize },
    /// 방(윈도우) 하나를 통째로 별도 OS 창으로 분리. `Terminal` 이 pane 하나를 보듯
    /// 이건 그 방의 **BSP 트리 전체**를 본다 — pane 여러 개가 자기 자리에 그려진다.
    ///
    /// 트리를 들고 오지 않고 `App.windows[window]` 에 그대로 둔 채 인덱스로 참조한다.
    /// 그래서 되돌리기가 `switch_window(window)` 한 줄이고, info 방 그룹핑과 세션
    /// 저장이 손대지 않아도 맞는다. 대가는 방 재배치 때 인덱스가 흔들리는 것인데,
    /// 그건 `reorder_window` 의 remap 이 이 필드까지 통과시켜 막는다.
    ///
    /// `focus` 는 이 창 안에서 키 입력을 받을 pane. `term_pane_id()` 가 이걸 내주므로
    /// 키·휠·IME 경로는 `Terminal` 것을 그대로 쓴다.
    Room { window: usize, focus: Option<String> },
}

/// 편집기 창의 OS 타이틀 — 파일명(+ dirty ●). doc.path 는 String 이라 Path 로 감싼다.
fn aux_editor_title(m: &MarkdownPane) -> String {
    let name = std::path::Path::new(m.doc.path.as_str())
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("untitled");
    if m.modified {
        format!("● {name}")
    } else {
        name.to_string()
    }
}

/// 별도창 헤더의 버튼.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum AuxHeaderBtn {
    /// 창을 접어 메인 창 하단바 칩으로 보낸다. pane·PTY 는 그대로 살아 있고
    /// 칩을 누르면 같은 창이 다시 선다 — 최소화와 달리 Dock 이 아니라 **일하던
    /// 창 안**으로 들어가므로, 꺼내 둔 것이 몇인지 거기서 한눈에 보인다(거노).
    Hide,
    /// 메인 그리드로 되돌린다(창 닫기·⌘W 와 같은 동작).
    Dock,
    /// 이 창 왼쪽에 파일트리 패널을 연다/닫는다. 창마다 따로 기억한다 — 전역
    /// 설정으로 두면 pane 하나를 크게 보려고 꺼낸 창까지 같이 좁아진다.
    FileTree,
    /// 마크다운을 **렌더 뷰**로 본다. 메인 그리드 헤더의 `Rendered | Raw` 알약과
    /// 같은 것이다 — 별도창엔 이게 없어 새 창으로 연 문서는 고칠 수가 없었다(거노).
    MdRender,
    /// 마크다운을 **원문 편집기**로 연다. 여기서 친 것은 Rendered 로 돌아갈 때
    /// 디스크에 쓰인다(pane 판과 같은 규칙 — `switch_md_mode` 한 벌을 공유한다).
    MdRaw,
    /// 이 창을 다른 앱 위에 고정한다(always-on-top). 별도창을 꺼내는 이유가 대개
    /// "다른 걸 보면서 이걸 곁눈질"이라, 클릭할 때마다 앞으로 끌어올리는 대신
    /// 아예 위에 붙여 둔다. 창마다 따로 기억한다.
    Pin,
}

/// 별도창 파일트리 패널 폭(logical px). 메인 창 컬럼보다 좁다 — 별도창은 대개
/// pane 하나를 크게 보려고 꺼낸 것이라, 트리가 본문을 반이나 먹으면 꺼낸 이유가
/// 사라진다.
const AUX_TREE_W: f32 = 200.0;

/// 파일트리 한 줄 높이(logical px).
const AUX_TREE_ROW_H: f32 = 22.0;

/// 페인트 루프에 넘길 파일트리 한 줄. `App.file_tree.nodes` 를 그대로 빌리면
/// `self.aux_windows` 가변 차용과 겹치므로, 그리기 전에 필요한 것만 떠 온다.
struct AuxTreeRow {
    path: std::path::PathBuf,
    name: String,
    is_dir: bool,
    depth: usize,
    expanded: bool,
    ignored: bool,
    is_repo: bool,
}

pub(crate) struct AuxWindow {
    /// 자체 wgpu 렌더러. `window` 보다 먼저 드롭돼야 하므로 앞에 선언(모듈 doc 참조).
    pub(crate) gpu: gpu::GpuRenderer,
    pub(crate) kind: AuxWindowKind,
    /// 다음 프레임에 다시 그려야 함(입력/리사이즈 후 set). 메인 렌더 루프에 얹지 않고
    /// 이 창 자신의 이벤트에서만 소비한다.
    pub(crate) dirty: bool,
    /// 이 창의 마지막 커서 위치(logical px). 드래그 선택·클릭 캐럿 히트테스트용.
    pub(crate) cursor_px: (f32, f32),
    /// 마우스 드래그 선택 진행 중.
    pub(crate) selecting: bool,
    /// OS 포커스 여부. 캐럿 blink 는 포커스된 창만(불필요한 GPU 낭비 방지).
    pub(crate) focused: bool,
    /// 이 창 편집기의 한글 조합 프리에딧. App.hangul 이 조합하고 드라이버가 이 창 것으로
    /// 스탬프한다 — 창마다 자기 프리에딧을 오버레이(메인창 preedit 과 안 섞임).
    pub(crate) preedit: String,
    /// 마지막으로 OS 창 타이틀에 세팅한 문자열(중복 set_title 회피).
    pub(crate) last_title: String,
    /// 헤드리스 캡처 (deadline, png 경로). 메인 `pending_capture` 의 aux 판 — 자동캡처가
    /// 메인 창만 찍으므로 별도창은 자기 gpu 로 따로 readback 한다.
    pub(crate) pending_capture: Option<(Instant, String)>,
    /// 렌더 뷰(마크다운) 본문 높이. raw 편집기는 줄 수 × 줄높이로 미리 알 수 있지만
    /// 렌더 뷰는 **그려 봐야** 안다(`draw_markdown` 의 반환값) — 휠 clamp 가 한 프레임
    /// 전 값을 쓰는 건 그래서다. 0 이면 아직 안 그렸다는 뜻이라 clamp 를 걸지 않는다.
    pub(crate) md_content_h: f32,
    /// 헤더 버튼의 히트 rect — 그린 프레임이 곧 클릭 판정이라 렌더가 채운다.
    /// 아이콘 자리를 코드 두 곳에 적으면 그림과 판정이 어긋난다.
    pub(crate) header_btns: Vec<(AuxHeaderBtn, (f32, f32, f32, f32))>,
    /// 이 창 왼쪽 파일트리 패널이 열려 있나. 트리 **내용**은 App.file_tree 하나를
    /// 공유한다(같은 루트·같은 펼침 상태) — 창마다 따로 두면 같은 폴더를 창마다
    /// 다시 펼쳐야 하고, 메인 창에서 연 것이 여기 안 보인다.
    pub(crate) tree_open: bool,
    /// 다른 앱 위에 고정(always-on-top)돼 있나. winit 에 "지금 레벨" 을 묻는 API 가
    /// 없어 우리가 기억한다 — 헤더 아이콘 불빛도 이 값으로 켠다.
    pub(crate) pinned: bool,
    /// 트리 세로 스크롤(logical px). 이건 창마다 따로다 — 보는 자리는 창의 것이다.
    pub(crate) tree_scroll: f32,
    /// 트리 행의 히트 rect — 그린 프레임이 곧 클릭 판정(header_btns 와 같은 규약).
    pub(crate) tree_rows: Vec<(std::path::PathBuf, bool, (f32, f32, f32, f32))>,
    /// statusline 프사의 히트 rect — 렌더가 채운다(header_btns 와 같은 규약).
    /// hover 팝업은 매 프레임 재판정이라 그 프레임을 **부를 사람**이 필요한데,
    /// CursorMoved 는 헤더 띠에서만 재렌더를 걸었다. 프사는 그 띠 밖(y≈131)이라
    /// 팝업이 뜨고 지는 프레임을 아무도 안 불렀고, 셸이 출력 중일 때만 PTY wake 에
    /// 얹혀 우연히 떴다(조용하면 안 뜸 — 거노의 "가끔 안 뜬다"가 이것).
    pub(crate) face_rects: Vec<(f32, f32, f32, f32)>,
    /// `window` 는 맨 뒤 — `gpu` 보다 나중에 드롭돼 surface 가 살아있는 창을 참조한다.
    pub(crate) window: Arc<Window>,
}

impl AuxWindow {
    /// 커서가 statusline 프사 위인가 — hover 팝업이 떠야 하는 상태인지.
    fn over_face(&self, at: (f32, f32)) -> bool {
        self.face_rects
            .iter()
            .any(|r| at.0 >= r.0 && at.0 <= r.0 + r.2 && at.1 >= r.1 && at.1 <= r.1 + r.3)
    }

    /// 마크다운 편집기 창 위에 얹히는 `Rendered | Raw` 띠의 높이. md 문서가 아니면 0.
    ///
    /// **본문 좌표는 전부 이 하나를 지나야 한다** — 그리는 곳(렌더 뷰·raw 뷰)과
    /// 클릭을 줄로 되돌리는 곳(`raw_editor_caret_at`)이 같은 값을 안 쓰면, 화면은
    /// 멀쩡한데 클릭만 한 줄씩 어긋난다(이 레포가 반복해 데인 자리라 게이트를 하나로 둔다).
    pub(crate) fn md_bar_h(&self) -> f32 {
        match self.editor() {
            Some(m) if m.is_md_doc => AUX_MD_BAR_H,
            _ => 0.0,
        }
    }

    /// 파일트리가 먹는 폭(logical px). 닫혀 있으면 0.
    fn tree_w(&self) -> f32 {
        if self.tree_open { AUX_TREE_W } else { 0.0 }
    }

    /// 셀 그리드가 시작되는 x — 트리 폭만큼 밀린다. **셀 원점·커서·leaf rect·
    /// cols 계산이 전부 이 하나를 지나야 한다.** 한 군데라도 빼먹으면 트리가 글자
    /// 위에 겹치거나(원점만 밀고 cols 를 안 줄임) 오른쪽이 잘린다(반대).
    fn cell_left(&self) -> f32 {
        PANE_INNER_X + self.tree_w()
    }

    pub(crate) fn editor(&self) -> Option<&MarkdownPane> {
        match &self.kind {
            AuxWindowKind::Editor(m) => Some(m),
            AuxWindowKind::Settings
            | AuxWindowKind::Terminal { .. }
            | AuxWindowKind::Room { .. } => None,
        }
    }
    pub(crate) fn editor_mut(&mut self) -> Option<&mut MarkdownPane> {
        match &mut self.kind {
            AuxWindowKind::Editor(m) => Some(m),
            AuxWindowKind::Settings
            | AuxWindowKind::Terminal { .. }
            | AuxWindowKind::Room { .. } => None,
        }
    }
    /// 키 입력이 갈 pane id. 터미널 창은 그 pane, 방 창은 지금 포커스된 pane.
    /// 둘을 한 값으로 내주는 덕에 키·휠·IME 라우팅이 한 벌로 끝난다.
    pub(crate) fn term_pane_id(&self) -> Option<&str> {
        match &self.kind {
            AuxWindowKind::Terminal { pane_id, .. } => Some(pane_id.as_str()),
            AuxWindowKind::Room { focus, .. } => focus.as_deref(),
            _ => None,
        }
    }
    /// 이 창이 통째로 들고 있는 방 인덱스(방 창일 때만).
    pub(crate) fn room_window(&self) -> Option<usize> {
        match &self.kind {
            AuxWindowKind::Room { window, .. } => Some(*window),
            _ => None,
        }
    }
    /// 창 client 영역의 논리 크기(px).
    fn logical_size(&self) -> (f32, f32) {
        let scale = self.gpu.scale();
        let phys = self.window.inner_size();
        ((phys.width.max(1) as f32) / scale, (phys.height.max(1) as f32) / scale)
    }
    /// 이 창이 표시할 OS 타이틀 — 파일명(+ dirty ●).
    fn title(&self) -> String {
        match &self.kind {
            AuxWindowKind::Editor(m) => aux_editor_title(m),
            AuxWindowKind::Settings => "Settings".to_string(),
            // v1 은 pane id — 프로세스명(vim/claude…) 인레이는 App 만 알아 aux_render 가
            // 더 나은 라벨로 덮어쓸 수 있다(현재는 id 그대로).
            AuxWindowKind::Terminal { pane_id, .. } => pane_id.clone(),
            // 방 이름(window_labels)은 App 만 알아 `aux_room_render` 가 덮어쓴다.
            AuxWindowKind::Room { window, .. } => format!("방 {}", window + 1),
        }
    }
    /// 한 프레임 렌더. 자체 gpu 로 배경 + `draw_raw_editor` 를 그리고 present.
    /// `cursor_on` = 캐럿을 그릴지(blink 위상, 포커스 상태 반영).
    pub(crate) fn render(&mut self, cursor_on: bool) {
        let scale = self.gpu.scale();
        let (w, h) = self.logical_size();
        self.gpu.clear_chrome();
        // 본문 배경 — draw_raw_editor 의 gutter bg(theme::bg)·surface clear 와 동일색이라
        // letterbox 없이 한 판. (clear 색과 겹쳐도 무해.)
        self.gpu.rect(0.0, 0.0, w, h, crate::theme::bg());
        let pe = self.preedit.clone();
        // 본문 원점을 미는 값 — 아래 두 갈래와 클릭 판정이 **같은 값**을 써야 한다.
        let bar = self.md_bar_h();
        if bar > 0.0 {
            draw_aux_md_bar(self, w, bar);
        }
        match &self.kind {
            // 렌더 뷰 — pane 과 같은 `raw_mode` 분기다. 별도창이 늘 raw 였던 건
            // 이 갈래가 없어서지 의도가 아니었다(거노: "별도창으로 보면 렌더뷰가
            // 안 된다"). 본문 높이는 그려 봐야 나오므로 휠이 쓰도록 받아 둔다.
            AuxWindowKind::Editor(m) if !m.raw_mode => {
                let blocks = m.doc.blocks.clone();
                let gen = m.doc.gen;
                let scroll = m.scroll;
                let ch = self.gpu.draw_markdown(&blocks, gen, 0.0, bar, w, h - bar, scroll, None);
                self.md_content_h = ch;
            }
            AuxWindowKind::Editor(m) => {
                let lang = crate::code_lang_for_path(std::path::Path::new(&m.doc.path));
                let sel = m.sel_range();
                // self.gpu(mut) 와 self.kind(shared, m) 은 disjoint 필드라 동시 차용 OK.
                self.gpu.draw_raw_editor(
                    &m.edit_lines,
                    (m.cur_line, m.cur_col),
                    sel,
                    0.0,
                    bar,
                    w,
                    h - bar,
                    m.scroll,
                    m.h_scroll,
                    lang,
                    &pe,
                    cursor_on,
                    // 팝아웃 창엔 아직 찾기 바가 없다(Cmd+F 는 aux 단축키
                    // 경로라 열리지 않는다) — 하이라이트만 켜면 켤 방법이
                    // 없는 표시가 남는다.
                    None,
                    // 자동완성도 같은 이유로 아직 없다 — aux 창은 키를
                    // `aux_insert` 로 받아 팝업 키 경로를 안 지난다. 목록만
                    // 띄우면 고를 수 없는 유령이 된다.
                    None,
                    // 진단은 App 이 들고 있어(`App.lsp`) 이 창에서 못 읽는다.
                    &[],
                    // 접기 UI 는 본 창 거터에만 있다 — 여기선 늘 비어 있다.
                    &[],
                    m.wrap,
                    // 팝아웃 창엔 멀티커서 키 경로가 없다 — 커서를 더할 방법이
                    // 없는데 그리기만 하면 지울 수도 없는 표시가 남는다.
                    &[],
                );
            }
            // Settings/Terminal/Room 창은 App 스냅샷(설정 상태·ws 셀 그리드)이 필요해
            // `aux_render_settings`/`aux_terminal_render`/`aux_room_render` 가 직접
            // 페인트한다 — 이 편집기 전용 render 로는 오지 않는다.
            AuxWindowKind::Settings
            | AuxWindowKind::Terminal { .. }
            | AuxWindowKind::Room { .. } => {}
        }
        let _ = self.gpu.render(&[], scale, 0.0, true);
    }
    /// 캐럿이 보이게 스크롤 보정(가로/세로) — 메트릭은 gpu 레이어가 소유해
    /// draw_raw_editor 와 드리프트하지 않는다.
    fn ensure_caret_visible(&mut self) {
        let (w, h) = self.logical_size();
        let snap = match &self.kind {
            AuxWindowKind::Editor(m) => {
                if !m.raw_mode {
                    return;
                }
                let line = m.cur_line.min(m.edit_lines.len().saturating_sub(1));
                let prefix: String = m
                    .edit_lines
                    .get(line)
                    .map(|l| l.chars().take(m.cur_col).collect())
                    .unwrap_or_default();
                ((*m.edit_lines).clone(), line, prefix, m.scroll, m.h_scroll)
            }
            AuxWindowKind::Settings
            | AuxWindowKind::Terminal { .. }
            | AuxWindowKind::Room { .. } => return,
        };
        let (lines, cur_line, prefix, scroll, h_scroll) = snap;
        let line_count = lines.len();
        let (ns, nh) = self
            .gpu
            .raw_editor_ensure_visible(
                line_count, cur_line, &prefix, w, h, scroll, h_scroll, &[], 0, &lines,
            );
        if let Some(m) = self.editor_mut() {
            m.scroll = ns.max(0.0);
            m.h_scroll = nh.max(0.0);
        }
    }
    /// PageUp/Down 한 스텝의 줄 수(본문 높이 / 줄높이 - 1).
    fn page_lines(&mut self) -> usize {
        let (_, h) = self.logical_size();
        let lh = self.gpu.raw_editor_line_h();
        (((h / lh).floor() as usize).saturating_sub(1)).max(1)
    }
}

impl App {
    // ── 별도창 스폰 ──────────────────────────────────────────────────────────

    /// `md` 를 담은 새 편집기 별도창을 만든다. `near` 가 Some 이면 그 물리좌표에
    /// 띄운다(Phase 3 tear-off), None 이면 OS 가 위치를 정한다. 새 창 인덱스 반환.
    pub(crate) fn spawn_aux_editor(
        &mut self,
        mut md: MarkdownPane,
        event_loop: &ActiveEventLoop,
        near: Option<winit::dpi::PhysicalPosition<i32>>,
    ) -> Option<usize> {
        // 버퍼만 미리 채운다 — 뷰 모드는 부르는 쪽이 정한다. 여기서 raw 로
        // 못박던 동안엔 `.md` 를 팝아웃하면 렌더 뷰가 통째로 사라졌다(거노).
        md.seed_edit_lines();
        let title = aux_editor_title(&md);
        let mut attrs = WindowAttributes::default()
            .with_title(title.clone())
            .with_theme(Some(Theme::Dark))
            .with_inner_size(LogicalSize::new(760.0, 560.0))
            // 배경 실행(헤드리스 검증)이면 뜨면서 키 포커스를 안 가져간다 — 메인 창은
            // 이미 그렇게 하는데 별도창만 빠져 있어, 검증 한 번에 작업하던 창을
            // 통째로 빼앗겼다(거노).
            .with_active(!crate::background_launch());
        if let Some(pos) = near {
            attrs = attrs.with_position(pos);
        }
        let window = match create_untabbed(event_loop, attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                eprintln!("[auxwin] window create failed: {e}");
                return None;
            }
        };
        // 메인 창과 동일한 IME 정책: macOS 는 첫 자모 유실 버그 때문에 OS IME 를 끄고
        // in-process hangul Composer(self.hangul) 로 조합, 그 외 플랫폼은 OS IME.
        #[cfg(target_os = "macos")]
        window.set_ime_allowed(false);
        #[cfg(not(target_os = "macos"))]
        window.set_ime_allowed(true);
        let gpu = match gpu::GpuRenderer::new(window.clone(), FONT_SIZE) {
            Ok(g) => g,
            Err(e) => {
                eprintln!("[auxwin] gpu init failed: {e}");
                return None;
            }
        };
        let aux = AuxWindow {
            gpu,
            kind: AuxWindowKind::Editor(md),
            dirty: true,
            cursor_px: (0.0, 0.0),
            selecting: false,
            focused: true,
            preedit: String::new(),
            last_title: title,
            pending_capture: None,
            md_content_h: 0.0,
            tree_open: false,
            pinned: false,
            tree_scroll: 0.0,
            tree_rows: Vec::new(),
            face_rects: Vec::new(),
            header_btns: Vec::new(),
            window,
        };
        self.aux_windows.push(aux);
        let idx = self.aux_windows.len() - 1;
        eprintln!("[auxwin] opened editor window #{idx}");
        self.aux_redraw(idx);
        Some(idx)
    }

    /// 파일트리 Opt+더블클릭 / 빠른파일 Opt+클릭 — 파일을 바로 별도창으로 연다.
    /// 이미지가 아닌 텍스트/코드/마크다운만(편집기 창이므로). 이미 별도창에 열려 있으면
    /// 그 창을 포커스한다.
    pub(crate) fn popout_file_window(
        &mut self,
        path: std::path::PathBuf,
        event_loop: &ActiveEventLoop,
    ) {
        let path = std::fs::canonicalize(&path).unwrap_or(path);
        // 이미 별도창에 있으면 포커스만.
        if let Some(i) = self.aux_windows.iter().position(|a| {
            a.editor()
                .map(|m| std::path::Path::new(&m.doc.path) == path.as_path())
                .unwrap_or(false)
        }) {
            self.aux_windows[i].window.focus_window();
            return;
        }
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if crate::is_image_path(&path) {
            // 이미지는 별도 편집기 창의 범위 밖 — 기존 보조탭 경로로 폴백.
            self.open_file(path, None, true);
            return;
        }
        let raw = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("[auxwin] 파일 읽기 실패 {}: {e}", path.display());
                return;
            }
        };
        let is_md = matches!(ext.as_str(), "md" | "markdown");
        let doc = Arc::new(build_markdown_doc(&path, &raw));
        let edit_lines: Arc<Vec<String>> =
            Arc::new(raw.split('\n').map(|s| s.to_string()).collect());
        let md = MarkdownPane {
            doc,
            is_md_doc: is_md,
            // 마크다운은 읽으려고 여는 것이라 렌더 뷰로 시작한다 — 코드·텍스트는
            // 그럴 뷰가 없으니 그대로 raw. 편집은 글자를 치면 알아서 넘어간다.
            raw_mode: !is_md,
            edit_lines,
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
            wrap: false,
            extra: Vec::new(),
            undo_locked: false,
            folds: Vec::new(),
            folds_gen: 0,
            edited_at: None,
        };
        self.spawn_aux_editor(md, event_loop, None);
    }

    /// 파일 탭(보조탭이든 전용 split pane 이든)을 그 MarkdownPane 째로 별도창에 옮긴다 —
    /// 원래 탭/pane 은 제거. 팝아웃 아이콘 클릭(near=None → OS 기본 위치) 과 드래그
    /// tear-off(near=Some(커서 스크린 물리좌표) → 커서 밑에 뜸) 의 공통 진입점.
    pub(crate) fn popout_pane_tab(
        &mut self,
        outer: &str,
        tab_idx: usize,
        event_loop: &ActiveEventLoop,
        near: Option<winit::dpi::PhysicalPosition<i32>>,
    ) {
        // 이 pane 의 유일한 탭인가? 그러면 leaf 째 접고, 아니면 그 탭만 뺀다.
        let only_tab = self
            .ws
            .lock()
            .unwrap()
            .panes
            .get(outer)
            .map(|p| p.tabs.len() == 1)
            .unwrap_or(false);
        // MarkdownPane 을 탭에서 꺼낸다(내용물을 터미널 기본값 husk 로 대체).
        let md = {
            let mut ws = self.ws.lock().unwrap();
            let Some(pane) = ws.panes.get_mut(outer) else { return };
            let Some(tab) = pane.tabs.get_mut(tab_idx) else { return };
            match std::mem::take(&mut tab.content) {
                PaneContent::Markdown(m) => m,
                other => {
                    tab.content = other;
                    return;
                }
            }
        };
        if only_tab {
            // 전용 pane — leaf 를 접는다(remove_pane 이 resize/publish/redraw 까지).
            self.remove_pane(outer);
        } else {
            // 보조탭 husk 만 제거(형제 탭은 유지). 레이아웃 불변 → chrome 만 dirty.
            self.close_tab(outer, tab_idx);
            self.chrome_dirty = true;
            if let Some(w) = &self.window {
                w.request_redraw();
            }
        }
        self.spawn_aux_editor(md, event_loop, near);
    }

    /// 별도창을 닫는다(dirty 여도 그냥 — P1 철학, ● 가 경고였다).
    /// 편집기 별도창 닫기 요청 — 저장 안 한 편집분이 있으면 확인 모달을 띄우고
    /// 창은 그대로 둔다(모달이 답을 받으면 `PendingClose::AuxEditor` 로 돌아온다).
    pub(crate) fn close_editor_window(&mut self, idx: usize) {
        let Some(id) = self.aux_windows.get(idx).map(|a| a.window.id()) else { return };
        if self.guard_dirty(&crate::PendingClose::AuxEditor(id)) {
            return;
        }
        self.close_aux_window(idx);
    }

    pub(crate) fn close_aux_window(&mut self, idx: usize) {
        if idx < self.aux_windows.len() {
            let _ = self.aux_windows.remove(idx);
            eprintln!("[auxwin] closed window #{idx}");
        }
    }

    /// 별도창 redraw 요청(dirty + request_redraw). 메인 루프는 안 건드린다.
    pub(crate) fn aux_redraw(&mut self, idx: usize) {
        if let Some(a) = self.aux_windows.get_mut(idx) {
            a.dirty = true;
            a.window.request_redraw();
        }
    }

    /// 별도창 한 프레임 그리기(RedrawRequested / capture 시). OS 타이틀도 여기서 동기화.
    pub(crate) fn aux_render(&mut self, idx: usize) {
        let blink = self.cursor_blink_on(Instant::now());
        // 타이틀 동기화(변경 시에만) — 편집기/설정 공통.
        {
            let Some(a) = self.aux_windows.get_mut(idx) else { return };
            let want = a.title();
            if want != a.last_title {
                a.window.set_title(&want);
                a.last_title = want;
            }
        }
        if matches!(self.aux_windows.get(idx).map(|a| &a.kind), Some(AuxWindowKind::Settings)) {
            self.aux_render_settings(idx, blink);
            return;
        }
        if matches!(
            self.aux_windows.get(idx).map(|a| &a.kind),
            Some(AuxWindowKind::Terminal { .. })
        ) {
            self.aux_terminal_render(idx, blink);
            return;
        }
        if matches!(
            self.aux_windows.get(idx).map(|a| &a.kind),
            Some(AuxWindowKind::Room { .. })
        ) {
            self.aux_room_render(idx, blink);
            return;
        }
        let Some(a) = self.aux_windows.get_mut(idx) else { return };
        let on = a.focused && blink;
        a.render(on);
        a.dirty = false;
    }

    /// 설정 별도창 한 프레임 — App 상태를 스냅샷해 `paint_settings` 를 그대로
    /// 재사용한다(오버레이 코드와 동일 함수). area 는 창 client 전체, cursor 는
    /// 이 창의 로컬 좌표. rects·scroll clamp 는 App 에 되돌려 클릭·휠이 참조한다.
    fn aux_render_settings(&mut self, idx: usize, blink: bool) {
        let (w, h, scale, cursor, focused) = {
            let Some(a) = self.aux_windows.get(idx) else { return };
            let (w, h) = a.logical_size();
            (w, h, a.gpu.scale(), a.cursor_px, a.focused)
        };
        let mut ctx = self.settings_snapshot((0.0, 0.0, w, h), cursor);
        // 캐럿 blink 는 포커스된 창만(메인창 last_blink_on 은 안 건드린다).
        ctx.caret_on = focused && blink;
        let Some(a) = self.aux_windows.get_mut(idx) else { return };
        a.gpu.clear_chrome();
        a.gpu.rect(0.0, 0.0, w, h, crate::theme::bg());
        let (rects, content_h) = settings::paint_settings(&mut a.gpu, &ctx);
        let _ = a.gpu.render(&[], scale, 0.0, true);
        a.dirty = false;
        self.settings_rects = rects;
        // 휠 스크롤 상한: content 높이 − 보이는 폼 밴드(84px 페이지 헤더 제외) + 여유.
        let view_h = (h - 84.0).max(0.0);
        self.settings_scroll_max = (content_h - view_h + 24.0).max(0.0);
        if self.settings_scroll > self.settings_scroll_max {
            self.settings_scroll = self.settings_scroll_max;
        }
    }

    // ── 이벤트 라우팅 ────────────────────────────────────────────────────────

    /// window id 가 별도창일 때 handler.rs 가 위임하는 단일 진입점. 반환 없이 소비.
    pub(crate) fn aux_window_event(
        &mut self,
        idx: usize,
        event: WindowEvent,
        event_loop: &ActiveEventLoop,
    ) {
        // ModifiersChanged 는 포커스된 창으로만 오는데 self.modifiers 갱신은 메인 창
        // 이벤트 arm 에만 있었다 — 별도창에서 Ctrl/Cmd 판정(터미널 제어바이트·Cmd+W·
        // 에디터 단축키)이 메인 창의 마지막 상태로 고정되는 버그. 종류 무관 공통 갱신.
        if let WindowEvent::ModifiersChanged(mods) = &event {
            self.modifiers = mods.state();
        }
        if matches!(self.aux_windows.get(idx).map(|a| &a.kind), Some(AuxWindowKind::Settings)) {
            self.aux_settings_event(idx, event);
            return;
        }
        if matches!(
            self.aux_windows.get(idx).map(|a| &a.kind),
            Some(AuxWindowKind::Terminal { .. })
        ) {
            self.aux_terminal_event(idx, event, event_loop);
            return;
        }
        if matches!(
            self.aux_windows.get(idx).map(|a| &a.kind),
            Some(AuxWindowKind::Room { .. })
        ) {
            self.aux_room_event(idx, event, event_loop);
            return;
        }
        match event {
            WindowEvent::CloseRequested => {
                self.close_editor_window(idx);
            }
            WindowEvent::Resized(size) => {
                if let Some(a) = self.aux_windows.get_mut(idx) {
                    a.gpu.resize(size.width, size.height);
                    a.dirty = true;
                    a.window.request_redraw();
                }
            }
            WindowEvent::ScaleFactorChanged { .. } => {
                if let Some(a) = self.aux_windows.get_mut(idx) {
                    let sf = a.window.scale_factor() as f32;
                    a.gpu.set_scale(sf);
                    a.gpu.set_font_size(FONT_SIZE);
                    let sz = a.window.inner_size();
                    a.gpu.resize(sz.width, sz.height);
                    a.dirty = true;
                    a.window.request_redraw();
                }
            }
            WindowEvent::Focused(f) => {
                if let Some(a) = self.aux_windows.get_mut(idx) {
                    a.focused = f;
                    a.window.request_redraw();
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                let scale = self
                    .aux_windows
                    .get(idx)
                    .map(|a| a.gpu.scale())
                    .unwrap_or(1.0);
                if let Some(a) = self.aux_windows.get_mut(idx) {
                    a.cursor_px = (position.x as f32 / scale, position.y as f32 / scale);
                }
                self.aux_mouse_drag(idx);
            }
            WindowEvent::MouseInput { state, button: MouseButton::Left, .. } => {
                match state {
                    ElementState::Pressed => self.aux_mouse_press(idx),
                    ElementState::Released => {
                        if let Some(a) = self.aux_windows.get_mut(idx) {
                            a.selecting = false;
                        }
                    }
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                self.aux_wheel(idx, delta);
            }
            WindowEvent::KeyboardInput { event, .. } => {
                self.aux_editor_key(idx, &event, event_loop);
            }
            WindowEvent::Ime(ime) => {
                self.aux_editor_ime(idx, ime);
            }
            WindowEvent::RedrawRequested => {
                self.aux_render(idx);
            }
            _ => {}
        }
    }

    // ── 키보드 ───────────────────────────────────────────────────────────────

    fn aux_editor_key(
        &mut self,
        idx: usize,
        event: &KeyEvent,
        _event_loop: &ActiveEventLoop,
    ) {
        use winit::keyboard::{KeyCode, PhysicalKey};
        if event.state != ElementState::Pressed {
            return;
        }
        self.last_input_at = Instant::now();
        // Cmd+W: 이 별도창 닫기. 저장 안 한 편집분이 있으면 먼저 묻는다.
        if self.host_mod()
            && matches!(event.physical_key, PhysicalKey::Code(KeyCode::KeyW))
        {
            self.close_editor_window(idx);
            return;
        }
        // Cmd/Ctrl 조합 = 편집기 단축키(저장/복붙/undo/선택/커서점프). 그 외 조합은 삼킴.
        if self.host_mod() || self.modifiers.control_key() {
            if let PhysicalKey::Code(code) = event.physical_key {
                self.aux_editor_shortcut(idx, code);
            }
            self.aux_redraw(idx);
            return;
        }
        // 렌더 뷰에서 글자를 치면 삼키는 대신 raw 로 넘어가 그 글자를 살린다
        // (pane 편집기와 같은 규칙). 방향키·PageUp 같은 이동 키는 렌더 뷰에서
        // 할 일이 없으니 그대로 삼킨다 — 스크롤은 휠이다.
        if self.aux_windows.get(idx).and_then(|a| a.editor()).is_some_and(|m| !m.raw_mode) {
            if !crate::markdown::md_mutating_key(event) {
                return;
            }
            if let Some(a) = self.aux_windows.get_mut(idx) {
                // 두 뷰의 스크롤은 단위가 다르다 — 렌더는 본문 픽셀, raw 는 줄
                // 좌표다. 값을 그대로 넘기면 엉뚱한 줄로 튀고 0 으로 되돌리면
                // 읽던 자리를 잃으니, 본문 높이 대비 비율로 옮겨 대략 같은 곳에서
                // 편집이 시작되게 한다.
                let lh = a.gpu.raw_editor_line_h();
                let ratio = if a.md_content_h > 0.0 {
                    (a.editor().map_or(0.0, |m| m.scroll) / a.md_content_h).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                if let Some(m) = a.editor_mut() {
                    m.ensure_raw_seeded();
                    let line = ((m.edit_lines.len() as f32 * ratio) as usize)
                        .min(m.edit_lines.len().saturating_sub(1));
                    m.cur_line = line;
                    m.cur_col = 0;
                    m.scroll = line as f32 * lh;
                }
            }
        }
        // 평문 키 — 한글 조합 경유 편집.
        self.aux_editor_input(idx, event);
        self.aux_redraw(idx);
    }

    /// Cmd/Ctrl 편집기 단축키. 반환값은 소비 여부(현재는 전부 true 계열이지만
    /// 확장 시 false 팔이 필요할 수 있어 유지).
    fn aux_editor_shortcut(&mut self, idx: usize, code: winit::keyboard::KeyCode) -> bool {
        use winit::keyboard::KeyCode;
        // 확정은 **모든** 팔 앞에서 한 번 — pane 편집기의 `md_flush_preedit` 과
        // 같은 규칙. 예전엔 C·화살표가 빠져 있어 조합 중 그 키를 누르면 음절이
        // 유실됐다(양쪽 테이블에 같은 버그가 복제돼 있었다).
        self.aux_flush_hangul(idx);
        match code {
            KeyCode::KeyS => {
                self.aux_editor_save(idx);
                true
            }
            KeyCode::KeyV => {
                self.aux_editor_paste(idx);
                true
            }
            KeyCode::KeyC => {
                self.aux_copy(idx, false);
                true
            }
            KeyCode::KeyX => {
                self.aux_copy(idx, true);
                true
            }
            KeyCode::KeyA => {
                if let Some(m) = self.aux_windows.get_mut(idx).and_then(|a| a.editor_mut()) {
                    m.select_all_buf();
                }
                true
            }
            // pane 편집기와 같은 Cmd+D = 캐럿 단어 선택.
            KeyCode::KeyD if !self.modifiers.shift_key() => {
                if let Some(m) = self.aux_windows.get_mut(idx).and_then(|a| a.editor_mut()) {
                    m.select_word_at();
                }
                if let Some(a) = self.aux_windows.get_mut(idx) {
                    a.ensure_caret_visible();
                }
                true
            }
            KeyCode::KeyZ => {
                let redo = self.modifiers.shift_key();
                if let Some(a) = self.aux_windows.get_mut(idx) {
                    if let Some(m) = a.editor_mut() {
                        if redo {
                            m.do_redo();
                        } else {
                            m.do_undo();
                        }
                    }
                    a.ensure_caret_visible();
                }
                true
            }
            KeyCode::ArrowLeft | KeyCode::ArrowRight | KeyCode::ArrowUp | KeyCode::ArrowDown => {
                let shift = self.modifiers.shift_key();
                if let Some(a) = self.aux_windows.get_mut(idx) {
                    if let Some(m) = a.editor_mut() {
                        m.apply_cmd_arrow(code, shift);
                    }
                    a.ensure_caret_visible();
                }
                true
            }
            _ => false,
        }
    }

    /// 별도창 편집기의 한글 조합 입력(md_editor_input 의 aux 판). 공유 composer
    /// self.hangul 를 쓰되 프리에딧은 이 창의 것으로 스탬프한다.
    fn aux_editor_input(&mut self, idx: usize, event: &KeyEvent) {
        use winit::keyboard::{Key, NamedKey};
        if crate::input::is_modifier_key(event) {
            return;
        }
        self.ime_retarget(crate::ImeFocus::AuxEditor(idx));
        #[cfg(target_os = "macos")]
        if let Some(t) = &event.text {
            if t.chars().count() == 1 {
                if let Some(c) = t.chars().next() {
                    if (0x3130..=0x318F).contains(&(c as u32)) {
                        if let Some(commit) = self.hangul.feed(c) {
                            self.aux_insert(idx, &commit);
                        }
                        let pe = self.hangul.preedit().unwrap_or_default();
                        if let Some(a) = self.aux_windows.get_mut(idx) {
                            a.preedit = pe;
                        }
                        return;
                    }
                }
            }
        }
        if matches!(event.logical_key, Key::Named(NamedKey::Backspace))
            && self.hangul.backspace()
        {
            let pe = self.hangul.preedit().unwrap_or_default();
            if let Some(a) = self.aux_windows.get_mut(idx) {
                a.preedit = pe;
            }
            return;
        }
        if let Some(flushed) = self.hangul.flush() {
            self.aux_insert(idx, &flushed);
        }
        if let Some(a) = self.aux_windows.get_mut(idx) {
            a.preedit.clear();
        }
        // 평문 편집/모션 키.
        let shift = self.modifiers.shift_key();
        let alt = self.modifiers.alt_key();
        let page_lines = if matches!(
            event.logical_key,
            Key::Named(NamedKey::PageUp) | Key::Named(NamedKey::PageDown)
        ) {
            self.aux_windows
                .get_mut(idx)
                .map(|a| a.page_lines())
                .unwrap_or(1)
        } else {
            0
        };
        if let Some(a) = self.aux_windows.get_mut(idx) {
            if let Some(m) = a.editor_mut() {
                m.apply_edit_key(event, shift, alt, page_lines);
            }
            a.ensure_caret_visible();
        }
    }

    /// 조합 중인 음절을 버퍼에 확정하고 프리에딧을 비운다(저장/복사/undo 전에 호출).
    fn aux_flush_hangul(&mut self, idx: usize) {
        if let Some(flushed) = self.hangul.flush() {
            self.aux_insert(idx, &flushed);
        }
        if let Some(a) = self.aux_windows.get_mut(idx) {
            a.preedit.clear();
        }
    }

    pub(crate) fn aux_insert(&mut self, idx: usize, text: &str) {
        if let Some(a) = self.aux_windows.get_mut(idx) {
            if let Some(m) = a.editor_mut() {
                m.insert_at_caret(text);
            }
            a.ensure_caret_visible();
        }
    }

    /// 비-macOS(Windows/Linux) OS IME 경로 — Preedit 는 이 창 프리에딧, Commit 은 삽입.
    fn aux_editor_ime(&mut self, idx: usize, ime: Ime) {
        match ime {
            Ime::Enabled | Ime::Disabled => {
                if let Some(a) = self.aux_windows.get_mut(idx) {
                    a.preedit.clear();
                }
            }
            Ime::Preedit(text, _) => {
                if let Some(a) = self.aux_windows.get_mut(idx) {
                    a.preedit = text;
                }
            }
            Ime::Commit(text) => {
                if let Some(a) = self.aux_windows.get_mut(idx) {
                    a.preedit.clear();
                }
                self.aux_insert(idx, &text);
            }
        }
        self.aux_redraw(idx);
    }

    fn aux_editor_save(&mut self, idx: usize) {
        let job = self.aux_windows.get(idx).and_then(|a| {
            a.editor().map(|m| (m.edit_lines.join("\n"), m.doc.path.clone()))
        });
        let Some((text, path)) = job else { return };
        match crate::markdown::write_atomic(&path, &text) {
            Ok(()) => {
                if let Some(m) = self.aux_windows.get_mut(idx).and_then(|a| a.editor_mut()) {
                    m.mark_saved();
                }
                let name = std::path::Path::new(&path)
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or_default()
                    .to_string();
                self.set_toast(format!("✓ {name} 저장됨"));
            }
            Err(e) => {
                eprintln!("[auxwin] 저장 실패 {path}: {e}");
                self.set_toast(format!("⚠ 저장 실패: {e}"));
            }
        }
    }

    fn aux_editor_paste(&mut self, idx: usize) {
        let text = match arboard::Clipboard::new().and_then(|mut cb| cb.get_text()) {
            Ok(t) => t,
            Err(_) => return,
        };
        if text.is_empty() {
            return;
        }
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        if let Some(a) = self.aux_windows.get_mut(idx) {
            if let Some(m) = a.editor_mut() {
                m.paste_at_caret(&text);
            }
            a.ensure_caret_visible();
        }
    }

    fn aux_copy(&mut self, idx: usize, cut: bool) {
        let text = self
            .aux_windows
            .get_mut(idx)
            .and_then(|a| a.editor_mut())
            .and_then(|m| m.take_copy(cut));
        let Some(text) = text else { return };
        match arboard::Clipboard::new() {
            Ok(mut cb) => {
                let _ = cb.set_text(text);
            }
            Err(e) => eprintln!("[auxwin] clipboard open failed: {e}"),
        }
        if cut {
            if let Some(a) = self.aux_windows.get_mut(idx) {
                a.ensure_caret_visible();
            }
        }
    }

    // ── 마우스 ───────────────────────────────────────────────────────────────

    /// 현재 커서 위치를 (line, col) 캐럿으로 히트테스트.
    fn aux_caret_at_cursor(&mut self, idx: usize) -> (usize, usize) {
        let snap = {
            let Some(a) = self.aux_windows.get(idx) else { return (0, 0) };
            let Some(m) = a.editor() else { return (0, 0) };
            (
                m.edit_lines.clone(),
                m.scroll,
                m.h_scroll,
                a.cursor_px.0,
                a.cursor_px.1,
            )
        };
        let (lines, scroll, h_scroll, cx, cy) = snap;
        let bar = self.aux_windows.get(idx).map_or(0.0, |a| a.md_bar_h());
        let Some(a) = self.aux_windows.get_mut(idx) else { return (0, 0) };
        a.gpu
            .raw_editor_caret_at(&lines, 0.0, bar, scroll, h_scroll, cx, cy, &[], 0)
    }

    fn aux_mouse_press(&mut self, idx: usize) {
        let (line, col) = self.aux_caret_at_cursor(idx);
        self.last_input_at = Instant::now();
        if let Some(a) = self.aux_windows.get_mut(idx) {
            if let Some(m) = a.editor_mut() {
                m.cur_line = line;
                m.cur_col = col;
                // anchor==cursor → 아직 무선택, 드래그하면 자란다.
                m.sel_anchor = Some((line, col));
                m.last_edit = EditKind::Break;
            }
            a.selecting = true;
        }
        self.aux_redraw(idx);
    }

    fn aux_mouse_drag(&mut self, idx: usize) {
        if !self.aux_windows.get(idx).map(|a| a.selecting).unwrap_or(false) {
            return;
        }
        let (line, col) = self.aux_caret_at_cursor(idx);
        if let Some(m) = self.aux_windows.get_mut(idx).and_then(|a| a.editor_mut()) {
            m.cur_line = line;
            m.cur_col = col;
        }
        self.aux_redraw(idx);
    }

    fn aux_wheel(&mut self, idx: usize, delta: MouseScrollDelta) {
        let Some(a) = self.aux_windows.get_mut(idx) else { return };
        let lh = a.gpu.raw_editor_line_h();
        let (_, h) = a.logical_size();
        let dy = match delta {
            MouseScrollDelta::LineDelta(_, y) => y * lh * 3.0,
            MouseScrollDelta::PixelDelta(p) => p.y as f32,
        };
        // 렌더 뷰는 줄 수로 높이를 못 구한다(블록마다 높이가 다르다) — 지난
        // 프레임이 남긴 실제 본문 높이를 쓴다. 아직 한 번도 안 그렸으면 0 이라
        // clamp 가 스크롤을 막아 버리므로, 그때만 줄 기준으로 물러선다.
        let rendered = a.editor().is_some_and(|m| !m.raw_mode);
        let lines_n = a.editor().map(|m| m.edit_lines.len()).unwrap_or(0);
        let content_h = match (rendered, a.md_content_h) {
            (true, ch) if ch > 0.0 => ch,
            _ => lines_n as f32 * lh,
        };
        // 본문 높이를 넘는 만큼만 스크롤 — 마지막 줄이 화면 안에 머물게 여유 2줄.
        let max_scroll = (content_h - h + lh * 2.0).max(0.0);
        if let Some(m) = a.editor_mut() {
            // 위로 스크롤(y>0) = scroll 감소.
            let ns = (m.scroll - dy).clamp(0.0, max_scroll);
            m.scroll = ns.max(0.0);
        }
        a.dirty = true;
        a.window.request_redraw();
    }

    // ── 설정 별도창 ────────────────────────────────────────────────────────

    /// 설정 별도창이 있으면 그 인덱스. `settings_open` 은 이 창의 존재와 동기화된
    /// 편의 플래그(spawn 시 true, 닫힐 때 false)라 chrome active 표시가 참조한다.
    pub(crate) fn settings_window_idx(&self) -> Option<usize> {
        self.aux_windows
            .iter()
            .position(|a| matches!(a.kind, AuxWindowKind::Settings))
    }

    /// 설정 별도창 진입점(기어·사이드바 항목·프사 클릭). 이미 열려 있으면 포커스만,
    /// `cat`/`student` 가 주어지면 그 페이지·학생으로 전환한다(딥링크).
    pub(crate) fn open_settings_window(
        &mut self,
        event_loop: &ActiveEventLoop,
        cat: Option<SettingsCat>,
        student: Option<String>,
    ) {
        let Some(idx) = self
            .settings_window_idx()
            .or_else(|| self.spawn_aux_settings(event_loop))
        else {
            return;
        };
        if let Some(c) = cat {
            if self.settings_cat != c {
                self.flush_student_persona();
                self.settings_cat = c;
                self.settings_input = None;
                self.settings_scroll = 0.0;
            }
        }
        if let Some(name) = student {
            self.select_student_for_edit(name);
        }
        if let Some(a) = self.aux_windows.get(idx) {
            a.window.focus_window();
        }
        self.aux_redraw(idx);
    }

    fn spawn_aux_settings(&mut self, event_loop: &ActiveEventLoop) -> Option<usize> {
        let attrs = WindowAttributes::default()
            .with_title("Settings")
            .with_theme(Some(Theme::Dark))
            // Wide enough that the theme grid wraps to three columns instead of
            // four rows — at 720 the palette cards alone filled the viewport and
            // pushed shape/accent below the fold.
            .with_inner_size(LogicalSize::new(920.0, 720.0));
        let window = match create_untabbed(event_loop, attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                eprintln!("[auxwin] settings window create failed: {e}");
                return None;
            }
        };
        // 설정 폼의 텍스트 필드(경로·persona)에 한글이 필요하다. 편집기와 동일한
        // IME 정책: macOS 는 OS IME 끄고 in-process composer, 그 외는 OS IME.
        #[cfg(target_os = "macos")]
        window.set_ime_allowed(false);
        #[cfg(not(target_os = "macos"))]
        window.set_ime_allowed(true);
        let gpu = match gpu::GpuRenderer::new(window.clone(), FONT_SIZE) {
            Ok(g) => g,
            Err(e) => {
                eprintln!("[auxwin] settings gpu init failed: {e}");
                return None;
            }
        };
        let aux = AuxWindow {
            gpu,
            kind: AuxWindowKind::Settings,
            dirty: true,
            cursor_px: (0.0, 0.0),
            selecting: false,
            focused: true,
            preedit: String::new(),
            last_title: "Settings".to_string(),
            pending_capture: None,
            md_content_h: 0.0,
            tree_open: false,
            pinned: false,
            tree_scroll: 0.0,
            tree_rows: Vec::new(),
            face_rects: Vec::new(),
            header_btns: Vec::new(),
            window,
        };
        self.aux_windows.push(aux);
        let idx = self.aux_windows.len() - 1;
        self.settings_open = true;
        self.settings_scroll = 0.0;
        self.settings_input = None;
        eprintln!("[auxwin] opened settings window #{idx}");
        self.aux_redraw(idx);
        Some(idx)
    }

    pub(crate) fn close_settings_window(&mut self, idx: usize) {
        self.flush_student_persona();
        self.settings_input = None;
        self.settings_open = false;
        self.close_aux_window(idx);
    }

    /// 설정 별도창 이벤트 라우팅 — 편집기와 다른 처리(폼 클릭·휠 스크롤·필드 키).
    fn aux_settings_event(&mut self, idx: usize, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => self.close_settings_window(idx),
            WindowEvent::Resized(size) => {
                if let Some(a) = self.aux_windows.get_mut(idx) {
                    a.gpu.resize(size.width, size.height);
                    a.dirty = true;
                    a.window.request_redraw();
                }
            }
            WindowEvent::ScaleFactorChanged { .. } => {
                if let Some(a) = self.aux_windows.get_mut(idx) {
                    let sf = a.window.scale_factor() as f32;
                    a.gpu.set_scale(sf);
                    a.gpu.set_font_size(FONT_SIZE);
                    let sz = a.window.inner_size();
                    a.gpu.resize(sz.width, sz.height);
                    a.dirty = true;
                    a.window.request_redraw();
                }
            }
            WindowEvent::Focused(f) => {
                if let Some(a) = self.aux_windows.get_mut(idx) {
                    a.focused = f;
                    a.window.request_redraw();
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                let scale = self.aux_windows.get(idx).map(|a| a.gpu.scale()).unwrap_or(1.0);
                if let Some(a) = self.aux_windows.get_mut(idx) {
                    a.cursor_px = (position.x as f32 / scale, position.y as f32 / scale);
                    // hover 피드백(카드·세그먼트·행)을 갱신하려면 매 이동에 재페인트.
                    a.dirty = true;
                    a.window.request_redraw();
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                self.last_input_at = Instant::now();
                let (cx, cy) = self.aux_windows.get(idx).map(|a| a.cursor_px).unwrap_or((0.0, 0.0));
                // rects 는 area=(0,0,w,h) 좌표계라 창 로컬 커서를 그대로 넘긴다.
                self.settings_click(cx, cy);
                self.aux_redraw(idx);
            }
            WindowEvent::MouseWheel { delta, .. } => self.aux_settings_wheel(idx, delta),
            WindowEvent::KeyboardInput { event, .. } => self.aux_settings_key(idx, &event),
            WindowEvent::Ime(ime) => self.aux_settings_ime(idx, ime),
            WindowEvent::RedrawRequested => self.aux_render(idx),
            _ => {}
        }
    }

    fn aux_settings_wheel(&mut self, idx: usize, delta: MouseScrollDelta) {
        let dy_px = match delta {
            MouseScrollDelta::LineDelta(_, y) => y * 40.0,
            MouseScrollDelta::PixelDelta(p) => p.y as f32,
        };
        let next = (self.settings_scroll - dy_px).clamp(0.0, self.settings_scroll_max);
        if (next - self.settings_scroll).abs() > 0.1 {
            self.settings_scroll = next;
            self.aux_redraw(idx);
        }
    }

    fn aux_settings_key(&mut self, idx: usize, event: &KeyEvent) {
        use winit::keyboard::{Key, KeyCode, NamedKey, PhysicalKey};
        if event.state != ElementState::Pressed {
            return;
        }
        self.last_input_at = Instant::now();
        if crate::input::is_modifier_key(event) {
            return;
        }
        self.ime_retarget(crate::ImeFocus::Settings);
        // Cmd/Ctrl+W: 설정 창 닫기.
        if self.host_mod() && matches!(event.physical_key, PhysicalKey::Code(KeyCode::KeyW)) {
            self.close_settings_window(idx);
            return;
        }
        // macOS 는 OS IME 를 껐으므로 한글 자모(U+3130..318F)는 in-process composer
        // 로 조합해 완성 음절만 포커스 필드에 넣는다.
        //
        // ⚠️ 자모는 `event.text` 로만 온다 — `logical_key` 는 같은 키의 **영문
        // 각인**(ㄱ→r, ㅖ→P)이라, 그걸 보고 판단하면 자모가 조합기를 그냥
        // 지나쳐 `settings_key` 에 낱자로 꽂힌다("계"가 "ㄱㅖ"로 남던 것).
        #[cfg(target_os = "macos")]
        if self.settings_input.is_some() {
            // text 가 비고 logical_key 만 자모로 오는 프레임이 있어 둘 다 본다 —
            // 한쪽만 보면 그 프레임의 자모가 조합기를 못 만나고 필드에 낱자로 꽂힌다.
            let one = |s: &str| {
                let mut it = s.chars();
                it.next().filter(|_| it.next().is_none())
            };
            let typed = event.text.as_ref().and_then(|t| one(t)).or_else(|| {
                match &event.logical_key {
                    Key::Character(s) => one(s),
                    _ => None,
                }
            });
            if let Some(c) = typed {
                if self.settings_hangul_char(c) {
                    self.aux_redraw(idx);
                    return;
                }
            }
            if matches!(event.logical_key, Key::Named(NamedKey::Backspace)) && self.hangul.backspace() {
                self.aux_redraw(idx);
                return;
            }
            self.settings_hangul_flush();
        }
        // 포커스 필드가 있으면 그 필드로(persona/단일라인 분기는 settings_key 내부).
        if self.settings_input.is_some() {
            self.settings_key(event);
            self.aux_redraw(idx);
            return;
        }
        // 포커스 필드가 없을 때 Esc = 창 닫기.
        if matches!(event.logical_key, Key::Named(NamedKey::Escape)) {
            self.close_settings_window(idx);
            return;
        }
        self.aux_redraw(idx);
    }

    /// 비-macOS OS IME 경로 — 설정 폼엔 preedit 렌더가 없어 Commit 만 반영한다.
    fn aux_settings_ime(&mut self, idx: usize, ime: Ime) {
        if let Ime::Commit(text) = ime {
            self.settings_insert_text(&text);
            self.aux_redraw(idx);
        }
    }

    // ── 터미널 별도창(undock) ──────────────────────────────────────────────
    //
    // 일반 터미널 pane 을 별도 OS 창으로 분리한다. 편집기/설정 aux 와 달리 이 창은
    // 자체 데이터를 안 들고, App.ws 의 그 pane 이 소유한 셀 그리드/커서를 매 프레임
    // 스냅샷해 `draw_cells` 로 그리는 뷰다. PtySession 은 App.pty 에 그대로 살아
    // 세션이 안 끊긴다. 입력은 `self.pty[pane_id].send_bytes` 로 직접, resize 는
    // 창 크기를 셀수로 환산해 `pty.resize`.

    /// `pane_id` 터미널 pane 을 별도창으로 띄운다. `near` Some 이면 그 물리좌표에
    /// (tear-off), None 이면 OS 기본 위치. `want_cells` Some 이면 그 칸수가 들어가는
    /// 크기로 연다(그리드에서 쓰던 폭·높이 유지). 새 창 인덱스 반환. 진입점(undock)은
    /// 이미 레이아웃 트리에서 leaf 를 빼고 pty·ws.panes 를 유지한 상태로 호출한다.
    pub(crate) fn spawn_aux_terminal(
        &mut self,
        pane_id: String,
        home_window: usize,
        event_loop: &ActiveEventLoop,
        near: Option<winit::dpi::PhysicalPosition<i32>>,
        want_cells: Option<(u16, u16)>,
    ) -> Option<usize> {
        let title = pane_id.clone();
        let mut attrs = WindowAttributes::default()
            .with_title(title.clone())
            // TODO(다음): 헤더의 클릭·드래그 배선(되돌리기 버튼 + drag_window)이
            // 붙는 즉시 `.with_decorations(false)` 를 여기 되살린다. 헤더 그리기는
            // 이미 됐지만 배선 없이 타이틀바를 끄면 창을 옮길 손잡이도, 되돌릴
            // 수단도 없는 창이 남는다 — 거노 요구는 "우리 UI 로"지 "손잡이 없이"가
            // 아니다.
            .with_theme(Some(Theme::Dark))
            .with_inner_size(LogicalSize::new(800.0, 520.0));
        if let Some(pos) = near {
            attrs = attrs.with_position(pos);
        }
        let window = match create_aux_window(event_loop, attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                eprintln!("[auxwin] terminal window create failed: {e}");
                return None;
            }
        };
        // 메인 창과 동일 IME 정책: macOS 는 OS IME 끄고 in-process hangul, 그 외는 OS IME.
        #[cfg(target_os = "macos")]
        window.set_ime_allowed(false);
        #[cfg(not(target_os = "macos"))]
        window.set_ime_allowed(true);
        let mut gpu = match gpu::GpuRenderer::new(window.clone(), FONT_SIZE) {
            Ok(g) => g,
            Err(e) => {
                eprintln!("[auxwin] terminal gpu init failed: {e}");
                return None;
            }
        };
        // 그리드에서 쓰던 칸수를 그대로 담을 크기로 창을 다시 잰다. 800×520 고정이면
        // 넓게 쓰던 pane 이 꺼내는 순간 좁아져 오른쪽·아래가 통째로 사라진다 —
        // 셸은 SIGWINCH 로 리플로우하니 "지워진" 게 아니라 "잘린" 것으로 보인다(거노).
        // gpu 를 만든 뒤라야 이 창의 셀 크기를 알고, 그래야 칸수→px 환산이 맞는다.
        if let Some((wc, hc)) = want_cells {
            let (cw, ch) = (gpu.cell_w, gpu.cell_h);
            if cw > 0.0 && ch > 0.0 {
                // 모니터 밖으로 나가는 창은 잘림을 옮겨 놓을 뿐이다 — 작업영역에 맞춘다.
                let (max_w, max_h) = window
                    .current_monitor()
                    .map(|m| {
                        let s = m.scale_factor() as f32;
                        (m.size().width as f32 / s * 0.94, m.size().height as f32 / s * 0.88)
                    })
                    .unwrap_or((1600.0, 1000.0));
                let want_w = (PANE_INNER_X * 2.0 + wc as f32 * cw).clamp(420.0, max_w);
                let want_h =
                    (AUX_CELL_TOP + PANE_INNER_Y + hc as f32 * ch).clamp(260.0, max_h);
                if let Some(got) =
                    window.request_inner_size(LogicalSize::new(want_w, want_h))
                {
                    gpu.resize(got.width, got.height);
                }
            }
        }
        let aux = AuxWindow {
            gpu,
            kind: AuxWindowKind::Terminal { pane_id: pane_id.clone(), window: home_window },
            dirty: true,
            cursor_px: (0.0, 0.0),
            selecting: false,
            focused: true,
            preedit: String::new(),
            last_title: title,
            pending_capture: None,
            md_content_h: 0.0,
            tree_open: false,
            pinned: false,
            tree_scroll: 0.0,
            tree_rows: Vec::new(),
            face_rects: Vec::new(),
            header_btns: Vec::new(),
            window,
        };
        self.aux_windows.push(aux);
        let idx = self.aux_windows.len() - 1;
        eprintln!(
            "[auxwin] opened terminal window #{idx} for {pane_id} 요청칸={want_cells:?} 창={:?}",
            self.aux_windows[idx].logical_size()
        );
        // 창 client 크기에 맞춰 PTY 를 즉시 resize — 셸이 SIGWINCH 로 새 셀수에 리플로우.
        self.aux_terminal_resize_pty(idx);
        self.aux_redraw(idx);
        Some(idx)
    }

    /// 창 client 크기(logical)를 셀수로 환산해 이 창이 뷰하는 pane 의 PTY 를 resize.
    /// 본문 = 창 − 좌우/상하 PANE_INNER 여백. 셀 메트릭은 이 창 gpu 의 것(논리 px).
    fn aux_terminal_resize_pty(&mut self, idx: usize) {
        let (pane_id, w, h, cw, ch, left) = {
            let Some(a) = self.aux_windows.get(idx) else { return };
            let Some(pid) = a.term_pane_id() else { return };
            let (w, h) = a.logical_size();
            (pid.to_string(), w, h, a.gpu.cell_w, a.gpu.cell_h, a.cell_left())
        };
        if cw <= 0.0 || ch <= 0.0 {
            return;
        }
        // 왼쪽은 트리 폭까지 포함한 `cell_left`, 오른쪽은 여백 하나 — 트리를 열면
        // 셀이 들어갈 칸 수가 그만큼 줄어야 셸이 새 폭으로 줄바꿈한다.
        let cols = (((w - left - PANE_INNER_X) / cw).floor() as i32).max(1) as u16;
        let rows = (((h - AUX_CELL_TOP - PANE_INNER_Y) / ch).floor() as i32).max(1) as u16;
        if let Some(pty) = self.pty.get(&pane_id) {
            let _ = pty.resize(cols, rows);
        }
    }

    /// 터미널 별도창 한 프레임 — ws 에서 그 pane 의 셀 그리드/커서를 스냅샷해
    /// `draw_cells` 로 본문을, blink 위상이면 커서 rect 를 그린다(단일 pane 이라
    /// 헤더/링크hover/선택 오버레이는 v1 제외 — paint_gpu_overlays 커서부만 복제).
    fn aux_terminal_render(&mut self, idx: usize, blink: bool) {
        let (pane_id, scale, w, h, focused, home_window) = {
            let Some(a) = self.aux_windows.get(idx) else { return };
            let Some(pid) = a.term_pane_id() else { return };
            let (w, h) = a.logical_size();
            let home = match &a.kind {
                AuxWindowKind::Terminal { window, .. } => *window,
                _ => 0,
            };
            (pid.to_string(), a.gpu.scale(), w, h, a.focused, home)
        };
        // draw 중 lock 을 안 쥐도록 셀/커서를 복사해 스냅샷. 배정 학생도 같이 —
        // 꺼낸 pane 도 메인 그리드에 있을 때와 **같은 답**을 내야 한다(거노: 별도창이
        // 완전 다른 앱 같다). 그래서 이름은 `display_pane_char`, 색은 `pane_accent`
        // 로 메인과 같은 함수를 지난다. 여기서 `ws.pane_character` 를 날로 읽던 동안
        // 셸만 도는 pane 이 이 창에선 학생, 메인 그리드에선 남이었다 — 되돌리면
        // 「학생 테마가 깨졌다」로 보이던 것의 정체다.
        let (snap, student, student_col) = {
            let ws = self.ws.lock().unwrap();
            let cells = ws.panes.get(&pane_id).and_then(|p| {
                p.term()
                    .map(|t| (t.cells.clone(), t.cursor_row, t.cursor_col, t.cursor_visible))
            });
            let name = self.display_pane_char(&ws, &pane_id);
            let col = self.pane_accent(&ws, &pane_id);
            (cells, name, col)
        };
        let working = self
            .pane_activity
            .get(&pane_id)
            .is_some_and(|a| a.status == "working");
        let accent = student_col.unwrap_or_else(crate::theme::accent);
        // 트리는 `self.file_tree` 를 읽는데 아래에서 `self.aux_windows` 를 가변으로
        // 잡으므로 먼저 떠 둔다. 닫혀 있으면 아예 안 뜬다 — 매 프레임 노드를 통째로
        // clone 하는 값은 트리를 보고 있을 때만 치른다.
        let tree_rows = if self.aux_windows.get(idx).is_some_and(|a| a.tree_open) {
            self.aux_tree_rows()
        } else {
            Vec::new()
        };
        // 학생 스프라이트 자리 — 셀 사본을 훑으며 자리표시자를 지우므로 draw_cells
        // 전에, 그리고 `aux_windows` 를 가변으로 잡기 전에 끝낸다.
        let mut snap = snap;
        // 입력박스 손질을 **메인 그리드와 같은 함수로** — 거노 2026-08-05: 별도창엔
        // `@aru-p12` 칩이 그대로 남아 있었다. 이 창은 자기 셀 스냅샷을 따로 뜨는
        // 경로라(`render_frame_gpu` 를 안 탄다) 메인에만 걸린 손질이 통째로 빠져
        // 있었다 — 꺼낸 pane 이 "완전 다른 앱 같다"는 그 증상의 남은 조각이다.
        //
        // 사본을 만들지 않고 `render::` 함수를 그대로 부른다. 오늘 같은 로직 두 벌이
        // 세 번 물었고(칩 관문·`is_rule`·인레이), 그때마다 한쪽만 고쳐졌다.
        if let Some((cells, ..)) = snap.as_mut() {
            let runs_claude = self
                .pty
                .get(pane_id.as_str())
                .and_then(|p| p.active_agent())
                .is_some();
            if runs_claude {
                crate::render::strip_agent_chip(cells);
                crate::render::style_prompt_box(cells, accent);
            }
        }
        let (sprites, anim_ms) = {
            let (cl, cw, ch) = self
                .aux_windows
                .get(idx)
                .map_or((PANE_INNER_X, 0.0, 0.0), |a| {
                    (a.cell_left(), a.gpu.cell_w, a.gpu.cell_h)
                });
            let s = match snap.as_mut() {
                Some((cells, ..)) if cw > 0.0 => {
                    self.aux_student_slots(&pane_id, cells, cl, AUX_CELL_TOP, cw, ch)
                }
                _ => crate::render::StudentOverlays::default(),
            };
            (s, self.version_anim_start.elapsed().as_millis() as u64)
        };
        let Some(a) = self.aux_windows.get_mut(idx) else { return };
        a.gpu.clear_chrome();
        a.gpu.rect(0.0, 0.0, w, h, crate::theme::bg());
        let Some((rows, cur_row, cur_col, cur_vis)) = snap else {
            // pane 이 사라졌으면(셸 종료 등) 빈 배경만 present.
            let _ = a.gpu.render(&[], scale, 0.0, true);
            a.dirty = false;
            return;
        };
        // macOS 는 OS 타이틀바를 투명으로 비웠으므로 이 띠가 그 자리에 얹힌다 —
        // 신호등만 OS 것이고 나머지는 우리가 그린다(메인 창과 같은 방식). 왼쪽에
        // 「아루 · %3 · 2번 방」, 오른쪽에 되돌리기. 창 이동·리사이즈는 OS 몫이다.
        {
            // 학생이 있으면 그 이름을 앞에 세우고 학생색으로 — 메인 그리드에서
            // 테두리·헤더가 하던 "이 창에 누가 있나"를 여기서도 한눈에. 이름은
            // 배정만으로 뜨고(메인 pane 헤더와 같은 규칙) 색은 실제로 도는 pane 만
            // (메인 pane 테두리와 같은 규칙) — 두 규칙이 원래 다르다.
            let label = match &student {
                Some(name) => format!("{name} · {pane_id} · {}번 방", home_window + 1),
                None => format!("{pane_id} · {}번 방", home_window + 1),
            };
            // 배경은 창 배경 그대로 둔다 — 투명 타이틀바 위에 판을 하나 더 깔면
            // 없애려던 두 겹이 색만 바뀐 채 돌아온다. 아래 hairline 만 남긴다.
            a.gpu.rect(0.0, AUX_HEADER_H - 1.0, w, 1.0, crate::theme::border());
            let label_col = if student_col.is_some() { accent } else { crate::theme::text_mute() };
            a.gpu.draw_text(
                AUX_HEADER_X,
                (AUX_HEADER_H - 12.0) / 2.0,
                &label,
                gpu::DrawOpts {
                    font_size: 12.0,
                    color: crate::theme::with_alpha(label_col, if focused { 0xF0 } else { 0x99 }),
                    bold: student.is_some(),
                    italic: false,
                },
            );
            // working 스윕바 — 메인 pane 상단의 그것과 같은 신호다. 없으면 꺼낸
            // 창만 "멈춘 것처럼" 보인다.
            if working {
                const BAR_H: f32 = 2.5;
                let phase = crate::render::anim_phase_secs();
                a.gpu.rect(0.0, 0.0, w, BAR_H, crate::theme::with_alpha(accent, 0x2e));
                let seg = (w * 0.32).clamp(36.0, 160.0);
                let span = w + seg;
                let off = (phase * 0.5).fract() * span - seg;
                let sx = off.max(0.0);
                let ex = (off + seg).min(w);
                if ex > sx {
                    a.gpu.rect(sx, 0.0, ex - sx, BAR_H, accent);
                }
            }
            draw_aux_header_btns(a, w);
        }
        if a.tree_open {
            draw_aux_tree(a, &tree_rows, h);
        }
        // origin_px 는 물리 px(draw_cells 규약), 커서 rect 은 논리 px(gpu.rect 규약).
        let origin_px = (a.cell_left() * scale, AUX_CELL_TOP * scale);
        let slot = gpu::PaneSlot {
            rows: &rows,
            origin_px,
            font_scale: 1.0,
            dim: false,
            links: Vec::new(),
            default_fg: crate::cells::default_fg(),
        };
        a.gpu.draw_cells(&[slot]);
        // 학생 스프라이트 — 메인 그리드와 같은 함수·같은 이미지 키. 셀 위 패스라
        // 비워 둔 자리표시자 위에 얼굴이 또렷하게 얹힌다.
        crate::render::paint_student_overlays(&mut a.gpu, &sprites, anim_ms);
        paint_aux_face_hover(a, &sprites, w);
        // 커서 자리(논리 px). 조합 중 한글이 있으면 그 프리에딧을, 없으면 blink 커서.
        let cw = a.gpu.cell_w;
        let ch = a.gpu.cell_h;
        let px = a.cell_left() + cur_col as f32 * cw;
        let py = AUX_CELL_TOP + cur_row as f32 * ch;
        let pe = a.preedit.clone();
        if pe.is_empty() {
            if cur_vis && focused && blink {
                let mut c = crate::cells::iterm_cursor();
                c[3] = 140; // ~0.55 alpha (paint_gpu_overlays 와 동일)
                a.gpu.rect(px, py, cw, ch, c);
            }
        } else {
            // 조합 중 한글 — 커서 자리에 프리에딧(메인 render 와 동일 draw_preedit).
            a.gpu.draw_preedit(px, py, &pe, crate::cells::iterm_cursor(), 1.0);
        }
        // 창 외곽선 — 셀 위에 얹어야 가장자리 글자에 안 먹힌다. 학생이 있으면 그
        // 색으로(메인 그리드의 active pane 테두리와 같은 신호), 없으면 border 로.
        // **학생 유무와 무관하게 항상 두른다**: OS 타이틀바를 껐으므로 테두리가
        // 없으면 창이 어디서 끝나는지가 배경색 차이 하나뿐이라, 어두운 바탕 위에서
        // 경계가 통째로 사라진다(거노). 포커스가 없을 땐 흐리게.
        {
            const T: f32 = 1.5;
            let base = if student_col.is_some() { accent } else { crate::theme::border() };
            let col = crate::theme::with_alpha(base, if focused { 0xFF } else { 0x66 });
            // 세로 변은 가로 변 **사이만** 채운다. 네 변을 각각 통짜로 그리면 모서리
            // 1.5×1.5 가 두 번 칠해지는데, 포커스가 없을 땐 알파가 0x66 이라 그
            // 네 점만 두 배로 진해져 꼭짓점이 점처럼 튄다(거노: "테두리 꼭짓점이
            // 이상해"). 불투명일 땐 안 보이던 게 반투명이 되며 드러났다.
            a.gpu.rect(0.0, 0.0, w, T, col);
            a.gpu.rect(0.0, h - T, w, T, col);
            a.gpu.rect(0.0, T, T, (h - T * 2.0).max(0.0), col);
            a.gpu.rect(w - T, T, T, (h - T * 2.0).max(0.0), col);
        }
        let _ = a.gpu.render(&[], scale, 0.0, true);
        a.dirty = false;
        // 스윕바는 애니메이션이라 다음 프레임을 스스로 불러야 한다 — PTY 출력이
        // 없으면 아무도 이 창을 다시 그리지 않아 바가 한 자리에 얼어붙는다.
        // working 이 끝나면 이 요청도 끊긴다.
        if working || sprites.animating() {
            a.dirty = true;
            a.window.request_redraw();
        }
    }

    /// 이 방이 지금 별도 창으로 나가 있나. 사이드바 탭 표시와 info 배지가 같은
    /// 판정을 쓰도록 한 곳에 둔다.
    pub(crate) fn window_is_undocked(&self, window: usize) -> bool {
        self.aux_windows.iter().any(|a| a.room_window() == Some(window))
    }

    /// 방 창이 그릴 leaf rect(셀 단위). 창 client 를 셀수로 환산해 그 방 트리를 편다.
    ///
    /// 꺼낸 방은 활성이 아니어서 트리가 `windows[i]` 에 있지만, 활성일 때도 되도록
    /// `pty_layout` 을 함께 본다 — 활성 판정이 한 틱 어긋나도 빈 창이 되지 않는다.
    pub(crate) fn room_leaf_rects(&self, idx: usize) -> Vec<(String, u16, u16, u16, u16)> {
        let Some(a) = self.aux_windows.get(idx) else { return Vec::new() };
        let Some(window) = a.room_window() else { return Vec::new() };
        let (w, h) = a.logical_size();
        let (cw, ch) = (a.gpu.cell_w, a.gpu.cell_h);
        if cw <= 0.0 || ch <= 0.0 {
            return Vec::new();
        }
        let cols = (((w - a.cell_left() - PANE_INNER_X) / cw).floor() as i32).max(1) as u16;
        let rows = (((h - AUX_CELL_TOP - PANE_INNER_Y) / ch).floor() as i32).max(1) as u16;
        let layout = if window == self.active_window {
            self.pty_layout.as_ref()
        } else {
            self.windows.get(window).and_then(|s| s.as_ref())
        };
        layout.map(|l| l.leaf_rects(cols, rows)).unwrap_or_default()
    }

    /// 방 별도창 한 프레임 — 트리를 `leaf_rects` 로 펼쳐 pane 마다 셀 그리드를 그린다.
    /// `draw_cells` 가 슬라이스를 받으므로 pane 이 몇이든 한 번에 올라간다. 포커스
    /// pane 만 또렷하고(나머지 dim) 커서도 거기만 — 메인 창의 관례 그대로다.
    fn aux_room_render(&mut self, idx: usize, blink: bool) {
        let (window, focus, scale, w, h, focused, cw, ch) = {
            let Some(a) = self.aux_windows.get(idx) else { return };
            let AuxWindowKind::Room { window, focus } = &a.kind else { return };
            let (w, h) = a.logical_size();
            (*window, focus.clone(), a.gpu.scale(), w, h, a.focused, a.gpu.cell_w, a.gpu.cell_h)
        };
        let rects = self.room_leaf_rects(idx);
        // 방 이름은 App 만 안다 — title() 이 붙인 「방 N」을 실제 라벨로 승격.
        let label = self
            .window_labels
            .get(window)
            .map(|(n, _)| n.clone())
            .filter(|n| !n.is_empty());
        // 헤더에 쓸 이름. 이름을 안 지은 방이면 번호로 — 띠를 비워 두면 창이
        // 무엇인지 말해주는 게 아무것도 없다(OS 제목을 껐으므로).
        //
        // 포커스 pane 에 학생이 있으면 그 이름을 **앞에** 세운다(터미널 별도창과
        // 같은 「학생 · %N · 방」 꼴). 방 이름만 있으면 pane 을 옮겨 다녀도 지금
        // 누구를 보고 있는지가 어디에도 안 뜬다(거노).
        let room_label = {
            let room = label.clone().unwrap_or_else(|| format!("{}번 방", window + 1));
            match focus.as_deref() {
                Some(pid) => {
                    let who = {
                        let ws = self.ws.lock().unwrap();
                        self.display_pane_char(&ws, pid)
                    };
                    match who {
                        Some(name) => format!("{name} · {pid} · {room}"),
                        None => format!("{pid} · {room}"),
                    }
                }
                None => room,
            }
        };
        // 헤더 라벨 색 — 포커스 pane 의 학생색. 창 테두리·pane 테두리와 같은 신호를
        // 띠에서도 준다(터미널 별도창이 이미 그렇게 한다).
        let label_col = focus.as_deref().and_then(|pid| {
            let ws = self.ws.lock().unwrap();
            self.pane_accent(&ws, pid)
        });
        // pane 별 배정 학생색 — 꺼낸 방도 메인 그리드와 **같은 함수**로 정해야 같은
        // 색이 나온다(`pane_accent`: 이름 + 도는 에이전트 관문 + 동명이인 순번).
        let pane_cols: HashMap<String, [u8; 4]> = {
            let ws = self.ws.lock().unwrap();
            rects
                .iter()
                .filter_map(|(pid, ..)| Some((pid.clone(), self.pane_accent(&ws, pid)?)))
                .collect()
        };
        // working pane 집합도 미리 — 아래에서 창(`a`)을 가변 차용하고 나면 self 를
        // 다시 못 읽는다.
        let working_panes: std::collections::HashSet<String> = rects
            .iter()
            .filter(|(pid, ..)| {
                self.pane_activity.get(pid).is_some_and(|s| s.status == "working")
            })
            .map(|(pid, ..)| pid.clone())
            .collect();
        let any_working = !working_panes.is_empty();
        // 셀은 draw 전에 복사해 둔다 — 그리는 동안 ws lock 을 쥐지 않기 위해서.
        let mut snaps: Vec<_> = {
            let ws = self.ws.lock().unwrap();
            rects
                .iter()
                .filter_map(|(pid, x, y, _, _)| {
                    ws.panes.get(pid).and_then(|p| {
                        p.term().map(|t| {
                            (
                                pid.clone(),
                                t.cells.clone(),
                                *x,
                                *y,
                                t.cursor_row,
                                t.cursor_col,
                                t.cursor_visible,
                            )
                        })
                    })
                })
                .collect()
        };
        // 입력박스 손질 — 터미널 별도창과 같은 이유·같은 함수(거노 2026-08-05:
        // 별도창에 `@aru-p12` 칩이 남아 있었다). 방 창은 pane 이 여럿이라 각자
        // 자기 accent 로 도색한다.
        for (pid, cells, ..) in snaps.iter_mut() {
            let runs_claude = self
                .pty
                .get(pid.as_str())
                .and_then(|p| p.active_agent())
                .is_some();
            if !runs_claude {
                continue;
            }
            crate::render::strip_agent_chip(cells);
            if let Some(col) = pane_cols.get(pid) {
                crate::render::style_prompt_box(cells, *col);
            }
        }
        // 학생 스프라이트 자리 — pane 마다 자기 셀을 훑는다. 사본을 훑으므로
        // 자리표시자 지우기가 draw_cells 에 그대로 반영된다(아래 slots 가 같은
        // 사본을 본다). `aux_windows` 를 가변 차용하기 전에 끝내야 self 를 읽을
        // 수 있다.
        let cell_left = self.aux_windows.get(idx).map_or(PANE_INNER_X, |a| a.cell_left());
        let mut student = crate::render::StudentOverlays::default();
        for (pid, cells, x, y, ..) in snaps.iter_mut() {
            let s = self.aux_student_slots(
                pid,
                cells,
                cell_left + *x as f32 * cw,
                AUX_CELL_TOP + *y as f32 * ch,
                cw,
                ch,
            );
            // 필드를 손으로 합치는 자리 — 하나라도 빠뜨리면 그 종류만 조용히
            // 사라진다(faces 를 빠뜨려 방 창 hover 팝업이 통째로 안 떴다). 늘릴
            // 때 여기도 같이 늘릴 것.
            student.banner.extend(s.banner);
            student.spinner.extend(s.spinner);
            student.waiting.extend(s.waiting);
            student.standing.extend(s.standing);
            student.profile.extend(s.profile);
            student.faces.extend(s.faces);
        }
        let anim_ms = self.version_anim_start.elapsed().as_millis() as u64;
        // 터미널 창과 같은 이유로 `aux_windows` 가변 차용 전에 떠 둔다.
        let tree_rows = if self.aux_windows.get(idx).is_some_and(|a| a.tree_open) {
            self.aux_tree_rows()
        } else {
            Vec::new()
        };
        let Some(a) = self.aux_windows.get_mut(idx) else { return };
        if let Some(name) = label {
            if a.last_title != name {
                a.window.set_title(&name);
                a.last_title = name;
            }
        }
        a.gpu.clear_chrome();
        a.gpu.rect(0.0, 0.0, w, h, crate::theme::bg());
        // 헤더 띠 — macOS 는 OS 타이틀바를 투명으로 비웠으므로 방 이름이 여기 선다.
        // 전엔 OS 가 그린 제목이 그 자리를 채웠는데, 그건 회색 OS 테마라 창 하나만
        // 다른 앱처럼 보였다(거노).
        {
            a.gpu.rect(0.0, AUX_CELL_TOP - 1.0, w, 1.0, crate::theme::border());
            a.gpu.draw_text(
                AUX_HEADER_X,
                (AUX_HEADER_H - 12.0) / 2.0,
                &room_label,
                gpu::DrawOpts {
                    font_size: 12.0,
                    color: crate::theme::with_alpha(
                        label_col.unwrap_or_else(crate::theme::text_mute),
                        if focused { 0xF0 } else { 0x99 },
                    ),
                    bold: label_col.is_some(),
                    italic: false,
                },
            );
            draw_aux_header_btns(a, w);
        }
        if a.tree_open {
            draw_aux_tree(a, &tree_rows, h);
        }
        let left = a.cell_left();
        let slots: Vec<gpu::PaneSlot> = snaps
            .iter()
            .map(|(pid, cells, x, y, _, _, _)| gpu::PaneSlot {
                rows: cells,
                // origin_px 는 물리 px(draw_cells 규약) — 셀 좌표를 논리 px 로 편 뒤 스케일.
                origin_px: (
                    (left + *x as f32 * cw) * scale,
                    (AUX_CELL_TOP + *y as f32 * ch) * scale,
                ),
                font_scale: 1.0,
                dim: focus.as_deref() != Some(pid.as_str()),
                links: Vec::new(),
                default_fg: crate::cells::default_fg(),
            })
            .collect();
        a.gpu.draw_cells(&slots);
        // pane 경계 — 나눠져 있다는 걸 보이게. 포커스 pane 은 그 학생색으로(메인
        // 그리드의 active 테두리와 같은 신호). 학생이 없으면 종전대로 accent.
        for (pid, x, y, pw, ph) in &rects {
            let rx = left + *x as f32 * cw;
            let ry = AUX_CELL_TOP + *y as f32 * ch;
            let (rw, rh) = (*pw as f32 * cw, *ph as f32 * ch);
            let is_focus = focus.as_deref() == Some(pid.as_str());
            let student = pane_cols.get(pid.as_str()).copied();
            let col = if is_focus {
                student.unwrap_or_else(crate::theme::accent)
            } else {
                // 비포커스라도 학생이 있으면 그 색을 흐리게 — 여러 pane 이 나온 방에서
                // 누가 어디 있는지가 경계선만으로 읽힌다.
                student
                    .map(|c| crate::theme::with_alpha(c, 0x55))
                    .unwrap_or_else(crate::theme::border)
            };
            // 세로 변은 가로 변 사이만 — 모서리를 두 번 칠하면 반투명(비포커스
            // 0x55)일 때 네 꼭짓점만 진해져 점처럼 튄다.
            a.gpu.rect(rx, ry, rw, 1.0, col);
            a.gpu.rect(rx, ry + rh - 1.0, rw, 1.0, col);
            a.gpu.rect(rx, ry + 1.0, 1.0, (rh - 2.0).max(0.0), col);
            a.gpu.rect(rx + rw - 1.0, ry + 1.0, 1.0, (rh - 2.0).max(0.0), col);
            // working 스윕바 — 메인 pane 상단의 그것과 같은 자리, 같은 신호.
            if working_panes.contains(pid.as_str()) {
                const BAR_H: f32 = 2.5;
                let base = student.unwrap_or_else(crate::theme::accent);
                let phase = crate::render::anim_phase_secs();
                a.gpu.rect(rx, ry, rw, BAR_H, crate::theme::with_alpha(base, 0x2e));
                let seg = (rw * 0.32).clamp(36.0, 160.0);
                let off = (phase * 0.5).fract() * (rw + seg) - seg;
                let (sx, ex) = ((rx + off).max(rx), (rx + off + seg).min(rx + rw));
                if ex > sx {
                    a.gpu.rect(sx, ry, ex - sx, BAR_H, base);
                }
            }
        }
        // 창 전체 외곽선은 **여기 넣지 않는다** — 방 창은 pane 마다 테두리를 두고,
        // 창 겉을 두르는 건 pane 별도창만이다(거노 2026-08-05). 어제 이 자리에
        // 외곽선을 넣었다가 되돌렸다: 방 창은 안에 pane 이 여럿이라 겉테두리까지
        // 두르면 테두리가 두 겹이 되고, 어느 pane 이 포커스인지를 말해야 할 색이
        // 창 전체로 번져 신호가 흐려진다.
        // 학생 스프라이트는 셀 위 패스 — statusline 테두리 글리프가 얼굴을
        // 가로지르지 않게. 메인 그리드와 같은 함수·같은 이미지 키를 쓴다.
        crate::render::paint_student_overlays(&mut a.gpu, &student, anim_ms);
        paint_aux_face_hover(a, &student, w);
        // 커서/프리에딧은 포커스 pane 자리에만.
        if let Some((_, _, x, y, cur_row, cur_col, cur_vis)) = snaps
            .iter()
            .find(|(pid, ..)| focus.as_deref() == Some(pid.as_str()))
        {
            let px = left + (*x as f32 + *cur_col as f32) * cw;
            let py = AUX_CELL_TOP + (*y as f32 + *cur_row as f32) * ch;
            let pe = a.preedit.clone();
            if pe.is_empty() {
                if *cur_vis && focused && blink {
                    let mut c = crate::cells::iterm_cursor();
                    c[3] = 140;
                    a.gpu.rect(px, py, cw, ch, c);
                }
            } else {
                a.gpu.draw_preedit(px, py, &pe, crate::cells::iterm_cursor(), 1.0);
            }
        }
        let _ = a.gpu.render(&[], scale, 0.0, true);
        a.dirty = false;
        // 스윕바가 도는 동안은 다음 프레임을 스스로 부른다(터미널 창과 같은 이유).
        // 움직이는 학생 스프라이트도 같은 이유로 — 정적 프사만 있으면 안 깨운다.
        if any_working || student.animating() {
            a.dirty = true;
            a.window.request_redraw();
        }
    }

    /// 이 pane 의 셀을 훑어 학생 스프라이트 자리를 모은다(별도창 좌표계).
    ///
    /// 메인 그리드가 `render_frame` 에서 하는 일과 같다 — 다른 건 좌표뿐이다.
    /// **찾는 규칙은 공유**한다(`find_statusline_face`·`find_standing_anchor`·
    /// `find_clawd_banners`·`find_claude_spinner`): 같은 화면인데 창마다 학생이
    /// 다른 자리에 서면 그게 곧 버그다. 그리는 것도 `paint_student_overlays`
    /// 한 곳이라, 이미지 키와 프레임 규칙이 갈릴 데가 없다.
    ///
    /// `cells` 는 창이 들고 있는 **사본**이라 자리표시자를 지워도 원본 화면은
    /// 안 상한다 — 지우지 않으면 U+FFFC 가 빈 네모로 얼굴 밑에 남는다.
    ///
    /// 메인과 같은 두 게이트를 지난다: claude 가 실제로 도는 pane 이어야 하고
    /// (남의 TUI 를 claude 로 오인하지 않기 위해), 그 pane 에 학생이 배정돼
    /// 있어야 한다.
    pub(crate) fn aux_student_slots(
        &self,
        pane_id: &str,
        cells: &mut [Vec<GridCell>],
        ox: f32,
        oy: f32,
        cw: f32,
        ch: f32,
    ) -> crate::render::StudentOverlays {
        let mut out = crate::render::StudentOverlays::default();
        if !self
            .pty
            .get(pane_id)
            .and_then(|p| p.active_agent())
                .is_some()
        {
            return out;
        }
        let Some((name, slug)) = ({
            let ws = self.ws.lock().unwrap();
            self.display_pane_char(&ws, pane_id)
                .and_then(|n| crate::theme::character_slug(&n).map(|s| (n, s)))
        }) else {
            return out;
        };
        let cols = cells.first().map_or(0, |r| r.len());
        let rows = cells.len();
        // Clawd 배너 → 학생 도트. 스크롤로 위아래가 잘리면 셀과 함께 잘리도록
        // pane 세로 범위로 클립한다.
        for (br, bc) in crate::render::find_clawd_banners(cells) {
            out.banner.push((
                slug,
                (
                    ox + bc as f32 * cw,
                    oy + br as f32 * ch,
                    crate::render::CLAWD_COLS as f32 * cw,
                    crate::render::CLAWD_ROWS as f32 * ch,
                ),
                (oy, oy + rows as f32 * ch),
            ));
            let r0 = br.max(0) as usize;
            let r1 = (br + crate::render::CLAWD_ROWS as isize).clamp(0, rows as isize) as usize;
            for row in cells[r0..r1].iter_mut() {
                for cell in row.iter_mut().skip(bc).take(crate::render::CLAWD_COLS) {
                    *cell = GridCell::blank();
                }
            }
        }
        // working 스피너 → 제자리 걸음. 스피너가 도는 동안은 standing 을 세우지
        // 않는다 — 같은 학생이 화면에 둘이면 버그로 보인다(메인과 같은 규칙).
        let mut busy = false;
        if let Some((sr, sc)) = crate::render::find_claude_spinner(cells) {
            busy = true;
            let top_r = sr.saturating_sub(1);
            out.spinner.push((
                slug,
                (
                    ox + sc as f32 * cw,
                    oy + top_r as f32 * ch,
                    2.0 * cw,
                    (sr - top_r + 1) as f32 * ch,
                ),
            ));
            if let Some(row) = cells.get_mut(sr) {
                if let Some(cell) = row.get_mut(sc) {
                    *cell = GridCell::blank();
                }
            }
        }
        let mut stand_anchor: Option<(usize, f32)> = None;
        if let Some((sr, sc, len)) = crate::render::find_statusline_face(cells) {
            for cell in cells[sr].iter_mut().skip(sc).take(len) {
                *cell = GridCell::blank();
            }
            let face_h = crate::render::STATUSLINE_FACE_ROWS as f32 * ch;
            let face_rect = (
                ox + sc as f32 * cw,
                (oy + (sr + 1) as f32 * ch - face_h).max(oy),
                len as f32 * cw,
                face_h,
            );
            out.profile.push((slug, face_rect));
            out.faces.push((name, slug, face_rect));
            stand_anchor = crate::render::find_standing_anchor(cells, sr, cols);
        }
        // statusline 자리표시자가 없는 하네스(codex)는 입력행에서 바로 — 메인 창과
        // 같은 규칙을 쓴다(`find_filled_standing_anchor`).
        if stand_anchor.is_none() {
            stand_anchor = crate::render::find_filled_standing_anchor(cells, cols);
        }
        {
            if !busy {
                if let Some((anchor, left_c)) = stand_anchor {
                    let h = crate::render::INPUT_STANDING_ROWS as f32 * ch;
                    // 턴이 끝났으면 손 흔들며 기다리는 wave, 아니면 idle. 완료
                    // 직후 cheer 는 메인 창의 notify_flash 타이머에 매인 연출이라
                    // 별도창에선 그 두 상태만 쓴다.
                    let motion = if self.turn_done_panes.contains(pane_id) {
                        "wave"
                    } else {
                        "idle"
                    };
                    out.standing.push((
                        slug,
                        motion,
                        (
                            ox + left_c * cw,
                            (oy + (anchor + 1) as f32 * ch - h).max(oy),
                            crate::render::STAND_CELLS * cw,
                            h,
                        ),
                    ));
                }
            }
        }
        out
    }

    /// 이 창이 뷰하는 pane 의 PTY 로 바이트 전송(빈 입력 무시).
    fn aux_term_send(&self, pane_id: &str, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        if let Some(pty) = self.pty.get(pane_id) {
            let _ = pty.send_bytes(bytes);
        }
    }

    /// 터미널 별도창 이벤트 라우팅. Resized/Scale 는 PTY resize 까지, Close 는 dock 복귀.
    fn aux_terminal_event(
        &mut self,
        idx: usize,
        event: WindowEvent,
        event_loop: &ActiveEventLoop,
    ) {
        let _ = event_loop;
        match event {
            WindowEvent::CloseRequested => self.dock_pane_terminal(idx),
            WindowEvent::Resized(size) => {
                if let Some(a) = self.aux_windows.get_mut(idx) {
                    a.gpu.resize(size.width, size.height);
                    a.dirty = true;
                    a.window.request_redraw();
                }
                self.aux_terminal_resize_pty(idx);
            }
            WindowEvent::ScaleFactorChanged { .. } => {
                if let Some(a) = self.aux_windows.get_mut(idx) {
                    let sf = a.window.scale_factor() as f32;
                    a.gpu.set_scale(sf);
                    a.gpu.set_font_size(FONT_SIZE);
                    let sz = a.window.inner_size();
                    a.gpu.resize(sz.width, sz.height);
                    a.dirty = true;
                    a.window.request_redraw();
                }
                self.aux_terminal_resize_pty(idx);
            }
            WindowEvent::Focused(f) => {
                if let Some(a) = self.aux_windows.get_mut(idx) {
                    a.focused = f;
                    a.window.request_redraw();
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                let scale = self.aux_windows.get(idx).map(|a| a.gpu.scale()).unwrap_or(1.0);
                if let Some(a) = self.aux_windows.get_mut(idx) {
                    let was_header = a.cursor_px.1 <= AUX_HEADER_H;
                    let was_face = a.over_face(a.cursor_px);
                    a.cursor_px = (position.x as f32 / scale, position.y as f32 / scale);
                    // 헤더를 지나는 동안만 다시 그린다 — 버튼 hover 는 그 띠에서만
                    // 바뀌고, 셀 위에서 매 픽셀 재렌더하면 그냥 낭비다. 띠를 벗어나는
                    // 프레임도 한 번은 그려야 hover 가 남아 굳지 않는다.
                    // 프사는 그 띠 밖이라 따로 봐야 한다 — 들고 나는 **경계에서만**
                    // 그리면 팝업이 뜨고 지면서도 셀 위 매 픽셀 재렌더는 피한다.
                    let is_face = a.over_face(a.cursor_px);
                    if was_header || a.cursor_px.1 <= AUX_HEADER_H || was_face != is_face {
                        a.dirty = true;
                        a.window.request_redraw();
                    }
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                if !self.aux_header_click(idx) {
                    self.aux_tree_click(idx);
                }
            }
            WindowEvent::MouseWheel { delta, .. } => self.aux_terminal_wheel(idx, delta),
            WindowEvent::KeyboardInput { event, .. } => self.aux_terminal_key(idx, &event),
            WindowEvent::Ime(ime) => self.aux_terminal_ime(idx, ime),
            WindowEvent::RedrawRequested => self.aux_render(idx),
            _ => {}
        }
    }

    /// 헤더 버튼 클릭 처리. 눌린 게 없으면 `false` — 호출부가 평소 경로를 잇는다.
    pub(crate) fn aux_header_click(&mut self, idx: usize) -> bool {
        let Some(a) = self.aux_windows.get(idx) else { return false };
        let (cx, cy) = a.cursor_px;
        let hit = a
            .header_btns
            .iter()
            .find(|(_, (bx, by, bw, bh))| {
                cx >= *bx && cx <= bx + bw && cy >= *by && cy <= by + bh
            })
            .map(|(k, _)| *k);
        match hit {
            Some(AuxHeaderBtn::Hide) => {
                self.hide_aux_window(idx);
                true
            }
            Some(AuxHeaderBtn::Dock) => {
                // 창 종류마다 되돌리는 곳이 다르다 — pane 은 메인 그리드로,
                // 방은 방 목록으로. 둘 다 창 닫기(⌘W)와 같은 경로다.
                match self.aux_windows.get(idx).map(|a| matches!(a.kind, AuxWindowKind::Room { .. }))
                {
                    Some(true) => self.dock_window_room(idx),
                    Some(false) => self.dock_pane_terminal(idx),
                    None => {}
                }
                true
            }
            Some(AuxHeaderBtn::FileTree) => {
                self.toggle_aux_tree(idx);
                true
            }
            // 두 칸이 각자 자기 모드를 **지정**한다(뒤집기가 아니라) — 메인 그리드
            // 알약과 같은 규약이라, 이미 그 모드면 아무 일도 안 일어난다.
            Some(AuxHeaderBtn::MdRender) => {
                self.aux_set_md_mode(idx, false);
                true
            }
            Some(AuxHeaderBtn::MdRaw) => {
                self.aux_set_md_mode(idx, true);
                true
            }
            Some(AuxHeaderBtn::Pin) => {
                if let Some(a) = self.aux_windows.get_mut(idx) {
                    a.pinned = !a.pinned;
                    a.window.set_window_level(if a.pinned {
                        winit::window::WindowLevel::AlwaysOnTop
                    } else {
                        winit::window::WindowLevel::Normal
                    });
                    a.window.request_redraw();
                }
                true
            }
            None => false,
        }
    }

    /// 이 창의 파일트리 패널을 열고 닫는다. 셀 그리드가 트리 폭만큼 밀리므로
    /// **PTY 도 같이 좁혀야** 한다 — 안 그러면 셸이 옛 폭으로 줄바꿈해 오른쪽이
    /// 창 밖으로 나간다. 트리를 처음 열 때 루트가 아직 없으면 세우고 채운다:
    /// 메인 창에서 트리를 한 번도 안 열었으면 `file_tree.nodes` 가 비어 있어,
    /// 패널이 열리긴 하는데 빈 판으로 뜬다(고장으로 읽힌다).
    pub(crate) fn toggle_aux_tree(&mut self, idx: usize) {
        let Some(a) = self.aux_windows.get_mut(idx) else { return };
        a.tree_open = !a.tree_open;
        let opened = a.tree_open;
        let is_room = matches!(a.kind, AuxWindowKind::Room { .. });
        if opened && self.file_tree.nodes.is_empty() {
            if self.file_tree.root.is_none() {
                // 이 창이 보는 pane 의 cwd 가 가장 그럴듯한 루트다 — 트리를 여는
                // 사람은 "여기서 일하는 폴더"를 보려는 것이다.
                let cwd = self
                    .aux_windows
                    .get(idx)
                    .and_then(|a| a.term_pane_id())
                    .and_then(|p| self.pane_cwd_cache.get(p).cloned())
                    .or_else(|| std::env::current_dir().ok());
                if let Some(root) = cwd {
                    // 루트는 펼친 채로 연다 — 메인 창이 루트를 세울 때와 같은 규칙.
                    // 안 그러면 트리를 열었는데 접힌 폴더 한 줄만 떠, 켠 게 아니라
                    // 고장난 것처럼 보인다.
                    self.file_tree.expanded.insert(root.clone());
                    self.file_tree.root = Some(root);
                }
            }
            self.rebuild_file_tree_nodes();
        }
        if is_room {
            self.aux_room_resize_pty(idx);
        } else {
            self.aux_terminal_resize_pty(idx);
        }
        self.aux_redraw(idx);
    }

    /// 그리기 직전에 뜨는 트리 스냅샷. `App.file_tree.nodes` 를 `aux_windows` 가변
    /// 차용과 같은 자리에서 읽을 수 없어 필요한 것만 옮겨 담는다.
    fn aux_tree_rows(&self) -> Vec<AuxTreeRow> {
        self.file_tree
            .nodes
            .iter()
            .map(|n| AuxTreeRow {
                path: n.path.clone(),
                name: n.name.clone(),
                is_dir: n.is_dir,
                depth: n.depth,
                expanded: self.file_tree.expanded.contains(&n.path),
                ignored: n.ignored,
                is_repo: n.is_repo,
            })
            .collect()
    }

    /// 트리 패널 클릭. 폴더는 펼치고/접고(메인 창과 **같은** 펼침 상태를 쓰므로
    /// 양쪽이 함께 움직인다), 파일은 연다. 판정은 `true` = 트리가 먹었다.
    fn aux_tree_click(&mut self, idx: usize) -> bool {
        let Some(a) = self.aux_windows.get(idx) else { return false };
        if !a.tree_open {
            return false;
        }
        let (cx, cy) = a.cursor_px;
        if cx >= AUX_TREE_W || cy < AUX_HEADER_H {
            return false;
        }
        let hit = a
            .tree_rows
            .iter()
            .find(|(_, _, (_, ry, _, rh))| cy >= *ry && cy < ry + rh)
            .map(|(p, d, _)| (p.clone(), *d));
        // 패널 빈자리를 눌러도 셀 클릭으로 새면 안 된다 — 트리 위는 트리 것이다.
        let Some((path, is_dir)) = hit else { return true };
        if is_dir {
            if !self.file_tree.expanded.remove(&path) {
                self.file_tree.expanded.insert(path.clone());
            }
            self.rebuild_file_tree_nodes();
            self.chrome_dirty = true;
        } else {
            // 파일은 메인 창에 연다(`open_file_split` — 사이드바 트리와 같은 경로).
            // 별도창 안에서 여는 길은 아직 없다: 이 창은 pane 하나를 보는 뷰라
            // 새 탭을 담을 자리가 없다.
            self.open_file_split(path);
        }
        self.aux_redraw(idx);
        true
    }

    /// 별도창을 접어 메인 창 하단바 칩으로 보낸다. pane·PTY·방 트리는 그대로
    /// 두고 **창만** 없앤다 — 그래서 되살리기가 `spawn_aux_*` 재호출로 끝난다.
    ///
    /// 되돌리기(dock)와 다르다: 되돌리기는 pane 을 메인 그리드에 다시 꽂아 레이아웃을
    /// 바꾸고, 접기는 꺼내 둔 상태를 유지한 채 화면에서만 물러난다. OS 최소화와도
    /// 다르다 — Dock 이 아니라 일하던 창 안으로 들어가므로, 꺼내 둔 게 몇인지
    /// 거기서 한눈에 보인다(거노).
    pub(crate) fn hide_aux_window(&mut self, idx: usize) {
        let Some(a) = self.aux_windows.get(idx) else { return };
        let pos = a.window.outer_position().ok();
        let what = match &a.kind {
            AuxWindowKind::Terminal { pane_id, window, .. } => {
                crate::HiddenAuxKind::Terminal { pane_id: pane_id.clone(), home_window: *window }
            }
            AuxWindowKind::Room { window, .. } => crate::HiddenAuxKind::Room { window: *window },
            // 편집기·설정은 접을 자리가 없다(하단바는 pane 그리드의 띠다).
            _ => return,
        };
        let label = match &what {
            crate::HiddenAuxKind::Terminal { pane_id, .. } => {
                let ws = self.ws.lock().unwrap();
                match ws.pane_character.get(pane_id) {
                    Some(name) => format!("{name} · {pane_id}"),
                    None => pane_id.clone(),
                }
            }
            crate::HiddenAuxKind::Room { window } => self
                .window_labels
                .get(*window)
                .map(|(n, _)| n.clone())
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| format!("{}번 방", window + 1)),
        };
        self.hidden_aux.push(crate::HiddenAux { label, what, pos });
        self.aux_windows.remove(idx);
        // 하단바가 새로 생기면 그리드가 그만큼 줄어든다 — PTY 도 같이 줄여야
        // 마지막 줄이 띠 밑으로 숨지 않는다.
        let (cols, rows) = self.window_cells();
        self.resize_backend(cols, rows);
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
    }

    /// 접어 둔 별도창을 다시 세운다. 그 사이 pane 이 사라졌거나 방이 없어졌으면
    /// 조용히 목록에서만 지운다 — 되살릴 대상이 없는데 빈 창을 띄우면 그게 더 나쁘다.
    pub(crate) fn unhide_aux(&mut self, i: usize, event_loop: &ActiveEventLoop) {
        if i >= self.hidden_aux.len() {
            return;
        }
        let h = self.hidden_aux.remove(i);
        match h.what {
            crate::HiddenAuxKind::Terminal { pane_id, home_window } => {
                if self.pty.contains_key(&pane_id) {
                    // 접었다 되살리는 창은 트리에 leaf 가 없다 — 잴 칸수가 없으므로 기본 크기.
                    self.spawn_aux_terminal(pane_id, home_window, event_loop, h.pos, None);
                }
            }
            crate::HiddenAuxKind::Room { window } => {
                self.spawn_aux_room(window, None, event_loop, h.pos);
            }
        }
        let (cols, rows) = self.window_cells();
        self.resize_backend(cols, rows);
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
    }

    /// 휠 → PTY 스크롤백(alacritty display_offset). 위로(y>0)=과거로.
    pub(crate) fn aux_terminal_wheel(&mut self, idx: usize, delta: MouseScrollDelta) {
        let pane_id = match self.aux_windows.get(idx).and_then(|a| a.term_pane_id()) {
            Some(p) => p.to_string(),
            None => return,
        };
        let lines = match delta {
            MouseScrollDelta::LineDelta(_, y) => y,
            MouseScrollDelta::PixelDelta(p) => (p.y as f32) / 20.0,
        };
        if lines.abs() < 0.01 {
            return;
        }
        // 커서가 트리 위면 트리를 굴린다 — 같은 창 안에 스크롤 대상이 둘이면
        // 어느 쪽이냐는 커서가 정한다(메인 창 사이드바와 같은 규칙). 클램프는
        // 그리는 쪽이 rows 를 알고 있으므로 draw_aux_tree 가 맡는다.
        let on_tree = self
            .aux_windows
            .get(idx)
            .is_some_and(|a| a.tree_open && a.cursor_px.0 < AUX_TREE_W && a.cursor_px.1 >= AUX_HEADER_H);
        if on_tree {
            if let Some(a) = self.aux_windows.get_mut(idx) {
                a.tree_scroll = (a.tree_scroll - lines * AUX_TREE_ROW_H).max(0.0);
            }
            self.aux_redraw(idx);
            return;
        }
        let step = lines.abs().ceil() as i32;
        if let Some(pty) = self.pty.get(&pane_id) {
            pty.scroll(if lines > 0.0 { step } else { -step });
        }
        self.aux_redraw(idx);
    }

    /// 터미널 별도창 키 입력 → PTY 바이트. forward_key 의 셸 전송부만 축약 재현
    /// (git/파일트리/이미지 side effect 없음). 한글은 편집기 aux 와 동일한 in-process
    /// composer(self.hangul) 경로.
    fn aux_terminal_key(&mut self, idx: usize, event: &KeyEvent) {
        use winit::keyboard::{Key, KeyCode, NamedKey, PhysicalKey};
        if event.state != ElementState::Pressed {
            return;
        }
        self.last_input_at = Instant::now();
        let pane_id = match self.aux_windows.get(idx).and_then(|a| a.term_pane_id()) {
            Some(p) => p.to_string(),
            None => return,
        };
        self.ime_retarget(crate::ImeFocus::Pane(pane_id.clone()));
        // OS 키 자동반복은 Cmd 조합에선 삼킨다 — 메인 창(forward_key)과 같은 규칙.
        // Cmd 단축키는 전부 단발성이라, 살짝 길게 눌린 것만으로 여러 번 발사된다.
        if self.host_mod() && event.repeat {
            return;
        }
        // Cmd/Ctrl+W: 이 창 닫기 → dock 복귀. 방 창이면 방을, 아니면 pane 을 되돌린다.
        if self.host_mod() && matches!(event.physical_key, PhysicalKey::Code(KeyCode::KeyW)) {
            if self.aux_windows.get(idx).and_then(|a| a.room_window()).is_some() {
                self.dock_window_room(idx);
            } else {
                self.dock_pane_terminal(idx);
            }
            return;
        }
        // Cmd+D / Cmd+Shift+D / Cmd+E: split — **방 창만**. pane 하나짜리 터미널
        // 별도창은 leaf 하나를 창 전체에 그리므로 쪼개 봐야 새 pane 이 어디에도 안
        // 보이고 셸만 는다. 화음은 메인 창(input::forward_key)과 같게 맞춘다.
        if self.host_mod() && self.aux_windows.get(idx).and_then(|a| a.room_window()).is_some() {
            if let PhysicalKey::Code(code) = event.physical_key {
                let dir = match code {
                    KeyCode::KeyD if self.host_mod_alt() => Some(kasa_pty::SplitDir::Vertical),
                    KeyCode::KeyD => Some(kasa_pty::SplitDir::Horizontal),
                    KeyCode::KeyE => Some(kasa_pty::SplitDir::Vertical),
                    _ => None,
                };
                if let Some(dir) = dir {
                    self.split_room_pane(idx, dir);
                    return;
                }
            }
        }
        // 폰트 줌(Cmd+= / Cmd+- / Cmd+0). **한글 조합 분기보다 먼저** 와야 한다 —
        // 조합 중엔 아래 자모 경로나 맨 끝 평문 경로가 이 키를 먼저 먹어 셸에 '-' 가
        // 박혔다(별도창엔 줌 처리가 아예 없었다). 메인 창(forward_key)과 같은 규칙으로
        // **물리키와 논리문자를 둘 다** 본다: 한글·유럽 배열은 같은 문자를 다른 물리
        // 위치에서 내놓는다. 메인 창은 App 전역 ui_zoom 을 움직이지만 별도창은 자기
        // gpu 의 폰트 크기가 전부라, 여기서 그 값을 직접 올리고 PTY 를 리플로우한다.
        let zoom_mod = if cfg!(target_os = "macos") {
            self.host_mod()
        } else {
            self.modifiers.control_key()
        };
        if zoom_mod {
            let logical_str = match &event.logical_key {
                Key::Character(s) => Some(s.as_str()),
                _ => None,
            };
            let code = match event.physical_key {
                PhysicalKey::Code(c) => Some(c),
                _ => None,
            };
            if let Some(z) = crate::input::zoom_key(code, logical_str) {
                if let Some(a) = self.aux_windows.get_mut(idx) {
                    let next = match z {
                        crate::input::ZoomKey::Reset => FONT_SIZE,
                        crate::input::ZoomKey::In => (a.gpu.font_size() + 1.0).clamp(8.0, 40.0),
                        crate::input::ZoomKey::Out => (a.gpu.font_size() - 1.0).clamp(8.0, 40.0),
                    };
                    a.gpu.set_font_size(next);
                    a.dirty = true;
                    a.window.request_redraw();
                }
                // 셀이 커졌으니 같은 창에 들어가는 칸 수가 달라진다 — 셸이 SIGWINCH
                // 로 리플로우하지 않으면 글자만 커지고 줄바꿈이 옛 폭에 묶인다.
                self.aux_terminal_resize_pty(idx);
                return;
            }
        }
        // macOS in-process 한글 조합: 자모(U+3130..318F)면 completer 로, 완성 음절만 PTY.
        #[cfg(target_os = "macos")]
        if let Some(t) = &event.text {
            if t.chars().count() == 1 {
                if let Some(c) = t.chars().next() {
                    if (0x3130..=0x318F).contains(&(c as u32)) {
                        if let Some(commit) = self.hangul.feed(c) {
                            self.aux_term_send(&pane_id, commit.as_bytes());
                        }
                        let pe = self.hangul.preedit().unwrap_or_default();
                        if let Some(a) = self.aux_windows.get_mut(idx) {
                            a.preedit = pe;
                        }
                        self.aux_redraw(idx);
                        return;
                    }
                }
            }
        }
        // Ctrl(+letter) → 제어바이트. host_mod(Cmd)는 제외(위 Cmd+W 외엔 삼킴).
        if self.modifiers.control_key() && !self.host_mod() {
            if let PhysicalKey::Code(code) = event.physical_key {
                if let Some(b) = ctrl_byte(code) {
                    self.aux_term_flush_hangul(idx, &pane_id);
                    self.aux_term_send(&pane_id, &[b]);
                    return;
                }
            }
        }
        // 특수키 → ANSI 시퀀스.
        let seq: Option<&[u8]> = match &event.logical_key {
            Key::Named(NamedKey::Enter) => Some(b"\r"),
            Key::Named(NamedKey::Tab) => Some(b"\t"),
            Key::Named(NamedKey::Escape) => Some(b"\x1b"),
            Key::Named(NamedKey::Backspace) => {
                // 조합 중이면 자모를 하나 빼고(셸로 안 보냄), 아니면 DEL.
                if self.hangul.backspace() {
                    let pe = self.hangul.preedit().unwrap_or_default();
                    if let Some(a) = self.aux_windows.get_mut(idx) {
                        a.preedit = pe;
                    }
                    self.aux_redraw(idx);
                    return;
                }
                Some(b"\x7f")
            }
            Key::Named(NamedKey::ArrowUp) => Some(b"\x1b[A"),
            Key::Named(NamedKey::ArrowDown) => Some(b"\x1b[B"),
            Key::Named(NamedKey::ArrowRight) => Some(b"\x1b[C"),
            Key::Named(NamedKey::ArrowLeft) => Some(b"\x1b[D"),
            Key::Named(NamedKey::Home) => Some(b"\x1b[H"),
            Key::Named(NamedKey::End) => Some(b"\x1b[F"),
            Key::Named(NamedKey::PageUp) => Some(b"\x1b[5~"),
            Key::Named(NamedKey::PageDown) => Some(b"\x1b[6~"),
            Key::Named(NamedKey::Delete) => Some(b"\x1b[3~"),
            _ => None,
        };
        if let Some(bytes) = seq {
            self.aux_term_flush_hangul(idx, &pane_id);
            self.aux_term_send(&pane_id, bytes);
            return;
        }
        // 평문 텍스트(자모는 위에서 소비됨) — 조합 잔여를 먼저 확정하고 그대로 전송.
        if let Some(t) = &event.text {
            if !t.is_empty() {
                self.aux_term_flush_hangul(idx, &pane_id);
                self.aux_term_send(&pane_id, t.as_bytes());
                self.aux_redraw(idx);
            }
        }
    }

    /// 조합 중인 음절을 확정해 PTY 로 보내고 프리에딧을 비운다(제어/특수/평문 전에).
    fn aux_term_flush_hangul(&mut self, idx: usize, pane_id: &str) {
        if let Some(flushed) = self.hangul.flush() {
            self.aux_term_send(pane_id, flushed.as_bytes());
        }
        if let Some(a) = self.aux_windows.get_mut(idx) {
            a.preedit.clear();
        }
    }

    /// 비-macOS(Windows/Linux) OS IME — Preedit 는 이 창 프리에딧, Commit 은 PTY 전송.
    fn aux_terminal_ime(&mut self, idx: usize, ime: Ime) {
        match ime {
            Ime::Enabled | Ime::Disabled => {
                if let Some(a) = self.aux_windows.get_mut(idx) {
                    a.preedit.clear();
                }
            }
            Ime::Preedit(text, _) => {
                if let Some(a) = self.aux_windows.get_mut(idx) {
                    a.preedit = text;
                }
            }
            Ime::Commit(text) => {
                let pane_id = self
                    .aux_windows
                    .get(idx)
                    .and_then(|a| a.term_pane_id())
                    .map(|s| s.to_string());
                if let Some(a) = self.aux_windows.get_mut(idx) {
                    a.preedit.clear();
                }
                if let Some(pid) = pane_id {
                    self.aux_term_send(&pid, text.as_bytes());
                }
            }
        }
        self.aux_redraw(idx);
    }

    // ── undock / dock ────────────────────────────────────────────────────────

    /// `pane_id` 터미널 pane 을 별도창으로 undock. **핵심**: `remove_pane` 을 쓰면
    /// `self.pty.remove` 로 세션까지 죽으므로 쓰지 않는다 — 레이아웃 트리에서 leaf 만
    /// 빼고 `self.pty`·`ws.panes` 는 유지해, PtySession 이 살아있고 그 셀 그리드를
    /// 별도창이 계속 뷰한다. 진입점 = 헤더 pop-out 아이콘 클릭(near=None) +
    /// 탭을 창 밖으로 드래그(tear-off, near=커서 물리좌표 — 파일 탭과 동일 제스처).
    pub(crate) fn undock_pane_terminal(
        &mut self,
        pane_id: &str,
        event_loop: &ActiveEventLoop,
        near: Option<winit::dpi::PhysicalPosition<i32>>,
    ) {
        // 이미 별도창이면 포커스만.
        if let Some(i) = self
            .aux_windows
            .iter()
            .position(|a| a.term_pane_id() == Some(pane_id))
        {
            self.aux_windows[i].window.focus_window();
            return;
        }
        // tmux 백엔드는 로컬 PTY 소유가 아니라 미지원. PTY 없는 pane(이미지/md)도 무시.
        if self.tmux.is_some() || !self.pty.contains_key(pane_id) {
            return;
        }
        // 나온 방을 지금 붙들어야 한다 — 아래에서 트리의 leaf 를 빼고 나면
        // `window_of_pane` 이 더는 못 찾는다.
        let home_window = self.window_of_pane(pane_id).unwrap_or(self.active_window);
        let leaves: Vec<String> = self
            .pty_layout
            .as_ref()
            .map(|t| t.leaves().iter().map(|s| s.to_string()).collect())
            .unwrap_or_default();
        // 활성 트리에 없는 pane(스태시/백그라운드)은 v1 범위 밖.
        if !leaves.iter().any(|l| l == pane_id) {
            return;
        }
        // 지금 이 pane 이 차지한 칸수 — **leaf 를 빼기 전에** 재야 한다. 빼고 나면
        // 트리에 없어 rect 를 못 구하고, 그러면 창이 고정 크기로 열려 보던 화면이
        // 잘린다.
        let want_cells = {
            let (cols, rows) = self.window_cells();
            self.pty_layout.as_ref().and_then(|t| {
                t.leaf_rects(cols, rows)
                    .into_iter()
                    .find(|(id, ..)| id == pane_id)
                    .map(|(_, _, _, w, h)| (w, h))
            })
        };
        let was_active =
            self.ws.lock().unwrap().active_pane.as_deref() == Some(pane_id);
        // 제거 pane 이 active 였으면 형제 leaf 로 포커스 이동(remove_pane 과 동일 규칙).
        let next_focus = if was_active && leaves.len() > 1 {
            let i = leaves.iter().position(|l| l == pane_id).unwrap_or(0);
            Some(if i + 1 < leaves.len() {
                leaves[i + 1].clone()
            } else {
                leaves[i - 1].clone()
            })
        } else {
            None
        };
        if leaves.len() > 1 {
            if let Some(tree) = self.pty_layout.as_mut() {
                tree.remove_leaf(pane_id);
            }
        } else {
            // 마지막 leaf — 트리 통째 드랍(단일 pane 폴백 재engage). 메인 창은 잠시 빈다;
            // dock 복귀나 새 split 이 다시 채운다.
            self.pty_layout = None;
        }
        {
            let mut ws = self.ws.lock().unwrap();
            if was_active {
                ws.active_pane = next_focus;
            }
            for pane in ws.panes.values_mut() {
                pane.dirty = true;
            }
        }
        let (cols, rows) = self.window_cells();
        self.resize_backend(cols, rows);
        self.publish_pty_layout();
        if let Some(w) = &self.window {
            w.request_redraw();
        }
        self.spawn_aux_terminal(pane_id.to_string(), home_window, event_loop, near, want_cells);
    }

    /// 터미널 별도창을 닫으며 그 pane 을 메인 레이아웃으로 되돌린다(dock). 창을 먼저
    /// 제거하고(idx 확보 후), 살아있는 세션이면 활성 pane 오른쪽(Horizontal)에
    /// `split_leaf` 로 기존 pane_id 를 재삽입한다 — 새 세션을 만드는 split_active_pane
    /// 과 달리 기존 PtySession 을 그대로 얹으므로 셸이 안 끊긴다.
    pub(crate) fn dock_pane_terminal(&mut self, idx: usize) {
        let pane_id = match self.aux_windows.get(idx).and_then(|a| a.term_pane_id()) {
            Some(p) => p.to_string(),
            None => {
                self.close_aux_window(idx);
                return;
            }
        };
        // 나온 방을 창이 들고 있다 — 닫기 전에 꺼내야 한다.
        let home = match self.aux_windows.get(idx).map(|a| &a.kind) {
            Some(AuxWindowKind::Terminal { window, .. }) => Some(*window),
            _ => None,
        };
        self.close_aux_window(idx);
        // 셸이 이미 종료돼 세션이 사라졌으면 되돌릴 게 없다.
        if !self.pty.contains_key(&pane_id) {
            return;
        }
        // 나왔던 방으로 돌아간다. 이게 없으면 그때 보고 있던 방에 남의 pane 이
        // 튀어나온다 — 꺼낼 때와 되돌릴 때 활성 방이 같으리란 보장이 없다.
        // 밖에 나가 있는 방으로는 보내지 않는다(그 방은 메인에 안 그려진다).
        if let Some(w) = home {
            if w < self.windows.len() && w != self.active_window && !self.window_is_undocked(w) {
                self.switch_window(w);
            }
        }
        let in_tree = self
            .pty_layout
            .as_ref()
            .map(|t| t.leaves().iter().any(|l| *l == pane_id))
            .unwrap_or(false);
        if !in_tree {
            let active = self.ws.lock().unwrap().active_pane.clone();
            let inserted = match (active, self.pty_layout.as_mut()) {
                (Some(active), Some(tree)) => {
                    tree.split_leaf(&active, kasa_pty::SplitDir::Horizontal, pane_id.clone())
                }
                _ => false,
            };
            if !inserted {
                // 트리가 비었거나 active 가 트리에 없음 — 이 pane 을 유일 leaf 로.
                self.pty_layout = Some(kasa_pty::PtyLayout::single(pane_id.as_str()));
            }
        }
        {
            let mut ws = self.ws.lock().unwrap();
            ws.active_pane = Some(pane_id.clone());
            for pane in ws.panes.values_mut() {
                pane.dirty = true;
            }
        }
        let (cols, rows) = self.window_cells();
        self.resize_backend(cols, rows);
        self.publish_pty_layout();
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    /// 방 별도창 이벤트. 키·휠·IME 는 터미널 창 경로를 그대로 쓴다 — `term_pane_id`
    /// 가 포커스 pane 을 내주므로 한 벌로 충분하다. 다른 건 셋뿐이다: 닫기는 방을
    /// 메인으로 되돌리고, 리사이즈는 leaf 마다 PTY 를 다시 재고, 클릭은 포커스를 옮긴다.
    fn aux_room_event(&mut self, idx: usize, event: WindowEvent, event_loop: &ActiveEventLoop) {
        let _ = event_loop;
        match event {
            WindowEvent::CloseRequested => self.dock_window_room(idx),
            WindowEvent::Resized(size) => {
                if let Some(a) = self.aux_windows.get_mut(idx) {
                    a.gpu.resize(size.width, size.height);
                    a.dirty = true;
                    a.window.request_redraw();
                }
                self.aux_room_resize_pty(idx);
            }
            WindowEvent::ScaleFactorChanged { .. } => {
                if let Some(a) = self.aux_windows.get_mut(idx) {
                    let sf = a.window.scale_factor() as f32;
                    a.gpu.set_scale(sf);
                    a.gpu.set_font_size(FONT_SIZE);
                    let sz = a.window.inner_size();
                    a.gpu.resize(sz.width, sz.height);
                    a.dirty = true;
                    a.window.request_redraw();
                }
                self.aux_room_resize_pty(idx);
            }
            WindowEvent::Focused(f) => {
                if let Some(a) = self.aux_windows.get_mut(idx) {
                    a.focused = f;
                    a.window.request_redraw();
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                let scale = self.aux_windows.get(idx).map(|a| a.gpu.scale()).unwrap_or(1.0);
                if let Some(a) = self.aux_windows.get_mut(idx) {
                    let was_header = a.cursor_px.1 <= AUX_HEADER_H;
                    let was_face = a.over_face(a.cursor_px);
                    a.cursor_px = (position.x as f32 / scale, position.y as f32 / scale);
                    // 헤더를 지나는 동안만 다시 그린다 — 버튼 hover 는 그 띠에서만
                    // 바뀌고, 셀 위에서 매 픽셀 재렌더하면 그냥 낭비다. 띠를 벗어나는
                    // 프레임도 한 번은 그려야 hover 가 남아 굳지 않는다.
                    // 프사는 그 띠 밖이라 따로 봐야 한다 — 들고 나는 **경계에서만**
                    // 그리면 팝업이 뜨고 지면서도 셀 위 매 픽셀 재렌더는 피한다.
                    let is_face = a.over_face(a.cursor_px);
                    if was_header || a.cursor_px.1 <= AUX_HEADER_H || was_face != is_face {
                        a.dirty = true;
                        a.window.request_redraw();
                    }
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                // 헤더 버튼이 먼저다 — 그 띠는 셀 그리드 밖이라 pane 포커스로
                // 흘려보내면 버튼이 영영 안 눌린다.
                if !self.aux_header_click(idx) && !self.aux_tree_click(idx) {
                    self.aux_room_click(idx);
                }
            }
            WindowEvent::MouseWheel { delta, .. } => self.aux_terminal_wheel(idx, delta),
            WindowEvent::KeyboardInput { event, .. } => self.aux_terminal_key(idx, &event),
            WindowEvent::Ime(ime) => self.aux_terminal_ime(idx, ime),
            WindowEvent::RedrawRequested => self.aux_render(idx),
            _ => {}
        }
    }

    /// 방 창 크기 변화 → leaf 마다 제 몫으로 PTY resize. 메인 `resize_backend` 가 하는
    /// 일을 이 창의 셀 메트릭 기준으로 한 것.
    fn aux_room_resize_pty(&mut self, idx: usize) {
        for (pid, _, _, w, h) in self.room_leaf_rects(idx) {
            if let Some(pty) = self.pty.get(&pid) {
                let _ = pty.resize(w.max(1), h.max(1));
            }
        }
    }

    /// 방 별도창의 포커스 pane 을 쪼갠다(⌘D / ⌘⇧D / ⌘E — 메인 창과 같은 화음).
    ///
    /// `split_active_pane` 을 못 쓰는 건 그게 **활성 window 트리**(`pty_layout`)만
    /// 보기 때문이다 — 꺼내 둔 방이 비활성이면 그 트리엔 이 pane 이 없어 split_leaf
    /// 이 false 를 돌려주고 새 셸만 조용히 새 나간다. 여기선 어느 window 를 그리고
    /// 있는지 창이 알고 있으니 그 트리에 직접 꽂고, 리사이즈도 메인 그리드가 아니라
    /// 이 창의 leaf_rects 로 한다.
    pub(crate) fn split_room_pane(&mut self, idx: usize, dir: kasa_pty::SplitDir) {
        if self.tmux.is_some() {
            return;
        }
        let Some((window, target)) = self
            .aux_windows
            .get(idx)
            .and_then(|a| Some((a.room_window()?, a.term_pane_id()?.to_string())))
        else {
            return;
        };
        // 트리 선택·롤백은 `split_active_pane` 이 이미 한다 — 사본을 두면 한쪽만
        // 고쳐진다. 그쪽이 `ws.active_pane` 을 기준으로 잡으므로 잠깐 갈아끼운다
        // (소켓 split 과 같은 관례).
        let prev = self.ws.lock().unwrap().active_pane.clone();
        self.ws.lock().unwrap().active_pane = Some(target);
        let new_id = match self.split_active_pane(dir) {
            Ok(id) => id,
            Err(e) => {
                eprintln!("[kasaterm] room split failed: {e:#}");
                self.ws.lock().unwrap().active_pane = prev;
                return;
            }
        };
        // 비활성 방을 쪼갠 거면 메인 창의 포커스는 원래 자리로 — 안 보이는 방의
        // pane 이 활성이 되면 키 입력이 화면 밖으로 샌다.
        if window != self.active_window {
            self.ws.lock().unwrap().active_pane = prev;
        }
        // 새 pane 으로 포커스를 옮긴다 — 메인 창 split 과 같은 관례(방금 만든 곳에
        // 바로 친다).
        if let Some(a) = self.aux_windows.get_mut(idx) {
            if let AuxWindowKind::Room { focus, .. } = &mut a.kind {
                *focus = Some(new_id);
            }
        }
        self.aux_room_resize_pty(idx);
        self.aux_redraw(idx);
    }

    /// 방 창 클릭 → 커서 아래 pane 으로 포커스 이동(키 입력이 그리로 간다).
    fn aux_room_click(&mut self, idx: usize) {
        let (cx, cy, cw, ch, left) = {
            let Some(a) = self.aux_windows.get(idx) else { return };
            (a.cursor_px.0, a.cursor_px.1, a.gpu.cell_w, a.gpu.cell_h, a.cell_left())
        };
        if cw <= 0.0 || ch <= 0.0 {
            return;
        }
        let hit = self.room_leaf_rects(idx).into_iter().find(|(_, x, y, w, h)| {
            let rx = left + *x as f32 * cw;
            let ry = AUX_CELL_TOP + *y as f32 * ch;
            cx >= rx && cx < rx + *w as f32 * cw && cy >= ry && cy < ry + *h as f32 * ch
        });
        let Some((pid, ..)) = hit else { return };
        if let Some(a) = self.aux_windows.get_mut(idx) {
            if let AuxWindowKind::Room { focus, .. } = &mut a.kind {
                if focus.as_deref() == Some(pid.as_str()) {
                    return;
                }
                *focus = Some(pid);
            }
        }
        self.aux_redraw(idx);
    }

    /// 방(윈도우) 하나를 통째로 별도 창으로 꺼낸다 — 탭을 창 밖에 놓았을 때.
    ///
    /// 꺼낼 방은 드래그 press 가 이미 활성으로 만들어 뒀고, 활성 방의 트리는 슬롯이
    /// 아니라 `pty_layout` 에 얹혀 있다. 그래서 먼저 제자리에 park 하고 메인이 볼
    /// 다른 방으로 활성을 옮긴 뒤 창을 띄운다. **방이 하나뿐이면 거부** — 꺼내고 나면
    /// 메인 창이 빈 채로 남는다.
    pub(crate) fn undock_window_room(
        &mut self,
        window: usize,
        event_loop: &ActiveEventLoop,
        near: Option<winit::dpi::PhysicalPosition<i32>>,
    ) {
        if let Some(i) = self.aux_windows.iter().position(|a| a.room_window() == Some(window)) {
            self.aux_windows[i].window.focus_window();
            return;
        }
        if self.tmux.is_some() || window >= self.windows.len() || self.windows.len() < 2 {
            return;
        }
        self.windows[self.active_window] = self.pty_layout.take();
        if self.active_window == window {
            self.active_window = if window + 1 < self.windows.len() {
                window + 1
            } else {
                window - 1
            };
        }
        self.pty_layout = self.windows[self.active_window].take();
        self.window_alert.remove(&self.active_window);
        let focus = self
            .windows
            .get(window)
            .and_then(|s| s.as_ref())
            .and_then(|l| l.leaves().first().map(|s| s.to_string()));
        // 메인의 활성 pane 이 꺼낸 방 소속이면 남은 방의 것으로 옮긴다 — 안 그러면
        // 화면에 없는 pane 이 선택된 채로 키 입력이 별도 창 pane 에 꽂힌다.
        let leaves: Vec<String> = self
            .pty_layout
            .as_ref()
            .map(|l| l.leaves().iter().map(|s| s.to_string()).collect())
            .unwrap_or_default();
        {
            let mut ws = self.ws.lock().unwrap();
            let stale = ws.active_pane.as_ref().map(|p| !leaves.contains(p)).unwrap_or(true);
            if stale {
                ws.active_pane = leaves.first().cloned();
            }
            for pane in ws.panes.values_mut() {
                pane.dirty = true;
            }
        }
        let (cols, rows) = self.window_cells();
        self.resize_backend(cols, rows);
        self.publish_pty_layout();
        self.window_labels_at = None;
        self.session_touched = true;
        self.chrome_dirty = true;
        if let Some(w) = &self.window {
            w.request_redraw();
        }
        self.spawn_aux_room(window, focus, event_loop, near);
    }

    /// 방 별도창을 닫으며 그 방을 메인으로 되돌린다. 트리는 처음부터 `windows` 에
    /// 그대로 있었으므로 창을 없애고 그 방으로 전환하면 끝이다 — 재삽입이 없다.
    /// (`switch_window` 는 밖에 나간 방이면 창을 앞으로 보내므로, 반드시 창을 먼저
    /// 없애고 전환한다.)
    pub(crate) fn dock_window_room(&mut self, idx: usize) {
        let window = self.aux_windows.get(idx).and_then(|a| a.room_window());
        self.close_aux_window(idx);
        let Some(w) = window else { return };
        if w < self.windows.len() {
            self.switch_window(w);
        }
        self.session_touched = true;
        self.chrome_dirty = true;
        if let Some(win) = &self.window {
            win.request_redraw();
        }
    }

    /// 방 별도창 스폰. `spawn_aux_terminal` 과 같은 얼개지만 pane 이 여럿이라 기본
    /// 크기가 더 크다.
    fn spawn_aux_room(
        &mut self,
        window: usize,
        focus: Option<String>,
        event_loop: &ActiveEventLoop,
        near: Option<winit::dpi::PhysicalPosition<i32>>,
    ) -> Option<usize> {
        let title = self
            .window_labels
            .get(window)
            .map(|(n, _)| n.clone())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| format!("방 {}", window + 1));
        let mut attrs = WindowAttributes::default()
            .with_title(title.clone())
            .with_theme(Some(Theme::Dark))
            .with_inner_size(LogicalSize::new(1000.0, 660.0))
            // 배경 실행(헤드리스 검증)이면 뜨면서 키 포커스를 안 가져간다 — 메인 창은
            // 이미 그렇게 하는데 별도창만 빠져 있어, 검증 한 번에 작업하던 창을
            // 통째로 빼앗겼다(거노).
            .with_active(!crate::background_launch());
        if let Some(pos) = near {
            attrs = attrs.with_position(pos);
        }
        let window_handle = match create_aux_window(event_loop, attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                eprintln!("[auxwin] room window create failed: {e}");
                return None;
            }
        };
        #[cfg(target_os = "macos")]
        window_handle.set_ime_allowed(false);
        #[cfg(not(target_os = "macos"))]
        window_handle.set_ime_allowed(true);
        let gpu = match gpu::GpuRenderer::new(window_handle.clone(), FONT_SIZE) {
            Ok(g) => g,
            Err(e) => {
                eprintln!("[auxwin] room gpu init failed: {e}");
                return None;
            }
        };
        self.aux_windows.push(AuxWindow {
            gpu,
            kind: AuxWindowKind::Room { window, focus },
            dirty: true,
            cursor_px: (0.0, 0.0),
            selecting: false,
            focused: true,
            preedit: String::new(),
            last_title: title,
            pending_capture: None,
            md_content_h: 0.0,
            tree_open: false,
            pinned: false,
            tree_scroll: 0.0,
            tree_rows: Vec::new(),
            face_rects: Vec::new(),
            header_btns: Vec::new(),
            window: window_handle,
        });
        let idx = self.aux_windows.len() - 1;
        eprintln!("[auxwin] opened room window #{idx} for window {window}");
        // 창 크기에 맞춰 leaf 마다 PTY 를 즉시 재운다(셸이 SIGWINCH 로 리플로우).
        self.aux_room_resize_pty(idx);
        self.aux_redraw(idx);
        Some(idx)
    }
}

/// Ctrl+글자 → 제어바이트(^A=0x01 … ^Z=0x1a). 터미널 별도창 전용 축약(메인
/// forward_key 의 동일 매핑을 winit KeyCode 기준으로 옮긴 것).
fn ctrl_byte(code: winit::keyboard::KeyCode) -> Option<u8> {
    use winit::keyboard::KeyCode;
    let b = match code {
        KeyCode::KeyA => 0x01,
        KeyCode::KeyB => 0x02,
        KeyCode::KeyC => 0x03,
        KeyCode::KeyD => 0x04,
        KeyCode::KeyE => 0x05,
        KeyCode::KeyF => 0x06,
        KeyCode::KeyG => 0x07,
        KeyCode::KeyH => 0x08,
        KeyCode::KeyI => 0x09,
        KeyCode::KeyJ => 0x0a,
        KeyCode::KeyK => 0x0b,
        KeyCode::KeyL => 0x0c,
        KeyCode::KeyM => 0x0d,
        KeyCode::KeyN => 0x0e,
        KeyCode::KeyO => 0x0f,
        KeyCode::KeyP => 0x10,
        KeyCode::KeyQ => 0x11,
        KeyCode::KeyR => 0x12,
        KeyCode::KeyS => 0x13,
        KeyCode::KeyT => 0x14,
        KeyCode::KeyU => 0x15,
        KeyCode::KeyV => 0x16,
        KeyCode::KeyW => 0x17,
        KeyCode::KeyX => 0x18,
        KeyCode::KeyY => 0x19,
        KeyCode::KeyZ => 0x1a,
        _ => return None,
    };
    Some(b)
}
