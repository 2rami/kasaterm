use kasa_socket::backend::RecentSession;

pub(super) fn key(session: &RecentSession) -> (String, String) {
    (session.harness.clone(), session.id.clone())
}

pub(super) fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub(super) fn harness_name(harness: &str) -> &str {
    match harness {
        "claude" => "Claude",
        "codex" => "Codex",
        "agy" => "Antigravity",
        _ => "대화",
    }
}

pub(super) fn folder(path: &str) -> &str {
    path.rsplit(['/', '\\'])
        .find(|s| !s.is_empty())
        .unwrap_or("폴더 미확인")
}

pub(super) fn title(session: &RecentSession) -> String {
    let label = one_line(&session.label);
    let is_id = !label.is_empty() && session.id.starts_with(&label);
    if !label.is_empty() && !is_id && !crate::screenread::label_is_roster_agent(&label) {
        label
    } else if !session.preview.trim().is_empty() {
        one_line(&session.preview)
    } else {
        format!("{} 대화", harness_name(&session.harness))
    }
}

pub(super) fn ago_at(mtime: u64, now: u64) -> String {
    if mtime == 0 {
        return "시각 미확인".into();
    }
    match now.saturating_sub(mtime) {
        0..=59 => "방금 수정".into(),
        d @ 60..=3599 => format!("{}분 전", d / 60),
        d @ 3600..=86399 => format!("{}시간 전", d / 3600),
        d => format!("{}일 전", d / 86400),
    }
}

pub(super) fn closed_key(c: &crate::ClosedPane) -> String {
    c.ui_id.to_string()
}

pub(super) fn closed_index(entries: &[crate::ClosedPane], key: &str) -> Option<usize> {
    let mut found = entries
        .iter()
        .enumerate()
        .filter(|(_, c)| closed_key(c) == key);
    let (index, _) = found.next()?;
    found.next().is_none().then_some(index)
}

pub(super) fn has_agent(record: &serde_json::Value) -> bool {
    record["session_id"].as_str().is_some_and(|s| !s.is_empty())
        || record["was_agent"]
            .as_str()
            .is_some_and(|s| matches!(s, "claude" | "codex" | "agy"))
        || record["was_claude"] == true
}

pub(super) fn scroll_thumb(top: f32, height: f32, content: f32, scroll: f32) -> Option<(f32, f32)> {
    if height <= 0.0 || content <= height {
        return None;
    }
    let h = (height * height / content).max(24.0).min(height);
    Some((
        top + scroll.clamp(0.0, content - height) / (content - height) * (height - h),
        h,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn session() -> RecentSession {
        RecentSession {
            harness: "codex".into(),
            id: "12345678-1234-1234-1234-123456789abc".into(),
            label: "12345678".into(),
            mtime: 0,
            cwd: "C:\\work\\카사텀".into(),
            preview: "나:  복원창\n다듬기".into(),
            student: String::new(),
            parent: String::new(),
        }
    }
    #[test]
    fn recognizable_titles_never_fall_back_to_a_uuid() {
        let mut s = session();
        assert_eq!(title(&s), "나: 복원창 다듬기");
        s.preview.clear();
        assert_eq!(title(&s), "Codex 대화");
        s.label = "네이티브 화면 정리".into();
        assert_eq!(title(&s), s.label);
        assert_eq!(folder(&s.cwd), "카사텀");
        assert_eq!(folder("/work/kasa/"), "kasa");
        assert_eq!(folder(""), "폴더 미확인");
    }
    #[test]
    fn selection_uses_harness_and_id_not_sorted_position() {
        let a = session();
        let mut b = session();
        b.harness = "claude".into();
        let selected = key(&a);
        assert_ne!(key(&b), selected);
        assert_eq!([b, a].iter().position(|s| key(s) == selected), Some(1));
    }
    #[test]
    fn unknown_and_future_dates_do_not_become_old_or_negative() {
        assert_eq!(ago_at(0, 1000), "시각 미확인");
        assert_eq!(ago_at(2000, 1000), "방금 수정");
        assert_eq!(ago_at(100, 220), "2분 전");
    }
    #[test]
    fn old_shell_character_is_not_agent_evidence() {
        assert!(!has_agent(&serde_json::json!({"character":"old-student"})));
        assert!(has_agent(&serde_json::json!({"session_id":"bound"})));
    }
    #[test]
    fn scrollbar_stays_in_track_at_both_ends() {
        assert_eq!(scroll_thumb(30.0, 300.0, 600.0, 0.0), Some((30.0, 150.0)));
        assert_eq!(
            scroll_thumb(30.0, 300.0, 600.0, 900.0),
            Some((180.0, 150.0))
        );
        assert!(scroll_thumb(30.0, 300.0, 200.0, 0.0).is_none());
    }
    #[test]
    fn a_stale_close_confirmation_cannot_target_a_reused_pane_number() {
        let record = || crate::ClosedPane {
            ui_id: crate::ClosedPane::next_ui_id(),
            rec: serde_json::json!({"cwd":"/work"}),
            pane_id: "%1".into(),
            character: String::new(),
            folder: "work".into(),
            neighbor: None,
            window: 0,
            alive: true,
            stashed: true,
            retained_by: None,
            idle_since: None,
            preview: None,
        };
        let old = record();
        let key = closed_key(&old);
        assert_eq!(closed_index(&[old], &key), Some(0));
        assert_eq!(closed_index(&[record()], &key), None);
    }
}
