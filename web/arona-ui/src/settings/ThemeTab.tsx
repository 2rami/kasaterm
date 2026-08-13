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

/// 테마 한 장. 미리보기 얼굴 셋 + 이름 + 「N명 · 쓰는 중」.
///
/// 프사는 `theme` 을 붙여 그 폴더의 그림을 받는다 — 붙이지 않으면 활성 테마의
/// 그림이 와서, 어느 카드를 봐도 같은 얼굴이 뜬다.
function ThemeCardView({ card, active }: { card: ThemeCard; active: boolean }) {
  return (
    <div
      className="relative overflow-hidden px-4 py-3"
      style={{
        borderRadius: 'var(--kt-radius-md)',
        background: active ? 'var(--kt-surface-active)' : 'var(--kt-surface)',
        boxShadow: `inset 0 0 0 var(--kt-border-w) var(--kt-border)`,
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
      <div className="mt-2 text-[14px] font-medium text-[var(--kt-text)]">{card.label}</div>
      <div className="text-[12px] text-[var(--kt-text-mute)]">
        {card.count}명{active && ' · 쓰는 중'}
      </div>
    </div>
  );
}

/// 테마를 새로 만드는 빈 칸. 네이티브 그리드의 마지막 칸과 같은 자리라 빼면
/// 「나란히 비교」에서 칸 수가 어긋난다 — 대신 아직 안 눌리는 걸 흐림으로 말한다.
function NewThemeCard() {
  return (
    <div
      // 높이를 못 박지 않는다 — grid stretch 라 카드와 같은 줄에 서면 알아서
      // 키가 맞고, 혼자 다음 줄로 떨어지면 얇은 띠로 남는다. 176px 을 박아 두면
      // 그 떨어진 줄이 빈 상자로 화면 하나를 먹는다.
      className="flex cursor-not-allowed items-center justify-center py-6 opacity-40"
      title="다음 단계에서 붙습니다"
      style={{
        borderRadius: 'var(--kt-radius-md)',
        // 있는 테마와 구별되게 점선. 살아 있는 카드는 실선 inset ring 이다.
        border: '1px dashed var(--kt-border)',
        color: 'var(--kt-text-mute)',
      }}
    >
      <span className="text-[13px]">+ 새 테마</span>
    </div>
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

/// 아직 안 붙은 액션. 흐리게 두는 게 정직하다 — 눌리는 것처럼 그려 두면 눌러
/// 보고 나서야 없는 기능인 걸 알게 된다.
function PendingButton({ label }: { label: string }) {
  return (
    <button
      type="button"
      disabled
      title="다음 단계에서 붙습니다"
      className="cursor-not-allowed px-3 py-1.5 text-[13px] opacity-40"
      style={{
        borderRadius: 'var(--kt-radius-sm)',
        background: 'var(--kt-surface-hover)',
        color: 'var(--kt-text)',
        boxShadow: `inset 0 0 0 var(--kt-border-w) var(--kt-border)`,
      }}
    >
      {label}
    </button>
  );
}

export function ThemeTab({
  data,
  onSelect,
}: {
  data: SettingsCharacters;
  onSelect: (c: Character) => void;
}) {
  return (
    <div
      className="p-6"
      style={{
        borderRadius: 'var(--kt-radius-md)',
        background: 'var(--kt-bg)',
        boxShadow: `inset 0 0 0 var(--kt-border-w) var(--kt-border)`,
      }}
    >
      <Section title="Theme" hint="폴더 하나가 테마 하나 — 이름·색·그림이 한 벌로 바뀝니다">
        <div className="grid grid-cols-[repeat(auto-fill,minmax(240px,1fr))] gap-3">
          {data.themes.map((card) => (
            <ThemeCardView
              key={card.id || '(bundled)'}
              card={card}
              active={card.id === data.active_theme}
            />
          ))}
          <NewThemeCard />
        </div>
      </Section>

      <Section
        title="Persona"
        hint="켜면 캐릭터 말투로 대답해요 — 새로 여는 pane 부터"
        right={
          <span
            className="relative inline-block h-[22px] w-[40px] shrink-0 opacity-60"
            title="다음 단계에서 붙습니다"
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
          </span>
        }
      />

      <Section
        title="Character images"
        hint="테마 폴더의 sprites/ 에: <slug>-0..3 · -walk-0..5 · -wave-0..3 · -cheer-0..3 · -profile.png"
      >
        <div className="flex gap-2">
          <PendingButton label="이미지 폴더 열기" />
          <PendingButton label="로스터 열기" />
          <PendingButton label="새로고침" />
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
