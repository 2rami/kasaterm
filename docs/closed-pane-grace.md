# Closed panes: execution and recovery are separate

Ordinary pane close starts a ten-second undo grace measured from the close,
not from idle detection. Activity and new messages never reset that deadline.
The GUI schedules a one-shot wakeup so a quiet/minimized app still expires it.

At close, all terminal tabs in the pane reject input. Local tasks receive one
interrupt request; a mirror never forwards that interruption to its source.
Claude's incoming-prompt/tool hook checks an app-socket-scoped close marker so
stale peer rosters cannot silently restart a closed student. Sender checks also
recognize the peer session name. Reopening removes the marker and input gate,
without replaying rejected messages or restarting canceled work automatically.

At expiry, owned local processes terminate explicitly even if a viewer retains
an Arc to the terminal. The recovery record remains in the bounded closed-pane
list and is saved across app restarts. Reopening after expiry restores from that
record rather than attaching to a dead process. Explicit stash is exempt.

`KASATERM_CLOSED_GRACE_SECS` is the isolated-test override. The native regression
`KASATERM_AUTOCLOSEGRACE=1` covers input rejection, same-session early undo,
actual expiry, stash survival, serialized records and new-session late undo.
Always isolate session/settings/window/students/socket paths when running it.

App relaunch helpers must be one-shot and must not be installed as persistent
restart jobs. Do not restart production apps as part of this verification.
