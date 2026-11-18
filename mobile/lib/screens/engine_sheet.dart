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

/// 바꾸기를 못 보내는 까닭. 일하는 중은 막지 않는다 — [showEngineSheet] 참고.
String? engineBlockedReason(Pane pane) {
  if (pane.needsYou) return '승인·질문 창에 먼저 답해 주세요.';
  if (pane.isCompacting) return '대화를 정리하는 중이라 끝나면 바꿀 수 있어요.';
  return null;
}

/// 대화 맥락 줄의 모델 칸을 눌렀을 때 — 모델과 effort 를 고르면 그 학생 칸에 슬래시 명령으로 보낸다.
///
/// 일하는 중에도 보낸다. Claude Code 2.1.327·Codex 0.169 실측에서 일하는 중 친 `/model`·`/effort` 는 다음 입력으로
/// 쌓이지 않고 바로 처리됐고 대화 기록에도 남지 않았다. Claude 는 캐시가 살아 있으면 「Switch model?」 확인 창을 띄우고,
/// 고르면 같은 턴의 다음 요청부터 새 모델이 받는다 — 그 창은 대화 보기의 선택지 카드로 답한다.
/// 승인·질문 창이 떠 있으면 막는다: 보내는 Enter 가 그 창의 지금 선택지를 고른다. 대화 정리 중은 확인하지 않았다.
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

  final blocked = engineBlockedReason(pane);
  if (blocked != null) {
    say(blocked);
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
