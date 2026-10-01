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
