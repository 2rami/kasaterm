import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:kasaterm_mobile/live_input.dart';

TextEditingValue v(String text, [int? cs, int? ce]) => TextEditingValue(
  text: text,
  composing: cs == null ? TextRange.empty : TextRange(start: cs, end: ce!),
);

void main() {
  test('덧붙인 글자만 보낸다', () {
    expect(liveDiff('ab', 'abc'), [0x63]);
  });

  test('지운 만큼 지우기(DEL)를 보낸다 — 한글 한 자가 하나', () {
    expect(liveDiff('가나', '가'), [0x7f]);
    expect(liveDiff('가나', ''), [0x7f, 0x7f]);
  });

  test('가운데가 바뀌면 같은 앞부분까지 물리고 다시 친다 — 자동 고침', () {
    expect(liveDiff('teh ', 'the '), [0x7f, 0x7f, 0x7f, ...'he '.codeUnits]);
  });

  test('조합 중인 한글은 보내지 않고 composing 으로 남긴다', () {
    final li = LiveInput();
    expect(li.update(v('ㄱ', 0, 1)), isEmpty);
    expect(li.composing, 'ㄱ');
    expect(li.update(v('가', 0, 1)), isEmpty);
    expect(li.composing, '가');
    // 다음 글자로 넘어가며 「가」가 확정된다.
    expect(li.update(v('가ㄴ', 1, 2)), [0xea, 0xb0, 0x80]);
    expect(li.composing, 'ㄴ');
  });

  test('엔터 직전 flush 는 조합 중인 것까지 보낸다', () {
    final li = LiveInput();
    li.update(v('가ㄴ', 1, 2));
    expect(li.flush(v('가나')), [0xeb, 0x82, 0x98]);
    expect(li.composing, '');
  });

  test('reset 뒤 빈 입력값은 지우기를 보내지 않는다', () {
    final li = LiveInput();
    li.update(v('abc'));
    li.reset();
    expect(li.update(v('')), isEmpty);
  });
}
