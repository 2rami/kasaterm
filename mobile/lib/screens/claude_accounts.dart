import 'dart:async';
import 'dart:math';

import 'package:flutter/material.dart';
import 'package:url_launcher/url_launcher.dart';

import '../claude_login.dart';
import '../look.dart';
import '../machine_look.dart';
import '../server.dart';
import '../twins_loading.dart';
import 'controls.dart';

class _Device {
  const _Device(this.id, this.label, this.route, this.online);
  final String id;
  final String label;
  final String? route;
  final bool online;
}

// A timed-out start may already own a server lease, including after this screen closes.
final _pendingStarts = Expando<_PendingStart>();

class _PendingStart {
  _PendingStart(this.server, this.device, this.slot, this.nonce);

  final Server server;
  final _Device device;
  final ClaudeAccount slot;
  final String nonce;
  ClaudePhoneLogin? flow;
  bool abandoned = false;
  bool settled = false;
  bool uncertain = false;
  Future<ClaudePhoneLogin>? _cancelling;
  final _cancelled = <String, ClaudePhoneLogin>{};

  void release() {
    settled = true;
    if (identical(_pendingStarts[server], this)) _pendingStarts[server] = null;
  }

  void take(ClaudePhoneLogin next) {
    flow = next;
    if (next.state.terminal) release();
  }

  Future<ClaudePhoneLogin> start() async {
    if (flow != null) return flow!;
    final next = await server.claudePhoneLogin(
      action: 'start',
      id: slot.id,
      label: nonce,
      machineId: device.id,
      slotId: slot.id,
      machine: device.route,
      // The screen limits its wait; the original reply must still be observed for cleanup.
      timeout: null,
    );
    if (!settled) take(next);
    if (abandoned || settled) {
      unawaited(cancel(next).then<void>((_) {}, onError: (_) {}));
    }
    return next;
  }

  Future<ClaudePhoneLogin> cancel([ClaudePhoneLogin? known]) {
    abandoned = true;
    if (flow == null) uncertain = true;
    return _cancelling ??= _cancel(known)
        .catchError((Object error) {
          if (error is ServerException &&
              const {
                'phone_login_unknown',
                'phone_login_expired',
              }.contains(error.code)) {
            release();
          }
          throw error;
        })
        .whenComplete(() => _cancelling = null);
  }

  Future<ClaudePhoneLogin> _cancel(ClaudePhoneLogin? known) async {
    final current =
        known ?? flow ?? await start().timeout(const Duration(seconds: 20));
    final cancelled = _cancelled[current.flowId];
    if (cancelled != null) return cancelled;
    final next = current.state.terminal
        ? current
        : await server.claudePhoneLogin(
            action: 'cancel',
            id: current.flowId,
            machineId: device.id,
            slotId: slot.id,
            machine: device.route,
          );
    if (next.state.terminal) _cancelled[current.flowId] = next;
    take(next);
    return next;
  }
}

class ClaudeAccountsScreen extends StatefulWidget {
  const ClaudeAccountsScreen({
    super.key,
    required this.server,
    this.open,
    this.every = const Duration(seconds: 2),
  });

  final Server server;
  final Future<bool> Function(Uri)? open;
  final Duration every;

  @override
  State<ClaudeAccountsScreen> createState() => _ClaudeAccountsScreenState();
}

class _ClaudeAccountsScreenState extends State<ClaudeAccountsScreen>
    with WidgetsBindingObserver {
  final _code = TextEditingController();
  List<_Device> _devices = const [];
  List<ClaudeAccount> _accounts = const [];
  _Device? _device;
  ClaudeAccount? _slot;
  ClaudePhoneLogin? _flow;
  _PendingStart? _pendingStart;
  String? _error;
  final _unconfirmedPrevious = <_PendingStart>{};
  bool _loading = true;
  bool _busy = false;
  bool _polling = false;
  bool _foreground = true;
  bool _disconnected = false;
  int _generation = 0;
  int _operation = 0;
  Timer? _timer;

  Server get _server => widget.server;
  bool get _active => _flow != null && !_flow!.state.terminal;
  bool get _pending => _pendingStart != null && !_pendingStart!.settled;
  bool get _locked => _active || _pending;
  bool _current(int generation) =>
      mounted &&
      generation == _generation &&
      !_server.isClosed &&
      !_disconnected;

  @override
  void initState() {
    super.initState();
    WidgetsBinding.instance.addObserver(this);
    _foreground =
        WidgetsBinding.instance.lifecycleState == null ||
        WidgetsBinding.instance.lifecycleState == AppLifecycleState.resumed;
    _server.addCloseListener(_closed);
    if (_server.isClosed) {
      _closed();
    } else if (_restorePending()) {
      _loading = false;
    } else {
      unawaited(_load());
    }
  }

  @override
  void didUpdateWidget(ClaudeAccountsScreen oldWidget) {
    super.didUpdateWidget(oldWidget);
    if (oldWidget.server == _server) return;
    oldWidget.server.removeCloseListener(_closed);
    final previous = _pendingStart;
    _abandon(oldWidget.server);
    _generation++;
    _timer?.cancel();
    _code.clear();
    _devices = const [];
    _accounts = const [];
    _device = null;
    _slot = null;
    _flow = null;
    _pendingStart = null;
    _busy = false;
    _polling = false;
    _disconnected = false;
    if (previous != null && !previous.settled) {
      _unconfirmedPrevious.add(previous);
      if (!previous.server.isClosed) {
        unawaited(
          previous
              .cancel()
              .whenComplete(() {
                if (mounted && previous.settled) {
                  setState(() => _unconfirmedPrevious.remove(previous));
                }
              })
              .then<void>((_) {}, onError: (_) {}),
        );
      }
    }
    _server.addCloseListener(_closed);
    if (_server.isClosed) {
      _closed();
    } else if (_restorePending()) {
      _loading = false;
    } else {
      unawaited(_load());
    }
  }

  bool _restorePending() {
    final pending = _pendingStarts[_server];
    if (pending == null || pending.settled) return false;
    _pendingStart = pending;
    _device = pending.device;
    _devices = [pending.device];
    _slot = pending.slot;
    _accounts = [pending.slot];
    _error = _uncertainStart;
    if (pending.abandoned) {
      final generation = _generation;
      scheduleMicrotask(() {
        if (_current(generation) && identical(_pendingStart, pending)) {
          unawaited(_request('cancel', pending.slot.id));
        }
      });
    }
    return true;
  }

  static const _uncertainStart =
      '로그인 요청의 처리 여부를 확인하지 못했어요. 연결을 확인하고 같은 요청을 다시 확인하거나 취소해 주세요. 복구하지 못하면 서버 요청이 최대 10분 동안 남을 수 있어요.';

  void _closed() {
    _generation++;
    _timer?.cancel();
    _code.clear();
    if (!mounted) return;
    setState(() {
      _disconnected = true;
      _busy = false;
      _loading = false;
      _error = _locked
          ? '연결이 바뀌었어요. 로그인 취소 여부를 확인하지 못했어요. 닫힌 연결에서는 같은 요청을 다시 확인할 수 없어요. 서버 요청이 끝나거나 최대 10분 후 만료된 뒤 다시 시작해 주세요.'
          : '연결이 바뀌었어요. 돌아가서 계정 목록을 다시 열어 주세요.';
    });
  }

  void _abandon(Server server) {
    final pending = _pendingStart;
    if (pending == null || pending.settled || pending.server != server) return;
    pending.abandoned = true;
    if (server.isClosed) return;
    unawaited(pending.cancel().then<void>((_) {}, onError: (_) {}));
  }

  @override
  void dispose() {
    _generation++;
    _timer?.cancel();
    _server.removeCloseListener(_closed);
    _abandon(_server);
    WidgetsBinding.instance.removeObserver(this);
    _code.dispose();
    super.dispose();
  }

  @override
  void didChangeAppLifecycleState(AppLifecycleState state) {
    _foreground = state == AppLifecycleState.resumed;
    _timer?.cancel();
    if (_foreground) unawaited(_poll());
  }

  Future<void> _load() async {
    if (_disconnected || _locked || _busy) return;
    final generation = ++_generation;
    setState(() {
      _loading = true;
      _polling = false;
      _accounts = const [];
      _error = null;
    });
    try {
      final values = await Future.wait<Object?>([
        _server.machineId(),
        _server.me(),
        _server.machines(),
      ]).timeout(const Duration(seconds: 15));
      if (!_current(generation)) return;
      final rootId = values[0] as String?;
      final me = values[1] as Me;
      final machines = values[2] as List<Machine>;
      final devices = <_Device>[
        if (rootId != null)
          _Device(
            rootId,
            me.machine ?? '기준 기기',
            _server.account == null ? null : '~$rootId',
            true,
          ),
      ];
      for (final machine in machines) {
        if (!machine.route.startsWith('~') || machine.route.length < 2) {
          continue;
        }
        final id = machine.route.substring(1);
        if (devices.any((d) => d.id == id)) continue;
        devices.add(
          _Device(
            id,
            machine.label.isEmpty ? id : machine.label,
            machine.route,
            machine.online,
          ),
        );
      }
      if (devices.isEmpty) {
        throw const ServerException(
          '기기 식별자를 확인하지 못했어요. 연결할 기기의 KASA를 업데이트해 주세요.',
        );
      }
      final device =
          devices.where((d) => d.id == _device?.id).firstOrNull ??
          devices.first;
      setState(() {
        _devices = devices;
        if (_device?.id != device.id) {
          _pendingStart = null;
          _slot = null;
          _flow = null;
        }
        _device = device;
      });
      await _loadAccounts(device, generation);
    } on ServerException catch (e) {
      if (_current(generation)) {
        setState(() {
          _error = e.message;
          _loading = false;
        });
      }
    } on TimeoutException {
      if (_current(generation)) {
        setState(() {
          _error = '기기에 닿지 못했어요. 연결을 확인하고 다시 시도해 주세요.';
          _loading = false;
        });
      }
    }
  }

  Future<void> _loadAccounts(_Device device, int generation) async {
    try {
      if (!device.online) {
        throw const ServerException(
          '이 기기는 연결되어 있지 않아요. 기기의 KASA를 켠 뒤 새로고침해 주세요.',
        );
      }
      final accounts = await _server.claudeAccounts(machine: device.route);
      if (!_current(generation)) return;
      setState(() {
        _accounts = accounts;
        _loading = false;
      });
    } on ServerException catch (e) {
      if (_current(generation)) {
        setState(() {
          _error = switch (e.status) {
            400 || 404 => '이 기기의 KASA를 새 판으로 업데이트해 주세요.',
            401 || 403 => '접근할 수 없어요. KASA 계정과 기기 연결을 확인해 주세요.',
            502 || 503 || 504 => '이 기기에 연결하지 못했어요. KASA를 켠 뒤 새로고침해 주세요.',
            _ => e.message,
          };
          _loading = false;
        });
      }
    }
  }

  String _name(_Device device) =>
      machineName(device.label, route: '~${device.id}');

  Future<void> _chooseDevice() async {
    final device = await showModalBottomSheet<_Device>(
      context: context,
      builder: (context) => SafeArea(
        child: ListView(
          shrinkWrap: true,
          children: [
            for (final device in _devices)
              SettingsRow(
                icon: machineIcon(device.label),
                title: _name(device),
                subtitle: device.online ? '연결됨' : '연결 안 됨',
                trailing: device.id == _device?.id
                    ? const Icon(Icons.check_rounded)
                    : null,
                onTap: () => Navigator.pop(context, device),
              ),
          ],
        ),
      ),
    );
    if (!mounted || device == null || _locked || _busy || _disconnected) return;
    final generation = ++_generation;
    setState(() {
      _device = device;
      _accounts = const [];
      _flow = null;
      _slot = null;
      _pendingStart = null;
      _error = null;
      _loading = true;
    });
    await _loadAccounts(device, generation);
  }

  String _newNonce() {
    final bytes = List<int>.generate(16, (_) => Random.secure().nextInt(256));
    bytes[6] = (bytes[6] & 15) | 64;
    bytes[8] = (bytes[8] & 63) | 128;
    final hex = bytes.map((b) => b.toRadixString(16).padLeft(2, '0')).join();
    return '${hex.substring(0, 8)}-${hex.substring(8, 12)}-${hex.substring(12, 16)}-${hex.substring(16, 20)}-${hex.substring(20)}';
  }

  Future<void> _start(ClaudeAccount slot) async {
    if (_busy || _active || _disconnected) return;
    if (_pending && _pendingStart!.slot.id != slot.id) return;
    if (_pending && _pendingStart!.abandoned) return _cancelPending();
    final device = _device;
    if (device == null || _server.isClosed) return;
    if (!_pending) {
      _pendingStart = _PendingStart(_server, device, slot, _newNonce());
    }
    _pendingStarts[_server] = _pendingStart;
    setState(() {
      _slot = slot;
      _flow = null;
    });
    await _request('start', slot.id);
  }

  Future<void> _cancelPending() async {
    if (_busy || _disconnected || !_pending) return;
    await _request('cancel', _slot!.id);
  }

  Future<void> _request(String action, String id, {String? label}) async {
    final device = _device;
    final slot = _slot;
    if (_busy ||
        device == null ||
        slot == null ||
        _disconnected ||
        _server.isClosed) {
      return;
    }
    final generation = _generation;
    final server = _server;
    final pending = _pendingStart;
    _operation++;
    _timer?.cancel();
    setState(() {
      _busy = true;
      _error = null;
    });
    try {
      final flow = action == 'start'
          ? await pending!.start().timeout(const Duration(seconds: 20))
          : action == 'cancel' && pending != null
          ? await pending.cancel()
          : await server.claudePhoneLogin(
              action: action,
              id: id,
              label: label,
              machineId: device.id,
              slotId: slot.id,
              machine: device.route,
            );
      if (!_current(generation)) return;
      setState(() => _take(flow));
    } on ServerException catch (e) {
      if (action == 'start' && e.code != null && !pending!.uncertain) {
        pending.release();
        if (_current(generation)) _pendingStart = null;
      }
      if (action == 'start' && e.code == null) pending?.uncertain = true;
      if (_current(generation)) {
        setState(() {
          if (action == 'cancel' && pending?.settled == true) {
            _pendingStart = null;
            _flow = null;
            _slot = null;
          }
          _error = _pending && _flow == null ? _uncertainStart : e.message;
        });
      }
    } on TimeoutException {
      pending?.uncertain = true;
      if (_current(generation)) setState(() => _error = _uncertainStart);
    } finally {
      if (_current(generation)) {
        setState(() => _busy = false);
        _schedule();
      }
    }
  }

  void _schedule() {
    _timer?.cancel();
    if (!mounted ||
        !_foreground ||
        !_active ||
        _busy ||
        _polling ||
        _disconnected) {
      return;
    }
    _timer = Timer(widget.every, () => unawaited(_poll()));
  }

  Future<void> _poll() async {
    if (!_foreground || !_active || _busy || _polling || _disconnected) return;
    final generation = _generation;
    final operation = _operation;
    final flow = _flow!;
    final device = _device!;
    _polling = true;
    _timer?.cancel();
    try {
      final next = await _server.claudePhoneLogin(
        action: 'status',
        id: flow.flowId,
        machineId: device.id,
        slotId: flow.slotId,
        machine: device.route,
      );
      if (_current(generation) && operation == _operation) {
        setState(() {
          _error = null;
          _take(next);
        });
      }
    } on ServerException catch (e) {
      if (_current(generation) && operation == _operation) {
        setState(() => _error = e.message);
      }
    } finally {
      if (_current(generation)) {
        _polling = false;
        _schedule();
      }
    }
  }

  void _take(ClaudePhoneLogin flow) {
    _flow = flow;
    _pendingStart?.take(flow);
    if (flow.state.terminal) {
      _pendingStart = null;
      _code.clear();
    }
  }

  Future<void> _openBrowser() async {
    final uri = _flow?.authorizationUrl;
    if (uri == null || _busy || _disconnected || _error != null) return;
    final generation = _generation;
    bool opened;
    try {
      opened =
          await (widget.open?.call(uri) ??
              launchUrl(uri, mode: LaunchMode.externalApplication));
    } catch (_) {
      opened = false;
    }
    if (_current(generation) && !opened) {
      setState(() => _error = 'Safari를 열지 못했어요. 다시 시도해 주세요.');
    }
  }

  Future<void> _submit() async {
    final flow = _flow;
    final code = _code.text.trim();
    if (flow == null || code.isEmpty || _busy || _error != null) return;
    _code.clear();
    FocusManager.instance.primaryFocus?.unfocus();
    await _request('code', flow.flowId, label: code);
  }

  void _finish() {
    if (_busy || _locked) return;
    setState(() {
      _flow = null;
      _slot = null;
      _pendingStart = null;
      _error = null;
    });
    unawaited(_load());
  }

  @override
  Widget build(BuildContext context) => TwinBackdrop(
    child: Scaffold(
      backgroundColor: Colors.transparent,
      appBar: AppBar(
        backgroundColor: Colors.transparent,
        title: const Text('Claude 계정'),
        actions: [
          IconButton(
            tooltip: '새로고침',
            onPressed: _loading || _busy || _locked || _disconnected
                ? null
                : _load,
            icon: const Icon(Icons.refresh_rounded),
          ),
        ],
      ),
      body: SafeArea(top: false, child: _body()),
    ),
  );

  Widget _body() {
    if (_loading && _device == null) {
      return const Center(
        child: TwinsLoading(label: '계정 확인', size: Look.twinsSmall),
      );
    }
    if (_device == null) {
      return Center(
        child: TwinsNotice(
          text: _error ?? '기기를 찾지 못했어요.',
          action: FilledButton(
            onPressed: _disconnected
                ? () => Navigator.maybePop(context)
                : _load,
            child: Text(_disconnected ? '돌아가기' : '다시 시도'),
          ),
        ),
      );
    }
    final device = _device!;
    final flow = _flow;
    return ListView(
      keyboardDismissBehavior: ScrollViewKeyboardDismissBehavior.onDrag,
      padding: const EdgeInsets.fromLTRB(
        Look.pagePad,
        0,
        Look.pagePad,
        Look.groupGap,
      ),
      children: [
        SettingsGroup(
          children: [
            SettingsRow(
              key: const Key('claude-device'),
              icon: machineIcon(device.label),
              title: _name(device),
              subtitle: _locked ? '로그인할 기기 · 진행 중에는 고정돼요' : '로그인할 기기',
              chevron:
                  _devices.length > 1 && !_locked && !_busy && !_disconnected,
              onTap: _devices.length > 1 && !_locked && !_busy && !_disconnected
                  ? _chooseDevice
                  : null,
            ),
          ],
        ),
        if (_unconfirmedPrevious.isNotEmpty)
          const ErrorBand(
            text:
                '이전 연결의 로그인 취소 여부를 확인하지 못했어요. 복구하지 못하면 서버 요청이 최대 10분 동안 남을 수 있어요.',
          ),
        if (_error != null)
          ErrorBand(
            text: _error!,
            onRetry: _disconnected || _busy
                ? null
                : _active
                ? _poll
                : _pending
                ? () => _start(_slot!)
                : _load,
          ),
        if (_pending && flow == null)
          TextButton(
            key: const Key('claude-cancel-pending'),
            onPressed: _busy || _disconnected ? null : _cancelPending,
            child: const Text('로그인 요청 취소'),
          ),
        if (flow != null)
          _login(flow)
        else if (_loading)
          const Padding(
            padding: EdgeInsets.all(Look.groupGap),
            child: TwinsLoading(label: '계정 확인', size: Look.twinsSmall),
          )
        else if (_accounts.isEmpty && _error == null)
          Padding(
            padding: const EdgeInsets.only(top: Look.groupGap),
            child: TwinsNotice(
              text:
                  '폰에서 다시 로그인할 등록 계정이 없어요. 기본 계정만 있다면 이 기기의 KASA에서 계정 슬롯을 먼저 추가해 주세요.',
              action: FilledButton(
                onPressed: _load,
                child: const Text('다시 확인'),
              ),
            ),
          )
        else if (_accounts.isNotEmpty)
          SettingsGroup(
            title: '등록된 계정',
            children: [
              for (final account in _accounts)
                SettingsRow(
                  key: Key('claude-account-${account.id}'),
                  icon: Icons.person_outline_rounded,
                  title: account.title,
                  subtitle: account.detail,
                  below: Align(
                    alignment: Alignment.centerLeft,
                    child: OutlinedButton(
                      key: Key('claude-login-${account.id}'),
                      onPressed:
                          _busy ||
                              _disconnected ||
                              (_pending && _slot?.id != account.id)
                          ? null
                          : () => _start(account),
                      child: Text(
                        _busy && _slot?.id == account.id
                            ? '시작하는 중'
                            : _pending && _slot?.id == account.id
                            ? _pendingStart!.abandoned
                                  ? '취소 다시 확인'
                                  : '로그인 상태 확인'
                            : '다시 로그인',
                      ),
                    ),
                  ),
                ),
            ],
          ),
      ],
    );
  }

  Widget _login(ClaudePhoneLogin flow) {
    final theme = Theme.of(context);
    final waiting =
        flow.state == ClaudeLoginState.awaitingAuthorization ||
        flow.state == ClaudeLoginState.awaitingCode;
    final ready = !_busy && !_disconnected && _error == null;
    final status = switch (flow.state) {
      ClaudeLoginState.starting => '로그인 준비 중',
      ClaudeLoginState.awaitingAuthorization || ClaudeLoginState.awaitingCode =>
        'Safari에서 Claude에 로그인해 주세요. 승인 화면의 인증코드를 아래에 붙여 넣어 주세요.',
      ClaudeLoginState.verifying => '계정을 확인하고 이 기기에 저장하는 중이에요.',
      ClaudeLoginState.needsConfirmation =>
        '이 슬롯의 기존 계정을 확인할 수 없어요. 아래 계정이 맞는지 확인한 뒤 저장해 주세요.',
      ClaudeLoginState.succeeded =>
        flow.runtimeUpdated == false
            ? flow.runtimeErrorCode == 'runtime_update_pending'
                  ? '이 기기의 계정 슬롯에 로그인을 저장했어요. 작업대가 바뀌거나 갱신에 실패해 현재 칸은 그대로예요.'
                  : '이 기기의 계정 슬롯에 로그인을 저장했어요. 현재 선택된 계정과 실행 중인 칸은 그대로예요.'
            : '이 기기에 다시 로그인했어요.',
      ClaudeLoginState.failed => claudeLoginError(flow.errorCode),
      ClaudeLoginState.cancelled => '로그인을 취소했어요.',
      ClaudeLoginState.expired => '로그인 시간이 지났어요. 다시 시작해 주세요.',
    };
    return SettingsGroup(
      title: '선택한 계정',
      children: [
        SettingsRow(
          key: const Key('claude-login-flow'),
          icon: Icons.person_outline_rounded,
          title: _slot!.title,
          subtitle: [
            if (_slot!.name.isNotEmpty && _slot!.name != _slot!.title)
              _slot!.name,
            '슬롯 ${flow.slotId}',
          ].join(' · '),
          below: Column(
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: [
              Semantics(
                liveRegion: true,
                child: Text(
                  status,
                  key: const Key('claude-login-status'),
                  style: theme.textTheme.bodyMedium,
                ),
              ),
              if (!flow.state.terminal) ...[
                const SizedBox(height: Look.fieldGap),
                Text(
                  '유효 시간 ${MaterialLocalizations.of(context).formatTimeOfDay(TimeOfDay.fromDateTime(DateTime.fromMillisecondsSinceEpoch(flow.expiresAtMs)))}까지',
                  style: theme.textTheme.bodySmall?.copyWith(
                    color: theme.colorScheme.onSurfaceVariant,
                  ),
                ),
              ],
              if (waiting && flow.authorizationUrl != null) ...[
                const SizedBox(height: Look.fieldGap),
                FilledButton(
                  key: const Key('claude-open-browser'),
                  onPressed: ready ? _openBrowser : null,
                  child: const Text('Safari에서 로그인'),
                ),
              ],
              if (waiting) ...[
                const SizedBox(height: Look.groupGap),
                LabeledField(
                  label: '승인 화면의 인증코드',
                  child: TextField(
                    key: const Key('claude-code'),
                    controller: _code,
                    enabled: ready,
                    style: theme.textTheme.bodyLarge?.copyWith(
                      fontSize: Look.group,
                    ),
                    autocorrect: false,
                    enableSuggestions: false,
                    enableIMEPersonalizedLearning: false,
                    obscureText: true,
                    keyboardType: TextInputType.visiblePassword,
                    textInputAction: TextInputAction.done,
                    autofillHints: const [],
                    decoration: const InputDecoration(hintText: '인증코드 붙여넣기…'),
                    onSubmitted: (_) => _submit(),
                  ),
                ),
                const SizedBox(height: Look.fieldGap),
                OutlinedButton(
                  key: const Key('claude-submit-code'),
                  onPressed: ready ? _submit : null,
                  child: Text(_busy ? '확인하는 중' : '인증코드 보내기'),
                ),
              ],
              if (flow.state == ClaudeLoginState.needsConfirmation) ...[
                const SizedBox(height: Look.fieldGap),
                Text(
                  flow.email.isEmpty ? '이메일 확인 안 됨' : flow.email,
                  key: const Key('claude-confirm-email'),
                  style: theme.textTheme.titleSmall,
                ),
                if (flow.org.isNotEmpty)
                  Text(flow.org, style: theme.textTheme.bodySmall),
                if (flow.expectedEmail?.isNotEmpty ?? false)
                  Text(
                    '기존 이메일 ${flow.expectedEmail}',
                    style: theme.textTheme.bodySmall,
                  ),
                if (flow.expectedOrg?.isNotEmpty ?? false)
                  Text(
                    '기존 조직 ${flow.expectedOrg}',
                    style: theme.textTheme.bodySmall,
                  ),
                if (flow.accountUuid?.isNotEmpty ?? false)
                  Text(
                    '계정 ID ${flow.accountUuid}',
                    style: theme.textTheme.bodySmall,
                  ),
                if (flow.organizationUuid?.isNotEmpty ?? false)
                  Text(
                    '조직 ID ${flow.organizationUuid}',
                    style: theme.textTheme.bodySmall,
                  ),
                const SizedBox(height: Look.fieldGap),
                FilledButton(
                  key: const Key('claude-confirm'),
                  onPressed: ready
                      ? () => _request(
                          'confirm',
                          flow.flowId,
                          label: flow.confirmationNonce,
                        )
                      : null,
                  child: const Text('이 계정으로 저장'),
                ),
              ],
              const SizedBox(height: Look.fieldGap),
              if (flow.state.terminal)
                OutlinedButton(
                  onPressed: _disconnected ? null : _finish,
                  child: const Text('계정 목록으로'),
                )
              else
                TextButton(
                  key: const Key('claude-cancel'),
                  onPressed: _busy || _disconnected
                      ? null
                      : () {
                          _code.clear();
                          unawaited(_request('cancel', flow.flowId));
                        },
                  child: const Text('로그인 취소'),
                ),
            ],
          ),
        ),
      ],
    );
  }
}
