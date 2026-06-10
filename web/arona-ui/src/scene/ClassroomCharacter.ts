import { Container, Graphics, Text, Texture, Rectangle, AnimatedSprite, type ContainerChild } from 'pixi.js';
import { ThoughtBubble } from './ThoughtBubble';

export type CharStatus = 'idle' | 'working' | 'waiting' | 'blocked';

// 스프라이트 시트 규격(ART_REPLACEMENT_SPEC.md): 32px 프레임, 행=모션, 열=프레임.
const FRAME = 32;
const MOTION_ROW: Record<CharStatus, number> = { idle: 0, working: 1, waiting: 2, blocked: 3 };

// 캐릭터 도트 칩 — placeholder(색 블록 + 이니셜) 위에 상태 모션을 얹는다:
//   working → 책상 앞 타이핑(좌우 미세 흔들림), idle → 제자리,
//   blocked → 머리 위 ⚠ 글리프, waiting → 점멸 도트.
// 그 위에 "지금 뭐하는 중" 생각 구름(ThoughtBubble)을 얹어 board 활동을 띄운다.
// LimeZu 0. spriteTex(시트)가 주어지면 색블록 대신 AnimatedSprite 로 그리고
// setStatus 가 모션 행을 전환한다 — 없으면 색블록 폴백(view/setPos/setStatus 계약 유지).
export class ClassroomCharacter {
  readonly view = new Container();
  // 생각 구름은 캐릭터 view 안이 아니라 같은 부모 레이어에 별도로 얹고(절대좌표),
  // setPos 때 머리 위로 동기화한다(munder 패턴 — view 의 모션 흔들림에 안 휩쓸림).
  readonly thought = new ThoughtBubble();
  private body?: Graphics;
  private initial?: Text;
  private anim?: AnimatedSprite;
  private motion: Partial<Record<CharStatus, Texture[]>> = {};
  private bubble: Text;
  private waitDot = new Graphics();
  private t = 0;
  private lastThought = '';
  private status: CharStatus = 'idle';

  constructor(public readonly id: string, public readonly name: string, color: number, spriteTex?: Texture) {
    if (spriteTex) {
      this.buildMotion(spriteTex);
      this.anim = new AnimatedSprite(this.motion.idle ?? [spriteTex]);
      this.anim.anchor.set(0.5);
      this.anim.y = -3;
      this.anim.animationSpeed = 0.12;
      this.anim.play();
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
    this.bubble.y = -26;
    this.bubble.visible = false;

    this.waitDot.circle(0, -24, 3).fill(0x6c8ef5);
    this.waitDot.visible = false;

    // pixi v8: 자식 추가 — sprite 모드면 anim, 아니면 색블록+이니셜.
    const v = this.view as Container<ContainerChild>;
    if (this.anim) v.addChild(this.anim);
    if (this.body) v.addChild(this.body);
    if (this.initial) v.addChild(this.initial);
    v.addChild(this.bubble, this.waitDot);
    this.view.eventMode = 'static';
    this.view.cursor = 'pointer';
  }

  // 시트를 32px 격자로 잘라 모션 행별 프레임 배열을 만든다. 행이 부족하면 그 모션은
  // 비워 두고 setStatus 가 idle 로 폴백한다.
  private buildMotion(tex: Texture): void {
    const cols = Math.max(1, Math.floor(tex.width / FRAME));
    const rows = Math.max(1, Math.floor(tex.height / FRAME));
    for (const [st, row] of Object.entries(MOTION_ROW) as [CharStatus, number][]) {
      if (row >= rows) continue;
      const frames: Texture[] = [];
      for (let c = 0; c < cols; c++) {
        frames.push(new Texture({ source: tex.source, frame: new Rectangle(c * FRAME, row * FRAME, FRAME, FRAME) }));
      }
      this.motion[st] = frames;
    }
  }

  setStatus(s: CharStatus): void {
    this.status = s;
    this.bubble.visible = s === 'blocked';
    this.waitDot.visible = s === 'waiting';
    if (this.anim) {
      const frames = this.motion[s] ?? this.motion.idle;
      if (frames && this.anim.textures !== frames) {
        this.anim.textures = frames;
        this.anim.play();
      }
    }
  }

  /** 도트칩 이니셜 교체 — 같은 첫글자 캐릭터가 공존하면 2글자(아로/아리)로
   *  구분, 유일하면 1글자. 2글자는 18px 박스에 맞게 폰트를 줄인다. sprite 모드는
   *  외형이 시트라 이니셜이 없다(무시). */
  setInitial(s: string): void {
    if (!this.initial) return;
    const t = (s || '?').trim() || '?';
    this.initial.text = t;
    this.initial.style.fontSize = t.length > 1 ? 8 : 11;
  }

  /** 생각 구름 텍스트 갱신. 같은 텍스트면 무시(불필요 redraw 방지). 빈 문자열 →
   *  잠깐 머문 뒤 사라짐(idle 로 조용해진 경우). */
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

  /** dt = pixi ticker deltaTime(프레임 비례). 상태별 미세 모션 + 구름 페이드.
   *  sprite 모드는 AnimatedSprite 가 모션을 표현하므로 색블록 흔들림은 생략. */
  tick(dt: number): void {
    this.t += dt;
    if (this.body) {
      this.body.x = this.status === 'working' ? Math.sin(this.t * 0.35) * 1.5 : 0;
    }
    if (this.status === 'waiting') {
      this.waitDot.alpha = 0.4 + 0.6 * (0.5 + 0.5 * Math.sin(this.t * 0.2)); // 점멸
    }
    // ThoughtBubble 은 초 단위(dt/60)로 페이드 — munder 와 같은 시간 척도.
    this.thought.update(dt / 60);
  }

  destroy(): void {
    this.thought.destroy();
    this.view.destroy({ children: true });
  }
}
