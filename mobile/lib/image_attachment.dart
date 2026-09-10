import 'dart:typed_data';
import 'dart:ui' as ui;
import 'dart:math' as math;

import 'package:image_picker/image_picker.dart';

import 'server.dart';

/// Only the explicitly selected photo is read. No library enumeration, camera,
/// or location/full metadata permission is needed.
Future<Uint8List?> pickAttachmentImage() async {
  final photo = await ImagePicker().pickImage(
    source: ImageSource.gallery,
    maxWidth: 4096,
    maxHeight: 4096,
    imageQuality: 95,
    requestFullMetadata: false,
  );
  if (photo == null) return null;
  if (await photo.length() > Server.maxImageBytes) {
    throw const ServerException('사진이 너무 커요. 32 MB 이하의 사진을 선택해 주세요.');
  }
  return normalizeAttachmentImage(await photo.readAsBytes());
}

/// Decode real pixels before upload: changing a HEIC MIME label does not make
/// it a JPEG. Platform decoding honors orientation; PNG output strips metadata
/// and is accepted by the source host's image decoder for both Claude/Codex.
Future<Uint8List> normalizeAttachmentImage(Uint8List bytes) async {
  if (bytes.isEmpty || bytes.length > Server.maxImageBytes) {
    throw const ServerException('비어 있거나 너무 큰 사진이에요. 다른 사진을 선택해 주세요.');
  }
  final ui.Codec codec;
  try {
    codec = await ui.instantiateImageCodecWithSize(
      await ui.ImmutableBuffer.fromUint8List(bytes),
      getTargetSize: (width, height) {
        final scale = math.min(1.0, 4096 / math.max(width, height));
        return ui.TargetImageSize(
          width: math.max(1, (width * scale).round()),
          height: math.max(1, (height * scale).round()),
        );
      },
    );
  } catch (_) {
    throw const ServerException('이 사진을 읽지 못했어요. JPEG나 PNG 사진으로 다시 선택해 주세요.');
  }
  try {
    final frame = await codec.getNextFrame();
    try {
      final data = await frame.image.toByteData(format: ui.ImageByteFormat.png);
      if (data == null || data.lengthInBytes > Server.maxImageBytes) {
        throw const ServerException('변환한 사진이 너무 커요. 더 작은 사진을 선택해 주세요.');
      }
      return data.buffer.asUint8List(data.offsetInBytes, data.lengthInBytes);
    } finally {
      frame.image.dispose();
    }
  } finally {
    codec.dispose();
  }
}
