import 'dart:ui';

import 'package:flutter_test/flutter_test.dart';
import 'package:kasaterm_mobile/claude_style.dart';
import 'package:kasaterm_mobile/grid.dart';
import 'package:kasaterm_mobile/reflow.dart';

const accent = Color(0xff4c6ef5);
const bg = Color(0xff252c35);
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
