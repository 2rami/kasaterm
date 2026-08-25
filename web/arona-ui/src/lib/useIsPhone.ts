import { useEffect, useState } from 'react';

/**
 * 폰 폭인가. 이 UI 의 레이아웃은 전부 인라인 `style={{}}` 이라(Tailwind 는
 * tool-renderers·settings 에만 쓴다) 미디어쿼리로는 못 이긴다 — `!important`
 * 싸움이 되므로, 이미 있는 `embedded`·`focusMode` 분기와 같은 결로 JS 로 가른다.
 * CSS 로 가는 건 인라인 스타일이 없는 것들뿐(입력 글자 크기·터치 스크롤 → global.css).
 *
 * ⚠️ `pointer: coarse` 를 같이 묶지 않는다 — 데스크톱 wry webview 를 좁게 만들 때도
 * 같은 레이아웃이 나와야 폰 화면을 맥에서 확인할 수 있다.
 */
export function useIsPhone(maxWidth = 768): boolean {
  const q = `(max-width: ${maxWidth}px)`;
  const [phone, setPhone] = useState(() => window.matchMedia(q).matches);
  useEffect(() => {
    const mq = window.matchMedia(q);
    const on = () => setPhone(mq.matches);
    on();
    mq.addEventListener('change', on);
    return () => mq.removeEventListener('change', on);
  }, [q]);
  return phone;
}
