import {
  Check,
  CheckCircle2,
  CircleAlert,
  CircleOff,
  Import,
  LoaderCircle,
  LogIn,
  MonitorCog,
  Palette,
  RotateCw,
  ShieldCheck,
  Terminal,
  Type,
} from 'lucide-react';
import type { KeyboardEvent } from 'react';
import { Stepper, TextField } from '../settings/controls';
import type {
  AppearanceValues,
  OnboardingAuthProvider,
  OnboardingAuthStatus,
  OnboardingState,
  ThemePreset,
} from '../settings/types';
import type { OnboardingStrings } from './strings';

export type AgentProvider = 'claude' | 'codex';
export type AppearanceMode = 'import' | 'manual';
export type ActionArgs = { id?: string; label?: string };
export type RunOnboardingAction = (action: string, args?: ActionArgs) => Promise<boolean>;

function moveRadioFocus(event: KeyboardEvent<HTMLDivElement>) {
  const keys = ['ArrowRight', 'ArrowDown', 'ArrowLeft', 'ArrowUp', 'Home', 'End'];
  if (!keys.includes(event.key)) return;
  const current = event.target instanceof Element
    ? event.target.closest<HTMLButtonElement>('button[role="radio"]')
    : null;
  if (!current) return;
  const radios = Array.from(
    event.currentTarget.querySelectorAll<HTMLButtonElement>('button[role="radio"]:not(:disabled)')
  );
  const index = radios.indexOf(current);
  if (index < 0 || radios.length === 0) return;
  event.preventDefault();
  const next = event.key === 'Home'
    ? radios[0]
    : event.key === 'End'
      ? radios[radios.length - 1]
      : radios[(index + (event.key === 'ArrowRight' || event.key === 'ArrowDown' ? 1 : -1) + radios.length) % radios.length];
  next.focus();
  next.click();
}

function EmptyState({ title, hint }: { title: string; hint: string }) {
  return (
    <div className="onboarding-empty" role="status">
      <CircleOff aria-hidden="true" size={20} />
      <div>
        <strong>{title}</strong>
        <p>{hint}</p>
      </div>
    </div>
  );
}

function ThemeChoice({
  theme,
  active,
  busy,
  onPick,
}: {
  theme: ThemePreset;
  active: boolean;
  busy: boolean;
  onPick: () => void;
}) {
  return (
    <button
      type="button"
      className="onboarding-theme"
      disabled={busy}
      aria-pressed={active}
      onClick={() => !active && onPick()}
      style={{ background: theme.bg, color: theme.text }}
    >
      <span className="onboarding-theme-command">❯ git status</span>
      <span className="onboarding-theme-dots" aria-hidden="true">
        {theme.ansi.slice(0, 6).map((color, index) => (
          <span key={`${color}-${index}`} style={{ background: color }} />
        ))}
      </span>
      <span className="onboarding-theme-name" style={{ color: theme.dim }}>
        {theme.label}
      </span>
      {active && (
        <span className="onboarding-theme-check" aria-hidden="true">
          <Check size={13} />
        </span>
      )}
    </button>
  );
}

export function AppearanceStep({
  t,
  state,
  appearance,
  mode,
  appliedImportId,
  busy,
  onMode,
  run,
  onImported,
}: {
  t: OnboardingStrings;
  state: OnboardingState;
  appearance: AppearanceValues;
  mode: AppearanceMode;
  appliedImportId: string | null;
  busy: boolean;
  onMode: (mode: AppearanceMode) => void;
  run: RunOnboardingAction;
  onImported: (id: string) => void;
}) {
  const canImport = state.platform === 'macos';

  return (
    <div className="onboarding-step-stack">
      {canImport && (
        <div className="onboarding-segment" role="group" aria-label={t.appearance.importTitle}>
          {(['import', 'manual'] as const).map((key) => (
            <button
              key={key}
              type="button"
              aria-pressed={mode === key}
              onClick={() => onMode(key)}
            >
              {key === 'import' ? <Import size={16} /> : <MonitorCog size={16} />}
              {key === 'import' ? t.appearance.import : t.appearance.manual}
            </button>
          ))}
        </div>
      )}

      {canImport && mode === 'import' ? (
        <section className="onboarding-section" aria-labelledby="terminal-import-title">
          <div className="onboarding-section-heading">
            <div>
              <h2 id="terminal-import-title">{t.appearance.importTitle}</h2>
              <p>{t.appearance.importHint}</p>
            </div>
          </div>
          {state.imports.length ? (
            <div className="onboarding-import-list">
              {state.imports.map((source) => {
                const applied = appliedImportId === source.id;
                const supported = source.support === 'full' || source.support === 'partial';
                return (
                  <div className="onboarding-import-row" key={source.id}>
                    <div
                      className="onboarding-profile-preview"
                      style={{
                        background: source.background ?? 'var(--kt-surface)',
                        color: source.foreground ?? 'var(--kt-text)',
                      }}
                      aria-hidden="true"
                    >
                      <span style={{ color: source.cursor ?? 'var(--kt-accent)' }}>❯</span>
                      <span> kasaterm</span>
                      <span className="onboarding-profile-colors">
                        {(source.ansi16 ?? []).slice(0, 8).map((color, index) => (
                          <i key={`${color}-${index}`} style={{ background: color }} />
                        ))}
                      </span>
                    </div>
                    <div className="onboarding-import-copy">
                      <div className="onboarding-title-line">
                        <strong>{source.label}</strong>
                        {source.detected && <span className="onboarding-tag">{t.appearance.detected}</span>}
                      </div>
                      <p>{source.profile || source.reason || t.appearance.unavailable}</p>
                    </div>
                    <button
                      type="button"
                      className={applied ? 'onboarding-button is-confirmed' : 'onboarding-button'}
                      disabled={busy || !supported || applied}
                      onClick={async () => {
                        if (await run('terminal-profile-import', { id: source.id })) {
                          onImported(source.id);
                        }
                      }}
                    >
                      {applied ? <CheckCircle2 size={16} /> : <Import size={16} />}
                      {applied ? t.appearance.imported : t.appearance.importAction}
                    </button>
                  </div>
                );
              })}
            </div>
          ) : (
            <EmptyState title={t.appearance.importEmpty} hint={t.appearance.importEmptyHint} />
          )}
        </section>
      ) : (
        <>
          <section className="onboarding-section" aria-labelledby="theme-title">
            <div className="onboarding-section-heading">
              <div>
                <h2 id="theme-title">{t.appearance.theme}</h2>
                <p>{t.appearance.themeHint}</p>
              </div>
              <Palette aria-hidden="true" size={19} />
            </div>
            {appearance.themes.length ? (
              <div className="onboarding-theme-grid">
                {appearance.themes.map((theme) => (
                  <ThemeChoice
                    key={theme.key}
                    theme={theme}
                    active={appearance.theme === theme.key}
                    busy={busy}
                    onPick={() => void run('theme-mode', { id: theme.key })}
                  />
                ))}
              </div>
            ) : (
              <EmptyState title={t.appearance.themeEmpty} hint={t.appearance.themeHint} />
            )}
          </section>

          <section className="onboarding-section" aria-labelledby="font-title">
            <div className="onboarding-section-heading">
              <div>
                <h2 id="font-title">{t.appearance.font}</h2>
                <p>{t.appearance.fontHint}</p>
              </div>
              <Type aria-hidden="true" size={19} />
            </div>
            <div className="onboarding-form-row">
              {state.fonts.length ? (
                <label className="onboarding-select-wrap">
                  <span className="sr-only">{t.appearance.font}</span>
                  <select
                    className="onboarding-select"
                    value={state.font_family ?? ''}
                    disabled={busy}
                    onChange={(event) => void run('font-family', { id: event.currentTarget.value })}
                  >
                    {state.font_family && !state.fonts.includes(state.font_family) && (
                      <option value={state.font_family}>{state.font_family}</option>
                    )}
                    {state.fonts.map((font) => (
                      <option key={font} value={font}>{font}</option>
                    ))}
                  </select>
                </label>
              ) : (
                <p className="onboarding-inline-empty">{t.appearance.fontEmpty}</p>
              )}
              <div className="onboarding-size-control">
                <span>{t.appearance.fontSize}</span>
                <Stepper
                  text={`${appearance.font_size}px`}
                  disabled={busy}
                  atMin={appearance.font_size <= 8}
                  atMax={appearance.font_size >= 32}
                  onStep={(delta) => void run('font-size-delta', { id: String(delta) })}
                />
              </div>
            </div>
            <div className="onboarding-accent-row" aria-label={t.appearance.accent}>
              <span>{t.appearance.accent}</span>
              {appearance.accents.map((accent) => (
                <button
                  key={accent.name}
                  type="button"
                  aria-label={accent.name}
                  aria-pressed={appearance.accent === accent.name}
                  disabled={busy}
                  style={{ background: accent.hex }}
                  onClick={() => void run('accent', { id: accent.name })}
                />
              ))}
            </div>
          </section>
        </>
      )}
    </div>
  );
}

function statusCopy(t: OnboardingStrings, status: OnboardingAuthStatus): string {
  if (status === 'logged_in') return t.auth.signedIn;
  if (status === 'logged_out') return t.auth.signedOut;
  if (status === 'checking') return t.auth.checking;
  if (status === 'not_installed') return t.auth.unavailable;
  return t.auth.error;
}

function AuthStatusIcon({ status }: { status: OnboardingAuthStatus }) {
  if (status === 'logged_in') return <CheckCircle2 size={20} />;
  if (status === 'checking') return <LoaderCircle className="onboarding-spin" size={20} />;
  if (status === 'not_installed') return <CircleOff size={20} />;
  return <CircleAlert size={20} />;
}

function AgentRow({
  t,
  provider,
  auth,
  preferred,
  tabStop,
  polling,
  busy,
  onPreferred,
  onLogin,
  onRetry,
}: {
  t: OnboardingStrings;
  provider: AgentProvider;
  auth: OnboardingAuthProvider;
  preferred: boolean;
  tabStop: boolean;
  polling: boolean;
  busy: boolean;
  onPreferred: () => void;
  onLogin: () => void;
  onRetry: () => void;
}) {
  const ready = auth.status === 'logged_in';
  const canLogin = auth.status === 'logged_out' || auth.status === 'error';
  return (
    <div className={`onboarding-agent-row status-${auth.status}`}>
      <button
        type="button"
        className="onboarding-agent-main"
        disabled={!ready}
        role="radio"
        aria-checked={preferred}
        tabIndex={tabStop ? 0 : -1}
        onClick={onPreferred}
      >
        <span className="onboarding-agent-icon"><AuthStatusIcon status={auth.status} /></span>
        <span className="onboarding-agent-copy">
          <strong>{provider === 'claude' ? t.auth.claude : t.auth.codex}</strong>
          <span>{auth.account || auth.detail || statusCopy(t, auth.status)}</span>
        </span>
        <span className="onboarding-agent-status">{statusCopy(t, auth.status)}</span>
        {preferred && <span className="onboarding-tag is-accent">{t.auth.preferred}</span>}
      </button>
      {(canLogin || polling) && (
        <button
          type="button"
          className="onboarding-button"
          disabled={busy || polling}
          onClick={onLogin}
        >
          {polling ? <LoaderCircle className="onboarding-spin" size={16} /> : <LogIn size={16} />}
          {polling ? t.auth.loginWaiting : t.auth.login}
        </button>
      )}
      {auth.status === 'not_installed' && <p className="onboarding-agent-note">{t.auth.unavailableHint}</p>}
      {auth.status === 'error' && !polling && (
        <button type="button" className="onboarding-text-button" onClick={onRetry}>
          <RotateCw size={14} /> {t.auth.retry}
        </button>
      )}
    </div>
  );
}

export function AuthStep({
  t,
  state,
  preferred,
  polling,
  busy,
  onPreferred,
  onLogin,
  onRetry,
}: {
  t: OnboardingStrings;
  state: OnboardingState;
  preferred: AgentProvider | null;
  polling: Record<AgentProvider, boolean>;
  busy: boolean;
  onPreferred: (provider: AgentProvider) => void;
  onLogin: (provider: AgentProvider) => void;
  onRetry: () => void;
}) {
  const readyCount = (['claude', 'codex'] as const).filter(
    (provider) => state.auth[provider].status === 'logged_in'
  ).length;
  const firstReady = (['claude', 'codex'] as const).find(
    (provider) => state.auth[provider].status === 'logged_in'
  );
  return (
    <div className="onboarding-step-stack">
      <section className="onboarding-section onboarding-agent-section" aria-label={t.auth.preferredTitle}>
        <div className="onboarding-section-heading">
          <div>
            <h2>{t.auth.preferredTitle}</h2>
            <p>{readyCount ? t.auth.preferredHint : t.auth.noneReadyHint}</p>
          </div>
          <ShieldCheck aria-hidden="true" size={20} />
        </div>
        <div
          className="onboarding-agent-list"
          role="radiogroup"
          aria-label={t.auth.preferredTitle}
          onKeyDown={moveRadioFocus}
        >
          {(['claude', 'codex'] as const).map((provider) => (
            <AgentRow
              key={provider}
              t={t}
              provider={provider}
              auth={state.auth[provider]}
              preferred={preferred === provider}
              tabStop={preferred === provider || (preferred == null && firstReady === provider)}
              polling={polling[provider]}
              busy={busy}
              onPreferred={() => onPreferred(provider)}
              onLogin={() => onLogin(provider)}
              onRetry={onRetry}
            />
          ))}
        </div>
        {!readyCount && <p className="onboarding-soft-note">{t.auth.noneReady}</p>}
      </section>
      <p className="onboarding-privacy"><ShieldCheck size={15} />{t.auth.privacy}</p>
    </div>
  );
}

export function PlatformStep({
  t,
  state,
  appearance,
  shell,
  appliedImportId,
  busy,
  run,
}: {
  t: OnboardingStrings;
  state: OnboardingState;
  appearance: AppearanceValues;
  shell: string;
  appliedImportId: string | null;
  busy: boolean;
  run: RunOnboardingAction;
}) {
  if (state.platform === 'windows') {
    const hasSelectedShell = state.windows_shells.some(
      (choice) => choice.id === state.selected_shell
    );
    return (
      <section className="onboarding-section" aria-labelledby="windows-shell-title">
        <div className="onboarding-section-heading">
          <div>
            <h2 id="windows-shell-title">{t.platform.windowsTitle}</h2>
            <p>{t.platform.windowsHint}</p>
          </div>
          <Terminal aria-hidden="true" size={20} />
        </div>
        {state.windows_shells.length ? (
          <div
            className="onboarding-shell-list"
            role="radiogroup"
            aria-label={t.platform.windowsTitle}
            onKeyDown={moveRadioFocus}
          >
            {state.windows_shells.map((choice, index) => {
              const selected = state.selected_shell === choice.id;
              return (
                <button
                  key={choice.id}
                  type="button"
                  className="onboarding-shell-row"
                  role="radio"
                  aria-checked={selected}
                  tabIndex={selected || (!hasSelectedShell && index === 0) ? 0 : -1}
                  disabled={busy}
                  onClick={() => !selected && void run('default-shell', { id: choice.id })}
                >
                  <span className="onboarding-radio" aria-hidden="true">{selected && <span />}</span>
                  <span><strong>{choice.label}</strong><small>{choice.path}</small></span>
                  {choice.detected && <span className="onboarding-tag">{t.platform.detected}</span>}
                </button>
              );
            })}
          </div>
        ) : (
          <EmptyState title={t.platform.shellEmpty} hint={t.platform.shellEmptyHint} />
        )}
        <div className="onboarding-custom-shell">
          <label htmlFor="custom-shell">{t.platform.customShell}</label>
          <TextField
            id="custom-shell"
            label={t.platform.customShell}
            value={shell}
            disabled={busy}
            mono
            className="w-full"
            placeholder={t.platform.customPlaceholder}
            onCommit={(next) => void run('shell-custom', { label: next })}
          />
        </div>
      </section>
    );
  }

  const imported = state.imports.find((source) => source.id === appliedImportId);
  const theme = appearance.themes.find((item) => item.key === appearance.theme)?.label ?? appearance.theme;
  return (
    <section className="onboarding-section" aria-labelledby="platform-confirm-title">
      <div className="onboarding-section-heading">
        <div>
          <h2 id="platform-confirm-title">
            {state.platform === 'macos' ? t.platform.macTitle : t.platform.shellTitle}
          </h2>
          <p>{state.platform === 'macos' ? t.platform.macHint : t.platform.linuxHint}</p>
        </div>
        <Terminal aria-hidden="true" size={20} />
      </div>
      <dl className="onboarding-summary-list">
        {state.platform === 'macos' && (
          <div><dt>{t.platform.importedFrom}</dt><dd>{imported?.label ?? t.platform.notImported}</dd></div>
        )}
        <div><dt>{t.platform.currentTheme}</dt><dd>{theme}</dd></div>
        <div><dt>{t.platform.currentFont}</dt><dd>{state.font_family || t.platform.systemFont} · {appearance.font_size}px</dd></div>
        <div><dt>{t.platform.shellTitle}</dt><dd>{shell || t.platform.systemDefault}</dd></div>
      </dl>
    </section>
  );
}

export function ReadyStep({
  t,
  state,
  appearance,
  shell,
  preferred,
}: {
  t: OnboardingStrings;
  state: OnboardingState;
  appearance: AppearanceValues;
  shell: string;
  preferred: AgentProvider | null;
}) {
  const theme = appearance.themes.find((item) => item.key === appearance.theme)?.label ?? appearance.theme;
  const signedIn = (['claude', 'codex'] as const).filter(
    (provider) => state.auth[provider].status === 'logged_in'
  ).length;
  const shellLabel = state.platform === 'windows'
    ? state.windows_shells.find((item) => item.id === state.selected_shell)?.label
    : shell;
  return (
    <div className="onboarding-ready">
      <div className="onboarding-ready-mark" aria-hidden="true"><Check size={34} /></div>
      <dl className="onboarding-ready-list">
        <div><dt><Palette size={17} />{t.ready.appearance}</dt><dd>{theme} · {state.font_family || t.platform.systemFont} {appearance.font_size}px</dd></div>
        <div><dt><ShieldCheck size={17} />{t.ready.agent}</dt><dd>{signedIn ? `${preferred === 'claude' ? t.auth.claude : preferred === 'codex' ? t.auth.codex : t.ready.signedInCount(signedIn)} · ${t.auth.preferred}` : t.ready.noAgent}</dd></div>
        <div><dt><Terminal size={17} />{t.ready.terminal}</dt><dd>{shellLabel || t.platform.systemDefault}</dd></div>
      </dl>
      {state.restart_required && (
        <p className="onboarding-restart-note"><CircleAlert size={15} />{t.ready.restart}</p>
      )}
    </div>
  );
}
