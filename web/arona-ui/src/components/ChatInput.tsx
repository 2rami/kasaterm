import { useRef, useState } from 'react';
import { sendToPane } from '@/lib/mcp';

/** 라이브 학생에게 그 자리에서 말 거는 입력줄.
 *
 * 한때 없앴던 것이다 — 데스크톱에서는 이 패널 바로 옆에 터미널 pane 이 있어서
 * 「어차피 뷰어」였다. 폰에서는 그 전제가 깨진다: 터미널은 아예 다른 주소라,
 * 아로나만 열어 둔 사람은 답할 방법이 없다(2026-08-25 「웹뷰 입력하는곳이 없어」).
 *
 * ⚠️ 엔터 판정에 `isComposing` 가드가 반드시 있어야 한다. 한글은 조합 중에도
 * 엔터 keydown 이 오는데(IME 확정), 그걸 전송으로 읽으면 마지막 글자가 잘리거나
 * 두 번 들어간다 — 폰 IME 에서 특히 그렇다.
 */
export function ChatInput({ surfaceId, isPhone }: { surfaceId: string; isPhone: boolean }) {
  const [text, setText] = useState('');
  const [sending, setSending] = useState(false);
  const [err, setErr] = useState(false);
  const ta = useRef<HTMLTextAreaElement | null>(null);

  const grow = (el: HTMLTextAreaElement) => {
    el.style.height = 'auto';
    el.style.height = `${Math.min(el.scrollHeight, isPhone ? 120 : 160)}px`;
  };

  const send = async () => {
    const t = text.trim();
    if (!t || sending) return;
    setSending(true);
    setErr(false);
    const ok = await sendToPane(surfaceId, t, true);
    setSending(false);
    if (!ok) {
      setErr(true);
      return;
    }
    setText('');
    if (ta.current) {
      ta.current.style.height = 'auto';
      // 폰에서는 포커스를 놓는다 — 안 놓으면 키보드가 화면 절반을 문 채로 남아
      // 방금 보낸 말이 어디로 갔는지가 안 보인다.
      if (isPhone) ta.current.blur();
      else ta.current.focus();
    }
  };

  return (
    <div style={{
      borderTop: '1px solid var(--cth-cream-200)', background: 'var(--cth-cream-50)',
      padding: '8px 10px', display: 'flex', alignItems: 'flex-end', gap: 8,
      paddingBottom: isPhone ? 'calc(8px + env(safe-area-inset-bottom, 0px))' : 8,
    }}>
      <textarea
        ref={ta}
        value={text}
        rows={1}
        // 「이 학생에게」라고 부르지 않는다 — 테마마다 캐릭터의 정체가 달라서(보컬로이드·
        // 치이카와…) 어느 테마에서나 맞는 말이 아니다(2026-08-25 지시).
        placeholder={err ? '보내지 못했어요 — 다시 눌러 보세요' : '여기에 입력'}
        onChange={(e) => { setText(e.target.value); grow(e.target); }}
        onKeyDown={(e) => {
          if (e.key !== 'Enter' || e.shiftKey) return;
          // 조합 중 엔터는 IME 확정이다 — 여기서 보내면 글자가 깨진다.
          if (e.nativeEvent.isComposing || e.keyCode === 229) return;
          e.preventDefault();
          void send();
        }}
        style={{
          flex: 1, resize: 'none', minHeight: isPhone ? 44 : 32, maxHeight: isPhone ? 120 : 160,
          boxSizing: 'border-box', padding: '9px 11px', borderRadius: 10,
          border: `1px solid ${err ? 'var(--cth-coral)' : 'var(--cth-cream-200)'}`,
          background: 'var(--cth-cream-50)', color: 'var(--cth-ink-900)',
          fontFamily: 'var(--cth-font-ui)',
          // 16px 미만이면 iOS 가 포커스 때 화면을 확대하고 스스로 안 돌아온다.
          fontSize: isPhone ? 16 : 13, lineHeight: 1.4, outline: 'none',
        }}
      />
      <button
        onClick={() => void send()}
        disabled={!text.trim() || sending}
        title="보내기 (Enter)"
        style={{
          flexShrink: 0, width: isPhone ? 44 : 34, height: isPhone ? 44 : 34,
          borderRadius: 10, border: 'none',
          cursor: text.trim() && !sending ? 'pointer' : 'not-allowed',
          background: text.trim() && !sending ? 'var(--cth-sky)' : 'var(--cth-cream-200)',
          color: text.trim() && !sending ? 'var(--cth-on-sky)' : 'var(--cth-ink-300)',
          display: 'flex', alignItems: 'center', justifyContent: 'center',
        }}
      >
        <svg width="17" height="17" viewBox="0 0 16 16" fill="none">
          <path d="M8 13V3M8 3L4 7M8 3l4 4" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" />
        </svg>
      </button>
    </div>
  );
}
