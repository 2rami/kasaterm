import 'package:flutter/material.dart';

import '../status_style.dart';
import '../work_mode.dart';
import '../workboard.dart';

/// 작업 모드·권한 시트. 모드는 여기서 사람이 고를 때만 나쵸에 적고, 권한 표는 나쵸 기능 안내를
/// 그대로 보인다 — PC 작업 탭과 같은 정본·같은 말.
class WorkModeSheet extends StatelessWidget {
  const WorkModeSheet({super.key, required this.desk});

  final WorkModeDesk desk;

  @override
  Widget build(BuildContext context) => DraggableScrollableSheet(
    expand: false,
    initialChildSize: 0.9,
    minChildSize: 0.5,
    maxChildSize: 0.95,
    builder: (context, controller) => ListenableBuilder(
      listenable: desk,
      builder: (context, _) => ListView(
        controller: controller,
        padding: const EdgeInsets.fromLTRB(20, 0, 20, 24),
        children: [..._mode(context), ..._permissions(context)],
      ),
    ),
  );

  List<Widget> _mode(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final state = desk.state;
    final block = desk.writeBlock;
    final dim = TextStyle(
      fontSize: 13,
      color: scheme.onSurfaceVariant,
      height: 1.45,
    );
    return [
      Row(
        children: [
          Text('작업 모드', style: theme.textTheme.titleLarge),
          if (desk.demo) ...[
            const SizedBox(width: 8),
            Text(
              '예시',
              style: TextStyle(fontSize: 13, color: scheme.onSurfaceVariant),
            ),
          ],
        ],
      ),
      const SizedBox(height: 12),
      SegmentedButton<WorkMode>(
        segments: [
          for (final m in WorkMode.values)
            ButtonSegment(value: m, label: Text(desk.labelOf(m))),
        ],
        selected: {?state?.mode},
        emptySelectionAllowed: true,
        showSelectedIcon: false,
        style: SegmentedButton.styleFrom(minimumSize: const Size(0, 48)),
        onSelectionChanged: block != null
            ? null
            : (picked) {
                if (picked.isNotEmpty) desk.choose(picked.first);
              },
      ),
      if (desk.writing)
        const Padding(
          padding: EdgeInsets.only(top: 8),
          child: LinearProgressIndicator(minHeight: 2),
        ),
      if (block != null && !desk.writing)
        Padding(
          padding: const EdgeInsets.only(top: 8),
          child: Text(block, style: dim),
        ),
      if (desk.notice != null)
        Padding(
          padding: const EdgeInsets.only(top: 8),
          child: Text(
            desk.notice!,
            style: dim.copyWith(color: StatusStyle.attentionInk),
          ),
        ),
      if (state != null)
        Padding(
          padding: const EdgeInsets.only(top: 8),
          child: Text(_origin(state), style: dim),
        ),
      const SizedBox(height: 14),
      for (final m in WorkMode.values)
        _ModeCard(desk: desk, mode: m, on: state?.mode == m),
      Padding(
        padding: const EdgeInsets.only(top: 4),
        child: Text('모드를 바꿔도 권한이 늘거나 도는 일이 멈추지 않아요.', style: dim),
      ),
    ];
  }

  String _origin(ModeState s) {
    if (s.changedAtMs == null) {
      return '나쵸 기본값 — 아직 아무도 바꾸지 않았어요';
    }
    final by = s.changedBy == null ? '' : ' · ${s.changedBy}';
    return '나쵸가 적은 판 ${s.rev}$by · ${freshLabel(s.changedAtMs)}';
  }

  List<Widget> _permissions(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final caps = desk.caps;
    final dim = TextStyle(
      fontSize: 13,
      color: scheme.onSurfaceVariant,
      height: 1.45,
    );
    final out = <Widget>[
      const Divider(height: 36),
      Text('권한', style: theme.textTheme.titleMedium),
      const SizedBox(height: 4),
    ];
    if (caps == null) {
      out.add(Text(desk.capsProblem ?? '나쵸에 묻는 중', style: dim));
      return out;
    }
    for (final tier in caps.tiers) {
      out
        ..add(const SizedBox(height: 12))
        ..add(
          Text(
            tier.label,
            style: const TextStyle(fontSize: 14, fontWeight: FontWeight.w700),
          ),
        )
        ..add(
          Padding(
            padding: const EdgeInsets.only(top: 2, bottom: 4),
            child: Text(tier.rule, style: dim),
          ),
        );
      for (final a in tier.actions) {
        out.add(_ActionLine(action: a));
      }
    }
    final approvals = caps.approvals;
    out
      ..add(const SizedBox(height: 12))
      ..add(
        _Fact(
          label: '무제한 모드',
          value: switch (caps.unlimitedMode) {
            false => '없어요 — 확인 없이 모든 도구를 쓰는 모드는 만들지 않아요',
            true => '나쵸가 있다고 답했어요 — 이 앱은 그 모드를 따르지 않아요',
            null => '나쵸가 알려 주지 않았어요',
          },
          warn: caps.unlimitedMode == true,
        ),
      )
      ..add(
        _Fact(
          label: '앱 안 승인',
          value: approvals == null
              ? '나쵸가 알려 주지 않았어요'
              : approvals.decideInApp
              ? '나쵸는 받는다고 해요 — 이 앱은 범위·만료·1회용을 확인하기 전까지 허용 단추를 켜지 않아요'
              : '지원 안 해요 — 결정은 주인 DM 의 확인 단추로만. 승인 시트의 허용 단추도 꺼 둬요',
        ),
      )
      ..add(
        _Fact(
          label: '승인 창구',
          value: approvals == null
              ? '나쵸가 알려 주지 않았어요'
              : approvals.enabled
              ? '켜짐${approvals.httpActions.isEmpty ? '' : ' · 받는 동작 ${approvals.httpActions.map(caps.actionLabel).join(', ')}'}'
              : '꺼짐 — 나쵸가 승인을 받지 않아요',
        ),
      );
    if (caps.notes.isNotEmpty) {
      out.add(
        _Fact(label: '나쵸 안내', value: caps.notes.map((n) => '· $n').join('\n')),
      );
    }
    return out;
  }
}

class _ModeCard extends StatelessWidget {
  const _ModeCard({required this.desk, required this.mode, required this.on});

  final WorkModeDesk desk;
  final WorkMode mode;
  final bool on;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final info = desk.caps?.info(mode);
    final summary = (info?.summary ?? '').isEmpty ? mode.blurb : info!.summary;
    final dim = TextStyle(
      fontSize: 13,
      color: scheme.onSurfaceVariant,
      height: 1.45,
    );
    return Container(
      margin: const EdgeInsets.only(bottom: 10),
      padding: const EdgeInsets.fromLTRB(14, 12, 14, 12),
      decoration: BoxDecoration(
        borderRadius: BorderRadius.circular(12),
        border: Border.all(
          color: on ? scheme.primary : scheme.outlineVariant,
          width: on ? 1.6 : 1,
        ),
      ),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Row(
            children: [
              Text(
                desk.labelOf(mode),
                style: TextStyle(
                  fontSize: 15,
                  fontWeight: FontWeight.w700,
                  color: on ? scheme.primary : scheme.onSurface,
                ),
              ),
              if (on) ...[
                const SizedBox(width: 8),
                Text(
                  '지금',
                  style: TextStyle(fontSize: 13, color: scheme.primary),
                ),
              ],
            ],
          ),
          const SizedBox(height: 4),
          Text(summary, style: const TextStyle(fontSize: 14, height: 1.45)),
          for (final e in info?.effects ?? const <String>[])
            Padding(
              padding: const EdgeInsets.only(top: 2),
              child: Text('· $e', style: dim),
            ),
        ],
      ),
    );
  }
}

class _ActionLine extends StatelessWidget {
  const _ActionLine({required this.action});

  final TierAction action;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final asks = action.how.isNotEmpty && action.how != 'none';
    return ConstrainedBox(
      constraints: const BoxConstraints(minHeight: 32),
      child: Row(
        children: [
          Expanded(
            child: Text(
              action.label.isEmpty ? action.id : action.label,
              style: const TextStyle(fontSize: 14),
            ),
          ),
          const SizedBox(width: 12),
          Text(
            action.howWord,
            style: TextStyle(
              fontSize: 13,
              color: asks ? StatusStyle.attentionInk : scheme.onSurfaceVariant,
              fontWeight: asks ? FontWeight.w600 : FontWeight.w400,
            ),
          ),
        ],
      ),
    );
  }
}

class _Fact extends StatelessWidget {
  const _Fact({required this.label, required this.value, this.warn = false});

  final String label;
  final String value;
  final bool warn;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    return Padding(
      padding: const EdgeInsets.only(bottom: 10),
      child: Row(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          SizedBox(
            width: 76,
            child: Text(
              label,
              style: TextStyle(fontSize: 13, color: scheme.onSurfaceVariant),
            ),
          ),
          Expanded(
            child: Text(
              value,
              style: TextStyle(
                fontSize: 14,
                height: 1.45,
                color: warn ? scheme.error : scheme.onSurface,
              ),
            ),
          ),
        ],
      ),
    );
  }
}
