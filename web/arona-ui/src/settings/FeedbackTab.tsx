import { useState } from 'react';
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
  /// 본문은 화면이 든다. 매 글자를 서버로 보낼 이유가 없고(저장 버튼이 따로 있다),
  /// 보내면 그때마다 앱의 편집 버퍼를 덮어써 네이티브 화면에서 쓰던 글과 다툰다.
  const [body, setBody] = useState('');
  const empty = body.trim() === '';

  async function save() {
    // **성공했을 때만** 비운다. 실패했는데 비우면 쓰던 글이 통째로 사라지고,
    // 화면에는 「저장 실패」 문구만 남아 되돌릴 방법이 없다.
    if (await run('save-feedback', { label: body })) setBody('');
  }

  return (
    <TabCard>
      <Notice notice={notice} />

      <Section title={t.feedback.body} hint={t.feedback.bodyHint}>
        <textarea
          className="kt-field h-[200px] w-full max-w-[560px] resize-y"
          value={body}
          disabled={busy}
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
          on={data.diag_on}
          disabled={busy}
          onToggle={() => void run('toggle-feedback-diag')}
        />
      </Row>

      <div className="flex gap-2">
        <Button
          label={t.feedback.save}
          primary
          disabled={busy || empty}
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
