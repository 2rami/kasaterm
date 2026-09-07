import 'dart:ui';

import 'package:flutter_test/flutter_test.dart';
import 'package:kasaterm_mobile/claude_style.dart';
import 'package:kasaterm_mobile/grid.dart';
import 'package:kasaterm_mobile/reflow.dart';

const accent = Color(0xff4c6ef5);
const bg = Color(0xff252c35);
const _accentRgb = RgbColor(0x4c, 0x6e, 0xf5);
const st = StudentStyle(slug: 'arisu', accent: accent, bg: bg);

Grid gridOf(List<String> rows, {int cols = 60, List<int>? cursor}) =>
    Grid()..apply({
      'cols': cols,
      'rows': rows.length,
      'dirty': [
        for (var r = 0; r < rows.length; r++)
          [
            r,
            if (rows[r].isEmpty)
              []
            else
              [
                [rows[r], null, null, 0],
              ],
          ],
      ],
      'cursor': cursor ?? [rows.length - 1, 0],
      'cursorVisible': true,
    });

String text(List<Run> runs) => runs.map((r) => r.text).join();

/// 칸 수 — 한글은 두 칸이라 글자 수와 다르다.
int cols(List<Run> runs) =>
    text(runs).runes.fold(0, (n, r) => n + cellWidth(r));

Matcher rgb(RgbColor c) => predicate<CellColor>(
  (v) => v is RgbColor && v.r == c.r && v.g == c.g && v.b == c.b,
  'rgb(${c.r},${c.g},${c.b})',
);

void main() {
  bannerTests();
  pinnedTests();
  historyTests();

  test('스피너 자리: 글리프를 지우고 걷는 도트 2칸×2줄, 문구는 학생색', () {
    final g = gridOf([
      '⏺ Bash(ls)',
      '  ⎿ done',
      '',
      '✻ Cerebrating… (3s · ↓ 1.2k tokens)',
      '',
      '──────────────────────────────',
      '❯ ',
      '──────────────────────────────',
      'Fable 5.1 ￼￼ main',
    ]);
    final v = restyleClaude(g, st, 0.5);
    expect(v.animated, isTrue);
    expect(v.slots.single.motion, 'walk');
    expect(v.slots.single.row, 2);
    expect(v.slots.single.col, 0);
    expect(v.slots.single.rows, 2);
    expect(v.slots.single.cols, 2);
    expect(text(v.lines[3]).startsWith('  Cerebrating…'), isTrue);
    // 동사 문구는 학생색(glow 섞임)이고 꼬리는 바탕에 눕힌 학생색.
    // glow 는 칸마다 색이 달라 동사 문구가 한 글자씩 갈린다 — 전부 학생색 계열이다.
    final verbRuns = v.lines[3].where(
      (r) => r.text.trim().isNotEmpty && !r.text.contains('('),
    );
    expect(verbRuns, isNotEmpty);
    for (final r in verbRuns) {
      expect(r.fg, isA<RgbColor>());
    }
    final tail = v.lines[3].last;
    expect(tail.fg, rgb(tintToward(bg, accent, 0.6)));
    // 프사 자리는 비운다.
    expect(text(v.lines[8]).contains('￼'), isFalse);
    // 작업 중엔 서 있는 도트가 없다.
    expect(v.slots.length, 1);
  });

  test('아래에 응답 마커가 있으면 옛 문구라 스피너가 아니다', () {
    final g = gridOf([
      '✻ Cerebrating… (3s · ↓ 1.2k tokens)',
      '⏺ 답이다',
      '',
      '──────────────────────────────',
      '❯ ',
      '──────────────────────────────',
      'Fable ￼',
    ]);
    final v = restyleClaude(g, st, 0);
    expect(v.slots.where((s) => s.motion == 'walk'), isEmpty);
  });

  test('쉴 때는 입력상자 위에 서고, 테두리·❯ 는 학생색', () {
    final g = gridOf([
      '⏺ 끝났다.',
      '',
      '',
      '',
      '──────────────────────────────',
      '❯ ',
      '──────────────────────────────',
      'Fable 5.1 ￼￼ main',
    ]);
    final v = restyleClaude(g, st, 0);
    final stand = v.slots.single;
    expect(stand.motion, 'idle');
    expect(stand.rows, 3);
    expect(stand.cols, 4);
    // 윗 테두리(4행) 위 앵커는 3행, 도트 바닥이 앵커 아래에 닿는다.
    expect(stand.row + stand.rows, 4);
    // 앵커 행이 비어 있으면 오른쪽 끝에서 왼쪽으로 4칸.
    expect(stand.col, 60 - 1 - 4);
    final fg = RgbColor(0x4c, 0x6e, 0xf5);
    for (final r in [4, 6]) {
      for (final run in v.lines[r]) {
        if (run.text.trim().isNotEmpty) {
          expect(run.fg, isA<RgbColor>());
          expect((run.fg as RgbColor).r, fg.r);
        }
      }
    }
    final marker = v.lines[5].first;
    expect(marker.text.startsWith('❯'), isTrue);
    expect((marker.fg as RgbColor).b, fg.b);
  });

  test('codex 바닥줄: claude statusline 의 말로 — 로고 자리·GPT-5.6 Sol 1M·%·effort', () {
    final g = gridOf([
      '  gpt-5.6-sol xhigh · main · kasaterm · never · Context 16% used'
          '${' ' * 20}Pursuing goal (2m)',
    ], cols: 120);
    const codex = StudentStyle(
      slug: 'yuuka',
      accent: accent,
      bg: bg,
      codex: true,
      branch: 'main',
      project: 'kasaterm',
    );
    final v = restyleClaude(g, codex, 0);
    final line = text(v.lines[0]);
    expect(line, contains('GPT-5.6 Sol 1M'));
    expect(line, contains('main'));
    expect(line, contains('kasaterm'));
    expect(line, contains('16%'));
    expect(line.trimRight(), endsWith('xhigh'));
    expect(line, isNot(contains('Pursuing')));
    expect(line, isNot(contains('never')));
    expect(v.slots.map((s) => s.motion), contains('icon:codex'));
    // 색: 모델은 파랑 굵게, effort xhigh 는 붉게.
    final model = v.lines[0].firstWhere((r) => r.text.contains('GPT'));
    expect(model.fg, rgb(const RgbColor(0x7a, 0xa2, 0xf7)));
    expect(model.flags & flagBold, flagBold);
    final eff = v.lines[0].lastWhere((r) => r.text.contains('xhigh'));
    expect(eff.fg, rgb(const RgbColor(0xf7, 0x76, 0x8e)));
  });

  test('codex 바닥줄이 아닌 줄(claude statusline)은 손대지 않는다', () {
    final g = gridOf([
      '  Fable 5.1 1M │ main │ kasaterm │ 42% │ xhigh',
    ], cols: 60);
    const codex = StudentStyle(
      slug: 'yuuka',
      accent: accent,
      bg: bg,
      codex: true,
    );
    final v = restyleClaude(g, codex, 0);
    expect(
      text(v.lines[0]).trim(),
      'Fable 5.1 1M │ main │ kasaterm │ 42% │ xhigh',
    );
  });

  test('codex 세션 배지: 입력창 첫 줄 오른쪽 끝 한 칸 앞에 학생색으로', () {
    final g = Grid()
      ..apply({
        'cols': 40,
        'rows': 3,
        'dirty': [
          [
            0,
            [
              ['› Ask Codex to do anything', null, 236, 0],
              [' ' * 14, null, 236, 0],
            ],
          ],
          [
            1,
            [
              [' ' * 40, null, 236, 0],
            ],
          ],
          [2, []],
        ],
        'cursor': [0, 2],
      });
    const codex = StudentStyle(
      slug: 'yuuka',
      accent: accent,
      bg: bg,
      codex: true,
      session: '개명함',
    );
    final v = restyleClaude(g, codex, 0);
    final line = text(v.lines[0]);
    expect(line, startsWith('› Ask Codex to do anything'));
    expect(line.trimRight(), endsWith('개명함'));
    expect(cols(v.lines[0]), 40, reason: '한글 두 칸까지 세서 오른쪽 한 칸이 남는다');
    final badge = v.lines[0].lastWhere((r) => r.text.contains('개명함'));
    expect(badge.fg, rgb(_accentRgb));
  });

  test('codex 세션 배지·상태줄: 폰이 pane 보다 좁으면 폰 폭 기준', () {
    final g = Grid()
      ..apply({
        'cols': 120,
        'rows': 3,
        'dirty': [
          [
            0,
            [
              // 서버는 뒤 빈칸을 잘라 보낸다 — 띠의 빈 첫 줄은 짧게 온다.
              ['› Ask Codex to do anything', null, 236, 0],
            ],
          ],
          [1, []],
          [
            2,
            [
              [
                '  gpt-5.6-sol xhigh · main · kasaterm · Context 16% used',
                null,
                null,
                0,
              ],
            ],
          ],
        ],
        'cursor': [0, 2],
      });
    const codex = StudentStyle(
      slug: 'yuuka',
      accent: accent,
      bg: bg,
      codex: true,
      session: 'codex',
      branch: 'main',
      project: 'kasaterm',
    );
    final v = restyleClaude(g, codex, 0, wrapCols: 42);
    final band = text(v.lines[0]);
    expect(band.substring(0, 42).trimRight(), endsWith('codex'));
    expect(cols(v.lines[0]), 42, reason: '폰 폭까지만 띠를 채우고 그 뒤는 없다');
    final status = text(v.lines[2]).trimRight();
    expect(cols(v.lines[2]), lessThanOrEqualTo(42));
    expect(status, contains('GPT-5.6 Sol 1M'));
    expect(status, contains('16%'));
    expect(status, isNot(contains('kasaterm')), reason: '좁으면 폴더부터 뺀다');
  });

  test('codex 세션 배지: 입력이 그 자리까지 찼으면 안 그린다', () {
    final g = Grid()
      ..apply({
        'cols': 20,
        'rows': 2,
        'dirty': [
          [
            0,
            [
              ['› typing all the way', null, 236, 0],
            ],
          ],
          [1, []],
        ],
        'cursor': [0, 2],
      });
    const codex = StudentStyle(
      slug: 'yuuka',
      accent: accent,
      bg: bg,
      codex: true,
      session: 'x',
    );
    final v = restyleClaude(g, codex, 0);
    expect(text(v.lines[0]).trimRight(), '› typing all the way');
  });

  test('사용자 프롬프트 띠: 본문 폭까지만 학생색 바탕, ❯ 는 학생색', () {
    final g = Grid()
      ..apply({
        'cols': 20,
        'rows': 2,
        'dirty': [
          [
            0,
            [
              ['❯ hi', null, 236, 0],
              [' ' * 16, null, 236, 0],
            ],
          ],
          [1, []],
        ],
        'cursor': [1, 0],
      });
    final v = restyleClaude(g, st, 0);
    final row = v.lines[0];
    final fill = tintToward(bg, accent, 0.18);
    expect(row.first.text.startsWith('❯'), isTrue);
    expect(row.first.fg, isA<RgbColor>());
    expect(row.first.bg, rgb(fill));
    // 꼬리(마지막 글자 + 2칸 뒤)는 기본 배경.
    expect(row.last.bg, isA<DefaultColor>());
    // 마지막 글자(i, 3열) + 2칸까지 띠, 나머지 15칸은 기본 배경.
    expect(row.last.text.length, 15);
  });

  test('접을 때 도트 자리는 지난 줄 수만큼 내려간다', () {
    final g = gridOf([
      '⏺ 끝났다.',
      '',
      '',
      '',
      '──────────────────────────────',
      '❯ ',
      '──────────────────────────────',
      'Fable ￼',
    ]);
    final live = restyleClaude(g, st, 0);
    final view = Reflow().apply(
      CombinedGrid([
        [const Run('old', DefaultColor(), DefaultColor(), 0)],
      ], live),
      60,
    );
    expect(view.slots.single.row + view.slots.single.rows, 5);
  });
}

void bannerTests() {
  const named = StudentStyle(
    slug: 'arisu',
    name: '아리스',
    accent: accent,
    bg: bg,
  );

  test('시작 배너: Clawd 그림 자리에 도트, 제목은 학생 이름, 환영문은 학생 말투', () {
    final g = gridOf([
      '╭──────────────────────────────────────╮',
      '│ Welcome back kasa!                    │',
      '│ ▐▛███▛█   Claude Code v2.1.0         │',
      '│▝▜██████▀                             │',
      '│ ▝▝ ▝▝                                │',
      '╰──────────────────────────────────────╯',
      '',
      '──────────────────────────────',
      '❯ ',
      '──────────────────────────────',
      'Fable ￼',
    ]);
    final v = restyleClaude(g, named, 0);
    final banner = v.slots.firstWhere((s) => s.cols == 9);
    expect(banner.motion, 'idle');
    expect(banner.row, 2);
    expect(banner.col, 1);
    expect(banner.cols, 9);
    // 그림 칸은 비고 제목·환영문이 바뀐다.
    expect(text(v.lines[2]).contains('▛'), isFalse);
    expect(text(v.lines[3]).contains('█'), isFalse);
    expect(text(v.lines[2]), contains('아리스'));
    expect(text(v.lines[2]), contains('v2.1.0'));
    expect(text(v.lines[1]), contains('kasa 선생님, 돌아왔구나!'));
    // 상자 선은 학생색.
    final corner = v.lines[0].first;
    expect(corner.fg, isA<RgbColor>());
    // 칸 폭이 보존된다 — 뒤 글자가 밀리지 않는다.
    for (final r in [1, 2]) {
      expect(cols(v.lines[r]), cols(g.lines[r]));
    }
  });

  test('머리가 화면 위로 밀린 배너는 행 -1 에서 시작해 위로 삐져나간다', () {
    final g = gridOf([
      '▝▜██████▀  Claude Code v2',
      ' ▝▝ ▝▝',
      '',
      '──────────────────────────────',
      '❯ ',
      '──────────────────────────────',
      'Fable ￼',
    ]);
    final v = restyleClaude(g, named, 0);
    final banner = v.slots.firstWhere((s) => s.cols == 9);
    expect(banner.row, -1);
    final view = Reflow().apply(CombinedGrid(const [], v), 60);
    expect(view.slots.firstWhere((s) => s.cols == 9).row, -1);
  });

  test('상태줄 모델 표식은 로고 자리가 되고 글자는 지운다', () {
    final g = gridOf([
      '',
      '──────────────────────────────',
      '❯ ',
      '──────────────────────────────',
      '\u{e0c0} Fable 5.1 ￼ main',
    ]);
    final v = restyleClaude(g, named, 0);
    final icon = v.slots.firstWhere((s) => s.motion.startsWith('icon:'));
    expect(icon.motion, 'icon:claude');
    expect(icon.row, 4);
    expect(icon.col, 0);
    expect(icon.cols, 2);
    expect(text(v.lines[4]).contains('\u{e0c0}'), isFalse);
  });
}

void pinnedTests() {
  test('붙잡을 첫 행은 입력상자의 위 테두리', () {
    final g = gridOf([
      '⏺ 답',
      '',
      '──────────────────────────────',
      '❯ 입력 중',
      '──────────────────────────────',
      '  ⏵⏵ bypass permissions on',
      'Fable ￼',
    ]);
    expect(pinnedInputTop(g.lines), 2);
    expect(pinnedInputTop(gridOf(['그냥 글', '']).lines), isNull);
  });
}

void historyTests() {
  test('지난 줄에도 프롬프트 띠·프사 자리표 지우기·배너 치환이 입혀진다', () {
    const named = StudentStyle(
      slug: 'arisu',
      name: '아리스',
      accent: accent,
      bg: bg,
    );
    final hist = <List<Run>>[
      [const Run('Fable ￼ main', DefaultColor(), DefaultColor(), 0)],
      [
        const Run('❯ 안녕', DefaultColor(), IndexColor(236), 0),
        const Run('               ', DefaultColor(), IndexColor(236), 0),
      ],
      [
        const Run(
          ' ▐▛███▛█   Claude Code v2',
          DefaultColor(),
          DefaultColor(),
          0,
        ),
      ],
      [const Run('▝▜██████▀', DefaultColor(), DefaultColor(), 0)],
      [const Run(' ▝▝ ▝▝', DefaultColor(), DefaultColor(), 0)],
    ];
    final (lines, slots) = restyleHistory(hist, named);
    expect(text(lines[0]).contains('￼'), isFalse);
    expect(lines[1].first.fg, isA<RgbColor>());
    expect(lines[1].last.bg, isA<DefaultColor>());
    expect(slots.single.motion, 'idle');
    expect(slots.single.row, 2);
    expect(text(lines[2]), contains('아리스'));
    expect(text(lines[3]).contains('█'), isFalse);
    // 지난 줄 슬롯은 합칠 때 그대로, 살아 있는 화면 슬롯은 지난 줄 수만큼 내려간다.
    final live = restyleClaude(
      gridOf([
        '',
        '──────────────────────────────',
        '❯ ',
        '──────────────────────────────',
        'Fable ￼',
      ]),
      named,
      0,
    );
    final c = CombinedGrid(lines, live, historySlots: slots);
    expect(c.slots.first.row, 2);
    expect(c.slots.last.row, greaterThanOrEqualTo(5));
  });
}
