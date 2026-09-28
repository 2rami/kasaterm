use super::*;
use serde_json::json;

pub(super) struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        Self(
            std::env::temp_dir()
                .canonicalize()
                .unwrap()
                .join(format!("kasa-assistant-{}", id())),
        )
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn ctx(account: &str, device: &str) -> AuthContext {
    AuthContext::authenticated(account, device).unwrap()
}
/// 닫은 저장소를 다시 연다. 같은 검사 바이너리의 다른 검사가 PTY 를 fork 하면 자식이 exec 전까지
/// 잠금 fd 를 물고 있어 flock 이 잠깐 안 풀린다(병렬 전체 실행에서 5번 중 4번 재현) — 그 틈만 기다린다.
fn reopen(dir: &Temp) -> Store {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        match Store::open(dir.0.clone()) {
            Ok(store) => return store,
            Err(error) if std::time::Instant::now() >= deadline => panic!("reopen failed: {error:?}"),
            Err(_) => std::thread::sleep(std::time::Duration::from_millis(20)),
        }
    }
}
pub(super) fn ready() -> (Temp, Store, AuthContext) {
    let dir = Temp::new();
    let store = Store::open(dir.0.clone()).unwrap();
    let context = ctx("alice", "device-a");
    store
        .set_key(&context, 0, Some("registered-test-key".into()))
        .unwrap();
    (dir, store, context)
}
fn task(store: &Store, context: &AuthContext) -> Task {
    store
        .create_task(
            context,
            "Git 브랜치 그래프 개선\n원문을 보존해 주세요.",
            "source123",
            vec!["tests".into()],
            100,
        )
        .unwrap()
}
fn proof(task: &Task) -> Evidence {
    Evidence {
        task_id: task.id.clone(),
        task_revision: task.rev,
        work_revision: task.work_revision.clone(),
        source_device: "device-a".into(),
        observed_at: 100,
        expires_at: 400,
        artifact_digest: "a".repeat(64),
        checks: vec![Check {
            id: "tests".into(),
            state: CheckState::Pass,
        }],
        complete: true,
    }
}

#[test]
fn key_registration_is_explicit_account_scoped_and_never_read_back() {
    let (dir, store, alice) = ready();
    let snapshot = store.snapshot(&alice).unwrap();
    assert!(snapshot.status.enabled);
    assert!(
        !store
            .snapshot(&ctx("bob", "device-b"))
            .unwrap()
            .status
            .enabled
    );
    assert!(!serde_json::to_string(&snapshot)
        .unwrap()
        .contains("registered-test-key"));
    let disk = std::fs::read_to_string(dir.0.join("alice.sealed")).unwrap();
    assert!(!disk.contains("registered-test-key"));
    assert!(AuthContext::authenticated("../alice", "device").is_err());
    assert!(AuthContext::authenticated("alice", "../device").is_err());
    assert!(matches!(
        store.create_task(&ctx("bob", "device-b"), "request", "work", vec![], 100),
        Err(Error::Disabled)
    ));
}

#[test]
fn vault_is_private_single_writer_and_survives_restart() {
    let (dir, store, alice) = ready();
    let task = task(&store, &alice);
    assert!(matches!(Store::open(dir.0.clone()), Err(Error::Storage)));
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        std::fs::metadata(dir.0.join("master.key"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert_eq!(
        std::fs::metadata(&dir.0).unwrap().permissions().mode() & 0o777,
        0o700
    );
    drop(store);
    let reopened = reopen(&dir);
    assert_eq!(
        reopened.snapshot(&alice).unwrap().tasks[0].original_prompt,
        task.original_prompt
    );
}

#[test]
fn corrupt_cross_account_envelopes_and_missing_master_fail_closed() {
    let (dir, store, alice) = ready();
    let original = std::fs::read(dir.0.join("alice.sealed")).unwrap();
    std::fs::copy(dir.0.join("alice.sealed"), dir.0.join("bob.sealed")).unwrap();
    assert!(matches!(
        store.snapshot(&ctx("bob", "device-b")),
        Err(Error::Storage)
    ));
    drop(store);
    let mut corrupt: serde_json::Value = serde_json::from_slice(&original).unwrap();
    corrupt["ciphertext"] = json!("AAAA");
    std::fs::write(
        dir.0.join("alice.sealed"),
        serde_json::to_vec(&corrupt).unwrap(),
    )
    .unwrap();
    let reopened = reopen(&dir);
    assert!(matches!(reopened.snapshot(&alice), Err(Error::Storage)));
    drop(reopened);
    std::fs::remove_file(dir.0.join("master.key")).unwrap();
    assert!(matches!(Store::open(dir.0.clone()), Err(Error::Storage)));
}

#[test]
fn symlink_and_broad_permission_vaults_are_rejected() {
    let dir = Temp::new();
    std::fs::create_dir(&dir.0).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&dir.0, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(Store::open(dir.0.clone()).is_err());
    std::fs::set_permissions(&dir.0, std::fs::Permissions::from_mode(0o700)).unwrap();
    let link = dir.0.join("link");
    std::os::unix::fs::symlink(&dir.0, &link).unwrap();
    assert!(Store::open(link).is_err());
}

#[test]
fn create_task_idempotency_preserves_original_and_rejects_changed_replay() {
    let (_dir, store, alice) = ready();
    let prompt = "  Git 개선\n\n원본 마지막 줄\n";
    let first = store
        .create_task_once(&alice, "request-1", prompt, "source", vec![], 100)
        .unwrap();
    let second = store
        .create_task_once(&alice, "request-1", prompt, "source", vec![], 200)
        .unwrap();
    assert_eq!(first.id, second.id);
    assert_eq!(first.original_prompt, prompt);
    assert_eq!(store.snapshot(&alice).unwrap().tasks.len(), 1);
    assert!(matches!(
        store.create_task_once(&alice, "request-1", "different", "source", vec![], 200),
        Err(Error::Conflict)
    ));
}

#[test]
fn decision_jobs_are_cancelled_by_key_rotation_task_revision_and_cancellation() {
    for action in ["rotate", "delete", "change", "cancel"] {
        let (_dir, store, alice) = ready();
        store
            .create_project(&alice, "Project", ProjectKind::Coding, vec!["git".into()])
            .unwrap();
        let task = task(&store, &alice);
        let (job, _, _) = store
            .begin(
                &alice,
                &task.id,
                task.rev,
                DecisionPurpose::ClassifyProject,
                100,
            )
            .unwrap();
        let advice = jev::Advice::fixture(&job.choices, "unassigned", 0.99);
        match action {
            "rotate" => {
                store
                    .set_key(&alice, 1, Some("rotated-test-key".into()))
                    .unwrap();
            }
            "delete" => {
                store.set_key(&alice, 1, None).unwrap();
            }
            "change" => {
                store
                    .update_task(
                        &alice,
                        &task.id,
                        task.rev,
                        TaskState::Working,
                        "new work",
                        "source456",
                        101,
                    )
                    .unwrap();
            }
            _ => {
                store
                    .update_task(
                        &alice,
                        &task.id,
                        task.rev,
                        TaskState::Cancelled,
                        "cancelled",
                        "source123",
                        101,
                    )
                    .unwrap();
            }
        }
        assert!(matches!(
            store.apply(&alice, &job.id, Some(advice), 102),
            Err(Error::Stale)
        ));
        assert!(!store.snapshot(&alice).unwrap().tasks[0].verify_ok);
    }
}

#[test]
fn task_and_evidence_revision_mismatch_cannot_be_overridden_by_done_prose() {
    let (_dir, store, alice) = ready();
    let task = task(&store, &alice);
    assert!(matches!(
        store.update_task(
            &alice,
            &task.id,
            task.rev,
            TaskState::Verified,
            "Everything done",
            "source123",
            101
        ),
        Err(Error::Invalid)
    ));
    let mut wrong = proof(&task);
    wrong.work_revision = "oldsource".into();
    assert!(matches!(
        store.record_evidence(
            &alice,
            &task.id,
            task.rev,
            wrong,
            ProofOrigin::TrustedRunner,
            101
        ),
        Err(Error::Conflict)
    ));
    let mut wrong = proof(&task);
    wrong.task_revision += 1;
    assert!(matches!(
        store.record_evidence(
            &alice,
            &task.id,
            task.rev,
            wrong,
            ProofOrigin::TrustedRunner,
            101
        ),
        Err(Error::Conflict)
    ));
}

#[test]
fn completion_requires_trusted_fresh_complete_passes_not_reported_or_failed_checks() {
    for case in [
        "reported",
        "failed",
        "unknown",
        "missing",
        "expired",
        "future",
        "duplicate",
        "artifact",
        "incomplete",
    ] {
        let (_dir, store, alice) = ready();
        let task = task(&store, &alice);
        let mut evidence = proof(&task);
        match case {
            "failed" => evidence.checks[0].state = CheckState::Fail,
            "unknown" => evidence.checks[0].state = CheckState::Unknown,
            "missing" => evidence.checks.clear(),
            "expired" => evidence.expires_at = 100,
            "future" => evidence.observed_at = 200,
            "duplicate" => evidence.checks.push(evidence.checks[0].clone()),
            "artifact" => evidence.artifact_digest.clear(),
            "incomplete" => evidence.complete = false,
            _ => {}
        }
        let task = store
            .record_evidence(
                &alice,
                &task.id,
                task.rev,
                evidence,
                if case == "reported" {
                    ProofOrigin::Reported
                } else {
                    ProofOrigin::TrustedRunner
                },
                101,
            )
            .unwrap();
        assert!(!task.verify_ok);
        assert!(
            matches!(
                store.begin(
                    &alice,
                    &task.id,
                    task.rev,
                    DecisionPurpose::VerifyCompletion,
                    101
                ),
                Err(Error::EvidenceRequired)
            ),
            "{case}"
        );
        assert!(store
            .claim_notification(&alice, &task.id, task.rev, 101)
            .is_err());
    }
}

#[test]
fn verified_completion_claim_is_durable_once_across_devices_and_replays() {
    let (dir, store, alice) = ready();
    let task = task(&store, &alice);
    let task = store
        .record_evidence(
            &alice,
            &task.id,
            task.rev,
            proof(&task),
            ProofOrigin::TrustedRunner,
            101,
        )
        .unwrap();
    let (job, _, _) = store
        .begin(
            &alice,
            &task.id,
            task.rev,
            DecisionPurpose::VerifyCompletion,
            102,
        )
        .unwrap();
    let advice = jev::Advice::fixture(&job.choices, "verified", 0.99);
    let task = store
        .apply(&alice, &job.id, Some(advice.clone()), 103)
        .unwrap();
    assert!(task.verify_ok && task.state == TaskState::Verified);
    assert!(matches!(
        store.apply(&alice, &job.id, Some(advice), 104),
        Err(Error::Stale)
    ));
    assert!(store
        .claim_notification(&alice, &task.id, task.rev, 104)
        .unwrap()
        .is_some());
    assert!(store
        .claim_notification(&ctx("alice", "device-other"), &task.id, task.rev, 104)
        .unwrap()
        .is_none());
    assert!(store
        .claim_notification(&ctx("bob", "device-other"), &task.id, task.rev, 104)
        .is_err());
    drop(store);
    let store = reopen(&dir);
    assert!(store
        .claim_notification(&alice, &task.id, task.rev, 105)
        .unwrap()
        .is_none());
}

#[test]
fn confidence_expiry_and_missing_reply_never_mark_complete() {
    for case in ["low", "missing", "expired", "wait"] {
        let (_dir, store, alice) = ready();
        let task = task(&store, &alice);
        let task = store
            .record_evidence(
                &alice,
                &task.id,
                task.rev,
                proof(&task),
                ProofOrigin::TrustedRunner,
                101,
            )
            .unwrap();
        let (job, _, _) = store
            .begin(
                &alice,
                &task.id,
                task.rev,
                DecisionPurpose::VerifyCompletion,
                102,
            )
            .unwrap();
        let advice = if case == "missing" {
            None
        } else {
            Some(jev::Advice::fixture(
                &job.choices,
                if case == "wait" { "wait" } else { "verified" },
                if case == "low" { 0.79 } else { 0.99 },
            ))
        };
        let result = store.apply(
            &alice,
            &job.id,
            advice,
            if case == "expired" { 223 } else { 103 },
        );
        if case == "expired" {
            assert!(matches!(result, Err(Error::Stale)));
        } else {
            assert!(!result.unwrap().verify_ok);
        }
    }
}

#[test]
fn classification_only_exports_dictionary_features_and_stable_candidate_ids() {
    let (_dir, store, alice) = ready();
    let project = store
        .create_project(
            &alice,
            "secretname@example.com",
            ProjectKind::Coding,
            vec!["git".into()],
        )
        .unwrap();
    let task = store
        .create_task(
            &alice,
            "Git build user@private.com /Users/private/repo Bearer registered-test-key 01012345678",
            "source",
            vec![],
            100,
        )
        .unwrap();
    let (job, key, _) = store
        .begin(
            &alice,
            &task.id,
            task.rev,
            DecisionPurpose::ClassifyProject,
            100,
        )
        .unwrap();
    assert_eq!(key, "registered-test-key");
    let outbound = job.request.to_string();
    for secret in [
        "secretname",
        "user@private",
        "/Users/private",
        "Bearer",
        "registered-test-key",
        "01012345678",
    ] {
        assert!(!outbound.contains(secret));
    }
    assert!(outbound.contains("git") && outbound.contains("build"));
    let pick = format!("p_{}", project.id);
    let result = store
        .apply(
            &alice,
            &job.id,
            Some(jev::Advice::fixture(&job.choices, &pick, 0.99)),
            101,
        )
        .unwrap();
    assert_eq!(result.project, Some(project.id));
}

#[test]
fn unknown_context_defers_without_model_call_and_rate_limit_is_bounded() {
    let (_dir, store, alice) = ready();
    store
        .create_project(&alice, "project", ProjectKind::General, vec![])
        .unwrap();
    let task = store
        .create_task(&alice, "알 수 없는 문장", "source", vec![], 100)
        .unwrap();
    assert!(matches!(
        store.begin(
            &alice,
            &task.id,
            task.rev,
            DecisionPurpose::ClassifyProject,
            100
        ),
        Err(Error::Unavailable)
    ));
    let mut rate = Rate::default();
    for _ in 0..60 {
        rate.take(100).unwrap();
    }
    assert!(matches!(rate.take(100), Err(Error::RateLimited)));
    assert!(rate.take(3600).is_ok());
    assert!(matches!(rate.take(100), Err(Error::RateLimited)));
}

#[test]
fn completion_cache_is_epoch_scoped_and_does_not_bypass_current_evidence() {
    let (_dir, store, alice) = ready();
    let first = task(&store, &alice);
    let first = store
        .record_evidence(
            &alice,
            &first.id,
            first.rev,
            proof(&first),
            ProofOrigin::TrustedRunner,
            101,
        )
        .unwrap();
    let (job, _, _) = store
        .begin(
            &alice,
            &first.id,
            first.rev,
            DecisionPurpose::VerifyCompletion,
            102,
        )
        .unwrap();
    store
        .apply(
            &alice,
            &job.id,
            Some(jev::Advice::fixture(&job.choices, "wait", 0.99)),
            103,
        )
        .unwrap();
    let second = task(&store, &alice);
    let second = store
        .record_evidence(
            &alice,
            &second.id,
            second.rev,
            proof(&second),
            ProofOrigin::TrustedRunner,
            104,
        )
        .unwrap();
    let (_, _, cached) = store
        .begin(
            &alice,
            &second.id,
            second.rev,
            DecisionPurpose::VerifyCompletion,
            105,
        )
        .unwrap();
    assert!(cached.is_some());
    store
        .set_key(&alice, 1, Some("new-test-key".into()))
        .unwrap();
    let (_, _, cached) = store
        .begin(
            &alice,
            &second.id,
            second.rev,
            DecisionPurpose::VerifyCompletion,
            106,
        )
        .unwrap();
    assert!(cached.is_none());
}

pub(super) fn request(id: &str) -> MessageRequest {
    MessageRequest {
        id: id.into(),
        text: "Git 브랜치 작업을 정리해 주세요".into(),
        task_id: None,
        model: None,
    }
}

#[tokio::test]
async fn revoked_message_never_starts_provider_or_persists_request() {
    let (_dir, store, alice) = ready();
    let provider = TextProvider::new(Vec::new()).unwrap();
    assert!(matches!(
        store
            .message(&alice, request("revoked"), &provider, 100, &|| false)
            .await,
        Err(Error::Stale)
    ));
    assert!(store.snapshot(&alice).unwrap().conversation.is_empty());
}

#[test]
fn authorization_revocation_between_decision_and_commit_cancels_verified_transition() {
    let (_dir, store, alice) = ready();
    let task = task(&store, &alice);
    let task = store
        .record_evidence(
            &alice,
            &task.id,
            task.rev,
            proof(&task),
            ProofOrigin::TrustedRunner,
            101,
        )
        .unwrap();
    let (job, _, _) = store
        .begin(
            &alice,
            &task.id,
            task.rev,
            DecisionPurpose::VerifyCompletion,
            102,
        )
        .unwrap();
    let advice = jev::Advice::fixture(&job.choices, "verified", 0.99);
    assert!(matches!(
        store.apply_checked(&alice, &job.id, Some(advice), 103, &|| false),
        Err(Error::Stale)
    ));
    assert!(!store.snapshot(&alice).unwrap().tasks[0].verify_ok);
    assert!(store
        .claim_notification(&alice, &task.id, task.rev, 104)
        .is_err());
}

#[test]
fn unknown_choice_or_invalid_confidence_is_not_a_completion_vote() {
    let (_dir, store, alice) = ready();
    let task = task(&store, &alice);
    let task = store
        .record_evidence(
            &alice,
            &task.id,
            task.rev,
            proof(&task),
            ProofOrigin::TrustedRunner,
            101,
        )
        .unwrap();
    let (job, _, _) = store
        .begin(
            &alice,
            &task.id,
            task.rev,
            DecisionPurpose::VerifyCompletion,
            102,
        )
        .unwrap();
    let unknown = jev::Advice::fixture(&job.choices, "override_everything", 1.0);
    assert!(!unknown.valid(&job.choices));
    assert!(!jev::Advice::fixture(&job.choices, "verified", 1.01).valid(&job.choices));
    let task = store.apply(&alice, &job.id, Some(unknown), 103).unwrap();
    assert!(!task.verify_ok);
}

#[test]
fn jev_adapter_refuses_missing_or_leaked_key_before_spawning_anything() {
    let (dir, _store, _alice) = ready();
    let adapter = JevAdapter::new("/usr/bin/false".into(), dir.0.join("master.key")).unwrap();
    assert!(matches!(
        adapter.choose(&json!({}), ""),
        Err(Error::Disabled)
    ));
    assert!(matches!(
        adapter.choose(
            &json!({"state":"registered-test-key"}),
            "registered-test-key"
        ),
        Err(Error::Invalid)
    ));
}

#[test]
fn decision_results_cannot_cross_account_or_initiating_device() {
    let (_dir, store, alice) = ready();
    store.create_project(&alice,"Project",ProjectKind::Coding,vec!["git".into()]).unwrap();
    let task = task(&store,&alice);
    let (job,_,_) = store.begin(&alice,&task.id,task.rev,DecisionPurpose::ClassifyProject,100).unwrap();
    let advice = jev::Advice::fixture(&job.choices,"unassigned",0.99);
    assert!(matches!(store.apply(&ctx("bob","device-a"),&job.id,Some(advice.clone()),101),Err(Error::Stale)));
    assert!(matches!(store.apply(&ctx("alice","device-b"),&job.id,Some(advice),101),Err(Error::Stale)));
    assert_eq!(store.snapshot(&alice).unwrap().tasks[0].rev,task.rev);
}
