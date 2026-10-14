// Keys, IME, paste, selection, wheel and mouse reports in a real browser (headless Chromium).
// Needs Playwright: `npm i -g playwright` (not a dependency of the library), then
// `node build.mjs && node test-view/input.mjs`.
import { createRequire } from 'node:module';
import { execSync } from 'node:child_process';
import { dirname, join } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const require = createRequire(import.meta.url);
const { chromium } = require(join(execSync('npm root -g').toString().trim(), 'playwright'));
const page_url = pathToFileURL(join(dirname(fileURLToPath(import.meta.url)), 'page.html')).href;
let failed = 0;
const browser = await chromium.launch();
const ctx = await browser.newContext({ viewport: { width: 1000, height: 640 }, permissions: ['clipboard-read', 'clipboard-write'] });
const page = await ctx.newPage();
page.on('pageerror', e => console.log('pageerror:', e.message));
await page.goto(page_url);
const take = () => page.evaluate(() => { const o = out.join(''); out.length = 0; return o; });
const check = (name, got, want) => { if (got !== want) failed++; console.log(got === want ? 'ok  ' : 'FAIL', name, got === want ? '' : `${JSON.stringify(got)} want ${JSON.stringify(want)}`); };
await page.mouse.click(200, 200);
await page.keyboard.type('ls -la');
await page.waitForTimeout(50);
check('type', await take(), 'ls -la');
await page.keyboard.press('Enter'); check('enter', await take(), '\r');
await page.keyboard.press('Shift+Enter'); check('shift+enter', await take(), '\n');
await page.keyboard.press('ArrowUp'); check('up', await take(), '\x1b[A');
await page.evaluate(() => term.write('\x1b[?1h')); await page.keyboard.press('ArrowUp'); check('up appcursor', await take(), '\x1bOA');
await page.keyboard.press('Control+ArrowLeft'); check('ctrl+left', await take(), '\x1b[1;5D');
await page.keyboard.press('Control+c'); check('ctrl+c', await take(), '\x03');
await page.keyboard.press('Alt+ArrowLeft'); check('alt+left', await take(), '\x1bb');
await page.keyboard.press('Backspace'); check('backspace', await take(), '\x7f');
await page.keyboard.press('Shift+Tab'); check('shift+tab', await take(), '\x1b[Z');
await page.keyboard.press('F5'); check('f5', await take(), '\x1b[15~');
await page.keyboard.press('Meta+Backspace'); check('cmd+backspace', await take(), '\x15');
await page.keyboard.press('Escape'); check('esc', await take(), '\x1b');
// Desktop IME: composition events, one commit at the end.
const cdp = await ctx.newCDPSession(page);
for (const t of ['ㅎ', '하', '한']) await cdp.send('Input.imeSetComposition', { text: t, selectionStart: t.length, selectionEnd: t.length });
check('composing sends nothing', await take(), '');
await cdp.send('Input.insertText', { text: '한' });
await page.waitForTimeout(30);
check('ime commit', await take(), '한');
for (const t of ['ㄱ', '그', '글']) await cdp.send('Input.imeSetComposition', { text: t, selectionStart: t.length, selectionEnd: t.length });
await cdp.send('Input.insertText', { text: '글' });
await page.waitForTimeout(30);
check('ime commit 2', await take(), '글');
// Phone keyboard: no composition events, the textarea just changes.
await page.evaluate(() => { const ta = term.view.textarea; ta.value = ''; term.view.prevInput = '';
  for (const v of ['ㅇ', '이', '일']) { ta.value = v; ta.dispatchEvent(new Event('input')); term.view.flushNow(); } });
check('phone ime', await take(), 'ㅇ\x7f이\x7f일');
// Paste, plain and bracketed.
const paste = text => page.evaluate(t => { const dt = new DataTransfer(); dt.setData('text/plain', t);
  term.view.textarea.dispatchEvent(new ClipboardEvent('paste', { clipboardData: dt, bubbles: true, cancelable: true })); }, text);
await paste('a\nb'); check('paste', await take(), 'a\rb');
await page.evaluate(() => term.write('\x1b[?2004h')); await paste('x\ny'); check('bracketed paste', await take(), '\x1b[200~x\ry\x1b[201~');
// Selection by drag, then copy.
await page.evaluate(() => term.write('\x1b[2J\x1b[Hhello world 한글 끝\r\nsecond line'));
await page.waitForTimeout(50);
const cell = await page.evaluate(() => term.view.cell);
const box = await page.locator('canvas').first().boundingBox();
const at = (col, row) => [box.x + (col + 0.5) * cell.width, box.y + (row + 0.5) * cell.height];
await page.mouse.move(...at(6, 0)); await page.mouse.down(); await page.mouse.move(...at(5, 1), { steps: 4 }); await page.mouse.up();
check('selection text', await page.evaluate(() => term.view.selectionText()), 'world 한글 끝\nsecond');
await page.mouse.dblclick(...at(14, 0));
check('word select', await page.evaluate(() => term.view.selectionText()), '한글');
// Wheel: view scrollback, then mouse reports.
await page.evaluate(() => { for (let i = 0; i < 100; i++) term.write(`line ${i}\r\n`); });
await page.mouse.move(...at(3, 3)); await page.mouse.wheel(0, -200);
check('wheel scrolls view', await page.evaluate(() => term.grid.displayOffset > 0), true);
await page.keyboard.type('z'); await page.waitForTimeout(30); take();
check('typing returns to bottom', await page.evaluate(() => term.grid.displayOffset), 0);
await page.evaluate(() => term.write('\x1b[?1000h\x1b[?1006h'));
await page.mouse.wheel(0, -60); await page.waitForTimeout(30);
check('wheel report', (await take()).slice(0, 12), '\x1b[<64;4;4M\x1b[');
await page.mouse.click(...at(2, 1));
check('click report', await take(), '\x1b[<0;3;2M\x1b[<0;3;2m');
await page.evaluate(() => term.write('\x1b[?1000l\x1b[?1049h'));
await page.mouse.wheel(0, 60); await page.waitForTimeout(30);
check('alt screen wheel → arrows', (await take()).slice(0, 3), '\x1bOB');
await page.evaluate(() => term.write('\x1b[?1004h')); await page.evaluate(() => term.view.textarea.blur());
check('focus out', await take(), '\x1b[O');
await browser.close();
process.exit(failed ? 1 : 0);
