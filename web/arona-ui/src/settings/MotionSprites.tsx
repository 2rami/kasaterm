import { useCallback, useEffect, useRef, useState } from 'react';
import { RotateCcw, Upload } from 'lucide-react';
import { useT } from './lang';
import type { Strings } from './strings';

/// 한 모션의 상태 — 서버가 정본이다. 프레임 수를 여기서 받는 이유는 그게 로더의
/// 규약(walk 만 6, 나머지 4)이라서다. 화면이 자기 상수로 칸을 그리면 로더가
/// 바뀔 때 칸 수만 옛 값으로 남아, 사용자는 다 넣었는데 앱은 한 장이 모자란다고
/// 판단하는 상태가 된다.
type MotionStatus = {
  motion: string;
  frames: number;
  ext: string;
  /// `user` = 내가 넣은 그림, `bundled` = 앱 기본, `none` = 기본조차 없는 자리.
  source: 'user' | 'bundled' | 'none';
};

/// 모션 이름·설명은 사전에 있다. 여기서 하는 건 **서버가 준 모션 키를 그 사전
/// 자리로 잇는 것**뿐 — 모르는 키가 와도 화면이 빈 글자로 뜨지 않게 키를 그대로
/// 제목에 쓴다.
function motionLabel(t: Strings, motion: string): { title: string; when: string } {
  const known = t.motion[motion as 'idle' | 'walk' | 'wave' | 'cheer' | 'profile' | 'gif'];
  return typeof known === 'object' && known !== null && 'title' in known
    ? known
    : { title: motion, when: '' };
}

function sourceLabel(t: Strings, source: MotionStatus['source']): string {
  if (source === 'user') return t.motion.sourceUser;
  if (source === 'bundled') return t.motion.sourceBundled;
  return t.motion.sourceNone;
}

/// 미리보기 애니메이션 간격 — 네이티브(render.rs)와 같은 값이라야 "walk 일 땐
/// 이렇게 보인다"가 실제와 같은 속도로 보인다.
const FRAME_MS: Record<string, number> = { walk: 140 };
const DEFAULT_FRAME_MS = 200;

function spriteUrl(slug: string, motion: string, frame: number, ver: number): string {
  return `/character-sprite?slug=${encodeURIComponent(slug)}&motion=${motion}&frame=${frame}&v=${ver}`;
}

/// ArrayBuffer → base64. 한 번에 `String.fromCharCode(...)` 로 펼치면 인자 수
/// 상한에 걸려 큰 PNG 에서 통째로 터진다 — 청크로 나눠 붙인다.
function toBase64(buf: ArrayBuffer): string {
  const bytes = new Uint8Array(buf);
  let s = '';
  for (let i = 0; i < bytes.length; i += 0x8000) {
    s += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
  }
  return btoa(s);
}

/// 파일 여러 장을 사람이 기대하는 순서로 — `frame-2` 가 `frame-10` 보다 앞이다.
function byName(files: File[]): File[] {
  return [...files].sort((a, b) => a.name.localeCompare(b.name, undefined, { numeric: true }));
}

/// 지금 쓰는 프레임 한 장을 그대로 다시 올릴 수 있는 형태로 가져온다. 한 칸만
/// 갈아 끼울 때 나머지 칸을 채우는 데 쓴다 — 저장이 벌 단위라 "이 한 장만"을
/// 서버에 보낼 방법이 없기 때문이다.
async function fetchFrameBase64(
  t: Strings,
  slug: string,
  motion: string,
  frame: number
): Promise<string> {
  const res = await fetch(spriteUrl(slug, motion, frame, Date.now()));
  if (!res.ok) throw new Error(t.motion.errReadFrame({ index: frame + 1 }));
  return toBase64(await res.arrayBuffer());
}

/// 이미지의 실제 픽셀 크기. 한 모션의 프레임이 서로 다른 크기면 그 모션은 통째로
/// 기본 도트로 돌아가므로(로더 규약), 올리기 전에 잡아 알려 준다.
function imageSize(file: File): Promise<{ w: number; h: number }> {
  return new Promise((resolve, reject) => {
    const url = URL.createObjectURL(file);
    const img = new Image();
    img.onload = () => {
      URL.revokeObjectURL(url);
      resolve({ w: img.naturalWidth, h: img.naturalHeight });
    };
    img.onerror = () => {
      URL.revokeObjectURL(url);
      // 파일 이름이 이미 무엇이 잘못됐는지 말한다 — 번역할 문장이 아니다.
      reject(new Error(file.name));
    };
    img.src = url;
  });
}

/// 캐릭터 한 명의 모션별 그림 설정.
///
/// 저장이 **벌 단위**인 것이 이 화면의 모든 모양을 정한다. 로더는 한 모션의
/// 프레임이 전부 있을 때만 그 벌을 쓰고, 하나라도 없으면 조용히 기본 도트로
/// 돌아간다. 그래서 한 칸만 바꾸는 조작도 나머지 칸을 함께 다시 올린다 —
/// 사용자에게 "절반만 올라간 상태"를 만들 방법을 주지 않는다.
export function MotionSprites({ slug }: { slug: string }) {
  const t = useT();
  const [motions, setMotions] = useState<MotionStatus[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  // 캐시버스터. 저장하면 올려서 브라우저가 옛 그림을 다시 그리지 않게 한다.
  const [ver, setVer] = useState(() => Date.now());

  const load = useCallback(async () => {
    try {
      const res = await fetch(`/character-sprite-status?slug=${encodeURIComponent(slug)}`);
      const out = (await res.json()) as { motions?: MotionStatus[] } | null;
      setMotions(out?.motions ?? []);
      setError(out?.motions ? null : t.motion.statusFailed);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
    // t 를 의존성에 넣지 않는다 — 언어를 바꿀 때마다 로스터를 다시 읽을 이유가
    // 없고, 넣으면 언어 전환이 화면 깜빡임을 부른다. 오류 문구는 다음 실패부터
    // 새 언어로 뜬다.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [slug]);

  useEffect(() => {
    setMotions(null);
    setVer(Date.now());
    void load();
  }, [load]);

  return (
    <section className="mt-6">
      <h3 className="text-[13px] font-medium text-[var(--kt-text)]">{t.motion.title}</h3>
      <p className="mt-0.5 text-[12px] text-[var(--kt-text-mute)]">{t.motion.hint}</p>

      {error && <p className="mt-3 text-[12px] text-[var(--kt-danger)]">{error}</p>}
      {!motions && !error && (
        <p className="mt-3 text-[12px] text-[var(--kt-text-mute)]">{t.common.loading}</p>
      )}

      <div className="mt-3 flex flex-col gap-3">
        {motions?.map((m) => (
          <MotionRow
            key={m.motion}
            slug={slug}
            status={m}
            ver={ver}
            onChanged={() => {
              setVer(Date.now());
              void load();
            }}
          />
        ))}
      </div>
    </section>
  );
}

function MotionRow({
  slug,
  status,
  ver,
  onChanged,
}: {
  slug: string;
  status: MotionStatus;
  ver: number;
  onChanged: () => void;
}) {
  const t = useT();
  const [busy, setBusy] = useState(false);
  const [msg, setMsg] = useState<{ ok: boolean; text: string } | null>(null);
  const pick = useRef<HTMLInputElement>(null);
  const label = motionLabel(t, status.motion);
  const missing = status.source === 'none';

  async function post(body: unknown): Promise<void> {
    setBusy(true);
    setMsg(null);
    try {
      const res = await fetch('/character-sprite', {
        method: 'POST',
        // text/plain 은 CORS simple request 라 preflight(OPTIONS)가 안 뜬다.
        // application/json 이면 preflight 가 붙고, post() 만 걸린 라우트는
        // OPTIONS 에 405 를 답해 요청이 조용히 죽는다.
        headers: { 'Content-Type': 'text/plain' },
        body: JSON.stringify(body),
      });
      const out = (await res.json()) as { ok: boolean; error?: string };
      if (!out.ok) {
        setMsg({ ok: false, text: out.error || t.detail.saveFailed });
        return;
      }
      setMsg({ ok: true, text: t.motion.changed });
      onChanged();
    } catch (e) {
      setMsg({ ok: false, text: e instanceof Error ? e.message : String(e) });
    } finally {
      setBusy(false);
    }
  }

  /// 파일 몇 장을 `at` 번째 칸부터 채워 **벌 전체**를 다시 올린다. 나머지 칸은
  /// 지금 쓰는 그림을 그대로 다시 보낸다.
  async function apply(files: File[], at: number) {
    const picked = byName(files).slice(0, status.frames - at);
    if (picked.length === 0) return;
    setBusy(true);
    setMsg(null);
    try {
      const wrong = picked.find((f) => !f.name.toLowerCase().endsWith(`.${status.ext}`));
      if (wrong) throw new Error(t.motion.errExt({ ext: status.ext.toUpperCase() }));
      // 크기가 다르면 그 모션이 통째로 기본 도트로 돌아간다 — 올린 뒤 "왜 안
      // 바뀌지"가 되는 대신 여기서 막는다.
      const sizes = await Promise.all(picked.map(imageSize));
      const first = sizes[0];
      if (sizes.some((s) => s.w !== first.w || s.h !== first.h)) {
        throw new Error(t.motion.errSizeMismatch);
      }
      const next = await Promise.all(
        Array.from({ length: status.frames }, async (_, i) => {
          const f = picked[i - at];
          if (f) return toBase64(await f.arrayBuffer());
          if (missing) throw new Error(t.motion.errNeedAll({ count: status.frames }));
          return fetchFrameBase64(t, slug, status.motion, i);
        })
      );
      await post({ slug, motion: status.motion, frames: next });
    } catch (e) {
      setMsg({ ok: false, text: e instanceof Error ? e.message : String(e) });
      setBusy(false);
    }
  }

  return (
    <div
      className="flex gap-4 p-3"
      style={{
        borderRadius: 'var(--kt-radius-md)',
        background: 'var(--kt-surface)',
        boxShadow: 'inset 0 0 0 var(--kt-border-w) var(--kt-border)',
      }}
    >
      <MotionPreview t={t} slug={slug} status={status} ver={ver} />

      <div className="min-w-0 flex-1">
        <div className="flex flex-wrap items-center gap-2">
          <span className="text-[13px] font-medium text-[var(--kt-text)]">{label.title}</span>
          <span
            className="px-1.5 py-0.5 text-[11px]"
            style={{
              borderRadius: 'var(--kt-radius-sm)',
              background: 'var(--kt-bg)',
              color:
                status.source === 'user' ? 'var(--kt-accent)' : 'var(--kt-text-mute)',
            }}
          >
            {sourceLabel(t, status.source)}
          </span>
          <span className="text-[11px] text-[var(--kt-text-mute)]">
            {t.motion.spec({ ext: status.ext.toUpperCase(), count: status.frames })}
          </span>
          {busy && (
            <span className="text-[11px] text-[var(--kt-text-mute)]">{t.common.saving}</span>
          )}
          {msg && (
            <span
              className="text-[11px]"
              style={{ color: msg.ok ? 'var(--kt-text-dim)' : 'var(--kt-danger)' }}
            >
              {msg.text}
            </span>
          )}
        </div>
        <p className="mt-0.5 text-[12px] text-[var(--kt-text-mute)]">{label.when}</p>

        <div className="mt-2 flex flex-wrap items-center gap-1.5">
          {Array.from({ length: status.frames }, (_, i) => (
            <FrameSlot
              key={i}
              t={t}
              index={i}
              src={missing ? null : spriteUrl(slug, status.motion, i, ver)}
              disabled={busy}
              onFiles={(files) => void apply(files, i)}
            />
          ))}
        </div>

        <div className="mt-2 flex items-center gap-2">
          <input
            ref={pick}
            type="file"
            accept={status.ext === 'gif' ? 'image/gif' : 'image/png'}
            multiple={status.frames > 1}
            className="hidden"
            onChange={(e) => {
              const files = Array.from(e.target.files ?? []);
              // 같은 파일을 다시 골라도 change 가 뜨게 비운다.
              e.target.value = '';
              void apply(files, 0);
            }}
          />
          <SmallButton disabled={busy} onClick={() => pick.current?.click()}>
            <Upload size={12} />
            {t.motion.pick({ count: status.frames })}
          </SmallButton>
          {status.source === 'user' && (
            <SmallButton
              disabled={busy}
              onClick={() => void post({ slug, motion: status.motion, clear: true })}
            >
              <RotateCcw size={12} />
              {t.motion.reset}
            </SmallButton>
          )}
        </div>
      </div>
    </div>
  );
}

/// 그 모션이 화면에서 실제로 어떻게 보이는지 — 프레임을 순환시켜 보여 준다.
///
/// 프레임을 전부 겹쳐 두고 투명도만 바꾸는 이유는 `src` 를 갈아 끼우면 첫 바퀴에
/// 매 프레임이 새로 로드되어 애니가 끊겨 보이기 때문이다. 겹쳐 두면 브라우저가
/// 한 번만 받아 온다.
function MotionPreview({
  t,
  slug,
  status,
  ver,
}: {
  t: Strings;
  slug: string;
  status: MotionStatus;
  ver: number;
}) {
  const [cur, setCur] = useState(0);
  const n = status.frames;

  useEffect(() => {
    if (n <= 1) return;
    const ms = FRAME_MS[status.motion] ?? DEFAULT_FRAME_MS;
    const id = window.setInterval(() => setCur((c) => (c + 1) % n), ms);
    return () => window.clearInterval(id);
  }, [n, status.motion]);

  return (
    <div
      className="relative h-[88px] w-[88px] shrink-0"
      style={{ borderRadius: 'var(--kt-radius-sm)', background: 'var(--kt-bg)' }}
    >
      {status.source === 'none' ? (
        <span className="absolute inset-0 flex items-center justify-center text-[11px] text-[var(--kt-text-mute)]">
          {t.motion.empty}
        </span>
      ) : (
        Array.from({ length: n }, (_, i) => (
          <img
            key={i}
            className="kt-face absolute inset-0 h-full w-full object-contain p-1"
            style={{ opacity: i === cur ? 1 : 0 }}
            src={spriteUrl(slug, status.motion, i, ver)}
            alt=""
          />
        ))
      )}
    </div>
  );
}

/// 프레임 한 칸. 여기에 파일을 떨어뜨리면 **그 칸부터** 채운다 — 한 장이면 그
/// 칸만, 여러 장이면 뒤 칸까지. 덕분에 "이 프레임만 다시 그렸어요"와 "한 벌
/// 통째로"가 같은 조작으로 된다.
function FrameSlot({
  t,
  index,
  src,
  disabled,
  onFiles,
}: {
  t: Strings;
  index: number;
  src: string | null;
  disabled: boolean;
  onFiles: (files: File[]) => void;
}) {
  const [over, setOver] = useState(false);
  const pick = useRef<HTMLInputElement>(null);

  return (
    <button
      type="button"
      disabled={disabled}
      title={t.motion.slotTitle({ index: index + 1 })}
      onClick={() => pick.current?.click()}
      onDragOver={(e) => {
        e.preventDefault();
        setOver(true);
      }}
      onDragLeave={() => setOver(false)}
      onDrop={(e) => {
        e.preventDefault();
        setOver(false);
        const files = Array.from(e.dataTransfer.files);
        if (files.length > 0) onFiles(files);
      }}
      className="relative h-[52px] w-[52px]"
      style={{
        borderRadius: 'var(--kt-radius-sm)',
        background: 'var(--kt-bg)',
        boxShadow: `inset 0 0 0 ${over ? '2px' : 'var(--kt-border-w)'} ${
          over ? 'var(--kt-accent)' : 'var(--kt-border)'
        }`,
        opacity: disabled ? 0.5 : 1,
      }}
    >
      <input
        ref={pick}
        type="file"
        multiple
        className="hidden"
        onChange={(e) => {
          const files = Array.from(e.target.files ?? []);
          e.target.value = '';
          if (files.length > 0) onFiles(files);
        }}
      />
      {src && (
        <img className="kt-face h-full w-full object-contain p-0.5" src={src} alt="" />
      )}
      <span className="absolute bottom-0 right-1 text-[10px] text-[var(--kt-text-mute)]">
        {index + 1}
      </span>
    </button>
  );
}

function SmallButton({
  children,
  disabled,
  onClick,
}: {
  children: React.ReactNode;
  disabled: boolean;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      disabled={disabled}
      onClick={onClick}
      className="flex items-center gap-1.5 px-2 py-1 text-[12px]"
      style={{
        borderRadius: 'var(--kt-radius-sm)',
        background: 'var(--kt-bg)',
        color: 'var(--kt-text)',
        boxShadow: 'inset 0 0 0 var(--kt-border-w) var(--kt-border)',
        opacity: disabled ? 0.5 : 1,
      }}
    >
      {children}
    </button>
  );
}
