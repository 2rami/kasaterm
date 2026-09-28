import 'package:web_socket_channel/web_socket_channel.dart';

import 'relay_account.dart';

WebSocketChannel connectTermSocket(Uri uri, {List<String>? protocols}) {
  // Browser WebSocket cannot enforce our no-redirect credential boundary.
  if (protocols != null) {
    throw const AccountException('계정 터미널은 카사모바일 앱에서 열어 주세요.');
  }
  return WebSocketChannel.connect(uri);
}
