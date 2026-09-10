# Device viewer follow-ups — 2026-09-10

User priority: finish the desktop mirroring/theme/restoration fixes first. The
following frontend browsing workflow is separate and may be implemented later.

- Put an embedded web pane inside a pane's tab stack so it can be used remotely
  without requiring another side-by-side split.
- Render web content for the device currently viewing it, including mobile,
  instead of inheriting the originating desktop's viewport dimensions.
- In the desktop bottom-bar Mobile settings, select both the device to use and
  whether browsing should open in an embedded web pane or Chrome.
- Expose the same active-device selection in the mobile app.
- Propagate the selected device and browser destination into KasaChrome/MCP and
  agent URL-opening routes. The user should not have to name a device in every
  request. Preserve the distinction between agent inspection and a page opened
  for the user; verify the selected target actually receives the page.

Safety/verification: do not restart either desktop app automatically, do not
replace current working panes with older saved sessions, and do not install or
publish a mobile build without the user's approval. Desktop updates are staged;
tell the user when to restart. Treat localhost routing and remote authentication
as explicit integration work, not as solved merely by opening a URL on the host.
