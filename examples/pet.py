#!/usr/bin/env python3
"""
도트 펫 캐릭터 — 치즈 고양이(나쵸네코 모티프). v3.
1층 이스터에그용. 반블록(▀) 픽셀 아트 + 프레임 교체 애니메이션.

좌우 대칭은 코드로 보장한다: 왼쪽 절반(10칸)만 그리고 거울 반사 → 20칸.
큰 눈 + 흰 하이라이트 + 볼터치로 귀여움을 살린다.
"""
import sys, time

ESC = "\033["
def fg(c): r, g, b = PAL[c]; return f"{ESC}38;2;{r};{g};{b}m"
def bg(c): r, g, b = PAL[c]; return f"{ESC}48;2;{r};{g};{b}m"
RESET = f"{ESC}0m"
HIDE, SHOW = f"{ESC}?25l", f"{ESC}?25h"
def up(n): return f"{ESC}{n}A"

PAL = {
    "O": (242, 145, 54),    # 치즈 주황 (몸)
    "o": (205, 102, 32),    # 진한 줄무늬
    "W": (255, 246, 230),   # 크림 (얼굴·배)
    "P": (255, 150, 170),   # 핑크 (코·귀안)
    "B": (54, 46, 56),      # 눈동자
    "H": (255, 255, 255),   # 눈 하이라이트
    "p": (255, 188, 200),   # 볼터치
    ".": None,
}

def mirror(half):
    """왼쪽 10칸 → 20칸 (거울 반사). 대칭 보장."""
    return half + half[::-1]

# 머리~얼굴: 왼쪽 절반(10칸). 눈만 open/blink 로 교체. 귀는 머리 양 끝으로.
def head(eyes):
    eye_rows = {
        "open":  ["OOOOBBBHOO",   # 검은자(col4-6) + 하이라이트(안쪽 col7)
                  "OOOOBBBBOO",   # 검은자(col4-7)
                  "OOOOOBBOOO"],  # 눈 아래
        "blink": ["OOOOOOOOOO",
                  "OOOOooooOO",   # 감은 줄
                  "OOOOOOOOOO"],
    }[eyes]
    half = [
        ".O........",   # r0 귀 끝
        ".OO.......",   # r1 귀
        "OOOO...OOO",   # r2 귀 + 머리 돔(가운데) — 귀 사이를 채움
        "OPOOOOOOOO",   # r3 귀안 핑크 + 머리
        "OOOOOOOOOO",   # r4 머리
        "OOOOOOOOOO",   # r5 (가장 넓음)
        eye_rows[0],    # r6 눈 위
        eye_rows[1],    # r7 눈
        eye_rows[2],    # r8 눈 아래
        "OOppOOOOOP",   # r9 볼터치(col2-3) + 코(중앙 col9)
        "OOOWWWWWWW",   # r10 크림 얼굴 (둥글게)
        "OOOOWWWWWW",   # r11 턱
    ]
    return [mirror(r) for r in half]

# 몸+꼬리: 왼쪽 절반. 꼬리는 오른쪽이라 미러 후 직접 덧그린다.
def body(tail):
    half = [
        "OOOWWWWOOO",   # r12 가슴(가운데 크림)
        ".OOOOOOOOO",   # r13 몸
        ".OOOOOOOOO",   # r14 몸 아래
        "..OOOOOOOO",   # r15 발 경계
        "....OOOO..",   # r16 앞발 (가운데 두 개, 깔끔)
    ]
    rows = [mirror(r) for r in half]
    # 꼬리를 몸 오른쪽에 덧그림 (20칸). up=끝을 들고, down=바닥에 눕힘
    if tail == "up":
        rows[1] = rows[1][:15] + "ooo" + rows[1][18:]   # r13 꼬리 중간
        rows[0] = rows[0][:16] + "oo" + rows[0][18:]    # r12 꼬리 끝(위)
    else:  # down
        rows[1] = rows[1][:15] + "oo" + rows[1][17:]    # r13 꼬리 시작
        rows[2] = rows[2][:15] + "ooo" + rows[2][18:]   # r14 꼬리 눕힘
    return rows

def cat(eyes, tail):
    return head(eyes) + body(tail)

def render(grid):
    out = []
    for y in range(0, len(grid), 2):
        top = grid[y]
        bot = grid[y + 1] if y + 1 < len(grid) else "." * len(top)
        line = "   "
        for x in range(max(len(top), len(bot))):
            t = top[x] if x < len(top) else "."
            b = bot[x] if x < len(bot) else "."
            if t != "." and b != ".":
                line += fg(t) + bg(b) + "▀" + RESET
            elif t != ".":
                line += fg(t) + "▀" + RESET
            elif b != ".":
                line += fg(b) + "▄" + RESET
            else:
                line += " "
        out.append(line)
    return out

FRAMES = [
    ("open", "up"), ("open", "up"), ("open", "down"), ("open", "down"),
    ("open", "up"), ("blink", "up"), ("open", "down"),
]

BUBBLE = [
    "  ╭───────────────────────────╮",
    "  │  냐옹~ 이스터에그 발견! 🥚  │",
    "  ╰──┬────────────────────────╯",
    "     │",
]

def main():
    loops = 3
    if len(sys.argv) > 1 and sys.argv[1].isdigit():
        loops = int(sys.argv[1])
    sys.stdout.write(HIDE)
    print("\n".join(fg("W") + b + RESET for b in BUBBLE))
    height = (len(cat("open", "up")) + 1) // 2
    first = True
    try:
        for _ in range(loops):
            for eyes, tail in FRAMES:
                lines = render(cat(eyes, tail))
                if not first:
                    sys.stdout.write(up(height))
                first = False
                sys.stdout.write("\n".join(lines) + "\n")
                sys.stdout.flush()
                time.sleep(0.18)
    finally:
        sys.stdout.write(SHOW + RESET)
        sys.stdout.flush()

if __name__ == "__main__":
    main()
