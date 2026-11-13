import 'dart:convert';

import 'package:flutter_secure_storage/flutter_secure_storage.dart';

import 'conversation.dart';

/// Info 시트 「내 프롬프트」의 한 줄 — 사람이 직접 친 말이나 묻는 말에 고른 답. 데스크톱 `info_prompts.rs` 의
/// `Prompt` 와 같은 규칙이고, [row] 는 그 말이 선 대화 보기의 칸(말풍선·답한 질문 칸)이라 누르면 거기로 옮긴다.
class MyPrompt {
  const MyPrompt({required this.text, required this.row, this.at, this.asked});
  final String text;
  final Object row;
  final DateTime? at;

  /// 묻는 말에 고른 답이면 그 질문.
  final String? asked;

  /// 데스크톱 체크 열쇠와 같은 꼴(`친 초|머리글|질문 머리글`) — 언젠가 체크를 맞출 때 그대로 합친다.
  String get checkKey =>
      '${at == null ? 0 : at!.millisecondsSinceEpoch ~/ 1000}|${headKey(text)}|${asked == null ? '' : headKey(asked!)}';
}

/// 맞춰 볼 머리글 — 첫 줄, 프롬프트 마커와 공백을 걷은 앞 40자(데스크톱 `head_key`).
String headKey(String s) {
  final first = s.trim().split('\n').first.replaceFirst(RegExp(r'^[❯›>]+'), '');
  final kept = first.replaceAll(RegExp(r'\s'), '');
  return String.fromCharCodes(kept.runes.take(40));
}

/// 대화 보기의 칸에서 고른 내 프롬프트, 새 것이 위. 쪽지·완료 보고(다른 쪽이 넣은 말)·예약·보내는 중·슬래시
/// 명령(따로 [ChatCommand])은 질문이 아니다. codex 가 묻고 고른 답은 사람 턴 안의 `<send_user_message_question_reply>`.
List<MyPrompt> myPrompts(List<Object> rows) {
  final out = <MyPrompt>[];
  void answers(Iterable<(String, String)> pairs, Object row, DateTime? at) {
    for (final (q, a) in pairs) {
      if (a.trim().isEmpty || a == '—') continue;
      out.add(MyPrompt(text: a, row: row, at: at, asked: q));
    }
  }

  for (final row in rows) {
    if (row is ChatBubble && row.mine && !row.queued && !row.sending) {
      final text = row.text.trim();
      final codex = codexAnswers(text);
      if (codex != null) {
        answers(codex, row, row.at);
        continue;
      }
      final photos = row.images.length + row.omittedImages;
      if (text.isNotEmpty) {
        out.add(MyPrompt(text: text, row: row, at: row.at));
      } else if (photos > 0) {
        out.add(MyPrompt(text: '그림 $photos장', row: row, at: row.at));
      }
    } else if (row is ChatAnswered && row.pairs != null) {
      answers(row.pairs!, row, row.at);
    }
  }
  return out.reversed.toList();
}

/// codex 가 묻고 사람이 고른 답 — `[{"question":…,"answer":…}]` 를 감싼 사람 턴. 못 읽어도 사람 말로 세지 않는다.
List<(String, String)>? codexAnswers(String text) {
  const open = '<send_user_message_question_reply>';
  if (!text.startsWith(open)) return null;
  var body = text.substring(open.length).trim();
  const close = '</send_user_message_question_reply>';
  if (body.endsWith(close)) body = body.substring(0, body.length - close.length);
  try {
    final list = jsonDecode(body.trim());
    return [
      if (list is List)
        for (final v in list)
          if (v is Map && v['question'] is String && v['answer'] is String) (v['question'] as String, v['answer'] as String),
    ];
  } on FormatException {
    return const [];
  }
}

/// 친 날짜·시각 `10.12 22:46` — 해가 다르면 앞에 해(데스크톱 `clock`).
String promptClock(DateTime at, {DateTime? now}) {
  final t = at.toLocal();
  String two(int n) => n.toString().padLeft(2, '0');
  final day = '${two(t.month)}.${two(t.day)} ${two(t.hour)}:${two(t.minute)}';
  return t.year == (now ?? DateTime.now()).toLocal().year ? day : '${t.year}.$day';
}

/// 끝낸 것으로 체크한 프롬프트. 데스크톱은 설정 파일 옆 `prompt-checks.json` 에 두는데 그 파일을 내주는 길이
/// 없고 GUI 스레드가 쥔 사본을 쓰므로, 폰은 계정 범위별로 따로 둔다 — 열쇠 꼴과 상한은 같다.
class PromptChecks {
  PromptChecks(this.scope);
  final String scope;

  static const _max = 2000;
  static const _storage = FlutterSecureStorage();
  static final _loaded = <String, List<String>>{};

  String get _key => 'prompt-checks.$scope';

  Future<Set<String>> load() async {
    final cached = _loaded[scope];
    if (cached != null) return cached.toSet();
    List<String> list;
    try {
      final raw = await _storage.read(key: _key);
      final v = raw == null ? null : jsonDecode(raw);
      list = [for (final k in (v is List ? v : const [])) if (k is String) k];
    } catch (_) {
      list = [];
    }
    _loaded[scope] = list;
    return list.toSet();
  }

  /// 켜고 끈 뒤의 체크 모음. 넘치면 오래된 것부터 잊는다.
  Future<Set<String>> toggle(String key) async {
    await load();
    final list = _loaded[scope]!;
    if (!list.remove(key)) {
      list.add(key);
      if (list.length > _max) list.removeRange(0, list.length - _max);
    }
    await _storage.write(key: _key, value: jsonEncode(list));
    return list.toSet();
  }
}

/// Info 시트가 대화 보기를 움직이는 손잡이 — 화면이 쥐고 대화 보기가 붙는다.
class ConversationJump {
  Future<bool> Function(Object row)? _toTop;
  Object? Function(Set<Object> rows)? _topOf;

  void attach({required Future<bool> Function(Object row) toTop, required Object? Function(Set<Object> rows) topOf}) {
    _toTop = toTop;
    _topOf = topOf;
  }

  void detach() {
    _toTop = null;
    _topOf = null;
  }

  /// 그 칸을 화면 맨 위에 붙여 세운다. 대화 보기가 없거나 그 칸이 목록에 없으면 false.
  Future<bool> toTop(Object row) => _toTop?.call(row) ?? Future.value(false);

  /// [rows] 가운데 지금 화면 맨 위 줄을 품은 것 — 맨 위 줄에서 거슬러 올라가 처음 만나는 것.
  Object? topOf(Set<Object> rows) => _topOf?.call(rows);
}
