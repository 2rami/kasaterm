import 'dart:io';

import 'package:web_socket_channel/io.dart';
import 'package:web_socket_channel/web_socket_channel.dart';

import 'relay_account.dart';

class _HandshakeClient implements HttpClient {
  _HandshakeClient(this.origin);
  final Uri origin;
  final HttpClient _inner = HttpClient();

  @override
  Future<HttpClientRequest> openUrl(String method, Uri url) async {
    if (!sameOrigin(origin, url) || url.userInfo.isNotEmpty) {
      throw const AccountException('소켓 연결 주소가 바뀌었어요.');
    }
    final request = await _inner.openUrl(method, url);
    request.followRedirects = false;
    return request;
  }

  @override
  void close({bool force = false}) => _inner.close(force: force);

  @override
  dynamic noSuchMethod(Invocation invocation) => super.noSuchMethod(invocation);
}

/// [ping] 마다 핑을 보내고 그만큼 안에 퐁이 안 오면 닫는다. 폰이 망을 갈아타거나 앱이 멈춘 사이 끊긴 소켓은 닫힘
/// 신호 없이 반쯤 열린 채 남아, 핑이 없으면 화면이 멈춘 채 다시 붙지 않았다.
WebSocketChannel connectTermSocket(
  Uri uri, {
  List<String>? protocols,
  Duration? ping,
}) {
  if (protocols == null) return IOWebSocketChannel.connect(uri, pingInterval: ping);
  final origin = uri.replace(scheme: uri.scheme == 'wss' ? 'https' : 'http');
  final client = _HandshakeClient(origin);
  final channel = IOWebSocketChannel.connect(
    uri,
    protocols: protocols,
    customClient: client,
    pingInterval: ping,
    connectTimeout: const Duration(seconds: 15),
  );
  channel.ready.then(
    (_) => client.close(),
    onError: (Object _) => client.close(force: true),
  );
  return channel;
}
