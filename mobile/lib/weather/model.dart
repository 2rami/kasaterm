/// 날씨 설정과 「이 카드에 비가 얼마나 오나」 판정 — 정본은 docs/weather.md, 데스크톱
/// `app/kasaterm/src/weather/model.rs` 와 같은 모델이다. 폰의 「창」은 카드, 「초점」은 방금
/// 만지거나 스크롤한 카드. 값 이름은 계정 동기화(`weather` 키)의 JSON 그대로다.
library;

enum WeatherCondition {
  rain('빗줄기와 물보라', 0, 0),
  drizzle('이슬비', 0, 1),
  heavyRain('폭우', 0, 2),
  sunShowers('여우비', 0, 3),
  freezingDrizzle('어는 이슬비', 0, 4),
  freezingRain('어는 비', 0, 5),
  nightDrizzle('밤 이슬비', 0, 6),
  clear('맑음', 1, 0),
  mostlyClear('대체로 맑음', 1, 1),
  partlyCloudy('구름 조금', 1, 2),
  mostlyCloudy('구름 많음', 1, 3),
  cloudy('흐림', 1, 4),
  clearNight('맑은 밤', 1, 5),
  mostlyClearNight('대체로 맑은 밤', 1, 6),
  partlyCloudyNight('구름 낀 밤', 1, 7),
  sunrise('일출', 1, 8),
  sunset('일몰', 1, 9),
  foggy('안개', 2, 0),
  haze('연무', 2, 1),
  smoky('연기', 2, 2),
  blowingDust('먼지바람', 2, 3),
  breezy('산들바람', 3, 0),
  windy('강풍', 3, 1),
  flurries('날리는 눈', 4, 0),
  snow('눈', 4, 1),
  heavySnow('폭설', 4, 2),
  sunFlurries('햇눈', 4, 3),
  blowingSnow('날림눈', 4, 4),
  blizzard('눈보라', 4, 5),
  sleet('얼음싸라기', 4, 6),
  wintryMix('진눈깨비', 4, 7),
  hail('우박', 4, 8),
  isolatedThunderstorms('국지성 뇌우', 5, 0),
  scatteredThunderstorms('산발적 뇌우', 5, 1),
  thunderstorms('뇌우', 5, 2),
  strongStorms('강한 뇌우', 5, 3),
  hot('폭염', 6, 0),
  frigid('혹한', 6, 1),
  tropicalStorm('열대 폭풍', 7, 0),
  hurricane('허리케인', 7, 1),
  tornado('토네이도', 7, 2);

  const WeatherCondition(this.label, this.material, this.variant);
  final String label;
  final int material, variant;
}

enum RainStyle { liquid, showers }

enum RainAmount {
  none('없음'),
  drizzle('이슬비'),
  rain('비'),
  downpour('폭우');

  const RainAmount(this.label);
  final String label;

  static RainAmount parse(Object? v, RainAmount fallback) =>
      values.firstWhere((a) => a.name == v, orElse: () => fallback);
}

enum WeatherTarget {
  allWindows('all_windows', '전체 카드'),
  focusedOnly('focused_only', '초점 카드만'),
  unfocusedOnly('unfocused_only', '초점 아닌 카드만'),
  pickedOnly('picked_only', '고른 카드만'),
  backgroundOnly('background_only', '카드 밖 배경만');

  const WeatherTarget(this.id, this.label);
  final String id;
  final String label;

  static WeatherTarget parse(Object? v) =>
      values.firstWhere((t) => t.id == v, orElse: () => WeatherTarget.focusedOnly);
}

enum Effect {
  streaks('빗줄기'),
  drops('창 물방울'),
  mist('김서림'),
  ripples('파문'),
  buttons('단추 물방울');

  const Effect(this.label);
  final String label;
}

/// 학생 상태 날씨의 세 갈래.
enum Mood {
  busy('하는 중'),
  yourTurn('내 차례'),
  resting('쉬는 중');

  const Mood(this.label);
  final String label;
}

class WeatherSettings {
  const WeatherSettings({
    this.enabled = false,
    this.style = RainStyle.liquid,
    this.condition = WeatherCondition.rain,
    this.amount = RainAmount.drizzle,
    this.windDir = 0,
    this.windStrength = 0,
    this.target = WeatherTarget.focusedOnly,
    this.effects = const {...Effect.values},
    this.rewetSecs = 180,
    this.byStatus = false,
    this.busy = RainAmount.drizzle,
    this.yourTurn = RainAmount.downpour,
    this.resting = RainAmount.none,
    this.ignoreOs = false,
  });

  final bool enabled;
  final RainStyle style;
  final WeatherCondition condition;
  final RainAmount amount;

  /// −1(왼쪽) ~ 1(오른쪽).
  final double windDir;

  /// 0 ~ 1.
  final double windStrength;
  final WeatherTarget target;
  final Set<Effect> effects;

  /// 비를 맞기 시작한 카드가 다 젖기까지(알갱이가 맺히고 김이 낀다). 30 ~ 600초.
  final int rewetSecs;
  final bool byStatus;
  final RainAmount busy, yourTurn, resting;

  /// 켜면 OS 「동작 줄이기」「투명도 줄이기」를 따르지 않는다.
  final bool ignoreOs;

  static const rewetMin = 30, rewetMax = 600;

  bool has(Effect e) => effects.contains(e);

  /// 바람 한 값(−1~1): 방향 × 세기.
  double get wind => windDir.clamp(-1.0, 1.0) * windStrength.clamp(0.0, 1.0);

  RainAmount moodAmount(Mood m) => switch (m) {
    Mood.busy => busy,
    Mood.yourTurn => yourTurn,
    Mood.resting => resting,
  };

  WeatherSettings copyWith({
    bool? enabled,
    RainStyle? style,
    WeatherCondition? condition,
    RainAmount? amount,
    double? windDir,
    double? windStrength,
    WeatherTarget? target,
    Set<Effect>? effects,
    int? rewetSecs,
    bool? byStatus,
    RainAmount? busy,
    RainAmount? yourTurn,
    RainAmount? resting,
    bool? ignoreOs,
  }) => WeatherSettings(
    enabled: enabled ?? this.enabled,
    style: style ?? this.style,
    condition: condition ?? this.condition,
    amount: amount ?? this.amount,
    windDir: windDir ?? this.windDir,
    windStrength: windStrength ?? this.windStrength,
    target: target ?? this.target,
    effects: effects ?? this.effects,
    rewetSecs: rewetSecs ?? this.rewetSecs,
    byStatus: byStatus ?? this.byStatus,
    busy: busy ?? this.busy,
    yourTurn: yourTurn ?? this.yourTurn,
    resting: resting ?? this.resting,
    ignoreOs: ignoreOs ?? this.ignoreOs,
  );

  WeatherSettings withMood(Mood m, RainAmount a) => switch (m) {
    Mood.busy => copyWith(busy: a),
    Mood.yourTurn => copyWith(yourTurn: a),
    Mood.resting => copyWith(resting: a),
  };

  WeatherSettings withEffect(Effect e, bool on) =>
      copyWith(effects: on ? {...effects, e} : ({...effects}..remove(e)));

  Map<String, Object?> toJson() => {
    'enabled': enabled,
    if (style != RainStyle.liquid) 'style': style.name,
    if (condition != WeatherCondition.rain) 'condition': condition.name,
    'amount': amount.name,
    'wind_dir': windDir,
    'wind_strength': windStrength,
    'target': target.id,
    'effects': {for (final e in Effect.values) e.name: effects.contains(e)},
    'rewet_secs': rewetSecs,
    'by_status': byStatus,
    'busy': busy.name,
    'your_turn': yourTurn.name,
    'resting': resting.name,
    'ignore_os': ignoreOs,
  };

  /// 저장·동기화로 들어온 값. 모르는 칸·범위 밖 값은 기본으로 — 데스크톱 `sanitized` 와 같다.
  factory WeatherSettings.fromJson(Object? raw) {
    const d = WeatherSettings();
    if (raw is! Map) return d;
    double ranged(Object? v, double lo, double hi, double fallback) =>
        v is num && v.isFinite ? v.toDouble().clamp(lo, hi) : fallback;
    final fx = raw['effects'];
    return WeatherSettings(
      enabled: raw['enabled'] == true,
      condition: WeatherCondition.values.firstWhere((c) => c.name == raw['condition'], orElse: () => WeatherCondition.rain),
      style: raw['style'] == 'showers' ? RainStyle.showers : RainStyle.liquid,
      amount: RainAmount.parse(raw['amount'], d.amount),
      windDir: ranged(raw['wind_dir'], -1, 1, 0),
      windStrength: ranged(raw['wind_strength'], 0, 1, 0),
      target: WeatherTarget.parse(raw['target']),
      effects: fx is Map ? {for (final e in Effect.values) if (fx[e.name] != false) e} : d.effects,
      rewetSecs: ranged(raw['rewet_secs'], rewetMin.toDouble(), rewetMax.toDouble(), 180).round(),
      byStatus: raw['by_status'] == true,
      busy: RainAmount.parse(raw['busy'], d.busy),
      yourTurn: RainAmount.parse(raw['your_turn'], d.yourTurn),
      resting: RainAmount.parse(raw['resting'], d.resting),
      ignoreOs: raw['ignore_os'] == true,
    );
  }

  @override
  bool operator ==(Object other) =>
      other is WeatherSettings && _eq(toJson(), other.toJson());

  @override
  int get hashCode => toJson().toString().hashCode;
}

bool _eq(Map<String, Object?> a, Map<String, Object?> b) => a.toString() == b.toString();

String rewetLabel(int secs) {
  if (secs < 60) return '$secs초';
  if (secs % 60 == 0) return '${secs ~/ 60}분';
  return '${secs ~/ 60}분 ${secs % 60}초';
}

/// 카드 길게 누르기 「이 카드 날씨」. 이 기기에만 남는다.
sealed class CardWeather {
  const CardWeather();

  static const follow = FollowWeather();
  static const picked = PickedWeather();
  static const clear = ClearWeather();

  static const menu = <CardWeather>[
    follow,
    picked,
    clear,
    FixedWeather(RainAmount.drizzle),
    FixedWeather(RainAmount.rain),
    FixedWeather(RainAmount.downpour),
  ];

  String get label;
  Object? toJson();

  static CardWeather fromJson(Object? v) {
    if (v == 'picked') return picked;
    if (v == 'clear') return clear;
    if (v is Map && v['fixed'] is String) {
      return FixedWeather(RainAmount.parse(v['fixed'], RainAmount.drizzle));
    }
    return follow;
  }
}

class FollowWeather extends CardWeather {
  const FollowWeather();
  @override
  String get label => '설정 따름';
  @override
  Object? toJson() => null;
}

/// 「고른 카드만」일 때 비가 오는 카드. 비 양은 설정을 따른다.
class PickedWeather extends CardWeather {
  const PickedWeather();
  @override
  String get label => '이 카드도 비';
  @override
  Object? toJson() => 'picked';
}

class ClearWeather extends CardWeather {
  const ClearWeather();
  @override
  String get label => '맑음';
  @override
  Object? toJson() => 'clear';
}

class FixedWeather extends CardWeather {
  const FixedWeather(this.amount);
  final RainAmount amount;
  @override
  String get label => '${amount.label} 고정';
  @override
  Object? toJson() => {'fixed': amount.name};
  @override
  bool operator ==(Object other) => other is FixedWeather && other.amount == amount;
  @override
  int get hashCode => amount.hashCode;
}

class OsMotion {
  const OsMotion({this.reduceMotion = false, this.reduceTransparency = false});
  final bool reduceMotion, reduceTransparency;
}

/// 판정: [active] 가 거짓이면 날씨를 통째로 건너뛴다(비용 0), [moving] 이 거짓이면 맺힌 것만 남는다.
({bool active, bool moving}) verdict(WeatherSettings s, OsMotion os) {
  final osBlocks = !s.ignoreOs && os.reduceTransparency;
  final active = s.enabled && !osBlocks && (s.style == RainStyle.showers || s.effects.isNotEmpty);
  return (active: active, moving: active && (s.ignoreOs || !os.reduceMotion));
}

/// 카드 하나에 오는 비 양. 카드별 덮어쓰기가 「어디에」보다 이기고, 학생 상태는 비 양만 바꾼다.
RainAmount cardAmount(WeatherSettings s, CardWeather over, {required bool focused, Mood? mood}) {
  RainAmount base() => !s.byStatus || mood == null ? s.amount : s.moodAmount(mood);
  return switch (over) {
    ClearWeather() => RainAmount.none,
    FixedWeather(:final amount) => amount,
    PickedWeather() => base(),
    FollowWeather() => switch (s.target) {
      WeatherTarget.allWindows => base(),
      WeatherTarget.focusedOnly => focused ? base() : RainAmount.none,
      WeatherTarget.unfocusedOnly => focused ? RainAmount.none : base(),
      WeatherTarget.pickedOnly || WeatherTarget.backgroundOnly => RainAmount.none,
    },
  };
}

/// 카드 밖(화면 바탕)에 오는 비 양.
RainAmount backgroundAmount(WeatherSettings s) => switch (s.target) {
  WeatherTarget.allWindows || WeatherTarget.backgroundOnly => s.amount,
  _ => RainAmount.none,
};

/// 여러 학생이 든 카드(방)의 기분 — 가장 급한 것.
Mood? moodOf(Iterable<Mood?> students) {
  final all = students.whereType<Mood>().toList();
  if (all.contains(Mood.yourTurn)) return Mood.yourTurn;
  if (all.contains(Mood.busy)) return Mood.busy;
  if (all.isNotEmpty && all.every((m) => m == Mood.resting)) return Mood.resting;
  return null;
}
