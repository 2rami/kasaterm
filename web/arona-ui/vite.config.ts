import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';
import tailwindcss from '@tailwindcss/vite';
import { fileURLToPath, URL } from 'node:url';
import { resolve } from 'node:path';

// dev 프록시가 실서버로 넘길 때 Origin 도 함께 바꾼다. 안 바꾸면 브라우저가 붙인
// `http://localhost:<vite>` 가 그대로 가고, 서버의 `ws_origin_ok` 는 Origin 이 Host 와
// **정확히** 같기를 요구해서 POST 가 전부 403 이 된다(2026-08-25: 입력줄이 dev 에서만
// 「보내지 못했어요」였다 — 실서버는 same-origin 이라 멀쩡하다).
const MCP_TARGET = process.env.VITE_MCP_TARGET || 'http://127.0.0.1:8765';

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
    // ⚠️ 여기 없는 경로는 vite 가 **SPA 껍데기(index.html)를 200 으로** 돌려준다.
    // 404 가 아니라 200 이라 `r.ok` 검사를 통과하고 `r.json()` 에서야 터지는데,
    // 호출부가 대개 catch 로 null 을 삼켜서 「조용히 아무 일도 안 일어남」이 된다
    // (2026-08-25: theme-roster 가 빠져 있어 다른 테마 학생 프사가 다 이니셜로 떴다).
    // mcp.ts 에 새 엔드포인트를 만들면 이 목록에도 넣을 것.
    proxy: {
      '^/(mode|sessions|recent-sessions|characters|board|layout|claude-usage|background-agents|background-kill|peek|transcript|transcript-raw|conversation|session-transcript-raw|subagent-transcript-raw|pane-tasks|schale-state|git-status|git-panel|git-commit|git-push|blocks|messages|subagents|session-resume|session-switch|session-new|session-close|session-save|room-cd|close-pane|schedule|schedule-delete|slash-commands|spawn-student|repersona|focus|send|terminal-reveal|paste-active|paste-image|sent-images|image-file|open-file|list-dir|design-tokens|settings|onboarding|theme-roster|character-face|character-sprite|character-sprite-status|machines|pane-migrate)(/|$|\\?)':
        { target: MCP_TARGET, changeOrigin: true, headers: { origin: MCP_TARGET } }
    }
  }
});
