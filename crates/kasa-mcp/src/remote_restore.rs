//! Read-only, fail-closed identity resolution before restoring a remote mirror.
//! Pane numbers are reusable addresses, not conversation identities.

use futures_util::{stream, StreamExt, TryStreamExt};
use serde_json::Value;
use std::time::Duration;

#[derive(Clone, Debug, Default)]
pub struct RestoreIdentity {
    pub surface_key: Option<String>,
    pub session_id: Option<String>,
    pub cwd: Option<String>,
    pub character: Option<String>,
}

#[derive(Debug)]
struct Candidate {
    id: String,
    identity: RestoreIdentity,
}

fn present(value: Option<&str>) -> Option<&str> {
    value.filter(|s| !s.trim().is_empty())
}

fn field(row: &Value, key: &str) -> Option<String> {
    present(row.get(key).and_then(Value::as_str)).map(str::to_owned)
}

fn candidates(value: Value) -> Result<Vec<Candidate>, String> {
    let rows = value.as_array().ok_or("원본 기기의 창 목록 형식이 올바르지 않아")?;
    let mut result = Vec::new();
    let mut ids = std::collections::HashSet::new();
    for row in rows {
        if row.get("detached").and_then(Value::as_bool) == Some(true)
            || row.get("closed").and_then(Value::as_bool) == Some(true)
            || row.get("mirror_of").is_some_and(|v| !v.is_null())
        {
            continue;
        }
        let id = field(row, "id").ok_or("원본 기기의 창 번호를 확인할 수 없어")?;
        if !ids.insert(id.clone()) {
            return Err("원본 기기의 창 번호가 중복되어 복원을 중단했어".into());
        }
        result.push(Candidate {
            id,
            identity: RestoreIdentity {
                surface_key: field(row, "surface_key"),
                session_id: field(row, "session_id"),
                cwd: field(row, "cwd"),
                // `session` is a human title, never the session UUID.
                character: field(row, "name").or_else(|| field(row, "character")),
            },
        });
    }
    Ok(result)
}

fn unique<'a>(mut matches: impl Iterator<Item = &'a Candidate>) -> Result<String, String> {
    let first = matches.next().ok_or("저장된 창과 같은 원본 창을 찾지 못했어")?;
    if matches.next().is_some() {
        return Err("저장된 창과 일치하는 원본 창이 여러 개라 복원을 중단했어".into());
    }
    Ok(first.id.clone())
}

fn select(rows: &[Candidate], saved_id: &str, hint: &RestoreIdentity) -> Result<String, String> {
    if let Some(key) = present(hint.surface_key.as_deref()) {
        // Surface identity outlives the agent session running inside it.
        // Missing/ambiguous keys must never fall through to conversation hints.
        return unique(rows.iter().filter(|p| p.identity.surface_key.as_deref() == Some(key)));
    }
    if let Some(sid) = present(hint.session_id.as_deref()) {
        // Never fall back to a pane number, cwd, or character after a UUID miss.
        return unique(rows.iter().filter(|p| p.identity.session_id.as_deref() == Some(sid)));
    }
    let cwd = present(hint.cwd.as_deref());
    let character = present(hint.character.as_deref());
    let matches = |p: &&Candidate| {
        cwd.is_none_or(|s| p.identity.cwd.as_deref() == Some(s))
            && character.is_none_or(|s| p.identity.character.as_deref() == Some(s))
    };
    if let Some(same) = rows.iter().find(|p| p.id == saved_id).filter(matches) {
        return Ok(same.id.clone());
    }
    if cwd.is_some() && character.is_some() {
        return unique(rows.iter().filter(matches));
    }
    Err("저장된 창 번호의 대화를 확인할 정보가 부족하거나 일치하지 않아".into())
}

async fn get(
    client: &reqwest::Client,
    url: reqwest::Url,
    token: Option<&str>,
    max_bytes: usize,
) -> Result<Vec<u8>, String> {
    let mut request = client.get(url).timeout(Duration::from_secs(8));
    if let Some(token) = token {
        request = request.header("x-kasa-token", token);
    }
    let mut response = request.send().await.map_err(|_| "원본 기기에 연결하지 못했어")?
        .error_for_status().map_err(|_| "원본 기기가 대화 확인 요청을 거부했어")?;
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| "원본 기기의 응답을 읽지 못했어")? {
        if bytes.len().saturating_add(chunk.len()) > max_bytes {
            return Err("원본 기기의 대화 확인 응답이 너무 커".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

/// Resolve a saved address only when its current owner can be verified.
/// Makes GET requests only; never creates, resumes, closes, or writes to a pane.
pub async fn resolve(
    client: &reqwest::Client,
    base: &str,
    token: Option<&str>,
    saved_id: &str,
    hint: &RestoreIdentity,
) -> Result<String, String> {
    tokio::time::timeout(Duration::from_secs(20), async {
        let endpoint = |path: &str| {
            reqwest::Url::parse(&format!("{}{path}", base.trim_end_matches('/')))
                .map_err(|_| "원본 기기의 접속 주소가 올바르지 않아".to_string())
        };
        let bytes = get(client, endpoint("/term/panes")?, token, 4 << 20).await?;
        let value = serde_json::from_slice(&bytes).map_err(|_| "원본 기기의 창 목록을 읽지 못했어")?;
        let mut rows = candidates(value)?;
        if present(hint.surface_key.as_deref()).is_none()
            && present(hint.session_id.as_deref()).is_some() {
            // All unresolved candidates must be checked to prove uniqueness.
            // Any failed lookup leaves that proof incomplete, so fail closed.
            let session_url = endpoint("/pane-session")?;
            rows = stream::iter(rows.into_iter().map(|mut row| {
                let mut url = session_url.clone();
                url.query_pairs_mut().append_pair("pane", &row.id);
                async move {
                    if row.identity.session_id.is_none() {
                        let bytes = get(client, url, token, 4096).await?;
                        let sid = std::str::from_utf8(&bytes).map_err(|_| "원본 대화 식별자를 읽지 못했어")?.trim();
                        row.identity.session_id = present(Some(sid)).map(str::to_owned);
                    }
                    Ok::<_, String>(row)
                }
            }))
            .buffer_unordered(8)
            .try_collect()
            .await?;
        }
        select(&rows, saved_id, hint)
    })
    .await
    .map_err(|_| "원본 대화 확인 시간이 초과되어 복원을 중단했어".to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn hint(sid: Option<&str>, cwd: Option<&str>, name: Option<&str>) -> RestoreIdentity {
        RestoreIdentity { surface_key: None, session_id: sid.map(str::to_owned), cwd: cwd.map(str::to_owned), character: name.map(str::to_owned) }
    }

    #[test]
    fn surface_key_wins_over_replaced_session_and_never_falls_back() {
        let rows = candidates(json!([
            {"id":"%1","surface_key":"new-key","session_id":"old-session"},
            {"id":"%3","surface_key":"legacy:%4","session_id":"new-session"}
        ])).unwrap();
        let mut identity = hint(Some("old-session"), None, None);
        identity.surface_key = Some("legacy:%4".into());
        assert_eq!(select(&rows, "%1", &identity).unwrap(), "%3");
        identity.surface_key = Some("missing-key".into());
        assert!(select(&rows, "%1", &identity).is_err());
        let duplicate = candidates(json!([
            {"id":"%1","surface_key":"legacy:%4"}, {"id":"%3","surface_key":"legacy:%4"}
        ])).unwrap();
        identity.surface_key = Some("legacy:%4".into());
        assert!(select(&duplicate, "%1", &identity).is_err());
    }

    #[test]
    fn uuid_follows_conversation_not_reused_number() {
        let rows = candidates(json!([
            {"id":"%1","session_id":"other","cwd":"/repo","name":"Azusa"},
            {"id":"%8","session_id":"saved","cwd":"/else","name":"Momoi"}
        ])).unwrap();
        assert_eq!(select(&rows, "%1", &hint(Some("saved"), Some("/repo"), Some("Azusa"))).unwrap(), "%8");
        assert!(select(&rows, "%1", &hint(Some("gone"), Some("/repo"), Some("Azusa"))).is_err());
    }

    #[test]
    fn duplicate_session_is_ambiguous_even_at_saved_number() {
        let rows = candidates(json!([
            {"id":"%1","session_id":"saved"}, {"id":"%8","session_id":"saved"}
        ])).unwrap();
        assert!(select(&rows, "%1", &hint(Some("saved"), None, None)).is_err());
    }

    #[test]
    fn legacy_same_number_checks_every_available_hint() {
        let rows = candidates(json!([{"id":"%1","cwd":"/repo","name":"Azusa"}])).unwrap();
        for identity in [hint(None, Some("/repo"), None), hint(None, None, Some("Azusa")), hint(None, None, None)] {
            assert_eq!(select(&rows, "%1", &identity).unwrap(), "%1");
        }
        for identity in [hint(None, Some("/else"), None), hint(None, None, Some("Momoi")), hint(None, Some("/repo"), Some("Momoi"))] {
            assert!(select(&rows, "%1", &identity).is_err());
        }
        assert!(select(&rows, "%8", &RestoreIdentity::default()).is_err());
    }

    #[test]
    fn legacy_relocation_requires_both_hints_and_unique_match() {
        let rows = candidates(json!([{"id":"%8","cwd":"/repo","name":"Azusa"}])).unwrap();
        let identity = hint(None, Some("/repo"), Some("Azusa"));
        assert_eq!(select(&rows, "%1", &identity).unwrap(), "%8");
        assert!(select(&rows, "%1", &hint(None, Some("/repo"), None)).is_err());
        assert!(select(&rows, "%1", &hint(None, None, Some("Azusa"))).is_err());
        let duplicated = candidates(json!([
            {"id":"%8","cwd":"/repo","name":"Azusa"}, {"id":"%9","cwd":"/repo","name":"Azusa"}
        ])).unwrap();
        assert!(select(&duplicated, "%1", &identity).is_err());
    }

    #[test]
    fn excludes_closed_detached_and_mirrors_and_rejects_malformed_ids() {
        let rows = candidates(json!([
            {"id":"%1","closed":true}, {"id":"%2","detached":true},
            {"id":"%3","mirror_of":"Mini"}, {"id":"%4","mirror_of":null}
        ])).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "%4");
        assert!(candidates(json!([{"id":"%1"},{"id":"%1"}])).is_err());
        assert!(candidates(json!([{}])).is_err());
        assert!(candidates(json!({})).is_err());
    }

    #[tokio::test]
    async fn http_resolves_uuid_with_encoded_pane_and_auth_using_get_only() {
        use axum::{extract::Query, http::{HeaderMap, StatusCode}, routing::get, Router};
        let app = Router::new()
            .route("/term/panes", get(|| async { axum::Json(json!([
                {"id":"%1","surface_key":"other-key"},
                {"id":"%8","surface_key":"legacy:%4"}, {"id":"%9","closed":true}
            ])) }))
            .route("/pane-session", get(|headers: HeaderMap, Query(query): Query<std::collections::HashMap<String, String>>| async move {
                if headers.get("x-kasa-token").and_then(|v| v.to_str().ok()) != Some("test-only") {
                    return (StatusCode::UNAUTHORIZED, "");
                }
                match query.get("pane").map(String::as_str) {
                    Some("%1") => (StatusCode::OK, "other"),
                    Some("%8") => (StatusCode::OK, "saved\n"),
                    _ => (StatusCode::INTERNAL_SERVER_ERROR, ""),
                }
            }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = reqwest::Client::new();
        let identity = hint(Some("saved"), None, None);
        assert_eq!(resolve(&client, &base, Some("test-only"), "%1", &identity).await.unwrap(), "%8");
        // An unreadable candidate cannot be ignored even if another might match.
        assert!(resolve(&client, &base, None, "%1", &identity).await.is_err());
        let mut keyed = hint(Some("replaced-session"), None, None);
        keyed.surface_key = Some("legacy:%4".into());
        // Without auth, /pane-session rejects us. A stable surface key must
        // neither query that endpoint nor care about the agent session change.
        assert_eq!(resolve(&client, &base, None, "%1", &keyed).await.unwrap(), "%8");
        server.abort();
    }
}
