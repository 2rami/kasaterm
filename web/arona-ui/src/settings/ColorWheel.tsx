/// 화면에 박힌 색 휠. OS 색 패널을 칸마다 여는 대신, 팔레트 위에 휠 하나를 두고
/// 고른 칸을 그 자리에서 드래그로 맞춘다(2026-08-25 지시 「원으로 드래그하면서
/// 되게 해봐, 하나씩 클릭 말고」).
///
/// 각도가 색상(H), 중심에서의 거리가 채도(S), 오른쪽 띠가 명도(V)다. 명도는 휠
/// 위에 검정을 덮어 나타내므로, 휠에 보이는 색이 곧 지금 고른 색이다.
import { useEffect, useRef, useState } from 'react';

export type Hsv = { h: number; s: number; v: number };

const WHEEL = 168;

function clamp01(x: number): number {
  return x < 0 ? 0 : x > 1 ? 1 : x;
}

export function hsvToRgb({ h, s, v }: Hsv): [number, number, number] {
  const c = v * s;
  const hp = (((h % 360) + 360) % 360) / 60;
  const x = c * (1 - Math.abs((hp % 2) - 1));
  const [r, g, b] =
    hp < 1 ? [c, x, 0]
    : hp < 2 ? [x, c, 0]
    : hp < 3 ? [0, c, x]
    : hp < 4 ? [0, x, c]
    : hp < 5 ? [x, 0, c]
    : [c, 0, x];
  const m = v - c;
  return [
    Math.round((r + m) * 255),
    Math.round((g + m) * 255),
    Math.round((b + m) * 255),
  ];
}

export function hsvToHex(hsv: Hsv): string {
  const [r, g, b] = hsvToRgb(hsv);
  return `#${[r, g, b].map((n) => n.toString(16).padStart(2, '0')).join('')}`;
}

export function hexToHsv(hex: string): Hsv {
  const m = /^#?([0-9a-f]{6})$/i.exec(hex.trim());
  if (!m) return { h: 0, s: 0, v: 0 };
  const n = parseInt(m[1], 16);
  const r = ((n >> 16) & 255) / 255;
  const g = ((n >> 8) & 255) / 255;
  const b = (n & 255) / 255;
  const max = Math.max(r, g, b);
  const min = Math.min(r, g, b);
  const d = max - min;
  let h = 0;
  if (d > 0) {
    if (max === r) h = 60 * (((g - b) / d) % 6);
    else if (max === g) h = 60 * ((b - r) / d + 2);
    else h = 60 * ((r - g) / d + 4);
  }
  return { h: (h + 360) % 360, s: max === 0 ? 0 : d / max, v: max };
}

/// 값 하나를 **한 번에 하나씩만** 보내는 창구. 보내는 중에 새 값이 오면 마지막
/// 것만 남겨 뒀다가 응답이 온 뒤 이어 보낸다.
///
/// 휠은 손을 움직이는 내내 값을 뿜으므로 그대로 흘려보내면 요청이 밀린다. 시간
/// 간격으로 솎으면(스로틀) 얼마로 잡아도 느린 기계에선 밀리고 빠른 기계에선 덜
/// 부드러운데, 이 방식은 응답 속도가 곧 간격이 되어 저절로 맞는다.
export function useLatestOnly(send: (v: string) => Promise<unknown>) {
  const pending = useRef<string | null>(null);
  const busy = useRef(false);
  const frame = useRef<number | null>(null);
  const alive = useRef(true);
  const sendRef = useRef(send);
  sendRef.current = send;
  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
      if (frame.current != null) window.cancelAnimationFrame(frame.current);
    };
  }, []);
  const schedule = () => {
    if (!alive.current || busy.current || frame.current != null) return;
    frame.current = window.requestAnimationFrame(() => {
      frame.current = null;
      const next = pending.current;
      pending.current = null;
      if (next === null) return;
      busy.current = true;
      void sendRef.current(next).finally(() => {
        busy.current = false;
        if (pending.current !== null) schedule();
      });
    });
  };
  return (v: string) => {
    pending.current = v;
    schedule();
  };
}

/// 휠 원판 — 색상×채도만 그린다(명도는 위에 덮는 검정이 맡는다). 한 번 그려 두면
/// 드래그 내내 다시 그릴 일이 없다.
function useWheelCanvas(canvas: React.RefObject<HTMLCanvasElement | null>) {
  useEffect(() => {
    const cv = canvas.current;
    if (!cv) return;
    const dpr = window.devicePixelRatio || 1;
    const px = Math.round(WHEEL * dpr);
    cv.width = px;
    cv.height = px;
    const ctx = cv.getContext('2d');
    if (!ctx) return;
    const img = ctx.createImageData(px, px);
    const d = img.data;
    const c = px / 2;
    for (let y = 0; y < px; y++) {
      for (let x = 0; x < px; x++) {
        const dx = x - c + 0.5;
        const dy = y - c + 0.5;
        const r = Math.hypot(dx, dy);
        const i = (y * px + x) * 4;
        if (r > c) continue;
        const h = (Math.atan2(dy, dx) * 180) / Math.PI;
        const [R, G, B] = hsvToRgb({ h: h + 360, s: Math.min(1, r / c), v: 1 });
        d[i] = R;
        d[i + 1] = G;
        d[i + 2] = B;
        // 가장자리 한 겹은 부분 투명으로 — 안 하면 원 둘레가 톱니로 보인다.
        d[i + 3] = r > c - 1 ? Math.round(255 * (c - r)) : 255;
      }
    }
    ctx.putImageData(img, 0, 0);
  }, [canvas]);
}

export function ColorWheel({
  hex,
  disabled,
  onPreview,
  onCommit,
}: {
  hex: string;
  disabled?: boolean;
  /// 드래그하는 내내 불린다 — 파일에 굳히지 않고 화면 색만 바꾸는 쪽.
  onPreview: (next: string) => Promise<unknown>;
  /// 손을 뗄 때 한 번 불린다 — 저장하는 쪽.
  onCommit: (next: string) => void;
}) {
  const canvas = useRef<HTMLCanvasElement>(null);
  useWheelCanvas(canvas);
  const [hsv, setHsv] = useState<Hsv>(() => hexToHsv(hex));
  // 방금 고른 값. state 는 이 이벤트 안에서 아직 새 값이 아니라, 손을 뗄 때 굳힐
  // 값을 여기서 읽는다.
  const last = useRef(hsv);
  const preview = useLatestOnly(onPreview);

  // 밖에서 온 색이 **지금 손잡이가 가리키는 색과 다를 때만** 다시 잡는다. 무조건
  // 역산하면 채도나 명도가 0 인 자리를 지날 때마다 색상이 빨강으로 튀고, 미리보기
  // 중에는 서버가 옛 색을 돌려주므로 손잡이가 매번 제자리로 끌려간다.
  useEffect(() => {
    setHsv((cur) => {
      if (hsvToHex(cur).toLowerCase() === hex.toLowerCase()) return cur;
      const next = hexToHsv(hex);
      last.current = next;
      return next;
    });
  }, [hex]);

  const apply = (next: Hsv, live: boolean) => {
    last.current = next;
    setHsv(next);
    const hx = hsvToHex(next);
    if (live) preview(hx);
    else onCommit(hx);
  };

  // 원 밖으로 나간 커서는 채도 1 로 잡아 둔다 — 손이 살짝 벗어났다고 픽이 끊기면
  // 가장 진한 색을 잡을 수가 없다.
  const pickWheel = (e: React.PointerEvent<HTMLDivElement>) => {
    const r = e.currentTarget.getBoundingClientRect();
    const c = r.width / 2;
    const dx = e.clientX - r.left - c;
    const dy = e.clientY - r.top - c;
    const h = ((Math.atan2(dy, dx) * 180) / Math.PI + 360) % 360;
    apply({ ...last.current, h, s: clamp01(Math.hypot(dx, dy) / c) }, true);
  };

  const pickValue = (e: React.PointerEvent<HTMLDivElement>) => {
    const r = e.currentTarget.getBoundingClientRect();
    apply({ ...last.current, v: 1 - clamp01((e.clientY - r.top) / r.height) }, true);
  };

  /// press → 드래그 → release 를 한 벌로 묶는다. 포인터를 잡아 두어야(capture)
  /// 커서가 칸 밖으로 나가도 이어지고, release 도 여기로 돌아온다.
  const dragProps = (pick: (e: React.PointerEvent<HTMLDivElement>) => void) => ({
    onPointerDown: (e: React.PointerEvent<HTMLDivElement>) => {
      if (disabled) return;
      // 잡기는 실패할 수 있다(그 사이 사라진 포인터 등). 잡히든 말든 픽은 해야
      // 하므로 여기서 예외가 손잡이를 멈추게 두지 않는다.
      try {
        e.currentTarget.setPointerCapture(e.pointerId);
      } catch {
        /* 잡지 못해도 드래그 자체는 이어진다 */
      }
      pick(e);
    },
    onPointerMove: (e: React.PointerEvent<HTMLDivElement>) => {
      if (disabled || e.buttons === 0) return;
      pick(e);
    },
    onPointerUp: (e: React.PointerEvent<HTMLDivElement>) => {
      if (disabled) return;
      try {
        e.currentTarget.releasePointerCapture(e.pointerId);
      } catch {
        /* 안 잡혀 있었으면 놓을 것도 없다 */
      }
      onCommit(hsvToHex(last.current));
    },
  });

  const rad = (hsv.h * Math.PI) / 180;
  const hx = WHEEL / 2 + Math.cos(rad) * hsv.s * (WHEEL / 2);
  const hy = WHEEL / 2 + Math.sin(rad) * hsv.s * (WHEEL / 2);
  const cur = hsvToHex(hsv);
  // 손잡이 테두리는 밝은 자리선 검정, 어두운 자리선 흰색 — 어느 색 위에서도 보인다.
  const ring = hsv.v > 0.55 ? '#000' : '#fff';

  return (
    <div className="flex items-start gap-3" style={{ opacity: disabled ? 0.4 : 1 }}>
      <div
        className="relative shrink-0 touch-none select-none"
        style={{ width: WHEEL, height: WHEEL, cursor: disabled ? 'default' : 'crosshair' }}
        {...dragProps(pickWheel)}
      >
        <canvas
          ref={canvas}
          className="absolute inset-0 h-full w-full rounded-full"
          style={{ pointerEvents: 'none' }}
        />
        <div
          className="absolute inset-0 rounded-full bg-black"
          style={{ opacity: 1 - hsv.v, pointerEvents: 'none' }}
        />
        <div
          className="absolute rounded-full"
          style={{
            left: hx - 7,
            top: hy - 7,
            width: 14,
            height: 14,
            background: cur,
            border: `2px solid ${ring}`,
            boxShadow: '0 0 0 1px rgba(0,0,0,.35)',
            pointerEvents: 'none',
          }}
        />
      </div>

      <div
        className="relative shrink-0 touch-none select-none rounded-[3px]"
        style={{
          width: 16,
          height: WHEEL,
          cursor: disabled ? 'default' : 'ns-resize',
          background: `linear-gradient(to bottom, ${hsvToHex({ ...hsv, v: 1 })}, #000)`,
          border: 'var(--kt-border-w) solid var(--kt-border)',
        }}
        {...dragProps(pickValue)}
      >
        <div
          className="absolute left-[-3px] right-[-3px] rounded-[2px]"
          style={{
            top: (1 - hsv.v) * WHEEL - 2,
            height: 4,
            background: 'var(--kt-text)',
            pointerEvents: 'none',
          }}
        />
      </div>
    </div>
  );
}
