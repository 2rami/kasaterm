# kasaterm.js

xterm.js 를 대신할 자체 웹 터미널. PTY 바이트를 받아 브라우저 안에서 해석하고(파서 포함) 그린다. 의존성이 없고
빌드 결과는 파일 하나(`dist/kasaterm.js`, UMD — 페이지에서는 `window.KasaTerm`, node 에서는 `require`)다.

해석의 정본은 카사텀 앱 자신의 터미널(kasa-pty 안의 alacritty_terminal)이다. 같은 바이트를 넣으면 앱 창과 **셀 하나까지
같은 화면**이 나와야 한다 — 그래서 alacritty 의 버릇도 그대로 따른다(아래 「일부러 따르는 것」). 고치고 싶은 동작이
있으면 여기서 고치지 말고 kasa-pty 에서 고친 뒤 이쪽을 맞춘다. 안 그러면 거울과 앱이 서로 다른 화면을 보인다.

## 지금 되는 것 (1단계: 해석)

- 파서: vt100.net 의 DEC ANSI 상태 기계. CSI·OSC·DCS·SOS/PM/APC 통과, 인자 32개·중간 문자 2개 한도, OSC 는 BEL·ST 로
  끝남, UTF-8 깨진 바이트 처리까지 vte 0.15 와 같다.
- 화면: 주/대체 화면(1049), 스크롤백(저장소·캐시까지 alacritty 구조), 줄 접힘 표지(WRAPLINE)와 크기 바꿀 때 다시 접기,
  넓은 글자·결합 문자·NFC 합성, SGR(16·256·트루컬러, 굵게·흐리게·기울임·밑줄 다섯 꼴·밑줄 색·반전·숨김·취소선),
  커서 이동·지우기·삽입/삭제·DECSTBM·탭·DEC 특수 문자 집합·원점 모드·REP·DECALN, 동기 갱신(2026)은 앱처럼 모았다가
  한 번에 적용.
- 모드: DECCKM·DECKPAM·삽입·줄 감싸기·커서 보이기·마우스(1000/1002/1003, 1005/1006)·포커스(1004)·괄호 붙여넣기(2004)·
  대체 스크롤(1007) 등. DECRQM 응답.
- OSC: 0/2 제목(22/23 쌓기 포함), 4/10/11/12 색 설정·질의, 8 링크, 50 커서 꼴, 52 클립보드 쓰기, 104/110~112 색 되돌리기.
  앱이 그냥 지나치는 7(작업 폴더)·133(프롬프트 표지)은 화면을 안 바꾸고 이벤트로만 낸다.
- 응답: DA1(`ESC[?62;22c`, kasa-pty 와 같은 값)·DA2·DSR·CPR·창 크기(18t·14t)·색 질의.

## 쓰는 법 (헤드리스)

```js
const { Terminal } = require('./dist/kasaterm.js');
const term = new Terminal({ cols: 80, rows: 24, scrollback: 10000 });
term.onData(reply => pty.write(reply));      // DA·CPR 등 프로그램에 돌려줄 바이트
term.on('title', t => …); term.on('bell', …); term.on('clipboard', (kind, text) => …);
term.on('cwd', uri => …); term.on('prompt', params => …);
term.queryColor = index => [r, g, b];        // OSC 4/10/11/12 질의에 답할 색(없으면 답하지 않음)
term.write(bytes);                           // Uint8Array(PTY read 한 번) 또는 문자열
term.resize(cols, rows);
const row = term.line(y);                    // y < 0 은 스크롤백(-term.historySize 까지)
row.text(x); row.fg(x); row.bg(x); row.flags(x); row.extra(x);   // extra: {zw, ul, link}
term.cursor;                                 // {line, col, wrap, …}
term.mode;                                   // alacritty TermMode 비트(MODE.*)
```

`write` 는 **PTY read 한 번을 한 번에** 넣는 것이 좋다. 앱도 read 한 번마다 NFC 를 돌리므로 묶음 경계가 같아야 분해형
한글이 같은 칸에 같은 모양으로 앉는다(`/term/ws` 는 read 한 번을 메시지 하나로 보낸다).

## 대조 시험

```
cargo build --release -p kasa-pty --example vt_oracle    # 정답 해석기 (crates/kasa-pty/examples/vt_oracle.rs)
node build.mjs && node test/run.mjs                      # 녹화 말뭉치 + 손 시험 + 퍼즈 200개
KTJS_FUZZ=3000 node test/run.mjs                         # 퍼즈를 더
node test/run.mjs fuzz:100001-103000                     # 거친 퍼즈(색 지우기·스크롤백 지우기·리셋·화면 전환)
node test/minimize.mjs fuzz:<seed>                       # 실패한 퍼즈를 최소 재현으로 줄이기
```

`vt_oracle` 은 kasa-pty 의 pane reader 와 똑같이 바이트를 alacritty 에 먹인다(read 경계에서 끊긴 UTF-8 보류, read 마다 NFC,
열 수에 따른 스크롤백 한도와 그 갱신). PTY 묶음 하나를 넣을 때마다 양쪽 화면을 셀 단위로 맞춘다 — 글자(NFC), 폭 표지,
전경·배경색(이름 색·256색·트루컬러를 구분), 꾸밈, 밑줄 색, 링크, 커서(줄·칸·감싸기 대기), 모드 비트, 커서 꼴,
스크롤백 줄 수와 최근 줄, 동기 갱신 중인지, 프로그램에 돌려준 응답·제목·종·클립보드. 끝에서 스크롤백 전체를 맞춘다.
글자 폭 표도 `vt_oracle widths` 가 alacritty 에 코드 포인트를 하나씩 넣어 본 결과로 만든다(`tools/gen-width.mjs`).

녹화(`corpus/*.kcast.gz`, JSON 줄: 머리 + `[t,"o",base64]`·`[t,"r",cols,rows]`):

| 파일 | 내용 |
|---|---|
| `shell-ls-korean` | zsh — `ls --color`·NFD 한글 파일 이름·한글 파일·SGR·링크·크기 바꿈 |
| `vim` | vim — 구문 강조·한글 입력·세로 나누기·`:set list`·크기 바꿈 |
| `less` | less — 긴 한글 파일 넘기기·찾기·크기 바꿈 |
| `htop` | htop — 색 막대·도움말·트리·크기 바꿈 (가짜 프로세스만 보이게) |
| `claude-classic` / `claude-fullscreen` | Claude Code 두 화면 꼴 — 한글 답·표·코드 블록·크기 바꿈·휠 |
| `codex` | Codex — 폴더 신뢰·한글 답·표·코드 블록·크기 바꿈 |

새로 녹화하려면 `tools/record.py`(PTY read 한 번 = 사건 하나, 질의는 kasaterm.js 가 답함, 자식은 `env -i` 같은 깨끗한
환경)로 찍고, **반드시** `tools/scrub.py` 를 거친다. 계정 이름·호스트 이름·홈 경로·전자우편·세션 id 를 같은 길이로
바꾸고, 토큰 꼴이나 개인 정보가 남아 있으면 파일을 쓰지 않는다. 공개 레포다.

## 일부러 따르는 것 (앱과 같아야 해서)

- 47·1047·1048 대체 화면 모드는 무시한다 — 앱이 무시하므로 여기서 켜면 거울만 다른 화면을 보인다. 실제 프로그램은
  xterm-256color 의 smcup 인 1049 를 쓴다.
- 줄 0 에서 DL(`CSI M`)·SU 로 밀려난 줄, 맨 위가 0 인 스크롤 영역에서 밀려난 줄은 스크롤백으로 간다.
- `CSI 1 J` 는 커서가 1번 줄일 때 0번 줄을 안 지운다. 원점 모드의 CUU·CUD·CHA 는 영역 위쪽을 두 번 더한다.
- 창을 넓힌 줄은 새 칸이 기본 배경이라, 그 줄을 기본 배경으로 다시 지워도 옛 칸의 배경색이 남는다(행 `occ` 최적화).
- 대체 화면에서 폭이 바뀌면 kasa-pty 가 스크롤백 한도를 그 화면에 다시 걸어, 대체 화면에도 스크롤백이 쌓인다.
- 동기 갱신에 시간 제한이 없다(앱도 `ESC[?2026l` 이 올 때까지 기다린다). 필요하면 `flushSync()`.
- kitty 키보드 프로토콜·XTGETTCAP·DECRQSS·식셀은 없다(앱 설정과 같음).

## 다음 단계

2. 그리기·입력 — 캔버스 렌더러, 키 → 바이트, 한글 IME(숨긴 textarea, `assets/term/grid.js` 의 교훈), 붙여넣기, 마우스·휠,
   선택·복사, 칸 맞추기, 테마(`KasaTermChrome` 토큰).
3. `/term` 갈아 끼우기(사람 확인 뒤), xterm.js 걷기.
4. 속도 — xterm.js 와 처리량·프레임 시간 비교.
