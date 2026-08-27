//! /resume 가시성 스위퍼 — claude 자체 /resume 피커는 transcript head 64KB 를
//! 텍스트 스캔해 `"teamName":"…"` 이 보이면 그 세션을 무조건 숨긴다(v2.1.212
//! 바이너리 실측, 설정·env 우회 없음). kasaterm 은 모든 pane claude 를 팀
//! 트리플로 띄우므로 pane 세션 전부가 /resume 에서 사라진다(거노 실사고).
//!
//! 해법 2축:
//! 1. **숨김 해제** — transcript 의 `"teamName":` 키를 **같은 바이트 길이**의
//!    `"ktTeamNm":` 으로 바꿔치기(라인은 유효 JSON 유지, 파일 크기·구조 불변).
//!    teamName 은 세션당 team_context attachment 라인 1곳뿐임을 실측했고,
//!    스캐너가 읽는 창(head/tail 64KB)만 패치하면 충분. 팀 종류 불문 전부
//!    되살린다(거노: "팀원이든 뭐든 다 뜨게") — 라이브 세션도 패치한다.
//!    라이브 재개 중복은 upstream 의 bg 가드 + kasaterm-cli [실행중] 마커가 막고,
//!    append 전용 파일이라 라이브 중 창 되쓰기도 안전.
//! 2. **학생 표시** — 세션→학생 바인딩(session_characters.json)이 있으면
//!    `{"type":"tag","tag":"<학생>"}` 라인을 append. /resume 행 설명줄이 tag 를
//!    `#학생` 으로 렌더하고(zzt 실측) 검색어로도 잡힌다. 제목(aiTitle 갱신)은
//!    안 건드리는 비파괴 채널. 학생 이름이 아닌 기존 태그(사용자 지정)는 존중.
//!
//! 공통: 패치·append 후 mtime 원복 — /resume 은 mtime 정렬이라 안 지키면 옛
//! 세션이 맨 위로 튄다(실측). 재개된 세션이 새 attachment 를 또 붙여도 주기
//! 스윕(60초)이 재처리하므로 루프는 안정.

use std::collections::{HashMap, HashSet};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// 이미 훑어 본 파일 — `경로 → (mtime, 크기, 그때의 학생 바인딩)`. 셋이 그대로면
/// 이번 바퀴의 결과도 같을 수밖에 없으니 파일을 아예 열지 않는다.
///
/// 전수 스윕은 60초마다 도는데, 이 기기의 `~/.claude/projects` 는 jsonl 3,427개
/// 1.5GB 다 — 파일마다 head+tail 64KB 를 읽어 바이트 검색하면 한 바퀴에 수백 MB
/// 를 훑고 코어 하나를 몇 초씩 문다(실측: 3초 sample 내내 이 스레드 100%).
/// 게다가 세션이 쌓일수록 무거워진다. 옛 transcript 는 두 번 다시 바뀌지 않으니
/// 첫 바퀴 뒤로는 claude 가 실제로 쓴 몇 개만 남는다.
///
/// 바인딩까지 키에 넣는 건 `stamp_tag` 때문이다 — 파일이 그대로여도 나중에 학생
/// 배정이 생기면 태그를 새로 찍어야 하는데, mtime 만 보면 영영 건너뛴다.
type SeenMap = HashMap<PathBuf, (SystemTime, u64, Option<String>)>;
static SEEN: std::sync::LazyLock<std::sync::Mutex<SeenMap>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(HashMap::new()));

fn file_stamp(path: &Path) -> Option<(SystemTime, u64)> {
    let m = std::fs::metadata(path).ok()?;
    Some((m.modified().ok()?, m.len()))
}

/// claude /resume 스캐너의 head/tail 읽기 창과 동일(바이너리 uD=65536 실측).
const WINDOW: u64 = 65536;

/// (원본, 치환) — 반드시 같은 바이트 길이(in-place 패치 전제). 공백 변형은
/// 스캐너(yY)가 `"key": "` 도 허용해서 함께 커버.
const NEEDLES: [(&[u8], &[u8]); 2] = [
    (b"\"teamName\":\"", b"\"ktTeamNm\":\""),
    (b"\"teamName\": \"", b"\"ktTeamNm\": \""),
];

/// 서버 상주 루프 — 부팅 직후 1회 + 60초 주기. run_scheduler 게이트 뒤에서만
/// 돌린다(standalone 은 공유 transcript 를 건드리면 안 됨, schedule_loop 동일
/// 철학).
pub async fn sweep_loop() {
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    loop {
        let n = tokio::task::spawn_blocking(sweep_all_projects).await.unwrap_or(0);
        if n > 0 {
            eprintln!("[resume-visibility] {n}개 세션 transcript 갱신(/resume 노출·학생 태그)");
        }
        tokio::time::sleep(std::time::Duration::from_secs(60)).await;
    }
}

/// `~/.claude/projects/*/<uuid>.jsonl` 전수 스윕. 반환 = 변경 파일 수.
pub fn sweep_all_projects() -> usize {
    let home = std::env::var("HOME").unwrap_or_default();
    if home.is_empty() {
        return 0;
    }
    let root = Path::new(&home).join(".claude/projects");
    let bindings = load_bindings(&Path::new(&home).join(".config/kasaterm/session_characters.json"));
    let students = student_names();
    sweep_projects_root(&root, &bindings, &students)
}

fn sweep_projects_root(
    root: &Path,
    bindings: &HashMap<String, String>,
    students: &HashSet<String>,
) -> usize {
    let Ok(projects) = std::fs::read_dir(root) else { return 0 };
    let mut touched = 0;
    for proj in projects.flatten() {
        let dir = proj.path();
        if !dir.is_dir() {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for e in entries.flatten() {
            let path = e.path();
            if !is_session_jsonl(&path) {
                continue;
            }
            let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
            let binding = bindings.get(stem).cloned();
            let before = file_stamp(&path);
            // 지난 바퀴와 완전히 같은 상태면 열지 않는다(위 SEEN 주석).
            if let Some((mtime, len)) = before {
                let same = SEEN
                    .lock()
                    .ok()
                    .and_then(|g| g.get(&path).cloned())
                    .is_some_and(|(t, l, b)| t == mtime && l == len && b == binding);
                if same {
                    continue;
                }
            }
            let mut changed = patch_file(&path).unwrap_or(false);
            if let Some(student) = &binding {
                // `/rename` 으로 붙인 이름만 온다(자동 제목은 안 섞인다) — 없으면
                // None 이고 태그는 종전대로 학생 이름 하나다.
                let rename = kasa_socket::sessions::session_rename_for(&path);
                changed |=
                    stamp_tag(&path, stem, student, students, rename.as_deref()).unwrap_or(false);
            }
            // 처리 뒤 상태로 기록한다 — stamp_tag 는 파일을 늘릴 수 있다.
            if let (Some(stamp), Ok(mut g)) = (file_stamp(&path), SEEN.lock()) {
                g.insert(path.clone(), (stamp.0, stamp.1, binding));
            }
            if changed {
                touched += 1;
            }
        }
    }
    touched
}

/// uuid 스템의 .jsonl 만 — agent-*.jsonl(서브에이전트)·orphaned 파일 제외.
fn is_session_jsonl(path: &Path) -> bool {
    if path.extension().and_then(|x| x.to_str()) != Some("jsonl") {
        return false;
    }
    path.file_stem()
        .and_then(|s| s.to_str())
        .is_some_and(kasa_socket::sessions::is_uuid)
}

/// head/tail 창에서 needle 을 같은 길이로 치환. 변경 시 mtime 원복. 반환 =
/// 실제 바뀜 여부. append 전용 파일이라 창 되쓰기는 새 append 와 겹치지 않는다.
pub(crate) fn patch_file(path: &Path) -> std::io::Result<bool> {
    let meta = std::fs::metadata(path)?;
    let len = meta.len();
    if len == 0 {
        return Ok(false);
    }
    let mut f = std::fs::OpenOptions::new().read(true).write(true).open(path)?;
    let mut changed = false;
    let head_len = len.min(WINDOW) as usize;
    changed |= patch_window(&mut f, 0, head_len)?;
    if len > WINDOW {
        changed |= patch_window(&mut f, len - WINDOW, WINDOW as usize)?;
    }
    drop(f);
    if changed {
        restore_times(path, &meta);
    }
    Ok(changed)
}

fn patch_window(f: &mut std::fs::File, offset: u64, len: usize) -> std::io::Result<bool> {
    f.seek(SeekFrom::Start(offset))?;
    let mut buf = vec![0u8; len];
    f.read_exact(&mut buf)?;
    let mut changed = false;
    for (needle, repl) in NEEDLES {
        debug_assert_eq!(needle.len(), repl.len());
        let mut at = 0;
        while let Some(i) = find_bytes(&buf[at..], needle) {
            let start = at + i;
            buf[start..start + repl.len()].copy_from_slice(repl);
            at = start + repl.len();
            changed = true;
        }
    }
    if changed {
        f.seek(SeekFrom::Start(offset))?;
        f.write_all(&buf)?;
    }
    Ok(changed)
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// `#학생` 태그 스탬프 — /resume 행 설명줄에 학생 이름을 띄우는 비파괴 채널.
/// claude 의 태그 라인 스키마(`{"type":"tag","tag":…,"sessionId":…}`, bId 실측)
/// 그대로 append. 이미 원하는 태그면 무쓰기, 학생 이름이 아닌 기존 태그
/// (사용자가 직접 단 것)는 절대 안 덮는다. 스캐너가 tail 창만 보므로 현재
/// 태그 판독도 tail 창으로 충분(파일이 자라 창 밖으로 밀리면 재스탬프).
pub(crate) fn stamp_tag(
    path: &Path,
    sid: &str,
    student: &str,
    students: &HashSet<String>,
    rename: Option<&str>,
) -> std::io::Result<bool> {
    let meta = std::fs::metadata(path)?;
    let len = meta.len();
    let want = tag_value(student, rename);
    match current_tag(path, len)? {
        Some(cur) if cur == want => return Ok(false),
        // 학생 이름이 아닌 태그 = 사용자 지정 — 존중.
        //
        // ⚠️ **첫 토막**으로 본다. 태그가 `우사기 #account-theme` 처럼 두 낱말이
        // 되면서, 값 전체로 대조하면 우리가 방금 찍은 것도 「사용자 지정」으로
        // 읽혀 그 세션은 영영 갱신이 멈춘다.
        Some(cur) if !students.contains(cur.split(' ').next().unwrap_or_default()) => {
            return Ok(false)
        }
        _ => {}
    }
    let line = serde_json::json!({ "type": "tag", "tag": want, "sessionId": sid });
    let mut f = std::fs::OpenOptions::new().append(true).open(path)?;
    f.write_all(format!("{line}\n").as_bytes())?;
    drop(f);
    restore_times(path, &meta);
    Ok(true)
}

/// `/resume` 행 설명줄에 세울 태그 값 — `<학생>` 또는 `<학생> #<세션이름>`.
///
/// claude 는 이 값을 `#<값>` 으로 렌더하므로, 두 번째 `#` 을 **값 안에** 넣어야
/// `#우사기 #account-theme` 처럼 둘 다 태그로 선다(실측 2026-08-27). 태그 라인을
/// 두 개 append 하는 길은 안 된다 — 마지막 것만 그려져 **학생 이름이 사라진다**.
///
/// 세션 이름의 공백은 하이픈으로 바꾼다. 그대로 두면 `#account theme` 이
/// `#account` 와 `theme` 로 갈려 보이고(같은 실측), 화면의 발신자 라벨도 이미
/// 같은 규칙으로 하이픈이라 두 화면이 같은 이름을 말하게 된다.
fn tag_value(student: &str, rename: Option<&str>) -> String {
    let Some(name) = rename.map(str::trim).filter(|s| !s.is_empty()) else {
        return student.to_string();
    };
    let slug = name.replace(' ', "-");
    // 이름을 학생 이름 그대로 붙여 둔 창은 같은 말을 두 번 하지 않는다.
    if slug == student || slug.is_empty() {
        return student.to_string();
    }
    format!("{student} #{slug}")
}

/// tail 창의 마지막 `"type":"tag"` 라인에서 tag 값 — /resume 스캐너와 같은 창.
fn current_tag(path: &Path, len: u64) -> std::io::Result<Option<String>> {
    let mut f = std::fs::File::open(path)?;
    let start = len.saturating_sub(WINDOW);
    f.seek(SeekFrom::Start(start))?;
    let mut buf = Vec::with_capacity((len - start) as usize);
    f.read_to_end(&mut buf)?;
    let text = String::from_utf8_lossy(&buf);
    for line in text.lines().rev() {
        if !line.contains("\"type\":\"tag\"") {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        if v.get("type").and_then(|t| t.as_str()) == Some("tag") {
            return Ok(v.get("tag").and_then(|t| t.as_str()).map(String::from));
        }
    }
    Ok(None)
}

/// 파일 갱신 후 atime/mtime 원복 — /resume 은 mtime 정렬이라 보존 필수. 실패는
/// 무해(정렬만 흔들림)라 무시. windows 는 미지원(정렬 오차 감수).
#[cfg(unix)]
fn restore_times(path: &Path, meta: &std::fs::Metadata) {
    use std::os::unix::ffi::OsStrExt;
    fn tv(t: std::io::Result<std::time::SystemTime>) -> libc::timeval {
        let d = t
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .unwrap_or_default();
        libc::timeval {
            tv_sec: d.as_secs() as libc::time_t,
            tv_usec: d.subsec_micros() as libc::suseconds_t,
        }
    }
    let times = [tv(meta.accessed()), tv(meta.modified())];
    let Ok(cpath) = std::ffi::CString::new(path.as_os_str().as_bytes()) else { return };
    unsafe {
        libc::utimes(cpath.as_ptr(), times.as_ptr());
    }
}

#[cfg(not(unix))]
fn restore_times(_path: &Path, _meta: &std::fs::Metadata) {}

/// `{sid: 학생명}` 평면 JSON(session_characters.json). 없거나 깨지면 빈 맵.
fn load_bindings(path: &Path) -> HashMap<String, String> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| v.as_object().cloned())
        .map(|m| {
            m.into_iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k, s.to_string())))
                .collect()
        })
        .unwrap_or_default()
}

/// 로스터 전체 학생 이름 — characters.json(leader/leaders/members). 태그가
/// 학생 이름인지(=우리가 단 것) 판정에 쓴다.
/// 우리가 배정에 쓰는 모든 캐릭터 이름 — **활성 테마가 아니라 설치된 테마 전부.**
///
/// `stamp_tag` 가 「이 태그는 우리가 찍은 것인가, 사용자가 손수 붙인 것인가」를 이
/// 집합으로 가른다. 활성 명단만 담으면 테마를 갈아탄 순간 옛 캐릭터 이름이 집합
/// 밖으로 나가고, 그 태그가 사용자 지정으로 오인돼 **영영 갱신되지 않는다**
/// (2026-08-27 실측: 배정이 아리스로 바뀐 세션의 /resume 행에 옛 테마 페이몬 얼굴이
/// 붙은 채 굳어 있었다 — 태그는 `페이몬`, 바인딩은 `아리스`).
fn student_names() -> HashSet<String> {
    names_from_rosters(&crate::character::all_rosters())
}

/// 로스터 여럿 → 이름 하나의 집합. **합집합인 것이 요점**이라 순수 함수로 떼어
/// 테스트가 지킨다.
fn names_from_rosters(rosters: &[serde_json::Value]) -> HashSet<String> {
    let mut out = HashSet::new();
    for c in rosters {
        if let Some(n) = c.get("leader").and_then(|l| l.get("name")).and_then(|n| n.as_str()) {
            out.insert(n.to_string());
        }
        for key in ["leaders", "members"] {
            if let Some(arr) = c.get(key).and_then(|a| a.as_array()) {
                for m in arr {
                    if let Some(n) = m.get("name").and_then(|n| n.as_str()) {
                        out.insert(n.to_string());
                    }
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn tmpdir(tag: &str) -> PathBuf {
        let n = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let d = std::env::temp_dir().join(format!("kasaterm-rsmv-{tag}-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    const SID: &str = "11111111-2222-3333-4444-555555555555";

    fn team_line(team: &str) -> String {
        format!(
            "{{\"type\":\"attachment\",\"attachment\":{{\"type\":\"team_context\",\"teamName\":\"{team}\"}}}}\n"
        )
    }

    fn students() -> HashSet<String> {
        ["시로코", "프라나"].into_iter().map(String::from).collect()
    }

    #[test]
    fn head_needle_patched_and_json_stays_valid() {
        let d = tmpdir("head");
        let p = d.join(format!("{SID}.jsonl"));
        std::fs::write(&p, team_line("kt-room-abcd")).unwrap();
        assert!(patch_file(&p).unwrap());
        let body = std::fs::read_to_string(&p).unwrap();
        assert!(!body.contains("\"teamName\""));
        assert!(body.contains("\"ktTeamNm\":\"kt-room-abcd\""));
        // 라인은 여전히 유효 JSON(같은 길이 키 치환).
        serde_json::from_str::<serde_json::Value>(body.trim()).unwrap();
        // 재실행은 무변경(멱등).
        assert!(!patch_file(&p).unwrap());
    }

    #[test]
    fn any_team_prefix_patched() {
        // 거노: "팀원이든 뭐든 다 뜨게" — kt- 외 팀(TeamCreate·수동)도 해제.
        let d = tmpdir("anyteam");
        let p = d.join(format!("{SID}.jsonl"));
        std::fs::write(&p, team_line("session-12345678")).unwrap();
        assert!(patch_file(&p).unwrap());
        assert!(!std::fs::read_to_string(&p).unwrap().contains("\"teamName\""));
    }

    #[test]
    fn tail_needle_beyond_head_window_patched() {
        let d = tmpdir("tail");
        let p = d.join(format!("{SID}.jsonl"));
        // head 창(64KB) 밖으로 밀어낸 뒤 tail 에 needle — resume 재부팅이 붙인
        // 새 attachment 시나리오.
        let mut body = "x".repeat((WINDOW as usize) + 1024);
        body.push('\n');
        body.push_str(&team_line("kt-room-abcd"));
        std::fs::write(&p, &body).unwrap();
        assert!(patch_file(&p).unwrap());
        let out = std::fs::read_to_string(&p).unwrap();
        assert!(!out.contains("\"teamName\""));
    }

    #[test]
    fn tag_stamped_once_and_respects_user_tag() {
        let d = tmpdir("tag");
        let p = d.join(format!("{SID}.jsonl"));
        std::fs::write(&p, team_line("kt-room-abcd")).unwrap();
        // 첫 스탬프 → tag 라인 append.
        assert!(stamp_tag(&p, SID, "시로코", &students(), None).unwrap());
        let body = std::fs::read_to_string(&p).unwrap();
        assert!(body.contains("\"type\":\"tag\""));
        assert!(body.contains("\"tag\":\"시로코\""));
        // 같은 값 재실행 = 무쓰기(멱등).
        assert!(!stamp_tag(&p, SID, "시로코", &students(), None).unwrap());
        // 바인딩이 바뀌면(학생 이름 태그) 갱신 append — 마지막 태그가 이긴다.
        assert!(stamp_tag(&p, SID, "프라나", &students(), None).unwrap());
        assert_eq!(
            current_tag(&p, std::fs::metadata(&p).unwrap().len()).unwrap().as_deref(),
            Some("프라나")
        );
        // 사용자 지정 태그(학생 이름 아님)는 안 덮는다.
        let user = serde_json::json!({"type":"tag","tag":"내태그","sessionId":SID});
        let mut f = std::fs::OpenOptions::new().append(true).open(&p).unwrap();
        f.write_all(format!("{user}\n").as_bytes()).unwrap();
        drop(f);
        assert!(!stamp_tag(&p, SID, "시로코", &students(), None).unwrap());
    }

    /// `/rename` 한 창은 그 이름도 태그에 실린다 — 요약 제목만으로는 어느 창인지
    /// 모르는 것이 이 기능의 요점이다(2026-08-27 지시).
    #[test]
    fn rename_rides_along_in_the_same_tag() {
        let d = tmpdir("tag-rename");
        let p = d.join(format!("{SID}.jsonl"));
        std::fs::write(&p, team_line("kt-room-abcd")).unwrap();
        assert!(stamp_tag(&p, SID, "시로코", &students(), Some("account theme")).unwrap());
        assert_eq!(
            current_tag(&p, std::fs::metadata(&p).unwrap().len()).unwrap().as_deref(),
            Some("시로코 #account-theme"),
            "공백은 하이픈으로 — 안 그러면 화면에서 두 낱말로 갈린다"
        );
        // 우리가 찍은 두 낱말 태그를 「사용자 지정」으로 오해하면 그 세션은 영영
        // 갱신이 멈춘다 — 첫 토막으로 가르는지 본다.
        assert!(!stamp_tag(&p, SID, "시로코", &students(), Some("account theme")).unwrap());
        assert!(stamp_tag(&p, SID, "시로코", &students(), Some("딴이름")).unwrap());
        assert_eq!(
            current_tag(&p, std::fs::metadata(&p).unwrap().len()).unwrap().as_deref(),
            Some("시로코 #딴이름")
        );
        // 사용자 지정 태그는 두 낱말 규칙에서도 존중된다.
        let user = serde_json::json!({"type":"tag","tag":"내태그 #뭐시기","sessionId":SID});
        let mut f = std::fs::OpenOptions::new().append(true).open(&p).unwrap();
        f.write_all(format!("{user}\n").as_bytes()).unwrap();
        drop(f);
        assert!(!stamp_tag(&p, SID, "시로코", &students(), Some("account theme")).unwrap());
    }

    #[test]
    fn tag_value_shapes() {
        assert_eq!(tag_value("우사기", None), "우사기");
        assert_eq!(tag_value("우사기", Some("  ")), "우사기");
        // 이름을 학생 이름 그대로 붙인 창은 같은 말을 두 번 하지 않는다.
        assert_eq!(tag_value("우사기", Some("우사기")), "우사기");
        assert_eq!(tag_value("모모이", Some("sidebar")), "모모이 #sidebar");
        assert_eq!(
            tag_value("케이", Some("터미널 미러링 데몬 pty")),
            "케이 #터미널-미러링-데몬-pty"
        );
    }

    #[test]
    fn sweep_patches_and_tags_via_bindings() {
        let d = tmpdir("sweep");
        let proj = d.join("-Users-x-proj");
        std::fs::create_dir_all(&proj).unwrap();
        let bound = proj.join(format!("{SID}.jsonl"));
        std::fs::write(&bound, team_line("kt-room-abcd")).unwrap();
        let plain_sid = "99999999-2222-3333-4444-555555555555";
        let plain = proj.join(format!("{plain_sid}.jsonl"));
        std::fs::write(&plain, "{\"type\":\"user\"}\n").unwrap();
        let agent = proj.join("agent-abc123.jsonl");
        std::fs::write(&agent, team_line("kt-room-abcd")).unwrap();
        let bindings: HashMap<String, String> = [(SID.to_string(), "시로코".to_string())].into();
        // 바인딩 세션 = 패치+태그(1건 변경), 무팀·무바인딩 세션 = 무변경,
        // agent-*.jsonl = 제외.
        assert_eq!(sweep_projects_root(&d, &bindings, &students()), 1);
        let body = std::fs::read_to_string(&bound).unwrap();
        assert!(!body.contains("\"teamName\""));
        assert!(body.contains("\"tag\":\"시로코\""));
        assert!(!std::fs::read_to_string(&plain).unwrap().contains("tag"));
        assert!(std::fs::read_to_string(&agent).unwrap().contains("\"teamName\""));
    }

    /// 전수 스윕은 60초마다 도는데 파일이 수천 개다 — 안 바뀐 파일은 아예 열지
    /// 않아야 한다. 다만 "안 열기"가 지나치면 **나중에 생긴 학생 바인딩**을 영영
    /// 못 찍으므로(파일은 그대로니까), 그 한 경우는 다시 봐야 한다.
    #[test]
    fn sweep_skips_unchanged_files_but_still_sees_a_new_binding() {
        let d = tmpdir("skip");
        let proj = d.join("-Users-x-proj");
        std::fs::create_dir_all(&proj).unwrap();
        let path = proj.join(format!("{SID}.jsonl"));
        std::fs::write(&path, team_line("kt-room-abcd")).unwrap();

        let none: HashMap<String, String> = HashMap::new();
        // 첫 바퀴 = needle 패치.
        assert_eq!(sweep_projects_root(&d, &none, &students()), 1);
        // 둘째 바퀴 = 파일도 바인딩도 그대로 → 건너뛴다.
        assert_eq!(sweep_projects_root(&d, &none, &students()), 0);
        // 바인딩이 새로 생기면 같은 파일이라도 다시 봐서 태그를 찍는다.
        let bound: HashMap<String, String> = [(SID.to_string(), "프라나".to_string())].into();
        assert_eq!(sweep_projects_root(&d, &bound, &students()), 1);
        assert!(std::fs::read_to_string(&path).unwrap().contains("\"tag\":\"프라나\""));
    }

    #[cfg(unix)]
    #[test]
    fn mtime_preserved_after_patch_and_tag() {
        let d = tmpdir("mtime");
        let p = d.join(format!("{SID}.jsonl"));
        std::fs::write(&p, team_line("kt-room-abcd")).unwrap();
        // 과거 mtime 을 심고 패치+태그 후 그대로인지 — /resume 정렬 오염 방지 핵심.
        let old = libc::timeval { tv_sec: 1_600_000_000, tv_usec: 0 };
        let times = [old, old];
        let c = std::ffi::CString::new(p.to_str().unwrap()).unwrap();
        unsafe { libc::utimes(c.as_ptr(), times.as_ptr()) };
        assert!(patch_file(&p).unwrap());
        assert!(stamp_tag(&p, SID, "시로코", &students(), None).unwrap());
        let m = std::fs::metadata(&p).unwrap().modified().unwrap();
        let secs = m.duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
        assert_eq!(secs, 1_600_000_000);
    }

    /// 활성 테마 하나만 담으면, 테마를 갈아탄 뒤 옛 캐릭터 태그가 「사용자 지정」으로
    /// 오인돼 영영 안 갈린다(2026-08-27 실측). 합집합인 것이 그 방어다.
    #[test]
    fn names_come_from_every_roster_not_just_the_active_one() {
        let blue = serde_json::json!({ "leader": { "name": "아리스" }, "members": [{ "name": "시로코" }] });
        let genshin = serde_json::json!({ "members": [{ "name": "페이몬" }, { "name": "호두" }] });
        let names = names_from_rosters(&[blue.clone(), genshin.clone()]);
        for n in ["아리스", "시로코", "페이몬", "호두"] {
            assert!(names.contains(n), "{n} 이 빠졌다 — 합집합이 아니다");
        }
        // 활성 명단만 보던 옛 동작이면 이쪽이 비어 페이몬을 남의 태그로 오인한다.
        assert!(!names_from_rosters(&[blue]).contains("페이몬"));
    }

}
