// `node tools/view.mjs corpus/x.kcast.gz [event]` — replay a recording through kasaterm.js and
// print the screen (and the last scrollback lines) as plain text.
import { readCast, KasaTerm } from '../test/oracle.mjs';

const cast = readCast(process.argv[2]);
const upto = process.argv[3] ? Number(process.argv[3]) : cast.events.length;
const t = new KasaTerm.Terminal({ cols: cast.header.cols, rows: cast.header.rows, scrollback: 100000 });
cast.events.slice(0, upto).forEach(e => (e.kind === 'r' ? t.resize(e.cols, e.rows) : t.write(e.data)));
const line = y => { let s = ''; const r = t.line(y); for (let x = 0; x < t.cols; x++) if (!(r.flags(x) & KasaTerm.WIDE_CHAR_SPACER)) s += r.text(x); return s.replace(/\s+$/, ''); };
console.log(`${t.cols}x${t.rows} history ${t.historySize} alt ${t.isAltScreen} cursor ${t.cursor.line},${t.cursor.col}`);
for (let y = Math.max(-t.historySize, -5); y < t.rows; y++) console.log((y < 0 ? '~' : '|') + line(y));
