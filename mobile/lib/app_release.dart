import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:url_launcher/url_launcher.dart';

/// 이 판의 빌드 번호 — `tool/ios-archive.sh` 가 KASA_BUILD 로 굽는다. 개발 설치(phone.sh·flutter run)는 비어 있다.
const kasaBuild = String.fromEnvironment('KASA_BUILD');

/// 관문에 올라온 Ad Hoc 판(`/relay/install/latest`, `mobile/tool/adhoc.sh` 가 올린다).
class AppRelease {
  const AppRelease(this.version, this.build, this.install);

  final String version;
  final String build;

  /// `itms-services://…` — 열면 iOS 가 바로 「설치」를 묻는다.
  final Uri install;

  /// 빌드 번호는 시각(yyMMddHHmm)이라 수로 견준다. 개발 설치는 어느 판인지 몰라 알리지 않는다.
  bool get newer {
    final mine = int.tryParse(kasaBuild);
    final theirs = int.tryParse(build);
    return mine != null && theirs != null && theirs > mine;
  }

  Future<bool> open() =>
      launchUrl(install, mode: LaunchMode.externalApplication);

  /// 같은 판의 사파리 설치 화면(`…/relay/install/<token>/`) — manifest 주소의 디렉터리다.
  Uri? get page {
    final manifest = Uri.tryParse(install.queryParameters['url'] ?? '');
    if (manifest == null || !manifest.isScheme('https')) return null;
    return manifest.resolve('./');
  }
}

/// 관문이 판이 바뀌는 순간 답하는 길의 한 번 답. [waits] 가 없으면 옛 관문이라 붙들지 않고 곧바로 답했다.
class ReleaseWatch {
  const ReleaseWatch(this.release, {required this.waits});

  /// 올라온 판이 없으면 null.
  final AppRelease? release;
  final bool waits;
}

/// 새 판 [설치]. iOS 는 앞에 떠 있는 앱을 갈아 끼우지 않아(홈으로 가야 설치가 시작된다) 설치 확인 창이 닫혀 앱으로
/// 돌아오면 잠깐 알린 뒤 앱이 스스로 홈으로 비켜선다. 설치를 마친 앱을 스스로 다시 켤 길은 없어 「눌러서 열기」 알림을
/// 남긴다. 확인 창에서 「취소」를 눌렀는지는 앱이 알 수 없어 알림 띠의 [취소]로 멈춘다.
///
/// 확인 창이 앱을 비활성으로 만들지 않거나 열기가 실패로 답하면 돌아온 순간을 못 잡는다 — 그때는 [앱 닫기] 단추 띠를
/// 남겨 사람이 한 번에 비켜서게 한다(10-07 실기에서 앱이 비켜서지 않았다). 단계마다 [releaseLog] 로 남긴다.
Future<void> installRelease(
  BuildContext context,
  AppRelease release, {
  String from = '',
  Future<bool> Function()? open,
  Duration wait = const Duration(minutes: 2),
}) async {
  final messenger = ScaffoldMessenger.maybeOf(context);
  unawaited(_bridge('arm'));
  releaseLog('[설치] 누름 → ${release.version} (${release.build}) · $from');
  final watch = _InstallWatch(wait);
  var opened = false;
  try {
    opened = await (open ?? release.open)();
  } on Object catch (e) {
    releaseLog('설치 창 열기 오류 $e');
  }
  releaseLog('설치 창 열기 → $opened');
  if (messenger == null) {
    watch.finish(_Return.timeout);
    return;
  }
  ScaffoldFeatureController<SnackBar, SnackBarClosedReason>? manual;
  final first = await Future.any<_Return?>([
    watch.done,
    Future.delayed(releaseLeaveGrace, () => null),
  ]);
  if (first == null && !watch.left) {
    releaseLog('${releaseLeaveGrace.inSeconds}초 안에 비활성이 안 됨 — [앱 닫기] 띠');
    manual = messenger.showSnackBar(
      SnackBar(
        content: const Text('설치를 눌렀다면 앱이 비켜서야 설치가 시작돼요'),
        duration: releaseManualBand,
        persist: false,
        action: SnackBarAction(label: '앱 닫기', onPressed: () {}),
      ),
    );
    unawaited(
      manual.closed.then((r) {
        if (r == SnackBarClosedReason.action) watch.finish(_Return.pressed);
      }),
    );
  }
  final how = await watch.done;
  releaseLog('기다림 끝 · ${how.name}');
  switch (how) {
    case _Return.timeout || _Return.gone:
      manual?.close();
      return;
    case _Return.pressed:
      break;
    case _Return.back:
      manual?.close();
      final band = messenger.showSnackBar(
        SnackBar(
          content: const Text('새 판을 설치하려고 앱을 닫아요 · 끝나면 알림을 눌러 열어요'),
          duration: releaseStepAsideDelay,
          // 단추가 달린 띠는 기본으로 안 사라진다 — 그러면 영영 비켜서지 않는다.
          persist: false,
          action: SnackBarAction(label: '취소', onPressed: () {}),
        ),
      );
      final closed = await band.closed;
      releaseLog('띠 닫힘 · ${closed.name}');
      if (closed == SnackBarClosedReason.action) return;
  }
  await _bridge('stepAside', {
    'title': 'KASATERM 새 판 ${release.version} (${release.build})',
    'body': '설치가 끝나면 눌러서 열어요',
    'after': releaseOpenNoticeAfter.inSeconds,
    if (release.page case final page?) 'page': page.toString(),
  });
}

/// 비켜서기 전에 [취소]를 누를 틈.
const releaseStepAsideDelay = Duration(seconds: 3);

/// 설치 창을 연 뒤 이만큼 안에 앱이 비활성이 안 되면 돌아온 순간을 못 잡는 것으로 본다.
const releaseLeaveGrace = Duration(seconds: 2);

/// [앱 닫기] 띠를 두는 시간 — 확인 창을 읽고 누를 만큼.
const releaseManualBand = Duration(seconds: 60);

/// 「눌러서 열기」 알림을 띄울 때 — 29MB 받기와 설치가 보통 이 안에 끝난다. 새 판이 먼저 켜지면 다리가 알림을 거둔다.
const releaseOpenNoticeAfter = Duration(seconds: 45);

const _releaseChannel = MethodChannel('kasaterm/release');

/// 설치 기록 한 줄 — 다리가 시각·빌드를 붙여 폰에 남긴다(설정 「지난 설치 기록」).
void releaseLog(String line) => unawaited(_bridge('log', {'line': line}));

/// 남은 설치 기록(오래된 것부터). 다리가 없는 판은 빈 목록.
Future<List<String>> releaseTrail() async {
  try {
    return (await _releaseChannel.invokeListMethod<String>('trail')) ??
        const [];
  } on MissingPluginException {
    return const [];
  }
}

Future<void> _bridge(String method, [Object? args]) async {
  try {
    await _releaseChannel.invokeMethod<void>(method, args);
  } on MissingPluginException {
    // 다리가 없는 판(웹·시험)은 사람이 홈으로 간다.
  } on PlatformException {
    // 기록·비켜서기가 실패해도 설치 자체는 iOS 가 이어 간다.
  }
}

enum _Return { back, pressed, gone, timeout }

/// 설치 확인 창이 떠 앱이 잠깐 비활성이 됐다가 돌아오는 순간([_Return.back]). 앱이 아예 뒤로 가면 사람이 홈으로 간
/// 것이라 거기서 끝낸다([_Return.gone]).
class _InstallWatch {
  _InstallWatch(Duration wait) {
    _listener = AppLifecycleListener(onStateChange: _changed);
    _timer = Timer(wait, () => finish(_Return.timeout));
  }

  final _done = Completer<_Return>();
  late final AppLifecycleListener _listener;
  late final Timer _timer;
  bool left = false;

  Future<_Return> get done => _done.future;

  void _changed(AppLifecycleState state) {
    releaseLog('앱 상태 ${state.name}');
    switch (state) {
      case AppLifecycleState.inactive:
        left = true;
      case AppLifecycleState.resumed:
        if (left) finish(_Return.back);
      case AppLifecycleState.hidden ||
          AppLifecycleState.paused ||
          AppLifecycleState.detached:
        finish(_Return.gone);
    }
  }

  void finish(_Return how) {
    if (_done.isCompleted) return;
    _timer.cancel();
    _listener.dispose();
    _done.complete(how);
  }
}
