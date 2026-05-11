/**
 * 터미널 입력 — 진짜 <textarea> 사용해서 한글 IME 정상 동작.
 *
 * 동작:
 *  - textarea 는 host 위에 1x1px opacity:0 로 떠 있고, 항상 focus 유지.
 *  - compositionstart → 조합 시작. 이때부터 onInput 무시.
 *  - compositionend → 조합 완료된 텍스트를 한 번에 send.
 *  - 조합 중이 아닐 때 onInput 으로 들어온 단발성 텍스트(영문/숫자/기호)는 즉시 send.
 *  - 특수키(arrow, Enter, Backspace, Tab, Ctrl-X 등)는 keydown 에서 escape seq 로 매핑 후 send.
 *
 * onSend(text): UTF-8 인코딩 / hex 변환은 호출자가 책임.
 */

export type InputHandlerOptions = {
  onSend: (text: string) => void;
  /** applicationCursorKeys 모드 (DEC private 1) — claude/readline 이 사용 */
  getAppCursorKeys?: () => boolean;
};

export class TerminalInput {
  readonly textarea: HTMLTextAreaElement;
  private composing = false;
  private opts: InputHandlerOptions;

  constructor(opts: InputHandlerOptions) {
    this.opts = opts;
    const ta = document.createElement("textarea");
    ta.className = "tm-input";
    ta.autocapitalize = "off";
    ta.autocomplete = "off";
    ta.spellcheck = false;
    ta.setAttribute("aria-label", "Terminal input");
    ta.addEventListener("compositionstart", this.onCompStart);
    ta.addEventListener("compositionend", this.onCompEnd);
    ta.addEventListener("input", this.onInput);
    ta.addEventListener("keydown", this.onKeyDown);
    // blur 되어도 다시 focus — host 클릭/포커스 위임
    ta.addEventListener("blur", () => {
      // 약간 지연 후 refocus (사용자가 다른 곳 클릭한 게 아니라면)
      // 실용적으론 host 클릭 시 명시 focus 만으로 충분
    });
    this.textarea = ta;
  }

  attach(host: HTMLElement) {
    host.appendChild(this.textarea);
    // host 클릭 시 textarea 로 포커스 위임
    host.addEventListener("mousedown", (e) => {
      // 텍스트 선택은 host 의 일반 영역에서. textarea 로 포커스만 옮김.
      // 선택 후 복사 단축키는 textarea 가 받음.
      setTimeout(() => this.textarea.focus(), 0);
      void e;
    });
  }

  focus() {
    this.textarea.focus();
  }

  // ---- IME 핸들러 ----

  private onCompStart = () => {
    this.composing = true;
  };

  private onCompEnd = (e: Event) => {
    this.composing = false;
    const ce = e as CompositionEvent;
    if (ce.data) this.opts.onSend(ce.data);
    // textarea 안의 누적 값은 비워서 다음 입력에 영향 없게
    this.textarea.value = "";
  };

  // ---- 일반 input (영문/숫자/기호) ----

  private onInput = (e: Event) => {
    if (this.composing) return;
    const v = this.textarea.value;
    if (v.length > 0) {
      this.opts.onSend(v);
      this.textarea.value = "";
    }
    void e;
  };

  // ---- 특수키 / 단축키 ----

  private onKeyDown = (e: KeyboardEvent) => {
    if (this.composing) return;
    const seq = this.keyToSequence(e);
    if (seq !== null) {
      e.preventDefault();
      this.opts.onSend(seq);
    }
  };

  private keyToSequence(e: KeyboardEvent): string | null {
    const key = e.key;
    const ctrl = e.ctrlKey;
    const meta = e.metaKey;
    const alt = e.altKey;
    const appCursor = !!this.opts.getAppCursorKeys?.();

    // Cmd 단축키 — OS/앱 단축키. 가로채지 않음 (Cmd+C 복사 등).
    if (meta) return null;

    // Ctrl+letter → C0 (Ctrl+A=0x01 ... Ctrl+Z=0x1a)
    if (ctrl && key.length === 1) {
      const lower = key.toLowerCase();
      const code = lower.charCodeAt(0);
      if (code >= 0x61 && code <= 0x7a) {
        return String.fromCharCode(code - 0x60);
      }
      if (lower === "@" || lower === " ") return "\x00";
      if (lower === "[") return "\x1b";
      if (lower === "\\") return "\x1c";
      if (lower === "]") return "\x1d";
      if (lower === "^") return "\x1e";
      if (lower === "_") return "\x1f";
    }

    // Alt+letter → ESC + letter (meta key 모드)
    if (alt && key.length === 1 && !ctrl) {
      return "\x1b" + key;
    }

    switch (key) {
      case "Enter":
        return "\r";
      case "Backspace":
        return "\x7f"; // DEL — 대부분 셸이 backspace 로 인식
      case "Tab":
        return e.shiftKey ? "\x1b[Z" : "\t";
      case "Escape":
        return "\x1b";
      case "ArrowUp":
        return appCursor ? "\x1bOA" : "\x1b[A";
      case "ArrowDown":
        return appCursor ? "\x1bOB" : "\x1b[B";
      case "ArrowRight":
        return appCursor ? "\x1bOC" : "\x1b[C";
      case "ArrowLeft":
        return appCursor ? "\x1bOD" : "\x1b[D";
      case "Home":
        return appCursor ? "\x1bOH" : "\x1b[H";
      case "End":
        return appCursor ? "\x1bOF" : "\x1b[F";
      case "PageUp":
        return "\x1b[5~";
      case "PageDown":
        return "\x1b[6~";
      case "Insert":
        return "\x1b[2~";
      case "Delete":
        return "\x1b[3~";
      case "F1":
        return "\x1bOP";
      case "F2":
        return "\x1bOQ";
      case "F3":
        return "\x1bOR";
      case "F4":
        return "\x1bOS";
      case "F5":
        return "\x1b[15~";
      case "F6":
        return "\x1b[17~";
      case "F7":
        return "\x1b[18~";
      case "F8":
        return "\x1b[19~";
      case "F9":
        return "\x1b[20~";
      case "F10":
        return "\x1b[21~";
      case "F11":
        return "\x1b[23~";
      case "F12":
        return "\x1b[24~";
    }
    return null;
  }
}
