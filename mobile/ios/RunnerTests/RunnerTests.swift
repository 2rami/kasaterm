import UIKit
import XCTest
@testable import Runner

class RunnerTests: XCTestCase {
  @MainActor
  func testBackgroundNotificationReceipt() throws {
    let application = UIApplication.shared
    let delegate = try XCTUnwrap(application.delegate as? AppDelegate)
    let selector = NSSelectorFromString("application:didReceiveRemoteNotification:fetchCompletionHandler:")
    XCTAssertTrue(delegate.responds(to: selector))

    let done = expectation(description: "handled the APNs receipt")
    let origin = "~fixture-" + UUID().uuidString
    let handler: UIApplicationDelegate = delegate
    guard handler.application?(
      application,
      didReceiveRemoteNotification: [
        "aps": ["content-available": 1],
        "kind": "notification:read",
        "machine": origin,
        "read_ids": [701, 707],
      ],
      fetchCompletionHandler: { _ in done.fulfill() }
    ) != nil else {
      XCTFail("UIKit cannot invoke the background notification callback")
      return
    }
    wait(for: [done], timeout: 8)

    let saved = UserDefaults.standard.dictionary(forKey: "kasaReadNotifications") as? [String: [Int]]
    XCTAssertEqual(saved?[origin], [701, 707])
  }
}
