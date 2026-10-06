//! 격자 글꼴 찾기 — 주 고정폭 글꼴과 굵은·기울임 짝, 시스템 대체 글꼴 사슬.

use kasa_cells::Shaper;

#[cfg(target_os = "macos")]
fn home_var() -> Result<String, std::env::VarError> {
    match std::env::var("HOME") {
        Ok(h) if !h.is_empty() => Ok(h),
        _ => std::env::var("USERPROFILE"),
    }
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
pub fn windows_font_candidates(names: &[&str]) -> Vec<String> {
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

pub fn sibling_bold_font_path(regular: &str) -> Option<(String, u32)> {
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
pub fn attach_fallback_chain(shaper: &mut Shaper, bundled: &[&'static [u8]]) {
    for (path, idx) in fallback_font_paths() {
        let bold = sibling_bold_font_path(&path);
        shaper.add_fallback_with_bold(&path, idx, bold);
    }
    for bytes in bundled {
        shaper.add_fallback_bytes(bytes, 0);
    }
}

pub fn fallback_font_paths() -> Vec<(String, u32)> {
    let mut out: Vec<(String, u32)> = Vec::new();
    #[cfg(target_os = "macos")]
    {
        let home = home_var().unwrap_or_default();
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

/// Real italic variant of the primary mono face. JetBrains Mono ships
/// `JetBrainsMonoNerdFontMono-Italic.ttf` — D2Coding has none, so when
/// D2Coding is the primary we fall through to None (skew synthesis).
pub fn primary_italic_font_path() -> Option<(String, u32)> {
    if let Ok(p) = std::env::var("KASATERM_GRID_FONT_ITALIC") {
        if !p.is_empty() && std::path::Path::new(&p).exists() {
            return Some((p, 0));
        }
    }
    #[cfg(target_os = "macos")]
    {
        let home = home_var().unwrap_or_default();
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
/// "볼드가 약한" 증상이 난다(사용자 2026-07-26 실측: 한글 세션명 1.22x → 1.33x).
pub fn primary_bold_font_path(primary: &str) -> Option<(String, u32)> {
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
        let home = home_var().unwrap_or_default();
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

pub fn default_font_path() -> String {
    #[cfg(target_os = "macos")]
    {
        // JetBrains Mono for Latin; Hangul falls through to D2Coding 논-Mono
        // in the fallback chain (사용자 요청 2026-07-27).
        //
        // 예전에 JetBrains-as-primary 를 시도했다 되돌린 적이 있는데, 그때 자간이
        // 벌어진 원인은 JetBrains 자체가 아니라 **한글을 받던 폴백이 D2Coding
        // Mono** 였다는 데 있다. Mono 패치는 한글까지 0.5em 으로 압축하는데 칸은
        // 라틴 0.6em × 2 = 1.2em 이라 글리프가 칸의 절반도 못 채웠다. 지금은
        // 논-Mono(한글 1.0em)가 받고 shaper 가 두 칸에 맞춰 키운다.
        //
        // 선·모서리(Box Drawing)는 렌더러가 직접 긋는다(cells::box_line_rects).
        // 한 번 폰트에 맡겨 봤지만(2026-08-15 오전) 글리프가 advance 폭까지만
        // 그려서 칸이 그보다 넓으면 이웃과 틈이 남아 표 가로줄이 점선이 됐다 —
        // 폰트 선택은 이제 표·테두리 이음새에 영향이 없다.
        let home = home_var().unwrap_or_default();
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
