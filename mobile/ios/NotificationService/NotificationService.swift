import Intents
import UserNotifications

/// 푸시를 「학생이 보낸 메시지」 모양으로 바꾼다 — 아이콘 자리에 학생 얼굴(대화 알림).
/// 서버가 payload 에 `sender`(이름)·`avatar`(얼굴 그림 주소)를 실어 주면 그림을 받아
/// INSendMessageIntent 로 감싼다. 못 받으면 원래 알림 그대로 낸다(30초 안에 끝내야
/// 하므로 그림은 짧게 기다린다).
final class NotificationService: UNNotificationServiceExtension {
  private var handler: ((UNNotificationContent) -> Void)?
  private var content: UNMutableNotificationContent?
  private let completionLock = NSLock()

  override func didReceive(
    _ request: UNNotificationRequest,
    withContentHandler contentHandler: @escaping (UNNotificationContent) -> Void
  ) {
    handler = contentHandler
    let mutable = (request.content.mutableCopy() as? UNMutableNotificationContent)
    content = mutable
    guard let mutable else {
      finish(request.content)
      return
    }
    let info = request.content.userInfo
    // 소리는 종류로 고른다 — 파일은 앱 본체(Runner/Sounds)에 있고 서버는 "default" 만
    // 실어 보낸다. 확장이 못 돌면 그 기본음이 남는다(2026-09-17 「알림음 바꾸자」).
    mutable.sound = UNNotificationSound(
      named: UNNotificationSoundName(Self.soundFile(for: info["kind"] as? String)))
    let sender = (info["sender"] as? String).flatMap { $0.isEmpty ? nil : $0 }
    let avatar = (info["avatar"] as? String).flatMap(URL.init(string:))
    guard let sender else {
      finish(mutable)
      return
    }
    fetch(avatar) { data in
      self.finish(Self.asMessage(mutable, from: sender, image: data, thread: mutable.threadIdentifier))
    }
  }

  /// 기다림(승인·질문)은 또렷한 상승 3음, 끝냄은 부드러운 2음, 나머지(쪽지·주소)는 가벼운 1음.
  static func soundFile(for kind: String?) -> String {
    let k = kind ?? ""
    if k.hasPrefix("waiting") { return "kasa-wait.caf" }
    if k == "done" { return "kasa-done.caf" }
    return "kasa-note.caf"
  }

  override func serviceExtensionTimeWillExpire() {
    if let content {
      finish(content)
    }
  }

  // 다운로드 완료와 만료 콜백이 겹쳐도 같은 알림을 두 번 완료하지 않는다.
  private func finish(_ content: UNNotificationContent) {
    completionLock.lock()
    let callback = handler
    handler = nil
    completionLock.unlock()
    callback?(content)
  }

  private func fetch(_ url: URL?, _ done: @escaping (Data?) -> Void) {
    guard let url else {
      done(nil)
      return
    }
    var req = URLRequest(url: url)
    req.timeoutInterval = 8
    URLSession.shared.dataTask(with: req) { data, resp, _ in
      let ok = (resp as? HTTPURLResponse)?.statusCode == 200
      done(ok ? data : nil)
    }.resume()
  }

  private static func asMessage(
    _ content: UNMutableNotificationContent,
    from sender: String,
    image: Data?,
    thread: String?
  ) -> UNNotificationContent {
    let person = INPerson(
      personHandle: INPersonHandle(value: sender, type: .unknown),
      nameComponents: nil,
      displayName: sender,
      image: image.map { INImage(imageData: $0) },
      contactIdentifier: nil,
      customIdentifier: sender
    )
    let intent = INSendMessageIntent(
      recipients: nil,
      outgoingMessageType: .outgoingMessageText,
      content: content.body,
      speakableGroupName: nil,
      conversationIdentifier: thread.flatMap { $0.isEmpty ? nil : $0 } ?? sender,
      serviceName: nil,
      sender: person,
      attachments: nil
    )
    let interaction = INInteraction(intent: intent, response: nil)
    interaction.direction = .incoming
    interaction.donate(completion: nil)
    return (try? content.updating(from: intent)) ?? content
  }
}
