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

  Future<bool> open() => launchUrl(install, mode: LaunchMode.externalApplication);
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
Future<void> installRelease(
  BuildContext context,
  AppRelease release, {
  Future<bool> Function()? open,
  Duration wait = const Duration(minutes: 2),
}) async {
  final messenger = ScaffoldMessenger.maybeOf(context);
  final (back, cancel) = _returnAfterLeaving(wait);
  if (!await (open ?? release.open)()) {
    cancel();
    return;
  }
  if (!await back || messenger == null) return;
  final band = messenger.showSnackBar(
    SnackBar(
      content: const Text('새 판을 설치하려고 앱을 닫아요 · 끝나면 알림을 눌러 열어요'),
      duration: releaseStepAsideDelay,
      // 단추가 달린 띠는 기본으로 안 사라진다 — 그러면 영영 비켜서지 않는다.
      persist: false,
      action: SnackBarAction(label: '취소', onPressed: () {}),
    ),
  );
  if (await band.closed == SnackBarClosedReason.action) return;
  try {
    await _releaseChannel.invokeMethod('stepAside', {
      'title': 'KASATERM 새 판 ${release.version} (${release.build})',
      'body': '설치가 끝나면 눌러서 열어요',
      'after': releaseOpenNoticeAfter.inSeconds,
    });
  } on MissingPluginException {
    // 다리가 없는 판(웹·시험)은 사람이 홈으로 간다.
  } on PlatformException {
    // 알림 권한이 없어도 비켜서기는 다리가 먼저 한다 — 여기 올 일은 다리 자체가 실패한 때뿐이다.
  }
}

/// 비켜서기 전에 [취소]를 누를 틈.
const releaseStepAsideDelay = Duration(seconds: 3);

/// 「눌러서 열기」 알림을 띄울 때 — 29MB 받기와 설치가 보통 이 안에 끝난다. 새 판이 먼저 켜지면 다리가 알림을 거둔다.
const releaseOpenNoticeAfter = Duration(seconds: 45);

const _releaseChannel = MethodChannel('kasaterm/release');

/// 설치 확인 창이 떠 앱이 잠깐 비활성이 됐다가 돌아오는 순간. [wait] 안에 안 돌아오면 거짓.
(Future<bool>, void Function()) _returnAfterLeaving(Duration wait) {
  final done = Completer<bool>();
  var left = false;
  void finish(bool v) {
    if (!done.isCompleted) done.complete(v);
  }

  final listener = AppLifecycleListener(
    onInactive: () => left = true,
    onResume: () {
      if (left) finish(true);
    },
  );
  final timer = Timer(wait, () => finish(false));
  done.future.whenComplete(() {
    timer.cancel();
    listener.dispose();
  });
  return (done.future, () => finish(false));
}
