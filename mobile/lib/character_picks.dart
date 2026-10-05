/// 계정 공통 설정 `character_picks` — 새 창에 누구를 배정할지. 데스크톱 `session.rs` 의
/// `updated_character_picks`·`apply_theme_pick_all`, `native_settings.rs` 의 `character_choice_selected_from`,
/// `character.rs` 의 `picked_names_from` 과 같은 규칙이다. 한쪽만 고치면 폰과 데스크톱이 같은 명단을 다르게 고친다.
library;

/// 번들 테마의 키. 테마 카드 id 는 빈 문자열이지만 명단·`theme-roster` 는 이 이름을 쓴다 — 빈 값은 「안 줬다」와 구분이 안 된다.
const baseThemeKey = '__base';

String themeKeyOf(String id) => id.isEmpty ? baseThemeKey : id;

/// 테마별 이름, 적힌 순서 그대로. 테마 순서와 이름 순서가 곧 새 창 배정 순서다.
typedef Picks = List<(String, List<String>)>;

class PickError implements Exception {
  const PickError(this.message);
  final String message;
}

class RosterMember {
  const RosterMember(this.name, this.slug);
  final String name;

  /// 얼굴 파일 키. 같은 그림 키를 두 테마에서 함께 켜지 않는다(바꿔치기 판정).
  final String? slug;
}

/// 테마 하나의 명단(`theme-roster` 응답 — leader·leaders·members). 이름은 처음 나온 것 하나만.
class Roster {
  const Roster(this.members);
  final List<RosterMember> members;

  factory Roster.fromJson(Object? json) {
    final entries = <Map>[
      if (json is Map && json['leader'] is Map) json['leader'] as Map,
      for (final key in const ['leaders', 'members'])
        if (json is Map && json[key] is List)
          for (final e in json[key] as List)
            if (e is Map) e,
    ];
    final members = <RosterMember>[];
    for (final e in entries) {
      final name = e['name'];
      if (name is! String || name.isEmpty || members.any((m) => m.name == name)) continue;
      final slug = e['slug'];
      members.add(RosterMember(name, slug is String && slug.isNotEmpty ? slug : null));
    }
    return Roster(members);
  }

  List<String> get names => [for (final m in members) m.name];
  bool has(String name) => members.any((m) => m.name == name);
  String? slugOf(String name) => members.where((m) => m.name == name).firstOrNull?.slug;
}

/// 데스크톱 `settings/characters` 의 테마 카드 한 장.
class ThemeCard {
  const ThemeCard({required this.id, required this.label, required this.count, required this.faces});
  final String id;
  final String label;
  final int count;
  final List<String> faces;

  String get key => themeKeyOf(id);

  static List<ThemeCard> listFrom(Object? json) => [
    if (json is Map && json['themes'] is List)
      for (final t in json['themes'] as List)
        if (t is Map && t['id'] is String)
          ThemeCard(
            id: t['id'] as String,
            label: t['label'] is String && (t['label'] as String).isNotEmpty ? t['label'] as String : t['id'] as String,
            count: t['count'] is int ? t['count'] as int : 0,
            faces: [
              for (final f in t['faces'] is List ? t['faces'] as List : const [])
                if (f is String) f,
            ],
          ),
  ];
}

Picks parsePicks(Object? raw) => [
  if (raw is Map)
    for (final e in raw.entries)
      if (e.key is String && e.value is List)
        ('${e.key}', [
          for (final n in e.value as List)
            if (n is String) n,
        ]),
].where((e) => e.$2.isNotEmpty).toList();

Map<String, List<String>> encodePicks(Picks picks) => {
  for (final (theme, names) in picks)
    if (names.isNotEmpty) theme: names,
};

Picks _copy(Picks picks) => [for (final (t, n) in picks) (t, [...n])];

/// 다른 테마에서 같은 이름이나 같은 그림 키를 가진 학생을 뺀다 — 새로 고른 쪽이 이긴다.
void _replaceConflicts(Picks picks, String theme, List<String> names, Roster? Function(String) load) {
  final roster = load(theme);
  final slugs = roster == null ? const <String>{} : {for (final n in names) ?roster.slugOf(n)};
  for (final (other, selected) in picks) {
    if (other == theme) continue;
    final otherRoster = load(other);
    selected.removeWhere((n) => names.contains(n) || slugs.contains(otherRoster?.slugOf(n)));
  }
}

/// 학생 하나를 켜거나 끈다.
Picks pickOne(Picks current, String theme, String name, bool on, Roster? Function(String) load) {
  if (theme.isEmpty || name.trim().isEmpty) throw const PickError('어느 테마의 누구인지 알 수 없어요');
  name = name.trim();
  // 끄기는 이름을 안 본다 — 이미 들어앉은 유령이나 이름이 바뀐 항목을 지울 길이 남아야 한다.
  if (on) {
    final roster = load(theme);
    if (roster == null || roster.members.isEmpty) throw const PickError('그 테마의 명단을 못 읽었어요');
    if (!roster.has(name)) throw PickError('$name 은(는) 그 테마에 없어요');
  }
  final picks = _copy(current);
  final unrestricted = picks.every((e) => e.$2.isEmpty);
  if (on) _replaceConflicts(picks, theme, [name], load);
  var index = picks.indexWhere((e) => e.$1 == theme);
  if (index < 0) {
    picks.add((theme, <String>[]));
    index = picks.length - 1;
  }
  var names = picks[index].$2;
  // 아무도 안 고른 상태(그 테마 전원)에서 한 명을 뺄 때만 나머지를 명시적으로 적는다.
  if (!on && unrestricted) {
    names = load(theme)?.names ?? <String>[];
    picks[index] = (theme, names);
  }
  names.removeWhere((n) => n == name);
  if (on) names.add(name);
  picks.removeWhere((e) => e.$2.isEmpty);
  if (picks.isEmpty) throw const PickError('최소 한 명은 선택해 주세요');
  return picks;
}

/// 테마 하나를 통째로 켜거나 끈다. 켜면 그 테마가 차례 맨 뒤로 간다.
Picks pickAll(Picks current, String theme, bool on, Roster? Function(String) load) {
  if (theme.isEmpty) throw const PickError('어느 테마인지 알 수 없어요');
  final names = load(theme)?.names ?? const <String>[];
  if (names.isEmpty) throw const PickError('그 테마의 명단을 못 읽었어요');
  final picks = _copy(current)..removeWhere((e) => e.$1 == theme);
  if (on) {
    _replaceConflicts(picks, theme, names, load);
    picks.add((theme, [...names]));
  }
  picks.removeWhere((e) => e.$2.isEmpty);
  if (picks.isEmpty) throw const PickError('최소 한 명은 선택해 주세요');
  return picks;
}

/// 이 기기에 실재하는 학생만 남긴 명단. 비면 고른 학생이 없는 것으로 본다(활성 테마 전원).
Picks livePicks(Picks picks, Map<String, Roster> rosters) => [
  for (final e in picks)
    if (rosters[e.$1] case final r? when e.$2.any(r.has)) e,
];

/// 그 테마 칸에서 이 학생이 켜져 보이는가. 두 테마에 같은 이름이 있으면 차례가 앞선 테마 쪽만 켜진다.
bool isPicked(Picks picks, Map<String, Roster> rosters, String activeTheme, String key, String name) {
  final live = livePicks(picks, rosters);
  if (live.isEmpty) return key == themeKeyOf(activeTheme);
  for (final (theme, names) in live) {
    if (names.contains(name) && rosters[theme]!.has(name)) return theme == key;
  }
  return false;
}

/// 새 창이 받을 차례 — 테마를 가로질러 실재하는 이름만, 겹치는 이름은 한 번.
List<(String, String)> assignmentOrder(Picks picks, Map<String, Roster> rosters) {
  final seen = <String>{};
  return [
    for (final (theme, names) in picks)
      if (rosters[theme] case final r?)
        for (final n in names)
          if (r.has(n) && seen.add(n)) (theme, n),
  ];
}
