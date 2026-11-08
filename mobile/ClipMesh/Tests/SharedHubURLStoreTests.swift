@testable import ClipMesh
import XCTest

@MainActor
final class SharedHubURLStoreTests: XCTestCase {
    private let appSuite = "org.example.ClipMeshTests.app-preferences"
    private let sharedSuite = "org.example.ClipMeshTests.shared-hub-url"

    override func setUp() async throws {
        UserDefaults().removePersistentDomain(forName: appSuite)
        UserDefaults().removePersistentDomain(forName: sharedSuite)
    }

    override func tearDown() async throws {
        UserDefaults().removePersistentDomain(forName: appSuite)
        UserDefaults().removePersistentDomain(forName: sharedSuite)
    }

    func testAURLSavedBeforeTheKeyboardReachesItWhenTheAppOpens() async throws {
        let appDefaults = try XCTUnwrap(UserDefaults(suiteName: appSuite))
        ProtectedPreferences(defaults: appDefaults).saveEndpoint(try ProtocolFixtures.endpoint())
        let shared = try SharedHubURLStore(defaults: XCTUnwrap(UserDefaults(suiteName: sharedSuite)))
        XCTAssertNil(shared.load())

        let upgraded = ProtectedPreferences(defaults: appDefaults, sharedHubURL: shared)
        _ = MobileSessionModel(
            transport: SyntheticClipTransport(),
            pasteboard: SyntheticPasteboardWriter(),
            preferences: upgraded,
        )

        XCTAssertEqual(shared.load(), try ProtocolFixtures.endpoint())
    }

    func testSavingANewURLUpdatesTheKeyboardAndSharesNothingElse() async throws {
        let sharedDefaults = try XCTUnwrap(UserDefaults(suiteName: sharedSuite))
        let shared = SharedHubURLStore(defaults: sharedDefaults)
        let preferences = try ProtectedPreferences(
            defaults: XCTUnwrap(UserDefaults(suiteName: appSuite)),
            sharedHubURL: shared,
        )
        preferences.saveEndpoint(try ProtocolFixtures.endpoint())
        let changed = try HubEndpoint("ws://100.64.0.9:8123/v1/stream")
        preferences.saveEndpoint(changed)
        preferences.saveConnectionState(.live)

        XCTAssertEqual(shared.load(), changed)
        let persisted = try XCTUnwrap(sharedDefaults.persistentDomain(forName: sharedSuite))
        XCTAssertEqual(Set(persisted.keys), SharedHubURLStore.allowedKeys)
    }

    func testTheKeyboardRejectsAURLTheAppWouldReject() async throws {
        let sharedDefaults = try XCTUnwrap(UserDefaults(suiteName: sharedSuite))
        sharedDefaults.set("ws://example.com/v1/stream", forKey: "hub_url")
        XCTAssertNil(SharedHubURLStore(defaults: sharedDefaults).load())
    }
}
