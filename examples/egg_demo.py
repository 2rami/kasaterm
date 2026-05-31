#!/usr/bin/env python3
"""
터미널 이스터에그 기법 데모 (1층 = ANSI/문자 출력).
외부 툴 없이 순수 ANSI 이스케이프 코드만 사용한다.

핵심 원리: 터미널은 "\033[" 로 시작하는 특수 문자열(이스케이프 시퀀스)을
글자가 아니라 '명령'으로 해석한다. 색을 바꾸고, 커서를 옮기고, 화면을 지운다.
이 명령들을 글자 사이에 끼워 넣으면 그림과 애니메이션이 된다.
"""
import sys, time, math, random, shutil

ESC = "\033["
def rgb(r, g, b):            # 글자색을 RGB(트루컬러)로
    return f"{ESC}38;2;{r};{g};{b}m"
def bg(r, g, b):             # 배경색을 RGB로
    return f"{ESC}48;2;{r};{g};{b}m"
RESET = f"{ESC}0m"
HIDE  = f"{ESC}?25l"        # 커서 숨김
SHOW  = f"{ESC}?25h"        # 커서 보임
CLEAR = f"{ESC}2J{ESC}H"    # 화면 지우고 좌상단으로
def at(row, col):           # 커서를 (행,열)로 이동
    return f"{ESC}{row};{col}H"

W = shutil.get_terminal_size((80, 24)).columns


# ───────────────────────────── 1. ASCII 배너 ─────────────────────────────
# 글자를 큰 도트 패턴으로 손수 그린 것. figlet 이 자동으로 해주는 일이다.
BANNER = r"""
 _  __    _    ____    _
| |/ /   / \  / ___|  / \
| ' /   / _ \ \___ \ / _ \
| . \  / ___ \ ___) / ___ \
|_|\_\/_/   \_\____/_/   \_\
"""

def show_banner():
    for i, line in enumerate(BANNER.strip("\n").split("\n")):
        # 줄마다 색을 조금씩 바꿔 무지개 그라데이션
        r = int(127 + 127 * math.sin(i * 0.6 + 0))
        g = int(127 + 127 * math.sin(i * 0.6 + 2))
        b = int(127 + 127 * math.sin(i * 0.6 + 4))
        print(rgb(r, g, b) + line + RESET)
    print()


# ────────────────────── 2. 트루컬러 그라데이션 바 ──────────────────────
def show_gradient():
    print("  트루컬러 그라데이션 (한 칸 = 배경색 블록):")
    print("  ", end="")
    for i in range(W - 6):
        t = i / (W - 6)
        r = int(255 * t)
        g = int(128 + 100 * math.sin(t * math.pi))
        b = int(255 * (1 - t))
        print(bg(r, g, b) + " " + RESET, end="")
    print("\n")


# ───────────────── 3. 반블록(▀) 픽셀 아트 = 도트 디자인 ─────────────────
# ▀ 문자는 글자칸의 '위 절반'만 칠한다. 글자색=위 픽셀, 배경색=아래 픽셀.
# 즉 글자 1칸으로 세로 2픽셀을 표현 → 정사각형에 가까운 도트를 찍는다.
HEART = [
    "  ........  ",
    " ..######.. ",
    ".##########.",
    ".##########.",
    " .########. ",
    "  .######.  ",
    "   .####.   ",
    "    .##.    ",
    "     ..     ",
]
def palette(c):
    return {"#": (235, 64, 52), ".": (40, 40, 50), " ": None}[c]

def show_pixel_art():
    print("  반블록 픽셀 아트 (글자 1칸 = 세로 2픽셀):")
    # 두 줄씩 묶어 위/아래 픽셀로 합친다
    for y in range(0, len(HEART), 2):
        top = HEART[y]
        bot = HEART[y + 1] if y + 1 < len(HEART) else " " * len(top)
        print("  ", end="")
        for x in range(len(top)):
            up = palette(top[x])
            dn = palette(bot[x]) if x < len(bot) else None
            if up and dn:
                print(rgb(*up) + bg(*dn) + "▀" + RESET, end="")
            elif up:
                print(rgb(*up) + "▀" + RESET, end="")
            elif dn:
                print(rgb(*dn) + "▄" + RESET, end="")
            else:
                print(" ", end="")
        print()
    print()


# ─────────────────────── 4. 매트릭스 비 (애니메이션) ───────────────────────
def matrix_rain(seconds=4):
    cols = W
    drops = [random.randint(-20, 0) for _ in range(cols)]
    chars = "ｱｲｳｴｵｶｷｸ01<>+*=#@$%&"
    rows = shutil.get_terminal_size((80, 24)).lines
    sys.stdout.write(HIDE + CLEAR)
    end = time.time() + seconds
    while time.time() < end:
        for c in range(cols):
            r = drops[c]
            if 0 <= r < rows:
                ch = random.choice(chars)
                sys.stdout.write(at(r + 1, c + 1) + rgb(180, 255, 180) + ch)  # 머리(밝음)
                if r - 1 >= 0:
                    sys.stdout.write(at(r, c + 1) + rgb(0, 180, 0) + random.choice(chars))  # 꼬리(어두움)
                if r - 8 >= 0:
                    sys.stdout.write(at(r - 8, c + 1) + " ")  # 오래된 건 지움
            drops[c] = r + 1 if r < rows + 8 else random.randint(-20, 0)
        sys.stdout.write(RESET)
        sys.stdout.flush()
        time.sleep(0.06)
    sys.stdout.write(CLEAR + SHOW + RESET)
    sys.stdout.flush()


def main():
    args = sys.argv[1:]
    if "rain" in args:
        matrix_rain()
        return
    print(CLEAR)
    show_banner()
    show_gradient()
    show_pixel_art()
    print(rgb(150, 150, 160) +
          "  팁: `python3 egg_demo.py rain` 으로 매트릭스 비 애니메이션 실행" +
          RESET)

if __name__ == "__main__":
    main()
