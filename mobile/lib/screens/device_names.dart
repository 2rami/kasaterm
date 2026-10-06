import 'dart:async';

import 'package:flutter/material.dart';

import '../look.dart';
import '../machine_look.dart';
import '../relay_account.dart';
import '../server.dart';
import '../twins_loading.dart';
import 'controls.dart';

/// 이름을 붙일 수 있는 기기 — id 를 아는 기기만이다. 이름은 id 에 붙는다.
class DeviceEntry {
  const DeviceEntry({required this.id, required this.label, this.local = false});
  final String id;

  /// 기기 제 이름(명부·컴퓨터 이름). 색·아이콘·길은 이것으로 찾는다.
  final String label;

  /// 폰이 붙은 기준 기기.
  final bool local;
}

/// 기준 기기와 `~id` 로 닿는 계정 기기. 같은 id 는 한 줄.
List<DeviceEntry> deviceEntries({String? rootId, String? rootName, required List<Machine> machines}) {
  final out = <DeviceEntry>[
    if (rootId != null) DeviceEntry(id: rootId, label: rootName ?? '이 기계', local: true),
  ];
  for (final m in machines) {
    if (m.route.length < 2 || !m.route.startsWith('~')) continue;
    final id = m.route.substring(1);
    if (out.any((e) => e.id == id)) continue;
    out.add(DeviceEntry(id: id, label: m.label.isEmpty ? id : m.label));
  }
  return out;
}

/// 기기 이름 — 계정 설정 `device_names`(기기 id → 이름). 바꾼 이름은 같은 계정의 PC·폰 화면에 함께 뜬다.
/// 길(`m/<이름>/`)·기기색·아이콘은 기기 제 이름 그대로라 바꿔도 안 끊긴다.
class DeviceNamesStore extends ChangeNotifier {
  DeviceNamesStore({required this.server, required this.api});

  final Server server;
  final RelayAccountApi Function() api;

  static const _keys = ['device_names'];

  bool loading = true;
  String? error;
  List<DeviceEntry> devices = const [];
  Map<String, String> names = const {};
  AccountSyncSnapshot? _snapshot;
  bool _closed = false;

  Future<void> load() async {
    loading = true;
    error = null;
    _notify();
    final machines = server.machines().then<List<Machine>?>((v) => v, onError: (_) => null);
    final me = server.me().then<Me?>((v) => v, onError: (_) => null);
    final rootId = server.machineId();
    final client = api();
    try {
      _take(await client.readSyncWith(_keys));
      devices = deviceEntries(rootId: await rootId, rootName: (await me)?.machine, machines: await machines ?? const []);
      if (devices.isEmpty) error = '이름을 붙일 기기를 못 찾았어요. 데스크톱이 꺼졌거나 옛 판이에요.';
    } on AccountException catch (e) {
      error = e.message;
    } finally {
      client.close();
    }
    loading = false;
    _notify();
  }

  void _take(AccountSyncSnapshot snapshot) {
    _snapshot = snapshot;
    names = parseDeviceNames(snapshot.settings['device_names']);
    machineLooks.value = machineLooks.value.copyWith(names: names);
  }

  String shown(DeviceEntry d) => names[d.id] ?? d.label;

  /// 이름을 바꾼다. 빈 이름은 기기 제 이름으로 돌린다. 실패하면 사람에게 보일 한 줄.
  Future<String?> rename(DeviceEntry d, String name) async {
    final client = api();
    try {
      var snapshot = _snapshot ?? await client.readSyncWith(_keys);
      for (var attempt = 0; attempt < 3; attempt++) {
        final next = renameDevice(parseDeviceNames(snapshot.settings['device_names']), d.id, name);
        try {
          _take(await client.patchSync(snapshot.revision, {'device_names': next.isEmpty ? null : next}, keys: _keys));
          _notify();
          return null;
        } on AccountSyncConflict catch (e) {
          snapshot = e.current;
        }
      }
      _take(snapshot);
      _notify();
      return '다른 기기에서 이름이 바뀌었어요. 다시 해 주세요.';
    } on AccountException catch (e) {
      // 옛 관문은 모르는 설정 키를 400 으로 거부한다.
      return e.status == 400 ? '관문이 아직 기기 이름을 몰라요. 관문 새 판이 올라간 뒤 다시 해 주세요.' : e.message;
    } finally {
      client.close();
    }
  }

  void _notify() {
    if (!_closed) notifyListeners();
  }

  @override
  void dispose() {
    _closed = true;
    super.dispose();
  }
}

class DeviceNamesScreen extends StatefulWidget {
  const DeviceNamesScreen({super.key, required this.server, required this.api});

  final Server server;
  final RelayAccountApi Function() api;

  @override
  State<DeviceNamesScreen> createState() => _DeviceNamesScreenState();
}

class _DeviceNamesScreenState extends State<DeviceNamesScreen> {
  late final store = DeviceNamesStore(server: widget.server, api: widget.api);

  @override
  void initState() {
    super.initState();
    unawaited(store.load());
  }

  @override
  void dispose() {
    store.dispose();
    super.dispose();
  }

  Future<void> _edit(DeviceEntry d) async {
    final named = store.names.containsKey(d.id);
    final name = await showDialog<String>(
      context: context,
      builder: (_) => ModalLook(child: _NameDialog(initial: store.shown(d), label: d.label, named: named)),
    );
    if (name == null || (name.isEmpty ? !named : name == store.shown(d))) return;
    final problem = await store.rename(d, name);
    if (problem != null && mounted) {
      ScaffoldMessenger.of(context)
        ..hideCurrentSnackBar()
        ..showSnackBar(SnackBar(content: Text(problem)));
    }
  }

  @override
  Widget build(BuildContext context) => TwinBackdrop(
    child: Scaffold(
      backgroundColor: Colors.transparent,
      appBar: AppBar(backgroundColor: Colors.transparent, title: const Text('기기 이름')),
      body: ListenableBuilder(listenable: store, builder: (context, _) => _body()),
    ),
  );

  Widget _body() {
    if (store.loading && store.devices.isEmpty) {
      return const Center(child: TwinsLoading(label: '기기 목록을 받는 중', size: Look.twinsSmall));
    }
    if (store.error != null) {
      return Center(
        child: TwinsNotice(
          text: store.error!,
          action: FilledButton(onPressed: store.load, child: const Text('다시 시도')),
        ),
      );
    }
    final theme = Theme.of(context);
    return ListView(
      padding: const EdgeInsets.fromLTRB(Look.pagePad, 0, Look.pagePad, Look.groupGap * 2),
      children: [
        SettingsGroup(
          children: [
            for (final (i, d) in store.devices.indexed)
              SettingsRow(
                key: Key('device-${d.id}'),
                tone: i,
                icon: machineIcon(d.label),
                title: store.shown(d),
                subtitle: [
                  store.names.containsKey(d.id) ? '원래 이름 ${d.label}' : '이름을 안 붙였어요',
                  if (d.local) '이 폰이 붙은 기기',
                ].join(' · '),
                chevron: true,
                onTap: () => unawaited(_edit(d)),
              ),
          ],
        ),
        Padding(
          padding: const EdgeInsets.fromLTRB(4, Look.groupGap, 4, 0),
          child: Text(
            '바꾼 이름은 같은 KASA 계정의 PC·폰 화면에 함께 떠요. 데스크톱 터미널에서 `to` 로 부르는 이름과 기기색은 그대로예요.',
            style: theme.textTheme.bodySmall?.copyWith(color: theme.colorScheme.onSurfaceVariant),
          ),
        ),
      ],
    );
  }
}

/// 입력칸 버퍼는 대화상자가 쥔다 — 닫히는 동안에도 입력칸이 그려지므로 밖에서 먼저 버리면 안 된다.
class _NameDialog extends StatefulWidget {
  const _NameDialog({required this.initial, required this.label, required this.named});

  final String initial;
  final String label;
  final bool named;

  @override
  State<_NameDialog> createState() => _NameDialogState();
}

class _NameDialogState extends State<_NameDialog> {
  late final _ctl = TextEditingController(text: widget.initial);

  @override
  void dispose() {
    _ctl.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) => AlertDialog(
    title: const Text('기기 이름'),
    content: TextField(
      key: const Key('device-name-field'),
      controller: _ctl,
      autofocus: true,
      maxLength: 40,
      decoration: InputDecoration(helperText: '원래 이름 ${widget.label}'),
      onSubmitted: (v) => Navigator.pop(context, v.trim()),
    ),
    actions: [
      if (widget.named) TextButton(onPressed: () => Navigator.pop(context, ''), child: const Text('원래 이름으로')),
      TextButton(onPressed: () => Navigator.pop(context), child: const Text('그만')),
      FilledButton(onPressed: () => Navigator.pop(context, _ctl.text.trim()), child: const Text('바꾸기')),
    ],
  );
}
