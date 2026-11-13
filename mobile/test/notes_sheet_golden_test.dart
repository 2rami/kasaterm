import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:kasaterm_mobile/hub_model.dart';
import 'package:kasaterm_mobile/screens/notes_sheet.dart';
import 'package:kasaterm_mobile/server.dart';

/// 종 목록의 생김새 — 끝냄·승인·막힘 쪽지(안 읽은 것만 선다).
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
        kind: 'done',
        summary: '폰 상태줄 두 줄을 폴더 이름 빼서 한 줄로 맞추고 커밋했어요.',
        when: now.subtract(const Duration(minutes: 4)),
        read: false,
      ),
      Note(
        id: 2,
        pane: '%7',
        character: '코하루',
        kind: 'permission',
        summary: '선생님, 미니맵 탭 줄 굽게 Bash 허락해 주시겠어요?',
        when: now.subtract(const Duration(hours: 2)),
        read: true,
      ),
      Note(
        id: 1,
        pane: '%9',
        character: '세이아',
        kind: 'blocked',
        summary: '선생님, 막혔어요. 랙 미니 재시작이 실패해서 로그만 남겼어요.',
        when: now.subtract(const Duration(days: 1)),
        read: true,
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
