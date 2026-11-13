/// 허브 스냅숏 파일 하나 — 이름은 연결 범위의 해시라 계정·주소가 드러나지 않는다.
abstract class HubSnapshotDisk {
  Future<String?> read(String key);
  Future<void> write(String key, String text);
  Future<void> delete(String key);
}

/// 파일이 없는 판(웹)은 앱이 사는 동안만 기억한다.
HubSnapshotDisk? openHubSnapshotDisk() => null;
