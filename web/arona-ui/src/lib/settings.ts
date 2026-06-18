// ccsv thinking-block 이 expandThinking 토글만 읽는다. arona 는 localStorage
// settings store 가 따로 없으니 항상 "접힘" 기본값을 주는 얇은 shim.
export function useSettings() {
  return { expandThinking: false };
}
