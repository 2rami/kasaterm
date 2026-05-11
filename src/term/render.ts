/**
 * vanilla DOM 렌더러 — ScreenBuffer 의 dirty 행만 갱신.
 *
 * 구조:
 *   <div class="tm-host">
 *     <div class="tm-grid">
 *       <div class="tm-row" data-row="0">
 *         <span class="tm-cell" style="..."> char </span>
 *         ...
 *       </div>
 *       ...
 *     </div>
 *     <div class="tm-cursor"></div>
 *   </div>
 *
 * 셀 단위 span 은 동일 attr 인접하면 한 span 으로 묶음 (run-length).
 * 폰트는 ch metric 캐싱 후 셀 폭/높이로 사용.
 */

import type { ScreenBuffer, Cell } from "./buffer";

export type Palette16 = {
  /** ANSI 0~15 — Cell.fg/bg 가 number 일 때 사용 */
  ansi: string[];
  /** fg=null 일 때 기본색 */
  defaultFg: string;
  /** bg=null 일 때 기본색 (보통 투명) */
  defaultBg: string;
  /** 커서 색 */
  cursor: string;
};

export class TerminalRenderer {
  readonly host: HTMLDivElement;
  private grid: HTMLDivElement;
  private cursorEl: HTMLDivElement;
  private rowEls: HTMLDivElement[] = [];

  private cellW = 0;
  private cellH = 0;

  constructor(
    private buffer: ScreenBuffer,
    private palette: Palette16,
  ) {
    this.host = document.createElement("div");
    this.host.className = "tm-host";

    this.grid = document.createElement("div");
    this.grid.className = "tm-grid";
    this.host.appendChild(this.grid);

    this.cursorEl = document.createElement("div");
    this.cursorEl.className = "tm-cursor";
    this.host.appendChild(this.cursorEl);

    this.rebuildRows();
  }

  /** 호스트 div 를 외부 컨테이너에 부착 후 호출. */
  mount(container: HTMLElement) {
    container.appendChild(this.host);
    this.measureCell();
    this.renderAll();
  }

  unmount() {
    this.host.remove();
  }

  setPalette(p: Palette16) {
    this.palette = p;
    this.renderAll();
  }

  /** 폰트 변경 등으로 셀 크기 재측정 + 전체 다시 그림. */
  remeasure() {
    this.measureCell();
    this.renderAll();
  }

  /** 컨테이너 크기로부터 (cols, rows) 계산. */
  computeGridSize(container: HTMLElement): { cols: number; rows: number } {
    if (this.cellW === 0 || this.cellH === 0) this.measureCell();
    const rect = container.getBoundingClientRect();
    const cols = Math.max(1, Math.floor(rect.width / this.cellW));
    const rows = Math.max(1, Math.floor(rect.height / this.cellH));
    return { cols, rows };
  }

  /** 버퍼 크기 바뀌면 호출 — DOM 행 재구성. */
  syncRows() {
    if (this.rowEls.length !== this.buffer.rows) this.rebuildRows();
  }

  /** dirty 만 반영. requestAnimationFrame 으로 호출. */
  renderDirty() {
    if (this.buffer.dirtyAll) {
      this.renderAll();
      this.buffer.clearDirty();
      return;
    }
    for (const r of this.buffer.dirty) {
      this.renderRow(r);
    }
    this.buffer.clearDirty();
    this.placeCursor();
  }

  renderAll() {
    if (import.meta.env.DEV) {
      console.log("[render] renderAll rows=", this.buffer.rows, "cols=", this.buffer.cols, "cellW=", this.cellW, "cellH=", this.cellH);
    }
    for (let r = 0; r < this.buffer.rows; r++) this.renderRow(r);
    this.placeCursor();
  }

  // ----- private -----

  private rebuildRows() {
    this.grid.replaceChildren();
    this.rowEls = [];
    for (let r = 0; r < this.buffer.rows; r++) {
      const el = document.createElement("div");
      el.className = "tm-row";
      el.dataset.row = String(r);
      this.grid.appendChild(el);
      this.rowEls.push(el);
    }
  }

  private measureCell() {
    // 측정용 임시 span — ASCII 한 글자 폭 + 행 높이.
    // CJK 는 별도로 측정하지 않고 정확히 2 * ASCII 로 강제.
    const probe = document.createElement("span");
    probe.className = "tm-cell";
    probe.textContent = "MMMMMMMMMM"; // 10 글자 평균
    probe.style.visibility = "hidden";
    probe.style.position = "absolute";
    probe.style.display = "inline-block";
    this.host.appendChild(probe);
    const r = probe.getBoundingClientRect();
    this.cellW = (r.width || 80) / 10;
    this.cellH = r.height || 18;
    probe.remove();
    this.host.style.setProperty("--cell-w", `${this.cellW}px`);
    this.host.style.setProperty("--cell-h", `${this.cellH}px`);
    // 행 높이를 cellH 로 강제
    for (const el of this.rowEls) el.style.height = `${this.cellH}px`;
  }

  private renderRow(r: number) {
    if (r < 0 || r >= this.buffer.rows) return;
    const snap = this.buffer.snapshot();
    const row = snap.cells[r];
    if (!row) return;
    const el = this.rowEls[r];
    if (!el) return;

    // run-length 묶기 — 같은 attr 인접 셀들을 한 span 으로.
    // 각 span 의 width 는 (해당 run 의 버퍼 셀 수 * cellW) 로 강제.
    // CJK 와이드 글자가 차지하는 wcont 셀도 폭 계산에 포함됨 (span width 늘림).
    const frag = document.createDocumentFragment();
    let runStart = 0;
    let prevKey = cellStyleKey(row[0]);

    const flush = (end: number) => {
      const span = document.createElement("span");
      span.className = "tm-cell";
      const sample = row[runStart];
      applyCellStyle(span, sample, this.palette);
      let s = "";
      for (let i = runStart; i < end; i++) {
        const c = row[i];
        if (c.wcont) continue;
        s += c.ch || " ";
      }
      span.textContent = s;
      frag.appendChild(span);
    };

    for (let i = 1; i < row.length; i++) {
      const k = cellStyleKey(row[i]);
      if (k !== prevKey) {
        flush(i);
        runStart = i;
        prevKey = k;
      }
    }
    flush(row.length);

    el.replaceChildren(frag);
  }

  private placeCursor() {
    const snap = this.buffer.snapshot();
    if (!snap.cursorVisible) {
      this.cursorEl.style.display = "none";
    } else {
      this.cursorEl.style.display = "";
      this.cursorEl.style.transform = `translate(${snap.cursorCol * this.cellW}px, ${snap.cursorRow * this.cellH}px)`;
      this.cursorEl.style.width = `${this.cellW}px`;
      this.cursorEl.style.height = `${this.cellH}px`;
      this.cursorEl.style.background = this.palette.cursor;
    }
    // IME 입력 위치도 cursor 따라가게 — macOS IME 후보창이 정확한 위치에 뜸
    this.onCursorMove?.(snap.cursorCol * this.cellW, snap.cursorRow * this.cellH);
  }

  /** cursor 좌표 바뀔 때마다 호출되는 콜백 (IME textarea 위치 동기화용) */
  onCursorMove?: (x: number, y: number) => void;
}

/** 같은 스타일이면 같은 키 — run-length 묶기용 */
function cellStyleKey(c: Cell): string {
  return [
    c.fg ?? "-",
    c.bg ?? "-",
    c.bold ? "B" : "-",
    c.faint ? "f" : "-",
    c.italic ? "I" : "-",
    c.underline ? "U" : "-",
    c.inverse ? "V" : "-",
    c.strike ? "S" : "-",
    c.hidden ? "H" : "-",
  ].join("");
}

function applyCellStyle(el: HTMLSpanElement, c: Cell, p: Palette16) {
  let fg = resolveColor(c.fg, p, "fg");
  let bg = resolveColor(c.bg, p, "bg");
  if (c.inverse) [fg, bg] = [bg, fg];
  if (c.hidden) fg = bg;
  el.style.color = fg;
  el.style.backgroundColor = bg === p.defaultBg ? "" : bg;
  el.style.fontWeight = c.bold ? "bold" : "";
  el.style.fontStyle = c.italic ? "italic" : "";
  el.style.opacity = c.faint ? "0.6" : "";
  el.style.textDecoration =
    [c.underline ? "underline" : "", c.strike ? "line-through" : ""]
      .filter(Boolean)
      .join(" ") || "";
}

function resolveColor(
  v: number | string | null,
  p: Palette16,
  kind: "fg" | "bg",
): string {
  if (v === null || v === undefined) {
    return kind === "fg" ? p.defaultFg : p.defaultBg;
  }
  if (typeof v === "string") return v; // already hex
  if (v >= 0 && v <= 15) return p.ansi[v];
  if (v >= 16 && v <= 231) {
    // 6x6x6 cube
    const i = v - 16;
    const r = Math.floor(i / 36);
    const g = Math.floor((i % 36) / 6);
    const b = i % 6;
    const channel = (n: number) => (n === 0 ? 0 : 55 + n * 40);
    return `rgb(${channel(r)},${channel(g)},${channel(b)})`;
  }
  if (v >= 232 && v <= 255) {
    const gray = 8 + (v - 232) * 10;
    return `rgb(${gray},${gray},${gray})`;
  }
  return kind === "fg" ? p.defaultFg : p.defaultBg;
}
