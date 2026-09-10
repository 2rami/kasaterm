# Device viewer follow-ups — 2026-09-10

User priority: finish the desktop mirroring/theme/restoration fixes first. The
frontend browsing workflow below was implemented separately on
`hifumi/device-viewer` (contract: `docs/browse-target.md`).

## Done (2026-09-10, hifumi/device-viewer)

- Embedded web pane inside a pane's tab stack: `kasaterm-cli web --tab`, and
  every human-facing open when the destination is 「내장 웹」. Web tabs inside a
  terminal pane survive restart (`tabs[].web_url`).
- Render web content for the selected device: when the Mobile popover picks a
  phone, embedded web panes take that phone's reported CSS viewport and a
  mobile UA (letterboxed inside the pane, pageZoom keeps CSS px exact), and
  KasaChrome `browser_new_tab`/`browser_new_window` auto-emulate the same phone.
- Desktop bottom-bar Mobile popover: 「페이지 여는 곳」 (auto / this machine /
  listed machines / phones) and 「여는 방식」 (내장 웹 / 브라우저). Picking a
  machine also moves the KasaChrome selection (existing propagation to sources).
- Mobile app: settings card 「브라우징」 with the same choices, `POST /mobile/device`
  screen registration, `/mobile/ws` control socket (open-url → in-app web screen
  or Safari, `opened` reply), `GET /browse/devices`.
- Propagation into agent routes: `kasaterm-cli open` / shell `open` shim /
  `/open-url` follow `browse_device` + `browse_open`; machines get
  `/open-url?local=1[&web=1]`, phones get the control socket (push-notification
  fallback when the app is not in front). Agent inspection (`web`, `browser_*`)
  keeps its own destination and only borrows the device shape.
- Verification of delivery: machine = far host's `{"ok":true}`; phone = `opened`
  ack → toast, 4 s without ack → 「응답하지 않아요」 toast; local = `KASATERM_OPEN_URL_SINK`.

## Done (2026-09-10, hifumi/mobile-pane-add)

- **모바일 pane 추가**: hub app bar 「+」 (pick machine when more than one is
  online → pick room or 「새 방」 → `surface.split` next to the room's first pane,
  or `window.new`), and a 「pane 추가」 action in the session screen's top bar
  (「옆에 쪼개기」 = `surface.split from:<viewed>`, 「탭으로」 = `surface.new_tab
  outer:<viewed>`; `surface.new_tab` added to the HTTP `POST /cmd` allowlist).
  Same command as the desktop pane header, so only a shell spawns. The new pane
  id comes back from `result.surface.id`; the hub re-reads the list at once
  (`HubModel.locateNew`, a few retries) and opens the new pane. Mirror panes
  send the command to the machine the mirror lives on, which applies the
  local-shell rule from `docs/mirror-viewer-lifecycle.md`. Not verified on a
  real phone (install needs approval); `hub_minimap` golden is 1.18% off here
  (1.06% before this change — the extra is the new app-bar icon).

## Not verified yet

- Real phone/simulator run of the mobile app (install requires approval).
- Push-notification tap path for `open-url` alerts (the phone app handles the
  socket path; the APNs `kind: open-url:<mode>` payload is sent but the app does
  not yet open the URL from a notification tap).
- A viewer mirroring a pane receiving `mode: web` end-to-end across two live
  apps (unit-level plumbing is in place: `RemoteOpenUrl(pane, url, mode)`).

## Next items (from the user, 2026-09-10 — not started)

- **알림 통일** — done 2026-09-10 on the desktop/phone side (`docs/notifications.md`,
  commit a0c3d5a4) and on the 나쵸네코 side (branch `hifumi/notify-dedup` in
  ~/Desktop/momewomo/nacho-neko, commit c69d8c1 — student alerts now default off,
  not pushed) and the sentry side (`hifumi/sentry-dedup` in
  ~/Desktop/momewomo/sionic/storm-assistant, single-instance lock + no 429 fallback).
  None is live yet: app rebuild, bot restart, sentry deploy + killing the duplicate
  watchd all await the user.
- **모바일 pane 추가** (user answer 2026-09-10: both places): a 「+」 on the
  phone hub (pick machine/room → new pane) and a 「pane 추가」 action in the
  session screen's top bar (split/tab next to the pane being viewed — the same
  thing the desktop pane bottom bar offers). Owner: 히후미 (may be split).
- **나쵸네코 pane 생성 시 방 정리** (user answer: "방목록, 보드 보고 이쁘게
  정렬하게 못하나"): 나쵸네코 should read the room list / board before spawning
  and place its pane so rooms stay tidy (own room, sorted), rather than dropping
  it into whatever room is active. Owner: 나쵸 side (collaboration), 히후미 to
  provide the room list/board affordance if the CLI lacks one.

- **화면공유 바탕화면이 까맣고 독이 안 보임** (user 2026-09-10): when mirroring the
  Mac mini's screen, the desktop is black and the Dock is missing. Investigated
  2026-09-10 (세이아): not a kasaterm or Screen Sharing defect. The mini's wallpaper
  on every display is `~/Pictures/nacho-midnight.png` (mean brightness 29/255,
  set 2026-09-05 07:42 KST) so the desktop *is* almost black, and the Dock has
  `autohide = 1`, so over Screen Sharing it only appears when the pointer reaches
  the bottom edge. Both are user settings; changing them is the user's call.
- **맥미니가 꺼지면 카사텀 모바일이 안 됨** (user 2026-09-10): the MacBook keeps
  running but the phone app cannot connect. Suspected cause: the gateway/tunnel
  (debimarlene side) lives on the mini, so the MacBook has no public route of its
  own. Not investigated.

Safety/verification: do not restart either desktop app automatically, do not
replace current working panes with older saved sessions, and do not install or
publish a mobile build without the user's approval. Desktop updates are staged;
tell the user when to restart. Treat localhost routing and remote authentication
as explicit integration work, not as solved merely by opening a URL on the host.
