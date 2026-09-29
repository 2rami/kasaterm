import 'package:flutter_test/flutter_test.dart';
import 'package:kasaterm_mobile/screens/dev_server.dart';

void main() {
  test('보여 주기 링크 중 데스크톱 localhost 만 앱 안 웹뷰로', () {
    ({int port, String path})? at(String s) => desktopLocal(Uri.parse(s));
    expect(at('http://localhost:3000/deals/7?tab=notes'), (port: 3000, path: '/deals/7?tab=notes'));
    expect(at('http://0.0.0.0:5173'), (port: 5173, path: '/'));
    expect(at('http://127.0.0.1:8080/x'), (port: 8080, path: '/x'));
    expect(at('http://[::1]:4000/'), (port: 4000, path: '/'));
    expect(at('https://localhost/'), (port: 443, path: '/'));
    for (final s in [
      'https://example.com/',
      'https://abc.trycloudflare.com/x',
      'http://10.1.2.3:3000/',
      'http://mini.local:3000/',
      'kasaterm://open?pane=%1',
    ]) {
      expect(at(s), isNull, reason: s);
    }
  });
}
