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
pane = os.environ.get('KASATERM_PANE_ID', '')
collab = os.path.join('/tmp/kasaterm-collab', os.getcwd().replace('/', '-').replace('.', '-'))
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
