import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:kasaterm_mobile/weather/card.dart';
import 'package:kasaterm_mobile/weather/model.dart';
import 'package:kasaterm_mobile/weather/scene.dart';
import 'package:kasaterm_mobile/weather/sim.dart';
import 'package:kasaterm_mobile/weather/store.dart';

void main() {
  const on = WeatherSettings(enabled: true);

  test('기본값은 결정 기록(docs/weather.md) 그대로', () {
    const s = WeatherSettings();
    expect(s.enabled, isFalse);
    expect(s.amount, RainAmount.drizzle);
    expect(s.target, WeatherTarget.focusedOnly);
    expect(s.effects, Effect.values.toSet());
    expect(s.wipe, WipeMode.onInput);
    expect(s.rewetSecs, 180);
    expect(s.byStatus, isFalse);
    expect((s.busy, s.yourTurn, s.resting), (RainAmount.drizzle, RainAmount.downpour, RainAmount.none));
  });

  test('데스크톱이 계정에 올린 JSON 을 그대로 읽고, 범위 밖 값은 다듬는다', () {
    final s = WeatherSettings.fromJson({
      'enabled': true,
      'amount': 'downpour',
      'wind_dir': -3.0,
      'wind_strength': 0.5,
      'target': 'background_only',
      'effects': {'streaks': true, 'drops': false, 'mist': true, 'ripples': true, 'buttons': false},
      'wipe': 'never',
      'rewet_secs': 5,
      'by_status': true,
      'busy': 'rain',
      'your_turn': 'none',
      'resting': 'drizzle',
      'ignore_os': true,
    });
    expect(s.amount, RainAmount.downpour);
    expect(s.windDir, -1);
    expect(s.wind, -0.5);
    expect(s.target, WeatherTarget.backgroundOnly);
    expect(s.has(Effect.drops), isFalse);
    expect(s.has(Effect.buttons), isFalse);
    expect(s.wipe, WipeMode.never);
    expect(s.rewetSecs, WeatherSettings.rewetMin);
    expect(s.moodAmount(Mood.busy), RainAmount.rain);
    expect(WeatherSettings.fromJson(s.toJson()), s);
    // 계정 동기화 검사기(`schema.rs weather`)가 받는 이름만 쓴다.
    expect(s.toJson().keys.toSet(), {
      'enabled', 'amount', 'wind_dir', 'wind_strength', 'target', 'effects', 'wipe',
      'rewet_secs', 'by_status', 'busy', 'your_turn', 'resting', 'ignore_os',
    });
  });

  test('「초점 카드만」이면 방금 만진 카드에만 온다', () {
    expect(cardAmount(on, CardWeather.follow, focused: true), RainAmount.drizzle);
    expect(cardAmount(on, CardWeather.follow, focused: false), RainAmount.none);
    final unfocused = on.copyWith(target: WeatherTarget.unfocusedOnly);
    expect(cardAmount(unfocused, CardWeather.follow, focused: true), RainAmount.none);
    expect(cardAmount(unfocused, CardWeather.follow, focused: false), RainAmount.drizzle);
  });

  test('카드별 덮어쓰기가 「어디에」를 이긴다', () {
    final bg = on.copyWith(target: WeatherTarget.backgroundOnly, amount: RainAmount.rain);
    expect(cardAmount(bg, CardWeather.follow, focused: true), RainAmount.none);
    expect(cardAmount(bg, const FixedWeather(RainAmount.downpour), focused: false), RainAmount.downpour);
    expect(cardAmount(on.copyWith(target: WeatherTarget.allWindows), CardWeather.clear, focused: true), RainAmount.none);
    final picked = on.copyWith(target: WeatherTarget.pickedOnly);
    expect(cardAmount(picked, CardWeather.follow, focused: true), RainAmount.none);
    expect(cardAmount(picked, CardWeather.picked, focused: false), RainAmount.drizzle);
  });

  test('학생 상태 날씨는 비 양만 바꾸고 「어디에」는 따른다', () {
    final s = on.copyWith(byStatus: true);
    expect(cardAmount(s, CardWeather.follow, focused: true, mood: Mood.yourTurn), RainAmount.downpour);
    expect(cardAmount(s, CardWeather.follow, focused: false, mood: Mood.yourTurn), RainAmount.none);
    expect(cardAmount(s, CardWeather.follow, focused: true), RainAmount.drizzle);
    expect(moodOf([Mood.resting, Mood.busy, null]), Mood.busy);
    expect(moodOf([Mood.resting, Mood.yourTurn]), Mood.yourTurn);
    expect(moodOf([null]), isNull);
  });

  test('카드 밖 바탕은 「전체」「카드 밖 배경만」일 때만 젖는다', () {
    expect(backgroundAmount(on), RainAmount.none);
    expect(backgroundAmount(on.copyWith(target: WeatherTarget.allWindows)), RainAmount.drizzle);
    expect(backgroundAmount(on.copyWith(target: WeatherTarget.backgroundOnly)), RainAmount.drizzle);
  });

  test('투명도 줄이기면 끄고, 동작 줄이기면 멈춘다 — OS 설정 무시면 둘 다 예외', () {
    expect(verdict(on, const OsMotion()), (active: true, moving: true));
    expect(verdict(on, const OsMotion(reduceTransparency: true)).active, isFalse);
    expect(verdict(on, const OsMotion(reduceMotion: true)), (active: true, moving: false));
    final ignore = on.copyWith(ignoreOs: true);
    expect(verdict(ignore, const OsMotion(reduceTransparency: true, reduceMotion: true)), (active: true, moving: true));
    expect(verdict(on.copyWith(effects: {}), const OsMotion()).active, isFalse);
    expect(verdict(const WeatherSettings(), const OsMotion()).active, isFalse);
  });

  test('이 카드 날씨는 JSON 을 오가도 같다', () {
    for (final w in CardWeather.menu) {
      expect(CardWeather.fromJson(w.toJson()), w);
    }
  });

  test('비가 그친 카드는 몇 초 안에 마르고, 닦은 길의 물은 걷힌다', () {
    final sim = GlassSim(1)..size = const Size(360, 150);
    for (var i = 0; i < 300; i++) {
      sim.step(1 / 30, RainAmount.downpour);
    }
    expect(sim.beads, isNotEmpty);
    sim.wipePath(const Offset(0, 75), const Offset(360, 75), radius: 200);
    expect(sim.beads, isEmpty);
    for (var i = 0; i < 300; i++) {
      sim.step(1 / 30, RainAmount.downpour);
    }
    for (var i = 0; i < 6 * 30; i++) {
      sim.step(1 / 30, RainAmount.none);
    }
    expect(sim.settled, isTrue);
  });

  testWidgets('iOS 「동작 줄이기」(reduceMotion 표시)를 받으면 날씨가 멈춘다', (tester) async {
    tester.platformDispatcher.accessibilityFeaturesTestValue = const FakeAccessibilityFeatures(reduceMotion: true);
    weather.didChangeAccessibilityFeatures();
    expect(weather.reduceMotion.value, isTrue);
    expect(verdict(on, OsMotion(reduceMotion: weather.reduceMotion.value)).moving, isFalse);
    tester.platformDispatcher.clearAccessibilityFeaturesTestValue();
    weather.didChangeAccessibilityFeatures();
    expect(weather.reduceMotion.value, isFalse);
  });

  testWidgets('날씨가 꺼져 있으면 카드와 화면 층은 아무것도 더 그리지 않는다', (tester) async {
    weather.settings.value = const WeatherSettings();
    await tester.pumpWidget(
      const MaterialApp(
        home: WeatherScene(child: WeatherCard(id: 'room:a', child: Text('방'))),
      ),
    );
    await tester.pump();
    expect(find.text('방'), findsOneWidget);
    expect(
      find.descendant(of: find.byType(WeatherCard), matching: find.byType(Listener)),
      findsNothing,
    );
    expect(
      find.descendant(of: find.byType(WeatherScene), matching: find.byType(CustomPaint)),
      findsNothing,
    );
    expect(tester.binding.transientCallbackCount, 0);
  });
}
