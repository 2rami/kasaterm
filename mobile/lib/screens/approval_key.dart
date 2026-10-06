import 'dart:async';
import 'dart:convert';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import '../look.dart';
import '../relay_account.dart';
import '../secure_key.dart';
import '../twins_loading.dart';
import 'controls.dart';

/// Face ID 승인 열쇠 — 학생의 1Password 비밀 요청을 이 폰이 한 번씩 허락할 때 쓰는 Secure Enclave 키
/// (docs/op-faceid-approval.md). 만들면 공개키를 관문에 맡기고, 맥에서 같은 지문의 「믿기」를 눌러야 쓰인다.
class ApprovalKeyScreen extends StatefulWidget {
  const ApprovalKeyScreen({super.key, required this.api, this.signer = const SecureEnclaveSigner()});

  final RelayAccountApi Function() api;
  final ApprovalSigner signer;

  @override
  State<ApprovalKeyScreen> createState() => _ApprovalKeyScreenState();
}

class _ApprovalKeyScreenState extends State<ApprovalKeyScreen> {
  String? _fingerprint;
  bool _loaded = false;
  bool _busy = false;
  String? _error;

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
    try {
      final public = await widget.signer.publicKey();
      if (public != null) await _with((api) => api.registerApprovalKey(public));
      _show(public);
    } on AccountException catch (e) {
      _show(await widget.signer.publicKey(), error: e.message);
    }
  }

  void _show(String? public, {String? error}) {
    if (!mounted) return;
    setState(() {
      _loaded = true;
      _fingerprint = public == null ? null : approvalFingerprint(approvalKeyId(base64.decode(public)));
      _error = error;
    });
  }

  Future<void> _create() async {
    setState(() {
      _busy = true;
      _error = null;
    });
    try {
      final public = await widget.signer.create();
      await _with((api) => api.registerApprovalKey(public));
      _show(public);
    } on AccountException catch (e) {
      _show(await widget.signer.publicKey(), error: e.message);
    } on PlatformException catch (e) {
      _show(null, error: '열쇠를 만들지 못했어요. Face ID 가 켜져 있는지 봐 주세요. (${e.message ?? e.code})');
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  Future<void> _delete() async {
    final ok = await showDialog<bool>(
      context: context,
      builder: (context) => ModalLook(
        child: AlertDialog(
          title: const Text('열쇠 지우기'),
          content: const Text('이 폰은 더는 1Password 요청을 허락하지 못해요. 맥이 믿던 기록은 관문 목록에서 빠지며 함께 막혀요.'),
          actions: [
            TextButton(onPressed: () => Navigator.of(context).pop(false), child: const Text('취소')),
            FilledButton(onPressed: () => Navigator.of(context).pop(true), child: const Text('지우기')),
          ],
        ),
      ),
    );
    if (ok != true || !mounted) return;
    setState(() => _busy = true);
    try {
      await _with((api) => api.deleteApprovalKey());
      await widget.signer.delete();
      _show(null);
    } on AccountException catch (e) {
      if (mounted) setState(() => _error = e.message);
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final fingerprint = _fingerprint;
    final dim = theme.textTheme.bodySmall?.copyWith(color: scheme.onSurfaceVariant, fontSize: Look.sub);
    return TwinBackdrop(
      child: Scaffold(
        backgroundColor: Colors.transparent,
        appBar: AppBar(backgroundColor: Colors.transparent, title: const Text('Face ID 승인 열쇠')),
        body: ListView(
          padding: const EdgeInsets.fromLTRB(Look.pagePad, 8, Look.pagePad, Look.groupGap * 2),
          children: [
            if (!_loaded)
              const Padding(padding: EdgeInsets.all(Look.groupGap), child: Center(child: CircularProgressIndicator()))
            else if (fingerprint == null)
              SettingsGroup(
                children: [
                  SettingsRow(
                    key: const Key('approval-key-create'),
                    tone: 1,
                    icon: Icons.fingerprint_rounded,
                    title: '열쇠 만들기',
                    subtitle: '이 폰의 Secure Enclave 에 열쇠를 만들고 관문에 공개키만 맡겨요',
                    chevron: true,
                    onTap: _busy ? null : () => unawaited(_create()),
                  ),
                ],
              )
            else ...[
              DecoratedBox(
                decoration: TwinTone.of(context).cardBox(),
                child: Padding(
                  padding: const EdgeInsets.all(Look.cardPad),
                  child: Column(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    children: [
                      Text('지문', style: dim?.copyWith(fontWeight: FontWeight.w600)),
                      const SizedBox(height: Look.groupTitleGap),
                      SelectableText(
                        fingerprint,
                        key: const Key('approval-key-fingerprint'),
                        style: TextStyle(
                          fontFamily: 'TermMono',
                          fontFamilyFallback: Look.flowMonoFallback,
                          fontSize: 24,
                          fontWeight: FontWeight.w600,
                          color: scheme.onSurface,
                        ),
                      ),
                      const SizedBox(height: Look.rowGap * 2),
                      Text(
                        '맥 kasaterm 설정 → 계정 → 1Password 에 같은 지문의 새 열쇠가 떠요. 지문이 같은지 보고 「믿기」를 눌러 주세요.',
                        style: dim,
                      ),
                    ],
                  ),
                ),
              ),
              const SizedBox(height: Look.groupGap),
              SettingsGroup(
                children: [
                  SettingsRow(
                    key: const Key('approval-key-recreate'),
                    icon: Icons.refresh_rounded,
                    title: '다시 만들기',
                    subtitle: 'Face ID 를 다시 등록했으면 열쇠가 무효예요 — 새로 만들고 맥에서 다시 믿기',
                    onTap: _busy ? null : () => unawaited(_create()),
                  ),
                  SettingsRow(
                    key: const Key('approval-key-delete'),
                    icon: Icons.delete_outline_rounded,
                    danger: true,
                    title: '지우기',
                    onTap: _busy ? null : () => unawaited(_delete()),
                  ),
                ],
              ),
            ],
            if (_error case final e?) ...[
              const SizedBox(height: Look.groupGap),
              ErrorBand(text: e),
            ],
            const SizedBox(height: Look.groupGap),
            Text(
              '학생이 맥에서 1Password 비밀을 읽으려 하면 이 폰에 요청이 와요. 내용을 보고 Face ID 로 허락하면 그 한 번만 풀려요. '
              '개인 키는 이 폰 밖으로 나가지 않고, 비밀 값은 이 폰과 관문을 지나지 않아요.',
              style: dim,
            ),
          ],
        ),
      ),
    );
  }
}
