import 'dart:convert';

import 'package:flutter/material.dart';

import 'contrast.dart';

/// 기기의 얼굴(색·아이콘) — 데스크톱 설정 「기기 색」「기기 아이콘」과 같은 값을 쓴다. 폰이 이름으로
/// 따로 고르면 맥미니가 데스크톱에선 파랑, 폰에선 보라가 된다(2026-10-01 지적).
///
/// 색은 기준 기기가 지금 칠하는 색 그대로(`settings/values` 의 `appearance.device_colors` —
/// 사용자가 고른 색 → 명부 배정색), 아이콘은 계정 설정 `device_icons`. 표에 없는 이름은 데스크톱이
/// 모르는 이름에 쓰는 규칙(`pane_identity::hashed_device_color`·`device_icons::automatic`)을 그대로 따른다.
/// 폰의 밝게·어둡게와 상관없이 같은 색이다 — 데스크톱도 기기색은 테마를 안 탄다.
class MachineLooks {
  const MachineLooks({
    this.colors = const {},
    this.icons = const {},
    this.presets = defaultPresets,
    this.local,
    this.names = const {},
    this.rootId,
    this.ids = const {},
  });

  /// 데스크톱 `DEVICE_COLOR_PRESETS` — 옛 판이 `device_presets` 를 안 줄 때만 쓴다.
  static const defaultPresets = <Color>[
    Color(0xff4c86e4),
    Color(0xff9e68e6),
    Color(0xff1aac9c),
    Color(0xffe65a96),
    Color(0xffd09a22),
  ];

  /// 정규화한 이름(`normalizeDevice`) → 색.
  final Map<String, Color> colors;

  /// `trim().toLowerCase()` 이름 → 아이콘 이름(laptop·monitor·server·smartphone).
  final Map<String, String> icons;
  final List<Color> presets;

  /// 기준 기기 자신의 색 — 기준 기기 칸은 이름이 「이 기계」로 떠도 이 색을 쓴다.
  final Color? local;

  /// 계정이 기기에 붙인 이름(`device_names`) — 기기 id → 이름. 보여 줄 때만 쓴다.
  final Map<String, String> names;

  /// 기준 기기의 id(`/version` 의 `machine_id`).
  final String? rootId;

  /// 정규화한 기기 이름 → id. `~id` 길로 닿는 기계를 허브가 알려 준다 — 거울 칩처럼 이름만 든 자리가 쓴다.
  final Map<String, String> ids;

  /// `appearance` 는 `settings/values` 의 `appearance`, `deviceIcons`·`deviceNames` 는 계정 설정의
  /// `device_icons`·`device_names`. [deviceNames] 가 null 이면 `appearance.device_names` 를 쓴다.
  factory MachineLooks.parse({
    Object? appearance,
    Object? deviceIcons,
    Object? deviceNames,
    String? rootId,
  }) {
    final a = appearance is Map ? appearance : const {};
    final colors = <String, Color>{};
    Color? local;
    for (final row in (a['device_colors'] as List?) ?? const []) {
      if (row is! Map) continue;
      final label = row['label'];
      final color = parseHexColor(row['hex'] as String?);
      if (label is! String || color == null) continue;
      colors[normalizeDevice(label)] = color;
      if (row['local'] == true) local = color;
    }
    final presets = [
      for (final p in (a['device_presets'] as List?) ?? const [])
        if (p is Map) ?parseHexColor(p['hex'] as String?),
    ];
    final icons = <String, String>{};
    if (deviceIcons is Map) {
      for (final e in deviceIcons.entries) {
        final v = e.value;
        if (v is String && _iconOf.containsKey(v)) {
          icons['${e.key}'.trim().toLowerCase()] = v;
        }
      }
    }
    return MachineLooks(
      colors: colors,
      icons: icons,
      presets: presets.isEmpty ? defaultPresets : presets,
      local: local,
      // 계정 값을 받았으면 그것이 정본이다. 못 받았으면(계정 없이 붙은 폰·옛 관문) 기준 기기가 든 사본.
      names: parseDeviceNames(deviceNames ?? a['device_names']),
      rootId: rootId == null || rootId.isEmpty ? null : rootId,
    );
  }

  MachineLooks copyWith({Map<String, String>? names, Map<String, String>? ids}) => MachineLooks(
    colors: colors,
    icons: icons,
    presets: presets,
    local: local,
    names: names ?? this.names,
    rootId: rootId,
    ids: ids ?? this.ids,
  );

  /// 기기 id — [local] 이면 기준 기기, `~id` 길이면 그 id, 아니면 허브가 알려 준 이름 표.
  String? idOf(String label, {String? route, bool local = false}) {
    if (local) return rootId;
    if (route != null && route.startsWith('~') && route.length > 1) return route.substring(1);
    return ids[normalizeDevice(label)];
  }

  /// 사람이 부르는 이름 — 계정 이름이 있으면 그것, 없으면 [label]. 색·아이콘·길은 [label] 그대로 쓴다.
  String name(String label, {String? route, bool local = false}) =>
      names[idOf(label, route: route, local: local)] ?? label;

  /// [local] 이면 기준 기기 — 폰이 그 기기를 부르는 이름(「이 기계」)이 명부 이름과 다를 수 있다.
  Color color(String label, {bool local = false}) {
    final mine = this.local;
    if (local && mine != null) return mine;
    final key = normalizeDevice(label);
    return colors[key] ?? _hashed(key);
  }

  IconData icon(String label) =>
      _iconOf[icons[label.trim().toLowerCase()] ?? _automatic(label)]!;

  Color _hashed(String key) {
    var h = 2166136261;
    for (final b in utf8.encode(key)) {
      h = ((h ^ b) * 16777619) & 0xffffffff;
    }
    return presets[h % presets.length];
  }

  static const _iconOf = <String, IconData>{
    'laptop': Icons.laptop_outlined,
    'monitor': Icons.desktop_windows_outlined,
    'server': Icons.dns_outlined,
    'smartphone': Icons.smartphone_outlined,
  };

  static String _automatic(String label) {
    final l = label.trim().toLowerCase();
    bool any(List<String> parts) => parts.any(l.contains);
    if (any(const ['macbook', '맥북', 'laptop'])) return 'laptop';
    if (any(const ['mini', '미니', 'server', '서버'])) return 'server';
    if (any(const ['phone', 'mobile', '폰', '모바일'])) return 'smartphone';
    return 'monitor';
  }
}

/// 계정 설정 `device_names` — 빈 이름·글이 아닌 값은 버린다.
Map<String, String> parseDeviceNames(Object? raw) => {
  if (raw is Map)
    for (final e in raw.entries)
      if (e.value is String && (e.value as String).trim().isNotEmpty) '${e.key}': (e.value as String).trim(),
};

/// [names] 에서 [id] 의 이름을 바꾼 새 표. 빈 [name] 은 지워 기기 제 이름으로 돌린다.
Map<String, String> renameDevice(Map<String, String> names, String id, String name) {
  final next = {...names};
  final trimmed = name.trim();
  if (trimmed.isEmpty) {
    next.remove(id);
  } else {
    next[id] = trimmed;
  }
  return next;
}

/// 데스크톱 `normalize_device` — 맥 컴퓨터 이름의 NBSP 같은 공백 종류 차이를 지운다.
String normalizeDevice(String label) => label
    .trim()
    .split(RegExp(r'\s+'))
    .where((s) => s.isNotEmpty)
    .join(' ')
    .toLowerCase();

Color? parseHexColor(String? hex) {
  if (hex == null) return null;
  final h = hex.replaceFirst('#', '');
  if (h.length != 6) return null;
  final v = int.tryParse(h, radix: 16);
  return v == null ? null : Color(0xff000000 | v);
}

/// 허브가 기준 기기에서 받아 채운다. 터미널 화면 머리의 거울 칩도 같은 표를 본다.
final machineLooks = ValueNotifier<MachineLooks>(const MachineLooks());

Color machineColor(String label, {bool local = false}) =>
    machineLooks.value.color(label, local: local);

IconData machineIcon(String label) => machineLooks.value.icon(label);

String machineName(String label, {String? route, bool local = false}) =>
    machineLooks.value.name(label, route: route, local: local);

/// 기기색으로 쓴 글자 — 그대로 칠하면 밝은 바탕 위 노랑이 안 읽힌다. 데스크톱 머리의
/// `MachineIdentity::foreground`(대비 4.5)와 같은 보정이다. 아이콘·띠는 기기색 그대로 둔다.
Color machineInk(Color machine, Color background) =>
    enforceContrast(machine, background, 4.5);
