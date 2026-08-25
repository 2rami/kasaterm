import { useEffect, useState } from 'react';
import { fetchThemeRoster, postAction } from './api';
import { MiniButton, Notice, Section, TabCard, Toggle } from './controls';
import { serverText, useT } from './lang';
import { NewStudentCard, ThemeGenEngine } from './ThemeGen';
import { faceUrl } from './types';
import type { Strings } from './strings';
import type { Character, SettingsCharacters, ThemeCard } from './types';

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
  t,
  card,
  active,
  busy,
  renaming,
  onSelect,
  onAction,
  onRenameStart,
  onRenameEnd,
}: {
  t: Strings;
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
          <MiniButton label={t.theme.rename} disabled={busy} onClick={onRenameStart} />
          <MiniButton
            label={t.theme.folder}
            disabled={busy}
            onClick={() => onAction('open-theme-dir')}
          />
          <MiniButton
            label={t.theme.remove}
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
        {t.theme.members({ count: card.count })}
        {active && ` · ${t.theme.inUse}`}
      </div>
    </div>
  );
}

/// 테마를 새로 만드는 칸. 목록 밖 버튼으로 빼지 않는 이유는 네이티브와 같다 —
/// 테마가 늘어날수록 멀어져서, 정작 만들려는 사람이 못 찾는다.
function NewThemeCard({
  t,
  busy,
  onClick,
}: {
  t: Strings;
  busy: boolean;
  onClick: () => void;
}) {
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
      <span className="text-[13px]">{t.theme.newTheme}</span>
    </button>
  );
}

/// 캐릭터 한 칸. 이름 위에 학교색 점을 둬서 소속이 색으로 읽힌다(네이티브
/// header_color 와 같은 값).
///
/// 누르는 뜻이 테마에 따라 갈린다. **활성 테마**면 카드가 상세로 가고(성격·그림을
/// 고치는 입구는 거기뿐이다) 켬/끔은 오른쪽 위 동그라미가 맡는다. **다른 테마**면
/// 상세를 열어 봐야 저장이 거부되므로(`/settings/character` 가 활성 명단만 받는다)
/// 카드 전체가 켬/끔이다.
///
/// 꺼진 표시로 얼굴 위에 체크를 얹지 않는다 — 미리보기를 가린다(네이티브가 같은
/// 자리에서 이미 겪은 문제다). 대신 **흐리게 + 학교색 점 제거**로, 켜진 게 기본인
/// 화면에서 「빠진 것」이 눈에 띄게 한다.
function CharacterCell({
  c,
  theme,
  picked,
  detail,
  disabled,
  onSelect,
  onTogglePick,
}: {
  c: Character;
  /// 프사를 받을 폴더. 활성 테마면 빈 값이라 활성 → 번들 순으로 찾는다.
  theme?: string;
  picked: boolean;
  /// 카드 본체를 눌렀을 때 상세로 가는지(활성 테마만 true).
  detail: boolean;
  disabled: boolean;
  onSelect: () => void;
  onTogglePick: () => void;
}) {
  const body = () => (detail ? onSelect() : onTogglePick());
  return (
    <div
      // 카드를 `<button>` 으로 못 만든다 — 안에 켬/끔 버튼이 들어가고 버튼 중첩은
      // 잘못된 HTML 이다. ThemeCardView 와 같은 방식으로 조작만 준다.
      role="button"
      tabIndex={0}
      aria-pressed={detail ? undefined : picked}
      onClick={body}
      onKeyDown={(e) => {
        if ((e.key === 'Enter' || e.key === ' ') && e.target === e.currentTarget) {
          e.preventDefault();
          body();
        }
      }}
      className="relative flex flex-col items-center gap-1 px-2 py-3"
      style={{
        borderRadius: 'var(--kt-radius-sm)',
        background: 'var(--kt-surface)',
        boxShadow: `inset 0 0 0 var(--kt-border-w) var(--kt-border)`,
        opacity: picked ? 1 : 0.38,
      }}
    >
      <button
        type="button"
        disabled={disabled}
        aria-pressed={picked}
        title={c.name}
        // 카드가 상세로 가는 활성 테마에서만 따로 필요하다. 다른 테마는 카드
        // 전체가 같은 일을 하므로 동그라미를 겹쳐 두면 판정만 헷갈린다.
        className={`absolute right-1.5 top-1.5 h-[15px] w-[15px] ${detail ? '' : 'hidden'}`}
        onClick={(e) => {
          e.stopPropagation();
          onTogglePick();
        }}
        style={{
          borderRadius: 'var(--kt-dot-radius)',
          background: picked ? 'var(--kt-accent)' : 'transparent',
          boxShadow: `inset 0 0 0 var(--kt-border-w) var(--kt-border)`,
        }}
      />
      <img className="kt-face h-[64px] w-auto" src={faceUrl(c.slug, theme)} alt="" />
      <div className="flex items-center gap-1.5">
        <span
          className="inline-block h-[7px] w-[7px] shrink-0"
          style={{
            background: picked ? c.header_color : 'transparent',
            borderRadius: 'var(--kt-dot-radius)',
          }}
        />
        <span className="text-[12px] text-[var(--kt-text)]">{c.name}</span>
      </div>
    </div>
  );
}

/// 테마 하나의 캐릭터 묶음. 머리에 이름·`3/21`·[전부]/[해제] 가 있고, 펼쳐야
/// 명단을 받는다.
///
/// 접어 두는 이유는 규모다 — 11테마 300명을 한 번에 그리면 무겁고, 정작 고르려는
/// 사람이 어디 있는지도 안 보인다. 활성 테마만 펼친 채 시작한다.
function CharacterGroup({
  t,
  card,
  active,
  activeRoster,
  busy,
  onSelect,
  onAction,
  onAdded,
}: {
  t: Strings;
  card: ThemeCard;
  active: boolean;
  /// 활성 테마 명단은 `/settings/characters` 에 이미 실려 온다 — 다시 받지 않는다.
  activeRoster: Character[];
  busy: boolean;
  onSelect: (c: Character) => void;
  onAction: (action: string, args: { id: string; label?: string }) => void;
  /// 학생 추가는 액션이 아니라 자체 업로드다 — 끝났다는 말만 위로 올린다.
  onAdded: (msg: string) => void;
}) {
  const [open, setOpen] = useState(active);
  const [roster, setRoster] = useState<Character[] | null>(active ? activeRoster : null);
  const [failed, setFailed] = useState(false);
  // 번들은 폴더가 없어 카드 id 가 빈 문자열인데, 명단 조회와 고르기 저장은 `__base`
  // 라는 예약어로 부른다 — 빈 값은 「안 줬다」와 구분이 안 돼 백엔드가 거부한다.
  // 테마 자체를 고르는 `select-theme` 은 지금대로 빈 id 를 쓴다(그쪽은 「기본으로
  // 돌아가기」라 빈 값이 곧 뜻이다).
  const key = card.id === '' ? '__base' : card.id;

  useEffect(() => {
    if (active) setRoster(activeRoster);
  }, [active, activeRoster]);

  useEffect(() => {
    if (!open || active || roster) return;
    let live = true;
    fetchThemeRoster(key)
      .then((r) => live && setRoster(r))
      .catch(() => live && setFailed(true));
    return () => {
      live = false;
    };
  }, [open, active, roster, key]);

  const picked = new Set(card.picked);
  // 명단을 아직 안 받았으면 카드가 알려 준 총원을 쓴다 — 펼치기 전에도 「몇 명
  // 중 몇 명」이 보여야 어느 묶음을 열지 정할 수 있다.
  const total = roster?.length ?? card.count;
  // 접힌 동안에는 명단이 없어 저장된 개수를 그대로 센다. 이름이 바뀌거나 테마가
  // 줄어든 옛 설정에는 이제 없는 사람이 남아 있을 수 있어서, 그대로 두면 「23/21」
  // 같은 숫자가 나온다 — 펼치면 실제 명단으로 다시 세니 여기서만 눌러 둔다.
  const on = roster
    ? roster.filter((c) => picked.has(c.name)).length
    : Math.min(picked.size, total);

  return (
    <div
      className="overflow-hidden"
      style={{
        borderRadius: 'var(--kt-radius-md)',
        boxShadow: `inset 0 0 0 var(--kt-border-w) var(--kt-border)`,
      }}
    >
      <div
        role="button"
        tabIndex={0}
        aria-expanded={open}
        onClick={() => setOpen((v) => !v)}
        onKeyDown={(e) => {
          if ((e.key === 'Enter' || e.key === ' ') && e.target === e.currentTarget) {
            e.preventDefault();
            setOpen((v) => !v);
          }
        }}
        className="flex items-center gap-2 px-3 py-2"
        style={{ background: open ? 'var(--kt-surface-active)' : 'var(--kt-surface)' }}
      >
        <span className="text-[11px] text-[var(--kt-text-mute)]">{open ? '▾' : '▸'}</span>
        <span className="text-[13px] text-[var(--kt-text)]">{card.label}</span>
        {active && (
          <span className="text-[11px] text-[var(--kt-text-mute)]">{t.theme.inUse}</span>
        )}
        <span className="ml-auto text-[11px] text-[var(--kt-text-mute)]">
          {on === 0 ? t.theme.pickNone : t.theme.pickCount({ on, total })}
        </span>
        <MiniButton
          label={t.theme.pickAll}
          disabled={busy}
          onClick={() => onAction('theme-pick-all', { id: key })}
        />
        <MiniButton
          label={t.theme.pickClear}
          disabled={busy}
          onClick={() => onAction('theme-pick-none', { id: key })}
        />
      </div>

      {open && (
        <div className="px-3 pb-3 pt-1">
          <p className="mb-2 text-[11px] text-[var(--kt-text-mute)]">
            {active ? t.theme.pickHintActive : t.theme.pickHintOther}
          </p>
          {roster ? (
            <div className="grid grid-cols-[repeat(auto-fill,minmax(96px,1fr))] gap-2">
              {roster.map((c) => (
                <CharacterCell
                  key={c.name}
                  c={c}
                  theme={active ? undefined : card.id}
                  picked={picked.has(c.name)}
                  detail={active}
                  disabled={busy}
                  onSelect={() => onSelect(c)}
                  onTogglePick={() =>
                    onAction(picked.has(c.name) ? 'character-pick-off' : 'character-pick', {
                      id: key,
                      label: c.name,
                    })
                  }
                />
              ))}
              {/* 번들은 폴더가 없어 그림을 놓을 자리부터 없다 — 카드 자체를 안 준다. */}
              {active && card.id !== '' && <NewStudentCard onAdded={onAdded} />}
            </div>
          ) : (
            <p className="text-[12px] text-[var(--kt-text-mute)]">
              {failed ? t.theme.pickLoadFailed : t.theme.pickLoading}
            </p>
          )}
        </div>
      )}
    </div>
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
  const t = useT();
  const [busy, setBusy] = useState(false);
  const [notice, setNotice] = useState<{ ok: boolean; msg: string } | null>(null);
  /// 이름을 고치는 중인 테마 id. 네이티브도 카드 안에서 바로 고친다.
  const [renaming, setRenaming] = useState<string | null>(null);

  /// 골라 둔 총원. 테마를 가로질러 세되 **이름으로 합집합**을 낸다 — 배정 쪽
  /// `assignable_names` 가 같은 이름을 한 번만 쓰므로, 두 테마에 같은 이름이
  /// 있으면 화면 숫자가 실제 배정 인원보다 커지면 안 된다.
  const pickedTotal = new Set(data.themes.flatMap((c) => c.picked)).size;

  async function run(action: string, args?: { id?: string; label?: string }) {
    setBusy(true);
    setNotice(null);
    try {
      const out = await postAction(action, args);
      // `error` 는 요청이 거부된 것(모르는 액션·못 쓰는 이름), `message` 는
      // 네이티브가 하려던 말이다 — 성공에도 온다("테마를 바꿨어요…").
      //
      // 코드가 함께 오면 사전에서 만들고, 없으면 서버 문구를 그대로 쓴다 —
      // 서버 쪽 코드화가 덜 끝난 자리도 화면이 안 깨진다.
      if (out.error || out.error_code) {
        setNotice({
          ok: false,
          msg: serverText(t, out.error_code, out.error, out.error_args),
        });
      } else if (out.message || out.message_code) {
        setNotice({
          ok: out.ok,
          msg: serverText(t, out.message_code, out.message, out.message_args),
        });
      } else if (!out.ok) setNotice({ ok: false, msg: t.common.failed });
      await onChanged();
    } catch (e) {
      setNotice({ ok: false, msg: e instanceof Error ? e.message : String(e) });
    } finally {
      setBusy(false);
      setRenaming(null);
    }
  }

  return (
    <TabCard>
      {/* 네이티브 토스트는 웹뷰 창에서 안 보인다 — 그 문구가 갈 자리가 여기다.
          맨 위에 두는 이유는 「새로 여는 pane 부터 적용돼요」처럼 놓치면 안 되는
          말이 섞여 오기 때문이다. */}
      <Notice notice={notice} />

      <Section title={t.theme.section} hint={t.theme.sectionHint}>
        <div className="grid grid-cols-[repeat(auto-fill,minmax(240px,1fr))] gap-3">
          {data.themes.map((card) => (
            <ThemeCardView
              key={card.id || '(bundled)'}
              t={t}
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
          <NewThemeCard t={t} busy={busy} onClick={() => void run('new-theme')} />
        </div>
      </Section>

      <Section
        title={t.theme.persona}
        hint={t.theme.personaHint}
        right={
          <Toggle
            on={data.persona_enabled}
            disabled={busy}
            onToggle={() => void run('toggle-persona')}
          />
        }
      />

      <ThemeGenEngine />

      {/* 폴더 안내를 규격 나열에서 「언제 폴더를 여는가」로 바꿨다 — 모션별 교체가
          캐릭터 상세에 생긴 뒤로는 파일 이름 규약을 외울 일이 없다. */}
      <Section title={t.theme.images} hint={t.theme.imagesHint}>
        <div className="flex gap-2">
          <MiniButton
            label={t.theme.openImages}
            disabled={busy}
            onClick={() => void run('open-students-dir')}
          />
          <MiniButton
            label={t.theme.openRoster}
            disabled={busy}
            onClick={() => void run('open-roster')}
          />
          <MiniButton
            label={t.theme.refresh}
            disabled={busy}
            onClick={() => void run('refresh-assets')}
          />
        </div>
      </Section>

      {/* 테마별로 접어 쌓는다. 한 테마 안에서 고르는 화면이 아니라 **테마를
          가로질러 한 명단을 만드는** 화면이라 묶음이 여럿 보여야 한다(거노 지시
          2026-08-25: 「테마 상관없이 섞어서」). */}
      <Section title={t.theme.characters} hint={t.theme.charactersHint}>
        {/* 아무도 안 골랐으면 폴백(활성 테마 전원)이 도는 중이다 — 숫자만 봐선
            「0명이라 아무도 안 나온다」로 읽혀서 그 상태를 말로 적어 준다. */}
        <p className="mb-2 text-[12px] text-[var(--kt-text-mute)]">
          {pickedTotal === 0 ? t.theme.pickEmpty : t.theme.pickTotal({ count: pickedTotal })}
        </p>
        <div className="flex flex-col gap-2">
          {data.themes.map((card) => (
            <CharacterGroup
              key={card.id || '(bundled)'}
              t={t}
              card={card}
              active={card.id === data.active_theme}
              activeRoster={data.roster}
              busy={busy}
              onSelect={onSelect}
              onAction={(action, args) => void run(action, args)}
              onAdded={(msg) => {
                setNotice({ ok: true, msg });
                void onChanged();
              }}
            />
          ))}
        </div>
      </Section>
    </TabCard>
  );
}
