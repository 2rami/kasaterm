//! 세션 복원 — 저장본을 읽어 방·칸을 다시 세우고, 에이전트는 같은 하네스·같은 대화로
//! 되살린다. 복원 전에 묻는 수(`count_*`)와 학생 예약도 저장본을 읽는 여기 있다.
use super::*;

pub(super) fn latest_restored_state(dir: &std::path::Path) -> Option<serde_json::Value> {
    let mut backups: Vec<_> = std::fs::read_dir(dir).ok()?.flatten().filter_map(|entry| {
        let name = entry.file_name();
        let name = name.to_str()?;
        let stamp = name.strip_prefix("session-restored-")?.strip_suffix(".json")?
            .parse::<u64>().ok()?;
        let metadata = entry.metadata().ok()?;
        if !metadata.is_file() { return None; }
        Some((stamp, entry.path()))
    }).collect();
    // Filename timestamps reflect restore order even if backups were copied
    // later and acquired different mtimes. Propagate identities through every
    // intermediate legacy snapshot, not just the already-renumbered last one.
    backups.sort_by_key(|(stamp, _)| *stamp);
    let mut previous = None;
    for (_, path) in backups {
        let state = std::fs::read(path).ok()
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
            .filter(serde_json::Value::is_object);
        // A corrupt checkpoint breaks the lineage. Later valid checkpoints
        // start independently; a corrupt final checkpoint returns None.
        previous = state.map(|state| kasa_mcp::surface_keys::prepare_restore_state(&state, previous.as_ref()));
    }
    previous
}

pub(super) fn contains_saved_codex(node: &serde_json::Value) -> bool {
    match node {
        serde_json::Value::Object(object) => object.get("was_agent").and_then(|v| v.as_str()) == Some("codex")
            || object.values().any(contains_saved_codex),
        serde_json::Value::Array(array) => array.iter().any(contains_saved_codex),
        _ => false,
    }
}

pub(super) struct RestoredSurfaceKey {
    pub(super) id: String,
    pub(super) committed: bool,
}

impl RestoredSurfaceKey {
    pub(super) fn register(id: &str, record: &serde_json::Value) -> Self {
        if let Some(key) = record.get("surface_key").and_then(|v| v.as_str()).filter(|s| !s.trim().is_empty()) {
            kasa_mcp::surface_keys::set(id, key);
        } else {
            kasa_mcp::surface_keys::ensure(id);
        }
        Self { id: id.into(), committed: false }
    }
}

impl Drop for RestoredSurfaceKey {
    fn drop(&mut self) {
        if !self.committed {
            // Failed restore must not leave a key for a later unrelated shell
            // that happens to reuse the still-free pane number.
            kasa_mcp::surface_keys::remove(&self.id);
        }
    }
}

impl App {
    /// Count leaves that were running claude across the whole saved state — the
    /// number the restore prompt shows. 총 pane 수는 `count_panes`.
    ///
    /// 예전엔 `character` 가 붙어 있으면 claude pane 으로 셌다. 캐릭터는 claude
    /// 여부와 무관하게 **spawn 때 모든 pane 에 배정**되므로(assign_character_env),
    /// 순수 셸 3개짜리 창이 "claude 세션 3개"로 표시됐다. 감지 실패 보정은
    /// session_id 로 한다 — 그건 claude 가 실제로 세션을 바인딩했을 때만 붙어,
    /// 저장 시점에 claude 가 포그라운드가 아니어도 남는다.
    pub(crate) fn count_claude_panes(state: &serde_json::Value) -> usize {
        fn walk(node: &serde_json::Value, n: &mut usize) {
            if let Some(leaf) = node.get("leaf") {
                if crate::internal_room::is_saved_window(node) {
                    return;
                }
                // 이 함수는 복원 확인창을 그릴 때마다 불린다. 디스크 rollout 확인은
                // 실제 저장·복원 시 한 번만 하고, 여기서는 저장본 표식과 이미 결속된
                // session_id만 센다.
                let was_agent = saved_agent_marker(leaf).is_some();
                let bound_sid = leaf
                    .get("session_id")
                    .and_then(|c| c.as_str())
                    .is_some_and(|s| !s.is_empty());
                if was_agent || bound_sid {
                    *n += 1;
                }
            } else if let Some(split) = node.get("split") {
                if let Some(a) = split.get("a") {
                    walk(a, n);
                }
                if let Some(b) = split.get("b") {
                    walk(b, n);
                }
            }
        }
        let mut n = 0;
        if let Some(sessions) = state.get("sessions").and_then(|s| s.as_array()) {
            for s in sessions {
                if let Some(windows) = s.get("windows").and_then(|w| w.as_array()) {
                    for w in windows {
                        walk(w, &mut n);
                    }
                }
                for u in s.get("undocked").and_then(|u| u.as_array()).into_iter().flatten() {
                    walk(u, &mut n);
                }
            }
        }
        n
    }
    /// 저장된 상태의 전체 pane(leaf) 수. claude 가 하나도 없는 순수 셸 작업 공간도
    /// 레이아웃·스크롤백은 복원할 값이 있으므로, 프롬프트를 띄울지는 이 수로 정한다
    /// (claude 수로 정하면 셸만 쓰던 창은 강제 종료 후 아무것도 못 되살린다).
    pub(crate) fn count_panes(state: &serde_json::Value) -> usize {
        fn walk(node: &serde_json::Value, n: &mut usize) {
            if node.get("leaf").is_some() {
                if !crate::internal_room::is_saved_window(node) {
                    *n += 1;
                }
            } else if let Some(split) = node.get("split") {
                if let Some(a) = split.get("a") {
                    walk(a, n);
                }
                if let Some(b) = split.get("b") {
                    walk(b, n);
                }
            }
        }
        let mut n = 0;
        if let Some(sessions) = state.get("sessions").and_then(|s| s.as_array()) {
            for s in sessions {
                if let Some(windows) = s.get("windows").and_then(|w| w.as_array()) {
                    for w in windows {
                        walk(w, &mut n);
                    }
                }
                for u in s.get("undocked").and_then(|u| u.as_array()).into_iter().flatten() {
                    walk(u, &mut n);
                }
            }
        }
        n
    }
    /// Rebuild the workspace saved by `save_session_state` (user chose 복원):
    /// recreate each window's split layout, spawn a pane per leaf seeded with
    /// its saved scrollback, and queue `claude --resume <id>` for panes that
    /// were running claude so the conversation — and, via the shim, the student
    /// identity — comes back.
    ///
    /// Only the active session's windows are restored into the live fields;
    /// detached sessions aren't wired up (`self.sessions` is always `[None]`),
    /// so the saved `sessions` array carries exactly one entry in practice.
    /// 복원 중 「저장된 학생을 먼저 잡아 둔」 자리의 키 앞머리. 실제 pane id 는
    /// `%` 로 시작하므로 이 앞머리와는 절대 겹치지 않는다.
    pub(super) const RESERVE_KEY: &'static str = "\u{0}restore-reserve-";

    /// 저장본에서 되살릴 세션의 창 목록과 활성 창 번호. 복원과 학생 예약이 **같은
    /// 창을** 보도록 한 곳에 둔다 — 둘이 다른 세션을 읽으면 예약이 엉뚱한 이름을
    /// 지키고 정작 되살아나는 학생은 밀린다.
    pub(super) fn saved_windows(state: &serde_json::Value) -> Option<(&[serde_json::Value], usize)> {
        let sessions = state.get("sessions")?.as_array()?;
        let active = state
            .get("active_session")
            .and_then(|n| n.as_u64())
            .unwrap_or(0) as usize;
        let session = sessions.get(active).or_else(|| sessions.first())?;
        let windows = session.get("windows")?.as_array()?;
        let active_window = session
            .get("active_window")
            .and_then(|n| n.as_u64())
            .unwrap_or(0) as usize;
        Some((windows.as_slice(), active_window))
    }

    /// 활성 세션의 별도창 기록(`undocked`) — `saved_windows` 와 같은 세션을 본다.
    pub(super) fn saved_undocked(state: &serde_json::Value) -> &[serde_json::Value] {
        let active = state
            .get("active_session")
            .and_then(|n| n.as_u64())
            .unwrap_or(0) as usize;
        state
            .get("sessions")
            .and_then(|s| s.as_array())
            .and_then(|sessions| sessions.get(active).or_else(|| sessions.first()))
            .and_then(|s| s.get("undocked"))
            .and_then(|u| u.as_array())
            .map_or(&[], |v| v.as_slice())
    }

    /// 활성 세션의 방별 본문 보기(`room_body`) — `saved_windows` 와 같은 세션을 본다.
    /// 없는 방은 담기지 않는다(전역 기본을 따른다는 뜻).
    pub(crate) fn saved_room_bodies(state: &serde_json::Value) -> Vec<(usize, bool)> {
        let active = state
            .get("active_session")
            .and_then(|n| n.as_u64())
            .unwrap_or(0) as usize;
        state
            .get("sessions")
            .and_then(|s| s.as_array())
            .and_then(|sessions| sessions.get(active).or_else(|| sessions.first()))
            .and_then(|s| s.get("room_body"))
            .and_then(|m| m.as_object())
            .map(|m| {
                m.iter()
                    .filter_map(|(k, v)| Some((k.parse().ok()?, v.as_str()? == "list")))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// 저장본의 leaf 가 쥐고 있던 학생 이름 — 트리 순서대로, 빈 이름은 뺀다.
    pub(crate) fn saved_characters(state: &serde_json::Value) -> Vec<String> {
        fn walk(n: &serde_json::Value, out: &mut Vec<String>) {
            if let Some(leaf) = n.get("leaf") {
                if crate::internal_room::is_saved_window(n) {
                    return;
                }
                if let Some(c) = leaf
                    .get("character")
                    .and_then(|c| c.as_str())
                    .filter(|s| !s.is_empty())
                {
                    out.push(c.to_string());
                }
                return;
            }
            if let Some(sp) = n.get("split") {
                if let Some(a) = sp.get("a") {
                    walk(a, out);
                }
                if let Some(b) = sp.get("b") {
                    walk(b, out);
                }
            }
        }
        let mut out = Vec::new();
        if let Some((windows, _)) = Self::saved_windows(state) {
            for w in windows {
                walk(w, &mut out);
            }
        }
        for u in Self::saved_undocked(state) {
            walk(u, &mut out);
        }
        out
    }

    /// 저장본의 학생을 전부 「쓰는 중」으로 걸어 둔다. `assign_character_env` 의
    /// 중복 회피는 `ws.pane_character` 의 **값**을 보므로 키가 실제 pane 이 아니어도
    /// 되고, 그래서 이 예약 키는 pane 번호와 겹칠 수 없는 꼴이다.
    ///
    /// 부팅(`resumed`)이 첫 pane 을 띄우기 전에 부르고, 복원이 한 번 더 부른다(같은
    /// 키에 같은 이름이라 두 번 걸어도 하나다). 걷는 쪽은
    /// `release_reserved_characters` — 복원 끝, 「새로 시작」, 닫기.
    pub(crate) fn reserve_saved_characters(&self, state: &serde_json::Value) {
        let mut ws = self.ws.lock().unwrap();
        for (i, c) in Self::saved_characters(state).into_iter().enumerate() {
            ws.pane_character
                .insert(format!("{}{i}", Self::RESERVE_KEY), c);
        }
    }

    /// 예약을 걷는다. 남겨 두면 그 이름들이 영영 taken 으로 잡혀, 앞으로 새로
    /// 쪼개는 pane 이 그 학생을 못 쓴다.
    pub(crate) fn release_reserved_characters(&self) {
        self.ws
            .lock()
            .unwrap()
            .pane_character
            .retain(|k, _| !k.starts_with(Self::RESERVE_KEY));
    }

    pub(crate) fn restore_session_state(&mut self, state: &serde_json::Value) {
        // Read the previous identity table before this restore writes its own
        // backup. The configured session directory also isolates test fixtures.
        let previous = crate::socket::session_file_path()
            .and_then(|path| path.parent().and_then(latest_restored_state));
        let mut prepared = kasa_mcp::surface_keys::prepare_restore_state(state, previous.as_ref());
        crate::server_restore::import_pending_servers(&mut prepared);
        let state = &prepared;
        self.restore_progress = Some(crate::restore_progress::RestoreProgress::new(state.clone()));
        // codex 는 재시작마다 옛 pid 의 pane 홈 경로를 물고 있어 `resume` 이 죽는다 —
        // 되살리기 전에 그 색인을 실체 자리로 고친다(2026-09-08, 아래 함수 주석).
        let fixed = if crate::verification_run() || !contains_saved_codex(state) { 0 } else { crate::socket::codex_repair_thread_paths() };
        if fixed > 0 {
            eprintln!("[restore] codex rollout 경로 {fixed}줄을 실체 자리로 고침");
        }
        // 복원 직전 저장본을 곁에 남긴다 — 복원에서 창이 빠졌을 때(2026-08-30
        // 「재시작했는데 세션이 없어진 것 같다」, %0 미도리가 소리 없이 빠짐)
        // 무엇이 저장돼 있었는지 되짚을 증거가 이것뿐이다. 원본 session.json 은
        // 몇 초 안에 현재 상태로 덮여 사라진다. 최근 5벌만 남긴다.
        // 저장본이 놓인 폴더에 함께 둔다 — `home_dir()` 를 직접 부르면
        // `KASATERM_SESSION_FILE` 로 격리한 검증 실행까지 사람의 config 에
        // 백업을 쌓고, 5벌 상한에 걸려 **사람의 백업을 밀어낸다**
        // (2026-09-01 실측: 5벌 중 3벌이 검증 실행 것이었다).
        if let Some(dir) =
            crate::socket::session_file_path().and_then(|p| p.parent().map(|d| d.to_path_buf()))
        {
            let ts = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let _ = std::fs::write(
                dir.join(format!("session-restored-{ts}.json")),
                state.to_string(),
            );
            let _ = kasa_socket::session_storage::prune_restored(&dir, 5);
        }
        // 저장본이 형식 밖이면 부팅 때 걸어 둔 예약도 여기서 푼다 — 안 풀면 복원은
        // 안 됐는데 그 이름들만 영영 taken 으로 남는다.
        let Some((windows, active_window)) = Self::saved_windows(state) else {
            self.release_reserved_characters();
            if let Some(progress) = self.restore_progress.as_mut() {
                progress.failure = Some("저장된 창 목록을 읽지 못했어요".into());
            }
            return;
        };
        // Tear down the blank session start_pty just spawned: drop its PTY and
        // clear its pane state so the rebuilt layout starts from an empty slate.
        // The socket server (start_socket_pty) stays up — only panes are rebuilt.
        self.pending_restores.clear();
        self.pty.clear();
        {
            let mut ws = self.ws.lock().unwrap();
            ws.panes.clear();
            ws.active_pane = None;
        }
        self.pty_layout = None;
        self.windows.clear();
        self.settings_scene.leave();
        // ── 저장된 배정 먼저 잡아 두기 ────────────────────────────────────
        // 복원은 leaf 를 하나씩 되살리는데, 저장된 학생을 **그 차례가 와야** 잡는다.
        // 그래서 앞 차례의 leaf 가 새로 배정받다가 뒤 leaf 의 학생을 집어가면, 뒤
        // leaf 는 「이미 다른 자리가 쓰는 중」으로 밀려 엉뚱한 학생이 된다 —
        // 대화는 `--resume` 으로 그대로 이어지는데 이름·얼굴·말투만 갈린다
        // (2026-08-28 실측: 재시작 한 번에 우사기 자리가 리오가 됐고, 정작 우사기는
        // 어느 자리에도 없었다 — 밀어낸 쪽이 그 뒤 사라진 것이다).
        //
        // 그래서 되살릴 학생을 **전부 먼저** 예약한다. 부팅(`resumed`)이 첫 pane 을
        // 띄우기 전에 이미 같은 예약을 걸어 두는데(그 첫 pane 이 저장본의 이름을
        // 집던 2026-09-03 사고), 복원이 그 길 밖에서 불릴 수도 있어 여기서 한 번 더
        // 건다 — 같은 키·같은 이름이라 겹쳐도 하나다. 복원이 끝나면 걷는다(아래).
        self.reserve_saved_characters(state);
        let (cols, rows) = self.window_cells();
        let mut restored_active = 0usize;
        for (j, w) in windows.iter().enumerate() {
            if crate::internal_room::is_saved_window(w) {
                continue;
            }
            let tree = self.restore_window_layout(w, cols, rows);
            if j == active_window {
                restored_active = self.windows.len();
                self.pty_layout = tree;
                self.windows.push(None);
            } else {
                self.windows.push(tree);
            }
        }
        self.active_window = restored_active.min(self.windows.len().saturating_sub(1));
        // 활성 방이 하나도 안 살아났을 때 화면이 비지 않게 메운다.
        //
        // ⚠️ **다른 방은 절대 건드리지 마라.** 예전에는 여기서 `windows` 를 통째로
        // `vec![None]` 으로 갈아 버려, 활성 방 하나가 못 살아난 것만으로 **멀쩡히
        // 복원된 나머지 방이 전부 사라졌다.** 2026-08-28 에 실제로 그렇게 잃었다:
        // 원격으로 이사 보낸 학생들이 활성 방에 몰려 있었는데 그 기계에 못 닿자,
        // 방 여섯 개가 빈 pane 하나로 뭉개지고 앱이 그 상태를 그대로 저장해
        // 「어느 학생이 어느 방에 있었는지」까지 지워졌다.
        if self.pty_layout.is_none() {
            if self.windows.is_empty() {
                self.windows.push(None);
                self.active_window = 0;
            } else if let Some(tree) = self
                .windows
                .get_mut(self.active_window)
                .and_then(Option::take)
            {
                // 인덱스가 밀려 살아 있는 방을 가리키게 된 경우다. 그 방을 활성으로
                // 올린다 — 살아난 방을 두고 빈 pane 을 띄우는 쪽이 더 나쁘다.
                self.pty_layout = Some(tree);
            }
            if self.pty_layout.is_none() {
                let _ = self.spawn_session_pane();
            }
        }
        if let Some(first) = self
            .pty_layout
            .as_ref()
            .and_then(|l| l.leaves().first().map(|s| s.to_string()))
        {
            self.ws.lock().unwrap().active_pane = Some(first);
        }
        // 방별 본문 보기(목록/배치도)도 방 번호가 굳은 지금 되살린다 — 고른 것이
        // 재시작에 전역 기본으로 되돌아가면 방마다 따로 둔 뜻이 없어진다.
        self.room_list_body = Self::saved_room_bodies(state).into_iter().collect();
        // 별도창으로 뗀 pane — 트리에 안 꽂고 pane 만 살린 뒤(셸·`--resume` 큐잉은
        // leaf 와 같은 길) 창은 다음 틱의 `flush_aux_opens` 가 연다(여기엔 event
        // loop 가 없다). 방 번호는 방 복원이 끝나 인덱스가 굳은 지금 환산한다.
        for u in Self::saved_undocked(state) {
            if crate::internal_room::is_saved_window(u) {
                continue;
            }
            let Some(leaf) = u.get("leaf").filter(|l| !l.is_null()) else {
                continue;
            };
            let Some(id) = self.restore_leaf(leaf, cols, rows) else {
                continue;
            };
            let home = (u.get("home_window").and_then(|v| v.as_u64()).unwrap_or(0) as usize)
                .min(self.windows.len().saturating_sub(1));
            let frame = u
                .get("frame")
                .cloned()
                .and_then(|f| serde_json::from_value(f).ok());
            self.queue_aux_terminal(id, home, frame);
        }
        self.release_reserved_characters();
        // 숨겨 둔 pane 을 되살리기 목록으로 되돌린다. **띄우지는 않는다** — 숨긴
        // 것은 화면에 없는 게 그 사람이 고른 상태고, 켜자마자 우르르 튀어나오면
        // 숨긴 의미가 없다. 목록에 서 있다가 사용자가 부를 때 뜬다.
        //
        // `alive: false` 로 들어간다 — 재시작으로 PTY 는 다 죽었으니 되살리기는
        // 레코드로 새로 띄우는 쪽이다. claude 였던 pane 은 `rec` 의 세션 id 를
        // `restore_leaf` 가 `--resume` 으로 이어 준다.
        for c in state
            .get("stashed_panes")
            .and_then(|v| v.as_array())
            .into_iter()
            .flatten()
        {
            let Some(rec) = c.get("rec").cloned().filter(|r| !r.is_null()) else {
                continue;
            };
            let str_of = |k: &str| {
                c.get(k)
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string()
            };
            self.closed_panes.push(crate::ClosedPane {
                rec,
                pane_id: str_of("pane_id"),
                character: str_of("character"),
                folder: str_of("folder"),
                neighbor: c
                    .get("neighbor")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()),
                // 방 번호는 저장 당시 것이다. 그 방이 이번에 안 살아났으면 되살리기가
                // 활성 방으로 떨어뜨린다(`window` 를 쓰는 쪽의 기존 규칙).
                window: c.get("window").and_then(|v| v.as_u64()).unwrap_or(0) as usize,
                alive: false,
                stashed: c.get("stashed").and_then(|v| v.as_bool()).unwrap_or(true),
                idle_since: None,
                preview: None,
            });
        }
        // 재접속 자동 따라잡기 — 이 기계가 꺼져 있는 동안 원격(본진)에 쌓인
        // 미push 커밋·미저장 변경을, 거울이 다시 붙은 레포마다 이사와 같은
        // 관문으로 끌어와 로컬을 따라잡는다(2026-09-02 감사 ②). origin 에 이미
        // push 된 것은 여기 소관이 아니다 — 그건 평범한 git pull 의 영역이고,
        // 이 층은 push 안 된 것만 나른다(이사 동율과 같은 철학). 부팅을 세우지
        // 않게 백그라운드로 돌고, 막히면(로컬 미저장 등) 덮지 않고 incoming
        // 보관(OnBlock::Deposit)이라 잃는 것이 없다.
        {
            let registry = kasa_mcp::machines::machines();
            let mut seen = std::collections::HashSet::new();
            let mut catchup: Vec<(String, String, String, String)> = Vec::new();
            for id in kasa_pty::live_sessions() {
                let Some(info) = kasa_mcp::remote::remote_info(&id) else {
                    continue;
                };
                let Some(rcwd) = info.remote_cwd.clone() else {
                    continue;
                };
                let Some(m) = registry
                    .iter()
                    .find(|m| m.base == info.base.trim_end_matches('/'))
                else {
                    continue;
                };
                let Some(local) = info
                    .origin_cwd
                    .clone()
                    .or_else(|| kasa_mcp::machines::map_remote_to_local(m, &rcwd))
                else {
                    continue;
                };
                if !std::path::Path::new(&local).join(".git").exists() {
                    continue;
                }
                if seen.insert(local.clone()) {
                    catchup.push((info.base.clone(), m.label.clone(), rcwd, local));
                }
            }
            if !catchup.is_empty() {
                let proxy = self.proxy.clone();
                std::thread::spawn(move || {
                    for (base, label, rcwd, local) in catchup {
                        let tail = local.rsplit('/').next().unwrap_or(&local).to_string();
                        let msg = match kasa_mcp::remote::fetch_repo_sync(&base, &rcwd, None) {
                            Ok(kasa_mcp::remote::RepoSyncFetch::Bundle(meta, bytes)) => {
                                match kasa_mcp::reposync::apply(
                                    std::path::Path::new(&local),
                                    &bytes,
                                    &meta.head,
                                    &meta.sync,
                                    &meta.branch,
                                    meta.dirty,
                                    false,
                                    kasa_mcp::reposync::OnBlock::Deposit,
                                ) {
                                    Ok(what) => format!("{label} 따라잡기({tail}): {what}"),
                                    Err(e) => {
                                        format!("{label} 따라잡기 실패({tail}): {e:#}")
                                    }
                                }
                            }
                            // 최신이거나 낡은 기계 — 조용히. 낡음 경고는 이사 탭 몫이다.
                            Ok(_) => continue,
                            Err(e) => format!("{label} 따라잡기 실패({tail}): {e:#}"),
                        };
                        let _ = proxy.send_event(crate::UserEvent::RepoCatchup(msg));
                    }
                });
            }
        }
        if let Some(progress) = self.restore_progress.as_mut() { progress.built = true; }
        self.weather_restore_overrides(state);
        self.chrome_dirty = true;
        self.resize_backend(cols, rows);
        self.publish_pty_layout();
        if let Some(win) = self.window.as_ref() {
            win.request_redraw();
        }
    }
    /// Recursively rebuild one window's BSP tree from its saved JSON, spawning a
    /// pane per surviving leaf. A leaf whose record is null (cwd/pid unresolved
    /// at save) or whose PTY fails to spawn is dropped, and a split with one
    /// dead child collapses to the survivor so the tree never carries an empty
    /// half.
    pub(super) fn restore_window_layout(
        &mut self,
        node: &serde_json::Value,
        cols: u16,
        rows: u16,
    ) -> Option<kasa_pty::PtyLayout> {
        self.restore_window_layout_at(node, cols, rows)
    }

    pub(super) fn restore_window_layout_at(
        &mut self,
        node: &serde_json::Value,
        cols: u16,
        rows: u16,
    ) -> Option<kasa_pty::PtyLayout> {
        if let Some(leaf) = node.get("leaf") {
            if leaf.is_null() {
                return None;
            }
            let id = self.restore_leaf(leaf, cols, rows)?;
            return Some(kasa_pty::PtyLayout::Leaf { pane_id: id });
        }
        if let Some(split) = node.get("split") {
            let dir = match split.get("dir").and_then(|d| d.as_str()) {
                Some("v") => kasa_pty::SplitDir::Vertical,
                _ => kasa_pty::SplitDir::Horizontal,
            };
            let ratio = split.get("ratio").and_then(|r| r.as_f64()).unwrap_or(0.5) as f32;
            let probe = kasa_pty::PtyLayout::Split {
                dir,
                ratio,
                a: Box::new(kasa_pty::PtyLayout::single("a")),
                b: Box::new(kasa_pty::PtyLayout::single("b")),
            };
            let rects = probe.leaf_rects(cols, rows);
            let (_, _, _, aw, ah) = rects[0].clone();
            let (_, _, _, bw, bh) = rects[1].clone();
            let a = split
                .get("a")
                .and_then(|a| self.restore_window_layout_at(a, aw, ah));
            let b = split
                .get("b")
                .and_then(|b| self.restore_window_layout_at(b, bw, bh));
            return match (a, b) {
                (Some(a), Some(b)) => Some(kasa_pty::PtyLayout::Split {
                    dir,
                    ratio,
                    a: Box::new(a),
                    b: Box::new(b),
                }),
                (Some(only), None) | (None, Some(only)) => Some(only),
                (None, None) => None,
            };
        }
        None
    }
    /// Spawn one restored pane from its saved record and, when it was running an
    /// agent, queue the command that brings it back. Returns the new pane id, or
    /// None if the PTY failed to start (caller then collapses the split).
    pub(super) fn restore_leaf(&mut self, rec: &serde_json::Value, cols: u16, rows: u16) -> Option<String> {
        let id = self.restore_surface(rec, cols, rows, None)?;
        // pane 안에 겹쳐 둔 탭들(저장은 layout_to_json 의 `tabs`). leaf 와 **같은
        // 길**로 되살린다 — 이어갈 대화가 실재하는지, 캐릭터를 되살릴지, 권한
        // 모드를 승계할지가 한 함수에 있어야 둘이 어긋나지 않는다.
        let tabs = rec
            .get("tabs")
            .and_then(|v| v.as_array())
            .filter(|a| !a.is_empty());
        if tabs.is_some() {
            // ⚠️ 자리를 **먼저** 세운다. leaf 의 `PaneState` 는 첫 화면 프레임이 와야
            // 생기는데(apply_screen_update), 그건 남의 스레드라 여기 올 때까지 아직
            // 없을 수 있다 — 그러면 탭이 붙을 데가 없어 조용히 빠진다. `pane_mut` 이
            // 기본 탭 하나로 자리를 세우고, 첫 프레임은 그 탭에 제 pid 를 채운다.
            self.ws.lock().unwrap().pane_mut(&id);
        }
        for t in tabs.into_iter().flatten() {
            if self.restore_surface(t, cols, rows, Some(&id)).is_none() {
                // 조용히 빠지면 「탭이 왜 안 돌아왔나」를 사후에 짚을 수가 없다 —
                // 2026-09-14 에 학생 넷을 잃고도 로그가 한 줄도 없어 저장·복원 중
                // 어느 쪽인지 가리는 데만 한참 걸렸다.
                eprintln!(
                    "[restore] pane {id} 의 탭 {} 를 못 살렸다",
                    t.get("pane_id").and_then(|v| v.as_str()).unwrap_or("?")
                );
            }
        }
        // 보던 탭으로 되돌린다. 하나가 못 살아났을 수 있으므로 실제 개수로 자른다.
        if let Some(n) = rec.get("active_tab").and_then(|v| v.as_u64()) {
            let mut ws = self.ws.lock().unwrap();
            if let Some(pane) = ws.panes.get_mut(&id) {
                pane.active_tab = (n as usize).min(pane.tabs.len().saturating_sub(1));
                pane.tab_last_active = pane.active_tab;
                pane.dirty = true;
            }
        }
        Some(id)
    }

    /// leaf 하나, 또는 그 pane 안의 탭 하나를 되살린다.
    ///
    /// `tab_of` 가 있으면 원격도 바깥 pane 의 탭으로 앉힌 뒤 출력을 연결한다.
    pub(super) fn restore_surface(
        &mut self,
        rec: &serde_json::Value,
        cols: u16,
        rows: u16,
        tab_of: Option<&str>,
    ) -> Option<String> {
        let saved = rec.get("pane_id").and_then(|v| v.as_str());
        // 저장된 번호를 되살릴 수 있는지는 alloc 과 **같은 기준**으로 본다 — `self.pty`
        // 만 보면 이미 복원된 미리보기 pane 의 번호를 빼앗는다.
        let used = self.used_pane_ids();
        let id = pick_restore_id(saved, |s| used.contains(s))
            .unwrap_or_else(|| next_free_pane_id(&used));
        // ID allocation already excluded every living PTY/tab. Register the
        // identity on that resolved free ID, never overwrite a live source ID.
        let mut key_registration = RestoredSurfaceKey::register(&id, rec);
        if let Some(progress) = self.restore_progress.as_mut() { progress.track(&id, rec); }
        // 웹 pane — PTY 를 안 띄운다. 그리드 자리(WebPane)만 앉히고 자식 창은
        // pending_web_hosts 로 미룬다: 복원 경로엔 ActiveEventLoop 가 없어
        // 창을 만들 수 없다(about_to_wait 의 drain 이 다음 턴에 만든다).
        if let Some(url) = rec
            .get("web_url")
            .and_then(|v| v.as_str())
            .filter(|_| tab_of.is_none())
        {
            let host_id = self.alloc_web_host_id();
            let mut tab = crate::PaneTab::default();
            tab.content = crate::PaneContent::Web(crate::WebPane {
                url: url.to_string(),
                host_id,
            });
            tab.title = Some(
                rec.get("title")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.trim().is_empty())
                    .map(str::to_string)
                    .unwrap_or_else(|| crate::webpane::short_label(url)),
            );
            tab.title_pinned = true;
            let ps = crate::PaneState {
                tabs: vec![tab],
                dirty: true,
                ..Default::default()
            };
            self.ws.lock().unwrap().panes.insert(id.clone(), ps);
            self.pending_web_hosts.push((host_id, url.to_string()));
            key_registration.committed = true;
            return Some(id);
        }
        // Remote restore is asynchronous: an unavailable host must never turn
        // a saved mirror into an unrelated local shell.
        if let (Some(base), Some(rpane)) = (
            rec.get("remote_base").and_then(|v| v.as_str()),
            rec.get("remote_pane").and_then(|v| v.as_str()),
        ) {
            let rec_str = |k: &str| rec.get(k).and_then(|v| v.as_str()).map(str::to_string);
            let spec = kasa_mcp::remote::RemoteSpec {
                base: base.to_string(),
                pane: Some(rpane.to_string()),
                cwd: None,
                token: None,
                identity: kasa_mcp::remote::RemoteIdentity {
                    label: rec_str("remote_label")
                        .or_else(|| kasa_mcp::machines::label_for_base(base))
                        .unwrap_or_default(),
                    remote_cwd: rec_str("remote_cwd"),
                    origin_cwd: rec_str("remote_origin_cwd"),
                    owned: rec
                        .get("remote_owned")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false),
                },
            };
            // 거울(view)로 살던 pane 은 거울로 되살린다 — 소유자로 붙으면
            // 재시작 한 번에 원본 크기를 도로 뺏는다.
            let view = rec
                .get("remote_view")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            // Restore the same surface, not the agent conversation that happened
            // to occupy it when the viewer last saved its layout.
            let surface_key = rec_str("remote_surface_key")
                .or_else(|| rpane.starts_with('%').then(|| format!("legacy:{rpane}")));
            let attempt = if surface_key.is_some() {
                kasa_mcp::remote::restore_connection_identified(
                    spec, &id, cols, rows, view,
                    kasa_mcp::remote_restore::RestoreIdentity {
                        surface_key,
                        ..Default::default()
                    },
                )
            } else {
                kasa_mcp::remote::restore_connection(spec, &id, cols, rows, view)
            };
            match attempt {
                Ok(remote) => {
                    {
                        let mut ws = self.ws.lock().unwrap();
                        if let Some(outer) = tab_of {
                            ws.panes.get(outer)?;
                            ws.pid_to_pane.insert(id.clone(), outer.to_string());
                            if let Some(room) = ws.pane_room.get(outer).cloned() {
                                ws.pane_room.insert(id.clone(), room);
                            }
                            let mut tab = crate::PaneTab::default();
                            tab.pid = Some(id.clone());
                            ws.panes.get_mut(outer).unwrap().tabs.push(tab);
                        } else {
                            let pane = ws.panes.entry(id.clone()).or_default();
                            if let Some(tab) = pane.tabs.first_mut() {
                                tab.pid = Some(id.clone());
                            }
                            ws.pid_to_pane.insert(id.clone(), id.clone());
                        }
                    }
                    self.insert_pty(id.clone(), remote.session.clone());
                    self.pump_pty_screens(
                        remote.session.screens.clone(),
                        id.clone(),
                        std::sync::Arc::downgrade(&remote.session),
                    );
                    // The startup shell may have queued EOF for this reused
                    // ID before the replacement remote PTY was registered.
                    // Match local restore's stale-death cleanup.
                    self.dead_panes.lock().unwrap().retain(|old| old != &id);
                    if let Some(t) = rec
                        .get("title")
                        .and_then(|v| v.as_str())
                        .filter(|s| !s.trim().is_empty())
                    {
                        let mut ws = self.ws.lock().unwrap();
                        let outer = ws.outer_for_pty(&id).unwrap_or_else(|| id.clone());
                        let pane = ws.panes.get_mut(&outer).unwrap();
                        let tab = if tab_of.is_some() {
                            pane.tabs.iter_mut().find(|tab| tab.pid.as_deref() == Some(&id))
                        } else { pane.tabs.first_mut() };
                        if let Some(tab) = tab {
                            tab.title = Some(t.to_string());
                            tab.title_pinned = true;
                        }
                    }
                    // 학생 이름을 되살린다. 이 갈래는 여기서 돌아가므로 **아래의
                    // 저장된-캐릭터 복원에 닿지 않는다** — 그래서 이사 간 학생은
                    // 재시작 한 번에 이름·색·얼굴이 통째로 사라졌고, 저장은
                    // `pane_character` 에서 뜨므로 그 다음 저장에 영영 굳었다
                    // (2026-08-30 지적: 「맥미니로 가면 왜 맥북에서 테마가 안보여」.
                    // 실측으로 미러 두 자리에 `character` 키가 아예 없었다).
                    //
                    // Seed the saved identity without blocking restoration on
                    // network I/O. The live source poll replaces it when ready.
                    if let Some(name) = rec_str("character")
                        .filter(|s| !s.is_empty())
                        .filter(|s| !s.is_empty())
                    {
                        self.relabel_pane(&id, &name);
                    }
                    // 이사로 나간 학생의 대화 id 를 지도(pane_claude_sid)에도 되살린다 —
                    // 데려오기(migrate_pane_back)의 유일한 열쇠인데 지도는 메모리라
                    // 재시작 한 번에 증발했고, leaf 에 저장은 되면서 복원이 안 채워
                    // 「세션 id 를 모른다」로 서던 자리다(2026-08-30 실측: 미도리·미쿠
                    // 둘 다 수동 이사로 돌아왔다).
                    if let Some(sid) = rec
                        .get("session_id")
                        .and_then(|v| v.as_str())
                        .filter(|s| !s.is_empty())
                    {
                        self.pane_claude_sid.insert(id.clone(), sid.to_string());
                    }
                    key_registration.committed = true;
                    return Some(id);
                }
                Err(e) => {
                    self.collab.toast = Some((
                        format!("원격 pane 을 못 이었어요: {e}"),
                        std::time::Instant::now(),
                    ));
                    return None;
                }
            }
        }
        let cwd = rec
            .get("cwd")
            .and_then(|c| c.as_str())
            .map(|s| s.to_string())
            .or_else(resolve_initial_cwd);
        let was_agent = saved_agent(rec);
        let session_id = rec
            .get("session_id")
            .and_then(|s| s.as_str())
            .map(|s| s.to_string());
        // 저장된 캐릭터를 되살린다(사용자: 재시작하면 랜덤 둔갑). pending 으로 세팅하면
        // assign_character_env 가 랜덤 대신 이걸 재사용하고, 저장 세션 id 가 있으면 그
        // 원본 sid 에 캐릭터를 다시 bind 해 --resume 후 shim 교정·다음 재시작까지 영속화한다.
        // 고른 명단 밖이면 **되살리지 않는다** — 그러면 아래 `assign_character_env` 가
        // 명단 안에서 새로 뽑는다. 저장된 이름을 무조건 되살리던 탓에, 명단을 바꿔도
        // 이미 배정된 학생은 재시작을 넘어 영원히 남았다(사용자 2026-08-25 「설정에서
        // 원하는거 다 골랐는데 그거 반영안되고 선택안된학생도 스폰돼」 — 새 배정은
        // 멀쩡했고 옛 배정이 안 바뀐 것이었다).
        //
        // 대화는 안 끊긴다. 바뀌는 것은 이름·얼굴·말투뿐이고 `--resume` 은 그대로 탄다.
        let stored_char = rec
            .get("character")
            .and_then(|c| c.as_str())
            .filter(|s| !s.is_empty());
        let saved_char = rec
            .get("character")
            .and_then(|c| c.as_str())
            .filter(|s| !s.is_empty())
            .filter(|s| {
                // 저장된 세션 id 로 **수동 지정 면제**를 본다 — 사람이 직접 고른
                // 자리는 명단을 바꿔도 지킨다(2026-08-26 지시: 「손으로 고른 건
                // 명단 상관없이 지키게 해줘」).
                let sid = rec.get("session_id").and_then(|v| v.as_str()).unwrap_or("");
                let keep = kasa_mcp::character::is_assignable_for(sid, s);
                if !keep {
                    eprintln!("[restore] {s} 는 고른 명단 밖 — 새로 배정한다");
                }
                keep
            })
            .map(|s| s.to_string());
        let mut revived = false;
        if let Some(ref c) = saved_char {
            // **이미 다른 자리가 쓰는 학생이면 되살리지 않는다.** `pending_character`
            // 는 배정의 중복 회피(taken)를 건너뛰는 지름길이라, 저장본에 겹침이 하나
            // 있으면 그대로 복원되고 그 상태가 다시 저장돼 **재시작마다 누적된다**
            // (2026-08-26 실측: 아리스 %0·%9, 세이아 %17·%19 — 후보가 넉넉한데도
            // 겹쳤다). pending 을 안 세우면 아래 assign_character_env 가 taken 을
            // 보고 빈 학생을 새로 뽑는다.
            //
            // 손으로 고른 자리는 예외다 — 사람이 일부러 같은 학생을 둘 앉혔다면
            // 그건 배정 사고가 아니다(같은 날 정한 「손으로 고른 건 지킨다」 규칙).
            let manual = session_id
                .as_deref()
                .is_some_and(kasa_mcp::character::is_manual_pick);
            // ⚠️ 예약(위 `RESERVE_KEY`)은 **세지 않는다.** 그건 「이 이름은 되살릴
            // 자리가 있으니 새 배정이 집어가지 마라」는 표시라, 여기서 함께 세면
            // 되살리려는 자리가 제 예약에 막혀 통째로 새로 배정된다(자기 자신을
            // 막는 자책골 — 재현 리그에서 잡았다).
            let taken = self
                .ws
                .lock()
                .unwrap()
                .pane_character
                .iter()
                .any(|(k, v)| v == c && !k.starts_with(Self::RESERVE_KEY));
            if revive_saved_character(taken, manual) {
                revived = true;
                // 예약을 하나 소비한다. 안 걷으면 그 이름이 계속 예약으로 남아,
                // 같은 학생을 저장본이 둘 이상 들고 있을 때 두 번째 자리가 제
                // 예약에 막힌다.
                {
                    let mut ws = self.ws.lock().unwrap();
                    if let Some(k) = ws
                        .pane_character
                        .iter()
                        .find(|(k, v)| *v == c && k.starts_with(Self::RESERVE_KEY))
                        .map(|(k, _)| k.clone())
                    {
                        ws.pane_character.remove(&k);
                    }
                }
                self.pending_character = Some(c.clone());
                if let Some(ref sid) = session_id {
                    let _ = kasa_mcp::character::bind_session_character(sid, c);
                }
            } else {
                eprintln!("[restore] {c} 는 이미 다른 자리가 쓰는 중 — 새로 배정한다");
            }
        }
        let restores_agent = was_agent.is_some() || (saved_char.is_some() && session_id.is_some());
        let scrollback = restored_scrollback(rec, restores_agent);
        // 탭은 PTY 를 띄우기 **전에** 자리를 잡아야 한다 — 출력은 `pid_to_pane` 으로
        // 길을 찾으므로(apply_screen_update), 등록이 늦으면 첫 프레임이 바깥 pane 을
        // 새로 만들어 탭이 pane 하나로 떨어져 나간다. 방은 바깥에서 물려받는다
        // (spawn_new_tab 과 같은 대접 — 안 물려주면 collab 훅이 다른 slug 를 쓴다).
        let room = match tab_of {
            Some(outer) => {
                let mut ws = self.ws.lock().unwrap();
                ws.panes.get(outer)?; // 바깥 pane 이 없으면 탭도 없다
                let room = ws.pane_room.get(outer).cloned();
                ws.pid_to_pane.insert(id.clone(), outer.to_string());
                if let Some(r) = room.clone() {
                    ws.pane_room.insert(id.clone(), r);
                }
                let mut tab = crate::PaneTab::default();
                tab.pid = Some(id.clone());
                // 붙인 이름은 탭에 붙는다 — pane 은 활성 탭으로 Deref 하므로 pane 에
                // 쓰면 지금 보이는 탭의 이름을 덮는다.
                if let Some(t) = rec
                    .get("title")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.trim().is_empty())
                {
                    tab.title = Some(t.to_string());
                    tab.title_pinned = true;
                }
                if let Some(pane) = ws.panes.get_mut(outer) {
                    pane.tabs.push(tab);
                    pane.dirty = true;
                }
                room
            }
            None => None,
        };
        let mut env = crate::proxy_env(&id);
        if let Some(ref r) = room {
            env.push(("KASATERM_ROOM".to_string(), r.clone()));
        }
        env.extend(self.assign_character_env(&id, cwd.as_deref(), room.as_deref()));
        // 저장된 학생을 **안 쓰기로 했으면 옛 세션 바인딩도 갈아 끼운다.** claude 는
        // `--resume <옛 sid>` 로 돌아오고, board 는 그 sid 의 바인딩을 최우선으로 읽어
        // pane 색·이름·프사를 정한다. 옛 이름이 남아 있으면 방금 새로 뽑은 학생을 매
        // 폴링마다 되덮어, **재배정이 화면에 영영 반영되지 않는다** — 저장본이 다시 옛
        // 이름으로 쓰이니 재시작해도 그대로다(2026-08-26 실측: 이름표는 코유키인데
        // 바인딩만 아리스라 board·헤더가 계속 아리스였다).
        if stored_char.is_some() && !revived {
            let now = self.ws.lock().unwrap().pane_character.get(&id).cloned();
            if let (Some(sid), Some(now)) = (session_id.as_deref(), now) {
                let _ = kasa_mcp::character::bind_session_character(sid, &now);
            }
        }
        let session = match kasa_pty::PtySession::start(kasa_pty::PtyOptions {
            shell: resolve_default_shell(),
            cwd: cwd.clone(),
            cols,
            rows,
            env,
            pane_id: id.clone(),
            initial_scrollback: scrollback,
        }) {
            Ok(s) => Arc::new(s),
            Err(e) => {
                // 조용히 버리면 사용자는 「세션이 없어졌다」만 안다 — 어느 창이
                // 왜 빠졌는지 화면에 말한다. 대화 파일은 그대로라 resume 으로
                // 살릴 수 있다는 것까지.
                eprintln!("[restore] pane {id} spawn failed: {e:#}");
                self.collab.toast = Some((
                    format!(
                        "복원에서 {} 창을 잃었어요({e}) — 대화 파일은 남아 있어요",
                        rec.get("character").and_then(|c| c.as_str()).unwrap_or(&id)
                    ),
                    std::time::Instant::now(),
                ));
                return None;
            }
        };
        self.insert_pty(id.clone(), session.clone());
        self.pump_pty_screens(
            session.screens.clone(),
            id.clone(),
            std::sync::Arc::downgrade(&session),
        );
        if let Some(ref c) = cwd {
            self.pane_cwd_cache
                .insert(id.clone(), std::path::PathBuf::from(c));
        }
        // 복원은 부팅 pane 을 통째로 놓고(`pty.clear`) 시작하는데, 그 셸의 EOF 는 복원이
        // 도는 **도중**에 도착한다. 그때 명부에 그 번호가 없으면 `pane_replaced` 가
        // 「바뀐 적 없다」로 답해 죽음표시가 그대로 실리고, 저장본이 같은 번호(%0)로
        // 되살린 pane 을 다음 턴 reap 이 걷는다 — 2026-09-08 13:13 재시작에서 4번방
        // 미도리(%0)가 그렇게 사라졌다(격리 앱에선 EOF 가 늦어 안 걸린다). 같은 번호로
        // 다시 띄운 자리(`swap_character`)와 같은 규칙으로 옛 표시를 지운다.
        self.dead_panes.lock().unwrap().retain(|x| x != &id);
        // 붙인 이름을 되살린다. 핀도 같이 세워야 한다 — 안 세우면 되살린 이름이
        // pane 안 프로그램의 첫 OSC 에 곧바로 덮여, 저장한 보람이 몇 초 만에 사라진다
        // (claude 는 뜨자마자 제목을 쏜다). 저장 쪽이 핀 선 것만 넣으므로 여기 온
        // 값은 전부 사람이 정한 이름이다.
        if let Some(t) = rec
            .get("title")
            .and_then(|v| v.as_str())
            .filter(|s| !s.trim().is_empty())
            .filter(|_| tab_of.is_none())
        {
            let mut ws = self.ws.lock().unwrap();
            let pane = ws.pane_mut(&id);
            pane.title = Some(t.to_string());
            pane.title_pinned = true;
        }
        // Bring the agent back: --resume the saved conversation (the shim
        // re-attaches team/persona/character from the session id), or a fresh
        // one when the pane ran an agent but no session id was captured.
        // Unregistered shells restore to just their shell + scrollback. 900ms
        // mirrors swap_character's wait for the shell prompt before injection.
        // 하네스 감지가 실패했어도 캐릭터+저장 sid 가 있으면 claude 학생 pane 이었던
        // 것이라 --resume 으로 대화를 복원한다(감지 실패 시 셸만 뜨던 회귀 차단).
        if restores_agent {
            // --resume 대상 대화가 실재할 때만 resume 한다. 저장된 sid 의 jsonl 이
            // 사라졌으면 claude 가 "No conversation found" 를 뱉고 빈 셸만 남아 학생
            // pane 이 통째 죽는다(사용자: %3 시로코 복원 실패 — claude 세션이 없어 board
            // 순회에서 빠졌다). 그땐 fresh claude 로 폴백해 최소한 학생 pane(캐릭터는
            // env/marker 로 유지)은 살린다 — 대화는 잃지만 pane 이 통째 죽는 것보다 낫다.
            //
            // 파일이 사는 곳이 하네스마다 다르다 — claude 는 `~/.claude/projects/<슬러그>/
            // <sid>.jsonl`, codex 는 `~/.codex/sessions/<Y>/<M>/<D>/rollout-<ts>-<sid>.jsonl`.
            let resumable = session_id
                .as_deref()
                .and_then(|sid| {
                    if was_agent == Some("codex") {
                        socket::codex_rollout_for_session(sid)
                    } else {
                        socket::transcript_path_for_session(sid)
                    }
                })
                .map(|p| p.exists())
                .unwrap_or(false);
            // 이어가기 실패는 무증상이었다 — 빈 세션이 학생 얼굴로 멀쩡히 떠서,
            // 대화를 잃은 줄 모른 채 계속 쓰게 된다(미도리 실측). 자리를 만들어
            // 준 것만으로는 부족하고 잃은 것을 말해 줘야 한다. 하네스와 무관하게
            // 같다 — codex 도 이제 이어가므로 잃으면 똑같이 말해야 한다.
            if session_id.is_some() && !resumable {
                self.collab.toast = Some((
                    format!(
                        "{} 이어갈 대화를 못 찾아 새로 시작합니다",
                        saved_char.as_deref().unwrap_or("이 pane 은")
                    ),
                    std::time::Instant::now(),
                ));
            }
            // `--effort ultracode` 로 되살린 pane 은 **transcript 에 아무 흔적을 안
            // 남긴다**(플래그 launch 는 enter attachment 를 안 쓴다 — 훅 주석의 실측).
            // 훅은 첫 프롬프트가 있어야 돌고 꼬리 스캔도 볼 것이 없으니, 앱이 자기가
            // 그렇게 띄웠다는 사실만이 유일한 근거다. 안 세워 두면 복원하자마자 다시
            // 끄는 것만으로 ultracode 가 xhigh 로 풀린다.
            if saved_effort(rec) == Some("ultracode") {
                self.mark_restored_ultracode(&id);
            }
            // 하네스와 어긋나는 모델은 여기서 버린다 — 2026-09-05 이전에 저장된
            // 오염분(갈아 끼운 자리에 남은 옛 모델)이 `codex -m 'claude-…'` 로
            // 되살아나는 것을 막는다. 새 오염은 저장 쪽(layout_to_json)이 같은
            // 판정으로 막으므로, 이 겹은 이미 디스크에 있는 것만 상대한다.
            // effort 는 모델과 한 벌이라 함께 버린다.
            let (use_model, use_effort) = match saved_model(rec) {
                Some(m) if !saved_model_fits_agent(was_agent, m) => {
                    eprintln!(
                        "[restore] pane {id}: {} 자리에 남은 옛 모델({m})을 빼고 되살린다",
                        was_agent.unwrap_or("claude")
                    );
                    (None, None)
                }
                m => (m, saved_effort(rec)),
            };
            // 방금 이 값으로 띄웠다는 사실을 남긴다 — statusline 보고가 오기 전에
            // 저장이 돌면 그 창의 모델·effort 가 통째로 빠진다.
            self.mark_restored_agent_cfg(
                &id,
                use_model.unwrap_or_default(),
                use_effort.unwrap_or_default(),
            );
            // sid 지도도 바로 채운다 — 훅(첫 프롬프트 뒤)만 기다리면 「재시작 직후엔
            // 이사를 못 보낸다」가 남는다. 잘못 묶일 값이 아니다: 이 pane 의 저장본이
            // 실은 그 sid 고, 훅이 오면 어차피 같은 값으로 덮는다.
            if let (Some(sid), true) = (session_id.as_deref(), resumable) {
                self.pane_claude_sid.insert(id.clone(), sid.to_string());
            }
            let mut cmd = restore_agent_command(
                was_agent,
                session_id.as_deref(),
                resumable,
                use_model,
                use_effort,
            );
            // 권한 모드 승계 — 저장이 화면에서 읽어 실은 플래그(layout_to_json).
            // 안 실으면 복원된 학생이 전부 물어보는 모드로 깨어나고, 특히 사람이
            // 안 보는 기계(미니)에선 그 물음에 답할 손이 없어 조용히 선다
            // (2026-08-29 미니 재시작 실측). claude 전용 — codex/agy 는 규약이 다르다.
            if rec.get("bypass").and_then(|v| v.as_bool()).unwrap_or(false)
                && resumable
                && matches!(was_agent, None | Some("claude"))
                && cmd.ends_with('\r')
            {
                cmd.pop();
                cmd.push_str(" --dangerously-skip-permissions\r");
            }
            let at = std::time::Instant::now() + std::time::Duration::from_millis(900);
            self.pending_restores.push((session, cmd, at));
        } else {
            self.restore_server(&id, &session, rec);
        }
        key_registration.committed = true;
        Some(id)
    }
}

pub(super) fn restored_scrollback(rec: &serde_json::Value, restarting_agent: bool) -> Vec<String> {
    if restarting_agent {
        return Vec::new();
    }
    rec.get("scrollback")
        .and_then(|v| v.as_array())
        .map(|lines| {
            lines
                .iter()
                .filter_map(|line| line.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

/// 복원되는 pane 이 쓸 id 를 고른다. **저장된 id 를 최우선**으로 되살린다 —
/// `--resume` 으로 되살아난 학생은 재시작 전의 surface_id 를 대화 기록째 기억하고
/// 있어서, 번호를 새로 매기면 `tell` 이 없는 pane 이거나 그 사이 다른 pane 이
/// 물려받은 번호로 배달된다(사용자: "재시작하면 학생들이 tell 을 이상한 pane 에 쓴다").
///
/// 저장본에 id 가 없거나(옛 포맷) 이미 쓰이는 번호면 새로 발급한다. 되살린 번호가
/// 카운터보다 크면 카운터를 그 위로 밀어, 이후 split 이 같은 번호를 다시 내주지
/// 않게 한다.
/// 저장된 leaf 가 어떤 하네스로 돌던 pane 인지 — 없으면 순수 셸.
///
/// 정본 키는 `was_agent`(`AgentKind::as_str` 이 쓴 id). 그 전 포맷은 `was_claude: true`
/// 뿐이라 **옛 저장본은 claude 로 읽는다** — 안 그러면 이번 판올림 한 번에 사용자가 쓰던
/// 학생 pane 이 전부 셸로 되살아난다. 새 코드는 `was_agent` 만 쓴다(두 키를 같이 쓰면
/// 언젠가 갈린다).
///
/// 되읽기를 `AgentKind::from_id` 하나로 모은 이유: 예전엔 여기가 세 종류를 손으로
/// 나열했는데, 하네스가 서른이 된 지금 그 사본을 두면 표에만 있고 여기엔 없는
/// 하네스가 **재시작 한 번에 셸로 되살아난다**(학생·대화 이어가기가 통째로 빠진다).
pub(super) fn saved_agent_map_with(
    rec: &serde_json::Map<String, serde_json::Value>,
    mut codex_root_exists: impl FnMut(&str) -> bool,
) -> Option<&'static str> {
    if let Some(kind) = saved_agent_marker_map(rec) {
        return Some(kind);
    }
    let may_infer = rec.get("was_agent").is_none_or(serde_json::Value::is_null);
    let sid = rec
        .get("session_id")
        .and_then(|v| v.as_str())
        .filter(|sid| !sid.is_empty());
    (may_infer && sid.is_some_and(&mut codex_root_exists)).then_some("codex")
}

/// 화면에 표시할 복원 수처럼 반복 호출되는 경로가 읽는 저장본 표식.
/// 정확한 Codex rollout 확인은 재귀 파일 탐색이라 이 순수 판정에 넣지 않는다.
pub(super) fn saved_agent_marker_map(
    rec: &serde_json::Map<String, serde_json::Value>,
) -> Option<&'static str> {
    if let Some(kind) = rec
        .get("was_agent")
        .and_then(|v| v.as_str())
        .and_then(kasa_pty::AgentKind::from_id)
    {
        return Some(kind.as_str());
    }
    if rec
        .get("was_claude")
        .and_then(|b| b.as_bool())
        .unwrap_or(false)
    {
        return Some("claude");
    }
    None
}

pub(super) fn saved_agent_marker(rec: &serde_json::Value) -> Option<&'static str> {
    saved_agent_marker_map(rec.as_object()?)
}

pub(super) fn saved_agent(rec: &serde_json::Value) -> Option<&'static str> {
    saved_agent_map_with(rec.as_object()?, |sid| {
        socket::codex_root_rollout_for_session(sid).is_some()
    })
}

/// 저장하려는 대화 번호가 그 하네스의 것인가.
///
/// 한 자리에서 하네스를 갈아 끼워도 `pane_claude_sid` 는 옛 번호를 쥐고 있다 — 그 지도는
/// claude sid 와 codex rollout uuid 를 한 칸에 담으면서 전환을 신호로 걷지 않는다. 그래서
/// codex 자리에 claude 번호가 실려 저장되고, 복원은 codex 창고에서 그 번호를 못 찾아 새
/// 대화로 떨어진다(2026-09-05 실측: 그 자리의 4.3MB 대화가 통째로 안 열렸다).
///
/// **상대 창고에서 실물이 확인될 때만 거짓**을 낸다. 갓 뜬 세션은 자기 기록 파일이 아직
/// 없을 수 있어서, "내 창고에 없다"만으로 버리면 멀쩡한 번호를 잃는다.
pub(super) fn saved_sid_fits_agent(agent: Option<&str>, sid: &str) -> bool {
    saved_sid_fits_agent_with(
        agent,
        sid,
        |s| socket::codex_root_rollout_for_session(s).is_some(),
        |s| socket::transcript_path_for_session(s).is_some(),
    )
}

/// `saved_sid_fits_agent` 의 순수 부분 — 두 창고 조회를 주입해 파일 없이 시험한다.
pub(super) fn saved_sid_fits_agent_with(
    agent: Option<&str>,
    sid: &str,
    is_codex: impl Fn(&str) -> bool,
    is_claude: impl Fn(&str) -> bool,
) -> bool {
    match agent {
        Some("codex") => is_codex(sid) || !is_claude(sid),
        // 하네스 미상은 claude 로 되살아난다(`restore_agent_command`) — 판정도 같이 간다.
        Some("claude") | None => is_claude(sid) || !is_codex(sid),
        // agy 는 아직 번호를 안 넘겨받는다. 남의 규칙으로 재단하지 않는다.
        _ => true,
    }
}

/// 저장하려는 모델이 그 하네스의 것인가. 번호와 같은 사고의 다른 면이다 — 갈아 끼운
/// 자리에 옛 모델이 남으면 복원이 `codex -m 'claude-opus-5[1m]'` 를 내보낸다(2026-09-05
/// 실측: codex 창이 claude 모델명을 달고 떠 있었다).
pub(super) fn saved_model_fits_agent(agent: Option<&str>, model: &str) -> bool {
    let m = model.trim();
    match agent {
        Some("codex") => !m.starts_with("claude-"),
        Some("claude") | None => !m.starts_with("gpt-"),
        _ => true,
    }
}

pub(super) fn normalize_saved_agent_map_with(
    rec: &mut serde_json::Map<String, serde_json::Value>,
    codex_root_exists: impl FnMut(&str) -> bool,
) {
    if rec.get("was_agent").is_none_or(serde_json::Value::is_null)
        && saved_agent_map_with(rec, codex_root_exists) == Some("codex")
    {
        rec.insert("was_agent".to_string(), serde_json::json!("codex"));
    }
}

/// 저장된 leaf 가 쓰던 모델 — 없으면 `None`(복원 명령에 플래그를 안 붙인다).
///
/// 옛 저장본엔 이 키가 없다. 그때 빈 문자열이 아니라 `None` 이어야 하는 이유는,
/// 호출부가 "없으면 플래그 자체를 뺀다"로 갈리기 때문이다 — 빈 값을 흘리면
/// `--model ''` 이 나가 하네스가 기본값도 못 고른다.
pub(super) fn saved_model(rec: &serde_json::Value) -> Option<&str> {
    rec.get("model")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
}

/// 저장된 leaf 가 쓰던 reasoning effort — 없으면 `None`. `saved_model` 과 같은 규약.
pub(super) fn saved_effort(rec: &serde_json::Value) -> Option<&str> {
    rec.get("effort")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
}

/// 복원된 pane 에 넣을 명령. 하네스 셋 × (이어가기/새로)라 순수 함수로 뺐다 —
/// `restore_leaf` 는 살아있는 PTY 없이 못 부르고, 그러면 이 분기를 테스트할 방법이
/// 사라진다.
///
/// ⚠️ **마지막 갈래가 claude 라는 게 함정이다.** 새 하네스를 여기 안 적으면 오류
/// 없이 claude 로 되살아나고, 이어가기까지 걸리면 남의 하네스 세션 id 로
/// `claude --resume` 을 친다. agy 를 붙일 때 실제로 그 상태였다(2026-08-11).
///
/// codex 도 이어간다. 셋을 실측으로 확인했다(2026-08-05):
/// - `codex resume <uuid>` 는 **pane 홈이 사라져도** 대화를 되살린다. shim 이 세운
///   pane 별 CODEX_HOME 은 GUI pid 별이라 재시작이면 통째로 없어지는데, `sessions` 가
///   `~/.codex/sessions` 심볼릭이라 실체가 남고 codex 가 거기서 찾아낸다(홈을 치우고
///   다른 pane 홈에서 resume 해 첫 질문까지 그대로 복원되는 것을 확인).
///   ⚠️ codex 0.153 부터는 상태 db 의 `rollout_path`(옛 pane 홈 경로)를 먼저 믿어
///   그 자리가 없으면 죽는다 — `restore_session_state` 가 되살리기 전에
///   `codex_repair_thread_paths` 로 그 열을 실체 자리로 고친다(2026-09-08).
/// - 세션 id 는 `pane_claude_sid`(PID→열린 rollout 결속)로 들어온다 — rollout
///   파일명에서 uuid 를 떼어낸 값이다. argv 로는 fresh thread를 못 집는다.
/// - `resume --last` 는 쓰지 않는다. 그건 미러된 `~/.codex/sessions` 전체에서 최신 하나를
///   고르므로 **다른 pane·pane 밖 codex 의 대화**를 물어온다. id 가 없으면 새로 띄운다.
/// `model`/`effort` 는 끄기 직전 그 pane 이 쓰던 값이다(없으면 `None`). **없으면
/// 플래그 자체를 안 붙인다** — 빈 값을 넘기면 하네스가 기본값조차 못 고른다.
///
/// 문법이 셋 다 다르다(실측 2026-08-11): claude·agy 는 `--model`/`--effort` 플래그,
/// codex 는 `-m` 과 config 오버라이드(`-c model_reasoning_effort=`).
///
/// ⚠️ agy 에는 지금 model 이 실려 오지 않는다 — 전사본의 모델이 되먹일 수 없는
/// 표시용 이름(`Gemini 3.6 Flash (Low)`)이라 수집 쪽에서 일부러 안 담는다. 나중에
/// 담게 되거든 **`agy models` 목록과 대조하고 나서** 담아라: agy 는 없는 모델값에
/// 에러를 안 내고 조용히 기본값으로 돌아, 틀려도 아무 데도 안 남는다.
///
/// 나쵸가 띄운 세션이면 그 표식(`KASATERM_ORIGIN*`)을 명령 앞 env 로 되붙인다 — 새 pane
/// env 에는 부팅 명령의 표식이 없어, 복원된 학생의 `nacho-report` 가 거부됐다.
pub(crate) fn restore_agent_command(
    agent: Option<&str>,
    session_id: Option<&str>,
    resumable: bool,
    model: Option<&str>,
    effort: Option<&str>,
) -> String {
    let origin = session_id
        .filter(|_| resumable)
        .and_then(kasa_socket::nacho_inbox::origin_for_session);
    restore_agent_command_for(agent, session_id, resumable, model, effort, origin.as_ref())
}

pub(super) fn restore_agent_command_for(
    agent: Option<&str>,
    session_id: Option<&str>,
    resumable: bool,
    model: Option<&str>,
    effort: Option<&str>,
    origin: Option<&kasa_socket::nacho_inbox::Origin>,
) -> String {
    // 값은 작은따옴표로 감싼다 — `claude-opus-5[1m]` 의 `[1m]` 이 zsh 글롭이라 무인용
    // 이면 "no matches found" 로 명령이 통째 실패한다. shim 쪽에서 같은 사고가 실제로
    // 났다(2026-07-27: 학생이 전부 구세대 Opus 로 떨어짐).
    let q = |s: &str| format!("'{}'", s.replace('\'', r"'\''"));
    let model = model.filter(|s| !s.is_empty());
    let effort = effort.filter(|s| !s.is_empty());
    let resume = session_id.filter(|_| resumable);
    let mut cmd = match (agent, resume) {
        (Some("codex"), Some(sid)) => {
            format!("codex resume {sid} -c check_for_update_on_startup=false")
        }
        // Automatic restoration cannot wait for an updater that exits to the shell.
        (Some("codex"), None) => "codex -c check_for_update_on_startup=false".to_string(),
        // agy 는 아직 세션 id 가 안 들어온다 — bind-transcript 훅이 claude·codex
        // shim 에만 걸려 있어서다. 그래도 하네스는 맞춰 띄운다: 여기 없으면 agy
        // pane 이 claude 로 되살아난다.
        (Some("agy"), Some(sid)) => format!("agy --conversation {sid}"),
        (Some("agy"), None) => "agy".to_string(),
        (_, Some(sid)) => format!("claude --resume {sid}"),
        (_, None) => "claude".to_string(),
    };
    if agent == Some("codex") {
        if let Some(m) = model {
            cmd.push_str(&format!(" -m {}", q(m)));
        }
        if let Some(e) = effort {
            cmd.push_str(&format!(" -c model_reasoning_effort={}", q(e)));
        }
    } else {
        // claude 는 shim 이 전역 `--model` 을 **앞에** 붙이는데, 뒤에 온 우리 값이
        // 이긴다(clap 은 같은 플래그를 마지막 것으로 덮는다). 그래서 pane 별 값이
        // 전역 설정을 넘어선다.
        if let Some(m) = model {
            cmd.push_str(&format!(" --model {}", q(m)));
        }
        if let Some(e) = effort {
            cmd.push_str(&format!(" --effort {}", q(e)));
        }
    }
    if let (Some(origin), Some(_)) = (origin, resume) {
        cmd.insert_str(0, &kasa_socket::nacho_inbox::origin_env_prefix(origin));
    }
    cmd.push('\r');
    cmd
}

/// 저장된 학생을 그대로 되살릴 것인가.
///
/// 복원은 `pending_character` 로 배정을 건너뛰는데, 그 지름길은 중복 회피도 함께
/// 건너뛴다. 그래서 저장본에 겹침이 하나 생기면 재시작마다 그대로 실려 누적된다.
/// 손으로 고른 자리만 예외로 둔다 — 사람이 일부러 겹쳤다면 사고가 아니다.
pub(super) fn revive_saved_character(already_taken: bool, manual: bool) -> bool {
    manual || !already_taken
}

/// 저장본의 pane 번호를 그대로 되살릴 수 있으면 그것. 없거나(옛 저장본) 형식이
/// 깨졌거나 이미 살아 있으면 `None` — 그때는 호출부가 `alloc_pane_id` 로 새로 받는다.
pub(super) fn pick_restore_id(saved: Option<&str>, taken: impl Fn(&str) -> bool) -> Option<String> {
    let s = saved?;
    s.strip_prefix('%').and_then(|d| d.parse::<u32>().ok())?;
    (!taken(s)).then(|| s.to_string())
}

#[cfg(test)]
mod agy_restore_tests {
    use super::{
        normalize_saved_agent_map_with, restore_agent_command, restore_agent_command_for, saved_agent, saved_agent_map_with,
        saved_agent_marker, saved_effort, saved_model, saved_model_fits_agent,
        saved_sid_fits_agent_with,
    };

    /// 하네스를 갈아 끼운 자리에 남은 옛 대화 번호는 **상대 창고에서 실물이 보일 때만**
    /// 버린다. 2026-09-05 사고가 정확히 이 조합이었다 — codex 자리에 실재하는 claude
    /// 번호가 실려, 복원이 codex 창고를 뒤지다 새 대화로 떨어졌다.
    #[test]
    fn foreign_harness_session_id_is_dropped_before_saving() {
        const CLAUDE_SID: &str = "af880ea3-3138-4836-9db2-d8503f32b724";
        const CODEX_SID: &str = "01a06e68-c840-7033-be2f-60be3285ffc8";
        let is_codex = |s: &str| s == CODEX_SID;
        let is_claude = |s: &str| s == CLAUDE_SID;

        assert!(
            !saved_sid_fits_agent_with(Some("codex"), CLAUDE_SID, is_codex, is_claude),
            "codex 자리의 claude 번호는 버린다"
        );
        assert!(
            !saved_sid_fits_agent_with(Some("claude"), CODEX_SID, is_codex, is_claude),
            "claude 자리의 codex 번호도 같다"
        );
        assert!(
            !saved_sid_fits_agent_with(None, CODEX_SID, is_codex, is_claude),
            "하네스 미상은 claude 로 되살아나므로 같은 잣대"
        );

        assert!(saved_sid_fits_agent_with(Some("codex"), CODEX_SID, is_codex, is_claude));
        assert!(saved_sid_fits_agent_with(Some("claude"), CLAUDE_SID, is_codex, is_claude));
        // 어느 창고에도 아직 없는 번호(갓 뜬 세션)를 잘못 버리지 않는다 — 그러면
        // 첫 저장에서 대화가 통째로 날아간다.
        let fresh = "00000000-0000-4000-8000-00000000ffff";
        assert!(saved_sid_fits_agent_with(Some("codex"), fresh, is_codex, is_claude));
        assert!(saved_sid_fits_agent_with(Some("claude"), fresh, is_codex, is_claude));
        // agy 는 아직 번호를 안 넘겨받는다 — 남의 규칙으로 재단하지 않는다.
        assert!(saved_sid_fits_agent_with(Some("agy"), CLAUDE_SID, is_codex, is_claude));
    }

    /// 모델도 같은 사고의 다른 면 — 걸러 내지 않으면 복원이
    /// `codex -m 'claude-opus-5[1m]'` 를 내보낸다(2026-09-05 실측).
    #[test]
    fn foreign_harness_model_is_dropped_and_never_reaches_the_command() {
        assert!(!saved_model_fits_agent(Some("codex"), "claude-opus-5[1m]"));
        assert!(!saved_model_fits_agent(Some("claude"), "gpt-5.6-sol"));
        assert!(!saved_model_fits_agent(None, "gpt-5.6-sol"));
        assert!(saved_model_fits_agent(Some("codex"), "gpt-5.6-sol"));
        assert!(saved_model_fits_agent(Some("claude"), "claude-opus-5[1m]"));
        assert!(saved_model_fits_agent(Some("agy"), "claude-opus-5[1m]"));

        // 복원이 실제로 쓰는 조합: 걸러낸 값은 명령에 아예 안 실린다.
        let clean = restore_agent_command(Some("codex"), Some("sid"), true, None, None);
        assert!(!clean.contains("claude-"), "{clean}");
        assert!(!clean.contains("model_reasoning_effort"), "모델을 버리면 effort 도 함께");
    }

    /// 하네스 갈래만 보는 판 — 모델·effort 는 아래 전용 테스트가 건다.
    fn cmd(agent: Option<&str>, sid: Option<&str>, resumable: bool) -> String {
        restore_agent_command(agent, sid, resumable, None, None)
    }

    /// 복원 명령의 마지막 갈래가 claude 라, 하네스를 여기 안 적으면 **오류 없이**
    /// claude 로 되살아난다. agy 를 붙이며 실제로 그 상태였다 — 하네스가 하나 더
    /// 늘 때 같은 함정에 다시 빠지지 않게 셋을 다 건다.
    #[test]
    fn every_harness_restores_as_itself() {
        for (agent, fresh) in [
            ("claude", "claude\r"),
            ("codex", "codex -c check_for_update_on_startup=false\r"),
            ("agy", "agy\r"),
        ] {
            assert_eq!(cmd(Some(agent), None, false), fresh, "{agent} 새로 띄우기");
        }
        assert_eq!(
            cmd(Some("claude"), Some("s1"), true),
            "claude --resume s1\r"
        );
        assert_eq!(
            cmd(Some("codex"), Some("s2"), true),
            "codex resume s2 -c check_for_update_on_startup=false\r"
        );
        assert_eq!(
            cmd(Some("agy"), Some("s3"), true),
            "agy --conversation s3\r"
        );
    }

    /// 이어갈 수 없는 세션(파일이 사라짐)은 **id 를 버리고** 새로 띄워야 한다 —
    /// 남의 하네스 id 를 넘기면 그 CLI 가 엉뚱한 대화를 물어온다.
    #[test]
    fn unresumable_drops_the_id() {
        assert_eq!(cmd(Some("agy"), Some("s3"), false), "agy\r");
        assert_eq!(cmd(Some("codex"), Some("s2"), false), "codex -c check_for_update_on_startup=false\r");
    }

    /// 모델·effort 문법이 하네스마다 다르다. 한 판에서 복붙하다 갈리기 쉬운 자리라
    /// 셋을 다 못박는다.
    #[test]
    fn each_harness_gets_its_own_model_and_effort_syntax() {
        let m = Some("claude-opus-5[1m]");
        assert_eq!(
            restore_agent_command(Some("claude"), Some("s1"), true, m, Some("xhigh")),
            "claude --resume s1 --model 'claude-opus-5[1m]' --effort 'xhigh'\r",
            "★ 작은따옴표가 빠지면 `[1m]` 이 zsh 글롭이라 명령이 통째 실패한다"
        );
        assert_eq!(
            restore_agent_command(
                Some("codex"),
                Some("s2"),
                true,
                Some("gpt-5.5"),
                Some("high")
            ),
            "codex resume s2 -c check_for_update_on_startup=false -m 'gpt-5.5' -c model_reasoning_effort='high'\r"
        );
        let codex = restore_agent_command(
            Some("codex"),
            Some("s2"),
            true,
            Some("gpt-5.6-sol"),
            Some("xhigh"),
        );
        assert_eq!(codex.matches("-c ").count(), 2, "서로 다른 config override 두 개");
        assert_eq!(
            restore_agent_command(Some("agy"), Some("s3"), true, None, Some("low")),
            "agy --conversation s3 --effort 'low'\r"
        );
    }

    /// 나쵸가 띄운 세션을 되살릴 때만 표식이 앞에 붙는다. 새로 띄우는 명령(세션 없음)에
    /// 붙으면 나쵸가 안 띄운 학생이 나쵸에게 보고할 자격을 얻는다.
    #[test]
    fn restore_reattaches_nacho_origin_only_to_the_resumed_session() {
        let origin = kasa_socket::nacho_inbox::Origin {
            conv: "discord:809".into(), task_id: "we409f946".into(), machine: String::new(), run: "we409f946.r2".into(),
        };
        assert_eq!(
            restore_agent_command_for(Some("claude"), Some("sid-1"), true, None, None, Some(&origin)),
            "KASATERM_ORIGIN=nacho KASATERM_ORIGIN_CONV='discord:809' KASATERM_ORIGIN_TASK='we409f946' KASATERM_ORIGIN_RUN='we409f946.r2' claude --resume sid-1\r"
        );
        assert!(restore_agent_command_for(Some("codex"), Some("sid-1"), true, None, None, Some(&origin))
            .starts_with("KASATERM_ORIGIN=nacho "), "codex 복원도 같은 표식");
        assert_eq!(
            restore_agent_command_for(Some("claude"), Some("sid-1"), false, None, None, Some(&origin)),
            "claude\r",
            "되살리지 않는 새 부팅엔 표식이 없다"
        );
        assert_eq!(restore_agent_command(Some("claude"), Some("s1"), true, None, None), "claude --resume s1\r", "기억이 없으면 전과 같다");
    }

    /// 값이 없으면 **플래그 자체가 빠져야** 한다 — 빈 문자열을 흘리면 `--model ''` 이
    /// 나가 하네스가 기본값조차 못 고른다. 옛 저장본엔 이 키가 아예 없다.
    #[test]
    fn missing_values_drop_the_flag_entirely() {
        assert_eq!(
            restore_agent_command(Some("claude"), Some("s1"), true, None, None),
            "claude --resume s1\r"
        );
        // 빈 문자열도 없음으로 친다(수집 쪽이 빈 값을 실어 보낼 수 있다).
        assert_eq!(
            restore_agent_command(Some("claude"), None, false, Some(""), Some("")),
            "claude\r"
        );
        // 한쪽만 있어도 그쪽만 붙는다 — effort 를 안 정한 세션이 흔하다.
        assert_eq!(
            restore_agent_command(Some("claude"), None, false, Some("sonnet"), None),
            "claude --model 'sonnet'\r"
        );
    }

    /// 저장본 어댑터. 옛 저장본(`{}`)이 `None` 이어야 위 "플래그를 뺀다"가 성립한다.
    #[test]
    fn saved_model_and_effort_fall_back_to_none() {
        let j = |s: &str| serde_json::from_str::<serde_json::Value>(s).unwrap();
        assert_eq!(
            saved_model(&j(r#"{"model":"claude-opus-5[1m]"}"#)),
            Some("claude-opus-5[1m]")
        );
        assert_eq!(saved_effort(&j(r#"{"effort":"xhigh"}"#)), Some("xhigh"));
        assert_eq!(saved_model(&j(r#"{}"#)), None, "옛 저장본");
        assert_eq!(saved_effort(&j(r#"{}"#)), None, "옛 저장본");
        // 빈 문자열이 새어 들어와도 없음으로 친다.
        assert_eq!(saved_model(&j(r#"{"model":""}"#)), None);
    }

    #[test]
    fn saved_agent_reads_agy_and_keeps_the_legacy_key() {
        let j = |s: &str| serde_json::from_str::<serde_json::Value>(s).unwrap();
        assert_eq!(saved_agent(&j(r#"{"was_agent":"agy"}"#)), Some("agy"));
        assert_eq!(saved_agent(&j(r#"{"was_agent":"codex"}"#)), Some("codex"));
        // 옛 저장본 — 이게 깨지면 판올림 한 번에 학생 pane 이 전부 셸이 된다.
        assert_eq!(saved_agent(&j(r#"{"was_claude":true}"#)), Some("claude"));
        assert_eq!(saved_agent(&j(r#"{}"#)), None);
    }

    #[test]
    fn restore_count_uses_only_pure_saved_markers() {
        let unknown = serde_json::json!({
            "was_agent": null,
            "session_id": "01900000-0000-7000-8000-000000000001"
        });
        assert_eq!(saved_agent_marker(&unknown), None);

        let source = include_str!("restore.rs");
        let count_body = source
            .split_once("pub(crate) fn count_claude_panes")
            .unwrap()
            .1
            .split_once("pub(crate) fn count_panes")
            .unwrap()
            .0;
        assert!(count_body.contains("saved_agent_marker(leaf)"));
        assert!(!count_body.contains("saved_agent(leaf)"));
        assert!(!count_body.contains("codex_root_rollout_for_session"));
    }

    #[test]
    fn null_agent_with_an_exact_codex_root_sid_recovers_as_codex() {
        const ROOT_ALPHA: &str = "01900000-0000-7000-8000-000000000001";
        const ROOT_BETA: &str = "01900000-0000-7000-8000-000000000002";
        let j = |sid: &str| {
            serde_json::json!({
                "was_agent": null,
                "session_id": sid,
                "cwd": "/workspace/shared"
            })
        };
        for sid in [ROOT_ALPHA, ROOT_BETA] {
            let mut rec = j(sid);
            let got = saved_agent_map_with(rec.as_object().unwrap(), |candidate| candidate == sid);
            assert_eq!(got, Some("codex"), "{sid}");
            assert!(restore_agent_command(got, Some(sid), true, None, None)
                .starts_with("codex resume "));
            normalize_saved_agent_map_with(rec.as_object_mut().unwrap(), |candidate| {
                candidate == sid
            });
            assert_eq!(rec["was_agent"], "codex", "저장본도 즉시 정규화: {sid}");
        }
    }

    #[test]
    fn explicit_claude_and_missing_rollouts_are_never_guessed_as_codex() {
        let claude_sid = "00000000-0000-4000-8000-000000000001";
        let explicit = serde_json::json!({
            "was_agent": "claude",
            "session_id": claude_sid
        });
        assert_eq!(
            saved_agent_map_with(explicit.as_object().unwrap(), |_| true),
            Some("claude"),
            "명시 하네스가 rollout 추론보다 우선"
        );
        let mut normalized = explicit.clone();
        normalize_saved_agent_map_with(normalized.as_object_mut().unwrap(), |_| true);
        assert_eq!(normalized["was_agent"], "claude");

        let unknown = serde_json::json!({
            "was_agent": null,
            "session_id": claude_sid,
            "character": "아루"
        });
        let agent = saved_agent_map_with(unknown.as_object().unwrap(), |_| false);
        assert_eq!(agent, None, "exact Codex root가 없으면 추측 금지");
        let mut normalized = unknown.clone();
        normalize_saved_agent_map_with(normalized.as_object_mut().unwrap(), |_| false);
        assert!(normalized["was_agent"].is_null());
        assert_eq!(
            restore_agent_command(agent, Some(claude_sid), false, None, None),
            "claude\r",
            "기존 sid-only fallback은 fresh Claude"
        );

        let explicit_codex = serde_json::json!({
            "was_agent": "codex",
            "session_id": "01900000-0000-7000-8000-000000000099"
        });
        let agent = saved_agent_map_with(explicit_codex.as_object().unwrap(), |_| false);
        assert_eq!(agent, Some("codex"));
        assert_eq!(
            restore_agent_command(agent, explicit_codex["session_id"].as_str(), false, None, None),
            "codex -c check_for_update_on_startup=false\r",
            "명시 Codex의 rollout이 사라졌으면 fresh Codex"
        );
    }
}

#[cfg(test)]
mod room_body_tests {
    #[test]
    fn saved_room_bodies_reads_the_active_session_only() {
        let state = serde_json::json!({
            "active_session": 1,
            "sessions": [
                { "room_body": { "0": "list" } },
                { "room_body": { "1": "list", "2": "map" } },
            ],
        });
        let mut got = crate::App::saved_room_bodies(&state);
        got.sort();
        assert_eq!(got, vec![(1, true), (2, false)]);
    }

    #[test]
    fn saved_room_bodies_is_empty_without_the_field() {
        // 옛 저장본(이 필드가 없던 판)은 빈 목록이어야 한다 — 그래야 모든 방이
        // 전역 기본을 따르는 예전 동작 그대로 뜬다.
        let state = serde_json::json!({ "sessions": [{}] });
        assert!(crate::App::saved_room_bodies(&state).is_empty());
    }
}

/// 복원이 **같은 학생을 둘 앉히지 않는가.**
///
/// 2026-08-26 실측: 후보가 넉넉한데도 아리스가 `%0`·`%9`, 세이아가 `%17`·`%19` 에
/// 겹쳐 있었다. 복원이 `pending_character` 로 배정을 건너뛰는데 그 지름길은 중복
/// 회피도 함께 건너뛰어서, 겹침이 하나 생기면 재시작마다 그대로 실려 누적된다.
#[cfg(test)]
mod revive_saved_character_tests {
    use super::revive_saved_character;

    #[test]
    fn a_free_character_is_revived() {
        assert!(revive_saved_character(false, false));
    }

    #[test]
    fn a_character_another_pane_holds_is_not_revived() {
        assert!(
            !revive_saved_character(true, false),
            "겹친 학생이 그대로 되살아난다"
        );
    }

    /// 손으로 고른 자리는 겹쳐도 지킨다 — 사람이 일부러 그랬다면 배정 사고가 아니다.
    #[test]
    fn a_hand_picked_seat_keeps_its_character_even_when_taken() {
        assert!(revive_saved_character(true, true));
        assert!(revive_saved_character(false, true));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn surface_key_save_close_reopen_preserves_identity_without_reusing_live_ids() {
        let alive = format!("%{}", uuid::Uuid::new_v4().as_u128() as u32);
        let restored = format!("surface-key-restored-{}", uuid::Uuid::new_v4());
        let old_key = kasa_mcp::surface_keys::ensure(&alive);
        let mut record = serde_json::Map::new();
        super::append_surface_record_metadata(&mut record, &alive);
        assert_eq!(record["surface_key"], old_key);
        // Close the original, then let an unrelated live shell reuse its number.
        kasa_mcp::surface_keys::remove(&alive);
        let newcomer_key = kasa_mcp::surface_keys::ensure(&alive);
        assert_ne!(newcomer_key, old_key);
        assert!(super::pick_restore_id(Some(&alive), |id| id == alive).is_none());
        let mut registered = super::RestoredSurfaceKey::register(&restored, &serde_json::Value::Object(record.clone()));
        registered.committed = true;
        drop(registered);
        assert_eq!(kasa_mcp::surface_keys::get(&alive), Some(newcomer_key));
        assert_eq!(kasa_mcp::surface_keys::get(&restored), Some(old_key.clone()));
        // A saved closed record keeps its key after real resource removal.
        assert_eq!(record["surface_key"], old_key);
        kasa_mcp::surface_keys::remove(&alive);
        kasa_mcp::surface_keys::remove(&restored);
    }

    #[test]
    fn surface_key_failed_restore_does_not_poison_a_reused_number() {
        let id = format!("surface-key-failed-{}", uuid::Uuid::new_v4());
        {
            let _pending = super::RestoredSurfaceKey::register(&id, &serde_json::json!({"surface_key": "saved-key"}));
            assert_eq!(kasa_mcp::surface_keys::get(&id).as_deref(), Some("saved-key"));
        }
        assert_eq!(kasa_mcp::surface_keys::get(&id), None);
        assert_ne!(kasa_mcp::surface_keys::ensure(&id), "saved-key");
        kasa_mcp::surface_keys::remove(&id);
    }

    #[test]
    fn surface_key_restore_reads_latest_backup_before_preparing_renumbered_state() {
        let dir = std::env::temp_dir().join(format!("kasaterm-surface-key-fixture-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&dir).unwrap();
        let previous = serde_json::json!({"leaf": {"pane_id": "%4", "session_id": "same-session"}});
        let current = serde_json::json!({"leaf": {"pane_id": "%3", "session_id": "same-session"}});
        let backup = dir.join("session-restored-200.json");
        std::fs::write(dir.join("session-restored-100.json"), b"{}").unwrap();
        std::fs::write(&backup, previous.to_string()).unwrap();
        std::fs::write(dir.join("session.json"), current.to_string()).unwrap();
        let saved = super::latest_restored_state(&dir).unwrap();
        assert_eq!(saved["leaf"]["pane_id"], "%4");
        assert_eq!(saved["leaf"]["surface_key"], "legacy:%4");
        let prepared = kasa_mcp::surface_keys::prepare_restore_state(&current, Some(&saved));
        assert_eq!(prepared["leaf"]["pane_id"], "%3");
        assert_eq!(prepared["leaf"]["surface_key"], "legacy:%4");
        assert_eq!(serde_json::from_slice::<serde_json::Value>(&std::fs::read(&backup).unwrap()).unwrap(), previous);
        std::fs::write(dir.join("session-restored-300.json"), b"invalid").unwrap();
        assert!(super::latest_restored_state(&dir).is_none(), "do not guess from an older table after corruption");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn surface_key_backup_lineage_survives_multiple_legacy_restarts_and_new_pane() {
        let dir = std::env::temp_dir().join(format!("kasaterm-surface-lineage-fixture-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&dir).unwrap();
        let records = |pairs: &[(&str, &str)]| serde_json::json!({"windows": pairs.iter()
            .map(|(id, sid)| serde_json::json!({"leaf":{"pane_id":id,"session_id":sid}})).collect::<Vec<_>>()});
        let old = records(&[("%4", "a"), ("%2", "b"), ("%7", "c"), ("%8", "d")]);
        let renamed = records(&[("%3", "a"), ("%0", "b"), ("%1", "c"), ("%2", "d")]);
        // Write the oldest last, deliberately reversing mtimes: ordering must
        // follow the restore timestamp, not when a backup happened to be copied.
        std::fs::write(dir.join("session-restored-1788966036.json"), renamed.to_string()).unwrap();
        std::fs::write(dir.join("session-restored-1788966035.json"), renamed.to_string()).unwrap();
        std::fs::write(dir.join("session-restored-1788964315.json"), old.to_string()).unwrap();
        let previous = super::latest_restored_state(&dir).unwrap();
        let current = records(&[("%4", "new-session"), ("%3", "a"), ("%0", "b"), ("%1", "c"), ("%2", "d")]);
        let prepared = kasa_mcp::surface_keys::prepare_restore_state(&current, Some(&previous));
        for (index, old_id) in ["%4", "%2", "%7", "%8"].iter().enumerate() {
            assert_eq!(prepared["windows"][index + 1]["leaf"]["surface_key"], format!("legacy:{old_id}"));
        }
        assert!(uuid::Uuid::parse_str(prepared["windows"][0]["leaf"]["surface_key"].as_str().unwrap()).is_ok());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn surface_key_corrupt_checkpoint_breaks_lineage_and_corrupt_latest_returns_none() {
        let dir = std::env::temp_dir().join(format!("kasaterm-surface-corrupt-fixture-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join("session-restored-100.json"),
            serde_json::json!({"leaf":{"pane_id":"%4","session_id":"same"}}).to_string()).unwrap();
        std::fs::write(dir.join("session-restored-200.json"), b"corrupt").unwrap();
        std::fs::write(dir.join("session-restored-300.json"),
            serde_json::json!({"leaf":{"pane_id":"%3","session_id":"same"}}).to_string()).unwrap();
        let restarted = super::latest_restored_state(&dir).unwrap();
        assert_eq!(restarted["leaf"]["surface_key"], "legacy:%3", "never bridge across a corrupt identity checkpoint");
        std::fs::write(dir.join("session-restored-400.json"), b"corrupt").unwrap();
        assert!(super::latest_restored_state(&dir).is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// 부팅 예약과 복원이 같은 목록을 봐야 한다 — 되살릴 세션의 leaf 를 트리 깊이와
    /// 무관하게 순서대로 모으고, 빈 이름과 다른 세션은 건너뛴다.
    #[test]
    fn saved_characters_walks_active_session_leaves() {
        let state = serde_json::json!({
            "active_session": 1,
            "sessions": [
                {"windows": [{"leaf": {"character": "다른세션"}}]},
                {"windows": [
                    {"split": {"a": {"leaf": {"character": "호시노"}},
                               "b": {"split": {"a": {"leaf": {"character": ""}},
                                               "b": {"leaf": {"character": "세이아"}}}}}},
                    {"leaf": {"character": "유즈"}}
                ]}
            ]
        });
        assert_eq!(
            super::App::saved_characters(&state),
            vec!["호시노", "세이아", "유즈"]
        );
        assert!(super::App::saved_characters(&serde_json::json!({})).is_empty());
    }

    #[test]
    fn agent_restore_drops_terminal_history_but_shell_restore_keeps_it() {
        let rec = serde_json::json!({ "scrollback": ["old output", "old prompt"] });
        assert!(restored_scrollback(&rec, true).is_empty());
        assert_eq!(
            restored_scrollback(&rec, false),
            vec!["old output".to_string(), "old prompt".to_string()]
        );
    }

    #[test]
    fn restore_keeps_the_saved_pane_id() {
        assert_eq!(
            pick_restore_id(Some("%9"), |_| false).as_deref(),
            Some("%9")
        );
    }

    #[test]
    fn restore_falls_back_when_the_id_is_missing_or_taken() {
        // 옛 저장본엔 pane_id 가 없다 → 호출부가 새 번호를 받는다.
        assert_eq!(pick_restore_id(None, |_| false), None);
        // 이미 살아 있는 번호는 뺏지 않는다.
        assert_eq!(pick_restore_id(Some("%1"), |s| s == "%1"), None);
        // `%` 없는 쓰레기 값도 폴백.
        assert_eq!(pick_restore_id(Some("garbage"), |_| false), None);
    }
}
