import { useRef, useState } from 'react';
import { sendToGod } from '@/lib/mcp';

// 교실 하단 고정 입력바 — 사용자→아로나(god pane) 직접 지시.
// 백엔드 /chat-send 는 유우카 발주 중이라 404 도 fail-soft(입력만 비움).
export function ClassroomChatInput() {
  const [text, setText] = useState('');
  const [sending, setSending] = useState(false);
  const [flash, setFlash] = useState<'ok' | 'err' | null>(null);
  const [hover, setHover] = useState(false);
  const [pressed, setPressed] = useState(false);
  const inputRef = useRef<HTMLInputElement>(null);

  const send = async () => {
    const t = text.trim();
    if (!t || sending) return;
    setSending(true);
    const ok = await sendToGod(t);
    setSending(false);
    setText('');
    setFlash(ok ? 'ok' : 'err');
    setTimeout(() => setFlash(null), 1400);
    inputRef.current?.focus();
  };

  const onKey = (e: React.KeyboardEvent<HTMLInputElement>) => {
    if (e.key === 'Enter' && !e.shiftKey) { e.preventDefault(); void send(); }
  };

  const btnBg = sending
    ? 'var(--cth-cream-300)'
    : hover
    ? 'var(--cth-ink-700)'
    : 'var(--cth-ink-900)';

  return (
    <div
      style={{
        position: 'fixed',
        bottom: 0,
        left: 0,
        right: 0,
        height: 52,
        background: 'var(--cth-cream-100)',
        boxShadow: 'inset 0 2px 0 var(--cth-ink-900)',
        display: 'flex',
        alignItems: 'center',
        gap: 8,
        padding: '0 16px'
      }}
    >
      {/* 상태 힌트 */}
      <span
        style={{
          fontFamily: 'var(--cth-font-display)',
          fontSize: 'var(--cth-text-display-sm)',
          color: flash === 'ok'
            ? 'var(--cth-mint)'
            : flash === 'err'
            ? 'var(--cth-coral)'
            : 'var(--cth-ink-500)',
          whiteSpace: 'nowrap',
          width: 80,
          flexShrink: 0
        }}
      >
        {flash === 'ok' ? '전송됨' : flash === 'err' ? '전송실패' : '아로나에게'}
      </span>

      {/* 입력창 */}
      <input
        ref={inputRef}
        value={text}
        onChange={(e) => setText(e.target.value)}
        onKeyDown={onKey}
        disabled={sending}
        placeholder="지시 내용을 입력하세요…"
        style={{
          flex: 1,
          height: 32,
          padding: '0 10px',
          fontFamily: 'var(--cth-font-ui)',
          fontSize: 'var(--cth-text-body-md)',
          color: 'var(--cth-ink-900)',
          background: 'var(--cth-cream-50)',
          border: 'none',
          boxShadow: 'inset 0 0 0 2px var(--cth-ink-900)',
          outline: 'none',
          opacity: sending ? 0.5 : 1
        }}
      />

      {/* 전송 버튼 — 목업 "+ 새 의회 작성" CTA 톤 */}
      <button
        onClick={() => void send()}
        onMouseEnter={() => setHover(true)}
        onMouseLeave={() => { setHover(false); setPressed(false); }}
        onMouseDown={() => setPressed(true)}
        onMouseUp={() => setPressed(false)}
        disabled={sending || !text.trim()}
        style={{
          height: 32,
          padding: '0 16px',
          background: btnBg,
          color: 'var(--cth-cream-50)',
          border: 'none',
          boxShadow: pressed
            ? 'inset 0 0 0 2px var(--cth-ink-900)'
            : 'inset 0 0 0 2px var(--cth-ink-900), 0 2px 0 var(--cth-ink-900)',
          transform: pressed ? 'translateY(2px)' : 'none',
          fontFamily: 'var(--cth-font-ui)',
          fontSize: 'var(--cth-text-body-md)',
          cursor: sending || !text.trim() ? 'not-allowed' : 'pointer',
          whiteSpace: 'nowrap',
          userSelect: 'none'
        }}
      >
        {sending ? '전송 중…' : '+ 지시 보내기'}
      </button>
    </div>
  );
}
