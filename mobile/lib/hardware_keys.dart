import 'dart:convert';

import 'package:flutter/services.dart';

/// 하드웨어 키보드(아이패드 매직 키보드·블루투스 키보드)의 키 하나를 pane 에 보낼 것.
/// 글자는 입력칸이 받아 바로 치기로 흘려보내므로 여기서 다루지 않는다 — 입력칸이 못
/// 받는 키(Esc·Ctrl·방향·Tab)만 옮긴다. 소프트 키 줄(`_KeyBar`)과 같은 바이트다.
typedef HardwareKey = ({List<int> bytes, bool resetField});

/// null 이면 입력칸이 그대로 받는다.
///
/// [fieldEmpty] — 방향키·지우기는 입력칸이 비었을 때만 pane 으로 간다. 글이 있으면 그
/// 글을 고치는 키다(바로 치기는 입력칸과 pane 입력상자가 같은 글이라는 전제로 차이만
/// 보내서, 거기서 pane 커서를 따로 움직이면 둘이 어긋난다).
/// [resetField] — Ctrl 조합은 pane 쪽 입력상자를 바꾸므로(^C 는 비우고 ^U 는 지운다) 입력칸도
/// 보내지 않고 비워야 다음 글자부터 다시 맞는다.
HardwareKey? hardwareKey(
  LogicalKeyboardKey key,
  PhysicalKeyboardKey physical, {
  required bool ctrl,
  required bool shift,
  required bool alt,
  required bool meta,
  required bool fieldEmpty,
  required bool appCursor,
}) {
  HardwareKey send(String s, {bool reset = false}) =>
      (bytes: utf8.encode(s), resetField: reset);

  // 매직 키보드엔 Esc 가 없다 — 아이패드 앱들이 ⌘. 을 Esc 로 쓴다.
  if (meta) return key == LogicalKeyboardKey.period ? send('\x1b') : null;
  if (ctrl && !alt) {
    final byte = _ctrlByte(key, physical);
    if (byte != null) return (bytes: [byte], resetField: !fieldEmpty);
  }
  if (key == LogicalKeyboardKey.escape) return send('\x1b');
  // Shift+Tab 은 claude 의 모드 순환이다 — 입력칸의 포커스 이동으로 새면 안 된다.
  if (key == LogicalKeyboardKey.tab) return send(shift ? '\x1b[Z' : '\t');
  if (key == LogicalKeyboardKey.pageUp) return send('\x1b[5~');
  if (key == LogicalKeyboardKey.pageDown) return send('\x1b[6~');
  if (!fieldEmpty) return null;
  final arrow = _arrows[key];
  if (arrow != null) return send('${appCursor ? '\x1bO' : '\x1b['}$arrow');
  if (key == LogicalKeyboardKey.backspace) return send('\x7f');
  if (key == LogicalKeyboardKey.delete) return send('\x1b[3~');
  if (key == LogicalKeyboardKey.home) return send('\x1b[H');
  if (key == LogicalKeyboardKey.end) return send('\x1b[F');
  return null;
}

final _arrows = {
  LogicalKeyboardKey.arrowUp: 'A',
  LogicalKeyboardKey.arrowDown: 'B',
  LogicalKeyboardKey.arrowRight: 'C',
  LogicalKeyboardKey.arrowLeft: 'D',
};

/// Ctrl+글자 → C0 제어 문자. ^[ 는 Esc, ^\ ^] 는 셸·telnet 이 쓴다.
/// 한글 자판이면 논리 키가 「ㅊ」로 올 수 있어, 글자가 아니면 자리(물리 키)로 읽는다.
int? _ctrlByte(LogicalKeyboardKey key, PhysicalKeyboardKey physical) {
  final id = key.keyId;
  if (id >= LogicalKeyboardKey.keyA.keyId && id <= LogicalKeyboardKey.keyZ.keyId) {
    return id - LogicalKeyboardKey.keyA.keyId + 1;
  }
  if (key == LogicalKeyboardKey.bracketLeft) return 0x1b;
  if (key == LogicalKeyboardKey.backslash) return 0x1c;
  if (key == LogicalKeyboardKey.bracketRight) return 0x1d;
  final usb = physical.usbHidUsage;
  if (usb >= PhysicalKeyboardKey.keyA.usbHidUsage &&
      usb <= PhysicalKeyboardKey.keyZ.usbHidUsage) {
    return usb - PhysicalKeyboardKey.keyA.usbHidUsage + 1;
  }
  return null;
}
