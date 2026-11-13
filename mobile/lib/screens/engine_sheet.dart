import 'package:flutter/material.dart';

import '../claude_style.dart' show claudeBrand;
import '../look.dart';
import '../server.dart';
import 'controls.dart' show effortColor;
import 'desktop_settings.dart' show claudeModelChoices;

/// Claude Code `/effort` 가 받는 단계. 데스크톱 상태줄 색표와 같은 다섯 칸이다.
const effortLevels = <(String, String)>[
  ('low', '낮음'),
  ('medium', '보통'),
  ('high', '높음'),
  ('xhigh', '매우 높음'),
  ('max', '최대'),
];

/// 고른 것을 학생 칸에 보낼 슬래시 명령. Codex 는 인자 없는 `/model` 이 모델·추론 깊이를 함께 고르는
/// 자체 창을 띄우므로 그 창(대화 선택 메뉴)으로 넘긴다.
String engineCommand({required String harness, String? model, String? effort}) {
  if (harness == 'codex') return '/model';
  if (model != null) return '/model $model';
  return '/effort $effort';
}

/// 대화 맥락 줄의 모델 칸을 눌렀을 때 — 모델과 effort 를 고르면 그 학생 칸에 슬래시 명령으로 보낸다.
/// 일하는 중에 보내면 명령이 다음 입력으로 쌓여 지금 턴과 섞이므로 끝난 뒤에만 연다.
Future<void> showEngineSheet(
  BuildContext context, {
  required Server server,
  required Pane pane,
}) async {
  final messenger = ScaffoldMessenger.of(context);
  void say(String text) {
    messenger
      ..hideCurrentSnackBar()
      ..showSnackBar(SnackBar(content: Text(text)));
  }

  if (pane.isBusy) {
    say('일하는 중이라 끝나면 바꿀 수 있어요.');
    return;
  }
  final harness = pane.harness == 'codex' ? 'codex' : 'claude';
  if (harness == 'codex') {
    try {
      await server.send(pane.id, engineCommand(harness: harness), machine: pane.machine);
      say('Codex 모델 고르기 창을 열었어요.');
    } on ServerException catch (e) {
      say(e.message);
    }
    return;
  }

  var models = claudeModelChoices(null);
  try {
    final values = await server.desktopValues('settings/values', machine: pane.machine);
    final claude = values['claude'];
    models = claudeModelChoices(claude is Map ? claude['model_choices'] : null);
  } catch (_) {
    // 목록을 못 받아도 기본 별칭(opus·sonnet·haiku)으로 고를 수 있다.
  }
  models = [for (final m in models) if (m.$1.isNotEmpty) m];
  if (!context.mounted) return;

  final picked = await showModalBottomSheet<(String?, String?)>(
    context: context,
    showDragHandle: true,
    isScrollControlled: true,
    builder: (ctx) => _EngineSheet(pane: pane, models: models),
  );
  if (picked == null) return;
  final (model, effort) = picked;
  try {
    await server.send(
      pane.id,
      engineCommand(harness: harness, model: model, effort: effort),
      machine: pane.machine,
    );
    say(model != null ? '모델을 바꾸라고 보냈어요.' : 'effort 를 바꾸라고 보냈어요.');
  } on ServerException catch (e) {
    say(e.message);
  }
}

class _EngineSheet extends StatelessWidget {
  const _EngineSheet({required this.pane, required this.models});

  final Pane pane;
  final List<(String, String)> models;

  bool _isCurrentModel((String, String) m) {
    final raw = (pane.model ?? '').toLowerCase();
    final label = (pane.modelLabel ?? '').toLowerCase();
    final value = m.$1.toLowerCase();
    return raw == value || (label.isNotEmpty && label == m.$2.toLowerCase());
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final current = (pane.effortLabel ?? pane.effort ?? '').toLowerCase();
    return SafeArea(
      child: ConstrainedBox(
        constraints: BoxConstraints(maxHeight: MediaQuery.sizeOf(context).height * .8),
        child: ListView(
          shrinkWrap: true,
          padding: const EdgeInsets.only(bottom: Look.pagePad),
          children: [
            ListTile(
              title: Text('모델', style: theme.textTheme.titleMedium),
              subtitle: Text(pane.modelLabel ?? pane.model ?? '모름'),
            ),
            for (final m in models)
              ListTile(
                minTileHeight: Look.tap,
                leading: Image.asset('assets/icons/claude.png', width: 18, height: 18,
                    color: claudeBrand, colorBlendMode: BlendMode.srcIn),
                title: Text(m.$2),
                trailing: _isCurrentModel(m) ? Icon(Icons.check, color: theme.colorScheme.primary) : null,
                onTap: () => Navigator.of(context).pop((m.$1, null)),
              ),
            const Divider(),
            ListTile(title: Text('effort', style: theme.textTheme.titleMedium)),
            Padding(
              padding: const EdgeInsets.symmetric(horizontal: Look.pagePad),
              child: Wrap(
                spacing: 8,
                runSpacing: 8,
                children: [
                  for (final (level, label) in effortLevels)
                    ChoiceChip(
                      label: Text('$label · $level'),
                      selected: current == level,
                      avatar: CircleAvatar(radius: 5, backgroundColor: effortColor(level)),
                      onSelected: (_) => Navigator.of(context).pop((null, level)),
                    ),
                ],
              ),
            ),
          ],
        ),
      ),
    );
  }
}
