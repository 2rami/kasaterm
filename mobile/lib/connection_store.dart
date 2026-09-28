import 'dart:convert';

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
