import 'package:flutter/widgets.dart';

/// 웹 화면이 앱으로 건너뛸 때 주는 링크 `kasaterm://open?root=…&machine=…&pane=…`.
/// 폰의 사파리(슬랙 알림 링크·주소 직접 열기)로 들어와도 앱에서 그 학생을 연다.
class AppLink {
  const AppLink({this.root, this.machine, this.pane});

  static const scheme = 'kasaterm';

  final String? root;
  final String? machine;
  final String? pane;

  /// 엔진이 스킴을 벗기고 `/?pane=…` 꼴로 줄 수도 있어 쿼리만으로도 알아본다.
  static AppLink? parse(Uri? u) {
    if (u == null) return null;
    final q = u.queryParameters;
    if (u.scheme != scheme && !q.containsKey('pane') && !q.containsKey('root')) return null;
    String? nz(String? s) => (s == null || s.isEmpty) ? null : s;
    return AppLink(root: nz(q['root']), machine: nz(q['machine']), pane: nz(q['pane']));
  }
}

/// 링크를 받는 옵저버. **runApp 전에** 세워야 한다 — WidgetsApp 보다 뒤에 서면
/// WidgetsApp 이 그 링크를 이름 있는 라우트로 밀어 넣다 「generator 없음」으로 터진다.
/// 화면이 아직 없을 때(앱이 링크로 켜진 직후) 온 링크는 붙잡아 두었다가 넘긴다.
class AppLinkObserver with WidgetsBindingObserver {
  AppLinkObserver._();

  static final AppLinkObserver instance = AppLinkObserver._();

  Future<void> Function(AppLink)? _handler;
  AppLink? _pending;

  void install() {
    final binding = WidgetsBinding.instance;
    binding.addObserver(this);
    _pending = AppLink.parse(Uri.tryParse(binding.platformDispatcher.defaultRouteName));
  }

  void attach(Future<void> Function(AppLink) handler) {
    _handler = handler;
    final p = _pending;
    _pending = null;
    if (p != null) handler(p);
  }

  void detach() => _handler = null;

  @override
  Future<bool> didPushRouteInformation(RouteInformation routeInformation) async {
    final link = AppLink.parse(routeInformation.uri);
    if (link == null) return false;
    final h = _handler;
    if (h == null) {
      _pending = link;
    } else {
      await h(link);
    }
    return true;
  }
}
