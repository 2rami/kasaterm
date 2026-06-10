import { Container, Graphics, Text, type ContainerChild } from 'pixi.js';
import { ThoughtBubble } from './ThoughtBubble';

export type CharStatus = 'idle' | 'working' | 'waiting' | 'blocked';

// 캐릭터 도트 칩 — placeholder(색 블록 + 이니셜) 위에 상태 모션을 얹는다:
//   working → 책상 앞 타이핑(좌우 미세 흔들림), idle → 제자리,
//   blocked → 머리 위 ⚠ 글리프, waiting → 점멸 도트.
// 그 위에 "지금 뭐하는 중" 생각 구름(ThoughtBubble)을 얹어 board 활동을 띄운다.
// LimeZu 0. characters.json 의 sprite 필드(시트 경로)가 채워지면 이 클래스의
// body 를 스프라이트 애니로 교체하면 된다(view/setPos/setStatus 계약 유지).
export class ClassroomCharacter {
  readonly view = new Container();
  // 생각 구름은 캐릭터 view 안이 아니라 같은 부모 레이어에 별도로 얹고(절대좌표),
  // setPos 때 머리 위로 동기화한다(munder 패턴 — view 의 모션 흔들림에 안 휩쓸림).
  readonly thought = new ThoughtBubble();
  private body = new Graphics();
  private initial: Text;
  private bubble: Text;
  private waitDot = new Graphics();
  private t = 0;
  private lastThought = '';
  private status: CharStatus = 'idle';

  constructor(public readonly id: string, public readonly name: string, color: number) {
    this.body.roundRect(-9, -14, 18, 22, 2).fill(color).stroke({ width: 2, color: 0x1a1320 });

    const initial = new Text({
      text: (name || '?').trim().charAt(0).toUpperCase() || '?',
      style: { fontFamily: 'monospace', fontSize: 11, fill: 0x1a1320 }
    });
    initial.anchor.set(0.5);
    initial.y = -3;
    this.initial = initial;

    this.bubble = new Text({ text: '⚠', style: { fontSize: 15, fill: 0xff6b6b } });
    this.bubble.anchor.set(0.5);
    this.bubble.y = -26;
    this.bubble.visible = false;

    this.waitDot.circle(0, -24, 3).fill(0x6c8ef5);
    this.waitDot.visible = false;

    // pixi v8: 자식 추가
    (this.view as Container<ContainerChild>).addChild(this.body, initial, this.bubble, this.waitDot);
    this.view.eventMode = 'static';
    this.view.cursor = 'pointer';
  }

  setStatus(s: CharStatus): void {
    this.status = s;
    this.bubble.visible = s === 'blocked';
    this.waitDot.visible = s === 'waiting';
  }

  /** 도트칩 이니셜 교체 — 같은 첫글자 캐릭터가 공존하면 2글자(아로/아리)로
   *  구분, 유일하면 1글자. 2글자는 18px 박스에 맞게 폰트를 줄인다. */
  setInitial(s: string): void {
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

  /** dt = pixi ticker deltaTime(프레임 비례). 상태별 미세 모션 + 구름 페이드. */
  tick(dt: number): void {
    this.t += dt;
    if (this.status === 'working') {
      this.body.x = Math.sin(this.t * 0.35) * 1.5; // 타이핑 흔들림
    } else {
      this.body.x = 0;
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
