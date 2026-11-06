use std::sync::{Arc, Mutex};
use std::time::Duration;
use anyhow::{anyhow, Result};
use kasa_pty::{ExtEvent, ExternalIo, PtyOptions, PtySession};
use kasa_remote::remote::{RemoteSpec, RepoSyncFetch, RepoSyncMeta, connect, connect_view, is_remote_pane, is_view_pane, kill_remote, restore_connection, set_viewport, held_source_size, expand_source, restore_source, remote_get_json, remote_post_json, push_repo_sync, fetch_repo_sync};

mod tests {
    use super::*;
    fn wait_visible(sess: &PtySession, needle: &str, secs: u64) -> bool {
        let deadline = std::time::Instant::now() + Duration::from_secs(secs);
        while std::time::Instant::now() < deadline {
            if sess.visible_text(50).contains(needle) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        false
    }

    /// 파일 싱크 전 구간 — 같은 프로세스에 서버를 띄워 「원격 기계」로 삼고,
    /// ①push_repo_sync(순방향: 로컬 스냅샷을 원격 레포에 재현) ②fetch_repo_sync
    /// (역방향: 원격의 것을 떠 와서 로컬에 apply) ③깨끗한 레포의 Nothing 까지
    /// HTTP 창구·헤더 메타·클라이언트 파싱을 한 번에 검증한다.
    /// unix 전용인 이유는 [`reposync`] 쪽 왕복 테스트와 같다 — 레포 상태를 `sh -c`
    /// 로 짓는데 Windows 엔 `sh` 가 없고, 이걸 쓰는 이사가 `#[cfg(unix)]` 다.
    #[cfg(unix)]
    #[test]
    fn repo_sync_http_roundtrip() {
        let sh = |dir: &std::path::Path, cmd: &str| -> String {
            let out = std::process::Command::new("sh")
                .arg("-c")
                .arg(cmd)
                .current_dir(dir)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "`{cmd}` 실패: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        let backend: Arc<dyn kasa_socket::backend::Backend> = Arc::new(
            kasa_backend::standalone::StandaloneBackend::new(std::env::temp_dir()),
        );
        let port = crate::spawn_http_server_opts(backend, 0, false).expect("server");
        let base = format!("http://127.0.0.1:{port}");
        let root =
            std::env::temp_dir().join(format!("kasaterm-reposync-http-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let ident = "-c user.name=t -c user.email=t@t";
        sh(&root, "git init --bare origin.git");
        sh(&root, &format!(
            "git clone origin.git src 2>/dev/null && cd src && echo one > a.txt && git add -A && git {ident} commit -qm init && git push -q origin HEAD"
        ));
        sh(&root, "git clone origin.git dst 2>/dev/null");
        sh(&root, "git clone origin.git back 2>/dev/null");
        sh(&root, &format!(
            "cd src && echo two >> a.txt && git add -A && git {ident} commit -qm local-only && echo three >> a.txt && echo new > c.txt"
        ));
        let (src, dst, back) = (root.join("src"), root.join("dst"), root.join("back"));
        // ① 순방향: src 스냅샷 → HTTP POST → 서버가 dst 에 재현.
        let snap = kasa_remote::reposync::snapshot(&src)
            .unwrap()
            .expect("실어 갈 것");
        let meta = RepoSyncMeta {
            head: snap.head.clone(),
            sync: snap.sync,
            branch: snap.branch,
            origin: snap.origin,
            dirty: snap.dirty,
        };
        let applied = push_repo_sync(
            &base,
            &dst.to_string_lossy(),
            &meta,
            snap.bundle,
            None,
            false,
        )
        .expect("push")
        .expect("창구가 있어야 한다");
        assert!(applied.contains("미저장"), "applied={applied}");
        assert_eq!(sh(&dst, "git rev-parse HEAD"), snap.head);
        assert_eq!(sh(&dst, "cat a.txt"), "one\ntwo\nthree");
        assert_eq!(sh(&dst, "cat c.txt"), "new");
        // ② 역방향: 방금 미push+미커밋이 생긴 dst 를 GET 으로 떠 와 back 에 재현.
        match fetch_repo_sync(&base, &dst.to_string_lossy(), None).expect("fetch") {
            RepoSyncFetch::Bundle(m, bytes) => {
                assert_eq!(m.head, snap.head);
                assert!(m.dirty);
                let msg = kasa_remote::reposync::apply(
                    &back,
                    &bytes,
                    &m.head,
                    &m.sync,
                    &m.branch,
                    m.dirty,
                    false,
                    kasa_remote::reposync::OnBlock::Bail,
                )
                .expect("apply");
                assert!(msg.contains("미저장"), "msg={msg}");
                assert_eq!(sh(&back, "cat a.txt"), "one\ntwo\nthree");
                assert_eq!(sh(&back, "cat c.txt"), "new");
            }
            _ => panic!("Bundle 이 와야 한다"),
        }
        // ③ 깨끗한 레포는 Nothing — 실어 올 것이 없다는 판정도 창구를 거쳐 온다.
        sh(&root, "git clone origin.git clean 2>/dev/null");
        assert!(matches!(
            fetch_repo_sync(&base, &root.join("clean").to_string_lossy(), None).expect("fetch"),
            RepoSyncFetch::Nothing
        ));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 같은 프로세스에 서버(spawn_http_server_opts)와 클라이언트를 함께 띄워
    /// 전 구간을 돈다: 스폰 → 타이핑 → 둘째 클라이언트 이어받기(스냅샷) → kill.
    #[test]
    fn remote_spawn_type_reattach_kill_roundtrip() {
        let backend: Arc<dyn kasa_socket::backend::Backend> = Arc::new(
            kasa_backend::standalone::StandaloneBackend::new(std::env::temp_dir()),
        );
        let port = crate::spawn_http_server_opts(backend, 0, false).expect("server");
        let spec = RemoteSpec {
            base: format!("http://127.0.0.1:{port}"),
            pane: None,
            cwd: None,
            token: None,
            identity: Default::default(),
        };
        let rs = connect(spec.clone(), "%rmt0", 60, 12).expect("connect");
        assert!(rs.remote_id.starts_with("web-"), "id={}", rs.remote_id);
        assert!(is_remote_pane("%rmt0"));
        // 셸이 프롬프트를 그린 뒤에 친다 — 그 전에 들어간 글은 셸이 시작하며 입력 큐를 비울 때 사라져, 부하가
        // 걸린 기계에서 셸이 늦게 뜨면 에코가 영영 안 맺혔다.
        let shell_up = (0..300).any(|_| {
            let up = !rs.session.visible_text(50).trim().is_empty();
            if !up { std::thread::sleep(Duration::from_millis(100)); }
            up
        });
        assert!(shell_up, "셸 프롬프트가 30초 안에 안 떴다");
        // 셸이 뜨고 에코가 로컬 그리드에 맺힌다 — 원격 파싱이 아니라 로컬 파싱.
        rs.session
            .send_bytes(b"printf 'hi-remote-42\\n'\r")
            .unwrap();
        assert!(
            wait_visible(&rs.session, "hi-remote-42", 10),
            "타이핑 에코가 그리드에 없다: {:?}",
            rs.session.visible_text(20)
        );
        // 이어받기: 같은 remote_id 로 붙은 둘째 클라이언트는 접속 스냅샷만으로
        // 이전 출력을 본다(그 사이 새 출력 없음).
        let rs2 = connect(
            RemoteSpec {
                pane: Some(rs.remote_id.clone()),
                ..spec.clone()
            },
            "%rmt1",
            60,
            12,
        )
        .expect("reattach");
        assert!(
            wait_visible(&rs2.session, "hi-remote-42", 10),
            "스냅샷 재생에 이전 출력이 없다"
        );
        // kill — keep 목록에서 걷히고, 세션은 eof 프레임으로 pane 을 걷게 한다.
        assert!(kill_remote("%rmt0"));
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            let kept = kasa_pty::kept_sessions();
            if !kept.contains(&rs.remote_id) {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "kill 후에도 keep 목록에 남아 있다: {kept:?}"
            );
            std::thread::sleep(Duration::from_millis(100));
        }
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let mut saw_eof = false;
        while std::time::Instant::now() < deadline {
            match rs.session.screens.recv_timeout(Duration::from_millis(200)) {
                Ok(u) if u.eof => {
                    saw_eof = true;
                    break;
                }
                _ => continue,
            }
        }
        assert!(saw_eof, "kill 뒤 eof 센티널이 안 왔다");
    }

    /// 거울(뷰어)은 원본 세션의 격자를 **어떤 경로로도** 못 바꾼다 — 붙는 순간의
    /// 맞춤 resize 도, 그 뒤 pane 을 줄여 생기는 resize 도. 대조군으로 소유자
    /// 연결(connect)은 여전히 바꾼다. 서버 격자는 「아무것도 안 바꾸는」 거울을
    /// 하나 더 붙여 그 size 핸드셰이크로 읽는다.
    #[test]
    fn view_link_never_resizes_origin() {
        let backend: Arc<dyn kasa_socket::backend::Backend> = Arc::new(
            kasa_backend::standalone::StandaloneBackend::new(std::env::temp_dir()),
        );
        let port = crate::spawn_http_server_opts(backend, 0, false).expect("server");
        let spec = RemoteSpec {
            base: format!("http://127.0.0.1:{port}"),
            pane: None,
            cwd: None,
            token: None,
            identity: Default::default(),
        };
        let owner = connect(spec.clone(), "%vw0", 100, 30).expect("connect");
        // 서버 격자 읽기: 아무것도 안 바꾸는 거울을 하나 붙여 size 핸드셰이크만 본다.
        fn server_size(spec: &RemoteSpec, remote_id: &str, tag: &str) -> (u16, u16) {
            connect_view(
                RemoteSpec {
                    pane: Some(remote_id.to_string()),
                    ..spec.clone()
                },
                tag,
            )
            .expect("probe")
            .session
            .size()
        }
        fn settle(spec: &RemoteSpec, remote_id: &str, want: (u16, u16), tag: &str) -> (u16, u16) {
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            let mut n = 0u32;
            let mut got = server_size(spec, remote_id, &format!("{tag}{n}"));
            while got != want && std::time::Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(150));
                n += 1;
                got = server_size(spec, remote_id, &format!("{tag}{n}"));
            }
            got
        }
        let rid = owner.remote_id.clone();
        // 소유자가 세운 격자가 기준선.
        assert_eq!(
            settle(&spec, &rid, (100, 30), "%vwa"),
            (100, 30),
            "소유자 resize 가 안 먹었다"
        );

        // 거울로 붙는다 — 붙는 것만으로 격자가 흔들리면 안 된다.
        let mirror = connect_view(
            RemoteSpec {
                pane: Some(rid.clone()),
                ..spec.clone()
            },
            "%vw1",
        )
        .expect("view");
        assert!(is_view_pane("%vw1"));
        assert!(!is_view_pane("%vw0"), "소유자 연결이 거울로 오판됐다");
        assert_eq!(
            mirror.session.size(),
            (100, 30),
            "거울이 서버 격자를 못 받았다"
        );

        // 거울 pane 을 줄인다(로컬 창 축소와 같은 길) — 원본은 그대로여야 한다.
        let _ = mirror.session.resize(40, 10);
        std::thread::sleep(Duration::from_millis(400));
        assert_eq!(
            server_size(&spec, &rid, "%vwb"),
            (100, 30),
            "거울이 원본 격자를 뺏었다"
        );

        // 대조군: 주인은 여전히 바꿀 수 있다.
        let _ = owner.session.resize(70, 20);
        assert_eq!(
            settle(&spec, &rid, (70, 20), "%vwc"),
            (70, 20),
            "소유자 resize 가 막혀 버렸다"
        );
        assert!(kill_remote("%vw0"));
        // kill 은 서버를 거쳐 늦게 닿는다 — 이 검사가 붙든 셸이 뒤 검사 시간까지 살아
        // `/term/panes` 에 남지 않게 직접 놓는다.
        kasa_pty::release_session(&rid);
    }

    #[test]
    fn legacy_gui_attach_preserves_source_size_and_survives_viewer_drop() {
        let id = format!("%{}", std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).unwrap().as_micros());
        let local = format!("legacy-mirror-{id}");
        let (_events, receiver) = crossbeam_channel::unbounded();
        let source = Arc::new(PtySession::start_external(
            PtyOptions { pane_id: id.clone(), cols: 60, rows: 12, ..Default::default() },
            ExternalIo { events: receiver, writer: Box::new(std::io::sink()), on_resize: Arc::new(|_, _| {}) },
        ).unwrap());
        kasa_pty::register_session(&id, &source);
        let backend: Arc<dyn kasa_socket::backend::Backend> = Arc::new(
            kasa_backend::standalone::StandaloneBackend::new(std::env::temp_dir()),
        );
        let port = crate::spawn_http_server_opts(backend, 0, false).unwrap();
        let spec = RemoteSpec {
            base: format!("http://127.0.0.1:{port}"), pane: Some(id.clone()),
            cwd: None, token: None, identity: Default::default(),
        };
        let mirror = connect(spec.clone(), &local, 100, 30).unwrap();
        assert!(is_view_pane(&local));
        assert_eq!(source.size(), (60, 12));
        assert!(set_viewport(&local, 100, 30));
        std::thread::sleep(Duration::from_millis(300));
        assert!(!source.has_viewer_size_control(), "legacy attach acquired source control");
        assert_eq!(source.size(), (60, 12));
        source.resize(40, 10).unwrap();
        assert_eq!(source.size(), (40, 10), "source lost ownership of its own dimensions");
        drop(mirror);
        let until = std::time::Instant::now() + Duration::from_secs(5);
        while is_view_pane(&local) {
            assert!(std::time::Instant::now() < until, "closing the mirror did not detach");
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(source.size(), (40, 10));
        assert!(kasa_pty::lookup_session(&id).is_some(), "closing the mirror killed its origin");
        let restored = restore_connection(spec, &local, 100, 30, false).unwrap();
        assert!(is_view_pane(&local), "legacy saved own=1 GUI attach must also restore passively");
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(source.size(), (40, 10), "deferred restore changed the host dimensions");
        drop(restored);
    }

    #[test]
    fn viewport_mirror_preserves_source_and_last_cell_input() {
        // 한도는 멈춘 시험을 끊는 울타리일 뿐 — 판정은 상태로만 한다(`standalone::mirror_host` 머리말).
        fn wait_for(mut condition: impl FnMut() -> bool, message: &str) {
            let deadline = std::time::Instant::now() + Duration::from_secs(60);
            while !condition() {
                assert!(std::time::Instant::now() < deadline, "{message}");
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        struct InputLog(Arc<Mutex<Vec<u8>>>);
        impl std::io::Write for InputLog {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
        }
        let input = Arc::new(Mutex::new(Vec::new()));
        // standalone 이 여는 창은 `web-` 이고 방이 없다. 이름이 다르면 `/term/panes` 가 이 창을
        // 「닫았지만 산 창」으로 내, 열쇠로 되찾는 재접속 후보에서 빠진다.
        let id = format!("web-viewport-source-{}", uuid::Uuid::new_v4());
        let (events, receiver) = crossbeam_channel::unbounded();
        let source = Arc::new(PtySession::start_external(
            PtyOptions { pane_id: id.clone(), cols: 120, rows: 40, ..Default::default() },
            ExternalIo {
                events: receiver,
                writer: Box::new(InputLog(input.clone())),
                on_resize: Arc::new(|_, _| {}),
            },
        ).unwrap());
        kasa_pty::register_session(&id, &source);
        // 열쇠를 먼저 붙여 거울이 실제 앱처럼 **늘** 열쇠 재접속 길을 타게 한다. 안 붙이면 서버의
        // 보드 수집기가 악수보다 먼저 도느냐에 따라 길이 갈렸다 — 앞 검사가 프로세스 표 캐시를
        // 데워 두면 수집기가 먼저 돌아 열쇠를 붙였고, 단독으로는 악수가 이겨 열쇠 없이 붙었다.
        let key = kasa_collab::surface_keys::ensure(&id);
        let backend = kasa_backend::standalone::mirror_host::MirrorHost::new(&[&id]);
        let port = crate::spawn_http_server_opts(backend, 0, false).unwrap();
        let spec = RemoteSpec {
            base: format!("http://127.0.0.1:{port}"), pane: Some(id.clone()),
            cwd: None, token: None, identity: Default::default(),
        };
        let mirror = connect_view(spec.clone(), "%viewport-passive-first").unwrap();
        assert!(set_viewport("%viewport-passive-first", 21, 6));
        let second = connect_view(spec, "%viewport-passive-second").unwrap();
        assert!(set_viewport("%viewport-passive-second", 200, 60));
        // 손대지 않은 거울은 칸 크기를 적어 두기만 한다 — 쥔 크기가 없으니 보낼 길이 없다.
        assert_eq!((held_source_size("%viewport-passive-first"), held_source_size("%viewport-passive-second")), (None, None));
        assert_eq!(source.size(), (120, 40), "viewer bounds changed the source grid");
        assert!(!source.has_viewer_size_control(), "a mirror acquired source ownership");
        assert_eq!(mirror.session.size(), (120, 40));
        assert_eq!(second.session.size(), (120, 40));
        events.send(ExtEvent::Bytes(b"\x1b[2J\x1b[40;116HEDGE!".to_vec())).unwrap();
        assert!(wait_visible(&mirror.session, "EDGE!", 5), "last row/column output was clipped");
        let snapshot = mirror.session.full_snapshot();
        let last_row = snapshot.dirty.iter().find(|(row, _)| *row == 39).unwrap();
        assert_eq!(last_row.1[119].ch.to_string(), "!");
        let click = b"\x1b[<0;120;40M";
        mirror.session.send_bytes(click).unwrap();
        wait_for(|| input.lock().unwrap().as_slice() == click, "last-cell input coordinates changed");

        source.resize(100, 30).unwrap();
        wait_for(|| mirror.session.size() == (100, 30) && second.session.size() == (100, 30),
            "viewers did not follow the source's own resize");
        assert!(set_viewport("%viewport-passive-first", 15, 4));
        drop(mirror);
        drop(second);
        wait_for(|| !is_view_pane("%viewport-passive-first") && !is_view_pane("%viewport-passive-second"),
            "viewers did not detach");
        assert_eq!(source.size(), (100, 30), "viewer detach changed source dimensions");
        assert_eq!(kasa_collab::surface_keys::get(&id).as_deref(), Some(key.as_str()), "재접속이 원본 열쇠를 바꿨다");
        let _ = events.send(ExtEvent::Eof);
        kasa_collab::surface_keys::remove(&id);
    }

    /// 확대한 동안만 원본을 키운다 — claude 가 접어 둔 줄은 호스트가 그 rows 로
    /// 그리지 않아 스크롤백에도 없으므로, 거울에서 아무리 재투영해도 안 나온다.
    #[test]
    fn zoomed_mirror_expands_source_then_restores_it() {
        fn wait_for(mut condition: impl FnMut() -> bool, message: &str) {
            let deadline = std::time::Instant::now() + Duration::from_secs(60);
            while !condition() {
                assert!(std::time::Instant::now() < deadline, "{message}");
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        let id = format!("zoom-source-{}", uuid::Uuid::new_v4());
        let (events, receiver) = crossbeam_channel::unbounded();
        let source = Arc::new(PtySession::start_external(
            PtyOptions { pane_id: id.clone(), cols: 100, rows: 30, ..Default::default() },
            ExternalIo {
                events: receiver,
                writer: Box::new(std::io::sink()),
                on_resize: Arc::new(|_, _| {}),
            },
        ).unwrap());
        kasa_pty::register_session(&id, &source);
        let backend = kasa_backend::standalone::mirror_host::MirrorHost::new(&[&id]);
        let port = crate::spawn_http_server_opts(backend, 0, false).unwrap();
        let local = format!("%zoom-mirror-{}", uuid::Uuid::new_v4());
        let mirror = connect_view(RemoteSpec {
            base: format!("http://127.0.0.1:{port}"), pane: Some(id),
            cwd: None, token: None, identity: Default::default(),
        }, &local).unwrap();
        wait_for(|| mirror.session.size() == (100, 30), "거울이 호스트 격자를 못 받았다");

        // 평상시 창 크기 기록은 원본에 닿지 않는다 — 「줄이면 저쪽도 줄어들어」의 자리.
        assert!(set_viewport(&local, 200, 60));
        assert_eq!(held_source_size(&local), None, "viewport 기록이 원본 격자를 잡으러 갔다");
        assert_eq!(source.size(), (100, 30), "viewport 기록이 원본 격자를 흔들었다");

        assert!(expand_source(&local, 200, 60, (100, 30)));
        wait_for(|| source.has_viewer_size_control(), "확대가 viewport 소유권을 못 잡았다");
        wait_for(|| source.size() == (200, 60), "확대가 원본 격자를 못 키웠다");
        // 거울도 같은 연결로 키운 격자를 받는다 — 재접속했다면 소유권이 풀려 위가 깨진다.
        wait_for(|| mirror.session.size() == (200, 60), "거울이 키운 격자를 못 받았다");
        // 맨 아래 행에 찍는다 — `visible_text(n)` 은 아래쪽 n 행만 본다.
        events.send(ExtEvent::Bytes(b"\x1b[2J\x1b[60;1HEXPANDED!".to_vec())).unwrap();
        assert!(wait_visible(&mirror.session, "EXPANDED!", 5), "확대 뒤 화면이 거울에 안 온다");

        // 확대한 채 창을 더 늘려도 되돌릴 자리는 처음 격자를 지킨다.
        assert!(expand_source(&local, 220, 70, (200, 60)));
        wait_for(|| source.size() == (220, 70), "확대 중 재확대가 안 먹었다");

        // 좁히는 요청은 보내지 않는다 — 호스트에 앉은 사람 화면만 망가진다.
        // 보내지 않았다는 답(false)이 곧 판정이다 — 보낸 것이 없으니 기다려 볼 것도 없다.
        assert!(!expand_source(&local, 40, 10, (220, 70)), "확대가 원본을 좁히려 했다");
        assert_eq!(source.size(), (220, 70), "좁히는 요청이 원본에 닿았다");

        assert!(restore_source(&local));
        wait_for(|| source.size() == (100, 30), "확대를 풀었는데 원본이 안 돌아왔다");
        assert!(!restore_source(&local), "키운 적 없는데 복원이 무언가를 보냈다");

        // 확대한 채 pane 을 닫아도 호스트는 원래 격자로 남는다.
        assert!(expand_source(&local, 180, 50, (100, 30)));
        wait_for(|| source.size() == (180, 50), "닫기 전 확대가 안 먹었다");
        drop(mirror);
        wait_for(|| source.size() == (100, 30), "확대한 채 뗐더니 호스트가 커진 채 남았다");

        let _ = events.send(ExtEvent::Eof);
    }

}
/// 앱 업데이트 창구를 실제 HTTP 로 — 같은 프로세스에 기기 서버를 둘 띄우고, 조종 쪽 러너(`kasa_socket::app_update::run`)가 앱이 다른
/// 기기에 넘길 때와 같은 클라이언트(`remote_get_json`·`remote_post_json`)로 사실·작업·상태를 주고받는다. 기기 판정은 실제 `accept`
/// (나쵸 승인은 가짜)이고, 받기·갈아 끼우기는 한 걸음씩 흉내 낸다 — 그 경로는 kasa-socket 검사가 실제 sh 도우미로 돈다.
#[cfg(all(test, unix))]
mod update_http_e2e {
    use super::*;
    use kasa_socket::app_restart::{ApprovalView, Authority, BinaryId, Facts};
    use kasa_socket::app_update::{self as update, Outcome, Reach, State};
    use kasa_socket::backend::Backend;
    use base64::Engine as _;

    const NOW_BASE: u64 = 1_790_000_000_000;
    const AP: &str = "ap_0123456789abcdef0123456789abcdef";
    const SHA: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const COMMIT: &str = "5e4d138684156c710831055f5b042688b335f617";

    struct Nacho(Mutex<ApprovalView>);

    impl Authority for Nacho {
        fn get(&self, _: &str) -> std::result::Result<ApprovalView, String> {
            Ok(self.0.lock().unwrap().clone())
        }
        fn consume(&self, _: &str, scope: &serde_json::Value, consumer: &str) -> std::result::Result<ApprovalView, String> {
            let mut v = self.0.lock().unwrap();
            if v.consumed_at_ms.is_some() {
                return Err("already_used".into());
            }
            if &v.scope != scope {
                return Err("scope_changed".into());
            }
            v.consumed_at_ms = Some(kasa_socket::board::now_ms());
            v.consumed_by = Some(consumer.into());
            v.expires_at_ms = kasa_socket::board::now_ms() + 2 * 35 * 60_000;
            Ok(v.clone())
        }
    }

    /// 기기 하나 — 사실·작업 기록·나쵸 읽기를 쥔 앱 흉내. 받은 뒤 상태를 물을 때마다 한 걸음씩 나아가고, 적용 직전에 `waits` 만큼
    /// 「바쁜 학생」으로 기다린다(다시 보내기가 올 때마다 하나씩 풀린다).
    struct Target {
        facts: Mutex<Facts>,
        dir: std::path::PathBuf,
        nacho: Arc<Nacho>,
        waits: Mutex<u32>,
        posts: Mutex<u32>,
        relay_views: Mutex<std::collections::HashMap<String, ApprovalView>>,
    }

    impl Backend for Target {
        fn list_workspaces(&self) -> Result<Vec<kasa_socket::backend::WorkspaceInfo>> { Ok(vec![]) }
        fn current_workspace(&self) -> Result<Option<kasa_socket::backend::WorkspaceInfo>> { Ok(None) }
        fn list_surfaces(&self) -> Result<Vec<kasa_socket::backend::SurfaceInfo>> { Ok(vec![]) }
        fn focus_surface(&self, _: &str) -> Result<()> { anyhow::bail!("unexpected") }
        fn split_surface(&self, _: kasa_socket::SplitDirection, _: bool, _: Option<&str>) -> Result<kasa_socket::backend::SurfaceInfo> { anyhow::bail!("unexpected") }
        fn send_text(&self, _: Option<&str>, _: &str) -> Result<()> { anyhow::bail!("unexpected") }
        fn send_key(&self, _: Option<&str>, _: &str) -> Result<()> { anyhow::bail!("unexpected") }
        fn restart_facts(&self, _: Option<&str>) -> Result<Facts> {
            let mut f = self.facts.lock().unwrap().clone();
            f.observed_at_ms = kasa_socket::board::now_ms();
            Ok(f)
        }
        fn restart_approval(&self, id: &str) -> Result<ApprovalView> {
            self.relay_views.lock().unwrap().get(id).cloned().ok_or_else(|| anyhow!("no_approval"))
        }
        fn update_start(&self, _: Option<&str>, req: &update::UpdateRequest) -> Result<serde_json::Value> {
            *self.posts.lock().unwrap() += 1;
            {
                let mut w = self.waits.lock().unwrap();
                *w = w.saturating_sub(u32::from(*self.posts.lock().unwrap() > 1));
            }
            let facts = self.restart_facts(None)?;
            let (s, created) = update::accept(req, &facts, "0.2.0", &*self.nacho, true, &self.dir, kasa_socket::board::now_ms())
                .map_err(anyhow::Error::msg)?;
            Ok(serde_json::json!({"ok": true, "state": s.state.word(), "created": created}))
        }
        fn update_job(&self, _: Option<&str>, id: &str) -> Result<update::Status> {
            let now = update::status(&self.dir, id)?;
            let next = [State::Fetched, State::Checked, State::Staged, State::Armed, State::HelperStarted, State::Exited,
                        State::Swapped, State::Launched, State::Booted, State::Done]
                .into_iter().find(|s| *s > now.state).filter(|_| !now.state.terminal());
            match next {
                Some(State::Armed) if *self.waits.lock().unwrap() > 0 => {
                    update::note(&self.dir, id, "waiting: 일하는 중이거나 사람 답을 기다리는 학생 1명")?;
                }
                Some(next) => {
                    update::advance(&self.dir, id, next, "http e2e")?;
                    if next == State::Done {
                        let mut f = self.facts.lock().unwrap();
                        f.pid += 1000;
                        f.binary.build = COMMIT[..8].into();
                    }
                }
                None => {}
            }
            update::status(&self.dir, id)
        }
    }

    fn target(mid: &str, pid: u32, nacho: &Arc<Nacho>, waits: u32) -> (Arc<Target>, String) {
        let app = "/Users/x/Applications/kasaterm.app";
        let dir = std::env::temp_dir().join(format!("kasa-update-http-{mid}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let facts = Facts {
            schema: kasa_socket::app_restart::SCHEMA.into(), machine_id: mid.into(), label: mid.into(), os: "macos".into(),
            capability: kasa_socket::app_restart::CAPABILITY, app_path: app.into(), pid, running_exe: format!("{app}/Contents/MacOS/kasaterm"),
            binary: BinaryId { inode: 1, mtime_ms: 2, build: "8933a0ca".into() }, observed_at_ms: NOW_BASE,
            update_capability: update::CAPABILITY, update_enabled: true, ..Facts::default()
        };
        let t = Arc::new(Target { facts: Mutex::new(facts), dir, nacho: nacho.clone(), waits: Mutex::new(waits), posts: Mutex::new(0),
                                  relay_views: Mutex::new(Default::default()) });
        let port = crate::spawn_http_server_opts(t.clone() as Arc<dyn Backend>, 0, false).expect("server");
        (t, format!("http://127.0.0.1:{port}"))
    }

    fn job_for(t: &Target, rid: &str) -> update::UpdateJob {
        let f = t.facts.lock().unwrap().clone();
        let tag = "v0.2.1";
        update::UpdateJob {
            schema: update::SCHEMA.into(), job_id: update::job_id(rid, &f.machine_id, SHA), plan_hash: rid.into(),
            machine_id: f.machine_id.clone(), target_hash: kasa_socket::app_restart::target_hash(&f), tag: tag.into(),
            version: "0.2.1".into(), commit: COMMIT.into(), build: COMMIT.into(),
            asset: update::Asset { name: update::asset_name(tag), url: update::asset_url(tag, &update::asset_name(tag)), size: 10,
                                   sha256: SHA.into(), ed_signature: base64::engine::general_purpose::STANDARD.encode([7u8; 64]) },
            team: "L366799VND".into(), require_notarized: true, old_pid: f.pid, created_at_ms: NOW_BASE,
        }
    }

    fn rollout(jobs: Vec<update::UpdateJob>, controller: &str) -> update::Rollout {
        let scope = update::rollout_scope(&jobs, controller);
        update::Rollout { id: jobs[0].plan_hash.clone(), release_plan: "0011223344556677".into(), created_at_ms: NOW_BASE,
                          approval_scope_hash: update::scope_hash(&scope), approval_scope: scope, jobs, excluded: vec![] }
    }

    /// 기기들과 조종 쪽이 같은 나쵸를 본다 — rollout 을 지은 뒤 그 범위로 승인을 채운다.
    fn approve(nacho: &Nacho, r: &update::Rollout) {
        *nacho.0.lock().unwrap() = ApprovalView {
            id: AP.into(), action: update::ACTION.into(), scope: r.approval_scope.clone(), scope_hash: r.approval_scope_hash.clone(),
            state: "approved".into(), expires_at_ms: kasa_socket::board::now_ms() + 600_000, consumed_at_ms: None, consumed_by: None,
        };
    }

    /// 조종 쪽이 쓰는 길 — 앱이 다른 기기에 넘길 때와 같은 HTTP 클라이언트·같은 경로.
    struct Http(std::collections::HashMap<String, String>);

    impl update::Transport for Http {
        fn facts(&self, id: &str) -> std::result::Result<Facts, String> {
            let v = remote_get_json(&self.0[id], "/app/restart/facts").map_err(|e| format!("{e:#}"))?;
            serde_json::from_value(v).map_err(|e| e.to_string())
        }
        fn start(&self, id: &str, req: &update::UpdateRequest) -> std::result::Result<serde_json::Value, Reach> {
            remote_post_json(&self.0[id], "/app/update/jobs", &serde_json::to_value(req).unwrap()).map_err(|e| update::reach_of(&format!("{e:#}")))
        }
        fn status(&self, id: &str, job_id: &str) -> std::result::Result<update::Status, String> {
            let v = remote_get_json(&self.0[id], &format!("/app/update/jobs/{job_id}")).map_err(|e| format!("{e:#}"))?;
            serde_json::from_value(v).map_err(|e| e.to_string())
        }
    }

    fn fast() -> update::RunPolicy {
        update::RunPolicy { poll_every: Duration::from_millis(5), resend_every_ms: 40, ..update::RunPolicy::default() }
    }

    fn go(r: &update::Rollout, nacho: &Nacho, http: &Http) -> Vec<(String, Outcome)> {
        update::run(r, AP, nacho, http, &fast(), None, &|_| Ok(()), &kasa_socket::board::now_ms, &std::thread::sleep)
    }

    fn words(out: &[(String, Outcome)]) -> Vec<String> {
        out.iter().map(|(_, o)| serde_json::to_value(o).unwrap()["outcome"].as_str().unwrap().to_string()).collect()
    }

    fn fleet(names: &[(&str, u32, u32)]) -> (Arc<Nacho>, Vec<Arc<Target>>, Http, update::Rollout) {
        let nacho = Arc::new(Nacho(Mutex::new(ApprovalView::default())));
        let mut targets = Vec::new();
        let mut bases = std::collections::HashMap::new();
        for (mid, pid, waits) in names {
            let (t, base) = target(mid, *pid, &nacho, *waits);
            bases.insert(mid.to_string(), base);
            targets.push(t);
        }
        let controller = names.last().unwrap().0;
        let r = rollout(targets.iter().map(|t| job_for(t, "abcdef0123456789")).collect(), controller);
        approve(&nacho, &r);
        (nacho, targets, Http(bases), r)
    }

    #[test]
    fn the_runner_updates_over_real_http_waits_out_a_busy_machine_and_hands_off_last() {
        let (nacho, t, http, r) = fleet(&[("mac-http-a", 11, 1), ("ctl-http-a", 22, 0)]);
        let out = go(&r, &nacho, &http);
        assert_eq!(words(&out), ["updated", "handed_off"], "{out:?}");
        assert_eq!(*t[0].posts.lock().unwrap(), 2, "처음 한 번 + 바쁜 학생을 기다리며 한 번");
        assert!(update::status(&t[0].dir, &r.jobs[0].job_id).unwrap().events.iter().any(|e| e.note.starts_with("waiting: ")));
        assert_eq!(nacho.0.lock().unwrap().consumed_by.as_deref(), Some("ctl-http-a"), "한 번 소비");
        assert!(matches!(&out[0].1, Outcome::Updated { new_pid: 1011, .. }));
    }

    #[test]
    fn a_revoked_approval_is_refused_by_the_machine_over_http_and_nothing_after_it_runs() {
        let (nacho, t, http, r) = fleet(&[("mac-http-b", 11, 1000), ("ctl-http-b", 22, 0)]);
        let revoker = nacho.clone();
        // 첫 기기가 기다리는 사이 주인이 거둔다 — 다음 다시 보내기를 기기가 409 로 거절한다.
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(60));
            revoker.0.lock().unwrap().state = "revoked".into();
        });
        let out = go(&r, &nacho, &http);
        assert!(matches!(&out[0].1, Outcome::Failed { reason, .. } if reason.contains("revoked")), "{out:?}");
        assert!(matches!(&out[1].1, Outcome::Skipped { .. }));
        assert_eq!(*t[1].posts.lock().unwrap(), 0, "뒤 기기에는 보내지 않았다");
    }

    #[test]
    fn an_unreachable_machine_stops_the_rollout_before_anything_is_consumed() {
        let (nacho, t, mut http, r) = fleet(&[("mac-http-c", 11, 0), ("ctl-http-c", 22, 0)]);
        let dead = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = dead.local_addr().unwrap().port();
        drop(dead);
        http.0.insert("mac-http-c".into(), format!("http://127.0.0.1:{port}"));
        let out = go(&r, &nacho, &http);
        assert!(matches!(&out[0].1, Outcome::Failed { .. }), "{out:?}");
        assert!(nacho.0.lock().unwrap().consumed_at_ms.is_none(), "닿지 않는 기기가 있으면 소비하지 않는다");
        assert_eq!(*t[1].posts.lock().unwrap(), 0);
        assert!(matches!(update::reach_of("원격 POST 요청: error sending request"), Reach::Unreachable(_)));
        assert!(matches!(update::reach_of("HTTP 409 Conflict: {\"ok\":false,\"error\":\"downgrade\"}"), Reach::Refused(_)));
    }

    /// 승인 중계는 동작마다 창구가 따로다 — 재시작 창구는 재시작 승인만, 업데이트 창구는 업데이트 승인만 답한다.
    #[test]
    fn each_relay_route_answers_only_its_own_action() {
        let (nacho, t, http, r) = fleet(&[("relay-http", 11, 0)]);
        let base = http.0["relay-http"].clone();
        let mut restart_view = nacho.0.lock().unwrap().clone();
        restart_view.action = kasa_socket::app_restart::ACTION.into();
        let upd = format!("ap_{}", "1".repeat(32));
        let rst = format!("ap_{}", "2".repeat(32));
        t[0].relay_views.lock().unwrap().insert(upd.clone(), ApprovalView { id: upd.clone(), ..nacho.0.lock().unwrap().clone() });
        t[0].relay_views.lock().unwrap().insert(rst.clone(), ApprovalView { id: rst.clone(), ..restart_view });
        assert_eq!(remote_get_json(&base, &format!("/app/update/approvals/{upd}")).unwrap()["approval"]["action"], update::ACTION);
        assert!(remote_get_json(&base, &format!("/app/update/approvals/{rst}")).unwrap_err().to_string().contains("404"));
        assert_eq!(remote_get_json(&base, &format!("/app/restart/approvals/{rst}")).unwrap()["approval"]["action"], kasa_socket::app_restart::ACTION);
        assert!(remote_get_json(&base, &format!("/app/restart/approvals/{upd}")).unwrap_err().to_string().contains("404"),
                "재시작으로 위임된 중계가 업데이트 승인을 나르지 않는다");
        // 키 없는 기기가 쓰는 실제 중계 클라이언트 — 위임 표식이 update_approvals 인 항목만 고른다.
        let me = kasa_collab::board_service::local_id().unwrap();
        let entries = vec![serde_json::json!({"machine_id": me, "restart_approvals": true})];
        assert!(kasa_socket::app_restart::target_authority_for(&update::UPDATE_RELAY, false, &entries, &me, |_| unreachable!()).is_err(),
                "재시작 위임만 있으면 update 승인은 못 읽는다");
        let entries = vec![serde_json::json!({"machine_id": me, "update_approvals": true})];
        let fetch_base = base.clone();
        let relay = kasa_socket::app_restart::target_authority_for(&update::UPDATE_RELAY, false, &entries, &me, move |_| {
            let b = fetch_base.clone();
            Ok(Box::new(move |path: &str| remote_get_json(&b, path).map_err(|e| e.to_string())))
        }).unwrap().expect("update 위임 중계");
        assert_eq!(relay.get(&upd).unwrap().action, update::ACTION);
        assert!(relay.get(&rst).is_err());
        let _ = r;
    }
}
