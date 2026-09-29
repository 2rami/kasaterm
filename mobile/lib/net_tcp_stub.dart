import 'server.dart';

/// 웹에는 로컬 소켓을 열 길이 없다.
class NetTcpBridge {
  static Future<NetTcpBridge> start(
    Server server, {
    required int port,
    String? machine,
  }) => Future.error(UnsupportedError('이 기기에서는 개발 서버를 끌어올 수 없어요'));

  int get localPort => 0;
  Future<void> close() async {}
}
