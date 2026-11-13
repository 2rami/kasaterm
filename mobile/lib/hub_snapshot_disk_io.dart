import 'dart:io';

import 'package:path_provider/path_provider.dart';

import 'hub_snapshot_disk_stub.dart';

export 'hub_snapshot_disk_stub.dart' show HubSnapshotDisk;

HubSnapshotDisk? openHubSnapshotDisk() => IoHubSnapshotDisk();

/// OS 가 비우는 Cache 대신 Application Support — 지난 화면이 앱을 껐다 켜도 남아야 한다.
class IoHubSnapshotDisk implements HubSnapshotDisk {
  IoHubSnapshotDisk({Directory? root}) : _root = root;
  Directory? _root;

  Future<Directory> _dir() async =>
      _root ??= Directory('${(await getApplicationSupportDirectory()).path}/hub-snapshots');

  @override
  Future<String?> read(String key) async {
    final file = File('${(await _dir()).path}/$key.json');
    return await file.exists() ? file.readAsString() : null;
  }

  @override
  Future<void> write(String key, String text) async {
    final dir = await _dir();
    await dir.create(recursive: true);
    final tmp = File('${dir.path}/$key.json.tmp');
    await tmp.writeAsString(text, flush: true);
    await tmp.rename('${dir.path}/$key.json');
  }

  @override
  Future<void> delete(String key) async {
    final dir = await _dir();
    for (final name in ['$key.json', '$key.json.tmp']) {
      final file = File('${dir.path}/$name');
      if (await file.exists()) await file.delete();
    }
  }
}
