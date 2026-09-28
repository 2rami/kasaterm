//! KASA-share 창구 셋 — 다른 기기가 부르는 목록·파일, 폰이 부르는 폴더 보기.
//! 모두 읽기 전용이고 공유 폴더 밖은 못 가리킨다. 원격 요청의 토큰 검사는
//! `origin_guard_mw` 가 이미 했고, 여기서는 주인 아닌 폰 주소를 막는다.

use super::path;
use axum::extract::Query;
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;
use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};

type Q = Query<HashMap<String, String>>;

/// 폰이 한 번에 받는 상한. 동기화는 조각으로 받으므로 이 상한과 무관하다.
const WHOLE_MAX: u64 = 32 << 20;
const PIECE_MAX: u64 = 8 << 20;

fn guard(req: &axum::extract::Request) -> Option<Response> {
    if req.extensions().get::<crate::http::MobileAuth>().is_some_and(|a| !a.0.owner) {
        return Some((StatusCode::FORBIDDEN, "owner only").into_response());
    }
    None
}

/// 요청에 실린 엔진이 먼저 — 한 프로세스에 두 기기를 띄우는 테스트가 쓴다.
fn pick(req: &axum::extract::Request) -> Option<std::sync::Arc<super::Engine>> {
    req.extensions().get::<std::sync::Arc<super::Engine>>().cloned().or_else(super::engine)
}

fn off() -> Response {
    (StatusCode::SERVICE_UNAVAILABLE, "KASA-share is not running").into_response()
}

pub(crate) async fn manifest(q: Q, req: axum::extract::Request) -> Response {
    if let Some(r) = guard(&req) {
        return r;
    }
    let Some(engine) = pick(&req) else { return off() };
    let since = q.get("since").and_then(|s| s.parse::<u64>().ok()).unwrap_or(0);
    let body = engine.with_index(|idx| {
        let full = q.get("epoch").map(String::as_str) != Some(idx.epoch.as_str());
        let entries: Vec<_> = idx.entries.values().filter(|e| full || e.seq > since).cloned().collect();
        json!({
            "ok": true,
            "machine_id": engine.me,
            "label": engine.label,
            "epoch": idx.epoch,
            "seq": idx.seq,
            "entries": entries,
        })
    });
    Json(body).into_response()
}

fn percent(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

pub(crate) async fn file(q: Q, req: axum::extract::Request) -> Response {
    let rel = q.get("path").cloned().unwrap_or_default();
    serve_file(rel, q, req).await
}

/// 경로형(`/term/share/f/<경로>`) — 폰이 여는 html 안의 상대 경로 자원(`style.css`,
/// `img/a.png`)이 같은 폴더로 풀리게. 쿼리형이면 `term/share/` 기준으로 풀려 깨진다.
pub(crate) async fn file_at(axum::extract::Path(rel): axum::extract::Path<String>, q: Q, req: axum::extract::Request) -> Response {
    serve_file(rel, q, req).await
}

async fn serve_file(rel: String, q: Q, req: axum::extract::Request) -> Response {
    if let Some(r) = guard(&req) {
        return r;
    }
    let Some(engine) = pick(&req) else { return off() };
    if !path::safe(&rel) {
        return (StatusCode::BAD_REQUEST, "bad path").into_response();
    }
    let key = path::key(&rel);
    let Some((entry, stat)) = engine.with_index(|i| {
        let e = i.entries.get(&key).filter(|e| !e.deleted)?.clone();
        Some((e, i.stats.get(&key)?.clone()))
    }) else {
        return (StatusCode::NOT_FOUND, "no such file").into_response();
    };
    if q.get("sha").is_some_and(|s| *s != entry.sha) {
        return (StatusCode::CONFLICT, "changed").into_response();
    }
    let offset = q.get("offset").and_then(|s| s.parse::<u64>().ok());
    let len = q.get("len").and_then(|s| s.parse::<u64>().ok());
    if offset.is_none() && stat.size > WHOLE_MAX {
        return (StatusCode::PAYLOAD_TOO_LARGE, "too large for one request").into_response();
    }
    let root = engine.root.clone();
    let disk = stat.disk.clone();
    let read = tokio::task::spawn_blocking(move || -> std::io::Result<Vec<u8>> {
        let full = super::join(&root, &disk);
        // 루트 안의 모든 조상이 진짜 폴더여야 한다 — 중간에 링크가 끼면 폴더 밖이다.
        let mut p = root.clone();
        for part in disk.split('/') {
            p = p.join(part);
            if std::fs::symlink_metadata(&p)?.file_type().is_symlink() {
                return Err(std::io::Error::other("link"));
            }
        }
        if !full.canonicalize()?.starts_with(root.canonicalize()?) {
            return Err(std::io::Error::other("outside"));
        }
        let mut f = std::fs::File::open(&full)?;
        let start = offset.unwrap_or(0);
        let want = len.unwrap_or(u64::MAX).min(if offset.is_some() { PIECE_MAX } else { WHOLE_MAX });
        f.seek(SeekFrom::Start(start))?;
        let mut buf = Vec::new();
        f.take(want).read_to_end(&mut buf)?;
        Ok(buf)
    })
    .await;
    let Ok(Ok(bytes)) = read else {
        return (StatusCode::NOT_FOUND, "unreadable").into_response();
    };
    let name = rel.rsplit('/').next().unwrap_or(&rel);
    (
        [
            (header::CONTENT_TYPE, path::content_type(&rel).to_string()),
            // 학생이 만든 html·svg 가 이 주소에서 스크립트를 돌리면 `/send` 로 셸에 칠 수 있다.
            (header::CONTENT_SECURITY_POLICY, "sandbox".to_string()),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_string()),
            (header::CONTENT_DISPOSITION, format!("inline; filename*=UTF-8''{}", percent(name))),
            (header::HeaderName::from_static("x-share-size"), entry.size.to_string()),
            (header::HeaderName::from_static("x-share-sha"), entry.sha.clone()),
        ],
        bytes,
    )
        .into_response()
}

/// 폰의 폴더 보기. `<날짜>-<주제>/` 폴더를 최신순으로, 폴더 밖 파일은 따로.
pub(crate) async fn list(req: axum::extract::Request) -> Response {
    if let Some(r) = guard(&req) {
        return r;
    }
    let Some(engine) = pick(&req) else { return off() };
    let body = engine.with_index(|idx| {
        let mut folders: std::collections::BTreeMap<String, (u64, Vec<serde_json::Value>)> = Default::default();
        let mut loose = Vec::new();
        for (_, e, _) in idx.live() {
            let (folder, name) = match e.path.split_once('/') {
                Some((f, rest)) => (Some(f.to_string()), rest.to_string()),
                None => (None, e.path.clone()),
            };
            let item = json!({
                "path": e.path,
                "name": name,
                "size": e.size,
                "modified_ms": e.mtime_ms,
                "kind": path::kind(&e.path),
                "origin": e.origin,
            });
            match folder {
                Some(f) => {
                    let slot = folders.entry(f).or_default();
                    slot.0 = slot.0.max(e.mtime_ms);
                    slot.1.push(item);
                }
                None => loose.push(item),
            }
        }
        let mut folders: Vec<_> = folders.into_iter().collect();
        folders.sort_by(|a, b| b.1 .0.cmp(&a.1 .0).then_with(|| b.0.cmp(&a.0)));
        let by_name = |a: &serde_json::Value, b: &serde_json::Value| a["name"].as_str().cmp(&b["name"].as_str());
        loose.sort_by(by_name);
        let folders: Vec<_> = folders
            .into_iter()
            .map(|(name, (modified, mut files))| {
                files.sort_by(by_name);
                json!({ "name": name, "modified_ms": modified, "files": files })
            })
            .collect();
        json!({ "ok": true, "name": super::FOLDER, "folders": folders, "files": loose })
    });
    Json(body).into_response()
}
