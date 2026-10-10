//! 닫은 칸 — 다시 열 수 있게 기록을 남기고, 되살리고, 오래 쉰 기록을 걷는다.
use super::*;

impl App {
    /// 닫히기 직전의 pane 을 되살릴 수 있게 적어 둔다. `remove_pane` 이 유일한
    /// 호출자다 — ⌘W·헤더 ×·CLI 어느 경로로 닫아도 거길 지나므로 한 곳에서 잡힌다.
    ///
    /// 레코드는 세션 저장이 쓰는 것과 **같은 형식**(`layout_to_json` 의 leaf 본문)이다.
    /// 그래서 되살리기가 `restore_leaf` 재사용이 되고, claude 였던 pane 은 `--resume`
    /// 까지 그 함수가 알아서 딸려 온다.
    ///
    /// `alive` 는 이 pane 의 PTY 가 계속 도는지다 — 사용자가 닫은 것(`hide_pane`)은
    /// 참이라 되살리기가 재부착이 되고, 셸이 스스로 끝난 것(`reap_dead_panes`)은
    /// 거짓이라 레코드로 새로 띄운다.
    /// `stashed` 는 사이드바 「숨기기」로 치운 것 — 두 정리 루프(개수 상한·idle reap)가
    /// 건너뛴다. 닫기(⌘W)는 `false` 로 들어와 종전대로 정리 대상이다.
    pub(crate) fn record_closed_pane(&mut self, pane: &str, alive: bool, stashed: bool) {
        if self.tmux.is_some() || !self.pty.contains_key(pane) {
            return;
        }
        // 되돌릴 자리를 가리킬 닻 — 같은 트리의 이웃 leaf(뒤쪽 우선, 없으면 앞).
        let neighbor = self.pty_layout.as_ref().and_then(|t| {
            let leaves = t.leaves();
            let i = leaves.iter().position(|l| *l == pane)?;
            leaves
                .get(i + 1)
                .or_else(|| leaves.get(i.wrapping_sub(1)))
                .map(|s| s.to_string())
        });
        // ws 락 **밖에서** 먼저 뜬다 — 이 스냅샷은 백엔드의 다른 뮤텍스를 잡으므로,
        // 락 안에서 부르면 두 락의 획득 순서가 뒤엉킬 자리를 만든다.
        let agent_cfg = self.agent_cfg_snapshot();
        let (rec, character) = {
            let ws = self.ws.lock().unwrap();
            let rec = Self::layout_to_json(
                &kasa_pty::PtyLayout::single(pane),
                &self.pty,
                &ws,
                &self.pane_claude_sid,
                &agent_cfg,
            );
            // 되살리기 목록의 학생 이름도 「클로드가 돌던 pane 인가」 관문을 지난다 —
            // 배정은 spawn 때 **모든** pane 에 되므로(`assign_character_env`) 안 걸면
            // 순수 셸을 닫아도 `%7 이로하 · kasaterm` 로 남는다(사용자 2026-08-20).
            // 닫는 순간 claude 가 이미 내려갔을 수 있어 바인딩된 세션 id 도 함께 본다 —
            // `count_claude_panes` 가 쓰는 기준과 같다.
            let was_agent = self.pane_claude_sid.contains_key(pane)
                || self.pty.get(pane).and_then(|p| p.active_agent()).is_some();
            let ch = was_agent
                .then(|| ws.pane_character.get(pane).cloned())
                .flatten()
                .unwrap_or_default();
            (rec, ch)
        };
        let Some(mut rec) = rec.get("leaf").cloned().filter(|r| !r.is_null()) else {
            return;
        };
        // 학생이 먼저 죽고 나중에 닫힌 자리 — 산 표식은 이미 걷혀 이름도 대화 번호도 없다.
        // 비석으로 채워 둬야 되살리기 줄에 얼굴이 뜨고, 눌러서 그 대화를 이어 열 수 있다.
        let mut character = character;
        if let Some((seat_name, seat_sid)) = self.pane_last_seat.get(pane).cloned() {
            if character.is_empty() { character.clone_from(&seat_name); }
            if let Some(obj) = rec.as_object_mut() {
                if !seat_name.is_empty() && obj.get("character").and_then(|v| v.as_str()).is_none() {
                    obj.insert("character".into(), serde_json::json!(seat_name));
                }
                if !seat_sid.is_empty() && obj.get("session_id").and_then(|v| v.as_str()).is_none() {
                    let harness = if socket::codex_root_rollout_for_session(&seat_sid).is_some() { "codex" } else { "claude" };
                    obj.insert("session_id".into(), serde_json::json!(seat_sid));
                    obj.insert("was_agent".into(), serde_json::json!(harness));
                }
            }
        }
        if let Some(progress) = &self.restore_progress { progress.preserve_record(&mut rec); }
        // cwd 캐시는 `lsof` 로 채워져 갓 만든 pane 에선 아직 비어 있다 — 그때는
        // 레코드에 실린 cwd 로 되짚는다(복원도 그 값을 쓰므로 어긋날 일이 없다).
        let folder = self
            .pane_view_cwd
            .get(pane)
            .or_else(|| self.pane_cwd_cache.get(pane))
            .map(|p| p.to_string_lossy().into_owned())
            .or_else(|| {
                rec.get("cwd")
                    .and_then(|c| c.as_str())
                    .map(|s| s.to_string())
            })
            .and_then(|s| s.rsplit('/').find(|t| !t.is_empty()).map(|t| t.to_string()))
            .unwrap_or_default();
        let window = self.window_of_pane(pane).unwrap_or(self.active_window);
        self.push_closed_pane(crate::ClosedPane {
            rec,
            pane_id: pane.to_string(),
            character,
            folder,
            neighbor,
            window,
            alive,
            stashed,
            idle_since: (alive && !stashed).then(Instant::now),
            preview: None,
        });
        self.chrome_dirty = true;
    }

    /// 그 번호를 **지금 물고 있는** 되살리기 레코드. 죽은 레코드는 여기 안 걸린다.
    ///
    /// pane 번호는 재사용된다 — [`Self::used_pane_ids`] 가 `alive` 인 레코드의
    /// 번호만 잡아 두므로, 이미 죽은 레코드의 번호는 다음 pane 에 그대로 다시
    /// 나간다. 그래서 번호만 맞춰 보면 **살아서 도는 남**이 옛 묘비 때문에
    /// 「닫힌 pane」으로 판정된다(2026-08-25: 방 6 의 `%21` 이 그랬다. 모모이가
    /// 멀쩡히 일하는데 인포가 그 pane 을 되살리기 칸으로 보내, 그 방에 설 pane 이
    /// 하나도 없어 **방 자체가 목록에서 사라졌다**).
    ///
    /// 되살리기 목록에 같은 번호가 여럿 뜨는 것은 정상이다 — 서로 다른 pane 의
    /// 서로 다른 대화라 하나로 합치면 안 된다.
    pub(crate) fn stashed_record(&self, pane: &str) -> Option<&crate::ClosedPane> {
        stashed_in(&self.closed_panes, pane)
    }

    /// 위와 같은 판정의 인덱스 판. 살아 있는 것을 먼저 집고, 없으면 죽은 기록에서
    /// 찾는다 — 목록에서 지우는 조작은 묘비에도 걸려야 한다.
    pub(crate) fn closed_pane_index(&self, pane: &str) -> Option<usize> {
        closed_index_in(&self.closed_panes, pane)
    }

    /// 그 번호를 물고 있던 **살아 있는** 레코드를 걷는다 — PTY 가 진짜 죽는 길목
    /// (`remove_pane`)에서 부른다. 걷지 않으면 숨겨 둔 pane 을 끈 뒤 같은 번호가
    /// 「살아 있음」(옛 숨김 레코드)과 「죽음」(방금 적은 묘비) 둘로 남아, 사이드바가
    /// 죽은 pane 을 재부착할 수 있다고 보이고 `used_pane_ids` 가 그 번호를 영영 잡아
    /// 둔다(2026-09-02 실측: `dismiss` 두 번 = 숨김 → 끄기). 죽은 레코드는 건드리지
    /// 않는다 — 그건 다른 대화의 묘비일 수 있다.
    pub(crate) fn drop_live_closed_records(&mut self, pane: &str) {
        if drop_live_records(&mut self.closed_panes, pane) > 0 {
            self.chrome_dirty = true;
        }
    }

    /// 닫힘 스택에 넣고 상한을 정리한다 — pane 닫기와 미리보기 탭 닫기가 같은
    /// 스택을 쓰므로 ⌘⇧T 가 「가장 최근에 닫은 것」 순서를 하나로 지킨다.
    ///
    /// 오래된 것부터 버린다 — 레코드마다 스크롤백이 통째 붙어 있고, 살아 있는
    /// 것은 프로세스까지 물고 있다. 여기서 놓지 않으면 닫기만 반복해도 셸이
    /// 무한정 쌓인다.
    /// 상한은 **정리 대상만** 센다. 숨긴 것(`stashed`)은 세지도 놓지도 않는다 —
    /// 숨겨 둔 학생 여럿 때문에 방금 닫은 pane 이 밀려 죽으면 안 된다.
    pub(crate) fn push_closed_pane(&mut self, c: crate::ClosedPane) {
        self.closed_panes.push(c);
        while self.closed_panes.iter().filter(|c| !c.stashed).count() > crate::CLOSED_PANE_KEEP {
            let Some(i) = self.closed_panes.iter().position(|c| !c.stashed) else {
                break;
            };
            let c = self.closed_panes.remove(i);
            if c.alive {
                self.kill_hidden_pane(&c.pane_id);
            }
        }
    }

    /// Absolute close deadline. Keep recovery metadata after stopping execution.
    pub(crate) fn reap_idle_closed_panes(&mut self) {
        self.finish_close_grace();
    }

    /// 인포의 × — 되살리기 목록에서 지우고, 아직 돌고 있으면 프로세스까지 끈다.
    pub(crate) fn discard_closed_pane_at(&mut self, idx: usize) {
        if idx >= self.closed_panes.len() {
            return;
        }
        let c = self.closed_panes.remove(idx);
        if c.alive {
            self.kill_hidden_pane(&c.pane_id);
        }
        // 마지막 하나를 지우면 하단바가 접힌다 — 그만큼 그리드가 다시 늘어야 한다.
        // 안 그러면 바가 있던 40px 이 빈 띠로 남는다(닫을 때는 `hide_pane` 이 이미
        // 같은 일을 한다).
        let (cols, rows) = self.window_cells();
        self.resize_backend(cols, rows);
        self.chrome_dirty = true;
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    /// ⌘⇧T — 가장 최근에 닫은 pane 을 되살린다.
    pub(crate) fn reopen_closed_pane(&mut self) {
        let Some(c) = self.closed_panes.pop() else {
            return;
        };
        self.reopen_pane_record(c);
    }

    /// 인포의 닫힘 줄 클릭용 — 스택 가운데 하나를 지목해 되살린다.
    pub(crate) fn reopen_closed_pane_at(&mut self, idx: usize) {
        if idx >= self.closed_panes.len() {
            return;
        }
        let c = self.closed_panes.remove(idx);
        self.reopen_pane_record(c);
    }

    /// 닫힌 방이 아직 있으면 그 방으로 돌아가 원래 이웃 옆에 되살린다. 이웃이 그 사이
    /// 사라졌으면 활성 pane 옆으로 — 자리를 못 찾았다고 되살리기를 포기하진 않는다.
    pub(super) fn reopen_pane_record(&mut self, c: crate::ClosedPane) {
        // 밖에 나가 있는 방으로는 보내지 않는다 — `switch_window` 가 그 창을 앞으로
        // 보낼 뿐 메인은 그대로라, 되살린 pane 이 보이지 않는 방에 들어가 버린다.
        if c.window < self.windows.len() && c.window != self.active_window {
            self.switch_window(c.window);
        }
        if self.internal_room_active_any() && !self.return_from_active_internal_room() {
            // 설정 sole-leaf를 되살리기 anchor로 쓰지 않는다. 돌아갈 사용자 방이
            // 없다면 레코드를 잃지 않고 제자리에 돌려놓는다.
            self.closed_panes.push(c);
            return;
        }
        // 미리보기 탭 레코드 — pane 이 아니라 보조 탭이었으니 `open_file` 로 다시
        // 연다. 원래 붙어 있던 pane 이 사라졌으면 open_file 이 활성 pane 으로 폴백.
        if let Some((outer, path)) = c.preview.clone() {
            self.open_file(path.clone(), Some(outer), true);
            // `as_tab` 은 배경 탭 규약이지만 ⌘⇧T 는 「다시 보여 달라」다 —
            // 되살린 탭을 앞으로 끌어낸다.
            {
                let mut ws = self.ws.lock().unwrap();
                let found = ws.panes.iter().find_map(|(id, p)| {
                    p.tabs
                        .iter()
                        .position(|t| t.preview_path.as_deref() == Some(path.as_path()))
                        .map(|i| (id.clone(), i))
                });
                if let Some((id, i)) = found {
                    if let Some(p) = ws.panes.get_mut(&id) {
                        p.active_tab = i;
                        p.dirty = true;
                    }
                    ws.active_pane = Some(id);
                }
            }
            self.handoff_ime_to_active_surface();
            self.chrome_dirty = true;
            if let Some(w) = &self.window {
                w.request_redraw();
            }
            eprintln!("[reopen] 미리보기 {} 되살림", path.display());
            return;
        }
        // 아직 돌고 있으면 새로 띄우지 않는다 — 그 pane 은 화면에서만 빠져 있었을
        // 뿐 셸도 claude 도 그대로다. `--resume` 으로 대화를 되감으면 오히려 하던
        // 일이 끊긴다. `alive` 를 믿지 말고 실제 PTY 로 확인하는 건, 숨긴 사이에
        // 셸이 스스로 끝났을 수 있어서다(그때는 아래 레코드 경로로 흘러간다).
        let attached = c.alive && self.pty.contains_key(&c.pane_id);
        let new_id = if attached {
            self.close_grace_input(&c.pane_id, false);
            c.pane_id.clone()
        } else {
            let (cols, rows) = self.window_cells();
            let Some(id) = self.restore_leaf(&c.rec, cols, rows) else {
                eprintln!("[reopen] {} 되살리기 실패 — PTY 를 못 띄웠다", c.pane_id);
                return;
            };
            id
        };
        let anchor = c
            .neighbor
            .filter(|n| {
                self.pty.contains_key(n)
                    && self
                        .pty_layout
                        .as_ref()
                        .is_some_and(|t| t.leaves().iter().any(|l| *l == n.as_str()))
            })
            .or_else(|| self.ws.lock().unwrap().active_pane.clone());
        if anchor.as_deref().is_some_and(|pane| {
            self.ensure_user_mutation_target(
                pane,
                crate::settings_room::SettingsMutation::Split,
            )
            .is_err()
        }) {
            return;
        }
        let grafted = match (anchor, self.pty_layout.as_mut()) {
            (Some(a), Some(tree)) => {
                tree.split_leaf(&a, kasa_pty::SplitDir::Horizontal, new_id.clone())
            }
            _ => false,
        };
        if !grafted {
            // 트리가 비었거나 닻이 사라졌다 — 이 pane 을 유일 leaf 로 세운다.
            self.pty_layout = Some(kasa_pty::PtyLayout::single(new_id.as_str()));
        }
        {
            let mut ws = self.ws.lock().unwrap();
            ws.active_pane = Some(new_id.clone());
            for pane in ws.panes.values_mut() {
                pane.dirty = true;
            }
        }
        self.handoff_ime_to_active_surface();
        let (cols, rows) = self.window_cells();
        self.resize_backend(cols, rows);
        self.publish_pty_layout();
        self.session_touched = true;
        self.chrome_dirty = true;
        if let Some(w) = &self.window {
            w.request_redraw();
        }
        eprintln!("[reopen] {} → {new_id} 되살림", c.pane_id);
    }
}

/// [`App::stashed_record`] 의 판정 본체. App 없이 검사할 수 있게 갈라 뒀다.
pub(super) fn stashed_in<'a>(list: &'a [crate::ClosedPane], pane: &str) -> Option<&'a crate::ClosedPane> {
    list.iter().find(|c| c.alive && c.pane_id == pane)
}

/// [`App::closed_pane_index`] 의 판정 본체.
pub(super) fn closed_index_in(list: &[crate::ClosedPane], pane: &str) -> Option<usize> {
    list.iter()
        .position(|c| c.alive && c.pane_id == pane)
        .or_else(|| list.iter().position(|c| c.pane_id == pane))
}

/// [`App::drop_live_closed_records`] 의 본체. 걷은 개수를 돌려준다.
pub(super) fn drop_live_records(list: &mut Vec<crate::ClosedPane>, pane: &str) -> usize {
    let before = list.len();
    list.retain(|c| !(c.alive && c.pane_id == pane));
    before - list.len()
}

#[cfg(test)]
mod dead_agent_seat_tests {
    //! 학생이 죽은 뒤 닫힌 자리 — 되살리기 줄이 얼굴과 대화 번호를 되찾는지.
    //! 산 표식(`pane_character`·`pane_claude_sid`)은 죽는 순간 걷히므로 비석이 정본이다.

    /// `close_pane` 이 비석으로 기록을 메우는 규칙만 떼어낸 것(App 없이 검사한다).
    fn fill_from_seat(rec: &mut serde_json::Value, character: &mut String, seat: (&str, &str)) {
        let (seat_name, seat_sid) = seat;
        if character.is_empty() { *character = seat_name.to_string(); }
        let Some(obj) = rec.as_object_mut() else { return };
        if !seat_name.is_empty() && obj.get("character").and_then(|v| v.as_str()).is_none() {
            obj.insert("character".into(), serde_json::json!(seat_name));
        }
        if !seat_sid.is_empty() && obj.get("session_id").and_then(|v| v.as_str()).is_none() {
            obj.insert("session_id".into(), serde_json::json!(seat_sid));
            obj.insert("was_agent".into(), serde_json::json!("claude"));
        }
    }

    #[test]
    fn a_dead_students_seat_restores_face_and_conversation() {
        let mut rec = serde_json::json!({"pane_id": "%13", "was_agent": null, "session_id": null,
            "cwd": "/Users/kasa/Desktop"});
        let mut character = String::new();
        fill_from_seat(&mut rec, &mut character, ("코유키", "fd844548"));
        assert_eq!(character, "코유키");
        assert_eq!(rec["character"], "코유키");
        assert_eq!(rec["session_id"], "fd844548");
        assert_eq!(rec["was_agent"], "claude");
    }

    #[test]
    fn a_live_record_is_never_overwritten_by_the_headstone() {
        let mut rec = serde_json::json!({"pane_id": "%3", "character": "아로나", "session_id": "live-sid",
            "was_agent": "codex"});
        let mut character = "아로나".to_string();
        fill_from_seat(&mut rec, &mut character, ("코유키", "fd844548"));
        assert_eq!(character, "아로나");
        assert_eq!(rec["character"], "아로나");
        assert_eq!(rec["session_id"], "live-sid");
        assert_eq!(rec["was_agent"], "codex", "산 기록의 하네스를 비석이 덮으면 안 된다");
    }
}

#[cfg(test)]
mod closed_pane_id_reuse_tests {
    use super::{closed_index_in, drop_live_records, stashed_in};
    use crate::ClosedPane;

    fn rec(pane: &str, alive: bool, folder: &str) -> ClosedPane {
        ClosedPane {
            rec: serde_json::Value::Null,
            pane_id: pane.to_string(),
            character: String::new(),
            folder: folder.to_string(),
            neighbor: None,
            window: 0,
            alive,
            stashed: false,
            idle_since: None,
            preview: None,
        }
    }

    /// 죽은 기록은 번호를 안 잡으므로(`used_pane_ids`) 같은 번호가 다음 pane 에
    /// 다시 나간다. 그 새 pane 을 「닫힌 것」으로 보면 인포에서 통째로 사라지고,
    /// 그 방의 유일한 pane 이었다면 **방까지 목록에서 없어진다**(2026-08-25 실측).
    #[test]
    fn dead_record_does_not_claim_a_live_pane() {
        let list = [
            rec("%21", false, "nacho-neko"),
            rec("%21", false, "Desktop"),
        ];
        assert!(stashed_in(&list, "%21").is_none());
    }

    /// 숨긴 pane 은 실제로 그 번호를 물고 있으니 걸려야 한다.
    #[test]
    fn live_record_is_found() {
        let list = [rec("%21", false, "옛것"), rec("%21", true, "숨긴것")];
        assert_eq!(
            stashed_in(&list, "%21").map(|c| c.folder.as_str()),
            Some("숨긴것")
        );
    }

    /// 목록 조작(숨김 해제·끄기)은 살아 있는 것을 먼저 집는다 — 앞에 놓인 묘비
    /// 때문에 정작 자원을 문 항목을 못 건드리면 안 된다.
    #[test]
    fn index_prefers_the_live_record() {
        let list = [rec("%21", false, "옛것"), rec("%21", true, "숨긴것")];
        assert_eq!(closed_index_in(&list, "%21"), Some(1));
    }

    /// 묘비만 있으면 그건 집는다 — 목록에서 지우는 조작은 죽은 기록에도 걸려야 한다.
    #[test]
    fn index_falls_back_to_a_dead_record() {
        let list = [rec("%21", false, "옛것")];
        assert_eq!(closed_index_in(&list, "%21"), Some(0));
        assert_eq!(closed_index_in(&list, "%22"), None);
    }

    /// 숨겨 둔 pane 을 끄면 그 번호의 살아 있는 레코드만 걷힌다 — 다른 대화의 묘비와
    /// 남의 번호는 그대로. 2026-09-02: `dismiss` 두 번에 `%6` 이 「살아 있음」과
    /// 「죽음」으로 둘 남았다.
    #[test]
    fn killing_a_hidden_pane_drops_only_its_live_record() {
        let mut list = vec![
            rec("%6", false, "옛대화"),
            rec("%6", true, "숨긴것"),
            rec("%7", true, "남의것"),
        ];
        assert_eq!(drop_live_records(&mut list, "%6"), 1);
        let left: Vec<(&str, bool)> = list
            .iter()
            .map(|c| (c.folder.as_str(), c.alive))
            .collect();
        assert_eq!(left, vec![("옛대화", false), ("남의것", true)]);
        // 살아 있는 레코드가 없으면 아무것도 안 걷는다 — 묘비는 건드리지 않는다.
        assert_eq!(drop_live_records(&mut list, "%6"), 0);
        assert_eq!(list.len(), 2);
    }
}
