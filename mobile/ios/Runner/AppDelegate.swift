import AuthenticationServices
import Flutter
import UIKit
import UserNotifications

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

  override func application(
    _ application: UIApplication,
    didFinishLaunchingWithOptions launchOptions: [UIApplication.LaunchOptionsKey: Any]?
  ) -> Bool {
    UNUserNotificationCenter.current().delegate = self
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
        if self.graceTask == .invalid {
          self.graceTask = UIApplication.shared.beginBackgroundTask(withName: "kasaterm.grace") { [weak self] in
            self?.backgroundChannel?.invokeMethod("expired", arguments: nil)
            self?.endGrace()
          }
        }
        result(self.graceTask != .invalid)
      case "end":
        self.endGrace()
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
    for key in ["machine", "pane", "kind"] {
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
    completionHandler(pushEnabled ? [.banner, .list, .sound] : [])
  }

  override func userNotificationCenter(
    _ center: UNUserNotificationCenter,
    didReceive response: UNNotificationResponse,
    withCompletionHandler completionHandler: @escaping () -> Void
  ) {
    guard pushEnabled else { completionHandler(); return }
    let tap = Self.payload(response.notification.request.content.userInfo)
    if let ch = channel {
      ch.invokeMethod("onTap", arguments: tap)
    } else {
      pendingTap = tap
    }
    completionHandler()
  }
}

extension AppDelegate: ASWebAuthenticationPresentationContextProviding {
  func presentationAnchor(for session: ASWebAuthenticationSession) -> ASPresentationAnchor {
    let windows = UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }.flatMap(\.windows)
    return windows.first(where: \.isKeyWindow) ?? windows.first ?? ASPresentationAnchor()
  }
}
