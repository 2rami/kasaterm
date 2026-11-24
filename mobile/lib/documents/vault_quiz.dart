import 'package:flutter/material.dart';
import 'package:fsrs/fsrs.dart' as fsrs;

import '../look.dart';
import '../twins_loading.dart';
import 'vault_api.dart';
import 'vault_study.dart';
import 'vault_widgets.dart';

class VaultQuiz extends StatefulWidget {
  const VaultQuiz({
    super.key,
    required this.path,
    required this.cards,
    required this.store,
  });

  final String path;
  final List<StudyCard> cards;
  final VaultStudyStore store;

  @override
  State<VaultQuiz> createState() => _VaultQuizState();
}

class _VaultQuizState extends State<VaultQuiz> {
  final _scheduler = fsrs.Scheduler();
  late final _states = <StudyCard, fsrs.Card>{};
  List<StudyCard> _queue = [];
  int _reviewed = 0;
  bool _loading = true;
  bool _revealed = false;
  bool _saving = false;
  String? _error;

  static const _ratings = {
    fsrs.Rating.again: '다시',
    fsrs.Rating.hard: '어려움',
    fsrs.Rating.good: '잘 기억함',
    fsrs.Rating.easy: '쉬움',
  };

  @override
  void initState() {
    super.initState();
    _load();
  }

  Future<void> _load() async {
    setState(() {
      _error = null;
      _loading = true;
    });
    final now = DateTime.now().toUtc();
    try {
      final cards = await Future.wait([
        for (final (index, card) in widget.cards.indexed)
          widget.store
              .read(widget.path, card)
              .then(
                (state) => (card, state ?? fsrs.Card(cardId: index, due: now)),
              ),
      ]);
      if (!mounted) return;
      setState(() {
        _states.addEntries(cards.map((entry) => MapEntry(entry.$1, entry.$2)));
        _queue = _dueCards();
        _loading = false;
      });
    } catch (_) {
      if (mounted) {
        setState(() {
          _loading = false;
          _error = '이 폰에 저장된 복습 기록을 읽지 못했어요.';
        });
      }
    }
  }

  List<StudyCard> _dueCards() {
    final now = DateTime.now().toUtc();
    final due = widget.cards
        .where((card) => !_states[card]!.due.isAfter(now))
        .toList();
    return due..sort((a, b) => _states[a]!.due.compareTo(_states[b]!.due));
  }

  Future<void> _rate(fsrs.Rating rating) async {
    if (_saving || !_revealed || _queue.isEmpty) return;
    final card = _queue.first;
    setState(() {
      _saving = true;
      _error = null;
    });
    try {
      final next = _scheduler.reviewCard(_states[card]!, rating).card;
      await widget.store.write(widget.path, card, next);
      if (!mounted) return;
      setState(() {
        _states[card] = next;
        _queue = _dueCards();
        _reviewed++;
        _revealed = false;
      });
    } catch (_) {
      if (mounted) {
        setState(() => _error = '복습 기록을 저장하지 못했어요. 다시 평가해 주세요.');
      }
    } finally {
      if (mounted) setState(() => _saving = false);
    }
  }

  @override
  Widget build(BuildContext context) => TwinBackdrop(
    child: Scaffold(
      backgroundColor: Colors.transparent,
      appBar: AppBar(
        backgroundColor: Colors.transparent,
        title: Text(vaultTitle(widget.path)),
      ),
      body: SafeArea(top: false, child: _body(context)),
    ),
  );

  Widget _body(BuildContext context) {
    if (_loading) return const VaultLoading('복습 기록을 받는 중');
    if (_states.isEmpty && _error != null) {
      return VaultNotice(
        icon: Icons.style_outlined,
        text: _error!,
        retry: _load,
      );
    }
    final theme = Theme.of(context);
    if (_states.isEmpty) {
      return const VaultNotice(
        icon: Icons.style_outlined,
        text: '복습할 카드가 없어요.',
      );
    }
    if (_queue.isEmpty) {
      final next = _states.values
          .map((state) => state.due)
          .reduce((a, b) => a.isBefore(b) ? a : b)
          .toLocal();
      final local = MaterialLocalizations.of(context);
      return Center(
        child: Padding(
          padding: const EdgeInsets.all(Look.pagePad),
          child: VaultPanel(
            child: Padding(
              padding: const EdgeInsets.all(Look.groupGap),
              child: Column(
                mainAxisSize: MainAxisSize.min,
                children: [
                  Icon(Icons.task_alt, color: theme.colorScheme.primary),
                  const SizedBox(height: Look.cardGap),
                  Text(
                    _reviewed == 0 ? '지금 복습할 카드가 없어요' : '복습 완료 · $_reviewed장',
                    style: theme.textTheme.titleLarge,
                    textAlign: TextAlign.center,
                  ),
                  const SizedBox(height: Look.groupTitleGap),
                  Text(
                    '다음 복습  ${local.formatShortDate(next)}  ${local.formatTimeOfDay(TimeOfDay.fromDateTime(next))}',
                  ),
                  const SizedBox(height: Look.groupGap),
                  FilledButton.tonal(
                    onPressed: () => Navigator.pop(context),
                    child: const Text('문서로 돌아가기'),
                  ),
                ],
              ),
            ),
          ),
        ),
      );
    }
    final card = _queue.first;
    return Column(
      children: [
        Padding(
          padding: const EdgeInsets.fromLTRB(
            Look.pagePad,
            Look.groupTitleGap,
            Look.pagePad,
            Look.cardGap,
          ),
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Text(
                '복습 · 남은 카드 ${_queue.length}장',
                style: theme.textTheme.titleSmall,
              ),
              const SizedBox(height: Look.groupTitleGap),
              LinearProgressIndicator(
                value: _reviewed / (_reviewed + _queue.length),
                semanticsLabel: '복습 진행률',
              ),
            ],
          ),
        ),
        Expanded(
          child: ListView(
            padding: const EdgeInsets.fromLTRB(
              Look.pagePad,
              0,
              Look.pagePad,
              Look.cardGap,
            ),
            children: [
              VaultPanel(
                child: Padding(
                  padding: const EdgeInsets.all(Look.groupGap),
                  child: Column(
                    crossAxisAlignment: CrossAxisAlignment.stretch,
                    children: [
                      Text(
                        '앞면',
                        style: theme.textTheme.labelLarge?.copyWith(
                          color: theme.colorScheme.primary,
                        ),
                      ),
                      const SizedBox(height: Look.cardGap),
                      SelectableText(
                        card.front,
                        style: theme.textTheme.headlineSmall,
                      ),
                      if (_revealed) ...[
                        const SizedBox(height: Look.fieldGap),
                        const Divider(),
                        const SizedBox(height: Look.fieldGap),
                        Text(
                          '뒷면',
                          style: theme.textTheme.labelLarge?.copyWith(
                            color: theme.colorScheme.primary,
                          ),
                        ),
                        const SizedBox(height: Look.cardGap),
                        SelectableText(
                          card.back,
                          style: theme.textTheme.bodyLarge?.copyWith(
                            height: 1.6,
                          ),
                        ),
                      ],
                    ],
                  ),
                ),
              ),
              if (_error case final error?)
                Padding(
                  padding: const EdgeInsets.only(top: Look.cardGap),
                  child: Text(
                    error,
                    style: TextStyle(color: theme.colorScheme.error),
                  ),
                ),
            ],
          ),
        ),
        Padding(
          padding: const EdgeInsets.fromLTRB(
            Look.pagePad,
            Look.groupTitleGap,
            Look.pagePad,
            Look.cardGap,
          ),
          child: _revealed
              ? Wrap(
                  alignment: WrapAlignment.center,
                  spacing: Look.groupTitleGap,
                  runSpacing: Look.groupTitleGap,
                  children: [
                    for (final entry in _ratings.entries)
                      FilledButton.tonal(
                        onPressed: _saving ? null : () => _rate(entry.key),
                        child: Text(entry.value),
                      ),
                  ],
                )
              : FilledButton.icon(
                  onPressed: () => setState(() => _revealed = true),
                  icon: const Icon(Icons.flip_outlined),
                  label: const Text('정답 보기'),
                ),
        ),
      ],
    );
  }
}
