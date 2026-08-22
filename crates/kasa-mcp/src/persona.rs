//! 우측 상주 페르소나 — 전신 원화 한 장과, 값싼 모델로 도는 말상대.
//!
//! 학생(pane)과 다른 물건이다. 학생은 claude 프로세스라 pane 을 하나 먹고 일을
//! 하지만, 이쪽은 **일을 하지 않는다** — board 를 읽고 지금 무슨 일이 돌아가는지
//! 말해 주고 잡담을 받는 자리다. 그래서 pane 을 띄우지 않고 앱이 Messages API 를
//! 직접 부른다: 자리를 안 뺏고, 대화 상태를 앱이 쥐고, 값싼 모델이라 늘 켜 둬도
//! 부담이 없다.

use kasa_socket::backend::PaneActivity;
use std::path::PathBuf;

fn home() -> Option<PathBuf> {
    std::env::var("HOME").ok().map(PathBuf::from)
}

/// 이름(「아로나」) → slug(`arona`). 그림 파일이 slug 로 놓여 있어서 필요하다.
pub fn slug_for(name: &str) -> Option<String> {
    let chars = crate::character::characters_json()?;
    let want = name.trim();
    for key in ["leaders", "members"] {
        for m in chars.get(key)?.as_array().into_iter().flatten() {
            if m.get("name").and_then(|x| x.as_str()) == Some(want) {
                return m.get("slug").and_then(|x| x.as_str()).map(|s| s.to_string());
            }
        }
    }
    None
}

/// 그 캐릭터의 정체성 문구. `character::persona_for` 와 달리 협업 규약을 붙이지
/// 않는다 — 그건 일하는 pane 을 위한 규칙이고, 말상대에겐 「커밋은 네가 책임진다」
/// 같은 조항이 오히려 거짓말을 시킨다.
pub fn identity_for(name: &str) -> Option<String> {
    let chars = crate::character::characters_json()?;
    let want = name.trim();
    for key in ["leaders", "members"] {
        for m in chars.get(key)?.as_array().into_iter().flatten() {
            if m.get("name").and_then(|x| x.as_str()) == Some(want) {
                return m
                    .get("persona")
                    .and_then(|x| x.as_str())
                    .filter(|p| !p.is_empty())
                    .map(|p| p.to_string());
            }
        }
    }
    None
}

/// 패널에 세울 전신 그림. 도트 스프라이트가 아니라 위키 원본이라 세로로 긴
/// 패널에서 사람 크기로 선다.
///
/// 원화는 저작권 때문에 레포에 커밋하지 않는다(`.gitignore: theme-src/*/ref.png`).
/// 그래서 배포본에는 아예 없고 — 못 찾았을 때 404 를 주면 남의 머신에서 패널이
/// 통째로 빈칸이 된다. 사용자가 자기 그림을 놓을 자리를 먼저 보고, 없으면 개발
/// 트리를 보고, 그래도 없으면 호출부가 스프라이트로 떨어질 수 있게 None 을 준다.
pub fn portrait(slug: &str) -> Option<(Vec<u8>, &'static str)> {
    if slug.is_empty() || slug.contains(['/', '\\', '.']) {
        return None;
    }
    let mut cands: Vec<PathBuf> = Vec::new();
    if let Ok(d) = std::env::var("KASATERM_PORTRAIT_DIR") {
        cands.push(PathBuf::from(&d).join(format!("{slug}.png")));
    }
    if let Some(h) = home() {
        cands.push(h.join(format!(".config/kasaterm/portraits/{slug}.png")));
    }
    // 개발 트리 — 이 crate 에서 레포 루트로 두 단계 올라간다.
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .map(|p| p.to_path_buf());
    if let Some(r) = repo {
        cands.push(r.join(format!("theme-src/{slug}/ref.png")));
    }
    for p in cands {
        if let Ok(b) = std::fs::read(&p) {
            if !b.is_empty() {
                return Some((b, "image/png"));
            }
        }
    }
    None
}

/// board 한 판을 모델이 읽을 몇 줄로 줄인다.
///
/// 원본 row 는 토큰 사용량·도구 히스토리까지 들고 있어 그대로 넣으면 값싼 모델의
/// 200k 창을 대화 몇 번에 태운다. 남기는 것은 「누가·어디서·무엇을·방금 무슨 말을
/// 주고받았나」뿐이다.
pub fn board_digest(board: &[PaneActivity]) -> String {
    if board.is_empty() {
        return "지금 돌고 있는 pane 이 없다.".to_string();
    }
    let mut out = String::new();
    for r in board {
        let who = r.character.as_deref().unwrap_or("이름 없음");
        let proj = r.cwd.rsplit('/').next().unwrap_or("");
        let title = if r.title.is_empty() { "제목 없음" } else { &r.title };
        out.push_str(&format!(
            "{} {} · {} · {} · {}\n",
            r.surface_id, who, title, r.status, proj
        ));
        if !r.last_prompt.is_empty() {
            out.push_str(&format!("   들은 말: {}\n", clip(&r.last_prompt, 200)));
        }
        if !r.last_reply.is_empty() {
            out.push_str(&format!("   한 말: {}\n", clip(&r.last_reply, 260)));
        }
        if !r.intent.is_empty() && r.status != "idle" {
            out.push_str(&format!("   지금: {}\n", clip(&r.intent, 90)));
        }
    }
    out
}

/// 문자 경계로 자른다 — 한글은 바이트 슬라이스가 패닉을 낸다.
fn clip(s: &str, n: usize) -> String {
    let t = s.trim().replace('\n', " ");
    if t.chars().count() <= n {
        return t;
    }
    t.chars().take(n).collect::<String>() + "…"
}

/// OpenGateway(Anthropic 호환) 키. kasa-ai 가 쓰는 자리와 같은 파일을 본다.
fn api_key() -> Option<String> {
    if let Ok(k) = std::env::var("KASATERM_PERSONA_KEY") {
        if !k.trim().is_empty() {
            return Some(k.trim().to_string());
        }
    }
    let p = home()?.join(".config/opengateway.key");
    let k = std::fs::read_to_string(p).ok()?;
    let k = k.trim().to_string();
    if k.is_empty() { None } else { Some(k) }
}

fn base_url() -> String {
    std::env::var("KASATERM_PERSONA_BASE").unwrap_or_else(|_| "https://apis.opengateway.ai".into())
}

fn model() -> String {
    std::env::var("KASATERM_PERSONA_MODEL").unwrap_or_else(|_| "z-ai/glm-5.2-ultrafast".into())
}

#[derive(serde::Deserialize, Clone)]
pub struct Turn {
    pub role: String,
    pub content: String,
}

#[derive(serde::Deserialize)]
pub struct ChatReq {
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub history: Vec<Turn>,
    /// 마스코트 이름. 비면 env → 아로나 순으로 떨어진다.
    #[serde(default)]
    pub character: String,
    /// 사용자가 물어서 하는 말인지, 상태가 바뀌어 스스로 여는 말인지. 후자는
    /// 프롬프트가 달라야 한다 — 안 그러면 아무도 안 물었는데 「무엇을 도와드릴까요」
    /// 로 시작한다.
    #[serde(default)]
    pub unprompted: bool,
}

/// 고른 마스코트가 적히는 자리. 「한 명 고정」이라 pane 이나 세션이 아니라 앱 하나에
/// 하나뿐이고, 그래서 세션 파일이 아닌 설정 옆에 둔다.
fn choice_path() -> Option<PathBuf> {
    Some(home()?.join(".config/kasaterm/persona.json"))
}

pub fn character_name(req_name: &str) -> String {
    if !req_name.trim().is_empty() {
        return req_name.trim().to_string();
    }
    if let Some(n) = choice_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .and_then(|v| v.get("character").and_then(|c| c.as_str()).map(|s| s.to_string()))
        .filter(|s| !s.trim().is_empty())
    {
        return n;
    }
    std::env::var("KASATERM_PERSONA_CHARACTER")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "아로나".into())
}

/// 마스코트를 바꾼다. 로스터에 없는 이름은 거절한다 — 오타 하나로 그림도 말투도
/// 없는 유령이 서면 화면만 비고 원인이 안 보인다.
pub fn set_character(name: &str) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("이름이 비어 있어요.".into());
    }
    let slug = slug_for(name).ok_or_else(|| format!("{name} 은 로스터에 없어요."))?;
    let p = choice_path().ok_or_else(|| "설정 폴더를 못 찾았어요.".to_string())?;
    if let Some(d) = p.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    std::fs::write(&p, serde_json::json!({ "character": name }).to_string())
        .map_err(|e| format!("저장을 못 했어요 — {e}"))?;
    Ok(slug)
}

fn system_prompt(name: &str, digest: &str, unprompted: bool) -> String {
    let identity = identity_for(name).unwrap_or_else(|| {
        format!("너는 {name} — 사용자를 '선생님'이라 부르는 비서다. 짧고 다정한 존댓말로 말한다.")
    });
    let job = if unprompted {
        "지금은 선생님이 무언가 묻지 않았다. 방금 바뀐 상황 중 **말할 가치가 있는 것 하나**만 골라 \
         한두 문장으로 먼저 건네라. 바뀐 게 없으면 작업과 무관한 짧은 잡담 한 마디도 좋다. \
         인사말·「무엇을 도와드릴까요」로 시작하지 마라 — 아무도 부르지 않았다."
    } else {
        "선생님의 말에 한두 문장으로 답하라. 물은 것이 pane 상황이면 아래 현황을 근거로 \
         구체적으로(어느 pane 의 누가 무엇을) 답하고, 잡담이면 현황을 억지로 끼워 넣지 마라."
    };
    format!(
        "{identity}\n\n\
         [자리] 너는 지금 터미널 앱 kasaterm 의 우측 패널에 상주하는 말상대다. 코드를 고치거나 \
         명령을 실행하지 않는다 — 할 수 있는 것은 보는 것과 말하는 것뿐이다. 못 하는 일을 \
         하겠다고 하지 마라.\n\n\
         [지금 돌아가는 작업들]\n{digest}\n\
         [말투] 말풍선 한 칸에 들어갈 길이. 두 문장을 넘기지 마라. 목록·코드블록·마크다운 \
         표를 쓰지 마라. 이모지를 쓰지 마라.\n\n\
         [이번 차례] {job}"
    )
}

/// 값싼 모델을 한 번 부른다. 실패는 문자열로 돌려 호출부가 말풍선에 그대로 띄운다
/// — 패널이 조용히 죽는 것보다 「키가 없어요」가 보이는 편이 낫다.
pub async fn chat(req: &ChatReq, board: &[PaneActivity]) -> Result<String, String> {
    let key = api_key().ok_or_else(|| {
        "말할 수가 없어요 — ~/.config/opengateway.key 가 비어 있어요.".to_string()
    })?;
    let name = character_name(&req.character);
    let sys = system_prompt(&name, &board_digest(board), req.unprompted);

    let mut msgs: Vec<serde_json::Value> = Vec::new();
    // 창을 태우지 않게 최근 왕복만 남긴다. 페르소나 대화는 앞을 잊어도 크게 안 아프다.
    let tail = req.history.len().saturating_sub(12);
    for t in &req.history[tail..] {
        let role = if t.role == "assistant" { "assistant" } else { "user" };
        if t.content.trim().is_empty() {
            continue;
        }
        msgs.push(serde_json::json!({ "role": role, "content": t.content }));
    }
    let user_text = if req.message.trim().is_empty() {
        "(선생님은 아무 말도 하지 않았다. 위 지시대로 먼저 말을 걸어라.)".to_string()
    } else {
        req.message.clone()
    };
    msgs.push(serde_json::json!({ "role": "user", "content": user_text }));

    let body = serde_json::json!({
        "model": model(),
        "max_tokens": 300,
        "system": sys,
        "messages": msgs,
    });
    let resp = reqwest::Client::new()
        .post(format!("{}/v1/messages", base_url()))
        .header("Authorization", format!("Bearer {key}"))
        .header("anthropic-version", "2023-06-01")
        .header("content-type", "application/json")
        .body(serde_json::to_vec(&body).map_err(|e| format!("보낼 걸 못 만들었어요 — {e}"))?)
        .send()
        .await
        .map_err(|e| format!("모델에 못 닿았어요 — {e}"))?;
    let raw = resp
        .text()
        .await
        .map_err(|e| format!("답을 못 읽었어요 — {e}"))?;
    let v: serde_json::Value =
        serde_json::from_str(&raw).map_err(|e| format!("답이 JSON 이 아니었어요 — {e}"))?;
    if let Some(err) = v.get("error").and_then(|e| e.get("message")).and_then(|m| m.as_str()) {
        return Err(format!("모델이 거절했어요 — {err}"));
    }
    let text = v
        .get("content")
        .and_then(|c| c.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
                .collect::<Vec<_>>()
                .join("")
        })
        .unwrap_or_default();
    if text.trim().is_empty() {
        return Err("빈 답이 왔어요.".into());
    }
    Ok(text.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 한글을 바이트로 자르면 패닉이 난다 — 말풍선에 들어갈 문장은 거의 다 한글이라
    /// 이 자리가 실제로 밟히는 경로다.
    #[test]
    fn clip_is_char_safe() {
        let s = "가나다라마바사아자차";
        assert_eq!(clip(s, 3), "가나다…");
        assert_eq!(clip(s, 100), s);
    }

    /// 그림 경로에 `..` 나 `/` 가 섞여 들어오면 레포 밖 파일을 읽어 준다.
    #[test]
    fn portrait_rejects_traversal() {
        assert!(portrait("../../etc/passwd").is_none());
        assert!(portrait("a/b").is_none());
        assert!(portrait("").is_none());
    }
}
