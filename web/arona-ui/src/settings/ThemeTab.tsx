import { useEffect, useRef, useState } from 'react';
import { ChevronDown, ChevronRight } from 'lucide-react';
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
  t,
  c,
  theme,
  picked,
  detail,
  disabled,
  onOpenSettings,
  onTogglePick,
}: {
  t: Strings;
  c: Character;
  /// 프사를 받을 폴더. 활성 테마면 빈 값이라 활성 → 번들 순으로 찾는다.
  theme?: string;
  picked: boolean;
  /// 이 테마가 지금 쓰는 테마인가. 상세로 갈 수 있는지를 가른다 — 프사·성격 편집은
  /// 그 테마가 설치돼 있어야 한다.
  detail: boolean;
  disabled: boolean;
  /// 세부설정 창을 연다. 창을 띄우는 것은 앱이라 여기서는 요청만 올린다.
  onOpenSettings: () => void;
  onTogglePick: () => void;
}) {
  // 클릭=고르기, **우클릭=메뉴**(거노 2026-08-25). 예전엔 쓰는 테마에서만 클릭이
  // 상세로 갔는데, 그러면 같은 동작이 테마에 따라 딴 일을 해서 어느 테마를 보고
  // 있는지 먼저 확인해야 눌 수 있었다. 이제 어디서나 클릭은 고르기다.
  //
  // 더블클릭을 안 쓰는 이유: 세부설정이 **별도 창**으로 나가면서, 두 번 누르는 동안
  // 첫 클릭이 이미 고르기를 내고 그걸 되돌리는 편법이 필요했다. 메뉴는 그 왕복이 없고
  // 「무엇을 할 수 있는지」가 눈에 보인다.
  const [menu, setMenu] = useState<{ x: number; y: number } | null>(null);
  const card = useRef<HTMLDivElement>(null);
  return (
    <div
      ref={card}
      // 카드를 `<button>` 으로 못 만든다 — 안에 켬/끔 버튼이 들어가고 버튼 중첩은
      // 잘못된 HTML 이다. ThemeCardView 와 같은 방식으로 조작만 준다.
      role="button"
      tabIndex={0}
      aria-pressed={picked}
      aria-haspopup="menu"
      aria-expanded={menu != null}
      title={detail ? `${c.name} — 우클릭하면 설정` : c.name}
      // 저장이 도는 동안은 안 받는다 — 켬/끔이 서버 왕복이라, 연달아 누르면 마지막
      // 응답이 앞선 것을 덮어 화면과 저장이 어긋난다.
      onClick={() => !disabled && onTogglePick()}
      onContextMenu={(e) => {
        e.preventDefault();
        if (disabled) return;
        // 카드-로컬 좌표(카드가 relative) — 격자가 스크롤해도 메뉴가 따라온다.
        const r = e.currentTarget.getBoundingClientRect();
        setMenu({ x: e.clientX - r.left, y: e.clientY - r.top });
      }}
      onKeyDown={(e) => {
        if (e.key === 'Escape' && menu) {
          e.preventDefault();
          setMenu(null);
          card.current?.focus();
          return;
        }
        if ((e.key === 'ContextMenu' || (e.shiftKey && e.key === 'F10')) && e.target === e.currentTarget) {
          e.preventDefault();
          setMenu({ x: 20, y: 32 });
          return;
        }
        if ((e.key === 'Enter' || e.key === ' ') && e.target === e.currentTarget) {
          e.preventDefault();
          if (!disabled) onTogglePick();
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
      {/* 켬/끔 동그라미를 없앴다 — 이제 카드 전체가 고르기라 같은 일을 하는 표적이
          둘이 되고, 작은 동그라미를 빗맞히면 「눌렀는데 딴 게 됐다」가 된다.
          켜짐은 카드 밝기와 이름 옆 색점으로 이미 보인다. */}
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

      {menu && (
        <>
          {/* 바깥 아무 데나 눌러 닫는다. 카드 위에 겹치는 투명 판이라, 이게 없으면
              메뉴를 닫는 클릭이 밑의 카드까지 눌러 고르기가 뒤집힌다. */}
          <div
            className="fixed inset-0 z-40"
            onClick={(e) => {
              e.stopPropagation();
              setMenu(null);
              card.current?.focus();
            }}
            onContextMenu={(e) => {
              e.preventDefault();
              setMenu(null);
            }}
          />
          <div
            className="absolute z-50 py-1"
            role="menu"
            style={{
              left: menu.x,
              top: menu.y,
              minWidth: 116,
              borderRadius: 'var(--kt-radius-md)',
              background: 'var(--kt-surface)',
              boxShadow: `inset 0 0 0 var(--kt-border-w) var(--kt-border), 0 6px 20px rgba(0,0,0,.35)`,
            }}
            onClick={(e) => e.stopPropagation()}
          >
            {/* 그림·성격은 그 테마가 설치돼 있어야 고칠 수 있다 — 아니면 항목을 아예 뺀다.
                눌리는데 아무 일도 안 나는 항목이 제일 나쁘다. */}
            {detail && (
              <button
                type="button"
                role="menuitem"
                autoFocus
                className="block min-h-[36px] w-full px-3 py-1.5 text-left text-[12px] text-[var(--kt-text)]"
                onClick={() => {
                  setMenu(null);
                  onOpenSettings();
                }}
              >
                {t.theme.menuSettings}
              </button>
            )}
            <button
              type="button"
              role="menuitem"
              autoFocus={!detail}
              className="block min-h-[36px] w-full px-3 py-1.5 text-left text-[12px] text-[var(--kt-text)]"
              onClick={() => {
                setMenu(null);
                onTogglePick();
              }}
            >
              {picked ? t.theme.menuPickOff : t.theme.menuPickOn}
            </button>
          </div>
        </>
      )}
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
  onAction,
  onAdded,
  pane,
}: {
  t: Strings;
  card: ThemeCard;
  active: boolean;
  /// `'body'` 면 **머리 없이 본문만** 그린다 — 오른쪽 칸에 한 테마만 펼쳐 두는
  /// 2단 배치용(거노 2026-08-25 「창 두개로 테마로 선택, 개별선택으로」). 접이식은
  /// 11테마를 세로로 쌓아 스크롤이 길어지는데, 고르는 동안 보는 것은 늘 한 테마다.
  pane?: 'body';
  /// 활성 테마 명단은 `/settings/characters` 에 이미 실려 온다 — 다시 받지 않는다.
  activeRoster: Character[];
  busy: boolean;
  onAction: (action: string, args: { id: string; label?: string }) => void;
  /// 학생 추가는 액션이 아니라 자체 업로드다 — 끝났다는 말만 위로 올린다.
  onAdded: (msg: string) => void;
}) {
  const [open, setOpen] = useState(pane === 'body' ? true : active);
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
      {pane !== 'body' && (
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
        {open ? (
          <ChevronDown aria-hidden="true" className="h-4 w-4 text-[var(--kt-text-mute)]" />
        ) : (
          <ChevronRight aria-hidden="true" className="h-4 w-4 text-[var(--kt-text-mute)]" />
        )}
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
      )}

      {open && (
        <div className="px-3 pb-3 pt-1">
          {pane === 'body' && (
            // 머리를 없앤 자리 — 「전부/해제」와 개수는 여기 한 줄로 옮긴다.
            <div className="mb-2 flex items-center gap-2">
              <span className="text-[13px] text-[var(--kt-text)]">{card.label}</span>
              {active && (
                <span className="text-[11px] text-[var(--kt-text-mute)]">{t.theme.inUse}</span>
              )}
              <span className="ml-auto text-[11px] text-[var(--kt-text-mute)]">
                {on === 0 ? t.theme.pickNone : t.theme.pickCount({ on, total })}
              </span>
              {/* 보고 있는 테마를 그 자리에서 쓸 수 있게 — 예전에는 위쪽 테마 격자로
                  올라가야 했다. 아무도 안 골랐을 때의 폴백이 「쓰는 테마 전원」이라,
                  어느 테마를 쓰는지가 고르기 화면에서도 뜻이 있다. */}
              {!active && (
                <MiniButton
                  label={t.theme.useTheme}
                  disabled={busy}
                  onClick={() => onAction('select-theme', { id: card.id })}
                />
              )}
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
          )}
          <p className="mb-2 text-[11px] text-[var(--kt-text-mute)]">
            {active ? t.theme.pickHintActive : t.theme.pickHintOther}
          </p>
          {roster ? (
            <div className="grid grid-cols-[repeat(auto-fill,minmax(96px,1fr))] gap-2">
              {roster.map((c) => (
                <CharacterCell
                  key={c.name}
                  t={t}
                  c={c}
                  theme={active ? undefined : card.id}
                  picked={picked.has(c.name)}
                  detail={active}
                  disabled={busy}
                  /// 창을 띄우는 것은 앱이다 — 웹은 slug 와 테마 키만 올린다.
                  onOpenSettings={() => onAction('open-student', { id: c.slug, label: key })}
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

/// 두 탭(테마·캐릭터)이 공유하는 액션 통로. 둘 다 같은 로스터 파일을 고치고
/// 같은 방식으로 알림을 띄우므로, 통로가 갈리면 한쪽만 고쳐지는 자리가 생긴다.
function useRosterAction(onChanged: () => Promise<void>, onSettled?: () => void) {
  const t = useT();
  const [busy, setBusy] = useState(false);
  const [notice, setNotice] = useState<{ ok: boolean; msg: string } | null>(null);

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
      onSettled?.();
    }
  }

  return { t, busy, notice, setNotice, run };
}

export function ThemeTab({
  data,
  onChanged,
}: {
  data: SettingsCharacters;
  /// 액션이 끝난 뒤 로스터를 다시 읽는다. **파일이 진실**이라, 요청값으로 화면을
  /// 그리면 저장 쪽에서 거부된 변경이 화면에만 남는다.
  onChanged: () => Promise<void>;
}) {
  /// 이름을 고치는 중인 테마 id. 네이티브도 카드 안에서 바로 고친다.
  const [renaming, setRenaming] = useState<string | null>(null);
  const { t, busy, notice, run } = useRosterAction(onChanged, () => setRenaming(null));

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
        {/* 가져오기는 버튼이 아니라 드롭이라 화면에 아무 흔적이 없다. 적어 두지
            않으면 그런 길이 있다는 걸 알 방법이 없고, 그럼 테마를 건네받은 사람은
            숨은 폴더를 손으로 찾아 들어가야 한다. */}
        <p className="mt-3 text-[11px] opacity-60">{t.theme.importHint}</p>
      </Section>

      <Section
        title={t.theme.persona}
        hint={t.theme.personaHint}
        right={
          <Toggle
            label={t.theme.persona}
            on={data.persona_enabled}
            disabled={busy}
            onToggle={() => void run('toggle-persona')}
          />
        }
      />

    </TabCard>
  );
}

/// 캐릭터 한 명씩 고치는 칸. 테마 격자와 한 화면에 있던 것을 갈랐다(2026-08-26
/// 지시) — 「어느 세트를 쓸까」와 「이 애를 어떻게 고칠까」는 다른 일이라, 캐릭터를
/// 고치러 온 사람이 테마 격자를 지나 한참 내려가야 했다. 네이티브의
/// `SettingsCat::Students` 와 같은 칸이다.
export function CharactersTab({
  data,
  onChanged,
}: {
  data: SettingsCharacters;
  onChanged: () => Promise<void>;
}) {
  const { t, busy, notice, setNotice, run } = useRosterAction(onChanged);

  /// 골라 둔 총원. 테마를 가로질러 세되 **이름으로 합집합**을 낸다 — 배정 쪽
  /// `assignable_names` 가 같은 이름을 한 번만 쓰므로, 두 테마에 같은 이름이
  /// 있으면 화면 숫자가 실제 배정 인원보다 커지면 안 된다.
  const pickedTotal = new Set(data.themes.flatMap((c) => c.picked)).size;

  /// 오른쪽 칸에 펼칠 테마. 처음엔 **쓰는 중인 테마**를 연다 — 대개 거기서 고르고,
  /// 그 명단은 이미 `data.roster` 로 실려 와 있어 첫 화면이 왕복 없이 뜬다.
  const [selThemeId, setSelThemeId] = useState<string>(data.active_theme ?? '');
  /// 고른 id 가 목록에 없으면(테마를 지웠다) 첫 칸으로 떨어진다 — 빈 오른쪽을
  /// 보여 주면 「고장」으로 읽힌다.
  const selTheme = data.themes.find((c) => c.id === selThemeId) ?? data.themes[0];

  return (
    <TabCard>
      <Notice notice={notice} />

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
        {/* 왼쪽에서 테마를 고르고 오른쪽에서 그 테마의 학생을 켜고 끈다. 접이식을
            그만둔 이유는 규모다 — 11테마를 세로로 쌓으면 스크롤이 길어지는데, 정작
            고르는 동안 보는 것은 늘 한 테마다. 왼쪽에 개수를 함께 세워, 어느 테마에서
            몇을 골랐는지 펼치지 않고도 보이게 했다. */}
        <div className="flex gap-3">
          <div className="flex w-[186px] shrink-0 flex-col gap-1">
            {data.themes.map((card) => {
              const chosen = card.id === selThemeId;
              return (
                <button
                  key={card.id || '(bundled)'}
                  type="button"
                  onClick={() => setSelThemeId(card.id)}
                  className="flex items-center gap-2 px-2 py-[7px] text-left"
                  style={{
                    borderRadius: 'var(--kt-radius-md)',
                    background: chosen ? 'var(--kt-surface-active)' : 'var(--kt-surface)',
                    boxShadow: chosen
                      ? `inset 0 0 0 var(--kt-border-w) var(--kt-accent)`
                      : `inset 0 0 0 var(--kt-border-w) var(--kt-border)`,
                  }}
                >
                  <span className="truncate text-[12px] text-[var(--kt-text)]">{card.label}</span>
                  {card.id === data.active_theme && (
                    <span className="shrink-0 text-[10px] text-[var(--kt-text-mute)]">
                      {t.theme.inUse}
                    </span>
                  )}
                  <span className="ml-auto shrink-0 text-[11px] text-[var(--kt-text-mute)]">
                    {card.picked.length === 0 ? t.theme.pickNone : `${card.picked.length}/${card.count}`}
                  </span>
                </button>
              );
            })}
          </div>
          <div className="min-w-0 flex-1">
            {selTheme && (
              <CharacterGroup
                /* 테마를 바꾸면 명단을 새로 읽어야 한다 — key 로 갈아 끼운다. */
                key={selTheme.id || '(bundled)'}
                pane="body"
                t={t}
                card={selTheme}
                active={selTheme.id === data.active_theme}
                activeRoster={data.roster}
                busy={busy}
                onAction={(action, args) => void run(action, args)}
                onAdded={(msg) => {
                  setNotice({ ok: true, msg });
                  void onChanged();
                }}
              />
            )}
          </div>
        </div>
      </Section>
    </TabCard>
  );
}
