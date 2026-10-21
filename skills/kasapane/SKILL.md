---
name: kasapane
description: 카사텀의 학생 협업, 칸 조작, 긴 잡, 미디어 결과물 전달, 세션 이사에 사용한다. 일반 코드 수정만으로는 읽지 않는다.
---

# Kasapane

현재 작업의 주제 하나만 읽는다. 관련 없는 자료를 함께 읽지 않고, 이미 읽은 내용은 바뀌기 전까지 재사용한다.

| 지금 할 일 | 명령 | 같은 원문 |
|---|---|---|
| 학생에게 일을 맡기거나 보고·쪽지 전달 | `kasaterm-cli guide collab` | [협업](collab.md) |
| 칸·방 조회, 만들기, 이름·배치·닫기 | `kasaterm-cli guide panes` | [칸 조작](references/panes.md) |
| 오래 실행되는 빌드·시험·서버 | `kasaterm-cli guide jobs` | [긴 잡](references/jobs.md) |
| 완성한 이미지·음성·영상 전달 | `kasaterm-cli guide media` | [결과물](references/media.md) |
| 다른 기기로 세션 옮기기 | `kasaterm-cli guide transfer` | [이사](references/transfer.md) |
| 카사텀 자체의 코드·화면 수정 | `kasaterm-cli guide develop` | [개발](references/develop.md) |

`guide`는 앱 연결 없이 현재 디렉터리와 무관하게 주제 하나를 출력한다. 구판 CLI에 명령이 없으면 위의 같은 원문을 읽는다. 앱을 임의로 재시작하지 않는다.
명령의 옵션은 `kasaterm-cli --help`, 이동은 `kasaterm-cli machines move --help`에서 확인한다. 명령어 목록 전체를 미리 읽지 않는다.
