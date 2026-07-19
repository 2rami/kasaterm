# kasaterm Windows 세팅 (회사 컴퓨터용)

회사 컴퓨터가 Windows일 때 kasaterm을 설치하고 Claude Code까지 쓰는 최소 절차.
(맥이면 이 문서 대신 releases에서 `.dmg`를 받아 Applications에 드래그하면 끝.)

## 1. kasaterm 설치

1. https://github.com/2rami/kasaterm/releases/latest 에서 `kasaterm-*-x86_64.msi` 다운로드
2. 실행 → SmartScreen 경고가 뜨면 **"추가 정보" → "실행"**
3. 설치 후 시작 메뉴/바탕화면의 kasaterm 실행

- 자동 업데이트 내장(WinSparkle) — 새 버전이 나오면 켤 때 알림이 뜬다.
- 아로나 웹뷰 UI는 v0.1.11부터 MSI에 번들돼 있어 별도 설정 불필요.
- 웹뷰가 안 뜨는 경우에만: WebView2 런타임 확인
  (Win10/11은 Edge에 포함돼 거의 항상 있음. 없으면
  https://developer.microsoft.com/microsoft-edge/webview2/ 에서 Evergreen 설치)

## 2. 셸 (권장: Git for Windows)

kasaterm 기본 셸 선택 순서: PowerShell 7 → Git Bash → Windows PowerShell.
사이드바 "+" 버튼으로 설치된 셸(PowerShell/CMD/Git Bash/WSL) 중 골라 열 수 있고,
설정 화면에서 기본 셸 지정 가능.

- **Git Bash 권장** — `ls`/`grep` 등 맥 zsh 워크플로우와 가장 가깝고,
  claude 관련 부가 기능(셸 스크립트 기반)이 온전히 동작한다.
- Git for Windows: https://git-scm.com/download/win (기본 옵션으로 설치)
- PowerShell만 있어도 터미널·claude 기본 사용은 된다(cwd 표시도 동작 —
  프롬프트 래퍼 자동 주입).

## 3. Claude Code

```
# Node.js LTS 설치 (https://nodejs.org 또는 winget install OpenJS.NodeJS.LTS)
npm install -g @anthropic-ai/claude-code
```

kasaterm 안에서 `claude` 실행 → 브라우저 로그인 (회사 계정 정책 확인).
로그인하면 pane 헤더의 claude 상태 감지(승인 대기 뱃지 등)가 그대로 동작한다.

## 4. 알려진 제약 (Windows)

- **한글 IME**: 조합 입력 경로는 실기에서만 검증 가능. 이상하면 증상을 메모해 둘 것
  (`KASATERM_IME_DEBUG=1` 로 키 코드포인트 로깅 가능).
- 회사 보안 소프트웨어가 미서명 MSI를 막으면: 관리자에게 문의하거나
  `msiexec /i kasaterm-*.msi` 를 관리자 콘솔에서 실행.
- 프록시 환경이면 claude 로그인/API가 회사 프록시 설정(`HTTPS_PROXY`)을 따라야 할 수 있다.
