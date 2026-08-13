//! KASATERM_RENDERER=gpu path. Owns its own wgpu Surface + cell
//! pipeline, parallel to (and mutually exclusive with) the existing
//! sugarloaf path. Phase 2a renders the cell grid only; chrome
//! (sidebar, tabs, headers) is intentionally absent until the
//! rect/text facade lands in Phase 2b+.
//!
//! Two surfaces on one window are not portable — we only init this
//! module when `KASATERM_RENDERER=gpu` is set, and in that case we
//! skip sugarloaf init entirely so the swapchain has a single
//! owner.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Context, Result};
use kasa_cells::pipeline::CellInstance;
use kasa_cells::{Atlas, AtlasEntry, GlyphKey, Pipeline, Shaper};
use raw_window_handle::{HasDisplayHandle, HasWindowHandle};
use kasa_bridge::screen::Cell;
use winit::window::Window;

const ATLAS_SIZE: u32 = 2048;

/// Bundled pixel face for chrome labels under a pixel Shape (SIL OFL 1.1 — see
/// assets/fonts/OFL-Galmuri.txt). Shipped verbatim, not subset: a subset is a
/// Modified Version under that license, and the few MB saved aren't worth it.
const GALMURI_11: &[u8] = include_bytes!("../assets/fonts/Galmuri11.ttf");
/// Device px per Galmuri dot — every cut draws one dot per `upem/100` units, so
/// Galmuri11 (upem 1200) is crisp only at whole multiples of 12.
const GALMURI_DOT_PX: u32 = 12;
/// Side of the pixel icons' design grid. Their paths sit on whole units, so any
/// raster size off this multiple lands dot edges on fractions and softens them.
const ICON_GRID_PX: u32 = 24;

/// Glyph supersampling factor for a render scale. Below Retina the logical
/// pixel size is too small to resolve a coverage mask cleanly, so bake at 2x
/// and let the Linear sampler downsample; Retina already has the pixels.
/// Must be re-evaluated on every DPI change, not just at startup — see
/// `Renderer::set_scale`.
fn oversample_for(scale: f32) -> u32 {
    if scale < 2.0 { 2 } else { 1 }
}

/// 칸 폭을 주 폰트의 자연 advance 보다 좁히는 비율. `KASATERM_CELL_TIGHTEN`.
///
/// 자간은 "칸 폭 − 글자 폭"이라, 글자를 안 건드리고 좁히려면 여기밖에 없다.
/// 한글은 두 칸을 쓰므로 칸을 1px 줄이면 한글 사이는 2px, 라틴은 1px 줄어
/// **한글이 두 배 속도로** 좁아진다 — 라틴 자간이 지나치게 붙기 전에 한글이
/// 먼저 제자리를 찾는다는 뜻이다. 칸이 글자보다 좁아지면 글자가 옆 칸을
/// 침범하므로 0.85 아래로는 못 내려간다.
fn cell_tighten() -> f32 {
    use std::sync::OnceLock;
    static T: OnceLock<f32> = OnceLock::new();
    *T.get_or_init(|| {
        std::env::var("KASATERM_CELL_TIGHTEN")
            .ok()
            .and_then(|s| s.parse::<f32>().ok())
            .unwrap_or(DEFAULT_CELL_TIGHTEN)
            .clamp(0.85, 1.0)
    })
}

/// 0.87 = JetBrains Mono 의 자연 칸(0.6em)에서 12% 조임. 눈으로 골랐다 —
/// 16px 칸에선 한글이 글자 단위로 흩어져 읽히고, 14px 에서 비로소 단어로
/// 뭉친다(한글 사이 8px→4px). 라틴은 4px→2px 인데 JetBrains 가 넓은 얼굴이라
/// 여전히 답답하지 않다. 글리프 잉크는 24px 로 셋 다 같다 — 칸만 좁아진다.
const DEFAULT_CELL_TIGHTEN: f32 = 0.87;

/// 칸 폭 계산의 **단 하나의 자리**. 부팅과 폰트 크기 변경이 서로 다른 식을
/// 쓰면 크기를 바꾸는 순간 조임이 풀린다(실제로 그랬다).
fn cell_w_for(shaper: &mut Shaper, size_px: f32) -> f32 {
    (shaper.cell_advance(size_px) * cell_tighten()).ceil().max(1.0)
}

/// A decoded image uploaded to its own wgpu texture. Kept alive (texture +
/// view) for as long as the pane shows it, since the bind group borrows the
/// view. Keyed by pane id in `GpuRenderer::images`.
struct ImageEntry {
    _texture: wgpu::Texture,
    _view: wgpu::TextureView,
    bind_group: wgpu::BindGroup,
    w: u32,
    h: u32,
}

pub struct GpuRenderer {
    _window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    /// When set, the next `render` reads the presented frame back into a PNG at
    /// this path — permission-free self-capture for headless verification.
    pub capture_next: Option<String>,
    /// 물리픽셀 크롭 `(x, y, w, h)`. `capture_next` 와 함께 소비된다. None = 창 전체.
    /// `surface.capture` 가 pane 한 칸만 잘라 내는 데 쓴다.
    pub capture_crop: Option<(u32, u32, u32, u32)>,
    /// 저장 전 가로 상한(0 = 원본). 넘을 때만 비율을 지켜 줄인다 — 캡처를 읽는
    /// 쪽은 보통 에이전트라, 원본 해상도 그대로면 컨텍스트를 크게 태운다.
    pub capture_max_w: u32,
    pipeline: Pipeline,
    atlas: Atlas,
    shaper: Shaper,
    /// Bundled pixel face for chrome labels under a pixel Shape (font=3 in the
    /// shared atlas). Loaded on first use, not at startup: most sessions never
    /// select that shape, and the face is 5 MB of Hangul.
    chrome_shaper: Option<Shaper>,
    /// Set once loading fails so a broken face doesn't retry every frame.
    chrome_shaper_failed: bool,
    /// Secondary shaper for markdown body/heading text — a proportional gothic
    /// (Noto Sans KR if installed, else Apple SD Gothic Neo) so documents read
    /// like prose, not code. Glyphs go into the SAME atlas keyed by font=1.
    md_shaper: Shaper,
    /// Bold weight of the markdown gothic (font=2). A real heavy face reads far
    /// cleaner than smearing the regular glyph, so headings / **bold** use this.
    md_bold_shaper: Shaper,
    bind_group: wgpu::BindGroup,
    font_size_px: u32,
    pub cell_w: f32,
    pub cell_h: f32,
    /// Per-frame chrome instances. main.rs's chrome code pushes via
    /// `rect()` / `draw_text()` between frames; `render()` drains.
    chrome: Vec<CellInstance>,
    /// 이번 프레임에 그린 문자열 원문 — 하네스 전용, `KASATERM_TEXT_LOG` 가 있을
    /// 때만 켜진다(없으면 `Option` 검사 하나라 프로덕션엔 비용이 없다).
    ///
    /// `chrome` 에는 글리프 인스턴스만 남아 문자열을 되살릴 수 없다. 그래서
    /// "헤더에 학생 이름이 떴나" 같은 걸 캡처를 눈으로 보는 것 말고는 물을 수가
    /// 없었다 — 안 보면 조용히 통과한다.
    text_log: Option<Vec<String>>,
    /// Scale we cached on init. winit logical→physical conversion.
    scale: f32,
    /// True when KASATERM_P3_ROOT installed our own root metal layer and
    /// wgpu was given that layer via `SurfaceTargetUnsafe::CoreAnimationLayer`.
    /// In this mode the legacy per-frame P3 re-apply / re-promote calls must
    /// be skipped — they target wgpu's would-be sublayer, which doesn't
    /// exist on this path, and on macOS 26 they actively undo our root install.
    p3_root_owned: bool,
    /// Separate pipeline for image panes — built with linear filtering so a
    /// photo scaled to a pane reads smooth, not pixelated. Has its own
    /// instance buffer so the image quads don't collide with the chrome
    /// pass's buffer in the same render pass.
    image_pipeline: Pipeline,
    /// Linear, clamp-to-edge sampler shared by every image bind group.
    image_sampler: wgpu::Sampler,
    /// Uploaded image textures keyed by pane id. Populated lazily on the
    /// first frame a given image pane is drawn.
    images: HashMap<String, ImageEntry>,
    /// Per-frame image quads: (pane id, instance, chrome watermark). Drained
    /// in `render()` where each is drawn with that pane's texture bind group.
    ///
    /// The watermark is `chrome.len()` at queue time — how much chrome was
    /// already queued when this quad was asked for. `render` uses it to slice
    /// the chrome pass and drop the quad back into its place, so **the order
    /// you queue things in is the order they come out**. Without it images and
    /// icons live in their own passes, permanently below/above all chrome, and
    /// no panel or modal can cover them however late it is drawn.
    image_quads: Vec<(String, CellInstance, u32, Option<[u32; 4]>)>,
    /// Per-frame chrome icon quads: (texture key, instance, chrome watermark).
    /// Same texture path and same ordering rule as `image_quads`.
    icon_quads: Vec<(String, CellInstance, u32, Option<[u32; 4]>)>,
    /// 지금 유효한 클립 사각형들 — LOGICAL px `[x0, y0, x1, y1]`, 이미 교집합이
    /// 접혀 있어 `last()` 하나가 곧 현재 클립이다. 프레임마다 `clear_chrome` 이
    /// 비운다.
    ///
    /// 클로저(`with_clip(|g| …)`)가 아니라 스택인 건 호출부 사정이다. 칼럼 그리기는
    /// `&mut GpuRenderer` 를 이미 손에 쥔 자유 함수들이고 루프 안에서 `continue`
    /// 로 빠져나간다 — 클로저로 감싸면 그 `continue` 가 안 넘어가고, 12,000줄짜리
    /// 파일 전체에 들여쓰기가 한 단 더 붙는다.
    clip_stack: Vec<[f32; 4]>,
    /// 클립이 바뀐 지점들 — `(그 시점의 chrome.len(), 그 뒤로 유효한 클립)`.
    /// PHYSICAL px `[x, y, w, h]`(`set_scissor_rect` 가 받는 그대로), `None` 은
    /// 클립 없음. `render` 가 이걸로 chrome 패스를 세그먼트로 잘라 그린다.
    clip_runs: Vec<(u32, Option<[u32; 4]>)>,
    /// 이번 프레임에 `hover_rect` 가 한 번이라도 그려졌나 — 즉 커서 밑에
    /// 누를 수 있는 표면이 있나. 커서 모양(손가락)을 이 하나로 정하려고 둔다.
    ///
    /// 히트렉트 목록을 따로 순회하지 않는 건, 그렇게 하면 "배경은 들리는데
    /// 커서는 그대로"인 표면이 계속 새로 생기기 때문이다. 들림을 그리는
    /// 함수가 곧 이 플래그를 세우니 둘이 갈릴 수가 없다.
    pub hover_pointer: bool,
    /// Logical-px rects of link spans drawn in the most recent markdown
    /// frame: (x, y, w, h, dest). main.rs hit-tests a click against these to
    /// open a file (Finder) or URL (browser). Cleared at the start of every
    /// `draw_markdown` so it always reflects the current scroll position.
    pub md_link_rects: Vec<(f32, f32, f32, f32, String)>,
    /// Logical-px rects of markdown code-block copy buttons: (x, y, w, h,
    /// code). main.rs hit-tests a click and copies `code`. Rebuilt each
    /// `draw_markdown` like `md_link_rects`.
    pub md_copy_rects: Vec<(f32, f32, f32, f32, String)>,
    /// Logical-px rects of every word drawn in the most recent markdown frame:
    /// (x, y, w, h, text). Drives text selection in the rendered view — the
    /// document has no cell grid, so a drag range has to be resolved against
    /// what was actually laid out. Rebuilt each `draw_markdown`.
    pub md_word_rects: Vec<(f32, f32, f32, f32, String)>,
    /// Active selection for the rendered view, in **screen** logical px:
    /// (ax, ay, bx, by), unordered. Set by `draw_markdown` from the caller's
    /// document-space anchor; `md_runs` paints the band behind each word that
    /// falls inside. Read here rather than passed down because every block kind
    /// calls `md_runs` and none of them cares about selection.
    pub md_sel_screen: Option<(f32, f32, f32, f32)>,
    /// Document-space y (logical px from the top of the doc, scroll excluded)
    /// where each block starts, index-aligned with the blocks just drawn. The
    /// Raw↔Render toggle pairs this with `MarkdownDoc::block_lines` to convert
    /// a scroll offset in one mode to the other. Filled by `draw_markdown`;
    /// render.rs moves it out per pane id.
    pub md_block_ys: Vec<f32>,
    /// Tree-sitter span cache for raw-editor buffers, content-addressed by a
    /// hash of (lang, lines) — no pane id needed, and a tiny LRU keeps a few
    /// split editors from thrashing each other's entries.
    raw_hl: Vec<RawHlEntry>,
    /// 재파싱 대기 — (버퍼 해시, 그 해시를 처음 본 시각). 타이핑이 이어지면
    /// 키마다 해시가 바뀌어 이 값이 계속 새로 서므로 파싱이 미뤄진다.
    raw_hl_pending: Option<(u64, std::time::Instant)>,
    /// 직전 전체 파싱이 실제로 걸린 시간. 다음 재파싱을 얼마나 미룰지를 이
    /// 값에서 뽑는다(`RAW_HL_COST_MULT`) — 파일 크기를 세지 않고도 무거운
    /// 문서에서 저절로 더 참는다.
    raw_hl_cost: std::time::Duration,
    /// 이 언어로 한 번이라도 파싱했는지. **첫 파싱은 비용 기준에서 뺀다** —
    /// 그때는 tree-sitter config·쿼리 빌드가 함께 잡혀 9줄 파일에서도 14ms 가
    /// 나오고, 그걸 기준으로 임계를 세우면 작은 파일까지 210ms 를 기다렸다
    /// (실측).
    raw_hl_parsed_once: bool,
    /// Laid-out height of each markdown block, so a block scrolled off screen
    /// can be stepped over instead of re-measured. Same tiny-LRU shape as
    /// `raw_hl` (keyed by doc + layout, so two markdown panes don't evict each
    /// other every frame).
    md_heights: Vec<MdHeightEntry>,
}

/// One document's block heights under one layout. `key` is (doc generation,
/// column width, base font size, dpi scale) — every input the layout depends
/// on, so a stale entry can't be served after a resize or a reparse.
struct MdHeightEntry {
    key: (u64, u32, u32, u32),
    h: Vec<f32>,
}

/// One cached tree-sitter highlight: the buffer hash it was computed from and
/// the per-line (token, kind) runs, shared with the draw loop via Rc so the
/// cache lookup doesn't fight the `&mut self` draw calls. `len` = the line
/// count it was computed for, so a deferred reparse can pick a stale entry
/// whose row indices still line up (see `raw_editor_ts_spans`).
struct RawHlEntry {
    hash: u64,
    len: usize,
    spans: std::rc::Rc<Vec<Vec<(String, crate::syntax::SynKind)>>>,
}

/// 재파싱 간격을 **직전 파싱 비용의 몇 배로 벌리는지**. 고정 임계는 못 쓴다 —
/// 80ms 로 뒀더니 사람의 타이핑 간격(100~300ms)이 그보다 길어 매 키마다 그냥
/// 통과해 버렸다(실측: 20타에 재파싱 11회 잔존). 비용에 비례해 벌리면 9줄
/// 파일(0.84ms)은 사실상 즉시 갱신되고 5736줄(20ms)은 드물게 갱신된다 —
/// 파싱에 쓰는 시간이 어느 파일에서든 대략 1/12 로 묶인다.
const RAW_HL_COST_MULT: u32 = 12;
/// 비례 임계의 하한·상한. 하한은 작은 파일에서 매 프레임 파싱하지 않게,
/// 상한은 아주 큰 파일에서 색이 몇 초씩 낡아 있지 않게 잡는다.
const RAW_HL_QUIET_MIN_MS: u64 = 80;
const RAW_HL_QUIET_MAX_MS: u64 = 600;

/// One pane's slot in `render_frame`. Mirrors the data the existing
/// sugarloaf renderer carries through `PaneFrame` but trimmed to
/// what Phase 2a needs (background fills, fg color, and the wide
/// markers come back in 2b).
pub struct PaneSlot<'a> {
    pub rows: &'a [Vec<Cell>],
    /// Pane top-left in physical pixels.
    pub origin_px: (f32, f32),
    /// Per-pane font multiplier. The shared cell metric (`cell_w`/`cell_h`)
    /// and font size are multiplied by this so one pane can render bigger/
    /// smaller than its neighbours without touching the BSP layout (which
    /// stays on the base cell). 1.0 = same as the rest of the UI.
    pub font_scale: f32,
    /// Unfocused pane: glyphs render at reduced alpha (text-only dim) so
    /// the active pane stands out without darkening the whole box.
    pub dim: bool,
    /// Clickable URL ranges in this pane's visible rows. Drawn as accent
    /// underlines (always-on hyperlink affordance) after the glyph pass.
    pub links: Vec<crate::links::LinkSpan>,
    /// 이 pane 의 "기본 전경색" — tmux `window-style fg=<색>` 등가 pane 틴트.
    /// 테마 default fg 를 쓰는 셀만 이 색으로 풀리고 명시 색(ANSI/truecolor)은
    /// 그대로다. 무틴트 pane 은 `cells::default_fg()` 를 넣는다(셀당 추가 분기 0).
    pub default_fg: [u8; 4],
}

/// Pending chrome instances accumulated between `clear()` and the
/// next `render()`. Mirrors sugarloaf's immediate-mode API surface
/// (`rect`, `text_mut().draw`) but flushes through our retained
/// pipeline. Caller order is preserved so the rect-then-text painters
/// in main.rs paint in the same z-order as before.
#[derive(Default)]
#[allow(dead_code)]
pub struct ChromeBuffer {
    pub instances: Vec<CellInstance>,
}

#[derive(Debug, Clone, Copy)]
pub struct DrawOpts {
    pub font_size: f32,
    pub color: [u8; 4],
    pub bold: bool,
    pub italic: bool,
}

/// 셀 스냅샷에 **써넣은** 인레이 텍스트를 담는 통 — `KASATERM_TEXT_LOG` 가 있을 때만.
///
/// `text_log` 는 크롬 텍스트 draw 경로만 채운다. 입력박스 보더의 제목·pane 이름
/// 인레이는 셀에 직접 써넣어 그 경로를 안 타므로, 하네스가 "그 자리에 무엇이
/// 그려졌나"를 물을 수단이 없었다. 그 공백의 대가를 실제로 치렀다 — 칩 제거 관문이
/// 폭 조건에서 조용히 돌아서는 걸 아무 판정도 못 잡아 거노 화면까지 갔다(2026-08-05).
///
/// 메서드가 아니라 자유 함수인 이유: 슬롯 조립(`inlay_prompt_box_*`)은 `g` 를 만들기
/// **전에** 돌아서 `&mut GpuRenderer` 가 없다. 거기서 `self.gpu` 를 다시 빌리면
/// borrow 가 충돌한다.
fn cell_text_log() -> Option<&'static std::sync::Mutex<Vec<String>>> {
    static ON: std::sync::OnceLock<Option<std::sync::Mutex<Vec<String>>>> =
        std::sync::OnceLock::new();
    ON.get_or_init(|| {
        std::env::var_os("KASATERM_TEXT_LOG").map(|_| std::sync::Mutex::new(Vec::new()))
    })
    .as_ref()
}

/// 신고 통과 크롬 텍스트 로그를 **둘 다** 비운다 — 하네스가 "이 프레임에 그렸나"를
/// 물으려면 필수다.
///
/// 비우지 않으면 `drew_text` 는 "지금까지 한 번이라도"에 답하고, 그건 **꺼진 기능도
/// 통과시킨다**. 실제로 그랬다(2026-08-05): 인레이가 초반 프레임엔 그리다가 조건이
/// 무너져 꺼졌는데, 남아 있던 옛 신고가 뒤 프레임 판정을 PASS 로 만들었다. 판정
/// 직전에 이걸 부르고 → 한 프레임 그리고 → 그 프레임만 보라.
pub fn clear_text_logs(g: &mut GpuRenderer) {
    if let Some(log) = g.text_log.as_mut() {
        log.clear();
    }
    if let Some(m) = cell_text_log() {
        m.lock().unwrap().clear();
    }
}


/// `KASATERM_CELL_PROBE=<문자>` — 그 문자를 담은 행이 **글리프 패스에 도달할 때**
/// 그 행의 셀 속성을 한 번 찍는다.
///
/// 왜 여기인가: "셀에 써넣었는데 화면에 없다"를 진단할 자리는 쓴 쪽이 아니라 **받는
/// 쪽**이다. 쓴 쪽 로그는 데이터가 들어갔다는 것만 말하고, 그 뒤 어디서 떨어졌는지는
/// 침묵한다(2026-08-05: 인레이가 자기 결과 문자열을 뱉는데도 픽셀엔 없었고, 그
/// 사이 구간이 통째로 안 보였다). 이 프로브는 `hidden`(SGR 8 — 글리프만 생략하고
/// 텍스트 추출엔 남아 하네스가 "그렸다"로 읽는다)·`bold`·`fg`·`bg`·`dim` 을 나란히
/// 찍으므로, 안 보이는 셀과 보이는 이웃을 **같은 줄에서** 대조할 수 있다.
///
/// 행당 한 번만(프레임마다 반복하면 로그가 흐른다).
fn probe_cell_row(r: usize, row: &[kasa_bridge::screen::Cell], dim: bool, font_scale: f32) {
    use std::collections::HashSet;
    use std::sync::{Mutex, OnceLock};
    static WANT: OnceLock<Option<String>> = OnceLock::new();
    static SEEN: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    let Some(want) = WANT.get_or_init(|| std::env::var("KASATERM_CELL_PROBE").ok()).as_deref()
    else {
        return;
    };
    if want.is_empty() || !row.iter().any(|c| want.contains(c.ch)) {
        return;
    }
    let text: String = row.iter().map(|c| c.ch).collect();
    // 호출 순번을 키와 출력에 함께 싣는다 — 한 프레임에 `draw_cells` 가 두 번
    // 불리고 나중 것이 앞의 것을 덮으면, 순번 없이는 그 사실이 로그에서 안 보인다.
    let pass = DRAW_CELLS_CALLS.load(std::sync::atomic::Ordering::Relaxed);
    let key = format!("{pass}:{r}:{}", text.trim_end());
    if !SEEN.get_or_init(Default::default).lock().is_ok_and(|mut s| s.insert(key)) {
        return;
    }
    eprintln!(
        "[cell-probe] call#{pass} row {r} dim={dim} font_scale={font_scale} → {:?}",
        text.trim_end()
    );
    for (col, c) in row.iter().enumerate() {
        if matches!(c.ch, ' ' | '\0') {
            continue;
        }
        eprintln!(
            "  col {col:>3} {:?} bold={} hidden={} dim={} inverse={} fg={:?} bg={:?}",
            c.ch, c.bold, c.hidden, c.dim, c.inverse, c.fg, c.bg
        );
    }
}

/// `draw_cells` 진입 횟수 — `probe_cell_row` 가 "몇 번째 호출의 그리드인가"를 찍는다.
static DRAW_CELLS_CALLS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

impl GpuRenderer {
    pub fn new(window: Arc<Window>, font_size_logical: f32) -> Result<Self> {
        let scale = window.scale_factor() as f32;
        let font_size_px = (font_size_logical * scale).round() as u32;
        let size = window.inner_size();
        let instance = wgpu::Instance::default();
        // P3 color reproduction is the DEFAULT path. Despite ghostty's
        // `+show-config --default` advertising `window-colorspace = srgb`,
        // empirical measurement against ghostty's actual output shows it
        // applies the sRGB→Display P3 matrix in practice (e.g. emitting
        // sRGB byte (202,58,50) makes Digital Color Meter read (186,70,58)
        // on the same display, which matches the matrix-converted value).
        // To match ghostty byte-for-byte we have to run the same matrix.
        // Set `KASATERM_P3_ROOT=0` to fall back to the legacy
        // RawHandle/sublayer path (byte passthrough — useful only when
        // comparing against a non-P3 reference).
        #[cfg(target_os = "macos")]
        let p3_root = std::env::var("KASATERM_P3_ROOT")
            .ok()
            .map(|v| v != "0" && !v.eq_ignore_ascii_case("false"))
            .unwrap_or(true);
        #[cfg(not(target_os = "macos"))]
        let p3_root = false;
        let surface = if p3_root {
            #[cfg(target_os = "macos")]
            unsafe {
                let layer_ptr = install_root_p3_layer(&window, scale)
                    .context("install_root_p3_layer failed")?;
                let target = wgpu::SurfaceTargetUnsafe::CoreAnimationLayer(layer_ptr);
                instance.create_surface_unsafe(target)?
            }
            #[cfg(not(target_os = "macos"))]
            unreachable!()
        } else {
            let surface_target = wgpu::SurfaceTargetUnsafe::RawHandle {
                raw_display_handle: window.display_handle()?.as_raw(),
                raw_window_handle: window.window_handle()?.as_raw(),
            };
            unsafe { instance.create_surface_unsafe(surface_target)? }
        };
        // Live-resize would otherwise show the layer's stale pixels stretched
        // into the new bounds until our next frame lands. Pinning the layer's
        // contentsGravity to top-left keeps content anchored — same trick
        // ghostty uses (see feedback_tmuxify_rendering_pipeline).
        #[cfg(target_os = "macos")]
        unsafe {
            patch_metal_layer_gravity(&window);
            if !p3_root {
                // Legacy path: promote wgpu's observer CAMetalLayer to root
                // (try 5 — recorded as ineffective on macOS 26 because the
                // observer reattaches its own layer as a child). Kept for
                // the default `RawHandle` branch only.
                promote_metal_layer_to_root(&window, &surface);
            } else {
                eprintln!("[gpu] P3 root layer path active (KASATERM_P3_ROOT)");
            }
        }
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: Some(&surface),
            force_fallback_adapter: false,
        }))
        .context("no compatible wgpu adapter")?;
        let info = adapter.get_info();
        eprintln!(
            "[gpu] backend={:?} device={:?} type={:?}",
            info.backend, info.name, info.device_type
        );
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("kasaterm gpu device"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
            memory_hints: wgpu::MemoryHints::default(),
            experimental_features: wgpu::ExperimentalFeatures::default(),
            trace: wgpu::Trace::Off,
        }))?;
        let caps = surface.get_capabilities(&adapter);
        // Pick a NON-sRGB (linear-storage) framebuffer and feed it
        // already-sRGB-encoded colours directly. Why not an sRGB
        // target? Alpha blending. An sRGB target makes the hardware
        // blend glyph coverage in *linear* space, which lightens
        // anti-aliased edges and makes body text read thin/grey. A
        // plain Unorm target blends in gamma (sRGB) space — the same
        // gamma-incorrect-but-bolder blend sugarloaf / Terminal.app
        // use — so text matches. We hand it sRGB bytes and clear with
        // sRGB bytes, so the stored values are correct on screen too.
        // Non-sRGB Unorm + raw sRGB bytes + CAMetalLayer P3 tag = the
        // simplest path to "punchier" colours. The bytes the GPU stores
        // get reinterpreted as P3-encoded at scan-out → sRGB pure red
        // (byte 255) displays at P3 pure red chromaticity, which is the
        // wider-gamut "look". Switching to an sRGB-tagged framebuffer +
        // shader decode introduced round-trip precision loss that
        // visibly dimmed Claude Code's saturated bgs.
        // CAMetalLayer.colorspace = P3 is honored more reliably by macOS
        // when the surface pixel format has wider precision than plain
        // 8-bit Unorm. Try in order:
        //   1. Rgba16Float (HDR-capable, P3 always honored)
        //   2. Bgra8Unorm (legacy, works in sugarloaf but flaky on
        //      macOS 26 sublayer setups)
        // Env override KASATERM_PIXEL_FORMAT for diagnostics.
        let prefer = std::env::var("KASATERM_PIXEL_FORMAT").unwrap_or_default();
        let format = if prefer == "float" {
            caps.formats
                .iter()
                .copied()
                .find(|f| matches!(f, wgpu::TextureFormat::Rgba16Float))
                .unwrap_or(wgpu::TextureFormat::Bgra8Unorm)
        } else if prefer == "srgb" {
            caps.formats
                .iter()
                .copied()
                .find(|f| f.is_srgb())
                .unwrap_or_else(|| caps.formats[0].add_srgb_suffix())
        } else {
            caps.formats
                .iter()
                .copied()
                .find(|f| !f.is_srgb())
                .unwrap_or_else(|| caps.formats[0].remove_srgb_suffix())
        };
        eprintln!("[gpu] surface format = {:?} srgb={}", format, format.is_srgb());
        let config = wgpu::SurfaceConfiguration {
            // COPY_SRC lets us read the presented frame back into a buffer for
            // headless self-capture (no screen-recording permission needed).
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            // Fifo (vsync) queues 2-3 frames, adding 33-50ms of
            // input-to-screen latency — typing felt laggy vs Ghostty /
            // iTerm. AutoNoVsync picks the lowest-latency mode the
            // surface supports (Immediate or Mailbox), falling back to
            // Fifo only if neither exists. Tearing is irrelevant for a
            // text grid, and the damage gate already bounds how often we
            // actually present.
            present_mode: wgpu::PresentMode::AutoNoVsync,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
            // 1, not the wgpu default of 2: a 2-deep frame queue holds a
            // freshly-rendered frame for an extra vblank before it's
            // scanned out, so a keystroke that paints "now" only appears
            // ~1 frame later. The terminal renders tiny diffs, so a depth
            // of 1 is plenty and shaves that frame of input latency.
            desired_maximum_frame_latency: 1,
        };
        eprintln!(
            "[gpu] present_modes={:?} chosen={:?} frame_latency={}",
            caps.present_modes, config.present_mode, config.desired_maximum_frame_latency
        );
        surface.configure(&device, &config);

        // Phase 2a font path: macOS Menlo for now, mirrors the
        // grid_bw example. The real fallback chain (D2Coding →
        // Nerd Font → Segoe UI Symbol) reattaches in Phase 2c when
        // chrome text comes back.
        let font_path = std::env::var("KASATERM_GRID_FONT")
            .unwrap_or_else(|_| default_font_path());
        eprintln!("[font] primary={font_path}");
        let mut shaper = Shaper::from_path(&font_path, 0)
            .with_context(|| format!("load font {font_path}"))?;
        // Register a bold variant of the primary face. swash uses this when
        // a cell's BOLD flag is set; no variant → renderer can fall back to
        // double-draw synthesised bold (handled in draw_cells).
        if let Some((bold_path, bold_idx)) = primary_bold_font_path(&font_path) {
            shaper.set_bold_face_path(0, &bold_path, bold_idx);
        }
        // Real italic file (JetBrains Mono Italic etc). Without one, the
        // shaper synthesises italic via a 10° skew transform — works but
        // designed italic reads much cleaner. Same trick for the bold-
        // italic combo (renderer adds dilation on top of italic glyphs).
        if let Some((italic_path, italic_idx)) = primary_italic_font_path() {
            shaper.set_italic_face_path(0, &italic_path, italic_idx);
        }
        attach_fallback_chain(&mut shaper);
        // Markdown body font — a proportional gothic. Falls back to the primary
        // mono if the gothic can't load so the renderer never panics.
        let (md_font, md_idx) = md_font_path();
        let mut md_shaper = Shaper::from_path(&md_font, md_idx)
            .or_else(|_| Shaper::from_path(&font_path, 0))
            .with_context(|| format!("load markdown font {md_font}"))?;
        eprintln!("[font] markdown={md_font}");
        // Same bundled symbol/icon fallbacks so glyphs the gothic lacks still
        // resolve (and CJK falls through to the gothic's own coverage first).
        attach_fallback_chain(&mut md_shaper);
        // Bold weight of the markdown gothic.
        let (md_bold_font, md_bold_idx) = md_bold_font_path();
        let mut md_bold_shaper = Shaper::from_path(&md_bold_font, md_bold_idx)
            .or_else(|_| Shaper::from_path(&md_font, md_idx))
            .with_context(|| format!("load markdown bold font {md_bold_font}"))?;
        attach_fallback_chain(&mut md_bold_shaper);
        let cell_w = cell_w_for(&mut shaper, font_size_px as f32);
        // Use the font's natural line metric (ascent+descent+leading)
        // for cell height instead of an arbitrary multiplier. Lines
        // pack at the same density sugarloaf produces with
        // `line_height=1.0` (which itself reads the same metrics
        // under the hood via cosmic-text).
        let cell_h = shaper.line_height(font_size_px as f32).ceil();
        let mut atlas = Atlas::new(&device, &queue, ATLAS_SIZE);
        // Supersample glyphs on sub-Retina displays (scale < 2): at 100% DPI
        // the logical pixel size (e.g. 13px) is too small to resolve a crisp
        // coverage mask, so bake at 2x and let the Linear sampler downsample
        // — Retina-class sharpness without changing layout. Retina (scale>=2)
        // already has the pixels, so keep it 1:1.
        atlas.set_oversample(oversample_for(scale));
        for code in 0x20u32..0x7Fu32 {
            if let Some(ch) = char::from_u32(code) {
                let key = GlyphKey {
                    ch,
                    bold: false,
                    italic: false,
                    size_px: font_size_px,
                    font: 0,
                };
                let _ = atlas.get_or_bake(&device, &queue, &mut shaper, key);
            }
        }
        // filterable=true: the glyph atlas now uses a Linear sampler so the
        // supersampled glyphs downsample smoothly (see Atlas::set_oversample).
        let pipeline = Pipeline::with_filtering(&device, format, 32_768, true);
        let init_dims = [config.width as f32, config.height as f32];
        let (init_gamma, init_contrast, init_sat) = text_render_knobs();
        pipeline.write_uniforms_full(
            &queue,
            init_dims,
            init_gamma,
            init_contrast,
            init_sat,
            p3_root,
            0.0,
        );
        let bind_group = pipeline.make_bind_group(&device, atlas.view(), atlas.sampler());

        // Image pass: own buffer (a few quads), linear filtering for smooth
        // scaling. Shares the same screen-size uniform projection.
        let image_pipeline = Pipeline::with_filtering(&device, format, 64, true);
        image_pipeline.write_uniforms_full(
            &queue,
            init_dims,
            init_gamma,
            init_contrast,
            init_sat,
            p3_root,
            0.0,
        );
        let image_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("kasaterm image sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            ..Default::default()
        });

        Ok(Self {
            _window: window,
            surface,
            device,
            queue,
            capture_next: None,
            capture_crop: None,
            capture_max_w: 0,
            config,
            pipeline,
            atlas,
            shaper,
            chrome_shaper: None,
            chrome_shaper_failed: false,
            md_shaper,
            md_bold_shaper,
            bind_group,
            font_size_px,
            cell_w: cell_w / scale,
            cell_h: cell_h / scale,
            chrome: Vec::with_capacity(1024),
            text_log: std::env::var_os("KASATERM_TEXT_LOG").map(|_| Vec::new()),
            scale,
            p3_root_owned: p3_root,
            image_pipeline,
            image_sampler,
            images: HashMap::new(),
            image_quads: Vec::new(),
            icon_quads: Vec::new(),
            clip_stack: Vec::new(),
            clip_runs: Vec::new(),
            hover_pointer: false,
            md_link_rects: Vec::new(),
            md_copy_rects: Vec::new(),
            md_word_rects: Vec::new(),
            md_sel_screen: None,
            md_block_ys: Vec::new(),
            raw_hl: Vec::new(),
            raw_hl_pending: None,
            raw_hl_cost: std::time::Duration::ZERO,
            raw_hl_parsed_once: false,
            md_heights: Vec::new(),
        })
    }

    /// Logical-pixel solid rect (sugarloaf.rect drop-in). Caller
    /// passes the same logical coordinates main.rs has been using;
    /// we promote to physical pixels here to stay consistent with
    /// Resize the cell grid to a new logical font size (Cmd+= zoom).
    /// Atlas glyphs are keyed by size internally, so a re-bake happens
    /// lazily on the next draw — we just refresh the cached cell
    /// metrics so chrome/layout code sees the new geometry on the
    /// very next frame. Returns the new (cell_w, cell_h) in logical px.
    /// Update the effective render scale (DPI × ui_zoom). All chrome/cell
    /// draws multiply logical coords by `self.scale`, so changing it here and
    /// re-running `set_font_size` rescales the whole UI. Caller reflows layout.
    ///
    /// The atlas has to follow. Its supersampling factor is chosen from the
    /// scale, so leaving it stale after a monitor move bakes 1x-resolution
    /// coverage masks for a 1x display — the "글씨 깨짐" on the external
    /// monitor. And every cached entry is keyed by a `size_px` derived from
    /// the old scale, so without a repack the dead set just sits there until
    /// the texture is full.
    pub fn set_scale(&mut self, scale: f32) {
        let scale = scale.max(0.1);
        if (scale - self.scale).abs() < f32::EPSILON {
            return;
        }
        self.scale = scale;
        // set_oversample only requests a reset when the factor actually
        // changes (Retina↔Retina moves keep it), so ask explicitly — the
        // size_px keys are stale either way.
        self.atlas.set_oversample(oversample_for(scale));
        self.atlas.request_reset();
    }

    /// Current effective render scale the GPU side is drawing with. Used by
    /// the render loop to detect drift from the window's `effective_scale()`
    /// (a missed DPI change) and self-heal before painting a compressed frame.
    pub fn scale(&self) -> f32 {
        self.scale
    }

    /// Current font size in **logical** px — the value `set_font_size` was last
    /// given (round-tripped through the physical size it stores). Separate
    /// windows keep their own zoom here instead of in App's global `ui_zoom`,
    /// so a zoom shortcut needs to read back what this window is at.
    pub fn font_size(&self) -> f32 {
        self.font_size_px as f32 / self.scale
    }

    /// Repack the glyph atlas if it asked to be repacked — because a bake
    /// found no room, or because a DPI / font-size change invalidated every
    /// cached size. **Frame boundary only**: quads already queued this frame
    /// hold UVs into the current packing, and a repack would leave them
    /// pointing at whatever lands in those texels next.
    ///
    /// Missing glyphs re-bake on the paint that follows, so a full atlas
    /// costs one frame with some blank cells instead of blanking those
    /// characters for the rest of the session.
    pub fn maintain_atlas(&mut self) {
        let before = self.atlas.len();
        if self.atlas.begin_frame() {
            eprintln!("[gpu] atlas repacked ({before} glyphs dropped, scale={})", self.scale);
        }
    }

    /// True when the frame just painted left blank cells the next one can
    /// fill. The caller must schedule that frame — nothing else will.
    pub fn atlas_needs_another_frame(&self) -> bool {
        self.atlas.needs_another_frame()
    }

    /// Unconditional repack — the manual "화면 새로고침" escape hatch for
    /// state we failed to invalidate on our own.
    pub fn force_atlas_reset(&mut self) {
        self.atlas.request_reset();
    }

    /// Re-apply the surface configuration as-is. A monitor move can leave the
    /// swapchain describing the display we left; reconfiguring against the
    /// current size makes the next frame land on the one we are on.

    pub fn set_font_size(&mut self, font_size_logical: f32) -> (f32, f32) {
        let new_px = (font_size_logical * self.scale).round().max(8.0) as u32;
        // Only on a real change: this is called on every DPI event and every
        // reflow, usually with the value it already has, and an unconditional
        // repack would throw the atlas away several times per second.
        if new_px != self.font_size_px {
            self.atlas.request_reset();
        }
        self.font_size_px = new_px;
        let cell_w_px = cell_w_for(&mut self.shaper, new_px as f32);
        let cell_h_px = self.shaper.line_height(new_px as f32).ceil();
        self.cell_w = cell_w_px / self.scale;
        self.cell_h = cell_h_px / self.scale;
        eprintln!(
            "[gpu] font resized → size_px={} cell={}x{} (logical {}x{})",
            new_px, cell_w_px as u32, cell_h_px as u32, self.cell_w, self.cell_h
        );
        (self.cell_w, self.cell_h)
    }

    /// the cell pass. `rgba_f` is 0..1.
    /// Logical-pixel solid rect (sugarloaf.rect drop-in). Caller
    /// passes the same u8 RGBA they would have handed sugarloaf —
    /// we sRGB-decode here so the framebuffer's sRGB encode round-
    /// trips back to the same on-screen bytes.
    pub fn rect(&mut self, x: f32, y: f32, w: f32, h: f32, rgba_u8: [u8; 4]) {
        let s = self.scale;
        self.chrome.push(CellInstance {
            cell_px: [x * s, y * s, w * s, h * s],
            uv_min: Atlas::SOLID_UV,
            uv_max: Atlas::SOLID_UV,
            fg_rgba: srgb_rgba_to_linear(rgba_u8),
            ..Default::default()
        });
    }

    /// Working-indicator rail (logical px). Pushes ONE `FLAG_WORKING_BAR`
    /// instance; the shader sweeps an indeterminate ~32% segment over a faint
    /// track from `u.time`, so a busy pane's loading bar animates on the GPU
    /// and the CPU never re-emits the bar per frame — the key to idle-0 CPU
    /// while any pane is working. uv carries the 0..1 horizontal sweep coord.
    pub fn working_bar(&mut self, x: f32, y: f32, w: f32, h: f32, rgba_u8: [u8; 4]) {
        let s = self.scale;
        self.chrome.push(CellInstance {
            cell_px: [x * s, y * s, w * s, h * s],
            uv_min: [0.0, 0.0],
            uv_max: [1.0, 1.0],
            fg_rgba: srgb_rgba_to_linear(rgba_u8),
            flags: CellInstance::FLAG_WORKING_BAR,
            ..Default::default()
        });
    }

    /// Pulse-indicator rail (logical px). Pushes ONE `FLAG_PULSE_BAR` instance;
    /// the shader breathes a full-width fill's alpha on a slow 3s sine from
    /// `u.time`, so a pane with a background/Monitor job animates on the GPU with
    /// no per-frame CPU rebuild — same idle-0-CPU property as `working_bar`.
    pub fn pulse_bar(&mut self, x: f32, y: f32, w: f32, h: f32, rgba_u8: [u8; 4]) {
        let s = self.scale;
        self.chrome.push(CellInstance {
            cell_px: [x * s, y * s, w * s, h * s],
            uv_min: [0.0, 0.0],
            uv_max: [1.0, 1.0],
            fg_rgba: srgb_rgba_to_linear(rgba_u8),
            flags: CellInstance::FLAG_PULSE_BAR,
            ..Default::default()
        });
    }

    /// Compact-progress rail (logical px). Pushes ONE `FLAG_COMPACT_BAR` instance;
    /// the shader fills from the left on a 2.4s loop and restarts, so the header
    /// says "something with an end is running" — the shape a sweep can't say.
    /// Same idle-0-CPU property as the two bars above.
    ///
    /// 채운 칸이 실제 진행률은 아니다. claude 는 compact 진행률을 화면에만 내놓고
    /// 우리에게 넘기지 않으므로 시간으로 채운다. 그 화면 표시가 teammate 메시지
    /// 오버레이에 가려질 수 있어서 헤더에 따로 신호를 두는 것이 이 바의 존재 이유다.
    pub fn compact_bar(&mut self, x: f32, y: f32, w: f32, h: f32, rgba_u8: [u8; 4]) {
        let s = self.scale;
        self.chrome.push(CellInstance {
            cell_px: [x * s, y * s, w * s, h * s],
            uv_min: [0.0, 0.0],
            uv_max: [1.0, 1.0],
            fg_rgba: srgb_rgba_to_linear(rgba_u8),
            flags: CellInstance::FLAG_COMPACT_BAR,
            ..Default::default()
        });
    }

    /// Filled rounded rectangle (logical px) — circle-traced caps, same as
    /// main.rs's `round_rect` but a method so the markdown renderer can round
    /// code blocks / inline-code chips.
    pub fn round_rect_fill(&mut self, x: f32, y: f32, w: f32, h: f32, r: f32, col: [u8; 4]) {
        let r = r.min(w / 2.0).min(h / 2.0).max(0.0);
        // Straight middle band — no rounding needed between the two caps.
        self.rect(x, y + r, w, (h - 2.0 * r).max(0.0), col);
        if r <= 0.0 {
            return;
        }
        // Trace the caps at DEVICE-pixel resolution with a fractional-alpha
        // edge column so the corner reads smooth instead of stair-stepped.
        // The old version stepped one LOGICAL px (= 2 device px on retina)
        // with no anti-aliasing, which is the "hover 사각형 모서리 픽셀" the
        // user saw. One logical row here = `inv` device px; each row gets a
        // partial-coverage pixel on each side plus a solid middle span.
        let s = self.scale;
        let inv = 1.0 / s;
        let steps = (r * s).ceil() as i32;
        for k in 0..steps {
            let yy = k as f32 * inv; // logical distance inward from the cap edge
            let yc = yy + 0.5 * inv; // sample the row center for the circle test
            let d = (r * r - (r - yc) * (r - yc)).max(0.0).sqrt();
            let dx_dev = ((r - d) * s).max(0.0); // horizontal inset, device px
            let dx_floor = dx_dev.floor();
            let frac = dx_dev - dx_floor; // uncovered fraction of the boundary px
            let edge_col = [col[0], col[1], col[2], (col[3] as f32 * (1.0 - frac)).round() as u8];
            let lx = x + dx_floor * inv;
            let rx = x + w - (dx_floor + 1.0) * inv;
            let cx = x + (dx_floor + 1.0) * inv;
            let cw = (w - 2.0 * (dx_floor + 1.0) * inv).max(0.0);
            for ry in [y + yy, y + h - yy - inv] {
                self.rect(lx, ry, inv, inv, edge_col);
                self.rect(rx, ry, inv, inv, edge_col);
                if cw > 0.0 {
                    self.rect(cx, ry, cw, inv, col);
                }
            }
        }
    }

    /// Draw a text label using glyphs baked into the atlas at the
    /// requested size. Returns the pen-x after the last glyph
    /// (mirrors sugarloaf's `text.draw` return behaviour for callers
    /// that want it). Coordinates are logical pixels; `y` is the
    /// label's top edge — we approximate baseline via cell_h * 0.78
    /// matching the cell-grid path.
    /// Logical width `draw_text` would advance for `text` at `font_size`,
    /// without drawing. Same per-glyph stepping (wide-char tightening
    /// included) so tab backgrounds size to the exact drawn run.
    /// Resolve which face chrome text renders in, and at what device size.
    ///
    /// A pixel face only stays crisp on whole multiples of its dot grid — every
    /// Galmuri cut draws one dot per `upem/100` units, so Galmuri11 (upem 1200)
    /// wants multiples of 12 device px. Off-grid sizes resample the dots and the
    /// result reads as a blurry mono font rather than a pixel one. Measuring and
    /// drawing both come through here so the snapped size can never diverge.
    fn chrome_face(&mut self, font_size: f32) -> (u8, u32) {
        self.chrome_face_opt(font_size, false)
    }

    /// `force_mono` pins the terminal face regardless of shape — for chrome that
    /// is *depicting* the terminal (the theme cards' `ls -la` line). Drawing that
    /// in the UI face would make the preview lie about what the terminal shows.
    fn chrome_face_opt(&mut self, font_size: f32, force_mono: bool) -> (u8, u32) {
        let raw = (font_size * self.scale).round().max(1.0) as u32;
        if force_mono || !crate::theme::pixel_chrome() {
            return (0, raw);
        }
        self.ensure_chrome_shaper();
        if self.chrome_shaper.is_none() {
            return (0, raw);
        }
        let dot = GALMURI_DOT_PX as f32;
        let steps = (raw as f32 / dot).round().max(1.0);
        (3, (dot * steps) as u32)
    }

    fn ensure_chrome_shaper(&mut self) {
        if self.chrome_shaper.is_some() || self.chrome_shaper_failed {
            return;
        }
        match Shaper::from_bytes(GALMURI_11.to_vec(), 0) {
            Ok(mut sh) => {
                // Hangul and Latin come from Galmuri itself; the chain covers
                // the icon/symbol glyphs a text face has no reason to carry.
                attach_fallback_chain(&mut sh);
                self.chrome_shaper = Some(sh);
            }
            Err(e) => {
                eprintln!("[font] pixel chrome face failed to load: {e}");
                self.chrome_shaper_failed = true;
            }
        }
    }

    fn chrome_glyph(&mut self, key: GlyphKey) -> Option<AtlasEntry> {
        if key.font == 3 {
            if let Some(sh) = self.chrome_shaper.as_mut() {
                return self.atlas.get_or_bake(&self.device, &self.queue, sh, key);
            }
        }
        self.atlas
            .get_or_bake(&self.device, &self.queue, &mut self.shaper, key)
    }

    /// Space width for chrome runs. The mono primary's cell advance is the right
    /// answer for itself, but on the proportional pixel face it would space words
    /// out to an 'M' — that face gets its own designed space instead.
    fn chrome_space_advance(&mut self, size_px: f32, font: u8) -> f32 {
        if font == 3 {
            if let Some(sh) = self.chrome_shaper.as_ref() {
                return sh.advance(' ', size_px);
            }
        }
        self.shaper.cell_advance(size_px)
    }

    pub fn measure_chrome_text(&mut self, text: &str, font_size: f32, bold: bool) -> f32 {
        let s = self.scale;
        let (font, size_px) = self.chrome_face(font_size);
        let mut pen = 0.0_f32;
        for ch in text.chars() {
            if ch == ' ' {
                pen += self.chrome_space_advance(size_px as f32, font);
                continue;
            }
            let key = GlyphKey {
                ch,
                bold,
                italic: false,
                size_px,
                font,
            };
            if let Some(entry) = self.chrome_glyph(key) {
                pen += if is_wide_char(ch) {
                    entry.px_w as f32 + size_px as f32 * 0.18
                } else {
                    entry.advance
                };
            }
        }
        pen / s
    }

    pub fn draw_text(&mut self, x: f32, y: f32, text: &str, opts: DrawOpts) -> f32 {
        self.draw_text_clipped(x, y, text, opts, f32::NEG_INFINITY, f32::INFINITY)
    }

    /// `draw_text` pinned to the terminal face — see `chrome_face_opt`.
    pub fn draw_text_mono(&mut self, x: f32, y: f32, text: &str, opts: DrawOpts) -> f32 {
        self.draw_text_inner(x, y, text, opts, f32::NEG_INFINITY, f32::INFINITY, true)
    }

    /// `draw_text` with hard left/right edges (logical px): a glyph that would
    /// cross either edge is skipped, but the pen keeps advancing so the returned
    /// width stays accurate. This renderer has no scissor (see render loop), so
    /// a Raw-editor pane's long code line would otherwise bleed past the pane
    /// (right) or, once panned by horizontal scroll, into the line-number gutter
    /// (left). Pass the pane's right edge and the gutter's right edge to fence
    /// the text in on both sides.
    pub fn draw_text_clipped(
        &mut self,
        x: f32,
        y: f32,
        text: &str,
        opts: DrawOpts,
        clip_left: f32,
        clip_right: f32,
    ) -> f32 {
        self.draw_text_inner(x, y, text, opts, clip_left, clip_right, false)
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_text_inner(
        &mut self,
        x: f32,
        y: f32,
        text: &str,
        opts: DrawOpts,
        clip_left: f32,
        clip_right: f32,
        force_mono: bool,
    ) -> f32 {
        let s = self.scale;
        if let Some(log) = self.text_log.as_mut() {
            log.push(text.to_string());
        }
        let (font, size_px) = self.chrome_face_opt(opts.font_size, force_mono);
        // The pixel face sets ascent == em, so its glyphs sit far higher above
        // the baseline than the mono primary's 0.78 assumption — without the
        // taller ratio the whole label rides up out of its row.
        let baseline_ratio = if font == 3 { 0.92 } else { 0.78 };
        let baseline_px = y * s + (size_px as f32 * baseline_ratio);
        let fg = srgb_rgba_to_linear(opts.color);
        let clip_l = clip_left * s;
        let clip_px = clip_right * s;
        let mut pen = x * s;
        for ch in text.chars() {
            if ch == ' ' {
                pen += self.chrome_space_advance(size_px as f32, font);
                continue;
            }
            let key = GlyphKey {
                ch,
                bold: opts.bold,
                italic: opts.italic,
                size_px,
                font,
            };
            let Some(entry) = self.chrome_glyph(key) else {
                continue;
            };
            let glyph_x = pen + entry.bearing_x as f32;
            let glyph_y = baseline_px - entry.bearing_y as f32;
            if glyph_x < clip_l || glyph_x + entry.px_w as f32 > clip_px {
                pen += Self::pen_step(ch, entry.px_w as f32, entry.advance, size_px as f32);
                continue;
            }
            self.chrome.push(CellInstance {
                cell_px: [glyph_x, glyph_y, entry.px_w as f32, entry.px_h as f32],
                uv_min: entry.uv_min,
                uv_max: entry.uv_max,
                fg_rgba: fg,
                ..Default::default()
            });
            pen += Self::pen_step(ch, entry.px_w as f32, entry.advance, size_px as f32);
        }
        pen / s
    }

    /// 비례 배치 텍스트에서 글자 하나가 펜을 얼마나 밀어내는가 —
    /// `draw_text_clipped` 이 실제로 쓰는 규칙 그 자체다.
    ///
    /// 헤더·편집기 텍스트는 모노 격자가 아니다. 와이드(CJK) 글리프는 모노
    /// 페이스에서 2셀에 가까운 advance 를 들고 오는데 그대로 쓰면 작은
    /// 라벨의 한글이 뜨문뜨문 벌어져 보인다("탭이름 테스트"). 그래서 잉크
    /// 폭 + 약간의 트래킹으로 좁힌다.
    ///
    /// 캐럿·선택 밴드·클릭 히트테스트가 이 함수를 거치지 않으면 화면과
    /// 좌표가 갈린다 — 한글 3글자를 선택했는데 밴드가 2.3글자만 덮던
    /// 실측 버그가 정확히 그것이었다(측정 쪽만 폰트 원시 advance 를 썼다).
    fn pen_step(ch: char, px_w: f32, advance: f32, size_px: f32) -> f32 {
        if is_wide_char(ch) {
            px_w + size_px * 0.18
        } else {
            advance
        }
    }

    /// `draw_text` 가 이 글자에서 펜을 미는 거리(논리 px).
    ///
    /// 코드 블록 접기는 잴 때와 그릴 때가 같은 규칙이어야 한다 — 자가
    /// 다르면 접은 자리가 한 글자씩 어긋나 상자 안에서 잘리거나 밖으로
    /// 삐져나간다.
    fn mono_advance(&mut self, ch: char, font_size: f32) -> f32 {
        let s = self.scale;
        let size_px = (font_size * s).round() as u32;
        if ch == ' ' {
            return self.shaper.cell_advance(size_px as f32) / s;
        }
        let key = GlyphKey {
            ch,
            bold: false,
            italic: false,
            size_px,
            font: 0,
        };
        match self
            .atlas
            .get_or_bake(&self.device, &self.queue, &mut self.shaper, key)
        {
            Some(e) => Self::pen_step(ch, e.px_w as f32, e.advance, size_px as f32) / s,
            None => 0.0,
        }
    }

    /// 편집기 코드 줄을 **격자**에 그린다 — 글자마다 정수 칸.
    ///
    /// 배치 규칙은 터미널(`draw_cells`)과 **같다**: 와이드(CJK·한글)는 2칸 박스
    /// 중앙(넘치면 종횡비 유지 축소), 좁은데 칸보다 넓은 글리프(①ⓐⅠ 같은
    /// Ambiguous)는 이웃 **빈칸으로 슬라이드**한다(`fit_cell_glyph`). 같은 문서가
    /// 터미널과 편집기에서 같은 자리에 놓여야 하니 규칙이 하나여야 한다.
    ///
    /// 비례 배치(`draw_text_clipped`)와 갈라 둔 이유: 편집기는 한글이 섞여도
    /// 들여쓰기와 세로 정렬이 맞아야 하고, 좌표가 정수 칸이면 캐럿·선택·클릭이
    /// 아틀라스 조회 없이 곱셈 한 번으로 나온다. 마크다운 렌더 뷰는 계속 비례
    /// 배치이므로 그쪽은 이 함수를 쓰지 않는다.
    ///
    /// `cells` 는 글자별 색 — 토큰을 이어 그리는 대신 줄 전체를 한 번에 받는다.
    /// 이웃 칸이 비었는지 봐야 슬라이드를 판단할 수 있고, 토큰 단위로 끊으면
    /// 경계에서 그 판단이 불가능하다. 반환값은 그 줄이 쓴 칸 수.
    pub fn draw_editor_cells(
        &mut self,
        cells: &[(char, [u8; 4])],
        line_x: f32,
        y: f32,
        size: f32,
        clip_left: f32,
        clip_right: f32,
    ) -> usize {
        let s = self.scale;
        let size_px = (size * s).round().max(1.0) as u32;
        let cw = self.cell_w * s;
        let baseline_y = y * s + size_px as f32 * 0.78;
        let clip_l = clip_left * s;
        let clip_r = clip_right * s;
        let x0 = line_x * s;
        let blank = |c: Option<&(char, [u8; 4])>| matches!(c, Some(&(' ' | '\t', _)));
        let mut col = 0usize;
        // 슬라이드가 왼쪽으로 얼마나 갈 수 있는지 재려면 앞 글리프가 실제로
        // 어디까지 찼는지 알아야 한다(터미널과 같은 이유).
        let mut glyph_right = x0;
        for (i, &(ch, color)) in cells.iter().enumerate() {
            let step = 1 + usize::from(is_wide_char(ch));
            if ch == ' ' || ch == '\t' {
                col += step;
                continue;
            }
            let key = GlyphKey {
                ch,
                bold: false,
                italic: false,
                size_px,
                font: 0,
            };
            let Some(entry) =
                self.atlas
                    .get_or_bake(&self.device, &self.queue, &mut self.shaper, key)
            else {
                col += step;
                continue;
            };
            let cell_x = x0 + col as f32 * cw;
            let rect = if step == 2 {
                let span = cw * 2.0;
                let gw0 = entry.px_w as f32;
                let fit = if gw0 > span { span / gw0 } else { 1.0 };
                [
                    cell_x + (span - gw0 * fit) * 0.5,
                    baseline_y - entry.bearing_y as f32 * fit,
                    gw0 * fit,
                    entry.px_h as f32 * fit,
                ]
            } else {
                let room_right = if blank(cells.get(i + 1)) { cw } else { 0.0 };
                let room_left = if i > 0 && blank(cells.get(i - 1)) {
                    cw.min((cell_x - glyph_right).max(0.0))
                } else {
                    0.0
                };
                fit_cell_glyph(&entry, cell_x, baseline_y, cw, room_left, room_right)
            };
            glyph_right = rect[0] + rect[2];
            if rect[0] >= clip_l && rect[0] + rect[2] <= clip_r {
                self.chrome.push(CellInstance {
                    cell_px: rect,
                    uv_min: entry.uv_min,
                    uv_max: entry.uv_max,
                    fg_rgba: srgb_rgba_to_linear(color),
                    ..Default::default()
                });
            }
            col += step;
        }
        col
    }

    /// `draw_text_clipped` 이 그릴 폭(logical px) — 같은 규칙·같은 폰트(0)로
    /// 펜만 굴리고 글리프는 안 그린다. 편집기의 캐럿 x·선택 밴드 경계·
    /// 클릭 히트테스트·거터 폭은 전부 이걸로 재야 한다.
    ///
    /// `measure_run` 과 헷갈리면 안 된다 — 그쪽은 `md_draw_word`(마크다운
    /// 비례 배치)의 파트너로, 공백을 폰트 metric 으로 재고 CJK 를 고딕
    /// 페이스로 넘길 수 있다. 두 렌더 경로가 규칙이 다르므로 측정 함수도
    /// 둘이어야 하고, 섞어 쓰면 어느 한쪽이 반드시 어긋난다.
    pub fn measure_pen_run(&mut self, text: &str, size: f32, bold: bool, italic: bool) -> f32 {
        let s = self.scale;
        let size_px = (size * s).round().max(1.0) as u32;
        let mut pen = 0.0f32;
        for ch in text.chars() {
            if ch == ' ' {
                pen += self.shaper.cell_advance(size_px as f32);
                continue;
            }
            let key = GlyphKey {
                ch,
                bold,
                italic,
                size_px,
                font: 0,
            };
            let Some(entry) =
                self.atlas
                    .get_or_bake(&self.device, &self.queue, &mut self.shaper, key)
            else {
                continue;
            };
            pen += Self::pen_step(ch, entry.px_w as f32, entry.advance, size_px as f32);
        }
        pen / s
    }

    /// Draw the IME preedit (composing Hangul) the SAME way the cell
    /// grid draws committed text. `draw_text` used a `size_px * 0.78`
    /// baseline, but the grid uses `cell_h_px * 0.78`; since the line
    /// height is taller than the font size the composing syllable
    /// floated above the row ("조합 중 글자가 올라간다"). It also walked
    /// the pen by glyph advance, which drifts wide chars. Here we pin to
    /// the cell grid: cell-grid baseline + per-glyph 2-cell fit, exactly
    /// like draw_cells, mirroring the sugarloaf fix that routes preedit
    /// through render_row. `origin` is logical px (top-left of the
    /// anchor cell); colors are the accent (text + underline).
    pub fn draw_preedit(
        &mut self,
        origin_x: f32,
        origin_y: f32,
        text: &str,
        accent: [u8; 4],
        font_scale: f32,
    ) {
        let cell_w_px = self.cell_w * self.scale * font_scale;
        let cell_h_px = self.cell_h * self.scale * font_scale;
        // Glyph atlas size follows the pane zoom too — same rounding as
        // draw_cells so the composing syllable matches committed text.
        let size_px = ((self.font_size_px as f32 * font_scale).round() as u32).max(8);
        let ox = origin_x * self.scale;
        let oy = origin_y * self.scale;
        // Cell span: wide (CJK/Hangul) chars take two columns.
        let span_cells: u32 = text
            .chars()
            .map(|c| if is_wide_char(c) { 2 } else { 1 })
            .sum();
        let span_px = span_cells.max(1) as f32 * cell_w_px;
        // Opaque background so the composing glyph isn't muddied by the
        // grid cells underneath, plus an accent underline.
        self.chrome.push(CellInstance {
            cell_px: [ox, oy, span_px, cell_h_px],
            uv_min: Atlas::SOLID_UV,
            uv_max: Atlas::SOLID_UV,
            fg_rgba: srgb_rgba_to_linear(crate::cells::default_bg()),
            ..Default::default()
        });
        let acc = srgb_rgba_to_linear(accent);
        self.chrome.push(CellInstance {
            cell_px: [ox, oy + cell_h_px - 2.0 * self.scale, span_px, 2.0 * self.scale],
            uv_min: Atlas::SOLID_UV,
            uv_max: Atlas::SOLID_UV,
            fg_rgba: acc,
            ..Default::default()
        });
        // Glyphs — identical placement math to draw_cells.
        let baseline_y = oy + cell_h_px * 0.78;
        let mut col = 0u32;
        for ch in text.chars() {
            let wide = is_wide_char(ch);
            if ch != ' ' {
                let key = GlyphKey {
                    ch,
                    bold: false,
                    italic: false,
                    size_px,
                    font: 0,
                };
                if let Some(entry) = self.atlas.get_or_bake(
                    &self.device,
                    &self.queue,
                    &mut self.shaper,
                    key,
                ) {
                    let cell_x = ox + col as f32 * cell_w_px;
                    if wide {
                        let span_w = cell_w_px * 2.0;
                        let gw0 = entry.px_w as f32;
                        let scale_fit = if gw0 > span_w { span_w / gw0 } else { 1.0 };
                        let gw = gw0 * scale_fit;
                        let gh = entry.px_h as f32 * scale_fit;
                        let x = cell_x + (span_w - gw) * 0.5;
                        let y = baseline_y - entry.bearing_y as f32 * scale_fit;
                        self.chrome.push(CellInstance {
                            cell_px: [x, y, gw, gh],
                            uv_min: entry.uv_min,
                            uv_max: entry.uv_max,
                            fg_rgba: acc,
                            ..Default::default()
                        });
                    } else {
                        self.chrome.push(CellInstance {
                            // preedit / ghost text is drawn standalone, with no
                            // row to look sideways into — no room to lend.
                            cell_px: fit_cell_glyph(
                                &entry, cell_x, baseline_y, cell_w_px, 0.0, 0.0,
                            ),
                            uv_min: entry.uv_min,
                            uv_max: entry.uv_max,
                            fg_rgba: acc,
                            ..Default::default()
                        });
                    }
                }
            }
            col += if wide { 2 } else { 1 };
        }
    }

    /// Draw inline-autosuggestion ghost text. Same cell-grid placement
    /// math as `draw_preedit` / `draw_cells`, but with NO background fill
    /// or underline and a dim foreground, so it reads as a hint sitting
    /// behind where the user would type. `max_cells` clips it to the
    /// remaining columns on the row (no wrapping). `origin` is logical px
    /// at the top-left of the first ghost cell.
    pub fn draw_ghost(
        &mut self,
        origin_x: f32,
        origin_y: f32,
        text: &str,
        max_cells: u32,
        font_scale: f32,
    ) {
        let cell_w_px = self.cell_w * self.scale * font_scale;
        let cell_h_px = self.cell_h * self.scale * font_scale;
        let size_px = ((self.font_size_px as f32 * font_scale).round() as u32).max(8);
        let ox = origin_x * self.scale;
        let oy = origin_y * self.scale;
        let fg = srgb_rgba_to_linear(crate::cells::GHOST_FG);
        let baseline_y = oy + cell_h_px * 0.78;
        let mut col = 0u32;
        for ch in text.chars() {
            let wide = is_wide_char(ch);
            let span = if wide { 2 } else { 1 };
            if col + span > max_cells {
                break;
            }
            if ch != ' ' {
                let key = GlyphKey {
                    ch,
                    bold: false,
                    italic: false,
                    size_px,
                    font: 0,
                };
                if let Some(entry) =
                    self.atlas
                        .get_or_bake(&self.device, &self.queue, &mut self.shaper, key)
                {
                    let cell_x = ox + col as f32 * cell_w_px;
                    if wide {
                        let span_w = cell_w_px * 2.0;
                        let gw0 = entry.px_w as f32;
                        let scale_fit = if gw0 > span_w { span_w / gw0 } else { 1.0 };
                        let gw = gw0 * scale_fit;
                        let gh = entry.px_h as f32 * scale_fit;
                        let x = cell_x + (span_w - gw) * 0.5;
                        let y = baseline_y - entry.bearing_y as f32 * scale_fit;
                        self.chrome.push(CellInstance {
                            cell_px: [x, y, gw, gh],
                            uv_min: entry.uv_min,
                            uv_max: entry.uv_max,
                            fg_rgba: fg,
                            ..Default::default()
                        });
                    } else {
                        self.chrome.push(CellInstance {
                            // preedit / ghost text is drawn standalone, with no
                            // row to look sideways into — no room to lend.
                            cell_px: fit_cell_glyph(
                                &entry, cell_x, baseline_y, cell_w_px, 0.0, 0.0,
                            ),
                            uv_min: entry.uv_min,
                            uv_max: entry.uv_max,
                            fg_rgba: fg,
                            ..Default::default()
                        });
                    }
                }
            }
            col += span;
        }
    }

    /// Bake (or fetch cached) a glyph from the requested font (0 = primary
    /// mono, 1 = markdown gothic) into the shared atlas. Centralizes the
    /// shaper choice so every caller stays consistent.
    fn bake_glyph(
        &mut self,
        ch: char,
        bold: bool,
        italic: bool,
        size_px: u32,
        font: u8,
    ) -> Option<AtlasEntry> {
        let key = GlyphKey { ch, bold, italic, size_px, font };
        match font {
            2 => self
                .atlas
                .get_or_bake(&self.device, &self.queue, &mut self.md_bold_shaper, key),
            1 => self
                .atlas
                .get_or_bake(&self.device, &self.queue, &mut self.md_shaper, key),
            _ => self
                .atlas
                .get_or_bake(&self.device, &self.queue, &mut self.shaper, key),
        }
    }

    /// Space/cell advance for the requested font at `size_px`.
    #[allow(dead_code)]
    fn font_cell_advance(&mut self, size_px: u32, font: u8) -> f32 {
        match font {
            2 => self.md_bold_shaper.cell_advance(size_px as f32),
            1 => self.md_shaper.cell_advance(size_px as f32),
            _ => self.shaper.cell_advance(size_px as f32),
        }
    }

    /// True space advance for the requested font (metrics, not the 'M' cell
    /// width) — markdown word spacing.
    fn font_space_advance(&self, size_px: u32, font: u8) -> f32 {
        let sz = size_px as f32;
        match font {
            2 => self.md_bold_shaper.advance(' ', sz),
            1 => self.md_shaper.advance(' ', sz),
            _ => self.shaper.advance(' ', sz),
        }
    }

    /// Draw a single word (no internal wrapping) at logical (x, y) using the
    /// given font. Mirrors draw_text's glyph placement but lets the markdown
    /// renderer pick the gothic (font=1) for prose and mono (font=0) for code.
    fn md_draw_word(
        &mut self,
        text: &str,
        x: f32,
        y: f32,
        size: f32,
        color: [u8; 4],
        bold: bool,
        italic: bool,
        font: u8,
        // Inline code only: route CJK glyphs to the gothic body font. The mono
        // code face's Hangul advance is narrower than its raster, so a mono
        // syllable overlaps the next; the gothic face has matching metrics.
        cjk_gothic: bool,
    ) {
        let s = self.scale;
        let size_px = (size * s).round().max(1.0) as u32;
        let baseline = y * s + size_px as f32 * 0.78;
        let fg = srgb_rgba_to_linear(color);
        let mut pen = x * s;
        // Proportional layout: each glyph advances by its own font metric. No
        // mono-grid wide-char fudge (terminal-only; made Hangul read loose).
        // Space has no raster, so its advance comes from metrics.
        for ch in text.chars() {
            let gfont = if cjk_gothic && is_wide_char(ch) {
                if bold { 2 } else { 1 }
            } else {
                font
            };
            if ch == ' ' {
                pen += self.font_space_advance(size_px, gfont);
                continue;
            }
            if let Some(e) = self.bake_glyph(ch, bold, italic, size_px, gfont) {
                {
                    let gx = pen + e.bearing_x as f32;
                    let gy = baseline - e.bearing_y as f32;
                    let (col, flags) = if e.is_color {
                        ([1.0, 1.0, 1.0, 1.0], CellInstance::FLAG_COLOR)
                    } else {
                        (fg, 0)
                    };
                    self.chrome.push(CellInstance {
                        cell_px: [gx, gy, e.px_w as f32, e.px_h as f32],
                        uv_min: e.uv_min,
                        uv_max: e.uv_max,
                        fg_rgba: col,
                        flags,
                        ..Default::default()
                    });
                }
                pen += e.advance;
            }
        }
    }

    /// Width (logical px) a styled run occupies, matching `md_draw_word`'s
    /// advance so word-wrap measurement equals what gets drawn. `code` selects
    /// the mono font (0); prose uses the gothic (1).
    fn measure_run(
        &mut self,
        text: &str,
        size: f32,
        bold: bool,
        italic: bool,
        code: bool,
        cjk_gothic: bool,
    ) -> f32 {
        let s = self.scale;
        let size_px = (size * s).round().max(1.0) as u32;
        let base_font: u8 = if code {
            0
        } else if bold {
            2
        } else {
            1
        };
        let mut w = 0.0;
        for ch in text.chars() {
            // Match md_draw_word: inline-code CJK measures on the gothic face.
            let font = if cjk_gothic && is_wide_char(ch) {
                if bold { 2 } else { 1 }
            } else {
                base_font
            };
            if ch == ' ' {
                w += self.font_space_advance(size_px, font);
                continue;
            }
            if let Some(e) = self.bake_glyph(ch, bold, italic, size_px, font) {
                w += e.advance;
            }
        }
        w / s
    }

    /// Lay styled spans into `max_w` at logical (x_start, y_start), wrapping on
    /// word boundaries. Returns pen_y after the last line. Lines fully outside
    /// [clip_top, clip_bot) are skipped — that's the scroll clip for markdown.
    /// 선택 띠 한 칸. 높이는 줄간격이 아니라 글자 박스에 맞춘다 — 줄간격(1.5배)
    /// 으로 깔면 띠가 글자 아래 여백까지 먹어 글줄이 아래로 밀려 보인다.
    /// 전용 선택 토큰은 없어 accent 의 알파만 낮춰 쓴다(글자 밑에 깔리는 배경).
    fn md_sel_band(&mut self, x: f32, y: f32, w: f32, size: f32) {
        let mut col = crate::theme::accent();
        col[3] = 90;
        self.rect(x, y - size * 0.1, w, size * 1.22, col);
    }

    fn md_runs(
        &mut self,
        spans: &[crate::MdSpan],
        x_start: f32,
        y_start: f32,
        max_w: f32,
        size: f32,
        force_bold: bool,
        color: [u8; 4],
        clip_top: f32,
        clip_bot: f32,
    ) -> f32 {
        // Line metrics from the gothic (markdown body font), even when a run
        // is inline code — keeps the baseline steady across a mixed line.
        // 1.5× the natural line height for Notion-like airy paragraphs.
        let lh = (self.md_shaper.line_height(size * self.scale).ceil() / self.scale) * 1.5;
        // Real space advance, not the 'M' cell width (that over-spaced words).
        let space_w = self.measure_run(" ", size, false, false, false, false);
        let mut pen_x = x_start;
        let mut pen_y = y_start;
        // 앞 낱말이 같은 줄의 인라인 코드였고, 그 칩이 사이 공백까지 덮었는가.
        // 칩 이음매를 메울지 판단하는 유일한 근거다(앞뒤 예측 없이 이 한 칸).
        let mut code_joint = false;
        for span in spans {
            let bold = span.bold || force_bold;
            for word in span.text.split_inclusive(' ') {
                let trailing_space = word.ends_with(' ');
                let trimmed = word.trim_end_matches(' ');
                if trimmed.is_empty() {
                    // 스팬 경계의 공백은 코드 런 *안쪽*이 아니다(`a` `b` 는 칩
                    // 두 개다) — 이음매를 끊어 별개의 칩이 하나로 붙지 않게 한다.
                    code_joint = false;
                    // 스팬 경계에 붙은 선행 공백(`[링크](…) 가` 의 " ")은 낱말이
                    // 아니라 버려졌는데, 그러면 선택 띠가 그 폭에서 끊기고 복사문에서
                    // 공백이 사라진다 — 폭 있는 빈 칸으로 똑같이 기록한다.
                    if trailing_space && pen_y + lh > clip_top && pen_y < clip_bot {
                        self.md_word_rects
                            .push((pen_x, pen_y, space_w, size, word.to_string()));
                        if word_in_sel(self.md_sel_screen, pen_x + space_w * 0.5, pen_y + size * 0.5)
                        {
                            self.md_sel_band(pen_x, pen_y, space_w, size);
                        }
                    }
                } else {
                    let ww = self.measure_run(trimmed, size, bold, span.italic, span.code, span.code);
                    if pen_x + ww > x_start + max_w && pen_x > x_start {
                        pen_x = x_start;
                        pen_y += lh;
                        // 줄이 바뀌면 앞 칩은 다른 줄에 있다 — 이음매 없음.
                        code_joint = false;
                    }
                    if pen_y + lh > clip_top && pen_y < clip_bot {
                        // 선택은 셀 격자가 없어 "그려진 낱말" 이 유일한 기준이다 —
                        // 낱말 사각형을 적어 두고(복사·히트테스트가 이걸 읽는다),
                        // 범위에 들면 글자 **전에** 띠를 깔아야 배경이 된다(rect 와
                        // 글리프가 같은 버퍼라 나중에 그리면 글자를 덮는다). 높이는
                        // 줄간격(lh, 1.5배)이 아니라 글자 박스(size)다 — lh 로 재면
                        // 중심이 글자 아래 여백에 떨어져 히트 판정이 한 줄씩 밀린다.
                        // 원문 공백을 살려 적어 복사문이 원문 간격 그대로 나온다.
                        self.md_word_rects
                            .push((pen_x, pen_y, ww, size, word.to_string()));
                        if word_in_sel(self.md_sel_screen, pen_x + ww * 0.5, pen_y + size * 0.5) {
                            let band = ww + if trailing_space { space_w } else { 0.0 };
                            self.md_sel_band(pen_x, pen_y, band, size);
                        }
                        if span.code {
                            // Notion-style chip: a hair *lighter* than the body
                            // (SURFACE_ACTIVE > BG) so the code reads as a raised
                            // pill, not a black hole. (BORDER was near-black and
                            // swallowed the glyphs.) Size off the glyph metrics
                            // (not the 1.5× line height) so it hugs the text, and
                            // span the trailing space so a multi-word `inline
                            // code` is one chip, not one box per word.
                            let chip_r = size * 0.28;
                            let chip_y = pen_y + size * 0.06;
                            let chip_h = size * 1.04;
                            if code_joint {
                                // 칩을 낱말마다 그리니 이웃과의 겹침(0.4×공백)이
                                // 모서리 지름(2r)보다 좁고, 그러면 두 칩이 서로의
                                // 모서리 호를 못 메워 이음매 위·아래에 홈이 남는다
                                // — 물결 모양 알약. 겹침이 2r 이 되게 늘리려면
                                // 다음 낱말을 미리 알아야 하고(줄바꿈까지 예측),
                                // 뒤 칩을 왼쪽으로 늘리면 이미 그려진 앞 낱말 글자를
                                // 덮는다(rect 와 글리프가 같은 버퍼). 그래서 두 칩이
                                // 각자 모서리를 깎는 그 구간만 각진 사각형으로 메운다.
                                // 왼쪽 끝은 앞 낱말 글자 끝(pen_x - 공백)까지만.
                                let jl = (pen_x + space_w * 0.2 - chip_r).max(pen_x - space_w);
                                let jr = pen_x + chip_r - space_w * 0.2;
                                if jr > jl {
                                    self.rect(
                                        jl,
                                        chip_y,
                                        jr - jl,
                                        chip_h,
                                        crate::theme::surface_active(),
                                    );
                                }
                            }
                            let chip_w = ww
                                + space_w * 0.4
                                + if trailing_space { space_w } else { 0.0 };
                            self.round_rect_fill(
                                pen_x - space_w * 0.2,
                                chip_y,
                                chip_w,
                                chip_h,
                                chip_r,
                                crate::theme::surface_active(),
                            );
                        }
                        if span.code {
                            // Inline code: syntax-highlight the word token by
                            // token (same lexer as code blocks; language is
                            // unknown inline so the generic keyword set applies),
                            // chaining pen-x with measure_run. Hangul still routes
                            // to the gothic via cjk_gothic=true so it never
                            // overlaps inside the chip.
                            let mut tpx = pen_x;
                            for (tok, tcol) in highlight_code_line(trimmed, "", crate::theme::text()) {
                                self.md_draw_word(
                                    &tok, tpx, pen_y, size, tcol, bold, span.italic, 0, true,
                                );
                                tpx += self.measure_run(&tok, size, bold, span.italic, true, true);
                            }
                        } else {
                            // Link → tint by destination kind; otherwise the
                            // block's own color.
                            let col = match &span.link {
                                Some(d) => link_color(d),
                                None => color,
                            };
                            let font: u8 = if bold { 2 } else { 1 };
                            self.md_draw_word(
                                trimmed, pen_x, pen_y, size, col, bold, span.italic, font, false,
                            );
                        }
                        if span.strike {
                            // 링크 밑줄과 같은 획을 x-height 중간에 — 글자를 가로질러야
                            // 취소선으로 읽힌다. 트레일링 스페이스까지 이어 그려
                            // 여러 낱말이 한 줄로 지워진다.
                            let sy = pen_y + size * 0.6;
                            let sw = ww + if trailing_space { space_w } else { 0.0 };
                            self.rect(pen_x, sy, sw, (size * 0.06).max(1.0), color);
                        }
                        if let Some(dest) = &span.link {
                            // Underline just below the glyph baseline (size-based,
                            // not the inflated line height) so it tracks the text.
                            // Span the trailing space so a multi-word link reads
                            // as one continuous underline, not one per word.
                            let uy = pen_y + size * 0.92;
                            let uw = ww + if trailing_space { space_w } else { 0.0 };
                            self.rect(pen_x, uy, uw, (size * 0.06).max(1.0), link_color(dest));
                            self.md_link_rects
                                .push((pen_x, pen_y, uw, lh, dest.clone()));
                        }
                    }
                    pen_x += ww;
                    // 이 낱말의 칩이 뒤따르는 공백까지 덮었을 때만 다음 낱말과
                    // 이음매가 생긴다. 코드 스팬 안에서 trailing_space 가 참이면
                    // 뒤에 같은 스팬의 낱말이 반드시 더 있다(split_inclusive 는
                    // 마지막 조각에만 공백을 안 남긴다) — 이게 예측 없는 예측이다.
                    code_joint = span.code && trailing_space;
                }
                if trailing_space {
                    pen_x += space_w;
                }
            }
        }
        pen_y + lh
    }

    /// Copy-button: rounded chip background (chrome layer) + Lucide copy SVG
    /// (icon layer, on top), sized to ICON_SIZE so it matches every other
    /// chrome icon. All logical px.
    fn draw_copy_icon(&mut self, bx: f32, by: f32, bw: f32, bh: f32) {
        let bg = crate::theme::with_alpha(crate::theme::surface_active(), 0xE0);
        self.round_rect_fill(bx, by, bw, bh, crate::theme::radius_sm(), bg);
        let isz = crate::theme::ICON_SIZE;
        self.queue_icon(
            "copy",
            bx + (bw - isz) / 2.0,
            by + (bh - isz) / 2.0,
            isz,
            crate::theme::text_dim(),
        );
    }

    /// Height (logical px) `md_runs` needs to wrap `spans` into `max_w`. Runs
    /// the real wrap with a clip range that makes every line invisible, so the
    /// measurement can never drift from what the draw pass lays out (table rows
    /// need the row height before they can place the row box).
    ///
    /// `x_start` must be the same one the draw pass will use: the wrap test is
    /// `pen_x + word > x_start + max_w`, and when a cell's text lands exactly on
    /// its column edge, measuring at x=0 and drawing at x=1050 disagree on that
    /// comparison — f32 drops the low bits of the sum once the offset is large.
    /// That mismatch showed up as a row twice as tall as the line inside it.
    fn md_runs_height(
        &mut self,
        spans: &[crate::MdSpan],
        x_start: f32,
        max_w: f32,
        size: f32,
        force_bold: bool,
    ) -> f32 {
        self.md_runs(
            spans,
            x_start,
            0.0,
            max_w,
            size,
            force_bold,
            crate::theme::text(),
            f32::MAX,
            f32::MIN,
        )
    }

    /// Unwrapped width (logical px) of a table cell's spans — the natural width
    /// its column wants before any shrink.
    fn md_cell_width(&mut self, cell: &[crate::MdSpan], size: f32, force_bold: bool) -> f32 {
        let mut w = 0.0;
        for sp in cell {
            w += self.measure_run(
                &sp.text,
                size,
                sp.bold || force_bold,
                sp.italic,
                sp.code,
                sp.code,
            );
        }
        w
    }

    /// Narrowest a table cell can get before its column starts overlapping the
    /// next one: the widest single word. `md_runs` only breaks on spaces, so a
    /// column squeezed below this can't wrap — it just spills.
    fn md_cell_min_width(&mut self, cell: &[crate::MdSpan], size: f32, force_bold: bool) -> f32 {
        let mut m: f32 = 0.0;
        for sp in cell {
            for word in sp.text.split_whitespace() {
                m = m.max(self.measure_run(
                    word,
                    size,
                    sp.bold || force_bold,
                    sp.italic,
                    sp.code,
                    sp.code,
                ));
            }
        }
        m
    }

    /// 목록 표식을 본문 시작선 왼쪽에 **오른쪽 맞춤**으로 그린다.
    ///
    /// 왼쪽 맞춤으로 그리면 표식마다 폭이 달라 결과가 어긋난다 — 점 하나는
    /// 여백이 넉넉한데 `1.` 은 본문 시작선까지 밀려 숫자와 글자가 붙고,
    /// `10.` 은 글자를 덮는다. 오른쪽 끝을 고정하면 표식이 몇 칸이든 본문과의
    /// 간격이 같다.
    ///
    /// 마크다운 셰이퍼로 그리는 이유는 `draw_text` 가 터미널 폰트라, 본문 바로
    /// 옆에서 서체가 튀기 때문이다(Meta 블록이 같은 함정을 적어 두고 있다).
    fn md_list_marker(
        &mut self,
        marker: &str,
        body_x: f32,
        left: f32,
        pen_y: f32,
        size: f32,
        col: [u8; 4],
        clip_top: f32,
        clip_bot: f32,
    ) {
        let cell = [crate::MdSpan {
            text: marker.to_string(),
            bold: false,
            italic: false,
            code: false,
            strike: false,
            link: None,
        }];
        let mw = self.md_cell_width(&cell, size, false);
        // 들여쓰기가 표식보다 좁으면 왼쪽 여백에 붙인다. 음수로 나가면 pane
        // 밖에서 잘려 표식이 통째로 사라진다.
        let mx = (body_x - size * 0.4 - mw).max(left);
        self.md_runs(&cell, mx, pen_y, mw + 1.0, size, false, col, clip_top, clip_bot);
    }

    /// Lay out + draw a markdown document into the pane box (all logical px).
    /// Glyphs/rects go into the chrome buffer (drawn over the empty cell pass,
    /// under pane headers). Returns total content height (logical) so the
    /// caller can clamp the scroll offset.
    pub fn draw_markdown(
        &mut self,
        blocks: &[crate::MdBlock],
        doc_gen: u64,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        scroll: f32,
        // 선택 범위, **문서 좌표**(ax, ay, bx, by). 화면 좌표로 받으면 선택 중
        // 스크롤할 때 범위가 손가락을 따라 흘러간다 — 여기서 스크롤을 빼 화면
        // 좌표로 바꿔 둔다.
        sel_doc: Option<(f32, f32, f32, f32)>,
    ) -> f32 {
        use crate::MdBlock;
        // Link / copy-button rects are rebuilt from scratch each frame so
        // they track the current scroll offset; main.rs hit-tests clicks.
        self.md_link_rects.clear();
        self.md_copy_rects.clear();
        self.md_word_rects.clear();
        // 문서 좌표 = 화면 좌표 + scroll (본문 박스 오프셋은 낱말 사각형과 선택에
        // 똑같이 들어가므로 비교에서 상쇄된다 — 뺄 필요가 없다).
        self.md_sel_screen = sel_doc.map(|(ax, ay, bx, by)| (ax, ay - scroll, bx, by - scroll));
        self.md_block_ys.clear();
        self.md_block_ys.reserve(blocks.len());
        let base = self.font_size_px as f32 / self.scale;
        // Notion-style reading column: generous side padding, capped content
        // width, centered in the pane. Shadow x/w so the block code below lays
        // out into the column without per-line changes. Clipping still uses the
        // full pane box (y/h).
        // 표식이 오른쪽 맞춤으로 왼쪽으로 넘칠 때의 한계선. 읽기 열 왼쪽에서
        // 멈추게 하면 `100.` 같은 세 자리 표식이 본문 글자를 덮는다 — 브라우저가
        // `<ol>` 을 그리는 방식대로, 넘치는 만큼 열 바깥 여백을 쓰게 둔다. 본문
        // 시작선은 건드리지 않으므로 한 목록 안의 글줄 맞춤이 흔들리지 않는다.
        let marker_floor = x + base * 0.3;
        let side_pad = base * 1.7;
        let avail = (w - side_pad * 2.0).max(1.0);
        let cw = avail.min(base * 46.0);
        // 읽기 열은 좁지만 **클리핑은 pane 상자 전체**다. 표식이 왼쪽으로 넘치는 것도
        // 스크롤바가 오른쪽 여백에 서는 것도 열 밖이지만 pane 안이라, 열로 자르면
        // 멀쩡한 것들이 사라진다.
        let (pane_x, pane_w) = (x, w);
        let x = x + side_pad + (avail - cw) * 0.5;
        let w = cw;
        let clip_top = y;
        let clip_bot = y + h;
        // 지금까지 이 뷰의 잘라내기는 블록·줄 단위 「완전히 밖이면 건너뛴다」뿐이었다.
        // 그래서 경계에 반쯤 걸친 것은 통째로 그려져 pane 밖으로 샜다 — 스크롤 1100px
        // 에서 상자 top 에 걸친 본문 한 줄이 글자·인라인코드 배경째 헤더 위에 그려지는
        // 것을 확인했다(8863px). 표는 `by0 = pen_y.max(clip_top)` 로 손으로 잘라 둬서
        // 안 샜는데, 그런 자리는 그 하나만 막을 뿐이라 30여 곳에 같은 짓을 반복해야 한다.
        //
        // 컬링(`clip_top`/`clip_bot` 검사)은 그대로 남긴다 — 문단이 수천 줄일 수 있고,
        // 그걸 다 그리면 인스턴스가 그만큼 늘어난다. 시저는 그 위에 얹혀 삐져나온
        // 픽셀만 자른다.
        self.push_clip(pane_x, y, pane_w, h.max(0.0));
        let top0 = y - scroll;
        let mut pen_y = top0 + base * 1.1;
        // 지난 프레임에 잰 블록 높이. 스크롤은 레이아웃을 바꾸지 않으므로
        // (문서·폭·글자크기·dpi 가 같으면) 그대로 쓸 수 있고, 화면 밖 블록은
        // 재지 않고 높이만큼 건너뛴다. 큰 문서에선 이 스캔이 마크다운 그리기
        // 시간의 절반이었다(4399줄 3.1ms 중 1.6ms — 보이는 양은 110줄 문서와
        // 똑같은데도).
        // 꺼내 들고 가는 이유는 self 를 다시 빌려야 해서다.
        let key = (doc_gen, w.to_bits(), base.to_bits(), self.scale.to_bits());
        let mut heights = match self.md_heights.iter().position(|e| e.key == key) {
            Some(i) => self.md_heights.remove(i).h,
            None => Vec::new(),
        };
        // 블록 수가 다르면(같은 세대에 있을 수 없지만) 인덱스가 어긋나므로 버린다.
        if heights.len() != blocks.len() {
            heights.clear();
            heights.resize(blocks.len(), f32::NAN);
        }
        for (bi, block) in blocks.iter().enumerate() {
            // 이 블록이 문서 어디쯤에 놓였는지(스크롤 뺀 좌표) 적어 둔다. 레이아웃
            // 은 여기서만 계산되므로, 모드 토글이 쓸 위치는 실제 그린 값이어야
            // 한다 — 따로 추정하면 헤딩 간격·이미지 높이에서 어긋난다.
            self.md_block_ys.push(pen_y - top0);
            let block_y0 = pen_y;
            // 화면 밖이고 높이를 이미 아는 블록은 통째로 건너뛴다. 링크·복사
            // 버튼 rect 는 원래 보이는 것만 등록되므로(md_runs 의 clip 검사 안,
            // 코드블록은 `if visible`) 건너뛰어도 히트 영역이 어긋나지 않는다.
            let known = heights[bi];
            // 알림의 첫 조각은 건너뛰지 않는다 — 상자 배경을 이 조각이 통째로
            // 그리므로, 조각이 화면 위로 벗어난 순간 나머지 문단의 배경까지 사라진다.
            let draws_for_others = matches!(block, MdBlock::Callout { first: true, .. });
            if !draws_for_others && known.is_finite() && (pen_y + known < clip_top || pen_y > clip_bot)
            {
                pen_y += known;
                continue;
            }
            match block {
                MdBlock::Heading { level, spans } => {
                    let scale_f = match level {
                        1 => 1.9,
                        2 => 1.5,
                        3 => 1.25,
                        4 => 1.1,
                        _ => 1.0,
                    };
                    let size = base * scale_f;
                    // Notion: big space above a heading, tight below so it binds
                    // to the text it introduces.
                    pen_y += if *level <= 1 { base * 1.6 } else { base * 1.2 };
                    pen_y = self.md_runs(
                        spans, x, pen_y, w, size, true, crate::theme::text(), clip_top, clip_bot,
                    );
                    pen_y += base * 0.35;
                }
                MdBlock::Para { spans } => {
                    let size = base;
                    pen_y = self.md_runs(
                        spans, x, pen_y, w, size, false, crate::theme::text(), clip_top, clip_bot,
                    );
                    pen_y += base * 0.85;
                }
                MdBlock::Code { code, lang } => {
                    let size = base * 0.9;
                    let lh =
                        (self.md_shaper.line_height(size * self.scale).ceil() / self.scale) * 1.35;
                    let pad = base * 0.85;
                    // 위 여백만 넓다 — 복사 버튼·언어 라벨이 첫 코드 줄과 같은
                    // 높이에 떠 있어서, 첫 줄이 길면 글자가 버튼 밑을 지나갔다.
                    // 겹침을 z 순서로 덮는 대신 자리부터 갈라 둔다.
                    let pad_top = base * 1.8;
                    let inner_w = (w - pad * 2.0).max(base);
                    let lines: Vec<&str> = code.trim_end_matches('\n').split('\n').collect();
                    let cell = self.mono_advance(' ', size);
                    // 논리 줄 하나를 상자 폭에 맞는 시각 줄들로 접는다: (줄 번호,
                    // 들여쓰기, 문자 범위). 이 렌더러엔 scissor 가 없어 넘친 코드가
                    // 상자·읽기 열·스크롤바까지 밟고 지나갔다. 가로 스크롤은 블록마다
                    // 상태가 필요하니 읽기 뷰에선 노션처럼 접는 쪽이 맞다.
                    let mut plans: Vec<(usize, f32, usize, usize)> = Vec::new();
                    for (li, line) in lines.iter().enumerate() {
                        let chs: Vec<char> = line.chars().collect();
                        // 대부분의 줄은 여유롭게 들어간다 — 그 판정을 셀 폭 산술로
                        // 끝내 글자별 아틀라스 조회는 경계에 걸린 줄만 물린다.
                        // 1.15 는 안전 쪽 여유값(넉넉히 들어갈 때만 건너뛴다).
                        let bound = chs
                            .iter()
                            .map(|c| if is_wide_char(*c) { 2.0 } else { 1.0 })
                            .sum::<f32>()
                            * cell;
                        if chs.is_empty() || bound * 1.15 <= inner_w {
                            plans.push((li, 0.0, 0, chs.len()));
                            continue;
                        }
                        // 접힌 줄은 원래 줄 들여쓰기에 한 칸 더 물려 이어짐을 보인다.
                        let lead = chs.iter().take_while(|c| **c == ' ' || **c == '\t').count();
                        let cont = ((lead as f32 + 2.0) * cell).min(inner_w * 0.5);
                        let mut i = 0usize;
                        let mut first = true;
                        while i < chs.len() {
                            let ox = if first { 0.0 } else { cont };
                            let avail = inner_w - ox;
                            let mut pen = 0.0;
                            let mut j = i;
                            let mut brk: Option<usize> = None;
                            while j < chs.len() {
                                let a = self.mono_advance(chs[j], size);
                                if pen + a > avail && j > i {
                                    break;
                                }
                                if chs[j] == ' ' && j > i {
                                    brk = Some(j + 1);
                                }
                                pen += a;
                                j += 1;
                            }
                            // 낱말 경계가 있으면 거기서 끊는다(공백은 앞 줄이 먹는다).
                            let cut = if j < chs.len() {
                                brk.filter(|c| *c > i).unwrap_or(j)
                            } else {
                                j
                            };
                            plans.push((li, ox, i, cut));
                            i = cut;
                            first = false;
                        }
                    }
                    let block_h = plans.len() as f32 * lh + pad_top + pad;
                    let block_top = pen_y;
                    let visible = pen_y + block_h > clip_top && pen_y < clip_bot;
                    if visible {
                        self.round_rect_fill(x, pen_y, w, block_h, base * 0.5, crate::theme::surface());
                    }
                    let clip_r = x + w - pad * 0.4;
                    let mut ly = pen_y + pad_top;
                    // 같은 논리 줄이 여러 시각 줄로 접히니 하이라이트는 줄이 바뀔
                    // 때만 다시 돈다.
                    let mut hl: Option<(usize, Vec<(char, [u8; 4])>)> = None;
                    for (li, ox, from, to) in &plans {
                        if ly + lh > clip_top && ly < clip_bot {
                            if hl.as_ref().map(|(i, _)| i != li).unwrap_or(true) {
                                let mut cells: Vec<(char, [u8; 4])> = Vec::new();
                                for (tok, col) in
                                    highlight_code_line(lines[*li], lang, crate::theme::text_dim())
                                {
                                    cells.extend(tok.chars().map(|c| (c, col)));
                                }
                                hl = Some((*li, cells));
                            }
                            let cells = &hl.as_ref().unwrap().1;
                            let mut tx = x + pad + ox;
                            let mut k = *from;
                            let end = (*to).min(cells.len());
                            while k < end {
                                // 색이 같은 이웃 글자는 한 번에 — draw_text 는 글자마다
                                // 펜을 이어 주니 나눠 그려도 자리는 같다.
                                let col = cells[k].1;
                                let mut run = String::new();
                                while k < end && cells[k].1 == col {
                                    run.push(cells[k].0);
                                    k += 1;
                                }
                                tx = self.draw_text_clipped(
                                    tx,
                                    ly,
                                    &run,
                                    DrawOpts {
                                        font_size: size,
                                        color: col,
                                        bold: false,
                                        italic: false,
                                    },
                                    f32::NEG_INFINITY,
                                    clip_r,
                                );
                            }
                        }
                        ly += lh;
                    }
                    if visible {
                        // Copy button, top-right; language label to its left.
                        let btn = base * 1.5;
                        let by = block_top + base * 0.35;
                        let bx = x + w - btn - base * 0.35;
                        self.draw_copy_icon(bx, by, btn, btn * 0.78);
                        self.md_copy_rects
                            .push((bx, by, btn, btn * 0.78, code.clone()));
                        if !lang.is_empty() {
                            // 라벨은 draw_text(모노)로 그리는데 md 셰이퍼로 재면 폭이
                            // 6할로 나와 복사 버튼 밑으로 파고들었다 — 그릴 때와 같은
                            // 자로 잰다.
                            let lsize = size * 0.82;
                            let lw = self.measure_chrome_text(lang, lsize, false);
                            self.draw_text(
                                bx - lw - base * 0.5,
                                by + base * 0.05,
                                lang,
                                DrawOpts {
                                    font_size: lsize,
                                    color: crate::theme::text_mute(),
                                    bold: false,
                                    italic: false,
                                },
                            );
                        }
                    }
                    pen_y += block_h + base * 0.85;
                }
                MdBlock::ListItem { depth, marker, spans, task } => {
                    let size = base;
                    let lh = self.md_shaper.line_height(size * self.scale).ceil() / self.scale;
                    let indent = (*depth as f32 + 1.0) * base * 1.5;
                    if pen_y + lh > clip_top && pen_y < clip_bot {
                        match task {
                            // 체크박스는 글리프가 아니라 아이콘으로 그린다 — ☐/☑ 는
                            // 폰트에 있을 때만 나와서 기기마다 다르게 보인다.
                            Some(checked) => {
                                let isz = size * 0.95;
                                self.queue_icon(
                                    if *checked { "square-check" } else { "square" },
                                    x + indent - base * 1.35,
                                    pen_y + (lh - isz) / 2.0,
                                    isz,
                                    if *checked {
                                        crate::theme::accent()
                                    } else {
                                        crate::theme::text_dim()
                                    },
                                );
                            }
                            None => self.md_list_marker(
                                marker,
                                x + indent,
                                marker_floor,
                                pen_y,
                                size,
                                crate::theme::accent(),
                                clip_top,
                                clip_bot,
                            ),
                        }
                    }
                    // 끝낸 할 일은 노션처럼 본문까지 흐려진다 — 체크박스만 바뀌면
                    // 목록을 훑을 때 남은 일과 끝난 일이 같은 무게로 읽힌다.
                    let body = match task {
                        Some(true) => crate::theme::text_dim(),
                        _ => crate::theme::text(),
                    };
                    pen_y = self.md_runs(
                        spans,
                        x + indent,
                        pen_y,
                        (w - indent).max(1.0),
                        size,
                        false,
                        body,
                        clip_top,
                        clip_bot,
                    );
                    pen_y += base * 0.4;
                }
                MdBlock::Quote { spans } => {
                    let size = base;
                    let indent = base * 1.3;
                    let start_y = pen_y;
                    pen_y = self.md_runs(
                        spans,
                        x + indent,
                        pen_y,
                        (w - indent).max(1.0),
                        size,
                        false,
                        crate::theme::text_dim(),
                        clip_top,
                        clip_bot,
                    );
                    let bar_h = pen_y - start_y;
                    if start_y + bar_h > clip_top && start_y < clip_bot {
                        self.rect(x, start_y, base * 0.22, bar_h, crate::theme::accent());
                    }
                    pen_y += base * 0.8;
                }
                MdBlock::Callout { kind, spans, first, last, list } => {
                    let (icon_name, title, col) = kind.face();
                    let size = base;
                    let pad = base * 0.8;
                    let bar_w = base * 0.22;
                    let text_x = x + bar_w + pad;
                    let text_w = (w - bar_w - pad * 2.0).max(1.0);
                    // 상자 안 목록은 바깥 목록과 같은 들여쓰기·간격을 쓴다 — 알림에
                    // 들어갔다고 목록 모양이 달라지면 같은 글이 다르게 읽힌다.
                    let indent_of = |l: &Option<(u8, String)>| {
                        l.as_ref().map_or(0.0, |(d, _)| (*d as f32 + 1.0) * base * 1.5)
                    };
                    // 조각 뒤 간격. 문단 사이는 바깥 문단 간격(0.85)보다 좁혀야 상자
                    // 안이 한 덩어리로 읽힌다.
                    let after_of =
                        |l: &Option<(u8, String)>| if l.is_some() { base * 0.4 } else { base * 0.55 };
                    let indent = indent_of(list);
                    let body_x = text_x + indent;
                    let body_w = (text_w - indent).max(1.0);
                    if *first {
                        // 표지 제목은 본문과 같은 경로(md_runs)로 그린다 — draw_text 는
                        // 터미널 등폭 폰트라 상자 머리만 서체가 튄다(Meta 블록이 겪은
                        // 것과 같은 함정).
                        let title_size = size * 0.95;
                        let isz = size * 1.05;
                        let icon_col = isz + base * 0.45;
                        let title_spans = [crate::MdSpan {
                            text: title.to_string(),
                            bold: true,
                            italic: false,
                            code: false,
                            strike: false,
                            link: None,
                        }];
                        let title_w = (text_w - icon_col).max(1.0);
                        let head_h = self.md_runs_height(
                            &title_spans,
                            text_x + icon_col,
                            title_w,
                            title_size,
                            true,
                        ) + base * 0.25;
                        // 상자는 첫 조각이 통째로 그린다. 조각마다 그리면 이음새에서
                        // 배경이 두 번 겹쳐 그 띠만 색이 진해지고, 둥근 모서리가
                        // 상자 중간에 생긴다.
                        let mut box_h =
                            pad + head_h + self.md_runs_height(spans, body_x, body_w, size, false);
                        let mut next_gap = after_of(list);
                        for nb in &blocks[bi + 1..] {
                            match nb {
                                MdBlock::Callout { spans: s2, list: l2, first: false, .. } => {
                                    let ind = indent_of(l2);
                                    box_h += next_gap
                                        + self.md_runs_height(
                                            s2,
                                            text_x + ind,
                                            (text_w - ind).max(1.0),
                                            size,
                                            false,
                                        );
                                    next_gap = after_of(l2);
                                }
                                _ => break,
                            }
                        }
                        box_h += pad;
                        if pen_y + box_h > clip_top && pen_y < clip_bot {
                            let mut tint = col;
                            // 배경은 종류색을 옅게 깐다 — 코드블록의 surface() 배경과
                            // 색으로 갈려야 무엇이 코드고 무엇이 알림인지 읽힌다.
                            tint[3] = 30;
                            self.round_rect_fill(x, pen_y, w, box_h, base * 0.5, tint);
                            self.rect(x, pen_y, bar_w, box_h, col);
                        }
                        pen_y += pad;
                        if pen_y + head_h > clip_top && pen_y < clip_bot {
                            // 아이콘은 제목 글줄의 세로 중앙에 놓는다 — 위쪽에 맞추면
                            // 한글 제목처럼 글줄이 높은 경우 표지가 떠 보인다.
                            let lh = self.md_shaper.line_height(title_size * self.scale).ceil()
                                / self.scale;
                            self.queue_icon(icon_name, text_x, pen_y + (lh - isz) / 2.0, isz, col);
                            self.md_runs(
                                &title_spans,
                                text_x + icon_col,
                                pen_y,
                                title_w,
                                title_size,
                                true,
                                col,
                                clip_top,
                                clip_bot,
                            );
                        }
                        pen_y += head_h;
                    }
                    if let Some((_, marker)) = list {
                        self.md_list_marker(
                            // 상자 안에서도 표식이 왼쪽으로 넘칠 수 있다. 세로 막대
                            // 바로 뒤까지만 허용한다 — 그보다 왼쪽은 상자 밖이다.
                            marker, body_x, x + bar_w + base * 0.15, pen_y, size, col, clip_top,
                            clip_bot,
                        );
                    }
                    pen_y = self.md_runs(
                        spans,
                        body_x,
                        pen_y,
                        body_w,
                        size,
                        false,
                        crate::theme::text(),
                        clip_top,
                        clip_bot,
                    );
                    pen_y += if *last { pad + base * 0.85 } else { after_of(list) };
                }
                MdBlock::Rule => {
                    pen_y += base * 0.9;
                    if pen_y > clip_top && pen_y < clip_bot {
                        self.rect(x, pen_y, w, 1.0, crate::theme::border());
                    }
                    pen_y += base * 0.9;
                }
                MdBlock::Meta { rows } => {
                    // 노션 속성 영역: 본문보다 작고 흐린 라벨 열 + 값 열, 아래에 얇은
                    // 경계선. 본문과 같은 경로(md_runs)로 그린다 — draw_text 는 터미널
                    // 폰트라 등폭 폭으로 열을 잡게 되고, 그러면 값이 라벨 위에 겹쳐
                    // 그려진다(실측: `metadata.node_ty` 에 `memory` 가 포개졌다).
                    let size = base * 0.82;
                    let mk = |t: &str| crate::MdSpan {
                        text: t.to_string(),
                        bold: false,
                        italic: false,
                        code: false,
                        strike: false,
                        link: None,
                    };
                    let label_w = rows
                        .iter()
                        .map(|(k, _)| self.measure_run(k, size, false, false, false, false))
                        .fold(0.0_f32, f32::max)
                        + base * 1.2;
                    for (k, v) in rows {
                        let label = [mk(k)];
                        let value = [mk(v)];
                        let y0 = pen_y;
                        self.md_runs(
                            &label,
                            x,
                            y0,
                            label_w,
                            size,
                            false,
                            crate::theme::text_dim(),
                            clip_top,
                            clip_bot,
                        );
                        // 값은 남은 폭 안에서 접힌다 — 긴 description 을 잘라 버리면
                        // 속성 영역이 정보를 잃는다.
                        pen_y = self.md_runs(
                            &value,
                            x + label_w,
                            y0,
                            (w - label_w).max(1.0),
                            size,
                            false,
                            crate::theme::text(),
                            clip_top,
                            clip_bot,
                        );
                    }
                    pen_y += base * 0.55;
                    if pen_y > clip_top && pen_y < clip_bot {
                        self.rect(x, pen_y, w, 1.0, crate::theme::border());
                    }
                    pen_y += base * 0.9;
                }
                MdBlock::Image { key, alt, w: iw_px, h: ih_px, .. } => {
                    if *iw_px > 0 && *ih_px > 0 && !key.is_empty() {
                        let iw = *iw_px as f32;
                        let ih = *ih_px as f32;
                        // Fit to the content column width, never upscaling past
                        // the image's own logical size. Keep aspect.
                        let disp_w = w.min(iw / self.scale);
                        let disp_h = disp_w * ih / iw;
                        if pen_y + disp_h > clip_top && pen_y < clip_bot {
                            self.queue_image(key, x, pen_y, disp_w, disp_h, 1.0, 0.0, 0.0);
                        }
                        pen_y += disp_h + base * 0.7;
                    } else {
                        // Decode failed / remote URL — show the alt text dimmed.
                        let lh = (self.md_shaper.line_height(base * self.scale).ceil()
                            / self.scale)
                            * 1.4;
                        if pen_y + lh > clip_top && pen_y < clip_bot {
                            self.md_draw_word(
                                &format!("[이미지: {alt}]"),
                                x,
                                pen_y,
                                base,
                                crate::theme::text_mute(),
                                false,
                                true,
                                1,
                                false,
                            );
                        }
                        pen_y += lh + base * 0.4;
                    }
                }
                MdBlock::Table { head, rows, align } => {
                    let ncols = head.len().max(rows.iter().map(|r| r.len()).max().unwrap_or(0));
                    if ncols == 0 {
                        continue;
                    }
                    let size = base * 0.92;
                    let pad_x = base * 0.6;
                    let pad_y = base * 0.4;
                    // Column widths: each column wants its widest cell, and can
                    // give back down to its widest *word*.
                    let mut colw = vec![0.0f32; ncols];
                    let mut colmin = vec![0.0f32; ncols];
                    for (ci, cell) in head.iter().enumerate().take(ncols) {
                        colw[ci] = colw[ci].max(self.md_cell_width(cell, size, true));
                        colmin[ci] = colmin[ci].max(self.md_cell_min_width(cell, size, true));
                    }
                    for row in rows {
                        for (ci, cell) in row.iter().enumerate().take(ncols) {
                            colw[ci] = colw[ci].max(self.md_cell_width(cell, size, false));
                            colmin[ci] = colmin[ci].max(self.md_cell_min_width(cell, size, false));
                        }
                    }
                    for c in colw.iter_mut().chain(colmin.iter_mut()) {
                        *c += pad_x * 2.0;
                    }
                    // Overflow: take the excess out of the columns that have
                    // slack, proportional to how much each has. A column whose
                    // content is one long token (`anchor_cache`) keeps its width
                    // and the prose column next to it wraps instead — an even
                    // shrink would squeeze both and the token would spill into
                    // its neighbour.
                    let total: f32 = colw.iter().sum();
                    if total > w {
                        let min_total: f32 = colmin.iter().sum();
                        let slack = total - min_total;
                        if slack > 0.0 && min_total < w {
                            let k = (total - w) / slack;
                            for (c, m) in colw.iter_mut().zip(colmin.iter()) {
                                *c -= (*c - *m) * k;
                            }
                        } else {
                            // Even the minimums don't fit — nothing to do but
                            // scale everything and accept the spill.
                            let k = w / total;
                            for c in colw.iter_mut() {
                                *c *= k;
                            }
                        }
                    }
                    let table_w: f32 = colw.iter().sum();
                    pen_y += base * 0.6;
                    let table_top = pen_y;
                    let empty: crate::MdCell = Vec::new();
                    // Per-row (pen origin, wrap width), shared by the measure and
                    // draw passes below.
                    let mut cellbox: Vec<(f32, f32)> = Vec::with_capacity(ncols);
                    let head_rows = if head.is_empty() { &[][..] } else { std::slice::from_ref(head) };
                    for (row, is_head) in head_rows
                        .iter()
                        .map(|r| (r, true))
                        .chain(rows.iter().map(|r| (r, false)))
                    {
                        // Pass 1: pin every cell's pen origin + wrap width, and
                        // take the row height from those exact numbers. Pass 2
                        // draws from the same list so the two can't diverge.
                        cellbox.clear();
                        let mut row_h: f32 = 0.0;
                        let mut cx = x;
                        for ci in 0..ncols {
                            let cell = row.get(ci).unwrap_or(&empty);
                            let inner = (colw[ci] - pad_x * 2.0).max(base);
                            // Alignment only bites when the cell fits on one
                            // line; a wrapped cell has no single width to align
                            // against, so it stays left.
                            let nat = self.md_cell_width(cell, size, is_head);
                            let off = if nat < inner {
                                match align.get(ci) {
                                    Some(crate::MdAlign::Center) => (inner - nat) * 0.5,
                                    Some(crate::MdAlign::Right) => inner - nat,
                                    _ => 0.0,
                                }
                            } else {
                                0.0
                            };
                            let tx = cx + pad_x + off;
                            row_h = row_h.max(self.md_runs_height(cell, tx, inner, size, is_head));
                            cellbox.push((tx, inner));
                            cx += colw[ci];
                        }
                        let row_h = row_h + pad_y * 2.0;
                        if pen_y + row_h > clip_top && pen_y < clip_bot {
                            if is_head {
                                let by0 = pen_y.max(clip_top);
                                let by1 = (pen_y + row_h).min(clip_bot);
                                // A hair *lighter* than bg so the header band
                                // reads as raised; SURFACE is near-black here and
                                // made the table top-heavy.
                                self.rect(x, by0, table_w, by1 - by0, crate::theme::surface_hover());
                            }
                            let col = if is_head {
                                crate::theme::text()
                            } else {
                                crate::theme::text_dim()
                            };
                            for (ci, (tx, inner)) in cellbox.iter().enumerate() {
                                let cell = row.get(ci).unwrap_or(&empty);
                                self.md_runs(
                                    cell,
                                    *tx,
                                    pen_y + pad_y,
                                    *inner,
                                    size,
                                    is_head,
                                    col,
                                    clip_top,
                                    clip_bot,
                                );
                            }
                            self.rect(x, pen_y + row_h, table_w, 1.0, crate::theme::border());
                        }
                        pen_y += row_h;
                    }
                    // Column rules + the top hairline, clamped to the scroll clip
                    // (this renderer has no scissor, so a tall table would
                    // otherwise bleed past the pane box).
                    let vy0 = table_top.max(clip_top);
                    let vy1 = pen_y.min(clip_bot);
                    if vy1 > vy0 {
                        let mut vx = x;
                        for c in colw.iter().take(ncols - 1) {
                            vx += c;
                            self.rect(vx, vy0, 1.0, vy1 - vy0, crate::theme::border());
                        }
                        if table_top >= clip_top {
                            self.rect(x, table_top, table_w, 1.0, crate::theme::border());
                        }
                    }
                    pen_y += base * 0.9;
                }
            }
            // 방금 그리며 실제로 잰 높이 — 다음 프레임에 이 블록이 화면 밖으로
            // 밀려나면 이 값으로 건너뛴다.
            heights[bi] = pen_y - block_y0;
        }
        // 최근 문서 몇 개만 들고 있는다(raw_hl 과 같은 꼬마 LRU) — 마크다운
        // pane 이 둘이어도 서로 쫓아내지 않을 만큼.
        self.md_heights.insert(0, MdHeightEntry { key, h: heights });
        self.md_heights.truncate(4);
        let content_h = (pen_y - top0).max(0.0);
        // 문서 안 어디쯤인지 — 긴 메모리 파일을 굴리다 보면 위치 감각이 통째로
        // 없다. macOS 오버레이 스타일로 트랙 없이 엄지만, 읽기 칼럼 밖(pane 오른쪽
        // 여백)에 둔다. 여기서 그리는 이유는 총 높이가 방금 잰 값이라서다 — 앞에서
        // 그리면 한 프레임 전 높이를 써야 한다.
        if content_h > h + 1.0 {
            let track_h = (h - 8.0).max(1.0);
            let th = (h / content_h * track_h).max(24.0);
            let t = (scroll / (content_h - h)).clamp(0.0, 1.0);
            let mut col = crate::theme::text();
            col[3] = 45;
            // 본문 박스 오른쪽이 곧 글줄 오른쪽이라, 박스 안에 두면 막대가 글에
            // 달라붙는다. pane 안쪽 여백(PANE_INNER_X) 쪽으로 밀어 글과 떨어뜨린다
            // — 그 여백은 본문 박스 밖이지만 여전히 pane 안이다.
            self.round_rect_fill(
                x + w + 1.0,
                y + 4.0 + (track_h - th) * t,
                3.0,
                th,
                1.5,
                col,
            );
        }
        // 히트렉트를 pane 상자와 교집합 낸다. 시저는 픽셀만 자르지 클릭은 안 자르므로,
        // 경계에 걸친 낱말·링크의 **잘려 안 보이는 쪽**이 그대로 눌린다 — 마크다운 뷰는
        // pane 하나라, 그 위쪽은 pane 헤더거나 아예 다른 pane 이다.
        //
        // 쌓는 자리(낱말 둘·링크·복사)마다 거는 대신 여기 한 곳에서 몰아서 한다.
        // `mem::take` 로 잠깐 꺼내는 건 `retain_mut`(&mut self)와 `clip_hit`(&self)이
        // 같이 못 살아서다 — 교집합 계산을 손으로 베끼면 그게 클립과 갈린다.
        macro_rules! clip_flat {
            ($f:ident) => {{
                let mut v = std::mem::take(&mut self.$f);
                v.retain_mut(|e| match self.clip_hit((e.0, e.1, e.2, e.3)) {
                    Some((x, y, w, h)) => {
                        (e.0, e.1, e.2, e.3) = (x, y, w, h);
                        true
                    }
                    None => false,
                });
                self.$f = v;
            }};
        }
        clip_flat!(md_word_rects);
        clip_flat!(md_link_rects);
        clip_flat!(md_copy_rects);
        self.pop_clip();
        content_h
    }

    /// Draw the Raw markdown editor: source lines in the mono font + a cursor
    /// bar. All logical px; returns total content height for scroll clamping.
    /// Hit-test a click (logical px) inside a raw-editor body box to a caret
    /// (line, col). Mirrors `draw_raw_editor`'s metrics so the caret lands where
    /// the glyph the user clicked actually sits. `x`/`y` are the body box origin,
    /// `scroll`/`h_scroll` the editor's pan.
    pub fn raw_editor_caret_at(
        &mut self,
        lines: &[String],
        x: f32,
        y: f32,
        scroll: f32,
        h_scroll: f32,
        click_x: f32,
        click_y: f32,
        folds: &[(usize, usize)],
        wrap_cols: usize,
    ) -> (usize, usize) {
        let base = self.font_size_px as f32 / self.scale;
        let pad = base * 0.6;
        let lh = (self.shaper.line_height(base * self.scale).ceil() / self.scale) * 1.25;
        let digits = ((lines.len().max(1)) as f32).log10().floor() as usize + 1;
        let gutter_w = base * 0.62 * digits as f32 + base * 1.0;
        let cx0 = x + pad + gutter_w;
        let tx0 = cx0 - h_scroll;
        let top0 = (y - scroll) + pad;
        // 접힘·랩이 있으면 화면 행과 버퍼 줄이 갈린다 — 클릭은 **행**을 가리키므로
        // 줄과 그 행이 시작하는 열로 되돌린다(둘 다 없으면 항등이다).
        let rows = crate::markdown::layout_rows(lines, folds, wrap_cols);
        let row = ((click_y - top0) / lh).floor().max(0.0) as usize;
        let (line, from, upto) = crate::markdown::row_span(&rows, row, lines);
        // 격자라 클릭 x 는 실수 칸 위치로 바로 떨어진다 — 글자마다 아틀라스를
        // 조회하며 펜을 굴릴 필요가 없다. 칸에서 열로 되돌릴 때만 순회하는데,
        // 와이드 글자가 2칸이라 나눗셈 한 번으로는 안 되기 때문이다. 글자의
        // 절반을 넘어섰을 때 다음 열로 넘긴다(그 글자를 클릭한 것으로 본다).
        let want = (click_x - tx0) / self.cell_w;
        let mut acc = 0.0f32;
        let mut col = from;
        for ch in lines
            .get(line)
            .map_or("", |l| l.as_str())
            .chars()
            .skip(from)
            .take(upto - from)
        {
            let step = (1 + usize::from(is_wide_char(ch))) as f32;
            if want < acc + step * 0.5 {
                break;
            }
            acc += step;
            col += 1;
        }
        (line, col)
    }

    /// Raw-editor line box height in logical px — the one number
    /// `draw_raw_editor`, hit-testing and scroll math must all agree on.
    pub fn raw_editor_line_h(&mut self) -> f32 {
        let base = self.font_size_px as f32 / self.scale;
        (self.shaper.line_height(base * self.scale).ceil() / self.scale) * 1.25
    }

    /// Compute the scroll pan that keeps the caret visible inside a raw-editor
    /// body box of `w`×`h`. Mirrors `draw_raw_editor`'s metrics. `prefix` is
    /// the caret line's text up to the caret column. Returns the corrected
    /// (scroll, h_scroll); unchanged values mean the caret was already in view.
    pub fn raw_editor_ensure_visible(
        &mut self,
        line_count: usize,
        cur_line: usize,
        prefix: &str,
        w: f32,
        h: f32,
        scroll: f32,
        h_scroll: f32,
        folds: &[(usize, usize)],
        wrap_cols: usize,
        lines: &[String],
    ) -> (f32, f32) {
        let base = self.font_size_px as f32 / self.scale;
        let pad = base * 0.6;
        let lh = self.raw_editor_line_h();
        let digits = ((line_count.max(1)) as f32).log10().floor() as usize + 1;
        let gutter_w = base * 0.62 * digits as f32 + base * 1.0;
        // Vertical: line top on screen is y + pad + li*lh - scroll, so the box
        // stays fully visible while scroll ∈ [pad+(li+1)*lh - h, pad + li*lh].
        // 스크롤은 **화면 행** 기준이라 접힘·랩을 반영한 행 번호로 재야 한다.
        let rows = crate::markdown::layout_rows(lines, folds, wrap_cols);
        let cur_col = prefix.chars().count();
        let row = crate::markdown::row_of(&rows, cur_line, cur_col) as f32;
        let hi = pad + row * lh;
        let lo = (pad + (row + 1.0) * lh - h).max(0.0);
        let ns = scroll.clamp(lo, hi.max(lo));
        // 랩이 켜져 있으면 줄이 폭 안에서 접히므로 가로로 밀 곳이 없다 —
        // 여기서 0 으로 고정하지 않으면 옛 h_scroll 이 남아 본문이 잘려 보인다.
        if wrap_cols > 0 {
            return (ns, 0.0);
        }
        // Horizontal: the caret pen-x must stay inside the text viewport
        // (right of the gutter, left of the pane edge), with a small margin so
        // the next glyph is already visible while typing at the edge.
        let view_w = (w - pad * 2.0 - gutter_w).max(base);
        let margin = (base * 2.0).min(view_w * 0.25);
        let caret_x = cell_cols(prefix) as f32 * self.cell_w;
        let mut nh = h_scroll;
        if caret_x < nh + margin {
            nh = (caret_x - margin).max(0.0);
        } else if caret_x > nh + view_w - margin {
            nh = caret_x - view_w + margin;
        }
        (ns, nh)
    }

    /// Content-addressed lookup of tree-sitter spans for a raw-editor buffer.
    /// Returns `(spans, stale)`; None for unsupported or oversized files → the
    /// caller uses the line lexer for every line.
    ///
    /// **재파싱은 타이핑이 멈춘 뒤로 미룬다.** `tree-sitter-highlight` 에는
    /// 증분 API 가 없어 한 글자만 바뀌어도 문서를 통째로 다시 파싱하는데,
    /// 그 값이 5736줄에서 **1키당 20.3ms**(9줄은 0.84ms)로 프레임 예산
    /// 16.7ms 를 넘었다 — 키마다 화면을 1~2프레임 떨어뜨려 거노가 "반응이
    /// 0.3초 느리다"고 한 그것이다(실측). 연타 중에는 버퍼 해시가 매 키마다
    /// 바뀌므로 `raw_hl_pending` 이 계속 갱신되어 파싱이 한 번도 돌지 않고,
    /// 손이 멈추면 커서 blink 스레드가 깨우는 프레임에 실려 한 번만 돈다.
    ///
    /// 기다리는 동안엔 `stale=true` 로 직전 색을 그대로 쓴다. 폴백 후보를
    /// 줄 수가 같은 항목으로 제한하는 이유는 이 캐시가 pane id 를 안 들고
    /// 있어서다 — 편집기를 둘 띄워 두면 남의 스팬을 물어올 수 있다.
    fn raw_editor_ts_spans(
        &mut self,
        lines: &[String],
        lang: &str,
    ) -> Option<(
        std::rc::Rc<Vec<Vec<(String, crate::syntax::SynKind)>>>,
        bool,
    )> {
        crate::syntax::canon_lang(lang)?;
        let prof = crate::info::profiling().then(std::time::Instant::now);
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        lang.hash(&mut h);
        lines.len().hash(&mut h);
        for l in lines {
            l.hash(&mut h);
        }
        let hash = h.finish();
        let hash_us = prof.map(|t| t.elapsed().as_micros());
        if let Some(i) = self.raw_hl.iter().position(|e| e.hash == hash) {
            let e = self.raw_hl.remove(i);
            let spans = e.spans.clone();
            self.raw_hl.insert(0, e);
            self.raw_hl_pending = None;
            if let Some(us) = hash_us {
                eprintln!("[prof] ts_spans hit hash={us}us lines={}", lines.len());
            }
            return Some((spans, false));
        }
        let now = std::time::Instant::now();
        let quiet = self
            .raw_hl_cost
            .saturating_mul(RAW_HL_COST_MULT)
            .clamp(
                std::time::Duration::from_millis(RAW_HL_QUIET_MIN_MS),
                std::time::Duration::from_millis(RAW_HL_QUIET_MAX_MS),
            );
        let due = match self.raw_hl_pending {
            Some((h, since)) if h == hash => now.duration_since(since) >= quiet,
            _ => {
                self.raw_hl_pending = Some((hash, now));
                false
            }
        };
        if !due {
            // 첫 로드만은 기다리지 않는다 — 쓸 색이 아직 하나도 없는데
            // 무색 화면을 0.5초 보여줄 이유가 없다.
            if let Some(e) = self.raw_hl.iter().find(|e| e.len == lines.len()) {
                if let Some(us) = hash_us {
                    eprintln!("[prof] ts_spans defer hash={us}us lines={}", lines.len());
                }
                return Some((e.spans.clone(), true));
            }
        }
        self.raw_hl_pending = None;
        // 이 시각은 프로파일링과 무관하게 항상 잰다 — 다음 재파싱을 얼마나
        // 미룰지가 이 값에서 나오므로 계측이 곧 동작이다.
        let t_parse = std::time::Instant::now();
        let spans = std::rc::Rc::new(crate::syntax::highlight_lines(lang, lines)?);
        let cost = t_parse.elapsed();
        if self.raw_hl_parsed_once {
            self.raw_hl_cost = cost;
        } else {
            self.raw_hl_parsed_once = true;
        }
        if let Some(us) = hash_us {
            eprintln!(
                "[prof] ts_spans MISS hash={us}us parse={}us quiet={}ms lines={}",
                cost.as_micros(),
                quiet.as_millis(),
                lines.len()
            );
        }
        self.raw_hl.insert(
            0,
            RawHlEntry {
                hash,
                len: lines.len(),
                spans: spans.clone(),
            },
        );
        self.raw_hl.truncate(4);
        Some((spans, false))
    }

    /// 물결 밑줄. 이 렌더러엔 선분 프리미티브가 없어서 짧은 사각형을 위아래로
    /// 번갈아 놓아 톱니를 만든다 — 1px 단위라 눈에는 물결로 읽힌다. 직선이
    /// 아닌 이유는 편집기 밑줄이 preedit(직선)과 진단 둘 다 쓰기 때문이다.
    fn wavy_line(&mut self, x0: f32, x1: f32, y: f32, col: [u8; 4]) {
        const SEG: f32 = 2.0;
        const TH: f32 = 1.4;
        let mut px = x0;
        let mut up = true;
        while px < x1 {
            let w = SEG.min(x1 - px);
            self.rect(px, if up { y } else { y + TH }, w, TH, col);
            px += SEG;
            up = !up;
        }
    }

    /// 랩 폭(칸). 끄면 0. **본문 폭 계산이 여기 한 곳에만 있어야** 그리는 쪽과
    /// 클릭·스크롤 쪽이 다른 폭으로 줄을 접는 사고가 안 난다.
    pub fn raw_editor_wrap_cols(&mut self, w: f32, line_count: usize, wrap: bool) -> usize {
        if !wrap {
            return 0;
        }
        let base = self.font_size_px as f32 / self.scale;
        let pad = base * 0.6;
        let digits = ((line_count.max(1)) as f32).log10().floor() as usize + 1;
        let gutter_w = base * 0.62 * digits as f32 + base * 1.0;
        let body = w - pad - gutter_w;
        ((body / self.cell_w).floor() as usize).max(8)
    }

    /// 거터의 접기 삼각형을 눌렀는가 — 눌렀으면 그 **버퍼 줄**. 판정 기준이
    /// `draw_raw_editor` 와 갈리면 안 보이는 자리가 눌리므로 같은 수치를 쓴다.
    #[allow(clippy::too_many_arguments)]
    pub fn raw_editor_fold_hit(
        &mut self,
        lines: &[String],
        x: f32,
        y: f32,
        scroll: f32,
        click_x: f32,
        click_y: f32,
        folds: &[(usize, usize)],
    ) -> Option<usize> {
        let base = self.font_size_px as f32 / self.scale;
        let (pad, lh) = self.raw_editor_metrics();
        let digits = ((lines.len().max(1)) as f32).log10().floor() as usize + 1;
        let gutter_w = base * 0.62 * digits as f32 + base * 1.0;
        // 삼각형이 그려지는 띠. 조금 넓게 잡는다 — 8px 표적을 정확히 맞히라고
        // 요구하면 아무도 안 쓴다.
        let lo = x + pad + gutter_w - base * 0.95;
        let hi = x + pad + gutter_w + base * 0.15;
        if click_x < lo || click_x > hi {
            return None;
        }
        let top0 = (y - scroll) + pad;
        let row = ((click_y - top0) / lh).floor().max(0.0) as usize;
        Some(crate::markdown::buffer_line(folds, row, lines.len()))
    }

    /// Raw-editor row metrics for the current font: (top pad, line height) in
    /// logical px. `draw_raw_editor` lays lines out at `pad + line * lh`, and
    /// `set_md_mode` inverts that to turn a scroll offset into a line number —
    /// so both must read the numbers from here, not restate them.
    pub fn raw_editor_metrics(&mut self) -> (f32, f32) {
        let base = self.font_size_px as f32 / self.scale;
        let lh = (self.shaper.line_height(base * self.scale).ceil() / self.scale) * 1.25;
        (base * 0.6, lh)
    }

    /// `find` = the find bar's matches as (line, start col, end col) plus the
    /// index of the highlighted one. Every match gets a band, so the spread of
    /// hits down the page is visible, not just the one you're standing on.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_raw_editor(
        &mut self,
        lines: &[String],
        cursor: (usize, usize),
        sel: Option<((usize, usize), (usize, usize))>,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        scroll: f32,
        h_scroll: f32,
        lang: &str,
        preedit: &str,
        cursor_on: bool,
        find: Option<(&[(usize, usize, usize)], usize)>,
        // 자동완성 팝업: (후보, 고른 것, 낱말이 시작한 열).
        complete: Option<(&[String], usize, usize)>,
        // LSP 진단 — 물결 밑줄과 캐럿 줄 옆 인라인 메시지.
        diags: &[crate::lsp::Diag],
        // 접힌 구간들. 비어 있으면 모든 줄이 그대로 그려진다.
        folds: &[(usize, usize)],
        // 긴 줄을 본문 폭에서 접어 내릴지. 끄면 가로 스크롤로 본다.
        wrap: bool,
        // 보조 커서들. 비어 있는 게 보통이고, 그때는 아래 loop 가 한 번도 안 돈다.
        extra: &[crate::markdown::Caret],
    ) -> f32 {
        // 이 렌더러엔 scissor 가 없어서 clip_right 가 곧 본문의 오른쪽 벽이다.
        let clip_right = x + w;
        let base = self.font_size_px as f32 / self.scale;
        let (pad, lh) = self.raw_editor_metrics();
        // The line box (lh) is 1.25× the glyph height for breathing room, so the
        // text/number/cursor must drop by half the slack to sit centered in the
        // row — otherwise they cling to the top and the current-line highlight
        // band (which fills the whole box) looks misaligned.
        let glyph_voff = (lh - base) * 0.5;
        // Line-number gutter, sized to the digit count, right-aligned numbers.
        let digits = ((lines.len().max(1)) as f32).log10().floor() as usize + 1;
        let gutter_w = base * 0.62 * digits as f32 + base * 1.0;
        let cx0 = x + pad + gutter_w;
        // Text origin pans left with the horizontal scroll; the fixed gutter is
        // overpainted after each line so panned-left text never bleeds into it.
        let tx0 = cx0 - h_scroll;
        let clip_top = y;
        let clip_bot = y + h;
        let top0 = (y - scroll) + pad;
        // Tree-sitter spans for the whole buffer (cached across frames, Rc so
        // the borrow doesn't block the &mut draw calls below). None → the
        // per-line lexer fallback inside the loop.
        let prof_draw = crate::info::profiling().then(std::time::Instant::now);
        let (ts_spans, ts_stale) = match self.raw_editor_ts_spans(lines, lang) {
            Some((spans, stale)) => (Some(spans), stale),
            None => (None, false),
        };
        // 캐럿에 붙은 괄호와 그 짝. 선택 중엔 계산하지 않는다 — 선택 밴드와
        // 겹쳐 그리면 어느 쪽이 선택인지 읽히지 않는다.
        let brackets = sel
            .is_none()
            .then(|| crate::markdown::match_bracket(lines, cursor.0, cursor.1))
            .flatten();
        // 격자라 칸 폭 하나로 모든 x 가 나온다 — 아래의 선택 밴드·괄호 강조·
        // 캐럿·가이드가 전부 이 값의 곱이다.
        let cw = self.cell_w;
        let guide_step = cw * crate::markdown::indent_step_cols() as f32;
        // 화면 행 배열 — 접힘은 행을 지우고 랩은 행을 늘린다. 이 배열 하나가
        // "몇 번째 줄을 어디부터 어디까지 그릴까"를 전부 답한다.
        // 보조 커서가 덮는 범위. 한 번만 뽑아 두고 줄마다 재사용한다.
        let extra_sel: Vec<((usize, usize), (usize, usize))> =
            extra.iter().filter(|c| c.anchor.is_some()).map(|c| c.span()).collect();
        let wrap_cols = self.raw_editor_wrap_cols(w, lines.len(), wrap);
        let rows = crate::markdown::layout_rows(lines, folds, wrap_cols);
        let mut pen_y = top0;
        for ri in 0..rows.len() {
            let (li, from, to) = crate::markdown::row_span(&rows, ri, lines);
            let line = &lines[li];
            // 이 줄의 마지막 행인가 — 줄 끝에 붙는 것들(캐럿의 끝자리·Error
            // Lens·접힘 배지·선택의 줄바꿈 표시)이 이걸 본다.
            let last_row = to >= line.chars().count();
            // 열 `c` 까지가 **이 행 안에서** 몇 칸인지. 행 밖은 잘라 낸다 —
            // 선택·괄호·찾기·진단이 전부 이 하나로 x 를 얻으므로, 랩이 걸려도
            // 좌표가 갈릴 수 없다.
            let cols_to = |c: usize| -> f32 {
                let c = c.clamp(from, to);
                cell_cols(&line.chars().skip(from).take(c - from).collect::<String>()) as f32
            };
            if pen_y + lh > clip_top && pen_y < clip_bot {
                // Current-line highlight: a faint band across the pane behind
                // the cursor's row (drawn first so code paints on top). Must be
                // brighter than BG — SURFACE is *darker*, so it reads invisible.
                if li == cursor.0 {
                    self.rect(x, pen_y, w, lh, crate::theme::surface_hover());
                }
                // 들여쓰기 가이드 — 현재 줄 밴드 위, 선택 밴드 아래. 첫 선이
                // 들여쓰기 0 칸 자리라 코드 왼쪽 끝에 붙는다(VS Code 와 같다).
                let guide_col = crate::theme::with_alpha(crate::theme::text(), 0x1A);
                // 랩으로 이어진 행엔 안 그린다 — 이어진 행은 들여쓰기가 아니라
                // 같은 줄의 계속이라, 세로선을 얹으면 없는 블록이 보인다.
                let guide_n = if from == 0 {
                    crate::markdown::indent_guide_depth(lines, li)
                } else {
                    0
                };
                for k in 0..guide_n {
                    let gx = tx0 + guide_step * k as f32;
                    if gx < cx0 || gx > clip_right {
                        continue;
                    }
                    self.rect(gx, pen_y, 1.0, lh, guide_col);
                }
                // Selection band for this line's slice of the (normalized)
                // range: full width on interior lines (plus a small nub for
                // the newline), prefix-measured ends on the boundary lines.
                // Drawn before the text so glyphs stay crisp on top.
                // 주 선택과 보조 커서의 선택을 **같은 규칙**으로 그린다 — 규칙을
                // 두 벌로 두면 랩·접힘이 바뀔 때 한쪽만 고쳐져 어긋난다.
                for (s, e) in sel.iter().copied().chain(extra_sel.iter().copied()) {
                    if li >= s.0 && li <= e.0 {
                        let c0 = if li == s.0 { s.1 } else { from };
                        let c1 = if li == e.0 { e.1 } else { to };
                        let sx0 = tx0 + cols_to(c0) * cw;
                        let mut sx1 = tx0 + cols_to(c1) * cw;
                        if li < e.0 && last_row {
                            // 줄바꿈도 선택에 들어갔다는 표시로 한 칸을 더 덮는다.
                            // 줄의 **마지막 행**에만 — 랩으로 이어지는 자리엔
                            // 줄바꿈이 없다.
                            sx1 += cw;
                        }
                        let rx0 = sx0.max(cx0);
                        let rx1 = sx1.min(clip_right);
                        if rx1 > rx0 {
                            self.rect(
                                rx0,
                                pen_y,
                                rx1 - rx0,
                                lh,
                                crate::theme::with_alpha(crate::theme::accent(), 0x4A),
                            );
                        }
                    }
                }
                // 괄호 짝 — 글자 뒤에 옅은 판을 깔아 두 짝이 같이 밝아진다.
                // 테두리가 아니라 배경인 이유: 셀 폭이 글리프마다 달라서
                // 1px 선은 글자와 어긋난 채로 붙는다.
                if let Some((a, b)) = brackets {
                    for (bl, bc) in [a, b] {
                        if bl != li {
                            continue;
                        }
                        if bc < from || bc >= to {
                            continue;
                        }
                        let bx0 = tx0 + cols_to(bc) * cw;
                        let bw = cw;
                        let rx0 = bx0.max(cx0);
                        let rx1 = (bx0 + bw).min(clip_right);
                        if rx1 > rx0 {
                            self.rect(
                                rx0,
                                pen_y,
                                rx1 - rx0,
                                lh,
                                crate::theme::with_alpha(crate::theme::accent(), 0x38),
                            );
                        }
                    }
                }
                // Find matches on this line, under the text like the selection.
                // The active one is opaque-ish, the rest are a faint wash.
                if let Some((hits, active)) = find {
                    for (hi, &(hl, c0, c1)) in hits.iter().enumerate() {
                        if hl != li {
                            continue;
                        }
                        let sx0 = tx0 + cols_to(c0) * cw;
                        let sx1 = tx0 + cols_to(c1) * cw;
                        let rx0 = sx0.max(cx0);
                        let rx1 = sx1.min(clip_right);
                        if rx1 > rx0 {
                            let a = if hi == active { 0x99 } else { 0x38 };
                            let col = crate::theme::with_alpha(crate::theme::syn_type(), a);
                            self.rect(rx0, pen_y, rx1 - rx0, lh, col);
                        }
                    }
                }
                // Code line: tree-sitter spans when the grammar is supported,
                // else the stateless line lexer (single TEXT color when `lang`
                // is empty, e.g. plain text). Panned by h_scroll.
                // 조합 중인 줄은 하이라이트를 한 프레임 접고 prefix/조합/suffix 를
                // 직접 그린다. 예전엔 줄을 다 그린 뒤 조합 글자를 캐럿 자리에
                // **덮어** 그려서, 편집기에선 뒤 글자와 뭉개져 어디에 쓰고 있는지
                // 안 보였다(거노: "입력중인거 이상한 위치에 있어"). 터미널은 셀
                // 격자라 덮어도 되지만 편집기는 밀어야 맞다.
                let composing = li == cursor.0 && !preedit.is_empty();
                // 재파싱을 미루는 동안(ts_stale)엔 **편집 중인 줄만** 줄 단위
                // lexer 로 칠한다. 그 줄의 캐시 색은 이미 낡아서 방금 친 글자가
                // 무색으로 남는데, 그러면 타이핑이 죽은 것처럼 읽힌다. 나머지
                // 줄은 캐시 색이 그대로 맞으므로 건드리지 않는다.
                let row = ts_spans.as_ref().and_then(|s| s.get(li));
                let lexer = row.is_none() || (ts_stale && li == cursor.0);
                // 글자별 색으로 펼쳐 한 줄을 한 번에 격자에 그린다 — 토큰마다
                // 나눠 그리면 경계에서 이웃 칸이 비었는지 알 수 없어 넓은 글리프의
                // 슬라이드 판정이 깨진다.
                let text_col = crate::theme::text();
                let mut cells: Vec<(char, [u8; 4])> = Vec::with_capacity(line.len() + 4);
                let mut pe_cols = (0usize, 0usize);
                if composing {
                    let accent = crate::theme::accent();
                    cells.extend(line.chars().take(cursor.1).map(|c| (c, text_col)));
                    pe_cols.0 = cells.iter().map(|&(c, _)| 1 + usize::from(is_wide_char(c))).sum();
                    cells.extend(preedit.chars().map(|c| (c, accent)));
                    pe_cols.1 = pe_cols.0 + cell_cols(preedit);
                    cells.extend(line.chars().skip(cursor.1).map(|c| (c, text_col)));
                } else if let (false, Some(spans)) = (lexer, row) {
                    for (tok, kind) in spans {
                        let c = kind.color(text_col);
                        cells.extend(tok.chars().map(|ch| (ch, c)));
                    }
                } else {
                    for (tok, col) in highlight_code_line(line, lang, text_col) {
                        cells.extend(tok.chars().map(|ch| (ch, col)));
                    }
                }
                // 이 행이 담는 부분만 그린다. `cells` 는 줄 전체로 만들어 두고
                // 여기서 자르는데, 그래야 tree-sitter 스팬을 행 경계에 맞춰
                // 쪼개는 일을 안 한다.
                let (sa, sb) = if composing {
                    // 조합 글자가 캐럿 앞에 끼어 `cells` 가 그만큼 길다 —
                    // 캐럿 뒤쪽 열은 그 길이만큼 밀린다.
                    let pe = preedit.chars().count();
                    (
                        from + if from > cursor.1 { pe } else { 0 },
                        to + if to >= cursor.1 { pe } else { 0 },
                    )
                } else {
                    (from, to)
                };
                let sa = sa.min(cells.len());
                let sb = sb.clamp(sa, cells.len());
                self.draw_editor_cells(&cells[sa..sb], tx0, pen_y + glyph_voff, base, cx0, clip_right);
                if pe_cols.1 > pe_cols.0 && cursor.1 >= from && cursor.1 <= to {
                    let at = cols_to(cursor.1);
                    let ux0 = (tx0 + at * cw).max(cx0);
                    let ux1 = (tx0 + (at + cell_cols(preedit) as f32) * cw).min(clip_right);
                    if ux1 > ux0 {
                        self.rect(
                            ux0,
                            pen_y + glyph_voff + base - 2.0,
                            ux1 - ux0,
                            2.0,
                            crate::theme::accent(),
                        );
                    }
                }
                // LSP 진단 — 글자 아래 물결. 밴드가 아니라 밑줄인 이유는 선택·
                // 찾기·괄호가 이미 배경 판을 쓰기 때문이다. 배경으로 겹치면
                // 어느 것이 선택인지 안 읽힌다.
                // 덜 심각한 것부터 — 범위가 겹치면 나중에 그린 쪽이 남는다.
                for d in [4u8, 3, 2, 1].iter().flat_map(|&s| {
                    diags
                        .iter()
                        .filter(move |d| d.severity == s && li >= d.line && li <= d.end_line)
                }) {
                    let c0 = if li == d.line { d.col } else { from };
                    let c1 = if li == d.end_line { d.end_col } else { to };
                    // 이 행과 안 겹치면 건너뛴다. 빈 범위(줄 끝을 가리키는
                    // 진단)는 겹치는 것으로 친다 — 그것도 보여야 한다.
                    if c1 < from || (c0 >= to && !(last_row && c0 == c1)) {
                        continue;
                    }
                    let ux0 = tx0 + cols_to(c0) * cw;
                    let ux1 = (tx0 + cols_to(c1) * cw).max(ux0 + cw);
                    let rx0 = ux0.max(cx0);
                    let rx1 = ux1.min(clip_right);
                    if rx1 > rx0 {
                        let col = diag_color(d.severity);
                        self.wavy_line(rx0, rx1, pen_y + glyph_voff + base - 1.0, col);
                    }
                }
                // 접힌 머리 줄 끝에 몇 줄이 숨었는지. 접힌 자리가 눈에 띄지
                // 않으면 사라진 코드를 찾다가 파일이 망가진 줄 안다.
                if let Some(&(_, fe)) = folds.iter().find(|&&(s, _)| s == li).filter(|_| last_row) {
                    let bx = tx0 + (cols_to(to) + 1.0) * cw;
                    let label = format!("⋯ {}", fe - li);
                    let lw = self.measure_pen_run(&label, base * 0.85, false, false) + cw;
                    if bx > cx0 && bx + lw < clip_right {
                        self.rect(
                            bx,
                            pen_y + glyph_voff * 0.4,
                            lw,
                            base * 1.15,
                            crate::theme::surface_active(),
                        );
                        self.draw_text(
                            bx + cw * 0.5,
                            pen_y + glyph_voff,
                            &label,
                            DrawOpts {
                                font_size: base * 0.85,
                                color: crate::theme::text_dim(),
                                bold: false,
                                italic: false,
                            },
                        );
                    }
                }
                // 보조 커서 — 주 캐럿과 같은 깜빡임을 탄다. 서 있는 커서가
                // 깜빡이지 않으면 "여기도 타이핑이 들어간다"가 안 읽힌다.
                if cursor_on {
                    for c in extra {
                        if c.line != li || c.col < from || (c.col >= to && !last_row) {
                            continue;
                        }
                        let ex = tx0 + cols_to(c.col) * cw;
                        if ex >= cx0 && ex < clip_right {
                            self.rect(ex, pen_y + glyph_voff, 2.0, base, crate::theme::accent());
                        }
                    }
                }
                // Cursor (drawn before the gutter mask so one panned under the
                // gutter gets clipped away cleanly).
                if li == cursor.0 && cursor.1 >= from && (cursor.1 < to || last_row) {
                    let mut cur_x = tx0 + cols_to(cursor.1) * cw;
                    // 조합 글자는 위 `composing` 가지가 이미 밀어 그렸다 — 여기선
                    // 그 폭만큼 캐럿을 뒤로 옮기기만 한다(두 번 그리면 겹친다).
                    if !preedit.is_empty() {
                        cur_x += cell_cols(preedit) as f32 * cw;
                    }
                    if cursor_on && cur_x >= cx0 {
                        // Cursor bar matches the glyph box (same voff + height as
                        // the text) so it lines up with the characters, not the
                        // padded line box.
                        self.rect(cur_x, pen_y + glyph_voff, 2.0, base, crate::theme::accent());
                    }
                    // 캐럿이 선 줄의 진단 메시지를 줄 끝에 덧붙인다(Error Lens).
                    // 호버는 마우스 좌표가 있어야 하는데, 편집 중엔 손이 키보드에
                    // 있으니 캐럿 줄에 붙이는 편이 실제로 읽힌다. 심각한 것 하나만.
                    if let Some(d) = diags
                        .iter()
                        .filter(|d| li >= d.line && li <= d.end_line)
                        .min_by_key(|d| d.severity)
                    {
                        let msg = d.message.lines().next().unwrap_or("");
                        let mx = tx0 + (cols_to(to) + 2.0) * cw;
                        if last_row && !msg.is_empty() && mx < clip_right {
                            self.draw_text_clipped(
                                mx,
                                pen_y + glyph_voff,
                                msg,
                                DrawOpts {
                                    font_size: base,
                                    color: crate::theme::with_alpha(
                                        diag_color(d.severity),
                                        0xB0,
                                    ),
                                    bold: false,
                                    italic: true,
                                },
                                cx0,
                                clip_right,
                            );
                        }
                    }
                }
                // Gutter mask: repaint the column over any text that scrolled
                // under it, then the right-aligned line number on top. The
                // current row keeps its highlight tint so the band reads as full
                // width (line number included).
                let gutter_bg = if li == cursor.0 {
                    crate::theme::surface_hover()
                } else {
                    crate::theme::bg()
                };
                self.rect(x, pen_y, cx0 - x, lh, gutter_bg);
                // 접기 표시 — 줄 번호와 코드 사이 여백에 삼각형 하나. 접을 수
                // 있는지는 **다음 줄이 더 깊은가**로 본다: 블록 끝까지 훑는
                // `fold_end` 는 화면의 모든 줄에서 부르기엔 비싸고, 여기 필요한
                // 건 "표시할까" 하나뿐이다.
                // 랩으로 이어진 행에는 번호도 삼각형도 없다 — VS Code 와 같다.
                // 번호가 반복되면 그게 새 줄인 줄 안다.
                let folded_here = from == 0 && folds.iter().any(|&(s, _)| s == li);
                let foldable = from == 0
                    && folded_here
                    || crate::markdown::fold_depth(line)
                        .zip(
                            lines
                                .get(li + 1)
                                .and_then(|n| crate::markdown::fold_depth(n)),
                        )
                        .is_some_and(|(a, b)| b > a);
                if foldable {
                    self.draw_text(
                        x + pad + gutter_w - base * 0.62,
                        pen_y + glyph_voff,
                        if folded_here { "▸" } else { "▾" },
                        DrawOpts {
                            font_size: base * 0.8,
                            color: crate::theme::with_alpha(
                                crate::theme::text_mute(),
                                if folded_here { 0xFF } else { 0x66 },
                            ),
                            bold: false,
                            italic: false,
                        },
                    );
                }
                let num = if from == 0 { format!("{}", li + 1) } else { String::new() };
                let num_w = self.measure_pen_run(&num, base, false, false);
                self.draw_text(
                    x + pad + (gutter_w - base * 0.5 - num_w).max(0.0),
                    pen_y + glyph_voff,
                    &num,
                    DrawOpts {
                        font_size: base,
                        color: crate::theme::text_mute(),
                        bold: false,
                        italic: false,
                    },
                );
            }
            pen_y += lh;
        }
        // 자동완성 팝업 — 줄 루프 **밖**에서 마지막에 그린다. 안에서 그리면
        // 뒤에 오는 줄들이 위에 덮여 목록이 반쯤 잘린다.
        if let Some((items, sel, from_col)) = complete {
            if !items.is_empty() {
                let px = (tx0 + from_col as f32 * cw).max(cx0);
                let wide = items.iter().map(|s| cell_cols(s)).max().unwrap_or(0);
                let bw = ((wide + 2) as f32 * cw).min(clip_right - px);
                let bh = items.len() as f32 * lh;
                let below = top0 + (cursor.0 + 1) as f32 * lh;
                // 아래로 넘치면 캐럿 줄 위로 뒤집는다 — 화면 밖에 뜬 목록은
                // 없는 것과 같다.
                let by = if below + bh <= y + h {
                    below
                } else {
                    (below - lh - bh).max(y)
                };
                // bg 보다 밝은 판 + 테두리라야 문서 위에 떠 있는 것으로 읽힌다.
                self.rect(px, by, bw, bh, crate::theme::surface_active());
                self.rect(px, by, bw, 1.0, crate::theme::border());
                self.rect(px, by + bh - 1.0, bw, 1.0, crate::theme::border());
                self.rect(px, by, 1.0, bh, crate::theme::border());
                self.rect(px + bw - 1.0, by, 1.0, bh, crate::theme::border());
                for (i, it) in items.iter().enumerate() {
                    let iy = by + i as f32 * lh;
                    if i == sel {
                        self.rect(
                            px + 1.0,
                            iy,
                            bw - 2.0,
                            lh,
                            crate::theme::with_alpha(crate::theme::accent(), 0x66),
                        );
                    }
                    let cells: Vec<(char, [u8; 4])> =
                        it.chars().map(|c| (c, crate::theme::text())).collect();
                    self.draw_editor_cells(
                        &cells,
                        px + cw,
                        iy + glyph_voff,
                        base,
                        px,
                        px + bw,
                    );
                }
            }
        }
        if let Some(t) = prof_draw {
            eprintln!(
                "[prof] draw_raw_editor {}us lines={}",
                t.elapsed().as_micros(),
                lines.len()
            );
        }
        (pen_y - top0 + pad).max(0.0)
    }

    /// Drop all pending chrome instances. main.rs calls this at the
    /// top of each frame so stale rects/labels from the previous
    /// frame don't pile up.
    pub fn clear_chrome(&mut self) {
        self.chrome.clear();
        if let Some(log) = self.text_log.as_mut() {
            log.clear();
        }
        self.image_quads.clear();
        self.icon_quads.clear();
        self.clip_stack.clear();
        self.clip_runs.clear();
        self.hover_pointer = false;
    }

    /// 지금 유효한 클립을 PHYSICAL px `[x, y, w, h]` 로. 클립이 없으면 `None`.
    /// 폭이나 높이가 0 이면 「아무것도 안 보이는 클립」이라 그리기 자체를 건너뛴다.
    fn cur_clip_phys(&self) -> Option<[u32; 4]> {
        let [x0, y0, x1, y1] = *self.clip_stack.last()?;
        let s = self.scale;
        let (fw, fh) = (self.config.width as f32, self.config.height as f32);
        // 바깥으로 나간 만큼은 잘라 낸다 — `set_scissor_rect` 는 어태치먼트를
        // 넘는 사각형에 패닉한다.
        let px0 = (x0 * s).floor().clamp(0.0, fw);
        let py0 = (y0 * s).floor().clamp(0.0, fh);
        let px1 = (x1 * s).ceil().clamp(px0, fw);
        let py1 = (y1 * s).ceil().clamp(py0, fh);
        Some([px0 as u32, py0 as u32, (px1 - px0) as u32, (py1 - py0) as u32])
    }

    /// 클립이 방금 바뀌었음을 기록한다. 같은 chrome 위치에 두 번 기록되면 뒤엣것만
    /// 살아남게 덮어쓴다 — `push_clip` 직후 `pop_clip` 처럼 사이에 아무것도 안 그린
    /// 경우 세그먼트가 빈 채로 쌓이는 것을 막는다.
    fn note_clip(&mut self) {
        let at = self.chrome.len() as u32;
        let cur = self.cur_clip_phys();
        match self.clip_runs.last_mut() {
            Some((i, c)) if *i == at => *c = cur,
            _ => self.clip_runs.push((at, cur)),
        }
    }

    /// 이 뒤로 그리는 chrome 을 `(x, y, w, h)`(LOGICAL px) 안으로 가둔다. 이미 클립이
    /// 서 있으면 **교집합**이 된다 — 안쪽 클립이 바깥을 넓힐 수는 없다.
    ///
    /// ⚠️ **행 루프 밖에서 한 번만 불러라.** 안에서 부르면 행마다 run 이 두 개씩
    /// 쌓여 세그먼트가 행 수만큼 늘고, 그만큼 draw call 이 는다.
    ///
    /// ⚠️ **시저는 픽셀만 자르지 클릭은 안 자른다.** 그리기 스킵을 지운 자리마다
    /// 히트렉트를 [`clip_hit`](Self::clip_hit) 로 교집합 내지 않으면 화면은 멀쩡한데
    /// 안 보이는 행이 눌린다 — 스크린샷이 절대 못 잡는 부류다.
    pub fn push_clip(&mut self, x: f32, y: f32, w: f32, h: f32) {
        let (mut x0, mut y0, mut x1, mut y1) = (x, y, x + w.max(0.0), y + h.max(0.0));
        if let Some([px0, py0, px1, py1]) = self.clip_stack.last().copied() {
            x0 = x0.max(px0);
            y0 = y0.max(py0);
            x1 = x1.min(px1).max(x0);
            y1 = y1.min(py1).max(y0);
        }
        self.clip_stack.push([x0, y0, x1, y1]);
        self.note_clip();
    }

    /// 가장 안쪽 클립을 걷는다. 짝이 안 맞는 `pop` 은 무시하고 로그만 남긴다 —
    /// 여기서 패닉하면 그리기 한 곳의 실수가 앱을 죽인다.
    pub fn pop_clip(&mut self) {
        if self.clip_stack.pop().is_none() {
            eprintln!("[clip] pop_clip 이 push 보다 많다 — 이 프레임의 클립은 버려진다");
            // 스택을 음수로 만들 수는 없으니, 대신 `render` 의 fail-open 이 물도록
            // 균형을 깨 둔다.
            self.clip_stack.push([0.0, 0.0, 0.0, 0.0]);
            return;
        }
        self.note_clip();
    }

    /// 히트렉트를 지금 클립과 교집합 낸다 — 잘려 안 보이는 부분은 눌려도 안 되므로.
    /// 교집합이 비면 `None`(= 그 rect 는 등록하지 마라).
    ///
    /// 클립이 없으면 받은 그대로 돌려준다. LOGICAL px `(x, y, w, h)`.
    pub fn clip_hit(&self, r: (f32, f32, f32, f32)) -> Option<(f32, f32, f32, f32)> {
        let Some([cx0, cy0, cx1, cy1]) = self.clip_stack.last().copied() else {
            return (r.2 > 0.0 && r.3 > 0.0).then_some(r);
        };
        let x0 = r.0.max(cx0);
        let y0 = r.1.max(cy0);
        let x1 = (r.0 + r.2).min(cx1);
        let y1 = (r.1 + r.3).min(cy1);
        (x1 > x0 && y1 > y0).then_some((x0, y0, x1 - x0, y1 - y0))
    }

    /// 이 사각형이 지금 클립에 조금이라도 걸치나 — **컬링용**이다.
    ///
    /// 완전히 밖이면 `false`, 즉 그리기를 통째로 건너뛰어도 되는 경우다. 반쯤
    /// 걸치면 `true` 이고, 그건 시저가 알아서 자르니 **그릴 것**이다. 목록이
    /// 5000행쯤 되면 이 컬링이 없을 때 인스턴스가 수만 개로 불어난다 —
    /// 시저는 컬링을 대체하지 않는다.
    pub fn clip_visible(&self, x: f32, y: f32, w: f32, h: f32) -> bool {
        let Some([cx0, cy0, cx1, cy1]) = self.clip_stack.last().copied() else {
            return true;
        };
        x + w > cx0 && x < cx1 && y + h > cy0 && y < cy1
    }

    /// Has this pane's image already been uploaded? Lets the caller skip
    /// re-handing us the pixel buffer every frame.
    pub fn has_image(&self, id: &str) -> bool {
        self.images.contains_key(id)
    }

    /// 이번 프레임에 실제로 올라간 이미지 드로우의 키들 — **두 목록 다**.
    ///
    /// 하네스가 "학생 스프라이트가 정말 그려졌나"를 그림 없이 판정하는 유일한
    /// 창구다. 스캐너(`find_statusline_face` 등)를 하네스가 직접 다시 돌리면
    /// **자리가 있는지**만 알 뿐, 렌더 패스가 그걸 부르는지는 못 본다 — 그게
    /// 정확히 #48 이었다(자리는 멀쩡했고 부르는 쪽이 없었다).
    ///
    /// `image_quads` 만 세면 안 된다: 같은 학생이라도 statusline 프사는 셀 위에
    /// 얹히는 `queue_image_above` → `icon_quads` 로 가고 배경/커버는 `image_quads`
    /// 로 간다. 한쪽만 보면 프사가 멀쩡히 떠 있는데 0 개로 읽힌다(실측).
    pub fn drawn_image_keys(&self) -> impl Iterator<Item = &str> {
        self.image_quads.iter().chain(self.icon_quads.iter()).map(|(k, ..)| k.as_str())
    }

    /// 이번 프레임에 그 키로 그린 quad 들의 자리 — LOGICAL px `(x, y, w, h)`.
    ///
    /// 키만으로는 못 가르는 게 있어서 필요하다: 프사 마우스오버 팝업은 작은
    /// 프사와 **같은 키**(`student:<slug>:profile`)를 크기만 키워 재사용한다.
    /// 그래서 「팝업이 떴나」는 키 존재가 아니라 **큰 quad 가 하나 더 붙었나**로
    /// 판정해야 한다.
    ///
    /// 덤으로 하네스가 프사 자리를 렌더에서 되읽을 수 있다 — 좌표 계산
    /// (`cell_left()`·`AUX_CELL_TOP`)을 복제하면 그 복제본이 틀려도 하네스는
    /// 통과해 버린다.
    pub fn drawn_image_rects(&self, key: &str) -> Vec<(f32, f32, f32, f32)> {
        let s = self.scale.max(f32::EPSILON);
        self.image_quads
            .iter()
            .chain(self.icon_quads.iter())
            .filter(|(k, ..)| k == key)
            .map(|(_, c, ..)| {
                let [x, y, w, h] = c.cell_px;
                (x / s, y / s, w / s, h / s)
            })
            .collect()
    }

    /// 이번 프레임에 그린 문자열에 `needle` 이 들어간 게 있나. `KASATERM_TEXT_LOG`
    /// 를 안 켜면 항상 `None` — "안 그려졌다"와 "안 재고 있다"를 섞지 않기 위해
    /// bool 이 아니라 `Option` 이다.
    pub fn drew_text(&self, needle: &str) -> Option<bool> {
        let log = self.text_log.as_ref()?;
        Some(log.iter().any(|t| t.contains(needle)) || Self::staged_cell_text(needle))
    }

    /// `drew_text` 가 셀 인레이까지 보게 하는 두 번째 통. `stage_cell_text` 참고.
    fn staged_cell_text(needle: &str) -> bool {
        cell_text_log()
            .map(|m| m.lock().unwrap().iter().any(|t| t.contains(needle)))
            .unwrap_or(false)
    }

    /// 입력박스 보더 인레이가 신고한 마지막 자리 `(좌측 끝, 우측 시작, 우측 끝)`.
    /// 좌측이 없으면 -1. 좌우가 **겹쳤는지**를 재려면 이게 필요하다 — "그렸다"는
    /// 신고만으로는 같은 칸에 겹쳐 써도 통과한다.
    pub fn staged_span(&self) -> Option<(i64, usize, usize)> {
        let m = cell_text_log()?;
        let log = m.lock().unwrap();
        let line = log.iter().rev().find(|t| t.starts_with("[boxspan] "))?;
        let (l, r) = line.strip_prefix("[boxspan] L=")?.split_once(" R=")?;
        let (c0, c1) = r.split_once('-')?;
        Some((l.parse().ok()?, c0.parse().ok()?, c1.parse().ok()?))
    }

    /// Upload an image pane's RGBA8 pixels into a texture + bind group keyed
    /// by pane id. No-op if already present. `rgba` must be `w * h * 4` bytes.
    pub fn upload_image(&mut self, id: &str, rgba: &[u8], w: u32, h: u32) {
        if self.images.contains_key(id) || w == 0 || h == 0 {
            return;
        }
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("kasaterm image"),
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            // Non-srgb to match the surface: image crate yields sRGB bytes
            // and our framebuffer shows them verbatim (same reasoning as the
            // glyph atlas), so colours land correct without a colour-space hop.
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d { x: 0, y: 0, z: 0 },
                aspect: wgpu::TextureAspect::All,
            },
            rgba,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(w * 4),
                rows_per_image: Some(h),
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
        let view = texture.create_view(&Default::default());
        let bind_group =
            self.image_pipeline
                .make_bind_group(&self.device, &view, &self.image_sampler);
        self.images.insert(
            id.to_string(),
            ImageEntry {
                _texture: texture,
                _view: view,
                bind_group,
                w,
                h,
            },
        );
    }

    /// Free a pane's image texture when the pane closes.
    pub fn drop_image(&mut self, id: &str) {
        self.images.remove(id);
    }

    /// Evict every cached texture whose id starts with `prefix`. Used to force a
    /// reload after the user swaps character images — `upload_image` no-ops on an
    /// existing key, so the stale texture must be dropped first.
    pub fn drop_images_with_prefix(&mut self, prefix: &str) {
        self.images.retain(|k, _| !k.starts_with(prefix));
    }

    /// Bundled Lucide SVG source for a chrome icon name. Compiled in so the
    /// .app needs no external asset dir. `pub(crate)` 인 건 아이콘 이름을 짓는
    /// 쪽(예: `sesscol::harness_icon`)이 그 이름이 실제로 등록돼 있는지 테스트할
    /// 수 있어야 해서다 — 없는 이름은 `queue_icon` 이 조용히 그냥 돌아간다.
    pub(crate) fn icon_svg(name: &str) -> Option<&'static str> {
        Some(match name {
            "folder" => include_str!("../assets/icons/folder.svg"),
            "square" => include_str!("../assets/icons/square.svg"),
            "square-check" => include_str!("../assets/icons/square-check.svg"),
            "x" => include_str!("../assets/icons/x.svg"),
            "plus" => include_str!("../assets/icons/plus.svg"),
            "minus" => include_str!("../assets/icons/minus.svg"),
            "panel-left" => include_str!("../assets/icons/panel-left.svg"),
            "panel-right" => include_str!("../assets/icons/panel-right.svg"),
            "pin" => include_str!("../assets/icons/pin.svg"),
            "folder-tree" => include_str!("../assets/icons/folder-tree.svg"),
            "folder-open" => include_str!("../assets/icons/folder-open.svg"),
            "folder-plus" => include_str!("../assets/icons/folder-plus.svg"),
            "file-plus" => include_str!("../assets/icons/file-plus.svg"),
            "chevron-right" => include_str!("../assets/icons/chevron-right.svg"),
            "chevron-down" => include_str!("../assets/icons/chevron-down.svg"),
            "chevron-left" => include_str!("../assets/icons/chevron-left.svg"),
            "chevron-up" => include_str!("../assets/icons/chevron-up.svg"),
            "file" => include_str!("../assets/icons/file.svg"),
            "file-code" => include_str!("../assets/icons/file-code.svg"),
            "image" => include_str!("../assets/icons/image.svg"),
            "users" => include_str!("../assets/icons/users.svg"),
            "braces" => include_str!("../assets/icons/braces.svg"),
            "settings-2" => include_str!("../assets/icons/settings-2.svg"),
            "columns-2" => include_str!("../assets/icons/columns-2.svg"),
            "rows-2" => include_str!("../assets/icons/rows-2.svg"),
            "copy" => include_str!("../assets/icons/copy.svg"),
            "terminal" => include_str!("../assets/icons/terminal.svg"),
            "sparkles" => include_str!("../assets/icons/sparkles.svg"),
            "rotate-cw" => include_str!("../assets/icons/rotate-cw.svg"),
            "maximize" => include_str!("../assets/icons/maximize.svg"),
            "file-text" => include_str!("../assets/icons/file-text.svg"),
            "git-branch" => include_str!("../assets/icons/git-branch.svg"),
            "chevrons-down-up" => include_str!("../assets/icons/chevrons-down-up.svg"),
            "panel-bottom" => include_str!("../assets/icons/panel-bottom.svg"),
            "panel-bottom-dashed" => include_str!("../assets/icons/panel-bottom-dashed.svg"),
            "panel-top" => include_str!("../assets/icons/panel-top.svg"),
            "panel-top-dashed" => include_str!("../assets/icons/panel-top-dashed.svg"),
            "git-commit-horizontal" => include_str!("../assets/icons/git-commit-horizontal.svg"),
            "ellipsis-vertical" => include_str!("../assets/icons/ellipsis-vertical.svg"),
            "ellipsis-horizontal" => include_str!("../assets/icons/ellipsis-horizontal.svg"),
            "arrow-up" => include_str!("../assets/icons/arrow-up.svg"),
            "arrow-down" => include_str!("../assets/icons/arrow-down.svg"),
            "github" => include_str!("../assets/icons/github.svg"),
            "undo-2" => include_str!("../assets/icons/undo-2.svg"),
            "external-link" => include_str!("../assets/icons/external-link.svg"),
            "claude" => include_str!("../assets/icons/claude.svg"),
            "codex" => include_str!("../assets/icons/codex.svg"),
            "antigravity" => include_str!("../assets/icons/antigravity.svg"),
            // 마크다운 콜아웃(`> [!NOTE]` …) 표지. 이모지 대신 SVG 를 쓰는 이유는
            // 이모지가 폰트에 따라 흑백 글리프로 떨어지기 때문 — 실제로 `⚠️` 가
            // 밋밋한 `▲` 로 나온다.
            "info" => include_str!("../assets/icons/info.svg"),
            "lightbulb" => include_str!("../assets/icons/lightbulb.svg"),
            "triangle-alert" => include_str!("../assets/icons/triangle-alert.svg"),
            "octagon-alert" => include_str!("../assets/icons/octagon-alert.svg"),
            "message-square-warning" => include_str!("../assets/icons/message-square-warning.svg"),
            // File-type set (assets/icons/ft): VSCode Material 계열의 브랜드컬러
            // filled SVG — 모노크롬 틴트가 아닌 `queue_icon_colored` 로 그린다.
            "ft/audio" => include_str!("../assets/icons/ft/audio.svg"),
            "ft/c" => include_str!("../assets/icons/ft/c.svg"),
            "ft/console" => include_str!("../assets/icons/ft/console.svg"),
            "ft/cpp" => include_str!("../assets/icons/ft/cpp.svg"),
            "ft/csharp" => include_str!("../assets/icons/ft/csharp.svg"),
            "ft/css" => include_str!("../assets/icons/ft/css.svg"),
            "ft/database" => include_str!("../assets/icons/ft/database.svg"),
            "ft/docker" => include_str!("../assets/icons/ft/docker.svg"),
            "ft/document" => include_str!("../assets/icons/ft/document.svg"),
            "ft/font" => include_str!("../assets/icons/ft/font.svg"),
            "ft/git" => include_str!("../assets/icons/ft/git.svg"),
            "ft/go" => include_str!("../assets/icons/ft/go.svg"),
            "ft/graphql" => include_str!("../assets/icons/ft/graphql.svg"),
            "ft/html" => include_str!("../assets/icons/ft/html.svg"),
            "ft/image" => include_str!("../assets/icons/ft/image.svg"),
            "ft/java" => include_str!("../assets/icons/ft/java.svg"),
            "ft/javascript" => include_str!("../assets/icons/ft/javascript.svg"),
            "ft/json" => include_str!("../assets/icons/ft/json.svg"),
            "ft/kotlin" => include_str!("../assets/icons/ft/kotlin.svg"),
            "ft/license" => include_str!("../assets/icons/ft/license.svg"),
            "ft/lock" => include_str!("../assets/icons/ft/lock.svg"),
            "ft/lua" => include_str!("../assets/icons/ft/lua.svg"),
            "ft/markdown" => include_str!("../assets/icons/ft/markdown.svg"),
            "ft/nodejs" => include_str!("../assets/icons/ft/nodejs.svg"),
            "ft/pdf" => include_str!("../assets/icons/ft/pdf.svg"),
            "ft/php" => include_str!("../assets/icons/ft/php.svg"),
            "ft/powershell" => include_str!("../assets/icons/ft/powershell.svg"),
            "ft/prisma" => include_str!("../assets/icons/ft/prisma.svg"),
            "ft/python" => include_str!("../assets/icons/ft/python.svg"),
            "ft/react" => include_str!("../assets/icons/ft/react.svg"),
            "ft/readme" => include_str!("../assets/icons/ft/readme.svg"),
            "ft/ruby" => include_str!("../assets/icons/ft/ruby.svg"),
            "ft/rust" => include_str!("../assets/icons/ft/rust.svg"),
            "ft/sass" => include_str!("../assets/icons/ft/sass.svg"),
            "ft/settings" => include_str!("../assets/icons/ft/settings.svg"),
            "ft/svg" => include_str!("../assets/icons/ft/svg.svg"),
            "ft/swift" => include_str!("../assets/icons/ft/swift.svg"),
            "ft/todo" => include_str!("../assets/icons/ft/todo.svg"),
            "ft/tsconfig" => include_str!("../assets/icons/ft/tsconfig.svg"),
            "ft/typescript" => include_str!("../assets/icons/ft/typescript.svg"),
            "ft/video" => include_str!("../assets/icons/ft/video.svg"),
            "ft/vue" => include_str!("../assets/icons/ft/vue.svg"),
            "ft/yaml" => include_str!("../assets/icons/ft/yaml.svg"),
            "ft/zip" => include_str!("../assets/icons/ft/zip.svg"),
            "ft/folder-base" => include_str!("../assets/icons/ft/folder-base.svg"),
            "ft/folder-config" => include_str!("../assets/icons/ft/folder-config.svg"),
            "ft/folder-dist" => include_str!("../assets/icons/ft/folder-dist.svg"),
            "ft/folder-docs" => include_str!("../assets/icons/ft/folder-docs.svg"),
            "ft/folder-github" => include_str!("../assets/icons/ft/folder-github.svg"),
            "ft/folder-images" => include_str!("../assets/icons/ft/folder-images.svg"),
            "ft/folder-node" => include_str!("../assets/icons/ft/folder-node.svg"),
            "ft/folder-public" => include_str!("../assets/icons/ft/folder-public.svg"),
            "ft/folder-src" => include_str!("../assets/icons/ft/folder-src.svg"),
            "ft/folder-target" => include_str!("../assets/icons/ft/folder-target.svg"),
            "ft/folder-test" => include_str!("../assets/icons/ft/folder-test.svg"),
            _ => return None,
        })
    }

    /// Dot-matrix counterpart of `icon_svg`, used when the active Shape asks for
    /// pixel chrome. Falling back to `icon_svg` on a miss is deliberate: the set
    /// covers everything but the two brand marks (github, claude), which have no
    /// honest pixel form, and a miss should show the vector icon rather than a
    /// hole. Sourced from pixelarticons (MIT) plus the panel/tree glyphs drawn
    /// here — see assets/icons/pixel/LICENSE.
    fn icon_svg_pixel(name: &str) -> Option<&'static str> {
        Some(match name {
            "arrow-down" => include_str!("../assets/icons/pixel/arrow-down.svg"),
            "arrow-up" => include_str!("../assets/icons/pixel/arrow-up.svg"),
            "braces" => include_str!("../assets/icons/pixel/braces.svg"),
            "chevron-down" => include_str!("../assets/icons/pixel/chevron-down.svg"),
            "chevron-left" => include_str!("../assets/icons/pixel/chevron-left.svg"),
            "chevron-right" => include_str!("../assets/icons/pixel/chevron-right.svg"),
            "chevron-up" => include_str!("../assets/icons/pixel/chevron-up.svg"),
            "chevrons-down-up" => include_str!("../assets/icons/pixel/chevrons-down-up.svg"),
            "columns-2" => include_str!("../assets/icons/pixel/columns-2.svg"),
            "copy" => include_str!("../assets/icons/pixel/copy.svg"),
            "ellipsis-horizontal" => include_str!("../assets/icons/pixel/ellipsis-horizontal.svg"),
            "ellipsis-vertical" => include_str!("../assets/icons/pixel/ellipsis-vertical.svg"),
            "external-link" => include_str!("../assets/icons/pixel/external-link.svg"),
            "file" => include_str!("../assets/icons/pixel/file.svg"),
            "file-code" => include_str!("../assets/icons/pixel/file-code.svg"),
            "file-plus" => include_str!("../assets/icons/pixel/file-plus.svg"),
            "file-text" => include_str!("../assets/icons/pixel/file-text.svg"),
            "folder" => include_str!("../assets/icons/pixel/folder.svg"),
            "folder-open" => include_str!("../assets/icons/pixel/folder-open.svg"),
            "folder-plus" => include_str!("../assets/icons/pixel/folder-plus.svg"),
            "folder-tree" => include_str!("../assets/icons/pixel/folder-tree.svg"),
            "git-branch" => include_str!("../assets/icons/pixel/git-branch.svg"),
            "git-commit-horizontal" => include_str!("../assets/icons/pixel/git-commit-horizontal.svg"),
            "image" => include_str!("../assets/icons/pixel/image.svg"),
            "info" => include_str!("../assets/icons/pixel/info.svg"),
            "lightbulb" => include_str!("../assets/icons/pixel/lightbulb.svg"),
            "maximize" => include_str!("../assets/icons/pixel/maximize.svg"),
            "message-square-warning" => include_str!("../assets/icons/pixel/message-square-warning.svg"),
            "minus" => include_str!("../assets/icons/pixel/minus.svg"),
            "octagon-alert" => include_str!("../assets/icons/pixel/octagon-alert.svg"),
            "panel-bottom" => include_str!("../assets/icons/pixel/panel-bottom.svg"),
            "panel-bottom-dashed" => include_str!("../assets/icons/pixel/panel-bottom-dashed.svg"),
            "panel-left" => include_str!("../assets/icons/pixel/panel-left.svg"),
            "panel-right" => include_str!("../assets/icons/pixel/panel-right.svg"),
            "pin" => include_str!("../assets/icons/pixel/pin.svg"),
            "panel-top" => include_str!("../assets/icons/pixel/panel-top.svg"),
            "panel-top-dashed" => include_str!("../assets/icons/pixel/panel-top-dashed.svg"),
            "plus" => include_str!("../assets/icons/pixel/plus.svg"),
            "rotate-cw" => include_str!("../assets/icons/pixel/rotate-cw.svg"),
            "rows-2" => include_str!("../assets/icons/pixel/rows-2.svg"),
            "settings-2" => include_str!("../assets/icons/pixel/settings-2.svg"),
            "sparkles" => include_str!("../assets/icons/pixel/sparkles.svg"),
            "square" => include_str!("../assets/icons/pixel/square.svg"),
            "square-check" => include_str!("../assets/icons/pixel/square-check.svg"),
            "terminal" => include_str!("../assets/icons/pixel/terminal.svg"),
            "triangle-alert" => include_str!("../assets/icons/pixel/triangle-alert.svg"),
            "undo-2" => include_str!("../assets/icons/pixel/undo-2.svg"),
            "users" => include_str!("../assets/icons/pixel/users.svg"),
            "x" => include_str!("../assets/icons/pixel/x.svg"),
            _ => return None,
        })
    }

    /// Rasterize an SVG into a square `px`-side RGBA8 buffer. `currentColor`
    /// is forced white: only the alpha channel matters because icons draw
    /// through the glyph tint path (texel.a × fg.rgb), so the theme color is
    /// applied at draw time, not bake time.
    pub(crate) fn rasterize_icon(svg: &str, px: u32) -> Option<Vec<u8>> {
        let svg = svg.replace("currentColor", "#ffffff");
        let opt = resvg::usvg::Options::default();
        let tree = resvg::usvg::Tree::from_str(&svg, &opt).ok()?;
        let mut pixmap = resvg::tiny_skia::Pixmap::new(px, px)?;
        let size = tree.size();
        let scale = px as f32 / size.width().max(size.height());
        let tf = resvg::tiny_skia::Transform::from_scale(scale, scale);
        resvg::render(&tree, tf, &mut pixmap.as_mut());
        Some(pixmap.data().to_vec())
    }

    /// `rasterize_icon` 의 풀컬러 버전 — SVG 자체 fill 색을 보존한다.
    /// FLAG_COLOR 경로는 texel.rgb 를 그대로 샘플하므로 tiny_skia 의
    /// premultiplied 출력을 straight alpha 로 되돌려야 반투명 가장자리가
    /// 어두워지지 않는다.
    fn rasterize_icon_color(svg: &str, px: u32) -> Option<Vec<u8>> {
        let opt = resvg::usvg::Options::default();
        let tree = resvg::usvg::Tree::from_str(svg, &opt).ok()?;
        let mut pixmap = resvg::tiny_skia::Pixmap::new(px, px)?;
        let size = tree.size();
        let scale = px as f32 / size.width().max(size.height());
        let tf = resvg::tiny_skia::Transform::from_scale(scale, scale);
        resvg::render(&tree, tf, &mut pixmap.as_mut());
        let mut data = pixmap.take();
        for p in data.chunks_exact_mut(4) {
            let a = p[3] as u32;
            if a > 0 && a < 255 {
                p[0] = ((p[0] as u32 * 255) / a).min(255) as u8;
                p[1] = ((p[1] as u32 * 255) / a).min(255) as u8;
                p[2] = ((p[2] as u32 * 255) / a).min(255) as u8;
            }
        }
        Some(data)
    }

    /// Queue a chrome icon at `(x, y)` (logical px), `size`-side square, tinted
    /// `color`. Lazily rasterizes + caches the white alpha mask at the exact
    /// device-pixel resolution, then draws it through the monochrome tint path
    /// (`flags = 0` → shader does texel.a × fg.rgb) so it picks up hover /
    /// active colors exactly like a glyph would.
    pub fn queue_icon(&mut self, name: &str, x: f32, y: f32, size: f32, color: [u8; 4]) {
        let px = (size * self.scale).round() as u32;
        if px == 0 {
            return;
        }
        // Under a pixel Shape the dot-matrix cut replaces the vector one, and it
        // only stays crisp on whole multiples of its 24-unit grid — the icon
        // equivalent of the font's dot snapping. Floor rather than round: an
        // icon that grew past the box its caller reserved would collide with
        // neighbouring chrome, while one that shrinks just gets recentred below.
        let pixel = crate::theme::pixel_chrome().then(|| Self::icon_svg_pixel(name)).flatten();
        let draw_px = match pixel {
            Some(_) if px >= ICON_GRID_PX => px / ICON_GRID_PX * ICON_GRID_PX,
            _ => px,
        };
        let key = match pixel {
            Some(_) => format!("__iconp:{name}:{draw_px}"),
            None => format!("__icon:{name}:{px}"),
        };
        if !self.images.contains_key(&key) {
            let Some(svg) = pixel.or_else(|| Self::icon_svg(name)) else { return };
            let Some(rgba) = Self::rasterize_icon(svg, draw_px) else { return };
            self.upload_image(&key, &rgba, draw_px, draw_px);
        }
        if !self.images.contains_key(&key) {
            return;
        }
        // Snap to whole device pixels: the texture is rasterized 1:1 at `px`,
        // so a fractional dest makes the linear sampler blur / fringe the
        // edges ("마우스오버 픽셀 보임"). Integer dest = crisp 1:1 blit.
        let inset = ((px - draw_px) / 2) as f32;
        let (dx, dy) = (
            (x * self.scale).round() + inset,
            (y * self.scale).round() + inset,
        );
        let dpx = draw_px as f32;
        self.icon_quads.push((
            key,
            CellInstance {
                cell_px: [dx, dy, dpx, dpx],
                uv_min: [0.0, 0.0],
                uv_max: [1.0, 1.0],
                fg_rgba: srgb_rgba_to_linear(color),
                flags: CellInstance::FLAG_ICON,
                ..Default::default()
            },
            self.chrome.len() as u32,
            self.cur_clip_phys(),
        ));
    }

    /// `queue_icon` 의 풀컬러 버전 — 파일타입 아이콘(ft/*)처럼 SVG 자체 색을
    /// 가진 글리프용. FLAG_COLOR(이모지 경로)로 그려 texel 색을 그대로 쓰고,
    /// `alpha` 만 전역 불투명도로 곱한다(ignored/dim 행 표현).
    pub fn queue_icon_colored(&mut self, name: &str, x: f32, y: f32, size: f32, alpha: f32) {
        let px = (size * self.scale).round() as u32;
        if px == 0 {
            return;
        }
        let key = format!("__iconc:{name}:{px}");
        if !self.images.contains_key(&key) {
            let Some(svg) = Self::icon_svg(name) else { return };
            let Some(rgba) = Self::rasterize_icon_color(svg, px) else { return };
            self.upload_image(&key, &rgba, px, px);
        }
        if !self.images.contains_key(&key) {
            return;
        }
        let (dx, dy) = ((x * self.scale).round(), (y * self.scale).round());
        let dpx = px as f32;
        self.icon_quads.push((
            key,
            CellInstance {
                cell_px: [dx, dy, dpx, dpx],
                uv_min: [0.0, 0.0],
                uv_max: [1.0, 1.0],
                fg_rgba: [1.0, 1.0, 1.0, alpha],
                flags: CellInstance::FLAG_COLOR,
                ..Default::default()
            },
            self.chrome.len() as u32,
            self.cur_clip_phys(),
        ));
    }

    /// Queue an image pane for this frame. `(x, y, w, h)` is the pane's body
    /// box in LOGICAL px; the image is contain-fit (aspect preserved,
    /// centered) inside it. `zoom >= 1.0` scales past the fit size — when
    /// it would overflow the pane box we clip the dest rect AND adjust UVs
    /// so the image stays inside the pane (cropped to its center, never
    /// leaking into adjacent panes). `(pan_x, pan_y)` shift the crop window
    /// (logical px, image-center offset) so a drag pans a zoomed image;
    /// clamped here so the window never leaves the texture.
    pub fn queue_image(
        &mut self,
        id: &str,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        zoom: f32,
        pan_x: f32,
        pan_y: f32,
    ) {
        let Some(entry) = self.images.get(id) else { return };
        let s = self.scale;
        let (bx, by, bw, bh) = (x * s, y * s, w * s, h * s);
        if bw <= 0.0 || bh <= 0.0 {
            return;
        }
        let (iw, ih) = (entry.w as f32, entry.h as f32);
        // Contain fit, but never upscale past native — a small icon stays
        // crisp at 1:1 instead of blowing up blurry to fill the pane.
        let fit = (bw / iw).min(bh / ih).min(1.0);
        let z = zoom.max(1.0);
        let raw_dw = iw * fit * z;
        let raw_dh = ih * fit * z;
        // Per-axis: if the zoomed image fits, center it (pan has no room to
        // act); if it overflows, clip the dest to the pane edge and crop the
        // UV — shifted by the clamped pan so the visible window slides over
        // the texture instead of staying centered.
        let (dx, dw, uv_x0, uv_x1) = if raw_dw <= bw {
            (bx + (bw - raw_dw) * 0.5, raw_dw, 0.0_f32, 1.0_f32)
        } else {
            let max_off = (raw_dw - bw) * 0.5;
            let off = (pan_x * s).clamp(-max_off, max_off);
            let frac = (raw_dw - bw) / (2.0 * raw_dw);
            let d = off / raw_dw;
            (bx, bw, frac - d, 1.0 - frac - d)
        };
        let (dy, dh, uv_y0, uv_y1) = if raw_dh <= bh {
            (by + (bh - raw_dh) * 0.5, raw_dh, 0.0_f32, 1.0_f32)
        } else {
            let max_off = (raw_dh - bh) * 0.5;
            let off = (pan_y * s).clamp(-max_off, max_off);
            let frac = (raw_dh - bh) / (2.0 * raw_dh);
            let d = off / raw_dh;
            (by, bh, frac - d, 1.0 - frac - d)
        };
        self.image_quads.push((
            id.to_string(),
            CellInstance {
                cell_px: [dx, dy, dw, dh],
                uv_min: [uv_x0, uv_y0],
                uv_max: [uv_x1, uv_y1],
                fg_rgba: [1.0, 1.0, 1.0, 1.0],
                flags: CellInstance::FLAG_COLOR,
                ..Default::default()
            },
            self.chrome.len() as u32,
            self.cur_clip_phys(),
        ));
    }

    /// `queue_image` 의 cover-fit 바닥 배경 버전 — 박스를 꽉 채우고(fill) 넘치는
    /// 축은 UV 를 중앙 크롭한다. 이미지 패스(셀보다 먼저 그려짐)라 default-bg 셀
    /// 자리로 비친다 — agents/resume 피커의 교실 배경용. LOGICAL px.
    pub fn queue_image_cover(&mut self, id: &str, x: f32, y: f32, w: f32, h: f32) {
        let Some(entry) = self.images.get(id) else { return };
        let s = self.scale;
        let (bx, by, bw, bh) = (x * s, y * s, w * s, h * s);
        if bw <= 0.0 || bh <= 0.0 {
            return;
        }
        let (iw, ih) = (entry.w as f32, entry.h as f32);
        // cover: 박스를 덮는 최소 배율(둘 중 큰 쪽). no-upscale 캡을 두지 않는다 —
        // 배경은 살짝 확대돼 흐려도 빈틈 없이 채우는 게 맞다.
        let fit = (bw / iw).max(bh / ih);
        let (dw, dh) = (iw * fit, ih * fit);
        let uv_x0 = (1.0 - (bw / dw).min(1.0)) * 0.5;
        let uv_y0 = (1.0 - (bh / dh).min(1.0)) * 0.5;
        self.image_quads.push((
            id.to_string(),
            CellInstance {
                cell_px: [bx, by, bw, bh],
                uv_min: [uv_x0, uv_y0],
                uv_max: [1.0 - uv_x0, 1.0 - uv_y0],
                fg_rgba: [1.0, 1.0, 1.0, 1.0],
                flags: CellInstance::FLAG_COLOR,
                ..Default::default()
            },
            self.chrome.len() as u32,
            self.cur_clip_phys(),
        ));
    }

    /// `queue_image` 의 세로 클립 버전 — 박스가 pane 밖까지 이어질 때(스크롤로
    /// 잘린 학생 배너) contain-fit 결과를 클립 범위와 교차시키고 UV 를 같은
    /// 비율로 잘라, 스프라이트가 셀 스크롤과 함께 자연스럽게 잘려 나가게
    /// 한다. 클립이 박스를 다 덮으면 `queue_image(zoom=1)` 와 동일. LOGICAL px.
    pub fn queue_image_clipped(
        &mut self,
        id: &str,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        clip_y0: f32,
        clip_y1: f32,
    ) {
        let Some(entry) = self.images.get(id) else { return };
        let s = self.scale;
        let (bx, by, bw, bh) = (x * s, y * s, w * s, h * s);
        if bw <= 0.0 || bh <= 0.0 {
            return;
        }
        let (iw, ih) = (entry.w as f32, entry.h as f32);
        let fit = (bw / iw).min(bh / ih).min(1.0);
        let (dw, dh) = (iw * fit, ih * fit);
        let dx = bx + (bw - dw) * 0.5;
        let dy = by + (bh - dh) * 0.5;
        let top = dy.max(clip_y0 * s);
        let bot = (dy + dh).min(clip_y1 * s);
        if bot <= top {
            return;
        }
        let (uv_y0, uv_y1) = ((top - dy) / dh, (bot - dy) / dh);
        self.image_quads.push((
            id.to_string(),
            CellInstance {
                cell_px: [dx, top, dw, bot - top],
                uv_min: [0.0, uv_y0],
                uv_max: [1.0, uv_y1],
                fg_rgba: [1.0, 1.0, 1.0, 1.0],
                flags: CellInstance::FLAG_COLOR,
                ..Default::default()
            },
            self.chrome.len() as u32,
            self.cur_clip_phys(),
        ));
    }

    /// `queue_image` 의 전경(chrome 위) 버전 — icon 패스로 그려져 셀 글리프·
    /// rect 위에 뜬다(학생 걷기 도트 등 장식 스프라이트용). 박스 안 contain-fit
    /// 후 가로 중앙·**바닥 정렬**(발이 박스 바닥에 닿게). 좌표는 LOGICAL px.
    pub fn queue_image_above(&mut self, id: &str, x: f32, y: f32, w: f32, h: f32) {
        let Some(entry) = self.images.get(id) else { return };
        let s = self.scale;
        let (bx, by, bw, bh) = (x * s, y * s, w * s, h * s);
        if bw <= 0.0 || bh <= 0.0 {
            return;
        }
        let (iw, ih) = (entry.w as f32, entry.h as f32);
        let fit = (bw / iw).min(bh / ih);
        let dw = iw * fit;
        let dh = ih * fit;
        self.icon_quads.push((
            id.to_string(),
            CellInstance {
                cell_px: [bx + (bw - dw) * 0.5, by + (bh - dh), dw, dh],
                uv_min: [0.0, 0.0],
                uv_max: [1.0, 1.0],
                fg_rgba: [1.0, 1.0, 1.0, 1.0],
                flags: CellInstance::FLAG_COLOR,
                ..Default::default()
            },
            self.chrome.len() as u32,
            self.cur_clip_phys(),
        ));
    }

    /// 스왑체인이 지금 잡고 있는 물리 픽셀 크기. 창 크기와 어긋났는지 프레임마다
    /// 대조하는 자가치유용 — 어긋난 채로 두면 컴포지터가 그 작은 드로어블을 창
    /// 구석에 얹어 화면이 영구히 축소돼 처박힌다.
    pub fn surface_size(&self) -> (u32, u32) {
        (self.config.width, self.config.height)
    }

    pub fn resize(&mut self, w: u32, h: u32) {
        self.config.width = w.max(1);
        self.config.height = h.max(1);
        self.surface.configure(&self.device, &self.config);
        let dims = [self.config.width as f32, self.config.height as f32];
        let (gamma, contrast, sat) = text_render_knobs();
        self.pipeline.write_uniforms_full(
            &self.queue,
            dims,
            gamma,
            contrast,
            sat,
            self.p3_root_owned,
            0.0,
        );
        self.image_pipeline.write_uniforms_full(
            &self.queue,
            dims,
            gamma,
            contrast,
            sat,
            self.p3_root_owned,
            0.0,
        );
    }

    /// Render one frame. `panes` covers every pane the caller wants
    /// drawn this frame, each carrying its grid + pixel origin. The
    /// pipeline gathers all instances into one draw call regardless
    /// of pane count.
    /// Push cells from each pane onto the chrome instance list at
    /// the *current* z-order. Caller pushes background rects before
    /// this and overlays (cursor, selection, preedit) after. The
    /// pipeline draws everything in insertion order, so painting
    /// layers fall out naturally from the call sequence.
    pub fn draw_cells(&mut self, panes: &[PaneSlot<'_>]) {
        DRAW_CELLS_CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        // The URL currently under the mouse renders in this blue — both its
        // glyphs (Pass 2) and its underline (Pass 3) — so a hovered link reads
        // like a hyperlink. `pane.links` holds the 0..1 hovered range.
        const LINK_BLUE: [u8; 4] = [0x0a, 0x84, 0xff, 0xff];
        // Glyph alpha for unfocused panes (PaneSlot.dim). Backgrounds keep
        // full alpha — only the text fades, so the box doesn't darken.
        const DIM_TEXT_ALPHA: f32 = 0.70;
        // Pass 1: backgrounds only. A tall CJK glyph bleeds a little
        // into the row below; emitting EVERY background first stops the
        // next row's bg fill from painting over the previous glyph's
        // bottom half. That over-paint was clipping Hangul in claude's
        // input-echo row (a run of reverse/bg cells); claude's normal
        // output rows have no bg below them, so they rendered fine.
        // (Reverse-video spaces still fill here — claude's cursor is an
        // inverse space, "띄어쓰기 커서".)
        for pane in panes {
            // Per-pane cell size: base metric × this pane's font multiplier.
            let cell_w_px = self.cell_w * self.scale * pane.font_scale;
            let cell_h_px = self.cell_h * self.scale * pane.font_scale;
            for (r, row) in pane.rows.iter().enumerate() {
                for (col, cell) in row.iter().enumerate() {
                    let want_bg = !matches!(cell.bg, kasa_bridge::screen::Color::Default)
                        || cell.inverse;
                    let bg = cell_bg_rgba(cell, pane.default_fg);
                    if want_bg && bg[3] > 0 {
                        let cx = pane.origin_px.0 + col as f32 * cell_w_px;
                        let cy = pane.origin_px.1 + r as f32 * cell_h_px;
                        self.chrome.push(CellInstance {
                            cell_px: [cx, cy, cell_w_px, cell_h_px],
                            uv_min: Atlas::SOLID_UV,
                            uv_max: Atlas::SOLID_UV,
                            fg_rgba: srgb_rgba_to_linear(bg),
                            ..Default::default()
                        });
                    }
                }
            }
        }
        // Pass 2: glyphs, drawn over every background.
        for pane in panes {
            let cell_w_px = self.cell_w * self.scale * pane.font_scale;
            let cell_h_px = self.cell_h * self.scale * pane.font_scale;
            let pane_size_px = ((self.font_size_px as f32 * pane.font_scale).round() as u32).max(8);
            for (r, row) in pane.rows.iter().enumerate() {
                probe_cell_row(r, row, pane.dim, pane.font_scale);
                // Right edge of the last glyph painted on this row. A blank
                // cell an oversized neighbour already spilled into is not free
                // room any more, so `fit_cell_glyph` is told about it.
                let mut glyph_right = f32::NEG_INFINITY;
                for (col, cell) in row.iter().enumerate() {
                    // Blanks contribute no glyph.
                    let ch = cell.ch;
                    if ch == ' ' || ch == '\0' {
                        continue;
                    }
                    // SGR 8(conceal) — 배경은 위 패스에서 칠했고 글리프만 생략.
                    // statusline 의 세션 id 마커가 이 플래그로 화면에서 숨는다.
                    if cell.hidden {
                        continue;
                    }
                    // Block Elements (U+2580..259F) — paint as GPU
                    // quads instead of font glyphs. Monospace fonts
                    // render these with seams/gaps, so claude code's
                    // pixel-art character (built from half/quadrant
                    // blocks) tears when shaped as glyphs. The
                    // sub-cell rects from cells::block_rects fill the
                    // exact regions seamlessly.
                    {
                        if let Some(rects) = crate::cells::block_rects(ch) {
                            let mut fg = cell_fg_rgba(cell, pane.default_fg);
                            if pane.dim {
                                fg[3] = (fg[3] as f32 * DIM_TEXT_ALPHA) as u8;
                            }
                            let lin = srgb_rgba_to_linear(fg);
                            let cx = pane.origin_px.0 + col as f32 * cell_w_px;
                            let cy = pane.origin_px.1 + r as f32 * cell_h_px;
                            for &(x0, y0, x1, y1, alpha) in rects {
                                let mut c = lin;
                                c[3] *= alpha;
                                self.chrome.push(CellInstance {
                                    cell_px: [
                                        cx + x0 * cell_w_px,
                                        cy + y0 * cell_h_px,
                                        (x1 - x0) * cell_w_px,
                                        (y1 - y0) * cell_h_px,
                                    ],
                                    uv_min: Atlas::SOLID_UV,
                                    uv_max: Atlas::SOLID_UV,
                                    fg_rgba: c,
                                    ..Default::default()
                                });
                            }
                            continue;
                        }
                    }
                    let cell_x = pane.origin_px.0 + col as f32 * cell_w_px;
                    let cell_y = pane.origin_px.1 + r as f32 * cell_h_px;
                    let mut fg = cell_fg_rgba(cell, pane.default_fg);
                    if pane.links.iter().any(|l| {
                        l.row as usize == r
                            && (col as u16) >= l.col_start
                            && (col as u16) < l.col_end
                    }) {
                        fg = LINK_BLUE;
                    }
                    if pane.dim {
                        fg[3] = (fg[3] as f32 * DIM_TEXT_ALPHA) as u8;
                    }
                    let icon = is_icon_codepoint(ch as u32);
                    if icon {
                        // Ghostty-style fit-to-cell, done CRISP: scale
                        // happens at raster time, never on a finished
                        // bitmap. Two-pass — probe-bake at cell height
                        // to read the glyph's natural bbox, compute the
                        // size that lands the bbox at ~0.82 of the cell
                        // height, then re-bake natively at that size.
                        // Both bakes are atlas-cached so it's one-time
                        // per glyph. The final bitmap is sharp because
                        // swash rasterized the outline at the target
                        // size directly.
                        let target_h = cell_h_px * 0.82;
                        let probe_size = cell_h_px.round().max(1.0) as u32;
                        let probe = self.atlas.get_or_bake(
                            &self.device,
                            &self.queue,
                            &mut self.shaper,
                            GlyphKey { ch, bold: cell.bold, italic: cell.italic, size_px: probe_size, font: 0 },
                        );
                        if let Some(p) = probe {
                            if p.px_h > 0 {
                                let mut final_size =
                                    (probe_size as f32 * target_h / p.px_h as f32).round();
                                // Guard the width: scale down if the
                                // glyph would exceed ~1.9 cells.
                                let projected_w = p.px_w as f32
                                    * (final_size / probe_size as f32);
                                let max_w = cell_w_px * 1.9;
                                if projected_w > max_w {
                                    final_size *= max_w / projected_w;
                                }
                                let final_size = (final_size.round() as u32).max(1);
                                let entry = self.atlas.get_or_bake(
                                    &self.device,
                                    &self.queue,
                                    &mut self.shaper,
                                    GlyphKey { ch, bold: cell.bold, italic: cell.italic, size_px: final_size, font: 0 },
                                );
                                if let Some(e) = entry {
                                    let x = cell_x + (cell_w_px - e.px_w as f32) * 0.5;
                                    let y = cell_y + (cell_h_px - e.px_h as f32) * 0.5;
                                    self.chrome.push(CellInstance {
                                        cell_px: [x, y, e.px_w as f32, e.px_h as f32],
                                        uv_min: e.uv_min,
                                        uv_max: e.uv_max,
                                        fg_rgba: srgb_rgba_to_linear(fg),
                                        ..Default::default()
                                    });
                                }
                            }
                        }
                        continue;
                    }
                    let key = GlyphKey {
                        ch,
                        bold: cell.bold,
                        italic: cell.italic,
                        size_px: pane_size_px,
                        font: 0,
                    };
                    let Some(entry) = self.atlas.get_or_bake(
                        &self.device,
                        &self.queue,
                        &mut self.shaper,
                        key,
                    ) else {
                        continue;
                    };
                    let baseline_y = cell_y + cell_h_px * 0.78;
                    if entry.is_color {
                        // Color emoji: the atlas holds a verbatim RGBA
                        // bitmap. Fit it into a 2-cell box (emoji read as
                        // full-width) keeping aspect, never upscaling past
                        // native, and center it in the row. FLAG_COLOR
                        // tells the shader to sample the texture directly
                        // instead of fg × coverage.
                        let span_w = cell_w_px * 2.0;
                        let gw0 = entry.px_w as f32;
                        let gh0 = entry.px_h as f32;
                        let fit = (span_w / gw0).min(cell_h_px / gh0).min(1.0);
                        let gw = gw0 * fit;
                        let gh = gh0 * fit;
                        let x = cell_x + (span_w - gw) * 0.5;
                        let y = cell_y + (cell_h_px - gh) * 0.5;
                        glyph_right = x + gw;
                        self.chrome.push(CellInstance {
                            cell_px: [x, y, gw, gh],
                            uv_min: entry.uv_min,
                            uv_max: entry.uv_max,
                            fg_rgba: srgb_rgba_to_linear(fg),
                            flags: CellInstance::FLAG_COLOR,
                            ..Default::default()
                        });
                        continue;
                    }
                    if is_wide_char(ch) {
                        // Fit the glyph into its 2-cell box. Scale down
                        // (keeping aspect) only if it overflows, then
                        // center horizontally so syllables sit on the
                        // grid instead of bleeding into the next cell.
                        let span_w = cell_w_px * 2.0;
                        let gw0 = entry.px_w as f32;
                        let scale_fit = if gw0 > span_w { span_w / gw0 } else { 1.0 };
                        let gw = gw0 * scale_fit;
                        let gh = entry.px_h as f32 * scale_fit;
                        let x = cell_x + (span_w - gw) * 0.5;
                        let y = baseline_y - entry.bearing_y as f32 * scale_fit;
                        glyph_right = x + gw;
                        self.chrome.push(CellInstance {
                            cell_px: [x, y, gw, gh],
                            uv_min: entry.uv_min,
                            uv_max: entry.uv_max,
                            fg_rgba: srgb_rgba_to_linear(fg),
                            ..Default::default()
                        });
                    } else {
                        // An oversized ambiguous-width glyph is slid into the
                        // blank columns beside it. A column counts as free only
                        // when it really exists — past the row's end there is
                        // nothing to borrow and the spill would cross into the
                        // neighbouring pane — and only up to where the previous
                        // glyph already reaches, so `① ②잠금` cannot have the ②
                        // slide left onto the ①.
                        let room_right = match row.get(col + 1) {
                            Some(n) if matches!(n.ch, ' ' | '\0') => cell_w_px,
                            _ => 0.0,
                        };
                        let room_left = match col.checked_sub(1).and_then(|c| row.get(c)) {
                            Some(p) if matches!(p.ch, ' ' | '\0') => {
                                cell_w_px.min((cell_x - glyph_right).max(0.0))
                            }
                            _ => 0.0,
                        };
                        let rect = fit_cell_glyph(
                            &entry, cell_x, baseline_y, cell_w_px, room_left, room_right,
                        );
                        glyph_right = rect[0] + rect[2];
                        self.chrome.push(CellInstance {
                            cell_px: rect,
                            uv_min: entry.uv_min,
                            uv_max: entry.uv_max,
                            fg_rgba: srgb_rgba_to_linear(fg),
                            ..Default::default()
                        });
                    }
                }
            }
        }
        // Pass 3: blue underline beneath the URL currently under the mouse —
        // the hover hyperlink affordance (links are bare text until hovered).
        // The event handler flips the cursor to a pointer over the same range
        // and opens it on click. `pane.links` holds 0..1 ranges (the hovered
        // one), filled in render_frame_gpu from the live cursor position.
        let link_rgba = LINK_BLUE;
        for pane in panes {
            if pane.links.is_empty() {
                continue;
            }
            let cell_w_px = self.cell_w * self.scale * pane.font_scale;
            let cell_h_px = self.cell_h * self.scale * pane.font_scale;
            let thick = (cell_h_px * 0.06).max(1.0);
            let mut col = link_rgba;
            if pane.dim {
                col[3] = (col[3] as f32 * DIM_TEXT_ALPHA) as u8;
            }
            let lin = srgb_rgba_to_linear(col);
            for link in &pane.links {
                let x = pane.origin_px.0 + link.col_start as f32 * cell_w_px;
                let y = pane.origin_px.1 + (link.row as f32 + 1.0) * cell_h_px - thick - 1.0;
                let w = (link.col_end - link.col_start) as f32 * cell_w_px;
                self.chrome.push(CellInstance {
                    cell_px: [x, y, w, thick],
                    uv_min: Atlas::SOLID_UV,
                    uv_max: Atlas::SOLID_UV,
                    fg_rgba: lin,
                    ..Default::default()
                });
            }
        }
    }

    pub fn render(
        &mut self,
        _panes: &[PaneSlot<'_>],
        _scale: f32,
        time_secs: f32,
        chrome_changed: bool,
    ) -> Result<usize> {
        // Re-apply P3 colorspace before every drawable. wgpu's Metal HAL
        // doesn't touch this, but in practice the byte we read off the
        // panel ended up matching plain sRGB (255,0,0 measured as
        // 255,0,0 not the P3-wider 234,51,35 ghostty produces). Setting
        // it once at init wasn't enough on macOS 26.3 — possibly because
        // the layer's pixelFormat / drawableSize reconfig drops the tag.
        // Setting it every frame is cheap (one selector call) and keeps
        // the wider gamut sticky frame-to-frame.
        #[cfg(target_os = "macos")]
        if !self.p3_root_owned {
            // Legacy `RawHandle` path: wgpu owns the layer and creates it as
            // a sublayer that macOS won't color-manage. Re-apply / re-promote
            // every frame as a (mostly ineffective) workaround.
            apply_p3_via_hal(&self.surface);
            unsafe {
                reapply_p3(self._window.as_ref());
                promote_metal_layer_to_root(self._window.as_ref(), &self.surface);
            }
        } else {
            // P3_ROOT mode: we own the metal layer, but wgpu's
            // `surface.configure()` calls `setPixelFormat` / `setDevice` on
            // it which can quietly drop the Display P3 tag. Re-apply via the
            // hal handle every frame — same cheap setColorspace selector as
            // `apply_p3_via_hal`, just targeting the layer wgpu now reports
            // (which IS our root layer in this mode).
            apply_p3_via_hal(&self.surface);
        }
        // Advance the working-bar sweep on the GPU every present (cheap
        // offset write). When chrome is unchanged — a bar-only frame while a
        // pane is busy — skip re-uploading the instance buffers entirely, so a
        // working pane costs one uniform write + the draw, not a full chrome
        // rebuild. The cached instance buffer redraws as-is.
        self.pipeline.write_time(&self.queue, time_secs);
        let instance_count = self.chrome.len();
        let n_img = self.image_quads.len();
        if chrome_changed {
            self.pipeline
                .write_instances(&self.device, &self.queue, &self.chrome);
            // Upload this frame's image + icon quads (images first, icons
            // appended) into one buffer. Buffer position is just storage —
            // the draw order comes from the watermarks below.
            if !self.image_quads.is_empty() || !self.icon_quads.is_empty() {
                let all_instances: Vec<CellInstance> = self
                    .image_quads
                    .iter()
                    .chain(self.icon_quads.iter())
                    .map(|(_, inst, ..)| *inst)
                    .collect();
                self.image_pipeline
                    .write_instances(&self.device, &self.queue, &all_instances);
            }
        }
        let frame = self.surface.get_current_texture()?;
        let view = frame.texture.create_view(&Default::default());
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("kasaterm gpu encoder"),
            });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("kasaterm gpu pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear({
                            // Raw sRGB bytes → non-sRGB target = shown
                            // verbatim, matching cells::default_bg().
                            let b = crate::cells::default_bg();
                            wgpu::Color {
                                r: b[0] as f64 / 255.0,
                                g: b[1] as f64 / 255.0,
                                b: b[2] as f64 / 255.0,
                                a: 1.0,
                            }
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            // Chrome is one big instance buffer, but images and icons each
            // need their own texture bound, so they can't ride in it. Instead
            // of giving them fixed layers under/over the whole chrome pass, we
            // cut the chrome at each quad's watermark and drop the quad in
            // there — an icon queued before a panel ends up under that panel,
            // and one queued after ends up on top, which is what every caller
            // already assumes when it draws a cover over something.
            let mut ordered: Vec<(u32, u32, &String, Option<[u32; 4]>)> = self
                .image_quads
                .iter()
                .enumerate()
                .map(|(i, (id, _, wm, clip))| (*wm, i as u32, id, *clip))
                .chain(
                    self.icon_quads
                        .iter()
                        .enumerate()
                        .map(|(j, (id, _, wm, clip))| (*wm, (n_img + j) as u32, id, *clip)),
                )
                .collect();
            // Stable so quads with the same watermark keep queue order — that
            // is the case for an icon drawn right on top of an image.
            ordered.sort_by_key(|(wm, ..)| *wm);
            // 클립 세그먼트. `clip_runs` 는 `(chrome 인덱스, 그 뒤로 유효한 클립)` 이
            // 오름차순으로 들어 있고, 여기서 그 경계마다 chrome 패스를 끊어
            // `set_scissor_rect` 를 갈아 끼운다.
            //
            // ⚠️ fail-open: 스택이 안 닫힌 채로 프레임이 끝났으면 run 을 통째로
            // 버린다. 최악이 「오늘과 똑같은 그림」이어야지 「화면 절반 실종」이면
            // 안 된다 — 클립 하나 안 닫은 실수가 앱을 못 쓰게 만들면 안 된다.
            let runs: &[(u32, Option<[u32; 4]>)] = if self.clip_stack.is_empty() {
                &self.clip_runs
            } else {
                eprintln!(
                    "[clip] 프레임이 끝났는데 클립 {}개가 안 닫혔다 — 이 프레임은 클립 없이 그린다",
                    self.clip_stack.len()
                );
                &[]
            };
            let (full_w, full_h) = (self.config.width, self.config.height);
            let mut drawn = 0u32;
            let mut run_i = 0usize;
            let mut cur: Option<[u32; 4]> = None;
            // 한 세그먼트를 그리기 직전마다 시저를 **매번** 정한다. 「바뀔 때만
            // 세운다」로 하면 한 번 세운 사각형이 되돌려지지 않아 그 뒤 전부가
            // 거기 갇히는 사고가 난다.
            macro_rules! scissor {
                ($c:expr) => {
                    match $c {
                        Some([x, y, w, h]) => pass.set_scissor_rect(x, y, w, h),
                        None => pass.set_scissor_rect(0, 0, full_w, full_h),
                    }
                };
            }
            macro_rules! flush_chrome {
                ($upto:expr) => {{
                    let upto: u32 = $upto;
                    while drawn < upto {
                        while run_i < runs.len() && runs[run_i].0 <= drawn {
                            cur = runs[run_i].1;
                            run_i += 1;
                        }
                        // 다음 경계까지가 이번 세그먼트. 위에서 `<= drawn` 을 다
                        // 소비했으므로 남은 run 의 인덱스는 반드시 `drawn` 보다 커
                        // 세그먼트가 비지 않는다(무한루프 방지).
                        let seg_end =
                            runs.get(run_i).map(|(i, _)| (*i).min(upto)).unwrap_or(upto);
                        // 빈 클립은 그리기 자체를 건너뛴다 — 0 크기 시저를 세우느니
                        // draw call 을 안 내는 편이 싸고 검증도 명확하다.
                        if !matches!(cur, Some([_, _, 0, _]) | Some([_, _, _, 0])) {
                            scissor!(cur);
                            self.pipeline
                                .draw_range(&mut pass, &self.bind_group, drawn, seg_end);
                        }
                        drawn = seg_end;
                    }
                }};
            }
            for (wm, buf_idx, id, qclip) in ordered {
                flush_chrome!(wm.min(instance_count as u32));
                if let Some(entry) = self.images.get(id) {
                    // 이미지·아이콘은 자기가 큐잉될 때의 클립을 들고 다닌다.
                    // chrome 인덱스로 되짚으면 같은 워터마크에 여러 run 이 붙었을 때
                    // 어느 쪽인지 못 가른다.
                    if !matches!(qclip, Some([_, _, 0, _]) | Some([_, _, _, 0])) {
                        scissor!(qclip);
                        self.image_pipeline
                            .draw_at(&mut pass, &entry.bind_group, buf_idx);
                    }
                }
            }
            flush_chrome!(instance_count as u32);
            if std::env::var_os("KASATERM_CLIP_DEBUG").is_some() {
                eprintln!(
                    "[clip] 인스턴스 {instance_count} · 세그먼트 {} · runs {:?}",
                    runs.len() + 1,
                    runs
                );
            }
        }
        // Self-capture: copy the just-rendered frame into a buffer before
        // present, then read it back to a PNG. No OS screen-record permission
        // needed (screencapture is blocked in headless runs).
        let capture = self.capture_next.take();
        let cap = if capture.is_some() {
            let w = self.config.width;
            let h = self.config.height;
            let bpr = w.div_ceil(64) * 256; // align(w*4, 256)
            let buf = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("capture readback"),
                size: (bpr * h) as u64,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            });
            encoder.copy_texture_to_buffer(
                wgpu::TexelCopyTextureInfo {
                    texture: &frame.texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::TexelCopyBufferInfo {
                    buffer: &buf,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(bpr),
                        rows_per_image: Some(h),
                    },
                },
                wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            );
            Some((buf, w, h, bpr))
        } else {
            None
        };
        self.queue.submit(Some(encoder.finish()));
        frame.present();
        if let (Some(path), Some((buf, w, h, bpr))) = (capture, cap) {
            buf.slice(..).map_async(wgpu::MapMode::Read, |_| {});
            let _ = self.device.poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: None,
            });
            let bgra = matches!(
                self.config.format,
                wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Bgra8UnormSrgb
            );
            // 크롭은 GPU 가 아니라 여기서 한다. copy_texture_to_buffer 의 origin 을
            // 옮기면 bytes_per_row 256 정렬을 잘린 폭 기준으로 다시 맞춰야 하는데,
            // 캡처는 드문 연산이라 전체를 읽고 잘라 내는 편이 훨씬 단순하다.
            let (cx, cy, cw, chh) = match self.capture_crop.take() {
                Some((x, y, cw, ch)) => (
                    x.min(w.saturating_sub(1)),
                    y.min(h.saturating_sub(1)),
                    cw.min(w.saturating_sub(x)).max(1),
                    ch.min(h.saturating_sub(y)).max(1),
                ),
                None => (0, 0, w, h),
            };
            let max_w = std::mem::take(&mut self.capture_max_w);
            {
                let data = buf.slice(..).get_mapped_range();
                let mut rgba = Vec::with_capacity((cw * chh * 4) as usize);
                for row in cy..cy + chh {
                    let s = (row * bpr + cx * 4) as usize;
                    let line = &data[s..s + (cw * 4) as usize];
                    for px in line.chunks_exact(4) {
                        if bgra {
                            rgba.extend_from_slice(&[px[2], px[1], px[0], 0xFF]);
                        } else {
                            rgba.extend_from_slice(&[px[0], px[1], px[2], 0xFF]);
                        }
                    }
                }
                let saved = if max_w > 0 && cw > max_w {
                    let nh = ((chh as u64 * max_w as u64) / cw as u64).max(1) as u32;
                    match image::RgbaImage::from_raw(cw, chh, rgba.clone()) {
                        Some(img) => {
                            let small = image::imageops::resize(
                                &img,
                                max_w,
                                nh,
                                image::imageops::FilterType::Lanczos3,
                            );
                            save_rgba_png(&path, small.as_raw(), max_w, nh).map(|()| (max_w, nh))
                        }
                        None => save_rgba_png(&path, &rgba, cw, chh).map(|()| (cw, chh)),
                    }
                } else {
                    save_rgba_png(&path, &rgba, cw, chh).map(|()| (cw, chh))
                };
                match saved {
                    Ok((ow, oh)) => eprintln!("[capture] gpu readback → {path} ({ow}x{oh})"),
                    Err(e) => eprintln!("[capture] gpu png failed: {e}"),
                }
            }
            buf.unmap();
        }
        Ok(instance_count)
    }
}

/// Encode RGBA8 pixels to a PNG file. Used by the GPU self-capture path. Uses
/// the `image` crate (available on every target; `png` is Windows-only here).
pub(crate) fn save_rgba_png(path: &str, rgba: &[u8], w: u32, h: u32) -> std::io::Result<()> {
    image::save_buffer(path, rgba, w, h, image::ExtendedColorType::Rgba8)
        .map_err(|e| std::io::Error::other(e.to_string()))
}

/// Place a **single-column** glyph, keeping its natural size whenever the
/// columns beside it leave room. `room_left`/`room_right` are how many px of
/// blank the caller is willing to lend. Returns `[x, y, w, h]` in physical px.
///
/// East Asian **Ambiguous** codepoints (①②③, Ⅰ Ⅱ, ⓐ …) are counted as one
/// column by the grid — `unicode_width` says 1 and alacritty agrees — but no
/// monospace face in the chain carries them, so the fallback that does is a CJK
/// font that rasters them **full-width** (measured: D2Coding gives `①` exactly
/// 2× the advance of `A`). Drawn unguarded they ran over their neighbour:
/// `⑤브릿지` had the ⑤ sitting on top of the 브.
///
/// Widening the grid instead (adding the block to `is_wide_char`) is the wrong
/// half of the fix — the parser still advances one column, so every glyph after
/// it would sit a cell off. Shrinking to one cell is the other wrong fix: a lone
/// stunted ⑤ beside full-size ①②③④ reads worse than the overlap did.
///
/// So the glyph is *slid* rather than resized. It holds its natural position
/// until it would spill onto a real character, then backs off into whatever
/// blank the caller lent — `판정 ⑤브릿지` borrows the space to its left, `① ② ③`
/// the one to its right. Only a glyph boxed in on both sides scales, and it
/// keeps its aspect ratio. Glyphs that fit their own cell take the untouched
/// bearing path, so ASCII and every well-behaved monospace glyph render exactly
/// as before.
fn fit_cell_glyph(
    entry: &AtlasEntry,
    cell_x: f32,
    baseline_y: f32,
    cell_w: f32,
    room_left: f32,
    room_right: f32,
) -> [f32; 4] {
    let gw = entry.px_w as f32;
    let y = baseline_y - entry.bearing_y as f32;
    if gw <= cell_w {
        return [cell_x + entry.bearing_x as f32, y, gw, entry.px_h as f32];
    }
    let lo = cell_x - room_left;
    let hi = cell_x + cell_w + room_right;
    if gw <= hi - lo {
        let x = (cell_x + entry.bearing_x as f32).min(hi - gw).max(lo);
        return [x, y, gw, entry.px_h as f32];
    }
    // 양쪽이 막혀도 절반까지 쪼그라들게 두지는 않는다. 자리에 정확히 맞춰
    // 줄이면 `①②잠금` 의 ② 가 아래첨자처럼 혼자 작아지는데, 그 모습은
    // 겹침보다 못생겼다는 판정이 이미 났다("크기비율그대로 수정안돼?").
    // 하한에서 남는 넘침은 빌린 자리 한가운데를 기준으로 좌우 반씩 흘린다 —
    // monospace 이웃은 잉크가 칸을 꽉 채우지 않아 그 여백이 대부분을 먹고,
    // 한쪽으로 몰아줄 때처럼 이웃 글자를 파고들지 않는다.
    // 상한을 비율이 아니라 **넘침 폭**으로 건다: 비율 하한(예: 0.78배)을
    // 두면 3칸짜리 폴백 글리프가 2.3칸으로 앉아 이웃을 통째로 덮는다.
    // 실측 — 원문자 잉크는 1.79칸이라 이 상한에서 0.78배로 앉는다(절반보다
    // 훨씬 크다). 병적으로 넓은 글리프만 계속 크게 줄어든다.
    let gwf = gw.min(hi - lo + cell_w * 0.4);
    let fit = gwf / gw;
    [
        (lo + hi - gwf) * 0.5,
        baseline_y - entry.bearing_y as f32 * fit,
        gwf,
        entry.px_h as f32 * fit,
    ]
}

/// Nerd Font / symbol icon codepoint ranges that should be scaled to
/// fill the cell rather than rendered at the text font size. Mirrors
/// the ranges Ghostty constrains: BMP Private Use Area (where most
/// Nerd Font icons live), both supplementary PUA planes, and the
/// Misc-Technical block that carries powerline-adjacent symbols.
/// East Asian Wide / Fullwidth — these occupy two terminal cells.
/// alacritty fills the right half with an empty cell (skipped in the
/// glyph pass), so the glyph itself has to be fit into a 2-cell box.
/// The bundled Hangul fallback font rasters at its own natural advance,
/// which does not match the primary monospace font's `cell_w`; without
/// this the syllable drifts into / overlaps its neighbour ("출력 한글
/// 깨짐"). sugarloaf gets this for free because cosmic_text shapes onto
/// the monospace grid.
/// 편집기 격자에서 이 텍스트가 차지하는 칸 수. 캐럿 x·선택 밴드·괄호 강조·
/// 들여쓰기 가이드가 전부 이 하나로 좌표를 얻으므로, 그리는 쪽
/// (`draw_editor_cells`)과 칸 세는 규칙이 갈릴 수 없다.
pub(crate) fn cell_cols(text: &str) -> usize {
    text.chars()
        .map(|c| 1 + usize::from(is_wide_char(c)))
        .sum()
}

/// LSP severity → 색. 1=error 2=warning 3=information 4=hint.
pub(crate) fn diag_color(severity: u8) -> [u8; 4] {
    match severity {
        1 => crate::theme::danger(),
        2 => crate::theme::syn_type(),
        3 => crate::theme::accent(),
        _ => crate::theme::text_mute(),
    }
}

pub(crate) fn is_wide_char(ch: char) -> bool {
    let cp = ch as u32;
    matches!(cp,
        0x1100..=0x115F        // Hangul Jamo
        | 0x2E80..=0x303E      // CJK Radicals, Kangxi, CJK Symbols
        | 0x3041..=0x33FF      // Kana, CJK enclosed/compat
        | 0x3400..=0x4DBF      // CJK Ext A
        | 0x4E00..=0x9FFF      // CJK Unified
        | 0xA000..=0xA4CF      // Yi
        | 0xAC00..=0xD7A3      // Hangul Syllables
        | 0xF900..=0xFAFF      // CJK Compatibility Ideographs
        | 0xFE30..=0xFE4F      // CJK Compatibility Forms
        | 0xFF00..=0xFF60      // Fullwidth Forms
        | 0xFFE0..=0xFFE6      // Fullwidth signs
    ) || cp >= 0x20000          // CJK Ext B and beyond
}

/// 낱말 중심점이 선택 범위 안인지. 읽는 순서(줄 → 가로) 비교라 사전식이면
/// 충분하다 — 같은 줄에 놓인 낱말들은 같은 `pen_y` 를 쓰므로 y 가 정확히 같고,
/// 줄이 다르면 y 가 줄 높이만큼 벌어져 저절로 갈린다. 좌표는 화면 로컬 px.
pub(crate) fn word_in_sel(sel: Option<(f32, f32, f32, f32)>, cx: f32, cy: f32) -> bool {
    let Some((ax, ay, bx, by)) = sel else { return false };
    // 드래그는 위로도 아래로도 가므로 먼저 읽는 순서로 세운다.
    let (s, e) = if (ay, ax) <= (by, bx) {
        ((ax, ay), (bx, by))
    } else {
        ((bx, by), (ax, ay))
    };
    (cy, cx) >= (s.1, s.0) && (cy, cx) <= (e.1, e.0)
}

/// Link tint by destination kind, so links read as varied rather than one
/// flat blue: web=accent blue, local file=green, mailto=purple, anchor=cyan.
fn link_color(dest: &str) -> [u8; 4] {
    if dest.starts_with("http://") || dest.starts_with("https://") {
        crate::theme::accent()
    } else if dest.starts_with("wiki:") {
        // 문서 사이 링크는 본문색 그대로 두고 밑줄로만 알린다. 색을 주면 인덱스처럼
        // 링크가 줄마다 있는 문서가 통째로 물들고, 인라인 코드 칩과도 색이 섞여
        // 무엇이 코드고 무엇이 링크인지 안 읽힌다. 밖으로 나가는 링크만 색을 쓴다.
        crate::theme::text()
    } else if dest.starts_with("mailto:") {
        crate::theme::syn_keyword()
    } else if dest.starts_with('#') {
        crate::theme::syn_function()
    } else {
        crate::theme::syn_string()
    }
}

/// Language keyword set for code-block syntax highlighting. Coarse on purpose
/// — a lightweight lexer, not a full grammar; the goal is colorful, readable
/// code, not perfect parsing.
fn syn_keywords(lang: &str) -> &'static [&'static str] {
    match lang.to_ascii_lowercase().as_str() {
        "rust" | "rs" => &[
            "fn", "let", "mut", "if", "else", "match", "for", "while", "loop", "return",
            "struct", "enum", "impl", "trait", "pub", "use", "mod", "self", "Self", "as",
            "const", "static", "ref", "move", "dyn", "where", "async", "await", "break",
            "continue", "in", "type", "unsafe", "crate", "super", "true", "false",
        ],
        "bash" | "sh" | "shell" | "zsh" | "fish" => &[
            "if", "then", "else", "elif", "fi", "for", "do", "done", "while", "case", "esac",
            "function", "echo", "export", "local", "return", "in", "set", "unset", "source",
            "alias", "cd", "exit", "read", "select", "until",
        ],
        "js" | "javascript" | "ts" | "typescript" | "jsx" | "tsx" => &[
            "function", "const", "let", "var", "if", "else", "for", "while", "return",
            "class", "new", "import", "export", "from", "async", "await", "try", "catch",
            "finally", "throw", "typeof", "instanceof", "this", "super", "extends", "switch",
            "case", "break", "continue", "default", "null", "undefined", "true", "false",
            "void", "yield", "interface", "type", "enum",
        ],
        "py" | "python" => &[
            "def", "class", "if", "elif", "else", "for", "while", "return", "import", "from",
            "as", "with", "try", "except", "finally", "raise", "lambda", "yield", "pass",
            "break", "continue", "in", "is", "not", "and", "or", "None", "True", "False",
            "global", "nonlocal", "async", "await",
        ],
        "go" | "golang" => &[
            "func", "var", "const", "if", "else", "for", "range", "return", "struct",
            "interface", "type", "package", "import", "go", "defer", "chan", "map", "select",
            "switch", "case", "break", "continue", "default", "nil", "true", "false",
        ],
        "c" | "cpp" | "c++" | "h" | "hpp" => &[
            "int", "char", "float", "double", "void", "if", "else", "for", "while", "return",
            "struct", "enum", "union", "typedef", "const", "static", "sizeof", "switch",
            "case", "break", "continue", "default", "unsigned", "signed", "long", "short",
            "class", "public", "private", "protected", "new", "delete", "true", "false",
            "nullptr", "namespace", "template", "auto",
        ],
        "json" => &["true", "false", "null"],
        _ => &[
            "if", "else", "for", "while", "return", "function", "fn", "def", "class",
            "import", "const", "let", "var", "true", "false", "null",
        ],
    }
}

/// Line-comment prefix(es) for a language.
fn syn_line_comment(lang: &str) -> &'static [&'static str] {
    match lang.to_ascii_lowercase().as_str() {
        "bash" | "sh" | "shell" | "zsh" | "fish" | "py" | "python" | "yaml" | "yml" | "toml"
        | "rb" | "ruby" | "r" => &["#"],
        "lua" | "sql" | "hs" | "haskell" => &["--"],
        _ => &["//"],
    }
}

/// Tokenize one code line into (text, color) runs for syntax highlighting.
/// A small hand-rolled lexer: comments, strings, numbers, keywords, type-ish
/// (Capitalized) and call-ish (`name(`) identifiers; everything else uses
/// `base` — code blocks pass TEXT_DIM (light SURFACE bg), inline code passes
/// the brighter TEXT (its chip is darker, so dim plain text reads as black).
pub(crate) fn highlight_code_line(line: &str, lang: &str, base: [u8; 4]) -> Vec<(String, [u8; 4])> {
    use crate::theme;
    let kws = syn_keywords(lang);
    let comments = syn_line_comment(lang);
    let ch: Vec<char> = line.chars().collect();
    let n = ch.len();
    let mut out: Vec<(String, [u8; 4])> = Vec::new();
    let starts_comment = |i: usize| -> bool {
        comments
            .iter()
            .any(|cm| ch[i..].iter().take(cm.chars().count()).collect::<String>() == **cm)
    };
    let mut i = 0;
    while i < n {
        let c = ch[i];
        if starts_comment(i) {
            out.push((ch[i..].iter().collect(), theme::syn_comment()));
            break;
        }
        if c == '"' || c == '\'' || c == '`' {
            let q = c;
            let mut j = i + 1;
            while j < n {
                if ch[j] == '\\' {
                    j += 2;
                    continue;
                }
                if ch[j] == q {
                    j += 1;
                    break;
                }
                j += 1;
            }
            let j = j.min(n);
            out.push((ch[i..j].iter().collect(), theme::syn_string()));
            i = j;
            continue;
        }
        if c.is_ascii_digit() {
            let mut j = i;
            while j < n && (ch[j].is_ascii_alphanumeric() || ch[j] == '.' || ch[j] == '_') {
                j += 1;
            }
            out.push((ch[i..j].iter().collect(), theme::syn_number()));
            i = j;
            continue;
        }
        if c.is_alphabetic() || c == '_' {
            let mut j = i;
            while j < n && (ch[j].is_alphanumeric() || ch[j] == '_') {
                j += 1;
            }
            let word: String = ch[i..j].iter().collect();
            let col = if kws.contains(&word.as_str()) {
                theme::syn_keyword()
            } else if word.chars().next().is_some_and(|c0| c0.is_uppercase()) {
                theme::syn_type()
            } else if j < n && ch[j] == '(' {
                theme::syn_function()
            } else {
                base
            };
            out.push((word, col));
            i = j;
            continue;
        }
        // Run of punctuation / whitespace up to the next interesting char.
        let mut j = i;
        while j < n {
            let cj = ch[j];
            if cj == '"'
                || cj == '\''
                || cj == '`'
                || cj.is_ascii_digit()
                || cj.is_alphabetic()
                || cj == '_'
                || starts_comment(j)
            {
                break;
            }
            j += 1;
        }
        out.push((ch[i..j].iter().collect(), base));
        i = j;
    }
    out
}

fn is_icon_codepoint(cp: u32) -> bool {
    // Only Private-Use-Area Nerd Font icons get the fit-to-cell
    // enlargement — these are the statusline glyphs (server, git
    // branch, folder, gauge, …) that D2Coding designs small. Other
    // symbol blocks are left alone on purpose:
    //   - box drawing (2500..257F) must align to cell edges
    //   - Misc-Technical (2300..23FF, the bypass ▶ chevron) and
    //     misc arrows (2B00..2BFF) already read at the right size;
    //     enlarging them made bypass look oversized (user feedback).
    (0xE000..=0xF8FF).contains(&cp)        // BMP PUA — Nerd icons
        || (0xF0000..=0xFFFFD).contains(&cp)   // Supplementary PUA-A
        || (0x100000..=0x10FFFD).contains(&cp) // Supplementary PUA-B
}

fn cell_fg_rgba(cell: &Cell, default_fg: [u8; 4]) -> [u8; 4] {
    crate::cells::cell_fg_with(cell, default_fg)
}

fn cell_bg_rgba(cell: &Cell, default_fg: [u8; 4]) -> [u8; 4] {
    crate::cells::cell_bg_with(cell, default_fg)
}

/// Normalize u8 RGBA to 0..1 with NO colour-space conversion. The
/// Source colours are authored in sRGB. The CAMetalLayer is tagged with
/// the Display P3 colorspace (`patch_p3_colorspace_safe`), so the bytes
/// we write are interpreted as P3-encoded by macOS at scan-out. To
/// actually USE the wider gamut we have to remap sRGB → linear sRGB →
/// linear P3 → P3-encoded here (chromaticity-preserving primary
/// transform) — without this remap an sRGB-pure-red byte stays at its
/// sRGB chromaticity inside the P3 container ("same look as before").
/// With the remap, sRGB primaries get pushed out toward the P3 gamut
/// edge for the punchier reds / greens ghostty / Rio default to.
///
/// Alpha is left untouched. The framebuffer is non-sRGB Unorm, so the
/// hardware alpha blend happens in encoded P3 space — slightly bolder
/// text, matching the previous "gamma-space blending" we shipped.
/// Source colours are authored in sRGB byte triples (ANSI palette,
/// truecolor SGR, theme tokens). CAMetalLayer is tagged Display P3, so
/// the EXACT bytes we write get reinterpreted by macOS as P3-encoded —
/// which means sRGB pure red (255,0,0) renders at the WIDER P3 pure red
/// chromaticity. That's the free saturation boost ghostty / Rio rely on:
/// "no transform, just tag the layer". Doing the matrix sRGB→P3 here
/// would CANCEL the boost (it would map sRGB pure red to its sRGB-inside-
/// P3 chromaticity, i.e. same visual as before). Alpha is byte-divided.
#[inline]
pub fn srgb_rgba_to_linear(rgba: [u8; 4]) -> [f32; 4] {
    [
        rgba[0] as f32 / 255.0,
        rgba[1] as f32 / 255.0,
        rgba[2] as f32 / 255.0,
        rgba[3] as f32 / 255.0,
    ]
}

/// Text rendering knobs (text_gamma, text_contrast). WezTerm-style
/// `text_gamma>1.0` bends the glyph alpha mask so antialiased mid-tones
/// land more opaque — crisper text without changing the source colour.
/// `text_contrast` is an extra multiplier on top. Both readable from env
/// at startup so a user can tune without rebuilding.
fn text_render_knobs() -> (f32, f32, f32) {
    // gamma 1.0 = legacy linear alpha mask. Anything above sharpens but
    // also makes text feel "lifted / airy"; 1.0 stays grounded.
    let gamma = std::env::var("KASATERM_TEXT_GAMMA")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1.0_f32)
        .max(0.1);
    // 1.0 baseline — let the rendering pipeline pass alpha through
    // unchanged. The user wants knobs flat by default so the only
    // colour-shaping layer is the palette + P3 matrix.
    let contrast = std::env::var("KASATERM_TEXT_CONTRAST")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1.0_f32)
        .max(0.1);
    // Saturation 1.0 default = passthrough. Bumping shifts perceived
    // hue slightly even with luma preservation (claude code's # comment
    // green drifted to chartreuse at 1.5). Source bytes go through
    // unchanged unless user dials this up.
    let sat = std::env::var("KASATERM_COLOR_SAT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1.0_f32)
        .max(0.0);
    (gamma, contrast, sat)
}

/// Order matches the sugarloaf SugarloafFonts config we previously
/// shipped (`fonts.family = "D2CodingLigature Nerd Font Mono"`,
/// `symbol_map` for Misc-Tech / PUA / Supplementary PUA). Each entry
/// is `(path, face_index_inside_TTC)`; swash skips a face whose
/// charmap doesn't cover the codepoint, so the chain falls through
/// gracefully.
/// "-Regular" 파일 옆의 "-Bold" 형제를 찾는다. 폴백 페이스에 designed bold 를
/// 걸어주기 위한 것으로, 없으면 그 페이스가 담당하는 문자는 볼드로 요청해도
/// regular 로 그려진다 — CJK 가 특히 그렇다(합성 팽창을 CJK 에는 적용하지 않아
/// 굵어질 다른 경로가 없다).
/// Windows 는 폰트가 per-user(`%LOCALAPPDATA%\Microsoft\Windows\Fonts`) 와
/// 시스템 전체(`C:\Windows\Fonts`) 두 곳에 갈릴 수 있다. 설치 위치를 가정하지
/// 않도록 파일명마다 두 경로를 그 순서로 펼친다.
#[cfg(target_os = "windows")]
fn windows_font_candidates(names: &[&str]) -> Vec<String> {
    let local = std::env::var("LOCALAPPDATA").unwrap_or_default();
    let mut out = Vec::with_capacity(names.len() * 2);
    for name in names {
        if !local.is_empty() {
            out.push(format!(r"{local}\Microsoft\Windows\Fonts\{name}"));
        }
        out.push(format!(r"C:\Windows\Fonts\{name}"));
    }
    out
}

fn sibling_bold_font_path(regular: &str) -> Option<(String, u32)> {
    // 두 관례를 시도한다: Nerd Font 계열의 `-Regular`→`-Bold`, 그리고 Windows
    // 시스템 폰트의 `<stem>`→`<stem>bd`(consola→consolab, malgun→malgunbd).
    // 후자가 없으면 한글 최종 폴백(맑은 고딕)의 볼드가 합성으로 떨어져 획이
    // 뭉개진다.
    let mut candidates: Vec<String> = Vec::new();
    if let Some((head, tail)) = regular.rsplit_once("-Regular") {
        candidates.push(format!("{head}-Bold{tail}"));
    }
    if let Some((head, ext)) = regular.rsplit_once('.') {
        candidates.push(format!("{head}bd.{ext}"));
    }
    candidates
        .into_iter()
        .find(|p| std::path::Path::new(p).exists())
        .map(|p| (p, 0))
}

/// 폴백 체인을 한 벌 붙인다(설치 폰트 + 번들 폰트). 세 shaper(그리드·마크다운·
/// 마크다운 볼드)가 같은 체인을 쓰므로 한 곳에 모아 어긋나지 않게 한다.
/// 번들 폰트는 체인 끝에 둔다 — 사용자가 설치한 Nerd Font 가 먼저 이기되,
/// primary 의 빈 아웃라인 구멍은 여기까지 흘러와 반드시 글리프를 얻는다.
fn attach_fallback_chain(shaper: &mut Shaper) {
    for (path, idx) in fallback_font_paths() {
        let bold = sibling_bold_font_path(&path);
        shaper.add_fallback_with_bold(&path, idx, bold);
    }
    shaper.add_fallback_bytes(kasa_cells::CASCADIA_CODE_NF, 0);
    shaper.add_fallback_bytes(kasa_cells::SYMBOLS_NERD_FONT_MONO, 0);
}

fn fallback_font_paths() -> Vec<(String, u32)> {
    let mut out: Vec<(String, u32)> = Vec::new();
    #[cfg(target_os = "macos")]
    {
        let home = std::env::var("HOME").unwrap_or_default();
        let push_if = |out: &mut Vec<(String, u32)>, p: String, i: u32| {
            if std::path::Path::new(&p).exists() {
                out.push((p, i));
            }
        };
        // JetBrains Mono as the first fallback — covers Latin /
        // ASCII variants D2Coding's Korean designers left thinner,
        // plus its full Nerd Font icon table.
        push_if(
            &mut out,
            format!("{home}/Library/Fonts/JetBrainsMonoNerdFontMono-Regular.ttf"),
            0,
        );
        // D2Coding non-Mono variant catches a few glyphs the Mono
        // patch trims (the Mono variant force-fits everything into
        // a cell width — anything that didn't fit was dropped).
        push_if(
            &mut out,
            format!("{home}/Library/Fonts/D2CodingLigatureNerdFont-Regular.ttf"),
            0,
        );
        // STIX Two Math — the only macOS default with U+23F5 ⏵
        // (Black Medium Right-Pointing Triangle) baked in. Without
        // this, claude code's BYPASS prompt row shows a blank where
        // the chevron should sit.
        push_if(
            &mut out,
            "/System/Library/Fonts/Supplemental/STIXTwoMath.otf".into(),
            0,
        );
        // Menlo — generous BMP coverage for symbols D2Coding skips.
        push_if(&mut out, "/System/Library/Fonts/Menlo.ttc".into(), 0);
        // Hangul fallback — D2Coding has Hangul, but Apple SD Gothic
        // Neo catches anything D2 skips (very rare jamo cluster).
        push_if(
            &mut out,
            "/System/Library/Fonts/AppleSDGothicNeo.ttc".into(),
            0,
        );
        // Japanese / Chinese.
        push_if(
            &mut out,
            "/System/Library/Fonts/ヒラギノ角ゴシック W3.ttc".into(),
            0,
        );
        // Apple Symbols catches dingbats etc. when Menlo also misses.
        push_if(&mut out, "/System/Library/Fonts/Apple Symbols.ttf".into(), 0);
        // Color emoji last.
        push_if(
            &mut out,
            "/System/Library/Fonts/Apple Color Emoji.ttc".into(),
            0,
        );
    }
    #[cfg(target_os = "windows")]
    {
        let push_if = |out: &mut Vec<(String, u32)>, p: &str, i: u32| {
            if std::path::Path::new(p).exists() {
                out.push((p.to_string(), i));
            }
        };
        // 라틴 보강 + 한글 — macOS 체인과 같은 순서다. JetBrains 는 주 폰트가
        // 잡히지 않았을 때 라틴을 받고, D2Coding **논-Mono** 가 한글을 받는다
        // (Mono 패치는 한글을 0.5em 으로 압축해 칸의 절반만 채운다 — shaper 의
        // cjk_fit 이 키우기 전 원본 비율이 성한 쪽을 쓴다).
        for p in windows_font_candidates(&[
            "JetBrainsMonoNerdFontMono-Regular.ttf",
            "D2CodingLigatureNerdFont-Regular.ttf",
        ]) {
            push_if(&mut out, &p, 0);
        }
        // 맑은 고딕 — D2Coding 이 없는 기본 설치에서 한글을 받는 최후 보루.
        // 이게 없으면 한국어 출력 전체가 빈 칸으로 렌더된다.
        push_if(&mut out, r"C:\Windows\Fonts\malgun.ttf", 0);
        // CJK — Microsoft YaHei (Simplified Chinese) and Meiryo (Japanese).
        push_if(&mut out, r"C:\Windows\Fonts\msyh.ttc", 0);
        push_if(&mut out, r"C:\Windows\Fonts\meiryo.ttc", 0);
        // Symbols and color emoji.
        push_if(&mut out, r"C:\Windows\Fonts\seguisym.ttf", 0);
        push_if(&mut out, r"C:\Windows\Fonts\seguiemj.ttf", 0);
    }
    out
}

/// Markdown body font: a proportional gothic. Prefer Noto Sans KR if the user
/// installed it, else fall back to Apple SD Gothic Neo (always present on
/// macOS). Returns (path, face_index).
fn md_font_path() -> (String, u32) {
    #[cfg(target_os = "macos")]
    {
        let home = std::env::var("HOME").unwrap_or_default();
        let candidates = [
            format!("{home}/Library/Fonts/NotoSansKR-Regular.otf"),
            format!("{home}/Library/Fonts/NotoSansKR-Regular.ttf"),
            "/Library/Fonts/NotoSansKR-Regular.otf".to_string(),
        ];
        for c in candidates {
            if std::path::Path::new(&c).exists() {
                return (c, 0);
            }
        }
        return ("/System/Library/Fonts/AppleSDGothicNeo.ttc".to_string(), 0);
    }
    #[cfg(target_os = "windows")]
    {
        return (r"C:\Windows\Fonts\malgun.ttf".to_string(), 0);
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        return (
            "/usr/share/fonts/truetype/noto/NotoSansCJK-Regular.ttc".to_string(),
            0,
        );
    }
}

/// Bold weight of the markdown gothic. Apple SD Gothic Neo packs its Bold face
/// at TTC index 6; Noto Sans KR Bold ships as a separate file.
fn md_bold_font_path() -> (String, u32) {
    #[cfg(target_os = "macos")]
    {
        let home = std::env::var("HOME").unwrap_or_default();
        let candidates = [
            format!("{home}/Library/Fonts/NotoSansKR-Bold.otf"),
            format!("{home}/Library/Fonts/NotoSansKR-Bold.ttf"),
            "/Library/Fonts/NotoSansKR-Bold.otf".to_string(),
        ];
        for c in candidates {
            if std::path::Path::new(&c).exists() {
                return (c, 0);
            }
        }
        return ("/System/Library/Fonts/AppleSDGothicNeo.ttc".to_string(), 6);
    }
    #[cfg(target_os = "windows")]
    {
        return (r"C:\Windows\Fonts\malgunbd.ttf".to_string(), 0);
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        return (
            "/usr/share/fonts/truetype/noto/NotoSansCJK-Bold.ttc".to_string(),
            0,
        );
    }
}

/// Real italic variant of the primary mono face. JetBrains Mono ships
/// `JetBrainsMonoNerdFontMono-Italic.ttf` — D2Coding has none, so when
/// D2Coding is the primary we fall through to None (skew synthesis).
fn primary_italic_font_path() -> Option<(String, u32)> {
    if let Ok(p) = std::env::var("KASATERM_GRID_FONT_ITALIC") {
        if !p.is_empty() && std::path::Path::new(&p).exists() {
            return Some((p, 0));
        }
    }
    #[cfg(target_os = "macos")]
    {
        let home = std::env::var("HOME").unwrap_or_default();
        let p = format!("{home}/Library/Fonts/JetBrainsMonoNerdFontMono-Italic.ttf");
        if std::path::Path::new(&p).exists() {
            return Some((p, 0));
        }
    }
    None
}

/// Bold variant of the primary mono face. Returns None on platforms where
/// we can't find one — the renderer falls back to synthesised double-draw
/// bold in that case. Honours `KASATERM_GRID_FONT_BOLD` for overrides.
///
/// `primary` 는 실제 로드된 regular 경로 — 같은 패밀리의 `-Bold` 형제를 최우선
/// 으로 본다. 패밀리가 어긋나면(예: primary=D2Coding, bold=JetBrains) 한글처럼
/// bold 파일이 커버하지 않는 글자가 designed bold 를 못 타고 regular 로 폴백해
/// "볼드가 약한" 증상이 난다(거노 2026-07-26 실측: 한글 세션명 1.22x → 1.33x).
fn primary_bold_font_path(primary: &str) -> Option<(String, u32)> {
    if let Ok(p) = std::env::var("KASATERM_GRID_FONT_BOLD") {
        if !p.is_empty() && std::path::Path::new(&p).exists() {
            return Some((p, 0));
        }
    }
    // 패밀리 일치 우선 — "-Regular" → "-Bold" 형제 파일.
    if let Some(sib) = primary
        .rsplit_once("-Regular")
        .map(|(head, tail)| format!("{head}-Bold{tail}"))
    {
        if std::path::Path::new(&sib).exists() {
            return Some((sib, 0));
        }
    }
    #[cfg(target_os = "macos")]
    {
        let home = std::env::var("HOME").unwrap_or_default();
        let jb = format!("{home}/Library/Fonts/JetBrainsMonoNerdFontMono-Bold.ttf");
        if std::path::Path::new(&jb).exists() {
            return Some((jb, 0));
        }
        let p = format!("{home}/Library/Fonts/D2CodingLigatureNerdFontMono-Bold.ttf");
        if std::path::Path::new(&p).exists() {
            return Some((p, 0));
        }
        let menlo_bold = "/System/Library/Fonts/Menlo.ttc".to_string();
        if std::path::Path::new(&menlo_bold).exists() {
            return Some((menlo_bold, 1));
        }
    }
    #[cfg(target_os = "windows")]
    {
        // Designed bold matching the D2Coding primary; falls back to
        // Consolas Bold. Without a real bold face the shaper synthesises
        // bold via horizontal ink dilation, which spills past the glyph
        // advance and overlaps neighbours in bold chrome labels (active
        // tab title). The designed bold face fits its own advance.
        let local = std::env::var("LOCALAPPDATA").unwrap_or_default();
        let d2b = format!(
            r"{local}\Microsoft\Windows\Fonts\D2CodingLigatureNerdFontMono-Bold.ttf"
        );
        if std::path::Path::new(&d2b).exists() {
            return Some((d2b, 0));
        }
        let p = r"C:\Windows\Fonts\consolab.ttf";
        if std::path::Path::new(p).exists() {
            return Some((p.to_string(), 0));
        }
    }
    None
}

fn default_font_path() -> String {
    #[cfg(target_os = "macos")]
    {
        // JetBrains Mono for Latin; Hangul falls through to D2Coding 논-Mono
        // in the fallback chain (거노 요청 2026-07-27).
        //
        // 예전에 JetBrains-as-primary 를 시도했다 되돌린 적이 있는데, 그때 자간이
        // 벌어진 원인은 JetBrains 자체가 아니라 **한글을 받던 폴백이 D2Coding
        // Mono** 였다는 데 있다. Mono 패치는 한글까지 0.5em 으로 압축하는데 칸은
        // 라틴 0.6em × 2 = 1.2em 이라 글리프가 칸의 절반도 못 채웠다. 지금은
        // 논-Mono(한글 1.0em)가 받고 shaper 가 두 칸에 맞춰 키운다.
        //
        // Box-drawing chars are rendered as GPU quads via `block_rects` so the
        // font choice doesn't affect line continuity — ghostty does the same
        // thing in `src/font/sprite/draw/box.zig`.
        let home = std::env::var("HOME").unwrap_or_default();
        let jb = format!("{home}/Library/Fonts/JetBrainsMonoNerdFontMono-Regular.ttf");
        if std::path::Path::new(&jb).exists() {
            return jb;
        }
        let d2 = format!("{home}/Library/Fonts/D2CodingLigatureNerdFontMono-Regular.ttf");
        if std::path::Path::new(&d2).exists() {
            return d2;
        }
        return "/System/Library/Fonts/Menlo.ttc".into();
    }
    #[cfg(target_os = "windows")]
    {
        // macOS 와 같은 순서를 유지한다 — JetBrains Mono 가 라틴을 잡고 한글은
        // 폴백(D2Coding 논-Mono → 맑은 고딕)이 받는다. 플랫폼마다 주 폰트가
        // 다르면 같은 화면이 OS 별로 다르게 읽힌다.
        for p in windows_font_candidates(&[
            "JetBrainsMonoNerdFontMono-Regular.ttf",
            "D2CodingLigatureNerdFontMono-Regular.ttf",
        ]) {
            if std::path::Path::new(&p).exists() {
                return p;
            }
        }
        return r"C:\Windows\Fonts\consola.ttf".into();
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        return "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf".into();
    }
}

/// NSView 가 창의 콘텐츠 영역을 **꽉 채우는지** 확인하고, 작으면 다시 채운다.
/// 고쳤으면 `true`.
///
/// 거노가 큰 모니터에서 본 화면(창 1510x950 안에 UI 가 754x472 로 온전히
/// 축소돼 구석에 붙고, 빈 영역엔 우리 배경색이 아닌 NSWindow 기본색)이 바로
/// 이 상태다. 뷰가 작아지면 그 아래(레이어·`inner_size()`·스왑체인)가 전부
/// 사이좋게 작아지므로 **앱 내부에선 아무 모순이 안 보인다** — 어긋난 건 창과
/// 뷰 사이뿐이라, 자기 크기만 들여다보는 코드로는 영영 못 잡는다. 그래서 창
/// 쪽(`contentRectForFrameRect:`)을 기준으로 삼는다.
///
/// 스왑체인만 어긋난 경우와는 증상이 다르다(그쪽은 UI 가 **잘린다**).
/// `KASATERM_FORCE_SURFACE_HALF_MS` 로 둘을 갈라 실측해 둔 구분이다.
#[cfg(target_os = "macos")]
pub fn ensure_view_fills_window(window: &Window) -> bool {
    use objc2::msg_send;
    use objc2::runtime::AnyObject;
    use objc2_foundation::{NSPoint, NSRect, NSSize};
    use raw_window_handle::RawWindowHandle;
    let Ok(handle) = window.window_handle() else {
        return false;
    };
    let RawWindowHandle::AppKit(h) = handle.as_raw() else {
        return false;
    };
    let ns_view = h.ns_view.as_ptr() as *mut AnyObject;
    unsafe {
        let ns_window: *mut AnyObject = msg_send![ns_view, window];
        if ns_window.is_null() {
            return false;
        }
        let wf: NSRect = msg_send![ns_window, frame];
        let content: NSRect = msg_send![ns_window, contentRectForFrameRect: wf];
        let vf: NSRect = msg_send![ns_view, frame];
        // 미니마이즈/화면 밖 등으로 0 이 나올 때 뷰를 0 으로 만들지 않는다.
        if !(content.size.width > 1.0 && content.size.height > 1.0) {
            return false;
        }
        if (content.size.width - vf.size.width).abs() < 1.0
            && (content.size.height - vf.size.height).abs() < 1.0
            && vf.origin.x.abs() < 1.0
            && vf.origin.y.abs() < 1.0
        {
            return false;
        }
        let fixed = NSRect::new(
            NSPoint::new(0.0, 0.0),
            NSSize::new(content.size.width, content.size.height),
        );
        let _: () = msg_send![ns_view, setFrame: fixed];
        eprintln!(
            "[viewfit] 뷰가 창보다 작았다 — {:.0}x{:.0}@({:.0},{:.0}) → {:.0}x{:.0} 로 복구",
            vf.size.width, vf.size.height, vf.origin.x, vf.origin.y,
            content.size.width, content.size.height
        );
        true
    }
}

#[cfg(not(target_os = "macos"))]
pub fn ensure_view_fills_window(_window: &Window) -> bool {
    false
}

/// 레이어의 backing scale(`contentsScale`)이 창의 현재 scale 과 맞는지 확인하고,
/// 어긋나면 맞춘다. 고쳤으면 `true`.
///
/// 모니터를 옮기면 winit 의 `scale_factor()` 도 drawable 도 새 화면을 따라가는데
/// **레이어의 `contentsScale` 만 창을 만들 때 박힌 값에 영원히 머문다** — 내장↔외부를
/// 세 번 오가며 실측해도 cs 는 초기값 그대로였다. 그러면 레이어는 "이 넓이를 cs 배
/// 픽셀로 채워라" 라고 기대하는데 drawable 은 새 scale 기준이라, 2→1(내장→외부)
/// 이동에선 텍스처가 레이어 좌상단 1/4 에만 그려지고 나머지는 우리가 안 그린
/// NSWindow 기본색으로 남는다. 거노가 본 "큰 모니터로 옮기면 화면이 구석에 절반
/// 크기로 처박힘" 이 이것이고, 맥북으로 되돌리면 멀쩡한 건 고쳐져서가 아니라 cs 가
/// 원래 맞던 화면으로 돌아왔을 뿐이다.
///
/// 렌더버그 카탈로그 39번이 이 자리를 "죽은 가설" 로 폐기했던 건 반증 실험이 cs 를
/// **4.0 이라는 아무 화면과도 안 맞는 값**으로 강제해 본 것이었기 때문이다. 문제는
/// cs 의 절대값이 아니라 cs 와 drawable 이 서로 어긋나는 것이라, 틀린 값으로 흔들면
/// 아무 일도 안 일어나고 맞는 값으로 맞춰야 낫는다.
#[cfg(target_os = "macos")]
pub fn ensure_layer_scale_matches(window: &Window) -> bool {
    use objc2::msg_send;
    use objc2::runtime::AnyObject;
    use raw_window_handle::RawWindowHandle;
    let Ok(handle) = window.window_handle() else {
        return false;
    };
    let RawWindowHandle::AppKit(h) = handle.as_raw() else {
        return false;
    };
    let ns_view = h.ns_view.as_ptr() as *mut AnyObject;
    unsafe {
        let layer: *mut AnyObject = msg_send![ns_view, layer];
        if layer.is_null() {
            return false;
        }
        let cur: f64 = msg_send![layer, contentsScale];
        let want = window.scale_factor();
        // 화면이 없거나 축소 중이면 0 이 나올 수 있다 — 그 값으로 레이어를 망치지 않는다.
        if !(want > 0.0) || (cur - want).abs() < 0.01 {
            return false;
        }
        let _: () = msg_send![layer, setContentsScale: want];
        eprintln!("[layerscale] 레이어 backing scale 이 창과 어긋났다 — {cur} → {want} 로 맞춤");
        true
    }
}

#[cfg(not(target_os = "macos"))]
pub fn ensure_layer_scale_matches(_window: &Window) -> bool {
    false
}

/// 검증 전용: NSView 를 창의 절반으로 줄여 거노가 본 상태를 그대로 만든다.
/// `ensure_view_fills_window` 가 이걸 되돌리는지 보는 것이 이 하네스의 목적.
#[cfg(target_os = "macos")]
pub fn shrink_view_for_test(window: &Window) {
    use objc2::msg_send;
    use objc2::runtime::AnyObject;
    use objc2_foundation::{NSPoint, NSRect, NSSize};
    use raw_window_handle::RawWindowHandle;
    let Ok(handle) = window.window_handle() else {
        return;
    };
    let RawWindowHandle::AppKit(h) = handle.as_raw() else {
        return;
    };
    let ns_view = h.ns_view.as_ptr() as *mut AnyObject;
    unsafe {
        let vf: NSRect = msg_send![ns_view, frame];
        let half = NSRect::new(
            NSPoint::new(0.0, 0.0),
            NSSize::new(vf.size.width / 2.0, vf.size.height / 2.0),
        );
        let _: () = msg_send![ns_view, setFrame: half];
        eprintln!(
            "[forceview] 뷰를 {:.0}x{:.0} → {:.0}x{:.0} 로 축소",
            vf.size.width,
            vf.size.height,
            half.size.width,
            half.size.height
        );
    }
}

#[cfg(not(target_os = "macos"))]
pub fn shrink_view_for_test(_window: &Window) {}

#[cfg(target_os = "macos")]
unsafe fn patch_metal_layer_gravity(window: &Window) {
    use objc2::msg_send;
    use objc2::runtime::AnyObject;
    use objc2_foundation::NSString;
    use raw_window_handle::RawWindowHandle;

    let Ok(handle) = window.window_handle() else { return; };
    let RawWindowHandle::AppKit(h) = handle.as_raw() else { return; };
    let ns_view = h.ns_view.as_ptr() as *mut AnyObject;

    let root_layer: *mut AnyObject = msg_send![ns_view, layer];
    if root_layer.is_null() { return; }

    // wgpu attaches its drawing layer (WgpuObserverLayer wrapping a
    // CAMetalLayer) as a sublayer of the NSView's backing layer — so the
    // contents we need to anchor with gravity live on the SUBLAYER, not on
    // the NSView's root layer. Walk the tree and pin gravity on every
    // descendant we find.
    let gravity = NSString::from_str("topLeft");
    fn patch_recursive(layer: *mut objc2::runtime::AnyObject, gravity: &objc2_foundation::NSString) {
        use objc2::msg_send;
        unsafe {
            let _: () = msg_send![layer, setContentsGravity: gravity];
            let subs: *mut objc2::runtime::AnyObject = msg_send![layer, sublayers];
            if subs.is_null() { return; }
            let n: usize = msg_send![subs, count];
            for i in 0..n {
                let s: *mut objc2::runtime::AnyObject = msg_send![subs, objectAtIndex: i];
                if !s.is_null() {
                    patch_recursive(s, gravity);
                }
            }
        }
    }
    patch_recursive(root_layer, &gravity);
    eprintln!("[live-resize-probe] patched gravity recursively from root layer");

    // NSWindow-level colorspace. wgpu attaches its CAMetalLayer as a
    // SUBLAYER (sugarloaf replaces the view's layer entirely — that's
    // why their P3 tag stuck and ours didn't). For sublayer-based
    // setups the window's `colorSpace` is what macOS color-manages
    // against; setting it propagates Display P3 to everything inside.
    let ns_window: *mut AnyObject = msg_send![ns_view, window];
    if !ns_window.is_null() {
        if let Some(ns_cs_cls) = objc2::runtime::AnyClass::get(c"NSColorSpace") {
            let p3: *mut AnyObject = msg_send![ns_cs_cls, displayP3ColorSpace];
            if !p3.is_null() {
                let _: () = msg_send![ns_window, setColorSpace: p3];
                eprintln!("[gpu] NSWindow colorSpace → Display P3");
            }
        }
    }

    // Display P3 on CAMetalLayer. Doesn't modify source colours — just
    // tells macOS to interpret the same sRGB-encoded bytes as P3 at
    // scan-out. On Retina P3 panels the green (and red, blue) primaries
    // reach the wider P3 gamut → noticeably punchier diff bg highlights
    // / Claude Code colour chips. We had this once, removed it for fear
    // of "altering the terminal", but it's the layer-level setting
    // ghostty / iTerm2 use by default; the byte values stay untouched.
    patch_p3_colorspace_safe(root_layer);

    // NSViewLayerContentsRedrawPolicy: 2 = .duringViewResize. Default
    // (.onSetNeedsDisplay) lets AppKit skip paint during the live-resize
    // tracking loop, which is what makes the grid lag behind the frame.
    let _: () = msg_send![ns_view, setLayerContentsRedrawPolicy: 2_isize];
    // NSViewLayerContentsPlacement: 9 = .topLeft — mirrors the layer gravity
    // so AppKit's own resize-time scaling doesn't stretch contents either.
    let _: () = msg_send![ns_view, setLayerContentsPlacement: 9_isize];
}

/// Create a fresh CAMetalLayer, install it as the NSView's root layer,
/// tag it Display P3, and return the raw pointer. Used by the
/// `KASATERM_P3_ROOT=1` opt-in path: feeding this pointer to
/// `SurfaceTargetUnsafe::CoreAnimationLayer` makes wgpu reuse our layer
/// rather than create a sublayer-attached one (the macOS-color-management
/// blocker described in reference_kasaterm_color_pipeline).
///
/// Returns the layer pointer cast to `*mut c_void` — what wgpu wants.
#[cfg(target_os = "macos")]
unsafe fn install_root_p3_layer(
    window: &winit::window::Window,
    scale: f32,
) -> Result<*mut std::ffi::c_void> {
    use objc2::msg_send;
    use objc2::runtime::AnyObject;
    use raw_window_handle::RawWindowHandle;
    use std::sync::OnceLock;

    unsafe {
        let handle = window.window_handle().context("no window handle")?;
        let RawWindowHandle::AppKit(h) = handle.as_raw() else {
            anyhow::bail!("not an AppKit handle");
        };
        let ns_view = h.ns_view.as_ptr() as *mut AnyObject;

        // Fresh CAMetalLayer instance — `[[CAMetalLayer alloc] init]`.
        let metal_cls = objc2::runtime::AnyClass::get(c"CAMetalLayer")
            .context("CAMetalLayer class missing")?;
        let layer_obj: *mut AnyObject = msg_send![metal_cls, alloc];
        let layer_ptr: *mut AnyObject = msg_send![layer_obj, init];
        if layer_ptr.is_null() {
            anyhow::bail!("CAMetalLayer init returned nil");
        }

        // setFrame on the layer requires the NSRect encode trait we
        // don't bring in here — and wgpu's `surface.configure()` calls
        // `setDrawableSize` later anyway, so skipping the initial frame
        // is harmless. Just pin the backing scale.
        let _: () = msg_send![layer_ptr, setContentsScale: scale as f64];
        // Anchor content to top-left during live resize (same as
        // patch_metal_layer_gravity for the legacy path).
        let topleft = objc2_foundation::NSString::from_str("topLeft");
        let _: () = msg_send![layer_ptr, setContentsGravity: &*topleft];

        // P3 colorspace tag — cached because CGColorSpace is expensive.
        static CS: OnceLock<usize> = OnceLock::new();
        let cs = *CS.get_or_init(|| {
            #[link(name = "CoreGraphics", kind = "framework")]
            unsafe extern "C" {
                fn CGColorSpaceCreateWithName(name: *const std::ffi::c_void) -> *mut std::ffi::c_void;
                static kCGColorSpaceDisplayP3: *const std::ffi::c_void;
            }
            let p = CGColorSpaceCreateWithName(kCGColorSpaceDisplayP3);
            p as usize
        });
        if cs != 0 {
            let _: () = msg_send![layer_ptr, setColorspace: cs as *mut std::ffi::c_void];
        }

        // Install as the NSView's root layer (layer-hosting view).
        let _: () = msg_send![ns_view, setLayer: layer_ptr];
        let _: () = msg_send![ns_view, setWantsLayer: true];
        // Match the legacy patch_metal_layer_gravity: redraw on resize,
        // keep contents top-left during live drag.
        let _: () = msg_send![ns_view, setLayerContentsRedrawPolicy: 2_isize];
        let _: () = msg_send![ns_view, setLayerContentsPlacement: 9_isize];

        eprintln!(
            "[gpu] installed root P3 metal layer {:p} on NSView {:p}",
            layer_ptr, ns_view
        );
        Ok(layer_ptr as *mut std::ffi::c_void)
    }
}

/// Promote wgpu's CAMetalLayer (created as a sublayer by `layer_observer`)
/// to be the NSView's root layer. Without this, macOS color-manages
/// the parent root and silently ignores the sublayer's `colorspace`
/// tag, so Display P3 never takes effect (Color Meter reads pure sRGB).
/// Sugarloaf does this directly because it owns the layer creation.
#[cfg(target_os = "macos")]
unsafe fn promote_metal_layer_to_root(
    window: &winit::window::Window,
    surface: &wgpu::Surface<'static>,
) {
    use objc2::msg_send;
    use objc2::runtime::AnyObject;
    use raw_window_handle::RawWindowHandle;
    unsafe {
        let Ok(handle) = window.window_handle() else { return };
        let RawWindowHandle::AppKit(h) = handle.as_raw() else { return };
        let ns_view = h.ns_view.as_ptr() as *mut AnyObject;
        let hal_surface_opt = surface.as_hal::<wgpu_hal::api::Metal>();
        let Some(hal_surface) = hal_surface_opt else { return };
        let layer_lock = hal_surface.render_layer().lock();
        let layer_ref = layer_lock.as_ref();
        let layer_ptr: *mut AnyObject = layer_ref as *const _ as *mut AnyObject;
        // setLayer: requires the view to want a layer.
        let _: () = msg_send![ns_view, setLayer: layer_ptr];
        let _: () = msg_send![ns_view, setWantsLayer: true];
        // P3 colorspace stays sticky only when EDR is enabled — on Apple
        // Silicon Mini-LED panels macOS color-manages SDR content to the
        // sRGB primary subspace of the display unless wantsEDR is on.
        // Use respondsToSelector to avoid the abort we hit earlier on
        // macOS 26 when calling it via the wrong object.
        let edr_sel = objc2::sel!(setWantsExtendedDynamicRangeContent:);
        let responds: bool = msg_send![layer_ptr, respondsToSelector: edr_sel];
        if responds {
            let _: () = msg_send![layer_ptr, setWantsExtendedDynamicRangeContent: true];
            eprintln!("[gpu] EDR enabled on render layer");
        }
        eprintln!("[gpu] promoted wgpu CAMetalLayer to NSView root layer");
    }
}

/// Apply P3 colorspace through wgpu-hal directly — the actual render
/// layer wgpu owns, not whatever sublayer we walked the NSView tree
/// looking for. Without this, the layer-walk approach silently fails
/// (Color Meter still reads 255,0,0 for a pure-red printf).
#[cfg(target_os = "macos")]
fn apply_p3_via_hal(surface: &wgpu::Surface<'static>) {
    use objc2::msg_send;
    use objc2::runtime::AnyObject;
    use std::sync::OnceLock;
    static CS: OnceLock<usize> = OnceLock::new();
    let cs = *CS.get_or_init(|| unsafe {
        #[link(name = "CoreGraphics", kind = "framework")]
        unsafe extern "C" {
            fn CGColorSpaceCreateWithName(name: *const std::ffi::c_void) -> *mut std::ffi::c_void;
            static kCGColorSpaceDisplayP3: *const std::ffi::c_void;
        }
        let p = CGColorSpaceCreateWithName(kCGColorSpaceDisplayP3);
        p as usize
    });
    if cs == 0 { return; }
    unsafe {
        let hal_surface_opt = surface.as_hal::<wgpu_hal::api::Metal>();
        let Some(hal_surface) = hal_surface_opt else { return };
        let layer_lock = hal_surface.render_layer().lock();
        // metal::MetalLayerRef IS the CAMetalLayer Obj-C object — its
        // `&Ref` IS the pointer. Cast through *const () to drop the
        // type info safely.
        let layer_ref = layer_lock.as_ref();
        let layer_ptr: *mut AnyObject = layer_ref as *const _ as *mut AnyObject;
        let _: () = msg_send![layer_ptr, setColorspace: cs as *mut std::ffi::c_void];
        if std::env::var_os("KASATERM_COLORSPACE_DEBUG").is_some() {
            let applied: *mut AnyObject = msg_send![layer_ptr, colorspace];
            eprintln!(
                "[gpu] HAL P3 set on render_layer={:p} applied={}",
                layer_ptr,
                !applied.is_null()
            );
        }
    }
}

/// Per-frame P3 colorspace re-application via NSView layer walk. Kept as
/// a belt-and-braces — wgpu-hal path is the real fix.
#[cfg(target_os = "macos")]
unsafe fn reapply_p3(window: &winit::window::Window) {
    use objc2::msg_send;
    use objc2::runtime::AnyObject;
    use raw_window_handle::RawWindowHandle;
    use std::sync::OnceLock;
    static CACHED: OnceLock<(usize, usize)> = OnceLock::new(); // (layer_ptr, cs_ptr)
    let entry = CACHED.get_or_init(|| {
        #[link(name = "CoreGraphics", kind = "framework")]
        unsafe extern "C" {
            fn CGColorSpaceCreateWithName(name: *const std::ffi::c_void) -> *mut std::ffi::c_void;
            static kCGColorSpaceDisplayP3: *const std::ffi::c_void;
        }
        unsafe {
            let cs = CGColorSpaceCreateWithName(kCGColorSpaceDisplayP3);
            let Ok(handle) = window.window_handle() else { return (0, 0) };
            let RawWindowHandle::AppKit(h) = handle.as_raw() else { return (0, 0) };
            let ns_view = h.ns_view.as_ptr() as *mut AnyObject;
            let root_layer: *mut AnyObject = msg_send![ns_view, layer];
            // Walk to find the first CAMetalLayer-subclass descendant.
            let Some(metal_cls) = objc2::runtime::AnyClass::get(c"CAMetalLayer") else {
                return (0, 0);
            };
            fn find(l: *mut AnyObject, cls: &objc2::runtime::AnyClass) -> *mut AnyObject {
                unsafe {
                    let is_metal: bool = msg_send![l, isKindOfClass: cls];
                    if is_metal { return l; }
                    let subs: *mut AnyObject = msg_send![l, sublayers];
                    if !subs.is_null() {
                        let n: usize = msg_send![subs, count];
                        for i in 0..n {
                            let s: *mut AnyObject = msg_send![subs, objectAtIndex: i];
                            if !s.is_null() {
                                let r = find(s, cls);
                                if !r.is_null() { return r; }
                            }
                        }
                    }
                    std::ptr::null_mut()
                }
            }
            let metal_layer = find(root_layer, metal_cls);
            (metal_layer as usize, cs as usize)
        }
    });
    let (layer_ptr, cs_ptr) = *entry;
    if layer_ptr == 0 || cs_ptr == 0 { return; }
    let layer = layer_ptr as *mut AnyObject;
    let cs = cs_ptr as *mut std::ffi::c_void;
    let _: () = unsafe { msg_send![layer, setColorspace: cs] };
}

/// Walks the layer tree and sets every CAMetalLayer descendant's
/// colorspace to Display P3 via direct CoreGraphics FFI. Skips any
/// non-CAMetalLayer (CALayer doesn't respond to `setColorspace:` on
/// older OS versions and the previous "patch every layer" version
/// aborted there). Returns silently on any failure — colours stay
/// sRGB rather than crashing the process.
#[cfg(target_os = "macos")]
fn patch_p3_colorspace_safe(root_layer: *mut objc2::runtime::AnyObject) {
    use objc2::msg_send;
    use objc2::runtime::AnyClass;
    unsafe {
        // ExtendedDisplayP3 (vs plain DisplayP3): the "extended" variant
        // accepts encoded values outside [0,1] mapping to HDR-bright
        // colours. Even on a Bgra8Unorm framebuffer (which clamps), the
        // layer's intent telegraphs to the macOS compositor that we want
        // the panel's widest available gamut. Ghostty / iTerm2 both
        // settle on this when an EDR display is detected.
        #[link(name = "CoreGraphics", kind = "framework")]
        unsafe extern "C" {
            fn CGColorSpaceCreateWithName(name: *const std::ffi::c_void) -> *mut std::ffi::c_void;
            fn CGColorSpaceRelease(cs: *mut std::ffi::c_void);
            static kCGColorSpaceDisplayP3: *const std::ffi::c_void;
            static kCGColorSpaceExtendedDisplayP3: *const std::ffi::c_void;
        }
        // env override KASATERM_COLORSPACE=p3|extended-p3|disabled
        let cs_name = std::env::var("KASATERM_COLORSPACE")
            .unwrap_or_else(|_| "p3".to_string());
        let cs_ref: *const std::ffi::c_void = match cs_name.as_str() {
            "disabled" => return,
            "extended-p3" => kCGColorSpaceExtendedDisplayP3,
            _ => kCGColorSpaceDisplayP3,
        };
        let cs = CGColorSpaceCreateWithName(cs_ref);
        if cs.is_null() {
            return;
        }
        let Some(metal_class) = AnyClass::get(c"CAMetalLayer") else {
            CGColorSpaceRelease(cs);
            return;
        };
        fn walk(
            layer: *mut objc2::runtime::AnyObject,
            cs: *mut std::ffi::c_void,
            metal_class: &AnyClass,
        ) -> usize {
            unsafe {
                let mut hits = 0usize;
                let is_metal: bool = msg_send![layer, isKindOfClass: metal_class];
                if is_metal {
                    let _: () = msg_send![layer, setColorspace: cs];
                    hits += 1;
                }
                let subs: *mut objc2::runtime::AnyObject = msg_send![layer, sublayers];
                if subs.is_null() {
                    return hits;
                }
                let n: usize = msg_send![subs, count];
                for i in 0..n {
                    let s: *mut objc2::runtime::AnyObject = msg_send![subs, objectAtIndex: i];
                    if !s.is_null() {
                        hits += walk(s, cs, metal_class);
                    }
                }
                hits
            }
        }
        let hits = walk(root_layer, cs, metal_class);
        // Sugarloaf's defensive pattern: never release the colorspace
        // we just handed to the layer. The CA property is documented to
        // retain on set, but if Apple ever changes that semantics our
        // colorspace would silently drop and the layer falls back to
        // sRGB — exactly the "set returned ok but colours look wrong"
        // symptom. We create one per process, so the leak is fine.
        // (See sugarloaf-0.4.4/src/context/metal.rs.)
        // `cs` is a *mut c_void (Copy) — `mem::forget` on it is a no-op;
        // we just want to suppress unused-result warnings. The actual
        // retain happens at the setColorspace: msg_send above.
        let _ = cs;
        eprintln!("[gpu] CAMetalLayer colorspace → {cs_name} ({hits} layer(s) tagged)");
    }
}

/// True while AppKit's live-resize tracking loop owns the window — the user
/// is dragging an edge. ghostty's resize trick depends on knowing this:
/// during live resize we leave the CAMetalLayer's drawableSize alone (no
/// surface.configure, no render) so the layer keeps its last painted
/// contents, and gravity=topLeft anchors that to the top-left while AppKit
/// stretches the bounds. The newly revealed area shows the clear colour
/// instead of stretched stale pixels.
#[cfg(target_os = "macos")]
pub fn is_in_live_resize(window: &Window) -> bool {
    use objc2::msg_send;
    use objc2::runtime::AnyObject;
    use raw_window_handle::RawWindowHandle;
    let Ok(handle) = window.window_handle() else {
        return false;
    };
    let RawWindowHandle::AppKit(h) = handle.as_raw() else {
        return false;
    };
    let ns_view = h.ns_view.as_ptr() as *mut AnyObject;
    unsafe {
        let r: bool = msg_send![ns_view, inLiveResize];
        r
    }
}

#[cfg(not(target_os = "macos"))]
pub fn is_in_live_resize(_window: &Window) -> bool {
    false
}

/// Run `f` inside a CATransaction with implicit animations disabled. AppKit
/// hangs a layer animation on bounds jumps (zoom / maximize is the worst
/// case) and lets stale contents interpolate to the new bounds — gravity
/// alone can't fix that mid-animation. Wrapping the resize + render kills
/// the animation so the new frame is what AppKit composites.
#[cfg(target_os = "macos")]
pub fn with_disabled_layer_actions<F: FnOnce()>(f: F) {
    use objc2::msg_send;
    use objc2::runtime::AnyClass;
    unsafe {
        let Some(class) = AnyClass::get(c"CATransaction") else {
            f();
            return;
        };
        let _: () = msg_send![class, begin];
        let _: () = msg_send![class, setDisableActions: true];
        f();
        let _: () = msg_send![class, commit];
    }
}

#[cfg(not(target_os = "macos"))]
pub fn with_disabled_layer_actions<F: FnOnce()>(f: F) {
    f();
}

/// Toggle window maximize ("zoom") with NO frame animation. winit's
/// `set_maximized` routes through `[NSWindow zoom:]`, which animates the frame
/// over `animationResizeTime:` — that's the slow "이상한 애니메이션으로 늦게
/// 커짐" the user sees on a title-strip double-click. We drive the frame swap
/// ourselves with `animate:NO` so it snaps instantly. `saved` holds the
/// pre-zoom frame (Cocoa screen coords) so the next toggle can restore it;
/// `None` means currently un-maximized.
#[cfg(target_os = "macos")]
pub fn toggle_maximize_no_anim(window: &Window, saved: &mut Option<(f64, f64, f64, f64)>) {
    use objc2::msg_send;
    use objc2::runtime::{AnyClass, AnyObject};
    use objc2_foundation::{NSPoint, NSRect, NSSize};
    use raw_window_handle::RawWindowHandle;
    let Ok(handle) = window.window_handle() else {
        return;
    };
    let RawWindowHandle::AppKit(h) = handle.as_raw() else {
        return;
    };
    let ns_view = h.ns_view.as_ptr() as *mut AnyObject;
    unsafe {
        let ns_window: *mut AnyObject = msg_send![ns_view, window];
        if ns_window.is_null() {
            return;
        }
        // isZoomed reflects the real frame regardless of how it got there
        // (our path, the green button, a live-resize drag), so it's a safer
        // truth than tracking our own bool.
        let is_zoomed: bool = msg_send![ns_window, isZoomed];
        if is_zoomed {
            if let Some((x, y, w, ht)) = saved.take() {
                let frame = NSRect::new(NSPoint::new(x, y), NSSize::new(w, ht));
                let _: () = msg_send![ns_window, setFrame: frame, display: true, animate: false];
            }
            // saved == None here means we never recorded a restore frame
            // (e.g. the window was already zoomed by some other path). Leave
            // it maximized rather than guessing a frame.
        } else {
            let cur: NSRect = msg_send![ns_window, frame];
            *saved = Some((cur.origin.x, cur.origin.y, cur.size.width, cur.size.height));
            let mut screen: *mut AnyObject = msg_send![ns_window, screen];
            if screen.is_null() {
                if let Some(cls) = AnyClass::get(c"NSScreen") {
                    screen = msg_send![cls, mainScreen];
                }
            }
            if screen.is_null() {
                return;
            }
            // visibleFrame excludes the menu bar + Dock — same target AppKit
            // zoom uses, so this matches the old maximize bounds exactly.
            let vf: NSRect = msg_send![screen, visibleFrame];
            let _: () = msg_send![ns_window, setFrame: vf, display: true, animate: false];
        }
    }
}

#[cfg(not(target_os = "macos"))]
pub fn toggle_maximize_no_anim(window: &Window, _saved: &mut Option<(f64, f64, f64, f64)>) {
    window.set_maximized(!window.is_maximized());
}

/// 창을 **다른 물리 모니터로 옮긴다**. 검증 전용 — 거노가 손으로 하는
/// "맥북 화면 ↔ 큰 모니터" 이동을 헤드리스에서 그대로 일으키려면 backing
/// scale 이 진짜로 바뀌어야 하는데, winit 이벤트는 외부에서 합성할 수 없고
/// 레이어 속성만 흉내 내는 건 (실측으로) 증상을 재현하지 못했다. 유일하게
/// 정직한 재현은 실제 `setFrame:` 으로 창을 옮겨 AppKit 이 스스로
/// `ScaleFactorChanged` 를 쏘게 하는 것이다.
#[cfg(target_os = "macos")]
pub fn move_window_to_other_screen(window: &Window) {
    use objc2::msg_send;
    use objc2::runtime::{AnyClass, AnyObject};
    use objc2_foundation::{NSPoint, NSRect, NSSize};
    use raw_window_handle::RawWindowHandle;
    let Ok(handle) = window.window_handle() else {
        return;
    };
    let RawWindowHandle::AppKit(h) = handle.as_raw() else {
        return;
    };
    let ns_view = h.ns_view.as_ptr() as *mut AnyObject;
    unsafe {
        let ns_window: *mut AnyObject = msg_send![ns_view, window];
        if ns_window.is_null() {
            return;
        }
        let Some(cls) = AnyClass::get(c"NSScreen") else {
            return;
        };
        let screens: *mut AnyObject = msg_send![cls, screens];
        let n: usize = msg_send![screens, count];
        let cur: *mut AnyObject = msg_send![ns_window, screen];
        if cur.is_null() {
            eprintln!("[movescreen] 창이 어느 화면에도 안 걸림");
            return;
        }
        let cur_frame: NSRect = msg_send![cur, frame];
        let cur_scale: f64 = msg_send![cur, backingScaleFactor];
        // NSScreen 인스턴스는 재생성될 수 있어 포인터 비교가 위험하다 —
        // origin 으로 같은 화면인지 판정한다.
        let mut target: *mut AnyObject = std::ptr::null_mut();
        for i in 0..n {
            let s: *mut AnyObject = msg_send![screens, objectAtIndex: i];
            let f: NSRect = msg_send![s, frame];
            let sc: f64 = msg_send![s, backingScaleFactor];
            eprintln!(
                "[movescreen]   #{i} scale={sc} {}x{} @({},{})",
                f.size.width, f.size.height, f.origin.x, f.origin.y
            );
            let same = (f.origin.x - cur_frame.origin.x).abs() < 1.0
                && (f.origin.y - cur_frame.origin.y).abs() < 1.0;
            if !same && target.is_null() {
                target = s;
            }
        }
        if target.is_null() {
            eprintln!("[movescreen] 화면이 하나뿐 — 이동 불가(재현 실패)");
            return;
        }
        let vf: NSRect = msg_send![target, visibleFrame];
        let tscale: f64 = msg_send![target, backingScaleFactor];
        let cf: NSRect = msg_send![ns_window, frame];
        let w = cf.size.width.min(vf.size.width);
        let ht = cf.size.height.min(vf.size.height);
        let frame = NSRect::new(
            NSPoint::new(
                vf.origin.x + (vf.size.width - w) / 2.0,
                vf.origin.y + (vf.size.height - ht) / 2.0,
            ),
            NSSize::new(w, ht),
        );
        let _: () = msg_send![ns_window, setFrame: frame, display: true, animate: false];
        eprintln!("[movescreen] scale {cur_scale} → {tscale}, frame {w}x{ht}");
    }
}

#[cfg(not(target_os = "macos"))]
pub fn move_window_to_other_screen(_window: &Window) {}

/// CAMetalLayer 의 실측 기하를 찍는다. GPU 리드백 캡처는 **우리 렌더 타깃**을
/// 읽으므로 컴포지터가 그 타깃을 레이어 어디에 어떤 크기로 얹는지는 절대
/// 안 보인다 — 모니터 이동 버그는 정확히 그 층에 있어서 이 프로브가 유일한 눈이다.
///
/// 불변식: `drawableSize == bounds × contentsScale`. 어긋난 채로
/// `contentsGravity = topLeft` 면 화면이 창 구석에 축소돼 처박힌다.
#[cfg(target_os = "macos")]
pub fn log_layer_geometry(window: &Window, tag: &str) {
    use objc2::msg_send;
    use objc2::runtime::AnyObject;
    use objc2_foundation::{NSRect, NSSize};
    use raw_window_handle::RawWindowHandle;
    let Ok(handle) = window.window_handle() else {
        return;
    };
    let RawWindowHandle::AppKit(h) = handle.as_raw() else {
        return;
    };
    let ns_view = h.ns_view.as_ptr() as *mut AnyObject;
    unsafe {
        let layer: *mut AnyObject = msg_send![ns_view, layer];
        if layer.is_null() {
            eprintln!("[layergeom] {tag}: 레이어 없음");
            return;
        }
        let bounds: NSRect = msg_send![layer, bounds];
        let cs: f64 = msg_send![layer, contentsScale];
        let responds: bool = msg_send![layer, respondsToSelector: objc2::sel!(drawableSize)];
        let ds: NSSize = if responds {
            msg_send![layer, drawableSize]
        } else {
            NSSize::new(-1.0, -1.0)
        };
        let vb: NSRect = msg_send![ns_view, frame];
        let ns_window: *mut AnyObject = msg_send![ns_view, window];
        // contentLayoutRect 는 타이틀바를 뺀 값이라 기준이 못 된다 — 우리 창은
        // 타이틀바를 투명하게 두고 뷰가 그 위까지 덮는다. contentRectForFrameRect:
        // 가 "뷰가 채워야 할 진짜 영역"이다.
        let content: NSRect = if ns_window.is_null() {
            NSRect::new(
                objc2_foundation::NSPoint::new(0.0, 0.0),
                NSSize::new(-1.0, -1.0),
            )
        } else {
            let wf: NSRect = msg_send![ns_window, frame];
            msg_send![ns_window, contentRectForFrameRect: wf]
        };
        let view_fills = (content.size.width - vb.size.width).abs() < 1.0
            && (content.size.height - vb.size.height).abs() < 1.0;
        let inner = window.inner_size();
        let sf = window.scale_factor();
        let want = (bounds.size.width * cs, bounds.size.height * cs);
        let ok = (want.0 - ds.width).abs() < 1.0 && (want.1 - ds.height).abs() < 1.0;
        eprintln!(
            "[layergeom] {tag}: viewFrame={:.0}x{:.0}@({:.0},{:.0}) content={:.0}x{:.0} {} | \
             layerBounds={:.0}x{:.0} cs={cs} \
             drawable={:.0}x{:.0} (기대 {:.0}x{:.0} {}) | winit inner={}x{} sf={sf}",
            vb.size.width,
            vb.size.height,
            vb.origin.x,
            vb.origin.y,
            content.size.width,
            content.size.height,
            if view_fills { "채움" } else { "★뷰가 작음★" },
            bounds.size.width,
            bounds.size.height,
            ds.width,
            ds.height,
            want.0,
            want.1,
            if ok { "일치" } else { "★어긋남★" },
            inner.width,
            inner.height,
        );
    }
}

#[cfg(not(target_os = "macos"))]
pub fn log_layer_geometry(_window: &Window, _tag: &str) {}

/// While the window is NOT zoomed, remember its frame as the un-zoom restore
/// target. The green traffic-light zoom never passes through
/// `toggle_maximize_no_anim`, so without this a title double-click after a
/// green-button zoom had no frame to restore to (`saved == None` → stayed
/// maximized, read as a dead click). Called from Moved/Resized — two
/// msg_sends, cheap enough for live-resize spam.
#[cfg(target_os = "macos")]
pub fn remember_unzoomed_frame(window: &Window, saved: &mut Option<(f64, f64, f64, f64)>) {
    use objc2::msg_send;
    use objc2::runtime::AnyObject;
    use objc2_foundation::NSRect;
    use raw_window_handle::RawWindowHandle;
    let Ok(handle) = window.window_handle() else {
        return;
    };
    let RawWindowHandle::AppKit(h) = handle.as_raw() else {
        return;
    };
    let ns_view = h.ns_view.as_ptr() as *mut AnyObject;
    unsafe {
        let ns_window: *mut AnyObject = msg_send![ns_view, window];
        if ns_window.is_null() {
            return;
        }
        let is_zoomed: bool = msg_send![ns_window, isZoomed];
        if !is_zoomed {
            let cur: NSRect = msg_send![ns_window, frame];
            *saved = Some((cur.origin.x, cur.origin.y, cur.size.width, cur.size.height));
        }
    }
}

#[cfg(not(target_os = "macos"))]
pub fn remember_unzoomed_frame(_window: &Window, _saved: &mut Option<(f64, f64, f64, f64)>) {}
