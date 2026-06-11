import React from 'react';

// 대화 버블용 경량 마크다운 렌더러 — 라이브러리 없이 claude 답변에 흔한 것만:
// GFM 표(비교용, 거노 요청)·코드블록·인라인 코드·볼드·헤딩·불릿. 과하지 않게.

// 인라인: **볼드**, `코드`. (링크/이탤릭 등은 평문 처리 — 대화엔 충분.)
function inline(text: string, keyBase: string): React.ReactNode[] {
  const out: React.ReactNode[] = [];
  const re = /(\*\*([^*]+)\*\*|`([^`]+)`)/g;
  let last = 0;
  let m: RegExpExecArray | null;
  let i = 0;
  while ((m = re.exec(text))) {
    if (m.index > last) out.push(text.slice(last, m.index));
    if (m[2] != null) out.push(<strong key={`${keyBase}-b${i}`}>{m[2]}</strong>);
    else if (m[3] != null) out.push(
      <code key={`${keyBase}-c${i}`} style={{ fontFamily: 'var(--cth-font-mono, monospace)', fontSize: '0.92em', background: 'var(--cth-cream-100)', padding: '1px 4px', borderRadius: 4 }}>{m[3]}</code>
    );
    last = m.index + m[0].length;
    i++;
  }
  if (last < text.length) out.push(text.slice(last));
  return out;
}

const isTableSep = (l: string) => /^\s*\|?[\s:|-]*-[\s:|-]*\|?\s*$/.test(l) && l.includes('-');
const cells = (l: string) => l.replace(/^\s*\|/, '').replace(/\|\s*$/, '').split('|').map((c) => c.trim());

export function Markdown({ text }: { text: string }) {
  const lines = text.split('\n');
  const blocks: React.ReactNode[] = [];
  let i = 0;
  let key = 0;

  while (i < lines.length) {
    const line = lines[i];

    // 코드 블록 ``` … ```
    if (/^\s*```/.test(line)) {
      const buf: string[] = [];
      i++;
      while (i < lines.length && !/^\s*```/.test(lines[i])) { buf.push(lines[i]); i++; }
      i++; // 닫는 ```
      blocks.push(
        <pre key={key++} style={{ margin: '6px 0', padding: '8px 10px', background: 'var(--cth-ink-900)', color: '#cfe3ff', borderRadius: 8, overflowX: 'auto', fontFamily: 'var(--cth-font-mono, monospace)', fontSize: 12, lineHeight: 1.5 }}>
          {buf.join('\n')}
        </pre>
      );
      continue;
    }

    // GFM 표 — 헤더 줄 + 구분선
    if (line.includes('|') && i + 1 < lines.length && isTableSep(lines[i + 1])) {
      const header = cells(line);
      i += 2;
      const rows: string[][] = [];
      while (i < lines.length && lines[i].includes('|') && lines[i].trim()) { rows.push(cells(lines[i])); i++; }
      blocks.push(
        <div key={key++} style={{ overflowX: 'auto', margin: '6px 0' }}>
          <table style={{ borderCollapse: 'collapse', fontSize: 12, width: '100%' }}>
            <thead>
              <tr>{header.map((h, hi) => (
                <th key={hi} style={{ border: '1px solid var(--cth-cream-200)', padding: '4px 8px', background: 'var(--cth-cream-100)', textAlign: 'left', fontWeight: 700, whiteSpace: 'nowrap' }}>{inline(h, `th${key}-${hi}`)}</th>
              ))}</tr>
            </thead>
            <tbody>{rows.map((r, ri) => (
              <tr key={ri}>{header.map((_, ci) => (
                <td key={ci} style={{ border: '1px solid var(--cth-cream-200)', padding: '4px 8px', verticalAlign: 'top' }}>{inline(r[ci] ?? '', `td${key}-${ri}-${ci}`)}</td>
              ))}</tr>
            ))}</tbody>
          </table>
        </div>
      );
      continue;
    }

    // 헤딩 #
    const h = line.match(/^(#{1,4})\s+(.*)$/);
    if (h) {
      const sz = [16, 15, 14, 13][h[1].length - 1];
      blocks.push(<div key={key++} style={{ fontWeight: 800, fontSize: sz, margin: '8px 0 4px' }}>{inline(h[2], `h${key}`)}</div>);
      i++;
      continue;
    }

    // 불릿 리스트
    if (/^\s*[-*]\s+/.test(line)) {
      const items: string[] = [];
      while (i < lines.length && /^\s*[-*]\s+/.test(lines[i])) { items.push(lines[i].replace(/^\s*[-*]\s+/, '')); i++; }
      blocks.push(
        <ul key={key++} style={{ margin: '4px 0', paddingLeft: 18 }}>
          {items.map((it, ii) => <li key={ii} style={{ margin: '2px 0' }}>{inline(it, `li${key}-${ii}`)}</li>)}
        </ul>
      );
      continue;
    }

    // 빈 줄
    if (!line.trim()) { i++; continue; }

    // 문단 — 연속 텍스트 줄 묶기(표/코드/헤딩/불릿 아닌 것)
    const para: string[] = [];
    while (
      i < lines.length && lines[i].trim() &&
      !/^\s*```/.test(lines[i]) && !/^(#{1,4})\s+/.test(lines[i]) && !/^\s*[-*]\s+/.test(lines[i]) &&
      !(lines[i].includes('|') && i + 1 < lines.length && isTableSep(lines[i + 1]))
    ) { para.push(lines[i]); i++; }
    blocks.push(<div key={key++} style={{ margin: '2px 0', whiteSpace: 'pre-wrap', wordBreak: 'break-word' }}>{inline(para.join('\n'), `p${key}`)}</div>);
  }

  return <>{blocks}</>;
}
