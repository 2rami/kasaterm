import { useState } from 'react';
import { PixelPanel } from './PixelPanel';
import { PixelButton } from './PixelButton';
import { RoomChip } from './RoomChip';
import { setMode, fetchBoard, fetchCharacters, spawnAgent, closeArona } from '@/lib/mcp';

// 시작 선택 화면 — mode 미설정(또는 ?picker=1)일 때 풀스크린 2장 픽셀 카드:
// '터미널로 열기(solo)' vs '아로나와 함께(god)'.
//   solo → POST /mode?set=solo → /arona-close(창 닫고 터미널 복귀) → 콜백.
//   god  → POST /mode?set=god → board 가 비어 있으면 leader(아로나) 자동 등판
//          (solo→god 전환 직후 빈 교실 방지) → 콜백. 스폰 중 로딩, 실패 시 재시도.
export interface ModePickerProps {
  cwd: string | null;
  onPicked: (mode: 'solo' | 'god') => void;
}

type Phase = 'idle' | 'switching' | 'spawning' | 'error';

export function ModePicker({ cwd, onPicked }: ModePickerProps) {
  const [phase, setPhase] = useState<Phase>('idle');
  const [picking, setPicking] = useState<'solo' | 'god' | null>(null);
  const [err, setErr] = useState<string | null>(null);

  const pickSolo = async () => {
    setPicking('solo');
    setPhase('switching');
    await setMode('solo');
    await closeArona(); // 창 닫고 터미널 복귀(네이티브 미구현 동안 404 허용)
    onPicked('solo');
  };

  const pickGod = async () => {
    setPicking('god');
    setErr(null);
    setPhase('switching');
    const ok = await setMode('god');
    if (!ok) { setErr('모드 전환에 실패했어요.'); setPhase('error'); return; }

    // 빈 교실 방지: 아무 학생도 없으면 leader(아로나)를 먼저 등판시킨다.
    const board = await fetchBoard();
    if (board.length === 0) {
      setPhase('spawning');
      const chars = await fetchCharacters();
      const leader = chars?.leader?.name;
      if (leader) {
        const res = await spawnAgent({ character: leader });
        if (!res.ok) {
          setErr(res.notes ?? '아로나를 부르지 못했어요.');
          setPhase('error');
          return;
        }
      }
    }
    onPicked('god');
  };

  const busy = phase === 'switching' || phase === 'spawning';

  return (
    <div
      style={{
        position: 'fixed', inset: 0,
        background: 'var(--cth-cream-100)',
        display: 'flex', flexDirection: 'column', alignItems: 'center', justifyContent: 'center',
        gap: 'var(--cth-space-6)'
      }}
    >
      <style>{`@keyframes arona-spin { to { transform: rotate(360deg); } }`}</style>
      <div style={{ display: 'flex', flexDirection: 'column', alignItems: 'center', gap: 'var(--cth-space-3)' }}>
        <h1
          style={{
            fontFamily: 'var(--cth-font-display)',
            fontSize: 'var(--cth-text-display-lg)',
            color: 'var(--cth-ink-900)', margin: 0
          }}
        >
          어떻게 시작할까요?
        </h1>
        <RoomChip cwd={cwd} />
      </div>

      {phase === 'error' ? (
        <PixelPanel variant="dialog" style={{ width: 320 }}>
          <div style={{ display: 'flex', flexDirection: 'column', gap: 'var(--cth-space-4)' }}>
            <p style={{ color: 'var(--cth-coral)', margin: 0 }}>{err}</p>
            <div style={{ display: 'flex', gap: 8, justifyContent: 'flex-end' }}>
              <PixelButton onClick={() => { setPhase('idle'); setPicking(null); }}>돌아가기</PixelButton>
              <PixelButton variant="primary" onClick={pickGod}>다시 시도</PixelButton>
            </div>
          </div>
        </PixelPanel>
      ) : busy ? (
        <div style={{ display: 'flex', flexDirection: 'column', alignItems: 'center', gap: 'var(--cth-space-4)' }}>
          <div
            style={{
              width: 24, height: 24,
              background: 'var(--cth-sky)',
              boxShadow: 'inset 0 0 0 2px var(--cth-ink-900)',
              animation: 'arona-spin 0.8s steps(8) infinite'
            }}
          />
          <span style={{ fontFamily: 'var(--cth-font-ui)', color: 'var(--cth-ink-700)' }}>
            {phase === 'spawning' ? '아로나를 부르는 중…' : picking === 'god' ? '교실을 여는 중…' : '터미널을 여는 중…'}
          </span>
        </div>
      ) : (
        <div style={{ display: 'flex', gap: 'var(--cth-space-6)' }}>
          <button
            onClick={pickSolo}
            disabled={busy}
            style={{ border: 'none', background: 'transparent', cursor: 'pointer', padding: 0 }}
          >
            <PixelPanel variant="default" style={{ width: 280, height: 340 }}>
              <div style={{ display: 'flex', flexDirection: 'column', height: '100%', gap: 'var(--cth-space-4)' }}>
                <h2 style={{ fontFamily: 'var(--cth-font-display)', fontSize: 'var(--cth-text-display-md)', margin: 0, color: 'var(--cth-ink-900)' }}>
                  터미널로 열기
                </h2>
                <p style={{ color: 'var(--cth-ink-500)', margin: 0, lineHeight: 'var(--cth-lh-body-md)' }}>
                  혼자 작업하는 깔끔한 터미널. 파일 겹침은 알아서 막아드려요.
                </p>
                <div style={{ marginTop: 'auto', fontSize: 'var(--cth-text-body-sm)', color: 'var(--cth-ink-300)' }}>solo</div>
              </div>
            </PixelPanel>
          </button>

          <button
            onClick={pickGod}
            disabled={busy}
            style={{ border: 'none', background: 'transparent', cursor: 'pointer', padding: 0 }}
          >
            <PixelPanel variant="active" accent="sky" style={{ width: 280, height: 340 }}>
              <div style={{ display: 'flex', flexDirection: 'column', height: '100%', gap: 'var(--cth-space-4)' }}>
                <h2 style={{ fontFamily: 'var(--cth-font-display)', fontSize: 'var(--cth-text-display-md)', margin: 0, color: 'var(--cth-ink-900)' }}>
                  아로나와 함께
                </h2>
                <p style={{ color: 'var(--cth-ink-700)', margin: 0, lineHeight: 'var(--cth-lh-body-md)' }}>
                  아로나가 학생들과 작업을 챙겨요. 샬레 교실에서 함께 일해요.
                </p>
                <div style={{ marginTop: 'auto', fontSize: 'var(--cth-text-body-sm)', color: 'var(--cth-ink-500)' }}>god</div>
              </div>
            </PixelPanel>
          </button>
        </div>
      )}
    </div>
  );
}
