# KASA style lock

Established: 2026-09-28. Source: existing native UI and the user's rebranding request.

## Canonical contract

[`docs/design.md`](../docs/design.md) is the single source for colors, dimensions, typography, shape and motion. This file is a routing record, not a second token table. Its native application rules take precedence over web-only skill defaults.

The surfaces are a Rust/wgpu desktop app and Flutter mobile app. Extend their existing controls; do not substitute HTML mockups, introduce a web component stack, or add marketing-page motion.

## Current direction

- Keep terminal content and controls readable in both light and dark themes. Mobile login label contrast is checked by `mobile/test/mobile_account_layout_test.dart`; this is not a claim that every legacy color pairing was audited.
- Desktop device identity uses neutral icons and names, not device-specific fills or borders. The chat composer footer carries the device and reported runtime status next to the terminal toggle; details remain in `docs/design.md`. Character and attention colors retain their existing meaning.
- Desktop settings remain a native, restrained Operate surface: distinguish this device from other devices, separate display names from connection names, keep connection changes as drafts until explicit save, and make search and editing feedback consistent across pages. Geometry and keyboard rules stay in `docs/design.md`; input and scrolling do not gain decorative motion.
- Shared files retain the desktop-folder entry and gain native search/recent views in the existing desktop side panel and phone screen. Shortcut location is a per-device preference, never an implicit data migration. Describe peer receive checks and offline copies honestly; no cloud-backup guarantee or separate application is implied.
- Remove default franchise branding without destroying users' character bindings, custom themes or stored identifiers. Original art is available in `assets/original/`; native and web bundled-character migration remains separate work.
- Mobile UI uses bundled Pretendard regular/semibold; terminal cells retain the existing monospace and Korean fallback contract. `KASA Mobile` is the display name, not a new bundle identifier.
- Rain-wet glass is a future visual direction, not a shipped shader. Repeated input and scrolling must not acquire decorative motion.
- The existing original umbrella icon remains the application mark. Mobile includes only the new original Sky/Amber static art and twins illustration; generated assets and exact prompts are documented in `assets/original/README.md`.

## Decisions and scope

The human work hub combines task progress and Nacho conversation in one native room. Schedules and the all-session target grid are excluded; agent observation remains an explicit tool. A wide viewport pairs task rows with chat; narrow viewports switch between the two without discarding drafts. Dimensions are in `docs/design.md`. This is a task/communication surface, not an organizational chart or calendar.

Project evidence is in `decisions.log`. The exact font and English title are agent choices pending visual review; the request to remove franchise branding and keep glass effects as future direction is explicit user input. No cross-project personal profile was changed.

## OS surface direction — 2026-10-15

The user requested a desktop-wide redesign inspired by Nava OS, including responsive settings, shared files and color editing. This replaces the previous outline-only controls rule. `docs/design.md` remains the only token contract.

Native wgpu controls use filled selection, quiet neutral surfaces, 10/16px rounded shapes and system UI typography when no UI font is selected. Frost Light/Dark are additional palettes; saved explicit theme and typography choices remain selectable. Compact settings move navigation above the content; shared-file previews move beside the list when space permits. HSV gradients are GPU-calculated rather than CPU swatch grids.

Viewed reference: Nava OS Personalization, https://www.youtube.com/watch?v=E7nGN03tvKI, 00:06–00:52. The source is a concept video, not measured production performance. Frosted material is a visual reference; these native panels do not implement live backdrop blur. No React registry applies to this Rust renderer. Visual choices are pending user review. No cross-project profile promotion.

Color contract: Frost Light text/background 12.39:1 and text/surface 13.99:1; Frost Dark 15.43:1 and 13.01:1. Default blue #5a8ce6 with black label 6.33:1. Subtle borders are decorative; selected controls also have filled state/check marks. Text on accents uses `foreground_on`, and secondary accent text and keyboard focus enforce contrast at the actual background. Shared rounded fills/strokes use one analytic GPU quad rather than row strips, including fractional UI scales.

## Custom colors and Info — 2026-10-15

Accent editing exposes an arbitrary RGB/HEX picker with optional preset shortcuts. Statusbar item colors use the same editor, including reverting to the inherited default. Preview, commit, cancellation and eyedropper callbacks carry an explicit color target. Shared folders live inside Info beside Runtime, with separate scroll/focus/hit maps; the independent Share tab and titlebar Share button are removed. These interaction requirements come from the user; exact screen styling remains subject to review.

## Selective restore — 2026-10-15

The startup restore dialog groups local tabs by room and identifies their titles, agent identities, folders and pane numbers. Checkboxes support individual rooms/tabs and all/none; fixed action buttons survive small window sizes. Unchecked work remains in closed-pane history with the explicit reason 복원 보류. Saved display titles do not become pinned titles. Token dimensions remain in `docs/design.md`; exact styling is pending review.
