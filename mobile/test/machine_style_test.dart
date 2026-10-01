import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:kasaterm_mobile/server.dart';
import 'package:kasaterm_mobile/machine_look.dart';
import 'package:kasaterm_mobile/status_style.dart';

void main() {
  test('기기색은 기준 기기가 칠하는 색 그대로 — 밝게·어둡게와 상관없다', () {
    final looks = MachineLooks.parse(
      appearance: {
        'device_colors': [
          {'label': 'MacBook\u00a0Pro', 'local': true, 'hex': '#e65a96'},
          {'label': '맥미니', 'local': false, 'hex': '#1aac9c'},
        ],
      },
    );
    expect(looks.color('맥미니'), const Color(0xff1aac9c));
    // 맥 컴퓨터 이름의 NBSP 와 보통 공백은 한 기기다(데스크톱 normalize_device).
    expect(looks.color('macbook pro'), const Color(0xffe65a96));
    // 폰이 「이 기계」라 불러도 기준 기기는 local 줄의 색.
    expect(looks.color('이 기계', local: true), const Color(0xffe65a96));
  });

  test('표에 없는 이름은 데스크톱 hashed_device_color 와 같은 색', () {
    const looks = MachineLooks();
    // FNV-1a("vm") % 5 = 0 → blue, FNV-1a("맥북") % 5 = 1 → violet. 데스크톱 프리셋 순서 그대로.
    expect(looks.color('VM'), MachineLooks.defaultPresets[0]);
    expect(looks.color('맥북'), MachineLooks.defaultPresets[1]);
  });

  test('기기 아이콘 — 계정 설정이 먼저, 없으면 데스크톱 automatic 규칙', () {
    final looks = MachineLooks.parse(deviceIcons: {'맥미니': 'monitor', 'x': 'bogus'});
    expect(looks.icon('맥미니'), Icons.desktop_windows_outlined);
    expect(looks.icon('맥북'), Icons.laptop_outlined);
    expect(looks.icon('MacBook Pro'), Icons.laptop_outlined);
    expect(looks.icon('rack-mini'), Icons.dns_outlined);
    expect(looks.icon('아이폰'), Icons.smartphone_outlined);
    expect(looks.icon('x'), Icons.desktop_windows_outlined);
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

  test('목록 줄 글은 세션 이름과 도는 시간뿐 — 학생 이름은 안 쓴다', () {
    Pane p(Map<String, Object?> extra) => Pane.fromJson({'id': '%1', 'cwd': '/w/kasaterm', ...extra});
    expect(p({'name': '아리스', 'session': '폰 목록 줄', 'busy_secs': 840}).rowTitle, '폰 목록 줄');
    expect(p({'name': '아리스', 'session': '폰 목록 줄', 'busy_secs': 840}).busySecs, 840);
    expect(p({'name': '아리스', 'harness': 'codex'}).rowTitle, 'Codex');
    expect(p({'name': '아리스'}).rowTitle, 'Claude');
    expect(p({}).rowTitle, 'kasaterm', reason: '셸은 폴더');
    // 데스크톱 배치도 칸과 같은 말 — 1분 미만 없음, 두 시간까지 분, 그 뒤 시간(내림).
    expect(elapsedLabel(59), isNull);
    expect(elapsedLabel(60), '1분');
    expect(elapsedLabel(7199), '119분');
    expect(elapsedLabel(7200), '2시간');
    expect(elapsedLabel(null), isNull);
  });

  test('거울 pane 은 어느 기계의 거울인지 안다', () {
    expect(Pane.fromJson({'id': '%9', 'mirror_of': '나쵸네코'}).mirrorOf, '나쵸네코');
    expect(Pane.fromJson({'id': '%9'}).mirrorOf, isNull);
  });
}
