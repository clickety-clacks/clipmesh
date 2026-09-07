import XCTest
import UIKit
import UniformTypeIdentifiers

@MainActor
final class ExplicitClipboardUITests: XCTestCase {
    func testImagePreviewSendAndThumbnailCopy() throws {
        let endpoint = ProcessInfo.processInfo.environment["CLIPMESH_TEST_HUB_URL"] ?? ""
        guard endpoint.hasPrefix("ws://") else { throw XCTSkip("Requires an isolated test hub") }
        continueAfterFailure = false
        let name = "ui-" + UUID().uuidString + ".png"
        let png = UIGraphicsImageRenderer(size: CGSize(width: 100, height: 60)).pngData { context in
            UIColor.orange.setFill()
            context.fill(CGRect(x: 0, y: 0, width: 100, height: 60))
        }
        let provider = NSItemProvider()
        provider.suggestedName = name
        provider.registerDataRepresentation(forTypeIdentifier: UTType.png.identifier, visibility: .all) { completion in
            completion(png, nil)
            return nil
        }
        UIPasteboard.general.setItemProviders([provider], localOnly: true, expirationDate: nil)
        let baseline = UIPasteboard.general.changeCount
        let app = XCUIApplication()
        app.launchArguments = ["-hub_url", endpoint]
        app.launch()
        let springboard = XCUIApplication(bundleIdentifier: "com.apple.springboard")
        let permission = springboard.alerts.buttons["Allow Paste"]
        if permission.waitForExistence(timeout: 5) { permission.tap() }
        let send = app.buttons["copyToClipMesh"]
        XCTAssertTrue(send.waitForExistence(timeout: 15))
        expectation(for: NSPredicate(format: "enabled == true"), evaluatedWith: send)
        waitForExpectations(timeout: 15)
        XCTAssertTrue(send.label.contains(name))
        XCTAssertEqual(UIPasteboard.general.changeCount, baseline)
        send.tap()
        XCTAssertTrue(app.staticTexts["Sent to ClipMesh"].waitForExistence(timeout: 15))
        let thumbnail = app.buttons["copyFileThumbnail-" + name]
        XCTAssertTrue(thumbnail.waitForExistence(timeout: 20))
        XCTAssertEqual(UIPasteboard.general.changeCount, baseline, "Arrival must not write the clipboard")
        thumbnail.tap()
        XCTAssertTrue(app.staticTexts["Copied to device clipboard"].waitForExistence(timeout: 5))
        XCTAssertEqual(UIPasteboard.general.changeCount, baseline + 1)
        let screenshot = XCTAttachment(screenshot: app.screenshot())
        screenshot.name = "File preview and chronological clippings"
        screenshot.lifetime = .keepAlways
        add(screenshot)
        XCUIDevice.shared.orientation = .landscapeLeft
        defer { XCUIDevice.shared.orientation = .portrait }
        expectation(for: NSPredicate { _, _ in app.frame.width > app.frame.height }, evaluatedWith: app)
        waitForExpectations(timeout: 10)
        XCTAssertTrue(send.isHittable)
        XCTAssertTrue(app.staticTexts["Live"].exists)
        XCTAssertEqual(UIPasteboard.general.changeCount, baseline + 1, "Rotation must not write the clipboard")
        let landscape = XCTAttachment(screenshot: app.screenshot())
        landscape.name = "Landscape clipping list"
        landscape.lifetime = .keepAlways
        add(landscape)
        app.terminate()
    }

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
        let springboard = XCUIApplication(bundleIdentifier: "com.apple.springboard")
        let permission = springboard.alerts.buttons["Allow Paste"]
        if permission.waitForExistence(timeout: 5) { permission.tap() }
        let send = app.buttons["copyToClipMesh"]
        XCTAssertTrue(send.waitForExistence(timeout: 10))
        let enabled = NSPredicate(format: "enabled == true")
        expectation(for: enabled, evaluatedWith: send)
        waitForExpectations(timeout: 15)
        XCTAssertEqual(UIPasteboard.general.changeCount, baseline, "Launching must not write the clipboard")
        send.tap()
        if permission.waitForExistence(timeout: 5) { permission.tap() }
        XCTAssertTrue(app.staticTexts["Copied to ClipMesh"].waitForExistence(timeout: 15))
        let preview = app.buttons["latestClip"]
        XCTAssertTrue(preview.waitForExistence(timeout: 10))
        XCTAssertTrue(preview.label.contains(sentText))
        XCTAssertEqual(UIPasteboard.general.changeCount, baseline, "Sending and the echoed clip must not write the clipboard")
        UIPasteboard.general.string = "Unrelated local clipboard after sending"
        app.terminate()
        app.launch()
        if permission.waitForExistence(timeout: 5) { permission.tap() }
        XCTAssertTrue(preview.waitForExistence(timeout: 10))
        XCTAssertTrue(preview.label.contains(sentText))
        XCTAssertEqual(UIPasteboard.general.changeCount, baseline + 1, "Reopening with shared history must preserve local clipboard")
        preview.tap()
        XCTAssertTrue(app.staticTexts["Copied to device clipboard"].waitForExistence(timeout: 5))
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
