import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';
import tailwindcss from '@tailwindcss/vite';
import { fileURLToPath, URL } from 'node:url';

// 정적 번들: build-app.sh 가 dist/ 를 Resources/arona-ui 로 복사해 kasaterm 이
// 로컬 파일/웹뷰로 띄운다. base: './' 라 어느 경로에 놓여도 상대 참조로 로드된다.
export default defineConfig({
  plugins: [react(), tailwindcss()],
  base: './',
  resolve: {
    alias: { '@': fileURLToPath(new URL('./src', import.meta.url)) }
  },
  build: { outDir: 'dist', emptyOutDir: true },
  // dev 서버는 API 경로를 모르고 없는 경로를 전부 index.html 로 돌려준다 — fetch 가
  // 200 HTML 을 받아 **화면이 통째로 빈다**. 실서버로 넘겨 same-origin 을 만들어 준다
  // (절대주소로 부르던 옛 방식은 그 포트에 CORS 가 없어 똑같이 막혔다).
  // 헤드리스(`kasa-serve-web --port N`)에 붙으려면 `VITE_MCP_TARGET` 으로 바꿔라.
  server: {
    proxy: {
      '^/(mode|sessions|recent-sessions|characters|board|layout|claude-usage|background-agents|peek|transcript|transcript-raw|conversation|pane-tasks|schale-state|git-status|git-panel|blocks|messages|subagents|session-resume|session-switch|session-new|session-close|schedule|slash-commands|spawn-student|focus|send)(/|$|\\?)':
        { target: process.env.VITE_MCP_TARGET || 'http://127.0.0.1:8765', changeOrigin: true }
    }
  }
});
