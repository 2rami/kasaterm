# Mobile desktop controls

The hub device header opens **PC 기능**. The same entry is available in Settings as
**기기별 PC 기능**. An account connection pins the chosen desktop's stable machine
route before opening its controls. Reconnecting must not send a setting or a
session resume to whichever desktop becomes the gateway's new default.

## Coverage

| Desktop capability | Mobile entry |
| --- | --- |
| Live conversation, questions, approval, terminal, shell blocks | Existing pane screen |
| Photos, result files, images, audio and video | Existing conversation cards; player adds volume and mute |
| Offline conversation reading | Existing saved conversations |
| New room, room rename/close, pane close/swap | Hub room/pane menus |
| Split direction, pane move between rooms, split ratio, desktop focus | Expanded pane menu |
| Start Claude or Codex in a shell | Pane menu → 에이전트 시작 |
| Resume Claude/Codex/AGY history | PC 기능 → 대화 기록 이어하기; exact session, harness and cwd; new room |
| Default agent, per-provider permissions, Claude model/effort/options | PC 기능 → PC 설정 |
| Agent status line, account autoswitch/threshold | PC 설정 → 에이전트 |
| Color themes, system light/dark palettes, accent, shape, contrast, font size, zoom/reset | PC 설정 → 외형 |
| Shell, starting folder, file opening app/command, file tree, autosave | PC 설정 → 셸·파일 |
| Tab placement, pane header, all cursor shapes/thickness, mouse pointer, scroll speed | PC 설정 → PC 배치·입력 |
| Claude relogin, character selection, device names, browser destination | Existing Settings screens |
| Device reorder, room hide/resize, pane hide/restore/detach, cross-device transfer | PC 기능 → PC 화면 조작 |
| File editor/tree mutations, Git panel, work board, MCP, character/theme authoring, advanced account/status-bar configuration | PC 기능 → PC 화면 조작 |

The last two rows operate the desktop UI. They are not independent native phone
editors. Desktop online status, display capture and input permission are required.
Offline reading remains limited to saved conversations; mutations are not queued
while the desktop is unavailable.

## Interaction

Screen viewing starts read-only. In control mode, **스크롤** sends a wheel gesture;
**끌기·선택** holds the left mouse button through the gesture. Cancellation, leaving
control mode, backgrounding and disposal release the pointer. Long press remains
right click. Shift, Control, Option/Alt and Command/Windows modifiers apply to a
click, drag or key and clear after the operation. Letter, number, punctuation,
navigation and function keys use the existing screen protocol.

Sheets scroll within the screen instead of overflowing with many panes or large
text. Desktop settings only expose fields supplied by the source version. HTTP
200 with an application error is a failure, including settings and pane close;
the phone preserves the previous displayed setting and shows the error.

## Verification

- `desktop_tools_test.dart`: exact machine routing, server refusal, malformed
  responses, settings search, 320/390/430 px with 200% text and keyboard,
  Codex resume identity/path and pane movement.
- `screen_session_test.dart` and `screen_mirror_control_test.dart`: real loopback
  WebSocket pointer, drag cancellation, modifiers, scroll and no input in view mode.
- `inline_media_test.dart`: player volume/mute, playback and existing media lifecycle.
- `integration_test/desktop_tools_test.dart`: native iOS navigation, keyboard,
  settings mutation and Codex resume against an isolated HTTP fixture.
