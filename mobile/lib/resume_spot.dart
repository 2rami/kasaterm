import 'dart:convert';

import 'package:flutter_secure_storage/flutter_secure_storage.dart';

/// 앱이 뒤로 갈 때 보던 학생 화면. iOS 는 뒤로 간 앱을 메모리가 모자라면 말없이 거둬서, 다른 앱에 다녀오면 허브부터
/// 다시 떴고 쓰던 글도 사라졌다(2026-10-07 「다른 앱 갔다 와도 안 끊기게」). 다시 켜지면 이 자리로 돌아간다.
/// 쓰던 글은 Keychain 에 둔다 — 비밀번호를 적던 중일 수 있다.
class ResumeSpot {
  const ResumeSpot({
    required this.scope,
    required this.pane,
    required this.at,
    this.machine,
    this.view,
    this.chatDraft = '',
    this.termDraft = '',
  });

  /// 어느 연결의 자리인가 — 다른 계정·주소로 다시 켜졌으면 쓰지 않는다([Server.resumeScope]).
  final String scope;
  final String pane;
  final String? machine;

  /// `PaneView` 이름. 모르는 값이면 화면 기본 쪽으로 연다.
  final String? view;

  /// 대화 보기 입력줄의 글과, 바로 치기를 끄고 적어 두던 터미널 입력칸의 글.
  final String chatDraft;
  final String termDraft;
  final DateTime at;

  /// 이보다 오래 전에 떠났으면 허브부터 — 한참 뒤에 켠 앱이 옛 학생 화면으로 뛰어들면 낯설다.
  static const keep = Duration(hours: 1);

  bool freshAt(DateTime now) => now.difference(at) < keep;

  Map<String, Object?> toJson() => {
    'scope': scope,
    'pane': pane,
    if (machine != null) 'machine': machine,
    if (view != null) 'view': view,
    if (chatDraft.isNotEmpty) 'chat': chatDraft,
    if (termDraft.isNotEmpty) 'term': termDraft,
    'at': at.millisecondsSinceEpoch,
  };

  static ResumeSpot? fromJson(Object? raw) {
    if (raw is! Map) return null;
    final scope = raw['scope'];
    final pane = raw['pane'];
    final at = raw['at'];
    if (scope is! String || pane is! String || pane.isEmpty || at is! int) return null;
    String? text(Object? v) => v is String && v.isNotEmpty ? v : null;
    return ResumeSpot(
      scope: scope,
      pane: pane,
      machine: text(raw['machine']),
      view: text(raw['view']),
      chatDraft: text(raw['chat']) ?? '',
      termDraft: text(raw['term']) ?? '',
      at: DateTime.fromMillisecondsSinceEpoch(at),
    );
  }
}

class ResumeSpotStore {
  const ResumeSpotStore();

  static const _key = 'resume.spot';
  static const _storage = FlutterSecureStorage();

  Future<void> save(ResumeSpot spot) async {
    try {
      await _storage.write(key: _key, value: jsonEncode(spot.toJson()));
    } catch (_) {
      // 자리 기억은 덤이다 — 못 적으면 다시 켤 때 허브부터 뜰 뿐.
    }
  }

  /// 한 번 꺼내면 지운다 — 다시 켤 때마다 같은 화면으로 끌려가지 않게.
  Future<ResumeSpot?> take() async {
    try {
      final raw = await _storage.read(key: _key);
      if (raw == null) return null;
      await _storage.delete(key: _key);
      return ResumeSpot.fromJson(jsonDecode(raw));
    } catch (_) {
      return null;
    }
  }

  Future<void> clear() async {
    try {
      await _storage.delete(key: _key);
    } catch (_) {}
  }
}
