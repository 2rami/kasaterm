import { useEffect, useRef, useState } from 'react';
import { ArrowLeft } from 'lucide-react';
import { useT } from './lang';
import { MotionSprites } from './MotionSprites';
import { faceUrl } from './types';
import type { Character } from './types';

/// 성격 자동 저장이 기다리는 시간. 사람이 문장을 쓰다 멈추는 틈보다 길어야
/// 한다 — 짧으면 쉼표 하나 찍을 때마다 파일을 쓴다.
const PERSONA_DEBOUNCE_MS = 1500;

type SaveBody = { name: string; persona?: string; new_name?: string };
type SaveResult = { ok: boolean; name?: string; error?: string };

/// 캐릭터 한 명의 상세 — 이름과 성격을 고친다.
///
/// **이 화면이 이 이행의 목적이다.** 네이티브 판은 캐럿·선택·조합을 손으로 구현한
/// GPU 폼이라 Cmd+Z 도 Cmd+V 도 드래그 선택도 없었다. 여기서는 `<input>` 과
/// `<textarea>` 를 그대로 쓰므로 그 전부가 공짜로 온다 — 브라우저 IME 를 쓰니
/// 한글 조합도 마찬가지다.
///
/// 저장 시점이 필드마다 다른 건 되돌릴 수 있는 정도가 달라서다:
/// - **성격**은 타이핑이 멈추면 자동 저장 + blur. 덮어써도 잃는 게 없다.
/// - **이름은 blur 에서만.** 이름은 로스터의 키라 중간 상태("아로")가 저장되면
///   그 순간 persona·그림·색 조회가 통째로 그 이름을 따라가고, 되돌릴 창구가
///   UI 에 없다. 자동 저장은 여기에 쓰면 안 된다.
export function CharacterDetail({
  character,
  onBack,
  onSaved,
}: {
  character: Character;
  onBack: () => void;
  /// 저장이 실제로 반영된 뒤 — 이름이 바뀌었으면 그 새 이름을 준다. 부모가
  /// 로스터를 다시 읽어야 화면과 파일이 어긋나지 않는다.
  onSaved: (name: string) => void;
}) {
  const t = useT();
  const [name, setName] = useState(character.name);
  const [persona, setPersona] = useState(character.persona);
  const [toast, setToast] = useState<{ ok: boolean; msg: string } | null>(null);
  const [saving, setSaving] = useState(false);
  // 조합 중(한글·일본어 등)에는 자동 저장을 걸지 않는다 — 조합이 확정되기 전의
  // 값을 보내면 마지막 음절이 잘린 채 파일에 박힌다.
  const composing = useRef(false);
  const timer = useRef<number | null>(null);
  // 저장된 값. draft 와 같으면 보낼 게 없다(빈 POST 로 파일을 흔들지 않는다).
  const savedName = useRef(character.name);
  const savedPersona = useRef(character.persona);

  // 다른 캐릭터로 넘어가면 draft 를 새로 시드한다. 안 하면 앞 사람의 성격이
  // 남아 있다가 blur 한 번에 이 사람에게 저장된다.
  //
  // 의존성이 **`slug` 하나**인 게 핵심이다. `name`·`persona` 를 넣으면 저장
  // 직후 부모가 로스터를 다시 읽어 prop 이 새 값으로 오는 것까지 「다른 캐릭터」로
  // 읽혀, 방금 띄운 「저장했어요」가 즉시 지워진다(실측: 저장은 됐는데 화면이
  // 아무 말도 안 했다). slug 는 이름을 바꿔도 그대로라 사람이 바뀐 것만 잡는다.
  //
  // eslint-disable-next-line react-hooks/exhaustive-deps
  useEffect(() => {
    setName(character.name);
    setPersona(character.persona);
    savedName.current = character.name;
    savedPersona.current = character.persona;
    setToast(null);
  }, [character.slug]);

  useEffect(() => () => {
    if (timer.current) window.clearTimeout(timer.current);
  }, []);

  async function post(body: SaveBody): Promise<void> {
    setSaving(true);
    try {
      const res = await fetch('/settings/character', {
        method: 'POST',
        // text/plain 은 CORS simple request 라 preflight(OPTIONS)가 안 뜬다.
        // application/json 이면 preflight 가 붙고, post() 만 걸린 라우트는
        // OPTIONS 에 405 를 답해 요청이 조용히 죽는다.
        headers: { 'Content-Type': 'text/plain' },
        body: JSON.stringify(body),
      });
      const out = (await res.json()) as SaveResult;
      if (!out.ok) {
        // 거부되면 draft 를 저장된 값으로 되돌린다 — 안 되돌리면 화면엔 새 값이
        // 남아 저장된 것처럼 보이는데 파일은 옛 값 그대로다.
        setName(savedName.current);
        setPersona(savedPersona.current);
        setToast({ ok: false, msg: out.error || t.detail.saveFailed });
        return;
      }
      // 회신의 이름을 쓴다. 성격은 됐지만 이름이 거부된 경우 `ok` 는 true 인데
      // 이름만 옛것으로 남아 오므로, 요청값으로 화면을 그리면 어긋난다.
      const finalName = out.name || body.name;
      const renamed = body.new_name != null && finalName !== body.new_name;
      savedName.current = finalName;
      setName(finalName);
      if (body.persona != null) savedPersona.current = body.persona;
      setToast(
        renamed
          ? { ok: false, msg: t.detail.renameRejected }
          : { ok: true, msg: t.common.saved }
      );
      onSaved(finalName);
    } catch (e) {
      setToast({ ok: false, msg: e instanceof Error ? e.message : String(e) });
    } finally {
      setSaving(false);
    }
  }

  function savePersona(text: string) {
    if (text === savedPersona.current) return;
    void post({ name: savedName.current, persona: text });
  }

  function schedulePersona(text: string) {
    if (timer.current) window.clearTimeout(timer.current);
    if (composing.current) return;
    timer.current = window.setTimeout(() => savePersona(text), PERSONA_DEBOUNCE_MS);
  }

  function saveName() {
    const next = name.trim();
    if (next === savedName.current) {
      // 공백만 지운 것도 화면에는 반영해 둔다 — 안 그러면 blur 했는데 뒤 공백이
      // 남아 있어 저장이 안 된 것처럼 보인다.
      setName(savedName.current);
      return;
    }
    void post({ name: savedName.current, new_name: next });
  }

  return (
    <div>
      <div className="mb-4 flex items-center gap-3">
        <button
          type="button"
          onClick={() => {
            // 나가기 전에 미룬 저장을 굳힌다 — 타이머가 도는 중에 목록으로
            // 돌아가면 컴포넌트가 사라져 그 저장이 영영 안 나간다.
            if (timer.current) window.clearTimeout(timer.current);
            savePersona(persona);
            onBack();
          }}
          className="flex items-center gap-1.5 px-2.5 py-1.5 text-[13px]"
          style={{
            borderRadius: 'var(--kt-radius-sm)',
            background: 'var(--kt-surface)',
            color: 'var(--kt-text)',
            boxShadow: 'inset 0 0 0 var(--kt-border-w) var(--kt-border)',
          }}
        >
          <ArrowLeft size={14} />
          {t.detail.back}
        </button>
        <span className="text-[13px] text-[var(--kt-text-mute)]">
          {character.school} · {character.slug}
        </span>
        {saving && (
          <span className="text-[12px] text-[var(--kt-text-mute)]">{t.common.saving}</span>
        )}
        {toast && (
          <span
            className="text-[12px]"
            style={{ color: toast.ok ? 'var(--kt-text-dim)' : 'var(--kt-danger)' }}
          >
            {toast.msg}
          </span>
        )}
      </div>

      <div
        className="flex gap-6 p-6"
        style={{
          borderRadius: 'var(--kt-radius-md)',
          background: 'var(--kt-bg)',
          boxShadow: 'inset 0 0 0 var(--kt-border-w) var(--kt-border)',
        }}
      >
        <div className="flex w-[120px] shrink-0 flex-col items-center gap-2">
          <img className="kt-face w-full" src={faceUrl(character.slug)} alt="" />
          <span
            className="h-[6px] w-[40px]"
            style={{
              background: character.header_color,
              borderRadius: 'var(--kt-dot-radius)',
            }}
          />
        </div>

        <div className="min-w-0 flex-1">
          <label className="block text-[13px] font-medium text-[var(--kt-text)]">
            {t.detail.name}
          </label>
          <p className="mt-0.5 text-[12px] text-[var(--kt-text-mute)]">{t.detail.nameHint}</p>
          <input
            className="kt-field mt-2 w-full max-w-[320px]"
            value={name}
            onChange={(e) => setName(e.target.value)}
            onCompositionStart={() => {
              composing.current = true;
            }}
            onCompositionEnd={() => {
              composing.current = false;
            }}
            onBlur={saveName}
            onKeyDown={(e) => {
              if (e.key === 'Enter') e.currentTarget.blur();
              if (e.key === 'Escape') setName(savedName.current);
            }}
          />

          <label className="mt-6 block text-[13px] font-medium text-[var(--kt-text)]">
            {t.detail.persona}
          </label>
          <p className="mt-0.5 text-[12px] text-[var(--kt-text-mute)]">{t.detail.personaHint}</p>
          <textarea
            className="kt-field mt-2 min-h-[280px] w-full resize-y font-[inherit] leading-relaxed"
            value={persona}
            onChange={(e) => {
              setPersona(e.target.value);
              schedulePersona(e.target.value);
            }}
            onCompositionStart={() => {
              composing.current = true;
              if (timer.current) window.clearTimeout(timer.current);
            }}
            onCompositionEnd={(e) => {
              composing.current = false;
              schedulePersona(e.currentTarget.value);
            }}
            onBlur={() => {
              if (timer.current) window.clearTimeout(timer.current);
              savePersona(persona);
            }}
          />
          <p className="mt-1 text-[11px] text-[var(--kt-text-mute)]">
            {t.detail.charCount({ count: persona.length })}
          </p>

          {/* 그림은 이름·성격과 저장 경로가 다르다(파일 폴더 vs 로스터 json) —
              그래도 같은 화면에 두는 이유는 사람이 "이 캐릭터"를 한자리에서
              손보기 때문이다. 자기 상태는 스스로 읽어 오므로 위 저장과 섞이지
              않는다. */}
          <MotionSprites slug={character.slug} />
        </div>
      </div>
    </div>
  );
}
