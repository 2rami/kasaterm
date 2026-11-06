use super::*;
use super::testing::{seen, service, github, github_as, ME, now, pr};

#[tokio::test]
async fn reconnecting_replaces_tokens_and_the_book_stays_sealed() {
    let (service, _, dir) = service().await;
    let first = service.store(&ME, github(now() + 3600), &[Feature::GithubPr]).await.unwrap();
    assert_eq!(first["features"], json!(["github.pr"]));
    let mut again = github(now() + 3600);
    again.access_token = "gh-access-9".into();
    let again = service.store(&ME, again, &[Feature::GithubPr]).await.unwrap();
    assert_eq!(again["id"], first["id"]);
    let listed = service.list("one").await.unwrap();
    assert_eq!(listed["connections"].as_array().unwrap().len(), 1);
    assert!(listed.to_string().find("gh-access").is_none(), "a token reached the list");
    // Sealed on disk: the token does not appear in clear text.
    let sealed = std::fs::read(dir.join("connections/one.sealed")).unwrap();
    assert!(!String::from_utf8_lossy(&sealed).contains("gh-access-9"));
    assert!(service.list("two").await.unwrap()["connections"].as_array().unwrap().is_empty());
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn writes_wait_for_an_app_approval_of_the_exact_content() {
    let (service, calls, dir) = service().await;
    service.store(&ME, github(0), &[Feature::GithubPr]).await.unwrap();
    let queued = service.request(&ME, None, pr("2rami/kasaterm")).await.unwrap();
    assert_eq!(queued["status"], "pending");
    assert!(seen(&calls, "github/pulls").is_empty(), "a request opened a PR without approval");
    let id = queued["pending"]["id"].as_str().unwrap().to_string();
    let digest = queued["pending"]["digest"].as_str().unwrap().to_string();
    let key = crate::oauth_accounts::secret();

    assert_eq!(service.approve(&ME, &key, &id, &digest).await.unwrap_err(), Error::ApproverRequired);
    service.register_approver("dev_one", &key).unwrap();
    let other = Caller { device: "dev_cli", ..ME };
    assert_eq!(service.approve(&other, &key, &id, &digest).await.unwrap_err(), Error::ApproverRequired);
    assert_eq!(service.approve(&ME, &key, &id, "0000").await.unwrap_err(), Error::DigestMismatch);
    let created = service.approve(&ME, &key, &id, &digest).await.unwrap();
    assert_eq!(created["status"], "created");
    assert_eq!(service.approve(&ME, &key, &id, &digest).await.unwrap_err(), Error::NotFound);
    assert_eq!(seen(&calls, "github/pulls").len(), 1);

    let audit = service.audit("one", 10);
    let text = serde_json::to_string(&audit).unwrap();
    assert!(audit.iter().any(|line| line["action"] == "pr.create" && line["result"] == "ok" && line["via"] == "approver"));
    for secret in ["비밀 본문", "로그인 정리", "gh-access"] {
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
    let dead = service
        .store(&ME, github_as("4242", now() - 10, "dead"), &[Feature::GithubPr])
        .await
        .unwrap();
    assert_eq!(service.request(&ME, None, pr("a/b")).await.unwrap().get("ok"), Some(&json!(true)));
    let key = crate::oauth_accounts::secret();
    service.register_approver("dev_one", &key).unwrap();
    let pending = service.list("one").await.unwrap()["pending"][0].clone();
    let (id, digest) = (pending["id"].as_str().unwrap(), pending["digest"].as_str().unwrap());
    assert_eq!(service.approve(&ME, &key, id, digest).await.unwrap_err(), Error::Reconnect);
    assert_eq!(service.list("one").await.unwrap()["connections"][0]["state"], "reconnect_required");
    assert_eq!(service.approve(&ME, &key, id, digest).await.unwrap_err(), Error::Reconnect);
    assert_eq!(seen(&calls, "github/token").len(), 1, "kept knocking with a dead refresh token");

    let gone = service.disconnect(&ME, dead["id"].as_str().unwrap()).await.unwrap();
    assert_eq!(gone["provider_revoked"], true);
    assert!(seen(&calls, "github/revoke")[0].starts_with("app-id "));
    assert!(service.list("one").await.unwrap()["pending"].as_array().unwrap().is_empty());
    assert!(service.list("one").await.unwrap()["connections"].as_array().unwrap().is_empty());
    assert_eq!(service.disconnect(&ME, "con_missing").await.unwrap_err(), Error::NotFound);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn several_connections_need_a_choice() {
    let (service, _, dir) = service().await;
    service.store(&ME, github_as("4242", 0, "r1"), &[Feature::GithubPr]).await.unwrap();
    let work = service.store(&ME, github_as("5353", 0, "r2"), &[Feature::GithubPr]).await.unwrap();
    assert_eq!(service.request(&ME, None, pr("a/b")).await.unwrap_err(), Error::ChooseConnection);
    assert!(service.request(&ME, work["id"].as_str(), pr("a/b")).await.is_ok());
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn gmail_connections_sealed_before_the_withdrawal_are_revoked_and_forgotten() {
    let (service, calls, dir) = service().await;
    service.store(&ME, github(0), &[Feature::GithubPr]).await.unwrap();
    let mut book: Value = service.vault.read("one").unwrap().unwrap();
    book["connections"].as_array_mut().unwrap().extend([
        json!({"id":"con_mail","provider":"google","subject":"google-sub","display":"me@example.com",
            "features":["mail.read","mail.send"],"access_token":"g-access","access_expires":1,
            "refresh_token":"g-refresh-old","refresh_expires":0,"created":1,"updated":1}),
        json!({"id":"con_gone","provider":"google","subject":"google-work","display":"me@work.example",
            "features":["mail.read"],"access_token":"","access_expires":1,
            "refresh_token":"gone","refresh_expires":0,"created":1,"updated":1}),
    ]);
    book["pending"] = json!([{"id":"pw_mail","connection":"con_mail","digest":"d","device":"dev_one",
        "device_label":"Laptop","created":now(),
        "write":{"kind":"mail","to":["a@example.com"],"cc":[],"subject":"s","body":"b","reply_to":null}}]);
    service.vault.write("one", &book).unwrap();
    service.vault.write("two", &json!({"connections":[],"pending":[]})).unwrap();

    service.retire_gmail().await;
    let revoked = seen(&calls, "google/revoke");
    assert_eq!(revoked.len(), 2);
    assert!(revoked.iter().any(|body| body == "token=g-refresh-old"), "revoked the access token instead of the grant");
    let listed = service.list("one").await.unwrap();
    assert_eq!(listed["connections"].as_array().unwrap().len(), 1);
    assert_eq!(listed["connections"][0]["provider"], "github");
    assert!(listed["pending"].as_array().unwrap().is_empty(), "queued mail survived");
    let sealed: Value = service.vault.read("one").unwrap().unwrap();
    assert!(!sealed.to_string().contains("g-refresh-old"), "the Gmail token stayed sealed");
    let audit = service.audit("one", 10);
    assert!(audit.iter().any(|line| line["action"] == "gmail.retire" && line["target"]["connections"] == 2));
    assert!(!serde_json::to_string(&audit).unwrap().contains("g-refresh"));
    assert!(service.audit("two", 10).is_empty(), "an account without Gmail was touched");

    // Already retired: nothing more is revoked.
    service.retire_gmail().await;
    assert_eq!(seen(&calls, "google/revoke").len(), 2);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn writes_are_checked_before_they_wait() {
    let title = Write::Pr(PrDraft { repo: "a/b".into(), base: "main".into(), head: "x".into(), title: "t\r\nx".into(), body: String::new(), draft: false });
    assert_eq!(title.validate(), Err(Error::Invalid));
    for (repo, head) in [("../etc", "x"), ("a/b/c", "x"), ("a/b", "--upload-pack"), ("a/b", "x y")] {
        let pr = Write::Pr(PrDraft { repo: repo.into(), base: "main".into(), head: head.into(), title: "t".into(), body: String::new(), draft: false });
        assert_eq!(pr.validate(), Err(Error::Invalid), "{repo} {head}");
    }
    assert!(Write::Pr(PrDraft { repo: "a/b".into(), base: "main".into(), head: "fork:feature/x".into(), title: "t".into(), body: String::new(), draft: false }).validate().is_ok());
    assert_ne!(pr("a/b").digest(), {
        let Write::Pr(mut changed) = pr("a/b");
        changed.body.push('!');
        Write::Pr(changed).digest()
    });
}
