class ClaudeAccount {
  const ClaudeAccount({
    required this.id,
    required this.label,
    required this.name,
    required this.subtitle,
    required this.usageState,
    this.subtitleCode,
    this.loggedIn,
  });

  final String id;
  final String label;
  final String name;
  final String subtitle;
  final String usageState;
  final String? subtitleCode;
  final bool? loggedIn;

  String get title => label.isNotEmpty
      ? label
      : name.isNotEmpty
      ? name
      : id;
  String get status {
    if (loggedIn == false ||
        subtitleCode == 'account_login_required' ||
        const {
          'logged_out',
          'login_required',
          'expired',
          'unauthorized',
        }.contains(usageState)) {
      return '다시 로그인 필요';
    }
    if (usageState == 'failed' || usageState == 'error') return '조회 실패';
    if (subtitleCode == 'account_checking' ||
        usageState == 'checking' ||
        usageState == 'loading') {
      return '확인 중';
    }
    return '등록된 계정';
  }

  String get detail {
    final parts = <String>{};
    for (final text in [if (name != title) name, subtitle]) {
      for (final part in text.split(' · ').map((part) => part.trim())) {
        if (part.isEmpty ||
            part == title ||
            const {
              '로그인 필요',
              '다시 로그인 필요',
              '확인 중',
              '확인 중…',
              '조회 실패',
            }.contains(part)) {
          continue;
        }
        parts.add(part);
      }
    }
    if (status != '등록된 계정' || parts.isEmpty) parts.add(status);
    return parts.join(' · ');
  }

  static List<ClaudeAccount> parse(Object? raw) {
    final claude = raw is Map ? raw['claude'] : null;
    final accounts = claude is Map ? claude['accounts'] : null;
    if (accounts is! List) throw const FormatException('claude_accounts');
    return [
      for (final account in accounts)
        if (account is Map &&
            account['id'] is String &&
            (account['id'] as String).isNotEmpty)
          ClaudeAccount(
            id: account['id'] as String,
            label: account['label'] is String ? account['label'] as String : '',
            name: account['name'] is String ? account['name'] as String : '',
            subtitle: account['sub'] is String ? account['sub'] as String : '',
            subtitleCode: account['sub_code'] is String
                ? account['sub_code'] as String
                : null,
            loggedIn: account['logged_in'] is bool
                ? account['logged_in'] as bool
                : null,
            usageState: account['usage_state'] is String
                ? account['usage_state'] as String
                : '',
          ),
    ];
  }
}

enum ClaudeLoginState {
  starting,
  awaitingAuthorization,
  awaitingCode,
  verifying,
  needsConfirmation,
  succeeded,
  failed,
  cancelled,
  expired;

  bool get terminal => switch (this) {
    succeeded || failed || cancelled || expired => true,
    _ => false,
  };
}

class ClaudePhoneLogin {
  const ClaudePhoneLogin({
    required this.flowId,
    required this.slotId,
    required this.machineId,
    required this.label,
    required this.email,
    required this.org,
    required this.state,
    required this.expiresAtMs,
    this.authorizationUrl,
    this.errorCode,
    this.confirmationNonce,
    this.expectedEmail,
    this.expectedOrg,
    this.accountUuid,
    this.organizationUuid,
    this.runtimeUpdated,
    this.runtimeErrorCode,
  });

  final String flowId;
  final String slotId;
  final String machineId;
  final String label;
  final String email;
  final String org;
  final ClaudeLoginState state;
  final int expiresAtMs;
  final Uri? authorizationUrl;
  final String? errorCode;
  final String? confirmationNonce;
  final String? expectedEmail;
  final String? expectedOrg;
  final String? accountUuid;
  final String? organizationUuid;
  final bool? runtimeUpdated;
  final String? runtimeErrorCode;

  static ClaudePhoneLogin parse(Object? raw) {
    if (raw is! Map) throw const FormatException('phone_login');
    String requiredString(String key) {
      final value = raw[key];
      if (value is! String || value.isEmpty) {
        throw const FormatException('phone_login_field');
      }
      return value;
    }

    String? optional(String key) =>
        raw[key] is String ? raw[key] as String : null;
    final state = switch (raw['state']) {
      'starting' => ClaudeLoginState.starting,
      'awaiting_authorization' => ClaudeLoginState.awaitingAuthorization,
      'awaiting_code' => ClaudeLoginState.awaitingCode,
      'verifying' => ClaudeLoginState.verifying,
      'needs_confirmation' => ClaudeLoginState.needsConfirmation,
      'succeeded' => ClaudeLoginState.succeeded,
      'failed' => ClaudeLoginState.failed,
      'cancelled' => ClaudeLoginState.cancelled,
      'expired' => ClaudeLoginState.expired,
      _ => throw const FormatException('phone_login_state'),
    };
    final expiry = raw['expires_at_ms'];
    if (expiry is! num || !expiry.isFinite) {
      throw const FormatException('phone_login_expiry');
    }
    final url = optional('authorization_url');
    final authorizationUrl = url == null ? null : claudeAuthorizationUrl(url);
    if (url != null && authorizationUrl == null) {
      throw const FormatException('phone_login_url');
    }
    final nonce = optional('confirmation_nonce');
    if (state == ClaudeLoginState.needsConfirmation &&
        (nonce == null || nonce.isEmpty)) {
      throw const FormatException('phone_login_confirmation');
    }
    return ClaudePhoneLogin(
      flowId: requiredString('flow_id'),
      slotId: requiredString('slot_id'),
      machineId: requiredString('machine_id'),
      label: optional('label') ?? '',
      email: optional('email') ?? '',
      org: optional('org') ?? '',
      state: state,
      expiresAtMs: expiry.toInt(),
      authorizationUrl: authorizationUrl,
      errorCode: optional('error_code'),
      confirmationNonce: nonce,
      expectedEmail: optional('expected_email'),
      expectedOrg: optional('expected_org'),
      accountUuid: optional('account_uuid'),
      organizationUuid: optional('organization_uuid'),
      runtimeUpdated: raw['runtime_updated'] is bool
          ? raw['runtime_updated'] as bool
          : null,
      runtimeErrorCode: optional('runtime_error_code'),
    );
  }
}

Uri? claudeAuthorizationUrl(String value) {
  if (value.length > 8192 ||
      RegExp(r'''[\x00-\x20\x7f"'<>]''').hasMatch(value)) {
    return null;
  }
  final uri = Uri.tryParse(value);
  if (uri == null ||
      uri.scheme != 'https' ||
      uri.userInfo.isNotEmpty ||
      uri.hasFragment ||
      uri.hasPort) {
    return null;
  }
  final allowed = switch (uri.host) {
    'claude.ai' => uri.path == '/oauth/authorize',
    'console.anthropic.com' ||
    'platform.claude.com' => uri.path == '/oauth/authorize',
    _ => false,
  };
  if (!allowed) return null;
  try {
    final redirects = uri.queryParametersAll['redirect_uri'];
    if (redirects == null ||
        redirects.length != 1 ||
        !const {
          'https://platform.claude.com/oauth/code/callback',
          'https://console.anthropic.com/oauth/code/callback',
        }.contains(redirects.single)) {
      return null;
    }
  } on FormatException {
    return null;
  }
  return uri;
}

bool claudeLoginCode(String value) =>
    value.length <= 2048 &&
    RegExp(r'^[A-Za-z0-9_-]{16,}#[A-Za-z0-9_-]{16,}$').hasMatch(value);

bool claudeLoginNonce(String value) => RegExp(
  r'^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$',
).hasMatch(value);

String claudeLoginError(String? code) => switch (code) {
  'phone_login_identity_mismatch' ||
  'identity_mismatch' ||
  'account_mismatch' ||
  'email_mismatch' ||
  'organization_mismatch' => '선택한 슬롯과 다른 Claude 계정이에요. 원래 계정으로 다시 로그인해 주세요.',
  'phone_login_expired' ||
  'flow_expired' ||
  'expired' => '로그인 시간이 지났어요. 다시 시작해 주세요.',
  'phone_login_unknown' ||
  'flow_not_found' ||
  'not_found' => '로그인 요청이 없어졌어요. 목록에서 다시 시작해 주세요.',
  'phone_login_busy' ||
  'flow_conflict' ||
  'slot_busy' ||
  'busy' => '이 계정은 다른 곳에서 로그인 중이에요. 끝난 뒤 다시 해 주세요.',
  'phone_login_invalid_code' ||
  'invalid_code' ||
  'invalid_authorization_code' => '인증코드를 확인해 다시 붙여 넣어 주세요.',
  'phone_login_action_unknown' ||
  'unsupported' ||
  'action_unknown' ||
  'unknown_action' ||
  'update_required' => '이 기기의 KASA를 새 판으로 업데이트해 주세요.',
  'phone_login_target_changed' ||
  'slot_not_found' ||
  'invalid_slot' => '이 기기의 계정 슬롯이 바뀌었어요. 목록을 새로 받아 주세요.',
  'phone_login_identity_unverified' ||
  'identity_unavailable' => 'Claude 계정 정보를 확인하지 못했어요. 다시 시작해 주세요.',
  'phone_login_machine_mismatch' => '다른 기기에 연결됐어요. 기기 목록을 다시 확인해 주세요.',
  'phone_login_owner_changed' => '기기의 KASA 로그인이 바뀌었어요. 기기를 다시 연결해 주세요.',
  'phone_login_unsupported' => '이 환경에서는 폰 로그인을 지원하지 않아요. 기기의 KASA에서 로그인해 주세요.',
  'phone_login_too_many_attempts' => '로그인을 여러 번 시도했어요. 잠시 뒤 다시 시작해 주세요.',
  'phone_login_not_waiting' => '인증코드를 받는 단계가 아니에요. 로그인 상태를 다시 확인해 주세요.',
  'phone_login_confirmation_invalid' => '계정 확인 요청이 바뀌었어요. 로그인 상태를 다시 확인해 주세요.',
  'phone_login_storage_failed' =>
    '기기에 로그인 정보를 저장하지 못했어요. 기기의 저장 공간과 권한을 확인해 주세요.',
  'phone_login_cli_failed' ||
  'phone_login_output_limit' => '기기에서 Claude 로그인을 마치지 못했어요. 다시 시작해 주세요.',
  'phone_login_cancelled' => '로그인을 취소했어요.',
  'phone_login_invalid_start' => '로그인 요청을 확인하지 못했어요. 계정 목록에서 다시 시작해 주세요.',
  _ => '로그인을 마치지 못했어요. 다시 시작해 주세요.',
};
