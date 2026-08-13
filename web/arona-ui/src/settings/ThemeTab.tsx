import { useState } from 'react';
import { postAction } from './api';
import { faceUrl } from './types';
import type { Character, SettingsCharacters, ThemeCard } from './types';

/// 한 카드 안의 소제목 + 설명. 네이티브 폼의 `section` 대응 — 제목만 크게 하고
/// 설명은 dim 으로 한 줄 아래 둔다.
function Section({
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
function MiniButton({
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
        // 카드 자체가 「이 테마 쓰기」 라서, 막지 않으면 버튼 하나 누를 때마다
        // 테마까지 갈아 끼워진다.
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

/// 테마 한 장. 미리보기 얼굴 셋 + 이름 + 「N명 · 쓰는 중」.
///
/// 프사는 `theme` 을 붙여 그 폴더의 그림을 받는다 — 붙이지 않으면 활성 테마의
/// 그림이 와서, 어느 카드를 봐도 같은 얼굴이 뜬다.
///
/// 카드 전체가 「이 테마 쓰기」이고 관리 버튼은 hover 때만 보인다 — 네이티브와
/// 같은 규칙이다. 늘 띄우면 카드 열두 장에 버튼 서른여섯 개라 정작 고르기가
/// 안 보인다. `invisible` 로 숨기는 게 `opacity-0` 보다 맞다: 투명한 버튼은
/// 여전히 눌리고 탭으로 잡혀서, 안 보이는 「치우기」가 카드 위에 남는다.
function ThemeCardView({
  card,
  active,
  busy,
  renaming,
  onSelect,
  onAction,
  onRenameStart,
  onRenameEnd,
}: {
  card: ThemeCard;
  active: boolean;
  busy: boolean;
  renaming: boolean;
  onSelect: () => void;
  onAction: (action: string, label?: string) => void;
  onRenameStart: () => void;
  onRenameEnd: () => void;
}) {
  // 번들은 폴더가 없어 이름도 못 바꾸고 치울 수도 없다(그림이 바이너리 안에 있다).
  const managed = card.id !== '';
  return (
    <div
      className="group relative overflow-hidden px-4 py-3"
      // 카드를 `<button>` 으로 만들 수 없다 — 안에 관리 버튼이 들어가고 버튼 중첩은
      // 잘못된 HTML 이다. role/tabIndex 로 같은 조작을 준다.
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
        boxShadow: `inset 0 0 0 var(--kt-border-w) var(--kt-border)`,
        cursor: active ? 'default' : 'pointer',
      }}
    >
      {active && (
        <span
          className="absolute left-0 top-0 h-full w-[3px]"
          style={{ background: 'var(--kt-accent)' }}
        />
      )}
      <div className="flex h-[96px] items-end gap-1">
        {card.faces.map((slug) => (
          <img
            key={slug}
            className="kt-face h-[92px] w-auto"
            src={faceUrl(slug, card.id || undefined)}
            alt=""
            // 그림이 없는 테마도 카드는 서야 한다 — 깨진 이미지 아이콘 대신 빈
            // 자리로 둔다.
            onError={(e) => {
              e.currentTarget.style.visibility = 'hidden';
            }}
          />
        ))}
      </div>

      {managed && !renaming && (
        <div className="invisible absolute right-3 top-3 flex gap-1 group-focus-within:visible group-hover:visible">
          <MiniButton label="이름" disabled={busy} onClick={onRenameStart} />
          <MiniButton label="폴더" disabled={busy} onClick={() => onAction('open-theme-dir')} />
          <MiniButton
            label="치우기"
            danger
            disabled={busy}
            onClick={() => onAction('delete-theme')}
          />
        </div>
      )}

      {renaming ? (
        <input
          className="kt-field mt-2 w-full"
          autoFocus
          defaultValue={card.label}
          onClick={(e) => e.stopPropagation()}
          // 칸을 벗어나면 굳힌다 — 네이티브 이름 칸과 같은 시점이다.
          onBlur={(e) => {
            const next = e.currentTarget.value.trim();
            if (next && next !== card.label) onAction('rename-theme', next);
            else onRenameEnd();
          }}
          onKeyDown={(e) => {
            e.stopPropagation();
            if (e.key === 'Enter') e.currentTarget.blur();
            if (e.key === 'Escape') {
              // 되돌린 뒤 blur 가 또 저장하지 않도록 값을 먼저 원래대로.
              e.currentTarget.value = card.label;
              e.currentTarget.blur();
            }
          }}
        />
      ) : (
        <div className="mt-2 text-[14px] font-medium text-[var(--kt-text)]">{card.label}</div>
      )}
      <div className="text-[12px] text-[var(--kt-text-mute)]">
        {card.count}명{active && ' · 쓰는 중'}
      </div>
    </div>
  );
}

/// 테마를 새로 만드는 칸. 목록 밖 버튼으로 빼지 않는 이유는 네이티브와 같다 —
/// 테마가 늘어날수록 멀어져서, 정작 만들려는 사람이 못 찾는다.
function NewThemeCard({ busy, onClick }: { busy: boolean; onClick: () => void }) {
  return (
    <button
      type="button"
      disabled={busy}
      onClick={onClick}
      // 높이를 못 박지 않는다 — grid stretch 라 카드와 같은 줄에 서면 알아서
      // 키가 맞고, 혼자 다음 줄로 떨어지면 얇은 띠로 남는다. 176px 을 박아 두면
      // 그 떨어진 줄이 빈 상자로 화면 하나를 먹는다.
      className="flex items-center justify-center py-6 disabled:opacity-40"
      style={{
        borderRadius: 'var(--kt-radius-md)',
        // 있는 테마와 구별되게 점선. 살아 있는 카드는 실선 inset ring 이다.
        border: '1px dashed var(--kt-border)',
        color: 'var(--kt-text-mute)',
      }}
    >
      <span className="text-[13px]">+ 새 테마</span>
    </button>
  );
}

/// 캐릭터 한 칸. 이름 위에 학교색 점을 둬서 소속이 색으로 읽힌다(네이티브
/// header_color 와 같은 값). 누르면 상세로 — 그게 성격을 고치는 입구다.
function CharacterCell({ c, onSelect }: { c: Character; onSelect: () => void }) {
  return (
    <button
      type="button"
      onClick={onSelect}
      className="flex flex-col items-center gap-1 px-2 py-3"
      style={{
        borderRadius: 'var(--kt-radius-sm)',
        background: 'var(--kt-surface)',
        boxShadow: `inset 0 0 0 var(--kt-border-w) var(--kt-border)`,
      }}
    >
      <img className="kt-face h-[64px] w-auto" src={faceUrl(c.slug)} alt="" />
      <div className="flex items-center gap-1.5">
        <span
          className="inline-block h-[7px] w-[7px] shrink-0"
          style={{ background: c.header_color, borderRadius: 'var(--kt-dot-radius)' }}
        />
        <span className="text-[12px] text-[var(--kt-text)]">{c.name}</span>
      </div>
    </button>
  );
}

export function ThemeTab({
  data,
  onSelect,
  onChanged,
}: {
  data: SettingsCharacters;
  onSelect: (c: Character) => void;
  /// 액션이 끝난 뒤 로스터를 다시 읽는다. **파일이 진실**이라, 요청값으로 화면을
  /// 그리면 저장 쪽에서 거부된 변경이 화면에만 남는다.
  onChanged: () => Promise<void>;
}) {
  const [busy, setBusy] = useState(false);
  const [notice, setNotice] = useState<{ ok: boolean; msg: string } | null>(null);
  /// 이름을 고치는 중인 테마 id. 네이티브도 카드 안에서 바로 고친다.
  const [renaming, setRenaming] = useState<string | null>(null);

  async function run(action: string, args?: { id?: string; label?: string }) {
    setBusy(true);
    setNotice(null);
    try {
      const out = await postAction(action, args);
      // `error` 는 요청이 거부된 것(모르는 액션·못 쓰는 이름), `message` 는
      // 네이티브가 하려던 말이다 — 성공에도 온다("테마를 바꿨어요…").
      if (out.error) setNotice({ ok: false, msg: out.error });
      else if (out.message) setNotice({ ok: out.ok, msg: out.message });
      else if (!out.ok) setNotice({ ok: false, msg: '안 됐어요' });
      await onChanged();
    } catch (e) {
      setNotice({ ok: false, msg: e instanceof Error ? e.message : String(e) });
    } finally {
      setBusy(false);
      setRenaming(null);
    }
  }

  return (
    <div
      className="p-6"
      style={{
        borderRadius: 'var(--kt-radius-md)',
        background: 'var(--kt-bg)',
        boxShadow: `inset 0 0 0 var(--kt-border-w) var(--kt-border)`,
      }}
    >
      {/* 네이티브 토스트는 웹뷰 창에서 안 보인다 — 그 문구가 갈 자리가 여기다.
          맨 위에 두는 이유는 「새로 여는 pane 부터 적용돼요」처럼 놓치면 안 되는
          말이 섞여 오기 때문이다. */}
      {notice && (
        <p
          className="mb-4 text-[12px]"
          style={{ color: notice.ok ? 'var(--kt-text-dim)' : 'var(--kt-danger)' }}
        >
          {notice.msg}
        </p>
      )}

      <Section title="Theme" hint="폴더 하나가 테마 하나 — 이름·색·그림이 한 벌로 바뀝니다">
        <div className="grid grid-cols-[repeat(auto-fill,minmax(240px,1fr))] gap-3">
          {data.themes.map((card) => (
            <ThemeCardView
              key={card.id || '(bundled)'}
              card={card}
              active={card.id === data.active_theme}
              busy={busy}
              renaming={renaming === card.id}
              onSelect={() => void run('select-theme', { id: card.id })}
              onAction={(action, label) => void run(action, { id: card.id, label })}
              onRenameStart={() => setRenaming(card.id)}
              onRenameEnd={() => setRenaming(null)}
            />
          ))}
          <NewThemeCard busy={busy} onClick={() => void run('new-theme')} />
        </div>
      </Section>

      <Section
        title="Persona"
        hint="켜면 캐릭터 말투로 대답해요 — 새로 여는 pane 부터"
        right={
          <button
            type="button"
            disabled={busy}
            onClick={() => void run('toggle-persona')}
            aria-pressed={data.persona_enabled}
            className="relative inline-block h-[22px] w-[40px] shrink-0 disabled:opacity-40"
            style={{
              borderRadius: '11px',
              background: data.persona_enabled ? 'var(--kt-accent)' : 'var(--kt-surface-hover)',
            }}
          >
            <span
              className="absolute top-[3px] h-[16px] w-[16px] bg-white transition-all"
              style={{
                borderRadius: 'var(--kt-dot-radius)',
                left: data.persona_enabled ? '21px' : '3px',
              }}
            />
          </button>
        }
      />

      <Section
        title="Character images"
        hint="테마 폴더의 sprites/ 에: <slug>-0..3 · -walk-0..5 · -wave-0..3 · -cheer-0..3 · -profile.png"
      >
        <div className="flex gap-2">
          <MiniButton
            label="이미지 폴더 열기"
            disabled={busy}
            onClick={() => void run('open-students-dir')}
          />
          <MiniButton label="로스터 열기" disabled={busy} onClick={() => void run('open-roster')} />
          <MiniButton label="새로고침" disabled={busy} onClick={() => void run('refresh-assets')} />
        </div>
      </Section>

      <Section title="Characters" hint={`${data.roster.length}명 — 캐릭터를 눌러 성격과 그림을 고치세요`}>
        <div className="grid grid-cols-[repeat(auto-fill,minmax(96px,1fr))] gap-2">
          {data.roster.map((c) => (
            <CharacterCell key={c.name} c={c} onSelect={() => onSelect(c)} />
          ))}
        </div>
      </Section>
    </div>
  );
}
