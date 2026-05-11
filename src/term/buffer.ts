/**
 * 스크린 버퍼 — rows × cols 셀 그리드 + 커서 + 현재 attr.
 *
 * 메인/alt 두 버퍼 보유. ALT 진입 시 cursor 저장, ALT 화면은 매번 clear.
 * Scrollback 은 메인 화면에서 줄이 위로 밀려나갈 때만 쌓임.
 *
 * 와이드 문자(CJK): char.length===1 but visual width===2.
 * 와이드 글자가 (r,c) 에 들어가면 (r,c+1) 은 wcontinuation=true 셀이 됨.
 * 렌더러는 wcontinuation 셀을 그리지 않음(앞 셀이 2칸 차지).
 */

import type { SgrAttr, TerminalOp } from "./parser";

export type Cell = {
  ch: string; // "" 면 빈칸
  fg: number | string | null;
  bg: number | string | null;
  bold: boolean;
  faint: boolean;
  italic: boolean;
  underline: boolean;
  blink: boolean;
  inverse: boolean;
  hidden: boolean;
  strike: boolean;
  /** 와이드 글자의 두 번째 셀 자리 */
  wcont: boolean;
};

function blankCell(): Cell {
  return {
    ch: "",
    fg: null,
    bg: null,
    bold: false,
    faint: false,
    italic: false,
    underline: false,
    blink: false,
    inverse: false,
    hidden: false,
    strike: false,
    wcont: false,
  };
}

function blankRow(cols: number): Cell[] {
  const row: Cell[] = new Array(cols);
  for (let i = 0; i < cols; i++) row[i] = blankCell();
  return row;
}

/** 동아시아 와이드 문자 판정 (간이판 — 한자/한글/일본어 영역 위주) */
export function isWide(ch: string): boolean {
  if (!ch) return false;
  const code = ch.codePointAt(0) ?? 0;
  return (
    (code >= 0x1100 && code <= 0x115f) || // Hangul Jamo
    (code >= 0x2e80 && code <= 0x303e) || // CJK Radicals/Symbols
    (code >= 0x3041 && code <= 0x33ff) || // Hiragana/Katakana/Bopomofo/Hangul Compatibility Jamo
    (code >= 0x3400 && code <= 0x4dbf) || // CJK Ext A
    (code >= 0x4e00 && code <= 0x9fff) || // CJK Unified
    (code >= 0xa000 && code <= 0xa4cf) || // Yi
    (code >= 0xac00 && code <= 0xd7a3) || // Hangul Syllables
    (code >= 0xf900 && code <= 0xfaff) || // CJK Compat
    (code >= 0xfe30 && code <= 0xfe4f) ||
    (code >= 0xff00 && code <= 0xff60) || // Fullwidth Forms
    (code >= 0xffe0 && code <= 0xffe6) ||
    (code >= 0x20000 && code <= 0x2fffd) ||
    (code >= 0x30000 && code <= 0x3fffd)
  );
}

type CursorState = {
  row: number;
  col: number;
  attr: SgrAttr;
  /** 다음 char 를 찍기 직전에 줄바꿈을 미루는 플래그 (xterm 의 wraparound 와 동일) */
  wrapPending: boolean;
};

export type ScreenSnapshot = {
  rows: number;
  cols: number;
  cells: Cell[][];
  cursorRow: number;
  cursorCol: number;
  cursorVisible: boolean;
  altActive: boolean;
};

export class ScreenBuffer {
  rows: number;
  cols: number;

  /** 메인 / alt 셀 그리드 */
  private main: Cell[][];
  private alt: Cell[][];
  /** 메인의 scrollback (위쪽으로 밀려난 행) — alt 일 때는 채워지지 않음 */
  scrollback: Cell[][] = [];
  scrollbackLimit = 5000;

  /** 어떤 버퍼가 활성? */
  private altActive = false;
  /** 활성 버퍼에 대한 커서 */
  private cursor: CursorState;
  /** alt 진입 전 메인 커서 저장 (DEC 1049) */
  private savedMainCursor: CursorState | null = null;
  /** ESC 7 (DECSC) 저장 */
  private savedCursor: CursorState | null = null;

  cursorVisible = true;
  /** dirty row index 집합 — 렌더러가 읽고 비움 */
  dirty: Set<number> = new Set();
  /** 전체 다시 그려야 함 */
  dirtyAll = false;

  constructor(rows = 24, cols = 80) {
    this.rows = rows;
    this.cols = cols;
    this.main = this.makeGrid();
    this.alt = this.makeGrid();
    this.cursor = this.freshCursor();
    this.markAllDirty();
  }

  private makeGrid(): Cell[][] {
    const g: Cell[][] = new Array(this.rows);
    for (let r = 0; r < this.rows; r++) g[r] = blankRow(this.cols);
    return g;
  }

  private freshCursor(): CursorState {
    return { row: 0, col: 0, attr: {}, wrapPending: false };
  }

  private get grid(): Cell[][] {
    return this.altActive ? this.alt : this.main;
  }

  /** 현재 grid 의 사본을 얻음 (렌더용) */
  snapshot(): ScreenSnapshot {
    return {
      rows: this.rows,
      cols: this.cols,
      cells: this.grid,
      cursorRow: this.cursor.row,
      cursorCol: this.cursor.col,
      cursorVisible: this.cursorVisible,
      altActive: this.altActive,
    };
  }

  /** 외부에서 dirty 다 처리한 후 호출 */
  clearDirty() {
    this.dirty.clear();
    this.dirtyAll = false;
  }

  resize(rows: number, cols: number) {
    if (rows === this.rows && cols === this.cols) return;
    // 단순: alt/main 둘 다 재할당. 기존 컨텐츠 가능한 만큼 보존.
    this.main = resizeGrid(this.main, rows, cols);
    this.alt = resizeGrid(this.alt, rows, cols);
    this.rows = rows;
    this.cols = cols;
    if (this.cursor.row >= rows) this.cursor.row = rows - 1;
    if (this.cursor.col >= cols) this.cursor.col = cols - 1;
    this.markAllDirty();
  }

  apply(ops: TerminalOp[]) {
    for (const op of ops) this.applyOne(op);
  }

  private applyOne(op: TerminalOp) {
    switch (op.type) {
      case "print":
        this.print(op.text);
        return;
      case "lf":
        this.lineFeed();
        return;
      case "cr":
        this.cursor.col = 0;
        this.cursor.wrapPending = false;
        return;
      case "bs":
        if (this.cursor.col > 0) this.cursor.col--;
        this.cursor.wrapPending = false;
        return;
      case "tab":
        this.cursor.col = Math.min(this.cols - 1, ((this.cursor.col >> 3) + 1) << 3);
        return;
      case "bell":
        return;
      case "cup":
        this.cursor.row = clamp(op.row, 0, this.rows - 1);
        this.cursor.col = clamp(op.col, 0, this.cols - 1);
        this.cursor.wrapPending = false;
        return;
      case "cuu":
        this.cursor.row = Math.max(0, this.cursor.row - op.n);
        return;
      case "cud":
        this.cursor.row = Math.min(this.rows - 1, this.cursor.row + op.n);
        return;
      case "cuf":
        this.cursor.col = Math.min(this.cols - 1, this.cursor.col + op.n);
        return;
      case "cub":
        this.cursor.col = Math.max(0, this.cursor.col - op.n);
        return;
      case "cha":
        this.cursor.col = clamp(op.col, 0, this.cols - 1);
        return;
      case "el":
        this.eraseInLine(op.mode);
        return;
      case "ed":
        this.eraseInDisplay(op.mode);
        return;
      case "ech":
        this.eraseChar(op.n);
        return;
      case "ich":
        this.insertChars(op.n);
        return;
      case "dch":
        this.deleteChars(op.n);
        return;
      case "il":
        this.insertLines(op.n);
        return;
      case "dl":
        this.deleteLines(op.n);
        return;
      case "su":
        this.scrollUp(op.n);
        return;
      case "sd":
        this.scrollDown(op.n);
        return;
      case "sgr":
        this.applySgr(op.attrs, !!op.reset);
        return;
      case "altScreen":
        this.setAltScreen(op.on, op.saveCursor);
        return;
      case "cursorVisible":
        this.cursorVisible = op.visible;
        this.markRowDirty(this.cursor.row);
        return;
      case "saveCursor":
        this.savedCursor = { ...this.cursor, attr: { ...this.cursor.attr } };
        return;
      case "restoreCursor":
        if (this.savedCursor) {
          this.cursor = { ...this.savedCursor, attr: { ...this.savedCursor.attr } };
        }
        return;
      case "reset":
        this.fullReset();
        return;
      // bracketedPaste / appCursorKeys / title / unknown 은 버퍼에 영향 없음
      default:
        return;
    }
  }

  // ---------- 핵심 동작 ----------

  private print(text: string) {
    // 멀티 코드포인트 안전 iteration (이모지 등)
    for (const ch of text) {
      if (this.cursor.wrapPending) {
        this.cursor.col = 0;
        this.lineFeed();
        this.cursor.wrapPending = false;
      }
      const wide = isWide(ch);
      const width = wide ? 2 : 1;
      if (this.cursor.col + width > this.cols) {
        // 줄 끝에서 한 글자가 안 들어가면 다음 줄로
        this.cursor.col = 0;
        this.lineFeed();
      }
      const row = this.grid[this.cursor.row];
      this.writeCell(row, this.cursor.col, ch);
      if (wide && this.cursor.col + 1 < this.cols) {
        row[this.cursor.col + 1] = {
          ...blankCell(),
          wcont: true,
          fg: this.cursor.attr.fg ?? null,
          bg: this.cursor.attr.bg ?? null,
        };
      }
      this.markRowDirty(this.cursor.row);
      this.cursor.col += width;
      if (this.cursor.col >= this.cols) {
        this.cursor.col = this.cols - 1;
        this.cursor.wrapPending = true;
      }
    }
  }

  private writeCell(row: Cell[], col: number, ch: string) {
    const c = row[col];
    const a = this.cursor.attr;
    c.ch = ch;
    c.fg = a.fg ?? null;
    c.bg = a.bg ?? null;
    c.bold = !!a.bold;
    c.faint = !!a.faint;
    c.italic = !!a.italic;
    c.underline = !!a.underline;
    c.blink = !!a.blink;
    c.inverse = !!a.inverse;
    c.hidden = !!a.hidden;
    c.strike = !!a.strike;
    c.wcont = false;
  }

  private lineFeed() {
    if (this.cursor.row >= this.rows - 1) {
      this.scrollUp(1);
    } else {
      this.cursor.row++;
    }
  }

  private eraseInLine(mode: 0 | 1 | 2) {
    const row = this.grid[this.cursor.row];
    const [from, to] =
      mode === 0
        ? [this.cursor.col, this.cols]
        : mode === 1
        ? [0, this.cursor.col + 1]
        : [0, this.cols];
    for (let c = from; c < to; c++) row[c] = blankCell();
    this.markRowDirty(this.cursor.row);
  }

  private eraseInDisplay(mode: 0 | 1 | 2 | 3) {
    if (mode === 2 || mode === 3) {
      for (let r = 0; r < this.rows; r++) this.grid[r] = blankRow(this.cols);
      if (mode === 3) this.scrollback = [];
      this.markAllDirty();
      return;
    }
    if (mode === 0) {
      // cursor → end of screen
      const row = this.grid[this.cursor.row];
      for (let c = this.cursor.col; c < this.cols; c++) row[c] = blankCell();
      for (let r = this.cursor.row + 1; r < this.rows; r++) {
        this.grid[r] = blankRow(this.cols);
      }
    } else {
      // 1: start → cursor
      for (let r = 0; r < this.cursor.row; r++) this.grid[r] = blankRow(this.cols);
      const row = this.grid[this.cursor.row];
      for (let c = 0; c <= this.cursor.col; c++) row[c] = blankCell();
    }
    this.markAllDirty();
  }

  private eraseChar(n: number) {
    const row = this.grid[this.cursor.row];
    for (let i = 0; i < n && this.cursor.col + i < this.cols; i++) {
      row[this.cursor.col + i] = blankCell();
    }
    this.markRowDirty(this.cursor.row);
  }

  private insertChars(n: number) {
    const row = this.grid[this.cursor.row];
    for (let i = this.cols - 1; i >= this.cursor.col + n; i--) {
      row[i] = row[i - n];
    }
    for (let i = this.cursor.col; i < this.cursor.col + n && i < this.cols; i++) {
      row[i] = blankCell();
    }
    this.markRowDirty(this.cursor.row);
  }

  private deleteChars(n: number) {
    const row = this.grid[this.cursor.row];
    for (let i = this.cursor.col; i + n < this.cols; i++) {
      row[i] = row[i + n];
    }
    for (let i = this.cols - n; i < this.cols; i++) row[i] = blankCell();
    this.markRowDirty(this.cursor.row);
  }

  private insertLines(n: number) {
    const r = this.cursor.row;
    for (let i = this.rows - 1; i >= r + n; i--) {
      this.grid[i] = this.grid[i - n];
    }
    for (let i = r; i < r + n && i < this.rows; i++) {
      this.grid[i] = blankRow(this.cols);
    }
    this.markAllDirty();
  }

  private deleteLines(n: number) {
    const r = this.cursor.row;
    for (let i = r; i + n < this.rows; i++) {
      this.grid[i] = this.grid[i + n];
    }
    for (let i = this.rows - n; i < this.rows; i++) {
      this.grid[i] = blankRow(this.cols);
    }
    this.markAllDirty();
  }

  private scrollUp(n: number) {
    for (let i = 0; i < n; i++) {
      const removed = this.grid.shift();
      if (removed) {
        if (!this.altActive) {
          this.scrollback.push(removed);
          if (this.scrollback.length > this.scrollbackLimit) {
            this.scrollback.shift();
          }
        }
      }
      this.grid.push(blankRow(this.cols));
    }
    this.markAllDirty();
  }

  private scrollDown(n: number) {
    for (let i = 0; i < n; i++) {
      this.grid.pop();
      this.grid.unshift(blankRow(this.cols));
    }
    this.markAllDirty();
  }

  private applySgr(attrs: SgrAttr, reset: boolean) {
    if (reset) this.cursor.attr = {};
    Object.assign(this.cursor.attr, attrs);
  }

  private setAltScreen(on: boolean, saveCursor: boolean) {
    if (on === this.altActive) return;
    if (on) {
      if (saveCursor) {
        this.savedMainCursor = { ...this.cursor, attr: { ...this.cursor.attr } };
      }
      this.altActive = true;
      // alt 들어가면 화면 클리어
      this.alt = this.makeGrid();
      this.cursor = this.freshCursor();
    } else {
      this.altActive = false;
      if (saveCursor && this.savedMainCursor) {
        this.cursor = { ...this.savedMainCursor, attr: { ...this.savedMainCursor.attr } };
        this.savedMainCursor = null;
      }
    }
    this.markAllDirty();
  }

  private fullReset() {
    this.main = this.makeGrid();
    this.alt = this.makeGrid();
    this.cursor = this.freshCursor();
    this.savedCursor = null;
    this.savedMainCursor = null;
    this.altActive = false;
    this.cursorVisible = true;
    this.scrollback = [];
    this.markAllDirty();
  }

  private markRowDirty(r: number) {
    this.dirty.add(r);
  }
  private markAllDirty() {
    this.dirtyAll = true;
    for (let r = 0; r < this.rows; r++) this.dirty.add(r);
  }
}

function resizeGrid(prev: Cell[][], rows: number, cols: number): Cell[][] {
  const next: Cell[][] = new Array(rows);
  for (let r = 0; r < rows; r++) {
    const prevRow = prev[r];
    const row = blankRow(cols);
    if (prevRow) {
      const copyLen = Math.min(cols, prevRow.length);
      for (let c = 0; c < copyLen; c++) row[c] = prevRow[c];
    }
    next[r] = row;
  }
  return next;
}

function clamp(v: number, lo: number, hi: number): number {
  return v < lo ? lo : v > hi ? hi : v;
}
