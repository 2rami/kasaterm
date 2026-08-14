import { useCallback, useEffect, useState } from 'react';
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
import { GeneralTab } from './GeneralTab';
import { AppearanceTab } from './AppearanceTab';
import { ShellTab } from './ShellTab';
import { CharacterDetail } from './CharacterDetail';
import { fetchValues, postAction } from './api';
import type { Character, SettingsCharacters, SettingsValues } from './types';

/// 이 페이지가 붙은 인스턴스의 포트. 웹뷰가 same-origin 으로 로드되므로
/// `location.port` 가 곧 그 인스턴스다 — 네이티브의 `mcp_panel_port()` 는 8765
/// 폴백을 가지고 있어 **남의 인스턴스를 가리킬 수 있다.** 설정은 파일을 쓰므로
/// 어느 프로세스에 말하는지가 화면에 보여야 한다.
const PORT = location.port || '8765';

/// 좌측 nav — 네이티브와 같은 순서·같은 이름. `ready` 가 false 인 칸은 아직
/// 네이티브에만 있다(이행 중이라는 걸 화면이 말해 준다).
const CATS = [
  { key: 'general', label: 'General', Icon: SlidersHorizontal, ready: true },
  { key: 'appearance', label: 'Appearance', Icon: Sparkles, ready: true },
  { key: 'shell', label: 'Shell', Icon: Terminal, ready: true },
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

/// 값만 있으면 그려지는 탭들. Theme 은 여기 없다 — 캐릭터 상세로 화면이 통째로
/// 바뀌는 자기 상태가 있어서 본문에서 따로 다룬다.
const TABS: Partial<
  Record<CatKey, (v: SettingsValues, reload: () => Promise<void>) => React.ReactNode>
> = {
  general: (v, reload) => <GeneralTab data={v.general} reload={reload} />,
  appearance: (v, reload) => <AppearanceTab data={v.appearance} reload={reload} />,
  shell: (v, reload) => <ShellTab data={v.shell} reload={reload} />,
};

export function SettingsApp() {
  const tokens = useTokens();
  const [cat, setCat] = useState<CatKey>('theme');
  const [chars, setChars] = useState<SettingsCharacters | null>(null);
  const [values, setValues] = useState<SettingsValues | null>(null);
  const [err, setErr] = useState<string | null>(null);
  const [valErr, setValErr] = useState<string | null>(null);
  /// 열려 있는 캐릭터. **이름이 아니라 객체를 들고 있는다.**
  ///
  /// 이름으로 들고 로스터에서 찾으면 이름을 바꾼 직후 한 프레임 동안 그 이름이
  /// 로스터에 없다(리로드가 아직 안 끝났다) — 그 순간 상세가 언마운트되고 리로드
  /// 뒤 새 컴포넌트로 다시 마운트돼, 방금 띄운 「저장했어요」가 사라진다(실측).
  /// slug 로 찾는 것도 답이 아니다: slug 는 캐릭터끼리 겹칠 수 있어 엉뚱한 사람이
  /// 열린다.
  const [open, setOpen] = useState<Character | null>(null);

  /// 로스터를 다시 읽는다. 저장 뒤에도 부르는 이유는 **파일이 진실**이기
  /// 때문이다 — 화면이 요청값을 그대로 믿으면 저장 쪽에서 거부된 변경이 화면에만
  /// 남는다.
  const reload = useCallback(async () => {
    try {
      const res = await fetch('/settings/characters');
      if (!res.ok) throw new Error(`HTTP ${res.status}`);
      setChars((await res.json()) as SettingsCharacters);
      setErr(null);
    } catch (e) {
      setErr(e instanceof Error ? e.message : String(e));
    }
  }, []);

  /// 설정 값도 같은 규칙 — 액션이 끝날 때마다 다시 읽는다. 두 조회를 가른 것은
  /// 성격이 달라서다: 로스터는 79명치라 무겁고, 값은 앱 메모리 스냅샷이라 가볍다.
  const reloadValues = useCallback(async () => {
    try {
      setValues(await fetchValues());
      setValErr(null);
    } catch (e) {
      setValErr(e instanceof Error ? e.message : String(e));
    }
  }, []);

  useEffect(() => {
    void reload();
    void reloadValues();
  }, [reload, reloadValues]);

  // Esc 로 창 닫기. 네이티브 설정 화면은 키 핸들러가 직접 닫는데, 웹뷰 창은 키가
  // 웹뷰로 가 그 경로에 닿지 않는다(거노 2026-08-15 "esc왜안되냐") — 여기서 잡아
  // 액션으로 되돌린다. 입력 칸의 Esc(되돌리기)는 그 칸이 stopPropagation 하므로
  // 여기까지 오지 않는다.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') void postAction('close-settings');
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, []);

  const meta = TITLES[cat];
  const tab = TABS[cat];

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

        {cat === 'theme' && err && (
          <p className="text-[13px]" style={{ color: 'var(--kt-danger)' }}>
            /settings/characters 실패: {err}
          </p>
        )}
        {tab && valErr && (
          <p className="text-[13px]" style={{ color: 'var(--kt-danger)' }}>
            /settings/values 실패: {valErr}
          </p>
        )}

        {cat === 'theme' ? (
          chars ? (
            open ? (
              <CharacterDetail
                character={open}
                onBack={() => setOpen(null)}
                onSaved={(name) => {
                  // 이름만 갱신하고 객체는 그대로 둔다 — 상세는 자기가 유일한
                  // 편집자라 서버에서 다시 받아올 게 없고, 갈아치우면 draft 가
                  // 리셋된다.
                  setOpen((prev) => (prev ? { ...prev, name } : prev));
                  void reload();
                }}
              />
            ) : (
              <ThemeTab data={chars} onSelect={setOpen} onChanged={reload} />
            )
          ) : (
            !err && <p className="text-[13px] text-[var(--kt-text-mute)]">읽는 중…</p>
          )
        ) : tab ? (
          values ? (
            tab(values, reloadValues)
          ) : (
            !valErr && <p className="text-[13px] text-[var(--kt-text-mute)]">읽는 중…</p>
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
