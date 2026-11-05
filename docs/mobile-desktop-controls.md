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
| Hide current room members or individual panes on the phone; restore visibility | Room/pane menu, drag action dock, hidden-items sheet |
| Drag panes, select multiple panes, close and undo | Long press a minimap cell or list row; drop on a sibling edge or the action dock |
| Running subagents while their parent plans or waits | Green minimap outline, count and live task list |
| Start Claude or Codex in a shell | Pane menu → 에이전트 시작 |
| Resume Claude/Codex/AGY history | PC 기능 → 대화 기록 이어하기; exact session, harness and cwd; new room |
| Default agent, per-provider permissions, Claude model/effort/options | PC 기능 → PC 설정 |
| Agent status line, account autoswitch/threshold | PC 설정 → 에이전트 |
| Color themes, system light/dark palettes, accent, shape, contrast, font size, zoom/reset | PC 설정 → 외형 |
| Shell, starting folder, file opening app/command, file tree, autosave | PC 설정 → 셸·파일 |
| Tab placement, pane header, all cursor shapes/thickness, mouse pointer, scroll speed | PC 설정 → PC 배치·입력 |
| Claude relogin, character selection, device names, browser destination | Existing Settings screens |
| Desktop device order, desktop sidebar hide/resize, detach, cross-device transfer | PC 기능 → PC 화면 조작 |
| File editor/tree mutations, Git panel, work board, MCP, character/theme authoring, advanced account/status-bar configuration | PC 기능 → PC 화면 조작 |

The last two rows operate the desktop UI. They are not independent native phone
editors. Desktop online status, display capture and input permission are required.
Offline reading remains limited to saved conversations; mutations are not queued
while the desktop is unavailable.

## Interaction

Long press on a minimap cell or list row starts a drag. The bottom dock exposes
selection, hiding, trash and the existing pane menu. Dropping on another pane's
edge moves beside that pane on the same desktop. Selection stays active until
the user finishes it; taps then select additional panes on that device.

Hiding changes only the phone view. A room's hide action hides its current
members, leaving subsequently created panes visible. Hidden membership is scoped
to the connection and stable surface keys, so names, room order and reused pane
numbers do not hide unrelated sessions. Old sources without stable keys only
retain hiding for the current app run. A remaining visible tab replaces the
hidden tab's face without changing desktop geometry. Hidden panes can be restored
individually or together.

Trash obtains complete current addresses and uses the desktop's checked close
operation. Idle shells may close immediately; other sessions require confirmation.
The same addresses are retained through confirmation, so a replacement session
is rejected. Successful closes return restore receipts; partial failures retain
receipts for the panes already closed. Undo uses those receipts.

The desktop's active `term/panes.subagents` list is shown independently of the
parent's state. A parent waiting for a question keeps its orange attention icon
while its running children add a green outline and count. The detail sheet
updates with the hub; finished children disappear from both the outline and list.

Codex child state is derived from acknowledged spawn/followup/interrupt/close
results and runtime-tagged final child notifications. A planning parent or a completed parent
turn does not finish its children. The desktop reads rollout additions on one
worker and retains lifecycle state beyond the bounded transcript tail; snapshots
never wait for disk I/O. A changed child list wakes the existing phone change feed.
Failed launches and user text quoting a notification do not create lifecycle events.

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

- `hub_interactions_test.dart`: drag/drop, multi-selection, hiding and restoration,
  scoped persistence, tab visibility, exact close identity, partial-close undo,
  confirmation cancellation, active-child updates and narrow screens with large text.
- `integration_test/hub_interactions_test.dart`: native iOS minimap actions and
  screenshots using an isolated fixture, with no changes to user sessions.
- `desktop_tools_test.dart`: exact machine routing, server refusal, malformed
  responses, settings search, 320/390/430 px with 200% text and keyboard,
  Codex resume identity/path and pane movement.
- `screen_session_test.dart` and `screen_mirror_control_test.dart`: real loopback
  WebSocket pointer, drag cancellation, modifiers, scroll and no input in view mode.
- `inline_media_test.dart`: player volume/mute, playback and existing media lifecycle.
- `integration_test/desktop_tools_test.dart`: native iOS navigation, keyboard,
  settings mutation and Codex resume against an isolated HTTP fixture.
