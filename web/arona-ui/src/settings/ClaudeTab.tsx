import { useState } from 'react';
import {
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
import type { AccountRow, ClaudeValues } from './types';

/// 부제 색. 「로그인 필요」는 경고, 이메일은 보통, 「확인 중…」은 더 흐리게 —
/// 아직 모른다는 것과 없다는 것을 색으로 가른다.
const SUB_COLOR: Record<string, string> = {
  danger: 'var(--kt-danger)',
  mute: 'var(--kt-text-mute)',
  faint: 'color-mix(in srgb, var(--kt-text-mute) 60%, transparent)',
};

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
  onRemove,
}: {
  row: AccountRow;
  active: boolean;
  busy: boolean;
  onSelect: () => void;
  onRename: (label: string) => void;
  onReauth: () => void;
  onRemove: () => void;
}) {
  const [renaming, setRenaming] = useState(false);
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
            value={row.label}
            disabled={busy}
            placeholder="별명 (비우면 이메일로 불러요)"
            className="w-full max-w-[280px]"
            onCommit={(next) => {
              setRenaming(false);
              onRename(next);
            }}
          />
        ) : (
          <>
            <div className="truncate text-[13px] text-[var(--kt-text)]">{row.name}</div>
            {row.sub && (
              <div
                className="truncate text-[12px]"
                style={{ color: SUB_COLOR[row.sub_kind] ?? 'var(--kt-text-mute)' }}
              >
                {row.sub}
              </div>
            )}
          </>
        )}
      </div>
      {/* 첫 행(지금 로그인)은 우리가 만든 슬롯이 아니라 지울 것도 이름 붙일 것도
          없다 — 그래서 버튼 자체를 안 그린다. */}
      {row.slot && !renaming && (
        <div className="invisible flex shrink-0 gap-1 group-focus-within:visible group-hover:visible">
          <MiniButton label="이름" disabled={busy} onClick={() => setRenaming(true)} />
          <MiniButton label="다시 로그인" disabled={busy} onClick={onReauth} />
          <MiniButton label="빼기" danger disabled={busy} onClick={onRemove} />
        </div>
      )}
      {active && !renaming && (
        <span className="shrink-0 text-[11px] text-[var(--kt-text-mute)]">쓰는 중</span>
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
  const { busy, notice, run } = useSettingsAction(reload);
  // 계정이 하나뿐이면 자동 전환이 갈 곳이 없어 아무 일도 안 일어난다. 켜 놓고
  // "안 되네" 하는 게 이 기능에서 제일 흔한 오해라, 그 상태를 미리 말해 준다.
  const lone = data.accounts.filter((a) => a.slot).length === 0;

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
          onSelect={() => void run(`${provider}-account`, { id: row.id })}
          onRename={(label) => void run(`${provider}-account-label`, { id: row.id, label })}
          onReauth={() => void run('reauth-account', { id: row.id, label: provider })}
          onRemove={() => void run(`remove-${provider}-account`, { id: row.id })}
        />
      ))}
      <MiniButton
        label={`+ ${provider === 'claude' ? 'Claude' : 'Codex'} 계정 추가`}
        disabled={busy}
        onClick={() => void run(`add-${provider}-account`)}
      />
    </>
  );

  return (
    <TabCard>
      <Notice notice={notice} />

      <Row
        label="Shim injection"
        desc={['끄면 순정 Claude — 페르소나 · 프록시 · 훅 없음 (재시작 필요)']}
      >
        <Toggle
          on={data.shim_inject}
          disabled={busy}
          onToggle={() => void run('toggle-shim-inject')}
        />
      </Row>

      <Row label="Persona injection" desc={['이 pane 의 캐릭터를 Claude 시스템 프롬프트에 붙여요']}>
        <Toggle on={data.persona} disabled={busy} onToggle={() => void run('toggle-persona')} />
      </Row>

      <Section
        title="Account"
        hint="다음에 뜨는 claude 부터 이 계정으로 — 돌고 있는 세션은 그대로예요"
      >
        {accountList('claude', data.accounts, data.account)}
      </Section>

      <Row
        label="Auto switch"
        desc={[
          lone
            ? '계정이 하나뿐이라 지금은 넘어갈 곳이 없어요'
            : '한도가 차면 다음에 뜨는 claude 부터 다음 계정으로 — 떠난 계정은 풀릴 때까지 쉬어요',
        ]}
      >
        <Toggle
          on={data.autoswitch}
          disabled={busy}
          onToggle={() => void run('toggle-account-autoswitch')}
        />
      </Row>
      {data.autoswitch && (
        <Row label="Switch at" desc={['이 사용률을 넘으면 다음 계정으로 넘어가요']}>
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
      <Section
        title="Codex account"
        hint="다음에 뜨는 codex 부터 이 계정으로 — 돌고 있는 세션은 그대로예요"
      >
        {accountList('codex', data.codex_accounts, data.codex_account)}
      </Section>

      <Row label="Model" desc={['Claude 모델 덮어쓰기 (Default = 원래대로 유지)']}>
        <Segmented
          value={data.model}
          disabled={busy}
          options={[
            { key: '', label: 'Default' },
            { key: 'opus', label: 'opus' },
            { key: 'sonnet', label: 'sonnet' },
            { key: 'haiku', label: 'haiku' },
          ]}
          onPick={(key) => void run('claude-model', { id: key })}
        />
      </Row>

      <Row label="Effort" desc={['추론 강도 — Default 는 그대로 둬요']}>
        <Segmented
          value={data.effort}
          disabled={busy}
          options={[
            { key: '', label: 'Default' },
            { key: 'low', label: 'low' },
            { key: 'medium', label: 'medium' },
            { key: 'high', label: 'high' },
            { key: 'xhigh', label: 'xhigh' },
          ]}
          onPick={(key) => void run('claude-effort', { id: key })}
        />
      </Row>

      <Section title="Extra args" hint="claude 실행에 항상 붙는 플래그 (예: --verbose)">
        <TextField
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
