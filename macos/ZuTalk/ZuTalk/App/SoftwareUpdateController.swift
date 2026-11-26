import AppKit
import Combine
import Network
import Sparkle

/// Owns the single Sparkle updater used by the app menu, the settings panel,
/// and the sidebar footer.
///
/// The app updates itself without being asked and without interrupting: a new
/// version is found, downloaded, and staged in the background, and the only
/// thing the user ever sees is the sidebar footer turning into a download
/// progress row and then into a relaunch action. Nothing is modal, nothing
/// steals focus, and an update the user ignores installs the next time they
/// quit. See `SoftwareUpdateUserDriver` for why the app answers Sparkle's
/// prompts itself instead of using `SUAutomaticallyUpdate`.
///
/// Checks are driven from three places, all funnelled through
/// `requestBackgroundCheck(reason:)` with a floor between checks:
///
/// - Sparkle's own timer (`SUScheduledCheckInterval`), which covers a machine
///   that stays awake and online.
/// - Waking from sleep, which is how a laptop usually re-enters the world; the
///   in-process timer has no idea hours passed with the lid shut.
/// - The network becoming reachable, which covers launching offline, or
///   working on a plane and later joining Wi-Fi.
///
/// Launch needs no trigger of its own: Sparkle runs a check right after
/// `start()` when the interval has already elapsed, and otherwise arms its
/// timer for the remainder. Adding a forced check per launch would only mean
/// hammering the feed for people who quit and reopen the app all day.
///
/// Transient network failures (an interrupted download or an unreachable
/// appcast) are retried automatically a few times before giving up silently,
/// because the public release CDN is unreliable from some networks.
///
/// A check the user started is retried once as well, and when that also fails
/// on the network the app says so itself and offers the download page: the
/// feed and the downloads live on one host, and a browser often gets through
/// (a proxy, another route) where this process did not. 0.8.5 could only say
/// "try again later" to people it would never reach.
///
/// Sparkle starts only in a real app process. Unit and UI tests can exercise
/// routing without contacting the public appcast or changing updater defaults.
@MainActor
final class SoftwareUpdateController: NSObject, ObservableObject {
    static let shared = SoftwareUpdateController()

    static let maximumNetworkRetries = 2
    private static let retryDelaySeconds: TimeInterval = 6

    /// A person is watching a "checking" window: one more try, then tell them.
    static let maximumUserCheckRetries = 1
    /// Long enough for a route that just came up, short enough to wait through.
    private static let userRetryDelaySeconds: TimeInterval = 2
    /// How long, and how often, to wait for a finished session to let go
    /// before starting the check the person asked for from the alert.
    private static let sessionEndPollSeconds: TimeInterval = 0.2
    private static let sessionEndPollLimit = 25

    /// What to do about a check the user started that failed.
    enum UserCheckFailure: Equatable {
        /// Not a network failure, or nothing of ours to add: Sparkle's alert.
        case standardAlert
        /// A network failure with a try left: say nothing and check again.
        case retryQuietly
        /// A network failure with no tries left: say so, and offer the
        /// download page as a way round.
        case offerDownload
    }

    /// Shortest gap between two background checks, whatever triggered them.
    /// Waking a laptop repeatedly, or a flapping Wi-Fi connection, must not
    /// turn into a stream of requests against the release CDN.
    static let backgroundCheckFloorSeconds: TimeInterval = 30 * 60

    /// The floor after a check that failed on the network. Sparkle stamps its
    /// last-check date even when the check never reached the appcast, so the
    /// usual floor would punish exactly the case worth retrying: launching on
    /// a plane and joining Wi-Fi ten minutes later.
    static let retryFloorSeconds: TimeInterval = 2 * 60

    private var updater: SPUUpdater!
    private let userDriver: SoftwareUpdateUserDriver
    private var pathMonitor: NWPathMonitor?
    private var wakeObserver: NSObjectProtocol?
    private var testCheckAction: (() -> Void)?
    private var isConfigured = false
    private var isStarted = false
    private var retriesRemaining = SoftwareUpdateController.maximumNetworkRetries
    private var lastCheckWasUserInitiated = false
    private var lastCheckFailedOnNetwork = false
    private var isNetworkReachable = true

    /// What the sidebar footer renders. Stays `.idle` for a check that finds
    /// nothing, fails, or is driven by the user through Sparkle's own windows.
    @Published private(set) var activity: SoftwareUpdateActivity = .idle

    /// True only after Sparkle has downloaded and staged a signed update, so
    /// the sidebar never offers a relaunch that has nothing to install.
    var isUpdateReadyToInstall: Bool { activity == .readyToRelaunch }

    var isAvailable: Bool { isConfigured }

    var automaticallyChecksForUpdates: Bool {
        isConfigured && updater.automaticallyChecksForUpdates
    }

    override private init() {
        userDriver = SoftwareUpdateUserDriver(hostBundle: .main)
        super.init()
        isConfigured = Self.hasReleaseConfiguration
        updater = SPUUpdater(
            hostBundle: .main,
            applicationBundle: .main,
            userDriver: userDriver,
            delegate: self
        )
        userDriver.activityDidChange = { [weak self] activity in
            self?.activity = activity
        }
        userDriver.revealInFlightUpdate = {
            WindowCommandRouter.shared.openMainWindow(detail: "software-update")
        }
        userDriver.userCheckFailure = { [weak self] error in
            guard let self else { return .standardAlert }
            return Self.userCheckFailure(
                error,
                retriesRemaining: self.retriesRemaining,
                hasDownloadPage: Self.downloadPageURL(feedURL: Self.configuredFeedURL) != nil
            )
        }
        userDriver.checkAgain = { [weak self] in
            self?.checkForUpdatesWhenSessionEnds(pollsLeft: Self.sessionEndPollLimit)
        }
        userDriver.openDownloadPage = {
            guard let page = Self.downloadPageURL(feedURL: Self.configuredFeedURL) else { return }
            NSWorkspace.shared.open(page)
        }
        guard isConfigured, !TestEnvironment.isAnyTestMode else { return }
        startUpdater()
    }

    private func startUpdater() {
        do {
            try updater.start()
        } catch {
            // A misconfigured updater must not take the app down with it. The
            // menu action falls back to the "not configured" alert.
            isConfigured = false
            DebugLog.error("Sparkle updater failed to start", detail: error.localizedDescription)
            return
        }
        isStarted = true
        // The app renders its own download progress, which Sparkle's silent
        // driver cannot report. See `SoftwareUpdateUserDriver`.
        updater.automaticallyDownloadsUpdates = false
        observeWakeAndNetwork()
    }

    func checkForUpdates() {
        if let testCheckAction {
            testCheckAction()
            return
        }
        guard isConfigured, isStarted else {
            let alert = NSAlert()
            alert.messageText = String(localized: "updates.unavailable.title")
            alert.informativeText = String(localized: "updates.unavailable.message")
            alert.alertStyle = .informational
            alert.addButton(withTitle: String(localized: "updates.unavailable.dismiss"))
            alert.runModal()
            return
        }
        lastCheckWasUserInitiated = true
        retriesRemaining = Self.maximumUserCheckRetries
        updater.checkForUpdates()
    }

    /// "Try Again" in the alert: the failed session is still winding down when
    /// the button is pressed, and Sparkle ignores a check started over one.
    private func checkForUpdatesWhenSessionEnds(pollsLeft: Int) {
        guard isConfigured, isStarted else { return }
        guard updater.sessionInProgress, pollsLeft > 0 else {
            checkForUpdates()
            return
        }
        DispatchQueue.main.asyncAfter(deadline: .now() + Self.sessionEndPollSeconds) { [weak self] in
            self?.checkForUpdatesWhenSessionEnds(pollsLeft: pollsLeft - 1)
        }
    }

    func installTestCheckAction(_ action: (() -> Void)?) {
        guard TestEnvironment.isAnyTestMode else { return }
        testCheckAction = action
    }

    func setAutomaticallyChecksForUpdates(_ enabled: Bool) {
        guard isConfigured, isStarted else { return }
        updater.automaticallyChecksForUpdates = enabled
    }

    func installUpdateAndRelaunch() {
        userDriver.installStagedUpdateAndRelaunch()
    }

    // MARK: - Background checks

    private func observeWakeAndNetwork() {
        wakeObserver = NSWorkspace.shared.notificationCenter.addObserver(
            forName: NSWorkspace.didWakeNotification,
            object: nil,
            queue: .main
        ) { _ in
            Task { @MainActor in
                SoftwareUpdateController.shared.requestBackgroundCheck(reason: "wake")
            }
        }

        let monitor = NWPathMonitor()
        monitor.pathUpdateHandler = { path in
            let reachable = path.status == .satisfied
            Task { @MainActor in
                SoftwareUpdateController.shared.handleNetworkReachability(reachable)
            }
        }
        monitor.start(queue: DispatchQueue.global(qos: .utility))
        pathMonitor = monitor
    }

    /// Only the offline → online edge is a reason to check. Losing the network
    /// is not, and staying online is already Sparkle's timer's job.
    private func handleNetworkReachability(_ reachable: Bool) {
        let wasReachable = isNetworkReachable
        isNetworkReachable = reachable
        guard reachable, !wasReachable else { return }
        requestBackgroundCheck(reason: "network-reachable")
    }

    /// Whether a background check is worth starting right now. Split out so the
    /// throttling rules can be tested without a live updater.
    static func shouldStartBackgroundCheck(
        automaticChecksEnabled: Bool,
        sessionInProgress: Bool,
        lastCheckDate: Date?,
        lastCheckFailedOnNetwork: Bool,
        now: Date
    ) -> Bool {
        guard automaticChecksEnabled else { return false }
        // A session already running covers everything a new check would do,
        // including a staged update waiting for the user to relaunch.
        guard !sessionInProgress else { return false }
        guard let lastCheckDate else { return true }
        let floor = lastCheckFailedOnNetwork ? retryFloorSeconds : backgroundCheckFloorSeconds
        return now.timeIntervalSince(lastCheckDate) >= floor
    }

    private func requestBackgroundCheck(reason: String) {
        guard isConfigured, isStarted, !TestEnvironment.isAnyTestMode else { return }
        guard
            Self.shouldStartBackgroundCheck(
                automaticChecksEnabled: updater.automaticallyChecksForUpdates,
                sessionInProgress: updater.sessionInProgress,
                lastCheckDate: updater.lastUpdateCheckDate,
                lastCheckFailedOnNetwork: lastCheckFailedOnNetwork,
                now: Date()
            )
        else { return }
        DebugLog.info("Background update check", detail: reason)
        lastCheckWasUserInitiated = false
        retriesRemaining = Self.maximumNetworkRetries
        updater.checkForUpdatesInBackground()
    }

    // MARK: - Failures

    /// Whether an aborted update cycle failed on the network rather than on
    /// policy, signatures, or the user cancelling, and is worth retrying.
    static func isTransientNetworkFailure(_ error: NSError) -> Bool {
        guard error.domain == SUSparkleErrorDomain else { return false }
        let retryableCodes = [
            Int(SUError.downloadError.rawValue),
            Int(SUError.appcastError.rawValue),
        ]
        guard retryableCodes.contains(error.code) else { return false }
        if let underlying = error.userInfo[NSUnderlyingErrorKey] as? NSError,
            underlying.domain == NSURLErrorDomain,
            underlying.code == NSURLErrorCancelled
        {
            return false
        }
        return true
    }

    /// Whether a failed check the user started is retried, explained by the
    /// app, or left to Sparkle. Split out so the rule can be tested without a
    /// live updater; `handleAbort` and the user driver must agree on it.
    static func userCheckFailure(
        _ error: NSError,
        retriesRemaining: Int,
        hasDownloadPage: Bool
    ) -> UserCheckFailure {
        guard isTransientNetworkFailure(error) else { return .standardAlert }
        if retriesRemaining > 0 { return .retryQuietly }
        return hasDownloadPage ? .offerDownload : .standardAlert
    }

    /// The page the app can be downloaded from by hand. Derived from the feed
    /// address rather than written down a second time, so the two cannot come
    /// to name different projects. Nil for a feed that is not a release
    /// asset: then there is no page to offer.
    static func downloadPageURL(feedURL: String?) -> URL? {
        let asset = "/download/appcast.xml"
        guard let feedURL, feedURL.hasPrefix("https://"), feedURL.hasSuffix(asset) else {
            return nil
        }
        return URL(string: String(feedURL.dropLast(asset.count)))
    }

    private static var configuredFeedURL: String? {
        Bundle.main.object(forInfoDictionaryKey: "SUFeedURL") as? String
    }

    private func handleAbort(_ error: NSError) {
        guard isConfigured, isStarted, !TestEnvironment.isAnyTestMode else { return }
        guard Self.isTransientNetworkFailure(error), retriesRemaining > 0 else { return }
        retriesRemaining -= 1
        // The user driver swallowed this failure for a check the user started
        // (`userCheckFailure` said `.retryQuietly` while a try was left), so
        // the retry must be one they can see end: the same kind of check.
        let userInitiated = lastCheckWasUserInitiated
        let delay = userInitiated ? Self.userRetryDelaySeconds : Self.retryDelaySeconds
        DispatchQueue.main.asyncAfter(deadline: .now() + delay) { [weak self] in
            guard let self, self.isConfigured, self.isStarted else { return }
            // Something else began a check while this one waited; it covers it.
            guard !self.updater.sessionInProgress else { return }
            if userInitiated {
                self.updater.checkForUpdates()
            } else {
                self.updater.checkForUpdatesInBackground()
            }
        }
    }

    private static var hasReleaseConfiguration: Bool {
        guard
            let feedURL = Bundle.main.object(forInfoDictionaryKey: "SUFeedURL") as? String,
            let publicKey = Bundle.main.object(forInfoDictionaryKey: "SUPublicEDKey") as? String
        else {
            return false
        }
        return feedURL.hasPrefix("https://")
            && !publicKey.isEmpty
            && !publicKey.contains("$(")
    }
}

extension SoftwareUpdateController: SPUUpdaterDelegate {
    /// The new ZuTalk is on a channel of its own, offered only once this library has been
    /// handed over to it: nobody updates past their recordings.
    nonisolated func allowedChannels(for updater: SPUUpdater) -> Set<String> {
        LibraryHandover.isWritten ? [LibraryHandover.channel] : []
    }

    /// The last handover, so that it holds whatever changed since launch.
    nonisolated func updater(_ updater: SPUUpdater, willInstallUpdate item: SUAppcastItem) {
        MainActor.assumeIsolated { LibraryHandover.writeNow() }
    }

    nonisolated func updater(_ updater: SPUUpdater, didAbortWithError error: Error) {
        let nsError = error as NSError
        Task { @MainActor in
            self.handleAbort(nsError)
        }
    }

    nonisolated func updater(
        _ updater: SPUUpdater,
        didFinishUpdateCycleFor updateCheck: SPUUpdateCheck,
        error: Error?
    ) {
        let nsError = error as NSError?
        Task { @MainActor in
            if error == nil {
                self.retriesRemaining = Self.maximumNetworkRetries
            }
            // "You are up to date" is a completed check, not a failed one, and
            // must not shorten the floor before the next attempt.
            self.lastCheckFailedOnNetwork =
                nsError.map(Self.isTransientNetworkFailure) ?? false
            // Backstop: a cycle that ends while the footer still claims a
            // download is in flight would leave a progress bar that never
            // moves. A staged update waiting on the user is left alone.
            self.userDriver.resetActivityIfInFlight()
        }
    }
}
