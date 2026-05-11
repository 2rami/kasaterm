/**
 * Tauri 측 vt100 파서가 보낸 셀 그리드를 받아서 DOM 으로 그리는 컴포넌트.
 *
 * 핵심 원리:
 *  - Rust 가 ANSI 해석 + 화면 버퍼 유지 → tmux-screen 이벤트로 변경 row 만 전송.
 *  - 프론트는 단순히 받은 row 를 DOM 으로 mutation. xterm.js / WebGL 불필요.
 *  - 입력은 진짜 <textarea> — macOS IME 정상 작동.
 *
 * 직렬화 wire 포맷 (Rust 의 ScreenWire 와 동기 유지):
 *   { pane_id, rows, cols, dirty: [(row_idx, cells)], cursor_row, cursor_col, cursor_visible, alt }
 *   CellWire: { ch, fg?, bg?, a? } where fg/bg = { idx: n } | { hex: "#rrggbb" } | omitted
 */

import { useEffect, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { Palette } from "./themes";

type ColorWire = { idx: number } | { hex: string };
type CellWire = {
  ch: string;
  fg?: ColorWire;
  bg?: ColorWire;
  a?: number;
};
type RowWire = CellWire[];
type ScreenWire = {
  pane_id: string;
  rows: number;
  cols: number;
  dirty: [number, RowWire][];
  cursor_row: number;
  cursor_col: number;
  cursor_visible: boolean;
  alt: boolean;
};

const ATTR_BOLD = 1;
const ATTR_ITALIC = 2;
const ATTR_UNDERLINE = 4;
const ATTR_INVERSE = 8;

function toHexBytes(s: string): string {
  return Array.from(new TextEncoder().encode(s))
    .map((b) => b.toString(16).padStart(2, "0"))
    .join(" ");
}

/** 6x6x6 + grayscale 256 색 인덱스 → CSS rgb */
function ansi256ToCss(idx: number, palette: Palette): string {
  if (idx < 16) return palette16(idx, palette);
  if (idx < 232) {
    const i = idx - 16;
    const r = Math.floor(i / 36);
    const g = Math.floor((i % 36) / 6);
    const b = i % 6;
    const v = (n: number) => (n === 0 ? 0 : 55 + n * 40);
    return `rgb(${v(r)},${v(g)},${v(b)})`;
  }
  const gray = 8 + (idx - 232) * 10;
  return `rgb(${gray},${gray},${gray})`;
}

function palette16(i: number, p: Palette): string {
  const table = [
    p.black, p.red, p.green, p.yellow, p.blue, p.magenta, p.cyan, p.white,
    p.brightBlack, p.brightRed, p.brightGreen, p.brightYellow,
    p.brightBlue, p.brightMagenta, p.brightCyan, p.brightWhite,
  ];
  return table[i] ?? p.fg;
}

function colorCss(c: ColorWire | undefined, palette: Palette, fallback: string): string {
  if (!c) return fallback;
  if ("idx" in c) return ansi256ToCss(c.idx, palette);
  return c.hex;
}

/** 같은 attr 인접 셀 묶기용 키 */
function styleKey(c: CellWire): string {
  return `${c.fg ? JSON.stringify(c.fg) : "-"}|${c.bg ? JSON.stringify(c.bg) : "-"}|${c.a ?? 0}`;
}

export type TerminalViewProps = {
  palette: Palette;
  fontFamily: string;
  fontSize: number;
  /** 호스트 div 의 width/height 가 바뀌면 cols/rows 재계산해서 알려줌 */
  onSize?: (cols: number, rows: number) => void;
  /** 활성 pane id 가 정해지면 부모에 알림 (첫 tmux-screen 의 pane_id) */
  onActivePane?: (paneId: string) => void;
  /** 입력 전송 — 호출자가 send_keys_hex 호출 */
  activePaneRef: React.MutableRefObject<string | null>;
};

export function TerminalView({
  palette,
  fontFamily,
  fontSize,
  onSize,
  onActivePane,
  activePaneRef,
}: TerminalViewProps) {
  const hostRef = useRef<HTMLDivElement>(null);
  const gridRef = useRef<HTMLDivElement>(null);
  const cursorRef = useRef<HTMLDivElement>(null);
  const textareaRef = useRef<HTMLTextAreaElement>(null);
  const rowsRef = useRef<HTMLDivElement[]>([]);
  const cellSizeRef = useRef<{ w: number; h: number }>({ w: 8, h: 16 });
  const composingRef = useRef(false);
  const sizeRef = useRef<{ cols: number; rows: number }>({ cols: 80, rows: 24 });

  // 측정 + 그리드 셋업
  useEffect(() => {
    const host = hostRef.current;
    const grid = gridRef.current;
    if (!host || !grid) return;

    // 셀 측정 — 임시 span 으로
    const measure = () => {
      const probe = document.createElement("span");
      probe.style.fontFamily = fontFamily;
      probe.style.fontSize = `${fontSize}px`;
      probe.style.lineHeight = "1.2";
      probe.style.position = "absolute";
      probe.style.visibility = "hidden";
      probe.style.whiteSpace = "pre";
      probe.textContent = "MMMMMMMMMM";
      host.appendChild(probe);
      const r = probe.getBoundingClientRect();
      const w = (r.width || 80) / 10;
      const h = r.height || fontSize * 1.2;
      probe.remove();
      cellSizeRef.current = { w, h };
    };
    measure();

    const updateSize = () => {
      const rect = host.getBoundingClientRect();
      const { w, h } = cellSizeRef.current;
      const cols = Math.max(20, Math.floor(rect.width / w));
      const rows = Math.max(5, Math.floor(rect.height / h));
      if (cols !== sizeRef.current.cols || rows !== sizeRef.current.rows) {
        sizeRef.current = { cols, rows };
        onSize?.(cols, rows);
      }
    };
    updateSize();
    const obs = new ResizeObserver(updateSize);
    obs.observe(host);

    return () => {
      obs.disconnect();
    };
  }, [fontFamily, fontSize, onSize]);

  // tmux-screen 이벤트 처리
  useEffect(() => {
    let unlisten: UnlistenFn | undefined;

    (async () => {
      unlisten = await listen<ScreenWire>("tmux-screen", (e) => {
        const payload = e.payload;
        const grid = gridRef.current;
        const cursor = cursorRef.current;
        if (!grid || !cursor) return;

        // 활성 pane 결정 — 첫 수신 pane 또는 사용자가 이미 정한 것
        if (!activePaneRef.current) {
          activePaneRef.current = payload.pane_id;
          onActivePane?.(payload.pane_id);
        }
        if (payload.pane_id !== activePaneRef.current) return;

        // 행 수 동기화
        const need = payload.rows;
        const haveRows = rowsRef.current;
        while (haveRows.length < need) {
          const row = document.createElement("div");
          row.className = "tv-row";
          grid.appendChild(row);
          haveRows.push(row);
        }
        while (haveRows.length > need) {
          const r = haveRows.pop();
          r?.remove();
        }

        // dirty 행만 갱신
        for (const [idx, cells] of payload.dirty) {
          renderRow(haveRows[idx], cells, palette);
        }

        // 커서 위치
        const { w, h } = cellSizeRef.current;
        cursor.style.transform = `translate(${payload.cursor_col * w}px, ${payload.cursor_row * h}px)`;
        cursor.style.width = `${w}px`;
        cursor.style.height = `${h}px`;
        cursor.style.display = payload.cursor_visible ? "" : "none";
        cursor.style.background = palette.fg;

        // textarea 도 커서 위치로 — IME 후보창이 정확한 자리에
        const ta = textareaRef.current;
        if (ta) {
          ta.style.left = `${payload.cursor_col * w}px`;
          ta.style.top = `${payload.cursor_row * h}px`;
        }
      });
    })();

    return () => {
      unlisten?.();
    };
  }, [palette, activePaneRef, onActivePane]);

  // 키 입력 핸들러
  const onTextareaInput = (e: React.FormEvent<HTMLTextAreaElement>) => {
    if (composingRef.current) return;
    const el = e.currentTarget;
    const v = el.value;
    if (v.length > 0) {
      sendText(v, activePaneRef.current);
      el.value = "";
    }
  };

  const onCompEnd = (e: React.CompositionEvent<HTMLTextAreaElement>) => {
    composingRef.current = false;
    if (e.data) sendText(e.data, activePaneRef.current);
    if (textareaRef.current) textareaRef.current.value = "";
  };

  const onKeyDown = (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
    if (composingRef.current) return;
    const seq = keyToSeq(e);
    if (seq !== null) {
      e.preventDefault();
      sendText(seq, activePaneRef.current);
    }
  };

  return (
    <div
      className="tv-host"
      ref={hostRef}
      style={{
        fontFamily,
        fontSize: `${fontSize}px`,
        lineHeight: "1.2",
        position: "relative",
        overflow: "hidden",
        background: palette.bg1,
        color: palette.fg,
        width: "100%",
        height: "100%",
      }}
      onMouseDown={() => textareaRef.current?.focus()}
    >
      <div className="tv-grid" ref={gridRef} />
      <div
        className="tv-cursor"
        ref={cursorRef}
        style={{
          position: "absolute",
          top: 0,
          left: 0,
          mixBlendMode: "difference",
          pointerEvents: "none",
        }}
      />
      <textarea
        ref={textareaRef}
        className="tv-input"
        autoCapitalize="off"
        autoComplete="off"
        spellCheck={false}
        onInput={onTextareaInput}
        onCompositionStart={() => {
          composingRef.current = true;
        }}
        onCompositionEnd={onCompEnd}
        onKeyDown={onKeyDown}
        style={{
          position: "absolute",
          top: 0,
          left: 0,
          width: "2ch",
          height: "1em",
          opacity: 0,
          border: 0,
          outline: 0,
          resize: "none",
          background: "transparent",
          color: "transparent",
          caretColor: "transparent",
          font: "inherit",
          padding: 0,
          overflow: "hidden",
        }}
      />
    </div>
  );
}

function sendText(text: string, paneId: string | null) {
  const hex = toHexBytes(text);
  invoke("send_keys_hex", { paneId, hex }).catch((e) =>
    console.error("send_keys_hex", e),
  );
}

function renderRow(el: HTMLDivElement, cells: RowWire, palette: Palette) {
  // run-length 묶기 — 같은 style 인접 셀들을 하나의 span 으로
  const frag = document.createDocumentFragment();
  let i = 0;
  while (i < cells.length) {
    let j = i + 1;
    const k = styleKey(cells[i]);
    while (j < cells.length && styleKey(cells[j]) === k) j++;
    const span = document.createElement("span");
    let text = "";
    for (let m = i; m < j; m++) text += cells[m].ch || " ";
    span.textContent = text;
    applyStyle(span, cells[i], palette);
    frag.appendChild(span);
    i = j;
  }
  el.replaceChildren(frag);
}

function applyStyle(el: HTMLSpanElement, c: CellWire, palette: Palette) {
  let fg = colorCss(c.fg, palette, palette.fg);
  let bg = colorCss(c.bg, palette, palette.bg1);
  const a = c.a ?? 0;
  if (a & ATTR_INVERSE) [fg, bg] = [bg, fg];
  el.style.color = fg;
  if (bg !== palette.bg1) el.style.backgroundColor = bg;
  if (a & ATTR_BOLD) el.style.fontWeight = "bold";
  if (a & ATTR_ITALIC) el.style.fontStyle = "italic";
  if (a & ATTR_UNDERLINE) el.style.textDecoration = "underline";
}

function keyToSeq(e: React.KeyboardEvent): string | null {
  if (e.metaKey) return null;
  const key = e.key;
  const ctrl = e.ctrlKey;
  const alt = e.altKey;
  if (ctrl && key.length === 1) {
    const lower = key.toLowerCase();
    const code = lower.charCodeAt(0);
    if (code >= 0x61 && code <= 0x7a) return String.fromCharCode(code - 0x60);
  }
  if (alt && key.length === 1 && !ctrl) return "\x1b" + key;
  switch (key) {
    case "Enter": return "\r";
    case "Backspace": return "\x7f";
    case "Tab": return e.shiftKey ? "\x1b[Z" : "\t";
    case "Escape": return "\x1b";
    case "ArrowUp": return "\x1b[A";
    case "ArrowDown": return "\x1b[B";
    case "ArrowRight": return "\x1b[C";
    case "ArrowLeft": return "\x1b[D";
    case "Home": return "\x1b[H";
    case "End": return "\x1b[F";
    case "PageUp": return "\x1b[5~";
    case "PageDown": return "\x1b[6~";
    case "Delete": return "\x1b[3~";
  }
  return null;
}
