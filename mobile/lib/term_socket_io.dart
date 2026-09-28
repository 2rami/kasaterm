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

WebSocketChannel connectTermSocket(Uri uri, {List<String>? protocols}) {
  if (protocols == null) return WebSocketChannel.connect(uri);
  final origin = uri.replace(scheme: uri.scheme == 'wss' ? 'https' : 'http');
  final client = _HandshakeClient(origin);
  final channel = IOWebSocketChannel.connect(
    uri,
    protocols: protocols,
    customClient: client,
    connectTimeout: const Duration(seconds: 15),
  );
  channel.ready.then(
    (_) => client.close(),
    onError: (Object _) => client.close(force: true),
  );
  return channel;
}
