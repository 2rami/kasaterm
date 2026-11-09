import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import '../background_grace.dart';
import '../look.dart';
import '../network_watch.dart';
import '../server.dart';
import '../shared_rooms_fork.dart';
import '../shared_rooms_model.dart';
import '../shared_rooms_repository.dart';
import '../term_session.dart';
import '../twins_loading.dart';
import 'controls.dart';
import 'conversation_view.dart';
import 'saved_conversations.dart' show archiveSize, archiveTime;
import 'terminal.dart';

int _openRoutes = 0;

/// 공유방 화면이 떠 있는가 — 같은 계정의 데스크톱이 붙을 때 읽던 화면을 닫지 않는다(`main.dart`).
bool get sharedRoutesOpen => _openRoutes > 0;

mixin _CountedRoute<T extends StatefulWidget> on State<T> {
  @override
  void initState() {
    super.initState();
    _openRoutes++;
  }

  @override
  void dispose() {
    _openRoutes--;
    super.dispose();
  }
}

const _ended = '현재 계정의 연결이 종료됐어요.';

String _harness(String h) => h == 'codex' ? 'Codex' : 'Claude';

void openSharedRooms(BuildContext context, SharedRoomsRepository repository) => Navigator.of(context).push(
  MaterialPageRoute<void>(
    settings: const RouteSettings(name: 'shared-rooms'),
    builder: (_) => SharedRoomsScreen(repository: repository),
  ),
);

void openSharedRoom(BuildContext context, SharedRoomsRepository repository, String roomId) => Navigator.of(context).push(
  MaterialPageRoute<void>(
    settings: const RouteSettings(name: 'shared-room'),
    builder: (_) => SharedRoomScreen(repository: repository, roomId: roomId),
  ),
);

void _snack(BuildContext context, Object message) {
  if (context.mounted) ScaffoldMessenger.of(context).showSnackBar(SnackBar(content: Text('$message')));
}

Future<bool> _confirm(BuildContext context, {required String title, required String body, required String action}) async =>
    await showDialog<bool>(
      context: context,
      builder: (context) => ModalLook(
        child: AlertDialog(
          title: Text(title),
          content: Text(body),
          actions: [
            TextButton(autofocus: true, onPressed: () => Navigator.pop(context, false), child: const Text('취소')),
            FilledButton(
              style: FilledButton.styleFrom(
                backgroundColor: Theme.of(context).colorScheme.error,
                foregroundColor: Theme.of(context).colorScheme.onError,
              ),
              onPressed: () => Navigator.pop(context, true),
              child: Text(action),
            ),
          ],
        ),
      ),
    ) ==
    true;

/// 초대 코드로 지금 계정에 참여한다. 따로 로그인하지 않는다 — 앱의 KASA 로그인이 그 계정이다.
Future<void> joinSharedRoom(BuildContext context, SharedRoomsRepository repository) async {
  final joined = await showDialog<String>(context: context, builder: (_) => _JoinDialog(repository: repository));
  if (joined == null || !context.mounted || repository.locked) return;
  if (joined.isEmpty) {
    _snack(context, '공유방에 참여했어요.');
  } else {
    openSharedRoom(context, repository, joined);
  }
}

Future<void> createSharedRoom(BuildContext context, SharedRoomsRepository repository) async {
  final id = await showDialog<String>(context: context, builder: (_) => _RoomDialog(repository: repository));
  if (id != null && context.mounted && !repository.locked) openSharedRoom(context, repository, id);
}

/// 보이는 동안 [onTick] 을 주기적으로 — 앱이 앞에 있고 망이 바뀌면 곧바로 한 번 더.
class SharedRoomsWatch extends StatefulWidget {
  const SharedRoomsWatch({super.key, required this.onTick, required this.every, required this.child});
  final VoidCallback onTick;
  final Duration every;
  final Widget child;
  @override
  State<SharedRoomsWatch> createState() => _SharedRoomsWatchState();
}

class _SharedRoomsWatchState extends State<SharedRoomsWatch> with WidgetsBindingObserver {
  Timer? _timer;

  @override
  void initState() {
    super.initState();
    WidgetsBinding.instance.addObserver(this);
    BackgroundGrace.instance.addListener(_tick);
    NetworkWatch.instance.addListener(_tick);
    _timer = Timer.periodic(widget.every, (_) => _tick());
    WidgetsBinding.instance.addPostFrameCallback((_) => _tick());
  }

  void _tick() {
    if (mounted && BackgroundGrace.instance.live) widget.onTick();
  }

  @override
  void didChangeAppLifecycleState(AppLifecycleState state) {
    if (state == AppLifecycleState.resumed) _tick();
  }

  @override
  void dispose() {
    _timer?.cancel();
    WidgetsBinding.instance.removeObserver(this);
    BackgroundGrace.instance.removeListener(_tick);
    NetworkWatch.instance.removeListener(_tick);
    super.dispose();
  }

  @override
  Widget build(BuildContext context) => widget.child;
}

/// 공유방 한 줄 — 허브 섹션과 목록 화면이 같이 쓴다.
class SharedRoomRow extends StatelessWidget {
  const SharedRoomRow({super.key, required this.room, required this.onTap, this.tone = 0});
  final SharedRoom room;
  final VoidCallback onTap;
  final int tone;

  @override
  Widget build(BuildContext context) => SettingsRow(
    tone: tone,
    icon: room.owner ? Icons.groups_outlined : Icons.group_outlined,
    title: room.displayTitle,
    subtitle: [
      room.owner ? '내 방' : '${room.ownerName.isNotEmpty ? room.ownerName : room.ownerAccount}의 방',
      '대화 ${room.sessions.length}',
      '참여 ${room.members.where((m) => !m.owner).length}',
      if (room.allowFork) '포크 허용',
    ].join(' · '),
    chevron: true,
    onTap: onTap,
  );
}

class SharedRoomsScreen extends StatefulWidget {
  const SharedRoomsScreen({super.key, required this.repository});
  final SharedRoomsRepository repository;
  @override
  State<SharedRoomsScreen> createState() => _SharedRoomsScreenState();
}

class _SharedRoomsScreenState extends State<SharedRoomsScreen> with _CountedRoute {
  SharedRoomsRepository get _repo => widget.repository;

  @override
  Widget build(BuildContext context) => ListenableBuilder(
    listenable: _repo,
    builder: (context, _) {
      final theme = Theme.of(context);
      final mine = [for (final r in _repo.rooms) if (r.owner) r];
      final joined = [for (final r in _repo.rooms) if (!r.owner) r];
      return Scaffold(
        appBar: AppBar(
          title: const Text('공유방'),
          actions: [
            IconButton(
              tooltip: '새로고침',
              onPressed: _repo.locked || _repo.refreshing ? null : _repo.load,
              icon: const Icon(Icons.refresh),
            ),
          ],
        ),
        body: _repo.locked
            ? const Center(child: Text(_ended))
            : SharedRoomsWatch(
                every: const Duration(seconds: 15),
                onTick: _repo.load,
                child: SafeArea(
                  child: ListView(
                    padding: const EdgeInsets.fromLTRB(Look.pagePad, Look.groupTitleGap, Look.pagePad, Look.groupGap),
                    children: [
                      Text(
                        '다른 계정과 대화를 읽기 전용으로 나눠 봐요. 원본 터미널은 조작하지 않고, 허용된 대화는 내 계정의 새 대화로 포크해요.',
                        style: theme.textTheme.bodySmall?.copyWith(color: theme.colorScheme.onSurfaceVariant),
                      ),
                      const SizedBox(height: Look.fieldGap),
                      Wrap(
                        spacing: Look.groupTitleGap,
                        runSpacing: Look.groupTitleGap,
                        children: [
                          FilledButton.icon(
                            onPressed: () => joinSharedRoom(context, _repo),
                            icon: const Icon(Icons.login, size: Look.iconSize),
                            label: const Text('초대 코드로 참여'),
                          ),
                          OutlinedButton.icon(
                            onPressed: () => createSharedRoom(context, _repo),
                            icon: const Icon(Icons.add, size: Look.iconSize),
                            label: const Text('새 공유방'),
                          ),
                        ],
                      ),
                      if (_repo.refreshing) const Padding(padding: EdgeInsets.only(top: Look.groupTitleGap), child: LinearProgressIndicator(minHeight: 2)),
                      if (_repo.error != null) ErrorBand(text: _repo.error!, onRetry: _repo.load),
                      if (_repo.loading && _repo.rooms.isEmpty)
                        const Padding(
                          padding: EdgeInsets.only(top: Look.groupGap),
                          child: TwinsLoading(label: '공유방을 받는 중', size: Look.twinsSmall),
                        )
                      else if (_repo.rooms.isEmpty && _repo.error == null)
                        const TwinsNotice(text: '아직 공유방이 없어요. 초대 코드를 받았다면 참여를 눌러 주세요.'),
                      if (mine.isNotEmpty)
                        SettingsGroup(
                          title: '내가 만든 방',
                          children: [
                            for (final (i, r) in mine.indexed)
                              SharedRoomRow(tone: i, room: r, onTap: () => openSharedRoom(context, _repo, r.id)),
                          ],
                        ),
                      if (joined.isNotEmpty)
                        SettingsGroup(
                          title: '참여한 방',
                          children: [
                            for (final (i, r) in joined.indexed)
                              SharedRoomRow(tone: i, room: r, onTap: () => openSharedRoom(context, _repo, r.id)),
                          ],
                        ),
                    ],
                  ),
                ),
              ),
      );
    },
  );
}

class SharedRoomScreen extends StatefulWidget {
  const SharedRoomScreen({super.key, required this.repository, required this.roomId, this.forkHost});
  final SharedRoomsRepository repository;
  final String roomId;
  final ForkHost Function(Server)? forkHost;
  @override
  State<SharedRoomScreen> createState() => _SharedRoomScreenState();
}

class _SharedRoomScreenState extends State<SharedRoomScreen> with _CountedRoute {
  SharedRoomsRepository get _repo => widget.repository;
  String? _error;
  bool _busy = false;

  Future<void> _refresh() async {
    try {
      await _repo.refreshRoom(widget.roomId);
      if (mounted && _error != null) setState(() => _error = null);
    } on SharedRoomsException catch (e) {
      if (mounted) setState(() => _error = e.message);
    }
  }

  Future<void> _do(Future<void> Function() action) async {
    if (_busy) return;
    setState(() => _busy = true);
    try {
      await action();
    } on SharedRoomsException catch (e) {
      if (mounted) _snack(context, e);
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  Future<void> _invite(SharedRoom room) => _do(() async {
    final invite = await _repo.issueInvite(room.id);
    if (!mounted || _repo.locked) return;
    await showDialog<void>(context: context, builder: (_) => _InviteDialog(repository: _repo, invite: invite));
  });

  Future<void> _revoke(SharedRoom room, SharedInviteSummary invite) async {
    if (!await _confirm(context, title: '초대를 폐기할까요?', body: '이 코드로는 더 참여할 수 없어요. 이미 들어온 사람은 그대로예요.', action: '폐기')) return;
    await _do(() => _repo.revokeInvite(room.id, invite.id));
  }

  Future<void> _remove(SharedRoom room, SharedMember member) async {
    if (!await _confirm(context, title: '${member.label}님을 내보낼까요?', body: '이 방의 대화를 더 볼 수 없어요. 이미 만든 포크는 그 사람 계정에 남아요.', action: '내보내기')) {
      return;
    }
    await _do(() => _repo.removeMember(room.id, member.account));
  }

  Future<void> _leave(SharedRoom room) async {
    if (!await _confirm(context, title: '이 방에서 나갈까요?', body: '다시 들어오려면 새 초대 코드가 필요해요. 원본 대화에는 영향이 없어요.', action: '나가기')) return;
    await _do(() async {
      await _repo.leave(room.id);
      if (mounted) Navigator.of(context).pop();
    });
  }

  Future<void> _delete(SharedRoom room) async {
    if (!await _confirm(context, title: '공유를 끝낼까요?', body: '참여자 모두 이 방과 게시한 대화를 더 볼 수 없어요. 원본 대화와 이미 만든 포크는 그대로예요.', action: '공유 끝내기')) {
      return;
    }
    await _do(() async {
      await _repo.deleteRoom(room.id);
      if (mounted) Navigator.of(context).pop();
    });
  }

  Future<void> _rename(SharedRoom room) async {
    await showDialog<String>(context: context, builder: (_) => _RoomDialog(repository: _repo, room: room));
  }

  @override
  Widget build(BuildContext context) => ListenableBuilder(
    listenable: _repo,
    builder: (context, _) {
      final room = _repo.room(widget.roomId);
      final gone = _repo.gone[widget.roomId];
      return Scaffold(
        appBar: AppBar(
          title: Text(room?.displayTitle ?? '공유방', maxLines: 1, overflow: TextOverflow.ellipsis),
          actions: [IconButton(tooltip: '새로고침', onPressed: _repo.locked ? null : _refresh, icon: const Icon(Icons.refresh))],
        ),
        body: _repo.locked
            ? const Center(child: Text(_ended))
            : gone != null
            ? Center(child: Padding(padding: const EdgeInsets.all(Look.pagePad), child: Text(gone, textAlign: TextAlign.center)))
            : SharedRoomsWatch(
                every: const Duration(seconds: 8),
                onTick: () => unawaited(_refresh()),
                child: room == null
                    ? (_error == null
                          ? const TwinsLoading(label: '공유방을 여는 중', size: Look.twinsSmall)
                          : Padding(padding: const EdgeInsets.all(Look.pagePad), child: ErrorBand(text: _error!, onRetry: _refresh)))
                    : _body(context, room),
              ),
      );
    },
  );

  Widget _body(BuildContext context, SharedRoom room) {
    final theme = Theme.of(context);
    final now = DateTime.now();
    final dim = theme.textTheme.bodySmall?.copyWith(color: theme.colorScheme.onSurfaceVariant);
    return SafeArea(
      child: ListView(
        padding: const EdgeInsets.fromLTRB(Look.pagePad, 0, Look.pagePad, Look.groupGap),
        children: [
          if (_busy) const LinearProgressIndicator(minHeight: 2),
          if (_error != null) ErrorBand(text: _error!, onRetry: _refresh),
          Padding(
            padding: const EdgeInsets.only(top: Look.groupTitleGap),
            child: Text(
              [
                room.owner ? '내가 만든 방' : '${room.ownerName.isNotEmpty ? room.ownerName : room.ownerAccount}의 방',
                room.allowFork ? '읽기 · 포크 허용' : '읽기 전용',
                '갱신 ${archiveTime(room.updatedAt)}',
              ].join(' · '),
              style: dim,
            ),
          ),
          SettingsGroup(
            title: '공유된 대화',
            children: [
              if (room.owner)
                SettingsRow(
                  icon: Icons.upload_outlined,
                  title: 'PC 대화 게시하기',
                  subtitle: '내 PC의 대화를 골라 이 방에 읽기 전용으로 올려요.',
                  chevron: true,
                  onTap: () => showPublishSheet(context, _repo, room.id, host: widget.forkHost),
                ),
              if (room.sessions.isEmpty)
                Padding(
                  padding: const EdgeInsets.all(Look.cardPad),
                  child: Text(
                    room.owner ? '아직 게시한 대화가 없어요.' : '아직 공유된 대화가 없어요.',
                    style: dim,
                  ),
                ),
              for (final (i, s) in room.sessions.indexed)
                SettingsRow(
                  tone: i,
                  icon: Icons.forum_outlined,
                  title: s.displayTitle,
                  subtitle: [
                    _harness(s.harness),
                    archiveTime(s.updatedAt),
                    archiveSize(s.bytes),
                    if (!s.caughtUp) '올리는 중',
                    if (room.canFork(s)) '포크 가능',
                  ].join(' · '),
                  chevron: true,
                  onTap: () => Navigator.of(context).push(
                    MaterialPageRoute<void>(
                      settings: const RouteSettings(name: 'shared-session'),
                      builder: (_) => SharedSessionScreen(repository: _repo, roomId: room.id, sessionId: s.id),
                    ),
                  ),
                ),
            ],
          ),
          SettingsGroup(
            title: '참여자',
            children: [
              for (final (i, m) in room.members.indexed)
                SettingsRow(
                  tone: i,
                  icon: m.owner ? Icons.star_outline : Icons.person_outline,
                  title: m.label,
                  subtitle: m.owner ? '방 주인' : m.account == _repo.account.account ? '나' : '참여',
                  trailing: room.owner && !m.owner
                      ? IconButton(
                          tooltip: '내보내기',
                          onPressed: _busy ? null : () => _remove(room, m),
                          icon: const Icon(Icons.person_remove_outlined),
                        )
                      : null,
                ),
            ],
          ),
          if (room.owner)
            SettingsGroup(
              title: '초대',
              children: [
                SettingsRow(
                  icon: Icons.add_link,
                  title: '초대 코드 만들기',
                  subtitle: '24시간 동안 한 번 쓸 수 있어요. 코드는 만들 때 한 번만 보여요.',
                  onTap: _busy ? null : () => _invite(room),
                ),
                for (final (i, inv) in room.invites.indexed)
                  SettingsRow(
                    tone: i + 1,
                    icon: Icons.vpn_key_outlined,
                    title: inv.usable(now) ? '사용할 수 있는 초대' : '끝난 초대',
                    subtitle: '${inv.uses}/${inv.maxUses}번 사용 · 만료 ${archiveTime(inv.expiresAt)}',
                    trailing: IconButton(
                      tooltip: '초대 폐기',
                      onPressed: _busy ? null : () => _revoke(room, inv),
                      icon: const Icon(Icons.link_off),
                    ),
                  ),
              ],
            ),
          if (room.owner)
            SettingsGroup(
              title: '방 설정',
              children: [
                SettingsRow(icon: Icons.edit_outlined, title: '이름 바꾸기', subtitle: room.displayTitle, chevron: true, onTap: _busy ? null : () => _rename(room)),
                SettingsRow(
                  tone: 1,
                  icon: Icons.call_split,
                  title: '포크 허용',
                  subtitle: '참여자가 고정된 시점의 사본으로 자기 PC에 새 대화를 만들 수 있어요.',
                  trailing: Switch(
                    value: room.allowFork,
                    onChanged: _busy ? null : (v) => _do(() => _repo.setAllowFork(room, v)),
                  ),
                ),
                SettingsRow(icon: Icons.delete_outline, title: '공유 끝내기', danger: true, onTap: _busy ? null : () => _delete(room)),
              ],
            )
          else
            SettingsGroup(
              children: [SettingsRow(icon: Icons.logout, title: '이 방에서 나가기', danger: true, onTap: _busy ? null : () => _leave(room))],
            ),
        ],
      ),
    );
  }
}

/// 공유된 대화 하나 — PTY 없는 읽기 화면. 원본 칸에 붙지 않고 입력·승인·크기·파일 링크를 보내지 않는다.
class SharedSessionScreen extends StatefulWidget {
  const SharedSessionScreen({super.key, required this.repository, required this.roomId, required this.sessionId, this.forkHost});
  final SharedRoomsRepository repository;
  final String roomId, sessionId;

  /// 시험이 데스크톱 창구를 바꿔 끼운다.
  final ForkHost Function(Server)? forkHost;
  @override
  State<SharedSessionScreen> createState() => _SharedSessionScreenState();
}

class _SharedSessionScreenState extends State<SharedSessionScreen> with _CountedRoute {
  late final _reader = widget.repository.reader(widget.roomId, widget.sessionId);

  /// 어디에도 닿지 않는 닫힌 서버 — 대화 화면이 그림·링크·학생 얼굴을 원본 기기에 묻지 못하게.
  final _inert = Server(Uri.parse('https://shared-room.invalid/'))..close();
  late final Pane _pane = Pane(
    id: 'shared-${widget.sessionId}',
    name: '',
    title: '',
    status: 'idle',
    window: 0,
    cwd: '',
    harness: widget.repository.room(widget.roomId)?.session(widget.sessionId)?.harness ?? 'claude',
  );
  late final _session = TermSession(_inert, _pane)..pause();

  @override
  void dispose() {
    _reader.dispose();
    _session.dispose();
    super.dispose();
  }

  bool _scrolled(ScrollNotification n) {
    if (n.depth == 0 && n.metrics.axis == Axis.vertical) _reader.hold = n.metrics.pixels > 48;
    return false;
  }

  @override
  Widget build(BuildContext context) => ListenableBuilder(
    listenable: Listenable.merge([widget.repository, _reader]),
    builder: (context, _) {
      final repo = widget.repository;
      final room = repo.room(widget.roomId);
      final session = _reader.session ?? room?.session(widget.sessionId);
      final theme = Theme.of(context);
      final dim = theme.textTheme.bodySmall?.copyWith(color: theme.colorScheme.onSurfaceVariant);
      final gone = repo.locked ? _ended : _reader.gone;
      return Scaffold(
        appBar: AppBar(title: Text(session?.displayTitle ?? '공유된 대화', maxLines: 1, overflow: TextOverflow.ellipsis)),
        body: gone != null
            ? Center(child: Padding(padding: const EdgeInsets.all(Look.pagePad), child: Text(gone, textAlign: TextAlign.center)))
            : SharedRoomsWatch(
                every: const Duration(seconds: 4),
                onTick: () => unawaited(_reader.poll()),
                child: SafeArea(
                  child: Column(
                    crossAxisAlignment: CrossAxisAlignment.stretch,
                    children: [
                      Padding(
                        padding: const EdgeInsets.symmetric(horizontal: Look.pagePad, vertical: Look.groupTitleGap),
                        child: Text(
                          [
                            '공유 대화 · 읽기 전용',
                            if (room != null) room.displayTitle,
                            if (session != null) '갱신 ${archiveTime(session.updatedAt)}',
                            if (session != null && !session.caughtUp) '게시 기기에서 올리는 중',
                          ].join(' · '),
                          style: dim,
                        ),
                      ),
                      if (_reader.error != null)
                        Padding(
                          padding: const EdgeInsets.symmetric(horizontal: Look.pagePad),
                          child: ErrorBand(text: _reader.error!, onRetry: () => unawaited(_reader.poll())),
                        ),
                      if (_reader.fresh)
                        Padding(
                          padding: const EdgeInsets.symmetric(horizontal: Look.pagePad),
                          child: Row(
                            children: [
                              Expanded(child: Text('새 내용이 있어요. 읽던 자리는 그대로예요.', style: dim)),
                              TextButton(onPressed: _reader.showFresh, child: const Text('새 내용 보기')),
                            ],
                          ),
                        ),
                      Expanded(
                        child: _reader.loading
                            ? Column(
                                mainAxisAlignment: MainAxisAlignment.center,
                                children: [
                                  const TwinsLoading(label: '공유된 대화를 받는 중', size: Look.twinsSmall),
                                  Padding(
                                    padding: const EdgeInsets.symmetric(horizontal: Look.groupGap * 2),
                                    child: LinearProgressIndicator(value: _reader.progress, minHeight: 2),
                                  ),
                                ],
                              )
                            : NotificationListener<ScrollNotification>(
                                onNotification: _scrolled,
                                child: SelectionArea(
                                  child: ConversationView(
                                    server: _inert,
                                    pane: _pane,
                                    session: _session,
                                    accent: theme.colorScheme.primary,
                                    onTerminal: () {},
                                    conversation: _reader.conversation,
                                    readOnly: true,
                                    active: false,
                                  ),
                                ),
                              ),
                      ),
                      if (room != null && session != null) _forkBar(context, room, session, dim),
                    ],
                  ),
                ),
              ),
      );
    },
  );

  Widget _forkBar(BuildContext context, SharedRoom room, SharedSession session, TextStyle? dim) {
    if (room.canFork(session)) {
      return Padding(
        padding: const EdgeInsets.fromLTRB(Look.pagePad, Look.groupTitleGap, Look.pagePad, Look.groupTitleGap),
        child: FilledButton.icon(
          onPressed: () => showSharedForkSheet(context, widget.repository, room.id, session.id, host: widget.forkHost),
          icon: const Icon(Icons.call_split, size: Look.iconSize),
          label: const Text('내 계정으로 포크'),
        ),
      );
    }
    if (room.allowFork) {
      return Padding(
        padding: const EdgeInsets.all(Look.pagePad),
        child: Text('게시자가 포크 자료를 올리면 이 대화를 내 PC로 포크할 수 있어요.', style: dim),
      );
    }
    return const SizedBox.shrink();
  }
}

Future<void> showSharedForkSheet(
  BuildContext context,
  SharedRoomsRepository repository,
  String roomId,
  String sessionId, {
  ForkHost Function(Server)? host,
}) => showModalBottomSheet<void>(
  context: context,
  isScrollControlled: true,
  useSafeArea: true,
  builder: (_) => _ForkSheet(flow: SharedForkFlow(repository, roomId: roomId, sessionId: sessionId, host: host)),
);

class _ForkSheet extends StatefulWidget {
  const _ForkSheet({required this.flow});
  final SharedForkFlow flow;
  @override
  State<_ForkSheet> createState() => _ForkSheetState();
}

class _ForkSheetState extends State<_ForkSheet> {
  SharedForkFlow get _flow => widget.flow;
  ForkDestination? _pick;
  final _cwd = TextEditingController();
  bool _opening = false;

  /// 프로젝트 자료까지 가져온다. 대화만은 사람이 끌 때만.
  bool _project = true;

  @override
  void initState() {
    super.initState();
    _flow.addListener(_changed);
    unawaited(_flow.search());
  }

  void _changed() {
    if (_flow.phase == ForkPhase.choosing && _pick == null) {
      final ready = _flow.destinations.where((d) => d.ready).toList();
      if (ready.length == 1) _choose(ready.single);
    }
  }

  void _choose(ForkDestination d) {
    setState(() {
      _pick = d;
      _cwd.text = d.status.defaultCwd;
    });
  }

  @override
  void dispose() {
    _flow.removeListener(_changed);
    _flow.dispose();
    _cwd.dispose();
    super.dispose();
  }

  Future<void> _open(ForkAddress address) async {
    final server = _flow.server;
    if (server == null || _opening) return;
    setState(() => _opening = true);
    final route = '~${address.machineId}';
    try {
      for (var attempt = 0; attempt < 5; attempt++) {
        final panes = await server.panes(machine: route);
        if (!mounted || server.isClosed || _flow.repository.locked) return;
        final pane = panes
            .where((p) => p.id == address.surfaceId && p.machine == route && (address.surfaceKey.isEmpty || p.surfaceKey == address.surfaceKey))
            .firstOrNull;
        if (pane != null) {
          final nav = Navigator.of(context);
          nav.pop();
          unawaited(nav.push(MaterialPageRoute<void>(builder: (_) => TerminalScreen(server: server, pane: pane, initialView: PaneView.chat))));
          return;
        }
        await Future<void>.delayed(const Duration(seconds: 1));
        if (!mounted) return;
      }
      _snack(context, '새 대화를 만들었지만 아직 목록에 없어요. 허브에서 확인해 주세요.');
    } on ServerException catch (e) {
      _snack(context, e.message);
    } finally {
      if (mounted) setState(() => _opening = false);
    }
  }

  @override
  Widget build(BuildContext context) => ListenableBuilder(
    listenable: Listenable.merge([_flow, _flow.repository]),
    builder: (context, _) {
      final theme = Theme.of(context);
      final scheme = theme.colorScheme;
      final dim = theme.textTheme.bodySmall?.copyWith(color: scheme.onSurfaceVariant);
      final repo = _flow.repository;
      final room = repo.room(_flow.roomId);
      final session = room?.session(_flow.sessionId);
      final checkpoint = session?.checkpoint;
      final phase = _flow.phase;
      final busy = _flow.busy;
      final failed = phase == ForkPhase.failed;
      Widget body;
      if (repo.locked || room == null || session == null || checkpoint == null) {
        body = Text(repo.locked ? _ended : '이 대화는 지금 포크할 수 없어요.');
      } else {
        body = Column(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          mainAxisSize: MainAxisSize.min,
          children: [
            Text('원본은 그대로 두고, 고정된 시점의 사본으로 내 PC에 새 대화를 만들어요. 내 로그인과 권한 설정으로 실행돼요.', style: dim),
            const SizedBox(height: Look.groupTitleGap),
            Text(
              '기준 시점 ${archiveTime(checkpoint.capturedAt)}${checkpoint.boundaryLabel.isEmpty ? '' : ' · ${checkpoint.boundaryLabel}'}',
              style: theme.textTheme.bodyMedium,
            ),
            if (checkpoint.omissions.isNotEmpty) Text('빠진 것: ${checkpoint.omissions.join(', ')}', style: dim),
            if (phase == ForkPhase.searching)
              const Padding(padding: EdgeInsets.only(top: Look.fieldGap), child: LinearProgressIndicator(minHeight: 2)),
            if (_flow.destinations.isNotEmpty)
              SettingsGroup(
                title: '포크를 받을 내 PC',
                children: [
                  for (final (i, d) in _flow.destinations.indexed)
                    SettingsRow(
                      tone: i,
                      icon: Icons.computer_outlined,
                      title: d.label,
                      subtitle: d.ready ? '공유방 포크를 받을 수 있어요' : 'PC 카사텀에서 KASA 계정 로그인이 필요해요',
                      trailing: _pick?.status.machineId == d.status.machineId
                          ? Icon(Icons.check_circle, color: scheme.primary)
                          : null,
                      onTap: busy || phase == ForkPhase.done || !d.ready ? null : () => _choose(d),
                    ),
                ],
              ),
            if (_flow.outdated.isNotEmpty && _flow.destinations.isNotEmpty)
              Padding(
                padding: const EdgeInsets.only(top: Look.groupTitleGap),
                child: Text('업데이트가 필요한 PC: ${_flow.outdated.join(', ')}', style: dim),
              ),
            if (checkpoint.hasProject && _pick != null) ...[
              SwitchListTile(
                contentPadding: EdgeInsets.zero,
                title: const Text('프로젝트 자료도 가져오기'),
                subtitle: Text(_project ? '고른 폴더의 기존 저장소에 맞춰 별도 작업 위치에 풀어요.' : '대화만 가져와요. 프로젝트 파일은 받지 않아요.'),
                value: _project,
                onChanged: busy || phase == ForkPhase.done ? null : (v) => setState(() => _project = v),
              ),
            ],
            if (checkpoint.hasProject && _pick != null && _project) ...[
              LabeledField(
                label: '작업 폴더 (내 PC의 기존 프로젝트 폴더)',
                child: TextField(
                  controller: _cwd,
                  enabled: !busy && phase != ForkPhase.done,
                  autocorrect: false,
                  enableSuggestions: false,
                  style: const TextStyle(fontSize: 16),
                  decoration: InputDecoration(hintText: _pick!.status.defaultCwd.isEmpty ? '비우면 PC 기본 위치' : _pick!.status.defaultCwd),
                ),
              ),
              const SizedBox(height: Look.rowGap * 2),
              Text(
                '프로젝트 자료는 ${archiveTime(checkpoint.capturedAt)}에 캡처한 별도 자료예요. 고른 폴더의 지금 파일은 그 시점 파일이 아니고, 기존 파일을 덮지 않아요.',
                style: dim,
              ),
            ],
            if (_flow.message != null)
              Padding(
                padding: const EdgeInsets.only(top: Look.fieldGap),
                child: Text(_flow.message!, style: theme.textTheme.bodySmall?.copyWith(color: failed ? scheme.error : scheme.onSurfaceVariant)),
              ),
            const SizedBox(height: Look.fieldGap),
            if (phase == ForkPhase.done && _flow.result != null)
              FilledButton.icon(
                onPressed: _opening ? null : () => _open(_flow.result!),
                icon: const Icon(Icons.open_in_new, size: Look.iconSize),
                label: Text(_opening ? '여는 중' : '내 새 대화 열기'),
              )
            else if (repo.desktop != null)
              FilledButton.icon(
                onPressed: busy || _pick == null || !_pick!.ready
                    ? null
                    : () => _flow.start(_pick!, cwd: _project ? _cwd.text.trim() : '', includeProject: _project),
                icon: busy
                    ? const SizedBox.square(dimension: Look.iconSize, child: CircularProgressIndicator(strokeWidth: 2))
                    : const Icon(Icons.call_split, size: Look.iconSize),
                label: Text(failed ? '다시 시도' : '포크 시작'),
              ),
          ],
        );
      }
      return SafeArea(
        child: SingleChildScrollView(
          padding: EdgeInsets.fromLTRB(Look.pagePad, Look.pagePad, Look.pagePad, Look.pagePad + MediaQuery.viewInsetsOf(context).bottom),
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.stretch,
            mainAxisSize: MainAxisSize.min,
            children: [
              Text('내 계정으로 포크', style: theme.textTheme.titleLarge),
              const SizedBox(height: Look.groupTitleGap),
              body,
            ],
          ),
        ),
      );
    },
  );
}

Future<void> showPublishSheet(
  BuildContext context,
  SharedRoomsRepository repository,
  String roomId, {
  ForkHost Function(Server)? host,
}) => showModalBottomSheet<void>(
  context: context,
  isScrollControlled: true,
  useSafeArea: true,
  builder: (_) => _PublishSheet(repository: repository, roomId: roomId, host: host ?? ServerForkHost.new),
);

class _PublishSheet extends StatefulWidget {
  const _PublishSheet({required this.repository, required this.roomId, required this.host});
  final SharedRoomsRepository repository;
  final String roomId;
  final ForkHost Function(Server) host;
  @override
  State<_PublishSheet> createState() => _PublishSheetState();
}

/// 게시는 PC 가 원본과 파일을 읽어서 한다 — 폰은 그 PC 가 방금 준 완전한 주소를 고르기만 한다.
class _PublishSheetState extends State<_PublishSheet> {
  Server? _server;
  bool _loading = true, _busy = false, _project = false;
  List<(String, String?, List<PublishSource>)> _pcs = const [];
  List<String> _outdated = const [];
  String? _route, _message;
  bool _failed = false;
  final Set<String> _picked = {};

  bool get _current => mounted && !widget.repository.locked && _server != null && identical(widget.repository.desktop, _server);

  @override
  void initState() {
    super.initState();
    unawaited(_load());
  }

  Future<void> _load() async {
    _server = widget.repository.desktop;
    final server = _server;
    if (server == null) {
      setState(() {
        _loading = false;
        _message = '게시하려면 내 PC 연결이 필요해요. PC 카사텀을 켜고 같은 계정으로 로그인해 주세요.';
      });
      return;
    }
    setState(() {
      _loading = true;
      _message = null;
      _picked.clear();
    });
    final host = widget.host(server);
    final pcs = <(String, String?, List<PublishSource>)>[];
    final old = <String>[];
    final seen = <String>{};
    await Future.wait([
      for (final (label, route) in await myDesktops(server))
        host.sources(route).then((list) {
          final machine = list.firstOrNull?.address['machine_id'];
          if (machine is String && !seen.add(machine)) return;
          pcs.add((label, route, list));
        }).catchError((Object e) {
          if (e is ForkHostOutdated) old.add(label);
        }),
    ]);
    if (!_current) return;
    setState(() {
      _pcs = pcs;
      _outdated = old;
      _loading = false;
      _message = pcs.any((p) => p.$3.isNotEmpty)
          ? null
          : old.isNotEmpty
          ? const ForkHostOutdated().toString()
          : '게시할 수 있는 대화를 찾지 못했어요.';
    });
  }

  void _toggle(String? route, PublishSource s) => setState(() {
    if (_route != route) _picked.clear();
    _route = route;
    if (!_picked.remove(s.key)) _picked.add(s.key);
  });

  Future<void> _publish() async {
    final server = _server;
    final pc = _pcs.where((p) => p.$2 == _route).firstOrNull;
    if (server == null || pc == null || _picked.isEmpty || _busy) return;
    final addresses = [for (final s in pc.$3) if (_picked.contains(s.key) && s.ready) s.address];
    setState(() {
      _busy = true;
      _failed = false;
      _message = null;
    });
    try {
      await widget.host(server).publish(pc.$2, roomId: widget.roomId, addresses: addresses, includeProject: _project);
      if (!_current) return;
      unawaited(widget.repository.refreshRoom(widget.roomId).catchError((_) => null));
      setState(() {
        _picked.clear();
        _message = '${pc.$1}에서 게시를 시작했어요. PC가 대화를 올리면 목록에 나타나요.';
      });
    } catch (e) {
      if (!_current) return;
      setState(() {
        _failed = true;
        _message = e is ServerException ? e.message : '$e';
      });
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  @override
  Widget build(BuildContext context) => ListenableBuilder(
    listenable: widget.repository,
    builder: (context, _) {
      final theme = Theme.of(context);
      final scheme = theme.colorScheme;
      final dim = theme.textTheme.bodySmall?.copyWith(color: scheme.onSurfaceVariant);
      final locked = widget.repository.locked;
      return SafeArea(
        child: SingleChildScrollView(
          padding: const EdgeInsets.all(Look.pagePad),
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.stretch,
            mainAxisSize: MainAxisSize.min,
            children: [
              Text('PC 대화 게시하기', style: theme.textTheme.titleLarge),
              const SizedBox(height: Look.groupTitleGap),
              if (locked)
                const Text(_ended)
              else ...[
                Text('참여자는 읽기만 해요. 원본 터미널·입력은 공유되지 않아요.', style: dim),
                if (_loading) const Padding(padding: EdgeInsets.only(top: Look.fieldGap), child: LinearProgressIndicator(minHeight: 2)),
                for (final (label, route, sources) in _pcs)
                  if (sources.isNotEmpty)
                    SettingsGroup(
                      title: label,
                      children: [
                        for (final (i, s) in sources.indexed)
                          SettingsRow(
                            tone: i,
                            icon: Icons.forum_outlined,
                            title: s.displayTitle,
                            subtitle: [if (s.roomLabel.isNotEmpty) s.roomLabel, if (!s.ready) '아직 게시할 수 없어요'].join(' · '),
                            trailing: Checkbox(
                              value: _route == route && _picked.contains(s.key),
                              onChanged: _busy || !s.ready ? null : (_) => _toggle(route, s),
                            ),
                            onTap: _busy || !s.ready ? null : () => _toggle(route, s),
                          ),
                      ],
                    ),
                if (_outdated.isNotEmpty && _pcs.isNotEmpty)
                  Padding(
                    padding: const EdgeInsets.only(top: Look.groupTitleGap),
                    child: Text('업데이트가 필요한 PC: ${_outdated.join(', ')}', style: dim),
                  ),
                if (_pcs.any((p) => p.$3.isNotEmpty))
                  SwitchListTile(
                    contentPadding: EdgeInsets.zero,
                    title: const Text('프로젝트 자료도 함께'),
                    subtitle: const Text('포크하는 사람이 같은 시점의 프로젝트 변경을 받을 수 있어요. 자격 파일은 넣지 않아요.'),
                    value: _project,
                    onChanged: _busy ? null : (v) => setState(() => _project = v),
                  ),
                if (_message != null)
                  Padding(
                    padding: const EdgeInsets.only(top: Look.groupTitleGap),
                    child: Text(_message!, style: theme.textTheme.bodySmall?.copyWith(color: _failed ? scheme.error : scheme.onSurfaceVariant)),
                  ),
                const SizedBox(height: Look.fieldGap),
                if (_server != null)
                  FilledButton.icon(
                    onPressed: _busy || _picked.isEmpty ? null : _publish,
                    icon: _busy
                        ? const SizedBox.square(dimension: Look.iconSize, child: CircularProgressIndicator(strokeWidth: 2))
                        : const Icon(Icons.upload_outlined, size: Look.iconSize),
                    label: Text(_picked.isEmpty ? '게시할 대화를 골라 주세요' : '대화 ${_picked.length}개 게시'),
                  ),
              ],
            ],
          ),
        ),
      );
    },
  );
}

class _JoinDialog extends StatefulWidget {
  const _JoinDialog({required this.repository});
  final SharedRoomsRepository repository;
  @override
  State<_JoinDialog> createState() => _JoinDialogState();
}

class _JoinDialogState extends State<_JoinDialog> {
  final _code = TextEditingController();
  bool _busy = false;
  String? _error;

  @override
  void dispose() {
    _code.dispose();
    super.dispose();
  }

  Future<void> _submit() async {
    final token = parseInviteToken(_code.text);
    if (token == null) {
      setState(() => _error = '초대 코드 모양이 맞지 않아요. 받은 코드를 그대로 붙여 넣어 주세요.');
      return;
    }
    setState(() {
      _busy = true;
      _error = null;
    });
    try {
      final room = await widget.repository.accept(token);
      if (mounted) Navigator.pop(context, room?.id ?? '');
    } on SharedRoomsException catch (e) {
      if (mounted) setState(() => _error = e.message);
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  @override
  Widget build(BuildContext context) => ModalLook(
    child: AlertDialog(
      title: const Text('초대 코드로 참여'),
      content: Column(
        mainAxisSize: MainAxisSize.min,
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          Text('지금 로그인한 ${widget.repository.account.label} 계정으로 참여해요. 참여해도 원본 터미널을 조작할 수 없어요.'),
          const SizedBox(height: Look.fieldGap),
          TextField(
            controller: _code,
            autofocus: true,
            enabled: !_busy,
            autocorrect: false,
            enableSuggestions: false,
            style: const TextStyle(fontSize: 16),
            decoration: const InputDecoration(hintText: '초대 코드'),
            onSubmitted: (_) => _submit(),
          ),
          if (_error != null) ...[
            const SizedBox(height: Look.groupTitleGap),
            Text(_error!, style: TextStyle(color: Theme.of(context).colorScheme.error)),
          ],
        ],
      ),
      actions: [
        TextButton(onPressed: _busy ? null : () => Navigator.pop(context), child: const Text('취소')),
        FilledButton(onPressed: _busy ? null : _submit, child: Text(_busy ? '참여 중' : '참여')),
      ],
    ),
  );
}

/// 새 공유방 만들기·이름 바꾸기. 방을 만들어도 셸·창은 생기지 않는다.
class _RoomDialog extends StatefulWidget {
  const _RoomDialog({required this.repository, this.room});
  final SharedRoomsRepository repository;
  final SharedRoom? room;
  @override
  State<_RoomDialog> createState() => _RoomDialogState();
}

class _RoomDialogState extends State<_RoomDialog> {
  late final _title = TextEditingController(text: widget.room?.title ?? '');

  /// 이 폼의 표식 — 답을 못 받고 다시 눌러도 관문이 같은 방을 돌려준다.
  final _requestId = sharedRequestId();
  bool _allowFork = false, _busy = false;
  String? _error;

  @override
  void dispose() {
    _title.dispose();
    super.dispose();
  }

  Future<void> _submit() async {
    final title = _title.text.trim();
    if (title.isEmpty) {
      setState(() => _error = '방 이름을 적어 주세요.');
      return;
    }
    setState(() {
      _busy = true;
      _error = null;
    });
    try {
      final room = widget.room == null
          ? await widget.repository.create(title, allowFork: _allowFork, requestId: _requestId)
          : await widget.repository.rename(widget.room!, title);
      if (mounted) Navigator.pop(context, room.id);
    } on SharedRoomsException catch (e) {
      if (mounted) setState(() => _error = e.message);
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  @override
  Widget build(BuildContext context) => ModalLook(
    child: AlertDialog(
      title: Text(widget.room == null ? '새 공유방' : '공유방 이름'),
      content: Column(
        mainAxisSize: MainAxisSize.min,
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          TextField(
            controller: _title,
            autofocus: true,
            enabled: !_busy,
            maxLength: 80,
            style: const TextStyle(fontSize: 16),
            decoration: const InputDecoration(hintText: '방 이름'),
            onSubmitted: (_) => _submit(),
          ),
          if (widget.room == null)
            SwitchListTile(
              contentPadding: EdgeInsets.zero,
              title: const Text('포크 허용'),
              subtitle: const Text('참여자가 내 대화의 고정 사본으로 자기 PC에 새 대화를 만들 수 있어요.'),
              value: _allowFork,
              onChanged: _busy ? null : (v) => setState(() => _allowFork = v),
            ),
          if (_error != null) Text(_error!, style: TextStyle(color: Theme.of(context).colorScheme.error)),
        ],
      ),
      actions: [
        TextButton(onPressed: _busy ? null : () => Navigator.pop(context), child: const Text('취소')),
        FilledButton(onPressed: _busy ? null : _submit, child: Text(widget.room == null ? '만들기' : '저장')),
      ],
    ),
  );
}

/// 방금 만든 초대 코드. 관문은 원문을 다시 주지 않는다 — 계정이 바뀌면 곧바로 가린다.
class _InviteDialog extends StatelessWidget {
  const _InviteDialog({required this.repository, required this.invite});
  final SharedRoomsRepository repository;
  final SharedInvite invite;

  @override
  Widget build(BuildContext context) => ListenableBuilder(
    listenable: repository,
    builder: (context, _) {
      final hidden = repository.locked || repository.gone.containsKey(invite.roomId);
      return ModalLook(
        child: AlertDialog(
          title: const Text('초대 코드'),
          content: hidden
              ? const Text(_ended)
              : Column(
                  mainAxisSize: MainAxisSize.min,
                  crossAxisAlignment: CrossAxisAlignment.stretch,
                  children: [
                    Text('이 코드는 지금만 보여요. ${archiveTime(invite.expiresAt)}까지 ${invite.maxUses}번 쓸 수 있어요. 받은 사람은 카사모바일·카사텀의 공유방에서 참여해요.'),
                    const SizedBox(height: Look.fieldGap),
                    SelectableText(
                      invite.token,
                      style: const TextStyle(fontFamily: 'TermMono', fontFamilyFallback: Look.flowMonoFallback, fontSize: 16),
                    ),
                  ],
                ),
          actions: [
            TextButton(onPressed: () => Navigator.pop(context), child: const Text('닫기')),
            if (!hidden)
              FilledButton.icon(
                onPressed: () async {
                  await Clipboard.setData(ClipboardData(text: invite.token));
                  if (context.mounted) _snack(context, '초대 코드를 복사했어요.');
                },
                icon: const Icon(Icons.copy, size: Look.iconSize),
                label: const Text('복사'),
              ),
          ],
        ),
      );
    },
  );
}
