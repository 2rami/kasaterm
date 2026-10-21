// 시뮬레이터 검증용 가짜 등록소 — `term/inline-media*` 를 test/fixtures/media 로 답한다.
// 진짜 카사텀·사용자 세션에 닿지 않고 앱의 창구·재생 길을 끝까지 돌린다(integration_test/inline_media_test.dart).
//
//   dart run tool/inline_media_fixture_server.dart [포트] [조각마다 지연 ms] [긴 영상 mp4]
//
// 긴 영상은 id 6666666666666666 으로 선다 — 저장소에 넣기엔 커서 실행 때 ffmpeg 로 만든다(끝에 moov 를 둔 것).
// `GET /__origin?down=1` 이면 조각 요청을 503 으로 답해 원본 끊김을 흉내 내고, `down=0` 으로 되살린다.
// 조각 요청마다 `file <ms> <id> <offset> <len>` 한 줄을 찍는다 — 탐색·닫기 뒤 요청이 멎는지 이 줄로 잰다.
import 'dart:io';

import '../test/fixtures/inline_media_fixture.dart';

Future<void> main(List<String> args) async {
  final port = args.isNotEmpty ? int.parse(args[0]) : 8791;
  final delay = Duration(
    milliseconds: args.length > 1 ? int.parse(args[1]) : 150,
  );
  final media = loadMediaFixtures('test/fixtures/media');
  if (args.length > 2) {
    final long = MediaFixture(
      id: '6666666666666666',
      kind: 'video',
      name: 'long.mp4',
      mime: 'video/mp4',
      bytes: File(args[2]).readAsBytesSync(),
      durationMs: 60000,
      width: 1280,
      height: 720,
    );
    media[long.id] = long;
  }
  var down = false;
  final server = await HttpServer.bind(InternetAddress.loopbackIPv4, port);
  stdout.writeln('inline-media fixture on http://127.0.0.1:$port/');
  // 관문처럼 요청을 겹쳐 받는다 — AVPlayer 는 앞머리와 끝(moov)을 함께 묻는다.
  server.listen((req) async {
    if (req.uri.path == '/__origin') {
      down = req.uri.queryParameters['down'] == '1';
      stdout.writeln('origin ${down ? 'down' : 'up'}');
      await (req.response..statusCode = 204).close();
      return;
    }
    final file = req.uri.path.endsWith('/file');
    if (file) {
      final q = req.uri.queryParameters;
      stdout.writeln(
        'file ${DateTime.now().millisecondsSinceEpoch} ${q['id']} ${q['offset']} ${q['len']}${down ? ' down' : ''}',
      );
      await Future<void>.delayed(delay);
    }
    final r = file && down
        ? const FixtureReply(503, [], 'text/plain')
        : inlineMediaReply(media, req.uri);
    req.response
      ..statusCode = r.status
      ..headers.contentType = ContentType.parse(r.type)
      ..add(r.body);
    await req.response.close();
  });
}
