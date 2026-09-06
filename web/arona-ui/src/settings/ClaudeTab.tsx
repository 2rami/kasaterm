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

/// 부제 색. 「로그인 필요」는 경고, 이메일은 보통, 「확인 중…」은 더 흐리게 —
/// 아직 모른다는 것과 없다는 것을 색으로 가른다.
const SUB_COLOR: Record<string, string> = {
  danger: 'var(--kt-danger-text-surface)',
  mute: 'var(--kt-text-mute)',
  faint: 'color-mix(in srgb, var(--kt-text-mute) 60%, transparent)',
};

/// 하단바와 같은 임계 — 90 위험, 70 주의, 그 밑은 중립(초록으로 안심시키지 않는다).
function usageColor(p: number): string {
  if (p >= 90) return 'var(--kt-danger-text-surface)';
  if (p >= 70) return 'var(--kt-attention)';
  return 'var(--kt-text-mute)';
}

/// 계정 한 줄. 카드 전체가 「이 계정 쓰기」이고 관리 버튼은 hover 때만 보인다 —
/// 네이티브와 같은 규칙이다. `invisible` 로 숨기는 게 `opacity-0` 보다 맞다:
/// 투명한 버튼은 여전히 눌리고 탭으로도 잡힌다.
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
  return (
    <div
      className="group relative mb-1.5 flex items-center gap-3 px-4 py-2.5"
      // 안에 관리 버튼이 들어가므로 `<button>` 으로 만들 수 없다(버튼 중첩은 잘못된
      // HTML). role/tabIndex 로 같은 조작을 준다.
      role="button"
      tabIndex={0}
      aria-current={active}
      onClick={() => !active && onSelect()}
      onKeyDown={(e) => {
        if ((e.key === 'Enter' || e.key === ' ') && e.target === e.currentTarget) {
          e.preventDefault();
          if (!active) onSelect();
        }
      }}
      style={{
        borderRadius: 'var(--kt-radius-md)',
        background: active ? 'var(--kt-surface-active)' : 'var(--kt-surface)',
        boxShadow: 'inset 0 0 0 var(--kt-border-w) var(--kt-border)',
        cursor: active ? 'default' : 'pointer',
      }}
    >
      {active && (
        <span
          className="absolute left-0 top-0 h-full w-[3px]"
          style={{ background: 'var(--kt-accent)' }}
        />
      )}
      <div className="min-w-0 flex-1">
        {renaming ? (
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
        ) : (
          <>
            <div className="truncate text-[13px] text-[var(--kt-text)]">{name}</div>
            {sub && (
              <div
                className="truncate text-[12px]"
                style={{ color: SUB_COLOR[row.sub_kind] ?? 'var(--kt-text-mute)' }}
              >
                {sub}
              </div>
            )}
          </>
        )}
      </div>
      {/* 첫 행(지금 로그인)은 우리가 만든 슬롯이 아니라 지울 것도 이름 붙일 것도
          없다 — 그래서 버튼 자체를 안 그린다. */}
      {row.slot && !renaming && (
        <div className="invisible flex shrink-0 gap-1 group-focus-within:visible group-hover:visible">
          <MiniButton label={t.claude.rename} disabled={busy} onClick={() => setRenaming(true)} />
          {/* 네이티브 카드와 같은 순서 — 다시 로그인(쓰던 브라우저), 빈 창, 빼기. */}
          <MiniButton label={t.claude.reauth} disabled={busy} onClick={onReauth} />
          <MiniButton
            label={t.claude.reauthIsolated}
            disabled={busy}
            onClick={onReauthIsolated}
          />
          <MiniButton label={t.claude.removeSlot} danger disabled={busy} onClick={onRemove} />
        </div>
      )}
      {/* 한도 — 하단바가 쓰는 우물 그대로라 열자마자 뜬다(2026-08-31 지적
          「하단바랑 다르게 사용량 바로 안 뜨고」). `~` 는 낡은 값(하단바와 같은
          문법). 값이 없으면 아무것도 안 그린다 — 0% 는 여유 있다는 거짓말이다. */}
      {row.usage != null && !renaming && (
        <span
          className="shrink-0 text-[12px] tabular-nums"
          title={row.usage_label ?? undefined}
          style={{ color: usageColor(row.usage), opacity: row.usage_stale ? 0.65 : 1 }}
        >
          {row.usage_stale ? '~' : ''}
          {Math.round(row.usage)}%
          {row.usage_resets ? ` · ${row.usage_resets}` : ''}
        </span>
      )}
      {active && !renaming && (
        <span className="shrink-0 text-[11px] text-[var(--kt-text-mute)]">{t.claude.inUse}</span>
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
  // 계정이 하나뿐이면 자동 전환이 갈 곳이 없어 아무 일도 안 일어난다. 켜 놓고
  // "안 되네" 하는 게 이 기능에서 제일 흔한 오해라, 그 상태를 미리 말해 준다.
  const lone = data.accounts.filter((a) => a.slot).length === 0;

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
      {rows.map((row) => (
        <AccountCard
          key={row.id || '(default)'}
          row={row}
          active={row.id === activeId}
          busy={busy}
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
        {accountList('claude', data.accounts, data.account)}
      </Section>

      <Row
        label={t.claude.autoSwitch}
        desc={[lone ? t.claude.autoSwitchLone : t.claude.autoSwitchHint]}
      >
        <Toggle
          label={t.claude.autoSwitch}
          on={data.autoswitch}
          disabled={busy}
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
