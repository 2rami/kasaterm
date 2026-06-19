import React from 'react';

// ANSI SGR(\x1b[…m) → 색 span. /context·bash·read 등 컬러 터미널 출력 공용 렌더
// (거노: ls/git/test 컬러 출력이 대화 카드에서도 색으로). pre 안에 넣어 whitespace 보존.
const ANSI16 = ['#3b4252', '#bf616a', '#a3be8c', '#ebcb8b', '#5e81ac', '#b48ead', '#88c0d0', '#e5e9f0', '#4c566a', '#d08770', '#a3be8c', '#ebcb8b', '#81a1c1', '#b48ead', '#8fbcbb', '#eceff4'];

export function ansi256(n: number): string {
  if (n < 16) return ANSI16[n] ?? '#ccc';
  if (n < 232) { const i = n - 16, r = Math.floor(i / 36), g = Math.floor((i % 36) / 6), b = i % 6; const v = (c: number) => (c ? 55 + c * 40 : 0); return `rgb(${v(r)},${v(g)},${v(b)})`; }
  const v = 8 + (n - 232) * 10; return `rgb(${v},${v},${v})`;
}

export function AnsiText({ text }: { text: string }) {
  const nodes: React.ReactNode[] = [];
  let color: string | undefined;
  let key = 0;
  const re = /\x1b\[([0-9;]*)m/g;
  let last = 0;
  let m: RegExpExecArray | null;
  const seg = (t: string, c?: string) => { if (t) nodes.push(<span key={key++} style={c ? { color: c } : undefined}>{t}</span>); };
  while ((m = re.exec(text))) {
    seg(text.slice(last, m.index), color);
    const codes = m[1].split(';').filter((s) => s !== '').map(Number);
    for (let i = 0; i < codes.length; i++) {
      const c = codes[i];
      if (c === 0 || c === 39) color = undefined;
      else if (c >= 30 && c <= 37) color = ANSI16[c - 30];
      else if (c >= 90 && c <= 97) color = ANSI16[c - 90 + 8];
      else if (c === 38) {
        if (codes[i + 1] === 2) { color = `rgb(${codes[i + 2]},${codes[i + 3]},${codes[i + 4]})`; i += 4; }
        else if (codes[i + 1] === 5) { color = ansi256(codes[i + 2]); i += 2; }
      }
    }
    last = re.lastIndex;
  }
  seg(text.slice(last), color);
  return <>{nodes}</>;
}
