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

The first terminal frame binds its PTY id even when layout/resize created the
pane first. Without that binding, visible agent output could remain absent from
the readiness lookup, leaving a running restored conversation unable to accept
keyboard input. Resuming it again in another Codex process would then report an
active writer because the original conversation was still running.

Only initial layout construction blocks the main window globally. Afterwards,
ready surfaces and app controls remain usable while the card is visible.
Retry reconnects unfinished links without reconstructing existing panes.
If transport bytes arrived but the current frame is still unapplied, retry
requests a fresh snapshot on that link. The request expires when its connection
generation changes, and already ready panes are left connected.
Creating a pane or room on a source device does not automatically add a viewer
on another device. Each device retains its explicitly selected mirrors and layout.
Pointer actions on the card do not pass through to the terminal behind it;
dropping an existing drag on the card cancels the gesture.

After layout construction, saving retains newly opened/moved panes and rooms.
Unfinished surfaces keep their original resume information instead of saving a
temporary shell as the restored conversation. Explicit close/hide removes only
those targets from progress; automatic process failure is not treated as a
user cancellation. Failed spawns absent from the live layout remain recoverable
in the saved snapshot without creating another running pane.

Verification uses isolated state, fixture connections and helper shells only.
Never restart either user's desktop app automatically to test restoration.
