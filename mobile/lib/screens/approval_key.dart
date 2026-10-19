import 'dart:async';
import 'dart:convert';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import '../look.dart';
import '../relay_account.dart';
import '../secure_key.dart';
import '../server.dart';
import '../twins_loading.dart';
import 'controls.dart';

/// 비밀 승인 — 학생이 맥 키체인의 비밀을 쓸 때 이 폰이 Face ID 로 한 번씩 허락하게 한다(docs/secret-faceid-approval.md).
/// 「Face ID 로 연결」 한 번이 Secure Enclave 열쇠를 만들고(없을 때만) Face ID 로 서명해 본 뒤 공개키를 관문에 맡기고,
/// 계정의 맥마다 자동 믿기를 지금 돌리게 한다. 아직 1Password 판인 맥은 업데이트하라고 줄에 적는다.
class ApprovalKeyScreen extends StatefulWidget {
  const ApprovalKeyScreen({super.key, required this.api, this.server, this.signer = const SecureEnclaveSigner()});

  final RelayAccountApi Function() api;

  /// 계정의 맥들에 닿는 길 — 없으면(승인 화면에서 바로 온 경우) 맥 줄 없이 열쇠만 다룬다.
  final Server? server;
  final ApprovalSigner signer;

  @override
  State<ApprovalKeyScreen> createState() => _ApprovalKeyScreenState();
}

class _Mac {
  const _Mac(this.machine, {this.link, this.error});

  final Machine machine;

  /// null 이고 [error] 도 없으면 그 맥의 판이 이 동작을 모른다.
  final OpLink? link;
  final String? error;
}

class _ApprovalKeyScreenState extends State<ApprovalKeyScreen> {
  String? _public;

  /// 이 열쇠를 이번에 관문에 맡겼다 — 맥 목록 없이 들어왔을 때는 이것으로 연결을 판정한다.
  bool _registered = false;
  bool _loaded = false;
  bool _busy = false;
  String? _error;
  List<_Mac>? _macs;

  String? get _fingerprint {
    final public = _public;
    return public == null ? null : approvalFingerprint(approvalKeyId(base64.decode(public)));
  }

  /// 이 폰을 믿는 맥들의 상태 — 하나라도 토큰까지 있으면 바로 쓸 수 있다.
  Iterable<OpLink> get _trusting {
    final print = _fingerprint;
    if (print == null) return const [];
    return (_macs ?? const <_Mac>[]).map((m) => m.link).whereType<OpLink>().where((l) => l.trusted.contains(print));
  }

  @override
  void initState() {
    super.initState();
    unawaited(_load());
  }

  Future<T> _with<T>(Future<T> Function(RelayAccountApi api) work) async {
    final api = widget.api();
    try {
      return await work(api);
    } finally {
      api.close();
    }
  }

  /// 있는 열쇠는 관문에 다시 맡겨 둔다 — 같은 키면 같은 id 라 해가 없고, 관문이 잊었어도 되살아난다.
  Future<void> _load() async {
    final public = await widget.signer.publicKey();
    String? error;
    if (public != null) {
      try {
        await _with((api) => api.registerApprovalKey(public));
        _registered = true;
      } on AccountException catch (e) {
        error = e.message;
      }
    }
    if (!mounted) return;
    setState(() {
      _public = public;
      _loaded = true;
      _error = error;
    });
    await _check();
  }

  /// 계정의 켜진 맥마다 연결 상태를 묻는다 — 물음 자체가 그 맥의 자동 믿기를 지금 돌린다.
  Future<void> _check() async {
    final server = widget.server;
    if (server == null) return;
    List<Machine> machines;
    try {
      machines = (await server.machines()).where((m) => m.online).toList();
    } on ServerException catch (e) {
      if (mounted) setState(() => _error = e.message);
      return;
    }
    final macs = await Future.wait([
      for (final m in machines)
        server.opLink(machine: m.route).then((link) => _Mac(m, link: link)).catchError(
          (Object e) => _Mac(m, error: e is ServerException ? e.message : '맥에 닿지 못했어요.'),
        ),
    ]);
    if (mounted) setState(() => _macs = macs);
  }

  /// [fresh] 면 있던 열쇠를 버리고 새로 만든다(Face ID 를 다시 등록해 옛 열쇠가 무효일 때).
  Future<void> _link({bool fresh = false}) async {
    setState(() {
      _busy = true;
      _error = null;
    });
    try {
      final public = (fresh ? null : await widget.signer.publicKey()) ?? await widget.signer.create();
      if (mounted) {
        setState(() {
          _public = public;
          _registered = false;
        });
      }
      if (!await widget.signer.confirm('비밀 승인 폰으로 연결')) return;
      await _with((api) => api.registerApprovalKey(public));
      if (mounted) setState(() => _registered = true);
      await _check();
    } on AccountException catch (e) {
      if (mounted) setState(() => _error = e.message);
    } on PlatformException catch (e) {
      if (mounted) setState(() => _error = '연결하지 못했어요. Face ID 가 켜져 있는지 봐 주세요. (${e.message ?? e.code})');
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  Future<void> _delete() async {
    final ok = await showDialog<bool>(
      context: context,
      builder: (context) => ModalLook(
        child: AlertDialog(
          title: const Text('연결 끊기'),
          content: const Text('이 폰은 더는 비밀 요청을 허락하지 못해요. 열쇠가 관문 목록에서 빠지며 맥에서도 함께 막혀요.'),
          actions: [
            TextButton(onPressed: () => Navigator.of(context).pop(false), child: const Text('취소')),
            FilledButton(onPressed: () => Navigator.of(context).pop(true), child: const Text('끊기')),
          ],
        ),
      ),
    );
    if (ok != true || !mounted) return;
    setState(() => _busy = true);
    try {
      await _with((api) => api.deleteApprovalKey());
      await widget.signer.delete();
      if (mounted) {
        setState(() {
          _public = null;
          _registered = false;
        });
      }
      await _check();
    } on AccountException catch (e) {
      if (mounted) setState(() => _error = e.message);
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  String _say(_Mac mac) {
    if (mac.error case final e?) return e;
    final link = mac.link;
    if (link == null) return '이 맥의 KASA 를 새 판으로 업데이트해 주세요';
    final print = _fingerprint;
    final mine = print != null && link.trusted.contains(print);
    if (!link.keychain) {
      return mine && link.token ? '연결됨 · 옛 판(1Password) — 새 판으로 올리면 키체인을 써요' : '이 맥의 KASA 를 새 판으로 업데이트해 주세요 · 키체인 판부터 써요';
    }
    if (mine) return '연결됨 · 이 맥 키체인';
    if (print == null) return '이 폰은 아직이에요';
    return link.trusted.isEmpty
        ? '이 폰을 아직 믿지 않았어요 — 새로고침해 주세요'
        : '다른 폰을 믿고 있어요 · 맥 설정 → 계정 → 비밀 승인에서 이 폰을 「믿기」';
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final dim = theme.textTheme.bodySmall?.copyWith(color: scheme.onSurfaceVariant, fontSize: Look.sub);
    final print = _fingerprint;
    final trusting = _trusting;
    // 맥 목록이 없으면(승인 화면에서 바로 옴) 맡긴 것까지만 안다 — 맥은 다음 요청 직전에 이 열쇠를 믿는다.
    final blind = widget.server == null && _registered;
    final ready = blind || trusting.any((l) => l.ready);
    final linked = blind || trusting.isNotEmpty;
    final macs = _macs;
    return TwinBackdrop(
      child: Scaffold(
        backgroundColor: Colors.transparent,
        appBar: AppBar(
          backgroundColor: Colors.transparent,
          title: const Text('비밀 승인'),
          actions: [
            if (widget.server != null)
              IconButton(
                key: const Key('op-link-refresh'),
                tooltip: '다시 확인',
                onPressed: _busy || !_loaded ? null : () => unawaited(_check()),
                icon: const Icon(Icons.refresh_rounded),
              ),
          ],
        ),
        body: ListView(
          padding: const EdgeInsets.fromLTRB(Look.pagePad, 8, Look.pagePad, Look.groupGap * 2),
          children: [
            if (!_loaded)
              const Padding(padding: EdgeInsets.all(Look.groupGap), child: Center(child: CircularProgressIndicator()))
            else
              DecoratedBox(
                decoration: TwinTone.of(context).cardBox(),
                child: Padding(
                  padding: const EdgeInsets.all(Look.cardPad),
                  child: Column(
                    crossAxisAlignment: CrossAxisAlignment.stretch,
                    children: [
                      Row(
                        children: [
                          Icon(
                            ready ? Icons.verified_rounded : linked ? Icons.laptop_mac_rounded : Icons.fingerprint_rounded,
                            size: Look.iconSize,
                            color: ready ? scheme.primary : scheme.onSurfaceVariant,
                          ),
                          const SizedBox(width: 8),
                          Text(
                            ready ? '연결됐어요' : linked ? '맥에서 한 번만 더' : 'Face ID 로 허락하기',
                            style: theme.textTheme.titleMedium,
                          ),
                        ],
                      ),
                      const SizedBox(height: Look.groupTitleGap),
                      Text(
                        ready
                            ? '학생이 맥 키체인의 비밀을 쓰려 하면 이 폰에 요청이 와요. 내용을 보고 Face ID 로 허락하면 그 한 번만 꺼내요.'
                            : linked
                                ? '이 폰은 이어졌어요. 맥의 KASA 를 키체인 판으로 업데이트하면 끝나요.'
                                : '학생이 맥 키체인의 비밀을 쓸 때 이 폰 Face ID 로 한 번씩 허락해요. 한 번 누르면 맥까지 이어져요.',
                        style: dim,
                      ),
                      if (!linked) ...[
                        const SizedBox(height: Look.cardGap),
                        FilledButton.icon(
                          key: const Key('op-link'),
                          style: FilledButton.styleFrom(minimumSize: const Size.fromHeight(Look.buttonH)),
                          onPressed: _busy ? null : () => unawaited(_link()),
                          icon: _busy
                              ? const SizedBox.square(dimension: 18, child: CircularProgressIndicator(strokeWidth: 2))
                              : const Icon(Icons.fingerprint_rounded),
                          label: const Text('Face ID 로 연결'),
                        ),
                      ],
                    ],
                  ),
                ),
              ),
            if (macs != null)
              SettingsGroup(
                title: '맥',
                children: [
                  if (macs.isEmpty)
                    const SettingsRow(icon: Icons.laptop_mac_rounded, title: '켜진 맥이 없어요', subtitle: '맥을 켜고 다시 확인해 주세요'),
                  for (final mac in macs)
                    SettingsRow(
                      key: Key('op-mac-${mac.machine.route}'),
                      icon: Icons.laptop_mac_rounded,
                      title: mac.machine.label,
                      subtitle: _say(mac),
                      trailing: print != null && mac.link?.ready == true && (mac.link?.trusted.contains(print) ?? false)
                          ? Icon(Icons.check_circle_rounded, color: scheme.primary, size: Look.iconSize)
                          : null,
                    ),
                ],
              ),
            if (print != null)
              SettingsGroup(
                children: [
                  SettingsRow(
                    key: const Key('approval-key-recreate'),
                    icon: Icons.refresh_rounded,
                    title: '다시 연결',
                    subtitle: 'Face ID 를 다시 등록했으면 새 열쇠로 이어요',
                    onTap: _busy ? null : () => unawaited(_link(fresh: true)),
                  ),
                  SettingsRow(
                    key: const Key('approval-key-delete'),
                    icon: Icons.link_off_rounded,
                    danger: true,
                    title: '연결 끊기',
                    onTap: _busy ? null : () => unawaited(_delete()),
                  ),
                ],
              ),
            if (_error case final e?) ...[
              const SizedBox(height: Look.groupGap),
              ErrorBand(text: e),
            ],
            const SizedBox(height: Look.groupGap),
            Text(
              '개인 키는 이 폰 밖으로 나가지 않고, 비밀 값은 이 폰과 관문을 지나지 않아요.'
              '${print == null ? '' : ' 열쇠 지문 $print'}',
              key: const Key('approval-key-fingerprint'),
              style: dim,
            ),
          ],
        ),
      ),
    );
  }
}
