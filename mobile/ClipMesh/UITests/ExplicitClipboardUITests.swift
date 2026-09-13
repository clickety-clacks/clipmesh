import XCTest
import UIKit
import UniformTypeIdentifiers

@MainActor
final class ExplicitClipboardUITests: XCTestCase {
    override func setUpWithError() throws {
        try super.setUpWithError()
        XCUIDevice.shared.orientation = .portrait
    }

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
        let sentFeedback = app.staticTexts["clipboardFeedback"]
        XCTAssertTrue(sentFeedback.waitForExistence(timeout: 15))
        XCTAssertEqual(sentFeedback.label, "Sent to ClipMesh")
        let thumbnail = app.buttons["copyFileThumbnail-" + name]
        XCTAssertTrue(thumbnail.waitForExistence(timeout: 20))
        XCTAssertEqual(UIPasteboard.general.changeCount, baseline, "Arrival must not write the clipboard")
        thumbnail.tap()
        let copiedFeedback = app.staticTexts["fileFeedback"]
        expectation(for: NSPredicate(format: "label == %@", "Copied to device clipboard"), evaluatedWith: copiedFeedback)
        waitForExpectations(timeout: 5)
        XCTAssertEqual(UIPasteboard.general.changeCount, baseline + 1)
        let screenshot = XCTAttachment(screenshot: app.screenshot())
        screenshot.name = "File preview and chronological clippings"
        screenshot.lifetime = .keepAlways
        add(screenshot)
        XCUIDevice.shared.orientation = .landscapeLeft
        defer { XCUIDevice.shared.orientation = .portrait }
        expectation(for: NSPredicate { _, _ in app.frame.width > app.frame.height }, evaluatedWith: app)
        waitForExpectations(timeout: 10)
        scrollHistoryToTop(app)
        XCTAssertTrue(send.isHittable)
        let status = app.staticTexts["connectionStatus"].firstMatch
        XCTAssertTrue(status.waitForExistence(timeout: 10))
        XCTAssertTrue(status.label.contains("Live"))
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
        scrollHistoryToTop(app)
        let preview = app.buttons["latestClip"]
        XCTAssertTrue(preview.waitForExistence(timeout: 10))
        XCTAssertTrue(preview.label.contains(sentText))
        XCTAssertEqual(UIPasteboard.general.changeCount, baseline, "Sending and the echoed clip must not write the clipboard")
        UIPasteboard.general.string = "Unrelated local clipboard after sending"
        app.terminate()
        app.launch()
        if permission.waitForExistence(timeout: 5) { permission.tap() }
        scrollHistoryToTop(app)
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

    func testNotesTextWinsOverWebArchiveOnSend() throws {
        let endpoint = ProcessInfo.processInfo.environment["CLIPMESH_TEST_HUB_URL"] ?? ""
        guard endpoint.hasPrefix("ws://") else {
            throw XCTSkip("Requires an isolated test hub")
        }
        continueAfterFailure = false
        let sentText = "Notes UI test " + UUID().uuidString
        let archiveName = "notes-webarchive-" + UUID().uuidString + ".webarchive"
        let provider = NSItemProvider(object: sentText as NSString)
        provider.suggestedName = archiveName
        provider.registerDataRepresentation(forTypeIdentifier: "com.apple.webarchive", visibility: .all) { completion in
            completion(Data("web archive should not be sent".utf8), nil)
            return nil
        }
        UIPasteboard.general.setItemProviders([provider], localOnly: true, expirationDate: nil)
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
        XCTAssertTrue(send.waitForExistence(timeout: 15))
        expectation(for: NSPredicate(format: "enabled == true"), evaluatedWith: send)
        waitForExpectations(timeout: 15)
        XCTAssertTrue(send.label.contains(sentText))
        XCTAssertEqual(UIPasteboard.general.changeCount, baseline)
        send.tap()
        if permission.waitForExistence(timeout: 5) { permission.tap() }
        XCTAssertTrue(app.staticTexts["Copied to ClipMesh"].waitForExistence(timeout: 15))
        scrollHistoryToTop(app)
        let preview = app.buttons["latestClip"]
        XCTAssertTrue(preview.waitForExistence(timeout: 10))
        XCTAssertTrue(preview.label.contains(sentText))
        let archiveRows = app.staticTexts.matching(NSPredicate(format: "label CONTAINS[c] %@", archiveName))
        XCTAssertEqual(archiveRows.count, 0)
        XCTAssertEqual(UIPasteboard.general.changeCount, baseline)
        app.terminate()
    }

    func testNativeSearchFindsFullTextAndMachineName() throws {
        let endpoint = ProcessInfo.processInfo.environment["CLIPMESH_TEST_HUB_URL"] ?? ""
        guard endpoint.hasPrefix("ws://") else { throw XCTSkip("Requires an isolated test hub") }
        continueAfterFailure = false
        let marker = "search-tail-" + UUID().uuidString
        let longText = String(repeating: "history prefix ", count: 20) + marker
        UIPasteboard.general.string = longText
        let app = XCUIApplication()
        app.launchArguments = ["-hub_url", endpoint]
        addUIInterruptionMonitor(withDescription: "Paste permission") { alert in
            if alert.buttons["Allow Paste"].exists { alert.buttons["Allow Paste"].tap(); return true }
            if alert.buttons["Allow"].exists { alert.buttons["Allow"].tap(); return true }
            return false
        }
        app.launch()
        let springboard = XCUIApplication(bundleIdentifier: "com.apple.springboard")
        let permission = springboard.alerts.buttons["Allow Paste"]
        if permission.waitForExistence(timeout: 5) { permission.tap() }
        let send = app.buttons["copyToClipMesh"]
        XCTAssertTrue(send.waitForExistence(timeout: 15))
        expectation(for: NSPredicate(format: "enabled == true"), evaluatedWith: send)
        waitForExpectations(timeout: 15)
        send.tap()
        XCTAssertTrue(app.staticTexts["Copied to ClipMesh"].waitForExistence(timeout: 15))

        let search = app.searchFields.firstMatch
        XCTAssertTrue(search.waitForExistence(timeout: 10))
        search.tap()
        search.typeText(marker)
        XCTAssertTrue(app.buttons["latestClip"].waitForExistence(timeout: 10))
        search.buttons["Clear text"].tap()
        let sourceName = ProcessInfo.processInfo.environment["CLIPMESH_TEST_SOURCE_NAME"] ?? "eezo"
        search.typeText(sourceName)
        XCTAssertTrue(app.staticTexts["From \(sourceName)"].waitForExistence(timeout: 10))
        app.terminate()
    }

    private func scrollHistoryToTop(_ app: XCUIApplication) {
        let collection = app.collectionViews.firstMatch
        if collection.waitForExistence(timeout: 5) {
            for _ in 0 ..< 6 { collection.swipeDown() }
            return
        }
        let scrollView = app.scrollViews.firstMatch
        guard scrollView.waitForExistence(timeout: 5) else { return }
        for _ in 0 ..< 6 { scrollView.swipeDown() }
    }
}
