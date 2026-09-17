//! Identity belongs to a harness run/conversation, never to an empty terminal.
use super::*;

/// 이름을 정하는 순서: 사용자가 고른 것 > 이어 여는 대화의 학생 > **이 자리에 앉았던 학생** > 다음 차례.
///
/// 셋째가 2026-09-17 에 들어왔다. pane 에서 claude 를 껐다 다시 켜면 그 자리의 학생(비석
/// `pane_last_seat`)이 아직 다른 곳에 안 앉았는데도 다음 차례를 뽑아, 사용자 눈에는 「재시작했더니
/// 코하루가 세이아로 바뀌었다」로 보였다. 앱 안 `/resume` 으로 옛 대화를 이어 붙이면 대화는 옛
/// 학생 것인데 이름·말투는 새 학생인 상태가 된다. 같은 자리 재시작은 같은 학생이 맞는다 — 새
/// 학생은 다른 pane 이거나, 그 학생이 이미 다른 자리에 앉았을 때만.
fn choose<'a>(requested: Option<&'a str>, resumed: Option<&'a str>, seated: Option<&'a str>, next: Option<&'a str>) -> Option<&'a str> {
    // 빈 이름은 후보가 아니다 — 앞자리가 빈 문자열이면 뒷자리로 넘어간다(빈 비석이 다음 차례를 막지 않게).
    [requested, resumed, seated, next].into_iter().flatten().find(|name| !name.is_empty())
}

impl App {
    pub(crate) fn prepare_agent_identity(&mut self, pane: &str, sid: &str, requested: &str, pid: u32) -> anyhow::Result<serde_json::Value> {
        anyhow::ensure!(!kasa_mcp::remote::is_remote_pane(pane), "cannot launch an identity on a mirror");
        anyhow::ensure!(self.ws.lock().unwrap().outer_for_pty(pane).is_some(), "pane is no longer open");
        anyhow::ensure!(pid > 0, "missing harness process identity");
        let pending = self.ws.lock().unwrap().pane_next_character.get(pane).cloned();
        let requested = (!requested.is_empty()).then_some(requested).or(pending.as_deref());
        if let Some(name) = requested {
            anyhow::ensure!(crate::theme::character_slug_any(name).is_some(), "unknown character");
        }
        let mapped = (!sid.is_empty()).then(|| kasa_mcp::character::session_character(sid)).flatten()
            .filter(|name| kasa_mcp::character::is_assignable_for(sid, name));
        let automatic = requested.is_none() && mapped.is_none();
        let roster = kasa_mcp::character::roster_in_use();
        let members = roster.as_ref().map(kasa_mcp::character::assignable_names).unwrap_or_default();
        // 이 자리의 직전 학생 — 명부에 있고 지금 어디에도 안 앉아 있을 때만 다시 앉힌다.
        let seated = automatic.then(|| self.pane_last_seat.get(pane).map(|(name, _)| name.clone())).flatten()
            .filter(|name| !name.is_empty() && members.iter().any(|m| m == name)
                && !self.assigned_characters(pane).iter().any(|t| t == name));
        let next = (automatic && seated.is_none()).then(|| self.next_auto_character(&members, pane)).flatten();
        let name = choose(requested, mapped.as_deref(), seated.as_deref(), next.as_deref())
            .ok_or_else(|| anyhow::anyhow!("no student available for this launch"))?.to_string();
        let persona = if socket::read_claude_persona() {
            kasa_mcp::character::persona_for_any(&name)
                .ok_or_else(|| anyhow::anyhow!("assigned character has no instructions"))?
        } else { String::new() };
        let model = roster.as_ref().and_then(|r| kasa_mcp::character::model_for(r, &name)).unwrap_or_default();
        let backend = roster.as_ref().and_then(|r| kasa_mcp::character::backend_for(r, &name)).unwrap_or_default();
        if !sid.is_empty() { kasa_mcp::character::bind_session_character(sid, &name)?; }
        // GUI event serialization makes allocation and publication one operation
        // across all rooms. The old *pane label* is deliberately NOT a fallback —
        // 자리 기억은 위의 비석(pane_last_seat) 하나로만 잇는다.
        self.pane_claude_sid.remove(pane);
        // 학생이 앉았다(같은 학생이든 새 학생이든) — 비석은 여기서 지운다.
        self.pane_last_seat.remove(pane);
        if sid.is_empty() { self.pane_session_id.remove(pane); }
        else { self.pane_session_id.insert(pane.into(), sid.into()); }
        {
            let mut ws = self.ws.lock().unwrap();
            ws.pane_next_character.remove(pane);
            ws.pane_launch_character.insert(pane.into(), name.clone());
        }
        self.pane_agent_launches.insert(pane.into(), pid);
        let launch_token = kasa_mcp::character::new_session_id();
        self.file_tree.instruction_launches.insert(pane.into(), launch_token.clone());
        self.relabel_pane(pane, &name);
        if automatic { self.last_auto_character = Some(name.clone()); }
        Ok(serde_json::json!({"character": name, "persona": persona, "slug": crate::theme::agent_slug(&name),
            "model": model, "backend": backend, "launch_token": launch_token}))
    }

    pub(crate) fn release_finished_agent_identities(&mut self) {
        if self.pane_agent_launches.is_empty() { return; }
        let table = kasa_pty::process_table_shared();
        let finished: Vec<_> = self.pane_agent_launches.iter()
            .filter(|(_, pid)| !table.iter().any(|(p, _, _)| p == *pid))
            .map(|(pane, _)| pane.clone()).collect();
        if finished.is_empty() { return; }
        // The render cache can predate a just-started wrapper. Confirm absence
        // with a fresh process snapshot before clearing a student's live seat.
        let fresh = kasa_pty::fresh_process_table();
        if fresh.is_empty() { return; }
        for pane in finished {
            if self.pane_agent_launches.get(&pane).is_some_and(|pid| fresh.iter().any(|(p, _, _)| p == pid)) { continue; }
            self.pane_agent_launches.remove(&pane);
            self.file_tree.instruction_launches.remove(&pane);
            self.pane_session_id.remove(&pane);
            // 자리를 비우기 전에 비석을 남긴다 — 이 자리가 닫히면 되살리기 줄이 이걸로
            // 얼굴과 대화 번호를 되찾는다(없으면 기록을 손으로 뒤져야 했다).
            let sid = self.pane_claude_sid.remove(&pane).unwrap_or_default();
            let mut ws = self.ws.lock().unwrap();
            let character = ws.pane_character.remove(&pane).unwrap_or_default();
            if !character.is_empty() || !sid.is_empty() {
                self.pane_last_seat.insert(pane.clone(), (character, sid));
            }
            ws.pane_launch_character.insert(pane.clone(), String::new());
            if let Some(outer) = ws.outer_for_pty(&pane) {
                if let Some(state) = ws.panes.get_mut(&outer) { state.character = None; state.dirty = true; }
            }
            // Empty derived markers release the live seat; saved conversation
            // bindings remain intact, so resuming still restores this student.
            if let Some(cwd) = self.pane_cwd_cache.get(&pane) {
                let slug = kasa_mcp::character::rslug(cwd, ws.pane_room.get(&pane).map(String::as_str));
                let _ = kasa_mcp::character::write_marker(&slug, &pane, "");
            }
            let safe: String = pane.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect();
            let _ = std::fs::remove_file(kasa_socket::bound_marker_path(&safe));
            self.session_touched = true;
            self.chrome_dirty = true;
        }
    }

    pub(crate) fn run_agent_identity_probe(&mut self) {
        if !crate::verification_run() || std::env::var_os("KASATERM_AUTO_AGENT_IDENTITY").is_none()
            || self.pty.is_empty() { return; }
        static RAN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        if RAN.swap(true, std::sync::atomic::Ordering::Relaxed) { return; }
        let pane = self.target_surface().unwrap();
        let sid = kasa_mcp::character::new_session_id();
        kasa_mcp::character::bind_session_character(&sid, "코하루").unwrap();
        assert!(!self.ws.lock().unwrap().pane_character.contains_key(&pane), "empty shell has no student");
        let resolved = self.prepare_agent_identity(&pane, &sid, "", std::process::id()).unwrap();
        assert_eq!(resolved["character"], "코하루");
        assert!(resolved["persona"].as_str().unwrap().contains("너는 코하루"));
        // The exact historical failure: late saved mapping replaces only the
        // name while the harness has already loaded another student's voice.
        kasa_mcp::character::bind_session_character(&sid, "모모이").unwrap();
        self.apply_session_character(&pane, &sid);
        assert_eq!(self.ws.lock().unwrap().pane_character[&pane], "코하루");
        let resolved = self.prepare_agent_identity(&pane, &sid, "아로나", std::process::id()).unwrap();
        assert_eq!(resolved["character"], "아로나");
        assert!(resolved["persona"].as_str().unwrap().contains("너는 아로나"));
        assert_eq!(self.ws.lock().unwrap().pane_character[&pane], "아로나");
        let first = self.prepare_agent_identity(&pane, &kasa_mcp::character::new_session_id(), "", std::process::id()).unwrap();
        let second = self.prepare_agent_identity(&pane, "", "", std::process::id()).unwrap();
        assert_eq!(first["character"], "아로나");
        assert_ne!(first["character"], second["character"], "new harness in the same pane advances the roster");
        // 같은 자리 재시작: 직전 학생의 비석이 있고 그 학생이 비어 있으면 다시 앉는다.
        let prev = second["character"].as_str().unwrap().to_string();
        self.ws.lock().unwrap().pane_character.remove(&pane);
        self.pane_last_seat.insert(pane.clone(), (prev.clone(), String::new()));
        let again = self.prepare_agent_identity(&pane, "", "", std::process::id()).unwrap();
        assert_eq!(again["character"], prev.as_str(), "restart in the same pane keeps the student");
        assert!(!self.pane_last_seat.contains_key(&pane), "seat memory is consumed");
        // 그 학생이 다른 자리에 앉아 있으면 비석이 있어도 다음 차례로.
        self.ws.lock().unwrap().pane_character.remove(&pane);
        self.pane_last_seat.insert(pane.clone(), (prev.clone(), String::new()));
        self.ws.lock().unwrap().pane_character.insert("%elsewhere".into(), prev.clone());
        let moved = self.prepare_agent_identity(&pane, "", "", std::process::id()).unwrap();
        assert_ne!(moved["character"], prev.as_str(), "a student seated elsewhere is not reused");
        self.ws.lock().unwrap().pane_character.remove("%elsewhere");
        eprintln!("[agent-identity] PASS: resumed name/persona agree; late mapping cannot relabel; explicit selection updates both; same-seat restart keeps the student");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn resume_and_manual_launch_resolve_before_the_next_automatic_student() {
        assert_eq!(choose(None, Some("코하루"), None, Some("모모이")), Some("코하루"));
        assert_eq!(choose(Some("아로나"), Some("코하루"), None, Some("모모이")), Some("아로나"));
        assert_eq!(choose(None, None, None, Some("모모이")), Some("모모이"));
    }

    #[test]
    fn same_seat_restart_beats_the_next_automatic_student_but_not_resume_or_choice() {
        // 재시작: 비석의 학생이 다음 차례보다 앞선다.
        assert_eq!(choose(None, None, Some("코하루"), Some("세이아")), Some("코하루"));
        // 이어 여는 대화의 학생과 사용자의 선택은 여전히 그보다 앞선다.
        assert_eq!(choose(None, Some("모모이"), Some("코하루"), Some("세이아")), Some("모모이"));
        assert_eq!(choose(Some("아로나"), None, Some("코하루"), Some("세이아")), Some("아로나"));
        // 빈 비석은 없는 것과 같다.
        assert_eq!(choose(None, None, Some(""), Some("세이아")), Some("세이아"));
    }
}
