//! 설정 화면 창구 — 캐릭터·테마 명부·디자인 토큰·온보딩·테마 생성·얼굴/스프라이트·설정 동작.

use super::*;
#[cfg(test)]
use super::test_support::temp_dir;

/// characters.json 후보 경로 — kasaterm-assign-character.py 와 같은 우선순위:
/// ~/.config/kasaterm/characters.json → 번들 collab-hooks (env 오버라이드 →
/// .app Resources → 레포 소스). 파싱 실패 파일은 건너뛰고 다음 후보로 (py 동일).
fn characters_candidate_paths() -> Vec<std::path::PathBuf> {
    let mut v = Vec::new();
    if let Some(home) = kasa_socket::home_dir() {
        v.push(home.join(".config/kasaterm/characters.json"));
    }
    if let Ok(p) = std::env::var("KASATERM_COLLAB_HOOKS_DIR") {
        v.push(std::path::PathBuf::from(p).join("characters.json"));
    }
    if let Ok(exe) = std::env::current_exe() {
        // <bundle>/Contents/MacOS/kasaterm → <bundle>/Contents/Resources/collab-hooks
        if let Some(res) = exe
            .parent()
            .and_then(|m| m.parent())
            .map(|c| c.join("Resources/collab-hooks/characters.json"))
        {
            v.push(res);
        }
    }
    // cargo run (dev): 이 crate 기준 레포 안 정본
    v.push(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../app/kasaterm/collab-hooks/characters.json"),
    );
    v
}

/// 후보들 중 첫 번째로 읽히고 JSON 으로 파싱되는 파일의 내용.
fn first_valid_json(paths: &[std::path::PathBuf]) -> Option<serde_json::Value> {
    for p in paths {
        let Ok(s) = std::fs::read_to_string(p) else { continue };
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&s) {
            return Some(v);
        }
    }
    None
}

/// `GET /characters` — 캐릭터 테마 정의를 그대로 JSON 으로 반환. 없으면 404
/// (테마 미설치 = 기능 전체 skip 이 규약이라, 프런트가 404 로 분기한다).
async fn characters_handler() -> impl IntoResponse {
    let (status, body) = match first_valid_json(&characters_candidate_paths()) {
        Some(v) => (axum::http::StatusCode::OK, v),
        None => (
            axum::http::StatusCode::NOT_FOUND,
            serde_json::json!({ "error": "characters.json not found" }),
        ),
    };
    (status, [(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")], Json(body))
}

/// `GET /theme-roster?id=<테마id|__base>` — 그 테마의 로스터를 `/characters` 와
/// 같은 형태로. `__base` 는 활성 테마를 뺀 기본(번들) 로스터다. 진행 중 pane 의
/// 캐릭터 피커가 활성 밖 테마의 학생까지 묶음으로 보여 주는 데 쓴다(2026-08-24
/// 지시: 어느 테마가 활성이어도 다른 테마 캐릭터로 바꿀 수 있어야 한다).
async fn theme_roster_handler(
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let id = params.get("id").map(String::as_str).unwrap_or_default();
    let body = if id == "__base" {
        crate::character::base_characters_json()
    } else {
        crate::character::theme_characters_json(id)
    };
    let (status, body) = match body {
        Some(v) => (axum::http::StatusCode::OK, v),
        None => (
            axum::http::StatusCode::NOT_FOUND,
            serde_json::json!({ "error": "theme roster not found" }),
        ),
    };
    (status, [(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")], Json(body))
}

/// `GET /design-tokens` — 지금 화면에 쓰이는 색 팔레트·실루엣. 설정 웹뷰가 이걸
/// `--kt-*` CSS 변수로 심어 네이티브와 같은 색·같은 모서리로 그린다.
///
/// 경로 이름이 `/theme` 이 아닌 이유: 이 레포에서 "theme" 은 **캐릭터 테마**(학생
/// 프사·말투)와 **색 팔레트** 두 뜻으로 쓰인다. 한 이름에 얹으면 다음 사람이 무엇을
/// 받는 창구인지 URL 만 보고 가릴 수 없다.
async fn design_tokens_handler(backend: Arc<dyn Backend>) -> impl IntoResponse {
    (
        [(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")],
        Json(backend.design_tokens()),
    )
}

/// `GET /settings/characters` — 설정 화면 캐릭터 탭의 데이터: 테마 카드 목록과
/// 활성 테마의 로스터 전원(이름·슬러그·학교·색·성격).
async fn settings_characters_handler(backend: Arc<dyn Backend>) -> impl IntoResponse {
    (
        [(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")],
        Json(backend.settings_characters()),
    )
}

/// `GET /settings/values` — 캐릭터 탭 밖의 설정 값 전부. 카테고리마다 하위 객체
/// 하나씩이라(`general` · `appearance` · `shell` · `claude` · `feedback`) 탭이 늘어도
/// 라우트가 늘지 않는다.
///
/// 값은 여기서 파일을 읽어 만들지 않는다 — 정본이 GUI 프로세스의 메모리라서다.
/// 파일에 애초에 저장되지 않는 값(UI 배율은 세션 한정)이 섞여 있어, 파일에서 읽으면
/// 그 칸만 늘 기본값을 보여 준다.
async fn settings_values_handler(backend: Arc<dyn Backend>) -> impl IntoResponse {
    (
        [(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")],
        Json(backend.settings_values()),
    )
}

/// First-install choices plus detected host state. No credentials cross this
/// route; status changes while the page is open, so responses are never cached.
async fn onboarding_state_handler(backend: Arc<dyn Backend>) -> impl IntoResponse {
    let body = backend.settings_action("onboarding-state", None, None).unwrap_or_else(|e| {
        serde_json::json!({ "completed": true, "error": e.to_string() })
    });
    (
        [
            (header::ACCESS_CONTROL_ALLOW_ORIGIN, "*"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        Json(body),
    )
}

/// `GET /settings/themegen/state` — 캐릭터 생성 화면이 2초마다 묻는 진행 상태.
///
/// 캐시를 안 준다. 이 라우트의 존재 이유가 「지금 몇 번째 프레임인가」라서, 1초만
/// 캐시돼도 화면이 멈춘 것처럼 보인다.
async fn themegen_state_handler(backend: Arc<dyn Backend>) -> impl IntoResponse {
    (
        [
            (header::ACCESS_CONTROL_ALLOW_ORIGIN, "*"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        Json(backend.themegen_state()),
    )
}

/// 참조 그림 한 장의 상한. 화면이 512px 로 줄여 보내는 게 정상 경로지만, 원본을
/// 그대로 던지는 경로(드래그 놓기)도 있어 여유를 둔다.
const THEMEGEN_REF_LIMIT: usize = 32 << 20;

/// `GET /settings/themegen/ref?slug=<slug>` — 참조 그림 원본.
///
/// 캐시를 안 준다 — 사용자가 그림을 갈아 끼우는 화면이라, 캐시되면 방금 올린 것
/// 대신 옛것이 보여 업로드가 실패했다고 읽는다(`/character-sprite` 와 같은 이유).
async fn themegen_ref_get_handler(
    backend: Arc<dyn Backend>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> axum::response::Response {
    use axum::response::IntoResponse as _;
    let slug = params.get("slug").map(String::as_str).unwrap_or_default();
    match backend.themegen_ref(slug) {
        Some(bytes) => (
            axum::http::StatusCode::OK,
            [
                (header::CONTENT_TYPE, "image/png"),
                (header::ACCESS_CONTROL_ALLOW_ORIGIN, "*"),
                (header::CACHE_CONTROL, "no-store"),
            ],
            bytes,
        )
            .into_response(),
        None => (
            axum::http::StatusCode::NOT_FOUND,
            [
                (header::CONTENT_TYPE, "text/plain"),
                (header::ACCESS_CONTROL_ALLOW_ORIGIN, "*"),
                (header::CACHE_CONTROL, "no-store"),
            ],
            "not found",
        )
            .into_response(),
    }
}

/// `POST /settings/themegen/ref?slug=<slug>` — 참조 그림을 놓는다. 본문은 이미지
/// 바이트 그대로(base64 로 부풀리지 않는다).
///
/// `slug` 없이 `name=<파일명>` 으로 오면 새 캐릭터다 — 응답의 `slug` 가 실제로
/// 정해진 이름이라, 화면은 그걸로 상세를 이어서 연다.
async fn themegen_ref_put_handler(
    backend: Arc<dyn Backend>,
    Query(params): Query<std::collections::HashMap<String, String>>,
    body: axum::body::Bytes,
) -> impl IntoResponse {
    let slug = params.get("slug").map(String::as_str).unwrap_or_default();
    let name = params.get("name").map(String::as_str).unwrap_or_default();
    let body = match backend.themegen_put_ref(slug, name, &body) {
        Ok(slug) => serde_json::json!({ "ok": true, "slug": slug }),
        Err(e) => serde_json::json!({ "ok": false, "error": e }),
    };
    (
        [
            (header::ACCESS_CONTROL_ALLOW_ORIGIN, "*"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        Json(body),
    )
}

/// `GET /character-face?slug=<slug>&theme=<id>` — 캐릭터 프사 PNG. `theme` 을 주면
/// 그 테마 폴더의 그림(카드 미리보기), 안 주면 활성 폴더 → 번들 순.
///
/// 캐시를 1분만 주는 이유: 프사는 사용자가 스프라이트 폴더에 파일을 넣어 바꿀 수
/// 있다. `immutable` 로 굳히면 그림을 갈아도 화면이 그대로여서 원인을 못 찾는다.
/// 1분이면 한 화면을 그리는 동안은 캐시되고 파일 교체는 곧 반영된다.
async fn character_face_handler(
    backend: Arc<dyn Backend>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> axum::response::Response {
    use axum::response::IntoResponse as _;
    let slug = params.get("slug").map(String::as_str).unwrap_or_default();
    match backend.character_face(slug, params.get("theme").map(String::as_str)) {
        Some(bytes) => (
            axum::http::StatusCode::OK,
            [
                (header::CONTENT_TYPE, "image/png"),
                (header::ACCESS_CONTROL_ALLOW_ORIGIN, "*"),
                (header::CACHE_CONTROL, "max-age=60"),
            ],
            bytes,
        )
            .into_response(),
        None => (
            axum::http::StatusCode::NOT_FOUND,
            [
                (header::CONTENT_TYPE, "text/plain"),
                (header::ACCESS_CONTROL_ALLOW_ORIGIN, "*"),
                (header::CACHE_CONTROL, "no-store"),
            ],
            "not found",
        )
            .into_response(),
    }
}

/// `GET /claude-mod/face?name=<학생>` — 칸 안 「학생 얼굴」 mod 가 답 머리에 그릴 학생. 이름·색·얼굴 파일
/// 경로를 JSON 으로 준다(바이트가 아니라 경로인 것은 mod 의 HTTP 가 글자만 받아서다). 캐릭터 외형이
/// 꺼져 있으면 `{}` — mod 는 아무것도 안 그린다. 다른 mod 경로처럼 이 기계 안에서만 연다.
async fn claude_mod_face_handler(
    backend: Arc<dyn Backend>,
    Query(params): Query<std::collections::HashMap<String, String>>,
    req: axum::extract::Request,
) -> axum::response::Response {
    use axum::response::IntoResponse as _;
    if is_remote_peer(&req) {
        return (axum::http::StatusCode::FORBIDDEN, "claude-mod routes are loopback only").into_response();
    }
    let name = params.get("name").map(String::as_str).unwrap_or_default();
    let body = (!name.is_empty()).then(|| backend.claude_mod_face(name)).flatten();
    Json(body.unwrap_or_else(|| serde_json::json!({}))).into_response()
}

/// 업로드 한 벌의 상한(base64 부풀림 포함). 프레임 6장 × 4MB 가 상한이므로 그
/// 4/3 에 여유를 얹었다 — axum 기본 2MB 로는 큰 원본 한 장에도 요청이 통째로
/// 거부되고, 그 거부는 화면에 이유 없이 실패로만 온다.
const SPRITE_UPLOAD_LIMIT: usize = 48 << 20;

/// `GET /character-sprite?slug=<slug>&motion=<m>&frame=<i>` — 모션 프레임 한 장.
/// 사용자 그림이 있으면 그것, 없으면 번들(화면이 지금 쓰는 것과 같은 순서).
///
/// `/character-face` 와 달리 **캐시를 안 준다**. 이 라우트는 그림을 갈아 끼우는
/// 화면 전용이라, 1분이라도 캐시되면 방금 올린 그림 대신 옛것이 보여 사용자는
/// 업로드가 실패했다고 읽는다.
async fn character_sprite_handler(
    backend: Arc<dyn Backend>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> axum::response::Response {
    use axum::response::IntoResponse as _;
    let slug = params.get("slug").map(String::as_str).unwrap_or_default();
    let motion = params.get("motion").map(String::as_str).unwrap_or_default();
    let frame = params.get("frame").and_then(|s| s.parse::<usize>().ok()).unwrap_or(0);
    let mime = if motion == "gif" { "image/gif" } else { "image/png" };
    match backend.character_sprite(slug, motion, frame) {
        Some(bytes) => (
            axum::http::StatusCode::OK,
            [
                (header::CONTENT_TYPE, mime),
                (header::ACCESS_CONTROL_ALLOW_ORIGIN, "*"),
                (header::CACHE_CONTROL, "no-store"),
            ],
            bytes,
        )
            .into_response(),
        None => (
            axum::http::StatusCode::NOT_FOUND,
            [
                (header::CONTENT_TYPE, "text/plain"),
                (header::ACCESS_CONTROL_ALLOW_ORIGIN, "*"),
                (header::CACHE_CONTROL, "no-store"),
            ],
            "not found",
        )
            .into_response(),
    }
}

/// `GET /character-sprite-status?slug=<slug>` — 모션별 프레임 수와 그림 출처.
/// 화면은 이걸로 업로드 칸 수를 정하고 "기본 그림/내가 넣은 것"을 가른다.
async fn character_sprite_status_handler(
    backend: Arc<dyn Backend>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let slug = params.get("slug").map(String::as_str).unwrap_or_default();
    (
        [
            (header::ACCESS_CONTROL_ALLOW_ORIGIN, "*"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        Json(backend.character_sprite_status(slug)),
    )
}

/// `POST /character-sprite` — 사용자 그림을 굳히거나 지운다.
/// body(JSON): `{"slug","motion","frames":["<base64>",…]}` 또는
/// `{"slug","motion","clear":true}`.
///
/// 프레임을 한 장씩 받지 않는 것은 로더가 **벌 단위 all-or-nothing** 이기
/// 때문이다. 반쯤 올라간 폴더는 오류 없이 기본 도트로 폴백하므로, 사용자에게는
/// 업로드가 통째로 무시된 것처럼 보인다.
///
/// `Content-Type` 을 보지 않는 이유는 `/settings/character` 와 같다 —
/// `text/plain` 으로 보내면 CORS simple request 라 preflight 가 아예 안 뜬다.
async fn character_sprite_save_handler(
    backend: Arc<dyn Backend>,
    body: String,
) -> impl IntoResponse {
    let cors = || [(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")];
    let bad = |msg: String| (cors(), Json(serde_json::json!({ "ok": false, "error": msg })));
    let v: serde_json::Value = match serde_json::from_str(&body) {
        Ok(v) => v,
        Err(e) => return bad(format!("bad body: {e}")),
    };
    let slug = v.get("slug").and_then(|x| x.as_str()).unwrap_or("").trim();
    let motion = v.get("motion").and_then(|x| x.as_str()).unwrap_or("").trim();
    if slug.is_empty() || motion.is_empty() {
        return bad("slug/motion required".to_string());
    }
    if v.get("clear").and_then(|x| x.as_bool()).unwrap_or(false) {
        return match backend.clear_character_sprite(slug, motion) {
            Ok(v) => (cors(), Json(v)),
            Err(e) => bad(e.to_string()),
        };
    }
    let Some(arr) = v.get("frames").and_then(|x| x.as_array()) else {
        return bad("frames required".to_string());
    };
    let frames: Vec<Vec<u8>> =
        arr.iter().map(|f| crate::proxy::b64_decode(f.as_str().unwrap_or(""))).collect();
    match backend.save_character_sprite(slug, motion, &frames) {
        Ok(v) => (cors(), Json(v)),
        Err(e) => bad(e.to_string()),
    }
}

/// `GET /settings/character-raw?name=<이름>&format=json|yaml` — 캐릭터 한 명의
/// 정의를 글로 편다(원본 뷰가 읽는 것).
///
/// 변환을 서버가 하는 이유는 화면마다 다른 파서를 쓰지 않게 하려는 것이다.
/// 저장도 같은 짝의 함수로 되돌리므로 왕복이 어긋날 자리가 없다 — 웹에서 YAML
/// 라이브러리를 따로 들이면 두 화면이 같은 글을 다르게 저장하게 된다.
///
/// GUI 왕복을 안 타는 것은 로스터가 파일이기 때문이다(메모리에 정본이 있는
/// 설정 값들과 다르다).
async fn settings_character_raw_handler(name: String, want_yaml: bool) -> impl IntoResponse {
    let cors = || [(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")];
    let Some(chars) = crate::character::characters_json() else {
        return (cors(), Json(serde_json::json!({ "ok": false, "error": "로스터를 못 읽었어요" })));
    };
    let Some(def) = crate::character::member_def(&chars, name.trim()) else {
        return (
            cors(),
            Json(serde_json::json!({ "ok": false, "error": format!("{name} 은(는) 로스터에 없어요") })),
        );
    };
    let text = if want_yaml {
        crate::character::member_to_yaml(&def)
    } else {
        serde_json::to_string_pretty(&def).unwrap_or_default()
    };
    (cors(), Json(serde_json::json!({ "ok": true, "text": text })))
}

/// `POST /settings/character` — 캐릭터 한 명의 성격·이름을 굳힌다.
/// body(JSON): `{"name": "아로나", "persona": "…", "new_name": "…", "model": "…",
/// "backend": "…", "raw": "…", "format": "json"|"yaml"}` — **준 것만** 바꾼다.
/// `raw` 는 정의 전체 교체(원본 뷰 저장)라 오면 낱개 필드보다 우선한다.
///
/// body 를 `String` 으로 받아 직접 파싱하는 건 관례를 따른 것이다(`/task-add` 등).
/// 덤으로 `Content-Type` 을 안 보므로 `text/plain` 으로 보낼 수 있고, 그러면 이
/// 요청이 CORS simple request 라 preflight(OPTIONS)가 아예 안 뜬다 — `post()` 만
/// 걸린 라우트는 OPTIONS 에 405 를 답하고 요청은 조용히 죽는다.
///
/// 이름은 로스터의 **키**라 되돌릴 수 없는 두 경우를 여기서 먼저 막는다: 빈 이름
/// (그 캐릭터가 로스터에서 사라진다)과 중복(로스터 빌드가 뒤엣것을 통째로 버려 한
/// 명이 증발한다). 문구는 네이티브(`settings.rs` 의 `flush_student_name`)와 같은
/// 것을 쓴다 — 같은 거부를 두 화면이 다르게 말하면 다른 문제로 읽힌다.
///
/// 저장 쪽도 같은 판정을 한 번 더 한다. 겹치는 게 낭비가 아닌 이유는 둘이 답하는
/// 게 다르기 때문이다 — 여기 것은 **이유를 웹에 돌려주고**(네이티브 토스트는
/// 웹뷰에서 안 보인다), 저쪽 것은 그 사이 파일이 바뀌었을 때 파일을 지킨다.
async fn settings_character_handler(
    backend: Arc<dyn Backend>,
    body: String,
) -> impl IntoResponse {
    let cors = || [(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")];
    let bad = |msg: String| (cors(), Json(serde_json::json!({ "ok": false, "error": msg })));
    let v: serde_json::Value = match serde_json::from_str(&body) {
        Ok(v) => v,
        Err(e) => return bad(format!("bad body: {e}")),
    };
    let name = v.get("name").and_then(|x| x.as_str()).unwrap_or("").trim();
    if name.is_empty() {
        return bad("name required".to_string());
    }
    // 판정에 쓰는 로스터를 웹이 보는 것과 같은 창구에서 받는다 — 따로 읽으면
    // 화면엔 있는 캐릭터가 여기선 없는 것으로 갈릴 수 있다.
    let chars = backend.settings_characters();
    let names: Vec<&str> = chars
        .get("roster")
        .and_then(|r| r.as_array())
        .map(|a| a.iter().filter_map(|e| e.get("name")?.as_str()).collect())
        .unwrap_or_default();
    if !names.contains(&name) {
        return bad(format!("{name} 은(는) 로스터에 없어요"));
    }
    let persona = v.get("persona").and_then(|x| x.as_str());
    let new_name = match v.get("new_name").and_then(|x| x.as_str()) {
        // 같은 이름을 보낸 건 안 바꾸겠다는 뜻이다 — 그대로 넘기면 저장 쪽이
        // 「이미 있는 이름」으로 읽어 자기 자신과 부딪힌다.
        Some(n) if n.trim() == name => None,
        Some(n) => {
            let n = n.trim();
            if n.is_empty() {
                return bad("이름은 비울 수 없어요".to_string());
            }
            if names.contains(&n) {
                return bad(format!("{n} 은(는) 이미 있어요"));
            }
            Some(n)
        }
        None => None,
    };
    let model = v.get("model").and_then(|x| x.as_str());
    let backend_name = v.get("backend").and_then(|x| x.as_str());
    // 정의 통째 교체(원본 뷰). map 이 아닌 것을 받으면 저장 쪽이 거부하지만,
    // 여기서 먼저 걸러야 웹이 이유를 바로 본다.
    let raw = v.get("raw").and_then(|x| x.as_str()).map(str::to_string);
    let raw_yaml = v.get("format").and_then(|x| x.as_str()) == Some("yaml");
    if persona.is_none()
        && new_name.is_none()
        && model.is_none()
        && backend_name.is_none()
        && raw.is_none()
    {
        return bad("바꿀 게 없어요".to_string());
    }
    let req = CharacterSave {
        name: name.to_string(),
        persona: persona.map(str::to_string),
        new_name: new_name.map(str::to_string),
        model: model.map(str::to_string),
        backend: backend_name.map(str::to_string),
        raw,
        raw_yaml,
    };
    match backend.save_character(req) {
        Ok(v) => (cors(), Json(v)),
        Err(e) => bad(e.to_string()),
    }
}

/// `POST /settings/action` — 설정 화면의 버튼 하나를 누른 것과 같은 일.
/// body(JSON): `{"action": "select-theme", "id": "my-theme", "label": "새 이름"}`.
///
/// 액션별로 라우트를 파지 않은 이유는 네이티브가 이미 액션 enum 하나로 모여
/// 있어서다 — 1:1 로 옮기면 구현이 둘로 갈릴 수가 없고, 네이티브에 버튼이 늘어도
/// 여기와 프록시 목록에 손댈 게 없다. 나중에 갈라야 하면 그때 가르는 건 싸다.
///
/// `Content-Type` 을 보지 않는 것도 `/settings/character` 와 같은 이유다 —
/// `text/plain` 으로 보내면 CORS simple request 라 preflight 가 아예 안 뜬다.
///
/// **스냅샷을 회신에 싣지 않는다.** 부른 쪽은 이 응답을 받은 뒤 `/settings/characters`
/// 를 다시 읽는다. GUI 가 세 캐시(테마 해석·로스터·GPU)를 비운 **뒤에** 이 응답이
/// 나가므로 그 다음 읽기는 새 상태가 보장되고, 스냅샷 만드는 코드가 두 벌이 되는
/// 것도 막는다.
async fn settings_action_handler(backend: Arc<dyn Backend>, body: String) -> impl IntoResponse {
    let cors = || [(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")];
    let bad = |msg: String| (cors(), Json(serde_json::json!({ "ok": false, "error": msg })));
    let v: serde_json::Value = match serde_json::from_str(&body) {
        Ok(v) => v,
        Err(e) => return bad(format!("bad body: {e}")),
    };
    let action = v.get("action").and_then(|x| x.as_str()).unwrap_or("").trim();
    if action.is_empty() {
        return bad("action required".to_string());
    }
    // 테마 폴더 이름은 경로 조각이 된다. 탈출은 저장 쪽(`safe_theme_id`)도 막지만
    // 여기서 먼저 걸러 **이유를 웹에 돌려준다** — 저쪽 거부는 토스트로만 말한다.
    let id = v.get("id").and_then(|x| x.as_str());
    if id.is_some_and(|s| s.contains('/') || s.contains("..")) {
        return bad("테마 이름에 쓸 수 없는 글자가 있어요".to_string());
    }
    let label = v.get("label").and_then(|x| x.as_str());
    match backend.settings_action(action, id, label) {
        Ok(v) => (cors(), Json(v)),
        Err(e) => bad(e.to_string()),
    }
}

/// 이 모듈 창구의 라우트. `router` 가 한 표로 합친 뒤 공통 레이어(Origin·토큰 가드)를 두른다.
pub(super) fn routes(backend: &Arc<dyn Backend>) -> axum::Router {
    let design_tokens_backend = backend.clone();
    let settings_chars_backend = backend.clone();
    let settings_values_backend = backend.clone();
    let onboarding_state_backend = backend.clone();
    let settings_char_save_backend = backend.clone();
    let settings_action_backend = backend.clone();
    let character_face_backend = backend.clone();
    let mod_face_backend = backend.clone();
    let sprite_get_backend = backend.clone();
    let sprite_status_backend = backend.clone();
    let sprite_save_backend = backend.clone();
    let themegen_state_backend = backend.clone();
    let themegen_ref_get_backend = backend.clone();
    let themegen_ref_put_backend = backend.clone();
    axum::Router::new()
        .route("/characters", get(characters_handler))
        .route("/theme-roster", get(theme_roster_handler))
        .route(
            "/settings/character-raw",
            get(|q: axum::extract::Query<std::collections::HashMap<String, String>>| {
                let name = q.get("name").cloned().unwrap_or_default();
                let yaml = q.get("format").map(String::as_str) == Some("yaml");
                settings_character_raw_handler(name, yaml)
            }),
        )
        .route(
            "/settings/character",
            post(move |body: String| {
                settings_character_handler(settings_char_save_backend.clone(), body)
            }),
        )
        .route(
            "/settings/action",
            post(move |body: String| {
                settings_action_handler(settings_action_backend.clone(), body)
            }),
        )
        .route(
            "/design-tokens",
            get(move || design_tokens_handler(design_tokens_backend.clone())),
        )
        .route(
            "/settings/characters",
            get(move || settings_characters_handler(settings_chars_backend.clone())),
        )
        .route(
            "/settings/values",
            get(move || settings_values_handler(settings_values_backend.clone())),
        )
        .route(
            "/onboarding/state",
            get(move || onboarding_state_handler(onboarding_state_backend.clone())),
        )
        .route(
            "/settings/themegen/state",
            get(move || themegen_state_handler(themegen_state_backend.clone())),
        )
        .route(
            "/settings/themegen/ref",
            get(move |q: Query<std::collections::HashMap<String, String>>| {
                themegen_ref_get_handler(themegen_ref_get_backend.clone(), q)
            })
            .post(
                move |q: Query<std::collections::HashMap<String, String>>,
                      body: axum::body::Bytes| {
                    themegen_ref_put_handler(themegen_ref_put_backend.clone(), q, body)
                },
            )
            // 원본을 그대로 던지는 경로가 있어 axum 기본 2MB 로는 모자라다.
            .layer(axum::extract::DefaultBodyLimit::max(THEMEGEN_REF_LIMIT)),
        )
        .route(
            "/character-face",
            get(move |q: Query<std::collections::HashMap<String, String>>| {
                character_face_handler(character_face_backend.clone(), q)
            }),
        )
        .route(
            "/claude-mod/face",
            get(move |q: Query<std::collections::HashMap<String, String>>, req: axum::extract::Request| {
                claude_mod_face_handler(mod_face_backend.clone(), q, req)
            }),
        )
        .route(
            "/character-sprite",
            get(move |q: Query<std::collections::HashMap<String, String>>| {
                character_sprite_handler(sprite_get_backend.clone(), q)
            })
            .post(move |body: String| {
                character_sprite_save_handler(sprite_save_backend.clone(), body)
            })
            // 업로드 한 벌은 axum 기본 2MB 를 넘길 수 있다 — 그 거부는
            // 화면에 이유 없는 실패로만 와서 원인을 못 찾는다.
            .layer(axum::extract::DefaultBodyLimit::max(SPRITE_UPLOAD_LIMIT)),
        )
        .route(
            "/character-sprite-status",
            get(move |q: Query<std::collections::HashMap<String, String>>| {
                character_sprite_status_handler(sprite_status_backend.clone(), q)
            }),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_valid_json_skips_broken_files() {
        let d = temp_dir("char-json");
        let broken = d.join("broken.json");
        let valid = d.join("valid.json");
        let missing = d.join("missing.json");
        std::fs::write(&broken, "{not json").unwrap();
        std::fs::write(&valid, r#"{"leader":{"name":"아로나"}}"#).unwrap();
        // 깨진 파일·없는 파일은 건너뛰고 첫 유효 JSON 을 집는다
        let got = first_valid_json(&[missing.clone(), broken.clone(), valid.clone()]).unwrap();
        assert_eq!(got["leader"]["name"], "아로나");
        // 전부 무효 → None (핸들러는 404)
        assert!(first_valid_json(&[missing, broken]).is_none());
        let _ = std::fs::remove_dir_all(&d);
    }
}
