import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';
import tailwindcss from '@tailwindcss/vite';
import { fileURLToPath, URL } from 'node:url';
import { resolve } from 'node:path';

// 정적 번들: build-app.sh 가 dist/ 를 Resources/arona-ui 로 복사해 kasaterm 이
// 로컬 파일/웹뷰로 띄운다. base: './' 라 어느 경로에 놓여도 상대 참조로 로드된다.
export default defineConfig({
  plugins: [react(), tailwindcss()],
  base: './',
  resolve: {
    alias: { '@': fileURLToPath(new URL('./src', import.meta.url)) }
  },
  // 엔트리 둘. 청크 그래프를 가르는 게 목적이다 — `settings.html` 이 교실
  // 컴포넌트를 import 하지 않으면 pixi.js 가 설정 화면에 딸려오지 않는다.
  // 둘 다 dist 루트에 나오고, `arona_ui_serve` 와일드카드가 그대로 서빙한다
  // (`GET /arona-ui/settings.html` 은 Rust 를 한 줄도 안 고쳐도 동작한다).
  build: {
    outDir: 'dist',
    emptyOutDir: true,
    rollupOptions: {
      input: {
        index: resolve(fileURLToPath(new URL('.', import.meta.url)), 'index.html'),
        settings: resolve(fileURLToPath(new URL('.', import.meta.url)), 'settings.html')
      }
    }
  },
  // dev 서버는 API 경로를 모르고 없는 경로를 전부 index.html 로 돌려준다 — fetch 가
  // 200 HTML 을 받아 **화면이 통째로 빈다**. 실서버로 넘겨 same-origin 을 만들어 준다
  // (절대주소로 부르던 옛 방식은 그 포트에 CORS 가 없어 똑같이 막혔다).
  // 헤드리스(`kasa-serve-web --port N`)에 붙으려면 `VITE_MCP_TARGET` 으로 바꿔라.
  // ⚠️경로를 일일이 적는 목록이다 — **새 엔드포인트를 만들면 여기에도 넣어라.**
  // 빠뜨리면 dev 에서만, 그것도 404 가 아니라 200 HTML 로 돌아와서 "JSON 인 줄 알고
  // 파싱하다 죽는" 모양이 된다. 실서버는 멀쩡하니 원인을 프런트에서 찾게 된다.
  server: {
    proxy: {
      '^/(mode|sessions|recent-sessions|characters|board|layout|claude-usage|background-agents|peek|transcript|transcript-raw|conversation|pane-tasks|schale-state|git-status|git-panel|blocks|messages|subagents|session-resume|session-switch|session-new|session-close|schedule|slash-commands|spawn-student|focus|send|design-tokens|settings|character-face)(/|$|\\?)':
        { target: process.env.VITE_MCP_TARGET || 'http://127.0.0.1:8765', changeOrigin: true }
    }
  }
});
