use super::tests::{actual, binding, manual_url};
use super::*;
use std::cell::Cell;

const CODE: &str = "fixture-auth-code-1234#fixture-state-code-1234";

fn request(nonce: u128) -> StartRequest {
    parse_start(
        &serde_json::json!({
            "nonce": uuid::Uuid::from_u128(nonce).to_string(),
            "machine_id": "machine-fixture",
        })
        .to_string(),
    )
    .unwrap()
}

fn state(known: bool) -> State {
    let mut binding = binding();
    if known {
        binding.identity = crate::session::AccountIdentity::from_verified(identity());
    }
    State::new(
        "flow-fixture".into(),
        &request(1),
        binding,
        Instant::now(),
        600_000,
    )
}

fn identity() -> kasa_mcp::VerifiedClaudeIdentity {
    actual(1, 2, "one@example.test", "Example")
}

fn authorizing(state: &mut State) {
    let (sender, receiver) = std::sync::mpsc::sync_channel(8);
    read_output(
        Box::new(std::io::Cursor::new(format!(
            "{}\nPaste code",
            manual_url()
        ))),
        sender,
    );
    for output in receiver {
        state.output(output);
    }
    assert_eq!(state.phase, Phase::Code);
}

fn verified(state: &mut State) {
    authorizing(state);
    let mut pipe = Vec::new();
    assert_eq!(
        state.action(Action::Code(CODE), Some(&mut pipe)),
        Ok(Effect::None)
    );
    assert_eq!(pipe, format!("{CODE}\n").into_bytes());
    state.verified(identity(), false);
}

fn commit(state: &mut State, promoted: &Cell<usize>, result: Result<bool, &'static str>) -> bool {
    let binding = state.binding.clone();
    state
        .commit(
            Ok(&binding),
            true,
            true,
            Instant::now(),
            Some(identity()),
            true,
            || {
                promoted.set(promoted.get() + 1);
                result
            },
        )
        .is_some()
}

#[test]
fn start_replay_is_the_same_flow_and_does_not_acquire_another_cli() {
    let states = vec![state(true)];
    let mut cli_starts = 1;
    let start = request(1);
    let replay = replay_id(states.iter(), &start, &states[0].binding, |_| true).unwrap();
    if replay.is_none() {
        can_start(states.iter(), false).unwrap();
        cli_starts += 1;
    }
    assert_eq!(replay.as_deref(), Some("flow-fixture"));
    assert_eq!(cli_starts, 1);
    assert!(
        replay_id(states.iter(), &request(2), &states[0].binding, |_| true)
            .unwrap()
            .is_none()
    );
    assert_eq!(can_start(states.iter(), false), Err("phone_login_busy"));
    assert_eq!(
        replay_id(states.iter(), &start, &states[0].binding, |_| false),
        Err("phone_login_owner_changed")
    );
    let mut changed = states[0].binding.clone();
    changed.slot.push('2');
    assert_eq!(
        replay_id(states.iter(), &start, &changed, |_| true),
        Err("phone_login_target_changed")
    );
}

#[test]
fn known_identity_runs_the_real_transitions_and_commits_once() {
    let mut state = state(true);
    let promoted = Cell::new(0);
    assert_eq!(state.view()["state"], "starting");
    verified(&mut state);
    assert_eq!(state.phase, Phase::Verifying);
    assert!(state.view().get("authorization_url").is_none());
    assert!(state.view().get("confirmation_nonce").is_none());
    assert!(commit(&mut state, &promoted, Ok(true)));
    assert_eq!(state.view()["state"], "succeeded");
    assert_eq!(state.view()["runtime_updated"], true);
    assert!(!commit(&mut state, &promoted, Ok(true)));
    assert_eq!(promoted.get(), 1);
}

#[test]
fn the_first_code_is_written_once_and_no_code_is_kept_in_the_response() {
    let mut state = state(true);
    authorizing(&mut state);
    let mut pipe = Vec::new();
    for _ in 0..3 {
        let binding = state.binding.clone();
        state
            .check(Ok(&binding), true, true, Instant::now())
            .unwrap();
        let action = Action::parse("claude-phone-login-code", CODE).unwrap();
        assert_eq!(state.action(action, Some(&mut pipe)), Ok(Effect::None));
    }
    assert_eq!(pipe, format!("{CODE}\n").into_bytes());
    assert!(!state.view().to_string().contains(CODE));
    assert!(state.view().get("authorization_url").is_none());
}

#[test]
fn malformed_code_and_an_unavailable_cli_never_advance_to_verifying() {
    let mut state = state(true);
    authorizing(&mut state);
    let mut pipe = Vec::new();
    for value in ["bad", "abc#def", "x\n#injected"] {
        assert_eq!(
            state.action(Action::Code(value), Some(&mut pipe)),
            Err("phone_login_invalid_code")
        );
        assert_eq!(state.phase, Phase::Code);
    }
    assert_eq!(
        state.action(Action::Code(CODE), None),
        Err("phone_login_not_waiting")
    );
    assert!(pipe.is_empty());
    struct Broken;
    impl Write for Broken {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::ErrorKind::BrokenPipe.into())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    state.action(Action::Code(CODE), Some(&mut Broken)).unwrap();
    assert_eq!(state.phase, Phase::Failed);
    assert_eq!(state.error, Some("phone_login_cli_failed"));
}

#[test]
fn legacy_metadata_requires_a_one_use_confirmation_and_a_second_identity_check() {
    let mut state = state(false);
    let promoted = Cell::new(0);
    verified(&mut state);
    assert_eq!(state.phase, Phase::Confirm);
    assert!(!commit(&mut state, &promoted, Ok(true)));
    assert_eq!(promoted.get(), 0);
    assert_eq!(
        state.action(Action::Confirm("wrong"), None),
        Err("phone_login_confirmation_invalid")
    );
    let nonce = state.confirmation.clone().unwrap();
    assert_eq!(
        state.action(Action::Confirm(&nonce), None),
        Ok(Effect::VerifyConfirmed)
    );
    assert_eq!(
        state.action(Action::Confirm(&nonce), None),
        Err("phone_login_confirmation_invalid")
    );
    assert_eq!(state.verified(identity(), true), Effect::Ready);
    assert!(commit(&mut state, &promoted, Ok(true)));
    assert_eq!(promoted.get(), 1);
    assert!(state.view().get("confirmation_nonce").is_none());
}

#[test]
fn a_changed_identity_after_confirmation_never_reaches_the_store() {
    for change_in_commit in [false, true] {
        let mut state = state(false);
        let promoted = Cell::new(0);
        verified(&mut state);
        let nonce = state.confirmation.clone().unwrap();
        state.action(Action::Confirm(&nonce), None).unwrap();
        let other = actual(9, 2, "one@example.test", "Example");
        if change_in_commit {
            assert_eq!(state.verified(identity(), true), Effect::Ready);
            let binding = state.binding.clone();
            assert!(state
                .commit(
                    Ok(&binding),
                    true,
                    true,
                    Instant::now(),
                    Some(other),
                    true,
                    || {
                        promoted.set(promoted.get() + 1);
                        Ok(true)
                    }
                )
                .is_none());
        } else {
            assert_eq!(state.verified(other, true), Effect::None);
            assert!(!commit(&mut state, &promoted, Ok(true)));
        }
        assert_eq!(state.error, Some("phone_login_identity_mismatch"));
        assert_eq!(promoted.get(), 0);
    }
}

#[test]
fn slot_deletion_owner_change_and_lost_lease_reject_a_late_ready_event() {
    for reason in 0..5 {
        let mut state = state(true);
        verified(&mut state);
        let mut current = state.binding.clone();
        if reason == 0 {
            current.slot.push('2');
        }
        if reason == 1 {
            current.machine.push('2');
        }
        let binding = if reason == 2 {
            Err("phone_login_target_changed")
        } else {
            Ok(&current)
        };
        let promoted = Cell::new(0);
        assert!(state
            .commit(
                binding,
                reason != 3,
                reason != 4,
                Instant::now(),
                Some(identity()),
                true,
                || {
                    promoted.set(promoted.get() + 1);
                    Ok(true)
                }
            )
            .is_none());
        assert!(state.phase.terminal());
        assert_eq!(promoted.get(), 0);
    }
}

#[test]
fn cancel_and_expiry_block_late_outputs_verification_code_and_commit() {
    for expire in [false, true] {
        let mut state = state(true);
        verified(&mut state);
        if expire {
            let binding = state.binding.clone();
            state
                .check(Ok(&binding), true, true, state.deadline)
                .unwrap();
            assert_eq!(state.phase, Phase::Expired);
        } else {
            state.action(Action::Cancel, None).unwrap();
            assert_eq!(state.phase, Phase::Cancelled);
        }
        let phase = state.phase;
        state.output(Output::Url(manual_url().into()));
        assert_eq!(state.verified(identity(), false), Effect::None);
        let mut pipe = Vec::new();
        assert_eq!(
            state.action(Action::Code(CODE), Some(&mut pipe)),
            Err("phone_login_not_waiting")
        );
        let promoted = Cell::new(0);
        assert!(!commit(&mut state, &promoted, Ok(true)));
        assert_eq!(state.phase, phase);
        assert_eq!(promoted.get(), 0);
        assert!(pipe.is_empty());
        assert!(state.view().get("authorization_url").is_none());
    }
}

#[test]
fn a_failed_store_does_not_claim_success_and_a_retained_workbench_is_explicit() {
    let mut failed = state(true);
    verified(&mut failed);
    let promoted = Cell::new(0);
    assert!(!commit(
        &mut failed,
        &promoted,
        Err("phone_login_target_changed")
    ));
    assert_eq!(failed.view()["state"], "failed");
    assert_eq!(failed.view()["error_code"], "phone_login_target_changed");
    assert_eq!(promoted.get(), 1);
    let mut retained = state(true);
    verified(&mut retained);
    assert!(commit(&mut retained, &promoted, Ok(false)));
    assert_eq!(retained.view()["state"], "succeeded");
    assert_eq!(retained.view()["runtime_updated"], false);
    assert_eq!(
        retained.view()["runtime_error_code"],
        "runtime_update_pending"
    );
}

#[test]
fn a_missing_or_expired_profile_proof_does_not_reach_the_store() {
    let mut state = state(true);
    verified(&mut state);
    let binding = state.binding.clone();
    let promoted = Cell::new(0);
    assert!(state
        .commit(Ok(&binding), true, true, Instant::now(), None, true, || {
            promoted.set(1);
            Ok(true)
        })
        .is_none());
    assert_eq!(state.error, Some("phone_login_identity_unverified"));
    assert_eq!(promoted.get(), 0);
}

#[test]
fn login_stays_busy_until_both_the_cli_and_staged_cleanup_have_finished() {
    for cleanup_first in [false, true] {
        let mut state = state(false);
        verified(&mut state);
        let nonce = state.confirmation.clone().unwrap();
        state.action(Action::Confirm(&nonce), None).unwrap();
        assert_eq!(state.workers, 2);
        assert!(!state.worker_finished());
        assert_eq!(state.workers, 1);
        state.action(Action::Cancel, None).unwrap();
        assert!(state.cleanup_pending);
        assert_eq!(
            can_start(std::iter::once(&state), false),
            Err("phone_login_busy")
        );
        if cleanup_first {
            assert!(!state.cleanup_finished());
            assert_eq!(
                can_start(std::iter::once(&state), false),
                Err("phone_login_busy")
            );
            assert!(state.worker_finished());
        } else {
            assert!(!state.worker_finished());
            assert_eq!(
                can_start(std::iter::once(&state), false),
                Err("phone_login_busy")
            );
            state.finish(Phase::Cancelled, None);
            assert!(state.cleanup_pending);
            assert!(state.cleanup_finished());
        }
        assert_eq!(can_start(std::iter::once(&state), false), Ok(()));
    }
}

#[test]
fn stale_cleanup_cannot_release_a_new_flow_lease() {
    let job = |id: &str| LoginJob {
        provider: AccountProvider::Claude,
        id: lease_id(id),
        state: LoginState::Running,
        device: None,
        adopting: None,
    };
    let mut lease = (Some(job("old")), None, None);
    release_login_lease_in(&mut lease, "old");
    assert!(lease.0.is_none());
    lease.0 = Some(job("new"));
    release_login_lease_in(&mut lease, "old");
    assert_eq!(lease.0.as_ref().unwrap().id, lease_id("new"));
}

#[test]
fn a_blocked_cleanup_keeps_a_terminal_flow_busy_until_its_resource_drop_returns() {
    let mut state = state(false);
    verified(&mut state);
    state.worker_finished();
    state.action(Action::Cancel, None).unwrap();
    let state = Arc::new(Mutex::new(state));
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    struct BlockedDrop {
        entered: std::sync::mpsc::Sender<()>,
        release: std::sync::mpsc::Receiver<()>,
    }
    impl Drop for BlockedDrop {
        fn drop(&mut self) {
            self.entered.send(()).unwrap();
            self.release.recv_timeout(Duration::from_secs(5)).unwrap();
        }
    }
    let cleanup_state = state.clone();
    let cleanup = std::thread::spawn(move || {
        drop(BlockedDrop {
            entered: entered_tx,
            release: release_rx,
        });
        cleanup_state.lock().unwrap().cleanup_finished()
    });
    entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    {
        let mut state = state.lock().unwrap();
        state.action(Action::Cancel, None).unwrap();
        assert_eq!(
            can_start(std::iter::once(&*state), false),
            Err("phone_login_busy")
        );
        assert!(state.cleanup_pending);
    }
    release_tx.send(()).unwrap();
    assert!(cleanup.join().unwrap());
    let state = state.lock().unwrap();
    assert_eq!(can_start(std::iter::once(&*state), false), Ok(()));
}

#[test]
fn a_closed_confirmation_is_replayed_without_starting_a_new_flow() {
    let mut state = state(false);
    verified(&mut state);
    state.action(Action::Cancel, None).unwrap();
    state.worker_finished();
    state.cleanup_finished();
    let replay = replay_id(std::iter::once(&state), &request(1), &state.binding, |_| {
        true
    })
    .unwrap();
    assert_eq!(replay.as_deref(), Some("flow-fixture"));
    assert_eq!(state.view()["state"], "cancelled");
    assert_eq!(
        can_start(std::iter::once(&state), true),
        Err("phone_login_busy")
    );
}
