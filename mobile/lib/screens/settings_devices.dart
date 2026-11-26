import 'package:flutter/material.dart';

import '../look.dart';
import '../relay_account.dart';
import '../server.dart';
import '../twins_loading.dart';
import 'approval_key.dart';
import 'browser_device.dart';
import 'controls.dart';
import 'desktop_tools.dart';
import 'dev_server.dart';
import 'device_names.dart';
import 'settings_glyph.dart';

/// 설정 「계정·기기」 → 「연결 기기」 — 데스크톱 설정 「연결 기기」의 차례(기기 → 폰 → 브라우저)를 따른다.
/// 폰에만 있는 줄(기기별 PC 기능·개발 서버)도 가장 가까운 이 페이지에 둔다.
class DeviceSettingsScreen extends StatelessWidget {
  const DeviceSettingsScreen({super.key, required this.server, this.api});

  final Server server;

  /// 계정 동기화 길 — 계정으로 붙었을 때만. 옛 주소 연결은 기기 이름·비밀 승인이 없다.
  final RelayAccountApi Function()? api;

  @override
  Widget build(BuildContext context) {
    final api = this.api;
    final groups = <(String, List<Widget> Function(int))>[
      ('기기', (tone) => _deviceRows(context, tone, api)),
      if (api != null) ('이 폰', (tone) => [_approvalRow(context, tone, api)]),
      ('브라우저', (tone) => _browserRows(context, tone)),
    ];
    return TwinBackdrop(
      child: Scaffold(
        backgroundColor: Colors.transparent,
        appBar: AppBar(backgroundColor: Colors.transparent, title: const Text('연결 기기')),
        body: ListView(
          padding: const EdgeInsets.fromLTRB(Look.pagePad, 0, Look.pagePad, Look.groupGap * 2),
          children: [
            SettingsOverview(
              art: const SettingsMark(SettingsGlyph.devices, size: 36),
              title: api == null ? '주소로 연결' : '계정으로 연결',
              detail: api == null ? server.root.host : null,
            ),
            for (final (i, (title, rows)) in groups.indexed) SettingsGroup(title: title, children: rows(i)),
          ],
        ),
      ),
    );
  }

  Future<void> _push(BuildContext context, Widget screen) =>
      Navigator.of(context).push(MaterialPageRoute<void>(builder: (_) => screen));

  List<Widget> _deviceRows(BuildContext context, int tone, RelayAccountApi Function()? api) => [
    if (api != null)
      SettingsRow(
        key: const Key('device-names'),
        tone: tone,
        icon: Icons.drive_file_rename_outline_rounded,
        title: '기기 이름',
        subtitle: '여기서 바꾼 이름이 모든 PC·폰 화면에 같이',
        chevron: true,
        onTap: () => _push(context, DeviceNamesScreen(server: server, api: api)),
      ),
    SettingsRow(
      key: const Key('desktop-tools'),
      tone: tone,
      icon: Icons.desktop_windows_outlined,
      title: '기기별 PC 기능',
      subtitle: 'PC 설정·대화 기록·화면 조작',
      chevron: true,
      onTap: () => chooseDesktopTools(context, server),
    ),
  ];

  /// 데스크톱 「연결 기기」의 「폰」 — 맥이 Face ID 허락을 물을 이 폰의 열쇠.
  Widget _approvalRow(BuildContext context, int tone, RelayAccountApi Function() api) => SettingsRow(
    key: const Key('approval-key'),
    tone: tone,
    icon: Icons.fingerprint_rounded,
    title: '비밀 승인',
    subtitle: '캐릭터가 맥 키체인 비밀을 쓸 때 Face ID 로 허락',
    chevron: true,
    onTap: () => _push(context, ApprovalKeyScreen(api: api, server: server)),
  );

  List<Widget> _browserRows(BuildContext context, int tone) => [
    SettingsRow(
      key: const Key('browser-device'),
      tone: tone,
      icon: Icons.public_outlined,
      title: '브라우저 기기',
      subtitle: '캐릭터가 보여 주려 여는 페이지가 갈 곳',
      chevron: true,
      onTap: () => showBrowserDeviceSheet(context, server: server),
    ),
    SettingsRow(
      key: const Key('dev-server'),
      tone: tone,
      icon: Icons.web_outlined,
      title: '개발 서버 열기',
      subtitle: '데스크톱 localhost 페이지를 이 앱 안에서',
      chevron: true,
      onTap: () => openDevServer(context, server: server),
    ),
  ];
}
