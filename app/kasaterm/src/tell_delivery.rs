use super::*;
use kasa_socket::tell::{Address, Record, State};
use kasa_socket::Backend;
use std::time::Duration;
use std::collections::{BTreeMap,HashSet};
use std::sync::atomic::{AtomicBool,AtomicUsize,Ordering};

const PROOF_DEADLINE: Duration = Duration::from_secs(2);
const PROOF_FRESHNESS: Duration = Duration::from_millis(250);
const WORKERS: usize = 4;

#[derive(Clone,Debug)]
struct Proof {
    completed: Instant,
    binding_epoch: u64,
    harness: kasa_pty::AgentKind,
}

#[derive(Clone)]
pub(crate) struct Commit {
    record: Record,
    pty: Arc<kasa_pty::PtySession>,
    revision: u64,
    proof: std::result::Result<Proof,String>,
}

#[derive(Default)]
struct Scheduler { cursor: Option<String>, active: HashSet<String> }

impl Scheduler {
    fn select(&mut self, pending: Vec<Record>, limit: usize) -> Vec<Record> {
        let mut recipients = BTreeMap::<String,Record>::new();
        for record in pending {
            if !self.active.contains(&record.address.surface_id) {
                recipients.entry(record.address.surface_id.clone()).or_insert(record);
            }
        }
        let mut ordered: Vec<_> = recipients.into_iter().collect();
        if let Some(cursor) = &self.cursor {
            let next = ordered.partition_point(|(surface,_)|surface <= cursor);
            let len = ordered.len();
            if len > 0 { ordered.rotate_left(next % len); }
        }
        ordered.into_iter().take(limit).map(|(surface,record)| {
            self.cursor = Some(surface.clone()); self.active.insert(surface); record
        }).collect()
    }
}

fn scheduler() -> &'static std::sync::Mutex<Scheduler> {
    static VALUE: std::sync::OnceLock<std::sync::Mutex<Scheduler>> = std::sync::OnceLock::new();
    VALUE.get_or_init(Default::default)
}

fn release(surface: &str) { scheduler().lock().unwrap().active.remove(surface); }

fn finish(record: &Record, state: State, reason: &str) {
    let record = record.clone(); let reason = reason.to_owned();
    std::thread::spawn(move || {
        let _ = kasa_mcp::tell_service::transition(&record.message_id,state,&reason);
        release(&record.address.surface_id);
    });
}

fn deadline_proof<T: Send + 'static>(deadline: Duration, collect: impl FnOnce() -> std::result::Result<T,String> + Send + 'static) -> std::result::Result<T,String> {
    static ACTIVE: AtomicUsize = AtomicUsize::new(0);
    if ACTIVE.fetch_update(Ordering::AcqRel,Ordering::Acquire,|n|(n < WORKERS).then_some(n+1)).is_err() {
        return Err("identity proof workers are occupied".into());
    }
    let (tx,rx) = std::sync::mpsc::channel();
    struct Permit(&'static AtomicUsize);
    impl Drop for Permit { fn drop(&mut self) { self.0.fetch_sub(1,Ordering::AcqRel); } }
    let permit = Permit(&ACTIVE);
    std::thread::spawn(move || {
        let _permit = permit;
        let result = collect();
        let _ = tx.send(result);
    });
    rx.recv_timeout(deadline).map_err(|_|"identity proof deadline expired".to_string())?
}

fn collect_proof(backend: Arc<crate::socket::PtyBackend>, record: Record, pty: Arc<kasa_pty::PtySession>) -> std::result::Result<Proof,String> {
    deadline_proof(PROOF_DEADLINE,move || {
        let epoch = backend.tell_binding_epoch(&record.address.surface_id);
        let identity = backend.collab_tell_identity(&record.address.surface_id).map_err(|e|e.to_string())?;
        if Address::parse(&identity).ok().as_ref() != Some(&record.address)
            || identity["agent_pid"].as_u64() != Some(record.receiver_agent_pid as u64)
            || backend.tell_binding_epoch(&record.address.surface_id) != epoch
            || !kasa_pty::lookup_session(&record.address.surface_id).is_some_and(|current|Arc::ptr_eq(&current,&pty)) {
            return Err("live target session, binding or PTY changed".into());
        }
        let harness = identity["harness"].as_str().and_then(kasa_pty::AgentKind::from_id)
            .filter(|kind|matches!(kind,kasa_pty::AgentKind::Claude | kasa_pty::AgentKind::Codex))
            .ok_or_else(||"unsupported harness identity".to_owned())?;
        Ok(Proof { completed:Instant::now(),binding_epoch:epoch,harness })
    })
}

fn begin_proof(backend: Arc<crate::socket::PtyBackend>, proxy: winit::event_loop::EventLoopProxy<UserEvent>, record: Record) {
    std::thread::spawn(move || {
        let Some(pty) = kasa_pty::lookup_session(&record.address.surface_id) else {
            finish(&record,State::Failed,"live target PTY disappeared"); return;
        };
        let revision = pty.input_revision();
        let proof = collect_proof(backend,record.clone(),pty.clone());
        if let Err(reason) = &proof {
            if reason.contains("occupied") || reason.contains("deadline") { release(&record.address.surface_id); }
            else { finish(&record,State::Failed,reason); }
            return;
        }
        if kasa_mcp::tell_service::transition(&record.message_id,State::Dispatching,"identity proven; awaiting guarded first write").is_err() {
            release(&record.address.surface_id); return;
        }
        let delivery = Commit {record,pty,revision,proof};
        if proxy.send_event(UserEvent::SafeTellReady(delivery.clone())).is_err() {
            finish(&delivery.record,State::Failed,"GUI stopped before the first write");
        }
    });
}
impl std::fmt::Debug for Commit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TellCommit").field("message_id",&self.record.message_id).finish()
    }
}

#[cfg(test)]
fn prompt_empty(cells: &[Vec<GridCell>], cursor_row: usize, harness: kasa_pty::AgentKind) -> bool {
    prompt_empty_at(cells,cursor_row,2,harness)
}

fn prompt_empty_at(cells: &[Vec<GridCell>], cursor_row: usize, cursor_col: usize, harness: kasa_pty::AgentKind) -> bool {
    use crate::screenread::PromptBox;
    use kasa_bridge::screen::Color;
    let Some(row) = cells.get(cursor_row) else { return false };
    let prompt = match harness { kasa_pty::AgentKind::Claude => '❯', kasa_pty::AgentKind::Codex => '›', _ => return false };
    let blank = |cell: &GridCell|cell.ch == '\0' || cell.ch.is_whitespace();
    let Some(marker) = row.iter().position(|cell|!blank(cell)) else { return false };
    if row[marker].ch != prompt { return false; }
    let region = crate::screenread::prompt_box(cells).and_then(|area| {
        let supported = matches!((&area,harness),
            (PromptBox::Bordered {..},kasa_pty::AgentKind::Claude)
            | (PromptBox::Filled {..},kasa_pty::AgentKind::Codex));
        let rows = area.rows();
        (supported && rows.contains(&cursor_row)).then_some(rows)
    });
    let mut placeholder = false;
    if let Some(first) = row.iter().enumerate().skip(marker+1).find(|(_,cell)|!blank(cell)).map(|(index,_)|index) {
        // A hint is not editable text: it starts at the insertion cursor,
        // has a distinct muted style and belongs to a recognized input box.
        let hint = &row[first];
        let muted = hint.dim || matches!(hint.fg,Color::Idx(8) | Color::Idx(240..=247))
            || matches!(hint.fg,Color::Rgb(r,g,b) if r == g && g == b && (80..=190).contains(&r));
        let distinct = hint.dim != row[marker].dim || hint.fg != row[marker].fg;
        placeholder = region.is_some() && cursor_col == first && muted && distinct
            && row[first..].iter().filter(|cell|!blank(cell)).all(|cell|
                cell.fg == hint.fg && cell.dim == hint.dim && !cell.bold && !cell.inverse && !cell.hidden);
        if !placeholder { return false; }
    }
    let input = region.clone().unwrap_or(cursor_row..cells.len());
    // Footer text is outside the bordered/filled input region. The identical
    // text inside that region remains a draft, regardless of its wording.
    for (index,row) in cells[input.clone()].iter().enumerate() {
        let absolute = input.start+index;
        if absolute == cursor_row { continue; }
        if row.iter().any(|cell|!blank(cell)) { return false; }
    }
    if !placeholder && row.iter().skip(marker+1).any(|cell|!blank(cell)) { return false; }
    let start = region.as_ref().map_or(cursor_row,|range|range.start);
    for row in cells[start.saturating_sub(3)..start].iter().rev() {
        let text: String = row.iter().map(|cell|cell.ch).collect();
        let lower = text.to_lowercase();
        if lower.contains("[image") || lower.contains("[attachment") || lower.contains("[pasted text")
            || lower.contains("image #") || text.contains('\u{fffc}') { return false; }
    }
    true
}

impl App {
    fn tell_composing(&self, surface: &str) -> bool {
        let owner = self.os_ime_surface.as_deref().or_else(||self.ime_focus.as_ref().and_then(crate::ImeFocus::terminal_surface));
        (self.in_preedit || !self.preedit.is_empty() || self.os_ime_surface.is_some())
            && owner.is_none_or(|owner|owner == surface || self.ws.lock().unwrap().active_tab_pid(owner) == surface)
    }

    fn tell_target_unchanged(&self, delivery: &Commit) -> bool {
        let Ok(proof) = &delivery.proof else { return false };
        self.socket_backend.as_ref().is_some_and(|backend|backend.tell_binding_epoch(&delivery.record.address.surface_id) == proof.binding_epoch)
            && kasa_mcp::surface_keys::get(&delivery.record.address.surface_id).as_deref() == Some(delivery.record.address.surface_key.as_str())
            && self.pty_for_pane(&delivery.record.address.surface_id).is_some_and(|current|Arc::ptr_eq(current,&delivery.pty))
            && !delivery.pty.input_closed()
    }

    fn tell_proof_current(&self, delivery: &Commit) -> bool {
        delivery.proof.as_ref().is_ok_and(|proof|proof.completed.elapsed() <= PROOF_FRESHNESS)
            && delivery.pty.input_revision() == delivery.revision
            && delivery.record.expires_at_ms > kasa_socket::tell::now_ms()
    }

    fn tell_ready(&self, record: &Record, pty: &kasa_pty::PtySession, harness: kasa_pty::AgentKind, empty: bool) -> bool {
        if pty.input_closed() || self.tell_composing(&record.address.surface_id) { return false; }
        let screen = pty.live_screen();
        if !screen.bracketed_paste { return false; }
        let mut cells = vec![Vec::new();screen.rows as usize];
        for (index,row) in &screen.dirty {
            if let Some(target) = cells.get_mut(*index as usize) { *target = row.clone(); }
        }
        if crate::input::rows_show_approval_prompt(&cells).is_some() { return false; }
        !empty || (!pty.input_draft_present() && pty.input_quiet_for(Duration::from_millis(300))
            && prompt_empty_at(&cells,screen.cursor_row as usize,screen.cursor_col as usize,harness))
    }

    pub(crate) fn safe_tell_tick(&mut self) {
        static LAST: std::sync::Mutex<Option<Instant>> = std::sync::Mutex::new(None);
        static BATCH_ACTIVE: AtomicBool = AtomicBool::new(false);
        {
            let Ok(mut last) = LAST.lock() else { return };
            if last.is_some_and(|at|at.elapsed() < Duration::from_millis(500)) { return; }
            *last = Some(Instant::now());
        }
        let Some(backend) = self.socket_backend.clone() else { return };
        if BATCH_ACTIVE.swap(true,Ordering::AcqRel) { return; }
        let proxy = self.proxy.clone();
        std::thread::spawn(move || {
            if let Ok(pending) = kasa_mcp::tell_service::pending() {
                let selected = scheduler().lock().unwrap().select(pending,WORKERS);
                for record in selected { begin_proof(backend.clone(),proxy.clone(),record); }
            }
            BATCH_ACTIVE.store(false,Ordering::Release);
        });
    }

    pub(crate) fn safe_tell_ready(&mut self, delivery: &Commit) {
        if !self.tell_target_unchanged(delivery) {
            finish(&delivery.record,State::Failed,"target generation or PTY changed before the first write");
            return;
        }
        let proof = delivery.proof.as_ref().unwrap();
        if !self.tell_proof_current(delivery)
            || !self.tell_ready(&delivery.record,&delivery.pty,proof.harness,true) {
            let state = if delivery.record.reject_if_busy { State::Failed } else { State::Accepted };
            finish(&delivery.record,state,"no bytes written; waiting for fresh identity and an empty input without approval, attachments or composition");
            return;
        }
        let payload = format!("\x1b[200~{}\x1b[201~",delivery.record.body);
        let revision = match delivery.pty.send_bytes_guarded(payload.as_bytes(),Some(delivery.revision)) {
            Ok(revision) => revision,
            Err(_) => { finish(&delivery.record,State::Uncertain,"paste write failed or input changed; automatic retry prohibited"); return; }
        };
        let Some(backend) = self.socket_backend.clone() else {
            finish(&delivery.record,State::Uncertain,"receiver disappeared after paste"); return;
        };
        let proxy = self.proxy.clone();
        let mut commit = delivery.clone(); commit.revision = revision;
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(160));
            commit.proof = collect_proof(backend,commit.record.clone(),commit.pty.clone());
            if proxy.send_event(UserEvent::SafeTellCommit(commit.clone())).is_err() {
                finish(&commit.record,State::Uncertain,"GUI stopped after paste; automatic retry prohibited");
            }
        });
    }

    pub(crate) fn safe_tell_commit(&mut self, commit: &Commit) {
        let unchanged = self.tell_target_unchanged(commit)
            && self.tell_proof_current(commit)
            && commit.proof.as_ref().is_ok_and(|proof|self.tell_ready(&commit.record,&commit.pty,proof.harness,false));
        let tail = commit.pty.visible_text(30);
        let compact = |text: &str|text.chars().filter(|c|!c.is_whitespace()).collect::<String>();
        let probe = compact(&commit.record.body);
        let echoed = compact(&tail).contains(&probe) || tail.contains("[Pasted text #");
        if !unchanged || !echoed {
            finish(&commit.record,State::Uncertain,"input, session, prompt or paste confirmation changed; Enter withheld");
            return;
        }
        let result = commit.pty.send_bytes_guarded(b"\r",Some(commit.revision));
        let (state,reason) = if result.is_ok() { (State::Submitted,"paste and Enter writes succeeded; model read is unconfirmed") }
            else { (State::Uncertain,"Enter write unconfirmed; automatic retry prohibited") };
        finish(&commit.record,state,reason);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn row(text: &str) -> Vec<GridCell> { text.chars().map(|ch|GridCell {ch,..GridCell::blank()}).collect() }
    fn record(surface: &str) -> Record {
        Record {message_id:kasa_socket::tell::new_message_id(),address:Address {
            machine_id:"fixture-local-machine".into(),surface_key:surface.into(),surface_id:surface.into(),
            session_id:"session".into(),instance_id:"instance".into()},body:"hello".into(),
            body_hash:kasa_socket::tell::fingerprint("hello"),state:State::Accepted,reason:String::new(),
            accepted_at_ms:0,updated_at_ms:0,expires_at_ms:u64::MAX,reject_if_busy:false,receiver_agent_pid:1}
    }

    #[test]
    fn multiline_draft_attachments_and_unknown_placeholder_never_count_as_empty() {
        use kasa_pty::AgentKind::Codex;
        assert!(!prompt_empty(&[row("›"),row("  second draft line")],0,Codex));
        assert!(!prompt_empty(&[row("›"),row("────────────────────"),row("draft beyond separator")],0,Codex));
        assert!(!prompt_empty(&[row("[Image #1]"),row("›")],1,Codex));
        assert!(!prompt_empty(&[row("› Find and fix a bug")],0,Codex));
        assert!(!prompt_empty(&[row("›"),row("unverified footer or draft")],0,Codex));
    }

    fn input_box(harness: kasa_pty::AgentKind, input: &[String], footer: &str) -> Vec<Vec<GridCell>> {
        let wide = |text: &str| { let mut row = row(text); row.resize(90,GridCell::blank()); row };
        let filled = |text: &str| {
            let mut row = wide(text);
            for cell in &mut row { cell.bg = kasa_bridge::screen::Color::Rgb(63,69,77); }
            row
        };
        let mut rows = vec![wide("Thinking… (esc to interrupt)")];
        match harness {
            kasa_pty::AgentKind::Claude => {
                rows.push(wide(&"─".repeat(90)));
                rows.extend(input.iter().map(|text|wide(text)));
                rows.push(wide(&"─".repeat(90)));
            }
            kasa_pty::AgentKind::Codex => {
                rows.push(filled(""));
                rows.extend(input.iter().map(|text|filled(text)));
                rows.push(filled(""));
            }
            _ => unreachable!(),
        }
        rows.push(wide(footer)); rows.push(wide("")); rows
    }

    #[test]
    fn real_input_boundaries_allow_footers_but_protect_identical_draft_text() {
        use kasa_pty::AgentKind::{Claude,Codex};
        for (harness,prompt,footer) in [
            (Claude,"❯","  bypass permissions on (shift+tab to cycle)"),
            (Codex,"›","  gpt-5.5 medium · tmuxify · main · Ask for approval · Context 3% used"),
        ] {
            let empty = input_box(harness,&[prompt.into()],footer);
            assert!(prompt_empty_at(&empty,2,2,harness));
            let draft = input_box(harness,&[prompt.into(),footer.into()],footer);
            assert!(!prompt_empty_at(&draft,2,2,harness));
            let multiline = input_box(harness,&[prompt.into(),"  first draft line".into(),"  second line".into()],footer);
            assert!(!prompt_empty_at(&multiline,2,2,harness));
            let attached = input_box(harness,&[prompt.into(),"  [Image #1]".into()],footer);
            assert!(!prompt_empty_at(&attached,2,2,harness));
            let mut pending_attachment = empty.clone();
            pending_attachment[0] = row("[Attachment: image.png]");
            assert!(!prompt_empty_at(&pending_attachment,2,2,harness));
            // Text-only slices lack a real box edge and cannot prove footer ownership.
            assert!(!prompt_empty_at(&[row(prompt),row(footer)],0,2,harness));
        }
    }

    #[test]
    fn styled_placeholder_requires_input_geometry_and_the_insertion_cursor() {
        use kasa_pty::AgentKind::{Claude,Codex};
        for (harness,prompt,hint,footer) in [
            (Claude,"❯","Try \"fix lint errors\"","  bypass permissions on (shift+tab to cycle)"),
            (Codex,"›","Run /review on my current changes","  gpt-5.5 medium · main · Context 3% used"),
        ] {
            let text = format!("{prompt} {hint}");
            let typed = input_box(harness,&[text.clone()],footer);
            assert!(!prompt_empty_at(&typed,2,2,harness));
            let mut placeholder = typed.clone();
            for cell in placeholder[2].iter_mut().skip(2) {
                cell.fg = kasa_bridge::screen::Color::Idx(8); cell.dim = true;
            }
            assert!(prompt_empty_at(&placeholder,2,2,harness));
            assert!(!prompt_empty_at(&placeholder,2,text.chars().count(),harness));
            let continuation = input_box(harness,&[prompt.into(),hint.into()],footer);
            assert!(!prompt_empty_at(&continuation,2,2,harness));
            let no_geometry = vec![placeholder[2].clone()];
            if harness == Claude { assert!(!prompt_empty_at(&no_geometry,0,2,harness)); }
        }
    }

    #[test]
    fn approval_and_question_options_inside_real_input_regions_remain_blocked() {
        use kasa_pty::AgentKind::{Claude,Codex};
        for (harness,prompt) in [(Claude,"❯"),(Codex,"›")] {
            for option in ["1. Yes","1. First option"] {
                let rows = input_box(harness,&[format!("{prompt} {option}"),"  2. Second option".into()],"status");
                assert!(!prompt_empty_at(&rows,2,2,harness));
                assert!(crate::input::rows_show_approval_prompt(&rows).is_some());
            }
        }
    }

    #[test]
    fn working_output_allows_empty_input_while_approval_and_question_are_blocked() {
        use kasa_pty::AgentKind::{Claude,Codex};
        for (harness,prompt) in [(Claude,"❯"),(Codex,"›")] {
            let cells = [row("Thinking... build running"),row(prompt)];
            assert!(prompt_empty(&cells,1,harness));
            assert!(crate::input::rows_show_approval_prompt(&cells).is_none());
            let cells = [row("Which option should be used?"),row(&format!("{prompt} 1. First"))];
            assert!(!prompt_empty(&cells,1,harness));
            assert!(crate::input::rows_show_approval_prompt(&cells).is_some());
        }
        assert!(!prompt_empty(&[row("$ ")],0,Claude));
    }

    #[test]
    fn eight_waiting_recipients_do_not_starve_the_ninth_ready_recipient() {
        let pending: Vec<_> = (0..9).map(|n|record(&format!("%{n:02}"))).collect();
        let mut scheduler = Scheduler::default();
        let mut seen = HashSet::new();
        for _ in 0..3 {
            for record in scheduler.select(pending.clone(),WORKERS) { seen.insert(record.address.surface_id); }
            scheduler.active.clear();
        }
        assert!(seen.contains("%08"));
        assert_eq!(seen.len(),9);
    }

    #[test]
    fn recipient_is_serialized_while_another_recipient_remains_eligible() {
        let mut scheduler = Scheduler::default();
        let selected = scheduler.select(vec![record("%1"),record("%1"),record("%2")],WORKERS);
        assert_eq!(selected.len(),2);
        assert!(scheduler.select(vec![record("%1"),record("%2")],WORKERS).is_empty());
        assert_eq!(scheduler.select(vec![record("%3")],WORKERS).len(),1);
    }

    #[derive(Clone)]
    struct Capture(Arc<std::sync::Mutex<Vec<Vec<u8>>>>);
    impl std::io::Write for Capture {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> { self.0.lock().unwrap().push(bytes.to_vec()); Ok(bytes.len()) }
        fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
    }
    fn fake_pty() -> (Arc<kasa_pty::PtySession>,Capture,crossbeam_channel::Sender<kasa_pty::ExtEvent>) {
        let (events,receiver) = crossbeam_channel::unbounded();
        let capture = Capture(Default::default());
        let session = kasa_pty::PtySession::start_external(kasa_pty::PtyOptions {
            pane_id:format!("tell-proof-{}",kasa_socket::tell::new_message_id()),cols:40,rows:8,..Default::default()
        },kasa_pty::ExternalIo {events:receiver,writer:Box::new(capture.clone()),on_resize:Arc::new(|_,_|{})}).unwrap();
        (Arc::new(session),capture,events)
    }

    #[test]
    fn deadline_worker_returns_evidence_off_thread_without_late_pty_input() {
        let (pty,capture,_events) = fake_pty();
        let caller = std::thread::current().id();
        let other = deadline_proof(Duration::from_secs(1),||Ok(std::thread::current().id())).unwrap();
        assert_ne!(caller,other);
        let revision = pty.input_revision();
        let observed = pty.clone();
        let late = deadline_proof(Duration::from_millis(10),move || {
            std::thread::sleep(Duration::from_millis(80)); Ok(observed.input_revision())
        });
        assert!(late.unwrap_err().contains("deadline"));
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(pty.input_revision(),revision);
        assert!(capture.0.lock().unwrap().is_empty());
    }

    #[test]
    fn fake_pty_retains_multiline_and_pending_attachment_drafts() {
        let (pty,capture,_events) = fake_pty();
        pty.send_bytes(b"\r").unwrap();
        assert!(!pty.input_draft_present());
        pty.send_bytes(b"first\nsecond").unwrap();
        assert!(pty.input_draft_present());
        pty.send_bytes(b"\r").unwrap();
        let revision = pty.input_revision();
        pty.reserve_input_draft();
        assert!(pty.input_draft_present());
        assert!(pty.send_bytes_guarded(b"message",Some(revision)).is_err());
        assert_eq!(capture.0.lock().unwrap().as_slice(),[b"\r".to_vec(),b"first\nsecond".to_vec(),b"\r".to_vec()]);
    }
}
