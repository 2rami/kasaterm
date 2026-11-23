//! 화면 글자 — 시스템 고딕(맥 Apple SD Gothic Neo, Windows 설치본 Noto Sans KR → 맑은 고딕)으로 그린다. 격자 글꼴(고정폭)은
//! 한글이 한 칸씩 벌어져 보여, 본 앱 크롬처럼 비례 고딕을 쓴다. 글리프는 창 렌더러의 아틀라스에 글꼴 칸 1(보통)·2(굵게)로
//! 굽는다 — 격자 글꼴(칸 0)과 안 섞인다.

use kasa_cells::pipeline::CellInstance;
use kasa_cells::{GlyphKey, Shaper};
use kasa_gridview::renderer::{pen_step, srgb_rgba_to_linear};
use kasa_gridview::GridRenderer;

const REGULAR: u8 = 1;
const BOLD: u8 = 2;

fn faces() -> [(String, u32); 2] {
    #[cfg(target_os = "macos")]
    {
        let sd = "/System/Library/Fonts/AppleSDGothicNeo.ttc".to_string();
        [(sd.clone(), 0), (sd, 4)]
    }
    #[cfg(windows)]
    {
        let bundled = std::env::current_exe().ok().and_then(|e| Some(e.parent()?.join("fonts").join("NotoSansKR-Variable.ttf"))).filter(|p| p.is_file());
        match bundled {
            Some(p) => {
                let p = p.to_string_lossy().into_owned();
                [(p.clone(), 0), (p, 0)]
            }
            None => [(r"C:\Windows\Fonts\malgun.ttf".into(), 0), (r"C:\Windows\Fonts\malgunbd.ttf".into(), 0)],
        }
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        let noto = "/usr/share/fonts/truetype/noto/NotoSansCJK-Regular.ttc".to_string();
        [(noto.clone(), 0), (noto, 0)]
    }
}

/// 창 하나의 글자 손. 고딕을 못 실으면 격자 글꼴로 물러선다.
pub struct Ui {
    shapers: Option<(Shaper, Shaper)>,
}

impl Ui {
    pub fn new() -> Self {
        let [(regular, ri), (bold, bi)] = faces();
        let load = |path: &str, index: u32| Shaper::from_path(path, index).map_err(|e| eprintln!("[kasadesk] 글꼴 {path} 을 못 싣는다: {e:#}")).ok();
        let regular = load(&regular, ri);
        let bold = load(&bold, bi).or_else(|| load(&faces()[0].0, faces()[0].1));
        #[cfg_attr(not(windows), allow(unused_mut))]
        let mut shapers = regular.zip(bold);
        // 설치본 Noto 는 가변 글꼴이라 굵기를 정해 준다.
        #[cfg(windows)]
        if let Some((r, b)) = shapers.as_mut() {
            r.set_variation_weight(400.0);
            b.set_variation_weight(650.0);
        }
        Self { shapers }
    }

    pub fn draw(&mut self, g: &mut GridRenderer, x: f32, y: f32, text: &str, size: f32, color: [u8; 4], bold: bool) -> f32 {
        self.run(g, Some((x, y, color)), text, size, bold)
    }

    pub fn measure(&mut self, g: &mut GridRenderer, text: &str, size: f32, bold: bool) -> f32 {
        self.run(g, None, text, size, bold)
    }

    fn run(&mut self, g: &mut GridRenderer, at: Option<(f32, f32, [u8; 4])>, text: &str, size: f32, bold: bool) -> f32 {
        let Some((regular, heavy)) = self.shapers.as_mut() else {
            return match at {
                Some((x, y, c)) => g.draw_label(x, y, text, size, c),
                None => g.measure_label(text, size),
            };
        };
        let (shaper, font) = if bold { (heavy, BOLD) } else { (regular, REGULAR) };
        let s = g.scale;
        let size_px = (size * s).round().max(1.0) as u32;
        let space = shaper.advance(' ', size_px as f32);
        let origin = at.map_or(0.0, |(x, _, _)| x * s);
        let baseline = at.map_or(0.0, |(_, y, _)| y * s + size_px as f32 * 0.8);
        let fg = at.map(|(_, _, c)| srgb_rgba_to_linear(c));
        let mut pen = origin;
        for ch in text.chars() {
            if ch == ' ' {
                pen += space;
                continue;
            }
            let key = GlyphKey { ch, bold: false, italic: false, size_px, font };
            let Some(entry) = g.atlas.get_or_bake(&g.device, &g.queue, shaper, key) else { continue };
            if let Some(fg_rgba) = fg.filter(|_| entry.px_w > 0 && entry.px_h > 0) {
                g.chrome.push(CellInstance {
                    cell_px: [pen + entry.bearing_x as f32, baseline - entry.bearing_y as f32, entry.px_w as f32, entry.px_h as f32],
                    uv_min: entry.uv_min,
                    uv_max: entry.uv_max,
                    fg_rgba,
                    ..Default::default()
                });
            }
            pen += pen_step(ch, entry.advance, size_px as f32);
        }
        (pen - origin) / s
    }
}

/// 보기 HUD(`kasa_castview::paint`)가 이 고딕으로 그리게 거는 판.
pub struct Pen<'a> {
    pub g: &'a mut GridRenderer,
    pub ui: &'a mut Ui,
}

impl kasa_castview::Canvas for Pen<'_> {
    fn grid(&mut self) -> &mut GridRenderer {
        self.g
    }
    fn measure(&mut self, text: &str, size: f32) -> f32 {
        self.ui.measure(self.g, text, size, false)
    }
    fn text(&mut self, x: f32, y: f32, text: &str, size: f32, color: [u8; 4]) {
        self.ui.draw(self.g, x, y, text, size, color, false);
    }
}
