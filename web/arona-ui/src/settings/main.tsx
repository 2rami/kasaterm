// 두 번째 Vite 엔트리. arona 교실(main.tsx)과 청크 그래프를 가르는 게 목적이라
// 여기서 ClassroomView 계열을 import 하면 안 된다 — 그 순간 pixi.js 가 설정
// 화면에도 딸려와 이 엔트리를 나눈 이유가 없어진다.
import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';
import './settings.css';
import '../onboarding/onboarding.css';
import { LangProvider } from './lang';
import { SettingsRoot } from '../onboarding/SettingsRoot';

createRoot(document.getElementById('root')!).render(
  <StrictMode>
    <LangProvider>
      <SettingsRoot />
    </LangProvider>
  </StrictMode>
);
