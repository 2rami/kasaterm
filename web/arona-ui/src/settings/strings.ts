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
    claude: 'Claude',
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
    claude: { title: 'Claude', hint: '계정과 실행 방식' },
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
    claude: 'Claude',
    theme: 'Characters',
    feedback: 'Feedback',
    native: 'Native',
    portHint: 'the kasaterm on this port',
  },

  titles: {
    general: { title: 'General', hint: 'Window, working folder, opening files' },
    appearance: { title: 'Appearance', hint: 'Colors, shape, fonts' },
    shell: { title: 'Shell', hint: 'Shell and editor' },
    claude: { title: 'Claude', hint: 'Accounts and how it runs' },
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
    custom_theme_absent: 'You haven’t made a custom palette yet',
    palette_slot_missing: 'That color slot doesn’t exist',
    hex_invalid: 'Write it as #rrggbb',
    step_out_of_range: 'One step at a time',
    step_at_limit: 'That’s as far as it goes',
    feedback_empty: 'Tell us what got in your way',

    // 네이티브가 토스트로 띄우려던 말
    restart_to_apply: 'Takes effect after a restart',
    scale_reset: 'Zoom 100% · default font',
    login_in_browser: 'Sign in from the blank browser window',
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
