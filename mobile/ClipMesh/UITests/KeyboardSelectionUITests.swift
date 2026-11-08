import XCTest
import UIKit

/// R3 and R4 against real host fields: selects a long passage in another app,
/// then checks that Copy states how much it copied, that Cut removes nothing
/// and leaves both clipboards alone, and that Paste inserts what Copy sent.
/// iOS reports only part of a long selection to a keyboard, and nothing from
/// a Safari text area, so the checks do not expect the whole selection.
///
/// Turns on the ClipMesh keyboard and Full Access in Settings, so it runs only
/// when `CLIPMESH_TEST_KEYBOARD=1` and a dedicated test hub are supplied.
@MainActor
final class KeyboardSelectionUITests: XCTestCase {
    private let sentinel = "clipboard before the keyboard tap"
    private let caveat = " This may be only part of your selection, or differ in spacing."
    private let cutRefused = "Didn't cut: couldn't confirm the whole selection. Copy still works."
    private let noSelection = "No selected text shared with the keyboard"

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

    /// R3 long Copy, long Cut and Paste, in a native multi-line field.
    func testNativeTextFieldCopyStatesItsLengthAndCutRefuses() throws {
        try prepareKeyboard()
        let passage = Self.passage(label: "native")
        let (contacts, notes) = try nativeNotesField(holding: passage)
        UIPasteboard.general.string = sentinel
        menu("Select All", in: contacts, on: notes)
        let keyboard = try switchToClipMeshKeyboard(in: contacts)

        keyboard.buttons["keyboard.copy"].tap()
        let status = waitForStatus(matching: "Copied [0-9,]+ characters? to iPhone and ClipMesh\\.\(NSRegularExpression.escapedPattern(for: caveat))",
                                   in: keyboard)
        let copied = try XCTUnwrap(Int(status.split(separator: " ")[1].replacing(",", with: "")))
        XCTAssertEqual(notes.value as? String, passage, "Copy changed the field")

        UIPasteboard.general.string = sentinel
        keyboard.buttons["keyboard.cut"].tap()
        waitForStatus(cutRefused, in: keyboard)
        XCTAssertEqual(notes.value as? String, passage, "Cut removed text")
        print("KEYBOARD_R3 app=native passage=\(passage.count) copied=\(copied) cut=refused field=unchanged")

        // Cut left the selection whole, and pasting the iPhone clipboard over
        // it shows Cut did not write the clipboard.
        menu("Paste", in: contacts, on: notes)
        XCTAssertEqual(notes.value as? String, sentinel, "Cut changed the iPhone clipboard or the selection")

        // The newest ClipMesh clip is what Copy reported, at the length it stated.
        menu("Select All", in: contacts, on: notes)
        keyboard.buttons["keyboard.paste"].tap()
        waitForStatus("Pasted from ClipMesh", in: keyboard)
        let pasted = notes.value as? String ?? ""
        XCTAssertEqual(pasted.count, copied, "ClipMesh's newest clip is not the text Copy reported")
        XCTAssertTrue(passage.hasPrefix(String(pasted.prefix(20))), "ClipMesh's newest clip is not from the selection")
        print("KEYBOARD_R3 app=native paste=newest-clip characters=\(pasted.count)")
    }

    /// R3 repeated text: Cut refuses whatever the selection repeats.
    func testCutRefusesRepeatedTextInANativeTextField() throws {
        try prepareKeyboard()
        let passage = String(repeating: "Words keep the selection long enough to test. ", count: 40)
        let (contacts, notes) = try nativeNotesField(holding: passage)
        UIPasteboard.general.string = sentinel
        menu("Select All", in: contacts, on: notes)
        let keyboard = try switchToClipMeshKeyboard(in: contacts)
        keyboard.buttons["keyboard.cut"].tap()
        waitForStatus(cutRefused, in: keyboard)
        XCTAssertEqual(notes.value as? String, passage, "Cut removed text")
        menu("Paste", in: contacts, on: notes)
        XCTAssertEqual(notes.value as? String, sentinel, "Cut changed the iPhone clipboard or the selection")
        print("KEYBOARD_R3 app=native-repeated passage=\(passage.count) cut=refused field=unchanged clipboard=unchanged")
    }

    /// R4: a Safari text area shares no selection with the keyboard.
    func testSafariTextAreaSharesNoSelection() throws {
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

        UIPasteboard.general.string = sentinel
        menu("Select All", in: safari, on: area)
        let keyboard = try switchToClipMeshKeyboard(in: safari)
        keyboard.buttons["keyboard.cut"].tap()
        waitForStatus(noSelection, in: keyboard)
        keyboard.buttons["keyboard.copy"].tap()
        waitForStatus(noSelection, in: keyboard)
        XCTAssertEqual(area.value as? String, passage, "Cut or Copy changed the text area")
        menu("Paste", in: safari, on: area)
        XCTAssertEqual(area.value as? String, sentinel, "Cut or Copy changed the iPhone clipboard")
        print("KEYBOARD_R4 app=safari passage=\(passage.count) cut=no-selection copy=no-selection clipboard=unchanged")
    }

    // MARK: - Steps

    /// A new contact's Notes field in Contacts, holding `text`.
    private func nativeNotesField(holding text: String) throws -> (XCUIApplication, XCUIElement) {
        let contacts = XCUIApplication(bundleIdentifier: "com.apple.MobileAddressBook")
        contacts.terminate()
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
        UIPasteboard.general.string = text
        menu("Paste", in: contacts, on: notes)
        XCTAssertEqual(notes.value as? String, text)
        return (contacts, notes)
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
        wait(for: NSPredicate(format: "label == %@", text), described: text, in: keyboard)
    }

    @discardableResult
    private func waitForStatus(matching pattern: String, in keyboard: XCUIElement) -> String {
        wait(for: NSPredicate(format: "label MATCHES %@", pattern), described: pattern, in: keyboard)
    }

    @discardableResult
    private func wait(for predicate: NSPredicate, described: String, in keyboard: XCUIElement) -> String {
        let status = keyboard.staticTexts["keyboard.status"]
        let met = XCTNSPredicateExpectation(predicate: predicate, object: status)
        XCTAssertEqual(XCTWaiter().wait(for: [met], timeout: 15), .completed,
                       "status read \(status.exists ? status.label : "nothing"), wanted \(described)")
        return status.label
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
