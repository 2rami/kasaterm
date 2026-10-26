# HTTP 대화 기록 CPU 확인

`kasa-mcp`는 HTTP 라우터와 JSON 직렬화를 포함한다. `Cargo.toml`의 패키지별
`opt-level=0`은 Windows의 과거 LLVM 오류 우회다. 이 설정을 macOS에도 그대로 적용하면
배포판에서도 큰 대화 문자열을 검사하고 이스케이프하는 코드가 최적화되지 않는다.
macOS 번들 경로인 `scripts/build-app.sh`는 release에서 이 패키지만 `opt-level=3`으로
덮어쓴다. Windows 빌드 설정은 바꾸지 않는다. 개발용 debug 빌드의 수치로 성능을 판단하지 않는다.

## 같은 응답으로 비교

`transcript_http_bench`는 실제 `/transcript-raw` 라우터에 읽기 전용 가짜 Backend를 붙인다.
사용자 기록을 읽지 않으며, 임의 루프백 포트와 별도 협업·설정 경로를 쓴다.
Python 드라이버는 각각 512KiB·2MiB 기록을 네 요청씩 동시에 읽고, 변경 없는 증분 요청도
비교한다. 응답의 한글·이스케이프·바이트 수·파일 신원·체크포인트가 같은지 확인한다.

```sh
BENCH_DIR="$(mktemp -d /tmp/kasa-http-bench.XXXXXX)"
CARGO_BUILD_JOBS=2 cargo build --release -p kasa-mcp --example transcript_http_bench \
  --config 'profile.release.package.kasa-mcp.opt-level=0'
cp "${CARGO_TARGET_DIR:-target}/release/examples/transcript_http_bench" "$BENCH_DIR/before"
CARGO_BUILD_JOBS=2 cargo build --release -p kasa-mcp --example transcript_http_bench \
  --config 'profile.release.package.kasa-mcp.opt-level=3'
cp "${CARGO_TARGET_DIR:-target}/release/examples/transcript_http_bench" "$BENCH_DIR/after"
python3 scripts/bench-transcript-http.py --before "$BENCH_DIR/before" \
  --after "$BENCH_DIR/after" --output "$BENCH_DIR/results"
```

`bench-results.json`의 `server_cpu_seconds`는 서버 프로세스의 사용자·커널 CPU 시간 합이다.
같은 요청 수의 CPU 시간을 먼저 비교한다. `average_cpu_percent`는 요청을 가능한 한 빨리
처리한 구간의 평균이라, 더 빨라진 서버가 짧은 시간에 여러 코어를 써도 높게 나올 수 있다.
두 번의 M3 Max 격리 실측에서 2MiB × 32 요청의 서버 CPU 시간은 1.05–1.19초에서
0.15–0.16초로 줄었고 응답 바이트 수는 같았다(약 86% 감소). 실제 앱의 CPU 전후 수치는
새 판 적용 후 같은 방·미러·작업 조건에서 별도로 측정한다.

실제 앱에서 원인을 좁힐 때는 스택 샘플의 라우터·문자열 처리 경로, 연결별 바이트 변화,
처음 읽기와 증분 읽기를 구분한다. 샘플만 보고 특정 클라이언트의 반복 호출 횟수나
`offset=0` 재설정을 단정하지 않는다. 요청·응답 본문이나 인증 헤더를 진단 로그에 남기지 않는다.
