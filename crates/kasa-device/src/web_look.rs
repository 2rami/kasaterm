//! 앱 밖 웹 화면(폰 설치 페이지 · `/app` 안내)이 같이 쓰는 겉모습 — 앱 설정과 같은 클린 블루.
//! 관문의 설치 페이지와 이 기계의 `/app` 이 따로 놀아 둘이 다른 색으로 보였다.

pub const CSS: &str = include_str!("../assets/web/os.css");

/// 머리 막대 — 우산 표식 · 이름 · 이 화면이 하는 일.
pub fn bar(tag: &str) -> String {
    format!(
        "<header class=os-bar><span class=os-mark>{UMBRELLA}</span><span class=os-name>KASATERM</span><span class=os-tag>{}</span></header>",
        escape(tag)
    )
}

/// 하단바 QR 과 같은 우산(Lucide, ISC) — 웹은 이미지를 못 실어(설치 페이지 CSP) 선으로 그린다.
const UMBRELLA: &str = "<svg viewBox=\"0 0 24 24\" fill=none stroke=currentColor stroke-width=2.2 stroke-linecap=round stroke-linejoin=round aria-hidden=true>\
<path d=\"M12 13v7a2 2 0 0 0 4 0\"/><path d=\"M12 2v2\"/><path d=\"M20.992 13a1 1 0 0 0 .97-1.274 10.284 10.284 0 0 0-19.923 0A1 1 0 0 0 3 13z\"/></svg>";

/// 폰 카메라로 찍을 QR — 앱 하단바 QR 과 같은 모양(둥근 점 · 각진 찾기 무늬 · 가운데 우산 판).
/// 오류 정정은 High 라 가운데를 가려도 읽힌다. 바탕은 다크 모드에서도 흰색이어야 카메라가 읽는다.
/// 찾기 무늬는 각지고 속까지 남색이어야 한다 — 강조색(#2F6FD4)은 흑백으로 바꾸면 중간 회색이고, 모서리를 1.7칸만
/// 둥글려도 OpenCV 판독기가 무늬를 못 찾았다(2026-11-18, 크기 4단계 비교에서 각진 무늬만 모두 읽힘).
pub fn qr_svg(text: &str) -> Option<String> {
    let qr = qrcodegen::QrCode::encode_text(text, qrcodegen::QrCodeEcc::High).ok()?;
    let n = qr.size();
    let quiet = 3;
    let side = n + quiet * 2;
    let cover = qr_cover(n as usize) as i32;
    let lo = (n - cover) / 2;
    let in_cover = |x: i32, y: i32| (lo..lo + cover).contains(&x) && (lo..lo + cover).contains(&y);
    let finder = |x: i32, y: i32| (x < 7 && y < 7) || (x >= n - 7 && y < 7) || (x < 7 && y >= n - 7);
    let mut dots = String::new();
    for y in 0..n {
        for x in 0..n {
            if qr.get_module(x, y) && !finder(x, y) && !in_cover(x, y) {
                dots.push_str(&format!("<rect x=\"{}.08\" y=\"{}.08\" width=\".84\" height=\".84\" rx=\".3\"/>", x + quiet, y + quiet));
            }
        }
    }
    let mut eyes = String::new();
    for (fx, fy) in [(0, 0), (n - 7, 0), (0, n - 7)] {
        let (x, y) = (fx + quiet, fy + quiet);
        eyes.push_str(&format!(
            "<rect x=\"{}.5\" y=\"{}.5\" width=\"6\" height=\"6\" fill=none stroke=\"#15294A\" stroke-width=\"1\"/>\
<rect x=\"{}\" y=\"{}\" width=\"3\" height=\"3\" fill=\"#15294A\"/>",
            x, y, x + 2, y + 2
        ));
    }
    let c = f64::from(lo + quiet);
    let k = f64::from(cover);
    let icon = k * 0.72;
    Some(format!(
        "<svg viewBox=\"0 0 {side} {side}\" role=img aria-label=\"설치 링크 QR\" xmlns=\"http://www.w3.org/2000/svg\">\
<rect width=\"{side}\" height=\"{side}\" fill=\"#FFFFFF\"/><g fill=\"#15294A\">{dots}</g>{eyes}\
<rect x=\"{c}\" y=\"{c}\" width=\"{k}\" height=\"{k}\" rx=\"{r}\" fill=\"#2F6FD4\"/>\
<g transform=\"translate({ix} {ix}) scale({s})\" fill=none stroke=\"#FFFFFF\" stroke-width=\"2.2\" stroke-linecap=round stroke-linejoin=round>\
<path d=\"M12 13v7a2 2 0 0 0 4 0\"/><path d=\"M12 2v2\"/><path d=\"M20.992 13a1 1 0 0 0 .97-1.274 10.284 10.284 0 0 0-19.923 0A1 1 0 0 0 3 13z\"/></g></svg>",
        r = k * 0.28,
        ix = c + (k - icon) / 2.0,
        s = icon / 24.0,
    ))
}

/// 가운데를 덮는 칸 수 — 홀수라야 한가운데가 맞는다. 면적 15% 언저리로, High(30%) 안에서 넉넉하다.
/// 앱 하단바 QR(`statusbar.rs`)도 같은 값을 쓴다.
pub fn qr_cover(n: usize) -> usize {
    ((n as f32 * 0.22).floor() as usize | 1).clamp(3, 11)
}

pub fn escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&#39;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qr_keeps_finder_patterns_and_covers_only_the_middle() {
        let svg = qr_svg("https://example.com/relay/install/abc/").unwrap();
        assert_eq!(svg.matches("fill=\"#2F6FD4\"").count(), 1, "강조색은 가운데 판에만 — 찾기 무늬는 남색");
        assert!(svg.starts_with("<svg") && svg.ends_with("</svg>"));
        for n in [21, 25, 41, 57, 177] {
            let k = qr_cover(n);
            assert!(k % 2 == 1 && (k * k) as f32 / ((n * n) as f32) < 0.08, "가린 면적은 High 정정 안: n={n} k={k}");
        }
    }
}
