import 'dart:convert';

import 'package:crypto/crypto.dart';
import 'package:flutter/services.dart';

/// 비밀 요청 서명 대상 머리 — 관문·맥(`approval_text::SECRET_PREFIX`)과 글자 하나까지 같아야 한다.
const secretPrefix = 'kasaterm-secret/1\n';

/// 공개키(X9.63, 65바이트)의 id — 관문·맥 `key_id` 와 같은 값(SHA-256 앞 16바이트를 hex 로).
String approvalKeyId(List<int> public) =>
    sha256.convert(public).bytes.take(16).map((b) => b.toRadixString(16).padLeft(2, '0')).join();

/// 사람이 폰과 맥 두 화면에서 맞춰 보는 지문 — 키 id 앞 12자를 넷씩.
String approvalFingerprint(String id) {
  final head = id.toUpperCase().padRight(12).substring(0, 12);
  return [head.substring(0, 4), head.substring(4, 8), head.substring(8, 12)].join('-');
}

/// 승인 열쇠 — 개인 키는 폰(Secure Enclave) 밖으로 안 나가고 Face ID 가 풀어야 서명한다
/// (docs/op-faceid-approval.md). 시험은 가짜를 꽂는다.
abstract class ApprovalSigner {
  /// 있는 열쇠의 공개키(base64 X9.63). 없으면 null.
  Future<String?> publicKey();

  /// 새 열쇠를 만든다(있던 것은 지운다). 공개키를 준다.
  Future<String> create();

  Future<void> delete();

  /// [challenge] 를 머리와 함께 서명한다(Face ID). 사람이 취소하면 null — 그때는 아무것도 보내지 않는다.
  Future<String?> sign(String challenge, String reason);
}

class SecureEnclaveSigner implements ApprovalSigner {
  const SecureEnclaveSigner();

  static const _ch = MethodChannel('kasaterm/secure_key');

  @override
  Future<String?> publicKey() async {
    try {
      return await _ch.invokeMethod<String>('publicKey');
    } on MissingPluginException {
      return null;
    }
  }

  @override
  Future<String> create() async {
    final public = await _ch.invokeMethod<String>('create');
    if (public == null) throw PlatformException(code: 'create_failed');
    return public;
  }

  @override
  Future<void> delete() => _ch.invokeMethod<void>('delete');

  @override
  Future<String?> sign(String challenge, String reason) => _ch.invokeMethod<String>('sign', {
    'message': base64.encode(utf8.encode('$secretPrefix$challenge')),
    'reason': reason,
  });
}
