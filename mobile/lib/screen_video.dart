import 'dart:io';

import 'package:flutter/foundation.dart';
import 'package:flutter/services.dart';

/// 기기 화면 거울의 영상 장 머리(`docs/screen-stream.md` S2, `'F'` 틀) — (seq, 키프레임인가, 가로, 세로). 모자라면 null.
({int seq, bool key, int width, int height})? screenVideoHead(Uint8List b) {
  // 'F'(1) seq(4) cap_id(8) key(1) t_cap(8) t_enc(8) 가로(4) 세로(4)
  if (b.length < 38 || b[0] != 0x46) return null;
  final d = ByteData.sublistView(b);
  return (seq: d.getUint32(1), key: b[13] != 0, width: d.getUint32(30), height: d.getUint32(34));
}

/// 영상 장을 풀어 텍스처로 내주는 쪽 — 실제는 iOS 의 VideoToolbox(`ios/Runner/ScreenVideo.swift`), 시험은 가짜.
abstract class ScreenVideoSink {
  /// 텍스처 번호. 이 기기가 못 풀면 null — 그때는 JPEG 로 받는다.
  Future<int?> create();

  /// 장 하나를 풀게 넘긴다(순서대로).
  void push(Uint8List frame);

  /// 새 연결 — 키프레임부터 다시 푼다.
  void reset();

  void dispose();

  /// 풀기에 실패해 키프레임이 필요할 때.
  VoidCallback? onNeedKeyframe;
}

class NativeScreenVideo implements ScreenVideoSink {
  NativeScreenVideo() {
    _channel.setMethodCallHandler((call) async {
      if (call.method == 'keyframe') {
        final id = (call.arguments as Map?)?['id'];
        _sinks[id]?.onNeedKeyframe?.call();
      }
    });
  }

  static const _channel = MethodChannel('kasaterm/screen_video');
  static const _frames = BasicMessageChannel<ByteData?>(
    'kasaterm/screen_video/frames',
    BinaryCodec(),
  );
  static final Map<int, NativeScreenVideo> _sinks = {};

  int? _id;

  @override
  VoidCallback? onNeedKeyframe;

  /// iOS 앱에서만 — 시험·다른 OS 는 JPEG.
  static bool get supported => !kIsWeb && Platform.isIOS;

  @override
  Future<int?> create() async {
    if (!supported) return null;
    try {
      final id = await _channel.invokeMethod<int>('create');
      if (id == null) return null;
      _id = id;
      _sinks[id] = this;
      return id;
    } on PlatformException {
      return null;
    } on MissingPluginException {
      return null;
    }
  }

  @override
  void push(Uint8List frame) {
    final id = _id;
    if (id == null) return;
    // 앞 8바이트는 텍스처 번호 — 장마다 채널을 따로 두지 않는다.
    final msg = Uint8List(8 + frame.length);
    ByteData.sublistView(msg).setInt64(0, id, Endian.little);
    msg.setRange(8, msg.length, frame);
    _frames.send(ByteData.sublistView(msg));
  }

  /// 장 채널에 `R` 한 바이트로 — 메서드 채널로 따로 보내면 장과 차례가 엇갈려 첫 키프레임을 버린 채 기다릴 수 있다.
  @override
  void reset() => push(Uint8List.fromList(const [0x52]));

  @override
  void dispose() {
    final id = _id;
    _id = null;
    if (id == null) return;
    _sinks.remove(id);
    _channel.invokeMethod<void>('dispose', {'id': id});
  }
}
