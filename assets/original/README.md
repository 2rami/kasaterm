# Original KASA artwork

These three static illustrations were generated on 2026-09-28 with
`openai/gpt-image-2.5-sunburst` through OpenGateway, using the project's original
umbrella-twin reference. The school-style clothing was replaced with original
blue and amber rainwear. The reference itself is not redistributed here.

| File | Purpose | SHA-256 |
|---|---|---|
| `twins.png` | Shared KASA identity | `7fdd93e2afcfef8c178ac4554f7d30d16fc758ca3dde68ad025ac7af39266dbf` |
| `sky.png` | Sky, internal slug `kasa_sky` | `23337f1558858300cb92a7c73b77c0fd2f29d7418c9a756cc792df256ef12d0c` |
| `amber.png` | Amber, internal slug `kasa_amber` | `1570b5ce9abaa7d80a162b3f2d628e7f81170b1d1e03318517606f1432d2612c` |

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
