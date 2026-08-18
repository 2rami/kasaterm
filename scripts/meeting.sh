#!/bin/bash
# 회의 실시간 녹음·전사. 30초 청크를 받아 적어 한 파일에 이어 붙인다.
#
# ⚠️ 이 파이프라인의 존재 이유는 정확도가 아니라 **환각 차단**이다. whisper 계열은
# 무음을 받으면 침묵하지 않고 「감사합니다」를 지어낸다. macOS 는 마이크 권한이
# 없을 때 에러 대신 무음을 흘리므로, 검사가 없으면 **녹음이 실패한 회의의 그럴듯한
# 가짜 회의록**이 완성된다. 2026-08-18 타운홀에서 실제로 그 직전까지 갔다.
#
# 사용법:
#   meeting start [이름]   녹음+전사 시작
#   meeting stop           둘 다 정지
#   meeting status         상태·마지막 발언
#   meeting open           회의록 열기
#   meeting clean [이름]    끝난 회의의 wav 삭제(회의록은 남긴다)
set -uo pipefail

ROOT="${MEETING_ROOT:-$HOME/Documents/회의록}"
CUR="$ROOT/.current"
MODEL="${MEETING_MODEL:-mlx-community/whisper-large-v3-turbo}"
LANG_="${MEETING_LANG:-ko}"
CHUNK_SEC="${MEETING_CHUNK_SEC:-30}"

# 무음 문턱. 실측(2026-08-18 타운홀 84청크): 실제 발언은 **최저 0.0727** 이었고
# 0.005 미만은 하나도 없었다. 권한이 없을 때만 정확히 0.000000 이 나온다. 그래서
# 이 값은 「조용한 발언을 자르지 않으면서 무음만 거른다」 쪽으로 넉넉히 낮게 잡았다.
# ⚠️ 올리지 마라 — 회의실 뒤쪽 발언이 이 값과 0.07 사이에 있을 수 있는데 그 구간은
# 아직 실측이 없다(그 회의는 전원이 마이크 근처였다).
SILENCE="${MEETING_SILENCE:-0.0005}"

die() { echo "meeting: $*" >&2; exit 1; }
need() { command -v "$1" >/dev/null 2>&1 || die "$1 가 없다. brew install $2"; }

amp_of() {  # wav 의 최대 진폭. 못 재면 0.
  sox "$1" -n stat 2>&1 | awk -F: '/Maximum amplitude/{gsub(/ /,"",$2);print $2+0; f=1} END{if(!f)print 0}'
}

# 마이크가 실제로 소리를 잡는지 **직접 녹음해서** 판정한다. TCC.db 조회는 전체 디스크
# 접근이 필요하고, 권한 상태는 앱 서명·재시작으로 바뀌므로 「지금 이 프로세스가 소리를
# 받는가」만이 믿을 수 있는 답이다. kasaterm 에 마이크 권한이 생기면 이 검사가 저절로
# 통과해 Terminal 우회 없이 돈다 — 조건을 코드에 박아 두지 않는 이유다.
mic_works() {
  local t; t=$(mktemp -t micprobe).wav
  rec -q -r 16000 -c 1 "$t" trim 0 1 >/dev/null 2>&1
  local a; a=$(amp_of "$t"); rm -f "$t"
  awk -v a="$a" 'BEGIN{exit !(a > 0.000001)}'
}

cmd_start() {
  need sox sox; need rec sox
  command -v mlx_whisper >/dev/null 2>&1 || die "mlx_whisper 가 없다. pip install mlx-whisper"
  [ -f "$CUR" ] && [ -d "$(cat "$CUR" 2>/dev/null)" ] && {
    echo "이미 도는 회의가 있다: $(cat "$CUR")"
    echo "먼저 'meeting stop' 을 부르거나 'meeting status' 로 확인해라"; exit 1; }

  local name="${1:-}" dir
  dir="$ROOT/$(date '+%Y-%m-%d-%H%M')${name:+-$name}"
  mkdir -p "$dir/chunks" || die "폴더를 못 만든다: $dir"
  echo "$dir" > "$CUR"
  : > "$dir/회의록.md"
  {
    echo "# 회의록 — $(date '+%Y-%m-%d %H:%M')${name:+  ($name)}"
    echo
  } >> "$dir/회의록.md"

  # 전사 루프는 마이크가 필요 없다 — 여기서 바로 띄운다.
  nohup "$0" _transcribe "$dir" > "$dir/전사.log" 2>&1 &
  echo $! > "$dir/.transcribe.pid"

  if mic_works; then
    nohup "$0" _record "$dir" > "$dir/녹음.log" 2>&1 &
    echo $! > "$dir/.record.pid"
    echo "녹음 시작 (이 프로세스에 마이크 권한이 있다)"
  else
    # 우회. 이 창이 녹음 주체가 되고, 권한 팝업은 Terminal 이름으로 정상적으로 뜬다.
    # ⚠️ kasaterm 은 Info.plist 에 NSMicrophoneUsageDescription 이 없어 팝업이 아예
    # 안 뜨고 시스템 설정 마이크 목록에도 나타나지 않는다 — 켤 방법이 없어서 우회한다.
    local cmdf="$dir/녹음시작.command"
    cat > "$cmdf" <<CMDEOF
#!/bin/bash
# 이 창이 녹음 주체다. 창을 닫으면 녹음이 멈춘다.
echo "회의 녹음 중 — 이 창을 닫으면 멈춥니다."
echo "저장 위치: $dir"
exec "$0" _record "$dir"
CMDEOF
    chmod +x "$cmdf"
    open -a Terminal "$cmdf"
    echo "녹음은 Terminal 창에서 시작한다 (이 프로세스엔 마이크 권한이 없다)"
    echo "  → 권한을 물으면 허용해라. 그 창을 닫으면 녹음이 멈춘다."
  fi

  echo
  echo "회의록: $dir/회의록.md"
  echo "  실시간으로 보려면:  tail -f '$dir/회의록.md'"
  echo "  끝내려면:           meeting stop"
}

# 30초씩 잘라 저장하고 .done 마커를 찍는다. 마커가 있어야 전사가 **다 쓰인 파일만**
# 집어간다 — 없으면 쓰는 중인 wav 를 읽어 뒷부분이 잘린 전사가 나온다.
cmd_record() {
  local dir="$1" i=0 f
  # 이어서 시작할 때 번호가 겹치지 않게(재시작·창 다시 열기).
  while [ -f "$(printf "%s/chunks/chunk_%04d.wav" "$dir" $i)" ]; do i=$((i+1)); done
  while [ -f "$CUR" ]; do
    f=$(printf "%s/chunks/chunk_%04d.wav" "$dir" $i)
    rec -q -r 16000 -c 1 "$f" trim 0 "$CHUNK_SEC" 2>/dev/null
    [ -f "$f" ] || break
    local a; a=$(amp_of "$f")
    echo "[$(date '+%H:%M:%S')] chunk $i  음량=$a"
    touch "$f.done"
    i=$((i+1))
  done
}

# 같은 줄이 3회 이상 이어지면 발언이 아니라 환각이다(박수·웃음 구간에서 난다).
# **지우지 않고 한 줄로 접은 뒤 몇 번이었는지 남긴다** — 진짜로 세 번 말했을 수도
# 있어서, 판단 근거를 회의록에서 지워 버리면 나중에 되짚을 수가 없다.
# 실측: 「오늘의 주인공은 오늘의 주인공입니다.」 5줄 연속 뒤에 진짜 발언이 이어졌다.
# 그래서 청크를 통째로 버리면 안 되고 **줄 단위**여야 한다.
fold_repeats() {
  awk '
    function flush() {
      if (n == 0) return
      if (n >= 3) print prev "   ⟨×" n " 반복 — 환각 의심⟩"
      else for (k = 0; k < n; k++) print prev
      n = 0
    }
    { if ($0 == prev) n++; else { flush(); prev = $0; n = 1 } }
    END { flush() }
  '
}

cmd_transcribe() {
  local dir="$1"
  local log="$dir/회의록.md"   # ⚠️ 한 local 문에서 앞 변수를 참조하면 set -u 가 터진다
  cd "$dir" || exit 1
  while [ -f "$CUR" ]; do
    for w in chunks/chunk_*.wav; do
      [ -f "$w.done" ] || continue
      [ -f "$w.txt_done" ] && continue
      local a; a=$(amp_of "$w")
      # 무음이면 전사에 넣지 않는다. 넣으면 지어낸다.
      if awk -v a="$a" -v s="$SILENCE" 'BEGIN{exit !(a+0 <= s+0)}'; then
        echo "[skip] $w 음량 $a — 무음이라 건너뜀 (권한 문제일 수 있다)"
        touch "$w.txt_done"; continue
      fi
      mlx_whisper "$w" --model "$MODEL" --language "$LANG_" \
        --output-dir chunks --output-format txt --verbose False >/dev/null 2>&1
      local b; b="chunks/$(basename "$w" .wav).txt"
      if [ -s "$b" ]; then
        {
          echo "### $(date '+%H:%M:%S')"
          # 유튜브 상용구는 회의에서 나올 수 없다 — 무발화 구간의 환각이라 지운다.
          # (단독 「감사합니다.」는 회의에서 실제로 나오므로 **지우지 않는다**.)
          grep -vE '^(시청해주셔서 감사합니다\.?|구독과 좋아요.*)$' "$b" | fold_repeats
          echo
        } >> "$log"
      fi
      touch "$w.txt_done"
    done
    sleep 3
  done
}

cmd_stop() {
  [ -f "$CUR" ] || { echo "도는 회의가 없다"; exit 0; }
  local dir; dir=$(cat "$CUR")
  rm -f "$CUR"        # 두 루프의 while 조건이라, 지우는 것만으로 다음 바퀴에 멈춘다
  local p
  for p in "$dir/.record.pid" "$dir/.transcribe.pid"; do
    [ -f "$p" ] || continue
    kill "$(cat "$p")" 2>/dev/null
    rm -f "$p"
  done
  # 녹음 중인 rec 은 이 회의 폴더로 쓰는 것만 골라 멈춘다.
  # ⚠️ pkill -f rec 같은 이름 기반 종료는 절대 쓰지 마라 — 남의 프로세스를 죽인다.
  local pid
  # index() 로 **리터럴** 비교한다 — `~` 는 정규식이라 폴더 이름의 `.` 이 아무 글자나
  # 받아 남의 회의 rec 까지 매칭할 수 있다.
  pid=$(ps -Ao pid,command | awk -v d="$dir/chunks/" 'index($0, d) && /[r]ec -q/{print $1}')
  [ -n "$pid" ] && kill $pid 2>/dev/null
  echo "정지했다: $dir/회의록.md"
  local n; n=$(ls "$dir"/chunks/*.wav 2>/dev/null | wc -l | tr -d ' ')
  local sz; sz=$(du -sh "$dir/chunks" 2>/dev/null | cut -f1)
  echo "청크 $n 개 ($sz). 회의록만 남기려면: meeting clean '$(basename "$dir")'"
}

cmd_status() {
  if [ ! -f "$CUR" ]; then
    echo "도는 회의 없음"
    local last; last=$(ls -td "$ROOT"/*/ 2>/dev/null | head -1)
    [ -n "$last" ] && echo "마지막 회의: $last"
    return
  fi
  local dir; dir=$(cat "$CUR")
  local n done_ skip
  n=$(ls "$dir"/chunks/*.wav 2>/dev/null | wc -l | tr -d ' ')
  done_=$(ls "$dir"/chunks/*.txt_done 2>/dev/null | wc -l | tr -d ' ')
  skip=$(grep -c '^\[skip\]' "$dir/전사.log" 2>/dev/null || echo 0)
  echo "진행 중: $dir"
  echo "  청크 $n 개 · 전사 $done_ 개 · 무음으로 건너뜀 $skip 개"
  # 무음이 계속 쌓이면 녹음이 실패하고 있다는 뜻이다. 조용히 두면 빈 회의록이 남는다.
  if [ "$skip" -gt 2 ] && [ "$skip" -ge "$((done_ / 2 + 1))" ]; then
    echo "  ⚠️ 무음 청크가 많다 — 마이크 권한을 확인해라(설정 → 개인정보 보호 → 마이크)"
  fi
  echo "  최근 발언:"
  grep -v '^###' "$dir/회의록.md" 2>/dev/null | grep -v '^$' | tail -3 | sed 's/^/    /'
}

cmd_open() {
  local dir
  if [ -f "$CUR" ]; then dir=$(cat "$CUR"); else dir=$(ls -td "$ROOT"/*/ 2>/dev/null | head -1); fi
  [ -n "${dir:-}" ] || die "회의록이 없다"
  open "$dir/회의록.md"
}

cmd_clean() {
  local name="${1:-}" dir
  [ -n "$name" ] || die "어느 회의인지 알려줘라: meeting clean 2026-08-18-1520"
  dir="$ROOT/$name"
  [ -d "$dir" ] || die "그런 회의가 없다: $dir"
  [ -f "$CUR" ] && [ "$(cat "$CUR")" = "$dir" ] && die "지금 도는 회의다. 먼저 meeting stop"
  local sz; sz=$(du -sh "$dir/chunks" 2>/dev/null | cut -f1)
  rm -rf "$dir/chunks"
  echo "wav 삭제 ($sz). 회의록은 그대로: $dir/회의록.md"
}

case "${1:-}" in
  start)   shift; cmd_start "$@" ;;
  stop)    cmd_stop ;;
  status)  cmd_status ;;
  open)    cmd_open ;;
  clean)   shift; cmd_clean "$@" ;;
  _record) shift; cmd_record "$@" ;;       # 내부용 — 직접 부르지 마라
  _transcribe) shift; cmd_transcribe "$@" ;;
  *) sed -n '/^# 사용법:/,/^#   meeting clean/p' "$0" | sed 's/^# \{0,1\}//' ;;
esac
