import { Notice, Row, Segmented, TabCard, TextField, Toggle, useSettingsAction } from './controls';
import { useLang, useT } from './lang';
import type { Lang } from './strings';
import type { SettingsValues } from './types';

/// 저장값이 `"last"`·`"home"` 둘 중 하나가 아니면 그 문자열 자체가 사용자가 고른
/// 경로다 — 네이티브 폼의 `cwd_is` 와 같은 판정.
function cwdKey(mode: string): string {
  return mode === 'last' || mode === 'home' ? mode : 'custom';
}

/// `"system"` 은 `"app"` 의 옛 저장값이라 같은 칸으로 읽는다.
function openKey(mode: string): string {
  return mode === 'system' ? 'app' : mode;
}

/// 설명 문구에 박히는 주 수식키. 실제 바인딩이 이미 갈려 있는데(macOS=Cmd, 그 밖
/// =Ctrl) 문구만 Cmd 로 고정하면 Windows 사용자에게 키보드에 없는 키를 안내한다.
const PRIMARY_MOD = navigator.userAgent.includes('Mac') ? 'Cmd' : 'Ctrl';

export function GeneralTab({
  data,
  reload,
}: {
  data: SettingsValues['general'];
  reload: () => Promise<void>;
}) {
  const t = useT();
  const { lang, setLang } = useLang();
  const { busy, notice, run } = useSettingsAction(reload);
  const cwd = cwdKey(data.cwd_mode);
  const open = openKey(data.file_open_mode);

  return (
    <TabCard>
      <Notice notice={notice} />

      {/* 언어를 맨 위에 두는 이유는 이 칸이 **다른 모든 칸을 읽는 수단**이기
          때문이다. 말이 안 통하는 사람에게는 이 한 칸을 찾는 게 첫 일이라,
          아래로 내리면 정작 필요한 사람이 못 찾는다.

          `run` 을 안 쓰고 `setLang` 을 쓴다 — 이 값은 GUI 를 거치지 않고 파일로
          바로 가고, 화면은 회신을 기다리지 않고 즉시 바뀌어야 한다. */}
      <Row label={t.language.title} desc={[t.language.hint]}>
        <Segmented
          value={lang}
          disabled={false}
          options={[
            { key: 'ko', label: t.language.ko },
            { key: 'en', label: t.language.en },
          ]}
          onPick={(key) => setLang(key as Lang)}
        />
      </Row>

      <Row label={t.general.startupFolder} desc={[t.general.startupFolderHint]}>
        <Segmented
          value={cwd}
          disabled={busy}
          options={[
            { key: 'last', label: t.general.cwdLast },
            { key: 'home', label: t.general.cwdHome },
            { key: 'custom', label: t.general.cwdCustom },
          ]}
          onPick={(key) => void run('cwd-mode', { id: key })}
        />
      </Row>
      {/* 폭을 다 쓰는 칸이라 그 행 아래로 내린다 — 오른쪽 칸에 밀어 넣으면 라벨과
          겹친다(네이티브도 같은 이유로 줄을 통째로 쓴다). */}
      {cwd === 'custom' && (
        <div className="mb-4">
          <TextField
            value={data.cwd_mode}
            disabled={busy}
            mono
            onCommit={(next) => void run('cwd-path', { label: next })}
          />
        </div>
      )}

      <Row label={t.general.fileTree} desc={[t.general.fileTreeHint]}>
        <Toggle
          on={data.file_tree_default}
          disabled={busy}
          onToggle={() => void run('toggle-file-tree')}
        />
      </Row>

      <Row label={t.general.statusBar} desc={[t.general.statusBarHint]}>
        <Toggle
          on={data.footer_default}
          disabled={busy}
          onToggle={() => void run('toggle-footer')}
        />
      </Row>

      {/* 두 하단바 높이. 자유값이 아니라 프리셋인 건 안에 얹히는 것(게이지·칩)
          크기가 정해져 있어 쓸 수 있는 폭이 사실상 세 칸이어서다. */}
      <Row label={t.general.windowBarH} desc={[t.general.windowBarHHint]}>
        <Segmented
          value={String(data.status_bar_h)}
          disabled={busy}
          options={[
            { key: '20', label: t.general.barHeightLow },
            { key: '24', label: t.general.barHeightMid },
            { key: '30', label: t.general.barHeightHigh },
          ]}
          onPick={(key) => void run('status-bar-h', { id: key })}
        />
      </Row>

      <Row label={t.general.paneBarH} desc={[t.general.paneBarHHint]}>
        <Segmented
          value={String(data.pane_footer_h)}
          disabled={busy}
          options={[
            { key: '24', label: t.general.barHeightLow },
            { key: '30', label: t.general.barHeightMid },
            { key: '36', label: t.general.barHeightHigh },
          ]}
          onPick={(key) => void run('pane-footer-h', { id: key })}
        />
      </Row>

      <Row label={t.general.fileOpen} desc={[t.general.fileOpenHint]}>
        <Segmented
          value={open}
          disabled={busy}
          options={[
            { key: 'builtin', label: t.general.openBuiltin },
            { key: 'app', label: t.general.openApp },
            { key: 'terminal', label: t.general.openTerminal },
          ]}
          onPick={(key) => void run('file-open-mode', { id: key })}
        />
      </Row>
      {open === 'app' && (
        // 설치된 것만 뜬다. 마지막 「기본 앱」은 OS 연결 프로그램 — 목록에 없는
        // 앱을 쓰는 사람의 탈출구다. 앱 이름 자체는 옮길 말이 아니라 데이터다.
        <div className="mb-4 flex flex-wrap gap-1.5">
          <Segmented
            value={data.file_open_app}
            disabled={busy}
            options={[
              ...data.apps.map((a) => ({ key: a.name, label: a.short })),
              { key: '', label: t.general.defaultApp },
            ]}
            onPick={(key) => void run('file-open-app', { id: key })}
          />
        </div>
      )}
      {open === 'terminal' && (
        <div className="mb-4">
          <p className="mb-1.5 text-[12px] text-[var(--kt-text-mute)]">
            {t.general.terminalCmdHint}
          </p>
          <TextField
            value={data.file_open_cmd}
            disabled={busy}
            mono
            onCommit={(next) => void run('file-open-cmd', { label: next })}
          />
        </div>
      )}

      <Row label={t.general.autosave} desc={[t.general.autosaveHint({ mod: PRIMARY_MOD })]}>
        <Segmented
          value={String(data.autosave_ms)}
          disabled={busy}
          options={[
            { key: '0', label: t.general.autosaveOff },
            { key: '1000', label: '1s' },
            { key: '3000', label: '3s' },
            { key: '10000', label: '10s' },
          ]}
          onPick={(key) => void run('autosave-delay', { id: key })}
        />
      </Row>

      <Row label={t.general.tabPosition} desc={[t.general.tabPositionHint]}>
        <Segmented
          value={data.tabs_on_top ? 'top' : 'side'}
          disabled={busy}
          options={[
            { key: 'top', label: t.general.tabTop },
            { key: 'side', label: t.general.tabSide },
          ]}
          onPick={(key) => void run('tab-position', { id: key })}
        />
      </Row>

      <Row label={t.general.cursorShape} desc={[t.general.cursorShapeHint]}>
        <Segmented
          value={data.cursor_shape}
          disabled={busy}
          options={[
            { key: 'block', label: t.general.cursorBlock },
            { key: 'bar', label: t.general.cursorBar },
            { key: 'underline', label: t.general.cursorUnderline },
          ]}
          onPick={(key) => void run('cursor-shape', { id: key })}
        />
      </Row>

      {/* 굵기는 bar·underline 에만 쓰인다 — block 은 셀을 통째로 채운다. 줄 자체를
          감추면 「왜 사라졌지」가 되므로 두되, 무엇에 쓰이는지 곁글로 밝힌다. */}
      <Row
        label={t.general.cursorThickness}
        desc={[
          data.cursor_shape === 'block'
            ? t.general.cursorThicknessIdle
            : t.general.cursorThicknessHint,
        ]}
      >
        <Segmented
          value={String(Math.round(data.cursor_thickness))}
          disabled={busy}
          options={[1, 2, 3, 4].map((n) => ({ key: String(n), label: `${n}px` }))}
          onPick={(key) => void run('cursor-thickness', { id: key })}
        />
      </Row>

      <Row label={t.general.mousePointer} desc={[t.general.mousePointerHint]}>
        <Segmented
          value={data.mouse_cursor === 'ibeam' ? 'ibeam' : 'arrow'}
          disabled={busy}
          options={[
            { key: 'arrow', label: t.general.pointerArrow },
            { key: 'ibeam', label: t.general.pointerIbeam },
          ]}
          onPick={(key) => void run('mouse-cursor', { id: key })}
        />
      </Row>

      {/* 트랙패드와 고해상도 마우스휠은 같은 델타로 들어와 자동으로 못 가른다 —
          한쪽에 맞추면 다른 쪽이 어긋나므로 고르는 몫을 사람에게 넘긴다. */}
      <Row label={t.general.scroll} desc={[t.general.scrollHint]}>
        <Segmented
          value={String(data.wheel_gain_x100)}
          disabled={busy}
          options={[
            { key: '30', label: t.general.scrollTrackpad },
            { key: '60', label: t.general.scrollNormal },
            { key: '100', label: t.general.scrollMouse },
            { key: '150', label: t.general.scrollFast },
          ]}
          onPick={(key) => void run('wheel-gain', { id: key })}
        />
      </Row>
    </TabCard>
  );
}
