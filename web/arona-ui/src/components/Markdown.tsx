import React from 'react';
import { CodeBlock } from './CodeBlock';

// 대화 버블용 경량 마크다운 렌더러 — 라이브러리 없이 claude 답변에 흔한 것만:
// GFM 표(비교용, 거노 요청)·코드블록·인라인 코드·볼드·헤딩·불릿. 과하지 않게.

// 인라인: **볼드**, `코드`, ~~취소선~~, [링크](url), *이탤릭*/_이탤릭_. ccsv 의
// react-markdown+remark-gfm 인라인을 자작 정규식으로 이식(거노: 답변 속 URL 클릭·이탤릭).
// 순서 주의 — 볼드(**)/코드를 이탤릭(*) 보다 먼저 매칭해 **를 * 가 안 먹게.
function inline(text: string, keyBase: string): React.ReactNode[] {
  const out: React.ReactNode[] = [];
  const re = /(\*\*([^*]+)\*\*|`([^`]+)`|~~([^~]+)~~|\[([^\]]+)\]\(([^)\s]+)\)|\*([^*\n]+)\*|_([^_\n]+)_)/g;
  let last = 0;
  let m: RegExpExecArray | null;
  let i = 0;
  while ((m = re.exec(text))) {
    if (m.index > last) out.push(text.slice(last, m.index));
    if (m[2] != null) out.push(<strong key={`${keyBase}-b${i}`}>{m[2]}</strong>);
    else if (m[3] != null) out.push(
      <code key={`${keyBase}-c${i}`} style={{ fontFamily: 'var(--cth-font-mono, monospace)', fontSize: '0.92em', background: 'var(--cth-cream-100)', padding: '1px 4px', borderRadius: 4 }}>{m[3]}</code>
    );
    else if (m[4] != null) out.push(<del key={`${keyBase}-s${i}`}>{m[4]}</del>);
    else if (m[5] != null) out.push(
      <a key={`${keyBase}-l${i}`} href={m[6]} target="_blank" rel="noreferrer" style={{ color: 'var(--cth-sky-text-surface)', textDecoration: 'underline' }}>{m[5]}</a>
    );
    else if (m[7] != null) out.push(<em key={`${keyBase}-i${i}`}>{m[7]}</em>);
    else if (m[8] != null) out.push(<em key={`${keyBase}-u${i}`}>{m[8]}</em>);
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

    // 코드 블록 ``` … ``` — fence info(```ts)로 언어 잡아 구문강조(CodeBlock=prism).
    if (/^\s*```/.test(line)) {
      const lang = line.replace(/^\s*```/, '').trim().split(/\s+/)[0] || undefined;
      const buf: string[] = [];
      i++;
      while (i < lines.length && !/^\s*```/.test(lines[i])) { buf.push(lines[i]); i++; }
      i++; // 닫는 ```
      blocks.push(<CodeBlock key={key++} code={buf.join('\n')} lang={lang} />);
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
        <ul key={key++} style={{ margin: '4px 0', paddingLeft: 18, listStyleType: 'disc', listStylePosition: 'outside' }}>
          {items.map((it, ii) => <li key={ii} style={{ margin: '2px 0' }}>{inline(it, `li${key}-${ii}`)}</li>)}
        </ul>
      );
      continue;
    }

    // 번호 리스트 1. 2. — start 보존(ccsv: 답변 속 단계 목록).
    if (/^\s*\d+\.\s+/.test(line)) {
      const items: string[] = [];
      const start = parseInt(line.match(/^\s*(\d+)\./)?.[1] ?? '1', 10);
      while (i < lines.length && /^\s*\d+\.\s+/.test(lines[i])) { items.push(lines[i].replace(/^\s*\d+\.\s+/, '')); i++; }
      blocks.push(
        <ol key={key++} start={start} style={{ margin: '4px 0', paddingLeft: 22, listStyleType: 'decimal', listStylePosition: 'outside' }}>
          {items.map((it, ii) => <li key={ii} style={{ margin: '2px 0' }}>{inline(it, `ol${key}-${ii}`)}</li>)}
        </ol>
      );
      continue;
    }

    // 인용 > — 좌측 보더 + 흐린 글씨.
    if (/^\s*>\s?/.test(line)) {
      const items: string[] = [];
      while (i < lines.length && /^\s*>\s?/.test(lines[i])) { items.push(lines[i].replace(/^\s*>\s?/, '')); i++; }
      blocks.push(
        <blockquote key={key++} style={{ margin: '4px 0', paddingLeft: 10, borderLeft: '3px solid var(--cth-cream-200)', color: 'var(--cth-ink-500)' }}>
          {inline(items.join('\n'), `bq${key}`)}
        </blockquote>
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
      !/^\s*\d+\.\s+/.test(lines[i]) && !/^\s*>\s?/.test(lines[i]) &&
      !(lines[i].includes('|') && i + 1 < lines.length && isTableSep(lines[i + 1]))
    ) { para.push(lines[i]); i++; }
    // 문단 사이 여백 — 터미널은 빈 줄이 통째로 들어가 확 띄워지는데 웹뷰는 2px라 문단이
    // 뭉쳐 보였다(거노: 유즈 자소서). 문단답게 띄운다.
    blocks.push(<div key={key++} style={{ margin: '0.7em 0', whiteSpace: 'pre-wrap', wordBreak: 'break-word' }}>{inline(para.join('\n'), `p${key}`)}</div>);
  }

  return <>{blocks}</>;
}
