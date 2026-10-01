import 'dart:convert';
import 'dart:math';

import 'package:crypto/crypto.dart';

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
    a.scheme == b.scheme && a.host == b.host && _port(a) == _port(b);

/// dart:io 의 소켓 악수는 `wss://` 주소를 `https://` 로 바꿀 때 포트를 `wss` 기준으로 읽는다. Uri 는 wss 의 기본
/// 포트를 몰라 0 이 넘어오고, 그대로 비교하면 기본 포트 관문(443)으로 가는 계정 화면 소켓이 전부 「주소가 바뀌었다」로
/// 끊긴다. 0 은 그 스킴의 기본 포트로 읽는다.
int _port(Uri u) => u.port != 0
    ? u.port
    : switch (u.scheme) {
        'https' => 443,
        'http' => 80,
        _ => 0,
      };

class AccountSession {
  AccountSession({
    required this.origin,
    required this.account,
    required this.deviceId,
    required this.token,
    this.displayName,
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

  /// Google·GitHub 로 가입한 계정은 이름이 `oauth_<hex>` 라 사람에게는 이 값(메일·로그인 이름)을 보인다.
  final String? displayName;
  String get label => displayName?.isNotEmpty == true ? displayName! : account;
  Uri get root => origin.resolve('/relay/account/');
  List<String> get protocols => ['kasa-relay-account', 'kasa-auth.$token'];

  Map<String, Object> toJson() => {
    'kind': 'account',
    'origin': origin.toString(),
    'account': account,
    'device_id': deviceId,
    'token': token,
    'display_name': ?displayName,
  };

  factory AccountSession.fromJson(Map<String, dynamic> json) => AccountSession(
    origin: Uri.parse(json['origin'] as String),
    account: json['account'] as String,
    deviceId: json['device_id'] as String,
    token: json['token'] as String,
    displayName: json['display_name'] as String?,
  );
}

class AccountException implements Exception {
  const AccountException(this.message, {this.status, this.code});
  final String message;
  final int? status;

  /// 관문이 실어 보낸 `error` 값(`account_not_linked` 등). 없으면 null.
  final String? code;
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

/// Google·GitHub 로그인의 관문 오류 — 데스크톱 `native_device_account.rs` 의 `safe_error` 와 같은 뜻으로 말한다.
String oauthError(String? code, int status) => switch (code) {
  'setup_required' || 'oauth_unavailable' => '이 서버는 Google·GitHub 로그인을 아직 켜지 않았어요.',
  'account_not_linked' => '이 로그인이 연결된 KASA 계정이 없어요. 기존 계정으로 로그인한 뒤 설정에서 연결해 주세요.',
  'already_linked' => '이미 다른 KASA 계정에 연결된 로그인이에요. 계정은 자동으로 합치지 않아요.',
  'expired' || 'account_changed' || 'link_expired' || 'invalid_grant' => '로그인 요청이 만료되었거나 계정이 바뀌었어요. 다시 시작해 주세요.',
  'cancelled' => '로그인을 취소했어요.',
  'account_disabled' => '막힌 계정이에요. 관리자에게 물어봐 주세요.',
  'rate_limited' => accountError(429),
  'device_mismatch' => '이 폰의 로그인과 요청이 맞지 않아요. 로그아웃 뒤 다시 로그인해 주세요.',
  _ => status == 401 ? '저장된 로그인이 만료되었어요. 다시 로그인해 주세요.' : '로그인을 마치지 못했어요. 연결 상태를 확인하고 다시 시도해 주세요.',
};

enum OAuthProvider {
  google('google', 'Google'),
  github('github', 'GitHub');

  const OAuthProvider(this.id, this.label);
  final String id;
  final String label;
}

/// 관문이 로그인 결과(일회용 code)를 돌려보내는 이 앱의 주소. iOS 는 ASWebAuthenticationSession 이 이 스킴을 가로챈다.
const oauthRedirectScheme = 'kasaterm';
const oauthRedirectUri = '$oauthRedirectScheme://oauth';

/// 앱 리다이렉트 로그인(RFC 8252)의 비밀. [verifier] 가 없으면 돌아온 code 로 세션을 못 받으니
/// 남이 보낸 링크로 로그인해도 결과는 링크를 만든 쪽이 아니라 이 기기 브라우저로만 돌아온다.
class OAuthRedirect {
  OAuthRedirect._(this.verifier, this.state);

  factory OAuthRedirect.create() => OAuthRedirect._(_random(), _random());

  final String verifier;
  final String state;

  String get challenge => _base64Url(sha256.convert(ascii.encode(verifier)).bytes);

  static final _rng = Random.secure();
  static String _random() => _base64Url(List.generate(32, (_) => _rng.nextInt(256)));
  static String _base64Url(List<int> bytes) => base64Url.encode(bytes).replaceAll('=', '');
}

/// 관문의 Google·GitHub 로그인 요청 하나. `poll_token`·[redirect] 는 이 요청의 결과를 받는 자격이라 메모리에만 둔다.
/// 앱 리다이렉트 요청이면 [redirect] 만, 옛 확인 코드 요청이면 [pollToken]·[userCode] 만 있다.
class OAuthFlow {
  const OAuthFlow({
    required this.provider,
    required this.machineId,
    required this.requestId,
    this.pollToken,
    this.userCode,
    this.redirect,
    required this.authorization,
    required this.expires,
  });

  final OAuthProvider provider;
  final String machineId;
  final String requestId;
  final String? pollToken;

  /// 브라우저 확인 화면에 사람이 넣는 코드. 이 앱에만 보인다 — 링크만 가로챈 사람은 못 넘긴다.
  final String? userCode;
  final OAuthRedirect? redirect;
  final Uri authorization;
  final DateTime expires;

  Map<String, String?> get _poll => {
    'request_id': requestId,
    'poll_token': pollToken,
    'provider': provider.id,
    'kind': 'phone',
    'machine_id': machineId,
  };
}

/// 기다리는 중이면 둘 다 null, 로그인이면 [session], 연결이면 [linked].
class OAuthResult {
  const OAuthResult({this.session, this.linked = false});
  final AccountSession? session;
  final bool linked;
  bool get pending => session == null && !linked;
}

class OAuthProviders {
  const OAuthProviders(this.enabled, {this.signup = false, this.redirect = false});
  final List<OAuthProvider> enabled;

  /// 처음 보는 Google·GitHub 신원으로 새 계정을 만드는 서버인가.
  final bool signup;

  /// 확인 코드 없이 앱 리다이렉트(PKCE)로 결과를 주는 관문인가. 옛 관문은 코드 흐름만 안다.
  final bool redirect;
}

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
///
/// [direct] 가 참인 주소는 앱 안 카사넷 입구(데스크톱 직통)다 — 자격을 싣지 않는다(데스크톱 폰 입구가 관문과 같은
/// 자격을 준다). 입구로 간 GET 이 길에서 끊기면 [fallback] 이 준 관문 주소로 한 번 더 간다. 다른 요청은 다시 보내지
/// 않는다 — 데스크톱이 받았는데 답만 끊겼을 수 있다(키 입력이 두 번 간다).
class OriginClient extends http.BaseClient {
  OriginClient(
    this.origin, {
    http.Client? client,
    this.token,
    this.onUnauthorized,
    this.direct,
    this.fallback,
  }) : _inner = client ?? http.Client();
  final Uri origin;
  final String? token;
  final void Function()? onUnauthorized;
  final bool Function(Uri)? direct;
  final Uri? Function(Uri)? fallback;
  final http.Client _inner;
  bool _closed = false;

  @override
  Future<http.StreamedResponse> send(http.BaseRequest request) async {
    final viaDirect = direct?.call(request.url) ?? false;
    if (!viaDirect) return _send(request, false);
    try {
      return await _send(request, true);
    } on AccountException {
      rethrow;
    } catch (_) {
      final back = fallback?.call(request.url);
      if (back == null || request is! http.Request || request.method != 'GET') rethrow;
      return _send(http.Request('GET', back)..headers.addAll(request.headers), false);
    }
  }

  Future<http.StreamedResponse> _send(http.BaseRequest request, bool viaDirect) async {
    if (_closed ||
        (!viaDirect && !sameOrigin(origin, request.url)) ||
        request.url.userInfo.isNotEmpty) {
      throw const AccountException('연결 대상이 바뀌었어요. 다시 연결해 주세요.');
    }
    request.followRedirects = false;
    if (token != null && !viaDirect) request.headers['authorization'] = 'Bearer $token';
    final response = await _inner.send(request);
    final bytes = await response.stream.toBytes();
    if (_closed) throw const AccountException('이전 연결이 종료되었어요.');
    if (response.statusCode >= 300 && response.statusCode < 400) {
      throw const AccountException('다른 주소로의 이동을 차단했어요. 서버 주소를 확인해 주세요.');
    }
    if (response.statusCode == 401 && token != null && !viaDirect) onUnauthorized?.call();
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
    Map<String, Object?>? body,
    String Function(String? code, int status)? error,
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
        String? code;
        try {
          final e = (jsonDecode(utf8.decode(response.bodyBytes)) as Map)['error'];
          if (e is String && RegExp(r'^[a-z_]{1,40}$').hasMatch(e)) code = e;
        } catch (_) {}
        throw AccountException(
          (error ?? (_, status) => accountError(status))(code, response.statusCode),
          status: response.statusCode,
          code: code,
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

  /// 관문이 켜 둔 Google·GitHub 로그인. 옛 관문·끊김이면 빈 목록.
  Future<OAuthProviders> oauthProviders() async {
    try {
      final json = await _request('oauth/providers');
      final enabled = {
        for (final p in json['providers'] as List? ?? const [])
          if (p is Map && p['enabled'] == true) p['id'],
      };
      return OAuthProviders(
        [for (final p in OAuthProvider.values) if (enabled.contains(p.id)) p],
        signup: json['signup_enabled'] == true,
        redirect: json['redirect_login'] == true,
      );
    } on AccountException {
      return const OAuthProviders([]);
    }
  }

  /// [link] 면 지금 로그인한 계정에 이 로그인 방법을 더한다(기기 토큰이 실려야 한다), 아니면 그 신원으로 로그인.
  /// [redirect] 면(이 기기가 시스템 로그인 창을 띄울 수 있으면) 관문이 아는 한 확인 코드 없는 앱 리다이렉트로 시작한다.
  Future<OAuthFlow> oauthStart(OAuthProvider provider, String machineId, {bool link = false, bool redirect = false}) async {
    final r = redirect && (await oauthProviders()).redirect ? OAuthRedirect.create() : null;
    final json = await _request(
      'oauth/start',
      body: {
        'provider': provider.id,
        'kind': 'phone',
        'label': '카사모바일',
        'machine_id': machineId,
        'link': link,
        if (r != null) ...{
          'code_challenge': r.challenge,
          'code_challenge_method': 'S256',
          'redirect_uri': oauthRedirectUri,
          'state': r.state,
        },
      },
      error: oauthError,
    );
    final url = Uri.tryParse('${json['authorization_url']}');
    final (id, poll, code, ttl) = (json['request_id'], json['poll_token'], json['user_code'], json['expires_in']);
    // 확인 화면은 관문 자신의 주소여야 한다 — 다른 곳으로 보내는 응답은 따르지 않는다.
    if (url == null ||
        !sameOrigin(url, origin) ||
        id is! String ||
        ttl is! int ||
        (r == null && (poll is! String || code is! String))) {
      throw const AccountException('로그인 요청 응답을 확인하지 못했어요.');
    }
    return OAuthFlow(
      provider: provider,
      machineId: machineId,
      requestId: id,
      pollToken: r == null ? poll as String : null,
      userCode: r == null ? code as String : null,
      redirect: r,
      authorization: url,
      expires: DateTime.now().add(Duration(seconds: ttl)),
    );
  }

  /// 시스템 로그인 창이 [back] 으로 돌아오면 그 일회용 code 를 verifier 와 함께 세션으로 바꾼다.
  Future<OAuthResult> oauthRedeem(OAuthFlow flow, Uri back) async {
    final r = flow.redirect;
    final q = back.queryParameters;
    if (r == null || back.scheme != oauthRedirectScheme || q['state'] != r.state) {
      throw const AccountException('로그인 응답을 확인하지 못했어요.');
    }
    if (q['error'] case final e?) {
      throw AccountException(oauthError(e == 'access_denied' ? 'cancelled' : 'oauth_unavailable', 400));
    }
    final code = q['code'];
    if (code == null || code.isEmpty) throw const AccountException('로그인 응답을 확인하지 못했어요.');
    final json = await _request(
      'oauth/token',
      body: {'code': code, 'code_verifier': r.verifier, 'redirect_uri': oauthRedirectUri},
      error: oauthError,
    );
    final result = _oauthResult(json);
    if (result.pending) throw const AccountException('로그인 응답을 확인하지 못했어요.');
    return result;
  }

  Future<OAuthResult> oauthPoll(OAuthFlow flow) async =>
      _oauthResult(await _request('oauth/poll', body: flow._poll, error: oauthError));

  OAuthResult _oauthResult(Map<String, dynamic> json) {
    switch (json['status']) {
      case 'linked':
        return const OAuthResult(linked: true);
      case 'complete':
        try {
          return OAuthResult(
            session: AccountSession(
              origin: origin,
              account: json['account'] as String,
              deviceId: json['device_id'] as String,
              token: json['token'] as String,
              displayName: json['display_name'] as String?,
            ),
          );
        } on AccountException {
          rethrow;
        } catch (_) {
          throw const AccountException('로그인 응답을 확인하지 못했어요.');
        }
      default:
        return const OAuthResult();
    }
  }

  Future<void> oauthCancel(OAuthFlow flow) async {
    // 리다이렉트 요청은 거둘 자격이 없다 — 아무도 code 를 안 바꾸면 관문에서 10분 뒤 사라진다.
    if (flow.pollToken == null) return;
    try {
      await _request('oauth/cancel', body: flow._poll);
    } on AccountException {
      // 이미 끝났거나 만료된 요청 — 관문에서도 10분이면 사라진다.
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

  Future<AccountSyncSnapshot> setMobileTheme(int revision, String mode) {
    if (!const ['light', 'dark', 'system'].contains(mode)) {
      throw const AccountException('지원하지 않는 테마예요.');
    }
    return patchSync(
      revision,
      {'mobile_theme_mode': mode},
      failure: '계정 테마를 저장하지 못했어요. 연결을 확인해 주세요.',
    );
  }

  /// 옵트인 키(`weather` 등)는 이름을 대야 보인다 — 관문이 [keys] 를 모르면(옛 관문) 그 키만 빠진다.
  Future<AccountSyncSnapshot> readSyncWith(List<String> keys) async {
    try {
      final response = await _client
          .get(origin.resolve('/relay/account-sync'), headers: {_syncKeys: keys.join(',')})
          .timeout(const Duration(seconds: 15));
      if (response.statusCode != 200) {
        throw AccountException(accountError(response.statusCode), status: response.statusCode);
      }
      return AccountSyncSnapshot.fromJson(jsonDecode(utf8.decode(response.bodyBytes)) as Map<String, dynamic>);
    } on AccountException {
      rethrow;
    } catch (_) {
      throw const AccountException('계정 설정을 받지 못했어요. 연결을 확인해 주세요.');
    }
  }

  /// 계정 설정 몇 칸을 바꾼다. 다른 기기가 먼저 바꿨으면 [AccountSyncConflict] 에 지금 값을 싣는다.
  Future<AccountSyncSnapshot> patchSync(
    int revision,
    Map<String, Object?> settings, {
    List<String> keys = const [],
    String failure = '계정에 저장하지 못했어요. 연결을 확인해 주세요.',
  }) async {
    try {
      final response = await _client
          .patch(
            origin.resolve('/relay/account-sync'),
            headers: {'content-type': 'application/json', if (keys.isNotEmpty) _syncKeys: keys.join(',')},
            body: jsonEncode({
              'expected_revision': revision,
              'settings': settings,
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
      throw AccountException(failure);
    }
  }

  /// 데스크톱 `account_sync::schema::KEYS_HEADER` 와 같은 이름.
  static const _syncKeys = 'x-kasa-sync-keys';

  void close() => _client.close();
}
