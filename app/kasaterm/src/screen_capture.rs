//! 기기 화면 거울의 화면 뜨기 — `kasa_mcp::screen_mirror` 에 꽂는다(`docs/screen-mirror.md`).
//!
//! 화면을 OS 가 줄여 주는 크기로 바로 받아(macOS 는 비트맵 문맥에 그리기, Windows 는 StretchBlt) 줄인 픽셀만
//! 만진다 — 5K 화면을 통째로 들고 줄이면 한 장에 수십 ms 가 더 든다. 줄인 픽셀이 지난 장과 같으면 인코딩하지 않는다.

use kasa_mcp::screen_mirror::{Ask, Denied, Display, Shot, Source};

pub(crate) fn install() {
    kasa_mcp::screen_mirror::install(std::sync::Arc::new(Screen));
}

struct Screen;

impl Source for Screen {
    fn displays(&self) -> Vec<Display> {
        os::displays()
    }

    fn capture(&self, ask: &Ask) -> Result<Option<Shot>, Denied> {
        let Some((px, w, h)) = os::grab(ask)? else { return Ok(None) };
        let hash = fingerprint(&px);
        if hash == ask.prev {
            return Ok(None);
        }
        let Some(jpeg) = jpeg(&px, os::ORDER, w, h, ask.quality) else { return Ok(None) };
        Ok(Some(Shot { hash, jpeg, width: w, height: h }))
    }

    fn request_permission(&self) {
        os::request_permission();
    }
}

/// 32비트 픽셀의 바이트 순서 — 마지막 바이트는 늘 버린다.
#[derive(Clone, Copy)]
enum Order {
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    Rgbx,
    #[cfg_attr(not(windows), allow(dead_code))]
    Bgrx,
}

/// 비율을 지켜 상자 안으로 줄인 크기. 키우지는 않는다.
fn fit(w: u32, h: u32, max_w: u32, max_h: u32) -> (u32, u32) {
    let scale = (f64::from(max_w) / f64::from(w.max(1)))
        .min(f64::from(max_h) / f64::from(h.max(1)))
        .min(1.0);
    let side = |v: u32| ((f64::from(v) * scale).round() as u32).max(1);
    (side(w), side(h))
}

/// 같은 화면을 걸러 내는 데만 쓰는 빠른 지문. 0 은 「지난 장 없음」이라 피한다.
fn fingerprint(px: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for word in px.chunks_exact(8) {
        let word = u64::from_le_bytes([word[0], word[1], word[2], word[3], word[4], word[5], word[6], word[7]]);
        h = (h ^ word).wrapping_mul(0x0100_0000_01b3);
    }
    h.max(1)
}

fn jpeg(px: &[u8], order: Order, w: u32, h: u32, quality: u8) -> Option<Vec<u8>> {
    let rgb: Vec<u8> = match order {
        Order::Rgbx => px.chunks_exact(4).flat_map(|p| [p[0], p[1], p[2]]).collect(),
        Order::Bgrx => px.chunks_exact(4).flat_map(|p| [p[2], p[1], p[0]]).collect(),
    };
    let mut out = Vec::with_capacity(rgb.len() / 12);
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality)
        .encode(&rgb, w, h, image::ExtendedColorType::Rgb8)
        .ok()?;
    Some(out)
}

#[cfg(target_os = "macos")]
mod os {
    use super::{fit, Ask, Denied, Display, Order};
    use objc2_core_foundation::{CFRetained, CGPoint, CGRect, CGSize};
    use objc2_core_graphics::{
        CGBitmapContextCreate, CGContext, CGImage, CGImageAlphaInfo, CGImageByteOrderInfo, CGInterpolationQuality,
    };

    pub(super) const ORDER: Order = Order::Rgbx;

    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGPreflightScreenCaptureAccess() -> bool;
        fn CGRequestScreenCaptureAccess() -> bool;
        fn CGGetActiveDisplayList(max: u32, displays: *mut u32, count: *mut u32) -> i32;
        fn CGMainDisplayID() -> u32;
        fn CGDisplayBounds(display: u32) -> CGRect;
    }

    pub(super) fn displays() -> Vec<Display> {
        let mut ids = [0u32; 16];
        let mut count = 0u32;
        if unsafe { CGGetActiveDisplayList(ids.len() as u32, ids.as_mut_ptr(), &mut count) } != 0 {
            return Vec::new();
        }
        let main = unsafe { CGMainDisplayID() };
        ids[..(count as usize).min(ids.len())]
            .iter()
            .map(|&id| {
                let b = unsafe { CGDisplayBounds(id) };
                Display { id, width: b.size.width as u32, height: b.size.height as u32, main: id == main }
            })
            .collect()
    }

    pub(super) fn request_permission() {
        unsafe { CGRequestScreenCaptureAccess() };
    }

    /// SDK 는 ScreenCaptureKit 로 밀어냈지만 한 장 뜨기는 이 함수가 가장 단순하다. 헤더에서 빠져도 심볼은 있어
    /// 런타임에 찾는다(macos_sparkle.rs 의 `CGWindowListCreateImage` 와 같은 길).
    fn display_image(display: u32) -> Option<CFRetained<CGImage>> {
        type Create = unsafe extern "C" fn(u32) -> *mut CGImage;
        let symbol = unsafe { libc::dlsym(libc::RTLD_DEFAULT, c"CGDisplayCreateImage".as_ptr()) };
        if symbol.is_null() {
            return None;
        }
        let create: Create = unsafe { std::mem::transmute(symbol) };
        let image = std::ptr::NonNull::new(unsafe { create(display) })?;
        Some(unsafe { CFRetained::from_raw(image) })
    }

    pub(super) fn grab(ask: &Ask) -> Result<Option<(Vec<u8>, u32, u32)>, Denied> {
        // 권한 없이도 한 장이 뜨이지만 남의 창이 빠진 배경화면이다 — 그걸 화면이라고 보내면 안 된다.
        if !unsafe { CGPreflightScreenCaptureAccess() } {
            return Err(Denied::Permission);
        }
        // 잠든 화면·막 빠진 모니터는 잠깐 못 뜬다. 거절이 아니라 「이번 장 없음」이다.
        let Some(image) = display_image(ask.display) else { return Ok(None) };
        let (iw, ih) = (CGImage::width(Some(&image)) as u32, CGImage::height(Some(&image)) as u32);
        let (w, h) = fit(iw, ih, ask.max_w, ask.max_h);
        let mut px = vec![0u8; w as usize * h as usize * 4];
        // 화면 제 색 공간 그대로 그린다. sRGB 로 바꾸면 한 장에 16ms 가 더 들고(맥북 내장 화면 실측 41→25ms),
        // 보는 쪽에서 눈에 띄는 차이는 없다.
        let Some(space) = CGImage::color_space(Some(&image)) else { return Ok(None) };
        let info = CGImageAlphaInfo::NoneSkipLast.0 | CGImageByteOrderInfo::Order32Big.0;
        let Some(context) = (unsafe {
            CGBitmapContextCreate(px.as_mut_ptr().cast(), w as usize, h as usize, 8, w as usize * 4, Some(&space), info)
        }) else {
            return Ok(None);
        };
        CGContext::set_interpolation_quality(Some(&context), CGInterpolationQuality::Medium);
        let rect = CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(f64::from(w), f64::from(h)));
        CGContext::draw_image(Some(&context), rect, Some(&image));
        drop(context);
        Ok(Some((px, w, h)))
    }
}

#[cfg(windows)]
mod os {
    use super::{fit, Ask, Denied, Display, Order};
    use windows::Win32::Foundation::{LPARAM, RECT};
    use windows::Win32::Graphics::Gdi::{
        CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, EnumDisplayMonitors, GetDC, GetMonitorInfoW,
        ReleaseDC, SelectObject, SetStretchBltMode, StretchBlt, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, CAPTUREBLT,
        DIB_RGB_COLORS, HALFTONE, HDC, HMONITOR, MONITORINFO, SRCCOPY,
    };
    use windows::Win32::UI::WindowsAndMessaging::MONITORINFOF_PRIMARY;

    pub(super) const ORDER: Order = Order::Bgrx;

    /// 모니터의 가상 화면 사각형(물리 픽셀)과 주 모니터 여부. 번호는 이 차례다.
    fn monitors() -> Vec<(RECT, bool)> {
        unsafe extern "system" fn each(monitor: HMONITOR, _: HDC, _: *mut RECT, data: LPARAM) -> windows::core::BOOL {
            let list = unsafe { &mut *(data.0 as *mut Vec<(RECT, bool)>) };
            let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
            if unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool() {
                list.push((info.rcMonitor, info.dwFlags & MONITORINFOF_PRIMARY != 0));
            }
            true.into()
        }
        let mut list: Vec<(RECT, bool)> = Vec::new();
        unsafe {
            let _ = EnumDisplayMonitors(None, None, Some(each), LPARAM(&mut list as *mut Vec<(RECT, bool)> as isize));
        }
        list
    }

    pub(super) fn displays() -> Vec<Display> {
        monitors()
            .into_iter()
            .enumerate()
            .map(|(i, (r, main))| Display {
                id: i as u32,
                width: (r.right - r.left).max(0) as u32,
                height: (r.bottom - r.top).max(0) as u32,
                main,
            })
            .collect()
    }

    pub(super) fn request_permission() {}

    pub(super) fn grab(ask: &Ask) -> Result<Option<(Vec<u8>, u32, u32)>, Denied> {
        let list = monitors();
        let Some((r, _)) = list.get(ask.display as usize).or(list.first()).copied() else {
            return Err(Denied::Unavailable);
        };
        let (sw, sh) = (r.right - r.left, r.bottom - r.top);
        if sw <= 0 || sh <= 0 {
            return Ok(None);
        }
        let (w, h) = fit(sw as u32, sh as u32, ask.max_w, ask.max_h);
        let mut info = BITMAPINFO::default();
        info.bmiHeader = BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w as i32,
            // 음수 높이 = 위에서 아래로. 그래야 픽셀 순서가 화면 순서와 같다.
            biHeight: -(h as i32),
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        };
        unsafe {
            let screen = GetDC(None);
            let mut bits = std::ptr::null_mut();
            let Ok(bitmap) = CreateDIBSection(Some(screen), &info, DIB_RGB_COLORS, &mut bits, None, 0) else {
                ReleaseDC(None, screen);
                return Ok(None);
            };
            let dc = CreateCompatibleDC(Some(screen));
            let old = SelectObject(dc, bitmap.into());
            SetStretchBltMode(dc, HALFTONE);
            // 잠금 화면·UAC 같은 보안 데스크톱에서는 실패한다 — 그 동안은 지난 장을 보인다.
            let ok = StretchBlt(dc, 0, 0, w as i32, h as i32, Some(screen), r.left, r.top, sw, sh, SRCCOPY | CAPTUREBLT)
                .as_bool();
            let px = (ok && !bits.is_null())
                .then(|| std::slice::from_raw_parts(bits.cast::<u8>(), w as usize * h as usize * 4).to_vec());
            SelectObject(dc, old);
            let _ = DeleteDC(dc);
            let _ = DeleteObject(bitmap.into());
            ReleaseDC(None, screen);
            Ok(px.map(|px| (px, w, h)))
        }
    }
}

#[cfg(not(any(target_os = "macos", windows)))]
mod os {
    use super::{Ask, Denied, Display, Order};

    pub(super) const ORDER: Order = Order::Rgbx;

    pub(super) fn displays() -> Vec<Display> {
        Vec::new()
    }

    pub(super) fn request_permission() {}

    pub(super) fn grab(_: &Ask) -> Result<Option<(Vec<u8>, u32, u32)>, Denied> {
        Err(Denied::Unavailable)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_keeps_ratio_and_never_upscales() {
        assert_eq!(fit(5120, 2880, 1600, 1600), (1600, 900));
        assert_eq!(fit(1080, 1920, 1600, 900), (506, 900));
        assert_eq!(fit(800, 600, 1600, 1600), (800, 600));
    }

    #[test]
    fn fingerprint_tells_screens_apart_and_is_never_zero() {
        let a = vec![0u8; 64];
        let mut b = a.clone();
        b[40] = 1;
        assert_ne!(fingerprint(&a), fingerprint(&b));
        assert_eq!(fingerprint(&a), fingerprint(&a.clone()));
        assert_ne!(fingerprint(&[]), 0);
    }

    #[test]
    fn jpeg_reads_both_byte_orders_as_the_same_color() {
        let rgbx = [200u8, 30, 10, 0].repeat(16 * 16);
        let bgrx = [10u8, 30, 200, 0].repeat(16 * 16);
        for (px, order) in [(rgbx, Order::Rgbx), (bgrx, Order::Bgrx)] {
            let bytes = jpeg(&px, order, 16, 16, 80).unwrap();
            let decoded = image::load_from_memory(&bytes).unwrap().to_rgb8();
            let p = decoded.get_pixel(8, 8).0;
            assert!(p[0] > 150 && p[2] < 60, "{p:?}");
        }
    }
}

#[cfg(test)]
mod measure {
    use super::*;

    /// 실제 화면을 뜬다 — 화면 기록 권한이 있는 터미널에서
    /// `cargo test --release -p kasaterm --bin kasaterm screen_capture::measure -- --ignored --nocapture`.
    #[test]
    #[ignore = "실제 화면을 뜬다"]
    fn measure_main_display() {
        let screen = Screen;
        let main = screen.displays().iter().find(|d| d.main).map(|d| d.id).unwrap_or(0);
        for (edge, quality) in [(1280u32, 50u8), (1600, 60), (2560, 70)] {
            let ask = Ask { display: main, max_w: edge, max_h: edge, quality, prev: 0 };
            let started = std::time::Instant::now();
            let mut bytes = 0;
            for _ in 0..10 {
                match screen.capture(&ask) {
                    Ok(Some(shot)) => bytes = shot.jpeg.len(),
                    Ok(None) => {}
                    Err(why) => return eprintln!("denied {why:?}"),
                }
            }
            eprintln!("{edge} q{quality}: {:?}/장, jpeg {bytes}B", started.elapsed() / 10);
        }
    }
}
