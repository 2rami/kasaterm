/// 정리 — 작업판에서 고른 일 하나(나쵸 장부 작업이나 판의 학생)의 현재 작업·변경·다음 일·막힘·검증.
///
/// PC 작업 탭의 정리 칸(`native_board/side.rs` `organize_lines`)과 같은 다섯 칸·같은 말을 쓴다.
/// 재료는 정본 셋뿐이다 — 나쵸 장부 상세(`/api/app/tasks/<id>`, 학생 보고 포함), 판의 학생 상태.
/// 정본에 없는 칸은 모른다고 적고, 폰이 할 수 없는 일(git 직접 읽기, 키 없는 허브의 장부)은 까닭을 적는다.
library;

import 'nacho.dart';
import 'workboard.dart';

enum SlotTone { plain, quiet, alert, ok }

class OrganizeSlot {
  const OrganizeSlot(this.label, this.value, this.tone);

  final String label;
  final String value;
  final SlotTone tone;
}

class OrganizeView {
  const OrganizeView({
    required this.title,
    required this.slots,
    this.sources = const [],
    this.demo = false,
  });

  final String title;
  final List<OrganizeSlot> slots;

  /// 칸들이 어느 정본에서 왔나 — 「나쵸 장부 · 승인 대기 · 1분 전」처럼.
  final List<String> sources;
  final bool demo;
}

List<String> _changed(Map<String, Object?>? report) => [
  for (final c in (report?['changed'] as List? ?? const []))
    if (c.toString().trim().isNotEmpty) c.toString().trim(),
];

String _text(Map<String, Object?>? m, String key) =>
    (m?[key]?.toString() ?? '').trim();

/// [task] 가 없으면 장부 없이 판만으로 채운다. [ledgerProblem] 은 장부를 못 읽은 까닭(키 없는 허브 등) —
/// 있으면 장부에서만 오는 칸에 그대로 적는다. 권한을 넓혀 다른 길로 채우지 않는다.
OrganizeView organize({
  required WorkItem item,
  NachoTaskDetail? task,
  String? ledgerProblem,
  int? nowMs,
}) {
  final pane = item.pane;
  final report = task?.report;
  final reportStatus = _text(report, 'status');
  final unknownLedger = task == null && item.taskId == null
      ? (ledgerProblem == null
            ? '나쵸 장부에 이 학생의 작업이 없어요 — 학생이 나쵸에 보고하면 채워져요'
            : '나쵸 장부를 못 읽어요 — $ledgerProblem')
      : (ledgerProblem == null ? null : '나쵸 장부를 못 읽어요 — $ledgerProblem');
  final slots = <OrganizeSlot>[];

  // 현재 작업 — 장부의 요청과 지금 단계, 없으면 판의 제목과 하는 일.
  final now = <String>[];
  if (task != null) {
    now.add(task.request.isEmpty ? item.title : task.request);
    if (task.step.isNotEmpty && task.step != task.request) now.add(task.step);
  } else {
    now.add(item.title);
    if (pane != null) {
      now.add(pane.isWaiting ? pane.kindLabel : pane.busyLabel);
    } else if (item.detail.isNotEmpty) {
      now.add(item.detail);
    }
  }
  slots.add(OrganizeSlot('현재 작업', now.join('\n'), SlotTone.plain));

  // 변경 — 폰은 git 을 직접 읽지 않는다. 브랜치(판)와 학생이 보고한 파일만.
  final changed = _changed(report);
  final change = <String>[
    if ((pane?.branch ?? '').isNotEmpty) '브랜치 ${pane!.branch}',
    if (changed.isNotEmpty) ...[
      '보고한 파일 ${changed.length}개',
      ...changed.take(4),
      if (changed.length > 4) '외 ${changed.length - 4}개',
    ] else if (task != null)
      '학생 보고 전이라 바뀐 파일을 몰라요 — 폰은 git 을 직접 읽지 않아요'
    else
      unknownLedger ?? '바뀐 파일을 몰라요 — 폰은 git 을 직접 읽지 않아요',
  ];
  slots.add(
    OrganizeSlot(
      '변경',
      change.join('\n'),
      changed.isEmpty ? SlotTone.quiet : SlotTone.plain,
    ),
  );

  // 다음 일 — 학생이 적은 다음 일, 없으면 나쵸의 남은 단계.
  final next = _text(report, 'next');
  final remaining = task?.remaining ?? const <String>[];
  slots.add(
    next.isNotEmpty
        ? OrganizeSlot('다음 일', next, SlotTone.plain)
        : remaining.isNotEmpty
        ? OrganizeSlot(
            '다음 일',
            remaining.map((r) => '· $r').join('\n'),
            SlotTone.plain,
          )
        : task == null && unknownLedger != null
        ? OrganizeSlot('다음 일', unknownLedger, SlotTone.quiet)
        : const OrganizeSlot('다음 일', '적힌 다음 일이 없어요', SlotTone.quiet),
  );

  // 막힘 — 승인·막힘 보고·사람 답 기다림 순. 모르면 모른다고.
  final approval = task?.approval;
  final blocked = task?.blocked ?? '';
  OrganizeSlot stuck;
  if (approval != null || task?.state == 'approval_needed') {
    final what = _text(approval, 'what');
    stuck = OrganizeSlot(
      '막힘',
      '승인이 있어야 이어져요${what.isEmpty ? '' : ' · $what'}',
      SlotTone.alert,
    );
  } else if (blocked.isNotEmpty) {
    stuck = OrganizeSlot('막힘', '막혔어요 · $blocked', SlotTone.alert);
  } else if (const {
    'blocked',
    'needs_restart',
    'needs_approval',
  }.contains(reportStatus)) {
    final why = next.isEmpty ? '막힌 까닭이 적혀 있지 않아요' : next;
    stuck = OrganizeSlot('막힘', switch (reportStatus) {
      'needs_restart' => '재시작이 있어야 이어져요 · $why',
      'needs_approval' => '승인이 있어야 이어져요 · $why',
      _ => '막혔어요 · $why',
    }, SlotTone.alert);
  } else if (pane != null && needsYou(pane)) {
    stuck = OrganizeSlot(
      '막힘',
      '사람 답을 기다려요 · ${pane.kindLabel}',
      SlotTone.alert,
    );
  } else if (task?.paused == true) {
    stuck = const OrganizeSlot('막힘', '멈춰 둔 일이에요', SlotTone.quiet);
  } else if (task == null && ledgerProblem != null) {
    stuck = OrganizeSlot('막힘', '판에선 막힘이 안 보여요 · 나쵸 장부는 못 읽어요', SlotTone.quiet);
  } else {
    stuck = const OrganizeSlot('막힘', '없음', SlotTone.quiet);
  }
  slots.add(stuck);

  // 검증 — 장부에 적힌 검증만 「통과」다. 학생이 적은 검사는 그렇다고 밝힌다.
  final verify = task?.verify;
  final tests = _text(report, 'tests');
  OrganizeSlot check;
  if (verify != null) {
    final note = _text(verify, 'note');
    check = verify['ok'] == true
        ? OrganizeSlot(
            '검증',
            '나쵸 검증 통과${note.isEmpty ? '' : ' · $note'}',
            SlotTone.ok,
          )
        : OrganizeSlot(
            '검증',
            '나쵸 검증 실패${note.isEmpty ? '' : ' · $note'}',
            SlotTone.alert,
          );
  } else if (reportStatus == 'failed') {
    check = OrganizeSlot(
      '검증',
      tests.isEmpty ? '실패 보고' : '실패 보고 · $tests',
      SlotTone.alert,
    );
  } else if (tests.isNotEmpty) {
    check = OrganizeSlot('검증', '학생이 적은 검사 · $tests', SlotTone.plain);
  } else if (task?.state == 'done') {
    check = const OrganizeSlot('검증', '끝남 · 검증 기록 없음', SlotTone.quiet);
  } else if (task == null && unknownLedger != null) {
    check = OrganizeSlot('검증', unknownLedger, SlotTone.quiet);
  } else {
    check = const OrganizeSlot('검증', '검증 기록 없음', SlotTone.quiet);
  }
  slots.add(check);

  final at = nowMs ?? DateTime.now().millisecondsSinceEpoch;
  final sources = <String>[
    if (item.source == WorkSource.demo) '예시 데이터 — 실제 작업이 아니에요',
    if (task != null)
      '나쵸 장부 · ${task.stateLabel} · ${freshLabel(task.updatedMs == 0 ? null : task.updatedMs, nowMs: at)}',
    if (report != null)
      [
        '학생 보고',
        if (_text(report, 'character').isNotEmpty) _text(report, 'character'),
        if (reportStatus.isNotEmpty) reportStatus,
        freshLabel((report['at_ms'] as num?)?.toInt(), nowMs: at),
      ].join(' · '),
    if (pane != null)
      '판 · ${[if ((item.machine ?? '').isNotEmpty) item.machine!, pane.displayName, pane.id].join(' ')}',
    if (task == null && ledgerProblem != null) '나쵸 장부 없음 · $ledgerProblem',
  ];
  return OrganizeView(
    title: item.title,
    slots: slots,
    sources: sources,
    demo: item.source == WorkSource.demo,
  );
}

/// 예시 판의 줄이 가리키는 장부 상세 — 나쵸 고정 자료(task.detail) 모양. 화면이 늘 예시라고 밝힌다.
NachoTaskDetail? demoTaskDetail(WorkItem item, {int? nowMs}) {
  final now = nowMs ?? DateTime.now().millisecondsSinceEpoch;
  int ago(int secs) => now - secs * 1000;
  Map<String, Object?> base(String state, String label, int secs) => {
    'id': item.taskId ?? item.key,
    'goal': item.title,
    'request': item.title,
    'state': state,
    'state_label': label,
    'project': item.project,
    'updated_ms': ago(secs),
  };
  return switch (item.key) {
    'demo:1' => NachoTaskDetail({
      ...base('approval_needed', '승인 대기', 90),
      'step': '사람 승인 대기 — request-journal 재시작',
      'approval': {'needed': true, 'what': 'request-journal 재시작 한 번 — 맥북'},
      'remaining': ['재시작 뒤 펫 응답 확인'],
    }),
    'demo:3' => NachoTaskDetail({
      ...base('working', '진행 중', 120),
      'step': '학생 보고를 받았다',
      'report': {
        'status': 'done',
        'summary': '작업판 모드 줄과 정리 시트',
        'changed': [
          'mobile/lib/organize.dart',
          'mobile/lib/screens/organize_sheet.dart',
          'mobile/lib/screens/workboard_view.dart',
          'mobile/test/organize_test.dart',
          'mobile/test/workboard_test.dart',
        ],
        'tests': 'flutter test organize·workboard 통과',
        'next': '실제 글꼴 캡처로 화면 확인',
        'character': '케이',
        'at_ms': ago(120),
      },
    }),
    'demo:5' => NachoTaskDetail({
      ...base('verifying', '검증 중', 1500),
      'step': '나쵸가 직접 검증하는 중',
      'report': {
        'status': 'done',
        'changed': ['app/kasaterm/src/machines.rs'],
        'tests': '검사 32개 통과 · push 전',
        'character': '미도리',
        'at_ms': ago(1600),
      },
    }),
    'demo:6' => NachoTaskDetail({
      ...base('done', '끝남', 5400),
      'verify': {
        'ok': true,
        'note': 'TestFlight 2609252153 설치 확인',
        'at_ms': ago(5400),
      },
    }),
    _ => null,
  };
}
