# Original KASA artwork

These three static illustrations were generated on 2026-09-28 with
`openai/gpt-image-2.5-sunburst` through OpenGateway, using the project's original
umbrella-twin reference. The school-style clothing was replaced with original
blue and amber rainwear. The reference itself is not redistributed here.

| File | Purpose | SHA-256 |
|---|---|---|
| `mobile/assets/original/twins.png` | Shared KASA identity | `7fdd93e2afcfef8c178ac4554f7d30d16fc758ca3dde68ad025ac7af39266dbf` |
| `mobile/assets/original/sky.png` | Sky, internal slug `kasa_sky` | `23337f1558858300cb92a7c73b77c0fd2f29d7418c9a756cc792df256ef12d0c` |
| `mobile/assets/original/amber.png` | Amber, internal slug `kasa_amber` | `1570b5ce9abaa7d80a162b3f2d628e7f81170b1d1e03318517606f1432d2612c` |

All files are 1024×1024 RGBA PNGs. Their four corners have alpha zero;
transparent-pixel counts are 509,920, 632,348 and 741,059 respectively.
RGB color under alpha-zero pixels is invisible data, not a backdrop.
The images were visually inspected and contain no school insignia or halos.

The exact prompts are in `prompts/`. The first pair draft contained a printed
checkerboard; the separate transparency edit produced the final alpha channel.
Only the verified final files above are application assets. These are static
illustrations, not newly generated animation frames. Runtime status indicators
remain responsible for distinguishing work, waiting and idle states.

The generator used the existing Nacho image entrypoint with an explicitly
approved per-process adapter for `background=transparent` and PNG output.
The model was pinned without fallback. Credentials and the adapter's private
runtime paths are not part of the asset bundle.

## Character canon

Sky and Amber, the umbrella twins, are the KASA characters. New artwork follows
the identity in `prompts/kasa-twins-identity.prompt.txt`. Two looks are canonical
and show the same two characters:

| Look | Where it appears | Reference |
|---|---|---|
| Outfit A, rainwear (default) | KASA browser new-tab background (`kasabrowser-go` `internal/home/assets/kasa-garden.png`), the mobile app | `mobile/assets/original/twins.png`, `sky.png`, `amber.png` |
| Outfit B, everyday white blouse, ribbon tie and skirt | Personal site hero illustration | Not in this repository |

`assets/icon_src.png`, the current app icon, predates the twins: Sky alone with
short twin tails, a wink and a white dress. It is not an identity reference for
new artwork.

### Writing a prompt

1. Start with the whole identity block from `prompts/kasa-twins-identity.prompt.txt`.
2. Attach the reference image as an input image. Text alone drifts: hair length,
   bun shape and the umbrella split change between runs.
3. Add only the use: scene, framing, pose, outfit A or B, background.

For app icons and other small sizes, generated drafts that stay readable at
32 px share these traits: a saturated flat background instead of white, the
figures filling about 85% of the canvas, outlines somewhat bolder than the
illustration, and a white rim around the figures. Asking for thicker outlines,
a sticker rim and more contrast pushes the generator toward a glossy vinyl look
with shine streaks; the matte line in the identity block keeps the surface flat,
and repeating it in the use part helps. Check every draft downscaled to 64 px
and 32 px before choosing.
