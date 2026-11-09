use super::*;

#[test]
fn chunks_keep_partial_json_and_use_byte_offsets() {
    let mut reader = Reader { generation: "g".into(), ..Reader::default() };
    let raw = "{\"type\":\"user\",\"message\":{\"content\":\"공유 대화\"}}\n";
    let split = raw.find("대화").unwrap();
    for (from, end) in [(0, split), (split, raw.len())] {
        assert!(reader.append(&api::SharedContent { generation: "g".into(), revision: 1,
            offset: from as u64, next: end as u64, end: raw.len() as u64, raw: raw[from..end].into() }));
        if from == 0 { assert!(reader.conversation.items.is_empty()); }
    }
    assert!(reader.tail.is_empty());
    assert_eq!(reader.conversation.items.len(), 1);
    assert!(!reader.append(&api::SharedContent { generation: "g".into(), revision: 1,
        offset: 0, next: raw.len() as u64, end: raw.len() as u64, raw: raw.into() }));
}

#[test]
fn account_change_forgets_invites_reader_and_form() {
    let mut state = State::default();
    state.invites.insert("r".into(), api::SharedInvite { id: "i".into(), room_id: "r".into(), token: "private-invite".into(), expires_at_ms: 1, max_uses: 1 });
    state.form = Some(Form::new(FormKind::Join, "private-draft".into()));
    state.reader.tail = "private unfinished row".into();
    state.reader.conversation.apply("{\"type\":\"user\",\"message\":{\"content\":\"private message\"}}\n");
    state.pending = true;
    state.clear_account();
    assert!(state.invites.is_empty() && state.form.is_none() && state.client.is_none());
    assert!(state.reader.tail.is_empty() && state.reader.conversation.items.is_empty());
    assert!(!state.pending);
}

#[test]
fn shared_views_have_no_pty_and_never_persist_as_user_rooms() {
    let kind = crate::internal_room::InternalRoomKind::SharedRooms;
    let layout = kasa_pty::PtyLayout::single(kind.pane_id());
    assert!(!crate::internal_room::should_persist_layout(&layout));
    for action in crate::internal_room::InternalMutation::ALL {
        assert!(!crate::internal_room::pane_operation_allowed(PANE, action));
    }
    let nav = Nav { rows: vec![("r".into(), "방".into(), false, 3)], ..Nav::default() };
    assert_eq!(nav.height(), 88.0);
    assert_eq!(Nav { collapsed: true, ..nav }.height(), 44.0);
}

fn response(state: &State) -> Event {
    Event { sequence: state.sequence, view_sequence: state.view_sequence,
        catalog_sequence: state.catalog_sequence, write_sequence: None, reader_sequence: None,
        auth_epoch: state.auth_epoch, client: None, reading: false, polling: false,
        result: Err(kasa_remote::shared_rooms::client::Error { status: 503, code: "shared_storage".into() }) }
}

#[test]
fn stale_read_failure_cannot_cancel_the_new_read() {
    let mut state = State::default();
    let mut old = response(&state);
    old.reading = true; old.reader_sequence = Some(state.reader_sequence);
    state.reading = true;
    state.reset_reader();
    assert!(!state.reading);
    state.reading = true;
    assert!(!state.accepts(&old));
    assert!(state.reading);
    let mut current = response(&state);
    current.reading = true; current.reader_sequence = Some(state.reader_sequence);
    assert!(state.accepts(&current));
}

#[test]
fn stale_catalog_cannot_undo_a_room_mutation() {
    let mut state = State::default();
    let mut old = response(&state); old.polling = true;
    state.catalog_sequence += 1;
    assert!(!state.accepts(&old));
    state.clear_account();
    assert!(!state.accepts(&response(&State::default())));
}

#[test]
fn invite_response_keeps_its_room_and_cancel_marks_pending_launch() {
    let mut state = State::default();
    state.active = Some("b".into());
    state.invites.insert("a".into(), api::SharedInvite { id: "i".into(), room_id: "a".into(),
        token: "private-a".into(), expires_at_ms: 1, max_uses: 1 });
    assert!(state.current_invite().is_none());
    state.active = Some("a".into());
    assert_eq!(state.current_invite().unwrap().token, "private-a");
    state.pending = true;
    state.form = Some(Form::new(FormKind::Join, "code".into()));
    state.cancel_view();
    assert!(state.form.is_none());
    assert!(state.cancelled_writes.contains(&state.write_sequence));
}
