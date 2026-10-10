//! 아로나 UI(웹 교실) 정적 서빙과 그 곁 — 슬래시 명령 목록·모드·SCHALE 상태.

use super::*;
use super::auth::remote_token_cookie;

/// SKILL.md / commands/*.md frontmatter("--- … ---" 사이)의 description 추출.
fn frontmatter_desc(path: &std::path::Path) -> Option<String> {
    let s = std::fs::read_to_string(path).ok()?;
    let mut in_fm = false;
    for line in s.lines() {
        let t = line.trim();
        if t == "---" {
            if in_fm {
                break;
            }
            in_fm = true;
            continue;
        }
        if in_fm {
            if let Some(rest) = t.strip_prefix("description:") {
                return Some(rest.trim().trim_matches('"').trim_matches('\'').to_string());
            }
        }
    }
    None
}

fn add_cmd(
    cmds: &mut Vec<serde_json::Value>,
    seen: &mut std::collections::HashSet<String>,
    cmd: String,
    desc: String,
) {
    if seen.insert(cmd.clone()) {
        cmds.push(serde_json::json!({ "cmd": cmd, "desc": desc }));
    }
}

/// `GET /slash-commands` — claude 가 `/` 자동완성에 보여주는 동적 명령(스킬·커스텀·플러그인)을
/// 디스크 스캔(사용자: 스킬 이런 거 다). ~/.claude/skills·commands·plugins + 프로젝트 .claude/skills.
/// MCP 프롬프트는 서버 런타임이라 파일 스캔 불가 — 프런트 정적 목록이 내장 명령을 커버한다.
async fn slash_commands_handler(backend: Arc<dyn Backend>) -> impl IntoResponse {
    let cors = [(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")];
    let mut cmds: Vec<serde_json::Value> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    if let Some(home) = kasa_socket::home_dir() {
        let home = home.as_path();
        // ~/.claude/skills/<name>/SKILL.md → /<name>
        if let Ok(rd) = std::fs::read_dir(home.join(".claude/skills")) {
            for e in rd.flatten() {
                let md = e.path().join("SKILL.md");
                if md.exists() {
                    let name = e.file_name().to_string_lossy().to_string();
                    add_cmd(&mut cmds, &mut seen, format!("/{name}"), frontmatter_desc(&md).unwrap_or_default());
                }
            }
        }
        // ~/.claude/commands/<name>.md → /<name>
        if let Ok(rd) = std::fs::read_dir(home.join(".claude/commands")) {
            for e in rd.flatten() {
                let p = e.path();
                if p.extension().and_then(|x| x.to_str()) == Some("md") {
                    let name = p.file_stem().unwrap_or_default().to_string_lossy().to_string();
                    add_cmd(&mut cmds, &mut seen, format!("/{name}"), frontmatter_desc(&p).unwrap_or_default());
                }
            }
        }
        // 플러그인 ~/.claude/plugins/cache/<mk>/<plugin>/<ver>/skills/<name>/SKILL.md → /<plugin>:<name>
        if let Ok(mks) = std::fs::read_dir(home.join(".claude/plugins/cache")) {
            for mk in mks.flatten() {
                let Ok(plugins) = std::fs::read_dir(mk.path()) else { continue };
                for plugin in plugins.flatten() {
                    let pname = plugin.file_name().to_string_lossy().to_string();
                    let Ok(vers) = std::fs::read_dir(plugin.path()) else { continue };
                    for v in vers.flatten() {
                        if let Ok(rd) = std::fs::read_dir(v.path().join("skills")) {
                            for e in rd.flatten() {
                                let md = e.path().join("SKILL.md");
                                if md.exists() {
                                    let name = e.file_name().to_string_lossy().to_string();
                                    add_cmd(&mut cmds, &mut seen, format!("/{pname}:{name}"), frontmatter_desc(&md).unwrap_or_default());
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    // 프로젝트 .claude/skills/<name>/SKILL.md (활성 방 cwd)
    if let Ok(rd) = std::fs::read_dir(resolve_cwd(&backend).join(".claude/skills")) {
        for e in rd.flatten() {
            let md = e.path().join("SKILL.md");
            if md.exists() {
                let name = e.file_name().to_string_lossy().to_string();
                add_cmd(&mut cmds, &mut seen, format!("/{name}"), frontmatter_desc(&md).unwrap_or_default());
            }
        }
    }
    (cors, Json(serde_json::json!({ "commands": cmds }))).into_response()
}

/// cwd → 모드 마커 파일명 slug. kasacollab.py `mode_path` 와 동일 규칙
/// ('/' 와 '.' 을 '-' 로): 두 구현이 같은 마커를 읽고 써야 한다.
pub(super) fn mode_slug(cwd: &std::path::Path) -> String {
    cwd.to_string_lossy()
        .chars()
        .map(|c| if c == '/' || c == '.' { '-' } else { c })
        .collect()
}

/// `GET /mode` — 활성 pane 의 `{ cwd }`. 옛 solo 모드 필드(mode·configured)는
/// 제거됐다(shim_inject 가 대체). 라우트 자체는 resolveBase 헬스 프로브 + cwd 소스
/// (터미널 cd 반영)로 살아있어 경로명은 유지한다.
async fn mode_get_handler(backend: Arc<dyn Backend>) -> impl IntoResponse {
    let cwd = resolve_cwd(&backend);
    (
        [(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")],
        Json(serde_json::json!({
            "cwd": cwd.to_string_lossy(),
        })),
    )
}

/// arona-ui 정적 번들 루트: env 오버라이드 → .app Resources → 레포 dev 빌드.
/// (characters_candidate_paths 와 같은 3단 resolve 철학.)
fn arona_ui_root() -> Option<std::path::PathBuf> {
    if let Ok(p) = std::env::var("KASATERM_ARONA_UI_DIR") {
        let p = std::path::PathBuf::from(p);
        if p.is_dir() {
            return Some(p);
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        // <bundle>/Contents/MacOS/kasaterm → <bundle>/Contents/Resources/arona-ui
        if let Some(res) = exe
            .parent()
            .and_then(|m| m.parent())
            .map(|c| c.join("Resources/arona-ui"))
        {
            if res.is_dir() {
                return Some(res);
            }
        }
        // Windows MSI 는 bundle Resources 가 없다 — exe 옆 bin\arona-ui\ 에 번들.
        if let Some(adj) = exe.parent().map(|d| d.join("arona-ui")) {
            if adj.is_dir() {
                return Some(adj);
            }
        }
    }
    let dev = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../web/arona-ui/dist");
    if dev.is_dir() {
        return Some(dev);
    }
    None
}

/// 확장자 → Content-Type. vite dist 가 내는 파일 종류만 커버하면 충분.
fn static_content_type(path: &std::path::Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "text/javascript",
        Some("css") => "text/css",
        Some("json") | Some("map") => "application/json",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("ico") => "image/x-icon",
        Some("woff2") => "font/woff2",
        _ => "application/octet-stream",
    }
}

/// `GET /arona-ui/` + `GET /arona-ui/{*path}` — arona-ui dist 정적 서빙.
/// webview 가 http 로 로드하면 MCP 와 same-origin 이 돼 fetch 가 CORS/포트
/// 문제 없이 붙는다(file:// 로드 대비 이게 선택 이유). canonicalize 비교로
/// 루트 밖 탈출(../)을 차단한다.
async fn arona_ui_serve(rel: String) -> axum::response::Response {
    use axum::response::IntoResponse as _;
    let not_found = || {
        (
            axum::http::StatusCode::NOT_FOUND,
            [(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")],
            "not found",
        )
            .into_response()
    };
    let Some(root) = arona_ui_root() else { return not_found() };
    let rel = if rel.is_empty() { "index.html".to_string() } else { rel };
    let (Ok(canon_root), Ok(canon)) = (root.canonicalize(), root.join(&rel).canonicalize())
    else {
        return not_found();
    };
    if !canon.starts_with(&canon_root) {
        return not_found();
    }
    match std::fs::read(&canon) {
        // no-store: webview(WKWebView)가 옛 index.html+JS 를 통째 캐시해 relaunch 후에도
        // stale UI 를 띄우던 문제 차단(사용자: 모달 z-fix 가 안 보이던 근본). 로컬·소번들
        // 이라 매 로드 재요청 비용 무시 가능 — 항상 최신.
        Ok(bytes) => (
            axum::http::StatusCode::OK,
            [
                (header::CONTENT_TYPE, static_content_type(&canon)),
                (header::ACCESS_CONTROL_ALLOW_ORIGIN, "*"),
                (header::CACHE_CONTROL, "no-store"),
            ],
            bytes,
        )
            .into_response(),
        Err(_) => not_found(),
    }
}

/// ~/.config/kasaterm/schale-state.json 경로.
fn schale_state_path() -> Option<std::path::PathBuf> {
    let home = kasa_socket::home_dir()?;
    Some(home.join(".config/kasaterm/schale-state.json"))
}

/// schale-state.json 읽기. 파일 없으면 초기값 반환.
fn read_schale_state() -> serde_json::Value {
    let default = serde_json::json!({ "credits": 0, "gold": 0, "affinity_lv": 1, "exp": 0 });
    let Some(path) = schale_state_path() else { return default };
    let Ok(s) = std::fs::read_to_string(&path) else { return default };
    serde_json::from_str::<serde_json::Value>(&s).unwrap_or(default)
}

/// `GET /schale-state` — SCHALE OS 재화/Exp 영속 스냅샷.
async fn schale_state_handler() -> impl IntoResponse {
    let s = read_schale_state();
    ([(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")], Json(s))
}

/// 이 모듈 창구의 라우트. `router` 가 한 표로 합친 뒤 공통 레이어(Origin·토큰 가드)를 두른다.
pub(super) fn routes(backend: &Arc<dyn Backend>) -> axum::Router {
    let slash_backend = backend.clone();
    let mode_get_backend = backend.clone();
    axum::Router::new()
        .route(
            "/mode",
            get(move || mode_get_handler(mode_get_backend.clone())),
        )
        // /arona-ui(슬래시 없음)는 /arona-ui/ 로 리다이렉트 —
        // index.html 의 상대경로 assets(./assets/*) 가 디렉토리
        // 기준으로 풀리려면 trailing slash 가 필요하다.
        //
        // ⚠️ 쿼리를 반드시 들고 간다. 그냥 "/arona-ui/" 로 보내면 `?t=` 가
        // 통째로 사라져, 폰이 슬래시를 빠뜨리고 친 순간 빈 화면이 된다
        // (실측 2026-08-26: 308 뒤 쿠키 0). 토큰 입구는 예외가 없어야 한다.
        .route(
            "/arona-ui",
            get(|axum::extract::RawQuery(q): axum::extract::RawQuery| async move {
                let to = match q.as_deref().filter(|s| !s.is_empty()) {
                    Some(q) => format!("/arona-ui/?{q}"),
                    None => "/arona-ui/".to_string(),
                };
                // temporary(307) 인 이유: 목적지가 쿼리에 따라 달라졌다.
                // 308 은 브라우저가 오래 캐시하므로 토큰이 바뀐 뒤에도 옛
                // 토큰이 붙은 주소로 계속 보내게 된다.
                axum::response::Redirect::temporary(&to)
            }),
        )
        // 폰은 `/arona-ui/?t=<토큰>` 으로 들어온다 — 여기서 쿠키를 안 심으면
        // index.html 만 200 이고 그 뒤 assets 가 전부 403 이라 빈 화면이 된다.
        .route(
            "/arona-ui/",
            get(|q: Query<std::collections::HashMap<String, String>>| async move {
                let cookie = remote_token_cookie(&q.0);
                let mut res = arona_ui_serve(String::new()).await;
                if let Some(c) = cookie {
                    if let Ok(v) = axum::http::HeaderValue::from_str(&c) {
                        res.headers_mut().insert(header::SET_COOKIE, v);
                    }
                }
                res
            }),
        )
        // 여기도 쿠키를 심는다. 진입 페이지가 `/arona-ui/` 하나가 아니기
        // 때문이다 — `settings.html` 이 두 번째 엔트리이고, 앞으로 늘 수도
        // 있다. 그 주소를 북마크하면 HTML 만 200 이고 assets 가 전부 403 이라
        // 빈 화면이 된다(실측 2026-08-26). assets 요청은 `?t=` 를 안 달고
        // 오므로 쿠키가 붙는 건 사람이 주소로 들어온 순간뿐이다.
        .route(
            "/arona-ui/{*path}",
            get(|axum::extract::Path(p): axum::extract::Path<String>,
                 q: Query<std::collections::HashMap<String, String>>| async move {
                let cookie = remote_token_cookie(&q.0);
                let mut res = arona_ui_serve(p).await;
                if let Some(c) = cookie {
                    if let Ok(v) = axum::http::HeaderValue::from_str(&c) {
                        res.headers_mut().insert(header::SET_COOKIE, v);
                    }
                }
                res
            }),
        )
        .route("/schale-state", get(schale_state_handler))
        .route(
            "/slash-commands",
            get(move || slash_commands_handler(slash_backend.clone())),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// kasacollab.py mode_path 와 같은 치환이어야 같은 마커를 공유한다.
    #[test]
    fn mode_slug_matches_python_rule() {
        assert_eq!(
            mode_slug(std::path::Path::new("/Users/kasa/Desktop/momewomo/kasaterm")),
            "-Users-kasa-Desktop-momewomo-kasaterm"
        );
        // '.' 포함 경로 — slug 엣지케이스
        assert_eq!(
            mode_slug(std::path::Path::new("/tmp/app.v1.2/run")),
            "-tmp-app-v1-2-run"
        );
        assert_eq!(mode_slug(std::path::Path::new("/")), "-");
    }
}
