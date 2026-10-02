use super::*;
use axum::extract::{Path as AxPath, Query, State};
use axum::routing::{delete, get, post};

/// What the fake providers saw: (method path, body or query).
pub(crate) type Seen = Arc<Mutex<Vec<(String, String)>>>;

pub(crate) fn seen(calls: &Seen, what: &str) -> Vec<String> {
    calls
        .lock()
        .unwrap()
        .iter()
        .filter(|(path, _)| path == what)
        .map(|(_, body)| body.clone())
        .collect()
}

fn note(calls: &Seen, path: &str, body: String) {
    calls.lock().unwrap().push((path.into(), body));
}

pub(crate) async fn providers() -> (String, Seen) {
    let calls: Seen = Arc::default();
    let app = axum::Router::new()
        .route(
            "/google/token",
            post(|State(calls): State<Seen>, body: String| async move {
                note(&calls, "google/token", body.clone());
                if body.contains("refresh_token=dead") {
                    return (axum::http::StatusCode::BAD_REQUEST, axum::Json(json!({"error":"invalid_grant"})));
                }
                (axum::http::StatusCode::OK, axum::Json(json!({"access_token":"g-access-2","expires_in":3600,"token_type":"Bearer"})))
            }),
        )
        .route(
            "/google/revoke",
            post(|State(calls): State<Seen>, body: String| async move {
                note(&calls, "google/revoke", body);
                axum::http::StatusCode::OK
            }),
        )
        .route(
            "/gmail/messages",
            get(|State(calls): State<Seen>, Query(query): Query<HashMap<String, String>>| async move {
                note(&calls, "gmail/list", format!("{query:?}"));
                axum::Json(json!({"messages":[{"id":"18a1"},{"id":"18a2"}]}))
            }),
        )
        .route(
            "/gmail/messages/{id}",
            get(|AxPath(id): AxPath<String>, Query(query): Query<HashMap<String, String>>| async move {
                let text = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode("안녕하세요\n본문".as_bytes());
                let mut message = json!({"id":id,"threadId":"thread-1","snippet":"안녕하세요","labelIds":["INBOX","UNREAD"],
                    "payload":{"mimeType":"multipart/alternative","headers":[
                        {"name":"From","value":"Sender <sender@example.com>"},{"name":"Subject","value":"회의"},
                        {"name":"Message-ID","value":"<orig@example.com>"},{"name":"Date","value":"Thu, 2 Oct 2026 10:00:00 +0900"}],
                    "parts":[{"mimeType":"text/plain","body":{"data":text}},{"mimeType":"application/pdf","filename":"안건.pdf","body":{}}]}});
                if query.get("format").map(String::as_str) == Some("metadata") {
                    message["payload"]["parts"] = json!([]);
                }
                axum::Json(message)
            }),
        )
        .route(
            "/gmail/messages/send",
            post(|State(calls): State<Seen>, body: String| async move {
                note(&calls, "gmail/send", body);
                axum::Json(json!({"id":"sent-1","threadId":"thread-1"}))
            }),
        )
        .route(
            "/github/token",
            post(|State(calls): State<Seen>, body: String| async move {
                note(&calls, "github/token", body);
                axum::Json(json!({"access_token":"gh-access-2","expires_in":28800,"refresh_token":"gh-refresh-2",
                    "refresh_token_expires_in":15897600,"token_type":"bearer"}))
            }),
        )
        .route(
            "/github/repos/{owner}/{name}/pulls",
            post(|State(calls): State<Seen>, AxPath((owner, name)): AxPath<(String, String)>, headers: axum::http::HeaderMap, body: String| async move {
                note(&calls, "github/pulls", format!("{owner}/{name} {} {body}", headers["authorization"].to_str().unwrap()));
                match name.as_str() {
                    "closed" => (axum::http::StatusCode::NOT_FOUND, axum::Json(json!({"message":"Not Found"}))),
                    "exists" => (axum::http::StatusCode::UNPROCESSABLE_ENTITY,
                        axum::Json(json!({"message":"Validation Failed","errors":[{"message":"A pull request already exists"}]}))),
                    _ => (axum::http::StatusCode::CREATED, axum::Json(json!({"number":7,"html_url":format!("https://github.com/{owner}/{name}/pull/7")}))),
                }
            }),
        )
        .route(
            "/github/applications/{id}/grant",
            delete(|State(calls): State<Seen>, AxPath(id): AxPath<String>, body: String| async move {
                note(&calls, "github/revoke", format!("{id} {body}"));
                axum::http::StatusCode::NO_CONTENT
            }),
        )
        .with_state(calls.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (base, calls)
}

pub(crate) fn endpoints(base: &str) -> Endpoints {
    Endpoints {
        google_token: format!("{base}/google/token"),
        google_revoke: format!("{base}/google/revoke"),
        gmail: format!("{base}/gmail"),
        github_token: format!("{base}/github/token"),
        github_api: format!("{base}/github"),
    }
}

async fn service() -> (Arc<Service>, Seen, PathBuf) {
    let (oauth, dir) = crate::oauth_accounts::tests::fixture();
    std::fs::create_dir_all(&dir).unwrap();
    let mut service = Service::open(Some(&dir.join("state.json")), oauth.config.clone()).unwrap();
    let (base, calls) = providers().await;
    Arc::get_mut(&mut service).unwrap().endpoints = endpoints(&base);
    (service, calls, dir)
}

fn google(scopes: &[&str], access_expires: u64, refresh: &str) -> Grant {
    Grant {
        provider: Provider::Google,
        subject: "google-sub".into(),
        display: "me@example.com".into(),
        access_token: "g-access-1".into(),
        access_expires,
        refresh_token: Some(refresh.into()),
        refresh_expires: 0,
        scopes: scopes.iter().map(|s| s.to_string()).collect(),
    }
}

fn github(access_expires: u64) -> Grant {
    Grant {
        provider: Provider::Github,
        subject: "4242".into(),
        display: "octo".into(),
        access_token: "gh-access-1".into(),
        access_expires,
        refresh_token: Some("gh-refresh-1".into()),
        refresh_expires: 0,
        scopes: Vec::new(),
    }
}

const ME: Caller<'static> = Caller {
    account: "one",
    device: "dev_one",
    label: "Laptop",
};

fn now() -> u64 {
    crate::relay_auth::now_secs()
}

fn mail() -> Write {
    Write::Mail(MailDraft {
        to: vec!["a@example.com".into()],
        cc: vec![],
        subject: "주간 보고".into(),
        body: "비밀 본문 텍스트".into(),
        reply_to: Some("18a1".into()),
    })
}

#[tokio::test]
async fn connect_keeps_only_granted_scopes_and_reconnect_replaces_tokens() {
    let (service, _, dir) = service().await;
    let both = [Feature::MailRead, Feature::MailSend];
    let first = service
        .store(&ME, google(&[GMAIL_READ, "openid"], now() + 3600, "r1"), &both)
        .await
        .unwrap();
    assert_eq!(first["features"], json!(["mail.read"]), "an unchecked scope became a feature");
    let again = service
        .store(&ME, google(&[GMAIL_READ, GMAIL_SEND], now() + 3600, "r2"), &both)
        .await
        .unwrap();
    assert_eq!(again["id"], first["id"]);
    let listed = service.list("one").await.unwrap();
    assert_eq!(listed["connections"].as_array().unwrap().len(), 1);
    assert_eq!(listed["connections"][0]["features"], json!(["mail.read", "mail.send"]));
    assert!(listed.to_string().find("g-access").is_none(), "a token reached the list");
    // Sealed on disk: neither token appears in clear text.
    let sealed = std::fs::read(dir.join("connections/one.sealed")).unwrap();
    assert!(!String::from_utf8_lossy(&sealed).contains("g-access-1"));
    assert!(service.list("two").await.unwrap()["connections"].as_array().unwrap().is_empty());
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn reading_mail_refreshes_an_expiring_token_once() {
    let (service, calls, dir) = service().await;
    service
        .store(&ME, google(&[GMAIL_READ], now() + 5, "r1"), &[Feature::MailRead])
        .await
        .unwrap();
    let listed = service.mail_list(&ME, None, "is:unread", 5).await.unwrap();
    assert_eq!(listed["messages"].as_array().unwrap().len(), 2);
    assert_eq!(listed["messages"][0]["subject"], "회의");
    assert_eq!(listed["messages"][0]["unread"], true);
    assert!(seen(&calls, "gmail/list")[0].contains("is:unread"));
    let read = service.mail_read(&ME, None, "18a1").await.unwrap();
    assert_eq!(read["text"], "안녕하세요\n본문");
    assert_eq!(read["attachments"], json!(["안건.pdf"]));
    assert_eq!(seen(&calls, "google/token").len(), 1, "the refreshed token was not kept");
    assert!(seen(&calls, "google/token")[0].contains("grant_type=refresh_token"));
    assert_eq!(service.mail_read(&ME, None, "../x").await.unwrap_err(), Error::Invalid);
    assert_eq!(
        service.mail_list(&ME, None, "", 5).await.is_ok(),
        true
    );
    // Sending needs its own permission.
    assert_eq!(service.request(&ME, None, mail()).await.unwrap_err(), Error::FeatureMissing);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn writes_wait_for_an_app_approval_of_the_exact_content() {
    let (service, calls, dir) = service().await;
    service
        .store(&ME, google(&[GMAIL_READ, GMAIL_SEND], now() + 3600, "r1"), &[Feature::MailRead, Feature::MailSend])
        .await
        .unwrap();
    let queued = service.request(&ME, None, mail()).await.unwrap();
    assert_eq!(queued["status"], "pending");
    assert!(seen(&calls, "gmail/send").is_empty(), "a request sent mail without approval");
    let id = queued["pending"]["id"].as_str().unwrap().to_string();
    let digest = queued["pending"]["digest"].as_str().unwrap().to_string();
    let key = crate::oauth_accounts::secret();

    assert_eq!(service.approve(&ME, &key, &id, &digest).await.unwrap_err(), Error::ApproverRequired);
    service.register_approver("dev_one", &key).unwrap();
    let other = Caller { device: "dev_cli", ..ME };
    assert_eq!(service.approve(&other, &key, &id, &digest).await.unwrap_err(), Error::ApproverRequired);
    assert_eq!(service.approve(&ME, &key, &id, "0000").await.unwrap_err(), Error::DigestMismatch);
    let sent = service.approve(&ME, &key, &id, &digest).await.unwrap();
    assert_eq!(sent["status"], "sent");
    assert_eq!(service.approve(&ME, &key, &id, &digest).await.unwrap_err(), Error::NotFound);

    let body: Value = serde_json::from_str(&seen(&calls, "gmail/send")[0]).unwrap();
    assert_eq!(body["threadId"], "thread-1");
    let raw = String::from_utf8(
        base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(body["raw"].as_str().unwrap())
            .unwrap(),
    )
    .unwrap();
    assert!(raw.contains("To: a@example.com\r\n"));
    assert!(raw.contains("From: me@example.com\r\n"));
    assert!(raw.contains("In-Reply-To: <orig@example.com>\r\n"));
    assert!(raw.contains(&format!("Subject: =?UTF-8?B?{}?=", base64::engine::general_purpose::STANDARD.encode("주간 보고"))));

    let audit = service.audit("one", 10);
    let text = serde_json::to_string(&audit).unwrap();
    assert!(audit.iter().any(|line| line["action"] == "mail.send" && line["result"] == "ok" && line["via"] == "approver"));
    for secret in ["비밀 본문", "주간 보고", "a@example.com", "g-access"] {
        assert!(!text.contains(secret), "audit kept {secret}");
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn pull_requests_refresh_rotating_tokens_and_keep_unsent_refusals() {
    let (service, calls, dir) = service().await;
    service.store(&ME, github(now() + 5), &[Feature::GithubPr]).await.unwrap();
    let key = crate::oauth_accounts::secret();
    service.register_approver("dev_one", &key).unwrap();
    let pr = |repo: &str| {
        Write::Pr(PrDraft {
            repo: repo.into(),
            base: "main".into(),
            head: "feature/login".into(),
            title: "로그인 정리".into(),
            body: "본문".into(),
            draft: true,
        })
    };
    let queued = service.request(&ME, None, pr("2rami/kasaterm")).await.unwrap();
    let (id, digest) = (queued["pending"]["id"].as_str().unwrap(), queued["pending"]["digest"].as_str().unwrap());
    let created = service.approve(&ME, &key, id, digest).await.unwrap();
    assert_eq!(created["number"], 7);
    assert_eq!(created["url"], "https://github.com/2rami/kasaterm/pull/7");
    assert!(seen(&calls, "github/pulls")[0].contains("Bearer gh-access-2"), "used the expired token");
    assert!(seen(&calls, "github/token")[0].contains("refresh_token=gh-refresh-1"));
    assert!(seen(&calls, "github/token")[0].contains("client_id=app-id"), "refreshed with the sign-in app");

    // Not installed on that repository: nothing was created, so the person can approve it later.
    let queued = service.request(&ME, None, pr("2rami/closed")).await.unwrap();
    let (id, digest) = (queued["pending"]["id"].as_str().unwrap(), queued["pending"]["digest"].as_str().unwrap());
    assert_eq!(service.approve(&ME, &key, id, digest).await.unwrap_err(), Error::NotAccessible);
    assert_eq!(service.list("one").await.unwrap()["pending"].as_array().unwrap().len(), 1);
    // The provider refused the content itself: its reason comes back.
    let queued = service.request(&ME, None, pr("2rami/exists")).await.unwrap();
    let (id, digest) = (queued["pending"]["id"].as_str().unwrap(), queued["pending"]["digest"].as_str().unwrap());
    match service.approve(&ME, &key, id, digest).await.unwrap_err() {
        Error::Rejected(reason) => assert!(reason.contains("already exists")),
        other => panic!("{other:?}"),
    }
    assert_eq!(seen(&calls, "github/token").len(), 1, "a rotated refresh token was reused");
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn a_refused_refresh_asks_to_reconnect_and_disconnect_revokes() {
    let (service, calls, dir) = service().await;
    let connected = service
        .store(&ME, google(&[GMAIL_READ], now() - 10, "dead"), &[Feature::MailRead])
        .await
        .unwrap();
    assert_eq!(service.mail_list(&ME, None, "", 5).await.unwrap_err(), Error::Reconnect);
    assert_eq!(service.list("one").await.unwrap()["connections"][0]["state"], "reconnect_required");
    assert_eq!(service.mail_list(&ME, None, "", 5).await.unwrap_err(), Error::Reconnect);
    assert_eq!(seen(&calls, "google/token").len(), 1, "kept knocking with a dead refresh token");

    service.store(&ME, github(0), &[Feature::GithubPr]).await.unwrap();
    let pending = service
        .request(&ME, None, Write::Pr(PrDraft { repo: "a/b".into(), base: "main".into(), head: "x".into(), title: "t".into(), body: String::new(), draft: false }))
        .await
        .unwrap();
    let github_id = pending["pending"]["connection"].as_str().unwrap().to_string();
    let gone = service.disconnect(&ME, &github_id).await.unwrap();
    assert_eq!(gone["provider_revoked"], true);
    assert!(seen(&calls, "github/revoke")[0].starts_with("app-id "));
    assert!(service.list("one").await.unwrap()["pending"].as_array().unwrap().is_empty());
    service.disconnect(&ME, connected["id"].as_str().unwrap()).await.unwrap();
    assert!(service.list("one").await.unwrap()["connections"].as_array().unwrap().is_empty());
    assert_eq!(service.disconnect(&ME, "con_missing").await.unwrap_err(), Error::NotFound);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn several_connections_need_a_choice() {
    let (service, _, dir) = service().await;
    service.store(&ME, google(&[GMAIL_READ], 0, "r1"), &[Feature::MailRead]).await.unwrap();
    let mut work = google(&[GMAIL_READ], 0, "r2");
    work.subject = "google-work".into();
    let work = service.store(&ME, work, &[Feature::MailRead]).await.unwrap();
    assert_eq!(service.mail_list(&ME, None, "", 1).await.unwrap_err(), Error::ChooseConnection);
    assert!(service.mail_list(&ME, work["id"].as_str(), "", 1).await.is_ok());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn writes_are_checked_before_they_wait() {
    let header = Write::Mail(MailDraft {
        to: vec!["a@example.com\r\nBcc: x@evil.example".into()],
        cc: vec![],
        subject: "s".into(),
        body: String::new(),
        reply_to: None,
    });
    assert_eq!(header.validate(), Err(Error::Invalid));
    let subject = Write::Mail(MailDraft {
        to: vec!["a@example.com".into()],
        cc: vec![],
        subject: "s\r\nBcc: x@evil.example".into(),
        body: String::new(),
        reply_to: None,
    });
    assert_eq!(subject.validate(), Err(Error::Invalid));
    for (repo, head) in [("../etc", "x"), ("a/b/c", "x"), ("a/b", "--upload-pack"), ("a/b", "x y")] {
        let pr = Write::Pr(PrDraft { repo: repo.into(), base: "main".into(), head: head.into(), title: "t".into(), body: String::new(), draft: false });
        assert_eq!(pr.validate(), Err(Error::Invalid), "{repo} {head}");
    }
    assert!(Write::Pr(PrDraft { repo: "a/b".into(), base: "main".into(), head: "fork:feature/x".into(), title: "t".into(), body: String::new(), draft: false }).validate().is_ok());
    assert_ne!(mail().digest(), {
        let Write::Mail(mut changed) = mail() else { unreachable!() };
        changed.body.push('!');
        Write::Mail(changed).digest()
    });
}

#[test]
fn html_mail_reads_as_text() {
    assert_eq!(strip_html("<p>안녕&nbsp;<b>하세요</b></p><br>a &amp; b"), "\n안녕 하세요\n\na & b");
}
