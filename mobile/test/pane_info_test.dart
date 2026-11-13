import 'dart:convert';

import 'package:flutter_test/flutter_test.dart';
import 'package:kasaterm_mobile/conversation.dart';
import 'package:kasaterm_mobile/my_prompts.dart';
import 'package:kasaterm_mobile/pane_info.dart';

List<(String, String?)> rows(List<FocusLine> lines) => [
      for (final l in lines)
        switch (l) {
          FocusKv(:final name, :final value) => (name, value),
          FocusLink(:final name, :final value) => (name, value),
          _ => null,
        },
    ].whereType<(String, String?)>().toList();

void main() {
  group('「지금 보는 칸」 — 데스크톱 info_focus.rs 시험과 같은 줄', () {
    test('claude 칸은 상태·경로·모델·문맥·승인·도구·최근·백그라운드·포트', () {
      final answer = PaneInfoAnswer.fromJson(jsonDecode(jsonEncode({
        'schema': 'kasa.pane-info.v1',
        'ok': true,
        'seq': '4.ab',
        'card': {
          'pane': '%14',
          'harness': 'claude',
          'shell': 'zsh',
          'program': '',
          'cwd': '/Users/someone/work/repo',
          'model': 'claude-opus-5-5',
          'effort': 'high',
          'facts': {
            'state': 'permission',
            'tools': [
              {'tool': 'Bash', 'label': 'npm test', 'agent': false, 'age_ms': 12000, 'took_ms': null, 'error': false},
            ],
            'recent': [
              {'tool': 'Edit', 'label': 'a.rs', 'agent': false, 'age_ms': 0, 'took_ms': 200, 'error': true},
            ],
            'background': [
              {'kind': 'shell', 'label': 'npm run dev', 'status': 'running', 'age_ms': 180000},
            ],
            'context': {'tokens': 124000, 'window': 200000, 'percent': 62.0},
            'ask': {'tool': 'Bash', 'preview': 'rm -rf build', 'age_ms': 3000},
          },
          'ports': [
            {'port': 5173, 'name': 'node', 'site': 'Vite App'},
          ],
        },
      })));
      expect(answer.seq, '4.ab');
      final lines = focusLines(answer.card);
      expect(lines.first, isA<FocusHead>().having((h) => h.title, 'title', 'Claude').having((h) => h.note, 'note', '%14'));
      final r = rows(lines);
      expect([for (final x in r) x.$1], ['상태', '경로', '모델', '문맥', '승인', '도구', '최근', '백그라운드', '포트']);
      expect(r[0].$2, '승인 기다림');
      expect(r[1].$2, '~/work/repo');
      expect(r[2].$2, 'claude-opus-5-5 · high');
      expect(r[3].$2, '62% · 124k / 200k');
      expect(r[4].$2, startsWith('Bash · rm -rf build · '));
      expect(r[5].$2, 'Bash · npm test · 12초');
      expect(r[6].$2, 'Edit · a.rs · 0.2초 · 실패');
      expect(r[7].$2, '셸 · npm run dev · 3분');
      expect(r[8].$2, '5173 · node · Vite App');
      final warns = [for (final l in lines) if (l is FocusKv && l.warn) l.name];
      expect(warns, ['상태', '승인', '최근']);
      expect(lines.whereType<FocusLink>().map((l) => (l.path, l.port)), [('/Users/someone/work/repo', null), (null, 5173)]);
      // 받은 뒤 흐른 시간은 나이에 더한다.
      expect(rows(focusLines(answer.card, elapsedMs: 50000))[5].$2, 'Bash · npm test · 1분');
    });

    test('셸은 위에 도는 프로그램을, mod 없는 claude 는 도구 칸에 까닭을 단다', () {
      String head(FocusCard c, {bool alt = false}) => (focusLines(c, altScreen: alt).first as FocusHead).title;
      expect(head(const FocusCard(pane: '%2', shell: 'zsh')), '셸 · zsh');
      expect(head(const FocusCard(pane: '%2', shell: 'zsh', program: 'cargo')), 'zsh · cargo 실행 중');
      expect(head(const FocusCard(pane: '%2', shell: 'zsh', program: 'nvim'), alt: true), '전체 화면 · nvim');
      expect(head(const FocusCard(pane: '%2', harness: 'codex')), 'Codex');
      final bare = focusLines(const FocusCard(pane: '%2', harness: 'claude'));
      expect(bare.whereType<FocusKv>().single, isA<FocusKv>().having((k) => k.value, 'value', isNull));
    });

    test('못 받은 답은 까닭으로 — 옛 데스크톱·신원 바뀜', () {
      expect(PaneInfoAnswer.fromJson({'schema': 'kasa.pane-info.v1', 'ok': false, 'error': 'source_changed'}).error, 'source_changed');
      expect(PaneInfoAnswer.fromJson({'ok': true}).error, 'update_needed');
      final lines = focusLines(null, issue: paneInfoIssue('update_needed'));
      expect(lines.last, isA<FocusText>().having((t) => t.warn, 'warn', isTrue).having((t) => t.text, 'text', contains('옛 판')));
    });
  });

  group('「내 프롬프트」 — 데스크톱 info_prompts.rs 와 같은 규칙', () {
    String line(Map<String, Object?> ev) => jsonEncode(ev);
    Map<String, Object?> user(String text, String at) =>
        {'type': 'user', 'timestamp': at, 'message': {'role': 'user', 'content': text}};

    test('친 말과 고른 답만, 새 것이 위 — 쪽지·슬래시 명령은 빼고 답은 고른 때', () {
      final c = Conversation();
      final raw = [
        user('첫 질문\n둘째 줄', '2026-11-13T01:00:00Z'),
        {
          'type': 'assistant',
          'timestamp': '2026-11-13T01:00:05Z',
          'message': {
            'content': [
              {
                'type': 'tool_use',
                'id': 'q1',
                'name': 'AskUserQuestion',
                'input': {
                  'questions': [
                    {'question': '어떤 색?', 'header': '색', 'options': []},
                  ],
                },
              },
            ],
          },
        },
        {
          'type': 'user',
          'timestamp': '2026-11-13T03:30:00Z',
          'message': {
            'content': [
              {'type': 'tool_result', 'tool_use_id': 'q1', 'content': 'User has answered your questions: "어떤 색?"="파랑". You can now continue.'},
            ],
          },
        },
        user('⟦아로나⟧ 쪽지 왔어요', '2026-11-13T04:00:00Z'),
        user('<command-name>/model</command-name>', '2026-11-13T04:10:00Z'),
        user('  ❯  둘째   질문 ', '2026-11-13T05:00:00Z'),
      ].map(line).join('\n');
      c.apply('$raw\n', reset: true, next: raw.length + 1);
      final prompts = myPrompts(groupRows(c.items));
      expect([for (final p in prompts) (p.text, p.asked)], [
        ('❯  둘째   질문', null),
        ('파랑', '어떤 색?'),
        ('첫 질문\n둘째 줄', null),
      ]);
      expect(prompts[1].at!.isAtSameMomentAs(DateTime.utc(2026, 11, 13, 3, 30)), isTrue, reason: '답은 물은 때가 아니라 고른 때');
      expect(prompts[1].row, isA<ChatAnswered>());
      // 체크 열쇠는 데스크톱 check_key 꼴 — 친 초|머리글(마커·공백 걷은 앞 40자)|질문 머리글.
      final at = DateTime.utc(2026, 11, 13, 5).millisecondsSinceEpoch ~/ 1000;
      expect(prompts[0].checkKey, '$at|둘째질문|');
      expect(prompts[1].checkKey.endsWith('|파랑|어떤색?'), isTrue);
    });

    test('머리글은 앞 40자, codex 의 질문 답도 답으로', () {
      expect(headKey('${'가' * 50}\n다음'), '가' * 40);
      expect(headKey('> 하나 둘'), '하나둘');
      const reply = '<send_user_message_question_reply>[{"question":"배포?","answer":"프리뷰"}]</send_user_message_question_reply>';
      final rows = [ChatBubble(user: true, text: reply)];
      expect([for (final p in myPrompts(rows)) (p.text, p.asked)], [('프리뷰', '배포?')]);
      expect(myPrompts([ChatBubble(user: true, text: '보내는 중', sending: true)]), isEmpty);
      expect(myPrompts([ChatBubble(user: true, text: '예약', queued: true)]), isEmpty);
    });

    test('친 시각은 월.일 시:분, 해가 다르면 앞에 해', () {
      final now = DateTime(2026, 11, 13, 12);
      expect(promptClock(DateTime(2026, 3, 5, 9, 7), now: now), '03.05 09:07');
      expect(promptClock(DateTime(2025, 10, 12, 22, 46), now: now), '2025.10.12 22:46');
    });
  });
}
