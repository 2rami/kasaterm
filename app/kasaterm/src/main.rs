// Release builds hide the console window so kasaterm launches as a pure GUI
// app (Start menu / .msi install). Debug builds keep the console so stderr
// startup/IME logs stay visible for the self-test cycle.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
//! kasaterm — sugarloaf-rendered terminal driven by
//! tmux-bridge. Multi-pane: tmux's split-window creates additional
//! panes, layout-change events tell us how to lay them out, and we
//! render each pane inside its rect from the parsed Layout tree.
//! Phase A Task #13/14: wheel + scrollback, IME, selection + clipboard,
//! cursor blink, OSC titles, multi-pane render + focus routing.

mod autosuggest;
mod auxterm;
mod auxwin;
mod agent_state;
mod agent_transitions;
mod bridge;
mod cells;
mod chat_view;
mod chrome;
mod claude_auth;
mod clipboard;
mod codexlimits;
use kasa_gridview::cursor;
use kasa_gridview::overlay::{PaneOverlay as GpuOverlay, Selection};
mod eyedropper;
mod gpu;
mod handler;
mod homeaccounts;
mod input;
// claude 한도 초기화권 — 잔량 읽기와 쓰는 페이지 열기.
mod limit_reset;
mod layout;
mod lineedit;
mod markdown;
mod board_digest;
mod transfer_endpoints;
mod tell_delivery;
mod trust_prompt;
mod native_onboarding;
mod native_settings;
mod device_icons;
mod feedback_delivery;
mod account_sync_ui;
mod native_controls;
mod native_strings;
mod notify_banner;
mod onboarding;
mod quota_alerts;
mod render;
mod rich_document;
mod vault;
mod vault_graph;
mod screenread;
mod session;
mod bootstrap;
mod pane_shims;
mod spawn_env;
mod server_restore;
mod session_transfer;
mod drag_transfer;
mod settings;
mod agent_preferences;
mod settings_media;
mod settings_room;
mod socket;
mod sprites;
mod stream;
mod testkit;
mod theme;
mod toast;
mod update_notice;
mod remote_approval;
mod themegen;
// 대화 기록 읽기는 kasa-agents 로 옮겼다. `crate::transcript::…` 그대로 쓴다.
use kasa_agents::transcript;
mod webpane;
mod weather;
// settings.rs 가 `use super::*` 로 받는 자유함수들 — 모듈 경로를 UI 쪽에 흘리지
// 않으려고 여기서 한 번 재수출한다.
/// 폭에 맞춘 정보 밀도 — chrome 이 정의하지만 그 판정을 읽어 그리는 쪽(render·info)이
/// 다른 모듈이라 루트에서 재수출한다(`use super::*` 하나로 닿게).
pub(crate) use chrome::Density;
mod gitdiff;
mod git_panel;
mod info;
mod info_focus;
mod context_info;
mod internal_room;
mod links;
mod lsp;
mod machinescol;
mod sidebar_navigation;
mod sidebar_pulse;
mod mirror_theme;
mod mirror_view;
mod mirror_render;
mod shell_view;
mod mirror_follow;
mod mirror_focus_probe;
mod character_assignment;
mod agent_identity;
mod app_restart;
#[cfg(unix)]
mod app_update;
mod restore_progress;
mod mirror_close;
mod close_grace;
mod mirror_sync;
mod own_room;
mod mirror_layout;
mod mirror_diff;
mod mcpcol;
mod proc;
mod sesscol;
mod state;
mod statusbar;
mod statusbar_config;
mod syntax;
mod turnjump;
mod prompt_nav;
mod turn_probe;
mod frame_pace;
// 배포 피드의 최신판 확인 — 상태줄 버전 조각과 계정 메뉴 바닥 줄이 읽는다.
mod version;
// 물리 메모리 압박 판정 — 하단바 사용량 위젯의 「재시작 권장」 근거.
mod sysmem;
// macOS `.md` 더블클릭(odoc Apple Event) 핸들러. 다른 OS 엔 파일오픈 이벤트가
// 이 경로로 안 와서 macos 전용.
#[cfg(target_os = "macos")]
mod macos_open;
// 알림 배너 클릭 → 그 pane 으로. UNUserNotificationCenter 는 delegate 로만 클릭을
// 알려주고, 그 delegate 는 macOS 에만 있다.
#[cfg(target_os = "macos")]
mod macos_notify;
#[cfg(target_os = "macos")]
mod macos_sparkle;
// Windows 자동 업데이트 — WinSparkle.dll 런타임 로드(macos_sparkle 대칭).
// 모듈 자체는 전 플랫폼 컴파일: 토스트 센티널·버전 파서(+테스트)·no-op 스텁은
// 공용이고 FFI/체커 스레드만 cfg(windows) 게이트 — mac 에서도 헬퍼 테스트가 돈다.
mod win_sparkle;

use anyhow::Result;
use kasa_bridge::layout::{parse_layout, Layout};
use kasa_bridge::screen::Cell as GridCell;
use kasa_bridge::screen::Row;
use kasa_bridge::{ScreenUpdate, StartOptions, TmuxEvent, TmuxSession};
use std::collections::{HashMap, VecDeque};
use std::error::Error;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, Ime, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{Key, ModifiersState, NamedKey};
#[cfg(target_os = "macos")]
use winit::platform::macos::WindowAttributesExtMacOS;
use winit::window::{CursorIcon, Theme, Window, WindowAttributes, WindowId};
use bootstrap::{
    apply_lite_env, arm_self_install, background_launch, install_panic_logger, install_pending,
    install_pending_paths, install_pty_host_policy, install_stderr_log, lite_mode,
    live_kasaterm_pids, load_capture_config, prune_empty_inboxes, prune_finished_tasks,
    scrub_inherited_claude_markers, verification_run, LITE_MODE,
};
// bridge.rs 의 Windows attach 가 `crate::CLAUDE_MARKER_ENV` 로 부른다.
#[cfg(windows)]
use bootstrap::CLAUDE_MARKER_ENV;
use pane_shims::{
    agent_name_suffix, codex_binary, install_claude_hook_shim, install_codex_shim,
    install_pane_shims, install_student_shims, proxy_env, python3_program,
    write_codex_account_file,
};
use session::remap_window_index;
use spawn_env::{
    available_shells, hook_shell_program, mcp_panel_port, mcp_port_file_for, resolve_default_shell,
    resolve_initial_cwd, resolve_kasaterm_socket_path, resolve_spawn_cwd,
};

// Match ghostty's default font-size=13. We were at 14, which made every
// cell ~7.7% taller than ghostty's — visible side-by-side as a "slightly
// larger" terminal even though the chrome looked identical.
const FONT_SIZE: f32 = 16.0;

/// Display columns a char occupies in a proportional-ish label: CJK /
/// Hangul / fullwidth glyphs are double-width, everything else single.
/// Used to budget sidebar tab text so a Hangul title doesn't overflow the
/// strip into the cell grid.
fn cjk_display_w(c: char) -> usize {
    let u = c as u32;
    let wide = matches!(u,
        0x1100..=0x115F   // Hangul Jamo
        | 0x2E80..=0x303E // CJK radicals, Kangxi, CJK symbols
        | 0x3041..=0x33FF // Hiragana, Katakana, CJK compat
        | 0x3400..=0x4DBF // CJK ext A
        | 0x4E00..=0x9FFF // CJK unified
        | 0xA000..=0xA4CF // Yi
        | 0xAC00..=0xD7A3 // Hangul syllables
        | 0xF900..=0xFAFF // CJK compat ideographs
        | 0xFE30..=0xFE4F // CJK compat forms
        | 0xFF00..=0xFF60 // Fullwidth forms
        | 0xFFE0..=0xFFE6
        | 0x20000..=0x3FFFD // CJK ext B+ / supplementary ideographs
    );
    if wide {
        2
    } else {
        1
    }
}

/// Draw a rounded-corner rect with the sharp-quad renderer: a full-width
/// middle block plus per-row caps whose horizontal inset follows a circle of
/// radius `r`, giving genuine rounded corners. `r` is small (≤~10px) so this
/// is only a handful of extra quads; rendering is throttled anyway.
/// Quote a filesystem path for safe pasting into a shell prompt. Bare when it
/// holds only shell-safe characters, single-quoted (with embedded quotes
/// escaped) otherwise — covers spaces, parens, and non-ASCII (e.g. 한글) paths.
fn shell_quote_path(p: &str) -> String {
    let safe = !p.is_empty()
        && p.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-/~+=,@:".contains(&b));
    if safe {
        p.to_string()
    } else {
        format!("'{}'", p.replace('\'', "'\\''"))
    }
}

/// 커서가 놓인 칸의 글자가 몇 칸짜리인지. 한글·CJK·이모지는 두 칸이다.
///
/// 전각 글자는 그리드에서 두 칸을 먹고 뒤 칸엔 스페이서(`'\0'`)가 들어간다. 커서를
/// 항상 한 칸으로 칠하면 글자의 왼쪽 절반만 덮여, 커서가 글자를 가리키는 게 아니라
/// 반쯤 깨진 것처럼 보인다. 커서가 스페이서 쪽에 놓이는 경우는 앞 칸을 보지 않는다 —
/// vt100 은 전각 글자의 첫 칸에 커서를 세우고, 안 겪은 경우를 위한 코드는 검증할 수
/// 없는 채로 남는다.
fn cursor_cell_width(cells: &[kasa_bridge::screen::Row], row: u16, col: u16) -> u16 {
    use unicode_width::UnicodeWidthChar;
    cells
        .get(row as usize)
        .and_then(|r| r.get(col as usize))
        .and_then(|c| c.ch.width())
        .unwrap_or(1)
        .clamp(1, 2) as u16
}

fn round_rect(g: &mut gpu::GpuRenderer, x: f32, y: f32, w: f32, h: f32, r: f32, col: [u8; 4]) {
    // Single anti-aliased implementation lives on the renderer so the chrome
    // and the markdown code-block chips round identically.
    g.round_rect_fill(x, y, w, h, r, col);
}

/// A mark that means *circle* — status dots, avatar chips, toggle knobs.
/// Callers state the intent and the active silhouette decides how round it
/// actually is (`theme::roundness`), because a square dot is what reads as
/// pixel-art while a dot drawn with a small corner radius just reads as a
/// mistake. Passing `size / 2.0` to `round_rect` by hand froze that decision at
/// every call site.
fn circle_rect(g: &mut gpu::GpuRenderer, x: f32, y: f32, size: f32, col: [u8; 4]) {
    round_rect(g, x, y, size, size, size / 2.0 * theme::roundness(), col);
}

/// A mark that means *capsule* — pill toggles, scrollbar thumbs, count chips.
/// Same contract as `circle_rect`: the fully-round radius is derived from the
/// short side, then bent by the silhouette.
fn pill_rect(g: &mut gpu::GpuRenderer, x: f32, y: f32, w: f32, h: f32, col: [u8; 4]) {
    round_rect(g, x, y, w, h, w.min(h) / 2.0 * theme::roundness(), col);
}

/// A raised surface — card, panel, selected tab. Lays down the silhouette's
/// hard drop shadow and its border rule before the fill, which is what makes a
/// pixel UI read as a physical object rather than a flat square. At the default
/// silhouette (offset 0, 1px border) both extras collapse and this is exactly
/// one `round_rect`, so existing surfaces render unchanged.
fn panel_rect(g: &mut gpu::GpuRenderer, x: f32, y: f32, w: f32, h: f32, r: f32, fill: [u8; 4]) {
    let off = theme::shadow_offset();
    if off > 0.0 {
        // Blur-less and fully opaque-ish: a soft shadow would just look like a
        // rounded theme with extra steps.
        round_rect(g, x + off, y + off, w, h, r, [0, 0, 0, 0x66]);
    }
    let b = theme::border_w();
    if b > 1.0 {
        // The pixel outline is black rather than theme::border() — a border tinted
        // to match the surface disappears into it, which is what killed the first cut.
        round_rect(g, x, y, w, h, r, [0, 0, 0, 0xE0]);
        round_rect(
            g,
            x + b,
            y + b,
            w - b * 2.0,
            h - b * 2.0,
            (r - b).max(0.0),
            fill,
        );
    } else {
        round_rect(g, x, y, w, h, r, fill);
    }
}

/// 판 중에서도 **테두리로 층을 선언해야 하는 것** — 행 위에 뜨는 메뉴·드롭다운,
/// 값을 받는 입력 상자.
///
/// `panel_rect` 과 갈리는 건 사이드바 탭처럼 바탕에 붙어 있는 판이다 — 그건
/// 테두리가 없어야 목록이 조용하고, 행을 가리며 뜨는 메뉴는 테두리가 있어야
/// 어디까지가 메뉴인지 읽힌다.
///
/// 밝은 링은 실루엣과 무관하게 **항상** 두른다. 픽셀 실루엣의 검은 테두리는
/// 어두운 바탕 위에서 그냥 사라져서, 스크림 위에 뜬 모달이나 검은 터미널 위의
/// 메뉴가 테두리 없는 색판으로 보였다 — 층을 선언하라고 만든 함수가 정작
/// 층이 필요한 자리에서만 침묵한 셈이다.
///
/// 그 링 색이 `theme::border()` 인 동안은 위 문단이 의도로만 남아 있었다. 그
/// 토큰은 한 자리에 고정된 색이라 어두운 판 위에서 채움과 같은 대역으로
/// 내려앉는다 — 복원 카드도 사이드바의 펼치기 배지도 테두리 없는 색판으로
/// 보였다. `theme::edge_on(fill)` 은 링을 **채움 기준으로** 잡으므로 어느 판
/// 위에서든 한 줄이 남는다(2026-08-27 지적).
fn panel_rect_outlined(
    g: &mut gpu::GpuRenderer,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    r: f32,
    fill: [u8; 4],
) {
    round_rect(
        g,
        x - 1.0,
        y - 1.0,
        w + 2.0,
        h + 2.0,
        r,
        theme::edge_on(fill),
    );
    panel_rect(g, x, y, w, h, r, fill);
}

/// 속이 빈 둥근 테두리 — **고른 것**을 채우지 않고 두르는 자리.
///
/// `panel_rect` 과 갈리는 건 무엇을 말하느냐다. 판은 "여기가 층이다"를 말하고
/// 테두리는 "이걸 골랐다"만 말한다. 골랐다는 걸 판으로 그리면 그 색이 카드
/// 전체를 덮어, 그 위에 얹히는 상태색이 같은 밝기 대역에서 겨루게 된다.
///
/// 안쪽을 `bg` 로 되메우는 방식이라 **바탕색을 호출자가 알아야 한다** — 링만
/// 그리는 스트로크가 렌더러에 없어서고, 사각 넷으로 두르면 모서리 라운드가
/// 죽는다.
#[allow(dead_code)]
fn outline_rect(
    g: &mut gpu::GpuRenderer,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    r: f32,
    col: [u8; 4],
    t: f32,
    bg: [u8; 4],
) {
    round_rect(g, x, y, w, h, r, col);
    round_rect(
        g,
        x + t,
        y + t,
        (w - t * 2.0).max(0.0),
        (h - t * 2.0).max(0.0),
        (r - t).max(0.0),
        bg,
    );
}

/// 커서가 얹힌 것을 **들어 올리는** 유일한 함수 — 버튼·탭·행·메뉴 항목, 눌리는
/// 것이면 전부 여기를 지난다.
///
/// 색을 인자로 받지 않는 게 요점이다. 예전엔 자리마다 `surface_hover` 와
/// `text` 22%·18% 반투명이 섞여 같은 동작에 세 가지 밝기가 났다. 그리고 들림을
/// 그리는 김에 커서 플래그까지 세우니, 손 모양이 안 뜨는 버튼이 생길 수 없다.
fn hover_rect(g: &mut gpu::GpuRenderer, x: f32, y: f32, w: f32, h: f32, r: f32) {
    g.hover_pointer = true;
    round_rect(g, x, y, w, h, r, theme::surface_hover());
}

/// 하위 레포 고르기 메뉴에 싣는 줄 수 — 그 아래는 덜 최근이라 고를 일이 드물다.
const GIT_NESTED_MENU_ROWS: usize = 8;

/// Paint the git-column header dropdowns (repo path picker + branch switcher)
/// and fill their click rects. A free fn, not a method, so it can run inside
/// the `&mut self.gpu` block (which can't re-borrow `&self`): the caller hands
/// it `g` plus the disjoint rect Vecs. Drawn last so it overlays the list.
#[allow(clippy::too_many_arguments)]
fn git_paint_dropdowns(
    g: &mut gpu::GpuRenderer,
    col_x: f32,
    col_w: f32,
    _title_h: f32,
    path_hdr: Option<(f32, f32, f32, f32)>,
    path_open: bool,
    repos: &[std::path::PathBuf],
    pinned: &Option<std::path::PathBuf>,
    path_rects: &mut Vec<(Option<std::path::PathBuf>, (f32, f32, f32, f32))>,
    nested: &[std::path::PathBuf],
    chosen: Option<&std::path::Path>,
    nested_rects: &mut Vec<(std::path::PathBuf, (f32, f32, f32, f32))>,
) {
    let item_h = 28.0_f32;
    let pad = 6.0_f32;
    let px = col_x + 6.0;
    let pw = (col_w - 12.0).max(0.0);
    // A raised menu panel with a 1px border so it reads above the list.
    let panel = |g: &mut gpu::GpuRenderer, y: f32, h: f32| {
        round_rect(
            g,
            px - 1.0,
            y - 1.0,
            pw + 2.0,
            h + 2.0,
            theme::radius_md(),
            theme::border(),
        );
        round_rect(g, px, y, pw, h, theme::radius_md(), theme::surface_active());
    };
    let row = |g: &mut gpu::GpuRenderer, iy: f32, label: &str, on: bool| {
        if on {
            round_rect(
                g,
                px + 4.0,
                iy + 1.0,
                pw - 8.0,
                item_h - 2.0,
                theme::radius_sm(),
                theme::with_alpha(theme::accent(), 0x40),
            );
        }
        let col = if on { theme::text() } else { theme::text_dim() };
        g.draw_text(
            px + 12.0,
            iy + (item_h - 12.0) / 2.0,
            label,
            gpu::DrawOpts {
                font_size: 12.0,
                color: col,
                bold: false,
                italic: false,
            },
        );
    };
    if path_open {
        if let Some((_hx, hy, _hw, hh)) = path_hdr {
            // 부모 폴더 칸의 하위 레포 — 최근에 만진 순, 맨 위가 자동으로 보이는 것.
            let nested = &nested[..nested.len().min(GIT_NESTED_MENU_ROWS)];
            let n = repos.len() + nested.len() + 1; // +1 for the "자동 추적" toggle
            let menu_h = n as f32 * item_h + pad * 2.0;
            let my = hy + hh + 2.0;
            panel(g, my, menu_h);
            let mut iy = my + pad;
            // "자동 추적" — selected when nothing is pinned or chosen.
            row(g, iy, "자동 추적 (활성 pane)", pinned.is_none() && chosen.is_none());
            path_rects.push((None, (px, iy, pw, item_h)));
            iy += item_h;
            for (i, r) in nested.iter().enumerate() {
                let name = r.file_name().and_then(|s| s.to_str()).unwrap_or("?");
                let label = if i == 0 { format!("› {name}  ·  최근") } else { format!("› {name}") };
                row(g, iy, &label, pinned.is_none() && chosen == Some(r.as_path()));
                nested_rects.push((r.clone(), (px, iy, pw, item_h)));
                iy += item_h;
            }
            for r in repos {
                let name = r.file_name().and_then(|s| s.to_str()).unwrap_or("?");
                let sel = pinned.as_ref() == Some(r);
                row(g, iy, name, sel);
                path_rects.push((Some(r.clone()), (px, iy, pw, item_h)));
                iy += item_h;
            }
        }
    }
}

/// Lucide icon name for a sidebar tab's chip, chosen from the window label.
/// 에이전트 pane 은 sparkle, markdown 은 문서, 나머지는 터미널 글리프.
/// codex 도 학생 대접이라 같은 sparkle 을 쓴다(사용자 2026-08-05) — 종류를 아이콘으로
/// 가르면 "누가 에이전트인가"가 한눈에 안 들어온다.
/// 방 카드 앞의 글리프. **무엇이 도는지**가 아니라 **무엇을 여는 자리인지**만
/// 말한다.
///
/// 예전엔 이름에 claude·codex·✳ 가 걸리면 sparkles(별)를 줬는데, 여기 방은 거의
/// 다 claude 라 결국 모든 카드에 같은 별이 박혔다 — 가르는 데 아무 일도 안 하면서
/// 목록에서 가장 눈에 띄는 자리를 차지한 셈이다(2026-08-11 지시: "별표시 저건
/// 없애자"). 무엇이 도는지는 이제 방을 펴면 학생이 걷는 것으로 보인다.
fn tab_icon_glyph(name: &str) -> &'static str {
    if name.to_ascii_lowercase().ends_with(".md") {
        "file-text"
    } else {
        "terminal"
    }
}

/// Pick the shell to spawn inside a PTY. claude code's teammate mode
/// emits Unix-quoted commands (`cd 'path' && env VAR=val cmd`), so a
/// cmd.exe default leaves teammate spawns dead on arrival. Honor
/// KASATERM_SHELL / SHELL when set, otherwise auto-discover Git for
/// Windows' bash so users with a stock setup get a working unix-style
/// shell without configuration. Returns None to let portable-pty's
/// `new_default_prog` pick (cmd.exe on Windows, $SHELL on Unix).
/// Prefix well-known interactive programs with a small sigil so the
/// pane header reads at a glance. Mirrors how the programs themselves
/// brand their own OSC titles in other terminals (Claude Code ships
/// "✱ Claude Code", vim/less label themselves with their name). For
/// anything we don't have an opinion on, just return the comm as-is.
fn decorate_process_name(comm: &str) -> String {
    match comm {
        "claude" => "✱ claude".to_string(),
        "node" | "deno" | "bun" => format!("⬢ {comm}"),
        "vim" | "nvim" => format!("⌨ {comm}"),
        "less" | "more" => format!("☰ {comm}"),
        "git" => format!("⎇ {comm}"),
        _ => comm.to_string(),
    }
}

/// claude Code 가 OSC 제목에 붙이는 선행 활동 글리프(✳/✶/✻/✽ … dingbat 별표류
/// + ∗ ＊ * + 브라유 스피너 ⠂⠐… U+2800 블록)와 공백 run 을 벗긴다. 타이틀바·
/// 헤더·board 라벨이 "아로나 · ⠂ 요약" 대신 "아로나 · 요약" 을 보이게(사용자).
/// 활동 글리프로 시작 안 하면 원문 그대로(rename 사용자 값 보호).
pub(crate) fn strip_activity_prefix(s: &str) -> &str {
    s.trim_start_matches(|c: char| {
        c.is_whitespace()
            || matches!(c,
                '*' | '\u{2217}' | '\u{FF0A}'   // ASCII * / ∗ / ＊
                | '\u{2721}'..='\u{2749}'        // ✢..❉ dingbat asterisks·stars
                | '\u{2800}'..='\u{28FF}'        // braille 스피너 프레임
                | '\u{25CF}'                     // ● reduce motion 고정 스피너 (2026-09-02)
            )
    })
}

/// 내장 이미지 뷰로 열 수 있는 확장자인가. 이미지는 "파일 열기" 설정을 타지
/// 않는다 — CLI 편집기에 넘기면 바이너리 쓰레기만 뜬다.
pub(crate) fn is_image_path(path: &std::path::Path) -> bool {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    matches!(
        ext.as_str(),
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "tiff" | "tif" | "ico"
    )
}

/// File-type icon (assets/icons/ft, 브랜드컬러 filled)를 파일명에서 고른다.
/// 특수 파일명(README, Dockerfile, tsconfig…)이 확장자보다 우선. `None` 이면
/// 매핑이 없다는 뜻 — 호출부는 기존 모노크롬 "file" 글리프로 폴백한다.
fn file_icon(name: &str) -> Option<&'static str> {
    let l = name.to_ascii_lowercase();
    // Well-known file names first — these outrank their extension.
    let by_name = match l.as_str() {
        "readme" | "readme.md" | "readme.txt" => Some("ft/readme"),
        "license" | "license.md" | "license.txt" | "licence" => Some("ft/license"),
        "todo" | "todo.md" => Some("ft/todo"),
        "dockerfile" | ".dockerignore" | "docker-compose.yml" | "docker-compose.yaml" => {
            Some("ft/docker")
        }
        "tsconfig.json" => Some("ft/tsconfig"),
        "package.json" | "package-lock.json" => Some("ft/nodejs"),
        ".gitignore" | ".gitattributes" | ".gitmodules" | ".gitconfig" => Some("ft/git"),
        _ => None,
    };
    if by_name.is_some() {
        return by_name;
    }
    if l.starts_with(".env") {
        return Some("ft/settings");
    }
    let ext = l.rsplit_once('.').map(|(_, e)| e)?;
    Some(match ext {
        "rs" => "ft/rust",
        "ts" | "mts" | "cts" => "ft/typescript",
        "tsx" | "jsx" => "ft/react",
        "js" | "mjs" | "cjs" => "ft/javascript",
        "json" | "jsonc" => "ft/json",
        "md" | "markdown" | "mdx" => "ft/markdown",
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "ico" | "bmp" | "avif" | "heic" => "ft/image",
        "svg" => "ft/svg",
        "toml" | "ini" | "conf" | "cfg" | "plist" | "properties" => "ft/settings",
        "yaml" | "yml" => "ft/yaml",
        "sh" | "bash" | "zsh" | "fish" | "bat" | "cmd" => "ft/console",
        "ps1" | "psm1" => "ft/powershell",
        "py" | "pyi" => "ft/python",
        "c" | "h" => "ft/c",
        "cpp" | "cc" | "cxx" | "hpp" | "hh" => "ft/cpp",
        "cs" => "ft/csharp",
        "css" => "ft/css",
        "scss" | "sass" | "less" => "ft/sass",
        "html" | "htm" | "xhtml" => "ft/html",
        "go" => "ft/go",
        "java" => "ft/java",
        "kt" | "kts" => "ft/kotlin",
        "lua" => "ft/lua",
        "php" => "ft/php",
        "rb" | "erb" => "ft/ruby",
        "swift" => "ft/swift",
        "vue" => "ft/vue",
        "graphql" | "gql" => "ft/graphql",
        "prisma" => "ft/prisma",
        "sql" | "db" | "sqlite" | "sqlite3" => "ft/database",
        "pdf" => "ft/pdf",
        "zip" | "tar" | "gz" | "bz2" | "xz" | "7z" | "rar" | "tgz" => "ft/zip",
        "mp3" | "wav" | "flac" | "ogg" | "m4a" | "aac" => "ft/audio",
        "mp4" | "mov" | "mkv" | "avi" | "webm" => "ft/video",
        "ttf" | "otf" | "woff" | "woff2" => "ft/font",
        "lock" => "ft/lock",
        "txt" | "rtf" | "log" => "ft/document",
        _ => return None,
    })
}

/// Draw a chrome icon glyph centered inside a square chip whose top-left is
/// (`chip_x`, `chip_y`). One place owns icon sizing / centering / hover so every
/// icon button (title bar, sidebar, pane header, image controls) reads
/// identically. The clickable area is `theme::ICON_CHIP` square — hit-test
/// against the same rect.
/// Line spacing multiplier passed to `compute_cell_metrics`. 1.0 keeps
/// rows at font ascent+descent so the cell aspect ratio stays close to
/// 1:2 — that's what makes half-block sprite art (Claude Code's mascot,
/// `▀▄▌▐` characters) read as squares instead of tall rectangles. The
/// earlier 1.3 stretched cells to 1:3 and made the sprite look
/// elongated next to Ghostty / iTerm2.
/// Logical-pixel padding between the window edge and the cell grid on
/// every side. Mirrors what Terminal.app / Ghostty give the content so
/// text doesn't jam against the chrome. Must match `render_frame`'s
/// origin and `px_to_pane_cell`'s offset.
const WINDOW_PADDING: f32 = 0.0;
/// Logical-pixel height of the custom chrome strip that sits above the
/// cell grid (traffic light row + future tab bar). macOS's traffic
/// light buttons end around y ≈ 28 in logical units, so 38 leaves a
/// few pixels of breathing room. Cells start at y = TITLE_HEIGHT (not
/// at WINDOW_PADDING) so the title strip is fully clear of glyph
/// drawing.
const TITLE_HEIGHT: f32 = 36.0;
/// Width of the macOS traffic-light cluster (close/min/zoom) measured
/// from the window's left edge. Mouse events inside this rectangle are
/// reserved for the native buttons; our drag handler ignores them so a
/// click on the red dot still closes the window.
///
/// Windows 는 프레임리스라 비켜 줄 신호등이 없으니 0 이다.
///
/// 예전엔 참조처가 `sidebar_toggle_rect` 와 타이틀바 드래그 판정 둘뿐이고 둘 다
/// `cfg(not(windows))` 라 상수도 같이 접었다. 그 뒤 라이트 제목을 가운데 놓는
/// 자리(`render.rs`)가 생겼는데 거기는 양쪽에서 도는 코드였고, 접힌 상수를 쓰는
/// 바람에 **Windows 빌드만** 섰다 — 맥에서는 끝까지 멀쩡해 보이고 크로스 컴파일도
/// `ring` 의 C 코드에서 먼저 막혀서, CI 윈도우 러너에 올리기 전까지 아무 데서도
/// 안 드러난다. 그래서 접지 않고 값으로 가른다(2026-09-23).
const TRAFFIC_LIGHT_WIDTH: f32 = if cfg!(windows) { 0.0 } else { 78.0 };
/// iTerm-style per-pane header height in logical pixels. Each split
/// pane gets one of these strips above its cell grid; a single
/// un-split window renders no header at all (matches the iTerm
/// behavior the user pointed at).
const PANE_HEADER_HEIGHT: f32 = 34.0;
/// 설정 「칸 머리」 — 켜면 모든 칸이 머리 띠(제목·단추·더블클릭 확대)를 갖고, 끄면 hover ⋮ 만
/// 쓴다. `PaneState::has_header` 가 App 을 못 보므로 전역에 둔다.
static PANE_HEADER_BAR: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// Per-pane status bar height (logical px) — the strip below a pane's cell grid
/// holding the cwd / git-branch / diff chips. Mirrors the header band: when a
/// pane shows its bar, the PTY's usable rows shrink by the equivalent cell
/// count so the grid still fits above it. Toggled per pane (see
/// `statusbar_hidden`); a hidden pane reserves nothing.
///
/// 헤더(34)보다 낮은 건 의도다 — 헤더는 탭을 이고 있고 하단바는 칩만 얹는다.
/// 다만 예전 24 는 18px 칩에 여백이 3px 밖에 안 남아 띠가 칩에 눌려 보였다.
/// **기본값**이다 — 실제 높이는 설정에서 바뀌므로 `App::pane_footer_h()` 를 써라.
/// 이 상수를 직접 참조하면 설정을 내려도 안 따라가는 자리가 하나 생긴다.
const PANE_FOOTER_HEIGHT_DEFAULT: f32 = 30.0;
/// 창 맨 아래 상태줄 높이(logical px) — 계정 한도가 **항상** 보이는 자리.
///
/// 조건 없이 늘 예약한다. 한도는 「볼 일이 생겼을 때 찾아보는 값」이
/// 아니라 「지금 얼마나 남았나」라서, 접혀 있으면 그걸 확인하려고 패널을 여는 순간
/// 이미 늦는다. Orca 하단바(24px)와 같은 높이 — 거기서 형식을 가져왔다.
/// **기본값**이다 — 실제 높이는 설정에서 바뀌므로 `App::status_h()` 를 써라.
const STATUS_HEIGHT_DEFAULT: f32 = 24.0;
/// 활성 탭 상단 accent 선 두께(logical px). BORDER stroke(1px)보다 살짝 굵게.
const ACTIVE_ACCENT_STROKE: f32 = 2.0;
/// Inner padding between a pane's box edges and its cell grid, in logical
/// pixels. Keeps text off the divider / window edge and gives abutting
/// panes visible breathing room. The PTY's usable cols/rows shrink by the
/// equivalent cell count so the grid still fits inside the inset box, and
/// every render origin + click-to-cell map applies the same offset.
const PANE_INNER_X: f32 = 6.0;
const PANE_INNER_Y: f32 = 0.0;
/// Left sidebar width in logical pixels. Hosts the vertical tab list
/// (one row per tab) plus the new-tab "+" button. The cell grid origin
/// shifts right by this amount so pane contents never overlap the
/// sidebar. Sidebar is always shown — including single-tab sessions —
/// so the layout doesn't reflow when a second tab appears.
// Warp-style narrow vertical tab strip on the left, one tab per window
// in the current session. Logical px (the renderer multiplies by scale).
// Every `col`/`origin_x` calc already adds `WINDOW_PADDING + SIDEBAR_W`,
// so bumping this off 0 shifts the whole cell grid right automatically.
// 240 — 목록 줄이 「이름 · 지금 일」과 사정 한 줄의 두 줄이라 200 에서는 지금 일이 거의 다 잘렸다.
const SIDEBAR_W: f32 = 240.0;

/// Sidebar layout (logical px), Warp-style. Non-selected tabs are flat
/// (icon + two text lines, no box); the selected tab gets a subtle rounded
/// highlight inset from the strip edges. Tabs sit close together.
const SIDEBAR_TAB_H: f32 = 54.0;
const SIDEBAR_TAB_GAP: f32 = 3.0;
/// 방을 펼쳤을 때 그 안 pane 한 줄의 높이와, 목록 위아래 숨통.
const SIDEBAR_ROW_H: f32 = 22.0;
/// 방 카드 배치도 아래 「별도창」 띠 — 별도 OS 창으로 뗀 pane 이 한 칸씩 앉는다.
const UNDOCK_STRIP_H: f32 = 30.0;
const SIDEBAR_ROW_PAD: f32 = 8.0;
/// 방을 펴고 접는 데 걸리는 시간. 사이드바는 하루에도 여러 번 여닫는 곳이라
/// 테마 전환(0.34)보다 짧다 — 여기서 기다려야 하면 곧 안 쓰게 된다.
const EXPAND_ANIM_SECS: f32 = 0.16;
const SIDEBAR_TAB_INSET: f32 = 8.0;
/// 고른 방을 두르는 테두리 두께. 1px 은 카드 사이 구분선(1px)과 굵기가 같아
/// "골랐다"가 "칸막이"로 읽히고, 2px 은 좁은 칼럼에서 카드를 조여 보인다.
const SIDEBAR_ACTIVE_RING: f32 = 1.5;
/// 사이드바 바닥에 붙박인 트레이(+ · 피드백 · 설정)의 높이. 세션 목록은 여기까지만
/// 자란다.
const SIDEBAR_TRAY_H: f32 = 44.0;
/// File-tree column, parked just right of the session-tab sidebar (VSCode
/// explorer layout). Its own width + visibility, independent of the tab
/// strip, so the tree can be toggled / resized without touching the tabs.
/// `effective_sidebar_w()` folds this in, so the cell grid origin shifts
/// right by tabs+tree together and the terminal reflows automatically.
const FILE_TREE_W: f32 = 220.0;
const FILE_TREE_W_MIN: f32 = 140.0;
const FILE_TREE_W_MAX: f32 = 480.0;
/// Right-hand git column, mirroring the file-tree column on the left:
/// `effective_right_chrome_w()` folds its width in so the cell grid reflows
/// and panes never overlap it.
const GIT_COL_W: f32 = 420.0;
const GIT_COL_W_MIN: f32 = 220.0;
const GIT_COL_W_MAX: f32 = 720.0;

/// 창이 좁아 자동으로 좁혀질 때의 하한(logical px). 드래그 하한(`*_W_MIN`)과 따로
/// 두는 건 방향이 달라서다 — 드래그는 사용자가 원해서 줄이는 것이라 읽기 좋은 폭에서
/// 멈추지만, 이쪽은 창이 강제해서 줄어드는 것이라 「접히느니 좁게라도 남는다」가 낫다.
/// 창을 다시 넓히면 사용자가 정한 폭으로 저절로 되돌아간다(원본 값을 안 건드린다).
const SIDEBAR_W_AUTO_MIN: f32 = 104.0;
const FILE_TREE_W_AUTO_MIN: f32 = 132.0;
const GIT_COL_W_AUTO_MIN: f32 = 216.0;
/// 자동 축소가 터미널에 남겨 주려는 폭(칸). chrome 세 기둥의 합이 창을 넘치면 이만큼을
/// 먼저 떼어 두고 남는 것만 나눠 준다.
const GRID_KEEP_COLS: f32 = 40.0;
/// 칸수의 절대 하한. 예산이 이미 폭을 지켜 주므로 여기 걸리는 건 창이 극단적으로
/// 작을 때뿐인데, 그때도 0 칸을 PTY 에 알리면 TUI 가 깨지므로 바닥을 둔다.
///
/// 예전엔 이 자리가 40 이었다. 그 값은 하한이 아니라 **거짓말**이었다 — 쓸 폭이
/// 160px 뿐인데도 40칸(340px)이라고 PTY 에 알려, 터미널이 우측 칼럼 밑으로 180px
/// 파고들어 그려졌다(1000px 창에서 실측). 폭은 예산이 지키고 여기는 바닥만 맡는다.
const GRID_MIN_COLS: f32 = 20.0;

/// 기둥이 자기 폭에 맞춰 **내용을 바꾸는** 문턱(logical px). 폭만 줄이면 좁은 칼럼에
/// 넓을 때 쓰던 글자가 그대로 남아 잘리거나 서로 겹친다 — 좁아지면 덜 중요한 것을
/// 덜어내고 남길 것만 남겨야 한다(2026-08-30 지시: "좁아도 그에맞는 정보가 나오게").
const SIDEBAR_DENSE_FULL: f32 = 168.0;
const SIDEBAR_DENSE_COMPACT: f32 = 124.0;
const FILE_TREE_DENSE_FULL: f32 = 186.0;
const FILE_TREE_DENSE_COMPACT: f32 = 150.0;
const GIT_DENSE_FULL: f32 = 340.0;
const GIT_DENSE_COMPACT: f32 = 248.0;
/// 칼럼 발치 「최근 커밋」 구역이 손대기 전에 보여 주는 줄 수. 사용자가 그 구역의
/// 경계선을 끌면 높이가 잡히고, 그때부터는 높이에 들어가는 만큼 가져온다.
const GIT_RECENT_COMMITS_DEFAULT: usize = 5;
/// 그 구역의 최소·최대 높이(LOGICAL px). 최소는 머리 24px + 한 줄 20px 이라 끝까지
/// 줄여도 커밋 하나는 남는다 — 0 까지 줄면 경계선도 함께 사라져 되돌릴 손잡이가
/// 없어진다. 최대는 창 크기에 따라 다시 좁혀지므로(`commits_cap`) 여기선 헐겁게.
const GIT_COMMITS_H_MIN: f32 = 44.0;
const GIT_COMMITS_H_MAX: f32 = 4000.0;
const SCROLLBACK_MAX: usize = 5000;
/// Min ms between wheel emits. Default 0 = pass every macOS scroll event
/// straight through to `pty.scroll`; the try_send-based reader pipeline
/// absorbs the burst without back-pressuring bash, so throttling here just
/// muddies the inertia feel. Raise via `KASATERM_WHEEL_THROTTLE_MS=<n>` to
/// dampen if you want a smoother (lenis-style) scroll.
fn wheel_throttle_ms() -> u64 {
    static CACHED: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *CACHED.get_or_init(|| {
        std::env::var("KASATERM_WHEEL_THROTTLE_MS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0)
    })
}
/// PixelDelta(트랙패드·고해상도 마우스휠) 스크롤 감도 배율. 기본 **0.3** 은 트랙패드
/// 기준이다 — 트랙패드와 고해상도 마우스휠은 winit 에서 **같은 PixelDelta 로 와서
/// 구분할 수가 없으므로**, 한쪽에 맞추면 다른 쪽이 어긋난다. 마우스휠이 굼떠 1.0 +
/// 최소 1셀 floor 로 올렸던 적이 있는데(2026-07-23 `1aa9b6c`) 그러자 트랙패드가 세
/// 배 넘게 민감해졌다(사용자: "트랙패드 스크롤 원래대로 돌려줘"). 손이 늘 닿아 있는
/// 쪽을 기본값으로 두고, 마우스를 쓸 때 설정에서 올린다.
///
/// 설정(`settings.json` 의 `wheel_pixel_gain`) → env(`KASATERM_WHEEL_PIXEL_GAIN`) 순.
/// 매 휠 이벤트마다 파일을 읽을 수는 없어 캐시하고, 설정을 저장할 때 비운다.
static WHEEL_GAIN_CACHE: std::sync::RwLock<Option<f32>> = std::sync::RwLock::new(None);
pub(crate) fn wheel_pixel_gain() -> f32 {
    if let Ok(g) = WHEEL_GAIN_CACHE.read() {
        if let Some(v) = *g {
            return v;
        }
    }
    let v = std::env::var("KASATERM_WHEEL_PIXEL_GAIN")
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|v: &f32| *v > 0.0)
        .unwrap_or_else(socket::read_wheel_pixel_gain);
    if let Ok(mut g) = WHEEL_GAIN_CACHE.write() {
        *g = Some(v);
    }
    v
}
/// 설정이 바뀌었으니 다음 휠 이벤트에서 다시 읽어라.
pub(crate) fn invalidate_wheel_pixel_gain() {
    if let Ok(mut g) = WHEEL_GAIN_CACHE.write() {
        *g = None;
    }
}
/// Half-period of the cursor blink in milliseconds. macOS uses 530 by
/// default; iTerm2 uses 500. 530 matches the platform feel.
const BLINK_HALF_PERIOD_MS: u64 = 530;
/// Launch build banner: hold the `v…·<rev>` corner label fully visible
/// for HOLD ms, then fade it out over FADE ms so you can tell builds
/// apart at startup without it nagging afterwards.
const VERSION_HOLD_MS: u128 = 4000;
const VERSION_FADE_MS: u128 = 1200;
/// While the user is actively typing we keep the cursor solid for this
/// long after the last keystroke so it's easy to follow the caret. Same
/// idea as iTerm2's "smart cursor" pause.
const BLINK_PAUSE_AFTER_INPUT_MS: u64 = 700;

/// Image-viewer window page. `__NAME__` is the filename (title strip),
/// `__SRC__` a self-contained `data:` URI of the image bytes (injected at
/// open time, so the page is fully offline). Fit-to-window by default;
/// clicking the image toggles 1:1 actual size with scroll-to-pan.
#[allow(dead_code)]
const IMAGE_VIEWER_HTML: &str = r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<style>
  :root { color-scheme: dark; }
  * { box-sizing: border-box; }
  html, body { height: 100%; margin: 0; }
  body {
    display: flex; flex-direction: column;
    background: #1a1d23; color: #ecedf3;
    font: 12px/1.4 -apple-system, "SF Pro Text", system-ui, sans-serif;
    -webkit-user-select: none; user-select: none;
  }
  .bar {
    display: flex; align-items: center; gap: 10px;
    padding: 8px 12px; background: #101217; border-bottom: 1px solid #101217;
    flex: 0 0 auto;
  }
  .name { font-weight: 600; white-space: nowrap; overflow: hidden;
    text-overflow: ellipsis; }
  .dim { color: #787e8a; margin-left: auto; }
  .stage {
    flex: 1 1 auto; overflow: auto; display: flex;
    align-items: center; justify-content: center; padding: 12px;
  }
  /* checkerboard so transparent PNGs read clearly */
  .stage {
    background-image:
      linear-gradient(45deg, #20242c 25%, transparent 25%),
      linear-gradient(-45deg, #20242c 25%, transparent 25%),
      linear-gradient(45deg, transparent 75%, #20242c 75%),
      linear-gradient(-45deg, transparent 75%, #20242c 75%);
    background-size: 20px 20px;
    background-position: 0 0, 0 10px, 10px -10px, -10px 0;
  }
  img { display: block; }
  img.fit { max-width: 100%; max-height: 100%; object-fit: contain; cursor: zoom-in; }
  img.actual { max-width: none; max-height: none; cursor: zoom-out; }
</style>
</head>
<body>
  <div class="bar">
    <span class="name">__NAME__</span>
    <span class="dim" id="hint">클릭: 원본 크기 ↔ 맞춤</span>
  </div>
  <div class="stage" id="stage">
    <img id="img" class="fit" src="__SRC__" alt="__NAME__">
  </div>
<script>
  const img = document.getElementById('img');
  img.addEventListener('click', () => {
    if (img.classList.contains('fit')) {
      img.classList.remove('fit'); img.classList.add('actual');
    } else {
      img.classList.remove('actual'); img.classList.add('fit');
    }
  });
</script>
</body>
</html>"#;

/// Markdown editor/preview window page. `__NAME__` filename, `__PATH__` the
/// absolute path (JSON string), `__CONTENT__` the file text (JSON string),
/// `__PORT__` the MCP server port for the save POST. Split textarea + live
/// preview rendered by a tiny inline parser (no CDN, works offline). Save
/// POSTs `{path, content}` as text/plain (a CORS "simple" request — no
/// preflight, same trick the git-commit panel uses).
#[allow(dead_code)]
const MARKDOWN_EDITOR_HTML: &str = r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<style>
  :root { color-scheme: dark; }
  * { box-sizing: border-box; }
  html, body { height: 100%; margin: 0; }
  body {
    display: flex; flex-direction: column;
    background: #1a1d23; color: #ecedf3;
    font: 13px/1.5 -apple-system, "SF Pro Text", system-ui, sans-serif;
  }
  .bar {
    display: flex; align-items: center; gap: 10px;
    padding: 8px 12px; background: #101217; flex: 0 0 auto;
  }
  .name { font-weight: 600; white-space: nowrap; overflow: hidden;
    text-overflow: ellipsis; }
  .actions { margin-left: auto; display: flex; align-items: center; gap: 10px; }
  #status { font-size: 11px; color: #787e8a; }
  #status.ok { color: #3fb950; }
  #status.bad { color: #f85149; }
  button {
    background: #238636; border: 1px solid #2ea043; color: #fff;
    border-radius: 6px; padding: 5px 12px; font-size: 12px; cursor: pointer;
  }
  button:hover:not(:disabled) { background: #2ea043; }
  button:disabled { opacity: .5; cursor: default; }
  .split { flex: 1 1 auto; display: flex; min-height: 0; }
  .pane { flex: 1 1 50%; min-width: 0; overflow: auto; }
  .pane.edit { border-right: 1px solid #101217; }
  textarea {
    width: 100%; height: 100%; resize: none; border: 0; outline: 0;
    background: #1a1d23; color: #ecedf3; padding: 14px;
    font: 13px/1.6 ui-monospace, "SF Mono", Menlo, monospace;
    -webkit-user-select: text; user-select: text;
  }
  .preview { padding: 14px 18px; }
  .preview h1, .preview h2, .preview h3 { line-height: 1.25; margin: 1em 0 .5em; }
  .preview h1 { font-size: 1.7em; border-bottom: 1px solid #22262e; padding-bottom: .25em; }
  .preview h2 { font-size: 1.4em; border-bottom: 1px solid #22262e; padding-bottom: .2em; }
  .preview h3 { font-size: 1.15em; }
  .preview p { margin: .6em 0; }
  .preview a { color: #5a8ce6; }
  .preview code {
    background: #101217; border-radius: 5px; padding: .12em .4em;
    font: .9em ui-monospace, Menlo, monospace;
  }
  .preview pre {
    background: #101217; border-radius: 9px; padding: 10px 12px; overflow-x: auto;
  }
  .preview pre code { background: none; padding: 0; }
  .preview blockquote {
    margin: .6em 0; padding: .2em .9em; border-left: 3px solid #2e323b; color: #a0a6b0;
  }
  .preview ul, .preview ol { padding-left: 1.4em; margin: .5em 0; }
  .preview img { max-width: 100%; }
  .preview hr { border: 0; border-top: 1px solid #22262e; margin: 1.2em 0; }
  .preview table { border-collapse: collapse; }
  .preview td, .preview th { border: 1px solid #22262e; padding: 4px 8px; }
</style>
</head>
<body>
  <div class="bar">
    <span class="name">__NAME__</span>
    <span class="actions">
      <span id="status"></span>
      <button id="save">저장</button>
    </span>
  </div>
  <div class="split">
    <div class="pane edit"><textarea id="src" spellcheck="false"></textarea></div>
    <div class="pane"><div class="preview" id="preview"></div></div>
  </div>
<script>
  const PORT = "__PORT__";
  const PATH = __PATH__;
  const INITIAL = __CONTENT__;
  const src = document.getElementById('src');
  const preview = document.getElementById('preview');
  const status = document.getElementById('status');
  const saveBtn = document.getElementById('save');

  function esc(s) {
    return s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
  }
  // Inline spans: code, bold, italic, links, images. `code` first so its
  // contents aren't re-parsed for emphasis.
  function inline(s) {
    s = esc(s);
    s = s.replace(/`([^`]+)`/g, (_, c) => '<code>' + c + '</code>');
    s = s.replace(/!\[([^\]]*)\]\(([^)\s]+)\)/g, '<img alt="$1" src="$2">');
    s = s.replace(/\[([^\]]+)\]\(([^)\s]+)\)/g, '<a href="$2">$1</a>');
    s = s.replace(/\*\*([^*]+)\*\*/g, '<strong>$1</strong>');
    s = s.replace(/__([^_]+)__/g, '<strong>$1</strong>');
    s = s.replace(/\*([^*]+)\*/g, '<em>$1</em>');
    s = s.replace(/_([^_]+)_/g, '<em>$1</em>');
    return s;
  }
  // Block-level: fenced code, headings, hr, blockquote, lists, paragraphs.
  function render(md) {
    const lines = md.replace(/\r\n/g, '\n').split('\n');
    let html = '', i = 0;
    while (i < lines.length) {
      let line = lines[i];
      if (/^```/.test(line)) {
        let body = []; i++;
        while (i < lines.length && !/^```/.test(lines[i])) { body.push(lines[i]); i++; }
        i++;
        html += '<pre><code>' + esc(body.join('\n')) + '</code></pre>';
        continue;
      }
      let h = line.match(/^(#{1,6})\s+(.*)$/);
      if (h) { const n = h[1].length; html += '<h' + n + '>' + inline(h[2]) + '</h' + n + '>'; i++; continue; }
      if (/^\s*([-*_])\s*\1\s*\1[\s\1]*$/.test(line)) { html += '<hr>'; i++; continue; }
      if (/^\s*>/.test(line)) {
        let body = [];
        while (i < lines.length && /^\s*>/.test(lines[i])) { body.push(lines[i].replace(/^\s*>\s?/, '')); i++; }
        html += '<blockquote>' + render(body.join('\n')) + '</blockquote>';
        continue;
      }
      if (/^\s*[-*+]\s+/.test(line)) {
        let items = [];
        while (i < lines.length && /^\s*[-*+]\s+/.test(lines[i])) { items.push(lines[i].replace(/^\s*[-*+]\s+/, '')); i++; }
        html += '<ul>' + items.map(t => '<li>' + inline(t) + '</li>').join('') + '</ul>';
        continue;
      }
      if (/^\s*\d+\.\s+/.test(line)) {
        let items = [];
        while (i < lines.length && /^\s*\d+\.\s+/.test(lines[i])) { items.push(lines[i].replace(/^\s*\d+\.\s+/, '')); i++; }
        html += '<ol>' + items.map(t => '<li>' + inline(t) + '</li>').join('') + '</ol>';
        continue;
      }
      if (line.trim() === '') { i++; continue; }
      let para = [];
      while (i < lines.length && lines[i].trim() !== '' && !/^(#{1,6}\s|```|\s*>|\s*[-*+]\s|\s*\d+\.\s)/.test(lines[i])) {
        para.push(lines[i]); i++;
      }
      html += '<p>' + inline(para.join('\n')).replace(/\n/g, '<br>') + '</p>';
    }
    return html;
  }
  function refresh() { preview.innerHTML = render(src.value); }

  src.value = INITIAL;
  refresh();
  let dirty = false;
  src.addEventListener('input', () => { refresh(); dirty = true; status.textContent = '편집됨'; status.className = ''; });

  async function save() {
    saveBtn.disabled = true; status.textContent = '저장 중…'; status.className = '';
    try {
      const r = await fetch('http://127.0.0.1:' + PORT + '/save-markdown', {
        method: 'POST', headers: { 'Content-Type': 'text/plain' },
        body: JSON.stringify({ path: PATH, content: src.value }),
      });
      const j = await r.json();
      if (j.ok) { status.textContent = '저장됨'; status.className = 'ok'; dirty = false; }
      else { status.textContent = j.error || '저장 실패'; status.className = 'bad'; }
    } catch (e) {
      status.textContent = '저장 실패: ' + e; status.className = 'bad';
    } finally { saveBtn.disabled = false; }
  }
  saveBtn.addEventListener('click', save);
  window.addEventListener('keydown', (e) => {
    if ((e.metaKey || e.ctrlKey) && e.key === 's') { e.preventDefault(); save(); }
  });
</script>
</body>
</html>"#;

/// Cell width / height / baseline in logical pixels. Filled at startup
/// from `Sugarloaf::compute_cell_metrics` so columns align with the
/// actual font advance instead of a hardcoded guess. Falls back to a
/// reasonable default before the first measurement lands.
#[derive(Copy, Clone, Debug)]
struct CellGeom {
    w: f32,
    h: f32,
    #[allow(dead_code)]
    baseline: f32,
}

impl Default for CellGeom {
    fn default() -> Self {
        Self {
            w: 8.6,
            h: 18.0,
            baseline: 14.0,
        }
    }
}

/// Normalise so (start.row, start.col) <= (end.row, end.col) in reading
/// order. Used both for highlight rendering and clipboard extraction.
fn normalise(sel: Selection) -> ((u16, u16), (u16, u16)) {
    let a = sel.anchor;
    let b = sel.end;
    if (a.1, a.0) <= (b.1, b.0) {
        (a, b)
    } else {
        (b, a)
    }
}

/// Append a run of grid cells to `out` as text, dropping the blank
/// "spacer" cell that trails every full-width (CJK) glyph. The grid
/// stores a wide character as TWO cells — the glyph in the first, an
/// empty placeholder in the second — so a naive copy emits a stray space
/// after each syllable ("한글" → "한 글"). We peek: a cell right after a
/// wide char is its spacer and gets skipped. Genuine blanks (empty cell
/// not following a wide char) still render as a single space.
/// 입력칸 줄(`❯ `·`› `)의 글이 흐린 글씨면 붙일 표시. 다 흐리면 추천·안내 글, 뒤만 흐리면 자동완성 제안이다.
/// 고른 선택지(`❯ 1. Yes`)처럼 흐리지 않은 줄은 그대로 둔다.
fn ghost_note(cells: &[(char, bool)]) -> Option<&'static str> {
    let blank = |c: char| c == ' ' || c == '\0';
    let first = cells.iter().position(|(c, _)| !blank(*c))?;
    if !matches!(cells[first].0, '❯' | '›') {
        return None;
    }
    let body: Vec<bool> = cells[first + 1..].iter().filter(|(c, _)| !blank(*c)).map(|(_, d)| *d).collect();
    let dim = body.iter().filter(|d| **d).count();
    if dim == 0 {
        None
    } else if dim == body.len() {
        Some("  ⟨흐린 글 — 추천·안내일 뿐, 입력된 글 아님⟩")
    } else if body.iter().skip_while(|d| !**d).all(|d| *d) {
        Some("  ⟨뒤의 흐린 부분은 추천일 뿐, 입력된 글 아님⟩")
    } else {
        None
    }
}

#[cfg(test)]
mod ghost_note_tests {
    use super::ghost_note;

    fn row(text: &str, dim_from: usize) -> Vec<(char, bool)> {
        text.chars().enumerate().map(|(i, c)| (c, i >= dim_from)).collect()
    }

    #[test]
    fn only_a_dim_prompt_line_gets_the_note() {
        assert_eq!(ghost_note(&row("❯ 깔았어 확인해봐", 2)), Some("  ⟨흐린 글 — 추천·안내일 뿐, 입력된 글 아님⟩"));
        assert_eq!(ghost_note(&row("› Ask Codex to do anything", 2)), Some("  ⟨흐린 글 — 추천·안내일 뿐, 입력된 글 아님⟩"));
        assert_eq!(ghost_note(&row("❯ 깔았 어떻게", 6)), Some("  ⟨뒤의 흐린 부분은 추천일 뿐, 입력된 글 아님⟩"));
        assert_eq!(ghost_note(&row("❯ 1. Yes", 99)), None, "고른 선택지는 흐리지 않다");
        assert_eq!(ghost_note(&row("❯ ", 0)), None, "빈 입력칸");
        assert_eq!(ghost_note(&row("  흐린 안내 줄", 0)), None, "입력칸 줄이 아니면 안 붙인다");
    }
}

fn append_cells_text<'a>(cells: impl IntoIterator<Item = &'a GridCell>, out: &mut String) {
    let mut skip_spacer = false;
    for cell in cells {
        if cell.leading_wide_spacer {
            skip_spacer = false;
            continue;
        }
        if skip_spacer {
            skip_spacer = false;
            // The spacer is blank by construction; only swallow it when it
            // really is empty/space so a glitch never eats real text.
            if cell.ch == '\0' || cell.ch == ' ' {
                continue;
            }
        }
        if cell.ch == '\0' {
            out.push(' ');
        } else {
            out.push(cell.ch);
            skip_spacer = gpu::is_wide_char(cell.ch);
        }
    }
}

/// Most recent scrollback lines to persist per pane on exit. Caps the saved
/// session file so a pane with a huge history doesn't bloat session.json.
const SCROLLBACK_SAVE_MAX: usize = 500;

/// 자동 스냅샷 주기. 강제 종료 시 잃는 최대치가 이 값이다. 짧을수록 안전하지만
/// pane 마다 500줄을 직렬화하므로, 체감되지 않으면서 손실이 충분히 작은 값으로.
pub(crate) const SESSION_AUTOSAVE_PERIOD: std::time::Duration = std::time::Duration::from_secs(5);

/// Capture a pane's scrollback (history + current screen) as trimmed text
/// lines for session restore, newest-biased: keeps the last SCROLLBACK_SAVE_MAX
/// lines and drops the trailing blank rows so a restored pane doesn't carry an
/// empty tail. v1 saves text only (color/attrs dropped) — the content is what
/// "what I typed/saw is still there" needs.
/// pane 이 아니라 **화면 하나**를 받는다. `PaneState` 는 활성 탭으로 Deref 하므로,
/// pane 을 그대로 읽으면 탭마다 제 스크롤백을 저장할 수가 없다.
fn term_scrollback_lines(t: &TerminalPane) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for row in t.history.iter().chain(t.cells.iter()) {
        let mut s = String::new();
        append_cells_text(row.iter(), &mut s);
        lines.push(s.trim_end().to_string());
    }
    while lines.last().map_or(false, |l| l.is_empty()) {
        lines.pop();
    }
    if lines.len() > SCROLLBACK_SAVE_MAX {
        lines.drain(0..lines.len() - SCROLLBACK_SAVE_MAX);
    }
    lines
}

/// Pull the selected text out of the visible row grid. Joined with `\n`,
/// trailing spaces trimmed per row. Mirrors kasaterm::extract_selection.
/// 렌더가 뷰포트 원본(`term.cells`) 대신 그리는 **보기 배치** — 거울 pane 을 이 창
/// 폭에 맞춰 다시 접은 것(`projection`). 화면에서 고른 글자와 클립보드에 담기는 글자가
/// 같으려면 복사·링크·마우스도 이걸 되짚어야 한다(2026-09-05 "복사가 이상하게 되고").
///
/// classic claude 의 입력창을 바닥에 붙이던 옮김(여백 끼우기·입력창 붙잡기)도 여기
/// 살았는데, claude·codex 를 대체화면으로 돌리며 걷었다(2026-09-28).
#[derive(Default, Clone)]
struct PaneViewShift {
    /// Independent viewer layout with exact display-to-source cell mapping.
    projection: Option<Arc<crate::mirror_view::Projection>>,
}

impl PaneViewShift {
    /// 화면 행 `r` 에 **실제로 그려진** 줄. 화면 좌표를 쓰는 모든 판독(복사·링크
    /// 집기)이 이걸 거쳐야 고른 자리와 집히는 글자가 같다.
    fn row<'a>(&'a self, r: usize, base: &'a [Vec<GridCell>]) -> Option<&'a Vec<GridCell>> {
        match &self.projection {
            Some(projection) => projection.rows.get(r),
            None => base.get(r),
        }
    }

    /// 화면 행을 **앱이 아는 행**으로 되돌린다 — 마우스 이벤트를 TUI 로 넘길 때
    /// 쓴다. 보기 전용으로 다시 접은 칸은 앱 화면에 없으니 `None`.
    fn term_row(&self, r: usize) -> Option<usize> {
        match &self.projection {
            Some(projection) => projection.source_map.get(r)?.iter().flatten().next().map(|&(row, _)| row),
            None => Some(r),
        }
    }

    fn term_pos(&self, row: usize, col: usize) -> Option<(usize, usize)> {
        match &self.projection {
            Some(projection) => projection.source_map.get(row)?.get(col).copied().flatten(),
            None => self.term_row(row).map(|row| (row, col)),
        }
    }

    fn display_pos(&self, row: usize, col: usize) -> Option<(usize, usize)> {
        if let Some(projection) = &self.projection {
            return projection.source_map.iter().enumerate().find_map(|(r, cells)| {
                cells.iter().position(|cell| *cell == Some((row, col))).map(|c| (r, c))
            });
        }
        Some((row, col))
    }

    /// 화면에 실제로 그려진 그대로의 행들. 복사는 원본이 아니라 이걸 봐야 한다.
    fn compose(&self, base: &[Vec<GridCell>]) -> Vec<Vec<GridCell>> {
        match &self.projection {
            Some(projection) => projection.rows.clone(),
            None => base.to_vec(),
        }
    }
}

fn extract_selection(rows: &[Vec<GridCell>], sel: Selection) -> String {
    let (start, end) = normalise(sel);
    let mut out = String::new();
    for (r, row) in rows.iter().enumerate() {
        let r = r as u16;
        if r < start.1 || r > end.1 {
            continue;
        }
        let (mut cs, ce) = if start.1 == end.1 {
            (start.0 as usize, end.0 as usize)
        } else if r == start.1 {
            (start.0 as usize, row.len().saturating_sub(1))
        } else if r == end.1 {
            (0, end.0 as usize)
        } else {
            (0, row.len().saturating_sub(1))
        };
        // A selection can start on the second cell of a wide glyph. That cell
        // is not a selected space, and its glyph lies outside the column range.
        if cs > 0 && row.get(cs).is_some_and(|cell| matches!(cell.ch, ' ' | '\0'))
            && row.get(cs - 1).is_some_and(|cell| gpu::is_wide_char(cell.ch))
        {
            cs += 1;
        }
        append_cells_text(row.iter().take(ce + 1).skip(cs), &mut out);
        let continues = r < end.1 && (r as usize + 1) < rows.len()
            && row.last().is_some_and(|cell| cell.wrapped);
        if !continues {
            // Viewer reflow is not a user-authored newline. Preserve real
            // spaces at a soft boundary; trim only a complete logical line or
            // the end of the selected range, as before.
            while out.ends_with(' ') { out.pop(); }
            out.push('\n');
        }
    }
    if out.ends_with('\n') {
        out.pop();
    }
    // Trim blank lines at both ends. A drag across an alt-screen TUI
    // (Claude Code, less, etc.) usually picks up empty padding rows
    // above/below the visible text — strip those so what lands in the
    // clipboard matches what the user actually highlighted.
    let trimmed: Vec<&str> = out
        .lines()
        .skip_while(|l| l.trim().is_empty())
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .skip_while(|l| l.trim().is_empty())
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    trimmed.join("\n")
}

/// True for any codepoint the Hangul IME composes. Covers the four
/// Unicode blocks that hold Korean syllables and jamo. Used to drop
/// keyboard-side Character events when the IME is the authoritative
/// channel — winit emits both `KeyboardInput.text` and `Ime::Preedit`
/// for the first keystroke after a script switch, and forwarding both
/// would echo the jamo twice (or, more often, leak the first 자모 to
/// the shell as raw `ㅎ` / `ㅇ` before the Commit lands).
fn is_hangul_codepoint(c: char) -> bool {
    let cp = c as u32;
    (0x1100..=0x11FF).contains(&cp) // Hangul Jamo
        || (0x3130..=0x318F).contains(&cp) // Hangul Compat Jamo
        || (0xA960..=0xA97F).contains(&cp) // Hangul Jamo Extended-A
        || (0xAC00..=0xD7A3).contains(&cp) // Hangul Syllables
        || (0xD7B0..=0xD7FF).contains(&cp) // Hangul Jamo Extended-B
}

/// Wheel accumulator. Returns Some(lines) when an emit fires, None while
/// accumulating sub-cell ticks or while the throttle window is open.
/// Mirrors kasaterm-cli's wheel_step semantics exactly.
fn wheel_step(
    accum: &mut f32,
    dy_cells: f32,
    last_emit: &mut Instant,
    now: Instant,
) -> Option<i32> {
    if accum.signum() != dy_cells.signum() && *accum != 0.0 && dy_cells != 0.0 {
        *accum = 0.0;
    }
    *accum += dy_cells;
    let lines = accum.trunc() as i32;
    if lines == 0 {
        return None;
    }
    if now.duration_since(*last_emit) < std::time::Duration::from_millis(wheel_throttle_ms()) {
        return None;
    }
    *accum -= lines as f32;
    *last_emit = now;
    Some(lines)
}

/// Per-pane render state. One of these per tmux pane (`%N`). Holds the
/// cell grid, scrollback ring, cursor, and the flags we need to route
/// wheel events correctly (alt-screen / SGR mouse mode are per-pane in
/// real terminals — claude in pane 0 can be in alt-screen while a
/// shell prompt sits in pane 1).
/// Direction for spatial pane focus / swap (Cmd+Option+Arrow).
#[derive(Clone, Copy)]
enum FocusDir {
    Left,
    Right,
    Up,
    Down,
}

/// Which edge of the drop-target pane the cursor is over during a header
/// drag. Determines where the dragged pane lands: a Left/Right drop
/// splits horizontally, Up/Down splits vertically. `Center` means the
/// cursor sits in the inner 50% square — drop merges the tab into the
/// target pane's tab strip instead of splitting.
#[derive(Clone, Copy, PartialEq, Debug)]
enum DropZone {
    Left,
    Right,
    Up,
    Down,
    Center,
}

/// State for an in-flight header drag-and-drop relocation.
struct HeaderDrag {
    /// Pane being dragged (its tree leaf id).
    pane: String,
    /// Press position in logical px, to measure the click→drag threshold.
    start: (f32, f32),
    /// True once the cursor moved far enough to count as a drag rather
    /// than a click.
    active: bool,
    /// True when the drag began on a ⋮ handle (header-less pane). A release
    /// under the drag threshold then toggles that pane's handle menu; a
    /// header-bar drag just focuses.
    from_handle: bool,
}

/// Image-pane action button kinds, drawn in place of the terminal-action
/// cluster on the right side of an image pane's header.
#[derive(Clone, Copy, PartialEq)]
enum ImageBtn {
    ZoomOut,
    ZoomIn,
    Rotate,
    Reset,
}

/// `to` 가 끝난 뒤 걷을 자리와 열 방. `at` 이 지나면 `tick_migrate_handoff` 가 처리한다.
struct MigrateHandoff {
    pid: String,
    label: String,
    base: String,
    remote_id: String,
    at: std::time::Instant,
}

/// What a confirmed close-dialog should actually close.
#[derive(Clone, Debug)]
enum PendingClose {
    /// Drop one tab of a multi-tab pane.
    Tab { pane: String, idx: usize },
    /// Drop a whole pane (its last tab).
    Pane { pane: String },
    /// Close one sidebar session (window `idx`) — the app stays open. Distinct
    /// from `Window` (whole-app quit): only this session's panes are killed.
    Session(usize),
    /// 다른 기기의 방(사이드바 절의 카드 ×·메뉴). 그쪽 `window.close` 로 보낸다.
    RemoteRoom { label: String, window: Option<u64>, room: String },
    /// Close one detached document window. The WindowId stays stable while
    /// Vec indices move when another auxiliary window closes.
    AuxEditor(WindowId),
    /// Quit the app (window red-light / Cmd+W on the last pane).
    Window,
}

/// Where one unsaved editor lives, so the dialog can go back and save it (or
/// drop its changes) once the user has decided.
#[derive(Clone)]
enum DirtyDoc {
    /// Tab `tab` of pane `pane` in the main window.
    Tab { pane: String, tab: usize },
    /// A document owned by a detached OS window.
    Aux(WindowId),
}

/// Why a close is being held up.
#[derive(Clone)]
enum CloseWhy {
    Mirror {
        targets: Vec<mirror_close::MirrorTarget>,
        closing: bool,
        error: Option<String>,
    },
    /// A real foreground job is running — the process name, for the message.
    Busy(String),
    /// Editors with unsaved changes: where each one is, and its file name.
    Dirty(Vec<(DirtyDoc, String)>),
    /// Cmd+W 를 눌렀는데 그 pane 이 이 방의 **마지막**이라, 닫으면 방(세션)째
    /// 사라지는 경우. 전에는 여기서 아무 일도 안 일어나 키가 죽은 것처럼 보였다
    /// (사용자: "pane 하나 있고 다른 방 있으면 커맨드 W 해도 무반응"). 방이 하나뿐일
    /// 때는 여전히 no-op 다 — 그건 앱 종료라 OS 닫기 버튼(Cmd+Q)의 몫이다.
    ///
    /// 바쁜 것도 저장 안 된 것도 없어도 **무조건 묻는다**: Cmd+W 는 「하나 닫기」로
    /// 익힌 키인데 여기선 방 전체가 닫혀, 조용히 실행하면 되돌릴 수 없는 것을
    /// 눌린 줄도 모르고 잃는다.
    LastPane,
}

/// A pending close confirmation: `why` it was raised, `action` is what
/// proceeding actually closes.
#[derive(Clone)]
struct ConfirmClose {
    why: CloseWhy,
    action: PendingClose,
}

/// Buttons in the confirm-close modal. A busy dialog shows 취소/닫기; an
/// unsaved-changes dialog shows 취소/저장 안 함/저장 — `Close` is the
/// "proceed without saving" button in both.
#[derive(Clone, Copy, PartialEq)]
enum ConfirmBtn {
    Cancel,
    Close,
    Save,
    CloseSource,
}

/// The two buttons in the Chrome-style session-restore prompt shown at launch
/// when a saved layout is found.
#[derive(Clone, Copy, PartialEq)]
enum RestoreBtn {
    /// Rebuild the saved layout and resume each pane's claude session.
    Restore,
    /// Discard the saved state and keep the fresh single-pane session.
    Fresh,
    /// 카드만 접는다 — 저장본은 건드리지 않고 아무것도 복원하지 않는다. 닫기(×)와
    /// Esc 가 이것. 「고르지 않고 빠져나갈 길」이 없어서 카드가 화면을 가둔 채로
    /// 남았다(2026-08-27 지적).
    Dismiss,
}

/// A login shell (vs. a real foreground job like claude / vim / a build).
/// Closing a pane whose foreground is just a shell needs no confirmation.
fn is_shell_name(name: &str) -> bool {
    let base = name.strip_prefix('-').unwrap_or(name);
    matches!(
        base,
        "zsh" | "bash" | "fish" | "sh" | "dash" | "tcsh" | "ksh"
    )
}

/// Terminal-pane action button kinds, painted on the right side of a
/// terminal pane's header (split-v / split-h). New-terminal and web were
/// dropped — the +button covers "new shell" and the web overlay added
/// complexity for little payoff. Wired to per-frame `pane_action_hits`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ActionKind {
    SplitV,
    SplitH,
    /// Toggle this pane's bottom status bar (cwd / branch / diff). Lives next to
    /// the split buttons; collapsing the bar gives the cell grid its rows back.
    ToggleStatusbar,
    /// Markdown panes only: the "Rendered | Raw" header segmented toggle.
    /// `MdRender` shows the laid-out doc; `MdRaw` opens the wgpu source editor.
    /// View changes never write the file; `MdSave` is the only save action.
    MdRender,
    MdRaw,
    /// Markdown panes only: save without changing view mode.
    MdSave,
    /// Move this exact MarkdownPane into a detached OS window.
    MdPopout,
    /// 터미널 pane(또는 활성 탭)을 별도 OS 창으로 뗀다 — auxterm.rs.
    Undock,
    /// ghostty ⋮ 메뉴의 "닫기" — 이 pane을 닫는다.
    Close,
    /// ··· 메뉴의 "새 탭" — 이 pane(outer)에 in-pane 탭을 추가한다. 탭이 둘
    /// 이상이 되면 has_header()가 켜져 탭 스트립이 보인다.
    NewTab,
    /// ⋮ 메뉴의 상단바(pane 헤더) 토글. 하단 상태바 토글과 대칭 — 헤더는 지금까지
    /// 탭 여러 개·이미지·md 일 때만 자동으로 떴고 사용자가 켤 방법이 없었다.
    ToggleHeader,
    /// ⋮ 메뉴의 최대화 — 헤더/탭 더블클릭과 같은 tmux 식 줌. 제스처를 모르거나
    /// 더블클릭이 안 잡히는 경우를 위한 보이는 경로.
    ToggleZoom,
    /// ⋮ 메뉴의 화면 새로고침 — Cmd+Shift+R 과 같은 `refresh_renderer()`.
    /// pane 스코프 메뉴에 있지만 동작은 창 전체다. 모니터를 옮겨 화면이 깨졌을 때
    /// 단축키를 모르는 채로도 닿을 수 있는 자리가 필요했다.
    RefreshRenderer,
    /// 탭 띠 헤더 오른쪽의 ⋮ 단추 — 단추 넷을 접고 ⋮ 메뉴(`handle_menu`)를 연다.
    /// 헤더 우클릭과 같은 메뉴다.
    HandleMenu,
    /// ⋮ 메뉴의 「대화로 보기 / 터미널로 보기」 — 학생 pane 만(chat_view.rs).
    ChatView,
    /// 웹 pane 헤더의 브라우저 컨트롤 — 터미널 클러스터(분할·상태바) 대신
    /// 이 넷이 그려진다. 실행은 webpane::web_nav.
    WebBack,
    WebForward,
    WebReload,
    /// 현재 주소를 OS 기본 브라우저로 연다 — 로그인·개발자도구가 필요해지면
    /// 내장 웹뷰에서 갈아탈 탈출구.
    WebOpenExternal,
    /// 웹 pane 헤더의 주소 pill — 클릭하면 인라인 주소 편집(`App.web_addr`)로
    /// 들어간다. Enter 로 그 주소를 연다.
    WebAddress,
}

/// 타이틀바 사용량 pill 드롭다운의 한 줄.
#[derive(Clone, PartialEq, Eq)]
pub(crate) enum AccountMenuItem {
    /// 로스터 한 행 = 제공자 하나. 누르면 그 제공자의 계정 목록이 옆으로 열린다.
    ///
    /// 계정을 첫 화면에 죽 늘어놓지 않는 건 Orca 하단바를 그대로 따른 것이다(사용자
    /// 2026-08-12 「똑같이 하라니까」). 첫 화면이 답하는 질문은 «어느 쪽이 얼마나
    /// 찼나» 고, «누구로 바꿀까» 는 그 다음이다 — 제공자가 늘수록 이 차이가 커진다.
    Provider(AccountProvider),
    /// 서브메뉴 안의 계정 행. 빈 문자열 = 기본 로그인(env 를 아예 안 붙임).
    Select(AccountProvider, String),
    /// 계정 행의 곁 단추 둘 — 다시 로그인, 목록에서 빼기. 설정 화면에만 있던
    /// 것을 여기에도 둔다(2026-09-07 「하단바랑 설정이랑 완전 똑같이 떠야해」):
    /// 로그인이 풀린 것을 **여기서** 보게 됐으니 고치는 것도 여기여야 한다.
    Reauth(AccountProvider, String),
    Forget(AccountProvider, String),
    /// 로스터 하단 액션 둘. Orca 와 같은 자리·같은 순서.
    UsageDetails,
    ManageAccounts,
    /// 표시 밀도 — `true` = Compact(가장 빡빡한 창 하나만).
    Density(bool),
    /// claude 블록의 초기화권 줄 곁 단추 — claude.ai 사용량 페이지를 연다.
    UseLimitReset,
    /// 바닥의 판 번호 줄 — 이 기기의 업데이터로 최신 판을 확인·받는다(`version::update_entry`).
    CheckUpdates,
}

/// 계정을 가진 하네스. 저장소도 전환 수단도 갈려서(claude 는 자격 저장소 env,
/// codex 는 auth.json 링크) 타입으로 못 박아 둔다.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AccountProvider {
    Claude,
    Codex,
}

impl AccountProvider {
    /// 로스터에 적는 이름과 그 옆 아이콘 에셋 이름.
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Claude => "Claude",
            Self::Codex => "Codex",
        }
    }
    pub(crate) fn icon(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
        }
    }
}

/// State for an in-flight in-pane tab reorder drag. A press on a tab arms
/// this; it only becomes a real drag past the threshold, so a plain press
/// just switches the active tab on release.
struct TabDrag {
    /// Pane whose tab strip owns the drag.
    pane: String,
    /// Index of the grabbed tab at press time.
    from: usize,
    /// Press position in logical px, for the click→drag threshold.
    start: (f32, f32),
    /// True once the cursor moved past the threshold.
    active: bool,
    /// Current insertion index (0..=tabs.len()) the tab would drop into.
    target: usize,
    /// Pane the cursor is currently hovering. Equals `pane` for in-place
    /// reorder; differs when the user dragged onto another pane's tab strip
    /// — release commits a cross-pane move into `drop_pane.tabs[target]`.
    drop_pane: String,
}

/// 닫은 pane 하나 — ⌘⇧T 로 되살릴 대상.
///
/// **닫아도 프로세스는 죽지 않는다.** 사용자가 닫은 pane 은 화면(BSP 트리)에서만
/// 빠지고 PTY 는 계속 돌아, 되살리기가 "다시 붙이기"가 된다 — claude 가 하던 일을
/// 이어서 하고 있으므로 `--resume` 으로 대화를 되감을 이유가 없다(사용자: 데몬처럼
/// 계속 돌기를 원함). 진짜로 끄고 싶으면 인포 줄의 × 다.
///
/// 다만 죽은 채 목록에 남는 경우도 있다 — 셸이 스스로 exit 한 pane, 그리고 앱을
/// 껐다 켜 프로세스가 사라진 뒤의 복원분. 그때는 세션 복원과 **똑같은 레코드**
/// (cwd·claude 세션 id·캐릭터·스크롤백)를 들고 있으므로 `restore_leaf` 재사용으로
/// 새로 띄우고 `--resume` 까지 그 함수가 처리한다. `alive` 가 그 두 길을 가른다.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum InlineWebKind {
    Arona,
}

pub(crate) struct InlineWebHost {
    pub(crate) webview: wry::WebView,
    pub(crate) window: Option<Arc<Window>>,
    pub(crate) kind: InlineWebKind,
    pub(crate) visible: bool,
    pub(crate) restore_sidebar: bool,
}

pub(crate) struct ClosedPane {
    /// `restore_leaf` 가 먹는 레코드(`layout_to_json` 의 leaf 본문).
    pub(crate) rec: serde_json::Value,
    /// 닫힌 pane 의 원래 id — 인포 줄에 그대로 뜬다(`%3`).
    pub(crate) pane_id: String,
    /// 학생 이름. 없으면 빈 문자열이고 인포는 셸 이름 자리를 비운다.
    pub(crate) character: String,
    /// 작업 폴더의 끝 조각 — 어느 일감이었는지 가르는 실제 단서다(경로 전체는 좁은
    /// 칼럼에서 앞부분만 남고 정작 구분되는 꼬리가 잘린다).
    pub(crate) folder: String,
    /// 되살릴 때 이 pane 옆에 꽂는다. 그 사이 사라졌으면 활성 pane 옆.
    pub(crate) neighbor: Option<String>,
    /// 닫힌 방. 아직 있으면 그 방으로 돌아간다. 방 재배치를 따라 remap 된다 —
    /// 안 그러면 되살린 pane 이 남의 방에서 튀어나온다.
    pub(crate) window: usize,
    /// PTY 가 아직 도는가. 참이면 되살리기는 트리에 leaf 를 다시 꽂는 것뿐이고
    /// (`pane_id` 가 그대로 유효하다), 거짓이면 `rec` 로 새로 띄운다.
    pub(crate) alive: bool,
    /// 사이드바 「숨기기」로 치운 것인가. **참이면 두 정리 루프가 건너뛴다** —
    /// 개수 상한(`CLOSED_PANE_KEEP`)도 idle reap(`CLOSED_PANE_IDLE_REAP`)도.
    ///
    /// 닫기와 갈라 두는 이유: 숨기기는 작업이 도는 중에 화면에서만 치우는 것이라
    /// 돌아왔을 때 대화가 끊겨 있으면 쓸모가 없다(2026-08-11 지시). 대신 숨긴 만큼
    /// 메모리를 계속 문다 — 그건 사용자가 고른 값이다.
    pub(crate) stashed: bool,
    /// Ordinary close began here. Activity never extends the undo grace.
    pub(crate) idle_since: Option<Instant>,
    /// 이미지·마크다운 미리보기 **탭**이었나 — `(붙어 있던 pane, 파일 경로)`.
    /// 참이면 되살리기는 `restore_leaf` 가 아니라 `open_file` 재호출이고 `rec` 는
    /// Null 이다. pane 닫기와 같은 스택을 쓰므로 ⌘⇧T 의 「가장 최근에 닫은 것」
    /// 순서에 미리보기 탭도 함께 선다.
    pub(crate) preview: Option<(String, std::path::PathBuf)>,
}

/// 닫은 pane 을 몇 개까지 들고 있을지. 레코드마다 스크롤백이 통째 붙어 있어
/// 무한히 쌓으면 닫기만 반복해도 메모리가 는다.
const CLOSED_PANE_KEEP: usize = 10;

/// `to ..`(옛 `book`)가 뱉는 예약 알림 title(OSC 777 `notify`). 이 값이 오면 데스크톱 알림
/// 대신 「원격 pane 을 로컬로 되돌리기」로 해석한다. 셰임 생성(`write_shim`)과
/// 화면 펌프(`pump_pty_screens`)가 같은 문자열을 써야 하므로 한 곳에 둔다.
pub(crate) const BRING_HOME_MARKER: &str = "kasaterm-home";

/// Ordinary close gets ten seconds to undo, regardless of ongoing activity.
/// Explicit stash remains a separate background-execution action.
const CLOSED_PANE_IDLE_REAP: std::time::Duration = std::time::Duration::from_secs(10);

/// Isolated tests may shorten the grace. Invalid/zero values use ten seconds.
fn closed_pane_idle_reap() -> std::time::Duration {
    std::env::var("KASATERM_CLOSED_GRACE_SECS")
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .filter(|s| *s > 0)
        .map(std::time::Duration::from_secs)
        .unwrap_or(CLOSED_PANE_IDLE_REAP)
}

/// State for an in-flight window-tab (방) reorder drag in the sidebar / top
/// strip. Unlike `TabDrag` the press already switched to the tab — a window
/// switch is cheap and browser tabs behave that way — so a press that never
/// passes the threshold leaves nothing for the release to undo.
/// 사이드바에서 **pane 줄 하나**를 끌고 있는 상태.
///
/// 방 탭(`WinTabDrag`)과 갈리는 건 옮기는 것이 방이 아니라 그 안의 pane 이라는
/// 점이다. 목록이 방 경계를 넘어 이어져 있으므로, 남의 방 줄 위에 떨어뜨리면 그게
/// 곧 방 옮기기가 된다 — 따로 "다른 방으로" 명령을 두지 않아도 된다.
struct SidebarRowDrag {
    /// 잡은 pane.
    pane: String,
    /// 누른 자리(논리 px) — 클릭과 드래그를 가르는 문턱용.
    start: (f32, f32),
    /// 문턱을 넘었나.
    active: bool,
    /// 떨어질 자리 — `(기준 pane, 어느 모서리에)`. 목록 줄은 Up/Down 만, 배치도
    /// 칸은 네 모서리가 다 나온다(Center 는 안 쓴다). 아무 줄 위도 아니면 None.
    target: Option<(String, DropZone)>,
    /// 다른 기기의 카드 위 — 놓으면 그 기기로 **이사**(2026-09-18 지시). `target` 과 배타.
    machine: Option<String>,
}

struct WinTabDrag {
    /// Window index grabbed at press time.
    from: usize,
    /// Press position in logical px, for the click→drag threshold.
    start: (f32, f32),
    /// True once the cursor moved past the threshold.
    active: bool,
    /// Insertion slot (0..=windows.len()) the tab would drop into.
    target: usize,
}

/// In-flight file-tree → terminal drag. Armed on a press over a tree row;
/// once the cursor moves past the threshold (`active`), releasing over a
/// terminal pane types that file's path into the dropped-on shell. A press
/// that never moves stays a plain click (the row's expand/preview already
/// fired on press), so this is take-and-ignore on release.
struct FileTreeDrag {
    /// Absolute path of the grabbed row.
    path: std::path::PathBuf,
    /// Press position in logical px, for the click→drag threshold.
    start: (f32, f32),
    /// True once the cursor moved past the threshold.
    active: bool,
}

/// A terminal pane's screen state: the PTY-backed cell grid, cursor,
/// scrollback, and the modes the emulator reports. Lives inside
/// `PaneContent::Terminal`.
#[derive(Default)]
struct TerminalPane {
    live_output: bool,
    output_generation: u64,
    rows: u16,
    cols: u16,
    cells: Vec<Vec<GridCell>>,
    cursor_row: u16,
    cursor_col: u16,
    cursor_visible: bool,
    alt_screen: bool,
    mouse_enabled: bool,
    mouse_sgr: bool,
    mouse_motion: bool,
    /// DECCKM (application cursor keys). When true, plain arrows go out
    /// as SS3 (`ESC O A`) instead of CSI (`ESC [ A`) so claude code /
    /// vim line navigation works. See the arrow-key send path.
    app_cursor: bool,
    /// DECSET 2004 (bracketed paste). Only when the app turned this on may a
    /// paste be wrapped in `ESC[200~ … ESC[201~` — otherwise those bytes land
    /// in the app's input buffer as if typed.
    bracketed_paste: bool,
    history: VecDeque<Vec<GridCell>>,
    /// Scrollback offset in rows. `0` = live tail; positive = N rows
    /// back into history visible at the top.
    scroll_offset: usize,
    /// Cached previous cells used by the shift-detection heuristic that
    /// promotes scrolled-off rows into `history`.
    prev_cells: Vec<Vec<GridCell>>,
    /// Last OSC 133 `B` mark (prompt end / command-input start), (row, col).
    prompt_end: Option<(u16, u16)>,
    /// 이번 프레임에 뷰포트와 겹치는 인라인 이미지(OSC 1337)들 — PTY 쪽이 절대
    /// 줄 앵커를 화면 좌표로 환산해 보낸 그대로. 렌더는 이 좌표에 그리기만 한다.
    inline_images: Vec<kasa_bridge::screen::InlineImageView>,
}

/// A markdown pane's state: the parsed doc plus the Raw editor buffer/cursor.
struct MarkdownPane {
    doc: Arc<MarkdownDoc>,
    saved_text: String,
    /// true only for actual `.md`/`.markdown` files. Code/text files reuse this
    /// pane as a plain Raw editor but get no "Rendered | Raw" header toggle —
    /// rendering a `.toml` as markdown would just mangle it.
    is_md_doc: bool,
    /// false = Render (laid-out view), true = Raw (wgpu text editor).
    raw_mode: bool,
    /// Raw-mode edit buffer, one entry per line.
    ///
    /// Shared, not owned: the renderer takes a copy every frame (it reads under
    /// the ws lock and draws after releasing it) and every undo snapshot takes
    /// another. As a plain `Vec` that was a full deep copy of the file each
    /// time — thousands of `String` allocations per frame on a big document.
    /// Behind an `Arc` those are pointer bumps, and `Arc::make_mut` (see
    /// `lines_mut`) pays for exactly one real copy per edit run: the first
    /// keystroke after a snapshot, which is the copy `push_undo` was making
    /// anyway.
    edit_lines: Arc<Vec<String>>,
    /// Edit cursor: line index + column in chars.
    cur_line: usize,
    cur_col: usize,
    /// Scroll offset in logical px (both Render and Raw). f32, not an integer:
    /// a trackpad frame carries sub-pixel tails, and rounding each one away
    /// made a slow swipe stall instead of glide.
    scroll: f32,
    /// Raw-mode horizontal scroll in logical px. Long code lines (checksums,
    /// URLs) overflow the pane — this pans the text under a fixed line-number
    /// gutter. 0 = flush left. Render mode ignores it (markdown wraps).
    h_scroll: f32,
    /// Buffer has edits not yet written to `doc.path`. Cleared by Cmd+S and
    /// the .md Raw→Render save; drives the ● unsaved dot on the tab label.
    modified: bool,
    /// When the buffer was last touched. Autosave waits for this to go quiet
    /// (VS Code's afterDelay), so a typing run writes once at the end instead
    /// of once per keystroke. Set with `touch`, cleared on save.
    edited_at: Option<Instant>,
    /// Selection anchor as (line, col) in chars; the cursor is the head, so
    /// anchor..cursor is the selection either direction. None = no selection.
    sel_anchor: Option<(usize, usize)>,
    /// Undo/redo: whole-buffer snapshots pushed at edit-run boundaries (start
    /// of a typing run / delete run, Enter, paste, selection replace). Whole
    /// snapshots are fine at this editor's file sizes and can't drift the way
    /// operation logs do. Capped at `markdown::UNDO_CAP`.
    undo_stack: Vec<EditSnapshot>,
    redo_stack: Vec<EditSnapshot>,
    /// Kind of the last mutation — consecutive same-kind edits (a typing run,
    /// a backspace run) coalesce into one undo unit; any caret move breaks it.
    last_edit: EditKind,
    /// Find/replace bar. Some == open, and while it is open it owns typing
    /// (Esc closes and hands the keyboard back to the buffer).
    find: Option<FindState>,
    /// 자동완성 팝업. Some == 열림, 그동안 ↑↓·Tab·Enter·Esc 를 팝업이 먼저 먹는다.
    complete: Option<CompleteState>,
    /// 가장 긴 줄의 칸 수 — 가로 스크롤 상한. `None` 이면 다음에 물어볼 때 다시
    /// 센다. 트랙패드 제스처는 프레임마다 상한을 묻는데, 캐시가 없으면 그때마다
    /// 버퍼 전체를 훑었다.
    longest_cache: Option<usize>,
    /// 긴 줄을 본문 폭에서 접어 내릴지. 끄면 가로로 스크롤해 본다.
    wrap: bool,
    /// 보조 커서들 — 주 커서(`cur_line`/`cur_col`/`sel_anchor`)와 같은 편집을 함께
    /// 받는다. 비어 있는 게 보통이고, 그때는 지금까지와 완전히 같은 단일 커서
    /// 편집기라 비용이 0 이다.
    extra: Vec<crate::markdown::Caret>,
    /// 멀티커서 편집이 도는 동안 undo 스냅샷을 막는다 — 커서 N 개의 한 번 편집이
    /// undo 도 한 번이라야 한다.
    undo_locked: bool,
    /// 접힌 구간들 — `(머리 줄, 마지막 숨은 줄)`. 비어 있는 게 보통이고, 그때
    /// 화면 행 ↔ 버퍼 줄 변환은 전부 항등이라 비용이 0 이다.
    folds: crate::markdown::Folds,
    /// `folds` 를 마지막으로 검증한 버퍼 세대. 편집으로 블록 모양이 바뀌면
    /// 접힘이 엉뚱한 줄을 가리키므로 세대가 다를 때 한 번 훑어 걷어낸다.
    folds_gen: u64,
    /// 버퍼가 바뀔 때마다 오르는 세대 번호. LSP 재전송 디바운스가 "달라졌나"를
    /// 이 정수 하나로 판정한다 — 매 틱 5천 줄을 이어 붙여 해시하면 그 자체가
    /// 프레임 예산이고, 저장 플래그(`dirty`)는 Cmd+S 에 꺼져서 못 쓴다.
    edit_gen: u64,
    /// HEAD 대비 변경 표시(거터의 색 바). `None` = 그릴 게 없다 — 레포 밖 파일,
    /// 미추적 파일, 아직 한 번도 안 뜬 상태가 전부 여기다. 접힘·보조커서와 같은
    /// 규율으로, 비어 있으면 거터 루프 비용이 정확히 0 이다.
    ///
    /// 이 값은 pane 상태와 함께 움직여 그리드를 재배치해도 같은 표시를 유지한다.
    diff: Option<crate::gitdiff::BufferDiff>,
    /// 거터 바를 눌러 펼친 헝크가 덮는 버퍼 줄. 패널은 그 줄 아래에 뜬다.
    /// 편집으로 줄이 밀리면 다음 diff 갱신 때 자리를 잃으므로 그때 닫는다.
    diff_peek: Option<usize>,
    /// diff 의 왼쪽 — HEAD 시점의 이 파일 본문. `git show` 는 프로세스를 하나
    /// 띄우므로 타자마다 못 부른다. 여기 붙들어 두고 커밋·스테이지로 HEAD 가
    /// 움직였을 때만 비운다(`invalidate_git_diffs`).
    diff_head: Option<crate::gitdiff::HeadText>,
}

/// 자동완성 팝업 상태.
///
/// 후보는 버퍼 안 낱말에서 만든다(`markdown::word_completions`). LSP 가 붙은
/// 뒤에도 이 목록은 남는다 — 서버 응답은 왕복이 있어서, 그 사이 한 프레임을
/// 이걸로 메워야 타이핑이 멈춘 것처럼 보이지 않는다.
struct CompleteState {
    /// 후보 목록. 캐럿에서 가까운 줄이 앞.
    items: Vec<String>,
    /// 고른 후보.
    sel: usize,
    /// 채워 넣을 낱말이 시작하는 열. 확정할 때 여기부터 캐럿까지를 후보로
    /// 갈아끼운다 — 캐럿 위치만 들고 있으면 이미 친 앞부분이 남아 `cocost`
    /// 처럼 겹쳐 들어간다.
    from_col: usize,
    /// 서버에 보낸 자동완성 요청 id. 응답은 비동기로 오므로, 도착한 것이 **이
    /// 팝업의** 답인지 이걸로 가린다. `None` = 버퍼 낱말만으로 채운 상태.
    lsp_req: Option<i64>,
}

/// 호버 툴팁 상태. 마우스가 편집기 위에서 **멎으면** 서버에 묻고, 답이 오면 그
/// 자리에 띄운다. 움직이는 동안 묻지 않는 이유는 지나가는 글자마다 요청을 쏘면
/// 서버가 취소·재시작만 반복하기 때문이다.
struct HoverState {
    /// 마우스가 멎은 화면 좌표(logical px).
    at: (f32, f32),
    /// 그 자리에 멎은 시각.
    since: std::time::Instant,
    /// 보낸 요청 id. `Some` 이면 답을 기다리는 중 — 같은 자리를 다시 묻지 않는다.
    req: Option<i64>,
    /// 서버가 준 글. 있으면 툴팁을 그린다.
    text: Option<String>,
}

/// 커서가 `[Image #N]` 위에 멎고부터 썸네일을 찾아 나서기까지. 사람이 "이게
/// 뭐지" 하고 멈추는 시간 — 지나가는 커서마다 그림이 튀면 화면이 소란스럽고,
/// 여기서 transcript 를 수 MB 읽으므로 스쳐 가는 셀마다 읽을 일도 아니다.
const IMAGE_TIP_DELAY: std::time::Duration = std::time::Duration::from_millis(300);

/// `[Image #N]` 썸네일 툴팁의 상태.
///
/// claude pane 은 붙인 그림을 그 글자로만 남겨서, 무슨 그림이었는지 화면만 봐서는
/// 알 수가 없다. 커서가 그 위에 멎으면 원본을 transcript 에서 되찾아 띄운다.
struct ImageTip {
    /// 커서가 멎은 참조 — (pane id, `#` 뒤 번호).
    at: (String, u32),
    /// 멎은 시각. 지나가다 툴팁이 튀지 않게 잠깐 기다렸다 읽는다.
    since: std::time::Instant,
    /// 디코드한 썸네일(RGBA, 픽셀 폭·높이). 업로드가 첫 프레임에만 필요해
    /// `Arc` 로 들고 그 뒤 프레임은 참조만 복사한다.
    thumb: Option<(Arc<Vec<u8>>, u32, u32)>,
    /// transcript 를 이미 뒤졌나. 못 찾은 참조(아직 제출 안 한 프롬프트 등)를
    /// 매 프레임 다시 뒤지면 커서가 멎어 있는 동안 내내 수 MB 를 읽는다.
    looked: bool,
}

/// Clickable control on the find bar. Every one has a keyboard equivalent —
/// the mouse is for the hand that's already there, not the only way in.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum FindBtn {
    /// Expand/collapse the replace row.
    ToggleReplace,
    Prev,
    Next,
    Close,
    ReplaceOne,
    ReplaceAll,
}

/// Find/replace bar state for one raw editor pane.
#[derive(Clone)]
struct FindState {
    query: String,
    replace: String,
    /// The replace row is showing (Cmd+Opt+F). Find-only otherwise.
    replacing: bool,
    /// Typing lands in the replace field rather than the query; Tab flips it.
    focus_replace: bool,
    /// Matches as (line, start col, end col) in chars, document order. Rebuilt
    /// when the query or buffer changes — the bar owns the keyboard, so the
    /// buffer only moves under it via replace.
    hits: Vec<(usize, usize, usize)>,
    /// Index of the highlighted match in `hits`; meaningless when empty.
    idx: usize,
}

/// One undo/redo unit for the raw editor: the full buffer + cursor.
#[derive(Clone)]
struct EditSnapshot {
    lines: Arc<Vec<String>>,
    /// Primary and secondary carets, including each selection anchor. Keeping
    /// only the primary cursor left stale selections behind after undo.
    carets: Vec<crate::markdown::Caret>,
}

/// Coalescing class for `MarkdownPane::last_edit`. `Break` = no run in
/// progress (cursor moved, undo happened, pane just opened).
#[derive(Clone, Copy, PartialEq, Debug)]
enum EditKind {
    Break,
    Typing,
    Deleting,
    Other,
}

/// What a pane shows. Terminal panes drive a PTY + cell grid; image/markdown
/// panes are PTY-less and rendered directly with wgpu. Keeping these in an enum
/// stops terminal-assuming code (cursor, scrollback, PTY resize) from silently
/// touching non-terminal panes — the compiler forces a match. A future
/// `Browser(BrowserPane)` (webview overlay) slots in here too.
enum PaneContent {
    Terminal(TerminalPane),
    Image(Arc<ImagePane>),
    Markdown(MarkdownPane),
    Web(WebPane),
    /// PTY 없는 네이티브 설정 방. 화면 상태는 `App.settings_scene`가 소유한다.
    Settings,
}

/// 웹(브라우저) pane 의 그리드 쪽 상태 — 주소뿐이다. 실제 브라우저는 이 pane
/// 사각형 위에 접착된 자식 OS 창(`App.web_hosts`, GUI 스레드 전용)이다:
/// wgpu 본창 안에는 WKWebView 를 겹칠 수 없어(CAMetalLayer 가 자식 뷰를 덮음,
/// 2026-05-25 실측) 셀 그리드 자리는 자식 창이 가리는 바탕일 뿐이다.
/// `Workspace` 는 PTY 스레드와 공유(Send 필요)라 webview 실물을 여기 못 둔다.
struct WebPane {
    url: String,
    /// `App.web_hosts` 의 키. pane/tab 이 트리를 옮겨 다녀도 이 번호로 창을
    /// 따라붙인다.
    host_id: u64,
}

/// 웹 pane 헤더 주소창의 인라인 편집 상태(`App.web_addr`). 버퍼는 **빈 채로**
/// 시작한다 — lineedit 엔 선택이 없어 기존 주소를 지우고 다시 치게 하는 것보다,
/// 현재 주소를 흐린 자리표시자로 깔고 새 주소를 바로 치게 하는 편이 빠르다
/// (빈 채 Enter = 그대로 두기).
struct WebAddrEdit {
    /// 편집 대상 pane(outer id).
    pane: String,
    text: String,
    /// 문자 단위 커서(lineedit 규약).
    cursor: usize,
}

impl Default for PaneContent {
    fn default() -> Self {
        PaneContent::Terminal(TerminalPane::default())
    }
}

/// One tab inside a pane. Stage 3: each tab carries its own content + PTY pid +
/// title — switching tabs swaps which one drives the pane's header label,
/// terminal grid, and input routing. `pid` is the key into `App.pty`; `None`
/// for image / markdown tabs that have no shell behind them.
#[derive(Default)]
struct PaneTab {
    content: PaneContent,
    server: Option<server_restore::RegisteredServer>,
    /// OSC 0/2 title — `printf '\e]0;hello\a'` from this tab's shell, or a
    /// pinned label. Falls back to the live process name in the header paint
    /// when None.
    title: Option<String>,
    /// Sticky-title flag (set by `surface.rename`): while pinned, OSC titles
    /// from the inner program are ignored.
    title_pinned: bool,
    /// Image-pane view state. `image_zoom < 1.0` clamps to fit; >= 1 zooms
    /// past native (image overflows the box, centered). `image_rot` is the
    /// number of 90° CW rotations applied (0..4). Ignored for non-image tabs.
    image_zoom: f32,
    image_rot: u8,
    /// Pan offset (logical px) of the zoomed image's center from the pane
    /// center, set by dragging the image body. Only has an effect while the
    /// image overflows its box (zoomed in); `queue_image` clamps it so the
    /// crop window never leaves the texture. 0,0 = centered.
    image_pan_x: f32,
    image_pan_y: f32,
    /// PtySession key in `App.pty` for terminal tabs. `None` for image /
    /// markdown tabs. The outer pane id (the layout-tree key) equals the
    /// pid of the *first* tab; secondary tabs have their own pid and a
    /// reverse-map entry in `Workspace.pid_to_pane`.
    pid: Option<String>,
    /// Daemon preview id when this tab is a host-attached image/markdown
    /// preview (`imgopen` inside this pane). The GUI reconciles these tabs
    /// against `view.pane_previews` each broadcast; closing the tab fires
    /// `close_surface(preview_id)` so the daemon drops it (else it resurrects).
    /// `None` for normal terminal / leaf tabs.
    preview_id: Option<String>,
    /// Source file path for a locally-opened image/markdown preview (file-tree
    /// double-click). Markdown also carries its path in `doc.path`, but image
    /// tabs have nowhere else to keep it; this single field lets the file-tree
    /// highlight + de-dup logic ask "which file is this tab showing" uniformly.
    preview_path: Option<std::path::PathBuf>,
}

impl PaneTab {
    fn term(&self) -> Option<&TerminalPane> {
        if let PaneContent::Terminal(t) = &self.content {
            Some(t)
        } else {
            None
        }
    }
    fn term_mut(&mut self) -> Option<&mut TerminalPane> {
        if let PaneContent::Terminal(t) = &mut self.content {
            Some(t)
        } else {
            None
        }
    }
    /// The visible screen as text — the last `lines` non-blank rows of the
    /// current grid (0 = all). Trailing blank rows are dropped so a peek of a
    /// half-empty screen doesn't return a wall of whitespace. Backs
    /// `surface.peek` / the board's `screen_lines` so a sibling can read what
    /// this pane is currently showing (a prompt, a menu, build output).
    pub(crate) fn visible_text(&self, lines: usize) -> String {
        self.screen_text(lines, false)
    }

    /// `surface.peek` 이 주는 화면 — `visible_text` 에 입력칸의 흐린 글 표시만 더한다. Claude Code 는 빈
    /// 입력칸에 다음에 칠 만한 말을 흐린 글씨(SGR 2)로 띄우는데, 색이 빠진 글자만으로는 사람이 친 글과 안
    /// 갈려 에이전트가 그걸 선생님 말로 읽었다(2026-10-09). 보드 `screen_lines` 처럼 화면을 판정하는 길이
    /// 표시 글자를 입력으로 오해하지 않게 `visible_text` 와 따로 둔다.
    pub(crate) fn peek_text(&self, lines: usize) -> String {
        self.screen_text(lines, true)
    }

    fn screen_text(&self, lines: usize, mark_ghost: bool) -> String {
        let Some(t) = self.term() else {
            return String::new();
        };
        let mut out: Vec<String> = Vec::new();
        for row in t.cells.iter() {
            let mut s = String::new();
            append_cells_text(row.iter(), &mut s);
            let mut s = s.trim_end().to_string();
            if mark_ghost {
                let cells: Vec<(char, bool)> = row.iter().filter(|c| !c.leading_wide_spacer).map(|c| (c.ch, c.dim)).collect();
                if let Some(note) = ghost_note(&cells) {
                    s.push_str(note);
                }
            }
            out.push(s);
        }
        while out.last().map_or(false, |l| l.is_empty()) {
            out.pop();
        }
        if lines > 0 && out.len() > lines {
            out.drain(0..out.len() - lines);
        }
        out.join("\n")
    }
    /// Same as `visible_text` but serializes cell fg/bg/attributes as ANSI SGR
    /// escape codes so a viewer can reproduce the terminal colors. Each row is
    /// trimmed to the last non-blank cell before encoding; trailing blank rows
    /// are dropped. Used by `GET /peek?ansi=1`.
    pub(crate) fn visible_text_ansi(&self, lines: usize) -> String {
        use kasa_bridge::screen::Color;
        let Some(t) = self.term() else {
            return String::new();
        };

        fn render_row(row: &[GridCell]) -> String {
            let last_vis = row
                .iter()
                .rposition(|c| c.ch != ' ' && c.ch != '\0')
                .map_or(0, |i| i + 1);
            if last_vis == 0 {
                return String::new();
            }
            let row = &row[..last_vis];
            let mut s = String::new();
            let mut any_attr = false;
            let mut p_fg = Color::Default;
            let mut p_bg = Color::Default;
            let mut p_bold = false;
            let mut p_italic = false;
            let mut p_under = false;
            let mut p_dim = false;
            let mut p_inv = false;
            for cell in row {
                let ch = if cell.ch == '\0' { ' ' } else { cell.ch };
                if cell.fg != p_fg
                    || cell.bg != p_bg
                    || cell.bold != p_bold
                    || cell.italic != p_italic
                    || cell.underline != p_under
                    || cell.dim != p_dim
                    || cell.inverse != p_inv
                {
                    s.push_str("\x1b[0m");
                    if cell.bold {
                        s.push_str("\x1b[1m");
                    }
                    if cell.dim {
                        s.push_str("\x1b[2m");
                    }
                    if cell.italic {
                        s.push_str("\x1b[3m");
                    }
                    if cell.underline {
                        s.push_str("\x1b[4m");
                    }
                    if cell.inverse {
                        s.push_str("\x1b[7m");
                    }
                    match &cell.fg {
                        Color::Default => {}
                        Color::Idx(n) => s.push_str(&format!("\x1b[38;5;{n}m")),
                        Color::Rgb(r, g, b) => s.push_str(&format!("\x1b[38;2;{r};{g};{b}m")),
                    }
                    match &cell.bg {
                        Color::Default => {}
                        Color::Idx(n) => s.push_str(&format!("\x1b[48;5;{n}m")),
                        Color::Rgb(r, g, b) => s.push_str(&format!("\x1b[48;2;{r};{g};{b}m")),
                    }
                    p_fg = cell.fg.clone();
                    p_bg = cell.bg.clone();
                    p_bold = cell.bold;
                    p_italic = cell.italic;
                    p_under = cell.underline;
                    p_dim = cell.dim;
                    p_inv = cell.inverse;
                    any_attr = p_fg != Color::Default
                        || p_bg != Color::Default
                        || p_bold
                        || p_italic
                        || p_under
                        || p_dim
                        || p_inv;
                }
                s.push(ch);
            }
            if any_attr {
                s.push_str("\x1b[0m");
            }
            s
        }

        let mut rows: Vec<String> = t.cells.iter().map(|r| render_row(r)).collect();
        while rows.last().map_or(false, |l| l.is_empty()) {
            rows.pop();
        }
        if lines > 0 && rows.len() > lines {
            rows.drain(0..rows.len() - lines);
        }
        rows.join("\n")
    }

    fn markdown(&self) -> Option<&MarkdownPane> {
        if let PaneContent::Markdown(m) = &self.content {
            Some(m)
        } else {
            None
        }
    }
    fn markdown_mut(&mut self) -> Option<&mut MarkdownPane> {
        if let PaneContent::Markdown(m) = &mut self.content {
            Some(m)
        } else {
            None
        }
    }
    fn image_view_zoom(&self) -> f32 {
        if self.image_zoom < 1.0 {
            1.0
        } else {
            self.image_zoom
        }
    }
    fn image(&self) -> Option<&Arc<ImagePane>> {
        if let PaneContent::Image(i) = &self.content {
            Some(i)
        } else {
            None
        }
    }
    fn web(&self) -> Option<&WebPane> {
        if let PaneContent::Web(w) = &self.content {
            Some(w)
        } else {
            None
        }
    }
}

struct PaneState {
    /// In-pane tabs. Always non-empty — single-tab panes have `tabs.len() == 1`.
    /// `active_tab` indexes the visually-active tab; its state is exposed via
    /// the `Deref`/`DerefMut` impls so the rest of the code can keep writing
    /// `pane.title`, `pane.content`, `pane.term()` as if the pane were single.
    tabs: Vec<PaneTab>,
    /// Index into `tabs` of the visually-active tab.
    active_tab: usize,
    /// Accent color for this pane's header band (RGBA). None = default.
    /// Pane-level, not per-tab — the band is shared above all tabs.
    color: Option<[u8; 4]>,
    /// Tab-strip overflow windowing: index of the first tab drawn in this
    /// pane's header (whole-tab run, no partial clipping). Stepped by the
    /// wheel; the render pass clamps and writes the effective value back.
    tab_first: usize,
    /// How many tabs the strip fit last frame — render-written, wheel-read.
    tab_vis: usize,
    /// `active_tab` as of the last frame. The render pass compares to spot
    /// a tab switch (from any of its many call sites) and auto-reveals the
    /// newly active tab, without touching free wheel scrolling.
    tab_last_active: usize,
    /// 배정된 캐릭터명(미도리 등). 진실 소스는 Workspace.pane_character,
    /// apply_screen_update 가 매 업데이트 동기. 타이틀바(render)가 claude 실행 중
    /// (agents --json)일 때만 이 이름을 그린다 — 작업 중 표시는 pane_activity 로딩바.
    character: Option<String>,
    /// Frame-dirty flag; cleared after the next render. When every pane is
    /// clean and no chrome anim is pending, the render loop skips the GPU pass.
    dirty: bool,
    /// ⋮ 메뉴의 상단바 토글. `None` = 자동(탭 여러 개·이미지·md 면 켬), `Some(b)` =
    /// 사용자가 직접 정함. 하단바처럼 App 쪽 HashSet 으로 두지 않고 pane 에 붙인
    /// 이유는 `has_header()`/`header_px()` 가 이미 pane 만 보고 답하기 때문이다 —
    /// 여기 두면 렌더·히트테스트·resize 의 모든 기존 호출부가 그대로 맞는다.
    /// 헤더 유무는 셀 그리드를 밀므로 세 곳이 어긋나면 클릭이 행 하나씩 밀린다.
    header_override: Option<bool>,
}

impl Default for PaneState {
    fn default() -> Self {
        Self {
            tabs: vec![PaneTab::default()],
            active_tab: 0,
            color: None,
            tab_first: 0,
            tab_vis: usize::MAX,
            tab_last_active: 0,
            character: None,
            dirty: false,
            header_override: None,
        }
    }
}

impl PaneState {
    /// 탭이 둘 이상이면 탭 스트립을 위해, 이미지·마크다운(.md)이면 전용 컨트롤을
    /// 위해 헤더 띠를 유지한다. 그 외 일반 터미널은 hover ⋮ 만 쓴다. image()/
    /// markdown()은 Deref로 active 탭을 본다.
    fn has_header(&self) -> bool {
        // 학생 헤더 띠 폐기(사용자) — 학생 이름은 상단 타이틀바(claude 실행 시),
        // 로딩바는 pane 위 별도. 헤더 띠는 멀티탭·이미지·md 전용 컨트롤만 남긴다.
        // 코드/텍스트 raw 편집기도 헤더를 가진다 — 파일명 + ● 미저장 도트의 자리.
        // ⋮ 에서 직접 정했으면 그게 우선 — 탭이 하나인 터미널에도 띠를 띄워
        // 제목을 쓸 수 있고, md 처럼 자동으로 뜨는 pane 은 접을 수 있다.
        if let Some(forced) = self.header_override {
            return forced;
        }
        if PANE_HEADER_BAR.load(std::sync::atomic::Ordering::Relaxed) {
            return true;
        }
        self.tabs.len() > 1
            || self.image().is_some()
            || self.markdown().is_some()
            || self.web().is_some()
    }
    /// 셀 그리드를 아래로 미는 헤더 높이(logical px). render/layout 양쪽이
    /// 같은 값을 써야 PTY 그리드↔셀 클립이 어긋나지 않는다.
    fn header_px(&self) -> f32 {
        if self.has_header() {
            PANE_HEADER_HEIGHT
        } else {
            0.0
        }
    }
    /// `pid` 를 가진 탭. 없으면 활성 탭(= `Deref` 가 주는 것).
    ///
    /// **탭을 지목한 조회가 지나야 하는 유일한 문이다.** `PaneState` 는 `Deref` 로
    /// 활성 탭을 가리키므로, 탭 pid 를 outer pane 으로 접은 뒤 그냥 읽으면 **앞 탭
    /// 화면이 돌아온다** — 실패가 아니라 조용히 틀린 답이라 부른 쪽이 알 수가 없다
    /// (아루 실측 2026-08-07: `peek %뒤탭` 이 앞 탭 화면을 줬다).
    pub(crate) fn tab_for_pid(&self, pid: &str) -> &PaneTab {
        self.tabs
            .iter()
            .find(|t| t.pid.as_deref() == Some(pid))
            .unwrap_or_else(|| self.active())
    }
    /// 활성 탭. **`tabs` 가 비어도 패닉하지 않는다** — 화면이 통째로 죽는 것보다
    /// 빈 탭 한 장을 그리는 편이 낫다.
    ///
    /// 전에는 `&self.tabs[self.active_tab.min(self.tabs.len() - 1)]` 이었고, 빈
    /// 벡터에서 `len() - 1` 이 `usize::MAX` 로 언더플로우해 인덱스가 그대로
    /// 터졌다(2026-08-22 실측, 실제로 앱이 죽었다). ⚠️ 그때 패닉 로그가 가리킨
    /// 줄은 `Deref` 가 아니라 **위 `tab_for_pid` 의 fallback** 이었는데, 그건
    /// 릴리스 빌드가 **토씨 하나 안 틀리게 같은 두 표현식을 하나로 합쳐** 먼저
    /// 나온 쪽 줄번호를 찍었기 때문이다 — `tab_for_pid` 는 socket peek 에서만
    /// 불려 render 경로엔 없다. **패닉 로그의 행 번호는 호출 사슬과 대조하기
    /// 전까지 사실이 아니다.**
    ///
    /// 비는 경로는 각각 막았지만(`close_tab` 의 마지막 탭 가드,
    /// `merge_pane_into_tabs` 의 take 순서), `Deref` 는 `PaneState` 를 쓰는
    /// **모든** 자리가 지나므로 불변식이 또 깨질 때를 위한 그물을 남긴다.
    pub(crate) fn active(&self) -> &PaneTab {
        static EMPTY: std::sync::OnceLock<PaneTab> = std::sync::OnceLock::new();
        self.tabs
            .get(self.active_tab.min(self.tabs.len().saturating_sub(1)))
            .unwrap_or_else(|| EMPTY.get_or_init(PaneTab::default))
    }
}

impl std::ops::Deref for PaneState {
    type Target = PaneTab;
    fn deref(&self) -> &PaneTab {
        self.active()
    }
}

impl std::ops::DerefMut for PaneState {
    fn deref_mut(&mut self) -> &mut PaneTab {
        // 읽기 쪽(`active`)은 `&self` 라 정적 기본 탭을 가리킬 수밖에 없지만,
        // 여기는 `&mut self` 라 **불변식을 그 자리에서 되돌릴 수 있다** — 빈
        // 껍데기가 계속 빈 채로 돌아다니지 않게 기본 탭을 세운다.
        if self.tabs.is_empty() {
            self.tabs.push(PaneTab::default());
            self.active_tab = 0;
        }
        let i = self.active_tab.min(self.tabs.len() - 1);
        &mut self.tabs[i]
    }
}

/// One frame of an image pane. `rgba` is tightly-packed RGBA8 (`w * h * 4`).
/// Static images (png/jpg/…) are a single frame with zero delay; animated
/// gifs carry one entry per frame with its inter-frame delay.
struct ImageFrame {
    rgba: Vec<u8>,
    delay: std::time::Duration,
}

/// A decoded image bound to a pane. Each frame is uploaded once into a wgpu
/// texture keyed by `(pane, rotation, frame)`. `cur`/`last` drive gif playback
/// (advanced in `about_to_wait`); they stay put for single-frame images.
struct ImagePane {
    frames: Vec<ImageFrame>,
    w: u32,
    h: u32,
    cur: std::sync::atomic::AtomicUsize,
    last: std::sync::Mutex<std::time::Instant>,
}

impl ImagePane {
    fn cur_idx(&self) -> usize {
        self.cur
            .load(std::sync::atomic::Ordering::Relaxed)
            .min(self.frames.len().saturating_sub(1))
    }
    fn cur_rgba(&self) -> &[u8] {
        &self.frames[self.cur_idx()].rgba
    }
    /// Advance to the next frame when the current one's delay has elapsed.
    /// Returns true when the frame changed (caller requests a redraw).
    fn tick(&self, now: std::time::Instant) -> bool {
        if self.frames.len() < 2 {
            return false;
        }
        let cur = self.cur_idx();
        let mut last = self.last.lock().unwrap();
        if now.duration_since(*last) >= self.frames[cur].delay {
            self.cur.store(
                (cur + 1) % self.frames.len(),
                std::sync::atomic::Ordering::Relaxed,
            );
            *last = now;
            true
        } else {
            false
        }
    }
    /// When the current frame is due to flip — feeds the loop's WaitUntil so
    /// playback ticks without busy-waiting. None for single-frame images.
    fn next_deadline(&self) -> Option<std::time::Instant> {
        if self.frames.len() < 2 {
            return None;
        }
        Some(*self.last.lock().unwrap() + self.frames[self.cur_idx()].delay)
    }
}

/// Largest texture edge we upload. Comfortably under every backend's
/// max-texture-dimension and keeps a huge screenshot from eating VRAM;
/// the pane fits the image anyway so the downscale is invisible.
const MAX_IMAGE_EDGE: u32 = 4096;

/// Decode an image file to RGBA8, downscaling so neither edge exceeds
/// `MAX_IMAGE_EDGE`. Returns an error the `imgopen` caller surfaces on a
/// path that isn't a decodable image.
fn decode_image_rgba(path: &std::path::Path) -> anyhow::Result<ImagePane> {
    use std::sync::atomic::AtomicUsize;
    use std::sync::Mutex;
    use std::time::{Duration, Instant};
    // Animated gif: decode every frame + delay so about_to_wait can cycle them.
    // A single-frame gif falls through to the static path below.
    let is_gif = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("gif"))
        .unwrap_or(false);
    if is_gif {
        use image::AnimationDecoder;
        let file = std::fs::File::open(path)?;
        let dec = image::codecs::gif::GifDecoder::new(std::io::BufReader::new(file))?;
        let raw = dec.into_frames().collect_frames()?;
        if raw.len() > 1 {
            let mut frames = Vec::with_capacity(raw.len());
            let (mut w, mut h) = (0u32, 0u32);
            for f in raw {
                let delay: Duration = f.delay().into();
                let buf = f.into_buffer();
                w = buf.width();
                h = buf.height();
                frames.push(ImageFrame {
                    rgba: buf.into_raw(),
                    // Floor near-zero delays to ~100ms (as browsers do) so a
                    // 0-delay frame doesn't spin the loop.
                    delay: if delay.is_zero() {
                        Duration::from_millis(100)
                    } else {
                        delay
                    },
                });
            }
            return Ok(ImagePane {
                frames,
                w,
                h,
                cur: AtomicUsize::new(0),
                last: Mutex::new(Instant::now()),
            });
        }
    }
    let img = image::open(path).map_err(|e| anyhow::anyhow!("decode failed: {e}"))?;
    let (w0, h0) = (img.width(), img.height());
    let img = if w0 > MAX_IMAGE_EDGE || h0 > MAX_IMAGE_EDGE {
        img.resize(
            MAX_IMAGE_EDGE,
            MAX_IMAGE_EDGE,
            image::imageops::FilterType::Lanczos3,
        )
    } else {
        img
    };
    let rgba = img.to_rgba8();
    let (w, h) = (rgba.width(), rgba.height());
    Ok(ImagePane {
        frames: vec![ImageFrame {
            rgba: rgba.into_raw(),
            delay: Duration::ZERO,
        }],
        w,
        h,
        cur: AtomicUsize::new(0),
        last: Mutex::new(Instant::now()),
    })
}

/// Rotate RGBA8 image data by `quarters` * 90° clockwise. Returns the new
/// pixel buffer plus its new (w, h). quarters=0 returns the input untouched.
fn rotate_rgba_cw(rgba: &[u8], w: u32, h: u32, quarters: u8) -> (Vec<u8>, u32, u32) {
    let q = quarters % 4;
    if q == 0 || rgba.is_empty() {
        return (rgba.to_vec(), w, h);
    }
    let (nw, nh) = if q % 2 == 1 { (h, w) } else { (w, h) };
    let mut out = vec![0u8; rgba.len()];
    for y in 0..h {
        for x in 0..w {
            let src = ((y * w + x) * 4) as usize;
            let (nx, ny) = match q {
                1 => (h - 1 - y, x),         // 90° CW
                2 => (w - 1 - x, h - 1 - y), // 180°
                3 => (y, w - 1 - x),         // 270° CW
                _ => unreachable!(),
            };
            let dst = ((ny * nw + nx) * 4) as usize;
            out[dst..dst + 4].copy_from_slice(&rgba[src..src + 4]);
        }
    }
    (out, nw, nh)
}

/// Byte offset of the `col`-th char in `s` (clamped to `s.len()`). Used by the
/// Raw markdown editor to translate a char-column cursor into a byte index.
/// `col` 번째 글자가 시작하는 바이트 오프셋. 줄보다 크면 줄 끝.
///
/// backspace·insert·delete·newline 마다 두 번씩 불리는 자리라 두 단계로 짧게
/// 끊는다. ASCII 줄(코드에서 대부분)은 바이트=글자라 곱셈 하나로 끝나고,
/// 그 밖에는 UTF-8 **선두 바이트만 세는 바이트 스캔**을 쓴다 — 원래의
/// `char_indices().nth()` 는 줄을 실제로 디코딩하며 걸어갔다.
///
/// 실측(20만 회, release): 짧은 ASCII 5.2배 · 긴 ASCII 5.8배 · 긴 한글 1.8배
/// 빠르다. 짧은 한글 줄만 `is_ascii` 검사값 때문에 회당 2.3ns 손해인데, 편집 한
/// 번에 두 번 불려도 5ns 라 긴 줄에서 버는 것에 비하면 없는 값이다.
fn char_byte(s: &str, col: usize) -> usize {
    if s.is_ascii() {
        return col.min(s.len());
    }
    let mut n = 0;
    for (i, &b) in s.as_bytes().iter().enumerate() {
        // 0b10xxxxxx 는 이어지는 바이트 — 글자 시작이 아니다.
        if b & 0xC0 != 0x80 {
            if n == col {
                return i;
            }
            n += 1;
        }
    }
    s.len()
}

/// One styled inline run inside a markdown block. The renderer picks the
/// font weight/slant and (for `code`) a mono face + chip background from
/// these flags.
#[derive(Clone, Debug)]
struct MdSpan {
    text: String,
    bold: bool,
    italic: bool,
    code: bool,
    /// `~~취소선~~`. 렌더는 밑줄과 같은 선을 x-height 중간에 긋는다.
    strike: bool,
    /// `Some(dest)` for the text inside a `[text](dest)` link. The renderer
    /// draws it accented + underlined; a click resolves `dest` to a file
    /// (Finder) or URL (browser).
    link: Option<String>,
}

/// A laid-out-able markdown block. Parsed once on open; the renderer walks
/// this every frame (cheap) rather than re-parsing the source text.
#[derive(Clone)]
enum MdBlock {
    Heading {
        level: u8,
        spans: Vec<MdSpan>,
    },
    Para {
        spans: Vec<MdSpan>,
    },
    /// Fenced/indented code block — raw text with embedded newlines. `lang`
    /// is the fence info string (e.g. "rust"), empty for indented blocks.
    Code {
        code: String,
        lang: String,
    },
    /// `task` = `Some(checked)` for a `- [ ]` / `- [x]` item; the renderer draws
    /// a checkbox instead of `marker`.
    ListItem {
        depth: u8,
        marker: String,
        spans: Vec<MdSpan>,
        task: Option<bool>,
    },
    Quote {
        spans: Vec<MdSpan>,
    },
    /// `> [!NOTE]` 알림. 인용문과 같은 문단 평탄화를 쓰되 `first`/`last` 로 여러
    /// 문단이 한 상자로 이어지게 한다 — 문단마다 상자를 닫으면 이어진 글이
    /// 토막토막 끊겨 읽힌다.
    ///
    /// `list` 는 알림 안 목록 항목의 (깊이, 표식). 목록을 `ListItem` 으로 내보내면
    /// 상자 밖에 매달려 경고에 딸린 목록이 경고 밖의 글로 읽힌다. 알림 안
    /// 체크박스(`- [ ]`)는 표식으로만 떨어진다 — 상자 안 체크박스는 실제 문서에서
    /// 거의 안 쓰여, 렌더 코드를 겹쳐 둘 값이 없다.
    Callout {
        kind: MdCallout,
        spans: Vec<MdSpan>,
        first: bool,
        last: bool,
        list: Option<(u8, String)>,
    },
    Rule,
    /// YAML frontmatter, as label/value rows. Kept as its own block rather than
    /// parsed into the body: without it the closing `---` reads as a setext
    /// heading and the whole header lands on screen as huge bold text.
    Meta {
        rows: Vec<(String, String)>,
    },
    /// `![alt](path)` — rendered as a wgpu texture inline (same path as the
    /// image pane). `key` is the texture cache id, `w`/`h` the decoded pixel
    /// size for aspect layout; all three are filled in after parse when the
    /// image is decoded (0/empty until then). `path` is kept for alt fallback.
    Image {
        path: String,
        alt: String,
        key: String,
        w: u32,
        h: u32,
    },
    /// GFM table. `head` is the header row (empty for a headerless table),
    /// `rows` the body; every cell carries its own inline spans so a cell can
    /// hold bold/code/links like any other block. `align` is per column.
    Table {
        head: Vec<MdCell>,
        rows: Vec<Vec<MdCell>>,
        align: Vec<MdAlign>,
    },
}

/// A drag selection inside a rendered markdown pane. Coordinates are **document
/// space** = screen px + the pane's scroll offset. Keeping the scroll folded in
/// is what lets a selection survive scrolling mid-drag; with plain screen y the
/// whole range would slide along with the wheel.
struct MdRenderSel {
    pane: String,
    anchor: (f32, f32),
    end: (f32, f32),
    /// Mouse still held. Cleared on release so the selection stays visible (and
    /// copyable) afterwards without the cursor dragging it around.
    dragging: bool,
}

/// One table cell's inline content.
type MdCell = Vec<MdSpan>;

/// GFM 알림 종류(`> [!NOTE]`) — 노션 콜아웃 자리. 인용문과 달리 "이걸 조심해라"
/// 는 신호라, 색과 표지로 본문 흐름에서 튀어나와야 한다.
#[derive(Clone, Copy, PartialEq, Debug)]
enum MdCallout {
    Note,
    Tip,
    Important,
    Warning,
    Caution,
}

impl MdCallout {
    /// 표지 아이콘·제목·색. 짝은 GitHub 이 쓰는 그대로 둔다 — 같은 문서를 딴 데서
    /// 볼 때와 뜻이 어긋나면 안 된다. 색은 전부 테마 토큰이라 테마를 바꿔도 따라간다.
    fn face(self) -> (&'static str, &'static str, [u8; 4]) {
        match self {
            Self::Note => ("info", "Note", theme::accent()),
            Self::Tip => ("lightbulb", "Tip", theme::success()),
            Self::Important => ("message-square-warning", "Important", theme::syn_keyword()),
            Self::Warning => ("triangle-alert", "Warning", theme::syn_type()),
            Self::Caution => ("octagon-alert", "Caution", theme::danger()),
        }
    }
}

/// Per-column alignment from a table's `|:--:|` delimiter row.
#[derive(Clone, Copy, PartialEq, Debug)]
enum MdAlign {
    Left,
    Center,
    Right,
}

/// A decoded image referenced by a markdown document, uploaded to the GPU
/// under `key` and drawn where its `MdBlock::Image` sits.
struct MdDocImage {
    key: String,
    rgba: Vec<u8>,
    w: u32,
    h: u32,
}

/// A parsed markdown document bound to a pane (same lifetime discipline as
/// `ImagePane`). The renderer lays the blocks out into the pane box. `path`
/// is the source file (for the edit button); `images` are decoded inline
/// images the renderer uploads + draws.
struct MarkdownDoc {
    /// Serial number, unique per parse. The renderer memoizes block heights
    /// against it — a pointer/path wouldn't do, since a reparsed doc can land
    /// on the same address and would then be served the old layout.
    gen: u64,
    blocks: Vec<MdBlock>,
    /// 0-based source line of each block, index-aligned with `blocks`. Lets the
    /// Raw↔Render toggle keep the line you were reading on screen.
    block_lines: Vec<usize>,
    path: String,
    images: Vec<MdDocImage>,
    /// Original source text — seeds the Raw editor buffer and is rewritten on
    /// save, then re-parsed into `blocks`.
    raw: String,
}

fn heading_level(l: pulldown_cmark::HeadingLevel) -> u8 {
    use pulldown_cmark::HeadingLevel::*;
    match l {
        H1 => 1,
        H2 => 2,
        H3 => 3,
        H4 => 4,
        H5 => 5,
        H6 => 6,
    }
}

/// Parse markdown source into a flat block list. Nesting beyond list depth
/// is flattened (a quote's paragraphs collapse into Quote blocks, list-item
/// paragraphs into the item) — enough structure for a document-style reader
/// without a full layout tree.
///
/// The second return is each block's 0-based source line, index-aligned with
/// the blocks. Raw↔Render mode switching uses it to keep the line you were
/// looking at on screen; without it the toggle can only guess.
/// frontmatter 를 화면에 세울 라벨/값 줄로 눕힌다. 진짜 YAML 파서를 붙일 이유는
/// 없다 — 속성 줄로 보여줄 만큼만 읽는다. 중첩 키는 `부모.자식`으로 펴고, `- `
/// 목록은 바로 위 키의 값에 이어 붙인다.
fn parse_frontmatter_rows(src: &str) -> Vec<(String, String)> {
    let mut rows: Vec<(String, String)> = Vec::new();
    let mut parent: Option<String> = None;
    for line in src.lines() {
        let body = line.trim();
        if body.is_empty() {
            continue;
        }
        let indented = line.starts_with(' ') || line.starts_with('\t');
        if let Some(item) = body.strip_prefix("- ") {
            if let Some(last) = rows.last_mut() {
                if last.1.is_empty() {
                    last.1 = item.trim().to_string();
                } else {
                    last.1.push_str(", ");
                    last.1.push_str(item.trim());
                }
            }
            continue;
        }
        let Some((k, v)) = body.split_once(':') else {
            continue;
        };
        let (k, v) = (k.trim(), v.trim());
        if k.is_empty() {
            continue;
        }
        // 값이 빈 최상위 키(`metadata:`)는 뒤따르는 들여쓰기 줄들의 부모다.
        if v.is_empty() && !indented {
            parent = Some(k.to_string());
            continue;
        }
        let label = match (indented, parent.as_deref()) {
            (true, Some(p)) => format!("{p}.{k}"),
            _ => k.to_string(),
        };
        if !indented {
            parent = None;
        }
        rows.push((label, v.to_string()));
    }
    rows
}

/// 블록이 확정될 때 `[[토픽이름]]` 을 링크 스팬으로 바꾼다.
///
/// 파싱 도중에 못 하는 이유: cmark 는 짝이 안 맞는 대괄호를 **낱개 이벤트**로
/// 흘린다(`[`·`[`·이름·`]`·`]` 다섯 개). 이벤트 하나만 보면 표기가 완성돼 보이는
/// 순간이 없어, 스팬이 다 모인 뒤 이어 붙여야 비로소 눈에 띈다.
///
/// 스타일이 같은 평문 스팬만 이어 붙인다 — 인라인 코드나 이미 링크인 조각을
/// 삼키면 `` `[[a]]` `` 처럼 일부러 표기를 보여 주는 글까지 링크가 된다.
fn wikilinked(spans: &mut Vec<MdSpan>) -> Vec<MdSpan> {
    let src = std::mem::take(spans);
    if !src
        .iter()
        .any(|s| s.link.is_none() && !s.code && s.text.contains('['))
    {
        return src;
    }
    let mut out: Vec<MdSpan> = Vec::with_capacity(src.len());
    let mut run = String::new();
    let mut style = (false, false, false);
    fn flush(out: &mut Vec<MdSpan>, run: &mut String, st: (bool, bool, bool)) {
        if !run.is_empty() {
            push_wikilinked(out, run, st.0, st.1, st.2);
            run.clear();
        }
    }
    for s in src {
        let st = (s.bold, s.italic, s.strike);
        if s.link.is_none() && !s.code {
            if !run.is_empty() && st != style {
                flush(&mut out, &mut run, style);
            }
            style = st;
            run.push_str(&s.text);
        } else {
            flush(&mut out, &mut run, style);
            out.push(s);
        }
    }
    flush(&mut out, &mut run, style);
    out
}

/// `[[토픽이름]]` 을 클릭 가능한 링크로 쪼갠다. 메모리 볼트는 문서를 이 표기로
/// 엮는데(인덱스 한 줄이 곧 한 토픽) 마크다운 표준이 아니라 여태 죽은 글자였다 —
/// 노션의 페이지 링크에 해당하는 자리다. 목적지는 `wiki:` 스킴으로 넘겨 클릭할 때
/// 파일을 찾고, 화면에는 대괄호를 벗긴 이름만 남긴다.
fn push_wikilinked(spans: &mut Vec<MdSpan>, t: &str, bold: bool, italic: bool, strike: bool) {
    let plain = |spans: &mut Vec<MdSpan>, s: &str| {
        if !s.is_empty() {
            spans.push(MdSpan {
                text: s.to_string(),
                bold,
                italic,
                code: false,
                strike,
                link: None,
            });
        }
    };
    let mut rest = t;
    while let Some(i) = rest.find("[[") {
        let after = &rest[i + 2..];
        let Some(j) = after.find("]]") else { break };
        let name = &after[..j];
        // 이름에 대괄호나 줄바꿈이 섞이면 위키링크가 아니다 — 여는 괄호까지만
        // 평문으로 흘리고 그 뒤에서 다시 찾는다.
        if name.is_empty() || name.contains(['[', ']', '\n']) {
            plain(spans, &rest[..i + 2]);
            rest = after;
            continue;
        }
        plain(spans, &rest[..i]);
        spans.push(MdSpan {
            text: name.to_string(),
            bold,
            italic,
            code: false,
            strike,
            link: Some(format!("wiki:{name}")),
        });
        rest = &after[j + 2..];
    }
    plain(spans, rest);
}

/// 원시 HTML 조각에서 사람이 읽을 글자만 뽑는다. 렌더뷰엔 HTML 엔진이 없어
/// 태그를 그릴 수는 없지만, 태그에 감싸였다는 이유로 본문을 버리면 문서 내용이
/// 조용히 사라진다 — `<div>`·`<summary>`·`<system-reminder>` 안의 문장이 실제로
/// 화면에서 없어졌다. 태그 이름에 밑줄이 든 `<critical_rule>` 은 HTML 태그
/// 규칙에 어긋나 애초에 평문으로 흐르니 이 경로를 타지 않는다.
///
/// 주석(`<!-- -->`)은 반대로 지우는 게 맞다. 감춰 두려고 쓴 표기라 드러내면
/// 글쓴이의 뜻이 뒤집힌다.
fn html_visible_text(raw: &str) -> String {
    let mut out = String::new();
    let mut rest = raw;
    while let Some(i) = rest.find('<') {
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        let skip = if tail.starts_with("<!--") {
            // 닫히지 않은 주석은 문서 끝까지 주석이다(CommonMark).
            match tail[4..].find("-->") {
                Some(k) => 4 + k + 3,
                None => break,
            }
        } else {
            match tail.find('>') {
                Some(k) => k + 1,
                None => break,
            }
        };
        rest = &tail[skip..];
    }
    out.push_str(rest);
    // 엔티티는 자주 쓰는 것만 푼다. 전체 표를 들일 만큼 마크다운 문서에
    // 엔티티가 잦지 않다. `&amp;` 를 맨 뒤에 두는 건 `&amp;lt;` 가 `<` 로
    // 두 번 풀리지 않게 하기 위한 것이다.
    let out = out
        .replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&");
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn parse_markdown(text: &str) -> (Vec<MdBlock>, Vec<usize>) {
    use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
    let mut blocks: Vec<MdBlock> = Vec::new();
    let mut block_lines: Vec<usize> = Vec::new();
    // Byte offset of every line start, so a block's byte offset becomes a line
    // number by binary search. Counting newlines per block would be O(n) each.
    let line_starts: Vec<usize> = std::iter::once(0)
        .chain(text.match_indices('\n').map(|(i, _)| i + 1))
        .collect();
    let line_of = |off: usize| line_starts.partition_point(|&s| s <= off).saturating_sub(1);
    let mut spans: Vec<MdSpan> = Vec::new();
    let mut bold = 0i32;
    let mut italic = 0i32;
    let mut strike = 0i32;
    let mut in_meta = false;
    let mut meta_buf = String::new();
    let mut item_task: Option<bool> = None;
    let mut heading: Option<u8> = None;
    let mut in_code = false;
    let mut code_buf = String::new();
    let mut code_lang = String::new();
    // Each open list level: Some(next_number) for ordered, None for bullet.
    let mut list_stack: Vec<Option<u64>> = Vec::new();
    let mut in_item = false;
    let mut item_marker = String::new();
    let mut in_quote = false;
    // 열려 있는 인용문이 알림(`> [!NOTE]`)이면 그 종류. `quote_first` 는 상자
    // 머리(아이콘·제목)를 첫 문단에만 그리기 위한 것이다.
    let mut quote_kind: Option<MdCallout> = None;
    let mut quote_first = true;
    let mut in_image = false;
    let mut img_url = String::new();
    let mut img_alt = String::new();
    let mut link_url: Option<String> = None;
    // Table accumulators. Cells reuse `spans` (they hold inline content only —
    // pulldown emits no Paragraph inside a cell), flushed at each TagEnd.
    let mut tbl_align: Vec<MdAlign> = Vec::new();
    let mut tbl_head: Vec<MdCell> = Vec::new();
    let mut tbl_rows: Vec<Vec<MdCell>> = Vec::new();
    let mut tbl_row: Vec<MdCell> = Vec::new();

    let push_span = |spans: &mut Vec<MdSpan>,
                     t: &str,
                     b: bool,
                     i: bool,
                     s: bool,
                     c: bool,
                     link: Option<String>| {
        if !t.is_empty() {
            spans.push(MdSpan {
                text: t.to_string(),
                bold: b,
                italic: i,
                code: c,
                strike: s,
                link,
            });
        }
    };

    // GFM 확장을 켜 둔다: 켜지 않으면 `~~취소선~~`·`- [ ]` 가 평문으로 떨어지고,
    // 무엇보다 frontmatter 의 닫는 `---` 가 setext heading 으로 읽혀 YAML 머리가
    // 문서 첫 화면을 거대한 굵은 글씨로 덮는다. `ENABLE_GFM` 은 알림 태그
    // (`> [!NOTE]`) 하나만 켠다 — 다른 파싱 규칙은 건드리지 않는다.
    let opts = Options::ENABLE_TABLES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_GFM
        | Options::ENABLE_YAML_STYLE_METADATA_BLOCKS;
    for (ev, rng) in Parser::new_ext(text, opts).into_offset_iter() {
        match ev {
            Event::Start(tag) => match tag {
                Tag::Heading { level, .. } => {
                    heading = Some(heading_level(level));
                    spans.clear();
                }
                Tag::Paragraph => spans.clear(),
                Tag::CodeBlock(kind) => {
                    in_code = true;
                    code_buf.clear();
                    code_lang = match kind {
                        pulldown_cmark::CodeBlockKind::Fenced(info) => {
                            info.split_whitespace().next().unwrap_or("").to_string()
                        }
                        pulldown_cmark::CodeBlockKind::Indented => String::new(),
                    };
                }
                Tag::List(start) => {
                    // 중첩 리스트가 열리면 부모 항목을 **여기서** 밀어 넣는다.
                    // TagEnd::Item 까지 미루면 자식이 먼저 블록 목록에 들어가
                    // 화면에서 부모 위로 올라오고, 게다가 자식의 Tag::Item 이
                    // 공유 버퍼인 spans 를 비워 부모 텍스트가 통째로 사라진다
                    // (`- outer` / `  - inner` 가 inner, "" 순으로 나왔다).
                    if in_item && !spans.is_empty() {
                        blocks.push(MdBlock::ListItem {
                            depth: list_stack.len().saturating_sub(1) as u8,
                            marker: std::mem::take(&mut item_marker),
                            spans: wikilinked(&mut spans),
                            task: item_task.take(),
                        });
                        in_item = false;
                    }
                    list_stack.push(start)
                }
                Tag::Item => {
                    in_item = true;
                    item_task = None;
                    spans.clear();
                    item_marker = match list_stack.last_mut() {
                        Some(Some(n)) => {
                            let m = format!("{n}.");
                            *n += 1;
                            m
                        }
                        _ => "•".to_string(),
                    };
                }
                Tag::Emphasis => italic += 1,
                Tag::Strong => bold += 1,
                Tag::Strikethrough => strike += 1,
                Tag::MetadataBlock(_) => {
                    in_meta = true;
                    meta_buf.clear();
                }
                Tag::BlockQuote(kind) => {
                    in_quote = true;
                    quote_kind = kind.map(|k| match k {
                        pulldown_cmark::BlockQuoteKind::Note => MdCallout::Note,
                        pulldown_cmark::BlockQuoteKind::Tip => MdCallout::Tip,
                        pulldown_cmark::BlockQuoteKind::Important => MdCallout::Important,
                        pulldown_cmark::BlockQuoteKind::Warning => MdCallout::Warning,
                        pulldown_cmark::BlockQuoteKind::Caution => MdCallout::Caution,
                    });
                    quote_first = true;
                    spans.clear();
                }
                Tag::Image { dest_url, .. } => {
                    in_image = true;
                    img_url = dest_url.to_string();
                    img_alt.clear();
                }
                Tag::Link { dest_url, .. } => {
                    link_url = Some(dest_url.to_string());
                }
                Tag::Table(aligns) => {
                    tbl_align = aligns
                        .iter()
                        .map(|a| match a {
                            pulldown_cmark::Alignment::Center => MdAlign::Center,
                            pulldown_cmark::Alignment::Right => MdAlign::Right,
                            _ => MdAlign::Left,
                        })
                        .collect();
                    tbl_head.clear();
                    tbl_rows.clear();
                    tbl_row.clear();
                }
                Tag::TableHead | Tag::TableRow => tbl_row.clear(),
                Tag::TableCell => spans.clear(),
                _ => {}
            },
            Event::End(tag) => match tag {
                TagEnd::Heading(_) => blocks.push(MdBlock::Heading {
                    level: heading.take().unwrap_or(1),
                    spans: wikilinked(&mut spans),
                }),
                TagEnd::Paragraph => {
                    // Skip empty paragraphs (e.g. a paragraph that held only an
                    // image, which was emitted as its own Image block).
                    if in_quote {
                        if !spans.is_empty() {
                            let spans = wikilinked(&mut spans);
                            blocks.push(match quote_kind {
                                Some(kind) => {
                                    let first = std::mem::take(&mut quote_first);
                                    // `last` 는 인용문이 닫힐 때 되짚어 세운다 —
                                    // 여기선 다음 문단이 있을지 알 수 없다.
                                    MdBlock::Callout {
                                        kind,
                                        spans,
                                        first,
                                        last: false,
                                        list: None,
                                    }
                                }
                                None => MdBlock::Quote { spans },
                            });
                        }
                    } else if !in_item && !spans.is_empty() {
                        blocks.push(MdBlock::Para {
                            spans: wikilinked(&mut spans),
                        });
                    }
                    if !in_item {
                        spans.clear();
                    }
                    // in_item: keep spans; flushed at TagEnd::Item.
                }
                TagEnd::CodeBlock => {
                    in_code = false;
                    blocks.push(MdBlock::Code {
                        code: std::mem::take(&mut code_buf),
                        lang: std::mem::take(&mut code_lang),
                    });
                }
                TagEnd::List(_) => {
                    list_stack.pop();
                }
                TagEnd::Item => {
                    // `in_item` 이 false 면 중첩 리스트가 열릴 때 이미 내보낸
                    // 항목이다 — 여기서 또 밀면 빈 항목이 하나 더 생긴다.
                    if in_item {
                        let depth = list_stack.len().saturating_sub(1) as u8;
                        let marker = std::mem::take(&mut item_marker);
                        let spans = wikilinked(&mut spans);
                        let task = item_task.take();
                        blocks.push(match quote_kind {
                            Some(kind) => MdBlock::Callout {
                                kind,
                                spans,
                                first: std::mem::take(&mut quote_first),
                                last: false,
                                list: Some((depth, marker)),
                            },
                            None => MdBlock::ListItem {
                                depth,
                                marker,
                                spans,
                                task,
                            },
                        });
                        in_item = false;
                    }
                }
                TagEnd::Emphasis => italic -= 1,
                TagEnd::Strong => bold -= 1,
                TagEnd::Strikethrough => strike -= 1,
                TagEnd::MetadataBlock(_) => {
                    in_meta = false;
                    let rows = parse_frontmatter_rows(&std::mem::take(&mut meta_buf));
                    if !rows.is_empty() {
                        blocks.push(MdBlock::Meta { rows });
                    }
                }
                TagEnd::Link => link_url = None,
                TagEnd::BlockQuote(_) => {
                    in_quote = false;
                    // 상자 아래를 닫는다. `quote_first` 가 아직 서 있으면 이 알림엔
                    // 문단이 하나도 없던 것(목록만 든 알림)이라 닫을 상자가 없다.
                    // 뒤에서부터 찾는 이유: 알림 안 목록은 상자 밖으로 평탄화돼
                    // `blocks` 맨 끝이 ListItem 일 수 있다.
                    if quote_kind.take().is_some() && !quote_first {
                        if let Some(MdBlock::Callout { last, .. }) = blocks
                            .iter_mut()
                            .rev()
                            .find(|b| matches!(b, MdBlock::Callout { .. }))
                        {
                            *last = true;
                        }
                    }
                }
                TagEnd::Image => {
                    blocks.push(MdBlock::Image {
                        path: std::mem::take(&mut img_url),
                        alt: std::mem::take(&mut img_alt),
                        key: String::new(),
                        w: 0,
                        h: 0,
                    });
                    in_image = false;
                }
                TagEnd::TableCell => tbl_row.push(wikilinked(&mut spans)),
                TagEnd::TableHead => tbl_head = std::mem::take(&mut tbl_row),
                TagEnd::TableRow => tbl_rows.push(std::mem::take(&mut tbl_row)),
                TagEnd::Table => blocks.push(MdBlock::Table {
                    head: std::mem::take(&mut tbl_head),
                    rows: std::mem::take(&mut tbl_rows),
                    align: std::mem::take(&mut tbl_align),
                }),
                _ => {}
            },
            Event::Text(t) => {
                if in_meta {
                    meta_buf.push_str(&t);
                } else if in_image {
                    img_alt.push_str(&t);
                } else if in_code {
                    code_buf.push_str(&t);
                } else if link_url.is_some() {
                    push_span(
                        &mut spans,
                        &t,
                        bold > 0,
                        italic > 0,
                        strike > 0,
                        false,
                        link_url.clone(),
                    );
                } else {
                    push_span(
                        &mut spans,
                        &t,
                        bold > 0,
                        italic > 0,
                        strike > 0,
                        false,
                        None,
                    );
                }
            }
            Event::Code(t) => push_span(
                &mut spans,
                &t,
                bold > 0,
                italic > 0,
                strike > 0,
                true,
                link_url.clone(),
            ),
            Event::Html(raw) => {
                // 블록 HTML. 태그만 든 줄은 아무것도 남기지 않고, 태그에 감싸인
                // 문장만 문단으로 살아난다. cmark 가 이 블록을 한 덩어리로 주든
                // 줄마다 쪼개 주든 결과가 같아, 어느 쪽이어도 글이 안 사라진다.
                let text = html_visible_text(&raw);
                if !text.is_empty() {
                    let mut s = vec![MdSpan {
                        text,
                        bold: false,
                        italic: false,
                        code: false,
                        strike: false,
                        link: None,
                    }];
                    blocks.push(MdBlock::Para {
                        spans: wikilinked(&mut s),
                    });
                }
            }
            Event::InlineHtml(raw) => {
                // 인라인 태그. 통째로 버리면 강조가 사라지고 글자로 그리면 문서에
                // 없던 꺾쇠가 생긴다 — 아는 태그는 서체로 옮기고 모르는 태그만
                // 조용히 지운다. `<br>` 는 띄어쓰기로 떨어진다(스팬에 줄바꿈
                // 표기가 없다). 붙여 버리면 앞뒤 낱말이 한 덩어리가 된다.
                let t = raw.trim();
                let closing = t.starts_with("</");
                let name = t
                    .trim_start_matches(['<', '/'])
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric())
                    .flat_map(|c| c.to_lowercase())
                    .collect::<String>();
                let bump = |n: &mut i32| {
                    if closing {
                        *n = (*n - 1).max(0)
                    } else {
                        *n += 1
                    }
                };
                match name.as_str() {
                    "b" | "strong" => bump(&mut bold),
                    "i" | "em" => bump(&mut italic),
                    "s" | "del" | "strike" => bump(&mut strike),
                    "br" => push_span(
                        &mut spans,
                        " ",
                        bold > 0,
                        italic > 0,
                        strike > 0,
                        false,
                        link_url.clone(),
                    ),
                    _ => {}
                }
            }
            Event::TaskListMarker(checked) => item_task = Some(checked),
            Event::SoftBreak | Event::HardBreak => {
                if in_meta {
                    meta_buf.push('\n');
                } else if in_code {
                    code_buf.push('\n');
                } else {
                    push_span(
                        &mut spans,
                        " ",
                        bold > 0,
                        italic > 0,
                        strike > 0,
                        false,
                        link_url.clone(),
                    );
                }
            }
            Event::Rule => blocks.push(MdBlock::Rule),
            _ => {}
        }
        // 이 이벤트가 블록을 밀어 넣었으면 소스 줄을 짝지어 둔다. End 이벤트의
        // 범위는 요소 전체라 `rng.start` 가 곧 그 블록이 시작한 줄이다. 푸시
        // 지점마다 적지 않고 여기 한 곳에서 채우는 건, 그래야 두 벡터의 길이가
        // 구조적으로 어긋날 수 없어서다.
        while block_lines.len() < blocks.len() {
            block_lines.push(line_of(rng.start));
        }
    }
    (blocks, block_lines)
}

/// Parse + decode a markdown document: parse blocks, then decode each inline
/// image (resolving relative paths against the md file's dir, skipping remote
/// URLs) under path-keyed textures. Shared by initial open and post-edit
/// re-parse.
fn build_markdown_doc(p: &std::path::Path, text: &str) -> MarkdownDoc {
    let (mut blocks, block_lines) = parse_markdown(text);
    let md_dir = p.parent().map(|d| d.to_path_buf());
    let mut images: Vec<MdDocImage> = Vec::new();
    for block in blocks.iter_mut() {
        if let MdBlock::Image {
            path: ipath,
            key,
            w,
            h,
            ..
        } = block
        {
            if ipath.starts_with("http://") || ipath.starts_with("https://") {
                continue;
            }
            let resolved = if std::path::Path::new(&ipath).is_absolute() {
                std::path::PathBuf::from(&*ipath)
            } else if let Some(dir) = &md_dir {
                dir.join(&*ipath)
            } else {
                std::path::PathBuf::from(&*ipath)
            };
            if let Ok(img) = decode_image_rgba(&resolved) {
                // 텍스처 id 는 **이미지 파일 경로**다. 예전엔 `{pane}#img{블록번호}`
                // 였는데, 이미지 위에 문단 하나만 끼워 넣어도 번호가 밀려 새 키가
                // 되고 옛 텍스처는 `Gpu::images` 에 주인 없이 남았다(누수). 게다가
                // 같은 파일을 두 pane 에서 열면 같은 그림을 두 번 올렸다.
                // 경로로 잡으면 문서를 다시 파싱해도, pane 이 몇 개든 하나다.
                let canon = std::fs::canonicalize(&resolved).unwrap_or_else(|_| resolved.clone());
                let k = format!("mdimg:{}", canon.display());
                *key = k.clone();
                *w = img.w;
                *h = img.h;
                images.push(MdDocImage {
                    key: k,
                    rgba: img.cur_rgba().to_vec(),
                    w: img.w,
                    h: img.h,
                });
            }
        }
    }
    static GEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    MarkdownDoc {
        gen: GEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        blocks,
        block_lines,
        path: p.to_string_lossy().into_owned(),
        images,
        raw: text.to_string(),
    }
}

/// Whole-window state: HashMap of panes keyed by tmux pane id, the
/// most recently parsed Layout tree, and which pane is active for
/// keyboard / selection / cursor display.
/// 사이드바 방 이름 인라인 편집 상태.
///
/// **느린 더블클릭**(Finder 파일명 바꾸기)이라 진짜 더블클릭과 갈라야 한다. 규칙은
/// `starts_room_rename` 에 순수 함수로 두고 여기선 상태만 든다.
#[derive(Default)]
struct RoomRename {
    /// 마지막으로 누른 방 줄과 그 시각 — 다음 클릭이 "느린" 것인지 판정하는 기준.
    last_click: Option<(usize, std::time::Instant)>,
    /// 편집 중인 방과 입력 버퍼. None = 편집 아님.
    editing: Option<(usize, String)>,
    /// 버퍼 안 커서(문자 단위). 편집을 열 때 이름 끝에 둔다.
    cursor: usize,
    /// 다른 기기의 방을 고치는 중 — (기기, 그쪽 방 번호, 카드 키). `editing` 의 인덱스는
    /// 이때 자리표시(`usize::MAX`)라 본기기 방과 안 겹친다.
    remote: Option<(String, u64, String)>,
}

/// 닫는 동안 목록 자리를 얼려 두는 상태 — 되살리기 목록·pane 헤더 탭·사이드바 방.
///
/// 크롬이 탭을 닫을 때 하는 것과 같다: 커서가 그 목록 위에 있는 동안은 남은 항목이
/// 자리를 안 옮겨, ×를 **한자리에서 연달아** 누를 수 있다(2026-08-27 지시). 커서를
/// 떼면 그때 재정렬된다.
///
/// **시한이 있는 이유**는 `pump_info` 의 목록 동결과 같다 — `CursorLeft` 를 받지
/// 않으므로 커서 좌표는 창을 떠나도 마지막 자리에 남는다. 시한이 없으면 목록 위에
/// 마우스를 얹은 채 자리를 뜨면 영영 굳는다.
///
/// `struct App` 정의는 병렬 작업 충돌 핫스팟이라(CLAUDE.md) 필드를 흩지 않고 묶었다.
#[derive(Default)]
struct CloseFreeze {
    /// pane 헤더 탭: 닫기 직전 탭 알약의 `(pane id, [(x, w)])`. 이 띠만 기하가
    /// 진짜로 어긋난다 — 알약 폭이 라벨 길이를 따라가서, 하나 닫으면 남은 탭이
    /// 넓어지며 × 가 딴 자리로 간다. 남은 탭이 이 슬롯을 앞에서부터 채운다.
    tab_slots: Option<(String, Vec<(f32, f32)>)>,
    /// 사이드바 방 목록: 얼어붙은 스크롤 위치(px). 방이 줄면 스크롤 한계가 작아져
    /// 목록이 아래로 당겨지는 것을 막는다.
    sidebar_scroll: Option<f32>,
    /// 되살리기 패널: 얼어붙은 content 높이. 항목이 줄면 스크롤 상한이 같이 줄어
    /// 목록 전체가 밀린다 — 그 상한을 붙잡아 둔다.
    info_content: Option<f32>,
    /// 언제 얼었나. 위 주석의 시한이 여기서 재진다.
    since: Option<Instant>,
}

impl CloseFreeze {
    /// 얼어 있고 아직 시한 안인가. 아니면 호출자가 `thaw` 로 녹인다.
    fn live(&self) -> bool {
        self.since.is_some_and(|t| t.elapsed() < FREEZE_TTL)
    }
    fn thaw(&mut self) {
        *self = Self::default();
    }
}

/// 닫기 동결 시한 — `pump_info` 의 목록 동결과 같은 3초.
const FREEZE_TTL: std::time::Duration = std::time::Duration::from_secs(3);

/// 어느 목록을 얼리나 — `freeze_closing` 의 인자.
pub(crate) enum CloseFreezeKind {
    /// 사이드바 방 목록. 값은 얼려 둘 스크롤 시작점.
    Sidebar(f32),
    /// 되살리기 패널. 값은 얼려 둘 content 높이.
    Info(f32),
    /// pane 헤더 탭 띠. `(pane id, [(x, w)])`.
    Tabs(String, Vec<(f32, f32)>),
}

struct Workspace {
    panes: HashMap<String, PaneState>,
    layout: Option<Layout>,
    active_pane: Option<String>,
    /// Reverse map: PtySession id (a tab's `pid`) → outer pane id (the layout
    /// key in `panes`). Stage 3 lets one pane host multiple tabs each with
    /// their own PTY; this lookup lets `pump_pty_screens` find the right
    /// `(PaneState, tab_index)` from a backend update keyed by its own pid.
    /// The first tab's pid equals the outer pane id, so single-tab panes
    /// don't need an entry — but secondary tabs always insert/remove here.
    pid_to_pane: HashMap<String, String>,
    /// 방별 분리(사용자): pane → 방(윈도우) 식별자. 새 방 pane 만 들어가고, 기본 방은
    /// 없음 → cwd-slug 그대로. ws 에 둬서 GUI(spawn)와 PtyBackend(collab_board) 가
    /// 같은 매핑을 본다(별 스레드라 App 필드는 socket.rs 가 못 봄).
    pane_room: HashMap<String, String>,
    /// pane → 배정 캐릭터명(미도리 등). pane_room 과 같은 이유로 ws 에 둔다 —
    /// pump 스레드(apply_screen_update)가 PaneState.character 를 동기하고, 헤더
    /// 렌더(render.rs)가 같은 매핑을 본다. 새 대화 실행 시 채우고 종료하면 비운다.
    pane_character: HashMap<String, String>,
    /// Delivered launch identity; empty means shell/retired and rejects late metadata.
    pane_launch_character: HashMap<String, String>,
    /// Explicit selection waiting for its first harness launch, not a shell identity.
    pane_next_character: HashMap<String, String>,
    /// 활성 윈도우(보이는 방)의 leaf pane id 집합. `publish_pty_layout` 이 갱신한다.
    /// collab_board 가 이걸로 bound pane 을 필터해 *활성 방 학생만* board 에 올린다
    /// (사용자: 아로나 방 + 프라나 방이 한 교실에 같이 뜨던 문제 — 방별 격리).
    active_window_panes: std::collections::HashSet<String>,
    /// pane → 속한 윈도우(방) 인덱스. 전 윈도우 leaf 를 `publish_pty_layout` 이 채운다.
    /// collab_board(PtyBackend, 별 스레드)가 App 의 windows/pty_layout 을 못 봐서 ws 로
    /// 미러 — board 가 전 방 학생을 window_idx 와 함께 실어 arona 좌측 방별 트리를 영속한다.
    pane_window: HashMap<String, usize>,
    /// 별도 OS 창으로 뗀 pane(auxterm.rs). `pane_window` 에는 떠나온 방으로 실리고
    /// 여기엔 「트리 밖」이라는 표시만 — 폰·board 가 닫힌 pane 과 가른다.
    undocked: std::collections::HashSet<String>,
    /// compact 중인 pane → 진행률. `pane_activity` 는 App 것이라 소켓 백엔드(별 스레드)가
    /// 못 보므로 활동 틱마다 여기로 미러 — `/term/panes` 가 폰에 「컴팩트 중」을 싣는다.
    compacting: HashMap<String, Option<u8>>,
    /// 방(윈도우)마다의 화면 배치 — 폰 허브의 미니맵이 「어느 방에 누가 어떤 크기로」를
    /// 그리는 재료. `layout` 은 보고 있는 방 하나뿐이라 따로 둔다.
    window_layouts: HashMap<usize, Layout>,
    /// pane 영역 가로÷세로(픽셀). 모든 방이 같은 창을 쓰므로 하나면 된다.
    grid_aspect: Option<f32>,
    /// 방(윈도우 인덱스)마다 사람이 보는 번호. GUI 가 방 이름을 다시 지을 때 미러한다 — 소켓 쪽(where·
    /// board)은 `App.windows` 를 못 봐서 인덱스+1 을 찍었고, 보기 방이 끼면 화면 ⌘N 과 하나씩 밀려
    /// 사람이 부른 「2번방」이 엉뚱한 방이 됐다(2026-10-09).
    room_numbers: Vec<RoomNumber>,
}

/// 사이드바가 그 방에 다는 번호(⌘N 과 같다, 1부터)와, 다른 기기 방의 보기 창이면 그 기기 이름.
/// 번호는 이 기기 방을 먼저 세고 보기 방을 뒤에 붙인다(`room_number_for_window`). 사이드바에 없는
/// 보기 방은 번호가 없다.
#[derive(Clone, Debug, Default, PartialEq)]
struct RoomNumber {
    shown: Option<usize>,
    view_of: Option<String>,
}

impl Default for Workspace {
    fn default() -> Self {
        Self {
            panes: HashMap::new(),
            layout: None,
            active_pane: None,
            pid_to_pane: HashMap::new(),
            pane_room: HashMap::new(),
            pane_character: HashMap::new(),
            pane_launch_character: HashMap::new(),
            pane_next_character: HashMap::new(),
            active_window_panes: std::collections::HashSet::new(),
            pane_window: HashMap::new(),
            undocked: std::collections::HashSet::new(),
            compacting: HashMap::new(),
            window_layouts: HashMap::new(),
            grid_aspect: None,
            room_numbers: Vec::new(),
        }
    }
}

impl Workspace {
    fn pane_mut(&mut self, id: &str) -> &mut PaneState {
        self.panes
            .entry(id.to_string())
            .or_insert_with(PaneState::default)
    }

    fn active(&self) -> Option<&PaneState> {
        self.active_pane
            .as_deref()
            .and_then(|id| self.panes.get(id))
    }

    fn active_mut(&mut self) -> Option<&mut PaneState> {
        let id = self.active_pane.clone()?;
        self.panes.get_mut(&id)
    }

    /// Rebuild `pid_to_pane` from the current `panes` so it's the
    /// authoritative pid→outer lookup. Cheap (one HashMap rebuild per
    /// layout/tab mutation) and lets the hot `outer_for_pty` path stay
    /// O(1) — important because every PTY ScreenUpdate calls it.
    fn rebuild_pid_map(&mut self) {
        let mut m: HashMap<String, String> = HashMap::with_capacity(self.pid_to_pane.len());
        for (outer, pane) in &self.panes {
            for tab in &pane.tabs {
                if let Some(pid) = tab.pid.as_deref() {
                    m.insert(pid.to_string(), outer.clone());
                }
            }
        }
        self.pid_to_pane = m;
    }

    /// O(1) pid → outer pane lookup. `pid_to_pane` is maintained on every
    /// layout/tab mutation; the panes-contains-key fallback only fires
    /// in the brief window before the first `ScreenUpdate` for a fresh
    /// shell has populated the tab's pid.
    ///
    /// 칸 번호와 PTY 번호는 같은 `%N` 이름을 나눠 쓴다. 제 PTY 가 끝나 「빈 자리」로 남은 칸(`%4`)에
    /// 남의 학생이 탭으로 앉고 `%4` 라는 PTY 가 다른 칸에 있을 수 있다 — 그때 표나 이름만 믿으면
    /// 그 PTY 의 화면이 엉뚱한 칸의 첫 탭에 써지고, 정작 보이는 칸은 얼어붙는다(2026-10-08 히후미).
    /// 그래서 실제로 그 pid 의 탭을 든 칸만 돌려준다.
    fn outer_for_pty(&self, pty_id: &str) -> Option<String> {
        let hosts = |p: &PaneState| p.tabs.iter().any(|t| t.pid.as_deref() == Some(pty_id));
        if let Some(outer) = self.pid_to_pane.get(pty_id).filter(|o| self.panes.get(*o).is_some_and(hosts)) {
            return Some(outer.clone());
        }
        if let Some((outer, _)) = self.panes.iter().find(|(_, p)| hosts(p)) {
            return Some(outer.clone());
        }
        // 바깥 칸 번호를 그대로 물은 것이거나, 첫 프레임 전의 새 셸이다.
        self.panes.contains_key(pty_id).then(|| pty_id.to_string())
    }

    /// `outer_for_pty` 의 반대 — 바깥 pane 이 **지금 보여 주는 탭**의 pid.
    ///
    /// 학생 관련 상태(`pane_character`·`pane_claude_sid`·`pane_cwd_cache`…)는 전부
    /// **탭 pid** 로 기록된다(`assign_character_env` 가 spawn 하는 pane 마다 그 pid 로
    /// 쓴다). 그런데 그리는 쪽은 BSP leaf(=outer)를 들고 있어, 접지 않으면 탭으로 띄운
    /// 학생의 색·프사·이름이 **아무 데도 안 나온다**(사용자 2026-08-07: "탭안에서
    /// 생성하면 학생테마가안먹네"). PTY 조회의 `pty_for_pane` 과 짝이다.
    ///
    /// 탭이 아직 pid 를 못 받은 순간(첫 ScreenUpdate 전)엔 outer 를 그대로 돌려준다 —
    /// 단일 탭 pane 은 그 둘이 같은 값이라 무해하다.
    pub(crate) fn active_tab_pid(&self, outer: &str) -> String {
        self.panes
            .get(outer)
            .and_then(|p| p.tabs.get(p.active_tab).and_then(|t| t.pid.clone()))
            .unwrap_or_else(|| outer.to_string())
    }

    /// 주소로 받은 `%N` 에 글을 쓸 PTY. 그 번호의 PTY 가 살아 있으면 **그것뿐**이다.
    ///
    /// 칸 번호와 첫 탭 PTY 번호가 같아서, 칸 번호로 활성 탭을 고르면 뒤 탭(`%9`, 학생)에 보낸
    /// 브리프가 같은 칸 앞 탭(`%5`, 서버가 돌던 셸)에 들어가 서버를 멈추는 순간 셸 명령으로
    /// 실행됐다(2026-10-09). 칸 번호만 남은 자리는 탭이 하나일 때만 그 탭으로 잇고, 탭이 여럿이면
    /// 어느 탭인지 모르므로 거절한다.
    fn surface_pty(&self, surface: &str, live: impl Fn(&str) -> bool) -> Result<String, String> {
        if live(surface) {
            return Ok(surface.to_string());
        }
        let pane = self.panes.get(surface).ok_or_else(|| format!("surface {surface} 없음"))?;
        match pane.tabs.as_slice() {
            [] => Ok(surface.to_string()),
            [only] => Ok(only.pid.clone().unwrap_or_else(|| surface.to_string())),
            tabs => Err(format!(
                "칸 {surface} 에 탭이 {}개다 — 받을 탭의 번호(where 의 탭 줄 %N)로 보내라",
                tabs.len()
            )),
        }
    }

    /// Locate `(outer_pane, tab_index)` for a backend pty id. Used by
    /// `pump_pty_screens` to write the right tab's content even when the
    /// update came from a non-active or secondary-tab shell.
    ///
    /// 그 pid 의 탭도, pid 를 아직 못 받은 탭도 없으면 None — 번호만 같은 남의 칸의 첫 탭에
    /// 이 PTY 화면을 쓰지 않는다.
    fn find_tab_by_pty<'a>(&'a mut self, pty_id: &str) -> Option<(&'a mut PaneState, usize)> {
        let outer = self.outer_for_pty(pty_id)?;
        let pane = self.panes.get_mut(&outer)?;
        let idx = pane
            .tabs
            .iter()
            .position(|t| t.pid.as_deref() == Some(pty_id))
            .or_else(|| pane.tabs.iter().position(|t| t.pid.is_none()))
            .or_else(|| pane.tabs.is_empty().then_some(0))?;
        Some((pane, idx))
    }
}

/// Cross-thread wakeup. The PTY ScreenUpdate thread can't reliably wake
/// a parked `WaitUntil` via `request_redraw` on macOS (winit defers the
/// paint to the deadline), so it sends this through the EventLoopProxy
/// instead — winit delivers it as a `user_event` that wakes the loop
/// immediately, so a committed Hangul echo / backspace / space paints
/// without the ~0.5s blink-cadence lag.
#[derive(Debug, Clone)]
enum UserEvent {
    AccountSyncApply(kasa_mcp::account_sync::PendingApply, std::sync::mpsc::Sender<Result<(), String>>),
    VaultPicked { owner: WindowId, root: String },
    VaultGraph { owner: WindowId, generation: u64, request_generation: u64, request_id: String, graph: vault_graph::Graph },
    VaultListings { owner: WindowId, generation: u64, listings: Vec<vault::Listing> },
    VaultSearch { owner: WindowId, generation: u64, query: String, request_id: String, entries: Vec<vault::Entry>, error: Option<String> },
    VaultDocument { owner: WindowId, generation: u64, relative: String, path: String, text: std::result::Result<String, String> },
    RichDocument { owner: WindowId, message: String },
    Redraw,
    CloseGraceExpired,
    MirrorCloseDone {
        action: PendingClose,
        targets: Vec<mirror_close::MirrorTarget>,
        result: Result<(), String>,
    },
    /// bg-agents 폴러가 `sessionId→parentSessionId` 맵을 갱신했다 — 포크/백그라운드
    /// 세션의 부모 학생 상속을 재적용하라는 신호. 폴러는 3초 주기라 세션 바인딩
    /// (SocketSessionBound) 시점엔 맵이 비어 상속을 놓친다 → 맵이 채워지면 이
    /// 이벤트로 pane_claude_sid 를 다시 훑어 뒤늦게 부모를 물려준다.
    BgAgentsChanged,
    /// A background git op (push/pull/commit) finished — clears the panel's
    /// spinner. Carries nothing: only one git op runs at a time.
    GitOpDone,
    /// 다른 기기 레포에 시킨 깃 동작이 실패했다 — 사유를 토스트로.
    GitOpFailed(String),
    /// Local cmux socket backend → GUI delegation. The socket server runs on
    /// its own thread and can't touch `self.pty` (not Arc<Mutex>), so it routes
    /// pane writes / split / focus to the GUI thread via the proxy. `surface_id`
    /// None = active pane, Some = that PTY (`Workspace::surface_pty`), not its pane's active tab.
    SocketBytes(Option<String>, Vec<u8>),
    SafeTellWake,
    SafeTellReady(tell_delivery::Commit),
    SafeTellCommit(tell_delivery::Commit),
    TellWaiting(HashMap<String, (usize, kasa_socket::tell::Hold)>),
    /// Split delegated from the socket thread. The `Sender` carries the new
    /// pane's real id back so `split_surface` can return it instead of the old
    /// `"pane-new"` placeholder — without it the teammate launcher targets a
    /// non-existent pane and its `send-keys` payload is dropped. The `bool` is
    /// `focus`: false (CLI/automation default) keeps focus on the current pane,
    /// true follows into the new one.
    /// 세 번째 필드는 **쪼갤 pane**. None 이면 포커스된 pane 을 쪼갠다(GUI 와 같은
    /// 뜻). 에이전트가 자기 pane 에서 부를 때는 자기 id 를 실어 보낸다 — 안 그러면
    /// 사람이 보고 있는 창이 쪼개진다(사용자: "자꾸 내가 포커스하는 윈도우에 띄우냐").
    /// 네 번째 필드는 회신 채널 — `Ok(새 pane id)` 아니면 `Err(사유)`. 사유를
    /// 실어야 소켓이 `ok:false` 로 답할 수 있다. 빈 문자열을 성공으로 실어 보내면
    /// 호출자가 실패를 감지할 방법이 없다(사용자 실사고 2026-08-05).
    /// 첫 번째 필드가 `None` 이면 **auto** — GUI 스레드가 쪼갤 pane 의 종횡비를 보고
    /// 긴 축을 고른다. 소켓 스레드는 pane 픽셀 크기를 모르므로 거기서 못 정한다.
    SocketSplit(
        Option<kasa_pty::SplitDir>,
        bool,
        Option<String>,
        std::sync::mpsc::Sender<std::result::Result<String, String>>,
    ),
    SocketServer(
        serde_json::Value,
        std::sync::mpsc::Sender<std::result::Result<serde_json::Value, String>>,
    ),
    /// 원격 PTY 호스트(`kasa-serve-web`)의 세션을 pane 으로 앉힌다 — 스폰(원격
    /// id 없음) 또는 이어받기. (base 또는 기계 이름, 원격 cwd, 원격 pane id, 기준
    /// pane, 그 자리에서 갈아끼우기, 회신)
    SocketRemotePane(
        String,
        Option<String>,
        Option<String>,
        Option<String>,
        bool,
        Option<String>,
        std::sync::mpsc::Sender<std::result::Result<String, String>>,
    ),
    /// pane 을 로컬 상주 데몬으로 **무중단 승격** — (pane, 회신=원격 세션 id).
    SocketPromote(
        String,
        std::sync::mpsc::Sender<std::result::Result<String, String>>,
    ),
    /// `to ..`(옛 `book`) — 원격 셸 거울을 **그 자리에서** 로컬 셸로 되돌린다.
    /// 원격 셸 안에서 셰임이 예약 알림(OSC 777 `kasaterm-home`)을 뱉고, 그 pane 을
    /// 소유한 이 앱의 화면 펌프가 그걸 잡아 올린다. 회신 없음 — 셰임을 부른 CLI 는
    /// 그 원격 셸의 자식이라 함께 걷힌다(성공의 표시는 화면이 로컬로 바뀌는 것).
    SocketBringHome(String),
    /// pane 의 claude 를 다른 기계로 **이사** — (pane, base, 원격 cwd, force,
    /// 태생 실행 명령, 회신=원격 세션 id). 대화 jsonl 운반 + 같은 자리 원격 스왑 +
    /// resume 주입. 태생 실행 명령(run)은 에이전트 없는 셸 pane 의 태생 스폰에서만
    /// 뜻이 있다 — `mini codex` 처럼 claude 아닌 것을 저쪽에서 돌릴 때.
    SocketMigrate(
        String,
        String,
        Option<String>,
        bool,
        Option<String>,
        std::sync::mpsc::Sender<std::result::Result<String, String>>,
    ),
    /// `surface.migrate` 의 역방향(`base: "local"`) — 원격 pane 을 이 기계로.
    SocketMigrateBack(
        String,
        Option<String>,
        bool,
        std::sync::mpsc::Sender<std::result::Result<String, String>>,
    ),
    /// 이사 워커의 단계 보고 — (pane, 단계 번호, 상태, 한 줄 메모).
    MigrateStage(String, usize, state::MigrateStageState, String),
    /// 이사 워커가 끝났다 — Ok 면 GUI 가 자리를 갈아끼우고 켠다(`migrate_finish`).
    MigrateDone(
        String,
        std::result::Result<Box<session::MigrateReady>, String>,
    ),
    RemoteShellReady(Arc<layout::RemoteShellReady>),
    SocketMigrateToRoom(
        kasa_socket::transfer::MigrateRequest,
        std::sync::mpsc::Sender<std::result::Result<String, String>>,
    ),
    ValidateTransfer(
        kasa_socket::transfer::SessionIdentity,
        std::sync::mpsc::Sender<std::result::Result<(), String>>,
    ),
    /// `machine.unfold` — 기계 라벨 하나로 그 기계 학생 pane 전부를 거울로
    /// **펼친다**(방마다 새 창). (라벨, 회신=요약 문장).
    SocketUnfold(
        String,
        std::sync::mpsc::Sender<std::result::Result<String, String>>,
    ),
    /// 재접속 자동 따라잡기(복원 뒤 백그라운드 스레드)의 결과 한 줄 — GUI 스레드로
    /// 건너와 토스트가 된다. 스레드에서 `set_toast` 를 직접 못 부르는 자리의 통로.
    RepoCatchup(String),
    /// `/term/panes` cwd 폴백 — pane 별 셸 cwd 를 GUI 스레드에서 읽어 회신.
    /// (`App.pty` 의 셸 pid 가 GUI 소유라 소켓 스레드가 직접 못 읽는다.)
    SocketPaneCwds(std::sync::mpsc::Sender<Vec<(String, String)>>),
    /// `surface.split_fleet` 위임 — pane 여러 개를 **한 번에** 배치한다.
    ///
    /// `SocketSplit` 을 N 번 보내는 것과 결과가 다르다. 그러면 회차마다 대상이 직전에
    /// 만든 pane 으로 이어져 몫이 1/2 → 1/4 → 1/8 로 반감한다 — 그게 지금 고치는
    /// 것이다. 여기서는 pane 을 다 낳은 **뒤에** 트리를 한 번 갈아 끼운다.
    ///
    /// GUI 스레드여야 하는 이유는 `SocketSplit` 과 같다: `App.pty_layout` 이 GUI
    /// 소유이고, 축 판정에 필요한 셀 픽셀 크기도 여기만 안다.
    ///
    /// `(인원, 기준 pane, 호스트 몫, 회신)`. 회신은 **실제로 앉힌** pane id 들이다 —
    /// 하한에 걸려 요청보다 적을 수 있어 부른 쪽이 개수를 대조해야 한다.
    SocketSplitFleet(
        usize,
        Option<String>,
        f32,
        std::sync::mpsc::Sender<std::result::Result<Vec<String>, String>>,
    ),
    /// 되살리기 목록(`closed_panes`)을 읽거나 그중 하나를 **진짜 끈다**.
    ///
    /// 왜 소켓에 뚫는가: 닫은 pane 은 **죽지 않는다**(`alive`). 프로세스를 그대로 물고
    /// 되살리기 목록에 앉아 있어서, 오케스트레이터가 `dismiss` 로 학생을 정리해도 그
    /// claude 들은 계속 살아 있다 — 그런데 그 목록은 App 필드라 CLI 에서 보이지도, 끄지도
    /// 못했다(사용자 2026-08-06: "너도 보여? 너도 진짜 끄게 할 수 있는지").
    ///
    /// `Some(pane_id)` 면 그 항목을 버린다(살아 있으면 프로세스까지). `None` 이면 조회만.
    /// pane id 로 지목하는 이유: 인덱스는 목록이 바뀌면 다른 항목을 가리킨다 —
    /// 조회와 종료 사이에 pane 하나만 더 닫혀도 **엉뚱한 학생을 죽인다**.
    SocketClosedPanes(
        Option<String>,
        std::sync::mpsc::Sender<std::result::Result<serde_json::Value, String>>,
    ),
    /// 부른 pane **안에 새 탭**을 연다(쪼개지 않는다). 첫 필드가 없으면 포커스된
    /// pane. 회신은 새 탭의 pane id — 부른 쪽이 거기에 명령을 실어야 한다.
    ///
    /// split 과 나뉘는 이유: 학생을 하나 더 띄울 때마다 쪼개면 화면이 계속 줄어든다.
    /// 탭은 자리를 안 뺏는다(사용자 2026-08-05).
    ///
    /// 둘째(bool)는 새 탭을 **활성탭으로 올릴지**다. 소켓 스폰의 기본은 false —
    /// 오케스트레이터가 자기 pane 에 서브에이전트를 띄울 때 새 탭이 앞으로 오면
    /// 사람이 보던 화면(부모의 대화)이 통째로 덮인다(사용자 2026-08-18: 서브에이전트는
    /// 본인 탭 안에, 화면은 그대로).
    SocketNewTab(
        Option<String>,
        bool,
        std::sync::mpsc::Sender<std::result::Result<String, String>>,
    ),
    /// pane 을 **다른 pane 옆으로** 옮긴다 — 그 대상이 다른 창에 있으면 창을 건너뛴다
    /// (PTY 는 안 죽는다, 트리만 옮겨 붙는다). 셋째는 놓을 방향.
    SocketMovePane(
        String,
        String,
        crate::DropZone,
        std::sync::mpsc::Sender<std::result::Result<String, String>>,
    ),
    /// 새 창(사이드바에 하나 더). 창 간 이동(`surface.move`)의 목적지를 만들 때 쓴다 —
    /// 만들 수단이 없으면 그 기능 자체를 CLI 에서 못 쓴다.
    SocketNewWindow,
    SocketFocus(String),
    /// tmux control-mode가 보고한 실제 활성 pane 변경. 백엔드 스레드가 App 의
    /// IME 소유권을 만질 수 없으므로 GUI 이벤트로 건너와 함께 전환한다.
    BackendFocus(String),
    /// 데스크톱 알림 배너를 눌렀다 — 그 pane 으로 간다(`SocketFocus` 와 같은 길).
    ///
    /// `sid` 는 **알림을 쏜 시점의** claude 세션이다. surface id 는 재사용되므로,
    /// 그 사이 pane 이 닫히고 번호가 새 셸에 넘어갔으면 여기서 걸러진다 — 엉뚱한
    /// 자리로 끌려가는 것보다 아무 일도 안 일어나는 편이 낫다.
    NotifyFocus {
        pane: String,
        sid: Option<String>,
    },
    /// 활성 pane 의 shell OS pid 질의(socket 스레드 → GUI 동기 RPC,
    /// SocketSplit 의 Sender 패턴). GET /mode 등 방 판정이 쓰는
    /// `Backend::active_cwd` 용 — GUI 는 메모리 조회(active_pane→shell_pid)만
    /// 즉답하고, 느린 lsof(cwd 해석)는 backend 스레드가 한다(GUI 블록 금지).
    SocketQueryActivePid(std::sync::mpsc::Sender<Option<u32>>),
    /// 모든 pane 의 `(surface_id, shell_pid)` 질의(SocketQueryActivePid 의 전체판).
    /// hook-free transcript 발견(`collab_board`)이 쓴다 — 우리는 PTY 를 소유하니
    /// 셸 pid 만 알면 backend 스레드가 claude 자식·cwd·session 을 직접 찾아 bind 한다
    /// (claude 훅에 의존하지 않음). GUI 는 메모리 조회만 즉답, lsof/ps 는 backend.
    SocketQueryPanePids(std::sync::mpsc::Sender<Vec<(String, u32)>>),
    /// `GET /sessions` 위임 — 로컬 PTY 모드의 '방' = App 윈도우. PtyBackend 는 App
    /// 상태를 직접 못 봐(별 스레드) `SocketQueryPanePids` 패턴으로 질의: 응답은
    /// (윈도우 수, 활성 idx, [(name, cwd)] 라벨). arona-ui 좌측 방 네비가 쓴다.
    SocketQuerySessions(std::sync::mpsc::Sender<(usize, usize, Vec<(String, String)>)>),
    /// 재시작 계획용 사실 — 바쁜 학생·미저장 편집기는 GUI 스레드만 안다.
    SocketRestartFacts(std::sync::mpsc::Sender<kasa_socket::app_restart::Facts>),
    /// 승인된 재시작 작업을 받아 도우미를 띄웠다 — 이제 정상 종료(`exiting`)로 끈다.
    RestartExit(String),
    /// 확인·준비한 업데이트를 도우미에 넘겼다 — 이제 정상 종료로 끄면 도우미가 갈아 끼운다.
    UpdateExit(String),
    /// `POST /session-switch?idx=N` 위임 — 보이는 윈도우를 idx 로 전환(사용자: GUI 에서
    /// 방=윈도우 클릭 시 그 터미널 윈도우로). `switch_window` 가 resize·redraw 자체 처리.
    SocketSwitchSession(usize),
    /// `POST /session-new?character=<name>` 위임 — 새 방(윈도우, 빈 셸) + 선택 캐릭터 라벨.
    /// 자동통솔 폐기(06-18)로 claude 자동 스폰 없음. `new_room_with_character` 가 처리.
    SocketNewRoom(String),
    /// `POST /spawn-student?character=<name>` 위임 — 현재 방에 캐릭터 지정 학생 추가
    /// (split + pending_character). 아로나/프라나도 학생처럼 고를 수 있다(사용자).
    /// Sender 로 새 pane id 를 돌려준다(SocketSplit 패턴) — 디스패처가 스폰 직후
    /// 그 pane 에 브리프를 쏘려면 주소가 필요하다. 빈 문자열 = pane 미생성.
    SocketSpawnStudent(String, std::sync::mpsc::Sender<String>),
    /// 다른 기계의 `to` 가 비출 맨 셸 pane — (cwd, 회신=새 pane id).
    SocketSpawnShell(Option<String>, std::sync::mpsc::Sender<String>),
    /// 자리를 지정한 셸 pane(새 방·pane 옆·탭) — 다른 기계의 보기 창이 같은 자리에 세울 때.
    SocketSpawnShellAt(kasa_socket::backend::SpawnShellAt, std::sync::mpsc::Sender<kasa_socket::backend::SpawnShellReply>),
    /// 다른 기기 칸을 이 기기 칸의 거울 탭으로(`collab.act attach`). anchor 가 없으면 보던 칸, 회신=새 거울 칸 번호.
    SocketMirrorPane {
        label: String,
        remote_id: String,
        name: String,
        cwd: String,
        anchor: Option<String>,
        focus: bool,
        reply: std::sync::mpsc::Sender<std::result::Result<String, String>>,
    },
    /// CLI `window-new --machine` — 저쪽에 새 방을 만들고 여기 보기 창으로(GUI 스레드).
    RemoteNewRoom(String, std::sync::mpsc::Sender<std::result::Result<(String, kasa_socket::backend::SeatRoom), String>>),
    TransferSnapshot((String, String), std::sync::mpsc::Sender<std::result::Result<kasa_socket::transfer::MachineSnapshot, String>>),
    TransferPrepareSpawn(kasa_socket::transfer::SpawnRequest, std::sync::mpsc::Sender<std::result::Result<transfer_endpoints::SpawnPlan, String>>),
    TransferFinishSpawn(Arc<transfer_endpoints::Spawned>, (String, String), std::sync::mpsc::Sender<std::result::Result<kasa_socket::transfer::SessionRow, String>>, bool),
    TransferReclaimSpawn(kasa_socket::transfer::SessionIdentity),
    TransferClose(kasa_socket::transfer::SessionIdentity, std::sync::mpsc::Sender<std::result::Result<(), String>>),
    /// `POST /swap-character?surface=<id>&character=<name>` 위임 — (pane, 캐릭터).
    /// 그 pane PTY 를 새 persona 로 respawn(대화 리셋, persona 는 셸 spawn 시 고정).
    SocketSwapCharacter(String, String),
    /// `GET /repersona?surface=<id>&character=<name>` 위임 — (pane, 캐릭터).
    /// respawn 없는 재배정: 학생 명령(`시로코`)이 claude 실행 직전에 호출, persona
    /// 는 래퍼의 override 파일이 싣고 GUI 는 헤더·마커·세션바인딩만 갱신.
    SocketRepersona(String, String),
    SocketAgentIdentity(String, String, String, u32, std::sync::mpsc::Sender<std::result::Result<serde_json::Value, String>>),
    /// `POST /session-close?idx=N` 위임 — 방(윈도우) 닫기(사용자). `close_window` 가
    /// 마지막 윈도우 가드·pane 정리. 닫기 실패(마지막)는 무시(프론트가 가드).
    SocketCloseRoom(usize),
    /// `GET /open-image`·`/open-markdown`(imgopen/mdopen 셰임·SendUserFile 훅)이
    /// 위임 — 그 경로를 미리보기(이미지/마크다운/텍스트)로 연다. `(path, target)`:
    /// `target` = 요청자 pane id(=pid, `$KASATERM_PANE_ID`). 있으면 그 pane 의 보조
    /// 탭으로(크롬 탭, 멀티뷰 빈-pane 회피), 없으면 active pane split 으로 폴백.
    /// 데몬 제거 때 빠졌던 open_preview 의 로컬 재구현. `open_file` 이 확장자로 분기.
    SocketOpenPreview(String, Option<String>),
    /// `surface.open_preview kind=web` — URL 을 요청 pane 옆 웹 pane 으로.
    /// (url, 요청자 pid). 파일 미리보기와 달리 winit 창 생성이 필요해
    /// `ActiveEventLoop` 가 있는 user_event 에서 처리한다.
    SocketOpenWeb(String, Option<String>),
    /// `surface.open_url` / `/open-url` — URL 을 사람이 보는 브라우저로.
    /// (url, 요청자 pid). 요청 pane 을 거울로 보는 기계가 있으면 그쪽으로 되돌린다.
    SocketOpenUrl(String, Option<String>),
    /// 원격 호스트가 거울로 되돌려 보낸 `open-url` — (로컬 pane id, url). 이 기계의
    /// 기본 브라우저로 연다(맥북에서 미니 학생의 페이지를 보는 길).
    RemoteOpenUrl(String, String),
    /// OS 닫기 확인 시트의 답 — true 면 「닫기」.
    NativeConfirm(bool),
    /// 웹뷰 안에서 친 앱 단축키(Cmd+D 분할 등). 자식 창이 key 인 동안 winit
    /// 키 이벤트는 앱에 안 오므로(WKWebView 가 first responder), 웹뷰에 심은
    /// 초기화 스크립트가 keydown 을 잡아 wry IPC → 이 이벤트로 넘긴다.
    /// `cmd` 는 "split-h"/"close"/"focus-left" 같은 동작 이름(webpane::web_pane_cmd).
    WebPaneCmd {
        host_id: u64,
        cmd: String,
    },
    /// 웹 pane 의 문서 제목 변경(wry document_title_changed) — 탭 라벨이 host
    /// 대신 페이지 제목을 쓰게 한다(Orca 탭 제목 규칙).
    WebTitleChanged {
        host_id: u64,
        title: String,
    },
    /// 웹 pane 로딩 시작/끝(wry PageLoadEvent) — 헤더 작업 바와 리로드↔정지
    /// 버튼 토글이 읽는다.
    WebLoadState {
        host_id: u64,
        loading: bool,
    },
    /// 웹뷰의 window.open/target=_blank — 그 pane 옆에 새 웹 pane 으로 받는다.
    /// (전엔 소리 없이 무동작이라 OAuth 팝업·새 탭 링크가 죽은 버튼이었다.)
    WebPopup {
        host_id: u64,
        url: String,
    },
    /// 웹뷰 다운로드 완료 — 토스트로 알린다. `path` 는 시작 때 우리가 정한
    /// 목적지(macOS 완료 콜백의 path 는 항상 비어서 못 쓴다).
    WebDownloadDone {
        path: String,
        ok: bool,
    },
    /// `web.drive` — 열린 웹 pane 을 조종한다(eval/text/shot/url). 웹뷰는 GUI
    /// 스레드 소유(!Send)라 소켓 스레드가 reply 채널로 결과를 기다린다
    /// (`SocketSpawnStudent` 와 같은 패턴). eval 의 답은 wry 콜백에서 오므로
    /// Sender 가 콜백 안까지 들어간다.
    SocketWebDrive {
        op: String,
        arg: String,
        surface: Option<String>,
        reply: std::sync::mpsc::Sender<std::result::Result<String, String>>,
    },
    /// `collab.bind_transcript`(SessionStart 훅) 위임 — (pane, 세션 id). transcript
    /// 파일명(stem) = claude 세션 id. 세션→캐릭터 영속 매핑을 조회/저장해 --resume 시
    /// 캐릭터 둔갑을 막는다(사용자: 재시작하면 프라나가 미도리로). `apply_session_character`.
    SocketSessionBound(String, String),
    /// pane 이 "보고 있는" 경로 — statusline report-cwd(claude 내부 cd 포함) 또는
    /// transcript bind 시 jsonl tail 의 cwd. 셸 pid cwd 와 달리 pane **내용**의
    /// 프로젝트를 가리켜, 파일트리 루트가 이걸 우선한다(bg-attach pane 은 셸이
    /// ~/Desktop 이라 파일트리가 pane 과 달랐던 것). `(pane, cwd)`.
    SocketViewCwd(String, std::path::PathBuf),
    /// stale statusline 재실행 강제 — 구버전 claude(≤2.1.209 실측)는 attach 에서
    /// statusline 을 재실행하지 않아 세션 id 마커가 프롬프트 전까지 안 흐른다(사용자:
    /// 들어오자마자 바뀌게). PTY 1행 지글(줄였다 원복)로 SIGWINCH 재레이아웃을 유도.
    /// 발동 게이트·rate-limit 은 backend(rebind_agents_panes)가 진다.
    NudgePaneResize(String),
    /// `surface.capture` 위임 — pane 한 칸만 잘라 PNG 로 굽는다.
    ///
    /// 소켓 스레드가 직접 못 하는 이유는 둘이다: pane 의 픽셀 사각형은 레이아웃 트리와
    /// 셀 치수를 알아야 나오고(GUI 만 쥐고 있다), 프레임버퍼 리드백은 wgpu Surface 를
    /// 만져야 한다. 그래서 좌표 계산과 무장을 GUI 스레드에서 하고, **저장이 끝난 뒤**
    /// 회신한다 — 무장하자마자 회신하면 받는 쪽이 아직 없는 파일을 열게 된다.
    /// `(pane, 저장경로 None=임시, 가로상한 0=원본, 회신)`.
    SocketCapture(
        String,
        Option<String>,
        u32,
        std::sync::mpsc::Sender<std::result::Result<serde_json::Value, String>>,
    ),
    /// `surface.close` delegated from the socket thread → `close_pane`. Local
    /// PTY mode only; the old tmux/daemon backend left this unsupported.
    SocketClose(String),
    /// `window.close` 위임 → `close_window`. 답을 돌려보낸다 — 마지막 방은 못 닫는
    /// 이유를 부른 쪽(폰의 「방 닫기」)이 그대로 보여 줘야 해서다.
    SocketCloseWindow(
        usize,
        std::sync::mpsc::Sender<std::result::Result<(), String>>,
    ),
    /// `POST /settings/character` 위임 — 웹뷰 설정이 고친 성격·이름을 굳힌다.
    ///
    /// 저장 함수(`flush_student_persona`/`flush_student_name`)를 직접 부르지 않고
    /// 네이티브가 사람 손으로 하는 3단(열기 → 버퍼 → 닫기)을 그대로 태운다.
    /// 순서가 규칙이기 때문이다 — 성격의 저장 키가 이름이라 이름부터 바꾸면 옛
    /// 이름 자리에 쓰려다 못 찾고, 뒤처리(shim 재생성·로스터 캐시 무효화)는
    /// `close_student_edit` 에 묶여 있다.
    ///
    /// **저장이 끝난 뒤** 회신한다 — 위임만 하고 바로 답하면 부른 쪽이 아직
    /// 안 써진 파일을 다시 읽는다. `(저장 요청, 회신)`.
    SocketSaveCharacter(
        kasa_socket::backend::CharacterSave,
        std::sync::mpsc::Sender<std::result::Result<serde_json::Value, String>>,
    ),
    /// `POST /settings/action` 위임 — 웹뷰 설정의 버튼 하나를 네이티브 액션으로 태운다.
    ///
    /// 액션 이름을 문자열로 받는 건 네이티브가 이미 `SettingsAction` enum 하나로
    /// 모여 있어서다. 여기서 그 이름을 그 변형으로 1:1 로 옮기기만 하므로 구현이
    /// 둘로 갈릴 수가 없다.
    ///
    /// GUI 스레드여야 하는 이유가 액션마다 다르다 — `refresh-assets` 는 GPU
    /// 텍스처를 축출하고, `select-theme` 은 그걸 안에서 부르며, 나머지는 `self` 의
    /// 편집 버퍼(`theme_label_edit`)와 설정 저장을 만진다.
    ///
    /// **끝난 뒤** 회신한다. 세 캐시(테마 해석·로스터·GPU)가 그 사이에 비워지므로,
    /// 부른 쪽이 회신을 받고 나서 다시 읽으면 새 상태가 보장된다.
    /// `(액션, 테마 id, 새 이름, 회신)`.
    SocketSettingsAction(
        String,
        Option<String>,
        Option<String>,
        std::sync::mpsc::Sender<std::result::Result<serde_json::Value, String>>,
    ),
    /// `POST /paste-image?surface=%N` — 아로나 프롬프트 입력창에 이미지 드롭(webview).
    /// 이미지 바이트를 시스템 클립보드에 비트맵으로 싣고 그 pane 에 Ctrl+V(0x16)를 보내
    /// claude 가 [Image] 칩으로 첨부하게 한다(터미널 DroppedFile 과 같은 경로). `(surface, bytes)`.
    SocketPasteImage(String, Vec<u8>, Option<std::sync::mpsc::Sender<Result<(), String>>>),
    ImagePasteDone(Result<(), String>),
    /// `POST /git-panel` — 아로나 타이틀바 버튼 → 터미널 GUI 의 git 소스컨트롤 패널 열기.
    /// 메인 터미널 창을 띄우고(숨겨져 있으면) git 컬럼을 토글한다(사용자: 그 버튼=소스컨트롤).
    SocketToggleGit,
    /// Show/hide the main terminal window, delegated from the socket thread
    /// (`POST /terminal-reveal` — the arona classroom's red-pill button).
    /// `(show, focus_pane)`: a reveal may also focus a specific pane so the
    /// classroom can jump the user to a character's seat.
    SocketRevealTerminal(bool, Option<String>),
    /// `surface.swap` delegated from the socket thread — exchange two leaves'
    /// tree positions (PTYs stay put, ids trade slots). `(a, b)`, both
    /// pre-validated to exist by the backend.
    SocketSwap(String, String),
    /// `surface.set_ratio` delegated from the socket thread — make a pane
    /// take `ratio` of its immediate split container ("오케스트레이터 pane 크게" 자동화).
    SocketSetRatio(String, f32),
    /// 거울이 끈 분할선 — 양쪽 pane 쌍들과 비율, 축. 쌍마다 그 둘의 최소 공통 조상을 고친다
    /// (정렬된 격자에선 원본의 분할선이 여러 개라 쌍도 여럿, 축이 다른 쌍은 건너뛴다).
    SocketSetRatioBetween(Vec<(String, String)>, f32, Option<kasa_pty::SplitDir>),
    /// 배치 채널의 순번 표지 `(연결, 순번)` — 앞선 배치 명령이 다 적용된 뒤에 처리된다.
    LayoutBarrier(u64, u64),
    /// `surface.rename` / `surface.set_color` delegated from the socket thread.
    /// Pane header title / accent band live in `ws.panes` which only the GUI
    /// thread may touch, so the backend routes them here. `(surface_id, title)`
    /// / `(surface_id, rgba)`.
    SocketRename(String, String),
    /// `window.rename` delegated from the socket thread. `(surface_id, title)`:
    /// rename the window/session the pane belongs to (sidebar label), not the
    /// pane header. Used by the rename override.
    SocketRenameWindow(String, String),
    SocketColor(String, [u8; 4]),
    /// `POST /session-resume` from arona-ui — open a pane and queue the resume
    /// command once its shell prompt is up. `newroom` opens a fresh window;
    /// otherwise it splits the active one. `cwd` (when set) is the session's
    /// project dir so resume lands in the right place.
    ResumeSession {
        id: String,
        cwd: Option<String>,
        newroom: bool,
        /// true → `claude attach <id>`(daemon background 세션 연결, 세션 background 유지).
        /// false → 하네스별 이어가기 명령(jsonl 새 프로세스, 과거 세션 이어가기).
        attach: bool,
        /// 그 세션을 만든 코딩 프로그램 — `claude`/`codex`/`agy`. 빈 값이면 claude.
        /// `attach` 는 claude 의 daemon 개념이라 이 값과 무관하게 claude 로 간다.
        harness: String,
        /// 실제 pane 생성 결과가 필요한 네이티브 호출자. 기존 HTTP·세션 칼럼은
        /// fire-and-forget 규약이라 None을 보낸다.
        reply: Option<std::sync::mpsc::Sender<std::result::Result<String, String>>>,
    },
    /// "대화 저장하기" — surface pane 의 foreground claude 를 ←←(agents view = bg-detach)
    /// 주입으로 background daemon 으로 detach. surface 없으면 active pane. 터미널이 꺼져도
    /// daemon 이 세션을 들고 살아남아 웹뷰에서 계속 보인다(사용자 핵심).
    SaveSession {
        surface: Option<String>,
        reply: Option<std::sync::mpsc::Sender<std::result::Result<String, String>>>,
    },
    /// A pane's claude finished (Stop hook → `kasaterm-cli notify`). Raise a
    /// desktop alert unless that pane is already the focused one, cmux-style.
    Notify {
        surface_id: String,
        title: String,
        body: String,
    },
    /// 캐릭터가 클립보드에 넣은 것을 사람에게 **보여 준다**. 클립보드는 보이지 않는
    /// 그릇이라, 알리지 않으면 붙여넣기 전까지 무엇이 담겼는지 알 수가 없다. 소켓
    /// 스레드는 토스트를 못 띄우므로(App 상태) GUI 로 넘긴다.
    SocketToast(String),
    /// A pane's claude is blocked on a permission / input prompt (its
    /// `Notification` hook → `kasaterm-cli attention`). Toast + flash the pane
    /// and, unless it's the focused pane, raise a desktop alert — the case cmux
    /// treats as its headline feature (a backgrounded agent stuck on "approve
    /// Bash?" that the transcript-board can't see).
    Attention {
        surface_id: String,
        reason: String,
    },
    /// 사용량 폴러가 "지금 계정이 임계를 넘었고, 갈 만한 다른 계정이 있다"고
    /// 판정했다. 실제 전환은 GUI 스레드 몫이다 — `settings_save` 가 shim 을 다시
    /// 깔아야 이미 열려 있는 pane 도 다음 claude 부터 새 계정으로 뜬다.
    ClaudeAccountAutoswitch {
        from: String,
        to: String,
        pct: f32,
        label: String,
    },
    ClaudeQuotaWarning(quota_alerts::QuotaAlert),
    /// 관문 사슬로 도는 Claude 로그인이 곧 끊기거나 끊겼다(`kasa_mcp::agent_chains`). (제목, 본문)
    AgentChainAlert(String, String),
    /// macOS `.md` 더블클릭(odoc Apple Event) 또는 argv → 새 워크스페이스에
    /// 마크다운 풀 뷰어. `SocketOpenPreview`(현재 창 split)와 달리 별도 탭의
    /// 단독 pane 으로 띄워 기존 작업 워크스페이스를 안 건드린다. 페이로드 = 경로.
    OpenMarkdownWindow(String),
}

/// One terminal session: its own pane set, layout, and workspace. The visible
/// session lives in App.{pty,pty_layout,ws}; the rest sit in
/// App.stashed_sessions. Each session's `ws` is its own Arc so its
/// pump_pty_screens threads keep updating it in the background even while
/// another session is on screen (tmux-style detached sessions).
#[allow(dead_code)]
struct Session {
    pty: HashMap<String, Arc<kasa_pty::PtySession>>,
    /// Layout of this session's *active* window. The other windows' layouts
    /// sit in `windows` (active slot `None`) — same stash-swap shape the
    /// session list uses one level up.
    pty_layout: Option<kasa_pty::PtyLayout>,
    /// All windows in this session by index. The active window's slot is
    /// `None` (its layout lives in `pty_layout`). Switching windows swaps a
    /// slot in/out; every window shares this session's `pty`/`ws`, so window
    /// switches never tear down panes.
    windows: Vec<Option<kasa_pty::PtyLayout>>,
    /// Index into `windows` of this session's active window.
    active_window: usize,
    ws: Arc<Mutex<Workspace>>,
}

/// One flattened row of the sidebar file tree (the expanded tree is walked
/// into a flat Vec for rendering + hit-testing). `depth` drives indentation.
struct FileNode {
    path: std::path::PathBuf,
    name: String,
    is_dir: bool,
    depth: usize,
    /// Gitignored or a dotfile — rendered italic + dim (VSCode/cursor cue).
    /// Set in a second pass by `rebuild_file_tree_nodes` (one batched
    /// `git check-ignore`), so `walk_dir` leaves it `false`.
    ignored: bool,
    /// 이 폴더가 git 저장소 루트인가 — 아이콘을 폴더 대신 브랜치로 바꿔 「어느
    /// 게 레포인지」를 목록에서 바로 읽게 한다(사용자). 워크트리는 `.git` 이
    /// 디렉터리가 아니라 gitdir 을 가리키는 파일이라 존재 여부로만 본다.
    is_repo: bool,
}

/// 이 경로가 git 저장소 루트인가. 워크트리·서브모듈은 `.git` 이 디렉터리가
/// 아니라 gitdir 을 가리키는 **파일**이라 종류를 안 따지고 존재만 본다.
/// 폴더 하나당 stat 한 번 — 파일트리는 이미 엔트리마다 file_type 을 물으므로
/// 실질 추가 비용이 없다. 파일에는 부르지 말 것.
fn is_git_repo(p: &std::path::Path) -> bool {
    p.join(".git").exists()
}

/// 화면에 띄울 한도 한 줄 — 가장 먼저 닫히는 창의 사용률, 그게 어느 창인지,
/// 그리고 그 숫자가 지금 값인지. 셋을 함께 들고 다니는 이유는 전에 percent 하나만
/// 들고 다니다 (가) 어느 창인지 몰라 0% 를 이상하게 여기지 않았고 (나) upstream 이
/// 막혀 며칠 묵은 값을 보여줘도 화면이 똑같아 보였기 때문이다(사용자 2026-08-05).
#[derive(Clone, PartialEq)]
pub(crate) struct UsageWindowBadge {
    pub(crate) label: String,
    pub(crate) pct: f32,
    /// 이 창 자체가 풀리는 시각. 다른 창의 시각으로 메우면 모델별 한도를 잘못
    /// 안내하므로, 응답에 없으면 None을 그대로 둔다.
    pub(crate) resets_at: Option<u64>,
}

#[derive(Clone, PartialEq)]
pub(crate) struct UsageBadge {
    pub(crate) pct: f32,
    /// `5h`/`7d`/`7d <모델>` — `socket::usage_pressure` 가 `limits[]` 에서 고른 라벨.
    /// 모델 스코프 주간 창은 응답의 모델명이 붙어 와서 고정 문자열이 아니다.
    pub(crate) label: String,
    /// upstream 이 막혀 마지막 성공값을 재사용한 것. 흐리게 그려 "지금 값이 아님"을
    /// 말한다 — 숨기지는 않는다(빈칸은 "한도 여유"로 오해된다).
    pub(crate) stale: bool,
    /// 이 숫자가 어느 계정 저장소의 것인가(`""` = 기본 로그인). 계정을 바꾼 직후
    /// 옛 계정 값을 새 계정 것으로 그리지 않기 위한 표식.
    pub(crate) account_dir: String,
    /// 그 창이 풀리는 시각(epoch 초). 화면은 이걸 **남은 시간**으로 바꿔 그린다 —
    /// 퍼센트만으로는 "지금 아껴야 하나 곧 풀리나"를 못 고른다(사용자 2026-08-07).
    pub(crate) resets_at: Option<u64>,
    /// 모든 한도 창. 5시간이 앞이고, 모델별 주간 창까지 각자 초기화 시각을 보존한다.
    /// 위의 `pct`/`label`은 자동 전환용 최고 압박이고, 이 목록은 상세 화면용이다.
    pub(crate) windows: Vec<UsageWindowBadge>,
}

/// 분까지만 쓴다 — 초는 매 프레임 바뀌어 눈이 그리로 끌리는데, 이 숫자로 하는 판단은
/// "지금 계정을 옮길까"라 분 단위면 충분하다.
pub(crate) fn remaining_duration_label(seconds: u64) -> String {
    let (first, first_unit, second, second_unit) = if seconds >= 86400 {
        (seconds / 86400, "일", seconds % 86400 / 3600, "시간")
    } else if seconds >= 3600 {
        (seconds / 3600, "시간", seconds % 3600 / 60, "분")
    } else if seconds >= 60 {
        return format!("{}분", seconds / 60);
    } else {
        return "곧".to_string();
    };
    if second == 0 {
        format!("{first}{first_unit}")
    } else {
        format!("{first}{first_unit} {second}{second_unit}")
    }
}

pub(crate) fn resets_in_label(resets_at: Option<u64>) -> Option<String> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    resets_in_label_at(resets_at, now)
}

fn resets_in_label_at(resets_at: Option<u64>, now: u64) -> Option<String> {
    let at = resets_at?;
    let left = at.saturating_sub(now);
    if left == 0 {
        return None; // 이미 지났다 — 다음 조회가 0% 를 실어 온다
    }
    Some(remaining_duration_label(left))
}

/// Parsed `git status` snapshot for the right-hand git column. The background
/// poller fills it from `kasa_mcp::git::git_status` (off the main thread);
/// the render reads it. Kept as a flat, render-ready struct so the gpu block
/// (which can't re-borrow `&self` to call helpers) paints straight from it.
#[derive(Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
struct GitColView {
    #[serde(skip)]
    generation: u64,
    #[serde(skip)]
    loading: bool,
    #[serde(skip)]
    issue: Option<String>,
    /// 보이는 것이 마지막으로 잘 읽은 열이고 새로 읽는 중이거나 방금 읽기가 실패했다.
    #[serde(skip)]
    stale: bool,
    #[serde(default)]
    repo_root: Option<std::path::PathBuf>,
    #[serde(default)]
    branch_list: Vec<kasa_mcp::git::GitBranch>,
    #[serde(default)]
    detached: bool,
    #[serde(default)]
    unborn: bool,
    #[serde(default)]
    head_oid: Option<String>,
    #[serde(default)]
    remote: Option<(String, String)>,
    /// cwd this snapshot was computed for — so a stale repo's rows aren't
    /// shown after a pane switch until the poller catches the new cwd.
    cwd: Option<std::path::PathBuf>,
    /// `git_status` found no repo here (home / arbitrary dir): render a soft
    /// notice instead of a branch + file list.
    no_repo: bool,
    branch: String,
    ahead: u32,
    behind: u32,
    insertions: u32,
    deletions: u32,
    clean: bool,
    /// Index (staged) changes — VSCode's "Staged Changes". `(marker, path)`
    /// where marker is `A`/`M`/`D`. Each row's - button unstages it.
    staged: Vec<(char, String)>,
    /// Worktree changes not yet staged — VSCode's "Changes". `(marker, path)`
    /// where marker is `M` modified · `?` untracked. Each row's + button
    /// stages it. A partially-staged file appears in BOTH lists.
    unstaged: Vec<(char, String)>,
    /// Local branch names for the switcher dropdown (current one is `branch`).
    branches: Vec<String>,
    /// Per-file `(insertions, deletions)` for the row's `+N -M` count, keyed by
    /// path. Filled from `git diff --numstat` (+ `--cached`).
    numstat: HashMap<String, (u32, u32)>,
    /// Most recent commits `(short_hash, subject)` for the panel's preview list.
    recent_commits: Vec<(String, String)>,
    #[serde(default)]
    commit_graph: Vec<kasa_mcp::git::GitGraphCommit>,
    #[serde(default)]
    graph_supported: bool,
    #[serde(default)]
    graph_truncated: bool,
    /// 칸 폴더가 여러 저장소를 담은 부모일 때 그 아래 저장소들(최근에 만진 순). 보이는 것은 `repo_root`.
    #[serde(default)]
    repos: Vec<std::path::PathBuf>,
}

/// 파일트리 우클릭 컨텍스트 메뉴 항목. `NewFile`/`NewFolder`/`Rename` 은 인라인
/// 입력행을 열고, `CopyPath` 는 절대경로를 클립보드로, `Reveal` 은 OS 파일매니저
/// (Finder/탐색기)에서 보여주고, `Delete` 는 선택 전체를 휴지통으로 보낸다.
#[derive(Clone, Copy, PartialEq, Eq)]
enum FtMenuAction {
    NewFile,
    NewFolder,
    Rename,
    CopyPath,
    Reveal,
    Delete,
    /// "<앱>에서 열기" — `proc::open_with_apps()` 의 인덱스. 이름이 아니라
    /// 인덱스인 건 후보가 기기마다 다르기 때문이다(설치된 것만 노출한다).
    OpenWith(usize),
    /// OS 기본 연결 프로그램으로 열기.
    OpenDefault,
}

/// 사이드바 pane 행 우클릭 메뉴 항목.
///
/// 갈래가 둘뿐이라 enum 이 과해 보이지만, 파일트리·Info 두 메뉴가 이미 같은 모양
/// (`(action, label, …)` → rect 벡터 → 실행)이라 그 골격을 그대로 쓰는 편이 항목이
/// 늘 때 싸다.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum SidebarMenuAction {
    /// 화면에서만 뗀다 — PTY 는 계속 돌고, 닫기와 달리 **정리 대상에서도 빠진다**.
    Hide,
    /// 숨겨 둔 것을 제자리로.
    Unhide,
    /// pane 닫기 — 숨기기와 달리 실제로 끝낸다.
    ClosePane,
    /// 별도 OS 창으로 뗀 pane 을 본창으로 되꽂는다(auxterm.rs).
    Dock,
    /// 방 카드 본문을 학생 줄 목록으로(모든 방 공통, settings.json `sidebar_body`).
    ListBody,
    /// 방 카드 본문을 배치도로.
    MapBody,
    /// 방 이름 편집 — 두 번 느리게 누르는 것과 같은 길.
    RenameRoom,
    /// 방 닫기 — × 와 같은 길(도는 claude 가 있으면 묻는다).
    CloseRoom,
    /// 사이드바 맨 위 현황 줄 숨기기·보이기(모든 방 공통, settings.json `sidebar_pulse`).
    TogglePulse,
    /// 「이 창 날씨」 — 이 기기 세션에만 남는 창별 덮어쓰기(weather/).
    Weather(weather::model::PaneWeather),
}

/// 한글 조합기(`App::hangul`)를 쓰는 입력 문맥. 조합기는 App 에 **하나뿐인데**
/// 이걸 쓰는 입구는 아홉 곳이라, 문맥이 바뀌어도 조합 상태가 그대로 남아 다음
/// 문맥으로 새어 나간다. 그 주인을 이 값으로 들고 다니며 바뀌는 순간 정리한다.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) enum ImeFocus {
    /// 터미널 surface(PTY id). 바깥 pane id 로 두면 같은 pane 안의 탭을
    /// 바꿨을 때 조합 중인 마지막 음절이 새 탭으로 넘어간다.
    Pane(String),
    /// 메인창 raw 편집기(pane id).
    Editor(String),
    /// Raw editor in a detached document window.
    AuxEditor(WindowId),
    GitCommit,
    /// MCP 탭의 URL 서버 추가 칸(이름·주소 두 칸을 한 문맥으로 본다 — 조합 중에
    /// Tab 으로 칸을 옮기면 그 음절은 옮기기 전 칸에 확정되는 것이 맞다).
    McpAdd,
    /// 좌측 사이드바 방 이름 인라인 편집(윈도우 인덱스).
    RoomRename(usize),
    PathSearch,
    TreeSearch,
    TreeNew,
    /// 웹 pane 헤더 주소창(한 번에 하나만 열리므로 pane id 는 `App.web_addr` 가
    /// 쥔다 — variant 에 중복으로 싣지 않는다).
    WebAddr,
    /// 웹 pane 페이지 내 찾기 칸(`App.web_find`) — WebAddr 와 같은 규칙.
    WebFind,
    /// PTY 없는 설정 방의 폼 입력. 필드를 함께 실어야 조합 중 다른 칸을 눌렀을 때
    /// 마지막 음절이 새 칸이 아니라 떠나는 칸에 확정된다.
    Settings(SettingsInput),
    /// 대화로 보는 학생 pane 의 입력칸(바깥 pane id). 터미널 surface 가 아니라서 조합
    /// 글자가 터미널 커서에 겹쳐 그려지지 않는다.
    Chat(String),
}

impl ImeFocus {
    pub(crate) fn terminal_surface(&self) -> Option<&str> {
        match self {
            Self::Pane(surface) => Some(surface),
            _ => None,
        }
    }
}

/// Action buttons at the foot of the git column. `Commit` hands the commit to
/// the active claude pane; `Pull`/`Push` sync the current branch with its
/// upstream. All shell out through `kasa_mcp::git` on a worker thread so the UI
/// never blocks. (전체 stage 버튼은 cursor 개조 때 사라졌다 — 파일 행마다
/// 개별 stage 하는 모델로 바뀌어서다.)
#[derive(Clone, Copy, PartialEq, Eq)]
enum GitColBtn {
    Commit,
    Pull,
    Push,
}

/// Items in the Commit-button split dropdown (cursor-style): plain commit,
/// commit + push, or open a PR. Wired to `git_commit_menu_rects`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum GitCommitAction {
    Commit,
    Push,
    Pull,
    CreatePr,
}

/// Clickable targets inside the Commit modal (screenshot #5).
#[derive(Clone, Copy, PartialEq, Eq)]
enum GitModalBtn {
    Close,
    IncludeUnstaged,
    Commit,
    CommitAndPush,
    Cancel,
    Confirm,
}

/// Left-nav category in the settings screen (Warp-style: list on the left,
/// form on the right). `Appearance` is the theme placeholder for phase 2.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SettingsCat {
    General,
    Appearance,
    Statusbar,
    Shell,
    Claude,
    /// 계정 — claude·codex 로그인을 넣고 갈아 끼우는 곳. `Claude`(모델 기본값)
    /// 안에 얹혀 있던 것을 제 칸으로 뺐다(2026-09-06 「Orca 랑 똑같이」): 계정을
    /// 보러 온 사람이 모델·effort·추가 인자를 지나 한참 내려가야 했고, 거기서
    /// 하는 일(로그인·제거)은 위쪽 토글들과 되돌리기 무게가 아예 다르다.
    Accounts,
    /// 기계 — ssh 로 붙는 다른 컴퓨터의 명부. 터미널의 `to <이름>` 이 여기를 읽고,
    /// 원격 pane·거울도 같은 명부를 쓴다. 손으로 json 을 고치던 것을 화면으로
    /// 올렸다(2026-09-07 지시).
    Machines,
    /// 캐릭터 세트 — 로스터·색·그림·persona 를 한 벌로 고르는 곳. 예전엔
    /// `Students`(그림 override 안내 한 줄)였는데, 테마 팩이 생기면서 「누가
    /// 나오는가」를 통째로 정하는 자리가 됐다.
    Theme,
    /// 캐릭터 한 명씩 — 목록에서 골라 성격·모델·그림을 고치는 곳. `Theme` 에서
    /// 갈라 나왔다(2026-08-26 지시): 「어느 세트를 쓸까」와 「이 애를 어떻게
    /// 고칠까」는 다른 일인데 한 화면에 쌓여 있어, 캐릭터를 고치러 온 사람이
    /// 테마 격자를 지나 한참 내려가야 했다.
    Students,
    /// 바탕화면 펫 — 창 밖에 서 있는 Live2D 캐릭터. 앱과 프로세스가 달라 kasaterm 을
    /// 내려도 남으므로, 켜고 끄는 자리와 「누가 나올까」를 여기 둔다(2026-09-07 지시).
    Pet,
    /// 날씨(리퀴드 비) — 창·판·단추에 비를 내린다. 기본 끔, 모든 항목을 사람이 고른다(docs/weather.md).
    Weather,
    /// 앱에 말을 거는 쪽 — 불편한 점을 적어 두는 곳. 다른 카테고리와 달리 설정을
    /// 바꾸지 않으므로 nav 맨 아래에 따로 떨어뜨린다.
    Feedback,
}

impl SettingsCat {
    /// 웹과의 칸 이름 대조에만 쓴다 — 값을 새로 만들 때 여기 빠뜨리면 그 칸은
    /// 대조에서 통째로 빠지므로, 변형을 더하면 이 배열도 같이 늘려라.
    /// `Weather` 만은 뺀다: 날씨는 GPU 패스라 웹 설정에 칸이 없는 네이티브 전용 칸이다.
    #[allow(dead_code)]
    pub(crate) const ALL: [SettingsCat; 11] = [
        Self::General,
        Self::Appearance,
        Self::Statusbar,
        Self::Shell,
        Self::Claude,
        Self::Accounts,
        Self::Machines,
        Self::Theme,
        Self::Students,
        Self::Pet,
        Self::Feedback,
    ];

    /// 옆 목록에 실제로 서는 칸 — `Theme` 은 「캐릭터」 밑으로 들어가 목록에서 빠졌다
    /// (2026-09-10 목업 IA: 셸+커서→터미널, 테마+캐릭터→캐릭터). 페이지 자체는
    /// 캐릭터 페이지의 「테마 관리」로 연다. 웹 대조는 그대로 `ALL` 이다.
    /// 묶음(화면·에이전트·앱)과 그 안의 순서는 자주 여는 것이 위다 — 시스템 테마를
    /// 찾는 사람이 캐릭터 칸부터 지나야 했다(2026-10-07). 묶음 머리는 `native_settings::NAV_GROUPS`.
    /// `Shell` 은 글꼴·커서가 「외형」으로 간 뒤 셸 하나만 남아 「앱 일반」에 합쳤다(2026-10-07 승인) —
    /// 값은 딥링크·웹 키로 남고, 열면 「앱 일반」이 선다(`SettingsScene::set_category`).
    pub(crate) const NAV: [SettingsCat; 10] = [
        Self::Appearance,
        Self::Statusbar,
        Self::Weather,
        Self::Accounts,
        Self::Claude,
        Self::Students,
        Self::General,
        Self::Machines,
        Self::Pet,
        Self::Feedback,
    ];

    /// 옆 목록에 실제로 서는 칸 — lite 는 「색만」이라 모양 하나뿐이다.
    pub(crate) fn nav() -> &'static [SettingsCat] {
        if crate::lite_mode() {
            &[Self::Appearance]
        } else {
            &Self::NAV
        }
    }

    /// 웹 설정(arona-ui)이 쓰는 카테고리 키. 딥링크를 URL 과 스크립트 양쪽으로
    /// 보내야 해서 이름이 한 곳에 있어야 한다 — 문자열을 부르는 자리마다 적으면
    /// 오타가 나도 **아무 일도 안 일어나** 원인을 못 찾는다(모르는 값은 무시된다).
    #[cfg(test)]
    pub(crate) fn web_key(self) -> &'static str {
        match self {
            Self::General => "general",
            Self::Appearance => "appearance",
            Self::Statusbar => "statusbar",
            Self::Shell => "shell",
            Self::Claude => "claude",
            Self::Accounts => "accounts",
            Self::Machines => "machines",
            Self::Theme => "theme",
            Self::Students => "students",
            Self::Pet => "pet",
            Self::Weather => "weather",
            Self::Feedback => "feedback",
        }
    }
}

/// 캐릭터 상세의 「원본」 뷰 상태 — 폼 대신 정의를 글로 보고 고치는 화면.
///
/// 낱개 필드로 흩지 않고 한 덩어리로 묶은 것은 `App` 이 세 층에 걸친 평면
/// struct 라 필드를 늘릴 때마다 병렬 작업이 같은 줄에서 충돌해서다 — 묶으면
/// 여기 한 줄만 는다.
#[derive(Default, Clone)]
pub(crate) struct StudentRawEdit {
    /// 원본 뷰가 열려 있나. 꺼져 있으면 렌더링된 폼.
    pub open: bool,
    /// YAML 로 보나(false = JSON).
    pub yaml: bool,
    /// 편집 버퍼. 뷰를 열 때·형식을 바꿀 때 저장된 정의로 다시 채운다.
    pub text: String,
    pub caret: usize,
    /// 마지막 저장이 실패한 이유. 있으면 편집기 아래에 뜬다 — 형식이 틀린 채로
    /// 저장하면 조용히 무시되는 대신 무엇이 틀렸는지 보여야 한다.
    pub err: Option<String>,
}

/// The two free-text fields in the settings form. Tracks which one (if any)
/// has keyboard focus so keystrokes route to its buffer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SettingsInput {
    DeviceAccountName,
    DeviceAccountPassword,
    /// 설정 「계정」 프로필 펼침의 닉네임.
    DeviceAccountNickname,
    /// 로그인 아이디 바꾸기의 새 아이디.
    DeviceAccountNewLogin,
    /// 비밀번호 바꾸기의 새 비밀번호와 확인 칸.
    DeviceAccountNewPassword,
    DeviceAccountNewPassword2,
    CwdPath,
    /// 터미널 편집기 명령줄 필드("파일 열기"가 `terminal` 일 때만 보인다).
    FileOpenCmd,
    /// 직접 적는 기본 셸 경로.
    Shell,
    /// Claude 실행에 붙일 자유 인자.
    ClaudeExtra,
    /// 캐릭터 상세 화면의 이름 칸. 어느 캐릭터인지는 `App.students_selected` 가
    /// 들고 있다 — 상세는 한 번에 한 명뿐이라 여기 실을 것이 없다.
    StudentName,
    /// 캐릭터 상세의 여러 줄 성격 입력.
    StudentPersona,
    /// Feedback 본문 멀티라인 필드. persona 와 같은 편집 경로를 타지만 캐럿·버퍼가
    /// 따로라, 설정 창을 두 카테고리로 오가도 서로 안 덮어쓴다.
    FeedbackBody,
    /// 캐릭터 정의 전체를 고치는 JSON/YAML 원본 편집기.
    StudentRaw,
    /// 나노바나나가 쓸 Gemini API 키. 저장값은 화면에 되돌리지 않고 새 입력만 둔다.
    ThemeGenKey,
    /// 커스텀 팔레트 표시명. 대상 slug 는 `App.custom_theme_label_edit` 이 든다.
    CustomThemeLabel,
    /// 기계 명부의 이름·ssh 칸. 어느 줄의 어느 칸인지는 `App.machine_edit` 이 든다.
    MachineField,
    /// 계정이 기기에 붙이는 화면 이름. 어느 기기인지는 `App.device_name_edit` 이 id 로 든다.
    DeviceName,
    /// Agent 계정 별명. 제공자와 슬롯 id 는 `App.account_label_edit` 이 든다.
    AccountLabel,
    /// 로그인 중인 슬롯에 붙여넣는 OAuth 코드. 어느 슬롯인지는 진행 중인 로그인
    /// 한 건뿐이라 실을 것이 없다(`settings::hidden_login_job`).
    LoginCode,
    /// 테마 이름 칸. 어느 테마인지는 `App.theme_label_edit` 이 폴더 id 로 들고
    /// 있다 — 이 enum 이 `Copy` 라 String 을 못 실어서다. 인덱스로 잡지 않는 건
    /// 테마를 지우면 뒷 번호가 밀려 엉뚱한 폴더를 고치게 되기 때문이다.
    ThemeLabel,
    /// 팔레트 hex 편집 필드. 인덱스는 `theme::PALETTE_KEYS` 순서(0..11)이고,
    /// 그 뒤(11..27)는 ANSI 0..16 — 이 enum 이 Copy 라 키 문자열 대신 번호로
    /// 싣는다. 버퍼는 `App.set_palette_edit` 하나를 같이 쓴다(한 번에 한 칸).
    PaletteHex(usize),
    /// 기기색 hex 편집 필드. 인덱스는 `pane_identity::device_color_rows()` 순서
    /// (이 기기가 0, 그 뒤 명부 순). 버퍼는 팔레트와 같은 `App.set_palette_edit`.
    DeviceHex(usize),
}

/// Clickable targets painted into the settings screen, collected each frame for
/// hit-testing. String-carrying variants (shell presets) keep this `Clone`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SettingsAction {
    DeviceAccount(native_settings::device_account::Action),
    PreferredAgent(&'static str),
    AgentPermission(&'static str, &'static str),
    AgentStatusline(&'static str, bool),
    AgentStatuslineCustom(bool),
    /// 설정과 웹 화면이 쓸 언어(`ko`/`en`).
    UiLanguage(&'static str),
    /// 「카사크롬이 쓰는 크롬」 — 명부의 기계 라벨, 빈 문자열=이 기계.
    ChromeMachine(String),
    /// 사람에게 보여 줄 페이지를 폰(쪽지+알림)으로. 크롬 선택은 건드리지 않는다.
    OpenOnPhone(bool),
    CwdMode(&'static str),
    /// 파일트리에서 파일을 열 때 쓸 방식 — `"builtin"` · `"app"` · `"terminal"`.
    FileOpenMode(&'static str),
    /// `"app"` 모드가 쓸 앱 이름. 빈 문자열 = OS 연결 프로그램.
    FileOpenApp(String),
    /// 최소 대비 프리셋 이름 (`theme::CONTRAST_PRESETS`).
    MinContrast(&'static str),
    ToggleFileTree,
    /// 바탕화면 펫을 켜고 끈다.
    TogglePet,
    /// 날씨 페이지의 한 번 누름.
    Weather(crate::weather::model::Change),
    /// 펫으로 띄울 캐릭터 폴더 이름(`~/.config/kasaterm/pet/<이름>`).
    PetCharacter(String),
    PetPreference(kasa_pet_config::PreferenceChange),
    ToggleFooter,
    /// Editor autosave quiet period in ms; 0 = off.
    AutosaveDelay(u64),
    ShellPreset(String),
    /// 프리셋 키 · `"system"` · `custom:<slug>`. 커스텀 키는 설정 파일에서 읽은
    /// slug 를 달고 태어나 `&'static str` 로 못 담는다.
    ThemeMode(String),
    /// System 테마가 OS 밝기별로 입을 실제 팔레트.
    ThemeSystemSlot(bool, String),
    /// 지금 팔레트를 복제해 **새** 커스텀을 목록에 더하고 그것으로 전환 —
    /// 팔레트 편집의 입구. 하던 편집은 목록에 그대로 남는다.
    StartCustomTheme,
    /// 지금 편집 중인 커스텀을 base 프리셋 값으로 다시 시드 — 편집을 처음부터.
    ResetCustomTheme,
    /// 커스텀 팔레트 하나를 목록에서 치운다(인자는 slug).
    DeleteCustomTheme(String),
    /// 커스텀 팔레트 이름을 고치는 인라인 입력칸.
    FocusCustomThemeLabel(String),
    /// 팔레트 색 한 칸의 hex 필드에 포커스(인덱스 규약은 `SettingsInput::PaletteHex`).
    FocusPaletteHex(usize),
    /// 색 선택기의 채도×명도 사각형. 좌표는 액션에 못 싣는다(연속값) —
    /// settings_click 이 히트 rect 와 커서로 상대좌표를 직접 계산한다
    /// (2026-08-13 지시: hex 타이핑 말고 마우스로 고르게).
    PickerSV,
    /// 색 선택기의 색상(Hue) 띠. 처리 방식은 PickerSV 와 같다.
    PickerHue,
    /// 화면의 한 점에서 색을 집어 현재 팔레트 칸에 넣는다.
    PaletteEyedropper(usize),
    /// 기기 한 줄의 색 칸에 포커스(인덱스 규약은 `SettingsInput::DeviceHex`).
    FocusDeviceHex(usize),
    /// 기기 색을 프리셋(#rrggbb)으로 한 번에 — 피커를 열지 않고 고르는 길.
    DevicePreset(usize, String),
    /// 기기 하나의 저장색을 지워 배정색으로 되돌린다.
    ResetDeviceColor(usize),
    /// 모든 기기의 저장색을 지운다.
    ResetAllDeviceColors,
    /// 화면의 한 점에서 색을 집어 기기 칸에 넣는다.
    DeviceEyedropper(usize),
    DeviceIcon(String, String),
    ImportDeviceIcon(String),
    Accent(String),
    /// Silhouette preset: "rounded" · "sharp" · "pixel". Its own axis, so any
    /// palette can be worn with any corner treatment.
    Shape(&'static str),
    /// 크롬 글꼴: "terminal" · "system" · 설치 글꼴 이름. 재시작 없이 바로 먹는다.
    UiFont(String),
    /// 터미널 격자 글꼴(가족 이름). 첫 실행 화면의 글꼴 고르기와 같은 길로 저장한다.
    TerminalFont(String),
    /// Font-size stepper: −1 / +1 logical px on the base cell font.
    FontSizeDelta(i8),
    /// UI 배율 스테퍼(±10%). Cmd+/− 와 같은 축이지만, 키로만 있으면 얼마나
    /// 틀어졌는지 화면에 안 보여 되돌릴 생각을 못 한다.
    UiZoomDelta(i8),
    /// 배율 100% + 폰트 기본값으로 한 번에. 둘을 따로 만지다 보면 어느 쪽이
    /// 어긋났는지 알 수 없게 되는데, 그때 돌아올 자리가 필요하다.
    ResetScale,
    /// Window-tab placement: "top" (title-strip tabs) or "side" (Warp strip).
    TabPosition(&'static str),
    /// 칸 머리: "bar"(모든 칸에 머리 띠) · "handle"(hover ⋮ 만).
    PaneHeader(&'static str),
    CursorShape(cursor::CursorShape),
    /// `SettingsAction` 이 `Eq` 를 derive 하므로 f32 를 실을 수 없다 — 굵기는 어차피
    /// 픽셀 정수라 u8 로 나른다.
    CursorThickness(u8),
    /// 터미널 셀 위 마우스 포인터 — `"arrow"` · `"ibeam"`.
    MouseCursor(&'static str),
    /// 떠 있는 pane 보호·확인 카드를 포함한 계정 전환. 빈 id는 기본 로그인.
    SwitchAccount(AccountProvider, String),
    ToggleClaudePersona,
    ToggleCharacterAppearance,
    ToggleShimInject,
    ClaudeModel(String),
    ClaudeEffort(String),
    /// 계정 추가 — 저장소 dir 을 만들고 그 dir 을 물린 로그인 CLI 를 **터미널 없이**
    /// 돌린다. 브라우저 승인만 사람이 하고, 결과는 설정 화면 그 자리에 뜬다.
    AddClaudeAccount,
    /// 이미 있는 슬롯에 로그인을 다시 돌린다. 토큰이 만료돼 「로그인 필요」로 뜬
    /// 슬롯을 되살리는 유일한 길이다 — 예전엔 지우고 새로 만드는 수밖에 없었고,
    /// 그러면 슬롯 dir 이 바뀌어 그 계정에 붙은 한도 이력까지 함께 버렸다.
    ///
    /// 마지막 칸은 승인을 **어느 브라우저**에서 받을지다. 되살리려는 계정이 이미
    /// 브라우저에 로그인돼 있으면 쓰던 창이 훨씬 짧다(`settings::LoginBrowser`).
    ReauthAccount(AccountProvider, String, settings::LoginBrowser),
    /// 다른 기기에 로그인된 계정(관문 목록의 열쇠)을 이 기기에도 새 슬롯으로 로그인한다.
    AdoptSharedAccount(AccountProvider, String),
    /// 진행 중인 숨은 OAuth 로그인과 그 브라우저 자식을 함께 멈춘다.
    CancelLogin,
    /// 계정 칸이 다룰 기계를 고른다(`true` = 본진).
    AccountScopeHome(bool),
    /// 브라우저가 준 OAuth 코드를 로그인 중인 CLI 의 stdin 으로 보낸다. 지금 로그인은
    /// localhost 콜백이 아니라 **코드 붙여넣기**로 끝나므로, 이 칸이 없으면 로그인은
    /// 영영 안 끝난다(2026-09-05 확정).
    SubmitLoginCode,
    /// 계정 슬롯 별명을 고치는 인라인 입력칸.
    FocusAccountLabel(AccountProvider, String),
    /// 계정을 목록에서 뺀다. Keychain 항목은 건드리지 않는다 — 지우면 재로그인
    /// 말고는 복구가 없고, 남겨 둬도 해가 없다.
    RemoveClaudeAccount(String),
    /// 한도가 차면 다음 계정으로 알아서 넘어가는 스위치.
    ToggleAccountAutoswitch,
    /// 기계 명부에 빈 줄을 하나 붙이고 그 이름 칸에 커서를 둔다.
    AddMachine,
    /// 기계 명부에서 그 줄을 지운다.
    RemoveMachine(usize),
    /// 그 줄의 칸을 고치기 시작한다 — `true` 면 ssh 칸.
    FocusMachineField(usize, bool),
    /// 그 기기(id)의 화면 이름을 고치기 시작한다.
    FocusDeviceName(String),
    /// 빌드가 다른 기계 한 대에 현재 dist를 보낸다. 값은 명부의 정확한 ssh 대상.
    SyncMachine(String, String),
    /// 하단바에 **안 쓰는 계정의 한도까지** 세우는 스위치.
    /// 그 전환을 부르는 사용률(%).
    AccountAutoswitchPct(u32),
    /// PixelDelta 스크롤 감도 배율 ×100. 트랙패드와 고해상도 마우스휠이 같은
    /// 델타로 와 구분이 안 되므로 사람이 고른다.
    WheelPixelGain(u32),
    /// 창 맨 아래 상태줄 높이(logical px). 슬라이더가 아니라 프리셋인 것은 1px
    /// 단위로 고를 값이 아니어서다 — 안에 얹힌 게이지·칩 크기가 정해져 있어,
    /// 쓸 수 있는 폭이 사실상 세 칸이다.
    StatusBarH(u32),
    /// pane 하단바(경로·브랜치·diff 칩) 높이(logical px).
    PaneFooterH(u32),
    ToggleStatusbarItem(String),
    MoveStatusbarItem(String, i8),
    /// 빈 색은 해당 위젯의 테마 기본색으로 복귀한다.
    SetStatusbarColor(String, String),
    ToggleStatusbarUsageField(String, String),
    ToggleStatusbarSeparators,
    ResetStatusbar,
    /// 위 넷의 codex(ChatGPT) 판. 로그인 수단이 달라 동작이 갈리므로(claude 는
    /// `claude auth login`, codex 는 `CODEX_HOME=<슬롯> codex login`) 액션도 가른다.
    AddCodexAccount,
    RemoveCodexAccount(String),
    /// Open `~/.config/kasaterm/students/` in the OS file manager so the user
    /// can drop replacement character images there.
    OpenStudentsDir,
    /// Open `~/.config/kasaterm/characters.json` in the default editor to edit
    /// names / colors / persona text.
    OpenCharactersJson,
    /// 캐릭터 테마를 갈아 끼운다 — 빈 문자열이면 번들. 로스터·색·그림이 한꺼번에
    /// 바뀌므로 캐시 무효화가 짝으로 따라붙는다.
    SelectTheme(String),
    /// 새 테마를 만든다 — 지금 로스터와 그림을 `themes/<id>/` 로 복제해 채운다.
    /// 79명치 JSON 과 파일명 규칙을 맨손으로 맞추는 건 현실적인 경로가 아니라,
    /// 빈 껍데기가 아니라 채워진 본보기를 만든다.
    ExportTheme,
    /// 그 테마의 폴더를 파일 관리자로 연다(그림·json 을 손으로 갈아 끼우는 자리).
    OpenThemeDir(String),
    /// 그 테마를 목록에서 치운다 — 지우지 않고 `themes/_trash/` 로 옮긴다.
    DeleteTheme(String),
    /// 그 테마의 이름 칸에 포커스를 준다. 인자는 폴더 id.
    FocusThemeLabel(String),
    /// 테마 카드 아래에서 명단을 펼치거나 접는다.
    InspectTheme(String),
    /// 테마 한 벌의 고른 명단을 전원/기본값으로 바꾼다.
    ThemePickAll(String, bool),
    /// 테마 한 벌에서 캐릭터 한 명을 켜거나 끈다.
    CharacterPick(String, String, bool),
    /// Evict cached character textures so edited images reload on next paint.
    RefreshStudentAssets,
    /// Select a character in the Students list → load its persona into the edit
    /// buffer. Carries the character's display name.
    SelectStudent(String),
    /// HTTP/테마 카드가 연 정확한 `(테마, 이름)` 상세.
    SelectStudentInTheme(String, String),
    /// 캐릭터 상세를 닫고 목록으로 돌아간다(편집 중이던 것은 저장하고).
    CloseStudent,
    /// 상세 화면의 이름 칸에 포커스.
    FocusStudentName,
    /// 상세를 「렌더링됨 ↔ 원본」으로 전환한다.
    ToggleStudentRaw(bool),
    /// 원본 뷰의 표기를 JSON ↔ YAML 로 바꾼다. 버퍼를 저장된 정의로 다시 채우므로
    /// 고치던 것은 사라진다 — 형식만 바꾸면서 편집 상태까지 옮기려면 한쪽으로
    /// 파싱해 다른 쪽으로 다시 쓰는 왕복이 필요한데, 문법이 깨진 중간 상태에서는
    /// 그게 불가능하다. 사라지는 쪽이 조용히 어긋나는 것보다 낫다.
    StudentRawFormat(bool),
    /// 원본 JSON/YAML 버퍼를 검증해 로스터에 저장한다.
    SaveStudentRaw,
    /// 열려 있는 캐릭터가 쓸 모델을 고른다 — `(--model 값, 실행 통로)`. 둘을 함께
    /// 나르는 이유는 축이 달라서다: 게이트웨이 모델은 `--model` 로 못 닿고 래퍼가
    /// 환경을 씌워야 하므로, 한 칸을 골라도 저장할 필드가 둘이다.
    StudentModel(String, String),
    /// 진단 정보(버전·OS·창 구성)를 같이 남길지. 기본 켬 — 없으면 대부분의
    /// 제보가 "어느 버전에서요?"로 한 번 더 왕복한다.
    ToggleFeedbackDiag,
    /// 피드백을 파일로 굳힌다. 보낼 곳이 아직 없어서, 나가는 게 아니라 쌓인다.
    SaveFeedback,
    SendFeedback,
    /// 쌓인 피드백 폴더를 파일 관리자로 연다.
    OpenFeedbackDir,
    /// 그림 생성 엔진을 고른다 — `"opengateway"` · `"codex"` · `"nanobanana"`.
    /// 정본은 settings.json 의 `theme_gen_provider`.
    ThemeGenProvider(String),
    /// 선택한 캐릭터의 참조 그림으로 생성 잡을 시작한다.
    ThemeGenStart,
    /// 다음에 놓는 그림이 바꿀 모션/프레임 칸.
    SelectMotionFrame(String, usize),
    /// 사용자 모션 한 벌을 걷고 번들 그림으로 되돌린다.
    ResetMotion(String),
}

/// Which dropdown a pane's status bar has open. `Path` lists the cwd's sibling
/// directories (click → cd that pane); `Branch` lists local branches (click →
/// checkout in that pane's repo).
#[derive(Clone, Copy, PartialEq, Eq)]
enum StatusbarMenu {
    Path,
    Branch,
}

/// Recompose conjoining Hangul jamo (NFD) back into precomposed syllables
/// (NFC). macOS returns filenames decomposed, so a Korean name like "한글"
/// arrives as `ㅎ ㅏ ㄴ ㄱ ㅡ ㄹ` and renders as scattered jamo / boxes. This
/// is the canonical Hangul composition (L+V[+T]) only — no full Unicode table,
/// since the visible breakage is Hangul-specific. Non-Hangul codepoints pass
/// through untouched, so a string with no jamo returns an identical copy.
fn nfc_hangul(s: &str) -> String {
    const S_BASE: u32 = 0xAC00;
    const L_BASE: u32 = 0x1100;
    const V_BASE: u32 = 0x1161;
    const T_BASE: u32 = 0x11A7;
    const L_COUNT: u32 = 19;
    const V_COUNT: u32 = 21;
    const T_COUNT: u32 = 28;
    const N_COUNT: u32 = V_COUNT * T_COUNT;
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        let c = ch as u32;
        // Trailing jamo onto an already-composed LV syllable → LVT.
        if (T_BASE + 1..T_BASE + T_COUNT).contains(&c) {
            if let Some(prev) = out.chars().last() {
                let p = prev as u32;
                if p >= S_BASE && (p - S_BASE) % T_COUNT == 0 && p < S_BASE + L_COUNT * N_COUNT {
                    out.pop();
                    if let Some(syl) = char::from_u32(p + (c - T_BASE)) {
                        out.push(syl);
                        continue;
                    }
                }
            }
        }
        // Vowel jamo onto a leading consonant → LV syllable.
        if (V_BASE..V_BASE + V_COUNT).contains(&c) {
            if let Some(prev) = out.chars().last() {
                let l = prev as u32;
                if (L_BASE..L_BASE + L_COUNT).contains(&l) {
                    out.pop();
                    let syl = S_BASE + ((l - L_BASE) * V_COUNT + (c - V_BASE)) * T_COUNT;
                    if let Some(syl) = char::from_u32(syl) {
                        out.push(syl);
                        continue;
                    }
                }
            }
        }
        out.push(ch);
    }
    out
}

/// Map a file extension to the syntax-highlighter language name (the same
/// names `syn_keywords`/`syn_line_comment` match on). Unknown → "" (the
/// highlighter still colors strings/numbers/comments generically).
fn code_lang_for_path(p: &std::path::Path) -> &'static str {
    match p
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "rs" => "rust",
        "py" | "pyi" | "pyw" => "python",
        "js" | "mjs" | "cjs" | "jsx" => "javascript",
        "ts" | "tsx" => "typescript",
        "go" => "go",
        "c" | "h" => "c",
        "cpp" | "cc" | "cxx" | "hpp" | "hxx" => "c++",
        "json" | "jsonc" => "json",
        "sh" | "bash" | "zsh" | "fish" => "bash",
        "sql" => "sql",
        "toml" | "lock" => "toml", // Cargo.lock is TOML
        "yml" | "yaml" => "yaml",
        "html" | "htm" => "html",
        "css" | "scss" | "sass" => "css",
        _ => "",
    }
}

/// A pane's cwd + git badge, published by the GUI for the socket thread's
/// `/layout` so the BA GUI can draw a Warp-style status bar on plain terminal
/// tiles. Both fields come from caches the GUI already maintains off the
/// lsof/git hot path (`pane_cwd_cache` / `window_git`).
#[derive(Clone, Debug)]
pub(crate) struct PaneStatus {
    pub(crate) cwd: std::path::PathBuf,
    pub(crate) badge: Option<kasa_mcp::git::GitBadge>,
    /// Shared handle to the pane's OSC 133 command blocks. The socket `/blocks`
    /// reads it directly — no clone, no `App.pty` access. None for non-terminal
    /// tiles or panes whose PTY isn't tracked.
    pub(crate) blocks: Option<
        std::sync::Arc<std::sync::Mutex<std::collections::VecDeque<kasa_pty::CommandBlock>>>,
    >,
}

/// 테마 플립 때 떠 있는 claude 한 pane 을 갈아입히는 대기표 한 장
/// (`poll_claude_retheme`, input.rs). idle + 빈 입력줄이 될 때까지 기다렸다
/// `/config theme=<값>` 을 쳐 준다.
struct RethemeState {
    /// 이 시각을 지나면 포기한다 — 영원히 바쁜 pane 에 큐가 눌러붙지 않게.
    expires: Instant,
    /// `/config theme=` 뒤에 붙일 값. 플립 시점의 settings.json theme 값을
    /// 한 번 읽어 박아 둔다(입력줄에 그대로 타이핑되므로 토큰 검증을 통과한
    /// 값만 온다).
    value: String,
}

struct App {
    viewer_only: bool,
    /// KasaLite 고정판 — `ViewerLaunch::lite`. 부가 화면·서버·자기설치의 진입점이 이걸 본다.
    lite: bool,
    viewer_resumed: bool,
    web_visual: render::terminal_scene::VisualPump,
    window: Option<Arc<Window>>,
    /// Set when `KASATERM_RENDERER=gpu`. Mutually exclusive with
    /// `sugarloaf` — both own a wgpu Surface, only one can present.
    gpu: Option<gpu::GpuRenderer>,
    /// 살아 있는 rust-analyzer 하나 — **프로젝트 루트당 하나**. 첫 rust 파일을
    /// 편집기로 열 때 뜨고, 그 전엔 아예 띄우지 않는다: 인덱싱에 수십 초와 수 GB
    /// 를 쓰는 프로세스라 쓸지 모르는 채로 켜 두면 안 된다.
    lsp: Option<lsp::LspClient>,
    /// 마우스가 편집기 위에 멎어 있는 자리. 없으면 툴팁도 없다.
    hover: Option<HoverState>,
    /// 마우스가 `[Image #N]` 글자 위에 멎어 있을 때의 썸네일 툴팁.
    image_tip: Option<ImageTip>,
    /// 커서가 마지막으로 있던 (pane, 열, 행). 툴팁 후보가 바뀌었는지는 그리드를
    /// 봐야 아는데 그건 렌더만 안다 — 셀이 바뀐 프레임에만 다시 그려, 픽셀마다
    /// chrome 을 재구성하는 일을 막는다.
    image_hover_cell: Option<(String, u16, u16)>,
    /// 답을 기다리는 정의 이동 요청 id. 응답은 왕복이라 클릭한 그 자리에서
    /// 기다릴 수 없어, 틱이 `lsp_goto_pump` 로 받아 파일을 연다.
    lsp_goto: Option<i64>,
    tmux: Option<Arc<TmuxSession>>,
    /// Phase C backend. Mutually exclusive with `tmux` — exactly one
    /// is `Some` after `start_backend`. Selection driven by the
    /// KASATERM_BACKEND env var; defaults to PTY now that the Phase C
    /// path is the recommended one (no tmux daemon, no focus-events
    /// warnings from Claude Code).
    /// All live PTY sessions, keyed by pane id. Empty when running in
    /// tmux mode. Multi-pane PTY mode inserts one entry per split.
    pty: HashMap<String, Arc<kasa_pty::PtySession>>,
    /// BSP layout tree for multi-pane PTY mode. `None` in tmux mode —
    /// the tmux daemon owns the layout there and ships it via
    /// `%layout-change` instead.
    pty_layout: Option<kasa_pty::PtyLayout>,
    /// Queued `claude --resume …\n` injections for restored panes, one per
    /// claude pane, fired once each pane's shell prompt is up. Holds the
    /// PtySession Arc directly so it works for panes in any session (active or
    /// stashed background). (session, command, time-to-send).
    pending_restores: Vec<(Arc<kasa_pty::PtySession>, String, std::time::Instant)>,
    /// 지글 원복 큐 — NudgePaneResize 가 1행 줄인 pane 을 (원 cols, 원 rows)로 되돌릴
    /// 시각. pending_restores 와 같은 drain 사이클에서 시간 도달분만 발사.
    pending_unjiggle: Vec<(String, u16, u16, std::time::Instant)>,
    /// 이사 예약 — 턴 중인 학생의 이사를 여기 앉혀 두면, 턴이 끝난 것을 틱
    /// (`run_pending_migrations`)이 보고 실행한다. 턴 중 SIGTERM 이 하던 일을
    /// 자르는 것을 막는 관문(2026-08-31 「이사하고 작업이 끊겨」).
    migrate_queue: Vec<crate::session::PendingMigration>,
    /// Headless verification: clean-exit (runs `exiting` → save_session_state)
    /// at this instant when KASATERM_AUTOQUIT_MS is set. None disables it.
    autoquit_at: Option<std::time::Instant>,
    /// Pending GPU self-captures `(deadline, png path)` from KASATERM_AUTOCAPTURE_MS
    /// (콤마로 여러 시각 지정 가능 — 애니메이션 프레임 비교 검증용).
    /// `about_to_wait` arms `gpu.capture_next` once a deadline passes so the
    /// next render reads the frame back to a PNG — no screen-record permission.
    pending_capture: Vec<(std::time::Instant, String)>,
    /// `surface.capture` 회신 대기 `(png 경로, 회신 채널)`.
    ///
    /// GPU 리드백은 렌더 안에서 동기로 끝나므로(`device.poll(Wait)`), 무장한 프레임을
    /// 그린 직후 파일이 이미 있다. 그래서 렌더 뒤에 이 큐를 훑어 파일을 확인하고
    /// 회신한다 — 무장 시점에 미리 답하면 받는 쪽이 없는 파일을 Read 하게 된다.
    pending_capture_reply: Vec<(
        String,
        std::sync::mpsc::Sender<std::result::Result<serde_json::Value, String>>,
    )>,
    /// Headless git-panel demo `(deadline, action)` from KASATERM_AUTOGIT —
    /// "diff" expands the first changed file's inline diff, "modal" opens the
    /// commit modal, so those states can be self-captured without clicking.
    pending_autogit: Option<(std::time::Instant, String)>,
    /// Queued split directions driven by KASATERM_AUTOSPLIT — headless
    /// repro for the multi-pane render path. Empty in normal use.
    autosplit_plan: Vec<kasa_pty::SplitDir>,
    autosplit_at: Option<Instant>,
    /// Headless file-open repro. KASATERM_AUTOOPEN=<path> fires
    /// `open_file_split` after AUTOOPEN_MS so the preview-pane + file-tree
    /// highlight path can be screenshotted without a real double-click.
    autoopen_path: Option<std::path::PathBuf>,
    autoopen_at: Option<Instant>,
    /// Headless confirm-modal repro: deadline to fire `confirm_or_close_window`.
    autoconfirm_at: Option<Instant>,
    /// Headless tab-drag simulation. KASATERM_AUTODRAG="src:from:dst"
    /// (e.g. "%2:0:%0") fires `simulate_tab_merge` after AUTODRAG_MS so
    /// the cross-pane merge path can be verified without a real mouse.
    autodrag_plan: Option<(String, usize, String)>,
    autodrag_at: Option<Instant>,
    /// Headless repro for a cross-window pane move (KASATERM_AUTOPANEMOVE=<dst
    /// window idx>): relocates the active window's first leaf next to that
    /// window's first leaf via `move_pane`, exercising the sidebar-chip drop
    /// path without a real drag.
    autopanemove_dst: Option<usize>,
    autopanemove_at: Option<Instant>,
    /// Headless repro for the drag *preview* (KASATERM_FORCE_DRAG="%N"): parks
    /// the named leaf in an active header_drag with the cursor over a sibling,
    /// then stops — so a capture shows the floating ghost + vacated-slot scrim
    /// without committing the drop.
    force_drag_leaf: Option<String>,
    force_drag_at: Option<Instant>,
    /// Headless repro for the window sidebar: number of extra windows left to
    /// spawn (KASATERM_AUTOWINDOWS) and when the next one fires. 0 disables.
    autowindow_left: usize,
    autowindow_at: Option<Instant>,
    /// Headless repro for the sidebar toggle (KASATERM_AUTOTOGGLE_SIDEBAR_MS):
    /// flips the sidebar once at this instant so a screenshot can capture the
    /// collapsed-grid state without a human clicking the title-bar button.
    autotoggle_sidebar_at: Option<Instant>,
    /// Extra sidebar flips queued after the first (KASATERM_AUTOTOGGLE_SIDEBAR_N),
    /// 1.5s apart, to stress hide↔show reflow without a human.
    autotoggle_left: u32,
    /// Headless repro for the in-pane tab bar (KASATERM_AUTOTABS=N): pushes N
    /// dummy tabs onto the active pane once so a screenshot can capture the
    /// multi-tab header without a human clicking the "+". 0 disables.
    autotabs_n: usize,
    autotabs_at: Option<Instant>,
    /// Pane ids whose PTY reader thread has disconnected (shell exited
    /// or PTY closed). Drained on the main thread in `about_to_wait`
    /// so the tree mutation runs without holding the workspace lock
    /// across a session drop.
    dead_panes: Arc<Mutex<Vec<String>>>,
    /// Set when running as a GUI attached to a daemon (KASATERM_DAEMON=1):
    /// PTY input / resize / scroll route to the daemon over this client
    /// instead of a local PtySession. None in the default in-process mode.
    /// Arc so background timers (autosend) can hold a handle too.
    ws: Arc<Mutex<Workspace>>,
    /// All sessions (tmux-style tabs) by tab index. The visible session's slot
    /// holds `None` — its live state is the fields above (pty/pty_layout/ws).
    /// Switching swaps a slot in/out; background sessions keep running via
    /// their own ws Arc, captured by their pump_pty_screens threads.
    sessions: Vec<Option<Session>>,
    /// Index into `sessions` of the visible session (its slot is None).
    active_session: usize,
    /// Windows of the *visible* session, by index. The active window's slot
    /// holds `None` — its live layout is `pty_layout` above. A window is a
    /// pane grouping (one BSP tree); a session can hold several. Switching
    /// windows swaps `pty_layout` ↔ `windows[idx]` while `pty`/`ws` stay put,
    /// so the panes' shells keep running across the switch (same session).
    /// When the visible session is stashed, these move into its `Session`.
    windows: Vec<Option<kasa_pty::PtyLayout>>,
    /// Index into `windows` of the visible window.
    active_window: usize,
    /// Measured cell geometry from sugarloaf — see `compute_cell_metrics`.
    cell: CellGeom,
    preedit: String,
    in_preedit: bool,
    /// 마지막으로 OS 에 알린 IME 후보창 좌표(물리 px) — 안 바뀌었으면 안 부른다.
    /// macOS 는 플랫폼 IME 를 꺼서(`set_ime_allowed(false)`) 읽는 쪽이 없다.
    #[cfg_attr(target_os = "macos", allow(dead_code))]
    ime_cursor_px: Option<(i32, i32)>,
    /// Surface that owned the current platform-IME preedit. Windows/Linux can
    /// deliver `Ime::Commit` after our internal pane focus has moved; keeping
    /// this separate from `ime_focus` prevents that late commit from landing
    /// in the newly-active terminal.
    os_ime_surface: Option<String>,
    /// (committed text, cursor-at-commit, owner surface). gpu paints frames so fast it
    /// draws the moment AFTER a syllable commits but BEFORE the shell's
    /// echo arrives, so the preedit ("ㄴ") briefly shows where the
    /// committed glyph ("안") will land. We overlay the committed text
    /// in front of the preedit until the echo lands (cursor advances ⇒
    /// `cursor != stored`), which is what sugarloaf got for free by
    /// being slow enough to wait for the echo.
    commit_overlay: Option<(String, (u16, u16), String)>,
    /// True between `Ime::Enabled` and `Ime::Disabled`. Tracks whether
    /// the OS IME owns this keyboard at all — when active, Hangul (and
    /// other CJK) keystrokes are double-delivered (KeyboardInput.text
    /// + Ime::Preedit/Commit) and we have to drop the keyboard side
    /// even before the first Preedit lands.
    ime_active: bool,
    /// In-process Hangul jamo → syllable composer. We drive this from
    /// the KeyboardInput path whenever the OS keyboard layout hands us
    /// a Hangul jamo — macOS's NSTextInputContext doesn't fire
    /// Ime::Preedit for the *first* keystroke after a script switch
    /// (the jamo arrives only via KeyboardInput.text), so to compose
    /// "ㄱ + ㅏ → 가" reliably from the very first key we route every
    /// jamo through our own Composer instead of trusting macOS to
    /// queue it for us.
    hangul: kasa_ime::Composer,
    /// 위 조합기를 지금 쓰고 있는 입력 문맥. `ime_retarget` 이 이 값이 바뀌는
    /// 순간 조합 중이던 음절을 **떠나는 쪽에** 확정시킨다 — 안 그러면 터미널에서
    /// 치던 글자가 편집기에 떨어지고, Backspace 는 그 잔재만 갉는다.
    ime_focus: Option<ImeFocus>,
    /// ghostty식 pane 핸들(⋮) hit rect: (pane id, logical rect). 상단 중앙에
    /// 평소 흐릿하게 상시 표시, 클릭=컨트롤 메뉴(Phase 3)·드래그=pane 이동(Phase 4).
    pane_handle_rects: Vec<(String, (f32, f32, f32, f32))>,
    /// pane 상단 띠(box 높이 30%) hit rect: (pane id, logical rect). 이 영역에
    /// 커서가 들어오면 ⋮ 가 흐릿하게 등장한다(평소엔 완전히 숨김).
    pane_top_zones: Vec<(String, (f32, f32, f32, f32))>,
    /// ⋮ 핸들 위 커서 여부 — 손모양 커서 transition + ⋮ 강조 redraw 트리거.
    handle_hovered: bool,
    /// pane 상단 띠(top_zone) 안에 커서가 있는지 — ⋮ 등장/소멸 redraw 트리거.
    handle_zone_hovered: bool,
    /// ghostty ⋮ 메뉴가 열린 pane id(None=닫힘). ⋮ 클릭으로 토글.
    handle_menu: Option<String>,
    /// ⋮ 메뉴 버튼 hit rect: (액션, logical rect). render가 매 프레임 채움.
    handle_menu_hits: Vec<(ActionKind, (f32, f32, f32, f32))>,
    /// 타이틀바 Claude 계정 칩의 드롭다운이 열려 있는지. ⋮ 핸들 메뉴와 같은 짝 —
    /// render 가 매 프레임 rect 를 채우고 handler 가 이전 프레임 rect 로 힛테스트.
    account_menu: bool,
    account_menu_rect: Option<(f32, f32, f32, f32)>,
    account_menu_body_rect: Option<(f32, f32, f32, f32)>,
    account_menu_submenu_rect: Option<(f32, f32, f32, f32)>,
    account_menu_submenu_body_rect: Option<(f32, f32, f32, f32)>,
    account_menu_corridor_rect: Option<(f32, f32, f32, f32)>,
    account_menu_submenu_scroll: f32,
    account_menu_submenu_scroll_max: f32,
    account_menu_submenu_hit_start: usize,
    account_menu_scroll: f32,
    account_menu_scroll_max: f32,
    account_menu_suppressed_buttons: Vec<MouseButton>,
    account_menu_escape_release: bool,
    /// 사용량 pill 의 rect(클릭 = 계정 드롭다운 토글). pill 을 안 그리는 프레임엔 None.
    account_chip_rect: Option<(f32, f32, f32, f32)>,
    /// 하단 상태줄의 계정 세그먼트 rect. Info 탭을 안 열어도 **항상** 있는 손잡이라
    /// 실제로 계정을 여닫는 자리는 이쪽이 된다.
    status_account_rect: Option<(f32, f32, f32, f32)>,
    /// 상태줄 오른쪽의 판 번호 칸. 계정 세그먼트와 **같은 드롭다운**을 여는 두 번째
    /// 손잡이다 — 자리를 오른쪽으로 옮기면서도 「눌러서 캐묻기」를 잃지 않으려는 것.
    status_version_rect: Option<(f32, f32, f32, f32)>,
    /// 계정 칸이 지금 **어느 기계**를 다루나. `true` = 본진(홈 기계).
    ///
    /// 두 칸으로 가른 이유는 claude 가 실제로 **양쪽에서 돌기 때문**이다(2026-09-05
    /// 실측: 작업대 6개·본진 4개). 한쪽만 보여 주면 하단 상태줄(늘 이 기계 것)과
    /// 어긋나 「설정창이랑 하단이랑 왜 다르냐」가 된다 — 사용자가 실제로 그렇게 물었다.
    set_account_scope_home: bool,
    /// 드롭다운이 **어느 손잡이에서** 열렸나 — 메뉴를 그 자리에 붙여 그린다.
    /// 손잡이가 둘(Info 탭 계정 행 · 상태줄)이라, 하나로 고정하면 다른 쪽에서 열었을
    /// 때 메뉴가 화면 반대편에 뜬다.
    account_menu_anchor: Option<(f32, f32, f32, f32)>,
    /// 드롭다운 항목 hit rect.
    account_menu_hits: Vec<(AccountMenuItem, (f32, f32, f32, f32))>,
    /// Rendered markdown content height (logical px) per pane id, published by
    /// the renderer each frame. The scroll handler clamps scroll_offset to
    /// (content_h - visible_h) so a markdown pane can't over-scroll.
    md_content_h: HashMap<String, f32>,
    /// Document-space y of each rendered block per pane id, published by the
    /// renderer each frame (see `Gpu::md_block_ys`). `set_md_mode` reads it to
    /// carry the scroll position across a Raw↔Render toggle.
    md_block_ys: HashMap<String, Vec<f32>>,
    /// Pending "put this source line at the top" request per pane id, set by a
    /// Raw→Render toggle. The new layout's block positions only exist after a
    /// draw, so the renderer consumes this once `md_block_ys` is fresh.
    md_scroll_anchor: HashMap<String, usize>,
    /// Screen-space rects of every word the rendered view drew last frame, per
    /// pane id: (x, y, w, h, text). A document view has no cell grid, so this is
    /// what a drag selection and its copy resolve against.
    md_word_rects: HashMap<String, Vec<(f32, f32, f32, f32, String)>>,
    /// Live drag selection in the rendered view. `None` = nothing selected.
    md_render_sel: Option<MdRenderSel>,
    /// 마우스 노치 스크롤의 목표 위치와 마지막 보간 시각(pane id 별). 트랙패드
    /// 픽셀 델타는 그 자체가 부드러워 즉시 반영한다 — 보간하면 손가락보다 늦게
    /// 미끄러진다. 노치는 한 번에 세 줄을 뛰어 계단으로 읽히니 목표만 받아 두고
    /// `tick_md_scroll` 이 프레임마다 지수로 따라간다. 비어 있으면 애니 없음.
    /// (MarkdownPane 이 아니라 여기 두는 이유: pane 구조체에 필드를 더하면 생성부
    /// 다섯 곳이 동시에 깨져 병렬 작업이 서로를 막는다.)
    md_scroll_anim: HashMap<String, (f32, Instant)>,
    /// 클릭 연타 기억: (마지막 클릭 시각, x, y, 연타 횟수). 더블클릭 단어선택 ·
    /// 트리플클릭 줄선택 판정용. 레포에 더블클릭 판정이 아예 없어(터미널조차)
    /// 새로 두는 자리다.
    md_click_streak: Option<(Instant, f32, f32, u8)>,
    /// Find-bar button hit boxes (pane id, button, logical-px rect), rebuilt by
    /// the renderer each frame. Tested before the body box below, since the bar
    /// floats over the editor — a click on it must not also move the caret.
    md_find_rects: Vec<(String, FindBtn, (f32, f32, f32, f32))>,
    /// Raw-editor body box (logical px) per pane id, published by the renderer
    /// each frame. A click in this box hit-tests to a caret position so the
    /// mouse can place the edit cursor (see `md_click_caret`).
    md_body_rects: HashMap<String, (f32, f32, f32, f32)>,
    md_task_hits: HashMap<String, Vec<(f32, f32, f32, f32, usize)>>,
    md_link_hits: HashMap<String, Vec<(f32, f32, f32, f32, String)>>,
    md_copy_hits: HashMap<String, Vec<(f32, f32, f32, f32, String)>>,
    /// In-pane tab hit rects: (pane id, tab index, logical rect). Click
    /// switches that pane's active_tab. Rebuilt each header paint.
    pane_tab_rects: Vec<(String, usize, (f32, f32, f32, f32))>,
    /// Per-tab × close hit rects: (pane id, tab index, logical rect).
    pane_tab_close_rects: Vec<(String, usize, (f32, f32, f32, f32))>,
    /// 계정이 바뀐 pane 헤더의 「재시작」 칩 hit rect — (pane, rect).
    /// 실제로 그리는 조건은 `pane_account_stale` 에 그 pane 이 있을 때.
    pane_restart_chip_rects: Vec<(String, (f32, f32, f32, f32))>,
    /// "+" new-tab button hit rect per pane: (pane id, logical rect).
    pane_plus_rects: Vec<(String, (f32, f32, f32, f32))>,
    /// Throttle for `refresh_pane_activity`: the working-bar/completion-toast
    /// busy scan walks every pane's grid, so it runs at most a few times a
    /// second rather than per frame. `None` until the first scan.
    pane_busy_check: Option<Instant>,
    /// 계정 전환 뒤 되띄우기를 기다리는 pane 이 **언제부터 조용한가**. 스피너는 도구
    /// 결과가 오가는 찰나에 잠깐 사라져서, 「지금 idle」 하나로 판정하면 일하는 중인
    /// pane 을 그 틈에 끊는다 — 2026-08-15 에 사용자가 대화하던 pane 이 그렇게
    /// 죽었다("하다가 계정전환하니까 너가 없어졌어"). 연속으로 조용한 시간을 재서
    /// 그 틈을 건너뛴다.
    pane_account_quiet_since: HashMap<String, Instant>,
    /// pane id → (transcript mtime, bg_active, (질문, 그 답변 줄들) 목록) — an
    /// mtime-gated cache for the header pulse bar. An idle pane's transcript
    /// rarely changes, so the bar's "background/Monitor running" check reads the
    /// tail only when mtime moves.
    ///
    /// 프롬프트를 함께 담는 이유: 스크롤 sticky 띠가 claude 화면에서 읽어 오던
    /// 프롬프트 행이 사라져(2026-08-30 실측: 스크롤해도 최상단 행이 빈 채로 온다)
    /// 띠의 글감이 없어졌다. 같은 tail 을 이미 읽고 있으므로 여기서 함께 꺼내면
    /// 추가 IO 없이 kasaterm 이 그 띠를 직접 그릴 수 있다.
    pane_bg_mtime: HashMap<
        String,
        (
            std::time::SystemTime,
            bool,
            Vec<(String, Vec<String>)>,
            bool,
        ),
    >,
    /// pane id → 그 pane 의 프롬프트 목록을 **깊게** 읽어 둔 transcript mtime.
    ///
    /// `pane_bg_mtime` 이 담는 목록은 512KB 꼬리에서 나온다. 일하는 pane 은 도구
    /// 출력 하나가 1MB 를 넘어서, 그 창에는 질문이 **한 개**밖에 안 들어간다
    /// (2026-08-31 실측: 24MB 기록에서 0.5MB→1개, 8MB→26개). 후보가 하나뿐이면
    /// 스크롤 띠는 무엇을 골라도 늘 그 하나라 「엉뚱한 질문이 붙는다」가 된다.
    ///
    /// 그렇다고 늘 8MB 를 읽을 수는 없다 — 이 캐시는 pane 마다, transcript 가
    /// 바뀔 때마다 갱신되므로 창 열두 개면 틱마다 100MB 를 읽게 된다. 그래서
    /// **위로 스크롤한 pane 에서만** 깊게 다시 읽고 그 mtime 을 여기 적어 둔다.
    pane_deep_prompts: HashMap<String, std::time::SystemTime>,
    /// 이번 프레임에 **위로 스크롤돼 있던** pane 들. 렌더는 ws 락을 쥔 채 도는
    /// `&self` 자리라 `&mut self` 를 못 부른다 — 표시만 남기고, 실제 깊은 읽기는
    /// 다음 틱(`refresh_pane_activity`)이 한다. 한 프레임 늦지만 스크롤은 사람이
    /// 붙들고 있는 상태라 눈에 안 띈다.
    pane_deep_want: std::cell::RefCell<std::collections::HashSet<String>>,
    /// pane id → 올려다보는 동안 **마지막으로 확정한** 질문.
    ///
    /// 띠는 화면에 보이는 `❯ 질문` 머리줄로 어느 턴인지 확정한다. 긴 답변 한가운데를
    /// 보고 있으면 그 머리줄이 화면 밖이라 확정할 재료가 없어지는데, 거기서 짐작으로
    /// 떨어지면 엉뚱한 질문이 붙는다. 스크롤은 한 번에 몇 줄씩만 움직이고 claude 는
    /// 그때마다 화면을 다시 그리므로 머리줄이 뷰포트를 가로지르는 장면을 우리가 반드시
    /// 본다 — 그때 갱신해 두면 파묻힌 자리에서도 답이 정확하다.
    ///
    /// 맨 아래로 돌아가면(스크롤 게이트가 닫히면) 버린다. 렌더가 `&self` 자리라
    /// RefCell 이다.
    pane_sticky_turn: std::cell::RefCell<std::collections::HashMap<String, String>>,
    /// (window index, rect) for every window tab in the left sidebar.
    /// Populated by the render path, consumed by the MouseInput handler so
    /// a click switches windows. Logical px.
    window_tab_rects: Vec<(usize, (f32, f32, f32, f32))>,
    /// 사이드바 방 이름 인라인 편집(느린 더블클릭). 필드를 늘리지 않으려 한 줄로 묶었다 —
    /// `struct App` 정의는 병렬 작업 충돌 핫스팟이다(CLAUDE.md).
    room_rename: RoomRename,
    /// 닫는 동안 목록 자리를 얼려 두는 상태 — `CloseFreeze` 주석 참조.
    close_freeze: CloseFreeze,
    /// 펼친 방 아래 pane 한 줄씩의 히트 영역 — (방, pane id, rect). 탭 rect 안에
    /// 들어 있으므로 클릭 판정은 **탭보다 먼저** 해야 한다.
    sidebar_row_rects: Vec<(usize, String, (f32, f32, f32, f32))>,
    /// 배치도 칸만 따로 — `sidebar_row_rects` 에도 섞여 있지만(클릭·우클릭은 한
    /// 벡터로 판정한다) 드래그 착지는 칸과 줄의 규칙이 달라 갈라 봐야 한다.
    sidebar_mini_rects: Vec<(usize, String, (f32, f32, f32, f32))>,
    /// 사이드바 pane 행 우클릭 메뉴 — `(x, y, 방, pane id)`.
    ///
    /// 방을 함께 쥐는 이유: 숨기기는 **그 방을 활성으로 만든 뒤** 돌아야 한다. 사이드바는
    /// 모든 방의 pane 을 보여주는데, 다른 방 pane 을 그냥 `stash_pane` 하면 활성 트리에
    /// 없어서 `remove_pane`(죽이는 경로)으로 샌다.
    sidebar_menu: Option<(f32, f32, usize, String)>,
    /// 그 메뉴 항목의 rect — 렌더가 채우고 클릭이 읽는다(파일트리·Info 메뉴와 같은 관례).
    sidebar_menu_rects: Vec<(SidebarMenuAction, (f32, f32, f32, f32))>,
    /// (window index, close-× rect) for each window tab. Only present when
    /// there's more than one window (the last window can't be closed).
    window_tab_close_rects: Vec<(usize, (f32, f32, f32, f32))>,
    /// Window-tab overflow windowing: index of the first tab shown in the
    /// strip. The strip shows a contiguous run of whole tabs (no partial
    /// clipping — 이 띠는 클립을 안 세운다); the wheel steps this, and
    /// switch/new reveal the active tab via `win_tab_reveal`.
    win_tab_first: usize,
    /// 세로 사이드바의 스크롤 위치(logical px). 방 목록은 카드 높이가 제각각이라
    /// 인덱스로 세면 한 번 밀 때 카드 한 장이 통째로 튄다 — 굴린 만큼 그대로
    /// 흐르게 픽셀로 쥔다(2026-08-28 지시: "한장씩이 아니라 그냥 보이는대로").
    /// 가로 탭은 폭으로 흐르고 알약이 다 같은 크기라 `win_tab_first` 그대로다.
    sidebar_scroll_px: f32,
    /// How many window tabs the strip fit last frame. Written by the render
    /// pass (sidebar_layout output), read by the wheel handler for clamping.
    win_tab_vis: usize,
    /// Sub-step wheel accumulator for the tab strip (px; one tab per 48).
    win_tab_wheel_accum: f32,
    /// In-flight window-tab reorder drag; `None` when no tab is being dragged.
    win_tab_drag: Option<WinTabDrag>,
    /// 닫은 pane 스택(최근이 뒤). ⌘⇧T 가 뒤에서 꺼내 되살리고, 인포가 흐린 줄로
    /// 보여 준다 — 되돌릴 수 있다는 걸 알리지 않으면 있으나 마나다.
    closed_panes: Vec<ClosedPane>,
    /// One hit rectangle keeps the bottom device action and top-tab room action aligned with their paint.
    room_list_add_rect: Option<(f32, f32, f32, f32)>,
    /// Whether the "+" shell picker popup is open. Toggled by clicking the
    /// top-tab "+"; dismissed on item-click or an outside click.
    shell_menu_open: bool,
    /// Popup item hit rects `(shell_command, rect)`, logical px. Rebuilt each
    /// paint while the menu is open; consumed by the MouseInput handler.
    shell_menu_hits: Vec<(String, (f32, f32, f32, f32))>,
    /// Shell command the next `spawn_session_pane` should launch instead of
    /// the default. Set by a shell-picker selection, consumed (taken) once.
    pending_shell: Option<String>,
    /// Per-window (name, cwd) tab labels, by window index. Refreshed on a
    /// throttle (cwd resolution shells out to lsof, so never per-frame).
    window_labels: Vec<(String, String)>,
    window_labels_at: Option<Instant>,
    /// Explicit window/session name overrides by window index (`window.rename`).
    /// `refresh_window_labels` derives labels from the representative pane, but
    /// rename override 는 세션 라벨이 어떤 leaf 가 대표든 유지돼야 한다
    /// pane is the representative — this map wins over the derived name. Not
    /// persisted: 호출자가 매번 재적용하므로, 재시작 후
    /// 재지정되면 윈도우가 다시 마킹된다.
    window_name_override: HashMap<usize, String>,
    selection: Option<Selection>,
    drag_anchor: Option<(u16, u16)>,
    /// A left-press that landed on a detected URL. Holds (url, press_px) so a
    /// release that stayed put (a click, not a drag) opens it; a drag clears it.
    link_armed: Option<(String, (f32, f32))>,
    /// In-flight pane-divider drag: the BSP tree path of the split being
    /// resized plus its axis. `Some` while the user holds the mouse on a
    /// seam; each motion event re-derives the ratio from the cursor.
    resize_drag: Option<(Vec<u8>, kasa_pty::SplitDir)>,
    /// Last cell-quantised divider position fired through `resize_backend`.
    /// Lets the divider-drag handler skip the heavy PTY reshape on every
    /// sub-cell wiggle of the cursor — only crossing a cell boundary
    /// SIGWINCHes the panes (Claude Code reflows are very expensive).
    last_divider_pos: Option<u16>,
    /// Instant of the most recent PTY reshape fired during a divider drag.
    /// Layout ratio still updates on every cursor move (live seam) but
    /// SIGWINCH is throttled to ~10 Hz here — Claude Code's full-screen
    /// repaint on every SIGWINCH otherwise turns into a melted-glass feel.
    last_divider_pty_resize: Option<std::time::Instant>,
    /// In-flight header drag-and-drop: which pane the user grabbed by its
    /// header, the press position, and whether the cursor has moved past
    /// the threshold (only then does releasing relocate, so a plain click
    /// still just focuses the pane).
    header_drag: Option<HeaderDrag>,
    /// In-flight in-pane tab reorder. `Some` while the user holds the mouse
    /// on a tab; releasing either reorders (if it became a drag) or switches
    /// the active tab (if it stayed a click).
    tab_drag: Option<TabDrag>,
    /// 라이브 드래그 이동의 원본 레이아웃 백업. active 드래그가 처음 layout을
    /// 실제로 건드릴 때 캡처되고, 드롭이 무효(빈 곳·Center·자기 자신)일 때 이걸로
    /// 복원한다. 드래그가 끝나면 None.
    drag_orig_layout: Option<kasa_pty::PtyLayout>,
    /// 라이브 드래그가 마지막으로 실제 적용한 `(target, zone)`. 같으면 reshape를
    /// 건너뛰어(throttle) zone이 바뀔 때만 SIGWINCH가 나가게 한다.
    drag_live_applied: Option<(String, DropZone)>,
    /// In-flight image-pane pan drag: `(pane_id, start_cursor_px, base_pan)`.
    /// `Some` while dragging a zoomed image's body; CursorMoved updates the
    /// active tab's `image_pan_*` from `base_pan + (cursor - start)`.
    image_pan_drag: Option<(String, (f32, f32), (f32, f32))>,
    /// In-flight file-tree → terminal path drag. `Some` while a tree row is
    /// held; releasing over a pane types the path into that shell.
    /// Inline "new file / folder" entry. `Some((is_dir, name_buffer))` while
    /// the user is naming a freshly-requested entry; Enter creates it under
    /// the tree root, Esc cancels. Keystrokes route here like the search box.
    /// Hit rects for the new-folder / new-file buttons beside the search box,
    /// refreshed each frame.
    /// Row rect of the inline new-entry naming box (for the I-beam hit-test).
    /// Tree row the user last clicked — the Cmd+Delete target.
    /// Whether the text (I-beam) mouse cursor is currently shown, so we only
    /// flip the OS cursor on the transition in/out of an input box.
    text_cursor_shown: bool,
    /// Active "close while a process is running?" modal. While `Some`, the
    /// dialog is painted over everything and swallows input until the user
    /// picks 취소/닫기.
    confirm_close: Option<ConfirmClose>,
    /// `confirm_close` 를 OS 시트가 대신 보여 주는 중 — 앱 안 카드는 안 그린다.
    confirm_native: bool,
    /// Confirm-modal button hit rects, refreshed each frame: `(btn, rect)`.
    confirm_btn_rects: Vec<(ConfirmBtn, (f32, f32, f32, f32))>,
    /// Chrome-style restore prompt shown at launch: the saved session state
    /// awaiting the user's 복원/새로 시작 decision. While `Some`, a modal is
    /// painted over the fresh session and swallows input.
    restore_prompt: Option<serde_json::Value>,
    /// 복원을 누른 뒤 실제 재구성까지의 짧은 유예 — `(저장본, 시작할 시각)`.
    ///
    /// 재구성은 pane 마다 PTY 를 띄우느라 수 초간 GUI 스레드를 잡는다. 그동안
    /// 화면은 직전 프레임에 멈춰 있는데, 그게 카드의 검은 막이라 새로 살아난
    /// pane 이 그 막 아래로 비쳐 「복원 창이 겹친 것처럼 보인다」가 됐다
    /// (2026-09-01 지적). 그래서 막 대신 「되살리는 중」을 한 프레임 그려 두고,
    /// 그 화면이 멈춰 있게 한 뒤에 재구성을 시작한다.
    restore_applying: Option<(serde_json::Value, std::time::Instant)>,
    restore_progress: Option<restore_progress::RestoreProgress>,
    restore_retry_rect: Option<(f32, f32, f32, f32)>,
    restore_toast_rect: Option<(f32, f32, f32, f32)>,
    /// Restore-prompt button hit rects, refreshed each frame: `(btn, rect)`.
    restore_btn_rects: Vec<(RestoreBtn, (f32, f32, f32, f32))>,
    /// 자동 스냅샷(강제 종료 대비) 상태 — 마지막 저장 시각, 그 뒤로 깨어난 적이
    /// 있는지, 그때 쓴 내용의 해시. `autosave_session` 참조.
    session_saved_at: std::time::Instant,
    session_touched: bool,
    session_saved_hash: Option<u64>,
    /// Currently hovered in-pane tab `(pane_id, tab_idx)`. Drives the
    /// hover-only × and brighter text on inactive tabs.
    pane_tab_hover: Option<(String, usize)>,
    /// Image pane action button hit rects (zoom in/out, rotate, reset),
    /// rebuilt each header paint. Mouse handler dispatches on these.
    image_btn_rects: Vec<(String, ImageBtn, (f32, f32, f32, f32))>,
    /// Live width of the left window-tab sidebar (logical px). User-draggable
    /// via the right edge; defaults to `SIDEBAR_W`. `effective_sidebar_w()`
    /// reads this when visible.
    sidebar_w_logical: f32,
    /// In-flight sidebar resize drag — `(start_cursor_x, start_width)`.
    sidebar_resize: Option<(f32, f32)>,
    /// Cell grid size last published to the PTY backend by the window-resize
    /// path. Live-resize fires Resized at ~60Hz with pixel-granular sizes,
    /// but only crosses a cell boundary every 16-32px — without this guard
    /// we re-shape every pane PTY (alacritty Term::resize + SIGWINCH) and
    /// re-publish layout on every wiggle of the pointer.
    last_resized_cells: (u16, u16),
    /// During a macOS live-resize we deliberately do NOT call surface.configure
    /// or render — the CAMetalLayer keeps its old IOSurface and gravity=topLeft
    /// anchors it to the top-left while AppKit stretches the layer bounds
    /// (ghostty's trick on top of wgpu). The final size that came in during
    /// the drag is stashed here so we can flush it once the user lets go.
    pending_resize: Option<winit::dpi::PhysicalSize<u32>>,
    /// Pane that owns the in-flight mouse reporting drag. `Some(pane_id)`
    /// when we forwarded a button-press into a mouse-reporting TUI and
    /// are now relaying motion + release into the same pane. None means
    /// no mouse-reporting drag is active; selection logic owns the
    /// pointer.
    mouse_forward_pane: Option<String>,
    /// 마지막으로 호버 이동을 보낸 칸과, 그 칸이 호버에 반응했는지(손가락 커서 판정).
    hover_probe: Option<crate::input::HoverProbe>,
    /// Last left-click timestamp + position. Used only for the
    /// title-strip double-click → window-zoom shortcut. macOS handles
    /// this for us when the OS owns the titlebar, but our
    /// fullsize_content_view setup means we intercept those clicks.
    last_left_click: Option<(Instant, (f32, f32))>,
    /// 터미널 셀 위 연속 클릭 — (시각, 셀, 횟수). 두 번이면 단어, 세 번이면 줄을 고른다.
    cell_click: Option<(Instant, (u16, u16), u8)>,
    /// Last file-tree row click (time + path). A second click on the *same*
    /// file row within the double-click window opens it in a split — folders
    /// keep their single-click expand, so files need their own gate.
    last_tree_click: Option<(Instant, std::path::PathBuf)>,
    /// Pane id currently zoomed (tmux-style): rendered alone filling the work
    /// area with the other panes hidden, until toggled off. GUI-local render
    /// state — the daemon's layout tree is untouched (like a divider ratio).
    zoomed_pane: Option<String>,
    /// 이번 이벤트 묶음에 사람 손(키·IME·왼클릭)이 닿았다 — `about_to_wait` 에서
    /// 초점 칸을 보고 원본 격자를 누가 가질지 정한다(`mirror_follow`).
    human_touch: Option<crate::mirror_follow::HumanTouch>,
    /// 새 칸 → 그 칸을 연 칸. 벤토로 다시 짤 때 한 칸이 연 형제들을 잇대어 세운다
    /// (`rebento_window`). 재시작하면 비지만 트리 순서가 이미 그 줄을 담고 있다.
    pane_opener: HashMap<String, String>,
    /// Pre-maximize window frame (Cocoa screen coords: x, y, w, h), stashed
    /// when a title-strip double-click maximizes so the next double-click can
    /// snap the window back instantly. `None` = currently un-maximized. See
    /// `gpu::toggle_maximize_no_anim` — we drive the frame ourselves with
    /// `animate:NO` to kill AppKit's slow zoom animation.
    saved_window_frame: Option<(f64, f64, f64, f64)>,
    /// A titlebar press that hasn't yet decided between "click" and "drag".
    /// We defer `window.drag_window()` (which enters AppKit's modal move
    /// loop and would eat the second click of a double-click) until the
    /// pointer actually moves past a small threshold. Holds the press
    /// position; cleared on release or once the drag starts.
    titlebar_drag_pending: Option<(f32, f32)>,
    /// Cached value of the OS window title — `window.set_title` is
    /// cheap but not free, so we only call it when the resolved
    /// label actually changes.
    last_window_title: Option<String>,
    /// Deadline keeping the Claude "busy" anim alive after the
    /// spinner row briefly disappears from the grid. Without this,
    /// fast redraws toggle between "✱ claude" and the live status
    /// every frame because Claude Code repaints the spinner phase
    /// across separate cells. 800ms of stickiness smooths it out.
    #[allow(dead_code)]
    claude_busy_until: Option<Instant>,
    /// Most recent claude status line we lifted from the grid. Kept
    /// so the titlebar stays on the last "✻ Brewed for Ns" frame
    /// while Claude Code is mid-repaint and the marker row briefly
    /// vanishes. Cleared when the busy window expires.
    #[allow(dead_code)]
    last_claude_status: Option<String>,
    /// Per-pane collab activity (status + intent) from the daemon's StateView —
    /// the cross-window busy source. A "working" pane draws a header bar + a
    /// sidebar-window dot; a working→idle flip fires the completion toast. Keyed
    /// by surface id, replaced wholesale on each StateView.
    pane_activity: HashMap<String, crate::stream::PaneStatusView>,
    /// ultracode 가 켜진 pane — 입력박스를 보라색으로 두른다. claude 는 이 상태를
    /// statusline payload 에 안 실어 주므로(effort 는 low..max 뿐), UserPromptSubmit
    /// 훅이 남기는 `/tmp/kasaterm-collab/ultracode/<sid>.on` 마커를 대신 읽는다.
    /// `refresh_pane_activity` 박자(300ms)로 갱신 — 매 프레임 stat 하지 않는다.
    pane_ultracode: std::collections::HashSet<String>,
    /// 줄 서 있는 tell — 받는 pane → (몇 통, 막힌 까닭). 입력박스 아래 테두리에 「쪽지 대기」로 뜬다.
    /// 받는 사람은 제 입력칸이 쪽지를 막는 줄 몰라 15분 뒤 조용히 버려지던 자리다(2026-10-01).
    tell_waiting: HashMap<String, (usize, kasa_socket::tell::Hold)>,
    /// 괄호 없는 턴-시작 스피너(`✢ Transmuting…`)의 살아있음 프로브 — pane →
    /// (행, 마지막 글리프, 확정, 그 행 최초 목격 시각). 글리프가 틱 사이에
    /// 바뀌면 확정 = 진짜 스피너(인용문은 멈춰 있다). 방금 Enter 가 들어간
    /// pane(`PtySession::last_submit`)은 확정을 기다리지 않고 즉시 신뢰한다.
    /// 목격 시각은 미확정 후보가 있는 동안만 스캔 박자를 좁히는 근거(1.2초 상한 —
    /// 직전 틱의 팔레트 명암 — 라이트↔다크 플립을 `poll_claude_retheme` 이
    /// 감지하는 기준값. 어느 입구로 테마가 바뀌든(설정 화면·웹뷰·system 폴링)
    /// 전부 팔레트에 수렴하므로, 입구마다 훅을 다는 대신 결과를 비교한다.
    theme_light_last: Option<bool>,
    /// 테마 플립 때 「떠 있는 claude」에 /theme 피커 주입을 기다리는 pane 큐.
    /// claude 2.1.232 는 settings.json 변경도 CSI ?997 리포트도 실행 중엔 안
    /// 읽으므로(2026-08-14 실측 4종), 세션 안 피커를 실제로 조작하는 것이
    /// 실행 중 세션을 바꾸는 유일한 길이다.
    retheme_queue: HashMap<String, RethemeState>,
    /// Whether our window currently has OS focus. Drives notification
    /// suppression: a completion alert for the already-focused active pane is
    /// pointless (the user is looking right at it), so we skip the desktop
    /// alert when `window_focused && notify target == active pane`.
    window_focused: bool,
    /// Pane id → instant a completion notification flashed its header, so the
    /// render pass can pulse it for a beat then let it settle.
    notify_flash: HashMap<String, std::time::Instant>,
    /// Pane id → 알리지 않고 붙들어 둔 일시적 오류(글자, 처음 본 때). `TROUBLE_GRACE` 가
    /// 지나도 그대로면 그때 알린다.
    held_trouble: HashMap<String, (String, std::time::Instant)>,
    /// 계정이 바뀐 순간 — 계정 칩 둘레가 잠깐 반짝인다. 끝나면 `None` 으로 걷어야
    /// 프레임 펌프가 멎는다(안 걷으면 창 하나가 상시 8ms 로 돈다).
    account_flash: Option<std::time::Instant>,
    /// 다른 기기가 바꾼 계정을 따라가는 중 — 이때는 다시 퍼뜨리지 않는다(핑퐁 방지).
    account_switch_from_peer: bool,
    /// Panes whose last turn finished and the user hasn't typed into since —
    /// drives the student's cheer (arms-up) standing pose. Set on turn
    /// completion, cleared on the next `forward_key` into that pane, so the
    /// cheer persists until the user re-engages (not a brief flash).
    turn_done_panes: std::collections::HashSet<String>,
    /// Window indices with an *unseen* notification (a pane finished / needs
    /// attention while that window was in the background). The sidebar tab
    /// pulses until the user switches to that window, which clears the entry —
    /// a persistent "you missed this" cue, unlike the brief `notify_flash`.
    window_alert: std::collections::HashSet<usize>,
    /// 깜빡임을 멈춘 pane(leaf id). 사람이 그 방을 보러 오면 그 방 칸이 다 든다 — 손이 필요한 상태
    /// 표시는 그대로 서고 움직임만 멎는다. 끝남·기다림·오류가 새로 나면 그 pane 이 빠져 다시 깜빡인다
    /// (`quiet_room_blinks`·`wake_blink`, 2026-10-07 「방 포커스하면 없어지게」).
    blink_quiet: std::collections::HashSet<String>,
    /// 사이드바에서 pane 목록을 펴 둔 방. **펼친 것만** 담으므로 기본은 접힘이다
    /// (info.rs 의 `group_collapsed` 는 기본이 열림이라 반대 뜻 — 한 집합에 못 담아
    /// 따로 둔다). 방 인덱스가 키라 `reorder_window` 의 remap 을 반드시 통과해야
    /// 한다 — 안 그러면 2번을 펴 두고 3번 자리로 끌어 옮겼을 때 3번이 펴진 채 뜬다.
    expanded_windows: std::collections::HashSet<usize>,
    /// 펼친 카드를 **목록**으로 보는 방. 기본(비어 있음)은 배치도다.
    ///
    /// 지금 펴지거나 접히는 중인 방 — `(방, 펴는 중인가, 시작 시각)`.
    ///
    /// 한 번에 하나만 담는다. 다른 방을 토글하면 앞엣것은 그 자리에서 끝난 것으로
    /// 친다 — 목록이 동시에 두 군데서 밀리면 어느 쪽이 내가 누른 것인지 못 읽는다.
    expand_anim: Option<(usize, bool, std::time::Instant)>,
    /// 사이드바 pane 줄을 끌고 있는 중이면 그 상태.
    sidebar_row_drag: Option<SidebarRowDrag>,
    /// 밖에 나간 방을 **되돌리는** 아이콘의 자리 — `(방 인덱스, 사각)`. 매 프레임
    /// 재구축이라 창을 닫으면 그 자리도 같이 사라진다.
    /// claude 가 **한 번이라도** 떴던 pane. 학생 얼굴을 여기에만 내보인다.
    ///
    /// 캐릭터 자체는 pane 을 만들 때 배정된다 — 셸 env(`KASATERM_CHARACTER`·
    /// `KASATERM_PERSONA`)에 미리 심어 둬야 나중에 거기서 켜는 claude 가 그 학생으로
    /// 부팅되기 때문이다. 그런데 그러면 셸만 도는 pane 에도 얼굴이 먼저 떠 있어
    /// "누가 일하고 있다"로 읽힌다(사용자: "zsh 인데 학생이 이미 있는 이유는 뭐야").
    /// 배정은 그대로 두고 **보이는 시점만** claude 가 실제로 붙을 때로 미룬다.
    ///
    /// 한 번 본 것을 계속 기억하는 건, claude 가 cargo 같은 자식을 띄우면 그동안
    /// 프로세스 이름이 그쪽으로 바뀌어 얼굴이 깜빡 사라지기 때문이다.
    pane_claude_seen: std::collections::HashSet<String>,
    /// Panes that fired a notify/attention while not being looked at and
    /// haven't been opened since — drives the Dock badge count. Cleared when
    /// the pane becomes the focused active pane (`sync_dock_badge`).
    unread_panes: std::collections::HashSet<String>,
    /// 앱이 뜬 시각. 막 켜진 동안은 상태 판정이 한 번 흔들린다 — 훅·기록·명부가 아직 다 안
    /// 모여 일하는 중으로 보였다가 곧 쉬는 중이 되고, 그 사이가 「완료」 전이로 읽힌다.
    /// 그것을 알림·읽지 않음으로 세우면 껐다 켤 때마다 안 본 것이 쌓인 것처럼 보인다.
    booted_at: std::time::Instant,
    /// Last value pushed to the Dock badge, so AppKit is only touched on change.
    dock_badge_n: usize,
    /// Collab completion toast + approval card, grouped into a sub-struct
    /// (state.rs) so collab-UI work touches one file — CLAUDE.md 병렬 규칙.
    collab: state::CollabState,
    /// When we last recomputed the macOS window title. Rate-limits
    /// `maybe_update_window_title` to ~200ms because it locks the
    /// workspace + calls `ps -A` (process-tree lookup) on every hit,
    /// and a wheel burst fires `RedrawRequested` 60+ times per
    /// second.
    last_window_title_check: Option<Instant>,
    /// Per-pane shell cwd cache (pane id → working dir), feeding the header
    /// breadcrumb. Refreshed on a timer off the render path: `pid_cwd` shells
    /// out to `lsof`, so resolving it per pane on every frame would spawn a
    /// burst during a scroll/hover storm. See `refresh_pane_cwds`.
    /// 아로나 자동 시작(P5): characters 있는 방의 첫 pane 에 띄울
    /// claude 명령. start_pty 에서 가드 통과 시 세팅, 셸 prompt-end(OSC133) 감지
    /// 또는 타임아웃 시 1회 주입 후 None. solo·무테마면 애초에 None(무동작).
    pane_cwd_cache: HashMap<String, std::path::PathBuf>,
    /// pane 이 "보고 있는" 경로 오버라이드(SocketViewCwd 로 도착) — 파일트리 루트가
    /// 셸 cwd 보다 우선한다. 셸이 cd 로 움직이면(refresh_pane_cwds 에서 감지) 해당
    /// pane 의 오버라이드를 버려 stale 고착을 막는다(claude 살아 있으면 statusline
    /// 이 곧 재보고).
    pane_view_cwd: HashMap<String, std::path::PathBuf>,
    /// 방별 collab 분리(사용자). `pending_room`: 다음 spawn 할 pane 의 방 id(셸 env
    /// KASATERM_ROOM 주입 + ws.pane_room 기록용). pane→방 매핑은 ws.pane_room(공유).
    pending_room: Option<String>,
    next_room_seq: u32,
    /// 다음 spawn 할 pane 에 강제할 캐릭터(new_room_with_character 가 세팅). None 이면
    /// 빈 슬롯 순환 배정(미도리→모모이→…). 배정 결과는 KASATERM_CHARACTER env + /tmp 마커.
    pending_character: Option<String>,
    last_auto_character: Option<String>,
    pane_agent_launches: HashMap<String, u32>,
    /// 다음 split 이 셸을 띄울 폴더를 강제한다(`spawn_shell_pane` 이 세웠다 걷는다) —
    /// 다른 기계의 `to` 가 「이 폴더에서」를 실어 오는 유일한 길이다.
    pending_spawn_cwd: Option<String>,
    /// 닫을 때 **저쪽 pane 은 남길** 원격 pane(메뉴 「닫기 — 저쪽 pane 은 남김」).
    /// `to` 로 세운 자리는 기본이 함께 끄기라, 예외만 여기 적는다.
    remote_keep: std::collections::HashSet<String>,
    /// `to` 로 보낸 자리의 뒤처리 — 명령이 저쪽에 닿은 뒤 이 자리를 걷고 그 기기 방을 보기 창으로
    /// 연다(2026-09-17 결정: 원래 자리에 거울을 남기지 않는다).
    migrate_handoff: Option<MigrateHandoff>,
    /// pane id → current launch's conversation ID, never a permanent shell ID.
    pane_session_id: HashMap<String, String>,
    /// pane id → claude 실제 sessionId(transcript stem, `SocketSessionBound` 로 도착).
    /// pane_session_id(백엔드 발급)와 달리 fork/detach 시 갈라진 진짜 세션이라, 이걸로
    /// bg_agents 를 조회해 포크/백그라운드 배지를 판정한다.
    pane_claude_sid: HashMap<String, String>,
    /// pane id → 마지막으로 그 자리에 앉았던 (학생 이름, 대화 번호). **비석이다** —
    /// claude 가 죽으면 산 자리 표식(`pane_character`·`pane_claude_sid`)은 바로 걷히는데,
    /// 그 뒤에 닫힌 자리의 되살리기 줄이 얼굴도 번호도 없이 남았다(2026-09-17 지적:
    /// 「되살리기에 프사가 안 나와서 기록에서 찾았어」). 자리를 새로 차지하면 지운다.
    pane_last_seat: HashMap<String, (String, String)>,
    /// 거울 창 배치를 원본에 보낸 시각 — 그 직후의 당겨오기를 잠시 멈춘다.
    remote_view_push_at: Option<std::time::Instant>,
    /// 계정이 바뀐 뒤에도 **옛 계정으로 도는** pane → (떠난 계정 이름, 새 계정 이름).
    ///
    /// 계정은 프로세스 env 라 pane 이 뜰 때 박히고, 도는 프로세스는 못 바꾼다. 그래서
    /// 전환해도 이미 열린 pane 은 옛 계정 그대로다 — 화면(상태줄·Info)만 새 계정을
    /// 가리켜 「전환했는데 왜 그대로지」가 된다(사용자 2026-08-13). 여기 적어 두고
    /// pane 헤더에 「재시작할까요?」를 띄운다. Orca 도 같은 한계를 같은 방식으로 푼다.
    ///
    /// `restart_pane_agent` 가 성공하거나 사용자가 무시하면 지운다. A→B→A 로 되돌아온
    /// pane 도 지운다 — 다시 맞는 계정이 됐는데 물어볼 이유가 없다.
    pane_account_stale: HashMap<String, (String, String)>,
    /// cmux socket backend 핸들 — ResumeSession(attach/재개)이 세션 id 를 아는 유일한
    /// 시점에 pane↔transcript 를 bind_transcript 로 즉석 확정하기 위해 보관. attach 뷰는
    /// bind hook 이 안 떠서 board discovery 의 recent-jsonl 추측이 남의 활성 세션에
    /// 오귀속됐다(사용자: 왼쪽 pane 둘 다 프라나).
    socket_backend: Option<std::sync::Arc<socket::PtyBackend>>,
    /// 사용량 폴러에게 빌려주는 같은 핸들. 폴러 스레드는 소켓 backend 가 생기기 전에
    /// 뜰 수 있어 `socket_backend` 를 그때 클론해 갈 수 없다 — 이 자리를 통해 나중에
    /// 받는다. 폴러는 「지금 어느 모델이 도는가」를 알아야 안 쓰는 모델의 한도 때문에
    /// 계정을 옮기는 일을 피한다.
    shared_backend: std::sync::Arc<std::sync::Mutex<Option<std::sync::Arc<socket::PtyBackend>>>>,
    /// claude sessionId → parentSessionId(background kind 세션만). `claude agents
    /// --json --all` 폴러(handler.rs resumed)가 3초마다 갱신. 타이틀바 배지·학생 유지
    /// (부모 캐릭터 상속)가 읽는다. 백그라운드 세션이 아니면 키 없음.
    bg_agents: std::sync::Arc<std::sync::Mutex<HashMap<String, Option<String>>>>,
    /// 활성 계정의 **가장 먼저 닫히는 한도 창**. handler.rs resumed 의 폴러가 로컬
    /// `/claude-usage`(oauth/usage 프록시)를 조회해 채운다. Info 탭 머리의 계정 행이
    /// 읽는다. 토큰 없음/실패면 None → 숫자 숨김.
    claude_usage: std::sync::Arc<std::sync::Mutex<Option<UsageBadge>>>,
    /// **계정 저장소 경로 → 그 계정의 한도**(`""` = 기본 로그인). 위 `claude_usage` 가
    /// 활성 계정 하나만 담는 것과 달리, 이건 등록된 계정 전부를 담는다.
    ///
    /// 계정 드롭다운이 읽는다 — 누르기 **전에** 각 계정의 사용률이 보여야 하기 때문이다
    /// (사용자: "누르면 전환되버리잖아"). 전환해 봐야 아는 구조면 한도 때문에 옮기려는
    /// 사람이 옮길 곳을 못 고른다. 활성이 아닌 계정도 조회할 수 있는 것은 usage 프록시가
    /// 슬롯별 토큰을 직접 읽기 때문이다(`/claude-usage?dir=<슬롯>`) — 전환이 필요 없다.
    claude_usage_all: std::sync::Arc<std::sync::Mutex<HashMap<String, UsageBadge>>>,
    /// Sidebar git badge cache (cwd → branch/+ins/-del). A background thread
    /// polls each window's cwd directly (not via the daemon), so this stays
    /// off `%0`'s daemon path. Render reads it; the poller writes it.
    window_git:
        std::sync::Arc<std::sync::Mutex<HashMap<std::path::PathBuf, kasa_mcp::git::GitBadge>>>,
    /// cwds the git poller should refresh, set from the current windows' repr
    /// cwd just before the sidebar paints. Shared with the poll thread.
    git_poll_cwds: std::sync::Arc<std::sync::Mutex<Vec<std::path::PathBuf>>>,
    /// surface_id → {cwd, git badge} snapshot for the BA GUI's `/layout` (read
    /// off-thread by `socket::PtyBackend::window_layout`). The GUI fills it each
    /// frame from `pane_cwd_cache` + `window_git` (both already off the lsof/git
    /// hot path), so the socket thread never shells out — it lets the BA GUI
    /// draw a Warp-style cwd/branch/diff bar on plain (non-claude) terminal
    /// tiles, which carry no board row to read cwd from. CLAUDE.md 병렬 규칙.
    pane_status_pub: std::sync::Arc<std::sync::Mutex<HashMap<String, PaneStatus>>>,
    /// Right-hand git column + commit modal + path/branch dropdowns, grouped
    /// into a sub-struct (state.rs) so git-UI work touches one file, not this
    /// App definition — CLAUDE.md 병렬 규칙. (badge poller `git_poll_cwds` and
    /// the file-tree `git_ignore_*` stay separate — different domains.)
    git: state::GitState,
    /// 우측 칼럼의 Info 탭 — 활성 pane 셸 아래 프로세스 + listen 포트. 칼럼
    /// 폭/닫기는 `git` 과 공유하고 본문과 갱신 스레드만 여기 있다(state.rs).
    info: state::InfoState,
    /// 우측 칼럼의 Sessions 탭 — 과거 세션 기록(claude·codex·agy)을 골라 잇는다.
    /// `git`/`info` 와 칼럼 폭·닫기를 공유하고 본문과 수집 스레드만 여기 있다.
    sessions_col: state::SessionsColState,
    /// 우측 칼럼의 MCP·Skill 탭 — 하네스별로 무엇이 붙어 있나. 위 둘과 칼럼
    /// 폭·닫기를 공유하고 본문과 수집 스레드만 여기 있다.
    mcp_col: state::McpColState,
    /// Per-pane status bar (cwd/branch/diff chips at each pane's foot) + the
    /// open dropdown's state. Grouped into a sub-struct (state.rs) so statusbar
    /// work touches one file, not this App definition — CLAUDE.md 병렬 규칙.
    statusbar: state::StatusbarState,
    /// 대화 턴 헤더의 pane 별 앵커 캐시. 본체는 turnjump.rs — 여기 필드 한 줄만
    /// 두는 것도 statusbar 와 같은 이유다(CLAUDE.md 병렬 규칙).
    turn: turnjump::TurnJump,
    /// Last `refresh_pane_cwds` sweep — rate-limits the lsof calls.
    pane_cwd_check: Option<Instant>,
    /// Preview panes (image/markdown) already materialized from the daemon's
    /// StateView, keyed by pane id → the path we built it from. Guards against
    /// re-decoding the image on every (frequent) State broadcast; a changed
    /// path rebuilds. See the `UserEvent::DaemonState` handler.
    /// True while Alt/Option is held — draws each pane's %N big + centered
    /// (tmux `display-panes`), so the user can read a pane id for `tell %N` /
    /// focus without it cluttering the header normally. Toggled in
    /// `ModifiersChanged`.
    show_pane_numbers: bool,
    /// Sidebar file-tree column + its in-flight interactions, grouped into a
    /// sub-struct (state.rs) so file-tree work touches one file — CLAUDE.md 병렬.
    file_tree: state::FileTreeState,
    /// `git check-ignore` runs off-GUI-thread: spawning git from the unsigned
    /// kasaterm.exe triggers a Defender full-scan (~5s/call on Windows) that
    /// would freeze the toggle if run inline. The worker reads a (root, paths)
    /// request, fills the file-tree ignored set (`file_tree.ignored`), and wakes
    /// the loop so the next rebuild dims gitignored rows; until then, un-dimmed.
    git_ignore_req: std::sync::Arc<std::sync::Mutex<Option<(std::path::PathBuf, Vec<String>)>>>,
    git_ignore_started: bool,
    /// PTY 없는 싱글턴 설정 방의 카테고리와 돌아갈 사용자 pane.
    settings_scene: settings_room::SettingsScene,
    /// In-memory mirror of settings.json, edited live and written on each
    /// change so the next launch (and `resolve_*`) pick it up.
    set_cwd_mode: String,
    /// 파일트리에서 파일을 열 때의 기본 동작, `app` 모드가 쓸 앱 이름,
    /// `terminal` 모드가 실행할 명령줄.
    set_file_open_mode: String,
    set_file_open_app: String,
    set_file_open_cmd: String,
    set_file_tree_default: bool,
    set_footer_default: bool,
    /// Editor autosave quiet period. `None` = off, which is the default and
    /// what VS Code ships — writing a user's file without being asked is a
    /// surprise, so it stays opt-in. Cmd+S keeps its `✓ 저장됨` toast either way.
    set_autosave: Option<std::time::Duration>,
    set_shell: String,
    /// PixelDelta 스크롤 감도 배율. 기본 0.3 은 트랙패드 기준이고, 마우스휠을
    /// 쓰는 사람이 설정에서 올린다 — 둘이 같은 델타로 와 자동 구분이 안 된다.
    set_wheel_pixel_gain: f32,
    set_status_h: f32,
    set_pane_footer_h: f32,
    /// Per-pane claude wrapper injection (the shim reads these): persona on/off,
    /// model/effort overrides, and free-form extra args. Invariants
    /// (session-id/settings/task-list) stay hardcoded and are never exposed here.
    set_claude_persona: bool,
    set_shim_inject: bool,
    /// 팔레트 hex 편집 버퍼 — 포커스된 칸 하나가 쓴다. custom_theme 에는
    /// 완성된(파싱되는) 값만 나가므로, 타이핑 중의 반쪽 값은 여기서만 산다.
    set_palette_edit: String,
    /// 색 선택기의 HSV 상태. RGB 에서 매번 역산하면 s=0·v=0 에서 색상(H)이
    /// 소실돼 핸들이 튄다 — 포커스 때 한 번 역산해 시드하고 이후는 이게 정본.
    set_picker_hsv: (f32, f32, f32),
    /// 색 선택기 드래그 중인 면(액션)과 그 히트 rect(x,y,w,h). 커서가 rect
    /// 밖으로 나가도 클램프해 계속 따라오게, press 때 잡아 release 때 놓는다.
    set_claude_model: String,
    set_claude_effort: String,
    set_claude_extra: String,
    /// 전환 가능한 Claude 로그인 목록과 활성 계정 id(`""` = 기본 로그인). 설정
    /// 스냅샷이 프레임마다 만들어지므로 파일을 그때 읽지 않고 여기 들고 있는다.
    set_claude_accounts: Vec<socket::ClaudeAccount>,
    set_claude_account: String,
    /// 같은 것의 codex(ChatGPT) 판. 목록을 따로 드는 이유도 같다.
    set_codex_accounts: Vec<socket::CodexAccount>,
    set_codex_account: String,
    /// 계정 메뉴 표시 밀도 — `true` = Compact(가장 빡빡한 창 하나만, 막대 없이).
    set_usage_compact: bool,
    /// 창 하단바 표시 순서·숨김·색·사용량 세부 항목. 프레임마다 파일을 읽지 않는다.
    set_statusbar: statusbar_config::Prefs,
    /// 날씨(리퀴드 비) 설정·창별 덮어쓰기·시뮬레이션 — `weather/`.
    weather: weather::WeatherState,
    /// 하단바에 **활성 계정 말고 나머지 슬롯의 한도까지** 세운다. 활성 하나만
    /// 보이면 「지금 계정이 찼을 때 어디로 옮기나」에 답하려고 매번 드롭다운을
    /// 열어야 하는데, 그 손이 아까워 안 열다가 다 찬 계정을 계속 쓴다
    /// (2026-08-27 지시 「하단바에 다른계정 뭔지랑 사용량 실시간으로 계속
    /// 보이는거」). 활성은 게이지째 남고 나머지는 이름+% 로만 붙는다.
    /// 계정 메뉴에서 지금 계정 목록이 열려 있는 제공자. `None` = 로스터만.
    account_menu_provider: Option<AccountProvider>,
    /// 한도가 차면 다음 계정으로 알아서 넘어간다(기본 off) + 그 임계 사용률(%).
    set_account_autoswitch: bool,
    set_account_autoswitch_pct: f32,
    /// Which form text field has focus (cwd custom path / shell), if any.
    settings_input: Option<SettingsInput>,
    /// Caret (char index) for the focused single-line settings field
    /// (cwd path / shell / claude extra). Kept apart from `students_caret`
    /// (the persona multiline caret): only one is ever focused at a time, but
    /// sharing one store made the caret jump when focus crossed between a
    /// single-line field and the persona box.
    settings_caret: usize,
    /// Clickable targets collected during the settings paint, for hit-testing.
    /// Theme 카테고리의 캐릭터 상세 화면: 열린 캐릭터(이름) + persona 편집 버퍼 +
    /// 캐럿(문자 인덱스). 열 때 raw_persona 를 버퍼로 로드하고, blur/닫기 시
    /// characters.json 에 flush 한다. `None` 이면 목록 화면이다.
    students_selected: Option<String>,
    /// 상세를 연 테마 id. 빈 값/`__base`는 번들, 그 밖은 설치 테마 폴더다.
    students_theme: String,
    students_slug: String,
    students_model: String,
    students_backend: String,
    students_persona: String,
    students_caret: usize,
    /// 상세 화면의 이름 편집 버퍼. `students_selected` 는 파일에 **저장된** 이름을
    /// 유지한다 — 타이핑 도중의 반쯤 지운 이름으로 persona·그림을 조회하면
    /// 한 글자 지울 때마다 화면이 빈 캐릭터로 튄다.
    students_name: String,
    /// 상세 화면의 「원본」 뷰 — 열림/형식/버퍼를 한 덩어리로.
    students_raw: StudentRawEdit,
    /// 이름을 고치는 중인 테마 — `(폴더 id, 편집 버퍼)`. 캐럿은 `settings_caret`
    /// 을 같이 쓴다(한 번에 한 칸만 포커스). 버퍼를 파일과 따로 두는 건 타이핑
    /// 도중의 반쯤 지운 이름이 목록에 그대로 새어 나가지 않게 하려는 것이다.
    theme_label_edit: Option<(String, String)>,
    /// `(커스텀 팔레트 slug, 편집 버퍼)`.
    custom_theme_label_edit: Option<(String, String)>,
    /// `(제공자, 슬롯 id, 편집 버퍼)`.
    account_label_edit: Option<(AccountProvider, String, String)>,
    /// 기계 명부에서 지금 고치는 칸 — (줄 번호, ssh 칸인가, 버퍼).
    machine_edit: Option<(usize, bool, String)>,
    /// 화면 이름을 고치는 기기 — (기기 id, 버퍼).
    device_name_edit: Option<(String, String)>,
    device_account: native_settings::device_account::State,
    /// 마지막으로 화면에 반영한 신원 조회 세대. `settings::probe_generation`.
    probe_seen: u64,
    /// 로그인 중인 슬롯에 붙여넣는 OAuth 코드 버퍼. 진행 중인 로그인은 한 건뿐이라
    /// 슬롯 id 를 함께 들 필요가 없다.
    login_code_edit: String,
    /// Debounced window-frame save deadline: set 1s after every Moved/Resized,
    /// written by about_to_wait. Exit-only persistence lost the frame on a
    /// crash/force-quit.
    window_frame_save_due: Option<std::time::Instant>,
    /// Raw-editor mouse selection in progress: the pane id whose editor owns
    /// the drag (armed on body press, released on mouse-up).
    md_select_drag: Option<String>,
    /// 피드백 본문 편집 버퍼. 설정 창을 닫아도 안 비운다 — 쓰다 만 글이
    /// 실수로 창을 닫았다고 사라지면 다시 안 쓴다.
    feedback_body: String,
    /// 그 버퍼의 캐럿(문자 인덱스). persona 와 나눠 갖는다.
    feedback_caret: usize,
    /// 진단 정보 첨부 스위치.
    feedback_diag: bool,
    feedback_delivery: feedback_delivery::State,
    /// Sidebar "Settings" entry rect (bottom-anchored), for hit-testing.
    settings_btn_rect: (f32, f32, f32, f32),
    /// 사이드바 트레이의 피드백 버튼 rect — 설정과 같은 짝.
    feedback_btn_rect: (f32, f32, f32, f32),
    /// 사이드바 맨 위 현황 줄(`sidebar_pulse.rs`).
    pulse: sidebar_pulse::Pulse,
    /// Cursor-blink phase captured at the last successful render.
    /// Used by `render_frame`'s early-return: a blink toggle counts
    /// as "something changed" and forces the GPU pass even when
    /// every pane is clean.
    last_blink_on: bool,
    /// Chrome-level dirty flag. Set on any non-PTY state change that
    /// needs the next frame to repaint (selection, preedit, focus
    /// shifts, resize, mouse hover, etc.). PTY changes set the
    /// per-pane `PaneState::dirty` instead.
    chrome_dirty: bool,
    /// 테마가 방금 바뀌었으면 (시작 시각, 이전 배경색). 새 팔레트는 즉시 적용되고
    /// 이 값은 그 위에 덮을 **옛 배경**을 들고 있다가 픽셀 블록으로 흩어진다.
    ///
    /// 이전 화면을 통째로 캡처하지 않는 건 그럴 필요가 없어서다 — 눈이 읽는 건
    /// 바탕색이 갈리는 순간이지 그 위 글자가 아니다. 단색 블록만으로 충분하고,
    /// 그래야 전환 중에도 새 화면이 계속 갱신된다(캡처를 덮으면 그동안 화면이 언다).
    theme_fx: Option<(std::time::Instant, [u8; 4])>,
    cursor_px: (f32, f32),
    cursor_sample: Option<handler::PointerSample>,
    /// Headless hover testing: KASATERM_AUTOHOVER="x,y" (logical px) pins
    /// the cursor so a screenshot can capture a hover state without a real
    /// mouse (cliclick needs Accessibility perms). Real CursorMoved events
    /// are ignored while set.
    autohover: Option<(f32, f32)>,
    modifiers: ModifiersState,
    wheel_accum_y: f32,
    last_wheel_emit: Instant,
    /// Last keystroke / IME / mouse press timestamp. Resets the blink
    /// phase so the cursor stays solid while the user is actively
    /// interacting and only fades back to blinking on idle.
    last_input_at: Instant,
    /// Currently-applied logical font size. Mutated by the host_mod+=
    /// / Ctrl+- shortcuts (see `change_font_size`). Starts at the
    /// `FONT_SIZE` constant so first-frame layout matches the original
    /// behavior before any zoom.
    font_size: f32,
    /// Whole-UI zoom multiplier folded into the effective render scale
    /// (`effective_scale = DPI scale × ui_zoom`). 1.0 = native. Ctrl +/-/0
    /// drive this so chrome, sidebar, and every pane scale together.
    ///
    /// 값은 `settings.json` 에 남는다 — 배율은 그 사람 눈과 그 모니터에 맞춘
    /// 것이라 앱을 껐다고 사라지면 매번 다시 맞춰야 한다(2026-09-01 사용자).
    ui_zoom: f32,
    /// 배율을 **아직 아무도 안 골랐다**는 표시. 저장된 `ui_zoom` 이 없을 때만
    /// 참이고, 창이 뜬 뒤 모니터 크기로 한 번 추정할 자격이 된다
    /// (`handler.rs` resumed). 사용자가 Ctrl +/-/0 을 한 번 누르면 값이
    /// 저장되면서 이 플래그가 내려가, 다음 실행부터는 추정이 끼어들지 않는다.
    ui_zoom_unset: bool,
    /// Per-pane font multiplier, keyed by the pane's pty id (the BSP leaf id
    /// the renderer + resize path use). Absent = 1.0. Keyed here rather than
    /// on PaneState because split leaves don't all get a ws.panes entry.
    pane_font_scales: std::collections::HashMap<String, f32>,
    /// 렌더가 pane 마다 얹은 자리 옮김(BSP leaf id 로 키). 복사·선택이 화면과
    /// 같은 글자를 보게 하는 유일한 창구다 — 렌더 파이프라인 한복판에서 만들어지는
    /// 화면 행을 복사 시점에 되짚을 다른 길이 없다.
    pane_view_shift: std::collections::HashMap<String, PaneViewShift>,
    mirror_view_scroll: HashMap<String, usize>,
    /// Wakes the event loop from background threads (PTY snapshots,
    /// socket commands) so a parked WaitUntil repaints immediately.
    proxy: EventLoopProxy<UserEvent>,
    /// 메인 작업공간을 덮는 웹 화면. 장식 없는 자식 창을 본창에 붙여 titlebar와
    /// 왼쪽 sidebar는 남기고, pane grid와 우측 칼럼을 한 면으로 교체한다.
    inline_web: Option<InlineWebHost>,
    /// 대기 중인 계정 전환 확인. `Some` 인 동안 메인 또는 인라인 웹이 스크림을
    /// 깔고 입력을 삼킨다.
    /// **여기 담긴 것 말고는 아무 상태도 미리 안 바뀐다** — 작업대 갈아 끼우기·설정
    /// 저장·pane 재시작은 [전환] 을 눌러야 그때 돈다.
    account_switch_confirm: Option<session::PendingAccountSwitch>,
    /// 학생 교체 확인 카드 — 말투까지 바꾸려면 다시 띄워야 해서 먼저 묻는다.
    character_swap_confirm: Option<session::PendingCharacterSwap>,
    /// Per-frame hit rects for the terminal-pane right-side action cluster
    /// (new-terminal / web / split-v / split-h). Re-built each chrome
    /// paint alongside `image_btn_rects`; the mouse handler matches a
    /// click against it before falling through to tab/plus/cell tests.
    pane_action_hits: Vec<(String, ActionKind, (f32, f32, f32, f32))>,
    /// When the launch build banner began animating. Drives the
    /// hold-then-fade alpha and keeps the frame loop awake (WaitUntil)
    /// only while the banner is still visible.
    version_anim_start: Instant,
    /// macOS menu bar (muda). Held here because the menu must outlive the
    /// app; `git_menu_item` is matched against incoming MenuEvent ids to
    /// toggle the git panel from the menu. 짓고 읽는 곳이 전부 cfg(macos) 라
    /// 다른 플랫폼에선 자리만 차지한다 — 필드째 접으면 생성자까지 갈라져야 해서
    /// 경고만 끈다.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    menu: Option<muda::Menu>,
    git_menu_item: Option<muda::MenuItem>,
    /// "세션 패널" menu item id, matched against MenuEvents to toggle the
    /// session panel.
    session_menu_item: Option<muda::MenuItem>,
    /// 편집 메뉴 복사/붙여넣기 — 네이티브 PredefinedMenuItem 은 Cmd+C/Cmd+V
    /// keyDown 을 가로채 터미널까지 안 내려보낸다(먹통). 커스텀 항목으로 만들어
    /// MenuEvent id 로 매칭, webview 우선 위임 후 폴백으로 직접 클립보드 처리.
    copy_menu_item: Option<muda::MenuItem>,
    paste_menu_item: Option<muda::MenuItem>,
    /// "업데이트 확인" 메뉴 — MenuEvent id 로 매칭해 Sparkle checkForUpdates 를 부른다.
    update_menu_item: Option<muda::MenuItem>,
    /// "kasaterm 종료"(⌘Q) 메뉴 — MenuEvent id 로 매칭해 종료 확인 NSAlert 를 띄운다.
    quit_menu_item: Option<muda::MenuItem>,
    /// Sparkle SPUStandardUpdaterController — 보관해야 판 확인이 유지된다(드롭=정지).
    #[cfg(target_os = "macos")]
    sparkle_updater: Option<macos_sparkle::Updater>,
    /// 새 판 알림 — 업데이터가 찾고 사람이 아직 답하지 않은 판(`update_notice.rs`).
    update_notice: Option<update_notice::Notice>,
    /// 다른 기기 학생의 원격 승인 요청 — 알림·시트·결정(`remote_approval.rs`).
    remote_approval: remote_approval::State,
    /// History store for inline autosuggestion. See autosuggest.rs.
    autosuggest: autosuggest::History,
    /// What the user has typed at the current shell prompt since the last
    /// Enter / line-reset. The source of truth for the suggestion prefix;
    /// validated each frame against the grid (see `update_suggestion`) so
    /// shell-side edits we can't see (Tab-complete, paste) just suppress
    /// the suggestion instead of showing a wrong one.
    input_buf: String,
    /// The remainder currently drawn as ghost text (None = nothing shown).
    /// Recomputed at render time in `update_suggestion`; read by the key
    /// handler so → / Ctrl-E can accept exactly what's on screen.
    current_suggestion: Option<String>,
    /// Whether the left window-tab sidebar is shown. Toggled by the
    /// title-bar button (next to the traffic lights). When false the cell
    /// grid reflows to full width — every origin/layout calc reads
    /// `effective_sidebar_w()` instead of the `SIDEBAR_W` const directly,
    /// so flipping this is all it takes to collapse the strip.
    sidebar_visible: bool,
    /// Window tabs live in the title strip (Windows Terminal-style horizontal
    /// tabs) instead of the left sidebar. `sidebar_layout` swaps its rect math
    /// and `tab_strip_w()` pins the side strip to 0, so render + click routing
    /// follow automatically. Persisted as settings.json `tab_position`.
    tabs_on_top: bool,
    /// 사이드바 방 카드 본문의 **기본** 보기 — 참이면 배치도 대신 학생 줄 목록.
    /// 사이드바 맨 위 「목록 | 배치도」가 바꾸고 settings.json `sidebar_body` 에 남는다.
    /// 값이 없으면 목록(2026-09-29 합의). 방마다 따로 고른 것은 `room_list_body` 가 이긴다.
    sidebar_list_body: bool,
    /// **그 방만** 따로 고른 본문 보기 — 참이면 목록, 거짓이면 배치도. 없는 방은
    /// 위 기본값을 따른다.
    ///
    /// 전에는 전역 값 하나뿐이라 한 방을 목록으로 바꾸면 **모든 방이** 목록이
    /// 됐다(2026-09-15 지적). 방마다 쓰임이 다르다 — 학생이 여럿인 방은 누가
    /// 무엇을 하는지 줄로 읽고 싶고, pane 배치를 자주 바꾸는 방은 지도가 낫다.
    ///
    /// 방 인덱스가 키라 `expanded_windows` 와 똑같이 **remap 을 반드시 통과**해야
    /// 한다 — 안 그러면 2번 방을 목록으로 두고 3번 자리로 끌어 옮겼을 때 엉뚱한
    /// 방이 목록으로 뜬다.
    room_list_body: std::collections::HashMap<usize, bool>,
    /// settings.json 의 모르는 값은 읽는 경계에서 block 으로 떨어뜨린다.
    cursor_shape: cursor::CursorShape,
    cursor_thickness: f32,
    /// 터미널 셀 위 마우스 포인터 — `"arrow"`(기본) · `"ibeam"`. settings.json
    /// `mouse_cursor`. 텍스트 입력칸 위 I-beam 은 이 값과 무관하게 늘 뜬다.
    mouse_cursor: String,
    /// macOS `.md` 더블클릭이 cold-launch(앱 꺼진 채)로 들어오면 odoc 이벤트가
    /// `resumed()`(window·pty_layout 생성) 전에 도착할 수 있다. 그때 경로를 여기
    /// 쌓아두고 start_pty 직후 flush 한다(빈손이면 무비용). 앱 켜진 채 더블클릭은
    /// 디퍼 없이 바로 `open_markdown_window`.
    pending_open_md: Vec<std::path::PathBuf>,
    /// Detached document windows and their launch/restore queue. Kept behind
    /// one field so editor-window lifetime does not spread across App.
    aux: auxwin::AuxWindows,
    /// 떠 있는 완료 알림 배너들(가장 오래된 것이 앞). macOS 알림 센터를 자체 서명
    /// 번들이 못 쓰기 때문에 우리가 그린다 — 사연은 `notify_banner` 모듈 doc.
    banners: Vec<notify_banner::Banner>,
    /// 아직 창을 못 만든 배너 요청 `(제목, 본문, 학생)`. 창 생성은 `ActiveEventLoop`
    /// 가 있어야 하는데 알림이 오는 자리(`handle_notify`)엔 그게 없다 — 거기서
    /// 줄을 세우고 `about_to_wait` 에서 만든다.
    /// 웹 pane 실물(자식 창 + webview). 키 = `WebPane.host_id`. GUI 스레드
    /// 전용 — `Workspace` 는 PTY 스레드와 공유라 !Send 인 webview 를 못 담는다.
    web_hosts: HashMap<u64, webpane::WebHost>,
    /// `WebPane.host_id` 발급 시퀀스.
    web_host_seq: u64,
    /// 웹 pane 헤더 주소창의 인라인 편집(한 번에 하나). None = 편집 아님.
    web_addr: Option<WebAddrEdit>,
    /// 웹 pane 페이지 내 찾기(Cmd+F, 한 번에 하나). 주소창과 같은 버퍼 꼴을
    /// 쓰고 헤더의 같은 pill 자리에 그린다 — 서로 배타(한쪽 begin 이 다른쪽
    /// cancel).
    web_find: Option<WebAddrEdit>,
    /// pane 마다 기억하는 대화 보기 — chat_view.rs.
    chat_view: crate::chat_view::ChatViews,
    /// 거울 셸 칸의 명령 묶음 보기(`shell_view.rs`).
    shell_view: crate::shell_view::ShellViews,
    /// 세션 복원이 앉힌 웹 pane 의 자식 창 대기열 `(host_id, url)` — 복원
    /// 경로엔 ActiveEventLoop 가 없어 창을 못 만든다. about_to_wait 가 다음
    /// 턴에 걷어 spawn_web_host 로 실물을 만든다.
    pending_web_hosts: Vec<(u64, String)>,
    /// 참조 그림 한 장으로 테마 캐릭터를 굽는 잡·프로바이더 감지 캐시. 정의 본체는
    /// `themegen.rs` 에 있다 — 이 struct 에 필드를 평평하게 늘리면 서로 다른 기능을
    /// 만지는 작업끼리 같은 정의 줄에서 충돌한다.
    themegen: themegen::ThemeGenState,
    mirror_sync: mirror_sync::MirrorSyncState,
}

impl App {
    fn new(proxy: EventLoopProxy<UserEvent>, viewer_only: bool, lite: bool) -> Self {
        PANE_HEADER_BAR.store(socket::read_pane_header_bar(), std::sync::atomic::Ordering::Relaxed);
        if !viewer_only {
            let visual_proxy = proxy.clone();
            kasa_mcp::visual::register_producer(Arc::new(move || {
                let _ = visual_proxy.send_event(UserEvent::Redraw);
            }));
        }
        // 마지막에 쓰던 화면 배치. 검증 실행은 저장도 안 하니 읽지도 않는다(짝이 맞는다).
        let ui = if verification_run() { socket::WindowUi::default() } else { socket::read_window_ui() };
        Self {
            viewer_only,
            lite,
            viewer_resumed: false,
            web_visual: Default::default(),
            window: None,
            gpu: None,
            lsp: None,
            hover: None,
            image_tip: None,
            image_hover_cell: None,
            lsp_goto: None,
            tmux: None,
            pty: HashMap::new(),
            pty_layout: None,
            pending_restores: Vec::new(),
            pending_unjiggle: Vec::new(),
            migrate_queue: Vec::new(),
            autoquit_at: None,
            pending_capture: Vec::new(),
            pending_capture_reply: Vec::new(),
            pending_autogit: None,
            autosplit_plan: Vec::new(),
            autosplit_at: None,
            autoopen_path: None,
            autoopen_at: None,
            autoconfirm_at: None,
            autodrag_plan: None,
            autodrag_at: None,
            autopanemove_dst: None,
            autopanemove_at: None,
            force_drag_leaf: None,
            force_drag_at: None,
            autowindow_left: 0,
            autowindow_at: None,
            autotoggle_sidebar_at: None,
            autotoggle_left: 0,
            autotabs_n: 0,
            autotabs_at: None,
            dead_panes: Arc::new(Mutex::new(Vec::new())),
            ws: Arc::new(Mutex::new(Workspace::default())),
            sessions: vec![None],
            active_session: 0,
            windows: vec![None],
            active_window: 0,
            cell: CellGeom::default(),
            preedit: String::new(),
            in_preedit: false,
            ime_cursor_px: None,
            os_ime_surface: None,
            commit_overlay: None,
            ime_active: false,
            hangul: kasa_ime::Composer::new(),
            ime_focus: None,
            pane_handle_rects: Vec::new(),
            pane_top_zones: Vec::new(),
            handle_hovered: false,
            handle_zone_hovered: false,
            // testkit: KASATERM_FORCE_HANDLE_MENU=%N 으로 그 pane ⋮ 메뉴를
            // 부팅 시 강제로 열어 자동캡처로 메뉴 레이아웃을 검증한다. 미설정이면 None.
            handle_menu: std::env::var("KASATERM_FORCE_HANDLE_MENU").ok(),
            handle_menu_hits: Vec::new(),
            // KASATERM_FORCE_HANDLE_MENU 와 같은 헤드리스 검증용 — 클릭 합성 없이
            // 드롭다운이 열린 프레임을 캡처한다.
            account_menu: std::env::var_os("KASATERM_FORCE_ACCOUNT_MENU").is_some(),
            account_menu_rect: None,
            account_menu_body_rect: None,
            account_menu_submenu_rect: None,
            account_menu_submenu_body_rect: None,
            account_menu_corridor_rect: None,
            account_menu_submenu_scroll: 0.0,
            account_menu_submenu_scroll_max: 0.0,
            account_menu_submenu_hit_start: 0,
            account_menu_scroll: 0.0,
            account_menu_scroll_max: 0.0,
            account_menu_suppressed_buttons: Vec::new(),
            account_menu_escape_release: false,
            account_chip_rect: None,
            status_account_rect: None,
            status_version_rect: None,
            set_account_scope_home: false,
            account_menu_anchor: None,
            account_menu_hits: Vec::new(),
            md_content_h: HashMap::new(),
            md_block_ys: HashMap::new(),
            md_scroll_anchor: HashMap::new(),
            md_word_rects: HashMap::new(),
            md_render_sel: None,
            md_scroll_anim: HashMap::new(),
            md_click_streak: None,
            md_find_rects: Vec::new(),
            md_body_rects: HashMap::new(),
            md_task_hits: HashMap::new(),
            md_link_hits: HashMap::new(),
            md_copy_hits: HashMap::new(),
            pane_tab_rects: Vec::new(),
            pane_tab_close_rects: Vec::new(),
            pane_restart_chip_rects: Vec::new(),
            pane_plus_rects: Vec::new(),
            pane_busy_check: None,
            pane_account_quiet_since: HashMap::new(),
            pane_bg_mtime: HashMap::new(),
            pane_deep_prompts: HashMap::new(),
            pane_deep_want: Default::default(),
            pane_sticky_turn: Default::default(),
            window_tab_rects: Vec::new(),
            close_freeze: CloseFreeze::default(),
            sidebar_row_rects: Vec::new(),
            sidebar_mini_rects: Vec::new(),
            sidebar_menu: None,
            sidebar_menu_rects: Vec::new(),
            window_tab_close_rects: Vec::new(),
            win_tab_first: 0,
            sidebar_scroll_px: 0.0,
            win_tab_vis: usize::MAX,
            win_tab_wheel_accum: 0.0,
            win_tab_drag: None,
            closed_panes: Vec::new(),
            room_list_add_rect: None,
            shell_menu_open: false,
            shell_menu_hits: Vec::new(),
            pending_shell: None,
            window_labels: Vec::new(),
            window_labels_at: None,
            window_name_override: HashMap::new(),
            room_rename: RoomRename::default(),
            selection: None,
            drag_anchor: None,
            link_armed: None,
            resize_drag: None,
            last_divider_pos: None,
            last_divider_pty_resize: None,
            header_drag: None,
            tab_drag: None,
            drag_orig_layout: None,
            drag_live_applied: None,
            image_pan_drag: None,
            text_cursor_shown: false,
            confirm_close: None,
            confirm_native: false,
            confirm_btn_rects: Vec::new(),
            restore_prompt: None,
            restore_applying: None,
            restore_progress: None,
            restore_retry_rect: None,
            restore_toast_rect: None,
            restore_btn_rects: Vec::new(),
            session_saved_at: std::time::Instant::now(),
            session_touched: false,
            session_saved_hash: None,
            pane_tab_hover: None,
            image_btn_rects: Vec::new(),
            sidebar_w_logical: ui.sidebar_w.unwrap_or(SIDEBAR_W),
            sidebar_resize: None,
            last_resized_cells: (0, 0),
            pending_resize: None,
            mouse_forward_pane: None,
            hover_probe: None,
            last_left_click: None,
            cell_click: None,
            last_tree_click: None,
            zoomed_pane: None,
            human_touch: None,
            pane_opener: HashMap::new(),
            saved_window_frame: None,
            titlebar_drag_pending: None,
            last_window_title: None,
            claude_busy_until: None,
            last_claude_status: None,
            pane_activity: HashMap::new(),
            pane_ultracode: std::collections::HashSet::new(),
            tell_waiting: HashMap::new(),
            theme_light_last: None,
            retheme_queue: HashMap::new(),
            window_focused: true,
            notify_flash: HashMap::new(),
            held_trouble: HashMap::new(),
            account_flash: None,
            account_switch_from_peer: false,
            turn_done_panes: std::collections::HashSet::new(),
            booted_at: std::time::Instant::now(),
            window_alert: std::collections::HashSet::new(),
            blink_quiet: std::collections::HashSet::new(),
            expanded_windows: std::collections::HashSet::new(),
            expand_anim: None,
            sidebar_row_drag: None,
            pane_claude_seen: std::collections::HashSet::new(),
            unread_panes: std::collections::HashSet::new(),
            dock_badge_n: 0,
            collab: Default::default(),
            last_window_title_check: None,
            pane_cwd_cache: HashMap::new(),
            pane_view_cwd: HashMap::new(),
            pending_room: None,
            next_room_seq: 1,
            pending_character: None,
            last_auto_character: None,
            pane_agent_launches: HashMap::new(),
            pending_spawn_cwd: None,
            remote_keep: std::collections::HashSet::new(),
            migrate_handoff: None,
            pane_session_id: HashMap::new(),
            pane_claude_sid: HashMap::new(),
            pane_last_seat: HashMap::new(),
            remote_view_push_at: None,
            pane_account_stale: HashMap::new(),
            socket_backend: None,
            shared_backend: Default::default(),
            bg_agents: std::sync::Arc::new(std::sync::Mutex::new(HashMap::new())),
            claude_usage: std::sync::Arc::new(std::sync::Mutex::new(None)),
            claude_usage_all: std::sync::Arc::new(std::sync::Mutex::new(HashMap::new())),
            window_git: std::sync::Arc::new(std::sync::Mutex::new(HashMap::new())),
            git_poll_cwds: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
            pane_status_pub: std::sync::Arc::new(std::sync::Mutex::new(HashMap::new())),
            git: state::GitState {
                col_visible: std::env::var("KASASPACE_GIT_PANEL")
                    .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                    .unwrap_or_else(|_| ui.git_col_visible.unwrap_or(false)),
                col_w_logical: ui.git_col_w.unwrap_or(GIT_COL_W),
                // KASATERM_FORCE_ACCOUNT_MENU 와 같은 헤드리스 검증용 — 클릭 합성
                // 없이 모달이 열린 프레임을 캡처한다(모달은 창 안 모든 것 위에
                // 떠야 해서 겹침 회귀가 나기 쉬운 자리다).
                commit_modal_open: std::env::var_os("KASATERM_FORCE_COMMIT_MODAL").is_some(),
                commit_modal_include_unstaged: true,
                ..Default::default()
            },
            info: state::InfoState {
                // 헤드리스 검증용 — 시작 탭을 Info 로. 탭 전환은 클릭이라
                // PTY-only autosend 로는 재현할 수 없다(KASATERM_TEST_FILETREE
                // 가 사이드바를 강제로 여는 것과 같은 이유).
                tab: if std::env::var("KASATERM_TEST_INFO").is_ok() {
                    state::SideTab::Info
                } else if std::env::var("KASATERM_TEST_SESSIONS").is_ok() {
                    state::SideTab::Sessions
                } else if std::env::var("KASATERM_TEST_MCP").is_ok() {
                    state::SideTab::Mcp
                } else {
                    state::SideTab::Git
                },
                ..Default::default()
            },
            sessions_col: state::SessionsColState {
                // 헤드리스 검증에서 방 하나에 기록이 없으면 빈 목록만 찍힌다 —
                // 전체 범위로 열어야 목록이 실제로 그려진 프레임을 캡처할 수 있다.
                scope_all: std::env::var("KASATERM_TEST_SESSIONS").is_ok_and(|v| v == "all"),
                ..Default::default()
            },
            mcp_col: state::McpColState {
                // 추가 칸은 열어야만 보이는데 헤드리스 캡처는 클릭을 못 한다.
                add: std::env::var("KASATERM_TEST_MCP")
                    .ok()
                    .filter(|v| v == "add")
                    .map(|_| state::McpAddForm {
                        harness: "claude",
                        ..Default::default()
                    }),
                ..Default::default()
            },
            statusbar: Default::default(),
            turn: Default::default(),
            pane_cwd_check: None,
            // Alt 를 누르는 동안만 뜨는 오버레이라 헤드리스로는 찍을 길이 없다.
            // 이 env 로 켠 채 띄우면 자동 캡처가 그 화면을 잡는다(검증 전용).
            show_pane_numbers: std::env::var("KASATERM_AUTOPANENUM").is_ok(),
            file_tree: state::FileTreeState {
                // Headless test override (KASATERM_TEST_FILETREE) forces the
                // sidebar open at launch so quick-files/tree captures render
                // without needing a chrome click the PTY-only autosend can't do.
                // 마지막에 쓰던 대로가 먼저고, 그 기록이 없을 때만 설정의 기본값이다.
                visible: ui.file_tree_visible.unwrap_or_else(socket::read_file_tree_default)
                    || std::env::var("KASATERM_TEST_FILETREE").is_ok(),
                w_logical: ui.file_tree_w.unwrap_or(FILE_TREE_W),
                ..Default::default()
            },
            git_ignore_req: std::sync::Arc::new(std::sync::Mutex::new(None)),
            git_ignore_started: false,
            // 헤드리스 초기 열림은 KASATERM_AUTOSETTINGS(testkit)가 event_loop
            // 위에서 담당한다.
            settings_scene: settings_room::SettingsScene::default(),
            set_cwd_mode: socket::read_default_cwd_mode(),
            set_file_open_mode: socket::read_file_open_mode(),
            set_file_open_app: socket::read_file_open_app(),
            set_file_open_cmd: socket::read_file_open_cmd(),
            set_file_tree_default: socket::read_file_tree_default(),
            set_footer_default: socket::read_footer_default(),
            set_autosave: socket::read_editor_autosave(),
            set_shell: socket::read_default_shell().unwrap_or_default(),
            set_wheel_pixel_gain: socket::read_wheel_pixel_gain(),
            set_status_h: socket::read_status_h(),
            set_pane_footer_h: socket::read_pane_footer_h(),
            set_claude_persona: socket::read_claude_persona(),
            set_shim_inject: socket::read_shim_inject(),
            set_palette_edit: String::new(),
            set_picker_hsv: (0.0, 0.0, 0.0),
            set_claude_model: socket::read_claude_model(),
            set_claude_effort: socket::read_claude_effort(),
            set_claude_extra: socket::read_claude_extra(),
            set_claude_accounts: socket::read_claude_accounts(),
            set_claude_account: socket::read_claude_account(),
            set_codex_accounts: socket::read_codex_accounts(),
            set_codex_account: socket::read_codex_account(),
            set_usage_compact: socket::read_usage_compact(),
            set_statusbar: statusbar_config::Prefs::from_settings(&socket::read_settings()),
            weather: weather::WeatherState::with_settings(weather::WeatherState::load(socket::read_settings().get("weather"))),
            account_menu_provider: None,
            set_account_autoswitch: socket::read_account_autoswitch(),
            set_account_autoswitch_pct: socket::read_account_autoswitch_pct(),
            settings_input: None,
            // KASATERM_TEST_STUDENT=<이름> 이면 그 캐릭터를 선택 상태로 시드해
            // persona 편집기 렌더를 헤드리스로 캡처할 수 있게 한다(테스트 전용).
            students_selected: std::env::var("KASATERM_TEST_STUDENT")
                .ok()
                .filter(|s| !s.is_empty()),
            students_theme: socket::read_character_theme(),
            students_slug: String::new(),
            students_model: String::new(),
            students_backend: String::new(),
            students_persona: std::env::var("KASATERM_TEST_STUDENT")
                .ok()
                .filter(|s| !s.is_empty())
                .and_then(|n| {
                    kasa_mcp::character::characters_json()
                        .and_then(|c| kasa_mcp::character::raw_persona_for(&c, &n))
                })
                .unwrap_or_default(),
            students_caret: 0,
            students_raw: StudentRawEdit::default(),
            students_name: std::env::var("KASATERM_TEST_STUDENT").unwrap_or_default(),
            theme_label_edit: None,
            custom_theme_label_edit: None,
            account_label_edit: None,
            machine_edit: None,
            device_name_edit: None,
            device_account: native_settings::device_account::State::default(),
            probe_seen: 0,
            login_code_edit: String::new(),
            settings_caret: 0,
            window_frame_save_due: None,
            md_select_drag: None,
            feedback_body: socket::read_feedback_draft(),
            feedback_caret: 0,
            feedback_diag: true,
            feedback_delivery: feedback_delivery::State::default(),
            settings_btn_rect: (0.0, 0.0, 0.0, 0.0),
            feedback_btn_rect: (0.0, 0.0, 0.0, 0.0),
            pulse: sidebar_pulse::Pulse::from_settings(),
            last_blink_on: false,
            chrome_dirty: true,
            theme_fx: None,
            cursor_sample: None,
            cursor_px: std::env::var("KASATERM_AUTOHOVER")
                .ok()
                .and_then(|s| {
                    let (a, b) = s.split_once(',')?;
                    Some((a.trim().parse().ok()?, b.trim().parse().ok()?))
                })
                .unwrap_or((0.0, 0.0)),
            autohover: std::env::var("KASATERM_AUTOHOVER").ok().and_then(|s| {
                let (a, b) = s.split_once(',')?;
                Some((a.trim().parse().ok()?, b.trim().parse().ok()?))
            }),
            modifiers: ModifiersState::empty(),
            wheel_accum_y: 0.0,
            last_wheel_emit: Instant::now() - std::time::Duration::from_secs(1),
            last_input_at: Instant::now(),
            font_size: socket::read_font_size(),
            ui_zoom: socket::read_ui_zoom().unwrap_or(1.0),
            ui_zoom_unset: socket::read_ui_zoom().is_none(),
            pane_font_scales: std::collections::HashMap::new(),
            pane_view_shift: std::collections::HashMap::new(),
            mirror_view_scroll: HashMap::new(),
            proxy,
            inline_web: None,
            account_switch_confirm: None,
            character_swap_confirm: None,
            pane_action_hits: Vec::new(),
            version_anim_start: Instant::now(),
            menu: None,
            git_menu_item: None,
            session_menu_item: None,
            copy_menu_item: None,
            paste_menu_item: None,
            update_menu_item: None,
            quit_menu_item: None,
            #[cfg(target_os = "macos")]
            sparkle_updater: None,
            update_notice: None,
            remote_approval: Default::default(),
            autosuggest: autosuggest::History::new(),
            input_buf: String::new(),
            current_suggestion: None,
            // Default closed — single-pane, no chrome reads as a plain
            // terminal at first launch. User toggles via the title-bar
            // button or the "보기 → 세션 패널" menu item.
            // 카드 덱 호버 명단은 사이드바 배치도 위에서만 뜬다. 헤드리스는
            // 마우스를 못 움직이니 AUTODECKTIP 일 때만 열어 둔 채 띄운다.
            sidebar_visible: std::env::var_os("KASATERM_AUTODECKTIP").is_some()
                || ui.sidebar_visible.unwrap_or(false),
            // 기본은 side(사이드바 탭) — read_tab_position 이 "top" 만 top 으로,
            // 그 외/키없음은 side 로 폴백한다.
            tabs_on_top: socket::read_tab_position() == "top",
            sidebar_list_body: socket::read_settings()
                .get("sidebar_body")
                .and_then(|v| v.as_str())
                != Some("map"),
            room_list_body: std::collections::HashMap::new(),
            // lite 는 Ghostty 프롬프트처럼 얇은 바 — 설정 화면이 색뿐이라 여기서 박는다.
            cursor_shape: if lite { cursor::CursorShape::Bar } else { socket::read_cursor_shape() },
            cursor_thickness: if lite { 1.0 } else { socket::read_cursor_thickness() },
            // lite 는 Ghostty 처럼 글자 위에서 I-beam.
            mouse_cursor: if lite { "ibeam".to_string() } else { socket::read_mouse_cursor() },
            pending_open_md: Vec::new(),
            aux: auxwin::AuxWindows::load(viewer_only),
            banners: Vec::new(),
            web_hosts: HashMap::new(),
            web_host_seq: 0,
            web_addr: None,
            web_find: None,
            chat_view: Default::default(),
            shell_view: Default::default(),
            pending_web_hosts: Vec::new(),
            themegen: Default::default(),
            mirror_sync: Default::default(),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
struct ViewerLaunch {
    viewer_only: bool,
    /// 터미널만 남긴 고정판(KasaLite). 캐릭터·board·서버·자기설치가 뜨지 않고
    /// 설정·소켓은 `~/.config/kasaterm-lite` 로 갈린다 — 본판을 굽고 껐다 켜도
    /// 이 창은 그대로다. `viewer_only` 와 배타이며 뷰어 동작은 하나도 안 탄다.
    lite: bool,
    paths: Vec<std::path::PathBuf>,
}

impl ViewerLaunch {
    fn detect(executable: Option<&std::ffi::OsStr>, mut args: Vec<std::ffi::OsString>) -> Self {
        let stem = executable.and_then(|name| std::path::Path::new(name).file_stem());
        let named_viewer = stem == Some(std::ffi::OsStr::new("kasaterm-viewer"));
        let named_lite = stem == Some(std::ffi::OsStr::new("kasaterm-lite"));
        let flagged = args.first().is_some_and(|arg| arg == "--viewer");
        let flagged_lite = args.first().is_some_and(|arg| arg == "--lite");
        if flagged || flagged_lite {
            args.remove(0);
        }
        let viewer_only = named_viewer || flagged;
        Self {
            viewer_only,
            lite: !viewer_only && (named_lite || flagged_lite),
            paths: args.into_iter().map(std::path::PathBuf::from).collect(),
        }
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    // Viewer mode is decided before the first logger/config/shim write. A
    // renamed bundle executable and `kasaterm --viewer` share the same code,
    // but only the terminal app may perform startup maintenance.
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    let executable = std::env::current_exe()
        .ok()
        .and_then(|path| path.file_name().map(std::ffi::OsStr::to_owned));
    let launch = ViewerLaunch::detect(executable.as_deref(), args);
    if !launch.viewer_only {
        let log_suffix = if launch.lite { "-lite" } else { "" };
        install_panic_logger(log_suffix);
        install_stderr_log(log_suffix);
        scrub_inherited_claude_markers();
        if launch.lite {
            LITE_MODE.store(true, std::sync::atomic::Ordering::Relaxed);
            apply_lite_env();
        }
    // `open`(1) doesn't forward shell env to the launched .app, but the
    // .app's screen-recording TCC permission only applies when launched
    // via `open` (not when the binary runs directly). So a capture/test
    // config file is how we still drive autocapture/autosplit through an
    // `open`-launched instance. Loaded (and deleted) before anything
    // reads KASATERM_* vars.
        load_capture_config();
        install_pty_host_policy();
        kasa_mcp::install_collab_env();
        socket::prepare_session_storage();
    // 기기 이름 수집은 첫 화면을 준비하는 동안 끝낸다. Info를 열어야
    // 이름·기기색이 생기거나 렌더 스레드에서 scutil을 띄우지 않도록 한다.
        std::thread::spawn(info::local_machine_name);
    // 첫 설치 여부는 어떤 부팅 작업도 ~/.config/kasaterm 을 만들기 전에 확정한다.
    // 기존 설정 파일이 있던 사용자는 완료 표식만 보강하고 화면을 띄우지 않는다.
        if !launch.lite {
            onboarding::prepare_boot();
        }
    // Apply the persisted theme + accent into the global color slots before any
    // window or pane paints, so the first frame is already in the right palette.
        theme::apply_from_settings();
        render::pane_identity::reload_device_colors();
    // Install pane shims before anything spawns a shell — every PtySession
    // reads KASATERM_TMUX_SHIM_DIR we set here (kasaterm-cli/preview/OSC133).
    // best-effort: failures just log and skip, the rest still works.
        if !launch.lite && !verification_run() {
            let _ = crate::claude_auth::recover_workbench_account(&socket::read_claude_accounts());
        }
        install_pane_shims(launch.lite);
    // 죽은 인스턴스가 남긴 소켓 잔재 청소(재시작·빌드 반복 누적). 살아있는
    // 소켓은 connect 로 가려 건드리지 않으므로 멀티 인스턴스에서도 안전.
    // 그렇게 얻은 live pid 목록으로 죽은 인스턴스의 캐릭터 마커도 지운다 — 예전엔
    // "다른 인스턴스가 하나도 없을 때만" 이라는 게이트를 뒀는데, 개발용 `cargo run`
    // 하나만 떠 있어도 청소가 통째로 건너뛰어져 마커가 재시작마다 쌓였다(그 끝이
    // 배정 풀 고갈 = 같은 학생 중복). 이제 주인 pid 로 가리므로 게이트가 필요 없다.
        // lite 는 마커를 안 쓰고, 청소 함수들은 본판 설정 폴더를 하드코딩으로 본다.
        if !verification_run() && !launch.lite {
            let live = live_kasaterm_pids();
            // An isolated/custom socket directory is not a complete process
            // inventory. Keep any owner still alive outside that directory;
            // if process enumeration fails, do not sweep other owners at all.
            let processes = kasa_pty::fresh_process_table();
            kasa_mcp::character::sweep_stale_markers(|pid| live.contains(&pid)
                || processes.is_empty() || processes.iter().any(|(p, _, _)| *p == pid));
            prune_finished_tasks();
            prune_empty_inboxes();
        }
    } else {
        // Palette reads are required by the shared document renderer; unlike
        // onboarding/shims this path does not write or start a service.
        theme::apply_viewer_palette_read_only();
    }
    // 헤드리스 검증 실행이 사용자 화면을 뺏지 않게 한다. 스스로 종료하는 실행
    // (`KASATERM_AUTOQUIT_MS`)은 정의상 테스트라 자동으로 배경에 띄운다 —
    // Accessory 정책이면 Dock/⌘Tab 에도 안 올라오고 활성 앱도 안 바뀌므로,
    // 캡처를 도는 동안 사용자가 하던 창에 그대로 머문다. `KASATERM_NO_FOCUS`
    // 로 직접 켜고 끌 수도 있다(0/false 면 강제로 평소처럼 뜬다).
    let mut builder = EventLoop::<UserEvent>::with_user_event();
    #[cfg(target_os = "macos")]
    if !launch.viewer_only && background_launch() {
        use winit::platform::macos::{ActivationPolicy, EventLoopBuilderExtMacOS};
        builder
            .with_activation_policy(ActivationPolicy::Accessory)
            .with_activate_ignoring_other_apps(false);
    }
    let event_loop = builder.build()?;
    let proxy = event_loop.create_proxy();
    #[cfg(target_os = "macos")]
    macos_open::prepare_open_doc_handler(proxy.clone());
    turn_probe::start_lag_probe(proxy.clone());
    // 원격 호스트의 `open-url` 되돌림을 GUI 이벤트로 — kasa-mcp 는 창을 모른다.
    if !launch.viewer_only {
        let p = std::sync::Mutex::new(proxy.clone());
        kasa_mcp::remote::set_open_url_sink(Box::new(move |pane, url| {
            if let Ok(p) = p.lock() {
                let _ = p.send_event(UserEvent::RemoteOpenUrl(pane.to_string(), url.to_string()));
            }
        }));
    }
    // argv 폴백: `kasaterm file.md` / 커맨드라인. `.md` 인자면 새 워크스페이스
    // 마크다운으로 위임(resumed 전이면 디퍼됐다 start_pty 후 flush). `open`(1)은
    // odoc 로만 오므로 둘이 겹쳐도 open_markdown_window 의 dedup 이 흡수한다.
    if !launch.viewer_only {
        for p in &launch.paths {
            let is_md = p
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.eq_ignore_ascii_case("md") || e.eq_ignore_ascii_case("markdown"))
                .unwrap_or(false);
            if is_md && p.is_file() {
                if let Ok(abs) = std::fs::canonicalize(p) {
                    let _ = proxy.send_event(UserEvent::OpenMarkdownWindow(
                        abs.to_string_lossy().into_owned(),
                    ));
                }
            }
        }
    }
    let mut app = App::new(proxy, launch.viewer_only, launch.lite);
    if launch.viewer_only {
        app.configure_viewer_vault_startup(!launch.paths.is_empty());
        for path in launch.paths {
            app.queue_aux_file(path, true);
        }
    }
    event_loop.run_app(&mut app)?;
    Ok(())
}

#[cfg(test)]
mod viewer_launch_tests {
    use super::ViewerLaunch;
    use std::ffi::OsStr;

    #[test]
    fn executable_name_and_flag_select_viewer_without_consuming_paths() {
        let named = ViewerLaunch::detect(
            Some(OsStr::new("kasaterm-viewer")),
            vec!["one.md".into()],
        );
        assert!(named.viewer_only);
        assert_eq!(named.paths, vec![std::path::PathBuf::from("one.md")]);

        let flagged = ViewerLaunch::detect(
            Some(OsStr::new("kasaterm")),
            vec!["--viewer".into(), "two.md".into()],
        );
        assert!(flagged.viewer_only);
        assert_eq!(flagged.paths, vec![std::path::PathBuf::from("two.md")]);
    }

    #[test]
    fn ordinary_kasaterm_stays_in_terminal_mode() {
        let launch = ViewerLaunch::detect(
            Some(OsStr::new("kasaterm")),
            vec!["note.md".into()],
        );
        assert!(!launch.viewer_only);
        assert!(!launch.lite);
    }

    #[test]
    fn lite_by_executable_name_or_flag_never_becomes_viewer() {
        let named = ViewerLaunch::detect(Some(OsStr::new("kasaterm-lite")), vec![]);
        assert!(named.lite && !named.viewer_only);

        let flagged = ViewerLaunch::detect(
            Some(OsStr::new("kasaterm")),
            vec!["--lite".into(), "note.md".into()],
        );
        assert!(flagged.lite && !flagged.viewer_only);
        assert_eq!(flagged.paths, vec![std::path::PathBuf::from("note.md")]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn remaining_duration_uses_days_hours_and_minutes() {
        for (seconds, expected) in [
            (0, "곧"), (59, "곧"), (60, "1분"), (3599, "59분"),
            (3600, "1시간"), (484 * 60, "8시간 4분"),
            (24 * 3600, "1일"), (177 * 3600, "7일 9시간"),
        ] {
            assert_eq!(remaining_duration_label(seconds), expected);
            if seconds > 0 {
                assert_eq!(resets_in_label_at(Some(1000 + seconds), 1000).as_deref(), Some(expected));
            }
        }
        assert_eq!(resets_in_label_at(None, 1000), None);
        assert_eq!(resets_in_label_at(Some(999), 1000), None);
        assert_eq!(resets_in_label_at(Some(1000), 1000), None);
    }

    /// 화면 한 줄을 실제 그리드처럼 만든다 — 넓은 글자 뒤에 뒷칸(진짜 공백)이 붙는다.
    fn grid_row(s: &str, width: usize) -> Vec<GridCell> {
        use unicode_width::UnicodeWidthChar;
        let mut row: Vec<GridCell> = Vec::new();
        for ch in s.chars() {
            let mut c = GridCell::blank();
            c.ch = ch;
            row.push(c);
            if ch.width().unwrap_or(1) > 1 {
                row.push(GridCell::blank());
            }
        }
        while row.len() < width {
            row.push(GridCell::blank());
        }
        row.truncate(width);
        row
    }

    /// 보기 배치가 없는 pane(거울이 아닌 모든 pane)은 원본 그대로.
    #[test]
    fn copy_without_shift_is_untouched() {
        const W: usize = 8;
        let base: Vec<Vec<GridCell>> = (0..3).map(|i| grid_row(&format!("b{i}"), W)).collect();
        let view = PaneViewShift::default().compose(&base);
        assert_eq!(view, base);
    }

    /// 올려다보는 화면의 **맨 윗줄에 본문이 있어도** 띠가 붙는다.
    ///
    /// 전에는 최상단 행이 빈 화면에만 얹었는데, 실화면의 맨 윗줄은 대개 접힌 본문
    /// 조각이라 그 조건이 거의 안 걸렸다 — 「스크롤하면 붙는다」가 아니라 「됐다
    /// 안 됐다 한다」로 보인 원인이다(2026-08-31 실측).
    #[test]
    fn sticky_band_covers_top_row_with_body() {
        const W: usize = 80;
        let screen = [
            "신호입니다.",
            "- 하지만 굽기엔 codex 작업이 안 들어갔어요.",
            "두 가지 여쭤요 -",
            "❯ 끝났어 워크트리깨끗하게 커밋푸시하고 빌드해줘",
            "⏺ 끝났으면 커밋·푸시·빌드까지 하겠습니다.",
            "Jump to bottom: fn+↓ to scroll",
        ];
        let rows: Vec<Vec<GridCell>> = screen.iter().map(|l| grid_row(l, W)).collect();
        assert!(
            crate::screenread::scrolled_gate(&rows),
            "안내 문구가 있으면 게이트가 열려야 한다"
        );

        let prompts = vec![
            (
                "codex 작업 상태 알려줘".to_string(),
                vec!["앞 턴의 답변 한 줄".to_string()],
            ),
            (
                "끝났어 워크트리깨끗하게 커밋푸시하고 빌드해줘".to_string(),
                vec!["끝났으면 커밋·푸시·빌드까지 하겠습니다.".to_string()],
            ),
        ];
        let mut memo = None;
        let got = crate::screenread::find_sticky_prompt(&rows, &prompts, &mut memo)
            .expect("맨 윗줄에 본문이 있어도 띠가 나와야 한다");
        assert!(
            got.cells.is_none(),
            "답은 화면 밖의 앞 질문이라 옮겨 올 셀이 없다"
        );
        assert_eq!(got.row, 0);
        // 화면에 보이는 질문의 **바로 앞** 질문 — 그 위는 화면 밖이다.
        assert_eq!(got.text, "codex 작업 상태 알려줘");
        assert_eq!(memo.as_deref(), Some("codex 작업 상태 알려줘"));
    }

    /// 머리줄이 화면 밖으로 나가도 **직전에 확정한 턴**을 그대로 문다.
    #[test]
    fn sticky_band_remembers_turn_past_header() {
        const W: usize = 80;
        let rows: Vec<Vec<GridCell>> = [
            "앞 턴 답변 한가운데",
            "이어지는 줄",
            "Jump to bottom (click) ↓",
        ]
        .iter()
        .map(|l| grid_row(l, W))
        .collect();
        let prompts = vec![
            (
                "codex 작업 상태 알려줘".to_string(),
                vec!["앞 턴 답변 한가운데".to_string()],
            ),
            (
                "끝났어 커밋푸시하고 빌드해줘".to_string(),
                vec!["끝났으면 커밋까지".to_string()],
            ),
        ];
        let mut memo = Some("codex 작업 상태 알려줘".to_string());
        let got = crate::screenread::find_sticky_prompt(&rows, &prompts, &mut memo)
            .expect("띠가 나와야 한다");
        assert_eq!(got.text, "codex 작업 상태 알려줘");
    }

    fn grid_row_dim(s: &str, width: usize) -> Vec<GridCell> {
        let mut row = grid_row(s, width);
        for c in row.iter_mut() {
            c.dim = true;
        }
        row
    }

    /// 머리줄이 **화면 첫 줄**이면 띠는 그 질문 자신이다 — 한 칸 앞이 아니다.
    #[test]
    fn sticky_band_uses_own_turn_when_header_is_first_row() {
        const W: usize = 80;
        let rows: Vec<Vec<GridCell>> = [
            "❯ 끝났어 커밋푸시하고 빌드해줘",
            "⏺ 끝났으면 커밋·푸시까지 하겠습니다.",
            "Jump to bottom (click) ↓",
        ]
        .iter()
        .map(|l| grid_row(l, W))
        .collect();
        let prompts = vec![
            (
                "codex 작업 상태 알려줘".to_string(),
                vec!["앞 턴 답변".to_string()],
            ),
            (
                "끝났어 커밋푸시하고 빌드해줘".to_string(),
                vec!["끝났으면 커밋·푸시까지 하겠습니다.".to_string()],
            ),
        ];
        let mut memo = None;
        let got = crate::screenread::find_sticky_prompt(&rows, &prompts, &mut memo)
            .expect("띠가 나와야 한다");
        assert_eq!(got.text, "끝났어 커밋푸시하고 빌드해줘");
    }

    /// 한 화면씩 뛰어 머리줄을 놓쳤을 때, 낡은 기억보다 **보이는 본문**이 세다.
    #[test]
    fn sticky_band_prefers_body_over_stale_memo() {
        const W: usize = 80;
        let rows: Vec<Vec<GridCell>> = [
            "첫 턴에만 있는 고유한 문장",
            "이어지는 줄 하나",
            "Jump to bottom (click) ↓",
        ]
        .iter()
        .map(|l| grid_row(l, W))
        .collect();
        let prompts = vec![
            (
                "첫 질문".to_string(),
                vec!["첫 턴에만 있는 고유한 문장".to_string()],
            ),
            ("둘째 질문".to_string(), vec!["둘째 턴 문장".to_string()]),
            ("셋째 질문".to_string(), vec!["셋째 턴 문장".to_string()]),
        ];
        let mut memo = Some("셋째 질문".to_string());
        let got = crate::screenread::find_sticky_prompt(&rows, &prompts, &mut memo)
            .expect("띠가 나와야 한다");
        assert_eq!(got.text, "첫 질문");
        assert_eq!(
            memo.as_deref(),
            Some("첫 질문"),
            "본문으로 되짚었으면 기억도 갱신된다"
        );
    }

    /// 흐릿한 `>` 로 시작해도 **아는 질문이 아니면** 잡지 않는다(본문 인용·diff).
    #[test]
    fn sticky_band_ignores_unknown_marked_row() {
        const W: usize = 80;
        let mut rows: Vec<Vec<GridCell>> = vec![grid_row_dim("> 남의 말을 인용한 줄입니다", W)];
        rows.push(grid_row("첫 턴에만 있는 고유한 문장", W));
        rows.push(grid_row("Jump to bottom (click) ↓", W));
        let prompts = vec![
            (
                "첫 질문".to_string(),
                vec!["첫 턴에만 있는 고유한 문장".to_string()],
            ),
            ("둘째 질문".to_string(), vec!["둘째 턴 문장".to_string()]),
        ];
        let mut memo = None;
        let got = crate::screenread::find_sticky_prompt(&rows, &prompts, &mut memo)
            .expect("띠가 나와야 한다");
        assert!(
            got.cells.is_none(),
            "인용 줄을 그 자리에서 하이라이트하면 안 된다"
        );
        assert_eq!(got.text, "첫 질문");
    }

    /// 질문 넷이 **같은 말로 시작할 때** 머리글자만으로 가리면 전부 첫 질문이 된다.
    /// 실측에서 띠가 어느 자리에서든 1번 질문을 물었던 자리다(2026-08-31).
    #[test]
    fn sticky_band_does_not_confuse_same_prefixed_questions() {
        const W: usize = 100;
        let rows: Vec<Vec<GridCell>> = [
            "  붉은 사막에 모래바람이 길게 분다.",
            "  붉은 사막에 모래바람이 길게 분다.",
            "❯ 아무 도구도 쓰지 말고 「푸른 숲에 새벽 종소리가 울린다」 를 40줄 적어라.",
            "  푸른 숲에 새벽 종소리가 울린다.",
            "Jump to bottom (click) ↓",
        ]
        .iter()
        .map(|l| grid_row(l, W))
        .collect();
        let q = |tail: &str| format!("아무 도구도 쓰지 말고 「{tail}」 를 40줄 적어라.");
        let prompts = vec![
            (
                q("고요한 호수 위로 물안개가 피어오른다"),
                vec!["고요한 호수 위로 물안개가 피어오른다.".to_string()],
            ),
            (
                q("붉은 사막에 모래바람이 길게 분다"),
                vec!["붉은 사막에 모래바람이 길게 분다.".to_string()],
            ),
            (
                q("푸른 숲에 새벽 종소리가 울린다"),
                vec!["푸른 숲에 새벽 종소리가 울린다.".to_string()],
            ),
        ];
        let mut memo = None;
        let got = crate::screenread::find_sticky_prompt(&rows, &prompts, &mut memo)
            .expect("띠가 나와야 한다");
        // 화면 맨 위는 「붉은 사막」 턴의 꼬리다. 머리글자로 가리면 여기서 첫 질문이 나왔다.
        assert_eq!(got.text, q("붉은 사막에 모래바람이 길게 분다"));
    }

    fn ms(t: Instant, n: u64) -> Instant {
        t + Duration::from_millis(n)
    }

    /// 빈 `tabs` 로 프레임을 그려도 죽지 않는다.
    ///
    /// 2026-08-22: 탭 하나짜리 pane 의 알약을 휠 클릭하면 마지막 탭이 지워져
    /// `tabs` 가 비었고, 다음 프레임의 `Deref` 가 `len() - 1` 을 언더플로우해
    /// (`usize::MAX`) 인덱스가 터지면서 **앱이 통째로 죽었다**. 비게 만드는
    /// 경로는 각각 막았지만 `Deref` 는 `PaneState` 를 쓰는 **모든** 자리가
    /// 지나는 길이라, 불변식이 또 깨져도 렌더가 살아남는지를 여기서 못 박는다.
    /// 칸 `%4` 가 「빈 자리」로 남아 남의 학생(`%13`)을 탭으로 들고, `%4` 라는 PTY 는 다른 칸 `%19`
    /// 의 탭에 있을 때 — 표가 낡아 `%4 → %4` 를 가리켜도 화면은 `%19` 로 가야 한다(2026-10-08 히후미
    /// 칸이 15:00 에 얼어붙은 꼴). 아무 칸도 안 든 번호는 남의 칸에 앉히지 않는다.
    #[test]
    fn pty_updates_route_to_the_pane_that_actually_hosts_them() {
        let tab = |pid: &str| PaneTab { pid: Some(pid.to_string()), ..PaneTab::default() };
        let mut ws = Workspace::default();
        ws.panes.insert("%4".into(), PaneState { tabs: vec![tab("%13")], ..PaneState::default() });
        ws.panes.insert("%19".into(), PaneState { tabs: vec![tab("%4")], ..PaneState::default() });
        ws.pid_to_pane.insert("%4".into(), "%4".into());
        assert_eq!(ws.outer_for_pty("%4").as_deref(), Some("%19"), "낡은 표보다 실제로 든 칸");
        let (pane, idx) = ws.find_tab_by_pty("%4").unwrap();
        assert_eq!((pane.tabs[idx].pid.as_deref(), idx), (Some("%4"), 0));
        assert_eq!(ws.outer_for_pty("%13").as_deref(), Some("%4"));
        assert_eq!(ws.outer_for_pty("%77"), None);
        ws.panes.remove("%19");
        assert!(ws.find_tab_by_pty("%4").is_none(), "번호만 같은 남의 칸의 첫 탭에 쓰지 않는다");
        ws.panes.insert("%5".into(), PaneState::default());
        assert_eq!(ws.find_tab_by_pty("%5").map(|(_, i)| i), Some(0), "첫 프레임 전의 새 셸은 제 칸");
    }

    /// 칸 `%9` 이 탭 둘(뒤 `%9` 학생, 앞 `%5` 서버 셸)을 들 때 `%9` 로 보낸 글은 `%9` 에만 간다
    /// (2026-10-09 브리프가 `%5` 셸에서 명령으로 실행됐다). 칸 번호만 남은 자리는 탭이 하나일 때만 잇는다.
    #[test]
    fn send_reaches_the_addressed_tab_not_the_cells_front_tab() {
        let tab = |pid: &str| PaneTab { pid: Some(pid.to_string()), ..PaneTab::default() };
        let mut ws = Workspace::default();
        ws.panes.insert("%9".into(), PaneState { tabs: vec![tab("%9"), tab("%5")], active_tab: 1, ..PaneState::default() });
        let live = |p: &str| matches!(p, "%9" | "%5" | "%7");
        assert_eq!(ws.surface_pty("%9", live).as_deref(), Ok("%9"), "뒤 탭");
        assert_eq!(ws.surface_pty("%5", live).as_deref(), Ok("%5"), "앞 탭");
        assert_eq!(ws.active_tab_pid("%9"), "%5", "옛 길이 고르던 앞 탭");
        ws.panes.insert("%3".into(), PaneState { tabs: vec![tab("%7")], ..PaneState::default() });
        assert_eq!(ws.surface_pty("%3", live).as_deref(), Ok("%7"), "탭 하나뿐인 칸 번호");
        ws.panes.insert("%4".into(), PaneState { tabs: vec![tab("%9"), tab("%7")], ..PaneState::default() });
        assert!(ws.surface_pty("%4", live).is_err(), "탭 여럿인 칸 번호는 고르지 않는다");
        assert!(ws.surface_pty("%77", live).is_err());
    }

    #[test]
    fn empty_tabs_never_panic_the_render() {
        let mut ps = PaneState::default();
        ps.tabs.clear();
        // 옛 코드가 실제로 만들어 낸 값(`close_tab` 의 `len() - 1`). `min()` 으로
        // 못 막는다 — 비교 상대인 `len() - 1` 자체가 `usize::MAX` 라서다.
        ps.active_tab = usize::MAX;

        // 읽기 세 갈래 — 전부 정적 기본 탭으로 떨어진다.
        assert!(ps.active().pid.is_none(), "active()");
        assert!(ps.title.is_none(), "Deref");
        assert!(ps.tab_for_pid("%99").pid.is_none(), "tab_for_pid fallback");
        // 렌더가 프레임마다 묻는 것들도 같은 길을 지난다.
        assert!(!ps.has_header(), "has_header");
        assert!(ps.image().is_none() && ps.markdown().is_none() && ps.web().is_none());

        // 쓰기 쪽은 `&mut self` 라 불변식을 그 자리에서 되돌린다.
        ps.title = Some("복구".into());
        assert_eq!(ps.tabs.len(), 1, "DerefMut 이 기본 탭을 세운다");
        assert_eq!(ps.active_tab, 0, "active_tab 도 함께 눕는다");
        assert_eq!(ps.title.as_deref(), Some("복구"));
    }

    /// 탭 하나를 지워 비더라도 `active_tab` 이 `usize::MAX` 로 넘어가지 않는다
    /// — `close_tab` 이 쓰는 보정식 그대로. 위 그물이 있어도 언더플로우한 값이
    /// 상태에 남으면 저장·복원을 타고 다음 세션까지 따라간다.
    #[test]
    fn active_tab_never_underflows_when_tabs_empty() {
        let mut tabs: Vec<PaneTab> = vec![PaneTab::default()];
        let mut active_tab = 0usize;
        tabs.remove(0);
        if active_tab >= tabs.len() {
            active_tab = tabs.len().saturating_sub(1);
        }
        assert_eq!(active_tab, 0, "빈 벡터에서도 0 — usize::MAX 가 아니다");
    }

    #[test]
    fn cursor_covers_the_whole_glyph_it_sits_on() {
        use kasa_bridge::screen::Cell;
        let row: Vec<Cell> = "가A 나"
            .chars()
            .flat_map(|ch| {
                // 전각 뒤엔 그리드가 스페이서를 하나 둔다 — 실제 화면과 같은 모양으로.
                let mut c = Cell::blank();
                c.ch = ch;
                let wide = matches!(ch, '가' | '나');
                let mut out = vec![c];
                if wide {
                    let mut sp = Cell::blank();
                    sp.ch = '\0';
                    out.push(sp);
                }
                out
            })
            .collect();
        let cells = vec![row];
        assert_eq!(cursor_cell_width(&cells, 0, 0), 2, "한글은 두 칸");
        assert_eq!(cursor_cell_width(&cells, 0, 2), 1, "ASCII 는 한 칸");
        assert_eq!(cursor_cell_width(&cells, 0, 3), 1, "공백도 한 칸");
        assert_eq!(cursor_cell_width(&cells, 0, 4), 2, "한글은 두 칸");
        // 스페이서와 그리드 밖은 폭을 모른다 — 0 칸짜리 커서는 안 보이므로 1 로 눕힌다.
        assert_eq!(cursor_cell_width(&cells, 0, 1), 1, "스페이서");
        assert_eq!(cursor_cell_width(&cells, 0, 99), 1, "행 밖");
        assert_eq!(cursor_cell_width(&cells, 9, 0), 1, "화면 밖");
    }

    fn spans_of(src: &str) -> Vec<(String, Option<String>)> {
        let (blocks, _) = parse_markdown(src);
        blocks
            .iter()
            .filter_map(|b| match b {
                MdBlock::ListItem { spans, .. }
                | MdBlock::Para { spans }
                | MdBlock::Heading { spans, .. }
                | MdBlock::Callout { spans, .. }
                | MdBlock::Quote { spans } => Some(spans),
                _ => None,
            })
            .flatten()
            .map(|s| (s.text.clone(), s.link.clone()))
            .collect()
    }

    /// 파서 전체를 거쳐야 하는 테스트 — cmark 가 `[[` 를 낱개 이벤트로 흘리므로
    /// `push_wikilinked` 단독 테스트만으론 실제 문서에서 링크가 죽는 걸 못 잡는다
    /// (실제로 그렇게 놓쳐서 화면에 대괄호가 그대로 나왔다).
    #[test]
    fn wikilink_survives_the_whole_parser() {
        for src in [
            "- [[topic_a]] — 뒤에 설명\n",
            "[[topic_a]] 문단\n",
            "# [[topic_a]] 제목\n",
            "> [[topic_a]] 인용\n",
        ] {
            let got = spans_of(src);
            assert_eq!(
                got.first().map(|(t, l)| (t.as_str(), l.as_deref())),
                Some(("topic_a", Some("wiki:topic_a"))),
                "{src:?} 에서 링크가 죽었다: {got:?}"
            );
        }
    }

    /// 알림 종류별 블록 모양. `first`/`last` 가 상자를 그리는 기준이라, 이게
    /// 어긋나면 배경이 안 그려지거나 문단마다 상자가 끊긴다.
    #[test]
    fn callout_tags_become_callout_blocks() {
        for (src, want) in [
            ("> [!NOTE]\n> 알림\n", MdCallout::Note),
            ("> [!TIP]\n> 팁\n", MdCallout::Tip),
            ("> [!IMPORTANT]\n> 중요\n", MdCallout::Important),
            ("> [!WARNING]\n> 경고\n", MdCallout::Warning),
            ("> [!CAUTION]\n> 주의\n", MdCallout::Caution),
        ] {
            let (blocks, _) = parse_markdown(src);
            match blocks.as_slice() {
                [MdBlock::Callout {
                    kind,
                    first,
                    last,
                    spans,
                    ..
                }] => {
                    assert_eq!(*kind, want, "{src:?}");
                    assert!(
                        *first && *last,
                        "{src:?} 한 문단이면 상자를 열고 닫아야 한다"
                    );
                    // 태그 줄은 표지로 그려지므로 본문에 남으면 두 번 보인다.
                    let text: String = spans.iter().map(|s| s.text.as_str()).collect();
                    assert!(
                        !text.contains("[!"),
                        "{src:?} 본문에 태그가 남았다: {text:?}"
                    );
                }
                other => panic!("{src:?} → 콜아웃이 아니다: {} 블록", other.len()),
            }
        }
    }

    /// 알림이 아닌 것은 알림이 되지 말아야 한다 — 인용문은 인용문으로 남는다.
    #[test]
    fn plain_and_unknown_quotes_stay_quotes() {
        for src in ["> 그냥 인용문\n", "> [!HELLO]\n> 없는 종류\n"] {
            let (blocks, _) = parse_markdown(src);
            assert!(
                blocks.iter().all(|b| !matches!(b, MdBlock::Callout { .. })),
                "{src:?} 가 콜아웃으로 잡혔다"
            );
            assert!(
                blocks.iter().any(|b| matches!(b, MdBlock::Quote { .. })),
                "{src:?} 가 인용문으로도 안 남았다"
            );
        }
    }

    /// 여러 문단 알림은 첫 조각만 상자를 열고 마지막 조각만 닫는다 — 조각마다
    /// 상자를 그리면 이음새에서 배경이 겹쳐 그 띠만 색이 진해진다.
    #[test]
    fn multi_paragraph_callout_opens_once_and_closes_once() {
        let (blocks, _) = parse_markdown("> [!WARNING]\n> 첫 문단\n>\n> 둘째 문단\n");
        let flags: Vec<(bool, bool)> = blocks
            .iter()
            .filter_map(|b| match b {
                MdBlock::Callout { first, last, .. } => Some((*first, *last)),
                _ => None,
            })
            .collect();
        assert_eq!(
            flags,
            vec![(true, false), (false, true)],
            "조각 표시가 어긋났다"
        );
    }

    /// 알림 안 목록은 상자 안에 남아야 한다 — `ListItem` 으로 새면 경고에 딸린
    /// 목록이 경고 밖의 글로 읽힌다(실제로 상자 아래에 매달려 나왔다).
    #[test]
    fn list_inside_callout_stays_in_the_box() {
        let (blocks, _) = parse_markdown("> [!WARNING]\n> 확인해라.\n>\n> - 첫째\n> - 둘째\n");
        assert!(
            blocks
                .iter()
                .all(|b| !matches!(b, MdBlock::ListItem { .. })),
            "목록이 상자 밖으로 샜다"
        );
        let depths: Vec<Option<u8>> = blocks
            .iter()
            .filter_map(|b| match b {
                MdBlock::Callout { list, .. } => Some(list.as_ref().map(|(d, _)| *d)),
                _ => None,
            })
            .collect();
        assert_eq!(
            depths,
            vec![None, Some(0), Some(0)],
            "문단 하나 + 목록 둘이어야 한다"
        );
        assert!(
            matches!(blocks.last(), Some(MdBlock::Callout { last: true, .. })),
            "마지막 조각이 상자를 닫지 않았다"
        );
    }

    /// 알림 안에서도 문서 사이 링크는 살아 있어야 한다 — 경고문에 관련 문서를
    /// 달아 두는 게 이 볼트의 실제 사용법이다.
    #[test]
    fn wikilink_works_inside_callout() {
        let got = spans_of("> [!WARNING]\n> 자세히는 [[topic_a]] 참고\n");
        assert!(
            got.iter()
                .any(|(t, l)| t == "topic_a" && l.as_deref() == Some("wiki:topic_a")),
            "알림 안 링크가 죽었다: {got:?}"
        );
    }

    /// 인라인 코드 안의 표기는 링크가 아니다 — 문서에서 표기 자체를 설명할 때
    /// 쓰는 자리다(이 레포 주석·메모리 문서가 실제로 그렇게 쓴다).
    #[test]
    fn wikilink_inside_inline_code_stays_literal() {
        let got = spans_of("`[[topic_a]]` 는 표기다\n");
        assert!(
            got.iter().all(|(_, l)| l.is_none()),
            "코드 안 표기가 링크가 됐다: {got:?}"
        );
    }

    /// HTML 태그에 감싸인 본문은 살아야 한다. 옛 렌더는 `Event::Html` 을 통째로
    /// 버려서, 유효한 태그 이름을 만나면 그 안 문장이 화면에서 사라졌다 —
    /// GitHub 접기 절(`<details>`)의 제목과 `<div>` 안 문장이 실제로 그랬다.
    #[test]
    fn html_tag_bodies_survive() {
        let got = spans_of(
            "<details>\n<summary>접히는 제목</summary>\n</details>\n\n\
             <div align=\"center\">\n가운데 문장.\n</div>\n\n\
             <system-reminder>\n하이픈 태그 안.\n</system-reminder>\n",
        );
        let texts: Vec<&str> = got.iter().map(|(t, _)| t.as_str()).collect();
        for want in ["접히는 제목", "가운데 문장.", "하이픈 태그 안."] {
            assert!(
                texts.iter().any(|t| t.contains(want)),
                "{want} 이 사라졌다: {got:?}"
            );
        }
        // 태그 자체는 문서에 없던 글자다.
        assert!(
            texts.iter().all(|t| !t.contains('<') && !t.contains('>')),
            "태그가 글자로 그려졌다: {got:?}"
        );
    }

    /// 주석은 반대로 감춰져야 한다 — 안 보이게 하려고 쓴 표기라, 태그만 벗겨
    /// 내용을 드러내면 글쓴이의 뜻이 뒤집힌다.
    #[test]
    fn html_comments_stay_hidden() {
        let got = spans_of("앞 문단.\n\n<!-- 감춰 둔 메모 -->\n\n뒤 문단.\n");
        assert!(
            got.iter().all(|(t, _)| !t.contains("감춰 둔 메모")),
            "주석이 드러났다: {got:?}"
        );
    }

    /// 인라인 태그는 서체로 옮긴다. 태그를 지우기만 하면 강조가 사라지고,
    /// 글자로 그리면 문서에 없던 꺾쇠가 생긴다.
    #[test]
    fn inline_html_maps_to_styles() {
        let (blocks, _) = parse_markdown("평범 <b>굵게</b> 와 <em>기울게</em> 끝\n");
        let spans = match &blocks[0] {
            MdBlock::Para { spans } => spans,
            _ => panic!("문단이 아니다"),
        };
        let shape: Vec<(&str, bool, bool)> = spans
            .iter()
            .map(|s| (s.text.as_str(), s.bold, s.italic))
            .collect();
        assert!(
            shape.iter().any(|&(t, b, _)| t == "굵게" && b),
            "b 태그가 굵게로 안 옮겨졌다: {shape:?}"
        );
        assert!(
            shape.iter().any(|&(t, _, i)| t == "기울게" && i),
            "em 태그가 기울게로 안 옮겨졌다: {shape:?}"
        );
        assert!(
            shape.iter().all(|&(t, ..)| !t.contains('<')),
            "태그가 글자로 남았다: {shape:?}"
        );
    }

    /// `<br>` 는 스팬에 줄바꿈 표기가 없어 띄어쓰기로 떨어진다 — 그냥 지우면
    /// 앞뒤 낱말이 한 덩어리로 붙는다(표 셀에서 자주 쓰는 표기다).
    #[test]
    fn inline_br_becomes_space() {
        let got = spans_of("여러<br>줄\n");
        let joined: String = got.iter().map(|(t, _)| t.as_str()).collect();
        assert_eq!(joined, "여러 줄");
    }

    /// `[[이름]]` 은 대괄호를 벗긴 링크 스팬이 되고 앞뒤 글은 평문으로 남아야 한다 —
    /// 메모리 인덱스가 통째로 이 표기라, 여기서 어긋나면 문서 사이 이동이 죽는다.
    #[test]
    fn wikilink_becomes_link_span() {
        let mut spans = Vec::new();
        push_wikilinked(&mut spans, "앞 [[topic_a]] 뒤", false, false, false);
        let shape: Vec<(&str, Option<&str>)> = spans
            .iter()
            .map(|s| (s.text.as_str(), s.link.as_deref()))
            .collect();
        assert_eq!(
            shape,
            vec![
                ("앞 ", None),
                ("topic_a", Some("wiki:topic_a")),
                (" 뒤", None)
            ]
        );
    }

    /// 닫히지 않은 `[[` 나 대괄호가 섞인 이름은 링크가 아니다. 링크로 만들면 문서에
    /// 없는 파일을 가리키는 죽은 링크가 생긴다.
    #[test]
    fn malformed_wikilink_stays_plain() {
        for src in ["[[열린 채", "[[a[b]]", "[[]]"] {
            let mut spans = Vec::new();
            push_wikilinked(&mut spans, src, false, false, false);
            assert!(
                spans.iter().all(|s| s.link.is_none()),
                "{src} 가 링크로 잡혔다"
            );
            let joined: String = spans.iter().map(|s| s.text.as_str()).collect();
            assert_eq!(joined, src, "{src} 의 글자가 유실됐다");
        }
    }

    /// 한 줄에 여러 개, 그리고 붙어 있는 경우까지. 메모리 인덱스는 한 줄에 두세 개를
    /// 예사로 쓴다.
    #[test]
    fn multiple_wikilinks_in_one_run() {
        let mut spans = Vec::new();
        push_wikilinked(&mut spans, "[[a]]·[[b]]", false, false, false);
        let links: Vec<&str> = spans.iter().filter_map(|s| s.link.as_deref()).collect();
        assert_eq!(links, vec!["wiki:a", "wiki:b"]);
        let joined: String = spans.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(joined, "a·b");
    }

    /// Tables only reach the renderer when `ENABLE_TABLES` is on — without it
    /// pulldown emits the rows as plain text that the block builder drops on the
    /// floor, which is exactly how CLAUDE.md's 3-layer table went missing.
    #[test]
    fn table_parses_into_head_rows_and_alignment() {
        let md = "| 층 | 코드네임 |\n|:---|---:|\n| ① 엔진 | **kasaterm** |\n| ② 작업환경 | `kasaspace` |\n";
        let (blocks, _) = parse_markdown(md);
        let table = blocks
            .iter()
            .find_map(|b| match b {
                MdBlock::Table { head, rows, align } => Some((head, rows, align)),
                _ => None,
            })
            .expect("표가 블록으로 나오지 않았다");
        let (head, rows, align) = table;
        assert_eq!(head.len(), 2);
        assert_eq!(head[0][0].text, "층");
        assert_eq!(align, &vec![MdAlign::Left, MdAlign::Right]);
        // A trailing empty row would render as a phantom band under the table.
        assert_eq!(rows.len(), 2, "본문 행 개수");
        assert!(
            rows.iter().all(|r| r.len() == 2),
            "빈 행이 섞였다: {rows:?}"
        );
        assert!(rows[0][1][0].bold, "셀 안 인라인 스타일 보존");
        assert!(rows[1][1][0].code, "셀 안 인라인 코드 보존");
    }

    /// 중첩 리스트의 부모 항목은 자식 리스트가 열릴 때 나가야 한다. TagEnd::Item
    /// 까지 미루면 ① 자식이 먼저 블록에 들어가 화면에서 부모 위로 올라오고
    /// ② 자식의 Tag::Item 이 공유 spans 를 비워 부모 텍스트가 사라졌다.
    #[test]
    fn nested_list_keeps_parent_text_and_order() {
        let (blocks, _) = parse_markdown("- outer\n  - inner\n");
        let items: Vec<(u8, String)> = blocks
            .iter()
            .filter_map(|b| match b {
                MdBlock::ListItem { depth, spans, .. } => {
                    Some((*depth, spans.iter().map(|s| s.text.as_str()).collect()))
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            items,
            vec![(0, "outer".to_string()), (1, "inner".to_string())],
            "부모가 먼저, 텍스트를 지닌 채로 나와야 한다"
        );
    }

    /// `block_lines` 는 블록과 개수가 같고 **오름차순**이어야 한다 — Raw↔Render
    /// 토글이 이진 탐색(`partition_point`)으로 줄↔블록을 짝지으므로, 순서가
    /// 뒤집히면 엉뚱한 위치로 점프한다.
    #[test]
    fn block_lines_align_with_blocks_and_ascend() {
        let md = "# 제목\n\n문단 하나.\n\n- outer\n  - inner\n- 둘째\n\n\
                  > 인용\n\n```rust\nfn main() {}\n```\n\n\
                  | a | b |\n|---|---|\n| 1 | 2 |\n\n---\n\n마지막 문단.\n";
        let (blocks, lines) = parse_markdown(md);
        assert_eq!(blocks.len(), lines.len(), "두 벡터의 길이가 어긋났다");
        assert!(
            lines.windows(2).all(|w| w[0] <= w[1]),
            "줄 번호가 역행한다: {lines:?}"
        );
        assert_eq!(lines[0], 0, "첫 블록은 0줄");
        assert_eq!(*lines.last().unwrap(), 20, "마지막 문단의 줄");
    }

    #[test]
    fn count_claude_panes_walks_nested_splits_and_windows() {
        // Mirrors save_session_state's schema: nested split leaves + a second
        // window, mixed agents. Only agent leaves count; a null leaf
        // (unresolved pane at save) and a plain shell are ignored.
        //
        // 이 테스트는 **옛 키(`was_claude`)로만** 짜여 있다 — 그대로 두는 것이
        // 하위호환의 단언이다. 새 키는 아래 `was_agent_*` 테스트가 본다.
        let leaf = |claude: bool| {
            serde_json::json!({ "leaf": {
                "cwd": "/repo",
                "was_claude": claude,
                "session_id": if claude { Some("abcd-1234") } else { None },
                "scrollback": ["$ claude", "hello"],
            }})
        };
        let state = serde_json::json!({
            "active_session": 0,
            "sessions": [{
                "active_window": 0,
                "windows": [
                    // Window 0: split( claude , split( shell , claude ) ) = 2 claude
                    { "split": {
                        "dir": "h", "ratio": 0.5,
                        "a": leaf(true),
                        "b": { "split": {
                            "dir": "v", "ratio": 0.4,
                            "a": leaf(false),
                            "b": leaf(true),
                        }},
                    }},
                    // Window 1: a lone claude leaf + a null leaf (dropped)
                    { "split": {
                        "dir": "h", "ratio": 0.5,
                        "a": leaf(true),
                        "b": { "leaf": serde_json::Value::Null },
                    }},
                ],
            }],
        });
        assert_eq!(App::count_claude_panes(&state), 3);
        // Degenerate inputs never panic and count zero.
        assert_eq!(App::count_claude_panes(&serde_json::json!({})), 0);
        assert_eq!(
            App::count_claude_panes(&serde_json::json!({ "sessions": [] })),
            0
        );
        // 캐릭터는 claude 증거가 아니다 — assign_character_env 가 spawn 때 모든
        // pane 에 배정하므로, 이걸 세면 순수 셸 3개짜리 창이 "claude 세션 3개"로
        // 표시된다(실제 발생). 이 단언이 그 회귀를 막는다.
        let char_only = serde_json::json!({ "sessions": [{ "windows": [{
            "leaf": { "cwd": "/repo", "was_claude": false, "session_id": null, "character": "아루" }
        }]}]});
        assert_eq!(
            App::count_claude_panes(&char_only),
            0,
            "캐릭터만으론 claude 아님"
        );
        // was_claude 감지 실패(저장 순간 claude 가 포그라운드가 아님) 보정은
        // session_id 로 한다 — claude 가 실제로 세션을 바인딩했을 때만 붙는다.
        let sid_only = serde_json::json!({ "sessions": [{ "windows": [{
            "leaf": { "cwd": "/repo", "was_claude": false, "session_id": "abcd-1234" }
        }]}]});
        assert_eq!(
            App::count_claude_panes(&sid_only),
            1,
            "바인딩된 세션은 claude"
        );
        // 프롬프트를 띄울 기준은 전체 pane 수 — claude 가 0이어도 레이아웃과
        // 스크롤백은 되살릴 값이 있다.
        assert_eq!(App::count_panes(&state), 5, "null leaf 포함 전체 leaf");
        assert_eq!(App::count_panes(&char_only), 1);
        assert_eq!(App::count_panes(&serde_json::json!({})), 0);
    }

    /// 새 키 `was_agent` 는 종류를 담고, codex pane 도 복원 대상으로 센다.
    /// 옛 `was_claude` 는 계속 claude 로 읽혀야 한다 — 판올림 한 번에 사용자가 쓰던
    /// 학생 pane 이 전부 셸로 되살아나는 것을 막는 단언.
    #[test]
    fn was_agent_counts_codex_and_keeps_legacy_was_claude() {
        let win = |leaf: serde_json::Value| serde_json::json!({ "sessions": [{ "windows": [{ "leaf": leaf }]}]});
        let n = |leaf: serde_json::Value| App::count_claude_panes(&win(leaf));
        assert_eq!(
            n(serde_json::json!({ "cwd": "/repo", "was_agent": "codex", "session_id": null })),
            1,
            "codex pane 도 복원 대상"
        );
        assert_eq!(
            n(serde_json::json!({ "cwd": "/repo", "was_agent": "claude", "session_id": null })),
            1
        );
        assert_eq!(
            n(serde_json::json!({ "cwd": "/repo", "was_agent": null, "session_id": null })),
            0,
            "순수 셸"
        );
        assert_eq!(
            n(serde_json::json!({ "cwd": "/repo", "was_claude": true, "session_id": null })),
            1,
            "옛 저장본은 claude 로 읽힌다"
        );
        assert_eq!(
            n(serde_json::json!({ "cwd": "/repo", "was_claude": false, "session_id": null })),
            0
        );
    }

    /// 복원 명령 분기. codex pane 이 셸로 되살아나던 것이 이 작업의 출발점이라,
    /// "codex 였으면 codex 로" 를 못박는다.
    #[test]
    fn restore_command_picks_the_harness_it_was() {
        // 여기서는 **하네스 갈래만** 본다 — 모델·effort 조합은 session.rs 쪽에 건다.
        let cmd = |a, s, r| crate::session::restore_agent_command(a, s, r, None, None);
        assert_eq!(cmd(Some("codex"), None, false), "codex -c check_for_update_on_startup=false\r");
        assert_eq!(
            cmd(
                Some("codex"),
                Some("01900000-0000-7000-8000-000000000003"),
                true
            ),
            "codex resume 01900000-0000-7000-8000-000000000003 -c check_for_update_on_startup=false\r"
        );
        // rollout 이 사라졌으면 새로 — `resume --last` 로 흘리지 않는다(미러된
        // ~/.codex/sessions 전체에서 골라 남의 대화를 물어온다).
        assert_eq!(cmd(Some("codex"), Some("019fd187-ba6e"), false), "codex -c check_for_update_on_startup=false\r");
        assert_eq!(
            cmd(Some("claude"), Some("abcd-1234"), true),
            "claude --resume abcd-1234\r"
        );
        // jsonl 이 사라진 세션은 fresh 로 — "No conversation found" 로 pane 이 통째 죽는다.
        assert_eq!(cmd(Some("claude"), Some("abcd-1234"), false), "claude\r");
        assert_eq!(cmd(Some("claude"), None, false), "claude\r");
    }

    #[test]
    fn strip_activity_prefix_removes_claude_glyphs() {
        // claude OSC 제목 "✳ 요약" → "요약"; 별표류·∗·＊·* 접두 + 공백 제거.
        assert_eq!(strip_activity_prefix("✳ 학생 프사 개선"), "학생 프사 개선");
        assert_eq!(strip_activity_prefix("✻  Brewed for 5s"), "Brewed for 5s");
        assert_eq!(strip_activity_prefix("* build"), "build");
        assert_eq!(strip_activity_prefix("＊작업"), "작업");
        // 브라유 스피너(U+2800 블록) 접두 — 연속 run 도 한 번에.
        assert_eq!(
            strip_activity_prefix("⠂ 세션 요약 디버깅"),
            "세션 요약 디버깅"
        );
        assert_eq!(strip_activity_prefix("⠐⠑ 이름"), "이름");
        // reduce motion(/config)은 스피너가 ● 하나로 고정된다 — 2026-09-02 실측.
        assert_eq!(
            strip_activity_prefix("● 스피너 판독 수리"),
            "스피너 판독 수리"
        );
        // 별표로 시작 안 하면 원문 그대로(rename 사용자 값 보호).
        assert_eq!(strip_activity_prefix("학생 프사 개선"), "학생 프사 개선");
        assert_eq!(strip_activity_prefix("main.rs · vim"), "main.rs · vim");
    }

    #[test]
    fn cleanup_collab_markers_removes_pane_files() {
        // %987 keeps the test clear of any real pane's markers in /tmp.
        // 픽스처는 코드와 **같은 헬퍼**로 놓는다 — 리터럴 "/tmp" 를 쓰면 Windows 에서
        // 현재 드라이브의 `C:\tmp` 로 가고 코드는 `%TEMP%` 를 봐서 영영 안 만난다.
        let bound = kasa_socket::bound_marker_path("_987");
        let room = kasa_socket::collab_root().join("test-marker-cleanup");
        std::fs::create_dir_all(&room).unwrap();
        let character = room.join("character-987");
        let nudged = room.join("god-nudged-%987");
        let other = room.join("character-988");
        for f in [&bound, &character, &nudged, &other] {
            std::fs::write(f, "x").unwrap();
        }
        // cwd=None → 폴백(전체 방 순회). 같은 번호 마커는 지우고 다른 번호는 보존.
        App::cleanup_collab_markers("%987", None);
        assert!(!bound.exists(), "bound marker should be deleted");
        assert!(!character.exists(), "character marker should be deleted");
        assert!(!nudged.exists(), "god-nudged marker should be deleted");
        assert!(other.exists(), "another pane's marker must survive");
        std::fs::remove_dir_all(&room).unwrap();
    }

    #[test]
    fn cleanup_collab_markers_spares_other_rooms() {
        // 같은 pane 번호라도 *다른 방*의 마커는 살아남아야 한다(사용자: 캐릭터 유실 근본).
        let mine = kasa_socket::collab_root().join("-tmp-room-mine");
        let other = kasa_socket::collab_root().join("-tmp-room-other");
        std::fs::create_dir_all(&mine).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        let my_char = mine.join("character-1");
        let other_char = other.join("character-1");
        std::fs::write(&my_char, "미도리").unwrap();
        std::fs::write(&other_char, "아리스").unwrap();
        App::cleanup_collab_markers("%1", Some(std::path::Path::new("/tmp/room/mine")));
        assert!(!my_char.exists(), "내 방의 닫힌 pane 마커는 삭제");
        assert!(other_char.exists(), "다른 방의 같은 번호 마커는 보존");
        std::fs::remove_dir_all(&mine).unwrap();
        std::fs::remove_dir_all(&other).unwrap();
    }

    #[test]
    fn wheel_sub_cell_ticks_accumulate() {
        let mut accum = 0.0;
        let mut last = Instant::now();
        let t0 = ms(last, 100);
        assert_eq!(wheel_step(&mut accum, 0.3, &mut last, ms(t0, 0)), None);
        assert_eq!(wheel_step(&mut accum, 0.3, &mut last, ms(t0, 20)), None);
        assert_eq!(wheel_step(&mut accum, 0.3, &mut last, ms(t0, 40)), None);
        assert_eq!(wheel_step(&mut accum, 0.3, &mut last, ms(t0, 60)), Some(1));
    }

    #[test]
    fn wheel_direction_flip_drops_residual() {
        let mut accum = 0.0;
        let mut last = Instant::now();
        let t0 = ms(last, 100);
        wheel_step(&mut accum, 0.6, &mut last, ms(t0, 0));
        let out = wheel_step(&mut accum, -1.0, &mut last, ms(t0, 50));
        assert_eq!(out, Some(-1));
    }

    #[test]
    fn selection_extract_single_row() {
        let mut row = vec![GridCell::blank(); 10];
        for (i, c) in "hello".chars().enumerate() {
            row[i] = GridCell {
                ch: c,
                ..GridCell::blank()
            };
        }
        let sel = Selection {
            anchor: (0, 0),
            end: (4, 0),
        };
        let s = extract_selection(&[row], sel);
        assert_eq!(s, "hello");
    }

    #[test]
    fn selection_normalise_reverses_when_needed() {
        let sel = Selection {
            anchor: (5, 2),
            end: (1, 0),
        };
        let (a, b) = normalise(sel);
        assert_eq!(a, (1, 0));
        assert_eq!(b, (5, 2));
    }

    /// Build a grid row from a string, expanding each full-width (CJK)
    /// char into a glyph cell + a blank spacer cell — exactly how the PTY
    /// backend stores them. Pads to `width` with blanks.
    fn wide_row(s: &str, width: usize) -> Vec<GridCell> {
        let mut row = Vec::new();
        for ch in s.chars() {
            row.push(GridCell {
                ch,
                ..GridCell::blank()
            });
            if gpu::is_wide_char(ch) {
                row.push(GridCell {
                    ch: ' ',
                    ..GridCell::blank()
                });
            }
        }
        while row.len() < width {
            row.push(GridCell::blank());
        }
        row
    }

    #[test]
    fn copy_drops_wide_char_spacer() {
        // Selection copy: "한글" must not become "한 글".
        let row = wide_row("한글", 12);
        let sel = Selection {
            anchor: (0, 0),
            end: (3, 0),
        };
        assert_eq!(extract_selection(&[row], sel), "한글");

        // Mixed ASCII + CJK keeps real spaces, drops only spacers.
        let row = wide_row("a한 b", 12);
        let sel = Selection {
            anchor: (0, 0),
            end: (5, 0),
        };
        assert_eq!(extract_selection(&[row], sel), "a한 b");
    }

    #[test]
    fn selection_soft_wrap_joins_but_hard_lines_stay_separate() {
        let mut rows = vec![wide_row("abcd", 4), wide_row("efgh", 4), wide_row("NEXT", 4)];
        rows[0][3].wrapped = true;
        let selection = Selection { anchor: (0, 0), end: (3, 2) };
        assert_eq!(extract_selection(&rows, selection), "abcdefgh\nNEXT");
        assert_eq!(extract_selection(&rows, Selection { anchor: selection.end, end: selection.anchor }),
            "abcdefgh\nNEXT");
        assert_eq!(extract_selection(&rows, Selection { anchor: (2, 0), end: (1, 1) }), "cdef");
        assert_eq!(extract_selection(&rows, Selection { anchor: (1, 0), end: (2, 0) }), "bc");
    }

    #[test]
    fn selection_soft_wrap_preserves_real_boundary_spaces_and_blank_hard_lines() {
        let mut rows = vec![wide_row("ab  ", 4), wide_row(" cd ", 4), wide_row("", 4), wide_row("end", 4)];
        rows[0][3].wrapped = true;
        assert_eq!(extract_selection(&rows, Selection { anchor: (0, 0), end: (3, 3) }), "ab   cd\n\nend");
        assert_eq!(extract_selection(&rows, Selection { anchor: (0, 0), end: (3, 0) }), "ab");
    }

    #[test]
    fn selection_soft_wrap_wide_glyphs_keep_column_boundaries() {
        let mut rows = vec![wide_row("한글", 4), wide_row("가 나", 5)];
        rows[0][3].wrapped = true;
        assert_eq!(extract_selection(&rows, Selection { anchor: (0, 0), end: (4, 1) }), "한글가 나");
        assert_eq!(extract_selection(&rows, Selection { anchor: (1, 0), end: (0, 1) }), "글가");
        assert_eq!(extract_selection(&rows, Selection { anchor: (1, 0), end: (1, 0) }), "");
    }

    #[test]
    fn selection_soft_wrap_omits_leading_wide_padding_but_keeps_real_spaces() {
        let mut rows = vec![wide_row("ab  ", 4), wide_row("한글", 4)];
        rows[0][3].wrapped = true;
        rows[0][3].leading_wide_spacer = true;
        assert_eq!(extract_selection(&rows, Selection { anchor: (0, 0), end: (3, 1) }), "ab 한글");
        assert_eq!(extract_selection(&rows, Selection { anchor: (3, 0), end: (1, 1) }), "한");
    }

    /// 딥링크의 칸 이름은 Rust 가 만들고 TS 가 알아본다 — 한쪽만 고치면 예외 없이
    /// 기본 칸으로 떨어지고, 그게 「계정 관리가 캐릭터 화면으로 간다」의 모양이었다.
    /// 그래서 웹의 목록을 소스에서 직접 읽어 대조한다.
    #[test]
    fn every_settings_cat_web_key_exists_in_the_web_nav() {
        let src = include_str!("../../../web/arona-ui/src/settings/SettingsApp.tsx");
        let cats = src
            .split_once("const CATS = [")
            .expect("CATS 목록을 못 찾았다 — 웹이 이름을 바꿨으면 여기도 같이 봐라")
            .1
            .split_once(']')
            .unwrap()
            .0;
        let keys: Vec<&str> = cats
            .split("key: '")
            .skip(1)
            .filter_map(|s| s.split_once('\''))
            .map(|(k, _)| k)
            .collect();
        assert_eq!(
            keys.len(),
            SettingsCat::ALL.len(),
            "칸 개수가 어긋난다: {keys:?}"
        );
        for c in SettingsCat::ALL {
            assert!(
                keys.contains(&c.web_key()),
                "웹에 없는 칸 이름: {}",
                c.web_key()
            );
        }
    }
}
