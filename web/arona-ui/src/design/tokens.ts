// Design tokens — single source of truth. Mirrors tokens.css for non-styled consumers (Pixi).
// Any change here must also update tokens.css.
// SCHALE OS 클린 블루 테마 (목업 기준 2026-06-11). 이름 유지, 값만 블루 재매핑.

export const colors = {
  cream: {
    50: 0xffffff,
    100: 0xeaf3fc,
    200: 0xd6e6f5,
    300: 0xb9d3ed
  },
  paper: {
    100: 0xf5fafe,
    200: 0xe3effa
  },
  ink: {
    900: 0x15294a,
    700: 0x25406b,
    500: 0x4a638f,
    300: 0x8aa6c8,
    100: 0xcbdcef
  },
  accent: {
    coral: 0xff6b6b,
    coralLight: 0xffb4b4,
    mint: 0x6bcf7f,
    mintLight: 0xb4e5bd,
    sky: 0x4a90e2,
    skyLight: 0xa9cbf0,
    lemon: 0xffd93d,
    lemonLight: 0xffec99,
    lilac: 0xb197fc,
    lilacLight: 0xd6c5ff,
    peach: 0xffa07a,
    peachLight: 0xffd0b5
  },
  status: {
    idle: 0x8aa6c8,
    thinking: 0x4a90e2,
    working: 0xffc83d,
    blocked: 0xff6b6b,
    success: 0x6bcf7f,
    ghost: 0xcbdcef
  },
  world: {
    grassLight: 0xdce9f5,
    grassDark: 0xc3d8ee,
    woodLight: 0xe5c896,
    woodDark: 0xc9a66b,
    path: 0xe8f0f8,
    wall: 0xa9c2de
  }
} as const;

export const space = {
  0: 0, 1: 4, 2: 8, 3: 12, 4: 16, 5: 24, 6: 32, 7: 48, 8: 64
} as const;

export const type = {
  display: '"Pretendard Variable", Pretendard, "Noto Sans KR", system-ui, sans-serif',
  ui: '"Pretendard Variable", Pretendard, "Noto Sans KR", system-ui, sans-serif',
  mono: '"JetBrains Mono", ui-monospace, "SF Mono", monospace'
} as const;

export const tileSize = 32; // px — the world is built from 32×32 tiles

export type AccentColorName =
  | 'coral' | 'mint' | 'sky' | 'lemon' | 'lilac' | 'peach';

export const accentByName: Record<AccentColorName, number> = {
  coral: colors.accent.coral,
  mint:  colors.accent.mint,
  sky:   colors.accent.sky,
  lemon: colors.accent.lemon,
  lilac: colors.accent.lilac,
  peach: colors.accent.peach
};

export const accentLightByName: Record<AccentColorName, number> = {
  coral: colors.accent.coralLight,
  mint:  colors.accent.mintLight,
  sky:   colors.accent.skyLight,
  lemon: colors.accent.lemonLight,
  lilac: colors.accent.lilacLight,
  peach: colors.accent.peachLight
};

// Convert 0xRRGGBB to "#RRGGBB"
export function hex(c: number): string {
  return '#' + c.toString(16).padStart(6, '0').toUpperCase();
}
