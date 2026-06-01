# kasa-cells

**A framework-neutral GPU cell renderer for terminal-style grids.**
The rendering half of a terminal emulator — the part everyone
re-implements by hand — packaged as one small crate.

Rust has excellent terminal *parsers* (`alacritty_terminal`,
`wezterm-term`, `vte`): they give you the cell grid, escape handling,
and scrollback. None of them draw pixels — by design. So every project
that wants to embed a terminal ends up writing its own GPU renderer.
`kasa-cells` is that renderer, made reusable.

```text
bytes ──► alacritty_terminal / wezterm-term ──► cell grid ──► kasa-cells ──► pixels
          (parser: not this crate)                            (this crate)
```

## Why this one

- **Framework-neutral.** It consumes `CellInstance` arrays and a wgpu
  device — *not* a caller-side grid type and *not* a UI framework's
  paint path. Embeds under winit, egui, iced, or a bare wgpu surface.
  (Existing options — `iced_term`, `egui_term` — are bound to a single
  framework's widget tree.)
- **Atlas-cached, not shape-per-frame.** Glyphs bake into a swash atlas
  once per `(codepoint, weight, style, size)`. Each frame is one
  instance-buffer write + one draw call. A 164×63 grid that costs
  ~30–50 ms with shape-every-glyph drops to well under a millisecond.
- **Batteries included.** Per-cell RGBA, bold/italic, CJK/wide-char
  layout, emoji bitmaps, Nerd-icon cell fitting, box-drawing quads, and
  an optional sRGB→DisplayP3 conversion in the shader. Two Nerd fonts
  are bundled, so icons render with no system-font install.

## Status

Extracted from [kasaterm](https://github.com/2rami/kasaterm), where it
is the live terminal renderer. The public surface (`Atlas`, `Pipeline`,
`Shaper`, `CellInstance`, `GlyphKey`) is small but still pre-1.0 — minor
versions may break it until 1.0.

## Quick look

```rust
use kasa_cells::{Atlas, GlyphKey, Pipeline, Shaper};

// 1. load a font (or use a bundled one: kasa_cells::CASCADIA_CODE_NF)
let mut shaper = Shaper::from_path("/System/Library/Fonts/Menlo.ttc", 0)?;

// 2. one atlas + one pipeline per surface
let mut atlas = Atlas::new(&device, &queue, 2048);
let pipeline  = Pipeline::new(&device, surface_format, 16_384);

// 3. per frame: bake glyphs you need, push one CellInstance per cell
let entry = atlas.get_or_bake(&device, &queue, &mut shaper, GlyphKey {
    ch: 'A', bold: false, italic: false, size_px: 28, font: 0,
}).unwrap();
// build CellInstance { cell_px, uv_min, uv_max, fg_rgba, .. } from `entry`,
// then pipeline.write_instances(&device, &queue, &instances) and draw.
```

A complete, runnable window:

```sh
cargo run --release -p kasa-cells --example grid_bw
```

## License

Dual-licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.

Bundled fonts are under their own licenses — see
[assets/THIRD-PARTY-FONTS.md](assets/THIRD-PARTY-FONTS.md).
