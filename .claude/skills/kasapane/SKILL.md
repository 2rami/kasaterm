---
name: kasapane
description: kasaterm/kasaspace pane을 다루고, 그 안에서 긴 잡(빌드·dev server·배포)을 풀 사이클로 돌리고, 작업 결과(이미지·마크다운)를 창 안 pane에 띄우고, 여러 pane의 claude(또는 codex·antigravity 등)가 같은 레포를 동시에 만질 때 충돌 없이 협업하고(board 패널·tell·peek), kasaterm 자체 UI를 빌드→스폰→스크린샷→확인 사이클로 자체검증하고, TeamCreate로 띄운 팀원 pane의 race·좀비를 청소한다. 사용자가 "모니터 띄워줘", "로그 따로 보게 해줘", "pane 쪼개/이름/색", "dev 서버 옆에 띄워", "이미지/마크다운 띄워줘", "다른/옆 pane 뭐하는지", "협업", "충돌 피해", "같이 작업", "팀원 pane 정리", "팀 좀비 청소", "빌드 돌려줘", "kasaterm 화면 확인해줘", "스크린샷 찍어서 봐줘" 같은 요청, 또는 멀티 pane 환경(KASATERM_PANE_ID env 존재)에서 코드 작업을 시작하거나 작업 중 만든 스샷·문서를 사용자 화면에 보여줄 때 사용. raw tmux(비-kasaterm) 컨텍스트는 tmux-pane-job 스킬로.
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

## 1) Pane 제어 — kasaterm-cli 명령 레퍼런스

모든 명령은 JSON으로 응답한다(`{"ok":true,...}`). pane id는 `%0`, `%1` … 형식.

| 명령 | 동작 |
|---|---|
| `kasaterm-cli list surfaces` | 현재 pane 목록 + id |
| `kasaterm-cli split <left\|right\|up\|down>` | 현재 pane을 해당 방향으로 분할. 새 pane id 반환 |
| `kasaterm-cli focus <id>` | 포커스 이동 |
| `kasaterm-cli close <id>` | pane 닫기 (셸 종료) |
| `kasaterm-cli rename <id> <제목>` | 헤더 제목 |
| `kasaterm-cli color <id> <#rrggbb>` | 헤더 accent 색상 |
| `kasaterm-cli swap <a> <b>` | 두 pane 위치 교환(내용 유지) |
| `kasaterm-cli send --surface <id> <텍스트>` | 특정 pane에 텍스트 입력(제출 안 함) |
| `kasaterm-cli tell <id> <텍스트>` | 특정 pane에 보내기+제출 — idle claude 깨움 (협업, §5) |
| `kasaterm-cli key <id> …` | 특정 pane에 키 전송 |
| `kasaterm-cli board` | 모든 pane이 뭘 왜 하는지 (협업, §5) |
| `kasaterm-cli announce <의도> [status]` | 내 pane 작업 등록 (협업, §5) |
| `kasaterm-cli peek <id> [lines]` | 다른 pane 화면 텍스트 읽기 (협업, §5) |
| `imgopen <파일.png>` | 이미지를 새 pane으로 띄움 (맞춤↔원본 토글) |
| `mdopen <파일.md>` | 마크다운을 노션풍 렌더 pane (Render/Raw) |

split이 반환하는 JSON의 `result.surface.id`가 새 pane id다.

```bash
NEW=$(kasaterm-cli split right | python3 -c 'import sys,json;print(json.load(sys.stdin)["result"]["surface"]["id"])')
echo "$NEW"   # 예: %1
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

---

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
  $"cd /Users/kasa/Desktop/momewomo/tmuxify && cargo build --release -p kasaterm 2>&1 | tee /tmp/$TASK.log; echo '---DONE---' >> /tmp/$TASK.log; exit\n"
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
| `KASATERM_AUTOCAPTURE_PATH` | `$TMPDIR/tmuxify.png` | 캡처 저장 경로 |
| `KASATERM_AUTOQUIT_MS` | (없음) | N ms 후 깨끗하게 종료(저장됨). 캡처 전용 run엔 **설정하지 말 것** — 종료 시 `session.json`이 테스트 레이아웃으로 덮어쓰임 |
| `KASATERM_AUTOSEND` / `_MS` | (없음) | 활성 pane에 키 자동 전송 (IME 조합 경로는 재현 X) |
| `KASATERM_IME_DEBUG=1` | — | 키 코드포인트 로깅 |

캡처 region은 **winit 창 좌표**에서 자동 계산해 `screencapture -R`로 전달한다(예전엔 osascript bounds가 None 떨어져 전체화면으로 새던 버그가 있었음 — `app/kasaterm/src/main.rs::schedule_autocapture`에서 winit 기반으로 고정).

### 풀 사이클 (UI 검증)

**0) 호스트 보호 — 절대 죽이지 마라**

사용자가 켠 kasaterm은 보통 `.app`(`/Users/kasa/Applications/kasaterm.app/Contents/MacOS/kasaterm`). pkill 패턴은 `target/debug|release/kasaterm`만 매칭하므로 호스트는 살아 있다. 확인:
```bash
ps aux | grep -iE "kasaterm" | grep -v grep | grep -oE "/[^ ]*kasaterm[^ ]*"
```
`.app` 경로면 호스트, `target/...`면 옛 테스트 인스턴스(죽여도 됨).

**1) 빌드**
```bash
cd /Users/kasa/Desktop/momewomo/tmuxify
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

## 5) pane 협업 — board(작업현황 조회) + tell(직접 깨우기) (board · tell · peek)

§4 팀 모드가 **TeamCreate 위계**(리드↔팀원)라면, 이건 **위계 없는 peer 협업**이다. 떠 있는 pane들끼리 — claude든 codex·antigravity든 — 소통하고 충돌을 피한다. `KASATERM_PANE_ID`가 비어 있으면 형제 pane이 없는 것이니 비적용.

> **2026-05-31 구조 변경:** 파일 mailbox(`kasa-chat`)·`chat-inject` hook 자동주입·mailbox watcher inbox는 **전부 폐기**됐다. 이제 협업은 ① 자동 채워지는 **board**(사용자는 GUI 패널로, claude는 CLI로 조회) + ② **PTY 직접 주입 `tell`**(claude끼리·사용자 둘 다) 두 축뿐이다.

### board — 각 pane이 뭘 하는지 (자동 등록)

호스트가 각 pane의 claude transcript를 tail해 tool_use(Read/Edit/Bash…)를 그 pane 활동으로 **자동 등록**한다. 평소처럼 작업하면 `Read auth.ts → Bash cargo build`가 저절로 뜬다 — `announce` 직접 호출 불필요.

- **사용자(거노)**: 상단 **보기 메뉴 → "board 패널"** 로 전 pane 작업 현황을 직접 본다. 각 행에 그 pane으로 바로 보내는 메시지 입력창이 있다.
- **claude(너)**: 자동 주입은 더 없으니 필요할 때 `kasaterm-cli board` 로 **직접 조회**한다. 내가 만질 파일을 board에서 다른 pane이 잡고 있으면 → 충돌 회피(같은 문제면 합류, 빌드 중이면 `peek %N`로 보고 대기).

### tell — 다른 pane에게 말 걸기 + 깨우기

```bash
kasaterm-cli tell %3 "auth.ts 너가 맡아줄래? 난 ui 쪽 볼게"
```

`tell`은 대상 pane PTY에 텍스트를 주입하고 **끝에 `\r`(엔터)을 붙여 제출**한다 — 그래서 **idle claude도** 새 user turn으로 받아 깨어난다. focus는 안 바뀐다.

- `send`(=`surface.send_text`)는 제출(`\r`)을 안 붙여 프롬프트에 글자만 남는다. **깨우려면 반드시 `tell`.**
- 사용자도 board 패널 각 행 입력창에서 같은 일을 한다(내부적으로 `POST /board-tell`).

### 함정

| 안 됨 | 왜 |
|---|---|
| `kasa-chat send` / 공지방 / hook 자동주입에 의존 | **폐기됨(2026-05-31)**. board는 `kasaterm-cli board`로 조회, 깨우기는 `tell` |
| `send`로 깨우려 함 | `send`는 `\r` 없음 → 프롬프트에 글자만. idle 깨우기는 `tell` |
| `tell`에 surface_id 생략 | `tell`은 항상 `<surface_id> <text>`. 자기 자신엔 안 씀 |
| board가 비어 보임 | 그 pane이 claude 아니거나 transcript 미bind — 정상 |
| 협업하려고 `announce` 호출 | 불필요 — transcript가 자동 등록 |

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

---

## 메모리 연결

- [[reference_autonomous_testing]] — 모델 직접 빌드→스폰→캡처→자동입력 사이클의 원형
- [[feedback_tmuxify_rendering_pipeline]] — 렌더 버그 카탈로그(첫 의심 순서)
- [[reference_kasaterm_design_unification]] — 디자인 토큰·통일 결정 (2026-05-26 배경 BG로 통일)
- [[feedback_team_lead_role_in_teamcreate]] — 리드는 지시만
- [[feedback_teamcreate_pane_failure]] — 병렬 Agent spawn race + 좀비 청소 절차 (이 스킬 §좀비 청소 절차)
- [[feedback_tmux_send_keys_enter_eaten]] — 엔터 씹힘 패턴 (kasaterm-cli send도 같은 함정)
- [[feedback_background_jobs_in_tmux_pane]] — 백그라운드 잡은 항상 새 pane
