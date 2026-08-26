import { useCallback, useEffect, useState } from 'react';
import {
  Asterisk,
  Images,
  MessageSquare,
  SlidersHorizontal,
  Sparkles,
  Terminal,
  Users,
} from 'lucide-react';
import { useT } from './lang';
import { useTokens } from './useTokens';
import { CharactersTab, ThemeTab } from './ThemeTab';
import { GeneralTab } from './GeneralTab';
import { AppearanceTab } from './AppearanceTab';
import { ShellTab } from './ShellTab';
import { ClaudeTab } from './ClaudeTab';
import { FeedbackTab } from './FeedbackTab';
import { CharacterDetail } from './CharacterDetail';
import { fetchThemeRoster } from './api';
import { fetchValues, postAction } from './api';
import type { Character, SettingsCharacters, SettingsValues } from './types';

/// 이 페이지가 붙은 인스턴스의 포트. 웹뷰가 same-origin 으로 로드되므로
/// `location.port` 가 곧 그 인스턴스다 — 네이티브의 `mcp_panel_port()` 는 8765
/// 폴백을 가지고 있어 **남의 인스턴스를 가리킬 수 있다.** 설정은 파일을 쓰므로
/// 어느 프로세스에 말하는지가 화면에 보여야 한다.
const PORT = location.port || '8765';

/// 좌측 nav — 네이티브와 같은 순서. `ready` 가 false 인 칸은 아직 네이티브에만
/// 있다(이행 중이라는 걸 화면이 말해 준다).
///
/// 이름은 여기 없다. 칸 이름과 제목·힌트는 전부 사전에서 오고, 이 표는 **어떤
/// 칸이 어떤 순서로 있고 아이콘이 무엇인가**만 쥔다 — 언어를 늘릴 때 고칠 자리가
/// 사전 하나로 남는다.
const CATS = [
  { key: 'general', Icon: SlidersHorizontal, ready: true },
  { key: 'appearance', Icon: Sparkles, ready: true },
  { key: 'shell', Icon: Terminal, ready: true },
  { key: 'claude', Icon: Asterisk, ready: true },
  { key: 'theme', Icon: Images, ready: true },
  { key: 'students', Icon: Users, ready: true },
  { key: 'feedback', Icon: MessageSquare, ready: true },
] as const;

type CatKey = (typeof CATS)[number]['key'];

/// 로스터(`/settings/characters`)를 읽어야 그려지는 칸들. 테마 격자와 캐릭터
/// 목록이 같은 파일을 보므로, 한쪽만 적으면 다른 칸이 영원히 「읽는 중」에 선다.
const ROSTER_CATS: CatKey[] = ['theme', 'students'];

/// 딥링크로 받은 시작 칸. 하단바의 「계정 관리」처럼 **특정 칸을 보려고** 여는
/// 입구가 있는데, 웹 창은 그 뜻을 URL 로만 받는다 — 네이티브는 `SettingsCat` 을
/// 인자로 받지만 웹뷰를 여는 경로가 그 인자를 버려서, 눌러도 늘 기본 칸(캐릭터)이
/// 열렸다(2026-08-15). 모르는 값이면 예전대로.
function initialCat(): CatKey {
  const q = new URLSearchParams(location.search).get('cat');
  return CATS.some((c) => c.key === q) ? (q as CatKey) : 'theme';
}

declare global {
  interface Window {
    /// 이미 떠 있는 설정 창의 칸을 바꾼다 — 네이티브가 `evaluate_script` 로 부른다.
    /// URL 을 다시 읽히면 입력 중이던 값이 날아가므로 그 길은 안 쓴다.
    __ktSetCat?: (key: string) => void;
  }
}

/// 값만 있으면 그려지는 탭들. Theme 은 여기 없다 — 캐릭터 상세로 화면이 통째로
/// 바뀌는 자기 상태가 있어서 본문에서 따로 다룬다.
const TABS: Partial<
  Record<CatKey, (v: SettingsValues, reload: () => Promise<void>) => React.ReactNode>
> = {
  general: (v, reload) => <GeneralTab data={v.general} reload={reload} />,
  appearance: (v, reload) => <AppearanceTab data={v.appearance} reload={reload} />,
  shell: (v, reload) => <ShellTab data={v.shell} reload={reload} />,
  claude: (v, reload) => <ClaudeTab data={v.claude} reload={reload} />,
  feedback: (v, reload) => <FeedbackTab data={v.feedback} reload={reload} />,
};

export function SettingsApp() {
  const t = useT();
  const tokens = useTokens();
  const [cat, setCat] = useState<CatKey>(initialCat);
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

  // 이미 떠 있는 창을 딥링크가 다시 부를 때의 입구. 창이 하나뿐이라 두 번째
  // 「계정 관리」는 창을 새로 여는 대신 이쪽으로 온다 — 모르는 값은 조용히 무시한다
  // (네이티브가 새 칸을 추가했는데 웹이 아직 모를 때, 화면이 깨지는 것보다 낫다).
  useEffect(() => {
    window.__ktSetCat = (key: string) => {
      if (CATS.some((c) => c.key === key)) setCat(key as CatKey);
    };
    return () => {
      delete window.__ktSetCat;
    };
  }, []);

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

  const meta = t.titles[cat];
  const tab = TABS[cat];

  /// `?student=<slug>&theme=<id>` 로 열린 **학생 전용 창**인가. 설정 본체가 앱 안으로
  /// 들어가면 세부 화면은 밖으로 나와야 한다 — 안에서 안을 덮을 수 없다(거노
  /// 2026-08-25 「설정창을 안으로, 세부설정을 별도창으로」).
  const studentSlug = new URLSearchParams(location.search).get('student');
  const studentTheme = new URLSearchParams(location.search).get('theme') ?? '';

  useEffect(() => {
    if (!studentSlug || open) return;
    let dead = false;
    void (async () => {
      // 쓰는 테마면 이미 받아 둔 명단에 있다 — 왕복이 없다.
      if (chars && (studentTheme === '' || studentTheme === chars.active_theme)) {
        const hit = chars.roster.find((c) => c.slug === studentSlug);
        if (hit && !dead) setOpen(hit);
        return;
      }
      if (!studentTheme) return;
      try {
        const list = await fetchThemeRoster(studentTheme);
        const hit = list.find((c) => c.slug === studentSlug);
        if (hit && !dead) setOpen(hit);
      } catch {
        // 아래 로딩·오류 문구가 그대로 받는다.
      }
    })();
    return () => {
      dead = true;
    };
  }, [studentSlug, studentTheme, chars, open]);

  if (studentSlug) {
    // 좌측 nav 도 제목 줄도 없다. 이 창의 존재 이유가 그 학생 하나라, 다른 칸으로
    // 갈 길을 주면 같은 설정이 두 창에 떠서 어느 쪽이 정본인지 알 수 없게 된다.
    return (
      <div className="min-h-screen px-7 py-5">
        {open && chars ? (
          <CharacterDetail
            character={open}
            models={chars.models}
            /// 독립 창에서 「뒤로」는 갈 데가 없다 — 창을 닫는다.
            onBack={() => window.close()}
            onSaved={(name) => {
              setOpen((prev) => (prev ? { ...prev, name } : prev));
              void reload();
            }}
          />
        ) : (
          <p className="text-[13px] text-[var(--kt-text-mute)]">
            {err ? t.common.fetchFailed({ path: '/settings/characters', error: err }) : t.common.loading}
          </p>
        )}
      </div>
    );
  }

  return (
    <div className="flex min-h-screen">
      <nav
        className="w-[188px] shrink-0 px-3 py-5"
        style={{ background: 'var(--kt-surface)' }}
      >
        <div className="mb-4 px-2 text-[13px] font-semibold text-[var(--kt-text-dim)]">
          {t.nav.settings}
        </div>
        {CATS.map(({ key, Icon, ready }) => {
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
              <span className="whitespace-nowrap">{t.nav[key]}</span>
              {/* 아직 네이티브에만 있는 칸 — 눌러도 빈 화면이 나오는 이유를 여기서 알린다.
                  nowrap 이 없으면 「Appearance」 칸에서 이 라벨만 두 줄로 쪼개진다. */}
              {!ready && (
                <span className="ml-auto whitespace-nowrap text-[11px] text-[var(--kt-text-mute)]">
                  {t.nav.native}
                </span>
              )}
            </button>
          );
        })}
        <div className="mt-5 px-2 text-[11px] leading-relaxed text-[var(--kt-text-mute)]">
          127.0.0.1:{PORT}
          <br />
          {t.nav.portHint}
        </div>
      </nav>

      <main className="min-w-0 flex-1 px-8 py-6">
        <h1 className="text-[24px] font-semibold text-[var(--kt-text)]">{meta.title}</h1>
        <p className="mt-1 text-[13px] text-[var(--kt-text-mute)]">{meta.hint}</p>
        <div className="my-5 h-px" style={{ background: 'var(--kt-border)' }} />

        {ROSTER_CATS.includes(cat) && err && (
          <p className="text-[13px]" style={{ color: 'var(--kt-danger)' }}>
            {t.common.fetchFailed({ path: '/settings/characters', error: err })}
          </p>
        )}
        {tab && valErr && (
          <p className="text-[13px]" style={{ color: 'var(--kt-danger)' }}>
            {t.common.fetchFailed({ path: '/settings/values', error: valErr })}
          </p>
        )}

        {ROSTER_CATS.includes(cat) ? (
          chars ? (
            // 상세는 더 이상 이 화면 안에서 안 열린다 — 카드 우클릭 → 「설정 열기」가
            // 별도 창(`?student=`)을 띄운다. 설정 본체가 앱 안으로 들어가면 세부는
            // 밖에 있어야 하고, 두 곳에서 같은 로스터를 고치면 정본이 갈린다.
            cat === 'theme' ? (
              <ThemeTab data={chars} onChanged={reload} />
            ) : (
              <CharactersTab data={chars} onChanged={reload} />
            )
          ) : (
            !err && (
              <p className="text-[13px] text-[var(--kt-text-mute)]">{t.common.loading}</p>
            )
          )
        ) : tab ? (
          values ? (
            tab(values, reloadValues)
          ) : (
            !valErr && (
              <p className="text-[13px] text-[var(--kt-text-mute)]">{t.common.loading}</p>
            )
          )
        ) : (
          <p className="text-[13px] text-[var(--kt-text-mute)]">{t.common.notWebYet}</p>
        )}

        {/* 토큰이 실제로 실렸는지 화면에서 보이게 — 색이 어긋나면 여기부터 본다. */}
        {tokens && (
          <p className="mt-6 text-[11px] text-[var(--kt-text-mute)]">
            {t.common.palette({
              theme: tokens.theme,
              accent: tokens.accent_name,
              radius: tokens.shape.radius_md,
            })}
          </p>
        )}
      </main>
    </div>
  );
}
