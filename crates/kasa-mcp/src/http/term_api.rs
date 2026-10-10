//! 웹 세션 REST(`/term/spawn`·`input`·`screen`·세션 닫기)와 칸 목록(`/term/panes`)·캡처·터널·에이전트 멈춤.

use super::*;
use super::term_assets::avatar_slug;

/// `GET /term/shot?pane=%N&w=<최대 가로px>` — 그 pane 을 kasaterm 이 **실제로 그리는
/// 모습** 그대로 PNG 로.
///
/// 폰 격자 화면은 셀을 브라우저 폰트로 다시 그리므로 테마·폰트·프사가 데스크톱과
/// 다르다(2026-09-02 지적 「폰으로도 테마나 폰트 안 깨지게 보고 싶다」). 나쵸 알림
/// 사진과 같은 오프스크린 렌더(`capture_surface`)를 쓴다 — GUI 가 한 프레임을 그려
/// 파일로 떨구므로 여기서는 그 파일을 읽어 넘기고 지운다. 한 장에 GUI 한 프레임이라
/// 프레임마다 부르지 말고 격자가 바뀔 때만(클라이언트가 스로틀) 부른다. 격자 없는
/// 헤드리스 셸(kasa-serve-web)은 그림이 없어 503 — 클라이언트는 글자 화면에 머문다.
///
/// `w`=0(기본)이면 원본 크기 — 핀치로 키워 읽는 것이 목적이라 줄이면 글자가 뭉갠다.
async fn term_shot_get(
    backend: Arc<dyn Backend>,
    Query(q): Query<std::collections::HashMap<String, String>>,
) -> axum::response::Response {
    let Some(pane) = q.get("pane").filter(|s| !s.is_empty()).cloned() else {
        return (axum::http::StatusCode::BAD_REQUEST, "pane 이 없다").into_response();
    };
    let max_w: u32 = q.get("w").and_then(|v| v.parse().ok()).unwrap_or(0);
    // 요청마다 다른 파일 — 같은 pane 을 두 창이 동시에 보면 한 경로에 두 렌더가
    // 겹쳐 쓴다. GUI 의 「무장 하나」 규칙은 겹침을 거절만 하지 경로를 갈라 주지 않는다.
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let path = std::env::temp_dir().join(format!(
        "kasaterm-shot-{}-{nonce}.png",
        pane.trim_start_matches('%')
    ));
    let path_s = path.to_string_lossy().into_owned();
    // capture_surface 는 GUI 스레드 왕복을 최대 5초 기다리는 동기 호출이다.
    let res = tokio::task::spawn_blocking(move || {
        backend.capture_surface(&pane, Some(&path_s), max_w)
    })
    .await;
    let bytes = match res {
        Ok(Ok(_)) => std::fs::read(&path),
        Ok(Err(e)) => {
            let _ = std::fs::remove_file(&path);
            return (axum::http::StatusCode::SERVICE_UNAVAILABLE, format!("{e:#}")).into_response();
        }
        Err(_) => {
            return (axum::http::StatusCode::SERVICE_UNAVAILABLE, "capture task died").into_response()
        }
    };
    let _ = std::fs::remove_file(&path);
    match bytes {
        Ok(b) => (
            axum::http::StatusCode::OK,
            [
                (header::CONTENT_TYPE, "image/png"),
                (header::CACHE_CONTROL, "no-store"),
            ],
            b,
        )
            .into_response(),
        Err(e) => (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            format!("그림 파일을 못 읽었다: {e}"),
        )
            .into_response(),
    }
}

/// 살아 있는 pane 목록 — 미러 대상을 고르는 데 쓴다.
/// `GET /term/panes` — 붙을 수 있는 pane 목록.
///
/// id 만 주면 폰 드롭다운에 `%86` 이 뜰 뿐이라 **누가 무슨 일을 하던 pane 인지 알
/// 수가 없다.** 그 정보는 이미 board 가 들고 있으므로(캐릭터 이름·작업 제목·상태)
/// 여기서 얹어 준다. 목록의 정본은 여전히 `live_sessions()` 다 — board 에만 있고
/// PTY 가 없는 행에 붙으면 연결이 그냥 끊긴다.
///
/// 웹 셸(`web-…`)은 board 에 없다. 그건 이름 없이 id 만 나가고, 클라가 그때 id 를
/// 그대로 보여 준다.
/// shim 이 붙이는 자동 이름(`seia-p0-70p` — 슬러그·pane 번호·세 글자)인가. 사람이 붙인
/// 이름이 아니라 폰 머리의 세션 이름 자리엔 안 어울린다(2026-09-08 지적 「Seia-asdf
/// 이렇게 칩으로 뜨던데」).
fn is_auto_peer_name(s: &str) -> bool {
    let mut it = s.rsplitn(3, '-');
    let (Some(tail), Some(mid), Some(head)) = (it.next(), it.next(), it.next()) else {
        return false;
    };
    !head.is_empty()
        && tail.len() == 3
        && tail.chars().all(|c| c.is_ascii_alphanumeric())
        && mid.strip_prefix('p').is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
}

/// board 가 상태줄을 못 읽은 pane(탭 안의 claude 등)은 `model` 이 실행 설정 원문
/// (`claude-opus-5`, `claude-fable-5-1[1m]`)으로 남는다. 폰 상태줄엔 사람 말
/// (「Opus 5」·「Fable 5.1 1M」)로 — 이미 다듬어진 값은 그대로 돌려준다.
fn pretty_model_id(raw: &str) -> String {
    let raw = raw.trim();
    let (id, window) = match raw.strip_suffix("[1m]") {
        Some(id) => (id, true),
        None => (raw, false),
    };
    let lower = id.to_ascii_lowercase();
    let title = |s: &str| {
        let mut c = s.chars();
        c.next()
            .map(|f| f.to_ascii_uppercase().to_string() + c.as_str())
            .unwrap_or_default()
    };
    let pretty = if let Some(rest) = lower.strip_prefix("claude-") {
        let mut parts = rest.split('-').filter(|p| !p.is_empty());
        let Some(family) = parts.next() else {
            return raw.to_string();
        };
        // 날짜 꼬리(`20251001`)는 버리고 숫자 조각만 점으로 잇는다: `4-5` → `4.5`.
        let version: Vec<&str> = parts
            .filter(|p| p.chars().all(|c| c.is_ascii_digit()) && p.len() < 8)
            .collect();
        if version.is_empty() {
            title(family)
        } else {
            format!("{} {}", title(family), version.join("."))
        }
    } else if lower.starts_with("gpt-") {
        let mut pieces = lower.split('-');
        let _ = pieces.next();
        let version = pieces.next().unwrap_or_default();
        let suffix = pieces.map(title).collect::<Vec<_>>().join(" ");
        if suffix.is_empty() {
            format!("GPT-{version}")
        } else {
            format!("GPT-{version} {suffix}")
        }
    } else {
        return raw.to_string();
    };
    if window {
        format!("{pretty} 1M")
    } else {
        pretty
    }
}

/// 폰 머리에 다는 세션 이름. claude 는 `/rename` 이 peer_name 으로 온다. codex 는
/// 그런 통로가 없어 사람이 pane 에 붙인 이름(핀)이 그 자리다 — 데스크톱이 codex
/// 화면 안에 배지로 그리는 바로 그 이름. OSC 요약(핀 없음)은 세션 이름이 아니다.
fn pane_session_name(p: &kasa_socket::backend::PaneActivity) -> Option<String> {
    p.peer_name
        .clone()
        .filter(|s| !s.is_empty() && !is_auto_peer_name(s))
        .or_else(|| {
            (p.title_pinned && !p.title.is_empty()).then(|| p.title.clone())
        })
}

async fn term_panes_handler(backend: Arc<dyn Backend>) -> impl IntoResponse {
    let board = backend.collab_board().unwrap_or_default();
    // 방별 그룹핑(폰 목록을 사이드바처럼) — board 는 claude 바인딩 pane 만 담아
    // 순수 셸이 빠지므로, 트리 전체를 아는 pane_windows 가 정본이다.
    let pane_windows: std::collections::HashMap<String, usize> =
        backend.pane_windows().into_iter().collect();
    // 별도 OS 창으로 뗀 pane — 방엔 속하지만 배치 칸엔 없어 폰이 따로 표시한다.
    let undocked: std::collections::HashSet<String> =
        backend.undocked_panes().into_iter().collect();
    // compact 중인 pane — board 는 transcript 만 봐서 「working」으로 뭉개므로 GUI 의
    // 화면 판독을 덧씌운다. 폰이 「컴팩트 중 NN%」로 가른다.
    let compacting: std::collections::HashMap<String, Option<u8>> =
        backend.compacting_panes().into_iter().collect();
    // 방 안의 칸 — 폰 미니맵과 같은 백분율 사각. 다른 기기의 사이드바가 이 방을
    // 본기기 방처럼 배치도로 그린다(2026-09-16 지시). 없으면 그쪽이 칸을 고르게 나눈다.
    // 탭은 바깥 pane 의 칸을 함께 쓴다 — `tab_of` 로 어느 자리의 탭인지 말해 주면
    // 받는 쪽이 한 칸(덱)으로 접는다. 방 이름도 싣는다: 받는 쪽이 pane 마다 폴더
    // 꼬리로 방을 지어 한 방이 셋으로 갈라졌다(2026-09-16 지적).
    let mut rects: std::collections::HashMap<String, (serde_json::Value, Option<String>)> = Default::default();
    for w in backend.windows_overview().unwrap_or_default() {
        for r in &w.panes {
            let rect = serde_json::json!([r.x, r.y, r.w, r.h]);
            for pid in &r.tabs {
                if pid != &r.surface_id {
                    rects.insert(pid.clone(), (rect.clone(), Some(r.surface_id.clone())));
                }
            }
            rects.insert(r.surface_id.clone(), (rect, None));
        }
    }
    let room_labels = backend.sessions().labels;
    // cwd 도 board 만으론 순수 셸이 빠진다 — 셸 pid 에서 직접 읽는 폴백. 이 값이
    // 비면 그 pane 의 거울은 레포를 몰라 재접속 자동 따라잡기가 통째로 건너뛴다.
    let pane_cwds: std::collections::HashMap<String, String> =
        backend.pane_cwds().into_iter().collect();
    // 모델·effort — 원격에서 태어난 학생을 데려가는 기계는 이 값을 달리 알 길이 없다
    // (statusline 보고는 몸통이 있는 기계로만 온다). 비어 있으면 필드가 null.
    let agent_cfg: std::collections::HashMap<String, (String, String)> = backend
        .agent_cfg()
        .into_iter()
        .map(|(p, m, e)| (p, (m, e)))
        .collect();
    // 학생색(header_color) — 이름을 그 학생의 색으로 칠한다(사이드바의 학생 테마).
    // 캐릭터 매칭 규칙이 서버(find_character)에 이미 있으니 클라에 JSON 파싱을
    // 중복시키지 않고 여기서 hex 로 얹는다.
    let rows: Vec<serde_json::Value> = kasa_pty::live_sessions()
        .into_iter()
        .map(|id| {
            let raw = board.iter().find(|p| p.surface_id == id);
            let remote = crate::remote::remote_info(&id);
            // A disconnected/cold-cache mirror is still a mirror. Without this
            // marker another viewer can discover it as a fresh source pane.
            let mirror_label = remote.as_ref().map(|i| {
                if i.label.is_empty() {
                    crate::machines::label_for_base(&i.base).unwrap_or_else(|| i.base.clone())
                } else {
                    i.label.clone()
                }
            });
            // 학생이 지금 돌고 있는 자리만 학생이다. 이름표(`pane_character`)는 claude 가
            // 끝나도 자리에 남아, 셸만 남은 pane 이 「우사기」로 떴다(2026-09-07 지적
            // 「아무것도 없는 pane 셸인데 우사기라고 뜨지」). `harness` 는 셸 밑에 살아
            // 있는 하네스 프로세스를 본 것이라 그 판정에 맞다. 거울 pane 은 하네스가
            // 저쪽 기계에 있어 이 관문을 안 탄다(이름은 저쪽 목록에서 온다).
            // 거울 pane 은 이쪽 board 에 줄이 없다(몸통이 저쪽). 저 기계의 목록 캐시에서
            // 같은 pane 의 줄을 그대로 가져와 이쪽 자리·창 번호만 덮는다 — 이름·얼굴·
            // 상태·상태줄이 저쪽과 똑같이 나온다. `mirror_of` 로 어느 기계의 거울인지.
            if crate::remote::is_remote_pane(&id) {
                if let Some((label, mut row)) =
                    crate::remote::remote_info(&id).and_then(|i| {
                        let label = if i.label.is_empty() {
                            crate::machines::label_for_base(&i.base)?
                        } else {
                            i.label
                        };
                        crate::machines::cached_pane(&label, &i.remote_id).map(|r| (label, r))
                    })
                {
                    row["id"] = serde_json::Value::String(id.clone());
                    row["window"] = serde_json::json!(pane_windows.get(&id).copied());
                    row["closed"] = serde_json::json!(!pane_windows.contains_key(&id));
                    row["undocked"] = serde_json::json!(undocked.contains(&id));
                    row["mirror_of"] = serde_json::Value::String(label);
                    // 칸은 이쪽 방 안의 자리다 — 저쪽 목록의 칸을 그대로 두면 거울이
                    // 저쪽 방의 자리에 그려진다.
                    row["rect"] = rects.get(&id).map(|(r, _)| r.clone()).unwrap_or(serde_json::Value::Null);
                    row["tab_of"] = serde_json::json!(rects.get(&id).and_then(|(_, t)| t.clone()));
                    row["room_label"] = serde_json::json!(pane_windows.get(&id).and_then(|w| room_labels.get(*w)));
                    return row;
                }
            }
            let b = raw.filter(|p| p.harness.is_some() && remote.is_none());
            let web = id.starts_with("web-").then(|| kasa_pty::lookup_session(&id)).flatten();
            serde_json::json!({
                "id": id,
                "surface_key": crate::surface_keys::get(&id),
                "rect": rects.get(&id).map(|(r, _)| r),
                "tab_of": rects.get(&id).and_then(|(_, t)| t.clone()),
                "room_label": pane_windows.get(&id).and_then(|w| room_labels.get(*w)),
                "mirror_of": mirror_label,
                "name": b.and_then(|p| p.character.clone()),
                "title": b.map(|p| p.title.clone()).filter(|s| !s.is_empty()),
                "status": b.map(|p| p.status.clone()).filter(|s| !s.is_empty()).map(|s| {
                    if s == "working" && compacting.contains_key(&id) { "compacting".to_string() } else { s }
                }),
                "compact_pct": compacting.get(&id).copied().flatten(),
                // 무엇을 기다리나(permission·question·idle) · 그 이유 · 쉰 지 몇 초 —
                // 폰이 「승인 기다림 / 질문 기다림 / 답 기다림 / 방금 끝냄 / 쉬는 중」을 가른다.
                "kind": b.and_then(|p| p.attention_kind.clone()),
                "attention_kind": b.and_then(|p| p.attention_kind.clone()),
                "waiting_for": b.and_then(|p| p.waiting_for.clone()),
                "idle_secs": b.and_then(|p| p.idle_secs),
                // 도는 지 몇 초 — 폰 목록 줄이 데스크톱 사이드바 줄처럼 띠 끝에 시간을 단다.
                "busy_secs": b.and_then(|p| p.busy_secs),
                // 무엇을 하는 중인가 — 폰이 「작업 중」 대신 정확한 말을 쓴다(2026-09-08 지시
                // 「모니터링이나 백그라운드 셸 돌아가면 … 작업 중 말고 정확히」). doing 은
                // 최신 도구 라벨, background 는 아직 안 끝난 백그라운드 셸·감시의 설명,
                // subagents 는 도는 서브에이전트의 설명.
                "doing": b.map(|p| p.intent.clone()).filter(|s| !s.is_empty()),
                "background": b.map(|p| p.background.clone()).unwrap_or_default(),
                "subagents": b.map(|p| p.subagents.clone()).unwrap_or_default(),
                // agent 이름이 없는 하네스(codex)는 이름표로 얼굴을 찾는다.
                "slug": b
                    .and_then(|p| p.agent_name.as_deref())
                    .and_then(avatar_slug)
                    .or_else(|| b.and_then(|p| p.character.as_deref()).and_then(crate::character::slug_for_any)),
                // 폰이 PC 처럼 세 줄(이름·세션 이름 / 제목 / 상태줄)을 그리는 재료
                // (2026-09-07 지시 「코덱스 학생이랑 우리가 붙인 세션 이름, 상태줄 3개
                // pc 처럼」). session = `/rename` 으로 붙인 세션 이름(codex 는 없다),
                // harness = claude/codex, context_pct·branch = 상태줄의 그것.
                "session": b.and_then(pane_session_name),
                // 기록이 아직 없는 막 띄운 학생은 board 에 줄이 없다 — 셸 밑 프로세스로 본다. 거울이 첫 말 전부터
                // 이 칸을 대화형으로 연다.
                "harness": b
                    .and_then(|p| p.harness.clone())
                    .or_else(|| kasa_pty::lookup_session(&id).and_then(|s| s.active_agent()).map(|k| k.as_str().to_string())),
                "context_pct": b.map(|p| p.context_pct).filter(|v| *v > 0),
                "branch": b.and_then(|p| p.branch.clone()).filter(|s| !s.is_empty()),
                // 모델 표시명(「Fable 5.1 1M」)은 board 가 이미 사람 말로 다듬어 둔 것 —
                // 아래 `model` 은 실행 설정의 원문(`claude-fable-5-1[1m]`)이라 둘 다 싣는다.
                "model_label": b
                    .map(|p| pretty_model_id(&p.model))
                    .filter(|s| !s.is_empty()),
                "effort_label": b.map(|p| p.effort_default.clone()).filter(|s| !s.is_empty()),
                "window": pane_windows
                    .get(&id)
                    .copied()
                    .or_else(|| raw.map(|p| p.window_idx)),
                // 닫았지만 살아 있는 pane(되살리기 목록) — 어느 창에도 없다. 폰이 이걸
                // 「1번방」에 올렸다(2026-09-07 지적). 웹 셸(`web-`)은 원래 창이 없다.
                "closed": !pane_windows.contains_key(&id) && !id.starts_with("web-"),
                "undocked": undocked.contains(&id),
                // 방 이름 재료 — 원격에서 이 목록을 보는 쪽(이사 탭)은 window 번호만으론
                // 「어느 방」인지 못 말한다. 사람이 읽는 방 이름 규칙(폴더 꼬리)과 같은
                // 원천을 실어 준다.
                "cwd": raw
                    .map(|p| p.cwd.clone())
                    .filter(|s| !s.is_empty())
                    .or_else(|| pane_cwds.get(&id).cloned())
                    .or_else(|| web.as_ref().and_then(|s| s.reported_cwd()).map(|p| p.display().to_string())),
                // 창 없는 웹 셸에서 지금 도는 명령(없으면 프롬프트에서 쉬는 빈 셸). 사이드바가
                // 빈 셸만 한꺼번에 닫고, 일하는 셸은 이름을 보여 주고 남긴다.
                "job": web.as_ref().and_then(|s| s.running_job()),
                "color": b
                    .and_then(|p| p.character.as_deref())
                    .and_then(crate::character::header_color_any),
                "model": agent_cfg.get(&id).map(|(m, _)| m.clone()).filter(|s| !s.is_empty()),
                "effort": agent_cfg.get(&id).map(|(_, e)| e.clone()).filter(|s| !s.is_empty()),
            })
        })
        .collect();
    Json(rows)
}

/// `GET /term/tunnel` — 바깥주소 상태 `{on, host}`. `POST` body `{"on":bool}` —
/// 켜고 끈다. 실체는 `crate::tunnel`(GUI 우하단 칩과 같은 손이다). 가드는 전
/// 라우트 공통 레이어가 덮는다 — 원격은 remote 토큰, 로컬 브라우저는 Origin.
async fn term_tunnel_get() -> impl IntoResponse {
    Json(serde_json::json!({
        "ok": true,
        "on": crate::tunnel::is_on(),
        "host": crate::tunnel::host(),
    }))
}

async fn term_tunnel_post(body: String) -> impl IntoResponse {
    let want_on = serde_json::from_str::<serde_json::Value>(&body)
        .ok()
        .and_then(|v| v.get("on").and_then(|b| b.as_bool()));
    let Some(want_on) = want_on else {
        return Json(serde_json::json!({ "ok": false, "error": "{\"on\":true|false} 가 필요해요" }));
    };
    match crate::tunnel::set(want_on) {
        Ok(on) => Json(serde_json::json!({ "ok": true, "on": on, "host": crate::tunnel::host() })),
        Err(e) => Json(serde_json::json!({ "ok": false, "error": e })),
    }
}

/// 웹 세션 REST 4종 — 소켓도 GUI 도 없는 기계(맥미니 상주 에이전트)가 curl 만으로
/// 셸 세션을 만들고 부리는 창구. `/term/ws` 스폰과 같은 세션을 만들며(등록+keep),
/// 인증은 라우트 공통 레이어(원격=remote 토큰, 로컬 브라우저=Origin)가 덮는다.
///
/// `web-` 접두사만 받는 이유: 이 라우트는 kasaterm GUI(8765)에도 열리므로, GUI 의
/// `%n` pane 을 원격 텍스트 주입·종료의 사정권에 두지 않기 위해서다.
fn web_pane_ok(pane: &str) -> bool {
    pane.starts_with("web-") && pane.len() <= 60
        && pane.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

const TERM_SPAWN_LEASE_MIN: u64 = 30;

const TERM_SPAWN_LEASE_MAX: u64 = 6 * 60 * 60;

/// `POST /term/spawn?cwd=<dir>&cols=&rows=[&lease=<초>]` → `{ok, id[, lease]}` — 새 셸 세션.
///
/// `lease` 는 잠깐 쓰고 버릴 셸(요청 장부 요약 등)용이다. 띄운 쪽이 응답을 못 받으면
/// id 를 몰라 닫을 수가 없다 — 불안정한 터널 너머에서 그렇게 셸이 한 시간에 하나꼴로
/// 쌓였다(2026-09-28, 맥미니 22개). 수명이 다하면 서버가 스스로 놓는다.
async fn term_spawn_post(
    q: Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let id = format!("web-{}", uuid::Uuid::new_v4());
    let opts = kasa_pty::PtyOptions {
        cwd: q
            .get("cwd")
            .cloned()
            .or_else(|| kasa_socket::home_dir().map(|p| p.display().to_string())),
        cols: q.get("cols").and_then(|v| v.parse().ok()).unwrap_or(120),
        rows: q.get("rows").and_then(|v| v.parse().ok()).unwrap_or(32),
        pane_id: id.clone(),
        ..Default::default()
    };
    match kasa_pty::PtySession::start(opts) {
        Ok(s) => {
            let sess = std::sync::Arc::new(s);
            kasa_pty::register_session(&id, &sess);
            // 연결 없이도 살려 둔다 — 이 창구의 존재 이유가 「부착자 없는 세션」이다.
            kasa_pty::keep_session(&id, sess);
            let lease = q
                .get("lease")
                .and_then(|v| v.parse::<u64>().ok())
                .map(|s| s.clamp(TERM_SPAWN_LEASE_MIN, TERM_SPAWN_LEASE_MAX));
            if let Some(secs) = lease {
                let id = id.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_secs(secs)).await;
                    kasa_pty::release_session(&id);
                });
            }
            Json(serde_json::json!({ "ok": true, "id": id, "lease": lease }))
        }
        Err(e) => Json(serde_json::json!({ "ok": false, "error": format!("{e:#}") })),
    }
}

/// `POST /term/input?pane=web-…` body=raw bytes — 세션 stdin 에 그대로 쓴다.
/// 제출(엔터)은 body 에 `\r` 을 실어 보낸다.
async fn term_input_post(
    q: Query<std::collections::HashMap<String, String>>,
    body: axum::body::Bytes,
) -> impl IntoResponse {
    let Some(pane) = q.get("pane").filter(|p| web_pane_ok(p)) else {
        return Json(serde_json::json!({ "ok": false, "error": "`pane`(web-…) 이 필요해요" }));
    };
    let Some(sess) = kasa_pty::lookup_session(pane) else {
        return Json(serde_json::json!({ "ok": false, "error": format!("세션 {pane} 이 없다") }));
    };
    match sess.send_bytes(&body) {
        Ok(()) => Json(serde_json::json!({ "ok": true, "bytes": body.len() })),
        Err(e) => Json(serde_json::json!({ "ok": false, "error": format!("{e:#}") })),
    }
}

/// `GET /term/screen?pane=web-…&lines=N` — 화면+스크롤백 꼬리 N줄(기본 60)을 평문으로.
async fn term_screen_get(
    q: Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let Some(pane) = q.get("pane").filter(|p| web_pane_ok(p)) else {
        return (axum::http::StatusCode::BAD_REQUEST, "`pane`(web-…) 이 필요해요".to_string());
    };
    let Some(sess) = kasa_pty::lookup_session(pane) else {
        return (axum::http::StatusCode::NOT_FOUND, format!("세션 {pane} 이 없다"));
    };
    let lines = q.get("lines").and_then(|v| v.parse().ok()).unwrap_or(60usize).min(2000);
    (axum::http::StatusCode::OK, sess.scrollback_text(lines).join("\n"))
}

/// `DELETE /term/session?pane=web-…` — keep 을 놓아 세션을 끝낸다(마지막 참조면 셸 종료).
async fn term_session_delete(
    q: Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let Some(pane) = q.get("pane").filter(|p| web_pane_ok(p)) else {
        return Json(serde_json::json!({ "ok": false, "error": "`pane`(web-…) 이 필요해요" }));
    };
    let released = kasa_pty::release_session(pane);
    Json(serde_json::json!({ "ok": true, "released": released }))
}

/// 에이전트 pid 를 곱게 끈다. Windows 엔 libc::kill(POSIX) 이 없어 taskkill 로 대신한다
/// — 원격 이사는 지금 macOS 끼리라 이 갈래는 CI 컴파일용이다.
#[cfg(unix)]
fn agent_term_signal(pid: u32) {
    unsafe { libc::kill(pid as i32, libc::SIGTERM) };
}

#[cfg(windows)]
fn agent_term_signal(pid: u32) {
    let _ = std::process::Command::new("taskkill")
        .args(["/PID", &pid.to_string()])
        .spawn();
}

/// pid 가 살아 있나 — unix 는 `kill(pid, 0)`, Windows 는 taskkill 이 동기 종료라 죽은 것으로 본다.
#[cfg(unix)]
fn agent_pid_alive(pid: u32) -> bool {
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

#[cfg(windows)]
fn agent_pid_alive(_pid: u32) -> bool {
    false
}

async fn term_agent_stop_post(
    q: Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let err = |m: String| Json(serde_json::json!({ "ok": false, "error": m }));
    // web- 세션과 GUI pane(%N) 둘 다 받는다. `/term/input` 류의 web-전용 규칙과
    // 달리 여기는 이사 전용이고, `/send` 가 이미 %N 에 입력(exit·Ctrl-C 포함)을
    // 넣을 수 있어 종료만 막는 것은 방어가 아니었다 — 그 반쪽 금지가 「진짜
    // pane(spawn-student)으로 나간 학생은 자동으로 못 데려온다」만 남겼다
    // (2026-08-30 실측: 미도리·미쿠 둘 다 수동 절차로 데려왔다).
    let Some(pane) = q.get("pane").filter(|p| web_pane_ok(p) || p.starts_with('%')) else {
        return err("`pane`(web-… 또는 %N) 이 필요해요".into());
    };
    let Some(sess) = kasa_pty::lookup_session(pane) else {
        return err(format!("세션 {pane} 이 없다"));
    };
    let Some(shell) = sess.shell_pid() else {
        return err(format!("{pane} 의 셸 pid 를 모른다"));
    };
    let table = kasa_pty::process_table_shared();
    let Some((kind, agent_pid)) = kasa_pty::agent_pid_for_shell(&table, shell) else {
        // 이미 꺼져 있는 것은 실패가 아니다 — 대화는 디스크에 남아 있고,
        // 역이사는 그걸 걷어 가면 된다.
        return Json(serde_json::json!({ "ok": true, "stopped": false }));
    };
    let bypass = std::process::Command::new("ps")
        .args(["-o", "command=", "-p", &agent_pid.to_string()])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).contains("--dangerously-skip-permissions"))
        .unwrap_or(false);
    agent_term_signal(agent_pid);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
    while agent_pid_alive(agent_pid) {
        if std::time::Instant::now() > deadline {
            return err(format!(
                "{kind:?}(pid {agent_pid}) 가 8초 안에 안 꺼졌다 — 반쯤 산 채 두는 것보다 세우는 게 낫다"
            ));
        }
        tokio::time::sleep(std::time::Duration::from_millis(120)).await;
    }
    // 이사로 학생을 내줬다 — 이 pane 의 sid 주장을 걷으라고 GUI 틱에 알린다.
    // 안 걷으면 세션 저장이 그 대화를 이 pane 것으로 굳혀, 재시작 복원이 남의
    // 기계로 간 대화를 다시 연다(2026-08-30 이중 열림 실측).
    crate::remote::note_migrated_away(pane);
    Json(serde_json::json!({
        "ok": true,
        "stopped": true,
        "agent": format!("{kind:?}").to_lowercase(),
        "bypass": bypass,
    }))
}

/// 이 모듈 창구의 라우트. `router` 가 한 표로 합친 뒤 공통 레이어(Origin·토큰 가드)를 두른다.
pub(super) fn routes(backend: &Arc<dyn Backend>) -> axum::Router {
    let panes_backend = backend.clone();
    let colors_backend = backend.clone();
    let shot_backend = backend.clone();
    axum::Router::new()
        .route(
            "/term/panes",
            get(move || term_panes_handler(panes_backend.clone())),
        )
        .route(
            "/term/pane-info",
            get(|q: Query<std::collections::HashMap<String, String>>| async move {
                Json(crate::pane_info::wait(q.0).await)
            }),
        )
        .route("/term/device-colors", get(move || {
            let backend = colors_backend.clone();
            async move { Json(backend.device_colors().unwrap_or_else(|_| serde_json::json!({}))) }
        }))
        .route(
            "/term/shot",
            get(move |q: Query<std::collections::HashMap<String, String>>| {
                term_shot_get(shot_backend.clone(), q)
            }),
        )
        .route("/term/spawn", post(term_spawn_post))
        .route("/term/input", post(term_input_post))
        .route("/term/screen", get(term_screen_get))
        .route("/term/session", axum::routing::delete(term_session_delete))
        .route("/term/agent-stop", post(term_agent_stop_post))
        .route(
            "/term/tunnel",
            get(term_tunnel_get).post(term_tunnel_post),
        )
}

#[cfg(test)]
mod tests {
    #[test]
    fn pretty_model_id_turns_raw_ids_into_status_words() {
        assert_eq!(super::pretty_model_id("claude-opus-5"), "Opus 5");
        assert_eq!(super::pretty_model_id("claude-fable-5-1[1m]"), "Fable 5.1 1M");
        assert_eq!(super::pretty_model_id("claude-haiku-4-5-20251001"), "Haiku 4.5");
        assert_eq!(super::pretty_model_id("gpt-5.6-sol"), "GPT-5.6 Sol");
        assert_eq!(super::pretty_model_id("Fable 5.1 1M"), "Fable 5.1 1M", "이미 다듬어진 값은 그대로");
        assert_eq!(super::pretty_model_id(""), "");
    }

    #[test]
    fn pane_session_name_prefers_peer_name_then_pinned_title() {
        let mut p = kasa_socket::backend::PaneActivity {
            title: "kasaterm".into(),
            ..Default::default()
        };
        assert_eq!(super::pane_session_name(&p), None, "OSC 요약은 세션 이름이 아니다");
        p.title_pinned = true;
        assert_eq!(super::pane_session_name(&p).as_deref(), Some("kasaterm"));
        p.peer_name = Some("wgpu로바꾸기".into());
        assert_eq!(super::pane_session_name(&p).as_deref(), Some("wgpu로바꾸기"));
        p.peer_name = Some(String::new());
        p.title.clear();
        assert_eq!(super::pane_session_name(&p), None);
        p.peer_name = Some("seia-p0-70p".into());
        assert_eq!(super::pane_session_name(&p), None, "자동 이름은 세션 이름이 아니다");
        assert!(super::is_auto_peer_name("arona-p4-70p"));
        assert!(!super::is_auto_peer_name("codex /rename, /resume"));
        assert!(!super::is_auto_peer_name("wgpu로바꾸기"));
        assert!(!super::is_auto_peer_name("my-p1-thing"), "꼬리가 세 글자가 아니면 사람 이름");
    }
}
