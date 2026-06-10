# ⑤ munder 메시징 · idle wake · 말풍선 조사 보고서

> 거노 질문 ⑤ + 8d3f(말풍선) 대응. munder-difflin(`~/Desktop/munder-difflin`) 코드 직접 조사 +
> 우리 kasacollab / board 대비 + 샬레 교실 말풍선 구현안. 인용은 munder 측 `파일:줄`.

---

## A. 메시지 파일 / 포맷

### munder
- **메일박스(actor model)**: `<harnessHome>/hive/agents/<agentId>/` 아래
  `inbox/`(수신) · `inbox/.done/`(처리완료) · `outbox/`(발신) · `outbox/.sent/`(라우팅완료).
  메시지 1건 = **개별 JSON 파일** `<timestamp>-<random>.json`. (`src/main/hive.ts:79-82`)
- **HiveMessage 스키마** (`hive.ts:40-55`): `id`(시간정렬+random) · `conversation`(스레드) ·
  `in_reply_to` · `from` · `to`(agentId|`god`|`broadcast`) · `act`(FIPA-lite:
  request/inform/propose/query/agree/refuse/done) · `subject` · `body` ·
  `hops`(회신마다 ++, cap=12 무한루프 방지) · `requires_reply` · `needs_human` · `created_at`.
- **멱등성**: `agents/<id>/cursor.json` = `{lastProcessed: <msgId>|null}`. Stop 훅이 마지막 처리
  msgId 추적 → 같은 메시지 두 번 처리 안 함 (`hive.ts:295-296` 생성, `500-508` 사용).
  파일쓰기는 `*.tmp-<rand>`→`rename` 원자성 (`hive.ts:985-989`).

### kasacollab (우리)
- **단일 append 로그**: `/tmp/kasaterm-collab/<slug>/messages.jsonl` 1파일에 전 메시지 누적.
  스키마 `{id, from:%N, to:%N, text, ts, read}` — munder의 act/conversation/hops/requires_reply
  같은 **speech-act 메타 없음**(평문 text 1필드).
- **멱등성**: munder는 `id > lastProcessed` 단조비교지만 우리 short_id는 16bit
  `(time_ms ^ hash(pane)) & 0xFFFF` 라 충돌·비단조 → **비교 불가**. 그래서 기존 `read` 플래그를
  멱등키로 재활용(이미 read=true면 skip). lost-update는 **flock + atomic replace** 로 봉쇄.

**차이 요약**: munder=파일/건 + 풍부한 speech-act + cursor 단조비교. 우리=jsonl 1파일 + 평문 +
read 플래그 멱등 + flock. 우리가 더 가볍고, munder가 더 프로토콜적(agree/refuse/propose 협상).

---

## B. idle 에이전트 깨우기 (wake)

### munder — Polling Router + Stop Hook drain
1. **라우터 폴링 1500ms**: 메인 프로세스가 각 `outbox/` 폴 → 새 파일 읽어 수신자 `inbox/`에 작성
   (`hive.ts:727-765`, 부팅 `index.ts:1607`).
2. **Stop 훅 drain**: 에이전트가 Stop 도달 → `cth-hook.cjs`가 UDS로 페이로드 전송 →
   HookServer `drainForStop()`가 `cursor.json` 읽어 미처리 메시지 필터 → **있으면
   `{"decision":"block","reason":...}` 반환 → 턴 종료 막고 계속 일함** (`hooks.ts:143-159`,
   `hive.ts:497-518`). ← 이게 우리가 이미 베낀 패턴.
3. **무한루프 가드**: `stop_hook_active=true`면 다음 Stop은 정상 종료 허용 (`hooks.ts:143-145`).
4. **라우팅**: direct(to=id) / broadcast(전 활성, assistant·archived·hook불가 제외) /
   `to:"human"`→god 프록시 (`hive.ts:610-675`).
5. **Graceful interrupt**: ControlRegistry steer 주입 → 다음 훅 경계(PostToolUse/
   UserPromptSubmit)에서 확인 (`closingTime.ts:137-142`, `hooks.ts:182-188`).

### kasacollab (우리)
- **Stop 훅 drain 동일 채택**: `kasaterm-stop-drain.sh` — 미읽 inbox 있으면 stop 차단(이번 턴에도
  발동). munder의 stdout `{"decision":"block"}`+exit0 실증을 그대로 씀.
- **차이 = 즉시 깨우기 채널 보유**: munder는 "다음 Stop까지" 기다려야 inbox를 보지만(폴링+drain만),
  우리는 `kasacollab msg %N`이 **그 즉시 `tell %N`으로 깨운다**(강제 제출 = idle이면 즉행, 바쁘면
  입력창 누적). munder엔 없는 push. 단 라우터 폴(1500ms) 같은 중앙 큐가 없어 수신자 검증을
  보낼 때 직접 함(`list surfaces`로 죽은 pane 거름 — '%3에 자꾸 보냄' 버그 수정 경로).
- **LEAD 부재 가능**: 우리는 god 옵트인(solo 기본). munder는 god 상시-온 고정.

---

## C. LEAD / god 오케스트레이터

### munder
- **god = 상시-온 orchestrator** (Michael 룸 고정, `isGod:true`, Registry `godId`,
  `hive.ts:101,121-124,309`). 책임: ① `to:"human"` 라우팅 프록시(잡무 직접 해결, 파괴/지출/범위만
  인간 에스컬레이션) ② **board.md 유일 기록자**(타 에이전트는 propose만) ③ tasks.json kanban
  (todo/doing/blocked/done, assignee/우선도/의존성) ④ fleet.json 실시간 모니터(토큰/비용/breaker/
  inbox백로그) ⑤ Closing Time(shutdown brief→broadcast→ACK 수집).

### kasacollab (우리)
- **선출제 god**(O_EXCL 원자, god-elect.sh) — 상시 아님, 옵트인. 커밋 단독 + 라우팅. board는
  **읽기 전용 뷰**(누구나 보지만 god이 write 독점하지 않음 — 차이). tasks는 kasacollab task,
  roster로 인원 추적. munder fleet.json ≈ 우리 board(status/intent/tokens).

---

## D. 말풍선(작업중 표시) — munder 구조 + 우리 매핑안 ★ 거노 핵심

### munder 말풍선 UI
- **ThoughtBubble**(`src/renderer/src/scene/office/ThoughtBubble.ts`): pixi `Container` =
  `tail`(아래 2개 puff 동그라미)+`bg`(둥근박스 r=5)+`label`(mono bold 12px). 내부 2배 렌더 후
  `RENDER_SCALE=0.5` 다운(선명). 캐릭터 기준 `OFFSET_Y=-38`(위), 수평 중앙, **맵경계 클램프**+
  **겹침 시 위로 lift**(`:161-180`). 상태머신 hidden/fading-in(150ms)/visible/lingering(1.2s)/
  fading-out(300ms), 사고중 "…" 도트 450ms.
- **ToolBubble**(어두운 버블): tool 아이콘 매핑(Read=`<` Edit/Write=`>` Bash=`$` Grep=`?`
  MCP=`*`). Character가 `showThought(text, tool?)`/`hideThought()`로 제어, 매 프레임
  `update(dt)`+`setPosition(px,py)`.

### 데이터 소스 (munder)
- store `Agent.action`(현재 활동) · `status` · `carrying`(도구) · `lastPrompt`.
- **Hook 이벤트 push**(`useHive.ts:264-305`, `window.cth.onHiveHookEvent`): PreToolUse→
  `action:'using ${tool}'`+carrying, PreInvocation→`thinking`, PreCompact→`compacting`,
  Stop→`idle`, Notification(차단)→`reading inbox`.
- **우선순위**(`OfficeFloor.tsx:175-179` `liveActivity`): `agent.action` → 없으면
  `firstWords(agent.lastPrompt)` → fallback.
- **갱신**: 표시 애니메이션은 60fps pixi ticker(`update(dt)`), **텍스트 값은 hook 이벤트
  push로 즉시**(폴링 아님). 보조로 board 5s·context 15s 폴.
- **캐릭터 상태 표현**(말풍선 외): statusGlyph(blocked=빨강"!"깜빡, success=금별, compacting=보라박스,
  looping=주황링) + workGlow(0.6s 사인 맥박 할로) + 위치(desk/floor/door) (`Character.ts:733-766`,
  `OfficeFloor.tsx applyState:1411-1535`).

### 우리 매핑안 (kasaterm 교실)
**핵심: 우리 board가 이미 munder agent.action 데이터를 거의 다 들고 있다 — 새 배선 최소.**
실측 board row 필드: `intent`(="TaskUpdate"/"Bash cd ..." = **tool 기반 현재 활동**, munder
action과 동형) · `last_prompt` · `last_reply` · `status`(working/waiting/blocked/idle) ·
`tool_counts` · `tokens_in/out`.

| munder | 우리 board 대응 | 비고 |
|--------|----------------|------|
| `agent.action` (hook push) | **`intent`** | 이미 tool 이름 라벨. 말풍선 1순위 텍스트 |
| `firstWords(lastPrompt)` 폴백 | `last_reply` 첫 어절(또는 `last_prompt`) | intent 없을 때 |
| `carrying`(도구 아이콘) | `intent`에서 tool 토큰 파싱 | Read/Edit/Bash 아이콘 매핑 |
| `status`→글리프/glow | `status` | 이미 ClassroomCharacter.setStatus 보유 |
| 60fps ticker | 페이드/linger만 ticker, 텍스트는 board 1s 폴 | granularity 충분(행동 라벨) |

**구현 단계(후속, 보고서 범위 밖 실작업)**:
1. `ClassroomCharacter`에 ThoughtBubble 포팅(MIT, pixi Container+bg+tail+Text, OFFSET_Y=-38).
   ClassroomView는 이미 `app.ticker.add`로 `c.tick(dt)` 돌림 → 거기서 `bubble.update(dt)`.
2. board→Agent 매핑(`mcp.ts toAgent`)에 `action = r.intent`, `lastReply = r.last_reply` 추가.
   `liveActivity = agent.action || firstWords(agent.lastReply)`.
3. ClassroomView `sync()`에서 status/action 변할 때 `c.showThought(liveActivity, toolOf(intent))`.
   munder처럼 prevAction/prevStatus 비교로 불필요 재드로우 방지.
4. (옵션) status 글리프(blocked "!", success 별)+workGlow — 우리 setStatus 확장.
- **데이터 차이 1개**: munder는 hook push라 즉시(<100ms), 우리는 board 폴 1s 지연. 말풍선이
  "행동 라벨"이라 1s면 체감 충분. 더 빠르게 원하면 후속에 intent 변경 시 MCP가 push.

---

## E. 한 줄 결론
- 메시징: 우리가 더 가볍다(jsonl+flock+tell즉시깨움). munder는 speech-act 협상·중앙라우터가 강점.
  **Stop drain 멱등은 이미 흡수 완료.** 추가 차용 후보 = `act`(propose/agree/refuse) 협상 메타,
  broadcast `to:human`→god 프록시.
- 말풍선: **새 데이터 파이프라인 불필요** — board.intent/last_reply/status로 munder liveActivity를
  그대로 재현. ThoughtBubble(MIT) 포팅 + toAgent 2필드 추가 + sync 1줄이면 교실 학생 위에 "지금
  뭐함" 뜬다.
