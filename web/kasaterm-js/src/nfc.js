// Whether NFC could change a UTF-8 batch. Most terminal text — Hangul syllables included — is
// already NFC, and proving that is a byte scan, much cheaper than decode, normalize and encode.
const NFC_CHECK_BMP = (() => {
  const t = new Uint8Array(0x10000);
  for (let k = 0; k < NFC_CHECK_RUNS.length; k += 2) {
    const a = NFC_CHECK_RUNS[k], b = Math.min(NFC_CHECK_RUNS[k + 1], 0xffff);
    for (let cp = a; cp <= b; cp++) t[cp] = 1;
  }
  return t;
})();

function nfcCheckAstral(cp) {
  let lo = 0, hi = NFC_CHECK_RUNS.length / 2 - 1;
  while (lo <= hi) {
    const mid = (lo + hi) >> 1;
    if (cp < NFC_CHECK_RUNS[mid * 2]) hi = mid - 1;
    else if (cp > NFC_CHECK_RUNS[mid * 2 + 1]) lo = mid + 1;
    else return true;
  }
  return false;
}

// Malformed UTF-8 is decoded loosely here: such a batch is never normalized anyway, so a false
// "may change" only costs the slow path's validity check.
function mayChangeUnderNfc(bytes) {
  const n = bytes.length;
  for (let i = 0; i < n;) {
    const b = bytes[i];
    if (b < 0x80) { i++; continue; }
    let cp;
    if (b < 0xe0) { cp = ((b & 0x1f) << 6) | (bytes[i + 1] & 0x3f); i += 2; }
    else if (b < 0xf0) { cp = ((b & 0x0f) << 12) | ((bytes[i + 1] & 0x3f) << 6) | (bytes[i + 2] & 0x3f); i += 3; }
    else {
      cp = ((b & 0x07) << 18) | ((bytes[i + 1] & 0x3f) << 12) | ((bytes[i + 2] & 0x3f) << 6) | (bytes[i + 3] & 0x3f);
      i += 4;
      if (cp > 0xffff) { if (nfcCheckAstral(cp)) return true; continue; }
    }
    if (NFC_CHECK_BMP[cp]) return true;
  }
  return false;
}
