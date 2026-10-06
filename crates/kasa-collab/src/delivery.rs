//! GUI 없는 호스트(`kasa tui` 서버)의 tell 전달 루프. 장부에 쌓인 쪽지를 받는 칸 입력창에
//! 붙여넣고 Enter 를 친다. 본판 GUI 는 화면 판독·claude mod 우편함까지 쓰는 자기 루프가 따로 있다.
//!
//! 지키는 것:
//! - 받는 칸의 신원(세션)이 접수 때와 같아야 쓴다. 바뀌었으면 실패로 끝낸다.
//! - 사람이 쓰던 글이 입력창에 있거나 방금 치는 중이면 기다린다(만료까지).
//! - 붙여넣기와 Enter 사이에 칸 입력이 바뀌면 Enter 를 치지 않고 「불확실」로 끝낸다 — 자동 재시도는 없다.

use std::sync::{Arc, Weak};
use std::time::Duration;

use kasa_socket::tell::{Hold, Record, State};
use kasa_socket::Backend;

const TICK: Duration = Duration::from_millis(500);
/// 사람이 마지막으로 친 뒤 이만큼 조용해야 붙여넣는다.
const TYPING_QUIET: Duration = Duration::from_millis(1500);
/// 붙여넣은 글이 입력창에 그려질 틈.
const PASTE_SETTLE: Duration = Duration::from_millis(250);
/// 붙여넣기~Enter 사이 사람 입력을 붙들어 두는 상한.
const HOLD_FOR: Duration = Duration::from_secs(3);

/// 전달 스레드를 띄운다. 백엔드가 사라지면 스스로 끝난다.
pub fn spawn(backend: Weak<dyn Backend>) -> std::io::Result<std::thread::JoinHandle<()>> {
    std::thread::Builder::new().name("collab-tell-delivery".into()).spawn(move || loop {
        let Some(backend) = backend.upgrade() else { return };
        tick(&backend);
        drop(backend);
        std::thread::sleep(TICK);
    })
}

/// 장부를 한 번 훑는다. 시험이 직접 부른다.
pub fn tick(backend: &Arc<dyn Backend>) {
    let Ok(pending) = crate::tell_service::pending() else { return };
    let Ok(local) = crate::board_service::local_id() else { return };
    // 칸마다 맨 앞 쪽지 하나만 — 뒤 쪽지는 앞이 끝난 다음 바퀴에 간다.
    let mut first: std::collections::BTreeMap<String, Record> = std::collections::BTreeMap::new();
    for record in pending.into_iter().filter(|r| r.address.machine_id == local) {
        let slot = first.entry(record.address.surface_id.clone()).or_insert_with(|| record.clone());
        if record.message_id < slot.message_id {
            *slot = record;
        }
    }
    for record in first.values() {
        deliver(backend.as_ref(), record);
    }
}

fn finish(record: &Record, state: State, reason: &str) {
    if let Err(e) = crate::tell_service::transition(&record.message_id, state, reason) {
        eprintln!("[tell] {} 상태 기록 실패: {e:#}", record.message_id);
    }
}

fn deliver(backend: &dyn Backend, record: &Record) {
    if record.expires_at_ms <= kasa_socket::tell::now_ms() {
        finish(record, State::Failed, "tell expired before delivery");
        return;
    }
    let surface = record.address.surface_id.as_str();
    let current = match crate::tell_service::current_address(backend, surface) {
        Ok(address) => address,
        Err(e) => {
            finish(record, State::Failed, &format!("receiver unavailable: {e:#}"));
            return;
        }
    };
    if current != record.address {
        finish(record, State::Failed, "receiver identity changed before the first write");
        return;
    }
    let Some(pty) = kasa_pty::lookup_session(surface) else {
        finish(record, State::Failed, "receiver closed before the first write");
        return;
    };
    let hold = if pty.input_draft_present() {
        Some(Hold::Draft)
    } else if !pty.input_quiet_for(TYPING_QUIET) {
        Some(Hold::Typing)
    } else {
        None
    };
    if let Some(hold) = hold {
        if record.reject_if_busy {
            finish(record, State::Failed, hold.reason());
        } else if record.reason != hold.reason() {
            // 접수 → 보류는 전달 중을 거쳐야 장부가 받는다. 까닭이 바뀔 때만 적는다.
            if crate::tell_service::transition(&record.message_id, State::Dispatching, "checking receiver").is_ok() {
                finish(record, State::Accepted, hold.reason());
            }
        }
        return;
    }
    if crate::tell_service::transition(&record.message_id, State::Dispatching, "writing to receiver").is_err() {
        return;
    }
    let revision = pty.input_revision();
    pty.hold_input(HOLD_FOR);
    let payload = format!("\x1b[200~{}\x1b[201~", record.body);
    let revision = match pty.send_bytes_guarded(payload.as_bytes(), Some(revision)) {
        Ok(rev) => rev,
        Err(_) => {
            let _ = pty.release_input();
            finish(record, State::Uncertain, "paste write failed or input changed; automatic retry prohibited");
            return;
        }
    };
    std::thread::sleep(PASTE_SETTLE);
    let enter = pty.send_bytes_guarded(b"\r", Some(revision));
    let _ = pty.release_input();
    match enter {
        Ok(_) => finish(record, State::Submitted, "paste and Enter writes succeeded; model read is unconfirmed"),
        Err(_) => finish(record, State::Uncertain, "Enter withheld: input changed after paste"),
    }
}
