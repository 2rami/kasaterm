import 'package:flutter/material.dart';

import '../server.dart';

class SettingsScreen extends StatelessWidget {
  const SettingsScreen({
    super.key,
    required this.server,
    required this.onChangeAddress,
  });

  final Server server;
  final Future<void> Function() onChangeAddress;

  Future<void> _forget(BuildContext context) async {
    final ok = await showDialog<bool>(
      context: context,
      builder: (context) => AlertDialog(
        title: const Text('주소를 지울까'),
        content: const Text('이 폰에서 저장한 주소를 지운다. 데스크톱 허브에서 다시 복사해 넣으면 된다.'),
        actions: [
          TextButton(
            onPressed: () => Navigator.pop(context, false),
            child: const Text('취소'),
          ),
          FilledButton(
            onPressed: () => Navigator.pop(context, true),
            child: const Text('지우기'),
          ),
        ],
      ),
    );
    if (ok != true || !context.mounted) return;
    Navigator.of(context).popUntil((r) => r.isFirst);
    await onChangeAddress();
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return Scaffold(
      appBar: AppBar(title: const Text('설정')),
      body: ListView(
        padding: const EdgeInsets.all(12),
        children: [
          Card(
            child: Column(
              children: [
                ListTile(
                  leading: const Icon(Icons.link),
                  title: const Text('연결된 주소'),
                  subtitle: Text(server.describe()),
                ),
                const Divider(height: 1),
                ListTile(
                  leading: Icon(
                    Icons.delete_outline,
                    color: theme.colorScheme.error,
                  ),
                  title: const Text('주소 바꾸기 · 지우기'),
                  subtitle: const Text('지우면 연결 화면으로 돌아간다'),
                  onTap: () => _forget(context),
                ),
              ],
            ),
          ),
          const SizedBox(height: 16),
          Text(
            '테마는 시스템 설정(밝게/어둡게)을 따른다.',
            style: theme.textTheme.bodySmall?.copyWith(
              color: theme.colorScheme.onSurfaceVariant,
            ),
          ),
        ],
      ),
    );
  }
}
