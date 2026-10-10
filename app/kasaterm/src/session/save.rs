//! 세션 저장 — 방·칸 배치와 칸마다의 복원 기록을 session.json 에 쓴다.
use super::*;

pub(super) fn append_surface_record_metadata(obj: &mut serde_json::Map<String, serde_json::Value>, surface: &str) {
    obj.insert("surface_key".into(), serde_json::json!(kasa_mcp::surface_keys::ensure(surface)));
    if let Some(key) = kasa_mcp::remote::remote_surface_key(surface) {
        obj.insert("remote_surface_key".into(), serde_json::json!(key));
    }
}

impl App {
    /// Serialize every session (active + stashed) as a layout tree so the next
    /// launch can restore the full multi-pane, multi-session workspace.
    ///
    /// Split out from `save_session_state` so the autosave path can hash the
    /// result and skip an identical write — the two must produce byte-identical
    /// state or a Cmd+Q would look like a change and rewrite for nothing.
    /// 저장이 leaf 에 실을 surface_id → (model, effort). 소켓 백엔드가 없으면 빈 맵.
    ///
    /// 값을 모으는 쪽은 백엔드다(claude 는 statusline 보고, codex 는 transcript 머리).
    /// App 에 같은 맵을 하나 더 두지 않고 그때그때 떠 오는 이유는, App struct 의 필드
    /// 정의가 워커 여럿이 동시에 못 만지는 병목이기 때문이다(CLAUDE.md).
    pub(crate) fn agent_cfg_snapshot(&self) -> HashMap<String, (String, String)> {
        let mut cfg = self
            .socket_backend
            .as_ref()
            .map(|b| b.agent_cfg_snapshot())
            .unwrap_or_default();
        // ultracode 는 statusline 이 xhigh 로 보고한다(claude 가 effort 페이로드에 안
        // 실음) — 그대로 저장하면 재시작 복원이 xhigh 로 잇는다(2026-08-15 신고
        // 「울트라코드로 이어가는거 왜안돼」). 마커 판정(pane_ultracode — 입력박스
        // 보라 글로우와 같은 근거)이 참인 pane 은 여기서 덮는다. `--effort ultracode`
        // 는 CLI 가 실제로 받아 켠다(print 자기보고 실측: ultracode→ON, xhigh→OFF).
        // 보고가 아직 없는 pane 은 **복원이 띄울 때 쓴 값**으로 메운다. 안 메우면
        // 그 창은 다음 복원에서 기본 모델로 뜨고, 사용자가 고른 값이 재시작 한 번에
        // 조용히 풀린다. 보고가 있으면 그쪽이 정본이다 — 그 사이 `/model` 로 바꿨을
        // 수 있고, 그건 이 기준선이 모르는 사실이다.
        for (pane, (model, effort)) in restored_agent_cfg().lock().unwrap().iter() {
            if !self.pty.contains_key(pane) {
                continue;
            }
            let e = cfg.entry(pane.clone()).or_default();
            if e.0.is_empty() && !model.is_empty() {
                e.0 = model.clone();
            }
            if e.1.is_empty() && !effort.is_empty() {
                e.1 = effort.clone();
            }
        }
        for pane in &self.pane_ultracode {
            cfg.entry(pane.clone()).or_default().1 = "ultracode".to_string();
        }
        cfg
    }

    pub(crate) fn session_state_json(&self) -> Option<serde_json::Value> {
        let mut sessions_json = Vec::new();
        // 창별 워크스페이스 락을 잡기 전에 한 번만 뜬다(락 순서 얽힘 방지).
        let agent_cfg = self.agent_cfg_snapshot();
        for i in 0..self.sessions.len() {
            // Each session contributes all its windows. The active session's
            // live state is in self.{pty,pty_layout,windows,active_window};
            // stashed sessions carry the same fields on their Session.
            let (pty, active_layout, windows, active_window, ws_arc) = if i == self.active_session {
                (
                    &self.pty,
                    self.pty_layout.as_ref(),
                    &self.windows,
                    self.active_window,
                    &self.ws,
                )
            } else {
                match self.sessions[i].as_ref() {
                    Some(s) => (
                        &s.pty,
                        s.pty_layout.as_ref(),
                        &s.windows,
                        s.active_window,
                        &s.ws,
                    ),
                    None => continue,
                }
            };
            let persisted_active_window = if i == self.active_session {
                if let Some(kind) = self.internal_room_kind_at(active_window) {
                    self.internal_return_pane(kind)
                        .and_then(|pane| self.window_of_pane(pane))
                        .filter(|idx| *idx != active_window)
                    .or_else(|| {
                        (0..windows.len()).find(|idx| {
                            let layout = if *idx == active_window {
                                active_layout
                            } else {
                                windows.get(*idx).and_then(Option::as_ref)
                            };
                            layout.is_some_and(crate::internal_room::should_persist_layout)
                        })
                    })
                    .unwrap_or(0)
                } else {
                    active_window
                }
            } else {
                active_window
            };
            // Lock this session's workspace once so each leaf can read its
            // pane scrollback while serializing the window trees.
            let ws_guard = ws_arc.lock().unwrap();
            // Serialize every window. The active window's tree lives in
            // active_layout; the rest sit in `windows[j]` (active slot None).
            let mut windows_json = Vec::new();
            let mut new_active = 0usize;
            // 원래 방 인덱스 → 저장된 인덱스. 트리 없는 방·안쪽 방은 건너뛰어 번호가
            // 밀리므로, 별도창의 `home_window` 는 이 맵으로 환산해 싣는다.
            let mut persisted_idx: Vec<Option<usize>> = vec![None; windows.len()];
            for (j, slot) in windows.iter().enumerate() {
                let layout = if j == active_window {
                    active_layout
                } else {
                    slot.as_ref()
                };
                let Some(layout) = layout else { continue };
                if !crate::internal_room::should_persist_layout(layout) {
                    continue;
                }
                if j == persisted_active_window {
                    new_active = windows_json.len();
                }
                persisted_idx[j] = Some(windows_json.len());
                windows_json.push(Self::layout_to_json(
                    layout,
                    pty,
                    &ws_guard,
                    &self.pane_claude_sid,
                    &agent_cfg,
                ));
            }
            if windows_json.is_empty() {
                continue;
            }
            // 별도 OS 창으로 뗀 pane — 트리에 없어 위 루프가 못 싣는다. leaf 기록은
            // 같은 함수로 뽑고(스크롤백·cwd·세션 id·탭까지), 떠나온 방과 창 틀을
            // 곁들인다. 옛 별도창(09-03 이전)은 이걸 안 실어 재시작마다 사라졌다.
            let undocked_json: Vec<serde_json::Value> = if i == self.active_session {
                self.undocked_frames()
                    .into_iter()
                    .filter_map(|(pane, home, frame)| {
                        let node = Self::layout_to_json(
                            &kasa_pty::PtyLayout::single(pane.as_str()),
                            pty,
                            &ws_guard,
                            &self.pane_claude_sid,
                            &agent_cfg,
                        );
                        let rec = node.get("leaf").cloned().filter(|r| !r.is_null())?;
                        let home = persisted_idx
                            .get(home)
                            .copied()
                            .flatten()
                            .unwrap_or(new_active);
                        Some(serde_json::json!({
                            "leaf": rec,
                            "home_window": home,
                            "frame": frame,
                        }))
                    })
                    .collect()
            } else {
                Vec::new()
            };
            // 방마다 따로 고른 본문 보기(목록/배치도). 방 인덱스가 키인데 저장하며
            // 빈 방이 빠져 번호가 당겨지므로, `persisted_idx` 로 **저장본 번호**로
            // 옮겨 적는다 — 옛 번호 그대로 실으면 재시작에 엉뚱한 방이 목록이 된다.
            let room_body_json: serde_json::Map<String, serde_json::Value> =
                if i == self.active_session {
                    self.room_list_body
                        .iter()
                        .filter_map(|(old, list)| {
                            let ni = persisted_idx.get(*old).copied().flatten()?;
                            Some((
                                ni.to_string(),
                                serde_json::Value::String(
                                    if *list { "list" } else { "map" }.to_string(),
                                ),
                            ))
                        })
                        .collect()
                } else {
                    serde_json::Map::new()
                };
            sessions_json.push(serde_json::json!({
                "windows": windows_json,
                "active_window": new_active,
                "undocked": undocked_json,
                "room_body": room_body_json,
            }));
        }
        if sessions_json.is_empty() {
            return None;
        }
        // 숨겨 둔 pane 도 함께 싣는다. 여태 안 실려서, 화면에서 치워 둔 작업은
        // **재시작 한 번에 통째로 사라졌다**(2026-08-27 지적 「재시작하면 완전히
        // 없어지고」). 숨기기의 약속이 「돌아왔을 때 그 자리에 있다」인데 그 약속이
        // 앱 수명까지만이었던 셈이다.
        //
        // `alive` 는 싣지 않는다 — 재시작하면 PTY 는 어차피 다 죽으므로 되살리기는
        // 레코드로 새로 띄우는 쪽이고, claude 였던 pane 은 `rec` 에 세션 id 가 실려
        // 있어 `restore_leaf` 가 `--resume` 까지 붙여 준다. 살아 있다고 적어 두면
        // 되살리기가 「있지도 않은 PTY 에 재부착」을 시도한다.
        //
        // Both stashes and the bounded closed-pane history survive app restart.
        // Expiring execution must not also erase the user's recovery record.
        let internal_windows: Vec<usize> = (0..self.windows.len())
            .filter(|idx| self.internal_room_kind_at(*idx).is_some())
            .collect();
        let closed_json: Vec<serde_json::Value> = self
            .closed_panes
            .iter()
            .filter(|c| !c.rec.is_null())
            .map(|c| {
                let window = c.window.saturating_sub(
                    internal_windows.iter().filter(|idx| **idx < c.window).count(),
                );
                serde_json::json!({
                    "rec": c.rec,
                    "pane_id": c.pane_id,
                    "character": c.character,
                    "folder": c.folder,
                    "neighbor": c.neighbor,
                    "window": window,
                    "stashed": c.stashed,
                })
            })
            .collect();
        let mut state = serde_json::json!({
            "active_session": self.active_session,
            "sessions": sessions_json,
            "stashed_panes": closed_json,
            "last_used_unix": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs()),
            "pane_weather": self.weather_overrides_json(),
        });
        if let Some(progress) = &self.restore_progress { progress.preserve_snapshot(&mut state); }
        Some(state)
    }
    /// Write the restore snapshot on exit.
    ///
    /// 복원 창이 아직 떠 있으면 쓰지 않는다 — 그 화면은 사용자가 "복원"을 고르기
    /// 전의 빈 새 세션이라, 여기서 저장하면 **되살리려던 작업 공간을 그 빈 세션으로
    /// 덮어써** 영영 잃는다(복원할지 말지 못 정하고 그냥 껐을 때). autosave_session
    /// 과 같은 이유.
    pub(crate) fn save_session_state(&self) {
        if self.lite {
            return;
        }
        self.save_aux_windows_state();
        // Once the layout exists, keep new work while preserving the original
        // execution records of unfinished surfaces in session_state_json.
        if self.restore_prompt.is_some() || self.restoration_blocks_input() {
            return;
        }
        if let Some(state) = self.session_state_json() {
            socket::write_session_state(&state);
        }
    }
    /// 강제 종료 대비 자동 스냅샷. `exiting()` 만으로는 Cmd+Q(정중한 종료) 때만
    /// 저장돼, SIGKILL·크래시·정전이면 그 세션의 작업이 디스크에 아예 안 남고
    /// 복원 창은 **직전에 정상 종료했던 시점**의 낡은 상태를 띄운다.
    ///
    /// 실제로 바뀐 경우에만 쓴다. 이 앱은 마우스만 움직여도 깨어나므로 wake 를
    /// 곧 변경으로 보면 몇 초마다 같은 내용을 다시 쓰게 된다 — 직렬화는 하되
    /// 해시가 같으면 디스크는 건드리지 않는다.
    pub(crate) fn autosave_session(&mut self) {
        self.session_saved_at = std::time::Instant::now();
        self.session_touched = false;
        if self.lite {
            return;
        }
        self.save_aux_windows_state();
        // 복원 창이 떠 있는 동안은 절대 저장하지 않는다 — 사용자가 "복원"을 고르기
        // 전의 화면은 빈 새 세션이라, 자동 저장이 복원 대상 자체를 덮어써 버린다
        // (되돌릴 수 없는 자해). 선택이 끝나면 그 클릭이 다시 touched 를 세운다.
        if self.restore_prompt.is_some() || self.restoration_blocks_input() {
            return;
        }
        let Some(state) = self.session_state_json() else {
            return;
        };
        let body = state.to_string();
        let mut h = std::collections::hash_map::DefaultHasher::new();
        std::hash::Hash::hash(&body, &mut h);
        let sum = std::hash::Hasher::finish(&h);
        if self.session_saved_hash == Some(sum) {
            return;
        }
        self.session_saved_hash = Some(sum);
        socket::write_session_state(&state);
    }

    /// leaf 와 탭이 함께 쓰는 「이 surface 의 복원 재료」를 채운다.
    ///
    /// `surface` 는 leaf 면 바깥 pane id, 탭이면 그 탭의 pid 다 — 대화 번호·모델·
    /// 캐릭터가 전부 그 키로 잡힌다. `term` 은 그 화면(권한 모드 판정·스크롤백 폴백):
    /// `PaneState` 는 **활성 탭**으로 Deref 하므로 pane 을 그대로 읽으면 둘째 탭을
    /// 보던 중에 저장했을 때 남의 화면이 leaf 에 실린다.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn fill_surface_record(
        obj: &mut serde_json::Map<String, serde_json::Value>,
        surface: &str,
        title: Option<&str>,
        term: Option<&crate::TerminalPane>,
        pty: &HashMap<String, Arc<kasa_pty::PtySession>>,
        ws: &Workspace,
        pane_claude_sid: &HashMap<String, String>,
        agent_cfg: &HashMap<String, (String, String)>,
    ) {
        append_surface_record_metadata(obj, surface);
        // 캐릭터 영속(사용자: 재시작하면 미도리로 둔갑): pane_character 는
        // claude 프로세스 감지(was_claude)와 무관하게 살아있으므로, 감지가
        // 실패해도 캐릭터는 여기서 확실히 저장한다.
        if let Some(name) = ws.pane_character.get(surface) {
            obj.insert("character".to_string(), serde_json::json!(name));
        }
        // Every surface, including inactive tabs, must retain its remote endpoint.
        if let Some(info) = kasa_mcp::remote::remote_info(surface) {
            obj.insert("remote_base".into(), serde_json::json!(info.base));
            obj.insert("remote_pane".into(), serde_json::json!(info.remote_id));
            obj.insert("remote_view".into(), serde_json::json!(info.view));
            obj.insert("remote_owned".into(), serde_json::json!(info.owned));
            obj.insert("remote_label".into(), serde_json::json!(info.label));
            if let Some(cwd) = info.remote_cwd {
                obj.insert("remote_cwd".into(), serde_json::json!(cwd));
            }
            if let Some(cwd) = info.origin_cwd {
                obj.insert("remote_origin_cwd".into(), serde_json::json!(cwd));
            }
        }
        // 붙인 이름(`/rename`·`surface.rename`). 이게
        // 없으면 재시작마다 이름이 증발해 OSC 제목으로 되돌아갔다 — 이 앱은
        // 종료 시 자기 설치를 하므로 껐다 켜는 일이 잦고, 그래서 이름을
        // 붙이는 행위 자체가 몇 분짜리가 됐다.
        //
        // ⚠️ **핀이 섰을 때만 저장한다.** 핀 없는 `title` 은 안에서 도는
        // 프로그램이 쏜 OSC 라, 그걸 굳혀 두면 다음에 켤 때 「사람이 정한
        // 이름」인 척하면서 그 뒤의 OSC 를 영영 막는다.
        //
        // 자동으로 붙는 세션 주소(`yuzu-p0-4iz` 꼴)는 핀이 서 있어도 싣지 않는다.
        // 그 주소가 이름표에 오르는 길은 2026-09-14 에 닫았지만, 닫기 전에 이미
        // 굳어 파일에 들어간 값은 재시작마다 되살아나 핀까지 물고 온다. 거르는
        // 자리는 여기 한 곳이어야 한다 — 호출부(leaf·탭)에 나눠 두면 한쪽만
        // 고쳐지는, 이 레포가 되풀이해 온 사고가 된다.
        if let Some(t) = title
            .filter(|s| !s.trim().is_empty())
            .filter(|s| !crate::screenread::label_is_roster_agent(s))
        {
            obj.insert("title".to_string(), serde_json::json!(t));
        }
        // per-pane 실제 세션은 SocketSessionBound 로 채워진 pane_claude_sid
        // (정본)로 최우선 확정한다. 예전엔 argv(pane_record)·cwd 최신 jsonl 로
        // 폴백했는데, argv 없는 fresh `claude` 여럿이 같은 cwd 면 전부 cwd 최신
        // 세션 하나로 뭉쳐 재시작 시 여러 pane 이 다 같은 대화+캐릭터(미도리)로
        // 복원됐다(사용자: 다른 세션이 다 미도리로 뭉침). cwd 최신 폴백을 제거하고
        // pane_claude_sid 로만 session_id 를 확정한다 — 없으면 pane_record 의
        // argv sid, 그것도 없으면 restore_leaf 가 fresh claude 로 복원.
        if let Some(sid) = pane_claude_sid.get(surface) {
            obj.insert("session_id".to_string(), serde_json::json!(sid));
        }
        // Codex가 업데이트를 고른 뒤 종료하면 셸만 남아 live process 감지는
        // `null`이지만, 직전에 정확히 결속한 root rollout UUID는 남아 있다.
        // 그 둘이 함께 있을 때만 종류를 복원해 다음 저장이 fresh Claude로
        // 오염되지 않게 한다. cwd 최신 파일은 같은 폴더 pane을 섞으므로 보지 않는다.
        normalize_saved_agent_map_with(obj, |sid| {
            socket::codex_root_rollout_for_session(sid).is_some()
        });
        // 하네스를 갈아 끼운 자리에 남은 **옛 대화 번호**를 여기서 뺀다.
        // `pane_claude_sid` 는 claude sid 와 codex rollout uuid 를 한 칸에
        // 담고 전환을 신호로 걷지 않아, claude 를 끄고 codex 를 띄운 pane 이
        // 옛 claude 번호를 그대로 들고 저장된다. 복원은 자기 창고에서 그
        // 번호를 못 찾아 새 대화로 떨어지고, 그 fresh 세션의 번호가 다시
        // 저장되면서 옛 대화는 영영 안 열린다(2026-09-05: codex pane 에
        // 실린 claude 번호 탓에 4.3MB 대화가 통째로 묻혔다).
        let saved_agent = obj
            .get("was_agent")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        if let Some(sid) = obj
            .get("session_id")
            .and_then(|v| v.as_str())
            .map(str::to_string)
        {
            if !saved_sid_fits_agent(saved_agent.as_deref(), &sid) {
                eprintln!(
                    "[save] pane {surface}: {} 자리에 남은 옛 대화 번호({sid})를 빼고 저장한다",
                    saved_agent.as_deref().unwrap_or("claude")
                );
                obj.insert("session_id".to_string(), serde_json::Value::Null);
            }
        }
        // 끄기 직전 쓰던 모델·effort. 없으면 키를 아예 안 넣는다 — 복원은
        // "없으면 플래그를 안 붙인다"라, 빈 문자열을 남기면 되살릴 때
        // `--model ''` 같은 게 나갈 위험만 는다.
        //
        // 모델도 대화 번호와 같은 이유로 하네스와 대조한다 — 어긋나면
        // effort 까지 함께 버린다. 두 값은 같은 자리의 한 벌이라, 모델만
        // 빼면 codex 자리에 claude 의 `ultracode` 같은 값이 남는다.
        if let Some((model, effort)) = agent_cfg
            .get(surface)
            .filter(|(m, _)| saved_model_fits_agent(saved_agent.as_deref(), m))
        {
            if !model.is_empty() {
                obj.insert("model".to_string(), serde_json::json!(model));
            }
            if !effort.is_empty() {
                obj.insert("effort".to_string(), serde_json::json!(effort));
            }
        }
        // 권한 모드 영속 — 복원 resume 이 이 플래그를 안 실으면 학생이
        // 전부 물어보는 모드로 깨어난다(2026-08-29 미니 재시작 실측:
        // 셋 다 auto — 라이브 재기동으로 복구했다).
        if term.is_some_and(Self::term_bypass_on) {
            obj.insert("bypass".to_string(), serde_json::json!(true));
        }
        // Agent TUI는 새 화면을 다시 그리므로 옛 터미널 행을 넣지 않는다.
        // 일반 셸은 실제 PTY history를 저장해야 재시작 뒤 출력이 남는다.
        let restores_agent = obj
            .get("was_agent")
            .and_then(|v| v.as_str())
            .is_some_and(|agent| matches!(agent, "claude" | "codex" | "agy"))
            || obj
                .get("was_claude")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
            || obj
                .get("session_id")
                .and_then(|v| v.as_str())
                .is_some_and(|sid| !sid.is_empty());
        let sb = if restores_agent {
            Vec::new()
        } else {
            pty.get(surface)
                .map(|p| p.scrollback_text(SCROLLBACK_SAVE_MAX))
                .or_else(|| term.map(term_scrollback_lines))
                .unwrap_or_default()
        };
        obj.insert("scrollback".to_string(), serde_json::json!(sb));
        if let Some(server) = pty.get(surface).and_then(|session| crate::server_restore::save_server(ws, surface, session)) {
            obj.insert("server".into(), server);
            // A previous agent's binding can outlive its process in this tab.
            obj.insert("was_agent".into(), serde_json::Value::Null);
            obj.insert("session_id".into(), serde_json::Value::Null);
            obj.remove("was_claude");
            obj.remove("character");
        }
    }

    /// Walk a live PtyLayout into the nested JSON the restore loader reads,
    /// resolving each leaf's pane id to its cwd/claude record.
    pub(crate) fn layout_to_json(
        layout: &kasa_pty::PtyLayout,
        pty: &HashMap<String, Arc<kasa_pty::PtySession>>,
        ws: &Workspace,
        pane_claude_sid: &HashMap<String, String>,
        agent_cfg: &HashMap<String, (String, String)>,
    ) -> serde_json::Value {
        match layout {
            kasa_pty::PtyLayout::Leaf { pane_id } => {
                let mut rec = pty
                    .get(pane_id)
                    .map(|s| socket::pane_record(s))
                    .unwrap_or(serde_json::Value::Null);
                // 웹 pane 은 PTY 가 없어 record 가 Null 로 떨어져 재시작하면
                // 그 자리가 통째로 증발했다(복원이 Null leaf 를 버린다). 주소만
                // 있으면 되살릴 수 있으니 web_url 을 실은 최소 record 를 만든다
                // — 복원 쪽 분기는 restore_leaf 의 web_url 가지.
                if rec.is_null() {
                    if let Some(url) = ws
                        .panes
                        .get(pane_id)
                        .and_then(|p| p.tabs.iter().find_map(|t| t.web()))
                        .map(|w| w.url.clone())
                    {
                        rec = serde_json::json!({ "web_url": url });
                    }
                }
                // 원격 pane — 전송 명세를 실어 재시작 후 같은 원격 세션에 다시
                // 붙는다(restore_leaf 의 remote 가지). cwd·agent 감지(ps 기반)는
                // 원격이라 비지만, 스크롤백과 sid 마커 오버레이(pane_claude_sid)는
                // 로컬 파서 덕에 그대로 동작한다.
                if kasa_mcp::remote::is_remote_pane(pane_id) && !rec.is_object() {
                    rec = serde_json::json!({});
                }
                // Attach the pane's scrollback (text lines) so restore can
                // repaint what was on screen. Only when we have a real record.
                if rec.is_null() {
                    // PTY 가 없다고 **자리까지** 지우면 안 된다. null leaf 는 복원이
                    // 소리 없이 버리는데, 버려지는 것이 그 pane 하나가 아니다:
                    // 탭 목록은 아래 `obj` 블록 안에서만 실리므로 **그 안에서 돌던
                    // 학생이 통째로** 같이 사라지고, 좌우 leaf 가 다 null 이면
                    // `restore_window_layout_at` 이 None 을 반환해 **window 가 방째로**
                    // 없어진다(2026-09-14 실측: leaf 8 개 중 4 개가 null, 방 하나 증발).
                    //
                    // 그래서 빈 레코드로 승격시켜 pane 번호와 탭만은 남긴다. 번호가
                    // 남아야 `--resume` 으로 되살아난 학생의 `tell %N` 이 엉뚱한 pane
                    // 으로 가지 않는다(아래 pane_id 주석과 같은 이유).
                    eprintln!("[save] pane {pane_id} 에 PTY 가 없다 — 빈 자리로 남긴다(번호·탭은 유지)");
                    rec = serde_json::json!({});
                }
                if let Some(obj) = rec.as_object_mut() {
                    // pane id 자체를 저장한다. 이게 없으면 복원이 `%1` 부터 새로
                    // 번호를 매기는데, `--resume` 으로 되살아난 학생은 재시작 **전**
                    // 의 surface_id 를 대화 기록째 기억하고 있다 → `tell %5` 가 없는
                    // pane 이거나 그 사이 다른 pane 이 물려받은 번호로 배달된다
                    // (사용자: "재시작하면 학생들이 tell 을 이상한 pane 에 쓴다").
                    obj.insert("pane_id".to_string(), serde_json::json!(pane_id));
                    let pane = ws.panes.get(pane_id);
                    // leaf 의 몫은 **첫 탭**이다 — 바깥 pane id 가 곧 첫 탭의 pid 다.
                    let first = pane.and_then(|p| p.tabs.first());
                    // 탭을 빼내 번호가 갈린 pane 은 PTY·원격 명세가 첫 탭 pid 에 있다 — 그걸로
                    // 채워야 거울이 「빈 자리」로 저장되지 않는다(2026-09-18).
                    let record_of = first.and_then(|t| t.pid.as_deref()).unwrap_or(pane_id);
                    Self::fill_surface_record(
                        obj,
                        record_of,
                        first
                            .filter(|t| t.title_pinned)
                            .and_then(|t| t.title.as_deref()),
                        first.and_then(|t| t.term()),
                        pty,
                        ws,
                        pane_claude_sid,
                        agent_cfg,
                    );
                    // 탭 — leaf 에는 첫 탭만 실렸다. 둘째 탭부터는 자기 PtySession 을
                    // 갖는데(PaneTab.pid) 그게 저장에 안 실려, 앱을 껐다 켜면 탭에서
                    // 돌던 학생이 통째로 사라졌다(2026-09-08 지시 「탭 안의 복원하는
                    // 거 해」 — 그날 실제로 탭에서 돌던 학생을 잃었다). 웹 탭만 살아
                    // 남던 것은 위 web_url 갈래가 따로 건져 주기 때문이다.
                    let tabs: Vec<serde_json::Value> = pane
                        .map(|p| {
                            p.tabs
                                .iter()
                                .skip(1)
                                .filter_map(|t| {
                                    let tab_pid = t.pid.as_deref()?;
                                    // 탭의 PTY 가 그 순간 없어도 자리를 지우지 않는다 —
                                    // 바깥 leaf 와 같은 이유(e380ac71). 탭은 레코드가
                                    // 빠지면 그 학생이 저장 파일에 **존재한 적이 없게**
                                    // 되고, 되살릴 단서(세션 id·캐릭터)까지 함께 사라진다.
                                    // 아래 fill_surface_record 가 pane_claude_sid 로 세션을
                                    // 채우므로 빈 기록이라도 `--resume` 은 선다.
                                    let mut trec = pty
                                        .get(tab_pid)
                                        .map(|s| socket::pane_record(s))
                                        .unwrap_or_else(|| {
                                            eprintln!(
                                                "[save] 탭 {tab_pid} 에 PTY 가 없다 — 빈 기록으로 남긴다(세션은 유지)"
                                            );
                                            serde_json::json!({})
                                        });
                                    let o = trec.as_object_mut()?;
                                    o.insert(
                                        "pane_id".to_string(),
                                        serde_json::json!(tab_pid),
                                    );
                                    Self::fill_surface_record(
                                        o,
                                        tab_pid,
                                        t.title
                                            .as_deref()
                                            .filter(|_| t.title_pinned),
                                        t.term(),
                                        pty,
                                        ws,
                                        pane_claude_sid,
                                        agent_cfg,
                                    );
                                    Some(trec)
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    if !tabs.is_empty() {
                        obj.insert("tabs".to_string(), serde_json::json!(tabs));
                        // 보던 탭도 되살린다 — 안 실으면 재시작마다 첫 탭으로 돌아간다.
                        obj.insert(
                            "active_tab".to_string(),
                            serde_json::json!(pane.map(|p| p.active_tab).unwrap_or(0)),
                        );
                    }
                }
                serde_json::json!({ "leaf": rec })
            }
            kasa_pty::PtyLayout::Split { dir, ratio, a, b } => {
                let dir = match dir {
                    kasa_pty::SplitDir::Horizontal => "h",
                    kasa_pty::SplitDir::Vertical => "v",
                };
                serde_json::json!({ "split": {
                    "dir": dir,
                    "ratio": ratio,
                    "a": Self::layout_to_json(a, pty, ws, pane_claude_sid, agent_cfg),
                    "b": Self::layout_to_json(b, pty, ws, pane_claude_sid, agent_cfg),
                }})
            }
        }
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn inactive_remote_tab_record_keeps_endpoint_and_view_identity() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let pid = "%saved-remote-tab-regression";
        let remote = kasa_mcp::remote::restore_connection(kasa_mcp::remote::RemoteSpec {
            base: base.clone(), pane: Some("%73".into()), cwd: None, token: None,
            identity: kasa_mcp::remote::RemoteIdentity {
                label: "맥미니".into(), remote_cwd: Some("/source/project".into()),
                origin_cwd: Some("/viewer/project".into()), owned: false,
            },
        }, pid, 80, 24, true).unwrap();
        let mut record = serde_json::Map::new();
        let mut ws = crate::Workspace::default();
        ws.pane_character.insert(pid.into(), "모모이".into());
        super::App::fill_surface_record(&mut record, pid, Some("거울 탭"), None,
            &Default::default(), &ws, &Default::default(), &Default::default());
        assert_eq!(record["remote_base"], base);
        assert_eq!(record["remote_pane"], "%73");
        assert_eq!(record["remote_view"], true);
        assert_eq!(record["remote_label"], "맥미니");
        assert_eq!(record["remote_cwd"], "/source/project");
        assert_eq!(record["remote_origin_cwd"], "/viewer/project");
        assert_eq!(record["character"], "모모이");
        assert_eq!(record["title"], "거울 탭");
        drop(remote);
    }
}
