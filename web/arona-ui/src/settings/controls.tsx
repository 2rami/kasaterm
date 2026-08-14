import { useState } from 'react';
import { postAction } from './api';

/// 카드 안의 소제목 + 설명. 네이티브 폼의 `row_wide` 대응 — 제목만 크게 하고 설명은
/// dim 으로 한 줄 아래 둔다. `right` 는 제목 줄 오른쪽에 붙는 컨트롤(스위치처럼
/// 설명과 나란히 서야 하는 것).
export function Section({
  title,
  hint,
  right,
  children,
}: {
  title: string;
  hint?: string;
  right?: React.ReactNode;
  children?: React.ReactNode;
}) {
  return (
    <section className="mb-7 last:mb-0">
      <div className="flex items-start justify-between gap-4">
        <div>
          <h2 className="text-[15px] font-semibold text-[var(--kt-text)]">{title}</h2>
          {hint && <p className="mt-0.5 text-[13px] text-[var(--kt-text-mute)]">{hint}</p>}
        </div>
        {right}
      </div>
      {children && <div className="mt-3">{children}</div>}
    </section>
  );
}

/// 카드·목록에서 같은 모양으로 쓰는 작은 버튼.
export function MiniButton({
  label,
  onClick,
  disabled,
  danger,
}: {
  label: string;
  onClick: () => void;
  disabled?: boolean;
  danger?: boolean;
}) {
  return (
    <button
      type="button"
      disabled={disabled}
      onClick={(e) => {
        // 카드 안에 놓이는 버튼이라 — 막지 않으면 버튼 하나 누를 때마다 그 카드의
        // 「고르기」까지 함께 돈다.
        e.stopPropagation();
        onClick();
      }}
      className="px-2 py-1 text-[11px] disabled:opacity-40"
      style={{
        borderRadius: 'var(--kt-radius-sm)',
        background: 'var(--kt-surface-hover)',
        color: danger ? 'var(--kt-danger)' : 'var(--kt-text)',
        boxShadow: 'inset 0 0 0 var(--kt-border-w) var(--kt-border)',
      }}
    >
      {label}
    </button>
  );
}

/// 라벨·설명이 왼쪽, 컨트롤이 오른쪽 — 네이티브 `row2` 대응. 설명은 여러 줄이 올 수
/// 있어 배열로 받는다(네이티브도 `&[&str]`).
///
/// 좁은 창에서 세로로 접는다. 가로로 버티면 세그먼트가 라벨을 밀어 폼이 창 밖으로
/// 나가는데, 설정은 사용자가 자유롭게 창을 줄이는 화면이다.
export function Row({
  label,
  desc,
  children,
}: {
  label: string;
  desc?: string[];
  children: React.ReactNode;
}) {
  return (
    <div className="mb-4 flex flex-wrap items-start justify-between gap-x-6 gap-y-2 last:mb-0">
      <div className="min-w-[180px] flex-1">
        <div className="text-[13px] text-[var(--kt-text)]">{label}</div>
        {desc?.map((d) => (
          <p key={d} className="mt-0.5 text-[12px] text-[var(--kt-text-mute)]">
            {d}
          </p>
        ))}
      </div>
      <div className="shrink-0">{children}</div>
    </div>
  );
}

/// 세그먼트 컨트롤 — 네이티브 `segmented`. 고른 칸만 채워지고 나머지는 트랙 위에
/// 얹힌다.
export function Segmented({
  value,
  options,
  onPick,
  disabled,
}: {
  value: string;
  options: { key: string; label: string }[];
  onPick: (key: string) => void;
  disabled?: boolean;
}) {
  return (
    <div
      className="inline-flex p-[2px]"
      style={{
        borderRadius: 'var(--kt-radius-md)',
        background: 'var(--kt-surface)',
        boxShadow: 'inset 0 0 0 var(--kt-border-w) var(--kt-border)',
      }}
    >
      {options.map((o) => {
        const on = o.key === value;
        return (
          <button
            key={o.key}
            type="button"
            disabled={disabled}
            aria-pressed={on}
            onClick={() => !on && onPick(o.key)}
            className="whitespace-nowrap px-3 py-1 text-[12.5px] disabled:opacity-40"
            style={{
              borderRadius: 'calc(var(--kt-radius-md) - 2px)',
              background: on ? 'var(--kt-surface-active)' : 'transparent',
              color: on ? 'var(--kt-text)' : 'var(--kt-text-dim)',
              fontWeight: on ? 600 : 400,
            }}
          >
            {o.label}
          </button>
        );
      })}
    </div>
  );
}

/// 스위치 — 네이티브 `toggle`.
export function Toggle({
  on,
  onToggle,
  disabled,
}: {
  on: boolean;
  onToggle: () => void;
  disabled?: boolean;
}) {
  return (
    <button
      type="button"
      disabled={disabled}
      onClick={onToggle}
      aria-pressed={on}
      className="relative inline-block h-[22px] w-[40px] shrink-0 disabled:opacity-40"
      style={{
        borderRadius: '11px',
        background: on ? 'var(--kt-accent)' : 'var(--kt-surface-hover)',
      }}
    >
      <span
        className="absolute top-[3px] h-[16px] w-[16px] bg-white transition-all"
        style={{ borderRadius: 'var(--kt-dot-radius)', left: on ? '21px' : '3px' }}
      />
    </button>
  );
}

/// 단일라인 입력. **칸을 벗어나거나 Enter 를 누를 때** 굳는다 — 네이티브 텍스트
/// 필드와 같은 시점이다.
///
/// 값을 `defaultValue` 로 주고 `key` 로 갈아 끼우는 이유: 매 글자를 상위 상태로
/// 올리면 저장 왕복마다 커서가 끝으로 튄다. 서버 값이 바뀌면 key 가 바뀌어 칸이
/// 새로 서고, 그때만 화면이 파일을 따라간다.
export function TextField({
  value,
  onCommit,
  disabled,
  placeholder,
  mono,
  className,
}: {
  value: string;
  onCommit: (next: string) => void;
  disabled?: boolean;
  placeholder?: string;
  mono?: boolean;
  className?: string;
}) {
  return (
    <input
      key={value}
      className={`kt-field ${className ?? 'w-full max-w-[420px]'}`}
      style={mono ? { fontFamily: 'var(--kt-font-mono)' } : undefined}
      defaultValue={value}
      disabled={disabled}
      placeholder={placeholder}
      onBlur={(e) => {
        const next = e.currentTarget.value;
        if (next !== value) onCommit(next);
      }}
      onKeyDown={(e) => {
        // Esc 는 이 칸의 되돌리기다. 설정 창 자체를 닫는 전역 Esc 가 위에 걸려
        // 있어서, 막지 않으면 편집을 되돌리려다 창이 통째로 닫힌다.
        e.stopPropagation();
        if (e.key === 'Enter') e.currentTarget.blur();
        if (e.key === 'Escape') {
          // 값을 먼저 되돌려야 뒤따르는 blur 가 편집본을 저장하지 않는다.
          e.currentTarget.value = value;
          e.currentTarget.blur();
        }
      }}
    />
  );
}

/// −/값/+ 스테퍼 — 네이티브 `stepper_btn` 한 벌.
export function Stepper({
  text,
  onStep,
  disabled,
  right,
}: {
  text: string;
  onStep: (d: -1 | 1) => void;
  disabled?: boolean;
  right?: React.ReactNode;
}) {
  const box = {
    borderRadius: 'var(--kt-radius-md)',
    background: 'var(--kt-surface-hover)',
    boxShadow: 'inset 0 0 0 var(--kt-border-w) var(--kt-border)',
    color: 'var(--kt-text)',
  };
  return (
    <div className="flex items-center gap-2">
      <button
        type="button"
        disabled={disabled}
        onClick={() => onStep(-1)}
        aria-label="한 칸 줄이기"
        className="h-[30px] w-[30px] text-[15px] leading-none disabled:opacity-40"
        style={box}
      >
        −
      </button>
      <span className="min-w-[52px] text-center text-[15px] font-semibold text-[var(--kt-text)]">
        {text}
      </span>
      <button
        type="button"
        disabled={disabled}
        onClick={() => onStep(1)}
        aria-label="한 칸 늘리기"
        className="h-[30px] w-[30px] text-[15px] leading-none disabled:opacity-40"
        style={box}
      >
        +
      </button>
      {right}
    </div>
  );
}

/// 폼 안의 보통 버튼. `primary` 는 그 화면에서 실제로 무언가를 굳히는 하나에만.
export function Button({
  label,
  onClick,
  disabled,
  primary,
}: {
  label: string;
  onClick: () => void;
  disabled?: boolean;
  primary?: boolean;
}) {
  return (
    <button
      type="button"
      disabled={disabled}
      onClick={onClick}
      className="px-3.5 py-2 text-[13px] disabled:opacity-40"
      style={{
        borderRadius: 'var(--kt-radius-md)',
        background: primary ? 'var(--kt-accent)' : 'var(--kt-surface-hover)',
        color: primary ? 'var(--kt-bg)' : 'var(--kt-text)',
        fontWeight: primary ? 600 : 400,
        boxShadow: primary ? 'none' : 'inset 0 0 0 var(--kt-border-w) var(--kt-border)',
      }}
    >
      {label}
    </button>
  );
}

/// 네이티브가 토스트로 띄우려던 문구가 갈 자리. 웹뷰 창에서는 그 토스트가 안
/// 보이므로, 이게 없으면 「재시작하면 적용돼요」 같은 단서가 통째로 사라진다.
export function Notice({ notice }: { notice: { ok: boolean; msg: string } | null }) {
  if (!notice) return null;
  return (
    <p
      className="mb-4 text-[12px]"
      style={{ color: notice.ok ? 'var(--kt-text-dim)' : 'var(--kt-danger)' }}
    >
      {notice.msg}
    </p>
  );
}

/// 액션 하나를 태우고 결과를 알림으로 남긴다. 탭마다 같은 규칙으로 돈다 —
/// `error` 는 요청이 거부된 것, `message` 는 네이티브가 하려던 말(성공에도 온다).
///
/// 끝나면 **값을 다시 읽는다**. 화면이 요청값을 그대로 믿으면 저장 쪽에서 거부된
/// 변경이 화면에만 남는다 — 파일이 진실이다.
export function useSettingsAction(reload: () => Promise<void>) {
  const [busy, setBusy] = useState(false);
  const [notice, setNotice] = useState<{ ok: boolean; msg: string } | null>(null);

  async function run(action: string, args?: { id?: string; label?: string }) {
    setBusy(true);
    setNotice(null);
    try {
      const out = await postAction(action, args);
      if (out.error) setNotice({ ok: false, msg: out.error });
      else if (out.message) setNotice({ ok: out.ok, msg: out.message });
      else if (!out.ok) setNotice({ ok: false, msg: '안 됐어요' });
      await reload();
    } catch (e) {
      setNotice({ ok: false, msg: e instanceof Error ? e.message : String(e) });
    } finally {
      setBusy(false);
    }
  }

  return { busy, notice, run, setNotice };
}

/// 탭 본문을 감싸는 카드. 다섯 탭이 같은 테두리 안에 서야 nav 를 옮겨 다닐 때
/// 폼이 제자리에 있는 것으로 읽힌다.
export function TabCard({ children }: { children: React.ReactNode }) {
  return (
    <div
      className="p-6"
      style={{
        borderRadius: 'var(--kt-radius-md)',
        background: 'var(--kt-bg)',
        boxShadow: 'inset 0 0 0 var(--kt-border-w) var(--kt-border)',
      }}
    >
      {children}
    </div>
  );
}
