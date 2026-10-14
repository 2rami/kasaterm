// Inline pictures in a real browser: iTerm, kitty PNG, kitty raw RGBA with zlib, kitty Unicode
// placeholders, and the app's rule that an iTerm picture goes once its line is rewritten.
//   node build.mjs && node test-view/images.mjs   (Playwright installed globally)
import { createRequire } from 'node:module';
import { execSync } from 'node:child_process';
import { deflateSync } from 'node:zlib';
import { dirname, join } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const require = createRequire(import.meta.url);
const { chromium } = require(join(execSync('npm root -g').toString().trim(), 'playwright'));
const browser = await chromium.launch();
const page = await browser.newPage({ viewport: { width: 900, height: 520 }, deviceScaleFactor: 1 });
page.on('pageerror', e => console.log('pageerror:', e.message));
await page.goto(pathToFileURL(join(dirname(fileURLToPath(import.meta.url)), 'page.html')).href);
const rgba = Buffer.alloc(16 * 8 * 4);
for (let i = 0; i < 16 * 8; i++) rgba.set([0, 0, 255, 255], i * 4);
let failed = 0;
const check = (name, ok) => { if (!ok) failed++; console.log(ok ? 'ok  ' : 'FAIL', name); };
const r = await page.evaluate(async zlib => {
  await document.fonts.load('13px KasaTerm');
  term.view.remeasure();
  const c = document.createElement('canvas'); c.width = 80; c.height = 40;
  const g = c.getContext('2d'); g.fillStyle = '#f00'; g.fillRect(0, 0, 80, 40); g.fillStyle = '#0f0'; g.fillRect(30, 10, 20, 20);
  const b64 = btoa(String.fromCharCode(...new Uint8Array(await (await new Promise(r => c.toBlob(r))).arrayBuffer())));
  const settle = () => new Promise(r => setTimeout(r, 300));
  const { width: cw, height: ch } = term.view.cell;
  const px = (col, row) => {
    const cv = term.view.imageCanvas, d = cv.getContext('2d').getImageData(Math.round((col + 0.5) * cw * term.view.renderer.dpr), Math.round((row + 0.5) * ch * term.view.renderer.dpr), 1, 1).data;
    return d[3] === 0 ? 'none' : d[1] > 200 ? 'green' : d[0] > 200 ? 'red' : d[2] > 200 ? 'blue' : 'other';
  };
  const out = {};
  term.write(`\x1b]1337;File=name=dGVzdA==;size=${b64.length};width=12;inline=1:${b64}\x07`);
  term.write('\x1b[5;1H');
  term.write(`\x1b_Ga=T,f=100,c=10,r=3,i=7;${b64}\x1b\\`);
  term.write('\x1b[9;1H');
  term.write(`\x1b_Ga=T,f=32,s=16,v=8,o=z,c=8,r=2,i=8;${zlib}\x1b\\`);
  term.write('\x1b[12;1H');
  term.write(`\x1b_Ga=T,f=100,U=1,c=6,r=2,i=9,q=2;${b64}\x1b\\`);
  const D = [0x0305, 0x030D, 0x030E, 0x0310, 0x0312, 0x033D];
  for (let rr = 0; rr < 2; rr++) {
    term.write(`\x1b[${12 + rr};1H\x1b[38;5;9m`);
    for (let cc = 0; cc < 6; cc++) term.write(String.fromCodePoint(0x10EEEE, D[rr], D[cc]));
    term.write('\x1b[0m');
  }
  await settle();
  out.itermCentre = px(6, 1);
  out.kittyPngCentre = px(5, 5);
  out.kittyRgba = px(4, 9);
  out.placeholderCentre = px(3, 12);
  out.placeholderOutside = px(8, 12);
  // Overwriting the line the iTerm picture hangs on removes it.
  term.write('\x1b[1;1Hrewritten');
  await settle();
  out.itermAfterRewrite = px(6, 1);
  out.kittyStays = px(5, 5);
  // Deleting a kitty image removes its placements.
  term.write('\x1b_Ga=d,d=I,i=7\x1b\\');
  await settle();
  out.kittyAfterDelete = px(5, 5);
  return out;
}, deflateSync(rgba).toString('base64'));
check('iTerm picture drawn in its box', r.itermCentre === 'green');
check('kitty PNG fitted to its box', r.kittyPngCentre === 'green');
check('kitty raw RGBA (zlib) drawn', r.kittyRgba === 'blue');
check('kitty placeholder cells show the picture', r.placeholderCentre === 'green' || r.placeholderCentre === 'red');
check('nothing outside the placeholder cells', r.placeholderOutside === 'none');
check('iTerm picture dropped when its line is rewritten', r.itermAfterRewrite === 'none');
check('other pictures stay', r.kittyStays === 'green');
check('kitty delete removes the placement', r.kittyAfterDelete === 'none');
if (failed) console.log(JSON.stringify(r));
await browser.close();
process.exit(failed ? 1 : 0);
