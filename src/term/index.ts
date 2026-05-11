/**
 * 미니 터미널 facade — parser + buffer + renderer + input 묶음.
 * tmuxify 의 TerminalScreen 컴포넌트가 이 한 객체만 다루면 됨.
 */

import { AnsiParser } from "./parser";
import { ScreenBuffer } from "./buffer";
import { TerminalRenderer, type Palette16 } from "./render";
import { TerminalInput } from "./input";
import type { TerminalOp } from "./parser";

export type TerminalOptions = {
  palette: Palette16;
  rows?: number;
  cols?: number;
  onSend: (text: string) => void;
  onResize?: (cols: number, rows: number) => void;
};

export class MiniTerminal {
  readonly host: HTMLDivElement;
  private parser: AnsiParser;
  private buffer: ScreenBuffer;
  private renderer: TerminalRenderer;
  private input: TerminalInput;
  private appCursorKeys = false;
  private rafScheduled = false;
  private container: HTMLElement | null = null;
  private resizeObs: ResizeObserver | null = null;
  private onResize?: (cols: number, rows: number) => void;

  constructor(opts: TerminalOptions) {
    this.parser = new AnsiParser();
    this.buffer = new ScreenBuffer(opts.rows ?? 24, opts.cols ?? 80);
    this.renderer = new TerminalRenderer(this.buffer, opts.palette);
    this.input = new TerminalInput({
      onSend: opts.onSend,
      getAppCursorKeys: () => this.appCursorKeys,
    });
    this.onResize = opts.onResize;

    this.host = document.createElement("div");
    this.host.className = "tm-host-wrap";
    this.host.appendChild(this.renderer.host);
    this.input.attach(this.host);

    // cursor 위치 → IME textarea 위치 동기화
    this.renderer.onCursorMove = (x, y) => {
      this.input.textarea.style.left = `${x}px`;
      this.input.textarea.style.top = `${y}px`;
    };
  }

  mount(container: HTMLElement) {
    this.container = container;
    container.appendChild(this.host);
    this.renderer.mount(this.renderer.host.parentElement ?? this.host);
    this.fitToContainer();
    // 컨테이너 크기 변화 자동 추적
    this.resizeObs = new ResizeObserver(() => this.fitToContainer());
    this.resizeObs.observe(container);
    this.input.focus();
  }

  unmount() {
    this.resizeObs?.disconnect();
    this.resizeObs = null;
    this.host.remove();
    this.container = null;
  }

  /** tmux %output (UTF-8 디코딩된 문자열) 주입 */
  write(data: string) {
    const ops = this.parser.parse(data);
    if (import.meta.env.DEV) {
      const counts: Record<string, number> = {};
      for (const o of ops) counts[o.type] = (counts[o.type] ?? 0) + 1;
      console.log("[term.write]", data.length, "bytes →", counts);
    }
    this.applyOps(ops);
    this.scheduleRender();
  }

  /** 외부에서 직접 op 주입 (테스트용) */
  applyOps(ops: TerminalOp[]) {
    for (const op of ops) {
      if (op.type === "appCursorKeys") this.appCursorKeys = op.on;
    }
    this.buffer.apply(ops);
  }

  /** 팔레트 변경 (테마 스왑) */
  setPalette(p: Palette16) {
    this.renderer.setPalette(p);
  }

  /** 폰트 변경 시 재측정 + 컨테이너 크기 기준 cols/rows 재계산 */
  remeasure() {
    this.renderer.remeasure();
    this.fitToContainer();
  }

  focus() {
    this.input.focus();
  }

  size(): { cols: number; rows: number } {
    return { cols: this.buffer.cols, rows: this.buffer.rows };
  }

  private fitToContainer() {
    if (!this.container) return;
    const { cols, rows } = this.renderer.computeGridSize(this.container);
    if (cols !== this.buffer.cols || rows !== this.buffer.rows) {
      this.buffer.resize(rows, cols);
      this.renderer.syncRows();
      this.scheduleRender();
      this.onResize?.(cols, rows);
    }
  }

  private scheduleRender() {
    if (this.rafScheduled) return;
    this.rafScheduled = true;
    requestAnimationFrame(() => {
      this.rafScheduled = false;
      this.renderer.renderDirty();
    });
  }
}

export type { Palette16 };
