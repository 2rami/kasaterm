# 덮으면 이사

맥북에서 학생이 일하는 중에 덮개를 닫으면, 하던 턴을 마저 끝내고 고른 기기(보통 맥미니)로 옮긴 뒤
잠든다. 설정 「연결 기기 → 덮으면 이사」에서 기기와 상한(10·30·60분)을 고른다. 코드는
`app/kasaterm/src/lid_away.rs`(정책), `crates/kasa-socket/src/lid_away.rs`(설치물·pmset 명령).

## 왜 sudo 가 필요한가

덮개 잠자기는 일반 권한으로 못 막는다. 전원 단언(`caffeinate`)은 덮개를 이기지 못하고, 잠자기 알림에
늦게 답해도 30초가 끝인데 이사는 턴이 끝나길 기다리고 한 건에 수 분이 걸린다. 그래서
`pmset -a disablesleep` 를 sudoers 규칙 한 줄로 허락받는다. 처음 한 번 터미널에서:

```sh
kasaterm-cli lid-away install    # sudoers 규칙 + 감시 LaunchAgent, 비밀번호를 묻는다
kasaterm-cli lid-away status
kasaterm-cli lid-away uninstall  # 막음을 풀고 둘 다 지운다
```

규칙은 `/etc/sudoers.d/kasaterm-lid-away` 에 pmset 세 명령(`-a disablesleep 1`·`0`·`sleepnow`)만 연다.
이 계정에서 도는 다른 프로그램도 같은 명령을 비밀번호 없이 쓸 수 있게 되는 것이 대가다.

## 흐름

1. **막음(Armed)** — 이 기기 칸에서 학생이 일하거나 이사 예약·실행·보기 창 전환이 있으면 잠자기를 막는다. 일과 이사가 모두 60초 넘게 멎으면 푼다.
   학생이 일하는 동안에는 메뉴의 잠자기도 막힌다.
2. **떠남(Away)** — 덮였고 외부 모니터도 없으면, 일하는 학생 칸마다 예약 이사를 건다(`migrate_pane`,
   턴이 끝나면 `run_pending_migrations` 가 하나씩 쏜다). 덮인 동안 새로 일하기 시작한 학생도 건다.
3. **잠** — 일하는 학생도 예약·실행·보기 창 전환 중인 이사도 없이 15초가 지나면 막음을 풀고 재운다. 본인이 CLI로 신청한 이사도 포함한다.
4. **다시 열면** 아직 안 떠난 예약을 거둔다. 이미 옮긴 학생은 거울로 본다 — 자동으로 데려오지 않는다.

옮기지 않는 것: 쉬던 학생, 셸·dev 서버 칸, 경로를 그 기기로 못 옮기는 칸(`machines.json` roots),
세션 id 를 아직 모르는 칸. 이 칸들은 맥북과 함께 잠들었다 깨어 이어간다. 이사는 대화만 옮기므로
([session-transfer.md](session-transfer.md)) 미커밋 변경은 맥북에 남는다.

상한이 차거나, 배터리가 20% 아래로 떨어지거나, 열 상태가 serious 이상이면 새 예약을 멈추고 안 떠난
예약을 거둔 뒤, 도는 이사만 마저 끝내고 잔다(그래도 상한 + 5분이면 무조건 잔다). 덮인 채 잠깐 깨는
Power Nap 에서는 다시 막지 않는다 — 가방 속에서 상한만큼 깨어 있게 되기 때문이다.

## 막음이 남지 않게

`disablesleep` 는 재부팅 뒤에도 남는다. 앱은 막는 동안 5초마다 `~/.config/kasaterm/lid-away.armed` 를
고쳐 쓰고, 감시 LaunchAgent(`com.kasaterm.lid-away-watchdog`, 30초마다·로그인 때)가 90초 넘게 멎은
박동을 보면 막음을 풀고 덮여 있으면 재운다. 앱도 켜질 때 남은 박동을 보면 먼저 푼다. 덮이면 창이 안 보여
App Nap 이 타이머를 미루므로 깨어 있는 동안은 NSProcessInfo 활동을 쥔다.

## 리그로 확인하기

진짜 잠자기 설정을 건드리지 않게 두 env 로 흉내 낸다([verify-app.md](verify-app.md) 격리 줄에 더한다).

- `KASATERM_LID_AWAY_PMSET=<실행 파일>` — `sudo -n /usr/bin/pmset` 대신 부른다(인자를 기록하는 스크립트).
  이 값이 있으면 sudo 허락도 있는 것으로 본다.
- `KASATERM_LID_PROBE=<파일>` — 내용이 `closed` 면 덮인 것으로 본다. 외부 모니터 검사는 건너뛴다.

박동 파일은 `KASATERM_SETTINGS_FILE` 옆에 생겨 본판 감시가 리그를 보지 않는다.
