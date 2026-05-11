# tmuxify

Warp 같은 GUI 터미널 + tmux 백엔드 + AI 통합 시도.

iTerm2 외엔 tmux control mode (`-CC`) native 통합이 없어서 자체 제작.

## 현재 상태 (PoC week 1)

- Tauri (Rust) + React + TypeScript 스캐폴드
- tmux `-C` subprocess 띄우고 `%` 이벤트 라인 단위 파싱 → 프론트에 emit
- 프론트에서 tmux command 직접 송신
- 이벤트 로그 뷰 + 명령 입력창

## 개발

```bash
npm install
npm run tauri dev
```

`start` 버튼 → tmux 가 `main` 세션에 -C 모드로 attach/new. 들어오는 이벤트 라이브 표시.
명령 입력창에 `list-windows -F '#{window_id} #{window_name}'` 같은 거 쳐서 응답 확인.

## 다음 단계

- [ ] %output 데이터를 xterm.js terminal emulator 에 연결
- [ ] %layout-change 파싱해서 native 분할 트리 구축
- [ ] %window-add/close 로 native 탭 동기화
- [ ] AI 사이드바 (Anthropic SDK) — 모든 pane capture 자동 + 명령 제안

## 메모

자세한 설계 / 프로토콜 참고는 메모리:
- `experiments/project_custom_tmux_terminal.md`
- `experiments/reference_tmux_control_mode_protocol.md`
