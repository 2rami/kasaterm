import 'dart:ui' as ui;

import 'package:flutter/material.dart';

import 'look.dart';
import 'server.dart';

/// 걷기 한 벌 — 데스크톱 사이드바가 그리는 프레임 그대로(같은 크기)와 한 프레임의 길이.
class WalkSheet {
  const WalkSheet(this.frames, this.frameMs);
  final List<ui.Image> frames;
  final int frameMs;
}

/// 받은 걷기는 주소마다 한 번 받아 잘라 둔다 — 칸이 일을 시작·멈출 때마다 새로 받으면 터널 너머에서 얼굴과
/// 걷기가 번갈아 깜빡인다. 데스크톱이 테마를 갈아입으면 같은 주소가 다른 학생 그림이 되므로 서버 캐시와 같은
/// 1분만 쥔다. 못 받은 답(옛 서버의 404)도 같이 쥔다 — 칸마다 매번 다시 묻지 않는다.
class WalkSheets {
  WalkSheets({this.keep = const Duration(minutes: 1), DateTime Function()? now})
      : _now = now ?? DateTime.now;

  final Duration keep;
  final DateTime Function() _now;
  final _got = <String, _Got>{};

  _Got? _fresh(Server server, String slug, String? machine) {
    final hit = _got[server.walk(slug, machine: machine).toString()];
    return hit != null && _now().difference(hit.at) < keep ? hit : null;
  }

  /// 이미 받아 둔 것 — 첫 그림부터 걷게 한다(비동기로만 받으면 칸이 다시 설 때마다 한 장은 얼굴이다).
  WalkSheet? ready(Server server, String slug, String? machine) =>
      _fresh(server, slug, machine)?.sheet;

  Future<WalkSheet?> load(Server server, String slug, String? machine) {
    final hit = _fresh(server, slug, machine);
    if (hit != null) return hit.done;
    final got = _Got(_now(), _fetch(server, slug, machine));
    got.done.then((s) => got.sheet = s);
    _got[server.walk(slug, machine: machine).toString()] = got;
    return got.done;
  }

  static Future<WalkSheet?> _fetch(Server server, String slug, String? machine) async {
    try {
      final got = await server.walkSheet(slug, machine: machine);
      if (got == null) return null;
      final sheet = await decodeImageFromList(got.png);
      if (sheet.width % got.frames != 0) return null;
      final w = sheet.width ~/ got.frames;
      // 한 장째로 잘라 그리면 줄여 그릴 때 이웃 프레임 가장자리가 번진다 — 미리 프레임마다 떼어 둔다.
      return WalkSheet([
        for (var i = 0; i < got.frames; i++)
          await _crop(sheet, Rect.fromLTWH((i * w).toDouble(), 0, w.toDouble(), sheet.height.toDouble())),
      ], got.frameMs);
    } catch (_) {
      return null;
    }
  }

  static Future<ui.Image> _crop(ui.Image sheet, Rect r) {
    final rec = ui.PictureRecorder();
    Canvas(rec).drawImageRect(sheet, r, Offset.zero & r.size, Paint()..filterQuality = FilterQuality.none);
    return rec.endRecording().toImage(r.width.toInt(), r.height.toInt());
  }
}

class _Got {
  _Got(this.at, this.done);
  final DateTime at;
  final Future<WalkSheet?> done;
  WalkSheet? sheet;
}

final walkSheets = WalkSheets();

/// 일하는 학생 — 데스크톱 사이드바처럼 얼굴 상자 안에서 제자리걸음한다. 걷기를 받기 전·못 받으면(옛 서버·외형을
/// 끈 데스크톱·그림 없는 학생) [fallback](얼굴)이 그 자리에 선다.
class StudentWalk extends StatefulWidget {
  const StudentWalk({
    super.key,
    required this.server,
    required this.slug,
    this.machine,
    required this.size,
    required this.fallback,
    this.sheets,
  });

  final Server server;
  final String slug;
  final String? machine;
  final double size;
  final Widget fallback;

  /// 시험이 캐시를 따로 쓴다 — 없으면 앱 전체가 함께 쓰는 [walkSheets].
  final WalkSheets? sheets;

  @override
  State<StudentWalk> createState() => _StudentWalkState();
}

class _StudentWalkState extends State<StudentWalk> with SingleTickerProviderStateMixin {
  late final AnimationController _clock = AnimationController(vsync: this)..addListener(_tick);
  final _frame = ValueNotifier(0);
  WalkSheet? _sheet;

  WalkSheets get _sheets => widget.sheets ?? walkSheets;

  @override
  void initState() {
    super.initState();
    _load();
  }

  @override
  void didChangeDependencies() {
    super.didChangeDependencies();
    _sync();
  }

  @override
  void didUpdateWidget(StudentWalk old) {
    super.didUpdateWidget(old);
    if (old.server != widget.server || old.slug != widget.slug || old.machine != widget.machine) {
      _load();
      _sync();
    }
  }

  void _load() {
    final (server, slug, machine) = (widget.server, widget.slug, widget.machine);
    _sheet = _sheets.ready(server, slug, machine);
    if (_sheet != null) return;
    _sheets.load(server, slug, machine).then((s) {
      if (!mounted || s == null) return;
      if (server != widget.server || slug != widget.slug || machine != widget.machine) return;
      setState(() => _sheet = s);
      _sync();
    });
  }

  /// 움직임 줄이기에서는 첫 프레임에 서 있다 — 걷는 자세가 「하는 중」을 말하고, 칸 윤곽 빛도 그때 멈춘다.
  void _sync() {
    final s = _sheet;
    if (s == null || s.frames.length < 2 || Look.still(context)) {
      _clock.stop();
      _frame.value = 0;
      return;
    }
    final lap = Duration(milliseconds: s.frames.length * s.frameMs);
    if (_clock.duration != lap || !_clock.isAnimating) {
      _clock.duration = lap;
      _clock.repeat();
    }
  }

  void _tick() {
    final n = _sheet?.frames.length ?? 0;
    if (n > 0) _frame.value = (_clock.value * n).floor() % n;
  }

  @override
  void dispose() {
    _clock.dispose();
    _frame.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final s = _sheet;
    if (s == null) return widget.fallback;
    // 데스크톱 `fit_terminal_art`(전경)와 같은 자리 — 비율을 지켜 상자에 담고 가로 가운데·바닥에 세운다.
    return SizedBox.square(
      dimension: widget.size,
      child: ValueListenableBuilder<int>(
        valueListenable: _frame,
        builder: (_, i, _) => RawImage(
          image: s.frames[i],
          fit: BoxFit.contain,
          alignment: Alignment.bottomCenter,
          filterQuality: FilterQuality.medium,
        ),
      ),
    );
  }
}
