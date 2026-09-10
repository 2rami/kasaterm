//! Identity belongs to a harness run/conversation, never to an empty terminal.
use super::*;

fn choose<'a>(requested: Option<&'a str>, resumed: Option<&'a str>, next: Option<&'a str>) -> Option<&'a str> {
    requested.or(resumed).or(next).filter(|name| !name.is_empty())
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
        let next = automatic.then(|| self.next_auto_character(&members, pane)).flatten();
        let name = choose(requested, mapped.as_deref(), next.as_deref())
            .ok_or_else(|| anyhow::anyhow!("no student available for this launch"))?.to_string();
        let persona = if socket::read_claude_persona() {
            kasa_mcp::character::persona_for_any(&name)
                .ok_or_else(|| anyhow::anyhow!("assigned character has no instructions"))?
        } else { String::new() };
        let model = roster.as_ref().and_then(|r| kasa_mcp::character::model_for(r, &name)).unwrap_or_default();
        let backend = roster.as_ref().and_then(|r| kasa_mcp::character::backend_for(r, &name)).unwrap_or_default();
        if !sid.is_empty() { kasa_mcp::character::bind_session_character(sid, &name)?; }
        // GUI event serialization makes allocation and publication one operation
        // across all rooms. The old pane name is deliberately NOT a fallback.
        self.pane_claude_sid.remove(pane);
        if sid.is_empty() { self.pane_session_id.remove(pane); }
        else { self.pane_session_id.insert(pane.into(), sid.into()); }
        {
            let mut ws = self.ws.lock().unwrap();
            ws.pane_next_character.remove(pane);
            ws.pane_launch_character.insert(pane.into(), name.clone());
        }
        self.pane_agent_launches.insert(pane.into(), pid);
        self.relabel_pane(pane, &name);
        if automatic { self.last_auto_character = Some(name.clone()); }
        Ok(serde_json::json!({"character": name, "persona": persona, "slug": crate::theme::agent_slug(&name),
            "model": model, "backend": backend}))
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
            self.pane_session_id.remove(&pane);
            self.pane_claude_sid.remove(&pane);
            let mut ws = self.ws.lock().unwrap();
            ws.pane_character.remove(&pane);
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
        eprintln!("[agent-identity] PASS: resumed name/persona agree; late mapping cannot relabel; explicit selection updates both");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn resume_and_manual_launch_resolve_before_the_next_automatic_student() {
        assert_eq!(choose(None, Some("코하루"), Some("모모이")), Some("코하루"));
        assert_eq!(choose(Some("아로나"), Some("코하루"), Some("모모이")), Some("아로나"));
        assert_eq!(choose(None, None, Some("모모이")), Some("모모이"));
    }
}
