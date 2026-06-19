import { PrismLight as SyntaxHighlighter } from 'react-syntax-highlighter';
import { oneDark, oneLight } from 'react-syntax-highlighter/dist/esm/styles/prism';
import bash from 'react-syntax-highlighter/dist/esm/languages/prism/bash';
import c from 'react-syntax-highlighter/dist/esm/languages/prism/c';
import cpp from 'react-syntax-highlighter/dist/esm/languages/prism/cpp';
import css from 'react-syntax-highlighter/dist/esm/languages/prism/css';
import diff from 'react-syntax-highlighter/dist/esm/languages/prism/diff';
import go from 'react-syntax-highlighter/dist/esm/languages/prism/go';
import java from 'react-syntax-highlighter/dist/esm/languages/prism/java';
import javascript from 'react-syntax-highlighter/dist/esm/languages/prism/javascript';
import json from 'react-syntax-highlighter/dist/esm/languages/prism/json';
import markdown from 'react-syntax-highlighter/dist/esm/languages/prism/markdown';
import python from 'react-syntax-highlighter/dist/esm/languages/prism/python';
import rust from 'react-syntax-highlighter/dist/esm/languages/prism/rust';
import sql from 'react-syntax-highlighter/dist/esm/languages/prism/sql';
import tsx from 'react-syntax-highlighter/dist/esm/languages/prism/tsx';
import typescript from 'react-syntax-highlighter/dist/esm/languages/prism/typescript';
import yaml from 'react-syntax-highlighter/dist/esm/languages/prism/yaml';

// ccsv CodeBlock 이식 — react-syntax-highlighter prism-light(필요 언어만 register). 거노
// 세션 대부분 코드라 단색 pre 덤프 → 구문강조로 승격. rust 추가(kasaterm 세션).
/* eslint-disable @typescript-eslint/no-explicit-any */
const LANGS: Record<string, any> = { bash, c, cpp, css, diff, go, java, javascript, json, markdown, python, rust, sql, tsx, typescript, yaml };
for (const [k, v] of Object.entries(LANGS)) SyntaxHighlighter.registerLanguage(k, v);

// fence info string 별칭 → 등록 언어. 미지원은 'text'(plain).
const ALIAS: Record<string, string> = {
  ts: 'typescript', js: 'javascript', jsx: 'javascript', mjs: 'javascript', cjs: 'javascript',
  py: 'python', sh: 'bash', shell: 'bash', zsh: 'bash', console: 'bash',
  rs: 'rust', yml: 'yaml', md: 'markdown', jsonc: 'json', 'c++': 'cpp', golang: 'go',
};

function resolveLang(lang?: string): string {
  const l = (lang || '').toLowerCase();
  if (LANGS[l]) return l;
  return ALIAS[l] || 'text';
}

export function CodeBlock({ code, lang }: { code: string; lang?: string }) {
  const dark = typeof document !== 'undefined' && document.documentElement.dataset.theme === 'dark';
  return (
    <SyntaxHighlighter
      language={resolveLang(lang)}
      style={dark ? oneDark : oneLight}
      customStyle={{ margin: '6px 0', padding: '8px 10px', borderRadius: 8, fontSize: 12, lineHeight: 1.5 }}
      codeTagProps={{ style: { fontFamily: 'var(--cth-font-mono, monospace)' } }}
    >
      {code}
    </SyntaxHighlighter>
  );
}
