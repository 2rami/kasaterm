import 'dart:convert';

import 'package:kasaterm_mobile/server.dart';

const syntheticCodexLabel = '[합성 QA · 실제 계정 아님]';
const codexToolCommand = 'flutter test test/codex_mobile_test.dart';
const codexLongReply =
    '로그인 상태와 데스크톱 연결 상태를 분리했습니다. '
    '같은 계정으로 로그인한 뒤 연결할 기기가 없으면 로그인 화면으로 되돌리지 않고 연결 대기 화면을 유지합니다. '
    '한글과 English가 섞인 긴 응답도 화면 너비에 맞춰 이어지며, '
    '위로 스크롤해 읽던 위치와 선택한 문장의 줄바꿈은 보존합니다.';

enum CodexScene { progress, complete, approval }

// Field names mirror http.rs term_panes_handler; character/name is optional.
Map<String, Object?> codexPaneJson(CodexScene scene) => {
  'id': '%7',
  'name': null,
  'slug': null,
  'harness': 'codex',
  'title': '모바일 계정 연결 검증',
  'window': 0,
  'cwd': '/workspace/kasa',
  'status': switch (scene) {
    CodexScene.progress => 'working',
    CodexScene.complete => 'idle',
    CodexScene.approval => 'waiting',
  },
  'kind': scene == CodexScene.approval ? 'permission' : null,
  'waiting_for': scene == CodexScene.approval ? codexToolCommand : null,
  'doing': scene == CodexScene.progress ? 'exec_command flutter test' : null,
  'idle_secs': scene == CodexScene.complete ? 12 : null,
  'model': 'gpt-5.6-sol',
  'model_label': 'GPT-5.6 Sol 1M',
  'effort': 'xhigh',
  'effort_label': 'xhigh',
  'context_pct': 16,
  'branch': 'main',
  'session': null,
  'background': <String>[],
  'subagents': <String>[],
};

Pane codexPane(CodexScene scene) => Pane.fromJson(codexPaneJson(scene));

// Menu marker/labels come from input.rs codex_live_choices_are_distinct_from_history_above_the_composer.
const codexApprovalRows = [
  syntheticCodexLabel,
  'Would you like to run the following command?',
  '',
  '  Reason: Run the focused mobile checks.',
  '  \$ flutter test test/codex_mobile_test.dart',
  '',
  '› 1. Yes, proceed (y)',
  '  2. No, and tell Codex what to do differently (esc)',
  '',
  '  Press enter to confirm or esc to cancel',
];

List<String> codexTerminalRows(CodexScene scene) => switch (scene) {
  CodexScene.approval => codexApprovalRows,
  _ => [
    syntheticCodexLabel,
    'OpenAI Codex',
    'model: gpt-5.6-sol (xhigh)',
    'directory: /workspace/kasa',
    '',
    '› 모바일 로그인과 한글 응답을 확인해 주세요.',
    '',
    '• 계정 연결 경계와 한글 표시를 확인하고 있어요.',
    '• Ran $codexToolCommand',
    if (scene == CodexScene.progress) ...[
      '  └ Running the focused fixture checks',
      '',
      '• Working (2s • esc to interrupt)',
    ] else ...[
      '  └ 12 tests passed',
      '',
      '• $codexLongReply',
      '',
      '  확인: 계정 전환 · 줄바꿈 · 글자 선택',
      '',
      '› 다음 작업을 입력해 주세요',
    ],
    '',
    '  gpt-5.6-sol xhigh · main · kasa · never · Context 16% used',
  ],
};

// The envelope matches conversation_test.dart's Codex rollout fixture. All content is synthetic.
String codexRollout(CodexScene scene) {
  Map<String, Object?> event(String type, Map<String, Object?> payload) => {
    'timestamp': '2026-09-28T00:00:00Z',
    'type': type,
    'payload': payload,
  };
  return [
    event('event_msg', {
      'type': 'user_message',
      'message': '$syntheticCodexLabel\n모바일 로그인과 한글 응답을 확인해 주세요.',
    }),
    event('event_msg', {
      'type': 'agent_message',
      'message': '계정 연결 경계와 한글 표시를 확인하고 있어요.',
    }),
    event('response_item', {
      'type': 'function_call',
      'name': 'exec_command',
      'call_id': 'synthetic-call',
      'arguments': jsonEncode({'cmd': codexToolCommand}),
    }),
    if (scene == CodexScene.complete) ...[
      event('response_item', {
        'type': 'function_call_output',
        'call_id': 'synthetic-call',
        'output': jsonEncode({
          'output': '12 tests passed\n한글 줄바꿈 fixture 통과',
          'metadata': {'exit_code': 0},
        }),
      }),
      event('event_msg', {
        'type': 'agent_message',
        'message': '**완료**\n\n$codexLongReply\n\n- 계정 전환 검사 통과\n- 글자 선택 검사 통과',
      }),
    ],
  ].map(jsonEncode).join('\n');
}
