---
description: 이 pane 의 세션 이름 바꾸기 — 팀원 세션에서 막히는 내장 /rename 을 대신한다
allowed-tools: Bash(kasaterm-cli rename:*)
---

kasaterm pane 의 claude 는 shim 이 트리플(`--agent-id`/`--agent-name`/`--team-name`)을
자동으로 붙이므로 전부 팀원이고, 내장 `/rename` 은 "Teammate names are set by the
team leader" 로 무조건 거부한다. `kasaterm-cli rename` 이 그 차단을 우회해 jsonl 에
`nameSource:"user"` 로 제목을 박는다 — 그 표식이 있으면 title-sync 가 자동 제목으로
덮지 않는다.

!`kasaterm-cli rename "$ARGUMENTS"`
