import 'dart:math' as math;
import 'dart:typed_data';
import 'dart:ui';

import 'model.dart';

/// 비 양마다 1만 pt² 에 1초마다 맺히는 알갱이·굵은 방울 수(리퀴드 비 2차 시제품에서 고른 값).
({double beads, double drops}) rainRates(RainAmount a) => switch (a) {
  RainAmount.none => (beads: 0.0, drops: 0.0),
  RainAmount.drizzle => (beads: 2.5, drops: 0.05),
  RainAmount.rain => (beads: 6.0, drops: 0.3),
  RainAmount.downpour => (beads: 14.0, drops: 1.4),
};

/// 카드 한 장 크기의 유리에 맺히는 물. raindrop-fx(SardineFish, MIT)의 흐름 — 작은 알갱이가
/// 맺혀 합쳐지다 무거워지면 멈칫거리며 미끄러지고, 지나간 자리에 자국 알갱이를 남긴다 — 을
/// 카드 단위로 새로 짰다. 원본과 달리 흘러내린 물은 카드 아래 테두리에 고인다(수면 높이 + 1차원 물결).
class GlassSim {
  GlassSim(this.seed) : _rnd = math.Random(seed);

  final int seed;
  final math.Random _rnd;
  Size size = Size.zero;

  final beads = <Bead>[];
  final runs = <Run>[];

  static const poolCells = 32;
  static const maxLevel = 12.0;
  final pool = Float64List(poolCells);
  final _poolV = Float64List(poolCells);
  double level = 0;

  double _beadAcc = 0, _dropAcc = 0, _time = 0;

  double get _area => size.width * size.height / 10000;
  int get _beadCap => (size.width * size.height / 200).round();

  double _range(double a, double b) => a + _rnd.nextDouble() * (b - a);

  double surfaceAt(double x) {
    final f = (x / size.width).clamp(0.0, 1.0) * (poolCells - 1);
    final i = f.floor(), j = math.min(i + 1, poolCells - 1);
    return size.height - (level + pool[i] + (pool[j] - pool[i]) * (f - i));
  }

  Bead? _spawn(bool big) {
    final r = big ? _range(1.8, 3.0) : _range(0.6, 1.3);
    final p = Offset(_rnd.nextDouble() * size.width, _rnd.nextDouble() * (size.height - level - 2));
    if (beads.length >= _beadCap && !big) {
      // 자리가 다 찼으면 새로 맺히는 대신 아무 알갱이나 굵어진다 — 오래 둘수록 알이 커진다.
      final b = beads[_rnd.nextInt(beads.length)];
      b.r = math.sqrt(b.r * b.r + r * r);
      return b;
    }
    var b = Bead(p, r, _range(2.9, 4.1));
    for (final o in List<Bead>.of(beads)) {
      if ((o.p - b.p).distance < (o.r + b.r) * 0.9) {
        final a1 = o.r * o.r, a2 = b.r * b.r;
        o.p = (o.p * a1 + b.p * a2) / (a1 + a2);
        o.r = math.sqrt(a1 + a2);
        b = o;
        beads.remove(o);
      }
    }
    beads.add(b);
    return b;
  }

  void _land(double x, double r) {
    level = math.min(maxLevel, level + math.pi * r * r * 1.6 / size.width);
    final i = ((x / size.width).clamp(0.0, 1.0) * (poolCells - 1)).round();
    _poolV[i] -= r * 5;
  }

  /// [rate] 는 「젖는 시간」이 짧을수록 커진다. [wind] −1~1 은 흐르는 방울을 옆으로 민다.
  void step(double dt, RainAmount amount, {double rate = 1, double wind = 0, bool impacts = false}) {
    if (size.isEmpty) return;
    _time += dt;
    _wind = wind;
    final r = rainRates(amount);
    _beadAcc += r.beads * rate * _area * dt;
    _dropAcc += r.drops * rate * _area * dt;
    for (; _beadAcc >= 1; _beadAcc--) {
      _promote(_spawn(false));
    }
    for (; _dropAcc >= 1; _dropAcc--) {
      final b = _spawn(true);
      if (impacts && b != null) ripple(b.p, strength: 0.6);
      _promote(b);
    }

    // 비가 안 오게 된 카드는 몇 초 안에 마른다 — 대상에서 빠진 카드가 계속 젖어 있으면 설정이 안
    // 먹는 것처럼 보인다(데스크톱과 같은 판단).
    final dry = amount == RainAmount.none;
    final evap = dry ? 1.0 : 0.006;
    for (final b in beads) {
      b.r -= evap * dt;
    }
    beads.removeWhere((b) => b.r < 0.35 || b.p.dy + b.r > surfaceAt(b.p.dx));
    if (dry) runs.removeWhere((d) => (d.r -= evap * dt) < 1.3);
    // 고인 물은 차오를수록 테두리로 빨리 넘쳐 빠진다 — 그냥 두면 큰비에 금방 차서 카드 아래를 덮은 채 남는다.
    level = math.max(0, level - (dry ? 3.0 : 0.02 + level * 0.2) * dt);

    for (final run in runs) {
      _move(run, dt);
    }
    runs.removeWhere((r) => r.dead);
    _waves(dt);
    for (final w in ripples) {
      w.age += dt;
    }
    ripples.removeWhere((w) => w.age > Ripple.life);
  }

  /// 물이 다 걷혀 더 그릴 것이 없는지.
  bool get settled =>
      beads.isEmpty && runs.isEmpty && level < 0.05 && ripples.isEmpty;

  final ripples = <Ripple>[];
  double _wind = 0;

  /// 누른 자리·떨어진 빗방울에서 퍼지는 고리. 셰이더가 한 번에 [Ripple.max] 개까지 받는다.
  void ripple(Offset at, {double strength = 1}) {
    if (ripples.length >= Ripple.max) ripples.removeAt(0);
    ripples.add(Ripple(at, strength));
  }

  void _promote(Bead? b) {
    if (b == null || b.r < b.slideAt) return;
    beads.remove(b);
    runs.add(Run(b.p, b.r)..nextTrail = _range(6, 12));
  }

  void _move(Run d, double dt) {
    if (_time >= d.nextRoll) {
      d.nextRoll = _time + _range(0.1, 0.45);
      final vmax = math.min(170.0, 30 + 28 * (d.r - 2.5));
      d.target = _rnd.nextDouble() < 0.3 ? 0 : _rnd.nextDouble() * vmax;
      d.shift = _range(-0.08, 0.08) + _wind * 0.25;
    }
    d.vy += (d.target - d.vy) * math.min(1, dt * 6);
    final from = d.p;
    d.p += Offset(d.shift * d.vy * dt, d.vy * dt);
    d.travelled += (d.p - from).distance;

    // 지나가는 길의 알갱이를 삼킨다.
    beads.removeWhere((o) {
      if ((o.p - d.p).distance >= d.r + o.r * 0.8) return false;
      d.r = math.sqrt(d.r * d.r + o.r * o.r * 0.9);
      return true;
    });
    for (final o in runs) {
      if (identical(o, d) || o.dead || (o.p - d.p).distance >= (o.r + d.r) * 0.8) continue;
      final (big, small) = d.r >= o.r ? (d, o) : (o, d);
      big.r = math.sqrt(big.r * big.r + small.r * small.r);
      small.dead = true;
    }
    if (d.travelled >= d.nextTrail && d.r > 2.2) {
      d.travelled = 0;
      d.nextTrail = _range(6, 12);
      final t = math.max(0.5, d.r * _range(0.28, 0.4));
      d.r = math.sqrt(math.max(0, d.r * d.r - t * t));
      beads.add(Bead(d.p - Offset(0, d.r * 0.8), t, _range(2.9, 4.1)));
    }
    if (d.r < 1.3) {
      d.dead = true;
      beads.add(Bead(d.p, d.r, 9));
    } else if (d.p.dy + d.r * 1.1 >= surfaceAt(d.p.dx)) {
      d.dead = true;
      _land(d.p.dx, d.r);
    }
  }

  void _waves(double dt) {
    for (var i = 0; i < poolCells; i++) {
      final l = pool[math.max(0, i - 1)], r = pool[math.min(poolCells - 1, i + 1)];
      final a = (l + r - 2 * pool[i]) * 400 - pool[i] * 30 - _poolV[i] * 3.5;
      _poolV[i] += a * dt;
    }
    for (var i = 0; i < poolCells; i++) {
      pool[i] = (pool[i] + _poolV[i] * dt).clamp(-4.0, 4.0);
    }
  }
}

class Bead {
  Bead(this.p, this.r, this.slideAt);

  Offset p;
  double r;

  /// 이만큼 굵어지면 제 무게로 미끄러진다.
  final double slideAt;
}

class Run {
  Run(this.p, this.r);

  Offset p;
  double r;
  double vy = 0, target = 0, shift = 0, nextRoll = 0, travelled = 0, nextTrail = 8;
  bool dead = false;
}

class Ripple {
  Ripple(this.at, this.strength);

  static const max = 8;
  static const life = 1.2;

  final Offset at;
  final double strength;
  double age = 0;
}
