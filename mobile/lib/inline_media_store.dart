// 앱(iOS)은 받은 소리·영상을 캐시 폴더에 두고 그 파일을 재생한다. 웹 판에는 둘 곳이 없다 — lib 은
// dart:io 를 직접 들이지 않는다(README 「지킬 것」).
export 'inline_media_store_stub.dart'
    if (dart.library.io) 'inline_media_store_io.dart';
