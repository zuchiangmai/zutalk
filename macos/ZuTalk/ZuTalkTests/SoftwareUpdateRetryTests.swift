// SoftwareUpdateRetryTests.swift
//
// Coverage for the two decisions that keep background updating silent: which
// aborted Sparkle cycles are worth retrying (only genuine network failures —
// signature, policy, and user-cancelled aborts must not), and when a wake or a
// reconnect is allowed to start another check against the release CDN.
//
// And for the one decision about a check the user started: a network failure
// is retried once without a word, then explained with the download page.

import Sparkle
import XCTest

@testable import ZuTalk

@MainActor
final class SoftwareUpdateRetryTests: XCTestCase {

    private func sparkleError(_ code: SUError, underlying: NSError? = nil) -> NSError {
        var userInfo: [String: Any] = [:]
        if let underlying {
            userInfo[NSUnderlyingErrorKey] = underlying
        }
        return NSError(
            domain: SUSparkleErrorDomain,
            code: Int(code.rawValue),
            userInfo: userInfo
        )
    }

    func testInterruptedDownloadAndUnreachableAppcastAreRetryable() {
        XCTAssertTrue(
            SoftwareUpdateController.isTransientNetworkFailure(
                sparkleError(
                    .downloadError,
                    underlying: NSError(
                        domain: NSURLErrorDomain,
                        code: NSURLErrorNetworkConnectionLost
                    )
                )
            )
        )
        XCTAssertTrue(
            SoftwareUpdateController.isTransientNetworkFailure(sparkleError(.appcastError))
        )
    }

    func testUserCancelledDownloadIsNotRetryable() {
        XCTAssertFalse(
            SoftwareUpdateController.isTransientNetworkFailure(
                sparkleError(
                    .downloadError,
                    underlying: NSError(domain: NSURLErrorDomain, code: NSURLErrorCancelled)
                )
            )
        )
    }

    func testNonNetworkAbortsAreNotRetryable() {
        XCTAssertFalse(
            SoftwareUpdateController.isTransientNetworkFailure(
                sparkleError(.signatureError)
            )
        )
        XCTAssertFalse(
            SoftwareUpdateController.isTransientNetworkFailure(
                sparkleError(.noUpdateError)
            )
        )
        XCTAssertFalse(
            SoftwareUpdateController.isTransientNetworkFailure(
                NSError(domain: NSURLErrorDomain, code: NSURLErrorTimedOut)
            )
        )
    }

    // MARK: - Background check throttling

    private let now = Date(timeIntervalSince1970: 1_700_000_000)

    private func shouldCheck(
        automatic: Bool = true,
        sessionInProgress: Bool = false,
        lastCheckFailedOnNetwork: Bool = false,
        secondsSinceLastCheck: TimeInterval?
    ) -> Bool {
        SoftwareUpdateController.shouldStartBackgroundCheck(
            automaticChecksEnabled: automatic,
            sessionInProgress: sessionInProgress,
            lastCheckDate: secondsSinceLastCheck.map { now.addingTimeInterval(-$0) },
            lastCheckFailedOnNetwork: lastCheckFailedOnNetwork,
            now: now
        )
    }

    func testFirstCheckOfAnInstallIsAllowed() {
        XCTAssertTrue(shouldCheck(secondsSinceLastCheck: nil))
    }

    /// Waking a laptop a dozen times an hour, or a Wi-Fi connection that keeps
    /// dropping, must not turn into a dozen requests against the release CDN.
    func testChecksWithinTheFloorAreDropped() {
        let floor = SoftwareUpdateController.backgroundCheckFloorSeconds
        XCTAssertFalse(shouldCheck(secondsSinceLastCheck: 0))
        XCTAssertFalse(shouldCheck(secondsSinceLastCheck: floor - 1))
        XCTAssertTrue(shouldCheck(secondsSinceLastCheck: floor))
        XCTAssertTrue(shouldCheck(secondsSinceLastCheck: floor * 4))
    }

    /// A running session already covers everything a new check would do,
    /// including an update that is downloaded and waiting on a relaunch.
    func testRunningSessionBlocksAnotherCheck() {
        XCTAssertFalse(
            shouldCheck(sessionInProgress: true, secondsSinceLastCheck: nil)
        )
    }

    /// Sparkle stamps its last-check date even when the check never reached the
    /// appcast. Reconnecting after a failed check is the moment most likely to
    /// succeed, so it must not wait out the full floor.
    func testCheckThatFailedOnTheNetworkIsRetriedSoon() {
        let retryFloor = SoftwareUpdateController.retryFloorSeconds
        XCTAssertFalse(
            shouldCheck(lastCheckFailedOnNetwork: true, secondsSinceLastCheck: retryFloor - 1)
        )
        XCTAssertTrue(
            shouldCheck(lastCheckFailedOnNetwork: true, secondsSinceLastCheck: retryFloor)
        )
        // The same gap after a check that simply found nothing stays blocked.
        XCTAssertFalse(shouldCheck(secondsSinceLastCheck: retryFloor))
    }

    func testUserWhoTurnedOffAutomaticChecksIsNeverOverridden() {
        XCTAssertFalse(
            shouldCheck(automatic: false, secondsSinceLastCheck: nil)
        )
    }

    // MARK: - A check the user started

    /// What 0.8.5 met on 2026-11-26: the feed's host dropped the TLS
    /// handshake, twice, and all the app could say was "try again later".
    private var droppedHandshake: NSError {
        sparkleError(
            .downloadError,
            underlying: NSError(domain: NSURLErrorDomain, code: NSURLErrorSecureConnectionFailed)
        )
    }

    func testUserCheckThatFailedOnTheNetworkIsRetriedOnceWithoutAWord() {
        XCTAssertEqual(SoftwareUpdateController.maximumUserCheckRetries, 1)
        XCTAssertEqual(
            SoftwareUpdateController.userCheckFailure(
                droppedHandshake,
                retriesRemaining: SoftwareUpdateController.maximumUserCheckRetries,
                hasDownloadPage: true
            ),
            .retryQuietly
        )
    }

    func testUserCheckThatFailedAgainOffersTheDownloadPage() {
        XCTAssertEqual(
            SoftwareUpdateController.userCheckFailure(
                droppedHandshake,
                retriesRemaining: 0,
                hasDownloadPage: true
            ),
            .offerDownload
        )
    }

    /// With no page to offer, the app's alert would have nothing Sparkle's
    /// lacks.
    func testUserCheckWithNoDownloadPageKeepsSparklesAlert() {
        XCTAssertEqual(
            SoftwareUpdateController.userCheckFailure(
                droppedHandshake,
                retriesRemaining: 0,
                hasDownloadPage: false
            ),
            .standardAlert
        )
    }

    /// A bad signature is not something a second try or a browser fixes, and
    /// must never be routed round.
    func testUserCheckThatFailedOffTheNetworkKeepsSparklesAlert() {
        for retriesRemaining in [0, 1] {
            XCTAssertEqual(
                SoftwareUpdateController.userCheckFailure(
                    sparkleError(.signatureError),
                    retriesRemaining: retriesRemaining,
                    hasDownloadPage: true
                ),
                .standardAlert
            )
        }
    }

    func testDownloadPageIsTheReleasePageTheFeedHangsFrom() {
        XCTAssertEqual(
            SoftwareUpdateController.downloadPageURL(
                feedURL: "https://github.com/4seas-community/ZuTalk/releases/latest/download/appcast.xml"
            )?.absoluteString,
            "https://github.com/4seas-community/ZuTalk/releases/latest"
        )
    }

    func testFeedThatIsNotAReleaseAssetHasNoDownloadPage() {
        XCTAssertNil(SoftwareUpdateController.downloadPageURL(feedURL: nil))
        XCTAssertNil(SoftwareUpdateController.downloadPageURL(feedURL: "$(SPARKLE_FEED_URL)"))
        XCTAssertNil(
            SoftwareUpdateController.downloadPageURL(feedURL: "https://example.org/appcast.xml")
        )
        XCTAssertNil(
            SoftwareUpdateController.downloadPageURL(
                feedURL: "http://example.org/releases/latest/download/appcast.xml"
            )
        )
    }
}
