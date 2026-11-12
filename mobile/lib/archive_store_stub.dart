import 'archive_model.dart';

Future<ArchiveStore> openArchiveStore() async => _Unavailable();

class _Unavailable implements ArchiveStore {
  @override
  bool get persistent => false;
  @override
  Future<List<ArchiveEntry>> list(String scope) async => [];
  @override
  Future<SavedConversation?> read(String scope, String id) async => null;
  @override
  Future<void> remove(String scope, String id) async {}
  @override
  Future<bool> excluded(String scope, String id) async => false;
  @override
  Future<void> save(
    String scope,
    SavedConversation conversation, {
    required bool Function() current,
  }) async {
    throw const ArchiveException('오프라인 저장은 모바일 앱에서 사용할 수 있어요.');
  }
}
