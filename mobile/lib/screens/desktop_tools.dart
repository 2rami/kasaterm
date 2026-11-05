import 'package:flutter/material.dart';

import '../look.dart';
import '../machine_look.dart';
import '../server.dart';
import '../twins_loading.dart';
import 'controls.dart';
import 'desktop_controls.dart';
import 'desktop_settings.dart';
import 'screen_mirror.dart';

Future<void> chooseDesktopTools(BuildContext context, Server server) async {
  try {
    final machines = await server.machines();
    final root = await server.machineId();
    if (!context.mounted) return;
    final choices = <(String, String)>[
      if (root != null) ('~$root', '기준 기기'),
      for (final m in machines)
        if (m.online && m.route != '~$root')
          (m.route, machineName(m.label, route: m.route)),
    ];
    if (choices.isEmpty) {
      await openDesktopTools(context, server: server, title: '연결된 PC');
      return;
    }
    final route = await desktopChoice(context, '관리할 기기', choices);
    if (route == null || !context.mounted) return;
    await openDesktopTools(
      context,
      server: server,
      machine: route,
      title: choices.firstWhere((v) => v.$1 == route).$2,
    );
  } catch (error) {
    if (context.mounted) {
      ScaffoldMessenger.of(
        context,
      ).showSnackBar(SnackBar(content: Text(desktopError(error))));
    }
  }
}

Future<void> openDesktopTools(
  BuildContext context, {
  required Server server,
  required String title,
  String? machine,
}) async {
  var route = machine;
  if (route == null && server.account != null) {
    final id = await server.machineId();
    if (!context.mounted) return;
    if (id == null) {
      ScaffoldMessenger.of(context).showSnackBar(
        const SnackBar(content: Text('기기를 확인하지 못했어요. 다시 연결한 뒤 열어 주세요.')),
      );
      return;
    }
    route = '~$id';
  }
  if (!context.mounted) return;
  await Navigator.of(context).push<void>(
    MaterialPageRoute(
      builder: (_) =>
          DesktopToolsScreen(server: server, title: title, machine: route),
    ),
  );
}

class DesktopToolsScreen extends StatelessWidget {
  const DesktopToolsScreen({
    super.key,
    required this.server,
    required this.title,
    this.machine,
  });
  final Server server;
  final String title;
  final String? machine;
  @override
  Widget build(BuildContext context) => Scaffold(
    appBar: AppBar(title: Text(title)),
    body: ListView(
      padding: const EdgeInsets.all(Look.pagePad),
      children: [
        SettingsGroup(
          title: 'PC 기능',
          children: [
            SettingsRow(
              title: 'PC 설정',
              subtitle: '에이전트·모델·외형·셸·파일·입력',
              icon: Icons.tune,
              chevron: true,
              onTap: () => Navigator.push(
                context,
                MaterialPageRoute<void>(
                  builder: (_) => DesktopSettingsScreen(
                    server: server,
                    title: title,
                    machine: machine,
                  ),
                ),
              ),
            ),
            SettingsRow(
              title: '대화 기록 이어하기',
              subtitle: 'Claude·Codex 기록을 새 방에서 계속',
              icon: Icons.history,
              chevron: true,
              onTap: () => Navigator.push(
                context,
                MaterialPageRoute<void>(
                  builder: (_) => DesktopHistoryScreen(
                    server: server,
                    title: title,
                    machine: machine,
                  ),
                ),
              ),
            ),
            SettingsRow(
              title: 'PC 화면 조작',
              subtitle: '파일·Git·작업·캐릭터 편집 등 PC 화면의 모든 조작',
              icon: Icons.touch_app_outlined,
              chevron: true,
              onTap: () => Navigator.push(
                context,
                MaterialPageRoute<void>(
                  builder: (_) => ScreenMirrorScreen(
                    server: server,
                    title: title,
                    machine: machine,
                  ),
                ),
              ),
            ),
          ],
        ),
      ],
    ),
  );
}

class DesktopHistoryScreen extends StatefulWidget {
  const DesktopHistoryScreen({
    super.key,
    required this.server,
    required this.title,
    this.machine,
  });
  final Server server;
  final String title;
  final String? machine;
  @override
  State<DesktopHistoryScreen> createState() => _DesktopHistoryScreenState();
}

class _DesktopHistoryScreenState extends State<DesktopHistoryScreen> {
  List<Map<String, dynamic>>? _rows;
  String? _error;
  String _query = '';
  String? _pending;
  @override
  void initState() {
    super.initState();
    _load();
  }

  Future<void> _load() async {
    try {
      final v = await widget.server.desktopValues(
        'recent-sessions',
        machine: widget.machine,
        query: const {'scope': 'all'},
      );
      if (mounted) {
        setState(() {
          _rows = desktopRows(v['sessions']);
          _error = null;
        });
      }
    } catch (error) {
      if (mounted) setState(() => _error = desktopError(error));
    }
  }

  Future<void> _resume(Map<String, dynamic> row) async {
    final id = row['id'];
    if (id is! String || id.isEmpty || _pending != null) return;
    final harness = row['harness'];
    if (harness is! String ||
        !const ['claude', 'codex', 'agy'].contains(harness)) {
      return;
    }
    if (!await desktopConfirm(
          context,
          '${row['label'] ?? harness} 대화를 새 방에서 이어할까요?',
          action: '이어하기',
        ) ||
        !mounted) {
      return;
    }
    setState(() => _pending = id);
    try {
      await widget.server.desktopAction(
        'session-resume',
        machine: widget.machine,
        query: {
          'id': id,
          'harness': harness,
          'newroom': 'true',
          if (row['cwd'] is String) 'cwd': row['cwd'] as String,
        },
      );
      if (mounted) {
        ScaffoldMessenger.of(context).showSnackBar(
          const SnackBar(content: Text('새 방에서 대화를 이어했어요. 허브에서 열 수 있어요.')),
        );
      }
    } catch (error) {
      if (mounted) setState(() => _error = desktopError(error));
    } finally {
      if (mounted) setState(() => _pending = null);
    }
  }

  @override
  Widget build(BuildContext context) {
    final rows = _rows
        ?.where(
          (r) => '${r['label']} ${r['cwd']} ${r['harness']} ${r['character']}'
              .toLowerCase()
              .contains(_query.toLowerCase()),
        )
        .toList();
    return Scaffold(
      appBar: AppBar(title: Text('${widget.title} 대화 기록')),
      body: RefreshIndicator(
        onRefresh: _load,
        child: ListView(
          padding: const EdgeInsets.all(Look.pagePad),
          children: [
            TextField(
              decoration: const InputDecoration(
                hintText: '대화·폴더·에이전트 검색',
                prefixIcon: Icon(Icons.search),
              ),
              onChanged: (q) => setState(() => _query = q),
            ),
            if (_error != null) ErrorBand(text: _error!, onRetry: _load),
            if (rows == null && _error == null)
              const TwinsLoading(label: '기록을 받는 중', size: Look.twinsSmall),
            if (rows?.isEmpty == true)
              const Padding(
                padding: EdgeInsets.all(Look.pagePad),
                child: Text('대화 기록이 없어요.'),
              ),
            for (final row in rows ?? <Map<String, dynamic>>[])
              ListTile(
                title: Text(
                  '${row['label'] ?? row['character'] ?? row['harness']}',
                ),
                subtitle: Text('${row['harness']} · ${row['cwd'] ?? ''}'),
                trailing: _pending == row['id']
                    ? const SizedBox.square(
                        dimension: 20,
                        child: CircularProgressIndicator(strokeWidth: 2),
                      )
                    : const Icon(Icons.play_arrow_outlined),
                enabled: _pending == null,
                onTap: () => _resume(row),
              ),
          ],
        ),
      ),
    );
  }
}
