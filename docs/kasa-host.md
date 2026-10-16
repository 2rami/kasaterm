# kasa-host — 창 없는 카사텀

다른 프로그램이 띄운 셸 세션을 `kasa-pty` 로 붙들고 유닉스 소켓으로 내주는 실행 파일. 첫 손님은 나쵸다 —
나쵸가 일하는 claude·codex 세션을 본판 칸 대신 여기 띄워, 카사텀 판에는 학생 칸만 남고 나쵸 세션은 나쵸 앱에서만 보인다.
본판 GUI 가 아니라 이 프로세스가 PTY 를 쥐므로 부르는 쪽(나쵸 봇)이 재시작해도 세션은 끊기지 않는다.

```sh
kasa-host --socket ~/.config/nacho/host.sock
```

- 같은 자리에 살아 있는 kasa-host 가 있으면 뜨지 않는다. 죽은 호스트가 남긴 소켓 파일만 치운다. 소켓은 0600.
- 카사텀 칸에서 띄워도 `KASATERM_*` env 와 카사텀 shim 폴더(PATH 항목·그 폴더를 가리키는 `ZDOTDIR`)를 걷고 시작한다 — 안 걷으면 여기서 띄운 세션의 `claude` 가
  학생 감싸개로 풀리고 훅이 그 칸의 카사텀 판에 보고한다.
- 클립보드(OSC 52)는 받을 사람이 없어 읽기는 빈 글, 쓰기는 버린다(`kasa-pty` 를 `os-clipboard` 없이 받는다).

## 소켓 계약

요청 한 줄 `{"id", "method", "params"}` → 답 한 줄 `{"id", "ok": true, "result"}` 또는 `{"id", "ok": false, "error"}`.
연결 하나에 요청을 여러 줄 보내도 되고, 롱폴(`screen` 의 `wait`)은 그 연결만 붙잡는다.

| method | params | result |
|---|---|---|
| `ping` | — | `{version, pid}` |
| `spawn` | `name?` `cwd?` `env?{}` `cols?`(120) `rows?`(40) `shell?`(기본 `$SHELL` 로그인 셸) `cmd?` | `{id, name, pid}` |
| `list` | — | `[{id, name, pid, cols, rows, title, cwd, agent, created_ms}]`(띄운 순) |
| `write` | `id` `data` | `{}` — 바이트 그대로 |
| `paste` | `id` `text` `submit?` | `{}` — 앱이 붙여넣기 감싸기(2004)를 켰을 때만 감싸고, `submit` 이면 150ms 뒤 Enter |
| `key` | `id` `name` | `{}` — `enter`·`tab`·`shift+tab`·`escape`·`backspace`·`delete`·`space`·방향·`home`·`end`·`pageup`·`pagedown`·`ctrl+<글자>`·한 글자 |
| `screen` | `id` `since?` `wait?`(초, 25 상한) | `{hash, frame, title, alive}` — `frame` 은 본판 웹 터미널과 같은 grid 프레임(`kasa_screen::wire`). `since` 가 지금 해시면 바뀔 때까지 기다린다 |
| `text` | `id` `lines?`(40) | `{text}` — 보이는 마지막 줄들, 줄 끝 공백 걷음 |
| `resize` | `id` `cols` `rows` | `{}` |
| `rename` | `id` `name` | `{}` |
| `close` | `id` | `{}` — 셸과 그 자식을 끝내고 목록에서 뺀다 |

- `spawn` 의 `cmd` 는 프로세스로 바로 띄우지 않고 띄운 셸에 쳐 넣는다. 셸 함수·별칭이 풀리고, 명령이 끝나도 셸이 남아 같은 칸에서 다시 켤 수 있다.
- 세션 env 에 `KASA_HOST_SESSION=<id>` 가 붙는다. 칸 안의 훅·도구가 자기 세션을 안다.
- 셸이 끝나면(EOF) 세션은 목록에서 빠진다. 없는 `id` 는 오류다.

## 남은 단계

- 이 호스트를 판 올림 없이 넘기기 — `PtySession::adopt` 로 PTY fd 를 새 프로세스에 건넨다.
- 나쵸 쪽: LaunchAgent, `farpane` 자리의 손님, claude 훅으로 턴 끝 받기 — nacho-neko `docs/development/subscription.md` §30.
