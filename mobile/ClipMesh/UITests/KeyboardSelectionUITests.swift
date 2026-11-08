import XCTest
import UIKit

/// AS2: when the keyboard reads a non-empty selection, it is the whole
/// selection. Selects a long multi-paragraph passage in another app, taps
/// Copy, and checks the clipboard against the passage by pasting it back.
///
/// Turns on the ClipMesh keyboard and Full Access in Settings, so it runs only
/// when `CLIPMESH_TEST_KEYBOARD=1` and a dedicated test hub are supplied.
@MainActor
final class KeyboardSelectionUITests: XCTestCase {
    private let sentinel = "clipboard before the keyboard tap"

    override func setUpWithError() throws {
        try super.setUpWithError()
        continueAfterFailure = false
        XCUIDevice.shared.orientation = .portrait
        let environment = ProcessInfo.processInfo.environment
        guard environment["CLIPMESH_TEST_HUB_URL"]?.hasPrefix("ws://") == true,
              environment["CLIPMESH_TEST_KEYBOARD"] == "1"
        else {
            throw XCTSkip("Requires CLIPMESH_TEST_KEYBOARD=1 and an isolated test hub")
        }
    }

    func testCopyTakesTheWholeSelectionInANativeTextField() throws {
        try prepareKeyboard()
        let passage = Self.passage(label: "native")
        let contacts = XCUIApplication(bundleIdentifier: "com.apple.MobileAddressBook")
        contacts.launch()
        // Add sits in the navigation bar or, on newer systems, beside Search.
        let add = contacts.buttons.matching(NSPredicate(format: "identifier == 'Add' OR label ==[c] 'Add'")).firstMatch
        XCTAssertTrue(add.waitForExistence(timeout: 10), contacts.debugDescription)
        add.tap()
        let notes = contacts.textViews["Notes"].firstMatch
        XCTAssertTrue(notes.waitForExistence(timeout: 10), contacts.debugDescription)
        // The new contact form is a sheet; swipe the form, not the app.
        scrollTo(notes, in: contacts, swiping: contacts.tables.firstMatch)
        notes.tap()
        UIPasteboard.general.string = passage
        menu("Paste", in: contacts, on: notes)
        XCTAssertEqual(notes.value as? String, passage)
        try checkCopyCutAndPaste(passage, field: notes, app: contacts)
    }

    func testCopyTakesTheWholeSelectionInASafariTextArea() throws {
        try prepareKeyboard()
        let passage = Self.passage(label: "web")
        let page = "<meta name=viewport content='width=device-width'>"
            + "<textarea style='width:95%;height:60vh'>\(passage)</textarea>"
        let url = try XCTUnwrap(URL(string: "data:text/html;charset=utf-8;base64,"
            + Data(page.utf8).base64EncodedString()))
        let safari = XCUIApplication(bundleIdentifier: "com.apple.mobilesafari")
        safari.launch()
        safari.open(url)
        let area = safari.webViews.textViews.firstMatch
        XCTAssertTrue(area.waitForExistence(timeout: 20), safari.debugDescription)
        area.tap()
        XCTAssertEqual(area.value as? String, passage)
        try checkCopyCutAndPaste(passage, field: area, app: safari)
    }

    // MARK: - Steps

    /// Copy, then paste the clipboard over the selection; Cut, then paste it
    /// into the emptied field; then the keyboard's Paste from ClipMesh. Each
    /// leaves the field equal to the passage only if the keyboard took the
    /// whole selection.
    private func checkCopyCutAndPaste(_ passage: String, field: XCUIElement, app: XCUIApplication) throws {
        UIPasteboard.general.string = sentinel
        menu("Select All", in: app, on: field)
        let keyboard = try switchToClipMeshKeyboard(in: app)
        keyboard.buttons["keyboard.copy"].tap()
        waitForStatus("Copied to iPhone and ClipMesh", in: keyboard)
        // Copy leaves the whole passage selected, so the menu has no Select All.
        // Pasting over it gives back the passage only if Copy took all of it.
        menu("Paste", in: app, on: field)
        XCTAssertEqual(field.value as? String, passage, "Copy put something other than the selection on the clipboard")
        print("KEYBOARD_AS2 copy app=\(app.identifier) characters=\(passage.count) clipboard=whole-selection")

        UIPasteboard.general.string = sentinel
        menu("Select All", in: app, on: field)
        keyboard.buttons["keyboard.cut"].tap()
        waitForStatus("Cut to iPhone and ClipMesh", in: keyboard)
        XCTAssertNotEqual(field.value as? String, passage)
        XCTAssertFalse((field.value as? String ?? "").contains("paragraph"), "Cut left part of the selection behind")
        menu("Paste", in: app, on: field)
        XCTAssertEqual(field.value as? String, passage, "Cut put something other than the selection on the clipboard")

        menu("Select All", in: app, on: field)
        keyboard.buttons["keyboard.paste"].tap()
        waitForStatus("Pasted from ClipMesh", in: keyboard)
        XCTAssertEqual(field.value as? String, passage, "ClipMesh's newest clip differs from the selection")
        print("KEYBOARD_AS2 cut-paste app=\(app.identifier) characters=\(passage.count) hub=whole-selection")
    }

    /// Saves the test hub URL in the app, then adds the keyboard and allows
    /// Full Access in Settings if they are not already set.
    private func prepareKeyboard() throws {
        let app = XCUIApplication()
        app.launchArguments = ["-hub_url", ProcessInfo.processInfo.environment["CLIPMESH_TEST_HUB_URL"] ?? ""]
        app.launch()
        XCTAssertTrue(app.wait(for: .runningForeground, timeout: 10))
        sleep(2)
        app.terminate()

        let settings = XCUIApplication(bundleIdentifier: "com.apple.Preferences")
        settings.terminate()
        settings.launch()
        open("General", in: settings)
        open("Keyboard", in: settings)
        open("Keyboards", in: settings)
        let entry = settings.cells.containing(.staticText, identifier: "ClipMesh").firstMatch
        if !entry.waitForExistence(timeout: 3) {
            settings.cells["AddNewKeyboard"].tap()
            let choice = settings.cells.staticTexts["ClipMesh"].firstMatch
            scrollTo(choice, in: settings)
            choice.tap()
        }
        XCTAssertTrue(entry.waitForExistence(timeout: 5), settings.debugDescription)
        entry.tap()
        let access = settings.switches.firstMatch
        XCTAssertTrue(access.waitForExistence(timeout: 5), settings.debugDescription)
        if access.value as? String != "1" {
            // The row is one wide switch; its toggle is a switch inside it.
            let toggle = access.switches.firstMatch
            (toggle.exists ? toggle : access).tap()
            let allow = settings.alerts.buttons["Allow"]
            if allow.waitForExistence(timeout: 5) { allow.tap() }
        }
        XCTAssertEqual(access.value as? String, "1")
        settings.terminate()
    }

    /// Taps a Settings row by its title, scrolling to it first.
    private func open(_ title: String, in settings: XCUIApplication) {
        let row = settings.cells.staticTexts[title].firstMatch
        XCTAssertTrue(row.waitForExistence(timeout: 5), settings.debugDescription)
        scrollTo(row, in: settings)
        row.tap()
    }

    /// Cycles the globe key until the ClipMesh keyboard is up. Returns the app,
    /// since neither the globe key nor the extension's keys sit inside the
    /// system's Keyboard element on every system version.
    private func switchToClipMeshKeyboard(in app: XCUIApplication) throws -> XCUIElement {
        let keyboard: XCUIElement = app
        for _ in 0..<6 {
            if keyboard.buttons["keyboard.copy"].exists {
                return keyboard
            }
            let globe = keyboard.buttons.matching(NSPredicate(format: "label ==[c] 'Next keyboard'")).firstMatch
            XCTAssertTrue(globe.waitForExistence(timeout: 5), app.debugDescription)
            globe.tap()
            _ = keyboard.buttons["keyboard.copy"].waitForExistence(timeout: 2)
        }
        XCTAssertTrue(keyboard.buttons["keyboard.copy"].exists, app.debugDescription)
        return keyboard
    }

    private func waitForStatus(_ text: String, in keyboard: XCUIElement) {
        let status = keyboard.staticTexts["keyboard.status"]
        let predicate = NSPredicate(format: "label == %@", text)
        let met = XCTNSPredicateExpectation(predicate: predicate, object: status)
        XCTAssertEqual(XCTWaiter().wait(for: [met], timeout: 15), .completed,
                       "status read \(status.exists ? status.label : "nothing"), wanted \(text)")
    }

    /// Opens the field's edit menu and chooses an item. For Select All, a tap
    /// on the cursor opens the menu, while a long press in a web view selects
    /// a word and drops Select All from it, so the press comes last.
    private func menu(_ item: String, in app: XCUIApplication, on field: XCUIElement) {
        let choice = app.menuItems[item]
        let opens: [() -> Void] = item == "Select All"
            ? [{ field.tap() }, { field.tap() }, { field.press(forDuration: 1.0) }]
            : [{ field.press(forDuration: 1.0) }, { field.tap() }]
        for _ in 0..<3 where !choice.exists {
            for open in opens where !choice.exists {
                open()
                _ = choice.waitForExistence(timeout: 2)
            }
        }
        XCTAssertTrue(choice.exists, "no \(item) in the edit menu: \(app.debugDescription)")
        choice.tap()
    }

    private func scrollTo(_ element: XCUIElement, in app: XCUIApplication, swiping surface: XCUIElement? = nil) {
        for _ in 0..<8 where !(element.exists && element.isHittable) {
            if let surface {
                // A slow drag from just above the keyboard; a swipe does not move the sheet.
                let start = surface.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.55))
                start.press(forDuration: 0.1, thenDragTo: surface.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.1)),
                            withVelocity: .slow, thenHoldForDuration: 0.3)
            } else {
                app.swipeUp()
            }
        }
        XCTAssertTrue(element.isHittable, app.debugDescription)
    }

    /// A passage of more than 5,000 characters over many paragraphs, with
    /// tabs and characters outside ASCII. Free of HTML markup characters so it
    /// can sit inside a text area as written.
    static func passage(label: String) -> String {
        let paragraphs = (1...24).map { index in
            "\(label) paragraph \(index)\tcafé 🧪 naïve\n"
                + String(repeating: "Words keep the selection long enough to test. ", count: 5)
        }
        let text = paragraphs.joined(separator: "\n\n")
        precondition(text.count >= 5_000)
        return text
    }
}
