---
name: "kasaterm"
description: "SCHALE 클린 블루 작업영역과 활성 터미널 팔레트를 연결하는 데스크톱 작업 OS."
colors:
  sky-primary: "#4A90E2"
  sky-soft: "#A9CBF0"
  cloud-white: "#FFFFFF"
  sky-wash: "#EAF3FC"
  paper-blue: "#F5FAFE"
  blue-border: "#D6E6F5"
  navy-ink: "#15294A"
  navy-secondary: "#25406B"
  navy-muted: "#4A638F"
  night-bg: "#1B2541"
  night-surface: "#1F2A48"
  night-text: "#EAF2FF"
  settings-bg-default: "#252C35"
  settings-surface-default: "#1A1D23"
  settings-hover-default: "#303843"
  settings-active-default: "#3C4654"
  settings-border-default: "#505C6E6E"
  settings-accent-default: "#5A8CE6"
  settings-text-default: "#ECEEF3"
  settings-text-dim-default: "#A0A6B0"
  settings-text-muted-default: "#787E8A"
  status-success: "#3FB950"
  status-danger: "#E0584E"
  status-attention: "#FA8C2A"
typography:
  display:
    fontFamily: '"Pretendard Variable", Pretendard, "Noto Sans KR", system-ui, sans-serif'
    fontSize: "20px"
    lineHeight: "28px"
  headline:
    fontFamily: '"Pretendard Variable", Pretendard, "Noto Sans KR", system-ui, sans-serif'
    fontSize: "15px"
    lineHeight: "22px"
  body:
    fontFamily: '"Pretendard Variable", Pretendard, "Noto Sans KR", system-ui, sans-serif'
    fontSize: "14px"
    lineHeight: "20px"
  label:
    fontFamily: '"Pretendard Variable", Pretendard, "Noto Sans KR", system-ui, sans-serif'
    fontSize: "13px"
    lineHeight: "18px"
  mono:
    fontFamily: '"JetBrains Mono", ui-monospace, "SF Mono", monospace'
    fontSize: "14px"
    lineHeight: "20px"
  settings-title:
    fontFamily: '-apple-system, BlinkMacSystemFont, "Segoe UI", system-ui, sans-serif'
    fontSize: "24px"
    fontWeight: 600
  onboarding-title:
    fontFamily: '-apple-system, BlinkMacSystemFont, "Segoe UI", system-ui, sans-serif'
    fontSize: "clamp(25px, 3vw, 34px)"
    fontWeight: 690
    lineHeight: 1.22
    letterSpacing: "-0.03em"
  settings-label:
    fontFamily: '-apple-system, BlinkMacSystemFont, "Segoe UI", system-ui, sans-serif'
    fontSize: "12.5px"
    fontWeight: 500
rounded:
  sm: "6px"
  md: "9px"
  dot: "50%"
spacing:
  "0": "0px"
  "1": "4px"
  "2": "8px"
  "3": "12px"
  "4": "16px"
  "5": "24px"
  "6": "32px"
  "7": "48px"
  "8": "64px"
components:
  settings-button-primary:
    backgroundColor: "{colors.settings-accent-default}"
    textColor: "{colors.settings-surface-default}"
    typography: "{typography.settings-label}"
    rounded: "{rounded.md}"
    padding: "0 14px"
    height: "40px"
  settings-button-secondary:
    backgroundColor: "{colors.settings-hover-default}"
    textColor: "{colors.settings-text-default}"
    typography: "{typography.settings-label}"
    rounded: "{rounded.md}"
    padding: "0 14px"
    height: "40px"
  settings-input:
    backgroundColor: "{colors.settings-surface-default}"
    textColor: "{colors.settings-text-default}"
    typography: "{typography.label}"
    rounded: "{rounded.sm}"
    padding: "8px 10px"
  settings-card:
    backgroundColor: "{colors.settings-bg-default}"
    textColor: "{colors.settings-text-default}"
    rounded: "{rounded.md}"
    padding: "24px"
  settings-nav-active:
    backgroundColor: "{colors.settings-active-default}"
    textColor: "{colors.settings-text-default}"
    typography: "{typography.label}"
    rounded: "{rounded.sm}"
    padding: "8px 10px"
---

# Design System: kasaterm

## Overview

**Creative North Star: "SCHALE 작업대"**

`SCHALE 작업대`는 코드의 “SCHALE OS 클린 블루” 주석과 실제 설정 동작을 설명하기 위한 문서용 이름이다. 새로운 브랜드 약속이 아니다. 기본 작업영역은 흰색·연하늘 표면, 네이비 텍스트, 단일 하늘색 강조를 쓰며, 다크 모드에서는 깊은 네이비 층으로 같은 역할을 반전한다.

설정과 첫 실행 온보딩은 별도 세계를 만들지 않는다. 현재 터미널 팔레트와 형태를 `--kt-*` 런타임 토큰으로 받아 창 옆의 터미널과 맞추고, 서버 응답 전이나 실패 시에만 문서화된 다크 폴백을 쓴다. 화면은 얇은 경계, 절제된 곡률, 촘촘한 산세리프 계층으로 작업 상태와 선택을 빠르게 읽게 한다.

**Key Characteristics:**

- 밝은 하늘색 작업영역과 깊은 네이비 텍스트의 클린 블루 기반
- 설정·온보딩에서 활성 터미널 팔레트와 형태를 그대로 잇는 런타임 테마
- 얇은 경계와 톤 차이로 구분하는 평평한 정보 구조
- 작은 보조문구와 CTA까지 대비와 키보드 상태를 드러내는 조작 체계

## Colors

기본 작업영역은 차가운 흰색과 연하늘 표면 위에 네이비 잉크를 놓고, SCHALE Sky 한 색으로 선택과 포커스를 묶는다. 설정과 온보딩은 같은 역할 구조를 유지하되 실제 값은 현재 터미널 팔레트가 공급한다.

### Primary

- **SCHALE Sky** (`colors.sky-primary`): 기본 작업영역의 포커스 링, 브랜드 강조, 선택 상태, 조절 손잡이에 사용한다.
- **Soft Sky** (`colors.sky-soft`): 선택·호버처럼 낮은 강도의 면 강조에 사용한다.
- **Runtime Accent** (`colors.settings-accent-default`): 설정·온보딩의 선택, 포커스, 주요 CTA가 사용하는 폴백 값이다. 실제 실행 중에는 `--kt-accent`가 현재 터미널 강조색으로 대체한다.

### Secondary

- **Success Green** (`colors.status-success`): 로그인 완료, 가져오기 완료, 준비 완료처럼 성공이 확정된 상태에만 사용한다.
- **Danger Red** (`colors.status-danger`): 요청 실패와 오류 알림에 사용한다.
- **Attention Amber** (`colors.status-attention`): 로그인 필요, 재시작 필요처럼 사용자의 다음 조작을 기다리는 상태에 사용한다.

### Neutral

- **Cloud / Sky Wash / Paper Blue** (`colors.cloud-white`, `colors.sky-wash`, `colors.paper-blue`): 기본 작업영역의 바탕, 낮은 층, 카드 층을 만든다.
- **Navy Ink Family** (`colors.navy-ink`, `colors.navy-secondary`, `colors.navy-muted`): 본문, 보조 본문, 비활성 정보의 순서를 만든다.
- **Night Surfaces** (`colors.night-bg`, `colors.night-surface`, `colors.night-text`): 작업영역의 명시적 다크 테마다.
- **Settings Runtime Neutrals** (`colors.settings-bg-default`부터 `colors.settings-text-muted-default`까지): `--kt-*` 응답을 기다리는 첫 프레임과 조회 실패 때만 쓰는 설정·온보딩 폴백이다.

### Named Rules

**The One Palette Owner Rule.** 기본 작업영역은 `--cth-*`, 설정과 온보딩은 서버가 주입하는 `--kt-*`를 사용한다. 설정 엔트리에 클린 블루 Tailwind 매핑을 가져와 런타임 터미널 팔레트를 덮지 않는다.

**The 4.5:1 Working Copy Rule.** 작은 안내문과 CTA 레이블도 실제 배경에서 최소 4.5:1 대비를 유지한다. 온보딩은 이 기준 때문에 `text-mute`를 한 단계 선명한 `text-dim`으로 재매핑한다.

## Typography

**Display Font:** Pretendard Variable, Pretendard, Noto Sans KR, system sans-serif
**Body Font:** Pretendard Variable, Pretendard, Noto Sans KR, system sans-serif
**Label/Mono Font:** JetBrains Mono, UI monospace fallbacks

**Character:** 기본 작업영역은 한글과 영문을 함께 읽기 좋은 Pretendard 계열을 쓴다. 오프라인에서도 반드시 떠야 하는 설정과 온보딩은 OS 산세리프를 쓰고, 터미널 명령·경로·색상값만 모노 계열로 제한한다.

### Hierarchy

- **Display** (`typography.display`): 작업영역의 가장 높은 UI 제목이다.
- **Headline** (`typography.headline`): 패널과 설정 섹션 제목이다.
- **Body** (`typography.body`): 기본 설명과 작업 내용이다.
- **Label** (`typography.label`): 행 레이블, 상태, 보조 정보다.
- **Mono** (`typography.mono`): 터미널 명령, 경로, 해시, 설정값이다.
- **Settings Title** (`typography.settings-title`): 설정 탭의 페이지 제목이다.
- **Onboarding Title** (`typography.onboarding-title`): 첫 실행 단계의 유동형 제목이다.

### Named Rules

**The Values-Only Mono Rule.** 한글 UI 문장과 조작 레이블을 모노로 만들지 않는다. 모노는 명령, 경로, 해시, 터미널 미리보기처럼 고정폭이 의미를 주는 값에만 쓴다.

## Layout

기본 작업영역은 창 전체를 채우며 패널 사이를 촘촘한 간격 척도로 정렬한다. 공통 간격은 `spacing`의 4px 기반 단계에서 고르고, 임의의 새 간격 척도를 만들지 않는다.

설정은 고정 폭 188px 내비게이션과 `min-width: 0`인 유동 본문으로 나뉜다. 폼 행은 레이블 영역을 최소 180px로 유지하다가 공간이 부족하면 `flex-wrap`으로 세로 배치되어 가로 스크롤을 만들지 않는다.

온보딩은 넓은 창에서 168px 진행 내비게이션과 최대 820px 본문을 나란히 둔다. 780px 이하에서는 한 열과 가로 4단계 진행표로 바뀌고, 가져오기 CTA는 전체 폭으로 내려간다. 560px 이하에서는 헤더와 카드 내부를 세로로 쌓고, 진행 레이블을 숨겨 숫자·완료 표시만 남기며, 에이전트·셸·요약 행과 CTA를 한 열로 바꾼다.

## Elevation & Depth

기본 작업영역은 낮고 부드러운 그림자를 일부 외곽 도구에 쓰지만, 설정과 온보딩은 기본적으로 평평하다. 배경·표면·호버·활성의 톤 차이와 한 줄 경계가 층을 만들며, 카드와 컨트롤의 인셋 경계가 위치를 고정한다. 픽셀 형태 프리셋이 선택된 경우에만 런타임 형태 토큰이 블러 없는 오프셋 그림자를 허용한다.

### Shadow Vocabulary

- **Workspace Soft Shadow** (`0 4px 12px rgba(21, 41, 74, 0.10)`): 기본 작업영역의 떠 있는 요소에 제한적으로 사용한다.
- **Settings Hairline Inset** (`inset 0 0 0 var(--kt-border-w) var(--kt-border)`): 설정 카드, 보조 버튼, 세그먼트의 구조 경계다.
- **Settings Button Lift** (`0 2px 8px rgba(74, 144, 226, 0.30)`): 기본 작업영역의 설정 진입 버튼 호버에만 쓰인다.

### Named Rules

**The Flat-by-Default Rule.** 설정과 온보딩의 정지 상태에는 새 드롭 섀도를 더하지 않는다. 먼저 표면 톤과 한 줄 경계로 깊이를 표현한다.

## Shapes

설정·온보딩의 기본 실루엣은 작은 컨트롤에 `rounded.sm`, 큰 카드와 주요 버튼에 `rounded.md`, 점·스위치 손잡이·상태 마크에 `rounded.dot`을 쓴다. 이 값은 기본 폴백이며 실행 중에는 서버의 형태 토큰이 같은 역할을 유지한 채 곡률, 경계 두께, 점의 둥글기를 바꿀 수 있다.

형태는 색과 독립된 축이다. 팔레트 값을 바꿔 모서리를 흉내 내지 않고, 카드 안에 원형 의미를 가진 마크는 일반 카드 반지름에서 계산하지 않는다.

## Components

### Buttons

- **Shape:** 주요·보조 CTA는 중간 곡률을 사용하며, 온보딩 버튼은 최소 높이 40px와 좌우 14px 패딩을 갖는다.
- **Primary:** 런타임 강조색 배경과 표면색 레이블을 사용하고 굵기 700으로 확정 조작을 드러낸다.
- **Secondary:** 호버 표면과 한 줄 경계를 사용하며, 활성 호버에서는 한 단계 진한 표면으로 이동한다.
- **Focus / Disabled:** 온보딩의 버튼은 2px 강조색 외곽선과 2px 오프셋을 사용한다. 비활성 조작은 커서를 기본으로 돌리고 투명도를 낮춘다.

### Segmented Controls and Toggles

- **Segmented:** 한 줄 경계 트랙 안에서 선택 칸만 활성 표면과 더 굵은 레이블을 쓴다. 선택 상태는 `aria-pressed`로 함께 전달한다.
- **Toggle:** 40×22px 트랙과 16px 손잡이를 사용한다. 켜짐은 런타임 강조색, 꺼짐은 호버 표면이며 손잡이 이동으로 상태를 보강한다.

### Cards / Containers

- **Corner Style:** 섹션과 탭 카드는 중간 곡률을 사용한다.
- **Background:** 바깥은 표면, 내용 카드는 배경 토큰을 사용해 한 단계 밝거나 어두운 층을 만든다.
- **Shadow Strategy:** 설정·온보딩 카드는 드롭 섀도 없이 한 줄 경계 또는 인셋 경계를 사용한다.
- **Internal Padding:** 설정 탭 카드는 24px, 온보딩 섹션은 화면 폭에 따라 18–26px 범위의 유동 패딩을 사용한다.

### Inputs / Fields

- **Style:** 표면 배경, 현재 텍스트색, 작은 곡률, 1px 런타임 경계를 사용한다. 값과 경로만 모노로 전환한다.
- **Focus:** 텍스트 필드는 이웃 칸을 덮는 그림자 대신 경계를 강조색으로 바꾼다. 온보딩의 입력과 선택은 2px 외곽선도 받는다.
- **Interaction:** Enter와 blur는 값을 확정하고 Escape는 편집값을 되돌린 뒤 필드를 빠져나간다.

### Navigation

- **Settings:** 선택된 행은 활성 표면과 왼쪽 3px 강조 막대를 사용하며, 비선택 행은 보조 텍스트색을 쓴다.
- **Onboarding:** 현재 단계는 강조색으로 채우고 `aria-current="step"`을 제공한다. 아직 도달하지 않은 단계는 비활성화한다.
- **Responsive:** 780px 이하에서는 진행 내비게이션이 가로 4열로 바뀌고, 560px 이하에서는 텍스트 레이블을 숨긴다.

### Choice Cards and Radio Rows

- **Theme Choice:** 최소 146px 폭의 유동 격자 안에서 104px 높이의 터미널 미리보기를 보여 주고, 선택 카드는 강조색 1px 경계를 받는다.
- **Agent / Shell Choice:** `radiogroup`과 roving tab stop을 사용하고, 방향키·Home·End로 선택과 포커스를 함께 이동한다.
- **Status:** 성공, 주의, 오류는 색뿐 아니라 아이콘과 텍스트 상태를 함께 제공한다.

## Do's and Don'ts

### Do:

- **Do** 기본 작업영역은 `--cth-*`, 설정·온보딩은 `--kt-*` 역할 토큰으로 구현한다.
- **Do** 작은 안내문과 CTA 레이블을 포함한 실제 텍스트 대비를 배경별로 확인하고 최소 4.5:1을 지킨다.
- **Do** 모든 버튼·입력·선택 컨트롤에 보이는 포커스 상태와 의미 있는 선택 속성을 제공한다.
- **Do** 온보딩을 780px와 560px 경계에서 구현된 한 열 구조로 재배치하고 `prefers-reduced-motion`에서 진입·회전 애니메이션을 제거한다.

### Don't:

- **Don't** 설정 엔트리에 클린 블루 전역 매핑을 가져와 사용자가 고른 터미널 팔레트를 덮지 않는다.
- **Don't** 보조 설명에 대비가 부족한 `text-mute`를 그대로 사용하거나 색만으로 성공·주의·오류를 구분하지 않는다.
- **Don't** 설정·온보딩 카드에 임의의 새 그림자, 곡률, 색상값을 추가해 런타임 토큰 체계를 우회하지 않는다.
- **Don't** 좁은 창에서 데스크톱 행을 억지로 유지해 가로 스크롤이나 잘린 CTA를 만들지 않는다.
