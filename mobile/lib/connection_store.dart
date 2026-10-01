import 'dart:convert';
import 'dart:math';

import 'package:flutter_secure_storage/flutter_secure_storage.dart';

import 'relay_account.dart';

class SavedConnection {
  const SavedConnection({this.account, this.legacyRoot});
  final AccountSession? account;
  final Uri? legacyRoot;
}

class ConnectionStore {
  const ConnectionStore();
  static const _storage = FlutterSecureStorage();
  static const key = 'connection.v1';

  Future<SavedConnection?> load() async {
    final text = await _storage.read(key: key);
    if (text == null) {
      final legacy = await _storage.read(key: 'root');
      return legacy == null
          ? null
          : SavedConnection(legacyRoot: Uri.tryParse(legacy));
    }
    try {
      final json = jsonDecode(text) as Map<String, dynamic>;
      return switch (json['kind']) {
        'account' => SavedConnection(account: AccountSession.fromJson(json)),
        'legacy' => SavedConnection(
          legacyRoot: Uri.parse(json['root'] as String),
        ),
        _ => const SavedConnection(),
      };
    } catch (_) {
      return const SavedConnection();
    }
  }

  /// 이 설치의 고정 id — Google·GitHub 로그인이 관문에 「이 폰」을 대는 값(`machine_id`). 로그아웃해도
  /// 바뀌지 않아야 같은 폰의 연결 요청이 그 폰의 로그인과 맞는다.
  Future<String> installId() async {
    const key = 'install-id.v1';
    final saved = await _storage.read(key: key);
    if (saved != null && RegExp(r'^[0-9a-f]{32}$').hasMatch(saved)) return saved;
    final rng = Random.secure();
    final id = List.generate(16, (_) => rng.nextInt(256).toRadixString(16).padLeft(2, '0')).join();
    await _storage.write(key: key, value: id);
    return id;
  }

  Future<void> save(SavedConnection connection) async {
    // One Keychain item prevents mixing the old account with a newly written token.
    await _storage.write(
      key: key,
      value: jsonEncode(
        connection.account?.toJson() ??
            (connection.legacyRoot == null
                ? {'kind': 'none'}
                : {'kind': 'legacy', 'root': connection.legacyRoot.toString()}),
      ),
    );
    await _storage.delete(key: 'root');
  }
}
