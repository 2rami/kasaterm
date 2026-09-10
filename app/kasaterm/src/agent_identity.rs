//! Resolve identity once, before exec, and share it with all display consumers.
use super::*;

fn choose<'a>(requested: Option<&'a str>, resumed: Option<&'a str>, current: Option<&'a str>) -> Option<&'a str> {
    requested.or(resumed).or(current).filter(|name| !name.is_empty())
}

pub(crate) fn prepare(ws: &Mutex<Workspace>, pane: &str, sid: &str, requested: &str) -> anyhow::Result<serde_json::Value> {
    anyhow::ensure!(!kasa_mcp::remote::is_remote_pane(pane), "cannot launch an identity on a mirror");
    let mut ws = ws.lock().unwrap();
    anyhow::ensure!(ws.outer_for_pty(pane).is_some(), "pane is no longer open");
    let requested = (!requested.is_empty()).then_some(requested);
    if let Some(name) = requested {
        anyhow::ensure!(crate::theme::character_slug_any(name).is_some(), "unknown character");
    }
    let mapped = (!sid.is_empty()).then(|| kasa_mcp::character::session_character(sid)).flatten()
        .filter(|name| kasa_mcp::character::is_assignable_for(sid, name));
    let name = choose(requested, mapped.as_deref(), ws.pane_character.get(pane).map(String::as_str))
        .ok_or_else(|| anyhow::anyhow!("pane has no assigned character"))?.to_string();
    let persona = if socket::read_claude_persona() {
        kasa_mcp::character::persona_for_any(&name)
            .ok_or_else(|| anyhow::anyhow!("assigned character has no instructions"))?
    } else { String::new() };
    // Resolve both before publishing either. No automatic/manual-pick conversion.
    if !sid.is_empty() {
        kasa_mcp::character::bind_session_character(sid, &name)?;
    }
    ws.pane_character.insert(pane.to_string(), name.clone());
    ws.pane_launch_character.insert(pane.to_string(), name.clone());
    Ok(serde_json::json!({"character": name, "persona": persona, "slug": crate::theme::agent_slug(&name)}))
}

impl App {
    pub(crate) fn run_agent_identity_probe(&mut self) {
        if !crate::verification_run() || std::env::var_os("KASATERM_AUTO_AGENT_IDENTITY").is_none()
            || self.pty.is_empty() { return; }
        static RAN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        if RAN.swap(true, std::sync::atomic::Ordering::Relaxed) { return; }
        let pane = self.target_surface().unwrap();
        let sid = kasa_mcp::character::new_session_id();
        kasa_mcp::character::bind_session_character(&sid, "코하루").unwrap();
        let resolved = prepare(&self.ws, &pane, &sid, "").unwrap();
        assert_eq!(resolved["character"], "코하루");
        assert!(resolved["persona"].as_str().unwrap().contains("너는 코하루"));
        // The exact historical failure: late saved mapping replaces only the
        // name while the harness has already loaded another student's voice.
        kasa_mcp::character::bind_session_character(&sid, "모모이").unwrap();
        self.apply_session_character(&pane, &sid);
        assert_eq!(self.ws.lock().unwrap().pane_character[&pane], "코하루");
        let resolved = prepare(&self.ws, &pane, &sid, "아로나").unwrap();
        assert_eq!(resolved["character"], "아로나");
        assert!(resolved["persona"].as_str().unwrap().contains("너는 아로나"));
        assert_eq!(self.ws.lock().unwrap().pane_character[&pane], "아로나");
        eprintln!("[agent-identity] PASS: resumed name/persona agree; late mapping cannot relabel; explicit selection updates both");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn resume_and_manual_launch_resolve_before_the_inherited_shell_identity() {
        assert_eq!(choose(None, Some("코하루"), Some("모모이")), Some("코하루"));
        assert_eq!(choose(Some("아로나"), Some("코하루"), Some("모모이")), Some("아로나"));
        assert_eq!(choose(None, None, Some("모모이")), Some("모모이"));
    }
}
