import Foundation

/// The 0.8 line's last duty: it hands the library over to the new ZuTalk, which moves every
/// recording in on its first launch (handover v1, in the new repository's docs/migrate.md).
/// Written at each launch, and again just before an update installs; the new version is
/// offered on its own appcast channel, and only once a handover exists.
enum LibraryHandover {
    /// The appcast channel the new ZuTalk is published on.
    nonisolated static let channel = "zu"

    nonisolated static var isWritten: Bool {
        let index = URL(fileURLWithPath: CoreClient.defaultDataDir(), isDirectory: true)
            .appendingPathComponent("handover/index.json", isDirectory: false)
        return FileManager.default.fileExists(atPath: index.path)
    }

    static func writeInBackground() {
        guard TestEnvironment.isAnyTestMode == false, let core = CoreClient.shared.core else {
            return
        }
        DispatchQueue.global(qos: .utility).async { write(core) }
    }

    static func writeNow() {
        guard TestEnvironment.isAnyTestMode == false, let core = CoreClient.shared.core else {
            return
        }
        write(core)
    }

    nonisolated private static func write(_ core: ZuTalkCore) {
        var (count, failure): (UInt32, String?) = (0, nil)
        do {
            count = try core.writeHandover()
        } catch {
            failure = error.localizedDescription
        }
        let (done, failed) = (count, failure)
        Task { @MainActor in
            if let failed {
                DebugLog.error("Library handover failed", detail: failed)
            } else {
                DebugLog.trace("Library handed over", detail: "\(done) recordings")
            }
        }
    }
}
