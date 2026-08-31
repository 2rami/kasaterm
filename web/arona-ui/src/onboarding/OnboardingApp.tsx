import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import {
  ArrowLeft,
  ArrowRight,
  Check,
  Languages,
  LoaderCircle,
  SkipForward,
} from 'lucide-react';
import { fetchOnboardingState, fetchValues, postAction } from '../settings/api';
import { serverText, useLang, useT } from '../settings/lang';
import type { OnboardingState, SettingsValues } from '../settings/types';
import { useTokens } from '../settings/useTokens';
import {
  AppearanceStep,
  AuthStep,
  PlatformStep,
  ReadyStep,
  type AgentProvider,
  type AppearanceMode,
  type RunOnboardingAction,
} from './Steps';
import { ONBOARDING_STRINGS } from './strings';

const PROVIDERS: AgentProvider[] = ['claude', 'codex'];

export function OnboardingApp({
  initialState,
  onDone,
}: {
  initialState: OnboardingState;
  onDone: () => void;
}) {
  useTokens();
  const { lang, setLang } = useLang();
  const settingsStrings = useT();
  const t = ONBOARDING_STRINGS[lang];
  const [step, setStep] = useState(0);
  const [furthest, setFurthest] = useState(0);
  const [state, setState] = useState(initialState);
  const [settings, setSettings] = useState<SettingsValues | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [finishing, setFinishing] = useState(false);
  const [notice, setNotice] = useState<{ ok: boolean; text: string } | null>(null);
  const [preferred, setPreferred] = useState<AgentProvider | null>(null);
  const preferredTouched = useRef(false);
  const [appearanceMode, setAppearanceMode] = useState<AppearanceMode>(() =>
    initialState.platform === 'macos' && initialState.imports.some(
      (item) => item.detected && (item.support === 'full' || item.support === 'partial')
    )
      ? 'import'
      : 'manual'
  );
  const appearanceModeTouched = useRef(false);
  const [lastImported, setLastImported] = useState<string | null>(
    initialState.imported_profile ?? null
  );
  const [authPolling, setAuthPolling] = useState<Record<AgentProvider, boolean>>({
    claude: false,
    codex: false,
  });
  const pollGeneration = useRef<Record<AgentProvider, number>>({ claude: 0, codex: 0 });
  const alive = useRef(true);

  useEffect(() => {
    document.documentElement.lang = lang;
  }, [lang]);

  useEffect(() => {
    // 개발 모드의 StrictMode는 effect를 한 번 정리한 뒤 다시 붙인다. 다시 true로
    // 세우지 않으면 첫 화면 조회가 성공해도 "언마운트됨"으로 오해해 버린다.
    alive.current = true;
    return () => {
      alive.current = false;
      pollGeneration.current.claude += 1;
      pollGeneration.current.codex += 1;
    };
  }, []);

  const reloadAll = useCallback(async () => {
    const [nextState, nextSettings] = await Promise.all([
      fetchOnboardingState(),
      fetchValues(),
    ]);
    if (!alive.current) return;
    setState(nextState);
    setSettings(nextSettings);
    setLoadError(null);
  }, []);

  useEffect(() => {
    void reloadAll().catch((error) => {
      if (alive.current) setLoadError(error instanceof Error ? error.message : String(error));
    });
  }, [reloadAll]);

  useEffect(() => {
    if (preferredTouched.current || preferred) return;
    const detected = PROVIDERS.find(
      (provider) =>
        state.auth[provider].status === 'logged_in' &&
        (state.preferred_agent === provider || state.preferred_agent == null)
    ) ?? PROVIDERS.find((provider) => state.auth[provider].status === 'logged_in');
    if (detected) setPreferred(detected);
  }, [preferred, state.auth]);

  useEffect(() => {
    if (appearanceModeTouched.current || state.platform !== 'macos') return;
    setAppearanceMode(
      state.imports.some(
        (item) => item.detected && (item.support === 'full' || item.support === 'partial')
      ) ? 'import' : 'manual'
    );
  }, [state.imports, state.platform]);

  const run: RunOnboardingAction = useCallback(async (action, args) => {
    setBusy(true);
    setNotice(null);
    try {
      const result = await postAction(action, args);
      if (result.error || !result.ok) {
        const text = serverText(
          settingsStrings,
          result.error_code,
          result.error || result.message || t.actionFailed,
          result.error_args
        );
        setNotice({ ok: false, text });
        return false;
      }
      if (result.message) {
        setNotice({
          ok: true,
          text: serverText(
            settingsStrings,
            result.message_code,
            result.message,
            result.message_args
          ),
        });
      }
      try {
        await reloadAll();
      } catch (error) {
        // 저장은 이미 끝났다. 뒤따르는 화면 갱신 실패를 저장 실패로 바꾸면 완료·
        // 건너뛰기까지 막히므로, 경고는 남기되 액션 성공은 그대로 돌려준다.
        setNotice({
          ok: false,
          text: error instanceof Error ? error.message : t.actionFailed,
        });
      }
      return true;
    } catch (error) {
      setNotice({
        ok: false,
        text: error instanceof Error ? error.message : t.actionFailed,
      });
      return false;
    } finally {
      if (alive.current) setBusy(false);
    }
  }, [reloadAll, settingsStrings, t.actionFailed]);

  const pollAuth = useCallback(async (provider: AgentProvider) => {
    const generation = ++pollGeneration.current[provider];
    setAuthPolling((current) => ({ ...current, [provider]: true }));
    try {
      for (let attempt = 0; attempt < 60; attempt += 1) {
        await new Promise((resolve) => window.setTimeout(resolve, 1000));
        if (!alive.current || generation !== pollGeneration.current[provider]) return;
        const next = await fetchOnboardingState();
        if (!alive.current || generation !== pollGeneration.current[provider]) return;
        setState(next);
        if (next.auth[provider].status === 'logged_in') return;
        if (
          next.auth[provider].status === 'not_installed' ||
          next.auth[provider].status === 'error'
        ) return;
      }
    } catch (error) {
      if (alive.current) {
        setNotice({ ok: false, text: error instanceof Error ? error.message : t.actionFailed });
      }
    } finally {
      if (alive.current && generation === pollGeneration.current[provider]) {
        setAuthPolling((current) => ({ ...current, [provider]: false }));
      }
    }
  }, [t.actionFailed]);

  const login = useCallback(async (provider: AgentProvider) => {
    if (await run(`add-${provider}-account`)) void pollAuth(provider);
  }, [pollAuth, run]);

  const finish = useCallback(async (skip: boolean) => {
    setFinishing(true);
    const ok = await run(
      skip ? 'skip-onboarding' : 'complete-onboarding',
      !skip && preferred ? { id: preferred } : undefined
    );
    if (!ok) {
      setFinishing(false);
      return;
    }
    setNotice({ ok: true, text: t.ready.done });
    try {
      await postAction('close-settings').catch(() => undefined);
    } finally {
      if (alive.current) onDone();
    }
  }, [onDone, preferred, run, t.ready.done]);

  const currentMeta = t.steps[step];
  const appliedImportId = state.imported_profile ?? lastImported;
  const appearance = settings?.appearance;
  const content = useMemo(() => {
    if (!settings || !appearance) return null;
    if (step === 0) {
      return (
        <AppearanceStep
          t={t}
          state={state}
          appearance={appearance}
          mode={appearanceMode}
          appliedImportId={appliedImportId}
          busy={busy}
          onMode={(next) => {
            appearanceModeTouched.current = true;
            setAppearanceMode(next);
          }}
          run={run}
          onImported={setLastImported}
        />
      );
    }
    if (step === 1) {
      return (
        <AuthStep
          t={t}
          state={state}
          preferred={preferred}
          polling={authPolling}
          busy={busy}
          onPreferred={(provider) => {
            preferredTouched.current = true;
            setPreferred(provider);
          }}
          onLogin={(provider) => void login(provider)}
          onRetry={() => void reloadAll()}
        />
      );
    }
    if (step === 2) {
      return (
        <PlatformStep
          t={t}
          state={state}
          appearance={appearance}
          shell={settings.shell.shell}
          appliedImportId={appliedImportId}
          busy={busy}
          run={run}
        />
      );
    }
    return (
      <ReadyStep
        t={t}
        state={state}
        appearance={appearance}
        shell={settings.shell.shell}
        preferred={preferred}
      />
    );
  }, [
    appearance,
    appearanceMode,
    appliedImportId,
    authPolling,
    busy,
    login,
    preferred,
    reloadAll,
    run,
    settings,
    state,
    step,
    t,
  ]);

  const goNext = () => {
    const next = Math.min(3, step + 1);
    setStep(next);
    setFurthest((current) => Math.max(current, next));
    setNotice(null);
  };

  return (
    <div className="onboarding-root">
      <header className="onboarding-header">
        <div className="onboarding-brand"><span aria-hidden="true">k</span>{t.brand}</div>
        <div className="onboarding-header-actions">
          <div className="onboarding-language" aria-label={t.language}>
            <Languages aria-hidden="true" size={16} />
            <button type="button" aria-pressed={lang === 'ko'} onClick={() => setLang('ko')}>{t.ko}</button>
            <button type="button" aria-pressed={lang === 'en'} onClick={() => setLang('en')}>{t.en}</button>
          </div>
          <button
            type="button"
            className="onboarding-skip"
            disabled={finishing}
            onClick={() => void finish(true)}
          >
            <SkipForward aria-hidden="true" size={15} />
            {t.skip}
          </button>
        </div>
      </header>

      <div className="onboarding-layout">
        <nav className="onboarding-progress" aria-label="Onboarding">
          {t.steps.map((item, index) => {
            const current = index === step;
            const complete = index < step || index < furthest;
            return (
              <button
                key={item.short}
                type="button"
                disabled={index > furthest}
                aria-current={current ? 'step' : undefined}
                onClick={() => index <= furthest && setStep(index)}
              >
                <span aria-hidden="true">{complete ? <Check size={13} /> : index + 1}</span>
                <strong>{item.short}</strong>
              </button>
            );
          })}
        </nav>

        <main className="onboarding-main">
          <div className="onboarding-title-block">
            <h1>{currentMeta.title}</h1>
            <p>{currentMeta.hint}</p>
          </div>

          {loadError ? (
            <div className="onboarding-load-state is-error" role="alert">
              <h2>{t.loadFailed}</h2>
              <p>{t.loadFailedHint}</p>
              <code>{loadError}</code>
              <button type="button" className="onboarding-button is-primary" onClick={() => void reloadAll()}>
                <ArrowRight size={16} /> {t.retry}
              </button>
            </div>
          ) : content ? (
            content
          ) : (
            <div className="onboarding-load-state" role="status" aria-live="polite">
              <LoaderCircle className="onboarding-spin" size={24} />
              <h2>{t.loading}</h2>
              <p>{t.loadingHint}</p>
            </div>
          )}

          {notice && (
            <p className={`onboarding-notice ${notice.ok ? 'is-ok' : 'is-error'}`} role={notice.ok ? 'status' : 'alert'}>
              {notice.text}
            </p>
          )}

          <footer className="onboarding-footer">
            <button
              type="button"
              className="onboarding-button"
              disabled={step === 0 || busy || finishing}
              onClick={() => {
                setStep((current) => Math.max(0, current - 1));
                setNotice(null);
              }}
            >
              <ArrowLeft size={16} /> {t.back}
            </button>
            {step < 3 ? (
              <button
                type="button"
                className="onboarding-button is-primary"
                disabled={!content || busy || finishing}
                onClick={goNext}
              >
                {t.next} <ArrowRight size={16} />
              </button>
            ) : (
              <button
                type="button"
                className="onboarding-button is-primary"
                disabled={!content || busy || finishing}
                onClick={() => void finish(false)}
              >
                {finishing ? <LoaderCircle className="onboarding-spin" size={16} /> : <Check size={16} />}
                {finishing ? t.ready.opening : t.ready.open}
              </button>
            )}
          </footer>
        </main>
      </div>
    </div>
  );
}
