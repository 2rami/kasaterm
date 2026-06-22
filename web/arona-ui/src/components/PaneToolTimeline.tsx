import { useEffect, useMemo, useState } from 'react';
import { ToolUseCard } from './tool-use-card';
import { buildToolMap } from '@/lib/build-tool-map';
import { fetchTranscriptRaw } from '@/lib/mcp';
import type { SessionEvent } from '@/lib/types';

// 업무탭에서 학생 카드를 펼치면 그 pane 의 도구·서브에이전트(Task)를 채팅방과 동일한
// ToolUseCard 로 펼쳐 보여준다(거노: read/bash 누르면 채팅방처럼). board 는 도구 "이름"만
// 줘서 상세(input/output)는 per-pane transcript 가 필요 — 펼친 학생만 폴링해 비용 최소.
const MAX_CARDS = 14;

export function PaneToolTimeline({ paneId }: { paneId: string }) {
  const [events, setEvents] = useState<SessionEvent[]>([]);
  const [loaded, setLoaded] = useState(false);

  useEffect(() => {
    let stopped = false;
    setLoaded(false);
    setEvents([]);
    const tick = async () => {
      const evts = await fetchTranscriptRaw(paneId);
      if (stopped) return;
      setEvents(evts);
      setLoaded(true);
    };
    void tick();
    const iv = setInterval(tick, 2000);
    return () => { stopped = true; clearInterval(iv); };
  }, [paneId]);

  const tools = useMemo(() => {
    const map = buildToolMap(events);
    // 삽입 순(events 순) = 오래된→최근. AskUserQuestion 은 채팅방에서 선택지 카드로
    // 따로 뜨므로 도구 타임라인엔 제외. 최근 MAX_CARDS 개만(긴 세션 메모리·렌더 컷).
    const all = Array.from(map.values()).filter(
      (p) => p.toolUse && p.toolUse.name !== 'AskUserQuestion',
    );
    return all.slice(-MAX_CARDS);
  }, [events]);

  if (!tools.length) {
    return (
      <div style={{ marginLeft: 16, marginTop: 4, fontFamily: 'var(--cth-font-ui)', fontSize: 10, color: 'var(--cth-ink-300)' }}>
        {loaded ? '도구 기록이 없어요' : '불러오는 중…'}
      </div>
    );
  }

  return (
    <div style={{ marginLeft: 16, marginTop: 4, display: 'flex', flexDirection: 'column', gap: 4 }}>
      {tools.map((p) => (
        <ToolUseCard key={p.toolUse!.id ?? p.toolUse!.name} toolUse={p.toolUse!} pair={p} />
      ))}
    </div>
  );
}
