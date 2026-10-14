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
term.modes;                                  // xterm.js 꼴 요약(applicationCursorKeysMode·mouseTrackingMode·…)
term.reset(); term.clear(); term.writeln(s, cb); term.write(bytes, cb);
term.on('linefeed' | 'scroll' | 'cursormove' | 'iconname' | 'pointershape', …);
// xterm.js 의 term.parser — 내장 처리 앞에서 새것부터 불리고, true 를 돌려주면 거기서 끝(동기만)
term.parser.registerCsiHandler({ prefix: '?', final: 'h' }, params => false);   // params: [1, 2, [3]] 꼴
term.parser.registerOscHandler(1337, data => true);
term.parser.registerDcsHandler({ final: 'q' }, (data, params) => true);         // 식셀 같은 그림 애드온 받침
term.parser.registerApcHandler({ final: 'G' }, data => true);                   // kitty 그림
term.parser.registerEscHandler({ final: 'c' }, () => true);
term.buffer.active.getLine(y).translateToString(true);  // xterm.js 의 읽기 API(normal·alternate·getCell·cursorX…)
const m = term.registerMarker(0); m.line; m.onDispose(…); // 스크롤·되접기를 따라가고, 줄이 사라지면 dispose
term.loadAddon({ activate(term) {}, dispose() {} }); term.dispose();
term.options.convertEol = true;                         // profile·tabStopWidth·scrollback·cellPixels·colorScheme 도 실시간
term.scrollLines(-3); term.scrollToBottom();            // 화면 위치 — 'viewport' 이벤트
```

옵션: `profile`(아래), `convertEol`(LF 를 CRLF 로), `tabStopWidth`, `cellPixels`({width, height} — 14t·16t 답),
`colorScheme`('dark'·'light' — `CSI ? 996 n` 답, `setColorScheme()` 로 바꿈).

`write` 는 **PTY read 한 번을 한 번에** 넣는 것이 좋다. 앱도 read 한 번마다 NFC 를 돌리므로 묶음 경계가 같아야 분해형
한글이 같은 칸에 같은 모양으로 앉는다(`/term/ws` 는 read 한 번을 메시지 하나로 보낸다).

## 대조 시험

```
cargo build --release -p kasa-pty --example vt_oracle    # 정답 해석기 (crates/kasa-pty/examples/vt_oracle.rs)
node build.mjs && node test/run.mjs                      # 녹화 말뭉치 + 손 시험 + 퍼즈 200개
KTJS_FUZZ=3000 node test/run.mjs                         # 퍼즈를 더
node test/run.mjs fuzz:100001-103000                     # 거친 퍼즈(색 지우기·스크롤백 지우기·리셋·화면 전환)
node test/minimize.mjs fuzz:<seed>                       # 실패한 퍼즈를 최소 재현으로 줄이기
npm install && node test/xterm.mjs                       # xterm 프로필 — @xterm/headless 와 칸 단위로
node test/api.mjs                                        # 파서 훅·reset·clear·modes·이벤트
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
  xterm-256color 의 smcup 인 1049 를 쓴다. (`profile: 'xterm'` 에서만 켜진다 — 아래.)
- 줄 0 에서 DL(`CSI M`)·SU 로 밀려난 줄, 맨 위가 0 인 스크롤 영역에서 밀려난 줄은 스크롤백으로 간다.
- `CSI 1 J` 는 커서가 1번 줄일 때 0번 줄을 안 지운다. 원점 모드의 CUU·CUD·CHA 는 영역 위쪽을 두 번 더한다.
- 창을 넓힌 줄은 새 칸이 기본 배경이라, 그 줄을 기본 배경으로 다시 지워도 옛 칸의 배경색이 남는다(행 `occ` 최적화).
- 대체 화면에서 폭이 바뀌면 kasa-pty 가 스크롤백 한도를 그 화면에 다시 걸어, 대체 화면에도 스크롤백이 쌓인다.
- 동기 갱신에 시간 제한이 없다(앱도 `ESC[?2026l` 이 올 때까지 기다린다). 필요하면 `flushSync()`.
- kitty 키보드 프로토콜·DECRQSS 는 기본 프로필에서 꺼져 있다(앱 설정과 같음). XTGETTCAP·식셀 해석은 없다(식셀은 DCS 훅 위에).

## xterm 프로필 (`new Terminal({ profile: 'xterm' })`)

xterm.js 가 알고 앱(alacritty)은 무시하는 것을 켠다. **켜면 거울이 앱과 다른 화면을 보일 수 있다** — 앱이 그 시퀀스를
지나치기 때문이다. `/term` 은 기본 프로필을 쓴다. alacritty 가 다루는 것(ED·EL·SGR 21·1049·DA1 등)은 이 프로필에서도
alacritty 를 따르고, 나머지는 xterm.js(`@xterm/headless` 6.0, 없는 것은 main 브랜치)와 규격을 따른다.
`test/xterm.mjs` 가 칸 단위로 맞추고, 기본 프로필로 돌리면 전부 실패하는 것까지 확인했다.

- 화면: SL·SR(`CSI Ps SP @/A`), DECIC·DECDC(`CSI Ps ' }/~`), SD 의 `CSI Ps ^`, DECSCA 보호 + DECSED·DECSEL(`CSI ? J/K`),
  DECSTR(`CSI ! p`), 대체 화면 47·1047·1048, 역 줄바꿈(45), LNM(`CSI 20 h` — LF 가 CR 도), UTF-8 로 온 C1 제어
  (IND·NEL·HTS·CSI·OSC·DCS·SOS·PM·APC·ST), 나라별 문자 집합(`ESC ( A/4/C/5/R/Q/K/Y/E/6/Z/H/7/=`, G1~G3, `ESC - .`),
  잠금 시프트(`ESC n o | } ~`), `ESC % @/G`, `CSI ? 2 h`(문자 집합 되돌림), SGR 5·25(깜빡임)·53·55(윗줄)·221·222.
- 응답: XTVERSION(`CSI > q` → `kasaterm.js(판)`), DECRQSS(`m`·`r`·`"q`·`"p`·` q`), DECXCPR(`CSI ? 6 n`), 16t(칸 픽셀),
  22/23t 의 제목·아이콘 이름 고르기, DECRQM(2·12·8·9·45·66·67·1015·1016·1048·47·1047·9001), 색 구성표(`CSI ? 996 n`, 2031 구독).
- 모드: X10 마우스(9)·SGR 픽셀(1016)·66·win32 입력(9001)·kitty 키보드 플래그(`CSI = > < ? u`, alacritty 의 처리를 켠 것과 같게 —
  `KTJS_KITTY=1` 로 띄운 `vt_oracle` 과 대조).
- xterm.js 와 일부러 다른 것: 네덜란드 `[` 는 ĳ 한 글자(xterm.js 는 "ij" 를 넣고 i 만 그린다), DECSCL 은 DA1 이 말하는
  VT220(62)으로, DECRQSS `m` 은 늘 `0m` 이 아니라 지금 꾸밈을, kitty 스택이 넘치면 가장 오래된 것을 버린다(alacritty 는
  제목 스택에서 지우고, 그게 비어 있으면 멈춘다 — 앱은 kitty 를 꺼서 안 걸린다).

## 그리기·입력 (2단계)

```js
const term = new KasaTerm.Terminal({ cols: 80, rows: 24, scrollback: 10000 });
term.open(el, { fontSize: 13, fontFamily, lineHeight: 1.2, theme, optionAsMeta, onLink });
term.on('input', bytes => ws.send(bytes));   // 사람이 친 것만(키·IME·붙여넣기·마우스 보고·포커스)
term.fit();                                  // 담는 상자에 맞춰 cols·rows 를 정해 resize
term.setTheme({ background, foreground, cursor, cursorAccent, selectionBackground, ansi: [16] });
term.setTheme(KasaTerm.themeFromTokens(tokens));   // 카사텀 디자인 토큰(/design-tokens)
term.view.setFont(size); term.view.cell;     // {width, height} CSS px
term.view.scrollLines(n); term.view.selectionText(); term.focus();
```

- **캔버스 하나**에 바뀐 줄만 다시 그린다(줄마다 잘라 그려 넘친 글리프가 이웃에 자국을 안 남김). HiDPI 는 장치
  픽셀로 맞춘다. 넓은 글자는 두 칸 가운데, 칸보다 넓은 대체 글리프는 눌러 맞춘다. 박스·블록 문자(U+2500–259F)는
  글꼴 대신 도형으로 그려 줄 높이와 상관없이 이어진다. 밑줄 다섯 꼴·밑줄 색·취소선·흐리게·반전·숨김, 커서 꼴
  셋(블록·밑줄·막대)과 깜박임, 초점을 잃으면 속 빈 블록. 동기 갱신 중엔 앱처럼 직전 화면이 남는다.
- **입력**은 커서 자리의 숨긴 textarea 가 받고, 프로그램에 가는 것은 그 내용의 **변화**다(`assets/term/grid.js` 와 같은
  방식) — 조합 이벤트를 주는 데스크톱 IME 는 끝날 때 한 번, 조합 이벤트 없이 자모를 바꿔 쓰는 폰 키보드는 지우고
  다시 쓰기로 간다. 조합 중 글자는 커서 자리에 밑줄로 보인다.
- **키**는 앱(`input.rs`)과 같다: Shift/Option+Enter → LF, Option+←/→ → 단어 이동(`ESC b/f`), Cmd+Backspace → `^U`,
  Ctrl+Backspace → `^W`, Cmd+←/→ → 줄 처음/끝, 방향키는 DECCKM 을 따르고 수정키는 `CSI 1;m X`. Ctrl 조합은 물리 키로
  정해 한글 자판에서도 Ctrl+C 가 된다. Option 을 Meta 로 쓸지는 `optionAsMeta`(맥 기본 끔).
- **붙여넣기**는 줄바꿈을 CR 로 바꾸고 2004 가 켜져 있으면 괄호로 감싼다(붙여 넣은 글 속 괄호 표지는 걷는다).
- **마우스**: 1000/1002/1003 + SGR(1006)·UTF-8(1005)·옛 인코딩. Shift 를 누르면 보고 대신 선택. **휠·스와이프**는 앱과
  같은 규칙 — 마우스를 켠 프로그램엔 휠 보고, 대체 화면+1007 이면 방향키, 그 밖엔 스크롤백.
- **선택·복사**: 끌기·두 번(단어)·세 번(접힌 줄까지 한 줄) 누르기, Cmd+C(맥)·Ctrl+Shift+C. 접힌 줄은 이어 붙이고
  줄 끝 공백은 뗀다. **링크**: OSC 8 과 글 속 `http(s)://` 주소에 올리면 밑줄, Cmd/Ctrl+누르기로 연다.
- 브라우저 시험: `node test-view/input.mjs`(Playwright 를 전역에 깔아 두면 헤드리스 크롬으로 키·IME 조합·폰식 조합·
  붙여넣기·선택·휠·마우스 보고·포커스를 확인).

## 웹 터미널에 실린 자리 (3단계)

`/term` 의 기본 보기가 kasaterm.js 다(옛 링크의 `?renderer=ansi` 도 같다). 서버가 해석한 격자를 그리는 옛 보기는
`/term?renderer=grid`(→ `/term/grid`)로 남았다. `node build.mjs` 가 `dist/` 와 함께
`crates/kasa-mcp/assets/term/kasaterm.js` 를 쓰고, 앱이 그 파일을 `/term/kasaterm.js` 로 낸다(바이너리에 박힘).
src 를 고쳤으면 빌드한 자산도 같이 커밋한다. 페이지는 사람이 친 것(`term.on('input')`)만 서버로 보낸다 — 프로그램의
질의에는 서버 쪽 kasa-pty 가 이미 답한다.

## 남은 단계

4. 속도 — xterm.js 와 처리량·프레임 시간 비교(큰 출력 흘리기, 긴 스크롤백). 모자라면 WebGL 렌더러.
