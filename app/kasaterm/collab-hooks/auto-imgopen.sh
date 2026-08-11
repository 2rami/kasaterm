#!/bin/bash
# PostToolUse hook for SendUserFile — if any sent file is an image, also pop it
# into a kasaterm image pane so the user actually sees it (kasaterm doesn't
# render inline image escapes that Claude Code might or might not emit).
input=$(cat)
echo "$input" | python3 -c "
import sys, json, os, subprocess, time
try:
    d = json.load(sys.stdin)
except Exception:
    sys.exit(0)
paths = d.get('tool_input', {}).get('files', []) or []
exts = ('.png','.jpg','.jpeg','.gif','.webp','.bmp','.tiff','.tif')
# BA GUI 대화창 인라인용 기록처 — 이 방의 sent-images.jsonl(pane 별 필터).
# 방 모드(KASATERM_ROOM)면 slug 에 __room_<r> 접미 — 서버 sent_images_handler 의
# 읽기 경로(backend.active_room())와 일치시켜야 GET /sent-images 가 찾는다.
pane = os.environ.get('KASATERM_PANE_ID', '')
# 이 훅은 Git bash 가 부르지만 python3 은 네이티브라 cwd 가 'C:\\...', '/tmp' 가
# 'C:\\tmp' 로 잡힌다. 방 이름·루트를 sh 훅 형식(정본)으로 맞춰야 Rust 쪽
# (kasa_socket::collab_root / character::mode_slug)과 같은 폴더에서 만난다.
# chr(92) = 역슬래시 — 이 스크립트가 셸 큰따옴표 안이라 리터럴을 못 쓴다.
# nt 에서만 접는다 — unix 폴더 이름엔 역슬래시가 실제로 들어갈 수 있어,
# 거기서까지 접으면 멀쩡하던 슬러그가 sh 훅과 갈린다.
cwd = os.getcwd()
if os.name == 'nt':
    if len(cwd) >= 2 and cwd[0].isalpha() and cwd[1] == ':':
        cwd = '/' + cwd[0].lower() + '/' + cwd[2:].replace(chr(92), '/').lstrip('/')
    cwd = cwd.replace(chr(92), '/')
slug = cwd.replace('/', '-').replace('.', '-')
room = os.environ.get('KASATERM_ROOM', '')
if room:
    slug = slug + '__room_' + room
import tempfile
tmp_root = tempfile.gettempdir() if os.name == 'nt' else '/tmp'
collab = os.path.join(tmp_root, 'kasaterm-collab', slug)
for p in paths:
    if not isinstance(p, str): continue
    if not p.lower().endswith(exts): continue
    p_abs = p if os.path.isabs(p) else os.path.join(os.getcwd(), p)
    if not os.path.isfile(p_abs): continue
    # ① BA GUI 대화창 인라인 — transcript 엔 경로가 안 남아(input:{}) 훅이 유일 소스.
    try:
        os.makedirs(collab, exist_ok=True)
        with open(os.path.join(collab, 'sent-images.jsonl'), 'a') as f:
            f.write(json.dumps({'pane': pane, 'path': p_abs, 'ts': time.time()}) + '\n')
    except Exception:
        pass
    # ② 터미널 이미지 pane(기존 — solo 모드, BA GUI 와 병행)
    try:
        subprocess.run(['imgopen', p_abs], timeout=5, check=False)
    except Exception:
        pass
" >/dev/null 2>&1 || true
exit 0
