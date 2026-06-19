import { useState } from 'react';
import { PixelPanel } from './PixelPanel';
import { PixelButton } from './PixelButton';
import { FolderBrowser } from './FolderBrowser';
import { setMode, fetchBoard, fetchCharacters, newRoom, closeArona } from '@/lib/mcp';

// 시작 선택 화면 — mode 미설정(또는 ?picker=1)일 때 풀스크린 2장 픽셀 카드:
// '터미널로 열기(solo)' vs '아로나와 함께(god)'.
//   solo → POST /mode?set=solo → /arona-close(창 닫고 터미널 복귀) → 콜백.
//   god  → 먼저 작업 폴더 온보딩(FolderBrowser) → roomCd 로 그 방으로 이동 →
//          POST /mode?set=god → board 가 비어 있으면 leader(아로나) 자동 등판 → 콜백.
export interface ModePickerProps {
  cwd: string | null;
  /** configured=false(마커 없는 첫 실행)면 온보딩 문구 — 미설정 방을 처음 여는 맥락. */
  onboarding?: boolean;
  /** BA GUI 진입 온보딩 — solo/god 카드 없이 작업 폴더 선택만 보여주고 바로 god 진입(거노:
   *  "켤 때 경로지정"). arona 창 연 것 자체가 god 의도라 모드 선택은 생략한다. */
  pathOnly?: boolean;
  onPicked: (mode: 'solo' | 'god') => void;
}

type Phase = 'idle' | 'pickpath' | 'switching' | 'spawning' | 'error';

export function ModePicker({ cwd, onboarding, pathOnly, onPicked }: ModePickerProps) {
  const [phase, setPhase] = useState<Phase>(pathOnly ? 'pickpath' : 'idle');
  const [picking, setPicking] = useState<'solo' | 'god' | null>(pathOnly ? 'god' : null);
  const [err, setErr] = useState<string | null>(null);
  const [pickedPath, setPickedPath] = useState<string>(cwd ?? '');

  const pickSolo = async () => {
    setPicking('solo');
    setPhase('switching');
    await setMode('solo');
    await closeArona(); // 창 닫고 터미널 복귀(네이티브 미구현 동안 404 허용)
    onPicked('solo');
  };

  // god 카드 → 바로 켜지 않고 작업 폴더부터 고른다(온보딩).
  const pickGod = () => {
    setPicking('god');
    setErr(null);
    setPickedPath(cwd ?? '');
    setPhase('pickpath');
  };

  // 폴더 확정 → god 모드 진입. 돌아가는 세션에 cd 를 박지 않는다(거노: BA GUI 무접촉).
  // 대신 leader(아로나)를 **고른 폴더에서** 새로 띄운다(spawn cwd) — 폴더 선택이 실제로
  // 그 디렉토리의 god 으로 이어지고, 기존 claude 는 손대지 않는다.
  const enterGod = async () => {
    setErr(null);
    setPhase('switching');
    const ok = await setMode('god');
    if (!ok) { setErr('모드 전환에 실패했어요.'); setPhase('error'); return; }

    // 빈 교실 방지: 아무 학생도 없으면 leader(아로나)를 고른 폴더에서 등판.
    const board = await fetchBoard();
    if (board.length === 0) {
      setPhase('spawning');
      const chars = await fetchCharacters();
      const leader = chars?.leader?.name;
      if (leader) {
        // /spawn 폐기(거노) — god 방을 새로 만들면 첫 pane 이 leader 로 자동 배정된다
        // (백엔드 new_room_with_god). cwd(pickedPath) 반영은 후속(newRoom 이 아직 미지원).
        const ok2 = await newRoom(leader);
        if (!ok2) {
          setErr('아로나를 부르지 못했어요.');
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
          {phase === 'pickpath'
            ? '어느 폴더에서 작업할까요?'
            : onboarding ? '처음 오셨네요! 어떻게 시작할까요?' : '어떻게 시작할까요?'}
        </h1>
      </div>

      {phase === 'error' ? (
        <PixelPanel variant="dialog" style={{ width: 320 }}>
          <div style={{ display: 'flex', flexDirection: 'column', gap: 'var(--cth-space-4)' }}>
            <p style={{ color: 'var(--cth-coral)', margin: 0 }}>{err}</p>
            <div style={{ display: 'flex', gap: 8, justifyContent: 'flex-end' }}>
              <PixelButton onClick={() => { setPhase('idle'); setPicking(null); }}>돌아가기</PixelButton>
              <PixelButton variant="primary" onClick={enterGod}>다시 시도</PixelButton>
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
      ) : phase === 'pickpath' ? (
        <PixelPanel variant="active" accent="sky" style={{ width: 460, maxWidth: '92vw' }}>
          <div style={{ display: 'flex', flexDirection: 'column', gap: 'var(--cth-space-4)' }}>
            <p style={{ color: 'var(--cth-ink-700)', margin: 0, lineHeight: 'var(--cth-lh-body-md)' }}>
              아로나와 학생들이 이 폴더에서 작업해요. 하위 폴더로 들어가 고른 뒤 시작하세요.
            </p>
            <FolderBrowser initialPath={cwd} onPathChange={setPickedPath} height={240} />
            <div title={pickedPath} style={{
              fontFamily: 'var(--cth-font-mono)', fontSize: 11, color: 'var(--cth-ink-500)',
              overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap', direction: 'rtl', textAlign: 'left',
            }}>{pickedPath || '…'}</div>
            <div style={{ display: 'flex', gap: 8, justifyContent: 'flex-end' }}>
              {!pathOnly && <PixelButton onClick={() => { setPhase('idle'); setPicking(null); }}>뒤로</PixelButton>}
              <PixelButton variant="primary" onClick={enterGod}>이 폴더에서 시작</PixelButton>
            </div>
          </div>
        </PixelPanel>
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
