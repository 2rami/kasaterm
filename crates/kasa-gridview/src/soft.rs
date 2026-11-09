//! CPU 합성 — GPU 가 없는 기계(KubeVirt VM 의 Mesa llvmpipe 등)에서 쓰는 그리기 길.
//!
//! GPU 경로와 **같은 인스턴스 목록**을 받는다. 셀·크롬을 쌓는 코드는 한 벌이고, 여기서는 그
//! 목록을 셰이더(`kasa-cells/src/shader.wgsl`)와 같은 셈으로 픽셀에 칠할 뿐이다. GPU 없는 곳의
//! GL 은 결국 CPU 가 매 장 창 전체를 칠하는 llvmpipe 라서, 직접 칠하면서 **바뀐 띠만** 다시
//! 칠하고 X 서버에도 그 영역만 넘기는 쪽이 싸다(VNC 도 바뀐 영역만 보낸다).
//!
//! 바뀐 곳 찾기: 화면을 `STRIP` px 가로 띠로 나누고, 띠에 걸친 그리기 하나하나의 값(자리·uv·색·
//! 클립·그림 번호)을 띠마다 해시로 접는다. 앞 장과 해시가 같은 띠는 칠하지도 내보내지도 않는다.
//! 호스트가 무엇이 바뀌었는지 알려 줄 필요가 없다.

use std::collections::HashMap;
use std::num::NonZeroU32;
use std::sync::Arc;

use anyhow::{anyhow, Result};
use kasa_cells::pipeline::CellInstance;
use winit::window::Window;

const STRIP: u32 = 16;

/// CPU 로 그릴까. `KASATERM_RENDERER=cpu|gpu` 가 이긴다. 없으면 리눅스에서 GPU 렌더 노드
/// (`/dev/dri/renderD*`)가 없을 때만 CPU — 그런 기계의 GL 은 어차피 CPU 가 칠하는 llvmpipe 다.
/// 맥·윈도는 늘 GPU.
pub fn wanted() -> bool {
    match std::env::var("KASATERM_RENDERER").as_deref() {
        Ok("cpu") | Ok("soft") => return true,
        Ok("gpu") => return false,
        _ => {}
    }
    cfg!(target_os = "linux") && !has_render_node()
}

fn has_render_node() -> bool {
    std::fs::read_dir("/dev/dri")
        .map(|d| {
            d.flatten()
                .any(|e| e.file_name().to_string_lossy().starts_with("renderD"))
        })
        .unwrap_or(false)
}
const ANIMATED: u32 = CellInstance::FLAG_BAND_SWEEP | CellInstance::FLAG_BAND_BREATH | CellInstance::FLAG_BAND_FILL;

pub struct SoftImage {
    pub w: u32,
    pub h: u32,
    pub rgba: Vec<u8>,
    /// 같은 키로 다시 올린 그림을 다른 그림으로 알아보게 하는 번호.
    serial: u64,
}

pub struct SoftPresenter {
    _context: softbuffer::Context<Arc<Window>>,
    surface: softbuffer::Surface<Arc<Window>, Arc<Window>>,
    w: u32,
    h: u32,
    /// 지난 장까지 칠한 화면(0x00RRGGBB). 띠 하나만 다시 칠하려면 나머지가 남아 있어야 한다.
    frame: Vec<u32>,
    strip_hash: Vec<u64>,
    /// 다음 장은 해시와 상관없이 전부 칠한다(크기 변경·아틀라스 재배치).
    full: bool,
    alpha_lut: [u8; 256],
    sat: f32,
    pub images: HashMap<String, SoftImage>,
    next_serial: u64,
}

/// 한 장에 칠할 것 — 렌더러가 자기 목록을 빌려준다.
pub struct SoftFrame<'a> {
    pub chrome: &'a [CellInstance],
    /// `(chrome 인덱스, 그 뒤로 유효한 클립)` — PHYSICAL `[x, y, w, h]`.
    pub runs: &'a [(u32, Option<[u32; 4]>)],
    pub image_quads: &'a [(String, CellInstance, u32, Option<[u32; 4]>)],
    pub icon_quads: &'a [(String, CellInstance, u32, Option<[u32; 4]>)],
    pub atlas: &'a [u8],
    pub atlas_w: u32,
    pub atlas_h: u32,
    pub bg: [u8; 4],
    pub time: f32,
}

#[derive(Clone, Copy)]
enum Src<'a> {
    Atlas,
    Image(&'a SoftImage),
}

struct Op<'a> {
    inst: &'a CellInstance,
    src: Src<'a>,
    /// 화면·클립과 교집합한 자리 `[x0, y0, x1, y1)`.
    rect: [i32; 4],
    /// 반올림한 쿼드 전체 자리 — uv 보간의 기준.
    quad: [i32; 4],
}

impl SoftPresenter {
    pub fn new(window: Arc<Window>, w: u32, h: u32, gamma: f32, contrast: f32, sat: f32) -> Result<Self> {
        let context = softbuffer::Context::new(window.clone()).map_err(|e| anyhow!("softbuffer context: {e}"))?;
        let surface = softbuffer::Surface::new(&context, window).map_err(|e| anyhow!("softbuffer surface: {e}"))?;
        let mut alpha_lut = [0u8; 256];
        for (i, slot) in alpha_lut.iter_mut().enumerate() {
            let a = (i as f32 / 255.0).powf(1.0 / gamma.max(0.001)) * contrast;
            *slot = (a.clamp(0.0, 1.0) * 255.0).round() as u8;
        }
        let mut me = Self {
            _context: context,
            surface,
            w: 0,
            h: 0,
            frame: Vec::new(),
            strip_hash: Vec::new(),
            full: true,
            alpha_lut,
            sat,
            images: HashMap::new(),
            next_serial: 0,
        };
        me.resize(w, h)?;
        Ok(me)
    }

    pub fn resize(&mut self, w: u32, h: u32) -> Result<()> {
        let (w, h) = (w.max(1), h.max(1));
        if (w, h) == (self.w, self.h) {
            return Ok(());
        }
        self.surface
            .resize(NonZeroU32::new(w).unwrap(), NonZeroU32::new(h).unwrap())
            .map_err(|e| anyhow!("softbuffer resize: {e}"))?;
        self.w = w;
        self.h = h;
        self.frame = vec![0; (w * h) as usize];
        self.strip_hash = vec![0; h.div_ceil(STRIP) as usize];
        self.full = true;
        Ok(())
    }

    pub fn invalidate(&mut self) {
        self.full = true;
    }

    pub fn put_image(&mut self, id: &str, rgba: &[u8], w: u32, h: u32) {
        self.next_serial += 1;
        self.images.insert(
            id.to_string(),
            SoftImage {
                w,
                h,
                rgba: rgba.to_vec(),
                serial: self.next_serial,
            },
        );
    }

    /// 지금 화면을 RGBA 로 — 자기 캡처용. `crop` 은 PHYSICAL `(x, y, w, h)`.
    pub fn snapshot_rgba(&self, crop: Option<(u32, u32, u32, u32)>) -> (Vec<u8>, u32, u32) {
        let (cx, cy, cw, ch) = crop_box(crop, self.w, self.h);
        let mut out = Vec::with_capacity((cw * ch * 4) as usize);
        for y in cy..cy + ch {
            let row = &self.frame[(y * self.w + cx) as usize..(y * self.w + cx + cw) as usize];
            for &p in row {
                out.extend_from_slice(&[(p >> 16) as u8, (p >> 8) as u8, p as u8, 0xFF]);
            }
        }
        (out, cw, ch)
    }

    /// 한 장을 칠하고 바뀐 띠만 내보낸다. 바뀐 띠가 없으면 내보내지도 않는다.
    pub fn render(&mut self, f: &SoftFrame<'_>) -> Result<()> {
        let ops = collect_ops(&self.images, self.w, self.h, f);
        let strips = self.strip_hash.len();
        let seed = mix(
            mix(0x6b61_7361_6c69_7465, u32::from_le_bytes(f.bg) as u64),
            ((self.w as u64) << 32) | self.h as u64,
        );
        let mut hashes = vec![seed; strips];
        let time_bits = f.time.to_bits() as u64;
        for op in &ops {
            let mut h = inst_hash(op.inst);
            h = mix(h, ((op.rect[0] as u64) << 32) ^ op.rect[2] as u64);
            if let Src::Image(img) = op.src {
                h = mix(h, img.serial);
            }
            if op.inst.flags & ANIMATED != 0 {
                h = mix(h, time_bits);
            }
            let (s0, s1) = strip_span(op.rect[1], op.rect[3]);
            for slot in &mut hashes[s0..s1] {
                *slot = mix(*slot, h);
            }
        }
        let dirty: Vec<bool> = hashes
            .iter()
            .zip(self.strip_hash.iter())
            .map(|(a, b)| self.full || a != b)
            .collect();
        self.strip_hash = hashes;
        let spans = dirty_spans(&dirty, self.h);
        if spans.is_empty() {
            return Ok(());
        }
        let bg = rgb(f.bg[0], f.bg[1], f.bg[2]);
        for &(y0, y1) in &spans {
            self.frame[(y0 * self.w) as usize..(y1 * self.w) as usize].fill(bg);
        }
        let ctx = Paint {
            atlas: f.atlas,
            atlas_w: f.atlas_w,
            atlas_h: f.atlas_h,
            time: f.time,
            lut: &self.alpha_lut,
            sat: self.sat,
        };
        for op in &ops {
            for &(y0, y1) in &spans {
                let ry0 = op.rect[1].max(y0 as i32);
                let ry1 = op.rect[3].min(y1 as i32);
                if ry0 < ry1 {
                    paint_op(&mut self.frame, self.w, op, ry0, ry1, &ctx);
                }
            }
        }
        let full = std::mem::take(&mut self.full);
        let mut buf = self
            .surface
            .buffer_mut()
            .map_err(|e| anyhow!("softbuffer buffer: {e}"))?;
        // age 1 = 이 버퍼가 바로 앞 장을 들고 있다 — 바뀐 띠만 옮기면 된다.
        if !full && buf.age() == 1 {
            for &(y0, y1) in &spans {
                let r = (y0 * self.w) as usize..(y1 * self.w) as usize;
                buf[r.clone()].copy_from_slice(&self.frame[r]);
            }
        } else {
            buf.copy_from_slice(&self.frame);
        }
        let damage: Vec<softbuffer::Rect> = spans
            .iter()
            .map(|&(y0, y1)| softbuffer::Rect {
                x: 0,
                y: y0,
                width: NonZeroU32::new(self.w).unwrap(),
                height: NonZeroU32::new(y1 - y0).unwrap(),
            })
            .collect();
        buf.present_with_damage(&damage)
            .map_err(|e| anyhow!("softbuffer present: {e}"))?;
        Ok(())
    }
}

/// GPU 경로의 그리기 순서를 그대로 편다: chrome 을 클립 경계마다 끊고, 그림·아이콘은 큐에
/// 들어온 순간의 chrome 길이(워터마크) 자리에 끼운다.
fn collect_ops<'a>(images: &'a HashMap<String, SoftImage>, w: u32, h: u32, f: &SoftFrame<'a>) -> Vec<Op<'a>> {
    let mut quads: Vec<(u32, &'a str, &'a CellInstance, Option<[u32; 4]>)> = f
        .image_quads
        .iter()
        .chain(f.icon_quads.iter())
        .map(|(id, inst, wm, clip)| (*wm, id.as_str(), inst, *clip))
        .collect();
    quads.sort_by_key(|q| q.0);
    let mut ops = Vec::with_capacity(f.chrome.len() + quads.len());
    let mut run_i = 0usize;
    let mut cur: Option<[u32; 4]> = None;
    let push_chrome = |ops: &mut Vec<Op<'a>>, from: usize, to: usize, run_i: &mut usize, cur: &mut Option<[u32; 4]>| {
        for i in from..to {
            while *run_i < f.runs.len() && f.runs[*run_i].0 as usize <= i {
                *cur = f.runs[*run_i].1;
                *run_i += 1;
            }
            if let Some(op) = make_op(w, h, &f.chrome[i], Src::Atlas, *cur) {
                ops.push(op);
            }
        }
    };
    let mut drawn = 0usize;
    for (wm, id, inst, clip) in quads {
        let upto = (wm as usize).min(f.chrome.len());
        if upto > drawn {
            push_chrome(&mut ops, drawn, upto, &mut run_i, &mut cur);
            drawn = upto;
        }
        if let Some(img) = images.get(id) {
            if let Some(op) = make_op(w, h, inst, Src::Image(img), clip) {
                ops.push(op);
            }
        }
    }
    push_chrome(&mut ops, drawn, f.chrome.len(), &mut run_i, &mut cur);
    ops
}

fn make_op<'a>(w: u32, h: u32, inst: &'a CellInstance, src: Src<'a>, clip: Option<[u32; 4]>) -> Option<Op<'a>> {
    let [qx, qy, qw, qh] = inst.cell_px;
    // 셰이더의 pixel_perfect_quad 처럼 모서리를 정수 픽셀로 반올림한다.
    let quad = [
        qx.round() as i32,
        qy.round() as i32,
        (qx + qw).round() as i32,
        (qy + qh).round() as i32,
    ];
    let mut r = [
        quad[0].max(0),
        quad[1].max(0),
        quad[2].min(w as i32),
        quad[3].min(h as i32),
    ];
    if let Some([cx, cy, cw, ch]) = clip {
        r[0] = r[0].max(cx as i32);
        r[1] = r[1].max(cy as i32);
        r[2] = r[2].min((cx + cw) as i32);
        r[3] = r[3].min((cy + ch) as i32);
    }
    (r[0] < r[2] && r[1] < r[3]).then_some(Op {
        inst,
        src,
        rect: r,
        quad,
    })
}

struct Paint<'a> {
    atlas: &'a [u8],
    atlas_w: u32,
    atlas_h: u32,
    time: f32,
    lut: &'a [u8; 256],
    sat: f32,
}

/// 쿼드 하나의 `[ry0, ry1)` 행들을 칠한다. 색 셈은 `shader.wgsl` 의 `fs_main` 과 같다.
fn paint_op(frame: &mut [u32], fw: u32, op: &Op<'_>, ry0: i32, ry1: i32, p: &Paint<'_>) {
    let inst = op.inst;
    let [x0, _, x1, _] = op.rect;
    let [qx0, qy0, qx1, qy1] = op.quad;
    let (qw, qh) = ((qx1 - qx0).max(1) as f32, (qy1 - qy0).max(1) as f32);
    let fg = inst.fg_rgba;
    let fa = fg[3].clamp(0.0, 1.0);
    let [fr, fgc, fb] = saturate([fg[0], fg[1], fg[2]], p.sat);
    let (cr, cg, cb) = (to8(fr), to8(fgc), to8(fb));
    let u_at = |x: i32| inst.uv_min[0] + (inst.uv_max[0] - inst.uv_min[0]) * ((x as f32 + 0.5 - qx0 as f32) / qw);
    let v_at = |y: i32| inst.uv_min[1] + (inst.uv_max[1] - inst.uv_min[1]) * ((y as f32 + 0.5 - qy0 as f32) / qh);

    if inst.flags & ANIMATED != 0 {
        let t = p.time;
        for x in x0..x1 {
            let u = u_at(x);
            let k = if inst.flags & CellInstance::FLAG_BAND_SWEEP != 0 {
                let seg = 0.32;
                let head = -seg + (1.0 + seg) * (t / 1.2).fract();
                if head <= u && u <= head + seg {
                    1.0
                } else {
                    0.22
                }
            } else if inst.flags & CellInstance::FLAG_BAND_FILL != 0 {
                if u <= (t / 2.4).fract() {
                    1.0
                } else {
                    0.18
                }
            } else {
                0.30 + 0.45 * (0.5 + 0.5 * (t * 2.094_395_1).sin())
            };
            let a = to8(fa * k) as u32;
            for y in ry0..ry1 {
                let d = &mut frame[(y as u32 * fw + x as u32) as usize];
                *d = blend(*d, cr, cg, cb, a);
            }
        }
        return;
    }

    let (tex, tw, th): (&[u8], u32, u32) = match op.src {
        Src::Atlas => (p.atlas, p.atlas_w, p.atlas_h),
        Src::Image(img) => (&img.rgba, img.w, img.h),
    };
    let color = inst.flags & CellInstance::FLAG_COLOR != 0;
    let icon = inst.flags & CellInstance::FLAG_ICON != 0;
    let fa8 = (fa * 256.0).round() as u32;
    let tu0 = inst.uv_min[0] * tw as f32;
    let tv0 = inst.uv_min[1] * th as f32;
    let tu1 = inst.uv_max[0] * tw as f32;
    let tv1 = inst.uv_max[1] * th as f32;

    // 흰 블록(SOLID_UV)을 가리키는 사각형 — 텍셀 알파가 255 로 고정이다.
    if matches!(op.src, Src::Atlas) && !color && tu1 <= 2.0 && tv1 <= 2.0 {
        let a = if icon {
            fa8.min(255)
        } else {
            (p.lut[255] as u32 * fa8) >> 8
        };
        let a = a.min(255);
        for y in ry0..ry1 {
            let row = &mut frame[(y as u32 * fw) as usize..][x0 as usize..x1 as usize];
            if a == 255 {
                row.fill(rgb(cr, cg, cb));
            } else {
                for d in row {
                    *d = blend(*d, cr, cg, cb, a);
                }
            }
        }
        return;
    }

    let texel = |tx: i32, ty: i32| -> [u8; 4] {
        let tx = tx.clamp(0, tw as i32 - 1) as usize;
        let ty = ty.clamp(0, th as i32 - 1) as usize;
        let i = (ty * tw as usize + tx) * 4;
        [tex[i], tex[i + 1], tex[i + 2], tex[i + 3]]
    };
    // 텍셀 크기 = 쿼드 크기면 1:1 로 옮긴다(아틀라스 글리프의 대부분). 아니면 셰이더의 Linear
    // 샘플러처럼 이중선형으로 읽는다.
    let one_to_one = ((tu1 - tu0) - qw).abs() < 0.01 && ((tv1 - tv0) - qh).abs() < 0.01;
    let sample = |x: i32, y: i32| -> [u8; 4] {
        if one_to_one {
            return texel(tu0.round() as i32 + (x - qx0), tv0.round() as i32 + (y - qy0));
        }
        let fx = u_at(x) * tw as f32 - 0.5;
        let fy = v_at(y) * th as f32 - 0.5;
        let (ix, iy) = (fx.floor(), fy.floor());
        let (ax, ay) = (fx - ix, fy - iy);
        let (ix, iy) = (ix as i32, iy as i32);
        let t00 = texel(ix, iy);
        let t10 = texel(ix + 1, iy);
        let t01 = texel(ix, iy + 1);
        let t11 = texel(ix + 1, iy + 1);
        let mut out = [0u8; 4];
        for c in 0..4 {
            let top = t00[c] as f32 + (t10[c] as f32 - t00[c] as f32) * ax;
            let bot = t01[c] as f32 + (t11[c] as f32 - t01[c] as f32) * ax;
            out[c] = (top + (bot - top) * ay).round() as u8;
        }
        out
    };
    for y in ry0..ry1 {
        let base = (y as u32 * fw) as usize;
        for x in x0..x1 {
            let t = sample(x, y);
            let d = &mut frame[base + x as usize];
            if color {
                let a = ((t[3] as u32 * fa8) >> 8).min(255);
                let [r, g, b] = if p.sat == 1.0 {
                    [t[0], t[1], t[2]]
                } else {
                    let s = saturate([t[0] as f32 / 255.0, t[1] as f32 / 255.0, t[2] as f32 / 255.0], p.sat);
                    [to8(s[0]), to8(s[1]), to8(s[2])]
                };
                *d = blend(*d, r, g, b, a);
            } else {
                let cov = if icon { t[3] } else { p.lut[t[3] as usize] };
                let a = ((cov as u32 * fa8) >> 8).min(255);
                *d = blend(*d, cr, cg, cb, a);
            }
        }
    }
}

fn saturate(c: [f32; 3], sat: f32) -> [f32; 3] {
    if sat == 1.0 {
        return c;
    }
    let luma = c[0] * 0.2126 + c[1] * 0.7152 + c[2] * 0.0722;
    c.map(|v| (luma + (v - luma) * sat).clamp(0.0, 1.0))
}

fn to8(v: f32) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0).round() as u8
}

fn rgb(r: u8, g: u8, b: u8) -> u32 {
    ((r as u32) << 16) | ((g as u32) << 8) | b as u32
}

/// src-over, 감마 공간 그대로(GPU 의 Unorm 표면 블렌드와 같다).
#[inline]
fn blend(d: u32, r: u8, g: u8, b: u8, a: u32) -> u32 {
    if a == 0 {
        return d;
    }
    if a >= 255 {
        return rgb(r, g, b);
    }
    let ia = 255 - a;
    let ch = |s: u8, shift: u32| {
        let x = s as u32 * a + ((d >> shift) & 0xFF) * ia + 128;
        (x + (x >> 8)) >> 8
    };
    (ch(r, 16) << 16) | (ch(g, 8) << 8) | ch(b, 0)
}

#[inline]
fn mix(h: u64, v: u64) -> u64 {
    (h ^ v).wrapping_mul(0x9E37_79B9_7F4A_7C15).rotate_left(27)
}

fn inst_hash(i: &CellInstance) -> u64 {
    let mut h = 0u64;
    for v in i.cell_px.iter().chain(&i.uv_min).chain(&i.uv_max).chain(&i.fg_rgba) {
        h = mix(h, v.to_bits() as u64);
    }
    mix(h, i.flags as u64)
}

fn strip_span(y0: i32, y1: i32) -> (usize, usize) {
    (
        (y0.max(0) as u32 / STRIP) as usize,
        (y1.max(1) as u32 - 1) as usize / STRIP as usize + 1,
    )
}

/// 바뀐 띠를 이어 붙인 행 구간들 `[y0, y1)`.
fn dirty_spans(dirty: &[bool], h: u32) -> Vec<(u32, u32)> {
    let mut out: Vec<(u32, u32)> = Vec::new();
    for (s, &d) in dirty.iter().enumerate() {
        if !d {
            continue;
        }
        let (y0, y1) = (s as u32 * STRIP, ((s as u32 + 1) * STRIP).min(h));
        match out.last_mut() {
            Some(last) if last.1 == y0 => last.1 = y1,
            _ => out.push((y0, y1)),
        }
    }
    out
}

fn crop_box(crop: Option<(u32, u32, u32, u32)>, w: u32, h: u32) -> (u32, u32, u32, u32) {
    match crop {
        Some((x, y, cw, ch)) => {
            let x = x.min(w.saturating_sub(1));
            let y = y.min(h.saturating_sub(1));
            (x, y, cw.min(w - x).max(1), ch.min(h - y).max(1))
        }
        None => (0, 0, w, h),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spans_merge_neighbours_and_clip_last_strip() {
        let d = [true, true, false, true];
        assert_eq!(dirty_spans(&d, 60), vec![(0, 32), (48, 60)]);
    }

    #[test]
    fn strip_span_covers_partial_strips() {
        assert_eq!(strip_span(0, 16), (0, 1));
        assert_eq!(strip_span(15, 17), (0, 2));
        assert_eq!(strip_span(32, 33), (2, 3));
    }

    #[test]
    fn blend_matches_src_over() {
        assert_eq!(blend(0x000000, 255, 255, 255, 255), 0xFFFFFF);
        assert_eq!(blend(0x123456, 255, 255, 255, 0), 0x123456);
        let mid = blend(0x000000, 255, 255, 255, 128);
        assert_eq!(mid >> 16, 128);
    }
}
