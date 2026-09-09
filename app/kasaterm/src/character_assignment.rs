//! One automatic student order, independent of room/cwd and mirror copies.
use super::*;

fn workspace_characters<'a>(workspaces: impl Iterator<Item = &'a Arc<Mutex<Workspace>>>, excluded: &str) -> HashMap<String, String> {
    let mut taken = HashMap::new();
    for ws in workspaces {
        for (id, name) in &ws.lock().unwrap().pane_character {
            if id != excluded && !name.is_empty() { taken.insert(id.clone(), name.clone()); }
        }
    }
    taken
}

impl App {
    pub(crate) fn assigned_characters(&self, excluded: &str) -> Vec<String> {
        let mut taken: Vec<_> = workspace_characters(std::iter::once(&self.ws)
            .chain(self.sessions.iter().flatten().map(|s| &s.ws)), excluded)
            .into_iter().filter(|(id, _)| !kasa_mcp::remote::is_remote_pane(id))
            .map(|(_, name)| name).collect();
        taken.extend(kasa_mcp::character::assigned_other_instances());
        if !crate::verification_run() {
            taken.extend(kasa_mcp::machines::cached_character_assignments());
        }
        taken
    }

    pub(crate) fn next_auto_character(&self, members: &[String], excluded: &str) -> Option<String> {
        kasa_mcp::character::pick_in_order(members, &self.assigned_characters(excluded))
    }

    pub(crate) fn run_character_assignment_probe(&mut self) {
        if !crate::verification_run() || std::env::var_os("KASATERM_AUTO_CHARACTER_ORDER").is_none()
            || self.restore_progress.is_some() || self.pty.is_empty() { return; }
        static RAN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        if RAN.swap(true, std::sync::atomic::Ordering::Relaxed) { return; }
        let original = self.target_surface().expect("initial verification pane");
        assert_eq!(self.ws.lock().unwrap().pane_character[&original], "아로나");
        let one = self.spawn_new_tab(&original, false).expect("first ordered tab");
        assert_eq!(self.ws.lock().unwrap().pane_character[&one], "미도리");
        self.pane_cwd_cache.insert(one.clone(), std::path::PathBuf::from("/tmp/different-worktree"));
        let two = self.spawn_new_tab(&original, false).expect("second ordered tab");
        assert_eq!(self.ws.lock().unwrap().pane_character[&two], "모모이");
        self.new_window();
        let next = self.target_surface().expect("new ordered room");
        let ws = self.ws.lock().unwrap();
        assert_eq!(ws.pane_character[&next], "히후미");
        assert_eq!(ws.pane_character[&original], "아로나", "running identity must remain unchanged");
        assert_eq!(self.pty.len(), 4);
        eprintln!("[character-order] PASS: Arona first, distinct tabs/worktree/room, existing identities preserved, isolated registry={}", kasa_socket::collab_root().display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parked_rooms_and_new_tabs_reserve_names_without_disk_markers_or_cwd() {
        let active = Arc::new(Mutex::new(Workspace::default()));
        let parked = Arc::new(Mutex::new(Workspace::default()));
        active.lock().unwrap().pane_character.insert("%1".into(), "아로나".into());
        parked.lock().unwrap().pane_character.insert("%2".into(), "미도리".into());
        // No PaneState/cwd/process/marker yet: newly allocated tabs still count.
        let taken = workspace_characters([&active, &parked, &active].into_iter(), "%new");
        assert_eq!(taken.len(), 2, "an active workspace must not be counted twice");
        let members = ["미도리", "모모이", "아로나"].map(String::from);
        assert_eq!(kasa_mcp::character::pick_in_order(&members, &taken.into_values().collect::<Vec<_>>()).as_deref(), Some("모모이"));
        assert_eq!(workspace_characters([&active, &parked].into_iter(), "%1").len(), 1);
    }
}
