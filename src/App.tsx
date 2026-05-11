import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import {
  THEMES,
  DEFAULT_THEME_ID,
  findTheme,
  applyPaletteToRoot,
} from "./themes";
import { TerminalView } from "./term-view";
import "./App.css";

type TmuxEvent =
  | { kind: "begin" | "end" | "error"; ts: string; id: string; flags: string }
  | { kind: "output"; pane_id: string; data: string }
  | { kind: "window-add" | "window-close"; window_id: string }
  | { kind: "window-renamed"; window_id: string; name: string }
  | { kind: "session-changed"; session_id: string; name: string }
  | { kind: "layout-change"; window_id: string; layout: string }
  | { kind: "pane-mode-changed"; pane_id: string }
  | { kind: "client-detached" | "exit" }
  | { kind: "unknown" | "non-protocol-line"; raw: string };

type FontMode = "system" | "pixel";

const RECENT_KEY = "tmuxify.recent";
const THEME_KEY = "tmuxify.theme";
const FONT_KEY = "tmuxify.font";

function loadRecent(): string[] {
  try {
    const raw = localStorage.getItem(RECENT_KEY);
    return raw ? (JSON.parse(raw) as string[]) : [];
  } catch {
    return [];
  }
}

function pushRecent(path: string) {
  const cur = loadRecent().filter((p) => p !== path);
  cur.unshift(path);
  localStorage.setItem(RECENT_KEY, JSON.stringify(cur.slice(0, 8)));
}

// @ts-expect-error reserved for future use
function toHexBytes(s: string): string {
  const enc = new TextEncoder().encode(s);
  return Array.from(enc).map((b) => b.toString(16).padStart(2, "0")).join(" ");
}

function App() {
  const [themeId, setThemeId] = useState<string>(
    () => localStorage.getItem(THEME_KEY) ?? DEFAULT_THEME_ID,
  );
  const [fontMode, setFontMode] = useState<FontMode>(
    () => (localStorage.getItem(FONT_KEY) as FontMode) ?? "system",
  );
  const [screen, setScreen] = useState<"picker" | "term">("picker");
  const [cwd, setCwd] = useState<string | null>(null);
  const [recent, setRecent] = useState<string[]>(loadRecent);

  const theme = findTheme(themeId);

  // 테마/폰트 변경 → CSS 변수 + className.
  useEffect(() => {
    applyPaletteToRoot(theme.palette);
    document.documentElement.className = `font-${fontMode}`;
    localStorage.setItem(THEME_KEY, themeId);
    localStorage.setItem(FONT_KEY, fontMode);
  }, [themeId, fontMode, theme]);

  async function pickFolder() {
    const selected = await openDialog({
      directory: true,
      multiple: false,
      title: "프로젝트 폴더 선택",
    });
    if (typeof selected === "string" && selected) {
      openProject(selected);
    }
  }

  function openProject(path: string) {
    pushRecent(path);
    setRecent(loadRecent());
    setCwd(path);
    setScreen("term");
  }

  if (screen === "picker") {
    return (
      <PickerScreen
        themeId={themeId}
        setThemeId={setThemeId}
        fontMode={fontMode}
        setFontMode={setFontMode}
        recent={recent}
        onPick={pickFolder}
        onOpenRecent={openProject}
      />
    );
  }

  return (
    <TerminalScreen
      cwd={cwd!}
      theme={theme}
      fontMode={fontMode}
      onBack={() => {
        setScreen("picker");
        setCwd(null);
      }}
    />
  );
}

/* ============================================================
   시작화면
   ============================================================ */

function PickerScreen({
  themeId,
  setThemeId,
  fontMode,
  setFontMode,
  recent,
  onPick,
  onOpenRecent,
}: {
  themeId: string;
  setThemeId: (id: string) => void;
  fontMode: FontMode;
  setFontMode: (m: FontMode) => void;
  recent: string[];
  onPick: () => void;
  onOpenRecent: (path: string) => void;
}) {
  return (
    <div className="picker">
      <header>
        <div className="logo">▰ tmuxify</div>
        <div className="tagline">
          폴더를 열면 그 안에서 Claude Code 가 자동으로 시작됩니다
        </div>
      </header>

      <section className="picker-main">
        <button className="primary big" onClick={onPick}>
          폴더 열기
        </button>

        {recent.length > 0 && (
          <div className="recent">
            <h3>최근 프로젝트</h3>
            <ul>
              {recent.map((p) => (
                <li key={p}>
                  <button className="recent-row" onClick={() => onOpenRecent(p)}>
                    <span className="recent-name">{basename(p)}</span>
                    <span className="recent-path">{p}</span>
                  </button>
                </li>
              ))}
            </ul>
          </div>
        )}
      </section>

      <footer className="picker-footer">
        <label>
          테마{" "}
          <select value={themeId} onChange={(e) => setThemeId(e.target.value)}>
            {THEMES.map((t) => (
              <option key={t.id} value={t.id}>
                {t.name}
              </option>
            ))}
          </select>
        </label>
        <label>
          폰트{" "}
          <select
            value={fontMode}
            onChange={(e) => setFontMode(e.target.value as FontMode)}
          >
            <option value="system">내 터미널 (D2Coding)</option>
            <option value="pixel">픽셀 (Galmuri)</option>
          </select>
        </label>
      </footer>
    </div>
  );
}

function basename(p: string): string {
  const parts = p.split("/").filter(Boolean);
  return parts[parts.length - 1] ?? p;
}

/* ============================================================
   터미널 화면
   ============================================================ */

function TerminalScreen({
  cwd,
  theme,
  fontMode,
  onBack,
}: {
  cwd: string;
  theme: ReturnType<typeof findTheme>;
  fontMode: FontMode;
  onBack: () => void;
}) {
  const activePaneRef = useRef<string | null>(null);
  const [activePane, setActivePane] = useState<string | null>(null);

  const fontFamily =
    fontMode === "pixel"
      ? '"Galmuri11", "D2Coding", monospace'
      : '"D2CodingLigature Nerd Font Mono", "D2Coding Nerd Font Mono", "D2Coding", "JetBrains Mono", ui-monospace, Menlo, monospace';

  // 컨테이너 크기 변경 → tmux + vt100 parser 둘 다 resize.
  const onSize = useCallback((cols: number, rows: number) => {
    invoke("resize_client", { cols, rows }).catch(() => {});
  }, []);

  // tmux 시작.
  useEffect(() => {
    let unlistenEvent: UnlistenFn | undefined;
    let cancelled = false;

    (async () => {
      unlistenEvent = await listen<TmuxEvent>("tmux-event", (e) => {
        if (e.payload.kind === "exit" && !cancelled) {
          console.warn("[tmux exited]");
        }
      });
      try {
        await invoke<string>("start_tmux", { cwd, autoRun: "claude" });
      } catch (e) {
        console.error("start_tmux", e);
      }
    })();

    return () => {
      cancelled = true;
      unlistenEvent?.();
      invoke("detach_tmux").catch(() => {});
      activePaneRef.current = null;
    };
  }, [cwd]);

  const back = useCallback(() => {
    invoke("detach_tmux").catch(() => {});
    onBack();
  }, [onBack]);

  // 맥 단축키 — tmux 표준 split 으로 매핑.
  // Cmd+D       : 좌우 분할 (vertical pane → split-window -h)
  // Cmd+Shift+D : 상하 분할 (horizontal pane → split-window -v)
  // Cmd+W       : 현재 pane 종료
  // Cmd+[ / Cmd+] : 이전/다음 pane
  useEffect(() => {
    const onShortcut = (e: KeyboardEvent) => {
      if (!e.metaKey) return;
      let cmd: string | null = null;
      const k = e.key.toLowerCase();
      if (k === "d" && !e.shiftKey) cmd = "split-window -h";
      else if (k === "d" && e.shiftKey) cmd = "split-window -v";
      else if (k === "w") cmd = "kill-pane";
      else if (e.key === "[") cmd = "select-pane -t :.-";
      else if (e.key === "]") cmd = "select-pane -t :.+";
      if (cmd) {
        e.preventDefault();
        invoke("send_tmux_cmd", { cmd }).catch((err) =>
          console.error("send_tmux_cmd", err),
        );
      }
    };
    window.addEventListener("keydown", onShortcut);
    return () => window.removeEventListener("keydown", onShortcut);
  }, []);

  return (
    <div className="term-screen">
      <div className="topbar">
        <button onClick={back} title="시작화면으로">
          ←
        </button>
        <span className="title">▰ {basename(cwd)}</span>
        <span className="path">{cwd}</span>
        <span className="spacer" />
        <span className="muted">{activePane ? `pane ${activePane}` : "…"}</span>
      </div>
      <div className="term-host">
        <TerminalView
          palette={theme.palette}
          fontFamily={fontFamily}
          fontSize={14}
          onSize={onSize}
          onActivePane={setActivePane}
          activePaneRef={activePaneRef}
        />
      </div>
    </div>
  );
}

export default App;
