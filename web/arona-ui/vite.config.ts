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
  build: { outDir: 'dist', emptyOutDir: true }
});
