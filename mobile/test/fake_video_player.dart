import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:video_player_platform_interface/video_player_platform_interface.dart';

/// AVPlayer 대신 — 연 파일·재생·멈춤·찾기·해제를 기록만 한다.
class FakeVideoPlayer extends VideoPlayerPlatform {
  final calls = <String>[];
  final _events = <int, StreamController<VideoEvent>>{};
  final _positions = <int, Duration>{};
  int _next = 1;
  bool failInit = false;
  Duration duration = const Duration(seconds: 3);
  Size size = const Size(320, 180);

  int get live => _events.length;

  @override
  Future<void> init() async {}

  @override
  Future<int?> createWithOptions(VideoCreationOptions options) async {
    final id = _next++;
    calls.add('open ${options.dataSource.uri}');
    final events = _events[id] = StreamController<VideoEvent>();
    scheduleMicrotask(() {
      if (failInit) {
        events.addError(
          PlatformException(code: 'VideoError', message: 'unsupported'),
        );
      } else {
        events.add(
          VideoEvent(
            eventType: VideoEventType.initialized,
            duration: duration,
            size: size,
          ),
        );
      }
    });
    return id;
  }

  @override
  Future<void> dispose(int playerId) async {
    calls.add('dispose');
    await _events.remove(playerId)?.close();
  }

  @override
  Stream<VideoEvent> videoEventsFor(int playerId) => _events[playerId]!.stream;

  @override
  Future<void> play(int playerId) async => calls.add('play');

  @override
  Future<void> pause(int playerId) async => calls.add('pause');

  /// 흘려 받는 원본처럼 찾기가 오래 걸리게 — 풀 때까지 찾기가 끝나지 않는다.
  Completer<void>? seekGate;

  @override
  Future<void> seekTo(int playerId, Duration position) async {
    calls.add('seek ${position.inMilliseconds}');
    await seekGate?.future;
    _positions[playerId] = position;
  }

  @override
  Future<Duration> getPosition(int playerId) async =>
      _positions[playerId] ?? Duration.zero;

  /// 재생이 [at] 까지 흘러간 것처럼 — 컨트롤러는 다음 위치 묻기(0.5초마다)에서 받는다.
  void moveTo(Duration at) {
    for (final id in _events.keys) {
      _positions[id] = at;
    }
  }

  /// 연결이 끊겨 AVPlayer 가 오류를 낸 것처럼.
  void fail() {
    for (final e in _events.values) {
      e.addError(PlatformException(code: 'VideoError', message: 'lost'));
    }
  }

  /// 재생이 끝까지 간 것처럼.
  void complete() {
    for (final e in _events.entries) {
      _positions[e.key] = duration;
      e.value.add(VideoEvent(eventType: VideoEventType.completed));
    }
  }

  @override
  Future<void> setLooping(int playerId, bool looping) async {}
  @override
  Future<void> setVolume(int playerId, double volume) async {}
  @override
  Future<void> setPlaybackSpeed(int playerId, double speed) async {}
  @override
  Future<void> setMixWithOthers(bool mixWithOthers) async {}

  @override
  Widget buildViewWithOptions(VideoViewOptions options) =>
      const ColoredBox(color: Color(0xff2b4a6f));
}
