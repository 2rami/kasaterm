import 'package:video_player/video_player.dart';

import 'inline_media.dart';
import 'server.dart';

/// 웹 판은 받아 둘 곳도, AVPlayer 가 읽을 루프백 창구도 없다.
const mediaPlayable = false;

Future<MediaStore?> openMediaStore() async => null;

VideoPlayerController mediaController(Uri uri) =>
    throw UnsupportedError('이 판은 파일을 재생하지 못해요');

Future<MediaLink> openMediaBridge({
  required Server server,
  required String id,
  required String? machine,
  required InlineMedia media,
  required int chunk,
}) => throw UnsupportedError('이 판은 미디어 창구를 열지 못해요');
