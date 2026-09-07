import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:kasaterm_mobile/hub_model.dart';
import 'package:kasaterm_mobile/screens/notes_sheet.dart';
import 'package:kasaterm_mobile/server.dart';

/// 종 목록의 생김새 — 안 읽은 쪽지·읽은 쪽지·사진 딸린 쪽지·닫힌 pane 쪽지.
void main() {
  testWidgets('학생 쪽지 목록', (tester) async {
    tester.view.physicalSize = const Size(390, 640);
    tester.view.devicePixelRatio = 1;
    addTearDown(tester.view.reset);
    final server = Server(Uri.parse('http://127.0.0.1:1/'));
    final model = HubModel(server);
    final now = DateTime.now();
    model.notes = [
      Note(
        id: 3,
        pane: '%3',
        character: '유우카',
        kind: 'done_ok',
        summary: '폰 상태줄 두 줄 → 폴더 이름 빼서 한 줄로 맞추고 커밋',
        asked: '상태줄이랑 밑에 여러 줄 안 되게 하자',
        did: '폴더부터 빼고 괄호 힌트 떼서 한 줄',
        when: now.subtract(const Duration(minutes: 4)),
        read: false,
        image: false,
      ),
      Note(
        id: 2,
        pane: '%7',
        character: '코하루',
        kind: 'permission',
        summary: 'Bash 허락 기다림 · 미니맵 탭 줄 굽는 중',
        asked: '',
        did: '',
        when: now.subtract(const Duration(hours: 2)),
        read: true,
        image: false,
      ),
      Note(
        id: 1,
        pane: '%9',
        character: '세이아',
        kind: 'done_fail',
        summary: '나쵸 배포 → 랙 미니 재시작 실패, 로그만 남김',
        asked: '',
        did: '',
        when: now.subtract(const Duration(days: 1)),
        read: true,
        image: false,
      ),
    ];
    await tester.pumpWidget(
      MaterialApp(
        theme: ThemeData(colorSchemeSeed: const Color(0xff4c6ef5)),
        home: Scaffold(
          body: NotesSheet(model: model, server: server, onOpen: (_) {}),
        ),
      ),
    );
    await tester.pump(const Duration(milliseconds: 600));
    await expectLater(
      find.byType(NotesSheet),
      matchesGoldenFile('goldens/notes_sheet.png'),
    );
  });
}
