import { useRef, useState } from 'react';
import {
  Button,
  MiniButton,
  Notice,
  Row,
  Section,
  Segmented,
  TabCard,
  TextField,
  Toggle,
  useSettingsAction,
} from './controls';
import { serverText, useT } from './lang';
import type { AccountRow, ClaudeValues } from './types';
import type { AccountSwitchConfirmation } from './api';

/// 하단바와 같은 임계 — 90 위험, 70 주의, 그 밑은 중립(초록으로 안심시키지 않는다).
function usageColor(p: number): string {
  if (p >= 90) return 'var(--kt-danger-text-surface)';
  if (p >= 70) return 'var(--kt-attention)';
  return 'var(--kt-text-mute)';
}

function AccountCard({
  row,
  active,
  busy,
  onSelect,
  onRename,
  onReauth,
  onReauthIsolated,
  onRemove,
}: {
  row: AccountRow;
  active: boolean;
  busy: boolean;
  onSelect: () => void;
  onRename: (label: string) => void;
  onReauth: () => void;
  onReauthIsolated: () => void;
  onRemove: () => void;
}) {
  const t = useT();
  const [renaming, setRenaming] = useState(false);
  // 이름·부제는 코드가 붙어 온 것만 옮긴다. 코드가 없으면 그 자리는 **데이터**다 —
  // 사용자가 붙인 별명, 이메일, 팀 조직명이라 옮길 말이 아니다.
  const name = serverText(t, row.name_code, row.name, row.name_args ?? undefined);
  const sub = serverText(t, row.sub_code, row.sub);
  const loginRequired = row.logged_in === false || row.usage_state === 'logged_out'
    || row.sub_code === 'account_login_required';
  const failed = row.usage_state === 'failed';
  const hasUsage = !loginRequired && row.usage != null && Number.isFinite(row.usage);
  const stale = hasUsage && (row.usage_stale === true || failed);
  const status = loginRequired ? t.claude.loginRequired
    : failed ? t.claude.lookupFailed
      : stale ? t.claude.previousLookup
        : hasUsage ? t.claude.usageCurrent : t.claude.checking;
  const identity = row.sub_code ? '' : sub;
  return (
    <div
      className="kt-account-row"
      aria-busy={busy}
      style={{
        background: active ? 'var(--kt-surface-active)' : 'var(--kt-surface)',
      }}
    >
      {renaming ? (
        <div className="p-3">
          <TextField
            label={t.claude.labelPlaceholder}
            value={row.label}
            disabled={busy}
            placeholder={t.claude.labelPlaceholder}
            className="w-full max-w-[280px]"
            onCommit={onRename}
            // 폼 닫기는 onDone 에서 — onCommit 은 값이 바뀔 때만 불려서, 그대로
            // 나가기·Esc 면 폼이 영영 남았다(설정 창을 껐다 켜야 풀리던 문제).
            onDone={() => setRenaming(false)}
          />
        </div>
      ) : (
        <button
          type="button"
          className="kt-account-select"
          aria-pressed={active}
          disabled={busy}
          onClick={() => { if (!active) onSelect(); }}
        >
          <span className="kt-account-heading">
            <span className="kt-account-name">{name}</span>
            <span className="kt-account-selected">{active ? t.claude.inUse : t.claude.selectAccount}</span>
          </span>
          {identity && <span className="kt-account-identity">{identity}</span>}
          <span className="kt-account-status">
            <span style={{ color: loginRequired || failed ? 'var(--kt-danger-text-surface)' : undefined }}>
              {status}
            </span>
            {hasUsage && (
              <span className="tabular-nums" title={row.usage_label ?? undefined}
                style={{ color: stale ? undefined : usageColor(row.usage!) }}>
                {stale && failed ? `${t.claude.previousLookup} ` : ''}
                {row.usage_label ? `${row.usage_label} ` : ''}{Math.round(row.usage!)}%
                {!stale && row.usage_resets ? ` · ${row.usage_resets}` : ''}
              </span>
            )}
          </span>
        </button>
      )}
      {!renaming && (
        <div className="kt-account-actions" role="group" aria-label={`${name} · ${t.claude.manageAccount}`}>
          {row.slot && (
            <MiniButton label={t.claude.rename} disabled={busy} onClick={() => setRenaming(true)} />
          )}
          <MiniButton label={t.claude.reauth} disabled={busy} onClick={onReauth} />
          {row.slot && (
            <>
              <MiniButton
                label={t.claude.reauthIsolated}
                disabled={busy}
                onClick={onReauthIsolated}
              />
              <MiniButton label={t.claude.removeSlot} danger disabled={busy} onClick={onRemove} />
            </>
          )}
        </div>
      )}
    </div>
  );
}

export function ClaudeTab({
  data,
  reload,
}: {
  data: ClaudeValues;
  reload: () => Promise<void>;
}) {
  const t = useT();
  const { busy, notice, run, runResult } = useSettingsAction(reload);
  const [confirm, setConfirm] = useState<AccountSwitchConfirmation | null>(null);
  const confirmDialog = useRef<HTMLDivElement>(null);
  const confirmTrigger = useRef<HTMLElement | null>(null);
  const claudeAccounts = data.accounts.filter((row) => row.id !== '');
  const lone = claudeAccounts.length < 2;

  const selectAccount = async (provider: 'claude' | 'codex', id: string) => {
    confirmTrigger.current = document.activeElement as HTMLElement | null;
    const out = await runResult(`${provider}-account`, { id });
    if (out?.confirm) setConfirm(out.confirm);
  };

  const resolveAccount = async (accept: boolean) => {
    const pending = confirm;
    if (!pending) return;
    const out = await runResult(
      accept ? 'confirm-account-switch' : 'cancel-account-switch',
      { id: pending.id, label: `${pending.provider}:${pending.nonce}` }
    );
    if (out?.confirm) {
      setConfirm(out.confirm);
    } else if (out?.ok && !out.error) {
      setConfirm(null);
      window.requestAnimationFrame(() => confirmTrigger.current?.focus());
    }
  };

  const accountList = (
    provider: 'claude' | 'codex',
    rows: AccountRow[],
    activeId: string
  ) => (
    <>
      {provider === 'claude' && !rows.length && (
        <p className="kt-account-empty">{t.claude.noAccounts}</p>
      )}
      {provider === 'claude' && rows.length > 0 && !rows.some((row) => row.id === activeId) && (
        <p className="kt-account-empty">{t.claude.noSelection}</p>
      )}
      {rows.map((row) => (
        <AccountCard
          key={row.id || '(default)'}
          row={row}
          active={row.id === activeId}
          busy={busy || confirm !== null}
          onSelect={() => void selectAccount(provider, row.id)}
          onRename={(label) => void run(`${provider}-account-label`, { id: row.id, label })}
          onReauth={() => void run('reauth-account', { id: row.id, label: provider })}
          onReauthIsolated={() =>
            void run('reauth-account-isolated', { id: row.id, label: provider })
          }
          onRemove={() => void run(`remove-${provider}-account`, { id: row.id })}
        />
      ))}
      <MiniButton
        label={t.claude.addAccount({ provider: provider === 'claude' ? 'Claude' : 'Codex' })}
        disabled={busy}
        onClick={() => void run(`add-${provider}-account`)}
      />
      {/* 슬롯이 있을 때만 — 관리 버튼이 없는 화면에서는 설명할 것도 없다. */}
      {rows.some((r) => r.slot) && (
        <p className="mt-2 text-[12px] text-[var(--kt-text-mute)]">{t.claude.browserHint}</p>
      )}
    </>
  );

  return (
    <TabCard>
      <Notice notice={notice} />

      {confirm && (
        <div
          ref={confirmDialog}
          className="fixed inset-0 z-[100] flex items-center justify-center bg-black/60 p-6"
          role="alertdialog"
          aria-modal="true"
          aria-labelledby="account-switch-title"
          aria-describedby="account-switch-description"
          onKeyDown={(e) => {
            if (e.key === 'Escape') {
              e.preventDefault();
              e.stopPropagation();
              void resolveAccount(false);
              return;
            }
            if (e.key === 'Tab') {
              const buttons = Array.from(
                confirmDialog.current?.querySelectorAll<HTMLButtonElement>('button:not(:disabled)') ?? []
              );
              if (!buttons.length) return;
              const first = buttons[0];
              const last = buttons[buttons.length - 1];
              if (e.shiftKey && document.activeElement === first) {
                e.preventDefault();
                last.focus();
              } else if (!e.shiftKey && document.activeElement === last) {
                e.preventDefault();
                first.focus();
              }
            }
          }}
        >
          <div
            className="w-full max-w-[520px] p-6"
            style={{
              borderRadius: 'var(--kt-radius-md)',
              background: 'var(--kt-surface)',
              boxShadow: 'inset 0 0 0 var(--kt-border-w) var(--kt-border), 0 18px 48px rgba(0,0,0,.45)',
            }}
          >
            <h2 id="account-switch-title" className="text-[18px] font-semibold text-[var(--kt-text)]">
              {confirm.title}
            </h2>
            <div id="account-switch-description" className="mt-3 space-y-1.5">
              {confirm.lines.map((line) => (
                <p key={line} className="text-[13px] leading-relaxed text-[var(--kt-text-dim)]">
                  {line}
                </p>
              ))}
            </div>
            <div className="mt-6 flex justify-end gap-2">
              <Button
                label={t.common.cancel}
                disabled={busy}
                autoFocus
                onClick={() => void resolveAccount(false)}
              />
              <button
                type="button"
                disabled={busy}
                onClick={() => void resolveAccount(true)}
                className="min-h-[40px] px-3.5 py-2 text-[13px] font-semibold disabled:opacity-40"
                style={{
                  borderRadius: 'var(--kt-radius-md)',
                  background: confirm.dangerous ? 'var(--kt-danger)' : 'var(--kt-accent)',
                  color: confirm.dangerous ? 'var(--kt-on-danger)' : 'var(--kt-on-accent)',
                }}
              >
                {t.common.switch}
              </button>
            </div>
          </div>
        </div>
      )}

      <Row label={t.claude.shim} desc={[t.claude.shimHint]}>
        <Toggle
          label={t.claude.shim}
          on={data.shim_inject}
          disabled={busy}
          onToggle={() => void run('toggle-shim-inject')}
        />
      </Row>

      <Section title={t.claude.account} hint={t.claude.accountHint}>
        {accountList('claude', claudeAccounts, data.account)}
      </Section>

      <Row
        label={t.claude.autoSwitch}
        desc={[lone ? t.claude.autoSwitchLone : t.claude.autoSwitchHint]}
      >
        <Toggle
          label={t.claude.autoSwitch}
          on={data.autoswitch}
          disabled={busy || (lone && !data.autoswitch)}
          onToggle={() => void run('toggle-account-autoswitch')}
        />
      </Row>
      {data.autoswitch && (
        <Row label={t.claude.switchAt} desc={[t.claude.switchAtHint]}>
          <Segmented
            value={String(data.autoswitch_pct)}
            disabled={busy}
            options={[80, 85, 90, 95].map((p) => ({ key: String(p), label: `${p}%` }))}
            onPick={(key) => void run('autoswitch-pct', { id: key })}
          />
        </Row>
      )}

      {/* codex 슬롯을 claude 바로 아래 둔다 — pane 에서 codex 를 띄우는 것도 같은
          손이라, 두 로그인이 설정의 다른 층에 흩어져 있으면 「지금 어느 계정으로
          돌고 있나」를 두 군데서 확인해야 한다. */}
      <Section title={t.claude.codexAccount} hint={t.claude.codexAccountHint}>
        {accountList('codex', data.codex_accounts, data.codex_account)}
      </Section>

      <Row label={t.claude.model} desc={[t.claude.modelHint]}>
        <Segmented
          value={data.model}
          disabled={busy}
          options={[
            { key: '', label: t.claude.optionDefault },
            { key: 'opus', label: 'opus' },
            { key: 'sonnet', label: 'sonnet' },
            { key: 'haiku', label: 'haiku' },
          ]}
          onPick={(key) => void run('claude-model', { id: key })}
        />
      </Row>

      <Row label={t.claude.effort} desc={[t.claude.effortHint]}>
        <Segmented
          value={data.effort}
          disabled={busy}
          options={[
            { key: '', label: t.claude.optionDefault },
            { key: 'low', label: 'low' },
            { key: 'medium', label: 'medium' },
            { key: 'high', label: 'high' },
            { key: 'xhigh', label: 'xhigh' },
          ]}
          onPick={(key) => void run('claude-effort', { id: key })}
        />
      </Row>

      <Section title={t.claude.extraArgs} hint={t.claude.extraArgsHint}>
        <TextField
          label={t.claude.extraArgs}
          value={data.extra}
          disabled={busy}
          mono
          placeholder="--verbose"
          onCommit={(next) => void run('claude-extra', { label: next })}
        />
      </Section>
    </TabCard>
  );
}
