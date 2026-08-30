---
name: kasapane
description: kasaterm/kasaspace pane을 다루고, 그 안에서 긴 잡(빌드·dev server·배포)을 풀 사이클로 돌리고, 작업 결과(이미지·마크다운)를 창 안 pane에 띄우고, 여러 pane의 claude(또는 codex·antigravity 등)가 같은 레포를 동시에 만질 때 충돌 없이 협업하고(board 패널·SendMessage·peek), kasaterm 자체 UI를 빌드→스폰→스크린샷→확인 사이클로 자체검증하고, TeamCreate로 띄운 팀원 pane의 race·좀비를 청소한다. 사용자가 "모니터 띄워줘", "로그 따로 보게 해줘", "pane 쪼개/이름/색", "dev 서버 옆에 띄워", "이미지/마크다운 띄워줘", "다른/옆 pane 뭐하는지", "협업", "충돌 피해", "같이 작업", "팀원 pane 정리", "팀 좀비 청소", "빌드 돌려줘", "kasaterm 화면 확인해줘", "스크린샷 찍어서 봐줘" 같은 요청, 또는 멀티 pane 환경(KASATERM_PANE_ID env 존재)에서 코드 작업을 시작하거나 작업 중 만든 스샷·문서를 사용자 화면에 보여줄 때 사용. raw tmux(비-kasaterm) 컨텍스트는 tmux-pane-job 스킬로.
version: 0.4.0
user-invocable: true
argument-hint: "[pane 작업 또는 검증할 UI 항목]"
---

# kasapane — kasaterm pane 제어 · 잡 사이클 · UI 자체검증

이 스킬은 셋을 한 자리에 묶는다:

1. **pane 제어** — kasaterm 안에서 pane을 만들고 꾸미고 결과를 띄운다 (kasaterm-cli).
2. **긴 잡 사이클** — 빌드·dev server·배포 같은 1분+ 명령을 별도 pane에서 안전히 돌리고 결과를 회수한다.
3. **kasaterm 자체검증** — UI 코드를 고친 뒤 너 자신이 빌드→스폰→스크린샷→확인을 돌려 진짜 동작하는지 본다 (사용자에게 "테스트해보세요" 떠넘기지 않음).

raw tmux(예: debi-marlene 배포)는 [tmux-pane-job 스킬]로. 이 스킬은 kasaterm을 전제로 한다.

## 전제: kasaterm 셸 안에서만 동작 (1·2번에 한함)

`kasaterm-cli`은 `$KASATERM_SOCKET_PATH`(없으면 `$CMUX_SOCKET_PATH`)로 어느 kasaterm 인스턴스에 붙을지 정한다. 본체가 띄운 셸에는 이 env가 자동 export돼 있다(`pty-backend/src/state.rs`).

```bash
echo "$KASATERM_SOCKET_PATH"          # 비어 있으면 kasaterm 셸이 아님 → 1·2번 중단, 3번만 가능
kasaterm-cli list surfaces             # JSON pane 목록이 나오면 정상
```

env가 비어 있거나 list가 실패하면 pane 조작을 시도하지 말고 사용자에게 알린다 — 단, 3번 자체검증은 셸 밖에서도 가능(빌드·스폰·캡처는 모두 셸 무관).

---

## 도구 경계 — MCP vs Bash (가장 먼저 읽기)

`mcp__kasaspace__*` 와 `kasaterm-cli`(bash) 둘 다 같은 socket을 친다. 결과는 같다. **언제 무엇을 쓸지**가 다르다.

| 상황 | 쓰는 도구 | 왜 |
|---|---|---|
| 사용자가 직접 "이 이미지/문서 띄워" 시킨 일회성 GUI 액션 | **MCP** `mcp__kasaspace__*` | 의도 명확, 한 번이면 끝, 호출 흔적이 turn에 명시적으로 남음 |
| pane 분할·이름·색·send·focus를 **루프나 다단계로** 엮는 자동화 | **Bash** `kasaterm-cli` | 변수 캡처(`NEW=$(...)`), 파이프, jq 등 셸 도구 결합 자유 |
| TeamCreate 직후 race 검증·좀비 청소·config.json 조작 | **Bash** | jq + 조건 분기 + 파일 편집까지 한 흐름. MCP 없음 |
| 빌드·dev server·테스트 풀 사이클 (tee+DONE+Monitor) | **Bash** | 시퀀스가 길고 로그 파일 회수까지 결합 |
| 이미지·마크다운 결과물을 메인 창에 띄우기 (자동화의 마지막 한 컷) | **Bash** `imgopen`/`mdopen` | 자동화 흐름의 일부이면 셸로 끝맺음 |
| 사용자가 명시적으로 "이 결과 보여줘" 하면 | **MCP**도 OK | 일회성·명시적 GUI 액션이라 MCP가 의도 표현 더 깨끗 |

**원칙 한 줄**: *제어 루프와 자동화는 bash, 사용자 의도의 일회성 GUI 액션은 MCP*.

MCP 도구 카탈로그 전수는 [부록 A](#부록-a--mcp-도구-카탈로그)에.

---

## 1) Pane 제어 — kasaterm-cli 완전 명령 레퍼런스

> **`kasaterm-cli --help` / `<cmd> --help`를 실행하지 마라.** 아래가 모든 서브명령·인자·반환을 그대로 담는다. help는 컨텍스트만 태운다. 이 표에 없는 걸 만나면 그때만 `--help`.

**공통 규약** (한 번만 외우면 됨):
- 모든 명령은 **한 줄 JSON**으로 응답. 성공 `{"ok":true,"result":{…}}`, 실패 `{"ok":false,"error":{"code":-32603,"message":"no such pane: %9"}}` — 성공 판정은 최상위 `.ok`.
- `layout`·`windows`만 예외로 **plain text**(JSON 아님) 출력.
- pane id(=surface_id)는 `%0`,`%1`… 형식. split/close로 바뀌니 연속작업 전 `list surfaces`로 재확인.
- socket은 `$KASATERM_SOCKET_PATH`(없으면 `$CMUX_SOCKET_PATH`)로 **자동 선택** — 인자로 넘기지 않는다.

### 명령 전수

**pane 조작 (mutation)** — 대부분 `result`가 `{"ok":true}` 뿐. 예외만 표기.

| 명령 | 인자·동작 | 특이 반환 |
|---|---|---|
| `split <left\|right\|up\|down> [%id] [--focus]` | **부른 pane**(`$KASATERM_PANE_ID`)을 방향 분할 — 2026-08-04 이전엔 늘 포커스된 pane 이라, 에이전트가 학생을 띄우면 거노가 보고 있던 창이 쪼개졌다. `%id` 로 다른 pane 지정 가능, pane 밖에서 부르면 포커스 기준. `--focus` 없으면 포커스 유지 | `result.surface.id` = 새 pane id |
| `focus <id>` | 포커스 이동 | — |
| `close <id>` | pane 닫기(셸 종료) | — |
| `dismiss <id>… [--force]` | 일 끝난 학생 pane 을 한 번에 닫기. 각 pane 의 cwd 를 `git status` 로 보고 **커밋 안 된 변경이 있으면 안 닫고 보고**. 대상 항상 명시(`--all` 없음) — 패턴 G | 한 줄씩 plain text(`closed`/`kept`/`failed`) |
| `rename <id> <title>` | 헤더 제목 | — |
| `rename-window <title>` | **이 pane의 세션(윈도우) 이름.** id 안 받음 — 자기 세션 대상 | — |
| `color <id> <#rrggbb>` | 헤더 accent 색 | — |
| `swap <a> <b>` | 두 pane 위치 교환(내용 유지) | — |
| `resize <id> <ratio>` | 직계 split에서 차지 비중 `0..1`(오케스트레이터 pane 크게). **split 직후 새 pane엔 즉시 안 먹힘**(`no such pane`) → 기존 pane에만 | — |
| `send [--surface <id>] <text>` | **입력만, 제출 X.** 셸 명령 주입 전용 — 개행은 `$'cmd\n'`로 직접. 사람·claude엔 절대 쓰지 마라 → `tell` | — |
| `key [--surface <id>] <name>` | 키 1개 전송. name: `enter tab escape up down left right home end pageup pagedown backspace delete` | — |
| `tell <id> <text>` | send+제출(`\r`). idle claude를 새 턴으로 깨움 (§5) | — |

**배치·현황 조회 (read-only, 부작용 없음)**

| 명령 | 반환 |
|---|---|
| `list surfaces` | `result.surfaces` = `[{"id":"%1","workspace_id":"local-0"},…]` |
| `list workspaces` | `result.workspaces` = `[{"id":"local-0","name":"kasaterm"}]` |
| `identify` | `result.surface.id` = **지금 내가 어느 pane인지**(`$KASATERM_PANE_ID`와 동일) |
| `layout` | ASCII 박스 아트 — 활성 윈도우의 pane 배치 (**plain text**) |
| `windows` | 윈도우별 pane 목록, 사이드바 순서 (**plain text**). 단일 윈도우면 `(윈도우 없음)` |
| `peek [<id>] [lines]` | `result.text` = 그 pane 화면 tail(문자열). id 생략=자기 자신 (§5) |
| `transcript [<id>] [N]` | `result.turns` = `[{"role":"user\|assistant","text":"…"},…]` 마지막 N턴 (§5) |
| `activity [<id>] [N]` | `result.events` = `[{"kind":"prompt\|say\|tool\|result","name":"Bash","text":"…","is_error":false},…]` 최신 N건, 오래된 것부터. 도구 **인자 원문**+결과+성패 — board가 원리적으로 못 주는 것 (§5) |
| `board [screen_lines]` | `result.board` = pane별 상태 배열(필드는 아래). N 주면 각 항목에 `screen`(화면 tail N줄) 추가 (§5) |
| `ping` | `result.pong=true` — 소켓 살아있나 |
| `capabilities` | `result.methods` = RPC 메서드 전수(디버깅용) |

**`board` 항목 필드** — 협업 판단엔 이것만 보면 된다(원소당 필드가 20+개지만 나머지는 노이즈):

`surface_id` · `character`(학생 이름) · `status`(`idle`\|`working`) · `intent`(지금 하려는 것) · `last_prompt` · `last_reply` · `cwd`(셸 위치) · `view_cwd`(파일트리 위치) · `changed_files`(수정한 파일 절대경로) · `recent_tools` · `model` · `context_pct` · `window_idx` · `title`.
→ **충돌 회피는 `status`+`changed_files`+`intent` 세 개면 충분.**

⚠️ **board 는 도구의 성패를 안 싣는다.** `recent_tools` 는 호출 라벨 최근 **8개**뿐이라
「같은 명령이 세 번 넘게 같은 오류로 끝났다」를 **원리적으로 판정할 수 없다** — 재료가 없다.
막힌 이유를 알아야 하면 `activity <id>`(인자 원문+결과+오류 여부, 시간순). 「쟤 뭐 하나」=board,
「쟤 왜 저러나」=activity. 라벨은 `설명 — 명령` 꼴이다(2026-08-30 — 그 전엔 첫 줄만 남겨서
여러 줄 스크립트들이 죄다 같은 라벨로 보였다).

**hook 전용 (Claude Code hook이 자동 호출 — 에이전트가 수동으로 부를 일 없음)**

`bind-transcript <path>`(SessionStart) · `notify [--surface <id>] <title> [body]`(Stop) · `attention [--surface <id>] [reason]`(Notification).

**협업 스트림 (blocking — 반드시 background로)**

`board-watch [interval_s]`(변경된 pane 상태를 1줄/변경으로 스트림 → Claude Code Monitor에 먹임) · `wake-watch <id> [interval_s] [--timeout s]`(동료가 한 턴 끝낼 때까지 block 후 스스로 종료 → background task로 띄우면 완료 즉시 자동 wake). 둘 다 §5.

**결과물 띄우기 헬퍼** (CLI 서브명령 아님 — 별도 실행파일. 상세 패턴 D):
`imgopen <절대경로.png>`(이미지 → 새 pane, 맞춤↔원본) · `mdopen <절대경로.md>`(마크다운 → 노션풍 렌더 pane).

### 자주 쓰는 파싱 one-liner

```bash
# 새 pane id 추출 (split 반환)
NEW=$(kasaterm-cli split right | python3 -c 'import sys,json;print(json.load(sys.stdin)["result"]["surface"]["id"])')

# 지금 내가 어느 pane인지 (env가 더 빠름)
ME=${KASATERM_PANE_ID:-$(kasaterm-cli identify | python3 -c 'import sys,json;print(json.load(sys.stdin)["result"]["surface"]["id"])')}

# working 중인 형제 pane만 뽑기 (충돌 회피)
kasaterm-cli board | python3 -c 'import sys,json
for p in json.load(sys.stdin)["result"]["board"]:
    if p["status"]=="working": print(p["surface_id"], p["character"], "→", p["intent"])'
```

### 패턴 A — 모니터 pane (백그라운드 로그)

오래 도는 명령을 **별도 pane에서 돌려 로그를 흐르게** 한다. 메인 pane은 자유. 사용자와 claude가 같은 화면을 본다.

```bash
NEW=$(kasaterm-cli split right | python3 -c 'import sys,json;print(json.load(sys.stdin)["result"]["surface"]["id"])')
kasaterm-cli rename "$NEW" "dev log"
kasaterm-cli color  "$NEW" "#58a6ff"
kasaterm-cli send --surface "$NEW" $'npm run dev\n'   # ANSI-C 따옴표로 엔터 보장
```

원칙:
- **백그라운드 작업은 항상 새 pane에서.** `&`로 메인에 묻지 말 것.
- 명령 텍스트는 `$'...\n'`(ANSI-C 따옴표)로 보내 엔터가 전달되게.
- 용도별 색 컨벤션: 로그=파랑 `#58a6ff`, 빌드=주황 `#d29922`, 테스트=초록 `#3fb950`, 위험/에러=빨강 `#f85149`.

### 패턴 B — 레이아웃 구성

분할은 **현재 포커스된 pane** 기준. 다음 분할 전에 `focus`로 기준 pane을 옮긴다.

```bash
RIGHT=$(kasaterm-cli split right | ...id...)
kasaterm-cli focus "$RIGHT"
kasaterm-cli split down     # 오른쪽을 다시 위/아래로
```

자리 잘못 잡았으면 `swap`으로 교환(내용 유지).

### 패턴 C — 팀원 pane 정리

`TeamCreate`/`Agent`(teammateMode=tmux)로 만든 팀원 pane을 알아보기 쉽게.

```bash
kasaterm-cli list surfaces                       # 새 팀원 pane id 확인
kasaterm-cli rename "%2" "scout"
kasaterm-cli color  "%2" "#a371f7"
```

역할 색: 탐색=보라 `#a371f7`, 구현=파랑 `#58a6ff`, 검증=초록 `#3fb950`, 리드=노랑 `#d29922`.

### 패턴 D — 결과 이미지·문서를 pane에 띄우기

작업 중 만든 **스샷·다이어그램·생성 이미지·마크다운**은 별도 OS 창이 아니라 kasaterm 창 안 pane으로 띄운다. 사용자가 같은 화면에서 본다.

```bash
imgopen /절대경로/result.png     # 이미지 → 새 pane (맞춤↔원본)
mdopen  /절대경로/doc.md         # 마크다운 → 노션풍 렌더 pane
```

원칙:
- **결과물 만들면 자동으로 띄운다.** "이거 봐주세요" 하고 경로만 알려주지 말 것.
- 경로는 **절대경로**.
- 헬퍼 없다(`command not found`)면 본체가 옛 버전이거나 kasaterm 셸 밖. 셸 밖에서 직접 띄울 땐:
  ```bash
  curl -s --get --data-urlencode "path=<절대경로>" \
    "http://127.0.0.1:${KASASPACE_MCP_PORT:-8765}/open-image"
  # 마크다운은 /open-markdown
  ```

#### 사용자가 잘 보이게 띄우기 — 레이아웃 사전 정리

이미지/문서를 띄울 때 화면이 이미 4-pane으로 어수선하면 새로 뜬 pane이 손바닥만 해서 사용자가 못 본다. 띄우기 **전에** 한 번 확인 + 정리.

**1) 사전 점검** (imgopen/mdopen 직전):
```bash
COUNT=$(kasaterm-cli list surfaces | python3 -c \
  'import sys,json;print(len(json.load(sys.stdin)["result"]["surfaces"]))')
echo "pane count: $COUNT"
```

**2) 갯수별 정책**:

| pane 갯수 | 행동 |
|---|---|
| 1-2 | 그냥 `imgopen` — 자동 분할이 충분히 큰 자리 확보 |
| 3 | 그냥 `imgopen` — 4-pane 되지만 보통 OK. 사이즈 작아 보이면 그때 정리 |
| 4+ | **사용자한테 1줄 알림** ("스크린샷 띄울 자리 만들기 위해 이전 모니터 pane X 닫을게요") 후 본인이 만든 모니터/로그 pane 닫기. 사용자 작업 pane은 절대 안 건드림 |

본인이 만든 모니터 pane 구별 — 헤더 색이 컨벤션(`#58a6ff` 로그 / `#d29922` 빌드 / `#3fb950` 테스트 / `#a371f7` 탐색) 중 하나면 본인이 만든 것. 사용자 pane은 보통 색 없음 또는 다른 색.

```bash
# 본인 만든 모니터 색을 가진 pane id 추출
kasaterm-cli list surfaces | python3 -c "
import sys, json
rs = json.load(sys.stdin)['result']['surfaces']
mine_colors = {'#58a6ff','#d29922','#3fb950','#a371f7','#ff8800'}
for s in rs:
    if (s.get('color') or '').lower() in mine_colors:
        print(s['id'], s.get('title'))
"
```

**3) 띄운 후 검증** — 새 이미지 pane이 충분히 큰지(논리 픽셀 가로 ≥ 500). 너무 작으면 다른 pane을 swap하거나 사용자에게 한 줄 알림.

**4) 자동 정리 (다음 이미지로 교체될 때)** — 이미지가 일련의 결과 흐름(예: A 다음 B 다음 C)이면 이전 imgopen pane을 닫고 새로 띄움. 매번 새 pane 만들면 4개 금방 쌓임.
```bash
# 가장 최근 imgopen pane id를 변수에 저장
PREV_IMG=$(kasaterm-cli list surfaces | python3 -c "
import sys, json
rs = json.load(sys.stdin)['result']['surfaces']
imgs = [s['id'] for s in rs if (s.get('title') or '').endswith(('.png','.jpg','.jpeg','.webp'))]
print(imgs[-1] if imgs else '')
")
[ -n "$PREV_IMG" ] && kasaterm-cli close "$PREV_IMG"
imgopen /절대경로/new.png
```

**5) 검증용 임시 스샷은 imgopen 금지** — 모델 자체검증(헤드리스 캡처) 결과는 `Read("/tmp/...png")`로 본인이 확인만. 사용자 화면에 띄우는 건 **사용자가 봐야 하는 진짜 결과물**일 때만.

**안티 패턴**:
| 안 됨 | 왜 |
|---|---|
| 사전 점검 없이 매 결과마다 `imgopen` | 4-pane 넘어가면 사용자한테 손바닥만 함 |
| 사용자 작업 pane을 정리 명목으로 close | 사용자 작업 날아감. 본인이 만든 monitor/log pane만 정리 |
| 이전 이미지 pane 안 닫고 새로 띄움 | 누적되어 화면 어수선 |
| 검증용 임시 스샷까지 띄움 | 사용자가 모델 내부 검증 흐름 다 보게 됨 — 시각 노이즈 |

### 패턴 E — `SendUserFile` 자동 imgopen Hook

Claude Code의 `SendUserFile` 툴은 어느 터미널에서든 `[image] /path (size)` 텍스트 플레이스홀더만 출력하지(iTerm/kitty 인라인 escape를 emit 안 함), 실제 이미지를 인라인 렌더하지 않는다. kasaterm 안에서 작업할 땐 **PostToolUse hook**으로 SendUserFile 결과의 이미지 경로를 자동 `imgopen` 호출시켜 image pane으로 띄운다.

**1) Hook 스크립트** — `~/.claude/hooks/auto-imgopen.sh`:
```bash
#!/bin/bash
# PostToolUse hook for SendUserFile — image files은 자동으로 imgopen
input=$(cat)
echo "$input" | python3 -c "
import sys, json, os, subprocess
try:
    d = json.load(sys.stdin)
except Exception:
    sys.exit(0)
paths = d.get('tool_input', {}).get('files', []) or []
exts = ('.png','.jpg','.jpeg','.gif','.webp','.bmp','.tiff','.tif')
for p in paths:
    if not isinstance(p, str): continue
    if not p.lower().endswith(exts): continue
    p_abs = p if os.path.isabs(p) else os.path.join(os.getcwd(), p)
    if not os.path.isfile(p_abs): continue
    try:
        subprocess.run(['imgopen', p_abs], timeout=5, check=False)
    except Exception:
        pass
" >/dev/null 2>&1 || true
exit 0
```
`chmod +x` 잊지 말 것.

**2) `~/.claude/settings.json`의 `hooks.PostToolUse` 배열에 추가**:
```json
{
  "matcher": "SendUserFile",
  "hooks": [
    { "type": "command", "command": "~/.claude/hooks/auto-imgopen.sh", "timeout": 10 }
  ]
}
```
기존 PostToolUse 엔트리는 **반드시 보존하고 새 엔트리를 append**(replace 금지). 다른 hook(예: dotfiles autopush)이 같이 묶여 있으니 배열 추가 방식.

**검증**:
```bash
# 스키마 통과 확인 — 명령 문자열이 출력되면 OK
jq -e '.hooks.PostToolUse[] | select(.matcher=="SendUserFile") | .hooks[].command' \
  ~/.claude/settings.json

# 파이프 테스트 — 가짜 페이로드로 imgopen이 실제 호출되는지
echo '{"tool_name":"SendUserFile","tool_input":{"files":["/tmp/foo.png"]}}' \
  | ~/.claude/hooks/auto-imgopen.sh
```

**한계**:
- kasaterm 셸 밖에서 Claude Code가 돌면 `imgopen`이 PATH에 없거나 `KASASPACE_MCP_PORT` env가 없어서 hook이 silently no-op.
- hook watcher가 세션 시작 시점에 `.claude/` 디렉토리를 안 보고 있었으면 새 hook이 즉시 활성화 안 됨. `/hooks` 한 번 열거나 Claude Code 재시작.
- SendUserFile 외의 다른 이미지 출력 경로(예: tool result에 image 첨부)는 hook이 못 잡음 — 그건 직접 `imgopen` 호출.

### 패턴 F — 학생 pane 스폰 (오케스트레이터: 이름·색·모델·effort 지정 claude)

오케스트레이터(작업 배분하는 pane — 아무 학생이나 가능)가 작업을 병렬 배분할 때 학생 claude pane을 만드는 표준 레시피. teammate 플래그로 이름·색이 **부팅 시점에 네이티브 표시**된다(입력박스 상단 `@이름` — v2.1.207 실측).

**위임 기본값(거노 확정 2026-07-19): kasaterm 안에서 병렬·장시간 작업 위임은 학생 pane 스폰이 기본이다.** Agent 툴 백그라운드 서브에이전트는 board·statusline에 안 보여 거노가 진행을 지켜볼 수 없다 — Agent 툴은 kasaterm 셸 밖이거나 거노가 명시할 때만. 거노가 모델을 지정하면("오푸스로") 그 지시가 스킬의 기본 모델보다 우선하며, 이미 실행 중인 학생의 모델 전환은 `kasaterm-cli tell <id> "/model claude-opus-5[1m]"`(TUI 슬래시 명령 주입)로 컨텍스트 유지한 채 가능하다 — 반드시 풀네임+[1m] 변형으로.

```bash
# split 은 **부른 pane** 을 쪼갠다(2026-08-04) — 거노가 보고 있는 창이 아니라 내 자리 옆에 뜬다.
# 응답에 pane id 와 **그 pane 이 claude 로 뜨면 쓸 agent 이름**이 같이 온다.
eval "$(kasaterm-cli split right | python3 -c 'import sys,json
r=json.load(sys.stdin)["result"]
print(f"S={r[\"surface\"][\"id\"]}; A={r.get(\"agent\",\"\")}")')"
kasaterm-cli rename "$S" "<작업명>"
kasaterm-cli color  "$S" "#58a6ff"
# split 직후 send는 "surface 없음" 가드 오발동이 잦다(id 재사용, 실측 2회) → 실패 시 sleep 2 후 재시도
# 트리플(--agent-id/--agent-name/--team-name)·--model 은 **붙이지 마라** — shim 이 자동으로 붙인다.
# 직접 주면 자동 부착이 통째로 꺼진다.
kasaterm-cli send --surface "$S" $'cd /path/to/repo && claude\n'
# 브리프는 **SendMessage 로**. 파일에 쓰지도, 부팅 커맨드에 싣지도 마라.
#   SendMessage({to: "<split 응답의 agent>", message: "<배경·파일 포인터·검증 기준·커밋은 자기 브랜치에>"})
# 기다릴 필요 없다: 인박스 파일은 shim 이 `[ -f ] ||` 로 만들어 **먼저 넣어 둔 것을 안 덮으므로**,
# claude 가 뜨자마자 읽는다(거노: "claude 켜면 바로 켜지는데 바로 SendMessage 하면 되는데").
```

**`split` 응답이 그 pane 의 `agent`·`team` 을 준다** — 학생은 pane 이 생기는 순간 배정되고 shim 은 그
슬러그에 `-p<번호>` 를 붙일 뿐이라, **부팅 전에 이름이 이미 정해져 있다**. 그러니 board 를 되짚지도,
`<슬러그>-p<번호>` 를 손으로 짐작하지도 마라 — 짐작은 어긋나도 오류가 안 나고 브리프가 조용히 사라진다.
`--count N` 이면 `agents` 배열로 같이 온다. (팀은 pane cwd 로 계산하므로 학생을 **다른 레포로 `cd` 시켜**
띄우면 그 값이 틀린다 — 그 경우만 board 가 정본이다.)

⚠️ **부팅 커맨드는 갓 만든 빈 pane 에만.** 이미 claude 가 도는 pane 에 `... && claude ...` 를 보내면 셸이 아니라 **그 claude 의 입력창**이 받아 지시로 읽고, 하네스식 스폰 절차(인박스 선주입 → pane 부팅)를 그대로 쓰면 아무도 뜨지 않은 이름의 인박스가 하나 생겨 브리프가 조용히 사라진다(`inboxes/` 의 고아 파일들이 그 잔해다). 2026-08-04부터 **서버가 거부한다** — `surface.send_text` 가 board(=claude 가 도는 pane 목록)를 보고 부팅 커맨드면 막고 대신 쓸 `to:` 이름을 알려준다. 도는 pane 에 할 말은 SendMessage 뿐이다.

⚠️ **send 를 두 번 연달아 보내지 마라 — 둘째가 첫 명령줄 안으로 빨려 들어간다.** 부팅 커맨드를 보낸 직후 브리프를 또 보내면, 셸이 아직 `claude` 를 exec 하기 전이라 둘째 텍스트를 **같은 명령줄의 일부로** 읽는다(실측 2026-08-05: 모델명이 `claude-opus-5[1m]지금[1m]` 이 되어 부팅 실패, 프롬프트엔 「몇」 한 글자만 남음). 그래서 부팅은 **한 번의 send 로 끝내고**, 할 말은 전부 SendMessage 로 한다 — 인박스는 셸 명령줄과 무관한 경로라 이 함정 자체가 없다.

- `--agent-name`은 **`--agent-id`·`--team-name`과 셋이 세트** — 하나라도 빠지면 "must all be provided together" 에러(실측). `--agent-color`는 8색(red/blue/green/yellow/purple/orange/pink/cyan). `--model`·`--effort`·`--session-id`·`--resume`은 공개 플래그.
- **가벼운 작업의 기본은 glm·kimi 학생**(사내 OpenGateway, 키 `~/.config/opengateway.key`) — 정찰·검색·스샷 촬영·기계적 반복처럼 판단보다 손이 많은 일. Claude 로 띄울지부터 고민하지 말고 그냥 여기로 보낸다. 부팅 커맨드만 바꾸면 된다:
  ```bash
  kasaterm-cli send --surface "$S" $'cd /path/to/repo && glm claude --dangerously-skip-permissions\n'
  ```
  `glm` 자리에 `kimi` 를 쓰면 Kimi 다(`moonshotai/kimi-k3-ultrafast`). 둘 다 zshrc 의 `_opengateway_run` 하나를 공유하고 모델명만 다르다.
  **왜 기본값이어야 하나 — 컨텍스트 때문이다.** 손이 많고 판단이 적은 일에 오푸스를 붙이면 검색 결과와 파일 덩어리가 창을 금세 채워 compact 가 돌고, 압축될 때마다 앞의 맥락이 깎인다(거노가 지금 겪고 있는 문제). 값싼 창을 태워야 할 일에 비싼 창을 태우지 말 것.
  실측(2026-08-05): 모델 `z-ai/glm-5.2-ultrafast`, 응답 6.0s, `agent_name`·캐릭터·페르소나 전부 정상 부착 → **SendMessage 로 닿는다**(board 확인: `hoshino-p17` / team `kt-tmp-5ee1`). `glm` 은 `command claude` 라 shim 을 그대로 타고, shim 이 앞에 붙이는 `--model claude-opus-5[1m]` 은 glm 이 뒤에 주는 `--model z-ai/...` 에 덮인다.
  - ⚠️ **`--dangerously-skip-permissions` 를 직접 줘야 한다.** `glm`·`kimi` 가 `command claude` 로 호출해 zshrc 의 `claude()` 함수(그 플래그를 자동으로 붙여 주던)를 건너뛰기 때문. 빠뜨리면 학생이 첫 도구 호출에서 권한 프롬프트에 걸려 멈춘다.
  - ⚠️ **컨텍스트 200k — Claude `[1m]` 의 1/5.** 긴 파일 통독·장시간 작업엔 부적합하다. 짧고 손 많은 일에만 보내는 것이 이 둘을 쓰는 법이고, 본작업은 Claude 로.
- **학생 모델 = 반드시 풀네임 + `[1m]`(1M 컨텍스트) 변형**: 가벼운 잡·정찰=`'claude-sonnet-5[1m]'`, 구현·생성 본작업=`'claude-opus-5[1m]'`(유효 실측 2026-07-26). ⚠️ **`opus`·`sonnet` 짧은 alias 를 쓰지 마라** — alias 는 아직 이전 세대(`opus`→Opus 4.8)를 가리켜 오푸스 5 로 안 뜬다(거노 실사고 2026-07-26: 학생이 전부 4.8 로 소환됨). 세대가 바뀌면 alias 가 늦게 따라오므로 풀네임이 정본이다. 표준(200K) 변형으로 띄우거나 `/model opus`처럼 무접미 전환하면 컨텍스트 창이 줄어든다 — 실사고: sonnet[1m] 세션을 `/model opus`(200K)로 바꾸자 같은 대화가 87%로 점프. **미드세션 전환도 항상 `/model claude-opus-5[1m]` 꼴로.** 대괄호가 zsh glob이라 CLI에선 **따옴표 필수**(안 감싸면 "no matches found"). 거노가 모델을 지정하면 그 계열의 [1m] 변형으로 해석한다. 모델의 자기보고("200K 표준")는 훈련지식이라 믿지 말 것 — 하네스 statusline이 진실. **함정(실사고 07-19): tell로 주입한 슬래시 명령은 학생이 working 중이면 제출돼도 큐에만 걸리고 발화하지 않는다** — 코하루가 전환 미발화 상태로 200K를 100%까지 소진. 주입 후 반드시 peek로 statusline의 `1M` 표기를 확인하고, 큐에 걸려 있으면 `key escape`로 턴을 끊어 발화시켜라(파일 수정분은 보존됨).
- **agent-name은 학생 캐릭터명이 아니라 목표 작업명으로**(거노 확정, 예: `native-wiring-backend`) — 캐릭터는 kasaterm이 pane에 자동 배정하니 이름 중복이 불필요하고, ASCII 작업명이면 inbox 슬러그 유일성도 자연 해결.
- **표시 매핑(실제 팀모드 스크린샷 실측, 2026-07-13)**: `--agent-color`는 배지(`@이름`)뿐 아니라 teammate TUI 전체 톤을 그 색으로 테마한다. tmux pane 제목 = 에이전트 이름, TUI 상단 `✳ 헤더` = 역할(`--agent-type`, 예: Explore). → kasaterm 스폰도 `rename`을 agent-name(작업명)과 일치시키고, `--agent-color`는 배정 학생 accent에 가장 가까운 8색으로 골라 pane 테두리색과 TUI 색을 맞춘다.
- `--agent-type`은 역할 표시용으로 보이지만 **그 agent 정의(도구 제한 포함)를 실제 로드**한다 — Explore는 read-only라 작업 학생에 부적합. 임의 문자열 동작은 미검증 → 학생 스폰엔 생략.
- 실제 팀모드 자식 세션의 판별 마커: env `CLAUDE_CODE_TEAMMATE_MODE=tmux`·`CLAUDE_CODE_CHILD_SESSION=1`·`CLAUDE_CODE_FORK_SUBAGENT=1`, transcript에 `bridge-session` 레코드. 우리 수동 스폰엔 이 env가 없어도 inbox 송수신은 동작(F-2) — kasaterm이 "진짜 팀모드 자식" 여부를 구분해야 할 때 이 마커를 쓴다.
- teammate 플래그는 숨은 인터페이스 — Claude Code 업데이트 후 안 먹으면 플래그 없이 부팅하고 `/rename`·`/color`(v2.1.205+)로 대체.
- 배분 전 `board`로 형제 pane의 `changed_files`를 확인해 **파일 경계를 사전 분리**(같은 파일을 두 학생이 만지면 같은 작업트리라 git 이전에 물리 충돌). 검수·커밋은 오케스트레이터 단독.
- wake-watch가 울려도 board idle은 오판일 수 있다 — `peek`로 실화면(스피너/보고문) 확인 후 판단(§5 함정 표).

**학생 답장 프로토콜(2026-08-04 개정 — shim 자동 트리플 복원 후)** — 학생이 pane에 남기는 일반 답장(턴 최종 텍스트)은 **pane 사용자(거노)에게만 보이고 오케스트레이터에게 자동 전달되지 않는다.** 받는 경로를 브리프에 못박아라:
1. **push(기본) = SendMessage** — 브리핑에 포함: "보고는 SendMessage to `$KASATERM_AGENT`(= 내 이름)". 양쪽 다 shim 트리플로 떠 있어 양방향이 열려 있다. 인박스 주입이라 상대 턴을 안 깨뜨린다.
2. **pull(예비)** — 학생이 보고를 깜빡했을 때: board-watch 로 idle 감지 → `kasaterm-cli transcript <id> N`.
**완료 통지는 시키지 마라.** 끝났는지는 board 의 idle 과 커밋이 말한다. 「작업 완료했습니다」 왕복은 양쪽 컨텍스트만 태운다 — 일이 끝나면 오케스트레이터가 `kasaterm-cli dismiss <id>…` 로 그냥 닫는다(패턴 G).

#### 패턴 F-2 — 네이티브 팀 배선 (SendMessage = inbox 파일, 2026-07-13 실측 확정 · 같은 날 풀 왕복 재검증)

teammate 플래그로 부팅된 세션은 **`~/.claude/teams/<팀>/inboxes/<슬러그(agent-name)>.json`을 스스로 폴링**해서, 새 항목을 `<teammate-message>` user 턴으로 즉시 주입받는다(미드런 OK·발신자 이름·색·summary 표시). SendMessage 도구의 실체가 이 파일 append다 — 즉 **파일만 쓰면 누구든(오케스트레이터·kasaterm·스크립트) 네이티브로 메시지를 꽂을 수 있다.** 전 경로 왕복 재검증 완료(2026-07-13, collab-native 실측): 리더→학생(파일 append) / 학생↔학생(SendMessage 상호 지목) / 학생→리더(team-lead inbox) / idle 자동 알림 — 순서만 지키면(분할→config 선작성→부팅) 수동 스폰으로 완전한 네이티브 팀이 된다.

**⚡ shim 자동 부착은 복원됐다(2026-08-04)**: 2026-07-24~08-03 사이엔 꺼져 있었다. 껐던 이유 둘 — `--resume` 마다 agent 이름 꼬리(sid4)가 바뀌어 **옛 이름 인박스가 고아화**(SendMessage 조용히 유실)되고 유령 인박스가 재시작마다 쌓인 것 — 은 **꼬리가 세션 id 였던 탓**이라, 꼬리를 **pane 번호**(`<슬러그>-p<번호>`)로 바꿔 둘 다 사라졌다: pane 은 생애 동안 번호가 안 바뀌어 몇 번을 resume 해도 같은 인박스고, 인박스 수는 방의 pane 슬롯 수로 묶인다. 진짜 동기였던 `@이름` 칩(입력박스 구분선에서 `/rename` 세션 이름 자리를 뺏음)은 render.rs `strip_teammate_chip` 이 칩만 지워 해결한다. 이제 pane claude 는 전부 팀원이라 **SendMessage 가 기본**이고, env `KASATERM_TEAM`/`KASATERM_AGENT` 도 다시 자동 설정된다. tell 은 SendMessage 가 안 닿는 상대(다른 방·bg 세션·비-claude)에만. **config.json 없이도 수신 주입·SendMessage 발신이 전부 동작한다(v2.1.211 재실측)** — 아래 config 선작성은 idle 자동보고·`claude agents` 명부가 필요한 오케스트레이션용이고, 단순 지시·보고엔 불필요.

**⚠️ detach(←←) 포크는 팀에서 이탈한다(2026-07-16 실측)**: detach는 같은 프로세스 유지가 아니라 **데몬이 argv를 재구성한 새 포크 프로세스**다 — teammate 트리플·`--append-system-prompt`(persona)·`--model`이 전부 유실되고 인박스 폴러가 안 돈다(인박스에 써도 미소비 실측). env도 원 pane이 아니라 **데몬을 낳은 옛 pane의 env**를 물려받아 계보가 틀리다. 백그라운드 포크에 SendMessage는 닿지 않는다 — 팀원으로 되살리려면 pane에서 `claude --resume <포크 sid>`에 **트리플을 명시 부착**해 재부팅해야 한다(2026-07-24 자동 재부착 제거 — 플래그 없이 resume하면 일반 세션으로 돌아올 뿐 팀원이 아니다). persona는 kasaterm SessionStart 훅이 세션 바인딩(/persona 엔드포인트)으로 자동 재주입한다.

```bash
# 0) (선택) 오케스트레이션용 팀 config — 채팅만이면 생략(폴러는 트리플 플래그만으로 arm,
#    v2.1.211 실측). idle 자동보고·명부가 필요할 때만 스폰 전에 작성(부팅 후 등록은 idle 보고에 무효)
mkdir -p ~/.claude/teams/<팀>/inboxes
# config.json: {"name":"<팀>","leadAgentId":"team-lead@<팀>","leadSessionId":"<오케스트레이터 세션id>","members":[{team-lead 엔트리},{학생 엔트리(agentId·name·color·model)}]}
# inboxes/: team-lead.json 과 학생별 <슬러그>.json 을 '[]' 로 초기화

# 1) 학생에게 메시지 (파일 append = SendMessage와 동일)
python3 - <<'PY'
import json,uuid,datetime
p='/Users/kasa/.claude/teams/<팀>/inboxes/<슬러그>.json'
m=json.load(open(p))
m.append({"from":"<발신 캐릭터명>","color":"<발신자 색(8색)>","text":"<지시>","summary":"<요약>","timestamp":datetime.datetime.now(datetime.UTC).isoformat().replace("+00:00","Z"),"msgV":1,"msg_id":str(uuid.uuid4()),"type":"message","read":False})
json.dump(m,open(p,'w'),ensure_ascii=False)
PY

# 2) 학생 보고 수신: 학생이 SendMessage 하면 inboxes/team-lead.json 에 쌓임
#    오케스트레이터가 team-lead 플래그(--agent-id team-lead@<팀> --agent-name team-lead --team-name <팀>)로
#    부팅돼 있으면 네이티브 주입으로 받고, 아니면 파일을 직접 읽는다.
#    ⚠️ inbox는 로그가 아니라 큐 — 수신 폴러가 소비한 엔트리는 파일에서 삭제된다(실측:
#    주입 직후 학생 inbox가 []). 감사추적이 필요하면 리더가 읽는 즉시 별도 보관할 것.

# 3) 기존 일반 세션의 팀원화: /exit 후 --resume <세션id> + teammate 플래그로 재부팅
#    → 컨텍스트 유지 + 수신·발신 모두 활성 (실측: 재부팅 전 기억을 정확히 회신)
```

규칙: **메시지 `from` 필드는 발신 세션의 배정 학생 캐릭터명**(오케스트레이터가 보내면 그 pane 의 배정 캐릭터명 — 거노 확정, 수신 화면에 그 이름으로 표시). 단 config의 리더 멤버 엔트리 명칭은 `team-lead` 유지 — 하네스에 team-lead 하드코딩 경로(비대화형 드레인 등)가 있다.

함정: ①**inbox 파일명은 슬러그** — `[^a-zA-Z0-9_-] → "-"`라 한글 이름 "수신생"→`---.json`, 같은 팀의 같은 길이 한글 이름은 충돌 → agent-name은 ASCII 작업명 권장 ②teammate 세션 id는 팀+에이전트 조합 결정론 파생 — 같은 조합 재부팅 시 "Session ID already in use". **단 명시적 `--session-id <uuid>`가 파생을 이긴다(실측 2026-07-13: 지정 uuid로 jsonl 생성·폴러 정상 arm·발신 정상)** — kasaterm shim처럼 세션 id를 직접 주면 충돌 클래스 소멸 ③스키마 불일치 항목은 조용히 drop — 위 필드 구성 유지 ④`--parent-session-id`는 붙이지 마라(그 세션에 idle 알림이 새어감) ⑤전부 v2.1.207 바이너리 실측 — 버전 업 시 재검증.

역할 무관 풀 메시(거노 확정 방향): 인박스는 원래 에이전트별 분리라 발신은 누구나(파일 append), **수신만 "teammate 플래그 부팅" 게이트**다. 따라서 오케스트레이터도 `--agent-id team-lead@<팀> --agent-name team-lead --team-name <팀>`(+명시 --session-id)로 부팅하면 리더/팀메이트 구분 없이 전원 양방향 네이티브 — 학생↔학생도 같은 config 등재면 SendMessage로 서로 지목 가능. 오케스트레이터가 플래그 없이 떠 있는 동안은 team-lead.json을 직접 읽거나 파일 감시(Monitor)로 보완.

컬러: 발신 하네스가 부팅 `--agent-color`를 발신 메시지의 `color` 필드에 **자동 스탬프**하고, 수신 렌더는 그 필드 기준(실측: pink 부팅 학생의 요청이 color:pink, red 부팅 학생의 SendMessage가 color:red로 도착). 수동 파일 append 때만 `color`를 직접 넣으면 된다.

리더가 학생 상태를 아는 법: 학생이 턴을 끝내면 하네스가 team-lead 인박스에 idle 알림을 자동 발신한다(실측 2026-07-13) — 오케스트레이터가 team-lead 플래그로 부팅돼 있으면 네이티브로 받고, 아니면 파일에서 읽는다. wake-watch 없이도 이걸로 완료 감지가 가능. **포장 스키마 주의(같은 날 raw 실측)**: 겉 엔트리는 일반 메시지와 같은 `type:"message"`이고, `text` 필드에 `{"type":"idle_notification","from":…,"idleReason":"available","summary":"<그 턴 요약>"}` JSON **문자열**이 담겨 온다 — 파일을 직접 읽는 소비자는 `text`를 파싱해서 idle/실메시지를 구분해야 한다(겉 type만 보면 전부 message). 진짜 보고 메시지는 `text`가 평문이고 `summary`가 겉 레벨에 있다.

#### 패턴 F-3 — 학생 권한 라우팅 (AskUserQuestion은 pane에 안 뜬다)

teammate 학생의 `AskUserQuestion`(및 승인 필요 도구)은 자기 pane에 선택 UI를 그리지 않고 **team-lead에게 permission_request로 라우팅되고 학생은 블록**된다 — bypass permissions여도 마찬가지. 즉 학생은 pane 사용자에게 직접 질문할 수 없다.

**단 이 라우팅은 agent-id 로 꺼진다(07-17 실측, v2.1.212)**: claude 의 포워딩 게이트가 **agent-id가 정확히 `"team-lead"`면 리더로 판정**해 포워딩을 끄고, AskUserQuestion 을 그 pane 에 네이티브 렌더한다(수신 폴러·인박스는 agent-name 기준이라 id 중복 무해 — 로컬 피커+선택+수신 E2E 확인). 패턴 F 권장 스폰이 이 방식(`--agent-id team-lead --agent-name <작업명> --team-name <팀>`)인 이유다 — 학생의 질문이 그 pane 에서 거노에게 직접 뜬다. ((구)shim 자동 트리플도 전원 이 방식이었으나 2026-07-24 제거.) 아래 permission_request 라우팅·응답 절차는 **`<slug>@<팀>` 꼴 id 로 명시 스폰한 학생**(오케스트레이터가 승인을 대신 처리하고 싶을 때)에만 해당한다.

리더(스폰한 세션)의 응답: 학생 inbox에 `type:"message"` 엔트리로, `text`에 아래 JSON을 담아 append. **`from`은 반드시 `"team-lead"`** — 다른 발신자의 permission_response는 폴러가 무시한다.

```json
{"type":"permission_response","request_id":"<요청의 request_id>","subtype":"success"}
```

- 답 내용까지 전달하려면 `"response":{"updated_input":{...원래 input + answers 채움...}}` 포함. 거부는 `subtype:"rejected"` + `"error":"<사유>"`.
- **함정: 학생이 요청을 재시도하면 request_id가 새로 발급** — 항상 team-lead 인박스의 최신 permission_request id로 응답할 것(옛 id는 조용히 무시됨).

운영 정책(거노 확정): **사소한 판단이면 리더가 알아서** 승인/답변하고 진행시켜라. **중요한 결정이면 리더가 자기 AskUserQuestion으로 거노에게 물어본 뒤** 그 답을 permission_response(또는 후속 메시지)로 relay한다. 학생에게는 애초에 "중요 결정은 리더에게 메시지로 물어라"라고 브리핑하는 게 깔끔하다.

---

### 패턴 G — 해산 (`dismiss`)

일이 끝나면 인사말·완료 보고를 주고받지 말고 **그냥 닫는다**:

```bash
kasaterm-cli dismiss %64 %65 %66        # 커밋 안 된 변경이 있으면 그 pane 은 안 닫고 보고
kasaterm-cli dismiss %64 --force        # 회수 끝났으면
```

출력은 JSON이 아니라 한 줄씩(`closed %64 프라나 — mc-dashboard` / `kept %66 아리스 — …: 커밋 안 된 변경 3개`). pane 을 닫아도 워크트리 파일은 그대로 남는다(실측) — 막는 것은 "어느 pane 이 뭘 만지고 있었는지"가 board 와 함께 사라지기 전에 회수하라는 뜻이다.

**대상은 항상 명시한다.** `--all` 은 없다 — 화면에는 그 작업과 무관한 pane(거노 본인 작업, 다른 방)이 늘 함께 떠 있고, 한 번의 오작동이 남의 세션을 통째로 날린다. 스폰한 쪽은 자기가 띄운 id를 안다.

**「마무리하겠습니다」·「수고했다」·완료 통지는 전부 없어도 되는 왕복이다** — 무엇이 끝났는지는 커밋과 공유 태스크 목록과 dismiss 출력이 말한다.

### 패턴 H — 공유 태스크 목록

태스크 목록은 **팀 이름 단위**(`~/.claude/tasks/<팀>/`)고 팀은 방(cwd) 단위라, 같은 방 pane 은 **한 목록을 공유한다**(거노 확정 2026-08-04: 공유 유지). 인박스(SendMessage)와 같은 팀 이름을 쓰므로 둘은 따로 못 끈다 — 태스크를 pane 별로 가르려면 SendMessage 도 같이 죽는다.

이게 진행 보고를 대신한다. 말로 알리지 말고 목록을 갱신해라:
- 시작 시 `TaskUpdate`로 `in_progress`, 끝나면 `completed`. 안 하면 남이 같은 걸 또 잡는다.
- `owner`는 **여럿이 한 목록을 나눠 가질 때만** 자기 이름으로. 혼자 도는 pane이면 `owner: ""`를 status와 **함께 매번** 준다.
- **남의 owner가 붙은 태스크는 상태도 바꾸지 말고 지우지도 마라** — 그 사람이 아직 도는 중이다.
- 오케스트레이터는 배분 전에 `TaskList`로 이미 잡힌 것을 보고 겹치지 않게 나눈다.

**자기 배정 알림이 되돌아오는 이유와 끄는 법**(전수 실측 2026-08-04, claude 2.1.221). 하네스는 `owner`가 **바뀔 때** 그 owner 인박스로 `task_assignment`를 쏘는데 조건이 `if (g.owner && 팀모드)` 뿐 — **보낸 사람이 받는 사람인지 검사하지 않는다**. 그래서 자기 태스크를 자기가 잡아도 「Task #N assigned by 나」가 한 턴 뒤 자기에게 도착한다.

| 조작 | 알림 |
|---|---|
| `TaskCreate` 만 | ✕ |
| `status:"in_progress"` (owner 인자 없음, 주인 없음) | **○** — 하네스가 자동으로 내 이름을 박고 쏜다 |
| `status:"in_progress"` + `owner:""` | ✕ ← **이게 끄는 법** |
| `owner:""` 로 둔 뒤, owner 없이 다시 `in_progress` | **○** ⚠️ 빈 문자열이 falsy라 자동 배정이 되살아난다 — 그래서 매번 함께 줘야 한다 |
| `owner:"<내 이름>"` (주인 없던 태스크) | **○** |
| `owner:"<내 이름>"` (이미 내 이름) | ✕ (바뀐 필드가 없다) |
| `status:"completed"` / `"deleted"` | ✕ — **완료는 아무것도 안 쏜다**(`task_completed`는 렌더 코드만 있고 발신 코드가 없다) |

완료 즈음에 알림이 뜨는 건 완료 때문이 아니라, **잡을 때 쏜 것이 한 턴 늦게 배달**되기 때문이다. 인박스에서 지워 없애려 들지 마라 — 하네스의 write와 레이스가 나 남의 메시지를 유실시킨다.

### 패턴 I — 질문은 각자 거노께

**학생끼리 상의하지 마라**(거노 지시 2026-08-04). 「이 방향 맞나」·「이거 어떻게 할까」는 전부 그 학생 pane 에서 `AskUserQuestion`으로 거노께 직접 뜬다 — shim이 `--agent-id team-lead`로 부팅하므로 승인 포워딩 게이트가 꺼져 있어 네이티브로 렌더된다(F-3).

학생끼리 주고받는 상의는 거노 눈에 안 보이는 곳에서 방향이 정해지고, 왕복이 두 배가 되고, 물어본 쪽도 결국 추측으로 답한다. pane 간 SendMessage는 **질문이 아니라 통보**로 쓴다 — 「이 파일 내가 만진다」, 「이거 끝났으니 이어서」.

## 2) 긴 잡 사이클 — kasaterm pane에서 빌드·dev server·배포

raw tmux의 `tmux send-keys` 대신 `kasaterm-cli send --surface`로 같은 패턴을 돌린다. 핵심 4요소는 동일.

### 언제 쓰는가

- 빌드/배포/마이그레이션: `cargo build --release`, `make deploy`, DB 마이그레이션
- dev server: `npm run dev`, `cargo run` (종료 마커 안 찍히니 폴링 X, 시작만 자동화)
- 장시간 테스트: pytest, integration test

단순 1줄 명령은 그냥 `Bash` 동기 호출이 빠름.

### 풀 사이클 (5단계)

**1) pane 생성 + 이름·색**
```bash
NEW=$(kasaterm-cli split right | python3 -c 'import sys,json;print(json.load(sys.stdin)["result"]["surface"]["id"])')
kasaterm-cli rename "$NEW" "build"
kasaterm-cli color  "$NEW" "#d29922"
```

**2) 명령 박기 (표준 패턴)**
```bash
TASK=cargo-release      # /tmp/<task>.log 파일명
kasaterm-cli send --surface "$NEW" \
  $"cd /path/to/kasaterm && cargo build --release -p kasaterm 2>&1 | tee /tmp/$TASK.log; echo '---DONE---' >> /tmp/$TASK.log; exit\n"
```

핵심 4요소(빠뜨리면 사이클 깨짐):
1. `2>&1 | tee /tmp/<task>.log` — stderr 합치고 로그 파일에 동시 기록
2. `echo '---DONE---' >> /tmp/<task>.log` — 종료 마커. 반드시 로그 파일에도
3. `; exit` — pane 자동 종료. 좀비 안 남게
4. `\n` (ANSI-C 따옴표) — 엔터 전달

**3) 완료 감시 (Monitor 도구)**
```
Monitor({
  description: "<TASK> 완료 감시",
  timeout_ms: 1800000,
  persistent: false,
  command: "until grep -q '^---DONE---$' /tmp/<TASK>.log 2>/dev/null; do sleep 5; done; echo done"
})
```

빌드 도중 에러를 즉시 알고 싶으면:
```
tail -F /tmp/<TASK>.log | grep --line-buffered -E '---DONE---|ERROR|FAIL|error\\[|Traceback'
```

dev server처럼 종료 안 하는 잡엔 Monitor X — 시작 후 사용자에게 "서버 떴다, pane $NEW에서 돌고 있다" 보고하고 끝.

**4) 완료 알림 수신 후**
- tail 30줄로 결과 요약. 로그 전체를 Read하지 말 것(컨텍스트 폭발).
- 에러 의심되면 `grep -E "error\[|ERROR|FAIL" /tmp/$TASK.log | head -20`.

**5) 정리**
```bash
rm /tmp/$TASK.log
# pane은 `; exit`으로 닫혔어야 함. 잔존이면 `kasaterm-cli close $NEW`.
find /tmp -maxdepth 1 -name "*.log" -mtime +7 -delete 2>/dev/null
```

### 안티 패턴 (하지 마라)

| 안 됨 | 왜 |
|---|---|
| `; exec zsh` 끝맺음 | pane 안 닫히고 좀비로 남음 |
| `send --surface ... "cmd\n"` 큰따옴표 | `\n`이 문자 그대로. **ANSI-C 따옴표 `$'...\n'`** 또는 `$"...\n"` |
| 로그 파일 없이 stdout만 | pane 닫히면 출력 사라짐. 사후 회수 불가 |
| 같은 task 이름 두 번 | 로그 파일 충돌. timestamp/increment 붙여야 |
| destructive(`make deploy` 등) 사용자 승인 없이 | CLAUDE.md 위반. 명시적 승인 먼저 |

---

## 3) kasaterm UI 자체검증 사이클 — 코드 고치면 너가 직접 확인

UI 변경 후 사용자에게 "테스트해보세요" 떠넘기지 말 것. **너가** 빌드→스폰→스크린샷→읽고 어색한 부분 짚어내고 다시 고친다. 이 사이클은 kasaterm 셸 밖에서도 동작한다.

### 환경 변수 카탈로그 (헤드리스 verify 핸들)

| env | 기본 | 효과 |
|---|---|---|
| `KASATERM_AUTOSPLIT` | (없음) | `h`/`v`/`hv` … 문자별 한 번씩 분할 (헤더·탭바 노출) |
| `KASATERM_AUTOSPLIT_MS` | 2500 | split 시작 지연 |
| `KASATERM_AUTOTABS` | 0 | 활성 pane에 N개 dummy 탭 주입 ("탭 2", "탭 3" …) — 인-pane 탭바 테스트용 |
| `KASATERM_AUTOTABS_MS` | 3200 | tab 주입 지연 (autosplit 이후) |
| `KASATERM_AUTOWINDOWS` | 0 | N개 추가 window 스폰 (사이드바 멀티-윈도우 테스트) |
| `KASATERM_AUTOTOGGLE_SIDEBAR_MS` | (없음) | 사이드바 토글 후 캡처 |
| `KASATERM_AUTOCAPTURE_MS` | (없음) | N ms 후 PNG 캡처 |
| `KASATERM_AUTOCAPTURE_PATH` | `$TMPDIR/kasaterm.png` | 캡처 저장 경로 |
| `KASATERM_AUTOQUIT_MS` | (없음) | N ms 후 깨끗하게 종료(저장됨). 캡처 전용 run엔 **설정하지 말 것** — 종료 시 `session.json`이 테스트 레이아웃으로 덮어쓰임 |
| `KASATERM_AUTOSEND` / `_MS` | (없음) | 활성 pane에 키 자동 전송 (IME 조합 경로는 재현 X) |
| `KASATERM_IME_DEBUG=1` | — | 키 코드포인트 로깅 |

캡처 region은 **winit 창 좌표**에서 자동 계산해 `screencapture -R`로 전달한다(예전엔 osascript bounds가 None 떨어져 전체화면으로 새던 버그가 있었음 — `app/kasaterm/src/main.rs::schedule_autocapture`에서 winit 기반으로 고정).

### 풀 사이클 (UI 검증)

**0) 호스트 보호 — 절대 죽이지 마라**

사용자가 켠 kasaterm은 보통 `.app`(`~/Applications/kasaterm.app/Contents/MacOS/kasaterm`). pkill 패턴은 `target/debug|release/kasaterm`만 매칭하므로 호스트는 살아 있다. 확인:
```bash
ps aux | grep -iE "kasaterm" | grep -v grep | grep -oE "/[^ ]*kasaterm[^ ]*"
```
`.app` 경로면 호스트, `target/...`면 옛 테스트 인스턴스(죽여도 됨).

**1) 빌드**
```bash
cd /path/to/kasaterm
pkill -f "target/debug/kasaterm" 2>/dev/null
pkill -f "target/release/kasaterm" 2>/dev/null
sleep 1
cargo build -p kasaterm 2>&1 | tail -3
```

체감(스크롤·입력 지연) 테스트는 반드시 `--release`. 시각만 확인할 거면 debug면 충분.

**2) 클린 세션 (선택 — 인-pane 탭/2-pane 등 깔끔한 화면이 필요할 때)**

테스트 인스턴스도 `~/.config/kasaterm/session.json`을 읽고 쓴다. 사용자의 진짜 세션이 4-pane이면 테스트도 4-pane 시작해서 좁아진다. 백업·비우기:
```bash
SJSON="$HOME/.config/kasaterm/session.json"
[ -f "$SJSON" ] && cp "$SJSON" /tmp/kasaterm-session-backup.json && rm "$SJSON"
```
호스트는 종료 시에만 쓰므로 살아 있는 동안 백업해도 안전. **테스트 후 반드시 복원**:
```bash
[ -f /tmp/kasaterm-session-backup.json ] && \
  mkdir -p "$(dirname "$SJSON")" && \
  cp /tmp/kasaterm-session-backup.json "$SJSON"
```

**3) 스폰 + 캡처**

캡처 전용 run엔 `AUTOQUIT_MS`를 빼고 캡처 직후 `kill -9` — 종료 핸들러가 test 레이아웃을 `session.json`에 박는 걸 막는다.

```bash
rm -f /tmp/kasaterm-shot.png
KASATERM_AUTOSPLIT=h KASATERM_AUTOSPLIT_MS=1800 \
KASATERM_AUTOTABS=2 KASATERM_AUTOTABS_MS=3000 \
KASATERM_AUTOCAPTURE_MS=4500 KASATERM_AUTOCAPTURE_PATH=/tmp/kasaterm-shot.png \
./target/debug/kasaterm > /tmp/kasaterm-run.log 2>&1 &
TPID=$!
until [ -f /tmp/kasaterm-shot.png ]; do sleep 0.5; done
sleep 0.5
kill -9 $TPID 2>/dev/null
```

타이밍 가이드: 캡처는 마지막 mutation(autosplit·autotabs)보다 ≥1초 뒤. release 빌드는 첫 프레임이 더 빠르니 같은 값도 안전.

**4) Read tool로 즉시 보기**
```
Read("/tmp/kasaterm-shot.png")
```
`screencapture`는 Mac 권한으로 막혀서 안 됨 — 무조건 내장 autocapture 사용.

**5) 어색한 부분 크롭·확대 (선택)**

전체 캡처(예: 2200×1720)는 모델에 작게 보일 수 있다. 헤더·탭만 자세히 봐야 하면 상단을 크롭·확대:
```python
python3 -c "
from PIL import Image
im = Image.open('/tmp/kasaterm-shot.png')
top = im.crop((0,0,im.width,int(im.height*0.12)))
top = top.resize((int(top.width*1.6), int(top.height*1.6)), Image.LANCZOS)
top.save('/tmp/kasaterm-header.png')
"
```
그리고 `Read("/tmp/kasaterm-header.png")`.

**6) 시각 확인 + 다음 액션**

스크린샷 본 후 어색한 부분 **너의 판단으로** 짚어내고 수정. "어때보여요?" 묻지 말 것. 사용자가 결정해야 할 디자인 선택지만 AskUserQuestion으로 묻는다(예: 배경 톤, 탭 영역 크기 방향).

**7) 세션 복원 (위 2단계를 했다면)**
```bash
[ -f /tmp/kasaterm-session-backup.json ] && \
  mkdir -p "$(dirname "$SJSON")" && \
  cp /tmp/kasaterm-session-backup.json "$SJSON"
```

### 검증 한계 — 못 하는 것

- **마우스 클릭 시뮬레이션 없음** — 인-pane 탭 클릭 전환, 탭 드래그 재정렬, 헤더 드래그앤드롭(pane 이동), 분할 seam 드래그 같은 마우스 인터랙션은 헤드리스 재현 불가. 코드 로직만 리뷰하고 사용자에게 1줄 수동 검증 요청.
- **IME 조합 경로 못 재현** — `AUTOSEND`는 `send_bytes` 직접 주입이라 한글 첫 jamo·preedit 버그는 사용자가 직접 타이핑해야 한다(`KASATERM_IME_DEBUG=1`로 키 로그).
- **체감 지연(release 전용)** — debug 빌드는 원래 버벅임. "느리다" 판단은 release/`.app`로만.

### 사이클 결과를 사용자에게 보여주기

검증 끝났으면 패턴 D로 결과 스크린샷을 메인 창 안에 띄운다 (사용자가 같은 화면에서 본다):
```bash
imgopen /tmp/kasaterm-shot.png
```
또는 셸 밖이면 위의 `/open-image` 엔드포인트.

---

## 4) 팀 모드 (TeamCreate로 다중 pane 협업)

단일 잡은 위 사이클로 충분. **여러 잡을 병렬 + 역할 분리**가 필요하면 `TeamCreate`로 패널 분할.

### 언제 팀 모드를 쓰는가

tmux 안이고 아래 중 하나라도 해당:
- 독립된 3개+ 파일 동시 수정 (서로 영향 없는 모듈)
- 서로 다른 레이어 동시 변경 (백엔드 + 프론트엔드, DB + API)
- 탐색/구현/검증처럼 역할이 명확히 분리되는 흐름
- 디자인 + 구현처럼 도메인이 다른 작업

tmux 밖이면 `Agent` 툴 병렬 호출(단일 메시지에 여러 tool_use 블록).

### 기본 3패널

| 패널 | 역할 | 도구 제약 |
|---|---|---|
| **scout** (탐색) | 코드 읽기 / grep / 구조 파악 | Read/Grep/Glob. **Edit/Write 금지** |
| **builder** (구현) | 실제 코드 수정 | Read/Edit/Write. 파괴적 Bash는 리드 승인 |
| **verify** (검증) | 테스트/타입체크/UI 확인/로그 감시 | Bash + Read. **Edit 금지** |

리뷰는 별도 4번째 패널 → 단, **구현 완료 후**.

### Agent 스폰 — name 명시 + **순차 스폰 (race 회피)**

```
Agent({ team_name: "...", name: "scout", subagent_type: "general-purpose", ... })
```
`name`을 박지 않으면 "general-purpose"로 표시돼 분간 불가. 짧고 명확히: `scout` / `builder` / `verify` / `reviewer`.

**한 메시지에 Agent 여러 개 동시 호출 금지.** TeamCreate가 띄운 백그라운드 tmux 세션에 pane을 만들 때 race가 발생해서 두 번째 이후가 `tmuxPaneId: ""`(빈 문자열)인 좀비로 spawn된다. 좀비는 SendMessage·shutdown 모두 무응답이라 청소가 까다롭다.

**규칙**:
1. Agent 호출은 1번에 1명씩 순차.
2. 각 spawn 직후 `tmuxPaneId` 할당 확인:
   ```bash
   jq -r '.members[] | "\(.name) → tmuxPaneId=\(.tmuxPaneId|@json) active=\(.isActive)"' \
     ~/.claude/teams/<team-name>/config.json
   ```
3. 빈 문자열이면 즉시 → "[좀비 청소 절차](#좀비-청소-절차)"로.
4. 정상이면 다음 멤버 spawn.

### 팀 pane border — 오렌지

```bash
for ROLE in scout builder verify reviewer; do
  PANE=$(kasaterm-cli list surfaces | python3 -c "import sys,json,re;rs=json.load(sys.stdin)['result']['surfaces'];print(next((s['id'] for s in rs if re.search(r'$ROLE', s.get('title') or '')), ''))")
  [ -n "$PANE" ] && kasaterm-cli color "$PANE" "#ff8800"
done
```

### 리드의 역할 (가장 중요)

팀 가동 중 리드는 **SendMessage로 지시만**:
- 코드 수정 (Edit/Write) 금지 → builder
- 프로세스 재시작 / 로그 감시 금지 → verify
- 탐색 / grep 금지 → scout

리드 일: 사용자 대화·의사결정, 패널 간 메시지 라우팅, 충돌 중재.

**"내가 한 번에 빨리 끝낼 수 있어" 충동 억제** — 리드가 손대면 패널 작업과 충돌하고 컨텍스트 분기.

**예외**: 팀이 같은 작업에서 2번 이상 실패하면 진단용 `Read`/`Bash`까지 허용. `Edit`는 여전히 금지.

### 통신

```
scout → SendMessage(to=builder): "app/kasaterm/src/main.rs:6788-6900 에 탭 헤더 렌더 루프."
builder → SendMessage(to=verify): "main.rs 수정 완료. AUTOSPLIT=h AUTOTABS=2로 스샷 찍어줘."
verify → SendMessage(to=lead): "/tmp/kasaterm-shot.png 캡처. active 탭만 × 확인. 좌측 unfocused pane 띠 흰색 OK."
```

모든 메시지는 **구체 파일 경로**. 완료 신호는 **다음 패널이 즉시 행동 가능한 형태**.

### 안티 패턴

| 안 됨 | 왜 |
|---|---|
| Agent 스폰 시 `name` 생략 | "general-purpose"로 표시되어 분간 불가 |
| 한 메시지에 Agent 여러 개 병렬 호출 | tmux pane race → 좀비. 1명씩 순차 |
| 팀 패널 border 색 안 칠함 | 일반 잡 pane과 섞임 |
| 리드가 Edit/Write 직접 | builder와 컨텍스트 분기 |
| 패널 역할 안 정함 | 세 패널이 같은 파일 동시 수정 → conflict |
| 1인 단순 작업에 팀 모드 | 오버헤드 손해 |

### 좀비 청소 절차

좀비 = `~/.claude/teams/<team>/config.json`에 멤버 엔트리는 있는데 `tmuxPaneId`가 빈 문자열이거나 `isActive: false`인데도 안 사라지는 멤버. spawn 도중 tmux race로 pane을 못 받은 경우가 전형.

**증상**:
- `SendMessage`는 success로 응답하는데 실제 메시지 처리는 안 됨
- `shutdown_request` 보내도 `shutdown_approved` 응답 안 옴
- `TeamDelete`가 `Cannot cleanup team with N active member(s): <좀비이름>` 거부

**1단계 — shutdown 시도 (한 번)**:
```bash
# SendMessage로 shutdown_request 1회. 응답 안 오면 좀비 확정.
# (메인 turn에서 SendMessage 툴로 보냄, 여기선 진단만)
TEAM=<team-name>
sleep 5
jq -r '.members[] | select(.name == "<좀비이름>") | "tmuxPaneId=\(.tmuxPaneId|@json) active=\(.isActive)"' \
  ~/.claude/teams/$TEAM/config.json
```

**2단계 — config.json에서 강제 제거** (사용자에게 1줄 알리고 진행):
```bash
TEAM=<team-name>
ZOMBIE=<좀비이름>
CFG=~/.claude/teams/$TEAM/config.json
cp "$CFG" "$CFG.bak.$(date +%s)"   # 안전 백업
jq --arg name "$ZOMBIE" '.members |= map(select(.name != $name))' "$CFG" > "$CFG.tmp" && mv "$CFG.tmp" "$CFG"
```

`team-lead`(자기 자신)와 정상 active 멤버는 건드리지 말 것 — 좀비 이름만 정확히 박아서 제거.

**3단계 — TeamDelete 재시도**:
- 다른 멤버는 정상 shutdown 절차로 종료 후 `TeamDelete`.
- 좀비만 있고 다른 멤버 다 끝났으면 위 2단계 후 바로 `TeamDelete`.

**좀비 잔여 inbox/task 정리** (필요 시):
```bash
TEAM=<team-name>
rm -rf ~/.claude/teams/$TEAM/inboxes/<좀비이름>
# task ownership 정리는 TaskList/TaskUpdate로 (좀비 owned 태스크 → unassigned)
```

**왜 자동 청소 OK인가**: 좀비는 정의상 동작 안 하는 죽은 엔트리. 백업(`.bak.<ts>`) 만들고 진행하므로 복원 가능. 사용자에게는 "좀비 X 발견, 자동 청소 진행" 1줄만 알리고 묻지 않음. 단, `team-lead` 또는 `isActive: true` + `tmuxPaneId != ""` 멤버는 절대 제거 금지 — 그건 살아있는 멤버.

---

## 5) pane 협업 — board(현황) + SendMessage(소통) + wake-watch(대기)

§4 팀 모드가 **TeamCreate 위계**(리드↔팀원)라면, 이건 **위계 없는 peer 협업**이다. 떠 있는 pane들끼리 — claude든 codex·antigravity든 — 소통하고 충돌을 피한다. `KASATERM_PANE_ID`가 비어 있으면 형제 pane이 없는 것이니 비적용. 네 축이다:

- **board** — 누가 뭘 하나. `kasaterm-cli board`로 **직접 조회**한다(옛 `kasaterm-board-context.py` 자동주입은 폐기 — settings 미등록). 충돌 회피·합류 판단에 쓴다.
- **SendMessage** — **pane 간 소통의 기본**(2026-08-04 shim 자동 트리플 복원 후). `to:` 는 상대의 `$KASATERM_AGENT`. 인박스 네이티브 주입이라 상대 입력창을 안 건드리고 발신자 이름·색으로 렌더된다. **유휴로 프롬프트만 떠 있는 pane 도 5초 안에 읽는다**(실측 2026-08-04 — 넣자마자 드레인+턴 시작). 깨우려고 tell 을 덧댈 필요 없다.
  - ⚠️ **팀이 다르면 조용히 사라진다.** 팀은 방(cwd) 단위라 **다른 레포에 띄운 학생은 다른 팀**이다. 이때 SendMessage 는 실패하지 않고 *내 팀* 인박스에 그 이름의 고아 파일을 만들 뿐이며, 상대는 자기 팀 파일만 본다. 실사고(2026-08-04): mission-control 방에서 demo-showcase 방의 코하루에게 브리프 → 무응답. 내 팀 인박스 `koharu-p7.json` 1121B, 코하루가 읽는 파일은 `[]`.
  - 보내기 전에 `kasaterm-cli board` 로 상대의 **`team` 이 내 `$KASATERM_TEAM` 과 같은지** 확인해라 — board 가 `agent`·`team` 을 pane 프로세스에서 실측해 싣는다(2026-08-04 추가). 이름을 `<슬러그>-p<번호>` 규칙으로 짐작하지 마라: 어긋나도 오류가 안 난다.
  - **팀이 다르면 tell 로 지시하고, 브리프에 「보고는 하지 마라」고 적어라.** 반대 방향도 안 닿으므로 그 학생의 보고는 영영 오지 않는다 — 결과는 커밋과 `peek`·`transcript` 로 오케스트레이터가 직접 확인한다.
- **tell** — **그 외 전부의 기본 채널**: 스폰 관계 없는 같은 방 pane·다른 방·bg 세션·비-claude(codex·antigravity·셸)·학생→오케스트레이터 보고·`/model` 같은 슬래시 명령 주입·idle claude 즉시 깨우기. 2026-07-24부터 받는 pane에 발신 학생 프사+학생색으로 렌더되어 거노 입력과 구분된다. 서버가 발신자를 기록해 웹뷰 대화에도 발신 학생 이름으로 뜬다.
- **wait** — 동료 작업이 끝나길 기다릴 때. `tell`로 깨우거나 `board`를 반복 조회하지 말고 `wake-watch`를 background로 띄운다(아래).

### board — 각 pane이 뭘 하는지 (직접 조회)

`kasaterm-bind-transcript.sh`(SessionStart hook)가 각 pane의 transcript를 소켓에 등록한다. board(형제 활동)는 `kasaterm-cli board`로 **직접 조회**한다 — 옛 `kasaterm-board-context.py` 자동주입(매 턴 프롬프트 주입)은 폐기됐다(settings 미등록). 충돌 회피가 필요할 때 명시적으로 부른다.

- **사용자(거노)**: 상단 **보기 메뉴 → "board 패널"** 로 전 pane 현황을 본다(읽기 전용).
- **claude(너)**: `kasaterm-cli board`로 현황을 조회한다. 내가 만질 파일을 다른 pane이 잡고 있으면 충돌 회피(같은 문제면 합류, 빌드 중이면 `peek %N`로 보고 대기).

### wait — 동료 완료 기다리기 (wake-watch, background)

동료 pane의 한 턴이 끝나길 기다려야 할 때(예: 내 작업이 그 결과에 의존), **`tell`로 반복해 깨우거나 `board`를 폴링하지 마라** — 입력창을 더럽히고 컨텍스트만 태운다. `wake-watch`를 background task로 띄운다:

```bash
kasaterm-cli wake-watch %3          # %3이 한 턴 끝내면 스스로 종료
```

- Bash 도구의 `run_in_background: true`로 실행(또는 명령 끝에 `&`). 동료가 한 턴을 마치면 이 명령이 종료되고 시스템이 너를 자동으로 wake(task-notification)한다.
- 깨어나면 `board`/`peek %3`/`transcript %3`로 결과를 확인하고 이어간다.
- 상대 surface_id는 `kasaterm-cli board`로 확인. `[interval_s]`(폴링 주기)·`--timeout <s>`(최대 대기) 선택.

여러 pane 상태 변화를 실시간으로 흘려보고 싶으면(특정 완료 대기가 아니라) `board-watch [interval_s]`를 Monitor에 먹인다 — 변경된 pane 상태를 1줄/변경으로 스트림.

### tell — 폴백·TUI 조작 채널 (send+제출)

```bash
kasaterm-cli tell %3 "socket.rs 동결 해제 — 이어서 진행해"
```

`tell`은 대상 PTY에 텍스트 주입 후 `\r`로 제출 — idle claude를 새 user turn으로 깨운다. focus는 안 바뀐다. **SendMessage는 본인이 스폰한 학생에게만, 그 외 전부 tell이 기본**(2026-07-24 거노 확정). 발신 pane의 `$KASATERM_CHARACTER` 마커가 자동 프리픽스되어 받는 pane에 발신 학생 프사+학생색으로 렌더된다(2026-07-24). ⚠️ **그러니 본문에 발신자 이름을 직접 쓰지 말 것** — 「아로나: 확인했어요」가 아니라 「확인했어요」. 자동 마커 위에 한 번 더 찍혀 중복으로 보인다(2026-08-02 거노 지적). kasaterm-cli가 발신 메타(from_pane+plain)를 동봉하고 서버가 방 기준 `messages.jsonl`에 기록해, 웹뷰 대화에도 발신 학생 이름 버블로 뜬다(2026-07-12).

- **상대가 working/선택지 대기면 입력창에 큐잉**되고 즉시 처리 안 된다. 급한 게 아니면 `wake-watch`로 idle을 기다렸다 tell — 브리프 여러 건을 working 상대에게 연달아 쏘지 말 것.
- tell 텍스트는 **개행 없는 한 줄**로(개행=조기 제출).
- `send`(=`surface.send_text`)는 `\r` 없이 글자만 남는다 — 입력창에 텍스트만 걸린 채 **제출 안 됨**(실측 2026-07-01). `send`는 오직 셸 명령 주입(개행 직접 포함)용, 동료에게 보내는 텍스트는 무조건 `tell`.
- (구)`kasacollab msg`는 내부에서 tell을 타는 별칭이 됐다(2026-07-12) — 새 자동화엔 tell을 직접 써라. `kasacollab task add/list`(작업 분담 선언)는 별개 기능으로 유지.

### 함정

| 안 됨 | 왜 |
|---|---|
| 다른 방(cwd) pane에 SendMessage | 팀명이 방 단위라 인박스가 갈린다 — 안 닿으면 tell 로 |
| working 상대에게 브리프 연발 tell | 입력창 큐잉·선택지 오염. 브리프는 SendMessage(인박스 주입 — 상대 턴 안 깨뜨림), tell 밖에 없는 상대면 board-watch로 idle 기다렸다 1건 |
| inbox(`kasacollab inbox`)를 소통 채널로 설계 | 모모톡/inbox UI는 폐기(2026-07-12, board tell-피드로 대체). msg는 tell 별칭일 뿐 |
| `send`로 깨우려 함 | `\r` 없음 → 글자만. idle 깨우기는 `tell` |
| `tell`에 surface_id 생략 | 항상 `<surface_id> <text>`. 자기 자신엔 안 씀 |
| board가 비어 보임 | 미bind이거나, **소켓 탈취**(claude pane에서 `cargo run`이 메인 .app 소켓 가로챔 — 2026-06-08 수정). 인스턴스 난립 의심 |
| board status 단독 신뢰 | `agents --json` 2s 캐시 지연으로 생성 중이 idle로 뜰 수 있음 — wake-watch 헛울림 포함, 판단 전 `peek`로 실화면 확인 |

---

## 주의

- **`close`는 셸 종료.** 사용자가 작업 중일 수 있으니, 본인이 띄운 모니터 pane이 아니면 함부로 닫지 않는다.
- pane id는 split/close에 따라 바뀐다. 연속 작업 전에 `list surfaces`로 재확인.
- 셸 밖에선 socket을 못 찾는다 — 1·2번은 전제 점검 먼저, 3번은 셸 무관.
- 자체검증 후 `session.json` 백업 복원 잊지 말 것 — 빠뜨리면 사용자가 다음에 빈 kasaterm을 보게 된다.

## 부록 A — MCP 도구 카탈로그

`mcp__kasaspace__*`. kasaterm-cli과 1:1 매핑되는 도구는 같은 줄에 표시. **자동화는 kasaterm-cli 우선**, 사용자가 명시적으로 시킨 일회성 GUI 액션은 MCP.

| MCP 도구 | 인자 | kasaterm-cli 동치 | 비고 |
|---|---|---|---|
| `kasaspace_list` | — | `kasaterm-cli list surfaces` | surface 목록 + id |
| `kasaspace_split` | `direction` (left/right/up/down) | `kasaterm-cli split <dir>` | 현재 focused pane 기준 분할 |
| `kasaspace_focus` | `surface_id` | `kasaterm-cli focus <id>` | 포커스 이동 |
| `kasaspace_close` | `surface_id` | `kasaterm-cli close <id>` | pane 종료. 사용자 작업 중일 수 있으니 신중 |
| `kasaspace_rename` | `surface_id`, `title` | `kasaterm-cli rename <id> <제목>` | 헤더 제목 |
| `kasaspace_set_color` | `surface_id`, `color` | `kasaterm-cli color <id> <#rgb>` | 헤더 accent |
| `kasaspace_swap` | `a`, `b` | `kasaterm-cli swap <a> <b>` | 위치 교환(내용 유지) |
| `kasaspace_send` | `text`, `[surface_id]` | `kasaterm-cli send --surface <id> <text>` | 텍스트 전송. 엔터 필요하면 `text`에 `\n` 포함 |
| `kasaspace_send_key` | `key`(enter/tab/escape/…), `[surface_id]` | `kasaterm-cli key <id> …` | 명명 키 전송 |
| `kasaspace_run_job` | `command`, `[title]`, `[color]`, `[direction]`, `[auto_close]` | (없음 — 직접 split+rename+color+send 조합) | **사용자 옆에서 실시간 진행 보여주는 잡 전용**. 출력 스트림은 모델한테 안 옴(pane 안에만). 빌드/dev/배포 사용자 시연용 |
| `kasaspace_workspace_current` | — | (tmux session 정보) | 현재 워크스페이스 |
| `kasaspace_workspace_list` | — | (tmux session 정보) | 워크스페이스 전수 |

**`kasaspace_run_job` 주의**: 사용자가 "옆에 띄워서 보여줘" 요청하면 이게 가장 깔끔(타이틀+색+자동분할+자동종료 옵션 한 번에). 단 출력이 셸로 안 오니까 **로그 회수 필요한 자동화엔 부적합** — 그건 `kasaterm-cli send` + `tee /tmp/<task>.log` + Monitor.

**언제 MCP를 쓸지 결정 흐름**:
1. 사용자가 "지금 이거 띄워" 한 번 시킴? → MCP (`kasaspace_run_job`, `kasaspace_send` 등)
2. 자동 사이클(빌드→로그→완료알림)의 일부? → bash
3. config.json 검사·좀비 제거·jq 파이프? → bash (MCP에 없음)
4. 그냥 split·rename·color 한두 번? → 둘 다 OK, 흐름에 자연스러운 쪽