use super::*;

pub(super) fn tick(app: &mut App) {
    if !crate::verification_run() || std::env::var_os("KASATERM_SHARED_FIXTURE_CREDENTIAL").is_none() { return; }
    let Some(root) = std::env::var_os("KASATERM_TEST_SHARED_DIR").map(std::path::PathBuf::from) else { return; };
    static LAST: std::sync::Mutex<u64> = std::sync::Mutex::new(0);
    if let Some(command) = std::fs::read(root.join("command.json")).ok()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok()) {
        let id = command["id"].as_u64().unwrap_or(0);
        let fresh = {
            let mut last = LAST.lock().unwrap();
            let fresh = id > *last;
            if fresh { *last = id; }
            fresh
        };
        if fresh {
            match command["action"].as_str().unwrap_or("") {
                "open" => { app.open_shared_room_view(command["room"].as_str()); }
                #[cfg(target_os = "macos")]
                "click" => {
                    let x = command["x"].as_f64().unwrap_or(0.0) as f32;
                    let y = command["y"].as_f64().unwrap_or(0.0) as f32;
                    app.native_mouse(&[(5, x, y), (1, x, y), (2, x, y)], 0);
                }
                #[cfg(target_os = "macos")]
                "key" => {
                    let flags = command["flags"].as_u64().unwrap_or(0);
                    for ch in command["text"].as_str().unwrap_or("").chars() { app.native_key(flags, ch); }
                }
                "wheel" => {
                    let x = command["x"].as_f64().unwrap_or(0.0) as f32;
                    let y = command["y"].as_f64().unwrap_or(0.0) as f32;
                    app.shared_wheel((x, y), winit::event::MouseScrollDelta::LineDelta(0.0, command["delta"].as_f64().unwrap_or(0.0) as f32));
                }
                _ => {}
            }
        }
    }
    let state = &app.shared_rooms;
    let hits: Vec<_> = state.hits.iter().map(|(action, rect)| serde_json::json!({"action": label(action), "rect": rect})).collect();
    let nav_hits: Vec<_> = app.info.navigation.shared_verification_hits().into_iter()
        .map(|(name, rect)| serde_json::json!({"action": name, "rect": rect})).collect();
    let report = serde_json::json!({
        "command": *LAST.lock().unwrap(), "active": app.shared_room_active(), "room": state.active,
        "pty_count": app.pty.len(), "rooms": app.room_numbers_for_windows(),
        "local_sidebar_count": app.sidebar_layout(app.window.as_ref().map_or(700.0, |w| w.inner_size().height as f32 / app.effective_scale())).0.len(),
        "input_revisions": app.pty.iter().map(|(id, pty)| (id.clone(), pty.input_revision())).collect::<HashMap<_, _>>(),
        "bound_sessions": app.pane_claude_sid,
        "reader_items": state.reader.conversation.items.len(), "reader_next": state.reader.next,
        "reader_generation": state.reader.generation, "reader_session": state.reader.session,
        "error": state.error, "note": state.note, "pending": state.pending, "reading": state.reading,
        "hits": hits, "nav_hits": nav_hits, "area": state.area, "form_scroll_max": state.form_scroll_max,
        "form_text": state.form.as_ref().map(|f| f.text.as_str()),
        "operations": state.runtime.operations.iter().map(|op| op.to_json()).collect::<Vec<_>>()
    });
    if let Ok(bytes) = serde_json::to_vec(&report) {
        let path = root.join("report.json");
        let temporary = root.join("report.tmp");
        if std::fs::write(&temporary, bytes).is_ok() { let _ = std::fs::rename(temporary, path); }
    }
}

fn label(action: &Action) -> String {
    match action {
        Action::Open(id) => format!("open:{id}"), Action::Select(id) => format!("select:{id}"),
        Action::SelectSource(id) => format!("source:{id}"), Action::RemoveMember(id) => format!("remove:{id}"),
        Action::RevokeInvite(id) => format!("revoke:{id}"), Action::ToggleTool(id) => format!("tool:{id}"),
        Action::New => "new".into(), Action::Join => "join".into(), Action::Back => "back".into(),
        Action::CloseView => "close".into(), Action::Refresh => "refresh".into(), Action::SignIn => "signin".into(),
        Action::Invite => "invite".into(), Action::CopyInvite => "copy-invite".into(), Action::CopyConversation => "copy".into(),
        Action::Manage => "manage".into(), Action::Rename => "rename".into(), Action::ToggleFork => "toggle-fork".into(),
        Action::Leave => "leave".into(), Action::Delete => "delete".into(), Action::Confirm => "confirm".into(),
        Action::Cancel => "cancel".into(), Action::Submit => "submit".into(), Action::FocusField => "field".into(),
        Action::ToggleProject => "toggle-project".into(), Action::Publish => "publish".into(), Action::Fork => "fork".into(),
    }
}
