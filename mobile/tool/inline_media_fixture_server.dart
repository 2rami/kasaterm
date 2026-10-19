// 시뮬레이터 검증용 가짜 등록소 — `term/inline-media*` 를 test/fixtures/media 로 답한다.
// 진짜 카사텀·사용자 세션에 닿지 않고 앱의 받기·재생 길을 끝까지 돌린다(integration_test/inline_media_test.dart).
//
//   dart run tool/inline_media_fixture_server.dart [포트] [조각마다 지연 ms]
import 'dart:io';

import '../test/fixtures/inline_media_fixture.dart';

Future<void> main(List<String> args) async {
  final port = args.isNotEmpty ? int.parse(args[0]) : 8791;
  final delay = Duration(
    milliseconds: args.length > 1 ? int.parse(args[1]) : 150,
  );
  final media = loadMediaFixtures('test/fixtures/media');
  final server = await HttpServer.bind(InternetAddress.loopbackIPv4, port);
  stdout.writeln('inline-media fixture on http://127.0.0.1:$port/');
  await for (final req in server) {
    if (req.uri.path.endsWith('/file')) await Future<void>.delayed(delay);
    final r = inlineMediaReply(media, req.uri);
    stdout.writeln('${r.status} ${req.uri.path}?${req.uri.query}');
    req.response
      ..statusCode = r.status
      ..headers.contentType = ContentType.parse(r.type)
      ..add(r.body);
    await req.response.close();
  }
}
