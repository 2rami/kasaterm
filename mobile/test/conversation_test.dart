import 'dart:convert';

import 'package:flutter_test/flutter_test.dart';
import 'package:kasaterm_mobile/conversation.dart';


String lines(List<Map<String, Object?>> events) =>
    '${events.map(jsonEncode).join('\n')}\n';

Map<String, Object?> user(Object content, {bool meta = false}) => {
  'type': 'user',
  'sessionId': 's1',
  'timestamp': '2026-09-25T01:00:00Z',
  'isMeta': ?(meta ? true : null),
  'message': {'role': 'user', 'content': content},
};

Map<String, Object?> assistant(List<Object> blocks) => {
  'type': 'assistant',
  'sessionId': 's1',
  'timestamp': '2026-09-25T01:00:05Z',
  'message': {'role': 'assistant', 'content': blocks},
};

void main() {
  test('claude 대화: 말풍선·도구 짝·주입 숨김', () {
    final c = Conversation();
    c.apply(
      lines([
        user('<system-reminder>숨겨라</system-reminder>빌드 돌려줘'),
        user('Base directory for this skill: …', meta: true),
        assistant([
          {'type': 'thinking', 'thinking': '먼저 확인'},
          {'type': 'text', 'text': '돌릴게요'},
          {
            'type': 'tool_use',
            'id': 't1',
            'name': 'Bash',
            'input': {'command': 'cargo build', 'description': '빌드'},
          },
        ]),
        user([
          {'type': 'tool_result', 'tool_use_id': 't1', 'content': 'ok'},
        ]),
        user('This session is being continued from a previous conversation…'),
        user('[Request interrupted by user]'),
      ]),
      reset: true,
      next: 100,
    );
    final kinds = c.items.map((e) => e.runtimeType).toList();
    expect(kinds, [
      ChatBubble,
      ChatThinking,
      ChatBubble,
      ChatTool,
      ChatInterrupted,
    ]);
    final first = c.items.first as ChatBubble;
    expect(first.mine, isTrue);
    expect(first.text, '빌드 돌려줘');
    final tool = c.items[3] as ChatTool;
    expect(tool.summary, '빌드');
    expect(tool.result, 'ok');
    expect(c.offset, 100);
  });

  test('이어 받은 조각이 앞의 도구를 채우고, 세션이 바뀌면 거절한다', () {
    final c = Conversation();
    c.apply(
      lines([
        assistant([
          {
            'type': 'tool_use',
            'id': 't1',
            'name': 'Read',
            'input': {'file_path': '/a/b/hub.dart'},
          },
        ]),
      ]),
      reset: true,
      next: 10,
    );
    final v = c.version;
    expect(
      c.apply(
        lines([
          user([
            {
              'type': 'tool_result',
              'tool_use_id': 't1',
              'content': [
                {'type': 'text', 'text': 'body'},
              ],
              'is_error': true,
            },
          ]),
        ]),
        reset: false,
        next: 20,
      ),
      isTrue,
    );
    final tool = c.items.single as ChatTool;
    expect(tool.summary, 'hub.dart');
    expect(tool.error, isTrue);
    expect(c.version, greaterThan(v));

    final other = {...user('새 세션'), 'sessionId': 's2'};
    expect(c.apply(lines([other]), reset: false, next: 30), isFalse);
  });

  test('슬래시 명령·출력·예약·쪽지·질문 답', () {
    final c = Conversation();
    c.apply(
      lines([
        user(
          '<command-name>/model</command-name><command-args>opus</command-args>',
        ),
        user(
          '<local-command-stdout>Set model to \x1b[1mOpus\x1b[0m</local-command-stdout>',
        ),
        {
          'type': 'queue-operation',
          'operation': 'enqueue',
          'content': '끝나면 커밋',
          'timestamp': '2026-09-25T01:01:00Z',
        },
        user('<teammate-message teammate_id="유즈">다 했어요</teammate-message>'),
        assistant([
          {
            'type': 'tool_use',
            'id': 'q1',
            'name': 'AskUserQuestion',
            'input': {
              'questions': [
                {'question': '어디에 둘까?'},
              ],
            },
          },
        ]),
      ]),
      reset: true,
      next: 1,
    );
    expect((c.items[0] as ChatCommand).name, '/model');
    expect((c.items[1] as ChatOutput).text, 'Set model to Opus');
    final queued = c.items[2] as ChatBubble;
    expect(queued.queued, isTrue);
    expect((c.items[3] as ChatBubble).from, '유즈');
    expect((c.items[3] as ChatBubble).via, '쪽지');
    expect(groupRows(c.items).whereType<ChatAnswered>(), isEmpty);

    c.apply(
      lines([
        {'type': 'queue-operation', 'operation': 'dequeue'},
        user([
          {
            'type': 'tool_result',
            'tool_use_id': 'q1',
            'content': 'User has answered your questions: "어디에 둘까?"="위". ',
          },
        ]),
      ]),
      reset: false,
      next: 2,
    );
    expect(c.items.contains(queued), isFalse);
    final answered = groupRows(c.items).whereType<ChatAnswered>().single;
    expect(answered.pairs, [('어디에 둘까?', '위')]);
  });

  test('이어진 도구는 한 묶음', () {
    final a = ChatTool('Read', 'a');
    final b = ChatTool('Grep', 'b');
    final rows = groupRows([ChatBubble(user: false, text: 'x'), a, b]);
    expect(rows.length, 2);
    expect((rows[1] as ToolRun).tools, [a, b]);
  });

  test('codex rollout: 말·도구·결과', () {
    final c = Conversation();
    Map<String, Object?> ev(String type, Map<String, Object?> payload) => {
      'timestamp': '2026-09-25T01:00:00Z',
      'type': type,
      'payload': payload,
    };
    c.apply(
      lines([
        ev('event_msg', {'type': 'user_message', 'message': '테스트 돌려'}),
        ev('event_msg', {'type': 'user_message', 'message': '테스트 돌려'}),
        ev('response_item', {
          'type': 'function_call',
          'name': 'shell',
          'call_id': 'c1',
          'arguments': jsonEncode({
            'command': ['bash', '-lc', 'flutter test'],
          }),
        }),
        ev('response_item', {
          'type': 'function_call_output',
          'call_id': 'c1',
          'output': jsonEncode({
            'output': 'failed',
            'metadata': {'exit_code': 1},
          }),
        }),
        ev('event_msg', {'type': 'agent_message', 'message': '하나 실패'}),
      ]),
      reset: true,
      next: 1,
    );
    expect(c.items.length, 3);
    final tool = c.items[1] as ChatTool;
    expect(tool.summary, 'flutter test');
    expect(tool.result, 'failed');
    expect(tool.error, isTrue);
    expect((c.items[2] as ChatBubble).mine, isFalse);
  });

  test('mod 가 넣은 말은 사람이 쓴 말만 보인다', () {
    const raw = "The kasaterm-bridge plugin sent a message:\nUse the Bash tool to run exactly: touch y1.txt\n\nThis is how Claude Code surfaces a prompt a plugin submits between turns — it starts this turn in the user's place. Address the message above.";
    expect(stripMeta(raw), 'Use the Bash tool to run exactly: touch y1.txt');
    expect(stripMeta('The plan plugin sent a message: is a sentence'), 'is a sentence');
    expect(stripMeta('The big plan plugin sent a message: x'), 'The big plan plugin sent a message: x');
  });

  test('tell·완료 보고·나쵸가 맡긴 일은 보낸 쪽 이름을 단다', () {
    String? who(String raw) {
      final m = relayedMessage(stripMeta(raw));
      return m == null ? null : '${m.name}|${m.via}|${m.body}';
    }

    expect(who('⟦아즈사⟧ 폰 판 구워서 올려'), '아즈사|쪽지|폰 판 구워서 올려');
    expect(
      who('\n\n<pasted_content id="95ad">\n⟦유우카⟧ 나쵸 쪽 답\n둘째 줄\n</pasted_content id="95ad">\n'),
      '유우카|쪽지|나쵸 쪽 답\n둘째 줄',
    );
    expect(who('[완료] 코유키(%0) — 카사넷 끝'), '코유키|완료 보고|카사넷 끝');
    expect(who('[origin=nacho task=w1] 보고는 이렇게 한다\n거노 지시 그대로: 고쳐 줘'), '나쵸|맡긴 일|거노 지시 그대로: 고쳐 줘');
    expect(who('[완료] 표시만 친 사람 말'), isNull);
    expect(who('사람 말'), isNull);
  });

  test('화면의 선택 메뉴 — ❯ 커서가 있어야 메뉴', () {
    final permission = parsePromptMenu([
      ' Bash command',
      '   cargo test',
      ' Do you want to proceed?',
      ' ❯ 1. Yes',
      '   2. Yes, and don\'t ask again for cargo test commands',
      '   3. No, and tell Claude what to do differently (esc)',
    ]);
    expect(permission, isNotNull);
    expect(permission!.title, 'Do you want to proceed?');
    expect(permission.options.map((o) => o.index), [1, 2, 3]);
    expect(permission.cursor, 0);

    final question = parsePromptMenu([
      '☐ 위치',
      '어디에 둘까?',
      '',
      '  1. 위',
      '     앱바 밑',
      '❯ 2. 아래',
      '     입력줄 위',
      '  3. Type something.',
    ]);
    expect(question?.cursor, 1);
    expect(question?.options.length, 3);

    expect(parsePromptMenu(['할 일:', '1. 빌드', '2. 테스트', '3. 커밋']), isNull);
  });

  // Claude Code 2.1.291·codex 0.160.1 의 실제 창(100칸) — 데스크톱 parse.rs 의 같은 이름 시험과 같은 줄.
  test('실제 승인 창 — 도구·입력 원문과 선택지 문구를 그대로', () {
    final rule = '─' * 100;
    final dash = '╌' * 100;
    final bash = parsePromptMenu([
      '⏺ Removing nothing-here.txt file with force flag',
      rule,
      ' Bash command',
      '',
      '   rm -f nothing-here.txt',
      '   Remove nothing-here.txt file with force flag',
      '',
      dash,
      ' Do you want to proceed?',
      ' ❯ 1. Yes',
      '   2. Yes, and always allow access to /private/tmp/yuzu/tui/work from this project',
      '   3. No',
      '',
      ' Esc to cancel · Tab to amend',
    ])!;
    expect(bash.title, 'Do you want to proceed?');
    expect(bash.context, ['Bash command', 'rm -f nothing-here.txt', 'Remove nothing-here.txt file with force flag']);
    expect(bash.options.map((o) => o.label), [
      'Yes',
      'Yes, and always allow access to /private/tmp/yuzu/tui/work from this project',
      'No',
    ]);
    expect(bash.options.every((o) => o.note.isEmpty), isTrue);

    final write = parsePromptMenu([
      rule,
      ' Create file',
      ' note.txt',
      dash,
      '  1 hi',
      dash,
      ' Do you want to create note.txt?',
      ' ❯ 1. Yes',
      '   2. Yes, and switch to accept edits (auto-approve file edits and common file commands) for this',
      '      session (shift+tab)',
      '   3. No',
    ])!;
    expect(write.options[1].label,
        'Yes, and switch to accept edits (auto-approve file edits and common file commands) for this session (shift+tab)');
    expect(write.context, ['Create file', 'note.txt', '1 hi']);

    final question = parsePromptMenu([
      rule,
      ' ☐ 색상 선택',
      '',
      '어떤 색을 선호하나요?',
      '',
      '❯ 1. 빨강',
      '     밝고 활기찬 빨간색',
      '  2. 파랑',
      '     침착하고 안정적인 파란색',
      '  3. Type something.',
      rule,
      '  4. Chat about this',
      '',
      'Enter to select · ↑/↓ to navigate · Esc to cancel',
    ])!;
    expect(question.title, '어떤 색을 선호하나요?');
    expect(question.context, ['☐ 색상 선택']);
    expect(question.options.length, 4);
    expect(question.options[0].note, '밝고 활기찬 빨간색');
    expect(question.options[2].note, '');

    final codex = parsePromptMenu([
      '• Running touch hello.txt',
      '',
      '  Would you like to run the following command?',
      '',
      '  Environment: local',
      '',
      '  Reason: Allow me to create hello.txt in the current workspace?',
      '',
      '  \$ touch hello.txt',
      '',
      '› 1. Yes, proceed (y)',
      "  2. Yes, and don't ask again for commands that start with `touch hello.txt` (p)",
      '  3. No, and tell Codex what to do differently (esc)',
      '',
      '  Press enter to confirm or esc to cancel',
    ])!;
    expect(codex.title, '\$ touch hello.txt');
    expect(codex.context, [
      'Would you like to run the following command?',
      'Environment: local',
      'Reason: Allow me to create hello.txt in the current workspace?',
    ]);
    expect(codex.options[2].label, 'No, and tell Codex what to do differently (esc)');
  });
}
