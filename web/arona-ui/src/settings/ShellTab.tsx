import { useState } from 'react';
import { Notice, Row, Segmented, TabCard, TextField, useSettingsAction } from './controls';
import type { SettingsValues } from './types';

/// 네이티브가 칸으로 내주는 셸 셋. 값이 곧 실행 경로라 라벨과 따로 든다.
const PRESETS = [
  { key: '', label: 'System default' },
  { key: '/bin/zsh', label: 'zsh' },
  { key: '/bin/bash', label: 'bash' },
];

export function ShellTab({
  data,
  reload,
}: {
  data: SettingsValues['shell'];
  reload: () => Promise<void>;
}) {
  const { busy, notice, run } = useSettingsAction(reload);
  const isPreset = PRESETS.some((p) => p.key === data.shell);
  /// 「Custom」을 눌러 칸을 연 상태. 네이티브에선 그 칸에 커서가 가는 것이 곧 이
  /// 상태인데, 저장된 값이 아직 프리셋이라 서버는 알 수 없다 — 그래서 화면이 든다.
  const [custom, setCustom] = useState(false);
  const showField = !isPreset || custom;

  return (
    <TabCard>
      <Notice notice={notice} />
      <Row label="Default shell" desc={['새 pane 의 셸 (비우면 시스템 $SHELL)']}>
        <Segmented
          value={showField ? 'custom' : data.shell}
          options={[...PRESETS, { key: 'custom', label: 'Custom' }]}
          disabled={busy}
          onPick={(key) => {
            if (key === 'custom') {
              setCustom(true);
              return;
            }
            setCustom(false);
            void run('shell-preset', { id: key });
          }}
        />
      </Row>
      {showField && (
        <TextField
          value={isPreset ? '' : data.shell}
          disabled={busy}
          mono
          placeholder="/opt/homebrew/bin/fish"
          onCommit={(next) => void run('shell-custom', { label: next })}
        />
      )}
    </TabCard>
  );
}
