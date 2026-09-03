import { useState } from 'react';
import { Notice, Row, Segmented, TabCard, TextField, useSettingsAction } from './controls';
import { useT } from './lang';
import type { SettingsValues } from './types';

/// 네이티브가 칸으로 내주는 셸 셋. 값이 곧 실행 경로라 사전으로 옮길 것이 아니다 —
/// 「System default」만 말이고 나머지는 프로그램 이름이다.
const PRESET_KEYS = ['', '/bin/zsh', '/bin/bash'];

export function ShellTab({
  data,
  reload,
}: {
  data: SettingsValues['shell'];
  reload: () => Promise<void>;
}) {
  const t = useT();
  const { busy, notice, run } = useSettingsAction(reload);
  const isPreset = PRESET_KEYS.includes(data.shell);
  /// 「직접 지정」을 눌러 칸을 연 상태. 네이티브에선 그 칸에 커서가 가는 것이 곧 이
  /// 상태인데, 저장된 값이 아직 프리셋이라 서버는 알 수 없다 — 그래서 화면이 든다.
  const [custom, setCustom] = useState(false);
  const showField = !isPreset || custom;

  return (
    <TabCard>
      <Notice notice={notice} />
      <Row label={t.shell.defaultShell} desc={[t.shell.defaultShellHint]}>
        <Segmented
          value={showField ? 'custom' : data.shell}
          options={[
            { key: '', label: t.shell.systemDefault },
            { key: '/bin/zsh', label: 'zsh' },
            { key: '/bin/bash', label: 'bash' },
            { key: 'custom', label: t.shell.custom },
          ]}
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
          label={t.shell.custom}
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
