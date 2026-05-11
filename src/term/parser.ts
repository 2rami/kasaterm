/**
 * 미니 ANSI 파서 — claude UI 가 쓰는 시퀀스 위주의 subset.
 *
 * 입력: 바이트 스트림(string, UTF-8 디코딩 완료된 상태)
 * 출력: TerminalOp 시퀀스 (buffer 가 apply 할 의미 단위)
 *
 * VT100 state machine 의 단순화 버전:
 *   GROUND → ESC → CSI/OSC → 파라미터/intermediates/final
 *
 * 지원 범위:
 *  - print: 일반 문자 + 와이드(CJK)
 *  - C0: \r \n \t \b BEL
 *  - CSI: CUP/CUU/CUD/CUF/CUB/EL/ED/SGR/DECSET/DECRST/SM/RM/CHA/HVP/SU/SD/IL/DL/ICH/DCH/ECH
 *  - OSC: 0/2 (window title — 일단 drop)
 *  - 단순 ESC: c (RIS), 7/8 (save/restore cursor), D/E/M, H (tab set), Z
 */

export type SgrAttr = {
  bold?: boolean;
  faint?: boolean;
  italic?: boolean;
  underline?: boolean;
  blink?: boolean;
  inverse?: boolean;
  hidden?: boolean;
  strike?: boolean;
  /** null=default, [0..15]=palette index, [16..255]=256색, "#rrggbb"=truecolor */
  fg?: number | string | null;
  bg?: number | string | null;
};

export type TerminalOp =
  | { type: "print"; text: string }
  | { type: "lf" } // \n
  | { type: "cr" } // \r
  | { type: "bs" } // \b
  | { type: "tab" }
  | { type: "bell" }
  /** cursor absolute position (1-based in escapes, we normalize to 0-based) */
  | { type: "cup"; row: number; col: number }
  | { type: "cuu"; n: number }
  | { type: "cud"; n: number }
  | { type: "cuf"; n: number }
  | { type: "cub"; n: number }
  /** cursor horizontal absolute (column only) */
  | { type: "cha"; col: number }
  /** scroll up/down N lines within scroll region */
  | { type: "su"; n: number }
  | { type: "sd"; n: number }
  /** insert/delete lines/chars */
  | { type: "il"; n: number }
  | { type: "dl"; n: number }
  | { type: "ich"; n: number }
  | { type: "dch"; n: number }
  /** erase character (overwrite with space, preserves cursor) */
  | { type: "ech"; n: number }
  /** Erase In Line: 0=cursor→end, 1=start→cursor, 2=whole line */
  | { type: "el"; mode: 0 | 1 | 2 }
  /** Erase In Display: 0=cursor→end, 1=start→cursor, 2=whole screen, 3=scrollback */
  | { type: "ed"; mode: 0 | 1 | 2 | 3 }
  /** SGR — 누적해서 현재 attr 갱신 */
  | { type: "sgr"; attrs: SgrAttr; reset?: boolean }
  /** alternate screen on/off + 옵션 save/restore cursor */
  | { type: "altScreen"; on: boolean; saveCursor: boolean }
  /** cursor visibility */
  | { type: "cursorVisible"; visible: boolean }
  /** bracketed paste mode on/off */
  | { type: "bracketedPaste"; on: boolean }
  /** application cursor keys mode (claude/readline 가 씀) */
  | { type: "appCursorKeys"; on: boolean }
  /** save / restore cursor (DECSC/DECRC, ESC 7 / ESC 8) */
  | { type: "saveCursor" }
  | { type: "restoreCursor" }
  /** full reset (ESC c) */
  | { type: "reset" }
  /** OSC — 일단 윈도우 타이틀만 */
  | { type: "title"; text: string }
  /** 알 수 없는 시퀀스 — debug 용으로 raw 보관 */
  | { type: "unknown"; raw: string };

type State =
  | "ground"
  | "esc"
  | "csi-entry"
  | "csi-param"
  | "csi-intermediate"
  | "osc";

export class AnsiParser {
  private state: State = "ground";
  /** 누적된 출력 문자 — print op 묶기 위한 버퍼 */
  private printBuf = "";
  /** CSI parameter 수집용. ';' 로 분리된 숫자들. -1 = 빈 자리(default) */
  private params: number[] = [];
  /** CSI param 현재 토큰 빌더 */
  private paramCur = -1;
  /** CSI ? prefix 등 private 마커 */
  private privateMark = "";
  /** intermediate 바이트들 (보통 안 씀) */
  private intermediate = "";
  /** OSC 페이로드 */
  private oscBuf = "";

  /** 청크 입력 → op 배열. 상태는 유지되므로 멀티-청크 안전. */
  parse(input: string): TerminalOp[] {
    const ops: TerminalOp[] = [];
    for (let i = 0; i < input.length; i++) {
      const ch = input[i];
      const code = ch.charCodeAt(0);
      this.handle(ch, code, ops);
    }
    this.flushPrint(ops);
    return ops;
  }

  private handle(ch: string, code: number, ops: TerminalOp[]) {
    switch (this.state) {
      case "ground":
        this.handleGround(ch, code, ops);
        return;
      case "esc":
        this.handleEsc(ch, ops);
        return;
      case "csi-entry":
      case "csi-param":
      case "csi-intermediate":
        this.handleCsi(ch, code, ops);
        return;
      case "osc":
        this.handleOsc(ch, code, ops);
        return;
    }
  }

  private handleGround(ch: string, code: number, ops: TerminalOp[]) {
    if (code === 0x1b) {
      // ESC
      this.flushPrint(ops);
      this.state = "esc";
      return;
    }
    if (code < 0x20 || code === 0x7f) {
      this.flushPrint(ops);
      this.handleC0(code, ops);
      return;
    }
    this.printBuf += ch;
  }

  private handleC0(code: number, ops: TerminalOp[]) {
    switch (code) {
      case 0x07:
        ops.push({ type: "bell" });
        return;
      case 0x08:
        ops.push({ type: "bs" });
        return;
      case 0x09:
        ops.push({ type: "tab" });
        return;
      case 0x0a:
      case 0x0b:
      case 0x0c:
        ops.push({ type: "lf" });
        return;
      case 0x0d:
        ops.push({ type: "cr" });
        return;
      default:
        // ignore
        return;
    }
  }

  private handleEsc(ch: string, ops: TerminalOp[]) {
    switch (ch) {
      case "[":
        this.resetCsiState();
        this.state = "csi-entry";
        return;
      case "]":
        this.oscBuf = "";
        this.state = "osc";
        return;
      case "c":
        ops.push({ type: "reset" });
        this.state = "ground";
        return;
      case "7":
        ops.push({ type: "saveCursor" });
        this.state = "ground";
        return;
      case "8":
        ops.push({ type: "restoreCursor" });
        this.state = "ground";
        return;
      case "D":
        // IND (index) — like LF without CR
        ops.push({ type: "lf" });
        this.state = "ground";
        return;
      case "E":
        // NEL — newline
        ops.push({ type: "cr" });
        ops.push({ type: "lf" });
        this.state = "ground";
        return;
      case "M":
        // RI (reverse index) — scroll down 1 if at top
        ops.push({ type: "sd", n: 1 });
        this.state = "ground";
        return;
      default:
        // 기타는 일단 무시
        this.state = "ground";
        return;
    }
  }

  private resetCsiState() {
    this.params = [];
    this.paramCur = -1;
    this.privateMark = "";
    this.intermediate = "";
  }

  private handleCsi(ch: string, code: number, ops: TerminalOp[]) {
    // 0x30-0x39 = '0'-'9', 0x3b = ';'
    if (code >= 0x30 && code <= 0x39) {
      if (this.paramCur < 0) this.paramCur = 0;
      this.paramCur = this.paramCur * 10 + (code - 0x30);
      this.state = "csi-param";
      return;
    }
    if (ch === ";") {
      this.params.push(this.paramCur);
      this.paramCur = -1;
      this.state = "csi-param";
      return;
    }
    // private marker (?, >, =, !) 이 첫 위치에만
    if ((ch === "?" || ch === ">" || ch === "=" || ch === "!") && this.state === "csi-entry") {
      this.privateMark = ch;
      return;
    }
    // intermediate 0x20-0x2f
    if (code >= 0x20 && code <= 0x2f) {
      this.intermediate += ch;
      this.state = "csi-intermediate";
      return;
    }
    // final 0x40-0x7e
    if (code >= 0x40 && code <= 0x7e) {
      // 마지막 param flush
      this.params.push(this.paramCur);
      this.dispatchCsi(ch, ops);
      this.state = "ground";
      return;
    }
    // 그 외는 abort
    this.state = "ground";
  }

  private p(i: number, def: number): number {
    const v = this.params[i];
    if (v === undefined || v < 0) return def;
    return v;
  }

  private dispatchCsi(final: string, ops: TerminalOp[]) {
    const isPrivate = this.privateMark === "?";

    switch (final) {
      case "A":
        ops.push({ type: "cuu", n: this.p(0, 1) });
        return;
      case "B":
      case "e":
        ops.push({ type: "cud", n: this.p(0, 1) });
        return;
      case "C":
      case "a":
        ops.push({ type: "cuf", n: this.p(0, 1) });
        return;
      case "D":
        ops.push({ type: "cub", n: this.p(0, 1) });
        return;
      case "G":
      case "`":
        ops.push({ type: "cha", col: this.p(0, 1) - 1 });
        return;
      case "H":
      case "f":
        ops.push({
          type: "cup",
          row: this.p(0, 1) - 1,
          col: this.p(1, 1) - 1,
        });
        return;
      case "J": {
        const m = this.p(0, 0);
        ops.push({ type: "ed", mode: (m as 0 | 1 | 2 | 3) ?? 0 });
        return;
      }
      case "K": {
        const m = this.p(0, 0);
        ops.push({ type: "el", mode: (m as 0 | 1 | 2) ?? 0 });
        return;
      }
      case "L":
        ops.push({ type: "il", n: this.p(0, 1) });
        return;
      case "M":
        ops.push({ type: "dl", n: this.p(0, 1) });
        return;
      case "@":
        ops.push({ type: "ich", n: this.p(0, 1) });
        return;
      case "P":
        ops.push({ type: "dch", n: this.p(0, 1) });
        return;
      case "X":
        ops.push({ type: "ech", n: this.p(0, 1) });
        return;
      case "S":
        ops.push({ type: "su", n: this.p(0, 1) });
        return;
      case "T":
        ops.push({ type: "sd", n: this.p(0, 1) });
        return;
      case "m":
        ops.push(this.parseSgr());
        return;
      case "h":
      case "l": {
        const on = final === "h";
        if (isPrivate) {
          this.dispatchDecPrivate(this.params, on, ops);
        } else {
          // SM/RM — 일단 무시
        }
        return;
      }
      case "s":
        ops.push({ type: "saveCursor" });
        return;
      case "u":
        ops.push({ type: "restoreCursor" });
        return;
      case "n":
      case "c":
      case "t":
        // device status / attr / window manip — 답신 필요하지만 일단 drop
        return;
      default:
        ops.push({
          type: "unknown",
          raw: `CSI ${this.privateMark}${this.params.join(";")} ${final}`,
        });
        return;
    }
  }

  private dispatchDecPrivate(params: number[], on: boolean, ops: TerminalOp[]) {
    for (const p of params) {
      switch (p) {
        case 1:
          ops.push({ type: "appCursorKeys", on });
          break;
        case 25:
          ops.push({ type: "cursorVisible", visible: on });
          break;
        case 1049:
        case 1047:
          ops.push({ type: "altScreen", on, saveCursor: p === 1049 });
          break;
        case 1048:
          ops.push(on ? { type: "saveCursor" } : { type: "restoreCursor" });
          break;
        case 2004:
          ops.push({ type: "bracketedPaste", on });
          break;
        // 기타 DEC private 들은 무시
      }
    }
  }

  private parseSgr(): TerminalOp {
    // params 가 비어있거나 [0] 이면 reset
    if (this.params.length === 0 || (this.params.length === 1 && this.p(0, 0) === 0)) {
      return { type: "sgr", attrs: {}, reset: true };
    }
    const attrs: SgrAttr = {};
    let i = 0;
    while (i < this.params.length) {
      const v = this.p(i, 0);
      switch (true) {
        case v === 0:
          // reset 누적
          return { type: "sgr", attrs: {}, reset: true };
        case v === 1:
          attrs.bold = true;
          break;
        case v === 2:
          attrs.faint = true;
          break;
        case v === 3:
          attrs.italic = true;
          break;
        case v === 4:
          attrs.underline = true;
          break;
        case v === 5:
        case v === 6:
          attrs.blink = true;
          break;
        case v === 7:
          attrs.inverse = true;
          break;
        case v === 8:
          attrs.hidden = true;
          break;
        case v === 9:
          attrs.strike = true;
          break;
        case v === 22:
          attrs.bold = false;
          attrs.faint = false;
          break;
        case v === 23:
          attrs.italic = false;
          break;
        case v === 24:
          attrs.underline = false;
          break;
        case v === 25:
          attrs.blink = false;
          break;
        case v === 27:
          attrs.inverse = false;
          break;
        case v === 28:
          attrs.hidden = false;
          break;
        case v === 29:
          attrs.strike = false;
          break;
        case v >= 30 && v <= 37:
          attrs.fg = v - 30;
          break;
        case v === 38:
          {
            const next = this.p(i + 1, 0);
            if (next === 5) {
              attrs.fg = this.p(i + 2, 0);
              i += 2;
            } else if (next === 2) {
              const r = this.p(i + 2, 0);
              const g = this.p(i + 3, 0);
              const b = this.p(i + 4, 0);
              attrs.fg = `#${hex2(r)}${hex2(g)}${hex2(b)}`;
              i += 4;
            }
          }
          break;
        case v === 39:
          attrs.fg = null;
          break;
        case v >= 40 && v <= 47:
          attrs.bg = v - 40;
          break;
        case v === 48:
          {
            const next = this.p(i + 1, 0);
            if (next === 5) {
              attrs.bg = this.p(i + 2, 0);
              i += 2;
            } else if (next === 2) {
              const r = this.p(i + 2, 0);
              const g = this.p(i + 3, 0);
              const b = this.p(i + 4, 0);
              attrs.bg = `#${hex2(r)}${hex2(g)}${hex2(b)}`;
              i += 4;
            }
          }
          break;
        case v === 49:
          attrs.bg = null;
          break;
        case v >= 90 && v <= 97:
          attrs.fg = v - 90 + 8;
          break;
        case v >= 100 && v <= 107:
          attrs.bg = v - 100 + 8;
          break;
      }
      i++;
    }
    return { type: "sgr", attrs };
  }

  private handleOsc(ch: string, code: number, ops: TerminalOp[]) {
    // ST = ESC \, or BEL terminates
    if (code === 0x07) {
      this.dispatchOsc(ops);
      this.state = "ground";
      return;
    }
    if (code === 0x1b) {
      // 다음 char 가 \ 면 ST. 단순화: ESC 한 글자만으로도 종료 처리
      this.dispatchOsc(ops);
      this.state = "esc";
      this.oscBuf = "";
      return;
    }
    this.oscBuf += ch;
  }

  private dispatchOsc(ops: TerminalOp[]) {
    // "<n>;<text>"
    const semi = this.oscBuf.indexOf(";");
    if (semi < 0) return;
    const num = this.oscBuf.slice(0, semi);
    const text = this.oscBuf.slice(semi + 1);
    if (num === "0" || num === "2" || num === "1") {
      ops.push({ type: "title", text });
    }
  }

  private flushPrint(ops: TerminalOp[]) {
    if (this.printBuf) {
      ops.push({ type: "print", text: this.printBuf });
      this.printBuf = "";
    }
  }
}

function hex2(n: number): string {
  return (n & 0xff).toString(16).padStart(2, "0");
}
