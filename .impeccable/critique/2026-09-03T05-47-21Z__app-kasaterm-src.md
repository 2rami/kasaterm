---
target: wgpu로 만든 이상한 디자인 창들과 토스트
total_score: 25
max_score: 40
na_heuristics: 
p0_count: 0
p1_count: 3
timestamp: 2026-09-03T05-47-21Z
slug: app-kasaterm-src
---
Method: dual-agent (A: codex_restore_scout · B: codex_restore_verify)

## Design Health Score

| # | Heuristic | Score | Key Issue |
|---|-----------|------:|-----------|
| 1 | Visibility of System Status | 3 | 상태는 보이지만 중요한 보조 문구와 경고가 너무 흐리다. |
| 2 | Match System / Real World | 2 | `%0`, `ctx`, Raw, 복귀 화살표 같은 내부 언어를 사용자가 해석해야 한다. |
| 3 | User Control and Freedom | 3 | 닫기와 복귀는 있으나 작은 아이콘과 마우스 조작에 숨는다. |
| 4 | Consistency and Standards | 3 | 팔레트는 이어지지만 토스트·편집기·터미널의 창 문법이 갈린다. |
| 5 | Error Prevention | 3 | 작업 보호는 강하지만 축소 편집기의 기능 차이를 미리 알리지 않는다. |
| 6 | Recognition Rather Than Recall | 2 | 아이콘 전용 조작과 내부 식별자의 의미를 외워야 한다. |
| 7 | Flexibility and Efficiency | 3 | 분할·줌·단축키는 강하지만 보조 창은 마우스 의존적이다. |
| 8 | Aesthetic and Minimalist Design | 3 | 차분하지만 저대비와 빈 헤더가 미완성 인상을 만든다. |
| 9 | Error Recovery | 2 | 일부 창 생성 실패는 사용자에게 아무 설명 없이 사라진다. |
| 10 | Help and Documentation | 1 | 커스텀 아이콘의 이름·툴팁·맥락 도움말이 거의 없다. |
| **Total** | | **25/40** | **Acceptable — 제품 기반은 좋지만 공통 창 문법과 접근성 정리가 필요하다.** |

## Design Specificity Verdict

학생 이름·색·스프라이트가 있는 방은 kasaterm만의 작업실처럼 보인다. 하지만 학생이 없는 팝아웃 편집기와 토스트는 일반 다크 개발도구와 교환 가능하다. 제품 개성이 시스템 전체에 흐르기보다 캐릭터가 나타날 때만 켜지는 레이어에 머문다.

자동 검사기는 Rust 소스를 스캔하지 않아 결과가 빈 배열이었다. 이는 clean 판정이 아니라 지원 파일 0개의 false-clean이다. DOM 없는 Metal/wgpu 창이라 브라우저 오버레이도 성립하지 않았다. 대신 별도 네이티브 인스턴스의 GPU 캡처, 픽셀 대비 계산, 렌더·히트 영역 소스를 증거로 사용했다.

## Overall Impression

단단한 네이티브 작업도구 위에 매력적인 학생 세계가 올라가 있다. 가장 큰 기회는 장식을 더하는 것이 아니라, 토스트·빠른 편집기·설정 같은 보조 창에도 같은 제목 체계, 같은 타이포 역할, 같은 조작 순서를 적용하는 것이다.

## What's Working

- 런타임 팔레트와 평평한 표면, 얇은 경계가 대부분의 GPU 창에서 유지된다.
- 학생 방은 이름·색·스프라이트와 작업 맥락을 함께 보존해 제품 정체성이 가장 강하다.
- 선택 상태, 미저장 보호, 빈 입력의 저장 비활성화, 긴 피드백 본문의 줄바꿈은 안정적이다.

## Priority Issues

### [P1] 토스트의 정보 계층이 실제로 읽기 어렵다

본문은 12px `text_mute`이며 배경 대비가 약 3.17:1이다. 제목은 선명하지만 완료 이유와 다음 행동이 흐리고, 긴 제목·본문은 말줄임표나 줄바꿈 없이 잘린다. 모서리 14px도 런타임 형태 토큰을 타지 않아 다른 창과 실루엣이 갈린다.

**Fix:** 본문을 `text_dim` 이상으로 올리고 2줄 제한+말줄임표를 제공한다. 닫기와 열기의 영역을 시각적으로 분리하고, 반지름·경계·그림자를 공통 surface 토큰으로 옮긴다.

**Suggested command:** `$impeccable polish`

### [P1] 커스텀 GPU 조작이 마우스로만 존재한다

보조 창 헤더의 26px 아이콘, 계정 조작 24px, 배너 닫기 28px에는 접근성 이름·키보드 포커스·보이는 포커스 링이 확인되지 않는다. 시간 제한 배너의 정지도 pointer hover에만 있다.

**Fix:** 공통 보조창 헤더 컴포넌트에 접근 가능한 이름, Tab 순서, 포커스 링, Escape/Enter 동등 조작을 정의한다. 클릭 표적은 최소 36–40px로 통일한다.

**Suggested command:** `$impeccable audit`

### [P1] 실패가 무음이거나 창 밖으로 넘친다

일부 GPU/보조 창 생성 실패는 stderr에만 남아 사용자는 클릭이 안 먹은 것으로 본다. 설정 토스트는 메시지 폭을 그대로 재서 긴 오류일 때 화면 밖으로 나갈 수 있다.

**Fix:** 실패를 현재 창 안의 복구 가능한 알림으로 모으고, 오류 문구는 최대 폭·줄바꿈·재시도 또는 원문 열기 조작을 갖게 한다.

**Suggested command:** `$impeccable harden`

### [P2] 보조 창마다 제목과 창 조작 문법이 다르다

팝아웃 편집기, 터미널 방, 토스트가 제목·상태·닫기·되돌리기 순서를 서로 다르게 쓴다. undo 모양은 메인 창으로 복귀, minus는 작업을 살린 채 접기라는 뜻인데 아이콘만 보고 알 수 없다.

**Fix:** 모든 GPU 보조 창에 `맥락/제목 · 상태 · 보조 조작 · 닫기` 순서의 공통 헤더를 적용하고, 내부 pane 번호보다 학생 이름과 작업명을 먼저 보인다.

**Suggested command:** `$impeccable clarify`

### [P2] 빠른 편집기가 본 편집기처럼 보이지만 기능은 다르다

찾기·자동완성·진단·접기·멀티커서가 빠져 있으나 외형은 전체 편집기와 같다. 숙련자는 익숙한 기능이 조용히 실패한다고 느낀다.

**Fix:** 기능을 맞추거나 화면에 `빠른 편집`임을 명확히 표시하고 `본 편집기에서 열기`를 주요 보조 조작으로 제공한다.

**Suggested command:** `$impeccable harden`

## Persona Red Flags

**Alex — Power User:** 팝아웃 편집기에서 Cmd+F와 진단을 기대하지만 반응이 없고, pin·접기·복귀는 키보드 경로가 없다.

**Sam — Accessibility-dependent:** GPU 텍스트와 아이콘을 VoiceOver가 읽을 구조가 없고, 작은 닫기 표적과 hover 전용 시간 정지는 동등한 조작을 제공하지 않는다.

**kasaterm First-timer:** `%0`, `ctx`, Raw와 아이콘 전용 복귀·접기의 의미를 첫날부터 기억해야 한다. 학생 방에서는 맥락이 잘 보이지만 일반 팝아웃으로 나가면 그 도움을 잃는다.

## Minor Observations

- 토스트 X의 형태와 위치는 명확해졌지만 배경과 외곽 경계가 비슷한 앱 위에서 약하다.
- 기본 본문 텍스트는 약 11.14:1, `text_dim`은 약 5.28:1로 충분하므로 새 색을 만들 필요가 없다.
- 설정의 기본 CTA도 작은 글자 기준 대비가 부족하다.
- 네이티브 설정 창은 현재 fallback 경로라, 기본 웹 설정 화면과 별도 제품처럼 갈릴 가능성이 있다.

## Questions to Consider

- 토스트와 모든 보조 창의 공통 헤더·타이포·표면을 먼저 한 벌로 묶을 것인가?
- 팝아웃 편집기를 완전한 편집기로 키울 것인가, 빠른 임시 편집기로 정직하게 이름 붙일 것인가?
- 학생 정체성을 에이전트가 있는 창의 보너스로 둘 것인가, 일반 보조 창에도 작게 이어갈 것인가?
