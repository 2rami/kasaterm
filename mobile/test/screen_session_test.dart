import 'dart:async';
import 'dart:convert';
import 'dart:io';
import 'dart:typed_data';

import 'package:flutter/painting.dart';

import 'package:flutter_test/flutter_test.dart';
import 'package:kasaterm_mobile/screen_session.dart';
import 'package:kasaterm_mobile/screen_video.dart';
import 'package:kasaterm_mobile/server.dart';

/// 1×1 PNG — 디코더는 형식을 가리지 않아 JPEG 자리에 그대로 쓴다.
final _pixel = base64Decode(
  'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==',
);

/// 시험 바인딩은 HttpClient 를 가짜로 바꾼다 — 소켓 악수만은 진짜로 붙어야 한다.
class _NativeHttp extends HttpOverrides {}

Uint8List _frame(int seq, List<int> image) {
  final meta = utf8.encode(jsonEncode({'w': 1, 'h': 1, 'd': 1}));
  final head = ByteData(6)
    ..setUint32(0, seq)
    ..setUint16(4, meta.length);
  return Uint8List.fromList([...head.buffer.asUint8List(), ...meta, ...image]);
}

/// 영상 장 `'F'` 틀 — `kasa_remote::screen_stream::Frame::encode` 와 같은 꼴.
Uint8List _video(int seq, {bool key = false, int w = 2016, int h = 1310}) {
  final head = ByteData(38)
    ..setUint8(0, 0x46)
    ..setUint32(1, seq)
    ..setUint64(5, seq)
    ..setUint8(13, key ? 1 : 0)
    ..setUint32(30, w)
    ..setUint32(34, h);
  return Uint8List.fromList([...head.buffer.asUint8List(), key ? 1 : 0, if (key) ...[0, 1, 0x40], 9, 9]);
}

class _FakeVideo implements ScreenVideoSink {
  _FakeVideo(this.id);

  final int? id;
  final pushed = <Uint8List>[];
  int resets = 0;

  @override
  VoidCallback? onNeedKeyframe;

  @override
  Future<int?> create() async => id;

  @override
  void push(Uint8List frame) => pushed.add(frame);

  @override
  void reset() => resets++;

  @override
  void dispose() {}
}

/// 영상 방식 소켓 — `hello` 를 [helloCodec] 으로 보내고 [frames] 를 잇달아 보낸다. 받은 쿼리·말을 돌려준다.
Future<(HttpServer, List<Map<String, String>>, StreamController<Map<String, Object?>>)> _videoServer({
  String? helloCodec,
  required List<Uint8List> frames,
}) async {
  final http = await HttpServer.bind(InternetAddress.loopbackIPv4, 0);
  final queries = <Map<String, String>>[];
  final received = StreamController<Map<String, Object?>>.broadcast();
  http.listen((request) async {
    queries.add(request.uri.queryParameters);
    final ws = await WebSocketTransformer.upgrade(request);
    ws.listen((data) {
      if (data is String) received.add((jsonDecode(data) as Map).cast<String, Object?>());
    });
    ws.add(jsonEncode({
      't': 'hello',
      'display': 1,
      'displays': [
        {'id': 1, 'width': 2016, 'height': 1310, 'main': true},
      ],
      'codec': ?helloCodec,
    }));
    for (final f in frames) {
      ws.add(f);
    }
  });
  return (http, queries, received);
}

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();

  test('video head reads seq, key and size and refuses short or foreign bytes', () {
    final h = screenVideoHead(_video(7, key: true))!;
    expect((h.seq, h.key, h.width, h.height), (7, true, 2016, 1310));
    expect(screenVideoHead(Uint8List(37)), isNull);
    expect(screenVideoHead(_frame(1, _pixel)), isNull, reason: 'JPEG 틀은 영상 틀이 아니다');
  });

  test('video mode asks for hevc, hands frames to the decoder in order without acks, and asks keyframes for it', () => HttpOverrides.runWithHttpOverrides(() async {
    final (http, queries, received) = await _videoServer(
      helloCodec: 'hevc',
      frames: [_video(1, key: true), _video(2), _video(3)],
    );
    final sink = _FakeVideo(5);
    final session = ScreenSession(
      Server(Uri.parse('http://127.0.0.1:${http.port}/')),
      video: sink,
      wantVideo: (_) => true,
    );
    addTearDown(() async {
      session.dispose();
      await http.close(force: true);
    });
    await Future<void>.delayed(Duration.zero);
    session.connect();
    final deadline = DateTime.now().add(const Duration(seconds: 5));
    while (sink.pushed.length < 3 && DateTime.now().isBefore(deadline)) {
      await Future<void>.delayed(const Duration(milliseconds: 20));
    }
    expect(queries.last['codec'], 'hevc');
    expect(queries.last['fps'], '60');
    expect(session.videoMode, isTrue);
    expect(session.textureId, 5);
    expect(sink.resets, 1, reason: '새 연결은 키프레임부터');
    expect(sink.pushed.map((f) => screenVideoHead(f)!.seq), [1, 2, 3]);
    expect(session.hasPicture, isTrue);
    expect(session.pictureSize, const Size(2016, 1310));
    expect(session.state, ScreenState.live);

    final key = received.stream.firstWhere((m) => m['t'] == 'keyframe');
    sink.onNeedKeyframe!();
    await key.timeout(const Duration(seconds: 5));
    await Future<void>.delayed(const Duration(milliseconds: 50));
    expect(await received.stream.any((m) => m['t'] == 'ack').timeout(const Duration(milliseconds: 200), onTimeout: () => false), isFalse,
        reason: '영상 장에는 ack 가 없다');
  }, _NativeHttp()));

  test('an old desktop answers jpeg to a video ask and the session falls back', () => HttpOverrides.runWithHttpOverrides(() async {
    final (http, queries, received) = await _videoServer(frames: [_frame(1, _pixel)]);
    final sink = _FakeVideo(5);
    final session = ScreenSession(
      Server(Uri.parse('http://127.0.0.1:${http.port}/')),
      video: sink,
      wantVideo: (_) => true,
    );
    addTearDown(() async {
      session.dispose();
      await http.close(force: true);
    });
    await Future<void>.delayed(Duration.zero);
    final ack = received.stream.firstWhere((m) => m['t'] == 'ack');
    session.connect();
    await ack.timeout(const Duration(seconds: 5));
    expect(queries.last['codec'], 'hevc');
    expect(session.videoMode, isFalse);
    expect(sink.pushed, isEmpty);
    expect(session.image?.width, 1);
  }, _NativeHttp()));

  test('no decoder or the gateway path keeps jpeg', () => HttpOverrides.runWithHttpOverrides(() async {
    final (http, queries, _) = await _videoServer(frames: const []);
    final none = ScreenSession(Server(Uri.parse('http://127.0.0.1:${http.port}/')), video: _FakeVideo(null), wantVideo: (_) => true);
    final gateway = ScreenSession(Server(Uri.parse('http://127.0.0.1:${http.port}/')), video: _FakeVideo(5));
    addTearDown(() async {
      none.dispose();
      gateway.dispose();
      await http.close(force: true);
    });
    await Future<void>.delayed(Duration.zero);
    none.connect();
    gateway.connect();
    final deadline = DateTime.now().add(const Duration(seconds: 5));
    while (queries.length < 2 && DateTime.now().isBefore(deadline)) {
      await Future<void>.delayed(const Duration(milliseconds: 20));
    }
    expect(queries.map((q) => q['codec']), [null, null], reason: '텍스처가 없거나 직통이 아니면 영상을 청하지 않는다');
  }, _NativeHttp()));

  test('frame head reads seq and where the image starts', () {
    final b = _frame(7, [1, 2, 3]);
    final (seq, start) = screenFrameHead(b)!;
    expect(seq, 7);
    expect(b.sublist(start), [1, 2, 3]);
    expect(screenFrameHead(Uint8List(5)), isNull);
    expect(screenFrameHead(Uint8List.fromList([0, 0, 0, 1, 0, 9])), isNull);
  });

  test('decodes a frame, acks after, switches display and shows denial', () => HttpOverrides.runWithHttpOverrides(() async {
    final http = await HttpServer.bind(InternetAddress.loopbackIPv4, 0);
    final received = StreamController<Map<String, Object?>>.broadcast();
    final queries = <Map<String, String>>[];
    WebSocket? socket;
    http.listen((request) async {
      queries.add(request.uri.queryParameters);
      final ws = await WebSocketTransformer.upgrade(request);
      socket = ws;
      ws.listen((data) {
        if (data is String) received.add((jsonDecode(data) as Map).cast<String, Object?>());
      });
      ws.add(jsonEncode({
        't': 'hello',
        'display': 1,
        'displays': [
          {'id': 1, 'width': 1512, 'height': 982, 'main': true},
          {'id': 2, 'width': 2560, 'height': 1440, 'main': false},
        ],
      }));
      ws.add(_frame(1, _pixel));
    });
    final server = Server(Uri.parse('http://127.0.0.1:${http.port}/'));
    final session = ScreenSession(server);
    addTearDown(() async {
      session.dispose();
      await http.close(force: true);
    });

    final ack = received.stream.firstWhere((m) => m['t'] == 'ack');
    session.connect();
    expect((await ack.timeout(const Duration(seconds: 5)))['seq'], 1);
    expect(session.image?.width, 1);
    expect(session.state, ScreenState.live);
    expect(session.displays.map((d) => d.id), [1, 2]);
    expect(queries.single['w'], '1280', reason: '관문 길은 작게 받는다');

    final tune = received.stream.firstWhere((m) => m['t'] == 'tune');
    session.pickDisplay(2);
    expect((await tune.timeout(const Duration(seconds: 5)))['display'], 2);

    final told = Completer<void>();
    void listener() {
      if (session.state == ScreenState.denied && !told.isCompleted) told.complete();
    }
    session.addListener(listener);
    socket!.add(jsonEncode({'t': 'denied', 'reason': 'permission'}));
    await told.future.timeout(const Duration(seconds: 5));
    session.removeListener(listener);
    expect(session.denied, 'permission');
  }, _NativeHttp()));

  test('screen fraction clamps to the picture box', () {
    expect(screenFraction(const Offset(50, 25), const Size(200, 100)), (0.25, 0.25));
    expect(screenFraction(const Offset(-5, 300), const Size(200, 100)), (0.0, 1.0));
    expect(screenFraction(const Offset(5, 5), Size.zero), (0.5, 0.5));
  });

  test('control sends pointer, wheel, key and text, and shows input denial', () => HttpOverrides.runWithHttpOverrides(() async {
    final http = await HttpServer.bind(InternetAddress.loopbackIPv4, 0);
    final received = StreamController<Map<String, Object?>>.broadcast();
    final heard = <Map<String, Object?>>[];
    WebSocket? socket;
    http.listen((request) async {
      final ws = await WebSocketTransformer.upgrade(request);
      socket = ws;
      ws.listen((data) {
        if (data is! String) return;
        final m = (jsonDecode(data) as Map).cast<String, Object?>();
        if (m['t'] != 'ack') heard.add(m);
        received.add(m);
      });
      ws.add(jsonEncode({
        't': 'hello',
        'display': 1,
        'displays': [
          {'id': 1, 'width': 1512, 'height': 982, 'main': true},
        ],
      }));
      ws.add(_frame(1, _pixel));
    });
    final session = ScreenSession(Server(Uri.parse('http://127.0.0.1:${http.port}/')));
    addTearDown(() async {
      session.dispose();
      await http.close(force: true);
    });
    final ack = received.stream.firstWhere((m) => m['t'] == 'ack');
    session.connect();
    await ack.timeout(const Duration(seconds: 5));
    expect(session.displaySize, const Size(1512, 982));

    final last = received.stream.firstWhere((m) => m['t'] == 'text');
    session.click(0.25, 0.5);
    session.click(0.1, 0.2, right: true);
    session.scroll(0.5, 0.5, 0, 0);
    session.scroll(0.5, 0.5, 0, -40);
    session.key('Enter');
    session.typeText('');
    session.typeText('안녕');
    await last.timeout(const Duration(seconds: 5));
    expect(heard, [
      {'t': 'pointer', 'x': 0.25, 'y': 0.5, 'b': 1},
      {'t': 'pointer', 'x': 0.25, 'y': 0.5, 'b': 0},
      {'t': 'pointer', 'x': 0.1, 'y': 0.2, 'b': 2},
      {'t': 'pointer', 'x': 0.1, 'y': 0.2, 'b': 0},
      {'t': 'scroll', 'x': 0.5, 'y': 0.5, 'dx': 0, 'dy': -40},
      {'t': 'key', 'k': 'Enter'},
      {'t': 'text', 's': '안녕'},
    ]);

    Future<void> told(String? want) async {
      final done = Completer<void>();
      void listener() {
        if (session.inputDenied == want && !done.isCompleted) done.complete();
      }
      session.addListener(listener);
      await done.future.timeout(const Duration(seconds: 5));
      session.removeListener(listener);
    }

    socket!.add(jsonEncode({'t': 'input', 'ok': false, 'reason': 'permission'}));
    await told('permission');
    socket!.add(jsonEncode({'t': 'input', 'ok': true}));
    await told(null);
  }, _NativeHttp()));
}
