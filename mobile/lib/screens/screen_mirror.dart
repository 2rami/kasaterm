import 'dart:async';

import 'package:flutter/material.dart';

import '../background_grace.dart';
import '../look.dart';
import '../screen_session.dart';
import '../server.dart';
import '../twins_loading.dart';
import 'controls.dart';

/// 기기 화면 거울 — 그 기기의 카사텀이 뜬 화면을 본다. 보기만 하고, 핀치·두 번 탭으로 키워 읽는다(`docs/screen-mirror.md`).
class ScreenMirrorScreen extends StatefulWidget {
  const ScreenMirrorScreen({
    super.key,
    required this.server,
    required this.title,
    this.machine,
  });

  final Server server;
  final String title;

  /// 허브 절의 길 — 기준 기기면 null.
  final String? machine;

  @override
  State<ScreenMirrorScreen> createState() => _ScreenMirrorScreenState();
}

class _ScreenMirrorScreenState extends State<ScreenMirrorScreen> {
  late final ScreenSession _session = ScreenSession(widget.server, machine: widget.machine);

  /// 화면이 멈추면 프레임이 안 와 다시 그릴 일이 없다 — 상태 줄의 fps 가 내려가도록 1초마다 다시 센다.
  Timer? _tick;

  final TransformationController _zoom = TransformationController();
  Offset _tapAt = Offset.zero;

  @override
  void initState() {
    super.initState();
    BackgroundGrace.instance.addListener(_graceChanged);
    _session.connect();
    _tick = Timer.periodic(const Duration(seconds: 1), (_) {
      if (mounted && _session.state == ScreenState.live) setState(() {});
    });
  }

  @override
  void dispose() {
    _zoom.dispose();
    _tick?.cancel();
    BackgroundGrace.instance.removeListener(_graceChanged);
    _session.dispose();
    super.dispose();
  }

  /// 두 번 탭 — 키운 상태면 전체로, 아니면 그 자리를 세 배로. 데스크톱 글자는 전체 보기에서 안 읽힌다.
  void _toggleZoom() {
    if (_zoom.value.getMaxScaleOnAxis() > 1.01) {
      _zoom.value = Matrix4.identity();
      return;
    }
    const scale = 3.0;
    _zoom.value = Matrix4.diagonal3Values(scale, scale, 1)
      ..setTranslationRaw(-_tapAt.dx * (scale - 1), -_tapAt.dy * (scale - 1), 0);
  }

  void _graceChanged() {
    if (BackgroundGrace.instance.live) {
      _session.resume();
    } else {
      _session.pause();
    }
  }

  String _status() {
    final s = _session;
    return switch (s.state) {
      ScreenState.connecting => '연결 중',
      ScreenState.reconnecting => '다시 연결 중',
      ScreenState.denied => s.denied == 'permission' ? '권한 없음' : '보낼 수 없음',
      ScreenState.live => () {
        final rtt = s.direct ? widget.server.pathOf(widget.machine)?.$2 : null;
        final path = s.direct ? (rtt == null ? '직통' : '직통 · ${rtt}ms') : '관문';
        final fps = s.fps;
        return '$path · ${fps == 0 ? '변화 없음' : '${fps}fps'}';
      }(),
    };
  }

  static String _deniedText(String? reason) => reason == 'permission'
      ? '그 기기의 카사텀에 화면 기록 권한이 없어요. 그 기기에서 시스템 설정 › 개인정보 보호 및 보안 › '
            '화면 및 시스템 오디오 녹음에서 kasaterm 을 켠 뒤 카사텀을 다시 열어 주세요.'
      : '이 기기는 화면을 보낼 수 없어요. 화면 없는 기기이거나 화면 거울이 없는 옛 판이에요.';

  @override
  Widget build(BuildContext context) => ListenableBuilder(
    listenable: _session,
    builder: (context, _) {
      final theme = Theme.of(context);
      final dim = theme.colorScheme.onSurfaceVariant;
      final displays = _session.displays;
      return Scaffold(
        appBar: AppBar(
          titleSpacing: 0,
          title: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Text(widget.title, overflow: TextOverflow.ellipsis),
              Text(
                _status(),
                style: theme.textTheme.bodySmall?.copyWith(color: dim),
                overflow: TextOverflow.ellipsis,
              ),
            ],
          ),
          actions: [
            if (displays.length > 1)
              PopupMenuButton<int>(
                tooltip: '화면 고르기',
                icon: const Icon(Icons.monitor_outlined),
                onSelected: _session.pickDisplay,
                itemBuilder: (_) => [
                  for (final (i, d) in displays.indexed)
                    CheckedPopupMenuItem(
                      value: d.id,
                      checked: d.id == _session.display,
                      child: Text('${d.main ? '주 화면' : '화면 ${i + 1}'} · ${d.width}×${d.height}'),
                    ),
                ],
              ),
          ],
        ),
        body: _body(theme),
      );
    },
  );

  Widget _body(ThemeData theme) {
    final image = _session.image;
    final note = _session.note;
    if (image == null) {
      final Widget center = _session.state == ScreenState.denied
          ? TwinsNotice(text: _deniedText(_session.denied))
          : const TwinsLoading(label: '화면을 받는 중', size: Look.twinsSmall);
      return ListView(
        padding: const EdgeInsets.all(Look.pagePad),
        children: [
          if (note != null) ErrorBand(text: note),
          Padding(padding: const EdgeInsets.only(top: Look.groupGap * 2), child: center),
        ],
      );
    }
    return Stack(
      fit: StackFit.expand,
      children: [
        GestureDetector(
          onDoubleTapDown: (d) => _tapAt = d.localPosition,
          onDoubleTap: _toggleZoom,
          child: InteractiveViewer(
            transformationController: _zoom,
            maxScale: 8,
            child: Center(
              child: AspectRatio(
                aspectRatio: image.width / image.height,
                child: RawImage(image: image, fit: BoxFit.fill, filterQuality: FilterQuality.medium),
              ),
            ),
          ),
        ),
        if (_session.state == ScreenState.reconnecting)
          const Positioned(top: 0, left: 0, right: 0, child: TwinBar()),
        if (note != null || _session.state == ScreenState.denied)
          Positioned(
            top: 0,
            left: Look.pagePad,
            right: Look.pagePad,
            child: ErrorBand(text: note ?? _deniedText(_session.denied)),
          ),
      ],
    );
  }
}
