import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';

Future<void> loadShareFonts() async {
  TestWidgetsFlutterBinding.ensureInitialized();
  final text = FontLoader('Pretendard')
    ..addFont(rootBundle.load('assets/fonts/Pretendard-Regular.otf'))
    ..addFont(rootBundle.load('assets/fonts/Pretendard-SemiBold.otf'));
  final icons = FontLoader('MaterialIcons')
    ..addFont(rootBundle.load('fonts/MaterialIcons-Regular.otf'));
  await Future.wait([text.load(), icons.load()]);
}
