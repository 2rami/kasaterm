# 칸 실행 파일의 수명

칸의 PATH·ZDOTDIR에 들어가는 래퍼, 셸 시작 파일, 신원 도우미, 훅 설정은 앱이 켜져 있는 동안 필요하다.
오래 켠 앱은 처음 설치한 뒤 며칠이 지나서 새 칸을 열 수 있으므로 OS 임시 폴더에 보관하지 않는다.
설정 파일 옆 `runtime/kasaterm-shim-<pid>`를 사용하며 라이트는 `kasaterm-lite-shim-<pid>`로 분리한다.
Unix의 새 디렉터리는 소유자만 읽고 쓸 수 있게 만든다. 파일 교체는 기존처럼 같은 폴더의 임시 파일과 rename을 쓴다.

`KASATERM_SETTINGS_FILE`을 지정한 검증 앱은 그 설정 파일 옆에 설치한다. 사용자 런타임 폴더를 공유하지 않는다.
계정·복원·진행 중인 학생 신원을 바꾸는 작업이 아니며, 실행 중인 구판의 경로는 다음 기동까지 그대로다.
진행 중인 앱이나 그 칸의 하네스가 사용하는 런타임 폴더를 용량 정리 대상으로 삼지 않는다.

## 구판에서 새 학생을 못 띄울 때

- `real claude not found on PATH`: 실물 Claude뿐 아니라 shim의 `.zshenv`·`.zprofile`·`.zshrc`도 확인한다.
  ZDOTDIR의 파일이 없어지면 HOME의 시작 파일도 못 읽어 `~/.local/bin`이 PATH에서 빠질 수 있다.
- `python3: can't open file .../agent-identity.py`: Python 미설치가 아니라 도우미 유실이다.
  새 래퍼는 이를 따로 알리고 신원 없는 학생을 실행하지 않는다.
- `claude-hooks-settings.json`도 함께 확인한다. 셸 시작 파일만 복구하면 다음 단계에서 다시 막힐 수 있다.

재시작 없이 복구할 때는 실행 중인 앱의 PID·설치 버전·실제 shim 경로를 먼저 확인한다.
설치된 앱 번들 또는 그 버전의 소스로 유실된 파일만 원자적으로 복구한다. 다른 판의 래퍼·사용자 설정으로
덮거나 사용자 앱을 재시작하지 않는다. 이미 열린 셸의 PATH는 저절로 바뀌지 않으므로 새 셸에서 검사한다.
`claude --version`만으로 신원 복구를 완료 처리하지 않고, 새 학생의 보드에서 session_id·surface_key·캐릭터·방을 확인한다.

검증은 `cargo test -p kasaterm --bin kasaterm pane_shims`와 격리 앱의 임시 파일 정리 뒤 새 칸 열기로 한다.
셸 시작 파일, 사용자 PATH, 신원 도우미가 살아 있고 기존 칸도 이어지는지 확인한다.
