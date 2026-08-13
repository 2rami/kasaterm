import { useEffect, useState } from 'react';
import {
  Asterisk,
  MessageSquare,
  SlidersHorizontal,
  Sparkles,
  Terminal,
  Users,
} from 'lucide-react';
import { useTokens } from './useTokens';
import { ThemeTab } from './ThemeTab';
import type { SettingsCharacters } from './types';

/// 이 페이지가 붙은 인스턴스의 포트. 웹뷰가 same-origin 으로 로드되므로
/// `location.port` 가 곧 그 인스턴스다 — 네이티브의 `mcp_panel_port()` 는 8765
/// 폴백을 가지고 있어 **남의 인스턴스를 가리킬 수 있다.** 설정은 파일을 쓰므로
/// 어느 프로세스에 말하는지가 화면에 보여야 한다.
const PORT = location.port || '8765';

/// 좌측 nav — 네이티브와 같은 순서·같은 이름. `ready` 가 false 인 칸은 아직
/// 네이티브에만 있다(이행 중이라는 걸 화면이 말해 준다).
const CATS = [
  { key: 'general', label: 'General', Icon: SlidersHorizontal, ready: false },
  { key: 'appearance', label: 'Appearance', Icon: Sparkles, ready: false },
  { key: 'shell', label: 'Shell', Icon: Terminal, ready: false },
  { key: 'claude', label: 'Claude', Icon: Asterisk, ready: false },
  { key: 'theme', label: 'Theme', Icon: Users, ready: true },
  { key: 'feedback', label: 'Feedback', Icon: MessageSquare, ready: false },
] as const;

type CatKey = (typeof CATS)[number]['key'];

const TITLES: Record<CatKey, { title: string; hint: string }> = {
  general: { title: 'General', hint: '창·작업 폴더·파일 열기' },
  appearance: { title: 'Appearance', hint: '색·모양·글꼴' },
  shell: { title: 'Shell', hint: '셸과 편집기' },
  claude: { title: 'Claude', hint: '계정과 실행 방식' },
  theme: { title: 'Theme', hint: '학생 그림과 페르소나, 캐릭터 목록' },
  feedback: { title: 'Feedback', hint: '쓰다가 걸린 것을 남겨 주세요' },
};

export function SettingsApp() {
  const tokens = useTokens();
  const [cat, setCat] = useState<CatKey>('theme');
  const [chars, setChars] = useState<SettingsCharacters | null>(null);
  const [err, setErr] = useState<string | null>(null);

  useEffect(() => {
    let alive = true;
    (async () => {
      try {
        const res = await fetch('/settings/characters');
        if (!res.ok) throw new Error(`HTTP ${res.status}`);
        const data = (await res.json()) as SettingsCharacters;
        if (alive) setChars(data);
      } catch (e) {
        if (alive) setErr(e instanceof Error ? e.message : String(e));
      }
    })();
    return () => {
      alive = false;
    };
  }, []);

  const meta = TITLES[cat];

  return (
    <div className="flex min-h-screen">
      <nav
        className="w-[188px] shrink-0 px-3 py-5"
        style={{ background: 'var(--kt-surface)' }}
      >
        <div className="mb-4 px-2 text-[13px] font-semibold text-[var(--kt-text-dim)]">
          Settings
        </div>
        {CATS.map(({ key, label, Icon, ready }) => {
          const on = key === cat;
          return (
            <button
              key={key}
              type="button"
              onClick={() => setCat(key)}
              className="relative mb-0.5 flex w-full items-center gap-2.5 px-2.5 py-2 text-left text-[13px]"
              style={{
                borderRadius: 'var(--kt-radius-sm)',
                background: on ? 'var(--kt-surface-active)' : 'transparent',
                color: on ? 'var(--kt-text)' : 'var(--kt-text-dim)',
              }}
            >
              {on && (
                <span
                  className="absolute left-0 top-1/2 h-[18px] w-[3px] -translate-y-1/2"
                  style={{ background: 'var(--kt-accent)' }}
                />
              )}
              <Icon size={15} strokeWidth={2} />
              <span className="whitespace-nowrap">{label}</span>
              {/* 아직 네이티브에만 있는 칸 — 눌러도 빈 화면이 나오는 이유를 여기서 알린다.
                  nowrap 이 없으면 「Appearance」 칸에서 이 라벨만 두 줄로 쪼개진다. */}
              {!ready && (
                <span className="ml-auto whitespace-nowrap text-[11px] text-[var(--kt-text-mute)]">
                  네이티브
                </span>
              )}
            </button>
          );
        })}
        <div className="mt-5 px-2 text-[11px] leading-relaxed text-[var(--kt-text-mute)]">
          127.0.0.1:{PORT}
          <br />
          이 포트의 kasaterm
        </div>
      </nav>

      <main className="min-w-0 flex-1 px-8 py-6">
        <h1 className="text-[24px] font-semibold text-[var(--kt-text)]">{meta.title}</h1>
        <p className="mt-1 text-[13px] text-[var(--kt-text-mute)]">{meta.hint}</p>
        <div className="my-5 h-px" style={{ background: 'var(--kt-border)' }} />

        {err && (
          <p className="text-[13px]" style={{ color: 'var(--kt-danger)' }}>
            /settings/characters 실패: {err}
          </p>
        )}

        {cat === 'theme' ? (
          chars ? (
            <ThemeTab data={chars} />
          ) : (
            !err && <p className="text-[13px] text-[var(--kt-text-mute)]">읽는 중…</p>
          )
        ) : (
          <p className="text-[13px] text-[var(--kt-text-mute)]">
            이 화면은 아직 네이티브 설정 창에 있어요.
          </p>
        )}

        {/* 토큰이 실제로 실렸는지 화면에서 보이게 — 색이 어긋나면 여기부터 본다. */}
        {tokens && (
          <p className="mt-6 text-[11px] text-[var(--kt-text-mute)]">
            팔레트 {tokens.theme} · accent {tokens.accent_name} · 모서리{' '}
            {tokens.shape.radius_md}px
          </p>
        )}
      </main>
    </div>
  );
}
