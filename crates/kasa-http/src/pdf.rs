//! PDF 카드의 쪽 그림 — 등록 때 첫 쪽(카드 포스터)과 `/term/inline-media/page` 의 쪽. 순수 Rust(hayro)라 맥·윈도가
//! 같은 길이고, 다른 기기(거울 PC·폰)는 PDF 를 풀 줄 몰라도 PNG 만 받아 보인다.

use hayro::hayro_interpret::font::FontQuery;
use hayro::hayro_interpret::InterpreterSettings;
use hayro::hayro_syntax::Pdf;
use hayro::vello_cpu::color::palette::css::WHITE;
use hayro::{PixmapSettings, RenderCache, RenderSettings};
use std::path::Path;
use std::sync::{Arc, OnceLock};

/// 쪽 그림의 긴 변. 결과 그림·포스터 상한과 같다.
pub const EDGE: u32 = 1600;
/// 쪽을 그리려고 통째로 읽는 PDF 상한. 넘는 PDF 는 카드(첫 쪽)만 서고 쪽 보기는 원본 저장으로 본다.
pub const MAX_RENDER_BYTES: u64 = 256 * 1024 * 1024;

pub fn is_pdf(path: &Path) -> bool {
    use std::io::Read;
    let mut head = [0u8; 5];
    std::fs::File::open(path).and_then(|mut f| f.read_exact(&mut head)).is_ok() && &head == b"%PDF-"
}

/// 등록 정보 — 쪽 수와 첫 쪽 PNG(긴 변 [`EDGE`]).
pub struct Probe {
    pub pages: u32,
    pub poster: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

pub fn probe(path: &Path) -> anyhow::Result<Probe> {
    let meta = std::fs::metadata(path)?;
    anyhow::ensure!(meta.len() <= MAX_RENDER_BYTES, "PDF 가 256MiB 를 넘습니다");
    let pdf = open(Arc::new(std::fs::read(path)?))?;
    let pages = pdf.pages().len();
    anyhow::ensure!(pages > 0, "쪽이 없는 PDF 입니다");
    let (poster, width, height) = render(&pdf, 0, EDGE)?;
    Ok(Probe { pages: pages as u32, poster, width, height })
}

/// `index`(0부터) 쪽을 긴 변 `edge` 의 PNG 로.
pub fn render_page(data: Arc<Vec<u8>>, index: usize, edge: u32) -> anyhow::Result<Vec<u8>> {
    let pdf = open(data)?;
    anyhow::ensure!(index < pdf.pages().len(), "page out of range");
    Ok(render(&pdf, index, edge)?.0)
}

/// 등록된 PDF 카드의 `n`(1부터) 쪽 PNG — HTTP 창구와 이 기기 카드의 미리보기가 같이 쓴다. 원본은 등록 해시로
/// 확인된 조각으로만 읽는다.
pub fn registered_page(store: &kasa_socket::inline_media::Store, id: &str, n: u32, edge: u32) -> std::io::Result<Vec<u8>> {
    let invalid = |m: &str| std::io::Error::new(std::io::ErrorKind::InvalidInput, m.to_string());
    let media = store.metadata(id)?;
    if media.kind != "pdf" || n == 0 || n > media.pages.unwrap_or(0) {
        return Err(invalid("no such page"));
    }
    if media.size > MAX_RENDER_BYTES {
        return Err(invalid("document too large to render"));
    }
    let data = registered_bytes(store, id, media.size)?;
    render_page(data, (n - 1) as usize, edge).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))
}

/// 쪽을 넘길 때마다 원본을 다시 읽고 해시를 맞추지 않는다 — 마지막 문서 하나만 붙잡는다.
fn registered_bytes(store: &kasa_socket::inline_media::Store, id: &str, size: u64) -> std::io::Result<Arc<Vec<u8>>> {
    static LAST: OnceLock<std::sync::Mutex<Option<(String, Arc<Vec<u8>>)>>> = OnceLock::new();
    let last = LAST.get_or_init(Default::default);
    if let Some((_, data)) = last.lock().ok().and_then(|l| l.clone()).filter(|(held, _)| held == id) {
        return Ok(data);
    }
    let mut data = Vec::with_capacity(size as usize);
    while (data.len() as u64) < size {
        data.extend(store.read_chunk(id, data.len() as u64, kasa_socket::inline_media::MAX_CHUNK)?);
    }
    let data = Arc::new(data);
    if let Ok(mut last) = last.lock() {
        *last = Some((id.to_owned(), data.clone()));
    }
    Ok(data)
}

fn open(data: Arc<Vec<u8>>) -> anyhow::Result<Pdf> {
    Pdf::new(data).map_err(|e| anyhow::anyhow!("PDF 를 읽지 못했습니다: {e:?}"))
}

fn render(pdf: &Pdf, index: usize, edge: u32) -> anyhow::Result<(Vec<u8>, u32, u32)> {
    let pages = pdf.pages();
    let page = pages.get(index).ok_or_else(|| anyhow::anyhow!("page out of range"))?;
    let (w, h) = page.render_dimensions();
    anyhow::ensure!(w > 0.0 && h > 0.0, "쪽 크기가 없습니다");
    let scale = edge as f32 / w.max(h);
    let cache = RenderCache::new();
    let pixmap = hayro::render(
        page,
        &cache,
        &settings(),
        &RenderSettings::default(),
        &PixmapSettings { x_scale: scale, y_scale: scale, bg_color: WHITE },
    );
    let (pw, ph) = (u32::from(pixmap.width()), u32::from(pixmap.height()));
    let rgba = image::RgbaImage::from_raw(pw, ph, pixmap.data_as_u8_slice().to_vec())
        .ok_or_else(|| anyhow::anyhow!("쪽 그림 크기가 어긋났습니다"))?;
    let mut png = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(rgba).write_to(&mut png, image::ImageFormat::Png)?;
    Ok((png.into_inner(), pw, ph))
}

/// 글꼴을 싣지 않은 한·중·일 문서는 기본 대체 글꼴(라틴 표준 14종)로는 글자가 통째로 빠진다 — 그때만 이 기기의
/// 한글 글꼴로 대신한다(한자·가나도 대부분 든다).
fn settings() -> InterpreterSettings {
    let default = InterpreterSettings::default();
    let fallback = default.font_resolver.clone();
    InterpreterSettings {
        font_resolver: Arc::new(move |query| match query {
            FontQuery::Fallback(f) if f.character_collection.is_some() => cjk_font().or_else(|| fallback(query)),
            _ => fallback(query),
        }),
        ..default
    }
}

fn cjk_font() -> Option<(hayro::hayro_interpret::font::FontData, u32)> {
    static FONT: OnceLock<Option<Arc<Vec<u8>>>> = OnceLock::new();
    let font = FONT.get_or_init(|| {
        let candidates: &[&str] = if cfg!(target_os = "macos") {
            &["/System/Library/Fonts/AppleSDGothicNeo.ttc", "/System/Library/Fonts/Supplemental/AppleGothic.ttf"]
        } else if cfg!(windows) {
            &["C:\\Windows\\Fonts\\malgun.ttf", "C:\\Windows\\Fonts\\gulim.ttc"]
        } else {
            &["/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc"]
        };
        candidates.iter().find_map(|p| std::fs::read(p).ok()).map(Arc::new)
    });
    font.clone().map(|data| (data as hayro::hayro_interpret::font::FontData, 0))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// 쪽마다 색 사각 하나인 아주 작은 PDF — 글꼴 없이 그리기·쪽 수만 본다.
    pub(crate) fn fixture(colors: &[(f32, f32, f32)]) -> Vec<u8> {
        let mut objects: Vec<String> = Vec::new();
        let n = colors.len();
        objects.push("<< /Type /Catalog /Pages 2 0 R >>".into());
        let kids: Vec<String> = (0..n).map(|i| format!("{} 0 R", 3 + i * 2)).collect();
        objects.push(format!("<< /Type /Pages /Kids [{}] /Count {n} >>", kids.join(" ")));
        for (i, (r, g, b)) in colors.iter().enumerate() {
            objects.push(format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Contents {} 0 R >>", 4 + i * 2));
            let stream = format!("{r} {g} {b} rg 0 0 200 100 re f");
            objects.push(format!("<< /Length {} >>\nstream\n{stream}\nendstream", stream.len()));
        }
        let mut out = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::new();
        for (i, body) in objects.iter().enumerate() {
            offsets.push(out.len());
            out.extend_from_slice(format!("{} 0 obj\n{body}\nendobj\n", i + 1).as_bytes());
        }
        let xref = out.len();
        out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes());
        for o in offsets {
            out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objects.len() + 1).as_bytes());
        out
    }

    #[test]
    fn pages_render_at_the_card_edge_with_their_own_content() {
        let pdf = Arc::new(fixture(&[(1.0, 0.0, 0.0), (0.0, 0.0, 1.0)]));
        let dir = std::env::temp_dir().join(format!("kasa-pdf-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("two.pdf");
        std::fs::write(&path, pdf.as_slice()).unwrap();
        assert!(is_pdf(&path));
        let probe = probe(&path).unwrap();
        assert_eq!((probe.pages, probe.width, probe.height), (2, 1600, 800));
        let second = image::load_from_memory(&render_page(pdf.clone(), 1, 400).unwrap()).unwrap().to_rgba8();
        assert_eq!(second.dimensions(), (400, 200));
        assert_eq!(second.get_pixel(200, 100).0, [0, 0, 255, 255]);
        assert!(render_page(pdf, 2, 400).is_err());
        std::fs::write(&path, b"not a pdf").unwrap();
        assert!(!is_pdf(&path));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
