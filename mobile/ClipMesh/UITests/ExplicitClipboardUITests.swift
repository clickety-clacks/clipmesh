import XCTest
import UIKit

@MainActor
final class ExplicitClipboardUITests: XCTestCase {
    func testExplicitSendAndReceiveAgainstLiveHub() throws {
        let endpoint = ProcessInfo.processInfo.environment["CLIPMESH_TEST_HUB_URL"] ?? ""
        guard endpoint.hasPrefix("ws://") else {
            throw XCTSkip("Set CLIPMESH_TEST_HUB_URL to a dedicated test hub. This test publishes synthetic text.")
        }
        continueAfterFailure = false
        let sentText = "ClipMesh UI test \(UUID().uuidString)"
        UIPasteboard.general.string = sentText
        let baseline = UIPasteboard.general.changeCount
        let app = XCUIApplication()
        app.launchArguments = ["-hub_url", endpoint]
        addUIInterruptionMonitor(withDescription: "Paste permission") { alert in
            if alert.buttons["Allow Paste"].exists {
                alert.buttons["Allow Paste"].tap()
                return true
            }
            if alert.buttons["Allow"].exists {
                alert.buttons["Allow"].tap()
                return true
            }
            return false
        }
        app.launch()
        let send = app.buttons["copyToClipMesh"]
        XCTAssertTrue(send.waitForExistence(timeout: 10))
        let enabled = NSPredicate(format: "enabled == true")
        expectation(for: enabled, evaluatedWith: send)
        waitForExpectations(timeout: 15)
        XCTAssertEqual(UIPasteboard.general.changeCount, baseline, "Launching must not write the clipboard")
        send.tap()
        let springboard = XCUIApplication(bundleIdentifier: "com.apple.springboard")
        let permission = springboard.alerts.buttons["Allow Paste"]
        if permission.waitForExistence(timeout: 5) { permission.tap() }
        XCTAssertTrue(app.staticTexts["Copied to ClipMesh"].waitForExistence(timeout: 15))
        let preview = app.buttons["latestClip"]
        XCTAssertTrue(preview.waitForExistence(timeout: 10))
        XCTAssertTrue(preview.label.contains(sentText))
        XCTAssertEqual(UIPasteboard.general.changeCount, baseline, "Sending and the echoed clip must not write the clipboard")
        UIPasteboard.general.string = "Unrelated local clipboard after sending"
        app.terminate()
        app.launch()
        XCTAssertTrue(preview.waitForExistence(timeout: 10))
        XCTAssertTrue(preview.label.contains(sentText))
        XCTAssertEqual(UIPasteboard.general.changeCount, baseline + 1, "Reopening with shared history must preserve local clipboard")
        preview.tap()
        XCTAssertTrue(app.staticTexts["Copied to iPhone clipboard"].waitForExistence(timeout: 5))
        XCTAssertEqual(UIPasteboard.general.changeCount, baseline + 2)
        let provider = try XCTUnwrap(UIPasteboard.general.itemProviders.first)
        let readBack = expectation(description: "Read the clipboard written by tapping the preview")
        provider.loadObject(ofClass: NSString.self) { value, error in
            XCTAssertNil(error)
            XCTAssertEqual(value as? String, sentText)
            readBack.fulfill()
        }
        if permission.waitForExistence(timeout: 5) { permission.tap() }
        waitForExpectations(timeout: 10)
        let screenshot = XCTAttachment(screenshot: app.screenshot())
        screenshot.name = "Explicit clipboard controls with live hub"
        screenshot.lifetime = .keepAlways
        add(screenshot)
        app.terminate()
    }
}
