import { Container, Graphics, Text, Sprite, Texture, type ContainerChild } from 'pixi.js';
import { ThoughtBubble } from './ThoughtBubble';

export type CharStatus = 'idle' | 'working' | 'waiting' | 'blocked';

const CHIP_H = 52; // 책상 칩에서 캐릭터 전신 높이(px)

// 캐릭터 도트 칩 — char-<slug>.png(단일 전신 누끼)가 있으면 Sprite 로, 없으면
// 색블록+이니셜 폴백. 상태 모션: working → 미세 좌우 흔들림, blocked → 머리 위 ⚠,
// waiting → 점멸 도트. 그 위에 생각 구름(ThoughtBubble)으로 board 활동을 띄운다.
export class ClassroomCharacter {
  readonly view = new Container();
  // 생각 구름은 view 안이 아니라 같은 부모 레이어에 별도로 얹고(절대좌표), setPos
  // 때 머리 위로 동기화한다(view 의 모션 흔들림에 안 휩쓸리게).
  readonly thought = new ThoughtBubble();
  private body?: Graphics;
  private initial?: Text;
  private sprite?: Sprite;
  private bubble: Text;
  private waitDot = new Graphics();
  private t = 0;
  private lastThought = '';
  private status: CharStatus = 'idle';

  constructor(public readonly id: string, public readonly name: string, color: number, spriteTex?: Texture) {
    if (spriteTex) {
      const sp = new Sprite(spriteTex);
      sp.anchor.set(0.5, 1); // 발 바닥 기준 — 책상 위에 선다
      sp.scale.set(CHIP_H / spriteTex.height);
      sp.y = 10;
      this.sprite = sp;
    } else {
      const body = new Graphics();
      body.roundRect(-9, -14, 18, 22, 2).fill(color).stroke({ width: 2, color: 0x1a1320 });
      this.body = body;
      const initial = new Text({
        text: (name || '?').trim().charAt(0).toUpperCase() || '?',
        style: { fontFamily: 'monospace', fontSize: 11, fill: 0x1a1320 }
      });
      initial.anchor.set(0.5);
      initial.y = -3;
      this.initial = initial;
    }

    this.bubble = new Text({ text: '⚠', style: { fontSize: 15, fill: 0xff6b6b } });
    this.bubble.anchor.set(0.5);
    this.bubble.y = -30;
    this.bubble.visible = false;

    this.waitDot.circle(0, -28, 3).fill(0x6c8ef5);
    this.waitDot.visible = false;

    const v = this.view as Container<ContainerChild>;
    if (this.sprite) v.addChild(this.sprite);
    if (this.body) v.addChild(this.body);
    if (this.initial) v.addChild(this.initial);
    v.addChild(this.bubble, this.waitDot);
    this.view.eventMode = 'static';
    this.view.cursor = 'pointer';
  }

  setStatus(s: CharStatus): void {
    this.status = s;
    this.bubble.visible = s === 'blocked';
    this.waitDot.visible = s === 'waiting';
  }

  /** 도트칩 이니셜 교체(색블록 폴백 전용 — sprite 모드는 외형이 이미지라 무시). */
  setInitial(s: string): void {
    if (!this.initial) return;
    const t = (s || '?').trim() || '?';
    this.initial.text = t;
    this.initial.style.fontSize = t.length > 1 ? 8 : 11;
  }

  /** 생각 구름 텍스트 갱신. 같은 텍스트면 무시(불필요 redraw 방지). */
  setThought(text: string): void {
    const t = (text || '').trim();
    if (t === this.lastThought) return;
    this.lastThought = t;
    if (t) this.thought.show(t);
    else if (!this.thought.isHidden()) this.thought.startLinger();
  }

  setBounds(w: number, h: number): void {
    this.thought.setBounds(w, h);
  }

  setPos(x: number, y: number): void {
    this.view.x = x;
    this.view.y = y;
    this.thought.setPosition(x, y);
  }

  /** dt = pixi ticker deltaTime(프레임 비례). working 미세 흔들림 + 구름 페이드. */
  tick(dt: number): void {
    this.t += dt;
    const moving = this.sprite ?? this.body;
    if (moving) moving.x = this.status === 'working' ? Math.sin(this.t * 0.35) * 1.5 : 0;
    if (this.status === 'waiting') {
      this.waitDot.alpha = 0.4 + 0.6 * (0.5 + 0.5 * Math.sin(this.t * 0.2));
    }
    this.thought.update(dt / 60);
  }

  destroy(): void {
    this.thought.destroy();
    this.view.destroy({ children: true });
  }
}
