import { createContext, useCallback, useContext, useEffect, useMemo, useState } from 'react';
import { postAction } from './api';
import { STRINGS } from './strings';
import type { Lang, Strings } from './strings';

/// 언어를 모를 때 쓰는 값. 이 앱을 쓰는 사람이 한국어로 일하므로 **한국어가
/// 기본**이다 — 서버 응답을 기다리는 한 프레임도 한국어로 뜬다(거노 지시
/// 2026-08-15 「나는 한글이 기본으로」).
const DEFAULT_LANG: Lang = 'ko';

type LangCtx = {
  lang: Lang;
  t: Strings;
  /// 언어를 바꾸고 파일에 굳힌다. 실패해도 화면은 바뀐 채로 둔다 — 저장이
  /// 안 됐다고 방금 고른 언어를 되돌리면, 사용자에겐 클릭이 씹힌 것으로 보인다.
  setLang: (next: Lang) => void;
};

const Ctx = createContext<LangCtx>({
  lang: DEFAULT_LANG,
  t: STRINGS[DEFAULT_LANG],
  setLang: () => {},
});

/// 서버가 아는 언어인가. 설정 파일은 사람이 손으로 고칠 수 있어서 아무 문자열이나
/// 올 수 있는데, 그걸 그대로 키로 쓰면 사전 조회가 undefined 가 되어 화면 전체가
/// 빈 글자로 뜬다.
function asLang(v: unknown): Lang | null {
  return v === 'ko' || v === 'en' ? v : null;
}

export function LangProvider({ children }: { children: React.ReactNode }) {
  const [lang, setLangState] = useState<Lang>(DEFAULT_LANG);

  // 저장된 언어를 읽어 온다. 값 조회에 얹어 보내므로 요청이 늘지 않는다.
  useEffect(() => {
    let alive = true;
    void (async () => {
      try {
        const res = await fetch('/settings/values');
        const v = (await res.json()) as { language?: unknown } | null;
        const got = asLang(v?.language);
        if (alive && got) setLangState(got);
      } catch {
        // 못 읽으면 기본값 그대로 — 설정 화면이 언어 하나 때문에 안 뜨는 게 더 나쁘다.
      }
    })();
    return () => {
      alive = false;
    };
  }, []);

  const setLang = useCallback((next: Lang) => {
    setLangState(next);
    void postAction('set-language', { id: next });
  }, []);

  const value = useMemo<LangCtx>(
    () => ({ lang, t: STRINGS[lang], setLang }),
    [lang, setLang]
  );
  return <Ctx.Provider value={value}>{children}</Ctx.Provider>;
}

/// 지금 언어의 문구 묶음. `t.theme.title` 처럼 점으로 꺼낸다 — 키를 문자열로
/// 넘기지 않으므로 오타가 컴파일에서 잡히고 자동완성이 된다.
export function useT(): Strings {
  return useContext(Ctx).t;
}

/// 언어 자체를 다루는 자리(설정의 언어 칸)만 쓴다.
export function useLang(): { lang: Lang; setLang: (next: Lang) => void } {
  const { lang, setLang } = useContext(Ctx);
  return { lang, setLang };
}

/// 서버가 준 결과를 화면 문구로. **코드가 있으면 사전에서, 없으면 서버 문구를
/// 그대로** 쓴다 — 이 폴백이 있어야 Rust 쪽 코드화가 덜 끝난 자리도 화면이 안
/// 깨지고 원래 한국어로 멀쩡히 뜬다(노아와 합의, 2026-08-15).
export function serverText(
  t: Strings,
  code: string | null | undefined,
  fallback: string | null | undefined,
  args?: Record<string, string | number>
): string {
  if (code) {
    const entry = t.server[code as keyof Strings['server']];
    if (typeof entry === 'function') return entry(args ?? {});
    if (typeof entry === 'string') return entry;
  }
  return fallback ?? '';
}
