import { useEffect, useState } from 'react';
import { fetchOnboardingState } from '../settings/api';
import { SettingsApp } from '../settings/SettingsApp';
import type { OnboardingState } from '../settings/types';
import { OnboardingApp } from './OnboardingApp';

type Gate = { kind: 'checking' } | { kind: 'settings' } | { kind: 'onboarding'; state: OnboardingState };

export function SettingsRoot() {
  const [gate, setGate] = useState<Gate>({ kind: 'checking' });

  useEffect(() => {
    let alive = true;
    void fetchOnboardingState()
      .then((state) => {
        if (!alive) return;
        setGate(state.completed ? { kind: 'settings' } : { kind: 'onboarding', state });
      })
      .catch(() => {
        // 이전 버전 서버에는 온보딩 API가 없다. 설정 자체까지 막으면 복구할 길이 없다.
        if (alive) setGate({ kind: 'settings' });
      });
    return () => {
      alive = false;
    };
  }, []);

  if (gate.kind === 'checking') {
    return <div className="onboarding-gate-loader" role="status" aria-label="Loading" />;
  }
  if (gate.kind === 'onboarding') {
    return <OnboardingApp initialState={gate.state} onDone={() => setGate({ kind: 'settings' })} />;
  }
  return <SettingsApp />;
}
