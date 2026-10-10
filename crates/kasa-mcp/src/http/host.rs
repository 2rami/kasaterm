//! 이 호스트에 대한 창구 — 기계 명부·판 번호·기계 알림·claude 로그인 코드, 재시작·업데이트 승인 중계.

use super::*;

/// `GET /machines` — 기계 명부와 기계별 세션 목록(캐시). 아로나 이사 탭이 폴링한다.
async fn machines_handler(headers: HeaderMap) -> impl IntoResponse {
    let uplinks = crate::uplink::verified_machines(&headers);
    (
        [(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")],
        Json(serde_json::json!({
            "ok": true,
            "machines": crate::machines::snapshot_with_uplinks(&uplinks),
        })),
    )
}

/// `GET /version` — 이 프로그램의 판과 빌드 표식. 다른 기계의 폴링이 이걸 물어
/// 자기 것과 견준다(기계 탭 「빌드 다름」·`to` 경고). 옛 판은 이 라우트가 없어
/// 404 — 묻는 쪽은 그것도 「다름」으로 친다.
async fn version_handler() -> impl IntoResponse {
    (
        [(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")],
        Json(serde_json::json!({
            "ok": true,
            "version": env!("CARGO_PKG_VERSION"),
            "build": crate::machines::build_id(),
            "machine_id": crate::mobile::machine_identity(),
            // 카사넷 주소(id·주소·포트). 이것을 루프백 길로 받은 쪽만 이 기기를 허용 목록에 넣는다.
            "kasanet": crate::kasanet::info(),
        })),
    )
}

/// `POST /machines/announce` body `{label, port, host?, home?, build?, kasanet?}` — 이쪽으로
/// 터널을 든 기계가 「나는 이 이름, 이 포트로 오면 된다」고 알려 온다. 포트는 그쪽
/// ssh 가 이 기계에 열어 둔 되돌아오는 길(-R)이라 루프백으로 닿는다. 명부 파일엔
/// 안 적고 메모리에만 — 알림이 끊기면 반 분 안에 빠진다.
async fn machines_announce_handler(body: axum::body::Bytes) -> impl IntoResponse {
    let err = |m: &str| Json(serde_json::json!({ "ok": false, "error": m }));
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(&body) else {
        return err("JSON body 가 필요해요");
    };
    let label = v.get("label").and_then(|x| x.as_str()).unwrap_or_default();
    let port = v.get("port").and_then(|x| x.as_u64()).unwrap_or(0) as u16;
    if label.trim().is_empty() || port == 0 {
        return err("`label` 과 `port` 가 필요해요");
    }
    let s = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or_default();
    crate::machines::announce_guest(label, port, s("host"), s("home"), s("build"));
    // 알림에 실린 카사넷 id 는 믿지 않는다 — 토큰만 가진 원격도 이 창구를 부를 수 있다. 대신 곧바로
    // 한 바퀴 돌려 그 손님 길(루프백)로 /version 을 물어 배우게 한다.
    if v.get("kasanet").is_some_and(|k| !k.is_null()) {
        crate::machines::poke();
    }
    Json(serde_json::json!({ "ok": true }))
}

/// `POST /claude-login-code` body `{code}` — 브라우저가 주운 OAuth 코드.
///
/// claude CLI 의 승인은 `platform.claude.com` 으로 되돌아온다(localhost 가 아니다).
/// 그래서 코드가 화면에 뜨고 사람이 그것을 복사해 옮겨야 하는데, 그 화면을 보는 것은
/// 브라우저뿐이다 — 확장이 콜백 탭을 보는 순간 여기로 넘기면 사람 손이 빠진다.
/// `redirect_uri` 를 우리 주소로 바꾸는 길은 막혀 있다(토큰 교환이 같은 주소를 요구해
/// 승인까지 마치고 400 이 났다, 2026-09-07) — 그래서 코드를 **대신 받는** 쪽으로 푼다.
///
/// 기다리는 로그인이 없으면 `false` 다. 코드는 일회용이라 보관하지 않는다.
async fn claude_login_code_handler(
    backend: Arc<dyn Backend>,
    body: axum::body::Bytes,
) -> impl IntoResponse {
    let code = serde_json::from_slice::<serde_json::Value>(&body)
        .ok()
        .and_then(|v| v.get("code").and_then(|c| c.as_str()).map(str::to_owned))
        .unwrap_or_default();
    let code = code.trim().to_string();
    // 길이 상한은 넉넉히 — `<코드>#<state>` 꼴이고 둘 다 base64url 이다.
    if code.is_empty() || code.len() > 512 || code.contains(char::is_whitespace) {
        return Json(serde_json::json!({ "ok": false, "error": "코드가 없거나 모양이 아니에요" }));
    }
    let taken = tokio::task::spawn_blocking(move || backend.submit_login_code(&code))
        .await
        .unwrap_or(false);
    Json(serde_json::json!({ "ok": true, "taken": taken }))
}

/// 나쵸 승인 읽기를 중계한다 — 이 창구가 맡은 동작(`relay.action`)의 승인만 답한다. 나쵸는 이 기기 자신의 읽기와 중계를
/// 가를 수 없어서(같은 키), 거르지 않으면 「재시작 승인」으로 위임된 기기가 다른 동작의 승인까지 나른다.
async fn relay_approval(backend: Arc<dyn Backend>, id: String, relay: kasa_socket::app_restart::Relay) -> axum::response::Response {
    let read = move || Ok::<_, anyhow::Error>((crate::board_service::local_id()?, backend.restart_approval(&id)?));
    match tokio::task::spawn_blocking(read).await {
        Ok(Ok((_, view))) if view.action != relay.action => (
            axum::http::StatusCode::NOT_FOUND,
            Json(serde_json::json!({"ok": false, "error": format!("no_approval — 이 창구는 {} 승인만 중계한다", relay.action)})),
        ).into_response(),
        // 받는 쪽이 위임한 기기인지 대조하도록 이 기기의 id 를 함께 싣는다.
        Ok(Ok((machine_id, view))) => Json(serde_json::json!({"machine_id": machine_id, "approval": view})).into_response(),
        Ok(Err(e)) => (axum::http::StatusCode::CONFLICT, Json(serde_json::json!({"ok": false, "error": e.to_string()}))).into_response(),
        Err(_) => axum::http::StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

/// 이 모듈 창구의 라우트. `router` 가 한 표로 합친 뒤 공통 레이어(Origin·토큰 가드)를 두른다.
pub(super) fn routes(backend: &Arc<dyn Backend>) -> axum::Router {
    let login_code_backend = backend.clone();
    let restart_backend = backend.clone();
    axum::Router::new()
        .route("/machines", get(machines_handler))
        .route("/machines/announce", post(machines_announce_handler))
        .route("/claude-login-code", post(move |body: axum::body::Bytes|
            claude_login_code_handler(login_code_backend.clone(), body)))
        .route("/version", get(version_handler))
        // 나쵸가 띄운 학생의 구조화 보고 — 다른 기계에서 넘어온 것도 여기서 그 기계
        // 인박스에 놓인다(tell 과 같은 24 KiB 상한, 같은 인증·origin 가드).
        // 앱 재시작 — 이 기기의 사실과 작업 상태만 읽는다(다른 기기는 `/m/<route>/…` 로 온다).
        // 실행은 사람 승인 흐름이 생기기 전까지 받지 않는다 — 가짜 승인으로 통과할 길을 안 만든다.
        .route("/app/restart/facts", get({
            let backend = restart_backend.clone();
            move || {
                let backend = backend.clone();
                async move {
                    match tokio::task::spawn_blocking(move || backend.restart_facts(None)).await {
                        Ok(Ok(facts)) => Json(serde_json::to_value(facts).unwrap_or_default()).into_response(),
                        Ok(Err(e)) => (axum::http::StatusCode::SERVICE_UNAVAILABLE, Json(serde_json::json!({"ok": false, "error": e.to_string()}))).into_response(),
                        Err(_) => axum::http::StatusCode::INTERNAL_SERVER_ERROR.into_response(),
                    }
                }
            }
        }))
        .route("/app/restart/jobs/{id}", get(|AxPath(id): AxPath<String>| async move {
            let status = kasa_socket::app_restart::jobs_dir()
                .and_then(|dir| kasa_socket::app_restart::job_status(&dir, &id));
            match status {
                Ok(status) => Json(serde_json::to_value(status).unwrap_or_default()).into_response(),
                Err(e) => (axum::http::StatusCode::NOT_FOUND, Json(serde_json::json!({"ok": false, "error": e.to_string()}))).into_response(),
            }
        }))
        .route("/app/restart/approvals/{id}", get({
            let backend = restart_backend.clone();
            move |AxPath(id): AxPath<String>| relay_approval(backend.clone(), id, kasa_socket::app_restart::RESTART_RELAY)
        }))
        // 작업 걸기 — 요청이 무엇을 말하든 나쵸에서 읽은 승인과 이 기기의 지금 사실로만 판정한다.
        .route("/app/restart/jobs", post({
            let backend = restart_backend.clone();
            move |Json(request): Json<kasa_socket::app_restart::JobRequest>| {
                let backend = backend.clone();
                async move {
                    match tokio::task::spawn_blocking(move || backend.restart_start(None, &request)).await {
                        Ok(Ok(value)) => Json(value).into_response(),
                        Ok(Err(e)) => (axum::http::StatusCode::CONFLICT, Json(serde_json::json!({"ok": false, "error": e.to_string()}))).into_response(),
                        Err(_) => axum::http::StatusCode::INTERNAL_SERVER_ERROR.into_response(),
                    }
                }
            }
        }).layer(axum::extract::DefaultBodyLimit::max(16 * 1024)))
        // 앱 업데이트 — 요청은 URL·경로·명령을 싣지 못한다(`kasa_socket::app_update::check_job`). 받는 곳은 공식 피드와
        // 공식 릴리스뿐이고, 판정은 나쵸에서 읽은 승인과 이 기기의 지금 사실로만 한다. 설치 스위치가 꺼져 있으면 거부.
        .route("/app/update/jobs/{id}", get({
            let backend = restart_backend.clone();
            move |AxPath(id): AxPath<String>| {
                let backend = backend.clone();
                async move {
                    match tokio::task::spawn_blocking(move || backend.update_job(None, &id)).await {
                        Ok(Ok(status)) => Json(serde_json::to_value(status).unwrap_or_default()).into_response(),
                        Ok(Err(e)) => (axum::http::StatusCode::NOT_FOUND, Json(serde_json::json!({"ok": false, "error": e.to_string()}))).into_response(),
                        Err(_) => axum::http::StatusCode::INTERNAL_SERVER_ERROR.into_response(),
                    }
                }
            }
        }))
        // 키 없는 기기가 update 승인을 읽는 중계 — 재시작 중계와 창구를 가른다(각자 제 동작만 답한다).
        .route("/app/update/approvals/{id}", get({
            let backend = restart_backend.clone();
            move |AxPath(id): AxPath<String>| relay_approval(backend.clone(), id, kasa_socket::app_update::UPDATE_RELAY)
        }))
        .route("/app/update/jobs", post({
            let backend = restart_backend.clone();
            move |Json(request): Json<kasa_socket::app_update::UpdateRequest>| {
                let backend = backend.clone();
                async move {
                    match tokio::task::spawn_blocking(move || backend.update_start(None, &request)).await {
                        Ok(Ok(value)) => Json(value).into_response(),
                        Ok(Err(e)) => (axum::http::StatusCode::CONFLICT, Json(serde_json::json!({"ok": false, "error": e.to_string()}))).into_response(),
                        Err(_) => axum::http::StatusCode::INTERNAL_SERVER_ERROR.into_response(),
                    }
                }
            }
        }).layer(axum::extract::DefaultBodyLimit::max(16 * 1024)))
}
