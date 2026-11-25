---
name: kasapane
description: 카사텀 칸 안의 세션은 늘 지킨다 — 사용자가 맡긴 일이 다 끝났거나 사용자의 결정이 필요하면 `kasaterm-cli done succeeded|failed|blocked|needs_approval '한 줄 요약'`을 한 번 보낸다(폰·맥 알림은 이것만 간다). `⟦폰⟧`으로 끝나는 말은 폰에서 왔으니 짧게 답하고 화면 대신 링크·이미지를 준다. `⟦이름@기기⟧`로 시작하는 말은 다른 기기 학생이니 `kasaterm-cli tell 이름@기기`로 답한다. 완성한 그림(GIF·SVG도)은 `imgopen --block 절대경로`, 음성·영상·PDF·배포한 웹 주소는 `mediaopen --block 절대경로|URL`이 돌려준 블록을 답에 그대로 싣고 파일 링크를 곁에 둔다(경로만 주면 카드가 안 뜬다). 일을 나눌 때는 하위 에이전트 대신 `kasaterm-cli summon` 학생으로 — 조사 하나는 `--tab`, 여러 갈래는 칸을 나눠. 협업·칸 조작·긴 잡·결과물 카드·세션 이사·카사텀 개발 때 읽는다.
---

# Kasapane

설명의 다섯 규칙(완료 보고·`⟦폰⟧`·`⟦이름@기기⟧`·결과물 카드·학생으로 나누기)은 이 파일을 읽지 않아도 늘 지킨다. 승인·질문 창, 사용량 한도, 충돌은
저절로 알려지고 턴이 끝난 것만으로는 알리지 않는다. `⟦미러@기기⟧`로 끝나는 말은 그 기기의 미러 화면에서 보낸 것이다.

현재 작업의 주제 하나만 읽는다. 관련 없는 자료를 함께 읽지 않고, 이미 읽은 내용은 바뀌기 전까지 재사용한다.

| 지금 할 일 | 명령 | 같은 원문 |
|---|---|---|
| 학생에게 일을 맡기거나 보고·쪽지 전달 | `kasaterm-cli guide collab` | [협업](collab.md) |
| 칸·방 조회, 만들기, 이름·배치·닫기 | `kasaterm-cli guide panes` | [칸 조작](references/panes.md) |
| 오래 실행되는 빌드·시험·서버 | `kasaterm-cli guide jobs` | [긴 잡](references/jobs.md) |
| 완성한 그림·음성·영상·PDF·웹 카드 전달 | `kasaterm-cli guide media` | [결과물](references/media.md) |
| 다른 기기로 세션 옮기기 | `kasaterm-cli guide transfer` | [이사](references/transfer.md) |
| 카사텀 자체의 코드·화면 수정 | `kasaterm-cli guide develop` | [개발](references/develop.md) |

`guide`는 앱 연결 없이 현재 디렉터리와 무관하게 주제 하나를 출력한다. 구판 CLI에 명령이 없으면 위의 같은 원문을 읽는다. 앱을 임의로 재시작하지 않는다.
명령의 옵션은 `kasaterm-cli --help`, 이동은 `kasaterm-cli machines move --help`에서 확인한다. 명령어 목록 전체를 미리 읽지 않는다.
