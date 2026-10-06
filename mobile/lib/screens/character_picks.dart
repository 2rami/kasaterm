import 'dart:async';
import 'dart:typed_data';

import 'package:flutter/material.dart';

import '../character_picks.dart';
import '../look.dart';
import '../machine_look.dart';
import '../relay_account.dart';
import '../server.dart';
import '../twins_loading.dart';
import 'controls.dart';

/// 학생 고르기의 값 — 명단·테마는 계정에서, 고를 후보(테마·이름·얼굴)는 붙은 데스크톱에서 받는다.
/// 바꾸면 계정 동기화로 쓰고, 다른 기기는 다음 동기화에 같은 명단을 받는다.
class CharacterPicksStore extends ChangeNotifier {
  CharacterPicksStore({required this.server, required this.api});

  final Server server;
  final RelayAccountApi Function() api;

  static const _keys = ['character_picks'];

  bool loading = true;
  String? error;
  List<ThemeCard> themes = const [];
  String? machine;
  final Map<String, Roster> rosters = {};
  final Set<String> _noRoster = {};
  final Map<String, Future<void>> _rosterLoads = {};
  final Set<String> expanded = {};
  final Map<String, Future<Uint8List>> _faces = {};

  AccountSyncSnapshot? _snapshot;
  Picks _saved = const [];
  String _activeTheme = '';

  /// 고르고 아직 계정에 안 닿은 학생 테마.
  String? _themeWanted;

  /// 계정에 아직 안 닿은 바꿈 — 화면은 이걸 저장된 명단 위에 얹어 바로 보인다.
  final List<Picks Function(Picks)> _queue = [];
  Future<void> _writes = Future.value();
  bool _closed = false;

  String get activeTheme => _themeWanted ?? _activeTheme;

  Picks get picks {
    var p = _saved;
    for (final op in _queue) {
      try {
        p = op(p);
      } on PickError {
        // 계정에 쓸 때 다시 판정한다 — 여기선 그 바꿈만 건너뛴다.
      }
    }
    return p;
  }

  bool get saving => _queue.isNotEmpty || _themeWanted != null;

  Roster? rosterOf(String key) => rosters[key];
  bool rosterMissing(String key) => _noRoster.contains(key);

  ThemeCard? themeOf(String key) => themes.where((t) => t.key == key).firstOrNull;

  /// 이 데스크톱에 없는 테마에 적힌 이름 — 다른 기기에서 고른 것이라 지우지 않고 보이기만 한다.
  Picks get elsewhere => [
    for (final e in picks)
      if (themeOf(e.$1) == null) e,
  ];

  Future<void> load() async {
    loading = true;
    error = null;
    _notify();
    final client = api();
    try {
      final (choices, snapshot) = await (_settle(server.characterChoices()), _settle(client.readSyncWith(_keys))).wait;
      if (choices.$2 != null) {
        error = '데스크톱에서 학생 목록을 못 받았어요. 데스크톱이 꺼졌거나 옛 판이에요.';
      } else if (snapshot.$2 != null) {
        error = snapshot.$2 is AccountException ? (snapshot.$2 as AccountException).message : '계정 설정을 받지 못했어요.';
      } else {
        themes = ThemeCard.listFrom(choices.$1);
        _take(snapshot.$1!);
        unawaited(_loadMachine());
        await _ensure({themeKeyOf(_activeTheme), for (final e in _saved) e.$1});
        if (expanded.isEmpty) {
          expanded
            ..add(themeKeyOf(_activeTheme))
            ..addAll([for (final e in livePicksOf()) e.$1]);
        }
      }
    } finally {
      client.close();
    }
    loading = false;
    _notify();
  }

  static Future<(T?, Object?)> _settle<T>(Future<T> f) =>
      f.then<(T?, Object?)>((v) => (v, null), onError: (Object e) => (null, e));

  Future<void> _loadMachine() async {
    try {
      machine = (await server.me()).machine;
      _notify();
    } on ServerException {
      // 기기 이름은 안내문 장식이다.
    }
  }

  void _take(AccountSyncSnapshot snapshot) {
    _snapshot = snapshot;
    _saved = parsePicks(snapshot.settings['character_picks']);
    final theme = snapshot.settings['character_theme'];
    _activeTheme = theme is String ? theme : '';
  }

  Picks livePicksOf() => livePicks(picks, rosters);

  bool isOn(String key, String name) => isPicked(picks, rosters, activeTheme, key, name);

  List<(String, String)> get order => assignmentOrder(picks, rosters);

  /// 그 테마 명단을 받아 둔다. 판정(바꿔치기·켜짐)이 명단을 보므로 바꾸기 전에 부른다.
  Future<void> _ensure(Iterable<String> keys) => Future.wait([
    for (final key in keys.toSet())
      if (themeOf(key) != null && !rosters.containsKey(key))
        _rosterLoads[key] ??= () async {
          try {
            rosters[key] = Roster.fromJson(await server.themeRoster(key));
            _noRoster.remove(key);
          } on ServerException {
            _noRoster.add(key);
          } finally {
            _rosterLoads.remove(key);
          }
          _notify();
        }(),
  ]);

  void toggleExpanded(String key) {
    if (!expanded.remove(key)) {
      expanded.add(key);
      unawaited(_ensure([key]));
    }
    _notify();
  }

  Future<String?> pick(String key, String name, bool on) =>
      _change([key, for (final e in picks) e.$1], (p) => pickOne(p, key, name, on, rosterOf));

  Future<String?> pickTheme(String key, bool on) =>
      _change([key, for (final e in picks) e.$1], (p) => pickAll(p, key, on, rosterOf));

  /// 바꿈을 지금 명단에 먼저 얹어 보고(안 되면 까닭을 돌려준다), 계정에는 순서대로 쓴다.
  /// 다른 기기가 먼저 바꿨으면 그 최신 명단에 같은 바꿈을 다시 얹는다 — 남이 고친 테마는 덮지 않는다.
  Future<String?> _change(List<String> needs, Picks Function(Picks) op) async {
    await _ensure(needs);
    try {
      op(picks);
    } on PickError catch (e) {
      return e.message;
    }
    _queue.add(op);
    _notify();
    final done = Completer<String?>();
    _writes = _writes.then((_) async {
      done.complete(await _write(op));
      _queue.remove(op);
      _notify();
    });
    return done.future;
  }

  Future<String?> _write(Picks Function(Picks) op) async {
    final client = api();
    try {
      var snapshot = _snapshot ?? await client.readSyncWith(_keys);
      for (var attempt = 0; attempt < 3; attempt++) {
        final current = parsePicks(snapshot.settings['character_picks']);
        await _ensure([for (final e in current) e.$1]);
        final Picks next;
        try {
          next = op(current);
        } on PickError catch (e) {
          _take(snapshot);
          return e.message;
        }
        try {
          _take(await client.patchSync(snapshot.revision, {'character_picks': encodePicks(next)},
              keys: _keys, failure: '계정에 저장하지 못했어요. 연결을 확인해 주세요.'));
          return null;
        } on AccountSyncConflict catch (e) {
          snapshot = e.current;
        }
      }
      _take(snapshot);
      return '다른 기기에서 명단이 바뀌었어요. 다시 골라 주세요.';
    } on AccountException catch (e) {
      return _saveError(e);
    } finally {
      client.close();
    }
  }

  /// 학생 테마(그림·말투 한 벌). 번들은 계정에서 키를 지운다 — 빈 값은 동기화가 받지 않는 값이다.
  Future<String?> selectTheme(String id) async {
    if (id == activeTheme) return null;
    _themeWanted = id;
    _notify();
    unawaited(_ensure([themeKeyOf(id)]));
    final done = Completer<String?>();
    _writes = _writes.then((_) async {
      final client = api();
      String? problem;
      try {
        var snapshot = _snapshot ?? await client.readSyncWith(_keys);
        for (var attempt = 0; ; attempt++) {
          try {
            _take(await client.patchSync(snapshot.revision, {'character_theme': id.isEmpty ? null : id},
                keys: _keys, failure: '계정에 저장하지 못했어요. 연결을 확인해 주세요.'));
            break;
          } on AccountSyncConflict catch (e) {
            snapshot = e.current;
            if (attempt == 2) {
              _take(snapshot);
              problem = '다른 기기에서 설정이 바뀌었어요. 다시 골라 주세요.';
              break;
            }
          }
        }
      } on AccountException catch (e) {
        problem = _saveError(e);
      } finally {
        client.close();
      }
      if (_themeWanted == id) _themeWanted = null;
      done.complete(problem);
      _notify();
    });
    return done.future;
  }

  String _saveError(AccountException e) => e.status == 400
      ? '관문이 학생 명단을 받지 않았어요. 관문을 새 판으로 올려야 해요.'
      : '계정에 저장하지 못했어요. ${e.message}';

  Future<Uint8List> face(String slug, String themeId) =>
      _faces.putIfAbsent('$themeId/$slug', () => server.imageBytes(server.characterFace(slug, themeId)));

  void _notify() {
    if (!_closed) notifyListeners();
  }

  @override
  void dispose() {
    _closed = true;
    super.dispose();
  }
}

class CharacterPicksScreen extends StatefulWidget {
  const CharacterPicksScreen({super.key, required this.server, required this.api});

  final Server server;
  final RelayAccountApi Function() api;

  @override
  State<CharacterPicksScreen> createState() => _CharacterPicksScreenState();
}

class _CharacterPicksScreenState extends State<CharacterPicksScreen> {
  late final store = CharacterPicksStore(server: widget.server, api: widget.api);

  @override
  void initState() {
    super.initState();
    unawaited(store.load());
  }

  @override
  void dispose() {
    store.dispose();
    super.dispose();
  }

  void _say(String? text) {
    if (text == null || !mounted) return;
    ScaffoldMessenger.of(context)
      ..hideCurrentSnackBar()
      ..showSnackBar(SnackBar(content: Text(text)));
  }

  Future<void> _chooseTheme() async {
    final id = await showModalBottomSheet<String>(
      context: context,
      showDragHandle: true,
      isScrollControlled: true,
      builder: (_) => ModalLook(child: _ThemeSheet(store: store)),
    );
    if (id != null) _say(await store.selectTheme(id));
  }

  @override
  Widget build(BuildContext context) => TwinBackdrop(
    child: Scaffold(
      backgroundColor: Colors.transparent,
      appBar: AppBar(backgroundColor: Colors.transparent, title: const Text('학생 고르기')),
      body: ListenableBuilder(listenable: store, builder: (context, _) => _body()),
    ),
  );

  Widget _body() {
    if (store.loading && store.themes.isEmpty) {
      return const Center(child: TwinsLoading(label: '학생 목록을 받는 중', size: Look.twinsSmall));
    }
    if (store.error != null) {
      return Center(
        child: TwinsNotice(
          text: store.error!,
          action: FilledButton(onPressed: store.load, child: const Text('다시 시도')),
        ),
      );
    }
    final theme = Theme.of(context);
    return ListView(
      padding: const EdgeInsets.fromLTRB(Look.pagePad, 0, Look.pagePad, Look.groupGap * 2),
      children: [
        SettingsGroup(
          children: [
            SettingsRow(
              key: const Key('pick-order'),
              icon: Icons.format_list_numbered_rounded,
              title: '새 창에 나오는 차례',
              subtitle: _orderText(),
            ),
            SettingsRow(
              key: const Key('pick-theme'),
              tone: 1,
              icon: Icons.style_outlined,
              title: '학생 테마',
              subtitle: '${_themeLabel(store.activeTheme)} · 그림과 말투 한 벌',
              chevron: true,
              onTap: _chooseTheme,
            ),
          ],
        ),
        if (store.elsewhere.isNotEmpty)
          SettingsGroup(
            title: '이 데스크톱에 없는 테마',
            children: [
              for (final (key, names) in store.elsewhere)
                SettingsRow(
                  icon: Icons.cloud_outlined,
                  title: key,
                  subtitle: '${names.join(' · ')}\n다른 기기에서 고른 학생이라 그대로 둬요',
                ),
            ],
          ),
        for (final t in store.themes) _ThemeGroup(store: store, card: t, say: _say),
        Padding(
          padding: const EdgeInsets.fromLTRB(4, Look.groupGap, 4, 0),
          child: Text(
            '계정에 저장해 모든 기기가 같은 명단을 써요. 이미 떠 있는 창의 학생은 그대로예요.'
            '${store.machine == null ? '' : ' 학생 목록과 얼굴은 ${machineName(store.machine!, local: true)} 것이에요.'}',
            style: theme.textTheme.bodySmall?.copyWith(color: theme.colorScheme.onSurfaceVariant),
          ),
        ),
      ],
    );
  }

  String _themeLabel(String id) =>
      store.themeOf(themeKeyOf(id))?.label ?? (id.isEmpty ? '기본' : '$id · 이 데스크톱에 없음');

  String _orderText() {
    final order = store.order;
    if (order.isEmpty) return '고른 학생이 없어 「${_themeLabel(store.activeTheme)}」 전원이 차례로 나와요';
    const shown = 6;
    final names = [for (final (_, n) in order.take(shown)) n].join(' → ');
    return order.length > shown ? '$names 외 ${order.length - shown}명' : names;
  }
}

/// 테마 하나 — 머리 줄(얼굴·이름·N명 중 K명)을 누르면 펼치고, 펼치면 전부·해제와 학생 칸.
class _ThemeGroup extends StatelessWidget {
  const _ThemeGroup({required this.store, required this.card, required this.say});

  final CharacterPicksStore store;
  final ThemeCard card;
  final void Function(String?) say;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final key = card.key;
    final open = store.expanded.contains(key);
    final roster = store.rosterOf(key);
    final on = roster == null ? null : [for (final m in roster.members) if (store.isOn(key, m.name)) m.name].length;
    final count = roster?.members.length ?? card.count;
    final head = InkWell(
      key: Key('pick-group-$key'),
      onTap: () => store.toggleExpanded(key),
      child: ConstrainedBox(
        constraints: const BoxConstraints(minHeight: Look.row2),
        child: Padding(
          padding: const EdgeInsets.symmetric(horizontal: Look.cardPad, vertical: 10),
          child: Row(
            children: [
              _FaceStack(store: store, themeId: card.id, slugs: card.faces.take(3).toList()),
              const SizedBox(width: 12),
              Expanded(
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  mainAxisSize: MainAxisSize.min,
                  children: [
                    Text(card.label, style: theme.textTheme.titleSmall, overflow: TextOverflow.ellipsis),
                    const SizedBox(height: Look.rowGap),
                    Text(
                      on == null ? '$count명' : '$count명 중 $on명',
                      style: theme.textTheme.bodySmall?.copyWith(color: scheme.onSurfaceVariant),
                    ),
                  ],
                ),
              ),
              AnimatedRotation(
                turns: open ? 0.25 : 0,
                duration: Look.still(context) ? Duration.zero : const Duration(milliseconds: 180),
                child: Icon(Icons.chevron_right_rounded, color: scheme.onSurfaceVariant),
              ),
            ],
          ),
        ),
      ),
    );
    return Padding(
      padding: const EdgeInsets.only(top: Look.cardGap),
      child: DecoratedBox(
        decoration: TwinTone.of(context).cardBox(),
        child: ClipRRect(
          borderRadius: Look.cardCorners,
          child: Material(
            type: MaterialType.transparency,
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.stretch,
              children: [
                head,
                if (open) ...[
                  const Divider(height: 1),
                  Padding(
                    padding: const EdgeInsets.all(Look.cardPad),
                    child: roster == null
                        ? _rosterState(context, key)
                        : Column(
                            crossAxisAlignment: CrossAxisAlignment.stretch,
                            children: [
                              Row(
                                children: [
                                  Expanded(
                                    child: OutlinedButton(
                                      key: Key('pick-all-$key'),
                                      onPressed: () async => say(await store.pickTheme(key, true)),
                                      child: const Text('전부 고르기'),
                                    ),
                                  ),
                                  const SizedBox(width: 8),
                                  Expanded(
                                    child: OutlinedButton(
                                      key: Key('pick-none-$key'),
                                      onPressed: () async => say(await store.pickTheme(key, false)),
                                      child: const Text('선택 해제'),
                                    ),
                                  ),
                                ],
                              ),
                              const SizedBox(height: Look.cardGap),
                              _Grid(store: store, card: card, roster: roster, say: say),
                            ],
                          ),
                  ),
                ],
              ],
            ),
          ),
        ),
      ),
    );
  }

  Widget _rosterState(BuildContext context, String key) {
    if (!store.rosterMissing(key)) {
      return const Center(child: Padding(padding: EdgeInsets.all(8), child: TwinsMark(hopping: true, face: Look.face)));
    }
    final theme = Theme.of(context);
    return Text(
      '이 테마의 명단을 데스크톱에서 못 받았어요.',
      style: theme.textTheme.bodySmall?.copyWith(color: theme.colorScheme.onSurfaceVariant),
    );
  }
}

class _Grid extends StatelessWidget {
  const _Grid({required this.store, required this.card, required this.roster, required this.say});

  final CharacterPicksStore store;
  final ThemeCard card;
  final Roster roster;
  final void Function(String?) say;

  @override
  Widget build(BuildContext context) {
    final order = store.order;
    final explicit = store.livePicksOf().isNotEmpty;
    return LayoutBuilder(
      builder: (context, box) {
        const gap = 8.0;
        final cols = ((box.maxWidth + gap) / (Look.pickCell + gap)).floor().clamp(1, 12);
        final width = (box.maxWidth - gap * (cols - 1)) / cols;
        return Wrap(
          spacing: gap,
          runSpacing: gap,
          children: [
            for (final m in roster.members)
              SizedBox(
                width: width,
                child: _Cell(
                  store: store,
                  themeId: card.id,
                  member: m,
                  on: store.isOn(card.key, m.name),
                  rank: explicit ? order.indexWhere((e) => e.$1 == card.key && e.$2 == m.name) : -1,
                  onTap: (on) async => say(await store.pick(card.key, m.name, on)),
                ),
              ),
          ],
        );
      },
    );
  }
}

class _Cell extends StatelessWidget {
  const _Cell({
    required this.store,
    required this.themeId,
    required this.member,
    required this.on,
    required this.rank,
    required this.onTap,
  });

  final CharacterPicksStore store;
  final String themeId;
  final RosterMember member;
  final bool on;

  /// 새 창 차례(0부터). 고른 학생이 없어 테마 전원이 나오는 동안은 -1.
  final int rank;
  final ValueChanged<bool> onTap;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final note = !on ? '선택' : rank >= 0 ? '${rank + 1}번째' : '사용 중';
    return Semantics(
      button: true,
      selected: on,
      label: '${member.name}, $note',
      excludeSemantics: true,
      onTap: () => onTap(!on),
      child: Material(
        color: on ? scheme.primary.withValues(alpha: 0.18) : Colors.transparent,
        shape: RoundedRectangleBorder(borderRadius: Look.corners),
        clipBehavior: Clip.antiAlias,
        child: InkWell(
          key: Key('pick-cell-${themeKeyOf(themeId)}-${member.name}'),
          onTap: () => onTap(!on),
          child: Padding(
            padding: const EdgeInsets.symmetric(vertical: 8, horizontal: 4),
            child: Column(
              children: [
                Opacity(
                  opacity: on ? 1 : 0.45,
                  child: _Face(store: store, themeId: themeId, slug: member.slug, size: Look.pickFace),
                ),
                const SizedBox(height: 6),
                Text(
                  member.name,
                  maxLines: 1,
                  overflow: TextOverflow.ellipsis,
                  style: theme.textTheme.bodySmall?.copyWith(
                    fontWeight: FontWeight.w600,
                    color: on ? scheme.onSurface : scheme.onSurfaceVariant,
                  ),
                ),
                Text(
                  note,
                  maxLines: 1,
                  style: theme.textTheme.bodySmall?.copyWith(
                    color: on && rank >= 0 ? scheme.primary : scheme.onSurfaceVariant,
                  ),
                ),
              ],
            ),
          ),
        ),
      ),
    );
  }
}

class _Face extends StatelessWidget {
  const _Face({required this.store, required this.themeId, required this.slug, required this.size});

  final CharacterPicksStore store;
  final String themeId;
  final String? slug;
  final double size;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final blank = Container(
      width: size,
      height: size,
      color: scheme.surfaceContainerHighest,
      child: Icon(Icons.person_outline, size: size * 0.55, color: scheme.onSurfaceVariant),
    );
    final s = slug;
    return ClipOval(
      child: s == null
          ? blank
          : FutureBuilder<Uint8List>(
              future: store.face(s, themeId),
              builder: (context, snap) => snap.hasData
                  ? Image.memory(
                      snap.data!,
                      width: size,
                      height: size,
                      fit: BoxFit.cover,
                      alignment: Alignment.topCenter,
                      cacheWidth: (size * MediaQuery.devicePixelRatioOf(context)).round(),
                      errorBuilder: (_, _, _) => blank,
                    )
                  : blank,
            ),
    );
  }
}

/// 테마 머리의 얼굴 셋 — 데스크톱 테마 카드의 미리보기와 같은 학생들.
class _FaceStack extends StatelessWidget {
  const _FaceStack({required this.store, required this.themeId, required this.slugs});

  final CharacterPicksStore store;
  final String themeId;
  final List<String> slugs;

  @override
  Widget build(BuildContext context) {
    final card = TwinTone.of(context).card;
    final n = slugs.isEmpty ? 1 : slugs.length;
    return SizedBox(
      width: Look.face + (n - 1) * (Look.face - Look.faceOverlap),
      height: Look.face,
      child: Stack(
        children: [
          if (slugs.isEmpty) _Face(store: store, themeId: themeId, slug: null, size: Look.face),
          for (final (i, s) in slugs.indexed)
            Positioned(
              left: i * (Look.face - Look.faceOverlap),
              child: Container(
                decoration: BoxDecoration(shape: BoxShape.circle, border: Border.all(color: card, width: 1.5)),
                child: _Face(store: store, themeId: themeId, slug: s, size: Look.face - 3),
              ),
            ),
        ],
      ),
    );
  }
}

class _ThemeSheet extends StatelessWidget {
  const _ThemeSheet({required this.store});

  final CharacterPicksStore store;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    return SafeArea(
      child: ConstrainedBox(
        constraints: BoxConstraints(maxHeight: MediaQuery.sizeOf(context).height * 0.75),
        child: ListView(
          shrinkWrap: true,
          padding: const EdgeInsets.fromLTRB(Look.pagePad, 0, Look.pagePad, Look.pagePad),
          children: [
            Text('학생 테마', style: theme.textTheme.titleMedium),
            const SizedBox(height: Look.rowGap),
            Text(
              '새로 여는 창부터 그림과 말투가 이 테마로 바뀌어요. 모든 기기에 같이 저장돼요.',
              style: theme.textTheme.bodySmall?.copyWith(color: scheme.onSurfaceVariant),
            ),
            const SizedBox(height: Look.cardGap),
            for (final t in store.themes)
              InkWell(
                key: Key('pick-theme-${t.key}'),
                borderRadius: Look.corners,
                onTap: () => Navigator.of(context).pop(t.id),
                child: ConstrainedBox(
                  constraints: const BoxConstraints(minHeight: Look.row2),
                  child: Padding(
                    padding: const EdgeInsets.symmetric(horizontal: 4, vertical: 8),
                    child: Row(
                      children: [
                        _FaceStack(store: store, themeId: t.id, slugs: t.faces.take(3).toList()),
                        const SizedBox(width: 12),
                        Expanded(
                          child: Column(
                            crossAxisAlignment: CrossAxisAlignment.start,
                            mainAxisSize: MainAxisSize.min,
                            children: [
                              Text(t.label, style: theme.textTheme.titleSmall, overflow: TextOverflow.ellipsis),
                              const SizedBox(height: Look.rowGap),
                              Text(
                                '학생 ${t.count}명',
                                style: theme.textTheme.bodySmall?.copyWith(color: scheme.onSurfaceVariant),
                              ),
                            ],
                          ),
                        ),
                        if (t.id == store.activeTheme)
                          Icon(Icons.check_circle_rounded, color: scheme.primary, size: Look.iconSize),
                      ],
                    ),
                  ),
                ),
              ),
          ],
        ),
      ),
    );
  }
}
