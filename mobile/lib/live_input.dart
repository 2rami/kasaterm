import 'dart:convert';

import 'package:flutter/services.dart';

/// 「바로 치기」 — 입력칸의 글이 바뀔 때마다 그 차이만 pane 에 보낸다. 확정된 글자는
/// 곧바로 화면의 입력상자에 붙고, 조합 중인 한글(ㄱ, 가, 갃…)은 보내지 않고
/// `composing` 으로 남겨 커서 자리에 겹쳐 그린다 — 자모가 따로따로 날아가 깨지지 않게.
class LiveInput {
  String _committed = '';

  /// 아직 조합 중인 글(보내지 않은 것).
  String composing = '';

  /// 새 입력값을 받아 pane 에 보낼 바이트를 돌려준다. 없으면 빈 목록.
  List<int> update(TextEditingValue v) {
    final (committed, composing) = splitComposing(v);
    this.composing = composing;
    final bytes = liveDiff(_committed, committed);
    _committed = committed;
    return bytes;
  }

  /// 조합 중인 글까지 확정으로 보고 보낸다 — 엔터 직전.
  List<int> flush(TextEditingValue v) {
    final bytes = liveDiff(_committed, v.text);
    _committed = v.text;
    composing = '';
    return bytes;
  }

  /// 입력칸을 비운 뒤 — 지운 것을 지우기로 보내지 않게 기준을 같이 비운다.
  void reset() {
    _committed = '';
    composing = '';
  }
}

/// 확정된 글과 조합 중인 글로 가른다.
(String, String) splitComposing(TextEditingValue v) {
  final c = v.composing;
  final t = v.text;
  if (!c.isValid || c.isCollapsed || c.end > t.length) return (t, '');
  return (
    t.substring(0, c.start) + t.substring(c.end),
    t.substring(c.start, c.end),
  );
}

/// `prev` 를 `next` 로 만드는 키 바이트 — 앞부분이 같은 데까지 두고, 남은 것은 지우기
/// (DEL)로 물린 뒤 새 글자를 UTF-8 로 잇는다. 글자 단위(룬)라 한글 한 자가 지우기 하나.
List<int> liveDiff(String prev, String next) {
  final a = prev.runes.toList();
  final b = next.runes.toList();
  var c = 0;
  while (c < a.length && c < b.length && a[c] == b[c]) {
    c++;
  }
  return [
    for (var i = c; i < a.length; i++) 0x7f,
    ...utf8.encode(String.fromCharCodes(b.sublist(c))),
  ];
}
