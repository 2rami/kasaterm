// Hand-written streams for the corners real programs rarely hit in one recording, and a seeded
// fuzzer that strings escape sequences, text and resizes together at random.
const enc = s => Buffer.from(s, 'utf8');
const ESC = '\x1b', CSI = '\x1b[', OSC = '\x1b]', ST = '\x1b\\';

export function cast(cols, rows, steps) {
  const events = steps.map(s => (Array.isArray(s) ? { kind: 'r', cols: s[0], rows: s[1] }
    : { kind: 'o', data: Buffer.isBuffer(s) ? s : enc(s) }));
  return { header: { cols, rows }, events };
}

const lorem = n => Array.from({ length: n }, (_, i) => `line ${i} ` + 'abcdefghij'.repeat(1 + (i % 7))).join('\r\n');

export const CASES = {
  'text-wrap': cast(20, 5, ['hello world this wraps around the edge', '\r\n', lorem(12)]),
  'wide-boundaries': cast(10, 4, ['abcdefghi한', '글자가 넘친다 가나다라마바사아', '\r\n', 'x\x1b[10G한', 'ab\x1b[1;9H한글']),
  'combining': cast(12, 3, ['é ǟ 한 한́ ', '́start', '가‍나', 'x\x1b[1;12Hź']),
  'sgr': cast(40, 6, [`${CSI}1;2;3;4;7;8;9mA${CSI}0mB${CSI}4:3mC${CSI}4:0mD${CSI}38;5;196mE${CSI}48;2;1;2;3mF`,
    `${CSI}38:2::10:20:30mG${CSI}38:2:40:50:60mH${CSI}58;5;33;4mI${CSI}59mJ${CSI}21;22;23;24;27;28;29mK`,
    `${CSI}90;101mL${CSI}39;49mM${CSI}38;2;300;1;1mN${CSI}38;5mO${CSI}1:2mP${CSI}4:2:7mQ${CSI}mR`]),
  'erase': cast(20, 6, [lorem(6), `${CSI}3;5H${CSI}41m${CSI}K${CSI}4;5H${CSI}1K${CSI}5;5H${CSI}2K`,
    `${CSI}2;3H${CSI}1J${CSI}5;10H${CSI}J${CSI}1;1H${CSI}3X${CSI}2;2H${CSI}1J`]),
  'insert-delete': cast(20, 6, [lorem(6), `${CSI}2;5H${CSI}3@${CSI}3;5H${CSI}2P${CSI}4;1H${CSI}2L${CSI}5;1H${CSI}M`,
    `${CSI}1;18H${CSI}5P${CSI}6;1H${CSI}4h한글XY${CSI}4l${CSI}1;1H${CSI}99P${CSI}2;1H${CSI}99@`]),
  'scroll-region': cast(20, 8, [lorem(8), `${CSI}3;6r${CSI}6;1H\nA\nB\nC${CSI}3;1H${ESC}M${ESC}M${CSI}2S${CSI}3T`,
    `${CSI}r${CSI}1;5r${CSI}5;1H\n\n\nX${CSI}2;7r${CSI}?6h${CSI}1;1HO${CSI}10;1HP${CSI}2AQ${CSI}?6l${CSI}r`,
    `${CSI}5;30r${CSI}9;3r${CSI}1;1H${CSI}M${CSI}M${CSI}8;1H${CSI}3M`]),
  'tabs': cast(30, 3, ['a\tb\tc\td\te', `${CSI}1;1H${CSI}3I*${CSI}2Z+${ESC}H`, `${CSI}2;1H\t\t${CSI}g${CSI}2;1H\t\t!${CSI}3g\tq`, `${CSI}3;1H\t\t\t\t\t\tZ\tY`]),
  'dec-special': cast(20, 3, [`${ESC}(0lqqkx  x mqqj${ESC}(B ok`, `\r\n\x0e${ESC})0aaa\x0fbbb`, `\r\n${ESC}*0${ESC}+B_\`~`]),
  'origin-wrap': cast(10, 5, [`${CSI}?7labcdefghijklmnop${CSI}?7h\r\n0123456789AB`, `${CSI}?6h${CSI}2;4r${CSI}H*${CSI}5;5H#`, `${CSI}?6l${CSI}r${CSI}?3h`]),
  'alt-screen': cast(20, 5, [lorem(7), `${CSI}?1049h${CSI}H${CSI}2Jalt screen`, `${CSI}31m\r\nmore${CSI}?1049l`, 'back', `${CSI}?1049h`, [25, 7], 'resized in alt', `${CSI}?1049l`, [15, 4]]),
  'cursor-save': cast(20, 5, [`${CSI}2;3H${CSI}31m${ESC}7${CSI}H${CSI}0mX${ESC}8Y${CSI}s${CSI}5;5HZ${CSI}uW`, `${ESC}(0${ESC}7${ESC}(Bq${ESC}8q`]),
  'queries': cast(20, 5, [`${CSI}c${CSI}>c${CSI}0c${CSI}5n${CSI}3;4H${CSI}6n${CSI}18t${CSI}14t${CSI}?1$p${CSI}?2004h${CSI}?2004$p${CSI}?9999$p${CSI}4$p${CSI}?2026$p${ESC}Z`,
    `${OSC}11;?${ST}${OSC}4;1;?\x07${OSC}10;?;?\x07${CSI}?12$p${CSI}=c${CSI}?u${CSI}>1u`]),
  'osc': cast(30, 4, [`${OSC}0;title one\x07${OSC}2;a;b c ${ST}${CSI}22t${OSC}2;second\x07${CSI}23t`,
    `${OSC}8;id=x;https://example.com/a\x07link${OSC}8;;\x07 ${OSC}8;;file:///tmp/b;c\x07two${OSC}8;;\x07`,
    `${OSC}52;c;aGVsbG8=\x07${OSC}52;p;d29ybGQ=\x07${OSC}52;c;?\x07${OSC}7;file://host/tmp\x07${OSC}133;A\x07$ ${OSC}133;B\x07`,
    `${OSC}4;1;#ff0000\x07${OSC}104\x07${OSC}50;CursorShape=1\x07${CSI}3 q${CSI}?12h${CSI}0 q\x07`]),
  'modes': cast(20, 3, [`${CSI}?1h${CSI}?1000h${CSI}?1002h${CSI}?1006h${CSI}?1004h${CSI}?2004h${CSI}?25l${CSI}?1005h${CSI}=${ESC}=`,
    `${CSI}?1003h${CSI}?1000l${CSI}?7l${CSI}4h${CSI}20h${CSI}?1007l${CSI}?1042l${ESC}>`, `${CSI}?47h${CSI}?1047h${CSI}?1048h`]),
  'reset': cast(20, 4, [lorem(10), `${CSI}31m${CSI}2;3r${CSI}?1049h junk ${ESC}c after`, `${OSC}2;t\x07${ESC}cX`]),
  'ed-scrollback': cast(20, 4, [lorem(10), `${CSI}2J`, `${CSI}H${CSI}2J`, 'x', `${CSI}3J`, `${CSI}2J`, `${CSI}2J`]),
  'decaln-rep': cast(10, 3, [`${ESC}#8`, `${CSI}2;2Hab${CSI}5b`, `${CSI}3;1H한${CSI}3b`]),
  'parser-edges': cast(30, 4, [`a${CSI}1;2;3;4;5;6;7;8;9;10;11;12;13;14;15;16;17;18;19;20;21;22;23;24;25;26;27;28;29;30;31;32;33mB`,
    `${CSI}1${ESC}[31mC${CSI}?1;2\x18D${CSI}2\x07;3HE${ESC}P1;2qpayload\x9c${ESC}\\F${ESC}_apc\x07ignored${ESC}\\G${ESC}^pm${ESC}\\H`,
    `${CSI}  ${CSI}!pI${CSI}99999999999mJ${ESC}%GK${CSI}1;2'zL${CSI}1:2:3:4:5:6:7:8:9:10:11:12:13:14:15:16:17:18:19:20:21:22:23:24:25:26:27:28:29:30:31:32:33mM`,
    Buffer.from([0xe2, 0x82, 0x41, 0x80, 0x9b, 0xc0, 0xaf, 0xed, 0xa0, 0x80, 0xf4, 0x90, 0x80, 0x80, 0xff, 0x41, 0xe2, 0x82]),
    Buffer.from([0xac, 0x1b, 0x5b, 0x33, 0x32, 0x6d, 0xea, 0xb0]), Buffer.from([0x80, 0x5a, 0x7f, 0xc2, 0x85, 0xc2, 0xa0])]),
  'reflow': cast(30, 6, ['short\r\n', 'a long line that is going to wrap around a few times here\r\n', '한글이 섞인 긴 줄은 넓은 글자가 경계에 걸린다 가나다라마바사\r\n', 'tail',
    [17, 6], [11, 5], [40, 8], [7, 3], [30, 6], [9, 9], [60, 4], [30, 6]]),
  'reflow-cursor': cast(12, 4, ['0123456789AB', [10, 4], [14, 4], [12, 4], 'cd\r\nxyz', '0123456789ABCDE', [8, 4], [20, 4], [5, 4], [12, 6], [12, 2]]),
  'reflow-history': cast(16, 4, [lorem(30), [9, 4], [23, 3], [16, 10], [16, 4], [5, 4], [40, 2]]),
};

// Deterministic PRNG (mulberry32).
function rng(seed) {
  return () => {
    seed |= 0; seed = (seed + 0x6d2b79f5) | 0;
    let t = Math.imul(seed ^ (seed >>> 15), 1 | seed);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

const WORDS = ['hello', 'world', '한글', '가나다', '🙂', 'é', '한', '中文字', 'ｗｉｄｅ', 'x', ' ', '\t', '\r', '\n', '\r\n', '\b', 'abc def ghi', '❤️', '👍🏽', '​', '¡'];

// Seeds from 100000 up use a harsher mix: coloured erases, scrollback clears, resets, screen
// switches and long bursts that fill and recycle scrollback.
export function fuzzCast(seed, length = 400) {
  const harsh = seed >= 100000;
  const r = rng(seed);
  const pick = a => a[Math.floor(r() * a.length)];
  const int = n => Math.floor(r() * n);
  let cols = 5 + int(40), rows = 2 + int(12);
  const header = { cols, rows };
  const steps = [];
  const csi = () => {
    const finals = ['A', 'B', 'C', 'D', 'E', 'F', 'G', 'H', 'J', 'K', 'L', 'M', 'P', 'S', 'T', 'X', '@', 'b', 'd', 'r', 'm', 'g', 'I', 'Z', 's', 'u', '`', 'a', 'e', 'f'];
    const f = pick(finals);
    const n = int(3);
    const ps = Array.from({ length: n }, () => (r() < 0.2 ? '' : String(int(r() < 0.8 ? 12 : 60))));
    if (f === 'm') {
      const sgr = pick(['0', '1', '2', '3', '4', '4:3', '7', '9', '22', '24', '27', '31', '42', '93', '38;5;' + int(256), '48;2;' + int(256) + ';' + int(256) + ';9', '58;5;9', '39', '49']);
      return `${CSI}${sgr}m`;
    }
    return `${CSI}${ps.join(';')}${f}`;
  };
  const priv = () => `${CSI}?${pick([1, 6, 7, 25, 1049, 2004, 1000, 1006, 4, 2026])}${pick(['h', 'l'])}`;
  for (let i = 0; i < length; i++) {
    const x = r();
    let s;
    if (harsh && x < 0.12) {
      s = pick([`${CSI}3J`, `${CSI}2J`, `${CSI}J`, `${CSI}1J`, `${CSI}K`, `${ESC}c`, `${CSI}?1049h`, `${CSI}?1049l`,
        `${CSI}4${int(8)}m${CSI}2J`, `${CSI}48;5;${int(256)}m${CSI}${int(4)}K`, `${CSI}4${int(8)}m${CSI}${1 + int(5)}M`,
        lorem(1 + int(rows * 3)), `${CSI}4${int(8)}m\r\n\r\n`, `${CSI}${1 + int(3)};${2 + int(rows)}r`, `${CSI}r`]);
    } else if (x < 0.45) s = Array.from({ length: 1 + int(6) }, () => pick(WORDS)).join('');
    else if (x < 0.75) s = csi();
    else if (x < 0.82) s = priv();
    else if (x < 0.87) s = pick([`${ESC}M`, `${ESC}D`, `${ESC}E`, `${ESC}7`, `${ESC}8`, `${ESC}H`, `${ESC}(0`, `${ESC}(B`, '\x0e', '\x0f', `${CSI}4h`, `${CSI}4l`]);
    else if (x < 0.9) s = pick([`${OSC}8;;http://x/${int(9)}\x07`, `${OSC}8;;\x07`, `${OSC}2;t${int(9)}\x07`, `${CSI}6n`]);
    else if (x < 0.95) { cols = 3 + int(45); rows = 1 + int(14); steps.push([cols, rows]); continue; }
    else s = `${CSI}${int(rows) + 1};${int(cols) + 1}H`;
    steps.push(s);
  }
  // Chunk boundaries fall anywhere, including inside UTF-8 and escape sequences.
  const out = [];
  for (const s of steps) {
    if (Array.isArray(s)) { out.push(s); continue; }
    const b = enc(s);
    if (b.length > 2 && r() < 0.2) { const cut = 1 + int(b.length - 1); out.push(b.subarray(0, cut), b.subarray(cut)); }
    else out.push(b);
  }
  return cast(header.cols, header.rows, out);
}
