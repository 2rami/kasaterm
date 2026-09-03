import 'package:flutter_secure_storage/flutter_secure_storage.dart';

/// 서버 주소 하나를 Keychain 에 둔다. 주소 속 slug 가 곧 자격이라 평문 저장소는
/// 쓰지 않는다.
class AddressStore {
  const AddressStore();

  static const _key = 'root';
  static const _storage = FlutterSecureStorage();

  Future<Uri?> load() async {
    final text = await _storage.read(key: _key);
    if (text == null || text.isEmpty) return null;
    return Uri.tryParse(text);
  }

  Future<void> save(Uri root) => _storage.write(key: _key, value: root.toString());

  Future<void> clear() => _storage.delete(key: _key);
}
