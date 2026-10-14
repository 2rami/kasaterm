// `node test/images.mjs` — inline pictures and the replies around them. kitty and iTerm follow
// the app (the cases are kasa-pty kitty.rs's own tests and inline.rs's layout); sixel,
// XTGETTCAP and XTSMGRAPHICS follow the spec and xterm.js's image addon, xterm profile only.
import { createRequire } from 'node:module';
import { deflateSync } from 'node:zlib';

const require = createRequire(import.meta.url);
const K = require('../dist/kasaterm.js');
const ESC = '\x1b', CSI = '\x1b[', OSC = '\x1b]', DCS = '\x1bP', ST = '\x1b\\';
const b64 = bytes => Buffer.from(bytes).toString('base64');

// A PNG as far as anything here reads one: signature and IHDR.
function png(w, h) {
  const b = Buffer.alloc(33);
  Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]).copy(b, 0);
  b.writeUInt32BE(13, 8); b.write('IHDR', 12); b.writeUInt32BE(w, 16); b.writeUInt32BE(h, 20);
  return b;
}

function term(opts = {}) {
  const t = new K.Terminal({ cols: 80, rows: 24, ...opts }), log = { replies: [], images: [], deleted: [] };
  t.onData(d => log.replies.push(d));
  t.on('image', e => log.images.push(e));
  t.on('imagedelete', ids => log.deleted.push(...ids));
  return [t, log];
}
const apc = body => `${ESC}_G${body}${ST}`;

const tests = [];
const test = (name, fn) => tests.push([name, fn]);
const eq = (got, want, what) => {
  if (JSON.stringify(got) !== JSON.stringify(want)) throw new Error(`${what}: want ${JSON.stringify(want)} got ${JSON.stringify(got)}`);
};

// ── kitty (kitty.rs tests) ──

test('kitty query answers OK without storing', () => {
  const [t, log] = term();
  t.write(apc('i=31,s=1,v=1,a=q,t=d,f=24;AAAA'));
  eq([log.replies, log.images.length, t.kittyGraphics.images.length], [['\x1b_Gi=31;OK\x1b\\'], 0, 0], 'state');
});

test('kitty query errors, q=2 silences, no id no answer', () => {
  const [t, log] = term();
  t.write(apc('i=9,a=q,t=d,f=100;AAAA') + apc('i=9,a=q,q=2,t=d,f=100;AAAA') + apc('a=q,t=d,f=24,s=1,v=1;AAAA'));
  eq(log.replies.length, 1, 'one reply');
  if (!log.replies[0].startsWith('\x1b_Gi=9;EBADPNG')) throw new Error(log.replies[0]);
});

test('kitty chunked PNG lands once complete', () => {
  const [t, log] = term();
  const data = b64(png(3, 2));
  t.write(apc(`a=t,f=100,i=5,m=1;${data.slice(0, 8)}`));
  eq([log.replies, t.kittyGraphics.images.length], [[], 0], 'before the last chunk');
  t.write(apc(`m=0;${data.slice(8)}`));
  const im = t.kittyGraphics.image(5);
  eq([log.replies, im.width, im.height, Buffer.from(im.data).equals(png(3, 2))], [['\x1b_Gi=5;OK\x1b\\'], 3, 2, true], 'after');
});

test('kitty raw pixels; short raw data refused; zlib payload accepted', () => {
  const [t, log] = term();
  const rgba = Array.from({ length: 16 }, (_, i) => (i * 9) & 0xff);
  t.write(apc(`a=t,f=32,s=2,v=2,i=1;${b64(rgba)}`) + apc(`a=t,f=24,s=4,v=4,i=2;${b64(new Uint8Array(12))}`)
    + apc(`a=t,f=24,s=4,v=4,o=z,i=3;${b64(deflateSync(Buffer.alloc(48, 7)))}`) + apc('a=t,f=32,s=20000,v=1,i=4;AAAA'));
  eq(log.replies.map(r => r.replace(/^\x1b_G|\x1b\\$/g, '').split(':')[0]), ['i=1;OK', 'i=2;ENODATA', 'i=3;OK', 'i=4;EFBIG'], 'replies');
  eq(Array.from(t.kittyGraphics.image(1).data), rgba, 'pixels kept');
});

test('kitty virtual placement: no reply with q=2, no cursor move, resend replaces', () => {
  const [t, log] = term();
  const cmd = apc(`a=T,U=1,q=2,f=100,i=7,c=4,r=2;${b64(png(8, 8))}`);
  t.write(cmd);
  eq([log.replies, t.cursor.line, t.cursor.col, log.images.length, log.images[0].virtual], [[], 0, 0, 1, true], 'first');
  t.write(cmd);
  eq([t.kittyGraphics.placements.length, log.deleted.length, log.images.length], [1, 1, 2], 'resend');
});

test('kitty placement moves the cursor like kitty', () => {
  const [t, log] = term();
  const cmd = apc(`a=T,f=100,i=1,c=3,r=2;${b64(png(4, 4))}`);
  t.write(cmd);
  eq([t.cursor.line, t.cursor.col], [1, 3], 'right by width, down by height-1');
  t.write(`${CSI}1;79H` + cmd);
  eq([t.cursor.line, t.cursor.col], [2, 0], 'past the right edge: next line, first column');
  t.write(`${CSI}6;1H` + apc('a=p,i=1,C=1,p=4'));
  eq([t.cursor.line, t.cursor.col], [5, 0], 'C=1 stays');
  const last = log.images[log.images.length - 1];
  eq([last.marker.line, last.col, last.cols, last.rows, last.placementId], [5, 0, 1, 1, 4], 'event: 4×4 px at the 8×16 fallback cell');
});

test('kitty delete by id frees data only in uppercase; d=a spares virtual', () => {
  const [t] = term();
  t.write(apc(`a=T,U=1,f=100,i=7,c=4,r=2,q=2;${b64(png(2, 2))}`) + apc('a=d,d=i,i=7'));
  eq([t.kittyGraphics.placements.length, !!t.kittyGraphics.image(7)], [0, true], 'd=i');
  t.write(apc('a=d,d=I,i=7'));
  eq(!!t.kittyGraphics.image(7), false, 'd=I');
  t.write(apc(`a=T,U=1,f=100,i=7,c=4,r=2,q=2;${b64(png(2, 2))}`) + apc(`a=T,f=100,i=8,c=2,r=1,q=2;${b64(png(2, 2))}`) + apc('a=d'));
  eq(t.kittyGraphics.placements.map(p => p.virtual), [true], 'd=a');
});

test('kitty: a new command abandons a half upload', () => {
  const [t, log] = term();
  t.write(apc('a=t,f=100,i=5,m=1;AAAA') + apc(`a=t,f=100,i=6;${b64(png(1, 1))}`));
  eq([log.replies, !!t.kittyGraphics.image(5), !!t.kittyGraphics.image(6)], [['\x1b_Gi=6;OK\x1b\\'], false, true], 'state');
});

test('kitty box follows the aspect when one side is missing', () => {
  const [t, log] = term({ cellPixels: { width: 10, height: 20 } });
  const p = b64(png(100, 100));
  t.write(apc(`a=T,f=100,i=1,c=4,C=1;${p}`) + apc(`a=T,f=100,i=2,r=3,C=1;${p}`) + apc(`a=T,f=100,i=3,C=1;${b64(png(25, 41))}`));
  eq(log.images.map(e => [e.cols, e.rows]), [[4, 2], [6, 3], [3, 3]], 'boxes');
});

test('kitty file media cannot be read from the browser', () => {
  const [t, log] = term();
  t.write(apc(`a=t,t=f,i=4;${b64('/tmp/x.png')}`));
  eq(log.replies, ['\x1b_Gi=4;EBADF:Failed to read image file\x1b\\'], 'reply');
});

// ── iTerm OSC 1337 (inline.rs layout) ──

test('iTerm picture: anchored at the cursor, which stays; app layout', () => {
  const [t, log] = term();
  t.write(`ab${OSC}1337;File=size=123;name=${b64('a.png')};width=10;inline=1:${b64(png(200, 100))}\x07`);
  const e = log.images[0];
  eq([t.cursor.col, e.protocol, e.name, e.col, e.cols, e.rows, e.marker.line, e.mime], [2, 'iterm', 'a.png', 2, 10, 3, 0, 'image/png'], 'event');
  t.write(`\r\n${OSC}1337;File=inline=1:${b64(png(100, 100))}${ST}`);
  eq([log.images[1].cols, log.images[1].rows], [48, 23], '60% of 80 columns, 2.1 cell aspect');
});

test('iTerm: a name right after File= is not read (as the app); same name+size moves', () => {
  const [t, log] = term();
  const body = `File=name=${b64('x')};size=9:${b64(png(10, 10))}`;
  t.write(`${OSC}1337;${body}\x07`);
  t.write(`\r\n\r\n${OSC}1337;${body}\x07`);
  eq([log.images[0].name, log.images[1].id === log.images[0].id, log.images[1].marker.line, t.inlineImages.length], ['', true, 2, 1], 'state');
});

test('iTerm: width clamps to the room left; tiny payload and full scrollback ignored', () => {
  const [t, log] = term({ cols: 30, rows: 3, scrollback: 2 });
  t.write(`${CSI}1;26H${OSC}1337;File=width=50:${b64(png(10, 10))}\x07`);
  eq(log.images[0].cols, 5, 'clamped');
  t.write(`${OSC}1337;File=:${b64([1, 2, 3])}\x07`);
  t.write('\r\n1\r\n2\r\n3\r\n4\r\n5');
  t.write(`${OSC}1337;File=:${b64(png(10, 10))}\x07`);
  eq(log.images.length, 1, 'ignored');
});

test('pictures are dropped when lines renumber: width change, screen switch, ED 3', () => {
  const [t, log] = term();
  const pic = `${OSC}1337;File=:${b64(png(10, 10))}\x07`;
  t.write(pic); t.resize(70, 24);
  t.write(pic); t.write(`${CSI}?1049h`);
  t.write(`${CSI}?1049l\r\n` + pic + '\r\n'.repeat(30) + `${CSI}3J`);
  eq([log.images.length, log.deleted.length, t.inlineImages.length], [3, 3, 0], 'dropped');
});

test('at most 64 pictures; the oldest goes', () => {
  const [t, log] = term();
  for (let i = 0; i < 65; i++) t.write(`${OSC}1337;File=name=${b64('p' + i)};size=${i}:${b64(png(4, 4))}\x07`);
  eq([t.inlineImages.length, log.deleted, log.images[0].id], [64, [log.images[0].id], log.images[0].id], 'evicted');
});

// ── sixel (xterm profile) ──

const px = (e, x, y) => Array.from(e.data.subarray((y * e.width + x) * 4, (y * e.width + x) * 4 + 4));

test('sixel decodes colours, bands, repeats and raster size', () => {
  const [t, log] = term({ profile: 'xterm' });
  t.write(`${DCS}q#1;2;100;0;0#1~~-~$#2;1;240;50;100A${ST}`);
  const e = log.images[0];
  eq([e.protocol, e.width, e.height], ['sixel', 2, 12], 'size');
  eq([px(e, 0, 0), px(e, 1, 5), px(e, 0, 6), px(e, 1, 6), px(e, 0, 7), px(e, 0, 8)],
    [[255, 0, 0, 255], [255, 0, 0, 255], [255, 0, 0, 255], [0, 0, 0, 0], [0, 255, 0, 255], [255, 0, 0, 255]], 'pixels; HLS 240° is green');
  t.write(`${DCS}q#4!3~$!2?N${ST}`);
  eq([log.images[1].width, log.images[1].height, px(log.images[1], 2, 5)], [3, 6, [0, 0, 0, 0]].slice(0, 2).concat([px(log.images[1], 2, 5)]), 'repeat');
  if (px(log.images[1], 2, 5)[3] !== 255 || px(log.images[1], 2, 4)[3] !== 255) throw new Error('repeat painted all three columns');
  t.write(`${DCS}0;1;0q"1;1;10;20#3~${ST}`);
  eq([log.images[2].width, log.images[2].height, log.images[2].transparent], [10, 20, true], 'raster and P2');
});

test('sixel placement: cursor moves down to the last line, back to its column; DECSDM keeps it', () => {
  const [t, log] = term({ profile: 'xterm', cellPixels: { width: 10, height: 20 } });
  const tall = '~-'.repeat(8) + '~';   // 54 px → 3 rows
  t.write(`${CSI}3;5Hx${DCS}q#1;2;0;0;100${tall}${ST}`);
  eq([t.cursor.line, t.cursor.col, log.images[0].marker.line, log.images[0].col, log.images[0].rows], [4, 5, 2, 5, 3], 'scrolling');
  t.write(`${CSI}?80h${DCS}q${tall}${ST}`);
  eq([t.cursor.line, t.cursor.col, log.images[1].marker.line, log.images[1].col], [4, 5, 0, 0], 'DECSDM');
  t.write(`${CSI}?80$p`);
  eq(log.replies, ['\x1b[?80;1$y'], 'DECRQM 80');
});

test('the default profile shows no sixel and answers no XTGETTCAP (as the app)', () => {
  const [t, log] = term();
  t.write(`${CSI}3;5H${DCS}q~-~-~-~${ST}${DCS}+q544E${ST}${CSI}c`);
  eq([log.images.length, t.cursor.line, t.cursor.col, log.replies], [0, 2, 4, ['\x1b[?62;22c']], 'state');
});

// ── XTGETTCAP, XTSMGRAPHICS, DA1 (xterm profile) ──

test('XTGETTCAP', () => {
  const [t, log] = term({ profile: 'xterm' });
  const hex = s => Buffer.from(s).toString('hex').toUpperCase();
  t.write(`${DCS}+q${hex('TN')}${ST}${DCS}+q${hex('Co')};${hex('Tc')};${hex('kbs')}${ST}${DCS}+q${hex('nope')};${hex('Co')}${ST}${DCS}+q${hex('cols')}${ST}`);
  eq(log.replies, [`\x1bP1+r${hex('TN')}=${hex('xterm-256color')}\x1b\\`, `\x1bP1+r${hex('Co')}=${hex('256')}\x1b\\`,
    `\x1bP1+r${hex('Tc')}\x1b\\`, `\x1bP1+r${hex('kbs')}=7F\x1b\\`, '\x1bP0+r\x1b\\', `\x1bP1+r${hex('cols')}=${hex('80')}\x1b\\`], 'replies');
});

test('XTSMGRAPHICS and DA1 with sixel', () => {
  const [t, log] = term({ profile: 'xterm', cellPixels: { width: 10, height: 20 } });
  t.write(`${CSI}?1;1S${CSI}?1;3;256S${CSI}?1;1S${CSI}?1;2S${CSI}?1;4S${CSI}?2;1S${CSI}?2;4S${CSI}?3;1S${CSI}?1;9S${CSI}?1S${CSI}c${ESC}Z`);
  eq(log.replies, ['\x1b[?1;0;4096S', '\x1b[?1;0;256S', '\x1b[?1;0;256S', '\x1b[?1;0;4096S', '\x1b[?1;0;4096S', '\x1b[?2;0;800;480S',
    '\x1b[?2;0;4096;4096S', '\x1b[?3;1S', '\x1b[?1;2S', '\x1b[?62;4;9;22c', '\x1b[?62;4;9;22c'], 'replies');
});

let failed = 0;
for (const [name, fn] of tests) {
  try { fn(); } catch (e) { failed++; console.log(`FAIL ${name}\n  ${e.stack.split('\n').slice(0, 2).join('\n  ')}`); }
}
console.log(`${tests.length - failed}/${tests.length} picture checks pass`);
process.exit(failed ? 1 : 0);
