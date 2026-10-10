//! 웹 터미널 페이지와 정적 자산 — xterm.js·그리드·크롬·아이콘·그림 자산·학생 얼굴.
//!
//! xterm.js 는 vendored 다(assets/term, MIT). CDN 을 쓰면 오프라인에서 죽고
//! 사내망 정책에도 걸린다 — 바이너리에 박아 넣으면 서버 하나로 자족한다.

use super::*;
use super::auth::html_with_token_cookie;

async fn term_page_handler(
    Query(q): Query<std::collections::HashMap<String, String>>,
) -> axum::response::Response {
    html_with_token_cookie(include_str!("../../assets/term/index.html"), &q)
}

/// 터미널 폰트. claude code 의 Nerd Font 아이콘(사설영역)과 박스드로잉이 폰
/// 시스템 폰트에는 없어서, 안 내려주면 두부(□)와 끊긴 선으로 보인다.
///
/// 번들 CascadiaCodeNF 를 실제로 쓰는 범위만 남겨 서브셋했다(2.4MB → 356KB).
/// 한글은 일부러 뺐다 — 이 폰트에 애초에 없고, 넣으면 몇 MB가 된다. 폰에는 한글
/// 폰트가 이미 있으므로 폴백에 맡긴다.
async fn term_asset_font() -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, "font/woff2"),
            // 내용이 바뀌지 않으므로 길게 캐시한다 — 폰이 열 때마다 356KB 를
            // 다시 받으면 터널 너머에서 특히 아프다.
            (header::CACHE_CONTROL, "public, max-age=31536000, immutable"),
        ],
        include_bytes!("../../assets/term/font.woff2").as_slice(),
    )
}

async fn term_asset_js() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "application/javascript; charset=utf-8")],
        include_str!("../../assets/term/xterm.js"),
    )
}

/// 셀 그리드 렌더러. xterm.js 와 달리 VT 파서가 없다 — 서버가 파싱한 그리드를
/// 그대로 그린다(`gridwire.rs`).
async fn term_grid_js() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "application/javascript; charset=utf-8")],
        include_str!("../../assets/term/grid.js"),
    )
}

async fn term_viewport_js() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "application/javascript; charset=utf-8")],
        include_str!("../../assets/term/viewport.js"),
    )
}

async fn term_chrome_js() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "application/javascript; charset=utf-8")],
        include_str!("../../assets/term/chrome.js"),
    )
}

async fn term_chrome_css() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
        include_str!("../../assets/term/chrome.css"),
    )
}

async fn term_icon(AxPath(name): AxPath<String>) -> axum::response::Response {
    let icon = match name.as_str() {
        "claude.svg" => include_str!("../../../../app/kasaterm/assets/icons/claude.svg"),
        "codex.svg" => include_str!("../../../../app/kasaterm/assets/icons/codex.svg"),
        "terminal.svg" => include_str!("../../../../app/kasaterm/assets/icons/terminal.svg"),
        "server.svg" => include_str!("../../../../app/kasaterm/assets/icons/server.svg"),
        "laptop.svg" => include_str!("../../../../app/kasaterm/assets/icons/laptop.svg"),
        _ => return axum::http::StatusCode::NOT_FOUND.into_response(),
    };
    ([(header::CONTENT_TYPE, "image/svg+xml; charset=utf-8")], icon).into_response()
}

pub(super) async fn term_visual_builtin(AxPath(name): AxPath<String>) -> axum::response::Response {
    match crate::visual::builtin_asset(&name) {
        Some((bytes, mime)) => ([(header::CONTENT_TYPE, mime)], bytes).into_response(),
        None => axum::http::StatusCode::NOT_FOUND.into_response(),
    }
}

pub(super) async fn term_visual_asset(
    Query(query): Query<std::collections::HashMap<String, String>>,
) -> axum::response::Response {
    let missing = || (
        axum::http::StatusCode::NOT_FOUND,
        [(header::CACHE_CONTROL, "no-store")],
    ).into_response();
    let Some(pane) = query.get("pane") else {
        return missing();
    };
    let Some(id) = query.get("id") else {
        return missing();
    };
    if kasa_pty::lookup_session(pane).is_none() {
        return missing();
    }
    match crate::visual::inline_asset(pane, id) {
        Some((bytes, mime)) => (
            [(header::CONTENT_TYPE, mime), (header::CACHE_CONTROL, "private, max-age=31536000, immutable")],
            bytes.to_vec(),
        ).into_response(),
        None => missing(),
    }
}

async fn term_grid_css() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
        include_str!("../../assets/term/grid.css"),
    )
}

async fn term_grid_page(
    Query(q): Query<std::collections::HashMap<String, String>>,
) -> axum::response::Response {
    html_with_token_cookie(include_str!("../../assets/term/grid.html"), &q)
}

async fn term_asset_css() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
        include_str!("../../assets/term/xterm.css"),
    )
}

/// 프사가 있는 학생 슬러그. `term_avatar` 의 목록과 짝이므로 여기 없는 이름은
/// 프사도 없다 — 둘 다 `assets/students/profile/*.png` 전체와 맞춘다. 12명만
/// 있던 시절엔 로스터의 세나·이치카·하루나가 전부 탈락해 폰 목록이 무프사였다.
const AVATAR_SLUGS: &[&str] = &[
    "akane", "akari", "ako", "arisu", "arona", "aru", "asuna", "atsuko", "ayane", "azusa",
    "chihiro", "chinatsu", "eimi", "fubuki", "fuuka", "hanako", "hare", "haruka", "haruna",
    "hasumi", "hibiki", "hifumi", "himari", "hina", "hinata", "hiyori", "hoshino", "ichika",
    "iori", "iroha", "izuna", "kaho", "kanna", "karin", "kasumi", "kayoko", "kazusa", "kei",
    "kirino", "koharu", "konoka", "kotama", "kotori", "koyuki", "maki", "makoto", "mari",
    "mashiro", "michiru", "midori", "mika", "misaki", "momoi", "mutsuki", "nagisa", "neru",
    "niya", "noa", "nonomi", "prana", "rio", "sakurako", "saori", "satsuki", "seia", "sena",
    "serika", "shiroko", "shizuko", "sumire", "toki", "tsubaki", "tsukuyo", "tsurugi", "utaha",
    "wakamo", "yukari", "yuuka", "yuzu",
];

/// agent 이름(`aru-p151-1uc`)의 앞 토막이 캐릭터 슬러그다.
///
/// ⚠️ 이름→슬러그 표(`theme::character_slug`)를 여기 복제하지 않는다. 그건 인박스
/// 파일명을 정하는 정본이라 두 벌이 되면 어긋나고, 어긋나도 오류가 안 난다. 우리는
/// 이미 만들어진 결과를 되읽기만 한다 — 표에 없는 커스텀 캐릭터는 해시 슬러그로
/// 떨어져 여기서 `None` 이 되고, 프사 없이 이름만 뜬다.
pub(super) fn avatar_slug(agent_name: &str) -> Option<String> {
    let head = agent_name.split('-').next()?;
    // 번들 슬러그 목록은 **번들 프사가 있느냐**의 답일 뿐이다. 테마 학생의 슬러그는
    // 거기 없어 전부 떨어졌고(에무·하치와레·진천우 → `null`), 그 목록을 읽는 쪽에서
    // 이사 간 학생만 얼굴이 비었다. 아는 명부 전부에 물어본다 — 그림 실재는
    // `term_avatar` 가 따로 판정한다.
    crate::character::known_slug(head)
        .or_else(|| AVATAR_SLUGS.iter().find(|s| **s == head).map(|s| s.to_string()))
}

/// `GET /term/avatar/<slug>.png` — pane 칩에 띄울 학생 프사.
///
/// kasaterm 은 pane 헤더에 프사를 그리는데 미러는 PTY 바이트만 받으므로 그게 없다.
/// 폰에서 「누구 화면인가」가 이름 한 줄로만 남으면 눈에 안 들어와서, 같은 그림을
/// 웹에도 준다. 자산은 GUI 가 쓰는 것 그대로다(따로 복제하지 않는다).
async fn term_avatar(axum::extract::Path(slug): axum::extract::Path<String>) -> impl IntoResponse {
    macro_rules! avatars {
        ($($s:literal),* $(,)?) => {
            match slug.trim_end_matches(".png") {
                $($s => Some(
                    include_bytes!(concat!(
                        "../../../../app/kasaterm/assets/students/profile/", $s, ".png"
                    )).as_slice()
                ),)*
                _ => None,
            }
        };
    }
    // 디스크가 먼저다 — 테마 학생의 얼굴은 번들에 없고, 사용자가 덮어쓴 그림도
    // 여기 있다(GUI 의 찾기 순서와 같다).
    let disk = crate::character::profile_png_on_disk(slug.trim_end_matches(".png"));
    let bundled = avatars!(
        "akane", "akari", "ako", "arisu", "arona", "aru", "asuna", "atsuko", "ayane", "azusa",
        "chihiro", "chinatsu", "eimi", "fubuki", "fuuka", "hanako", "hare", "haruka", "haruna",
        "hasumi", "hibiki", "hifumi", "himari", "hina", "hinata", "hiyori", "hoshino", "ichika",
        "iori", "iroha", "izuna", "kaho", "kanna", "karin", "kasumi", "kayoko", "kazusa", "kei",
        "kirino", "koharu", "konoka", "kotama", "kotori", "koyuki", "maki", "makoto", "mari",
        "mashiro", "michiru", "midori", "mika", "misaki", "momoi", "mutsuki", "nagisa", "neru",
        "niya", "noa", "nonomi", "prana", "rio", "sakurako", "saori", "satsuki", "seia", "sena",
        "serika", "shiroko", "shizuko", "sumire", "toki", "tsubaki", "tsukuyo", "tsurugi",
        "utaha", "wakamo", "yukari", "yuuka", "yuzu",
    );
    // 번들 그림은 판이 바뀌기 전엔 안 변하니 영구 캐시. 디스크 그림은 사람이
    // 갈아 끼울 수 있고 **같은 슬러그가 다른 테마를 가리키게 되기도 한다** — 그걸
    // immutable 로 굳히면 브라우저가 옛 얼굴을 계속 쥔다.
    let cache = if disk.is_some() { "public, max-age=60" } else { "public, max-age=31536000, immutable" };
    match disk.or_else(|| bundled.map(<[u8]>::to_vec)) {
        Some(b) => (
            axum::http::StatusCode::OK,
            [(header::CONTENT_TYPE, "image/png"), (header::CACHE_CONTROL, cache)],
            b,
        ),
        None => (
            axum::http::StatusCode::NOT_FOUND,
            [
                (header::CONTENT_TYPE, "text/plain"),
                (header::CACHE_CONTROL, "no-store"),
            ],
            Vec::new(),
        ),
    }
}

/// 이 모듈 창구의 라우트. `router` 가 한 표로 합친 뒤 공통 레이어(Origin·토큰 가드)를 두른다.
pub(super) fn routes() -> axum::Router {
    axum::Router::new()
        // 웹 터미널 — 브라우저에서 독립으로 여는 셸.
        .route("/term", get(term_page_handler))
        .route("/term/xterm.js", get(term_asset_js))
        .route("/term/xterm.css", get(term_asset_css))
        .route("/term/grid", get(term_grid_page))
        .route("/term/grid.js", get(term_grid_js))
        .route("/term/viewport.js", get(term_viewport_js))
        .route("/term/chrome.js", get(term_chrome_js))
        .route("/term/chrome.css", get(term_chrome_css))
        .route("/term/icon/{name}", get(term_icon))
        .route("/term/visual-builtin/{name}", get(term_visual_builtin))
        .route("/term/visual-asset", get(term_visual_asset))
        .route("/term/grid.css", get(term_grid_css))
        .route("/term/font.woff2", get(term_asset_font))
        .route("/term/avatar/{slug}", get(term_avatar))
}
