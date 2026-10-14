// Cell width per code point: -1 dropped, 0 combines with the previous cell, 1 or 2 columns.
const BMP_WIDTH = (() => {
  const t = new Int8Array(0x10000).fill(1);
  for (let k = 0; k < WIDTH_RUNS.length; k += 3) {
    const a = WIDTH_RUNS[k], b = Math.min(WIDTH_RUNS[k + 1], 0xffff), w = WIDTH_RUNS[k + 2];
    for (let cp = a; cp <= b; cp++) t[cp] = w;
  }
  return t;
})();

const ASTRAL_RUNS = (() => {
  const out = [];
  for (let k = 0; k < WIDTH_RUNS.length; k += 3) {
    if (WIDTH_RUNS[k + 1] > 0xffff) out.push([Math.max(WIDTH_RUNS[k], 0x10000), WIDTH_RUNS[k + 1], WIDTH_RUNS[k + 2]]);
  }
  return out;
})();

function charWidth(cp) {
  if (cp < 0x10000) return BMP_WIDTH[cp];
  let lo = 0, hi = ASTRAL_RUNS.length - 1;
  while (lo <= hi) {
    const mid = (lo + hi) >> 1, run = ASTRAL_RUNS[mid];
    if (cp < run[0]) hi = mid - 1;
    else if (cp > run[1]) lo = mid + 1;
    else return run[2];
  }
  return 1;
}
