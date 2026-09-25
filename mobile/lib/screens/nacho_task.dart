import 'dart:async';

import 'package:flutter/material.dart';
import 'package:url_launcher/url_launcher.dart';

import '../nacho.dart';
import '../nacho_reply.dart';
import '../nacho_student.dart';
import '../server.dart';
import 'nacho_home.dart';
import 'nacho_reply_view.dart';

/// 작업 하나 — 목표·진행·결과·근거·승인 상태, 그리고 이 일에 주는 방향.
///
/// 보이는 것은 전부 나쵸 장부에 적힌 그대로다. 검증이 안 적혔으면 「검증 기록 없음」, 사진이
/// 없으면 사진 칸이 없다 — 없는 것을 채워 넣으면 그게 곧 거짓 완료다.
class NachoTaskScreen extends StatefulWidget {
  const NachoTaskScreen({
    super.key,
    required this.desk,
    required this.taskId,
    required this.onOpenStudents,
    this.students,
    this.onOpenPane,
    this.onLink,
  });

  final NachoDesk desk;
  final String taskId;
  final VoidCallback onOpenStudents;

  /// 맡은 학생을 실제 pane 으로 이어 그 화면을 곧장 연다. 없으면 학생 목록으로 보낸다.
  final StudentLookup? students;
  final void Function(Pane pane)? onOpenPane;
  final ValueChanged<String>? onLink;

  @override
  State<NachoTaskScreen> createState() => _NachoTaskScreenState();
}

class _NachoTaskScreenState extends State<NachoTaskScreen> {
  NachoTaskDetail? _task;
  String? _problem;
  bool _sending = false;
  int _seenSeq = 0;
  final _input = TextEditingController();

  @override
  void initState() {
    super.initState();
    _seenSeq = widget.desk.lastSeq;
    widget.desk.addListener(_onDesk);
    unawaited(_load());
  }

  @override
  void dispose() {
    widget.desk.removeListener(_onDesk);
    _input.dispose();
    super.dispose();
  }

  /// 이 일에 새 줄(답·소식·접수 상태)이 오면 다시 읽는다 — 앱을 옮겨 다니며 찾지 않게.
  void _onDesk() {
    final fresh = widget.desk.events.where(
      (e) => e.seq > _seenSeq && (e.task == widget.taskId),
    );
    _seenSeq = widget.desk.lastSeq;
    if (fresh.isNotEmpty) unawaited(_load());
  }

  Future<void> _load() async {
    try {
      final t = await widget.desk.task(widget.taskId);
      if (!mounted) return;
      setState(() {
        _task = t;
        _problem = null;
      });
    } on NachoError catch (e) {
      if (mounted) setState(() => _problem = e.message);
    } catch (e) {
      if (mounted) setState(() => _problem = '$e');
    }
  }

  Future<void> _send() async {
    final t = _task;
    final text = _input.text.trim();
    if (t == null || text.isEmpty || _sending) return;
    setState(() => _sending = true);
    try {
      await widget.desk.send(text, task: t.id, rev: t.rev);
      _input.clear();
      unawaited(_load());
    } on NachoError catch (e) {
      if (!mounted) return;
      ScaffoldMessenger.of(context).showSnackBar(SnackBar(content: Text(e.message)));
      if (e.code == 'stale_rev') unawaited(_load());
    } finally {
      if (mounted) setState(() => _sending = false);
    }
  }

  @override
  Widget build(BuildContext context) {
    final t = _task;
    return Scaffold(
      appBar: AppBar(title: const Text('작업')),
      body: t == null
          ? Center(
              child: Padding(
                padding: const EdgeInsets.all(24),
                child: Text(_problem ?? '불러오는 중', textAlign: TextAlign.center),
              ),
            )
          : Column(
              children: [
                Expanded(
                  child: RefreshIndicator(
                    onRefresh: _load,
                    child: ListView(
                      padding: const EdgeInsets.fromLTRB(16, 12, 16, 24),
                      children: _body(context, t),
                    ),
                  ),
                ),
                if (t.canDirect)
                  nachoComposer(
                    controller: _input,
                    hint: '이 작업에 방향 주기',
                    sending: _sending,
                    onSend: _send,
                  )
                else
                  _Footnote(
                    text: t.group == 'closed'
                        ? '닫힌 작업이에요. 이어 하려면 대화에서 새로 말해 주세요.'
                        : '이 작업은 지금 ${t.place}에 있어요. 그쪽에서 말하거나 대화에서 「디코에서 하던 거 여기로 이어서」처럼 이어받아 주세요.',
                  ),
              ],
            ),
    );
  }

  List<Widget> _body(BuildContext context, NachoTaskDetail t) {
    final scheme = Theme.of(context).colorScheme;
    final approval = t.approval;
    final report = t.report;
    final verify = t.verify;
    final student = t.student;
    final shots = [
      for (final e in t.events)
        if (e.kind == 'reply')
          for (var i = 0; i < e.files.length; i++)
            if (_image(e.files[i])) (e.seq, i, e.files[i]),
    ];
    return [
      SelectableText(t.request, style: const TextStyle(fontSize: 17, fontWeight: FontWeight.w600, height: 1.35)),
      const SizedBox(height: 8),
      Wrap(
        spacing: 6,
        runSpacing: 4,
        crossAxisAlignment: WrapCrossAlignment.center,
        children: [
          nachoPill(t.stateLabel, strong: t.group == 'attention'),
          nachoPill(t.project),
          nachoPill(t.place),
          Text(agoLabel(t.updatedMs), style: TextStyle(fontSize: 12, color: scheme.onSurfaceVariant)),
        ],
      ),
      if (approval != null)
        _Box(
          warn: true,
          title: '승인 대기',
          children: [
            if ((approval['what'] as String? ?? '').isNotEmpty) SelectableText(approval['what'] as String),
            const SizedBox(height: 6),
            Text(approval['note'] as String? ?? '', style: const TextStyle(fontSize: 13)),
          ],
        ),
      if (t.blocked.isNotEmpty) _Box(warn: true, title: '막힘', children: [SelectableText(t.blocked)]),
      _Box(
        title: '진행',
        children: [
          if (t.step.isNotEmpty) SelectableText(t.step),
          for (final r in t.remaining) Text('· $r', style: TextStyle(color: scheme.onSurfaceVariant)),
        ],
      ),
      _Box(
        title: '결과',
        children: [
          if (t.result.isNotEmpty) SelectableText(t.result),
          Text(
            verify == null
                ? '검증 기록 없음'
                : '검증 ${verify['ok'] == true ? '통과' : '실패'} · ${verify['note'] ?? ''}',
            style: TextStyle(color: scheme.onSurfaceVariant),
          ),
          if (report != null) ...[
            const SizedBox(height: 8),
            Text(
              '학생 보고(${report['status'] ?? ''}${(report['character'] as String? ?? '').isEmpty ? '' : ' · ${report['character']}'})',
              style: const TextStyle(fontWeight: FontWeight.w600),
            ),
            SelectableText(report['summary'] as String? ?? ''),
            if ((report['changed'] as List? ?? const []).isNotEmpty)
              Text('바꾼 파일: ${(report['changed'] as List).join(', ')}', style: const TextStyle(fontSize: 13)),
            if ((report['tests'] as String? ?? '').isNotEmpty)
              Text('검사: ${report['tests']}', style: const TextStyle(fontSize: 13)),
          ],
        ],
      ),
      if (t.previewUrl != null || t.hasShot || shots.isNotEmpty)
        _Box(
          title: '미리보기',
          children: [
            if (t.previewUrl != null)
              TextButton.icon(
                onPressed: () => launchUrl(Uri.parse(t.previewUrl!), mode: LaunchMode.externalApplication),
                icon: const Icon(Icons.open_in_new_rounded, size: 18),
                label: Text(t.previewUrl!, overflow: TextOverflow.ellipsis),
              ),
            if (t.hasShot) _Shot(url: widget.desk.shotUri(t.id).toString(), label: '웹 결과 사진'),
            for (final (seq, i, name) in shots)
              _Shot(url: widget.desk.fileUri(seq, i, task: t.id).toString(), label: name),
          ],
        ),
      if (student != null && widget.students != null && widget.onOpenPane != null)
        _Box(
          title: '맡은 학생',
          children: [
            StudentWorkCard(work: t, lookup: widget.students!, onOpenPane: widget.onOpenPane!),
          ],
        )
      else if (student != null)
        _Box(
          title: '맡은 학생',
          children: [
            Text('${student['host'] ?? ''} ${student['surface'] ?? ''}'.trim()),
            TextButton.icon(
              onPressed: widget.onOpenStudents,
              icon: const Icon(Icons.terminal_rounded, size: 18),
              label: const Text('학생 화면 열기'),
            ),
          ],
        ),
      if (t.history.isNotEmpty || t.hops.isNotEmpty)
        _Box(
          title: '근거',
          children: [
            for (final h in t.history)
              Text(
                '${_clock(h['at_ms'])} ${h['label'] ?? h['state'] ?? ''} · ${h['why'] ?? ''}',
                style: const TextStyle(fontSize: 13),
              ),
            for (final h in t.hops)
              Text(
                '${_clock(h['at_ms'])} 창구 이동 ${h['from'] ?? ''} → ${h['to'] ?? ''} ${h['why'] ?? ''}',
                style: const TextStyle(fontSize: 13),
              ),
          ],
        ),
      _Box(
        title: '이 작업의 대화',
        children: [
          for (final e in t.events)
            if (e.kind == 'reply')
              _ReplyEntry(event: e, root: widget.desk.server.root, onLink: widget.onLink)
            else if (e.kind == 'message' || e.kind == 'notice')
              Padding(
                padding: const EdgeInsets.only(bottom: 6),
                child: Text(
                  '${e.kind == 'message' ? '나' : '알림'} · '
                  '${e.kind == 'message' ? receiptLabel(widget.desk.stateOf(e.id ?? '')) : _clock(e.atMs)}\n${e.text}',
                  style: const TextStyle(fontSize: 13, height: 1.35),
                ),
              ),
          if (!t.events.any((e) => e.kind == 'message' || e.kind == 'reply' || e.kind == 'notice'))
            Text('아직 이 작업에 오간 말이 없어요.', style: TextStyle(color: scheme.onSurfaceVariant)),
        ],
      ),
    ];
  }
}

/// 작업 대화 속 나쵸 답 — 대화 탭과 같이 링크는 라벨로, 실행 명령·사용량은 접힌 상세로.
class _ReplyEntry extends StatelessWidget {
  const _ReplyEntry({required this.event, required this.root, this.onLink});

  final NachoEvent event;
  final Uri root;
  final ValueChanged<String>? onLink;

  @override
  Widget build(BuildContext context) {
    final view = splitReply(event.text, root: root);
    const style = TextStyle(fontSize: 13, height: 1.35);
    return Padding(
      padding: const EdgeInsets.only(bottom: 6),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Text('나쵸 · ${_clock(event.atMs)}', style: style),
          if (view.body.isNotEmpty)
            ReplyText(
              text: view.body,
              onLink: onLink ?? (url) => launchUrl(externalUri(url) ?? Uri(), mode: LaunchMode.externalApplication),
              style: style,
            ),
          if (view.meta != null) ReplyMetaLine(meta: view.meta!),
          if (view.folded) ReplyDetails(view: view),
        ],
      ),
    );
  }
}

bool _image(String name) {
  final n = name.toLowerCase();
  return n.endsWith('.png') || n.endsWith('.jpg') || n.endsWith('.jpeg') || n.endsWith('.gif') || n.endsWith('.webp');
}

String _clock(Object? ms) {
  final v = (ms as num?)?.toInt() ?? 0;
  if (v <= 0) return '';
  final d = DateTime.fromMillisecondsSinceEpoch(v);
  String two(int x) => x.toString().padLeft(2, '0');
  return '${two(d.month)}-${two(d.day)} ${two(d.hour)}:${two(d.minute)}';
}

class _Box extends StatelessWidget {
  const _Box({required this.title, required this.children, this.warn = false});

  final String title;
  final List<Widget> children;
  final bool warn;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    return Container(
      margin: const EdgeInsets.only(top: 12),
      padding: const EdgeInsets.all(14),
      decoration: BoxDecoration(
        color: warn ? scheme.errorContainer : scheme.surfaceContainerHigh,
        borderRadius: BorderRadius.circular(12),
      ),
      child: DefaultTextStyle.merge(
        style: TextStyle(color: warn ? scheme.onErrorContainer : scheme.onSurface, fontSize: 14, height: 1.35),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Semantics(
              header: true,
              child: Text(title, style: const TextStyle(fontWeight: FontWeight.w700)),
            ),
            const SizedBox(height: 6),
            ...children,
          ],
        ),
      ),
    );
  }
}

class _Shot extends StatelessWidget {
  const _Shot({required this.url, required this.label});

  final String url;
  final String label;

  @override
  Widget build(BuildContext context) => Padding(
    padding: const EdgeInsets.only(top: 8),
    child: ClipRRect(
      borderRadius: BorderRadius.circular(10),
      child: Image.network(
        url,
        fit: BoxFit.fitWidth,
        semanticLabel: label,
        errorBuilder: (_, _, _) => Text('$label — 지금은 불러오지 못했다', style: const TextStyle(fontSize: 13)),
      ),
    ),
  );
}

class _Footnote extends StatelessWidget {
  const _Footnote({required this.text});

  final String text;

  @override
  Widget build(BuildContext context) => SafeArea(
    top: false,
    child: Padding(
      padding: const EdgeInsets.all(16),
      child: Text(
        text,
        style: TextStyle(fontSize: 13, color: Theme.of(context).colorScheme.onSurfaceVariant),
      ),
    ),
  );
}
