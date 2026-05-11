/**
 * 터미널 컬러 테마 프리셋.
 *
 * 각 테마는 CSS 변수(앱 UI) + xterm.js theme(터미널 화면) 둘 다에 매핑된다.
 * 기본은 "Tokyo Night" — 사용자의 평소 셋업.
 */

export type Palette = {
  /** 가장 어두운 배경 (topbar, 사이드바 등) */
  bg0: string;
  /** 기본 배경 (메인) */
  bg1: string;
  /** 약간 밝은 배경 (카드/버튼) */
  bg2: string;
  /** 호버/강조 배경 */
  bg3: string;
  /** 보더 (강) */
  border: string;
  /** 보더 (약) */
  borderDim: string;
  /** 본문 글자 */
  fg: string;
  /** 디머진 글자 */
  fgDim: string;
  /** 16 ANSI — black ~ brightWhite */
  black: string;
  red: string;
  green: string;
  yellow: string;
  blue: string;
  magenta: string;
  cyan: string;
  white: string;
  brightBlack: string;
  brightRed: string;
  brightGreen: string;
  brightYellow: string;
  brightBlue: string;
  brightMagenta: string;
  brightCyan: string;
  brightWhite: string;
};

export type Theme = {
  id: string;
  name: string;
  palette: Palette;
};

export const THEMES: Theme[] = [
  {
    // iTerm2 defaults read 로 뽑은 사용자의 실제 다크 프로필 색상.
    id: "my-iterm",
    name: "내 iTerm",
    palette: {
      bg0: "#1c1f23",
      bg1: "#24282d",
      bg2: "#2d3138",
      bg3: "#353a43",
      border: "#595f68",
      borderDim: "#353a43",
      fg: "#d1d4d9",
      fgDim: "#969ca4",
      black: "#595f68",
      red: "#d8565e",
      green: "#68cd66",
      yellow: "#fbea8d",
      blue: "#4386f6",
      magenta: "#ad93e9",
      cyan: "#65c2cc",
      white: "#d1d4d9",
      brightBlack: "#969ca4",
      brightRed: "#e87c85",
      brightGreen: "#9ce5a3",
      brightYellow: "#fbea8d",
      brightBlue: "#87b6f9",
      brightMagenta: "#ad93e9",
      brightCyan: "#79d1da",
      brightWhite: "#fafafb",
    },
  },
  {
    id: "tokyo-night",
    name: "Tokyo Night",
    palette: {
      bg0: "#16161e",
      bg1: "#1a1b26",
      bg2: "#1f2335",
      bg3: "#2a2e42",
      border: "#414868",
      borderDim: "#2a2e42",
      fg: "#c0caf5",
      fgDim: "#565f89",
      black: "#15161e",
      red: "#f7768e",
      green: "#9ece6a",
      yellow: "#e0af68",
      blue: "#7aa2f7",
      magenta: "#bb9af7",
      cyan: "#7dcfff",
      white: "#a9b1d6",
      brightBlack: "#414868",
      brightRed: "#f7768e",
      brightGreen: "#9ece6a",
      brightYellow: "#e0af68",
      brightBlue: "#7aa2f7",
      brightMagenta: "#bb9af7",
      brightCyan: "#7dcfff",
      brightWhite: "#c0caf5",
    },
  },
  {
    id: "dracula",
    name: "Dracula",
    palette: {
      bg0: "#1e1f29",
      bg1: "#282a36",
      bg2: "#343746",
      bg3: "#44475a",
      border: "#6272a4",
      borderDim: "#44475a",
      fg: "#f8f8f2",
      fgDim: "#6272a4",
      black: "#21222c",
      red: "#ff5555",
      green: "#50fa7b",
      yellow: "#f1fa8c",
      blue: "#bd93f9",
      magenta: "#ff79c6",
      cyan: "#8be9fd",
      white: "#f8f8f2",
      brightBlack: "#6272a4",
      brightRed: "#ff6e6e",
      brightGreen: "#69ff94",
      brightYellow: "#ffffa5",
      brightBlue: "#d6acff",
      brightMagenta: "#ff92df",
      brightCyan: "#a4ffff",
      brightWhite: "#ffffff",
    },
  },
  {
    id: "catppuccin-mocha",
    name: "Catppuccin Mocha",
    palette: {
      bg0: "#11111b",
      bg1: "#1e1e2e",
      bg2: "#313244",
      bg3: "#45475a",
      border: "#585b70",
      borderDim: "#313244",
      fg: "#cdd6f4",
      fgDim: "#7f849c",
      black: "#45475a",
      red: "#f38ba8",
      green: "#a6e3a1",
      yellow: "#f9e2af",
      blue: "#89b4fa",
      magenta: "#f5c2e7",
      cyan: "#94e2d5",
      white: "#bac2de",
      brightBlack: "#585b70",
      brightRed: "#f38ba8",
      brightGreen: "#a6e3a1",
      brightYellow: "#f9e2af",
      brightBlue: "#89b4fa",
      brightMagenta: "#f5c2e7",
      brightCyan: "#94e2d5",
      brightWhite: "#a6adc8",
    },
  },
  {
    id: "gruvbox-dark",
    name: "Gruvbox Dark",
    palette: {
      bg0: "#1d2021",
      bg1: "#282828",
      bg2: "#3c3836",
      bg3: "#504945",
      border: "#665c54",
      borderDim: "#3c3836",
      fg: "#ebdbb2",
      fgDim: "#a89984",
      black: "#282828",
      red: "#cc241d",
      green: "#98971a",
      yellow: "#d79921",
      blue: "#458588",
      magenta: "#b16286",
      cyan: "#689d6a",
      white: "#a89984",
      brightBlack: "#928374",
      brightRed: "#fb4934",
      brightGreen: "#b8bb26",
      brightYellow: "#fabd2f",
      brightBlue: "#83a598",
      brightMagenta: "#d3869b",
      brightCyan: "#8ec07c",
      brightWhite: "#ebdbb2",
    },
  },
  {
    id: "nord",
    name: "Nord",
    palette: {
      bg0: "#242933",
      bg1: "#2e3440",
      bg2: "#3b4252",
      bg3: "#434c5e",
      border: "#4c566a",
      borderDim: "#3b4252",
      fg: "#eceff4",
      fgDim: "#7b88a1",
      black: "#3b4252",
      red: "#bf616a",
      green: "#a3be8c",
      yellow: "#ebcb8b",
      blue: "#81a1c1",
      magenta: "#b48ead",
      cyan: "#88c0d0",
      white: "#e5e9f0",
      brightBlack: "#4c566a",
      brightRed: "#bf616a",
      brightGreen: "#a3be8c",
      brightYellow: "#ebcb8b",
      brightBlue: "#81a1c1",
      brightMagenta: "#b48ead",
      brightCyan: "#8fbcbb",
      brightWhite: "#eceff4",
    },
  },
  {
    id: "solarized-dark",
    name: "Solarized Dark",
    palette: {
      bg0: "#00212b",
      bg1: "#002b36",
      bg2: "#073642",
      bg3: "#0a4757",
      border: "#586e75",
      borderDim: "#073642",
      fg: "#93a1a1",
      fgDim: "#586e75",
      black: "#073642",
      red: "#dc322f",
      green: "#859900",
      yellow: "#b58900",
      blue: "#268bd2",
      magenta: "#d33682",
      cyan: "#2aa198",
      white: "#eee8d5",
      brightBlack: "#586e75",
      brightRed: "#cb4b16",
      brightGreen: "#586e75",
      brightYellow: "#657b83",
      brightBlue: "#839496",
      brightMagenta: "#6c71c4",
      brightCyan: "#93a1a1",
      brightWhite: "#fdf6e3",
    },
  },
  {
    id: "github-dark",
    name: "GitHub Dark",
    palette: {
      bg0: "#010409",
      bg1: "#0d1117",
      bg2: "#161b22",
      bg3: "#21262d",
      border: "#30363d",
      borderDim: "#21262d",
      fg: "#c9d1d9",
      fgDim: "#8b949e",
      black: "#484f58",
      red: "#ff7b72",
      green: "#3fb950",
      yellow: "#d29922",
      blue: "#58a6ff",
      magenta: "#bc8cff",
      cyan: "#39c5cf",
      white: "#b1bac4",
      brightBlack: "#6e7681",
      brightRed: "#ffa198",
      brightGreen: "#56d364",
      brightYellow: "#e3b341",
      brightBlue: "#79c0ff",
      brightMagenta: "#d2a8ff",
      brightCyan: "#56d4dd",
      brightWhite: "#f0f6fc",
    },
  },
];

export const DEFAULT_THEME_ID = "my-iterm";

export function findTheme(id: string): Theme {
  return THEMES.find((t) => t.id === id) ?? THEMES[0];
}

/** 팔레트를 CSS 변수로 :root 에 박는다. */
export function applyPaletteToRoot(p: Palette) {
  const root = document.documentElement.style;
  root.setProperty("--bg-0", p.bg0);
  root.setProperty("--bg-1", p.bg1);
  root.setProperty("--bg-2", p.bg2);
  root.setProperty("--bg-3", p.bg3);
  root.setProperty("--border", p.border);
  root.setProperty("--border-dim", p.borderDim);
  root.setProperty("--fg", p.fg);
  root.setProperty("--fg-dim", p.fgDim);
  root.setProperty("--purple", p.magenta);
  root.setProperty("--blue", p.blue);
  root.setProperty("--cyan", p.cyan);
  root.setProperty("--green", p.green);
  root.setProperty("--yellow", p.yellow);
  root.setProperty("--orange", p.brightYellow);
  root.setProperty("--red", p.red);
}

/** MiniTerminal 의 Palette16 옵션으로 변환. */
export function paletteToMini(p: Palette) {
  return {
    ansi: [
      p.black,
      p.red,
      p.green,
      p.yellow,
      p.blue,
      p.magenta,
      p.cyan,
      p.white,
      p.brightBlack,
      p.brightRed,
      p.brightGreen,
      p.brightYellow,
      p.brightBlue,
      p.brightMagenta,
      p.brightCyan,
      p.brightWhite,
    ],
    defaultFg: p.fg,
    defaultBg: p.bg1,
    cursor: p.fg,
  };
}

/** xterm.js Terminal 의 theme 옵션으로 변환. */
export function paletteToXterm(p: Palette) {
  return {
    background: p.bg1,
    foreground: p.fg,
    cursor: p.fg,
    cursorAccent: p.bg1,
    selectionBackground: p.bg3,
    black: p.black,
    red: p.red,
    green: p.green,
    yellow: p.yellow,
    blue: p.blue,
    magenta: p.magenta,
    cyan: p.cyan,
    white: p.white,
    brightBlack: p.brightBlack,
    brightRed: p.brightRed,
    brightGreen: p.brightGreen,
    brightYellow: p.brightYellow,
    brightBlue: p.brightBlue,
    brightMagenta: p.brightMagenta,
    brightCyan: p.brightCyan,
    brightWhite: p.brightWhite,
  };
}
