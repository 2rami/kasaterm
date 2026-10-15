use super::*;

impl App {
    pub(crate) fn run_pending_session_probe(&mut self) {
        use std::sync::{Mutex, OnceLock};
        static MODE: OnceLock<String> = OnceLock::new();
        static START: OnceLock<Instant> = OnceLock::new();
        static STATE: Mutex<Option<(Instant, String, String, u8)>> = Mutex::new(None);
        static DONE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        let mode = MODE.get_or_init(|| {
            if crate::verification_run() { std::env::var("KASATERM_AUTOSESSION_PROBE").unwrap_or_default() }
            else { String::new() }
        });
        if mode.is_empty() || DONE.load(std::sync::atomic::Ordering::Relaxed) { return; }
        if START.get_or_init(Instant::now).elapsed().as_secs() < 3 { return; }
        if mode == "restore" {
            // This preview has no real conversation IDs and never invokes restoration.
            self.restore_prompt = Some(serde_json::json!({"sessions":[{"windows":[
                {"leaf":{"character":"아리스","was_agent":"codex","title":"복원창과 대화 기록 화면 정리","cwd":"/work/kasaterm"}},
                {"leaf":{"character":"마리","title":"로컬 개발 서버","cwd":"/work/website"}},
                {"leaf":{"character":"미카","was_agent":"claude","title":"폰 연결이 끊기지 않는지 확인","cwd":"/work/mobile"}},
                {"leaf":{"character":"유우카","was_agent":"codex","title":"공유 폴더 검색 개선","cwd":"/work/kasa-share"}},
                {"leaf":{"title":"문서 정리","cwd":"/work/docs"}},
                {"leaf":{"character":"호시노","was_agent":"claude","remote_label":"fixture-mini"}}
            ]}]}));
            self.chrome_dirty = true;
            self.render_frame();
            DONE.store(true, std::sync::atomic::Ordering::Relaxed);
            return;
        }
        if mode == "retained" {
            let records = serde_json::json!({"stashed_panes":[
                {"pane_id":"%90","folder":"직접 접어 둔 셸","stashed":true,"retained_by":"user","rec":{"pane_id":"%90"}},
                {"pane_id":"%91","folder":"배치에서 회수한 셸","stashed":true,"retained_by":"recovery","rec":{"pane_id":"%91"}},
                {"pane_id":"%92","folder":"이전 판의 기록","stashed":true,"rec":{"pane_id":"%92"}}
            ]});
            self.restore_closed_history(&records);
            if !self.git.col_visible { self.toggle_git_col(); }
            self.info.tab = crate::state::SideTab::Sessions;
            self.sessions_col.retained_mode = true;
            self.chrome_dirty = true;
            self.render_frame();
            DONE.store(true, std::sync::atomic::Ordering::Relaxed);
            return;
        }
        if matches!(mode.as_str(), "history" | "history-empty" | "history-check") {
            if !self.git.col_visible { self.toggle_git_col(); }
            self.info.tab = crate::state::SideTab::Sessions;
            self.sessions_col.cwd = Some("/work/kasaterm".into());
            let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
            let mut view = Vec::new();
            if mode != "history-empty" {
                for (i,(title,preview,student,harness)) in [
                    ("복원창과 대화 기록 화면 정리", "나: 어떤 일을 하던 대화인지 바로 알아볼 수 있게, 제목과 작업 폴더를 보여줘. 닫은 칸도 대화 기록과 구분해줘.", "아리스", "codex"),
                    ("폰에서 돌아왔을 때 연결 복구", "에이전트: 화면에 돌아오면 같은 칸으로 다시 연결하고 쓰던 초안은 유지하도록 확인했어요.", "미카", "claude"),
                    ("공유 폴더 검색과 최근 파일", "나: 어느 기기에서 만든 파일인지도 구분됐으면 좋겠어.", "유우카", "codex"),
                    ("계정 전환 후 로그인 확인", "에이전트: 선택한 계정과 실제 실행 중인 계정이 같은지 대조하고 있어요.", "프라나", "claude"),
                    ("터미널 스크롤 위치 보존", "나: 이전 대화를 읽다가 새 출력이 와도 읽던 위치가 바뀌지 않게 해줘.", "", "codex"),
                ].into_iter().enumerate() {
                    view.push(kasa_socket::backend::RecentSession { harness:harness.into(), id:format!("fixture-{i}"),
                        label:title.into(), preview:preview.into(), student:student.into(), parent:String::new(),
                        cwd:"/work/kasaterm".into(), mtime:now.saturating_sub((i as u64 + 1)*1200) });
                }
            }
            self.sessions_col.selected = view.first().map(|s| (s.harness.clone(),s.id.clone()));
            self.sessions_col.view = view;
            self.chrome_dirty = true;
            self.render_frame();
            if mode == "history-check" { self.verify_session_history_controls(); }
            DONE.store(true, std::sync::atomic::Ordering::Relaxed);
            return;
        }
        if mode != "primary-close" && mode != "primary-preview" { return; }
        let mut state = STATE.lock().unwrap();
        if mode == "primary-preview" {
            if state.is_none() {
                let Some(primary) = self.ws.lock().unwrap().active_pane.clone() else { return; };
                if self.ws.lock().unwrap().find_tab_by_pty(&primary).is_none() { return; }
                self.ws.lock().unwrap().panes.get_mut(&primary).unwrap().tabs.push(crate::PaneTab {
                    content: crate::PaneContent::Web(crate::WebPane { url: "about:blank".into(), host_id: 0 }),
                    ..Default::default()
                });
                self.close_tab(&primary, 0);
                assert!(!self.pty.contains_key(&primary));
                self.remove_finished_surface(&primary);
                *state = Some((Instant::now(), primary, String::new(), 0));
                return;
            }
            let (since, primary, _, _) = state.as_ref().unwrap();
            if since.elapsed().as_secs() < 4 { return; }
            assert!(!self.pty.contains_key(primary));
            assert!(self.leaf_lingers_anywhere(primary));
            assert!(matches!(self.ws.lock().unwrap().panes[primary].tabs[0].content, crate::PaneContent::Web(_)));
            eprintln!("[session-probe] PASS primary close keeps its PTY-free sibling and application alive");
            DONE.store(true, std::sync::atomic::Ordering::Relaxed);
            *state = None;
            return;
        }
        if state.is_none() {
            let Some(primary) = self.ws.lock().unwrap().active_pane.clone() else { return; };
            if self.ws.lock().unwrap().find_tab_by_pty(&primary).is_none() { return; }
            let second = self.spawn_new_tab(&primary, false).unwrap();
            let third = self.spawn_new_tab(&primary, false).unwrap();
            let held = self.pty[&primary].clone();
            self.close_tab(&primary, 0);
            assert!(!self.pty.contains_key(&primary), "closing a primary tab leaked its PTY");
            assert!(held.input_closed(), "HTTP references must not keep closed input usable");
            assert!(self.pty.contains_key(&second) && self.pty.contains_key(&third));
            self.remove_finished_surface(&primary);
            assert!(self.leaf_lingers_anywhere(&second), "late EOF removed the surviving cell");
            *state = Some((Instant::now(), second, third, 0));
            return;
        }
        let (since, second, third, step) = state.as_ref().unwrap();
        if since.elapsed().as_secs() < 4 { return; }
        assert!(self.closed_panes.iter().all(|entry| !entry.stashed), "closed tab became hidden");
        assert!(self.pty.contains_key(third), "a sibling tab was terminated");
        if *step == 0 {
            let second = second.clone();
            let third = third.clone();
            self.remove_finished_surface(&second);
            assert!(!self.pty.contains_key(&second));
            assert!(self.leaf_lingers_anywhere(&third));
            *state = Some((Instant::now(), second, third, 1));
            return;
        }
        assert!(self.leaf_lingers_anywhere(third));
        eprintln!("[session-probe] PASS primary close, late EOF, sibling EOF, survivor and no hidden PTY");
        DONE.store(true, std::sync::atomic::Ordering::Relaxed);
        *state = None;
    }
}
