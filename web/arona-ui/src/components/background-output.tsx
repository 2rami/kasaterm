// ccsv 는 /api/task-output (Next route)로 백그라운드 잡 로그를 fetch 하지만
// arona/kasa-mcp 엔 그 엔드포인트가 없다. no-op — 백그라운드 출력 섹션은 그리지
// 않는다(요청 input/결과 텍스트는 정상 표시됨).
export function BackgroundOutput(_props: { taskId: string; path: string }) {
  return null;
}
