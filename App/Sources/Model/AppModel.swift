// App-wide state. Views read it; every mutation goes through here so the
// core, the Keychain and notifications stay in step.
import Foundation
import Observation
import SwiftUI
import UserNotifications

/// A running core request the UI shows progress for.
@Observable
final class Operation: Identifiable {
    let id = UUID()
    let title: String
    var stage = "Starting"
    var fraction: Double?
    var task: Task<Void, Never>?
    init(title: String) { self.title = title }
    func cancel() { task?.cancel() }
}

@MainActor @Observable
final class AppModel {
    enum DeviceState: Equatable {
        case unknown, checking
        case connected(DeviceInfo)
        case failed(String)
    }

    // Settings (UserDefaults-backed).
    var anisetteURL: String { didSet { save("anisetteURL", anisetteURL); pushSettings() } }
    var deviceAddress: String { didSet { save("deviceAddress", deviceAddress); pushSettings() } }
    var notifyBeforeExpiry: Bool { didSet { save("notifyBeforeExpiry", notifyBeforeExpiry); scheduleExpiryNotifications() } }

    private(set) var account: AccountInfo?
    private(set) var signingIn = false
    private(set) var device: DeviceState = .unknown
    private(set) var library: [LibraryApp] = []
    private(set) var deviceApps: [InstalledApp] = []
    private(set) var operations: [Operation] = []
    var alert: AlertItem?

    // Prompts from the core.
    var twoFactor: TwoFactorPrompt?
    var revoke: RevokePrompt?

    let sources = SourceStore()
    let downloads = Downloads()

    static let anisetteServers: [(name: String, url: String)] = [
        ("SideStore", "https://ani.sidestore.zip"),
        ("SideStore (old)", "https://ani.sidestore.io"),
        ("jkcoxson", "https://mac-ani.jkcoxson.com"),
    ]

    init() {
        let d = UserDefaults.standard
        anisetteURL = d.string(forKey: "anisetteURL") ?? Self.anisetteServers[0].url
        deviceAddress = d.string(forKey: "deviceAddress") ?? "10.7.0.1"
        notifyBeforeExpiry = d.object(forKey: "notifyBeforeExpiry") as? Bool ?? true
        Core.shared.start(anisetteURL: anisetteURL, deviceAddress: deviceAddress)
        Core.shared.onPrompt = { [weak self] id, req in self?.present(prompt: id, req) }
    }

    private func save(_ k: String, _ v: Any) { UserDefaults.standard.set(v, forKey: k) }

    private func pushSettings() {
        struct U: Encodable { var anisette_url: String; var device_address: String }
        Task { _ = try? await Core.shared.call(.settingsUpdate, U(anisette_url: anisetteURL, device_address: deviceAddress), as: JSONValue.self) }
    }

    func show(_ error: Error, title: String = "Something went wrong") {
        if let e = error as? CoreError, e.isCancelled { return }
        if error is CancellationError { return }
        alert = AlertItem(title: title, message: error.localizedDescription)
    }

    // MARK: launch

    func launch() async {
        await reloadLibrary()
        async let s: Void = sources.refresh()
        async let d: Void = checkDevice()
        if let creds = Keychain.load() {
            await signIn(email: creds.email, password: creds.password, silent: true)
        }
        _ = await (s, d)
    }

    // MARK: account

    func signIn(email: String, password: String, silent: Bool = false) async {
        signingIn = true
        defer { signingIn = false }
        struct P: Encodable { var email: String; var password: String }
        do {
            account = try await Core.shared.call(.accountLogin, P(email: email, password: password))
            Keychain.save(email: email, password: password)
        } catch {
            account = nil
            if !silent { show(error, title: "Couldn't sign in") }
        }
    }

    func signOut() async {
        _ = try? await Core.shared.call(.accountLogout, as: Empty.self)
        Keychain.delete()
        account = nil
    }

    /// Sign in again if the session was lost (e.g. the core restarted).
    private func ensureSignedIn() async throws {
        if account != nil { return }
        guard let creds = Keychain.load() else { throw CoreError(message: "Sign in with your Apple ID in Settings first.") }
        await signIn(email: creds.email, password: creds.password)
        if account == nil { throw CoreError(message: "Sign in with your Apple ID in Settings first.") }
    }

    // MARK: device

    func checkDevice() async {
        device = .checking
        do {
            let info: DeviceInfo = try await Core.shared.call(.deviceConnect)
            device = .connected(info)
            deviceApps = (try? await Core.shared.call(.deviceApps)) ?? []
        } catch {
            device = .failed(error.localizedDescription)
        }
    }

    var hasPairingFile: Bool {
        FileManager.default.fileExists(atPath: Core.dataDir.appendingPathComponent("pairing.plist").path)
    }

    func importPairingFile(from url: URL) throws {
        let scoped = url.startAccessingSecurityScopedResource()
        defer { if scoped { url.stopAccessingSecurityScopedResource() } }
        let data = try Data(contentsOf: url)
        guard let plist = try PropertyListSerialization.propertyList(from: data, format: nil) as? [String: Any],
              plist["HostCertificate"] != nil, plist["HostPrivateKey"] != nil else {
            throw CoreError(message: "That isn't a pairing file (it has no HostCertificate/HostPrivateKey).")
        }
        try FileManager.default.createDirectory(at: Core.dataDir, withIntermediateDirectories: true)
        let dest = Core.dataDir.appendingPathComponent("pairing.plist")
        try data.write(to: dest, options: [.atomic, .completeFileProtection])
        Task { await checkDevice() }
    }

    // MARK: library

    func reloadLibrary() async {
        library = (try? await Core.shared.call(.libraryList)) ?? []
        scheduleExpiryNotifications()
    }

    /// Start an operation with progress; `body` gets a progress callback.
    @discardableResult
    func run(_ title: String, _ body: @escaping (@escaping @Sendable (CoreProgress) -> Void) async throws -> Void) -> Operation {
        let op = Operation(title: title)
        operations.append(op)
        op.task = Task { [weak self] in
            do {
                try await body { p in Task { @MainActor in op.stage = p.stage; op.fraction = p.fraction } }
            } catch {
                self?.show(error, title: "\(title) failed")
            }
            self?.operations.removeAll { $0.id == op.id }
        }
        return op
    }

    func install(ipa: URL, options: InstallOptions, name: String, deleteAfter: Bool = false) {
        run("Installing \(name)") { [self] progress in
            defer { if deleteAfter { try? FileManager.default.removeItem(at: ipa) } }
            try await ensureSignedIn()
            struct P: Encodable { var ipa: String; var options: InstallOptions }
            let _: LibraryApp = try await Core.shared.call(.appInstall, P(ipa: ipa.path, options: options), progress: progress)
            await reloadLibrary()
            deviceApps = (try? await Core.shared.call(.deviceApps)) ?? deviceApps
        }
    }

    func refresh(_ apps: [LibraryApp]? = nil) {
        let title = apps?.count == 1 ? "Refreshing \(apps![0].name)" : "Refreshing apps"
        run(title) { [self] progress in
            try await ensureSignedIn()
            struct P: Encodable { var bundle_ids: [String]; var within_days: Int }
            let r: RefreshResult = try await Core.shared.call(
                .libraryRefresh, P(bundle_ids: apps?.map(\.original_bundle_id) ?? [], within_days: 3), progress: progress)
            await reloadLibrary()
            if !r.failed.isEmpty {
                alert = AlertItem(title: "Some apps weren't refreshed",
                                  message: r.failed.map { "\($0.bundle_id): \($0.message)" }.joined(separator: "\n\n"))
            }
        }
    }

    func remove(_ app: LibraryApp, uninstall: Bool) {
        run("Removing \(app.name)") { [self] _ in
            struct P: Encodable { var bundle_id: String; var uninstall: Bool }
            _ = try await Core.shared.call(.libraryRemove, P(bundle_id: app.original_bundle_id, uninstall: uninstall), as: Empty.self)
            await reloadLibrary()
            deviceApps = (try? await Core.shared.call(.deviceApps)) ?? deviceApps
        }
    }

    func uninstall(_ app: InstalledApp) {
        run("Removing \(app.name)") { [self] _ in
            struct P: Encodable { var bundle_id: String }
            _ = try await Core.shared.call(.deviceUninstall, P(bundle_id: app.bundle_id), as: Empty.self)
            deviceApps = (try? await Core.shared.call(.deviceApps)) ?? deviceApps
        }
    }

    /// Download a source app, then hand the IPA to the install sheet.
    func download(_ app: SourceApp, version: SourceAppVersion? = nil) async -> URL? {
        guard let v = version ?? app.latest else { return nil }
        do { return try await downloads.download(v.downloadURL, name: app.name) } catch {
            show(error, title: "Couldn't download \(app.name)")
            return nil
        }
    }

    // MARK: prompts

    private func present(prompt id: UInt64, _ req: PromptRequest) {
        switch req {
        case .twoFactor(let method, let phones): twoFactor = TwoFactorPrompt(id: id, method: method, phones: phones)
        case .revokeCertificates(let certs): revoke = RevokePrompt(id: id, certificates: certs)
        }
    }

    func answer(_ prompt: UInt64, _ a: PromptAnswer) {
        Core.shared.respond(prompt, a)
    }

    // MARK: notifications

    func scheduleExpiryNotifications() {
        let center = UNUserNotificationCenter.current()
        center.removeAllPendingNotificationRequests()
        guard notifyBeforeExpiry else { return }
        let apps = library
        Task {
            guard (try? await center.requestAuthorization(options: [.alert, .sound])) == true else { return }
            for app in apps {
                guard let expiry = app.expiryDate else { continue }
                let when = expiry.addingTimeInterval(-24 * 3600)
                guard when > Date() else { continue }
                let content = UNMutableNotificationContent()
                content.title = "\(app.name) expires tomorrow"
                content.body = "Open Ferry with the VPN on to refresh it."
                content.sound = .default
                let trigger = UNCalendarNotificationTrigger(
                    dateMatching: Calendar.current.dateComponents([.year, .month, .day, .hour, .minute], from: when), repeats: false)
                try? await center.add(UNNotificationRequest(identifier: "expiry.\(app.id)", content: content, trigger: trigger))
            }
        }
    }
}

struct AlertItem: Identifiable {
    let id = UUID()
    var title: String
    var message: String
}

struct TwoFactorPrompt: Identifiable {
    var id: UInt64
    var method: String
    var phones: [Phone]
}

struct RevokePrompt: Identifiable {
    var id: UInt64
    var certificates: [String]
}
