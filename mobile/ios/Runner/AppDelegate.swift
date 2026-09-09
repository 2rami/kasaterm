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
  private var pendingTap: [String: Any]?
  private var lastToken: String?

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

  func didInitializeImplicitFlutterEngine(_ engineBridge: FlutterImplicitEngineBridge) {
    GeneratedPluginRegistrant.register(with: engineBridge.pluginRegistry)
    let messenger = engineBridge.applicationRegistrar.messenger()
    let ch = FlutterMethodChannel(name: "kasaterm/push", binaryMessenger: messenger)
    channel = ch
    ch.setMethodCallHandler { [weak self] call, result in
      guard let self else { return }
      switch call.method {
      case "request":
        self.requestPush()
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
        UIApplication.shared.registerForRemoteNotifications()
      }
    }
  }

  /// 디버그 빌드는 샌드박스 서버, TestFlight·앱스토어는 본 서버 — 서버가 갈라 쏜다.
  private static var env: String {
    #if DEBUG
      return "dev"
    #else
      return "prod"
    #endif
  }

  override func application(
    _ application: UIApplication,
    didRegisterForRemoteNotificationsWithDeviceToken deviceToken: Data
  ) {
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
    completionHandler([.banner, .list, .sound])
  }

  override func userNotificationCenter(
    _ center: UNUserNotificationCenter,
    didReceive response: UNNotificationResponse,
    withCompletionHandler completionHandler: @escaping () -> Void
  ) {
    let tap = Self.payload(response.notification.request.content.userInfo)
    if let ch = channel {
      ch.invokeMethod("onTap", arguments: tap)
    } else {
      pendingTap = tap
    }
    completionHandler()
  }
}
