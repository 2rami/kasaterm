//! 다른 기기 칸 — 원격 셸·거울 칸을 세우고, 원격 방 보기 창의 자리·나눔선·구성원을
//! 원본에 맞추고, 기기를 펼친다.
use super::*;

pub(super) fn self_view_machine() -> Result<kasa_mcp::machines::Machine> {
    let port = std::env::var("KASASPACE_MCP_PORT")
        .map_err(|_| anyhow::anyhow!("이 기기 HTTP 창구가 안 떠 있어요"))?;
    Ok(kasa_mcp::machines::Machine {
        label: kasa_mcp::machines::self_label(),
        machine_id: None,
        base: format!("http://127.0.0.1:{port}"),
        host: String::new(),
        kvm: None,
        roots: Vec::new(),
        home: false,
        ssh: None,
        chrome_port: None,
        key: None,
        tunneled: false,
        guest: false,
    })
}

impl App {
    /// 이 pane 을 **그 자리에서** 원격 셸의 거울로 바꾼다 — `mini` 한 마디로 맥미니
    /// 터미널이 보이게(2026-09-02 지시). 학생·대화 운반이 없는 맨 셸 전용이라
    /// `migrate_pane` 의 레포 준비·학생 소환은 타지 않고, 승격(`promote_pane`)과 같은
    /// 스왑 패턴만 쓴다: 같은 pane id 로 원격 세션을 앉히고 옛 로컬 셸은 insert 의
    /// Drop 이 걷는다. 그래서 자리·크기·방·학생 배정이 그대로다. 부른 CLI 는 그
    /// 셸의 자식이라 회신을 못 받고 함께 걷힌다 — 성공의 표시는 화면이 바뀌는 것이다.
    pub(crate) fn remote_shell_here(
        &mut self,
        pid: &str,
        base: &str,
        remote_cwd: Option<&str>,
    ) -> Result<String> {
        self.ensure_user_mutation_target(
            pid,
            crate::settings_room::SettingsMutation::RemotePane,
        )?;
        if self.tmux.is_some() {
            anyhow::bail!("tmux 백엔드에선 원격 pane 을 쓰지 않는다");
        }
        let Some(sess) = self.pty.get(pid).cloned() else {
            anyhow::bail!("pane {pid} 이 없다");
        };
        if kasa_mcp::remote::is_remote_pane(pid) {
            anyhow::bail!("{pid} 은 이미 원격 pane 이다");
        }
        // 역이사(`migrate %N local`)가 돌아올 자리 — 지금 이 로컬 폴더.
        let origin = sess
            .shell_pid()
            .and_then(socket::pid_cwd)
            .map(|p| p.to_string_lossy().into_owned());
        let (c, r) = sess.size();
        let identity = kasa_mcp::remote::RemoteIdentity {
            label: kasa_mcp::machines::label_for_base(base).unwrap_or_default(),
            remote_cwd: remote_cwd.map(str::to_string),
            origin_cwd: origin,
            owned: true,
        };
        // 저쪽 **창에 진짜 pane** 을 세우고 여기서 비춘다(2026-09-07 지시 「to 로 붙으면
        // 거기도 생기게 — 원격을 터미널로 조종한다는 느낌으로」). 창 없는 web 셸은 그
        // 기계 앞에 앉으면 안 보였다. 창구가 없는 낡은 서버·kasa-serve-web 은 옛
        // 길(web 셸)로 물러선다. 거울(view)로 붙는 이유는 그 pane 의 크기 주인이
        // 저쪽 창이라서다 — 이쪽 pane 이 작으면 글자 배율로 담는다.
        let remote = match kasa_mcp::remote::spawn_shell_pane(base, remote_cwd, None) {
            Ok(rid) => {
                let spec = kasa_mcp::remote::RemoteSpec {
                    base: base.to_string(),
                    pane: Some(rid.clone()),
                    cwd: None,
                    token: None,
                    identity: identity.clone(),
                };
                match kasa_mcp::remote::connect_view(spec, pid) {
                    Ok(r) => r,
                    Err(e) => {
                        // 세운 자리를 못 비추면 저쪽에 빈 pane 만 남는다 — 걷고 실패.
                        let _ = kasa_mcp::remote::close_remote_pane(base, &rid, None, true);
                        return Err(e);
                    }
                }
            }
            Err(e) => {
                eprintln!("[to] {base} 에 pane 을 못 세워 창 없는 셸로 물러섬: {e:#}");
                kasa_mcp::remote::connect(
                    kasa_mcp::remote::RemoteSpec {
                        base: base.to_string(),
                        pane: None,
                        cwd: remote_cwd.map(str::to_string),
                        token: None,
                        identity,
                    },
                    pid,
                    c,
                    r,
                )?
            }
        };
        // Register the replacement before its first queued snapshot can be pumped.
        // Otherwise pane_replaced sees the old local Arc and stops the new pump.
        self.insert_pty(pid.to_string(), remote.session.clone());
        self.pump_pty_screens(
            remote.session.screens.clone(),
            pid.to_string(),
            std::sync::Arc::downgrade(&remote.session),
        );
        // 옛 세션의 늦은 죽음표시 정리(스왑 패턴) — 정체 가드가 있지만 이중으로.
        self.dead_panes.lock().unwrap().retain(|x| x != pid);
        let (wc, wr) = self.window_cells();
        self.resize_backend(wc, wr);
        self.publish_pty_layout();
        if let Some(w) = &self.window {
            w.request_redraw();
        }
        Ok(pid.to_string())
    }

    /// 원격 PTY 호스트의 세션을 **이 창의 pane** 으로 앉힌다 — 스폰 또는 이어받기.
    ///
    /// `split_active_pane` 과 같은 삽입 규칙(기준 pane 이 든 트리에 꽂는다)을 쓰되,
    /// 로컬 셸 대신 `kasa_mcp::remote::connect` 의 로컬 파서 세션을 앉힌다. 방향은
    /// v1 에선 가로 고정 — auto 판정(픽셀 종횡비)은 활성 pane 전제라 배경 스폰과
    /// 어긋난다.
    pub(crate) fn spawn_remote_pane(
        &mut self,
        base: &str,
        cwd: Option<&str>,
        remote_pane: Option<&str>,
        from: Option<&str>,
    ) -> Result<String> {
        if self.tmux.is_some() {
            anyhow::bail!("tmux 백엔드에선 원격 pane 을 쓰지 않는다");
        }
        let anchor = from
            .map(str::to_string)
            .or_else(|| self.ws.lock().unwrap().active_pane.clone())
            .ok_or_else(|| anyhow::anyhow!("기준 pane 이 없다"))?;
        let anchor = self
            .ws
            .lock()
            .unwrap()
            .outer_for_pty(&anchor)
            .unwrap_or(anchor);
        self.ensure_user_mutation_target(
            &anchor,
            crate::settings_room::SettingsMutation::RemotePane,
        )?;
        // 연결 칸은 거울이 아니라 이쪽이 쥔 칸이다 — 보기 창에 섞이면 그 방이 이 기기 방이 된다.
        let anchor = self.own_spawn_host(&anchor);
        let owner = self.window_of_pane(&anchor);
        if owner.is_none() {
            anyhow::bail!("기준 pane {anchor} 이 없다 — 종료·재시작으로 사라졌는지 확인해라");
        }
        let new_id = self.alloc_pane_id();
        let (win_cols, win_rows) = self.window_cells();
        let remote = kasa_mcp::remote::connect(
            kasa_mcp::remote::RemoteSpec {
                base: base.to_string(),
                pane: remote_pane.map(str::to_string),
                cwd: cwd.map(str::to_string),
                token: None,
                identity: kasa_mcp::remote::RemoteIdentity {
                    label: kasa_mcp::machines::label_for_base(base).unwrap_or_default(),
                    remote_cwd: cwd.map(str::to_string),
                    origin_cwd: None,
                    owned: false,
                },
            },
            &new_id,
            win_cols,
            win_rows,
        )?;
        self.pump_pty_screens(
            remote.session.screens.clone(),
            new_id.clone(),
            std::sync::Arc::downgrade(&remote.session),
        );
        self.insert_pty(new_id.clone(), remote.session.clone());
        let foreign = owner.filter(|w| *w != self.active_window);
        let layout = match foreign {
            Some(w) => self.windows.get_mut(w).and_then(|s| s.as_mut()),
            None => self.pty_layout.as_mut(),
        };
        if !layout
            .is_some_and(|l| l.split_leaf(&anchor, kasa_pty::SplitDir::Horizontal, new_id.clone()))
        {
            // 이어받기(attach) 실패 롤백에서 남의 세션을 죽이는 쪽이 훨씬 나쁘므로
            // kill 은 안 한다 — Arc drop 이 detach 만 한다(새 스폰의 고아는 원격
            // keep 목록에 남아 다음에 이어받거나 걷을 수 있다).
            self.pty.remove(&new_id);
            anyhow::bail!("pane {anchor} 을 어느 window 트리에서도 못 찾았다");
        }
        // 몸통이 남의 기계라도 **이 창의 학생은 같은 사람**이어야 한다 — 안 그러면
        // 이름·색·얼굴이 없는 무명 pane 이 된다(사용자: 「옮기면 왜 테마가 없어져」).
        if let Some(name) = remote_pane
            .and_then(|p| kasa_mcp::remote::remote_pane_character(base, p, None))
            .filter(|n| !n.is_empty())
        {
            self.relabel_pane(&new_id, &name);
        }
        if foreign.is_none() {
            self.ws.lock().unwrap().active_pane = Some(new_id.clone());
            self.handoff_ime_to_active_surface();
            self.resize_backend(win_cols, win_rows);
            self.publish_pty_layout();
            if let Some(w) = &self.window {
                w.request_redraw();
            }
        } else {
            self.publish_pty_layout();
            self.chrome_dirty = true;
        }
        Ok(new_id)
    }

    /// 원격 학생 하나를 **포커스된 pane 의 탭**으로 거울 낸다(2026-09-03 지시
    /// 「거울 버튼 말고 그냥 누르면 포커스된 pane 탭 안에 띄워지게」). 옆에
    /// 쪼개 앉히던 09-02 판은 누를수록 창이 갈라졌다 — 탭은 자리를 안 뺏고,
    /// 다 봤으면 탭만 닫으면 된다. 이미 그 학생의 거울이 있으면 새로 안 만들고
    /// 그 창·그 pane·그 탭으로 간다.
    pub(crate) fn mirror_remote_pane(
        &mut self,
        label: &str,
        remote_id: &str,
        name: &str,
        remote_cwd: &str,
    ) -> Result<String> {
        self.mirror_remote_pane_at(None, label, remote_id, name, remote_cwd, true)
    }

    /// `mirror_remote_pane` 의 자리 지정판 — `anchor` 칸의 탭으로(없으면 포커스된 칸). `focus` 가 거짓이면 보던
    /// 화면을 안 뺏는다: 다른 기기가 이 기기에 거울을 열어 줄 때(`collab.act attach`) 사람이 보던 것이 그대로 남아야 한다.
    pub(crate) fn mirror_remote_pane_at(
        &mut self,
        anchor: Option<String>,
        label: &str,
        remote_id: &str,
        name: &str,
        remote_cwd: &str,
        focus: bool,
    ) -> Result<String> {
        if self.tmux.is_some() {
            anyhow::bail!("tmux 백엔드에선 원격 pane 을 쓰지 않는다");
        }
        let anchor = match anchor {
            Some(pane) => pane,
            None => self
                .ws
                .lock()
                .unwrap()
                .active_pane
                .clone()
                .ok_or_else(|| anyhow::anyhow!("포커스된 pane 이 없다"))?,
        };
        // 포커스가 보조 탭에 있어도 새 탭은 그 바깥 pane 에 붙는다.
        let outer = self
            .ws
            .lock()
            .unwrap()
            .outer_for_pty(&anchor)
            .unwrap_or(anchor);
        self.ensure_user_mutation_target(
            &outer,
            crate::settings_room::SettingsMutation::RemoteMirror,
        )?;
        let m = match kasa_mcp::machines::find(label) {
            Some(m) => m,
            // 이 기기의 창 밖 셸 — 명부엔 제 자신이 없으니 제 HTTP 창구로 붙는다. 보기 연결이라
            // 폰이 쥔 셸 크기를 안 건드리고, 셸이면 거울 셸처럼 명령 묶음으로 그려진다.
            None if label == kasa_mcp::machines::self_label() => self_view_machine()?,
            None => anyhow::bail!("기계 {label} 가 명부(machines.json)에 없다"),
        };
        if let Some(existing) = kasa_pty::live_sessions().into_iter().find(|id| {
            kasa_mcp::remote::remote_info(id)
                .is_some_and(|i| i.base == m.base && i.remote_id == remote_id)
        }) {
            if focus {
                self.reveal_pane_tab(&existing);
            }
            return Ok(existing);
        }
        if self.window_of_pane(&outer).is_none() {
            anyhow::bail!("pane {outer} 이 없다 — 종료·재시작으로 사라졌는지 확인해라");
        }
        self.attach_remote_view_tab(&outer, &m, remote_id, name, remote_cwd, focus)
    }

    /// 거울 하나를 `outer` 의 탭으로 앉힌다. `focus` 면 그 탭을 앞으로 내고 포커스까지 —
    /// 사람이 열 때. 보기 창 자동 동기(`sync_remote_view_members`)는 false 로 불러 보고 있던
    /// 탭과 포커스를 안 건드린다.
    pub(super) fn attach_remote_view_tab(
        &mut self,
        outer: &str,
        m: &kasa_mcp::machines::Machine,
        remote_id: &str,
        name: &str,
        remote_cwd: &str,
        focus: bool,
    ) -> Result<String> {
        let new_id = self.alloc_pane_id();
        let remote = kasa_mcp::remote::connect_view(
            kasa_mcp::remote::RemoteSpec {
                base: m.base.clone(),
                pane: Some(remote_id.to_string()),
                cwd: None,
                token: None,
                identity: kasa_mcp::remote::RemoteIdentity {
                    label: m.label.clone(),
                    remote_cwd: (!remote_cwd.is_empty()).then(|| remote_cwd.to_string()),
                    origin_cwd: None,
                    owned: false,
                },
            },
            &new_id,
        )?;
        self.pump_pty_screens(
            remote.session.screens.clone(),
            new_id.clone(),
            std::sync::Arc::downgrade(&remote.session),
        );
        self.insert_pty(new_id.clone(), remote.session.clone());
        self.dead_panes.lock().unwrap().retain(|x| x != &new_id);
        {
            // spawn_new_tab 과 같은 대접 — 탭은 트리를 안 바꾸고 pid_to_pane 과
            // 바깥 pane 의 탭 목록만 늘린다. 방도 바깥 pane 것을 물려받는다.
            let mut ws = self.ws.lock().unwrap();
            ws.pid_to_pane.insert(new_id.clone(), outer.to_string());
            if let Some(r) = ws.pane_room.get(outer).cloned() {
                ws.pane_room.insert(new_id.clone(), r);
            }
            let pane = ws.panes.entry(outer.to_string()).or_default();
            let mut tab = PaneTab::default();
            tab.pid = Some(new_id.clone());
            pane.tabs.push(tab);
            if focus {
                pane.active_tab = pane.tabs.len() - 1;
            }
            pane.dirty = true;
            if focus {
                ws.active_pane = Some(outer.to_string());
            }
        }
        if focus {
            self.handoff_ime_to_active_surface();
        }
        // 몸통이 남의 기계라도 이 창의 학생은 같은 사람 — 무명 탭 방지.
        let name = if name.is_empty() {
            kasa_mcp::remote::remote_pane_character(&m.base, remote_id, None).unwrap_or_default()
        } else {
            name.to_string()
        };
        if !name.is_empty() {
            self.relabel_pane(&new_id, &name);
        }
        let (win_cols, win_rows) = self.window_cells();
        self.resize_backend(win_cols, win_rows);
        self.publish_pty_layout();
        self.session_touched = true;
        self.chrome_dirty = true;
        if let Some(w) = &self.window {
            w.request_redraw();
        }
        Ok(new_id)
    }

    /// 보기 창이 원본과 어긋난 자리를 바로잡는다 — 원본에선 탭인데 여기선 옆 칸으로 앉은
    /// 거울은 그 바깥 거울의 탭으로 접고(옛 `unfold`·복원본·경쟁 착지), 원본에서 다른 방으로
    /// 옮겨 간 pane 의 거울은 걷는다(5초 넘게 그대로일 때 — 그 방의 보기 창이 있으면 그쪽이
    /// 다시 앉힌다). 어긋난 leaf 하나가 좌표 동기 전체를 멈추던 것을 푼다(2026-09-18).
    /// 반환값 = 무엇이든 바꿨는가.
    pub(super) fn repair_remote_view_window(&mut self, window: usize, label: &str, panes: &[serde_json::Value]) -> bool {
        use std::collections::HashMap;
        use std::sync::{Mutex, OnceLock};
        static MOVED_SINCE: OnceLock<Mutex<HashMap<String, Instant>>> = OnceLock::new();
        let leaves = self.window_leaves(window);
        if leaves.len() < 2 {
            return false;
        }
        let row_of = |rid: &str| panes.iter().find(|p| p.get("id").and_then(|v| v.as_str()) == Some(rid));
        let source_of = |s: &Self, leaf: &str| kasa_mcp::remote::remote_info(&s.leaf_pty_id(leaf)).map(|i| i.remote_id);
        let mut outer_of: HashMap<String, String> = HashMap::new();
        let mut windows: Vec<u64> = Vec::new();
        for leaf in &leaves {
            let Some(rid) = source_of(self, leaf) else { continue };
            if let Some(w) = row_of(&rid).and_then(|r| r.get("window").and_then(|v| v.as_u64())) {
                windows.push(w);
            }
            outer_of.insert(rid, leaf.clone());
        }
        // 이 창이 비추는 원본 방 = 거울 원본이 가장 많이 앉은 방.
        let mut counts: HashMap<u64, usize> = HashMap::new();
        for w in &windows { *counts.entry(*w).or_default() += 1; }
        let Some((&home, _)) = counts.iter().max_by_key(|(w, n)| (**n, std::cmp::Reverse(**w))) else { return false };
        for leaf in &leaves {
            let Some(rid) = source_of(self, leaf) else { continue };
            let Some(row) = row_of(&rid) else { continue };
            let key = format!("{label}/{rid}");
            let in_home = row.get("window").and_then(|v| v.as_u64()) == Some(home);
            if !in_home {
                let since = *MOVED_SINCE.get_or_init(Default::default).lock().unwrap()
                    .entry(key.clone()).or_insert_with(Instant::now);
                if since.elapsed() < std::time::Duration::from_secs(5) {
                    continue;
                }
                MOVED_SINCE.get_or_init(Default::default).lock().unwrap().remove(&key);
                eprintln!("[view] {label} {rid} 는 원본에서 다른 방으로 갔다 — 이 보기 창에서 걷는다");
                self.remote_keep.insert(leaf.clone());
                self.remove_pane(leaf);
                return true;
            }
            MOVED_SINCE.get_or_init(Default::default).lock().unwrap().remove(&key);
            let Some(outer_rid) = row.get("tab_of").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) else { continue };
            let Some(outer) = outer_of.get(outer_rid).cloned().filter(|o| o != leaf) else { continue };
            eprintln!("[view] {label} {rid} 는 원본에선 {outer_rid} 의 탭 — 옆 칸을 탭으로 접는다");
            self.fold_leaf_into_tabs(window, leaf, &outer);
            return true;
        }
        false
    }

    /// leaf `src` 를 같은 창의 `dst` 탭으로 접는다 — 활성 창이면 `merge_pane_into_tabs`,
    /// 안 보이는 창이면 그 창의 트리에서 직접(그 함수는 활성 트리만 만진다).
    pub(super) fn fold_leaf_into_tabs(&mut self, window: usize, src: &str, dst: &str) {
        {
            // 거울 leaf 의 첫 탭은 pid 가 비어 있다(leaf 번호가 곧 PTY) — 탭으로 옮기면 그
            // 번호를 달아야 화면이 그 탭으로 배달된다.
            let mut ws = self.ws.lock().unwrap();
            if let Some(p) = ws.panes.get_mut(src) {
                for t in p.tabs.iter_mut().filter(|t| t.pid.is_none()) {
                    t.pid = Some(src.to_string());
                }
            }
        }
        if window == self.active_window {
            self.merge_pane_into_tabs(src, dst);
            return;
        }
        let moved: Vec<PaneTab> = {
            let mut ws = self.ws.lock().unwrap();
            if !ws.panes.contains_key(dst) { return }
            match ws.panes.get_mut(src) {
                Some(s) if !s.tabs.is_empty() => std::mem::take(&mut s.tabs),
                _ => return,
            }
        };
        {
            let mut ws = self.ws.lock().unwrap();
            for t in &moved {
                if let Some(pid) = t.pid.clone() {
                    ws.pid_to_pane.insert(pid, dst.to_string());
                }
            }
            if let Some(d) = ws.panes.get_mut(dst) {
                d.tabs.extend(moved);
                d.dirty = true;
            }
            ws.panes.remove(src);
        }
        if let Some(tree) = self.windows.get_mut(window).and_then(|w| w.as_mut()) {
            tree.remove_leaf(src);
        }
        self.publish_pty_layout();
        self.session_touched = true;
        self.chrome_dirty = true;
    }

    /// 보기 창 `window` 에 원본 pane 하나의 거울 leaf 를 새로 앉힌다 — 자리는 첫 leaf 옆이고,
    /// 정확한 칸은 바로 뒤의 좌표 동기가 원본대로 잡는다.
    /// `at` 은 `(기준 leaf, 축, 앞에)` — 없으면 첫 leaf 오른쪽.
    pub(crate) fn seat_remote_view_leaf(
        &mut self,
        window: usize,
        m: &kasa_mcp::machines::Machine,
        remote_id: &str,
        name: &str,
        remote_cwd: &str,
        at: Option<(String, kasa_pty::SplitDir, bool)>,
    ) -> Result<String> {
        let (anchor, dir, before) = match at {
            Some(at) => at,
            None => match self.window_leaves(window).first().cloned() {
                Some(first) => (first, kasa_pty::SplitDir::Horizontal, false),
                None => anyhow::bail!("보기 창이 비어 있다"),
            },
        };
        let new_id = self.alloc_pane_id();
        let remote = kasa_mcp::remote::connect_view(
            kasa_mcp::remote::RemoteSpec {
                base: m.base.clone(),
                pane: Some(remote_id.to_string()),
                cwd: None,
                token: None,
                identity: kasa_mcp::remote::RemoteIdentity {
                    label: m.label.clone(),
                    remote_cwd: (!remote_cwd.is_empty()).then(|| remote_cwd.to_string()),
                    origin_cwd: None,
                    owned: false,
                },
            },
            &new_id,
        )?;
        self.insert_pty(new_id.clone(), remote.session.clone());
        self.pump_pty_screens(
            remote.session.screens.clone(),
            new_id.clone(),
            std::sync::Arc::downgrade(&remote.session),
        );
        self.dead_panes.lock().unwrap().retain(|x| x != &new_id);
        self.ws.lock().unwrap().panes.entry(new_id.clone()).or_default();
        let active = window == self.active_window;
        let planted = {
            let tree = if active { self.pty_layout.as_mut() }
                else { self.windows.get_mut(window).and_then(|w| w.as_mut()) };
            tree.is_some_and(|t| t.insert_beside(&anchor, dir, before, new_id.clone()))
        };
        if !planted {
            self.remove_pane(&new_id);
            anyhow::bail!("보기 창 트리에 못 끼웠다");
        }
        let name = if name.is_empty() {
            kasa_mcp::remote::remote_pane_character(&m.base, remote_id, None).unwrap_or_default()
        } else {
            name.to_string()
        };
        if !name.is_empty() {
            self.relabel_pane(&new_id, &name);
        }
        if active {
            let (cols, rows) = self.window_cells();
            self.resize_backend(cols, rows);
        }
        self.publish_pty_layout();
        self.session_touched = true;
        self.chrome_dirty = true;
        Ok(new_id)
    }

    /// 보기 창의 **구성원**을 원본 방에 맞춘다 — 원본에 새로 생긴 pane 은 거울 leaf 로, 새로
    /// 생긴 탭은 그 바깥 거울의 탭으로. 열 때만 맞추고 그 뒤로는 칸 좌표만 따라가서, 나중에
    /// 원본에서 만든 탭이 거울엔 없고 미니맵에만 보였다(2026-09-18 지적 「탭 안에 있던 게 안
    /// 보이고 미니맵에서는 보여」). 없어진 것은 여기서 걷지 않는다 — 원본 소켓의 gone 이 한다.
    /// 못 앉힌 것은 30초 뒤에 다시 시도한다(매 틱 연결을 시도하면 화면이 멈춘다).
    pub(super) fn sync_remote_view_members(&mut self) {
        use std::collections::{HashMap, HashSet};
        use std::sync::{Mutex, OnceLock};
        static RETRY: OnceLock<Mutex<HashMap<String, Instant>>> = OnceLock::new();
        if self.tmux.is_some() || self.restore_applying.is_some() {
            return;
        }
        let snap = kasa_mcp::machines::snapshot();
        for i in 0..self.windows.len() {
            let Some((label, _)) = self.remote_view_of_window(i) else { continue };
            let Some(m) = kasa_mcp::machines::find(&label) else { continue };
            let Some(machine) = snap.iter().find(|v| v.get("label").and_then(|l| l.as_str()) == Some(label.as_str())) else { continue };
            if machine.get("online").and_then(|b| b.as_bool()) != Some(true) {
                continue;
            }
            let Some(panes) = machine.get("panes").and_then(|p| p.as_array()) else { continue };
            // 이미 비추는 원본 pane 들(leaf + 탭)과, 원본 바깥 pane → 이쪽 leaf.
            let mut present: HashSet<String> = HashSet::new();
            let mut outer_of: HashMap<String, String> = HashMap::new();
            let per_leaf: Vec<(String, Vec<String>)> = {
                let ws = self.ws.lock().unwrap();
                self.window_leaves(i).into_iter().map(|leaf| {
                    let mut ids: Vec<String> = ws.panes.get(&leaf)
                        .map(|p| p.tabs.iter().filter_map(|t| t.pid.clone()).collect())
                        .unwrap_or_default();
                    if ids.first() != Some(&leaf) && !ids.iter().any(|id| id == &leaf) {
                        ids.insert(0, leaf.clone());
                    }
                    (leaf, ids)
                }).collect()
            };
            let mut source_window: Option<u64> = None;
            let mut consistent = true;
            for (leaf, ids) in &per_leaf {
                for (k, id) in ids.iter().enumerate() {
                    let Some(info) = kasa_mcp::remote::remote_info(id) else { continue };
                    if k == 0 {
                        outer_of.insert(info.remote_id.clone(), leaf.clone());
                        if let Some(w) = kasa_mcp::machines::cached_pane(&label, &info.remote_id)
                            .and_then(|row| row.get("window").and_then(|v| v.as_u64()))
                        {
                            if source_window.is_some_and(|s| s != w) { consistent = false; }
                            source_window = Some(w);
                        }
                    }
                    present.insert(info.remote_id);
                }
            }
            let (true, Some(source_window)) = (consistent, source_window) else {
                self.repair_remote_view_window(i, &label, panes);
                continue;
            };
            if self.repair_remote_view_window(i, &label, panes) {
                // 구성이 바뀌었다 — 다음 바퀴에 새 모습으로 다시 본다.
                continue;
            }
            // 원본 방의 pane 들 — 바깥이 먼저, 탭은 그 다음(바깥이 같은 바퀴에 생겨도 붙는다).
            let mut rows: Vec<&serde_json::Value> = panes.iter().filter(|p| {
                p.get("window").and_then(|v| v.as_u64()) == Some(source_window)
                    && p.get("mirror_of").and_then(|v| v.as_str()).is_none()
                    && !crate::machinescol::remote_pane_closed(p)
                    && p.get("id").and_then(|v| v.as_str()).is_some_and(|id| id.starts_with('%'))
            }).collect();
            let tab_of = |p: &serde_json::Value| p.get("tab_of").and_then(|v| v.as_str())
                .filter(|s| !s.is_empty()).map(str::to_string);
            rows.sort_by_key(|p| tab_of(p).is_some());
            for row in rows {
                let rid = row.get("id").and_then(|v| v.as_str()).unwrap_or_default().to_string();
                if present.contains(&rid) {
                    continue;
                }
                let key = format!("{label}/{rid}");
                if RETRY.get_or_init(Default::default).lock().unwrap().get(&key)
                    .is_some_and(|t| t.elapsed() < std::time::Duration::from_secs(30))
                {
                    continue;
                }
                let name = row.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let cwd = row.get("cwd").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let outcome = match tab_of(row) {
                    Some(outer_rid) => match outer_of.get(&outer_rid).cloned() {
                        Some(outer) => self.attach_remote_view_tab(&outer, &m, &rid, &name, &cwd, false).map(|_| ()),
                        // 바깥이 이 창에 없다 — 바깥이 거울 대상이 아니거나 다음 바퀴에.
                        None => continue,
                    },
                    None => self.seat_remote_view_leaf(i, &m, &rid, &name, &cwd, None)
                        .map(|local| { outer_of.insert(rid.clone(), local); }),
                };
                match outcome {
                    Ok(()) => { present.insert(rid); }
                    Err(e) => {
                        eprintln!("[view] {label} {rid} 거울을 못 앉혔다: {e:#}");
                        RETRY.get_or_init(Default::default).lock().unwrap().insert(key, Instant::now());
                    }
                }
            }
        }
    }

    /// pid 가 보조 탭이면 그 바깥 pane 을 포커스하고 그 탭을 앞으로 — 창이 다르면
    /// 창부터 바꾼다. `focus_pane` 은 바깥 pane 만 알아서 탭 pid 로는 못 찾는다.
    /// 다른 기기의 방 하나를 **여기 방으로** 연다 — 사이드바에서 그 방(또는 그 안의 칸)을
    /// 눌렀을 때. 이미 그 방의 거울이 여기 있으면 그리로 간다(누른 pane 의 거울이 있으면
    /// 그것). 없으면 새 창을 세우고 바깥 pane 마다 거울을 앉힌 뒤 원본 칸 좌표로 배치를
    /// 되살리고, 탭은 그 자리의 탭으로 연다. 거울 하나가 지금 방에 끼어들던 예전 동작과
    /// 다르다(2026-09-16 지시 「거기 방처럼 보이게」).
    pub(crate) fn open_remote_room(
        &mut self,
        label: &str,
        window: Option<u64>,
        room_label: &str,
        focus: Option<&str>,
    ) -> Result<()> {
        if self.tmux.is_some() {
            anyhow::bail!("tmux 백엔드에선 원격 pane 을 쓰지 않는다");
        }
        let m = kasa_mcp::machines::find(label)
            .ok_or_else(|| anyhow::anyhow!("기계 {label} 가 명부(machines.json)에 없다"))?;
        let rows: Vec<crate::state::MachinesColRow> = self
            .info
            .machines_col
            .machines
            .iter()
            .find(|x| x.label == label)
            .map(|x| {
                x.remote
                    .iter()
                    .chain(x.mirrored.iter())
                    .filter(|r| !r.closed && !r.remote_id.is_empty())
                    .filter(|r| if window.is_some() { r.window == window } else { r.room == room_label })
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        if rows.is_empty() {
            anyhow::bail!("{label} 의 그 방에 열 pane 이 없다");
        }
        // 이미 있는 거울은 **그 기기의 보기 창**에 앉은 것만 친다. `to` 로 보낸 학생의
        // 거울은 여기 방(원래 방)에 앉아 있어서, 그걸 찾아 가면 기기 방을 눌렀는데 이쪽
        // 방이 열렸다(2026-09-17 지적). 그 방은 따로 보기 창으로 연다.
        let view_mirrors: Vec<(String, String)> = kasa_pty::live_sessions().into_iter().filter_map(|id| {
            let info = kasa_mcp::remote::remote_info(&id)?;
            if !kasa_mcp::machines::same_machine_bases(&info.base, &m.base) { return None; }
            let in_view = self.window_of_pane(&id)
                .and_then(|w| self.remote_view_of_window(w))
                .is_some_and(|(view_label, _)| view_label == label);
            in_view.then(|| (id, info.remote_id))
        }).collect();
        let mirror_of = |rid: &str| view_mirrors.iter().find(|(_, r)| r == rid).map(|(id, _)| id.clone());
        let existing = focus
            .and_then(mirror_of)
            .or_else(|| rows.iter().find_map(|r| mirror_of(&r.remote_id)));
        if let Some(existing) = existing {
            self.focus_surface(&existing);
            return Ok(());
        }
        self.new_window();
        let owner = self.active_window;
        let Some(host) = self.ws.lock().unwrap().active_pane.clone() else {
            anyhow::bail!("새 창의 기본 pane 을 못 얻었다");
        };
        let (win_cols, win_rows) = self.window_cells();
        let outers: Vec<&crate::state::MachinesColRow> = rows.iter().filter(|r| r.tab_of.is_none()).collect();
        let mut seated: Vec<(String, String)> = Vec::new();
        let mut fail: Vec<String> = Vec::new();
        for row in &outers {
            let local_id = if seated.is_empty() { host.clone() } else { self.alloc_pane_id() };
            let spec = kasa_mcp::remote::RemoteSpec {
                base: m.base.clone(),
                pane: Some(row.remote_id.clone()),
                cwd: None,
                token: None,
                identity: kasa_mcp::remote::RemoteIdentity {
                    label: m.label.clone(),
                    remote_cwd: (!row.remote_cwd.is_empty()).then(|| row.remote_cwd.clone()),
                    origin_cwd: None,
                    owned: false,
                },
            };
            match kasa_mcp::remote::connect_view(spec, &local_id) {
                Ok(remote) => {
                    if seated.is_empty() {
                        if let Some(old) = self.pty.get(&host).cloned() {
                            old.stop_reader();
                        }
                    }
                    self.insert_pty(local_id.clone(), remote.session.clone());
                    self.pump_pty_screens(
                        remote.session.screens.clone(),
                        local_id.clone(),
                        std::sync::Arc::downgrade(&remote.session),
                    );
                    self.dead_panes.lock().unwrap().retain(|x| x != &local_id);
                    self.ws.lock().unwrap().panes.entry(local_id.clone()).or_default();
                    seated.push((local_id, row.remote_id.clone()));
                }
                Err(e) => fail.push(format!("{}: {e:#}", row.remote_id)),
            }
        }
        if seated.is_empty() {
            anyhow::bail!("거울을 하나도 못 열었다 — {}", fail.join(" · "));
        }
        if seated.len() > 1 {
            // 원본 칸 좌표가 다 있으면 그 배치 그대로, 아니면 고르게 나눈다.
            let cells: Vec<(String, [f32; 4])> = seated
                .iter()
                .filter_map(|(local, rid)| {
                    let rect = outers.iter().find(|r| &r.remote_id == rid)?.rect?;
                    Some((local.clone(), rect))
                })
                .collect();
            let tree = (cells.len() == seated.len())
                .then(|| crate::layout::layout_from_rects(&cells))
                .flatten()
                .unwrap_or_else(|| {
                    let others: Vec<String> = seated.iter().skip(1).map(|(id, _)| id.clone()).collect();
                    let dir = crate::layout::pick_split_axis(
                        win_cols as f32 * self.cell.w.max(1.0),
                        win_rows as f32 * self.cell.h.max(1.0),
                        win_cols,
                        win_rows,
                    );
                    kasa_pty::fleet(&seated[0].0, &others, dir, 1.0 / seated.len() as f32)
                });
            if !self.pty_layout.as_mut().is_some_and(|l| l.replace_leaf(&seated[0].0, tree)) {
                fail.push("배치 트리 심기 실패".into());
            }
        }
        for (local, rid) in &seated {
            let name = rows
                .iter()
                .find(|r| &r.remote_id == rid)
                .map(|r| r.name.clone())
                .filter(|n| !n.is_empty())
                .or_else(|| kasa_mcp::remote::remote_pane_character(&m.base, rid, None))
                .unwrap_or_default();
            if !name.is_empty() {
                self.relabel_pane(local, &name);
            }
        }
        self.ws.lock().unwrap().active_pane = Some(seated[0].0.clone());
        self.resize_backend(win_cols, win_rows);
        self.publish_pty_layout();
        // 탭 — 바깥 pane 의 거울을 활성으로 두고 그 탭으로 연다(`mirror_remote_pane` 이
        // 활성 pane 의 탭으로 여는 자리다).
        for row in rows.iter().filter(|r| r.tab_of.is_some()) {
            let Some((local, _)) = seated.iter().find(|(_, rid)| Some(rid) == row.tab_of.as_ref()) else {
                continue;
            };
            self.ws.lock().unwrap().active_pane = Some(local.clone());
            if let Err(e) = self.mirror_remote_pane(label, &row.remote_id, &row.name, &row.remote_cwd) {
                fail.push(format!("{} 탭: {e:#}", row.remote_id));
            }
        }
        if !room_label.is_empty() {
            self.window_name_override.insert(owner, room_label.to_string());
        }
        match focus.and_then(mirror_of) {
            Some(target) => {
                self.focus_surface(&target);
            }
            None => {
                self.ws.lock().unwrap().active_pane = Some(seated[0].0.clone());
            }
        }
        self.session_touched = true;
        self.chrome_dirty = true;
        if let Some(w) = &self.window {
            w.request_redraw();
        }
        if !fail.is_empty() {
            self.set_toast(format!("일부는 못 열었어요 — {}", fail.join(" · ")));
        }
        Ok(())
    }

    /// 다른 기기에 **새 방**을 만들고 그 첫 pane 을 여기 보기 창으로 연다 — 기기 절 머리의
    /// 「+」(2026-09-17 지시). 저쪽엔 그쪽 「+」 를 누른 것과 같은 방이 생기고, 이쪽 창은
    /// 그 방의 보기 창이라 로컬 방 목록엔 안 선다. 옛 판 기기는 활성 방에 pane 만 세운다.
    /// 새 창을 열고 그 기기의 pane 하나를 보기 창으로 앉힌다 — 「+ 새 방」과 `to` 뒤처리가 같이
    /// 쓴다. `owned` 는 이 창을 닫을 때 저쪽 pane 도 끝낼지(내가 만든 방이면 참).
    pub(crate) fn seat_remote_view_window(
        &mut self,
        label: &str,
        base: &str,
        remote_id: &str,
        owned: bool,
        name: Option<String>,
    ) -> Result<()> {
        self.new_window();
        let owner = self.active_window;
        let Some(host) = self.ws.lock().unwrap().active_pane.clone() else {
            anyhow::bail!("새 창의 기본 pane 을 못 얻었다");
        };
        let remote = kasa_mcp::remote::connect_view(
            kasa_mcp::remote::RemoteSpec {
                base: base.to_string(),
                pane: Some(remote_id.to_string()),
                cwd: None,
                token: None,
                identity: kasa_mcp::remote::RemoteIdentity {
                    label: label.to_string(),
                    remote_cwd: None,
                    origin_cwd: None,
                    owned,
                },
            },
            &host,
        )?;
        if let Some(old) = self.pty.get(&host).cloned() {
            old.stop_reader();
        }
        self.insert_pty(host.clone(), remote.session.clone());
        self.pump_pty_screens(
            remote.session.screens.clone(),
            host.clone(),
            std::sync::Arc::downgrade(&remote.session),
        );
        self.dead_panes.lock().unwrap().retain(|x| x != &host);
        self.ws.lock().unwrap().panes.entry(host.clone()).or_default();
        if let Some(name) = name {
            self.window_name_override.insert(owner, name);
        }
        let (cols, rows) = self.window_cells();
        self.resize_backend(cols, rows);
        self.publish_pty_layout();
        self.session_touched = true;
        self.chrome_dirty = true;
        if let Some(w) = &self.window {
            w.request_redraw();
        }
        Ok(())
    }

    pub(crate) fn new_remote_room(&mut self, label: &str) -> Result<(String, kasa_socket::backend::SeatRoom)> {
        if self.tmux.is_some() {
            anyhow::bail!("tmux 백엔드에선 원격 pane 을 쓰지 않는다");
        }
        let m = kasa_mcp::machines::find(label)
            .ok_or_else(|| anyhow::anyhow!("기계 {label} 가 명부(machines.json)에 없다"))?;
        self.set_toast(format!("{label} 에 새 방 여는 중…"));
        self.render_frame();
        let at = kasa_socket::backend::SpawnShellAt {
            window: Some(kasa_socket::backend::SpawnWindow::New),
            ..Default::default()
        };
        let (remote_id, room) = kasa_mcp::remote::spawn_shell_pane_at(&m.base, &at, None)?;
        self.seat_remote_view_window(&m.label, &m.base, &remote_id, true, room.shown().map(|n| format!("방 {n}")))?;
        self.set_toast(format!("{label} 에 새 방 — {remote_id}"));
        Ok((remote_id, room))
    }

    /// `machines connect <기기>`(`--here` 없이)의 새 셸. 부른 칸 옆에 연결 칸을 붙이면 다른 기기
    /// 셸이 이 기기 방에 섞였다 — 그 기기에 새 방을 세워 기기 방으로 둔다(2026-10-07 지적 「그
    /// 기기방에서 하게 돼있지 않나」). 보기 창은 뒤에 앉히고 보던 방으로 돌아온다 — 학생이 부르는
    /// 일이라 사람이 치던 글자가 저쪽 셸로 새면 안 된다. 저쪽이 칸을 못 세우면 `None`(옛 길로).
    pub(crate) fn connect_remote_room(&mut self, base: &str, cwd: Option<&str>) -> Result<Option<String>> {
        if self.tmux.is_some() {
            anyhow::bail!("tmux 백엔드에선 원격 pane 을 쓰지 않는다");
        }
        let at = kasa_socket::backend::SpawnShellAt {
            cwd: cwd.map(str::to_string),
            window: Some(kasa_socket::backend::SpawnWindow::New),
            ..Default::default()
        };
        let (remote_id, room) = match kasa_mcp::remote::spawn_shell_pane_at(base, &at, None) {
            Ok(seat) => seat,
            Err(e) => {
                eprintln!("[connect] {base} 에 방을 못 세워 옆 연결 칸으로 물러섬: {e:#}");
                return Ok(None);
            }
        };
        let label = kasa_mcp::machines::label_for_base(base).unwrap_or_default();
        let back = self.active_window;
        let seated = self.seat_remote_view_window(&label, base, &remote_id, true, room.shown().map(|n| format!("방 {n}")));
        let host = self.ws.lock().unwrap().active_pane.clone();
        self.switch_window(back);
        if let Err(e) = seated {
            let _ = kasa_mcp::remote::close_remote_pane(base, &remote_id, None, true);
            return Err(e);
        }
        self.set_toast(format!("{label} 에 새 방 — 사이드바 {label} 에서 보세요"));
        host.map(Some).ok_or_else(|| anyhow::anyhow!("보기 창의 칸을 못 얻었다"))
    }

    /// 보기 창의 배치를 **원본 방의 배치**에 맞춘다. 거울은 순서대로 쪼개져 앉거나(옛 unfold·
    /// 자동 동기) 저장본대로 되살아나서, 미니맵은 원본 좌표로 같아 보여도 안의 배치는 달랐다
    /// (2026-09-17 지적 「미니맵은 똑같은데 안에 배치가 달라」). 원본이 정본이다 — 2초마다
    /// 원본 칸 좌표(명부 캐시 `/term/panes` 의 `rect`)로 BSP 를 되살려 다르면 갈아 끼운다.
    /// 좌표를 모르는 옛 판 기기·풍차 배치는 그대로 둔다.
    /// 거울 창에서 바꾼 배치를 **원본에** 보낸다. 안 보내면 2초 뒤 당겨오기가 되돌려
    /// 놓아, 크기를 조절해도 제자리로 튀어 오른다(2026-09-17 지시).
    pub(crate) fn push_remote_view_divider(&mut self, window: usize, path: &[u8]) {
        let Some((label, _)) = self.remote_view_of_window(window) else { return };
        let tree = if window == self.active_window { self.pty_layout.as_ref() }
            else { self.windows.get(window).and_then(Option::as_ref) };
        let Some(ratio) = tree.and_then(|t| t.ratio_at(path)) else { return };
        let Some(dir) = tree.and_then(|t| t.dir_at(path)) else { return };
        // 트리 경로는 기기마다 달라 못 쓴다 — 분할선 양쪽의 pane 으로 짚는다. 한 쌍만 보내면
        // 정렬된 2×2 격자(거울은 늘 세로선부터 자르고 원본은 가로선부터일 수 있다)에서 원본
        // 분할선 하나만 움직여 나머지 줄이 당겨오기에 되돌아 보였다(2026-09-18). 양쪽 모든
        // 쌍을 축과 함께 보내고 원본이 같은 축의 분할선만 고른다.
        let Some((left, right)) = tree.and_then(|t| t.split_leaves_at(path)) else { return };
        let remote = |local: &str| kasa_mcp::remote::remote_info(&self.leaf_pty_id(local)).map(|i| i.remote_id);
        let left: Vec<String> = left.iter().filter_map(|l| remote(l)).collect();
        let right: Vec<String> = right.iter().filter_map(|l| remote(l)).collect();
        if left.is_empty() || right.is_empty() { return }
        let pairs: Vec<serde_json::Value> = left.iter()
            .flat_map(|a| right.iter().map(move |b| serde_json::json!([a, b])))
            .collect();
        let Some(m) = kasa_mcp::machines::find(&label) else { return };
        // `a`/`b` 는 `pairs` 를 모르는 옛 판(2026-09-17) 원본용.
        let params = serde_json::json!({
            "pairs": pairs, "ratio": ratio, "dir": crate::layout::seam_axis(dir).as_str(),
            "a": left[0], "b": right[0],
        });
        // 배치 채널이 있으면 그 소켓으로 — 순서가 지켜지고 원본이 적용을 확인해 준다.
        if kasa_mcp::layout_watch::send_ratio(&m.base, &params) {
            return;
        }
        self.remote_view_push_at = Some(Instant::now());
        queue_remote_divider(m.base.clone(), params);
    }

    pub(crate) fn sync_remote_view_layouts(&mut self) {
        use std::sync::{Mutex, OnceLock};
        static LAST: OnceLock<Mutex<Option<(Instant, u64)>>> = OnceLock::new();
        self.watch_view_machines();
        {
            // 기계 캐시가 새로 채워졌으면 2초를 기다리지 않는다.
            let generation = kasa_mcp::machines::generation();
            // 방금 저쪽에 보낸 직후면 당겨오지 않는다 — 도착 전에 옛 배치로 되돌리면
            // 손으로 맞춘 크기가 튀어 오른다. 3초가 지나도 **보낸 뒤에 읽은 명부가 아직
            // 없으면**(느린 기기) 15초까지 더 기다린다 — 옛 명부로 되돌리는 게 튐의 원인이다.
            if let Some(at) = self.remote_view_push_at {
                let since = at.elapsed();
                if since < std::time::Duration::from_secs(3) {
                    return;
                }
                if since < std::time::Duration::from_secs(15) {
                    let stale = kasa_mcp::machines::snapshot().iter().any(|m| {
                        m.get("ago_secs").and_then(|v| v.as_u64())
                            .is_some_and(|ago| ago as f32 > since.as_secs_f32())
                    });
                    if stale {
                        return;
                    }
                }
            }
            let mut last = LAST.get_or_init(|| Mutex::new(None)).lock().unwrap();
            if last.is_some_and(|(t, g)| g == generation && t.elapsed() < std::time::Duration::from_secs(2)) {
                return;
            }
            *last = Some((Instant::now(), generation));
        }
        // 구성원부터 — 새로 앉힌 leaf 의 칸은 바로 아래에서 원본대로 잡는다.
        self.sync_remote_view_members();
        // 배치 채널이 산 기계는 원본 트리 그대로. 아래 좌표 되짓기는 채널 없는 옛 판 원본용.
        let live = self.apply_remote_view_trees();
        let mut changed_active = false;
        let mut changed_any = false;
        for i in 0..self.windows.len() {
            if live.contains(&i) {
                continue;
            }
            let Some((label, _)) = self.remote_view_of_window(i) else { continue };
            let tree = if i == self.active_window { self.pty_layout.as_ref() } else { self.windows[i].as_ref() };
            let Some(tree) = tree else { continue };
            let leaves: Vec<String> = tree.leaves().iter().map(|s| s.to_string()).collect();
            if leaves.len() < 2 {
                continue;
            }
            // 원본 칸 — leaf 마다 그 거울의 원격 pane 을 명부 캐시에서 찾는다. 하나라도 모르거나
            // 원본 방이 갈리면 손대지 않는다.
            let mut cells: Vec<(String, [f32; 4])> = Vec::with_capacity(leaves.len());
            let mut source_window: Option<u64> = None;
            let mut complete = true;
            for leaf in &leaves {
                let Some(info) = kasa_mcp::remote::remote_info(&self.leaf_pty_id(leaf)) else { complete = false; break };
                let Some(row) = kasa_mcp::machines::cached_pane(&label, &info.remote_id) else { complete = false; break };
                let (Some(rect), Some(win)) = (crate::machinescol::row_rect(&row), row.get("window").and_then(|v| v.as_u64())) else { complete = false; break };
                if source_window.is_some_and(|w| w != win) { complete = false; break }
                source_window = Some(win);
                cells.push((leaf.clone(), rect));
            }
            if !complete {
                continue;
            }
            // 지금 배치와 같으면 그대로 — 매번 갈아 끼우면 사람이 끄는 분할선이 튄다.
            let current: std::collections::HashMap<String, [f32; 4]> = tree
                .leaf_rects(1000, 1000)
                .into_iter()
                .map(|(id, x, y, w, h)| (id, [x as f32 / 1000.0, y as f32 / 1000.0, w as f32 / 1000.0, h as f32 / 1000.0]))
                .collect();
            let same = cells.iter().all(|(id, want)| {
                current.get(id).is_some_and(|have| have.iter().zip(want).all(|(a, b)| (a - b).abs() < 0.03))
            });
            if same {
                continue;
            }
            let Some(fresh) = crate::layout::layout_from_rects(&cells) else { continue };
            changed_any = true;
            if i == self.active_window {
                self.pty_layout = Some(fresh);
                self.zoomed_pane = None;
                changed_active = true;
            } else {
                self.windows[i] = Some(fresh);
            }
        }
        if !changed_any {
            return;
        }
        if changed_active {
            let (cols, rows) = self.window_cells();
            self.resize_backend(cols, rows);
        }
        self.publish_pty_layout();
        self.session_touched = true;
        if changed_active {
            self.chrome_dirty = true;
            if let Some(w) = &self.window {
                w.request_redraw();
            }
        }
    }

    /// `i` 번 창이 다른 기기 방의 **보기 창**인가 — 모든 leaf 가 한 기계의 거울(view)이면
    /// `(기계 라벨, 원격 pane id 들)`. 이런 창은 이 기기 방 목록에 안 서고, 그 기계 절의
    /// 방 카드가 탭 노릇을 한다(2026-09-16 지시 「새로 여는 게 아니라 눌러서 보이게」).
    /// 셸을 하나라도 끼워 넣으면 그 순간 보통 방이 된다 — 표식이 아니라 내용으로 가른다.
    pub(crate) fn remote_view_of_window(&self, i: usize) -> Option<(String, Vec<String>)> {
        let leaves = self.window_leaves(i);
        if leaves.is_empty() {
            return None;
        }
        let mut base: Option<String> = None;
        let mut ids = Vec::with_capacity(leaves.len());
        for leaf in &leaves {
            let pid = self.leaf_pty_id(leaf);
            // PTY 도 링크도 없는 빈 자리(복원이 못 채운 leaf)는 방의 성격을 못 가른다 —
            // 그 하나 때문에 보기 창이 보통 방으로 둔갑하지 않게 건너뛴다(2026-09-18).
            if !self.pty.contains_key(&pid) && kasa_mcp::remote::remote_info(&pid).is_none() {
                continue;
            }
            let info = kasa_mcp::remote::remote_info(&pid).filter(|r| r.view)?;
            match &base {
                Some(b) if !kasa_mcp::machines::same_machine_bases(b, &info.base) => return None,
                None => base = Some(info.base.clone()),
                _ => {}
            }
            ids.push(info.remote_id);
        }
        let base = base?;
        let label = kasa_mcp::machines::label_for_base(&base).unwrap_or(base);
        Some((label, ids))
    }

    pub(crate) fn reveal_pane_tab(&mut self, pid: &str) -> bool {
        self.focus_surface(pid)
    }

    /// 방 펼치기 — 명부 기계 하나의 학생 pane 전부를 이 창에 거울로 앉힌다.
    ///
    /// 「완전동기화되는 세션」의 보기 쪽 절반(2026-09-02 지시 「일단 만들어봐」):
    /// 학생 본체는 그 기계에 그대로 살고, 여기는 원격 방(window)마다 새 창을 하나씩
    /// 만들어 거울을 균등 배치한다. 목록은 machines 폴링 캐시(`/term/panes`)를 그대로
    /// 쓰므로 원격에 새 창구가 필요 없고, 이미 거울이 있는 학생은 건너뛴다 — 두 번
    /// 눌러도 같은 학생이 둘 뜨지 않는다. 배치 비율까지 원본 그대로 재현하는 것은
    /// 원격 배치 창구가 생긴 뒤의 일이고, 지금은 방 단위 균등 배치가 정본이다.
    pub(crate) fn unfold_machine(&mut self, label: &str) -> Result<String> {
        if self.tmux.is_some() {
            anyhow::bail!("tmux 백엔드에선 원격 pane 을 쓰지 않는다");
        }
        let m = kasa_mcp::machines::find(label)
            .ok_or_else(|| anyhow::anyhow!("기계 {label} 가 명부(machines.json)에 없다"))?;
        let snap = kasa_mcp::machines::snapshot();
        let entry = snap
            .iter()
            .find(|v| v.get("label").and_then(|l| l.as_str()) == Some(label))
            .ok_or_else(|| anyhow::anyhow!("기계 {label} 상태를 아직 모른다 — 잠시 뒤 다시"))?;
        if entry.get("online").and_then(|v| v.as_bool()) != Some(true) {
            anyhow::bail!("{label} 이 지금 안 닿는다 — 그 기계의 kasaterm 이 떠 있는지 확인");
        }
        let panes = entry
            .get("panes")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        // 이 기계로 이미 난 거울들 — remote_id 는 원격 GUI pane id 와 같은 값이라
        // (이사 탭의 미러 대표 규칙과 동일) 그대로 집합 키가 된다.
        let mirrored: std::collections::HashSet<String> = kasa_pty::live_sessions()
            .into_iter()
            .filter_map(|id| kasa_mcp::remote::remote_info(&id))
            .filter(|i| i.base == m.base)
            .map(|i| i.remote_id)
            .collect();
        // (원격 방 번호, 원격 pane id, 학생 이름, 원격 cwd). GUI pane(`%…`)만 —
        // 같은 목록에 헤드리스 세션(`web-…`)도 실려 오는데 그건 화면의 방이 아니다.
        // cwd 를 링크 정체에 실어야 재접속 자동 따라잡기가 이 거울의 레포를 안다.
        let mut targets: Vec<(u64, String, String, String)> = panes
            .iter()
            .filter_map(|p| {
                let rid = p.get("id")?.as_str()?.to_string();
                if !rid.starts_with('%') || mirrored.contains(&rid) {
                    return None;
                }
                // 그 기계에서 닫힌 pane(되살리기 대열)은 화면에 없는 학생이다 — 거울로
                // 세우면 저쪽엔 없는 방이 이쪽에 생긴다(2026-09-07 지적).
                if p.get("closed").and_then(|v| v.as_bool()).unwrap_or(false) {
                    return None;
                }
                // 탭은 leaf 로 앉히지 않는다 — 바깥과 칸이 같아 좌표 동기가 포기하고, 원본에선
                // 탭이던 것이 여기선 옆 칸이 됐다. 바깥 거울이 서면 `sync_remote_view_members`
                // 가 그 탭으로 붙인다(2026-09-18). 남의 거울(`mirror_of`)도 되비추지 않는다.
                if p.get("tab_of").and_then(|v| v.as_str()).is_some_and(|s| !s.is_empty())
                    || p.get("mirror_of").and_then(|v| v.as_str()).is_some()
                {
                    return None;
                }
                let name = p
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let cwd = p
                    .get("cwd")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let win = p.get("window").and_then(|v| v.as_u64()).unwrap_or(u64::MAX);
                Some((win, rid, name, cwd))
            })
            .collect();
        if targets.is_empty() {
            return Ok(format!(
                "{label}: 펼칠 캐릭터가 없다 — 전부 이미 거울로 있거나 빈 기계다"
            ));
        }
        targets.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
        let mut rooms: Vec<Vec<(String, String, String)>> = Vec::new();
        let mut cur_win: Option<u64> = None;
        for (w, rid, name, cwd) in targets {
            if cur_win != Some(w) {
                cur_win = Some(w);
                rooms.push(Vec::new());
            }
            rooms.last_mut().unwrap().push((rid, name, cwd));
        }
        let room_n = rooms.len();
        let total: usize = rooms.iter().map(Vec::len).sum();
        let mut ok_n = 0usize;
        let mut fail: Vec<String> = Vec::new();
        for room in rooms {
            // 방 하나 = 새 로컬 창. 첫 학생은 새 창의 기본 셸 자리를 스왑으로
            // 차지하고(migrate 와 같은 insert_pty 교체 — 옛 reader 를 먼저 세워
            // EOF 죽음표시 경주를 닫는다), 나머지는 균등 트리로 앉는다.
            self.new_window();
            let Some(host) = self.ws.lock().unwrap().active_pane.clone() else {
                fail.push("새 창의 기본 pane 을 못 얻었다".into());
                continue;
            };
            let (win_cols, win_rows) = self.window_cells();
            let mut seated: Vec<(String, String, String)> = Vec::new();
            for (rid, name, rcwd) in &room {
                self.set_toast(format!(
                    "{label} 펼치는 중 — {}/{total}",
                    ok_n + fail.len() + 1
                ));
                self.render_frame();
                let local_id = if seated.is_empty() {
                    host.clone()
                } else {
                    self.alloc_pane_id()
                };
                // connect 가 아니라 connect_view — 거울은 원본 세션 크기를 절대
                // 바꾸지 않는다(사용자: 「미러링할때 크기 줄이면 미러링되는곳도
                // 줄어들어」). 격자가 pane 보다 크면 렌더가 그 pane 만 배율을 줄인다.
                match kasa_mcp::remote::connect_view(
                    kasa_mcp::remote::RemoteSpec {
                        base: m.base.clone(),
                        pane: Some(rid.clone()),
                        cwd: None,
                        token: None,
                        identity: kasa_mcp::remote::RemoteIdentity {
                            label: m.label.clone(),
                            // 원격 pane 의 작업 폴더 — 재접속 자동 따라잡기가
                            // 이 거울의 레포를 아는 유일한 길이다.
                            remote_cwd: (!rcwd.is_empty()).then(|| rcwd.clone()),
                            origin_cwd: None,
                            owned: false,
                        },
                    },
                    &local_id,
                ) {
                    Ok(remote) => {
                        if seated.is_empty() {
                            if let Some(old) = self.pty.get(&host).cloned() {
                                old.stop_reader();
                            }
                        }
                        self.insert_pty(local_id.clone(), remote.session.clone());
                        self.pump_pty_screens(
                            remote.session.screens.clone(),
                            local_id.clone(),
                            std::sync::Arc::downgrade(&remote.session),
                        );
                        self.dead_panes.lock().unwrap().retain(|x| x != &local_id);
                        // 첫 자리 말고는 아직 화면 상태(PaneState)가 없다 — 첫 화면이
                        // 와야 생기는데(apply_screen_update 의 or_insert) 그건 이 함수가
                        // 끝난 뒤다. 아래 relabel_pane 은 실존 pane 만 손대므로 미리
                        // 자리를 안 만들면 둘째 학생부터 이름표가 안 붙어 「셸」로
                        // 뜨고 테마도 없었다(2026-09-07 지적).
                        self.ws
                            .lock()
                            .unwrap()
                            .panes
                            .entry(local_id.clone())
                            .or_default();
                        seated.push((local_id, name.clone(), rid.clone()));
                        ok_n += 1;
                    }
                    Err(e) => fail.push(format!("{rid}({name}): {e:#}")),
                }
            }
            if seated.is_empty() {
                continue; // 새 창은 빈 셸로 남는다 — 실패 사유가 아래 보고에 찍힌다.
            }
            if seated.len() > 1 {
                let others: Vec<String> =
                    seated.iter().skip(1).map(|(id, _, _)| id.clone()).collect();
                let dir = crate::layout::pick_split_axis(
                    win_cols as f32 * self.cell.w.max(1.0),
                    win_rows as f32 * self.cell.h.max(1.0),
                    win_cols,
                    win_rows,
                );
                let tree = kasa_pty::fleet(&seated[0].0, &others, dir, 1.0 / seated.len() as f32);
                if !self
                    .pty_layout
                    .as_mut()
                    .is_some_and(|l| l.replace_leaf(&seated[0].0, tree))
                {
                    fail.push("배치 트리 심기 실패".into());
                }
            }
            // 몸통이 남의 기계라도 이 창의 학생은 같은 사람이어야 한다 —
            // 이름·색·얼굴이 없는 무명 pane 방지(spawn_remote_pane 과 같은 규칙).
            for (id, name, rid) in &seated {
                // 스냅샷에 이름이 없으면(막 뜬 학생·옛 원격) 그 기계에 직접 묻는다 —
                // 거울(mirror_remote_pane)과 같은 규칙.
                let name = if name.is_empty() {
                    kasa_mcp::remote::remote_pane_character(&m.base, rid, None).unwrap_or_default()
                } else {
                    name.clone()
                };
                if !name.is_empty() {
                    self.relabel_pane(id, &name);
                }
            }
            self.ws.lock().unwrap().active_pane = Some(seated[0].0.clone());
            self.resize_backend(win_cols, win_rows);
            self.publish_pty_layout();
        }
        if let Some(w) = &self.window {
            w.request_redraw();
        }
        let mut msg = format!("{label} 펼침 — 캐릭터 {ok_n}명 · 방(창) {room_n}개");
        if !fail.is_empty() {
            msg.push_str(&format!(" · 실패 {}건: {}", fail.len(), fail.join(" / ")));
        }
        self.set_toast(msg.clone());
        Ok(msg)
    }
}

/// 거울의 분할선 밀어내기는 한 줄로 세워 보낸다. 드래그 중 칸 경계마다 스레드를 띄우면 도착
/// 순서가 뒤바뀌어 원본이 마지막이 아닌 비율에 멈추고, 3초 뒤 당겨오기가 거울을 그 자리로
/// 되돌린다(2026-09-18). 같은 분할선의 것이 쌓였으면 마지막 것만 보낸다 — 다른 분할선의
/// 마지막은 지우지 않는다(끝내고 바로 다른 선을 잡았을 때 앞 선의 최종 위치가 사라진다).
pub(super) fn queue_remote_divider(base: String, params: serde_json::Value) {
    use std::sync::mpsc::{channel, Sender};
    use std::sync::{Mutex, OnceLock};
    type Push = (String, serde_json::Value);
    static TX: OnceLock<Mutex<Sender<Push>>> = OnceLock::new();
    let tx = TX.get_or_init(|| {
        let (tx, rx) = channel::<Push>();
        std::thread::spawn(move || {
            while let Ok(first) = rx.recv() {
                let mut batch = vec![first];
                while let Ok(next) = rx.try_recv() {
                    batch.push(next);
                }
                let key = |(base, p): &Push| (base.clone(), p.get("pairs").cloned(), p.get("dir").cloned());
                let mut i = 0;
                while i < batch.len() {
                    let k = key(&batch[i]);
                    if batch[i + 1..].iter().any(|later| key(later) == k) { batch.remove(i); } else { i += 1; }
                }
                for (base, params) in batch {
                    if let Err(e) = kasa_mcp::remote::remote_cmd(&base, "surface.set_ratio_between", params) {
                        eprintln!("[remote] divider push failed: {e:#}");
                    }
                }
                // 원본은 명령을 GUI 스레드에 넘기고 바로 답한다 — 그 직후 명부를 당기면 옛
                // 칸이 캐시에 앉는다. 한 프레임쯤 기다린 뒤 당긴다(배치 발행이 롱폴도 깨운다).
                std::thread::sleep(std::time::Duration::from_millis(200));
                kasa_mcp::machines::poke();
            }
        });
        Mutex::new(tx)
    });
    if let Ok(tx) = tx.lock() {
        let _ = tx.send((base, params));
    }
}
