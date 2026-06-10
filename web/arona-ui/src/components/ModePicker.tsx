import { useState } from 'react';
import { PixelPanel } from './PixelPanel';
import { setMode } from '@/lib/mcp';

// 시작 선택 화면 — mode 미설정(또는 ?picker=1)일 때 풀스크린 2장 픽셀 카드:
// '터미널로 열기(solo)' vs '아로나와 함께(god)'. 선택 시 POST /mode?set= 후 콜백.
export interface ModePickerProps {
  onPicked: (mode: 'solo' | 'god') => void;
}

export function ModePicker({ onPicked }: ModePickerProps) {
  const [busy, setBusy] = useState<'solo' | 'god' | null>(null);

  const pick = async (mode: 'solo' | 'god') => {
    setBusy(mode);
    await setMode(mode);
    onPicked(mode);
  };

  return (
    <div
      style={{
        position: 'fixed', inset: 0,
        background: 'var(--cth-cream-100)',
        display: 'flex', flexDirection: 'column', alignItems: 'center', justifyContent: 'center',
        gap: 'var(--cth-space-6)'
      }}
    >
      <h1
        style={{
          fontFamily: 'var(--cth-font-display)',
          fontSize: 'var(--cth-text-display-lg)',
          color: 'var(--cth-ink-900)', margin: 0
        }}
      >
        어떻게 시작할까요?
      </h1>
      <div style={{ display: 'flex', gap: 'var(--cth-space-6)' }}>
        <button
          onClick={() => pick('solo')}
          disabled={busy !== null}
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
              <div style={{ marginTop: 'auto', fontSize: 'var(--cth-text-body-sm)', color: 'var(--cth-ink-300)' }}>
                {busy === 'solo' ? '여는 중…' : 'solo'}
              </div>
            </div>
          </PixelPanel>
        </button>

        <button
          onClick={() => pick('god')}
          disabled={busy !== null}
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
              <div style={{ marginTop: 'auto', fontSize: 'var(--cth-text-body-sm)', color: 'var(--cth-ink-500)' }}>
                {busy === 'god' ? '부르는 중…' : 'god'}
              </div>
            </div>
          </PixelPanel>
        </button>
      </div>
    </div>
  );
}
