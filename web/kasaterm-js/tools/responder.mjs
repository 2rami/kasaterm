// Answers a recorded program's terminal queries (DA, CPR, colour queries, mode reports) the way
// kasaterm would, by running the stream through kasaterm.js. Used by record.py over stdin/stdout:
//   in:  'o' u32 len bytes  |  'r' u16 cols u16 rows
//   out: u32 len bytes (the reply to that frame; len 0 = nothing to say)
import { createRequire } from 'node:module';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const require = createRequire(import.meta.url);
const { Terminal } = require(join(dirname(fileURLToPath(import.meta.url)), '..', 'dist', 'kasaterm.js'));

// kasa-pty answers with ghostty's palette and the host theme's colours.
const BASE16 = [[0x1d, 0x1f, 0x21], [0xcc, 0x66, 0x66], [0xb5, 0xbd, 0x68], [0xf0, 0xc6, 0x74], [0x81, 0xa2, 0xbe],
  [0xb2, 0x94, 0xbb], [0x8a, 0xbe, 0xb7], [0xc5, 0xc8, 0xc6], [0x66, 0x66, 0x66], [0xd5, 0x4e, 0x53], [0xb9, 0xca, 0x4a],
  [0xe7, 0xc5, 0x47], [0x7a, 0xa6, 0xda], [0xc3, 0x97, 0xd8], [0x70, 0xc0, 0xb1], [0xea, 0xea, 0xea]];
function color(i) {
  if (i < 16) return BASE16[i];
  if (i < 232) { const n = i - 16, s = [0, 95, 135, 175, 215, 255]; return [s[Math.floor(n / 36)], s[Math.floor(n / 6) % 6], s[n % 6]]; }
  if (i < 256) { const v = 8 + (i - 232) * 10; return [v, v, v]; }
  return { 256: [0xd8, 0xd8, 0xd8], 257: [0x1a, 0x1b, 0x26], 258: [0xd8, 0xd8, 0xd8] }[i] || [0x66, 0x66, 0x66];
}

let term = null;
let pending = [];
let buf = Buffer.alloc(0);

function reply() {
  const body = Buffer.from(pending.join(''), 'utf8');
  pending = [];
  const head = Buffer.alloc(4);
  head.writeUInt32LE(body.length);
  process.stdout.write(Buffer.concat([head, body]));
}

process.stdin.on('data', chunk => {
  buf = Buffer.concat([buf, chunk]);
  for (;;) {
    if (buf.length < 1) return;
    if (buf[0] === 0x72) {
      if (buf.length < 5) return;
      const cols = buf.readUInt16LE(1), rows = buf.readUInt16LE(3);
      buf = buf.subarray(5);
      if (!term) {
        term = new Terminal({ cols, rows });
        term.queryColor = color;
        term.onData(s => pending.push(s));
      } else term.resize(cols, rows);
      reply();
    } else {
      if (buf.length < 5) return;
      const len = buf.readUInt32LE(1);
      if (buf.length < 5 + len) return;
      term.write(buf.subarray(5, 5 + len));
      buf = buf.subarray(5 + len);
      reply();
    }
  }
});
