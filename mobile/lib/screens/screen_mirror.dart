import 'dart:async';
import 'dart:convert';

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

/// 기기 화면 거울의 손 — 보기는 확대·이동만, 터치는 누른 자리, 트랙패드는 옮긴 만큼 커서가 간다.
enum MirrorInput { view, touch, trackpad }

/// 그 기기 입력기가 한 번에 받는 양을 넘지 않게 글을 UTF-8 4000바이트 안쪽으로 나눈다.
Iterable<String> remoteTextChunks(String value) sync* {
  var chunk = StringBuffer();
  var bytes = 0;
  for (final rune in value.runes) {
    final ch = String.fromCharCode(rune);
    final size = utf8.encode(ch).length;
    if (bytes + size > 4000) {
      yield chunk.toString();
      chunk = StringBuffer();
      bytes = 0;
    }
    chunk.write(ch);
    bytes += size;
  }
  if (bytes > 0) yield chunk.toString();
}

/// 기기 화면 거울 — 그 기기의 카사텀이 뜬 화면을 보고 조작한다(`docs/screen-mirror.md`). 보기가 기본이고 핀치·두 번 탭으로
/// 키워 읽는다. 아래 고르개로 터치·트랙패드를 고르면 그 배율 그대로 조작한다. 보기를 따로 두는 까닭: 핀치·끌기가 확대·이동과
/// 같은 손짓이고, 보다가 잘못 누르면 그 기기에서 일이 일어난다.
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
  late final ScreenSession _session =
      widget.session ?? ScreenSession(widget.server, machine: widget.machine);

  /// 화면이 멈추면 프레임이 안 와 다시 그릴 일이 없다 — 상태 줄의 fps 가 내려가도록 1초마다 다시 센다.
  Timer? _tick;
  Timer? _noticeHide;

  final TransformationController _zoom = TransformationController();
  final FocusNode _keys = FocusNode();
  Offset _tapAt = Offset.zero;

  MirrorInput _mode = MirrorInput.view;
  bool _dragging = false;
  bool _feedback = true;
  int _modifiers = 0;

  /// 그 기기에 눌러 둔 단추(1 왼쪽·2 오른쪽) — 0 이면 안 누름. 떼지 않고 끝나면 그 기기 마우스가 눌린 채 남는다.
  int _held = 0;
  Offset _cursor = const Offset(.5, .5);
  final Map<int, Offset> _fingers = {};
  Offset? _start;
  DateTime _startedAt = DateTime.now();
  bool _moved = false;
  bool _multi = false;
  Offset _wheel = Offset.zero;
  String _notice = '';

  bool get _canInput =>
      BackgroundGrace.instance.live &&
      _session.state == ScreenState.live &&
      _session.hasPicture &&
      _session.inputDenied == null &&
      _mode != MirrorInput.view;

  @override
  void initState() {
    super.initState();
    BackgroundGrace.instance.addListener(_graceChanged);
    _session.addListener(_sessionChanged);
    if (widget.session == null) _session.connect();
    _tick = Timer.periodic(const Duration(seconds: 1), (_) {
      if (mounted && _session.state == ScreenState.live) setState(() {});
    });
  }

  @override
  void dispose() {
    _session.removeListener(_sessionChanged);
    _release();
    _zoom.dispose();
    _keys.dispose();
    _tick?.cancel();
    _noticeHide?.cancel();
    BackgroundGrace.instance.removeListener(_graceChanged);
    _session.dispose();
    super.dispose();
  }

  void _sessionChanged() {
    if (_session.state != ScreenState.live || _session.inputDenied != null) {
      _release();
      _modifiers = 0;
    }
  }

  void _graceChanged() {
    if (BackgroundGrace.instance.live) {
      _session.resume();
    } else {
      _release();
      _modifiers = 0;
      _session.pause();
    }
  }

  /// 손가락 기록을 비우고 눌러 둔 단추를 그 자리에서 뗀다.
  void _release() {
    if (_held != 0) _session.pointer(_cursor.dx, _cursor.dy, 0, modifiers: _modifiers);
    _held = 0;
    _fingers.clear();
    _start = null;
    _multi = false;
    _moved = false;
  }

  void _show(String text) {
    if (!_feedback || !mounted) return;
    _noticeHide?.cancel();
    setState(() => _notice = text);
    _noticeHide = Timer(const Duration(milliseconds: 1150), () {
      if (mounted) setState(() => _notice = '');
    });
  }

  void _setMode(MirrorInput mode) {
    _release();
    setState(() {
      _mode = mode;
      _modifiers = 0;
      _dragging = false;
      _notice = '';
    });
    if (mode != MirrorInput.view) _keys.requestFocus();
  }

  static String _combo(int modifiers, String key) => [
    if (modifiers & 2 != 0) 'Ctrl',
    if (modifiers & 4 != 0) 'Alt',
    if (modifiers & 8 != 0) '⌘',
    if (modifiers & 1 != 0) 'Shift',
    key,
  ].join(' + ');

  void _key(String code, String label) {
    if (!_canInput) return;
    _session.key(code, modifiers: _modifiers);
    _show(_combo(_modifiers, label));
    setState(() => _modifiers = 0);
  }

  Future<void> _typeSheet() async {
    if (!_canInput) return;
    final text = await showModalBottomSheet<String>(
      context: context,
      isScrollControlled: true,
      useSafeArea: true,
      builder: (_) => const _TypeSheet(),
    );
    if (!mounted || !_canInput || text == null || text.isEmpty) return;
    if (remoteTextChunks(text).every(_session.typeText)) {
      _show('글 넣기 · ${text.characters.length}자');
    }
    _keys.requestFocus();
  }

  /// 그 기기 화면이 지금 그려진 상자 — 가운데 맞춤 그림에 보기에서 키운 배율·자리를 입힌다.
  Rect _pictureRect(Size area, Size picture) {
    final fitted = Alignment.center.inscribe(
      applyBoxFit(BoxFit.contain, picture, area).destination,
      Offset.zero & area,
    );
    return MatrixUtils.transformRect(_zoom.value, fitted);
  }

  Offset _fraction(Offset p, Rect r) => Offset(
    ((p.dx - r.left) / r.width).clamp(0.0, 1.0),
    ((p.dy - r.top) / r.height).clamp(0.0, 1.0),
  );

  void _down(PointerDownEvent e, Rect r) {
    if (!_canInput || (_mode == MirrorInput.touch && !r.contains(e.localPosition))) return;
    _fingers[e.pointer] = e.localPosition;
    if (_fingers.length > 1) {
      if (_held != 0) {
        _session.pointer(_cursor.dx, _cursor.dy, 0, modifiers: _modifiers);
        _held = 0;
      }
      _multi = true;
      _wheel = Offset.zero;
      return;
    }
    _multi = false;
    _moved = false;
    _start = e.localPosition;
    _startedAt = DateTime.now();
    if (_mode == MirrorInput.touch || e.kind == PointerDeviceKind.mouse) {
      _cursor = _fraction(e.localPosition, r);
    }
    if (_dragging || e.kind == PointerDeviceKind.mouse) {
      _held = e.buttons & 3 == 0 ? 1 : e.buttons & 3;
      _session.pointer(_cursor.dx, _cursor.dy, _held, modifiers: _modifiers);
      _show(_held == 2 ? '오른쪽 누름' : '누름');
    }
    setState(() {});
  }

  void _move(PointerMoveEvent e, Rect r) {
    final previous = _fingers[e.pointer];
    if (!_canInput || previous == null) return;
    _fingers[e.pointer] = e.localPosition;
    if (_multi) {
      if (_fingers.length != 2) return;
      // 두 손가락이 각자 움직임을 보내니 반씩 센다. 1포인트 밑은 모아 두어 느린 손짓도 굴린다.
      final d = e.localPosition - previous;
      _moved |= d.distance > 1;
      final size = _session.displaySize;
      _wheel += Offset(d.dx * size.width / r.width / 2, d.dy * size.height / r.height / 2);
      final (dx, dy) = (_wheel.dx.truncate(), _wheel.dy.truncate());
      if (dx == 0 && dy == 0) return;
      _wheel -= Offset(dx.toDouble(), dy.toDouble());
      _session.scroll(_cursor.dx, _cursor.dy, dx, dy);
      _show('스크롤');
      return;
    }
    _moved |= (e.localPosition - _start!).distance > 6;
    _cursor = _mode == MirrorInput.touch || e.kind == PointerDeviceKind.mouse
        ? _fraction(e.localPosition, r)
        : Offset(
            (_cursor.dx + e.localDelta.dx / r.width).clamp(0.0, 1.0),
            (_cursor.dy + e.localDelta.dy / r.height).clamp(0.0, 1.0),
          );
    if (_moved || _held != 0) {
      _session.pointer(_cursor.dx, _cursor.dy, _held, modifiers: _modifiers);
    }
    setState(() {});
  }

  void _up(PointerUpEvent e) {
    if (_fingers.remove(e.pointer) == null) return;
    final quick = DateTime.now().difference(_startedAt).inMilliseconds < 450;
    if (_held != 0) {
      _session.pointer(_cursor.dx, _cursor.dy, 0, modifiers: _modifiers);
      _held = 0;
      _show('놓기');
    } else if (!_multi && !_moved && _canInput) {
      _session.click(_cursor.dx, _cursor.dy, right: !quick, modifiers: _modifiers);
      if (!quick) HapticFeedback.selectionClick();
      _show(quick ? '클릭' : '오른쪽 클릭');
    }
    if (_fingers.isEmpty && _multi && !_moved && _canInput && quick) {
      _session.click(_cursor.dx, _cursor.dy, right: true, modifiers: _modifiers);
      HapticFeedback.selectionClick();
      _show('오른쪽 클릭');
    }
    if (_fingers.isEmpty) {
      _start = null;
      _multi = false;
      setState(() => _modifiers = 0);
    }
  }

  void _cancel(PointerCancelEvent e) {
    if (_fingers.remove(e.pointer) == null) return;
    _release();
    setState(() => _modifiers = 0);
  }

  void _hover(PointerHoverEvent e, Rect r) {
    if (!_canInput || e.kind != PointerDeviceKind.mouse || !r.contains(e.localPosition)) return;
    _cursor = _fraction(e.localPosition, r);
    _session.pointer(_cursor.dx, _cursor.dy, 0, modifiers: _modifiers);
    setState(() {});
  }

  void _signal(PointerSignalEvent e) {
    if (!_canInput || e is! PointerScrollEvent) return;
    _session.scroll(_cursor.dx, _cursor.dy, -e.scrollDelta.dx.round(), -e.scrollDelta.dy.round());
    _show('스크롤');
  }

  static const _namedKeys = {
    'Enter', 'Tab', 'Backspace', 'Escape', 'Delete', 'Home', 'End', 'PageUp', 'PageDown', //
    'ArrowLeft', 'ArrowRight', 'ArrowDown', 'ArrowUp',
  };

  /// 하드웨어 자판(아이패드·블루투스) — 글자는 글자째, 단축키와 이름 있는 키는 키 코드로 넘긴다.
  KeyEventResult _hardware(FocusNode node, KeyEvent e) {
    if (!_canInput || ModalRoute.of(context)?.isCurrent != true || e is KeyUpEvent) {
      return KeyEventResult.ignored;
    }
    final hk = HardwareKeyboard.instance;
    final m = _modifiers |
        (hk.isShiftPressed ? 1 : 0) |
        (hk.isControlPressed ? 2 : 0) |
        (hk.isAltPressed ? 4 : 0) |
        (hk.isMetaPressed ? 8 : 0);
    final code = e.physicalKey.debugName?.replaceAll(' ', '');
    final character = e.character;
    if (character != null && character.runes.every((r) => r >= 32) && m & 14 == 0) {
      _session.typeText(character);
      _show('글자');
    } else if (code != null &&
        (_namedKeys.contains(code) || RegExp(r'^(Key[A-Z]|Digit[0-9]|F[0-9]{1,2})$').hasMatch(code))) {
      _session.key(code, modifiers: m);
      _show(_combo(m, code.replaceFirst('Key', '').replaceFirst('Digit', '')));
    } else {
      return KeyEventResult.ignored;
    }
    setState(() => _modifiers = 0);
    return KeyEventResult.handled;
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
        return '$path · ${s.videoMode ? '영상' : 'JPEG'} · ${fps == 0 ? '변화 없음' : '${fps}fps'}';
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
            if (displays.length > 1)
              PopupMenuButton<int>(
                tooltip: '화면 고르기',
                icon: const Icon(Icons.monitor_outlined),
                onSelected: (display) {
                  _release();
                  _modifiers = 0;
                  _zoom.value = Matrix4.identity();
                  _session.pickDisplay(display);
                },
                itemBuilder: (_) => [
                  for (final (i, d) in displays.indexed)
                    CheckedPopupMenuItem(
                      value: d.id,
                      checked: d.id == _session.display,
                      child: Text(
                        '${d.main ? '주 화면' : '화면 ${i + 1}'} · ${d.width}×${d.height}',
                      ),
                    ),
                ],
              ),
            PopupMenuButton<String>(
              tooltip: '더 보기',
              onSelected: (value) {
                if (value == 'retry') {
                  _release();
                  _session.inputDenied = null;
                  _session.connect();
                } else {
                  setState(() {
                    _feedback = !_feedback;
                    _notice = '';
                  });
                }
              },
              itemBuilder: (_) => [
                const PopupMenuItem(value: 'retry', child: Text('다시 연결')),
                CheckedPopupMenuItem(
                  value: 'feedback',
                  checked: _feedback,
                  child: const Text('입력 표시'),
                ),
              ],
            ),
          ],
        ),
        body: Focus(
          focusNode: _keys,
          onKeyEvent: _hardware,
          child: Column(
            children: [
              Expanded(child: _body(theme)),
              if (_session.hasPicture) _bottom(theme),
            ],
          ),
        ),
      );
    },
  );

  Widget _bottom(ThemeData theme) => Material(
    color: theme.colorScheme.surfaceContainer,
    child: SafeArea(
      top: false,
      child: Column(
        mainAxisSize: MainAxisSize.min,
        children: [
          Padding(
            padding: const EdgeInsets.fromLTRB(Look.pagePad, 8, Look.pagePad, 0),
            child: SizedBox(
              width: double.infinity,
              child: SegmentedButton<MirrorInput>(
                showSelectedIcon: false,
                segments: const [
                  ButtonSegment(
                    value: MirrorInput.view,
                    label: Text('보기'),
                    icon: Icon(Icons.visibility_outlined),
                  ),
                  ButtonSegment(
                    value: MirrorInput.touch,
                    label: Text('터치'),
                    icon: Icon(Icons.touch_app_outlined),
                  ),
                  ButtonSegment(
                    value: MirrorInput.trackpad,
                    label: Text('트랙패드'),
                    icon: Icon(Icons.mouse_outlined),
                  ),
                ],
                selected: {_mode},
                onSelectionChanged: (v) => _setMode(v.single),
              ),
            ),
          ),
          if (_mode != MirrorInput.view) _keyRow() else const SizedBox(height: 8),
        ],
      ),
    ),
  );

  Widget _body(ThemeData theme) {
    final size = _session.pictureSize;
    final note = _session.note;
    if (size == null || !_session.hasPicture) {
      final Widget center = _session.state == ScreenState.denied
          ? TwinsNotice(text: _deniedText(_session.denied))
          : const TwinsLoading(label: '화면을 받는 중', size: Look.twinsSmall);
      return ListView(
        padding: const EdgeInsets.all(Look.pagePad),
        children: [
          if (note != null) ErrorBand(text: note),
          Padding(
            padding: const EdgeInsets.only(top: Look.groupGap * 2),
            child: center,
          ),
        ],
      );
    }
    // 영상 방식이면 네이티브가 푼 장을 텍스처로(`docs/screen-stream.md` S2), 아니면 JPEG 를 그림으로.
    final textureId = _session.textureId;
    final Widget picture = _session.videoMode && textureId != null
        ? Texture(textureId: textureId, filterQuality: FilterQuality.medium)
        : RawImage(
            image: _session.image,
            fit: BoxFit.fill,
            filterQuality: FilterQuality.medium,
          );
    final stage = Center(
      child: AspectRatio(aspectRatio: size.width / size.height, child: picture),
    );
    final inputDenied = _session.inputDenied;
    final scheme = theme.colorScheme;
    return LayoutBuilder(
      builder: (context, c) {
        final area = c.biggest;
        final r = _pictureRect(area, size);
        return Stack(
          fit: StackFit.expand,
          children: [
            if (_mode == MirrorInput.view)
              GestureDetector(
                onDoubleTapDown: (d) => _tapAt = d.localPosition,
                onDoubleTap: _toggleZoom,
                child: InteractiveViewer(
                  transformationController: _zoom,
                  maxScale: 8,
                  child: stage,
                ),
              )
            else
              // 보기에서 키운 배율·자리를 그대로 쥔다. 트랙패드 손짓은 그림 밖 여백에서 시작해도 받는다.
              Listener(
                behavior: HitTestBehavior.opaque,
                onPointerDown: (e) => _down(e, r),
                onPointerMove: (e) => _move(e, r),
                onPointerUp: _up,
                onPointerCancel: _cancel,
                onPointerHover: (e) => _hover(e, r),
                onPointerSignal: _signal,
                child: DecoratedBox(
                  position: DecorationPosition.foreground,
                  decoration: BoxDecoration(
                    border: Border.all(color: scheme.primary, width: 2),
                  ),
                  child: ClipRect(
                    child: Transform(transform: _zoom.value, child: stage),
                  ),
                ),
              ),
            if (_mode == MirrorInput.trackpad)
              Positioned(
                left: (r.left + _cursor.dx * r.width - 8).clamp(0.0, area.width - 16),
                top: (r.top + _cursor.dy * r.height - 8).clamp(0.0, area.height - 16),
                child: IgnorePointer(
                  child: Icon(Icons.adjust, size: 16, color: scheme.tertiary),
                ),
              ),
            if (_notice.isNotEmpty && _feedback && _mode != MirrorInput.view)
              Positioned(
                left: Look.pagePad,
                bottom: Look.cardGap,
                child: IgnorePointer(
                  child: DecoratedBox(
                    decoration: BoxDecoration(
                      color: scheme.inverseSurface.withValues(alpha: 0.9),
                      borderRadius: Look.corners,
                    ),
                    child: Padding(
                      padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 7),
                      child: Text(
                        _notice,
                        style: TextStyle(fontSize: Look.sub, color: scheme.onInverseSurface),
                      ),
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
              )
            else if (_mode != MirrorInput.view && inputDenied != null)
              Positioned(
                top: 0,
                left: Look.pagePad,
                right: Look.pagePad,
                child: ErrorBand(text: _inputDeniedText(inputDenied)),
              ),
          ],
        );
      },
    );
  }

  /// 두 번 탭 — 키운 상태면 전체로, 아니면 그 자리를 세 배로. 데스크톱 글자는 전체 보기에서 안 읽힌다.
  void _toggleZoom() {
    if (_zoom.value.getMaxScaleOnAxis() > 1.01) {
      _zoom.value = Matrix4.identity();
      return;
    }
    const scale = 3.0;
    _zoom.value = Matrix4.diagonal3Values(
      scale,
      scale,
      1,
    )..setTranslationRaw(-_tapAt.dx * (scale - 1), -_tapAt.dy * (scale - 1), 0);
  }

  Widget _keyRow() {
    final live = _canInput;
    KeyChip key(String label, String code, {IconData? icon}) => KeyChip(
      label: icon == null ? label : null,
      icon: icon,
      onTap: live ? () => _key(code, label) : () {},
    );
    return KeyRow(
      keys: [
        KeyChip(icon: Icons.keyboard_alt_outlined, onTap: live ? _typeSheet : () {}),
        KeyChip(
          label: '드래그',
          selected: _dragging,
          onTap: () {
            _release();
            setState(() => _dragging = !_dragging);
          },
        ),
        for (final (label, bit) in [('ctrl', 2), ('alt', 4), ('shift', 1), ('⌘ / win', 8)])
          KeyChip(
            label: label,
            selected: _modifiers & bit != 0,
            onTap: () => setState(() => _modifiers ^= bit),
          ),
        key('esc', 'Escape'),
        key('tab', 'Tab'),
        key('enter', 'Enter', icon: Icons.keyboard_return),
        key('⌫', 'Backspace', icon: Icons.backspace_outlined),
        key('←', 'ArrowLeft', icon: Icons.keyboard_arrow_left),
        key('↓', 'ArrowDown', icon: Icons.keyboard_arrow_down),
        key('↑', 'ArrowUp', icon: Icons.keyboard_arrow_up),
        key('→', 'ArrowRight', icon: Icons.keyboard_arrow_right),
        for (final k in 'ACVXZSFWRTEQYUIOPDGHJKLBNM'.split('')) key(k.toLowerCase(), 'Key$k'),
        for (final k in '1234567890'.split('')) key(k, 'Digit$k'),
        for (final (code, label) in [
          ('Space', 'space'),
          ('Backquote', '`'),
          ('Minus', '-'),
          ('Equal', '='),
          ('BracketLeft', '['),
          ('BracketRight', ']'),
          ('Backslash', '\\'),
          ('Semicolon', ';'),
          ('Quote', "'"),
          ('Comma', ','),
          ('Period', '.'),
          ('Slash', '/'),
        ])
          key(label, code),
        for (final k in ['Home', 'End', 'PageUp', 'PageDown', 'Delete']) key(k, k),
        for (var i = 1; i <= 12; i++) key('F$i', 'F$i'),
      ],
    );
  }
}

/// 그 기기에 넣을 글 — 폰 자판이 조합을 끝낸 글을 통째로 보낸다(한글을 키로 풀면 그 기기 입력기가 다시 조합해 어긋난다).
class _TypeSheet extends StatefulWidget {
  const _TypeSheet();

  @override
  State<_TypeSheet> createState() => _TypeSheetState();
}

class _TypeSheetState extends State<_TypeSheet> {
  final TextEditingController _text = TextEditingController();
  final FocusNode _focus = FocusNode();

  @override
  void dispose() {
    _text.dispose();
    _focus.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) => Padding(
    padding: EdgeInsets.only(bottom: MediaQuery.viewInsetsOf(context).bottom),
    child: ChatComposer(
      controller: _text,
      focusNode: _focus,
      enabled: true,
      autofocus: true,
      hint: '그 기기에 글 넣기',
      onSend: () => Navigator.pop(context, _text.text),
    ),
  );
}
