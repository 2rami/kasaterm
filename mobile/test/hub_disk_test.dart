import 'package:flutter_test/flutter_test.dart';
import 'package:kasaterm_mobile/hub_model.dart';
import 'package:kasaterm_mobile/server.dart';

const gib = 1 << 30;

class DiskServer extends Server {
  DiskServer() : super(Uri.parse('http://127.0.0.1/'));

  final Map<String?, Object?> disks = {};

  @override
  Future<List<Pane>> panes({String? machine}) async => const [];

  @override
  Future<List<String>> sessions() async => const [];

  @override
  Future<List<Machine>> machines() async => const [
    Machine(label: '미니', route: '~mini', online: true, panes: []),
    Machine(label: '옛판', route: '~old', online: true, panes: []),
  ];

  @override
  Future<List<WindowLayout>> windows({String? machine}) async => const [];

  @override
  Future<List<Note>> notes({String? machine}) async => const [];

  @override
  Future<DiskStatus?> disk({String? machine}) async {
    final j = disks[machine];
    if (j == 'old') return null;
    return j is Map ? DiskStatus.fromJson(j.cast<String, Object?>()) : null;
  }
}

void main() {
  test('디스크 상태는 큰 곳·지운 것·문턱 순으로 둘째 줄을 쓴다', () {
    final d = DiskStatus.fromJson({
      'free': 9 * gib ~/ 2,
      'level': 'critical',
      'warn_free': 20 * gib,
      'big': [
        {'label': '/var/folders 임시', 'path': '/private/var/folders/x', 'bytes': 69 * gib},
        {'label': '~/Library/Caches', 'path': '/Users/x/Library/Caches', 'bytes': 4 * gib},
      ],
      'last_pass': {'freed': 12 * gib, 'removed': 2},
    });
    expect(d.warn && d.critical, isTrue);
    expect(d.headline, '디스크 4.5GB 남음');
    expect(d.detail, '큰 곳 · /var/folders 임시 69GB · ~/Library/Caches 4.0GB\n빌드 산출물 12GB 지움');
    expect(
      DiskStatus.fromJson({'free': 15 * gib, 'level': 'low', 'warn_free': 20 * gib}).detail,
      '문턱 20GB',
    );
  });

  test('경고는 문턱 아래인 기기에만 서고 옛 판 기기는 조용하다', () async {
    final server = DiskServer()
      ..disks[null] = {'free': 80 * gib, 'level': 'ok', 'warn_free': 20 * gib}
      ..disks['~mini'] = {'free': 10 * gib, 'level': 'low', 'warn_free': 20 * gib}
      ..disks['~old'] = 'old';
    final model = HubModel(server);
    await model.refresh();
    expect(model.diskOf(null), isNull);
    expect(model.diskOf('~mini')?.headline, '디스크 10GB 남음');
    expect(model.diskOf('~old'), isNull);
    server.disks['~mini'] = {'free': 30 * gib, 'level': 'ok', 'warn_free': 20 * gib};
    await model.refresh();
    expect(model.diskOf('~mini'), isNull);
  });
}
