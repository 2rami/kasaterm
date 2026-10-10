//! pane id → 세션 등록부. 약한 참조 레지스트리(HTTP·소켓이 GUI 를 거치지 않고 찾는다)와
//! 칸을 닫아도 살려 두는 강한 참조 보관함.

use super::*;

/// 살아 있는 PTY 세션 레지스트리 — pane id → 세션.
///
/// 소유권은 GUI(`App.pty`)에 있고 여기엔 **Weak** 만 둔다. pane 이 닫히면 App 이
/// Arc 를 떨어뜨리는 것만으로 항목이 저절로 무효가 되므로, 해제를 잊어 유령
/// 세션이 남는 부류의 버그가 원천적으로 없다. HTTP·소켓 백엔드가 GUI 스레드를
/// 거치지 않고 세션에 직접 붙는 통로다.
fn registry() -> &'static Mutex<std::collections::HashMap<String, std::sync::Weak<PtySession>>> {
    static R: std::sync::OnceLock<
        Mutex<std::collections::HashMap<String, std::sync::Weak<PtySession>>>,
    > = std::sync::OnceLock::new();
    R.get_or_init(Default::default)
}

/// pane 을 띄운 쪽이 `Arc` 를 손에 넣은 직후 한 번 부른다.
pub fn register_session(id: &str, sess: &Arc<PtySession>) {
    registry()
        .lock()
        .unwrap()
        .insert(id.to_string(), Arc::downgrade(sess));
}

/// 살아 있으면 세션을 돌려준다. 이미 닫힌 pane 이면 `None`.
pub fn lookup_session(id: &str) -> Option<Arc<PtySession>> {
    registry().lock().unwrap().get(id)?.upgrade()
}

/// 지금 살아 있는 pane id 목록(정렬). 죽은 항목은 조회하는 김에 걷어낸다.
pub fn live_sessions() -> Vec<String> {
    let mut r = registry().lock().unwrap();
    r.retain(|_, w| w.strong_count() > 0);
    let mut ids: Vec<String> = r.keys().cloned().collect();
    ids.sort();
    ids
}

/// 보는 사람이 없어도 살려 둘 세션들.
///
/// `registry` 는 `Weak` 라서 **소유자가 사라지면 세션도 사라진다.** 웹에서 띄운
/// 셸은 소유자가 그 WebSocket 하나뿐이라, 탭을 닫는 순간 셸까지 죽었다. 여기에
/// 강한 `Arc` 를 두면 연결과 수명이 갈린다 — 폰을 덮었다 다시 열어도 하던 작업이
/// 그대로 있다.
fn persistent() -> &'static Mutex<std::collections::HashMap<String, Arc<PtySession>>> {
    static P: std::sync::OnceLock<Mutex<std::collections::HashMap<String, Arc<PtySession>>>> =
        std::sync::OnceLock::new();
    P.get_or_init(Default::default)
}

/// 세션을 프로세스에 붙들어 둔다. 셸이 끝나면(EOF) 스스로 빠진다.
///
/// ⚠️ **`screens` 를 소비하므로 GUI pane 에는 쓰면 안 된다.** 그 채널은 MPMC 라
/// 여기서 받은 프레임은 GUI pump 에 안 간다 — 화면이 띄엄띄엄 갱신된다. 보는
/// 사람이 따로 없는 웹 전용 셸에만 쓴다.
pub fn keep_session(id: &str, sess: Arc<PtySession>) {
    let watch = sess.screens.clone();
    persistent()
        .lock()
        .unwrap()
        .insert(id.to_string(), sess);
    let id = id.to_string();
    // 셸이 끝나면 스스로 빠진다 — 안 그러면 죽은 세션이 목록에 영원히 남는다.
    std::thread::Builder::new()
        .name(format!("pty-keep-{id}"))
        .spawn(move || {
            while let Ok(u) = watch.recv() {
                if u.eof {
                    break;
                }
            }
            persistent().lock().unwrap().remove(&id);
        })
        .ok();
}

/// 붙들어 둔 세션을 놓아 준다. 마지막 참조였다면 셸이 종료된다.
pub fn release_session(id: &str) -> bool {
    persistent().lock().unwrap().remove(id).is_some()
}

/// 붙들려 있는 세션 id 목록(정렬).
pub fn kept_sessions() -> Vec<String> {
    let mut ids: Vec<String> = persistent().lock().unwrap().keys().cloned().collect();
    ids.sort();
    ids
}
