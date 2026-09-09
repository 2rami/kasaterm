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

## Not verified yet

- Real phone/simulator run of the mobile app (install requires approval).
- Push-notification tap path for `open-url` alerts (the phone app handles the
  socket path; the APNs `kind: open-url:<mode>` payload is sent but the app does
  not yet open the URL from a notification tap).
- A viewer mirroring a pane receiving `mode: web` end-to-end across two live
  apps (unit-level plumbing is in place: `RemoteOpenUrl(pane, url, mode)`).

## Next items (from the user, 2026-09-10 — not started)

- **알림 통일**: notifications from 나쵸네코 / mobile / PC / mini should be one
  consistent system. Known bugs: 나쵸네코 sends the same alert three times, and
  odd/irrelevant alerts appear. Owner: 나쵸 (collaboration with the desktop side).
- **모바일 pane 추가** (user answer 2026-09-10: both places): a 「+」 on the
  phone hub (pick machine/room → new pane) and a 「pane 추가」 action in the
  session screen's top bar (split/tab next to the pane being viewed — the same
  thing the desktop pane bottom bar offers). Owner: 히후미 (may be split).
- **나쵸네코 pane 생성 시 방 정리** (user answer: "방목록, 보드 보고 이쁘게
  정렬하게 못하나"): 나쵸네코 should read the room list / board before spawning
  and place its pane so rooms stay tidy (own room, sorted), rather than dropping
  it into whatever room is active. Owner: 나쵸 side (collaboration), 히후미 to
  provide the room list/board affordance if the CLI lacks one.

Safety/verification: do not restart either desktop app automatically, do not
replace current working panes with older saved sessions, and do not install or
publish a mobile build without the user's approval. Desktop updates are staged;
tell the user when to restart. Treat localhost routing and remote authentication
as explicit integration work, not as solved merely by opening a URL on the host.
