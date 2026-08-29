import { StrictMode, useEffect } from 'react';
import { createRoot } from 'react-dom/client';
import './design/tailwind.css';
import './design/global.css';
import { startBoardPolling } from '@/lib/mcp';
import { MachinesTab } from './components/MachinesTab';

// 앱 우측 패널(SideTab::Machines)의 자식 웹뷰가 여는 단독 판 — Command Center 의
// '이사' 탭과 같은 컴포넌트를 탭 껍데기 없이 세운다. MachinesTab 은 로컬 학생을
// store.agents 에서 읽으므로, 그 폴링을 여기서 돌려 준다(본판에선 App 이 돌린다).
function Page() {
  useEffect(() => startBoardPolling(), []);
  return (
    <div style={{ position: 'fixed', inset: 0, display: 'flex', flexDirection: 'column', background: 'var(--cth-cream-100)' }}>
      <MachinesTab />
    </div>
  );
}

createRoot(document.getElementById('root')!).render(
  <StrictMode>
    <Page />
  </StrictMode>
);
