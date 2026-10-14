# Work hub reference

Reviewed: 2026-09-28. Mode: Operate. Native desktop task status and conversation, restrained motion and medium-high information density.

## Quality bar

- [Paperclip](https://github.com/paperclipai/paperclip): README and IssuesList/WhatNeedsMe implementation inspected, not a running UI. Borrow task ownership, attention-first decisions and evidence, not its company hierarchy, palette or assets.
- Existing KASA Mobile NachoHome: conversation and task data share the same journal. Reuse the protocol rather than sending chat text to a terminal pane.
- Existing KASA native controls and `docs/design.md`: preserve theme roles, Korean UI typography, outlined actions and immediate keyboard focus.
- User screenshot: the rejected schedule target grid repeats a device label across hundreds of session buttons. The human surface must not be a raw session inventory.

## Direction contract

- First viewport: user response needed, ongoing work, results; adjacent Nacho conversation on wide screens.
- The selected task supplies explicit context; selection alone never submits a message or approval.
- Sources and verification remain visible in details. A disconnected device is not a completed task.
- No calendar, company hierarchy, new web mockup, marketing animation, or duplicate operations rail.
- Exact spacing and cross-theme contrast of the final rendered implementation must be checked before handoff; no visual approval is implied by the reference analysis.

## Native review

- Actual isolated debug captures reviewed at 1280×820 (dark, task/chat split) and 760×720 (light, chat tab). Fixture labels remain visible; no claim of live-account data or performance measurement.
- Header actions, independent work viewport, wrapped conversation and fixed composer remained inside their bounds. Layout/caret tests cover 280–2560px widths and zero-advance input.
- Existing theme roles and SVG/native controls reused; the chat icon comes from Lucide ISC. No web component library or generated mock screen was introduced.
- Self-review: hierarchy4, specificity4, restraint5, evidence4, clarity4, structural fit5. Desktop synthetic layout reviewed; real Korean IME composition and authenticated live conversation remain separate checks.

## Desktop settings refinement — 2026-10-14

Design read: native settings for daily terminal use, Operate mode, existing KASA visual system; variance 3, motion 1, density 7, art direction 3.

Reference field: documentation and repository source inspected, not live external product screens.

- [Fluent Field](https://fluent2.microsoft.design/components/web/react/core/field/usage/): persistent labels and local validation feedback; adapt the pattern, not React components.
- [GOV.UK Text input](https://design-system.service.gov.uk/components/text-input/): labelled entry and supplementary hints.
- [GOV.UK Error summary](https://design-system.service.gov.uk/components/error-summary/): keep errors actionable and close to the affected form. This two-field native form uses inline feedback, not the web component.
- Existing `native_controls.rs`: outlined controls and immediate keyboard focus, with readable primary/danger text.
- Existing `SettingsScene` and `docs/design.md`: preserve room ownership, clipping, shared tokens and reversible input editing.

Direction contract: the first viewport identifies this device and other connections. Connection edits are drafts; cancel and validation failures do not write the roster. Display names and transport names are separate. Search ranks an exact menu name ahead of incidental mentions, preserves Unicode input, and does not pause account synchronization. Avoid duplicate device inventories, overlapping placeholders, silent save-on-blur for connection forms, and decorative motion.

Native evidence: isolated dark/light captures at 1280×900 and 640×720, an eleven-page settings tour, and real save/cancel button clicks against a fake roster. No real connection or account was changed. Actual Korean IME composition and native Windows execution remain outside these checks.

Review: hierarchy 4, specificity 5, restraint 5, state clarity 4, evidence 4, structural fit 5. Uses existing SVG icons and native controls; no new web framework, palette, illustration, or personal taste profile. Visual acceptance remains pending user review.

## Shared file browsing — 2026-10-14

Design read: native cross-device file collection for daily use, Operate mode, existing KASA system; variance 3, motion 1, density 7, art direction 3. Public help pages were read; external signed-in product screens were not operated or captured.

- [Drive for desktop](https://support.google.com/drive/answer/10838124?hl=en): paired application/Finder entry, search, recent sync activity and explicit status.
- [Dropbox sync status](https://help.dropbox.com/sync/check-sync-status): distinguish connecting, syncing, paused and current rather than using one generic success claim.
- [OneDrive files in Explorer](https://support.microsoft.com/en-us/sharepoint/sync/work-with-synced-files-in-file-explorer): keep familiar folder access alongside the application view.
- Existing KASA native controls, Flutter `Look`, `SettingsGroup`, SVG icons and bundled Pretendard remain the implementation foundation. Component-drawer search found no applicable native file-browser component; no React registry or web mockup was introduced.

Direction contract: recent files and search are the first viewport; folders and previews remain reachable without creating a desktop workspace. Moving or hiding a shortcut does not move data. Peer receive success is not global upload completion. Avoid unnamed folder-only entry points, unbounded thumbnail decoding and late preview responses replacing a newer selection.

Evidence: isolated native 1280×900 dark and 640×720 light captures, real preview and wheel events; Flutter 320/390/430px light/dark layout checks and 390×844 real-font renders. Physical phone and Korean IME use are not implied. Review: show-tell 4, philosophy 4, hierarchy 4, specificity 5, restraint 5, structural fit 5. Exact visuals remain pending review; no personal profile promotion.
