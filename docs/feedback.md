# 피드백 접수

앱의 **피드백 보내기**는 사용자가 작성한 내용과 선택한 진단 정보만
`https://kasaterm.debimarlene.com/feedback`으로 보낸다. 진단 정보는 앱 버전·OS·아키텍처다.
보내기 전에 로컬 사본을 저장하고, Discord의 전달 응답을 받은 뒤에만 성공으로 표시한다.
전송 중 새로 쓴 초안은 완료 응답이 지우지 않는다. **저장** API는 로컬 저장 의미를 유지한다.

서버는 내용을 에이전트 입력으로 넘기지 않고 개발자 Discord DM에 본문과 `feedback.txt` 첨부로 전달한다.
사용자 앱에는 봇 자격증명이나 수신자 ID를 넣지 않는다. 멘션은 허용하지 않는다.

## 서버 설정

`kasa-relay` 프로세스에 `KASATERM_FEEDBACK_DISCORD_TOKEN`과
`KASATERM_FEEDBACK_DISCORD_USER`를 설정한다. 또는 두 값을 담은 0600 파일을 만들고
`KASATERM_FEEDBACK_ENV_FILE`로 지정한다. 서비스 파일은 데스크톱·문서 폴더의 OS 접근 허가에
의존하지 않는 서버 설정 디렉터리에 둔다.

기존 환경 파일에서 두 값만 옮길 때는 다음 도구를 쓴다. 비밀값을 출력하거나 셸로 평가하지 않는다.

```sh
python3 scripts/configure-feedback-relay.py SOURCE.env PRIVATE.env \
  --token-key BOT_TOKEN_KEY --user-key RECIPIENT_ID_KEY
```

전송 한도는 본문 16,000바이트, 진단 2,000바이트다. 같은 출처는 60초에 한 번,
전체는 60초에 30개까지 허용한다. 수신 설정이 없으면 503으로 명시적으로 실패한다.
응답은 성공 시 `200 {"ok":true}`, 입력 오류 400, 요청 크기 초과 413, 빈도 초과 429다.
전달 오류 502·시간 초과는 전달 여부가 불명확할 수 있으므로 자동 재전송하지 않는다.

검증: `cargo test -p kasa-mcp --lib feedback`. 화면 검증은 앱의 격리 실행 절차
(`verify-app.md`)를 따른다.
