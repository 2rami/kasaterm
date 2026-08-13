// 두 번째 Vite 엔트리. arona 교실(main.tsx)과 청크 그래프를 가르는 게 목적이라
// 여기서 ClassroomView 계열을 import 하면 안 된다 — 그 순간 pixi.js 가 설정
// 화면에도 딸려와 이 엔트리를 나눈 이유가 없어진다.
import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';
import '../design/tailwind.css';
import '../design/global.css';
import { SettingsApp } from './SettingsApp';

createRoot(document.getElementById('root')!).render(
  <StrictMode>
    <SettingsApp />
  </StrictMode>
);
