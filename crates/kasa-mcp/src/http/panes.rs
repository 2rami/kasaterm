//! GUI 칸 조작 창구 — 초점·닫기·학생/셸 띄우기·명령·캐릭터 바꾸기·페르소나·붙여넣기·터미널 보이기.

use super::*;
use super::migrate::TRANSCRIPT_UPLOAD_LIMIT;

/// `POST /focus?surface=<id>` — pane 포커스(arona-ui 카드 클릭 → 해당 pane).
/// 쿼리 파라미터인 이유는 session-switch 와 같다(null-origin webview 의 CORS
/// preflight 회피). surface id 의 '%' 는 %25 인코딩(encodeURIComponent) 권장
/// — 다만 실측상 미인코딩 '%1' 도 디코더가 literal 로 통과시킨다(curl 검증).
async fn focus_handler(
    backend: Arc<dyn Backend>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let body = match params.get("surface").map(String::as_str) {
        Some(id) if !id.is_empty() => match backend.focus_surface(id) {
            Ok(()) => serde_json::json!({ "ok": true, "surface_id": id }),
            Err(e) => serde_json::json!({ "ok": false, "error": e.to_string() }),
        },
        _ => serde_json::json!({ "ok": false, "error": "surface query param required" }),
    };
    ([(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")], Json(body))
}

/// `POST /close-pane?surface=<id>[&kill=1]` — 학생(워커) pane 종료. PtyBackend 가
/// SocketClose 로 GUI 에 위임 → layout.rs close_pane 이 leaf 제거 + 포커스 이동.
///
/// 닫힌 pane 은 되살리기 대열에 남아 셸이 산다. `kill=1` 이면 그 대열에서도 걷어
/// 진짜 끝낸다 — 데려오기(역이사)가 쓴다: 대화는 이미 다른 기계로 갔으니 여기
/// 남는 것은 이름표만 붙은 빈 셸이고, 살려 두면 `/term/panes` 에 유령 학생으로 뜬다.
async fn close_pane_handler(
    backend: Arc<dyn Backend>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let kill = params.get("kill").is_some_and(|v| v == "1" || v == "true");
    let body = match params.get("surface").map(String::as_str) {
        Some(id) if !id.is_empty() => match backend.close_surface(id) {
            Ok(()) => {
                let killed = kill && backend.closed_panes(Some(id)).is_ok();
                serde_json::json!({ "ok": true, "surface_id": id, "killed": killed })
            }
            Err(e) => serde_json::json!({ "ok": false, "error": e.to_string() }),
        },
        _ => serde_json::json!({ "ok": false, "error": "surface query param required" }),
    };
    ([(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")], Json(body))
}

/// `POST /spawn-student?character=<name>` — 현재 방에 캐릭터 지정 학생 추가(아로나/
/// 프라나 포함). 자동 빈슬롯 배정 대신 사용자가 고른 캐릭터로 split.
async fn spawn_student_handler(
    backend: Arc<dyn Backend>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let character = params.get("character").map(|s| s.as_str()).unwrap_or("");
    let body = if character.is_empty() {
        serde_json::json!({ "ok": false, "error": "character required" })
    } else {
        match backend.spawn_student(character) {
            // surface = 새 pane id — 스폰 직후 그 학생에게 지시를 보낼 주소.
            Ok(surface) => {
                serde_json::json!({ "ok": true, "character": character, "surface": surface })
            }
            Err(e) => serde_json::json!({ "ok": false, "error": e.to_string() }),
        }
    };
    ([(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")], Json(body))
}

/// `POST /spawn-shell?cwd=<dir>` — 활성 방에 **맨 셸 pane** 하나(캐릭터 없음). 다른
/// 기계의 `to <이 기계>` 가 자기 pane 으로 비출 자리를 세우는 창구다 — 창 없는
/// `/term/spawn` 과 달리 이 창에 진짜로 보인다.
async fn spawn_shell_handler(
    backend: Arc<dyn Backend>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    // `window=new|<n>`·`beside=%id`·`tab_of=%id` — 자리 지정(2026-09-17). 없으면 활성 방.
    let at = kasa_socket::backend::SpawnShellAt::from_query(&params);
    let body = match backend.spawn_shell_at(&at) {
        Ok(reply) if !reply.surface.is_empty() => {
            serde_json::json!({ "ok": true, "surface": reply.surface, "window": reply.window, "room_number": reply.room_number })
        }
        Ok(reply) => serde_json::json!({
            "ok": false,
            "error": reply.error.unwrap_or_else(|| "pane 을 못 세웠어요(설정 화면이 앞이거나 쪼갤 자리가 없음)".into())
        }),
        Err(e) => serde_json::json!({ "ok": false, "error": e.to_string() }),
    };
    ([(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")], Json(body))
}

/// `POST /cmd` body `{"method": "surface.split", "params": {…}}` — 소켓 명령 몇 개를
/// HTTP 로 연다. 폰이 pane 을 닫고·쪼개고·자리 바꾸고·방을 만들 창구다(2026-09-07
/// 지시 「모바일에서도 pane 닫고 추가하고 정렬하고 방 만들고」). 소켓(`kasaterm-cli`)과
/// 같은 이름·같은 인자라 규약이 둘로 안 갈리고, `/m/<label>/cmd` 로 다른 기계에도 간다.
///
/// 허용 목록만 통과시킨다 — 소켓은 같은 사용자 계정의 프로세스만 붙지만 HTTP 는 폰
/// 주소(slug)만 알면 닿는 문이라, 화면 배치를 만지는 것 밖(send·session·profile)은
/// 기존 창구를 그대로 쓰게 둔다.
async fn cmd_handler(
    backend: Arc<dyn Backend>,
    Json(req): Json<serde_json::Value>,
) -> impl IntoResponse {
    const ALLOWED: &[&str] = &[
        "surface.split",
        "surface.close",
        "surface.swap",
        "surface.move",
        // 거울 창에서 분할선을 끌거나 자리를 옮기면 그대로 저쪽에 보낸다(2026-09-17).
        "surface.set_ratio",
        "surface.set_ratio_between",
        "surface.resize_divider",
        "surface.focus",
        "window.new",
        "window.close",
        "window.rename",
        "window.reorder",
        "window.switch",
        "window.list",
    ];
    let method = req.get("method").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let body = if !ALLOWED.contains(&method.as_str()) {
        serde_json::json!({ "ok": false, "error": format!("cmd: `{method}` 는 이 창구로 못 부른다") })
    } else {
        let params = req.get("params").cloned().unwrap_or(serde_json::Value::Null);
        let r = kasa_socket::methods::dispatch(
            &*backend,
            kasa_socket::Request { id: serde_json::json!("http"), method, params },
        );
        serde_json::to_value(&r).unwrap_or_else(|e| {
            serde_json::json!({ "ok": false, "error": format!("응답 직렬화 실패: {e}") })
        })
    };
    ([(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")], Json(body))
}

/// `POST /swap-character?surface=<id>&character=<name>` — pane 캐릭터 교체(PTY respawn,
/// 대화 리셋). persona 가 셸 spawn 시 고정이라 그 pane 을 새 persona 로 다시 띄운다.
async fn swap_character_handler(
    backend: Arc<dyn Backend>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let surface = params.get("surface").map(|s| s.as_str()).unwrap_or("");
    let character = params.get("character").map(|s| s.as_str()).unwrap_or("");
    let body = if surface.is_empty() || character.is_empty() {
        serde_json::json!({ "ok": false, "error": "surface and character required" })
    } else {
        match backend.swap_character(surface, character) {
            Ok(()) => serde_json::json!({ "ok": true }),
            Err(e) => serde_json::json!({ "ok": false, "error": e.to_string() }),
        }
    };
    ([(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")], Json(body))
}

/// `GET /repersona?surface=<id>&character=<name>` — pane 캐릭터 재배정(respawn
/// 없음, 대화·셸 유지). 학생 명령 셰임(`시로코`)이 claude 실행 직전에 호출 —
/// persona 는 셰임의 override 파일이 싣고 GUI 는 헤더·마커·세션바인딩만 갱신.
/// GET 인 이유: 순수 sh 셰임이 한글 캐릭터명을 percent-encode 할 방법이 없어
/// `curl --get --data-urlencode` 를 쓴다(imgopen 과 동일 관례).
async fn repersona_handler(
    backend: Arc<dyn Backend>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let surface = params.get("surface").map(|s| s.as_str()).unwrap_or("");
    let character = params.get("character").map(|s| s.as_str()).unwrap_or("");
    let body = if surface.is_empty() || character.is_empty() {
        serde_json::json!({ "ok": false, "error": "surface and character required" })
    } else {
        match backend.repersona(surface, character) {
            Ok(()) => {
                // 이사가 학생의 **원 세션 id** 를 함께 실어 오면 그 sid 에 캐릭터를
                // 못박는다(바인딩 + 수동 표식). repersona 자체는 pane 이 지금 물고
                // 있는 sid 만 묶는데, 이사 시점의 원격 pane 은 갓 태어나 원 대화의
                // sid 를 아직 모른다 — resume 이 붙은 **뒤**의 복원·명단 검사가
                // 이사 온 학생을 개명하는 구멍이 그래서 남았다(2026-08-31 실측:
                // 시로코가 이사 왕복에서 케이로 돌아왔고 수동 표식이 없어 자동
                // 개명으로 판정). 낡은 서버는 이 파라미터를 몰라도 그냥 무시한다.
                if let Some(sid) = params.get("sid").filter(|s| !s.is_empty()) {
                    let _ = crate::character::bind_session_character(sid, character);
                    crate::character::mark_manual_pick(sid);
                }
                serde_json::json!({ "ok": true })
            }
            Err(e) => serde_json::json!({ "ok": false, "error": e.to_string() }),
        }
    };
    ([(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")], Json(body))
}

async fn agent_identity_handler(
    backend: Arc<dyn Backend>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let field = |key| params.get(key).map(String::as_str).unwrap_or("");
    match backend.prepare_agent_identity(field("surface"), field("sid"), field("character"), field("pid").parse().unwrap_or(0)) {
        Ok(identity) => (axum::http::StatusCode::OK, Json(identity)),
        Err(error) => (axum::http::StatusCode::BAD_REQUEST, Json(serde_json::json!({"error": error.to_string()}))),
    }
}

/// `POST /term/character-theme?theme=<id>` (body = `character_picks` JSON) —
/// 이사(migrate)가 출발지의 캐릭터 테마 선택을 이 기계에 재현하는 창구.
/// 값만 설정 파일에 밖에서 적으면 도는 앱의 캐시(활성 테마·로스터)가 낡은 채
/// 남으므로, 앱 프로세스 안(backend)에서 설정 화면과 같은 경로를 태운다.
/// 테마 팩 폴더 자체는 나르지 않는다 — 없으면 오류로 알려 호출부가 경고만 하고
/// 이사는 계속한다(팩은 큰 그림 뭉치라 기계 간 동기화는 별도 절차).
async fn term_character_theme_post(
    backend: Arc<dyn Backend>,
    q: Query<std::collections::HashMap<String, String>>,
    body: axum::body::Bytes,
) -> impl IntoResponse {
    let err = |m: String| Json(serde_json::json!({ "ok": false, "error": m }));
    // `pack=1` = 테마 팩 zip 운반 — 도착지에 그 팩이 없어 위 적용이 거절됐을 때
    // 호출부가 팩을 싸 보내는 두 번째 호출이다. 풀기는 설정 창 zip 드롭과 같은
    // 코드(zip slip 검사 포함)를 백엔드에서 탄다.
    if q.get("pack").map(String::as_str) == Some("1") {
        if body.is_empty() {
            return err("팩 zip 몸통이 비었다".into());
        }
        let tmp = std::env::temp_dir().join(format!(
            "kasaterm-theme-pack-{}-{}.zip",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0)
        ));
        if let Err(e) = std::fs::write(&tmp, &body) {
            return err(format!("팩 임시 저장 실패: {e}"));
        }
        let out = backend.import_theme_pack(&tmp);
        let _ = std::fs::remove_file(&tmp);
        return match out {
            Ok(id) => Json(serde_json::json!({ "ok": true, "theme": id })),
            Err(e) => err(format!("팩 풀기 실패: {e:#}")),
        };
    }
    let theme = q.get("theme").map(|s| s.as_str()).unwrap_or("");
    // 빈 테마 id = 번들 — 팩 검사 없이 통과. 지정 테마는 팩이 실재해야 적용된다
    // (없는 테마 id 를 설정에 앉히면 로스터가 통째로 비어 배정이 멈춘다).
    if !theme.is_empty() {
        let has_pack = crate::character::themes_root()
            .map(|r| r.join(theme).join("theme.json").is_file())
            .unwrap_or(false);
        if !has_pack {
            return err(format!(
                "테마 팩 '{theme}' 이 이 기계에 없다 — ~/.config/kasaterm/themes/ 에 폴더째 복사해 와야 한다"
            ));
        }
    }
    let picks = String::from_utf8_lossy(&body);
    match backend.apply_character_theme(theme, picks.as_ref()) {
        Ok(()) => Json(serde_json::json!({ "ok": true, "theme": theme })),
        Err(e) => err(format!("{e:#}")),
    }
}

/// `GET /teamname?cwd=<abs>` — 그 cwd 방의 팀명(플레인 텍스트, sh 파싱 프리). claude shim
/// 이 teammate 트리플을 조립하기 직전에 호출한다 — 팀명의 fnv 해시 꼬리를 순수 sh 로 재현할
/// 수 없어 여기가 유일한 계산처다. 빈 cwd·미지정은 빈 응답 — shim 은 빈 팀명이면 플래그를
/// 통째 생략(순정 부팅 폴백). 방(room) 세분화는 안 탄다(cwd 단위): pane 의 room 은 GUI
/// 상태라 shim 이 모르고, 같은 프로젝트 방끼리 채팅이 막히는 것보다 permissive 가 낫다.
async fn teamname_handler(
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let cwd = params.get("cwd").map(|s| s.as_str()).unwrap_or("");
    let body = if cwd.is_empty() {
        String::new()
    } else {
        crate::team::team_name_for(&crate::character::mode_slug(std::path::Path::new(cwd)))
    };
    ([(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")], body)
}

/// `GET /pane-session?pane=<id>` — 그 pane 이 현재 foreground 로 소유한 claude 세션
/// id(bound transcript stem, 플레인 텍스트). statusline 이 `⑂ bg` 배지를 정밀
/// 판별하는 데 쓴다: 자기 session_id 와 이 응답이 같으면 foreground(복원·재부팅
/// 포함), 다르면 진짜 백그라운드 포크. anchor(KASATERM_SESSION_ID) 휴리스틱은
/// 앱 재시작 복원에서 세션↔anchor 가 갈라져 오발화했다(사용자) — 런타임 bound 조회가
/// 정본. 미바인딩·미지정은 빈 응답(statusline 은 빈 응답이면 배지 생략).
async fn pane_session_handler(
    backend: Arc<dyn Backend>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let pane = params.get("pane").map(|s| s.as_str()).unwrap_or("");
    let body = if pane.is_empty() {
        String::new()
    } else {
        backend
            .pane_session_ids()
            .unwrap_or_default()
            .into_iter()
            .find(|(p, _)| p == pane)
            .map(|(_, sid)| sid)
            .unwrap_or_default()
    };
    ([(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")], body)
}

/// `GET /persona?sid=<uuid>` — 그 세션에 바인딩된 학생의 persona(플레인 텍스트).
/// detach 포크는 데몬이 argv 를 재구성하며 --append-system-prompt 가 유실되고, env
/// persona 는 데몬 env(데몬을 낳은 옛 pane 고정)라 계보가 틀리다 — SessionStart 훅이
/// 물려받은 transcript stem(포크 첫 부팅 = 부모 세션 id)으로 여기서 바인딩을 조회해
/// 문맥으로 재주입한다(kasaterm-bind-transcript.sh). 미바인딩·미지정은 빈 응답.
async fn persona_handler(
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let sid = params.get("sid").map(|s| s.as_str()).unwrap_or("");
    // ⚠️ 「말투」 토글을 여기서도 본다. shim 의 `--append-system-prompt` 만 막으면
    // **이 재주입 경로로 그대로 새어 들어간다** — 토글을 꺼도 말투가 계속 붙던 것이
    // 그 때문이다(사용자 2026-08-25 "토글꺼도 적용안되던데").
    let body = if sid.is_empty() {
        String::new()
    } else if !crate::character::persona_enabled() {
        String::new()
    } else {
        // 활성 명부에 없는 이름(다른 테마 팩에서 고른 학생)도 합집합으로 찾는다 —
        // 활성만 보면 그 학생의 resume 부팅에 빈 답이 가서 shim 이 spawn 때의 말투를
        // 빈 것으로 덮고, 얼굴은 있는데 말투만 없는 pane 이 된다(2026-09-09).
        crate::character::session_character(sid)
            .and_then(|name| {
                crate::character::characters_json()
                    .and_then(|c| crate::character::persona_for(&c, &name))
                    .or_else(|| crate::character::persona_for_any(&name))
            })
            .map(|p| format!("[페르소나 유지] {p}"))
            .unwrap_or_default()
    };
    ([(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")], body)
}

/// `GET /persona-portrait?name=<이름>` — 우측 패널에 세울 전신 원화.
///
/// 도트 스프라이트(`/character-sprite`)와 달리 위키 원본이라 세로로 긴 패널에서
/// 사람 크기로 선다. 원화는 레포에 없으므로(gitignore) 못 찾으면 404 를 주고,
/// 프론트가 스프라이트로 떨어진다 — 남의 머신에서 패널이 빈칸이 되지 않게.
async fn persona_portrait_handler(
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let name = params.get("name").cloned().unwrap_or_default();
    let slug = params
        .get("slug")
        .cloned()
        .or_else(|| crate::persona::slug_for(&crate::persona::character_name(&name)))
        .unwrap_or_default();
    match crate::persona::portrait(&slug) {
        Some((bytes, mime)) => (
            [
                (header::ACCESS_CONTROL_ALLOW_ORIGIN, "*"),
                (header::CONTENT_TYPE, mime),
                (header::CACHE_CONTROL, "public, max-age=86400"),
            ],
            bytes,
        )
            .into_response(),
        None => (
            axum::http::StatusCode::NOT_FOUND,
            [(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")],
            Vec::<u8>::new(),
        )
            .into_response(),
    }
}

/// `GET /persona-who` — 지금 우측에 앉아 있는 마스코트가 누구인지(이름·slug·색).
async fn persona_who_handler() -> impl IntoResponse {
    let name = crate::persona::character_name("");
    let slug = crate::persona::slug_for(&name).unwrap_or_default();
    let has_portrait = crate::persona::portrait(&slug).is_some();
    (
        [(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")],
        Json(serde_json::json!({
            "name": name,
            "slug": slug,
            "has_portrait": has_portrait,
        })),
    )
}

/// `POST /persona-who` — 마스코트를 바꾼다. 「한 명 고정」이라 앱에 하나뿐이다.
async fn persona_who_set_handler(
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    let name = body.get("character").and_then(|c| c.as_str()).unwrap_or("");
    match crate::persona::set_character(name) {
        Ok(slug) => (
            [(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")],
            Json(serde_json::json!({ "ok": true, "name": name, "slug": slug })),
        ),
        Err(e) => (
            [(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")],
            Json(serde_json::json!({ "ok": false, "error": e })),
        ),
    }
}

/// `POST /persona-chat` — 말상대에게 한 번 묻는다. board 를 여기서 읽어 프롬프트에
/// 실으므로 프론트는 현황을 알 필요가 없다.
async fn persona_chat_handler(
    backend: Arc<dyn Backend>,
    Json(req): Json<crate::persona::ChatReq>,
) -> impl IntoResponse {
    let board = backend.collab_board().unwrap_or_default();
    let (text, ok) = match crate::persona::chat(&req, &board).await {
        Ok(t) => (t, true),
        Err(e) => (e, false),
    };
    (
        [(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")],
        Json(serde_json::json!({ "ok": ok, "text": text })),
    )
}

/// `GET /character?sid=<sid>` — 세션→캐릭터 바인딩의 정본 캐릭터명(없으면 빈 응답).
/// claude shim 이 --resume/--session-id 부팅 때 pane 상속 캐릭터 대신 이걸로
/// teammate 트리플·persona 를 짓는다(사용자: 모모이 세션이 프라나 배지로 부팅).
async fn character_binding_handler(
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let sid = params.get("sid").map(|s| s.as_str()).unwrap_or("");
    // 고른 명단 밖 이름은 **안 돌려준다**. 이 답을 쓰는 곳은 shim 의 resume 부팅
    // 교정인데(`--resume <sid>` 로 뜬 pane 이 옛 대화의 학생으로 정체성을 되찾는
    // 자리다), 명단을 바꾼 뒤 재배정된 pane 이 resume 되면 그 교정이 **명단 밖
    // 학생을 되살린다**. 빈 답이면 shim 이 pane env 를 그대로 쓰고, 그 env 는 이미
    // 명단 안에서 새로 배정된 이름이다.
    //
    // 2026-08-25 실측: 명단을 바꾸고 처음 재시작했더니 pane 여섯이 화면(인포·board)
    // 은 새 학생인데 이름표·말투만 옛 학생이었다. 배정·저장 계층은 전부 새 이름으로
    // 옳게 갔고, 이 엔드포인트만 옛 이름을 되돌려주고 있었다.
    //
    // 명단을 안 고른 사용자는 영향이 없다 — `is_assignable` 은 명단이 비면 전부
    // 통과시킨다. 「모모이 세션이 프라나로 부팅」 회귀도 그대로 막힌다: 모모이가
    // 명단 안이면 여기를 그냥 지난다.
    let body = if sid.is_empty() {
        String::new()
    } else {
        crate::character::session_character(sid)
            // 사람이 직접 고른 자리는 명단 밖이어도 지킨다(2026-08-26 지시).
            .filter(|c| crate::character::is_assignable_for(sid, c))
            .unwrap_or_default()
    };
    ([(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")], body)
}

/// `POST /terminal-reveal?show=0|1[&pane=%N]` — show/hide the main terminal
/// window. The arona classroom calls this when it opens (hide — the
/// classroom takes the screen over) and from its red-pill button (show —
/// back to the terminal). `pane` optionally focuses that pane on reveal so
/// the classroom can jump the user to a character's seat.
async fn terminal_reveal_handler(
    backend: Arc<dyn Backend>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let body = match params.get("show").map(String::as_str) {
        Some("0") | Some("1") => {
            let show = params.get("show").map(String::as_str) == Some("1");
            let pane = params.get("pane").map(String::as_str).filter(|s| !s.is_empty());
            match backend.reveal_terminal(show, pane) {
                Ok(()) => serde_json::json!({ "ok": true, "show": show }),
                Err(e) => serde_json::json!({ "ok": false, "error": e.to_string() }),
            }
        }
        other => serde_json::json!({
            "ok": false,
            "error": format!("bad or missing show={other:?} (expected 0|1)"),
        }),
    };
    ([(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")], Json(body))
}

/// `POST /paste-active` body:`{text, submit}` — inject `text` into the active
/// pane's PTY (no `surface` — uses whatever pane is focused). `submit=false`
/// (default) types the text without a trailing newline so the user reviews and
/// presses Enter themselves; `submit=true` appends a newline to run it. The BA
/// GUI's offline-session "resume in current terminal" button uses this.
async fn paste_active_handler(
    backend: Arc<dyn Backend>,
    body: String,
) -> impl IntoResponse {
    let cors = [(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")];
    let (text, submit) = if body.trim_start().starts_with('{') {
        match serde_json::from_str::<serde_json::Value>(&body) {
            Ok(v) => (
                v.get("text").and_then(|t| t.as_str()).unwrap_or("").to_string(),
                v.get("submit").and_then(|s| s.as_bool()).unwrap_or(false),
            ),
            Err(e) => {
                return (cors, Json(serde_json::json!({ "ok": false, "error": format!("bad body: {e}") })));
            }
        }
    } else {
        (body.trim().to_string(), false)
    };
    if text.is_empty() {
        return (cors, Json(serde_json::json!({ "ok": false, "error": "text is empty" })));
    }
    let payload = if submit { format!("{text}\n") } else { text };
    let body = match backend.send_text(None, &payload) {
        Ok(()) => serde_json::json!({ "ok": true }),
        Err(e) => serde_json::json!({ "ok": false, "error": e.to_string() }),
    };
    (cors, Json(body))
}

/// 이 모듈 창구의 라우트. `router` 가 한 표로 합친 뒤 공통 레이어(Origin·토큰 가드)를 두른다.
pub(super) fn routes(backend: &Arc<dyn Backend>) -> axum::Router {
    let persona_backend = backend.clone();
    let spawn_student_backend = backend.clone();
    let spawn_shell_backend = backend.clone();
    let cmd_backend = backend.clone();
    let character_theme_backend = backend.clone();
    let swap_character_backend = backend.clone();
    let repersona_backend = backend.clone();
    let agent_identity_backend = backend.clone();
    let terminal_reveal_backend = backend.clone();
    let paste_active_backend = backend.clone();
    let focus_backend = backend.clone();
    let close_backend = backend.clone();
    let pane_session_backend = backend.clone();
    axum::Router::new()
        .route(
            "/paste-active",
            post(move |body: String| {
                paste_active_handler(paste_active_backend.clone(), body)
            }),
        )
        .route(
            "/term/character-theme",
            post(move |q: Query<std::collections::HashMap<String, String>>,
                       body: axum::body::Bytes| {
                term_character_theme_post(character_theme_backend.clone(), q, body)
            })
            // 팩 zip 은 그림 뭉치라 수십 MB — 기본 2MB 상한이면 팩 운반이
            // 이유 없는 실패로만 보인다.
            .layer(axum::extract::DefaultBodyLimit::max(TRANSCRIPT_UPLOAD_LIMIT)),
        )
        .route(
            "/focus",
            post(move |q: Query<std::collections::HashMap<String, String>>| {
                focus_handler(focus_backend.clone(), q)
            }),
        )
        .route(
            "/close-pane",
            post(move |q: Query<std::collections::HashMap<String, String>>| {
                close_pane_handler(close_backend.clone(), q)
            }),
        )
        .route(
            "/spawn-student",
            post(move |q: Query<std::collections::HashMap<String, String>>| {
                spawn_student_handler(spawn_student_backend.clone(), q)
            }),
        )
        .route(
            "/spawn-shell",
            post(move |q: Query<std::collections::HashMap<String, String>>| {
                spawn_shell_handler(spawn_shell_backend.clone(), q)
            }),
        )
        .route(
            "/cmd",
            post(move |body: Json<serde_json::Value>| {
                cmd_handler(cmd_backend.clone(), body)
            }),
        )
        .route(
            "/swap-character",
            post(move |q: Query<std::collections::HashMap<String, String>>| {
                swap_character_handler(swap_character_backend.clone(), q)
            }),
        )
        .route(
            "/repersona",
            get(move |q: Query<std::collections::HashMap<String, String>>| {
                repersona_handler(repersona_backend.clone(), q)
            }),
        )
        .route("/teamname", get(teamname_handler))
        .route("/agent-identity", post(move |q| agent_identity_handler(agent_identity_backend.clone(), q)))
        .route("/persona", get(persona_handler))
        .route("/persona-portrait", get(persona_portrait_handler))
        .route(
            "/persona-who",
            get(persona_who_handler).post(persona_who_set_handler),
        )
        .route(
            "/persona-chat",
            post(move |body| persona_chat_handler(persona_backend.clone(), body)),
        )
        .route("/character", get(character_binding_handler))
        .route(
            "/terminal-reveal",
            post(move |q: Query<std::collections::HashMap<String, String>>| {
                terminal_reveal_handler(terminal_reveal_backend.clone(), q)
            }),
        )
        .route(
            "/pane-session",
            get(move |q: Query<std::collections::HashMap<String, String>>| {
                pane_session_handler(pane_session_backend.clone(), q)
            }),
        )
}
