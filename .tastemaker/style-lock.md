# KASA style lock

Established: 2026-09-28. Source: existing native UI and the user's rebranding request.

## Canonical contract

[`docs/design.md`](../docs/design.md) is the single source for colors, dimensions, typography, shape and motion. This file is a routing record, not a second token table. Its native application rules take precedence over web-only skill defaults.

The surfaces are a Rust/wgpu desktop app and Flutter mobile app. Extend their existing controls; do not substitute HTML mockups, introduce a web component stack, or add marketing-page motion.

## Current direction

- Keep terminal content and controls readable in both light and dark themes. Mobile login label contrast is checked by `mobile/test/mobile_account_layout_test.dart`; this is not a claim that every legacy color pairing was audited.
- Remove default franchise branding without destroying users' character bindings, custom themes or stored identifiers. Original art is available in `assets/original/`; native and web bundled-character migration remains separate work.
- Mobile UI uses bundled Pretendard regular/semibold; terminal cells retain the existing monospace and Korean fallback contract. `KASA Mobile` is the display name, not a new bundle identifier.
- Rain-wet glass is a future visual direction, not a shipped shader. Repeated input and scrolling must not acquire decorative motion.
- The existing original umbrella icon remains the application mark. Mobile includes only the new original Sky/Amber static art and twins illustration; generated assets and exact prompts are documented in `assets/original/README.md`.

## Decisions and scope

Project evidence is in `decisions.log`. The exact font and English title are agent choices pending visual review; the request to remove franchise branding and keep glass effects as future direction is explicit user input. No cross-project personal profile was changed.
