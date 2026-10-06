import 'dart:async';
import 'dart:typed_data';
import 'dart:ui' as ui;

import 'package:flutter/material.dart';
import 'package:image_picker/image_picker.dart';

import '../look.dart';
import '../relay_account.dart';
import 'controls.dart';

/// 공급자 로고 — Google 은 원래 색, GitHub 은 글자색으로 칠한다.
class ProviderLogo extends StatelessWidget {
  const ProviderLogo(this.provider, {super.key, this.size = Look.iconSize});

  final OAuthProvider provider;
  final double size;

  @override
  Widget build(BuildContext context) => Image.asset(
    provider == OAuthProvider.google
        ? 'assets/icons/google.png'
        : 'assets/icons/github.png',
    width: size,
    height: size,
    color: provider == OAuthProvider.github
        ? Theme.of(context).colorScheme.onSurface
        : null,
  );
}

/// 올린 계정 사진은 기기 자격증명으로만 받는다 — 판(`rev`)마다 한 번.
final _uploads = <String, Future<Uint8List>>{};

/// 계정 사진 동그라미. 사진이 없으면 [fallback].
class AccountFace extends StatelessWidget {
  const AccountFace({
    super.key,
    required this.avatar,
    required this.api,
    required this.size,
    required this.fallback,
  });

  final ProfileAvatar? avatar;
  final RelayAccountApi Function() api;
  final double size;
  final Widget fallback;

  Future<Uint8List> _upload(String key) => _uploads.putIfAbsent(key, () async {
    final client = api();
    try {
      return await client.avatarImage();
    } finally {
      client.close();
    }
  });

  @override
  Widget build(BuildContext context) {
    final avatar = this.avatar;
    final px = (size * MediaQuery.devicePixelRatioOf(context)).round();
    Widget image;
    if (avatar == null) {
      return fallback;
    } else if (avatar.uploaded) {
      image = FutureBuilder<Uint8List>(
        future: _upload(avatar.key),
        builder: (context, snap) => snap.data == null
            ? fallback
            : Image.memory(
                snap.data!,
                width: size,
                height: size,
                fit: BoxFit.cover,
                cacheWidth: px,
              ),
      );
    } else {
      image = Image.network(
        '${avatar.url}',
        width: size,
        height: size,
        fit: BoxFit.cover,
        cacheWidth: px,
        errorBuilder: (context, _, _) => fallback,
      );
    }
    return ClipOval(
      child: SizedBox.square(dimension: size, child: image),
    );
  }
}

/// 고른 사진의 가운데를 정사각으로 잘라 256 PNG 로 — 관문에 올릴 크기.
Future<Uint8List> squareAvatar(Uint8List bytes, {int side = 256}) async {
  final codec = await ui.instantiateImageCodec(bytes);
  final frame = await codec.getNextFrame();
  final source = frame.image;
  final edge = source.width < source.height ? source.width : source.height;
  final recorder = ui.PictureRecorder();
  final canvas = Canvas(recorder);
  canvas.drawImageRect(
    source,
    Rect.fromLTWH(
      (source.width - edge) / 2,
      (source.height - edge) / 2,
      edge.toDouble(),
      edge.toDouble(),
    ),
    Rect.fromLTWH(0, 0, side.toDouble(), side.toDouble()),
    Paint()..filterQuality = FilterQuality.high,
  );
  final out = await recorder.endRecording().toImage(side, side);
  final png = await out.toByteData(format: ui.ImageByteFormat.png);
  source.dispose();
  out.dispose();
  if (png == null) throw const AccountException('사진을 읽지 못했어요. 다른 사진을 골라 주세요.');
  return png.buffer.asUint8List();
}

/// 닉네임·사진 바꾸기 시트. 바뀐 프로필을 돌려준다.
Future<AccountProfile?> showProfileSheet(
  BuildContext context, {
  required AccountProfile profile,
  required RelayAccountApi Function() api,
  Future<Uint8List?> Function()? pick,
}) => showModalBottomSheet<AccountProfile>(
  context: context,
  isScrollControlled: true,
  showDragHandle: true,
  builder: (_) => ModalLook(
    child: _ProfileSheet(profile: profile, api: api, pick: pick ?? _pickPhoto),
  ),
);

Future<Uint8List?> _pickPhoto() async {
  final photo = await ImagePicker().pickImage(
    source: ImageSource.gallery,
    maxWidth: 1024,
    maxHeight: 1024,
    requestFullMetadata: false,
  );
  return photo?.readAsBytes();
}

class _ProfileSheet extends StatefulWidget {
  const _ProfileSheet({
    required this.profile,
    required this.api,
    required this.pick,
  });

  final AccountProfile profile;
  final RelayAccountApi Function() api;
  final Future<Uint8List?> Function() pick;

  @override
  State<_ProfileSheet> createState() => _ProfileSheetState();
}

class _ProfileSheetState extends State<_ProfileSheet> {
  late AccountProfile _profile = widget.profile;
  late final _nickname = TextEditingController(
    text: widget.profile.nickname ?? '',
  );
  bool _busy = false;
  String? _error;

  @override
  void dispose() {
    _nickname.dispose();
    super.dispose();
  }

  Future<void> _run(
    Future<AccountProfile> Function(RelayAccountApi api) work, {
    bool close = false,
  }) async {
    setState(() {
      _busy = true;
      _error = null;
    });
    final api = widget.api();
    try {
      final next = await work(api);
      if (!mounted) return;
      if (close) {
        Navigator.of(context).pop(next);
      } else {
        setState(() => _profile = next);
      }
    } on AccountException catch (e) {
      if (mounted) setState(() => _error = e.message);
    } finally {
      api.close();
      if (mounted) setState(() => _busy = false);
    }
  }

  Future<void> _upload() async {
    final Uint8List? bytes;
    try {
      bytes = await widget.pick();
    } catch (_) {
      setState(() => _error = '사진을 열지 못했어요.');
      return;
    }
    if (bytes == null) return;
    await _run((api) async => api.uploadAvatar(await squareAvatar(bytes!)));
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final providers = [
      for (final login in _profile.identities)
        if (login.picture != null) login.provider,
    ];
    return SafeArea(
      child: Padding(
        padding: EdgeInsets.fromLTRB(
          Look.pagePad,
          0,
          Look.pagePad,
          Look.pagePad + MediaQuery.viewInsetsOf(context).bottom,
        ),
        child: SingleChildScrollView(
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.stretch,
            mainAxisSize: MainAxisSize.min,
            children: [
              Text('프로필', style: theme.textTheme.titleLarge),
              const SizedBox(height: Look.groupGap),
              Center(
                child: AccountFace(
                  key: ValueKey(_profile.avatar?.key),
                  avatar: _profile.avatar,
                  api: widget.api,
                  size: Look.accountArt,
                  fallback: CircleAvatar(
                    radius: Look.accountArt / 2,
                    child: Text(
                      (_profile.name ?? '?').characters.first,
                      style: theme.textTheme.titleLarge,
                    ),
                  ),
                ),
              ),
              const SizedBox(height: Look.fieldGap),
              Wrap(
                alignment: WrapAlignment.center,
                spacing: 8,
                runSpacing: 8,
                children: [
                  OutlinedButton(
                    key: const Key('profile-photo-pick'),
                    onPressed: _busy ? null : () => unawaited(_upload()),
                    child: const Text('사진 고르기'),
                  ),
                  for (final provider in providers)
                    OutlinedButton.icon(
                      key: Key('profile-photo-${provider.id}'),
                      onPressed: _busy
                          ? null
                          : () => unawaited(
                              _run(
                                (api) => api.updateProfile(avatar: provider.id),
                              ),
                            ),
                      icon: ProviderLogo(provider, size: 16),
                      label: Text('${provider.label} 사진'),
                    ),
                  if (_profile.avatar?.uploaded == true)
                    OutlinedButton(
                      key: const Key('profile-photo-remove'),
                      onPressed: _busy
                          ? null
                          : () => unawaited(_run((api) => api.removeAvatar())),
                      child: const Text('올린 사진 지우기'),
                    ),
                ],
              ),
              const SizedBox(height: Look.groupGap),
              LabeledField(
                label: '닉네임',
                child: TextField(
                  key: const Key('profile-nickname'),
                  controller: _nickname,
                  enabled: !_busy,
                  maxLength: 40,
                  style: const TextStyle(fontSize: 16),
                  textInputAction: TextInputAction.done,
                  onSubmitted: (_) => unawaited(_save()),
                ),
              ),
              Text(
                '닉네임과 사진은 이 계정의 모든 기기·폰에 같이 보여요. 올린 사진이 없으면 이어진 Google·GitHub 사진을 써요.',
                style: theme.textTheme.bodySmall?.copyWith(
                  color: theme.colorScheme.onSurfaceVariant,
                ),
              ),
              if (_error case final error?) ...[
                const SizedBox(height: 12),
                Semantics(
                  liveRegion: true,
                  child: Text(
                    error,
                    style: TextStyle(color: theme.colorScheme.error),
                  ),
                ),
              ],
              const SizedBox(height: Look.groupGap),
              FilledButton(
                key: const Key('profile-save'),
                onPressed: _busy ? null : () => unawaited(_save()),
                child: _busy
                    ? const SizedBox.square(
                        dimension: 18,
                        child: CircularProgressIndicator(strokeWidth: 2),
                      )
                    : const Text('저장'),
              ),
            ],
          ),
        ),
      ),
    );
  }

  Future<void> _save() async {
    final nickname = _nickname.text.trim();
    if (nickname == (_profile.nickname ?? '')) {
      Navigator.of(context).pop(_profile);
      return;
    }
    await _run((api) => api.updateProfile(nickname: nickname), close: true);
  }
}

/// 로그인 아이디 또는 비밀번호 바꾸기 시트. 성공하면 바뀐 프로필(비밀번호면 지금 프로필)을 돌려준다.
Future<AccountProfile?> showSecretSheet(
  BuildContext context, {
  required AccountProfile profile,
  required RelayAccountApi Function() api,
  required bool password,
}) => showModalBottomSheet<AccountProfile>(
  context: context,
  isScrollControlled: true,
  showDragHandle: true,
  builder: (_) => ModalLook(
    child: _SecretSheet(profile: profile, api: api, password: password),
  ),
);

class _SecretSheet extends StatefulWidget {
  const _SecretSheet({
    required this.profile,
    required this.api,
    required this.password,
  });

  final AccountProfile profile;
  final RelayAccountApi Function() api;
  final bool password;

  @override
  State<_SecretSheet> createState() => _SecretSheetState();
}

class _SecretSheetState extends State<_SecretSheet> {
  late final _login = TextEditingController(text: widget.profile.login ?? '');
  final _current = TextEditingController();
  final _next = TextEditingController();
  final _again = TextEditingController();
  bool _busy = false;
  String? _error;

  @override
  void dispose() {
    for (final c in [_login, _current, _next, _again]) {
      c.dispose();
    }
    super.dispose();
  }

  Future<void> _submit() async {
    final problem = switch (widget.password) {
      _ when _current.text.isEmpty => '지금 비밀번호를 입력해 주세요.',
      false when _login.text.trim().isEmpty => '새 아이디를 입력해 주세요.',
      true when _next.text.length < minPasswordChars => profileError(
        'weak_password',
        400,
      ),
      true when _next.text != _again.text => '새 비밀번호 두 칸이 달라요.',
      _ => null,
    };
    if (problem != null) {
      setState(() => _error = problem);
      return;
    }
    setState(() {
      _busy = true;
      _error = null;
    });
    final api = widget.api();
    try {
      final AccountProfile result;
      if (widget.password) {
        await api.changePassword(_current.text, _next.text);
        result = widget.profile;
      } else {
        result = await api.changeLogin(_current.text, _login.text);
      }
      if (mounted) Navigator.of(context).pop(result);
    } on AccountException catch (e) {
      if (mounted) setState(() => _error = e.message);
    } finally {
      api.close();
      if (mounted) setState(() => _busy = false);
    }
  }

  TextField _field(
    Key key,
    TextEditingController c, {
    bool secret = true,
    bool last = false,
    List<String> hints = const [],
  }) => TextField(
    key: key,
    controller: c,
    enabled: !_busy,
    obscureText: secret,
    autocorrect: false,
    enableSuggestions: false,
    autofillHints: hints,
    style: const TextStyle(fontSize: 16),
    textInputAction: last ? TextInputAction.go : TextInputAction.next,
    onSubmitted: last ? (_) => unawaited(_submit()) : null,
  );

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final dim = theme.textTheme.bodySmall?.copyWith(
      color: theme.colorScheme.onSurfaceVariant,
    );
    return SafeArea(
      child: Padding(
        padding: EdgeInsets.fromLTRB(
          Look.pagePad,
          0,
          Look.pagePad,
          Look.pagePad + MediaQuery.viewInsetsOf(context).bottom,
        ),
        child: SingleChildScrollView(
          child: AutofillGroup(
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.stretch,
              mainAxisSize: MainAxisSize.min,
              children: [
                Text(
                  widget.password ? '비밀번호 바꾸기' : '아이디 바꾸기',
                  style: theme.textTheme.titleLarge,
                ),
                const SizedBox(height: Look.groupGap),
                if (!widget.password) ...[
                  LabeledField(
                    label: '새 아이디',
                    child: _field(
                      const Key('secret-login'),
                      _login,
                      secret: false,
                      hints: const [AutofillHints.username],
                    ),
                  ),
                  const SizedBox(height: Look.fieldGap),
                ],
                LabeledField(
                  label: '지금 비밀번호',
                  child: _field(
                    const Key('secret-current'),
                    _current,
                    last: !widget.password,
                    hints: const [AutofillHints.password],
                  ),
                ),
                if (widget.password) ...[
                  const SizedBox(height: Look.fieldGap),
                  LabeledField(
                    label: '새 비밀번호',
                    child: _field(
                      const Key('secret-next'),
                      _next,
                      hints: const [AutofillHints.newPassword],
                    ),
                  ),
                  const SizedBox(height: Look.fieldGap),
                  LabeledField(
                    label: '한 번 더',
                    child: _field(
                      const Key('secret-again'),
                      _again,
                      last: true,
                      hints: const [AutofillHints.newPassword],
                    ),
                  ),
                ],
                const SizedBox(height: Look.fieldGap),
                Text(
                  widget.password
                      ? '이미 로그인한 다른 기기·폰은 그대로 쓸 수 있어요.'
                      : '기기 로그인은 그대로예요. 옛 아이디는 로그인에 안 쓰이고, 다른 사람이 가져가지 못하게 묶어 둬요.',
                  style: dim,
                ),
                if (_error case final error?) ...[
                  const SizedBox(height: 12),
                  Semantics(
                    liveRegion: true,
                    child: Text(
                      error,
                      style: TextStyle(color: theme.colorScheme.error),
                    ),
                  ),
                ],
                const SizedBox(height: Look.groupGap),
                FilledButton(
                  key: const Key('secret-submit'),
                  onPressed: _busy ? null : () => unawaited(_submit()),
                  child: _busy
                      ? const SizedBox.square(
                          dimension: 18,
                          child: CircularProgressIndicator(strokeWidth: 2),
                        )
                      : Text(widget.password ? '비밀번호 바꾸기' : '아이디 바꾸기'),
                ),
              ],
            ),
          ),
        ),
      ),
    );
  }
}
