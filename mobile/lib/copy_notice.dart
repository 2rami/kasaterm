import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

/// 복사했다고 알린다 — 고르기 메뉴의 「복사」·코드 칸 단추·인라인 코드·말풍선 복사가 같은 손맛을 쓴다.
void tellCopied(ScaffoldMessengerState? messenger, {String done = '복사했어요'}) {
  HapticFeedback.lightImpact();
  if (messenger == null || !messenger.mounted) return;
  messenger
    ..hideCurrentSnackBar()
    ..showSnackBar(SnackBar(content: Text(done)));
}

/// [text] 를 담고 알린다. 알림 자리는 담기 전에 잡는다 — 메뉴·시트처럼 누르자마자 닫히는 곳에서도 뜨게.
Future<void> copyAndTell(BuildContext context, String text, {String done = '복사했어요'}) async {
  final messenger = ScaffoldMessenger.maybeOf(context);
  await Clipboard.setData(ClipboardData(text: text));
  tellCopied(messenger, done: done);
}
