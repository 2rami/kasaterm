# 덮어도 마저 하기

맥북에서 학생이 일하는 중에 덮개를 닫으면, 깨어 있는 채 하던 일을 마저 끝내고 잠든다. 학생을 다른
기기로 옮기지 않는다(2026-10-22 결정 — 덮인 채 옮기면 다시 열었을 때 칸이 다른 기기 거울로 바뀌어 있고
미커밋 변경은 맥북에 남는다). 설정 「연결 기기 → 덮어도 마저 하기」에서 「덮었을 때」(바로 잠자기·마저
하고 잠자기)와 상한(10·30·60분)을 고른다. 코드는 `app/kasaterm/src/lid_away/policy.rs`(정책),
`app/kasaterm/src/lid_away.rs`(판독·실행),
`crates/kasa-socket/src/lid_away.rs`(설치물·pmset 명령). 설정 값은 `lid_away`(켬·끔)·`lid_away_minutes` 이고,
`lid_away` 가 없으면 이사이던 때의 `lid_away_machine` 이 비어 있지 않은 것을 켬으로 본다.

## 왜 sudo 가 필요한가

덮개 잠자기는 일반 권한으로 못 막는다. 전원 단언(`caffeinate`, Claude Code 가 일하며 거는 것도)은 덮개를
이기지 못하고, 잠자기 알림에 늦게 답해도 30초가 끝인데 하던 턴은 수 분에서 수십 분이 걸린다. 그래서
`pmset -a disablesleep` 를 sudoers 규칙 한 줄로 허락받는다. 처음 한 번 터미널에서:

```sh
kasaterm-cli lid-away install    # sudoers 규칙 + 감시 LaunchAgent, 비밀번호를 묻는다
kasaterm-cli lid-away status
kasaterm-cli lid-away uninstall  # 막음을 풀고 둘 다 지운다
```

규칙은 `/etc/sudoers.d/kasaterm-lid-away` 에 pmset 세 명령(`-a disablesleep 1`·`0`·`sleepnow`)만 연다.
이 계정에서 도는 다른 프로그램도 같은 명령을 비밀번호 없이 쓸 수 있게 되는 것이 대가다.

## 흐름

1. **막음(Armed)** — 이 기기 칸에서 학생이 일하거나 사람이 신청한 이사가 예약·실행·보기 창 전환 중이면
   잠자기를 막는다. 일이 60초 넘게 멎으면 푼다. 학생이 일하는 동안에는 메뉴의 잠자기도 막힌다.
2. **덮임(Away)** — 덮였고 활성 외부 모니터도 없으면 막은 채 깨어 있는다. 학생은 그대로 이 맥에서 일한다.
   화면 목록은 `CGGetActiveDisplayList`로 읽는다. `Online` 목록은 연결이 끊겨 잠든 모니터도 남을 수 있어 쓰지 않는다.
3. **잠(Sleeping)** — 일이 60초 넘게 멎으면 막음을 풀고 재운다. 턴이 끝난 직후 다른 학생의 쪽지로 다시 일하면
   60초를 처음부터 센다. 덮기 전에 이미 쉬고 있었으면 남은 시간만 기다린다.
   잠자기 명령이 성공해도 덮개를 열 때까지 감시를 유지한다. 실패하거나 덮인 채 다시 깨면 5초 간격으로
   다시 재운다. 이 동안 들어온 새 작업은 잠자기 결정을 취소하지 않는다.
4. **다시 열면** 마저 하던 중에는 Armed로 돌아간다. 이미 잠자기를 요청한 뒤라면 막음을 풀었는지 확인한 뒤
   Idle로 돌아가 새 작업을 받는다. 막음 해제도 실패하면 5초 간격으로 재시도한다.

상한(덮인 뒤 10·30·60분)이 차면 아직 일하는 학생이 있어도 잔다 — 그 학생은 맥북과 함께 잠들었다 깨어
이어 간다. 배터리가 20% 아래로 떨어지거나 열 상태가 serious 이상이면 바로 잔다. 덮인 채 잠깐 깨는
Power Nap 에서는 다시 막지 않는다 — 가방 속에서 상한만큼 깨어 있게 되기 때문이다.

## 막음이 남지 않게

`disablesleep` 는 재부팅 뒤에도 남는다. 앱은 막는 동안 5초마다 `~/.config/kasaterm/lid-away.armed` 를
고쳐 쓰고, 감시 LaunchAgent(`com.kasaterm.lid-away-watchdog`, 30초마다·로그인 때)가 90초 넘게 멎은
박동을 보면 막음을 풀고 덮여 있으면 재운다. 앱도 켜질 때 남은 박동을 보면 먼저 푼다. 해제·잠자기를
시도하기 시작하면 박동 갱신을 멈추고, 요청이 모두 성공한 뒤에만 지운다. 실패를 새 박동으로 가리지 않는다.
덮이면 창이 안 보여 App Nap 이 타이머를 미루므로 작업 중과 잠자기 재시도 중에는 NSProcessInfo 활동을 쥔다.

## 리그로 확인하기

진짜 잠자기 설정을 건드리지 않게 두 env 로 흉내 낸다([verify-app.md](verify-app.md) 격리 줄에 더한다).

- `KASATERM_LID_AWAY_PMSET=<실행 파일>` — `sudo -n /usr/bin/pmset` 대신 부른다(인자를 기록하는 스크립트).
  이 값이 있으면 sudo 허락도 있는 것으로 본다.
- `KASATERM_LID_PROBE=<파일>` — 내용이 `closed` 면 덮인 것으로 본다. 외부 모니터 검사는 건너뛴다.
- 칸을 「일하는 중」으로 만들려면 같은 소켓으로 `kasaterm-cli turn --surface %0 start`, 끝내려면 `… end`.
  설정 파일에 `"lid_away": true` 를 넣어 켠다.

박동 파일은 `KASATERM_SETTINGS_FILE` 옆에 생겨 본판 감시가 리그를 보지 않는다.
