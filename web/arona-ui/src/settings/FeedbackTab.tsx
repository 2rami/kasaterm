import { useEffect, useState } from 'react';
import { Button, Notice, Row, Section, TabCard, Toggle, useSettingsAction } from './controls';
import { useT } from './lang';
import type { SettingsValues } from './types';

export function FeedbackTab({
  data,
  reload,
}: {
  data: SettingsValues['feedback'];
  reload: () => Promise<void>;
}) {
  const t = useT();
  const { busy, notice, run } = useSettingsAction(reload);
  const draftKey = 'kasaterm.settings.feedback-draft';
  const [body, setBody] = useState(() => localStorage.getItem(draftKey) ?? '');
  const [submitted, setSubmitted] = useState<{ body: string; attempt: number } | null>(null);
  useEffect(() => {
    if (body) localStorage.setItem(draftKey, body);
    else localStorage.removeItem(draftKey);
  }, [body]);
  const empty = body.trim() === '';
  const sending = busy || !!data.sending;

  useEffect(() => {
    if (!data.sending) return;
    const timer = window.setInterval(() => { void reload(); }, 1200);
    return () => window.clearInterval(timer);
  }, [data.sending, reload]);

  useEffect(() => {
    if (submitted === null || data.sending || !data.delivery || data.attempt !== submitted.attempt) return;
    if (!data.delivery.error) setBody((current) => current === submitted.body ? '' : current);
    setSubmitted(null);
  }, [submitted, data.sending, data.delivery, data.attempt]);

  async function save() {
    const text = body;
    const attempt = (data.attempt ?? 0) + 1;
    if (await run('send-feedback', { label: text })) setSubmitted({ body: text, attempt });
  }

  return (
    <TabCard>
      <Notice notice={notice} />
      <Notice notice={data.delivery ? { ok: !data.delivery.error, msg: data.delivery.message } : null} />

      <Section title={t.feedback.body} hint={t.feedback.bodyHint}>
        <p className="mb-3 text-sm">{t.feedback.destination}</p>
        <textarea
          className="kt-field h-[200px] w-full max-w-[560px] resize-y"
          value={body}
          disabled={sending}
          placeholder={t.feedback.placeholder}
          onChange={(e) => setBody(e.target.value)}
          onKeyDown={(e) => {
            // Esc 는 여기선 포커스 해제다. 전역 Esc(창 닫기)가 위에 걸려 있어서,
            // 막지 않으면 쓰던 글이 든 채로 창이 통째로 닫힌다.
            e.stopPropagation();
            if (e.key === 'Escape') e.currentTarget.blur();
          }}
        />
      </Section>

      {/* 진단 줄(버전·OS)은 서버가 만든 값이라 옮길 말이 아니다. */}
      <Row label={t.feedback.diag} desc={[data.diag]}>
        <Toggle
          label={t.feedback.diag}
          on={data.diag_on}
          disabled={sending}
          onToggle={() => void run('toggle-feedback-diag')}
        />
      </Row>

      <div className="flex gap-2">
        <Button
          label={sending ? t.feedback.sending : t.feedback.save}
          primary
          disabled={sending || empty}
          onClick={() => void save()}
        />
        <Button
          label={t.feedback.openFolder}
          disabled={busy}
          onClick={() => void run('open-feedback-dir')}
        />
      </div>
    </TabCard>
  );
}
