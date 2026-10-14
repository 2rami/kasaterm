// Bundles src/ into one dependency-free file (`dist/kasaterm.js`): the sources are plain scripts
// sharing one scope, wrapped in a UMD shell so the same file loads in a page and in node.
import { readFileSync, writeFileSync, mkdirSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = dirname(fileURLToPath(import.meta.url));
const ORDER = ['width-table.js', 'width.js', 'nfc-table.js', 'nfc.js', 'grid.js', 'terminal.js', 'buffer.js', 'images.js', 'box-table.js', 'render.js', 'image-layer.js', 'view.js'];
const EXPORTS = ['Terminal', 'Grid', 'Row', 'charWidth', 'MODE', 'View', 'Renderer', 'themeFromTokens', 'DEFAULT_THEME',
  'DEFAULT_FG', 'DEFAULT_BG', 'COLOR_INDEXED', 'COLOR_RGB',
  'INVERSE', 'BOLD', 'ITALIC', 'UNDERLINE', 'WRAPLINE', 'WIDE_CHAR', 'WIDE_CHAR_SPACER', 'DIM', 'HIDDEN',
  'STRIKEOUT', 'LEADING_WIDE_CHAR_SPACER', 'DOUBLE_UNDERLINE', 'UNDERCURL', 'DOTTED_UNDERLINE', 'DASHED_UNDERLINE',
  'ALL_UNDERLINES', 'BLINK', 'OVERLINE', 'PROTECTED', 'XMODE'];

const body = ORDER.map(f => `// ── ${f} ──\n` + readFileSync(join(root, 'src', f), 'utf8')).join('\n');
const out = `/*! kasaterm.js — terminal emulator for the web, built from web/kasaterm-js (MIT) */
(function (root, factory) {
  if (typeof module === 'object' && module.exports) module.exports = factory();
  else root.KasaTerm = factory();
})(typeof self !== 'undefined' ? self : this, function () {
'use strict';
${body}
return { ${EXPORTS.join(', ')} };
});
`;
mkdirSync(join(root, 'dist'), { recursive: true });
writeFileSync(join(root, 'dist', 'kasaterm.js'), out);
// The web terminal (`/term`) serves this copy, compiled into the app.
writeFileSync(join(root, '..', '..', 'crates', 'kasa-mcp', 'assets', 'term', 'kasaterm.js'), out);
console.log(`dist/kasaterm.js + crates/kasa-mcp/assets/term/kasaterm.js ${out.length} bytes`);
