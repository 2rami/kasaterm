# Desktop restoration progress

The launch choice (restore or start fresh) remains a dialog. After choosing
restore, progress is a nonmodal card at the bottom right, above the bottom bar.
It names the pending student/device when a safe display name is available.
Connection addresses and raw transport errors are not rendered as names.

The progress numerator counts ready saved surfaces, including inactive tabs.
The readiness conditions remain:

- Local terminal: live output and resume commands delivered; a saved agent
  additionally requires agent-process detection for the progress count.
- Mirror: a first live frame belonging to the current connection generation.
- Web: the native web host has been created, not full page/network completion.

This does not prove the exact conversation was resumed or that a remote
application accepted a user command. A live local terminal is deliberately
usable even if agent-process detection is still pending, so login/error prompts
can be handled. Other unfinished surfaces reject text and image input.

Only initial layout construction blocks the main window globally. Afterwards,
ready surfaces and app controls remain usable while the card is visible.
Retry reconnects unfinished links without reconstructing existing panes.
Pointer actions on the card do not pass through to the terminal behind it;
dropping an existing drag on the card cancels the gesture.

Verification uses isolated state, fixture connections and helper shells only.
Never restart either user's desktop app automatically to test restoration.
