import { Container, Graphics, Text, type ContainerChild } from 'pixi.js';

export type CharStatus = 'idle' | 'working' | 'waiting' | 'blocked';

// 캐릭터 도트 칩 — placeholder(색 블록 + 이니셜) 위에 상태 모션을 얹는다:
//   working → 책상 앞 타이핑(좌우 미세 흔들림), idle → 제자리,
//   blocked → 머리 위 ⚠ 말풍선, waiting → 점멸 도트.
// LimeZu 0. characters.json 의 sprite 필드(시트 경로)가 채워지면 이 클래스의
// body 를 스프라이트 애니로 교체하면 된다(view/setPos/setStatus 계약 유지).
export class ClassroomCharacter {
  readonly view = new Container();
  private body = new Graphics();
  private bubble: Text;
  private waitDot = new Graphics();
  private t = 0;
  private status: CharStatus = 'idle';

  constructor(public readonly id: string, public readonly name: string, color: number) {
    this.body.roundRect(-9, -14, 18, 22, 2).fill(color).stroke({ width: 2, color: 0x1a1320 });

    const initial = new Text({
      text: (name || '?').trim().charAt(0).toUpperCase() || '?',
      style: { fontFamily: 'monospace', fontSize: 11, fill: 0x1a1320 }
    });
    initial.anchor.set(0.5);
    initial.y = -3;

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

  setPos(x: number, y: number): void {
    this.view.x = x;
    this.view.y = y;
  }

  /** dt = pixi ticker deltaTime(프레임 비례). 상태별 미세 모션. */
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
  }

  destroy(): void {
    this.view.destroy({ children: true });
  }
}
