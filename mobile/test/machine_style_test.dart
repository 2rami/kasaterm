import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:kasaterm_mobile/server.dart';
import 'package:kasaterm_mobile/status_style.dart';

void main() {
  test('기계색은 이름으로 정해지고 이름이 다르면 갈린다', () {
    const scheme = ColorScheme.dark();
    expect(machineColor('맥북', scheme), machineColor('맥북', scheme));
    expect(machineColor('맥북', scheme), isNot(machineColor('맥미니', scheme)));
  });

  test('기계 모양 아이콘 — 노트북·데스크톱·그 밖', () {
    expect(machineIcon('맥북'), Icons.laptop_mac);
    expect(machineIcon('MacBook Pro'), Icons.laptop_mac);
    expect(machineIcon('맥미니'), Icons.desktop_mac);
    expect(machineIcon('rack-mini'), Icons.desktop_mac);
    expect(machineIcon('vm'), Icons.computer_outlined);
  });

  test('목록 둘째 줄엔 요약 제목이 안 오고, 셸만 폴더 이름', () {
    Pane p(String name, String title) => Pane(
      id: '%1',
      name: name,
      title: title,
      status: 'idle',
      window: 0,
      cwd: '/w/kasaterm',
    );
    expect(p('아리스', '폰 미니맵 화면').subtitle, '');
    expect(p('', 'zsh').subtitle, 'kasaterm');
  });
}
