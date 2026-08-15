import { useEffect, useState } from 'react';
import {
  Button,
  Notice,
  Section,
  Stepper,
  TabCard,
  TextField,
  useSettingsAction,
} from './controls';
import { useT } from './lang';
import type { AppearanceValues, ShapePreset, ThemePreset } from './types';

/// 테마 한 장 — 그 팔레트로 그린 미니 프리뷰(배경 칠 + 프롬프트 샘플 + ANSI 점 +
/// 라벨)라서 고르기 전에 색이 보인다. UI 토큰과 터미널 16색이 함께 바뀐다.
function ThemeCard({
  preset,
  active,
  disabled,
  onPick,
}: {
  preset: ThemePreset;
  active: boolean;
  disabled?: boolean;
  onPick: () => void;
}) {
  return (
    <button
      type="button"
      disabled={disabled}
      aria-current={active}
      onClick={() => !active && onPick()}
      className="px-3 pb-3 pt-3 text-left disabled:opacity-40"
      style={{
        borderRadius: 'var(--kt-radius-md)',
        background: preset.bg,
        // 선택은 링으로 낸다 — 카드 자체가 팔레트를 보여 주는 자리라 배경을
        // 바꿔 표시하면 그 팔레트를 거짓말하게 된다.
        boxShadow: active
          ? '0 0 0 2px var(--kt-accent)'
          : 'inset 0 0 0 var(--kt-border-w) var(--kt-border)',
      }}
    >
      {/* 미리보기 줄은 모노 고정 — 이 카드가 대변하는 건 UI 크롬이 아니라 터미널
          본문이고, 본문 폰트는 형태 축이 건드리지 않는다. */}
      <div
        className="text-[12px]"
        style={{ color: preset.text, fontFamily: 'var(--kt-font-mono)' }}
      >
        ❯ ls -la
      </div>
      <div className="mt-3 flex gap-1.5">
        {preset.ansi.map((c, i) => (
          <span
            key={i}
            className="inline-block h-[10px] w-[10px]"
            style={{ background: c, borderRadius: 'var(--kt-dot-radius)' }}
          />
        ))}
      </div>
      <div
        className="mt-3 text-[12px]"
        style={{ color: preset.dim, fontWeight: active ? 600 : 400 }}
      >
        {preset.label}
      </div>
    </button>
  );
}

/// 색 견본 겸 선택기. 웹뷰라 OS 색 선택 패널을 그대로 쓴다 — 네이티브 화면이 채도×
/// 명도 사각형을 손으로 그려야 했던 자리다.
///
/// 고르는 동안 `change` 가 연달아 오므로 그때는 화면만 바꾸고, 칸을 벗어날 때 한 번만
/// 굳힌다. 매 이벤트마다 저장하면 색을 끄는 내내 파일 쓰기가 폭주한다.
function ColorSwatch({
  hex,
  title,
  disabled,
  onCommit,
}: {
  hex: string;
  title: string;
  disabled?: boolean;
  onCommit: (next: string) => void;
}) {
  const t = useT();
  const [draft, setDraft] = useState(hex);
  useEffect(() => setDraft(hex), [hex]);
  return (
    <input
      type="color"
      value={draft}
      disabled={disabled}
      title={title}
      aria-label={t.common.colorOf({ name: title })}
      onChange={(e) => setDraft(e.target.value)}
      onBlur={() => {
        if (draft.toLowerCase() !== hex.toLowerCase()) onCommit(draft);
      }}
      className="h-[24px] w-[30px] shrink-0 cursor-pointer bg-transparent p-0 disabled:opacity-40"
      style={{ border: 'var(--kt-border-w) solid var(--kt-border)', borderRadius: 'var(--kt-radius-sm)' }}
    />
  );
}

/// 형태 카드. 카드 자체가 그 실루엣으로 그려진다 — 모서리 · 테두리 · 그림자 ·
/// 점과 캡슐의 둥글기까지. 테마 카드가 팔레트를 미리 보이는 것과 같은 규칙이라,
/// 고르기 전에 형태가 눈에 들어온다.
function ShapeCard({
  preset,
  active,
  disabled,
  onPick,
}: {
  preset: ShapePreset;
  active: boolean;
  disabled?: boolean;
  onPick: () => void;
}) {
  const t = useT();
  const dot = `${5 * preset.roundness}px`;
  // 이름은 사전에서 — 서버가 주는 `label` 은 영어 한 벌뿐이라 화면 언어를 못 따른다
  // (테마 이름은 Tokyo Night 같은 고유명이라 그대로 두지만, 형태는 옮길 말이다).
  // 서버가 새 형태를 늘렸는데 사전이 아직 모르면 원래 라벨로 떨어진다.
  const name = t.appearance.shapeNames[preset.key] ?? preset.label;
  return (
    <button
      type="button"
      disabled={disabled}
      aria-current={active}
      onClick={() => !active && onPick()}
      className="h-[58px] w-[108px] px-3 pt-3 text-left disabled:opacity-40"
      style={{
        borderRadius: `${preset.radius_md}px`,
        border: `${preset.border_w}px solid var(--kt-border)`,
        background: 'var(--kt-surface)',
        boxShadow: [
          preset.shadow_offset > 0
            ? `${preset.shadow_offset}px ${preset.shadow_offset}px 0 rgba(0,0,0,.4)`
            : '',
          active ? '0 0 0 2px var(--kt-accent)' : '',
        ]
          .filter(Boolean)
          .join(', '),
      }}
    >
      <div className="flex items-center gap-1.5">
        <span
          className="inline-block h-[10px] w-[10px]"
          style={{ background: 'var(--kt-accent)', borderRadius: dot }}
        />
        <span
          className="inline-block h-[10px] w-[28px]"
          style={{ background: 'var(--kt-text-mute)', borderRadius: dot }}
        />
      </div>
      <div
        className="mt-2 text-[11.5px]"
        style={{
          color: active ? 'var(--kt-text)' : 'var(--kt-text-dim)',
          fontWeight: active ? 600 : 400,
        }}
      >
        {name}
      </div>
    </button>
  );
}

export function AppearanceTab({
  data,
  reload,
}: {
  data: AppearanceValues;
  reload: () => Promise<void>;
}) {
  const t = useT();
  const { busy, notice, run } = useSettingsAction(reload);
  const uiCount = data.palette_keys.length;
  const uiHex = data.palette_hex.slice(0, uiCount);
  const ansiHex = data.palette_hex.slice(uiCount);
  // 커스텀은 여러 벌일 수 있고(2026-08-15), 편집 칸은 **지금 입고 있는** 한 벌을
  // 고친다 — 어느 것을 고치는지는 위 카드 그리드의 선택 링이 말한다.
  const custom = data.custom_active !== '';
  const activeCustom = data.custom_themes.find((c) => c.slug === data.custom_active);
  // 사용자가 `settings.json` 에 임의 값을 넣을 수 있으니 동등이 아니라 가장 가까운
  // 프리셋을 선택으로 본다(네이티브와 같은 판정).
  const nearestContrast = data.contrasts.reduce((a, b) =>
    Math.abs(a.value - data.min_contrast) <= Math.abs(b.value - data.min_contrast) ? a : b
  );
  const atDefaultScale =
    Math.abs(data.ui_zoom - 1) < 0.01 && Math.abs(data.font_size - data.font_size_default) < 0.01;

  const commitHex = (index: number) => (next: string) =>
    void run('palette-hex', { id: String(index), label: next });

  return (
    <TabCard>
      <Notice notice={notice} />

      <Section title={t.appearance.theme} hint={t.appearance.themeHint}>
        <div className="grid grid-cols-[repeat(auto-fill,minmax(158px,1fr))] gap-3">
          {data.themes.map((p) => (
            <ThemeCard
              key={p.key}
              preset={p}
              active={p.key === data.theme}
              disabled={busy}
              onPick={() => void run('theme-mode', { id: p.key })}
            />
          ))}
        </div>

        {/* system 을 따를 때 밝기별로 입을 테마 — OS 는 밝기만 알려 주고 팔레트는
            여기서 배정한다(내장 라이트/다크에 묶이지 않게, 2026-08-15). system 이
            활성일 때만 펼친다: 다른 테마를 골라 둔 사람에게는 소음이다. */}
        {data.theme === 'system' && (
          <div className="mt-4 space-y-2">
            <p className="text-[13px] text-[var(--kt-text)]">{t.appearance.systemSlots}</p>
            <p className="text-[12px] text-[var(--kt-text-mute)]">
              {t.appearance.systemSlotsHint}
            </p>
            {(
              [
                ['light', t.appearance.systemLightSlot, data.theme_system_light],
                ['dark', t.appearance.systemDarkSlot, data.theme_system_dark],
              ] as const
            ).map(([slot, label, current]) => (
              <div key={slot} className="flex flex-wrap items-center gap-1.5">
                <span className="w-[88px] shrink-0 text-[12.5px] text-[var(--kt-text-dim)]">
                  {label}
                </span>
                {data.themes
                  .filter((p) => p.key !== 'system')
                  .map((p) => (
                    <button
                      key={p.key}
                      type="button"
                      disabled={busy}
                      aria-pressed={p.key === current}
                      onClick={() =>
                        p.key !== current && void run(`theme-system-${slot}`, { id: p.key })
                      }
                      className="px-2.5 py-1 text-[12px] disabled:opacity-40"
                      style={{
                        borderRadius: 'var(--kt-radius-sm)',
                        // 미리보기 배지: 그 테마의 실제 배경·글자색으로 물들인다 —
                        // 라벨만 나열하면 이름을 외운 사람만 고를 수 있다.
                        background: p.bg,
                        color: p.text,
                        boxShadow:
                          p.key === current
                            ? '0 0 0 2px var(--kt-accent)'
                            : `inset 0 0 0 1px var(--kt-border)`,
                      }}
                    >
                      {p.label}
                    </button>
                  ))}
              </div>
            ))}
          </div>
        )}
      </Section>

      {/* 팔레트는 custom 일 때만 열린다 — 프리셋 위에 직접 덧칠하게 하면 원래
          색으로 돌아갈 길이 없다: 프리셋은 불변, 편집은 복제본에. 복제본을 몇 벌
          두든 상관없으므로 「새로 만들기」는 늘 자리에 있다. */}
      <Section
        title={t.appearance.palette}
        hint={custom ? t.appearance.paletteHint : t.appearance.paletteLocked}
        right={
          <Button
            label={
              data.custom_themes.length ? t.appearance.paletteNew : t.appearance.paletteStart
            }
            disabled={busy}
            onClick={() => void run('start-custom-theme')}
          />
        }
      >
        {custom && (
          <>
            {/* 이름과 치우기는 편집 칸 위에 둔다 — 어느 팔레트를 고치는 중인지
                먼저 읽히고, 그 다음이 색이다. */}
            <div className="mb-4 flex flex-wrap items-center gap-2">
              <span className="text-[13px] text-[var(--kt-text-dim)]">
                {t.appearance.paletteName}
              </span>
              <TextField
                value={activeCustom?.label ?? ''}
                disabled={busy}
                className="w-[200px]"
                onCommit={(next) =>
                  void run('rename-custom-theme', { id: data.custom_active, label: next })
                }
              />
              <Button
                label={t.appearance.paletteDelete}
                disabled={busy}
                onClick={() =>
                  void run('delete-custom-theme', { id: data.custom_active })
                }
              />
            </div>

            <div className="grid grid-cols-[repeat(auto-fill,minmax(294px,1fr))] gap-x-2 gap-y-1">
              {data.palette_keys.map((key, i) => (
                <div key={key} className="flex items-center gap-2 py-1">
                  <ColorSwatch
                    hex={uiHex[i] ?? '#000000'}
                    title={key}
                    disabled={busy}
                    onCommit={commitHex(i)}
                  />
                  <span className="flex-1 text-[13px] text-[var(--kt-text)]">{key}</span>
                  <TextField
                    value={uiHex[i] ?? '#000000'}
                    disabled={busy}
                    mono
                    className="w-[104px]"
                    onCommit={commitHex(i)}
                  />
                </div>
              ))}
            </div>

            <p className="mb-2 mt-5 text-[13px] text-[var(--kt-text)]">{t.appearance.ansi}</p>
            <p className="mb-2 text-[12px] text-[var(--kt-text-mute)]">{t.appearance.ansiHint}</p>
            <div className="grid w-max grid-cols-8 gap-2">
              {ansiHex.map((hex, i) => (
                <ColorSwatch
                  key={i}
                  hex={hex}
                  title={`ansi ${i}`}
                  disabled={busy}
                  onCommit={commitHex(uiCount + i)}
                />
              ))}
            </div>

            <div className="mt-5">
              <Button
                label={t.appearance.paletteReset}
                disabled={busy}
                onClick={() => void run('reset-custom-theme')}
              />
            </div>
          </>
        )}
      </Section>

      <Section title={t.appearance.shape} hint={t.appearance.shapeHint}>
        <div className="flex flex-wrap gap-3">
          {data.shapes.map((s) => (
            <ShapeCard
              key={s.key}
              preset={s}
              active={s.key === data.shape}
              disabled={busy}
              onPick={() => void run('shape', { id: s.key })}
            />
          ))}
        </div>
      </Section>

      <Section title={t.appearance.accent} hint={t.appearance.accentHint}>
        <div className="flex flex-wrap gap-3.5">
          {data.accents.map((a) => {
            const on = a.name === data.accent;
            return (
              <button
                key={a.name}
                type="button"
                disabled={busy}
                aria-current={on}
                title={a.name}
                onClick={() => !on && void run('accent', { id: a.name })}
                className="h-[30px] w-[30px] disabled:opacity-40"
                style={{
                  background: a.hex,
                  borderRadius: 'var(--kt-dot-radius)',
                  // 선택은 글자색 링, hover 는 CSS 로 두지 않고 링만 — 스와치 자체가
                  // 색이라 배경을 바꿀 자리가 없다.
                  boxShadow: on ? '0 0 0 3px var(--kt-text)' : 'none',
                }}
              />
            );
          })}
        </div>
      </Section>

      <Section title={t.appearance.contrast} hint={t.appearance.contrastHint}>
        <div className="flex flex-wrap gap-2.5">
          {data.contrasts.map((c) => {
            const on = c.label === nearestContrast.label;
            return (
              <button
                key={c.label}
                type="button"
                disabled={busy}
                aria-current={on}
                onClick={() => !on && void run('min-contrast', { id: c.label })}
                className="h-[44px] w-[86px] px-2.5 pt-1.5 text-left disabled:opacity-40"
                style={{
                  borderRadius: 'var(--kt-radius-md)',
                  background: 'var(--kt-surface)',
                  boxShadow: on
                    ? '0 0 0 2px var(--kt-accent)'
                    : 'inset 0 0 0 var(--kt-border-w) var(--kt-border)',
                }}
              >
                {/* 샘플은 그 임계를 실제로 적용한 색이라, 고르기 전에 얼마나
                    끌어올리는지 눈으로 비교된다. */}
                <div className="text-[12px]" style={{ color: c.sample }}>
                  {t.appearance.contrastSample}
                </div>
                <div
                  className="mt-1 text-[11px]"
                  style={{
                    color: on ? 'var(--kt-text)' : 'var(--kt-text-dim)',
                    fontWeight: on ? 600 : 400,
                  }}
                >
                  {/* 라벨은 곧 식별자라(액션의 id 가 이 문자열이다) 서버에서 옮길 수
                      없다 — 화면 이름만 사전에서 덧씌운다. */}
                  {t.appearance.contrastNames[c.label] ?? c.label}
                </div>
              </button>
            );
          })}
        </div>
      </Section>

      <Section title={t.appearance.fontSize} hint={t.appearance.fontSizeHint}>
        <Stepper
          text={String(Math.round(data.font_size))}
          disabled={busy}
          onStep={(d) => void run('font-size-delta', { id: String(d) })}
        />
      </Section>

      {/* 배율은 여태 키에만 있었다. 지금 몇 %인지 화면 어디에도 안 적혀서, 폰트와
          배율을 번갈아 만지다 UI 가 어긋나도 어느 쪽이 범인인지 알 수가 없었다. */}
      <Section title={t.appearance.uiZoom} hint={t.appearance.uiZoomHint}>
        <Stepper
          text={`${Math.round(data.ui_zoom * 100)}%`}
          disabled={busy}
          onStep={(d) => void run('ui-zoom-delta', { id: String(d) })}
          right={
            // 이미 기본값이면 누를 것이 없다 — 눌러도 아무 일 없는 버튼은 고장으로
            // 읽히므로 그때는 흐리게 두고 막는다.
            <Button
              label={t.appearance.scaleReset}
              disabled={busy || atDefaultScale}
              onClick={() => void run('reset-scale')}
            />
          }
        />
      </Section>
    </TabCard>
  );
}
