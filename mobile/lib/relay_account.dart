import 'dart:convert';

import 'package:http/http.dart' as http;

const defaultGateway = 'https://kasaterm.debimarlene.com';

Uri? parseGateway(String value) {
  final uri = Uri.tryParse(value.trim());
  if (uri == null ||
      uri.host.isEmpty ||
      uri.userInfo.isNotEmpty ||
      uri.hasQuery ||
      uri.hasFragment ||
      (uri.path.isNotEmpty && uri.path != '/')) {
    return null;
  }
  final loopback = const ['localhost', '127.0.0.1', '::1'].contains(uri.host);
  if (uri.scheme != 'https' && !(uri.scheme == 'http' && loopback)) return null;
  return uri.replace(path: '');
}

bool sameOrigin(Uri a, Uri b) =>
    a.scheme == b.scheme && a.host == b.host && a.port == b.port;

class AccountSession {
  AccountSession({
    required this.origin,
    required this.account,
    required this.deviceId,
    required this.token,
  }) {
    if (parseGateway(origin.toString()) == null ||
        account.isEmpty ||
        deviceId.isEmpty ||
        !RegExp(r'^[A-Za-z0-9._~-]+$').hasMatch(token)) {
      throw const AccountException('로그인 응답을 확인하지 못했어요.');
    }
  }

  final Uri origin;
  final String account;
  final String deviceId;
  final String token;
  Uri get root => origin.resolve('/relay/account/');
  List<String> get protocols => ['kasa-relay-account', 'kasa-auth.$token'];

  Map<String, Object> toJson() => {
    'kind': 'account',
    'origin': origin.toString(),
    'account': account,
    'device_id': deviceId,
    'token': token,
  };

  factory AccountSession.fromJson(Map<String, dynamic> json) => AccountSession(
    origin: Uri.parse(json['origin'] as String),
    account: json['account'] as String,
    deviceId: json['device_id'] as String,
    token: json['token'] as String,
  );
}

class AccountException implements Exception {
  const AccountException(this.message, {this.status});
  final String message;
  final int? status;
  @override
  String toString() => message;
}

String accountError(int status) => switch (status) {
  401 => '아이디나 비밀번호를 확인해 주세요. 저장된 로그인은 만료되었을 수 있어요.',
  404 => '이 서버는 계정 로그인을 지원하지 않아요. 서버 업데이트가 필요해요.',
  429 => '로그인 시도가 많아요. 잠시 뒤 다시 시도해 주세요.',
  503 => '계정은 로그인되어 있어요. 연결할 데스크톱을 기다리고 있어요.',
  _ => '서버가 요청을 처리하지 못했어요 ($status).',
};

class AccountSyncSnapshot {
  const AccountSyncSnapshot(this.revision, this.settings);
  final int revision;
  final Map<String, dynamic> settings;
  factory AccountSyncSnapshot.fromJson(Map<String, dynamic> json) {
    final revision = json['revision'];
    final settings = json['settings'];
    if (revision is! int || revision < 0 || settings is! Map<String, dynamic>) {
      throw const AccountException('계정 설정 응답을 확인하지 못했어요.');
    }
    return AccountSyncSnapshot(revision, settings);
  }
}

class AccountSyncConflict implements Exception {
  const AccountSyncConflict(this.current);
  final AccountSyncSnapshot current;
}

/// A credential belongs to one origin; redirects must never choose its recipient.
class OriginClient extends http.BaseClient {
  OriginClient(
    this.origin, {
    http.Client? client,
    this.token,
    this.onUnauthorized,
  }) : _inner = client ?? http.Client();
  final Uri origin;
  final String? token;
  final void Function()? onUnauthorized;
  final http.Client _inner;
  bool _closed = false;

  @override
  Future<http.StreamedResponse> send(http.BaseRequest request) async {
    if (_closed ||
        !sameOrigin(origin, request.url) ||
        request.url.userInfo.isNotEmpty) {
      throw const AccountException('연결 대상이 바뀌었어요. 다시 연결해 주세요.');
    }
    request.followRedirects = false;
    if (token != null) request.headers['authorization'] = 'Bearer $token';
    final response = await _inner.send(request);
    final bytes = await response.stream.toBytes();
    if (_closed) throw const AccountException('이전 연결이 종료되었어요.');
    if (response.statusCode >= 300 && response.statusCode < 400) {
      throw const AccountException('다른 주소로의 이동을 차단했어요. 서버 주소를 확인해 주세요.');
    }
    if (response.statusCode == 401 && token != null) onUnauthorized?.call();
    return http.StreamedResponse(
      Stream.value(bytes),
      response.statusCode,
      headers: response.headers,
      contentLength: bytes.length,
      reasonPhrase: response.reasonPhrase,
      request: request,
    );
  }

  @override
  void close() {
    _closed = true;
    _inner.close();
  }
}

class RelayAccountApi {
  RelayAccountApi(this.origin, {http.Client? client, AccountSession? session})
    : _client = OriginClient(origin, client: client, token: session?.token) {
    if (parseGateway(origin.toString()) == null ||
        (session != null && !sameOrigin(origin, session.origin))) {
      _client.close();
      throw const AccountException('안전한 서버 주소를 입력해 주세요.');
    }
  }
  final Uri origin;
  final http.Client _client;

  Future<Map<String, dynamic>> _request(
    String path, {
    Map<String, String>? body,
  }) async {
    try {
      final uri = origin.resolve('/relay/$path');
      final response =
          await (body == null
                  ? _client.get(uri)
                  : _client.post(
                      uri,
                      headers: {'content-type': 'application/json'},
                      body: jsonEncode(body),
                    ))
              .timeout(const Duration(seconds: 15));
      if (response.statusCode != 200) {
        throw AccountException(
          accountError(response.statusCode),
          status: response.statusCode,
        );
      }
      final json = jsonDecode(utf8.decode(response.bodyBytes));
      if (json is! Map<String, dynamic>) throw const FormatException();
      return json;
    } on AccountException {
      rethrow;
    } on FormatException {
      throw const AccountException('서버 응답을 읽지 못했어요. 서버 버전을 확인해 주세요.');
    } catch (_) {
      throw const AccountException('서버에 연결하지 못했어요. 네트워크와 서버 주소를 확인해 주세요.');
    }
  }

  Future<AccountSession> login(String account, String password) async {
    final json = await _request(
      'login',
      body: {
        'account': account.trim(),
        'password': password,
        'kind': 'phone',
        'label': '카사모바일',
      },
    );
    try {
      return AccountSession(
        origin: origin,
        account: json['account'] as String,
        deviceId: json['device_id'] as String,
        token: json['token'] as String,
      );
    } catch (_) {
      throw const AccountException('로그인 응답을 확인하지 못했어요.');
    }
  }

  Future<void> verify(AccountSession session) async {
    final json = await _request('whoami');
    if (json['ok'] != true ||
        json['account'] != session.account ||
        json['device_id'] != session.deviceId ||
        json['kind'] != 'phone') {
      throw const AccountException(
        '저장된 로그인과 서버 계정이 달라요. 다시 로그인해 주세요.',
        status: 401,
      );
    }
  }

  Future<List<Map<String, dynamic>>> devices() async {
    final json = await _request('devices');
    return (json['devices'] as List? ?? [])
        .whereType<Map<String, dynamic>>()
        .toList();
  }

  Future<void> logout() async {
    await _request('logout', body: {});
  }

  Future<AccountSyncSnapshot> readSync() async =>
      AccountSyncSnapshot.fromJson(await _request('account-sync'));

  Future<AccountSyncSnapshot> setMobileTheme(int revision, String mode) async {
    if (!const ['light', 'dark', 'system'].contains(mode)) {
      throw const AccountException('지원하지 않는 테마예요.');
    }
    try {
      final response = await _client
          .patch(
            origin.resolve('/relay/account-sync'),
            headers: {'content-type': 'application/json'},
            body: jsonEncode({
              'expected_revision': revision,
              'settings': {'mobile_theme_mode': mode},
              'machines': {},
            }),
          )
          .timeout(const Duration(seconds: 15));
      if (response.statusCode != 200 && response.statusCode != 409) {
        throw AccountException(
          accountError(response.statusCode),
          status: response.statusCode,
        );
      }
      final json =
          jsonDecode(utf8.decode(response.bodyBytes)) as Map<String, dynamic>;
      if (response.statusCode == 409) {
        throw AccountSyncConflict(
          AccountSyncSnapshot.fromJson(json['current'] as Map<String, dynamic>),
        );
      }
      return AccountSyncSnapshot.fromJson(json);
    } on AccountException {
      rethrow;
    } on AccountSyncConflict {
      rethrow;
    } catch (_) {
      throw const AccountException('계정 테마를 저장하지 못했어요. 연결을 확인해 주세요.');
    }
  }

  void close() => _client.close();
}
