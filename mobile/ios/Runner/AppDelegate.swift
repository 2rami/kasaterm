import AuthenticationServices
import Flutter
import LocalAuthentication
import UIKit
import UserNotifications
import os

/// 푸시(APNs) 다리. 다트가 `request` 를 부르면 권한을 묻고 애플에 등록하고, 토큰이
/// 오면 `onToken` 으로 넘긴다(다트가 카사텀 서버에 맡긴다). 알림을 눌러 앱이 뜨면
/// `onTap` 으로 payload(machine·pane)를 넘겨 그 학생 화면을 연다 — 앱이 죽어 있다
/// 켜진 경우는 다트가 아직 없으므로 `pending` 에 두었다가 `pending` 호출 때 준다.
@main
@objc class AppDelegate: FlutterAppDelegate, FlutterImplicitEngineDelegate {
  private var channel: FlutterMethodChannel?
  private var a11yChannel: FlutterMethodChannel?
  private var pendingTap: [String: Any]?
  private var lastToken: String?
  private var pushEnabled = UserDefaults.standard.bool(forKey: "kasaLegacyPushEnabled")
  private var webAuth: ASWebAuthenticationSession?
  private var backgroundChannel: FlutterMethodChannel?
  private var graceTask: UIBackgroundTaskIdentifier = .invalid
  /// 설치에 앞자리를 내주는 중 — 뒤로 간 앱이 시간을 더 받아 살아 있으면 설치가 그만큼 밀릴 수 있다.
  private var leavingForInstall = false
  /// 새 판 설치 뒤 「눌러서 열기」 알림. 새 판이 켜지면 거둔다.
  private static let releaseNotice = "kasa.release.open"

  override func application(
    _ application: UIApplication,
    didFinishLaunchingWithOptions launchOptions: [UIApplication.LaunchOptionsKey: Any]?
  ) -> Bool {
    UNUserNotificationCenter.current().delegate = self
    UNUserNotificationCenter.current().removePendingNotificationRequests(withIdentifiers: [Self.releaseNotice])
    UNUserNotificationCenter.current().removeDeliveredNotifications(withIdentifiers: [Self.releaseNotice])
    ReleaseTrail.launched()
    if let remote = launchOptions?[.remoteNotification] as? [AnyHashable: Any] {
      pendingTap = Self.payload(remote)
    }
    return super.application(application, didFinishLaunchingWithOptions: launchOptions)
  }

  private func endGrace() {
    guard graceTask != .invalid else { return }
    UIApplication.shared.endBackgroundTask(graceTask)
    graceTask = .invalid
  }

  func didInitializeImplicitFlutterEngine(_ engineBridge: FlutterImplicitEngineBridge) {
    GeneratedPluginRegistrant.register(with: engineBridge.pluginRegistry)
    let messenger = engineBridge.applicationRegistrar.messenger()
    // 「투명도 줄이기」는 플러터가 안 알려 준다 — 날씨가 이 값을 보고 꺼진다(docs/weather.md 접근성).
    let a11y = FlutterMethodChannel(name: "kasaterm/a11y", binaryMessenger: messenger)
    a11yChannel = a11y
    a11y.setMethodCallHandler { call, result in
      if call.method == "reduceTransparency" {
        result(UIAccessibility.isReduceTransparencyEnabled)
      } else {
        result(FlutterMethodNotImplemented)
      }
    }
    NotificationCenter.default.addObserver(
      forName: UIAccessibility.reduceTransparencyStatusDidChangeNotification, object: nil, queue: .main
    ) { [weak self] _ in
      self?.a11yChannel?.invokeMethod("reduceTransparency", arguments: UIAccessibility.isReduceTransparencyEnabled)
    }
    // 다른 앱에 다녀오는 동안 연결을 살려 둘 시간을 받는다(다트 `background_grace.dart`). 시간이 다 되면 다트가 닫는다.
    let background = FlutterMethodChannel(name: "kasaterm/background", binaryMessenger: messenger)
    backgroundChannel = background
    background.setMethodCallHandler { [weak self] call, result in
      guard let self else { return result(nil) }
      switch call.method {
      case "begin":
        if !self.leavingForInstall && self.graceTask == .invalid {
          self.graceTask = UIApplication.shared.beginBackgroundTask(withName: "kasaterm.grace") { [weak self] in
            self?.backgroundChannel?.invokeMethod("expired", arguments: nil)
            self?.endGrace()
          }
        }
        result(self.graceTask != .invalid)
      case "end":
        self.leavingForInstall = false
        self.endGrace()
        result(nil)
      default:
        result(FlutterMethodNotImplemented)
      }
    }
    // 새 판 설치(다트 `installRelease`). 앞에 떠 있는 앱은 iOS 가 갈아 끼우지 않아 홈으로 비켜서야 설치가 시작되고,
    // 설치된 앱을 스스로 켤 수는 없어 알림을 남긴다. 비켜서는 공개 API 가 없어 suspend 를 보낸다 — App Store 판이 아니다.
    // suspend 가 먹지 않는 iOS 면 사파리로 설치 화면을 연다(공개 API 로 앞자리를 내주는 길).
    let release = FlutterMethodChannel(name: "kasaterm/release", binaryMessenger: messenger)
    release.setMethodCallHandler { [weak self] call, result in
      let args = call.arguments as? [String: Any] ?? [:]
      switch call.method {
      case "log":
        ReleaseTrail.add(args["line"] as? String ?? "")
        result(nil)
      case "arm":
        ReleaseTrail.arm()
        result(nil)
      case "trail":
        result(ReleaseTrail.lines)
      case "stepAside":
        self?.stepAside(args)
        result(nil)
      default:
        result(FlutterMethodNotImplemented)
      }
    }
    // 계정 로그인 시스템 창. 관문이 kasaterm:// 로 돌려보낸 주소(일회용 code)를 다트에 준다 — 확인 코드 입력이 없다.
    let auth = FlutterMethodChannel(name: "kasaterm/web_auth", binaryMessenger: messenger)
    auth.setMethodCallHandler { [weak self] call, result in
      guard call.method == "authenticate", let self,
            let args = call.arguments as? [String: String],
            let url = args["url"].flatMap(URL.init(string:)),
            let scheme = args["scheme"]
      else {
        result(FlutterMethodNotImplemented)
        return
      }
      self.webAuth?.cancel()
      let session = ASWebAuthenticationSession(url: url, callbackURLScheme: scheme) { [weak self] back, error in
        DispatchQueue.main.async {
          self?.webAuth = nil
          if let back {
            result(back.absoluteString)
          } else if (error as? ASWebAuthenticationSessionError)?.code == .canceledLogin {
            result(nil)
          } else {
            result(FlutterError(code: "web_auth", message: error?.localizedDescription, details: nil))
          }
        }
      }
      session.presentationContextProvider = self
      // Safari 의 Google·GitHub 로그인을 그대로 쓴다 — 매번 다시 로그인하지 않게.
      session.prefersEphemeralWebBrowserSession = false
      self.webAuth = session
      if !session.start() {
        self.webAuth = nil
        result(FlutterError(code: "web_auth", message: "start failed", details: nil))
      }
    }
    // 1Password 비밀 요청 승인 열쇠(docs/op-faceid-approval.md). 다트는 공개키와 서명만 받는다.
    let secureKey = FlutterMethodChannel(name: "kasaterm/secure_key", binaryMessenger: messenger)
    secureKey.setMethodCallHandler { call, result in
      let args = call.arguments as? [String: Any] ?? [:]
      switch call.method {
      case "publicKey":
        result(ApprovalKey.find().flatMap(ApprovalKey.publicKey))
      case "create":
        do {
          result(try ApprovalKey.create())
        } catch {
          result(FlutterError(code: "create_failed", message: error.localizedDescription, details: nil))
        }
      case "delete":
        ApprovalKey.delete()
        result(nil)
      case "sign":
        guard let text = args["message"] as? String, let message = Data(base64Encoded: text),
              let reason = args["reason"] as? String
        else { return result(FlutterError(code: "bad_args", message: nil, details: nil)) }
        ApprovalKey.sign(message: message, reason: reason) { outcome in
          DispatchQueue.main.async {
            switch outcome {
            case .success(let signature): result(signature)
            case .failure(let error): result(FlutterError(code: "sign_failed", message: error.localizedDescription, details: nil))
            }
          }
        }
      default:
        result(FlutterMethodNotImplemented)
      }
    }
    let ch = FlutterMethodChannel(name: "kasaterm/push", binaryMessenger: messenger)
    channel = ch
    ch.setMethodCallHandler { [weak self] call, result in
      guard let self else { return }
      switch call.method {
      case "request":
        self.pushEnabled = true
        UserDefaults.standard.set(true, forKey: "kasaLegacyPushEnabled")
        self.requestPush()
        result(nil)
      case "suspend":
        self.pushEnabled = false
        UserDefaults.standard.set(false, forKey: "kasaLegacyPushEnabled")
        self.pendingTap = nil
        UIApplication.shared.unregisterForRemoteNotifications()
        result(nil)
      case "pending":
        let tap = self.pendingTap
        self.pendingTap = nil
        result(tap)
      case "token":
        result(self.lastToken.map { ["token": $0, "env": Self.env] })
      default:
        result(FlutterMethodNotImplemented)
      }
    }
  }

  private func requestPush() {
    let center = UNUserNotificationCenter.current()
    center.requestAuthorization(options: [.alert, .sound, .badge]) { granted, _ in
      guard granted else { return }
      DispatchQueue.main.async {
        guard self.pushEnabled else { return }
        UIApplication.shared.registerForRemoteNotifications()
      }
    }
  }

  /// 샌드박스(dev)인지 본 서버(prod)인지 — 서버가 갈라 쏜다. 빌드 모드가 아니라
  /// 서명 프로필의 `aps-environment` 가 정한다: `phone.sh` 는 release 로 굽지만
  /// Apple Development 로 서명해 토큰이 샌드박스 것이 된다(2026-09-17, 「prod」라
  /// 등록돼 본 서버가 BadDeviceToken 으로 지워 버렸다). 앱스토어 판은 프로필이 없다.
  private static var env: String {
    guard let url = Bundle.main.url(forResource: "embedded", withExtension: "mobileprovision"),
      let data = try? Data(contentsOf: url),
      let text = String(data: data, encoding: .isoLatin1),
      let key = text.range(of: "<key>aps-environment</key>"),
      let open = text.range(of: "<string>", range: key.upperBound..<text.endIndex),
      let close = text.range(of: "</string>", range: open.upperBound..<text.endIndex)
    else {
      return "prod"
    }
    return text[open.upperBound..<close.lowerBound] == "development" ? "dev" : "prod"
  }

  override func application(
    _ application: UIApplication,
    didRegisterForRemoteNotificationsWithDeviceToken deviceToken: Data
  ) {
    guard pushEnabled else { return }
    let token = deviceToken.map { String(format: "%02x", $0) }.joined()
    lastToken = token
    channel?.invokeMethod("onToken", arguments: ["token": token, "env": Self.env])
  }

  override func application(
    _ application: UIApplication,
    didFailToRegisterForRemoteNotificationsWithError error: Error
  ) {
    channel?.invokeMethod("onTokenError", arguments: error.localizedDescription)
  }

  private static func payload(_ info: [AnyHashable: Any]) -> [String: Any] {
    var out: [String: Any] = [:]
    for key in ["machine", "pane", "kind", "approval"] {
      if let v = info[key] as? String { out[key] = v }
    }
    return out
  }

  // 앱이 앞에 떠 있어도 배너·소리를 낸다 — 다른 학생 화면을 보는 중일 수 있다.
  override func userNotificationCenter(
    _ center: UNUserNotificationCenter,
    willPresent notification: UNNotification,
    withCompletionHandler completionHandler: @escaping (UNNotificationPresentationOptions) -> Void
  ) {
    // 원격 승인은 앱이 제 띠를 세운다 — 앞에 떠 있을 때 시스템 배너까지 내면 같은 요청이 두 번 보인다.
    let kind = notification.request.content.userInfo["kind"] as? String
    if notification.request.identifier == Self.releaseNotice {
      completionHandler([])
      return
    }
    if kind == "approval" {
      completionHandler(pushEnabled ? [.list] : [])
      return
    }
    completionHandler(pushEnabled ? [.banner, .list, .sound] : [])
  }

  override func userNotificationCenter(
    _ center: UNUserNotificationCenter,
    didReceive response: UNNotificationResponse,
    withCompletionHandler completionHandler: @escaping () -> Void
  ) {
    guard pushEnabled, response.notification.request.identifier != Self.releaseNotice else {
      completionHandler()
      return
    }
    let tap = Self.payload(response.notification.request.content.userInfo)
    if let ch = channel {
      ch.invokeMethod("onTap", arguments: tap)
    } else {
      pendingTap = tap
    }
    completionHandler()
  }
}

extension AppDelegate {
  fileprivate func stepAside(_ args: [String: Any]) {
    ReleaseTrail.add("비켜서기 요청")
    leavingForInstall = true
    endGrace()
    let content = UNMutableNotificationContent()
    content.title = args["title"] as? String ?? ""
    content.body = args["body"] as? String ?? ""
    content.sound = .default
    let after = max(5, (args["after"] as? NSNumber)?.doubleValue ?? 45)
    let request = UNNotificationRequest(
      identifier: Self.releaseNotice, content: content,
      trigger: UNTimeIntervalNotificationTrigger(timeInterval: after, repeats: false))
    let page = (args["page"] as? String).flatMap(URL.init(string:))
    let center = UNUserNotificationCenter.current()
    center.requestAuthorization(options: [.alert, .sound]) { granted, _ in
      center.add(request) { error in
        ReleaseTrail.add("열기 알림 \(error == nil ? "걸음" : "실패 \(error!.localizedDescription)") · 권한 \(granted)")
        DispatchQueue.main.async { self.leaveForeground(page) }
      }
    }
  }

  private func leaveForeground(_ page: URL?) {
    let app = UIApplication.shared
    let suspend = NSSelectorFromString("suspend")
    if app.responds(to: suspend) {
      ReleaseTrail.add("suspend 보냄 · 앱 \(ReleaseTrail.state(app.applicationState))")
      app.perform(suspend)
    } else {
      ReleaseTrail.add("suspend 없음")
    }
    // 비켜섰으면 이 블록은 앱이 뒤로 간 채로 돌거나(상태 background) 아예 돌지 않는다.
    DispatchQueue.main.asyncAfter(deadline: .now() + 1.2) {
      let state = app.applicationState
      ReleaseTrail.add("1.2초 뒤 앱 \(ReleaseTrail.state(state))")
      guard state == .active, let page else { return }
      app.open(page) { ok in ReleaseTrail.add("사파리로 설치 화면 열기 → \(ok)") }
    }
  }
}

/// 새 판 설치가 실기에서 어디까지 갔는지 남기는 기록(10-07: 앱이 비켜서지 않았는데 가상 아이폰은 Ad Hoc 설치를 못
/// 재현한다). [설치]를 누른 뒤 10분 동안만 앱 상태 바뀜까지 적고, 판이 바뀌어 켜지면 그 사실과 걸린 시간을 적는다.
/// 설정 「지난 설치 기록」이 보여 주고, 같은 줄이 통합 로그(subsystem 번들 id, category release)에도 간다.
enum ReleaseTrail {
  private static let key = "kasaReleaseTrail"
  private static let armedKey = "kasaReleaseArmedAt"
  private static let buildKey = "kasaReleaseLastBuild"
  private static let keep = 120
  private static let window: TimeInterval = 600
  private static let log = Logger(subsystem: Bundle.main.bundleIdentifier ?? "kasaterm", category: "release")
  private static var observing = false
  private static let stamp: DateFormatter = {
    let f = DateFormatter()
    f.locale = Locale(identifier: "en_US_POSIX")
    f.dateFormat = "MM-dd HH:mm:ss.SSS"
    return f
  }()

  static var build: String { Bundle.main.infoDictionary?["CFBundleVersion"] as? String ?? "?" }
  static var lines: [String] { UserDefaults.standard.stringArray(forKey: key) ?? [] }

  static func add(_ line: String) {
    let entry = "\(stamp.string(from: Date())) [\(build)] \(line)"
    log.notice("\(entry, privacy: .public)")
    var all = lines
    all.append(entry)
    UserDefaults.standard.set(Array(all.suffix(keep)), forKey: key)
  }

  static func arm() {
    UserDefaults.standard.set(Date().timeIntervalSince1970, forKey: armedKey)
    watchScenes()
  }

  private static var armedAt: TimeInterval? {
    let t = UserDefaults.standard.double(forKey: armedKey)
    return t > 0 && Date().timeIntervalSince1970 - t < window ? t : nil
  }

  static func launched() {
    let before = UserDefaults.standard.string(forKey: buildKey)
    UserDefaults.standard.set(build, forKey: buildKey)
    if let before, before != build {
      let since = armedAt.map { " · [설치] 누른 지 \(Int(Date().timeIntervalSince1970 - $0))초" } ?? " · 앱 안 [설치]를 거치지 않음"
      add("새 판으로 켜짐 \(before) → \(build)\(since)")
      UserDefaults.standard.removeObject(forKey: armedKey)
    } else if armedAt != nil {
      add("같은 판으로 다시 켜짐")
      watchScenes()
    }
  }

  static func state(_ s: UIApplication.State) -> String {
    switch s {
    case .active: return "active"
    case .inactive: return "inactive"
    case .background: return "background"
    @unknown default: return "unknown"
    }
  }

  /// 다트가 보는 상태는 엔진을 한 번 거친 것이라, 시스템이 실제로 보낸 알림을 따로 적는다.
  private static func watchScenes() {
    guard !observing else { return }
    observing = true
    let names: [(Notification.Name, String)] = [
      (UIScene.willDeactivateNotification, "scene 비활성"),
      (UIScene.didActivateNotification, "scene 활성"),
      (UIScene.didEnterBackgroundNotification, "scene 뒤로"),
      (UIScene.willEnterForegroundNotification, "scene 앞으로"),
    ]
    for (name, label) in names {
      NotificationCenter.default.addObserver(forName: name, object: nil, queue: .main) { _ in
        if armedAt != nil { add(label) }
      }
    }
  }
}

extension AppDelegate: ASWebAuthenticationPresentationContextProviding {
  func presentationAnchor(for session: ASWebAuthenticationSession) -> ASPresentationAnchor {
    let windows = UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }.flatMap(\.windows)
    return windows.first(where: \.isKeyWindow) ?? windows.first ?? ASPresentationAnchor()
  }
}

/// 승인 열쇠 — Secure Enclave 의 P-256 개인 키. 폰 밖으로 안 나가고, Face ID 가 그 자리에서 풀어야만 서명한다.
/// 생체 정보가 바뀌면(`.biometryCurrentSet`) 키가 무효가 되어 다시 만들어야 한다.
enum ApprovalKey {
  private static let tag = Data("kasaterm.approval.key".utf8)

  static func find(context: LAContext? = nil) -> SecKey? {
    var query: [String: Any] = [
      kSecClass as String: kSecClassKey,
      kSecAttrApplicationTag as String: tag,
      kSecAttrKeyType as String: kSecAttrKeyTypeECSECPrimeRandom,
      kSecReturnRef as String: true,
    ]
    if let context { query[kSecUseAuthenticationContext as String] = context }
    var item: CFTypeRef?
    guard SecItemCopyMatching(query as CFDictionary, &item) == errSecSuccess, let item else { return nil }
    return (item as! SecKey)
  }

  static func publicKey(_ key: SecKey) -> String? {
    guard let pub = SecKeyCopyPublicKey(key), let data = SecKeyCopyExternalRepresentation(pub, nil) as Data? else { return nil }
    return data.base64EncodedString()
  }

  static func delete() {
    SecItemDelete([kSecClass as String: kSecClassKey, kSecAttrApplicationTag as String: tag] as CFDictionary)
  }

  static func create() throws -> String {
    delete()
    var error: Unmanaged<CFError>?
    #if targetEnvironment(simulator)
    // 가상 아이폰에는 Secure Enclave 가 없다 — 검증 리그용 시험 키. 이 갈래는 실기 판에 컴파일되지 않는다.
    let flags: SecAccessControlCreateFlags = []
    #else
    let flags: SecAccessControlCreateFlags = [.privateKeyUsage, .biometryCurrentSet]
    #endif
    guard let access = SecAccessControlCreateWithFlags(nil, kSecAttrAccessibleWhenUnlockedThisDeviceOnly, flags, &error) else {
      throw error!.takeRetainedValue() as Error
    }
    var attrs: [String: Any] = [
      kSecAttrKeyType as String: kSecAttrKeyTypeECSECPrimeRandom,
      kSecAttrKeySizeInBits as String: 256,
      kSecPrivateKeyAttrs as String: [
        kSecAttrIsPermanent as String: true,
        kSecAttrApplicationTag as String: tag,
        kSecAttrAccessControl as String: access,
      ] as [String: Any],
    ]
    #if !targetEnvironment(simulator)
    attrs[kSecAttrTokenID as String] = kSecAttrTokenIDSecureEnclave
    #endif
    guard let key = SecKeyCreateRandomKey(attrs as CFDictionary, &error), let pub = publicKey(key) else {
      throw error?.takeRetainedValue() as Error? ?? NSError(domain: "kasaterm.approval", code: 1)
    }
    return pub
  }

  /// 사람이 취소하면 `nil`. 서명은 DER(X9.62) ECDSA P-256/SHA-256 — 관문·맥의 `ring` 이 그대로 읽는다.
  static func sign(message: Data, reason: String, done: @escaping (Result<String?, Error>) -> Void) {
    let context = LAContext()
    context.localizedReason = reason
    context.localizedCancelTitle = "취소"
    func signNow() {
      guard let key = find(context: context) else {
        return done(.failure(NSError(domain: "kasaterm.approval", code: 2, userInfo: [NSLocalizedDescriptionKey: "no_key"])))
      }
      var error: Unmanaged<CFError>?
      guard let signature = SecKeyCreateSignature(key, .ecdsaSignatureMessageX962SHA256, message as CFData, &error) as Data? else {
        let failure = error?.takeRetainedValue() as Error? ?? NSError(domain: "kasaterm.approval", code: 3)
        let code = (failure as NSError).code
        // 사람이 취소(LAError.userCancel·appCancel·systemCancel, errSecUserCanceled)면 아무것도 안 보낸다.
        if [-2, -4, -9, -128].contains(code) { return done(.success(nil)) }
        return done(.failure(failure))
      }
      done(.success(signature.base64EncodedString()))
    }
    #if targetEnvironment(simulator)
    // 시험 키는 Face ID 에 묶이지 않으니 서명 전에 Face ID 를 직접 묻는다(Features → Face ID 로 흉내).
    context.evaluatePolicy(.deviceOwnerAuthenticationWithBiometrics, localizedReason: reason) { ok, _ in
      if ok { signNow() } else { done(.success(nil)) }
    }
    #else
    DispatchQueue.global(qos: .userInitiated).async { signNow() }
    #endif
  }
}
