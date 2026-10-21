// 앱(iOS)은 원본 기기에서 필요한 구간만 루프백 창구로 받아 재생하고, 전 판이 받아 둔 완성본은 캐시 폴더에서
// 다시 쓴다. 웹 판에는 둘 다 없다 — lib 은 dart:io 를 직접 들이지 않는다(README 「지킬 것」).
export 'inline_media_store_stub.dart'
    if (dart.library.io) 'inline_media_store_io.dart';
