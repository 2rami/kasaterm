/// 설정 화면 문구. **한국어가 정본이고 영어가 그것을 따른다** — `en` 이 `Strings`
/// 타입이라, 한국어에 문구를 더하면 영어를 안 쓴 자리를 컴파일러가 잡는다.
///
/// 값이 끼는 문구는 문자열을 이어 붙이지 말고 **함수로** 둔다. 어순이 언어마다
/// 달라서(「79명 — 눌러서」 vs 「Click one of 79」) 조각으로 나누면 영어가
/// 부자연스러워지고, 조각 순서를 바꾸려면 화면 코드를 고쳐야 한다.

export type Lang = 'ko' | 'en';

export const ko = {
  nav: {
    settings: '설정',
    general: '일반',
    appearance: '모양',
    shell: '셸',
    /// 칸 이름은 「Agent」다 — 안에 Claude 와 Codex 로그인이 나란히 있어서, 한쪽
    /// 이름을 칸 이름으로 쓰면 다른 쪽이 곁방살이로 읽힌다. 키가 `claude` 로
    /// 남은 건 서버 JSON 의 키라서다(네이티브 `SettingsCat::Claude` 와 짝).
    claude: 'Agent',
    theme: '캐릭터',
    feedback: '피드백',
    /// 아직 네이티브 창에만 있는 칸에 붙는 꼬리표.
    native: '네이티브',
    portHint: '이 포트의 kasaterm',
  },

  titles: {
    general: { title: '일반', hint: '창·작업 폴더·파일 열기' },
    appearance: { title: '모양', hint: '색·모양·글꼴' },
    shell: { title: '셸', hint: '셸과 편집기' },
    claude: { title: 'Agent', hint: '계정과 실행 방식' },
    theme: { title: '캐릭터', hint: '학생 그림과 페르소나, 캐릭터 목록' },
    feedback: { title: '피드백', hint: '쓰다가 걸린 것을 남겨 주세요' },
  },

  common: {
    loading: '읽는 중…',
    failed: '안 됐어요',
    saving: '저장 중…',
    saved: '저장했어요',
    notWebYet: '이 화면은 아직 네이티브 설정 창에 있어요.',
    fetchFailed: (a: { path: string; error: string }) => `${a.path} 실패: ${a.error}`,
    palette: (a: { theme: string; accent: string; radius: number }) =>
      `팔레트 ${a.theme} · accent ${a.accent} · 모서리 ${a.radius}px`,
    valuesUnavailable: '이 인스턴스는 설정 값을 안 알려 줘요',
    stepDown: '한 칸 줄이기',
    stepUp: '한 칸 늘리기',
    colorOf: (a: { name: string }) => `${a.name} 색`,
  },

  language: {
    title: '언어',
    hint: '이 설정 화면의 말이에요 — 고르면 바로 바뀌어요',
    ko: '한국어',
    en: 'English',
  },

  theme: {
    section: '테마',
    sectionHint: '폴더 하나가 테마 하나 — 이름·색·그림이 한 벌로 바뀝니다',
    newTheme: '+ 새 테마',
    rename: '이름',
    folder: '폴더',
    remove: '치우기',
    inUse: '쓰는 중',
    members: (a: { count: number }) => `${a.count}명`,
    persona: '말투',
    personaHint: '켜면 캐릭터 말투로 대답해요 — 새로 여는 pane 부터',
    images: '그림 폴더',
    imagesHint:
      '캐릭터를 눌러 모션별로 바꾸는 게 편해요. 여러 명을 한꺼번에 갈아 끼울 때만 폴더를 여세요',
    openImages: '이미지 폴더 열기',
    openRoster: '로스터 열기',
    refresh: '새로고침',
    characters: '캐릭터',
    charactersHint: (a: { count: number }) =>
      `${a.count}명 — 캐릭터를 눌러 성격과 그림을 고치세요`,
  },

  detail: {
    back: '목록',
    name: '이름',
    nameHint: '로스터의 키예요 — 성격·색·그림이 이 이름을 따라가요. 칸을 벗어날 때 저장돼요.',
    persona: '성격',
    personaHint: '이 캐릭터로 뜨는 pane 의 claude 가 이대로 말해요 — 이미 도는 pane 은 안 바뀌어요.',
    charCount: (a: { count: number }) => `${a.count}자 · 타이핑을 멈추면 저장돼요`,
    renameRejected: '이름은 못 바꿨어요 — 성격만 저장했어요',
    saveFailed: '저장에 실패했어요',
  },

  motion: {
    title: '그림',
    hint: '모션마다 따로 바꿀 수 있어요. 한 모션은 프레임이 전부 있어야 쓰이고, 한 칸만 바꿔도 나머지는 지금 것 그대로 다시 저장돼요.',
    statusFailed: '그림 상태를 못 읽었어요',
    idle: { title: '평소', when: '아무 일도 없을 때 서 있는 모습이에요. 세션 시작 배너에도 나와요.' },
    walk: { title: '작업 중', when: 'claude 가 일하는 동안 스피너 옆에서 제자리걸음을 해요.' },
    wave: { title: '승인 대기', when: '주황색 선택지가 떠서 대답을 기다릴 때 손을 흔들어요.' },
    cheer: { title: '턴 완료', when: '한 턴이 끝나면 양팔을 들어요.' },
    profile: { title: '프사', when: '사이드바와 메시지 아바타에 쓰는 얼굴이에요.' },
    gif: { title: '대기 애니', when: '사이드바 카드에서 움직이는 얼굴이에요.' },
    sourceUser: '내가 넣은 그림',
    sourceBundled: '기본 그림',
    sourceNone: '그림 없음',
    spec: (a: { ext: string; count: number }) => `${a.ext} ${a.count}장`,
    pick: (a: { count: number }) => (a.count > 1 ? `${a.count}장 고르기` : '고르기'),
    reset: '기본으로',
    changed: '바꿨어요',
    empty: '없음',
    slotTitle: (a: { index: number }) =>
      `${a.index}번째 프레임 — 파일을 끌어다 놓거나 눌러서 고르세요`,
    errExt: (a: { ext: string }) => `${a.ext} 파일만 넣을 수 있어요`,
    errSizeMismatch: '프레임 크기가 서로 달라요 — 같은 크기로 맞춰 주세요',
    errNeedAll: (a: { count: number }) => `${a.count}장을 한 번에 넣어 주세요`,
    errReadFrame: (a: { index: number }) => `${a.index}번째 그림을 못 읽었어요`,
  },

  /// 라벨은 **한국어판에서 한국어로 옮긴다.** 네이티브 폼은 라벨만 영어였는데
  /// (「Startup folder」), 좌측 nav 와 페이지 제목이 이미 한국어가 된 마당에 그 안만
  /// 영어로 두면 한 화면에 두 말이 섞인다. 영어판이 네이티브의 원래 라벨을 그대로
  /// 물려받으므로 잃는 것도 없다(2026-08-15 판단).
  general: {
    startupFolder: '시작 폴더',
    startupFolderHint: '새 창과 탭이 열리는 위치',
    cwdLast: '마지막 폴더',
    cwdHome: '홈',
    cwdCustom: '직접 지정',
    fileTree: '파일 트리 기본으로 열기',
    fileTreeHint: '시작할 때 파일 트리 사이드바 열기',
    statusBar: 'pane 상태바 기본으로 켜기',
    statusBarHint: '각 pane 아래 경로 · 브랜치 · diff 바 표시',
    barHeightLow: '낮게',
    barHeightMid: '보통',
    barHeightHigh: '높게',
    windowBarH: '창 하단바 높이',
    windowBarHHint: '맨 아래 계정 한도 줄의 두께',
    paneBarH: 'pane 하단바 높이',
    paneBarHHint: '각 pane 아래 경로 · 브랜치 줄의 두께',
    fileOpen: '파일 열기',
    fileOpenHint: '파일 트리에서 파일을 열 때 무엇으로 열지',
    openBuiltin: '내장 편집기',
    openApp: '앱',
    openTerminal: '터미널',
    defaultApp: '기본 앱',
    /// `{}` 가 문구 안에 그대로 있어야 한다 — 자리표시자가 아니라 사용자가 칠 글자다.
    terminalCmdHint: '새 pane 에서 CLI 편집기 — {} 는 파일 경로 자리',
    autosave: '편집기 자동 저장',
    autosaveHint: (a: { mod: string }) => `타자가 멎으면 조용히 저장 (${a.mod}+S 는 그대로)`,
    autosaveOff: '끔',
    tabPosition: '탭 위치',
    tabPositionHint: '윈도우 탭을 타이틀바 또는 사이드바에 표시',
    tabTop: '위',
    tabSide: '옆',
    cursorShape: '커서 모양',
    cursorShapeHint: '셀을 채우는 블록, Ghostty 식 세로선, 또는 밑줄',
    cursorBlock: '블록',
    cursorBar: '세로선',
    cursorUnderline: '밑줄',
    cursorThickness: '커서 굵기',
    cursorThicknessHint: '세로선·밑줄의 굵기',
    /// 블록은 셀을 통째로 채워 고를 것이 없다. 줄을 감추면 「왜 사라졌지」가 되므로
    /// 두되, 무엇에 쓰이는 값인지로 말을 바꾼다.
    cursorThicknessIdle: '세로선·밑줄을 고르면 적용돼요',
    mousePointer: '마우스 포인터',
    mousePointerHint: '터미널 위 마우스 포인터 모양 (입력칸 위 I-beam 은 그대로예요)',
    pointerArrow: '화살표',
    pointerIbeam: 'I-beam',
    scroll: '스크롤 감도',
    scrollHint: '트랙패드 기준이 기본이에요. 휠이 굼뜨면 올리세요',
    scrollTrackpad: '트랙패드',
    scrollNormal: '보통',
    scrollMouse: '마우스',
    scrollFast: '빠르게',
  },

  appearance: {
    theme: '테마',
    themeHint:
      'UI + 터미널 ANSI 팔레트가 함께 바뀌어요 — System 은 OS 의 밝게/어둡게를 따라갑니다',
    systemSlots: '시스템을 따를 때',
    systemSlotsHint: 'OS 가 밝게/어둡게로 바뀔 때 각각 어떤 테마를 입을지 골라요',
    systemLightSlot: '밝게일 때',
    systemDarkSlot: '어둡게일 때',
    palette: '팔레트',
    paletteHint: '칸을 누르면 색 선택기가 열려요 — #rrggbb 를 쳐도 됩니다',
    paletteLocked: '지금 테마를 복제해 색을 한 칸씩 고칠 수 있어요 — 여러 개 만들어 둬도 돼요',
    paletteStart: '지금 테마를 복제해 시작',
    /// 이미 만들어 둔 게 있어도 이 버튼은 **새로** 만든다 — 이어서 고치려면 그
    /// 팔레트 카드를 고르면 되므로, 문구가 그 둘을 헷갈리게 하면 안 된다.
    paletteNew: '+ 새 팔레트',
    paletteName: '이름',
    paletteDelete: '이 팔레트 치우기',
    paletteReset: '베이스 값으로 되돌리기',
    ansi: '터미널 ANSI',
    ansiHint: '터미널 본문 16색 — 윗줄 0..7, 아랫줄 8..15 (bright)',
    shape: '형태',
    shapeHint: '모서리 · 점 · 토글의 실루엣 (팔레트와 별개 축)',
    accent: '강조색',
    accentHint: '선택 영역 · 커서 · 링크 색',
    contrast: '최소 대비',
    contrastHint: '앱이 직접 지정한 색이 배경에 묻힐 때만 끌어올려요 (dim 은 제외)',
    /// 대비 견본에 쓰는 글자. 그 언어에서 **실제로 읽는 글자**여야 대비가 눈에 온다.
    contrastSample: 'Aa 가나',
    fontSize: '글자 크기',
    fontSizeHint: '터미널 셀 폰트 크기 — UI 배율과는 별개인 기준값이에요',
    uiZoom: 'UI 배율',
    uiZoomHint: '크롬·사이드바·pane 이 함께 커져요',
    scaleReset: '1:1 로 되돌리기',
  },

  shell: {
    defaultShell: '기본 셸',
    defaultShellHint: '새 pane 의 셸 (비우면 시스템 $SHELL)',
    systemDefault: '시스템 기본',
    custom: '직접 지정',
  },

  claude: {
    shim: 'shim 주입',
    shimHint: '끄면 순정 Claude — 페르소나 · 프록시 · 훅 없음 (재시작 필요)',
    account: '계정',
    accountHint: '다음에 뜨는 claude 부터 이 계정으로 — 돌고 있는 세션은 그대로예요',
    codexAccount: 'Codex 계정',
    codexAccountHint: '다음에 뜨는 codex 부터 이 계정으로 — 돌고 있는 세션은 그대로예요',
    addAccount: (a: { provider: string }) => `+ ${a.provider} 계정 추가`,
    rename: '이름',
    reauth: '다시 로그인',
    reauthIsolated: '빈 창으로',
    /// 두 버튼이 왜 갈리는지 — 기본이 쓰던 브라우저라 그 창에 붙어 있는 계정이
    /// 그대로 승인된다. 다른 계정을 붙이려던 사람이 결과를 보고 놀라는 자리라,
    /// 고르기 전에 결과를 말해 준다.
    browserHint:
      '「다시 로그인」은 쓰던 브라우저에서 — 그 브라우저에 지금 로그인된 계정으로 붙어요. 다른 계정을 붙이려면 「빈 창으로」를 쓰세요.',
    removeSlot: '빼기',
    inUse: '쓰는 중',
    labelPlaceholder: '별명 (비우면 이메일로 불러요)',
    autoSwitch: '자동 전환',
    autoSwitchHint:
      '한도가 차면 다음에 뜨는 claude 부터 다음 계정으로 — 떠난 계정은 풀릴 때까지 쉬어요',
    /// 갈 곳이 없는데 켜 놓고 「안 되네」 하는 게 이 기능에서 제일 흔한 오해다.
    autoSwitchLone: '계정이 하나뿐이라 지금은 넘어갈 곳이 없어요',
    switchAt: '전환 시점',
    switchAtHint: '이 사용률을 넘으면 다음 계정으로 넘어가요',
    model: '모델',
    modelHint: 'Claude 모델 덮어쓰기 (기본 = 원래대로 유지)',
    effort: '추론 강도',
    effortHint: '기본은 그대로 둬요',
    optionDefault: '기본',
    extraArgs: '추가 인자',
    extraArgsHint: 'claude 실행에 항상 붙는 플래그 (예: --verbose)',
  },

  feedback: {
    body: '무엇이 불편했나요',
    bodyHint: '버그 · 이상한 동작 · 있었으면 하는 것 — 아무 형식이나 괜찮아요',
    placeholder: '어떤 화면에서 무엇을 하려다 무엇이 벌어졌는지 적어 주세요.',
    diag: '진단 정보 함께 남기기',
    /// 보내는 게 아니라 쌓는 거라 「저장」이다. 받는 곳이 생기기 전에 「보내기」라고
    /// 쓰면 안 간 걸 갔다고 말하는 셈이다.
    save: '저장',
    openFolder: '저장된 피드백 열기',
  },

  /// 서버가 코드로 알려 준 문구. 키는 Rust 가 보내는 `error_code`·`message_code`
  /// 그대로다. **여기 없는 코드는 서버가 함께 보낸 한국어 문구로 폴백**하므로,
  /// 이 표가 덜 찼다고 화면이 깨지지는 않는다.
  ///
  /// **한국어 쪽은 일부러 비어 있다.** 서버가 이미 한국어 문구를 함께 보내므로
  /// 여기 옮겨 적으면 같은 문장이 두 곳에 살고, 한쪽만 고쳐지는 날이 온다.
  /// 한국어의 정본은 Rust 고 이 표는 **다른 말로 갈아입힐 때만** 쓴다.
  server: {} as Record<
    string,
    string | ((a: Record<string, string | number>) => string)
  >,
};

export type Strings = typeof ko;

export const en: Strings = {
  nav: {
    settings: 'Settings',
    general: 'General',
    appearance: 'Appearance',
    shell: 'Shell',
    claude: 'Agent',
    theme: 'Characters',
    feedback: 'Feedback',
    native: 'Native',
    portHint: 'the kasaterm on this port',
  },

  titles: {
    general: { title: 'General', hint: 'Window, working folder, opening files' },
    appearance: { title: 'Appearance', hint: 'Colors, shape, fonts' },
    shell: { title: 'Shell', hint: 'Shell and editor' },
    claude: { title: 'Agent', hint: 'Accounts and how it runs' },
    theme: { title: 'Characters', hint: 'Student art, personas, and the roster' },
    feedback: { title: 'Feedback', hint: 'Tell us what tripped you up' },
  },

  common: {
    loading: 'Loading…',
    failed: "That didn't work",
    saving: 'Saving…',
    saved: 'Saved',
    notWebYet: 'This screen still lives in the native settings window.',
    fetchFailed: (a) => `${a.path} failed: ${a.error}`,
    palette: (a) => `palette ${a.theme} · accent ${a.accent} · corner ${a.radius}px`,
    valuesUnavailable: "This instance doesn't report its settings",
    stepDown: 'Step down',
    stepUp: 'Step up',
    colorOf: (a) => `${a.name} color`,
  },

  language: {
    title: 'Language',
    hint: 'Language of this settings window — changes right away',
    ko: '한국어',
    en: 'English',
  },

  theme: {
    section: 'Theme',
    sectionHint: 'One folder is one theme — names, colors, and art change together',
    newTheme: '+ New theme',
    rename: 'Rename',
    folder: 'Folder',
    remove: 'Remove',
    inUse: 'in use',
    members: (a) => `${a.count} characters`,
    persona: 'Persona',
    personaHint: 'Replies come in the character’s voice — from newly opened panes on',
    images: 'Art folder',
    imagesHint:
      'Click a character to change art per motion. Open the folder only when swapping many at once',
    openImages: 'Open art folder',
    openRoster: 'Open roster',
    refresh: 'Reload art',
    characters: 'Characters',
    charactersHint: (a) => `${a.count} in the roster — click one to edit persona and art`,
  },

  detail: {
    back: 'Roster',
    name: 'Name',
    nameHint:
      'This is the roster key — persona, color, and art all follow it. Saved when you leave the field.',
    persona: 'Persona',
    personaHint:
      'claude in panes opened as this character talks like this — panes already running keep their old one.',
    charCount: (a) => `${a.count} characters · saved when you stop typing`,
    renameRejected: "Couldn't rename — saved the persona only",
    saveFailed: "Couldn't save",
  },

  motion: {
    title: 'Art',
    hint: 'Each motion is set separately. A motion is used only when every frame is there, and changing one slot re-saves the rest exactly as they are.',
    statusFailed: "Couldn't read the art status",
    idle: { title: 'Idle', when: 'Standing around with nothing going on. Also the session banner.' },
    walk: { title: 'Working', when: 'Walking in place next to the spinner while claude works.' },
    wave: { title: 'Awaiting approval', when: 'Waving while an orange prompt waits for your answer.' },
    cheer: { title: 'Turn done', when: 'Both arms up when a turn finishes.' },
    profile: { title: 'Avatar', when: 'The face used in the sidebar and on messages.' },
    gif: { title: 'Idle animation', when: 'The moving face on the sidebar card.' },
    sourceUser: 'Your art',
    sourceBundled: 'Built-in art',
    sourceNone: 'No art',
    spec: (a) => `${a.ext} ×${a.count}`,
    pick: (a) => (a.count > 1 ? `Choose ${a.count} files` : 'Choose a file'),
    reset: 'Reset',
    changed: 'Changed',
    empty: 'none',
    slotTitle: (a) => `Frame ${a.index} — drop a file here or click to choose`,
    errExt: (a) => `Only ${a.ext} files go here`,
    errSizeMismatch: 'The frames are different sizes — make them match',
    errNeedAll: (a) => `Drop all ${a.count} frames at once`,
    errReadFrame: (a) => `Couldn't read frame ${a.index}`,
  },

  // 라벨은 네이티브 폼이 쓰던 영어를 그대로 물려받는다 — 그 화면을 보던 사람에게
  // 같은 말이어야 두 창구가 한 제품으로 읽힌다.
  general: {
    startupFolder: 'Startup folder',
    startupFolderHint: 'Where new windows and tabs open',
    cwdLast: 'Last folder',
    cwdHome: 'Home',
    cwdCustom: 'Custom',
    fileTree: 'File tree by default',
    fileTreeHint: 'Open the file-tree sidebar on start',
    statusBar: 'Pane status bar by default',
    statusBarHint: 'Show path · branch · diff under each pane',
    barHeightLow: 'Low',
    barHeightMid: 'Normal',
    barHeightHigh: 'Tall',
    windowBarH: 'Window status bar height',
    windowBarHHint: 'Thickness of the bottom usage row',
    paneBarH: 'Pane status bar height',
    paneBarHHint: 'Thickness of the path · branch row under each pane',
    fileOpen: 'File open',
    fileOpenHint: 'What opens a file picked in the file tree',
    openBuiltin: 'Built-in',
    openApp: 'App',
    openTerminal: 'Terminal',
    defaultApp: 'System default',
    terminalCmdHint: 'A CLI editor in a new pane — {} is where the path goes',
    autosave: 'Editor autosave',
    autosaveHint: (a) => `Save quietly once typing stops (${a.mod}+S still works)`,
    autosaveOff: 'Off',
    tabPosition: 'Tab position',
    tabPositionHint: 'Put window tabs on the title bar or the sidebar',
    tabTop: 'Top',
    tabSide: 'Side',
    cursorShape: 'Cursor shape',
    cursorShapeHint: 'A filled block, a Ghostty-style bar, or an underline',
    cursorBlock: 'Block',
    cursorBar: 'Bar',
    cursorUnderline: 'Underline',
    cursorThickness: 'Cursor thickness',
    cursorThicknessHint: 'How thick the bar or underline is',
    cursorThicknessIdle: 'Applies once you pick Bar or Underline',
    mousePointer: 'Mouse pointer',
    mousePointerHint: 'Pointer shape over the terminal (text fields keep the I-beam)',
    pointerArrow: 'Arrow',
    pointerIbeam: 'I-beam',
    scroll: 'Scroll sensitivity',
    scrollHint: 'Tuned for a trackpad. Raise it if a wheel feels sluggish',
    scrollTrackpad: 'Trackpad',
    scrollNormal: 'Normal',
    scrollMouse: 'Mouse',
    scrollFast: 'Fast',
  },

  appearance: {
    theme: 'Theme',
    themeHint:
      'The UI and the terminal ANSI palette change together — System follows the OS light/dark setting',
    systemSlots: 'When following the system',
    systemSlotsHint: 'Pick which theme to wear when the OS turns light or dark',
    systemLightSlot: 'When light',
    systemDarkSlot: 'When dark',
    palette: 'Palette',
    paletteHint: 'Click a swatch to open the color picker — or type #rrggbb',
    paletteLocked: 'Copy the current theme and edit it color by color — keep as many as you like',
    paletteStart: 'Copy the current theme to start',
    paletteNew: '+ New palette',
    paletteName: 'Name',
    paletteDelete: 'Remove this palette',
    paletteReset: 'Back to the base values',
    ansi: 'Terminal ANSI',
    ansiHint: 'The terminal’s 16 colors — 0..7 on top, 8..15 (bright) below',
    shape: 'Shape',
    shapeHint: 'Silhouette of corners, dots, and toggles (its own axis, apart from the palette)',
    accent: 'Accent color',
    accentHint: 'Selection, cursor, and link color',
    contrast: 'Minimum contrast',
    contrastHint: 'Lifts app-specified colors only when they sink into the background (dim is exempt)',
    contrastSample: 'Aa Bb',
    fontSize: 'Font size',
    fontSizeHint: 'Terminal cell font size — a base value, separate from UI zoom',
    uiZoom: 'UI zoom',
    uiZoomHint: 'Chrome, sidebar, and panes grow together',
    scaleReset: 'Back to 1:1',
  },

  shell: {
    defaultShell: 'Default shell',
    defaultShellHint: 'Shell for new panes (empty = the system $SHELL)',
    systemDefault: 'System default',
    custom: 'Custom',
  },

  claude: {
    shim: 'Shim injection',
    shimHint: 'Off means stock Claude — no persona, proxy, or hooks (needs a restart)',
    account: 'Account',
    accountHint: 'From the next claude on — sessions already running keep theirs',
    codexAccount: 'Codex account',
    codexAccountHint: 'From the next codex on — sessions already running keep theirs',
    addAccount: (a) => `+ Add ${a.provider} account`,
    rename: 'Rename',
    reauth: 'Sign in again',
    reauthIsolated: 'Blank window',
    browserHint:
      '“Sign in again” uses the browser you already have open — it attaches whichever account is signed in there. Use “Blank window” to attach a different account.',
    removeSlot: 'Remove',
    inUse: 'in use',
    labelPlaceholder: 'Nickname (empty = called by its email)',
    autoSwitch: 'Auto switch',
    autoSwitchHint:
      'When a limit fills, the next claude starts on the next account — the tired one rests until it clears',
    autoSwitchLone: 'Only one account, so there’s nowhere to switch to yet',
    switchAt: 'Switch at',
    switchAtHint: 'Move to the next account past this usage',
    model: 'Model',
    modelHint: 'Override the Claude model (Default = leave it alone)',
    effort: 'Effort',
    effortHint: 'Reasoning effort — Default leaves it alone',
    optionDefault: 'Default',
    extraArgs: 'Extra args',
    extraArgsHint: 'Flags always added to claude (e.g. --verbose)',
  },

  feedback: {
    body: 'What got in your way?',
    bodyHint: 'Bugs, odd behavior, things you wish existed — any shape is fine',
    placeholder: 'Which screen, what you were trying to do, and what happened instead.',
    diag: 'Include diagnostics',
    save: 'Save',
    openFolder: 'Open saved feedback',
  },

  // 서버가 코드로 보내는 문구의 영어판. 코드와 인자 이름은 Rust 쪽 표가 정본이다
  // (노아, 2026-08-15). 여기 없는 코드는 서버가 준 한국어가 그대로 뜬다.
  server: {
    // 거부 사유
    value_not_allowed: (a) => `“${a.value}” isn’t an allowed value`,
    account_missing: (a) => `There’s no account named “${a.account}”`,
    app_not_found: (a) => `Couldn’t find the app “${a.app}”`,
    theme_missing: (a) => `There’s no theme named “${a.theme}”`,
    action_unknown: (a) => `Unknown action: ${a.action}`,
    cwd_path_empty: 'The path can’t be empty',
    file_open_cmd_empty: 'The command can’t be empty',
    shell_path_empty: 'The shell path can’t be empty',
    // 팔레트가 하나도 없을 때와, 고른 팔레트가 이미 치워졌을 때 둘 다 이 코드다.
    custom_theme_absent: 'That custom palette isn’t there',
    label_empty: 'Give it a name',
    palette_slot_missing: 'That color slot doesn’t exist',
    hex_invalid: 'Write it as #rrggbb',
    step_out_of_range: 'One step at a time',
    step_at_limit: 'That’s as far as it goes',
    feedback_empty: 'Tell us what got in your way',

    // 네이티브가 토스트로 띄우려던 말
    restart_to_apply: 'Takes effect after a restart',
    scale_reset: 'Zoom 100% · default font',
    login_in_browser: 'Sign in from the blank browser window',
    login_in_default_browser:
      'Approve it in the browser you already use — it attaches whichever account is signed in there',
    terminal_editor_not_found:
      'Couldn’t find a terminal editor — type the command yourself',
    account_dir_failed: 'Couldn’t create the account folder',
    feedback_saved: 'Feedback saved',
    saved: 'Saved',

    // 계정 카드. 이름·이메일·조직명은 코드 없이 오므로 여기 없다 — 그건 데이터지
    // 옮길 말이 아니다.
    account_default: 'Default',
    account_numbered: (a) => `Account ${a.n}`,
    account_checking: 'Checking…',
    account_login_required: 'Sign-in needed',
  },
};

export const STRINGS: Record<Lang, Strings> = { ko, en };
