//! 이사(migrate) — 기계 사이 세션 옮기기의 받는 쪽과 보내는 쪽 창구: 레포 맞추기, claude·codex 대화
//! 올리기·내려받기, `/transfer/*`, `/pane-migrate`.

use super::*;

/// `POST /pane-migrate` body `{pane, target, cwd?, force?}` — 이사를 웹 UI 에서.
/// `target` 은 기계 라벨 또는 `"local"`(데려오기). 주소·경로 매핑은 여기(서버)가
/// 푼다 — UI 가 기계의 파일시스템 구조를 알 이유가 없다.
///
/// 이사는 240초까지 걸리는 동기 작업이라 blocking 스레드로 내린다 — 안 내리면
/// tokio 워커 하나가 그동안 통째로 잠긴다.
async fn pane_migrate_handler(
    backend: Arc<dyn Backend>,
    body: axum::body::Bytes,
) -> impl IntoResponse {
    let err = |m: String| Json(serde_json::json!({ "ok": false, "error": m }));
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(&body) else {
        return err("JSON body 가 필요해요".into());
    };
    let Some(pane) = v.get("pane").and_then(|x| x.as_str()).map(str::to_string) else {
        return err("`pane` 이 필요해요".into());
    };
    let Some(target) = v.get("target").and_then(|x| x.as_str()).map(str::to_string) else {
        return err("`target`(기계 라벨 또는 \"local\") 이 필요해요".into());
    };
    let cwd = v.get("cwd").and_then(|x| x.as_str()).map(str::to_string);
    let force = v.get("force").and_then(|x| x.as_bool()).unwrap_or(false);
    let out = tokio::task::spawn_blocking(move || {
        if target == "local" {
            backend.migrate_pane_back(&pane, cwd.as_deref(), force)
        } else {
            let Some(m) = crate::machines::find(&target) else {
                anyhow::bail!("기계 {target} 를 명부에서 못 찾았어요 — machines.json 을 확인");
            };
            // cwd 미지정이면 지금 pane 의 로컬 경로를 명부 roots 로 매핑한다.
            let cwd = match cwd {
                Some(c) => Some(c),
                None => {
                    let local = backend
                        .collab_board()
                        .unwrap_or_default()
                        .into_iter()
                        .find(|p| p.surface_id == pane)
                        .map(|p| p.cwd);
                    match local {
                        Some(l) if !l.is_empty() => {
                            Some(crate::machines::map_local_to_remote(&m, &l).ok_or_else(
                                || {
                                    anyhow::anyhow!(
                                        "{l} 를 {target} 경로로 못 옮겼어요 — machines.json roots 에 규칙을 적거나 cwd 를 지정"
                                    )
                                },
                            )?)
                        }
                        _ => None,
                    }
                }
            };
            // 이 경로는 대화 이사 전용이다 — 태생 실행 명령(run)은 셸 pane 을
            // 저쪽에서 처음부터 띄울 때만 뜻이 있어 여기선 늘 없다.
            backend.migrate_pane(&pane, &m.base, cwd.as_deref(), force, None)
        }
    })
    .await;
    match out {
        Ok(Ok(id)) => Json(serde_json::json!({ "ok": true, "remote_id": id })),
        Ok(Err(e)) => err(format!("{e:#}")),
        Err(e) => err(format!("작업 스레드 실패: {e}")),
    }
}

async fn transfer_snapshot_handler(backend: Arc<dyn Backend>) -> Json<serde_json::Value> {
    let result = tokio::task::spawn_blocking(move || backend.transfer_snapshot()).await;
    Json(match result {
        Ok(Ok(snapshot)) => serde_json::json!({"ok":true,"snapshot":snapshot}),
        Ok(Err(error)) => serde_json::json!({"ok":false,"error":error.to_string()}),
        Err(error) => serde_json::json!({"ok":false,"error":error.to_string()}),
    })
}

async fn transfer_spawn_handler(backend: Arc<dyn Backend>, Json(request): Json<kasa_socket::transfer::SpawnRequest>) -> Json<serde_json::Value> {
    let result = tokio::task::spawn_blocking(move || backend.transfer_spawn(&request)).await;
    Json(match result {
        Ok(Ok(session)) => serde_json::json!({"ok":true,"session":session}),
        Ok(Err(error)) => serde_json::json!({"ok":false,"error":error.to_string()}),
        Err(error) => serde_json::json!({"ok":false,"error":error.to_string()}),
    })
}

async fn transfer_close_handler(backend: Arc<dyn Backend>, Json(identity): Json<kasa_socket::transfer::SessionIdentity>) -> Json<serde_json::Value> {
    let result = tokio::task::spawn_blocking(move || backend.transfer_close(&identity)).await;
    Json(match result {
        Ok(Ok(())) => serde_json::json!({"ok":true}),
        Ok(Err(error)) => serde_json::json!({"ok":false,"error":error.to_string()}),
        Err(error) => serde_json::json!({"ok":false,"error":error.to_string()}),
    })
}

async fn transfer_migrate_handler(backend: Arc<dyn Backend>, Json(request): Json<kasa_socket::transfer::MigrateRequest>) -> Json<serde_json::Value> {
    let result = tokio::task::spawn_blocking(move || backend.transfer_migrate(&request)).await;
    Json(match result {
        Ok(Ok(remote_id)) => serde_json::json!({"ok":true,"remote_id":remote_id}),
        Ok(Err(error)) => serde_json::json!({"ok":false,"error":error.to_string()}),
        Err(error) => serde_json::json!({"ok":false,"error":error.to_string()}),
    })
}

/// `POST /term/repo?path=<abs>&url=<git>&branch=<name>` — 이 기계에 그 레포를
/// **있게** 만든다: 없으면 clone, 있으면 fetch + fast-forward pull.
///
/// 이사(migrate)의 전제다 — 대화만 건너오고 코드가 없거나 뒤처져 있으면 옮겨온
/// 학생이 딴 세상에서 깨어난다. 되돌릴 수 없는 짓은 하지 않는다: 이 기계에
/// 안 올린 변경이 있으면 **손대지 않고 사유를 돌려준다**(맥북에서 막아 세우는
/// 것과 같은 규칙), merge 도 rebase 도 아닌 fast-forward 만 받는다.
///
/// 셸을 안 거치고 git 을 직접 실행한다 — 인자에 셸 메타문자가 섞여도 명령이
/// 갈라지지 않는다.
async fn term_repo_post(
    q: Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let err = |m: String| Json(serde_json::json!({ "ok": false, "error": m }));
    let Some(path) = q.get("path").filter(|p| p.starts_with('/')) else {
        return err("`path`(절대경로) 가 필요해요".into());
    };
    let branch = q.get("branch").cloned().unwrap_or_default();
    let git = |args: Vec<String>| -> (bool, String) {
        match std::process::Command::new("git").args(&args).output() {
            Ok(o) => (
                o.status.success(),
                format!(
                    "{}{}",
                    String::from_utf8_lossy(&o.stdout),
                    String::from_utf8_lossy(&o.stderr)
                )
                .trim()
                .to_string(),
            ),
            Err(e) => (false, format!("git 실행 실패: {e}")),
        }
    };
    let exists = std::path::Path::new(path).join(".git").exists();
    let action;
    let mut dirty_lines = 0usize;
    if !exists {
        let Some(url) = q.get("url").filter(|u| !u.is_empty()) else {
            return err(format!("{path} 에 레포가 없고 `url` 도 없어요"));
        };
        if let Some(parent) = std::path::Path::new(path).parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                return err(format!("상위 폴더 생성 실패: {e}"));
            }
        }
        let (ok, out) = git(vec![
            "clone".into(),
            url.clone(),
            path.clone(),
        ]);
        if !ok {
            return err(format!("clone 실패: {out}"));
        }
        // 갓 clone 한 자리는 원격 기본 브랜치다 — 부른 쪽이 브랜치를 지정했으면
        // 거기로 옮겨야 한다. 안 그러면 학생이 남의 브랜치 위에서 깬다(2026-08-27
        // 실측: yuzu/grass-terrain 을 부탁했는데 main 으로 앉았다 — 그날은 두
        // 브랜치의 끝이 같아 티가 안 났을 뿐이다).
        if !branch.is_empty() {
            let (ok, out) = git(vec!["-C".into(), path.clone(), "checkout".into(), branch.clone()]);
            if !ok {
                return err(format!("clone 은 됐는데 {branch} 로 못 옮겼어요: {out}"));
            }
        }
        action = "cloned";
    } else {
        // 도착지에 커밋 안 한 변경이 있어도 **세우지 않는다.** 이 자리가 해야 하는 일은
        // 다음 단계(bundle 재현)의 전제인 origin 오브젝트를 이 기계에 들여놓는 것뿐이고,
        // fetch 는 오브젝트만 받아 워킹트리를 안 건드린다. 남의 작업을 덮는 것은 그 뒤의
        // checkout·ff-merge 라서 **그 걸음만** 건너뛴다 — 옮겨온 짐은 bundle 이
        // refs/kasaterm/incoming 으로 보관하므로 잃는 것도 없다(2026-09-09: 도착지 학생의
        // 미커밋 9개 때문에 6-pane 이사가 통째로 막혔다. 예전엔 여기서 「그쪽 학생이
        // 커밋하고 오라」고 거부했다).
        let (_, dirty) = git(vec!["-C".into(), path.clone(), "status".into(), "--porcelain".into()]);
        dirty_lines = dirty.lines().count();
        let (ok, out) = git(vec!["-C".into(), path.clone(), "fetch".into(), "--prune".into()]);
        if !ok {
            return err(format!("fetch 실패: {out}"));
        }
        if dirty_lines > 0 {
            action = "kept-dirty";
        } else {
            if !branch.is_empty() {
                let (ok, out) = git(vec!["-C".into(), path.clone(), "checkout".into(), branch.clone()]);
                if !ok {
                    return err(format!("{branch} 로 못 옮겼어요: {out}"));
                }
            }
            let (mut ok, mut out) = git(vec!["-C".into(), path.clone(), "merge".into(), "--ff-only".into(), "@{u}".into()]);
            // 업스트림이 안 잡힌 브랜치(`checkout -B` 로 앉힌 거울)는 `@{u}` 가 없어 여기서
            // 매번 서고, 거울이 origin 보다 한참 뒤처진 채 「준비됐다」로 넘어갔다
            // (2026-09-02 실측: 미니 swarm 이 origin 뒤 12 커밋에서 fetched-only). 같은
            // 이름의 origin 브랜치로 한 번 더 — 빨리감기만 하므로 이쪽 커밋을 잃을 길은 없다.
            if !ok && !branch.is_empty() && out.contains("no upstream") {
                (ok, out) = git(vec![
                    "-C".into(),
                    path.clone(),
                    "merge".into(),
                    "--ff-only".into(),
                    format!("origin/{branch}"),
                ]);
            }
            // 이미 최신이면 실패 문구가 나오지만 그건 사고가 아니다.
            action = if ok { "pulled" } else if out.contains("up to date") || out.contains("최신") {
                "already-current"
            } else {
                "fetched-only"
            };
        }
    }
    let (_, head) = git(vec!["-C".into(), path.clone(), "rev-parse".into(), "--short".into(), "HEAD".into()]);
    let (_, br) = git(vec!["-C".into(), path.clone(), "rev-parse".into(), "--abbrev-ref".into(), "HEAD".into()]);
    // claude 신뢰 선탑재 — 이 기계에서 처음 보는 폴더면 claude 가 뜨자마자
    // 「이 폴더를 신뢰하나」 화면에서 멈추고, 이사 온 학생은 자동 resume 이
    // 그 화면에 먹혀 밤새 서 있는다(2026-08-27 이사 실측 메모). 레포를 준비하는
    // 이 자리가 곧 「여기서 claude 를 돌리겠다」는 뜻이므로 여기서 심는다.
    kasa_socket::claude_trust::preseed(std::path::Path::new(&path));
    Json(serde_json::json!({ "ok": true, "action": action, "head": head, "branch": br, "path": path, "dirty": dirty_lines }))
}

/// 이사(migrate) 상한 — 대화 jsonl 하나의 최대 크기.
pub(super) const TRANSCRIPT_UPLOAD_LIMIT: usize = 512 << 20;

/// 이사에 실려 오는 세션 id 검증 — claude sid 는 uuid 꼴이다. 경로는 서버가
/// cwd·sid 로 계산하므로(claude 규칙: `/`·`.` → `-`) 이 문자집합 검사가 곧
/// 경로 탈출 방어다: `/` 도 `.` 도 여기서 걸러진다.
fn valid_session_id(sid: &str) -> bool {
    !sid.is_empty()
        && sid.len() <= 80
        && sid.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// `GET /term/transcript?cwd=<abs>&sid=<uuid>` — 업로드(POST)와 대칭인 내려받기.
/// 역이사(원격→로컬 되가져오기)가 대화를 걷어 갈 때 쓴다. 통짜 바이트로 준다 —
/// 업로드 쪽 상한(512MB)과 같은 급이라 JSON 래핑 없이 그대로가 맞다.
async fn term_transcript_get(
    q: Query<std::collections::HashMap<String, String>>,
) -> axum::response::Response {
    use axum::response::IntoResponse as _;
    let bad = |m: &str| {
        (axum::http::StatusCode::BAD_REQUEST, m.to_string()).into_response()
    };
    let Some(cwd) = q.get("cwd").filter(|s| s.starts_with('/')) else {
        return bad("`cwd`(이 기계 기준 절대경로) 가 필요해요");
    };
    let Some(sid) = q.get("sid").filter(|s| valid_session_id(s)) else {
        return bad("`sid`(claude 세션 uuid) 가 필요해요");
    };
    let Some(path) =
        kasa_socket::sessions::session_jsonl_path(std::path::Path::new(cwd.as_str()), sid)
    else {
        return bad("서버 HOME 을 몰라 저장 위치를 못 정해요");
    };
    match tokio::fs::read(&path).await {
        Ok(bytes) => (
            axum::http::StatusCode::OK,
            [(header::CONTENT_TYPE, "application/octet-stream")],
            bytes,
        )
            .into_response(),
        Err(e) => (
            axum::http::StatusCode::NOT_FOUND,
            format!("대화 파일이 없어요({}): {e}", path.display()),
        )
            .into_response(),
    }
}

/// `GET /term/repo?path=<abs>` — 그 레포의 「이 기계에만 있는 것」 상태.
/// 역이사의 git 관문이다: 순방향이 출발지에서 미커밋·미push 를 검사하듯,
/// 역방향은 이걸 물어 원격에만 있는 변경을 실은 채 떠나는 사고를 막는다.
async fn term_repo_get(
    q: Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let err = |m: String| Json(serde_json::json!({ "ok": false, "error": m }));
    let Some(path) = q.get("path").filter(|p| p.starts_with('/')) else {
        return err("`path`(절대경로) 가 필요해요".into());
    };
    let git = |args: &[&str]| -> (bool, String) {
        match std::process::Command::new("git").arg("-C").arg(path).args(args).output() {
            Ok(o) => (
                o.status.success(),
                format!(
                    "{}{}",
                    String::from_utf8_lossy(&o.stdout),
                    String::from_utf8_lossy(&o.stderr)
                )
                .trim()
                .to_string(),
            ),
            Err(e) => (false, format!("git 실행 실패: {e}")),
        }
    };
    if !std::path::Path::new(path).join(".git").exists() {
        return Json(serde_json::json!({ "ok": true, "exists": false }));
    }
    let (_, dirty) = git(&["status", "--porcelain"]);
    // 현재 브랜치만 본다 — 전 브랜치(--branches)를 세면 옛 실험 가지가 수천으로
    // 잡혀 착시가 된다(2026-08-28 실측: 미push 1046건이 전부 죽은 실험 브랜치였다).
    let (_, unpushed) = git(&["log", "--oneline", "@{u}..HEAD"]);
    let (_, origin) = git(&["remote", "get-url", "origin"]);
    let (_, branch) = git(&["rev-parse", "--abbrev-ref", "HEAD"]);
    let (_, head) = git(&["rev-parse", "--short", "HEAD"]);
    let count = |s: &str| if s.is_empty() { 0 } else { s.lines().count() };
    Json(serde_json::json!({
        "ok": true,
        "exists": true,
        "dirty": count(&dirty),
        "unpushed": count(&unpushed),
        "origin": origin,
        "branch": branch,
        "head": head,
    }))
}

/// `GET /term/repo-sync?path=<abs>` — 이 기계 레포의 「이 기계에만 있는 것」
/// (미push 커밋 + 미커밋 변경)을 bundle 로 떠서 내려준다. 역이사가 원격의
/// 작업 상태를 로컬에 재현할 때 쓴다. 실어 갈 것이 없으면 JSON 으로 답하고,
/// 있으면 메타를 응답 헤더에 싣고 본문은 bundle 통짜 바이트다(대화 창구와
/// 같은 꼴 — base64 래핑은 큰 레포에서 1/3 을 그냥 부풀린다).
async fn term_repo_sync_get(
    q: Query<std::collections::HashMap<String, String>>,
) -> axum::response::Response {
    use axum::response::IntoResponse as _;
    let Some(path) = q.get("path").filter(|p| p.starts_with('/')) else {
        return Json(serde_json::json!({ "ok": false, "error": "`path`(절대경로) 가 필요해요" }))
            .into_response();
    };
    let path = path.clone();
    // git 은 블로킹이고 큰 레포에선 초 단위다 — 요청 스레드를 세우지 않는다.
    let snap = tokio::task::spawn_blocking(move || {
        crate::reposync::snapshot(std::path::Path::new(&path))
    })
    .await;
    match snap {
        Ok(Ok(None)) => Json(serde_json::json!({ "ok": true, "nothing": true })).into_response(),
        Ok(Ok(Some(s))) => {
            if s.bundle.len() > TRANSCRIPT_UPLOAD_LIMIT {
                return Json(serde_json::json!({
                    "ok": false,
                    "error": format!("떠낸 bundle 이 너무 크다({}MB) — 커밋·push 로 줄이고 다시", s.bundle.len() >> 20),
                }))
                .into_response();
            }
            (
                axum::http::StatusCode::OK,
                [
                    (header::CONTENT_TYPE, "application/octet-stream".to_string()),
                    (header::HeaderName::from_static("x-kasa-head"), s.head),
                    (header::HeaderName::from_static("x-kasa-sync"), s.sync),
                    (header::HeaderName::from_static("x-kasa-branch"), s.branch),
                    (header::HeaderName::from_static("x-kasa-origin"), s.origin),
                    (
                        header::HeaderName::from_static("x-kasa-dirty"),
                        if s.dirty { "1" } else { "0" }.to_string(),
                    ),
                ],
                s.bundle,
            )
                .into_response()
        }
        Ok(Err(e)) => {
            Json(serde_json::json!({ "ok": false, "error": format!("{e:#}") })).into_response()
        }
        Err(e) => Json(serde_json::json!({ "ok": false, "error": format!("스냅샷 작업 실패: {e}") }))
            .into_response(),
    }
}

/// unix 도메인 소켓에 바이트 한 줄을 꽂는다(cross-session 메시지 배달).
#[cfg(unix)]
/// Windows named pipe 에 바이트 한 줄을 꽂는다(cross-session 메시지 배달).
///
/// claude Windows 의 `messagingSocketPath` 는 유닉스 소켓이 아니라 named pipe
/// (`\\.\pipe\LOCAL\cc-msg-<32hex>`, 2026-09-01 데스크탑 실측)다. 파이프는 파일
/// API 로 연다 — 서버(claude)가 있으면 클라이언트로 붙고, 유닉스 갈래와 똑같이
/// JSON 한 줄을 write 후 닫으면 배달이 된다. 파이프가 순간 바쁘면(다른 연결 처리
/// 중) ERROR_PIPE_BUSY 로 열기가 실패하므로 잠깐 뒤 몇 번 다시 시도한다.
/// ⚠️ 접근 모드는 실물에서 맞춘다 — claude 파이프가 inbound(서버가 읽기만)면
/// write only 여야 하고, DUPLEX 면 read+write 다 된다. 일단 write only 로 연다.
#[cfg(windows)]
fn inject_into_socket(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut last = std::io::Error::other("파이프 열기 실패");
    for _ in 0..5 {
        match std::fs::OpenOptions::new().write(true).open(path) {
            Ok(mut f) => {
                f.write_all(bytes)?;
                f.flush()?;
                return Ok(());
            }
            Err(e) => {
                last = e;
                std::thread::sleep(std::time::Duration::from_millis(120));
            }
        }
    }
    Err(last)
}

#[cfg(not(any(unix, windows)))]
fn inject_into_socket(_path: &std::path::Path, _bytes: &[u8]) -> std::io::Result<()> {
    Err(std::io::Error::other("cross-session 주입은 unix·windows 전용"))
}

/// `POST /term/repo-sync?path=&head=&sync=&branch=&dirty=1&force=1` body=bundle —
/// 순방향 이사가 출발지에서 떠낸 스냅샷을 이 기계 레포에 재현한다.
/// 도착지 보호 관문(dirty·브랜치 전환·되감기)은 reposync::apply 안에 있다.
async fn term_repo_sync_post(
    q: Query<std::collections::HashMap<String, String>>,
    body: axum::body::Bytes,
) -> impl IntoResponse {
    let err = |m: String| Json(serde_json::json!({ "ok": false, "error": m }));
    let Some(path) = q.get("path").filter(|p| p.starts_with('/')) else {
        return err("`path`(절대경로) 가 필요해요".into());
    };
    let (Some(head), Some(sync)) = (q.get("head"), q.get("sync")) else {
        return err("`head`·`sync` 가 필요해요".into());
    };
    let ok_sha = |s: &String| !s.is_empty() && s.len() <= 64 && s.chars().all(|c| c.is_ascii_hexdigit());
    if !ok_sha(head) || !ok_sha(sync) {
        return err("`head`·`sync` 는 커밋 sha 여야 해요".into());
    }
    let (path, head, sync) = (path.clone(), head.clone(), sync.clone());
    let branch = q.get("branch").cloned().unwrap_or_default();
    let dirty = q.get("dirty").map(|v| v == "1").unwrap_or(false);
    let force = q.get("force").map(|v| v == "1").unwrap_or(false);
    let applied = tokio::task::spawn_blocking(move || {
        // 관문(도착지 dirty·브랜치 다름·되감김)에 막혀도 이사를 세우지 않는다 —
        // Deposit 은 짐을 ref(refs/kasaterm/incoming)로만 보관하고 워킹트리는
        // 무접촉이라, 관문이 지키려는 것을 안 건드리고 잃는 것도 없다
        // (2026-08-30: 「푸시 순서」 수작업을 없앤 자리).
        crate::reposync::apply(
            std::path::Path::new(&path),
            &body,
            &head,
            &sync,
            &branch,
            dirty,
            force,
            crate::reposync::OnBlock::Deposit,
        )
    })
    .await;
    match applied {
        Ok(Ok(msg)) => Json(serde_json::json!({ "ok": true, "applied": msg })),
        Ok(Err(e)) => err(format!("{e:#}")),
        Err(e) => err(format!("적용 작업 실패: {e}")),
    }
}

/// 이사(migrate)의 대화 수신 창구 — 로컬 GUI 가 claude jsonl 을 올려 두면, 곧이어
/// 이 호스트에 스폰될 셸의 `claude --resume` 이 그것을 읽는다. 인증은 라우트 공통
/// 레이어(원격=remote 토큰, 로컬 브라우저=Origin)가 이미 덮는다.
async fn term_transcript_post(
    q: Query<std::collections::HashMap<String, String>>,
    body: axum::body::Bytes,
) -> impl IntoResponse {
    let err = |msg: String| Json(serde_json::json!({ "ok": false, "error": msg }));
    let Some(cwd) = q.get("cwd").filter(|s| s.starts_with('/')) else {
        return err("`cwd`(이 기계 기준 절대경로) 가 필요해요".into());
    };
    let Some(sid) = q.get("sid").filter(|s| valid_session_id(s)) else {
        return err("`sid`(claude 세션 uuid) 가 필요해요".into());
    };
    let force = q.get("force").map(String::as_str) == Some("1");
    let Some(path) =
        kasa_socket::sessions::session_jsonl_path(std::path::Path::new(cwd.as_str()), sid)
    else {
        return err("서버 HOME 을 몰라 저장 위치를 못 정해요".into());
    };
    // 이미 있는 파일이 더 크면 받은 쪽이 낡았을 공산이 크다 — 대화를 되감는
    // 덮어쓰기는 기본 거부하고, 알고 하는 재이사만 force 로 통과시킨다.
    if !force {
        if let Ok(meta) = std::fs::metadata(&path) {
            if meta.len() > body.len() as u64 {
                return err(format!(
                    "이 호스트에 더 큰 대화가 이미 있어요({}B > {}B) — force 로만 덮어쓸 수 있어요",
                    meta.len(),
                    body.len()
                ));
            }
        }
    }
    if let Some(dir) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(dir) {
            return err(format!("대화 폴더 생성 실패: {e}"));
        }
    }
    // 쓰다 만 파일이 정본 자리에 남지 않게 옆에 쓰고 rename 으로 앉힌다.
    let tmp = path.with_extension("jsonl.part");
    if let Err(e) = std::fs::write(&tmp, &body).and_then(|_| std::fs::rename(&tmp, &path)) {
        let _ = std::fs::remove_file(&tmp);
        return err(format!("대화 저장 실패: {e}"));
    }
    Json(serde_json::json!({
        "ok": true,
        "bytes": body.len(),
        "path": path.display().to_string(),
    }))
}

/// `GET /term/codex-session?sid=<uuid>` — 이 기계 Codex home(`~/.codex`)의 그
/// 대화(rollout)를 통짜 바이트로 준다. 대화 창구(`/term/transcript`)의 codex 판 —
/// pane 별 CODEX_HOME 은 sessions 를 실홈으로 심링크하므로 rollout 의 정본 자리는
/// 언제나 실홈이다. 도착지가 같은 자리에 앉힐 수 있게 상대경로를
/// `x-kasa-codex-rel` 헤더에 싣는다(경로 성분이 전부 ASCII 라 헤더에 안전).
async fn term_codex_session_get(
    q: Query<std::collections::HashMap<String, String>>,
) -> axum::response::Response {
    use axum::response::IntoResponse as _;
    let Some(sid) = q.get("sid").filter(|s| valid_session_id(s)).cloned() else {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            "`sid`(Codex 세션 uuid) 가 필요해요".to_string(),
        )
            .into_response();
    };
    let Some(home) = kasa_socket::home_dir().map(|h| h.join(".codex")) else {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            "서버 HOME 을 몰라 Codex home 을 못 정해요".to_string(),
        )
            .into_response();
    };
    // rollout 은 512MB 까지 허용이라 파일 IO 가 초 단위일 수 있다.
    let got = tokio::task::spawn_blocking(move || {
        kasa_socket::sessions::codex_sessions::bundle_codex_session_by_id(&home, &sid)
    })
    .await;
    match got {
        Ok(Ok(Some(mut bundle))) => {
            // v1 은 rollout 한 파일 — validate_bundle 이 강제한다.
            let file = bundle.files.pop().expect("v1 bundle은 파일 1개");
            let rel = file.codex_home_relative_path.to_string_lossy().into_owned();
            (
                axum::http::StatusCode::OK,
                [
                    (header::CONTENT_TYPE, "application/octet-stream".to_string()),
                    (
                        axum::http::HeaderName::from_static("x-kasa-codex-rel"),
                        rel,
                    ),
                ],
                file.bytes,
            )
                .into_response()
        }
        Ok(Ok(None)) => (
            axum::http::StatusCode::NOT_FOUND,
            "이 기계에 그 Codex 대화가 없어요".to_string(),
        )
            .into_response(),
        Ok(Err(e)) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            format!("Codex 대화 묶기 실패: {e:#}"),
        )
            .into_response(),
        Err(e) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            format!("작업 스레드 실패: {e}"),
        )
            .into_response(),
    }
}

/// `POST /term/codex-session?sid=<uuid>&rel=<sessions/…/rollout-….jsonl>`
/// body=rollout 통짜 바이트 — 받은 대화를 이 기계 Codex home 의 같은 자리에
/// 앉힌다. 검증·충돌 정책은 `codexhome::install_codex_rollout` 하나에 있다
/// (역이사가 로컬에 앉힐 때도 같은 함수를 쓴다 — 창구마다 정책이 갈리지 않게).
async fn term_codex_session_post(
    q: Query<std::collections::HashMap<String, String>>,
    body: axum::body::Bytes,
) -> impl IntoResponse {
    let err = |msg: String| Json(serde_json::json!({ "ok": false, "error": msg }));
    let Some(sid) = q.get("sid").filter(|s| valid_session_id(s)).cloned() else {
        return err("`sid`(Codex 세션 uuid) 가 필요해요".into());
    };
    let Some(rel) = q.get("rel").filter(|r| !r.is_empty()).cloned() else {
        return err("`rel`(Codex home 기준 상대경로) 가 필요해요".into());
    };
    let Some(home) = kasa_socket::home_dir().map(|h| h.join(".codex")) else {
        return err("서버 HOME 을 몰라 Codex home 을 못 정해요".into());
    };
    let out = tokio::task::spawn_blocking(move || {
        crate::codexhome::install_codex_rollout(
            &home,
            &sid,
            std::path::Path::new(&rel),
            &body,
        )
    })
    .await;
    match out {
        Ok(Ok(note)) => Json(serde_json::json!({ "ok": true, "note": note })),
        Ok(Err(e)) => err(format!("{e:#}")),
        Err(e) => err(format!("작업 스레드 실패: {e}")),
    }
}

/// 이 모듈 창구의 라우트. `router` 가 한 표로 합친 뒤 공통 레이어(Origin·토큰 가드)를 두른다.
pub(super) fn routes(backend: &Arc<dyn Backend>) -> axum::Router {
    let migrate_backend = backend.clone();
    let transfer_snapshot_backend = backend.clone();
    let transfer_spawn_backend = backend.clone();
    let transfer_close_backend = backend.clone();
    let transfer_migrate_backend = backend.clone();
    axum::Router::new()
        .route(
            "/pane-migrate",
            post(move |body: axum::body::Bytes| {
                pane_migrate_handler(migrate_backend.clone(), body)
            }),
        )
        .route("/term/repo", get(term_repo_get).post(term_repo_post))
        .route(
            "/term/transcript",
            get(term_transcript_get).post(
                term_transcript_post,
            )
                // 대화 jsonl 은 수백 MB 도 나온다 — axum 기본 2MB 로는
                // 이사 자체가 이유 없는 실패로만 보인다.
                .layer(axum::extract::DefaultBodyLimit::max(TRANSCRIPT_UPLOAD_LIMIT)),
        )
        .route(
            "/term/codex-session",
            get(term_codex_session_get).post(term_codex_session_post)
                // rollout 상한(512MiB)은 대화 jsonl 과 같은 급이다.
                .layer(axum::extract::DefaultBodyLimit::max(TRANSCRIPT_UPLOAD_LIMIT)),
        )
        .route(
            "/term/repo-sync",
            get(term_repo_sync_get).post(term_repo_sync_post)
                // bundle 은 미push 커밋+미커밋 변경 통짜다 — 대화 jsonl 과
                // 같은 급이라 같은 상한을 쓴다.
                .layer(axum::extract::DefaultBodyLimit::max(TRANSCRIPT_UPLOAD_LIMIT)),
        )
        .route("/transfer/snapshot", get(move || transfer_snapshot_handler(transfer_snapshot_backend.clone())))
        .route("/transfer/spawn", post(move |body: Json<kasa_socket::transfer::SpawnRequest>| transfer_spawn_handler(transfer_spawn_backend.clone(), body)))
        .route("/transfer/close", post(move |body: Json<kasa_socket::transfer::SessionIdentity>| transfer_close_handler(transfer_close_backend.clone(), body)))
        .route("/transfer/migrate", post(move |body: Json<kasa_socket::transfer::MigrateRequest>| transfer_migrate_handler(transfer_migrate_backend.clone(), body)))
}

#[cfg(test)]
mod tests {
    #[test]
    fn valid_session_id_rejects_path_material() {
        assert!(super::valid_session_id("deffe742-3b0d-40de-a135-ff8d7a207995"));
        // 경로 탈출 재료는 전부 거부 — 이 검사가 곧 저장 경로 방어다.
        for bad in ["", "../../etc/passwd", "a/b", "a.b", "a b", &"x".repeat(81)] {
            assert!(!super::valid_session_id(bad), "{bad:?} 가 통과했다");
        }
    }
}
