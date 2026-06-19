// ccsv 는 jotai atomWithStorage 로 expandThinking 를 영속+구독한다. arona 는
// jotai/zod 금지라 React 내장 useSyncExternalStore 로 같은 계약(localStorage
// 백킹 + 값 변경 시 모든 구독 컴포넌트 리렌더 + 크로스탭 storage 이벤트 동기화)을
// 직접 구현한다.
import { useSyncExternalStore } from "react";

const KEY = "arona-expand-thinking";

let expandThinking = readInitial();
const listeners = new Set<() => void>();

function readInitial(): boolean {
  if (typeof window === "undefined") return false;
  try {
    return window.localStorage.getItem(KEY) === "1";
  } catch {
    return false;
  }
}

function emit() {
  for (const l of listeners) l();
}

export function setExpandThinking(value: boolean) {
  if (value === expandThinking) return;
  expandThinking = value;
  try {
    window.localStorage.setItem(KEY, value ? "1" : "0");
  } catch {
    // 시크릿 모드 등 localStorage 차단 — 인메모리 값만 유지
  }
  emit();
}

function subscribe(onChange: () => void) {
  listeners.add(onChange);
  // 다른 탭에서 바꾼 값도 반영
  const onStorage = (e: StorageEvent) => {
    if (e.key !== KEY) return;
    const next = e.newValue === "1";
    if (next !== expandThinking) {
      expandThinking = next;
      emit();
    }
  };
  window.addEventListener("storage", onStorage);
  return () => {
    listeners.delete(onChange);
    window.removeEventListener("storage", onStorage);
  };
}

export function useSettings() {
  const value = useSyncExternalStore(
    subscribe,
    () => expandThinking,
    () => false,
  );
  return { expandThinking: value, setExpandThinking };
}
