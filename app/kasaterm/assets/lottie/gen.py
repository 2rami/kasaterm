#!/usr/bin/env python3
"""claude-mark.json — 클로드 마크가 숨쉬는 로티.

소스는 예전 claude.svg 의 12방향 rect. 팔 하나가 레이어 하나라, 팔마다 위상을
어긋나게 세로로 늘였다 줄이면 claude.ai 의 그 "파도가 도는" 스타버스트가 된다.
(공식 심볼 path 는 팔이 한 덩어리로 붙어 있어 개별 제어가 안 되므로 쓰지 않는다.)
"""
import json
import os

FPS, DUR = 60, 120          # 2초 루프
SIZE = 100                  # 컴프 한 변
C = SIZE / 2
ARMS = 12
K = SIZE / 24.0             # 원본 24x24 → 100x100

ARM_W = 1.8 * K
ARM_H = 7.9 * K
ARM_R = 0.9 * K
ARM_CY = 5.15 * K           # 원본 rect 중심 y (y=1.2 + h/2)
ARM_INNER = 9.1 * K         # 팔의 안쪽 끝 (y=1.2 + h). 늘리기의 고정점.

SCALE_LO, SCALE_HI = 92.0, 114.0
COLOR = [217 / 255, 119 / 255, 87 / 255, 1]   # 공식 SVG 의 hsl(14.8,63.1%,59.6%)


def bez(v):
    """정지점 없는 부드러운 ease. 로티 키프레임의 i/o 핸들."""
    return {"x": [v], "y": [v]}


def scale_keys(phase):
    """한 팔의 scaleY 왕복. phase 만큼 프레임을 밀어 파도를 만든다."""
    # 위상 이동은 키프레임 시각을 미는 대신 값을 미룬 형태로 넣는다 — 그래야
    # 0 프레임과 DUR 프레임의 값이 같아 루프 이음매가 안 보인다.
    def at(t):
        import math
        u = (t / DUR + phase) % 1.0
        return SCALE_LO + (SCALE_HI - SCALE_LO) * (0.5 - 0.5 * math.cos(2 * math.pi * u))

    steps = 12
    keys = []
    for i in range(steps + 1):
        t = DUR * i / steps
        k = {"t": t, "s": [100.0, round(at(t), 2)]}
        if i < steps:
            k["i"], k["o"] = bez(0.6), bez(0.4)
        keys.append(k)
    return keys


layers = []
for i in range(ARMS):
    layers.append({
        "ddd": 0, "ind": i + 1, "ty": 4, "nm": f"arm{i:02d}",
        "sr": 1, "ip": 0, "op": DUR, "st": 0, "bm": 0,
        "ks": {
            "o": {"a": 0, "k": 100},
            "r": {"a": 0, "k": i * (360.0 / ARMS)},
            "p": {"a": 0, "k": [C, C, 0]},
            "a": {"a": 0, "k": [C, C, 0]},
            "s": {"a": 0, "k": [100, 100, 100]},
        },
        "shapes": [{
            "ty": "gr", "nm": "g", "hd": False,
            "it": [
                {"ty": "rc", "nm": "r", "hd": False, "d": 1,
                 "p": {"a": 0, "k": [C, round(ARM_CY, 3)]},
                 "s": {"a": 0, "k": [round(ARM_W, 3), round(ARM_H, 3)]},
                 "r": {"a": 0, "k": round(ARM_R, 3)}},
                {"ty": "fl", "nm": "f", "hd": False, "r": 1, "bm": 0,
                 "c": {"a": 0, "k": COLOR[:3]},
                 "o": {"a": 0, "k": 100}},
                # 앵커는 마크 중심이 아니라 팔의 **안쪽 끝**이다. 중심에 두면
                # 팔이 늘 때 안쪽 끝까지 같이 밀려나 가운데가 뻥 뚫리고
                # 로딩 스피너처럼 보였다 — 안쪽을 고정해야 마크가 안 흩어진다.
                {"ty": "tr", "nm": "tr",
                 "p": {"a": 0, "k": [C, round(ARM_INNER, 3)]},
                 "a": {"a": 0, "k": [C, round(ARM_INNER, 3)]},
                 "s": {"a": 1, "k": scale_keys(i / ARMS)},
                 "r": {"a": 0, "k": 0}, "o": {"a": 0, "k": 100},
                 "sk": {"a": 0, "k": 0}, "sa": {"a": 0, "k": 0}},
            ],
        }],
    })

doc = {
    "v": "5.9.0", "fr": FPS, "ip": 0, "op": DUR,
    "w": SIZE, "h": SIZE, "nm": "Claude mark", "ddd": 0,
    "assets": [], "layers": layers,
}

HERE = os.path.dirname(os.path.abspath(__file__))

out = f"{HERE}/claude-mark.json"
with open(out, "w") as f:
    json.dump(doc, f, separators=(",", ":"))
print("wrote", out)

# 미리보기는 json 을 인라인하지 않고 fetch 한다 — 인라인하면 사본이 둘이 되어
# 애니메이션을 손볼 때 미리보기만 옛것으로 남는다. 대신 file:// 로는 CORS 에
# 막히니 이 폴더에서 http 서버를 띄워 봐야 한다(첫 줄 주석에 적어 둠).
preview = """<!doctype html>
<meta charset="utf-8"><title>Claude mark — Lottie</title>
<!-- 이 폴더에서: python3 -m http.server 8790 → http://127.0.0.1:8790/preview.html
     (file:// 로 열면 fetch 가 CORS 에 막혀 아무것도 안 뜬다) -->
<style>
  body{margin:0;min-height:100vh;display:grid;place-items:center;gap:28px;
       background:#1c1c1f;color:#8a8a92;font:13px ui-monospace,monospace}
  .row{display:flex;align-items:center;gap:36px}
  .cell{display:grid;place-items:center;gap:8px}
  .box{background:#232327;border-radius:12px;display:grid;place-items:center}
</style>
<div class="row">
  <div class="cell"><div class="box" style="width:200px;height:200px"><div id="a" style="width:160px;height:160px"></div></div><span>160px</span></div>
  <div class="cell"><div class="box" style="width:96px;height:96px"><div id="b" style="width:64px;height:64px"></div></div><span>64px</span></div>
  <div class="cell"><div class="box" style="width:48px;height:48px"><div id="c" style="width:20px;height:20px"></div></div><span>20px (탭 아이콘 크기)</span></div>
</div>
<script src="https://cdnjs.cloudflare.com/ajax/libs/bodymovin/5.12.2/lottie.min.js"></script>
<script>
fetch('./claude-mark.json').then(r => r.json()).then(data => {
  for (const id of ['a','b','c'])
    lottie.loadAnimation({container:document.getElementById(id), renderer:'svg',
                          loop:true, autoplay:true, animationData:structuredClone(data)});
});
</script>
"""
pv = f"{HERE}/preview.html"
with open(pv, "w") as f:
    f.write(preview)
print("wrote", pv)
