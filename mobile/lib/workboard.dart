/// 작업판 — 나쵸 장부(작업)와 실시간 판(학생·기기)을 「거노 차례 → 진행 → 검증 → 완료」
/// 네 줄기로 펴는 모델. 화면은 이것만 보고 그린다.
///
/// 정본은 둘이다. 작업의 상태·승인은 나쵸 장부(`/api/app/tasks`), 학생이 지금 무엇을 하는지와
/// 기기가 살아 있는지는 카사텀 판(`term/panes`·`machines`). 여기서 짐작해 합치지 않는다 —
/// 장부의 학생은 [StudentLookup] 이 기계까지 정한 것만 잇고, 못 이으면 따로 둔다.
/// 줄마다 어느 정본에서 왔는지([WorkSource])를 달아, 예시 데이터가 진짜처럼 섞이지 않게 한다.
library;

import 'dart:async';

import 'package:flutter/foundation.dart';

import 'nacho.dart';
import 'nacho_student.dart';
import 'server.dart';

enum WorkLane { yours, running, verifying, done }

/// 거노 차례인 까닭. 승인·질문은 거노가 답해야 이어지고, 막힘·멈춤은 판단이 필요하다.
enum YoursKind { approval, question, blocked, paused }

/// 줄의 출처. 예시는 검사와 화면 확인에서만 쓰고, 화면이 늘 그렇다고 밝힌다.
enum WorkSource { ledger, live, demo }

class WorkItem {
  const WorkItem({
    required this.key,
    required this.source,
    required this.lane,
    required this.title,
    required this.project,
    this.yours,
    this.detail = '',
    this.stateLabel = '',
    this.updatedMs,
    this.taskId,
    this.pane,
    this.machine,
    this.failed = false,
  });

  final String key;
  final WorkSource source;
  final WorkLane lane;
  final YoursKind? yours;
  final String title;
  final String project;

  /// 한 줄 설명 — 장부의 attention·step, 판의 doing.
  final String detail;
  final String stateLabel;

  /// 마지막으로 바뀐 때. 신선도를 이것으로 말한다.
  final int? updatedMs;
  final String? taskId;

  /// 맡은 학생의 지금 pane — 장부 작업은 이어진 경우만.
  final Pane? pane;

  /// 사람에게 보일 기계 이름.
  final String? machine;
  final bool failed;
}

class WorkDevice {
  const WorkDevice({
    required this.label,
    required this.online,
    this.route,
    this.agoSecs,
    this.rttMs,
    this.here = false,
    this.students = const [],
  });

  final String label;
  final String? route;
  final bool online;
  final int? agoSecs;
  final int? rttMs;

  /// 앱이 붙은 그 기계.
  final bool here;
  final List<Pane> students;

  int get waiting => students.where(needsYou).length;
}

class WorkBoard {
  const WorkBoard({
    this.items = const [],
    this.devices = const [],
    this.demo = false,
  });

  final List<WorkItem> items;
  final List<WorkDevice> devices;
  final bool demo;

  /// 프로젝트별 (거노 차례 수, 전체 수) — 거노 차례가 많은 것부터.
  List<({String name, int yours, int total})> get projects {
    final m = <String, ({int yours, int total})>{};
    for (final i in items) {
      final p = m[i.project] ?? (yours: 0, total: 0);
      m[i.project] = (
        yours: p.yours + (i.lane == WorkLane.yours ? 1 : 0),
        total: p.total + 1,
      );
    }
    final out = [
      for (final e in m.entries)
        (name: e.key, yours: e.value.yours, total: e.value.total),
    ];
    out.sort((a, b) {
      final y = b.yours.compareTo(a.yours);
      if (y != 0) return y;
      final t = b.total.compareTo(a.total);
      return t != 0 ? t : a.name.compareTo(b.name);
    });
    return out;
  }

  List<WorkItem> lane(WorkLane lane, {String? project}) => [
    for (final i in items)
      if (i.lane == lane && (project == null || i.project == project)) i,
  ];
}

/// 사람 손이 필요한 기다림 — 승인·질문만. 그냥 쉬는 학생(idle)은 아니다.
bool needsYou(Pane p) =>
    p.isWaiting && (p.kind == 'permission' || p.kind == 'question');

/// 장부의 상태 아홉을 네 줄기로. 앞으로만 가는 상태라 되돌림은 서버가 판단한다.
(WorkLane, YoursKind?, bool failed) laneOfTask(NachoTaskCard t) {
  switch (t.state) {
    case 'approval_needed':
      return (WorkLane.yours, YoursKind.approval, false);
    case 'verifying' || 'restart_pending' || 'post_restart_verifying':
      return (WorkLane.verifying, null, false);
    case 'done':
      return (WorkLane.done, null, false);
    case 'failed' || 'cancelled':
      return (WorkLane.done, null, true);
  }
  if (t.group == 'attention') {
    return (
      WorkLane.yours,
      t.paused ? YoursKind.paused : YoursKind.blocked,
      false,
    );
  }
  if (t.group == 'closed') return (WorkLane.done, null, false);
  return (WorkLane.running, null, false);
}

/// pane 의 프로젝트 — 장부와 같은 규칙(작업 폴더 이름).
String projectOf(String cwd) {
  final parts = cwd.split('/').where((s) => s.isNotEmpty).toList();
  return parts.isEmpty ? '분류 안 됨' : parts.last;
}

/// 장부 작업 + 판의 학생을 한 판으로.
///
/// 장부 작업이 이어 잡은 pane 은 따로 세우지 않는다(같은 일이 두 줄이 된다). 판에서는 사람
/// 손이 필요한 학생과 지금 움직이는 학생만 줄로 세우고, 쉬는 학생은 기기 목록에만 둔다.
/// 거울 pane(`mirrorOf`)은 원본 기계 쪽에서 이미 오므로 뺀다.
WorkBoard buildBoard({
  required List<NachoTaskCard> tasks,
  required List<WorkDevice> devices,
  StudentSeat? Function(Map<String, Object?> student)? seatOf,
  int? nowMs,
}) {
  final items = <WorkItem>[];
  final claimed = <String>{};
  for (final t in tasks) {
    final (lane, yours, failed) = laneOfTask(t);
    final seat = t.student == null ? null : seatOf?.call(t.student!);
    final pane = seat?.pane;
    if (pane != null) claimed.add('${seat!.route ?? ''}|${pane.id}');
    items.add(
      WorkItem(
        key: 'task:${t.id}',
        source: WorkSource.ledger,
        lane: lane,
        yours: yours,
        title: t.goal.isEmpty ? '(제목 없는 일)' : t.goal,
        project: t.project,
        detail: t.attention.isNotEmpty ? t.attention : t.step,
        stateLabel: t.stateLabel,
        updatedMs: t.updatedMs == 0 ? null : t.updatedMs,
        taskId: t.id,
        pane: pane,
        machine: seat?.machine,
        failed: failed,
      ),
    );
  }
  final now = nowMs ?? DateTime.now().millisecondsSinceEpoch;
  for (final d in devices) {
    for (final p in d.students) {
      if (p.isShell || p.isWebShell || p.closed) continue;
      if ((p.mirrorOf ?? '').isNotEmpty) continue;
      if (claimed.contains('${d.route ?? ''}|${p.id}')) continue;
      final yours = needsYou(p);
      if (!yours && !p.isBusy) continue;
      items.add(
        WorkItem(
          key: 'pane:${d.route ?? ''}|${p.id}',
          source: WorkSource.live,
          lane: yours ? WorkLane.yours : WorkLane.running,
          yours: yours
              ? (p.kind == 'permission'
                    ? YoursKind.approval
                    : YoursKind.question)
              : null,
          title: _paneTitle(p),
          project: projectOf(p.cwd),
          detail: yours ? p.kindLabel : _doingLine(p),
          stateLabel: yours ? p.kindLabel : '작업 중',
          updatedMs: p.idleSecs == null ? now : now - p.idleSecs! * 1000,
          pane: p,
          machine: d.label,
        ),
      );
    }
  }
  return WorkBoard(items: items, devices: devices);
}

/// 창 제목 앞의 도는 표시(◐·✳·점자 스피너)는 터미널 장식이라 걷는다.
final _titleGlyphs = RegExp(
  r'^[\s\u25D0-\u25D3\u2733\u2736\u273B\u273D\u2800-\u28FF\u00B7\u2022\u25CF\u25CB\u23FA]+',
);

String _paneTitle(Pane p) {
  final s = (p.session ?? '').trim();
  if (s.isNotEmpty) return s;
  final t = p.title.replaceFirst(_titleGlyphs, '').trim();
  return t.isEmpty ? '${p.displayName}의 일' : t;
}

/// 「Bash 설명 — 명령 원문」에서 설명까지만. 원문은 터미널에서 본다.
String _doingLine(Pane p) => p.busyLabel.split(' — ').first;

/// 「지금 · 12초 전 · 3분 전 · 2시간 전 · 3일 전」.
String freshLabel(int? ms, {int? nowMs}) {
  if (ms == null) return '언제인지 모름';
  final now = nowMs ?? DateTime.now().millisecondsSinceEpoch;
  return freshSecsLabel(((now - ms) / 1000).round());
}

String freshSecsLabel(int? secs) {
  if (secs == null) return '언제인지 모름';
  if (secs < 5) return '지금';
  if (secs < 60) return '$secs초 전';
  if (secs < 3600) return '${secs ~/ 60}분 전';
  if (secs < 86400) return '${secs ~/ 3600}시간 전';
  return '${secs ~/ 86400}일 전';
}

/// 실시간 판 — 이 주소의 기계와 명부의 다른 기계들. 화면이 떠 있는 동안만 돈다.
class LiveRoster extends ChangeNotifier {
  LiveRoster(this.server, {this.every = const Duration(seconds: 5)});

  final Server server;
  final Duration every;

  List<WorkDevice> devices = const [];
  String? problem;

  /// 마지막으로 끝까지 받은 때 — 이게 오래되면 화면이 「몇 초째 못 받음」을 말한다.
  DateTime? okAt;

  Timer? _timer;
  bool _busy = false;
  bool _disposed = false;

  void start() {
    unawaited(refresh());
    _timer ??= Timer.periodic(every, (_) => unawaited(refresh()));
  }

  void stop() {
    _timer?.cancel();
    _timer = null;
  }

  Future<void> refresh() async {
    if (_busy || _disposed) return;
    _busy = true;
    try {
      final got = await Future.wait<Object?>([
        server.panes(),
        server.machines(),
        server.me().then<Me?>((m) => m, onError: (_) => null),
      ]);
      final here = got[0] as List<Pane>;
      final machines = got[1] as List<Machine>;
      final me = got[2] as Me?;
      if (_disposed) return;
      devices = [
        WorkDevice(
          label: me?.machine ?? '이 기계',
          online: true,
          agoSecs: 0,
          here: true,
          students: here,
        ),
        for (final m in machines)
          WorkDevice(
            label: m.label,
            route: m.route,
            online: m.online,
            agoSecs: m.agoSecs,
            rttMs: m.rttMs,
            students: m.panes,
          ),
      ];
      problem = null;
      okAt = DateTime.now();
    } on ServerException catch (e) {
      problem = e.message;
    } finally {
      _busy = false;
      if (!_disposed) notifyListeners();
    }
  }

  @override
  void dispose() {
    _disposed = true;
    stop();
    super.dispose();
  }
}

/// 검사와 시뮬레이터 확인용 예시. 화면은 [WorkBoard.demo] 를 보고 「예시」 띠를 두른다.
WorkBoard demoBoard({int? nowMs}) {
  final now = nowMs ?? DateTime.now().millisecondsSinceEpoch;
  int ago(int secs) => now - secs * 1000;
  Pane pane(
    String id,
    String name,
    String slug,
    String status, {
    String? kind,
    String cwd = '/w/kasaterm',
    int? idle,
  }) => Pane(
    id: id,
    name: name,
    title: '',
    status: status,
    window: 0,
    cwd: cwd,
    slug: slug,
    kind: kind,
    idleSecs: idle,
  );
  final kei = pane('%5', '케이', 'kei', 'working');
  final mashiro = pane('%0', '마시로', 'mashiro', 'working', cwd: '/w/nacho-neko');
  final arona = pane(
    '%9',
    '아로나',
    'arona',
    'waiting',
    kind: 'question',
    idle: 40,
  );
  return WorkBoard(
    demo: true,
    items: [
      WorkItem(
        key: 'demo:1',
        source: WorkSource.demo,
        lane: WorkLane.yours,
        yours: YoursKind.approval,
        title: '펫 두뇌 다시 켜기',
        project: 'kasaterm',
        detail: 'request-journal 재시작 한 번 — 맥북',
        stateLabel: '승인 필요',
        updatedMs: ago(90),
        taskId: 'demo-approval',
        pane: kei,
        machine: '맥북',
      ),
      WorkItem(
        key: 'demo:2',
        source: WorkSource.demo,
        lane: WorkLane.yours,
        yours: YoursKind.question,
        title: '빠른 설정 순서 — 어느 쪽?',
        project: 'kasaterm',
        detail: '질문 기다림',
        stateLabel: '질문 기다림',
        updatedMs: ago(40),
        pane: arona,
        machine: '맥북',
      ),
      WorkItem(
        key: 'demo:3',
        source: WorkSource.demo,
        lane: WorkLane.running,
        title: '폰 작업판 구현',
        project: 'kasaterm',
        detail: 'Edit workboard_view.dart',
        stateLabel: '작업 중',
        updatedMs: ago(2),
        pane: kei,
        machine: '맥북',
      ),
      WorkItem(
        key: 'demo:4',
        source: WorkSource.demo,
        lane: WorkLane.running,
        title: '나쵸 이벤트 인박스 계약',
        project: 'nacho-neko',
        detail: 'Bash pytest tests/test_inbox.py',
        stateLabel: '작업 중',
        updatedMs: ago(240),
        pane: mashiro,
        machine: '맥미니',
      ),
      WorkItem(
        key: 'demo:5',
        source: WorkSource.demo,
        lane: WorkLane.verifying,
        title: '원격 자리 이동 뒤 배치 갱신',
        project: 'kasaterm',
        detail: '검사 32개 통과 · push 전',
        stateLabel: '검증 중',
        updatedMs: ago(1500),
        machine: '맥북',
      ),
      WorkItem(
        key: 'demo:6',
        source: WorkSource.demo,
        lane: WorkLane.done,
        title: '폰 「터미널 | 대화」 두 화면',
        project: 'kasaterm',
        detail: 'TestFlight 2609252153',
        stateLabel: '끝남',
        updatedMs: ago(5400),
        machine: '맥북',
      ),
    ],
    devices: [
      WorkDevice(
        label: '맥북',
        online: true,
        agoSecs: 0,
        here: true,
        students: [kei, arona],
      ),
      WorkDevice(
        label: '맥미니',
        online: true,
        agoSecs: 12,
        rttMs: 100,
        students: [mashiro],
      ),
      const WorkDevice(label: '집컴', online: false, agoSecs: 10800),
    ],
  );
}
