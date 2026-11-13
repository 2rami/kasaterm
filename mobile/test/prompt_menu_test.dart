import 'package:flutter_test/flutter_test.dart';
import 'package:kasaterm_mobile/conversation.dart';

import 'fixtures/claude_prompt_menus.dart';

/// 화면 줄에서 [text] 가 있는 줄 번호 — 탭 머리의 배경색 칸을 시험에서 짚는다.
Map<int, String> litAt(List<String> screen, String row, String on) =>
    {screen.indexWhere((l) => l.contains(row)): on};

void main() {
  group('실화면 고르기 창 — claude 2.1.323', () {
    test('도구 승인 — 넷째 선택지까지, 다시 묻지 않기는 화면 키로', () {
      final m = parsePromptMenu(claudeBashApproval)!;
      expect(m.title, 'Do you want to proceed?');
      expect(m.context.first, 'Bash command');
      expect(m.context, contains('touch hello.txt'));
      expect([for (final o in m.options) o.label], [
        'Yes',
        'Yes, and always allow access to /private/tmp/menu-test/work from this project',
        'Yes, and switch to auto mode · auto mode handles these prompts for you',
        'No',
      ]);
      expect(m.options.every((o) => !o.input && o.checked == null), isTrue);
      expect(menuStep(m, 1), (moves: 1, text: '', enter: true));

      final narrow = parsePromptMenu(claudeBashApprovalNarrow)!;
      expect(narrow.options[1].label,
          "Yes, and don't ask again for mkdir -p deploy-preview-folder and cp memo.md deploy-preview-folder/memo-copy.md commands in /private/tmp/menu-test/work");
      expect(narrow.options[2].label, 'Yes, and switch to auto mode · auto mode handles these prompts for you');
      expect(narrow.options[3].label, 'No');

      final write = parsePromptMenu(claudeWriteApproval)!;
      expect(write.context, ['Create file', 'memo.md', '1 hi']);
      expect(write.options.length, 3);
    });

    test('하나 고르기 — 설명 줄, 「Type something.」은 글 칸이라 빈 Enter 를 안 보낸다', () {
      final m = parsePromptMenu(claudeQuestionSingle)!;
      expect(m.title, '어떤 색을 쓸까요?');
      expect(m.context, ['☐ 색상']);
      expect([for (final o in m.options) o.label], ['빨강', '파랑', '초록', 'Type something.', 'Chat about this']);
      expect(m.options[0].note, '밝고 활기찬 빨간색');
      expect([for (final o in m.options) o.input], [false, false, false, true, false]);
      expect([for (final o in m.options) o.entry], [false, false, false, true, false]);
      expect(m.multi, isFalse);
      // 빈 Enter 는 질문 전체 거절이 된다 — 글이 없으면 키를 안 짠다.
      expect(menuStep(m, 3), isNull);
      expect(menuStep(m, 3, text: '  \n'), isNull);
      expect(menuStep(m, 3, text: '보라\n색'), (moves: 3, text: '보라 색', enter: true));
      expect(menuStep(m, 4), (moves: 4, text: '', enter: true));

      final narrow = parsePromptMenu(claudeQuestionNarrow)!;
      expect(narrow.cursor, 4);
      expect(narrow.options[4].input, isTrue);
      expect(menuStep(narrow, 0), (moves: -4, text: '', enter: true));
    });

    test('여러 개 고르기 — 체크 상자, 번호 없는 Submit 줄, 커서가 Submit 에 서도 메뉴', () {
      final m = parsePromptMenu(claudeQuestionMulti, lit: litAt(claudeQuestionMulti, '✔ Submit  →', ' ☐ 기능 '))!;
      expect(m.multi, isTrue);
      expect([for (final o in m.options) o.label], ['알림', '자동 저장', '다크 모드', 'Type something', 'Submit', 'Chat about this']);
      expect([for (final o in m.options) o.index], [1, 2, 3, 4, 0, 5]);
      expect([for (final o in m.options) o.row], [0, 1, 2, 3, 4, 5]);
      expect([for (final o in m.options) o.checked], [false, false, false, false, null, null]);
      expect(m.options[3].note, '', reason: 'Submit 줄이 설명으로 섞이면 안 된다');
      expect(m.options[3].input, isTrue);
      expect([for (final t in m.tabs) t.label], ['기능', 'Submit']);
      expect(m.currentTab, 0);
      // 고르기는 Enter 로 상자를 켠다. 글 칸은 치기만 해도 켜지고 Enter 는 도로 끈다.
      expect(menuStep(m, 1), (moves: 1, text: '', enter: true));
      expect(menuStep(m, 3, text: '귤'), (moves: 3, text: '귤', enter: false));
      expect(menuStep(m, 4), (moves: 4, text: '', enter: true));

      final onSubmit = parsePromptMenu(claudeQuestionMultiOnSubmit)!;
      expect(onSubmit.options[onSubmit.cursor].label, 'Submit');
      expect(onSubmit.options[0].checked, isTrue);
      expect(menuStep(onSubmit, 0), (moves: -4, text: '', enter: true));

      final typed = parsePromptMenu(claudeQuestionMultiOtherTyped)!;
      expect(typed.options[2].label, '귤');
      expect(typed.options[2].checked, isTrue);
      expect(typed.options[2].input, isFalse, reason: '이미 친 글은 고르기(Enter)로 다룬다');
      expect([for (final o in typed.options) o.entry], [false, false, true, false, false],
          reason: '글이 든 칸도 글 칸 자리 — 커서가 거기면 Tab 전에 한 칸 위로 뺀다');
    });

    test('검토 창 — 답한 질문과 답을 창 안 줄로 싣고, 지금 탭은 Submit', () {
      final m = parsePromptMenu(claudeQuestionMultiReview)!;
      expect(m.title, 'Ready to submit your answers?');
      expect(m.context, ['Review your answers', '● 어떤 기능을 켤까요?', '→ 알림']);
      expect([for (final o in m.options) o.label], ['Submit answers', 'Cancel']);
      expect([for (final t in m.tabs) (t.label, t.done, t.current)], [('기능', true, false), ('Submit', false, true)]);

      final two = parsePromptMenu(claudeQuestionTabsReview)!;
      expect(two.context, ['Review your answers', '● 어떤 언어로 쓸까요?', '→ 다트', '● 어떤 시험을 넣을까요?', '→ 단위']);
      expect(two.currentTab, 2);
    });

    test('질문 여러 개 — 머리 탭과 지금 탭(배경색 칸)', () {
      final first = parsePromptMenu(claudeQuestionTabsFirst,
          lit: litAt(claudeQuestionTabsFirst, '✔ Submit  →', ' ☐ 언어 '))!;
      expect([for (final t in first.tabs) (t.label, t.done, t.current)],
          [('언어', false, true), ('테스트', false, false), ('Submit', false, false)]);
      expect(first.context, isEmpty, reason: '탭 줄은 탭으로 세우고 창 안 줄로 겹치지 않는다');
      expect(first.multi, isFalse);
      expect(first.options[2].input, isTrue);

      final second = parsePromptMenu(claudeQuestionTabsSecond,
          lit: litAt(claudeQuestionTabsSecond, '✔ Submit  →', ' ☐ 테스트 '))!;
      expect([for (final t in second.tabs) (t.done, t.current)], [(true, false), (false, true), (false, false)]);
      expect(second.multi, isTrue);
      expect(second.options.map((o) => o.label), ['단위', '골든', 'Type something', 'Submit', 'Chat about this']);

      // 색 칸을 못 받으면 지금 탭은 모른다 — 지어내지 않는다.
      expect(parsePromptMenu(claudeQuestionTabsFirst)!.currentTab, -1);
    });

    test('화면보다 긴 질문 — ↑↓ 표식 줄도 선택지, 숨은 자리까지 센 차례로 옮긴다', () {
      final top = parsePromptMenu(claudeQuestionScrolledTop)!;
      expect([for (final o in top.options) o.index], [1, 2, 3, 4, 6]);
      expect([for (final o in top.options) o.row], [0, 1, 2, 3, 5]);
      expect(top.hiddenAbove, isNull);
      expect(top.hiddenBelow, 4);
      expect(menuReveal(top, up: false), 4);
      expect(menuReveal(top, up: true), isNull);
      // 「Chat about this」(6)는 숨은 5번을 건너 다섯 칸 아래다.
      expect(menuStep(top, 4), (moves: 5, text: '', enter: true));

      final bottom = parsePromptMenu(claudeQuestionScrolledBottom)!;
      expect([for (final o in bottom.options) o.index], [2, 3, 4, 5, 6]);
      expect(bottom.options[bottom.cursor].label, 'Type something.');
      expect(bottom.hiddenAbove, 0);
      expect(menuReveal(bottom, up: true), -4);
      expect(menuStep(bottom, 0), (moves: -3, text: '', enter: true));

      // 커서가 가장자리에 서면 그쪽 표식이 덮인다 — 번호 빈틈으로 숨은 칸을 안다.
      final edge = parsePromptMenu(claudeQuestionScrolledCursorEdge)!;
      expect([for (final o in edge.options) o.index], [2, 3, 4, 6]);
      expect((edge.hiddenAbove, edge.hiddenBelow), (0, 4));
      final bare = parsePromptMenu(claudeQuestionScrolledNoMarks)!;
      expect([for (final o in bare.options) o.index], [1, 2, 3, 6]);
      expect((bare.hiddenAbove, bare.hiddenBelow), (null, 3));
      expect(menuReveal(bare, up: false), 1);
      expect(menuStep(bare, 3), (moves: 3, text: '', enter: true));
    });

    test('계획 승인 — 칸막이 사이 계획을 싣고, 고칠 말은 글 칸', () {
      final m = parsePromptMenu(claudePlanApproval)!;
      expect(m.title, 'Claude has written up a plan and is ready to execute. Would you like to proceed?');
      expect(m.context.take(3), ['Ready to code?', "Here is Claude's plan:", '계획: notes.md 생성']);
      expect(m.context.last, '3. 파일이 생성되었고 내용이 hi인지 확인한다.');
      expect([for (final o in m.options) o.label],
          ['Yes, and use auto mode', 'Yes, manually approve edits', 'Tell Claude what to change']);
      expect(m.options[2].input, isTrue);
      expect(m.options[2].note, 'shift+tab to approve with this feedback');
      expect([for (final o in m.options) o.entry], [false, false, true]);
      expect(menuStep(m, 2, text: '파일 이름을 memo.md 로'), (moves: 2, text: '파일 이름을 memo.md 로', enter: true));

      final short = parsePromptMenu(claudePlanApprovalShort)!;
      expect(short.context, ['Ready to code?', "Here is Claude's plan:", '계획: memo.md 생성', 'Context'],
          reason: '상자 안 굴림 표식(↓)은 싣지 않는다');
      final low = parsePromptMenu(claudePlanApprovalLow)!;
      expect(low.context, ['Ready to code?'], reason: '줄 끝에 붙은 굴림 표식도 걷는다');
      expect(low.options[2].input, isTrue);
    });

    test('/model — 두 줄로 접힌 안내문은 제목 하나, 이름 옆 칸은 설명', () {
      final m = parsePromptMenu(claudeModelMenu)!;
      expect(m.context, ['Select model']);
      expect(m.title,
          'Switch between Claude models. Your pick becomes the default for new sessions. For other/previous model names, specify with --model.');
      expect([for (final o in m.options) o.label], ['Default (recommended)', 'Opus', 'Fable', 'Sonnet', 'Haiku ✔']);
      expect(m.options[3].note, 'Sonnet 5.5 · Efficient for routine tasks');
      expect(m.cursor, 4);
      expect(menuStep(m, 1), (moves: -3, text: '', enter: true));

      final scrolled = parsePromptMenu(claudeModelMenuScrolled)!;
      expect([for (final o in scrolled.options) o.label], ['Opus', 'Fable']);
      expect(scrolled.options[1].note, 'Fable 5.1 · Most capable for your hardest and longest-running tasks',
          reason: '「… +2 models」 줄은 설명이 아니다');
      expect((scrolled.hiddenAbove, scrolled.hiddenBelow), (0, 3));
    });
  });
}
