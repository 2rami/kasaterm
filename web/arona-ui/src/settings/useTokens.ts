import { useEffect, useState } from 'react';
import type { DesignTokens } from './types';

declare global {
  interface Window {
    __ktRefreshTokens?: () => Promise<void>;
  }
}

/// 네이티브 토큰을 `--kt-*` CSS 변수로 심는다. 이 화면은 arona 의 `--cth-*`
/// (SCHALE 클린 블루, 라이트 기본)를 쓰지 않는다 — 설정은 터미널 창과 나란히
/// 뜨는 화면이라 **터미널이 지금 입고 있는 색**을 그대로 입어야 한다. 사용자가
/// tokyo-night 을 골랐으면 여기도 tokyo-night 이어야 하고, 그 값은 서버만 안다.
///
/// 변수를 `documentElement` 에 심는 이유는 하나 — 카드·버튼·글자색을 각자
/// fetch 결과에 의존시키면 컴포넌트마다 로딩 상태를 그려야 한다. 변수는 값이
/// 늦게 와도 그때 리페인트만 일어난다.
export function useTokens(): DesignTokens | null {
  const [tokens, setTokens] = useState<DesignTokens | null>(null);

  useEffect(() => {
    let alive = true;
    let frame: number | null = null;
    let inFlight = false;
    let rerun = false;

    const fetchLatest = async () => {
      if (inFlight) {
        rerun = true;
        return;
      }
      inFlight = true;
      try {
        const res = await fetch('/design-tokens');
        if (!res.ok) return;
        const next = (await res.json()) as DesignTokens;
        if (!alive) return;
        applyTokens(next);
        setTokens(next);
      } catch {
        // CSS 폴백을 유지한다.
      } finally {
        inFlight = false;
        if (rerun && alive) {
          rerun = false;
          schedule();
        }
      }
    };

    const schedule = () => {
      if (frame != null || !alive) return;
      frame = window.requestAnimationFrame(() => {
        frame = null;
        void fetchLatest();
      });
    };

    window.__ktRefreshTokens = async () => {
      schedule();
    };
    schedule();
    return () => {
      alive = false;
      if (frame != null) window.cancelAnimationFrame(frame);
      delete window.__ktRefreshTokens;
    };
  }, []);

  return tokens;
}

function applyTokens(t: DesignTokens) {
  const root = document.documentElement;
  for (const [k, v] of Object.entries(t.palette)) {
    const value = k === 'text_mute' ? t.palette.text_dim : v;
    root.style.setProperty(`--kt-${k.replace(/_/g, '-')}`, value);
  }
  t.ansi.forEach((c, i) => root.style.setProperty(`--kt-ansi-${i}`, c));
  root.style.setProperty('--kt-radius-sm', `${t.shape.radius_sm}px`);
  root.style.setProperty('--kt-radius-md', `${t.shape.radius_md}px`);
  root.style.setProperty('--kt-border-w', `${t.shape.border_w}px`);
  // 원이 사각으로 굽는 정도(픽셀 테마는 각진 점을 원한다). 반지름이 아니라
  // 별도 축이라 radius 로 대신 계산하면 6px 점과 200px 패널이 같이 굽는다.
  root.style.setProperty('--kt-dot-radius', `${50 * t.shape.roundness}%`);
  // 배경이 어두운지로 스크롤바·form 컨트롤의 OS 기본 배색을 맞춘다. 팔레트가
  // 정본이므로 prefers-color-scheme 을 보지 않는다 — 사용자가 라이트 OS 에서
  // 다크 팔레트를 골랐을 때 둘이 어긋난다.
  root.style.setProperty('color-scheme', isDark(t.palette.bg) ? 'dark' : 'light');
}

/// hex 의 상대 휘도로 어두운지 판단. 정확한 WCAG 식이 필요한 자리가 아니라
/// (스크롤바 배색 한 곳) 계수만 쓴다.
function isDark(hex: string | undefined): boolean {
  if (!hex) return true;
  const h = hex.replace('#', '');
  if (h.length < 6) return true;
  const [r, g, b] = [0, 2, 4].map((i) => parseInt(h.slice(i, i + 2), 16));
  return 0.299 * r + 0.587 * g + 0.114 * b < 128;
}
