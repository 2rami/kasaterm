import { useCallback, useEffect, useRef, useState } from 'react';
import { postAction } from './api';
import { Section } from './controls';
import { serverText, useT } from './lang';
import type { ThemeGenState } from './types';

/// 그림 생성(참조 그림 → 스프라이트) 화면 조각들. 엔진 고르기는 테마 탭에,
/// 굽기는 캐릭터 상세에 얹는다 — 상태는 컴포넌트가 스스로 읽는다(MotionSprites
/// 와 같은 규칙: 부모의 로스터 저장 흐름과 안 섞인다).

async function fetchState(): Promise<ThemeGenState | null> {
  try {
    const res = await fetch('/settings/themegen/state');
    if (!res.ok) return null;
    return (await res.json()) as ThemeGenState;
  } catch {
    return null;
  }
}

/// 참조 그림 미리보기 URL. `bust` 는 업로드 직후 캐시를 깨는 값 — 서버가 mtime 을
/// 안 주므로 화면이 업로드 시각으로 만든다.
function refUrl(slug: string, bust: number): string {
  return `/settings/themegen/ref?slug=${encodeURIComponent(slug)}&t=${bust}`;
}

/// 참조 그림 업로드. `slug` 가 있으면 그 캐릭터의 참조 교체, 없이 `name`(파일
/// 이름)만 주면 새 캐릭터 등록까지. Content-Type 을 text/plain 으로 못 박는 건
/// 기존 라우트들과 같은 CORS 규약이다 — 서버는 바이트의 매직 넘버로 읽는다.
async function uploadRef(
  file: File,
  target: { slug?: string; name?: string }
): Promise<{ ok: boolean; slug?: string; error?: string }> {
  const q = new URLSearchParams();
  if (target.slug) q.set('slug', target.slug);
  if (target.name) q.set('name', target.name);
  try {
    const res = await fetch(`/settings/themegen/ref?${q}`, {
      method: 'POST',
      headers: { 'Content-Type': 'text/plain' },
      body: file,
    });
    return (await res.json()) as { ok: boolean; slug?: string; error?: string };
  } catch (e) {
    return { ok: false, error: e instanceof Error ? e.message : String(e) };
  }
}

const segStyle = (on: boolean, disabled?: boolean): React.CSSProperties => ({
  borderRadius: 'var(--kt-radius-sm)',
  background: on ? 'var(--kt-accent)' : 'var(--kt-surface)',
  color: on ? 'var(--kt-on-accent)' : 'var(--kt-text)',
  fontWeight: on ? 600 : 400,
  boxShadow: on ? 'none' : 'inset 0 0 0 var(--kt-border-w) var(--kt-border)',
  opacity: disabled ? 0.5 : 1,
});

/// 테마 탭의 「그림 생성 엔진」 — 엔진 셋 중 고르고, 나노바나나면 키를 받는다.
/// 준비 안 된 엔진도 고를 수는 있다(사유가 그 자리에 뜬다) — 회색으로 죽여 두면
/// 「왜 안 눌리는지」를 알 길이 없다.
export function ThemeGenEngine() {
  const t = useT();
  const [state, setState] = useState<ThemeGenState | null>(null);
  const [busy, setBusy] = useState(false);
  /// 키 입력 버퍼. null = 편집 중 아님(마스킹된 저장값 표시). 포커스하면 빈 칸에서
  /// 시작한다 — 마스킹 문자열을 「이어서 고치는」 것은 애초에 성립하지 않는다.
  const [keyDraft, setKeyDraft] = useState<string | null>(null);

  const reload = useCallback(async () => {
    setState(await fetchState());
  }, []);
  useEffect(() => {
    void reload();
  }, [reload]);

  // 라우트가 없는(구버전 앱) 인스턴스에서는 섹션을 통째로 숨긴다 — 반쯤 그려
  // 두면 고장으로 읽힌다.
  if (!state) return null;
  const sel = state.providers.find((p) => p.kind === state.provider);

  async function pick(kind: string) {
    setBusy(true);
    try {
      await postAction('theme-gen-provider', { id: kind });
      await reload();
    } finally {
      setBusy(false);
    }
  }

  async function saveKey() {
    const v = keyDraft?.trim();
    setKeyDraft(null);
    if (!v) return;
    setBusy(true);
    try {
      await postAction('gemini-key', { label: v });
      await reload();
    } finally {
      setBusy(false);
    }
  }

  return (
    <Section title={t.themegen.engine} hint={t.themegen.engineHint}>
      <div className="flex flex-wrap gap-1.5">
        {state.providers.map((p) => (
          <button
            key={p.kind}
            type="button"
            disabled={busy}
            onClick={() => void pick(p.kind)}
            className="min-h-[40px] px-3 py-1.5 text-[13px]"
            style={segStyle(state.provider === p.kind, busy)}
          >
            {p.label}
            {!p.available && (
              <span className="ml-1.5 text-[11px] opacity-70">{t.themegen.notReadyTag}</span>
            )}
          </button>
        ))}
      </div>
      {sel && !sel.available && (
        <p className="mt-2 text-[12px]" style={{ color: 'var(--kt-danger)' }}>
          {t.themegen.notReady({ why: sel.why })}
        </p>
      )}
      {state.provider === 'nanobanana' && (
        <div className="mt-3">
          <label className="block text-[12px] text-[var(--kt-text-mute)]">
            {t.themegen.geminiKey}
          </label>
          <p className="mt-0.5 text-[11px] text-[var(--kt-text-mute)]">
            {t.themegen.geminiKeyHint}
          </p>
          <input
            className="kt-field mt-1.5 w-full max-w-[360px]"
            // 저장값은 마스킹으로만 보여 준다 — 실값을 화면에 되돌리지 않는다.
            value={keyDraft ?? state.gemini_key_masked}
            spellCheck={false}
            onFocus={() => setKeyDraft('')}
            onChange={(e) => setKeyDraft(e.target.value)}
            onBlur={() => void saveKey()}
            onKeyDown={(e) => {
              if (e.key === 'Enter') e.currentTarget.blur();
              if (e.key === 'Escape') {
                setKeyDraft(null);
                e.currentTarget.blur();
              }
            }}
          />
        </div>
      )}
    </Section>
  );
}

/// 캐릭터 격자의 「+ 새 캐릭터」 — 참조 그림 한 장을 고르면 파일명으로 로스터에
/// 오르고, 새로 뜬 카드에서 굽는다. 번들 테마에서는 부모가 아예 안 그린다.
export function NewStudentCard({
  onAdded,
}: {
  /// 등록이 끝난 뒤 — 부모가 로스터를 다시 읽고 안내 문구를 띄운다.
  onAdded: (msg: string) => void;
}) {
  const t = useT();
  const [uploading, setUploading] = useState(false);
  const fileRef = useRef<HTMLInputElement>(null);

  async function onFile(file: File) {
    setUploading(true);
    try {
      const out = await uploadRef(file, { name: file.name });
      if (out.ok && out.slug) onAdded(t.themegen.newStudentAdded({ name: out.slug }));
      else onAdded(out.error || t.common.failed);
    } finally {
      setUploading(false);
      if (fileRef.current) fileRef.current.value = '';
    }
  }

  return (
    <button
      type="button"
      disabled={uploading}
      onClick={() => fileRef.current?.click()}
      title={t.themegen.newStudentHint}
      className="flex flex-col items-center justify-center gap-1 px-2 py-3 disabled:opacity-40"
      style={{
        borderRadius: 'var(--kt-radius-sm)',
        border: '1px dashed var(--kt-border)',
        color: 'var(--kt-text-mute)',
      }}
    >
      <span className="text-[13px]">
        {uploading ? t.themegen.uploading : t.themegen.newStudent}
      </span>
      <input
        ref={fileRef}
        type="file"
        accept="image/*"
        hidden
        onChange={(e) => {
          const f = e.target.files?.[0];
          if (f) void onFile(f);
        }}
      />
    </button>
  );
}

/// 캐릭터 상세의 「그림 생성」 — 참조를 놓고 버튼 하나로 전 모션+프사를 굽는다.
/// 잡이 도는 동안 2초 간격으로 상태를 다시 읽고, 경과 시계를 함께 보인다 —
/// 몇 분짜리 작업이라 시계가 없으면 멈춘 것인지 도는 것인지 알 수 없다.
export function ThemeGenSection({ slug }: { slug: string }) {
  const t = useT();
  const [state, setState] = useState<ThemeGenState | null>(null);
  const [refBust, setRefBust] = useState(0);
  /// 참조가 있는지는 미리보기 img 의 성패로 안다 — 상태 라우트에 실어 달랄 수도
  /// 있지만, 어차피 그림을 그려야 하니 그 요청 하나가 판정을 겸한다.
  const [hasRef, setHasRef] = useState(false);
  const [uploading, setUploading] = useState(false);
  const [now, setNow] = useState(() => Date.now());
  /// 시작이 거부된 사유(이미 굽는 중·참조 없음). 서버가 일부러 거부 코드를 주는데
  /// 삼키면 「눌렀는데 아무 일도 없는」 화면이 된다 — 화면 판정(hasRef·running)과
  /// 서버 판정이 어긋나는 틈이 실제로 있다(다른 창에서 지웠거나 폴링 2초 사이).
  const [startErr, setStartErr] = useState<string | null>(null);
  const fileRef = useRef<HTMLInputElement>(null);

  const reload = useCallback(async () => {
    setState(await fetchState());
  }, []);
  useEffect(() => {
    setHasRef(false);
    setRefBust(Date.now());
    setStartErr(null);
    void reload();
  }, [slug, reload]);

  const job = state?.jobs?.[slug];
  const running = !!job && !job.failed_reason && job.phase !== 'done';
  useEffect(() => {
    if (!running) return;
    const poll = window.setInterval(() => void reload(), 2000);
    const tick = window.setInterval(() => setNow(Date.now()), 1000);
    return () => {
      window.clearInterval(poll);
      window.clearInterval(tick);
    };
  }, [running, reload]);

  if (!state) return null;
  if (!state.active_theme) {
    return (
      <Section title={t.themegen.section} hint={t.themegen.bundledNo}>
        {null}
      </Section>
    );
  }

  async function onFile(file: File) {
    setUploading(true);
    try {
      const out = await uploadRef(file, { slug });
      if (out.ok) {
        setRefBust(Date.now());
        setHasRef(false); // img 가 새 주소로 다시 판정한다
      }
    } finally {
      setUploading(false);
      if (fileRef.current) fileRef.current.value = '';
    }
  }

  const sel = state.providers.find((p) => p.kind === state.provider);
  const provOk = !!sel?.available;
  const secs = job ? Math.max(0, Math.floor((now - job.started_ms) / 1000)) : 0;

  return (
    <Section title={t.themegen.section} hint={t.themegen.sectionHint}>
      <div
        className="flex items-center gap-3"
        // 창 어디든이 아니라 이 조각에 떨어뜨린다 — 웹뷰에서는 DOM 드롭이 이 길이다.
        onDragOver={(e) => e.preventDefault()}
        onDrop={(e) => {
          e.preventDefault();
          const f = e.dataTransfer.files?.[0];
          if (f) void onFile(f);
        }}
      >
        <img
          className="h-[72px] w-[72px] object-contain"
          style={{
            borderRadius: 'var(--kt-radius-sm)',
            boxShadow: 'inset 0 0 0 var(--kt-border-w) var(--kt-border)',
            display: hasRef ? 'block' : 'none',
          }}
          src={refUrl(slug, refBust)}
          alt=""
          onLoad={() => setHasRef(true)}
          onError={() => setHasRef(false)}
        />
        <div className="flex flex-col items-start gap-1.5">
          <button
            type="button"
            disabled={uploading || running}
            onClick={() => fileRef.current?.click()}
            className="min-h-[40px] px-3 py-1.5 text-[13px] disabled:opacity-50"
            style={segStyle(false)}
          >
            {uploading
              ? t.themegen.uploading
              : hasRef
                ? t.themegen.repickRef
                : t.themegen.pickRef}
          </button>
          <input
            ref={fileRef}
            type="file"
            accept="image/*"
            hidden
            onChange={(e) => {
              const f = e.target.files?.[0];
              if (f) void onFile(f);
            }}
          />
        </div>
      </div>

      <div className="mt-3">
        {job?.failed_reason ? (
          <p className="text-[12px]" style={{ color: 'var(--kt-danger)' }}>
            {t.themegen.failed({ reason: job.failed_reason })}
          </p>
        ) : running && job ? (
          <p className="text-[12px] text-[var(--kt-text)]">
            {job.provider} — {job.phase_label}
            {job.detail && ` · ${job.detail}`}
            {' · '}
            {t.themegen.elapsed({
              min: Math.floor(secs / 60),
              sec: String(secs % 60).padStart(2, '0'),
            })}
          </p>
        ) : job ? (
          <p className="text-[12px] text-[var(--kt-text-mute)]">{t.themegen.done}</p>
        ) : null}

        {!running &&
          (hasRef && provOk ? (
            <>
            <button
              type="button"
              className="mt-2 min-h-[40px] px-3 py-1.5 text-[13px]"
              style={segStyle(true)}
              onClick={() =>
                void postAction('theme-gen-start', { id: slug }).then((out) => {
                  setStartErr(
                    out.error || out.error_code
                      ? serverText(t, out.error_code, out.error, out.error_args)
                      : null
                  );
                  return reload();
                })
              }
            >
              {job ? t.themegen.restart : t.themegen.start}
            </button>
            {startErr && (
              <p className="mt-1 text-[12px]" style={{ color: 'var(--kt-danger)' }}>
                {startErr}
              </p>
            )}
          </>
          ) : (
            <p className="mt-1 text-[12px] text-[var(--kt-text-mute)]">
              {!hasRef
                ? t.themegen.needRef
                : t.themegen.engineNotReady({ why: sel?.why || '' })}
            </p>
          ))}
      </div>
    </Section>
  );
}
