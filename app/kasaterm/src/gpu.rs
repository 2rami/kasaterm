//! 본판 렌더러. 칸 격자·커서·그림·클립은 엔진(`kasa_gridview::GridRenderer`)이 그리고, 여기는 그 위에
//! 본판만의 것 — 크롬 글자·아이콘·마크다운·코드 편집기·날씨 — 을 같은 인스턴스 목록으로 얹는다.
//! `GpuRenderer` 는 엔진을 `grid` 로 품고 `Deref` 로 그 메서드를 그대로 내보인다.

use std::sync::Arc;

use anyhow::{Context, Result};
use kasa_cells::pipeline::CellInstance;
use kasa_cells::{AtlasEntry, GlyphKey, Shaper};
use kasa_gridview::fonts::attach_fallback_chain;
use kasa_gridview::renderer::{GridFonts, GridRenderer};
use raw_window_handle::HasWindowHandle;
use winit::window::Window;

pub use kasa_gridview::macos::{
    ensure_layer_scale_matches, ensure_view_fills_window, is_in_live_resize, with_disabled_layer_actions,
};
pub use kasa_gridview::renderer::{
    cell_cols, fit_cell_glyph, is_wide_char, srgb_rgba_to_linear, PaneSlot,
};
#[cfg(target_os = "macos")]
use kasa_gridview::macos::apply_p3_via_hal;
#[cfg(target_os = "windows")]
use kasa_gridview::fonts::windows_font_candidates;

/// Bundled pixel face for chrome labels under a pixel Shape (SIL OFL 1.1 — see
/// assets/fonts/OFL-Galmuri.txt). Shipped verbatim, not subset: a subset is a
/// Modified Version under that license, and the few MB saved aren't worth it.
const GALMURI_11: &[u8] = include_bytes!("../assets/fonts/Galmuri11.ttf");
/// Nerd 아이콘을 시스템 글꼴 설치 없이 그리려고 싣는 두 글꼴(assets/fonts/THIRD-PARTY-FONTS.md).
/// CascadiaCodeNF 는 Misc-Technical·Nerd 아이콘을 넓게 덮고, SymbolsNerdFontMono 는 주 글꼴의 Nerd 패치가
/// 빈 아웃라인으로 남긴 아이콘(U+E000..F8FF·U+F0000..1FFFD) 구멍을 메운다.
const CASCADIA_CODE_NF: &[u8] = include_bytes!("../assets/fonts/CascadiaCodeNF.ttf");
const SYMBOLS_NERD_FONT_MONO: &[u8] = include_bytes!("../assets/fonts/SymbolsNerdFontMono-Regular.ttf");
/// 격자·마크다운 shaper 가 시스템 대체 글꼴 뒤에 붙이는 내장 글꼴.
const BUNDLED_FONTS: [&[u8]; 2] = [CASCADIA_CODE_NF, SYMBOLS_NERD_FONT_MONO];
/// Device px per Galmuri dot — every cut draws one dot per `upem/100` units, so
/// Galmuri11 (upem 1200) is crisp only at whole multiples of 12.
const GALMURI_DOT_PX: u32 = 12;

/// 크롬 글자를 어느 얼굴로 그리는지(`theme::ui_font` 를 푼 것).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum UiFace {
    /// 터미널 격자와 같은 고정폭(font=0).
    Terminal,
    /// 마크다운 고딕(font=1/2) — 따로 싣지 않는다.
    System,
    /// 사용자가 고른 설치 글꼴(font=4, `ui_shaper`).
    Custom,
}
/// Side of the pixel icons' design grid. Their paths sit on whole units, so any
/// raster size off this multiple lands dot edges on fractions and softens them.
const ICON_GRID_PX: u32 = 24;






pub struct GpuRenderer {
    /// 칸 격자·그림·클립을 그리는 엔진. `Deref` 로 그 메서드·필드를 그대로 쓴다.
    pub grid: GridRenderer,
    /// Bundled pixel face for chrome labels under a pixel Shape (font=3 in the
    /// shared atlas). Loaded on first use, not at startup: most sessions never
    /// select that shape, and the face is 5 MB of Hangul.
    chrome_shaper: Option<Shaper>,
    /// Set once loading fails so a broken face doesn't retry every frame.
    chrome_shaper_failed: bool,
    /// 사용자가 고른 크롬 글꼴(font=4). `ui_font` 설정이 설치 글꼴 이름일 때만
    /// 실린다 — 터미널 글꼴·시스템 고딕은 이미 있는 shaper 를 그대로 쓴다.
    ui_shaper: Option<Shaper>,
    /// 마지막으로 해석한 `theme::ui_font_gen()`. 세대가 같으면 아무것도 안 한다.
    ui_font_gen: Option<u32>,
    /// 해석 결과. 라벨 하나 그릴 때마다 카탈로그를 뒤지지 않도록 캐시한다.
    ui_face: UiFace,
    /// Secondary shaper for markdown body/heading text — a proportional gothic
    /// (Noto Sans KR if installed, else Apple SD Gothic Neo) so documents read
    /// like prose, not code. Glyphs go into the SAME atlas keyed by font=1.
    md_shaper: Shaper,
    /// Bold weight of the markdown gothic (font=2). A real heavy face reads far
    /// cleaner than smearing the regular glyph, so headings / **bold** use this.
    md_bold_shaper: Shaper,
    /// 이번 프레임에 그린 문자열 원문 — 하네스 전용, `KASATERM_TEXT_LOG` 가 있을
    /// 때만 켜진다(없으면 `Option` 검사 하나라 프로덕션엔 비용이 없다).
    ///
    /// `chrome` 에는 글리프 인스턴스만 남아 문자열을 되살릴 수 없다. 그래서
    /// "헤더에 학생 이름이 떴나" 같은 걸 캡처를 눈으로 보는 것 말고는 물을 수가
    /// 없었다 — 안 보면 조용히 통과한다.
    text_log: Option<Vec<String>>,
    /// 이번 프레임에 `hover_rect` 가 한 번이라도 그려졌나 — 즉 커서 밑에
    /// 누를 수 있는 표면이 있나. 커서 모양(손가락)을 이 하나로 정하려고 둔다.
    ///
    /// 히트렉트 목록을 따로 순회하지 않는 건, 그렇게 하면 "배경은 들리는데
    /// 커서는 그대로"인 표면이 계속 새로 생기기 때문이다. 들림을 그리는
    /// 함수가 곧 이 플래그를 세우니 둘이 갈릴 수가 없다.
    pub hover_pointer: bool,
    /// 날씨 패스. 본창 렌더러에서 날씨를 켰을 때만 생긴다(끄면 비용 0).
    pub(crate) weather: Option<crate::weather::gpu::WeatherGpu>,
    /// 이번 장에 얹을 날씨 — 앱이 `render` 직전에 넣는다.
    pub(crate) weather_frame: Option<crate::weather::gpu::Frame>,
    /// 이번 장에 그린 조작 단추(`native_controls`) — 날씨의 단추 물방울 자리.
    pub(crate) weather_spots: Vec<crate::weather::sim::ButtonSpot>,
    /// Logical-px rects of link spans drawn in the most recent markdown
    /// frame: (x, y, w, h, dest). main.rs hit-tests a click against these to
    /// open a file (Finder) or URL (browser). Cleared at the start of every
    /// `draw_markdown` so it always reflects the current scroll position.
    pub md_link_rects: Vec<(f32, f32, f32, f32, String)>,
    /// Logical-px rects of markdown code-block copy buttons: (x, y, w, h,
    /// code). main.rs hit-tests a click and copies `code`. Rebuilt each
    /// `draw_markdown` like `md_link_rects`.
    pub md_copy_rects: Vec<(f32, f32, f32, f32, String)>,
    /// Logical-px rects of rendered task checkboxes: (x, y, w, h, block index).
    /// A click here toggles `- [ ]`↔`- [x]` on that block's source line and
    /// writes the file back. Rebuilt each `draw_markdown` like `md_link_rects`;
    /// empty in Raw mode so it never fires there.
    pub md_task_rects: Vec<(f32, f32, f32, f32, usize)>,
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
    md_find_query: Option<String>,
    md_find_target: Option<(usize, usize)>,
    md_find_block: usize,
    md_find_seen: usize,
    md_find_top: f32,
    pub md_find_target_y: Option<f32>,
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

/// 펼친 원본 패널에 한 번에 보이는 줄 수. 상한이 없으면 큰 헝크 하나가 화면을
/// 통째로 덮어 정작 편집하던 코드가 사라진다. 넘친 줄 수는 발치에 적는다.
const PEEK_MAX_LINES: usize = 10;

/// 펼친 원본 패널의 자리. `GpuRenderer::peek_geom` 이 그리기와 클릭 양쪽에 준다.
pub(crate) struct PeekGeom {
    pub panel: (f32, f32, f32, f32),
    pub button: (f32, f32, f32, f32),
    /// 실제로 그린 원본 줄 수(`PEEK_MAX_LINES` 로 잘린 뒤).
    pub shown: usize,
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
/// 폭 조건에서 조용히 돌아서는 걸 아무 판정도 못 잡아 사용자 화면까지 갔다(2026-08-05).
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




// OpenHuman 문서 팔레트 — 마크다운 리더를 그 앱과 같은 톤으로 그린다(사용자
// 2026-09-05 "오픈휴먼처럼 아예 똑같이"). 값은 openhuman `styles/tokens.css` 에서
// 그대로 가져왔고, 테마 프리셋과 무관하게 라이트/다크만 가른다 — 리더는 그
// 자체로 한 편의 문서라 앱 크롬 색이 아니라 문서 색을 입는다.
fn oh_text() -> [u8; 4] {
    if crate::theme::current_is_light() { [23, 23, 23, 255] } else { [245, 245, 245, 255] }
}
fn oh_dim() -> [u8; 4] {
    if crate::theme::current_is_light() { [115, 115, 115, 255] } else { [163, 163, 163, 255] }
}
fn oh_line() -> [u8; 4] {
    if crate::theme::current_is_light() { [229, 229, 229, 255] } else { [64, 64, 64, 255] }
}

impl std::ops::Deref for GpuRenderer {
    type Target = GridRenderer;
    fn deref(&self) -> &GridRenderer {
        &self.grid
    }
}

impl std::ops::DerefMut for GpuRenderer {
    fn deref_mut(&mut self) -> &mut GridRenderer {
        &mut self.grid
    }
}

impl GpuRenderer {
    pub fn new(window: Arc<Window>, font_size_logical: f32) -> Result<Self> {
        let primary = std::env::var("KASATERM_GRID_FONT")
            .ok()
            .filter(|p| !p.is_empty())
            .or_else(|| crate::socket::read_font_path().map(|p| p.to_string_lossy().into_owned()));
        let mut grid =
            GridRenderer::new(window, font_size_logical, GridFonts { primary, bundled: BUNDLED_FONTS.to_vec() })?;
        grid.palette = crate::cells::palette();
        let font_path = grid.font_path.clone();
        // Markdown body font — a proportional gothic. Falls back to the primary
        // mono if the gothic can't load so the renderer never panics.
        let (md_font, md_idx) = md_font_path();
        let mut md_shaper = Shaper::from_path(&md_font, md_idx)
            .or_else(|_| Shaper::from_path(&font_path, 0))
            .with_context(|| format!("load markdown font {md_font}"))?;
        #[cfg(target_os = "windows")]
        if crate::theme::viewer_chrome() {
            md_shaper.set_variation_weight(400.0);
        }
        eprintln!("[font] markdown={md_font}");
        // Same bundled symbol/icon fallbacks so glyphs the gothic lacks still
        // resolve (and CJK falls through to the gothic's own coverage first).
        attach_fallback_chain(&mut md_shaper, &BUNDLED_FONTS);
        // Bold weight of the markdown gothic.
        let (md_bold_font, md_bold_idx) = md_bold_font_path();
        let mut md_bold_shaper = Shaper::from_path(&md_bold_font, md_bold_idx)
            .or_else(|_| Shaper::from_path(&md_font, md_idx))
            .with_context(|| format!("load markdown bold font {md_bold_font}"))?;
        #[cfg(target_os = "windows")]
        if crate::theme::viewer_chrome() {
            md_bold_shaper.set_variation_weight(600.0);
        }
        // 이 shaper 는 이미 굵은 얼굴인데 부르는 쪽이 bold 키를 함께 넘긴다. 굵은 얼굴을
        // 제 슬롯에 걸어 두지 않으면 shaper 가 「굵은 얼굴 없음」으로 보고 라틴에 합성
        // 팽창을 한 번 더 얹는다 — 한글은 팽창 대상이 아니라 라틴만 두 배로 굵어졌다
        // (24px 실측 잉크: 라틴 ×2.2, 한글 ×1.48). 걸면 둘 다 ×1.5 로 맞는다.
        md_bold_shaper.set_bold_face_path(0, &md_bold_font, md_bold_idx);
        attach_fallback_chain(&mut md_bold_shaper, &BUNDLED_FONTS);
        Ok(Self {
            grid,
            chrome_shaper: None,
            chrome_shaper_failed: false,
            ui_shaper: None,
            ui_font_gen: None,
            ui_face: UiFace::Terminal,
            md_shaper,
            md_bold_shaper,
            text_log: std::env::var_os("KASATERM_TEXT_LOG").map(|_| Vec::new()),
            hover_pointer: false,
            weather: None,
            weather_frame: None,
            weather_spots: Vec::new(),
            md_link_rects: Vec::new(),
            md_copy_rects: Vec::new(),
            md_task_rects: Vec::new(),
            md_word_rects: Vec::new(),
            md_sel_screen: None,
            md_block_ys: Vec::new(),
            md_find_query: None,
            md_find_target: None,
            md_find_block: usize::MAX,
            md_find_seen: 0,
            md_find_top: 0.0,
            md_find_target_y: None,
            raw_hl: Vec::new(),
            raw_hl_pending: None,
            raw_hl_cost: std::time::Duration::ZERO,
            raw_hl_parsed_once: false,
            md_heights: Vec::new(),
        })
    }

    /// 엔진의 장 그리기에 날씨 패스를 얹는다.
    pub fn render(
        &mut self,
        _panes: &[PaneSlot<'_>],
        _scale: f32,
        time_secs: f32,
        chrome_changed: bool,
    ) -> Result<usize> {
        let Self { grid, weather, weather_frame, .. } = self;
        let frame = weather_frame.take();
        grid.render(time_secs, chrome_changed, |device, queue, encoder, texture, view, format| {
            let Some(f) = frame else { return };
            if weather.as_ref().is_some_and(|wg| wg.format() != format) {
                *weather = None;
            }
            let wg = weather.get_or_insert_with(|| crate::weather::gpu::WeatherGpu::new(device, format));
            wg.encode(device, queue, encoder, texture, view, &f, true);
        })
    }

    /// 화면 밖 칸 사진. 장 사이에 불릴 수 있어 팔레트를 지금 테마로 맞춘 뒤 엔진에 맡긴다.
    pub fn render_cells_offscreen(
        &mut self,
        panes: &[PaneSlot<'_>],
        w: u32,
        h: u32,
        path: &str,
        max_w: u32,
        paint: impl FnOnce(&mut GridRenderer),
    ) -> Result<(u32, u32), String> {
        self.grid.palette = crate::cells::palette();
        self.grid.render_cells_offscreen(panes, w, h, path, max_w, paint)
    }








    /// 「일하는 중」 한 바퀴·「뒤에서 도는 중」 흐르는 점선(논리 px 둥근 사각). 모양은 `theme::activity_edge` 한
    /// 곳에서 정하고, 셰이더가 `u.time` 으로 움직이므로 일하는 동안에도 CPU 는 테두리를 다시 짓지 않는다.
    pub fn activity_outline(&mut self, rect: (f32, f32, f32, f32), radius: f32, col: [u8; 4], look: crate::theme::ActivityEdge) {
        let fade = |a: u8| crate::theme::with_alpha(col, (col[3] as u32 * a as u32 / 255) as u8);
        match look {
            crate::theme::ActivityEdge::Orbit { head, tail_end, lap, tail, alpha } => {
                self.grid.edge_orbit(rect, fade(alpha), (head, tail_end), radius, lap, tail)
            }
            crate::theme::ActivityEdge::Dash { thick, cycle, step, duty, alpha } => {
                self.grid.edge_dash(rect, fade(alpha), thick, cycle, radius, step, duty)
            }
        }
    }

    /// Compact-progress rail (logical px). Pushes ONE `FLAG_BAND_FILL` instance;
    /// the shader fills from the left on a 2.4s loop and restarts, so the header
    /// says "something with an end is running" — the shape a sweep can't say.
    /// Same idle-0-CPU property as `activity_outline`.
    ///
    /// 채운 칸이 실제 진행률은 아니다. claude 는 compact 진행률을 화면에만 내놓고
    /// 우리에게 넘기지 않으므로 시간으로 채운다. 그 화면 표시가 teammate 메시지
    /// 오버레이에 가려질 수 있어서 헤더에 따로 신호를 두는 것이 이 바의 존재 이유다.
    pub fn compact_bar(&mut self, x: f32, y: f32, w: f32, h: f32, rgba_u8: [u8; 4]) {
        self.grid.band(x, y, w, h, rgba_u8, CellInstance::FLAG_BAND_FILL);
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
    fn chrome_face(&mut self, font_size: f32, bold: bool) -> (u8, u32) {
        self.chrome_face_opt(font_size, false, bold)
    }

    /// `force_mono` pins the terminal face regardless of shape — for chrome that
    /// is *depicting* the terminal (the theme cards' `ls -la` line). Drawing that
    /// in the UI face would make the preview lie about what the terminal shows.
    fn chrome_face_opt(&mut self, font_size: f32, force_mono: bool, bold: bool) -> (u8, u32) {
        let raw = (font_size * self.grid.scale).round().max(1.0) as u32;
        if !force_mono && crate::theme::viewer_chrome() {
            return (if bold { 2 } else { 1 }, raw);
        }
        if force_mono {
            return (0, raw);
        }
        if !crate::theme::pixel_chrome() {
            // 픽셀 형태가 아닐 때만 사용자 글꼴이 든다 — 픽셀 형태의 정체성이
            // 곧 그 글꼴이라, 거기에 다른 얼굴을 얹으면 형태를 고른 뜻이 없어진다.
            self.ensure_ui_shaper();
            return match self.ui_face {
                UiFace::Terminal => (0, raw),
                UiFace::System => (if bold { 2 } else { 1 }, raw),
                UiFace::Custom => (4, raw),
            };
        }
        self.ensure_chrome_shaper();
        if self.chrome_shaper.is_none() {
            return (0, raw);
        }
        let dot = GALMURI_DOT_PX as f32;
        let steps = (raw as f32 / dot).round().max(1.0);
        (3, (dot * steps) as u32)
    }

    /// `ui_font` 설정을 얼굴로 푼다. 세대가 바뀐 프레임에만 돌고, 그때 글꼴이
    /// 갈렸으면 아틀라스를 비운다 — font=4 자리에 옛 얼굴의 글리프가 남아 있다.
    fn ensure_ui_shaper(&mut self) {
        let gen = crate::theme::ui_font_gen();
        if self.ui_font_gen == Some(gen) {
            return;
        }
        self.ui_font_gen = Some(gen);
        let before = self.ui_face;
        let value = crate::theme::ui_font();
        self.ui_face = match value.as_str() {
            "" | "terminal" => UiFace::Terminal,
            "system" => UiFace::System,
            other => match crate::onboarding::resolve_ui_font(other) {
                Some(choice) => match Shaper::from_path(&choice.path.to_string_lossy(), choice.index) {
                    Ok(mut sh) => {
                        if let Some((bold_path, bold_idx)) = &choice.bold {
                            sh.set_bold_face_path(0, &bold_path.to_string_lossy(), *bold_idx);
                        }
                        attach_fallback_chain(&mut sh, &BUNDLED_FONTS);
                        eprintln!("[font] ui={} ({})", choice.family, choice.path.display());
                        self.ui_shaper = Some(sh);
                        UiFace::Custom
                    }
                    Err(e) => {
                        eprintln!("[font] ui font {} failed to load: {e}", choice.path.display());
                        UiFace::System
                    }
                },
                None => {
                    eprintln!("[font] ui font {other:?} not installed; using system gothic");
                    UiFace::System
                }
            },
        };
        if self.ui_face != UiFace::Custom {
            self.ui_shaper = None;
        }
        // 얼굴이 같은 종류라도(Custom→Custom) 파일이 갈렸을 수 있다. 세대가
        // 올랐다는 것 자체가 「값이 바뀌었다」이므로 무조건 비운다 — 처음 한 번은
        // 아직 아무것도 안 그린 상태라 비용이 없다.
        if before != self.ui_face || self.ui_face == UiFace::Custom {
            self.grid.atlas.request_reset();
        }
    }

    fn ensure_chrome_shaper(&mut self) {
        if self.chrome_shaper.is_some() || self.chrome_shaper_failed {
            return;
        }
        match Shaper::from_bytes(GALMURI_11.to_vec(), 0) {
            Ok(mut sh) => {
                // Hangul and Latin come from Galmuri itself; the chain covers
                // the icon/symbol glyphs a text face has no reason to carry.
                attach_fallback_chain(&mut sh, &BUNDLED_FONTS);
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
                return self.grid.atlas.get_or_bake(&self.grid.device, &self.grid.queue, sh, key);
            }
        }
        if key.font == 4 {
            if let Some(sh) = self.ui_shaper.as_mut() {
                return self.grid.atlas.get_or_bake(&self.grid.device, &self.grid.queue, sh, key);
            }
        }
        match key.font {
            2 => self.grid.atlas.get_or_bake(
                &self.grid.device,
                &self.grid.queue,
                &mut self.md_bold_shaper,
                key,
            ),
            1 => self
                .grid.atlas
                .get_or_bake(&self.grid.device, &self.grid.queue, &mut self.md_shaper, key),
            _ => self
                .grid.atlas
                .get_or_bake(&self.grid.device, &self.grid.queue, &mut self.grid.shaper, key),
        }
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
        if font == 4 {
            if let Some(sh) = self.ui_shaper.as_ref() {
                return sh.advance(' ', size_px);
            }
        }
        match font {
            2 => self.md_bold_shaper.advance(' ', size_px),
            1 => self.md_shaper.advance(' ', size_px),
            _ => self.grid.shaper.cell_advance(size_px),
        }
    }

    pub fn measure_chrome_text(&mut self, text: &str, font_size: f32, bold: bool) -> f32 {
        let s = self.grid.scale;
        let (font, size_px) = self.chrome_face(font_size, bold);
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
                // 규칙은 `pen_step` 하나뿐이다. 전에는 같은 식을 여기 한 벌 더
                // 들고 있었는데, 그런 복사본은 한쪽만 고쳐지는 날 재는 폭과
                // 그리는 폭이 소리 없이 갈린다.
                pen += Self::pen_step(ch, entry.px_w as f32, entry.advance, size_px as f32);
            }
        }
        pen / s
    }

    pub fn draw_text(&mut self, x: f32, y: f32, text: &str, opts: DrawOpts) -> f32 {
        self.draw_text_clipped(x, y, text, opts, f32::NEG_INFINITY, f32::INFINITY)
    }

    /// `draw_text` 에 좌우 울타리(logical px)를 세운 것. Raw 편집기의 긴 코드 줄이
    /// pane 오른쪽으로 넘치거나, 가로 스크롤로 밀렸을 때 줄번호 여백(왼쪽)을 침범하는
    /// 것을 막는다 — pane 오른쪽 끝과 여백의 오른쪽 끝을 넘기면 양쪽이 막힌다.
    ///
    /// **계약(2026-08-13 개정): 이건 가로 컬링이지 클리핑이 아니다.** 잘라내기의
    /// 보증은 [`push_clip`](Self::push_clip) 이 세우는 시저가 한다. 이 함수는 울타리
    /// 밖으로 **완전히** 나간 글리프만 버려서 인스턴스를 아끼고, 걸친 글리프는
    /// 그대로 그린 뒤 시저가 반으로 자르게 둔다. 브라우저와 같은 모양이다.
    ///
    /// 예전에는 걸친 글리프를 통째로 버렸는데(= 클리핑을 글리프 단위로 흉내), 그러면
    /// 경계에서 글자가 반쯤 잘리는 대신 **툭 사라진다**. "여기서 끝난 게 아니라
    /// 이어진다" 는 신호가 없어져 읽는 사람이 잘린 줄인 줄 모른다.
    ///
    /// 그래서 울타리가 유한하면 **이 함수가 자기 시저를 직접 세운다**. 호출부 다섯
    /// 중 넷(`draw_raw_editor`·최근 커밋 목록·`draw_find_bar`)은 바깥에 클립이 없어,
    /// 세워 주지 않으면 완화가 그냥 "울타리 밖으로 새는" 회귀가 된다.
    /// 세로는 건드리지 않는다 — 지금 서 있는 클립과 교집합이라 그게 그대로 남는다.
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
        let s = self.grid.scale;
        if let Some(log) = self.text_log.as_mut() {
            log.push(text.to_string());
        }
        let (font, size_px) = self.chrome_face_opt(opts.font_size, force_mono, opts.bold);
        // The pixel face sets ascent == em, so its glyphs sit far higher above
        // the baseline than the mono primary's 0.78 assumption — without the
        // taller ratio the whole label rides up out of its row.
        let baseline_ratio = if font == 3 { 0.92 } else { 0.78 };
        let baseline_px = y * s + (size_px as f32 * baseline_ratio);
        let fg = srgb_rgba_to_linear(opts.color);
        let clip_l = clip_left * s;
        let clip_px = clip_right * s;
        // 울타리가 유한하면 그 자리에 시저를 세운다. 무한(= `draw_text`)이면 아무것도
        // 안 세운다 — 이 경로가 압도적으로 흔해서, 여기에 run 하나라도 얹으면 세그먼트가
        // 라벨 수만큼 는다.
        let fenced = clip_left.is_finite() || clip_right.is_finite();
        if fenced {
            let (fw, fh) = (self.grid.config.width as f32 / s, self.grid.config.height as f32 / s);
            let l = if clip_left.is_finite() { clip_left } else { 0.0 };
            let r = if clip_right.is_finite() { clip_right } else { fw };
            self.push_clip(l, 0.0, (r - l).max(0.0), fh);
        }
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
            // 컬링이지 클리핑이 아니다 — **완전히** 밖일 때만 버린다. 걸친 글리프는
            // 그려서 위에서 세운 시저가 반으로 자르게 둔다.
            if glyph_x + entry.px_w as f32 <= clip_l || glyph_x >= clip_px {
                pen += Self::pen_step(ch, entry.px_w as f32, entry.advance, size_px as f32);
                continue;
            }
            self.grid.chrome.push(CellInstance {
                cell_px: [glyph_x, glyph_y, entry.px_w as f32, entry.px_h as f32],
                uv_min: entry.uv_min,
                uv_max: entry.uv_max,
                fg_rgba: fg,
                ..Default::default()
            });
            pen += Self::pen_step(ch, entry.px_w as f32, entry.advance, size_px as f32);
        }
        if fenced {
            self.pop_clip();
        }
        // 버린 글리프도 `pen_step` 만큼은 전진시켰으므로 이 값은 완화 전후가 같다.
        // git 칼럼 헤더(`경로 : 브랜치 ↑n ↓n`)가 이걸로 요소를 이어붙이고 히트렉트까지
        // 만든다 — 여기가 흔들리면 배치가 통째로 어긋난다.
        pen / s
    }

    /// 비례 배치 텍스트에서 글자 하나가 펜을 얼마나 밀어내는가 —
    /// `draw_text_clipped` 이 실제로 쓰는 규칙 그 자체다.
    ///
    /// 헤더·편집기 텍스트는 모노 격자가 아니다. 와이드(CJK) 글리프는 모노
    /// 페이스에서 2셀에 가까운 advance(≈1.2em)를 들고 오는데, 그대로 쓰면 작은
    /// 라벨의 한글이 뜨문뜨문 벌어져 보인다("탭이름 테스트"). 그래서 좁히는데,
    /// **글자마다 다른 잉크 폭으로 좁히면 안 된다** — 그게 2026-08-27 에 하단바
    /// 계정 이름에서 드러난 증상이다: 「개인사이오닉」이 `개인사 이오 닉` 으로,
    /// 「사이오닉팀」이 `사 이오 닉팀` 으로 읽혔다. 잉크가 좁은 글자(이·오) 뒤는
    /// 붙고 넓은 글자(개·사) 뒤는 떠서, 낱말이 제멋대로 끊겨 보인다. 2~3글자
    /// 라벨(「원격」)에서는 끊길 자리가 없어 여태 안 보였을 뿐이다.
    ///
    /// 그래서 **전각 한 칸(1em) 고정**이다. CJK 는 애초에 모든 글자가 같은 폭으로
    /// 설계된 문자라, 균일한 값이 곧 그 문자의 자연스러운 배치다. 문자열 전체
    /// 폭은 옛 규칙의 평균(잉크 0.8~0.9em + 0.18em)과 거의 같아 배치가 통째로
    /// 밀리지 않는다.
    ///
    /// 캐럿·선택 밴드·클릭 히트테스트가 이 함수를 거치지 않으면 화면과
    /// 좌표가 갈린다 — 한글 3글자를 선택했는데 밴드가 2.3글자만 덮던
    /// 실측 버그가 정확히 그것이었다(측정 쪽만 폰트 원시 advance 를 썼다).
    /// `measure_chrome_text` 도 이 함수를 부른다.
    fn pen_step(ch: char, _px_w: f32, advance: f32, size_px: f32) -> f32 {
        if is_wide_char(ch) {
            size_px
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
        let s = self.grid.scale;
        let size_px = (font_size * s).round() as u32;
        if ch == ' ' {
            return self.grid.shaper.cell_advance(size_px as f32) / s;
        }
        let key = GlyphKey {
            ch,
            bold: false,
            italic: false,
            size_px,
            font: 0,
        };
        match self
            .grid.atlas
            .get_or_bake(&self.grid.device, &self.grid.queue, &mut self.grid.shaper, key)
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
        let s = self.grid.scale;
        let size_px = (size * s).round().max(1.0) as u32;
        let cw = self.grid.cell_w * s;
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
                self.grid.atlas
                    .get_or_bake(&self.grid.device, &self.grid.queue, &mut self.grid.shaper, key)
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
                self.grid.chrome.push(CellInstance {
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
        let s = self.grid.scale;
        let size_px = (size * s).round().max(1.0) as u32;
        let mut pen = 0.0f32;
        for ch in text.chars() {
            if ch == ' ' {
                pen += self.grid.shaper.cell_advance(size_px as f32);
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
                self.grid.atlas
                    .get_or_bake(&self.grid.device, &self.grid.queue, &mut self.grid.shaper, key)
            else {
                continue;
            };
            pen += Self::pen_step(ch, entry.px_w as f32, entry.advance, size_px as f32);
        }
        pen / s
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
                .grid.atlas
                .get_or_bake(&self.grid.device, &self.grid.queue, &mut self.md_bold_shaper, key),
            1 => self
                .grid.atlas
                .get_or_bake(&self.grid.device, &self.grid.queue, &mut self.md_shaper, key),
            _ => self
                .grid.atlas
                .get_or_bake(&self.grid.device, &self.grid.queue, &mut self.grid.shaper, key),
        }
    }

    /// Space/cell advance for the requested font at `size_px`.
    #[allow(dead_code)]
    fn font_cell_advance(&mut self, size_px: u32, font: u8) -> f32 {
        match font {
            2 => self.md_bold_shaper.cell_advance(size_px as f32),
            1 => self.md_shaper.cell_advance(size_px as f32),
            _ => self.grid.shaper.cell_advance(size_px as f32),
        }
    }

    /// True space advance for the requested font (metrics, not the 'M' cell
    /// width) — markdown word spacing.
    fn font_space_advance(&self, size_px: u32, font: u8) -> f32 {
        let sz = size_px as f32;
        match font {
            2 => self.md_bold_shaper.advance(' ', sz),
            1 => self.md_shaper.advance(' ', sz),
            _ => self.grid.shaper.advance(' ', sz),
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
        let s = self.grid.scale;
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
                    self.grid.chrome.push(CellInstance {
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
        let s = self.grid.scale;
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

    fn md_find_band(&mut self, x: f32, y: f32, w: f32, size: f32, active: bool) {
        let mut col = crate::theme::accent();
        col[3] = if active { 120 } else { 62 };
        self.rect(x, y - size * 0.1, w.max(1.0), size * 1.22, col);
    }

    /// 대화 보기의 코드 글자 — 마크다운 렌더와 같은 고정폭 얼굴(한글만 고딕). 줄 바꿈은
    /// 부르는 쪽이 이미 했다.
    pub(crate) fn draw_code_text(&mut self, x: f32, y: f32, text: &str, size: f32, color: [u8; 4]) {
        self.md_draw_word(text, x, y, size, color, false, false, 0, true);
    }

    /// 터미널 격자 글꼴 크기(논리 px) — `cell_w`·`cell_h` 와 짝이다.
    pub(crate) fn term_font_size(&self) -> f32 {
        self.grid.font_size_px as f32 / self.grid.scale
    }

    pub(crate) fn measure_code_text(&mut self, text: &str, size: f32) -> f32 {
        self.measure_run(text, size, false, false, true, true)
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
        let lh = (self.md_shaper.line_height(size * self.grid.scale).ceil() / self.grid.scale) * 1.5;
        // Real space advance, not the 'M' cell width (that over-spaced words).
        let space_w = self.measure_run(" ", size, false, false, false, false);
        let track_find = clip_top <= clip_bot;
        let find_ranges = if track_find {
            self.md_find_query.as_ref().map_or_else(Vec::new, |query| {
                let text = spans.iter().map(|span| span.text.as_str()).collect::<String>();
                crate::markdown::find_hits(std::slice::from_ref(&text), query)
                    .into_iter()
                    .map(|(_, start, end)| (start, end))
                    .collect()
            })
        } else {
            Vec::new()
        };
        let find_base = self.md_find_seen;
        let mut text_col = 0usize;
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
                let word_start = text_col;
                let word_end = word_start + trimmed.chars().count();
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
                    for (occurrence, &(start, end)) in find_ranges.iter().enumerate() {
                        if start >= word_end || end <= word_start {
                            continue;
                        }
                        let active = self.md_find_target
                            == Some((self.md_find_block, find_base + occurrence));
                        if active && self.md_find_target_y.is_none() {
                            self.md_find_target_y = Some((pen_y - self.md_find_top).max(0.0));
                        }
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
                        for (occurrence, &(start, end)) in find_ranges.iter().enumerate() {
                            if start >= word_end || end <= word_start {
                                continue;
                            }
                            let local_start = start.max(word_start) - word_start;
                            let local_end = end.min(word_end) - word_start;
                            let prefix: String = trimmed.chars().take(local_start).collect();
                            let matched: String = trimmed
                                .chars()
                                .skip(local_start)
                                .take(local_end - local_start)
                                .collect();
                            let bx = pen_x
                                + self.measure_run(
                                    &prefix,
                                    size,
                                    bold,
                                    span.italic,
                                    span.code,
                                    span.code,
                                );
                            let bw = self.measure_run(
                                &matched,
                                size,
                                bold,
                                span.italic,
                                span.code,
                                span.code,
                            );
                            let active = self.md_find_target
                                == Some((self.md_find_block, find_base + occurrence));
                            self.md_find_band(bx, pen_y, bw, size, active);
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
                text_col += word.chars().count();
            }
        }
        if track_find {
            self.md_find_seen += find_ranges.len();
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
        self.draw_markdown_with_find(blocks, doc_gen, x, y, w, h, scroll, sel_doc, None, true)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn draw_markdown_with_find(
        &mut self,
        blocks: &[crate::MdBlock],
        doc_gen: u64,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        scroll: f32,
        sel_doc: Option<(f32, f32, f32, f32)>,
        find: Option<(&str, usize, usize)>,
        draw_scrollbar: bool,
    ) -> f32 {
        use crate::MdBlock;
        // Link / copy-button rects are rebuilt from scratch each frame so
        // they track the current scroll offset; main.rs hit-tests clicks.
        self.md_link_rects.clear();
        self.md_copy_rects.clear();
        self.md_task_rects.clear();
        self.md_word_rects.clear();
        self.md_find_query = find.map(|(query, _, _)| query.to_string());
        self.md_find_target = find.map(|(_, block, ordinal)| (block, ordinal));
        self.md_find_target_y = None;
        // 문서 좌표 = 화면 좌표 + scroll (본문 박스 오프셋은 낱말 사각형과 선택에
        // 똑같이 들어가므로 비교에서 상쇄된다 — 뺄 필요가 없다).
        self.md_sel_screen = sel_doc.map(|(ax, ay, bx, by)| (ax, ay - scroll, bx, by - scroll));
        self.md_block_ys.clear();
        self.md_block_ys.reserve(blocks.len());
        let base = self.grid.font_size_px as f32 / self.grid.scale;
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
        self.md_find_top = top0;
        let mut pen_y = top0 + base * 1.1;
        // 지난 프레임에 잰 블록 높이. 스크롤은 레이아웃을 바꾸지 않으므로
        // (문서·폭·글자크기·dpi 가 같으면) 그대로 쓸 수 있고, 화면 밖 블록은
        // 재지 않고 높이만큼 건너뛴다. 큰 문서에선 이 스캔이 마크다운 그리기
        // 시간의 절반이었다(4399줄 3.1ms 중 1.6ms — 보이는 양은 110줄 문서와
        // 똑같은데도).
        // 꺼내 들고 가는 이유는 self 를 다시 빌려야 해서다.
        let key = (doc_gen, w.to_bits(), base.to_bits(), self.grid.scale.to_bits());
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
            self.md_find_block = bi;
            self.md_find_seen = 0;
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
            let find_target = self
                .md_find_target
                .is_some_and(|(target, _)| target == bi);
            if !draws_for_others
                && !find_target
                && known.is_finite()
                && (pen_y + known < clip_top || pen_y > clip_bot)
            {
                pen_y += known;
                continue;
            }
            match block {
                MdBlock::Heading { level, spans } => {
                    // OpenHuman: 절제된 스케일(h1 text-xl, h2 text-lg, h3~ 본문 크기).
                    // 크기로 위계를 크게 벌리지 않고 굵기·간격으로 가른다.
                    let scale_f = match level {
                        1 => 1.25,
                        2 => 1.125,
                        _ => 1.0,
                    };
                    let size = base * scale_f;
                    // mt-5 위 / mb-2 아래(h1·h2), h3~ 는 mt-4 / mb-1.5 로 더 붙는다.
                    pen_y += if *level <= 2 { base * 1.25 } else if *level == 3 { base } else { base * 0.875 };
                    pen_y = self.md_runs(
                        spans, x, pen_y, w, size, true, oh_text(), clip_top, clip_bot,
                    );
                    pen_y += if *level <= 2 { base * 0.5 } else { base * 0.3 };
                }
                MdBlock::Para { spans } => {
                    let size = base;
                    pen_y = self.md_runs(
                        spans, x, pen_y, w, size, false, oh_text(), clip_top, clip_bot,
                    );
                    pen_y += base * 0.75;
                }
                MdBlock::Code { code, lang } => {
                    let size = base * 0.9;
                    let lh =
                        (self.md_shaper.line_height(size * self.grid.scale).ceil() / self.grid.scale) * 1.35;
                    let pad = base * 0.85;
                    // 위 여백만 넓다 — 복사 버튼·언어 라벨이 첫 코드 줄과 같은
                    // 높이에 떠 있어서, 첫 줄이 길면 글자가 버튼 밑을 지나갔다.
                    // 겹침을 z 순서로 덮는 대신 자리부터 갈라 둔다.
                    let pad_top = base * 1.8;
                    let inner_w = (w - pad * 2.0).max(base);
                    let lines: Vec<&str> = code.trim_end_matches('\n').split('\n').collect();
                    let code_find_ranges = self.md_find_query.as_ref().map_or_else(
                        Vec::new,
                        |query| {
                            crate::markdown::find_hits(std::slice::from_ref(code), query)
                                .into_iter()
                                .map(|(_, start, end)| (start, end))
                                .collect()
                        },
                    );
                    let find_base = self.md_find_seen;
                    let mut line_offsets = Vec::with_capacity(lines.len());
                    let mut offset = 0usize;
                    for line in &lines {
                        line_offsets.push(offset);
                        offset += line.chars().count() + 1;
                    }
                    let cell = self.mono_advance(' ', size);
                    // 논리 줄 하나를 상자 폭에 맞는 시각 줄들로 접는다: (줄 번호,
                    // 들여쓰기, 문자 범위). 접기 전에는 넘친 코드가 상자·읽기 열·
                    // 스크롤바까지 밟고 지나갔다. 시저가 생겼어도 접는 쪽이 맞다 —
                    // 자르면 글자가 안 보이지만 접으면 읽을 수 있고, 가로 스크롤로
                    // 대신하려면 블록마다 상태를 들고 있어야 한다.
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
                        let slice_start = line_offsets[*li] + *from;
                        let slice_end = line_offsets[*li] + *to;
                        for (occurrence, &(start, end)) in code_find_ranges.iter().enumerate() {
                            if start < slice_end && end > slice_start {
                                let active = self.md_find_target
                                    == Some((self.md_find_block, find_base + occurrence));
                                if active && self.md_find_target_y.is_none() {
                                    self.md_find_target_y =
                                        Some((ly - self.md_find_top).max(0.0));
                                }
                            }
                        }
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
                            for (occurrence, &(start, end)) in
                                code_find_ranges.iter().enumerate()
                            {
                                if start >= slice_end || end <= slice_start {
                                    continue;
                                }
                                let local_start = start.max(slice_start) - line_offsets[*li];
                                let local_end = end.min(slice_end) - line_offsets[*li];
                                let bx = x
                                    + pad
                                    + ox
                                    + lines[*li]
                                        .chars()
                                        .skip(*from)
                                        .take(local_start.saturating_sub(*from))
                                        .map(|ch| self.mono_advance(ch, size))
                                        .sum::<f32>();
                                let bw = lines[*li]
                                    .chars()
                                    .skip(local_start)
                                    .take(local_end - local_start)
                                    .map(|ch| self.mono_advance(ch, size))
                                    .sum::<f32>();
                                let active = self.md_find_target
                                    == Some((self.md_find_block, find_base + occurrence));
                                self.md_find_band(bx, ly, bw, size, active);
                            }
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
                    self.md_find_seen += code_find_ranges.len();
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
                    let lh = self.md_shaper.line_height(size * self.grid.scale).ceil() / self.grid.scale;
                    let indent = (*depth as f32 + 1.0) * base * 1.25;
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
                                        oh_dim()
                                    },
                                );
                                // 클릭 자리 — 아이콘보다 넉넉히(손가락·마우스 여유).
                                let pad = base * 0.5;
                                self.md_task_rects.push((
                                    x + indent - base * 1.35 - pad,
                                    pen_y + (lh - isz) / 2.0 - pad,
                                    isz + pad * 2.0,
                                    isz + pad * 2.0,
                                    bi,
                                ));
                            }
                            None => self.md_list_marker(
                                marker,
                                x + indent,
                                marker_floor,
                                pen_y,
                                size,
                                oh_dim(),
                                clip_top,
                                clip_bot,
                            ),
                        }
                    }
                    // 끝낸 할 일은 노션처럼 본문까지 흐려진다 — 체크박스만 바뀌면
                    // 목록을 훑을 때 남은 일과 끝난 일이 같은 무게로 읽힌다.
                    let body = match task {
                        Some(true) => oh_dim(),
                        _ => oh_text(),
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
                    pen_y += base * 0.25;
                }
                MdBlock::Quote { spans } => {
                    let size = base;
                    let indent = base * 1.1;
                    let start_y = pen_y;
                    pen_y = self.md_runs(
                        spans,
                        x + indent,
                        pen_y,
                        (w - indent).max(1.0),
                        size,
                        false,
                        oh_dim(),
                        clip_top,
                        clip_bot,
                    );
                    let bar_h = pen_y - start_y;
                    if start_y + bar_h > clip_top && start_y < clip_bot {
                        self.rect(x, start_y, base * 0.16, bar_h, oh_line());
                    }
                    pen_y += base * 0.75;
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
                            let lh = self.md_shaper.line_height(title_size * self.grid.scale).ceil()
                                / self.grid.scale;
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
                    pen_y += base * 0.75;
                    if pen_y > clip_top && pen_y < clip_bot {
                        self.rect(x, pen_y, w, 1.0, oh_line());
                    }
                    pen_y += base * 0.75;
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
                        let disp_w = w.min(iw / self.grid.scale);
                        let disp_h = disp_w * ih / iw;
                        if pen_y + disp_h > clip_top && pen_y < clip_bot {
                            self.queue_image(key, x, pen_y, disp_w, disp_h, 1.0, 0.0, 0.0);
                        }
                        pen_y += disp_h + base * 0.7;
                    } else {
                        // Decode failed / remote URL — show the alt text dimmed.
                        let lh = (self.md_shaper.line_height(base * self.grid.scale).ceil()
                            / self.grid.scale)
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
                                // A hair *lighter* than bg so the header band
                                // reads as raised; SURFACE is near-black here and
                                // made the table top-heavy.
                                self.rect(x, pen_y, table_w, row_h, crate::theme::surface_hover());
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
                    // Column rules + the top hairline. 스크롤 클립으로 손수 잘라 두던
                    // 자리다 — 시저가 같은 일을 하므로 온전한 길이로 그리고 맡긴다.
                    if pen_y > table_top {
                        let mut vx = x;
                        for c in colw.iter().take(ncols - 1) {
                            vx += c;
                            self.rect(vx, table_top, 1.0, pen_y - table_top, crate::theme::border());
                        }
                        self.rect(x, table_top, table_w, 1.0, crate::theme::border());
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
        if draw_scrollbar && content_h > h + 1.0 {
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
        clip_flat!(md_task_rects);
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
        let base = self.grid.font_size_px as f32 / self.grid.scale;
        let pad = base * 0.6;
        let lh = (self.grid.shaper.line_height(base * self.grid.scale).ceil() / self.grid.scale) * 1.25;
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
        let want = (click_x - tx0) / self.grid.cell_w;
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
        let base = self.grid.font_size_px as f32 / self.grid.scale;
        (self.grid.shaper.line_height(base * self.grid.scale).ceil() / self.grid.scale) * 1.25
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
        let base = self.grid.font_size_px as f32 / self.grid.scale;
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
        let caret_x = cell_cols(prefix) as f32 * self.grid.cell_w;
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
    /// 16.7ms 를 넘었다 — 키마다 화면을 1~2프레임 떨어뜨려 사용자가 "반응이
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
        let base = self.grid.font_size_px as f32 / self.grid.scale;
        let pad = base * 0.6;
        let digits = ((line_count.max(1)) as f32).log10().floor() as usize + 1;
        let gutter_w = base * 0.62 * digits as f32 + base * 1.0;
        let body = w - pad - gutter_w;
        ((body / self.grid.cell_w).floor() as usize).max(8)
    }

    /// 거터의 접기 삼각형을 눌렀는가 — 눌렀으면 그 **버퍼 줄**. 판정 기준이
    /// `draw_raw_editor` 와 갈리면 안 보이는 자리가 눌리므로 같은 수치를 쓴다.
    #[allow(clippy::too_many_arguments)]
    pub fn raw_editor_fold_hit(
        &mut self,
        lines: &[String],
        x: f32,
        y: f32,
        w: f32,
        scroll: f32,
        click_x: f32,
        click_y: f32,
        folds: &[(usize, usize)],
        wrap: bool,
    ) -> Option<usize> {
        let base = self.grid.font_size_px as f32 / self.grid.scale;
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
        let row = ((click_y - top0) / lh).floor();
        if row < 0.0 {
            return None;
        }
        let wrap_cols = self.raw_editor_wrap_cols(w, lines.len(), wrap);
        let rows = crate::markdown::layout_rows(lines, folds, wrap_cols);
        rows.get(row as usize).map(|&(line, _)| line)
    }

    /// Raw-editor row metrics for the current font: (top pad, line height) in
    /// logical px. `draw_raw_editor` lays lines out at `pad + line * lh`, and
    /// `set_md_mode` inverts that to turn a scroll offset into a line number —
    /// so both must read the numbers from here, not restate them.
    pub fn raw_editor_metrics(&mut self) -> (f32, f32) {
        let base = self.grid.font_size_px as f32 / self.grid.scale;
        let lh = (self.grid.shaper.line_height(base * self.grid.scale).ceil() / self.grid.scale) * 1.25;
        (base * 0.6, lh)
    }

    /// 거터의 **변경 바**를 눌렀나 — 눌렀으면 그 버퍼 줄.
    ///
    /// 접기 삼각형(`raw_editor_fold_hit`)과 띠가 안 겹친다: 저쪽은 거터 오른쪽
    /// 끝(`gutter_w` 언저리)이고 이쪽은 맨 왼쪽이다.
    ///
    /// ⚠️ 화면 행 → 버퍼 줄 변환에 `markdown::buffer_line` 을 쓰면 안 된다. 그건
    /// 접힘만 되짚고 **랩을 모른다** — 긴 줄이 접힌 파일에서 아래로 갈수록 어긋나
    /// 엉뚱한 헝크가 열린다. 그리기 루프가 쓰는 `layout_rows` 를 그대로 지난다.
    pub fn raw_editor_diff_hit(
        &mut self,
        lines: &[String],
        x: f32,
        y: f32,
        w: f32,
        scroll: f32,
        click_x: f32,
        click_y: f32,
        folds: &[(usize, usize)],
        wrap: bool,
    ) -> Option<usize> {
        let base = self.grid.font_size_px as f32 / self.grid.scale;
        let (pad, lh) = self.raw_editor_metrics();
        // 3px 막대를 정확히 맞히라고 요구하면 아무도 안 쓴다 — 줄번호 앞까지 연다.
        if click_x < x || click_x > x + pad + base * 0.5 {
            return None;
        }
        let top0 = (y - scroll) + pad;
        let row = ((click_y - top0) / lh).floor();
        if row < 0.0 {
            return None;
        }
        let wrap_cols = self.raw_editor_wrap_cols(w, lines.len(), wrap);
        let rows = crate::markdown::layout_rows(lines, folds, wrap_cols);
        rows.get(row as usize).map(|&(li, _)| li)
    }

    /// 펼친 패널의 [되돌리기] 버튼을 눌렀나. 그리기와 **같은** `peek_geom` 을
    /// 지나므로 보이는 자리와 눌리는 자리가 갈릴 수 없다.
    #[allow(clippy::too_many_arguments)]
    pub fn raw_editor_peek_btn_hit(
        &mut self,
        lines: &[String],
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        scroll: f32,
        click: (f32, f32),
        folds: &[(usize, usize)],
        wrap: bool,
        line: usize,
        old_len: usize,
    ) -> bool {
        let base = self.grid.font_size_px as f32 / self.grid.scale;
        let (pad, lh) = self.raw_editor_metrics();
        let digits = ((lines.len().max(1)) as f32).log10().floor() as usize + 1;
        let cx0 = x + pad + base * 0.62 * digits as f32 + base * 1.0;
        let wrap_cols = self.raw_editor_wrap_cols(w, lines.len(), wrap);
        let rows = crate::markdown::layout_rows(lines, folds, wrap_cols);
        let top0 = (y - scroll) + pad;
        let Some(g) = Self::peek_geom(&rows, top0, lh, cx0, x + w, y, h, line, old_len) else {
            return false;
        };
        let (bx, by, bw, bh) = g.button;
        click.0 >= bx && click.0 < bx + bw && click.1 >= by && click.1 < by + bh
    }

    /// 펼친 원본 패널의 자리. **그리기와 클릭이 같은 이 함수를 지난다** — 두 벌로
    /// 두면 패널이 보이는 자리와 눌리는 자리가 갈린다.
    ///
    /// `None` = 그 줄이 지금 화면 행에 없다(접혀 사라졌다). 그때는 패널도 안 뜨고
    /// 버튼도 없다.
    fn peek_geom(
        rows: &[(usize, usize)],
        top0: f32,
        lh: f32,
        cx0: f32,
        clip_right: f32,
        y: f32,
        h: f32,
        line: usize,
        old_len: usize,
    ) -> Option<PeekGeom> {
        // 랩된 줄은 행이 여럿이다. 패널은 그 **마지막** 행 아래에 붙어야 줄을
        // 반으로 가르지 않는다.
        let ri = rows.iter().rposition(|&(li, _)| li == line)?;
        // 순수 삭제 헝크는 그 자리의 줄을 자기 것으로 안 갖는다 — `hunk_at` 이
        // `new.start == line` 으로 집어 주므로, 그 경우 패널은 그 줄 **위**가
        // 자연스럽지만 아래로 통일한다(자리가 하나여야 클릭이 헷갈리지 않는다).
        let shown = old_len.min(PEEK_MAX_LINES);
        // 본문(줄) + 발치 한 줄(버튼과 「외 N줄」이 같이 앉는다).
        let ph = (shown + 1) as f32 * lh;
        let py = top0 + (ri + 1) as f32 * lh;
        // 아래로 넘치면 그 줄 **위**로 뒤집는다 — 화면 밖에 뜬 판은 없는 것과 같다.
        let py = if py + ph <= y + h { py } else { (py - lh - ph).max(y) };
        let pw = (clip_right - cx0).max(1.0);
        let bh = lh * 0.85;
        let bw = lh * 4.2;
        let button = (
            (cx0 + pw - bw - 8.0).max(cx0),
            py + ph - (lh + bh) * 0.5,
            bw,
            bh,
        );
        Some(PeekGeom { panel: (cx0, py, pw, ph), button, shown })
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
        // HEAD 대비 변경 — 거터의 색 바와 펼친 원본 패널. `None` 이면 아래 루프가
        // 지금까지와 완전히 같다. 마커·삭제자리·펼침을 낱개 인자로 더하지 않는
        // 이유는 이 시그니처가 이미 스무 개라서다.
        diff: Option<&crate::gitdiff::DiffView<'_>>,
    ) -> f32 {
        // 본문의 오른쪽 벽. `draw_text_clipped` 에 넘기면 그 함수가 이 자리에
        // 시저를 세우므로, 걸친 글자는 사라지지 않고 반으로 잘린다.
        let clip_right = x + w;
        let base = self.grid.font_size_px as f32 / self.grid.scale;
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
        let cw = self.grid.cell_w;
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
                // 안 보였다(사용자: "입력중인거 이상한 위치에 있어"). 터미널은 셀
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
                // HEAD 대비 변경 바. **마스크 rect 뒤에** 그린다 — 앞서 그리면
                // 저 rect 가 거터를 다시 칠하면서 통째로 지운다.
                //
                // 자리는 거터 맨 왼쪽이다. 오른쪽 끝은 접기 삼각형이 이미 쓰고
                // (`gutter_w - base*0.62`) 그 왼쪽을 줄번호가 채우므로, 거기 끼우면
                // 셋이 겹친다. 색은 git 컬럼의 어휘 그대로 — 같은 뜻이 두 화면에서
                // 다른 색이면 각각 외워야 한다.
                //
                // 랩으로 이어진 행에도 그린다. 한 논리 줄이 바뀐 것이므로 그 줄이
                // 차지한 화면 높이 전체가 표시를 받아야 띠가 끊겨 보이지 않는다.
                if let Some(d) = diff {
                    if let Some(mark) = d.marks.get(li).copied().flatten() {
                        let col = match mark {
                            crate::gitdiff::LineMark::Add => crate::theme::success(),
                            crate::gitdiff::LineMark::Mod => crate::theme::accent(),
                        };
                        self.rect(x + pad * 0.5, pen_y, 3.0, lh, col);
                    }
                    // 지워진 자리는 줄이 아니라 **줄 사이**다 — 지금 버퍼에 그 줄이
                    // 없으니 그것 말고 맞는 자리가 없다. 줄의 첫 행에서만 본다
                    // (랩된 줄의 중간 행마다 쐐기가 반복되면 안 된다).
                    if from == 0 {
                        // 파일 끝에서 지워졌으면 `dels` 값이 줄 수와 같다 —
                        // 그때는 가리킬 줄이 없으므로 마지막 줄의 아래 변에 붙인다.
                        let above = d.dels.contains(&li);
                        let below = li + 1 == lines.len() && d.dels.contains(&lines.len());
                        for (hit, wy) in [(above, pen_y), (below, pen_y + lh - 2.0)] {
                            if hit {
                                self.rect(x + pad * 0.5, wy, base * 0.9, 2.0, crate::screenread::DIFF_RED);
                            }
                        }
                    }
                }
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
        // 펼친 원본 패널 — 줄 루프 밖에서, 자동완성보다 **먼저**. 자동완성은
        // 지금 치고 있는 것이라 무엇보다 위에 있어야 한다.
        //
        // 버퍼에 가상 행을 끼워 밀어내지 않고 위에 띄우는 이유: 행을 끼우려면
        // `layout_rows` 를 건드려야 하는데 거기가 캐럿·클릭·스크롤·선택이 전부
        // 지나는 자리다. 보이는 모양은 거의 같고 위험은 훨씬 작다.
        if let Some((pl, old)) = diff.and_then(|d| d.peek) {
            if let Some(g) = Self::peek_geom(&rows, top0, lh, cx0, clip_right, y, h, pl, old.len())
            {
                let (px, py, pw, ph) = g.panel;
                self.rect(px, py, pw, ph, crate::theme::surface_active());
                self.rect(px, py, pw, 1.0, crate::theme::border());
                self.rect(px, py + ph - 1.0, pw, 1.0, crate::theme::border());
                // 왼쪽 변만 빨강 — 이 판이 「지워진 것」이라는 신호를 거터 쐐기와
                // 같은 색으로 잇는다.
                self.rect(px, py, 2.0, ph, crate::screenread::DIFF_RED);
                let dim = crate::theme::with_alpha(crate::screenread::DIFF_RED, 0x1C);
                for (i, l) in old.iter().take(g.shown).enumerate() {
                    let ly = py + i as f32 * lh;
                    self.rect(px + 2.0, ly, pw - 2.0, lh, dim);
                    let cells: Vec<(char, [u8; 4])> = std::iter::once('-')
                        .chain(std::iter::once(' '))
                        .chain(l.chars())
                        .map(|c| (c, crate::theme::text_dim()))
                        .collect();
                    self.draw_editor_cells(
                        &cells,
                        px + 8.0,
                        ly + glyph_voff,
                        base,
                        px,
                        px + pw,
                    );
                }
                // 잘린 나머지를 숨기지 않는다 — 「원본이 이게 다」로 읽히면
                // 되돌리기를 누를 때 무엇이 돌아오는지 잘못 안다.
                let more = old.len().saturating_sub(g.shown);
                let foot = py + g.shown as f32 * lh;
                if more > 0 {
                    self.draw_text(
                        px + 8.0,
                        foot + glyph_voff,
                        &format!("… 외 {more}줄"),
                        DrawOpts {
                            font_size: base * 0.85,
                            color: crate::theme::text_mute(),
                            bold: false,
                            italic: false,
                        },
                    );
                }
                let (bx, by, bw, bh) = g.button;
                self.rect(bx, by, bw, bh, crate::theme::surface_hover());
                self.rect(bx, by, bw, 1.0, crate::theme::border());
                self.rect(bx, by + bh - 1.0, bw, 1.0, crate::theme::border());
                self.rect(bx, by, 1.0, bh, crate::theme::border());
                self.rect(bx + bw - 1.0, by, 1.0, bh, crate::theme::border());
                let label = "되돌리기";
                let lw = self.measure_pen_run(label, base * 0.85, false, false);
                self.draw_text(
                    bx + (bw - lw) * 0.5,
                    by + (bh - base * 0.85) * 0.5,
                    label,
                    DrawOpts {
                        font_size: base * 0.85,
                        color: crate::theme::text(),
                        bold: false,
                        italic: false,
                    },
                );
            }
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
        self.grid.palette = crate::cells::palette();
        self.grid.clear_chrome();
        if let Some(log) = self.text_log.as_mut() {
            log.clear();
        }
        self.hover_pointer = false;
        self.weather_spots.clear();
    }

    /// 조작 단추 하나를 그렸다 — 날씨가 켜져 있으면 다음 장의 단추 물방울 자리가 된다.
    pub(crate) fn note_control(&mut self, rect: (f32, f32, f32, f32), hover: bool, enabled: bool) {
        if self.weather_frame.is_some() || self.weather.is_some() {
            self.weather_spots.push(crate::weather::sim::ButtonSpot { rect: [rect.0, rect.1, rect.2, rect.3], hover, enabled });
        }
    }

    /// 앱 프레임은 그대로 두고 날씨만 한 장 더 — 비만 움직이는 동안 화면 전체를 다시 짓지 않는다.
    /// 마지막 전체 프레임의 복사본이 없으면(창 크기가 바뀐 직후 등) 아무것도 안 한다.
    pub(crate) fn render_weather_only(&mut self, f: crate::weather::gpu::Frame) -> Result<bool> {
        let (w, h) = (self.grid.config.width, self.grid.config.height);
        if !self.weather.as_ref().is_some_and(|wg| wg.has_frame(w, h) && wg.format() == self.grid.config.format) {
            return Ok(false);
        }
        #[cfg(target_os = "macos")]
        apply_p3_via_hal(&self.grid.surface);
        let frame = self.grid.surface.get_current_texture()?;
        let view = frame.texture.create_view(&Default::default());
        let mut encoder = self.grid.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("weather only") });
        if let Some(wg) = self.weather.as_mut() {
            wg.encode(&self.grid.device, &self.grid.queue, &mut encoder, &frame.texture, &view, &f, false);
        }
        let submitted = std::time::Instant::now();
        self.grid.queue.submit(Some(encoder.finish()));
        if crate::weather::gpu::timing_requested() {
            let _ = self.grid.device.poll(wgpu::PollType::Wait { submission_index: None, timeout: None });
            if let Some(wg) = self.weather.as_mut() {
                wg.note_wall(submitted.elapsed().as_secs_f32() * 1000.0);
            }
        }
        frame.present();
        Ok(true)
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




    /// Bundled Lucide SVG source for a chrome icon name. Compiled in so the
    /// .app needs no external asset dir. `pub(crate)` 인 건 아이콘 이름을 짓는
    /// 쪽(예: `sesscol::harness_icon`)이 그 이름이 실제로 등록돼 있는지 테스트할
    /// 수 있어야 해서다 — 없는 이름은 `queue_icon` 이 조용히 그냥 돌아간다.
    pub(crate) fn icon_svg(name: &str) -> Option<&'static str> {
        Some(match name {
            "umbrella" => include_str!("../assets/icons/umbrella.svg"),
            "folder" => include_str!("../assets/icons/folder.svg"),
            "square" => include_str!("../assets/icons/square.svg"),
            "square-check" => include_str!("../assets/icons/square-check.svg"),
            "x" => include_str!("../assets/icons/x.svg"),
            "clipboard" => include_str!("../assets/icons/clipboard.svg"),
            "pencil" => include_str!("../assets/icons/pencil.svg"),
            "trash-2" => include_str!("../assets/icons/trash-2.svg"),
            "shield" => include_str!("../assets/icons/shield.svg"),
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
            "check" => include_str!("../assets/icons/check.svg"),
            "file" => include_str!("../assets/icons/file.svg"),
            "file-code" => include_str!("../assets/icons/file-code.svg"),
            "image" => include_str!("../assets/icons/image.svg"),
            "users" => include_str!("../assets/icons/users.svg"),
            "braces" => include_str!("../assets/icons/braces.svg"),
            "settings-2" => include_str!("../assets/icons/settings-2.svg"),
            "columns-2" => include_str!("../assets/icons/columns-2.svg"),
            "rows-2" => include_str!("../assets/icons/rows-2.svg"),
            "list" => include_str!("../assets/icons/list.svg"),
            "layout-grid" => include_str!("../assets/icons/layout-grid.svg"),
            "copy" => include_str!("../assets/icons/copy.svg"),
            "terminal" => include_str!("../assets/icons/terminal.svg"),
            // 픽셀 세트에는 없다 — `queue_icon` 이 벡터로 폴백하므로 픽셀 테마에서도
            // 뜬다. 라이선스가 다른 세트라 원본에 없는 아이콘을 손으로 그려 넣지 않는다.
            "plug" => include_str!("../assets/icons/plug.svg"),
            "globe" => include_str!("../assets/icons/globe.svg"),
            "smartphone" => include_str!("../assets/icons/smartphone.svg"),
            "server" => include_str!("../assets/icons/server.svg"),
            "laptop" => include_str!("../assets/icons/laptop.svg"),
            "monitor" => include_str!("../assets/icons/monitor.svg"),
            "monitor-smartphone" => include_str!("../assets/icons/monitor-smartphone.svg"),
            "database" => include_str!("../assets/icons/database.svg"),
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
            "gmail" => include_str!("../assets/icons/gmail.svg"),
            "google" => include_str!("../assets/icons/google.svg"),
            "naver" => include_str!("../assets/icons/naver.svg"),
            "mail" => include_str!("../assets/icons/mail.svg"),
            "antigravity" => include_str!("../assets/icons/antigravity.svg"),
            // 마크다운 콜아웃(`> [!NOTE]` …) 표지. 이모지 대신 SVG 를 쓰는 이유는
            // 이모지가 폰트에 따라 흑백 글리프로 떨어지기 때문 — 실제로 `⚠️` 가
            // 밋밋한 `▲` 로 나온다.
            "info" => include_str!("../assets/icons/info.svg"),
            "lightbulb" => include_str!("../assets/icons/lightbulb.svg"),
            "triangle-alert" => include_str!("../assets/icons/triangle-alert.svg"),
            "octagon-alert" => include_str!("../assets/icons/octagon-alert.svg"),
            "message-square-warning" => include_str!("../assets/icons/message-square-warning.svg"),
            "message-circle" => include_str!("../assets/icons/message-circle.svg"),
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
            // Shell picker set (assets/icons/sh): 브랜드색 접시 + 흰 글리프의
            // filled SVG — ft 와 같이 `queue_icon_colored` 로 그린다. 다섯 줄이
            // 같은 terminal 글리프였을 땐 이름을 읽어야 구분됐다.
            "sh/pwsh" => include_str!("../assets/icons/sh/pwsh.svg"),
            "sh/winps" => include_str!("../assets/icons/sh/winps.svg"),
            "sh/gitbash" => include_str!("../assets/icons/sh/gitbash.svg"),
            "sh/wsl" => include_str!("../assets/icons/sh/wsl.svg"),
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
            "check" => include_str!("../assets/icons/pixel/check.svg"),
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
            "layout-grid" => include_str!("../assets/icons/pixel/layout-grid.svg"),
            "lightbulb" => include_str!("../assets/icons/pixel/lightbulb.svg"),
            "list" => include_str!("../assets/icons/pixel/list.svg"),
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
    fn rasterize_icon_color(svg: &str, px_w: u32, px_h: u32) -> Option<Vec<u8>> {
        let opt = resvg::usvg::Options::default();
        let tree = resvg::usvg::Tree::from_str(svg, &opt).ok()?;
        let mut pixmap = resvg::tiny_skia::Pixmap::new(px_w, px_h)?;
        let size = tree.size();
        let scale = (px_w as f32 / size.width()).min(px_h as f32 / size.height());
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
        let px = (size * self.grid.scale).round() as u32;
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
        if !self.grid.images.contains_key(&key) {
            let custom = crate::device_icons::custom_svg(name);
            let Some(svg) = custom.as_deref().or(pixel).or_else(|| Self::icon_svg(name)) else { return };
            let Some(rgba) = Self::rasterize_icon(svg, draw_px) else { return };
            self.upload_image(&key, &rgba, draw_px, draw_px);
        }
        if !self.grid.images.contains_key(&key) {
            return;
        }
        // Snap to whole device pixels: the texture is rasterized 1:1 at `px`,
        // so a fractional dest makes the linear sampler blur / fringe the
        // edges ("마우스오버 픽셀 보임"). Integer dest = crisp 1:1 blit.
        let inset = ((px - draw_px) / 2) as f32;
        let (dx, dy) = (
            (x * self.grid.scale).round() + inset,
            (y * self.grid.scale).round() + inset,
        );
        let dpx = draw_px as f32;
        self.grid.icon_quads.push((
            key,
            CellInstance {
                cell_px: [dx, dy, dpx, dpx],
                uv_min: [0.0, 0.0],
                uv_max: [1.0, 1.0],
                fg_rgba: srgb_rgba_to_linear(color),
                flags: CellInstance::FLAG_ICON,
                ..Default::default()
            },
            self.grid.chrome.len() as u32,
            self.cur_clip_phys(),
        ));
    }

    /// `queue_icon` 의 풀컬러 버전 — 파일타입 아이콘(ft/*)처럼 SVG 자체 색을
    /// 가진 글리프용. FLAG_COLOR(이모지 경로)로 그려 texel 색을 그대로 쓰고,
    /// `alpha` 만 전역 불투명도로 곱한다(ignored/dim 행 표현).
    pub fn queue_icon_colored(&mut self, name: &str, x: f32, y: f32, size: f32, alpha: f32) {
        self.queue_icon_colored_rect(name, x, y, size, size, alpha);
    }

    /// 풀컬러 브랜드 워드마크처럼 정사각형이 아닌 SVG를 원래 비율로 그린다.
    /// 정사각형 버퍼에 억지로 넣으면 NAVER 로고가 2px 높이로 눌려 글자가 사라진다.
    pub fn queue_icon_colored_rect(
        &mut self,
        name: &str,
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        alpha: f32,
    ) {
        let px_w = (width * self.grid.scale).round() as u32;
        let px_h = (height * self.grid.scale).round() as u32;
        if px_w == 0 || px_h == 0 {
            return;
        }
        let key = format!("__iconc:{name}:{px_w}x{px_h}");
        if !self.grid.images.contains_key(&key) {
            let Some(svg) = Self::icon_svg(name) else { return };
            let Some(rgba) = Self::rasterize_icon_color(svg, px_w, px_h) else { return };
            self.upload_image(&key, &rgba, px_w, px_h);
        }
        if !self.grid.images.contains_key(&key) {
            return;
        }
        let (dx, dy) = ((x * self.grid.scale).round(), (y * self.grid.scale).round());
        self.grid.icon_quads.push((
            key,
            CellInstance {
                cell_px: [dx, dy, px_w as f32, px_h as f32],
                uv_min: [0.0, 0.0],
                uv_max: [1.0, 1.0],
                fg_rgba: [1.0, 1.0, 1.0, alpha],
                flags: CellInstance::FLAG_COLOR,
                ..Default::default()
            },
            self.grid.chrome.len() as u32,
            self.cur_clip_phys(),
        ));
    }









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








/// Markdown body font: a proportional gothic. Prefer Noto Sans KR if the user
/// installed it, else fall back to Apple SD Gothic Neo (always present on
/// macOS). Returns (path, face_index).
#[cfg(target_os = "windows")]
fn viewer_bundled_noto_path() -> Option<String> {
    if !crate::theme::viewer_chrome() {
        return None;
    }
    let exe = std::env::current_exe().ok()?;
    let path = exe.parent()?.join("fonts").join("NotoSansKR-Variable.ttf");
    path.is_file().then(|| path.to_string_lossy().into_owned())
}

fn md_font_path() -> (String, u32) {
    #[cfg(target_os = "macos")]
    {
        let home = kasa_socket::home_var().unwrap_or_default();
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
        if let Some(path) = viewer_bundled_noto_path() {
            return (path, 0);
        }
        if crate::theme::viewer_chrome() {
            for path in windows_font_candidates(&[
                "NotoSansKR-Regular.ttf",
                "NotoSansCJKkr-Regular.otf",
                "NotoSansCJK-Regular.ttc",
            ]) {
                if std::path::Path::new(&path).exists() {
                    return (path, 0);
                }
            }
        }
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

/// Emphasis weight of the markdown gothic. Viewer uses Apple SD Gothic Neo
/// SemiBold (TTC 4); the terminal app keeps its existing Bold face (TTC 6).
/// Noto Sans KR Bold ships as a separate file.
fn md_bold_font_path() -> (String, u32) {
    #[cfg(target_os = "macos")]
    {
        let home = kasa_socket::home_var().unwrap_or_default();
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
        return (
            "/System/Library/Fonts/AppleSDGothicNeo.ttc".to_string(),
            if crate::theme::viewer_chrome() { 4 } else { 6 },
        );
    }
    #[cfg(target_os = "windows")]
    {
        if let Some(path) = viewer_bundled_noto_path() {
            return (path, 0);
        }
        if crate::theme::viewer_chrome() {
            for path in windows_font_candidates(&[
                "NotoSansKR-SemiBold.ttf",
                "NotoSansKR-Bold.ttf",
                "NotoSansCJKkr-Bold.otf",
                "NotoSansCJK-Bold.ttc",
            ]) {
                if std::path::Path::new(&path).exists() {
                    return (path, 0);
                }
            }
        }
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








/// 검증 전용: NSView 를 창의 절반으로 줄여 사용자가 본 상태를 그대로 만든다.
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

/// 창을 **다른 물리 모니터로 옮긴다**. 검증 전용 — 사용자가 손으로 하는
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

#[cfg(test)]
mod account_icon_tests {
    use super::GpuRenderer;
    use kasa_cells::Shaper;

    #[test]
    fn official_email_assets_parse_and_paint_pixels() {
        for (name, width, height) in [("gmail", 28, 28), ("google", 16, 16), ("naver", 84, 16)] {
            let svg = GpuRenderer::icon_svg(name).expect("registered account icon");
            let rgba = GpuRenderer::rasterize_icon_color(svg, width, height)
                .expect("official SVG must parse");
            assert_eq!(rgba.len(), (width * height * 4) as usize);
            assert!(
                rgba.chunks_exact(4).any(|pixel| pixel[3] > 0),
                "{name} rendered blank"
            );
        }
        assert!(GpuRenderer::icon_svg("mail").is_some());
    }

    #[test]
    fn bundled_noto_variable_rasterizes_distinct_regular_and_semibold_weights() {
        let bytes = include_bytes!("../assets/fonts/NotoSansKR-Variable.ttf").to_vec();
        let mut regular = Shaper::from_bytes(bytes.clone(), 0).expect("bundled Noto regular");
        regular.set_variation_weight(400.0);
        let mut semibold = Shaper::from_bytes(bytes, 0).expect("bundled Noto semibold");
        semibold.set_variation_weight(600.0);
        let regular = regular.rasterize('한', 28.0).expect("regular Hangul glyph");
        let semibold = semibold.rasterize('한', 28.0).expect("semibold Hangul glyph");
        assert_eq!(regular.advance, semibold.advance);
        assert_ne!(regular.data, semibold.data);
    }
}
