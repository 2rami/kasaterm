// Byte streams for the comparison, each about the size of a long busy session. Built from a fixed
// seed so every run and every terminal sees the same bytes.
import { readdirSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { readCast } from '../test/oracle.mjs';

const here = dirname(fileURLToPath(import.meta.url));
const enc = s => Buffer.from(s, 'utf8');

function rng(seed) {
  return () => { seed = (seed * 1103515245 + 12345) & 0x7fffffff; return seed / 0x7fffffff; };
}

function lines(target, make) {
  const parts = [];
  let size = 0;
  for (let i = 0; size < target; i++) { const b = enc(make(i)); parts.push(b); size += b.length; }
  return Buffer.concat(parts);
}

export function workloads(scale = 1) {
  const r = rng(7);
  const MB = 1 << 20;
  const words = ['alpha', 'build', 'cargo', 'deploy', 'error', 'fetch', 'grid', 'hello', 'index', 'json', 'kasaterm', 'lint'];
  const hangul = ['한글', '터미널', '시험', '다람쥐', '헌', '쳇바퀴에', '타고파', '가나다라마바사', '넓은', '글자'];
  const out = {};
  out.ascii = lines(16 * MB * scale, i => `-rw-r--r--  1 user  staff  ${String(i * 37 % 99999).padStart(5)} Oct 14 22:${String(i % 60).padStart(2, '0')} file-${i}.txt\r\n`);
  out.sgr = lines(16 * MB * scale, i => {
    let s = '';
    for (let k = 0; k < 10; k++) {
      const w = words[Math.floor(r() * words.length)];
      const pick = Math.floor(r() * 4);
      s += pick === 0 ? `\x1b[3${k % 8}m${w}` : pick === 1 ? `\x1b[1;38;5;${Math.floor(r() * 256)}m${w}`
        : pick === 2 ? `\x1b[38;2;${Math.floor(r() * 256)};${Math.floor(r() * 256)};90m${w}` : `\x1b[4;48;5;${Math.floor(r() * 256)}m${w}`;
      s += '\x1b[0m ';
    }
    return s + '\r\n';
  });
  out.korean = lines(16 * MB * scale, i => {
    let s = `${i} `;
    for (let k = 0; k < 9; k++) s += hangul[Math.floor(r() * hangul.length)] + ' ';
    return s + (i % 7 === 0 ? '🙂 ' : '') + '\r\n';
  });
  // Full-screen redraws at 120x40, like htop or a TUI repainting every frame.
  out.tui = lines(16 * MB * scale, f => {
    let s = '\x1b[?2026h\x1b[H';
    for (let y = 1; y <= 40; y++) {
      s += `\x1b[${y};1H`;
      const bar = Math.floor((Math.sin((f + y) / 7) + 1) * 25);
      s += `\x1b[36m${String(y).padStart(3)}\x1b[0m [\x1b[32m${'|'.repeat(bar)}\x1b[31m${'|'.repeat(50 - bar)}\x1b[0m] `;
      s += `\x1b[1m${words[(f + y) % words.length].padEnd(12)}\x1b[0m ${hangul[(f * y) % hangul.length]} ${(f * y % 1000 / 10).toFixed(1)}%\x1b[K`;
    }
    return s + '\x1b[?2026l';
  });
  // The recorded sessions (Claude, Codex, vim, htop, less, zsh...) back to back.
  const corpus = join(here, '..', 'corpus');
  const casts = readdirSync(corpus).filter(f => /\.kcast(\.gz)?$/.test(f)).sort()
    .map(f => Buffer.concat(readCast(join(corpus, f)).events.filter(e => e.kind === 'o').map(e => e.data)));
  const one = Buffer.concat(casts.map(c => Buffer.concat([c, enc('\x1b[?1049l\x1b[?2026l\x1b[0m\x1bc')])));
  out.recordings = Buffer.concat(Array.from({ length: Math.max(1, Math.ceil(8 * MB * scale / one.length)) }, () => one));
  return out;
}

export function chunks(buf, size = 65536) {
  const out = [];
  for (let i = 0; i < buf.length; i += size) out.push(buf.subarray(i, i + size));
  return out;
}
