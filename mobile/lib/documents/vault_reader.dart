import 'package:flutter/material.dart';
import 'package:flutter_markdown_plus/flutter_markdown_plus.dart';

import '../chat_markdown.dart';
import '../look.dart';
import '../screens/controls.dart';
import '../twins_loading.dart';
import 'vault_api.dart';
import 'vault_quiz.dart';
import 'vault_study.dart';
import 'vault_widgets.dart';

class VaultReader extends StatefulWidget {
  const VaultReader({
    super.key,
    required this.api,
    required this.path,
    required this.studyScope,
    required this.onChanged,
  });

  final VaultApi api;
  final String path;
  final String studyScope;
  final VoidCallback onChanged;

  @override
  State<VaultReader> createState() => _VaultReaderState();
}

class _VaultReaderState extends State<VaultReader> {
  late Future<VaultFile> _loading;
  VaultFile? _file;
  VaultMarkdown? _markdown;
  bool _generating = false;

  @override
  void initState() {
    super.initState();
    _loading = widget.api.file(widget.path);
  }

  void _use(VaultFile file) {
    _file = file;
    _markdown = VaultMarkdown(file.text);
  }

  Future<void> _edit() async {
    final file = await Navigator.of(context).push<VaultFile>(
      MaterialPageRoute(
        builder: (_) => VaultEditor(api: widget.api, file: _file),
      ),
    );
    if (mounted && file != null) {
      setState(() => _use(file));
      widget.onChanged();
    }
  }

  void _review() {
    Navigator.of(context).push<void>(
      MaterialPageRoute(
        builder: (_) => VaultQuiz(
          path: _file!.path,
          cards: _markdown!.cards,
          store: VaultStudyStore(widget.studyScope),
        ),
      ),
    );
  }

  Future<void> _generate(VaultQuizKind kind) async {
    setState(() => _generating = true);
    try {
      final path = await widget.api.createQuiz(_file!.path, kind);
      if (!mounted) return;
      widget.onChanged();
      await Navigator.of(context).push<void>(
        MaterialPageRoute(
          builder: (_) => VaultReader(
            api: widget.api,
            path: path,
            studyScope: widget.studyScope,
            onChanged: widget.onChanged,
          ),
        ),
      );
    } catch (error) {
      if (mounted) {
        ScaffoldMessenger.of(
          context,
        ).showSnackBar(SnackBar(content: Text(vaultError(error))));
      }
    } finally {
      if (mounted) setState(() => _generating = false);
    }
  }

  String? _relativeMarkdown(String? href) {
    if (href == null || href.isEmpty) return null;
    final uri = Uri.tryParse(href);
    if (uri == null ||
        uri.hasScheme ||
        uri.hasAuthority ||
        uri.hasQuery ||
        uri.path.isEmpty ||
        uri.path.startsWith('/')) {
      return null;
    }
    final parts = _file!.path.split('/')..removeLast();
    for (final part in uri.pathSegments) {
      if (part.isEmpty || part == '.') continue;
      if (part == '..') {
        if (parts.isEmpty) return null;
        parts.removeLast();
      } else {
        parts.add(part);
      }
    }
    final path = parts.join('/');
    return path.endsWith('.md') && path != _file!.path ? path : null;
  }

  @override
  Widget build(BuildContext context) => FutureBuilder(
    future: _loading,
    builder: (context, snapshot) {
      if (_file == null && snapshot.hasData) _use(snapshot.requireData);
      final file = _file;
      return TwinBackdrop(
        child: Scaffold(
          backgroundColor: Colors.transparent,
          appBar: AppBar(
            backgroundColor: Colors.transparent,
            title: Text(vaultTitle(widget.path)),
            actions: [
              if (file != null && vaultWritable(file.path))
                PopupMenuButton<VaultQuizKind>(
                  tooltip: '덱 만들기',
                  enabled: !_generating,
                  icon: const Icon(Icons.auto_awesome_outlined),
                  onSelected: _generate,
                  itemBuilder: (_) => const [
                    PopupMenuItem(
                      value: VaultQuizKind.auto,
                      child: Text('덱 만들기 · 자동'),
                    ),
                    PopupMenuItem(
                      value: VaultQuizKind.terms,
                      child: Text('용어 덱 만들기'),
                    ),
                    PopupMenuItem(
                      value: VaultQuizKind.vocab,
                      child: Text('단어 덱 만들기'),
                    ),
                  ],
                ),
            ],
          ),
          body: file != null
              ? _read(context, file)
              : snapshot.connectionState == ConnectionState.done &&
                    snapshot.hasError
              ? VaultNotice(
                  icon: Icons.description_outlined,
                  text: vaultError(snapshot.error!),
                  retry: () => setState(() {
                    _loading = widget.api.file(widget.path);
                  }),
                )
              : const VaultLoading('문서를 받는 중'),
        ),
      );
    },
  );

  Widget _read(BuildContext context, VaultFile file) {
    final theme = Theme.of(context);
    final cards = _markdown!.cards;
    return Column(
      children: [
        if (_generating)
          const LinearProgressIndicator(semanticsLabel: '덱을 만드는 중'),
        Expanded(
          child: ListView(
            padding: const EdgeInsets.fromLTRB(
              Look.pagePad,
              Look.groupTitleGap,
              Look.pagePad,
              Look.groupGap,
            ),
            children: [
              Text(
                file.path,
                style: theme.textTheme.bodySmall?.copyWith(
                  color: theme.colorScheme.onSurfaceVariant,
                ),
              ),
              const SizedBox(height: Look.cardGap),
              VaultPanel(
                child: Padding(
                  padding: const EdgeInsets.all(Look.pagePad),
                  child: _markdown!.body.trim().isEmpty
                      ? const Text('빈 문서예요.')
                      : MarkdownBody(
                          data: _markdown!.body,
                          selectable: true,
                          styleSheet:
                              chatMarkdownStyle(
                                theme,
                                base: theme.textTheme.bodyLarge!.copyWith(
                                  height: 1.6,
                                ),
                                codeBg: theme.colorScheme.surfaceContainerHigh,
                              ).copyWith(
                                tableColumnWidth: _markdown!.hasWideTable
                                    ? null
                                    : const FlexColumnWidth(),
                                tableScrollbarThumbVisibility: false,
                              ),
                          // Do not let a document load resources from outside the vault.
                          imageBuilder: (_, _, alt) => Text(alt ?? ''),
                          onTapLink: (_, href, _) {
                            final link = _relativeMarkdown(href);
                            if (link != null) {
                              Navigator.of(context).push<void>(
                                MaterialPageRoute(
                                  builder: (_) => VaultReader(
                                    api: widget.api,
                                    path: link,
                                    studyScope: widget.studyScope,
                                    onChanged: widget.onChanged,
                                  ),
                                ),
                              );
                            }
                          },
                        ),
                ),
              ),
            ],
          ),
        ),
        if (vaultWritable(file.path) || cards.isNotEmpty)
          SafeArea(
            top: false,
            minimum: const EdgeInsets.fromLTRB(
              Look.pagePad,
              Look.groupTitleGap,
              Look.pagePad,
              Look.cardGap,
            ),
            child: Wrap(
              alignment: WrapAlignment.center,
              spacing: Look.groupTitleGap,
              runSpacing: Look.groupTitleGap,
              children: [
                if (vaultWritable(file.path))
                  FilledButton.tonalIcon(
                    onPressed: _generating ? null : _edit,
                    icon: const Icon(Icons.edit_outlined),
                    label: const Text('고치기'),
                  ),
                if (cards.isNotEmpty)
                  FilledButton.icon(
                    onPressed: _generating ? null : _review,
                    icon: const Icon(Icons.style_outlined),
                    label: Text('퀴즈 · ${cards.length}장'),
                  ),
              ],
            ),
          ),
      ],
    );
  }
}

enum _ConflictAction { reload, overwrite }

class VaultEditor extends StatefulWidget {
  const VaultEditor({
    super.key,
    required this.api,
    this.file,
    this.directory = '',
  });

  final VaultApi api;
  final VaultFile? file;
  final String directory;

  @override
  State<VaultEditor> createState() => _VaultEditorState();
}

class _VaultEditorState extends State<VaultEditor> {
  late final _text = TextEditingController(text: widget.file?.text);
  final _name = TextEditingController();
  late String? _basePath = widget.file?.path;
  late String? _baseSha = widget.file?.sha;
  late String _baseText = widget.file?.text ?? '';
  bool _saving = false;
  bool _askingConflict = false;
  bool _confirmingExit = false;
  String? _error;

  bool get _dirty => _text.text != _baseText || _name.text.isNotEmpty;
  String? get _path {
    if (widget.file case final file?) return file.path;
    var name = _name.text.trim();
    if (name.isEmpty || name.contains('/') || name.startsWith('.')) return null;
    if (!name.endsWith('.md')) name += '.md';
    final path = widget.directory.isEmpty ? name : '${widget.directory}/$name';
    return vaultWritable(path) ? path : null;
  }

  Future<void> _save() async {
    if (_saving) return;
    final path = _path;
    if (path == null) {
      setState(() => _error = '문서 이름을 확인해 주세요.');
      return;
    }
    FocusScope.of(context).unfocus();
    setState(() {
      _saving = true;
      _error = null;
    });
    final draft = _text.text;
    var sha = path == _basePath ? _baseSha : null;
    try {
      while (mounted) {
        try {
          final saved = await widget.api.save(path, draft, sha);
          if (mounted) Navigator.of(context).pop(saved);
          return;
        } on VaultConflict catch (conflict) {
          if (!mounted) return;
          setState(() => _askingConflict = true);
          final action = await _conflict(removed: conflict.sha == null);
          if (!mounted) return;
          setState(() => _askingConflict = false);
          if (action == null) return;
          if (action == _ConflictAction.reload) {
            setState(() {
              _text.text = _baseText = conflict.text ?? '';
              _basePath = path;
              _baseSha = conflict.sha;
            });
            return;
          }
          // A second concurrent edit must produce another decision.
          sha = conflict.sha;
        }
      }
    } catch (error) {
      if (mounted) setState(() => _error = vaultError(error));
    } finally {
      if (mounted) setState(() => _saving = false);
    }
  }

  Future<_ConflictAction?> _conflict({required bool removed}) =>
      showDialog<_ConflictAction>(
        context: context,
        builder: (dialog) => ModalLook(
          child: AlertDialog(
            title: const Text('다른 곳에서 바뀜'),
            content: Text(
              removed
                  ? '다른 기기에서 문서를 지웠어요. 편집기를 비우거나 초안으로 문서를 다시 만들 수 있어요.'
                  : '다른 기기에서 이 문서를 고쳤어요. 최신 문서를 불러오거나 지금 쓴 내용으로 덮을 수 있어요.',
            ),
            actions: [
              TextButton(
                onPressed: () => Navigator.pop(dialog, _ConflictAction.reload),
                child: Text(removed ? '편집기 비우기' : '다시 불러오기'),
              ),
              FilledButton(
                onPressed: () =>
                    Navigator.pop(dialog, _ConflictAction.overwrite),
                child: Text(removed ? '초안으로 복원' : '내 것으로 덮기'),
              ),
            ],
          ),
        ),
      );

  Future<void> _leave() async {
    if (_saving || _confirmingExit) return;
    if (!_dirty) {
      Navigator.pop(context);
      return;
    }
    _confirmingExit = true;
    final leave = await showDialog<bool>(
      context: context,
      builder: (dialog) => ModalLook(
        child: AlertDialog(
          title: const Text('저장하지 않은 변경이 있어요'),
          actions: [
            TextButton(
              onPressed: () => Navigator.pop(dialog),
              child: const Text('계속 고치기'),
            ),
            FilledButton(
              onPressed: () => Navigator.pop(dialog, true),
              child: const Text('나가기'),
            ),
          ],
        ),
      ),
    );
    _confirmingExit = false;
    if (mounted && leave == true) Navigator.pop(context);
  }

  @override
  void dispose() {
    _name.dispose();
    _text.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) => PopScope<VaultFile>(
    canPop: !_saving && !_dirty,
    onPopInvokedWithResult: (didPop, _) {
      if (!didPop) _leave();
    },
    child: TwinBackdrop(
      child: Scaffold(
        backgroundColor: Colors.transparent,
        appBar: AppBar(
          backgroundColor: Colors.transparent,
          leading: BackButton(onPressed: _leave),
          title: Text(widget.file == null ? '새 문서' : '문서 고치기'),
          actions: [
            Padding(
              padding: const EdgeInsets.only(right: Look.pagePad),
              child: FilledButton(
                onPressed: _saving || (!_dirty && widget.file != null)
                    ? null
                    : _save,
                child: const Text('저장'),
              ),
            ),
          ],
        ),
        body: SafeArea(
          top: false,
          child: Padding(
            padding: const EdgeInsets.fromLTRB(
              Look.pagePad,
              Look.groupTitleGap,
              Look.pagePad,
              Look.cardGap,
            ),
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                if (_saving && !_askingConflict)
                  const LinearProgressIndicator(semanticsLabel: '저장하는 중'),
                if (widget.file == null)
                  TextField(
                    controller: _name,
                    readOnly: _saving,
                    decoration: const InputDecoration(labelText: '문서 이름'),
                    onChanged: (_) => setState(() {}),
                  )
                else
                  Text(
                    widget.file!.path,
                    style: Theme.of(context).textTheme.bodySmall,
                  ),
                const SizedBox(height: Look.fieldGap),
                if (_error case final error?)
                  ErrorBand(text: error, onRetry: _save),
                Expanded(
                  child: TextField(
                    controller: _text,
                    readOnly: _saving,
                    expands: true,
                    minLines: null,
                    maxLines: null,
                    textAlignVertical: TextAlignVertical.top,
                    keyboardType: TextInputType.multiline,
                    onChanged: (_) => setState(() {}),
                    style: const TextStyle(
                      fontFamily: 'TermMono',
                      fontFamilyFallback: Look.flowMonoFallback,
                      height: 1.55,
                    ),
                    decoration: const InputDecoration(
                      hintText: '마크다운 입력',
                      alignLabelWithHint: true,
                    ),
                  ),
                ),
              ],
            ),
          ),
        ),
      ),
    ),
  );
}
