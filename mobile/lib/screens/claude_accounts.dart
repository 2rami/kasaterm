import 'dart:async';
import 'dart:math';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:url_launcher/url_launcher.dart';

import '../claude_login.dart';
import '../look.dart';
import '../machine_look.dart';
import '../server.dart';
import '../twins_loading.dart';
import 'controls.dart';
import 'settings_glyph.dart';

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
  // 기기마다 지금 계정·막힌 사정 — 고르지 않은 기기도 목록에서 바로 보이게 함께 받는다.
  final _summaries = <String, ClaudeDeviceAccounts>{};
  // 사람이 [다시 로그인]을 누른 그 흐름만 승인 페이지를 스스로 연다. 목록을 여는 것만으로는 열지 않는다.
  bool _autoOpen = false;
  _Device? _device;
  ClaudeAccount? _slot;
  ClaudePhoneLogin? _flow;
  _PendingStart? _pendingStart;
  String? _error;
  String? _codeError;
  final _unconfirmedPrevious = <_PendingStart>{};
  bool _loading = true;
  bool _busy = false;
  bool _polling = false;
  bool _foreground = true;
  bool _disconnected = false;
  int _generation = 0;
  int _operation = 0;
  Timer? _timer;
  Timer? _accountsTimer;
  bool _refreshingAccounts = false;
  String? _accountsError;
  String? _codexStarting;
  static const _accountsEvery = Duration(seconds: 5);


  Server get _server => widget.server;
  bool get _active => _flow != null && !_flow!.state.terminal;
  bool get _acceptsCode =>
      _flow?.state == ClaudeLoginState.awaitingAuthorization ||
      _flow?.state == ClaudeLoginState.awaitingCode;
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
    _scheduleAccounts();
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
    _codeError = null;
    _devices = const [];
    _accounts = const [];
    _summaries.clear();
    _autoOpen = false;
    _device = null;
    _slot = null;
    _flow = null;
    _pendingStart = null;
    _busy = false;
    _polling = false;
    _refreshingAccounts = false;
    _accountsError = null;
    _codexStarting = null;
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
    _scheduleAccounts();
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
    _accountsTimer?.cancel();
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
    _accountsTimer?.cancel();
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
    _accountsTimer?.cancel();
    if (_foreground) {
      unawaited(_poll());
      unawaited(_refreshAccounts());
      _scheduleAccounts();
    }
  }

  void _scheduleAccounts() {
    _accountsTimer?.cancel();
    if (!_foreground || _disconnected || _server.isClosed) return;
    _accountsTimer = Timer.periodic(_accountsEvery, (_) => unawaited(_refreshAccounts()));
  }

  Future<void> _refreshAccounts() async {
    final device = _device;
    if (!mounted || !_foreground || _disconnected || _loading || _locked ||
        _busy || _refreshingAccounts || device == null || !device.online ||
        ModalRoute.of(context)?.isCurrent == false) {
      return;
    }
    final generation = _generation;
    final server = _server;
    _refreshingAccounts = true;
    try {
      // A poll only reads the selected device. Starting a login always requires a tap.
      await _loadAccounts(device, generation, quiet: true);
    } finally {
      if (_server == server) _refreshingAccounts = false;
    }
  }

  Future<void> _load() async {
    if (_disconnected || _locked || _busy || _codexStarting != null) return;
    final generation = ++_generation;
    setState(() {
      _loading = true;
      _accountsError = null;
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
      await Future.wait([
        _loadAccounts(device, generation),
        for (final other in devices)
          if (other.id != device.id && other.online)
            _loadSummary(other, generation),
      ]);
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

  Future<void> _loadAccounts(_Device device, int generation, {bool quiet = false}) async {
    try {
      if (!device.online) {
        throw const ServerException(
          '이 기기는 연결되어 있지 않아요. 기기의 KASA를 켠 뒤 새로고침해 주세요.',
        );
      }
      final summary = await _server.claudeDeviceAccounts(
        machine: device.route,
        watching: true,
      );
      if (!_current(generation) || (quiet && _locked)) return;
      setState(() {
        _summaries[device.id] = summary;
        if (_device?.id == device.id) _accounts = summary.accounts;
        _accountsError = null;
        _loading = false;
      });
    } on ServerException catch (e) {
      if (_current(generation)) {
        setState(() {
          if (quiet && _locked) return;
          _summaries.remove(device.id);
          if (_device?.id == device.id) _accounts = const [];
          final message = switch (e.status) {
            400 || 404 => '이 기기의 KASA를 새 판으로 업데이트해 주세요.',
            401 || 403 => '접근할 수 없어요. KASA 계정과 기기 연결을 확인해 주세요.',
            502 || 503 || 504 => '이 기기에 연결하지 못했어요. KASA를 켠 뒤 새로고침해 주세요.',
            _ => e.message,
          };
          if (quiet) {
            _accountsError = message;
          } else {
            _error = message;
          }
          _loading = false;
        });
      }
    }
  }

  /// 고르지 않은 기기의 요약. 못 읽으면 그 줄에 「확인 못 함」만 남는다 — 화면 전체 오류로 올리지 않는다.
  Future<void> _loadSummary(_Device device, int generation) async {
    try {
      final summary = await _server.claudeDeviceAccounts(
        machine: device.route,
      );
      if (_current(generation)) {
        setState(() => _summaries[device.id] = summary);
      }
    } on ServerException {
      // 기기 줄이 「확인 못 함」으로 남는다.
    } on TimeoutException {
      // 같은 까닭.
    }
  }

  String _name(_Device device) =>
      machineName(device.label, route: '~${device.id}');

  /// 기기 줄의 둘째 줄 — 지금 쓰는 Claude 계정·이메일·사용률.
  String _deviceLine(_Device device) {
    if (!device.online) return '연결 안 됨';
    final summary = _summaries[device.id];
    if (summary == null) {
      return _loading ? '계정 확인 중' : '계정을 확인하지 못했어요';
    }
    final active = summary.active;
    if (active == null) {
      return summary.activeId.isEmpty ? 'Claude 기본 로그인' : 'Claude 계정 확인 중';
    }
    return [
      'Claude ${active.title}',
      if (active.email.isNotEmpty && active.email != active.title) active.email,
      if (active.org.isNotEmpty && active.org != active.title) active.org,
      if (active.usageText.isNotEmpty) active.usageText,
    ].join(' · ');
  }

  void _select(_Device device) {
    if (device.id == _device?.id || _locked || _busy || _disconnected || _codexStarting != null) return;
    final summary = _summaries[device.id];
    final generation = ++_generation;
    setState(() {
      _device = device;
      _accounts = summary?.accounts ?? const [];
      _accountsError = null;
      _flow = null;
      _slot = null;
      _pendingStart = null;
      _error = null;
      _loading = summary == null;
    });
    unawaited(_loadAccounts(device, generation));
  }

  /// 기기 줄의 [다시 로그인] — 기기를 고르고 그 슬롯 로그인을 한 번에 시작한다.
  Future<void> _loginOn(_Device device, ClaudeAccount slot) async {
    if (_locked || _busy || _disconnected) return;
    if (device.id != _device?.id) {
      _generation++;
      setState(() {
        _device = device;
        _accounts = _summaries[device.id]?.accounts ?? const [];
        _flow = null;
        _slot = null;
        _pendingStart = null;
        _error = null;
        _loading = false;
      });
    }
    await _start(slot);
  }

  String _newNonce() {
    final bytes = List<int>.generate(16, (_) => Random.secure().nextInt(256));
    bytes[6] = (bytes[6] & 15) | 64;
    bytes[8] = (bytes[8] & 63) | 128;
    final hex = bytes.map((b) => b.toRadixString(16).padLeft(2, '0')).join();
    return '${hex.substring(0, 8)}-${hex.substring(8, 12)}-${hex.substring(12, 16)}-${hex.substring(16, 20)}-${hex.substring(20)}';
  }

  Future<void> _start(ClaudeAccount slot) async {
    if (_busy || _active || _disconnected || _codexStarting != null) return;
    if (_pending && _pendingStart!.slot.id != slot.id) return;
    if (_pending && _pendingStart!.abandoned) return _cancelPending();
    final device = _device;
    if (device == null || _server.isClosed) return;
    if (!_pending) {
      _pendingStart = _PendingStart(_server, device, slot, _newNonce());
    }
    _pendingStarts[_server] = _pendingStart;
    _autoOpen = true;
    setState(() {
      _slot = slot;
      _flow = null;
      _codeError = null;
      _code.clear();
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
          if (action == 'code' && e.code == 'phone_login_invalid_code') {
            _codeError = e.message;
          } else {
            _error = _pending && _flow == null ? _uncertainStart : e.message;
          }
        });
      }
    } on TimeoutException {
      pending?.uncertain = true;
      if (_current(generation)) setState(() => _error = _uncertainStart);
    } finally {
      if (_current(generation)) {
        setState(() => _busy = false);
        _schedule();
        _maybeOpenBrowser();
      }
    }
  }

  /// 승인 페이지 주소가 처음 오면 한 번 연다 — 「다시 로그인」 뒤 「Safari에서 로그인」을 또 누르던 한 손을 던다.
  void _maybeOpenBrowser() {
    final flow = _flow;
    if (!_autoOpen || flow == null) return;
    if (flow.state.terminal) {
      _autoOpen = false;
      return;
    }
    if (!_acceptsCode || flow.authorizationUrl == null || !_foreground) return;
    _autoOpen = false;
    unawaited(_openBrowser());
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
        _maybeOpenBrowser();
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
    if (flow.state != ClaudeLoginState.awaitingAuthorization &&
        flow.state != ClaudeLoginState.awaitingCode &&
        flow.state != ClaudeLoginState.starting) {
      _code.clear();
      _codeError = null;
    }
    if (flow.state.terminal) {
      _pendingStart = null;
      _code.clear();
    }
  }

  Future<void> _openBrowser() async {
    final uri = _flow?.authorizationUrl;
    if (uri == null || _busy || _disconnected) return;
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
    if (flow == null || !_acceptsCode || _busy || _disconnected) return;
    if (!claudeLoginCode(code)) {
      setState(() => _codeError = '승인 화면의 인증코드 전체를 복사해 붙여 넣어 주세요.');
      return;
    }
    setState(() => _codeError = null);
    FocusManager.instance.primaryFocus?.unfocus();
    await _request('code', flow.flowId, label: code);
  }

  Future<void> _pasteCode() async {
    final generation = _generation, flowId = _flow?.flowId;
    try {
      final text = (await Clipboard.getData(
        Clipboard.kTextPlain,
      ))?.text?.trim();
      if (!_current(generation) ||
          _flow?.flowId != flowId ||
          _busy ||
          !_acceptsCode) {
        return;
      }
      setState(() {
        if (text == null || text.isEmpty) {
          _codeError = '복사한 인증코드가 없어요. 승인 화면에서 먼저 복사해 주세요.';
        } else {
          _code.value = TextEditingValue(
            text: text,
            selection: TextSelection.collapsed(offset: text.length),
          );
          _codeError = null;
        }
      });
    } on PlatformException {
      if (_current(generation)) {
        setState(() => _codeError = '붙여넣기를 허용하거나 입력칸을 길게 눌러 붙여 넣어 주세요.');
      }
    }
  }

  /// 승인 화면에서 복사해 돌아온 뒤 한 번에 — 붙여넣기와 보내기를 따로 누르던 두 손을 하나로.
  Future<void> _pasteAndSubmit() async {
    await _pasteCode();
    if (!mounted || _codeError != null || _code.text.trim().isEmpty) return;
    await _submit();
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

  Future<void> _startCodexLogin(_Device device, ClaudeAccount account) async {
    if (_codexStarting != null || _locked || _busy || _disconnected) return;
    final generation = _generation;
    setState(() => _codexStarting = account.id);
    try {
      await _server.settingsAction(
        'reauth-account',
        id: account.id,
        label: 'codex',
        machine: device.route,
      );
      if (!mounted || !_current(generation)) return;
      ScaffoldMessenger.of(context).showSnackBar(
        const SnackBar(content: Text('선택한 기기의 브라우저에서 Codex 로그인을 마쳐 주세요.')),
      );
    } on ServerException catch (error) {
      if (_current(generation)) setState(() => _error = error.message);
    } finally {
      if (_current(generation)) setState(() => _codexStarting = null);
    }
  }

  @override
  Widget build(BuildContext context) => TwinBackdrop(
    child: Scaffold(
      backgroundColor: Colors.transparent,
      appBar: AppBar(
        backgroundColor: Colors.transparent,
        title: const Text('코딩 에이전트'),
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
    final summary = _summaries[device.id];
    final codex = summary?.codexAccounts ?? const [];
    return ListView(
      keyboardDismissBehavior: ScrollViewKeyboardDismissBehavior.onDrag,
      padding: const EdgeInsets.fromLTRB(
        Look.pagePad,
        0,
        Look.pagePad,
        Look.groupGap,
      ),
      children: [
        if (!_locked)
          SettingsOverview(
            art: const SettingsMark(SettingsGlyph.agents, size: 36),
            title: "${_name(device)} · Claude",
            detail: _accounts.where((account) => account.id == _summaries[device.id]?.activeId).firstOrNull?.title,
          ),
        SettingsGroup(
          title: _locked ? '기기 · 로그인 중에는 고정돼요' : '기기',
          // 로그인 중에는 그 기기 줄만 — 다른 기기 줄이 승인 단계를 화면 아래로 밀어내지 않게.
          children: [
            for (final each in _devices)
              if ((flow == null && !_locked) || each.id == device.id)
                _deviceRow(each),
          ],
        ),
        if (_unconfirmedPrevious.isNotEmpty)
          const ErrorBand(
            text:
                '이전 연결의 로그인 취소 여부를 확인하지 못했어요. 복구하지 못하면 서버 요청이 최대 10분 동안 남을 수 있어요.',
          ),
        if (_accountsError != null && !_locked)
          ErrorBand(text: _accountsError!, onRetry: _load),
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
        else if (_accounts.isEmpty && codex.isEmpty && _error == null && _accountsError == null)
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
            title: 'Claude',
            children: [
              for (final account in _accounts)
                SettingsRow(
                  key: Key('claude-account-${account.id}'),
                  icon: account.id == _summaries[device.id]?.activeId
                      ? Icons.person_rounded
                      : Icons.person_outline_rounded,
                  title: account.title,
                  subtitle: [
                    if (account.id == _summaries[device.id]?.activeId)
                      '지금 쓰는 계정',
                    account.detail,
                    if (account.usageText.isNotEmpty) account.usageText,
                    if (account.relogin != null)
                      claudeReloginReason(account.relogin),
                  ].join(' · '),
                  below: account.needsLogin || (_pending && _slot?.id == account.id)
                      ? Align(
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
                        )
                      : null,
                ),
            ],
          ),
        if (flow == null && !_loading && codex.isNotEmpty)
          SettingsGroup(
            title: 'Codex',
            children: [
              for (final account in codex)
                SettingsRow(
                  key: Key('codex-account-${account.id}'),
                  icon: Icons.terminal_rounded,
                  title: account.title.isEmpty ? '기본 로그인' : account.title,
                  subtitle: [
                    if (account.id == summary?.activeCodexId) '지금 쓰는 계정',
                    account.detail,
                    if (account.usageText.isNotEmpty) account.usageText,
                  ].join(' · '),
                  below: account.needsLogin
                      ? Align(
                          alignment: Alignment.centerLeft,
                          child: FilledButton.tonal(
                            key: Key('codex-login-${account.id}'),
                            onPressed: _codexStarting != null || _locked || _busy || _disconnected
                                ? null
                                : () => _startCodexLogin(device, account),
                            child: Text(_codexStarting == account.id ? '여는 중' : 'PC에서 다시 로그인'),
                          ),
                        )
                      : null,
                ),
            ],
          ),
      ],
    );
  }

  Widget _deviceRow(_Device device) {
    final theme = Theme.of(context);
    final summary = device.online ? _summaries[device.id] : null;
    final problem = summary?.followProblem;
    final selected = device.id == _device?.id;
    final target = selected ? null : summary?.reloginTarget;
    final free = !_locked && !_busy && !_disconnected;
    return SettingsRow(
      key: Key('claude-device-${device.id}'),
      icon: machineIcon(device.label),
      title: _name(device),
      subtitle: _deviceLine(device),
      trailing: selected ? const Icon(Icons.check_rounded) : null,
      onTap: free && !selected ? () => _select(device) : null,
      below: problem == null && target == null
          ? null
          : Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                if (problem != null)
                  Text(
                    problem,
                    key: Key('claude-device-problem-${device.id}'),
                    style: theme.textTheme.bodySmall?.copyWith(
                      color: theme.colorScheme.error,
                    ),
                  ),
                if (target != null) ...[
                  const SizedBox(height: Look.fieldGap),
                  FilledButton.tonal(
                    key: Key('claude-device-login-${device.id}'),
                    onPressed: free ? () => _loginOn(device, target) : null,
                    child: Text('다시 로그인 · ${target.title}'),
                  ),
                ],
              ],
            ),
    );
  }

  Widget _login(ClaudePhoneLogin flow) {
    final theme = Theme.of(context);
    final waiting =
        flow.state == ClaudeLoginState.awaitingAuthorization ||
        flow.state == ClaudeLoginState.awaitingCode;
    final ready = !_busy && !_disconnected;
    final status = switch (flow.state) {
      ClaudeLoginState.starting => '로그인 준비 중',
      ClaudeLoginState.awaitingAuthorization || ClaudeLoginState.awaitingCode =>
        _code.text.trim().isNotEmpty
            ? '인증코드를 보내면 선택한 기기의 계정을 확인하고 로그인을 저장해요.'
            : 'Safari에서 Claude에 로그인한 뒤, 승인 화면의 인증코드를 복사해 돌아와 「인증코드 붙여넣고 보내기」를 눌러 주세요.',
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
          subtitle: _slot!.name.isNotEmpty && _slot!.name != _slot!.title
              ? _slot!.name
              : null,
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
              if (waiting) ...[
                const SizedBox(height: Look.fieldGap),
                FilledButton.icon(
                  key: const Key('claude-paste-submit'),
                  onPressed: ready ? _pasteAndSubmit : null,
                  icon: const Icon(Icons.content_paste_go_rounded),
                  label: Text(_busy ? '확인하는 중' : '인증코드 붙여넣고 보내기'),
                ),
              ],
              if (waiting && flow.authorizationUrl != null) ...[
                const SizedBox(height: Look.fieldGap),
                OutlinedButton(
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
                    decoration: InputDecoration(
                      hintText: '인증코드 붙여넣기…',
                      errorText: _codeError,
                      errorMaxLines: 4,
                    ),
                    onChanged: (_) => setState(() => _codeError = null),
                    onSubmitted: (_) => _submit(),
                  ),
                ),
                const SizedBox(height: Look.fieldGap),
                Align(
                  alignment: Alignment.centerLeft,
                  child: TextButton.icon(
                    key: const Key('claude-paste-code'),
                    onPressed: ready ? _pasteCode : null,
                    icon: const Icon(Icons.content_paste_rounded),
                    label: const Text('인증코드 붙여넣기'),
                  ),
                ),
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
              if (flow.state.terminal &&
                  flow.state != ClaudeLoginState.succeeded)
                FilledButton(
                  key: const Key('claude-retry-login'),
                  onPressed: ready ? () => _start(_slot!) : null,
                  child: const Text('다시 로그인'),
                ),
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
