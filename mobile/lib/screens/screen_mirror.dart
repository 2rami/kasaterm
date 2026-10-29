import 'dart:async';

import 'package:flutter/gestures.dart';
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import '../background_grace.dart';
import '../look.dart';
import '../screen_session.dart';
import '../server.dart';
import '../twins_loading.dart';
import 'controls.dart';
import 'conversation_view.dart' show ChatComposer;

/// 기기 화면 거울 — 그 기기의 카사텀이 뜬 화면을 보고 조작한다(`docs/screen-mirror.md`). 보기가 기본이고 핀치·두 번 탭으로
/// 키워 읽는다. 조작(앱 바 단추)에서는 그 배율 그대로 탭 = 클릭, 길게 = 오른쪽 클릭, 끌기 = 휠이고 아래에 키 줄·글 넣기가 붙는다.
/// 둘로 나누는 까닭: 핀치·끌기가 확대·이동과 같은 손짓이고, 보다가 잘못 누르면 그 기기에서 일이 일어난다.
class ScreenMirrorScreen extends StatefulWidget {
  const ScreenMirrorScreen({
    super.key,
    required this.server,
    required this.title,
    this.machine,
    @visibleForTesting this.session,
  });

  final Server server;
  final String title;

  /// 허브 절의 길 — 기준 기기면 null.
  final String? machine;

  /// 시험이 진짜 소켓으로 미리 붙여 둔 세션. 화면이 맡아 닫는다.
  final ScreenSession? session;

  @override
  State<ScreenMirrorScreen> createState() => _ScreenMirrorScreenState();
}

class _ScreenMirrorScreenState extends State<ScreenMirrorScreen> {
  late final ScreenSession _session = widget.session ?? ScreenSession(widget.server, machine: widget.machine);

  /// 화면이 멈추면 프레임이 안 와 다시 그릴 일이 없다 — 상태 줄의 fps 가 내려가도록 1초마다 다시 센다.
  Timer? _tick;

  final TransformationController _zoom = TransformationController();
  Offset _tapAt = Offset.zero;

  bool _control = false;
  final TextEditingController _text = TextEditingController();
  final FocusNode _textFocus = FocusNode();

  /// 끌기 — 시작한 자리(휠은 그 자리의 창이 받는다)와 아직 못 보낸 1포인트 밑 나머지.
  Offset? _dragFrom;
  Offset _wheel = Offset.zero;

  @override
  void initState() {
    super.initState();
    BackgroundGrace.instance.addListener(_graceChanged);
    if (widget.session == null) _session.connect();
    _tick = Timer.periodic(const Duration(seconds: 1), (_) {
      if (mounted && _session.state == ScreenState.live) setState(() {});
    });
  }

  @override
  void dispose() {
    _zoom.dispose();
    _text.dispose();
    _textFocus.dispose();
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

  /// 끈 거리를 그 기기 화면 포인트로 — 손가락 아래 내용이 손가락을 따라오게. 배율을 키웠어도 그림 상자 기준이라 같다.
  void _drag(DragUpdateDetails d, Size box) {
    final size = _session.displaySize;
    _wheel += Offset(d.delta.dx * size.width / box.width, d.delta.dy * size.height / box.height);
    final (dx, dy) = (_wheel.dx.truncate(), _wheel.dy.truncate());
    if (dx == 0 && dy == 0) return;
    _wheel -= Offset(dx.toDouble(), dy.toDouble());
    final (x, y) = screenFraction(_dragFrom ?? d.localPosition, box);
    _session.scroll(x, y, dx, dy);
  }

  void _sendText() {
    _session.typeText(_text.text);
    _text.clear();
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
        return '$path · ${fps == 0 ? '변화 없음' : '${fps}fps'}${_control ? ' · 조작 중' : ''}';
      }(),
    };
  }

  static String _deniedText(String? reason) => reason == 'permission'
      ? '그 기기의 카사텀에 화면 기록 권한이 없어요. 그 기기에서 시스템 설정 › 개인정보 보호 및 보안 › '
            '화면 및 시스템 오디오 녹음에서 kasaterm 을 켠 뒤 카사텀을 다시 열어 주세요.'
      : '이 기기는 화면을 보낼 수 없어요. 화면 없는 기기이거나 화면 거울이 없는 옛 판이에요.';

  static String _inputDeniedText(String reason) => reason == 'permission'
      ? '그 기기의 카사텀에 손쉬운 사용 권한이 없어 조작이 안 들어가요. 그 기기에서 시스템 설정 › 개인정보 보호 및 보안 › '
            '손쉬운 사용에서 kasaterm 을 켜 주세요. 켜면 바로 먹어요.'
      : '이 기기는 조작을 받을 수 없어요. 조작이 없는 옛 판이거나 지원하지 않는 OS 예요.';

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
            IconButton(
              tooltip: _control ? '보기로' : '조작하기',
              isSelected: _control,
              icon: const Icon(Icons.touch_app_outlined),
              selectedIcon: const Icon(Icons.touch_app),
              onPressed: _session.image == null
                  ? null
                  : () => setState(() {
                      _control = !_control;
                      if (!_control) _textFocus.unfocus();
                    }),
            ),
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
        body: Column(
          children: [
            Expanded(child: _body(theme)),
            if (_control && _session.image != null) ...[
              _keys(),
              ChatComposer(
                controller: _text,
                focusNode: _textFocus,
                enabled: _session.state == ScreenState.live,
                onSend: _sendText,
                hint: '그 기기에 글 넣기',
              ),
            ],
          ],
        ),
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
    final picture = RawImage(image: image, fit: BoxFit.fill, filterQuality: FilterQuality.medium);
    final stage = Center(
      child: AspectRatio(
        aspectRatio: image.width / image.height,
        child: _control ? _controlSurface(picture) : picture,
      ),
    );
    final inputDenied = _session.inputDenied;
    return Stack(
      fit: StackFit.expand,
      children: [
        if (_control)
          // 보기에서 키운 배율·자리를 그대로 쥔다. 확대 손이 없어야 한 손가락 끌기가 휠로 간다.
          DecoratedBox(
            position: DecorationPosition.foreground,
            decoration: BoxDecoration(border: Border.all(color: theme.colorScheme.primary, width: 2)),
            child: ClipRect(child: Transform(transform: _zoom.value, child: stage)),
          )
        else
          GestureDetector(
            onDoubleTapDown: (d) => _tapAt = d.localPosition,
            onDoubleTap: _toggleZoom,
            child: InteractiveViewer(transformationController: _zoom, maxScale: 8, child: stage),
          ),
        if (_session.state == ScreenState.reconnecting)
          const Positioned(top: 0, left: 0, right: 0, child: TwinBar()),
        if (note != null || _session.state == ScreenState.denied)
          Positioned(
            top: 0,
            left: Look.pagePad,
            right: Look.pagePad,
            child: ErrorBand(text: note ?? _deniedText(_session.denied)),
          )
        else if (_control && inputDenied != null)
          Positioned(
            top: 0,
            left: Look.pagePad,
            right: Look.pagePad,
            child: ErrorBand(text: _inputDeniedText(inputDenied)),
          ),
      ],
    );
  }

  /// 조작 손 — 그림 상자 안 자리를 그 화면 비율로 넘긴다.
  Widget _controlSurface(Widget picture) => LayoutBuilder(
    builder: (context, c) {
      final box = c.biggest;
      return GestureDetector(
        behavior: HitTestBehavior.opaque,
        // 문턱을 넘기 전 거리까지 굴린다 — 내용이 손가락에서 떨어져 뒤처지지 않게.
        dragStartBehavior: DragStartBehavior.down,
        onTapUp: (d) {
          final (x, y) = screenFraction(d.localPosition, box);
          _session.click(x, y);
        },
        onLongPressStart: (d) {
          HapticFeedback.selectionClick();
          final (x, y) = screenFraction(d.localPosition, box);
          _session.click(x, y, right: true);
        },
        onPanStart: (d) {
          _dragFrom = d.localPosition;
          _wheel = Offset.zero;
        },
        onPanUpdate: (d) => _drag(d, box),
        onPanEnd: (_) => _dragFrom = null,
        child: picture,
      );
    },
  );

  Widget _keys() {
    final keys = <Widget>[
      KeyChip(label: 'esc', onTap: () => _session.key('Escape')),
      KeyChip(label: 'tab', onTap: () => _session.key('Tab')),
      KeyChip(icon: Icons.keyboard_arrow_left, onTap: () => _session.key('ArrowLeft')),
      KeyChip(icon: Icons.keyboard_arrow_down, onTap: () => _session.key('ArrowDown')),
      KeyChip(icon: Icons.keyboard_arrow_up, onTap: () => _session.key('ArrowUp')),
      KeyChip(icon: Icons.keyboard_arrow_right, onTap: () => _session.key('ArrowRight')),
      KeyChip(icon: Icons.backspace_outlined, onTap: () => _session.key('Backspace')),
      KeyChip(icon: Icons.keyboard_return, onTap: () => _session.key('Enter')),
    ];
    return KeyRow(keys: keys);
  }
}
