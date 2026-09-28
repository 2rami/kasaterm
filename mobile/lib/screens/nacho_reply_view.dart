import 'package:flutter/material.dart';
import 'package:flutter_markdown_plus/flutter_markdown_plus.dart';

import '../chat_markdown.dart';
import '../nacho.dart';
import '../nacho_reply.dart';
import '../nacho_student.dart';
import '../server.dart';
import '../student_art.dart';

/// 본문 — 나쵸 답의 마크다운(목록·제목·인용·표·코드·링크·굵게). 링크는 라벨만 보이고 누르면 연다.
class ReplyText extends StatelessWidget {
  const ReplyText({
    super.key,
    required this.text,
    required this.onLink,
    this.style,
    this.selectable = true,
  });

  final String text;
  final ValueChanged<String> onLink;
  final TextStyle? style;

  /// 알림 줄처럼 줄 전체가 눌리는 자리는 끈다 — 선택 영역이 그 탭을 가져간다.
  final bool selectable;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final body = MarkdownBody(
      data: guardMarkdown(text),
      styleSheet: chatMarkdownStyle(
        theme,
        base: DefaultTextStyle.of(context).style.merge(style),
        // 나쵸 글이 놓이는 말풍선·알림 줄·작업 상자는 모두 surfaceContainer 층이라 코드는 한 층 아래로 판다.
        codeBg: theme.colorScheme.surface,
      ),
      extensionSet: nachoMarkdown,
      inlineSyntaxes: nachoInlineSyntaxes,
      softLineBreak: true,
      onTapLink: (_, href, _) {
        if (href != null) onLink(href);
      },
    );
    return selectable ? SelectionArea(child: body) : body;
  }
}

/// 말풍선 밑 잔글씨 한 줄 — 걸린 시간·턴·모델.
class ReplyMetaLine extends StatelessWidget {
  const ReplyMetaLine({super.key, required this.meta});

  final ReplyMeta meta;

  @override
  Widget build(BuildContext context) => Padding(
    padding: const EdgeInsets.only(top: 3),
    child: Text(
      meta.brief,
      maxLines: 1,
      overflow: TextOverflow.ellipsis,
      style: TextStyle(
        fontSize: 11.5,
        color: Theme.of(context).colorScheme.onSurfaceVariant,
      ),
    ),
  );
}

/// 접힌 상세 — 옮긴 실행 명령·진단 줄, 사용량 전부, 그리고 원문. 여기서 빠지는 글자는 없다.
class ReplyDetails extends StatefulWidget {
  const ReplyDetails({super.key, required this.view});

  final ReplyView view;

  @override
  State<ReplyDetails> createState() => _ReplyDetailsState();
}

class _ReplyDetailsState extends State<ReplyDetails> {
  bool _open = false;
  bool _raw = false;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final v = widget.view;
    final dim = TextStyle(fontSize: 12, color: scheme.onSurfaceVariant, height: 1.35);
    final mono = dim.copyWith(fontFamily: 'TermMono', fontFamilyFallback: const ['TermHangul']);
    final label = v.details.isNotEmpty ? '실행 명령·진단' : '응답 정보';
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        InkWell(
          borderRadius: BorderRadius.circular(8),
          onTap: () => setState(() => _open = !_open),
          child: Padding(
            padding: const EdgeInsets.symmetric(vertical: 4),
            child: Row(
              mainAxisSize: MainAxisSize.min,
              children: [
                Icon(
                  _open ? Icons.expand_less_rounded : Icons.expand_more_rounded,
                  size: 18,
                  color: scheme.onSurfaceVariant,
                ),
                const SizedBox(width: 2),
                Text(label, style: dim),
              ],
            ),
          ),
        ),
        if (_open)
          Container(
            width: double.infinity,
            margin: const EdgeInsets.only(top: 2),
            padding: const EdgeInsets.all(10),
            decoration: BoxDecoration(
              border: Border.all(color: scheme.outlineVariant),
              borderRadius: BorderRadius.circular(10),
            ),
            child: SelectionArea(
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  for (final d in v.details)
                    Padding(
                      padding: const EdgeInsets.only(bottom: 6),
                      child: Text(d, style: mono),
                    ),
                  if (v.meta != null)
                    Padding(
                      padding: const EdgeInsets.only(bottom: 6),
                      child: Text(v.meta!.parts.join(' · '), style: dim),
                    ),
                  GestureDetector(
                    onTap: () => setState(() => _raw = !_raw),
                    child: Text(
                      _raw ? '원문 접기' : '원문 보기',
                      style: dim.copyWith(decoration: TextDecoration.underline),
                    ),
                  ),
                  if (_raw) ...[
                    const SizedBox(height: 4),
                    Text(v.original, style: mono),
                  ],
                ],
              ),
            ),
          ),
      ],
    );
  }
}

/// 작업 장부의 맡은 학생 — 이름·기기·지금 상태와 그 학생 화면을 앱 안에서 여는 단추.
class StudentWorkCard extends StatelessWidget {
  const StudentWorkCard({
    super.key,
    required this.work,
    required this.lookup,
    required this.onOpenPane,
    this.onOpenTask,
  });

  final NachoTaskCard work;
  final StudentLookup lookup;
  final void Function(Pane pane) onOpenPane;

  /// 작업 상세로 — 상세 화면 안에서는 없다.
  final ValueChanged<String>? onOpenTask;

  @override
  Widget build(BuildContext context) => ListenableBuilder(
    listenable: lookup,
    builder: (context, _) {
      final scheme = Theme.of(context).colorScheme;
      final student = work.student ?? const {};
      final seat = lookup.seat(student);
      final pane = seat?.pane;
      final surface = student['surface'] as String? ?? '';
      final name = pane == null ? '학생 $surface' : pane.displayName;
      final live = switch (seat) {
        null => '확인 중',
        StudentSeat(placed: false) => '어느 기기인지 확인 안 됨',
        StudentSeat(pane: final p?) => p.kindLabel,
        StudentSeat(unreachable: true) => '기기에 닿지 못함',
        StudentSeat(read: false) => '확인 중',
        _ => '창이 닫혔거나 목록에 없음',
      };
      final step = [
        work.stateLabel,
        if (work.step.isNotEmpty) work.step,
      ].join(' · ');
      return Container(
        margin: const EdgeInsets.only(top: 6),
        padding: const EdgeInsets.fromLTRB(12, 10, 8, 6),
        decoration: BoxDecoration(
          border: Border.all(color: scheme.outlineVariant),
          borderRadius: BorderRadius.circular(12),
        ),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Row(
              children: [
                StudentFace(
                  server: lookup.server,
                  slug: pane?.slug,
                  url: pane?.slug == null
                      ? null
                      : lookup.server.avatar(pane!.slug!, machine: pane.machine),
                  shell: pane?.isShell ?? false,
                  size: 34,
                ),
                const SizedBox(width: 10),
                Expanded(
                  child: Column(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    children: [
                      Text(
                        name,
                        maxLines: 1,
                        overflow: TextOverflow.ellipsis,
                        style: const TextStyle(fontSize: 15, fontWeight: FontWeight.w700),
                      ),
                      Text(
                        [seat?.machine ?? '', live].where((s) => s.isNotEmpty).join(' · '),
                        maxLines: 1,
                        overflow: TextOverflow.ellipsis,
                        style: TextStyle(
                          fontSize: 12.5,
                          color: pane?.isWaiting == true ? scheme.error : scheme.onSurfaceVariant,
                        ),
                      ),
                    ],
                  ),
                ),
              ],
            ),
            if (step.isNotEmpty)
              Padding(
                padding: const EdgeInsets.only(top: 6),
                child: Text(
                  step,
                  maxLines: 2,
                  overflow: TextOverflow.ellipsis,
                  style: TextStyle(fontSize: 12.5, color: scheme.onSurfaceVariant),
                ),
              ),
            Row(
              mainAxisAlignment: MainAxisAlignment.end,
              children: [
                if (onOpenTask != null)
                  TextButton(
                    onPressed: () => onOpenTask!(work.id),
                    child: const Text('작업 상세'),
                  ),
                const SizedBox(width: 4),
                OutlinedButton.icon(
                  onPressed: pane == null ? null : () => onOpenPane(pane),
                  icon: const Icon(Icons.terminal_rounded, size: 18),
                  label: const Text('작업 열기'),
                ),
              ],
            ),
          ],
        ),
      );
    },
  );
}
