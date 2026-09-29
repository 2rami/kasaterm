import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:kasaterm_mobile/hardware_keys.dart';

HardwareKey? key(
  LogicalKeyboardKey k, {
  PhysicalKeyboardKey physical = PhysicalKeyboardKey.fn,
  bool ctrl = false,
  bool shift = false,
  bool meta = false,
  bool empty = true,
  bool appCursor = false,
}) => hardwareKey(
  k,
  physical,
  ctrl: ctrl,
  shift: shift,
  alt: false,
  meta: meta,
  fieldEmpty: empty,
  appCursor: appCursor,
);

void main() {
  test('Esc·⌘.·Tab·Shift+Tab 은 늘 pane 으로', () {
    expect(key(LogicalKeyboardKey.escape)?.bytes, [0x1b]);
    expect(key(LogicalKeyboardKey.period, meta: true)?.bytes, [0x1b]);
    expect(key(LogicalKeyboardKey.tab, empty: false)?.bytes, [0x09]);
    expect(key(LogicalKeyboardKey.tab, shift: true)?.bytes, '\x1b[Z'.codeUnits);
  });

  test('⌘ 조합은 입력칸(붙여넣기 등)이 받는다', () {
    expect(key(LogicalKeyboardKey.keyV, meta: true), isNull);
  });

  test('Ctrl+글자는 제어 문자, 칸에 글이 있으면 칸을 비운다', () {
    final c = key(LogicalKeyboardKey.keyC, ctrl: true, empty: false)!;
    expect(c.bytes, [0x03]);
    expect(c.resetField, isTrue);
    expect(key(LogicalKeyboardKey.keyC, ctrl: true)!.resetField, isFalse);
    expect(
      key(LogicalKeyboardKey.bracketLeft, physical: PhysicalKeyboardKey.bracketLeft, ctrl: true)?.bytes,
      [0x1b],
    );
  });

  test('한글 자판의 Ctrl+ㅊ 는 자리(물리 c)로 ^C', () {
    const hangul = LogicalKeyboardKey(0x314a); // ㅊ
    expect(
      key(hangul, physical: PhysicalKeyboardKey.keyC, ctrl: true)?.bytes,
      [0x03],
    );
  });

  test('방향·지우기는 칸이 비었을 때만 pane 으로, DECCKM 이면 SS3', () {
    expect(key(LogicalKeyboardKey.arrowUp)?.bytes, '\x1b[A'.codeUnits);
    expect(key(LogicalKeyboardKey.arrowLeft, appCursor: true)?.bytes, '\x1bOD'.codeUnits);
    expect(key(LogicalKeyboardKey.backspace)?.bytes, [0x7f]);
    expect(key(LogicalKeyboardKey.arrowUp, empty: false), isNull);
    expect(key(LogicalKeyboardKey.backspace, empty: false), isNull);
  });

  test('글자·엔터는 입력칸이 받는다', () {
    expect(key(LogicalKeyboardKey.keyA), isNull);
    expect(key(LogicalKeyboardKey.enter), isNull);
  });
}
