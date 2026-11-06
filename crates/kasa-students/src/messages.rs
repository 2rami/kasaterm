/// cwd → 모드 마커 파일명 slug. kasacollab.py `mode_path` 와 동일 규칙
/// ('/' 와 '.' 을 '-' 로): 두 구현이 같은 마커를 읽고 써야 한다.
pub fn mode_slug(cwd: &std::path::Path) -> String {
    cwd.to_string_lossy()
        .chars()
        .map(|c| if c == '/' || c == '.' { '-' } else { c })
        .collect()
}

/// 선생님(인간) 발신을 messages.jsonl 에 영속한다 — 모모톡 단톡방 가시용.
/// `read=true`: `/send` 로 이미 PTY 전달됐으니 학생 inbox drain 은 막고
/// 기록·표시만 남긴다.
/// claude TUI(Ink)에 텍스트를 *제출까지* 보내는 페이로드. 단순 `\n`(LF)은 Ink 가
/// 입력 내 개행으로 먹어 Enter 제출이 씹힌다(사용자 실측: 텍스트만 입력칸에 남음).
/// cli `tell` 과 동일하게 Ctrl-U(줄 비움) + bracketed paste + `\r`(CR=Enter):
/// handler 의 `split_trailing_submit` 가 끝 `\r` 을 떼어 140ms 후 보내(Ink 가
/// paste 처리를 끝낸 뒤) 제출이 확실히 먹는다.
pub fn submit_payload(text: &str) -> String {
    format!("\x15\x1b[200~{}\x1b[201~\r", text)
}

/// 선생님 발신을 messages.jsonl 에 append. `read=true`: 이미 PTY 로 전달돼 표시·
/// 오케스트레이터 가시용만(학생 inbox drain 막음). `read=false`: 모모톡 inbox 발신 — 받는
/// 에이전트의 drain_unread(to==me·read==false)가 집어 올려 컨텍스트로 받는다.
/// to/to_pane 은 surface(%N) — drain_unread 가 pane id 도 내 주소로 매칭한다.
pub fn persist_sensei_msg(room_cwd: &std::path::Path, surface: &str, text: &str, read: bool, room: Option<&str>) {
    // 활성 방 디렉터리에 직접 기록(없으면 생성) — 읽기와 달리 존재 여부로 안 거른다.
    // 방별 분리(사용자): room 있으면 slug 에 `__room_<id>` — 모모톡 inbox 도 방별 격리.
    let slug = match room {
        Some(r) => format!("{}__room_{}", mode_slug(room_cwd), r),
        None => mode_slug(room_cwd),
    };
    let dir = kasa_socket::collab_root().join(slug);
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0);
    let id = format!("{:08x}", (now * 1000.0) as u64 & 0xffff_ffff);
    let line = serde_json::json!({
        "id": id, "from": "sensei", "from_pane": "sensei",
        "to": surface, "to_pane": surface,
        "text": text, "ts": now, "read": read
    })
    .to_string();
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("messages.jsonl"))
    {
        use std::io::Write;
        let _ = writeln!(f, "{line}");
    }
}
