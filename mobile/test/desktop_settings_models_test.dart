import 'package:flutter_test/flutter_test.dart';
import 'package:kasaterm_mobile/screens/desktop_settings.dart';

void main() {
  test('Claude 모델 선택지는 데스크톱이 보낸 목록을 따른다', () {
    final choices = claudeModelChoices([
      {'value': '', 'label': '기본'},
      {'value': 'opus', 'label': 'Opus 5.5'},
      {'value': 'claude-fable-5-2', 'label': 'Fable 5.2'},
      {'value': 7, 'label': '깨진 항목'},
    ]);
    expect(choices, [
      ('', '기본'),
      ('opus', 'Opus 5.5'),
      ('claude-fable-5-2', 'Fable 5.2'),
    ]);
  });

  test('목록을 안 보내는 옛 데스크톱 판에는 예전 넷을 쓴다', () {
    expect(claudeModelChoices(null).map((c) => c.$1), ['', 'opus', 'sonnet', 'haiku']);
    expect(claudeModelChoices(const []).length, 4);
  });
}
