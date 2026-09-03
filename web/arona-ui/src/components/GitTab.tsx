import { useEffect, useState } from 'react';
import { fetchGitStatus, gitCommit, gitPush, type GitStatus } from '@/lib/mcp';

// 소스 컨트롤 탭 — 활성 pane cwd 의 git 상태(브랜치·변경파일)를 보고, 파일 골라 커밋·푸시.
// 터미널 git 컬럼과 같은 백엔드(/git-status·/git-commit·/git-push)를 BA GUI 에 노출(거노).
export function GitTab() {
  const [st, setSt] = useState<GitStatus>({});
  const [picked, setPicked] = useState<Set<string>>(new Set());
  const [msg, setMsg] = useState('');
  const [busy, setBusy] = useState(false);
  const [toast, setToast] = useState<string | null>(null);

  useEffect(() => {
    let stop = false;
    const tick = () => { void fetchGitStatus().then((s) => { if (!stop) setSt(s); }); };
    tick();
    const iv = setInterval(tick, 2500);
    return () => { stop = true; clearInterval(iv); };
  }, []);

  const toggle = (f: string) => setPicked((p) => {
    const n = new Set(p);
    n.has(f) ? n.delete(f) : n.add(f);
    return n;
  });

  const files: { path: string; tag: string; color: string }[] = [
    ...(st.untracked ?? []).map((p) => ({ path: p, tag: 'U', color: 'var(--cth-mint)' })),
    ...(st.modified ?? []).map((p) => ({ path: p, tag: 'M', color: 'var(--cth-sky)' })),
    ...(st.staged ?? []).filter((p) => !(st.modified ?? []).includes(p)).map((p) => ({ path: p, tag: 'S', color: 'var(--cth-lilac)' })),
  ];
  // 중복 경로 제거(staged+modified 동시).
  const seen = new Set<string>();
  const rows = files.filter((f) => (seen.has(f.path) ? false : (seen.add(f.path), true)));

  const doCommit = async () => {
    const sel = [...picked].filter((f) => rows.some((r) => r.path === f));
    if (busy || !sel.length || !msg.trim()) return;
    setBusy(true);
    const r = await gitCommit(sel, msg.trim());
    setBusy(false);
    setToast(r.ok ? '커밋 완료' : `실패: ${r.output}`.slice(0, 80));
    setTimeout(() => setToast(null), 2500);
    if (r.ok) { setMsg(''); setPicked(new Set()); void fetchGitStatus().then(setSt); }
  };

  const doPush = async () => {
    if (busy) return;
    setBusy(true);
    const r = await gitPush();
    setBusy(false);
    setToast(r.ok ? '푸시 완료' : `푸시 실패: ${r.output}`.slice(0, 80));
    setTimeout(() => setToast(null), 2500);
  };

  if (st.no_repo) {
    return <Empty text={`git 저장소가 아니에요\n${st.path ?? ''}`} />;
  }
  if (st.error) {
    return <Empty text={st.error} />;
  }

  return (
    <div style={{ flex: 1, display: 'flex', flexDirection: 'column', minHeight: 0 }}>
      {/* 브랜치 + ahead/behind + diff stat */}
      <div style={{ display: 'flex', alignItems: 'center', gap: 8, padding: '9px 12px', borderBottom: '1px solid var(--cth-cream-200)' }}>
        <span style={{ fontFamily: 'var(--cth-font-ui)', fontSize: 12, fontWeight: 800, color: 'var(--cth-ink-900)' }}>⎇ {st.branch || '—'}</span>
        {!!st.ahead && <Chip text={`↑${st.ahead}`} color="var(--cth-mint)" />}
        {!!st.behind && <Chip text={`↓${st.behind}`} color="var(--cth-coral)" />}
        <div style={{ flex: 1 }} />
        {(!!st.insertions || !!st.deletions) && (
          <span style={{ fontFamily: 'var(--cth-font-mono)', fontSize: 10 }}>
            <span style={{ color: 'var(--cth-mint)' }}>+{st.insertions ?? 0}</span>{' '}
            <span style={{ color: 'var(--cth-coral-text)' }}>−{st.deletions ?? 0}</span>
          </span>
        )}
      </div>

      {/* 변경 파일 */}
      <div style={{ flex: 1, overflowY: 'auto', padding: '4px 0' }}>
        {rows.length === 0 ? (
          <Empty text="변경된 파일 없음 (clean)" />
        ) : rows.map((f) => (
          <label key={f.path} style={{ display: 'flex', alignItems: 'center', gap: 8, padding: '5px 12px', cursor: 'pointer' }}>
            <input type="checkbox" checked={picked.has(f.path)} onChange={() => toggle(f.path)} style={{ accentColor: 'var(--cth-sky)', flexShrink: 0 }} />
            <span style={{ flexShrink: 0, width: 16, height: 16, borderRadius: 4, background: f.color, color: '#fff', fontFamily: 'var(--cth-font-ui)', fontSize: 9, fontWeight: 800, display: 'inline-flex', alignItems: 'center', justifyContent: 'center' }}>{f.tag}</span>
            <span style={{ flex: 1, fontFamily: 'var(--cth-font-mono)', fontSize: 11, color: 'var(--cth-ink-700)', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap', direction: 'rtl', textAlign: 'left' }}>{f.path}</span>
          </label>
        ))}
      </div>

      {/* 커밋 + 푸시 */}
      <div style={{ padding: 10, borderTop: '1px solid var(--cth-cream-200)', background: 'var(--cth-cream-50)', display: 'flex', flexDirection: 'column', gap: 7 }}>
        {toast && <div style={{ fontFamily: 'var(--cth-font-ui)', fontSize: 11, color: toast.startsWith('커밋 완료') || toast.startsWith('푸시 완료') ? 'var(--cth-mint)' : 'var(--cth-coral)' }}>{toast}</div>}
        <div style={{ display: 'flex', gap: 6, alignItems: 'center' }}>
          <button onClick={() => setPicked(new Set(rows.map((r) => r.path)))} style={miniBtn}>전체</button>
          <button onClick={() => setPicked(new Set())} style={miniBtn}>해제</button>
          <span style={{ fontFamily: 'var(--cth-font-ui)', fontSize: 10, color: 'var(--cth-ink-500)' }}>{picked.size}개 선택</span>
        </div>
        <input value={msg} onChange={(e) => setMsg(e.target.value)} placeholder="커밋 메시지" style={{
          width: '100%', padding: '6px 9px', borderRadius: 8, border: '1px solid var(--cth-cream-200)', outline: 'none',
          fontFamily: 'var(--cth-font-ui)', fontSize: 12, background: '#fff', color: 'var(--cth-ink-900)', boxSizing: 'border-box',
        }} />
        <div style={{ display: 'flex', gap: 6 }}>
          <button onClick={() => void doCommit()} disabled={busy || !picked.size || !msg.trim()} style={{
            flex: 1, padding: '7px 0', borderRadius: 8, border: 'none', color: '#fff', fontFamily: 'var(--cth-font-ui)', fontSize: 12, fontWeight: 700,
            cursor: busy || !picked.size || !msg.trim() ? 'not-allowed' : 'pointer',
            background: 'linear-gradient(180deg, #6BB0F0, #4A90E2)', opacity: busy || !picked.size || !msg.trim() ? 0.5 : 1,
          }}>커밋</button>
          <button onClick={() => void doPush()} disabled={busy} style={{
            padding: '7px 14px', borderRadius: 8, border: 'none', color: 'var(--cth-ink-700)', fontFamily: 'var(--cth-font-ui)', fontSize: 12, fontWeight: 700,
            cursor: busy ? 'not-allowed' : 'pointer', background: 'var(--cth-cream-200)',
          }}>푸시 {st.ahead ? `↑${st.ahead}` : ''}</button>
        </div>
      </div>
    </div>
  );
}

const miniBtn: React.CSSProperties = {
  padding: '3px 9px', borderRadius: 6, border: 'none', cursor: 'pointer',
  background: 'var(--cth-cream-100)', color: 'var(--cth-ink-500)', fontFamily: 'var(--cth-font-ui)', fontSize: 10, fontWeight: 700,
};

function Chip({ text, color }: { text: string; color: string }) {
  return <span style={{ fontFamily: 'var(--cth-font-ui)', fontSize: 10, fontWeight: 700, color: '#fff', background: color, padding: '1px 6px', borderRadius: 5 }}>{text}</span>;
}

function Empty({ text }: { text: string }) {
  return (
    <div style={{ flex: 1, display: 'flex', alignItems: 'center', justifyContent: 'center', padding: 20, textAlign: 'center', whiteSpace: 'pre-line', color: 'var(--cth-ink-300)', fontFamily: 'var(--cth-font-ui)', fontSize: 12, lineHeight: 1.6 }}>
      {text}
    </div>
  );
}
